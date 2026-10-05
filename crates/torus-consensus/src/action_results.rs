//! v2 action status: the executor's per-entry results mapped back to the
//! native actions' positions in the block body
//! (`torus_state::action_status`).
//!
//! The exec thread runs `sort_native_actions` (pre-EVM / post-EVM lists, each
//! sorted by (category, sender, content hash)) and one `execute_batch` per
//! list, which flattens every PlaceOrderBatch into one result per order. This
//! module undoes both steps for the failing entries only: flat result ->
//! action of the list (the executor's flatten, replayed) -> body position
//! (same sender + category, then same content and rank among identical
//! actions: the sort is stable). Only failures carry data out of here; the
//! encoding happens where the record is written (the flush worker on the
//! pipelined path).
//!
//! Deterministic: a pure function of the block, the replay-guard decisions and
//! the executor's results, which are themselves identical in every exec mode.

use std::collections::HashMap;

use alloy_primitives::Address;
use torus_bridge::native_executor::classify_action;
use torus_bridge::NativeBatchResult;
use torus_state::action_status::{BlockActionStatus, NativeActionFailure};
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

/// A failing action of one list: (list position, order, failed orders, message).
type ListFailure = (usize, u32, u32, String);

/// Failures of one `execute_batch` over `list`, by list position. `None` when
/// the result count does not match the flatten (never expected; the caller
/// then records no failure for the list rather than a wrong one).
fn list_failures(
    list: &[(Address, NativeAction)],
    mut result: NativeBatchResult,
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
            let message = entries[first]
                .error
                .take()
                .unwrap_or_else(|| "failed".to_string());
            out.push((pos, first as u32, failed_orders, message));
        }
    }
    Some(out)
}

/// The block's native execution failures, ascending body index.
///
/// `executed` = the actions handed to `sort_native_actions` (body order, the
/// replay guard's skips removed), `body_index[k]` = body position of
/// `executed[k]`; `batches` = each sorted list with its `execute_batch`
/// result.
pub fn native_failures(
    executed: &[(Address, NativeAction)],
    body_index: &[u32],
    batches: [(&[(Address, NativeAction)], NativeBatchResult); 2],
) -> Vec<NativeActionFailure> {
    let mut out = Vec::new();
    // Built on the first failure only: executed positions by sender.
    let mut by_sender: Option<HashMap<Address, Vec<usize>>> = None;
    for (list, result) in batches {
        let Some(failures) = list_failures(list, result) else {
            tracing::error!(
                actions = list.len(),
                "native batch result count does not match its actions — no failures recorded"
            );
            continue;
        };
        for (pos, order, failed_orders, message) in failures {
            let by_sender = by_sender.get_or_insert_with(|| {
                let mut m: HashMap<Address, Vec<usize>> = HashMap::new();
                for (k, (sender, _)) in executed.iter().enumerate() {
                    m.entry(*sender).or_default().push(k);
                }
                m
            });
            if let Some(k) = body_position(executed, by_sender, list, pos) {
                out.push(NativeActionFailure::new(
                    body_index[k],
                    order,
                    failed_orders,
                    message,
                ));
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
        m.exec_action_failures
            .inc_by(status.native_failed.len() as u64);
        m.exec_action_status_bytes.inc_by(bytes.len() as u64);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_bridge::NativeActionResult;
    use torus_state::action_status::FailureReason;
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
        }
    }

    fn err(msg: &str) -> NativeActionResult {
        NativeActionResult {
            action_type: "x",
            success: false,
            error: Some(msg.to_string()),
            gas_used: 0,
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
        // Flat: [] (empty batch) + 3 orders + 1 + 1.
        let result = batch(vec![
            ok(),
            err("order rejected: price 2 is not a multiple of the tick 1"),
            err("insufficient margin: need 1, have 0 (account)"),
            ok(),
            err("open order limit reached: 1 open orders, limit 1"),
        ]);
        let failures = native_failures(
            &executed,
            &body_index,
            [(&list, result), (&[], batch(vec![]))],
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

    #[test]
    fn mismatched_result_count_records_nothing() {
        let a = Address::repeat_byte(1);
        let executed = vec![(a, NativeAction::PlaceOrder(order(1)))];
        let failures = native_failures(
            &executed,
            &[0],
            [
                (&executed, batch(vec![err("x"), err("y")])),
                (&[], batch(vec![])),
            ],
        );
        assert!(failures.is_empty());
    }
}
