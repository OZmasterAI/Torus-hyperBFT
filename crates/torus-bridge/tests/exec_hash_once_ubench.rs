//! Section 92 step 8e µbench: exec-thread cost of the verify + anti-MEV sort
//! path per block, all EIP-712 actions warm in the trust-cache (the live
//! shape with `--exec-trust-cache`). `#[ignore]` (perf, not a gate); run in
//! release:
//!
//! ```text
//! cargo test --release -p torus-bridge --test exec_hash_once_ubench -- --ignored --nocapture
//! ```
//!
//! `old_path`: `batch_verify_native_actions_cached` + `sort_native_actions_indexed`
//! (two keccaks of the canonical bytes per action). `new_path`: the verify pass
//! returns the sort hashes and the sort takes them (one absorb).

use std::collections::HashMap;
use std::time::Instant;

use alloy_primitives::{Address, B256};
use torus_bridge::native_executor::{
    sort_native_actions_indexed, sort_native_actions_indexed_with_sort_hashes,
};
use torus_types::eip712::{
    batch_verify_native_actions_cached, batch_verify_native_actions_cached_with_sort_hashes,
    sign_native_action,
};
use torus_types::{
    FixedPoint, NativeAction, OrderType, PlaceOrderParams, SignedNativeAction, TimeInForce,
};

const NOW: u64 = 1_700_000_000_000;

/// `n_actions` EIP-712 `PlaceOrderBatch` actions of `orders` orders each, from
/// 64 signers, and a trust-cache holding every one of them.
fn block(n_actions: usize, orders: usize) -> (Vec<SignedNativeAction>, HashMap<B256, Address>) {
    let keys: Vec<k256::ecdsa::SigningKey> = (1..=64u8)
        .map(|b| {
            let mut bytes = [0u8; 32];
            bytes[31] = b;
            k256::ecdsa::SigningKey::from_slice(&bytes).unwrap()
        })
        .collect();
    let mut actions = Vec::with_capacity(n_actions);
    let mut cache = HashMap::new();
    for i in 0..n_actions {
        let mut batch: Vec<PlaceOrderParams> = (0..orders)
            .map(|j| PlaceOrderParams {
                market_id: (i + j) as u64 % 8,
                is_buy: j % 2 == 0,
                price: FixedPoint::from_raw(6_500_000_000_000 + (i * 1000 + j) as i128),
                quantity: FixedPoint::from_raw(10_000_000),
                order_type: OrderType::Limit,
                time_in_force: if j % 3 == 0 { TimeInForce::IOC } else { TimeInForce::GTC },
                reduce_only: false,
                client_order_id: Some((i * 100_000 + j) as u128),
            })
            .collect();
        let action = if orders == 1 {
            NativeAction::PlaceOrder(batch.remove(0))
        } else {
            NativeAction::PlaceOrderBatch(batch)
        };
        let key = &keys[i % keys.len()];
        let signed = sign_native_action(action, NOW + i as u64, key);
        let sender = signed.recover_sender().unwrap();
        cache.insert(torus_types::verified_cache_key(&signed).unwrap(), sender);
        actions.push(signed);
    }
    (actions, cache)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Returns (verify ms, sort ms) medians for one arm.
fn run(new: bool, actions: &[SignedNativeAction], cache: &HashMap<B256, Address>, iters: usize) -> (f64, f64) {
    let mut verify = Vec::with_capacity(iters);
    let mut sort = Vec::with_capacity(iters);
    for _ in 0..iters + 2 {
        let t = Instant::now();
        let (senders, hashes) = if new {
            batch_verify_native_actions_cached_with_sort_hashes(actions, NOW, |_| None, |k| {
                cache.get(k).copied()
            })
        } else {
            let s = batch_verify_native_actions_cached(actions, NOW, |_| None, |k| {
                cache.get(k).copied()
            });
            (s, Vec::new())
        };
        let tv = t.elapsed().as_secs_f64() * 1e3;
        // The replay-guard loop's clone (not timed: same in both arms).
        let mut sender_actions = Vec::with_capacity(actions.len());
        let mut sort_hashes = Vec::with_capacity(actions.len());
        for (i, a) in actions.iter().enumerate() {
            sender_actions.push((senders[i].unwrap(), a.action.clone()));
            if new {
                sort_hashes.push(hashes[i]);
            }
        }
        let t = Instant::now();
        let out = if new {
            sort_native_actions_indexed_with_sort_hashes(sender_actions, sort_hashes)
        } else {
            sort_native_actions_indexed(sender_actions)
        };
        let ts = t.elapsed().as_secs_f64() * 1e3;
        std::hint::black_box(out);
        verify.push(tv);
        sort.push(ts);
    }
    // Drop the two warm-up rounds.
    (median(verify[2..].to_vec()), median(sort[2..].to_vec()))
}

fn report(new: bool) {
    let arm = if new { "new_path" } else { "old_path" };
    for &(n, orders) in &[(100usize, 239usize), (24_000, 1)] {
        let (actions, cache) = block(n, orders);
        let (v, s) = run(new, &actions, &cache, 41);
        println!(
            "{arm} actions={n} orders/action={orders}: verify {v:.3} ms  sort {s:.3} ms  total {:.3} ms",
            v + s
        );
    }
}

#[test]
#[ignore]
fn exec_hash_once_ubench_old_path() {
    report(false);
}

#[test]
#[ignore]
fn exec_hash_once_ubench_new_path() {
    report(true);
}
