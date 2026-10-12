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
//! Not the global rayon pool (ingress on it starved consensus work, s352), and
//! nothing else runs on it. Thread names `torus-exec-pool-<i>` are not in the
//! node's fail-stop list, as the unnamed scoped threads were not.

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
