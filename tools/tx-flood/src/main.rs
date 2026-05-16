use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_consensus::TxEip1559;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes, Signature as AlloySig, TxKind, U256};
use alloy_rlp::Encodable;
use clap::Parser;
use k256::ecdsa::SigningKey;
use rand::Rng;
use tokio::sync::Semaphore;

// 20 hardhat well-known dev keys (deterministic — NEVER use in production)
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

const CHAIN_ID: u64 = 7778;

// Node mempool limits — must respect these to actually land txs in blocks
const MEMPOOL_MAX_PER_SENDER: u64 = 16;
const DRAIN_PER_SENDER_PER_BLOCK: u64 = 4;

#[derive(Parser)]
#[command(name = "torus-tx-flood", about = "Saturate Torus devnet with pre-signed EVM transfers")]
struct Cli {
    /// RPC endpoints (comma-separated). Txs fan out to ALL (no mempool gossip).
    #[arg(short, long, default_value = "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548")]
    rpc_urls: String,

    /// Total number of transactions per account
    #[arg(short = 'n', long, default_value_t = 500)]
    txs_per_account: u64,

    /// Number of accounts to use (1-20)
    #[arg(short, long, default_value_t = 20)]
    accounts: usize,

    /// Max concurrent in-flight HTTP requests
    #[arg(short, long, default_value_t = 256)]
    concurrency: usize,

    /// Gas price in gwei
    #[arg(short, long, default_value_t = 100)]
    gas_price_gwei: u64,

    /// Transfer amount in wei (default 1 TRS)
    #[arg(long, default_value_t = 1_000_000_000_000_000_000)]
    value_wei: u64,

    /// Only pre-sign, don't send (for benchmarking signing speed)
    #[arg(long)]
    dry_run: bool,

    /// Monitor block gas utilization every N seconds
    #[arg(long, default_value_t = 2)]
    monitor_interval: u64,

    /// Batch size per sender per round (default matches drain-per-block to avoid pool overflow)
    #[arg(long, default_value_t = 4)]
    batch_per_sender: u64,

    /// Max seconds to wait for nonce confirmation before moving on
    #[arg(long, default_value_t = 30)]
    drain_timeout_secs: u64,
}

struct Account {
    key: SigningKey,
    address: Address,
}

fn load_accounts(count: usize) -> Vec<Account> {
    HARDHAT_KEYS[..count]
        .iter()
        .map(|hex_key| {
            let bytes = hex::decode(hex_key).expect("valid hex");
            let key = SigningKey::from_slice(&bytes).expect("valid key");
            let verifying = key.verifying_key();
            let pubkey_bytes = verifying.to_encoded_point(false);
            let hash = alloy_primitives::keccak256(&pubkey_bytes.as_bytes()[1..]);
            let address = Address::from_slice(&hash[12..]);
            Account { key, address }
        })
        .collect()
}

fn sign_eip1559_tx(
    key: &SigningKey,
    chain_id: u64,
    nonce: u64,
    to: Address,
    value: U256,
    gas_price: u128,
    gas_limit: u64,
    input: Bytes,
) -> Vec<u8> {
    let tx = TxEip1559 {
        chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas: gas_price,
        max_priority_fee_per_gas: gas_price,
        to: TxKind::Call(to),
        value,
        input,
        access_list: Default::default(),
    };

    let mut rlp_buf = Vec::new();
    tx.encode(&mut rlp_buf);
    let mut hash_input = Vec::with_capacity(1 + rlp_buf.len());
    hash_input.push(0x02);
    hash_input.extend_from_slice(&rlp_buf);
    let signing_hash = alloy_primitives::keccak256(&hash_input);

    let (sig, recid) = key
        .sign_prehash_recoverable(signing_hash.as_slice())
        .expect("signing cannot fail");
    let sig_bytes = sig.to_bytes();
    let y_parity = recid.to_byte() != 0;

    let r_u256 = U256::from_be_slice(&sig_bytes[..32]);
    let s_u256 = U256::from_be_slice(&sig_bytes[32..]);
    let alloy_sig = AlloySig::new(r_u256, s_u256, y_parity);
    let signed = alloy_consensus::Signed::new_unchecked(tx, alloy_sig, signing_hash);
    let envelope = alloy_consensus::TxEnvelope::Eip1559(signed);

    let mut encoded = Vec::new();
    envelope.encode_2718(&mut encoded);
    encoded
}

async fn fetch_nonce(client: &reqwest::Client, url: &str, addr: &str) -> Result<u64, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionCount",
        "params": [addr, "pending"],
        "id": 1
    });
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("nonce fetch: {e}"))?
        .json()
        .await
        .map_err(|e| format!("nonce parse: {e}"))?;
    let hex_str = resp["result"]
        .as_str()
        .ok_or_else(|| format!("nonce error: {:?}", resp["error"]))?;
    let s = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    u64::from_str_radix(s, 16).map_err(|e| format!("nonce hex: {e}"))
}

async fn fetch_block_info(client: &reqwest::Client, url: &str) -> Option<(u64, u64, usize)> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBlockByNumber",
        "params": ["latest", false],
        "id": 1
    });
    let resp: serde_json::Value = client.post(url).json(&body).send().await.ok()?.json().await.ok()?;
    let block = resp.get("result")?;
    let number = u64::from_str_radix(
        block["number"].as_str()?.strip_prefix("0x")?,
        16,
    ).ok()?;
    let gas_used = u64::from_str_radix(
        block["gasUsed"].as_str()?.strip_prefix("0x")?,
        16,
    ).ok()?;
    let tx_count = block["transactions"].as_array().map(|a| a.len()).unwrap_or(0);
    Some((number, gas_used, tx_count))
}

/// Send raw tx to ONE endpoint, checking JSON-RPC error body (not just HTTP status).
async fn send_raw_tx(client: &reqwest::Client, url: &str, raw_hex: &str, id: u64) -> Result<String, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_sendRawTransaction",
        "params": [raw_hex],
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
        return Err(format!("rpc: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")));
    }
    resp["result"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "null result".to_string())
}

/// Send to ALL endpoints (true fan-out — no mempool gossip between validators).
async fn send_to_all(
    client: &reqwest::Client,
    urls: &[String],
    raw_hex: &str,
    id: u64,
) -> Result<String, String> {
    let mut any_hash = None;
    let mut last_err = String::new();
    for url in urls {
        match send_raw_tx(client, url, raw_hex, id).await {
            Ok(hash) => { any_hash = Some(hash); }
            Err(e) => { last_err = e; }
        }
    }
    any_hash.ok_or(last_err)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let rpc_urls: Vec<String> = cli.rpc_urls.split(',').map(|s| s.trim().to_string()).collect();
    let num_accounts = cli.accounts.min(20).max(1);
    let gas_price = (cli.gas_price_gwei as u128) * 1_000_000_000;
    let batch_per_sender = cli.batch_per_sender.min(MEMPOOL_MAX_PER_SENDER);
    let drain_timeout_secs = cli.drain_timeout_secs;

    let max_txs_per_block = DRAIN_PER_SENDER_PER_BLOCK * num_accounts as u64;

    println!("=== Torus TX Flood ===");
    println!("RPC endpoints:      {}", rpc_urls.join(", "));
    println!("Accounts:           {num_accounts}");
    println!("Txs per account:    {}", cli.txs_per_account);
    println!("Total txs:          {}", num_accounts as u64 * cli.txs_per_account);
    println!("Batch per sender:   {batch_per_sender} (mempool cap: {MEMPOOL_MAX_PER_SENDER})");
    println!("Drain strategy:     nonce-poll (timeout: {drain_timeout_secs}s)");
    println!("Concurrency:        {}", cli.concurrency);
    println!("Gas price:          {} gwei", cli.gas_price_gwei);
    println!("Fan-out:            ALL {} endpoints (no mempool gossip)", rpc_urls.len());
    println!("Est. throughput:    ~{} tx/block ({} gas/block)",
        max_txs_per_block, max_txs_per_block * 21_000);
    println!();

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(256)
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(30))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client");

    let accounts = load_accounts(num_accounts);
    println!("Loaded {} accounts:", accounts.len());
    for (i, acc) in accounts.iter().enumerate() {
        println!("  #{i:02}: {}", acc.address);
    }
    println!();

    // Fetch starting nonces
    println!("Fetching nonces from {} ...", rpc_urls[0]);
    let mut nonces = Vec::with_capacity(num_accounts);
    let mut nonce_ok = true;
    for acc in &accounts {
        let addr_hex = format!("0x{}", hex::encode(acc.address.as_slice()));
        match fetch_nonce(&client, &rpc_urls[0], &addr_hex).await {
            Ok(n) => nonces.push(n),
            Err(e) => {
                if cli.dry_run {
                    println!("  RPC unreachable ({e}) — using nonce 0 for all (dry-run)");
                    nonces = vec![0u64; num_accounts];
                    nonce_ok = false;
                    break;
                } else {
                    panic!("fetch nonce: {e}");
                }
            }
        }
    }
    if nonce_ok {
        for (i, n) in nonces.iter().enumerate() {
            println!("  #{i:02} nonce: {n}");
        }
    }
    println!();

    // Pre-sign ALL transactions upfront (fast — ~12k tx/s)
    let total_txs = num_accounts as u64 * cli.txs_per_account;
    println!("Pre-signing {total_txs} transactions...");
    let sign_start = Instant::now();

    // signed_txs[sender_idx][seq] = hex-encoded raw tx
    let mut signed_txs: Vec<Vec<String>> = Vec::with_capacity(num_accounts);
    let value = U256::from(cli.value_wei);
    let mut rng = rand::thread_rng();

    for (sender_idx, acc) in accounts.iter().enumerate() {
        let base_nonce = nonces[sender_idx];
        let mut sender_txs = Vec::with_capacity(cli.txs_per_account as usize);
        for seq in 0..cli.txs_per_account {
            let mut recv_idx = rng.gen_range(0..num_accounts);
            if recv_idx == sender_idx {
                recv_idx = (recv_idx + 1) % num_accounts;
            }
            let to = accounts[recv_idx].address;
            let raw = sign_eip1559_tx(
                &acc.key, CHAIN_ID, base_nonce + seq, to, value, gas_price, 21_000, Bytes::new(),
            );
            sender_txs.push(format!("0x{}", hex::encode(&raw)));
        }
        signed_txs.push(sender_txs);
    }

    let sign_elapsed = sign_start.elapsed();
    println!(
        "Signed {total_txs} txs in {:.2}s ({:.0} tx/s)",
        sign_elapsed.as_secs_f64(),
        total_txs as f64 / sign_elapsed.as_secs_f64()
    );
    println!();

    if cli.dry_run {
        println!("Dry run — not sending. First tx: {}", &signed_txs[0][0][..40]);
        return;
    }

    // === Drip-feed loop ===
    // Submit `batch_per_sender` txs per account, wait for them to drain (a few blocks),
    // then submit the next batch. This respects the mempool's per-sender caps.
    let submitted = Arc::new(AtomicU64::new(0));
    let accepted = Arc::new(AtomicU64::new(0));
    let rejected = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(cli.concurrency));

    // Start block monitor
    let monitor_client = client.clone();
    let monitor_url = rpc_urls[0].clone();
    let mon_submitted = submitted.clone();
    let mon_accepted = accepted.clone();
    let mon_rejected = rejected.clone();
    let monitor_interval = cli.monitor_interval;
    let blast_start = Instant::now();

    let monitor_handle = tokio::spawn(async move {
        let mut last_block = 0u64;
        let mut total_block_txs = 0usize;
        let mut blocks_seen = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(monitor_interval)).await;
            let sub = mon_submitted.load(Ordering::Relaxed);
            let acc = mon_accepted.load(Ordering::Relaxed);
            let rej = mon_rejected.load(Ordering::Relaxed);
            let elapsed = blast_start.elapsed().as_secs_f64();

            let block_info = fetch_block_info(&monitor_client, &monitor_url).await;
            let block_str = if let Some((num, gas, txs)) = block_info {
                if num != last_block {
                    let new_blocks = num - last_block;
                    total_block_txs += txs;
                    blocks_seen += new_blocks;
                    last_block = num;
                    let pct = gas as f64 / 30_000_000.0 * 100.0;
                    let avg_txs = if blocks_seen > 0 { total_block_txs as f64 / blocks_seen as f64 } else { 0.0 };
                    format!("blk #{num}: {txs} txs ({pct:.0}%) | avg: {avg_txs:.1} tx/blk")
                } else {
                    format!("blk #{num} (same)")
                }
            } else {
                "blk: ?".to_string()
            };

            let rate = if elapsed > 0.0 { acc as f64 / elapsed } else { 0.0 };
            println!(
                "[{elapsed:6.1}s] sub: {sub} | ok: {acc} | err: {rej} | {rate:.0} accepted/s | {block_str}"
            );
        }
    });

    println!("Drip-feeding txs in batches of {batch_per_sender}/sender to ALL {} endpoints...", rpc_urls.len());
    println!();

    let rpc_urls = Arc::new(rpc_urls);
    let client = Arc::new(client);
    let mut cursor = vec![0u64; num_accounts]; // next tx index per sender

    loop {
        // Check if all txs have been submitted
        let all_done = cursor.iter().enumerate().all(|(i, &c)| c >= signed_txs[i].len() as u64);
        if all_done {
            break;
        }

        // Submit one batch: up to batch_per_sender txs per sender, all senders in parallel
        let mut batch_handles = Vec::new();
        let mut batch_count = 0u64;

        for sender_idx in 0..num_accounts {
            let start = cursor[sender_idx] as usize;
            let end = (start + batch_per_sender as usize).min(signed_txs[sender_idx].len());
            if start >= end {
                continue;
            }

            for seq in start..end {
                let raw_hex = signed_txs[sender_idx][seq].clone();
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let client = client.clone();
                let urls = rpc_urls.clone();
                let accepted = accepted.clone();
                let rejected = rejected.clone();
                let submitted = submitted.clone();
                let tx_id = (sender_idx * cli.txs_per_account as usize + seq) as u64;

                batch_handles.push(tokio::spawn(async move {
                    let _permit = permit;
                    submitted.fetch_add(1, Ordering::Relaxed);
                    match send_to_all(&client, &urls, &raw_hex, tx_id).await {
                        Ok(_) => { accepted.fetch_add(1, Ordering::Relaxed); }
                        Err(_) => { rejected.fetch_add(1, Ordering::Relaxed); }
                    }
                }));
                batch_count += 1;
            }
            cursor[sender_idx] = end as u64;
        }

        // Wait for this batch to be submitted
        for h in batch_handles {
            let _ = h.await;
        }

        if batch_count == 0 {
            break;
        }

        // Poll nonces until all submitted txs are confirmed on-chain.
        // This ensures the mempool has drained before we send the next batch.
        let drain_deadline = Instant::now() + Duration::from_secs(drain_timeout_secs);
        loop {
            if Instant::now() > drain_deadline {
                tracing::warn!("drain timeout — proceeding with next batch");
                break;
            }
            let mut all_confirmed = true;
            for sender_idx in 0..num_accounts {
                let expected_nonce = nonces[sender_idx] + cursor[sender_idx];
                let addr_hex = format!("0x{}", hex::encode(accounts[sender_idx].address.as_slice()));
                if let Ok(current) = fetch_nonce(&client, &rpc_urls[0], &addr_hex).await {
                    if current < expected_nonce {
                        all_confirmed = false;
                        break;
                    }
                }
            }
            if all_confirmed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    let total_elapsed = blast_start.elapsed();
    let final_sub = submitted.load(Ordering::Relaxed);
    let final_acc = accepted.load(Ordering::Relaxed);
    let final_rej = rejected.load(Ordering::Relaxed);
    let final_rate = final_acc as f64 / total_elapsed.as_secs_f64();

    monitor_handle.abort();

    println!();
    println!("=== Done ===");
    println!("Submitted:  {final_sub}");
    println!("Accepted:   {final_acc}");
    println!("Rejected:   {final_rej}");
    println!("Duration:   {:.2}s", total_elapsed.as_secs_f64());
    println!("Rate:       {final_rate:.0} accepted/s");
    println!();

    // Final block check
    if let Some((num, gas, txs)) = fetch_block_info(&client, &rpc_urls[0]).await {
        let pct = gas as f64 / 30_000_000.0 * 100.0;
        println!("Latest block #{num}: {txs} txs, {gas} gas ({pct:.1}% full)");
    }
}
