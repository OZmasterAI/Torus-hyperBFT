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
    /// Maximum message size for consensus gossipsub (bytes). ACCEPT gate only
    /// — enforced on RECEIVE (swarm oversized-consensus path: drop + penalize
    /// the author); there is no send-side check. O5 sized it for the worst
    /// legal compact proposal (EVM gas budget 5M / 16 gas-per-calldata-byte
    /// ≈ 312.5 KB inline + manifest) with ~3x headroom, ≤ gossip transmit
    /// (ladder asserted in `caps::tests`). Senders must NOT produce bigger
    /// consensus messages until the whole fleet carries at least this accept
    /// value — stragglers drop AND penalize.
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
    /// Accept loopback/private/link-local addresses into the kademlia address
    /// book (identify-advertised and DHT-learned). Off by default: on a public
    /// network these entries are never dialable from here (a NAT'd peer
    /// advertising `127.0.0.1`, a container advertising its docker subnet) and
    /// only produce dial storms against ourselves or dead endpoints. Enable on
    /// devnets / single-host meshes where the fabric IS a private subnet.
    /// Explicit `--p2p-peers` entries are always exempt — operator intent wins.
    pub allow_private_addrs: bool,
}

/// Whether `addr`'s IP component is globally dialable — i.e. not loopback,
/// RFC1918-private, link-local, CGNAT, unspecified, or broadcast. Multiaddrs
/// without an IP component (e.g. `/dns4/...`) are considered dialable.
pub fn is_global_addr(addr: &Multiaddr) -> bool {
    for p in addr.iter() {
        match p {
            Protocol::Ip4(ip) => {
                let o = ip.octets();
                return !(ip.is_loopback()
                    || ip.is_private()
                    || ip.is_link_local()
                    || ip.is_unspecified()
                    || ip.is_broadcast()
                    // CGNAT 100.64.0.0/10
                    || (o[0] == 100 && (o[1] & 0xC0) == 64));
            }
            Protocol::Ip6(ip) => {
                let s = ip.segments();
                return !(ip.is_loopback()
                    || ip.is_unspecified()
                    // unique-local fc00::/7
                    || (s[0] & 0xfe00) == 0xfc00
                    // link-local fe80::/10
                    || (s[0] & 0xffc0) == 0xfe80);
            }
            _ => continue,
        }
    }
    true
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
            gossipsub_heartbeat_ms: crate::behaviour::DEFAULT_GOSSIPSUB_HEARTBEAT_MS,
            max_consensus_message_size: 1024 * 1024, // O5: was 256 KB (< EVM worst case — livelock trap)
            max_tx_message_size: 128 * 1024,
            tx_rate_limit_per_peer: 100,
            tx_dedup_window_secs: 60,
            consensus_rate_limit_per_peer: 50,
            ban_list_path: None,
            allow_private_addrs: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Multiaddr {
        s.parse().unwrap()
    }

    #[test]
    fn non_global_addresses_rejected() {
        for s in [
            "/ip4/127.0.0.1/udp/30333/quic-v1",   // loopback (dial-self)
            "/ip4/10.1.2.3/udp/30333/quic-v1",    // RFC1918
            "/ip4/172.28.0.20/udp/30333/quic-v1", // RFC1918 (docker devnet subnet)
            "/ip4/192.168.1.5/tcp/30333",         // RFC1918
            "/ip4/169.254.7.7/udp/30333/quic-v1", // link-local
            "/ip4/100.64.0.9/udp/30333/quic-v1",  // CGNAT
            "/ip4/0.0.0.0/udp/30333/quic-v1",     // unspecified
            "/ip6/::1/udp/30333/quic-v1",         // v6 loopback
            "/ip6/fe80::1/udp/30333/quic-v1",     // v6 link-local
            "/ip6/fd00::1/udp/30333/quic-v1",     // v6 unique-local
        ] {
            assert!(!is_global_addr(&addr(s)), "{s} must be rejected");
        }
    }

    #[test]
    fn global_addresses_accepted() {
        for s in [
            "/ip4/95.111.231.121/udp/30333/quic-v1", // live seed
            "/ip4/84.32.108.220/udp/30333/quic-v1",  // val1
            "/ip4/100.128.0.1/udp/30333/quic-v1",    // just past CGNAT range
            "/ip6/2a01:4f8::1/udp/30333/quic-v1",    // public v6
            "/dns4/seed.example.org/udp/30333/quic-v1", // no IP component
        ] {
            assert!(is_global_addr(&addr(s)), "{s} must be accepted");
        }
    }
}
