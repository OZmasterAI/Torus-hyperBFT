//! v2 action status: the executor's per-entry results mapped back to the
//! native actions' positions in the block body
//! (`torus_state::action_status`).
//!
//! The exec thread runs `sort_native_actions_indexed` (pre-EVM / post-EVM
//! lists, each sorted by (category, sender, content hash), with each entry's
//! input position) and one `execute_batch` per list, which flattens every
//! PlaceOrderBatch into one result per order. This module undoes both steps
//! for the failing entries only: flat result -> action of the list (the
//! executor's flatten, replayed) -> body position (the entry's input
//! position, then the replay guard's body index; item 6 cut 1 replaced a
//! content match). Only failures carry data out of here; the encoding
//! happens where the record is written (the flush worker on the pipelined
//! path).
//!
//! Deterministic: a pure function of the block, the replay-guard decisions and
//! the executor's results, which are themselves identical in every exec mode.

#[cfg(test)]
use std::collections::HashMap;

use alloy_primitives::Address;
#[cfg(test)]
use torus_bridge::native_executor::classify_action;
use torus_bridge::NativeBatchResult;
use torus_state::action_status::{BlockActionStatus, FailureReason, NativeActionFailure, Outcome};
use torus_types::NativeAction;

/// Result entries `action` contributes to `execute_batch`: one per order of a
/// PlaceOrderBatch within the batch cap, none for a batch outside it (skipped
/// wholesale by the executor), one for anything else.
fn flat_len(action: &NativeAction) -> usize {
    match action {
        NativeAction::PlaceOrderBatch(orders)
            if torus_types::batch_len_within_cap(orders.len()) =>
        {
            orders.len()
        }
        NativeAction::PlaceOrderBatch(_) => 0,
        _ => 1,
    }
}

/// A failing action of one list: (list position, order, failed orders,
/// the first failing entry's outcome, reason and message).
type ListFailure = (usize, u32, u32, Outcome, FailureReason, String);

/// Row 50: an order placement refused for a reason with an HL `*Rejected`
/// status is `Rejected` (the book's refusals and zero-fill cancels, and the
/// placement margin / reduce-only / tick / price-band checks); anything else
/// that did not execute `Failed`.
fn outcome(action: &NativeAction, reason: FailureReason) -> Outcome {
    let placement = matches!(action, NativeAction::PlaceOrder(_) | NativeAction::PlaceOrderBatch(_));
    if placement && reason.hl_rejected_name().is_some() {
        Outcome::Rejected
    } else {
        Outcome::Failed
    }
}

/// Failures of one `execute_batch` over `list`, by list position. `None` when
/// the result count does not match the flatten (never expected; the caller
/// then records no failure for the list rather than a wrong one).
fn list_failures(
    list: &[(Address, NativeAction)],
    result: &mut NativeBatchResult,
) -> Option<Vec<ListFailure>> {
    let total: usize = list.iter().map(|(_, a)| flat_len(a)).sum();
    if result.results.len() != total {
        return None;
    }
    let mut out = Vec::new();
    let mut flat = 0;
    for (pos, (_, action)) in list.iter().enumerate() {
        let len = flat_len(action);
        if let NativeAction::PlaceOrderBatch(orders) = action {
            if len == 0 {
                out.push((
                    pos,
                    0,
                    0,
                    Outcome::Failed,
                    FailureReason::BatchCap,
                    format!(
                        "PlaceOrderBatch skipped: {} orders, cap {}",
                        orders.len(),
                        torus_types::NATIVE_ORDERS_PER_BATCH_CAP
                    ),
                ));
                continue;
            }
        }
        let entries = &mut result.results[flat..flat + len];
        flat += len;
        let mut failed = entries.iter().enumerate().filter(|(_, r)| !r.success);
        if let Some((first, _)) = failed.next() {
            let failed_orders = 1 + failed.count() as u32;
            // The executor's typed reason; the message is only stored.
            let reason = entries[first].reason;
            let message = entries[first]
                .error
                .take()
                .unwrap_or_else(|| "failed".to_string());
            out.push((pos, first as u32, failed_orders, outcome(action, reason), reason, message));
        }
    }
    Some(out)
}

/// The block's native execution failures and order rejections, ascending
/// body index.
///
/// `body_index[k]` = body position of the k-th action handed to
/// `sort_native_actions_indexed` (body order, the replay guard's skips
/// removed); `batches` = each sorted list, the input position of each of its
/// entries (the sort's index list) and its `execute_batch` result. A failure
/// maps to its body position by index (item 6 cut 1; the pre-cut content
/// match is kept as the test oracle [`native_failures_by_content`]). The
/// failing entries' messages are moved out of the results; the caller drops
/// the rest (item 6 cut 5: off the execution thread).
#[allow(clippy::type_complexity)]
pub fn native_failures(
    body_index: &[u32],
    batches: [(&[(Address, NativeAction)], &[u32], &mut NativeBatchResult); 2],
) -> Vec<NativeActionFailure> {
    let mut out = Vec::new();
    for (list, executed_index, result) in batches {
        let Some(failures) = list_failures(list, result) else {
            tracing::error!(
                actions = list.len(),
                "native batch result count does not match its actions — no failures recorded"
            );
            continue;
        };
        for (pos, order, failed_orders, outcome, reason, message) in failures {
            // Both lookups always hit (the sort returns one input position
            // per entry); a miss records nothing, like the content match did.
            let Some(&index) = executed_index
                .get(pos)
                .and_then(|&k| body_index.get(k as usize))
            else {
                continue;
            };
            out.push(NativeActionFailure {
                outcome,
                ..NativeActionFailure::new(index, order, failed_orders, reason, message)
            });
        }
    }
    out.sort_by_key(|f| f.index);
    out
}

/// The pre-cut mapping (C, `9195c32`): each failing list entry found again
/// in `executed` by sender, category and canonical bytes. Test oracle for
/// [`native_failures`].
#[cfg(test)]
pub(crate) fn native_failures_by_content(
    executed: &[(Address, NativeAction)],
    body_index: &[u32],
    batches: [(&[(Address, NativeAction)], NativeBatchResult); 2],
) -> Vec<NativeActionFailure> {
    let mut out = Vec::new();
    // Built on the first failure only: executed positions by sender.
    let mut by_sender: Option<HashMap<Address, Vec<usize>>> = None;
    for (list, mut result) in batches {
        let Some(failures) = list_failures(list, &mut result) else {
            continue;
        };
        for (pos, order, failed_orders, outcome, reason, message) in failures {
            let by_sender = by_sender.get_or_insert_with(|| {
                let mut m: HashMap<Address, Vec<usize>> = HashMap::new();
                for (k, (sender, _)) in executed.iter().enumerate() {
                    m.entry(*sender).or_default().push(k);
                }
                m
            });
            if let Some(k) = body_position(executed, by_sender, list, pos) {
                out.push(NativeActionFailure {
                    outcome,
                    ..NativeActionFailure::new(body_index[k], order, failed_orders, reason, message)
                });
            }
        }
    }
    out.sort_by_key(|f| f.index);
    out
}

/// Position in `executed` of `list[pos]`. The sort orders by (category,
/// sender, content hash) and is stable, so `list[pos]` is the r-th executed
/// action with its sender and content, r = its rank among identical entries
/// of `list`. Content is compared (canonical bytes) only when the sender has
/// more than one action of that category.
#[cfg(test)]
fn body_position(
    executed: &[(Address, NativeAction)],
    by_sender: &HashMap<Address, Vec<usize>>,
    list: &[(Address, NativeAction)],
    pos: usize,
) -> Option<usize> {
    let (sender, action) = &list[pos];
    let category = classify_action(action);
    let same: Vec<usize> = by_sender
        .get(sender)?
        .iter()
        .copied()
        .filter(|&k| classify_action(&executed[k].1) == category)
        .collect();
    if let [only] = same[..] {
        return Some(only);
    }
    let bytes = action.canonical_bytes();
    let rank = list[..pos]
        .iter()
        .filter(|(s, a)| {
            s == sender && classify_action(a) == category && a.canonical_bytes() == bytes
        })
        .count();
    same.into_iter()
        .filter(|&k| executed[k].1.canonical_bytes() == bytes)
        .nth(rank)
}

/// `status` encoded for `CF_BLOCK_ACTION_STATUS`, counted in the metrics.
pub fn encode_status(
    status: &BlockActionStatus,
    metrics: Option<&torus_telemetry::Metrics>,
) -> Vec<u8> {
    let bytes = status.encode();
    if let Some(m) = metrics {
        let rejected = status
            .native_failed
            .iter()
            .filter(|f| f.outcome == Outcome::Rejected)
            .count();
        m.exec_action_failures
            .inc_by((status.native_failed.len() - rejected) as u64);
        m.exec_action_rejections.inc_by(rejected as u64);
        m.exec_action_status_bytes.inc_by(bytes.len() as u64);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_bridge::NativeActionResult;
    use torus_types::{FixedPoint, OrderType, PlaceOrderParams, TimeInForce};

    fn order(price: i128) -> PlaceOrderParams {
        PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(price),
            quantity: FixedPoint::ONE,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        }
    }

    fn ok() -> NativeActionResult {
        NativeActionResult {
            action_type: "x",
            success: true,
            error: None,
            gas_used: 0,
            reason: FailureReason::Other,
        }
    }

    fn err(reason: FailureReason, msg: &str) -> NativeActionResult {
        NativeActionResult {
            action_type: "x",
            success: false,
            error: Some(msg.to_string()),
            gas_used: 0,
            reason,
        }
    }

    fn batch(results: Vec<NativeActionResult>) -> NativeBatchResult {
        NativeBatchResult {
            results,
            total_gas: 0,
        }
    }

    /// A PlaceOrderBatch with bad orders maps to its own body position and to
    /// its first failing order; an oversize batch fails wholesale; identical
    /// actions of one sender map by rank; positions follow `body_index`.
    #[test]
    fn maps_flattened_results_back_to_body_positions() {
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let same = NativeAction::PlaceOrder(order(5));
        let executed = vec![
            (a, same.clone()), // body 0
            (
                b,
                NativeAction::PlaceOrderBatch(vec![order(1), order(2), order(3)]),
            ), // body 2
            (a, same.clone()), // body 3
            (b, NativeAction::PlaceOrderBatch(vec![])), // body 4
        ];
        let body_index = [0, 2, 3, 4];
        // A sorted list in a different order than the body.
        let list = vec![
            executed[3].clone(),
            executed[1].clone(),
            executed[0].clone(),
            executed[2].clone(),
        ];
        // Flat: [] (empty batch) + 3 orders + 1 + 1. The reasons are the
        // results' own, whatever the message says (no text parsing).
        let result = batch(vec![
            ok(),
            err(
                FailureReason::Tick,
                "order rejected: price 2 is not a multiple of the tick 1",
            ),
            err(
                FailureReason::Margin,
                "insufficient margin: need 1, have 0 (account)",
            ),
            ok(),
            err(FailureReason::OpenLimit, "some text"),
        ]);
        let failures = native_failures(
            &body_index,
            [
                (&list, &[3, 1, 0, 2], &mut result.clone()),
                (&[], &[], &mut batch(vec![])),
            ],
        );
        assert_eq!(
            failures,
            native_failures_by_content(
                &executed,
                &body_index,
                [(&list, result), (&[], batch(vec![]))],
            )
        );
        let got: Vec<_> = failures
            .iter()
            .map(|f| (f.index, f.order, f.failed_orders, f.reason))
            .collect();
        assert_eq!(
            got,
            vec![
                (2, 1, 2, FailureReason::Tick),
                (3, 0, 1, FailureReason::OpenLimit),
                (4, 0, 0, FailureReason::BatchCap),
            ]
        );
    }

    /// Row 50: an order placement refused for an HL rejection reason is
    /// recorded `Rejected`; any other failure (and a modify refused for
    /// margin or tick) stays `Failed`. A batch records its first order that
    /// did not execute, with that order's outcome, and counts every such
    /// order. Row 50 review (S3): off-tick and price-band placements are
    /// rejected too (`tickRejected`, `oracleRejected`).
    #[test]
    fn order_rejections_are_recorded_rejected() {
        use torus_state::action_status::Outcome;
        let a = Address::repeat_byte(1);
        let list = vec![
            (a, NativeAction::PlaceOrder(order(1))),
            (a, NativeAction::PlaceOrderBatch(vec![order(1), order(2), order(3), order(4)])),
            (a, NativeAction::ModifyOrder { order_id: 1, new_price: None, new_qty: None }),
            (a, NativeAction::PlaceOrder(order(2))),
            (a, NativeAction::PlaceOrderBatch(vec![order(1), order(2)])),
            (a, NativeAction::PlaceOrder(order(3))),
            (a, NativeAction::PlaceOrderBatch(vec![order(1), order(2)])),
            (a, NativeAction::ModifyOrder { order_id: 2, new_price: None, new_qty: None }),
        ];
        let result = batch(vec![
            err(FailureReason::IocCancel, "ioc"),
            ok(),
            err(FailureReason::BadAloPx, "alo"),
            err(FailureReason::Tick, "tick"),
            ok(),
            err(FailureReason::Margin, "modify margin"),
            err(FailureReason::Margin, "insufficient margin"),
            err(FailureReason::Lot, "lot"),
            err(FailureReason::FokCancel, "fok"),
            err(FailureReason::Tick, "tick"),
            ok(),
            err(FailureReason::PriceBand, "band"),
            err(FailureReason::Tick, "modify tick"),
        ]);
        let failures =
            native_failures(&[0, 1, 2, 3, 4, 5, 6, 7], [(&list, &[0, 1, 2, 3, 4, 5, 6, 7], &mut result.clone()), (&[], &[], &mut batch(vec![]))]);
        let got: Vec<_> = failures
            .iter()
            .map(|f| (f.index, f.order, f.failed_orders, f.outcome, f.reason, f.message.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 0, 1, Outcome::Rejected, FailureReason::IocCancel, "ioc"),
                (1, 1, 2, Outcome::Rejected, FailureReason::BadAloPx, "alo"),
                (2, 0, 1, Outcome::Failed, FailureReason::Margin, "modify margin"),
                (3, 0, 1, Outcome::Rejected, FailureReason::Margin, "insufficient margin"),
                (4, 0, 2, Outcome::Failed, FailureReason::Lot, "lot"),
                (5, 0, 1, Outcome::Rejected, FailureReason::Tick, "tick"),
                (6, 1, 1, Outcome::Rejected, FailureReason::PriceBand, "band"),
                (7, 0, 1, Outcome::Failed, FailureReason::Tick, "modify tick"),
            ]
        );
        assert!(failures.iter().all(|f| f.orders.is_empty()), "per-order statuses not written yet");
        assert_eq!(
            failures,
            native_failures_by_content(&list, &[0, 1, 2, 3, 4, 5, 6, 7], [(&list, result), (&[], batch(vec![]))])
        );
    }

    /// Row 50 review (S1): `torus_exec_action_failures` counts only failed
    /// entries and `torus_exec_action_rejections` only rejected ones; the
    /// bytes counter counts the record.
    #[test]
    fn encode_status_counts_failures_and_rejections_apart() {
        let m = torus_telemetry::Metrics::new();
        let rejected = |index| NativeActionFailure {
            outcome: Outcome::Rejected,
            ..NativeActionFailure::new(index, 0, 1, FailureReason::IocCancel, "ioc".into())
        };
        let status = |native_failed| BlockActionStatus {
            evm_skipped: vec![],
            native_skipped: vec![false; 4],
            native_failed,
        };
        let failed = NativeActionFailure::new(0, 0, 1, FailureReason::OpenLimit, "limit".into());
        let counts = |m: &torus_telemetry::Metrics| (m.exec_action_failures.get(), m.exec_action_rejections.get());

        let one_failed = encode_status(&status(vec![failed.clone()]), Some(&m));
        assert_eq!(counts(&m), (1, 0), "a failed action bumps failures only");
        let one_rejected = encode_status(&status(vec![rejected(1)]), Some(&m));
        assert_eq!(counts(&m), (1, 1), "a rejected action bumps rejections only");
        let mixed = encode_status(&status(vec![failed, rejected(2), rejected(3)]), Some(&m));
        assert_eq!(counts(&m), (2, 3));
        assert_eq!(
            m.exec_action_status_bytes.get(),
            (one_failed.len() + one_rejected.len() + mixed.len()) as u64
        );
        let text = m.encode();
        assert!(text.contains("torus_exec_action_failures_total 2\n"), "{text}");
        assert!(text.contains("torus_exec_action_rejections_total 3\n"), "{text}");
    }

    #[test]
    fn mismatched_result_count_records_nothing() {
        let a = Address::repeat_byte(1);
        let executed = vec![(a, NativeAction::PlaceOrder(order(1)))];
        let failures = native_failures(
            &[0],
            [
                (
                    &executed,
                    &[0],
                    &mut batch(vec![
                        err(FailureReason::Other, "x"),
                        err(FailureReason::Other, "y"),
                    ]),
                ),
                (&[], &[], &mut batch(vec![])),
            ],
        );
        assert!(failures.is_empty());
    }

    /// Item 6 cut 1: on random blocks (repeated identical actions of one
    /// sender, every category, PlaceOrderBatch with bad orders, empty and
    /// over-cap batches, replay-guard skips as gaps in the body index, random
    /// successes / failures and reasons), the index mapping stores exactly
    /// the record the pre-cut content match stored, byte for byte.
    #[test]
    fn index_mapping_matches_the_content_match_on_random_blocks() {
        use torus_bridge::native_executor::sort_native_actions_indexed;
        use torus_types::NATIVE_ORDERS_PER_BATCH_CAP;

        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        const REASONS: [FailureReason; 7] = [
            FailureReason::Other,
            FailureReason::Margin,
            FailureReason::OpenLimit,
            FailureReason::Tick,
            FailureReason::Lot,
            FailureReason::Price,
            FailureReason::Fill,
        ];
        let mut failures_seen = 0usize;
        for round in 0..400 {
            let body_len = next(48) as usize;
            let mut executed = Vec::new();
            let mut body_index = Vec::new();
            let mut native_skipped = Vec::new();
            for i in 0..body_len {
                // Replay-guard skips leave gaps in the body index.
                if next(6) == 0 {
                    native_skipped.push(true);
                    continue;
                }
                native_skipped.push(false);
                let sender = Address::repeat_byte(1 + next(3) as u8);
                let o = |p: u64, tif| PlaceOrderParams {
                    time_in_force: tif,
                    ..order(1 + p as i128)
                };
                let action = match next(8) {
                    0 => NativeAction::CancelOrder {
                        order_id: next(2) as u128,
                    },
                    1 => NativeAction::PlaceOrder(o(next(2), TimeInForce::IOC)),
                    2 | 3 => NativeAction::PlaceOrder(o(next(2), TimeInForce::GTC)),
                    4 => NativeAction::PlaceOrderBatch(
                        (0..next(4)).map(|k| o(k % 2, TimeInForce::GTC)).collect(),
                    ),
                    5 if next(10) == 0 => NativeAction::PlaceOrderBatch(vec![
                        order(1);
                        NATIVE_ORDERS_PER_BATCH_CAP
                            + 1
                    ]),
                    5 => NativeAction::TransferToPerp {
                        amount: alloy_primitives::U256::from(next(2)),
                    },
                    6 => NativeAction::ClaimRewards,
                    _ => NativeAction::CancelAllOrders {
                        market_id: Some(1 + next(2)),
                    },
                };
                executed.push((sender, action));
                body_index.push(i as u32);
            }
            let ((pre, pre_idx), (post, post_idx)) = sort_native_actions_indexed(executed.clone());
            // One random result per flat entry, built twice (new and oracle).
            let mut draw = |list: &[(Address, NativeAction)]| {
                let n: usize = list.iter().map(|(_, a)| flat_len(a)).sum();
                (0..n)
                    .map(|_| match next(3) {
                        0 => None,
                        _ => Some((REASONS[next(7) as usize], format!("e{}", next(5)))),
                    })
                    .collect::<Vec<_>>()
            };
            let to_batch = |spec: &[Option<(FailureReason, String)>]| {
                batch(
                    spec.iter()
                        .map(|e| match e {
                            None => ok(),
                            Some((r, m)) => err(*r, m),
                        })
                        .collect(),
                )
            };
            let (pre_spec, post_spec) = (draw(&pre), draw(&post));
            let new = native_failures(
                &body_index,
                [
                    (&pre, &pre_idx, &mut to_batch(&pre_spec)),
                    (&post, &post_idx, &mut to_batch(&post_spec)),
                ],
            );
            let old = native_failures_by_content(
                &executed,
                &body_index,
                [(&pre, to_batch(&pre_spec)), (&post, to_batch(&post_spec))],
            );
            failures_seen += new.len();
            let record = |native_failed| {
                BlockActionStatus {
                    evm_skipped: vec![false; round % 3],
                    native_skipped: native_skipped.clone(),
                    native_failed,
                }
                .encode()
            };
            assert_eq!(record(new), record(old), "round {round}");
        }
        assert!(failures_seen > 1000, "the rounds exercise failures");
    }

    /// Item 6 cut 1 µbench (exec-thread work outside every engine timer):
    /// the sort plus the failure mapping, pre-cut (`sort_native_actions`
    /// clone + content match) vs cut 1 (sort by value + index lookup), on a
    /// node-shaped block (section 17: ~400 actions, ~152 failures per native
    /// block): `UB_SENDERS` senders x `UB_PER_SENDER` PlaceOrderBatch of
    /// `UB_ORDERS` orders, `UB_FAILING` of them with a failing order.
    ///
    ///   cargo test -p torus-consensus --release --lib ubench_failure_mapping -- --ignored --nocapture
    #[test]
    #[ignore = "µbench — run with --release --ignored --nocapture"]
    fn ubench_failure_mapping() {
        use torus_bridge::native_executor::{sort_native_actions, sort_native_actions_indexed};
        let env = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let (senders, per_sender, orders, failing, iters) = (
            env("UB_SENDERS", 100),
            env("UB_PER_SENDER", 4),
            env("UB_ORDERS", 400),
            env("UB_FAILING", 152),
            env("UB_ITERS", 30),
        );
        let mut executed = Vec::new();
        for k in 0..per_sender {
            for s in 0..senders {
                let sender = Address::repeat_byte(1 + (s % 250) as u8);
                let batch: Vec<PlaceOrderParams> = (0..orders)
                    .map(|o| PlaceOrderParams {
                        market_id: 1 + (o % 10) as u64,
                        ..order(100 + (o + k * 7 + s) as i128)
                    })
                    .collect();
                executed.push((sender, NativeAction::PlaceOrderBatch(batch)));
            }
        }
        let body_index: Vec<u32> = (0..executed.len() as u32).collect();
        // `failing` of the block's actions, spread evenly, fail at one order.
        let n_actions = executed.len();
        let results = |list: &[(Address, NativeAction)]| {
            let mut out = Vec::new();
            for (pos, (_, a)) in list.iter().enumerate() {
                let fails = pos * failing / n_actions != (pos + 1) * failing / n_actions;
                for o in 0..flat_len(a) {
                    out.push(if fails && o == 3 {
                        err(FailureReason::OpenLimit, "open order limit reached")
                    } else {
                        ok()
                    });
                }
            }
            batch(out)
        };
        let (pre0, post0) = sort_native_actions(&executed);
        let (r_pre, r_post) = (results(&pre0), results(&post0));
        let median = |mut v: Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            v[v.len() / 2]
        };
        let (mut old_ms, mut new_ms) = (Vec::new(), Vec::new());
        let (mut old_map_ms, mut new_map_ms) = (Vec::new(), Vec::new());
        let mut n_failures = 0;
        let mut drop_ms = Vec::new();
        for _ in 0..iters {
            let (a, b) = (r_pre.clone(), r_post.clone());
            let t = std::time::Instant::now();
            let (pre, post) = sort_native_actions(&executed);
            let t_map = std::time::Instant::now();
            let old = native_failures_by_content(&executed, &body_index, [(&pre, a), (&post, b)]);
            old_map_ms.push(t_map.elapsed().as_secs_f64() * 1e3);
            old_ms.push(t.elapsed().as_secs_f64() * 1e3);
            drop((pre, post));

            let (mut a, mut b) = (r_pre.clone(), r_post.clone());
            let owned = executed.clone();
            let t = std::time::Instant::now();
            let ((pre, pre_i), (post, post_i)) = sort_native_actions_indexed(owned);
            let t_map = std::time::Instant::now();
            let new = native_failures(&body_index, [(&pre, &pre_i, &mut a), (&post, &post_i, &mut b)]);
            new_map_ms.push(t_map.elapsed().as_secs_f64() * 1e3);
            new_ms.push(t.elapsed().as_secs_f64() * 1e3);
            // Item 6 cut 5: the node drops the results on the end_resident
            // worker.
            let t_drop = std::time::Instant::now();
            drop((a, b));
            drop_ms.push(t_drop.elapsed().as_secs_f64() * 1e3);
            assert_eq!(new, old);
            n_failures = new.len();
        }
        println!(
            "UB failure_mapping actions={} orders/action={orders} failures={n_failures} \
             sort+map ms: old={:.2} new={:.2} | map only ms: old={:.2} (results dropped) \
             new={:.3} (results kept) | results drop ms={:.3} (cut 5: on the end_resident worker)",
            executed.len(),
            median(old_ms),
            median(new_ms),
            median(old_map_ms),
            median(new_map_ms),
            median(drop_ms),
        );
    }
}
