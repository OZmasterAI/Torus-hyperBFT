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

/// Both arms on the same blocks, alternating order over 3 rounds (the host
/// is shared, so one-shot runs are noisy).
#[test]
#[ignore]
fn exec_hash_once_ubench_ab() {
    for round in 0..3 {
        let order = if round % 2 == 0 { [false, true] } else { [true, false] };
        for new in order {
            report(new);
        }
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

// ============================================================================
// s80 keyed-local-trust-key: SHA-256 (SHA-NI) node-local trust-cache key with
// the anti-MEV sort hash keccak(C) memoized in the cache value.
//
// Arm `keccak` (today, 29bfc705): `batch_verify_native_actions_cached_with_sort_hashes`
// (encode C, one keccak absorb forked into keccak(C) and keccak(C||nonce||v||r||s),
// lookup) + `sort_native_actions_indexed_with_sort_hashes`.
// Arm `sha256`: `batch_verify_native_actions_local_key_memo` (encode C,
// SHA-256(C||nonce||v||r||s), lookup -> (sender, keccak(C)); keccak only on a
// miss) + the same sort with the memoized hashes.
//
// Block shape (bench CL21): 60 actions = 37 PlaceOrderBatch x 400 + 23
// CancelAllOrders. `tie_pct` = share of the places whose (category, sender)
// sort key ties with another place (pairs of places from one sender).
//
// Arm `sha256-tieonly` (the no-memo alternative): SHA-256 key -> sender only;
// keccak(C) is computed on E for the actions whose (category, sender) ties
// with another action (only there can the hash change the order).
// ============================================================================

use std::sync::RwLock;
use torus_bridge::native_executor::{classify_action, IndexedActions};
use torus_types::eip712::batch_verify_native_actions_local_key_memo;

const BENCH_PLACES: usize = 37;
const BENCH_CANCELS: usize = 23;
const BENCH_ORDERS: usize = 400;

type KeccakCache = RwLock<HashMap<B256, Address>>;
type MemoCache = RwLock<HashMap<B256, (Address, B256)>>;

/// The three trust-caches, seeded like ingress would.
struct Caches {
    /// keccak key -> sender (today).
    keccak: KeccakCache,
    /// SHA-256 key -> (sender, keccak(C)).
    memo: MemoCache,
    /// SHA-256 key -> sender (the tie-only arm).
    local: KeccakCache,
}

/// Number of same-sender place pairs for a tie share (participants = 2 x pairs).
fn tie_pairs(tie_pct: usize) -> usize {
    (BENCH_PLACES * tie_pct + 100) / 200
}

/// The bench-shaped block, input order shuffled deterministically by `salt`.
fn bench_block(tie_pct: usize, salt: u64) -> Vec<SignedNativeAction> {
    let keys: Vec<k256::ecdsa::SigningKey> = (1..=BENCH_PLACES as u8)
        .map(|b| {
            let mut bytes = [0u8; 32];
            bytes[30] = 0x5a;
            bytes[31] = b;
            k256::ecdsa::SigningKey::from_slice(&bytes).unwrap()
        })
        .collect();
    let pairs = tie_pairs(tie_pct);
    let distinct = BENCH_PLACES - pairs;
    let mut actions = Vec::with_capacity(BENCH_PLACES + BENCH_CANCELS);
    for i in 0..BENCH_PLACES {
        // The last `pairs` places reuse the signer of places 0..pairs.
        let signer = if i < distinct { i } else { i - distinct };
        let batch: Vec<PlaceOrderParams> = (0..BENCH_ORDERS)
            .map(|j| PlaceOrderParams {
                market_id: ((i * 7 + j) % 300) as u64,
                is_buy: j % 2 == 0,
                price: FixedPoint::from_raw(
                    6_500_000_000_000 + (salt as i128) * 1_000_000 + (i * 1000 + j) as i128,
                ),
                quantity: FixedPoint::from_raw(10_000_000 + j as i128),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                // The 300-market cells run with cloid share 0: 45 B per order.
                client_order_id: None,
            })
            .collect();
        actions.push(sign_native_action(
            NativeAction::PlaceOrderBatch(batch),
            NOW + salt * 1000 + i as u64,
            &keys[signer],
        ));
    }
    for i in 0..BENCH_CANCELS {
        actions.push(sign_native_action(
            NativeAction::CancelAllOrders { market_id: Some((i * 13 % 300) as u64) },
            NOW + salt * 1000 + 500 + i as u64,
            &keys[i],
        ));
    }
    // Deterministic Fisher-Yates (the proposer's order is arbitrary).
    let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ salt.wrapping_mul(0xD1B5_4A32_D192_ED03);
    for i in (1..actions.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        actions.swap(i, (x % (i as u64 + 1)) as usize);
    }
    actions
}

/// Both trust-caches seeded like ingress would (outside any timed region),
/// plus `filler` unrelated entries so the maps have a live-like size.
fn seed_caches(actions: &[SignedNativeAction], filler: usize) -> Caches {
    let mut kc = HashMap::with_capacity(filler + actions.len());
    let mut memo = HashMap::with_capacity(filler + actions.len());
    let mut local = HashMap::with_capacity(filler + actions.len());
    for i in 0..filler {
        let k = alloy_primitives::keccak256((i as u64).to_be_bytes());
        kc.insert(k, Address::from_word(k));
        memo.insert(k, (Address::from_word(k), k));
        local.insert(k, Address::from_word(k));
    }
    let mut scratch = Vec::new();
    for a in actions {
        let sender = a.recover_sender().unwrap();
        kc.insert(torus_types::verified_cache_key(a).unwrap(), sender);
        let (sort, key) = torus_types::sort_hash_and_local_trust_key_with_scratch(a, &mut scratch);
        memo.insert(key.unwrap(), (sender, sort));
        local.insert(key.unwrap(), sender);
    }
    Caches { keccak: RwLock::new(kc), memo: RwLock::new(memo), local: RwLock::new(local) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TrustKeyArm {
    Keccak,
    Sha256,
    Sha256TieOnly,
}

impl TrustKeyArm {
    fn name(self) -> &'static str {
        match self {
            TrustKeyArm::Keccak => "keccak",
            TrustKeyArm::Sha256 => "sha256",
            TrustKeyArm::Sha256TieOnly => "sha256-tieonly",
        }
    }
}

/// One E-path pass: (senders, sort hashes, sorted lists, verify ms, sort ms).
#[allow(clippy::type_complexity)]
fn e_path(
    arm: TrustKeyArm,
    actions: &[SignedNativeAction],
    caches: &Caches,
) -> (Vec<Option<Address>>, Vec<B256>, (IndexedActions, IndexedActions), f64, f64) {
    let t = Instant::now();
    let (senders, hashes) = match arm {
        TrustKeyArm::Keccak => batch_verify_native_actions_cached_with_sort_hashes(
            actions,
            NOW,
            |_| None,
            |k| caches.keccak.read().unwrap().get(k).copied(),
        ),
        TrustKeyArm::Sha256 => batch_verify_native_actions_local_key_memo(
            actions,
            NOW,
            |_| None,
            |k| caches.memo.read().unwrap().get(k).copied(),
        ),
        TrustKeyArm::Sha256TieOnly => {
            // A hit carries no sort hash (ZERO placeholder); a miss hashes C.
            let (senders, mut hashes) = batch_verify_native_actions_local_key_memo(
                actions,
                NOW,
                |_| None,
                |k| caches.local.read().unwrap().get(k).map(|s| (*s, B256::ZERO)),
            );
            // keccak(C) only where (category, sender) ties: elsewhere the
            // hash cannot change the order.
            let mut groups: HashMap<(u8, Address), u32> = HashMap::with_capacity(actions.len());
            let keyed: Vec<Option<(u8, Address)>> = actions
                .iter()
                .zip(&senders)
                .map(|(a, s)| s.map(|s| (classify_action(&a.action) as u8, s)))
                .collect();
            for k in keyed.iter().flatten() {
                *groups.entry(*k).or_default() += 1;
            }
            for (i, k) in keyed.iter().enumerate() {
                if k.is_some_and(|k| groups[&k] > 1) && hashes[i] == B256::ZERO {
                    hashes[i] = alloy_primitives::keccak256(actions[i].action.canonical_bytes());
                }
            }
            (senders, hashes)
        }
    };
    let tv = t.elapsed().as_secs_f64() * 1e3;
    // The replay guard's clone (untimed: identical in both arms).
    let sender_actions: Vec<(Address, NativeAction)> = actions
        .iter()
        .zip(&senders)
        .map(|(a, s)| (s.unwrap(), a.action.clone()))
        .collect();
    let t = Instant::now();
    let sorted = sort_native_actions_indexed_with_sort_hashes(sender_actions, hashes.clone());
    let ts = t.elapsed().as_secs_f64() * 1e3;
    (senders, hashes, sorted, tv, ts)
}

/// The order-defining part of a sorted list: input positions and senders.
fn order_of(list: &IndexedActions) -> (Vec<u32>, Vec<Address>) {
    (list.1.clone(), list.0.iter().map(|(s, _)| *s).collect())
}

/// Correctness gate (runs in the normal suite): at every tie share the
/// SHA-256 memo arm resolves the same senders and sort hashes and sorts the
/// block into the same lists as today's keccak arm and as the uncached path
/// (fresh recover + `sort_native_actions_indexed`).
#[test]
fn trust_key_memo_arm_matches_keccak_arm_on_bench_blocks() {
    for tie_pct in [0usize, 20, 50] {
        let actions = bench_block(tie_pct, tie_pct as u64);
        assert_eq!(actions.len(), BENCH_PLACES + BENCH_CANCELS);
        let caches = seed_caches(&actions, 1_000);
        let (s_k, h_k, sorted_k, _, _) = e_path(TrustKeyArm::Keccak, &actions, &caches);
        let (s_s, h_s, sorted_s, _, _) = e_path(TrustKeyArm::Sha256, &actions, &caches);
        assert_eq!(s_s, s_k, "tie {tie_pct}%: senders");
        assert_eq!(h_s, h_k, "tie {tie_pct}%: sort hashes");
        assert_eq!(order_of(&sorted_s.0), order_of(&sorted_k.0), "tie {tie_pct}%: pre-EVM");
        assert_eq!(order_of(&sorted_s.1), order_of(&sorted_k.1), "tie {tie_pct}%: post-EVM");
        // The tie-only arm: same senders and lists (its non-tie hashes are
        // placeholders by design).
        let (s_t, _, sorted_t, _, _) = e_path(TrustKeyArm::Sha256TieOnly, &actions, &caches);
        assert_eq!(s_t, s_k, "tie {tie_pct}%: tie-only senders");
        assert_eq!(order_of(&sorted_t.0), order_of(&sorted_k.0), "tie {tie_pct}%: tie-only pre");
        assert_eq!(order_of(&sorted_t.1), order_of(&sorted_k.1), "tie {tie_pct}%: tie-only post");
        // Uncached ground truth.
        let fresh: Vec<(Address, NativeAction)> = actions
            .iter()
            .map(|a| (a.recover_sender().unwrap(), a.action.clone()))
            .collect();
        assert_eq!(s_k, fresh.iter().map(|(s, _)| Some(*s)).collect::<Vec<_>>());
        let truth = sort_native_actions_indexed(fresh);
        assert_eq!(order_of(&sorted_s.0), order_of(&truth.0), "tie {tie_pct}%: pre truth");
        assert_eq!(order_of(&sorted_s.1), order_of(&truth.1), "tie {tie_pct}%: post truth");
        // The tie share is real: same-sender places in the post-EVM list.
        let senders_post: Vec<Address> = sorted_s.1 .0.iter().map(|(s, _)| *s).collect();
        let tied = senders_post
            .iter()
            .filter(|s| senders_post.iter().filter(|t| t == s).count() > 1)
            .count();
        assert_eq!(tied, 2 * tie_pairs(tie_pct), "tie {tie_pct}%: tied places");
    }
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

/// Raw single-thread throughput of the two key hashes on 1 MiB (MB/s).
fn hash_throughput() -> (f64, f64) {
    use sha2::Digest;
    let buf = vec![0xa5u8; 1 << 20];
    let mut best = (0f64, 0f64);
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(alloy_primitives::keccak256(std::hint::black_box(&buf)));
        }
        let k = 20.0 * buf.len() as f64 / t.elapsed().as_secs_f64() / 1e6;
        let t = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(sha2::Sha256::digest(std::hint::black_box(&buf)));
        }
        let s = 20.0 * buf.len() as f64 / t.elapsed().as_secs_f64() / 1e6;
        best = (best.0.max(k), best.1.max(s));
    }
    best
}

/// Decisive µbench: verify + sort ms per bench-shaped block, keccak vs
/// sha256, ABBA rounds (K S S K K S), median of `UB_REPS` (200) reps each;
/// then the tie-only arm against keccak (K T T K K T).
/// `UB_TRUST_KEY=keccak|sha256|tieonly` runs one arm only; `UB_TIES` (default
/// `0,20,50`) picks the tie shares; `UB_FILLER` (65536) the cache size.
#[test]
#[ignore]
fn trust_key_ubench_bench_block() {
    let reps: usize = std::env::var("UB_REPS").ok().and_then(|v| v.parse().ok()).unwrap_or(200);
    let filler: usize =
        std::env::var("UB_FILLER").ok().and_then(|v| v.parse().ok()).unwrap_or(65_536);
    let ties: Vec<usize> = std::env::var("UB_TIES")
        .unwrap_or_else(|_| "0,20,50".into())
        .split(',')
        .map(|t| t.trim().parse().unwrap())
        .collect();
    let only = std::env::var("UB_TRUST_KEY").ok();
    let order: Vec<TrustKeyArm> = match only.as_deref() {
        Some("keccak") => vec![TrustKeyArm::Keccak; 3],
        Some("sha256") => vec![TrustKeyArm::Sha256; 3],
        Some("tieonly") => vec![TrustKeyArm::Sha256TieOnly; 3],
        _ => {
            use TrustKeyArm::*;
            vec![
                Keccak, Sha256, Sha256, Keccak, Keccak, Sha256, // ABBA memo
                Keccak, Sha256TieOnly, Sha256TieOnly, Keccak, Keccak, Sha256TieOnly, // ABBA tie-only
            ]
        }
    };
    let (k_mbs, s_mbs) = hash_throughput();
    println!(
        "sha256_hw_accelerated(SHA-NI cpuid)={} keccak256 {k_mbs:.0} MB/s sha256 {s_mbs:.0} MB/s (1 MiB, best of 5)",
        torus_types::sha256_hw_accelerated()
    );
    for tie_pct in ties {
        let actions = bench_block(tie_pct, 1_000 + tie_pct as u64);
        let c_bytes: usize = actions.iter().map(|a| a.action.canonical_bytes().len()).sum();
        let caches = seed_caches(&actions, filler);
        let mut per_arm: HashMap<&'static str, Vec<(f64, f64, f64)>> = HashMap::new();
        for (round, &arm) in order.iter().enumerate() {
            let mut v = Vec::with_capacity(reps);
            let mut s = Vec::with_capacity(reps);
            let mut tot = Vec::with_capacity(reps);
            for rep in 0..reps + 5 {
                let (_, _, sorted, tv, ts) = e_path(arm, &actions, &caches);
                std::hint::black_box(&sorted);
                drop(sorted);
                if rep >= 5 {
                    v.push(tv);
                    s.push(ts);
                    tot.push(tv + ts);
                }
            }
            let (mv, ms, mt) = (median(v), median(s), median(tot));
            let name = arm.name();
            println!(
                "tie={tie_pct}% round={round} arm={name}: verify {mv:.4} ms  sort {ms:.4} ms  verify+sort {mt:.4} ms (median of {reps})"
            );
            per_arm.entry(name).or_default().push((mv, ms, mt));
        }
        let summary = |name: &str| -> Option<(f64, f64, f64)> {
            let r = per_arm.get(name)?;
            Some((
                mean(&r.iter().map(|x| x.0).collect::<Vec<_>>()),
                mean(&r.iter().map(|x| x.1).collect::<Vec<_>>()),
                mean(&r.iter().map(|x| x.2).collect::<Vec<_>>()),
            ))
        };
        for other in ["sha256", "sha256-tieonly"] {
            if let (Some(k), Some(s)) = (summary("keccak"), summary(other)) {
                println!(
                    "SUMMARY tie={tie_pct}% C={c_bytes} B/blk: keccak verify+sort {:.4} ms (verify {:.4} sort {:.4}) | {other} {:.4} ms (verify {:.4} sort {:.4}) | saved {:.4} ms/blk ({:+.1}%)",
                    k.2,
                    k.0,
                    k.1,
                    s.2,
                    s.0,
                    s.1,
                    k.2 - s.2,
                    (s.2 / k.2 - 1.0) * 100.0
                );
            }
        }
    }
}
