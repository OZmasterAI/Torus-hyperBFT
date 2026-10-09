//! Item 6 Phase 2 step 0.2 µbench (P2-5): the execution path's hot
//! `Address`- and `OrderId`-keyed maps with std's SipHash (`RandomState`)
//! vs `foldhash::fast::RandomState` (seeded per process, the plan's option C).
//! Section 22 of the ozarchy results doc put SipHash at ~13% of execution
//! self time (`write` 6.6%, `hash_one<Address>` 3.5%, `hash_one<u128>` 3.1%).
//!
//! Cases (shapes from the standard 300-market cell): hashing alone
//! (`hash_one`) of an `Address` and of an order id; lookups in an `Address`
//! map of the 5,000 bench senders (book `trader_orders`, the balance cache),
//! in a `(Address, MarketId)` map (the position cache), and in an order-id
//! map of one book (~5,300 resting orders); order-id insert + remove churn
//! (fills and cancels); `Address` set inserts (dirty sets). Each case prints
//! ns per op for both hashers (best of `UB_HASH_REPS` runs of `UB_HASH_OPS`
//! ops) and their ratio.
//!
//! The last line turns the `hash_one` ratios into an estimate of the saving
//! per native block: `UB_HASH_SHARE` (default 0.132, the section 22 share of
//! execution self time) x `UB_HASH_EXEC_MS` (default 190, engine ms per
//! native block on crab r2) x (1 - mean `hash_one` ratio). An estimate only:
//! a cell measures the real saving.
//!
//!   cargo test -q -p torus-bridge --release --test ubench_hasher -- --ignored --nocapture

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::hint::black_box;
use std::time::Instant;

use alloy_primitives::Address;

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Deterministic addresses whose every byte varies, like real ones.
fn address(i: u64) -> Address {
    let mut b = [0u8; 20];
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    for chunk in b.chunks_mut(8) {
        x = x.rotate_left(17).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        chunk.copy_from_slice(&x.to_le_bytes()[..chunk.len()]);
    }
    Address::from(b)
}

/// Best (lowest) ns per op of `reps` runs of `f`, which does `ops` ops.
fn ns_per_op(reps: usize, ops: usize, mut f: impl FnMut() -> u64) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..reps {
        let t = Instant::now();
        black_box(f());
        best = best.min(t.elapsed().as_nanos() as f64 / ops as f64);
    }
    best
}

/// ns per op of `case` with hasher `S`.
fn run_case<S: BuildHasher + Default + Clone>(case: &str, ops: usize, reps: usize) -> f64 {
    let s = S::default();
    let senders: Vec<Address> = (0..5_000).map(address).collect();
    let ids: Vec<u128> = (0..5_300u128).map(|i| 1_000_000 + i * 37).collect();
    let pick = |i: usize, n: usize| (i * 7_919) % n;
    match case {
        "hash_one_address" => ns_per_op(reps, ops, || {
            (0..ops).fold(0u64, |acc, i| {
                acc.wrapping_add(s.hash_one(senders[i % senders.len()]))
            })
        }),
        "hash_one_order_id" => ns_per_op(reps, ops, || {
            (0..ops).fold(0u64, |acc, i| {
                acc.wrapping_add(s.hash_one(ids[i % ids.len()] + i as u128))
            })
        }),
        "address_map_get" => {
            let mut m: HashMap<Address, u32, S> = HashMap::with_hasher(s.clone());
            m.extend(senders.iter().enumerate().map(|(i, a)| (*a, i as u32)));
            ns_per_op(reps, ops, || {
                (0..ops)
                    .map(|i| m[&senders[pick(i, senders.len())]] as u64)
                    .sum()
            })
        }
        "position_map_get" => {
            let mut m: HashMap<(Address, u64), u32, S> = HashMap::with_hasher(s.clone());
            for (i, a) in senders.iter().enumerate() {
                for k in 0..20u64 {
                    m.insert((*a, 1 + (i as u64 * 13 + k * 15) % 300), k as u32);
                }
            }
            let keys: Vec<(Address, u64)> = m.keys().copied().collect();
            ns_per_op(reps, ops, || {
                (0..ops).map(|i| m[&keys[pick(i, keys.len())]] as u64).sum()
            })
        }
        "order_id_map_get" => {
            let mut m: HashMap<u128, u32, S> = HashMap::with_hasher(s.clone());
            m.extend(ids.iter().enumerate().map(|(i, id)| (*id, i as u32)));
            ns_per_op(reps, ops, || {
                (0..ops).map(|i| m[&ids[pick(i, ids.len())]] as u64).sum()
            })
        }
        // Two ops per step: remove the oldest resting id, insert a new one.
        "order_id_churn" => ns_per_op(reps, ops, || {
            let mut m: HashMap<u128, u32, S> = HashMap::with_hasher(s.clone());
            m.extend((0..5_300u128).map(|i| (i, 0)));
            for i in 0..(ops / 2) as u128 {
                m.remove(&i);
                m.insert(5_300 + i, 0);
            }
            m.len() as u64
        }),
        "address_set_insert" => ns_per_op(reps, ops, || {
            let mut set: HashSet<Address, S> = HashSet::with_hasher(s.clone());
            let mut acc = 0u64;
            for i in 0..ops {
                if i % 2_000 == 0 {
                    acc += set.len() as u64;
                    set.clear();
                }
                set.insert(senders[pick(i, senders.len())]);
            }
            acc
        }),
        _ => unreachable!("{case}"),
    }
}

#[test]
#[ignore]
fn ubench_hasher() {
    let ops: usize = env("UB_HASH_OPS", 2_000_000);
    let reps: usize = env("UB_HASH_REPS", 5);
    let share: f64 = env("UB_HASH_SHARE", 0.132);
    let exec_ms: f64 = env("UB_HASH_EXEC_MS", 190.0);
    println!(
        "hasher_ubench ops={ops} reps={reps} (ns per op, best of reps; sip = std RandomState, \
         fold = foldhash::fast::RandomState)"
    );
    let mut hash_one = Vec::new();
    for case in [
        "hash_one_address",
        "hash_one_order_id",
        "address_map_get",
        "position_map_get",
        "order_id_map_get",
        "order_id_churn",
        "address_set_insert",
    ] {
        let sip = run_case::<std::collections::hash_map::RandomState>(case, ops, reps);
        let fold = run_case::<foldhash::fast::RandomState>(case, ops, reps);
        if case.starts_with("hash_one") {
            hash_one.push(fold / sip);
        }
        println!(
            "hasher_ubench case={case} sip_ns={sip:.2} fold_ns={fold:.2} ratio={:.3}",
            fold / sip
        );
    }
    let ratio = hash_one.iter().sum::<f64>() / hash_one.len() as f64;
    println!(
        "hasher_ubench estimate: share {share} x exec_ms {exec_ms} x (1 - hash_one ratio {ratio:.3}) \
         = {:.1} ms per native block",
        share * exec_ms * (1.0 - ratio)
    );
}
