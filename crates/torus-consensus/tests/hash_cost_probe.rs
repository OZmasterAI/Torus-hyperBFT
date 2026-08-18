//! Scratch probe (ignored): per-block CPU cost of the consensus-thread body
//! passes at the bench shape (cap 100 actions x 400-order batches).
//! Run: cargo test --release -p torus-consensus --test hash_cost_probe -- --ignored --nocapture
use std::time::Instant;

use torus_types::{
    compute_action_hash, FixedPoint, NativeAction, OrderType, PlaceOrderParams, SignedNativeAction,
    TimeInForce,
};

fn batch_action(seed: u64) -> SignedNativeAction {
    let key = k256::ecdsa::SigningKey::from_slice(&[0x11u8; 32]).unwrap();
    let orders: Vec<PlaceOrderParams> = (0..400u64)
        .map(|i| PlaceOrderParams {
            market_id: (i % 10) as u64,
            is_buy: i % 2 == 0,
            price: FixedPoint::from_raw(6_000_000_000_000 + (i as i128) * 1_000_000),
            quantity: FixedPoint::from_raw(FixedPoint::SCALE + seed as i128),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: Some(seed * 1000 + i),
        })
        .collect();
    torus_types::eip712::sign_native_action(NativeAction::PlaceOrderBatch(orders), seed, &key)
}

#[test]
#[ignore]
fn hash_cost_probe() {
    let actions: Vec<SignedNativeAction> = (0..100).map(batch_action).collect();
    let bytes: usize = actions
        .iter()
        .map(|a| bincode::serialize(a).unwrap().len())
        .sum();
    eprintln!("100 x bs400 actions = {} KiB bincode", bytes / 1024);
    for _ in 0..3 {
        let t = Instant::now();
        let hashes: Vec<_> = actions.iter().map(compute_action_hash).collect();
        let d_hash = t.elapsed();
        let t = Instant::now();
        let enc: Vec<Vec<u8>> = actions.iter().map(|a| bincode::serialize(a).unwrap()).collect();
        let d_ser = t.elapsed();
        let t = Instant::now();
        let cl = actions.clone();
        let d_clone = t.elapsed();
        let t = Instant::now();
        let dig = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            for a in &actions {
                h.update(bincode::serialize(a).unwrap_or_default());
            }
            h.finalize()
        };
        let d_att = t.elapsed();
        eprintln!(
            "compute_action_hash x100: {:.2} ms | bincode x100: {:.2} ms | clone x100: {:.2} ms | attestation digest: {:.2} ms  ({} {} {} {})",
            d_hash.as_secs_f64() * 1e3,
            d_ser.as_secs_f64() * 1e3,
            d_clone.as_secs_f64() * 1e3,
            d_att.as_secs_f64() * 1e3,
            hashes.len(),
            enc.len(),
            cl.len(),
            dig.len()
        );
    }
}
