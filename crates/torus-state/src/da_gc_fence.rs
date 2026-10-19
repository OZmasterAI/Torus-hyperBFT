//! RH4: the fence between a DA body GC pass running on its own thread and
//! the threads that decide "this body is here" (proposal checks, validation,
//! the proposer's body mirror).
//!
//! On the consensus thread the GC pass could not interleave with a block's
//! validation: a block validated before the pass was in the block tree when
//! the pass read its uncommitted tail, and one validated after it found the
//! deleted body missing and fetched it. A pass on a worker thread loses that
//! order: a block can be validated (its body found present) after the pass
//! read the tail and before its delete batch lands, and then reference a body
//! the pass deletes. The fence restores the order:
//!
//! - Before checking a block's bodies, the checking thread pins their hashes
//!   at the block's height ([`DaGcFence::pin`]). `pin` returns the hashes a
//!   pass is deleting right now; the caller treats those as missing (they may
//!   be gone the moment the batch lands, and a write racing that batch may be
//!   lost), and they read as absent once the batch landed.
//! - The pass, right before its batch write, drops every pinned body from its
//!   deletes and marks the rest as being deleted
//!   ([`DaGcFence::begin_delete`]), and clears that mark once the write
//!   returned ([`DaGcFence::end_delete`]).
//!
//! Both steps take one mutex, held for in-memory set operations only, never
//! across a DB read or write: a stalled pass never blocks the consensus
//! thread on the fence. Either the pin comes first (the pass keeps the body:
//! the row goes, the body, its reference and shards stay, as for the
//! uncommitted tail) or the mark does (the checker treats the body as
//! missing, and finds it absent once the batch landed). So a body found
//! present after its hash was pinned stays.
//!
//! A pin lasts until the GC worker finished a pass at or above the pin's
//! height ([`DaGcFence::release_through`]). By then a block committed at that
//! height is dispatched, and no later pass deletes anything until its GC
//! record (which owns the body from then on) is durable; any other block at
//! that height lost its fork and can never commit.
//!
//! # Bounds
//!
//! Every pinned hash has exactly one height entry: re-pinning it higher up
//! moves the entry (the old one is superseded), so hash plus height entries
//! stay at most twice [`DA_GC_FENCE_MAX_PINS`] whatever the pins. A pin above
//! the committed tip plus [`DA_GC_FENCE_HEIGHT_WINDOW`]
//! ([`DaGcFence::note_committed`]) is refused: no block that far ahead is
//! voted on or committed before the chain gets there, but a speculative
//! decode (`TORUS_ASYNC_VALIDATE`) pins whatever height a datum claims,
//! before any ancestry check. A refused pin returns every hash: the caller
//! treats those bodies as missing (`MissingData` / fetch), so it never relies
//! on a body the fence does not protect. Over the cap the fence pins nothing
//! more and the GC holds every pass (`DaGcHoldReason::FenceFull`) until the
//! worker passed the highest height asked for meanwhile; the window keeps
//! that height within reach of the chain, so the hold ends.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use alloy_primitives::B256;

/// Most hashes the fence pins at once (with their height entries about 100
/// MiB at worst). Pins are released as the GC worker passes their height, so
/// this only fills while the chain validates proposals without dispatching
/// (a stall) or the worker lags far behind. Past it the fence stops pinning
/// and every pass holds until the worker passed the highest height it was
/// asked to pin meanwhile: never unsafe, only late.
pub const DA_GC_FENCE_MAX_PINS: usize = 1 << 20;

/// How far above the committed tip ([`DaGcFence::note_committed`]) a pin is
/// taken. Proposals are checked one to a few heights above the tip, block
/// sync from the committed tip up; anything further is a speculative or
/// bogus height, refused (read as missing).
pub const DA_GC_FENCE_HEIGHT_WINDOW: u64 = 1024;

/// See the module docs. Off (every call a no-op) until
/// [`set_enabled`](Self::set_enabled): only a GC pass off the checking
/// threads needs it.
#[derive(Debug)]
pub struct DaGcFence {
    enabled: AtomicBool,
    max_pins: AtomicUsize,
    /// The highest committed height noted: the window's base.
    committed: AtomicU64,
    /// Pins refused above the window, ever.
    refused: AtomicU64,
    inner: Mutex<FenceInner>,
}

impl Default for DaGcFence {
    fn default() -> Self {
        Self::with_max_pins(DA_GC_FENCE_MAX_PINS)
    }
}

#[derive(Debug, Default)]
struct FenceInner {
    /// Pinned hash -> the highest height it is pinned at.
    pins: HashMap<[u8; 32], u64>,
    /// `(height, hash)` for every entry of `pins`, and nothing else (release
    /// order).
    by_height: BTreeSet<(u64, [u8; 32])>,
    /// The bodies the running pass is deleting (between `begin_delete` and
    /// `end_delete`; at most one pass's delete limit).
    deleting: HashSet<[u8; 32]>,
    /// Over the pin cap: the highest height a pin was asked for since. Every
    /// pass holds until the worker passed it.
    overflow_through: Option<u64>,
}

impl DaGcFence {
    /// A fence that pins at most `max_pins` hashes (see
    /// [`DA_GC_FENCE_MAX_PINS`]).
    pub fn with_max_pins(max_pins: usize) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            max_pins: AtomicUsize::new(max_pins),
            committed: AtomicU64::new(0),
            refused: AtomicU64::new(0),
            inner: Mutex::new(FenceInner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FenceInner> {
        // The guarded sets stay consistent at every await-free step, so a
        // poisoned lock (a panic elsewhere while holding it) is still usable.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Turn the fence on or off. Off drops every pin and mark.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
        if !on {
            let mut inner = self.lock();
            inner.pins.clear();
            inner.by_height.clear();
            inner.deleting.clear();
            inner.overflow_through = None;
        }
    }

    /// Whether the fence is on.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// A block at `height` committed: pins up to `height` plus
    /// [`DA_GC_FENCE_HEIGHT_WINDOW`] are taken from now on. Never moves
    /// back. One atomic, no lock (the consensus thread, every commit).
    pub fn note_committed(&self, height: u64) {
        self.committed.fetch_max(height, Ordering::SeqCst);
    }

    /// Change the pin cap (tests, tools).
    pub fn set_max_pins(&self, max_pins: usize) {
        self.max_pins.store(max_pins, Ordering::SeqCst);
    }

    /// Pin `hashes`, referenced by a block at `height`, BEFORE checking that
    /// their bodies are present. Returns the ones the caller must treat as
    /// missing: those a GC pass is deleting right now, or all of them when
    /// `height` is above the window (the pin is refused, nothing stored).
    /// Empty (and nothing pinned) when off.
    pub fn pin(&self, height: u64, hashes: &[B256]) -> Vec<B256> {
        if hashes.is_empty() || !self.is_enabled() {
            return Vec::new();
        }
        let top = self
            .committed
            .load(Ordering::SeqCst)
            .saturating_add(DA_GC_FENCE_HEIGHT_WINDOW);
        if height > top {
            self.refused.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                height,
                top,
                hashes = hashes.len(),
                "DA body GC fence: pin above the window refused, its bodies read as missing"
            );
            return hashes.to_vec();
        }
        let max = self.max_pins.load(Ordering::SeqCst);
        let mut guard = self.lock();
        let inner = &mut *guard;
        let mut full = inner.overflow_through.is_some();
        if !full {
            for hash in hashes {
                if let Some(at) = inner.pins.get_mut(&hash.0) {
                    // Superseded: one height entry per hash, the highest.
                    if *at < height {
                        inner.by_height.remove(&(*at, hash.0));
                        inner.by_height.insert((height, hash.0));
                        *at = height;
                    }
                } else if inner.pins.len() < max {
                    inner.pins.insert(hash.0, height);
                    inner.by_height.insert((height, hash.0));
                } else {
                    full = true;
                    break;
                }
            }
        }
        if full {
            // Over the cap: every pass holds instead (DA_GC_FENCE_MAX_PINS).
            let entering = inner.overflow_through.is_none();
            let through = inner.overflow_through.get_or_insert(height);
            *through = (*through).max(height);
            if entering {
                tracing::warn!(
                    pins = inner.pins.len(),
                    max,
                    height,
                    "DA body GC fence full: every GC pass holds until the worker passes this \
                     height"
                );
            }
        }
        if inner.deleting.is_empty() {
            return Vec::new();
        }
        hashes
            .iter()
            .filter(|h| inner.deleting.contains(&h.0))
            .copied()
            .collect()
    }

    /// A GC pass about to write the deletes of `bodies` (`(hash, shards)`):
    /// drop the pinned ones (all of them while the fence is full), mark the
    /// rest as being deleted. Returns how many it dropped. Call
    /// [`end_delete`](Self::end_delete) once the write returned, whatever
    /// its result.
    pub fn begin_delete(&self, bodies: &mut Vec<([u8; 32], u16)>) -> u64 {
        if bodies.is_empty() || !self.is_enabled() {
            return 0;
        }
        let mut inner = self.lock();
        let before = bodies.len();
        if inner.overflow_through.is_some() {
            bodies.clear();
        }
        bodies.retain(|(hash, _)| !inner.pins.contains_key(hash));
        inner.deleting.extend(bodies.iter().map(|(hash, _)| *hash));
        (before - bodies.len()) as u64
    }

    /// The pass's delete batch returned (landed or failed).
    pub fn end_delete(&self) {
        let mut inner = self.lock();
        if !inner.deleting.is_empty() {
            inner.deleting.clear();
        }
    }

    /// The GC worker finished a pass at `height`: drop every pin at or
    /// below it (a hash pinned again higher up keeps that pin) and end a
    /// full fence's hold once `height` reached it.
    pub fn release_through(&self, height: u64) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        if inner.overflow_through.is_some_and(|through| through <= height) {
            inner.overflow_through = None;
        }
        let Some(above) = height.checked_add(1) else {
            inner.pins.clear();
            inner.by_height.clear();
            return;
        };
        let keep = inner.by_height.split_off(&(above, [0u8; 32]));
        let released = std::mem::replace(&mut inner.by_height, keep);
        for (_, hash) in released {
            inner.pins.remove(&hash);
        }
    }

    /// How many hashes are pinned.
    pub fn pinned(&self) -> usize {
        self.lock().pins.len()
    }

    /// Whether `hash` is pinned.
    pub fn is_pinned(&self, hash: &B256) -> bool {
        self.lock().pins.contains_key(&hash.0)
    }

    /// How many bodies the running pass is deleting (0 between passes).
    pub fn deleting(&self) -> usize {
        self.lock().deleting.len()
    }

    /// Whether the fence is over its cap (every pass holds).
    pub fn is_full(&self) -> bool {
        self.lock().overflow_through.is_some()
    }

    /// Hash entries plus height entries held (at most twice the cap).
    pub fn metadata_entries(&self) -> usize {
        let inner = self.lock();
        inner.pins.len() + inner.by_height.len()
    }

    /// Pins refused above the window since start.
    pub fn refused_pins(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> B256 {
        B256::from([n; 32])
    }

    fn bodies(ns: &[u8]) -> Vec<([u8; 32], u16)> {
        ns.iter().map(|&n| ([n; 32], 0)).collect()
    }

    #[test]
    fn off_fence_pins_nothing_and_filters_nothing() {
        let fence = DaGcFence::default();
        assert!(fence.pin(5, &[h(1)]).is_empty());
        assert_eq!(fence.pinned(), 0);
        let mut b = bodies(&[1, 2]);
        assert_eq!(fence.begin_delete(&mut b), 0);
        assert_eq!(b.len(), 2);
        assert_eq!(fence.deleting(), 0);
    }

    /// A pinned body is dropped from the pass's deletes; the rest are marked
    /// until the write returned.
    #[test]
    fn a_pin_before_the_pass_keeps_the_body() {
        let fence = DaGcFence::default();
        fence.set_enabled(true);
        assert!(fence.pin(10, &[h(1), h(2)]).is_empty());
        let mut b = bodies(&[1, 3]);
        assert_eq!(fence.begin_delete(&mut b), 1);
        assert_eq!(b, bodies(&[3]));
        assert_eq!(fence.deleting(), 1);
        fence.end_delete();
        assert_eq!(fence.deleting(), 0);
    }

    /// A pin while the pass is deleting returns the bodies being deleted
    /// (the caller treats them as missing); after the write they stay pinned,
    /// so no later pass deletes them.
    #[test]
    fn a_pin_during_the_delete_reports_the_bodies_being_deleted() {
        let fence = DaGcFence::default();
        fence.set_enabled(true);
        let mut b = bodies(&[1, 2]);
        assert_eq!(fence.begin_delete(&mut b), 0);
        assert_eq!(fence.pin(10, &[h(2), h(4)]), vec![h(2)]);
        fence.end_delete();
        assert!(fence.pin(10, &[h(2)]).is_empty(), "the write returned");
        let mut b = bodies(&[1, 2]);
        assert_eq!(fence.begin_delete(&mut b), 1, "2 stays pinned");
        assert_eq!(b, bodies(&[1]));
        fence.end_delete();
    }

    /// Pins go once the worker passed their height; a hash pinned again
    /// higher keeps its pin; `u64::MAX` releases everything.
    #[test]
    fn release_drops_the_pins_at_or_below_the_height() {
        let fence = DaGcFence::default();
        fence.set_enabled(true);
        fence.pin(10, &[h(1), h(2)]);
        fence.pin(12, &[h(2), h(3)]);
        fence.pin(11, &[h(3)]);
        assert_eq!(fence.pinned(), 3);
        fence.release_through(9);
        assert_eq!(fence.pinned(), 3);
        fence.release_through(10);
        assert!(!fence.is_pinned(&h(1)));
        assert!(fence.is_pinned(&h(2)), "pinned again at 12");
        fence.release_through(11);
        assert!(fence.is_pinned(&h(3)), "pinned at 12 too");
        fence.release_through(12);
        assert_eq!(fence.pinned(), 0);
        fence.pin(u64::MAX, &[h(9)]);
        fence.pin(0, &[h(8)]);
        assert!(fence.is_pinned(&h(8)), "height 0 pins too");
        fence.release_through(u64::MAX);
        assert_eq!(fence.pinned(), 0);
    }

    /// Over the pin cap the fence pins nothing more and every pass keeps
    /// every body, until the worker passed the highest height asked for.
    #[test]
    fn a_full_fence_keeps_every_body_until_the_worker_passes() {
        let fence = DaGcFence::with_max_pins(2);
        fence.set_enabled(true);
        fence.pin(10, &[h(1), h(2)]);
        fence.pin(11, &[h(3)]);
        fence.pin(12, &[h(4)]);
        assert_eq!(fence.pinned(), 2, "3 and 4 not pinned: over the cap");
        let mut b = bodies(&[5, 6]);
        assert_eq!(fence.begin_delete(&mut b), 2, "everything kept");
        assert!(b.is_empty());
        fence.end_delete();
        fence.release_through(11);
        let mut b = bodies(&[5]);
        assert_eq!(fence.begin_delete(&mut b), 1, "12 not passed yet");
        fence.end_delete();
        fence.release_through(12);
        let mut b = bodies(&[5]);
        assert_eq!(fence.begin_delete(&mut b), 0, "back to normal");
        fence.end_delete();
        fence.pin(13, &[h(7)]);
        assert!(fence.is_pinned(&h(7)));
    }

    /// Codex adversarial review (s132): one hash re-pinned at 200k rising
    /// heights (the probe: `cap=2 unique_pins=1 height_entries=200000`)
    /// holds ONE height entry: a re-pin supersedes the old one.
    #[test]
    fn re_pinning_a_hash_supersedes_its_old_height_entry() {
        let fence = DaGcFence::with_max_pins(2);
        fence.set_enabled(true);
        let x = h(1);
        for height in 1_000_000..1_200_000u64 {
            fence.note_committed(height - 1);
            assert!(fence.pin(height, &[x]).is_empty(), "{height}");
        }
        assert_eq!((fence.pinned(), fence.metadata_entries(), fence.is_full()), (1, 2, false));
        fence.release_through(1_199_998);
        assert!(fence.is_pinned(&x), "pinned at 1_199_999");
        fence.release_through(1_199_999);
        assert_eq!(fence.metadata_entries(), 0);
    }

    /// Hash entries plus height entries never exceed twice the cap, whatever
    /// the pins (re-pins, rising and falling heights, more hashes than the
    /// cap); over the cap every pass keeps every body until the worker
    /// passed the heights asked for.
    #[test]
    fn fence_metadata_is_bounded_by_the_cap() {
        const CAP: usize = 16;
        let fence = DaGcFence::with_max_pins(CAP);
        fence.set_enabled(true);
        fence.note_committed(1_000);
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
        // Under the cap (12 hashes), then over it (40).
        for (hashes, rounds) in [(12u8, 20_000u64), (40, 20_000)] {
            for i in 0..rounds {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let height = 1_000 + s % (DA_GC_FENCE_HEIGHT_WINDOW + 1);
                let pair = [h((s >> 32) as u8 % hashes), h((s >> 40) as u8 % hashes)];
                fence.pin(height, &pair);
                assert!(fence.metadata_entries() <= 2 * CAP, "{hashes} hashes, pin {i}");
            }
            assert_eq!(fence.is_full(), hashes > CAP as u8);
        }
        let mut b = bodies(&[200]);
        assert_eq!(fence.begin_delete(&mut b), 1, "full: everything kept");
        fence.end_delete();
        fence.release_through(1_000 + DA_GC_FENCE_HEIGHT_WINDOW);
        assert!(!fence.is_full());
        assert_eq!(fence.metadata_entries(), 0);
    }

    /// A pin above the committed tip plus the window (a speculative datum
    /// decoded before any ancestry check, a peer's far-future height) is
    /// refused and returned whole: the caller treats those bodies as missing
    /// (`MissingData`, fetch). Refused pins never fill the fence and never
    /// make it hold beyond the window; the window follows the committed tip
    /// and never moves back.
    #[test]
    fn a_pin_beyond_the_window_is_refused_and_reads_as_missing() {
        let fence = DaGcFence::with_max_pins(1);
        fence.set_enabled(true);
        fence.note_committed(100);
        let top = 100 + DA_GC_FENCE_HEIGHT_WINDOW;
        assert!(fence.pin(top, &[h(1)]).is_empty(), "the window's top pins");
        assert_eq!(fence.pin(top + 1, &[h(2), h(3)]), vec![h(2), h(3)]);
        assert_eq!(fence.pin(u64::MAX, &[h(4)]), vec![h(4)]);
        assert_eq!((fence.pinned(), fence.refused_pins(), fence.is_full()), (1, 2, false));
        // At the cap, an in-window pin holds the passes through its height.
        assert!(fence.pin(150, &[h(5)]).is_empty());
        assert!(fence.is_full());
        assert_eq!(fence.pin(u64::MAX, &[h(6)]), vec![h(6)], "still refused");
        fence.release_through(149);
        assert!(fence.is_full(), "150 not passed");
        fence.release_through(top);
        assert!(!fence.is_full(), "the worker passed every height asked for");
        assert_eq!(fence.metadata_entries(), 0);
        fence.note_committed(5_000);
        assert!(fence.pin(5_000 + DA_GC_FENCE_HEIGHT_WINDOW, &[h(7)]).is_empty());
        fence.note_committed(4_000);
        assert!(
            fence.pin(5_000 + DA_GC_FENCE_HEIGHT_WINDOW, &[h(8)]).is_empty(),
            "the tip never moves back"
        );
        assert_eq!(fence.refused_pins(), 3);
    }

    /// Turning the fence off drops every pin and mark.
    #[test]
    fn disabling_clears_the_fence() {
        let fence = DaGcFence::default();
        fence.set_enabled(true);
        fence.pin(1, &[h(1)]);
        let mut b = bodies(&[2]);
        fence.begin_delete(&mut b);
        fence.set_enabled(false);
        assert_eq!((fence.pinned(), fence.deleting()), (0, 0));
    }
}
