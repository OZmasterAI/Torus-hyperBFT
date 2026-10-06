//! Per-block action status record, `CF_BLOCK_ACTION_STATUS` (s84 decision 1,
//! v2 per-action execution failures).
//!
//! A certified block is never rejected for something inside it: execution
//! runs every action and SKIPS (no state change) any that fails its validity
//! check (unresolvable signature or session, replayed or duplicate nonce,
//! undecodable EVM tx, a tx revm refuses). A native action that passes those
//! checks executes, and the executor may still refuse it (margin, open-order
//! limit, off-tick price, ...): it then FAILED. This record says which, so the
//! RPC and the explorer show every action as executed, skipped or failed.
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
//! Encoding v2 (at least one failure): `0x02 ‖ <v1 body after the version
//! byte> ‖ failure_count u32 BE ‖ failures`, each failure `index u32 BE ‖
//! order u32 BE ‖ failed_orders u32 BE ‖ reason u8 ‖ msg_len u8 ‖ msg`, in
//! ascending `index` order, `msg` UTF-8 cut to at most [`MAX_MESSAGE_BYTES`].

/// First byte of a v1 record (executed/skipped only).
const ACTION_STATUS_V1: u8 = 0x01;
/// First byte of a v2 record (v1 + native execution failures).
const ACTION_STATUS_V2: u8 = 0x02;
/// Longest stored failure message, in bytes (cut at a char boundary).
pub const MAX_MESSAGE_BYTES: usize = 96;

/// Why a native action failed at execution: the executor's typed reason
/// (`NativeActionResult::reason`, set where the check failed; the message
/// is stored too). The `u8` codes are stored: never renumber, only add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FailureReason {
    /// Anything without its own code (unknown order, ownership, reduce-only,
    /// staking / governance / oracle / session errors, state read errors).
    Other = 0,
    /// An account margin check: placement, modify, withdrawal.
    Margin = 1,
    /// Open-order limit reached (plain, or the reduce-only / stop rule).
    OpenLimit = 2,
    /// A limit (or stop-limit limit, or modify) price off the market tick.
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
        }
    }
}

/// One native action the executor refused, by body position.
///
/// A PlaceOrderBatch is one action of many orders: it fails if ANY of its
/// orders failed; `order` is the position (inside the batch) of the first
/// failing order, whose reason and message are the ones recorded, and
/// `failed_orders` counts its failed orders (the others executed). A batch
/// skipped wholesale (empty / over the cap) has `failed_orders = 0`. For any
/// other action `order = 0`, `failed_orders = 1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeActionFailure {
    pub index: u32,
    pub order: u32,
    pub failed_orders: u32,
    pub reason: FailureReason,
    pub message: String,
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
        }
    }
}

/// Which actions of one block execution skipped or failed, by position in the
/// block body (`evm_transactions`, `native_actions`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockActionStatus {
    pub evm_skipped: Vec<bool>,
    pub native_skipped: Vec<bool>,
    /// Native actions that executed and failed, ascending `index`; never a
    /// skipped one.
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

impl BlockActionStatus {
    /// v1 when nothing failed (the s84 bytes exactly), else v2.
    pub fn encode(&self) -> Vec<u8> {
        let failures = !self.native_failed.is_empty();
        let mut out = Vec::with_capacity(
            9 + self.evm_skipped.len().div_ceil(8) + self.native_skipped.len().div_ceil(8),
        );
        out.push(if failures {
            ACTION_STATUS_V2
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
                out.push(f.reason as u8);
                let msg = truncate_utf8(&f.message, MAX_MESSAGE_BYTES);
                out.push(msg.len() as u8);
                out.extend_from_slice(msg.as_bytes());
            }
        }
        out
    }

    /// `None` for an unknown version or a truncated / overlong record.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (&version, mut rest) = bytes.split_first()?;
        if version != ACTION_STATUS_V1 && version != ACTION_STATUS_V2 {
            return None;
        }
        let evm_count = take_u32(&mut rest)? as usize;
        let native_count = take_u32(&mut rest)? as usize;
        let evm_bits = take(&mut rest, evm_count.div_ceil(8))?;
        let native_bits = take(&mut rest, native_count.div_ceil(8))?;
        let mut native_failed = Vec::new();
        if version == ACTION_STATUS_V2 {
            let count = take_u32(&mut rest)?;
            for _ in 0..count {
                let index = take_u32(&mut rest)?;
                let order = take_u32(&mut rest)?;
                let failed_orders = take_u32(&mut rest)?;
                let reason = FailureReason::from_u8(take_u8(&mut rest)?);
                let len = take_u8(&mut rest)? as usize;
                let message = std::str::from_utf8(take(&mut rest, len)?).ok()?.to_string();
                if index as usize >= native_count {
                    return None;
                }
                native_failed.push(NativeActionFailure {
                    index,
                    order,
                    failed_orders,
                    reason,
                    message,
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

    /// Native action `i`'s label: `"skipped"`, `"failed"` or `"executed"`.
    pub fn native_label(&self, i: usize) -> &'static str {
        if self.native_skipped.get(i).copied().unwrap_or(false) {
            "skipped"
        } else if self.failure(i).is_some() {
            "failed"
        } else {
            "executed"
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

    #[test]
    fn v2_round_trips_failures() {
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
        assert_eq!(bytes[0], ACTION_STATUS_V2);
        let back = BlockActionStatus::decode(&bytes).expect("v2 decodes");
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
        wrong_version[0] = 0x03;
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
