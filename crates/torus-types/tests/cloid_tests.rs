//! s114 cloid: client order ids are u128 everywhere (owner decision 1), and
//! `NativeAction::CancelByCloid { market_id, cloid }` cancels by one.
//! Pins the canonical bytes, the EIP-712 encodings and the JSON / bincode
//! forms of the widened field.

use alloy_primitives::keccak256;
use torus_types::eip712::eip712_struct_hash;
use torus_types::{FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

const NONCE: u64 = 1_700_000_000_000;
/// Above u64::MAX: a u64 field cannot hold it.
const BIG: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;

fn params(cloid: Option<u128>) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 7,
        is_buy: true,
        price: FixedPoint::from_raw(6_500_000_000_000),
        quantity: FixedPoint::from_raw(10_000_000),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: cloid,
    }
}

fn word_u(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn word_i(v: i128) -> [u8; 32] {
    let mut w = [if v < 0 { 0xff } else { 0 }; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

/// The order fields of `PlaceOrder` / `PlaceOrderItem`, independently encoded.
fn order_fields(p: &PlaceOrderParams) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&word_u(p.market_id as u128));
    b.extend_from_slice(&word_u(p.is_buy as u128));
    b.extend_from_slice(&word_i(p.price.raw()));
    b.extend_from_slice(&word_i(p.quantity.raw()));
    b.extend_from_slice(&word_u(0)); // Limit
    b.extend_from_slice(&word_u(0)); // GTC
    b.extend_from_slice(&word_u(p.reduce_only as u128));
    b.extend_from_slice(&word_u(p.client_order_id.unwrap_or(0)));
    b.extend_from_slice(&word_u(p.client_order_id.is_some() as u128));
    b
}

#[test]
fn place_order_canonical_bytes_carry_a_16_byte_cloid() {
    let some = NativeAction::PlaceOrder(params(Some(BIG))).canonical_bytes();
    let mut tail = vec![1u8];
    tail.extend_from_slice(&BIG.to_be_bytes());
    assert!(some.ends_with(&tail), "{some:02x?}");
    let none = NativeAction::PlaceOrder(params(None)).canonical_bytes();
    assert_eq!(none.len() + 16, some.len());
    assert_eq!(*none.last().unwrap(), 0);
}

#[test]
fn place_order_eip712_types_the_cloid_as_uint128() {
    let p = params(Some(BIG));
    let th = keccak256(
        "PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
         uint8 orderType,uint8 timeInForce,bool reduceOnly,\
         uint128 clientOrderId,bool hasClientOrderId,uint64 nonce)",
    );
    let mut buf = th.to_vec();
    buf.extend_from_slice(&order_fields(&p));
    buf.extend_from_slice(&word_u(NONCE as u128));
    assert_eq!(
        eip712_struct_hash(&NativeAction::PlaceOrder(p), NONCE),
        keccak256(&buf)
    );
}

#[test]
fn place_order_batch_item_types_the_cloid_as_uint128() {
    let orders = vec![params(Some(BIG)), params(None)];
    let item_th = keccak256(
        "PlaceOrderItem(uint64 marketId,bool isBuy,int128 price,int128 quantity,\
         uint8 orderType,uint8 timeInForce,bool reduceOnly,\
         uint128 clientOrderId,bool hasClientOrderId)",
    );
    let mut acc = Vec::new();
    for p in &orders {
        let mut b = item_th.to_vec();
        b.extend_from_slice(&order_fields(p));
        acc.extend_from_slice(keccak256(&b).as_slice());
    }
    let th = keccak256("PlaceOrderBatch(bytes32 ordersHash,uint64 count,uint64 nonce)");
    let mut buf = th.to_vec();
    buf.extend_from_slice(keccak256(&acc).as_slice());
    buf.extend_from_slice(&word_u(2));
    buf.extend_from_slice(&word_u(NONCE as u128));
    assert_eq!(
        eip712_struct_hash(&NativeAction::PlaceOrderBatch(orders), NONCE),
        keccak256(&buf)
    );
}

#[test]
fn cancel_by_cloid_canonical_bytes_and_eip712() {
    let a = NativeAction::CancelByCloid {
        market_id: 7,
        cloid: BIG,
    };
    let mut want = vec![29u8];
    want.extend_from_slice(&7u64.to_be_bytes());
    want.extend_from_slice(&BIG.to_be_bytes());
    assert_eq!(a.canonical_bytes(), want);

    let th = keccak256("CancelByCloid(uint64 marketId,uint128 cloid,uint64 nonce)");
    let mut buf = th.to_vec();
    buf.extend_from_slice(&word_u(7));
    buf.extend_from_slice(&word_u(BIG));
    buf.extend_from_slice(&word_u(NONCE as u128));
    assert_eq!(eip712_struct_hash(&a, NONCE), keccak256(&buf));
}

#[test]
fn cancel_by_cloid_is_a_trading_action() {
    assert!(NativeAction::CancelByCloid {
        market_id: 1,
        cloid: 1
    }
    .is_trading());
}

/// JSON (RPC, HL style): a cloid is a `0x` + 32 hex digit string, so values
/// above 2^64 survive `serde_json::Value` and JS clients; a number is also
/// accepted on input. bincode keeps the plain 16-byte integer.
#[test]
fn cloid_json_is_hex_and_bincode_is_binary() {
    let p = params(Some(BIG));
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(
        v["client_order_id"],
        serde_json::json!("0x0123456789abcdeffedcba9876543210")
    );
    let back: PlaceOrderParams = serde_json::from_value(v).unwrap();
    assert_eq!(back.client_order_id, Some(BIG));

    let mut num = serde_json::to_value(params(None)).unwrap();
    assert!(num["client_order_id"].is_null());
    num["client_order_id"] = serde_json::json!(42);
    let back: PlaceOrderParams = serde_json::from_value(num).unwrap();
    assert_eq!(back.client_order_id, Some(42));

    let c = NativeAction::CancelByCloid {
        market_id: 3,
        cloid: BIG,
    };
    let v = serde_json::to_value(&c).unwrap();
    assert_eq!(
        v["CancelByCloid"]["cloid"],
        serde_json::json!("0x0123456789abcdeffedcba9876543210")
    );

    let wire = bincode::serialize(&p).unwrap();
    assert!(wire.windows(16).any(|w| w == BIG.to_le_bytes()));
    let back: PlaceOrderParams = bincode::deserialize(&wire).unwrap();
    assert_eq!(back.client_order_id, Some(BIG));
}
