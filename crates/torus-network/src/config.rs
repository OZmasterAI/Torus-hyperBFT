use libp2p::{Multiaddr, PeerId};

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
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: "/ip4/0.0.0.0/udp/0/quic-v1".parse().unwrap(),
            bootstrap_peers: Vec::new(),
            max_peers: 100,
            gossipsub_heartbeat_ms: 500,
            max_consensus_message_size: 256 * 1024,
            max_tx_message_size: 128 * 1024,
            tx_rate_limit_per_peer: 100,
            tx_dedup_window_secs: 60,
        }
    }
}
