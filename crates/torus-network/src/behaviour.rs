use std::time::Duration;

use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    allow_block_list, connection_limits, gossipsub, identify, kad, request_response, StreamProtocol,
};

use crate::codec::{
    BlockDataCodec, BorshCodec, NativeDaCodec, NativeDaShardsCodec, NATIVE_DA_SHARDS_PROTOCOL,
};
use crate::sync::{SyncRequest, SyncResponse};

pub const CONSENSUS_TOPIC: &str = "/torus/consensus/1.0";
pub const TX_TOPIC: &str = "/torus/transactions/1.0";
pub const NATIVE_ACTION_TOPIC: &str = "/torus/native-actions/1.0";
// The `/torus/native-actions/2.0` zstd gossip topic was REMOVED (s364): a v2
// subscriber had to decompress every frame an attacker published (gossipsub
// cannot negotiate compression per-peer), a cluster-wide DoS that
// `--no-gossip-zstd` could not close — it only stopped local publishing. The
// per-peer-negotiated zstd survives on the request_response body protocols
// (`/torus/{direct,block-data,native-da}/2.0`), never on gossip.

#[derive(NetworkBehaviour)]
pub struct TorusBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub direct: request_response::Behaviour<BorshCodec>,
    pub sync_proto: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    /// Connection limits enforcement (Phase 3: 3.1.7).
    pub connection_limits: connection_limits::Behaviour,
    /// Dedicated block-data fetch protocol (hybrid pipelining Task 2).
    pub block_data: request_response::Behaviour<BlockDataCodec>,
    /// Dedicated native-action DA fetch protocol (Phase C Task 5): serves
    /// native-action bodies by-hash as the RARE pull-fallback for CompactBlock
    /// reconstruction. Push stays primary; this fires only on a miss.
    pub native_da: request_response::Behaviour<NativeDaCodec>,
    /// Erasure-shard fetch protocol (Sprint 5 T6): serves a single erasure shard
    /// of a native-action body by `(body_hash, index)` so a lagging node gathers
    /// `k` shards from `k` DIFFERENT peers instead of pulling the whole body from
    /// one. `/1.0` only for now (a future `/2.0` adds zstd like native-da/2.0);
    /// negotiated per-peer, so a pre-shard binary simply never advertises it and
    /// the fetcher falls back to the whole-body pull (mixed-binary safe).
    pub native_da_shards: request_response::Behaviour<NativeDaShardsCodec>,
    /// Peer block list for banning (Phase 3: 3.1.7).
    pub block_list: allow_block_list::Behaviour<allow_block_list::BlockedPeers>,
}

/// Default gossipsub heartbeat interval (ms). Single source of truth shared
/// with `NetworkConfig::default()`. S391: the intended value was always 100ms
/// (config.rs), but the behaviour hardcoded 500ms and never read the config —
/// mesh re-grafting / IWANT retransmission after a WAN blip healed 5x slower
/// than designed.
pub const DEFAULT_GOSSIPSUB_HEARTBEAT_MS: u64 = 100;

/// Default per-peer connection-handler send-queue length (B3 send-queue
/// hygiene). libp2p's own default is 5000, of which len/2 = 2500 is the
/// priority-queue soft cap — i.e. up to 2500 queued `Publish` messages PER
/// PEER, shared across ALL topics, with queued publishes silently abandoned
/// after 5 s. A 1–3 KB consensus proposal sits FIFO behind megabytes of
/// queued 128 KB native-action batches and gets dropped invisibly (the
/// observed 21.5 → 1.15 blk/s collapse). 512 bounds the per-peer backlog to
/// 256 publishes ≈ 32 MB worst-case ≈ ~250 ms at 1 Gbps: overload becomes
/// fast, VISIBLE drops (SlowPeer / AllQueuesFull counters) instead of 5 s of
/// bufferbloat. Rollback to exact-today behavior: `TORUS_GOSSIP_QUEUE_LEN=5000`.
pub const DEFAULT_GOSSIPSUB_QUEUE_LEN: usize = 512;

/// Build the consensus gossipsub config with the given heartbeat interval,
/// transport frame cap, and per-peer connection-handler queue length.
/// Extracted (and unit-tested) so none of these values can silently drift
/// from their source of truth again — S391: this site hardcoded a 500ms
/// heartbeat while config said 100ms; O5: it hardcoded a 2 MiB frame cap
/// while the accept gates it must dominate live in `NetworkConfig` (ladder
/// ordering asserted in `caps::tests`); B3: it silently inherited libp2p's
/// 5000-deep per-peer send queue (5 s of invisible head-of-line blocking).
pub fn gossipsub_config(
    heartbeat_ms: u64,
    max_transmit: usize,
    queue_len: usize,
) -> Result<gossipsub::Config, String> {
    gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_millis(heartbeat_ms))
        .max_transmit_size(max_transmit)
        .connection_handler_queue_len(queue_len)
        .validation_mode(gossipsub::ValidationMode::Strict)
        .build()
        .map_err(|e| format!("gossipsub config: {e}"))
}

impl TorusBehaviour {
    pub fn new(key: &libp2p::identity::Keypair) -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_limits(key, 100)
    }

    pub fn with_limits(
        key: &libp2p::identity::Keypair,
        max_peers: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_limits_and_heartbeat(
            key,
            max_peers,
            DEFAULT_GOSSIPSUB_HEARTBEAT_MS,
            crate::caps::GOSSIP_MAX_TRANSMIT_SIZE,
            // Env-honoring, same as `NetworkConfig::default()` — the fallback
            // constructors must match a default-configured node.
            crate::config::gossip_queue_len(),
        )
    }

    pub fn with_limits_and_heartbeat(
        key: &libp2p::identity::Keypair,
        max_peers: usize,
        gossipsub_heartbeat_ms: u64,
        gossip_max_transmit: usize,
        gossipsub_queue_len: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let peer_id = key.public().to_peer_id();

        // GossipSub
        let gossipsub_config = gossipsub_config(
            gossipsub_heartbeat_ms,
            gossip_max_transmit,
            gossipsub_queue_len,
        )?;
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(key.clone()),
            gossipsub_config,
        )
        .map_err(|e| format!("gossipsub: {e}"))?;

        // Direct message request-response (borsh codec). Sprint 5: the /2.0
        // variant is zstd-framed; listed FIRST so multistream-select prefers it,
        // falling back per-peer to raw /1.0 with old binaries (mixed-binary safe).
        let direct = request_response::Behaviour::<BorshCodec>::new(
            [
                (
                    StreamProtocol::new("/torus/direct/2.0"),
                    request_response::ProtocolSupport::Full,
                ),
                (
                    StreamProtocol::new("/torus/direct/1.0"),
                    request_response::ProtocolSupport::Full,
                ),
            ],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Block data fetch request-response (borsh codec, hybrid pipelining).
        // /2.0 = zstd-framed, negotiated per peer (Sprint 5).
        let block_data = request_response::Behaviour::<BlockDataCodec>::new(
            [
                (
                    StreamProtocol::new("/torus/block-data/2.0"),
                    request_response::ProtocolSupport::Full,
                ),
                (
                    StreamProtocol::new("/torus/block-data/1.0"),
                    request_response::ProtocolSupport::Full,
                ),
            ],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Native-action DA fetch request-response (borsh codec, Phase C Task 5):
        // serves CompactBlock bodies by-hash for the RARE pull-fallback.
        // /2.0 = zstd-framed, negotiated per peer (Sprint 5).
        let native_da = request_response::Behaviour::<NativeDaCodec>::new(
            [
                (
                    StreamProtocol::new("/torus/native-da/2.0"),
                    request_response::ProtocolSupport::Full,
                ),
                (
                    StreamProtocol::new("/torus/native-da/1.0"),
                    request_response::ProtocolSupport::Full,
                ),
            ],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Erasure-shard fetch request-response (borsh codec, Sprint 5 T6). Single
        // `/1.0` protocol, negotiated per-peer: a pre-shard peer never advertises
        // it, so `send_request` to it yields an unsupported-protocol OutboundFailure
        // that the fetcher counts as "no shard" and falls back (T9). Mixed-binary
        // safe by the same construction the zstd 2.0-first work proved (s356).
        let native_da_shards = request_response::Behaviour::<NativeDaShardsCodec>::new(
            [(
                StreamProtocol::new(NATIVE_DA_SHARDS_PROTOCOL),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Block sync request-response (cbor codec)
        let sync_proto = request_response::cbor::Behaviour::<SyncRequest, SyncResponse>::new(
            [(
                StreamProtocol::new("/torus/sync/1.0"),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Kademlia DHT — server mode so nodes respond to bootstrap queries
        // and share peer addresses with each other.
        let mut kademlia = kad::Behaviour::new(peer_id, kad::store::MemoryStore::new(peer_id));
        kademlia.set_mode(Some(kad::Mode::Server));

        // Identify protocol
        let identify =
            identify::Behaviour::new(identify::Config::new("/torus/1.0".into(), key.public()));

        // Connection limits (Phase 3: 3.1.7)
        // 80% inbound, 20% reserved for outbound
        let max_inbound = (max_peers * 80) / 100;
        let max_outbound = max_peers - max_inbound;
        let limits = connection_limits::ConnectionLimits::default()
            .with_max_established_incoming(Some(max_inbound as u32))
            .with_max_established_outgoing(Some(max_outbound as u32))
            .with_max_established(Some(max_peers as u32))
            .with_max_established_per_peer(Some(2));
        let connection_limits = connection_limits::Behaviour::new(limits);

        // Block list (Phase 3: 3.1.7)
        let block_list = allow_block_list::Behaviour::default();

        Ok(Self {
            gossipsub,
            direct,
            block_data,
            native_da,
            native_da_shards,
            sync_proto,
            kademlia,
            identify,
            connection_limits,
            block_list,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S391: `NetworkConfig.gossipsub_heartbeat_ms` was dead config — the
    /// behaviour hardcoded a 500ms heartbeat while config.rs said 100ms. The
    /// heartbeat must come from the caller.
    #[test]
    fn gossipsub_heartbeat_comes_from_caller() {
        let cfg = gossipsub_config(
            100,
            crate::caps::GOSSIP_MAX_TRANSMIT_SIZE,
            DEFAULT_GOSSIPSUB_QUEUE_LEN,
        )
        .expect("build gossipsub config");
        assert_eq!(cfg.heartbeat_interval(), Duration::from_millis(100));
        let cfg = gossipsub_config(
            500,
            crate::caps::GOSSIP_MAX_TRANSMIT_SIZE,
            DEFAULT_GOSSIPSUB_QUEUE_LEN,
        )
        .expect("build gossipsub config");
        assert_eq!(cfg.heartbeat_interval(), Duration::from_millis(500));
    }

    /// The crate default and NetworkConfig's default must be the same value
    /// (single source of truth), so `TorusBehaviour::new`/`with_limits`
    /// fallbacks match a default-configured node.
    #[test]
    fn network_config_default_heartbeat_matches_crate_default() {
        assert_eq!(
            crate::config::NetworkConfig::default().gossipsub_heartbeat_ms,
            DEFAULT_GOSSIPSUB_HEARTBEAT_MS
        );
    }

    /// T6: the erasure-shard fetch protocol is registered on the behaviour. This
    /// is a compile-gate (the `NetworkBehaviour` derive fails without the field)
    /// plus a construction check; real coverage is the codec round-trip and the
    /// serve/fetch tests (T7/T8).
    #[test]
    fn behaviour_registers_shard_protocol() {
        let key = libp2p::identity::Keypair::generate_ed25519();
        let b = TorusBehaviour::new(&key).expect("construct behaviour with shard protocol");
        let _ = &b.native_da_shards;
        assert_eq!(NATIVE_DA_SHARDS_PROTOCOL, "/torus/native-da-shards/1.0");
    }

    /// The wired constructor honors the requested heartbeat (construction
    /// succeeds; the interval itself is asserted via `gossipsub_config` above
    /// since libp2p's Behaviour does not expose its config).
    #[test]
    fn with_limits_and_heartbeat_constructs() {
        let key = libp2p::identity::Keypair::generate_ed25519();
        TorusBehaviour::with_limits_and_heartbeat(
            &key,
            50,
            250,
            crate::caps::GOSSIP_MAX_TRANSMIT_SIZE,
            DEFAULT_GOSSIPSUB_QUEUE_LEN,
        )
        .expect("behaviour with custom heartbeat");
    }

    /// O5: the transmit cap must come from the caller (config-wired), not a
    /// hardcoded literal at the builder call site — and the default path must
    /// stay byte-identical to the shipped 2 MiB behavior.
    #[test]
    fn gossipsub_transmit_cap_is_configurable_not_hardcoded() {
        let cfg = gossipsub_config(100, 3 * 1024 * 1024, DEFAULT_GOSSIPSUB_QUEUE_LEN)
            .expect("build gossipsub config");
        assert_eq!(cfg.max_transmit_size(), 3 * 1024 * 1024);
        let default_cfg = gossipsub_config(
            100,
            crate::caps::GOSSIP_MAX_TRANSMIT_SIZE,
            DEFAULT_GOSSIPSUB_QUEUE_LEN,
        )
        .expect("build gossipsub config");
        assert_eq!(default_cfg.max_transmit_size(), 2 * 1024 * 1024);
    }

    /// B3 (send-queue hygiene): `connection_handler_queue_len` must come from
    /// the caller, not libp2p's silent 5000 default — the default queue holds
    /// up to 2500 queued Publish messages per peer (len/2 soft cap), i.e.
    /// megabytes of bulk a 1–3 KB proposal sits FIFO behind for up to the 5 s
    /// publish abandonment window. Mirrors the S391 heartbeat test: the value
    /// must be config-wired so it can never silently drift again.
    #[test]
    fn gossipsub_queue_len_comes_from_caller() {
        let cfg = gossipsub_config(100, crate::caps::GOSSIP_MAX_TRANSMIT_SIZE, 512)
            .expect("build gossipsub config");
        assert_eq!(cfg.connection_handler_queue_len(), 512);
        // 5000 is libp2p's own default: TORUS_GOSSIP_QUEUE_LEN=5000 must
        // restore exact-today behavior (the documented B3 rollback).
        let cfg = gossipsub_config(100, crate::caps::GOSSIP_MAX_TRANSMIT_SIZE, 5000)
            .expect("build gossipsub config");
        assert_eq!(cfg.connection_handler_queue_len(), 5000);
    }

    /// The crate default and NetworkConfig's default must be the same value
    /// (single source of truth), same contract as the heartbeat test above.
    /// 512 is the B3 hygiene default: priority soft cap 256 publishes/peer
    /// ≈ 32 MB worst-case of 128 KB batches — bounded latency instead of 5 s
    /// of invisible backlog.
    #[test]
    fn network_config_default_queue_len_matches_crate_default() {
        assert_eq!(DEFAULT_GOSSIPSUB_QUEUE_LEN, 512);
        assert_eq!(
            crate::config::NetworkConfig::default().gossipsub_queue_len,
            DEFAULT_GOSSIPSUB_QUEUE_LEN
        );
    }
}
