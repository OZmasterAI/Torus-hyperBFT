//! Exec-aware native admission (opt-in, `TORUS_ADMISSION_EXEC_AWARE`).
//!
//! Committed native actions leave the pool when their block is dispatched to
//! the execution thread, before they execute. With the exec channel holding up
//! to [`crate::rate_limit::EXEC_QUEUE_DEPTH`] blocks (plus rank-2 parks), that
//! committed-but-unexecuted work is invisible to the pooled-backlog check.
//! [`ExecBacklog`] counts it per height so admission can add it to the pooled
//! normal entries.
//!
//! Invariants: the total equals the sum of the outstanding heights; a height
//! is counted at most once; finishing height `h` releases every height at or
//! below `h` (execution is strictly in height order), and a height at or below
//! the finished watermark is never counted again. So the total never goes
//! negative and cannot leak upward through a skipped or re-delivered height.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Committed-but-unexecuted normal native actions, per dispatched height.
#[derive(Default)]
pub struct ExecBacklog {
    inner: Mutex<Inner>,
    /// Lock-free mirror of `Inner::sum` for the admission hot path.
    total: AtomicUsize,
}

#[derive(Default)]
struct Inner {
    by_height: BTreeMap<u64, usize>,
    sum: usize,
    /// Highest height reported executed (this process).
    finished_through: Option<u64>,
}

impl ExecBacklog {
    /// Count `height` as dispatched to execution with `count()` normal
    /// actions. A no-op (and `count` is not called) when `height` is already
    /// counted or at or below the finished watermark. `publish` gets the new
    /// total under the lock, so a gauge never goes stale.
    pub fn dispatched(
        &self,
        height: u64,
        count: impl FnOnce() -> usize,
        publish: impl FnOnce(usize),
    ) {
        let mut inner = self.inner.lock().unwrap();
        if inner.finished_through.is_some_and(|done| height <= done)
            || inner.by_height.contains_key(&height)
        {
            return;
        }
        let n = count();
        inner.by_height.insert(height, n);
        inner.sum = inner.sum.saturating_add(n);
        self.total.store(inner.sum, Ordering::Relaxed);
        publish(inner.sum);
    }

    /// Execution of `height` finished (or was abandoned at it): release every
    /// counted height at or below it.
    pub fn finished_through(&self, height: u64, publish: impl FnOnce(usize)) {
        let mut inner = self.inner.lock().unwrap();
        inner.finished_through = Some(inner.finished_through.map_or(height, |d| d.max(height)));
        // Heights above `height` stay; everything at or below is done.
        let above = match height.checked_add(1) {
            Some(next) => inner.by_height.split_off(&next),
            None => BTreeMap::new(),
        };
        let done = std::mem::replace(&mut inner.by_height, above);
        if done.is_empty() {
            return;
        }
        let released: usize = done.values().sum();
        inner.sum = inner.sum.saturating_sub(released);
        self.total.store(inner.sum, Ordering::Relaxed);
        publish(inner.sum);
    }

    /// Drop every outstanding height (the execution pipeline is gone).
    pub fn clear(&self, publish: impl FnOnce(usize)) {
        let mut inner = self.inner.lock().unwrap();
        inner.by_height.clear();
        inner.sum = 0;
        self.total.store(0, Ordering::Relaxed);
        publish(0);
    }

    /// Committed-but-unexecuted normal actions now.
    pub fn outstanding(&self) -> usize {
        self.total.load(Ordering::Relaxed)
    }

    /// Heights currently counted (tests, diagnostics).
    pub fn outstanding_heights(&self) -> usize {
        self.inner.lock().unwrap().by_height.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nop(_: usize) {}

    #[test]
    fn counts_dispatch_and_releases_on_finish() {
        let b = ExecBacklog::default();
        b.dispatched(5, || 10, nop);
        b.dispatched(6, || 7, nop);
        assert_eq!(b.outstanding(), 17);
        b.finished_through(5, nop);
        assert_eq!(b.outstanding(), 7);
        b.finished_through(6, nop);
        assert_eq!(b.outstanding(), 0);
        assert_eq!(b.outstanding_heights(), 0);
    }

    #[test]
    fn dispatch_is_idempotent_per_height_and_skips_counting() {
        let b = ExecBacklog::default();
        b.dispatched(3, || 4, nop);
        // A rank-2 retry of the same height: not counted, `count` not run.
        b.dispatched(3, || panic!("recounted a counted height"), nop);
        assert_eq!(b.outstanding(), 4);
    }

    #[test]
    fn finish_sweeps_lower_heights_and_watermark_blocks_late_adds() {
        let b = ExecBacklog::default();
        b.dispatched(1, || 2, nop);
        b.dispatched(2, || 3, nop);
        b.dispatched(4, || 5, nop);
        // Finishing 3 (never counted) releases 1 and 2, keeps 4.
        b.finished_through(3, nop);
        assert_eq!(b.outstanding(), 5);
        // A stale dispatch at or below the watermark never counts.
        b.dispatched(2, || panic!("counted below the watermark"), nop);
        b.dispatched(3, || panic!("counted at the watermark"), nop);
        assert_eq!(b.outstanding(), 5);
        // A lower finish does not lower the watermark or touch 4.
        b.finished_through(1, nop);
        assert_eq!(b.outstanding(), 5);
        b.finished_through(4, nop);
        assert_eq!(b.outstanding(), 0);
    }

    #[test]
    fn clear_drops_everything_and_later_heights_count_again() {
        let b = ExecBacklog::default();
        b.dispatched(7, || 9, nop);
        b.dispatched(8, || 1, nop);
        b.clear(nop);
        assert_eq!(b.outstanding(), 0);
        assert_eq!(b.outstanding_heights(), 0);
        b.dispatched(9, || 2, nop);
        assert_eq!(b.outstanding(), 2);
    }

    #[test]
    fn publish_sees_the_new_total() {
        let b = ExecBacklog::default();
        let seen = std::cell::Cell::new(usize::MAX);
        b.dispatched(1, || 6, |t| seen.set(t));
        assert_eq!(seen.get(), 6);
        b.dispatched(2, || 4, |t| seen.set(t));
        assert_eq!(seen.get(), 10);
        b.finished_through(1, |t| seen.set(t));
        assert_eq!(seen.get(), 4);
        b.clear(|t| seen.set(t));
        assert_eq!(seen.get(), 0);
    }

    /// Property: over random interleavings of in-order dispatches (with
    /// re-dispatch retries and stale re-deliveries), in-order finishes
    /// (sometimes of uncounted heights) and clears, the total always equals
    /// the sum over a reference model of outstanding heights, and ends at 0
    /// once everything is finished.
    #[test]
    fn total_equals_sum_of_outstanding_under_random_sequences() {
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _case in 0..200 {
            let b = ExecBacklog::default();
            let mut model: BTreeMap<u64, usize> = BTreeMap::new();
            let mut watermark: Option<u64> = None;
            let mut dispatched_to = 0u64; // highest height dispatched
            for _step in 0..300 {
                match next() % 10 {
                    // New height (strictly ascending, maybe with a gap).
                    0..=3 => {
                        let h = dispatched_to + 1 + next() % 2;
                        let n = (next() % 50) as usize;
                        b.dispatched(h, || n, nop);
                        if watermark.is_none_or(|w| h > w) {
                            model.entry(h).or_insert(n);
                        }
                        dispatched_to = h;
                    }
                    // Retry / re-delivery of an already-seen height.
                    4 => {
                        if dispatched_to > 0 {
                            let h = 1 + next() % dispatched_to;
                            b.dispatched(h, || 1_000_000, nop);
                            if watermark.is_none_or(|w| h > w) && !model.contains_key(&h) {
                                model.insert(h, 1_000_000);
                            }
                        }
                    }
                    // Execution finishes the next outstanding height, or a
                    // height in between (a skipped / uncounted block).
                    5..=8 => {
                        let lo = watermark.map_or(1, |w| w + 1);
                        if lo <= dispatched_to {
                            let h = lo + next() % (dispatched_to - lo + 1).min(3);
                            b.finished_through(h, nop);
                            model.retain(|k, _| *k > h);
                            watermark = Some(watermark.map_or(h, |w| w.max(h)));
                        }
                    }
                    // Pipeline teardown.
                    _ => {
                        if next() % 8 == 0 {
                            b.clear(nop);
                            model.clear();
                        }
                    }
                }
                assert_eq!(b.outstanding(), model.values().sum::<usize>());
                assert_eq!(b.outstanding_heights(), model.len());
            }
            b.finished_through(dispatched_to, nop);
            assert_eq!(b.outstanding(), 0, "leaked after finishing everything");
        }
    }

    #[test]
    fn concurrent_dispatch_and_finish_end_at_zero() {
        let b = std::sync::Arc::new(ExecBacklog::default());
        let (tx, rx) = std::sync::mpsc::sync_channel::<u64>(8);
        let exec = {
            let b = b.clone();
            std::thread::spawn(move || {
                while let Ok(h) = rx.recv() {
                    b.finished_through(h, nop);
                }
            })
        };
        for h in 1..=5_000u64 {
            // Counted BEFORE the send, as dispatch does.
            b.dispatched(h, || (h % 17) as usize, nop);
            tx.send(h).unwrap();
        }
        drop(tx);
        exec.join().unwrap();
        assert_eq!(b.outstanding(), 0);
        assert_eq!(b.outstanding_heights(), 0);
    }
}
