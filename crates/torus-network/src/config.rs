use std::path::PathBuf;

use libp2p::{multiaddr::Protocol, Multiaddr, PeerId};

/// Configuration for the Torus p2p network layer.
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    /// Address to listen on for incoming connections.
    pub listen_addr: Multiaddr,
    /// Bootstrap peer addresses for initial connection.
    pub bootstrap_peers: Vec<(PeerId, Multiaddr)>,
    /// Maximum number of peers to maintain.
    pub max_peers: usize,
    /// GossipSub heartbeat interval in milliseconds.
    pub gossipsub_heartbeat_ms: u64,
    /// Maximum message size for consensus gossipsub (bytes).
    pub max_consensus_message_size: usize,
    /// Maximum message size for transaction gossipsub (bytes).
    pub max_tx_message_size: usize,
    /// Transaction gossip rate limit per peer (tx/sec).
    pub tx_rate_limit_per_peer: u32,
    /// Transaction dedup window in seconds.
    pub tx_dedup_window_secs: u64,
    /// Consensus message rate limit per peer (msg/sec) (Phase 3: 3.1.7).
    pub consensus_rate_limit_per_peer: u32,
    /// Path to the peer ban list JSON file (Phase 3: 3.1.7).
    pub ban_list_path: Option<PathBuf>,
    /// Publish native-action gossip batches zstd-compressed on the v2 topic
    /// (Sprint 5). Receiving is always dual-topic; flip this ONLY once every
    /// validator runs a 2.0-capable binary — old peers cannot read v2.
    pub gossip_zstd: bool,
}

impl NetworkConfig {
    /// Parse a comma-separated list of multiaddrs (each with `/p2p/<peer_id>`)
    /// into bootstrap peer entries. Returns entries that parsed successfully;
    /// logs warnings for invalid ones.
    pub fn parse_bootstrap_peers(peers_csv: &str) -> Vec<(PeerId, Multiaddr)> {
        let mut result = Vec::new();
        for addr_str in peers_csv.split(',').filter(|s| !s.is_empty()) {
            let addr_str = addr_str.trim();
            let addr: Multiaddr = match addr_str.parse() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let peer_id = addr.iter().find_map(|p| {
                if let Protocol::P2p(id) = p {
                    Some(id)
                } else {
                    None
                }
            });
            if let Some(pid) = peer_id {
                let transport_addr = addr
                    .iter()
                    .filter(|p| !matches!(p, Protocol::P2p(_)))
                    .collect();
                result.push((pid, transport_addr));
            }
        }
        result
    }
}

pub const TESTNET_BOOTSTRAP_PEERS: &[&str] = &[
    "/ip4/95.111.231.121/udp/30333/quic-v1/p2p/12D3KooWQeKf21QBchGQUr25U6w6yNB4P78PPQZivhHRAqFnMK24",
];

impl NetworkConfig {
    pub fn default_bootstrap_peers() -> Vec<(PeerId, Multiaddr)> {
        let csv = TESTNET_BOOTSTRAP_PEERS.join(",");
        Self::parse_bootstrap_peers(&csv)
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: "/ip4/0.0.0.0/udp/0/quic-v1".parse().unwrap(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            gossipsub_heartbeat_ms: 100,
            max_consensus_message_size: 256 * 1024,
            max_tx_message_size: 128 * 1024,
            tx_rate_limit_per_peer: 100,
            tx_dedup_window_secs: 60,
            consensus_rate_limit_per_peer: 50,
            ban_list_path: None,
            gossip_zstd: false,
        }
    }
}
