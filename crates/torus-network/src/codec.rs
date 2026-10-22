use std::io;

use async_trait::async_trait;
use futures::prelude::*;
use libp2p::StreamProtocol;

/// Immutable direct-message bytes. One-off/inbound messages keep their Vec;
/// fanout messages share that Vec allocation across peers and retry queues.
/// Ownership is local only: Borsh always emits the original byte-vector format.
#[derive(Debug, Clone)]
pub enum DirectPayload {
    Owned(Vec<u8>),
    Shared(std::sync::Arc<Vec<u8>>),
}

impl PartialEq for DirectPayload {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl Eq for DirectPayload {}

impl From<Vec<u8>> for DirectPayload {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Owned(bytes)
    }
}

impl From<std::sync::Arc<Vec<u8>>> for DirectPayload {
    fn from(bytes: std::sync::Arc<Vec<u8>>) -> Self {
        Self::Shared(bytes)
    }
}

impl std::ops::Deref for DirectPayload {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

impl borsh::BorshSerialize for DirectPayload {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> io::Result<()> {
        borsh::BorshSerialize::serialize(&**self, writer)
    }
}

impl borsh::BorshDeserialize for DirectPayload {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> io::Result<Self> {
        Vec::<u8>::deserialize_reader(reader).map(Self::Owned)
    }
}

/// Borsh-encoded direct message for peer-to-peer delivery.
/// Used by the `send()` method of the Network trait.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug)]
pub struct DirectRequest {
    pub sender_key: [u8; 32],
    pub payload: DirectPayload,
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

// Codec caps live in `caps` — the single source of truth for the size-cap
// ladder (O5), where their ordering is test-enforced.
use crate::caps::{
    MAX_BLOCK_DATA_MSG_SIZE, MAX_DIRECT_MSG_SIZE, MAX_NATIVE_DA_MSG_SIZE,
    MAX_NATIVE_DA_SHARDS_MSG_SIZE,
};

#[async_trait]
impl libp2p::request_response::Codec for BorshCodec {
    type Protocol = StreamProtocol;
    type Request = DirectRequest;
    type Response = DirectResponse;

    async fn read_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        if is_v2_protocol(protocol) {
            read_frame(protocol, io, Self::MAX_MSG_SIZE).await
        } else {
            let (sender_key, payload) = read_raw_payload::<_, 32>(io, Self::MAX_MSG_SIZE).await?;
            Ok(DirectRequest {
                sender_key,
                payload: payload.into(),
            })
        }
    }

    async fn read_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(protocol, io, Self::MAX_MSG_SIZE).await
    }

    async fn write_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        if is_v2_protocol(protocol) {
            write_frame(protocol, io, &req, WirePath::Direct).await
        } else {
            write_raw_payload(io, &req.sender_key, &req.payload).await
        }
    }

    async fn write_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(protocol, io, &res, WirePath::Direct).await
    }
}

/// zstd level for `/torus/*/2.0` wire frames. Level 3 measured 3–5x on
/// order-JSON bodies at negligible CPU for our frame sizes (see the
/// `zstd_level_ratio_probe` test for the data behind the choice).
pub(crate) const ZSTD_WIRE_LEVEL: i32 = 3;

/// Body-carrying wire paths with `/2.0` (zstd) variants, indexing the
/// compression counters below.
#[derive(Clone, Copy, Debug)]
pub enum WirePath {
    Direct = 0,
    BlockData = 1,
    NativeDa = 2,
    GossipNative = 3,
}

pub const WIRE_PATH_NAMES: [&str; 4] = ["direct", "block-data", "native-da", "gossip-native"];

use std::sync::atomic::{AtomicU64, Ordering};
static WIRE_PRE_BYTES: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static WIRE_ON_BYTES: [AtomicU64; 4] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

pub(crate) fn record_wire_compression(path: WirePath, pre: usize, wire: usize) {
    WIRE_PRE_BYTES[path as usize].fetch_add(pre as u64, Ordering::Relaxed);
    WIRE_ON_BYTES[path as usize].fetch_add(wire as u64, Ordering::Relaxed);
}

/// Cumulative (pre-compress, on-wire) byte counters per `/2.0` path —
/// `(path, pre_bytes, wire_bytes)`. Sprint 5 T5: proves the compression
/// ratio on live traffic until full prometheus export lands with the
/// friend-deploy rollout.
pub fn wire_compression_stats() -> Vec<(&'static str, u64, u64)> {
    WIRE_PATH_NAMES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            (
                *name,
                WIRE_PRE_BYTES[i].load(Ordering::Relaxed),
                WIRE_ON_BYTES[i].load(Ordering::Relaxed),
            )
        })
        .collect()
}

fn is_v2_protocol(protocol: &StreamProtocol) -> bool {
    protocol.as_ref().ends_with("/2.0")
}

/// Protocol-dispatching read: `/2.0` = zstd-framed borsh, anything else =
/// legacy raw frame (old peers keep their exact wire format).
async fn read_frame<T, D>(protocol: &StreamProtocol, io: &mut T, max_size: usize) -> io::Result<D>
where
    T: AsyncRead + Unpin + Send,
    D: borsh::BorshDeserialize,
{
    if is_v2_protocol(protocol) {
        read_length_prefixed_borsh_zstd(io, max_size).await
    } else {
        read_length_prefixed_borsh(io, max_size).await
    }
}

/// Protocol-dispatching write, mirror of [`read_frame`].
async fn write_frame<T, S>(
    protocol: &StreamProtocol,
    io: &mut T,
    msg: &S,
    path: WirePath,
) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    S: borsh::BorshSerialize,
{
    if is_v2_protocol(protocol) {
        write_length_prefixed_borsh_zstd(io, msg, path).await
    } else {
        write_length_prefixed_borsh(io, msg).await
    }
}

async fn read_length_prefixed_borsh_zstd<T, D>(io: &mut T, max_size: usize) -> io::Result<D>
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
    // Decompression hard-bounded by the path cap: a frame that inflates past
    // `max_size` errors here WITHOUT the oversized allocation (bomb guard).
    let raw = zstd::bulk::decompress(&buf, max_size)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    D::try_from_slice(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn write_length_prefixed_borsh_zstd<T, S>(
    io: &mut T,
    msg: &S,
    path: WirePath,
) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    S: borsh::BorshSerialize,
{
    let raw = msg
        .try_to_vec()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let compressed = zstd::bulk::compress(&raw, ZSTD_WIRE_LEVEL)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    record_wire_compression(path, raw.len(), compressed.len());
    tracing::debug!(
        path = WIRE_PATH_NAMES[path as usize],
        pre = raw.len(),
        wire = compressed.len(),
        "zstd /2.0 frame"
    );
    let len = (compressed.len() as u32).to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(&compressed).await?;
    io.close().await?;
    Ok(())
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

/// Raw Borsh structs used here consist of a fixed header followed by Vec<u8>.
/// Read into the final payload allocation instead of allocating a whole frame
/// and copying it again during Borsh decoding. N is 32 (sender) or 8 (view).
async fn read_raw_payload<T, const N: usize>(
    io: &mut T,
    max_size: usize,
) -> io::Result<([u8; N], Vec<u8>)>
where
    T: AsyncRead + Unpin + Send,
{
    assert!(N <= 32);
    let mut length = [0u8; 4];
    io.read_exact(&mut length).await?;
    let frame_len = u32::from_be_bytes(length) as usize;
    if frame_len > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let header_len = N + 4;
    let mut header = [0u8; 36];
    // Consume the complete advertised frame before reporting malformed Borsh,
    // as the generic decoder does. A truncated frame must remain UnexpectedEof.
    if frame_len < header_len {
        io.read_exact(&mut header[..frame_len]).await?;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short payload header",
        ));
    }
    io.read_exact(&mut header[..header_len]).await?;
    let payload_len = u32::from_le_bytes(header[N..header_len].try_into().unwrap()) as usize;
    let remaining = frame_len - header_len;
    if payload_len != remaining {
        let mut left = remaining;
        let mut discard = [0u8; 1024];
        while left != 0 {
            let count = left.min(discard.len());
            io.read_exact(&mut discard[..count]).await?;
            left -= count;
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "payload length mismatch",
        ));
    }
    let mut fixed = [0u8; N];
    fixed.copy_from_slice(&header[..N]);
    let mut payload = vec![0u8; payload_len];
    io.read_exact(&mut payload).await?;
    Ok((fixed, payload))
}

/// Emit byte-identical raw Borsh framing without copying the body into a
/// second serialized Vec. Header and body use two writes, as the generic path.
async fn write_raw_payload<T, const N: usize>(
    io: &mut T,
    fixed: &[u8; N],
    payload: &[u8],
) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
{
    assert!(N <= 32);
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "payload length exceeds wire format",
        )
    };
    let payload_len = u32::try_from(payload.len()).map_err(|_| invalid())?;
    let frame_len = payload_len
        .checked_add((N + 4) as u32)
        .ok_or_else(invalid)?;
    let mut header = [0u8; 40];
    header[..4].copy_from_slice(&frame_len.to_be_bytes());
    header[4..N + 4].copy_from_slice(fixed);
    header[N + 4..N + 8].copy_from_slice(&payload_len.to_le_bytes());
    io.write_all(&header[..N + 8]).await?;
    io.write_all(payload).await?;
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
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(protocol, io, Self::MAX_MSG_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        if is_v2_protocol(protocol) {
            read_frame(protocol, io, Self::MAX_MSG_SIZE).await
        } else {
            let (view, payload) = read_raw_payload::<_, 8>(io, Self::MAX_MSG_SIZE).await?;
            Ok(BlockDataNetResponse {
                view: u64::from_le_bytes(view),
                payload,
            })
        }
    }

    async fn write_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(protocol, io, &req, WirePath::BlockData).await
    }

    async fn write_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        if is_v2_protocol(protocol) {
            write_frame(protocol, io, &res, WirePath::BlockData).await
        } else {
            write_raw_payload(io, &res.view.to_le_bytes(), &res.payload).await
        }
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
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(protocol, io, Self::MAX_MSG_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(protocol, io, Self::MAX_MSG_SIZE).await
    }

    async fn write_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(protocol, io, &req, WirePath::NativeDa).await
    }

    async fn write_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(protocol, io, &res, WirePath::NativeDa).await
    }
}

// ============================================================================
// Erasure-coded shard fetch — `/torus/native-da-shards/1.0` (Sprint 5 T3.1)
// ============================================================================
//
// SCAFFOLD (recovery-path Phase A of docs/plans/sprint5-erasure-coding.md).
// A lagging node that is missing a body fetches shard `i` from `k` DIFFERENT
// peers instead of the whole body from ONE — killing the s338 single-source
// hotspot. Each shard ships with its Merkle proof against the committed
// `erasure_root` so it is self-verifying (verify-then-reconstruct). A node that
// cannot gather `k` shards FALLS BACK to the existing whole-body
// `/torus/native-da/{1.0,2.0}` pull — additive, never a new wedge.
//
// STATUS: wire types + length-prefixed borsh codec are defined and unit-tested
// for round-trip. Behaviour registration + the serve/fetch/reconstruct loop are
// DEFERRED (TODO below) to keep this diff off the consensus-critical swarm event
// loop until a toolchain can compile it. `NATIVE_DA_SHARDS_PROTOCOL` mirrors the
// zstd 2.0-first per-peer-fallback precedent (mixed-binary safe).
//
// TODO(T3.1): (1) add a `native_da_shards: request_response::Behaviour<NativeDaShardsCodec>`
// field in behaviour.rs (mirror `native_da`); (2) serve from `CF_NATIVE_SHARDS`
// off the consensus loop; (3) on a body miss, request `k` shards from distinct
// peers, `verify_shard` each, `reconstruct`, then apply the body-hash backstop
// before absorbing into `CF_NATIVE_PENDING`.

/// Protocol id for erasure-shard fetch. `/1.0` only for now; a future `/2.0`
/// would add zstd framing exactly like `native-da/2.0`.
pub const NATIVE_DA_SHARDS_PROTOCOL: &str = "/torus/native-da-shards/1.0";

/// Request one shard of a body: `body_hash` identifies the erasure set,
/// `shard_index` selects the shard (`0..n`).
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug, Clone)]
pub struct NativeDaShardRequest {
    pub body_hash: [u8; 32],
    pub shard_index: u16,
}

/// Response carrying one shard plus everything a fetcher needs to verify it
/// standalone. `present == false` ⇒ the server does not custody this shard (the
/// fetcher tries another peer / index, or falls back to whole-body pull).
///
/// `proof` is the bottom-up sibling path (`torus_state::erasure::ShardProof`,
/// re-expressed as raw 32-byte hashes to keep the network crate free of a
/// state-crate dependency). `k`/`n`/`body_len`/`erasure_root` let the fetcher
/// verify-then-reconstruct without any out-of-band metadata.
#[derive(borsh::BorshSerialize, borsh::BorshDeserialize, Debug, Clone)]
pub struct NativeDaShardResponse {
    pub present: bool,
    pub shard_index: u16,
    pub shard_bytes: Vec<u8>,
    pub proof: Vec<[u8; 32]>,
    pub erasure_root: [u8; 32],
    pub k: u16,
    pub n: u16,
    pub body_len: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NativeDaShardsCodec;

impl NativeDaShardsCodec {
    // A single shard is at most ~body/k + proof + header — far below a whole
    // body. Its own tight cap (caps.rs, T3) rejects an oversize shard frame at a
    // bound that reflects a shard's true size, not a whole body's.
    const MAX_MSG_SIZE: usize = MAX_NATIVE_DA_SHARDS_MSG_SIZE;
}

#[async_trait]
impl libp2p::request_response::Codec for NativeDaShardsCodec {
    type Protocol = StreamProtocol;
    type Request = NativeDaShardRequest;
    type Response = NativeDaShardResponse;

    async fn read_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(protocol, io, Self::MAX_MSG_SIZE).await
    }

    async fn read_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(protocol, io, Self::MAX_MSG_SIZE).await
    }

    async fn write_request<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(protocol, io, &req, WirePath::NativeDa).await
    }

    async fn write_response<T>(
        &mut self,
        protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(protocol, io, &res, WirePath::NativeDa).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borsh::{BorshDeserialize, BorshSerialize};

    #[derive(BorshSerialize, BorshDeserialize)]
    struct LegacyDirectRequest {
        sender_key: [u8; 32],
        payload: Vec<u8>,
    }

    #[test]
    fn shared_direct_payload_preserves_legacy_wire_and_allocation() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        use std::sync::Arc;
        futures::executor::block_on(async {
            for len in [0, 31, 65_536] {
                for protocol in ["/torus/direct/1.0", "/torus/direct/2.0"] {
                    let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
                    let mut writer = FragmentedIo::new(&payload);
                    let bytes_ptr = payload.as_ptr();
                    let shared = Arc::new(payload);
                    let weak = Arc::downgrade(&shared);
                    let request = DirectRequest {
                        sender_key: [7; 32],
                        payload: shared.clone().into(),
                    };
                    assert_eq!(request.payload.as_ptr(), bytes_ptr);
                    let legacy = LegacyDirectRequest {
                        sender_key: request.sender_key,
                        payload: shared.as_ref().clone(),
                    };
                    assert_eq!(request.try_to_vec().unwrap(), legacy.try_to_vec().unwrap());
                    drop(shared);
                    let mut expected = Cursor::new(Vec::new());
                    let proto = StreamProtocol::new(protocol);
                    write_frame(&proto, &mut expected, &legacy, WirePath::Direct)
                        .await
                        .unwrap();
                    BorshCodec
                        .write_request(&proto, &mut writer, request)
                        .await
                        .unwrap();
                    assert_eq!(writer.cursor.get_ref(), expected.get_ref());
                    if !is_v2_protocol(&proto) {
                        assert_eq!(writer.borrowed_bytes, len);
                    }
                    assert!(
                        weak.upgrade().is_none(),
                        "last outbound owner released after write"
                    );
                    writer.cursor.set_position(0);
                    let decoded = BorshCodec.read_request(&proto, &mut writer).await.unwrap();
                    assert_eq!(&*decoded.payload, &legacy.payload);
                    assert!(matches!(decoded.payload, DirectPayload::Owned(_)));
                }
            }
        });
    }

    fn legacy_frame(value: &impl BorshSerialize) -> Vec<u8> {
        let body = value.try_to_vec().unwrap();
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        frame
    }

    #[test]
    fn raw_payload_codecs_match_legacy_bytes_and_size_boundaries() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            for len in [0, 1, 31, 65_536, MAX_DIRECT_MSG_SIZE - 36] {
                let req = DirectRequest {
                    sender_key: [0xa7; 32],
                    payload: (0..len).map(|i| (i % 251) as u8).collect::<Vec<_>>().into(),
                };
                let expected = legacy_frame(&req);
                let proto = StreamProtocol::new("/torus/direct/1.0");
                let mut out = Cursor::new(Vec::new());
                BorshCodec
                    .write_request(&proto, &mut out, req)
                    .await
                    .unwrap();
                assert_eq!(out.get_ref(), &expected);
                out.set_position(0);
                let actual = BorshCodec.read_request(&proto, &mut out).await.unwrap();
                let legacy: DirectRequest =
                    read_length_prefixed_borsh(&mut Cursor::new(expected), MAX_DIRECT_MSG_SIZE)
                        .await
                        .unwrap();
                assert_eq!(actual.sender_key, legacy.sender_key);
                assert_eq!(actual.payload, legacy.payload);
            }
            for len in [0, 1, 31, 65_536, MAX_BLOCK_DATA_MSG_SIZE - 12] {
                let resp = BlockDataNetResponse {
                    view: 0x1234_5678_9abc_def0,
                    payload: (0..len).map(|i| (i % 251) as u8).collect(),
                };
                let expected = legacy_frame(&resp);
                let proto = StreamProtocol::new("/torus/block-data/1.0");
                let mut out = Cursor::new(Vec::new());
                BlockDataCodec
                    .write_response(&proto, &mut out, resp)
                    .await
                    .unwrap();
                assert_eq!(out.get_ref(), &expected);
                out.set_position(0);
                let actual = BlockDataCodec
                    .read_response(&proto, &mut out)
                    .await
                    .unwrap();
                let legacy: BlockDataNetResponse =
                    read_length_prefixed_borsh(&mut Cursor::new(expected), MAX_BLOCK_DATA_MSG_SIZE)
                        .await
                        .unwrap();
                assert_eq!(actual.view, legacy.view);
                assert_eq!(actual.payload, legacy.payload);
            }
        });
    }

    #[test]
    fn raw_payload_malformed_frames_preserve_errors_and_consumption() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            // Differentially exercise every truncation of short frames, invalid
            // inner lengths, trailing bytes, and bytes belonging to a next frame.
            for direct in [false, true] {
                let (fixed, cap) = if direct {
                    (32, MAX_DIRECT_MSG_SIZE)
                } else {
                    (8, MAX_BLOCK_DATA_MSG_SIZE)
                };
                let valid = if direct {
                    legacy_frame(&DirectRequest {
                        sender_key: [7; 32],
                        payload: vec![9; 19].into(),
                    })
                } else {
                    legacy_frame(&BlockDataNetResponse {
                        view: 7,
                        payload: vec![9; 19],
                    })
                };
                let mut frames = Vec::new();
                for inner in [0, 1, 18, 19, 20, u32::MAX] {
                    let mut wire = valid.clone();
                    wire[4 + fixed..8 + fixed].copy_from_slice(&inner.to_le_bytes());
                    for end in 0..=wire.len() {
                        frames.push(wire[..end].to_vec());
                    }
                    wire.extend_from_slice(&valid);
                    frames.push(wire);
                }
                for outer in [
                    0,
                    1,
                    fixed as u32,
                    fixed as u32 + 3,
                    (cap + 1) as u32,
                    u32::MAX,
                ] {
                    let mut wire = valid.clone();
                    wire[..4].copy_from_slice(&outer.to_be_bytes());
                    frames.push(wire);
                }
                for wire in frames {
                    let mut reference = Cursor::new(wire.clone());
                    let mut candidate = Cursor::new(wire);
                    if direct {
                        let old: io::Result<DirectRequest> =
                            read_length_prefixed_borsh(&mut reference, cap).await;
                        let new = BorshCodec
                            .read_request(&StreamProtocol::new("/torus/direct/1.0"), &mut candidate)
                            .await;
                        assert_eq!(
                            old.as_ref().err().map(io::Error::kind),
                            new.as_ref().err().map(io::Error::kind)
                        );
                        if let (Ok(a), Ok(b)) = (old, new) {
                            assert_eq!(a.sender_key, b.sender_key);
                            assert_eq!(a.payload, b.payload);
                        }
                    } else {
                        let old: io::Result<BlockDataNetResponse> =
                            read_length_prefixed_borsh(&mut reference, cap).await;
                        let new = BlockDataCodec
                            .read_response(
                                &StreamProtocol::new("/torus/block-data/1.0"),
                                &mut candidate,
                            )
                            .await;
                        assert_eq!(
                            old.as_ref().err().map(io::Error::kind),
                            new.as_ref().err().map(io::Error::kind)
                        );
                        if let (Ok(a), Ok(b)) = (old, new) {
                            assert_eq!(a.view, b.view);
                            assert_eq!(a.payload, b.payload);
                        }
                    }
                    assert_eq!(reference.position(), candidate.position());
                }
            }
        });
    }

    struct FragmentedIo {
        cursor: futures::io::Cursor<Vec<u8>>,
        pending: bool,
        source_start: usize,
        source_len: usize,
        borrowed_bytes: usize,
        closed: bool,
        fail_at: Option<u64>,
        fail_close: bool,
    }

    impl FragmentedIo {
        fn new(source: &[u8]) -> Self {
            Self {
                cursor: futures::io::Cursor::new(Vec::new()),
                pending: false,
                source_start: source.as_ptr() as usize,
                source_len: source.len(),
                borrowed_bytes: 0,
                closed: false,
                fail_at: None,
                fail_close: false,
            }
        }

        fn yield_once(&mut self, cx: &mut std::task::Context<'_>) -> bool {
            self.pending = !self.pending;
            if self.pending {
                cx.waker().wake_by_ref();
            }
            self.pending
        }
    }

    impl AsyncRead for FragmentedIo {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<io::Result<usize>> {
            if self.yield_once(cx) {
                return std::task::Poll::Pending;
            }
            let len = buf.len().min(7);
            std::pin::Pin::new(&mut self.cursor).poll_read(cx, &mut buf[..len])
        }
    }

    impl AsyncWrite for FragmentedIo {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            if self.yield_once(cx) {
                return std::task::Poll::Pending;
            }
            let mut len = buf.len().min(7);
            if let Some(limit) = self.fail_at {
                if self.cursor.position() >= limit {
                    return std::task::Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                }
                len = len.min((limit - self.cursor.position()) as usize);
            }
            let pos = buf.as_ptr() as usize;
            if pos >= self.source_start && pos + len <= self.source_start + self.source_len {
                self.borrowed_bytes += len;
            }
            std::pin::Pin::new(&mut self.cursor).poll_write(cx, &buf[..len])
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if self.yield_once(cx) {
                return std::task::Poll::Pending;
            }
            self.closed = true;
            std::task::Poll::Ready(if self.fail_close {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            })
        }
    }

    #[test]
    fn raw_payload_fragmented_io_borrows_body_and_propagates_write_errors() {
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            for direct in [false, true] {
                for fail_at in [None, Some(0), Some(3), Some(45)] {
                    for fail_close in [false, true] {
                        let payload = vec![0x9d; 4096];
                        let mut io = FragmentedIo::new(&payload);
                        io.fail_at = fail_at;
                        io.fail_close = fail_close;
                        let (expected, result) = if direct {
                            let req = DirectRequest {
                                sender_key: [7; 32],
                                payload: payload.into(),
                            };
                            let expected = legacy_frame(&req);
                            (
                                expected,
                                BorshCodec
                                    .write_request(
                                        &StreamProtocol::new("/torus/direct/1.0"),
                                        &mut io,
                                        req,
                                    )
                                    .await,
                            )
                        } else {
                            let resp = BlockDataNetResponse { view: 7, payload };
                            let expected = legacy_frame(&resp);
                            (
                                expected,
                                BlockDataCodec
                                    .write_response(
                                        &StreamProtocol::new("/torus/block-data/1.0"),
                                        &mut io,
                                        resp,
                                    )
                                    .await,
                            )
                        };
                        if fail_at.is_some() || fail_close {
                            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
                            assert_eq!(io.closed, fail_at.is_none());
                            continue;
                        }
                        result.unwrap();
                        assert!(io.closed);
                        assert_eq!(
                            io.borrowed_bytes, 4096,
                            "writer must see original allocation"
                        );
                        assert_eq!(io.cursor.get_ref(), &expected);
                        io.cursor.set_position(0);
                        let payload = if direct {
                            BorshCodec
                                .read_request(&StreamProtocol::new("/torus/direct/1.0"), &mut io)
                                .await
                                .unwrap()
                                .payload.to_vec()
                        } else {
                            BlockDataCodec
                                .read_response(
                                    &StreamProtocol::new("/torus/block-data/1.0"),
                                    &mut io,
                                )
                                .await
                                .unwrap()
                                .payload
                        };
                        assert_eq!(payload, vec![0x9d; 4096]);
                    }
                }
            }
        });
    }

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
        assert!(NativeDaNetRequest::try_from_slice(&bytes)
            .unwrap()
            .hashes
            .is_empty());

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

    /// T3.1 (RED on HEAD — no shard types exist): the
    /// `/torus/native-da-shards/1.0` request/response round-trips through borsh,
    /// including the `present == false` (not-custodied) case and the proof path.
    #[test]
    fn native_da_shards_protocol_roundtrip() {
        let req = NativeDaShardRequest { body_hash: [7u8; 32], shard_index: 2 };
        let bytes = req.try_to_vec().expect("serialize shard request");
        let back = NativeDaShardRequest::try_from_slice(&bytes).expect("deserialize");
        assert_eq!(back.body_hash, req.body_hash);
        assert_eq!(back.shard_index, req.shard_index);

        // A found shard with a 2-level Merkle proof.
        let resp = NativeDaShardResponse {
            present: true,
            shard_index: 2,
            shard_bytes: vec![1, 2, 3, 4, 5],
            proof: vec![[9u8; 32], [8u8; 32]],
            erasure_root: [4u8; 32],
            k: 2,
            n: 3,
            body_len: 4096,
        };
        let bytes = resp.try_to_vec().expect("serialize shard response");
        let back = NativeDaShardResponse::try_from_slice(&bytes).expect("deserialize");
        assert!(back.present);
        assert_eq!(back.shard_bytes, resp.shard_bytes);
        assert_eq!(back.proof, resp.proof);
        assert_eq!(back.erasure_root, resp.erasure_root);
        assert_eq!((back.k, back.n, back.body_len), (2, 3, 4096));

        // Not-custodied: present=false with empty payload round-trips.
        let miss = NativeDaShardResponse {
            present: false,
            shard_index: 5,
            shard_bytes: Vec::new(),
            proof: Vec::new(),
            erasure_root: [0u8; 32],
            k: 2,
            n: 3,
            body_len: 0,
        };
        let bytes = miss.try_to_vec().unwrap();
        let back = NativeDaShardResponse::try_from_slice(&bytes).unwrap();
        assert!(!back.present, "not-custodied marker must survive");
        assert!(back.shard_bytes.is_empty());
    }

    /// Sprint 5 T1: a `/2.0` protocol round-trips zstd-framed borsh through every
    /// dispatching codec, and the on-wire bytes for a repetitive (JSON-like) body
    /// are genuinely smaller than the raw frame.
    #[test]
    fn zstd_v2_roundtrip_and_compression() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            // Representative body: repetitive order-JSON, the actual payload shape.
            let order_json = br#"{"market":"TORUS-PERP","side":"buy","price":"1.2345","size":"100.0","tif":"GTC","client_id":"bench-0000"}"#;
            let resp = NativeDaNetResponse {
                bodies: (0..200).map(|_| order_json.to_vec()).collect(),
            };
            let raw_len = resp.try_to_vec().unwrap().len();

            let proto_v2 = StreamProtocol::new("/torus/native-da/2.0");
            let mut codec = NativeDaCodec;
            let mut wbuf = Cursor::new(Vec::new());
            codec
                .write_response(&proto_v2, &mut wbuf, resp.clone())
                .await
                .unwrap();
            let wire = wbuf.into_inner();
            assert!(
                wire.len() < raw_len / 2,
                "zstd frame must be at least 2x smaller on order-JSON: wire {} vs raw {raw_len}",
                wire.len()
            );
            let mut rbuf = Cursor::new(wire);
            let back = codec.read_response(&proto_v2, &mut rbuf).await.unwrap();
            assert_eq!(back.bodies, resp.bodies);

            // Direct (push) codec dispatches on /2.0 the same way.
            let req = DirectRequest {
                sender_key: [7u8; 32],
                payload: vec![42u8; 64 * 1024].into(),
            };
            let proto_v2 = StreamProtocol::new("/torus/direct/2.0");
            let mut codec = BorshCodec;
            let mut wbuf = Cursor::new(Vec::new());
            codec
                .write_request(
                    &proto_v2,
                    &mut wbuf,
                    DirectRequest {
                        sender_key: req.sender_key,
                        payload: req.payload.clone(),
                    },
                )
                .await
                .unwrap();
            let mut rbuf = Cursor::new(wbuf.into_inner());
            let back = codec.read_request(&proto_v2, &mut rbuf).await.unwrap();
            assert_eq!(back.sender_key, req.sender_key);
            assert_eq!(back.payload, req.payload);

            // Block-data codec too.
            let resp = BlockDataNetResponse {
                view: 9,
                payload: vec![3u8; 32 * 1024],
            };
            let proto_v2 = StreamProtocol::new("/torus/block-data/2.0");
            let mut codec = BlockDataCodec;
            let mut wbuf = Cursor::new(Vec::new());
            codec
                .write_response(&proto_v2, &mut wbuf, resp.clone())
                .await
                .unwrap();
            let mut rbuf = Cursor::new(wbuf.into_inner());
            let back = codec.read_response(&proto_v2, &mut rbuf).await.unwrap();
            assert_eq!(back.payload, resp.payload);
        });
    }

    /// Sprint 5 T1: `/1.0` frames stay byte-identical raw length-prefixed borsh —
    /// the legacy wire format must not change underneath old peers.
    #[test]
    fn zstd_v1_frames_unchanged_raw() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            let resp = NativeDaNetResponse {
                bodies: vec![vec![1u8; 128], vec![]],
            };
            let raw = resp.try_to_vec().unwrap();
            let proto_v1 = StreamProtocol::new("/torus/native-da/1.0");
            let mut codec = NativeDaCodec;
            let mut wbuf = Cursor::new(Vec::new());
            codec
                .write_response(&proto_v1, &mut wbuf, resp.clone())
                .await
                .unwrap();
            let wire = wbuf.into_inner();
            assert_eq!(&wire[..4], (raw.len() as u32).to_be_bytes().as_slice());
            assert_eq!(&wire[4..], raw.as_slice());
            let mut rbuf = Cursor::new(wire);
            let back = codec.read_response(&proto_v1, &mut rbuf).await.unwrap();
            assert_eq!(back.bodies, resp.bodies);
        });
    }

    /// Sprint 5 T1 bomb guard: a tiny compressed frame that inflates past the
    /// path cap MUST be rejected at read (InvalidData), never allocated.
    #[test]
    fn zstd_read_rejects_decompressed_over_cap() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            // 9 MB of zeros compresses to ~a few KB but exceeds the 8 MB
            // native-da cap when inflated.
            let bomb_raw = vec![0u8; 9 * 1024 * 1024];
            let compressed = zstd::bulk::compress(&bomb_raw, 3).unwrap();
            let mut frame = (compressed.len() as u32).to_be_bytes().to_vec();
            frame.extend_from_slice(&compressed);
            let proto_v2 = StreamProtocol::new("/torus/native-da/2.0");
            let mut codec = NativeDaCodec;
            let mut rbuf = Cursor::new(frame);
            let err = codec
                .read_response(&proto_v2, &mut rbuf)
                .await
                .expect_err("decompression bomb must be rejected");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
    }

    /// Sprint 5 T1: corrupt zstd payloads error cleanly (InvalidData), no panic.
    #[test]
    fn zstd_read_rejects_corrupt_frame() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            let garbage = vec![0xABu8; 512];
            let mut frame = (garbage.len() as u32).to_be_bytes().to_vec();
            frame.extend_from_slice(&garbage);
            let proto_v2 = StreamProtocol::new("/torus/native-da/2.0");
            let mut codec = NativeDaCodec;
            let mut rbuf = Cursor::new(frame);
            let err = codec
                .read_response(&proto_v2, &mut rbuf)
                .await
                .expect_err("corrupt zstd frame must be rejected");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
    }

    /// Sprint 5 T3: a shard frame whose length prefix exceeds the (tighter)
    /// shard cap is rejected at read (InvalidData) before the oversized buffer is
    /// ever allocated — the shard protocol is `/1.0` (non-zstd), so the guard is
    /// the plain length-prefix check.
    #[test]
    fn shard_frame_over_cap_rejected() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            // Only the 4-byte length prefix is needed: the cap check fires before
            // the body is read. Claim one byte past the shard cap.
            let over = (MAX_NATIVE_DA_SHARDS_MSG_SIZE + 1) as u32;
            let frame = over.to_be_bytes().to_vec();
            let proto_v1 = StreamProtocol::new(NATIVE_DA_SHARDS_PROTOCOL);
            let mut codec = NativeDaShardsCodec;
            let mut rbuf = Cursor::new(frame);
            let err = codec
                .read_response(&proto_v1, &mut rbuf)
                .await
                .expect_err("over-cap shard frame must be rejected");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        });
    }

    /// Sprint 5 T1 ratio probe: prints level 1/3/6 ratios on a representative
    /// 500-order batch body so the level choice is data-driven; asserts the
    /// shipped level achieves >=2x.
    #[test]
    fn zstd_level_ratio_probe() {
        // Varied per-order fields — identical bodies would print a fantasy
        // 400x; this measures the realistic repetitive-but-unique shape.
        let resp = NativeDaNetResponse {
            bodies: (0..500u32)
                .map(|i| {
                    format!(
                        r#"{{"market":"TORUS-PERP","side":"{}","price":"{}.{:04}","size":"{}.{}","tif":"GTC","client_id":"bench-{:08x}","nonce":{}}}"#,
                        if i % 2 == 0 { "buy" } else { "sell" },
                        1 + (i % 7),
                        (i * 37) % 10_000,
                        10 + (i % 990),
                        i % 10,
                        i.wrapping_mul(0x9E37_79B9),
                        1_000_000 + i,
                    )
                    .into_bytes()
                })
                .collect(),
        };
        let raw = resp.try_to_vec().unwrap();
        for level in [1, 3, 6] {
            let c = zstd::bulk::compress(&raw, level).unwrap();
            println!(
                "zstd level {level}: {} -> {} bytes ({:.1}x)",
                raw.len(),
                c.len(),
                raw.len() as f64 / c.len() as f64
            );
        }
        let shipped = zstd::bulk::compress(&raw, ZSTD_WIRE_LEVEL).unwrap();
        assert!(
            shipped.len() * 2 < raw.len(),
            "shipped level must achieve >=2x on order-JSON"
        );
    }

    /// Sprint 5 T5: per-path wire compression counters move on a /2.0 write.
    #[test]
    fn zstd_wire_stats_counted() {
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        futures::executor::block_on(async {
            let before = wire_compression_stats();
            let pre_native = before.iter().find(|(p, _, _)| *p == "native-da").unwrap().1;
            let resp = NativeDaNetResponse {
                bodies: vec![vec![5u8; 4096]; 8],
            };
            let proto_v2 = StreamProtocol::new("/torus/native-da/2.0");
            let mut codec = NativeDaCodec;
            let mut wbuf = Cursor::new(Vec::new());
            codec
                .write_response(&proto_v2, &mut wbuf, resp)
                .await
                .unwrap();
            let after = wire_compression_stats();
            let (_, pre, wire) = *after.iter().find(|(p, _, _)| *p == "native-da").unwrap();
            assert!(pre > pre_native, "pre-compress byte counter must advance");
            assert!(
                wire > 0 && wire < pre,
                "on-wire counter must advance and stay below pre"
            );
        });
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
