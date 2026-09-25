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

/// Max total native actions per block (all senders combined). This also bounds
/// the block BODY to a size that disseminates over the WAN. History: 100 was a
/// deliberate dissemination guard — s365 raised it to 1000 and it WEDGED under
/// 3-box load (blocks grew to ~292 actions / the 6 MB `NATIVE_BLOCK_BYTES_CAP`,
/// 6 MB bodies overran native-DA: `native-da OUTBOUND FAILURE` + `body fetch
/// exhausted ... falling back to sync`, chain stalled until the load stopped).
/// Since then body dissemination was hardened (push-hardening, c408c0e off-loop
/// pull serving, chunked pulls, zstd wire, hash-only manifests; the S387
/// cap-probe proved cap=1000 no longer wedges), and the r3 block-cap-raise sweep
/// (perf/matched-200k, 3-val, bs400, 10 markets) measured cap 200 as the winner:
/// +12% matched/s avg / +18% best-60s over cap 100 with clean 3-validator
/// agreement. r4 PROMOTES that env-only bundle to the compiled default so an
/// unset env reproduces it byte-for-byte (see [`NATIVE_ORDERS_PER_BLOCK_CAP`],
/// [`NATIVE_BLOCK_BYTES_CAP`], [`default_verified_sender_cache_cap`]);
/// `TORUS_NATIVE_TOTAL_BLOCK_CAP=100` restores the previous behaviour.
///
/// Enforced only in produce_block selection; validate_block does not reject on
/// count, so mixed cap values across validators don't fork (rolling-upgrade
/// safe; the raise is proposer-local policy, not a chain parameter). WAN caveat
/// stands: the win was measured on loopback — a multi-box WAN fleet at bs400
/// pushes ~5.6 MB pre-proposal bodies per block (direct push up to the 8 MB
/// `torus_network::caps::DIRECT_PUSH_BODY_BYTES` floor, hash-manifest + pull
/// above it).
pub const NATIVE_TOTAL_BLOCK_CAP: usize = 200;

/// Effective total native action cap: `TORUS_NATIVE_TOTAL_BLOCK_CAP` overrides
/// the compiled default PER NODE. Proposer-local selection policy (enforced
/// only in produce_block; validate_block does not reject on count) — mixed
/// values across validators cannot fork consensus.
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

// ---- s65 item B: node-side admission limit ----
//
// Under overload the pool used to accept everything: ~35% of submitted actions
// then expired unused after NONCE_WINDOW_MS, having cost every node a signature
// check, a gossip hop and a DA write. With the limit on, RPC ingress sheds
// non-cancels BEFORE verification ("busy, retry") once the pool already holds
// more than `horizon` worth of recent commit throughput.

/// How many seconds of commit history the admission rate is measured over.
pub const ADMISSION_RATE_WINDOW_MS: u64 = 10_000;

/// Default admission horizon. Campaign s66-abc3 (cap 200, n=3): expired
/// actions 17-18% vs 29-50% with the limit off, throughput within noise.
pub const DEFAULT_ADMISSION_HORIZON_MS: u64 = 20_000;

/// Parse `TORUS_ADMISSION_HORIZON_MS`: the milliseconds of recent commit
/// throughput the native pool may hold before ingress sheds. `0` = limit OFF
/// (kill switch, the pre-s65 behaviour); unset or unparsable => the default.
pub fn parse_admission_horizon_ms(raw: Option<String>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_ADMISSION_HORIZON_MS)
}

/// Effective admission horizon for this node (node-local policy; mixed values
/// across validators are safe — it only decides what ingress accepts).
pub fn admission_horizon_ms() -> u64 {
    parse_admission_horizon_ms(std::env::var("TORUS_ADMISSION_HORIZON_MS").ok())
}

/// Default floor under the admission limit: 4 full blocks, so a cold node (no
/// commits measured yet) or a briefly stalled chain never sheds a normal load.
pub fn admission_floor() -> usize {
    4 * native_total_block_cap()
}

/// Pool size at which ingress starts shedding, or `None` when the limit is
/// off. `committed` = native actions committed within the last
/// `window_elapsed_ms` (at most [`ADMISSION_RATE_WINDOW_MS`]); the limit is that
/// rate times `horizon_ms`, never below `floor`.
pub fn admission_limit(
    horizon_ms: u64,
    floor: usize,
    committed: usize,
    window_elapsed_ms: u64,
) -> Option<usize> {
    if horizon_ms == 0 {
        return None;
    }
    let elapsed = window_elapsed_ms.clamp(1_000, ADMISSION_RATE_WINDOW_MS) as u128;
    let by_rate = (committed as u128 * horizon_ms as u128 / elapsed) as usize;
    Some(by_rate.max(floor))
}

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
///
/// r4: 50_000 (= 125 bs400 actions) was the cap that actually bound FIRST at
/// the old cap-100 default's bench shape and would silently pin a cap-200
/// block to 125 actions. Promoted with the cap-200 default to the r3 bundle
/// value `max(50_000, 200 * 400 * 5/4)` = 100_000 (tools/matched-bench
/// run-cell.sh `block_cap_bundle 200`), so an unset env reproduces the r3
/// selection caps exactly. Still the worst-case-exec-time bound: 100k orders
/// per block at the measured ~1 s exec block time.
pub const NATIVE_ORDERS_PER_BLOCK_CAP: usize = 100_000;

/// The r3 bundle's reference client batch size (bench `--batch-size 400`), the
/// shape the cap-200 companion defaults were derived against. Sizing constant
/// only (a client may send any batch up to `NATIVE_ORDERS_PER_BATCH_CAP`).
pub const BLOCK_CAP_BUNDLE_REF_BATCH: usize = 400;

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
/// and the proposal moves ~hashes only, so the per-block budget rose to 6MB
/// (~40k orders at ~150B/order). Gap-pulls + rotated body fetch cover misses.
///
/// r4: 12 MB — the r3 bundle value for the cap-200 default
/// (`min(12 MB, max(6 MB, 200 * 400 * 150 B))`, run-cell.sh
/// `block_cap_bundle 200`): 6 MB was touched at cap 200 / bs400 (~5.6 MB of
/// ~70 B orders, more with client_order_id) and would silently thin the block.
/// 12 MB is the ceiling the bundle allows because `/torus/block-data` (sync
/// codec, 16 MiB) must still carry a full body during catch-up; bodies above
/// the 8 MB direct-push floor go out as a hash manifest and are pulled in
/// chunks; the shard read cap (`MAX_NATIVE_DA_SHARDS_MSG_SIZE`, 8 MiB) admits
/// the 6 MB worst-case k=2 shard of a 12 MB body.
pub const NATIVE_BLOCK_BYTES_CAP: usize = 12_000_000;

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
/// consensus, each block up to `NATIVE_TOTAL_BLOCK_CAP` actions (6400 at cap 100, 12_800 at the r4 cap 200) => the
/// hard floor. Shipped at ~2.5x margin so churn / gossip dups don't evict
/// still-needed entries before exec reads them. Over-cap or cold => cache MISS =>
/// full recover + slash (safe). ~32B/entry => ~0.5MB at this cap.
///
/// PACKAGE D rank 3: this default's in-flight-window floor is `64 * 100`. Raising
/// `TORUS_NATIVE_TOTAL_BLOCK_CAP` above 100 raises that floor proportionally
/// (`64 * cap`) — at cap 400 the window is 64*400 = 25_600, ABOVE this 16_384
/// default, so entries would evict before exec reads them and the exec hot path
/// silently loses its secp256k1-recover skip (a MISS is still correct, just
/// slower). Block-cap-raise sweep (r2): the DEFAULT now follows the effective
/// block cap — [`default_verified_sender_cache_cap`] keeps the ~2.5x margin
/// (`64 * cap * 5/2`, never below this constant), so a `TORUS_NATIVE_TOTAL_BLOCK_CAP`
/// raise no longer needs a lockstep `TORUS_VERIFIED_SENDER_CACHE_CAP`; an explicit
/// env value still wins verbatim. This is NOT a clamp on the NUMBER of actions a
/// block can carry — it never bounds selection — but it is the trust-cache sizing
/// that keeps a raised cap fast. At cap 100 the resolved value is exactly this
/// constant; r4 (compiled cap 200, unset env) resolves to 32_000 — the r3
/// bundle's `TORUS_VERIFIED_SENDER_CACHE_CAP` value. This constant stays the
/// historical floor, not the shipped default.
pub const VERIFIED_SENDER_CACHE_CAP: usize = 16_384;

/// Depth of the committed-block exec queue (app.rs `sync_channel(64)`): the
/// number of blocks that can sit between consensus commit and execution, i.e.
/// the in-flight window the trust cache must bridge. Sizing constant only —
/// changing the real channel depth without this drifts the cache floor.
pub const EXEC_QUEUE_DEPTH: usize = 64;

/// Hard floor of trust-cache entries needed to bridge the in-flight window at a
/// given total block cap: `EXEC_QUEUE_DEPTH * cap` (12_800 at the compiled cap 200).
pub fn verified_sender_cache_in_flight_floor(total_block_cap: usize) -> usize {
    EXEC_QUEUE_DEPTH.saturating_mul(total_block_cap)
}

/// Cap-derived DEFAULT trust-cache capacity: the in-flight floor at ~2.5x margin
/// (`64 * cap * 5 / 2`), never below [`VERIFIED_SENDER_CACHE_CAP`]. cap 100 =>
/// 16_000 < 16_384 => the compiled default (unchanged); cap 200 => 32_000;
/// cap 300 => 48_000; cap 400 => 64_000. Pure, so unit-testable.
pub fn default_verified_sender_cache_cap(total_block_cap: usize) -> usize {
    let scaled = verified_sender_cache_in_flight_floor(total_block_cap).saturating_mul(5) / 2;
    scaled.max(VERIFIED_SENDER_CACHE_CAP)
}

/// Resolve the raw `TORUS_VERIFIED_SENDER_CACHE_CAP` value against the effective
/// total block cap (pure, so it is unit testable without env/OnceLock state — the
/// rank-1 parser doctrine). Unset, malformed, or `0` => the cap-derived default
/// (the `FifoCache` itself floors at 1, but `0` here means "operator left it
/// effectively unset", so we keep the safe default rather than degrade the cache
/// to a single entry). An explicit positive value is honored verbatim — a
/// too-small cache is slower (more MISS => full recover), never incorrect.
pub fn resolve_verified_sender_cache_cap(raw: Option<String>, total_block_cap: usize) -> usize {
    match raw.and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => default_verified_sender_cache_cap(total_block_cap),
    }
}

/// Compiled-cap view of [`resolve_verified_sender_cache_cap`] (kept for
/// callers/tests that pin the compiled-default contract; cap 200 since r4).
pub fn parse_verified_sender_cache_cap(raw: Option<String>) -> usize {
    resolve_verified_sender_cache_cap(raw, NATIVE_TOTAL_BLOCK_CAP)
}

/// Effective exec trust-cache capacity: `TORUS_VERIFIED_SENDER_CACHE_CAP`
/// overrides the cap-derived default PER NODE. Node-local sizing of a
/// performance-only cache (a HIT == a fresh recover, a MISS falls through to full
/// recover) — it can never change the resolved sender or fork, so mixed values
/// across nodes are safe. Read at mempool construction; unset => the default for
/// the effective `native_total_block_cap()`. An explicit value below the in-flight
/// floor WARNs (still honored): the exec hot path would lose its recover skip.
pub fn verified_sender_cache_cap() -> usize {
    let cap = native_total_block_cap();
    let resolved =
        resolve_verified_sender_cache_cap(std::env::var("TORUS_VERIFIED_SENDER_CACHE_CAP").ok(), cap);
    let floor = verified_sender_cache_in_flight_floor(cap);
    if resolved < floor {
        tracing::warn!(
            resolved,
            floor,
            total_block_cap = cap,
            "TORUS_VERIFIED_SENDER_CACHE_CAP below the exec in-flight window \
             (EXEC_QUEUE_DEPTH * block cap): trust-cache entries may evict before \
             exec reads them (slower, still correct)"
        );
    }
    resolved
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

    #[test]
    fn admission_horizon_parse_defaults_on() {
        assert_eq!(parse_admission_horizon_ms(None), 20_000);
        assert_eq!(parse_admission_horizon_ms(Some("abc".into())), 20_000);
        assert_eq!(parse_admission_horizon_ms(Some("".into())), 20_000);
        assert_eq!(parse_admission_horizon_ms(Some("0".into())), 0);
        assert_eq!(parse_admission_horizon_ms(Some("10000".into())), 10_000);
        assert_eq!(parse_admission_horizon_ms(Some(" 5000 ".into())), 5_000);
    }

    #[test]
    fn admission_limit_off_when_horizon_zero() {
        assert_eq!(admission_limit(0, 800, 2_200, 10_000), None);
    }

    #[test]
    fn admission_limit_is_commit_rate_times_horizon() {
        // 2,200 actions committed over 10 s = 220/s; 10 s horizon => 2,200.
        assert_eq!(admission_limit(10_000, 800, 2_200, 10_000), Some(2_200));
        // Same rate measured over 5 s of history.
        assert_eq!(admission_limit(10_000, 800, 1_100, 5_000), Some(2_200));
    }

    #[test]
    fn admission_limit_never_below_floor() {
        // Cold node: nothing committed yet.
        assert_eq!(admission_limit(10_000, 800, 0, 0), Some(800));
        // Slow chain: 50 actions in 10 s => 50 by rate, floor wins.
        assert_eq!(admission_limit(10_000, 800, 50, 10_000), Some(800));
    }

    #[test]
    fn admission_limit_rate_window_is_clamped() {
        // Under 1 s of history counts as 1 s (no rate spike from one block).
        assert_eq!(admission_limit(10_000, 0, 200, 10), Some(2_000));
        // Over the window counts as the window.
        assert_eq!(admission_limit(10_000, 0, 2_200, 60_000), Some(2_200));
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
        assert_eq!(
            order_count(&NativeAction::PlaceOrderBatch(vec![p.clone(); 5])),
            5
        );
        assert_eq!(order_count(&NativeAction::PlaceOrderBatch(vec![])), 0);
    }

    // ---- Package D rank 3: cap resolution + exact-today defaults ----

    #[test]
    fn native_block_cap_defaults_are_exactly_todays_values() {
        // r4: the compiled defaults ARE the r3 cap-200 bundle. These pin the
        // "unset env => byte-identical to run-cell.sh BLOCK_CAP=200" contract:
        // the per-block native TOTAL cap and per-SENDER cap defaults must not
        // drift; the trust-cache FLOOR constant stays the historical 16_384.
        assert_eq!(NATIVE_TOTAL_BLOCK_CAP, 200, "per-block total native cap default (r3 winner)");
        assert_eq!(NATIVE_PER_BLOCK_CAP, 64, "per-sender native cap default");
        assert_eq!(
            VERIFIED_SENDER_CACHE_CAP, 16_384,
            "exec trust-cache floor constant"
        );
    }

    /// r4: the compiled companion defaults reproduce tools/matched-bench
    /// run-cell.sh `block_cap_bundle 200` at the reference bs400 shape exactly:
    ///   orders = max(50_000, N*400*5/4)                       => 100_000
    ///   cache  = max(16_384, 64*N*5/2)                        =>  32_000
    ///   bytes  = min(12_000_000, max(6_000_000, N*400*150))   => 12_000_000
    /// so an unset env is the r3 record cell, not a fourth configuration.
    #[test]
    fn compiled_defaults_reproduce_the_r3_cap200_bundle() {
        let n = NATIVE_TOTAL_BLOCK_CAP;
        let b = BLOCK_CAP_BUNDLE_REF_BATCH;
        assert_eq!(b, 400);
        let orders = (n * b * 5 / 4).max(50_000);
        let cache = (64 * n * 5 / 2).max(16_384);
        let bytes = (n * b * 150).max(6_000_000).min(12_000_000);
        assert_eq!(NATIVE_ORDERS_PER_BLOCK_CAP, orders, "orders-per-block companion cap");
        assert_eq!(NATIVE_ORDERS_PER_BLOCK_CAP, 100_000);
        assert_eq!(default_verified_sender_cache_cap(n), cache, "trust-cache derived default");
        assert_eq!(default_verified_sender_cache_cap(n), 32_000);
        assert_eq!(NATIVE_BLOCK_BYTES_CAP, bytes, "block bytes companion cap");
        assert_eq!(NATIVE_BLOCK_BYTES_CAP, 12_000_000);
        // The orders cap never binds before the action cap at the reference
        // batch size (that was the cap-100-era trap: 50_000 = 125 bs400 actions).
        assert!(NATIVE_ORDERS_PER_BLOCK_CAP >= n * b);
        // The trust cache still bridges the in-flight window with margin.
        assert!(
            default_verified_sender_cache_cap(n) >= 2 * verified_sender_cache_in_flight_floor(n)
        );
    }

    #[test]
    fn verified_sender_cache_cap_parse_resolves_and_defaults() {
        // Unset / malformed / zero => the cap-derived default for the compiled
        // cap (32_000 at cap 200 — the r3 bundle value).
        let compiled_default = default_verified_sender_cache_cap(NATIVE_TOTAL_BLOCK_CAP);
        assert_eq!(compiled_default, 32_000);
        assert_eq!(parse_verified_sender_cache_cap(None), compiled_default);
        assert_eq!(
            parse_verified_sender_cache_cap(Some("not-a-number".into())),
            compiled_default
        );
        assert_eq!(
            parse_verified_sender_cache_cap(Some("".into())),
            compiled_default
        );
        assert_eq!(
            parse_verified_sender_cache_cap(Some("0".into())),
            compiled_default,
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

    // ---- block-cap raise: trust-cache default follows the block cap ----

    #[test]
    fn verified_sender_cache_floor_tracks_exec_queue_depth_times_cap() {
        // The exec pipeline lags up to EXEC_QUEUE_DEPTH committed blocks behind
        // consensus; each block carries up to `cap` actions => the hard floor
        // of entries that must survive until exec reads them.
        assert_eq!(EXEC_QUEUE_DEPTH, 64, "mirrors app.rs sync_channel(64)");
        assert_eq!(verified_sender_cache_in_flight_floor(100), 6_400);
        assert_eq!(verified_sender_cache_in_flight_floor(300), 19_200);
    }

    #[test]
    fn default_verified_sender_cache_cap_is_unchanged_at_cap_100_and_scales_above() {
        // cap 100 (the pre-r4 compiled default / `TORUS_NATIVE_TOTAL_BLOCK_CAP=100`
        // control): 64*100*2.5 = 16_000 < 16_384 => the historical default is
        // byte-identical. r4's compiled cap 200 => 32_000 (r3 bundle value).
        assert_eq!(default_verified_sender_cache_cap(100), VERIFIED_SENDER_CACHE_CAP);
        assert_eq!(default_verified_sender_cache_cap(100), 16_384);
        assert_eq!(default_verified_sender_cache_cap(NATIVE_TOTAL_BLOCK_CAP), 32_000);
        // Never BELOW the compiled default (a lowered cap keeps the 16_384).
        assert_eq!(default_verified_sender_cache_cap(10), 16_384);
        assert_eq!(default_verified_sender_cache_cap(0), 16_384);
        // Above 100 the ~2.5x in-flight margin is kept automatically, so a
        // TORUS_NATIVE_TOTAL_BLOCK_CAP raise does not silently lose the
        // secp256k1-recover skip on the exec hot path.
        assert_eq!(default_verified_sender_cache_cap(150), 24_000);
        assert_eq!(default_verified_sender_cache_cap(200), 32_000);
        assert_eq!(default_verified_sender_cache_cap(300), 48_000);
        assert_eq!(default_verified_sender_cache_cap(400), 64_000);
    }

    #[test]
    fn resolve_verified_sender_cache_cap_prefers_explicit_env_over_derived_default() {
        // Unset / malformed / 0 => the cap-derived default.
        assert_eq!(resolve_verified_sender_cache_cap(None, 300), 48_000);
        assert_eq!(resolve_verified_sender_cache_cap(Some("nope".into()), 300), 48_000);
        assert_eq!(resolve_verified_sender_cache_cap(Some("0".into()), 300), 48_000);
        // An explicit value is honored verbatim, even below the derived default
        // (operator sizing wins; a too-small cache is slower, never incorrect).
        assert_eq!(resolve_verified_sender_cache_cap(Some("40000".into()), 300), 40_000);
        assert_eq!(resolve_verified_sender_cache_cap(Some("1000".into()), 300), 1_000);
        // The legacy single-arg parser is the compiled-cap view of the same seam.
        assert_eq!(
            parse_verified_sender_cache_cap(None),
            resolve_verified_sender_cache_cap(None, NATIVE_TOTAL_BLOCK_CAP)
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
