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
//! that height lost its fork and can never commit. A pin at or below the
//! height the worker passed protects nothing any more: a pin that needs room
//! may evict one.
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
//! on a body the fence does not protect.
//!
//! A hash that finds the fence at its cap is not pinned, and every pass holds
//! (`DaGcHoldReason::FenceFull`) until the worker passed the highest height
//! such a hash was refused at (the window keeps it within reach of the
//! chain). Only refusals move that horizon: as soon as there is room again
//! the fence pins every new hash individually, so honest checks a height or
//! two ahead of the worker never extend a hold.
//!
//! No lock hold does work proportional to the number of pins: the pin map is
//! reserved at the cap when the fence is turned on (no rehash under the lock),
//! a pin touches only its own hashes (and evicts at most one passed pin per
//! hash), and [`DaGcFence::release_through`] drops at most
//! [`DA_GC_FENCE_RELEASE_CHUNK`] pins per lock hold, letting a waiting pin in
//! between two chunks.
//!
//! The fence keys a hash by its 64-bit fingerprint under a per-fence random
//! SipHash key (computed before the lock is taken), not by the hash: 16-byte
//! entries instead of 40. Two hashes sharing a fingerprint (about 2^-64 a
//! pair, not steerable without the key) share one entry at the higher of
//! their heights, which only errs safe: a body kept or read as missing that
//! did not need to be, a pin held longer. No pinned hash ever reads as
//! unpinned.

use std::collections::hash_map::Entry;
use std::collections::hash_map::RandomState;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use alloy_primitives::B256;

/// Most hashes the fence pins at once. A pin lives from a block's body check
/// until the GC worker finished the pass at its height, so honest pins are
/// about (heights checked but not passed yet) x (bodies per block): with the
/// 400-action default block (torus-mempool `NATIVE_TOTAL_BLOCK_CAP`) and the
/// worker one or two heights behind, a few thousand. 2^18 still covers a
/// worker a full handoff queue behind (torus-consensus `DA_GC_QUEUE_CAP`, 512
/// heights x 400 = 204,800) with room for re-proposals and forks; the former
/// 2^20 covered the whole height window at 1000 actions a block, which only a
/// flood of speculative datums reaches. About 9 MiB for the pin map
/// (16-byte entries, reserved at the cap when the fence is turned on) plus at
/// most about 8 MiB of height entries. Past the cap a hash is refused and the
/// passes hold until the worker passed it: never unsafe, only late.
pub const DA_GC_FENCE_MAX_PINS: usize = 1 << 18;

/// How far above the committed tip ([`DaGcFence::note_committed`]) a pin is
/// taken. Proposals are checked one to a few heights above the tip, block
/// sync from the committed tip up; anything further is a speculative or
/// bogus height, refused (read as missing).
pub const DA_GC_FENCE_HEIGHT_WINDOW: u64 = 1024;

/// Most pins [`DaGcFence::release_through`] drops in one lock hold (well
/// under a millisecond in an optimized build); a pin waiting on the mutex
/// goes before the next chunk.
pub const DA_GC_FENCE_RELEASE_CHUNK: usize = 1024;

/// How long a release waits, between two chunks, for the pins queued on the
/// mutex to take it (`std`'s mutex is not fair: the releasing thread would
/// take it straight back). Only a pin preempted between queueing and taking
/// the lock makes it wait that long; the release is background work.
const RELEASE_YIELD_MAX: Duration = Duration::from_millis(10);

/// The fence's maps are keyed by fingerprints that are already keyed SipHash
/// outputs: hashing them again would only cost time.
#[derive(Debug, Default, Clone, Copy)]
struct FingerprintHasher(u64);

impl Hasher for FingerprintHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        // Only `u64` keys are hashed; fold anything else anyway.
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

type ByFingerprint = BuildHasherDefault<FingerprintHasher>;

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
    /// Threads in [`pin`](Self::pin) waiting for the mutex: a release lets
    /// them in between its chunks.
    pin_waiters: AtomicUsize,
    /// Pins dropped per lock hold by a release.
    release_chunk: AtomicUsize,
    /// The random SipHash key of the fingerprints (see the module docs).
    keys: RandomState,
    inner: Mutex<FenceInner>,
    /// Runs between two release chunks, without the lock (tests).
    #[cfg(test)]
    chunk_hook: ChunkHook,
    /// Release chunks done, and the most a pin waited for while queued on
    /// the mutex (tests).
    #[cfg(test)]
    chunks: AtomicU64,
    #[cfg(test)]
    max_chunks_waited: AtomicU64,
    /// [`RELEASE_YIELD_MAX`] in ms, raisable by a test that counts chunks:
    /// on a loaded host a woken pin can stay off-CPU past 10 ms (tests).
    #[cfg(test)]
    release_yield_max_ms: AtomicU64,
}

impl Default for DaGcFence {
    fn default() -> Self {
        Self::with_max_pins(DA_GC_FENCE_MAX_PINS)
    }
}

#[derive(Debug, Default)]
struct FenceInner {
    /// Pinned fingerprint -> the highest height it is pinned at.
    pins: HashMap<u64, u64, ByFingerprint>,
    /// `(height, fingerprint)` for every entry of `pins`, and nothing else
    /// (release order). Both change together under one lock hold, so every
    /// hold sees them agree.
    by_height: BTreeSet<(u64, u64)>,
    /// The fingerprints of the bodies the running pass is deleting (between
    /// `begin_delete` and `end_delete`; at most one pass's delete limit).
    deleting: HashSet<u64, ByFingerprint>,
    /// The highest height a hash was refused at for lack of room, until the
    /// worker passed it: every pass holds meanwhile.
    overflow_through: Option<u64>,
    /// The highest height the worker passed ([`DaGcFence::release_through`]):
    /// a pin at or below it protects nothing any more.
    passed: Option<u64>,
}

impl FenceInner {
    /// Drop the lowest pin if the worker already passed its height. Whether
    /// it did (there is room for one more pin then).
    fn evict_passed(&mut self) -> bool {
        match (self.by_height.first(), self.passed) {
            (Some(&(at, fp)), Some(passed)) if at <= passed => {
                self.by_height.pop_first();
                self.pins.remove(&fp);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct ChunkHook(Mutex<Option<Box<dyn FnMut() + Send>>>);

#[cfg(test)]
impl std::fmt::Debug for ChunkHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChunkHook")
    }
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
            pin_waiters: AtomicUsize::new(0),
            release_chunk: AtomicUsize::new(DA_GC_FENCE_RELEASE_CHUNK),
            keys: RandomState::new(),
            inner: Mutex::new(FenceInner::default()),
            #[cfg(test)]
            chunk_hook: ChunkHook::default(),
            #[cfg(test)]
            chunks: AtomicU64::new(0),
            #[cfg(test)]
            max_chunks_waited: AtomicU64::new(0),
            #[cfg(test)]
            release_yield_max_ms: AtomicU64::new(RELEASE_YIELD_MAX.as_millis() as u64),
        }
    }

    /// `hash`'s fingerprint (see the module docs).
    fn fingerprint(&self, hash: &[u8; 32]) -> u64 {
        self.keys.hash_one(hash)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FenceInner> {
        // The guarded sets stay consistent at every await-free step, so a
        // poisoned lock (a panic elsewhere while holding it) is still usable.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Turn the fence on or off. On reserves the pin map at the cap (so no
    /// pin rehashes it under the lock); off drops every pin and mark, freeing
    /// them after the lock is released.
    pub fn set_enabled(&self, on: bool) {
        if on {
            let want = self.max_pins.load(Ordering::SeqCst);
            if self.lock().pins.capacity() < want {
                // Allocated, and the old map freed, outside the lock.
                let reserved = HashMap::with_capacity_and_hasher(want, ByFingerprint::default());
                let old = {
                    let mut inner = self.lock();
                    inner
                        .pins
                        .is_empty()
                        .then(|| std::mem::replace(&mut inner.pins, reserved))
                };
                drop(old);
            }
            self.enabled.store(true, Ordering::SeqCst);
        } else {
            self.enabled.store(false, Ordering::SeqCst);
            let retired = std::mem::take(&mut *self.lock());
            drop(retired);
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

    /// Change the pin cap (tests, tools). The pin map is reserved at the cap
    /// only by [`set_enabled`](Self::set_enabled).
    pub fn set_max_pins(&self, max_pins: usize) {
        self.max_pins.store(max_pins, Ordering::SeqCst);
    }

    /// Pin `hashes`, referenced by a block at `height`, BEFORE checking that
    /// their bodies are present. Returns the ones the caller must treat as
    /// missing: those a GC pass is deleting right now, or all of them when
    /// `height` is above the window (the pin is refused, nothing stored).
    /// A hash that finds the fence at its cap is not pinned; every pass holds
    /// until the worker passed `height` instead. Empty (and nothing pinned)
    /// when off.
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
        let fps: Vec<u64> = hashes.iter().map(|h| self.fingerprint(&h.0)).collect();
        self.pin_waiters.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)]
        let queued_at = self.chunks.load(Ordering::SeqCst);
        let mut guard = self.lock();
        self.pin_waiters.fetch_sub(1, Ordering::SeqCst);
        #[cfg(test)]
        self.max_chunks_waited.fetch_max(
            self.chunks.load(Ordering::SeqCst) - queued_at,
            Ordering::SeqCst,
        );
        let inner = &mut *guard;
        let mut refused = 0usize;
        for &fp in &fps {
            if inner.pins.len() >= max
                && !inner.pins.contains_key(&fp)
                && !(inner.evict_passed() && inner.pins.len() < max)
            {
                refused += 1;
                continue;
            }
            match inner.pins.entry(fp) {
                Entry::Occupied(mut pin) => {
                    // Superseded: one height entry per hash, the highest.
                    let at = pin.get_mut();
                    if *at < height {
                        inner.by_height.remove(&(*at, fp));
                        inner.by_height.insert((height, fp));
                        *at = height;
                    }
                }
                Entry::Vacant(pin) => {
                    pin.insert(height);
                    inner.by_height.insert((height, fp));
                }
            }
        }
        if refused > 0 {
            // At the cap: the passes hold through this height instead.
            let entering = inner.overflow_through.is_none();
            let through = inner.overflow_through.get_or_insert(height);
            *through = (*through).max(height);
            if entering {
                tracing::warn!(
                    pins = inner.pins.len(),
                    max,
                    height,
                    refused,
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
            .zip(&fps)
            .filter(|(_, fp)| inner.deleting.contains(fp))
            .map(|(hash, _)| *hash)
            .collect()
    }

    /// A GC pass about to write the deletes of `bodies` (`(hash, shards)`):
    /// drop the pinned ones (all of them while a refused pin holds the
    /// passes), mark the rest as being deleted. Returns how many it dropped.
    /// Call [`end_delete`](Self::end_delete) once the write returned,
    /// whatever its result.
    pub fn begin_delete(&self, bodies: &mut Vec<([u8; 32], u16)>) -> u64 {
        if bodies.is_empty() || !self.is_enabled() {
            return 0;
        }
        let fps: Vec<u64> = bodies
            .iter()
            .map(|(hash, _)| self.fingerprint(hash))
            .collect();
        let mut guard = self.lock();
        let inner = &mut *guard;
        let before = bodies.len();
        if inner.overflow_through.is_some() {
            bodies.clear();
        }
        let mut fps = fps.into_iter();
        bodies.retain(|_| {
            let fp = fps.next().expect("one fingerprint per body");
            let delete = !inner.pins.contains_key(&fp);
            if delete {
                inner.deleting.insert(fp);
            }
            delete
        });
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
    /// below it (a hash pinned again higher up keeps that pin) and end the
    /// hold of a refused pin once `height` reached it.
    ///
    /// At most [`DA_GC_FENCE_RELEASE_CHUNK`] pins per lock hold, lowest
    /// first, each with its hash entry (the detached height entries are freed
    /// after the lock is released); a pin waiting on the mutex goes between
    /// two chunks. Every hold leaves the pins exact (each remaining
    /// one at its current height), so a pin taken between two chunks above
    /// `height` is never dropped, and one not yet reached still keeps its
    /// body; only pins the worker already passed (unneeded) can be seen
    /// mid-release.
    pub fn release_through(&self, height: u64) {
        let chunk = self.release_chunk.load(Ordering::SeqCst).max(1);
        let mut first = true;
        loop {
            let (more, retired) = {
                let mut guard = self.lock();
                let inner = &mut *guard;
                if first {
                    first = false;
                    inner.passed = Some(inner.passed.map_or(height, |p| p.max(height)));
                    if inner
                        .overflow_through
                        .is_some_and(|through| through <= height)
                    {
                        inner.overflow_through = None;
                    }
                }
                // The first entry this hold keeps: above `height`, or the
                // one after a full chunk. Detach everything before it.
                let cut = inner
                    .by_height
                    .iter()
                    .take(chunk.saturating_add(1))
                    .enumerate()
                    .find(|&(i, &(at, _))| i == chunk || at > height)
                    .map(|(_, &entry)| entry);
                let released = match cut {
                    Some(entry) => {
                        let kept = inner.by_height.split_off(&entry);
                        std::mem::replace(&mut inner.by_height, kept)
                    }
                    None => std::mem::take(&mut inner.by_height),
                };
                for (_, fp) in &released {
                    inner.pins.remove(fp);
                }
                #[cfg(test)]
                self.chunks.fetch_add(1, Ordering::SeqCst);
                (cut.is_some_and(|(at, _)| at <= height), released)
            };
            // The released entries' nodes are freed here, without the lock.
            drop(retired);
            if !more {
                return;
            }
            #[cfg(test)]
            if let Some(hook) = self
                .chunk_hook
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
            {
                hook();
            }
            let waited = Instant::now();
            while self.pin_waiters.load(Ordering::SeqCst) > 0
                && waited.elapsed() < self.release_yield_max()
            {
                std::thread::yield_now();
            }
        }
    }

    /// How long a release waits between two chunks for the queued pins.
    fn release_yield_max(&self) -> Duration {
        #[cfg(test)]
        return Duration::from_millis(self.release_yield_max_ms.load(Ordering::SeqCst));
        #[cfg(not(test))]
        RELEASE_YIELD_MAX
    }

    /// Pins a release drops per lock hold (tests).
    #[cfg(test)]
    fn set_release_chunk(&self, chunk: usize) {
        self.release_chunk.store(chunk, Ordering::SeqCst);
    }

    /// Run `hook` between two release chunks, without the lock (tests).
    #[cfg(test)]
    fn set_chunk_hook(&self, hook: Option<Box<dyn FnMut() + Send>>) {
        *self.chunk_hook.0.lock().unwrap_or_else(|e| e.into_inner()) = hook;
    }

    /// How many hashes are pinned.
    pub fn pinned(&self) -> usize {
        self.lock().pins.len()
    }

    /// Whether `hash` is pinned.
    pub fn is_pinned(&self, hash: &B256) -> bool {
        let fp = self.fingerprint(&hash.0);
        self.lock().pins.contains_key(&fp)
    }

    /// How many bodies the running pass is deleting (0 between passes).
    pub fn deleting(&self) -> usize {
        self.lock().deleting.len()
    }

    /// Whether a hash refused at the cap holds every pass (until the worker
    /// passed its height).
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
    use alloy_primitives::U256;

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
        assert_eq!(
            (fence.pinned(), fence.metadata_entries(), fence.is_full()),
            (1, 2, false)
        );
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
                assert!(
                    fence.metadata_entries() <= 2 * CAP,
                    "{hashes} hashes, pin {i}"
                );
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
        assert_eq!(
            (fence.pinned(), fence.refused_pins(), fence.is_full()),
            (1, 2, false)
        );
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
        assert!(fence
            .pin(5_000 + DA_GC_FENCE_HEIGHT_WINDOW, &[h(7)])
            .is_empty());
        fence.note_committed(4_000);
        assert!(
            fence
                .pin(5_000 + DA_GC_FENCE_HEIGHT_WINDOW, &[h(8)])
                .is_empty(),
            "the tip never moves back"
        );
        assert_eq!(fence.refused_pins(), 3);
    }

    /// Codex adversarial round 2 (s132, `/tmp/rh4-round2-store-proof`): after
    /// one saturation every later pin used to extend the hold, even once the
    /// stored pins drained, so honest pipelined checks at H+1 kept every pass
    /// held forever. The fence tracks new pins again as soon as it has room;
    /// the hold covers only the hashes it refused, and ends once the worker
    /// passed the highest of their heights. The honest schedule (the block at
    /// H pinned until pass H, the proposal at H+1 checked before it) needs two
    /// pins at a time, hence the cap of 2.
    #[test]
    fn honest_pipelining_after_a_saturation_lets_the_gc_resume() {
        let fence = DaGcFence::with_max_pins(2);
        fence.set_enabled(true);
        fence.note_committed(13);
        let n = |i: u64| B256::from(U256::from(i));
        assert!(fence.pin(15, &[n(1), n(2), n(3)]).is_empty());
        assert!(fence.is_full(), "n(3) refused at 15");
        let mut held = Vec::new();
        for h in 14..=1013u64 {
            fence.note_committed(h);
            assert!(fence.pin(h + 1, &[n(h + 10)]).is_empty());
            let mut b = bodies(&[200]);
            if fence.begin_delete(&mut b) == 1 {
                held.push(h);
            }
            fence.end_delete();
            fence.release_through(h);
        }
        // 14 and 15: refusals at 15 and 16 (n(1), n(2) pinned through 15);
        // 16: the hold through 16. From 17 on every pin is tracked.
        assert_eq!(held, vec![14, 15, 16]);
        assert_eq!((fence.pinned(), fence.is_full()), (1, false));
        assert!(fence.is_pinned(&n(1023)), "the proposal at 1014");
    }

    /// A pin that only partly fits tracks what fits (later hashes of the same
    /// pin included, a re-pin needs no room) and holds only through its own
    /// height; the hold does not grow with later pins that fit.
    #[test]
    fn the_hold_covers_only_refused_heights() {
        let fence = DaGcFence::with_max_pins(2);
        fence.set_enabled(true);
        fence.pin(10, &[h(1)]);
        assert!(fence.pin(11, &[h(2), h(3), h(1)]).is_empty());
        assert!(fence.is_full(), "h(3) refused at 11");
        assert!(fence.is_pinned(&h(2)) && !fence.is_pinned(&h(3)));
        fence.release_through(10);
        assert!(fence.is_pinned(&h(1)), "re-pinned at 11 although full");
        fence.release_through(11);
        assert_eq!((fence.pinned(), fence.is_full()), (0, false));
        fence.pin(12, &[h(4), h(5)]);
        assert!(fence.pin(14, &[h(6)]).is_empty());
        assert!(fence.is_full(), "h(6) refused at 14");
        fence.release_through(12);
        assert!(fence.pin(50, &[h(7)]).is_empty(), "room again: tracked");
        assert!(fence.is_pinned(&h(7)));
        fence.release_through(14);
        assert!(!fence.is_full(), "the hold ends at 14, not at 50");
        let mut b = bodies(&[7, 8]);
        assert_eq!(fence.begin_delete(&mut b), 1, "h(7) kept by its own pin");
        assert_eq!(b, bodies(&[8]));
        fence.end_delete();
    }

    /// Codex adversarial round 2 (s132, `/tmp/rh4-round2-fence-proof`:
    /// "release_ms=642 concurrent_single_pin_ms=638"): releasing a full fence
    /// held the mutex the consensus thread's body checks take for the whole
    /// release. It now releases in chunks of at most
    /// [`DA_GC_FENCE_RELEASE_CHUNK`] entries and lets a waiting pin in
    /// between them, so a one-hash pin waits for one chunk at most: counted
    /// in chunks (every attempt, whatever the machine's load) and in time
    /// (the best of three attempts, as other processes can preempt the
    /// releasing thread mid-chunk; the old fence waited for the whole
    /// release every time).
    ///
    /// The chunk count holds whatever the load only if the release waits for
    /// a queued pin until it took the lock: the node's 10 ms
    /// [`RELEASE_YIELD_MAX`] is a scheduling bound (s140: "a queued pin
    /// waited for 4 chunks" on 18c at load ~100, where a woken pin can stay
    /// off-CPU past 10 ms), so the count is taken with it raised to 1 s. The
    /// old fence (no hand-off between chunks) still fails it.
    ///
    /// In time, the 99th-percentile pin of an attempt is held to the bound,
    /// not the slowest: tens of thousands of pins run during a release, and
    /// on a loaded host one of them is always preempted mid-call (s140, 18c
    /// at load ~45: the slowest pin 100 ms - 1 s in every attempt). The pins
    /// start once the release dropped its first chunk: a pin into the full
    /// fence evicts a passed entry, one less for the release to drop (s140:
    /// "252 lock holds" when the releasing thread started late). A release
    /// in one lock hold (the old fence) fails the lock-hold count; one that
    /// does not let a queued pin in between its chunks fails the chunk count.
    #[test]
    fn a_pin_waits_at_most_one_release_chunk() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        // Unoptimized test builds run a chunk ~10x slower than the node.
        let bound = Duration::from_millis(if cfg!(debug_assertions) { 50 } else { 5 });
        let n = |i: u64| B256::from(U256::from(i));
        let all: Vec<B256> = (0..DA_GC_FENCE_MAX_PINS as u64).map(n).collect();
        let mut attempts = Vec::new();
        for _ in 0..3 {
            let fence = DaGcFence::default();
            fence.release_yield_max_ms.store(1_000, Ordering::SeqCst);
            fence.set_enabled(true);
            fence.note_committed(100);
            for chunk in all.chunks(20_000) {
                assert!(fence.pin(101, chunk).is_empty());
            }
            assert_eq!(fence.pinned(), DA_GC_FENCE_MAX_PINS);
            let done = AtomicBool::new(false);
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (mut waits, release) = std::thread::scope(|s| {
                let release = s.spawn(|| {
                    started_tx.send(()).unwrap();
                    let t = Instant::now();
                    fence.release_through(101);
                    done.store(true, Ordering::SeqCst);
                    t.elapsed()
                });
                started_rx.recv().unwrap();
                while fence.chunks.load(Ordering::SeqCst) == 0 {
                    std::thread::yield_now();
                }
                let mut waits = Vec::new();
                while !done.load(Ordering::SeqCst) || waits.is_empty() {
                    let t = Instant::now();
                    assert!(fence
                        .pin(102, &[n(u64::MAX - waits.len() as u64)])
                        .is_empty());
                    waits.push(t.elapsed());
                }
                (waits, release.join().unwrap())
            });
            let pins = waits.len() as u64;
            waits.sort_unstable();
            let (p99, worst) = (waits[waits.len() * 99 / 100], waits[waits.len() - 1]);
            let chunks = fence.chunks.load(Ordering::SeqCst);
            assert!(
                chunks >= (DA_GC_FENCE_MAX_PINS / DA_GC_FENCE_RELEASE_CHUNK) as u64,
                "{chunks} lock holds"
            );
            let waited = fence.max_chunks_waited.load(Ordering::SeqCst);
            assert!(waited <= 2, "a queued pin waited for {waited} chunks");
            assert_eq!(fence.pinned() as u64, pins, "only the pins at 102 remain");
            attempts.push(format!(
                "p99 {p99:?}, worst {worst:?} (release {release:?}, {pins} pins)"
            ));
            if p99 < bound {
                return;
            }
        }
        panic!(
            "the 99th-percentile one-hash pin waited over {bound:?} in every attempt: {attempts:?}"
        );
    }

    /// Releasing in chunks keeps pin/delete exclusion: between two chunks
    /// (the hook) a pass sees every pin not yet released and every pin taken
    /// meanwhile (a hash moved above the released height, a new one), and
    /// none of them is released after: only entries at or below the height
    /// go, each with its hash, in one lock hold.
    #[test]
    fn a_chunked_release_keeps_the_pins_it_has_not_reached() {
        use std::sync::Arc;
        let fence = Arc::new(DaGcFence::with_max_pins(1_000));
        fence.set_enabled(true);
        fence.set_release_chunk(10);
        let n = |i: u64| B256::from(U256::from(i));
        for i in 1..=100u64 {
            fence.pin(i, &[n(i)]);
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let (fence2, seen) = (Arc::downgrade(&fence), seen.clone());
            fence.set_chunk_hook(Some(Box::new(move || {
                let fence = fence2.upgrade().unwrap();
                let left = fence.pinned();
                seen.lock().unwrap().push(left);
                if left != 90 {
                    return;
                }
                // After the first chunk: 1..=10 released, 11..=100 not yet.
                assert!(fence.pin(200, &[n(50)]).is_empty(), "moved above 100");
                assert!(fence.pin(300, &[n(150)]).is_empty(), "a new pin");
                assert!(fence.pin(7, &[n(7)]).is_empty(), "at or below 100");
                let mut b: Vec<([u8; 32], u16)> = [5u64, 50, 60, 150, 250]
                    .iter()
                    .map(|&i| (n(i).0, 0))
                    .collect();
                assert_eq!(fence.begin_delete(&mut b), 3);
                assert_eq!(b, vec![(n(5).0, 0), (n(250).0, 0)]);
                fence.end_delete();
            })));
        }
        fence.release_through(100);
        fence.set_chunk_hook(None);
        assert_eq!(seen.lock().unwrap()[0], 90, "the first chunk released 10");
        assert!(
            seen.lock().unwrap().len() >= 9,
            "chunked: {:?}",
            seen.lock().unwrap()
        );
        assert_eq!(fence.pinned(), 2, "n(50) at 200 and n(150) at 300 stay");
        assert!(fence.is_pinned(&n(50)) && fence.is_pinned(&n(150)));
        assert_eq!(fence.metadata_entries(), 4);
        fence.release_through(200);
        assert_eq!(fence.pinned(), 1);
    }

    /// Release racing pins on another thread (chunks of 3): a hash pinned
    /// above the released height is never released, so a pass that follows
    /// the release always keeps it; once released below, a hash can go.
    #[test]
    fn release_racing_pins_never_drops_a_pin_above_the_height() {
        let fence = DaGcFence::with_max_pins(100_000);
        fence.set_enabled(true);
        fence.set_release_chunk(3);
        let n = |i: u64| B256::from(U256::from(i));
        // The highest height the pinner pinned anything at so far, and the
        // highest the worker released (the pinner stays a few above it).
        let pinned_through = AtomicU64::new(0);
        let released = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut i = 0u64;
                while !stop.load(Ordering::SeqCst) {
                    i += 1;
                    // Heights rise; every hash re-pinned once 4 heights up.
                    let height = i / 8 + 1;
                    while height > released.load(Ordering::SeqCst) + 6 {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::yield_now();
                    }
                    fence.pin(height, &[n(i), n(i.saturating_sub(32))]);
                    pinned_through.fetch_max(height, Ordering::SeqCst);
                }
            });
            for passed in 0..400u64 {
                // Every hash of height passed + 2 pinned (heights rise).
                while pinned_through.load(Ordering::SeqCst) < passed + 3 {
                    std::thread::yield_now();
                }
                fence.release_through(passed);
                released.store(passed, Ordering::SeqCst);
                // Every hash pinned at passed + 2 (i in [8(passed+1),
                // 8(passed+2))) must stay pinned.
                let mut b: Vec<([u8; 32], u16)> = (8 * (passed + 1)..8 * (passed + 2))
                    .map(|i| (n(i).0, 0))
                    .collect();
                let want = b.len() as u64;
                assert_eq!(fence.begin_delete(&mut b), want, "after release {passed}");
                fence.end_delete();
            }
            stop.store(true, Ordering::SeqCst);
        });
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
