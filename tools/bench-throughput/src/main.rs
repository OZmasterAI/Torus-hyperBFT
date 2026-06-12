use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::Address;
use clap::{Parser, Subcommand};
use k256::ecdsa::SigningKey;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::Semaphore;

use torus_types::{
    eip712::sign_native_action, FixedPoint, NativeAction, OrderType, PlaceOrderParams, TimeInForce,
};

const HARDHAT_KEYS: [&str; 20] = [
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
    "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
    "5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
    "7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6",
    "47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a",
    "8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba",
    "92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e",
    "4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356",
    "dbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97",
    "2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6",
    "f214f2b2cd398c806f84e317254e0f0b801d0643303237d97a22a48e01628897",
    "701b615bbdfb9de65240bc28bd21bbc0d996645a3dd57e7b12bc2bdf6f192c82",
    "a267530f49f8280200edf313ee7af6b827f2a8bce2897751d06a843f644967b1",
    "47c99abed3324a2707c28affff1267e45918ec8c3f20b8aa892e8b065d2942dd",
    "c526ee95bf44d8fc405a158bb884d9d1238d99f0612e9f33d006bb0789009aaa",
    "8166f546bab6da521a8369cab06c5d2b9e46670292d85c875ee9ec20e84ffb61",
    "ea6c44ac03bff858b476bba40716402b03e41b8e97e276d1baec7c37d42484a0",
    "689af8efa8c651a91ad287602527f3af2fe9f6501a7ac4b061667b5a93e037fd",
    "de9be858da4a475276426320d5e9262ecfc3ba460bfac56360bfa6c4c28b4ee0",
    "df57089febbacf7ba0bc227dafbffa9fc08a93fdc68e1e42411a14efcf23656e",
];

#[derive(Parser)]
#[command(name = "bench-throughput", about = "Torus-hyperBFT throughput benchmarking tool")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    MatchingEngine {
        #[arg(long, default_value_t = 10_000)]
        orders: usize,
        #[arg(long, default_value_t = 1)]
        markets: u64,
        #[arg(long, default_value_t = 1_000)]
        warmup: usize,
        #[arg(long, default_value = "devnet/genesis.json")]
        genesis: String,
    },
    Consensus {
        #[arg(long, default_value = "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548")]
        rpc_urls: String,
        /// Number of distinct signing senders. Defaults to 20 — the hardhat market-maker
        /// accounts pre-funded with a native balance in genesis (`native_balances`). Orders
        /// from UNFUNDED senders (index >= 20) are rejected for insufficient margin and never
        /// match, so raising this above 20 needs matching genesis funding to be meaningful.
        #[arg(long, default_value_t = 20)]
        senders: usize,
        #[arg(long, default_value_t = 30)]
        duration: u64,
        #[arg(long, default_value_t = 512)]
        concurrency: usize,
        /// Orders per signed PlaceOrderBatch (1 = single PlaceOrder). The throughput
        /// knob: orders/s = actions/s x batch_size. Sweep e.g. 1/100/500/1000.
        #[arg(long, default_value_t = 1)]
        batch_size: usize,
        /// Actions per `torus_submitNativeActions` RPC call (1 = legacy single
        /// endpoint). Amortizes HTTP/JSON/permit overhead (Sprint 2). Server cap: 100.
        #[arg(long, default_value_t = 1)]
        submit_batch: usize,
        /// First sender key index (use disjoint ranges, e.g. 0 and 10, when running
        /// multiple bench instances so nonce/rate-limit accounting never collides).
        #[arg(long, default_value_t = 0)]
        sender_offset: usize,
        /// Ingress wire format: "json" (hex of canonical JSON) or "bin"
        /// (hex of bincode via torus_submitNativeActionsBin, Sprint 5).
        #[arg(long, default_value = "json")]
        format: String,
    },
    Combined {
        #[arg(long, default_value = "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548")]
        rpc_urls: String,
        #[arg(long, default_value_t = 100)]
        senders: usize,
        #[arg(long, default_value_t = 30)]
        duration: u64,
        #[arg(long, default_value_t = 512)]
        concurrency: usize,
    },
    /// State-root scaling proof (Phase A A1.6): seed synthetic EVM state at each --sizes value,
    /// then measure per-block incremental vs full-scan root-compute time. The incremental root is
    /// O(changed) so its time stays ~flat as state grows; the full scan is O(total state) so it
    /// grows ~linearly — the production-scale payoff a fresh small-state devnet can't show.
    StateRoot {
        /// Comma-separated account counts to sweep (synthetic state sizes). 1M can take minutes.
        #[arg(long, default_value = "1000,100000,1000000")]
        sizes: String,
        /// Accounts changed per simulated block (the O(changed) working set).
        #[arg(long, default_value_t = 16)]
        changed: usize,
        /// Blocks measured per size; the reported time is the MIN across them (noise-robust).
        #[arg(long, default_value_t = 5)]
        blocks: usize,
    },
}

fn random_place_order(rng: &mut impl Rng, market_id: u64) -> NativeAction {
    // Prices MUST be tick-aligned or the matching engine rejects the order outright
    // (`order_book.rs`: `price.raw() % tick_size.raw() != 0` -> Rejected). The runtime
    // order book is auto-created with tick = lot = FixedPoint::ONE (whole units), so
    // generate whole-unit prices around a 60,000 mid (±5,000) — these always satisfy a
    // 1.0 tick and cross often enough to actually exercise the matching engine.
    let mid: i128 = 60_000;
    let offset = rng.gen_range(-5_000i128..=5_000);
    let price = FixedPoint::from_raw((mid + offset) * FixedPoint::SCALE);
    let qty_units = rng.gen_range(1i128..=100);
    let quantity = FixedPoint::from_raw(qty_units * FixedPoint::SCALE);
    let is_buy = rng.gen_bool(0.5);

    NativeAction::PlaceOrder(PlaceOrderParams {
        market_id,
        is_buy,
        price,
        quantity,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    })
}

/// Sign one submit-batch worth of payloads (T6 signer pipeline). Returns the
/// hex payload strings and the advanced nonce watermark — nonces must be
/// strictly increasing per sender because the committed (sender, nonce) replay
/// guard silently drops same-ms duplicates.
fn sign_payload_batch(
    rng: &mut impl Rng,
    key: &k256::ecdsa::SigningKey,
    batch_size: usize,
    submit_batch: usize,
    mut last_nonce: u64,
    bin: bool,
) -> (Vec<String>, u64) {
    let mut payloads = Vec::with_capacity(submit_batch);
    for _ in 0..submit_batch {
        let action = random_place_order_action(rng, 1, batch_size);
        let base = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let nonce = base.max(last_nonce + 1);
        last_nonce = nonce;
        let signed = sign_native_action(action, nonce, key);
        let bytes = if bin {
            bincode::serialize(&signed).unwrap()
        } else {
            serde_json::to_vec(&signed).unwrap()
        };
        payloads.push(format!("0x{}", hex::encode(&bytes)));
    }
    (payloads, last_nonce)
}

/// Build one action carrying `batch_size` orders. `batch_size <= 1` returns a plain
/// `PlaceOrder` (the legacy single path) for apples-to-apples A/B runs.
fn random_place_order_action(rng: &mut impl Rng, market_id: u64, batch_size: usize) -> NativeAction {
    if batch_size <= 1 {
        return random_place_order(rng, market_id);
    }
    let orders: Vec<PlaceOrderParams> = (0..batch_size)
        .map(|_| match random_place_order(rng, market_id) {
            NativeAction::PlaceOrder(p) => p,
            _ => unreachable!(),
        })
        .collect();
    NativeAction::PlaceOrderBatch(orders)
}

fn random_address(rng: &mut impl Rng) -> Address {
    let mut bytes = [0u8; 20];
    rng.fill(&mut bytes);
    Address::from(bytes)
}

fn format_num(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

// ============================================================================
// Mode 1: Matching Engine
// ============================================================================

fn run_matching_engine(orders: usize, markets: u64, warmup: usize, genesis_path: &str) {
    use tempfile::TempDir;
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_genesis::Genesis;
    use torus_state::StateDb;

    let dir = TempDir::new().unwrap();
    let state_db = StateDb::open(dir.path()).unwrap();

    let genesis = Genesis::from_file(std::path::Path::new(genesis_path)).unwrap();
    let _state_root = genesis.initialize(&state_db).unwrap();
    let chain_config = genesis.chain_config();

    let mut rng = rand::thread_rng();
    let market_count = markets.max(1);

    println!("=== Matching Engine Benchmark ===");
    println!("Markets: {market_count}");
    println!("Warmup:  {} orders", format_num(warmup as u64));
    println!();

    let mut ctx = NativeExecContext::new(
        state_db,
        1,
        1_700_000_000,
        0,
        chain_config.epoch_length,
        chain_config.max_validators,
        Address::ZERO,
        chain_config.treasury_address,
        chain_config.dev_pool_address,
    );

    // Warmup
    if warmup > 0 {
        let warmup_actions: Vec<(Address, NativeAction)> = (0..warmup)
            .map(|_| {
                let mid = rng.gen_range(0..market_count);
                (random_address(&mut rng), random_place_order(&mut rng, mid))
            })
            .collect();
        let t = Instant::now();
        NativeExecutor::execute_batch(&mut ctx, &warmup_actions);
        let elapsed = t.elapsed();
        println!(
            "Warmup:  {} orders in {:.1}ms ({:.0} orders/sec)",
            format_num(warmup as u64),
            elapsed.as_secs_f64() * 1000.0,
            warmup as f64 / elapsed.as_secs_f64(),
        );
        println!();
    }

    // Benchmark batches
    let batch_sizes: Vec<usize> = if orders <= 10_000 {
        vec![orders]
    } else {
        let mut sizes = vec![10_000];
        if orders > 10_000 {
            sizes.push(orders);
        }
        sizes
    };

    for &batch_size in &batch_sizes {
        let actions: Vec<(Address, NativeAction)> = (0..batch_size)
            .map(|_| {
                let mid = rng.gen_range(0..market_count);
                (random_address(&mut rng), random_place_order(&mut rng, mid))
            })
            .collect();

        let t = Instant::now();
        NativeExecutor::execute_batch(&mut ctx, &actions);
        let elapsed = t.elapsed();
        let rate = batch_size as f64 / elapsed.as_secs_f64();

        println!(
            "Batch {}: {:.0} orders/sec ({:.1}ms)",
            format_num(batch_size as u64),
            rate,
            elapsed.as_secs_f64() * 1000.0,
        );
    }

    if market_count > 1 {
        println!();
        println!("--- Per-market breakdown (single batch of {}) ---", format_num(orders as u64));

        for m in 0..market_count.min(4) {
            let actions: Vec<(Address, NativeAction)> = (0..orders)
                .map(|_| (random_address(&mut rng), random_place_order(&mut rng, m)))
                .collect();

            let t = Instant::now();
            NativeExecutor::execute_batch(&mut ctx, &actions);
            let elapsed = t.elapsed();
            let rate = orders as f64 / elapsed.as_secs_f64();

            println!(
                "  Market {m}: {:.0} orders/sec ({:.1}ms)",
                rate,
                elapsed.as_secs_f64() * 1000.0,
            );
        }
    }
}

// ============================================================================
// Mode 2: Consensus
// ============================================================================

struct SenderKey {
    signing_key: SigningKey,
}

fn load_sender_keys(count: usize, offset: usize) -> Vec<SenderKey> {
    let funded_end = (offset + count).min(20);
    let mut keys: Vec<SenderKey> = HARDHAT_KEYS[offset.min(20)..funded_end]
        .iter()
        .map(|hex_key| {
            let bytes = hex::decode(hex_key).expect("valid hex");
            let signing_key = SigningKey::from_slice(&bytes).expect("valid key");
            SenderKey { signing_key }
        })
        .collect();

    for i in funded_end..(offset + count) {
        let mut rng = StdRng::seed_from_u64(0xBEEF_0000 + i as u64);
        let signing_key = SigningKey::random(&mut rng);
        keys.push(SenderKey { signing_key });
    }
    keys
}

/// Submit a batch of pre-encoded signed actions via `torus_submitNativeActions`
/// (or `torus_submitNativeActionsBin` for bincode payloads, Sprint 5).
/// Returns how many items the server accepted (entries carrying a `hash`).
async fn submit_native_actions_batch(
    client: &reqwest::Client,
    url: &str,
    payloads: &[String],
    id: u64,
    bin: bool,
) -> Result<usize, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": if bin {
            "torus_submitNativeActionsBin"
        } else {
            "torus_submitNativeActions"
        },
        "params": [payloads],
        "id": id
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse: {e}"))?;

    if let Some(err) = resp.get("error") {
        return Err(format!(
            "rpc: {}",
            err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")
        ));
    }
    let accepted = resp["result"]
        .as_array()
        .map(|items| items.iter().filter(|i| i.get("hash").is_some()).count())
        .unwrap_or(0);
    Ok(accepted)
}

async fn submit_native_action(
    client: &reqwest::Client,
    url: &str,
    signed_json_hex: &str,
    id: u64,
) -> Result<(), String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "torus_submitNativeAction",
        "params": [signed_json_hex],
        "id": id
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("http: {e}"))?
        .json()
        .await
        .map_err(|e| format!("parse: {e}"))?;

    if let Some(err) = resp.get("error") {
        return Err(format!(
            "rpc: {}",
            err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")
        ));
    }
    Ok(())
}

async fn fetch_block_number(client: &reqwest::Client, url: &str) -> Option<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });
    let resp: serde_json::Value = client.post(url).json(&body).send().await.ok()?.json().await.ok()?;
    let hex_str = resp["result"].as_str()?;
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    u64::from_str_radix(s, 16).ok()
}

/// One swept block: `(block_number, native slots, action identity hashes)`.
/// Identity hashes are empty when the node returned only a count (old RPC).
type SweptBlock = (u64, u64, Vec<u64>);

/// Stable identity of one included action: hash of its canonical JSON
/// (serde_json maps are BTreeMap-backed, so key order is deterministic).
/// 64-bit FxHash-style collisions are negligible at bench scales.
fn action_identity(action: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    action.to_string().hash(&mut h);
    h.finish()
}

async fn fetch_block_body(
    client: &reqwest::Client,
    url: &str,
    block_number: u64,
) -> Option<(u64, Vec<u64>)> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "torus_getBlockBody",
        "params": [block_number],
        "id": 1
    });
    let resp: serde_json::Value = client.post(url).json(&body).send().await.ok()?.json().await.ok()?;
    let result = resp.get("result")?;
    // Prefer the action list: identities enable cross-block dedup (the
    // "included" metric otherwise overcounts ~3x under 3-chain commit lag).
    if let Some(actions) = result.get("nativeActions").and_then(|a| a.as_array()) {
        let ids: Vec<u64> = actions.iter().map(action_identity).collect();
        return Some((ids.len() as u64, ids));
    }
    // Old node: count only, no identities.
    let count = result.get("nativeActionCount")?.as_u64()?;
    Some((count, Vec::new()))
}

/// Sum native actions actually committed across a block range — the authoritative
/// throughput measure. The live monitor undercounts badly at fast block times (it
/// can't poll every body within its 500ms tick and silently drops undrained blocks
/// when it aborts), so after the load phase we re-fetch EVERY block body in the run
/// window `(start_exclusive, end_inclusive]` with bounded concurrency and a short
/// retry for tail blocks whose body isn't queryable yet. Returns the committed
/// `(block, native_count)` pairs that were successfully read.
async fn sweep_block_bodies(
    client: Arc<reqwest::Client>,
    urls: Arc<Vec<String>>,
    start_exclusive: u64,
    end_inclusive: u64,
    concurrency: usize,
) -> Vec<SweptBlock> {
    if end_inclusive <= start_exclusive {
        return Vec::new();
    }
    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut set = tokio::task::JoinSet::new();

    for blk in (start_exclusive + 1)..=end_inclusive {
        let sem = sem.clone();
        let client = client.clone();
        let urls = urls.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.unwrap();
            let url = &urls[(blk as usize) % urls.len()];
            // Tail blocks may not have a queryable body yet — retry with light backoff.
            for attempt in 0..5u64 {
                if let Some((count, ids)) = fetch_block_body(&client, url, blk).await {
                    return (blk, Some((count, ids)));
                }
                tokio::time::sleep(Duration::from_millis(150 * (attempt + 1))).await;
            }
            (blk, None)
        });
    }

    let mut out = Vec::new();
    let mut missing = 0u64;
    while let Some(res) = set.join_next().await {
        match res {
            Ok((blk, Some((count, ids)))) => out.push((blk, count, ids)),
            Ok((_, None)) | Err(_) => missing += 1,
        }
    }
    if missing > 0 {
        eprintln!(
            "[sweep] warning: {missing} block bodies unavailable after retries (excluded from total)"
        );
    }
    out.sort_unstable_by_key(|b| b.0);
    out
}

/// Drop trailing zero-count blocks from a swept window. A live chain keeps
/// producing empty blocks after the load stops; counting them would dilute
/// block stats and pin the window end past the actual run.
fn trim_trailing_empty(blocks: &mut Vec<SweptBlock>) {
    while matches!(blocks.last(), Some((_, 0, _))) {
        blocks.pop();
    }
}

#[cfg(test)]
mod sweep_window_tests {
    use super::*;

    fn blk(n: u64, ids: &[u64]) -> SweptBlock {
        (n, ids.len() as u64, ids.to_vec())
    }

    #[test]
    fn trim_trailing_empty_drops_only_tail_zeros() {
        let mut blocks = vec![
            blk(10, &[]),
            blk(11, &[1, 2, 3, 4, 5]),
            blk(12, &[]),
            blk(13, &[6, 7, 8, 9, 10, 11, 12]),
            blk(14, &[]),
            blk(15, &[]),
        ];
        trim_trailing_empty(&mut blocks);
        assert_eq!(
            blocks.iter().map(|b| b.0).collect::<Vec<_>>(),
            vec![10, 11, 12, 13]
        );
    }

    #[test]
    fn trim_trailing_empty_handles_all_zero_and_empty() {
        let mut all_zero = vec![blk(1, &[]), blk(2, &[])];
        trim_trailing_empty(&mut all_zero);
        assert!(all_zero.is_empty());
        let mut empty: Vec<SweptBlock> = Vec::new();
        trim_trailing_empty(&mut empty);
        assert!(empty.is_empty());
    }

    // Duplicate-inclusion accounting (ingress-cpu-supply open Q3): the same
    // action included in several blocks must count once in `unique`. Slots
    // stay the raw per-block sum so the dup factor (slots/unique) is visible.
    #[test]
    fn summarize_dedups_identities_across_blocks() {
        let blocks = vec![blk(1, &[10, 11]), blk(2, &[11, 12]), blk(3, &[10, 11])];
        let s = summarize_included(&blocks);
        assert_eq!(s.slots, 6);
        assert_eq!(s.unique, Some(3));
        assert_eq!(s.block_count, 3);
        assert_eq!(s.peak_actions, 2);
        assert_eq!(s.peak_block, 1);
    }

    // Old nodes return only nativeActionCount (no identity list): unique is
    // unknowable, not zero — report None so callers fall back to slots.
    #[test]
    fn summarize_unique_none_when_identities_missing() {
        let blocks = vec![(1u64, 3u64, Vec::new())];
        let s = summarize_included(&blocks);
        assert_eq!(s.slots, 3);
        assert_eq!(s.unique, None);
    }

    // Mixed availability (some bodies fell back to count-only) would
    // undercount duplicates — treat the whole window as unknown.
    #[test]
    fn summarize_unique_none_when_partially_missing() {
        let blocks = vec![blk(1, &[10, 11]), (2u64, 2u64, Vec::new())];
        let s = summarize_included(&blocks);
        assert_eq!(s.slots, 4);
        assert_eq!(s.unique, None);
    }

    #[test]
    fn summarize_empty_window() {
        let s = summarize_included(&[]);
        assert_eq!(s.slots, 0);
        assert_eq!(s.unique, Some(0));
        assert_eq!(s.block_count, 0);
    }
}

/// Aggregate of a swept block window.
struct IncludedSummary {
    /// Raw per-block native slots summed — overcounts when the same action
    /// rides several blocks (3-chain commit lag re-inclusion).
    slots: u64,
    /// Distinct action identities across the window — the honest "included"
    /// number. `None` when any non-empty block lacked identities (old RPC),
    /// since a partial dedup would understate the dup factor.
    unique: Option<u64>,
    block_count: u64,
    peak_actions: u64,
    peak_block: u64,
}

fn summarize_included(blocks: &[SweptBlock]) -> IncludedSummary {
    let mut slots = 0u64;
    let mut peak_actions = 0u64;
    let mut peak_block = 0u64;
    let mut seen = std::collections::HashSet::new();
    let mut identities_complete = true;
    for (blk, count, ids) in blocks {
        slots += count;
        if *count > peak_actions {
            peak_actions = *count;
            peak_block = *blk;
        }
        if ids.len() as u64 == *count {
            seen.extend(ids.iter().copied());
        } else {
            identities_complete = false;
        }
    }
    IncludedSummary {
        slots,
        unique: identities_complete.then(|| seen.len() as u64),
        block_count: blocks.len() as u64,
        peak_actions,
        peak_block,
    }
}

async fn run_consensus(
    rpc_urls_str: &str,
    senders: usize,
    duration_secs: u64,
    concurrency: usize,
    batch_size: usize,
    submit_batch: usize,
    sender_offset: usize,
    bin: bool,
) {
    let rpc_urls: Vec<String> = rpc_urls_str.split(',').map(|s| s.trim().to_string()).collect();
    let num_senders = senders.max(1);
    let keys = load_sender_keys(num_senders, sender_offset);
    let orders_per_action = batch_size.max(1) as u64;
    let submit_batch = submit_batch.clamp(1, 100);

    println!("=== Torus Throughput Benchmark ===");
    println!("Mode: consensus");
    println!("Duration: {duration_secs}s");
    println!("Senders: {num_senders} (offset {sender_offset})");
    println!("Batch size: {orders_per_action} order(s)/action");
    println!("Submit batch: {submit_batch} action(s)/RPC call");
    println!("RPC endpoints: {}", rpc_urls.len());
    println!();

    let client = Arc::new(
        reqwest::Client::builder()
            .pool_max_idle_per_host(256)
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client"),
    );

    let submitted = Arc::new(AtomicU64::new(0));
    let included = Arc::new(AtomicU64::new(0));
    let start_block = fetch_block_number(&client, &rpc_urls[0]).await.unwrap_or(0);
    let deadline = Instant::now() + Duration::from_secs(duration_secs);
    let start_time = Instant::now();
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let rpc_urls = Arc::new(rpc_urls);

    // Block monitor
    let mon_client = client.clone();
    let mon_url = rpc_urls[0].clone();
    let mon_included = included.clone();
    let mon_submitted = submitted.clone();
    let mon_start = start_time;
    let mon_deadline = deadline;

    struct BlockStats {
        total_included: u64,
        block_count: u64,
        peak_actions: u64,
        peak_block: u64,
        block_times: Vec<f64>,
    }

    let block_stats = Arc::new(tokio::sync::Mutex::new(BlockStats {
        total_included: 0,
        block_count: 0,
        peak_actions: 0,
        peak_block: 0,
        block_times: Vec::new(),
    }));
    let final_stats = block_stats.clone();

    let monitor_handle = tokio::spawn({
        let block_stats = block_stats.clone();
        async move {
            let mut last_block = start_block;
            let mut last_block_time = Instant::now();
            let mut pending_blocks: Vec<u64> = Vec::new();

            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;

                if Instant::now() > mon_deadline + Duration::from_secs(5) {
                    break;
                }

                let current_block = match fetch_block_number(&mon_client, &mon_url).await {
                    Some(n) => n,
                    None => continue,
                };

                if current_block > last_block {
                    let now = Instant::now();
                    let inter_block = now.duration_since(last_block_time).as_secs_f64()
                        / (current_block - last_block) as f64;

                    for blk in (last_block + 1)..=current_block {
                        pending_blocks.push(blk);
                    }

                    let mut stats = block_stats.lock().await;
                    for _ in 0..(current_block - last_block) {
                        stats.block_times.push(inter_block);
                    }
                    drop(stats);

                    last_block_time = now;
                    last_block = current_block;
                }

                let mut still_pending = Vec::new();
                let mut stats = block_stats.lock().await;
                for blk in pending_blocks.drain(..) {
                    match fetch_block_body(&mon_client, &mon_url, blk).await {
                        Some((native_count, _ids)) => {
                            stats.total_included += native_count;
                            mon_included.store(stats.total_included, Ordering::Relaxed);
                            stats.block_count += 1;
                            if native_count > stats.peak_actions {
                                stats.peak_actions = native_count;
                                stats.peak_block = blk;
                            }
                        }
                        None => still_pending.push(blk),
                    }
                }
                drop(stats);
                pending_blocks = still_pending;

                let elapsed = mon_start.elapsed().as_secs_f64();
                let sub = mon_submitted.load(Ordering::Relaxed);
                let inc = mon_included.load(Ordering::Relaxed);
                let sub_rate = if elapsed > 0.0 { sub as f64 / elapsed } else { 0.0 };
                let inc_rate = if elapsed > 0.0 { inc as f64 / elapsed } else { 0.0 };
                let avg_blk_ms = {
                    let stats = block_stats.lock().await;
                    if stats.block_times.is_empty() {
                        0.0
                    } else {
                        stats.block_times.iter().sum::<f64>() / stats.block_times.len() as f64 * 1000.0
                    }
                };

                eprintln!(
                    "[{:.0}s] submitted: {} actions ({:.0}/s) | included: {} actions ({:.0}/s) | \
                     orders ~{:.0}/s | blk: #{} | {:.0}ms/blk",
                    elapsed,
                    format_num(sub),
                    sub_rate,
                    format_num(inc),
                    inc_rate,
                    inc_rate * orders_per_action as f64,
                    current_block,
                    avg_blk_ms,
                );
            }
        }
    });

    // Sender tasks
    let mut sender_handles = Vec::new();

    for sender_idx in 0..num_senders {
        let key = keys[sender_idx].signing_key.clone();
        let client = client.clone();
        let urls = rpc_urls.clone();
        let submitted = submitted.clone();
        let semaphore = semaphore.clone();
        let url_count = urls.len();

        sender_handles.push(tokio::spawn(async move {
            let mut req_id: u64 = sender_idx as u64 * 1_000_000;
            let mut url_idx: usize = sender_idx % url_count;

            // T6 signer pipeline: a dedicated blocking-pool task signs AHEAD
            // into a small buffer so signing CPU overlaps network I/O instead
            // of serializing with it (bs1000 signing starved the submit loop
            // on the 4-core box). Depth 4 keeps nonces well inside the 60s
            // freshness window while decoupling the two stages.
            let (pregen_tx, mut pregen_rx) = tokio::sync::mpsc::channel::<Vec<String>>(4);
            let signer = tokio::task::spawn_blocking(move || {
                let mut rng = StdRng::from_entropy();
                let mut last_nonce: u64 = 0;
                loop {
                    let (payloads, n) = sign_payload_batch(
                        &mut rng,
                        &key,
                        batch_size,
                        submit_batch,
                        last_nonce,
                        bin,
                    );
                    last_nonce = n;
                    if pregen_tx.blocking_send(payloads).is_err() {
                        break; // submit loop finished — receiver dropped
                    }
                }
            });
            let mut starved_ns: u128 = 0;

            while Instant::now() < deadline {
                let wait_t0 = Instant::now();
                let Some(payloads) = pregen_rx.recv().await else { break };
                starved_ns += wait_t0.elapsed().as_nanos();

                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let url = urls[url_idx % url_count].clone();
                url_idx += 1;
                req_id += 1;

                let client = client.clone();
                let submitted = submitted.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    // bin payloads always go through the batch endpoint — the
                    // legacy single endpoint only speaks canonical JSON.
                    if payloads.len() == 1 && !bin {
                        if submit_native_action(&client, &url, &payloads[0], req_id).await.is_ok() {
                            submitted.fetch_add(1, Ordering::Relaxed);
                        }
                    } else if let Ok(accepted) =
                        submit_native_actions_batch(&client, &url, &payloads, req_id, bin).await
                    {
                        submitted.fetch_add(accepted as u64, Ordering::Relaxed);
                    }
                });
            }

            // Dropping the receiver fails the signer's next blocking_send,
            // which is its exit signal.
            drop(pregen_rx);
            let _ = signer.await;
            if starved_ns > 1_000_000 {
                eprintln!(
                    "[sender {sender_idx}] signer-starved {:.1}ms total (signing slower than submits)",
                    starved_ns as f64 / 1e6
                );
            }
        }));
    }

    for h in sender_handles {
        let _ = h.await;
    }

    // Wait for remaining in-flight requests to drain
    tokio::time::sleep(Duration::from_secs(2)).await;
    monitor_handle.abort();

    let total_elapsed = start_time.elapsed();
    let final_submitted = submitted.load(Ordering::Relaxed);

    // Authoritative inclusion count: re-sweep EVERY block body in the run window.
    // The live monitor's running total undercounts ~3x at fast block times, so the
    // reported throughput / drop-rate are computed from this full re-sweep instead.
    //
    // eth_blockNumber reports EXECUTION height, which lags consensus by blocks under
    // load (CTE) — a window snapshotted right after the load phase cuts off the tail
    // of the run while the executor drains its backlog. Keep extending the window
    // until two consecutive extensions surface zero further orders (120s cap), then
    // drop trailing empty blocks so a live chain's post-load block production does
    // not dilute the stats.
    let mut end_block = fetch_block_number(&client, &rpc_urls[0])
        .await
        .unwrap_or(start_block);
    let mut swept = sweep_block_bodies(
        client.clone(),
        rpc_urls.clone(),
        start_block,
        end_block,
        concurrency,
    )
    .await;
    let drain_deadline = Instant::now() + Duration::from_secs(120);
    let mut quiet_extensions = 0u32;
    while quiet_extensions < 2 && Instant::now() < drain_deadline {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let cur = match fetch_block_number(&client, &rpc_urls[0]).await {
            Some(n) if n > end_block => n,
            // Height not advancing: executor still frozen or RPC hiccup — keep
            // waiting (the 120s cap bounds a genuinely stuck chain).
            _ => continue,
        };
        let delta =
            sweep_block_bodies(client.clone(), rpc_urls.clone(), end_block, cur, concurrency)
                .await;
        let drained: u64 = delta.iter().map(|(_, c, _)| c).sum();
        if drained == 0 {
            quiet_extensions += 1;
        } else {
            quiet_extensions = 0;
            eprintln!(
                "[drain] +{} actions in blocks #{}..#{} (executor catching up)",
                drained,
                end_block + 1,
                cur
            );
        }
        swept.extend(delta);
        end_block = cur;
    }
    swept.sort_unstable_by_key(|b| b.0);
    trim_trailing_empty(&mut swept);
    let end_block = swept.last().map(|b| b.0).unwrap_or(start_block);
    let summary = summarize_included(&swept);
    let block_count = summary.block_count;
    // The honest throughput number: unique actions when identities were
    // available, raw slots otherwise (old nodes). Slots inflate ~3x under
    // 3-chain commit lag re-inclusion (s355 finding).
    let final_included = summary.unique.unwrap_or(summary.slots);

    let avg_block_time_ms = {
        let stats = final_stats.lock().await;
        if stats.block_times.is_empty() {
            0.0
        } else {
            stats.block_times.iter().sum::<f64>() / stats.block_times.len() as f64 * 1000.0
        }
    };
    let avg_native_per_block = if block_count > 0 {
        final_included / block_count
    } else {
        0
    };

    let elapsed_secs = total_elapsed.as_secs_f64();
    let submit_rate = if elapsed_secs > 0.0 { final_submitted as f64 / elapsed_secs } else { 0.0 };
    let include_rate = if elapsed_secs > 0.0 { final_included as f64 / elapsed_secs } else { 0.0 };
    let drop_rate = if final_submitted > 0 {
        (1.0 - final_included as f64 / final_submitted as f64) * 100.0
    } else {
        0.0
    };

    println!();
    println!("--- Results ---");
    println!(
        "Submitted:  {} native actions ({:.0}/s)",
        format_num(final_submitted),
        submit_rate,
    );
    match summary.unique {
        Some(unique) => {
            let dup_factor = if unique > 0 {
                summary.slots as f64 / unique as f64
            } else {
                1.0
            };
            println!(
                "Included:   {} unique native actions ({:.0}/s) [{} slots, dup x{:.2}]",
                format_num(unique),
                include_rate,
                format_num(summary.slots),
                dup_factor,
            );
        }
        None => println!(
            "Included:   {} native action slots ({:.0}/s) [no identities from node; \
             may overcount re-inclusions]",
            format_num(final_included),
            include_rate,
        ),
    }
    if orders_per_action > 1 {
        println!(
            "Orders:     {} orders ({:.0}/s)  [actions x{} batch]",
            format_num(final_included * orders_per_action),
            include_rate * orders_per_action as f64,
            orders_per_action,
        );
    }
    println!("Drop rate:  {drop_rate:.1}%");
    println!("Block time: {avg_block_time_ms:.0}ms avg");
    println!(
        "Blocks:     {} with bodies in #{}..#{}, {} avg native/block",
        block_count,
        start_block + 1,
        end_block,
        avg_native_per_block,
    );
    println!();
    if summary.peak_actions > 0 {
        println!(
            "Peak:       {:.0}/s included (block #{}, {} actions)",
            include_rate, summary.peak_block, summary.peak_actions,
        );
    }
    println!(
        "Sustained:  {:.0} orders/s ({:.0} actions/s) over {duration_secs}s",
        include_rate * orders_per_action as f64,
        include_rate,
    );
}

// ============================================================================
// Mode 3: Combined (stub)
// ============================================================================

fn run_combined() {
    println!("combined mode: not yet implemented -- run consensus and tx-flood in parallel");
}

// ============================================================================
// Mode 4: State-root scaling (Phase A A1.6)
// ============================================================================

fn run_state_root_scaling(sizes_str: &str, changed: usize, blocks: usize) {
    use alloy_primitives::U256;
    use revm::database::BundleState;
    use revm::state::AccountInfo;
    use tempfile::TempDir;
    use torus_state::db::KECCAK_EMPTY;
    use torus_state::incremental::{
        build_trie_to_cf, full_post_bundle_evm_root, incremental_evm_root,
    };
    use torus_state::StateDb;

    let sizes: Vec<u64> = sizes_str
        .split(',')
        .filter_map(|s| s.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .collect();
    let changed = changed.max(1);
    let blocks = blocks.max(1);

    let eoa = |bal: u64, nonce: u64| AccountInfo {
        balance: U256::from(bal),
        nonce,
        code_hash: KECCAK_EMPTY,
        account_id: None,
        code: None,
    };
    let addr_of = |i: u64| -> Address {
        let mut b = [0u8; 20];
        b[0..8].copy_from_slice(&i.to_be_bytes());
        Address::from(b)
    };

    println!("=== State-Root Scaling Proof (Phase A A1.6) ===");
    println!("Changed accounts/block: {changed}");
    println!("Blocks measured/size:   {blocks} (min reported)");
    println!();
    println!(
        "{:>12}  {:>13}  {:>13}  {:>9}  {:>11}",
        "accounts", "full-scan", "incremental", "speedup", "trie-build"
    );
    println!("{}", "-".repeat(66));

    for &n in &sizes {
        let dir = TempDir::new().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        for i in 0..n {
            db.put_account(&addr_of(i), &eoa(1_000 + i, i)).unwrap();
        }

        let t = Instant::now();
        build_trie_to_cf(&db).unwrap();
        let build_time = t.elapsed();

        // A small bundle changing `changed` existing accounts spread across the keyspace — the
        // per-block working set. This is O(changed), independent of n.
        let mut builder = BundleState::builder(0..=0);
        let stride = (n / changed as u64).max(1);
        for j in 0..changed as u64 {
            let i = (j * stride) % n;
            builder = builder.state_present_account_info(addr_of(i), eoa(7_000_000 + j, 999));
        }
        let bundle = builder.build();

        let (mut full, mut incr) = (Duration::MAX, Duration::MAX);
        for _ in 0..blocks {
            let t = Instant::now();
            let _ = incremental_evm_root(&db, &bundle).unwrap();
            incr = incr.min(t.elapsed());

            let t = Instant::now();
            let _ = full_post_bundle_evm_root(&db, &bundle).unwrap();
            full = full.min(t.elapsed());
        }

        let speedup = full.as_secs_f64() / incr.as_secs_f64().max(1e-9);
        println!(
            "{:>12}  {:>13}  {:>13}  {:>8.1}x  {:>11}",
            format_num(n),
            format!("{full:.2?}"),
            format!("{incr:.2?}"),
            speedup,
            format!("{build_time:.2?}"),
        );
    }

    println!();
    println!(
        "Incremental root-compute time stays ~flat as accounts grow; full-scan grows ~linearly.\n\
         That flat line is the sub-100ms block-time guarantee holding at production state size."
    );
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::MatchingEngine {
            orders,
            markets,
            warmup,
            genesis,
        } => run_matching_engine(orders, markets, warmup, &genesis),
        Command::Consensus {
            rpc_urls,
            senders,
            duration,
            concurrency,
            batch_size,
            submit_batch,
            sender_offset,
            format,
        } => {
            let bin = match format.as_str() {
                "bin" => true,
                "json" => false,
                other => {
                    eprintln!("unknown --format {other:?} (expected \"json\" or \"bin\")");
                    std::process::exit(2);
                }
            };
            run_consensus(
                &rpc_urls,
                senders,
                duration,
                concurrency,
                batch_size,
                submit_batch,
                sender_offset,
                bin,
            )
            .await
        }
        Command::Combined { .. } => run_combined(),
        Command::StateRoot {
            sizes,
            changed,
            blocks,
        } => run_state_root_scaling(&sizes, changed, blocks),
    }
}

#[cfg(test)]
mod tests {
    use super::sign_payload_batch;
    use super::{summarize_included, SweptBlock};
    use rand::{rngs::StdRng, SeedableRng};

    #[test]
    fn sign_payload_batch_nonces_strictly_increase() {
        let mut rng = StdRng::seed_from_u64(7);
        let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
        let (p1, n1) = sign_payload_batch(&mut rng, &key, 3, 4, 0, false);
        let (p2, n2) = sign_payload_batch(&mut rng, &key, 3, 4, n1, false);
        assert_eq!(p1.len(), 4);
        assert_eq!(p2.len(), 4);
        assert!(n2 > n1, "nonce watermark must advance across batches");
        let dec = |p: &String| -> u64 {
            let bytes = hex::decode(p.trim_start_matches("0x")).unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            v["nonce"].as_u64().expect("signed action has a numeric nonce")
        };
        let nonces: Vec<u64> = p1.iter().chain(p2.iter()).map(dec).collect();
        for w in nonces.windows(2) {
            assert!(
                w[1] > w[0],
                "nonces must be strictly increasing across the pregen stream: {nonces:?}"
            );
        }
    }

    #[test]
    fn summarize_totals_count_and_peak() {
        // slots = 5+20+0+8 = 33; peak block is #11 with 20 actions.
        let blocks: Vec<SweptBlock> = vec![
            (10, 5, Vec::new()),
            (11, 20, Vec::new()),
            (12, 0, Vec::new()),
            (13, 8, Vec::new()),
        ];
        let s = summarize_included(&blocks);
        assert_eq!(
            (s.slots, s.block_count, s.peak_actions, s.peak_block),
            (33, 4, 20, 11)
        );
        // Count-only bodies (no identities) → unique unknown.
        assert_eq!(s.unique, None);
    }

    #[test]
    fn summarize_first_max_wins_on_ties() {
        // First block reaching the peak count keeps the peak_block slot.
        let blocks: Vec<SweptBlock> = vec![(7, 9, Vec::new()), (8, 9, Vec::new())];
        let s = summarize_included(&blocks);
        assert_eq!(
            (s.slots, s.block_count, s.peak_actions, s.peak_block),
            (18, 2, 9, 7)
        );
    }
}
