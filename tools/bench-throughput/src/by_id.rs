//! `--cancel-by-id-fraction F` / `--modify-fraction G` (item 6 Phase 2 step
//! 0.2, the P2-1b cell): econ senders also cancel and modify their OWN
//! resting orders by id, as HL makers do, so the cost of finding an order by
//! id is measured before P2-1b removes its scan over every book.
//!
//! Per fire, with probability F the sender sends a `CancelOrder` and with
//! probability G a `ModifyOrder` (price one tick away from the mid, so it
//! never crosses) instead of its drawn action. The ids come from
//! `torus_getOpenOrders(sender, market)` on one market of the sender's plan,
//! fetched when the sender's list is empty and used once each; a cancel-all
//! clears the list. A sender with no order there sends its drawn action.
//! Both 0 (default): no draw, so the load is byte-identical to before.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use alloy_primitives::Address;
use rand::Rng;
use torus_types::{FixedPoint, MarketId, NativeAction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Cancel,
    Modify,
}

/// The by-id kind of one fire, or `None` for the drawn action. Draws from
/// `rng` only when a fraction is set.
pub fn draw(rng: &mut impl Rng, cancel_fraction: f64, modify_fraction: f64) -> Option<Kind> {
    if cancel_fraction + modify_fraction <= 0.0 {
        return None;
    }
    let u: f64 = rng.gen_range(0.0..1.0);
    if u < cancel_fraction {
        Some(Kind::Cancel)
    } else if u < cancel_fraction + modify_fraction {
        Some(Kind::Modify)
    } else {
        None
    }
}

/// One resting order of the sender, from `torus_getOpenOrders`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnOrder {
    pub id: u128,
    pub is_buy: bool,
    pub price: FixedPoint,
}

/// The action for `o`: a cancel, or a modify moving the price one tick (1.0,
/// the econ books' tick) away from the mid.
pub fn action(kind: Kind, o: OwnOrder) -> NativeAction {
    match kind {
        Kind::Cancel => NativeAction::CancelOrder { order_id: o.id },
        Kind::Modify => {
            let price = if o.is_buy {
                o.price - FixedPoint::ONE
            } else {
                o.price + FixedPoint::ONE
            };
            NativeAction::ModifyOrder {
                order_id: o.id,
                new_price: Some(price),
                new_qty: None,
            }
        }
    }
}

/// A `torus_getOpenOrders` reply -> the orders (`None`: no result list).
pub fn parse_open_orders(reply: &serde_json::Value) -> Option<Vec<OwnOrder>> {
    let list = reply.get("result")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|o| {
                Some(OwnOrder {
                    id: u128::from_str_radix(
                        o.get("orderId")?.as_str()?.trim_start_matches("0x"),
                        16,
                    )
                    .ok()?,
                    is_buy: o.get("side")?.as_str()? == "buy",
                    price: o.get("price")?.as_str()?.parse().ok()?,
                })
            })
            .collect(),
    )
}

/// `trader`'s resting orders in `market`, read from `url`.
pub async fn fetch(
    client: &reqwest::Client,
    url: &str,
    trader: Address,
    market: MarketId,
) -> Result<Vec<OwnOrder>, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "method": "torus_getOpenOrders",
        "params": [format!("{trader:#x}"), format!("{market:#x}")], "id": 1
    });
    let reply: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    parse_open_orders(&reply)
        .ok_or_else(|| format!("no result: {}", reply.get("error").unwrap_or(&reply)))
}

/// Load-gen counts of the by-id actions, apart from the econ mix.
#[derive(Default)]
pub struct Stats {
    pub sent: [AtomicU64; 2],
    pub accepted: [AtomicU64; 2],
    pub lookups: AtomicU64,
    pub lookup_errors: AtomicU64,
    /// Draws that found no own order (the drawn action went instead).
    pub no_order: AtomicU64,
}

impl Stats {
    /// Count one by-id fire (one action) and whether the RPC admitted it.
    pub fn record(&self, kind: Kind, result: &Result<Vec<Option<String>>, String>) {
        let k = kind as usize;
        self.sent[k].fetch_add(1, Relaxed);
        if matches!(result, Ok(items) if items.first().is_none_or(|e| e.is_none())) {
            self.accepted[k].fetch_add(1, Relaxed);
        }
    }

    /// End-of-run line (parsed by tools/matched-bench/summarize.py).
    pub fn report(&self) -> String {
        let ld = |a: &AtomicU64| a.load(Relaxed);
        format!(
            "Cancel-by-id (load-gen): cancel sent {} accepted {} | modify sent {} accepted {} | \
             lookups {} errors {} | no own order {}",
            ld(&self.sent[0]),
            ld(&self.accepted[0]),
            ld(&self.sent[1]),
            ld(&self.accepted[1]),
            ld(&self.lookups),
            ld(&self.lookup_errors),
            ld(&self.no_order),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    #[test]
    fn off_draws_nothing() {
        let mut a = StdRng::seed_from_u64(7);
        let mut b = StdRng::seed_from_u64(7);
        assert_eq!(draw(&mut a, 0.0, 0.0), None);
        assert_eq!(
            a.gen::<u64>(),
            b.gen::<u64>(),
            "off must not consume the rng"
        );
    }

    #[test]
    fn draw_follows_the_fractions() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut n = [0usize; 3];
        for _ in 0..20_000 {
            n[match draw(&mut rng, 0.3, 0.2) {
                Some(Kind::Cancel) => 0,
                Some(Kind::Modify) => 1,
                None => 2,
            }] += 1;
        }
        for (got, want) in n.iter().zip([0.3, 0.2, 0.5]) {
            let share = *got as f64 / 20_000.0;
            assert!((share - want).abs() < 0.02, "{n:?}");
        }
    }

    #[test]
    fn actions_cancel_or_move_away_from_the_mid() {
        let buy = OwnOrder {
            id: 9,
            is_buy: true,
            price: fp(30_003),
        };
        let sell = OwnOrder {
            id: 10,
            is_buy: false,
            price: fp(29_998),
        };
        assert!(matches!(
            action(Kind::Cancel, buy),
            NativeAction::CancelOrder { order_id: 9 }
        ));
        for (o, id, want) in [(buy, 9, fp(30_002)), (sell, 10, fp(29_999))] {
            match action(Kind::Modify, o) {
                NativeAction::ModifyOrder {
                    order_id,
                    new_price: Some(p),
                    new_qty: None,
                } => {
                    assert_eq!((order_id, p), (id, want))
                }
                a => panic!("{a:?}"),
            }
        }
    }

    /// The node's reply shape (`RpcOpenOrder`, camelCase, hex ids, decimal
    /// prices); entries that do not parse are skipped.
    #[test]
    fn parses_the_open_orders_reply() {
        let reply = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": [
            {"orderId": "0x1f", "marketId": "0x3", "side": "buy", "price": "30001.00000000",
             "remainingQty": "1.00000000", "originalQty": "1.00000000"},
            {"orderId": "0x20", "marketId": "0x3", "side": "sell", "price": "29999.50000000"},
            {"orderId": "zz", "side": "sell", "price": "1"},
        ]});
        assert_eq!(
            parse_open_orders(&reply).unwrap(),
            vec![
                OwnOrder {
                    id: 31,
                    is_buy: true,
                    price: fp(30_001)
                },
                OwnOrder {
                    id: 32,
                    is_buy: false,
                    price: FixedPoint::from_raw(29_999 * FixedPoint::SCALE + FixedPoint::SCALE / 2)
                },
            ]
        );
        assert_eq!(
            parse_open_orders(&serde_json::json!({"error": {"code": -32000}})),
            None
        );
    }

    #[test]
    fn stats_count_sent_and_admitted() {
        let s = Stats::default();
        s.record(Kind::Cancel, &Ok(vec![None]));
        s.record(Kind::Cancel, &Ok(vec![Some("busy".into())]));
        s.record(Kind::Modify, &Err("down".into()));
        s.lookups.fetch_add(4, Relaxed);
        s.no_order.fetch_add(1, Relaxed);
        assert_eq!(
            s.report(),
            "Cancel-by-id (load-gen): cancel sent 2 accepted 1 | modify sent 1 accepted 0 | \
             lookups 4 errors 0 | no own order 1"
        );
    }
}
