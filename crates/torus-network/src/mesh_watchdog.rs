//! Mesh watchdog for the gossipsub subscription wedge (S395; tightened s114).
//!
//! libp2p-gossipsub sends our subscriptions to a peer only on that peer's
//! FIRST connection (`on_connection_established` returns early when
//! `other_established > 0`), and never retransmits them. A validator killed
//! and restarted in place is back in 3-4 s, while its peers still hold the
//! dead QUIC connection (idle timeout 10 s): they see the restarted node's
//! new connection as a second one and never send it their subscriptions. The
//! restarted node then sees its peers *connected but unsubscribed*, its
//! consensus publishes fail with `NoPeersSubscribedToTopic`, and with 3
//! validators (quorum = all) the chain halts. The only recovery that doesn't
//! patch libp2p is a fresh connection, which re-runs the subscription
//! exchange. Two triggers force one, both driven from the swarm loop:
//!
//! 1. **Stale-connection kick** (the side that still holds the dead
//!    connection, i.e. the healthy peers): a new connection from a validator
//!    we already hold an OLDER connection to means the peer lost that
//!    connection and redialed — and gossipsub just skipped sending our
//!    subscriptions on the new one. Disconnect the peer entirely and redial
//!    at once: the next connection is a first connection on both sides.
//!    Simultaneous dials (both connections within
//!    [`SIMULTANEOUS_DIAL_WINDOW`]) are ignored — subscriptions went out on
//!    the first of the pair — and kicks are rate-limited per peer
//!    ([`STALE_KICK_MIN_INTERVAL`]).
//! 2. **Unsubscribed watchdog** (backstop, any cause, e.g. the restarted
//!    node itself): a validator peer that stays connected but unsubscribed to
//!    the consensus topic past the grace (default
//!    [`DEFAULT_WATCHDOG_GRACE`], evaluated every [`DEFAULT_WATCHDOG_TICK`])
//!    is force-disconnected and redialed. Repeat kicks of the same peer back
//!    off exponentially (grace, 2x, 4x, ... capped at [`KICK_BACKOFF_MAX`])
//!    until it is seen subscribed.
//!
//! A false kick costs one reconnect; a missed wedge costs a chain halt.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use libp2p::swarm::ConnectionId;
use libp2p::PeerId;

/// Default time a validator peer may stay connected-but-unsubscribed to the
/// consensus topic before a forced reconnect (`TORUS_MESH_WATCHDOG_GRACE_MS`).
/// Subscription exchange completes in well under a second after
/// `ConnectionEstablished`; the swarm loop processes swarm events before
/// timer ticks, so a busy loop delays the check, not the subscription. Was
/// 60 s (S405), which with a 10 s tick made a fast restart a 56-75 s halt.
pub const DEFAULT_WATCHDOG_GRACE: Duration = Duration::from_secs(5);

/// Default watchdog evaluation period (`TORUS_MESH_WATCHDOG_TICK_MS`). Cheap:
/// one pass over the connected validators and gossipsub's peer-topic view.
pub const DEFAULT_WATCHDOG_TICK: Duration = Duration::from_secs(1);

/// Cap on the per-peer exponential backoff between watchdog kicks of a peer
/// that keeps reconnecting unsubscribed.
pub const KICK_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Two connections to one peer established within this window are a
/// simultaneous dial (both sides dialed at once), not a stale connection.
pub const SIMULTANEOUS_DIAL_WINDOW: Duration = Duration::from_secs(2);

/// Minimum spacing between stale-connection kicks of the same peer.
pub const STALE_KICK_MIN_INTERVAL: Duration = Duration::from_secs(30);

/// How long after a kick the watchdog keeps redialing the kicked peer until
/// it is connected again.
pub const REDIAL_WINDOW: Duration = Duration::from_secs(30);

/// Mesh-watchdog state: pure decision logic, no I/O. The swarm loop feeds it
/// connection/subscription events and a periodic [`MeshWatchdog::tick`], and
/// performs the disconnects and dials it returns.
pub struct MeshWatchdog {
    grace: Duration,
    stale_conn_kick: bool,
    /// Connected validator peer -> since when observed unsubscribed.
    unsubscribed_since: HashMap<PeerId, Instant>,
    /// Peer -> (earliest next watchdog kick, consecutive kicks without being
    /// seen subscribed). Survives reconnects; cleared once subscribed.
    backoff: HashMap<PeerId, (Instant, u32)>,
    /// Live connections per peer with their establishment time.
    connections: HashMap<PeerId, Vec<(ConnectionId, Instant)>>,
    last_stale_kick: HashMap<PeerId, Instant>,
    /// Peers we kicked -> kick time; redialed until connected again.
    redial: HashMap<PeerId, Instant>,
    /// Validator peer -> start of its current connect-to-subscribed episode
    /// (spans our own kicks/redials).
    episode: HashMap<PeerId, Instant>,
    /// Completed connect-to-subscribed delays, drained by the swarm loop.
    subscribe_delays: Vec<Duration>,
}

impl Default for MeshWatchdog {
    fn default() -> Self {
        Self::new(DEFAULT_WATCHDOG_GRACE, true)
    }
}

impl MeshWatchdog {
    /// `grace`: connected-but-unsubscribed time before a watchdog kick;
    /// `stale_conn_kick`: enable the stale-connection kick
    /// (`TORUS_MESH_STALE_CONN_KICK`).
    pub fn new(grace: Duration, stale_conn_kick: bool) -> Self {
        Self {
            grace,
            stale_conn_kick,
            unsubscribed_since: HashMap::new(),
            backoff: HashMap::new(),
            connections: HashMap::new(),
            last_stale_kick: HashMap::new(),
            redial: HashMap::new(),
            episode: HashMap::new(),
            subscribe_delays: Vec::new(),
        }
    }

    /// One watchdog evaluation.
    ///
    /// `connected` — validator peers currently connected; `subscribed` — the
    /// peers gossipsub reports as subscribed to the consensus topic. Returns
    /// the peers to force-disconnect (they are queued for redial).
    pub fn tick(
        &mut self,
        now: Instant,
        connected: &[PeerId],
        subscribed: &HashSet<PeerId>,
    ) -> Vec<PeerId> {
        for p in connected.iter().filter(|p| subscribed.contains(*p)) {
            self.backoff.remove(p);
            if let Some(start) = self.episode.remove(p) {
                self.subscribe_delays.push(now.duration_since(start));
            }
        }
        self.unsubscribed_since
            .retain(|p, _| connected.contains(p) && !subscribed.contains(p));
        // Forget strikes after a long quiet period and expired rate limits,
        // so the maps stay bounded by recently-misbehaving peers.
        self.backoff
            .retain(|_, (next, _)| now < *next + KICK_BACKOFF_MAX);
        self.last_stale_kick
            .retain(|_, t| now.duration_since(*t) < STALE_KICK_MIN_INTERVAL);

        let mut kick = Vec::new();
        for p in connected {
            if subscribed.contains(p) {
                continue;
            }
            let since = *self.unsubscribed_since.entry(*p).or_insert(now);
            if now.duration_since(since) < self.grace {
                continue;
            }
            let strikes = match self.backoff.get(p) {
                Some((next, _)) if now < *next => continue,
                Some((_, strikes)) => strikes + 1,
                None => 1,
            };
            let wait = self
                .grace
                .saturating_mul(1u32 << (strikes - 1).min(16))
                .min(KICK_BACKOFF_MAX);
            self.backoff.insert(*p, (now + wait, strikes));
            // Disconnect + reconnect + subscription exchange get a full grace
            // before the peer is judged again.
            self.unsubscribed_since.insert(*p, now);
            self.redial.insert(*p, now);
            kick.push(*p);
        }
        kick
    }

    /// Record a new connection. Returns `true` when it is a validator's
    /// redial over stale connections we still hold (see module docs): the
    /// caller must disconnect the peer (all connections) so the reconnect
    /// re-runs the subscription exchange. `already_subscribed`: gossipsub
    /// already lists the peer on the consensus topic.
    pub fn on_connection_established(
        &mut self,
        now: Instant,
        peer: PeerId,
        conn: ConnectionId,
        is_validator: bool,
        already_subscribed: bool,
    ) -> bool {
        let kick = self.stale_kick_check(now, peer, is_validator);
        self.connections.entry(peer).or_default().push((conn, now));
        if is_validator && !already_subscribed {
            self.episode.entry(peer).or_insert(now);
        }
        kick
    }

    /// A connection from `peer` was denied (our per-peer connection cap is
    /// full). Returns `true` when every connection we hold to that validator
    /// is older than [`SIMULTANEOUS_DIAL_WINDOW`]: a peer that still had them
    /// would not be dialing, so they are dead (e.g. two dead QUIC connections
    /// from a simultaneous dial, a killed node restarted in place) and would
    /// lock the restarted peer out until they idle out. The caller must
    /// disconnect the peer so its next dial is accepted.
    pub fn on_connection_denied(&mut self, now: Instant, peer: PeerId, is_validator: bool) -> bool {
        self.stale_kick_check(now, peer, is_validator)
    }

    /// Shared stale-connection rule (rate-limited per peer): stale kick
    /// enabled, a validator, at least one connection held, and the youngest
    /// held connection older than [`SIMULTANEOUS_DIAL_WINDOW`].
    fn stale_kick_check(&mut self, now: Instant, peer: PeerId, is_validator: bool) -> bool {
        if !(self.stale_conn_kick && is_validator) {
            return false;
        }
        let Some(youngest) = self
            .connections
            .get(&peer)
            .and_then(|c| c.iter().map(|(_, t)| *t).max())
        else {
            return false;
        };
        if now.duration_since(youngest) < SIMULTANEOUS_DIAL_WINDOW {
            return false;
        }
        if self
            .last_stale_kick
            .get(&peer)
            .is_some_and(|t| now.duration_since(*t) < STALE_KICK_MIN_INTERVAL)
        {
            return false;
        }
        self.last_stale_kick.insert(peer, now);
        self.redial.insert(peer, now);
        true
    }

    /// Record a closed connection. Returns `true` when it was the peer's last
    /// connection and we kicked the peer: redial it now.
    pub fn on_connection_closed(&mut self, peer: PeerId, conn: ConnectionId) -> bool {
        let Some(conns) = self.connections.get_mut(&peer) else {
            return false;
        };
        conns.retain(|(c, _)| *c != conn);
        if !conns.is_empty() {
            return false;
        }
        self.connections.remove(&peer);
        if self.redial.contains_key(&peer) {
            true
        } else {
            // A normal disconnect ends the episode unobserved; a kicked
            // peer's episode continues through the redial.
            self.episode.remove(&peer);
            false
        }
    }

    /// gossipsub reported `peer` subscribed to the consensus topic.
    pub fn on_subscribed(&mut self, now: Instant, peer: PeerId) {
        self.backoff.remove(&peer);
        if let Some(start) = self.episode.remove(&peer) {
            self.subscribe_delays.push(now.duration_since(start));
        }
    }

    /// Kicked peers (within [`REDIAL_WINDOW`] of the kick) that are not
    /// connected right now; the caller applies its own per-peer dial backoff.
    /// Entries live for the whole window: a peer is still connected in the
    /// tick that kicks it, and its close must still trigger the redial.
    pub fn redial_due(
        &mut self,
        now: Instant,
        is_connected: impl Fn(&PeerId) -> bool,
    ) -> Vec<PeerId> {
        self.redial
            .retain(|_, t| now.duration_since(*t) < REDIAL_WINDOW);
        self.redial
            .keys()
            .filter(|p| !is_connected(p))
            .copied()
            .collect()
    }

    /// Completed connect-to-subscribed delays since the last call.
    pub fn take_subscribe_delays(&mut self) -> Vec<Duration> {
        std::mem::take(&mut self.subscribe_delays)
    }

    /// Number of peers currently tracked as connected-but-unsubscribed.
    pub fn tracked(&self) -> usize {
        self.unsubscribed_since.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: Duration = DEFAULT_WATCHDOG_GRACE;

    fn peers(n: usize) -> Vec<PeerId> {
        (0..n).map(|_| PeerId::random()).collect()
    }

    fn cid(n: usize) -> ConnectionId {
        ConnectionId::new_unchecked(n)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// s114: the defaults are what make a fast restart recover in seconds.
    #[test]
    fn defaults_are_seconds_not_a_minute() {
        assert_eq!(DEFAULT_WATCHDOG_GRACE, secs(5));
        assert_eq!(DEFAULT_WATCHDOG_TICK, secs(1));
        assert!(DEFAULT_WATCHDOG_GRACE + DEFAULT_WATCHDOG_TICK <= secs(10));
    }

    #[test]
    fn subscribed_peer_never_kicked() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(1);
        let subscribed: HashSet<PeerId> = ps.iter().copied().collect();
        let t0 = Instant::now();
        assert!(wd.tick(t0, &ps, &subscribed).is_empty());
        assert!(wd.tick(t0 + G * 2, &ps, &subscribed).is_empty());
        assert_eq!(wd.tracked(), 0);
    }

    #[test]
    fn unsubscribed_peer_kicked_only_after_grace() {
        let mut wd = MeshWatchdog::new(G, true);
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        assert!(wd.tick(t0, &ps, &none).is_empty(), "tracked, not kicked");
        assert!(
            wd.tick(t0 + G - Duration::from_millis(1), &ps, &none)
                .is_empty(),
            "still inside grace"
        );
        assert_eq!(wd.tick(t0 + G, &ps, &none), ps, "grace expired -> kick");
    }

    /// The grace is configurable (TORUS_MESH_WATCHDOG_GRACE_MS).
    #[test]
    fn grace_is_configurable() {
        let mut wd = MeshWatchdog::new(secs(60), true);
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &none);
        assert!(wd.tick(t0 + secs(59), &ps, &none).is_empty());
        assert_eq!(wd.tick(t0 + secs(60), &ps, &none), ps);
    }

    /// Anti-flap: a peer that stays connected and unsubscribed is kicked at
    /// exponentially growing intervals (G, 2G, 4G, ... capped), not every G.
    #[test]
    fn repeat_kicks_back_off_exponentially_until_cap() {
        let mut wd = MeshWatchdog::new(G, true);
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        let mut kicks = Vec::new();
        // Tick every 100 ms for 10 minutes; the peer never subscribes.
        let mut t = t0;
        while t <= t0 + secs(600) {
            if !wd.tick(t, &ps, &none).is_empty() {
                kicks.push(t.duration_since(t0));
            }
            t += Duration::from_millis(100);
        }
        let gaps: Vec<Duration> = kicks.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(kicks[0], G);
        assert_eq!(&gaps[..4], &[G, G * 2, G * 4, G * 8]);
        assert!(gaps.iter().all(|g| *g <= KICK_BACKOFF_MAX), "{gaps:?}");
        assert_eq!(*gaps.last().unwrap(), KICK_BACKOFF_MAX);
        assert!(
            kicks.len() <= 15,
            "rate-limited: {} kicks in 10 min",
            kicks.len()
        );
    }

    /// Being seen subscribed clears the strikes: a later, unrelated wedge
    /// gets the fast first kick again.
    #[test]
    fn subscription_resets_backoff() {
        let mut wd = MeshWatchdog::new(G, true);
        let ps = peers(1);
        let none = HashSet::new();
        let all: HashSet<PeerId> = ps.iter().copied().collect();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &none);
        assert_eq!(wd.tick(t0 + G, &ps, &none), ps);
        wd.tick(t0 + G + secs(1), &ps, &all);
        let t1 = t0 + secs(100);
        wd.tick(t1, &ps, &none);
        assert_eq!(wd.tick(t1 + G, &ps, &none), ps, "fresh grace, no backoff");
    }

    #[test]
    fn disconnect_clears_tracking() {
        let mut wd = MeshWatchdog::new(G, true);
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &none);
        assert_eq!(wd.tracked(), 1);
        assert!(wd.tick(t0 + secs(3), &[], &none).is_empty());
        assert_eq!(wd.tracked(), 0);
        // A reconnect starts a fresh grace period.
        assert!(wd.tick(t0 + G + secs(1), &ps, &none).is_empty());
    }

    #[test]
    fn mixed_peers_only_wedged_kicked() {
        let mut wd = MeshWatchdog::new(G, true);
        let ps = peers(3);
        let subscribed: HashSet<PeerId> = [ps[0], ps[1]].into_iter().collect();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &subscribed);
        assert_eq!(wd.tick(t0 + G, &ps, &subscribed), vec![ps[2]]);
    }

    /// Anti-flap: a healthy mesh (peers subscribe ~500 ms after connecting,
    /// periodic clean reconnects that race a simultaneous dial) never
    /// produces a kick of either kind over a simulated hour.
    #[test]
    fn healthy_mesh_never_kicks_over_a_long_run() {
        let mut wd = MeshWatchdog::new(G, true);
        let ps = peers(3);
        let t0 = Instant::now();
        let mut next_cid = 0usize;
        let mut new_cid = || {
            next_cid += 1;
            cid(next_cid)
        };
        let mut live: Vec<(PeerId, ConnectionId)> = Vec::new();
        for p in &ps {
            let c = new_cid();
            assert!(!wd.on_connection_established(t0, *p, c, true, false));
            live.push((*p, c));
        }
        let mut subscribed = HashSet::new();
        let mut t = t0;
        let mut step = 0u64;
        while t < t0 + secs(3600) {
            step += 1;
            t += Duration::from_millis(250);
            if step == 2 {
                for p in &ps {
                    wd.on_subscribed(t, *p);
                    subscribed.insert(*p);
                }
            }
            // Every 10 min one peer reconnects cleanly; a simultaneous dial
            // adds a second connection 200 ms after the first, which the
            // remote then closes.
            if step.is_multiple_of(2400) {
                let p = ps[(step / 2400) as usize % 3];
                let (_, c) = live.iter().copied().find(|(q, _)| *q == p).unwrap();
                live.retain(|(q, _)| *q != p);
                assert!(!wd.on_connection_closed(p, c));
                subscribed.remove(&p);
                let c1 = new_cid();
                assert!(!wd.on_connection_established(t, p, c1, true, false));
                let c2 = new_cid();
                assert!(
                    !wd.on_connection_established(
                        t + Duration::from_millis(200),
                        p,
                        c2,
                        true,
                        false
                    ),
                    "simultaneous dial is not a stale connection"
                );
                assert!(!wd.on_connection_closed(p, c2));
                live.push((p, c1));
            }
            if step % 2400 == 2 && step > 2 {
                let p = ps[(step / 2400) as usize % 3];
                wd.on_subscribed(t, p);
                subscribed.insert(p);
            }
            let connected: Vec<PeerId> = live.iter().map(|(p, _)| *p).collect();
            assert!(
                wd.tick(t, &connected, &subscribed).is_empty(),
                "false kick at {:?}",
                t - t0
            );
            assert!(wd.redial_due(t, |_| true).is_empty());
        }
    }

    /// The fast-restart signature on the HEALTHY side: a validator redials
    /// while we still hold its (dead) older connection -> kick + redial.
    #[test]
    fn redial_over_stale_connection_is_kicked_and_redialed() {
        let mut wd = MeshWatchdog::new(G, true);
        let p = PeerId::random();
        let t0 = Instant::now();
        assert!(!wd.on_connection_established(t0, p, cid(1), true, false));
        wd.on_subscribed(t0 + secs(1), p);
        // Minutes later the peer is killed and restarted: a new connection
        // arrives while the dead one is still held.
        let t1 = t0 + secs(300);
        assert!(wd.on_connection_established(t1, p, cid(2), true, true));
        // We disconnect all; only the LAST close triggers the redial.
        assert!(!wd.on_connection_closed(p, cid(1)));
        assert!(wd.on_connection_closed(p, cid(2)));
        assert_eq!(wd.redial_due(t1 + secs(1), |_| false), vec![p]);
        // Reconnected: no longer due.
        assert!(wd.redial_due(t1 + secs(2), |_| true).is_empty());
    }

    /// s114 (measured in the wedge test): the restarted peer's first
    /// incarnation had TWO connections (simultaneous dial), so both dead ones
    /// fill the per-peer cap and every new connection is DENIED, never
    /// established. The denial is the stale signal there.
    #[test]
    fn denied_connection_over_two_stale_connections_is_kicked() {
        let mut wd = MeshWatchdog::new(G, true);
        let p = PeerId::random();
        let t0 = Instant::now();
        wd.on_connection_established(t0, p, cid(1), true, false);
        wd.on_connection_established(t0 + Duration::from_millis(300), p, cid(2), true, false);
        let t1 = t0 + secs(120);
        assert!(wd.on_connection_denied(t1, p, true));
        // Rate-limited like the established path.
        assert!(!wd.on_connection_denied(t1 + secs(1), p, true));
        assert!(!wd.on_connection_closed(p, cid(1)));
        assert!(
            wd.on_connection_closed(p, cid(2)),
            "redial after the last close"
        );
    }

    /// Startup: a third simultaneous dial is denied while the first two are
    /// fresh — not stale, no kick. Nor for non-validators, nor with no
    /// connection held, nor when disabled.
    #[test]
    fn denied_connection_not_kicked_when_fresh_unknown_or_disabled() {
        let t0 = Instant::now();
        let p = PeerId::random();
        let mut wd = MeshWatchdog::new(G, true);
        assert!(!wd.on_connection_denied(t0, p, true), "no connection held");
        wd.on_connection_established(t0, p, cid(1), true, false);
        wd.on_connection_established(t0 + Duration::from_millis(100), p, cid(2), true, false);
        assert!(
            !wd.on_connection_denied(t0 + secs(1), p, true),
            "fresh pair"
        );
        assert!(
            !wd.on_connection_denied(t0 + secs(60), p, false),
            "not a validator"
        );
        let mut off = MeshWatchdog::new(G, false);
        off.on_connection_established(t0, p, cid(1), true, false);
        assert!(
            !off.on_connection_denied(t0 + secs(60), p, true),
            "disabled"
        );
    }

    #[test]
    fn stale_kick_rate_limited_per_peer() {
        let mut wd = MeshWatchdog::new(G, true);
        let p = PeerId::random();
        let t0 = Instant::now();
        wd.on_connection_established(t0, p, cid(1), true, true);
        assert!(wd.on_connection_established(t0 + secs(10), p, cid(2), true, true));
        wd.on_connection_closed(p, cid(2));
        // Another redial over the same stale connection within the interval.
        assert!(!wd.on_connection_established(t0 + secs(20), p, cid(3), true, true));
        wd.on_connection_closed(p, cid(3));
        let later = t0 + secs(10) + STALE_KICK_MIN_INTERVAL;
        assert!(wd.on_connection_established(later, p, cid(4), true, true));
    }

    #[test]
    fn stale_kick_only_for_validators_and_when_enabled() {
        let t0 = Instant::now();
        let p = PeerId::random();
        let mut wd = MeshWatchdog::new(G, true);
        wd.on_connection_established(t0, p, cid(1), false, false);
        assert!(!wd.on_connection_established(t0 + secs(60), p, cid(2), false, false));
        let mut off = MeshWatchdog::new(G, false);
        off.on_connection_established(t0, p, cid(1), true, true);
        assert!(!off.on_connection_established(t0 + secs(60), p, cid(2), true, true));
    }

    /// The connect-to-subscribed episode spans our own kick + redial, so the
    /// metric shows the full wedge, not just the last reconnect.
    #[test]
    fn subscribe_delay_spans_kick_and_redial() {
        let mut wd = MeshWatchdog::new(G, true);
        let p = PeerId::random();
        let none = HashSet::new();
        let t0 = Instant::now();
        wd.on_connection_established(t0, p, cid(1), true, false);
        wd.tick(t0, &[p], &none);
        assert_eq!(wd.tick(t0 + G, &[p], &none), vec![p]);
        // Same tick: the kicked peer is still connected, nothing to dial yet,
        // but its close must still trigger the redial.
        assert!(wd.redial_due(t0 + G, |_| true).is_empty());
        assert!(
            wd.on_connection_closed(p, cid(1)),
            "kicked peer is redialed"
        );
        assert_eq!(wd.redial_due(t0 + G, |_| false), vec![p]);
        assert!(wd.redial_due(t0 + G + REDIAL_WINDOW, |_| false).is_empty());
        wd.on_connection_established(t0 + G + secs(1), p, cid(2), true, false);
        wd.on_subscribed(t0 + G + secs(2), p);
        assert_eq!(wd.take_subscribe_delays(), vec![G + secs(2)]);
        assert!(wd.take_subscribe_delays().is_empty());
    }

    /// An ordinary disconnect ends the episode without an observation, and
    /// an already-subscribed peer (extra connection) starts none.
    #[test]
    fn subscribe_delay_not_recorded_for_plain_disconnect_or_known_peer() {
        let mut wd = MeshWatchdog::new(G, true);
        let p = PeerId::random();
        let t0 = Instant::now();
        wd.on_connection_established(t0, p, cid(1), true, false);
        assert!(!wd.on_connection_closed(p, cid(1)));
        wd.on_subscribed(t0 + secs(1), p);
        wd.on_connection_established(t0 + secs(2), p, cid(2), true, true);
        wd.tick(t0 + secs(3), &[p], &[p].into_iter().collect());
        assert!(wd.take_subscribe_delays().is_empty());
    }
}
