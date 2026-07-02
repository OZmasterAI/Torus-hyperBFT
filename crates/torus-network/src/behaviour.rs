use std::time::Duration;

use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    allow_block_list, connection_limits, gossipsub, identify, kad, request_response,
    StreamProtocol,
};

use crate::codec::{BlockDataCodec, BorshCodec, NativeDaCodec};
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
    /// Peer block list for banning (Phase 3: 3.1.7).
    pub block_list: allow_block_list::Behaviour<allow_block_list::BlockedPeers>,
}

/// Default gossipsub heartbeat interval (ms). Single source of truth shared
/// with `NetworkConfig::default()`. S391: the intended value was always 100ms
/// (config.rs), but the behaviour hardcoded 500ms and never read the config —
/// mesh re-grafting / IWANT retransmission after a WAN blip healed 5x slower
/// than designed.
pub const DEFAULT_GOSSIPSUB_HEARTBEAT_MS: u64 = 100;

/// Build the consensus gossipsub config with the given heartbeat interval.
/// Extracted (and unit-tested) so the heartbeat can never silently drift from
/// `NetworkConfig.gossipsub_heartbeat_ms` again.
pub fn gossipsub_config(heartbeat_ms: u64) -> Result<gossipsub::Config, String> {
    gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_millis(heartbeat_ms))
        .max_transmit_size(2 * 1024 * 1024)
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
        Self::with_limits_and_heartbeat(key, max_peers, DEFAULT_GOSSIPSUB_HEARTBEAT_MS)
    }

    pub fn with_limits_and_heartbeat(
        key: &libp2p::identity::Keypair,
        max_peers: usize,
        gossipsub_heartbeat_ms: u64,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let peer_id = key.public().to_peer_id();

        // GossipSub
        let gossipsub_config = gossipsub_config(gossipsub_heartbeat_ms)?;
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
        let cfg = gossipsub_config(100).expect("build gossipsub config");
        assert_eq!(cfg.heartbeat_interval(), Duration::from_millis(100));
        let cfg = gossipsub_config(500).expect("build gossipsub config");
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

    /// The wired constructor honors the requested heartbeat (construction
    /// succeeds; the interval itself is asserted via `gossipsub_config` above
    /// since libp2p's Behaviour does not expose its config).
    #[test]
    fn with_limits_and_heartbeat_constructs() {
        let key = libp2p::identity::Keypair::generate_ed25519();
        TorusBehaviour::with_limits_and_heartbeat(&key, 50, 250)
            .expect("behaviour with custom heartbeat");
    }
}
