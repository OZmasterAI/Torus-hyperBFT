//! Per-address transaction rate limiting configuration and tracking.
//!
//! Task 3.1.4: Prevents any single address from consuming disproportionate block
//! space or flooding the mempool. All limits are grouped here as configurable constants.

use std::collections::{HashMap, VecDeque};

use alloy_primitives::Address;
use torus_types::NativeAction;

// ============================================================================
// Rate limit constants (Task 3.1.4)
// ============================================================================

/// Number of recent blocks to track for rate limiting (sliding window).
pub const RATE_WINDOW_BLOCKS: u64 = 100;

/// Max EVM transactions per sender within the rate window.
pub const EVM_RATE_LIMIT_PER_WINDOW: u32 = 50;

/// Max native actions per sender within the rate window.
/// Higher than EVM because trading involves many small actions.
pub const NATIVE_RATE_LIMIT_PER_WINDOW: u32 = 200;

/// Max EVM transactions per sender per block.
pub const EVM_PER_BLOCK_CAP: usize = 4;

/// Max total EVM transactions per block (all senders combined).
/// Bounds worst-case EVM execution time to stay within the view timeout.
pub const EVM_TOTAL_BLOCK_CAP: usize = 20;

/// Max native actions per sender per block.
pub const NATIVE_PER_BLOCK_CAP: usize = 64;

/// Max total native actions per block (all senders combined).
/// Bounded by sig verification cost in produce_block + validate_block.
pub const NATIVE_TOTAL_BLOCK_CAP: usize = 100;

/// Max total native pool size. With gossip replication, each validator holds
/// actions from all peers, so this must be large enough for the full mesh.
pub const NATIVE_POOL_MAX_SIZE: usize = 65536;

/// Max pending native actions per sender in the pool. With non-destructive
/// selection (actions stay until commit), this must cover burst submissions.
pub const NATIVE_PER_SENDER_CAP: usize = 512;

/// Max orders a single `PlaceOrderBatch` may carry (Phase B throughput keystone).
///
/// Chain-side safety ceiling rejected at RPC ingress + block validation. Clients
/// (market makers) tune their *actual* batch size up to this bound — that's the
/// "configurable" knob for finding the throughput sweet spot. Bytes per batch
/// ≈ size × ~70B, so a full block of batches must stay under
/// `max_consensus_message_size` — Phase C raises that limit and this cap together.
pub const NATIVE_ORDERS_PER_BATCH_CAP: usize = 1024;

/// Max total orders (individual `PlaceOrder` + expanded `PlaceOrderBatch`) admitted
/// per block. Bounds worst-case matching/execution time so block production stays
/// within the consensus view budget.
///
/// NOTE: enforcement is wired into `produce_block` order-aware selection in Phase C
/// (alongside raising `NATIVE_TOTAL_BLOCK_CAP`). Today selection counts *actions*;
/// this constant documents the target order ceiling. See `order_count`.
pub const NATIVE_ORDERS_PER_BLOCK_CAP: usize = 50_000;

/// Hard ceiling on the summed bincode-encoded size of native-action bodies in
/// one block (bytes) — the WAN dissemination budget. Selection stops before
/// exceeding this, so a flooded mempool degrades to more, smaller blocks
/// instead of undisseminatable ones (s334 bs1000 wedge: ~7.5MB bodies, body
/// fetches exhausted, every leader re-proposed the same mega-block). Unlike
/// `NATIVE_ORDERS_PER_BLOCK_CAP` (documented target, not yet enforced), this
/// IS enforced in `select_for_block_with_senders_excluding`.
///
/// 2MB was the push/manifest-pull-only budget (s334 measured 34.5k orders/s
/// pinned at exactly this cap × block rate). With Sprint 3 native-action
/// gossip pre-spread, bodies are already on every validator by proposal time
/// and the proposal moves ~hashes only, so the per-block budget rises to 6MB
/// (~40k orders at ~150B/order). Gap-pulls + rotated body fetch cover misses.
pub const NATIVE_BLOCK_BYTES_CAP: usize = 6_000_000;

// ============================================================================
// Rate tracker
// ============================================================================

/// Tracks per-address transaction inclusion rates using a sliding block window.
///
/// State is in-memory only — resets on node restart (cold start is acceptable
/// per spec). Every node makes the same accept/reject decision given the same
/// block history, ensuring determinism.
pub struct RateTracker {
    /// Per-sender EVM tx counts: sender -> deque of (block_height, count).
    evm_counts: HashMap<Address, VecDeque<(u64, u32)>>,
    /// Per-sender native action counts.
    native_counts: HashMap<Address, VecDeque<(u64, u32)>>,
    /// Highest recorded block height (for window pruning).
    current_block: u64,
    /// Window size in blocks.
    window_size: u64,
    /// EVM rate limit per window.
    evm_limit: u32,
    /// Native rate limit per window.
    native_limit: u32,
}

impl RateTracker {
    pub fn new(window_size: u64, evm_limit: u32, native_limit: u32) -> Self {
        Self {
            evm_counts: HashMap::new(),
            native_counts: HashMap::new(),
            current_block: 0,
            window_size,
            evm_limit,
            native_limit,
        }
    }

    /// Record senders whose transactions/actions were included in a committed block.
    pub fn record_block(
        &mut self,
        block_height: u64,
        evm_senders: &[Address],
        native_senders: &[Address],
    ) {
        self.current_block = block_height;

        // Aggregate EVM sender counts for this block.
        let mut evm_block: HashMap<Address, u32> = HashMap::new();
        for sender in evm_senders {
            *evm_block.entry(*sender).or_insert(0) += 1;
        }
        for (sender, count) in evm_block {
            self.evm_counts
                .entry(sender)
                .or_default()
                .push_back((block_height, count));
        }

        // Aggregate native sender counts for this block.
        let mut native_block: HashMap<Address, u32> = HashMap::new();
        for sender in native_senders {
            *native_block.entry(*sender).or_insert(0) += 1;
        }
        for (sender, count) in native_block {
            self.native_counts
                .entry(sender)
                .or_default()
                .push_back((block_height, count));
        }

        self.prune();
    }

    /// Check if a sender is rate-limited for EVM transactions.
    pub fn is_evm_rate_limited(&self, sender: &Address) -> bool {
        self.sender_total(&self.evm_counts, sender) >= self.evm_limit
    }

    /// Check if a sender is rate-limited for native actions.
    pub fn is_native_rate_limited(&self, sender: &Address) -> bool {
        self.sender_total(&self.native_counts, sender) >= self.native_limit
    }

    /// Get the total count for a sender within the current window.
    fn sender_total(
        &self,
        counts: &HashMap<Address, VecDeque<(u64, u32)>>,
        sender: &Address,
    ) -> u32 {
        let cutoff = self.current_block.saturating_sub(self.window_size);
        counts
            .get(sender)
            .map(|deque| {
                deque
                    .iter()
                    .filter(|(h, _)| *h > cutoff)
                    .map(|(_, c)| c)
                    .sum()
            })
            .unwrap_or(0)
    }

    /// Remove entries older than the window.
    fn prune(&mut self) {
        let cutoff = self.current_block.saturating_sub(self.window_size);
        Self::prune_map(&mut self.evm_counts, cutoff);
        Self::prune_map(&mut self.native_counts, cutoff);
    }

    fn prune_map(map: &mut HashMap<Address, VecDeque<(u64, u32)>>, cutoff: u64) {
        map.retain(|_, deque| {
            while let Some(&(h, _)) = deque.front() {
                if h <= cutoff {
                    deque.pop_front();
                } else {
                    break;
                }
            }
            !deque.is_empty()
        });
    }
}

/// Number of individual orders/operations an action represents.
///
/// A `PlaceOrderBatch` counts as its length; every other action counts as 1.
/// Used for per-block order accounting and for charging rate limits per *order*
/// rather than per *batch* (so one giant batch can't dodge the rate limiter).
pub fn order_count(action: &NativeAction) -> usize {
    match action {
        NativeAction::PlaceOrderBatch(orders) => orders.len(),
        _ => 1,
    }
}

/// Reject malformed batches at ingress: empty (no-op spam) or larger than
/// [`NATIVE_ORDERS_PER_BATCH_CAP`]. Non-batch actions always pass.
pub fn validate_batch_size(action: &NativeAction) -> Result<(), String> {
    if let NativeAction::PlaceOrderBatch(orders) = action {
        if orders.is_empty() {
            return Err("empty PlaceOrderBatch".to_string());
        }
        if orders.len() > NATIVE_ORDERS_PER_BATCH_CAP {
            return Err(format!(
                "PlaceOrderBatch size {} exceeds NATIVE_ORDERS_PER_BATCH_CAP {}",
                orders.len(),
                NATIVE_ORDERS_PER_BATCH_CAP
            ));
        }
    }
    Ok(())
}

/// Check if a native action type is exempt from rate limiting.
///
/// Oracle updates and governance actions are exempt because validators
/// must submit them reliably regardless of rate limit status.
/// Identified by action type, not by address whitelist.
pub fn is_exempt_action(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::SubmitOraclePrices(_)
            | NativeAction::SubmitProposal(_)
            | NativeAction::Vote { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_types::{OracleSubmission, Proposal, ProposalAction, VoteOption};

    #[test]
    fn rate_tracker_below_limit() {
        let mut tracker = RateTracker::new(100, 50, 200);
        let sender = Address::repeat_byte(1);

        for i in 1..=49 {
            tracker.record_block(i, &[sender], &[]);
        }
        assert!(!tracker.is_evm_rate_limited(&sender));
    }

    #[test]
    fn rate_tracker_at_limit_rejects() {
        let mut tracker = RateTracker::new(100, 50, 200);
        let sender = Address::repeat_byte(1);

        for i in 1..=50 {
            tracker.record_block(i, &[sender], &[]);
        }
        assert!(tracker.is_evm_rate_limited(&sender));
    }

    #[test]
    fn rate_tracker_window_slides() {
        let mut tracker = RateTracker::new(100, 50, 200);
        let sender = Address::repeat_byte(1);

        // Fill up to limit in blocks 1-50.
        for i in 1..=50 {
            tracker.record_block(i, &[sender], &[]);
        }
        assert!(tracker.is_evm_rate_limited(&sender));

        // Advance 51 blocks without this sender — old entries slide out.
        for i in 51..=101 {
            tracker.record_block(i, &[], &[]);
        }
        // Block 1 is now outside the window (current=101, cutoff=1).
        // Blocks 2-50 have 49 txs → below 50 limit.
        assert!(!tracker.is_evm_rate_limited(&sender));
    }

    #[test]
    fn rate_tracker_multiple_senders_independent() {
        let mut tracker = RateTracker::new(100, 50, 200);
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);

        for i in 1..=50 {
            tracker.record_block(i, &[a], &[]);
        }
        assert!(tracker.is_evm_rate_limited(&a));
        assert!(!tracker.is_evm_rate_limited(&b));
    }

    #[test]
    fn rate_tracker_native_limit() {
        let mut tracker = RateTracker::new(100, 50, 200);
        let sender = Address::repeat_byte(1);

        // 10 actions per block for 20 blocks = 200 → at limit.
        for i in 1..=20 {
            let senders: Vec<Address> = vec![sender; 10];
            tracker.record_block(i, &[], &senders);
        }
        assert!(tracker.is_native_rate_limited(&sender));
    }

    #[test]
    fn rate_tracker_multiple_txs_per_block() {
        let mut tracker = RateTracker::new(100, 50, 200);
        let sender = Address::repeat_byte(1);

        // 5 txs in each of 10 blocks = 50 → at limit.
        for i in 1..=10 {
            tracker.record_block(i, &vec![sender; 5], &[]);
        }
        assert!(tracker.is_evm_rate_limited(&sender));
    }

    #[test]
    fn cold_start_no_limits() {
        let tracker = RateTracker::new(100, 50, 200);
        let sender = Address::repeat_byte(1);
        assert!(!tracker.is_evm_rate_limited(&sender));
        assert!(!tracker.is_native_rate_limited(&sender));
    }

    #[test]
    fn exempt_actions() {
        assert!(is_exempt_action(&NativeAction::SubmitOraclePrices(
            OracleSubmission {
                prices: vec![],
                timestamp: 0,
            }
        )));
        assert!(is_exempt_action(&NativeAction::SubmitProposal(Proposal {
            title: String::new(),
            description: String::new(),
            action: ProposalAction::ParameterChange {
                key: String::new(),
                value: String::new(),
            },
        })));
        assert!(is_exempt_action(&NativeAction::Vote {
            proposal_id: 0,
            option: VoteOption::Yes,
        }));
        assert!(!is_exempt_action(&NativeAction::ClaimRewards));
        assert!(!is_exempt_action(&NativeAction::CancelOrder { order_id: 1 }));
    }

    // ---- PlaceOrderBatch caps (Phase B, Task B3) ----

    fn sample_params() -> torus_types::PlaceOrderParams {
        torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(100),
            quantity: torus_types::FixedPoint::from_raw(100),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    #[test]
    fn order_count_counts_orders_not_actions() {
        let p = sample_params();
        assert_eq!(order_count(&NativeAction::CancelOrder { order_id: 1 }), 1);
        assert_eq!(order_count(&NativeAction::PlaceOrder(p.clone())), 1);
        assert_eq!(order_count(&NativeAction::PlaceOrderBatch(vec![p.clone(); 5])), 5);
        assert_eq!(order_count(&NativeAction::PlaceOrderBatch(vec![])), 0);
    }

    #[test]
    fn validate_batch_size_rejects_oversize_and_empty() {
        let p = sample_params();

        // At the cap: accepted.
        let at_cap = NativeAction::PlaceOrderBatch(vec![p.clone(); NATIVE_ORDERS_PER_BATCH_CAP]);
        assert!(validate_batch_size(&at_cap).is_ok());

        // One over the cap: rejected.
        let over = NativeAction::PlaceOrderBatch(vec![p.clone(); NATIVE_ORDERS_PER_BATCH_CAP + 1]);
        assert!(validate_batch_size(&over).is_err());

        // Empty batch (no-op spam): rejected.
        assert!(validate_batch_size(&NativeAction::PlaceOrderBatch(vec![])).is_err());

        // Non-batch actions always pass.
        assert!(validate_batch_size(&NativeAction::CancelOrder { order_id: 1 }).is_ok());
        assert!(validate_batch_size(&NativeAction::PlaceOrder(p)).is_ok());
    }
}
