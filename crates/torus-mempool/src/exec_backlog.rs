//! Exec-aware native admission (opt-in, `TORUS_ADMISSION_EXEC_AWARE`).
//!
//! Committed native actions leave the pool when their block is dispatched to
//! the execution thread, before they execute. With the exec channel holding up
//! to [`crate::rate_limit::EXEC_QUEUE_DEPTH`] blocks (plus rank-2 parks), that
//! committed-but-unexecuted work is invisible to the pooled-backlog check.
//! [`ExecBacklog`] counts it so admission can add it to the pooled normal
//! entries.
//!
//! Thread roles:
//! - the dispatcher (the consensus thread, the only caller of
//!   [`ExecBacklog::register`] / [`ExecBacklog::sent`]) counts a committed
//!   height once, when it is first materialized for execution (sent or
//!   parked), and hands its count to the exec thread with the block;
//! - the exec thread releases that count after executing the block
//!   ([`ExecBacklog::release`]): one CAS on an atomic, never a lock, so it
//!   can never wait on the dispatcher;
//! - admission readers load the atomic total.
//!
//! Invariants: the total equals the sum of the counts of the registered
//! heights not yet sent plus the counts carried by blocks sent but not yet
//! released; a height is counted at most once (idempotent registration, and
//! a height at or below the sent watermark never counts again); the scan
//! that counts a block's actions runs with no lock held; [`ExecBacklog::close`]
//! is terminal: it zeroes the total and every later registration or release
//! is a no-op, so a dispatch racing the pipeline teardown cannot leak.
//!
//! Gauge publication: changes are published as deltas only once
//! [`ExecBacklog::attach`] ran (the metrics may attach after boot parks
//! registered). Attach publishes the total at that instant BEFORE it sets
//! [`ATTACHED`] (release), and an update publishes iff its own CAS saw
//! [`ATTACHED`] (acquire), so the gauge is the sum of every published delta
//! and equals the total once quiescent. It never goes negative: an add is
//! published BEFORE its CAS (and taken back if a close won), a subtraction
//! AFTER its CAS, so every published subtraction follows the publication of
//! what it subtracts (its add, or the attach total that included it).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// High bit of [`ExecBacklog::state`]: the execution pipeline is gone.
const CLOSED: usize = 1 << (usize::BITS - 1);
/// Next bit: the gauge is attached ([`ExecBacklog::attach`]); updates
/// publish their deltas from then on. Set once, never cleared.
const ATTACHED: usize = 1 << (usize::BITS - 2);
/// The total's bits.
const TOTAL: usize = ATTACHED - 1;

/// Committed-but-unexecuted normal native actions.
#[derive(Default)]
pub struct ExecBacklog {
    /// Outstanding total in the low bits ([`TOTAL`]), [`CLOSED`] once torn
    /// down, [`ATTACHED`] once the gauge is attached. The only state the exec
    /// thread and admission readers touch.
    state: AtomicUsize,
    /// Dispatcher only: registered heights not yet sent. The exec thread
    /// never takes this lock.
    pending: Mutex<Pending>,
    /// Every scan, lock or total update (knob-off zero-cost evidence: the
    /// mempool never reaches this type with the knob off).
    touches: AtomicUsize,
}

#[derive(Default)]
struct Pending {
    by_height: BTreeMap<u64, usize>,
    /// Highest height handed to the exec channel (this process).
    sent_through: Option<u64>,
}

impl ExecBacklog {
    /// Dispatcher: `height` is committed and materialized for execution
    /// (about to be sent, or parked). Counts it with `count()` and returns
    /// its count, which the block carries to the exec thread. Idempotent: an
    /// already-registered height returns its count without rescanning. A
    /// height at or below the sent watermark, or any height after
    /// [`Self::close`], is a no-op returning 0 (`count` not called).
    ///
    /// `count` runs with NO lock held; the critical sections are map
    /// lookups/inserts. `publish` gets the change to the total (once
    /// attached; see the module docs).
    pub fn register(
        &self,
        height: u64,
        count: impl FnOnce() -> usize,
        publish: impl Fn(i64),
    ) -> usize {
        self.touch();
        if self.is_closed() {
            return 0;
        }
        {
            let pending = self.pending.lock().unwrap();
            if pending.sent_through.is_some_and(|s| height <= s) {
                return 0;
            }
            if let Some(&n) = pending.by_height.get(&height) {
                return n;
            }
        }
        let n = count();
        let mut pending = self.pending.lock().unwrap();
        if let Some(&counted) = pending.by_height.get(&height) {
            return counted; // a concurrent registrant won (single-dispatcher callers never get here)
        }
        if !self.add(n, &publish) {
            return 0; // torn down during the scan
        }
        pending.by_height.insert(height, n);
        n
    }

    /// Dispatcher: `height` entered the exec channel; its count now rides
    /// the block (released by [`Self::release`]). Raises the sent watermark.
    /// A registered height below it can never be sent any more (execution is
    /// strictly in height order): its count is dropped here.
    pub fn sent(&self, height: u64, publish: impl Fn(i64)) {
        self.touch();
        let mut pending = self.pending.lock().unwrap();
        pending.by_height.remove(&height);
        pending.sent_through = Some(pending.sent_through.map_or(height, |s| s.max(height)));
        if pending
            .by_height
            .first_key_value()
            .is_some_and(|(&low, _)| low < height)
        {
            let above = pending.by_height.split_off(&height);
            let stale = std::mem::replace(&mut pending.by_height, above);
            let n: usize = stale.values().sum();
            self.sub(n, &publish);
        }
    }

    /// Exec thread: a block carrying `n` (its [`Self::register`] count) was
    /// executed or abandoned. Lock-free: a single CAS loop on the total; a
    /// no-op after [`Self::close`].
    pub fn release(&self, n: usize, publish: impl Fn(i64)) {
        if n == 0 {
            return;
        }
        self.touch();
        self.sub(n, &publish);
    }

    /// The execution pipeline is gone (fail-stop, closed channel): zero the
    /// total, TERMINALLY. Lock-free (the exec thread calls it); every later
    /// registration or release is a no-op.
    pub fn close(&self, publish: impl Fn(i64)) {
        self.touch();
        // One atomic step: drop the total, keep ATTACHED (set once, never
        // cleared), set CLOSED.
        let prev = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                Some((s & ATTACHED) | CLOSED)
            })
            .unwrap_or_else(|s| s);
        let n = prev & TOTAL;
        if prev & CLOSED == 0 && prev & ATTACHED != 0 && n > 0 {
            publish(-(n as i64));
        }
    }

    /// The gauge is attached (the metrics handle exists now): publish the
    /// total at this instant, then publish every later change as a delta.
    /// Lock-free and idempotent. The total is published BEFORE [`ATTACHED`]
    /// becomes visible, so an update that sees it publishes after the total
    /// that includes what it changes (never negative, see the module docs).
    pub fn attach(&self, publish: impl Fn(i64)) {
        self.touch();
        let mut cur = self.state.load(Ordering::Acquire);
        let mut published = 0usize;
        loop {
            if cur & ATTACHED != 0 {
                break; // already attached
            }
            let t = if cur & CLOSED != 0 { 0 } else { cur & TOTAL };
            if t != published {
                publish(t as i64 - published as i64);
                published = t;
            }
            match self.state.compare_exchange_weak(
                cur,
                cur | ATTACHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }
        if published > 0 {
            publish(-(published as i64)); // a concurrent attach won
        }
    }

    /// Committed-but-unexecuted normal actions now (0 once closed).
    pub fn outstanding(&self) -> usize {
        let s = self.state.load(Ordering::Relaxed);
        if s & CLOSED != 0 {
            0
        } else {
            s & TOTAL
        }
    }

    /// Registered heights not yet sent (tests, diagnostics; 0 once closed).
    pub fn pending_heights(&self) -> usize {
        if self.is_closed() {
            return 0;
        }
        self.pending.lock().unwrap().by_height.len()
    }

    /// Scans, locks and total updates so far (tests: knob-off zero cost).
    pub fn touches(&self) -> usize {
        self.touches.load(Ordering::Relaxed)
    }

    fn is_closed(&self) -> bool {
        self.state.load(Ordering::Relaxed) & CLOSED != 0
    }

    fn touch(&self) {
        self.touches.fetch_add(1, Ordering::Relaxed);
    }

    /// Add `n` unless closed (atomic with [`Self::close`]). Once attached,
    /// `+n` is published BEFORE the CAS that adds it (taken back if a close
    /// wins), so a racing close's `-total` can never publish first.
    fn add(&self, n: usize, publish: &impl Fn(i64)) -> bool {
        let mut cur = self.state.load(Ordering::Acquire);
        let mut published = false;
        loop {
            if cur & CLOSED != 0 {
                if published {
                    publish(-(n as i64));
                }
                return false;
            }
            if n > 0 && cur & ATTACHED != 0 && !published {
                publish(n as i64);
                published = true;
            }
            let total = (cur & TOTAL).saturating_add(n).min(TOTAL);
            match self.state.compare_exchange_weak(
                cur,
                (cur & !TOTAL) | total,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Subtract `n` unless closed (atomic with [`Self::close`]); published
    /// AFTER the CAS, iff that CAS saw [`ATTACHED`].
    fn sub(&self, n: usize, publish: &impl Fn(i64)) {
        if n == 0 {
            return;
        }
        let res = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                (s & CLOSED == 0).then(|| (s & !TOTAL) | (s & TOTAL).saturating_sub(n))
            });
        if let Ok(prev) = res {
            let dropped = n.min(prev & TOTAL);
            if prev & ATTACHED != 0 && dropped > 0 {
                publish(-(dropped as i64));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn nop(_: i64) {}

    #[test]
    fn counts_registration_and_releases_what_the_block_carries() {
        let b = ExecBacklog::default();
        let n5 = b.register(5, || 10, nop);
        let n6 = b.register(6, || 7, nop);
        assert_eq!((n5, n6), (10, 7));
        assert_eq!(b.outstanding(), 17);
        b.sent(5, nop);
        b.sent(6, nop);
        assert_eq!(b.outstanding(), 17, "sent, not executed");
        assert_eq!(b.pending_heights(), 0);
        b.release(n5, nop);
        assert_eq!(b.outstanding(), 7);
        b.release(n6, nop);
        assert_eq!(b.outstanding(), 0);
    }

    #[test]
    fn registration_is_idempotent_per_height_and_skips_counting() {
        let b = ExecBacklog::default();
        assert_eq!(b.register(3, || 4, nop), 4);
        // A rank-2 retry / park of the same height: not counted, `count` not
        // run, and the same count is handed back for the send.
        assert_eq!(
            b.register(3, || panic!("recounted a counted height"), nop),
            4
        );
        assert_eq!(b.outstanding(), 4);
        assert_eq!(b.pending_heights(), 1);
    }

    #[test]
    fn sent_watermark_blocks_late_registrations() {
        let b = ExecBacklog::default();
        let n = b.register(4, || 5, nop);
        b.sent(4, nop);
        assert_eq!(b.register(4, || panic!("recounted a sent height"), nop), 0);
        assert_eq!(
            b.register(2, || panic!("counted below the watermark"), nop),
            0
        );
        assert_eq!(b.outstanding(), 5);
        b.release(n, nop);
        assert_eq!(b.outstanding(), 0);
    }

    #[test]
    fn sending_past_a_registered_height_drops_its_count() {
        let b = ExecBacklog::default();
        b.register(2, || 3, nop);
        let n = b.register(3, || 4, nop);
        b.sent(3, nop); // 2 can never be sent now
        assert_eq!(b.outstanding(), 4);
        assert_eq!(b.pending_heights(), 0);
        b.release(n, nop);
        assert_eq!(b.outstanding(), 0);
    }

    /// Codex r1 finding 2: the teardown is terminal. A registration after it
    /// is a no-op (no scan, no count), so a dispatch racing the exec loop's
    /// fail-stop can never leave a positive count behind.
    #[test]
    fn clear_is_terminal_later_heights_never_count() {
        let b = ExecBacklog::default();
        b.register(7, || 9, nop);
        let n8 = b.register(8, || 1, nop);
        b.sent(7, nop);
        b.close(nop);
        assert_eq!(b.outstanding(), 0);
        assert_eq!(b.pending_heights(), 0);
        assert_eq!(
            b.register(9, || panic!("scanned after the teardown"), nop),
            0
        );
        b.release(n8, nop); // a late release cannot underflow or reopen
        assert_eq!(b.outstanding(), 0);
    }

    /// Codex r1 finding 2: a close landing DURING a registration's scan wins:
    /// the scanned count is not added.
    #[test]
    fn close_during_the_scan_wins() {
        let b = ExecBacklog::default();
        let n = b.register(
            1,
            || {
                b.close(nop);
                6
            },
            nop,
        );
        assert_eq!(n, 0);
        assert_eq!(b.outstanding(), 0);
    }

    /// Codex r1 finding 3: the exec thread's release must never wait on a
    /// registration that is scanning a block's actions. The scan is paused
    /// (bounded) until the release completes; the release must finish while
    /// the scan is still paused.
    #[test]
    fn release_never_waits_on_an_in_progress_registration_scan() {
        use std::sync::mpsc;
        use std::time::Duration;
        let b = std::sync::Arc::new(ExecBacklog::default());
        let n1 = b.register(1, || 5, nop);
        b.sent(1, nop);
        let (in_scan_tx, in_scan_rx) = mpsc::channel::<()>();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let registrar = {
            let b = b.clone();
            std::thread::spawn(move || {
                b.register(
                    2,
                    || {
                        in_scan_tx.send(()).unwrap();
                        // Paused mid-scan; bounded so a regression fails
                        // instead of deadlocking.
                        let _ = resume_rx.recv_timeout(Duration::from_secs(5));
                        3
                    },
                    nop,
                );
            })
        };
        in_scan_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let exec = {
            let b = b.clone();
            std::thread::spawn(move || {
                b.release(n1, nop);
                done_tx.send(()).unwrap();
            })
        };
        let released_during_scan = done_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        let _ = resume_tx.send(());
        registrar.join().unwrap();
        exec.join().unwrap();
        assert!(
            released_during_scan,
            "exec release blocked by an in-progress registration scan"
        );
        assert_eq!(b.outstanding(), 3);
    }

    /// The scan also runs outside the pending-map lock: a diagnostic read
    /// of the map completes while a registration is paused mid-scan.
    #[test]
    fn scan_holds_no_lock() {
        use std::sync::mpsc;
        use std::time::Duration;
        let b = std::sync::Arc::new(ExecBacklog::default());
        let (in_scan_tx, in_scan_rx) = mpsc::channel::<()>();
        let (resume_tx, resume_rx) = mpsc::channel::<()>();
        let registrar = {
            let b = b.clone();
            std::thread::spawn(move || {
                b.register(
                    1,
                    || {
                        in_scan_tx.send(()).unwrap();
                        let _ = resume_rx.recv_timeout(Duration::from_secs(5));
                        2
                    },
                    nop,
                )
            })
        };
        in_scan_rx.recv().unwrap();
        let unlocked = b.pending.try_lock().is_ok();
        let _ = resume_tx.send(());
        assert_eq!(registrar.join().unwrap(), 2);
        assert!(unlocked, "the scan held the pending-map lock");
    }

    #[test]
    fn publish_sees_every_change_and_sums_to_the_total() {
        let b = ExecBacklog::default();
        let gauge = std::cell::Cell::new(0i64);
        let add = |d: i64| gauge.set(gauge.get() + d);
        b.attach(add);
        assert_eq!(gauge.get(), 0);
        let n1 = b.register(1, || 6, add);
        assert_eq!(gauge.get(), 6);
        b.register(2, || 4, add);
        assert_eq!(gauge.get(), 10);
        b.sent(1, add);
        b.release(n1, add);
        assert_eq!(gauge.get(), 4);
        b.close(add);
        assert_eq!(gauge.get(), 0);
        b.register(3, || 9, add);
        assert_eq!(gauge.get(), 0);
    }

    /// Codex r2 finding 2: registrations before the gauge is attached (boot
    /// parks, before node startup attaches the metrics) publish nothing;
    /// attaching publishes the total, and later releases bring the gauge
    /// to exactly 0, never below.
    #[test]
    fn attach_after_registration_publishes_the_total_never_negative() {
        let b = ExecBacklog::default();
        let gauge = std::cell::Cell::new(0i64);
        let min = std::cell::Cell::new(0i64);
        let add = |d: i64| {
            gauge.set(gauge.get() + d);
            min.set(min.get().min(gauge.get()));
        };
        let n1 = b.register(1, || 1, add);
        let n2 = b.register(2, || 4, add);
        assert_eq!(gauge.get(), 0, "not attached yet: nothing published");
        b.attach(add);
        assert_eq!(gauge.get(), 5, "attach publishes the total");
        b.attach(add);
        assert_eq!(gauge.get(), 5, "attach is idempotent");
        b.sent(1, add);
        b.release(n1, add);
        assert_eq!((b.outstanding(), gauge.get()), (4, 4));
        b.sent(2, add);
        b.release(n2, add);
        assert_eq!((b.outstanding(), gauge.get()), (0, 0));
        assert_eq!(min.get(), 0, "the gauge went negative");
    }

    /// Attaching after the teardown publishes 0; nothing later publishes.
    #[test]
    fn attach_after_close_publishes_nothing() {
        let b = ExecBacklog::default();
        let gauge = std::cell::Cell::new(0i64);
        let add = |d: i64| gauge.set(gauge.get() + d);
        let n = b.register(1, || 3, add);
        b.close(add);
        b.attach(add);
        b.release(n, add);
        b.register(2, || 7, add);
        assert_eq!((b.outstanding(), gauge.get()), (0, 0));
    }

    /// Codex r2 finding 2, concurrent: the metrics attach at an arbitrary
    /// point while the dispatcher registers/sends and the exec thread
    /// releases (and sometimes fail-stops). The gauge never goes negative
    /// and ends equal to the counter.
    #[test]
    fn concurrent_attach_gauge_never_negative_and_ends_equal() {
        use std::sync::atomic::AtomicI64;
        use std::sync::Arc;
        for case in 0..100u64 {
            let b = Arc::new(ExecBacklog::default());
            let gauge = Arc::new(AtomicI64::new(0));
            let min = Arc::new(AtomicI64::new(0));
            let publish = {
                let (gauge, min) = (gauge.clone(), min.clone());
                move |d: i64| {
                    let now = gauge.fetch_add(d, Ordering::SeqCst) + d;
                    min.fetch_min(now, Ordering::SeqCst);
                }
            };
            let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(4);
            let close_at = (case % 3 == 0).then_some(30 + case as usize);
            let exec = {
                let (b, publish) = (b.clone(), publish.clone());
                std::thread::spawn(move || {
                    let mut seen = 0;
                    while let Ok(n) = rx.recv() {
                        b.release(n, &publish);
                        seen += 1;
                        if Some(seen) == close_at {
                            b.close(&publish);
                        }
                    }
                })
            };
            let attacher = {
                let (b, publish) = (b.clone(), publish.clone());
                std::thread::spawn(move || {
                    for _ in 0..(case * 37 % 2_000) {
                        std::hint::spin_loop();
                    }
                    b.attach(&publish);
                })
            };
            for h in 1..=300u64 {
                // Park a few heights ahead (registered, not yet sent).
                b.register(h + 2, || ((h + 2) % 7) as usize, &publish);
                let n = b.register(h, || (h % 7) as usize, &publish);
                if tx.send(n).is_err() {
                    break;
                }
                b.sent(h, &publish);
            }
            drop(tx);
            exec.join().unwrap();
            attacher.join().unwrap();
            assert!(
                min.load(Ordering::SeqCst) >= 0,
                "case {case}: gauge went negative"
            );
            assert_eq!(
                gauge.load(Ordering::SeqCst),
                b.outstanding() as i64,
                "case {case}: gauge != counter"
            );
        }
    }

    /// Property: over random sequences of in-order registrations (with
    /// park re-registrations and stale re-deliveries), in-order sends,
    /// FIFO releases (as the exec channel delivers) and a rare terminal
    /// close, the total always equals the reference model (pending counts
    /// plus in-flight counts, 0 once closed), and ends at 0 once everything
    /// is sent and released.
    #[test]
    fn total_equals_model_under_random_sequences() {
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        for _case in 0..200 {
            let b = ExecBacklog::default();
            let mut pending: BTreeMap<u64, usize> = BTreeMap::new();
            let mut in_flight: VecDeque<usize> = VecDeque::new();
            let mut sent_through = 0u64;
            let mut registered_to = 0u64;
            let mut closed = false;
            for _step in 0..300 {
                match next() % 10 {
                    // A newly committed height (ascending, maybe parked).
                    0..=3 => {
                        let h = registered_to + 1;
                        let n = (next() % 50) as usize;
                        let got = b.register(h, || n, nop);
                        if closed {
                            assert_eq!(got, 0);
                        } else {
                            assert_eq!(got, n);
                            pending.insert(h, n);
                        }
                        registered_to = h;
                    }
                    // Retry / re-delivery of an already-seen height.
                    4 => {
                        if registered_to > 0 {
                            let h = 1 + next() % registered_to;
                            let got = b.register(h, || 1_000_000, nop);
                            let want = if closed || h <= sent_through {
                                0
                            } else {
                                pending[&h]
                            };
                            assert_eq!(got, want);
                        }
                    }
                    // Send the lowest pending height.
                    5 | 6 => {
                        if let Some((&h, &n)) = pending.first_key_value() {
                            pending.remove(&h);
                            b.sent(h, nop);
                            sent_through = h;
                            in_flight.push_back(n);
                        }
                    }
                    // Execute the oldest in-flight block.
                    7 | 8 => {
                        if let Some(n) = in_flight.pop_front() {
                            b.release(n, nop);
                        }
                    }
                    // Pipeline teardown (rare, terminal).
                    _ => {
                        if next() % 16 == 0 {
                            b.close(nop);
                            closed = true;
                            pending.clear();
                        }
                    }
                }
                let model = if closed {
                    0
                } else {
                    pending.values().sum::<usize>() + in_flight.iter().sum::<usize>()
                };
                assert_eq!(b.outstanding(), model);
                if !closed {
                    assert_eq!(b.pending_heights(), pending.len());
                }
            }
            while let Some((&h, &n)) = pending.first_key_value() {
                pending.remove(&h);
                b.sent(h, nop);
                in_flight.push_back(n);
            }
            while let Some(n) = in_flight.pop_front() {
                b.release(n, nop);
            }
            assert_eq!(b.outstanding(), 0, "leaked after executing everything");
        }
    }

    #[test]
    fn concurrent_dispatch_and_release_end_at_zero() {
        let b = std::sync::Arc::new(ExecBacklog::default());
        let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(8);
        let exec = {
            let b = b.clone();
            std::thread::spawn(move || {
                while let Ok(n) = rx.recv() {
                    b.release(n, nop);
                }
            })
        };
        for h in 1..=5_000u64 {
            // Registered BEFORE the send, its count riding the block.
            let n = b.register(h, || (h % 17) as usize, nop);
            tx.send(n).unwrap();
            b.sent(h, nop);
        }
        drop(tx);
        exec.join().unwrap();
        assert_eq!(b.outstanding(), 0);
        assert_eq!(b.pending_heights(), 0);
    }

    /// A close racing a live dispatcher + exec thread: the total ends at 0
    /// and stays there.
    #[test]
    fn concurrent_close_never_leaks() {
        for _ in 0..50 {
            let b = std::sync::Arc::new(ExecBacklog::default());
            let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(4);
            let exec = {
                let b = b.clone();
                std::thread::spawn(move || {
                    let mut seen = 0;
                    while let Ok(n) = rx.recv() {
                        b.release(n, nop);
                        seen += 1;
                        if seen == 20 {
                            b.close(nop); // fail-stop; rx still alive
                        }
                    }
                })
            };
            for h in 1..=200u64 {
                let n = b.register(h, || 3, nop);
                if tx.send(n).is_err() {
                    break;
                }
                b.sent(h, nop);
            }
            drop(tx);
            exec.join().unwrap();
            assert_eq!(b.outstanding(), 0);
        }
    }
}
