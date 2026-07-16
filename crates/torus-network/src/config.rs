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
    /// GossipSub per-peer connection-handler send-queue length (B3 send-queue
    /// hygiene). One queue pair per peer, shared across ALL topics: len/2 is
    /// the soft cap on queued `Publish` messages, so libp2p's 5000 default
    /// let a consensus proposal sit FIFO behind up to 2500 bulk batches for
    /// the full 5 s publish-abandonment window — invisibly. Default 512
    /// bounds the backlog; `TORUS_GOSSIP_QUEUE_LEN=5000` restores exact-today
    /// behavior (rollback knob).
    pub gossipsub_queue_len: usize,
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
    /// B2 consensus isolation: fan consensus broadcasts over `/torus/direct`
    /// to every registered validator (reusing the vote-path send/buffer/redial
    /// machinery) instead of relying on gossipsub delivery, where a 1–3 KB
    /// proposal queues FIFO behind megabytes of bulk native-action batches and
    /// is silently abandoned after 5 s. Sender-side only — every deployed
    /// binary already parses hotstuff messages arriving on `/torus/direct`
    /// (the vote path), so this is mixed-fleet safe and can be flipped one
    /// node at a time. Default OFF: `TORUS_CONSENSUS_DIRECT_FAN` unset/`0` is
    /// exact-today behavior (the documented rollback).
    pub consensus_direct_fan: bool,
    /// B2: with the direct fan ON, ALSO publish consensus broadcasts to
    /// gossip (default ON) so non-validator observers (RPC nodes follow
    /// consensus via gossip) and not-yet-flipped validators keep their live
    /// feed during staged rollout. Ignored while the fan is off — a
    /// (fan=off, mirror=off) misconfiguration must never silently mute
    /// consensus. `TORUS_CONSENSUS_GOSSIP_MIRROR=0` is the fully-isolated
    /// end-state once the whole fleet runs the fan.
    pub consensus_gossip_mirror: bool,
    /// Accept loopback/private/link-local addresses into the kademlia address
    /// book (identify-advertised and DHT-learned). Off by default: on a public
    /// network these entries are never dialable from here (a NAT'd peer
    /// advertising `127.0.0.1`, a container advertising its docker subnet) and
    /// only produce dial storms against ourselves or dead endpoints. Enable on
    /// devnets / single-host meshes where the fabric IS a private subnet.
    /// Explicit `--p2p-peers` entries are always exempt — operator intent wins.
    pub allow_private_addrs: bool,
}

/// Effective gossipsub per-peer send-queue length: `TORUS_GOSSIP_QUEUE_LEN`
/// overrides the compiled default (512) PER NODE, read once at first use.
/// Node-local and transport-only — safe to A/B on a single validator without
/// coordination (no wire format involved). `=5000` restores libp2p's shipped
/// default, i.e. exact pre-B3 behavior (the documented rollback).
pub fn gossip_queue_len() -> usize {
    static LEN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LEN.get_or_init(|| {
        parse_gossip_queue_len(std::env::var("TORUS_GOSSIP_QUEUE_LEN").ok().as_deref())
    })
}

/// Pure parse seam for [`gossip_queue_len`] (unit-testable without touching
/// process env, same idiom as `TORUS_HASH_ONLY_PUSH_THRESHOLD`). Unset,
/// unparsable, or zero (a zero-length queue could never carry a message)
/// falls back to the crate default.
fn parse_gossip_queue_len(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(crate::behaviour::DEFAULT_GOSSIPSUB_QUEUE_LEN)
}

/// B2: effective `TORUS_CONSENSUS_DIRECT_FAN` — fan consensus broadcasts over
/// `/torus/direct` to every registered validator. Read once at first use.
/// Node-local and sender-side only (receivers need no change), so it is safe
/// to flip one validator at a time. Default OFF = exact-today behavior (the
/// documented rollback).
pub fn consensus_direct_fan() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        parse_env_flag(
            std::env::var("TORUS_CONSENSUS_DIRECT_FAN").ok().as_deref(),
            false,
        )
    })
}

/// B2: effective `TORUS_CONSENSUS_GOSSIP_MIRROR` — keep publishing consensus
/// broadcasts to gossip while the direct fan is on, so observers and
/// not-yet-flipped nodes stay fed during staged rollout. Read once at first
/// use. Default ON; `=0` is the fully-isolated end-state.
pub fn consensus_gossip_mirror() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        parse_env_flag(
            std::env::var("TORUS_CONSENSUS_GOSSIP_MIRROR").ok().as_deref(),
            true,
        )
    })
}

/// Pure parse seam for the B2 boolean env knobs (unit-testable without
/// touching process env, same idiom as [`parse_gossip_queue_len`]). Accepts
/// `1`/`true`/`on`/`yes` and `0`/`false`/`off`/`no` (case- and
/// whitespace-insensitive); unset or unrecognized falls back to `default` so
/// a typo can never silently flip a consensus-path knob.
fn parse_env_flag(raw: Option<&str>, default: bool) -> bool {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if ["1", "true", "on", "yes"].contains(&v.as_str()) => true,
        Some(v) if ["0", "false", "off", "no"].contains(&v.as_str()) => false,
        _ => default,
    }
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
            gossipsub_queue_len: gossip_queue_len(),
            max_consensus_message_size: 1024 * 1024, // O5: was 256 KB (< EVM worst case — livelock trap)
            max_tx_message_size: 128 * 1024,
            tx_rate_limit_per_peer: 100,
            tx_dedup_window_secs: 60,
            consensus_rate_limit_per_peer: 50,
            ban_list_path: None,
            consensus_direct_fan: consensus_direct_fan(),
            consensus_gossip_mirror: consensus_gossip_mirror(),
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

    /// B3 (send-queue hygiene): the `TORUS_GOSSIP_QUEUE_LEN` parse seam.
    /// Unset/garbage/zero fall back to the crate default (512); `5000`
    /// restores libp2p's shipped default (exact-today rollback).
    #[test]
    fn gossip_queue_len_parse_seam() {
        let default = crate::behaviour::DEFAULT_GOSSIPSUB_QUEUE_LEN;
        assert_eq!(parse_gossip_queue_len(None), default);
        assert_eq!(parse_gossip_queue_len(Some("5000")), 5000); // rollback-to-today
        assert_eq!(parse_gossip_queue_len(Some(" 1024 ")), 1024);
        assert_eq!(parse_gossip_queue_len(Some("0")), default); // 0 would deadlock sends
        assert_eq!(parse_gossip_queue_len(Some("bogus")), default);
        assert_eq!(parse_gossip_queue_len(Some("")), default);
    }

    /// B2 (consensus isolation): the `TORUS_CONSENSUS_DIRECT_FAN` /
    /// `TORUS_CONSENSUS_GOSSIP_MIRROR` boolean parse seam. Unset or
    /// unrecognized falls back to the per-knob default — fan OFF (exact-today
    /// rollback), mirror ON (observers keep their live consensus feed).
    #[test]
    fn consensus_isolation_flag_parse_seam() {
        // direct fan: default false
        assert!(!parse_env_flag(None, false));
        assert!(parse_env_flag(Some("1"), false));
        assert!(parse_env_flag(Some("true"), false));
        assert!(parse_env_flag(Some(" ON "), false));
        assert!(parse_env_flag(Some("yes"), false));
        assert!(!parse_env_flag(Some("0"), false));
        assert!(!parse_env_flag(Some("garbage"), false));
        assert!(!parse_env_flag(Some(""), false));
        // gossip mirror: default true
        assert!(parse_env_flag(None, true));
        assert!(!parse_env_flag(Some("0"), true));
        assert!(!parse_env_flag(Some("false"), true));
        assert!(!parse_env_flag(Some(" off "), true));
        assert!(!parse_env_flag(Some("no"), true));
        assert!(parse_env_flag(Some("1"), true));
        assert!(parse_env_flag(Some("garbage"), true));
    }

    /// B2: `NetworkConfig::default()` = exact-today behavior — direct fan OFF,
    /// gossip mirror ON. Flipping the fan is a per-node opt-in
    /// (`TORUS_CONSENSUS_DIRECT_FAN=1`); unset restores today's gossip-only
    /// broadcast path byte-for-byte.
    #[test]
    fn network_config_default_consensus_isolation_flags() {
        let cfg = NetworkConfig::default();
        assert!(
            !cfg.consensus_direct_fan,
            "direct fan defaults OFF (rollback = today's behavior)"
        );
        assert!(
            cfg.consensus_gossip_mirror,
            "gossip mirror defaults ON (observers stay fed during rollout)"
        );
    }

    #[test]
    fn global_addresses_accepted() {
        for s in [
            "/ip4/95.111.231.121/udp/30333/quic-v1",    // live seed
            "/ip4/84.32.108.220/udp/30333/quic-v1",     // val1
            "/ip4/100.128.0.1/udp/30333/quic-v1",       // just past CGNAT range
            "/ip6/2a01:4f8::1/udp/30333/quic-v1",       // public v6
            "/dns4/seed.example.org/udp/30333/quic-v1", // no IP component
        ] {
            assert!(is_global_addr(&addr(s)), "{s} must be accepted");
        }
    }
}
