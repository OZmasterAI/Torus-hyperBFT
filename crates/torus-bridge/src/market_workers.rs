//! Per-market parallel order matching pool.
//!
//! Dispatches PlaceOrder batches to scoped threads for concurrent matching.
//! Each thread temporarily owns its market's OrderBook for cache-local access.

use std::collections::HashMap;

use alloy_primitives::Address;
use torus_core::order_book::{OrderBook, PlaceResult};
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

/// Parallel matching dispatcher using scoped threads.
///
/// Each market's batch runs on a dedicated thread for the duration of matching.
/// The thread owns the OrderBook during execution, ensuring cache-local access
/// to BTreeMap nodes without lock contention.
pub struct MarketWorkerPool;

impl MarketWorkerPool {
    /// Match orders across multiple markets in parallel.
    ///
    /// Takes ownership of each market's OrderBook, dispatches matching to a
    /// CAPPED set of scoped threads, and returns updated books with match
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
        Self::match_parallel_capped(batches, timestamp, Self::resolve_worker_cap())
    }

    /// [`Self::match_parallel`] with an explicit worker cap (>= 1 enforced).
    /// Split out so tests can force a small cap independent of the host's core
    /// count; the public entry point supplies the resolved cap.
    pub fn match_parallel_capped<'a>(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'a>>)>,
        timestamp: u64,
        max_workers: usize,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic> {
        if batches.is_empty() {
            return Ok(Vec::new());
        }

        // Single market — skip thread spawn overhead.
        if batches.len() == 1 {
            let (market_id, (book, requests)) = batches.into_iter().next().unwrap();
            return Self::run_contained(market_id, || {
                Self::match_market(market_id, book, requests, timestamp)
            })
            .map(|r| vec![r]);
        }

        // Multiple markets — chunk them across at most `max_workers` scoped
        // threads. The borrowed `MatchRequest.params` refs outlive the scope
        // (they borrow from the caller's action slice), so scoped threads may
        // capture them freely; each chunk is moved into its worker thread.
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

        // Copy-capture closure (only `timestamp: u64`) so each worker gets its
        // own copy — the actual per-market matcher `process_chunk` runs.
        let match_one =
            move |market_id: MarketId, book: OrderBook, requests: Vec<MatchRequest<'a>>| {
                Self::match_market(market_id, book, requests, timestamp)
            };

        std::thread::scope(|s| {
            let handles: Vec<_> = chunks
                .into_iter()
                .map(|chunk| {
                    // First market in the chunk represents it for the (theoretical)
                    // join-failure fallback — a contained worker never unwinds.
                    let rep = chunk[0].0;
                    (rep, s.spawn(move || Self::process_chunk(chunk, &match_one)))
                })
                .collect();

            // Flatten per-worker results in worker order; the first `Err`
            // short-circuits and the remaining scoped threads are joined at
            // scope exit. Return order is irrelevant (caller sorts by id).
            let mut out = Vec::with_capacity(n);
            for (rep, h) in handles {
                let chunk_results = h.join().unwrap_or_else(|_| {
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
        })
    }

    /// Resolve the worker cap: `TORUS_MATCH_WORKERS` (clamped to >= 1) if set and
    /// parseable, else the host parallelism, else 1.
    fn resolve_worker_cap() -> usize {
        if let Ok(raw) = std::env::var("TORUS_MATCH_WORKERS") {
            if let Ok(n) = raw.parse::<usize>() {
                return n.max(1);
            }
        }
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    /// LPT (longest-processing-time) assignment of markets to `workers`.
    ///
    /// Markets are considered heaviest-first (request count desc, market_id asc
    /// as a deterministic tie-break) and each is placed on the currently
    /// least-loaded worker (lowest worker index breaks load ties). Returns, for
    /// each input index, its worker index in `[0, workers)`. Pure over the
    /// `(market_id, count)` pairs and independent of their input order, so chunk
    /// layout is reproducible.
    fn assign_chunks(markets: &[(MarketId, usize)], workers: usize) -> Vec<usize> {
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
    fn run_contained(
        market_id: MarketId,
        f: impl FnOnce() -> MarketBatchResult,
    ) -> Result<MarketBatchResult, MarketWorkerPanic> {
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
    ) -> MarketBatchResult {
        let mut results = Vec::with_capacity(requests.len());

        for req in requests {
            book.set_next_order_id(req.order_id);
            // C2: THE one params copy in the pipeline — the book takes ownership.
            let place_result = book.place_order(req.params.clone(), req.sender, timestamp);

            results.push(MatchResult {
                sender: req.sender,
                order_id: req.order_id,
                result: place_result,
            });
        }

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
        let batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest<'_>>)> = HashMap::new();
        let results =
            MarketWorkerPool::match_parallel(batches, 1000).expect("no worker, no panic");
        assert!(results.is_empty());
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
