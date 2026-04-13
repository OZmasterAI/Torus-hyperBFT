//! Native action execution — dispatches each NativeAction to the correct handler.
//!
//! NativeExecutor is the core dispatch layer for processing native (non-EVM) actions
//! within a block. It handles order book operations, staking, oracle, governance,
//! lockbox, and liquidation processing in a deterministic pipeline.
//!
//! Task 2.5.1: NativeExecutor dispatch table + batch execution.

use std::collections::HashMap;

use alloy_primitives::Address;
use torus_core::liquidation::LiquidationEngine;
use torus_core::lockbox::{fp_to_u256, u256_to_fp, Lockbox};
use torus_core::margin::MarketMarginConfig;
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::OrderBook;
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

        Self {
            positions,
            oracle,
            staking,
            governance,
            state_db,
            order_books: HashMap::new(),
            margin_configs: HashMap::new(),
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

            // ---- Oracle ----
            NativeAction::SubmitOraclePrices(submission) => {
                Self::exec_submit_oracle_prices(ctx, sender, &submission.prices, submission.timestamp)
            }

            // ---- Governance ----
            NativeAction::SubmitProposal(proposal) => {
                Self::exec_submit_proposal(ctx, sender, &proposal.title, &proposal.description)
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
            NativeAction::Withdraw { amount, .. } => {
                Self::exec_withdraw_from_native(ctx, sender, *amount)
            }

            // ---- Validator management (stubs for Phase 2) ----
            NativeAction::RegisterValidator { .. } => {
                NativeActionResult::ok("register_validator", 0)
            }
            NativeAction::UpdateCommission { .. } => {
                NativeActionResult::ok("update_commission", 0)
            }
            NativeAction::JailVote { target } => Self::exec_jail_vote(ctx, sender, target),
            NativeAction::UnjailSelf => Self::exec_unjail_self(ctx, sender),

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

        // Get or create order book for this market.
        let book = ctx
            .order_books
            .entry(market_id)
            .or_insert_with(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE));

        let result = book.place_order(params.clone(), *sender, ctx.timestamp);

        // Apply fills to position manager.
        for fill in &result.fills {
            // Taker side is opposite of maker side.
            let taker_is_buy = fill.maker_side != Side::Buy;
            let _ = ctx.positions.apply_fill(
                &fill.taker,
                market_id,
                taker_is_buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            );
            let _ = ctx.positions.apply_fill(
                &fill.maker,
                market_id,
                fill.maker_side == Side::Buy,
                fill.quantity,
                fill.price,
                MarginType::Cross,
            );
        }

        NativeActionResult::ok("place_order", 1000)
    }

    fn exec_cancel_order(ctx: &mut NativeExecContext, order_id: u128) -> NativeActionResult {
        for book in ctx.order_books.values_mut() {
            if book.cancel_order(order_id).is_ok() {
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
        match market_id {
            Some(mid) => {
                if let Some(book) = ctx.order_books.get_mut(&mid) {
                    book.cancel_all(*sender, Some(mid));
                }
            }
            None => {
                for book in ctx.order_books.values_mut() {
                    book.cancel_all(*sender, None);
                }
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
            if book.modify_order(order_id, new_price, new_qty).is_ok() {
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

    // ========================================================================
    // Oracle handlers
    // ========================================================================

    fn exec_submit_oracle_prices(
        ctx: &mut NativeExecContext,
        sender: &Address,
        prices: &[(MarketId, FixedPoint)],
        _submission_timestamp: u64,
    ) -> NativeActionResult {
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
        title: &str,
        description: &str,
    ) -> NativeActionResult {
        match ctx.governance.submit_proposal(
            *sender,
            title.to_string(),
            description.to_string(),
            None,
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
        let support = matches!(option, VoteOption::Yes);
        match ctx
            .governance
            .cast_vote(*sender, proposal_id, support, ctx.block_height)
        {
            Ok(()) => NativeActionResult::ok("vote", 2000),
            Err(e) => NativeActionResult::err("vote", e.to_string()),
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

    // ========================================================================
    // Block-level processing helpers (called by validator pipeline)
    // ========================================================================

    /// Drain and execute CoreWriter actions queued from the previous block.
    pub fn drain_core_writer(ctx: &mut NativeExecContext) -> Vec<NativeActionResult> {
        let queued = match CoreWriterQueue::drain(&ctx.state_db, ctx.block_height) {
            Ok(actions) => actions,
            Err(_) => return vec![],
        };

        let mut results = Vec::with_capacity(queued.len());
        for qa in &queued {
            let action = core_writer_to_native(qa);
            let result = Self::execute(ctx, &qa.trader, &action);
            results.push(result);
        }
        results
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

            let oracle_price = oracle_prices
                .iter()
                .find(|(mid, _)| *mid == *market_id)
                .map(|(_, p)| *p)
                .unwrap_or(FixedPoint::ZERO);

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

    // Stable sort within each group by category.
    pre_evm.sort_by_key(|(_, a)| classify_action(a));
    post_evm.sort_by_key(|(_, a)| classify_action(a));

    (pre_evm, post_evm)
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
