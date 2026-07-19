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

/// Effective per-sender native action cap: `TORUS_NATIVE_PER_BLOCK_CAP`
/// overrides the compiled default PER NODE (same pattern as
/// `TORUS_EVM_BLOCK_GAS_BUDGET`). Proposer-local selection policy —
/// validate_block does not reject on count, so mixed values can't fork.
pub fn native_per_block_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TORUS_NATIVE_PER_BLOCK_CAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(NATIVE_PER_BLOCK_CAP)
    })
}

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

/// Effective total native action cap: `TORUS_NATIVE_TOTAL_BLOCK_CAP` overrides
/// the compiled default PER NODE. Proposer-local selection policy (enforced
/// only in produce_block; validate_block does not reject on count) — mixed
/// values across validators cannot fork consensus. The S387 cap-probe proved
/// cap=1000 no longer wedges (push-hardening + c408c0e off-loop pull serving);
/// the compiled default stays 100 until a full-mesh bench earns the raise.
pub fn native_total_block_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TORUS_NATIVE_TOTAL_BLOCK_CAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(NATIVE_TOTAL_BLOCK_CAP)
    })
}

/// Max total native pool size. With gossip replication, each validator holds
/// actions from all peers, so this must be large enough for the full mesh.
pub const NATIVE_POOL_MAX_SIZE: usize = 65536;

/// Max pending native actions per sender in the pool. With non-destructive
/// selection (actions stay until commit), this must cover burst submissions.
pub const NATIVE_PER_SENDER_CAP: usize = 512;

/// Max orders a single `PlaceOrderBatch` may carry (Phase B throughput keystone).
///
/// Canonical definition lives in `torus-types` (single source shared with the
/// exec-side deterministic skip in torus-bridge). Enforced at RPC ingress
/// (`validate_batch_size`, torus-rpc torus.rs), at gossip/DA admission
/// (`Mempool::admit_gossip`), and — the consensus-critical layer — at the
/// `execute_batch` flatten, which skips an oversize batch wholesale on every
/// node identically. Clients (market makers) tune their actual batch size up
/// to this bound. Bytes per batch ≈ size × ~70B.
pub use torus_types::NATIVE_ORDERS_PER_BATCH_CAP;

/// Max total orders (individual `PlaceOrder` + expanded `PlaceOrderBatch`) admitted
/// per block. Bounds worst-case matching/execution time so block production stays
/// within the consensus view budget.
///
/// NOTE: Enforced in `select_for_block_with_senders_excluding` via
/// `order_count` (O2/G2, S416); selection-only — `validate_block` does not
/// reject on order count, so mixed values cannot fork.
pub const NATIVE_ORDERS_PER_BLOCK_CAP: usize = 50_000;

/// Effective per-block ORDER budget: `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP`
/// overrides the compiled default PER NODE (same OnceLock pattern as
/// `TORUS_NATIVE_TOTAL_BLOCK_CAP`). Proposer-local selection policy —
/// validate_block does not reject on order count — mixed values cannot fork.
pub fn native_orders_per_block_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        let parsed = std::env::var("TORUS_NATIVE_ORDERS_PER_BLOCK_CAP")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(NATIVE_ORDERS_PER_BLOCK_CAP);
        // Floor at the per-batch cap: selection breaks on the first entry whose
        // order_count would exceed the budget, so any value below a single
        // legal batch (or 0) silently wedges native selection to empty blocks.
        // Clamp + WARN rather than obey it (same doctrine as the O5 env floors).
        if parsed < NATIVE_ORDERS_PER_BATCH_CAP {
            tracing::warn!(
                requested = parsed,
                floor = NATIVE_ORDERS_PER_BATCH_CAP,
                "TORUS_NATIVE_ORDERS_PER_BLOCK_CAP below the per-batch cap would starve \
                 native selection; clamping to the floor"
            );
            return NATIVE_ORDERS_PER_BATCH_CAP;
        }
        parsed
    })
}

/// Hard ceiling on the summed bincode-encoded size of native-action bodies in
/// one block (bytes) — the WAN dissemination budget. Selection stops before
/// exceeding this, so a flooded mempool degrades to more, smaller blocks
/// instead of undisseminatable ones (s334 bs1000 wedge: ~7.5MB bodies, body
/// fetches exhausted, every leader re-proposed the same mega-block). Like
/// `NATIVE_ORDERS_PER_BLOCK_CAP` (O2/G2, S416), this is enforced in
/// `select_for_block_with_senders_excluding`.
///
/// 2MB was the push/manifest-pull-only budget (s334 measured 34.5k orders/s
/// pinned at exactly this cap × block rate). With Sprint 3 native-action
/// gossip pre-spread, bodies are already on every validator by proposal time
/// and the proposal moves ~hashes only, so the per-block budget rises to 6MB
/// (~40k orders at ~150B/order). Gap-pulls + rotated body fetch cover misses.
pub const NATIVE_BLOCK_BYTES_CAP: usize = 6_000_000;

/// Effective native block bytes cap: `TORUS_NATIVE_BLOCK_BYTES_CAP` overrides
/// the compiled default PER NODE. Proposer-local (selection stops before the
/// cap; validators execute whatever the committed block carries) — safe to A/B
/// on one proposer without coordination.
pub fn native_block_bytes_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TORUS_NATIVE_BLOCK_BYTES_CAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(NATIVE_BLOCK_BYTES_CAP)
    })
}

/// Capacity of the per-node exec trust-cache (`verified_senders`: locally-verified
/// action hash -> recovered sender). Sized to comfortably bridge the in-flight
/// window between ingress/gossip-recover and execution: the exec pipeline lags up
/// to the exec queue depth (64 committed blocks, app.rs `sync_channel(64)`) behind
/// consensus, each block up to `NATIVE_TOTAL_BLOCK_CAP` (100) actions => a 6400
/// hard floor. Shipped at ~2.5x margin so churn / gossip dups don't evict
/// still-needed entries before exec reads them. Over-cap or cold => cache MISS =>
/// full recover + slash (safe). ~32B/entry => ~0.5MB at this cap.
///
/// PACKAGE D rank 3: this default's in-flight-window floor is `64 * 100`. Raising
/// `TORUS_NATIVE_TOTAL_BLOCK_CAP` above 100 raises that floor proportionally
/// (`64 * cap`) — at cap 400 the window is 64*400 = 25_600, ABOVE this 16_384
/// default, so entries would evict before exec reads them and the exec hot path
/// silently loses its secp256k1-recover skip (a MISS is still correct, just
/// slower). A bench that raises the block cap should raise this in lockstep via
/// `TORUS_VERIFIED_SENDER_CACHE_CAP` (e.g. ~40_000 keeps the ~2.5x margin at cap
/// 400). This is NOT a clamp on the NUMBER of actions a block can carry — it never
/// bounds selection — but it is the trust-cache sizing that keeps a raised cap
/// fast. Default unchanged: unset env => byte-identical to today.
pub const VERIFIED_SENDER_CACHE_CAP: usize = 16_384;

/// Parse the raw `TORUS_VERIFIED_SENDER_CACHE_CAP` value (pure, so it is unit
/// testable without env/OnceLock state — the rank-1 parser doctrine). Unset,
/// malformed, or `0` => the compiled default (the `FifoCache` itself floors at 1,
/// but `0` here means "operator left it effectively unset", so we keep the safe
/// default rather than degrade the cache to a single entry).
pub fn parse_verified_sender_cache_cap(raw: Option<String>) -> usize {
    match raw.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => VERIFIED_SENDER_CACHE_CAP,
    }
}

/// Effective exec trust-cache capacity: `TORUS_VERIFIED_SENDER_CACHE_CAP`
/// overrides the compiled default PER NODE. Node-local sizing of a
/// performance-only cache (a HIT == a fresh recover, a MISS falls through to full
/// recover) — it can never change the resolved sender or fork, so mixed values
/// across nodes are safe. Read at mempool construction; unset => the default.
pub fn verified_sender_cache_cap() -> usize {
    parse_verified_sender_cache_cap(std::env::var("TORUS_VERIFIED_SENDER_CACHE_CAP").ok())
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
/// [`NATIVE_ORDERS_PER_BATCH_CAP`]. Non-batch actions always pass. Shares the
/// exact validity predicate ([`torus_types::batch_len_within_cap`]) with the
/// consensus exec-side skip, so admit and exec can never disagree on the bound.
pub fn validate_batch_size(action: &NativeAction) -> Result<(), String> {
    if let NativeAction::PlaceOrderBatch(orders) = action {
        if !torus_types::batch_len_within_cap(orders.len()) {
            return Err(format!(
                "PlaceOrderBatch size {} outside [1, {}]",
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
        assert_eq!(
            order_count(&NativeAction::PlaceOrderBatch(vec![p.clone(); 5])),
            5
        );
        assert_eq!(order_count(&NativeAction::PlaceOrderBatch(vec![])), 0);
    }

    // ---- Package D rank 3: cap resolution + exact-today defaults ----

    #[test]
    fn native_block_cap_defaults_are_exactly_todays_values() {
        // These pin the "unset env => byte-identical to ee763a2" contract: the
        // per-block native TOTAL cap and per-SENDER cap defaults must not drift.
        // A bench opts into a higher ceiling explicitly; the compiled defaults
        // stay put.
        assert_eq!(NATIVE_TOTAL_BLOCK_CAP, 100, "per-block total native cap default");
        assert_eq!(NATIVE_PER_BLOCK_CAP, 64, "per-sender native cap default");
        assert_eq!(
            VERIFIED_SENDER_CACHE_CAP, 16_384,
            "exec trust-cache default"
        );
    }

    #[test]
    fn verified_sender_cache_cap_parse_resolves_and_defaults() {
        // Unset / malformed / zero => the compiled default (byte-identical).
        assert_eq!(parse_verified_sender_cache_cap(None), VERIFIED_SENDER_CACHE_CAP);
        assert_eq!(
            parse_verified_sender_cache_cap(Some("not-a-number".into())),
            VERIFIED_SENDER_CACHE_CAP
        );
        assert_eq!(
            parse_verified_sender_cache_cap(Some("".into())),
            VERIFIED_SENDER_CACHE_CAP
        );
        assert_eq!(
            parse_verified_sender_cache_cap(Some("0".into())),
            VERIFIED_SENDER_CACHE_CAP,
            "0 means effectively-unset, keep the safe default rather than a 1-entry cache"
        );
        // A valid override is honored — this is the lever a cap-400 bench uses to
        // keep the trust-cache in-flight window (64 * cap) covered.
        assert_eq!(parse_verified_sender_cache_cap(Some("40000".into())), 40_000);
        assert_eq!(
            parse_verified_sender_cache_cap(Some("  40000  ".into())),
            40_000,
            "surrounding whitespace tolerated"
        );
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
