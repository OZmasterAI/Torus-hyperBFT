//! Mesh watchdog for the S395 gossipsub subscription wedge.
//!
//! Subscription exchange happens once per connection in libp2p gossipsub. When
//! a validator's subscription message races a dying connection (the in-place
//! fast-restart footgun), the peer stays *connected but unsubscribed* — we
//! gossip nothing to it, no protocol mechanism ever retransmits the
//! subscription, and the chain settles into silent degraded mode (~2.1 blk/s
//! at 1.4 views/block) or wedges. The only recovery that doesn't patch libp2p
//! is forcing a fresh connection, which re-runs the subscription exchange.
//!
//! This module is the pure decision logic, driven from the swarm's mesh tick:
//! a validator peer that stays connected-but-unsubscribed to the consensus
//! topic past [`WATCHDOG_GRACE`] is returned for a forced disconnect. The
//! mesh tick's existing redial then reconnects it within one tick.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use libp2p::PeerId;

/// How long a validator peer may stay connected-but-unsubscribed to the
/// consensus topic before we force a reconnect. Subscription exchange normally
/// completes well under a second after `ConnectionEstablished`; a minute means
/// wedged, not slow. A false positive costs one reconnect (~one mesh tick);
/// a false negative is an indefinite silent degraded mode.
pub const WATCHDOG_GRACE: Duration = Duration::from_secs(60);

/// Tracks, per connected validator peer, since when it has been observed
/// connected but not subscribed to the consensus topic.
#[derive(Default)]
pub struct MeshWatchdog {
    unsubscribed_since: HashMap<PeerId, Instant>,
}

impl MeshWatchdog {
    /// One mesh-tick evaluation.
    ///
    /// `connected` — validator peers currently connected; `subscribed` — the
    /// subset gossipsub reports as subscribed to the consensus topic. Returns
    /// the peers whose grace expired, to be force-disconnected. A returned
    /// peer's timer restarts, so a kick is emitted once per grace period, not
    /// once per tick, even if the disconnect fails or the peer lingers.
    pub fn tick(
        &mut self,
        now: Instant,
        connected: &[PeerId],
        subscribed: &HashSet<PeerId>,
    ) -> Vec<PeerId> {
        self.unsubscribed_since
            .retain(|p, _| connected.contains(p) && !subscribed.contains(p));

        let mut kick = Vec::new();
        for p in connected {
            if subscribed.contains(p) {
                continue;
            }
            let since = *self.unsubscribed_since.entry(*p).or_insert(now);
            if now.duration_since(since) >= WATCHDOG_GRACE {
                kick.push(*p);
                // Restart grace after a kick: disconnect + reconnect +
                // subscription exchange all get a full period before we
                // consider kicking again.
                self.unsubscribed_since.insert(*p, now);
            }
        }
        kick
    }

    /// Number of peers currently tracked as connected-but-unsubscribed.
    pub fn tracked(&self) -> usize {
        self.unsubscribed_since.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers(n: usize) -> Vec<PeerId> {
        (0..n).map(|_| PeerId::random()).collect()
    }

    #[test]
    fn subscribed_peer_never_kicked() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(1);
        let subscribed: HashSet<PeerId> = ps.iter().copied().collect();
        let t0 = Instant::now();
        assert!(wd.tick(t0, &ps, &subscribed).is_empty());
        assert!(wd
            .tick(t0 + WATCHDOG_GRACE * 2, &ps, &subscribed)
            .is_empty());
        assert_eq!(wd.tracked(), 0);
    }

    #[test]
    fn unsubscribed_peer_kicked_only_after_grace() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        assert!(wd.tick(t0, &ps, &none).is_empty(), "tracked, not kicked");
        assert!(
            wd.tick(t0 + WATCHDOG_GRACE - Duration::from_secs(1), &ps, &none)
                .is_empty(),
            "still inside grace"
        );
        assert_eq!(
            wd.tick(t0 + WATCHDOG_GRACE, &ps, &none),
            ps,
            "grace expired -> kick"
        );
    }

    #[test]
    fn kick_resets_timer_no_spam() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &none);
        assert_eq!(wd.tick(t0 + WATCHDOG_GRACE, &ps, &none), ps);
        // Immediately after a kick the peer may still show connected; no
        // re-kick until another full grace elapses.
        assert!(wd
            .tick(t0 + WATCHDOG_GRACE + Duration::from_secs(10), &ps, &none)
            .is_empty());
        assert_eq!(wd.tick(t0 + WATCHDOG_GRACE * 2, &ps, &none), ps);
    }

    #[test]
    fn disconnect_clears_tracking() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(1);
        let none = HashSet::new();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &none);
        assert_eq!(wd.tracked(), 1);
        // Peer drops off the connected list: forgotten...
        assert!(wd.tick(t0 + Duration::from_secs(30), &[], &none).is_empty());
        assert_eq!(wd.tracked(), 0);
        // ...and a reconnect starts a fresh grace period.
        assert!(wd
            .tick(t0 + WATCHDOG_GRACE + Duration::from_secs(5), &ps, &none)
            .is_empty());
    }

    #[test]
    fn resubscribe_clears_tracking() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(1);
        let none = HashSet::new();
        let all: HashSet<PeerId> = ps.iter().copied().collect();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &none);
        assert_eq!(wd.tracked(), 1);
        // Subscription arrives late but within grace: tracking cleared.
        assert!(wd
            .tick(t0 + Duration::from_secs(30), &ps, &all)
            .is_empty());
        assert_eq!(wd.tracked(), 0);
        // Flaps back to unsubscribed: grace restarts from scratch.
        assert!(wd.tick(t0 + WATCHDOG_GRACE, &ps, &none).is_empty());
        assert_eq!(
            wd.tick(t0 + WATCHDOG_GRACE * 2, &ps, &none),
            ps
        );
    }

    #[test]
    fn mixed_peers_only_wedged_kicked() {
        let mut wd = MeshWatchdog::default();
        let ps = peers(3);
        let subscribed: HashSet<PeerId> = [ps[0], ps[1]].into_iter().collect();
        let t0 = Instant::now();
        wd.tick(t0, &ps, &subscribed);
        assert_eq!(
            wd.tick(t0 + WATCHDOG_GRACE, &ps, &subscribed),
            vec![ps[2]]
        );
    }
}
