//! P2-3: the execution path's persistent worker pool (plan
//! `item6-phase2-impl.md` P2-3, option A: a dedicated rayon pool).
//!
//! Matching (`MarketWorkerPool::match_parallel_capped_with`) and settle pass A
//! (`settle_market_results_parallel`) used to open a `std::thread::scope` and
//! spawn one thread per chunk on every native block (~41 spawns per block,
//! results doc section 44). They now run the same chunks as jobs on this pool,
//! built once per process. Scheduling only: every site keeps its own cap and
//! its own deterministic chunking, and results come back by job index.
//!
//! 8k L2.1 (`TORUS_SETTLE_STREAMED=1`): [`run_streamed`] runs one call's
//! per-market jobs on this pool while the calling exec thread takes their
//! results in market order (settle pass B overlaps the pool).
//!
//! Not the global rayon pool (ingress on it starved consensus work, s352), and
//! nothing else runs on it. Thread names `torus-exec-pool-<i>` are not in the
//! node's fail-stop list, as the unnamed scoped threads were not.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Size: the largest of the sites' caps at first use (`TORUS_MATCH_WORKERS`,
/// `TORUS_SETTLE_WORKERS`, host parallelism), so no site loses parallelism.
/// A later call with a larger cap queues its extra jobs (same results).
fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let host = std::thread::available_parallelism().map_or(1, |n| n.get());
        let size = host
            .max(crate::market_workers::MarketWorkerPool::resolve_worker_cap_named(None))
            .max(
                crate::market_workers::MarketWorkerPool::resolve_worker_cap_named(Some(
                    "TORUS_SETTLE_WORKERS",
                )),
            );
        rayon::ThreadPoolBuilder::new()
            .num_threads(size)
            .thread_name(|i| format!("torus-exec-pool-{i}"))
            .build()
            .expect("P2-3: build the exec worker pool")
    })
}

/// Run `jobs` on the pool and wait for all of them. Returns, in job order,
/// each job's value or its panic payload (what `JoinHandle::join` returned
/// for a scoped thread): a panicking job never unwinds into the caller or
/// the pool, and every job has ended when this returns.
pub(crate) fn run_jobs<T, F>(jobs: Vec<F>) -> Vec<std::thread::Result<T>>
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    let mut out: Vec<Option<std::thread::Result<T>>> = (0..jobs.len()).map(|_| None).collect();
    pool().scope(|s| {
        for (job, slot) in jobs.into_iter().zip(out.iter_mut()) {
            s.spawn(move |_| {
                *slot = Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)));
            });
        }
    });
    out.into_iter()
        .map(|r| r.expect("P2-3: the pool scope joined every job"))
        .collect()
}

/// 8k L2.1: [`StreamRx`]'s "no wait announced" value of `waiting`.
const NO_WAIT: u64 = u64::MAX;

/// 8k L2.1: a wait announcement, `need` (the item the caller takes next)
/// and `target` (the last item of the batch it waits for).
fn pack_wait(need: usize, target: usize) -> u64 {
    ((need as u64) << 32) | target as u64
}

fn unpack_wait(w: u64) -> (usize, usize) {
    ((w >> 32) as usize, (w & 0xFFFF_FFFF) as usize)
}

/// 8k L2.1 wake batch: at most this many items per wake-up of the caller.
/// Bench 9b (perf doc section 114) put a park / unpark per market on the
/// exec thread (+4.4% exec-thread on-CPU per 1k fills on the deep book):
/// the exec thread's pass B outruns the pool, so it parked on nearly every
/// market. Waiting for a batch of `WAKE_BATCH_MAX` items cuts the parks
/// about that many times; the cost is a tail of at most `batch - 1`
/// markets' pass B after the last job ends.
const WAKE_BATCH_MAX: usize = 8;

/// 8k L2.1: calls with fewer items than `WAKE_BATCH_DIV` times the batch
/// get a smaller batch (`n / WAKE_BATCH_DIV`, at least 1), so a call with
/// few markets keeps its overlap (batch 1: a wake per item, as before).
const WAKE_BATCH_DIV: usize = 8;

thread_local! {
    /// 8k L2.1 test seam: [`with_wake_batch`]'s per-thread pin of the wake
    /// batch (`None`: [`wake_batch`]'s policy).
    static WAKE_BATCH_PIN: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    /// 8k L2.1: this thread's parks in [`StreamRx::take`] so far: (parks,
    /// nanoseconds parked).
    static STREAM_PARKS: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
}

/// 8k L2.1: the wake batch of a streamed call over `n` items: the pin if
/// set (at least 1), else `min(WAKE_BATCH_MAX, max(1, n / WAKE_BATCH_DIV))`.
/// Scheduling only: results and their order do not depend on it.
pub(crate) fn wake_batch(n: usize) -> usize {
    match WAKE_BATCH_PIN.with(std::cell::Cell::get) {
        Some(b) => b.max(1),
        None => (n / WAKE_BATCH_DIV).clamp(1, WAKE_BATCH_MAX),
    }
}

/// 8k L2.1 test seam: runs `f` with the wake batch pinned to `batch` on
/// this thread (`None`: the policy); the previous pin comes back when `f`
/// returns or unwinds.
pub(crate) fn with_wake_batch<R>(batch: Option<usize>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WAKE_BATCH_PIN.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(WAKE_BATCH_PIN.with(|c| c.replace(batch)));
    f()
}

/// 8k L2.1: (parks, nanoseconds parked) of this thread's
/// [`StreamRx::take`] calls so far.
pub(crate) fn stream_parks_on_this_thread() -> (u64, u64) {
    STREAM_PARKS.with(std::cell::Cell::get)
}

/// 8k L2.1 (`TORUS_SETTLE_STREAMED`): the results of [`run_streamed`]'s
/// items, taken by the calling thread in item order while the pool still
/// works on later items.
pub(crate) struct StreamRx<'s, R> {
    slots: &'s [StreamSlot<R>],
    /// The caller's wait: [`pack_wait`]`(need, target)`, or [`NO_WAIT`].
    waiting: &'s AtomicU64,
    /// Items finished so far.
    done: &'s AtomicUsize,
    /// Wake batch (>= 1): see [`wake_batch`].
    batch: usize,
}

/// One item's result: `ready` is set (SeqCst) after the value is stored.
struct StreamSlot<R> {
    value: std::sync::Mutex<Option<std::thread::Result<R>>>,
    ready: AtomicBool,
}

impl<R> StreamRx<'_, R> {
    /// Item `i`'s result (its value, or its panic payload), blocking until
    /// its job has stored it. Each item can be taken once.
    ///
    /// When item `i` is not ready, the caller parks until item `i` AND item
    /// `target = min(i + batch - 1, last)` are both ready, so one wake-up
    /// usually hands it a run of about `batch` ready items (taken without
    /// parking) instead of one item per wake-up. Results and their order are
    /// the same for every batch; only when the caller wakes changes.
    pub(crate) fn take(&self, i: usize) -> std::thread::Result<R> {
        if let Some(r) = self.try_take(i) {
            return r;
        }
        let target = i.saturating_add(self.batch - 1).min(self.slots.len() - 1);
        let wait = pack_wait(i, target);
        loop {
            // Announce the wait, then re-check: a worker stores `ready`
            // before it reads `waiting` (all SeqCst), so the worker that
            // finishes the later of `i` and `target` either sees this wait
            // (and both items ready) and unparks, or this re-check sees both
            // ready.
            self.waiting.store(wait, Ordering::SeqCst);
            if self.batch_ready(i, target) {
                self.waiting.store(NO_WAIT, Ordering::SeqCst);
                break;
            }
            let parked = std::time::Instant::now();
            std::thread::park();
            let ns = u64::try_from(parked.elapsed().as_nanos()).unwrap_or(u64::MAX);
            STREAM_PARKS.with(|c| {
                let (n, t) = c.get();
                c.set((n + 1, t.saturating_add(ns)));
            });
            self.waiting.store(NO_WAIT, Ordering::SeqCst);
            // Spurious wake-ups (`park` may return early; a stale unpark of
            // an earlier wait) re-announce and park again.
            if self.batch_ready(i, target) {
                break;
            }
        }
        self.try_take(i)
            .expect("L2.1: item i is ready after the wait")
    }

    fn batch_ready(&self, i: usize, target: usize) -> bool {
        self.slots[i].ready.load(Ordering::SeqCst)
            && self.slots[target].ready.load(Ordering::SeqCst)
    }

    fn try_take(&self, i: usize) -> Option<std::thread::Result<R>> {
        let slot = &self.slots[i];
        if !slot.ready.load(Ordering::SeqCst) {
            return None;
        }
        Some(
            slot.value
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .expect("L2.1: each streamed item is taken once"),
        )
    }

    /// True once every item's job has stored its result (or is about to
    /// mark it ready: true after the last item was taken).
    pub(crate) fn all_done(&self) -> bool {
        self.done.load(Ordering::SeqCst) == self.slots.len()
    }
}

/// 8k L2.1: run `work(i, item)` for every item on at most `jobs` pool jobs
/// while `body` runs on the CALLING thread with a [`StreamRx`] that hands
/// out each item's result in item order as soon as it is ready. Jobs take
/// items in index order from a shared cursor (each job loops until the items
/// run out), so items finish roughly in index order. Scheduling only: item
/// `i`'s result is `work(i, item_i)` whichever job ran it, and a panicking
/// item is that item's `Err` (payload kept; never unwinds into the pool or
/// the caller, and the other items still run). Returns when `body` has
/// returned AND every job has ended (rayon `in_place_scope`), so items may
/// borrow the caller's data. `batch` (>= 1; see [`wake_batch`]): a caller
/// that has to wait is woken once per run of about `batch` ready items.
/// `jitter` (test seam, 0 = off) sleeps each item a pseudo-random 0-255 us
/// before its work, to vary completion orders.
pub(crate) fn run_streamed<I, R, B>(
    items: Vec<I>,
    jobs: usize,
    batch: usize,
    jitter: u64,
    work: &(dyn Fn(usize, I) -> R + Sync),
    body: impl FnOnce(&StreamRx<'_, R>) -> B,
) -> B
where
    I: Send,
    R: Send,
{
    let n = items.len();
    assert!(
        n < u32::MAX as usize,
        "L2.1: item indices fit the wait word"
    );
    let inputs: Vec<std::sync::Mutex<Option<I>>> = items
        .into_iter()
        .map(|i| std::sync::Mutex::new(Some(i)))
        .collect();
    let slots: Vec<StreamSlot<R>> = (0..n)
        .map(|_| StreamSlot {
            value: std::sync::Mutex::new(None),
            ready: AtomicBool::new(false),
        })
        .collect();
    let cursor = AtomicUsize::new(0);
    let waiting = AtomicU64::new(NO_WAIT);
    let done = AtomicUsize::new(0);
    let caller = std::thread::current();
    let rx = StreamRx {
        slots: &slots,
        waiting: &waiting,
        done: &done,
        batch: batch.max(1),
    };
    let worker = || loop {
        let i = cursor.fetch_add(1, Ordering::Relaxed);
        if i >= n {
            return;
        }
        let item = inputs[i]
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("L2.1: each streamed item is taken once");
        if jitter != 0 {
            std::thread::sleep(std::time::Duration::from_micros(jitter_us(jitter, i)));
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(i, item)));
        *slots[i].value.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        // `done` first: once the caller has taken every item, `all_done`
        // holds (was: a taken item could still be uncounted, flaking
        // `run_streamed_hands_out_results_in_item_order`). `all_done` may
        // run ahead of `ready` by an item: it only labels a pass B early.
        done.fetch_add(1, Ordering::SeqCst);
        slots[i].ready.store(true, Ordering::SeqCst);
        // Wake the caller only when this item completes its announced wait
        // (the later of `need` and `target` to finish), not on every item.
        let w = waiting.load(Ordering::SeqCst);
        if w != NO_WAIT {
            let (need, target) = unpack_wait(w);
            if (i == need || i == target)
                && slots[need].ready.load(Ordering::SeqCst)
                && slots[target].ready.load(Ordering::SeqCst)
            {
                caller.unpark();
            }
        }
    };
    let worker = &worker;
    pool().in_place_scope(|s| {
        for _ in 0..jobs.max(1).min(n) {
            s.spawn(move |_| worker());
        }
        body(&rx)
    })
}

/// 8k L2.1 test seam: item `i`'s pseudo-random sleep (0-255 us) under
/// `seed` (splitmix64).
pub(crate) fn jitter_us(seed: u64, i: usize) -> u64 {
    let mut z = seed.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) & 0xFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn job_thread() -> (std::thread::ThreadId, String) {
        let t = std::thread::current();
        (t.id(), t.name().unwrap_or("").to_string())
    }

    /// Jobs run on the pool's named threads, results come back in job
    /// order, and a second call reuses the same threads (no new spawns).
    #[test]
    fn jobs_run_in_order_on_reused_pool_threads() {
        let size = pool().current_num_threads();
        let mut seen: HashSet<std::thread::ThreadId> = HashSet::new();
        for round in 0..4u64 {
            let jobs: Vec<_> = (0..64u64)
                .map(|i| {
                    move || {
                        std::thread::sleep(std::time::Duration::from_micros(200));
                        (round * 1000 + i, job_thread())
                    }
                })
                .collect();
            let out = run_jobs(jobs);
            assert_eq!(out.len(), 64);
            for (i, r) in out.into_iter().enumerate() {
                let (v, (id, name)) = r.expect("no panic");
                assert_eq!(v, round * 1000 + i as u64, "job order");
                assert!(name.starts_with("torus-exec-pool-"), "{name}");
                assert_ne!(id, std::thread::current().id());
                seen.insert(id);
            }
        }
        assert!(
            seen.len() <= size,
            "{} distinct threads over 4 rounds, pool of {size}",
            seen.len()
        );
    }

    /// A panicking job is that job's `Err` (payload kept); the others
    /// finish, the caller does not unwind and the pool keeps working.
    #[test]
    fn a_panicking_job_is_its_own_err() {
        let jobs: Vec<Box<dyn FnOnce() -> u32 + Send>> = vec![
            Box::new(|| 1),
            Box::new(|| panic!("planted exec pool job panic (test)")),
            Box::new(|| 3),
        ];
        let out = run_jobs(jobs);
        assert_eq!(*out[0].as_ref().unwrap(), 1);
        let payload = out[1].as_ref().expect_err("job 1 panicked");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"planted exec pool job panic (test)")
        );
        assert_eq!(*out[2].as_ref().unwrap(), 3);
        let again = run_jobs(vec![|| 7u32]);
        assert_eq!(*again[0].as_ref().unwrap(), 7);
    }

    /// 8k L2.1: every item's result comes back by index, in order, whatever
    /// the job count, the wake batch and the completion order (jitter); a
    /// panicking item is its own `Err` and the others still run; items
    /// borrow caller data; the caller sees `all_done` once everything
    /// finished.
    #[test]
    fn run_streamed_hands_out_results_in_item_order() {
        let base: Vec<u64> = (0..200).collect();
        for (jobs, batch) in [1usize, 2, 7, 64]
            .into_iter()
            .flat_map(|j| [1usize, 3, 8, 500].map(|b| (j, b)))
        {
            for jitter in [0u64, 0xC0FFEE, 0x5EED] {
                let work = |i: usize, x: &u64| {
                    if i == 77 {
                        panic!("planted streamed item panic (test)");
                    }
                    (i as u64) * 1000 + *x
                };
                let items: Vec<&u64> = base.iter().collect();
                let (got, done_at_end) = run_streamed(items, jobs, batch, jitter, &work, |rx| {
                    let mut got = Vec::new();
                    for i in 0..200 {
                        got.push(rx.take(i).map_err(|p| {
                            p.downcast_ref::<&str>()
                                .map(|s| s.to_string())
                                .unwrap_or_default()
                        }));
                    }
                    (got, rx.all_done())
                });
                assert!(
                    done_at_end,
                    "jobs {jobs} batch {batch}: every item finished before the last take"
                );
                for (i, r) in got.into_iter().enumerate() {
                    if i == 77 {
                        assert_eq!(r, Err("planted streamed item panic (test)".to_string()));
                    } else {
                        assert_eq!(
                            r,
                            Ok(i as u64 * 1001),
                            "jobs {jobs} batch {batch} jitter {jitter:#x} item {i}"
                        );
                    }
                }
            }
        }
    }

    /// 8k L2.1: the caller works while the pool works: with the last item
    /// slow, the caller takes the first items before every item finished,
    /// and the call still returns only after every job ended.
    #[test]
    fn run_streamed_overlaps_the_caller_with_the_pool() {
        let finished = AtomicUsize::new(0);
        let work = |i: usize, _: ()| {
            if i == 15 {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            finished.fetch_add(1, Ordering::SeqCst);
            i
        };
        let early = run_streamed(vec![(); 16], 4, 1, 0, &work, |rx| {
            let first = rx.take(0).unwrap();
            assert_eq!(first, 0);
            // Item 15 sleeps 50 ms: not everything is done yet.
            !rx.all_done()
        });
        assert!(early, "the caller ran while item 15 was still working");
        assert_eq!(
            finished.load(Ordering::SeqCst),
            16,
            "returns after every job ended"
        );
        // An empty item list runs the body with nothing to take.
        let work = |_: usize, x: u8| x;
        assert!(run_streamed(Vec::<u8>::new(), 4, 1, 0, &work, |rx| rx.all_done()));
    }

    /// 8k L2.1 wake batching: a caller that outruns the pool parks at most
    /// about once per `batch` items (same results), and the policy scales
    /// the batch with the item count.
    #[test]
    fn run_streamed_batches_the_callers_wake_ups() {
        let parks = |batch: usize| {
            let work = |i: usize, _: ()| {
                std::thread::sleep(std::time::Duration::from_micros(200));
                i
            };
            let before = stream_parks_on_this_thread().0;
            let got = run_streamed(vec![(); 64], 1, batch, 0, &work, |rx| {
                (0..64).map(|i| rx.take(i).unwrap()).collect::<Vec<_>>()
            });
            assert_eq!(
                got,
                (0..64).collect::<Vec<_>>(),
                "batch {batch}: item order"
            );
            stream_parks_on_this_thread().0 - before
        };
        // Upper bounds only (how often the caller has to wait at all depends
        // on scheduling). With one job, items finish in order, so a real
        // wake-up of a batch-`b` wait finds the whole run `need..=target`
        // ready and the caller takes it without parking: at most
        // `ceil(64 / b)` waits. Each wait parks at most twice (a leftover
        // unpark token from a wait the re-check satisfied makes one `park`
        // return at once).
        for batch in [1usize, 8, 64] {
            let bound = 2 * 64usize.div_ceil(batch) as u64;
            let got = parks(batch);
            assert!(got <= bound, "batch {batch}: {got} parks > bound {bound}");
        }
        assert_eq!(wake_batch(0), 1);
        assert_eq!(wake_batch(4), 1);
        assert_eq!(wake_batch(16), 2);
        assert_eq!(wake_batch(300), WAKE_BATCH_MAX);
        assert_eq!(with_wake_batch(Some(5), || wake_batch(300)), 5);
        assert_eq!(with_wake_batch(Some(0), || wake_batch(300)), 1);
        assert_eq!(wake_batch(300), WAKE_BATCH_MAX, "pin restored");
    }

    /// Jobs may borrow the caller's data (as scoped threads could).
    #[test]
    fn jobs_borrow_caller_data() {
        let data: Vec<u64> = (0..100).collect();
        let jobs: Vec<_> = data
            .chunks(7)
            .map(|c| move || c.iter().sum::<u64>())
            .collect();
        let total: u64 = run_jobs(jobs).into_iter().map(|r| r.unwrap()).sum();
        assert_eq!(total, data.iter().sum::<u64>());
    }
}
