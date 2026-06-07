use std::io;

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::StreamProtocol;

/// Borsh-encoded direct message for peer-to-peer delivery.
/// Used by the `send()` method of the Network trait.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug)]
pub struct DirectRequest {
    pub sender_key: [u8; 32],
    pub payload: Vec<u8>,
}

/// Acknowledgment for a direct message.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug)]
pub struct DirectResponse;

/// Length-prefixed borsh codec for the direct message protocol.
#[derive(Debug, Clone, Default)]
pub struct BorshCodec;

const MAX_DIRECT_MSG_SIZE: usize = 4 * 1024 * 1024; // 4 MB

#[async_trait]
impl libp2p::request_response::Codec for BorshCodec {
    type Protocol = StreamProtocol;
    type Request = DirectRequest;
    type Response = DirectResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed_borsh(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed_borsh(io, &res).await
    }
}

async fn read_length_prefixed_borsh<T, D>(io: &mut T) -> io::Result<D>
where
    T: AsyncRead + Unpin + Send,
    D: borsh::BorshDeserialize,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_DIRECT_MSG_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    D::try_from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn write_length_prefixed_borsh<T, S>(io: &mut T, msg: &S) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    S: borsh::BorshSerialize,
{
    let buf = msg
        .try_to_vec()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = (buf.len() as u32).to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(&buf).await?;
    io.close().await?;
    Ok(())
}

/// Request on the dedicated `/torus/block-data/1.0` protocol.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug, Clone)]
pub struct BlockDataNetRequest {
    pub block_hash: [u8; 32],
    pub view: u64,
}

/// Response on the dedicated `/torus/block-data/1.0` protocol.
/// `payload` is a borsh-encoded `Block`; empty when not found.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug, Clone)]
pub struct BlockDataNetResponse {
    pub view: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct BlockDataCodec;

#[async_trait]
impl libp2p::request_response::Codec for BlockDataCodec {
    type Protocol = StreamProtocol;
    type Request = BlockDataNetRequest;
    type Response = BlockDataNetResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed_borsh(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed_borsh(io, &res).await
    }
}

/// Request on the dedicated `/torus/native-da/1.0` protocol: fetch native-action
/// bodies by their 32-byte action-hash. Used by the RARE pull-fallback when a
/// `CompactBlock` references a body that is absent locally (push covers the
/// common case; mem a6cf33a9 — pull is fallback-only, never per-block).
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug, Clone)]
pub struct NativeDaNetRequest {
    pub hashes: Vec<[u8; 32]>,
}

/// Response on the dedicated `/torus/native-da/1.0` protocol. `bodies[i]` is the
/// `bincode(SignedNativeAction)` for `hashes[i]` from the request, in the same
/// order; an empty entry means the body was not found on the server.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug, Clone)]
pub struct NativeDaNetResponse {
    pub bodies: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub struct NativeDaCodec;

#[async_trait]
impl libp2p::request_response::Codec for NativeDaCodec {
    type Protocol = StreamProtocol;
    type Request = NativeDaNetRequest;
    type Response = NativeDaNetResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed_borsh(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_length_prefixed_borsh(io, &res).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::{BorshDeserialize, BorshSerialize};

    /// The `/torus/native-da/1.0` request/response wire format round-trips through
    /// the length-prefixed borsh codec, including the empty/not-found cases.
    #[test]
    fn native_da_protocol_roundtrip() {
        // Request: multiple hashes round-trip preserving order.
        let req = NativeDaNetRequest {
            hashes: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
        };
        let bytes = req.try_to_vec().expect("serialize request");
        let back = NativeDaNetRequest::try_from_slice(&bytes).expect("deserialize request");
        assert_eq!(back.hashes, req.hashes);

        // Empty request round-trips (degenerate fetch).
        let empty_req = NativeDaNetRequest { hashes: Vec::new() };
        let bytes = empty_req.try_to_vec().unwrap();
        assert!(NativeDaNetRequest::try_from_slice(&bytes).unwrap().hashes.is_empty());

        // Response: a found body, an empty (not-found) entry, another found body —
        // ordering and the empty marker must survive the round-trip.
        let resp = NativeDaNetResponse {
            bodies: vec![vec![9u8, 8, 7], Vec::new(), vec![1u8, 2, 3, 4]],
        };
        let bytes = resp.try_to_vec().expect("serialize response");
        let back = NativeDaNetResponse::try_from_slice(&bytes).expect("deserialize response");
        assert_eq!(back.bodies, resp.bodies);
        assert!(back.bodies[1].is_empty(), "not-found entry stays empty");
    }
}
