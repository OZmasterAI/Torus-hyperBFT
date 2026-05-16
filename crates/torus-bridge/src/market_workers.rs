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
    pub fn match_parallel(
        batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest>)>,
        timestamp: u64,
    ) -> Vec<MarketBatchResult> {
        if batches.is_empty() {
            return Vec::new();
        }

        // Single market — skip thread spawn overhead
        if batches.len() == 1 {
            let (market_id, (book, requests)) = batches.into_iter().next().unwrap();
            return vec![Self::match_market(market_id, book, requests, timestamp)];
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
                    s.spawn(move || Self::match_market(market_id, book, requests, timestamp))
                })
                .collect();

            handles
                .into_iter()
                .map(|h| h.join().expect("market worker panicked"))
                .collect()
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
