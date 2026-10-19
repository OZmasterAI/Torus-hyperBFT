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

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use alloy_primitives::B256;

/// Most hashes the fence pins at once (32 MiB-ish of map). Pins are
/// released as the GC worker passes their height, so this only fills while
/// the chain validates proposals without dispatching (a stall). Past it the
/// fence stops pinning and every pass keeps every body until the worker
/// passed the highest height it was asked to pin meanwhile: never unsafe,
/// only late.
pub const DA_GC_FENCE_MAX_PINS: usize = 1 << 20;

/// See the module docs. Off (every call a no-op) until
/// [`set_enabled`](Self::set_enabled): only a GC pass off the checking
/// threads needs it.
#[derive(Debug)]
pub struct DaGcFence {
    enabled: AtomicBool,
    max_pins: usize,
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
    /// The hashes pinned at each height (release order).
    by_height: BTreeMap<u64, Vec<[u8; 32]>>,
    /// The bodies the running pass is deleting (between `begin_delete` and
    /// `end_delete`).
    deleting: HashSet<[u8; 32]>,
    /// Over the pin cap: the highest height a pin was asked for since. Every
    /// pass keeps every body until the worker passed it.
    overflow_through: Option<u64>,
}

impl DaGcFence {
    /// A fence that pins at most `max_pins` hashes (see
    /// [`DA_GC_FENCE_MAX_PINS`]).
    pub fn with_max_pins(max_pins: usize) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            max_pins,
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

    /// Pin `hashes`, referenced by a block at `height`, BEFORE checking that
    /// their bodies are present. Returns the ones a GC pass is deleting right
    /// now: treat them as missing. Empty (and nothing pinned) when off.
    pub fn pin(&self, height: u64, hashes: &[B256]) -> Vec<B256> {
        if hashes.is_empty() || !self.is_enabled() {
            return Vec::new();
        }
        let mut guard = self.lock();
        let inner = &mut *guard;
        if inner.overflow_through.is_some() || inner.pins.len() + hashes.len() > self.max_pins {
            // Over the cap: keep everything instead (see DA_GC_FENCE_MAX_PINS).
            let entering = inner.overflow_through.is_none();
            let through = inner.overflow_through.get_or_insert(height);
            *through = (*through).max(height);
            if entering {
                tracing::warn!(
                    pins = inner.pins.len(),
                    max = self.max_pins,
                    height,
                    "DA body GC fence full: every GC pass keeps every body until the worker \
                     passes this height"
                );
            }
        } else {
            let at = inner.by_height.entry(height).or_default();
            for hash in hashes {
                match inner.pins.entry(hash.0) {
                    Entry::Occupied(mut pinned) => {
                        if *pinned.get() < height {
                            pinned.insert(height);
                            at.push(hash.0);
                        }
                    }
                    Entry::Vacant(slot) => {
                        slot.insert(height);
                        at.push(hash.0);
                    }
                }
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
    /// drop the pinned ones, mark the rest as being deleted. Returns how
    /// many it dropped. Call [`end_delete`](Self::end_delete) once the write
    /// returned, whatever its result.
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
    /// below it (a hash pinned again higher up keeps that pin).
    pub fn release_through(&self, height: u64) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let keep = inner.by_height.split_off(&height.saturating_add(1));
        let released = std::mem::replace(&mut inner.by_height, keep);
        if inner.overflow_through.is_some_and(|through| through <= height) {
            inner.overflow_through = None;
        }
        if height == u64::MAX {
            inner.pins.clear();
            return;
        }
        for (at, hashes) in released {
            for hash in hashes {
                if inner.pins.get(&hash) == Some(&at) {
                    inner.pins.remove(&hash);
                }
            }
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
