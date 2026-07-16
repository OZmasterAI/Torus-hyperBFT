//! Native action execution — dispatches each NativeAction to the correct handler.
//!
//! NativeExecutor is the core dispatch layer for processing native (non-EVM) actions
//! within a block. It handles order book operations, staking, oracle, governance,
//! lockbox, and liquidation processing in a deterministic pipeline.
//!
//! Task 2.5.1: NativeExecutor dispatch table + batch execution.

use std::collections::HashMap;

use alloy_primitives::{Address, B256};
use torus_core::error::CoreError;
use torus_core::liquidation::LiquidationEngine;
use torus_core::lockbox::{fp_to_u256, u256_to_fp, Lockbox};
use torus_core::margin::{effective_max_leverage, MarketMarginConfig};
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::{OrderBook, OrderStatus, PlaceResult};
use torus_core::position::{MarginType, NativeBalance, PositionManager};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_economics::epoch::ValidatorSetDiff;
use torus_economics::{
    EpochManager, GovernanceManager, RewardDistributor, StakingManager, ValidatorStatus,
};
use torus_state::cf::{CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES};
use torus_state::{RawCfKv, StateBackend, StateDb};
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderId, OrderType, PlaceOrderParams, PublicKey,
    SessionScope, Side, TimeInForce, ValidatorInfo, ValidatorSet, VoteOption, U256,
};

use crate::market_workers::{MarketWorkerPool, MatchRequest};

// ============================================================================
// Result types
// ============================================================================

/// Result of executing a single native action.
#[derive(Clone, Debug)]
pub struct NativeActionResult {
    pub action_type: &'static str,
    pub success: bool,
    pub error: Option<String>,
    pub gas_used: u64,
}

impl NativeActionResult {
    fn ok(action_type: &'static str, gas_used: u64) -> Self {
        Self {
            action_type,
            success: true,
            error: None,
            gas_used,
        }
    }

    fn err(action_type: &'static str, error: String) -> Self {
        Self {
            action_type,
            success: false,
            error: Some(error),
            gas_used: 0,
        }
    }
}

/// Result of executing a batch of native actions.
#[derive(Clone, Debug)]
pub struct NativeBatchResult {
    pub results: Vec<NativeActionResult>,
    pub total_gas: u64,
}

/// Result of epoch boundary processing. Carries the validator set diff
/// for future hotstuff_rs ValidatorSetUpdates integration.
pub struct EpochBoundaryResult {
    pub action: NativeActionResult,
    pub new_set: Option<ValidatorSet>,
    pub diff: Option<ValidatorSetDiff>,
}

// ============================================================================
// Execution context — holds all state managers and per-block metadata
// ============================================================================

/// All state managers and block metadata needed for native execution.
/// Per-`execute_batch`-call write-back cache for native balances (O1).
///
/// Phase 2 (margin reserve) and Phase 4 (margin release) touch each sender's
/// `NativeBalance` repeatedly. The dominant cost is the per-order `NativeStateOverlay`
/// PUT: each `put_cf_raw` allocates a `String` CF-name + key/value `Vec`s, Borsh-encodes,
/// takes an `RwLock`, and inserts into a `BTreeMap`. At bs400 with N flood senders, a
/// naive path pays one such PUT per order (~4×N per block); this cache defers those,
/// mutating an in-memory typed map and flushing each *unique* sender's final balance to
/// the overlay once (≈N PUTs), while first-read misses fall through to the overlay.
///
/// Coherence (the overlay must be authoritative at every point some *other* reader could
/// observe it): the only in-call reader that bypasses this cache is Phase 4 `apply_fill`,
/// which credits realized PnL straight to the overlay. Before each `apply_fill`, the
/// trader's pending balance is flushed and evicted (`flush_and_evict`) so `apply_fill`
/// reads the post-release balance and the later `flush_all` cannot clobber the credit.
/// `flush_all` runs at the end of the call, so the *next* `execute_batch` call and all
/// post-batch consumers (`drain_core_writer`, `save_order_books`, block-end flush) see a
/// fully materialized overlay. Flush order is over distinct per-sender keys, so it is
/// state-independent of iteration order; the map is otherwise never iterated.
struct BalanceCache {
    map: HashMap<Address, NativeBalance>,
    dirty: std::collections::HashSet<Address>,
}

impl BalanceCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            dirty: std::collections::HashSet::new(),
        }
    }

    /// Return the sender's balance, reading through to `positions` on a miss.
    fn load<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        addr: &Address,
    ) -> Result<NativeBalance, CoreError> {
        if let Some(bal) = self.map.get(addr) {
            return Ok(bal.clone());
        }
        let bal = positions.get_native_balance(addr)?;
        self.map.insert(*addr, bal.clone());
        Ok(bal)
    }

    /// Update the cached balance and mark it dirty (write-back — no overlay PUT yet).
    fn set(&mut self, addr: &Address, bal: NativeBalance) {
        self.map.insert(*addr, bal);
        self.dirty.insert(*addr);
    }

    /// Flush a trader's pending balance to the overlay (if dirty) and evict it, handing
    /// authority back to the overlay. Called before `apply_fill` credits that trader
    /// directly, so the credit lands on the post-release balance and survives `flush_all`.
    fn flush_and_evict<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
        addr: &Address,
    ) -> Result<(), CoreError> {
        if let Some(bal) = self.map.remove(addr) {
            if self.dirty.remove(addr) {
                positions.put_native_balance(addr, &bal)?;
            }
        }
        Ok(())
    }

    /// Flush every pending dirty balance to the overlay (end of the `execute_batch` call).
    /// Keys are distinct per sender, so final overlay state is independent of flush order;
    /// sorted anyway to keep the write sequence deterministic.
    fn flush_all<T: StateBackend>(
        &mut self,
        positions: &PositionManager<T>,
    ) -> Result<(), CoreError> {
        let mut addrs: Vec<Address> = self.dirty.iter().copied().collect();
        addrs.sort();
        for addr in &addrs {
            if let Some(bal) = self.map.get(addr) {
                positions.put_native_balance(addr, bal)?;
            }
        }
        self.dirty.clear();
        Ok(())
    }
}

pub struct NativeExecContext<T: StateBackend = StateDb> {
    pub positions: PositionManager<T>,
    pub oracle: OracleManager<T>,
    pub staking: StakingManager<T>,
    pub governance: GovernanceManager<T>,
    pub state: T,

    /// Order books (per market, in-memory).
    pub order_books: HashMap<MarketId, OrderBook>,
    /// Markets whose book was mutated during THIS block (placed / matched /
    /// cancelled / modified). `save_order_books` writes only these — untouched
    /// books already hold identical bytes in the CF, so skipping them is
    /// byte-identical in final state (state-root-safe even in mixed
    /// deployments) and turns the old O(all resting orders) rewrite into
    /// O(touched) (S395).
    pub dirty_books: std::collections::HashSet<MarketId>,
    /// Per-market margin configuration.
    pub margin_configs: HashMap<MarketId, MarketMarginConfig>,
    /// FIX 6 (ECON-FIND-09): Global order ID counter shared across all markets.
    pub next_global_order_id: u128,
    /// Counter value at load time — the counter row is persisted only when it
    /// advanced this block (or was never stored), so no-order blocks write nothing.
    loaded_next_global_order_id: Option<u128>,

    // Block metadata
    pub block_height: u64,
    pub timestamp: u64,
    pub epoch: u64,
    pub epoch_length: u64,
    pub max_validators: u32,
    pub proposer: Address,
    pub treasury_address: Address,
    pub dev_pool_address: Address,

    /// Accumulated native fees during this block.
    pub total_native_fees: u64,
    /// Per-block trade counter for unique trade keys.
    pub trade_index: u32,

    /// O3: when set, trade-history writes (`CF_NATIVE_TRADES` /
    /// `CF_NATIVE_USER_TRADES` — node-local CFs outside the native consensus
    /// root, never read during execution) are buffered in `pending_trades`
    /// instead of PUT into the state backend, for the caller to hand to a
    /// background writer after all exec phases. Off by default: every existing
    /// caller keeps inline writes.
    pub defer_trades: bool,
    /// Raw trade-history KVs buffered while `defer_trades` is set.
    pending_trades: Vec<RawCfKv>,

    /// Optional metrics handle for Prometheus instrumentation.
    pub metrics: Option<std::sync::Arc<torus_telemetry::Metrics>>,

    /// T1.5: set (never cleared) when a market worker panicked mid-matching.
    /// The panicking worker consumed its market's `OrderBook`, so this block's
    /// post-state is unreconstructable — the committer MUST treat this as
    /// fatal (fail-stop the node), never flush state or mark the block applied.
    pub fatal_error: Option<String>,
}

impl<T: StateBackend> NativeExecContext<T> {
    /// Create a new execution context from a state backend and block metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        state: T,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Self {
        let positions = PositionManager::new(state.clone());
        let oracle = OracleManager::new(state.clone(), OracleConfig::default());
        let staking = StakingManager::new(state.clone());
        let governance = GovernanceManager::new(state.clone());

        // FIX 1 (ECON-FIND-02): Load persisted order books from DB on startup.
        let (order_books, scanned_next_id) = Self::load_order_books(&state);
        // S395: the durable counter row is authoritative when present — the
        // book-maxima scan resets to 1 once all books drain, silently reusing
        // order ids across a restart. max() keeps back-compat with DBs written
        // before the counter row existed.
        let persisted_next_id = Self::load_next_global_order_id(&state);
        let next_global_order_id = scanned_next_id.max(persisted_next_id.unwrap_or(1));

        Self {
            positions,
            oracle,
            staking,
            governance,
            state,
            order_books,
            dirty_books: std::collections::HashSet::new(),
            margin_configs: HashMap::new(),
            next_global_order_id,
            loaded_next_global_order_id: persisted_next_id,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            total_native_fees: 0,
            trade_index: 0,
            defer_trades: false,
            pending_trades: Vec::new(),
            metrics: None,
            fatal_error: None,
        }
    }

    /// Drain the trade-history KVs buffered under `defer_trades`. The caller
    /// owns durability from here (background writer, or synchronous fallback).
    pub fn take_pending_trades(&mut self) -> Vec<RawCfKv> {
        std::mem::take(&mut self.pending_trades)
    }

    /// FIX 1 (ECON-FIND-02): Load order books from DB. Returns (books, next_global_order_id).
    fn load_order_books(state: &T) -> (HashMap<MarketId, OrderBook>, u128) {
        use borsh::BorshDeserialize;
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;

        let mut books = HashMap::new();
        let mut max_order_id: u128 = 0;

        if let Ok(entries) = state.iterate_cf(CF_NATIVE_ORDER_BOOKS, None) {
            for (key, value) in entries {
                if key.len() == 8 {
                    let market_id = u64::from_be_bytes(key[..8].try_into().unwrap());
                    if let Ok(book) = OrderBook::try_from_slice(&value) {
                        let book_next_id = book.next_order_id();
                        if book_next_id > max_order_id {
                            max_order_id = book_next_id;
                        }
                        books.insert(market_id, book);
                    }
                }
            }
        }

        // Global ID starts at max found + 1 (or 1 if no books loaded)
        let next_id = if max_order_id > 0 { max_order_id } else { 1 };
        (books, next_id)
    }

    /// Durable global-order-id counter row. Lives in `CF_NATIVE_MARKETS` — a
    /// non-consensus-root CF (NOT one of `NATIVE_ROOT_CFS`, and every reader of
    /// that CF skips keys whose length != 8) — so it is node-local metadata
    /// that can never perturb the state root and is invisible to old binaries.
    const NEXT_GLOBAL_ORDER_ID_KEY: &'static [u8] = b"__next_global_order_id__";

    /// Read the persisted counter, if the row exists (DBs written before S395
    /// have none — the caller falls back to the book-maxima scan).
    fn load_next_global_order_id(state: &T) -> Option<u128> {
        use torus_state::cf::CF_NATIVE_MARKETS;
        let bytes = state
            .get_cf_raw(CF_NATIVE_MARKETS, Self::NEXT_GLOBAL_ORDER_ID_KEY)
            .ok()
            .flatten()?;
        Some(u128::from_be_bytes(bytes.try_into().ok()?))
    }

    /// FIX 1 (ECON-FIND-02) + S395 dirty-only rewrite: persist the order books
    /// TOUCHED this block (placed / matched / cancelled / modified) and the
    /// global order-id counter when it advanced. Untouched books already hold
    /// identical bytes in the CF — the old rewrite-everything loop cost
    /// O(all resting orders) in Borsh serialization per block. Returns the
    /// number of book rows written (test hook). Called after block execution.
    pub fn save_order_books(&self) -> usize {
        use torus_state::cf::{CF_NATIVE_MARKETS, CF_NATIVE_ORDER_BOOKS};

        let mut written = 0;
        for &market_id in &self.dirty_books {
            let Some(book) = self.order_books.get(&market_id) else {
                continue;
            };
            let key = market_id.to_be_bytes();
            match borsh::to_vec(book) {
                Ok(data) => {
                    if let Err(e) = self.state.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data) {
                        tracing::error!(market_id, %e, "failed to persist order book");
                    } else {
                        written += 1;
                    }
                }
                Err(e) => {
                    tracing::error!(market_id, %e, "failed to serialize order book");
                }
            }
        }

        // Persist the counter only when it moved (or was never stored), so
        // blocks without order flow write nothing at all.
        if self.loaded_next_global_order_id != Some(self.next_global_order_id) {
            if let Err(e) = self.state.put_cf_raw(
                CF_NATIVE_MARKETS,
                Self::NEXT_GLOBAL_ORDER_ID_KEY,
                &self.next_global_order_id.to_be_bytes(),
            ) {
                tracing::error!(%e, "failed to persist next_global_order_id");
            }
        }

        written
    }
}

// ============================================================================
// NativeExecutor — dispatch + execution
// ============================================================================

/// Dispatches each NativeAction variant to the correct torus-core / torus-economics handler.
///
/// Individual action failures are captured in `NativeActionResult` and do NOT
/// prevent other actions from executing (deterministic batch semantics).
pub struct NativeExecutor;

impl NativeExecutor {
    /// Execute a single native action for the given sender.
    pub fn execute<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        action: &NativeAction,
    ) -> NativeActionResult {
        match action {
            // ---- Order book ----
            NativeAction::PlaceOrder(params) => Self::exec_place_order(ctx, sender, params),
            // A batch reaching the single-action path (e.g. crash-replay) is routed through
            // the same flatten + per-market parallel pipeline as the live path, then collapsed
            // to one summary result. `execute_batch` flattens batches into `PlaceOrder`s, so it
            // never re-enters this arm — no recursion.
            NativeAction::PlaceOrderBatch(orders) => {
                if !torus_types::batch_len_within_cap(orders.len()) {
                    return NativeActionResult::err(
                        "place_order_batch",
                        format!(
                            "batch size {} outside [1, {}] — skipped (deterministic cap)",
                            orders.len(),
                            torus_types::NATIVE_ORDERS_PER_BATCH_CAP
                        ),
                    );
                }
                let pair = (*sender, action.clone());
                let batch = Self::execute_batch(ctx, std::slice::from_ref(&pair));
                let total = batch.results.len();
                let ok = batch.results.iter().filter(|r| r.success).count();
                NativeActionResult {
                    action_type: "place_order_batch",
                    success: ok == total,
                    error: (ok != total).then(|| format!("{ok}/{total} orders placed")),
                    gas_used: batch.total_gas,
                }
            }
            NativeAction::CancelOrder { order_id } => {
                Self::exec_cancel_order(ctx, sender, *order_id)
            }
            NativeAction::CancelAllOrders { market_id } => {
                Self::exec_cancel_all(ctx, sender, *market_id)
            }
            NativeAction::ModifyOrder {
                order_id,
                new_price,
                new_qty,
            } => Self::exec_modify_order(ctx, *order_id, *new_price, *new_qty),

            // ---- Staking ----
            NativeAction::Delegate { validator, amount } => {
                Self::exec_delegate(ctx, sender, validator, *amount)
            }
            NativeAction::Undelegate { validator, amount } => {
                Self::exec_undelegate(ctx, sender, validator, *amount)
            }
            NativeAction::PermanentStake { amount } => {
                Self::exec_permanent_stake(ctx, sender, *amount)
            }
            NativeAction::ClaimRewards => Self::exec_claim_rewards(ctx, sender),
            // FIX ECON-FIND-15: TopUpSelfStake via NativeAction.
            NativeAction::TopUpSelfStake { amount } => {
                match ctx.staking.top_up_self_stake(*sender, *amount) {
                    Ok(()) => NativeActionResult::ok("top_up_self_stake", 2000),
                    Err(e) => NativeActionResult::err("top_up_self_stake", e.to_string()),
                }
            }

            // ---- Oracle ----
            NativeAction::SubmitOraclePrices(submission) => Self::exec_submit_oracle_prices(
                ctx,
                sender,
                &submission.prices,
                submission.timestamp,
            ),

            // ---- Governance ----
            NativeAction::SubmitProposal(proposal) => {
                Self::exec_submit_proposal(ctx, sender, proposal)
            }
            NativeAction::Vote {
                proposal_id,
                option,
            } => Self::exec_vote(ctx, sender, *proposal_id, *option),

            // ---- Lockbox / Transfers ----
            NativeAction::TransferToPerp { amount } => {
                Self::exec_deposit_to_native(ctx, sender, *amount)
            }
            NativeAction::TransferToSpot { amount } => {
                Self::exec_withdraw_from_native(ctx, sender, *amount)
            }
            // FIX ECON-PF-17: Wire `to` address — debit sender, credit recipient.
            NativeAction::Withdraw { amount, to } => {
                Self::exec_withdraw_to(ctx, sender, to, *amount)
            }

            // ---- Validator management ----
            NativeAction::RegisterValidator { pubkey, commission } => {
                Self::exec_register_validator(ctx, sender, pubkey, *commission)
            }
            NativeAction::UpdateCommission { new_rate } => {
                Self::exec_update_commission(ctx, sender, *new_rate)
            }
            NativeAction::JailVote { target } => Self::exec_jail_vote(ctx, sender, target),
            NativeAction::UnjailSelf => Self::exec_unjail_self(ctx, sender),
            NativeAction::RotateValidatorKey { new_pubkey } => {
                Self::exec_rotate_key(ctx, sender, new_pubkey)
            }

            // ---- Session Keys ----
            NativeAction::CreateSession {
                session_pubkey,
                expiry,
                scope,
            } => Self::exec_create_session(ctx, sender, session_pubkey, *expiry, *scope),
            NativeAction::RevokeSession { session_pubkey } => {
                Self::exec_revoke_session(ctx, sender, session_pubkey)
            }

            // ---- Admin (governance-gated, stubs) ----
            // AUDIT: ECON-PF-07 -- Intentional stubs. Market management (listing,
            // delisting, param updates) will be implemented in the market registry
            // feature. Variants are defined now so governance pipeline and EIP-712
            // encoding are stable before the registry is built.
            NativeAction::UpdateMarketParams { .. } => {
                NativeActionResult::ok("update_market_params", 0)
            }
            NativeAction::ListMarket(_) => NativeActionResult::ok("list_market", 0),
            NativeAction::DelistMarket { .. } => NativeActionResult::ok("delist_market", 0),
        }
    }

    /// Execute a batch of (sender, action) pairs with per-market parallel matching.
    ///
    /// Pipeline:
    ///   Phase 1 — Execute all non-PlaceOrder actions sequentially
    ///   Phase 2 — Pre-reserve margin, assign global order IDs, partition by market
    ///   Phase 3 — Parallel matching: one thread per market's OrderBook
    ///   Phase 4 — Sequential settlement: release margin, apply fills, persist trades
    ///
    /// Individual action failures do NOT stop the batch (deterministic semantics).
    pub fn execute_batch<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        actions: &[(Address, NativeAction)],
    ) -> NativeBatchResult {
        // Flatten any PlaceOrderBatch into individual (sender, PlaceOrder) entries so the
        // per-market parallel matching pipeline treats batched and singly-submitted orders
        // identically. Deterministic: actions in slice order, orders in batch order. Zero-copy
        // on the common no-batch path via Cow::Borrowed.
        let flattened: std::borrow::Cow<[(Address, NativeAction)]> = if actions
            .iter()
            .any(|(_, a)| matches!(a, NativeAction::PlaceOrderBatch(_)))
        {
            let mut out = Vec::with_capacity(actions.len());
            let mut skipped_batches = 0usize;
            for (sender, action) in actions {
                match action {
                    NativeAction::PlaceOrderBatch(orders) => {
                        // G1 (O2): DETERMINISTIC exec-side cap. The RPC/admit
                        // checks are node-local policy; this is the consensus-
                        // critical bound. An oversize (or empty) batch is
                        // skipped WHOLESALE — same doctrine as the replay-guard
                        // skip (app.rs warn + continue) — the block is never
                        // aborted, and every correct node skips identically.
                        //
                        // LOCKSTEP-DEPLOY: this skip is a consensus-semantics
                        // change (a pre-upgrade node flattens+executes an
                        // oversize batch; an upgraded node skips it → divergent
                        // state root on a block that carries one). The whole
                        // fleet must run this before any block can legally carry
                        // a batch above the cap — same deploy class as the
                        // SessionScope::Trading widening (torus-types lib.rs).
                        if !torus_types::batch_len_within_cap(orders.len()) {
                            // Aggregate the count; do NOT log per batch. A
                            // malicious proposer can pack a block with thousands
                            // of tiny empty/oversize batches under the byte cap;
                            // per-batch WARN with %sender formatting would stall
                            // the single exec thread every block (log-DoS).
                            skipped_batches += 1;
                            continue;
                        }
                        for p in orders {
                            out.push((*sender, NativeAction::PlaceOrder(p.clone())));
                        }
                    }
                    other => out.push((*sender, other.clone())),
                }
            }
            if skipped_batches > 0 {
                tracing::warn!(
                    skipped_batches,
                    cap = torus_types::NATIVE_ORDERS_PER_BATCH_CAP,
                    "skipped oversize/empty PlaceOrderBatch action(s) at exec (deterministic cap)"
                );
            }
            std::borrow::Cow::Owned(out)
        } else {
            std::borrow::Cow::Borrowed(actions)
        };
        let actions: &[(Address, NativeAction)] = &flattened;

        let n = actions.len();
        let mut results: Vec<NativeActionResult> = (0..n)
            .map(|_| NativeActionResult::ok("pending", 0))
            .collect();
        let mut total_gas = 0u64;

        // ---- Phase 1: Partition and execute non-PlaceOrder actions ----
        let mut place_order_indices: Vec<usize> = Vec::new();

        for (i, (sender, action)) in actions.iter().enumerate() {
            match action {
                NativeAction::PlaceOrder(_) => place_order_indices.push(i),
                _ => {
                    let result = Self::execute(ctx, sender, action);
                    total_gas += result.gas_used;
                    results[i] = result;
                }
            }
        }

        if place_order_indices.is_empty() {
            return NativeBatchResult { results, total_gas };
        }

        // ---- Phase 2: Pre-reserve margin, assign IDs, partition by market ----
        let margin_timer = std::time::Instant::now();
        struct PreparedOrder {
            index: usize,
            sender: Address,
            params: PlaceOrderParams,
            order_id: u128,
            margin_reserved: FixedPoint,
        }

        let mut market_batches: HashMap<MarketId, Vec<PreparedOrder>> = HashMap::new();

        // O1: write-through balance cache, scoped to this execute_batch call. Serves
        // repeated Phase 2 reserve / Phase 4 release reads for the same sender without
        // re-hitting the overlay's lock + alloc + Borsh path.
        let mut bal_cache = BalanceCache::new();

        for &i in &place_order_indices {
            let (sender, action) = &actions[i];
            let params = match action {
                NativeAction::PlaceOrder(p) => p,
                _ => unreachable!(),
            };

            let market_id = params.market_id;
            let is_market = matches!(params.order_type, OrderType::Market);

            // Reserve margin (same logic as exec_place_order Phase 2).
            // A5: reserve and every later release share reserve_for_qty.
            let order_margin_required = if !is_market {
                Self::reserve_for_qty(ctx, market_id, params.price, params.quantity)
            } else {
                FixedPoint::ZERO
            };

            if order_margin_required > FixedPoint::ZERO {
                match bal_cache.load(&ctx.positions, sender) {
                    Ok(mut bal) => {
                        if bal.available < order_margin_required {
                            // Funnel (perf A1): died pre-book on the margin reserve.
                            if let Some(ref m) = ctx.metrics {
                                m.orders_rejected_margin.inc();
                            }
                            results[i] = NativeActionResult::err(
                                "place_order",
                                format!(
                                    "insufficient margin: need {order_margin_required}, have {}",
                                    bal.available
                                ),
                            );
                            continue;
                        }
                        bal.available -= order_margin_required;
                        bal.order_margin += order_margin_required;
                        bal_cache.set(sender, bal);
                    }
                    Err(e) => {
                        // Funnel (perf A1): died pre-book on a balance read error.
                        if let Some(ref m) = ctx.metrics {
                            m.orders_rejected_other.inc();
                        }
                        results[i] = NativeActionResult::err("place_order", e.to_string());
                        continue;
                    }
                }
            }

            // Assign global order ID (monotonic, pre-matching)
            let order_id = ctx.next_global_order_id;
            ctx.next_global_order_id += 1;

            market_batches
                .entry(market_id)
                .or_default()
                .push(PreparedOrder {
                    index: i,
                    sender: *sender,
                    params: params.clone(),
                    order_id,
                    margin_reserved: order_margin_required,
                });
        }

        if let Some(ref m) = ctx.metrics {
            m.exec_phase_margin_seconds
                .observe(margin_timer.elapsed().as_secs_f64());
        }

        // ---- Phase 3: Parallel matching ----
        let match_timer = std::time::Instant::now();
        let mut worker_batches: HashMap<MarketId, (OrderBook, Vec<MatchRequest>)> = HashMap::new();

        for (&market_id, prepared) in &market_batches {
            let book = ctx
                .order_books
                .remove(&market_id)
                .unwrap_or_else(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE));

            let requests: Vec<MatchRequest> = prepared
                .iter()
                .map(|p| MatchRequest {
                    sender: p.sender,
                    params: p.params.clone(),
                    order_id: p.order_id,
                })
                .collect();

            worker_batches.insert(market_id, (book, requests));
        }

        let mut market_results = match MarketWorkerPool::match_parallel(worker_batches, ctx.timestamp)
        {
            Ok(r) => r,
            Err(panic) => {
                // T1.5 FAIL-STOP: the panicking worker consumed its market's
                // book (and this block's other books were consumed with it),
                // so settlement cannot proceed and the block must never be
                // applied. Latch the fatal on the context — the committer
                // halts the execution pipeline on it — and bail out loudly.
                tracing::error!(
                    market_id = panic.market_id,
                    message = %panic.message,
                    "market worker panicked — FATAL, block cannot be executed"
                );
                for &i in &place_order_indices {
                    results[i] = NativeActionResult::err(
                        "place_order",
                        "market worker panicked — block execution aborted".to_string(),
                    );
                }
                ctx.fatal_error = Some(format!(
                    "market worker panicked (market {}): {}",
                    panic.market_id, panic.message
                ));
                return NativeBatchResult { results, total_gas };
            }
        };
        if let Some(ref m) = ctx.metrics {
            m.exec_phase_match_seconds
                .observe(match_timer.elapsed().as_secs_f64());
        }

        // ---- Phase 4: Sequential settlement ----
        // A5: settle markets in market-id order. `match_parallel` returns
        // HashMap iteration order (random per instance) — balance mutations
        // are commutative so consensus state never depended on it, but the
        // per-block trade_index assignment (node-local trade keys) and the
        // defensive `.min(order_margin)` clamps on the new cross-trader maker
        // releases do observe settlement order. Sorting pins both.
        market_results.sort_by_key(|m| m.market_id);
        let settle_timer = std::time::Instant::now();
        for mbr in market_results {
            let market_id = mbr.market_id;

            // Reinsert updated book
            ctx.order_books.insert(market_id, mbr.book);
            ctx.dirty_books.insert(market_id);

            // Update global ID high-water mark
            if mbr.next_order_id > ctx.next_global_order_id {
                ctx.next_global_order_id = mbr.next_order_id;
            }

            let prepared = match market_batches.get(&market_id) {
                Some(p) => p,
                None => continue,
            };

            for (match_result, prep) in mbr.results.iter().zip(prepared.iter()) {
                let result = &match_result.result;

                // Funnel (perf A1): STP maker cancels already happened in the
                // book during matching, regardless of how settlement below
                // turns out — count them here, not with the status outcome.
                if let Some(ref m) = ctx.metrics {
                    m.orders_self_trade_cancels
                        .inc_by(result.self_trade_cancels.len() as u64);
                }

                // Release margin for filled/cancelled portion
                if prep.margin_reserved > FixedPoint::ZERO {
                    let order_rests = matches!(
                        result.status,
                        OrderStatus::Resting
                            | OrderStatus::PartiallyFilled
                            | OrderStatus::PendingTrigger
                    );
                    let filled_qty: FixedPoint = result
                        .fills
                        .iter()
                        .map(|f| f.quantity)
                        .fold(FixedPoint::ZERO, |a, b| a + b);

                    let margin_to_release = if !order_rests {
                        prep.margin_reserved
                    } else if filled_qty > FixedPoint::ZERO {
                        // A5: telescoping release — reserved minus the reserve
                        // still owed for the resting remainder, so the later
                        // releases of that remainder (maker fills / cancel)
                        // sum to EXACTLY the original reservation (no dust).
                        prep.margin_reserved
                            - Self::reserve_for_qty(
                                ctx,
                                market_id,
                                prep.params.price,
                                prep.params.quantity - filled_qty,
                            )
                    } else {
                        FixedPoint::ZERO
                    };

                    if margin_to_release > FixedPoint::ZERO {
                        if let Ok(mut bal) = bal_cache.load(&ctx.positions, &prep.sender) {
                            let release = margin_to_release.min(bal.order_margin);
                            bal.order_margin -= release;
                            bal.available += release;
                            bal_cache.set(&prep.sender, bal);
                        }
                    }
                }

                // Apply fills to position manager
                let mut fill_failed = false;
                for fill in &result.fills {
                    // apply_fill credits realized PnL straight to the overlay, bypassing
                    // bal_cache. Flush+evict these traders' pending balances first so the
                    // credit lands on the post-release balance and flush_all can't clobber
                    // it; a later margin release for them re-reads the overlay (O1 coherence).
                    let _ = bal_cache.flush_and_evict(&ctx.positions, &fill.taker);
                    let _ = bal_cache.flush_and_evict(&ctx.positions, &fill.maker);
                    let taker_is_buy = fill.maker_side != Side::Buy;
                    if let Err(e) = ctx.positions.apply_fill(
                        &fill.taker,
                        market_id,
                        taker_is_buy,
                        fill.quantity,
                        fill.price,
                        MarginType::Cross,
                    ) {
                        results[prep.index] = NativeActionResult::err(
                            "place_order",
                            format!("taker fill failed: {e}"),
                        );
                        fill_failed = true;
                        break;
                    }
                    if let Err(e) = ctx.positions.apply_fill(
                        &fill.maker,
                        market_id,
                        fill.maker_side == Side::Buy,
                        fill.quantity,
                        fill.price,
                        MarginType::Cross,
                    ) {
                        results[prep.index] = NativeActionResult::err(
                            "place_order",
                            format!("maker fill failed: {e}"),
                        );
                        fill_failed = true;
                        break;
                    }
                }

                if fill_failed {
                    // Funnel (perf A1): died on fill application, not on the book.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    continue;
                }

                // Persist trades
                for fill in &result.fills {
                    Self::persist_trade(ctx, market_id, fill);
                }

                if let Some(ref m) = ctx.metrics {
                    m.orders_matched.inc_by(result.fills.len() as u64);
                    Self::record_order_status_funnel(m, &result.status, result.fills.len());
                }

                total_gas += 1000;
                results[prep.index] = NativeActionResult::ok("place_order", 1000);
            }

            // A5 (maker-fill margin leak): release maker-side order margin for
            // resting orders consumed by this batch's fills, and the full
            // remaining reservation of makers STP-cancelled during matching.
            // Aggregated once per market across ALL results (a maker can be
            // consumed by several takers); amounts telescope on remaining
            // quantity so total released == total reserved (see
            // maker_margin_releases). Clamped so legacy state can't underflow.
            for (trader, amount) in
                Self::maker_margin_releases(ctx, market_id, mbr.results.iter().map(|m| &m.result))
            {
                if let Ok(mut bal) = bal_cache.load(&ctx.positions, &trader) {
                    let release = amount.min(bal.order_margin);
                    bal.order_margin -= release;
                    bal.available += release;
                    bal_cache.set(&trader, bal);
                }
            }
        }

        // O1: materialize all deferred balance mutations (reserves + releases) into the
        // overlay before the call returns, so the next execute_batch call and every
        // post-batch consumer sees authoritative state.
        let _ = bal_cache.flush_all(&ctx.positions);

        if let Some(ref m) = ctx.metrics {
            m.exec_phase_settle_seconds
                .observe(settle_timer.elapsed().as_secs_f64());
        }

        NativeBatchResult { results, total_gas }
    }

    // ========================================================================
    // Order book handlers
    // ========================================================================

    /// Funnel-truth (perf A1): classify one matching-engine outcome into the
    /// order-funnel counters. Called exactly once per PlaceOrder that reached
    /// the book AND settled (margin rejects, balance errors and fill-application
    /// failures are counted at their own sites as `orders_rejected_margin` /
    /// `orders_rejected_other`). Observability only: nothing in execution reads
    /// these counters back.
    fn record_order_status_funnel(
        m: &torus_telemetry::Metrics,
        status: &OrderStatus,
        fill_count: usize,
    ) {
        match status {
            OrderStatus::Filled => {
                m.orders_placed_accepted.inc();
            }
            OrderStatus::Resting | OrderStatus::PartiallyFilled => {
                m.orders_placed_accepted.inc();
                m.orders_resting.inc();
            }
            OrderStatus::Rejected => {
                m.orders_rejected_book.inc();
            }
            OrderStatus::Cancelled => {
                if fill_count > 0 {
                    // IOC/Market remainder cancelled after real fills — the
                    // filled part DID trade, never count it as a dead order.
                    m.orders_cancelled_partial_fill.inc();
                } else {
                    m.orders_rejected_cancelled.inc();
                }
            }
            // Stop order queued for trigger: neither accepted onto the book
            // nor dead — it re-enters the matching funnel when triggered.
            OrderStatus::PendingTrigger => {}
        }
    }

    /// A5 (maker-fill margin leak): THE reserve/release formula.
    ///
    /// Margin reserved for `qty` of a limit order at `price` in `market_id`:
    /// `notional / effective_max_leverage(notional)` (default 20x), FixedPoint
    /// truncating arithmetic — byte-identical to the Phase-2 reserve.
    ///
    /// Exactness invariant: an order resting with remaining quantity `r` has
    /// outstanding reservation EXACTLY `reserve(price, r)`. Every release is
    /// computed as a difference of this function at two quantities
    /// (telescoping), so over an order's whole lifetime
    /// Σ releases == the original reservation — no truncation dust stranded
    /// in `order_margin`, no over-release into other orders' reservations.
    fn reserve_for_qty<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        market_id: MarketId,
        price: FixedPoint,
        qty: FixedPoint,
    ) -> FixedPoint {
        if price <= FixedPoint::ZERO || qty <= FixedPoint::ZERO {
            return FixedPoint::ZERO;
        }
        let notional = price * qty;
        let max_lev = ctx
            .margin_configs
            .get(&market_id)
            .map(|c| effective_max_leverage(&c.tiers, notional))
            .unwrap_or(20);
        notional / FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE)
    }

    /// A5: margin releases owed to RESTING (maker) orders that were consumed
    /// by fills or STP-cancelled during matching of `results` in `market_id`.
    /// Returns deterministic `(trader, amount)` pairs (BTreeMap order-id order,
    /// then STP order-id order).
    ///
    /// Per maker order the release telescopes over remaining quantity:
    /// `reserve(r_before_fills) - reserve(r_after_fills)`, where the
    /// post-fill remainder comes from the book (still resting), the captured
    /// STP-cancelled order (remaining at cancel time), or zero (fully filled).
    /// STP-cancelled makers additionally release `reserve(remaining_at_cancel)`
    /// — their whole leftover reservation. Combined with the taker-side and
    /// cancel-path releases this makes total released == total reserved.
    ///
    /// MUST be called ONCE per market over ALL of the batch's results: fills
    /// from several takers consuming one maker order have to be aggregated
    /// before comparing against the book's final remaining quantity.
    fn maker_margin_releases<'a, T: StateBackend>(
        ctx: &NativeExecContext<T>,
        market_id: MarketId,
        results: impl Iterator<Item = &'a PlaceResult>,
    ) -> Vec<(Address, FixedPoint)> {
        use std::collections::BTreeMap;

        // maker order id -> (maker, resting price, total qty consumed by fills)
        let mut consumed: BTreeMap<OrderId, (Address, FixedPoint, FixedPoint)> = BTreeMap::new();
        // STP-cancelled maker id -> (trader, price, remaining_qty at cancel)
        let mut stp: BTreeMap<OrderId, (Address, FixedPoint, FixedPoint)> = BTreeMap::new();

        for r in results {
            for f in &r.fills {
                let e = consumed
                    .entry(f.maker_order_id)
                    .or_insert((f.maker, f.price, FixedPoint::ZERO));
                e.2 += f.quantity;
            }
            for o in &r.self_trade_cancels {
                stp.insert(o.id, (o.trader, o.price, o.remaining_qty));
            }
        }
        if consumed.is_empty() && stp.is_empty() {
            return Vec::new();
        }

        let book = ctx.order_books.get(&market_id);
        let mut out = Vec::with_capacity(consumed.len() + stp.len());

        for (&order_id, &(maker, price, qty_consumed)) in &consumed {
            // Remaining AFTER all of this batch's fills on the order: still on
            // the book, or captured at STP-cancel time, or 0 (fully filled).
            let remaining_after = book
                .and_then(|b| b.get_order(order_id))
                .map(|o| o.remaining_qty)
                .or_else(|| stp.get(&order_id).map(|&(_, _, rem)| rem))
                .unwrap_or(FixedPoint::ZERO);
            let release =
                Self::reserve_for_qty(ctx, market_id, price, remaining_after + qty_consumed)
                    - Self::reserve_for_qty(ctx, market_id, price, remaining_after);
            if release > FixedPoint::ZERO {
                out.push((maker, release));
            }
        }
        for (&_order_id, &(trader, price, remaining)) in &stp {
            // Full leftover reservation of the STP-cancelled maker (its fills
            // earlier in the batch, if any, are covered by the pass above).
            let release = Self::reserve_for_qty(ctx, market_id, price, remaining);
            if release > FixedPoint::ZERO {
                out.push((trader, release));
            }
        }
        out
    }

    fn exec_place_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        params: &PlaceOrderParams,
    ) -> NativeActionResult {
        let market_id = params.market_id;
        let is_market = matches!(params.order_type, OrderType::Market);

        // FIX 2 (ECON-FIND-05): Reserve order margin before placing the order.
        // For market orders, margin is settled at fill time (no resting order).
        // A5: reserve and every later release share reserve_for_qty.
        let order_margin_required = if !is_market {
            Self::reserve_for_qty(ctx, market_id, params.price, params.quantity)
        } else {
            FixedPoint::ZERO
        };

        if order_margin_required > FixedPoint::ZERO {
            match ctx.positions.get_native_balance(sender) {
                Ok(mut bal) => {
                    if bal.available < order_margin_required {
                        // Funnel (perf A1): died pre-book on the margin reserve.
                        if let Some(ref m) = ctx.metrics {
                            m.orders_rejected_margin.inc();
                        }
                        return NativeActionResult::err(
                            "place_order",
                            format!(
                                "insufficient margin: need {order_margin_required}, have {}",
                                bal.available
                            ),
                        );
                    }
                    bal.available -= order_margin_required;
                    bal.order_margin += order_margin_required;
                    if let Err(e) = ctx.positions.put_native_balance(sender, &bal) {
                        // Funnel (perf A1): died pre-book on a balance write error.
                        if let Some(ref m) = ctx.metrics {
                            m.orders_rejected_other.inc();
                        }
                        return NativeActionResult::err("place_order", e.to_string());
                    }
                }
                Err(e) => {
                    // Funnel (perf A1): died pre-book on a balance read error.
                    if let Some(ref m) = ctx.metrics {
                        m.orders_rejected_other.inc();
                    }
                    return NativeActionResult::err("place_order", e.to_string());
                }
            }
        }

        // Get or create order book for this market.
        ctx.dirty_books.insert(market_id);
        let book = ctx
            .order_books
            .entry(market_id)
            .or_insert_with(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE));

        // FIX 6 (ECON-FIND-09): Sync global order ID counter to prevent cross-market collisions.
        book.set_next_order_id(ctx.next_global_order_id);
        let result = book.place_order(params.clone(), *sender, ctx.timestamp);
        ctx.next_global_order_id = book.next_order_id();

        // Funnel (perf A1): STP maker cancels already happened in the book,
        // regardless of how settlement below turns out.
        if let Some(ref m) = ctx.metrics {
            m.orders_self_trade_cancels
                .inc_by(result.self_trade_cancels.len() as u64);
        }

        // FIX 2: Release margin for the filled portion; keep reserved for resting.
        if order_margin_required > FixedPoint::ZERO {
            let order_rests = matches!(
                result.status,
                OrderStatus::Resting | OrderStatus::PartiallyFilled | OrderStatus::PendingTrigger
            );
            let filled_qty: FixedPoint = result
                .fills
                .iter()
                .map(|f| f.quantity)
                .fold(FixedPoint::ZERO, |a, b| a + b);

            let margin_to_release = if !order_rests {
                // Fully filled, cancelled, or rejected — release all reserved margin
                order_margin_required
            } else if filled_qty > FixedPoint::ZERO {
                // A5: telescoping release — reserved minus the reserve still
                // owed for the resting remainder, so the later releases of that
                // remainder (maker fills / cancel) sum to EXACTLY the original
                // reservation (no truncation dust).
                order_margin_required
                    - Self::reserve_for_qty(
                        ctx,
                        market_id,
                        params.price,
                        params.quantity - filled_qty,
                    )
            } else {
                FixedPoint::ZERO
            };

            if margin_to_release > FixedPoint::ZERO {
                if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
                    let release = margin_to_release.min(bal.order_margin);
                    bal.order_margin -= release;
                    bal.available += release;
                    let _ = ctx.positions.put_native_balance(sender, &bal);
                }
            }
        }

        // A5 (maker-fill margin leak): release maker-side order margin consumed
        // by this order's fills, and the full remaining reservation of makers
        // STP-cancelled during matching — mirrors execute_batch Phase 4.
        for (trader, amount) in
            Self::maker_margin_releases(ctx, market_id, std::iter::once(&result))
        {
            if let Ok(mut bal) = ctx.positions.get_native_balance(&trader) {
                let release = amount.min(bal.order_margin);
                bal.order_margin -= release;
                bal.available += release;
                let _ = ctx.positions.put_native_balance(&trader, &bal);
            }
        }

        // Apply fills to position manager.
        // FIX 22 (ECON-FIND-23): Propagate fill errors instead of discarding them.
        for fill in &result.fills {
            let taker_is_buy = fill.maker_side != Side::Buy;
            if let Err(e) = ctx.positions.apply_fill(
                &fill.taker,
                market_id,
                taker_is_buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            ) {
                // Funnel (perf A1): died on fill application, not on the book.
                if let Some(ref m) = ctx.metrics {
                    m.orders_rejected_other.inc();
                }
                return NativeActionResult::err("place_order", format!("taker fill failed: {e}"));
            }
            if let Err(e) = ctx.positions.apply_fill(
                &fill.maker,
                market_id,
                fill.maker_side == Side::Buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            ) {
                // Funnel (perf A1): died on fill application, not on the book.
                if let Some(ref m) = ctx.metrics {
                    m.orders_rejected_other.inc();
                }
                return NativeActionResult::err("place_order", format!("maker fill failed: {e}"));
            }
        }

        // Persist trades to CF_NATIVE_TRADES and CF_NATIVE_USER_TRADES.
        // These CFs are NOT in the state root, so writes cannot affect consensus.
        for fill in &result.fills {
            Self::persist_trade(ctx, market_id, fill);
        }

        if let Some(ref m) = ctx.metrics {
            m.orders_matched.inc_by(result.fills.len() as u64);
            Self::record_order_status_funnel(m, &result.status, result.fills.len());
        }

        NativeActionResult::ok("place_order", 1000)
    }

    /// Persist a single fill to CF_NATIVE_TRADES and CF_NATIVE_USER_TRADES —
    /// inline, or buffered for a background writer when `ctx.defer_trades`
    /// (O3; both CFs are node-local, outside the native consensus root).
    fn persist_trade<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        market_id: MarketId,
        fill: &torus_core::order_book::Fill,
    ) {
        let taker_side: u8 = if fill.maker_side == Side::Buy { 1 } else { 0 };

        // Primary key: market_id(8) + block_number(8) + trade_index(4)
        let mut trade_key = [0u8; 20];
        trade_key[..8].copy_from_slice(&market_id.to_be_bytes());
        trade_key[8..16].copy_from_slice(&ctx.block_height.to_be_bytes());
        trade_key[16..20].copy_from_slice(&ctx.trade_index.to_be_bytes());

        let trade_id = ctx.trade_index as u128;
        let price_raw = fill.price.raw();
        let quantity_raw = fill.quantity.raw();

        // Borsh-serialize trade data (matches StoredTrade layout)
        let mut trade_data = Vec::with_capacity(64);
        trade_data.extend_from_slice(&trade_id.to_le_bytes());
        trade_data.extend_from_slice(&price_raw.to_le_bytes());
        trade_data.extend_from_slice(&quantity_raw.to_le_bytes());
        trade_data.push(taker_side);
        trade_data.extend_from_slice(&ctx.block_height.to_le_bytes());
        trade_data.extend_from_slice(&ctx.timestamp.to_le_bytes());

        // Secondary index: per-user trades (descending block order)
        let desc_block = u64::MAX - ctx.block_height;
        let mut maker_data = Vec::with_capacity(80);
        maker_data.extend_from_slice(&trade_id.to_le_bytes());
        maker_data.extend_from_slice(&market_id.to_le_bytes());
        maker_data.extend_from_slice(&price_raw.to_le_bytes());
        maker_data.extend_from_slice(&quantity_raw.to_le_bytes());
        maker_data.push(taker_side);
        maker_data.push(0u8); // role: maker
        maker_data.extend_from_slice(&ctx.block_height.to_le_bytes());
        maker_data.extend_from_slice(&ctx.timestamp.to_le_bytes());

        let mut maker_key = [0u8; 32];
        maker_key[..20].copy_from_slice(fill.maker.as_slice());
        maker_key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        maker_key[28..32].copy_from_slice(&ctx.trade_index.to_be_bytes());

        // Taker entry (flip role byte at offset 57: 16+8+16+16+1)
        let mut taker_data = maker_data.clone();
        taker_data[57] = 1u8; // role: taker
        let mut taker_key = [0u8; 32];
        taker_key[..20].copy_from_slice(fill.taker.as_slice());
        taker_key[20..28].copy_from_slice(&desc_block.to_be_bytes());
        taker_key[28..32].copy_from_slice(&ctx.trade_index.to_be_bytes());

        if ctx.defer_trades {
            ctx.pending_trades
                .push((CF_NATIVE_TRADES, trade_key.to_vec(), trade_data));
            ctx.pending_trades
                .push((CF_NATIVE_USER_TRADES, maker_key.to_vec(), maker_data));
            ctx.pending_trades
                .push((CF_NATIVE_USER_TRADES, taker_key.to_vec(), taker_data));
        } else {
            let _ = ctx
                .state
                .put_cf_raw(CF_NATIVE_TRADES, &trade_key, &trade_data);
            let _ = ctx
                .state
                .put_cf_raw(CF_NATIVE_USER_TRADES, &maker_key, &maker_data);
            let _ = ctx
                .state
                .put_cf_raw(CF_NATIVE_USER_TRADES, &taker_key, &taker_data);
        }

        ctx.trade_index += 1;
    }

    /// FIX CONS-FIND-30: Ownership check added -- only the order's trader can cancel.
    fn exec_cancel_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        order_id: u128,
    ) -> NativeActionResult {
        // Check ownership before cancelling (cheaper than cancel + re-insert).
        for book in ctx.order_books.values() {
            if let Some(order) = book.get_order(order_id) {
                if order.trader != *sender {
                    return NativeActionResult::err(
                        "cancel_order",
                        format!(
                            "order {order_id} belongs to {}, not sender {sender}",
                            order.trader
                        ),
                    );
                }
                break;
            }
        }
        for book in ctx.order_books.values_mut() {
            if let Ok(cancelled) = book.cancel_order(order_id) {
                // FIX 2 (ECON-FIND-05): Release order margin on cancel.
                let notional = cancelled.price * cancelled.remaining_qty;
                let market_id = book.market_id;
                ctx.dirty_books.insert(market_id);
                let max_lev = ctx
                    .margin_configs
                    .get(&market_id)
                    .map(|c| effective_max_leverage(&c.tiers, notional))
                    .unwrap_or(20);
                let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                let margin_to_release = notional / lev_fp;

                if margin_to_release > FixedPoint::ZERO {
                    if let Ok(mut bal) = ctx.positions.get_native_balance(&cancelled.trader) {
                        let release = margin_to_release.min(bal.order_margin);
                        bal.order_margin -= release;
                        bal.available += release;
                        let _ = ctx.positions.put_native_balance(&cancelled.trader, &bal);
                    }
                }
                return NativeActionResult::ok("cancel_order", 500);
            }
        }
        NativeActionResult::err("cancel_order", format!("order {order_id} not found"))
    }

    fn exec_cancel_all<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        market_id: Option<MarketId>,
    ) -> NativeActionResult {
        // FIX 2 (ECON-FIND-05): Compute total margin to release from cancelled orders.
        let mut total_margin_release = FixedPoint::ZERO;

        match market_id {
            Some(mid) => {
                if let Some(book) = ctx.order_books.get_mut(&mid) {
                    let cancelled = book.cancel_all(*sender, Some(mid));
                    if !cancelled.is_empty() {
                        ctx.dirty_books.insert(mid);
                    }
                    for order in &cancelled {
                        let notional = order.price * order.remaining_qty;
                        let max_lev = ctx
                            .margin_configs
                            .get(&mid)
                            .map(|c| effective_max_leverage(&c.tiers, notional))
                            .unwrap_or(20);
                        let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                        total_margin_release += notional / lev_fp;
                    }
                }
            }
            None => {
                let market_ids: Vec<MarketId> = ctx.order_books.keys().copied().collect();
                for mid in market_ids {
                    if let Some(book) = ctx.order_books.get_mut(&mid) {
                        let cancelled = book.cancel_all(*sender, None);
                        if !cancelled.is_empty() {
                            ctx.dirty_books.insert(mid);
                        }
                        for order in &cancelled {
                            let notional = order.price * order.remaining_qty;
                            let max_lev = ctx
                                .margin_configs
                                .get(&mid)
                                .map(|c| effective_max_leverage(&c.tiers, notional))
                                .unwrap_or(20);
                            let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                            total_margin_release += notional / lev_fp;
                        }
                    }
                }
            }
        }

        if total_margin_release > FixedPoint::ZERO {
            if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
                let release = total_margin_release.min(bal.order_margin);
                bal.order_margin -= release;
                bal.available += release;
                let _ = ctx.positions.put_native_balance(sender, &bal);
            }
        }

        NativeActionResult::ok("cancel_all", 500)
    }

    fn exec_modify_order<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        order_id: u128,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    ) -> NativeActionResult {
        for book in ctx.order_books.values_mut() {
            // Capture old order state for margin delta calculation.
            let old_order = book.get_order(order_id).cloned();
            if let Ok(modified) = book.modify_order(order_id, new_price, new_qty) {
                ctx.dirty_books.insert(book.market_id);
                // FIX 2 (ECON-FIND-05): Adjust order margin for the modified order.
                if let Some(old) = old_order {
                    let market_id = book.market_id;
                    let old_notional = old.price * old.remaining_qty;
                    let new_notional = modified.price * modified.remaining_qty;

                    let max_lev_old = ctx
                        .margin_configs
                        .get(&market_id)
                        .map(|c| effective_max_leverage(&c.tiers, old_notional))
                        .unwrap_or(20);
                    let max_lev_new = ctx
                        .margin_configs
                        .get(&market_id)
                        .map(|c| effective_max_leverage(&c.tiers, new_notional))
                        .unwrap_or(20);

                    let old_margin = old_notional
                        / FixedPoint::from_raw(max_lev_old as i128 * FixedPoint::SCALE);
                    let new_margin = new_notional
                        / FixedPoint::from_raw(max_lev_new as i128 * FixedPoint::SCALE);

                    if new_margin > old_margin {
                        let delta = new_margin - old_margin;
                        if let Ok(mut bal) = ctx.positions.get_native_balance(&modified.trader) {
                            if bal.available < delta {
                                return NativeActionResult::err(
                                    "modify_order",
                                    format!(
                                        "insufficient margin for modify: need {delta}, have {}",
                                        bal.available
                                    ),
                                );
                            }
                            bal.available -= delta;
                            bal.order_margin += delta;
                            let _ = ctx.positions.put_native_balance(&modified.trader, &bal);
                        }
                    } else if old_margin > new_margin {
                        let delta = old_margin - new_margin;
                        if let Ok(mut bal) = ctx.positions.get_native_balance(&modified.trader) {
                            let release = delta.min(bal.order_margin);
                            bal.order_margin -= release;
                            bal.available += release;
                            let _ = ctx.positions.put_native_balance(&modified.trader, &bal);
                        }
                    }
                }
                return NativeActionResult::ok("modify_order", 800);
            }
        }
        NativeActionResult::err("modify_order", format!("order {order_id} not found"))
    }

    // ========================================================================
    // Staking handlers
    // ========================================================================

    fn exec_delegate<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        validator: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx.staking.delegate(*sender, *validator, amount) {
            Ok(()) => NativeActionResult::ok("delegate", 2000),
            Err(e) => NativeActionResult::err("delegate", e.to_string()),
        }
    }

    fn exec_undelegate<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        validator: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx
            .staking
            .undelegate(*sender, *validator, amount, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("undelegate", 2000),
            Err(e) => NativeActionResult::err("undelegate", e.to_string()),
        }
    }

    fn exec_permanent_stake<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx
            .staking
            .permanent_stake(*sender, amount, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("permanent_stake", 2000),
            Err(e) => NativeActionResult::err("permanent_stake", e.to_string()),
        }
    }

    fn exec_claim_rewards<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.claim_rewards(*sender) {
            Ok(_) => NativeActionResult::ok("claim_rewards", 1500),
            Err(e) => NativeActionResult::err("claim_rewards", e.to_string()),
        }
    }

    fn exec_jail_vote<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        target: &Address,
    ) -> NativeActionResult {
        match ctx
            .staking
            .record_jail_vote(*sender, *target, ctx.block_height)
        {
            Ok(jailed) => {
                let gas = if jailed { 5000 } else { 2000 };
                NativeActionResult::ok("jail_vote", gas)
            }
            Err(e) => NativeActionResult::err("jail_vote", e.to_string()),
        }
    }

    fn exec_unjail_self<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
    ) -> NativeActionResult {
        match ctx.staking.unjail(sender, ctx.block_height) {
            Ok(()) => NativeActionResult::ok("unjail_self", 2000),
            Err(e) => NativeActionResult::err("unjail_self", e.to_string()),
        }
    }

    fn exec_register_validator<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        pubkey: &torus_types::PublicKey,
        commission: u16,
    ) -> NativeActionResult {
        // Phase 3 (3.2.1): Check governance whitelist before registration
        match ctx.governance.is_whitelisted(sender, ctx.block_height) {
            Ok(true) => {}
            Ok(false) => {
                return NativeActionResult::err(
                    "register_validator",
                    format!("validator registration not approved by governance for {sender}"),
                );
            }
            Err(e) => return NativeActionResult::err("register_validator", e.to_string()),
        }

        // Determine self-stake: sender's full balance is used as self-stake
        let self_stake = match ctx.state.get_account(sender) {
            Ok(Some(acct)) => acct.balance,
            Ok(None) => {
                return NativeActionResult::err(
                    "register_validator",
                    "sender account not found".to_string(),
                );
            }
            Err(e) => return NativeActionResult::err("register_validator", e.to_string()),
        };

        match ctx
            .staking
            .register_validator(*sender, pubkey.0, commission, self_stake)
        {
            Ok(()) => {
                // FIX ECON-FIND-21: Whitelist consume failure must fail the registration
                // to prevent a validator from registering without consuming their whitelist slot.
                if let Err(e) = ctx.governance.consume_whitelist(sender) {
                    tracing::error!(%sender, %e, "whitelist consumption failed after registration");
                    return NativeActionResult::err(
                        "register_validator",
                        format!("registered but whitelist error: {e}"),
                    );
                }
                NativeActionResult::ok("register_validator", 5000)
            }
            Err(e) => NativeActionResult::err("register_validator", e.to_string()),
        }
    }

    fn exec_update_commission<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        new_rate: u16,
    ) -> NativeActionResult {
        match ctx
            .staking
            .update_commission(*sender, new_rate, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("update_commission", 2000),
            Err(e) => NativeActionResult::err("update_commission", e.to_string()),
        }
    }

    fn exec_rotate_key<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        new_pubkey: &torus_types::PublicKey,
    ) -> NativeActionResult {
        let current_epoch = ctx.block_height / ctx.epoch_length.max(1);
        match ctx.staking.submit_key_rotation(
            *sender,
            new_pubkey.0,
            current_epoch,
            ctx.block_height,
        ) {
            Ok(()) => NativeActionResult::ok("rotate_validator_key", 5000),
            Err(e) => NativeActionResult::err("rotate_validator_key", e.to_string()),
        }
    }

    // ========================================================================
    // Session key handlers
    // ========================================================================

    fn exec_create_session<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        session_pubkey: &[u8; 32],
        expiry: u64,
        scope: SessionScope,
    ) -> NativeActionResult {
        use torus_types::eip712::{MAX_SESSIONS_PER_ADDRESS, MAX_SESSION_EXPIRY_MS};

        // The block timestamp is SECONDS (header.timestamp = .as_secs()), but
        // `expiry`, MAX_SESSION_EXPIRY_MS, and order-time validation (resolve_sender
        // against current_time_ms) are all MILLISECONDS. Convert to ms here so
        // creation and usage agree — otherwise no expiry satisfies both gates and
        // every session is unusable (rejected "expiry exceeds 24h maximum" on create,
        // so orders later fail "session key not found").
        let now_ms = ctx.timestamp.saturating_mul(1000);
        if expiry > now_ms + MAX_SESSION_EXPIRY_MS {
            return NativeActionResult::err("create_session", "expiry exceeds 24h maximum".into());
        }
        if expiry <= now_ms {
            return NativeActionResult::err("create_session", "session already expired".into());
        }

        // Check max sessions per owner
        match ctx.state.count_sessions_for_owner(sender) {
            Ok(count) if count >= MAX_SESSIONS_PER_ADDRESS => {
                return NativeActionResult::err(
                    "create_session",
                    "max 5 active sessions exceeded".into(),
                );
            }
            Err(e) => {
                return NativeActionResult::err("create_session", format!("state error: {e}"));
            }
            _ => {}
        }

        // Check if session key already exists
        match ctx.state.get_session(session_pubkey) {
            Ok(Some(_)) => {
                return NativeActionResult::err(
                    "create_session",
                    "session key already exists".into(),
                );
            }
            Err(e) => {
                return NativeActionResult::err("create_session", format!("state error: {e}"));
            }
            _ => {}
        }

        // Store the session
        let data = torus_types::SessionData {
            owner: *sender,
            expiry,
            scope,
            created_at: now_ms,
        };
        match ctx.state.put_session(session_pubkey, &data) {
            Ok(()) => NativeActionResult::ok("create_session", 5000),
            Err(e) => NativeActionResult::err("create_session", format!("store failed: {e}")),
        }
    }

    fn exec_revoke_session<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        session_pubkey: &[u8; 32],
    ) -> NativeActionResult {
        // Verify session exists and sender is the owner
        match ctx.state.get_session(session_pubkey) {
            Ok(Some(data)) => {
                if data.owner != *sender {
                    return NativeActionResult::err("revoke_session", "not session owner".into());
                }
            }
            Ok(None) => {
                return NativeActionResult::err("revoke_session", "session not found".into());
            }
            Err(e) => {
                return NativeActionResult::err("revoke_session", format!("state error: {e}"));
            }
        }

        match ctx.state.delete_session(session_pubkey) {
            Ok(()) => NativeActionResult::ok("revoke_session", 2000),
            Err(e) => NativeActionResult::err("revoke_session", format!("delete failed: {e}")),
        }
    }

    // ========================================================================
    // Oracle handlers
    // ========================================================================

    fn exec_submit_oracle_prices<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        prices: &[(MarketId, FixedPoint)],
        _submission_timestamp: u64,
    ) -> NativeActionResult {
        // FIX 18 (ECON-FIND-19): Verify sender is an active, non-jailed validator.
        match ctx.staking.get_validator(sender) {
            Ok(Some(v)) => {
                use torus_economics::types::ValidatorStatus;
                if v.status != ValidatorStatus::Active {
                    return NativeActionResult::err(
                        "submit_oracle_prices",
                        format!("validator {sender} is not active (status: {:?})", v.status),
                    );
                }
            }
            Ok(None) => {
                return NativeActionResult::err(
                    "submit_oracle_prices",
                    format!("{sender} is not a registered validator"),
                );
            }
            Err(e) => {
                return NativeActionResult::err("submit_oracle_prices", e.to_string());
            }
        }

        for &(market_id, price) in prices {
            if let Err(e) =
                ctx.oracle
                    .submit_price(sender, market_id, price, ctx.block_height, ctx.timestamp)
            {
                return NativeActionResult::err("submit_oracle_prices", e.to_string());
            }
        }
        NativeActionResult::ok("submit_oracle_prices", 1000)
    }

    // ========================================================================
    // Governance handlers
    // ========================================================================

    fn exec_submit_proposal<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        proposal: &torus_types::Proposal,
    ) -> NativeActionResult {
        // Convert torus_types::ProposalAction to torus_economics::governance::ExecutionPayload
        use torus_economics::governance::ExecutionPayload;
        use torus_types::ProposalAction;

        let execution_payload = match &proposal.action {
            ProposalAction::ParameterChange { key, value } => {
                Some(ExecutionPayload::ParameterChange {
                    param_key: key.clone(),
                    new_value: value.clone(),
                })
            }
            ProposalAction::ListMarket(listing) => Some(ExecutionPayload::MarketListing {
                market_id: 0, // auto-assigned
                base_asset: listing.base_asset.clone(),
                quote_asset: listing.quote_asset.clone(),
                lot_size: listing.lot_size,
                tick_size: listing.tick_size,
                initial_margin: torus_types::FixedPoint::from_raw(
                    listing.maintenance_margin_bps as i128 * torus_types::FixedPoint::SCALE / 10000,
                ),
            }),
            ProposalAction::UpdateMarketParams { .. } | ProposalAction::DelistMarket { .. } => {
                None // text-only for now
            }
            ProposalAction::ValidatorRegistration { candidate } => {
                Some(ExecutionPayload::ValidatorRegistration {
                    candidate: *candidate,
                })
            }
        };

        match ctx.governance.submit_proposal(
            *sender,
            proposal.title.clone(),
            proposal.description.clone(),
            execution_payload,
            ctx.block_height,
        ) {
            Ok(_id) => NativeActionResult::ok("submit_proposal", 5000),
            Err(e) => NativeActionResult::err("submit_proposal", e.to_string()),
        }
    }

    fn exec_vote<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        proposal_id: u64,
        option: VoteOption,
    ) -> NativeActionResult {
        // FIX 20 (ECON-PF-03): Abstain votes are handled separately so they
        // contribute to quorum without affecting the yes/no tally.
        match option {
            VoteOption::Abstain => {
                match ctx
                    .governance
                    .cast_vote_abstain(*sender, proposal_id, ctx.block_height)
                {
                    Ok(()) => NativeActionResult::ok("vote", 2000),
                    Err(e) => NativeActionResult::err("vote", e.to_string()),
                }
            }
            _ => {
                let support = matches!(option, VoteOption::Yes);
                match ctx
                    .governance
                    .cast_vote(*sender, proposal_id, support, ctx.block_height)
                {
                    Ok(()) => NativeActionResult::ok("vote", 2000),
                    Err(e) => NativeActionResult::err("vote", e.to_string()),
                }
            }
        }
    }

    // ========================================================================
    // Lockbox handlers
    // ========================================================================

    fn exec_deposit_to_native<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => return NativeActionResult::err("deposit_to_native", "amount overflow".into()),
        };
        match Lockbox::deposit_to_native(&ctx.state, sender, fp_amount) {
            Ok(()) => NativeActionResult::ok("deposit_to_native", 1500),
            Err(e) => NativeActionResult::err("deposit_to_native", e.to_string()),
        }
    }

    fn exec_withdraw_from_native<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => {
                return NativeActionResult::err("withdraw_from_native", "amount overflow".into())
            }
        };
        match Lockbox::withdraw_from_native(&ctx.state, sender, fp_amount) {
            Ok(()) => NativeActionResult::ok("withdraw_from_native", 1500),
            Err(e) => NativeActionResult::err("withdraw_from_native", e.to_string()),
        }
    }

    /// FIX ECON-PF-17: Withdraw from sender's native balance to a specified EVM address.
    fn exec_withdraw_to<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        sender: &Address,
        to: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => return NativeActionResult::err("withdraw_to", "amount overflow".into()),
        };
        match Lockbox::withdraw_from_native_to(&ctx.state, sender, to, fp_amount) {
            Ok(()) => NativeActionResult::ok("withdraw_to", 1500),
            Err(e) => NativeActionResult::err("withdraw_to", e.to_string()),
        }
    }

    // ========================================================================
    // Block-level processing helpers (called by validator pipeline)
    // ========================================================================

    /// Drain and execute CoreWriter actions queued from the previous block.
    ///
    /// Task 3.1.5 (anti-MEV): Asserts the one-block delay is enforced — all
    /// drained actions must have been queued in a strictly earlier block.
    ///
    /// FIX EVM-FIND-12: Propagates drain errors instead of silently returning empty vec.
    pub fn drain_core_writer<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Result<Vec<NativeActionResult>, torus_core::error::CoreError> {
        let queued = CoreWriterQueue::drain(&ctx.state, ctx.block_height)?;

        let mut results = Vec::with_capacity(queued.len());
        for qa in &queued {
            // BUG FIX (3.1): CoreWriter delay guard — skip any action that was
            // queued in the current or a future block (should never happen, but
            // guards against bypass bugs).
            if qa.block_queued >= ctx.block_height {
                tracing::error!(
                    queued_block = qa.block_queued,
                    current_block = ctx.block_height,
                    "BUG (3.1): CoreWriter action from current/future block — skipping"
                );
                results.push(NativeActionResult::err(
                    "core_writer",
                    format!(
                        "action queued in block {} cannot execute in block {}",
                        qa.block_queued, ctx.block_height
                    ),
                ));
                continue;
            }

            let action = core_writer_to_native(qa);
            let result = Self::execute(ctx, &qa.trader, &action);
            results.push(result);
        }
        Ok(results)
    }

    /// Aggregate oracle prices for listed markets after oracle submissions.
    pub fn aggregate_oracle_prices<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        markets: &[MarketId],
        validator_stakes: &[(Address, FixedPoint)],
    ) -> Vec<NativeActionResult> {
        let mut results = Vec::new();
        for &market_id in markets {
            match ctx
                .oracle
                .aggregate_price(market_id, ctx.block_height, validator_stakes)
            {
                Ok(_) => results.push(NativeActionResult::ok("oracle_aggregate", 500)),
                Err(e) => results.push(NativeActionResult::err("oracle_aggregate", e.to_string())),
            }
        }
        results
    }

    /// Run liquidation checks across all configured markets.
    pub fn run_liquidation_checks<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        traders: &[Address],
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Vec<NativeActionResult> {
        let mut results = Vec::new();

        // Iterate over a snapshot of config keys to avoid borrow conflict.
        let market_configs: Vec<(MarketId, MarketMarginConfig)> = ctx
            .margin_configs
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();

        for (market_id, config) in &market_configs {
            let liquidations = match LiquidationEngine::check_liquidations(
                &ctx.positions,
                traders,
                config,
                oracle_prices,
            ) {
                Ok(liqs) => liqs,
                Err(e) => {
                    results.push(NativeActionResult::err("liquidation_check", e.to_string()));
                    continue;
                }
            };

            // FIX 5 (ECON-PF-06): Skip liquidation if no valid oracle price.
            let oracle_price = match oracle_prices
                .iter()
                .find(|(mid, _)| *mid == *market_id)
                .map(|(_, p)| *p)
            {
                Some(p) if p > FixedPoint::ZERO => p,
                _ => {
                    tracing::warn!(market_id, "skipping liquidations: no valid oracle price");
                    continue;
                }
            };

            for liq in &liquidations {
                match LiquidationEngine::execute_liquidation(&ctx.positions, liq, oracle_price) {
                    Ok(lr) => {
                        if let Some(ref m) = ctx.metrics {
                            m.liquidations_triggered.inc();
                        }
                        results.push(NativeActionResult::ok("liquidation", 3000));
                        if lr.remaining_deficit > FixedPoint::ZERO {
                            let _ = LiquidationEngine::auto_deleverage(
                                &ctx.positions,
                                *market_id,
                                lr.remaining_deficit,
                                oracle_price,
                                traders,
                            );
                        }
                    }
                    Err(e) => {
                        results.push(NativeActionResult::err("liquidation", e.to_string()));
                    }
                }
            }
        }
        results
    }

    /// Distribute fees at end of block.
    pub fn distribute_fees<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        total_evm_fees: u128,
    ) -> NativeActionResult {
        let total_fees = U256::from(ctx.total_native_fees as u128 + total_evm_fees);
        if total_fees.is_zero() {
            return NativeActionResult::ok("fee_distribution", 0);
        }

        // FIX ECON-FIND-22: Removed dead FeeSplitter::split_fees call.
        // RewardDistributor::distribute_block_fees computes fee splits internally.

        match RewardDistributor::distribute_block_fees(
            &ctx.staking,
            ctx.proposer,
            total_fees,
            ctx.epoch,
            ctx.treasury_address,
            ctx.dev_pool_address,
        ) {
            Ok(()) => NativeActionResult::ok("fee_distribution", 2000),
            Err(e) => NativeActionResult::err("fee_distribution", e.to_string()),
        }
    }

    /// Check and process epoch boundary (reward distribution + validator rotation).
    ///
    /// Phase A — Rewards: distributes permanent staking rewards and validator
    /// inflation to the CURRENT active set. Errors log-and-continue (reward
    /// bugs should not block rotation).
    ///
    /// Phase B — Rotation: computes new validator set, applies rotation cap,
    /// updates statuses, logs changes. Errors on compute_new_validator_set
    /// early-return (broken validator set is a consensus-safety issue).
    pub fn process_epoch_boundary<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Option<EpochBoundaryResult> {
        if !EpochManager::is_epoch_boundary(ctx.block_height, ctx.epoch_length) {
            return None;
        }

        // --- Phase A: Reward distribution (uses CURRENT active set) ---

        if let Err(e) =
            RewardDistributor::distribute_permanent_staking_rewards(&ctx.staking, ctx.epoch_length)
        {
            tracing::error!(%e, "permanent staking rewards failed");
        }

        if let Err(e) =
            RewardDistributor::distribute_validator_inflation(&ctx.staking, ctx.epoch_length)
        {
            tracing::error!(%e, "validator inflation distribution failed");
        }

        // --- Phase B: Validator set rotation ---

        // B1: Build old set from current Active validators
        let old_set = build_current_validator_set(&ctx.staking, ctx.epoch);

        // B2: Compute new set
        let new_set = match EpochManager::compute_new_validator_set(
            &ctx.staking,
            ctx.max_validators,
            ctx.epoch + 1,
        ) {
            Ok(set) => set,
            Err(e) => {
                return Some(EpochBoundaryResult {
                    action: NativeActionResult::err("epoch_rotation", e.to_string()),
                    new_set: None,
                    diff: None,
                });
            }
        };

        // B3: Apply rotation cap
        let cap = EpochManager::safe_rotation_cap(old_set.validators.len());
        let new_set = EpochManager::apply_rotation_cap(&old_set, new_set, cap);

        // B3.5 (FIX 4, S443): enforce the BFT-minimum floor. If the capped rotation
        // would drop the active set below MIN_ACTIVE_VALIDATORS while the old set met
        // it, re-seat the highest-priority departed validator(s) so the cluster keeps
        // a viable quorum (t15: a wrongful deposition dropped 4 → 3 and stalled).
        let new_set = EpochManager::enforce_minimum_floor(&old_set, new_set);

        // B4: Check minimum set (post-condition; floor guard above should keep this
        // green whenever the old set met the minimum).
        if let Err(e) = EpochManager::check_minimum_set(&new_set) {
            tracing::error!(%e, "validator set below minimum");
        }

        // B5: Compute diff
        let diff = EpochManager::compute_validator_set_diff(&old_set, &new_set);

        // B6: Update statuses
        if let Err(e) = EpochManager::update_validator_statuses(&ctx.staking, &new_set) {
            tracing::error!(%e, "validator status update failed");
        }

        // B7: Log rotation
        EpochManager::log_rotation(&old_set, &new_set, &diff, ctx.epoch + 1);

        if let Some(ref m) = ctx.metrics {
            m.epoch_number.set((ctx.epoch + 1) as i64);
            m.validator_set_size.set(new_set.validators.len() as i64);
        }

        Some(EpochBoundaryResult {
            action: NativeActionResult::ok("epoch_boundary", 5000),
            new_set: Some(new_set),
            diff: Some(diff),
        })
    }

    /// Process pending governance proposals.
    pub fn process_governance<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
    ) -> Vec<NativeActionResult> {
        match ctx.governance.process_pending_proposals(ctx.block_height) {
            Ok(outcomes) => outcomes
                .iter()
                .map(|_| NativeActionResult::ok("governance_process", 1000))
                .collect(),
            Err(e) => vec![NativeActionResult::err("governance_process", e.to_string())],
        }
    }
}

/// Build a ValidatorSet from current Active validators in staking state.
/// Uses the same power conversion as `EpochManager::compute_new_validator_set`.
fn build_current_validator_set(
    staking: &StakingManager<impl StateBackend>,
    epoch: u64,
) -> ValidatorSet {
    let wei = U256::from(10u64).pow(U256::from(18u64));
    let validators: Vec<ValidatorInfo> = match staking.all_validators() {
        Ok(all) => all
            .into_iter()
            .filter(|v| v.status == ValidatorStatus::Active)
            .map(|v| ValidatorInfo {
                address: v.address,
                pubkey: PublicKey(v.pubkey),
                power: (v.total_stake() / wei).try_into().unwrap_or(u64::MAX),
                commission_bps: v.commission_bps,
            })
            .collect(),
        Err(e) => {
            tracing::error!(%e, "failed to read validators for old set");
            Vec::new()
        }
    };
    ValidatorSet { validators, epoch }
}

// ============================================================================
// Action classification and ordering (tech-req section 2.2)
// ============================================================================

/// Categories for deterministic block execution ordering.
///
/// Ordering per tech-req:
///   1. Native cancellations
///   2. Native non-GTC orders (IOC, FOK, market)
///   3. EVM transactions (not a native category — interleaved by caller)
///   4. Native GTC limit orders
///   5. CoreWriter drain (implicit, processed by validator)
///   6. Lockbox operations
///   7. Oracle submissions
///   8. Governance actions
///   9. Liquidation checks (implicit)
///   10. Fee distribution (implicit)
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionCategory {
    Cancellation = 0,
    NonGtcOrder = 1,
    GtcOrder = 2,
    Lockbox = 3,
    Oracle = 4,
    Governance = 5,
    Staking = 6,
    Other = 7,
}

/// Classify a NativeAction for block ordering.
pub fn classify_action(action: &NativeAction) -> ActionCategory {
    match action {
        NativeAction::CancelOrder { .. } | NativeAction::CancelAllOrders { .. } => {
            ActionCategory::Cancellation
        }
        NativeAction::PlaceOrder(params) => match params.order_type {
            OrderType::Market | OrderType::StopMarket { .. } => ActionCategory::NonGtcOrder,
            _ => match params.time_in_force {
                TimeInForce::GTC | TimeInForce::PostOnly => ActionCategory::GtcOrder,
                TimeInForce::IOC | TimeInForce::FOK => ActionCategory::NonGtcOrder,
            },
        },
        // A batch is a unit of GTC-class limit orders (the market-maker use case); it
        // sorts alongside other GTC orders into the post-EVM phase. Internal order is
        // preserved when `execute_batch` flattens it.
        NativeAction::PlaceOrderBatch(_) => ActionCategory::GtcOrder,
        NativeAction::ModifyOrder { .. } => ActionCategory::NonGtcOrder,
        NativeAction::TransferToPerp { .. }
        | NativeAction::TransferToSpot { .. }
        | NativeAction::Withdraw { .. } => ActionCategory::Lockbox,
        NativeAction::SubmitOraclePrices(_) => ActionCategory::Oracle,
        NativeAction::SubmitProposal(_) | NativeAction::Vote { .. } => ActionCategory::Governance,
        NativeAction::Delegate { .. }
        | NativeAction::Undelegate { .. }
        | NativeAction::PermanentStake { .. }
        | NativeAction::ClaimRewards => ActionCategory::Staking,
        _ => ActionCategory::Other,
    }
}

/// Sort native actions into pre-EVM and post-EVM groups per tech-req ordering.
///
/// Pre-EVM:  cancellations, non-GTC orders
/// Post-EVM: GTC orders, lockbox, oracle, governance, staking, other
///
/// Task 3.1.5 (anti-MEV): Within each category, actions are sorted by
/// (sender_address, action_content_hash) for determinism. This prevents a
/// validator-proposer from manipulating within-category ordering by choosing
/// submission order.
#[allow(clippy::type_complexity)]
pub fn sort_native_actions(
    actions: &[(Address, NativeAction)],
) -> (Vec<(Address, NativeAction)>, Vec<(Address, NativeAction)>) {
    let mut pre_evm = Vec::new();
    let mut post_evm = Vec::new();

    for (sender, action) in actions {
        let pair = (*sender, action.clone());
        match classify_action(action) {
            ActionCategory::Cancellation | ActionCategory::NonGtcOrder => pre_evm.push(pair),
            _ => post_evm.push(pair),
        }
    }

    // Deterministic sort: (category, sender, action_content_hash).
    sort_deterministic(&mut pre_evm);
    sort_deterministic(&mut post_evm);

    (pre_evm, post_evm)
}

/// Deterministic sort key for a native action.
///
/// Uses `NativeAction::canonical_bytes()` instead of `Debug` formatting to ensure
/// the sort key is identical across compiler versions and crate updates.
fn action_sort_key(sender: &Address, action: &NativeAction) -> (ActionCategory, Address, B256) {
    let category = classify_action(action);
    let hash = alloy_primitives::keccak256(action.canonical_bytes());
    (category, *sender, hash)
}

/// Sort actions deterministically by (category, sender, action_content_hash).
fn sort_deterministic(actions: &mut Vec<(Address, NativeAction)>) {
    // Pre-compute sort keys to avoid repeated hashing during sort.
    let mut keyed: Vec<_> = actions
        .drain(..)
        .map(|(s, a)| {
            let key = action_sort_key(&s, &a);
            (key, (s, a))
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    actions.extend(keyed.into_iter().map(|(_, pair)| pair));
}

// ============================================================================
// CoreWriter → NativeAction conversion
// ============================================================================

/// Convert a CoreWriter queued action to a NativeAction for execution.
fn core_writer_to_native(qa: &QueuedAction) -> NativeAction {
    match &qa.kind {
        QueuedActionKind::PlaceOrder {
            market_id,
            side,
            order_type,
            price,
            quantity,
            time_in_force,
        } => NativeAction::PlaceOrder(PlaceOrderParams {
            market_id: *market_id,
            is_buy: *side == 0,
            price: *price,
            quantity: *quantity,
            order_type: decode_order_type(*order_type),
            time_in_force: decode_time_in_force(*time_in_force),
            reduce_only: false,
            client_order_id: None,
        }),
        QueuedActionKind::CancelOrder { order_id } => NativeAction::CancelOrder {
            order_id: *order_id,
        },
        QueuedActionKind::CancelAll { market_id } => NativeAction::CancelAllOrders {
            market_id: Some(*market_id),
        },
        QueuedActionKind::Delegate { validator, amount } => NativeAction::Delegate {
            validator: *validator,
            amount: fp_to_u256(*amount),
        },
        QueuedActionKind::Undelegate { validator, amount } => NativeAction::Undelegate {
            validator: *validator,
            amount: fp_to_u256(*amount),
        },
        QueuedActionKind::ClaimRewards => NativeAction::ClaimRewards,
        QueuedActionKind::LockPermanent { amount } => NativeAction::PermanentStake {
            amount: fp_to_u256(*amount),
        },
    }
}

fn decode_order_type(code: u8) -> OrderType {
    match code {
        0 => OrderType::Limit,
        1 => OrderType::Market,
        _ => OrderType::Limit,
    }
}

fn decode_time_in_force(code: u8) -> TimeInForce {
    match code {
        0 => TimeInForce::GTC,
        1 => TimeInForce::IOC,
        2 => TimeInForce::FOK,
        3 => TimeInForce::PostOnly,
        _ => TimeInForce::GTC,
    }
}
