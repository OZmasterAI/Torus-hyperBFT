//! Per-market parallel order matching pool.
//!
//! Dispatches PlaceOrder batches to scoped threads for concurrent matching.
//! Each thread temporarily owns its market's OrderBook for cache-local access.

use std::collections::HashMap;

use alloy_primitives::Address;
use torus_core::order_book::{OrderBook, PlaceResult};
use torus_types::{MarketId, OrderId, PlaceOrderParams};

/// A single order to be matched by a worker thread.
pub struct MatchRequest {
    pub sender: Address,
    pub params: PlaceOrderParams,
    pub order_id: OrderId,
}

/// Result of matching a single order.
pub struct MatchResult {
    pub sender: Address,
    pub params: PlaceOrderParams,
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
    /// Takes ownership of each market's OrderBook, dispatches matching to
    /// scoped threads, and returns updated books with match results.
    ///
    /// T1.5: a panic in any worker is CONTAINED (`catch_unwind`) and surfaced
    /// as a typed [`MarketWorkerPanic`] instead of unwinding through
    /// `thread::scope` and killing the execution thread (which left consensus
    /// zombie-advancing on a closed exec channel).
    pub fn match_parallel(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest>)>,
        timestamp: u64,
    ) -> Result<Vec<MarketBatchResult>, MarketWorkerPanic> {
        if batches.is_empty() {
            return Ok(Vec::new());
        }

        // Single market — skip thread spawn overhead
        if batches.len() == 1 {
            let (market_id, (book, requests)) = batches.into_iter().next().unwrap();
            return Self::run_contained(market_id, || {
                Self::match_market(market_id, book, requests, timestamp)
            })
            .map(|r| vec![r]);
        }

        // Multiple markets — parallel via scoped threads
        let batches_vec: Vec<(MarketId, OrderBook, Vec<MatchRequest>)> = batches
            .into_iter()
            .map(|(id, (book, reqs))| (id, book, reqs))
            .collect();

        std::thread::scope(|s| {
            let handles: Vec<_> = batches_vec
                .into_iter()
                .map(|(market_id, book, requests)| {
                    (
                        market_id,
                        s.spawn(move || {
                            Self::run_contained(market_id, || {
                                Self::match_market(market_id, book, requests, timestamp)
                            })
                        }),
                    )
                })
                .collect();

            // Contained workers never unwind, so `join` itself cannot fail;
            // map a (theoretical) failure to the typed error anyway — no
            // expect/unwrap on this path. An `Err` short-circuits collect;
            // the remaining scoped threads are joined at scope exit.
            handles
                .into_iter()
                .map(|(market_id, h)| {
                    h.join().unwrap_or_else(|_| {
                        Err(MarketWorkerPanic {
                            market_id,
                            message: "worker thread died outside contained matching".to_string(),
                        })
                    })
                })
                .collect()
        })
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
        requests: Vec<MatchRequest>,
        timestamp: u64,
    ) -> MarketBatchResult {
        let mut results = Vec::with_capacity(requests.len());

        for req in requests {
            book.set_next_order_id(req.order_id);
            let place_result = book.place_order(req.params.clone(), req.sender, timestamp);

            results.push(MatchResult {
                sender: req.sender,
                params: req.params,
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
        let batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest>)> = HashMap::new();
        let results =
            MarketWorkerPool::match_parallel(batches, 1000).expect("no worker, no panic");
        assert!(results.is_empty());
    }
}
