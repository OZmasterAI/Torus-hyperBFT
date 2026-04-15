//! Native action execution — dispatches each NativeAction to the correct handler.
//!
//! NativeExecutor is the core dispatch layer for processing native (non-EVM) actions
//! within a block. It handles order book operations, staking, oracle, governance,
//! lockbox, and liquidation processing in a deterministic pipeline.
//!
//! Task 2.5.1: NativeExecutor dispatch table + batch execution.

use std::collections::HashMap;

use alloy_primitives::{Address, B256};
use torus_core::liquidation::LiquidationEngine;
use torus_core::lockbox::{fp_to_u256, u256_to_fp, Lockbox};
use torus_core::margin::{effective_max_leverage, MarketMarginConfig};
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::{OrderBook, OrderStatus};
use torus_core::position::{MarginType, PositionManager};
use torus_core::precompiles::{CoreWriterQueue, QueuedAction, QueuedActionKind};
use torus_economics::{
    EpochManager, FeeSplitter, GovernanceManager, RewardDistributor, StakingManager,
};
use torus_state::StateDb;
use torus_types::{
    FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, Side, TimeInForce, U256,
    VoteOption,
};

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

// ============================================================================
// Execution context — holds all state managers and per-block metadata
// ============================================================================

/// All state managers and block metadata needed for native execution.
pub struct NativeExecContext {
    pub positions: PositionManager,
    pub oracle: OracleManager,
    pub staking: StakingManager,
    pub governance: GovernanceManager,
    pub state_db: StateDb,

    /// Order books (per market, in-memory).
    pub order_books: HashMap<MarketId, OrderBook>,
    /// Per-market margin configuration.
    pub margin_configs: HashMap<MarketId, MarketMarginConfig>,
    /// FIX 6 (ECON-FIND-09): Global order ID counter shared across all markets.
    pub next_global_order_id: u128,

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
}

impl NativeExecContext {
    /// Create a new execution context from a StateDb and block metadata.
    pub fn new(
        state_db: StateDb,
        block_height: u64,
        timestamp: u64,
        epoch: u64,
        epoch_length: u64,
        max_validators: u32,
        proposer: Address,
        treasury_address: Address,
        dev_pool_address: Address,
    ) -> Self {
        let positions = PositionManager::new(state_db.clone());
        let oracle = OracleManager::new(state_db.clone(), OracleConfig::default());
        let staking = StakingManager::new(state_db.clone());
        let governance = GovernanceManager::new(state_db.clone());

        // FIX 1 (ECON-FIND-02): Load persisted order books from DB on startup.
        let (order_books, next_global_order_id) = Self::load_order_books(&state_db);

        Self {
            positions,
            oracle,
            staking,
            governance,
            state_db,
            order_books,
            margin_configs: HashMap::new(),
            next_global_order_id,
            block_height,
            timestamp,
            epoch,
            epoch_length,
            max_validators,
            proposer,
            treasury_address,
            dev_pool_address,
            total_native_fees: 0,
        }
    }

    /// FIX 1 (ECON-FIND-02): Load order books from DB. Returns (books, next_global_order_id).
    fn load_order_books(state_db: &StateDb) -> (HashMap<MarketId, OrderBook>, u128) {
        use borsh::BorshDeserialize;
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;

        let mut books = HashMap::new();
        let mut max_order_id: u128 = 0;

        let db = state_db.inner();
        if let Some(cf) = db.cf_handle(CF_NATIVE_ORDER_BOOKS) {
            let iter = db.iterator_cf(cf, rocksdb::IteratorMode::Start);
            for item in iter {
                if let Ok((key, value)) = item {
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
        }

        // Global ID starts at max found + 1 (or 1 if no books loaded)
        let next_id = if max_order_id > 0 { max_order_id } else { 1 };
        (books, next_id)
    }

    /// FIX 1 (ECON-FIND-02): Persist all order books to DB. Called after block execution.
    pub fn save_order_books(&self) {
        use torus_state::cf::CF_NATIVE_ORDER_BOOKS;

        for (&market_id, book) in &self.order_books {
            let key = market_id.to_be_bytes();
            match borsh::to_vec(book) {
                Ok(data) => {
                    if let Err(e) = self.state_db.put_cf_raw(CF_NATIVE_ORDER_BOOKS, &key, &data) {
                        tracing::error!(market_id, %e, "failed to persist order book");
                    }
                }
                Err(e) => {
                    tracing::error!(market_id, %e, "failed to serialize order book");
                }
            }
        }
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
    pub fn execute(
        ctx: &mut NativeExecContext,
        sender: &Address,
        action: &NativeAction,
    ) -> NativeActionResult {
        match action {
            // ---- Order book ----
            NativeAction::PlaceOrder(params) => Self::exec_place_order(ctx, sender, params),
            NativeAction::CancelOrder { order_id } => Self::exec_cancel_order(ctx, *order_id),
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
            NativeAction::SubmitOraclePrices(submission) => {
                Self::exec_submit_oracle_prices(ctx, sender, &submission.prices, submission.timestamp)
            }

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

            // ---- Admin (governance-gated, stubs) ----
            NativeAction::UpdateMarketParams { .. } => {
                NativeActionResult::ok("update_market_params", 0)
            }
            NativeAction::ListMarket(_) => NativeActionResult::ok("list_market", 0),
            NativeAction::DelistMarket { .. } => NativeActionResult::ok("delist_market", 0),
        }
    }

    /// Execute a batch of (sender, action) pairs in order.
    /// Does NOT stop on individual failure — every action runs.
    pub fn execute_batch(
        ctx: &mut NativeExecContext,
        actions: &[(Address, NativeAction)],
    ) -> NativeBatchResult {
        let mut results = Vec::with_capacity(actions.len());
        let mut total_gas = 0u64;

        for (sender, action) in actions {
            let result = Self::execute(ctx, sender, action);
            total_gas += result.gas_used;
            results.push(result);
        }

        NativeBatchResult { results, total_gas }
    }

    // ========================================================================
    // Order book handlers
    // ========================================================================

    fn exec_place_order(
        ctx: &mut NativeExecContext,
        sender: &Address,
        params: &PlaceOrderParams,
    ) -> NativeActionResult {
        let market_id = params.market_id;
        let is_market = matches!(params.order_type, OrderType::Market);

        // FIX 2 (ECON-FIND-05): Reserve order margin before placing the order.
        // For market orders, margin is settled at fill time (no resting order).
        let order_margin_required = if !is_market && params.price > FixedPoint::ZERO {
            let notional = params.price * params.quantity;
            let max_lev = ctx
                .margin_configs
                .get(&market_id)
                .map(|c| effective_max_leverage(&c.tiers, notional))
                .unwrap_or(20);
            let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
            notional / lev_fp
        } else {
            FixedPoint::ZERO
        };

        if order_margin_required > FixedPoint::ZERO {
            match ctx.positions.get_native_balance(sender) {
                Ok(mut bal) => {
                    if bal.available < order_margin_required {
                        return NativeActionResult::err(
                            "place_order",
                            format!(
                                "insufficient margin: need {order_margin_required}, have {}",
                                bal.available
                            ),
                        );
                    }
                    bal.available = bal.available - order_margin_required;
                    bal.order_margin = bal.order_margin + order_margin_required;
                    if let Err(e) = ctx.positions.put_native_balance(sender, &bal) {
                        return NativeActionResult::err("place_order", e.to_string());
                    }
                }
                Err(e) => return NativeActionResult::err("place_order", e.to_string()),
            }
        }

        // Get or create order book for this market.
        let book = ctx
            .order_books
            .entry(market_id)
            .or_insert_with(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE));

        // FIX 6 (ECON-FIND-09): Sync global order ID counter to prevent cross-market collisions.
        book.set_next_order_id(ctx.next_global_order_id);
        let result = book.place_order(params.clone(), *sender, ctx.timestamp);
        ctx.next_global_order_id = book.next_order_id();

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
                // Partially filled — release proportional margin
                order_margin_required * filled_qty / params.quantity
            } else {
                FixedPoint::ZERO
            };

            if margin_to_release > FixedPoint::ZERO {
                if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
                    bal.order_margin = bal.order_margin - margin_to_release;
                    bal.available = bal.available + margin_to_release;
                    let _ = ctx.positions.put_native_balance(sender, &bal);
                }
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
                return NativeActionResult::err(
                    "place_order",
                    format!("taker fill failed: {e}"),
                );
            }
            if let Err(e) = ctx.positions.apply_fill(
                &fill.maker,
                market_id,
                fill.maker_side == Side::Buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            ) {
                return NativeActionResult::err(
                    "place_order",
                    format!("maker fill failed: {e}"),
                );
            }
        }

        NativeActionResult::ok("place_order", 1000)
    }

    fn exec_cancel_order(ctx: &mut NativeExecContext, order_id: u128) -> NativeActionResult {
        for book in ctx.order_books.values_mut() {
            if let Ok(cancelled) = book.cancel_order(order_id) {
                // FIX 2 (ECON-FIND-05): Release order margin on cancel.
                let notional = cancelled.price * cancelled.remaining_qty;
                let market_id = book.market_id;
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
                        bal.order_margin = bal.order_margin - release;
                        bal.available = bal.available + release;
                        let _ = ctx.positions.put_native_balance(&cancelled.trader, &bal);
                    }
                }
                return NativeActionResult::ok("cancel_order", 500);
            }
        }
        NativeActionResult::err("cancel_order", format!("order {order_id} not found"))
    }

    fn exec_cancel_all(
        ctx: &mut NativeExecContext,
        sender: &Address,
        market_id: Option<MarketId>,
    ) -> NativeActionResult {
        // FIX 2 (ECON-FIND-05): Compute total margin to release from cancelled orders.
        let mut total_margin_release = FixedPoint::ZERO;

        match market_id {
            Some(mid) => {
                if let Some(book) = ctx.order_books.get_mut(&mid) {
                    let cancelled = book.cancel_all(*sender, Some(mid));
                    for order in &cancelled {
                        let notional = order.price * order.remaining_qty;
                        let max_lev = ctx
                            .margin_configs
                            .get(&mid)
                            .map(|c| effective_max_leverage(&c.tiers, notional))
                            .unwrap_or(20);
                        let lev_fp = FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                        total_margin_release = total_margin_release + notional / lev_fp;
                    }
                }
            }
            None => {
                let market_ids: Vec<MarketId> = ctx.order_books.keys().copied().collect();
                for mid in market_ids {
                    if let Some(book) = ctx.order_books.get_mut(&mid) {
                        let cancelled = book.cancel_all(*sender, None);
                        for order in &cancelled {
                            let notional = order.price * order.remaining_qty;
                            let max_lev = ctx
                                .margin_configs
                                .get(&mid)
                                .map(|c| effective_max_leverage(&c.tiers, notional))
                                .unwrap_or(20);
                            let lev_fp =
                                FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE);
                            total_margin_release = total_margin_release + notional / lev_fp;
                        }
                    }
                }
            }
        }

        if total_margin_release > FixedPoint::ZERO {
            if let Ok(mut bal) = ctx.positions.get_native_balance(sender) {
                let release = total_margin_release.min(bal.order_margin);
                bal.order_margin = bal.order_margin - release;
                bal.available = bal.available + release;
                let _ = ctx.positions.put_native_balance(sender, &bal);
            }
        }

        NativeActionResult::ok("cancel_all", 500)
    }

    fn exec_modify_order(
        ctx: &mut NativeExecContext,
        order_id: u128,
        new_price: Option<FixedPoint>,
        new_qty: Option<FixedPoint>,
    ) -> NativeActionResult {
        for book in ctx.order_books.values_mut() {
            // Capture old order state for margin delta calculation.
            let old_order = book.get_order(order_id).cloned();
            if let Ok(modified) = book.modify_order(order_id, new_price, new_qty) {
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
                            bal.available = bal.available - delta;
                            bal.order_margin = bal.order_margin + delta;
                            let _ = ctx.positions.put_native_balance(&modified.trader, &bal);
                        }
                    } else if old_margin > new_margin {
                        let delta = old_margin - new_margin;
                        if let Ok(mut bal) = ctx.positions.get_native_balance(&modified.trader) {
                            let release = delta.min(bal.order_margin);
                            bal.order_margin = bal.order_margin - release;
                            bal.available = bal.available + release;
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

    fn exec_delegate(
        ctx: &mut NativeExecContext,
        sender: &Address,
        validator: &Address,
        amount: U256,
    ) -> NativeActionResult {
        match ctx.staking.delegate(*sender, *validator, amount) {
            Ok(()) => NativeActionResult::ok("delegate", 2000),
            Err(e) => NativeActionResult::err("delegate", e.to_string()),
        }
    }

    fn exec_undelegate(
        ctx: &mut NativeExecContext,
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

    fn exec_permanent_stake(
        ctx: &mut NativeExecContext,
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

    fn exec_claim_rewards(ctx: &mut NativeExecContext, sender: &Address) -> NativeActionResult {
        match ctx.staking.claim_rewards(*sender) {
            Ok(_) => NativeActionResult::ok("claim_rewards", 1500),
            Err(e) => NativeActionResult::err("claim_rewards", e.to_string()),
        }
    }

    fn exec_jail_vote(
        ctx: &mut NativeExecContext,
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

    fn exec_unjail_self(ctx: &mut NativeExecContext, sender: &Address) -> NativeActionResult {
        match ctx.staking.unjail(sender, ctx.block_height) {
            Ok(()) => NativeActionResult::ok("unjail_self", 2000),
            Err(e) => NativeActionResult::err("unjail_self", e.to_string()),
        }
    }

    fn exec_register_validator(
        ctx: &mut NativeExecContext,
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
        let self_stake = match ctx.state_db.get_account(sender) {
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
                // Consume the whitelist entry after successful registration
                if let Err(e) = ctx.governance.consume_whitelist(sender) {
                    tracing::warn!(%sender, %e, "failed to consume whitelist entry");
                }
                NativeActionResult::ok("register_validator", 5000)
            }
            Err(e) => NativeActionResult::err("register_validator", e.to_string()),
        }
    }

    fn exec_update_commission(
        ctx: &mut NativeExecContext,
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

    fn exec_rotate_key(
        ctx: &mut NativeExecContext,
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
    // Oracle handlers
    // ========================================================================

    fn exec_submit_oracle_prices(
        ctx: &mut NativeExecContext,
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

    fn exec_submit_proposal(
        ctx: &mut NativeExecContext,
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

    fn exec_vote(
        ctx: &mut NativeExecContext,
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

    fn exec_deposit_to_native(
        ctx: &mut NativeExecContext,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => return NativeActionResult::err("deposit_to_native", "amount overflow".into()),
        };
        match Lockbox::deposit_to_native(&ctx.state_db, sender, fp_amount) {
            Ok(()) => NativeActionResult::ok("deposit_to_native", 1500),
            Err(e) => NativeActionResult::err("deposit_to_native", e.to_string()),
        }
    }

    fn exec_withdraw_from_native(
        ctx: &mut NativeExecContext,
        sender: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => {
                return NativeActionResult::err("withdraw_from_native", "amount overflow".into())
            }
        };
        match Lockbox::withdraw_from_native(&ctx.state_db, sender, fp_amount) {
            Ok(()) => NativeActionResult::ok("withdraw_from_native", 1500),
            Err(e) => NativeActionResult::err("withdraw_from_native", e.to_string()),
        }
    }

    /// FIX ECON-PF-17: Withdraw from sender's native balance to a specified EVM address.
    fn exec_withdraw_to(
        ctx: &mut NativeExecContext,
        sender: &Address,
        to: &Address,
        amount: U256,
    ) -> NativeActionResult {
        let fp_amount = match u256_to_fp(amount) {
            Some(fp) => fp,
            None => return NativeActionResult::err("withdraw_to", "amount overflow".into()),
        };
        match Lockbox::withdraw_from_native_to(&ctx.state_db, sender, to, fp_amount) {
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
    pub fn drain_core_writer(
        ctx: &mut NativeExecContext,
    ) -> Result<Vec<NativeActionResult>, torus_core::error::CoreError> {
        let queued = CoreWriterQueue::drain(&ctx.state_db, ctx.block_height)?;

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
    pub fn aggregate_oracle_prices(
        ctx: &mut NativeExecContext,
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
                Err(e) => {
                    results.push(NativeActionResult::err("oracle_aggregate", e.to_string()))
                }
            }
        }
        results
    }

    /// Run liquidation checks across all configured markets.
    pub fn run_liquidation_checks(
        ctx: &mut NativeExecContext,
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
    pub fn distribute_fees(ctx: &mut NativeExecContext, total_evm_fees: u64) -> NativeActionResult {
        let total_fees = U256::from(ctx.total_native_fees + total_evm_fees);
        if total_fees.is_zero() {
            return NativeActionResult::ok("fee_distribution", 0);
        }

        let _split = FeeSplitter::split_fees(total_fees, ctx.epoch);

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

    /// Check and process epoch boundary (validator rotation + reward distribution).
    pub fn process_epoch_boundary(ctx: &mut NativeExecContext) -> Option<NativeActionResult> {
        if !EpochManager::is_epoch_boundary(ctx.block_height, ctx.epoch_length) {
            return None;
        }

        match EpochManager::compute_new_validator_set(
            &ctx.staking,
            ctx.max_validators,
            ctx.epoch + 1,
        ) {
            Ok(_new_set) => Some(NativeActionResult::ok("epoch_rotation", 5000)),
            Err(e) => Some(NativeActionResult::err("epoch_rotation", e.to_string())),
        }
    }

    /// Process pending governance proposals.
    pub fn process_governance(ctx: &mut NativeExecContext) -> Vec<NativeActionResult> {
        match ctx.governance.process_pending_proposals(ctx.block_height) {
            Ok(outcomes) => outcomes
                .iter()
                .map(|_| NativeActionResult::ok("governance_process", 1000))
                .collect(),
            Err(e) => vec![NativeActionResult::err(
                "governance_process",
                e.to_string(),
            )],
        }
    }
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
    let hash = alloy_primitives::keccak256(&action.canonical_bytes());
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
        QueuedActionKind::CancelOrder { order_id } => {
            NativeAction::CancelOrder { order_id: *order_id }
        }
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
