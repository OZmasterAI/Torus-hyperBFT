//! Per-market parallel order matching pool.
//!
//! Dispatches PlaceOrder batches to the persistent exec pool (`exec_pool`, P2-3) for concurrent matching.
//! Each thread temporarily owns its market's OrderBook for cache-local access.

use alloy_primitives::map::HashMap;
use alloy_primitives::Address;
use torus_core::order_book::{MakerAccountSource, OrderBook, PlaceResult, TakerMarginLimit};
use torus_types::{MarketId, OrderId, PlaceOrderParams};

/// A single order to be matched by a worker thread.
///
/// C2: `params` is a BORROW of the caller's committed action data (the
/// `(Address, NativeAction)` slice owned by the block executor) — the matching
/// pipeline no longer deep-clones `PlaceOrderParams` per stage. The one copy
/// left is at the book boundary (`OrderBook::place_order` takes ownership).
pub struct MatchRequest<'a> {
    pub sender: Address,
    pub params: &'a PlaceOrderParams,
    pub order_id: OrderId,
    /// s515 review 4: the taker's match-time margin limit (`None` = unchecked).
    pub margin: Option<TakerMarginLimit>,
}

/// Result of matching a single order.
pub struct MatchResult {
    pub sender: Address,
    pub order_id: OrderId,
    pub result: PlaceResult,
}

/// Aggregated results from one market's worker thread.
pub struct MarketBatchResult {
    pub market_id: MarketId,
    pub book: OrderBook,
    pub results: Vec<MatchResult>,
    pub next_order_id: OrderId,
}

/// T1.5: a market worker panicked mid-matching. The panicking worker consumed
/// its market's `OrderBook`, so the block's post-state is unreconstructable —
/// the caller MUST treat this as FATAL for the whole block (fail-stop), never
/// skip-and-continue.
#[derive(Debug)]
pub struct MarketWorkerPanic {
    pub market_id: MarketId,
    pub message: String,
}

/// Parallel matching dispatcher on the persistent exec pool (P2-3; was scoped threads).
///
/// Each chunk of markets runs as one job on a pool thread for the duration of
/// matching. The job owns the OrderBook during execution, ensuring cache-local access
/// to BTreeMap nodes without lock contention.
pub struct MarketWorkerPool;

impl MarketWorkerPool {
    /// Match orders across multiple markets in parallel.
    ///
    /// Takes ownership of each market's OrderBook, dispatches matching to a
    /// CAPPED set of exec pool jobs (P2-3), and returns updated books with match
    /// results. Return order is unspecified (the caller sorts by market_id).
    ///
    /// Worker cap: `TORUS_MATCH_WORKERS` (usize, clamped to >= 1) overrides;
    /// unset/unparseable falls back to [`std::thread::available_parallelism`]
    /// (1 if that is unavailable). Resolved per call — cheap and stateless.
    /// One-thread-per-market oversubscribes past the core count and thrashes
    /// (measured cliff at 2x cores), so markets are chunked across the cap.
    ///
    /// T1.5: a panic in any worker is CONTAINED (`catch_unwind`) and surfaced
    /// as a typed [`MarketWorkerPanic`] instead of unwinding through
    /// `thread::scope` and killing the execution thread (which left consensus
    /// zombie-advancing on a closed exec channel).
    pub fn match_parallel(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)>,
        timestamp: u64,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic> {
        Self::match_parallel_with(batches, timestamp, None, false)
    }

    /// F1 (s517 #4): [`Self::match_parallel`] with the source of makers'
    /// accounts (read-only, `Sync`; `None` = makers unchecked).
    /// perf/cloid-engine-cost fix 3: `cloids_checked` = the caller has
    /// applied the one-live-order-per-`(trader, cloid)` rule to every
    /// request across every book (the executor's batch pre-pass), so the
    /// books skip their own lookup
    /// ([`OrderBook::place_order_with_accounts_cloid_checked`]).
    pub fn match_parallel_with(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)>,
        timestamp: u64,
        makers: Option<&dyn MakerAccountSource>,
        cloids_checked: bool,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic> {
        Self::match_parallel_capped_with(
            batches,
            timestamp,
            Self::resolve_worker_cap(),
            makers,
            cloids_checked,
        )
    }

    /// [`Self::match_parallel`] with an explicit worker cap (>= 1 enforced).
    /// Split out so tests can force a small cap independent of the host's core
    /// count; the public entry point supplies the resolved cap.
    pub fn match_parallel_capped<'a>(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'a>>)>,
        timestamp: u64,
        max_workers: usize,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic> {
        Self::match_parallel_capped_with(batches, timestamp, max_workers, None, false)
    }

    /// F1 (s517 #4): [`Self::match_parallel_capped`] with the makers' source.
    pub fn match_parallel_capped_with<'a>(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'a>>)>,
        timestamp: u64,
        max_workers: usize,
        makers: Option<&dyn MakerAccountSource>,
        cloids_checked: bool,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic> {
        if batches.is_empty() {
            return Ok(Vec::new());
        }

        // Single market — skip thread spawn overhead.
        if batches.len() == 1 {
            let (market_id, (book, requests)) = batches.into_iter().next().unwrap();
            return Self::run_contained(market_id, || {
                Self::match_market(market_id, book, requests, timestamp, makers, cloids_checked)
            })
            .map(|r| vec![r]);
        }

        // Multiple markets — chunk them across at most `max_workers` pool
        // jobs. The borrowed `MatchRequest.params` refs outlive the scope
        // (they borrow from the caller's action slice), so pool jobs (P2-3) may
        // capture them freely; each chunk is moved into its job.
        let batches_vec: Vec<(MarketId, OrderBook, Vec<MatchRequest<'a>>)> = batches
            .into_iter()
            .map(|(id, (book, reqs))| (id, book, reqs))
            .collect();
        let n = batches_vec.len();

        // Never more workers than markets; at least one.
        let workers = max_workers.max(1).min(n);
        let counts: Vec<(MarketId, usize)> = batches_vec
            .iter()
            .map(|(id, _, reqs)| (*id, reqs.len()))
            .collect();
        let assignment = Self::assign_chunks(&counts, workers);

        let mut chunks: Vec<Vec<(MarketId, OrderBook, Vec<MatchRequest<'a>>)>> =
            (0..workers).map(|_| Vec::new()).collect();
        for (i, tuple) in batches_vec.into_iter().enumerate() {
            chunks[assignment[i]].push(tuple);
        }
        // Drop workers with no markets (possible when workers > distinct loads);
        // keeps `chunk[0]` valid as the join-failure representative below.
        chunks.retain(|c| !c.is_empty());

        // Copy-capture closure (`timestamp: u64` and, F1, the shared `Sync`
        // makers ref) so each worker gets its own copy — the actual
        // per-market matcher `process_chunk` runs.
        let match_one =
            move |market_id: MarketId, book: OrderBook, requests: Vec<MatchRequest<'a>>| {
                Self::match_market(market_id, book, requests, timestamp, makers, cloids_checked)
            };

        // P2-3: the chunks run as jobs on the persistent exec pool (no
        // per-block spawns); a job that dies outside the per-market
        // containment is reported as a scoped thread's join failure was.
        let reps: Vec<MarketId> = chunks.iter().map(|chunk| chunk[0].0).collect();
        let jobs: Vec<_> = chunks
            .into_iter()
            .map(|chunk| move || Self::process_chunk(chunk, &match_one))
            .collect();

        // Flatten per-job results in job order; the first `Err`
        // short-circuits (every job has already ended). Return order is
        // irrelevant (caller sorts by id).
        let mut out = Vec::with_capacity(n);
        for (rep, joined) in reps.into_iter().zip(crate::exec_pool::run_jobs(jobs)) {
            let chunk_results = joined.unwrap_or_else(|_| {
                vec![Err(MarketWorkerPanic {
                    market_id: rep,
                    message: "worker thread died outside contained matching".to_string(),
                })]
            });
            for r in chunk_results {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// TORUS_PARALLEL_MATCH_SETUP: [`Self::match_parallel_capped_with`] where
    /// each market's job first builds its requests: `setup(market_id, &mut
    /// book, input)` runs on the job, then the market is matched. `items` are
    /// `(market_id, book, setup input, LPT weight)`; the weight is the
    /// market's request count, so the chunk layout is the one
    /// `match_parallel_capped_with` would pick. A panic in `setup` is
    /// contained like a matching panic: that market's [`MarketWorkerPanic`]
    /// (the block's fatal path), never a skip or a retry.
    pub(crate) fn match_parallel_setup_with<'a, W: Send, S>(
        items: Vec<(MarketId, OrderBook, W, usize)>,
        timestamp: u64,
        max_workers: usize,
        makers: Option<&dyn MakerAccountSource>,
        cloids_checked: bool,
        setup: &S,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic>
    where
        S: Fn(MarketId, &mut OrderBook, W) -> Vec<MatchRequest<'a>> + Sync,
    {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        // One market's setup + matching, contained (the job's unit of work).
        let run_one = move |market_id: MarketId, book: OrderBook, w: W| {
            Self::run_contained(market_id, move || {
                let mut book = book;
                let requests = setup(market_id, &mut book, w);
                Self::match_market(market_id, book, requests, timestamp, makers, cloids_checked)
            })
        };

        // Single market — no job, as in `match_parallel_capped_with`.
        if items.len() == 1 {
            let (market_id, book, w, _) = items.into_iter().next().unwrap();
            return run_one(market_id, book, w).map(|r| vec![r]);
        }

        // Multiple markets — LPT-chunk them across at most `max_workers`
        // pool jobs, exactly as `match_parallel_capped_with` does.
        let n = items.len();
        let workers = max_workers.max(1).min(n);
        let weights: Vec<(MarketId, usize)> = items
            .iter()
            .map(|(id, _, _, weight)| (*id, *weight))
            .collect();
        let assignment = Self::assign_chunks(&weights, workers);
        let mut chunks: Vec<Vec<(MarketId, OrderBook, W)>> =
            (0..workers).map(|_| Vec::new()).collect();
        for (i, (id, book, w, _)) in items.into_iter().enumerate() {
            chunks[assignment[i]].push((id, book, w));
        }
        chunks.retain(|c| !c.is_empty());

        // Each job runs its markets in order and stops at its first `Err`
        // (`process_chunk` semantics: the block is fatal anyway).
        let reps: Vec<MarketId> = chunks.iter().map(|chunk| chunk[0].0).collect();
        let run_one = &run_one;
        let jobs: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                move || {
                    let mut out = Vec::with_capacity(chunk.len());
                    for (market_id, book, w) in chunk {
                        let r = run_one(market_id, book, w);
                        let stop = r.is_err();
                        out.push(r);
                        if stop {
                            break;
                        }
                    }
                    out
                }
            })
            .collect();

        let mut out = Vec::with_capacity(n);
        for (rep, joined) in reps.into_iter().zip(crate::exec_pool::run_jobs(jobs)) {
            let chunk_results = joined.unwrap_or_else(|_| {
                vec![Err(MarketWorkerPanic {
                    market_id: rep,
                    message: "worker thread died outside contained matching".to_string(),
                })]
            });
            for r in chunk_results {
                out.push(r?);
            }
        }
        Ok(out)
    }

    /// Resolve the worker cap: `TORUS_MATCH_WORKERS` (clamped to >= 1) if set and
    /// parseable, else the host parallelism, else 1.
    fn resolve_worker_cap() -> usize {
        Self::resolve_worker_cap_named(None)
    }

    /// Resolve a worker cap from `primary` (an env var name, e.g.
    /// `TORUS_SETTLE_WORKERS`) if set and parseable, else `TORUS_MATCH_WORKERS`,
    /// else the host parallelism, else 1. Always >= 1. Resolved per call —
    /// cheap and stateless, so a phase can be capped independently without a
    /// process-global toggle.
    pub(crate) fn resolve_worker_cap_named(primary: Option<&str>) -> usize {
        for var in primary
            .into_iter()
            .chain(std::iter::once("TORUS_MATCH_WORKERS"))
        {
            if let Ok(raw) = std::env::var(var) {
                if let Ok(n) = raw.parse::<usize>() {
                    return n.max(1);
                }
            }
        }
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    /// Pack `markets` (`(market_id, weight)` in caller order) into at most
    /// `max_workers` non-empty chunks of INPUT INDICES via [`Self::assign_chunks`].
    ///
    /// Each returned chunk lists its indices ascending, so when the caller's
    /// slice is market-id sorted a worker walks its markets in market-id order.
    /// Empty chunks are dropped (possible when `max_workers` exceeds the market
    /// count), and chunk order follows worker index — the layout is a pure
    /// function of the `(id, weight)` pairs and independent of input order.
    ///
    /// This is a SCHEDULING primitive: callers must reassemble results by input
    /// index so that the chunk layout cannot leak into observable output.
    pub(crate) fn chunk_indices(
        markets: &[(MarketId, usize)],
        max_workers: usize,
    ) -> Vec<Vec<usize>> {
        if markets.is_empty() {
            return Vec::new();
        }
        let workers = max_workers.max(1).min(markets.len());
        let assignment = Self::assign_chunks(markets, workers);
        let mut chunks: Vec<Vec<usize>> = vec![Vec::new(); workers];
        for (i, &w) in assignment.iter().enumerate() {
            chunks[w].push(i);
        }
        chunks.retain(|c| !c.is_empty());
        chunks
    }

    /// LPT (longest-processing-time) assignment of markets to `workers`.
    ///
    /// Markets are considered heaviest-first (request count desc, market_id asc
    /// as a deterministic tie-break) and each is placed on the currently
    /// least-loaded worker (lowest worker index breaks load ties). Returns, for
    /// each input index, its worker index in `[0, workers)`. Pure over the
    /// `(market_id, count)` pairs and independent of their input order, so chunk
    /// layout is reproducible.
    pub(crate) fn assign_chunks(markets: &[(MarketId, usize)], workers: usize) -> Vec<usize> {
        let workers = workers.max(1);
        let n = markets.len();

        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            markets[b]
                .1
                .cmp(&markets[a].1) // count descending
                .then_with(|| markets[a].0.cmp(&markets[b].0)) // market_id ascending
        });

        let mut worker_load = vec![0usize; workers];
        let mut assignment = vec![0usize; n];
        for &idx in &order {
            let mut best = 0usize;
            for w in 1..workers {
                if worker_load[w] < worker_load[best] {
                    best = w;
                }
            }
            assignment[idx] = best;
            worker_load[best] += markets[idx].1;
        }
        assignment
    }

    /// Run one worker's chunk of markets sequentially, each contained. Stops at
    /// the first `Err` (a market panic consumed its book; the whole block is
    /// fatal anyway, so the rest of the chunk is not processed). `match_one` is
    /// the per-market matcher (production: [`Self::match_market`]); it is a
    /// parameter so the panic-containment path can be exercised with an injected
    /// panicking matcher.
    fn process_chunk<'a, F>(
        chunk: Vec<(MarketId, OrderBook, Vec<MatchRequest<'a>>)>,
        match_one: &F,
    ) -> Vec<Result<MarketBatchResult, MarketWorkerPanic>>
    where
        F: Fn(MarketId, OrderBook, Vec<MatchRequest<'a>>) -> MarketBatchResult,
    {
        let mut out = Vec::with_capacity(chunk.len());
        for (market_id, book, requests) in chunk {
            let r =
                Self::run_contained(market_id, || match_one(market_id, book, requests));
            let stop = r.is_err();
            out.push(r);
            if stop {
                break;
            }
        }
        out
    }

    /// Run one market's matching with panic containment: a panic becomes a
    /// typed [`MarketWorkerPanic`] carrying the market id and panic payload,
    /// so the caller can fail-stop loudly instead of dying silently.
    /// Generic over the result so the executor's parallel cancel-all book
    /// pass (`TORUS_PARALLEL_CANCEL_ALL`) contains its per-market jobs the
    /// same way.
    pub(crate) fn run_contained<R>(
        market_id: MarketId,
        f: impl FnOnce() -> R,
    ) -> Result<R, MarketWorkerPanic> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|payload| {
            let message = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "non-string panic payload".to_string()
            };
            MarketWorkerPanic { market_id, message }
        })
    }

    /// Match a single market's orders on the worker thread.
    ///
    /// Orders are processed in submission order (preserving within-market
    /// determinism). Each order's global ID is pre-assigned by the caller.
    fn match_market(
        market_id: MarketId,
        mut book: OrderBook,
        requests: Vec<MatchRequest<'_>>,
        timestamp: u64,
        makers: Option<&dyn MakerAccountSource>,
        cloids_checked: bool,
    ) -> MarketBatchResult {
        let mut results = Vec::with_capacity(requests.len());

        for req in requests {
            book.set_next_order_id(req.order_id);
            // C2: THE one params copy in the pipeline — the book takes ownership.
            let place_result = if cloids_checked {
                book.place_order_with_accounts_cloid_checked(
                    req.params.clone(),
                    req.sender,
                    timestamp,
                    req.margin.as_ref(),
                    makers,
                )
            } else {
                book.place_order_with_accounts(
                    req.params.clone(),
                    req.sender,
                    timestamp,
                    req.margin.as_ref(),
                    makers,
                )
            };

            results.push(MatchResult {
                sender: req.sender,
                order_id: req.order_id,
                result: place_result,
            });
        }

        // perf/cloid-engine-cost fix 2: look this book's logged cloid keys
        // up here, in parallel, so the exec thread's drain does not.
        book.resolve_cloid_log();
        let next_order_id = book.next_order_id();
        MarketBatchResult {
            market_id,
            book,
            results,
            next_order_id,
        }
    }
}

#[cfg(test)]
mod worker_panic_containment_tests {
    use super::*;

    /// T1.5 RED-first: on the pre-fix code a worker panic unwound through
    /// `thread::scope` and `join().expect("market worker panicked")` killed
    /// the calling (execution) thread — silent thread death, consensus kept
    /// finalizing. Contained code must surface a typed `MarketWorkerPanic`
    /// carrying the market id and the original panic payload.
    #[test]
    fn worker_panic_is_contained_as_typed_error() {
        let err = match MarketWorkerPool::run_contained(7, || panic!("boom in market 7")) {
            Err(e) => e,
            Ok(_) => panic!("a panicking worker must surface as MarketWorkerPanic"),
        };
        assert_eq!(err.market_id, 7);
        assert!(
            err.message.contains("boom in market 7"),
            "panic payload must be preserved, got: {}",
            err.message
        );
    }

    /// Healthy input keeps working through the new `Result` surface.
    #[test]
    fn empty_batches_return_ok_empty() {
        let batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> = HashMap::default();
        let results =
            MarketWorkerPool::match_parallel(batches, 1000).expect("no worker, no panic");
        assert!(results.is_empty());
    }

    /// perf/cloid-engine-cost fix 3: only the executor's batch path (which
    /// checked every cloid first) tells the books to skip their cloid
    /// lookup; the public entry points without the flag keep it, so two
    /// requests of one trader's cloid in one book still reject the second,
    /// one market or several.
    #[test]
    fn without_the_executor_check_the_book_still_rejects_a_duplicate_cloid() {
        use torus_state::action_status::FailureReason;
        use torus_types::{FixedPoint, OrderType, TimeInForce};
        let fp = |v: i64| FixedPoint::from_raw(v as i128 * FixedPoint::SCALE);
        let p = |m: MarketId, px: i64| PlaceOrderParams {
            market_id: m,
            is_buy: true,
            price: fp(px),
            quantity: fp(1),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: Some(7),
        };
        let trader = Address::new([1; 20]);
        let params: Vec<PlaceOrderParams> = vec![p(1, 50), p(1, 49), p(2, 50), p(2, 49)];
        for markets in [1u64, 2] {
            let mut batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> =
                HashMap::default();
            for m in 1..=markets {
                let reqs = params
                    .iter()
                    .filter(|q| q.market_id == m)
                    .enumerate()
                    .map(|(k, q)| MatchRequest {
                        sender: trader,
                        params: q,
                        order_id: 10 * m as u128 + k as u128,
                        margin: None,
                    })
                    .collect();
                batches.insert(m, (OrderBook::new(m, fp(1), fp(1)), reqs));
            }
            let results = MarketWorkerPool::match_parallel(batches, 1000).expect("no panic");
            assert_eq!(results.len(), markets as usize);
            for r in results {
                let rejects: Vec<_> = r.results.iter().map(|x| x.result.reject).collect();
                assert_eq!(
                    rejects,
                    vec![None, Some(FailureReason::DuplicateCloid)],
                    "market {}",
                    r.market_id
                );
                assert_eq!(r.book.cloid_count(), 1);
            }
        }
    }
}

#[cfg(test)]
mod capped_chunking_tests {
    use super::*;
    use torus_types::FixedPoint;

    fn tick_lot() -> FixedPoint {
        FixedPoint::from_raw(FixedPoint::SCALE)
    }

    /// LPT: a lone heavy market must not share its worker with the light ones.
    #[test]
    fn lpt_skewed_isolates_the_big_market() {
        let markets = [(1u64, 100), (2, 1), (3, 1), (4, 1), (5, 1)];
        let a = MarketWorkerPool::assign_chunks(&markets, 2);
        let big_w = a[0];
        assert!(
            (1..markets.len()).all(|i| a[i] != big_w),
            "the four small markets must not share the big market's worker: {a:?}"
        );
        assert!(
            a[1] == a[2] && a[2] == a[3] && a[3] == a[4],
            "the four small markets must all land on one worker: {a:?}"
        );
    }

    /// Equal loads → deterministic tie-break (count desc, then market_id asc,
    /// then lowest worker index): sorted ids 1,2,3,4 fill workers 0,1,0,1.
    #[test]
    fn lpt_tie_break_is_deterministic() {
        let markets = [(3u64, 5), (1, 5), (2, 5), (4, 5)];
        assert_eq!(
            MarketWorkerPool::assign_chunks(&markets, 2),
            MarketWorkerPool::assign_chunks(&markets, 2),
        );
        // input order [id3, id1, id2, id4] → id1,id2 sorted first; assignment
        // maps back to input positions.
        assert_eq!(MarketWorkerPool::assign_chunks(&markets, 2), vec![0, 0, 1, 1]);
    }

    /// More workers than markets → each market gets its own worker.
    #[test]
    fn lpt_more_workers_than_markets_each_alone() {
        let markets = [(1u64, 3), (2, 7), (3, 1)];
        let a = MarketWorkerPool::assign_chunks(&markets, 8);
        let mut w = a.clone();
        w.sort_unstable();
        w.dedup();
        assert_eq!(w.len(), 3, "each market must get a distinct worker: {a:?}");
    }

    /// One worker → every market on it.
    #[test]
    fn lpt_single_worker_all_together() {
        let markets = [(1u64, 3), (2, 7), (3, 1)];
        assert_eq!(MarketWorkerPool::assign_chunks(&markets, 1), vec![0, 0, 0]);
    }

    /// Panic containment through the per-worker chunk routine: a market that
    /// panics mid-matching surfaces as a typed `MarketWorkerPanic` carrying its
    /// id, and the rest of the chunk after it is not processed (books consumed —
    /// the whole block is fatal). Driven through the real `process_chunk`/
    /// `run_contained` path with an injected panicking matcher, since no normal
    /// `OrderBook` input reaches a genuine panic.
    #[test]
    fn chunk_panic_surfaces_typed_error_and_short_circuits() {
        let chunk: Vec<(MarketId, OrderBook, Vec<MatchRequest<'_>>)> = vec![
            (1, OrderBook::new(1, tick_lot(), tick_lot()), Vec::new()),
            (2, OrderBook::new(2, tick_lot(), tick_lot()), Vec::new()),
            (3, OrderBook::new(3, tick_lot(), tick_lot()), Vec::new()),
        ];

        let out = MarketWorkerPool::process_chunk(chunk, &|market_id, book, _reqs| {
            if market_id == 2 {
                panic!("boom in market {market_id}");
            }
            let next_order_id = book.next_order_id();
            MarketBatchResult {
                market_id,
                book,
                results: Vec::new(),
                next_order_id,
            }
        });

        assert_eq!(out.len(), 2, "third market must be skipped after the panic");
        assert!(out[0].is_ok(), "market 1 completed before the panic");
        let err = out[1]
            .as_ref()
            .err()
            .expect("market 2 must surface a MarketWorkerPanic");
        assert_eq!(err.market_id, 2);
        assert!(
            err.message.contains("boom in market 2"),
            "panic payload must be preserved, got: {}",
            err.message
        );
    }
}

#[cfg(test)]
mod chunk_indices_tests {
    use super::*;

    /// Every input index lands in exactly one chunk, and each chunk lists its
    /// indices ascending (so a worker walks its markets in market-id order
    /// when the input slice is market-id sorted).
    #[test]
    fn covers_every_index_once_ascending() {
        let markets: Vec<(MarketId, usize)> = (0..17u64)
            .map(|i| (i as MarketId, (i as usize * 7) % 13))
            .collect();
        for workers in [1usize, 2, 3, 5, 8, 17, 64] {
            let chunks = MarketWorkerPool::chunk_indices(&markets, workers);
            assert!(
                chunks.iter().all(|c| !c.is_empty()),
                "workers={workers}: empty chunk emitted"
            );
            assert!(chunks.len() <= workers.min(markets.len()));
            for c in &chunks {
                let mut sorted = c.clone();
                sorted.sort_unstable();
                assert_eq!(&sorted, c, "workers={workers}: chunk not ascending");
            }
            let mut all: Vec<usize> = chunks.iter().flatten().copied().collect();
            all.sort_unstable();
            assert_eq!(
                all,
                (0..markets.len()).collect::<Vec<_>>(),
                "workers={workers}: index coverage"
            );
        }
    }

    /// Cap 1 (and 0, clamped) means one chunk holding everything in input order.
    #[test]
    fn cap_one_is_one_sequential_chunk() {
        let markets: Vec<(MarketId, usize)> = (0..6u64).map(|i| (i as MarketId, 3)).collect();
        for workers in [0usize, 1] {
            let chunks = MarketWorkerPool::chunk_indices(&markets, workers);
            assert_eq!(chunks, vec![vec![0, 1, 2, 3, 4, 5]], "workers={workers}");
        }
    }

    /// Pure and input-order independent: the same (id, weight) multiset yields
    /// the same partition of market ids regardless of slice order.
    #[test]
    fn partition_is_input_order_independent() {
        let a: Vec<(MarketId, usize)> = vec![(1, 10), (2, 1), (3, 7), (4, 7), (5, 2)];
        let b: Vec<(MarketId, usize)> = vec![(4, 7), (1, 10), (5, 2), (3, 7), (2, 1)];
        let part = |m: &[(MarketId, usize)]| -> Vec<Vec<MarketId>> {
            let mut p: Vec<Vec<MarketId>> = MarketWorkerPool::chunk_indices(m, 3)
                .into_iter()
                .map(|c| {
                    let mut v: Vec<MarketId> = c.into_iter().map(|i| m[i].0).collect();
                    v.sort_unstable();
                    v
                })
                .collect();
            p.sort();
            p
        };
        assert_eq!(part(&a), part(&b));
    }

    /// LPT actually balances: one fat market must not be joined by the next
    /// fattest while a free worker exists.
    #[test]
    fn lpt_balances_skewed_weights() {
        let markets: Vec<(MarketId, usize)> = vec![(1, 100), (2, 90), (3, 5), (4, 5)];
        let chunks = MarketWorkerPool::chunk_indices(&markets, 2);
        assert_eq!(chunks.len(), 2);
        let loads: Vec<usize> = chunks
            .iter()
            .map(|c| c.iter().map(|&i| markets[i].1).sum())
            .collect();
        assert!(
            loads.iter().max().unwrap() - loads.iter().min().unwrap() <= 10,
            "unbalanced LPT chunks: {loads:?}"
        );
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(MarketWorkerPool::chunk_indices(&[], 4).is_empty());
    }

    /// The settle path resolves its cap from `TORUS_SETTLE_WORKERS` first and
    /// falls back to the matching cap; both clamp to >= 1.
    #[test]
    fn named_cap_falls_back_and_clamps() {
        assert!(MarketWorkerPool::resolve_worker_cap_named(None) >= 1);
        assert!(MarketWorkerPool::resolve_worker_cap_named(Some("TORUS_NO_SUCH_VAR_XYZ")) >= 1);
    }
}

#[cfg(test)]
mod setup_in_jobs_tests {
    //! TORUS_PARALLEL_MATCH_SETUP: [`MarketWorkerPool::match_parallel_setup_with`]
    //! builds each market's match inputs inside its pool match job. Whatever
    //! the cap, it returns what matching the same pre-built batches returns,
    //! and a setup panic is contained as that market's typed
    //! [`MarketWorkerPanic`] (the block's fatal path), never skipped.
    use super::*;
    use torus_core::order_book::ReduceOnlyPositions;
    use torus_types::{FixedPoint, OrderType, TimeInForce};

    const TS: u64 = 1000;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn trader(n: u8) -> Address {
        Address::new([n; 20])
    }

    fn limit(
        m: MarketId,
        is_buy: bool,
        px: i64,
        qty: i64,
        tif: TimeInForce,
        reduce_only: bool,
    ) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: m,
            is_buy,
            price: fp(px),
            quantity: fp(qty),
            order_type: OrderType::Limit,
            time_in_force: tif,
            reduce_only,
            client_order_id: None,
        }
    }

    /// One market's match input: its orders in submission order and the
    /// reduce-only positions its book polices (what the executor's setup
    /// installs on the book before matching).
    struct MarketInput {
        id: MarketId,
        orders: Vec<(Address, PlaceOrderParams, OrderId)>,
        ro: Vec<(Address, i64)>,
    }

    /// `markets` markets with uneven loads (LPT has real choices to make):
    /// crossing buys and sells, IOC / FOK takers, and a reduce-only sell of
    /// trader 9 (long 2: clamped to 2 only if the setup installed it).
    fn inputs(markets: u64) -> Vec<MarketInput> {
        (1..=markets)
            .map(|m| {
                let n = 2 + (m * 3 % 7);
                let mut orders = Vec::new();
                for k in 0..n {
                    let sender = trader(20 + (k % 5) as u8);
                    let tif = match k % 4 {
                        1 => TimeInForce::IOC,
                        3 => TimeInForce::FOK,
                        _ => TimeInForce::GTC,
                    };
                    let p = if k % 2 == 0 {
                        limit(
                            m,
                            true,
                            101 + (k % 3) as i64,
                            1 + (k % 3) as i64,
                            tif,
                            false,
                        )
                    } else {
                        limit(
                            m,
                            false,
                            99 - (k % 3) as i64,
                            1 + (k % 2) as i64,
                            tif,
                            false,
                        )
                    };
                    orders.push((sender, p, 1000 + 100 * m as OrderId + k as OrderId));
                }
                orders.push((
                    trader(9),
                    limit(m, false, 97, 3, TimeInForce::GTC, true),
                    1099 + 100 * m as OrderId,
                ));
                MarketInput {
                    id: m,
                    orders,
                    ro: vec![(trader(9), 2)],
                }
            })
            .collect()
    }

    /// The market's book with its makers' ladder (identical on every call).
    fn seeded_book(m: MarketId) -> OrderBook {
        let mut b = OrderBook::new(m, fp(1), fp(1));
        let ladder = [
            (1u8, false, 101),
            (2, false, 102),
            (3, false, 103),
            (4, true, 99),
            (5, true, 98),
            (6, true, 97),
        ];
        for (i, &(k, is_buy, px)) in ladder.iter().enumerate() {
            b.set_next_order_id(1 + i as OrderId);
            b.place_order(
                limit(m, is_buy, px, 2, TimeInForce::GTC, false),
                trader(k),
                TS,
            );
        }
        b
    }

    fn ro_positions(input: &MarketInput) -> ReduceOnlyPositions {
        let mut ro = ReduceOnlyPositions::new();
        for &(t, s) in &input.ro {
            ro.insert(t, fp(s));
        }
        ro
    }

    fn requests(input: &MarketInput) -> Vec<MatchRequest<'_>> {
        input
            .orders
            .iter()
            .map(|(sender, params, order_id)| MatchRequest {
                sender: *sender,
                params,
                order_id: *order_id,
                margin: None,
            })
            .collect()
    }

    /// What the executor's setup does, on the job: install the policed
    /// positions, then build the requests.
    fn setup<'a>(
        m: MarketId,
        book: &mut OrderBook,
        input: &'a MarketInput,
    ) -> Vec<MatchRequest<'a>> {
        assert_eq!(m, input.id);
        book.set_reduce_only_positions(ro_positions(input));
        requests(input)
    }

    /// [`setup`] without the policed positions (reduce-only polices as flat).
    fn setup_without_positions<'a>(
        _m: MarketId,
        _book: &mut OrderBook,
        input: &'a MarketInput,
    ) -> Vec<MatchRequest<'a>> {
        requests(input)
    }

    /// [`setup`] that panics for market 3.
    fn setup_panics_in_3<'a>(
        m: MarketId,
        book: &mut OrderBook,
        input: &'a MarketInput,
    ) -> Vec<MatchRequest<'a>> {
        if m == 3 {
            panic!("setup boom in market {m}");
        }
        setup(m, book, input)
    }

    type Digest = Vec<(MarketId, OrderId, Vec<(Address, OrderId, String)>, Vec<u8>)>;

    /// Everything a result carries, market-id sorted (the caller sorts).
    fn digest(mut rs: Vec<MarketBatchResult>) -> Digest {
        rs.sort_by_key(|r| r.market_id);
        rs.into_iter()
            .map(|r| {
                let results = r
                    .results
                    .iter()
                    .map(|x| (x.sender, x.order_id, format!("{:?}", x.result)))
                    .collect();
                (
                    r.market_id,
                    r.next_order_id,
                    results,
                    borsh::to_vec(&r.book).expect("book encodes"),
                )
            })
            .collect()
    }

    fn reference(inputs: &[MarketInput], cap: usize) -> Digest {
        let mut batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> = HashMap::default();
        for input in inputs {
            let mut book = seeded_book(input.id);
            book.set_reduce_only_positions(ro_positions(input));
            batches.insert(input.id, (book, requests(input)));
        }
        digest(
            MarketWorkerPool::match_parallel_capped_with(batches, TS, cap, None, true)
                .expect("no panic"),
        )
    }

    fn in_jobs(inputs: &[MarketInput], cap: usize) -> Digest {
        let items: Vec<(MarketId, OrderBook, &MarketInput, usize)> = inputs
            .iter()
            .map(|i| (i.id, seeded_book(i.id), i, i.orders.len()))
            .collect();
        digest(
            MarketWorkerPool::match_parallel_setup_with(items, TS, cap, None, true, &setup)
                .expect("no panic"),
        )
    }

    /// (a) Same results as the pre-built batches for every cap, and for a
    /// single market (the inline path).
    #[test]
    fn setup_in_jobs_matches_prebuilt_batches() {
        let many = inputs(7);
        for cap in [1usize, 2, 3, 32] {
            let want = reference(&many, cap);
            assert_eq!(want.len(), 7);
            assert_eq!(in_jobs(&many, cap), want, "cap {cap}");
        }
        let one = inputs(1);
        assert_eq!(in_jobs(&one, 4), reference(&one, 4), "single market");
    }

    /// The reduce-only sell (qty 3) is clamped to the installed position
    /// (long 2) in every market, so the job's setup ran before its matching;
    /// without the setup's positions the book does not police it (all 3).
    #[test]
    fn setup_runs_before_matching() {
        let ins = inputs(4);
        let run = |install: bool| -> Vec<(MarketId, FixedPoint)> {
            let items: Vec<(MarketId, OrderBook, &MarketInput, usize)> = ins
                .iter()
                .map(|i| (i.id, seeded_book(i.id), i, i.orders.len()))
                .collect();
            let out = if install {
                MarketWorkerPool::match_parallel_setup_with(items, TS, 2, None, true, &setup)
            } else {
                MarketWorkerPool::match_parallel_setup_with(
                    items,
                    TS,
                    2,
                    None,
                    true,
                    &setup_without_positions,
                )
            }
            .expect("no panic");
            let mut v: Vec<(MarketId, FixedPoint)> = out
                .iter()
                .map(|r| {
                    let ro = r.results.last().expect("the reduce-only order is last");
                    assert_eq!(ro.sender, trader(9));
                    let filled = ro
                        .result
                        .fills
                        .iter()
                        .fold(FixedPoint::ZERO, |a, f| a + f.quantity);
                    (r.market_id, filled + ro.result.rested_qty)
                })
                .collect();
            v.sort_by_key(|x| x.0);
            v
        };
        assert_eq!(run(true), (1..=4).map(|m| (m, fp(2))).collect::<Vec<_>>());
        assert_eq!(run(false), (1..=4).map(|m| (m, fp(3))).collect::<Vec<_>>());
    }

    #[test]
    fn empty_input_is_ok_empty() {
        let items: Vec<(MarketId, OrderBook, &MarketInput, usize)> = Vec::new();
        let out = MarketWorkerPool::match_parallel_setup_with(items, TS, 4, None, true, &setup)
            .expect("no panic");
        assert!(out.is_empty());
    }

    /// (b) A setup panic is that market's `MarketWorkerPanic` (payload kept),
    /// multi-market at every cap and single-market.
    #[test]
    fn setup_panic_is_a_contained_market_worker_panic() {
        let many = inputs(5);
        for cap in [1usize, 2, 32] {
            let items: Vec<(MarketId, OrderBook, &MarketInput, usize)> = many
                .iter()
                .map(|i| (i.id, seeded_book(i.id), i, i.orders.len()))
                .collect();
            let err = match MarketWorkerPool::match_parallel_setup_with(
                items,
                TS,
                cap,
                None,
                true,
                &setup_panics_in_3,
            ) {
                Err(e) => e,
                Ok(_) => panic!("cap {cap}: a setup panic must surface as MarketWorkerPanic"),
            };
            assert_eq!(err.market_id, 3, "cap {cap}");
            assert!(
                err.message.contains("setup boom in market 3"),
                "cap {cap}: {}",
                err.message
            );
        }
        let single: Vec<(MarketId, OrderBook, &MarketInput, usize)> =
            vec![(3, seeded_book(3), &many[2], many[2].orders.len())];
        let err = match MarketWorkerPool::match_parallel_setup_with(
            single,
            TS,
            4,
            None,
            true,
            &setup_panics_in_3,
        ) {
            Err(e) => e,
            Ok(_) => panic!("single market: a setup panic must surface as MarketWorkerPanic"),
        };
        assert_eq!(err.market_id, 3);
        assert!(
            err.message.contains("setup boom in market 3"),
            "{}",
            err.message
        );
    }
}
