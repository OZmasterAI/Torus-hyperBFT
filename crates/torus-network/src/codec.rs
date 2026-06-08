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

impl BorshCodec {
    const MAX_MSG_SIZE: usize = MAX_DIRECT_MSG_SIZE;
}

/// `/torus/direct` push-path cap (unchanged — the per-validator push backpressure
/// fix is tracked separately). Oversized pre-proposal bodies are recovered via the
/// chunked `/torus/native-da` pull path instead.
const MAX_DIRECT_MSG_SIZE: usize = 4 * 1024 * 1024; // 4 MB
/// `/torus/native-da` response cap — generous headroom; bodies are pulled in
/// size-bounded chunks (`NATIVE_DA_FETCH_CHUNK`), so a response stays well under this.
const MAX_NATIVE_DA_MSG_SIZE: usize = 8 * 1024 * 1024; // 8 MB
/// `/torus/block-data` response cap — must fit a full big block during sync.
const MAX_BLOCK_DATA_MSG_SIZE: usize = 16 * 1024 * 1024; // 16 MB

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
        read_length_prefixed_borsh(io, Self::MAX_MSG_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io, Self::MAX_MSG_SIZE).await
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

async fn read_length_prefixed_borsh<T, D>(io: &mut T, max_size: usize) -> io::Result<D>
where
    T: AsyncRead + Unpin + Send,
    D: borsh::BorshDeserialize,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_size {
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

impl BlockDataCodec {
    const MAX_MSG_SIZE: usize = MAX_BLOCK_DATA_MSG_SIZE;
}

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
        read_length_prefixed_borsh(io, Self::MAX_MSG_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io, Self::MAX_MSG_SIZE).await
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

impl NativeDaCodec {
    const MAX_MSG_SIZE: usize = MAX_NATIVE_DA_MSG_SIZE;
}

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
        read_length_prefixed_borsh(io, Self::MAX_MSG_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_length_prefixed_borsh(io, Self::MAX_MSG_SIZE).await
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

    /// Task 1 (RED first): a native-DA response larger than the legacy shared 4 MB
    /// cap MUST round-trip through `NativeDaCodec` once it has its own larger cap
    /// (Option C). This is exactly the case that wedged the chain at bs=1000
    /// (~4.17 MB blocks) — today it fails with "message too large".
    #[test]
    fn native_da_response_over_4mb_roundtrips() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            let proto = StreamProtocol::new("/torus/native-da/1.0");
            // ~5.5 MB total: 8 bodies x 700 KB — well over the old 4 MB cap.
            let resp = NativeDaNetResponse {
                bodies: (0..8u8).map(|i| vec![i; 700 * 1024]).collect(),
            };
            let mut codec = NativeDaCodec;
            let mut wbuf = Cursor::new(Vec::new());
            codec
                .write_response(&proto, &mut wbuf, resp.clone())
                .await
                .expect("write >4MB native-da response");
            let bytes = wbuf.into_inner();
            assert!(
                bytes.len() > 4 * 1024 * 1024,
                "test fixture must exceed the old 4MB cap to be meaningful"
            );
            let mut rbuf = Cursor::new(bytes);
            let back = codec
                .read_response(&proto, &mut rbuf)
                .await
                .expect("read >4MB native-da response");
            assert_eq!(back.bodies, resp.bodies);
        });
    }
}
