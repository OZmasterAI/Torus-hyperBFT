//! `torus_*` JSON-RPC namespace — native exchange, staking, governance endpoints.

use std::sync::atomic::Ordering::{self, Relaxed};

use alloy_primitives::keccak256;
use borsh::BorshDeserialize;
use jsonrpsee::core::{async_trait, RpcResult, SubscriptionResult};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::PendingSubscriptionSink;
use rocksdb::IteratorMode;

use torus_core::book_reader::{self, BookLayout};
use torus_core::oracle::{OracleConfig, OracleManager};
use torus_core::order_book::OrderBook;
use torus_core::position::{open_order_limit, PositionManager, OPEN_ORDER_MAX_LIMIT};
use torus_core::precompiles::OrderBookSnapshot;
use torus_economics::governance::{GovernanceManager, ProposalStatus, ProposalType};
use torus_economics::rewards::FeeSplitter;
use torus_economics::staking::StakingManager;
use torus_economics::types::{
    ValidatorStatus, FEE_END_BURN_BPS, FEE_END_DEV_POOL_BPS, FEE_END_TREASURY_BPS,
    FEE_END_VALIDATOR_BPS, FEE_START_BURN_BPS, FEE_START_DEV_POOL_BPS, FEE_START_TREASURY_BPS,
    FEE_START_VALIDATOR_BPS, TRANSITION_EPOCHS,
};
use torus_economics::{lerp_bps, PermanentStakeInfo};
use torus_state::cf::{
    CF_BLOCK_ACTION_STATUS, CF_BLOCK_BODIES, CF_GOVERNANCE_PROPOSALS, CF_NATIVE_MARKETS,
    CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS, CF_NATIVE_TRADES, CF_NATIVE_USER_TRADES,
    CF_STAKING_PERMANENT,
};
use torus_state::trade_rows::{
    decode_trade_row, decode_user_row, parse_trade_key, parse_user_trade_key, trade_key,
    MarketTrade, TRADE_KEY_LEN,
};
use torus_types::FixedPoint;

use crate::error::RpcError;
use crate::types::*;
use crate::RpcState;

/// Per-item reply when a non-cancel is shed because the native pool is full.
pub(crate) const POOL_FULL_PREVERIFY_MSG: &str = "mempool: pool full (pre-verify)";
/// Per-item reply when a non-cancel is shed by the admission limit (s65 item B):
/// the pool already holds more than the chain can include soon. Retryable.
pub(crate) const ADMISSION_BUSY_MSG: &str =
    "mempool: busy, admission limit reached (pre-verify), retry later";

// ============================================================================
// Internal deserialization helpers
// ============================================================================

/// Market data stored in CF_NATIVE_MARKETS by the governance module.
/// Format: base_asset(String) + quote_asset(String) + lot_size(i128) + tick_size(i128) + initial_margin(i128).
#[derive(BorshDeserialize)]
struct StoredMarket {
    base_asset: String,
    quote_asset: String,
    lot_size_raw: i128,
    tick_size_raw: i128,
    #[allow(dead_code)]
    initial_margin_raw: i128,
}

/// One `CF_NATIVE_TRADES` entry as returned over RPC (`trade_id` is the
/// fill's per-block `trade_index`).
fn rpc_trade(market_id: &str, block: u64, timestamp: u64, t: &MarketTrade) -> RpcTrade {
    RpcTrade {
        trade_id: hex_u128(t.trade_index as u128),
        market_id: market_id.to_string(),
        price: dec_fp(FixedPoint::from_raw(t.price_raw)),
        quantity: dec_fp(FixedPoint::from_raw(t.qty_raw)),
        side: if t.taker_side == 0 { "buy" } else { "sell" }.to_string(),
        block_number: hex_u64(block),
        timestamp: hex_u64(timestamp),
    }
}

/// Decode one packed trade-history row (key + value) for RPC as
/// `(block, timestamp, fills)`. An undecodable row (e.g. a leftover old-format
/// row) is skipped with a warning, so it never fails the whole query.
fn decode_market_row(key: &[u8], value: &[u8]) -> Option<(u64, u64, Vec<MarketTrade>)> {
    let decoded = parse_trade_key(key)
        .ok_or_else(|| format!("key length {}", key.len()))
        .and_then(|(_, block, _)| {
            let (timestamp, fills) = decode_trade_row(value).map_err(|e| e.to_string())?;
            Ok((block, timestamp, fills))
        });
    decoded
        .map_err(|e| tracing::warn!(key = %hex::encode(key), "skipping trade row: {e}"))
        .ok()
}

// ============================================================================
// Trait definition
// ============================================================================

#[rpc(server, namespace = "torus")]
pub trait TorusApi {
    // --- 2.9.1: Trading reads ---
    #[method(name = "getOrderBook")]
    async fn get_order_book(&self, market_id: String) -> RpcResult<RpcOrderBook>;

    #[method(name = "getPosition")]
    async fn get_position(
        &self,
        trader: String,
        market_id: String,
    ) -> RpcResult<Option<RpcPosition>>;

    #[method(name = "getBalances")]
    async fn get_balances(&self, trader: String) -> RpcResult<RpcBalances>;

    #[method(name = "getUserLimits")]
    async fn get_user_limits(&self, trader: String) -> RpcResult<RpcUserLimits>;

    // --- 2.9.2: Market info ---
    #[method(name = "getMarkets")]
    async fn get_markets(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcMarketInfo>>;

    #[method(name = "getTradeHistory")]
    async fn get_trade_history(
        &self,
        market_id: String,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcTrade>>;

    // --- 2.9.3: Staking ---
    #[method(name = "getStakingInfo")]
    async fn get_staking_info(&self, address: String) -> RpcResult<RpcStakingInfo>;

    #[method(name = "getValidators")]
    async fn get_validators(&self) -> RpcResult<Vec<RpcValidatorInfo>>;

    /// Running state hash: this node's checkpoint at `height` (default: the
    /// latest retained one) with the on-chain attestations and quorum hash.
    /// Error for heights that are not a retained checkpoint.
    #[method(name = "getStateHash")]
    async fn get_state_hash(&self, height: Option<u64>) -> RpcResult<RpcStateHash>;

    #[method(name = "getEpoch")]
    async fn get_epoch(&self) -> RpcResult<RpcEpochInfo>;

    #[method(name = "getDelegations")]
    async fn get_delegations(&self, delegator: String) -> RpcResult<Vec<RpcDelegation>>;

    // --- 2.9.4: Submission ---
    #[method(name = "submitNativeAction")]
    async fn submit_native_action(&self, signed_action: String) -> RpcResult<String>;

    /// Batch submission: up to `SUBMIT_BATCH_MAX` hex payloads in one call.
    /// One permit + one blocking task amortizes HTTP/JSON/scheduling overhead;
    /// per-item results so one bad action never poisons the batch (Sprint 2).
    #[method(name = "submitNativeActions")]
    async fn submit_native_actions(
        &self,
        signed_actions: Vec<String>,
    ) -> RpcResult<Vec<RpcSubmitResult>>;

    /// Batch submission with bincode-encoded payloads (hex of bincode bytes).
    /// Same pipeline and per-item semantics as `submitNativeActions`; action
    /// identity stays the canonical-JSON keccak in both formats (Sprint 5).
    #[method(name = "submitNativeActionsBin")]
    async fn submit_native_actions_bin(
        &self,
        payloads: Vec<String>,
    ) -> RpcResult<Vec<RpcSubmitResult>>;

    // --- 2.9.5: Governance ---
    #[method(name = "getProposal")]
    async fn get_proposal(&self, proposal_id: u64) -> RpcResult<Option<RpcProposal>>;

    #[method(name = "getProposals")]
    async fn get_proposals(&self, status: Option<String>) -> RpcResult<Vec<RpcProposal>>;

    #[method(name = "getGovernanceParams")]
    async fn get_governance_params(&self) -> RpcResult<RpcGovernanceParams>;

    // --- Treasury info (for explorer) ---
    #[method(name = "getTreasuryInfo")]
    async fn get_treasury_info(&self) -> RpcResult<RpcTreasuryInfo>;

    // --- Leader info (direct-to-leader) ---
    #[method(name = "getLeader")]
    async fn get_leader(&self) -> RpcResult<RpcLeaderInfo>;

    // --- Block body (native actions for explorer indexing) ---
    #[method(name = "getBlockBody")]
    async fn get_block_body(&self, block_number: u64) -> RpcResult<Option<RpcBlockBody>>;

    // --- Phase 7B: All trades in a single block (for explorer indexer) ---
    #[method(name = "getBlockTrades")]
    async fn get_block_trades(&self, block_number: u64) -> RpcResult<Vec<RpcTrade>>;

    // --- Phase 7B: Time-range trade query (forward, inclusive) ---
    #[method(name = "getTradeHistoryRange")]
    async fn get_trade_history_range(
        &self,
        market_id: String,
        from_block: String,
        to_block: String,
        limit: Option<u64>,
    ) -> RpcResult<Vec<RpcTrade>>;

    // --- Trading app endpoints ---
    #[method(name = "getOpenOrders")]
    async fn get_open_orders(
        &self,
        trader: String,
        market_id: Option<String>,
    ) -> RpcResult<Vec<RpcOpenOrder>>;

    #[method(name = "getOpenInterest")]
    async fn get_open_interest(&self, market_id: String) -> RpcResult<RpcOpenInterest>;

    #[method(name = "getMarkPrice")]
    async fn get_mark_price(&self, market_id: String) -> RpcResult<RpcMarkPrice>;

    #[method(name = "getUserTrades")]
    async fn get_user_trades(
        &self,
        trader: String,
        market_id: Option<String>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcUserTrade>>;

    // --- Phase 7B: Trade streaming subscription ---
    #[subscription(name = "subscribe" => "subscription", unsubscribe = "unsubscribe", item = serde_json::Value)]
    async fn subscribe(
        &self,
        sub_type: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult;
}

// ============================================================================
// Implementation
// ============================================================================

/// Ingress payload decoder: bytes (already hex-stripped) → signed action.
/// One per wire format; everything downstream of decode is format-agnostic.
type DecodeFn = fn(&[u8]) -> Result<torus_types::SignedNativeAction, String>;

/// Canonical-JSON ingress (legacy `submitNativeActions`).
fn decode_action_json(bytes: &[u8]) -> Result<torus_types::SignedNativeAction, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("invalid action encoding: {e}"))
}

/// bincode ingress (`submitNativeActionsBin`). Hash identity is unaffected:
/// canonical bytes are re-derived via serde_json after decode.
fn decode_action_bin(bytes: &[u8]) -> Result<torus_types::SignedNativeAction, String> {
    bincode::deserialize(bytes).map_err(|e| format!("invalid action encoding: {e}"))
}

/// RPC-only ingress guard (non-consensus; O2 design Open Question 2): reject
/// PlaceOrder / PlaceOrderBatch actions referencing a market_id with no row
/// in CF_NATIVE_MARKETS — closing the phantom-book trap where a typo'd id
/// "succeeds" into a tick=1/lot=1 book conjured at exec
/// (native_executor.rs:656; exec-side fix is a separate consensus item).
/// Point-gets on the 8-byte BE market key; the order-id counter row in the
/// same CF has a 24-byte key (NEXT_GLOBAL_ORDER_ID_KEY) so it never collides.
/// Cheap: runs BEFORE signature verify; batches read each market row once.
///
/// s92 item B: orders also get the book's placement tick and lot rules
/// (`OrderBook::place_order_with_accounts`), with the market row's tick/lot,
/// so they are refused here instead of being silently rejected by the book.
/// Item 6 M1: the rule, the text and the row decoder are the executor's
/// (`torus_core::order_book::{shape_violation, market_row_shape}`), and the
/// executor creates a missing book with the row's tick / lot (row 42).
/// A batch is one signed action, so one bad order rejects all of it.
pub(crate) fn validate_known_markets(
    action: &torus_types::NativeAction,
    state_db: &torus_state::StateDb,
) -> Result<(), String> {
    // `(tick, lot)` of a listed market (item 6 M1: the decoder the executor
    // creates books with); `None` when the row does not decode as a market
    // (placeholder rows): only existence is checked then.
    let spec = |mid: u64| -> Result<Option<(FixedPoint, FixedPoint)>, String> {
        match state_db.get_cf_raw(CF_NATIVE_MARKETS, &mid.to_be_bytes()) {
            Ok(Some(row)) => Ok(torus_core::order_book::market_row_shape(&row)),
            Ok(None) => Err(format!("unknown market_id {mid}")),
            Err(e) => Err(format!("market lookup failed: {e}")),
        }
    };
    let check = |mid: u64| spec(mid).map(|_| ());
    match action {
        torus_types::NativeAction::PlaceOrder(p) => check_tick_lot(p, spec(p.market_id)?),
        torus_types::NativeAction::PlaceOrderBatch(orders) => {
            let mut specs = std::collections::BTreeMap::new();
            for p in orders {
                let s = match specs.entry(p.market_id) {
                    std::collections::btree_map::Entry::Occupied(e) => *e.get(),
                    std::collections::btree_map::Entry::Vacant(e) => *e.insert(spec(p.market_id)?),
                };
                check_tick_lot(p, s)?;
            }
            Ok(())
        }
        // s517: the exec submission rules (NE exec_submit_oracle_prices) at
        // ingress, so a feeder gets an error instead of a silent exec failure.
        // Ingress-only: exec re-checks everything.
        torus_types::NativeAction::SubmitOraclePrices(sub) => {
            use torus_core::oracle::{valid_oracle_price, MAX_ORACLE_PRICES_PER_SUBMISSION as CAP};
            if sub.prices.is_empty() || sub.prices.len() > CAP {
                return Err(format!(
                    "oracle submission carries 1..={CAP} prices, got {}",
                    sub.prices.len()
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            for &(mid, price) in &sub.prices {
                if !seen.insert(mid) {
                    return Err(format!("duplicate market {mid} in oracle submission"));
                }
                check(mid)?;
                if !valid_oracle_price(price) {
                    return Err(format!("invalid oracle price {price} for market {mid}"));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The executor's pre-book placement shape check (item 6 M1:
/// `torus_core::order_book::shape_violation`, the same rule and text): the
/// lot applies to every order type (`qty < lot`, so lot 0 admits any qty >=
/// 0); the tick to a `Limit` price and a `StopLimit` limit, only when tick >
/// 0. Message = the executor's ("order rejected: ...").
fn check_tick_lot(
    p: &torus_types::PlaceOrderParams,
    spec: Option<(FixedPoint, FixedPoint)>,
) -> Result<(), String> {
    let Some((tick, lot)) = spec else {
        return Ok(());
    };
    match torus_core::order_book::shape_violation(p, tick, lot) {
        Some(v) => Err(v.placement_message()),
        None => Ok(()),
    }
}

/// Parse + structurally validate + signature-verify one hex-encoded signed
/// native action. Blocking-pool work (ecrecover); the batch endpoint runs a
/// whole batch of these inside one `spawn_blocking`. Error is a per-item
/// message, never a call-level failure.
fn verify_one_action_with(
    decode: DecodeFn,
    signed_action: &str,
    chain_id: u64,
    state_db: &torus_state::StateDb,
    current_time_ms: u64,
    action_bytes: &mut Vec<u8>,
) -> Result<
    (
        alloy_primitives::Address,
        torus_types::SignedNativeAction,
        alloy_primitives::B256,
    ),
    String,
> {
    let bytes = parse_bytes(signed_action).map_err(|e| format!("invalid hex: {e}"))?;
    let action = decode(&bytes)?;
    torus_mempool::rate_limit::validate_batch_size(&action.action)?;
    validate_known_markets(&action.action, state_db)?;
    let sender = action
        .validate_with_sessions(current_time_ms, chain_id, |pubkey| {
            state_db.get_session(pubkey).ok().flatten()
        })
        .map_err(|e| format!("signature verification failed: {e}"))?;
    // Acknowledgements remain keccak(canonical JSON), including for binary
    // ingress. The parsed action feeds admission/forwarding; those paths do
    // not need these bytes. Reuse task-local scratch and return only the hash
    // so completed batch results do not retain every action's JSON allocation.
    action_bytes.clear();
    serde_json::to_writer(&mut *action_bytes, &action)
        .map_err(|e| format!("serialize action: {e}"))?;
    let hash = keccak256(&*action_bytes);
    Ok((sender, action, hash))
}

/// JSON-ingress wrapper shared by the single-action endpoint and tests.
pub(crate) fn verify_one_action(
    signed_action: &str,
    chain_id: u64,
    state_db: &torus_state::StateDb,
    current_time_ms: u64,
) -> Result<
    (
        alloy_primitives::Address,
        torus_types::SignedNativeAction,
        alloy_primitives::B256,
    ),
    String,
> {
    verify_one_action_with(
        decode_action_json,
        signed_action,
        chain_id,
        state_db,
        current_time_ms,
        &mut Vec::new(),
    )
}

/// Dedicated bounded pool for ingress verification (s352 regression fix).
///
/// The GLOBAL rayon pool runs consensus-critical work — exec-thread
/// `batch_verify_native_actions`, the validate-path batch verify, and
/// per-market matching. Running ingress `par_iter` on that same pool let
/// ~100 queued ingress tasks starve consensus verify at the task-queue
/// level (s352 probe: block time 529ms→4034ms, exec verify phase 3x).
/// Half the cores (min 2) keeps ingress off the consensus threads' backs
/// while still parallelizing within a batch.
fn ingress_verify_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let threads = (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
            / 2)
        .max(2);
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("torus-ingress-verify-{i}"))
            .build()
            .expect("build ingress verify pool")
    })
}

/// Routing decision for one batch item, made before signature verification.
/// `Proceed` payloads are consumed (`mem::take`) when handed to the verify
/// closure; only the variant tag matters afterwards.
enum SubmitSlot {
    /// Pay full verification (normal path; cancels past the admission limit).
    Proceed(String),
    /// Shed before crypto with this per-item error (reason already counted).
    Rejected(String),
}

impl RpcState {
    /// Item 2: the oracle aggregate of `mid` if USABLE ([`OraclePrice::usable`]:
    /// time-based, stale 60 s of block time after the last fresh aggregate)
    /// at the header timestamp of the EXECUTED head (s89): the aggregate is
    /// read from executed state, so its age is judged at the block that state
    /// reflects — the eth view's head, min(applied, committed). The committed
    /// head runs ahead under exec lag and made every mark read stale. No
    /// header (or an unreadable one) ⇒ no mark.
    fn usable_oracle_price(&self, mid: u64) -> Option<torus_core::oracle::OraclePrice> {
        let executed = crate::eth::eth_head(self);
        let (header, _, _) = crate::eth::get_header_with_hash(self, executed).ok().flatten()?;
        OracleManager::new(self.state.clone(), OracleConfig::default())
            .get_price(mid, header.timestamp)
            .ok()
            .filter(|op| op.usable().is_some())
    }

    /// Anti-spam item B: refuse `action` when `sender` has used its
    /// per-address allowance. Runs after verify (the sender is unknown
    /// before) and before pool admission; a refusal is counted by reason and
    /// returned as the per-item reply. Charging happens on pool entry (the
    /// mempool counts every pooled action, whatever its source), so an
    /// admitted action is counted exactly once.
    fn check_addr_rate(
        &self,
        sender: &alloy_primitives::Address,
        action: &torus_types::NativeAction,
    ) -> Result<(), String> {
        self.mempool
            .addr_rate_admits(sender, action)
            .map_err(|refusal| {
                self.count_admit_reject(refusal.reason());
                refusal.message(sender)
            })
    }

    /// Count one admission-path rejection under its concrete reason label.
    fn count_admit_reject(&self, reason: &str) {
        if let Some(ref m) = self.metrics {
            m.rpc_submit_admit_rejects
                .get_or_create(&vec![("reason".into(), reason.into())])
                .inc();
        }
    }

    /// Shared batch-submit pipeline: cap check → permit → pool-full prescreen
    /// (decode-only shed) → blocking-pool verify → admit + leader-forward.
    /// `decode` fixes the wire format; everything downstream is format-agnostic.
    async fn run_submit_pipeline(
        &self,
        signed_actions: Vec<String>,
        decode: DecodeFn,
    ) -> RpcResult<Vec<RpcSubmitResult>> {
        if signed_actions.len() > crate::SUBMIT_BATCH_MAX {
            return Err(ErrorObjectOwned::from(RpcError::InvalidParams(format!(
                "batch too large: {} > {}",
                signed_actions.len(),
                crate::SUBMIT_BATCH_MAX
            ))));
        }
        // One permit covers the whole batch — that's the amortization: the
        // permit bounds concurrent blocking-pool verify tasks, and the batch
        // runs as exactly one such task.
        let permit_wait_t0 = std::time::Instant::now();
        let _permit = crate::acquire_submit_permit(&self.submit_semaphore)
            .await
            .ok_or_else(|| {
                ErrorObjectOwned::from(RpcError::Internal("server overloaded, try again".into()))
            })?;
        if let Some(ref m) = self.metrics {
            m.rpc_submit_permit_wait_seconds
                .observe(permit_wait_t0.elapsed().as_secs_f64());
        }

        // Sprint 5 (C): when the native pool is already full, every action
        // but an oracle submission is doomed at admission — shed it after a
        // decode-only pass instead of paying signature verification.
        // Anti-spam item C: cancels too, since a full pool no longer lets a
        // cancel evict an order. s517: oracle submissions proceed to full
        // verification (admission evicts a normal entry to make room; the
        // mempool's oracle gate admits only Active validators and signers).
        // s65 item B: the same pre-verify shed when the pool already holds
        // more than the admission limit (recent commit rate x horizon), with a
        // retryable "busy" instead of "pool full" — there cancels still pass
        // (the admission limit never sheds cancels).
        let (shed_msg, cancels_pass) = if self.mempool.native_pool_is_full() {
            (Some(POOL_FULL_PREVERIFY_MSG), false)
        } else if self.mempool.native_admission_backlogged() {
            (Some(ADMISSION_BUSY_MSG), true)
        } else {
            (None, true)
        };
        let mut slots: Vec<SubmitSlot> = if let Some(shed_msg) = shed_msg {
            let screened = tokio::task::spawn_blocking(move || {
                signed_actions
                    .into_iter()
                    .map(|signed_action| {
                        let decoded = parse_bytes(&signed_action)
                            .map_err(|e| format!("invalid hex: {e}"))
                            .and_then(|bytes| decode(&bytes));
                        match decoded {
                            Ok(action)
                                if torus_mempool::is_oracle_submission(&action.action)
                                    || (cancels_pass
                                        && torus_mempool::is_cancel(&action.action)) =>
                            {
                                SubmitSlot::Proceed(signed_action)
                            }
                            Ok(_) => SubmitSlot::Rejected(shed_msg.to_string()),
                            Err(e) => SubmitSlot::Rejected(e),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| {
                ErrorObjectOwned::from(RpcError::Internal(format!("spawn_blocking: {e}")))
            })?;
            for slot in &screened {
                if let SubmitSlot::Rejected(msg) = slot {
                    self.count_admit_reject(match msg.as_str() {
                        POOL_FULL_PREVERIFY_MSG => "pool_full_preverify",
                        ADMISSION_BUSY_MSG => "backlog_preverify",
                        _ => "verify_failed",
                    });
                }
            }
            screened
        } else {
            signed_actions
                .into_iter()
                .map(SubmitSlot::Proceed)
                .collect()
        };
        let to_verify: Vec<String> = slots
            .iter_mut()
            .filter_map(|slot| match slot {
                SubmitSlot::Proceed(payload) => Some(std::mem::take(payload)),
                SubmitSlot::Rejected(_) => None,
            })
            .collect();

        let state_db = self.state.clone();
        let chain_id = self.chain_id;
        let verify_t0 = std::time::Instant::now();
        let (verified, verify_cpu) = tokio::task::spawn_blocking(move || {
            // Option A (ingress-verify-fix): verify the batch in PARALLEL —
            // per-action cost is ~70ms CPU at bs500, so a serial loop made the
            // ack RTT scale with batch size. Runs on the DEDICATED ingress
            // pool, never the global one (s352: sharing starved consensus).
            // collect() preserves index order, which the per-item result
            // alignment below depends on (pinned by
            // submit_batch_order_preserved_with_interleaved_failures).
            use rayon::prelude::*;
            let cpu_t0 = std::time::Instant::now();
            let current_time_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_millis() as u64;
            let out = ingress_verify_pool().install(|| {
                to_verify
                    .into_par_iter()
                    .map_init(Vec::new, |action_bytes, signed_action| {
                        verify_one_action_with(
                            decode,
                            &signed_action,
                            chain_id,
                            &state_db,
                            current_time_ms,
                            action_bytes,
                        )
                    })
                    .collect::<Vec<_>>()
            });
            (out, cpu_t0.elapsed())
        })
        .await
        .map_err(|e| ErrorObjectOwned::from(RpcError::Internal(format!("spawn_blocking: {e}"))))?;
        if let Some(ref m) = self.metrics {
            m.rpc_submit_verify_seconds
                .observe(verify_t0.elapsed().as_secs_f64());
            m.rpc_submit_verify_cpu_seconds
                .observe(verify_cpu.as_secs_f64());
        }

        let admit_t0 = std::time::Instant::now();
        // Sub-phase accumulators (Option A): split admit into mempool insert
        // vs leader-forward so the probe can name the next bottleneck.
        let mut insert_dur = std::time::Duration::ZERO;
        let mut forward_dur = std::time::Duration::ZERO;
        let mut verified_iter = verified.into_iter();
        let results = slots
            .into_iter()
            .map(|slot| {
                let item = match slot {
                    // Pre-verify shed: reason already counted at prescreen.
                    SubmitSlot::Rejected(msg) => {
                        return RpcSubmitResult {
                            hash: None,
                            error: Some(msg),
                        };
                    }
                    SubmitSlot::Proceed(_) => verified_iter
                        .next()
                        .expect("one verified result per proceed slot"),
                };
                match item {
                    Ok((sender, action, hash)) => {
                        if let Err(msg) = self.check_addr_rate(&sender, &action.action) {
                            return RpcSubmitResult {
                                hash: None,
                                error: Some(msg),
                            };
                        }
                        let insert_t0 = std::time::Instant::now();
                        // B1: the leader-forward now carries the PARSED action
                        // (structured tuple, no JSON re-encode), so keep a copy
                        // for the forward — but ONLY when forwarding is armed
                        // and this node is not the leader (the clone of a
                        // PlaceOrderBatch is real work on the hot path).
                        let fwd_target = self.forward_leader_target();
                        let fwd_copy = fwd_target.map(|vk| (vk, action.clone()));
                        let admitted = self.mempool.add_native_action_presigned(sender, action);
                        insert_dur += insert_t0.elapsed();
                        match admitted {
                            Ok(()) => {
                                let forward_t0 = std::time::Instant::now();
                                if let Some((leader_vk, fwd_action)) = fwd_copy {
                                    self.push_forward(leader_vk, sender, fwd_action);
                                }
                                forward_dur += forward_t0.elapsed();
                                RpcSubmitResult {
                                    hash: Some(hex_b256(hash)),
                                    error: None,
                                }
                            }
                            Err(e) => {
                                self.count_admit_reject(match &e {
                                    torus_mempool::MempoolError::DuplicateNativeAction => {
                                        "duplicate"
                                    }
                                    torus_mempool::MempoolError::NativeSenderQueueFull {
                                        ..
                                    } => "sender_queue_full",
                                    torus_mempool::MempoolError::NativePoolFull => "pool_full",
                                    torus_mempool::MempoolError::UnfundedSender { .. } => {
                                        "unfunded"
                                    }
                                    _ => "other",
                                });
                                RpcSubmitResult {
                                    hash: None,
                                    error: Some(format!("mempool: {e}")),
                                }
                            }
                        }
                    }
                    Err(msg) => {
                        self.count_admit_reject("verify_failed");
                        RpcSubmitResult {
                            hash: None,
                            error: Some(msg),
                        }
                    }
                }
            })
            .collect();
        if let Some(ref m) = self.metrics {
            m.rpc_submit_admit_seconds
                .observe(admit_t0.elapsed().as_secs_f64());
            m.rpc_submit_admit_insert_seconds
                .observe(insert_dur.as_secs_f64());
            m.rpc_submit_admit_forward_seconds
                .observe(forward_dur.as_secs_f64());
        }

        Ok(results)
    }

    /// B1: resolve where a leader-forward would go — `Some(leader_vk)` only
    /// when full-body forwarding is armed (`forward_bodies`), the plumbing is
    /// wired, a leader hint exists, and it is NOT this node. Shared gate for
    /// the single and batch submit endpoints; also the "should I clone the
    /// action for the forward?" decision on the batch hot path.
    fn forward_leader_target(&self) -> Option<[u8; 32]> {
        if !self.forward_bodies {
            return None;
        }
        let (Some(ref leader_fn), Some(ref own_vk), Some(_)) =
            (&self.leader_vk_fn, &self.own_vk, &self.forward_action_tx)
        else {
            return None;
        };
        let leader_vk = leader_fn()?;
        (leader_vk != *own_vk).then_some(leader_vk)
    }

    /// B1: hand one admitted action to the node-side ForwardBatcher as a
    /// structured `(leader hint, verified sender, parsed action)` tuple — no
    /// JSON re-encode (the old payload was 20-byte sender ‖ serde_json, 2–4×
    /// wire bloat). The channel is BOUNDED: overflow drops the NEWEST item and
    /// counts `rpc_forward_dropped_full` — recoverable, the action is already
    /// in the local pool and the re-forward sweep re-sends it.
    fn push_forward(
        &self,
        leader_vk: [u8; 32],
        sender: alloy_primitives::Address,
        action: torus_types::SignedNativeAction,
    ) {
        let Some(ref fwd_tx) = self.forward_action_tx else {
            return;
        };
        if fwd_tx.try_send((leader_vk, sender, action)).is_err() {
            if let Some(ref m) = self.metrics {
                m.rpc_forward_dropped_full.inc();
            }
        }
    }

    /// Direct-to-leader forwarding for EVM txs (Option B): unicast the raw RLP to the current
    /// leader so a tx submitted to a non-proposer / RPC-only node still reaches the block
    /// producer. UNCONDITIONAL — unlike the native forward there is no `forward_bodies` gate
    /// (EVM has no gossip pre-spread, so this unicast is the only dissemination path). No-op
    /// when this node IS the leader or forwarding is unwired. The leader independently
    /// full-validates via `add_evm_tx`, so forwarding pre-validated bytes is safe.
    ///
    /// Takes the RLP by value (the caller already owns a clone, since `add_evm_tx` consumes the
    /// original): moving it into the channel avoids a second copy of the body on the hot path.
    pub(crate) fn forward_evm_to_leader(&self, raw_rlp: Vec<u8>) {
        if let (Some(ref leader_fn), Some(ref own_vk), Some(ref fwd_tx)) =
            (&self.leader_vk_fn, &self.own_vk, &self.forward_evm_tx)
        {
            if let Some(leader_vk) = leader_fn() {
                if leader_vk != *own_vk {
                    let _ = fwd_tx.send((leader_vk, raw_rlp));
                }
            }
        }
    }
}

/// Decode a `CF_NATIVE_ORDER_BOOKS` value into RPC price levels.
///
/// S444 / S395: the column family holds the PRODUCTION `OrderBook` blob
/// (written by `save_order_books` every block); `OrderBookSnapshot` is a
/// legacy format written only by old test scaffolding. `torus_getOrderBook`
/// used to decode ONLY the snapshot format, so it Borsh-errored ("Not all
/// bytes read") on every real book — first hit by the t15 native state-diff.
/// Try the production format first, fall back to the legacy snapshot.
fn decode_order_book_levels(
    data: &[u8],
) -> Result<(Vec<RpcPriceLevel>, Vec<RpcPriceLevel>), String> {
    if let Ok(book) = OrderBook::try_from_slice(data) {
        let to_levels = |depth: Vec<(FixedPoint, FixedPoint, usize)>| {
            depth
                .into_iter()
                .map(|(price, quantity, n)| RpcPriceLevel {
                    price: dec_fp(price),
                    quantity: dec_fp(quantity),
                    order_count: n as u32,
                })
                .collect::<Vec<_>>()
        };
        return Ok((to_levels(book.bid_depth()), to_levels(book.ask_depth())));
    }

    let snapshot = OrderBookSnapshot::try_from_slice(data)
        .map_err(|e| format!("borsh decode: {e}"))?;
    let to_levels = |levels: &[torus_core::precompiles::PriceLevel]| {
        levels
            .iter()
            .map(|lvl| RpcPriceLevel {
                price: dec_fp(lvl.price),
                quantity: dec_fp(lvl.quantity),
                order_count: 0,
            })
            .collect::<Vec<_>>()
    };
    Ok((to_levels(&snapshot.bids), to_levels(&snapshot.asks)))
}

/// Row-layout depth (`TORUS_BOOK_ROWS=1/2`) in RPC shape. Mode 2 is served
/// from the ROOT-CF level rows; mode 1 aggregates the per-order rows.
fn row_levels(levels: Vec<book_reader::DepthLevel>) -> Vec<RpcPriceLevel> {
    levels
        .into_iter()
        .map(|l| RpcPriceLevel {
            price: dec_fp(l.price),
            quantity: dec_fp(l.quantity),
            order_count: l.order_count,
        })
        .collect()
}

/// The layout actually on disk. Never derived from this process's env — an RPC
/// node must serve whatever the DB holds.
fn book_layout(state: &torus_state::StateDb) -> Result<BookLayout, ErrorObjectOwned> {
    book_reader::detect_layout(state)
        .map_err(|e| ErrorObjectOwned::from(RpcError::Internal(e.to_string())))
}

fn book_read_err(e: torus_core::error::CoreError) -> ErrorObjectOwned {
    ErrorObjectOwned::from(RpcError::Internal(e.to_string()))
}

/// `trader`'s open orders over all markets (resting + pending stops, as
/// `OrderBook::open_order_count` counts them) for `torus_getUserLimits`,
/// without materializing the book column families: rows are streamed, only
/// the trader's own order rows are decoded (the trader sits at a fixed offset
/// of every order row), and stop rows are reached with one seek per market
/// in the root CF instead of walking every level row.
fn count_open_orders(
    state: &torus_state::StateDb,
    trader: &alloy_primitives::Address,
    layout: BookLayout,
) -> Result<usize, String> {
    use torus_core::book_rows::{ROW_TAG_ORDER, ROW_TAG_STOP};
    let db = state.inner();
    let Ok(root_cf) = state.cf_handle(CF_NATIVE_ORDER_BOOKS) else {
        return Ok(0);
    };
    let mut root = db.raw_iterator_cf(root_cf);
    let mut count = 0;
    if layout == BookLayout::Classic {
        root.seek_to_first();
        while let (Some(key), Some(blob)) = (root.key(), root.value()) {
            if key.len() == 8 {
                let book = OrderBook::try_from_slice(blob)
                    .map_err(|e| format!("borsh decode order book: {e}"))?;
                count += book.open_order_count(trader);
            }
            root.next();
        }
        root.status().map_err(|e| format!("rocksdb: {e}"))?;
        return Ok(count);
    }

    // Resting: order rows (mode 1: the root CF; mode 2: the node-local store).
    let store_cf = match layout {
        BookLayout::OrderRows => Some(root_cf),
        _ => state.cf_handle(torus_state::cf::CF_BOOK_ORDER_ROWS).ok(),
    };
    if let Some(store_cf) = store_cf {
        let mut rows = db.raw_iterator_cf(store_cf);
        rows.seek_to_first();
        while let (Some(key), Some(row)) = (rows.key(), rows.value()) {
            if key.len() == 25
                && key[8] == ROW_TAG_ORDER
                && OrderBook::order_row_trader(row) == Some(trader.as_slice())
            {
                let (_, order) = OrderBook::decode_order_row(row)
                    .map_err(|e| format!("book order row: {e}"))?;
                count += usize::from(order.trader == *trader);
            }
            rows.next();
        }
        rows.status().map_err(|e| format!("rocksdb: {e}"))?;
    }

    // Pending stops: `market(8) ‖ 0x02 ‖ id(16)`, one seek per market.
    root.seek_to_first();
    while let Some(key) = root.key() {
        let Some(market) = key.get(..8) else {
            root.next();
            continue;
        };
        let market = u64::from_be_bytes(market.try_into().expect("8 bytes"));
        let mut prefix = [0u8; 9];
        prefix[..8].copy_from_slice(&market.to_be_bytes());
        prefix[8] = ROW_TAG_STOP;
        root.seek(prefix);
        while let (Some(key), Some(row)) = (root.key(), root.value()) {
            if !key.starts_with(&prefix) {
                break;
            }
            if key.len() == 25 {
                let mut one = OrderBook::new(market, FixedPoint::ONE, FixedPoint::ONE);
                one.restore_stop_row(row)
                    .map_err(|e| format!("market {market}: {e}"))?;
                count += one.open_order_count(trader);
            }
            root.next();
        }
        match market.checked_add(1) {
            Some(next) => root.seek(next.to_be_bytes()),
            None => break,
        }
    }
    root.status().map_err(|e| format!("rocksdb: {e}"))?;
    Ok(count)
}

/// Response cap for `torus_getOpenOrders`: the most open orders a user can
/// hold, so a user at the cap still sees every resting order.
const OPEN_ORDERS_LIMIT: usize = OPEN_ORDER_MAX_LIMIT as usize;

#[async_trait]
impl TorusApiServer for RpcState {
    // === 2.9.1: Trading reads ===

    async fn get_order_book(&self, market_id: String) -> RpcResult<RpcOrderBook> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let key = mid.to_be_bytes();

        // The book CF holds one of three layouts (classic blob / order rows /
        // level rows). Reading the classic key only — which is what this
        // handler used to do — silently returns an EMPTY book on a mode-1/2
        // node. Serve what is on disk, and error rather than fake an empty
        // book when the layout cannot be decoded.
        let layout = book_layout(&self.state)?;
        let (bids, asks) = if layout.is_rows() {
            let depth =
                book_reader::read_book_depth(&self.state, mid, layout).map_err(book_read_err)?;
            (row_levels(depth.bids), row_levels(depth.asks))
        } else {
            match self.state.get_cf_raw(CF_NATIVE_ORDER_BOOKS, &key) {
                Ok(Some(data)) => decode_order_book_levels(&data)
                    .map_err(RpcError::Internal)
                    .map_err(ErrorObjectOwned::from)?,
                Ok(None) => (vec![], vec![]),
                Err(e) => return Err(RpcError::State(e).into()),
            }
        };

        Ok(RpcOrderBook {
            market_id,
            bids,
            asks,
        })
    }

    async fn get_position(
        &self,
        trader: String,
        market_id: String,
    ) -> RpcResult<Option<RpcPosition>> {
        let addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;

        let pm = PositionManager::new(self.state.clone());
        let pos = pm
            .get_position(&addr, mid)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        match pos {
            None => Ok(None),
            Some(p) => {
                let side = if p.is_long { "long" } else { "short" };
                let margin_mode = match p.margin_type {
                    torus_core::position::MarginType::Cross => "cross",
                    torus_core::position::MarginType::Isolated => "isolated",
                };

                // Compute unrealized PnL at the usable oracle price; fall back to entry price.
                let mark_price = self
                    .usable_oracle_price(mid)
                    .map_or(p.entry_price, |op| op.price);
                let unrealized = p.unrealized_pnl(mark_price);

                // Simplified liquidation price estimate.
                let liquidation_price =
                    if p.size > FixedPoint::ZERO && p.isolated_margin > FixedPoint::ZERO {
                        if p.is_long {
                            p.entry_price - p.isolated_margin / p.size
                        } else {
                            p.entry_price + p.isolated_margin / p.size
                        }
                    } else {
                        FixedPoint::ZERO
                    };

                Ok(Some(RpcPosition {
                    market_id,
                    side: side.to_string(),
                    size: dec_fp(p.size),
                    entry_price: dec_fp(p.entry_price),
                    unrealized_pnl: dec_fp(unrealized),
                    realized_pnl: dec_fp(p.realized_pnl),
                    margin: dec_fp(p.isolated_margin),
                    margin_mode: margin_mode.to_string(),
                    liquidation_price: dec_fp(liquidation_price),
                }))
            }
        }
    }

    async fn get_balances(&self, trader: String) -> RpcResult<RpcBalances> {
        let addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;

        let pm = PositionManager::new(self.state.clone());
        let native_bal = pm
            .get_native_balance(&addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        // EVM balance
        let evm_balance = self
            .state
            .get_account(&addr)
            .map_err(RpcError::State)
            .map_err(ErrorObjectOwned::from)?
            .map(|a| a.balance)
            .unwrap_or_default();

        // Sum isolated margin from all open positions
        let positions = pm
            .positions_for_trader(&addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;
        let mut total_margin = native_bal.order_margin;
        for pos in &positions {
            total_margin += pos.isolated_margin;
        }

        // Permanent stake from CF_STAKING_PERMANENT
        let permanent_stake = match self.state.get_cf_raw(CF_STAKING_PERMANENT, addr.as_slice()) {
            Ok(Some(data)) => PermanentStakeInfo::try_from_slice(&data)
                .map(|info| hex_u256(info.amount))
                .unwrap_or_else(|_| "0x0".to_string()),
            _ => "0x0".to_string(),
        };

        let native_total = native_bal.available + native_bal.order_margin;

        Ok(RpcBalances {
            native_balance: dec_fp(native_total),
            evm_balance: hex_u256(evm_balance),
            total_margin_used: dec_fp(total_margin),
            available_balance: dec_fp(native_bal.available),
            permanent_stake,
        })
    }

    async fn get_user_limits(&self, trader: String) -> RpcResult<RpcUserLimits> {
        let addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;
        let cum_volume = PositionManager::new(self.state.clone())
            .get_cum_volume(&addr)
            .map_err(book_read_err)?;
        let layout = book_layout(&self.state)?;
        let open_orders = count_open_orders(&self.state, &addr, layout)
            .map_err(|e| ErrorObjectOwned::from(RpcError::Internal(e)))?;
        Ok(RpcUserLimits {
            open_orders: open_orders as u64,
            open_order_limit: open_order_limit(cum_volume),
            cum_volume: dec_fp(cum_volume),
        })
    }

    // === 2.9.2: Market info ===

    async fn get_markets(
        &self,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcMarketInfo>> {
        const MAX_MARKETS_PER_PAGE: usize = 500;
        let offset = offset.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(100).min(MAX_MARKETS_PER_PAGE as u32) as usize;

        let db = self.state.inner();
        let cf = db
            .cf_handle(CF_NATIVE_MARKETS)
            .ok_or_else(|| RpcError::Internal("missing CF_NATIVE_MARKETS".into()))
            .map_err(ErrorObjectOwned::from)?;

        let iter = db.iterator_cf(cf, IteratorMode::Start);
        let mut markets = Vec::new();
        let mut scanned = 0usize;

        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if key.len() != 8 {
                continue;
            }

            scanned += 1;

            // Skip entries before offset without deserializing
            if scanned <= offset {
                continue;
            }

            let mid = u64::from_be_bytes(key[..8].try_into().unwrap());

            let market = StoredMarket::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode market: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            markets.push(RpcMarketInfo {
                market_id: hex_u64(mid),
                base_asset: market.base_asset,
                quote_asset: market.quote_asset,
                lot_size: dec_fp(FixedPoint::from_raw(market.lot_size_raw)),
                tick_size: dec_fp(FixedPoint::from_raw(market.tick_size_raw)),
                status: "active".to_string(),
            });

            if markets.len() >= limit {
                break;
            }
        }

        // Keys are u64 big-endian — RocksDB lexicographic order == numeric order. No sort needed.
        Ok(markets)
    }

    async fn get_trade_history(
        &self,
        market_id: String,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcTrade>> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let limit = limit.unwrap_or(100).min(1000) as usize;
        let prefix = mid.to_be_bytes();

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_TRADES) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        let pruned_up_to = self.pruned_up_to.load(Relaxed);

        // Reverse-iterate from the upper bound of this market's key range.
        // Key format: market_id(8) + block_number(8) + chunk(2) = 18 bytes;
        // each row holds up to 1024 fills in trade_index order.
        let mut upper = [0xFFu8; TRADE_KEY_LEN];
        upper[..8].copy_from_slice(&prefix);
        let iter = db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&upper, rocksdb::Direction::Reverse),
        );
        let mut trades = Vec::with_capacity(limit);

        'rows: for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if !key.starts_with(&prefix) {
                break;
            }
            let Some((block, timestamp, fills)) = decode_market_row(&key, &value) else {
                continue;
            };

            // Since we iterate newest-first, once we hit a pruned block all
            // remaining entries are older — stop immediately.
            if pruned_up_to > 0 && block < pruned_up_to {
                break;
            }

            for t in fills.iter().rev() {
                trades.push(rpc_trade(&market_id, block, timestamp, t));
                if trades.len() >= limit {
                    break 'rows;
                }
            }
        }

        // Already in descending order (most recent first) from reverse iteration.
        Ok(trades)
    }

    // === 2.9.3: Staking ===

    async fn get_staking_info(&self, address: String) -> RpcResult<RpcStakingInfo> {
        let addr = parse_address(&address).map_err(ErrorObjectOwned::from)?;
        let staking = StakingManager::new(self.state.clone());
        let info = torus_economics::queries::get_staking_info(&staking, addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        let delegated = info
            .delegated
            .iter()
            .map(|(v, amt)| RpcDelegation {
                validator: hex_address(*v),
                amount: hex_u256(*amt),
            })
            .collect();

        let unbonding = info
            .unbonding
            .iter()
            .map(|u| RpcUnbonding {
                amount: hex_u256(u.amount),
                release_block: hex_u64(u.release_block),
            })
            .collect();

        Ok(RpcStakingInfo {
            delegated,
            permanent_stake: hex_u256(info.permanent_stake),
            pending_rewards: hex_u256(info.pending_rewards),
            unbonding,
        })
    }

    async fn get_state_hash(&self, height: Option<u64>) -> RpcResult<RpcStateHash> {
        use torus_state::running_hash::{checkpoint_heights, read_checkpoint};
        let retained = checkpoint_heights(&self.state);
        let height = match height.or_else(|| retained.last().copied()) {
            Some(h) => h,
            None => {
                return Err(ErrorObjectOwned::from(RpcError::InvalidParams(
                    "no state hash checkpoint retained yet".into(),
                )))
            }
        };
        let local = read_checkpoint(&self.state, height).ok_or_else(|| {
            ErrorObjectOwned::from(RpcError::InvalidParams(format!(
                "height {height} is not a retained state hash checkpoint (retained {:?}..={:?})",
                retained.first(),
                retained.last()
            )))
        })?;
        let staking = StakingManager::new(self.state.clone());
        let internal = |e: torus_economics::EconomicsError| {
            ErrorObjectOwned::from(RpcError::Internal(e.to_string()))
        };
        let votes = staking
            .state_hash_votes(height)
            .map_err(internal)?
            .into_iter()
            .map(|(validator, hash)| RpcStateHashVote {
                validator: hex_address(validator),
                hash: hex_b256(alloy_primitives::B256::from(hash)),
                matches_local: hash == local,
            })
            .collect();
        let quorum_hash = staking
            .state_hash_quorum(height)
            .map_err(internal)?
            .map(|q| hex_b256(alloy_primitives::B256::from(q)));
        Ok(RpcStateHash {
            height: hex_u64(height),
            local_hash: hex_b256(alloy_primitives::B256::from(local)),
            votes,
            quorum_hash,
        })
    }

    async fn get_validators(&self) -> RpcResult<Vec<RpcValidatorInfo>> {
        let staking = StakingManager::new(self.state.clone());
        let validators = torus_economics::queries::get_validators(&staking)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        // Also fetch full ValidatorState for status info.
        let all_states = staking
            .all_validators()
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        let result = validators
            .into_iter()
            .map(|v| {
                let state = all_states.iter().find(|s| s.address == v.address);
                let status = state
                    .map(|s| match s.status {
                        ValidatorStatus::Candidate => "candidate",
                        ValidatorStatus::Active => "active",
                        ValidatorStatus::Jailed => "jailed",
                        ValidatorStatus::Tombstoned => "tombstoned",
                    })
                    .unwrap_or("unknown");

                RpcValidatorInfo {
                    address: hex_address(v.address),
                    pubkey: format!("0x{}", hex::encode(v.pubkey.0)),
                    power: hex_u64(v.power),
                    commission_bps: v.commission_bps,
                    status: status.to_string(),
                    oracle_signer: state.and_then(|s| s.oracle_signer).map(hex_address),
                }
            })
            .collect();

        Ok(result)
    }

    async fn get_epoch(&self) -> RpcResult<RpcEpochInfo> {
        let current_height = self.latest_height.load(Ordering::Relaxed);
        let epoch_length = self.epoch_length;
        let info = torus_economics::queries::get_epoch_info(current_height, epoch_length);

        Ok(RpcEpochInfo {
            current_epoch: hex_u64(info.current_epoch),
            epoch_start_block: hex_u64(info.epoch_start_block),
            epoch_end_block: hex_u64(info.epoch_end_block),
            blocks_remaining: hex_u64(info.blocks_remaining),
            epoch_length: hex_u64(info.epoch_length),
        })
    }

    async fn get_delegations(&self, delegator: String) -> RpcResult<Vec<RpcDelegation>> {
        let addr = parse_address(&delegator).map_err(ErrorObjectOwned::from)?;
        let staking = StakingManager::new(self.state.clone());
        let delegations = torus_economics::queries::get_delegations(&staking, addr)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        Ok(delegations
            .into_iter()
            .map(|(v, amt)| RpcDelegation {
                validator: hex_address(v),
                amount: hex_u256(amt),
            })
            .collect())
    }

    // === 2.9.4: Submission ===

    async fn submit_native_action(&self, signed_action: String) -> RpcResult<String> {
        // Bounded queue: bursts wait up to SUBMIT_QUEUE_TIMEOUT for a permit
        // instead of instantly bouncing; sustained saturation still sheds.
        let _permit = crate::acquire_submit_permit(&self.submit_semaphore)
            .await
            .ok_or_else(|| {
                ErrorObjectOwned::from(RpcError::Internal("server overloaded, try again".into()))
            })?;
        // s65 item B: this endpoint honours the admission limit too, with the
        // same decode-only screen as the batch pipeline (priority actions —
        // cancels and, s517, oracle submissions — still pass).
        if self.mempool.native_admission_backlogged() {
            let is_priority = parse_bytes(&signed_action)
                .ok()
                .and_then(|bytes| decode_action_json(&bytes).ok())
                .is_some_and(|a| torus_mempool::is_priority(&a.action));
            if !is_priority {
                self.count_admit_reject("backlog_preverify");
                return Err(ErrorObjectOwned::from(RpcError::Internal(
                    ADMISSION_BUSY_MSG.into(),
                )));
            }
        }
        // Offload deserialization + ECDSA verification to the blocking thread pool
        // so heavy crypto doesn't starve the async runtime under load.
        // T2.4: same shared verify path as the batch endpoints — the canonical
        // JSON acknowledgement hash is computed ONCE there and the
        // hash reused below for the ack, instead of this endpoint
        // re-serializing serde_json + keccak in its own copy of the pipeline.
        // (B1: the leader-forward now carries the parsed action itself, so the
        // canonical bytes are no longer needed for the forward payload.)
        let state_db = self.state.clone();
        let chain_id = self.chain_id;
        let (sender, action, hash) = tokio::task::spawn_blocking(move || {
            let current_time_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_millis() as u64;
            verify_one_action(&signed_action, chain_id, &state_db, current_time_ms)
                .map_err(RpcError::InvalidParams)
        })
        .await
        .map_err(|e| ErrorObjectOwned::from(RpcError::Internal(format!("spawn_blocking: {e}"))))?
        .map_err(ErrorObjectOwned::from)?;

        self.check_addr_rate(&sender, &action.action)
            .map_err(|msg| ErrorObjectOwned::from(RpcError::Internal(msg)))?;
        self.mempool
            .add_native_action_presigned(sender, action.clone())
            .map_err(|e| {
                if matches!(e, torus_mempool::MempoolError::UnfundedSender { .. }) {
                    self.count_admit_reject("unfunded");
                }
                ErrorObjectOwned::from(RpcError::Internal(format!("mempool: {e}")))
            })?;

        // B1: forward the PARSED action (structured tuple, no JSON re-encode).
        if let Some(leader_vk) = self.forward_leader_target() {
            self.push_forward(leader_vk, sender, action);
        }

        Ok(hex_b256(hash))
    }

    async fn submit_native_actions(
        &self,
        signed_actions: Vec<String>,
    ) -> RpcResult<Vec<RpcSubmitResult>> {
        self.run_submit_pipeline(signed_actions, decode_action_json)
            .await
    }

    async fn submit_native_actions_bin(
        &self,
        payloads: Vec<String>,
    ) -> RpcResult<Vec<RpcSubmitResult>> {
        self.run_submit_pipeline(payloads, decode_action_bin).await
    }

    // === 2.9.5: Governance ===

    async fn get_proposal(&self, proposal_id: u64) -> RpcResult<Option<RpcProposal>> {
        let gov = GovernanceManager::new(self.state.clone());
        let proposal = gov
            .get_proposal(proposal_id)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        Ok(proposal.map(map_proposal))
    }

    async fn get_proposals(&self, status: Option<String>) -> RpcResult<Vec<RpcProposal>> {
        let gov = GovernanceManager::new(self.state.clone());

        let proposals = match status {
            Some(s) => {
                let ps = parse_proposal_status(&s).map_err(ErrorObjectOwned::from)?;
                gov.get_proposals_by_status(ps)
                    .map_err(|e| RpcError::Internal(e.to_string()))
                    .map_err(ErrorObjectOwned::from)?
            }
            None => {
                // Return all proposals by iterating the CF directly.
                let db = self.state.inner();
                let cf = db
                    .cf_handle(CF_GOVERNANCE_PROPOSALS)
                    .ok_or_else(|| RpcError::Internal("missing CF_GOVERNANCE_PROPOSALS".into()))
                    .map_err(ErrorObjectOwned::from)?;
                let iter = db.iterator_cf(cf, IteratorMode::Start);
                let mut all = Vec::new();
                for item in iter {
                    let (key, value) = item
                        .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                        .map_err(ErrorObjectOwned::from)?;
                    if key.len() != 8 {
                        continue;
                    }
                    let proposal = torus_economics::governance::Proposal::try_from_slice(&value)
                        .map_err(|e| RpcError::Internal(format!("borsh decode proposal: {e}")))
                        .map_err(ErrorObjectOwned::from)?;
                    all.push(proposal);
                }
                all
            }
        };

        Ok(proposals.into_iter().map(map_proposal).collect())
    }

    async fn get_governance_params(&self) -> RpcResult<RpcGovernanceParams> {
        let gov = GovernanceManager::new(self.state.clone());
        let params = gov
            .get_governance_params()
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        Ok(RpcGovernanceParams {
            voting_period_blocks: hex_u64(params.voting_period_blocks),
            quorum_bps: hex_u64(params.quorum_bps),
            min_proposal_stake: hex_u256(params.min_proposal_stake),
            permanent_weight_multiplier: format!(
                "{}/{}",
                params.permanent_weight_multiplier_num, params.permanent_weight_multiplier_den
            ),
            treasury_address: hex_address(params.treasury_address),
        })
    }

    async fn get_treasury_info(&self) -> RpcResult<RpcTreasuryInfo> {
        let gov = GovernanceManager::new(self.state.clone());
        let params = gov
            .get_governance_params()
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        let treasury_address = params.treasury_address;

        // Treasury balance from account state.
        let treasury_balance = self
            .state
            .get_account(&treasury_address)
            .map_err(RpcError::State)
            .map_err(ErrorObjectOwned::from)?
            .map(|a| a.balance)
            .unwrap_or_default();

        // Cumulative burn/treasury from SupplyTracker.
        let staking = StakingManager::new(self.state.clone());
        let tracker = FeeSplitter::get_supply_tracker(&staking)
            .map_err(|e| RpcError::Internal(e.to_string()))
            .map_err(ErrorObjectOwned::from)?;

        // Current fee split BPS using lerp.
        let current_height = self.latest_height.load(Ordering::Relaxed);
        let epoch_length = self.epoch_length;
        let epoch_info = torus_economics::queries::get_epoch_info(current_height, epoch_length);
        let epoch = epoch_info.current_epoch;

        let burn_bps = lerp_bps(
            FEE_START_BURN_BPS,
            FEE_END_BURN_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let validator_bps = lerp_bps(
            FEE_START_VALIDATOR_BPS,
            FEE_END_VALIDATOR_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let treasury_bps = lerp_bps(
            FEE_START_TREASURY_BPS,
            FEE_END_TREASURY_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );
        let dev_pool_bps = lerp_bps(
            FEE_START_DEV_POOL_BPS,
            FEE_END_DEV_POOL_BPS,
            epoch,
            TRANSITION_EPOCHS,
        );

        Ok(RpcTreasuryInfo {
            treasury_address: hex_address(treasury_address),
            treasury_balance: hex_u256(treasury_balance),
            cumulative_burned: hex_u256(tracker.cumulative_burned),
            cumulative_treasury: hex_u256(tracker.cumulative_treasury),
            current_fee_split: RpcFeeSplit {
                burn_bps,
                validator_bps,
                treasury_bps,
                dev_pool_bps,
            },
        })
    }

    async fn get_leader(&self) -> RpcResult<RpcLeaderInfo> {
        let (leader_vk_bytes, view) = match &self.leader_vk_fn {
            Some(f) => {
                let vk = f();
                let view = self.latest_height.load(Ordering::Relaxed).saturating_add(1);
                (vk, view)
            }
            None => (None, 0),
        };

        let (address, peer_id) = if let Some(vk_bytes) = leader_vk_bytes {
            let staking = StakingManager::new(self.state.clone());
            let addr = staking
                .find_validator_by_pubkey(&vk_bytes)
                .ok()
                .flatten()
                .map(|v| hex_address(v.address))
                .unwrap_or_else(|| format!("0x{}", ::hex::encode(vk_bytes)));
            (addr, format!("0x{}", ::hex::encode(vk_bytes)))
        } else {
            ("0x0".to_string(), String::new())
        };

        Ok(RpcLeaderInfo {
            address,
            peer_id,
            view: hex_u64(view),
        })
    }

    async fn get_block_body(&self, block_number: u64) -> RpcResult<Option<RpcBlockBody>> {
        let key = block_number.to_be_bytes();
        let data = match self.state.get_cf_raw(CF_BLOCK_BODIES, &key) {
            Ok(Some(d)) => d,
            Ok(None) => return Ok(None),
            Err(e) => return Err(ErrorObjectOwned::from(RpcError::State(e))),
        };
        // Records are either legacy JSON or the tagged bin record (r4
        // commit-persist); the codec dispatches on the first byte.
        let body: torus_types::TorusBlockBody = torus_state::block_body::decode_body_record(&data)
            .map_err(|e| RpcError::Internal(format!("body decode: {e}")))
            .map_err(ErrorObjectOwned::from)?;
        let native_actions: Vec<serde_json::Value> = body
            .native_actions
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or_default())
            .collect();
        // s84 executed/skipped record (written by execution; a block with no
        // actions has none and reports empty lists).
        let status = if body.native_actions.is_empty() && body.evm_transactions.is_empty() {
            Some(torus_state::action_status::BlockActionStatus::default())
        } else {
            self.state
                .get_cf_raw(CF_BLOCK_ACTION_STATUS, &key)
                .map_err(|e| ErrorObjectOwned::from(RpcError::State(e)))?
                .and_then(|bytes| torus_state::action_status::BlockActionStatus::decode(&bytes))
                .filter(|s| {
                    s.native_skipped.len() == body.native_actions.len()
                        && s.evm_skipped.len() == body.evm_transactions.len()
                })
        };
        let labels = |skipped: &[bool]| -> Vec<String> {
            skipped
                .iter()
                .map(|s| if *s { "skipped" } else { "executed" }.to_string())
                .collect()
        };
        let block_hash = crate::eth::get_header_with_hash(self, block_number)
            .map_err(ErrorObjectOwned::from)?
            .map(|(_, hash, _)| hash)
            .unwrap_or_default();
        let evm_transactions = body
            .evm_transactions
            .iter()
            .enumerate()
            .map(|(i, raw)| crate::eth::body_evm_tx_json(raw, block_hash, block_number, i as u32))
            .collect();
        Ok(Some(RpcBlockBody {
            block_number: hex_u64(block_number),
            native_actions,
            native_action_count: body.native_actions.len() as u32,
            native_action_status: status.as_ref().map(crate::types::native_action_labels),
            native_action_failures: status.as_ref().map(crate::types::native_action_failures),
            evm_transactions,
            evm_transaction_status: status.as_ref().map(|s| labels(&s.evm_skipped)),
        }))
    }

    // === Phase 7B: All trades in a single block ===

    async fn get_block_trades(&self, block_number: u64) -> RpcResult<Vec<RpcTrade>> {
        let trades_json = crate::scan_trades_for_block(&self.state, block_number);
        let mut trades = Vec::with_capacity(trades_json.len());
        for t in trades_json {
            trades.push(RpcTrade {
                trade_id: t
                    .get("tradeId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                market_id: t
                    .get("marketId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                price: t
                    .get("price")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                quantity: t
                    .get("quantity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                side: t
                    .get("side")
                    .and_then(|v| v.as_str())
                    .unwrap_or("buy")
                    .to_string(),
                block_number: t
                    .get("blockNumber")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
                timestamp: t
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("0x0")
                    .to_string(),
            });
        }
        Ok(trades)
    }

    // === Phase 7B: Time-range trade query ===

    async fn get_trade_history_range(
        &self,
        market_id: String,
        from_block: String,
        to_block: String,
        limit: Option<u64>,
    ) -> RpcResult<Vec<RpcTrade>> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;
        let from = parse_u64(&from_block).map_err(ErrorObjectOwned::from)?;
        let to = parse_u64(&to_block).map_err(ErrorObjectOwned::from)?;
        let limit = limit.unwrap_or(1000).min(5000) as usize;

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_TRADES) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        // market_id(8) + block(8) + chunk(2): from (from_block, chunk 0) up to
        // the first key of to_block + 1.
        let start_key = trade_key(mid, from, 0);
        let end_key = trade_key(mid, to.saturating_add(1), 0);

        let iter = db.iterator_cf(
            cf,
            rocksdb::IteratorMode::From(&start_key, rocksdb::Direction::Forward),
        );
        let mut trades = Vec::with_capacity(limit.min(256));

        'rows: for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;

            // Stop if we've left this market or past the end block
            if key.len() < 8 || key[..8] != mid.to_be_bytes() || key[..] >= end_key[..] {
                break;
            }
            let Some((block, timestamp, fills)) = decode_market_row(&key, &value) else {
                continue;
            };

            for t in &fills {
                trades.push(rpc_trade(&market_id, block, timestamp, t));
                if trades.len() >= limit {
                    break 'rows;
                }
            }
        }

        Ok(trades)
    }

    // === Trading app endpoints ===

    async fn get_open_orders(
        &self,
        trader: String,
        market_id: Option<String>,
    ) -> RpcResult<Vec<RpcOpenOrder>> {
        let trader_addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;

        // Row layouts: orders live in tagged rows (mode 1: root CF; mode 2:
        // the node-local order store). The classic branches below key on an
        // 8-byte market key, so on a mode-1/2 node the all-markets branch's
        // `key.len() != 8` filter matched NOTHING and this endpoint always
        // returned `[]`.
        let layout = book_layout(&self.state)?;
        if layout.is_rows() {
            let market = match market_id {
                Some(ref s) => Some(parse_u64(s).map_err(ErrorObjectOwned::from)?),
                None => None,
            };
            let rows = book_reader::read_open_orders(
                &self.state,
                &trader_addr,
                market,
                layout,
                OPEN_ORDERS_LIMIT,
            )
            .map_err(book_read_err)?;
            return Ok(rows
                .iter()
                .map(|(mid, order)| order_to_rpc(order, *mid))
                .collect());
        }

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_ORDER_BOOKS) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        let mut orders = Vec::new();

        if let Some(ref mid_str) = market_id {
            // Single market: read one OrderBook
            let mid = parse_u64(mid_str).map_err(ErrorObjectOwned::from)?;
            let key = mid.to_be_bytes();
            if let Some(data) = db
                .get_cf(cf, key)
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?
            {
                let book = OrderBook::try_from_slice(&data)
                    .map_err(|e| RpcError::Internal(format!("borsh decode order book: {e}")))
                    .map_err(ErrorObjectOwned::from)?;
                for order in book.orders_for_trader(&trader_addr) {
                    if orders.len() >= OPEN_ORDERS_LIMIT {
                        break;
                    }
                    orders.push(order_to_rpc(order, mid));
                }
            }
        } else {
            // All markets: iterate CF_NATIVE_ORDER_BOOKS
            let iter = db.iterator_cf(cf, IteratorMode::Start);
            for item in iter {
                if orders.len() >= OPEN_ORDERS_LIMIT {
                    break;
                }
                let (key, value) = item
                    .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                    .map_err(ErrorObjectOwned::from)?;
                if key.len() != 8 {
                    continue;
                }
                let mid = u64::from_be_bytes(key[..8].try_into().unwrap());
                let book = match OrderBook::try_from_slice(&value) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                for order in book.orders_for_trader(&trader_addr) {
                    if orders.len() >= OPEN_ORDERS_LIMIT {
                        break;
                    }
                    orders.push(order_to_rpc(order, mid));
                }
            }
        }

        Ok(orders)
    }

    async fn get_open_interest(&self, market_id: String) -> RpcResult<RpcOpenInterest> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_POSITIONS) {
            Some(cf) => cf,
            None => {
                return Ok(RpcOpenInterest {
                    market_id,
                    long_oi: dec_fp(FixedPoint::ZERO),
                    short_oi: dec_fp(FixedPoint::ZERO),
                });
            }
        };

        // Position key: trader(20) + market_id(8). Full scan, filter by market.
        let mut long_oi = FixedPoint::ZERO;
        let mut short_oi = FixedPoint::ZERO;
        let iter = db.iterator_cf(cf, IteratorMode::Start);
        for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if key.len() != 28 {
                continue;
            }
            let pos_market = u64::from_be_bytes(key[20..28].try_into().unwrap());
            if pos_market != mid {
                continue;
            }
            let pos = torus_core::position::Position::try_from_slice(&value)
                .map_err(|e| RpcError::Internal(format!("borsh decode position: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if pos.is_long {
                long_oi += pos.size;
            } else {
                short_oi += pos.size;
            }
        }

        Ok(RpcOpenInterest {
            market_id,
            long_oi: dec_fp(long_oi),
            short_oi: dec_fp(short_oi),
        })
    }

    async fn get_mark_price(&self, market_id: String) -> RpcResult<RpcMarkPrice> {
        let mid = parse_u64(&market_id).map_err(ErrorObjectOwned::from)?;

        let (mark_price, index_price, timestamp) = match self.usable_oracle_price(mid) {
            Some(op) => (op.price, op.price, op.block_number),
            None => (FixedPoint::ZERO, FixedPoint::ZERO, 0),
        };

        // Last trade price from the order book. Under the row layouts it lives
        // in the meta row's `ltp_tag` / `ltp_raw` suffix — reading only the
        // classic blob key reported 0 for every traded market on a mode-1/2
        // node.
        let layout = book_layout(&self.state)?;
        let last_trade_price = if layout.is_rows() {
            book_reader::read_last_trade_price(&self.state, mid, layout)
                .map_err(book_read_err)?
                .unwrap_or(FixedPoint::ZERO)
        } else {
            match self
                .state
                .get_cf_raw(CF_NATIVE_ORDER_BOOKS, &mid.to_be_bytes())
            {
                Ok(Some(data)) => OrderBook::try_from_slice(&data)
                    .ok()
                    .and_then(|book| book.last_trade_price())
                    .unwrap_or(FixedPoint::ZERO),
                _ => FixedPoint::ZERO,
            }
        };

        Ok(RpcMarkPrice {
            market_id,
            mark_price: dec_fp(mark_price),
            index_price: dec_fp(index_price),
            last_trade_price: dec_fp(last_trade_price),
            timestamp,
        })
    }

    async fn get_user_trades(
        &self,
        trader: String,
        market_id: Option<String>,
        limit: Option<u32>,
    ) -> RpcResult<Vec<RpcUserTrade>> {
        let trader_addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;
        let limit = limit.unwrap_or(100).min(1000) as usize;
        let market_filter = match market_id {
            Some(ref s) => Some(parse_u64(s).map_err(ErrorObjectOwned::from)?),
            None => None,
        };

        let db = self.state.inner();
        let cf = match db.cf_handle(CF_NATIVE_USER_TRADES) {
            Some(cf) => cf,
            None => return Ok(vec![]),
        };

        // Forward prefix scan: keys encode descending block order via
        // (u64::MAX - block), so forward iteration returns newest first.
        let prefix = trader_addr.as_slice();
        let iter = torus_state::db::prefix_iter(db, &cf, prefix);

        // Each row holds the trader's fills in one block, in trade_index order.
        let mut trades = Vec::with_capacity(limit.min(256));
        'rows: for item in iter {
            let (key, value) = item
                .map_err(|e| RpcError::Internal(format!("rocksdb: {e}")))
                .map_err(ErrorObjectOwned::from)?;
            if !key.starts_with(prefix) {
                break;
            }
            // Skip an undecodable row (e.g. leftover old format), as above.
            let decoded = parse_user_trade_key(&key)
                .ok_or_else(|| format!("key length {}", key.len()))
                .and_then(|(_, block)| {
                    let (timestamp, entries) = decode_user_row(&value).map_err(|e| e.to_string())?;
                    Ok((block, timestamp, entries))
                });
            let (block, timestamp, entries) = match decoded {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!(key = %hex::encode(&key), "skipping user trade row: {e}");
                    continue;
                }
            };

            for trade in entries {
                if market_filter.is_some_and(|mf| trade.market != mf) {
                    continue;
                }
                // s80: the user's own side (a maker is opposite the taker).
                let user_bought = (trade.taker_side == 0) == (trade.role == 1);
                trades.push(RpcUserTrade {
                    trade_id: hex_u128(trade.trade_index as u128),
                    market_id: hex_u64(trade.market),
                    side: if user_bought { "buy" } else { "sell" }.to_string(),
                    price: dec_fp(FixedPoint::from_raw(trade.price_raw)),
                    quantity: dec_fp(FixedPoint::from_raw(trade.qty_raw)),
                    role: if trade.role == 0 { "maker" } else { "taker" }.to_string(),
                    block_number: hex_u64(block),
                    timestamp: hex_u64(timestamp),
                });
                if trades.len() >= limit {
                    break 'rows;
                }
            }
        }

        Ok(trades)
    }

    // === Phase 7B: Trade streaming subscription ===

    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        sub_type: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult {
        use tokio::sync::broadcast::error::RecvError;

        // Validate before accepting: a bad request is rejected, never accepted.
        let kind = match crate::streams::parse_stream_kind(&sub_type, params.as_ref()) {
            Ok(k) => k,
            Err(msg) => {
                pending
                    .reject(ErrorObjectOwned::from(RpcError::InvalidParams(msg)))
                    .await;
                return Ok(());
            }
        };
        // s80 fix 2: all-markets newTrades is node-configurable (off on
        // validators by default); per-market and userFills always pass.
        if kind == crate::streams::StreamKind::NewTrades(None) && !self.all_market_trades {
            pending
                .reject(ErrorObjectOwned::from(RpcError::InvalidParams(
                    crate::streams::ALL_MARKET_TRADES_DISABLED.to_string(),
                )))
                .await;
            return Ok(());
        }
        let Some(_slot) = SubscriptionSlot::acquire(&self.active_subscriptions) else {
            pending
                .reject(ErrorObjectOwned::owned(
                    -32000,
                    "subscription limit reached",
                    None::<()>,
                ))
                .await;
            return Ok(());
        };
        // Subscribe before accepting so no block after the reply is missed.
        let mut rx = self.notifier.new_trades.subscribe();
        let sink = pending.accept().await?;

        // jsonrpsee runs this future on its own task until it returns; the
        // returned error becomes the subscription's close notification.
        loop {
            let block = tokio::select! {
                _ = sink.closed() => return Ok(()),
                r = rx.recv() => r,
            };
            let block = match block {
                Ok(b) => b,
                Err(RecvError::Lagged(n)) => {
                    return Err(format!(
                        "subscriber lagged: {n} blocks dropped; resubscribe and backfill \
                         with torus_getTradeHistoryRange / torus_getUserTrades"
                    )
                    .into());
                }
                Err(RecvError::Closed) => return Ok(()),
            };
            let msg = match kind {
                // Serialized once per (block, filter), shared by all
                // subscribers with this filter; embedded here verbatim.
                crate::streams::StreamKind::NewTrades(market) => {
                    match block.new_trades_payload(market).await? {
                        Some(payload) => {
                            let raw: &serde_json::value::RawValue = &payload;
                            Some(jsonrpsee::SubscriptionMessage::new(
                                sink.method_name(),
                                sink.subscription_id(),
                                &raw,
                            )?)
                        }
                        None => None,
                    }
                }
                crate::streams::StreamKind::UserFills(user) => {
                    stream_message(&sink, &crate::streams::fills_for_user(&block.fills, user))?
                }
            };
            if let Some(msg) = msg {
                if sink.send(msg).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// `torus_subscribe` cap on active WebSocket subscriptions (HIGH-NEW-05). The
/// counter is shared with `eth_subscribe`.
const MAX_SUBSCRIPTIONS: usize = 1000;

/// One slot of the subscription cap; released on drop, so every exit path of
/// a subscription frees it.
struct SubscriptionSlot(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl SubscriptionSlot {
    fn acquire(counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Option<Self> {
        counter
            .fetch_update(Relaxed, Relaxed, |n| {
                (n < MAX_SUBSCRIPTIONS).then_some(n + 1)
            })
            .ok()
            .map(|_| Self(counter.clone()))
    }
}

impl Drop for SubscriptionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Relaxed);
    }
}

/// One stream message holding a block's `rows`, or `None` when the block has
/// none for this subscriber.
fn stream_message<T: serde::Serialize>(
    sink: &jsonrpsee::SubscriptionSink,
    rows: &[T],
) -> Result<Option<jsonrpsee::SubscriptionMessage>, serde_json::Error> {
    if rows.is_empty() {
        return Ok(None);
    }
    jsonrpsee::SubscriptionMessage::new(sink.method_name(), sink.subscription_id(), &rows).map(Some)
}

// ============================================================================
// Helpers
// ============================================================================

fn map_proposal(p: torus_economics::governance::Proposal) -> RpcProposal {
    let proposal_type = match p.proposal_type {
        ProposalType::ParameterChange => "ParameterChange",
        ProposalType::TreasurySpend => "TreasurySpend",
        ProposalType::MarketListing => "MarketListing",
        ProposalType::TextProposal => "TextProposal",
        ProposalType::ValidatorRegistration => "ValidatorRegistration",
        ProposalType::PermanentUnlock => "PermanentUnlock",
    };

    let status = match p.status {
        ProposalStatus::Pending => "Pending",
        ProposalStatus::Active => "Active",
        ProposalStatus::Passed => "Passed",
        ProposalStatus::Rejected => "Rejected",
        ProposalStatus::Executed => "Executed",
        ProposalStatus::Expired => "Expired",
    };

    RpcProposal {
        id: p.id,
        proposer: hex_address(p.proposer),
        title: p.title,
        description: p.description,
        proposal_type: proposal_type.to_string(),
        status: status.to_string(),
        votes_for: hex_u256(p.votes_for),
        votes_against: hex_u256(p.votes_against),
        start_block: hex_u64(p.start_block),
        end_block: hex_u64(p.end_block),
    }
}

fn order_to_rpc(order: &torus_core::order_book::Order, market_id: u64) -> RpcOpenOrder {
    use torus_types::{OrderType, Side, TimeInForce};

    let side = match order.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    let order_type = match order.order_type {
        OrderType::Limit => "limit",
        OrderType::Market => "market",
        OrderType::StopMarket { .. } => "stop_market",
        OrderType::StopLimit { .. } => "stop_limit",
    };
    let time_in_force = match order.time_in_force {
        TimeInForce::GTC => "gtc",
        TimeInForce::IOC => "ioc",
        TimeInForce::FOK => "fok",
        TimeInForce::PostOnly => "post_only",
    };

    RpcOpenOrder {
        order_id: hex_u128(order.id),
        market_id: hex_u64(market_id),
        side: side.to_string(),
        price: dec_fp(order.price),
        remaining_qty: dec_fp(order.remaining_qty),
        original_qty: dec_fp(order.original_qty),
        order_type: order_type.to_string(),
        time_in_force: time_in_force.to_string(),
        reduce_only: order.reduce_only,
        client_order_id: order.client_order_id.map(hex_u64),
        timestamp: order.timestamp,
    }
}

fn parse_proposal_status(s: &str) -> Result<ProposalStatus, RpcError> {
    match s.to_lowercase().as_str() {
        "pending" => Ok(ProposalStatus::Pending),
        "active" => Ok(ProposalStatus::Active),
        "passed" => Ok(ProposalStatus::Passed),
        "rejected" => Ok(ProposalStatus::Rejected),
        "executed" => Ok(ProposalStatus::Executed),
        "expired" => Ok(ProposalStatus::Expired),
        _ => Err(RpcError::InvalidParams(format!(
            "unknown proposal status: {s}"
        ))),
    }
}

#[cfg(test)]
mod ack_scratch_tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use torus_types::{ActionSignature, NativeAction, SignedNativeAction};

    const NOW: u64 = 1_000_000;

    // Independent pre-change oracle: allocate canonical JSON with to_vec.
    fn old_verify(
        decode: DecodeFn,
        payload: &str,
        state: &torus_state::StateDb,
    ) -> Result<(Address, Vec<u8>, B256), String> {
        let bytes = parse_bytes(payload).map_err(|e| format!("invalid hex: {e}"))?;
        let action = decode(&bytes)?;
        torus_mempool::rate_limit::validate_batch_size(&action.action)?;
        validate_known_markets(&action.action, state)?;
        let sender = action
            .validate_with_sessions(NOW, torus_types::eip712::TORUS_CHAIN_ID, |key| {
                state.get_session(key).ok().flatten()
            })
            .map_err(|e| format!("signature verification failed: {e}"))?;
        let bytes = serde_json::to_vec(&action).map_err(|e| format!("serialize action: {e}"))?;
        let hash = keccak256(&bytes);
        Ok((sender, bytes, hash))
    }

    fn fixtures(state: &torus_state::StateDb) -> Vec<SignedNativeAction> {
        use torus_types::{FixedPoint, OrderType, PlaceOrderParams, TimeInForce};
        state
            .put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), b"market")
            .unwrap();
        let key = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
        let sign = |action, nonce| torus_types::eip712::sign_native_action(action, nonce, &key);
        let order = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(1_000_000_000),
            quantity: FixedPoint::from_raw(100_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: Some(42),
        };
        let session = torus_types::eip712::sign_native_action_with_session(
            NativeAction::CancelOrder { order_id: 42 },
            NOW,
            &([8u8; 32].into()),
        );
        let ActionSignature::Session { session_pubkey, .. } = &session.signature else {
            panic!("expected session signature")
        };
        state
            .put_session(
                session_pubkey,
                &torus_types::SessionData {
                    owner: Address::from([9; 20]),
                    expiry: NOW + 60_000,
                    scope: torus_types::SessionScope::Full,
                    created_at: NOW,
                },
            )
            .unwrap();
        let mut invalid_session = session.clone();
        if let ActionSignature::Session { sig, .. } = &mut invalid_session.signature {
            sig.0[0] ^= 1;
        }
        let mut unknown_market = order.clone();
        unknown_market.market_id = 99;
        vec![
            sign(NativeAction::PlaceOrderBatch(vec![order; 400]), NOW),
            invalid_session,
            sign(NativeAction::ClaimRewards, NOW),
            sign(NativeAction::PlaceOrder(unknown_market), NOW),
            session,
            sign(NativeAction::ClaimRewards, 0),
            sign(NativeAction::PlaceOrderBatch(vec![]), NOW),
            sign(NativeAction::CancelOrder { order_id: 3 }, NOW),
        ]
    }

    fn payloads(actions: &[SignedNativeAction], binary: bool) -> Vec<String> {
        let mut out: Vec<_> = actions
            .iter()
            .map(|action| {
                let bytes = if binary {
                    bincode::serialize(action).unwrap()
                } else {
                    serde_json::to_vec(action).unwrap()
                };
                format!("0x{}", hex::encode(bytes))
            })
            .collect();
        out.insert(2, "0xzz".to_string());
        out.insert(4, "0x00".to_string());
        out
    }

    #[test]
    fn acknowledgement_scratch_matches_old_json_and_binary_oracle() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = torus_state::StateDb::open(dir.path()).unwrap();
        let actions = fixtures(&state);
        for binary in [false, true] {
            let decode = if binary {
                decode_action_bin
            } else {
                decode_action_json
            };
            let mut scratch = Vec::new();
            let mut allocation = None;
            let mut successes = 0;
            for payload in payloads(&actions, binary) {
                let expected = old_verify(decode, &payload, &state);
                let actual = verify_one_action_with(
                    decode,
                    &payload,
                    torus_types::eip712::TORUS_CHAIN_ID,
                    &state,
                    NOW,
                    &mut scratch,
                )
                .map(|(sender, action, hash)| {
                    let old_bytes = serde_json::to_vec(&action).unwrap();
                    assert_eq!(
                        scratch, old_bytes,
                        "scratch must reset after large bodies/errors"
                    );
                    (sender, old_bytes, hash)
                });
                assert_eq!(
                    actual, expected,
                    "binary={binary}, including exact error text"
                );
                successes += usize::from(actual.is_ok());
                // First body is largest. Later successes and failures must not
                // discard its reusable allocation or grow it for smaller bodies.
                let current = (scratch.as_ptr(), scratch.capacity());
                if let Some(first) = allocation {
                    assert_eq!(current, first);
                } else {
                    assert!(actual.is_ok(), "largest body must populate the scratch");
                    allocation = Some(current);
                }
            }
            assert_eq!(successes, 4, "both signature types must reach JSON hashing");
        }
    }

    #[test]
    fn acknowledgement_scratch_parallel_results_preserve_index_order() {
        use rayon::prelude::*;
        let dir = tempfile::TempDir::new().unwrap();
        let state = torus_state::StateDb::open(dir.path()).unwrap();
        let actions = fixtures(&state);
        for binary in [false, true] {
            let decode = if binary {
                decode_action_bin
            } else {
                decode_action_json
            };
            let inputs = payloads(&actions, binary);
            let expected: Vec<_> = inputs
                .iter()
                .map(|p| old_verify(decode, p, &state))
                .collect();
            let actual: Vec<_> = ingress_verify_pool().install(|| {
                inputs
                    .into_par_iter()
                    .map_init(Vec::new, |scratch, payload| {
                        verify_one_action_with(
                            decode,
                            &payload,
                            torus_types::eip712::TORUS_CHAIN_ID,
                            &state,
                            NOW,
                            scratch,
                        )
                        .map(|(sender, action, hash)| {
                            (sender, serde_json::to_vec(&action).unwrap(), hash)
                        })
                    })
                    .collect()
            });
            assert_eq!(actual, expected);
        }
    }
}


/// s517 oracle feeder R2: ingress applies the exec submission rules to
/// `SubmitOraclePrices` (the feeder gets an error instead of a silent exec failure).
#[cfg(test)]
mod oracle_ingress_tests {
    use super::*;
    use torus_core::oracle::{MAX_ORACLE_PRICES_PER_SUBMISSION, MAX_ORACLE_PRICE_RAW};
    use torus_types::{FixedPoint, MarketId, NativeAction, OracleSubmission};

    const NOW: u64 = 1_000_000;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn verify(state: &torus_state::StateDb, prices: Vec<(MarketId, FixedPoint)>) -> Result<(), String> {
        let key = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::SubmitOraclePrices(OracleSubmission { prices, timestamp: 0 }),
            NOW,
            &key,
        );
        let payload = format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()));
        verify_one_action(&payload, torus_types::eip712::TORUS_CHAIN_ID, state, NOW).map(|_| ())
    }

    #[test]
    fn oracle_submission_ingress_checks() {
        let dir = tempfile::TempDir::new().unwrap();
        let state = torus_state::StateDb::open(dir.path()).unwrap();
        state.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), b"market").unwrap();
        let over = FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW + 1);
        let many: Vec<_> = (0..=MAX_ORACLE_PRICES_PER_SUBMISSION as u64).map(|m| (m, fp(1))).collect();
        let cases: Vec<(Vec<(MarketId, FixedPoint)>, &str)> = vec![
            (vec![(1, fp(100)), (2, fp(100))], "unknown market_id 2"),
            (vec![(1, fp(100)), (1, fp(101))], "duplicate market 1"),
            (vec![(1, FixedPoint::ZERO)], "invalid oracle price"),
            (vec![(1, fp(-5))], "invalid oracle price"),
            (vec![(1, over)], "invalid oracle price"),
            (vec![], "1..=256"),
            (many, "1..=256"),
        ];
        for (prices, needle) in cases {
            let e = verify(&state, prices).expect_err(needle);
            assert!(e.contains(needle), "{needle}: {e}");
        }
        verify(&state, vec![(1, fp(100))]).unwrap();
        verify(&state, vec![(1, FixedPoint::from_raw(MAX_ORACLE_PRICE_RAW))]).unwrap();
    }
}

/// s92 item B: ingress applies the book's placement tick and lot rules
/// (`OrderBook::place_order_with_accounts`) using the market row's tick/lot,
/// so an off-tick Limit or a sub-lot order gets an error instead of a silent
/// book reject reported as executed.
#[cfg(test)]
mod tick_lot_ingress_tests {
    use super::*;
    use torus_types::{MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

    const NOW: u64 = 1_000_000;

    /// A market row in the genesis / governance borsh layout (`StoredMarket`).
    fn list(state: &torus_state::StateDb, mid: MarketId, tick_raw: i128, lot_raw: i128) {
        use borsh::BorshSerialize;
        let mut row = Vec::new();
        "BTC".to_string().serialize(&mut row).unwrap();
        "USD".to_string().serialize(&mut row).unwrap();
        lot_raw.serialize(&mut row).unwrap();
        tick_raw.serialize(&mut row).unwrap();
        (5 * FixedPoint::SCALE).serialize(&mut row).unwrap();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, &mid.to_be_bytes(), &row)
            .unwrap();
    }

    fn order(
        mid: MarketId,
        price_raw: i128,
        qty_raw: i128,
        order_type: OrderType,
    ) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: mid,
            is_buy: true,
            price: FixedPoint::from_raw(price_raw),
            quantity: FixedPoint::from_raw(qty_raw),
            order_type,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn place(state: &torus_state::StateDb, p: PlaceOrderParams) -> Result<(), String> {
        validate_known_markets(&NativeAction::PlaceOrder(p), state)
    }

    fn db() -> (tempfile::TempDir, torus_state::StateDb) {
        let dir = tempfile::TempDir::new().unwrap();
        let state = torus_state::StateDb::open(dir.path()).unwrap();
        (dir, state)
    }

    const S: i128 = FixedPoint::SCALE;

    #[test]
    fn off_tick_limit_rejected_with_executor_message() {
        let (_d, state) = db();
        list(&state, 1, S / 2, S); // tick 0.5, lot 1
        let e = place(&state, order(1, 100 * S + S / 4, S, OrderType::Limit)).unwrap_err();
        assert_eq!(
            e,
            "order rejected: price 100.25000000 is not a multiple of the tick 0.50000000"
        );
    }

    #[test]
    fn on_tick_limit_passes() {
        let (_d, state) = db();
        list(&state, 1, S / 2, S);
        place(&state, order(1, 100 * S + S / 2, S, OrderType::Limit)).unwrap();
    }

    /// A Market order's price is a slippage cap and a StopMarket has no
    /// limit: neither is tick-checked. Item 6 M1 (row 40): a StopLimit's
    /// LIMIT is (it rests at it once triggered); its trigger and its own
    /// `price` field are not.
    #[test]
    fn only_limit_prices_and_stop_limit_limits_are_tick_checked() {
        let (_d, state) = db();
        list(&state, 1, S, S);
        let odd = 100 * S + 7;
        place(&state, order(1, odd, S, OrderType::Market)).unwrap();
        place(
            &state,
            order(
                1,
                odd,
                S,
                OrderType::StopMarket {
                    trigger: FixedPoint::from_raw(odd),
                },
            ),
        )
        .unwrap();
        let odd_fp = FixedPoint::from_raw(odd);
        let e = place(
            &state,
            order(1, 100 * S, S, OrderType::StopLimit { trigger: odd_fp, limit: odd_fp }),
        )
        .unwrap_err();
        assert_eq!(e, "order rejected: price 100.00000007 is not a multiple of the tick 1.00000000");
        let on_tick = FixedPoint::from_raw(100 * S);
        place(
            &state,
            order(1, odd, S, OrderType::StopLimit { trigger: odd_fp, limit: on_tick }),
        )
        .unwrap();
    }

    /// Item 6 M1 (row 42): the RPC intake check and the executor (which now
    /// creates books from the same market row) give the same answer and the
    /// same text on a market whose tick / lot are not 1, for every order
    /// type: an order the RPC admits passes the executor's pre-book check.
    #[test]
    fn rpc_and_executor_agree_on_a_non_one_tick_market() {
        use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
        use torus_core::position::NativeBalance;
        let (_d, state) = db();
        list(&state, 1, S / 2, S / 10); // tick 0.5, lot 0.1
        let mut ctx = NativeExecContext::new(
            state.clone(),
            1,
            1000,
            0,
            100,
            10,
            alloy_primitives::Address::repeat_byte(99),
            alloy_primitives::Address::repeat_byte(100),
            alloy_primitives::Address::repeat_byte(101),
        );
        let trader = alloy_primitives::Address::repeat_byte(1);
        let bal = NativeBalance { available: FixedPoint::from_raw(1_000_000 * S), order_margin: FixedPoint::ZERO };
        ctx.positions.put_native_balance(&trader, &bal).unwrap();
        let fpr = FixedPoint::from_raw;
        let trig = fpr(200 * S);
        let orders = [
            order(1, 100 * S + S / 2, S / 5, OrderType::Limit),     // ok
            order(1, 100 * S + S / 4, S, OrderType::Limit),         // off tick
            order(1, 100 * S, S / 20, OrderType::Limit),            // below lot
            order(1, 100 * S + 3, S, OrderType::Market),            // cap not checked (no liquidity: book rejects)
            order(1, 0, S, OrderType::StopLimit { trigger: trig, limit: fpr(150 * S + 1) }), // off tick
            order(1, 0, S, OrderType::StopLimit { trigger: trig, limit: fpr(150 * S) }),     // ok
            order(1, 100 * S + 1, S / 20, OrderType::StopMarket { trigger: trig }),         // below lot
        ];
        for (i, p) in orders.into_iter().enumerate() {
            let rpc = place(&state, p.clone());
            let exec = NativeExecutor::execute(&mut ctx, &trader, &NativeAction::PlaceOrder(p));
            match rpc {
                Err(e) => assert_eq!(exec.error.as_deref(), Some(e.as_str()), "#{i}"),
                Ok(()) => assert!(
                    exec.error.as_deref().is_none_or(|e| !e.contains("tick") && !e.contains("lot size")),
                    "#{i}: {:?}",
                    exec.error
                ),
            }
        }
        let book = &ctx.order_books[&1];
        assert_eq!((book.tick_size, book.lot_size), (fpr(S / 2), fpr(S / 10)));
    }

    #[test]
    fn qty_below_lot_rejected_for_every_order_type_and_equal_passes() {
        let (_d, state) = db();
        list(&state, 1, S, S / 10); // tick 1, lot 0.1
        let trig = FixedPoint::from_raw(90 * S);
        for ot in [
            OrderType::Limit,
            OrderType::Market,
            OrderType::StopMarket { trigger: trig },
            OrderType::StopLimit {
                trigger: trig,
                limit: trig,
            },
        ] {
            let e = place(&state, order(1, 100 * S, S / 10 - 1, ot)).unwrap_err();
            assert_eq!(
                e, "order rejected: quantity 0.09999999 below the lot size 0.10000000",
                "{ot:?}"
            );
            place(&state, order(1, 100 * S, S / 10, ot)).unwrap();
        }
    }

    /// Executor: tick <= 0 disables the tick check (no division); the dust
    /// check is a plain `qty < lot`, so lot 0 admits any qty >= 0.
    #[test]
    fn zero_tick_and_zero_lot_match_the_book() {
        let (_d, state) = db();
        list(&state, 1, 0, 0);
        place(&state, order(1, 100 * S + 7, 1, OrderType::Limit)).unwrap();
        place(&state, order(1, 100 * S + 7, 0, OrderType::Limit)).unwrap();
        let e = place(&state, order(1, 100 * S, -1, OrderType::Limit)).unwrap_err();
        assert_eq!(e, "order rejected: quantity -0.00000001 below the lot size 0.00000000");
    }

    /// A batch is one signed action: one bad order rejects the whole action
    /// (as an unknown market does today), each market's row read once.
    #[test]
    fn batch_with_one_bad_order_is_rejected_whole() {
        let (_d, state) = db();
        list(&state, 1, S, S);
        list(&state, 2, S / 2, S);
        let good = vec![
            order(1, 100 * S, S, OrderType::Limit),
            order(2, 100 * S + S / 2, S, OrderType::Limit),
        ];
        validate_known_markets(&NativeAction::PlaceOrderBatch(good.clone()), &state).unwrap();
        let mut bad = good.clone();
        bad.push(order(1, 100 * S + S / 2, S, OrderType::Limit));
        let e = validate_known_markets(&NativeAction::PlaceOrderBatch(bad), &state).unwrap_err();
        assert_eq!(
            e,
            "order rejected: price 100.50000000 is not a multiple of the tick 1.00000000"
        );
        let mut dust = good;
        dust.insert(0, order(2, 100 * S, S - 1, OrderType::Limit));
        let e = validate_known_markets(&NativeAction::PlaceOrderBatch(dust), &state).unwrap_err();
        assert_eq!(e, "order rejected: quantity 0.99999999 below the lot size 1.00000000");
    }

    /// The rejection reaches the client through the normal ingress path
    /// (per-item error string from `verify_one_action`).
    #[test]
    fn verify_one_action_surfaces_the_tick_error() {
        let (_d, state) = db();
        list(&state, 1, S, S);
        let key = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
        let signed = torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrder(order(1, 100 * S + 1, S, OrderType::Limit)),
            NOW,
            &key,
        );
        let payload = format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()));
        let e = verify_one_action(&payload, torus_types::eip712::TORUS_CHAIN_ID, &state, NOW)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            e,
            "order rejected: price 100.00000001 is not a multiple of the tick 1.00000000"
        );
    }

    /// Rows that do not decode as a market (test fixtures seed placeholders)
    /// keep today's behaviour: the market exists, nothing else is checked.
    #[test]
    fn undecodable_row_only_checks_existence() {
        let (_d, state) = db();
        state
            .put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), b"market")
            .unwrap();
        place(&state, order(1, 100 * S + 7, 1, OrderType::Limit)).unwrap();
        let e = place(&state, order(9, 100 * S, S, OrderType::Limit)).unwrap_err();
        assert_eq!(e, "unknown market_id 9");
    }
}

#[cfg(test)]
mod order_book_decode_tests {
    //! S444 / S395 RED-first: `torus_getOrderBook` must decode the PRODUCTION
    //! `OrderBook` blob stored in CF_NATIVE_ORDER_BOOKS. RED at 2a80ff0: the
    //! handler decoded only the legacy test `OrderBookSnapshot` format, so it
    //! errored `borsh decode: Not all bytes read` on every real book — which
    //! blocked the t15 native state-diff the first time it ever ran.

    use torus_core::order_book::OrderBook;
    use torus_core::precompiles::{OrderBookSnapshot, PriceLevel};
    use torus_types::{Address, FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

    use super::decode_order_book_levels;
    use crate::types::dec_fp;

    fn limit(is_buy: bool, price: FixedPoint, quantity: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy,
            price,
            quantity,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    #[test]
    fn decodes_production_order_book_blob() {
        let tick = FixedPoint::from_raw(1);
        let lot = FixedPoint::from_raw(1);
        let mut book = OrderBook::new(1, tick, lot);

        let bid_px = FixedPoint::from_raw(10_000);
        let ask_px = FixedPoint::from_raw(11_000);
        let q1 = FixedPoint::from_raw(500);
        let q2 = FixedPoint::from_raw(700);
        let q3 = FixedPoint::from_raw(300);

        // Two resting bids on ONE level (aggregated), one resting ask. Prices
        // do not cross, so nothing matches.
        book.place_order(limit(true, bid_px, q1), Address::from([1u8; 20]), 1);
        book.place_order(limit(true, bid_px, q2), Address::from([2u8; 20]), 2);
        book.place_order(limit(false, ask_px, q3), Address::from([3u8; 20]), 3);

        // Serialize exactly as `save_order_books` (torus-bridge native_executor.rs) does.
        let blob = borsh::to_vec(&book).expect("serialize prod OrderBook");
        let (bids, asks) =
            decode_order_book_levels(&blob).expect("the PRODUCTION blob must decode");

        assert_eq!(bids.len(), 1, "one aggregated bid level");
        assert_eq!(bids[0].price, dec_fp(bid_px));
        assert_eq!(
            bids[0].quantity,
            dec_fp(FixedPoint::from_raw(1_200)),
            "level quantity must be the SUM of resting remaining quantities"
        );
        assert_eq!(bids[0].order_count, 2, "two resting orders on the level");

        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].price, dec_fp(ask_px));
        assert_eq!(asks[0].quantity, dec_fp(q3));
        assert_eq!(asks[0].order_count, 1);
    }

    #[test]
    fn falls_back_to_legacy_snapshot_blob() {
        let snap = OrderBookSnapshot {
            bids: vec![PriceLevel {
                price: FixedPoint::from_raw(9_000),
                quantity: FixedPoint::from_raw(42),
            }],
            asks: vec![],
        };
        let blob = borsh::to_vec(&snap).expect("serialize legacy snapshot");
        let (bids, asks) =
            decode_order_book_levels(&blob).expect("the legacy snapshot must still decode");
        assert_eq!(bids.len(), 1);
        assert_eq!(bids[0].quantity, dec_fp(FixedPoint::from_raw(42)));
        assert_eq!(asks.len(), 0);
    }
}
