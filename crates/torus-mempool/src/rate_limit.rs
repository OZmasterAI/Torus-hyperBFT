//! Block-space budgets and per-address limits for mempool selection.
//!
//! Prevents any single address from consuming disproportionate block space or
//! flooding the mempool. All limits are grouped here as configurable constants.
//! D1 (S392): EVM selection is bounded by a per-block GAS BUDGET; the count
//! caps and the dormant sliding-window RateTracker were deleted.

use torus_types::NativeAction;

// ============================================================================
// Block-space limits
// ============================================================================

/// Per-block EVM gas budget (D1, S392) — replaces the deleted count caps
/// (EVM_TOTAL_BLOCK_CAP=20 / EVM_PER_BLOCK_CAP=4). Gas is the correct unit
/// for the guarantee those caps existed for: bounding worst-case EVM
/// execution time on the commit path. 5M is the conservative debut (~1/6 of
/// the 30M block gas limit); the acceptance bench runs with the env override
/// at 15M (half the block) and only a passing mixed-load bench promotes that
/// to the default — see docs/plans/evm-blocker-set-decisions.md D1.
pub const EVM_BLOCK_GAS_BUDGET: u64 = 5_000_000;

/// Per-sender share of the EVM gas budget, in percent (D1) — a faucet-era
/// spam guard, ON for testnet. 0 disables it (mainnet = pure gas budget,
/// Ethereum-style).
pub const EVM_SENDER_SHARE_PCT: u32 = 25;

/// Effective EVM gas budget: `TORUS_EVM_BLOCK_GAS_BUDGET` overrides the
/// compiled default PER NODE, read once at first use (same operational
/// pattern as `TORUS_HASH_ONLY_PUSH_THRESHOLD`). Proposer-local selection
/// policy — validators execute whatever the committed block carries — so it
/// can be A/B'd on one validator without coordination and can never split
/// consensus.
pub fn evm_block_gas_budget() -> u64 {
    static BUDGET: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| {
        std::env::var("TORUS_EVM_BLOCK_GAS_BUDGET")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(EVM_BLOCK_GAS_BUDGET)
    })
}

/// Effective per-sender share percent: `TORUS_EVM_SENDER_SHARE_PCT` overrides
/// the compiled default (0 disables the cap). Read once at first use;
/// proposer-local like the budget.
pub fn evm_sender_share_pct() -> u32 {
    static PCT: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *PCT.get_or_init(|| {
        std::env::var("TORUS_EVM_SENDER_SHARE_PCT")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(EVM_SENDER_SHARE_PCT)
    })
}

/// Max native actions per sender per block.
pub const NATIVE_PER_BLOCK_CAP: usize = 64;

/// Max total native actions per block (all senders combined). Held at 100 — this
/// also bounds the block BODY to a size that disseminates over the WAN. s365
/// raised it to 1000 and it WEDGED under 3-box load: blocks grew to ~292 actions
/// (the 6 MB `NATIVE_BLOCK_BYTES_CAP`), and 6 MB bodies overran native-DA
/// (`native-da OUTBOUND FAILURE` + `body fetch exhausted ... falling back to
/// sync`), stalling the chain until the load stopped. So 100 is a deliberate
/// dissemination guard, not just a sig-verify bound — at bs400 it keeps blocks
/// ~2 MB (stable). Raise it ONLY after body dissemination is fixed (erasure-coded
/// bodies, docs/plans). Enforced only in produce_block selection; validate_block
/// does not reject on count, so mixed cap values don't fork.
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

/// Capacity of the per-node exec trust-cache (`verified_senders`: locally-verified
/// action hash -> recovered sender). Sized to comfortably bridge the in-flight
/// window between ingress/gossip-recover and execution: the exec pipeline lags up
/// to the exec queue depth (64 committed blocks, app.rs `sync_channel(64)`) behind
/// consensus, each block up to `NATIVE_TOTAL_BLOCK_CAP` (100) actions => a 6400
/// hard floor. Shipped at ~2.5x margin so churn / gossip dups don't evict
/// still-needed entries before exec reads them. Over-cap or cold => cache MISS =>
/// full recover + slash (safe). ~32B/entry => ~0.5MB at this cap.
pub const VERIFIED_SENDER_CACHE_CAP: usize = 16_384;


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

#[cfg(test)]
mod tests {
    use super::*;
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
