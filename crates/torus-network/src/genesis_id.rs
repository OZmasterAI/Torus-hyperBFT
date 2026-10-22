//! The genesis digest in the p2p handshake (owner s138, K question option B).
//!
//! Nodes started from different genesis files (stakes, markets, activations,
//! a consensus parameter such as the leader-miss threshold K) execute
//! different state from height 1 and halt at the first point they disagree.
//! Each node binds its data dir to its genesis file's digest (P0.0 phase 4,
//! `torus_genesis::GenesisDigest`: keccak256 of the canonical JSON, so
//! whitespace and key order do not change it), but nothing compared genesis
//! ACROSS nodes. Two layers do now, both keyed by that same digest:
//!
//! 1. **Namespacing.** Every app protocol (`/torus/direct`, `/torus/sync`,
//!    `/torus/block-data`, `/torus/native-da`, `/torus/native-da-shards`,
//!    kademlia) and every gossip topic carries a tag of the digest
//!    (`/torus/g<16 hex>/direct/2.0`). A peer with another genesis negotiates
//!    none of them and shares no topic, so no consensus message, transaction,
//!    block, sync or DHT record crosses, even before layer 2 runs. Gossipsub's
//!    own protocol id stays the libp2p default (a custom id would drop
//!    gossipsub 1.2); its topics carry the tag instead.
//! 2. **Explicit check.** Identify, the one protocol both sides still speak,
//!    advertises the full digest in its protocol version
//!    (`/torus/1.0/genesis/0x<64 hex>`). A peer whose digest differs (or that
//!    advertises none) is logged (`peer <id> genesis digest <x> != ours <y>:
//!    refusing`), counted (`torus_p2p_genesis_mismatch_refusals`) and
//!    disconnected.
//!
//! A node without `--genesis` (default chain config) has no digest: it keeps
//! the legacy names (`/torus/direct/2.0`, `/torus/1.0`) and refuses peers
//! that advertise a digest, and they refuse it.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use libp2p::core::transport::PortUse;
use libp2p::core::Endpoint;
use libp2p::swarm::{
    dummy, CloseConnection, ConnectionDenied, ConnectionId, FromSwarm, NetworkBehaviour, THandler,
    THandlerInEvent, THandlerOutEvent, ToSwarm,
};
use libp2p::{Multiaddr, PeerId, StreamProtocol};

/// Identify protocol version of a node without a genesis digest, and the
/// prefix of one with a digest.
pub const IDENTIFY_PROTOCOL_BASE: &str = "/torus/1.0";

/// Between [`IDENTIFY_PROTOCOL_BASE`] and the digest's 64 hex digits.
const IDENTIFY_GENESIS_INFIX: &str = "/genesis/0x";

/// Prefix every namespaced protocol and topic name starts with.
const TORUS_PREFIX: &str = "/torus/";

/// Bytes of the digest in the protocol / topic tag (16 hex digits). The tag
/// only keeps mismatched peers from negotiating; the identify check compares
/// the full digest.
const TAG_BYTES: usize = 8;

/// The genesis this node's network layer is keyed by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkGenesis {
    digest: Option<[u8; 32]>,
}

/// A peer refused for its genesis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenesisMismatch {
    /// The peer's digest (`0x..`), `none`, or its raw identify protocol
    /// version when that is not a torus one.
    pub theirs: String,
    /// Ours, same form.
    pub ours: String,
}

impl fmt::Display for GenesisMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "genesis digest {} != ours {}", self.theirs, self.ours)
    }
}

/// A peer's identify protocol version, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PeerGenesis {
    /// A torus node with this digest, or (`None`) a torus node without one.
    Torus(Option<[u8; 32]>),
    /// Not a torus identify version (raw text).
    Foreign(String),
}

fn hex32(d: &[u8; 32]) -> String {
    let mut s = String::with_capacity(66);
    s.push_str("0x");
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn parse_peer(protocol_version: &str) -> PeerGenesis {
    if protocol_version == IDENTIFY_PROTOCOL_BASE {
        return PeerGenesis::Torus(None);
    }
    protocol_version
        .strip_prefix(IDENTIFY_PROTOCOL_BASE)
        .and_then(|rest| rest.strip_prefix(IDENTIFY_GENESIS_INFIX))
        .and_then(|hex| parse_hex32(&hex.to_ascii_lowercase()))
        .map(|d| PeerGenesis::Torus(Some(d)))
        .unwrap_or_else(|| PeerGenesis::Foreign(protocol_version.to_string()))
}

impl NetworkGenesis {
    /// Keyed by `digest` (`torus_genesis::GenesisDigest::digest`), or legacy
    /// names without one.
    pub fn new(digest: Option<[u8; 32]>) -> Self {
        Self { digest }
    }

    pub fn digest(&self) -> Option<[u8; 32]> {
        self.digest
    }

    /// `0x<64 hex>`, or `none`.
    pub fn display_digest(&self) -> String {
        self.digest
            .as_ref()
            .map_or_else(|| "none".to_string(), hex32)
    }

    /// The protocol / topic tag (`g` + 16 hex digits), `None` without a digest.
    fn tag(&self) -> Option<String> {
        self.digest.map(|d| {
            let mut s = String::from("g");
            for b in &d[..TAG_BYTES] {
                s.push_str(&format!("{b:02x}"));
            }
            s
        })
    }

    /// `base` (`/torus/<name>/<version>`) with the tag after `/torus/`;
    /// `base` unchanged without a digest.
    pub fn name(&self, base: &str) -> String {
        match (self.tag(), base.strip_prefix(TORUS_PREFIX)) {
            (Some(tag), Some(rest)) => format!("{TORUS_PREFIX}{tag}/{rest}"),
            (Some(tag), None) => format!("{TORUS_PREFIX}{tag}{base}"),
            (None, _) => base.to_string(),
        }
    }

    /// A request-response protocol: [`Self::name`] of `base`.
    pub fn protocol(&self, base: &str) -> StreamProtocol {
        StreamProtocol::try_from_owned(self.name(base))
            .expect("a /torus/ protocol name starts with '/'")
    }

    /// A gossip topic name: [`Self::name`] of `base`.
    pub fn topic(&self, base: &str) -> String {
        self.name(base)
    }

    /// The kademlia protocol: `/torus/g<tag>/kad/1.0.0`; `None` (libp2p's
    /// default `/ipfs/kad/1.0.0`) without a digest.
    pub fn kad_protocol(&self) -> Option<StreamProtocol> {
        self.digest.map(|_| self.protocol("/torus/kad/1.0.0"))
    }

    /// The identify protocol version this node advertises.
    pub fn identify_protocol_version(&self) -> String {
        match &self.digest {
            Some(d) => format!(
                "{IDENTIFY_PROTOCOL_BASE}{IDENTIFY_GENESIS_INFIX}{}",
                &hex32(d)[2..]
            ),
            None => IDENTIFY_PROTOCOL_BASE.to_string(),
        }
    }

    /// Check a peer's identify protocol version. With a digest, anything but
    /// the same digest is refused (another digest, none, a non-torus
    /// version). Without one, a peer advertising a digest is refused; a
    /// non-torus version is let through (legacy behaviour).
    pub fn check_peer(&self, peer_protocol_version: &str) -> Result<(), GenesisMismatch> {
        let peer = parse_peer(peer_protocol_version);
        let ok = match (&self.digest, &peer) {
            (Some(ours), PeerGenesis::Torus(Some(theirs))) => ours == theirs,
            (Some(_), _) => false,
            (None, PeerGenesis::Torus(Some(_))) => false,
            (None, _) => true,
        };
        if ok {
            return Ok(());
        }
        let theirs = match peer {
            PeerGenesis::Torus(Some(d)) => hex32(&d),
            PeerGenesis::Torus(None) => "none".to_string(),
            PeerGenesis::Foreign(raw) => format!("{raw:?} (not a torus identify version)"),
        };
        Err(GenesisMismatch {
            theirs,
            ours: self.display_digest(),
        })
    }
}

impl fmt::Display for NetworkGenesis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_digest())
    }
}

/// First cooldown after a peer is refused for its genesis; it doubles on each
/// further refusal of the same peer up to [`MISMATCH_COOLDOWN_MAX`].
pub const MISMATCH_COOLDOWN_MIN: Duration = Duration::from_secs(5);
/// Cap of the per-peer mismatch cooldown.
pub const MISMATCH_COOLDOWN_MAX: Duration = Duration::from_secs(60);
/// Entries kept before stale ones (cooldown long over) are pruned.
const COOLDOWN_PRUNE_AT: usize = 1024;

/// A refused connection is closed this long after our own identify info has
/// gone out on it (see [`GenesisGate::refuse`]), so the peer reads it and
/// refuses us too instead of seeing only a closed connection.
pub const REFUSAL_CLOSE_LINGER: Duration = Duration::from_millis(250);
/// A refused connection is closed at the latest this long after the refusal,
/// whether or not the peer ever asked for our identify info.
pub const REFUSAL_CLOSE_MAX_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
struct Cooldown {
    until: Instant,
    interval: Duration,
    strikes: u32,
}

/// Per-peer cooldown after a genesis refusal (Codex P2 on the handshake
/// branch). A refused validator stays in the peer map, so without it every
/// consensus send, the mesh tick, kademlia and gossipsub's explicit-peer tick
/// redialed it: each new QUIC connection ran identify, was refused, closed,
/// and the next send dialed again (~20 connections/s per peer). While a peer
/// cools down, [`GenesisGate`] denies every connection to or from it, so no
/// dial path reaches the wire.
#[derive(Debug, Default)]
pub struct MismatchCooldown {
    peers: HashMap<PeerId, Cooldown>,
}

impl MismatchCooldown {
    /// Record a refusal of `peer` at `now`; returns the cooldown applied and
    /// how many refusals in a row this peer has had.
    pub fn record(&mut self, peer: PeerId, now: Instant) -> (Duration, u32) {
        if self.peers.len() >= COOLDOWN_PRUNE_AT {
            self.peers
                .retain(|_, c| c.until + MISMATCH_COOLDOWN_MAX > now);
        }
        let c = self.peers.entry(peer).or_insert(Cooldown {
            until: now,
            interval: Duration::ZERO,
            strikes: 0,
        });
        c.interval = if c.strikes == 0 {
            MISMATCH_COOLDOWN_MIN
        } else {
            (c.interval * 2).min(MISMATCH_COOLDOWN_MAX)
        };
        c.strikes = c.strikes.saturating_add(1);
        c.until = now + c.interval;
        (c.interval, c.strikes)
    }

    /// Whether `peer` is still cooling down at `now`.
    pub fn is_cooling(&self, peer: &PeerId, now: Instant) -> bool {
        self.peers.get(peer).is_some_and(|c| now < c.until)
    }

    /// Forget `peer` (it was accepted).
    pub fn clear(&mut self, peer: &PeerId) {
        self.peers.remove(peer);
    }
}

/// Whether the `strikes`-th refusal in a row of one peer is logged at WARN
/// (the first two, then powers of two); the rest go to DEBUG. With the
/// cooldown a refusal happens at most every 5..60 s per peer already.
pub fn warn_on_strike(strikes: u32) -> bool {
    strikes <= 2 || strikes.is_power_of_two()
}

/// A connection denied because its peer is cooling down after a genesis
/// refusal.
#[derive(Debug)]
pub struct GenesisCooldown(pub PeerId);

impl fmt::Display for GenesisCooldown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "peer {} cooling down after a genesis digest refusal",
            self.0
        )
    }
}

impl std::error::Error for GenesisCooldown {}

/// Connection gate keyed by the genesis handshake. Must be the FIRST field of
/// `TorusBehaviour` (with the other deniers): it denies, it never registers
/// connection state.
///
/// - **Cooldown**: every connection to or from a peer in its
///   [`MismatchCooldown`] is denied, before the dial reaches the wire for a
///   dial by peer id.
/// - **Acceptance**: a peer is `accepted` once identify on one of its
///   connections has passed the genesis check, until its last connection
///   closes. Buffered sends are flushed to a peer only once it is accepted,
///   and direct sends to a connected but not yet accepted peer are buffered.
/// - **Refusal close**: a refused connection is closed once our identify info
///   has been sent on it (plus [`REFUSAL_CLOSE_LINGER`]), or after
///   [`REFUSAL_CLOSE_MAX_WAIT`]. Identify runs both ways at once; closing at
///   the refusal itself let the side that read the other's info first cut the
///   connection before answering the other's request, so only one side ever
///   saw the mismatch (under load, every time). Nothing crosses meanwhile:
///   the app protocols and topics carry the genesis tag.
#[derive(Debug, Default)]
pub struct GenesisGate {
    cooldown: MismatchCooldown,
    accepted: HashSet<PeerId>,
    /// Open connections on which identify sent our info, and when.
    identify_sent: HashMap<ConnectionId, Instant>,
    /// Refused connections still open: peer, close-at.
    closing: HashMap<ConnectionId, (PeerId, Instant)>,
    /// Wakes `poll` at the earliest close-at.
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl GenesisGate {
    /// Refuse `peer` on `connection`: start or extend its cooldown, drop its
    /// acceptance and schedule the close of its connections (see the type
    /// docs: not before our identify info went out on `connection`).
    pub fn refuse(&mut self, peer: PeerId, connection: ConnectionId) -> (Duration, u32) {
        self.refuse_at(peer, connection, Instant::now())
    }

    fn refuse_at(
        &mut self,
        peer: PeerId,
        connection: ConnectionId,
        now: Instant,
    ) -> (Duration, u32) {
        self.accepted.remove(&peer);
        let close_at = match self.identify_sent.get(&connection) {
            Some(sent) => (*sent + REFUSAL_CLOSE_LINGER).max(now),
            None => now + REFUSAL_CLOSE_MAX_WAIT,
        };
        self.closing.insert(connection, (peer, close_at));
        self.timer = None;
        self.cooldown.record(peer, now)
    }

    /// Identify sent our info to the peer on `connection`.
    pub fn identify_sent(&mut self, connection: ConnectionId) {
        self.identify_sent_at(connection, Instant::now())
    }

    fn identify_sent_at(&mut self, connection: ConnectionId, now: Instant) {
        self.identify_sent.entry(connection).or_insert(now);
        if let Some((_, close_at)) = self.closing.get_mut(&connection) {
            *close_at = (*close_at).min(now + REFUSAL_CLOSE_LINGER);
            self.timer = None;
        }
    }

    /// Refused connections due to close at `now` (removed from the schedule).
    fn take_due(&mut self, now: Instant) -> Vec<(PeerId, ConnectionId)> {
        let due: Vec<(PeerId, ConnectionId)> = self
            .closing
            .iter()
            .filter(|(_, (_, at))| *at <= now)
            .map(|(c, (p, _))| (*p, *c))
            .collect();
        for (_, c) in &due {
            self.closing.remove(c);
        }
        due
    }

    fn next_close_at(&self) -> Option<Instant> {
        self.closing.values().map(|(_, at)| *at).min()
    }

    /// Identify accepted `peer`; true when it was not accepted before (its
    /// buffered sends are due now).
    pub fn accept(&mut self, peer: PeerId) -> bool {
        self.cooldown.clear(&peer);
        self.accepted.insert(peer)
    }

    pub fn is_accepted(&self, peer: &PeerId) -> bool {
        self.accepted.contains(peer)
    }

    pub fn is_cooling(&self, peer: &PeerId) -> bool {
        self.cooldown.is_cooling(peer, Instant::now())
    }

    fn enforce(&self, peer: &PeerId) -> Result<(), ConnectionDenied> {
        if self.is_cooling(peer) {
            return Err(ConnectionDenied::new(GenesisCooldown(*peer)));
        }
        Ok(())
    }
}

impl NetworkBehaviour for GenesisGate {
    type ConnectionHandler = dummy::ConnectionHandler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.enforce(&peer)?;
        Ok(dummy::ConnectionHandler)
    }

    fn handle_pending_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: Option<PeerId>,
        _: &[Multiaddr],
        _: Endpoint,
    ) -> Result<Vec<Multiaddr>, ConnectionDenied> {
        if let Some(peer) = peer {
            self.enforce(&peer)?;
        }
        Ok(vec![])
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        peer: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<THandler<Self>, ConnectionDenied> {
        self.enforce(&peer)?;
        Ok(dummy::ConnectionHandler)
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionClosed(c) = event {
            self.identify_sent.remove(&c.connection_id);
            self.closing.remove(&c.connection_id);
            if c.remaining_established == 0 {
                self.accepted.remove(&c.peer_id);
            }
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        event: THandlerOutEvent<Self>,
    ) {
        match event {}
    }

    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<ToSwarm<Infallible, THandlerInEvent<Self>>> {
        loop {
            if let Some((peer_id, connection)) = self.take_due(Instant::now()).into_iter().next() {
                // Any other due entry is picked up by the next poll (the
                // swarm polls a behaviour again after it returns Ready).
                self.timer = None;
                return Poll::Ready(ToSwarm::CloseConnection {
                    peer_id,
                    connection: CloseConnection::One(connection),
                });
            }
            let Some(at) = self.next_close_at() else {
                self.timer = None;
                return Poll::Pending;
            };
            let timer = self
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(at.into())));
            if timer.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.timer = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(byte: u8) -> [u8; 32] {
        let mut d = [byte; 32];
        d[31] = 0x5a;
        d
    }

    #[test]
    fn identify_version_round_trips_and_same_digest_is_accepted() {
        let g = NetworkGenesis::new(Some(d(0xab)));
        let v = g.identify_protocol_version();
        assert_eq!(
            v,
            format!("/torus/1.0/genesis/0x{}5a", "ab".repeat(31)),
            "the full digest is advertised"
        );
        assert_eq!(g.check_peer(&v), Ok(()));
        // Upper-case hex from a peer is the same digest.
        let upper = format!("/torus/1.0/genesis/0x{}5A", "AB".repeat(31));
        assert_eq!(g.check_peer(&upper), Ok(()));
    }

    #[test]
    fn a_different_digest_is_refused_naming_both() {
        let ours = NetworkGenesis::new(Some(d(1)));
        let theirs = NetworkGenesis::new(Some(d(2)));
        let err = ours
            .check_peer(&theirs.identify_protocol_version())
            .expect_err("refused");
        assert_eq!(err.theirs, theirs.display_digest());
        assert_eq!(err.ours, ours.display_digest());
        assert_eq!(
            err.to_string(),
            format!(
                "genesis digest {} != ours {}",
                theirs.display_digest(),
                ours.display_digest()
            )
        );
    }

    #[test]
    fn a_peer_without_a_digest_and_a_foreign_peer_are_refused() {
        let ours = NetworkGenesis::new(Some(d(1)));
        let err = ours
            .check_peer("/torus/1.0")
            .expect_err("no digest refused");
        assert_eq!(err.theirs, "none");
        assert!(ours.check_peer("/ipfs/0.1.0").is_err());
        assert!(
            ours.check_peer("/torus/1.0/genesis/0x1234").is_err(),
            "short digest"
        );
        assert!(ours.check_peer("/torus/1.0/genesis/").is_err());
    }

    #[test]
    fn a_node_without_a_digest_refuses_one_with_a_digest() {
        let none = NetworkGenesis::new(None);
        assert_eq!(none.identify_protocol_version(), "/torus/1.0");
        assert_eq!(none.check_peer("/torus/1.0"), Ok(()));
        let err = none
            .check_peer(&NetworkGenesis::new(Some(d(1))).identify_protocol_version())
            .expect_err("refused");
        assert_eq!(err.ours, "none");
        // Legacy: a non-torus identify version is not judged.
        assert_eq!(none.check_peer("/ipfs/0.1.0"), Ok(()));
    }

    #[test]
    fn protocol_and_topic_names_carry_the_tag() {
        let g = NetworkGenesis::new(Some(d(0xab)));
        let tag = format!("g{}", "ab".repeat(8));
        assert_eq!(
            g.protocol("/torus/direct/2.0").as_ref(),
            format!("/torus/{tag}/direct/2.0")
        );
        assert_eq!(
            g.topic("/torus/consensus/1.0"),
            format!("/torus/{tag}/consensus/1.0")
        );
        assert_eq!(
            g.kad_protocol().unwrap().as_ref(),
            format!("/torus/{tag}/kad/1.0.0")
        );
        // The zstd dispatch keys on the "/2.0" suffix: it survives.
        assert!(g
            .protocol("/torus/native-da/2.0")
            .as_ref()
            .ends_with("/2.0"));
        // Another digest, other names.
        let other = NetworkGenesis::new(Some(d(0xac)));
        assert_ne!(
            g.protocol("/torus/sync/1.0"),
            other.protocol("/torus/sync/1.0")
        );
    }

    #[test]
    fn without_a_digest_names_are_the_legacy_ones() {
        let g = NetworkGenesis::new(None);
        assert_eq!(
            g.protocol("/torus/direct/2.0").as_ref(),
            "/torus/direct/2.0"
        );
        assert_eq!(g.topic("/torus/consensus/1.0"), "/torus/consensus/1.0");
        assert!(g.kad_protocol().is_none());
    }

    #[test]
    fn mismatch_cooldown_backs_off_caps_and_expires() {
        let peer = PeerId::random();
        let mut c = MismatchCooldown::default();
        let t0 = Instant::now();
        assert!(!c.is_cooling(&peer, t0));
        assert_eq!(c.record(peer, t0), (MISMATCH_COOLDOWN_MIN, 1));
        assert!(c.is_cooling(&peer, t0 + MISMATCH_COOLDOWN_MIN / 2));
        assert!(!c.is_cooling(&peer, t0 + MISMATCH_COOLDOWN_MIN));
        assert_eq!(c.record(peer, t0).0, MISMATCH_COOLDOWN_MIN * 2);
        let mut last = Duration::ZERO;
        for _ in 0..20 {
            last = c.record(peer, t0).0;
        }
        assert_eq!(last, MISMATCH_COOLDOWN_MAX, "capped");
        assert!(!c.is_cooling(&PeerId::random(), t0), "per peer");
        c.clear(&peer);
        assert!(!c.is_cooling(&peer, t0));
        assert_eq!(
            c.record(peer, t0),
            (MISMATCH_COOLDOWN_MIN, 1),
            "fresh after clear"
        );
    }

    #[test]
    fn gate_denies_a_cooling_peer_and_accept_lifts_it() {
        let peer = PeerId::random();
        let mut g = GenesisGate::default();
        assert!(g.enforce(&peer).is_ok());
        assert!(g.accept(peer), "first acceptance is new");
        assert!(!g.accept(peer), "second is not");
        g.refuse(peer, ConnectionId::new_unchecked(1));
        assert!(!g.is_accepted(&peer), "refusal drops acceptance");
        assert!(g.enforce(&peer).is_err(), "cooling peer is denied");
        assert!(g.accept(peer));
        assert!(g.enforce(&peer).is_ok(), "accepted peer is let through");
    }

    /// Batch A 18c gate (load ~175): a node with a digest never counted the
    /// refusal of a peer without one (a=0 b=3). Both sides identify at once;
    /// the one that read the other's info first closed the connection before
    /// answering the other's request. A refused connection now stays open
    /// until our info went out on it (plus a linger), capped.
    #[test]
    fn refused_connection_closes_only_after_our_identify_went_out() {
        let peer = PeerId::random();
        let conn = ConnectionId::new_unchecked(7);
        let mut g = GenesisGate::default();
        let t0 = Instant::now();
        g.refuse_at(peer, conn, t0);
        assert!(
            g.take_due(t0).is_empty(),
            "not closed before our info went out"
        );
        assert!(g.take_due(t0 + REFUSAL_CLOSE_LINGER * 2).is_empty());
        let t1 = t0 + Duration::from_millis(400);
        g.identify_sent_at(conn, t1);
        assert!(g.take_due(t1).is_empty(), "lingers after sending");
        assert_eq!(g.take_due(t1 + REFUSAL_CLOSE_LINGER), vec![(peer, conn)]);
        assert!(
            g.take_due(t1 + REFUSAL_CLOSE_MAX_WAIT).is_empty(),
            "closed once"
        );
    }

    #[test]
    fn refused_connection_whose_info_already_went_out_closes_after_the_linger() {
        let peer = PeerId::random();
        let conn = ConnectionId::new_unchecked(8);
        let mut g = GenesisGate::default();
        let t0 = Instant::now();
        g.identify_sent_at(conn, t0);
        let t1 = t0 + REFUSAL_CLOSE_LINGER * 4;
        g.refuse_at(peer, conn, t1);
        assert_eq!(g.take_due(t1), vec![(peer, conn)], "linger already over");
    }

    #[test]
    fn refused_connection_is_closed_after_the_max_wait_without_identify() {
        let peer = PeerId::random();
        let conn = ConnectionId::new_unchecked(9);
        let mut g = GenesisGate::default();
        let t0 = Instant::now();
        g.refuse_at(peer, conn, t0);
        assert!(g.take_due(t0 + REFUSAL_CLOSE_MAX_WAIT / 2).is_empty());
        assert_eq!(g.take_due(t0 + REFUSAL_CLOSE_MAX_WAIT), vec![(peer, conn)]);
    }

    #[test]
    fn refusal_warn_is_rate_limited() {
        let warned: Vec<u32> = (1..=40).filter(|s| warn_on_strike(*s)).collect();
        assert_eq!(warned, vec![1, 2, 4, 8, 16, 32]);
    }
}
