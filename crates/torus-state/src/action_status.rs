//! Per-block action status record, `CF_BLOCK_ACTION_STATUS` (s84 decision 1,
//! v2 per-action execution failures).
//!
//! A certified block is never rejected for something inside it: execution
//! runs every action and SKIPS (no state change) any that fails its validity
//! check (unresolvable signature or session, replayed or duplicate nonce,
//! undecodable EVM tx, a tx revm refuses). A native action that passes those
//! checks executes, and the executor may still refuse it (open-order limit,
//! lot size, a modify's margin, ...): it then FAILED. Row 50: an order
//! refused with a Hyperliquid `*Rejected` status (the book refused it or
//! cancelled it without a fill, or a placement margin / reduce-only / tick /
//! price-band check) was REJECTED. This record says which, so the RPC and the explorer show every action as
//! executed, skipped, failed or rejected.
//!
//! One row per executed block that carries at least one native action or EVM
//! tx or drained a CoreWriter queue row (v4), keyed by 8-byte BE height, written in the block's flush batch (same
//! atomic write as its state and applied-height marker; on the pipelined path
//! the flush worker encodes it into its sidecar of that batch).
//!
//! Node-local derived data, NOT hashed by the running state hash and not a
//! native-root CF (like receipts and block bodies): it is a pure function of
//! the committed block and its pre-state. Every skip decision already shows in
//! hashed state (an executed native action consumes its `CF_NATIVE_NONCES`
//! row, an executed EVM tx moves its sender's account nonce); a failure is the
//! executor's own deterministic result for the action, the same on every
//! validator and in every exec mode.
//!
//! Encoding v1 (a block without failures, byte-identical to the s84 record):
//! `0x01 ‖ evm_count u32 BE ‖ native_count u32 BE ‖ evm bitmap ‖ native
//! bitmap`, each bitmap `ceil(count / 8)` bytes, bit `i % 8` (LSB first) of
//! byte `i / 8` set = action `i` skipped.
//!
//! Encoding v2 (s92 to row 50, read only): `0x02 ‖ <v1 body after the
//! version byte> ‖ failure_count u32 BE ‖ failures`, each failure `index u32
//! BE ‖ order u32 BE ‖ failed_orders u32 BE ‖ reason u8 ‖ msg_len u8 ‖ msg`,
//! in ascending `index` order, `msg` UTF-8 cut to at most
//! [`MAX_MESSAGE_BYTES`]. Every v2 entry is [`Outcome::Failed`].
//!
//! Encoding v3 (row 50, written whenever an action failed or was rejected):
//! v2 with each entry `index u32 BE ‖ order u32 BE ‖ failed_orders u32 BE ‖
//! outcome u8 ‖ reason u8 ‖ msg_len u8 ‖ msg ‖ order_count u32 BE ‖ orders`,
//! each order `outcome u8 ‖ reason u8 ‖ msg_len u8 ‖ msg`. `outcome` is
//! [`Outcome::Failed`] or [`Outcome::Rejected`] for an entry. The per-order
//! list is the room for HL-style per-order statuses (one per order of a
//! PlaceOrderBatch): written empty today; a reader already decodes it.
//! s100 (S2): a [`Outcome::Rejected`] entry or order whose message is its
//! reason's [`FailureReason::book_reject_message`] is written with an empty
//! `msg` (an IOC no-fill entry: 19 B instead of 98), and a reader restores
//! that text for a rejected one with an empty `msg`. Same version byte:
//! records written before still hold the text in full and read the same.
//!
//! Encoding v4 (s112, owner option b; written whenever the block drained at
//! least one CoreWriter queue row): v3 (its `failure_count` may be 0) ‖
//! `core_writer_count u32 BE ‖ results`, one result per drained queue row in
//! queue (sequence) order, each `seq u64 BE ‖ kind u8 ‖ trader 20 B ‖
//! queued_block u64 BE ‖ outcome u8 ‖ reason u8 ‖ msg_len u8 ‖ msg`
//! ([`CoreWriterResult`]). A row is keyed for a client by (this block = the
//! drain height, `seq`): the queue row key is `drain height ‖ seq`. An
//! undecodable row ([`CoreWriterKind::Undecodable`]) has a zero `trader` and
//! `queued_block`. A block that drained CoreWriter rows has a record even
//! without native actions or EVM txs (both counts 0). Read only since v5.
//!
//! Encoding v5 (s114, written instead of v4): v4 with each result followed
//! by `ext u8` and the fields its bits announce: bit 0 (`0x01`) = `oid u128
//! BE`, the order id an executed `PlaceOrder` row's order got (resting or
//! filled). Every other row has `ext = 0` (one byte). An unknown bit does not
//! decode (its field's size is unknown).

/// First byte of a v1 record (executed/skipped only).
const ACTION_STATUS_V1: u8 = 0x01;
/// First byte of a v2 record (v1 + native execution failures; read only).
const ACTION_STATUS_V2: u8 = 0x02;
/// First byte of a v3 record (v2 + outcome per entry + per-order statuses).
const ACTION_STATUS_V3: u8 = 0x03;
/// First byte of a v4 record (v3 + the block's CoreWriter drain results;
/// read only since v5).
const ACTION_STATUS_V4: u8 = 0x04;
/// First byte of a v5 record (v4 + an ext byte per CoreWriter result: the
/// order id of an executed `PlaceOrder` row).
const ACTION_STATUS_V5: u8 = 0x05;
/// v5 CoreWriter result ext flag: an order id (u128 BE) follows.
const CW_EXT_OID: u8 = 0x01;
/// s114: v5 CoreWriter result ext bit 1 — `cloid` u128 BE follows (after the
/// oid when both are set).
const CW_EXT_CLOID: u8 = 0x02;
/// Longest stored failure message, in bytes (cut at a char boundary).
pub const MAX_MESSAGE_BYTES: usize = 96;

/// Why a native action failed at execution: the executor's typed reason
/// (`NativeActionResult::reason`, set where the check failed; the message
/// is stored too). The `u8` codes are stored: never renumber, only add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FailureReason {
    /// Anything without its own code (unknown order, ownership, staking /
    /// governance / oracle / session errors, state read errors). Records
    /// written before row 52 also hold reduce-only rejects here.
    Other = 0,
    /// An account margin check: placement, modify, withdrawal.
    Margin = 1,
    /// Open-order limit reached (plain, or the reduce-only / stop rule).
    OpenLimit = 2,
    /// A limit (or stop-limit limit, or modify) price off the market tick
    /// (a placement: HL `tickRejected`).
    Tick = 3,
    /// Quantity below the market lot size (or a modify quantity <= 0).
    Lot = 4,
    /// Invalid price: not positive (limit, market cap, stop-limit limit,
    /// modify), or a notional that overflows.
    Price = 5,
    /// A PlaceOrderBatch skipped wholesale (empty or over the batch cap).
    BatchCap = 6,
    /// A fill could not be applied at settlement.
    Fill = 7,
    /// s94: an order price outside the price band around the market's
    /// reference price, at placement or modify. A placement is rejected (HL
    /// `oracleRejected`); a modify stays failed.
    PriceBand = 8,
    /// Row 52 (s94 B): a reduce-only order that cannot reduce the position
    /// (no position, or the increasing side), at placement or modify
    /// (HL `reduceOnlyRejected`; the row 50 status maps it). Before row 52
    /// stored as `Other`; an older reader reads code 9 as `Other`.
    ReduceOnly = 9,
    /// Row 50: an IOC limit order that found nothing to fill against (HL
    /// `iocCancelRejected`).
    IocCancel = 10,
    /// Row 50: a PostOnly (ALO) order that would have crossed the book (HL
    /// `badAloPxRejected`).
    BadAloPx = 11,
    /// Row 50: a market order with nothing to fill against within its cap
    /// (HL `marketOrderNoLiquidityRejected`).
    MarketNoLiquidity = 12,
    /// Row 50: a FOK order the book could not fill whole (no HL equivalent:
    /// `fokCancelRejected`, named like `iocCancelRejected`).
    FokCancel = 13,
    /// Row 50: a stop whose trigger is on the wrong side of the last trade
    /// (HL `badTriggerPxRejected`).
    BadTriggerPx = 14,
    /// s114: a placement whose client order id (cloid) is already used by
    /// one of its trader's open orders (resting or pending stop, any
    /// market). Not an HL `*Rejected` status: recorded `Failed`.
    DuplicateCloid = 15,
    /// Pre-testnet P0: an action naming a market that has no
    /// `CF_NATIVE_MARKETS` row (never listed at genesis or by governance):
    /// rejected in consensus with the RPC's text, `unknown market_id N`. Not
    /// an HL `*Rejected` status: recorded `Failed`.
    UnknownMarket = 16,
}

impl FailureReason {
    /// Unknown codes (a newer writer) read as `Other`.
    pub fn from_u8(code: u8) -> Self {
        match code {
            1 => Self::Margin,
            2 => Self::OpenLimit,
            3 => Self::Tick,
            4 => Self::Lot,
            5 => Self::Price,
            6 => Self::BatchCap,
            7 => Self::Fill,
            8 => Self::PriceBand,
            9 => Self::ReduceOnly,
            10 => Self::IocCancel,
            11 => Self::BadAloPx,
            12 => Self::MarketNoLiquidity,
            13 => Self::FokCancel,
            14 => Self::BadTriggerPx,
            15 => Self::DuplicateCloid,
            16 => Self::UnknownMarket,
            _ => Self::Other,
        }
    }

    /// Stable lowercase name (RPC / explorer).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Other => "other",
            Self::Margin => "margin",
            Self::OpenLimit => "open_limit",
            Self::Tick => "tick",
            Self::Lot => "lot",
            Self::Price => "price",
            Self::BatchCap => "batch_cap",
            Self::Fill => "fill",
            Self::PriceBand => "price_band",
            Self::ReduceOnly => "reduce_only",
            Self::IocCancel => "ioc_cancel",
            Self::BadAloPx => "bad_alo_px",
            Self::MarketNoLiquidity => "market_no_liquidity",
            Self::FokCancel => "fok_cancel",
            Self::BadTriggerPx => "bad_trigger_px",
            Self::DuplicateCloid => "duplicate_cloid",
            Self::UnknownMarket => "unknown_market",
        }
    }

    /// Row 50: the Hyperliquid `orderStatus` name of an ORDER refused for
    /// this reason, `None` for a reason that is not an order rejection. An
    /// order placement refused for one of these is recorded
    /// [`Outcome::Rejected`] (the rest stay [`Outcome::Failed`]).
    pub fn hl_rejected_name(self) -> Option<&'static str> {
        Some(match self {
            Self::Tick => "tickRejected",
            Self::PriceBand => "oracleRejected",
            Self::Margin => "perpMarginRejected",
            Self::ReduceOnly => "reduceOnlyRejected",
            Self::IocCancel => "iocCancelRejected",
            Self::BadAloPx => "badAloPxRejected",
            Self::MarketNoLiquidity => "marketOrderNoLiquidityRejected",
            Self::FokCancel => "fokCancelRejected",
            Self::BadTriggerPx => "badTriggerPxRejected",
            _ => return None,
        })
    }

    /// Row 50: the message of a book rejection for this reason (the book
    /// refused the order or cancelled it without a fill; the executor
    /// records this text). s100 S2: a v3 writer stores an EMPTY message for
    /// a [`Outcome::Rejected`] entry or order whose message is exactly this
    /// text, and the reader restores it. Never change a text, and never
    /// give a reason that falls to the `_` arm (e.g. `PriceBand`) an arm of
    /// its own: records already written read back the current text. Pinned
    /// by `book_reject_messages_never_change`.
    pub fn book_reject_message(self) -> &'static str {
        match self {
            Self::IocCancel => {
                "order rejected: IOC order could not immediately match against any resting order"
            }
            Self::BadAloPx => "order rejected: post-only order would have immediately matched",
            Self::MarketNoLiquidity => {
                "order rejected: no liquidity for the market order within its price cap"
            }
            Self::FokCancel => "order rejected: FOK order could not be filled completely",
            Self::ReduceOnly => "reduce-only order rejected: would not reduce the position",
            Self::Margin => "insufficient margin: none left for the first fill (match time)",
            Self::BadTriggerPx => {
                "order rejected: stop trigger price is on the wrong side of the last trade"
            }
            Self::Lot => "order rejected: quantity below the lot size",
            Self::Price => "order rejected: price must be positive",
            Self::Tick => "order rejected: price is not a multiple of the tick",
            Self::DuplicateCloid => {
                "order rejected: client order id is already used by an open order"
            }
            _ => "order rejected by the book",
        }
    }
}

/// Row 50: what an executed native action (or one order of a batch) came
/// to. The `u8` codes are stored: never renumber, only add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Outcome {
    /// Only in a per-order list: this order executed (rests, filled, or
    /// partly filled with the rest cancelled).
    Executed = 0,
    /// The executor refused it (state, limits, shape; not an HL order
    /// status).
    Failed = 1,
    /// An order refused with an HL `*Rejected` status (the book refused or
    /// cancelled it without a fill, or a placement margin / reduce-only /
    /// tick / price-band check): see [`FailureReason::hl_rejected_name`].
    Rejected = 2,
}

impl Outcome {
    /// Unknown codes (a newer writer) read as `Failed`.
    fn from_u8(code: u8) -> Self {
        match code {
            0 => Self::Executed,
            2 => Self::Rejected,
            _ => Self::Failed,
        }
    }

    /// `"executed"`, `"failed"` or `"rejected"` (RPC / explorer).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Executed => "executed",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
        }
    }
}

/// v4: what kind of CoreWriter / lockbox queue row a [`CoreWriterResult`]
/// is. The codes mirror the queue row's own borsh tags; stored: never
/// renumber, only add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CoreWriterKind {
    PlaceOrder = 0,
    CancelOrder = 1,
    CancelAll = 2,
    Delegate = 3,
    Undelegate = 4,
    ClaimRewards = 5,
    LockPermanent = 6,
    ClaimUnbonded = 7,
    /// s114: CoreWriter `cancelByCloid`.
    CancelByCloid = 8,
    LockboxDeposit = 0x20,
    LockboxWithdraw = 0x21,
    /// A code this reader does not know (a newer writer).
    Unknown = 0xFE,
    /// The queue row was present but did not decode (A-exception, s111).
    Undecodable = 0xFF,
}

impl CoreWriterKind {
    /// Unknown codes read as [`Self::Unknown`].
    pub fn from_u8(code: u8) -> Self {
        match code {
            0 => Self::PlaceOrder,
            1 => Self::CancelOrder,
            2 => Self::CancelAll,
            3 => Self::Delegate,
            4 => Self::Undelegate,
            5 => Self::ClaimRewards,
            6 => Self::LockPermanent,
            7 => Self::ClaimUnbonded,
            8 => Self::CancelByCloid,
            0x20 => Self::LockboxDeposit,
            0x21 => Self::LockboxWithdraw,
            0xFF => Self::Undecodable,
            _ => Self::Unknown,
        }
    }

    /// Stable lowercase name (RPC).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlaceOrder => "place_order",
            Self::CancelOrder => "cancel_order",
            Self::CancelAll => "cancel_all",
            Self::Delegate => "delegate",
            Self::Undelegate => "undelegate",
            Self::ClaimRewards => "claim_rewards",
            Self::LockPermanent => "lock_permanent",
            Self::ClaimUnbonded => "claim_unbonded",
            Self::CancelByCloid => "cancel_by_cloid",
            Self::LockboxDeposit => "lockbox_deposit",
            Self::LockboxWithdraw => "lockbox_withdraw",
            Self::Unknown => "unknown",
            Self::Undecodable => "undecodable",
        }
    }
}

/// v4: the result of one CoreWriter / lockbox queue row the block drained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreWriterResult {
    /// The row's sequence in this block's queue (its key's low 8 bytes; the
    /// key is `drain height ‖ seq`).
    pub seq: u64,
    pub kind: CoreWriterKind,
    /// The precompile caller (`msg.sender`) that queued it; zero for an
    /// undecodable row.
    pub trader: alloy_primitives::Address,
    /// The block whose EVM queued it (the row's own field); zero for an
    /// undecodable row.
    pub queued_block: u64,
    /// `Executed`, `Failed`, or `Rejected` (a CoreWriter order refused with
    /// an HL `*Rejected` status, as for a native placement).
    pub outcome: Outcome,
    /// `Other` for an executed row.
    pub reason: FailureReason,
    /// Empty for an executed row; for a `Rejected` one, empty stands for its
    /// reason's [`FailureReason::book_reject_message`] (as in v3).
    pub message: String,
    /// s114 (v5): the order id an executed `PlaceOrder` row's order got
    /// (resting or filled); `None` for every other row (and in v4 records).
    pub oid: Option<u128>,
    /// s114 (v5 ext bit 1): the client order id the row carried (a
    /// `place_order` with one, a `cancel_by_cloid`), whatever its outcome.
    pub cloid: Option<u128>,
}

/// Row 50 (v3): one order's status inside an entry (see
/// [`NativeActionFailure::orders`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderOutcome {
    pub outcome: Outcome,
    pub reason: FailureReason,
    pub message: String,
}

/// One native action the executor refused (failed) or rejected, by body
/// position.
///
/// A PlaceOrderBatch is one action of many orders: it has an entry if ANY of
/// its orders did not execute; `order` is the position (inside the batch) of
/// the first such order, whose outcome, reason and message are the ones
/// recorded, and `failed_orders` counts its orders that did not execute
/// (failed or rejected; the others executed). A batch skipped wholesale
/// (empty / over the cap) has `failed_orders = 0`. For any other action
/// `order = 0`, `failed_orders = 1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeActionFailure {
    pub index: u32,
    pub order: u32,
    pub failed_orders: u32,
    pub reason: FailureReason,
    /// s100 S2: empty for a `Rejected` entry stands for its reason's
    /// [`FailureReason::book_reject_message`] (the record's own form: the
    /// executor's path leaves it empty, [`BlockActionStatus::decode`] gives
    /// the text).
    pub message: String,
    /// Row 50 (v3): `Failed` or `Rejected` (v1 / v2 entries: `Failed`).
    pub outcome: Outcome,
    /// Row 50 (v3): per-order statuses of a batch, in order; empty = not
    /// recorded (every record written today).
    pub orders: Vec<OrderOutcome>,
}

impl NativeActionFailure {
    pub fn new(
        index: u32,
        order: u32,
        failed_orders: u32,
        reason: FailureReason,
        message: String,
    ) -> Self {
        Self {
            index,
            order,
            failed_orders,
            reason,
            message,
            outcome: Outcome::Failed,
            orders: Vec::new(),
        }
    }
}

/// Which actions of one block execution skipped or failed, by position in the
/// block body (`evm_transactions`, `native_actions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockActionStatus {
    pub evm_skipped: Vec<bool>,
    pub native_skipped: Vec<bool>,
    /// Native actions that executed and failed or were rejected, ascending
    /// `index`; never a skipped one.
    pub native_failed: Vec<NativeActionFailure>,
    /// v4: one result per CoreWriter queue row drained in this block, queue
    /// (sequence) order.
    pub core_writer: Vec<CoreWriterResult>,
}

fn push_bitmap(out: &mut Vec<u8>, bits: &[bool]) {
    let start = out.len();
    out.resize(start + bits.len().div_ceil(8), 0);
    for (i, _) in bits.iter().enumerate().filter(|(_, skipped)| **skipped) {
        out[start + i / 8] |= 1 << (i % 8);
    }
}

fn read_bitmap(bytes: &[u8], count: usize) -> Vec<bool> {
    (0..count)
        .map(|i| bytes[i / 8] & (1 << (i % 8)) != 0)
        .collect()
}

/// `s` cut to at most `max` bytes at a char boundary.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = bytes.split_at_checked(n)?;
    *bytes = rest;
    Some(head)
}

fn take_u32(bytes: &mut &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(take(bytes, 4)?.try_into().ok()?))
}

fn take_u8(bytes: &mut &[u8]) -> Option<u8> {
    take(bytes, 1).map(|b| b[0])
}

/// `msg_len u8 ‖ msg`, cut to [`MAX_MESSAGE_BYTES`]; empty for a rejection
/// with its reason's canonical message (s100 S2,
/// [`FailureReason::book_reject_message`]).
fn push_message(out: &mut Vec<u8>, outcome: Outcome, reason: FailureReason, message: &str) {
    let msg = if outcome == Outcome::Rejected && message == reason.book_reject_message() {
        ""
    } else {
        truncate_utf8(message, MAX_MESSAGE_BYTES)
    };
    out.push(msg.len() as u8);
    out.extend_from_slice(msg.as_bytes());
}

/// The message [`push_message`] stored: a rejection's empty message is its
/// reason's canonical one.
fn take_message(bytes: &mut &[u8], outcome: Outcome, reason: FailureReason) -> Option<String> {
    let len = take_u8(bytes)? as usize;
    let msg = std::str::from_utf8(take(bytes, len)?).ok()?;
    Some(
        if msg.is_empty() && outcome == Outcome::Rejected {
            reason.book_reject_message()
        } else {
            msg
        }
        .to_string(),
    )
}

impl BlockActionStatus {
    /// v1 when nothing failed or was rejected and no CoreWriter row was
    /// drained (the s84 bytes exactly), v5 when a CoreWriter row was
    /// drained, else v3.
    pub fn encode(&self) -> Vec<u8> {
        let core_writer = !self.core_writer.is_empty();
        let failures = core_writer || !self.native_failed.is_empty();
        let mut out = Vec::with_capacity(
            9 + self.evm_skipped.len().div_ceil(8) + self.native_skipped.len().div_ceil(8),
        );
        out.push(if core_writer {
            ACTION_STATUS_V5
        } else if failures {
            ACTION_STATUS_V3
        } else {
            ACTION_STATUS_V1
        });
        out.extend_from_slice(&(self.evm_skipped.len() as u32).to_be_bytes());
        out.extend_from_slice(&(self.native_skipped.len() as u32).to_be_bytes());
        push_bitmap(&mut out, &self.evm_skipped);
        push_bitmap(&mut out, &self.native_skipped);
        if failures {
            out.extend_from_slice(&(self.native_failed.len() as u32).to_be_bytes());
            for f in &self.native_failed {
                out.extend_from_slice(&f.index.to_be_bytes());
                out.extend_from_slice(&f.order.to_be_bytes());
                out.extend_from_slice(&f.failed_orders.to_be_bytes());
                out.push(f.outcome as u8);
                out.push(f.reason as u8);
                push_message(&mut out, f.outcome, f.reason, &f.message);
                out.extend_from_slice(&(f.orders.len() as u32).to_be_bytes());
                for o in &f.orders {
                    out.push(o.outcome as u8);
                    out.push(o.reason as u8);
                    push_message(&mut out, o.outcome, o.reason, &o.message);
                }
            }
        }
        if core_writer {
            out.extend_from_slice(&(self.core_writer.len() as u32).to_be_bytes());
            for r in &self.core_writer {
                out.extend_from_slice(&r.seq.to_be_bytes());
                out.push(r.kind as u8);
                out.extend_from_slice(r.trader.as_slice());
                out.extend_from_slice(&r.queued_block.to_be_bytes());
                out.push(r.outcome as u8);
                out.push(r.reason as u8);
                push_message(&mut out, r.outcome, r.reason, &r.message);
                let ext = if r.oid.is_some() { CW_EXT_OID } else { 0 }
                    | if r.cloid.is_some() { CW_EXT_CLOID } else { 0 };
                out.push(ext);
                for v in [r.oid, r.cloid].into_iter().flatten() {
                    out.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
        out
    }

    /// `None` for an unknown version or a truncated / overlong record.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&version, mut rest) = bytes.split_first()?;
        if !matches!(
            version,
            ACTION_STATUS_V1
                | ACTION_STATUS_V2
                | ACTION_STATUS_V3
                | ACTION_STATUS_V4
                | ACTION_STATUS_V5
        ) {
            return None;
        }
        let evm_count = take_u32(&mut rest)? as usize;
        let native_count = take_u32(&mut rest)? as usize;
        let evm_bits = take(&mut rest, evm_count.div_ceil(8))?;
        let native_bits = take(&mut rest, native_count.div_ceil(8))?;
        let mut native_failed = Vec::new();
        if version != ACTION_STATUS_V1 {
            let v3 = version >= ACTION_STATUS_V3;
            let count = take_u32(&mut rest)?;
            for _ in 0..count {
                let index = take_u32(&mut rest)?;
                let order = take_u32(&mut rest)?;
                let failed_orders = take_u32(&mut rest)?;
                let outcome = if v3 {
                    Outcome::from_u8(take_u8(&mut rest)?)
                } else {
                    Outcome::Failed
                };
                let reason = FailureReason::from_u8(take_u8(&mut rest)?);
                let message = take_message(&mut rest, outcome, reason)?;
                let mut orders = Vec::new();
                if v3 {
                    for _ in 0..take_u32(&mut rest)? {
                        let outcome = Outcome::from_u8(take_u8(&mut rest)?);
                        let reason = FailureReason::from_u8(take_u8(&mut rest)?);
                        let message = take_message(&mut rest, outcome, reason)?;
                        orders.push(OrderOutcome {
                            outcome,
                            reason,
                            message,
                        });
                    }
                }
                if index as usize >= native_count || outcome == Outcome::Executed {
                    return None;
                }
                native_failed.push(NativeActionFailure {
                    index,
                    order,
                    failed_orders,
                    reason,
                    message,
                    outcome,
                    orders,
                });
            }
        }
        let mut core_writer = Vec::new();
        if version >= ACTION_STATUS_V4 {
            for _ in 0..take_u32(&mut rest)? {
                let seq = u64::from_be_bytes(take(&mut rest, 8)?.try_into().ok()?);
                let kind = CoreWriterKind::from_u8(take_u8(&mut rest)?);
                let trader = alloy_primitives::Address::from_slice(take(&mut rest, 20)?);
                let queued_block = u64::from_be_bytes(take(&mut rest, 8)?.try_into().ok()?);
                let outcome = Outcome::from_u8(take_u8(&mut rest)?);
                let reason = FailureReason::from_u8(take_u8(&mut rest)?);
                let message = take_message(&mut rest, outcome, reason)?;
                let mut oid = None;
                let mut cloid = None;
                if version == ACTION_STATUS_V5 {
                    let ext = take_u8(&mut rest)?;
                    // A bit this reader does not know: its field's size is
                    // unknown, so the record does not decode.
                    if ext & !(CW_EXT_OID | CW_EXT_CLOID) != 0 {
                        return None;
                    }
                    let mut u128_field = |bit| -> Option<Option<u128>> {
                        if ext & bit == 0 {
                            return Some(None);
                        }
                        Some(Some(u128::from_be_bytes(
                            take(&mut rest, 16)?.try_into().ok()?,
                        )))
                    };
                    oid = u128_field(CW_EXT_OID)?;
                    cloid = u128_field(CW_EXT_CLOID)?;
                }
                core_writer.push(CoreWriterResult {
                    seq,
                    kind,
                    trader,
                    queued_block,
                    outcome,
                    reason,
                    message,
                    oid,
                    cloid,
                });
            }
        }
        if !rest.is_empty() {
            return None;
        }
        Some(Self {
            evm_skipped: read_bitmap(evm_bits, evm_count),
            native_skipped: read_bitmap(native_bits, native_count),
            native_failed,
            core_writer,
        })
    }

    /// Native action `i`'s label: `"skipped"`, `"failed"`, `"rejected"` or
    /// `"executed"`.
    pub fn native_label(&self, i: usize) -> &'static str {
        if self.native_skipped.get(i).copied().unwrap_or(false) {
            "skipped"
        } else {
            self.failure(i).map_or(Outcome::Executed, |f| f.outcome).as_str()
        }
    }

    /// Native action `i`'s failure, if it failed.
    pub fn failure(&self, i: usize) -> Option<&NativeActionFailure> {
        self.native_failed
            .binary_search_by_key(&i, |f| f.index as usize)
            .ok()
            .map(|k| &self.native_failed[k])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// s100 S2: rejected entries with the canonical text are stored empty,
    /// so the text read back for every code (unknown codes too) is frozen.
    #[test]
    fn book_reject_messages_never_change() {
        let fallback = "order rejected by the book";
        let want = [
            fallback,
            "insufficient margin: none left for the first fill (match time)",
            fallback,
            "order rejected: price is not a multiple of the tick",
            "order rejected: quantity below the lot size",
            "order rejected: price must be positive",
            fallback,
            fallback,
            fallback,
            "reduce-only order rejected: would not reduce the position",
            "order rejected: IOC order could not immediately match against any resting order",
            "order rejected: post-only order would have immediately matched",
            "order rejected: no liquidity for the market order within its price cap",
            "order rejected: FOK order could not be filled completely",
            "order rejected: stop trigger price is on the wrong side of the last trade",
            "order rejected: client order id is already used by an open order",
            fallback,
        ];
        for (code, text) in want.iter().enumerate() {
            assert_eq!(
                FailureReason::from_u8(code as u8).book_reject_message(),
                *text,
                "code {code}"
            );
        }
    }

    fn failure(
        index: u32,
        order: u32,
        failed: u32,
        reason: FailureReason,
        msg: &str,
    ) -> NativeActionFailure {
        NativeActionFailure::new(index, order, failed, reason, msg.to_string())
    }

    #[test]
    fn round_trips_and_packs_one_bit_per_action() {
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![false, true, false],
            native_skipped: (0..10).map(|i| i == 0 || i == 9).collect(),
            native_failed: vec![],
        };
        let bytes = status.encode();
        // version + 2 counts + 1 EVM byte + 2 native bytes.
        assert_eq!(bytes.len(), 1 + 8 + 1 + 2);
        assert_eq!(bytes[0], ACTION_STATUS_V1, "no failure keeps the v1 form");
        assert_eq!(bytes[9], 0b010);
        assert_eq!(&bytes[10..], &[0b1, 0b10]);
        assert_eq!(BlockActionStatus::decode(&bytes), Some(status));
    }

    /// The exact bytes the s84 (v1-only) writer produced still read.
    #[test]
    fn v1_records_written_before_v2_still_read() {
        let v1 = [0x01, 0, 0, 0, 1, 0, 0, 0, 3, 0b1, 0b100];
        let status = BlockActionStatus::decode(&v1).expect("v1 decodes");
        assert_eq!(status.evm_skipped, vec![true]);
        assert_eq!(status.native_skipped, vec![false, false, true]);
        assert!(status.native_failed.is_empty());
        assert_eq!(status.native_label(0), "executed");
        assert_eq!(status.native_label(2), "skipped");
    }

    /// Row 50: a v3 record round-trips failed and rejected entries and the
    /// per-order statuses (written empty today; a later writer fills one per
    /// order of a batch, HL style, with no new version).
    #[test]
    fn v3_round_trips_rejections_and_per_order_statuses() {
        let rejected = |index, order, count, reason| NativeActionFailure {
            outcome: Outcome::Rejected,
            ..failure(index, order, count, reason, "order rejected: x")
        };
        let mut batch = rejected(2, 1, 2, FailureReason::IocCancel);
        batch.orders = vec![
            OrderOutcome { outcome: Outcome::Executed, reason: FailureReason::Other, message: String::new() },
            OrderOutcome { outcome: Outcome::Rejected, reason: FailureReason::IocCancel, message: "ioc".into() },
            OrderOutcome { outcome: Outcome::Failed, reason: FailureReason::Tick, message: "tick".into() },
        ];
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![true],
            native_skipped: vec![false, false, false, true, false],
            native_failed: vec![
                failure(0, 0, 1, FailureReason::Tick, "off tick"),
                rejected(1, 0, 1, FailureReason::BadAloPx),
                batch,
                rejected(4, 0, 1, FailureReason::Margin),
            ],
        };
        let bytes = status.encode();
        assert_eq!(bytes[0], ACTION_STATUS_V3, "any entry writes v3");
        let back = BlockActionStatus::decode(&bytes).expect("v3 decodes");
        assert_eq!(back, status);
        let labels: Vec<_> = (0..5).map(|i| back.native_label(i)).collect();
        assert_eq!(labels, ["failed", "rejected", "rejected", "skipped", "rejected"]);
        assert_eq!(back.failure(2).unwrap().orders.len(), 3);
        // An entry can never be "executed".
        let mut bad = bytes.clone();
        // version 1 + counts 8 + bitmaps 1+1 + entry count 4 + index, order,
        // count 12 -> the first entry's outcome byte.
        assert_eq!(bad[27], Outcome::Failed as u8);
        bad[27] = Outcome::Executed as u8;
        assert_eq!(BlockActionStatus::decode(&bad), None);
    }

    /// A v3 record of one entry for native action 0 with one per-order
    /// status, and the offsets of its entry outcome byte, its order_count and
    /// its per-order outcome byte.
    fn v3_one_entry() -> (Vec<u8>, usize, usize, usize) {
        let mut entry = failure(0, 0, 1, FailureReason::IocCancel, "ab");
        entry.outcome = Outcome::Rejected;
        entry.orders = vec![OrderOutcome {
            outcome: Outcome::Rejected,
            reason: FailureReason::IocCancel,
            message: "c".into(),
        }];
        let bytes = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![false],
            native_failed: vec![entry],
        }
        .encode();
        // version 1 + counts 8 + bitmaps 0+1 + entry count 4 + index, order,
        // count 12 -> the entry outcome; + outcome, reason, msg_len 1+1+1 +
        // msg 2 -> order_count; + 4 -> the order's outcome.
        let (outcome_at, count_at) = (26, 31);
        assert_eq!(bytes[outcome_at], Outcome::Rejected as u8);
        assert_eq!(&bytes[count_at..count_at + 4], &1u32.to_be_bytes());
        assert_eq!(bytes[count_at + 4], Outcome::Rejected as u8);
        assert_eq!(bytes.len(), count_at + 4 + 1 + 1 + 1 + 1, "the record ends with the order's msg");
        (bytes, outcome_at, count_at, count_at + 4)
    }

    /// Row 50 review: a v3 per-order list shorter than its order_count (cut
    /// mid-order, or a count larger than the bytes, up to u32::MAX) is a
    /// truncated record: `None`, like every other truncation, never a panic
    /// or an allocation sized by the count.
    #[test]
    fn v3_truncated_per_order_list_does_not_decode() {
        let (bytes, _, count_at, _) = v3_one_entry();
        assert!(BlockActionStatus::decode(&bytes).is_some(), "the base record decodes");
        for cut in count_at..bytes.len() {
            assert_eq!(BlockActionStatus::decode(&bytes[..cut]), None, "cut at {cut}");
        }
        for count in [2u32, 3, u32::MAX] {
            let mut more = bytes.clone();
            more[count_at..count_at + 4].copy_from_slice(&count.to_be_bytes());
            assert_eq!(BlockActionStatus::decode(&more), None, "order_count {count}");
        }
        // A count smaller than the list leaves bytes over: also rejected.
        let mut fewer = bytes.clone();
        fewer[count_at..count_at + 4].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(BlockActionStatus::decode(&fewer), None, "order_count 0 with one order");
    }

    /// Row 50 review: an outcome code this reader does not know (a newer
    /// writer) reads as `Failed`, in an entry and in a per-order status (as
    /// an unknown reason code reads as `Other`); no panic. Only `Executed`
    /// is refused, and only for an entry.
    #[test]
    fn v3_unknown_outcome_code_reads_as_failed() {
        let (bytes, outcome_at, _, order_outcome_at) = v3_one_entry();
        for code in 3..=255u8 {
            let mut entry = bytes.clone();
            entry[outcome_at] = code;
            let status = BlockActionStatus::decode(&entry).expect("unknown entry outcome decodes");
            assert_eq!(status.native_failed[0].outcome, Outcome::Failed, "code {code}");
            assert_eq!(status.native_label(0), "failed", "code {code}");
            assert_eq!(status.native_failed[0].orders[0].outcome, Outcome::Rejected);

            let mut order = bytes.clone();
            order[order_outcome_at] = code;
            let status = BlockActionStatus::decode(&order).expect("unknown order outcome decodes");
            assert_eq!(status.native_failed[0].orders[0].outcome, Outcome::Failed, "code {code}");
            assert_eq!(status.native_failed[0].outcome, Outcome::Rejected);
        }
        let mut executed_order = bytes.clone();
        executed_order[order_outcome_at] = Outcome::Executed as u8;
        let status = BlockActionStatus::decode(&executed_order).expect("an executed order decodes");
        assert_eq!(status.native_failed[0].orders[0].outcome, Outcome::Executed);
        let mut executed_entry = bytes;
        executed_entry[outcome_at] = Outcome::Executed as u8;
        assert_eq!(BlockActionStatus::decode(&executed_entry), None);
    }

    /// The exact bytes the v2 writer produced (s92 to row 50) still read:
    /// every entry is a failure, no per-order statuses.
    #[test]
    fn v2_records_written_before_v3_still_read() {
        let v2 = [
            0x02, 0, 0, 0, 0, 0, 0, 0, 2, 0b10, // v1 body: 0 evm, 2 native, #1 skipped
            0, 0, 0, 1, // one failure
            0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 2, 1, 2, b'n', b'o', // #0, order 3, 2 failed, margin, "no"
        ];
        let status = BlockActionStatus::decode(&v2).expect("v2 decodes");
        assert_eq!(status.native_skipped, vec![false, true]);
        assert_eq!(
            status.native_failed,
            vec![failure(0, 3, 2, FailureReason::Margin, "no")],
            "a v2 entry reads as failed, without per-order statuses"
        );
        assert_eq!(status.native_failed[0].outcome, Outcome::Failed);
        assert_eq!(status.native_label(0), "failed");
        assert_eq!(status.native_label(1), "skipped");
    }

    /// Row 50: the rejection reasons and their Hyperliquid status names
    /// (`orderStatus`; FOK has none in HL: `fokCancelRejected`, named like
    /// `iocCancelRejected`). Every other reason has no rejected name.
    /// Row 50 review (S3, owner): off-tick -> `tickRejected`, outside the
    /// price band -> `oracleRejected`.
    #[test]
    fn rejected_reasons_have_hyperliquid_names() {
        let named = [
            (FailureReason::Tick, "tickRejected"),
            (FailureReason::PriceBand, "oracleRejected"),
            (FailureReason::Margin, "perpMarginRejected"),
            (FailureReason::ReduceOnly, "reduceOnlyRejected"),
            (FailureReason::IocCancel, "iocCancelRejected"),
            (FailureReason::BadAloPx, "badAloPxRejected"),
            (FailureReason::MarketNoLiquidity, "marketOrderNoLiquidityRejected"),
            (FailureReason::FokCancel, "fokCancelRejected"),
            (FailureReason::BadTriggerPx, "badTriggerPxRejected"),
        ];
        for (reason, name) in named {
            assert_eq!(reason.hl_rejected_name(), Some(name));
        }
        for code in 0..=255u8 {
            let reason = FailureReason::from_u8(code);
            if !named.iter().any(|(r, _)| *r == reason) {
                assert_eq!(reason.hl_rejected_name(), None, "{reason:?}");
            }
        }
    }

    /// A one-action record holding `entry`, and the bytes of that entry
    /// (after version 1 + counts 8 + bitmaps 0+1 + entry count 4).
    fn one_entry_record(entry: NativeActionFailure) -> (BlockActionStatus, Vec<u8>) {
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![false],
            native_failed: vec![entry],
        };
        let bytes = status.encode();
        assert_eq!(bytes[0], ACTION_STATUS_V3);
        let entry_bytes = bytes[14..].to_vec();
        (status, entry_bytes)
    }

    fn rejected_entry(reason: FailureReason, msg: &str) -> NativeActionFailure {
        NativeActionFailure {
            outcome: Outcome::Rejected,
            ..failure(0, 0, 1, reason, msg)
        }
    }

    /// s100 S2 (option 1): a rejected entry whose message is its reason's
    /// canonical book message stores an empty message (an IOC no-fill entry:
    /// 98 B -> 19 B, still v3); the reader restores the text.
    #[test]
    fn v3_canonical_rejection_message_is_not_stored() {
        let ioc = FailureReason::IocCancel.book_reject_message();
        assert_eq!(ioc.len(), 79, "the message an IOC no-fill used to store");
        let (status, entry) = one_entry_record(rejected_entry(FailureReason::IocCancel, ioc));
        // index 4 + order 4 + failed_orders 4 + outcome 1 + reason 1 +
        // msg_len 1 (0, no msg) + order_count 4.
        assert_eq!(entry.len(), 19, "was 19 + 79 = 98 B");
        assert_eq!(entry[14], 0, "msg_len 0");
        let back = BlockActionStatus::decode(&status.encode()).expect("v3 decodes");
        assert_eq!(back, status, "the full message comes back");
        assert_eq!(back.native_failed[0].message, ioc);

        // Every book rejection, and a per-order status, the same way.
        for code in 0..=14u8 {
            let reason = FailureReason::from_u8(code);
            let mut entry = rejected_entry(reason, reason.book_reject_message());
            entry.orders = vec![OrderOutcome {
                outcome: Outcome::Rejected,
                reason,
                message: reason.book_reject_message().to_string(),
            }];
            let (status, bytes) = one_entry_record(entry);
            assert_eq!(
                bytes.len(),
                19 + 3,
                "{reason:?}: entry + one order, no message bytes"
            );
            assert_eq!(
                BlockActionStatus::decode(&status.encode()),
                Some(status),
                "{reason:?}"
            );
        }
    }

    /// s100 S2: v3 records written before option 1 hold the canonical
    /// message in full; they decode exactly as before (and re-encode
    /// shorter, to the same status).
    #[test]
    fn v3_old_rejected_entry_with_full_message_decodes_unchanged() {
        let ioc = FailureReason::IocCancel.book_reject_message();
        let mut old = vec![0x03, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
        old.extend_from_slice(&[0; 12]); // index 0, order 0 ...
        old[25] = 1; // ... failed_orders 1
        old.extend_from_slice(&[Outcome::Rejected as u8, FailureReason::IocCancel as u8, 79]);
        old.extend_from_slice(ioc.as_bytes());
        old.extend_from_slice(&[0; 4]); // no per-order statuses
        assert_eq!(old.len(), 14 + 98);
        let status = BlockActionStatus::decode(&old).expect("an old v3 record decodes");
        assert_eq!(
            status.native_failed,
            vec![rejected_entry(FailureReason::IocCancel, ioc)]
        );
        assert_eq!(status.native_label(0), "rejected");
        let new = status.encode();
        assert_eq!(new.len(), 14 + 19);
        assert_eq!(BlockActionStatus::decode(&new), Some(status));
    }

    /// s100 S2: only the exact canonical text of a REJECTED entry is left
    /// out. A rejected entry with its own message (a placement-time check),
    /// another reason's text, or a failed entry with a canonical text are
    /// stored verbatim.
    #[test]
    fn v3_non_canonical_messages_are_stored_verbatim() {
        let placement_margin = "insufficient margin: need 5, have 1 (account)";
        let cases = [
            rejected_entry(FailureReason::Margin, placement_margin),
            rejected_entry(
                FailureReason::Tick,
                "price 100.25 is not a multiple of the tick 0.5",
            ),
            rejected_entry(
                FailureReason::BadAloPx,
                FailureReason::IocCancel.book_reject_message(),
            ),
            failure(
                0,
                0,
                1,
                FailureReason::IocCancel,
                FailureReason::IocCancel.book_reject_message(),
            ),
        ];
        for entry in cases {
            let message = entry.message.clone();
            let (status, bytes) = one_entry_record(entry);
            assert_eq!(bytes.len(), 19 + message.len(), "{message} is stored");
            assert_eq!(&bytes[15..15 + message.len()], message.as_bytes());
            assert_eq!(BlockActionStatus::decode(&status.encode()), Some(status));
        }
    }

    #[test]
    fn v3_round_trips_failures() {
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![true, false, false, false],
            native_failed: vec![
                failure(
                    1,
                    0,
                    1,
                    FailureReason::Margin,
                    "insufficient margin: need 5, have 1 (account)",
                ),
                failure(
                    3,
                    7,
                    2,
                    FailureReason::OpenLimit,
                    "open order limit reached: 1000 open orders, limit 1000",
                ),
            ],
        };
        let bytes = status.encode();
        assert_eq!(bytes[0], ACTION_STATUS_V3);
        let back = BlockActionStatus::decode(&bytes).expect("v3 decodes");
        assert_eq!(back, status);
        assert_eq!(back.native_failed[0].reason, FailureReason::Margin);
        assert_eq!(back.native_failed[1].reason, FailureReason::OpenLimit);
        assert_eq!(back.native_label(0), "skipped");
        assert_eq!(back.native_label(1), "failed");
        assert_eq!(back.native_label(2), "executed");
        assert_eq!(back.failure(3).map(|f| f.order), Some(7));
        assert!(back.failure(2).is_none());
    }

    #[test]
    fn long_messages_are_cut_at_a_char_boundary() {
        let long = format!("x{}", "é".repeat(100));
        let status = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![false],
            native_failed: vec![failure(0, 0, 1, FailureReason::Other, &long)],
        };
        let back = BlockActionStatus::decode(&status.encode()).unwrap();
        let msg = &back.native_failed[0].message;
        assert!(msg.len() <= MAX_MESSAGE_BYTES && msg.len() >= MAX_MESSAGE_BYTES - 1);
        assert!(long.starts_with(msg.as_str()));
    }

    /// The stored codes and names are a format: pinned, one per reason, and
    /// a stored code always reads back as the reason it was written as.
    #[test]
    fn reason_codes_and_names_are_stable() {
        let all = [
            (FailureReason::Other, 0, "other"),
            (FailureReason::Margin, 1, "margin"),
            (FailureReason::OpenLimit, 2, "open_limit"),
            (FailureReason::Tick, 3, "tick"),
            (FailureReason::Lot, 4, "lot"),
            (FailureReason::Price, 5, "price"),
            (FailureReason::BatchCap, 6, "batch_cap"),
            (FailureReason::Fill, 7, "fill"),
            (FailureReason::PriceBand, 8, "price_band"),
            (FailureReason::ReduceOnly, 9, "reduce_only"),
            (FailureReason::IocCancel, 10, "ioc_cancel"),
            (FailureReason::BadAloPx, 11, "bad_alo_px"),
            (FailureReason::MarketNoLiquidity, 12, "market_no_liquidity"),
            (FailureReason::FokCancel, 13, "fok_cancel"),
            (FailureReason::BadTriggerPx, 14, "bad_trigger_px"),
            (FailureReason::DuplicateCloid, 15, "duplicate_cloid"),
            (FailureReason::UnknownMarket, 16, "unknown_market"),
        ];
        for (reason, code, name) in all {
            assert_eq!(reason as u8, code, "{name}");
            assert_eq!(FailureReason::from_u8(code), reason, "{name}");
            assert_eq!(reason.as_str(), name);
            let status = BlockActionStatus {
                core_writer: vec![],
                evm_skipped: vec![],
                native_skipped: vec![false],
                // The message says nothing about the reason: it is not parsed.
                native_failed: vec![failure(0, 0, 1, reason, "x")],
            };
            let back = BlockActionStatus::decode(&status.encode()).unwrap();
            assert_eq!(back.native_failed[0].reason, reason, "{name}");
        }
        assert_eq!(FailureReason::from_u8(200), FailureReason::Other);
    }

    fn cw(
        seq: u64,
        kind: CoreWriterKind,
        outcome: Outcome,
        reason: FailureReason,
        msg: &str,
    ) -> CoreWriterResult {
        let undecodable = kind == CoreWriterKind::Undecodable;
        CoreWriterResult {
            seq,
            kind,
            trader: if undecodable {
                alloy_primitives::Address::ZERO
            } else {
                alloy_primitives::Address::repeat_byte(0x40 + seq as u8)
            },
            queued_block: if undecodable { 0 } else { 6 },
            outcome,
            reason,
            message: msg.to_string(),
            oid: None,
            cloid: None,
        }
    }

    /// s112 (owner option b): a block's CoreWriter drain results, one per
    /// queue row in queue order with mixed outcomes, round-trip in a v4
    /// record next to native failures; a block without native actions or
    /// EVM txs keeps them too (both counts 0).
    #[test]
    fn v4_round_trips_core_writer_results_in_queue_order() {
        use CoreWriterKind as K;
        let ioc = FailureReason::IocCancel.book_reject_message();
        let results = vec![
            cw(
                0,
                K::LockboxDeposit,
                Outcome::Executed,
                FailureReason::Other,
                "",
            ),
            cw(
                1,
                K::Undecodable,
                Outcome::Failed,
                FailureReason::Other,
                "undecodable queued action (block 7, seq 1): x",
            ),
            cw(
                2,
                K::CancelOrder,
                Outcome::Failed,
                FailureReason::Other,
                "action queued in block 7 cannot execute in block 7",
            ),
            cw(
                3,
                K::PlaceOrder,
                Outcome::Failed,
                FailureReason::Other,
                "CoreWriter stop order type 2 is not supported",
            ),
            cw(
                4,
                K::LockboxWithdraw,
                Outcome::Failed,
                FailureReason::Margin,
                "insufficient native balance",
            ),
            cw(
                5,
                K::PlaceOrder,
                Outcome::Rejected,
                FailureReason::IocCancel,
                ioc,
            ),
            cw(9, K::Unknown, Outcome::Executed, FailureReason::Other, ""),
        ];
        for (evm, native, failed) in [
            (vec![], vec![], vec![]),
            (
                vec![true],
                vec![false, false],
                vec![failure(1, 0, 1, FailureReason::Lot, "lot")],
            ),
        ] {
            let status = BlockActionStatus {
                evm_skipped: evm,
                native_skipped: native,
                native_failed: failed,
                core_writer: results.clone(),
            };
            let bytes = status.encode();
            assert_eq!(bytes[0], ACTION_STATUS_V5, "CoreWriter results write v5");
            let back = BlockActionStatus::decode(&bytes).expect("v5 decodes");
            assert_eq!(back, status);
            let seqs: Vec<u64> = back.core_writer.iter().map(|r| r.seq).collect();
            assert_eq!(seqs, [0, 1, 2, 3, 4, 5, 9]);
            assert_eq!(
                back.core_writer[5].message, ioc,
                "a canonical rejection reads back in full"
            );
        }
        // The canonical rejection text is not stored (as in v3).
        let one = |message: &str| {
            BlockActionStatus {
                core_writer: vec![cw(
                    0,
                    K::PlaceOrder,
                    Outcome::Rejected,
                    FailureReason::IocCancel,
                    message,
                )],
                ..Default::default()
            }
            .encode()
            .len()
        };
        assert_eq!(one(ioc), one(""));
        // version 1 + counts 8 + failure_count 4 + cw count 4 + seq 8 + kind 1
        // + trader 20 + queued_block 8 + outcome, reason, msg_len 3 + ext 1.
        assert_eq!(one(""), 1 + 8 + 4 + 4 + 8 + 1 + 20 + 8 + 3 + 1);
    }

    /// v4: no CoreWriter result keeps the v1 / v3 bytes exactly.
    #[test]
    fn no_core_writer_results_keeps_v1_and_v3() {
        let v1 = BlockActionStatus {
            native_skipped: vec![false],
            ..Default::default()
        };
        assert_eq!(v1.encode()[0], ACTION_STATUS_V1);
        let v3 = BlockActionStatus {
            native_skipped: vec![false],
            native_failed: vec![failure(0, 0, 1, FailureReason::Lot, "lot")],
            ..Default::default()
        };
        assert_eq!(v3.encode()[0], ACTION_STATUS_V3);
    }

    /// v4: a truncated or overlong CoreWriter section does not decode; a
    /// kind or outcome code this reader does not know reads as `Unknown` /
    /// `Failed`.
    #[test]
    fn v4_truncated_section_does_not_decode_and_unknown_codes_read() {
        let status = BlockActionStatus {
            core_writer: vec![cw(
                3,
                CoreWriterKind::Delegate,
                Outcome::Failed,
                FailureReason::Other,
                "no stake",
            )],
            ..Default::default()
        };
        let bytes = status.encode();
        assert_eq!(BlockActionStatus::decode(&bytes), Some(status));
        // The section starts after version 1 + counts 8 + failure_count 4.
        for cut in 13..bytes.len() {
            assert_eq!(
                BlockActionStatus::decode(&bytes[..cut]),
                None,
                "cut at {cut}"
            );
        }
        let mut overlong = bytes.clone();
        overlong.push(0);
        assert_eq!(BlockActionStatus::decode(&overlong), None);
        let mut huge = bytes.clone();
        huge[13..17].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(BlockActionStatus::decode(&huge), None);
        // kind at 17 + 8; outcome at 17 + 8 + 1 + 20 + 8.
        let (kind_at, outcome_at) = (25, 54);
        assert_eq!(bytes[kind_at], CoreWriterKind::Delegate as u8);
        assert_eq!(bytes[outcome_at], Outcome::Failed as u8);
        let mut kind = bytes.clone();
        kind[kind_at] = 0x99;
        assert_eq!(
            BlockActionStatus::decode(&kind).unwrap().core_writer[0].kind,
            CoreWriterKind::Unknown
        );
        let mut outcome = bytes;
        outcome[outcome_at] = 0x77;
        assert_eq!(
            BlockActionStatus::decode(&outcome).unwrap().core_writer[0].outcome,
            Outcome::Failed
        );
    }

    /// s114 (v5): an executed CoreWriter `PlaceOrder` row carries the order
    /// id its order got: an ext flags byte after the message (bit 0 = oid)
    /// and the oid, 16 B BE. Every other row carries the flags byte (0)
    /// only. An unknown ext bit (a newer writer) or a cut oid does not
    /// decode.
    #[test]
    fn v5_place_order_rows_carry_their_order_id() {
        use CoreWriterKind as K;
        use FailureReason as R;
        let placed = CoreWriterResult {
            oid: Some(0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10),
            ..cw(0, K::PlaceOrder, Outcome::Executed, R::Other, "")
        };
        let max = CoreWriterResult {
            oid: Some(u128::MAX),
            ..cw(1, K::PlaceOrder, Outcome::Executed, R::Other, "")
        };
        let other = cw(
            2,
            K::CancelOrder,
            Outcome::Failed,
            R::Other,
            "no such order",
        );
        let status = BlockActionStatus {
            core_writer: vec![placed.clone(), max, other],
            ..Default::default()
        };
        let bytes = status.encode();
        assert_eq!(bytes[0], ACTION_STATUS_V5);
        assert_eq!(BlockActionStatus::decode(&bytes), Some(status));

        let one = |r: CoreWriterResult| {
            BlockActionStatus {
                core_writer: vec![r],
                ..Default::default()
            }
            .encode()
        };
        let plain = one(cw(0, K::PlaceOrder, Outcome::Rejected, R::IocCancel, ""));
        assert_eq!(plain.len(), 1 + 8 + 4 + 4 + 8 + 1 + 20 + 8 + 3 + 1);
        assert_eq!(*plain.last().unwrap(), 0x00, "no oid: ext 0");
        let with = one(placed.clone());
        assert_eq!(with.len(), plain.len() + 16);
        assert_eq!(with[with.len() - 17], 0x01, "ext bit 0: oid follows");
        assert_eq!(&with[with.len() - 16..], &placed.oid.unwrap().to_be_bytes());

        let mut unknown = plain.clone();
        *unknown.last_mut().unwrap() = 0x04;
        assert_eq!(BlockActionStatus::decode(&unknown), None, "unknown ext bit");
        assert_eq!(
            BlockActionStatus::decode(&with[..with.len() - 1]),
            None,
            "cut oid"
        );
        assert_eq!(
            BlockActionStatus::decode(&plain[..plain.len() - 1]),
            None,
            "no ext"
        );
    }

    /// s114: v4 records (s112, no ext byte per row) still read, every oid
    /// `None`.
    #[test]
    fn v4_records_written_before_v5_still_read() {
        use CoreWriterKind as K;
        let rows = vec![
            cw(
                0,
                K::LockboxDeposit,
                Outcome::Executed,
                FailureReason::Other,
                "",
            ),
            cw(
                3,
                K::Delegate,
                Outcome::Failed,
                FailureReason::Other,
                "no stake",
            ),
        ];
        // A v4 record by hand: version, counts 0/0, failure_count 0, then
        // each row's v5 bytes without its ext byte.
        let mut v4 = vec![0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];
        for r in &rows {
            let one = BlockActionStatus {
                core_writer: vec![r.clone()],
                ..Default::default()
            }
            .encode();
            v4.extend_from_slice(&one[17..one.len() - 1]);
        }
        let back = BlockActionStatus::decode(&v4).expect("v4 decodes");
        assert_eq!(back.core_writer, rows);
        assert!(back.core_writer.iter().all(|r| r.oid.is_none()));
    }

    /// The stored CoreWriter kind codes and names are a format.
    #[test]
    fn core_writer_kind_codes_and_names_are_stable() {
        use CoreWriterKind as K;
        let all = [
            (K::PlaceOrder, 0, "place_order"),
            (K::CancelOrder, 1, "cancel_order"),
            (K::CancelAll, 2, "cancel_all"),
            (K::Delegate, 3, "delegate"),
            (K::Undelegate, 4, "undelegate"),
            (K::ClaimRewards, 5, "claim_rewards"),
            (K::LockPermanent, 6, "lock_permanent"),
            (K::ClaimUnbonded, 7, "claim_unbonded"),
            (K::CancelByCloid, 8, "cancel_by_cloid"),
            (K::LockboxDeposit, 0x20, "lockbox_deposit"),
            (K::LockboxWithdraw, 0x21, "lockbox_withdraw"),
            (K::Unknown, 0xFE, "unknown"),
            (K::Undecodable, 0xFF, "undecodable"),
        ];
        for (kind, code, name) in all {
            assert_eq!(kind as u8, code, "{name}");
            assert_eq!(K::from_u8(code), kind, "{name}");
            assert_eq!(kind.as_str(), name);
        }
        assert_eq!(K::from_u8(9), K::Unknown);
    }

    /// s114: a row that carried a cloid (a placement, a cancel by cloid)
    /// records it: ext bit 1, 16 BE bytes after the oid (if any).
    #[test]
    fn v5_rows_carry_their_cloid() {
        use CoreWriterKind as K;
        use FailureReason as R;
        let cloid = 0xc10d_0000_0000_0000_0000_0000_0000_0001u128;
        let both = CoreWriterResult {
            oid: Some(7),
            cloid: Some(cloid),
            ..cw(0, K::PlaceOrder, Outcome::Executed, R::Other, "")
        };
        let cancel = CoreWriterResult {
            cloid: Some(u128::MAX),
            ..cw(
                1,
                K::CancelByCloid,
                Outcome::Failed,
                R::Other,
                "no such order",
            )
        };
        let dup = CoreWriterResult {
            cloid: Some(cloid),
            ..cw(2, K::PlaceOrder, Outcome::Failed, R::DuplicateCloid, "dup")
        };
        let status = BlockActionStatus {
            core_writer: vec![both.clone(), cancel, dup],
            ..Default::default()
        };
        let bytes = status.encode();
        assert_eq!(bytes[0], ACTION_STATUS_V5);
        assert_eq!(BlockActionStatus::decode(&bytes), Some(status));

        let one = BlockActionStatus {
            core_writer: vec![both],
            ..Default::default()
        }
        .encode();
        assert_eq!(one[one.len() - 33], 0x03, "ext: oid and cloid");
        assert_eq!(&one[one.len() - 32..one.len() - 16], &7u128.to_be_bytes());
        assert_eq!(&one[one.len() - 16..], &cloid.to_be_bytes());
        assert_eq!(
            BlockActionStatus::decode(&one[..one.len() - 1]),
            None,
            "cut cloid"
        );
    }

    #[test]
    fn empty_record_round_trips() {
        let status = BlockActionStatus::default();
        assert_eq!(BlockActionStatus::decode(&status.encode()), Some(status));
    }

    #[test]
    fn rejects_unknown_version_and_bad_lengths() {
        let bytes = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![true],
            native_skipped: vec![true; 9],
            native_failed: vec![],
        }
        .encode();
        let mut wrong_version = bytes.clone();
        wrong_version[0] = 0x06;
        assert_eq!(BlockActionStatus::decode(&wrong_version), None);
        assert_eq!(BlockActionStatus::decode(&bytes[..bytes.len() - 1]), None);
        let mut overlong = bytes.clone();
        overlong.push(0);
        assert_eq!(BlockActionStatus::decode(&overlong), None);
        assert_eq!(BlockActionStatus::decode(&[]), None);

        let v2 = BlockActionStatus {
            core_writer: vec![],
            evm_skipped: vec![],
            native_skipped: vec![false; 2],
            native_failed: vec![failure(1, 0, 1, FailureReason::Fill, "boom")],
        }
        .encode();
        assert_eq!(BlockActionStatus::decode(&v2[..v2.len() - 1]), None);
        let mut v2_overlong = v2.clone();
        v2_overlong.push(0);
        assert_eq!(BlockActionStatus::decode(&v2_overlong), None);
        let mut out_of_range = v2.clone();
        // failure index after version 1 + counts 8 + bitmaps 0+1 + count 4.
        out_of_range[14..18].copy_from_slice(&5u32.to_be_bytes());
        assert_eq!(BlockActionStatus::decode(&out_of_range), None);
    }
}
