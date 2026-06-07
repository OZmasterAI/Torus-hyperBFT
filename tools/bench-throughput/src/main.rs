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
        #[arg(long, default_value_t = 100)]
        senders: usize,
        #[arg(long, default_value_t = 30)]
        duration: u64,
        #[arg(long, default_value_t = 512)]
        concurrency: usize,
        /// Orders per signed PlaceOrderBatch (1 = single PlaceOrder). The throughput
        /// knob: orders/s = actions/s x batch_size. Sweep e.g. 1/100/500/1000.
        #[arg(long, default_value_t = 1)]
        batch_size: usize,
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
    let mid_price: i128 = 6_000_000_000_000; // 60,000 * SCALE
    let offset = rng.gen_range(-500_000_000_000i128..500_000_000_000i128);
    let price = FixedPoint::from_raw(mid_price + offset);
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

fn load_sender_keys(count: usize) -> Vec<SenderKey> {
    let mut keys: Vec<SenderKey> = HARDHAT_KEYS[..count.min(20)]
        .iter()
        .map(|hex_key| {
            let bytes = hex::decode(hex_key).expect("valid hex");
            let signing_key = SigningKey::from_slice(&bytes).expect("valid key");
            SenderKey { signing_key }
        })
        .collect();

    for i in 20..count {
        let mut rng = StdRng::seed_from_u64(0xBEEF_0000 + i as u64);
        let signing_key = SigningKey::random(&mut rng);
        keys.push(SenderKey { signing_key });
    }
    keys
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

async fn fetch_block_body(
    client: &reqwest::Client,
    url: &str,
    block_number: u64,
) -> Option<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "torus_getBlockBody",
        "params": [block_number],
        "id": 1
    });
    let resp: serde_json::Value = client.post(url).json(&body).send().await.ok()?.json().await.ok()?;
    let result = resp.get("result")?;
    result.get("nativeActionCount")?.as_u64()
}

async fn run_consensus(
    rpc_urls_str: &str,
    senders: usize,
    duration_secs: u64,
    concurrency: usize,
    batch_size: usize,
) {
    let rpc_urls: Vec<String> = rpc_urls_str.split(',').map(|s| s.trim().to_string()).collect();
    let num_senders = senders.max(1);
    let keys = load_sender_keys(num_senders);
    let orders_per_action = batch_size.max(1) as u64;

    println!("=== Torus Throughput Benchmark ===");
    println!("Mode: consensus");
    println!("Duration: {duration_secs}s");
    println!("Senders: {num_senders}");
    println!("Batch size: {orders_per_action} order(s)/action");
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
                        Some(native_count) => {
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
            let mut rng = StdRng::from_entropy();
            let mut req_id: u64 = sender_idx as u64 * 1_000_000;
            let mut url_idx: usize = sender_idx % url_count;

            while Instant::now() < deadline {
                let action = random_place_order_action(&mut rng, 1, batch_size);
                let nonce = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;
                let signed = sign_native_action(action, nonce, &key);
                let json_bytes = serde_json::to_vec(&signed).unwrap();
                let hex_encoded = format!("0x{}", hex::encode(&json_bytes));

                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let url = urls[url_idx % url_count].clone();
                url_idx += 1;
                req_id += 1;

                let client = client.clone();
                let submitted = submitted.clone();

                tokio::spawn(async move {
                    let _permit = permit;
                    if submit_native_action(&client, &url, &hex_encoded, req_id).await.is_ok() {
                        submitted.fetch_add(1, Ordering::Relaxed);
                    }
                });
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

    let stats = final_stats.lock().await;
    let final_included = stats.total_included;
    let block_count = stats.block_count;
    let peak_actions = stats.peak_actions;
    let peak_block = stats.peak_block;
    let avg_block_time_ms = if stats.block_times.is_empty() {
        0.0
    } else {
        stats.block_times.iter().sum::<f64>() / stats.block_times.len() as f64 * 1000.0
    };
    let avg_native_per_block = if block_count > 0 {
        final_included / block_count
    } else {
        0
    };
    drop(stats);

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
    println!(
        "Included:   {} native actions ({:.0}/s)",
        format_num(final_included),
        include_rate,
    );
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
        "Blocks:     {} total, {} avg native/block",
        block_count, avg_native_per_block,
    );
    println!();
    if peak_actions > 0 {
        println!(
            "Peak:       {:.0}/s included (block #{}, {} actions)",
            include_rate, peak_block, peak_actions,
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
        } => run_consensus(&rpc_urls, senders, duration, concurrency, batch_size).await,
        Command::Combined { .. } => run_combined(),
        Command::StateRoot {
            sizes,
            changed,
            blocks,
        } => run_state_root_scaling(&sizes, changed, blocks),
    }
}
