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
//! tx, keyed by 8-byte BE height, written in the block's flush batch (same
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

/// First byte of a v1 record (executed/skipped only).
const ACTION_STATUS_V1: u8 = 0x01;
/// First byte of a v2 record (v1 + native execution failures; read only).
const ACTION_STATUS_V2: u8 = 0x02;
/// First byte of a v3 record (v2 + outcome per entry + per-order statuses).
const ACTION_STATUS_V3: u8 = 0x03;
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
    /// reference price (placement, modify; HL `oracleRejected`).
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

/// `msg_len u8 ‖ msg`, cut to [`MAX_MESSAGE_BYTES`].
fn push_message(out: &mut Vec<u8>, message: &str) {
    let msg = truncate_utf8(message, MAX_MESSAGE_BYTES);
    out.push(msg.len() as u8);
    out.extend_from_slice(msg.as_bytes());
}

fn take_message(bytes: &mut &[u8]) -> Option<String> {
    let len = take_u8(bytes)? as usize;
    Some(std::str::from_utf8(take(bytes, len)?).ok()?.to_string())
}

impl BlockActionStatus {
    /// v1 when nothing failed or was rejected (the s84 bytes exactly), else
    /// v3.
    pub fn encode(&self) -> Vec<u8> {
        let failures = !self.native_failed.is_empty();
        let mut out = Vec::with_capacity(
            9 + self.evm_skipped.len().div_ceil(8) + self.native_skipped.len().div_ceil(8),
        );
        out.push(if failures {
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
                push_message(&mut out, &f.message);
                out.extend_from_slice(&(f.orders.len() as u32).to_be_bytes());
                for o in &f.orders {
                    out.push(o.outcome as u8);
                    out.push(o.reason as u8);
                    push_message(&mut out, &o.message);
                }
            }
        }
        out
    }

    /// `None` for an unknown version or a truncated / overlong record.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&version, mut rest) = bytes.split_first()?;
        if !matches!(version, ACTION_STATUS_V1 | ACTION_STATUS_V2 | ACTION_STATUS_V3) {
            return None;
        }
        let evm_count = take_u32(&mut rest)? as usize;
        let native_count = take_u32(&mut rest)? as usize;
        let evm_bits = take(&mut rest, evm_count.div_ceil(8))?;
        let native_bits = take(&mut rest, native_count.div_ceil(8))?;
        let mut native_failed = Vec::new();
        if version != ACTION_STATUS_V1 {
            let v3 = version == ACTION_STATUS_V3;
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
                let message = take_message(&mut rest)?;
                let mut orders = Vec::new();
                if v3 {
                    for _ in 0..take_u32(&mut rest)? {
                        orders.push(OrderOutcome {
                            outcome: Outcome::from_u8(take_u8(&mut rest)?),
                            reason: FailureReason::from_u8(take_u8(&mut rest)?),
                            message: take_message(&mut rest)?,
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
        if !rest.is_empty() {
            return None;
        }
        Some(Self {
            evm_skipped: read_bitmap(evm_bits, evm_count),
            native_skipped: read_bitmap(native_bits, native_count),
            native_failed,
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

    #[test]
    fn v3_round_trips_failures() {
        let status = BlockActionStatus {
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
        ];
        for (reason, code, name) in all {
            assert_eq!(reason as u8, code, "{name}");
            assert_eq!(FailureReason::from_u8(code), reason, "{name}");
            assert_eq!(reason.as_str(), name);
            let status = BlockActionStatus {
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

    #[test]
    fn empty_record_round_trips() {
        let status = BlockActionStatus::default();
        assert_eq!(BlockActionStatus::decode(&status.encode()), Some(status));
    }

    #[test]
    fn rejects_unknown_version_and_bad_lengths() {
        let bytes = BlockActionStatus {
            evm_skipped: vec![true],
            native_skipped: vec![true; 9],
            native_failed: vec![],
        }
        .encode();
        let mut wrong_version = bytes.clone();
        wrong_version[0] = 0x04;
        assert_eq!(BlockActionStatus::decode(&wrong_version), None);
        assert_eq!(BlockActionStatus::decode(&bytes[..bytes.len() - 1]), None);
        let mut overlong = bytes.clone();
        overlong.push(0);
        assert_eq!(BlockActionStatus::decode(&overlong), None);
        assert_eq!(BlockActionStatus::decode(&[]), None);

        let v2 = BlockActionStatus {
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
