//! Off-loop native-action intake (Phase-3 Round 2, scope 1 + swarm fold-in #1).
//!
//! The swarm event loop must stay a pure ROUTER. Decoding a gossiped
//! `PlaceOrderBatch` body (up to the per-batch order cap) on the loop is
//! head-of-line blocking that starved consensus routing under b400 load — the
//! availability-starvation stall confirmed in the Round-1 proof (blocks froze
//! ~76s while 17,205 actions backed up in the single on-loop ingest path).
//!
//! So each inbound native body is forwarded as RAW bytes (tagged by wire kind)
//! into a bounded channel; a dedicated OFF-LOOP mirror worker decodes it ONCE,
//! mirrors the body to the durable DA store AT RECEIPT (before the verify FIFO),
//! then hands the decoded action to the verify queue. Decode-off-loop
//! (integration finding #1) and mirror-at-receipt (R2.1) are the SAME worker, so
//! a body is decoded exactly once (co-design).
//!
//! The channel is bounded (memory + DoS bound) and carries a per-peer in-flight
//! BYTE budget so a single flooding peer cannot monopolise the intake. Devnet
//! peers are validator-set-only, so the per-peer budget is defence-in-depth
//! flagged for WAN review; a raw-intake drop is recoverable via gossip
//! redundancy or the DA pull-fallback (the proposer always mirrors the bodies it
//! includes at produce_block).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use libp2p::PeerId;

/// Capacity (in items) of the bounded raw-intake channel. Each item is at most
/// one network message (already capped by `max_tx_message_size`), so the global
/// in-flight bound is `cap × max_tx_message_size`. Well above the worst backlog
/// observed in the Round-1 stall (17,205) so legitimate bodies are not shed; the
/// fast (no-crypto) mirror worker keeps it shallow in steady state.
/// `TORUS_NATIVE_RAW_INBOUND_CAP` overrides per node.
pub fn raw_inbound_channel_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TORUS_NATIVE_RAW_INBOUND_CAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(65_536)
    })
}

/// Capacity of the POST-mirror verify queue (mirror worker → verify worker).
/// Drops here are SAFE by construction: the body is already DA-resident, so a
/// drop only forfeits pool candidacy (the R2.3 drop-safety property). Tunable
/// via `TORUS_NATIVE_VERIFY_QUEUE_CAP` (invariant 5: batch/queue sizes tunable).
pub fn verify_queue_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TORUS_NATIVE_VERIFY_QUEUE_CAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(8_192)
    })
}

/// Max raw items the mirror worker drains + mirrors in one `put_batch` before
/// handing to verify. Bounds the mirror write-stream burst so it cannot steal
/// from consensus (invariant 5). Tunable via `TORUS_MIRROR_BATCH_MAX`.
pub fn mirror_batch_max() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("TORUS_MIRROR_BATCH_MAX")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|&v| v > 0)
            .unwrap_or(256)
    })
}

/// Max in-flight (queued-but-not-yet-mirrored) bytes a single peer may hold in
/// the raw-intake channel. Defence-in-depth against one flooding peer; devnet
/// peers are validator-set-only (WAN review). `0` disables the per-peer cap.
/// `TORUS_NATIVE_PEER_INBOUND_BUDGET_BYTES` overrides per node.
pub fn per_peer_inbound_budget_bytes() -> usize {
    static B: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *B.get_or_init(|| {
        std::env::var("TORUS_NATIVE_PEER_INBOUND_BUDGET_BYTES")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(32 * 1024 * 1024)
    })
}

/// Wire encoding of a raw inbound native body, so the off-loop worker knows how
/// to decode it without the swarm loop paying the decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeInboundKind {
    /// bincode `(Address, SignedNativeAction)` — gossip single / batch element.
    Pair,
    /// bincode `Vec<(Address, SignedNativeAction)>` — pre-proposal full-body push.
    PreProposalBatch,
    /// 20-byte sender prefix + serde_json `SignedNativeAction` — leader forward.
    ForwardedJson,
}

/// One raw inbound native body en route to the off-loop mirror/verify worker.
/// Carries a per-peer byte-budget reservation that is released when the worker
/// drops the item (via [`PeerBudgetGuard`]'s `Drop`).
pub struct RawNativeInbound {
    pub kind: NativeInboundKind,
    pub bytes: Vec<u8>,
    // Released on Drop after the worker finishes decoding/mirroring this item.
    _budget: Option<PeerBudgetGuard>,
}

impl RawNativeInbound {
    pub fn new(kind: NativeInboundKind, bytes: Vec<u8>, budget: Option<PeerBudgetGuard>) -> Self {
        Self {
            kind,
            bytes,
            _budget: budget,
        }
    }
}

/// Per-peer in-flight-bytes budget for the raw native intake channel. Bounds the
/// bytes any one peer may have queued-but-not-yet-mirrored, so the bounded
/// channel's global cap cannot be monopolised by one flooding peer.
pub struct PerPeerBudget {
    inflight: Mutex<HashMap<PeerId, usize>>,
    per_peer_max: usize,
}

impl PerPeerBudget {
    /// Build a budget allowing at most `per_peer_max` in-flight bytes per peer.
    /// `0` disables the per-peer cap (still globally bounded by the channel).
    pub fn new(per_peer_max: usize) -> Arc<Self> {
        Arc::new(Self {
            inflight: Mutex::new(HashMap::new()),
            per_peer_max,
        })
    }

    /// Reserve `len` bytes for `peer`. Returns a guard (released on Drop) on
    /// success, or `None` when the peer is already at its budget — the caller
    /// then drops the body and counts it (recoverable via gossip / DA pull).
    pub fn try_reserve(self: &Arc<Self>, peer: PeerId, len: usize) -> Option<PeerBudgetGuard> {
        if self.per_peer_max == 0 {
            // Cap disabled: hand back a guard that tracks nothing.
            return Some(PeerBudgetGuard {
                budget: Arc::clone(self),
                peer,
                len: 0,
            });
        }
        let mut map = self.inflight.lock().unwrap();
        let cur = map.entry(peer).or_insert(0);
        if cur.saturating_add(len) > self.per_peer_max {
            return None;
        }
        *cur += len;
        Some(PeerBudgetGuard {
            budget: Arc::clone(self),
            peer,
            len,
        })
    }

    #[cfg(test)]
    fn inflight_for(&self, peer: &PeerId) -> usize {
        *self.inflight.lock().unwrap().get(peer).unwrap_or(&0)
    }
}

/// RAII release of a per-peer byte reservation. Decrements the peer's in-flight
/// byte count when the worker is done with the [`RawNativeInbound`] item.
pub struct PeerBudgetGuard {
    budget: Arc<PerPeerBudget>,
    peer: PeerId,
    len: usize,
}

impl Drop for PeerBudgetGuard {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        let mut map = self.budget.inflight.lock().unwrap();
        if let Some(cur) = map.get_mut(&self.peer) {
            *cur = cur.saturating_sub(self.len);
            if *cur == 0 {
                map.remove(&self.peer);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_peer_budget_rejects_over_cap_and_releases_on_drop() {
        let budget = PerPeerBudget::new(100);
        let peer = PeerId::random();

        let g1 = budget.try_reserve(peer, 60).expect("first fits");
        assert_eq!(budget.inflight_for(&peer), 60);
        // 60 + 50 > 100 -> rejected, in-flight unchanged.
        assert!(budget.try_reserve(peer, 50).is_none());
        assert_eq!(budget.inflight_for(&peer), 60);
        // 60 + 40 == 100 -> fits.
        let g2 = budget.try_reserve(peer, 40).expect("second fits at exact cap");
        assert_eq!(budget.inflight_for(&peer), 100);

        drop(g1);
        assert_eq!(budget.inflight_for(&peer), 40);
        drop(g2);
        assert_eq!(budget.inflight_for(&peer), 0);
    }

    #[test]
    fn per_peer_budget_is_isolated_across_peers() {
        let budget = PerPeerBudget::new(100);
        let a = PeerId::random();
        let b = PeerId::random();
        let _ga = budget.try_reserve(a, 100).expect("a fills its budget");
        // b is unaffected by a's saturation.
        let _gb = budget.try_reserve(b, 100).expect("b has its own budget");
        assert!(budget.try_reserve(a, 1).is_none());
    }

    #[test]
    fn zero_cap_disables_per_peer_limit() {
        let budget = PerPeerBudget::new(0);
        let peer = PeerId::random();
        // No reservation is ever refused; nothing is tracked.
        let _g = budget.try_reserve(peer, usize::MAX).expect("disabled cap always admits");
        assert_eq!(budget.inflight_for(&peer), 0);
    }
}
