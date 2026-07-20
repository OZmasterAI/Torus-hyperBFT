//! B1 — batched direct-to-leader forward envelope (Package B, design §1).
//!
//! Coalesces RPC-admitted native actions into ONE `/torus/direct` request per
//! (flush window, leader), reusing the 0xFD pre-proposal batch wire format
//! (`0xFD ‖ bincode(Vec<(Address, SignedNativeAction)>)`) that every deployed
//! binary already decodes, verifies, dedups, and pool-inserts. This replaces
//! the one-request-per-action 0xFE path, whose request rate is O(client
//! actions/s) — at client batch_size=1 that exceeds the request-response
//! stream cap (100/connection) the moment leader latency spikes. Batched, the
//! request rate is O(10–200)/s regardless of client batching.
//!
//! Knobs (all `OnceLock` + pure-parse-seam, same idiom as
//! `TORUS_HASH_ONLY_PUSH_THRESHOLD`):
//! - `TORUS_D2L_BATCH` (default `1`): `0` restores the per-action 0xFE
//!   forward path byte-for-byte (the documented rollback).
//! - `TORUS_D2L_BATCH_MS` (default `25`): flush interval — half the 50 ms
//!   gossip pre-spread cadence, because in d2l mode the forward is on the
//!   inclusion critical path.
//! - `TORUS_D2L_BATCH_MAX_BYTES` (default `524288`, clamped to the 4 MB
//!   legacy-fleet direct-codec floor): envelope byte cap.
//! - `TORUS_D2L_REFORWARD_MS` (default `2000`, `0` = off): re-forward sweep —
//!   re-enqueue the oldest still-pending pool actions to the CURRENT leader,
//!   healing lost envelopes and stale leader hints (dedup at the leader makes
//!   this idempotent).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

use torus_types::{Address, SignedNativeAction};

/// bincode's `Vec` length prefix (u64) — the fixed overhead between the sum
/// of the serialized pairs and the envelope body the receiving codec measures.
pub(crate) const VEC_LEN_PREFIX_BYTES: usize = 8;

/// Max pending pool actions re-enqueued per re-forward sweep tick. Bounds the
/// sweep's clone-out and the burst of envelopes it can emit (the byte cap
/// still slices them); dedup at the leader makes re-sends free.
const REFORWARD_MAX_ACTIONS: usize = 1024;

type Pairs = Vec<(Address, SignedNativeAction)>;

/// One per-target accumulation bucket: pairs in arrival order plus the
/// running sum of their serialized sizes (WITHOUT the Vec length prefix —
/// [`VEC_LEN_PREFIX_BYTES`] is added wherever the envelope-body size is
/// compared against the cap).
#[derive(Default)]
struct Bucket {
    pairs: Pairs,
    bytes: usize,
}

/// Pure per-target accumulator for the batched d2l forward (design §1): every
/// `push` either stays pending (the tick ships it) or returns ONE
/// `(target, pairs)` envelope forced out by the byte cap. No I/O, no clocks —
/// the async task around it owns the tick and the network handle.
pub(crate) struct ForwardBatcher {
    /// Hard cap on the bincode envelope BODY (`Vec` prefix + pairs); the 0xFD
    /// marker byte is noise. Callers clamp it to the legacy-fleet 4 MB
    /// direct-codec floor before construction.
    max_bytes: usize,
    buckets: HashMap<[u8; 32], Bucket>,
}

impl ForwardBatcher {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            buckets: HashMap::new(),
        }
    }

    /// Accumulate one admitted action for `target`. Returns a flushed
    /// envelope when the byte cap forces one out ahead of the tick:
    /// - the newcomer lands exactly AT the cap → ship the whole bucket now;
    /// - the newcomer WOULD push the bucket over → ship the pending pairs,
    ///   the newcomer starts the next bucket;
    /// - a single item at/over the cap alone ships alone (never wedges).
    ///
    /// Every returned envelope is therefore ≤ the cap except the
    /// oversized-alone case, which no cap choice can split.
    pub(crate) fn push(
        &mut self,
        target: [u8; 32],
        sender: Address,
        action: SignedNativeAction,
    ) -> Option<([u8; 32], Pairs)> {
        // Sizing serializes through references — identical bytes to the owned
        // pair, no clone. These types cannot fail in-memory serialization
        // (the encode seam in torus-network serializes the same pairs).
        let pair_bytes = bincode::serialized_size(&(sender, &action))
            .expect("bincode sizing of in-memory action") as usize;
        let bucket = self.buckets.entry(target).or_default();

        if bucket.pairs.is_empty() {
            bucket.pairs.push((sender, action));
            bucket.bytes = pair_bytes;
            if VEC_LEN_PREFIX_BYTES + bucket.bytes >= self.max_bytes {
                // Oversized (or cap-sized) alone: ship immediately.
                let pairs = std::mem::take(&mut bucket.pairs);
                bucket.bytes = 0;
                return Some((target, pairs));
            }
            return None;
        }

        let new_total = VEC_LEN_PREFIX_BYTES + bucket.bytes + pair_bytes;
        if new_total > self.max_bytes {
            // Ship the pending pairs; the newcomer starts the next bucket.
            let pairs = std::mem::take(&mut bucket.pairs);
            bucket.pairs.push((sender, action));
            bucket.bytes = pair_bytes;
            return Some((target, pairs));
        }
        bucket.pairs.push((sender, action));
        bucket.bytes += pair_bytes;
        if new_total >= self.max_bytes {
            // Landed exactly at the cap: no headroom left, ship now.
            let pairs = std::mem::take(&mut bucket.pairs);
            bucket.bytes = 0;
            return Some((target, pairs));
        }
        None
    }

    /// Drain every non-empty bucket — the tick handler. One envelope per
    /// target; order across targets is unspecified.
    pub(crate) fn flush_all(&mut self) -> Vec<([u8; 32], Pairs)> {
        self.buckets
            .drain()
            .filter(|(_, b)| !b.pairs.is_empty())
            .map(|(target, b)| (target, b.pairs))
            .collect()
    }

    /// Test seam: nothing pending in any bucket (production code drains via
    /// the tick's `flush_all`, which never needs to ask first).
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.buckets.values().all(|b| b.pairs.is_empty())
    }

    /// Running serialized size of `target`'s pending pairs (WITHOUT the Vec
    /// length prefix). Test seam for the accounting == serializer invariant.
    #[cfg(test)]
    fn bucket_bytes(&self, target: &[u8; 32]) -> usize {
        self.buckets.get(target).map_or(0, |b| b.bytes)
    }
}

/// Flush-time leader resolution (design §1): the CURRENT leader hint wins
/// over the (possibly stale) admission-time bucket target; `None` means skip
/// the flush entirely — this node became the leader and the actions are
/// already in its local pool, so forwarding would be a self-send.
pub(crate) fn resolve_flush_target(
    bucket_target: [u8; 32],
    current_hint: Option<[u8; 32]>,
    own_vk: [u8; 32],
) -> Option<[u8; 32]> {
    let target = current_hint.unwrap_or(bucket_target);
    (target != own_vk).then_some(target)
}

// ---------------------------------------------------------------------------
// Env knobs — pure parse seams + OnceLock readers (the
// TORUS_HASH_ONLY_PUSH_THRESHOLD idiom, torus-network/src/bridge.rs).
// ---------------------------------------------------------------------------

/// `TORUS_D2L_BATCH`: default ON; only an explicit off-word disables (a typo
/// must never silently flip the dissemination path).
fn parse_d2l_batch(v: Option<&str>) -> bool {
    match v {
        Some(s) => !matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// `TORUS_D2L_BATCH_MS`: default 25 ms (half the 50 ms gossip cadence); zero
/// would busy-loop the tick, so it falls back like garbage does.
fn parse_d2l_batch_ms(v: Option<&str>) -> u64 {
    v.and_then(|s| s.trim().parse().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(25)
}

/// `TORUS_D2L_BATCH_MAX_BYTES`: default 512 KB (deliberately equal to the
/// pre-proposal `HASH_ONLY_PUSH_THRESHOLD` default), CLAMPED to the
/// legacy-fleet 4 MB direct-codec floor — an above-floor cap orders sends the
/// oldest fleet codec must reject at read time (the S388 lesson).
fn parse_d2l_batch_max_bytes(v: Option<&str>, floor: usize) -> usize {
    v.and_then(|s| s.trim().parse().ok())
        .filter(|&b| b > 0)
        .unwrap_or(512 * 1024)
        .min(floor)
}

/// `TORUS_D2L_REFORWARD_MS`: default 2000; `0` is a REAL value (sweep off).
fn parse_d2l_reforward_ms(v: Option<&str>) -> u64 {
    v.and_then(|s| s.trim().parse().ok()).unwrap_or(2000)
}

/// Whether the batched d2l forward path is enabled (`TORUS_D2L_BATCH`,
/// default ON). `=0` restores the per-action 0xFE path byte-for-byte.
pub(crate) fn d2l_batch_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| parse_d2l_batch(std::env::var("TORUS_D2L_BATCH").ok().as_deref()))
}

fn d2l_batch_ms() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| parse_d2l_batch_ms(std::env::var("TORUS_D2L_BATCH_MS").ok().as_deref()))
}

fn d2l_batch_max_bytes() -> usize {
    static V: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let requested = std::env::var("TORUS_D2L_BATCH_MAX_BYTES").ok();
        let floor = torus_network::caps::LEGACY_FLEET_DIRECT_MSG_FLOOR;
        let effective = parse_d2l_batch_max_bytes(requested.as_deref(), floor);
        if let Some(ref r) = requested {
            if r.trim().parse::<usize>().map(|b| b > floor).unwrap_or(false) {
                warn!(
                    requested = %r,
                    floor, "TORUS_D2L_BATCH_MAX_BYTES above fleet direct-msg floor — clamped"
                );
            }
        }
        effective
    })
}

fn d2l_reforward_ms() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        parse_d2l_reforward_ms(std::env::var("TORUS_D2L_REFORWARD_MS").ok().as_deref())
    })
}

// ---------------------------------------------------------------------------
// The batcher task (replaces the per-item RPC→network forward bridge).
// ---------------------------------------------------------------------------

/// B2 verifier gate: the re-forward sweep runs ONLY when leader-forwarding is
/// armed (`--native-gossip=false`, the same `forward_bodies` gate the RPC
/// forward path honors) AND the ms knob is non-zero. In default gossip mode
/// the pre-spread already delivers every body to the leader — a sweep there
/// ships pooled actions the leader already has (default-behavior drift).
fn sweep_enabled(reforward_ms: u64, forwarding_armed: bool) -> bool {
    forwarding_armed && reforward_ms > 0
}

/// Spawn the B1 ForwardBatcher task: consume structured
/// `(leader hint, sender, action)` tuples from the bounded RPC forward
/// channel, coalesce per target, and ship ONE 0xFD envelope per
/// (flush window, target) via `forward_native_action_batch` — leader
/// re-resolved at flush time. Every `TORUS_D2L_REFORWARD_MS` (0 = off) the
/// sweep re-enqueues up to [`REFORWARD_MAX_ACTIONS`] oldest still-pending
/// pool actions to the CURRENT leader (dedup at the leader makes this
/// idempotent), healing lost envelopes and stale leader hints.
pub(crate) fn spawn(
    mut fwd_rx: tokio::sync::mpsc::Receiver<torus_rpc::ForwardedAction>,
    network: torus_network::bridge::LibP2PNetwork,
    leader_vk_fn: Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>,
    own_vk: [u8; 32],
    mempool: Arc<torus_mempool::Mempool>,
    forwarding_armed: bool,
) {
    let batch_ms = d2l_batch_ms();
    let max_bytes = d2l_batch_max_bytes();
    let reforward_ms = d2l_reforward_ms();
    let sweep_on = sweep_enabled(reforward_ms, forwarding_armed);
    info!(
        batch_ms,
        max_bytes, reforward_ms, sweep_on, "B1 d2l forward batcher armed (TORUS_D2L_BATCH=1)"
    );
    tokio::spawn(async move {
        let mut batcher = ForwardBatcher::new(max_bytes);
        let mut tick = tokio::time::interval(Duration::from_millis(batch_ms));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // With the sweep off the interval still exists but its branch is
        // disabled below (a 0 ms interval would panic).
        let mut sweep = tokio::time::interval(Duration::from_millis(reforward_ms.max(1)));
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                item = fwd_rx.recv() => {
                    let Some((target, sender, action)) = item else {
                        // RPC side gone — drain what's pending and exit.
                        for (t, pairs) in batcher.flush_all() {
                            dispatch(&network, &leader_vk_fn, own_vk, t, pairs);
                        }
                        break;
                    };
                    if let Some((t, pairs)) = batcher.push(target, sender, action) {
                        dispatch(&network, &leader_vk_fn, own_vk, t, pairs);
                    }
                }
                _ = tick.tick() => {
                    for (t, pairs) in batcher.flush_all() {
                        dispatch(&network, &leader_vk_fn, own_vk, t, pairs);
                    }
                }
                _ = sweep.tick(), if sweep_on => {
                    // Re-forward the oldest still-pending pool actions to the
                    // CURRENT leader. Selection is read-only over the sorted
                    // pool; the byte cap slices the re-sends into envelopes.
                    if let Some(leader) = leader_vk_fn() {
                        if leader != own_vk {
                            for (sender, action) in
                                mempool.select_native_for_block_with_senders(REFORWARD_MAX_ACTIONS)
                            {
                                if let Some((t, pairs)) = batcher.push(leader, sender, action) {
                                    dispatch(&network, &leader_vk_fn, own_vk, t, pairs);
                                }
                            }
                        }
                    }
                }
            }
        }
    });
}

/// Ship one flushed envelope: re-resolve the leader AT FLUSH TIME (the hint
/// wins over the admission-time bucket target; a self-target skips — the
/// actions are already in the local pool), then hand the pairs to the
/// network's PushScheduler-bounded, retry-tracked batch forward.
fn dispatch(
    network: &torus_network::bridge::LibP2PNetwork,
    leader_vk_fn: &Arc<dyn Fn() -> Option<[u8; 32]> + Send + Sync>,
    own_vk: [u8; 32],
    bucket_target: [u8; 32],
    pairs: Pairs,
) {
    let Some(target) = resolve_flush_target(bucket_target, leader_vk_fn(), own_vk) else {
        return;
    };
    match ed25519_dalek::VerifyingKey::from_bytes(&target) {
        Ok(vk) => network.forward_native_action_batch(vk, pairs),
        Err(e) => warn!(?e, "d2l flush target is not a valid verifying key — dropping envelope"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_network::caps::LEGACY_FLEET_DIRECT_MSG_FLOOR;
    use torus_types::{ActionSignature, NativeAction, Signature, SignedNativeAction};

    fn act(nonce: u64) -> SignedNativeAction {
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    fn addr(seed: u8) -> torus_types::Address {
        torus_types::Address::from([seed; 20])
    }

    fn vk(seed: u8) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = seed;
        b
    }

    /// One serialized pair's size on the wire, for byte-cap arithmetic.
    fn pair_size() -> usize {
        bincode::serialized_size(&(addr(1), act(1))).unwrap() as usize
    }

    /// B1 (RED first): items accumulate per target and stay pending under the
    /// byte cap — the tick (flush_all) is what ships them, ONE envelope per
    /// (window, target), pairs in arrival order. MUST fail before the module
    /// exists.
    #[test]
    fn batcher_one_envelope_per_target_per_window() {
        let mut b = ForwardBatcher::new(1024 * 1024);
        // 3 items for leader A, 2 for leader B, all far below the cap.
        assert!(b.push(vk(1), addr(1), act(1)).is_none());
        assert!(b.push(vk(1), addr(2), act(2)).is_none());
        assert!(b.push(vk(1), addr(3), act(3)).is_none());
        assert!(b.push(vk(2), addr(4), act(4)).is_none());
        assert!(b.push(vk(2), addr(5), act(5)).is_none());
        assert!(!b.is_empty());

        let mut flushed = b.flush_all();
        flushed.sort_by_key(|(t, _)| *t);
        assert_eq!(flushed.len(), 2, "one envelope per target per window");
        assert_eq!(flushed[0].0, vk(1));
        assert_eq!(
            flushed[0].1.iter().map(|(a, _)| *a).collect::<Vec<_>>(),
            vec![addr(1), addr(2), addr(3)],
            "arrival order preserved"
        );
        assert_eq!(flushed[1].0, vk(2));
        assert_eq!(flushed[1].1.len(), 2);
        assert!(b.is_empty(), "flush_all drains every bucket");
        assert!(b.flush_all().is_empty(), "nothing pending after a flush");
    }

    /// B1 (RED first): the byte cap forces an envelope out ahead of the tick —
    /// both when the bucket lands exactly at/over the cap (ship it now) and
    /// when the incoming item would push a pending bucket over (ship the
    /// pending pairs first; the newcomer starts the next bucket). Every
    /// envelope stays <= the cap, hence <= the 4 MB legacy codec floor.
    #[test]
    fn batcher_flushes_on_byte_cap() {
        let p = pair_size();

        // Cap = exactly two pairs: the second push lands AT the cap and ships
        // both immediately (no waiting out the tick).
        let mut b = ForwardBatcher::new(VEC_LEN_PREFIX_BYTES + 2 * p);
        assert!(b.push(vk(1), addr(1), act(1)).is_none());
        let (target, pairs) = b.push(vk(1), addr(2), act(2)).expect("cap flush");
        assert_eq!(target, vk(1));
        assert_eq!(pairs.len(), 2);
        assert!(
            bincode::serialized_size(&pairs).unwrap() as usize
                <= VEC_LEN_PREFIX_BYTES + 2 * p,
            "flushed envelope body within the cap"
        );
        assert!(b.is_empty());

        // Cap = one-and-a-half pairs: the second push WOULD exceed, so the
        // pending single-pair bucket ships and the newcomer stays pending.
        let mut b = ForwardBatcher::new(VEC_LEN_PREFIX_BYTES + p + p / 2);
        assert!(b.push(vk(1), addr(1), act(1)).is_none());
        let (_, pairs) = b.push(vk(1), addr(2), act(2)).expect("pre-cap flush");
        assert_eq!(pairs.len(), 1, "pending pairs ship; newcomer starts fresh");
        assert_eq!(pairs[0].0, addr(1));
        let rest = b.flush_all();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].1[0].0, addr(2), "newcomer flushed on the next tick");

        // A single item at/over the cap alone still ships (alone) rather than
        // wedging the bucket.
        let mut b = ForwardBatcher::new(1);
        let (_, pairs) = b.push(vk(1), addr(3), act(3)).expect("oversized-alone flush");
        assert_eq!(pairs.len(), 1);
        assert!(b.is_empty());
    }

    /// B1 (RED first): the incremental byte accounting must equal the real
    /// bincode envelope-body size, or the cap drifts from what the receiving
    /// codec measures (same invariant the gossip batcher pins in
    /// `native_batch_size_accounting_matches_serializer`).
    #[test]
    fn batcher_byte_accounting_matches_serializer() {
        let mut b = ForwardBatcher::new(usize::MAX);
        for i in 0..5u8 {
            b.push(vk(1), addr(i + 1), act(u64::from(i) + 10));
        }
        let tracked = VEC_LEN_PREFIX_BYTES + b.bucket_bytes(&vk(1));
        let flushed = b.flush_all();
        assert_eq!(
            tracked,
            bincode::serialize(&flushed[0].1).unwrap().len(),
            "running budget must equal the on-wire envelope body size"
        );
    }

    /// B1 (RED first): the forward target is re-resolved AT FLUSH TIME — a
    /// leader rotation inside the window re-targets the whole envelope (kills
    /// intra-window staleness); if this node became the leader, the flush is
    /// skipped entirely (the actions are already in the local pool); with no
    /// current hint the admission-time bucket target is the fallback.
    #[test]
    fn flush_target_re_resolves_leader() {
        let own = vk(9);
        // Current leader hint wins over the (stale) bucket target.
        assert_eq!(resolve_flush_target(vk(1), Some(vk(2)), own), Some(vk(2)));
        // We became the leader: skip — never forward to self.
        assert_eq!(resolve_flush_target(vk(1), Some(own), own), None);
        // No hint: fall back to the admission-time target.
        assert_eq!(resolve_flush_target(vk(1), None, own), Some(vk(1)));
        // No hint and the bucket target IS us: skip.
        assert_eq!(resolve_flush_target(own, None, own), None);
    }

    /// B1 (RED first): the knob parse seams. `TORUS_D2L_BATCH` unset/garbage
    /// defaults ON (`0` is the exact-today per-action rollback); the flush
    /// interval and byte cap fall back on unset/zero/garbage; the byte cap is
    /// CLAMPED to the legacy-fleet 4 MB direct-codec floor; the re-forward
    /// sweep accepts `0` as a real value (off).
    #[test]
    fn d2l_knob_parse_seams() {
        // TORUS_D2L_BATCH — default ON.
        assert!(parse_d2l_batch(None));
        assert!(parse_d2l_batch(Some("1")));
        assert!(parse_d2l_batch(Some(" on ")));
        assert!(!parse_d2l_batch(Some("0")));
        assert!(!parse_d2l_batch(Some("false")));
        assert!(parse_d2l_batch(Some("garbage")), "typo never flips the path");

        // TORUS_D2L_BATCH_MS — default 25; zero would busy-loop the tick.
        assert_eq!(parse_d2l_batch_ms(None), 25);
        assert_eq!(parse_d2l_batch_ms(Some("100")), 100);
        assert_eq!(parse_d2l_batch_ms(Some("0")), 25);
        assert_eq!(parse_d2l_batch_ms(Some("junk")), 25);

        // TORUS_D2L_BATCH_MAX_BYTES — default 512 KB, clamped to the floor.
        let floor = LEGACY_FLEET_DIRECT_MSG_FLOOR;
        assert_eq!(parse_d2l_batch_max_bytes(None, floor), 512 * 1024);
        assert_eq!(parse_d2l_batch_max_bytes(Some("1048576"), floor), 1048576);
        assert_eq!(
            parse_d2l_batch_max_bytes(Some("8000000"), floor),
            floor,
            "cap above the legacy codec floor is clamped (S388 lesson)"
        );
        assert_eq!(parse_d2l_batch_max_bytes(Some("0"), floor), 512 * 1024);
        assert_eq!(parse_d2l_batch_max_bytes(Some("junk"), floor), 512 * 1024);

        // TORUS_D2L_REFORWARD_MS — default 2000; 0 is a REAL value (off).
        assert_eq!(parse_d2l_reforward_ms(None), 2000);
        assert_eq!(parse_d2l_reforward_ms(Some("0")), 0);
        assert_eq!(parse_d2l_reforward_ms(Some("500")), 500);
        assert_eq!(parse_d2l_reforward_ms(Some("junk")), 2000);
    }

    /// Verifier fix (RED first): the re-forward sweep must run ONLY when
    /// leader-forwarding is armed (`--native-gossip=false`, the same
    /// `forward_bodies` gate the RPC forward path honors). Default gossip mode
    /// has NO direct-to-leader forwarding today — a sweep that ships pooled
    /// actions to the leader every 2 s under stock flags is default-behavior
    /// drift (the pre-spread already delivers those bodies), violating the
    /// exact-today-default hard rule.
    #[test]
    fn sweep_only_when_forwarding_armed() {
        // d2l mode (forwarding armed): sweep honors the ms knob.
        assert!(sweep_enabled(2000, true));
        assert!(!sweep_enabled(0, true), "TORUS_D2L_REFORWARD_MS=0 stays off");
        // gossip mode (forwarding NOT armed): sweep must never run, whatever
        // the knob says — exact-today default behavior.
        assert!(!sweep_enabled(2000, false));
        assert!(!sweep_enabled(0, false));
    }
}
