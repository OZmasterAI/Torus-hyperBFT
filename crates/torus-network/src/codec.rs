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
