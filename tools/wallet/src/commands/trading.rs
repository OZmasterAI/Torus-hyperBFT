//! Trading commands: place-order, cancel-order, cancel-all, modify-order.
//!
//! Prices and quantities are `FixedPoint` (8 decimals, i128). All decimal args
//! flow through `parse_decimal_to_fixed_point` — never `parse_trs_to_wei`.

use torus_types::{NativeAction, OrderType, PlaceOrderParams, TimeInForce};

use crate::parse::parse_decimal_to_fixed_point;
use crate::rpc::RpcClient;
use crate::sign::submit_native_action;
use crate::Cli;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_place_order(
    cli: &Cli,
    rpc: &RpcClient,
    market: u64,
    side: &str,
    price: &str,
    quantity: &str,
    order_type: &str,
    tif: &str,
    trigger_price: Option<&str>,
    reduce_only: bool,
    client_id: Option<u64>,
) -> Result<(), String> {
    let is_buy = match side.to_lowercase().as_str() {
        "buy" => true,
        "sell" => false,
        _ => return Err(format!("--side must be 'buy' or 'sell' (got '{side}')")),
    };
    let price_fp = parse_decimal_to_fixed_point(price)?;
    let qty_fp = parse_decimal_to_fixed_point(quantity)?;

    let order_type_enum = match order_type.to_lowercase().as_str() {
        "limit" => OrderType::Limit,
        "market" => OrderType::Market,
        "stop-market" => {
            let t = trigger_price.ok_or("--trigger-price required for stop-market")?;
            OrderType::StopMarket {
                trigger: parse_decimal_to_fixed_point(t)?,
            }
        }
        "stop-limit" => {
            let t = trigger_price.ok_or("--trigger-price required for stop-limit")?;
            OrderType::StopLimit {
                trigger: parse_decimal_to_fixed_point(t)?,
                limit: price_fp,
            }
        }
        other => {
            return Err(format!(
                "unknown --order-type '{other}' (limit|market|stop-market|stop-limit)"
            ))
        }
    };

    let tif_enum = match tif.to_lowercase().as_str() {
        "gtc" => TimeInForce::GTC,
        "ioc" => TimeInForce::IOC,
        "fok" => TimeInForce::FOK,
        "post-only" => TimeInForce::PostOnly,
        other => return Err(format!("unknown --tif '{other}' (gtc|ioc|fok|post-only)")),
    };

    let params = PlaceOrderParams {
        market_id: market,
        is_buy,
        price: price_fp,
        quantity: qty_fp,
        order_type: order_type_enum,
        time_in_force: tif_enum,
        reduce_only,
        client_order_id: client_id,
    };
    submit_native_action(cli, rpc, NativeAction::PlaceOrder(params)).await
}

pub(crate) async fn cmd_cancel_order(
    cli: &Cli,
    rpc: &RpcClient,
    order_id: u128,
) -> Result<(), String> {
    submit_native_action(cli, rpc, NativeAction::CancelOrder { order_id }).await
}

pub(crate) async fn cmd_cancel_all(
    cli: &Cli,
    rpc: &RpcClient,
    market: Option<u64>,
) -> Result<(), String> {
    submit_native_action(
        cli,
        rpc,
        NativeAction::CancelAllOrders { market_id: market },
    )
    .await
}

pub(crate) async fn cmd_modify_order(
    cli: &Cli,
    rpc: &RpcClient,
    order_id: u128,
    new_price: Option<&str>,
    new_quantity: Option<&str>,
) -> Result<(), String> {
    if new_price.is_none() && new_quantity.is_none() {
        return Err("--price or --quantity required for modify-order".into());
    }
    let new_price = match new_price {
        Some(p) => Some(parse_decimal_to_fixed_point(p)?),
        None => None,
    };
    let new_qty = match new_quantity {
        Some(q) => Some(parse_decimal_to_fixed_point(q)?),
        None => None,
    };
    submit_native_action(
        cli,
        rpc,
        NativeAction::ModifyOrder {
            order_id,
            new_price,
            new_qty,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use torus_types::eip712::sign_native_action;
    use torus_types::{NativeAction, OrderType, PlaceOrderParams, TimeInForce};

    use crate::keystore::address_from_key;
    use crate::test_utils::test_signing_key;

    #[test]
    fn test_place_order_stop_limit_roundtrip() {
        let key = test_signing_key();
        let params = PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(250_000_000),
            quantity: torus_types::FixedPoint::from_raw(100_000),
            order_type: OrderType::StopLimit {
                trigger: torus_types::FixedPoint::from_raw(240_000_000),
                limit: torus_types::FixedPoint::from_raw(250_000_000),
            },
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: Some(42),
        };
        let signed =
            sign_native_action(NativeAction::PlaceOrder(params), 1_700_000_000_000u64, &key);
        let recovered = signed.recover_sender().expect("recover");
        assert_eq!(recovered, address_from_key(&key));
    }

    #[test]
    fn test_cancel_order_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::CancelOrder {
                order_id: 0xdead_beef_u128,
            },
            1_700_000_000_000u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_cancel_all_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::CancelAllOrders { market_id: Some(7) },
            1_700_000_000_000u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }

    #[test]
    fn test_modify_order_roundtrip() {
        let key = test_signing_key();
        let signed = sign_native_action(
            NativeAction::ModifyOrder {
                order_id: 123,
                new_price: Some(torus_types::FixedPoint::from_raw(150_000_000)),
                new_qty: None,
            },
            1_700_000_000_000u64,
            &key,
        );
        assert_eq!(signed.recover_sender().unwrap(), address_from_key(&key));
    }
}
