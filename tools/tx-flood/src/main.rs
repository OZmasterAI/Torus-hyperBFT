use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

const DEFAULT_CHAIN_ID: u64 = 7778;

// Node mempool limits — must respect these to actually land txs in blocks
const MEMPOOL_MAX_PER_SENDER: u64 = 16;
const DRAIN_PER_SENDER_PER_BLOCK: u64 = 4;

/// Paced mode: stats file and progress line period.
const STATS_EVERY: Duration = Duration::from_secs(10);
/// Paced mode: a scheduler that wakes this late skips the missed ticks
/// (counted as `missed`) instead of sending them in a burst.
const MAX_LATE: Duration = Duration::from_secs(1);
/// Paced mode: the most one send (to all its endpoints) may take. A send not
/// answered by then is recorded as a `timeout` refusal (its nonce is sent
/// again), so every send has an outcome when the run ends.
const PACED_SEND_TIMEOUT: Duration = Duration::from_secs(12);

/// Which endpoints sender `i` sends to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Fanout {
    /// Every tx to every endpoint (the original flood: no mempool gossip).
    All,
    /// Sender `i` only to endpoint `i % n`, like a wallet (the node forwards
    /// the tx to the next EVM leader).
    Pinned,
}

impl Fanout {
    fn as_str(self) -> &'static str {
        match self {
            Fanout::All => "all",
            Fanout::Pinned => "pinned",
        }
    }
}

#[derive(Parser)]
#[command(
    name = "torus-tx-flood",
    about = "Saturate Torus devnet with pre-signed EVM transfers"
)]
struct Cli {
    /// RPC endpoints (comma-separated). With --fanout all (default) every tx
    /// goes to ALL of them (no mempool gossip).
    #[arg(
        short,
        long,
        default_value = "http://localhost:8545,http://localhost:8546,http://localhost:8547,http://localhost:8548"
    )]
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

    /// Endpoints per sender: `all` sends every tx to every URL; `pinned` sends
    /// sender i's txs only to URL i % n, like a wallet.
    #[arg(long, value_enum, default_value_t = Fanout::All)]
    fanout: Fanout,

    /// Paced mode: R transfers/s in aggregate (> 0), round-robin over the
    /// senders, one in flight per sender; a refused tx is sent again (same
    /// nonce) after a back-off. Runs until --duration-secs or SIGTERM/SIGINT.
    /// 0 (default) = the batch drip-feed of --txs-per-account.
    #[arg(long, default_value_t = 0.0)]
    rate: f64,

    /// Paced mode: stop after this many seconds (0 = only on SIGTERM/SIGINT).
    #[arg(long, default_value_t = 0)]
    duration_secs: u64,

    /// Sender keys, one per line: `<index> <0xaddress> <0xkey>` (the
    /// `bench-throughput gen-accounts --secret-keys` format; the address must
    /// match the key) or a bare `<0xkey>`. Default: the first --accounts
    /// hardhat keys.
    #[arg(long)]
    keys_file: Option<PathBuf>,

    /// EIP-155 chain id the transactions are signed for.
    #[arg(long, default_value_t = DEFAULT_CHAIN_ID)]
    chain_id: u64,

    /// Paced mode: write JSON stats here every 10 s and at exit.
    #[arg(long)]
    stats_file: Option<PathBuf>,
}

struct Account {
    /// The key's label: its index in the key file (or the hardhat position).
    index: u64,
    key: SigningKey,
    address: Address,
}

fn key_address(key: &SigningKey) -> Address {
    let pubkey_bytes = key.verifying_key().to_encoded_point(false);
    let hash = alloy_primitives::keccak256(&pubkey_bytes.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

fn load_accounts(count: usize) -> Vec<Account> {
    HARDHAT_KEYS[..count]
        .iter()
        .enumerate()
        .map(|(i, hex_key)| {
            let bytes = hex::decode(hex_key).expect("valid hex");
            let key = SigningKey::from_slice(&bytes).expect("valid key");
            let address = key_address(&key);
            Account {
                index: i as u64,
                key,
                address,
            }
        })
        .collect()
}

/// One key-file line: `<index> <0xaddress> <0xkey>` or `<0xkey>`; blank and
/// `#` lines are `None`. `pos` labels a bare key.
fn parse_key_line(line: &str, pos: u64) -> Result<Option<Account>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let fields: Vec<&str> = line.split_whitespace().collect();
    let (index, want, key_hex) = match fields.as_slice() {
        [key] => (pos, None, *key),
        [idx, addr, key] => (
            idx.parse::<u64>()
                .map_err(|e| format!("bad index '{idx}': {e}"))?,
            Some(*addr),
            *key,
        ),
        _ => {
            return Err(format!(
                "want '<index> <0xaddress> <0xkey>' or '<0xkey>', got {} fields",
                fields.len()
            ))
        }
    };
    let bytes =
        hex::decode(key_hex.trim_start_matches("0x")).map_err(|e| format!("bad key hex: {e}"))?;
    let key = SigningKey::from_slice(&bytes).map_err(|e| format!("bad key: {e}"))?;
    let address = key_address(&key);
    if let Some(want) = want {
        let got = format!("{address:#x}");
        if !got.eq_ignore_ascii_case(want) {
            return Err(format!("address {want} does not match its key ({got})"));
        }
    }
    Ok(Some(Account {
        index,
        key,
        address,
    }))
}

fn load_keys_file(path: &Path) -> Result<Vec<Account>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        match parse_key_line(line, out.len() as u64) {
            Ok(Some(a)) => out.push(a),
            Ok(None) => {}
            Err(e) => return Err(format!("{}:{}: {e}", path.display(), n + 1)),
        }
    }
    if out.is_empty() {
        return Err(format!("{}: no keys", path.display()));
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
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
    let resp: serde_json::Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let block = resp.get("result")?;
    let number = u64::from_str_radix(block["number"].as_str()?.strip_prefix("0x")?, 16).ok()?;
    let gas_used = u64::from_str_radix(block["gasUsed"].as_str()?.strip_prefix("0x")?, 16).ok()?;
    let tx_count = block["transactions"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    Some((number, gas_used, tx_count))
}

/// Send raw tx to ONE endpoint, checking JSON-RPC error body (not just HTTP status).
async fn send_raw_tx(
    client: &reqwest::Client,
    url: &str,
    raw_hex: &str,
    id: u64,
) -> Result<String, String> {
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
        return Err(format!(
            "rpc: {}",
            err.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
        ));
    }
    resp["result"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "null result".to_string())
}

/// Send to every endpoint in `urls` (true fan-out — no mempool gossip
/// between validators); Ok if any accepted.
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
            Ok(hash) => {
                any_hash = Some(hash);
            }
            Err(e) => {
                last_err = e;
            }
        }
    }
    any_hash.ok_or(last_err)
}

/// The endpoints sender `i` sends to.
fn sender_urls(fanout: Fanout, urls: &[String], i: usize) -> Vec<String> {
    match fanout {
        Fanout::All => urls.to_vec(),
        Fanout::Pinned => vec![urls[i % urls.len()].clone()],
    }
}

/// The endpoint whose pool holds sender `i`'s pending txs, so its nonces are
/// read there: the one it sends to (`pinned`), else the first.
fn home_url(fanout: Fanout, urls: &[String], i: usize) -> &str {
    match fanout {
        Fanout::All => &urls[0],
        Fanout::Pinned => &urls[i % urls.len()],
    }
}

/// Flag combinations refused before anything is signed or sent.
fn check_flags(cli: &Cli) -> Result<(), String> {
    if !cli.rate.is_finite() || cli.rate < 0.0 {
        return Err(format!("--rate must be a number >= 0 (got {})", cli.rate));
    }
    if cli.rate > 0.0 && cli.dry_run {
        return Err("--dry-run signs without sending, --rate sends: pick one".to_string());
    }
    if cli.rate == 0.0 && (cli.duration_secs > 0 || cli.stats_file.is_some()) {
        return Err("--duration-secs and --stats-file need --rate > 0 (paced mode)".to_string());
    }
    Ok(())
}

/// Paced mode: the recipient of sender `i`'s tx with `nonce`, another
/// sender (deterministic, so repeated runs send the same txs); itself when
/// it is the only one.
fn recipient(i: usize, nonce: u64, n: usize) -> usize {
    if n < 2 {
        return i;
    }
    (i + 1 + (nonce % (n as u64 - 1)) as usize) % n
}

/// Paced mode: what one RPC answer means for the sender's next send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// Pooled: the nonce is used.
    Accepted,
    /// Already pooled (an earlier attempt went through): the nonce is used.
    Duplicate,
    /// The chain is past this nonce: go on at `minimum` when the node said.
    NonceTooLow(Option<u64>),
    /// Refused for now (`reason`): send the same nonce again after the back-off.
    Retry {
        reason: &'static str,
        backoff_ms: u64,
    },
}

/// The first run of digits after `key` in `s`.
fn number_after(s: &str, key: &str) -> Option<u64> {
    let rest = &s[s.find(key)? + key.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Map a send result to an [`Outcome`] (the node's refusal texts: torus-mempool
/// `MempoolError` and torus-rpc `TxSubmitRateLimit`).
fn classify(res: &Result<String, String>) -> Outcome {
    let e = match res {
        Ok(_) => return Outcome::Accepted,
        Err(e) => e.as_str(),
    };
    if e.contains("duplicate tx") {
        return Outcome::Duplicate;
    }
    if e.contains("nonce too low") {
        return Outcome::NonceTooLow(number_after(e, "minimum "));
    }
    let (reason, backoff_ms) = if e.contains("mempool full") {
        ("mempool_full", 1_000)
    } else if e.contains("rate limit") {
        ("rate_limited", 2_000)
    } else if e.contains("nonce too far") {
        ("nonce_too_far", 1_000)
    } else if e.starts_with("timeout:") {
        ("timeout", 1_000)
    } else if e.starts_with("http:") || e.starts_with("parse:") {
        ("http", 1_000)
    } else {
        ("other", 5_000)
    };
    Outcome::Retry { reason, backoff_ms }
}

/// Paced mode: one sender's state (at most one send in flight).
#[derive(Clone, Debug, Default)]
struct SenderState {
    start_nonce: u64,
    next_nonce: u64,
    busy: bool,
    backoff_until: Option<Instant>,
    sent: u64,
    accepted: u64,
    rejected: u64,
}

/// The next sender at or after `cursor` (round-robin) that is idle and not
/// backing off, marked busy. `None`: every sender is busy or backing off.
fn pick_sender(senders: &mut [SenderState], cursor: usize, now: Instant) -> Option<usize> {
    let n = senders.len();
    for j in 0..n {
        let i = (cursor + j) % n;
        let s = &mut senders[i];
        if !s.busy && s.backoff_until.is_none_or(|t| now >= t) {
            s.busy = true;
            return Some(i);
        }
    }
    None
}

/// Apply one send's outcome to its sender.
fn settle(s: &mut SenderState, nonce: u64, outcome: Outcome, now: Instant) {
    s.busy = false;
    match outcome {
        Outcome::Accepted | Outcome::Duplicate => {
            s.next_nonce = nonce + 1;
            s.accepted += 1;
            s.backoff_until = None;
        }
        Outcome::NonceTooLow(minimum) => {
            s.rejected += 1;
            s.next_nonce = s.next_nonce.max(minimum.unwrap_or(nonce + 1));
        }
        Outcome::Retry { backoff_ms, .. } => {
            s.rejected += 1;
            s.backoff_until = Some(now + Duration::from_millis(backoff_ms));
        }
    }
}

/// Paced mode's schedule: tick `k` is due `k / rate` s after the start. A
/// scheduler more than [`MAX_LATE`] behind jumps to the current tick:
/// returns (next tick, ticks skipped).
fn catch_up(k: u64, elapsed: Duration, rate: f64) -> (u64, u64) {
    let due = Duration::from_secs_f64(k as f64 / rate);
    if elapsed <= due + MAX_LATE {
        return (k, 0);
    }
    let now_k = (elapsed.as_secs_f64() * rate).floor() as u64;
    (now_k.max(k), now_k.saturating_sub(k))
}

/// Paced mode's counters, shared by the scheduler and the send tasks.
#[derive(Debug, Default)]
struct Paced {
    senders: Vec<SenderState>,
    sent: u64,
    accepted: u64,
    duplicates: u64,
    rejected: u64,
    reasons: BTreeMap<&'static str, u64>,
    last_error: Option<String>,
    /// Per endpoint key (the URL, or "all"): [sent, accepted, rejected].
    per_rpc: BTreeMap<String, [u64; 3]>,
}

impl Paced {
    fn record(&mut self, i: usize, nonce: u64, rpc: &str, res: &Result<String, String>) {
        let outcome = classify(res);
        settle(&mut self.senders[i], nonce, outcome, Instant::now());
        self.senders[i].sent += 1;
        self.sent += 1;
        let r = self.per_rpc.entry(rpc.to_string()).or_default();
        r[0] += 1;
        match outcome {
            Outcome::Accepted => {
                self.accepted += 1;
                r[1] += 1;
            }
            Outcome::Duplicate => {
                self.accepted += 1;
                self.duplicates += 1;
                r[1] += 1;
            }
            Outcome::NonceTooLow(_) | Outcome::Retry { .. } => {
                self.rejected += 1;
                r[2] += 1;
                let reason = match outcome {
                    Outcome::Retry { reason, .. } => reason,
                    _ => "nonce_too_low",
                };
                *self.reasons.entry(reason).or_default() += 1;
                if let Err(e) = res {
                    self.last_error = Some(e.chars().take(300).collect());
                }
            }
        }
    }
}

/// Paced mode: the run's fixed facts, echoed into the stats file.
struct PacedPlan {
    rate: f64,
    duration_secs: u64,
    fanout: Fanout,
    chain_id: u64,
    rpc_urls: Vec<String>,
    value_wei: u64,
    gas_price_gwei: u64,
    started_unix_ms: u128,
    /// Per sender: (key index, address, endpoint key).
    senders: Vec<(u64, Address, String)>,
}

/// Paced mode: where the scheduler is.
#[derive(Clone, Copy, Debug, Default)]
struct Progress {
    elapsed_s: f64,
    ticks: u64,
    throttled: u64,
    missed: u64,
    /// Sends still running when the run ended (outcome unknown; aborted).
    unfinished: u64,
}

fn stats_json(plan: &PacedPlan, p: &Paced, prog: Progress, stopped_by: &str) -> serde_json::Value {
    let per_sender: Vec<serde_json::Value> = plan
        .senders
        .iter()
        .zip(&p.senders)
        .map(|((index, address, rpc), s)| {
            serde_json::json!({
                "index": index,
                "address": format!("{address:#x}"),
                "rpc": rpc,
                "start_nonce": s.start_nonce,
                "next_nonce": s.next_nonce,
                "sent": s.sent,
                "accepted": s.accepted,
                "rejected": s.rejected,
            })
        })
        .collect();
    let per_rpc: serde_json::Map<String, serde_json::Value> = p
        .per_rpc
        .iter()
        .map(|(k, [sent, acc, rej])| {
            let senders = plan.senders.iter().filter(|(_, _, r)| r == k).count();
            (
                k.clone(),
                serde_json::json!({"senders": senders, "sent": sent, "accepted": acc, "rejected": rej}),
            )
        })
        .collect();
    serde_json::json!({
        "mode": "paced",
        "rate": plan.rate,
        "duration_secs": plan.duration_secs,
        "fanout": plan.fanout.as_str(),
        "chain_id": plan.chain_id,
        "rpc_urls": plan.rpc_urls,
        "value_wei": plan.value_wei.to_string(),
        "gas_price_gwei": plan.gas_price_gwei,
        "senders": plan.senders.len(),
        "sender_offset": plan.senders.iter().map(|(i, _, _)| *i).min(),
        "started_unix_ms": plan.started_unix_ms as u64,
        "elapsed_s": (prog.elapsed_s * 1000.0).round() / 1000.0,
        "stopped_by": stopped_by,
        "ticks": prog.ticks,
        "sent": p.sent,
        "accepted": p.accepted,
        "duplicates": p.duplicates,
        "rejected": p.rejected,
        "reject_reasons": p.reasons,
        "throttled": prog.throttled,
        "missed": prog.missed,
        "unfinished": prog.unfinished,
        "last_error": p.last_error,
        "per_rpc": per_rpc,
        "per_sender": per_sender,
    })
}

/// Write `value` to `path` atomically (tmp + rename).
fn write_json(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

/// Paced mode: `rate` transfers/s until `duration_secs` (if > 0) or
/// SIGTERM/SIGINT. Returns the final stats.
async fn run_paced(
    cli: &Cli,
    accounts: Vec<Account>,
    rpc_urls: Vec<String>,
    client: reqwest::Client,
    send_timeout: Duration,
) -> Result<serde_json::Value, String> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).map_err(|e| format!("SIGTERM: {e}"))?;
    let mut sigint = signal(SignalKind::interrupt()).map_err(|e| format!("SIGINT: {e}"))?;
    let n = accounts.len();
    let urls: Vec<Vec<String>> = (0..n)
        .map(|i| sender_urls(cli.fanout, &rpc_urls, i))
        .collect();
    let rpc_key = |i: usize| match cli.fanout {
        Fanout::All => "all".to_string(),
        Fanout::Pinned => urls[i][0].clone(),
    };

    // Start nonces, each from the sender's own endpoint (its pool counts).
    // A node may be down for a while: the harness's SIGTERM ends this too.
    let init = async {
        let mut senders = Vec::with_capacity(n);
        for (i, acc) in accounts.iter().enumerate() {
            let addr = format!("{:#x}", acc.address);
            let mut tries = 0;
            let nonce = loop {
                match fetch_nonce(&client, home_url(cli.fanout, &rpc_urls, i), &addr).await {
                    Ok(v) => break v,
                    Err(e) if tries >= 10 => return Err(format!("start nonce of {addr}: {e}")),
                    Err(_) => {
                        tries += 1;
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            };
            senders.push(SenderState {
                start_nonce: nonce,
                next_nonce: nonce,
                ..Default::default()
            });
        }
        Ok(senders)
    };
    let senders = tokio::select! {
        r = init => r?,
        _ = sigterm.recv() => return Err("SIGTERM while fetching the start nonces".to_string()),
        _ = sigint.recv() => return Err("SIGINT while fetching the start nonces".to_string()),
    };
    let plan = PacedPlan {
        rate: cli.rate,
        duration_secs: cli.duration_secs,
        fanout: cli.fanout,
        chain_id: cli.chain_id,
        rpc_urls: rpc_urls.clone(),
        value_wei: cli.value_wei,
        gas_price_gwei: cli.gas_price_gwei,
        started_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        senders: accounts
            .iter()
            .enumerate()
            .map(|(i, a)| (a.index, a.address, rpc_key(i)))
            .collect(),
    };
    let state = Arc::new(Mutex::new(Paced {
        senders,
        ..Default::default()
    }));
    let accounts = Arc::new(accounts);
    let client = Arc::new(client);
    let gas_price = (cli.gas_price_gwei as u128) * 1_000_000_000;
    let value = U256::from(cli.value_wei);
    let chain_id = cli.chain_id;

    println!(
        "Paced: {} tx/s over {n} senders, fanout {}, {}",
        cli.rate,
        cli.fanout.as_str(),
        if cli.duration_secs > 0 {
            format!("{} s", cli.duration_secs)
        } else {
            "until SIGTERM".to_string()
        }
    );
    let start = Instant::now();
    let deadline = (cli.duration_secs > 0).then(|| start + Duration::from_secs(cli.duration_secs));
    let mut prog = Progress::default();
    let mut cursor = 0usize;
    let mut tasks = tokio::task::JoinSet::new();
    let mut next_stats = start + STATS_EVERY;
    let stopped_by = loop {
        let (k, skipped) = catch_up(prog.ticks, start.elapsed(), cli.rate);
        prog.ticks = k;
        prog.missed += skipped;
        let due = start + Duration::from_secs_f64(prog.ticks as f64 / cli.rate);
        // The window ends at the deadline itself, not at the last tick before
        // it: with no tick left inside it, wait out the deadline.
        let end = deadline.filter(|d| due >= *d);
        tokio::select! {
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(end.unwrap_or(due))) => {}
            _ = sigterm.recv() => break "SIGTERM",
            _ = sigint.recv() => break "SIGINT",
        }
        // ... and a tick due before the deadline but woken after it (a late
        // scheduler) is not sent: nothing leaves after the window.
        if end.is_some() || deadline.is_some_and(|d| Instant::now() >= d) {
            break "duration";
        }
        prog.ticks += 1;
        while tasks.try_join_next().is_some() {}
        let picked = {
            let mut st = state.lock().unwrap();
            pick_sender(&mut st.senders, cursor, Instant::now())
                .map(|i| (i, st.senders[i].next_nonce))
        };
        match picked {
            None => prog.throttled += 1,
            Some((i, nonce)) => {
                cursor = (i + 1) % n;
                let (state, accounts, client, urls) = (
                    state.clone(),
                    accounts.clone(),
                    client.clone(),
                    urls[i].clone(),
                );
                let rpc = rpc_key(i);
                tasks.spawn(async move {
                    let to = accounts[recipient(i, nonce, accounts.len())].address;
                    let raw = sign_eip1559_tx(
                        &accounts[i].key,
                        chain_id,
                        nonce,
                        to,
                        value,
                        gas_price,
                        21_000,
                        Bytes::new(),
                    );
                    let raw_hex = format!("0x{}", hex::encode(&raw));
                    let res = match tokio::time::timeout(
                        send_timeout,
                        send_to_all(&client, &urls, &raw_hex, nonce),
                    )
                    .await
                    {
                        Ok(res) => res,
                        Err(_) => Err(format!(
                            "timeout: no answer in {} s",
                            send_timeout.as_secs_f64()
                        )),
                    };
                    state.lock().unwrap().record(i, nonce, &rpc, &res);
                });
            }
        }
        if Instant::now() >= next_stats {
            next_stats += STATS_EVERY;
            prog.elapsed_s = start.elapsed().as_secs_f64();
            let st = state.lock().unwrap();
            println!(
                "[{:6.1}s] ticks {} sent {} accepted {} rejected {} throttled {} missed {}",
                prog.elapsed_s,
                prog.ticks,
                st.sent,
                st.accepted,
                st.rejected,
                prog.throttled,
                prog.missed
            );
            if let Some(path) = &cli.stats_file {
                if let Err(e) = write_json(path, &stats_json(&plan, &st, prog, "running")) {
                    eprintln!("stats file {}: {e}", path.display());
                }
            }
        }
    };
    // Let in-flight sends finish. A send still running after that is counted
    // (`unfinished`: its outcome is unknown) and aborted, never dropped silently.
    let _ = tokio::time::timeout(send_timeout + Duration::from_secs(3), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    prog.unfinished = tasks.len() as u64;
    tasks.abort_all();
    prog.elapsed_s = start.elapsed().as_secs_f64();
    let st = state.lock().unwrap();
    Ok(stats_json(&plan, &st, prog, stopped_by))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    if let Err(e) = check_flags(&cli) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let rpc_urls: Vec<String> = cli
        .rpc_urls
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let accounts = match &cli.keys_file {
        Some(path) => load_keys_file(path).unwrap_or_else(|e| {
            eprintln!("--keys-file: {e}");
            std::process::exit(2);
        }),
        None => load_accounts(cli.accounts.clamp(1, 20)),
    };
    let num_accounts = accounts.len();
    let chain_id = cli.chain_id;
    let gas_price = (cli.gas_price_gwei as u128) * 1_000_000_000;

    if cli.rate > 0.0 {
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(64)
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client");
        println!("=== Torus TX Flood (paced) ===");
        println!("RPC endpoints:      {}", rpc_urls.join(", "));
        println!("Senders:            {num_accounts}");
        let stats = match run_paced(&cli, accounts, rpc_urls, client, PACED_SEND_TIMEOUT).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("paced run failed: {e}");
                std::process::exit(1);
            }
        };
        if let Some(path) = &cli.stats_file {
            if let Err(e) = write_json(path, &stats) {
                eprintln!("stats file {}: {e}", path.display());
            }
        }
        println!();
        println!("=== Done ({}) ===", stats["stopped_by"]);
        for k in [
            "elapsed_s",
            "ticks",
            "sent",
            "accepted",
            "rejected",
            "throttled",
            "missed",
        ] {
            println!("{k:<10} {}", stats[k]);
        }
        println!("reasons    {}", stats["reject_reasons"]);
        return;
    }

    let batch_per_sender = cli.batch_per_sender.min(MEMPOOL_MAX_PER_SENDER);
    let drain_timeout_secs = cli.drain_timeout_secs;

    let max_txs_per_block = DRAIN_PER_SENDER_PER_BLOCK * num_accounts as u64;

    println!("=== Torus TX Flood ===");
    println!("RPC endpoints:      {}", rpc_urls.join(", "));
    println!("Accounts:           {num_accounts}");
    println!("Txs per account:    {}", cli.txs_per_account);
    println!(
        "Total txs:          {}",
        num_accounts as u64 * cli.txs_per_account
    );
    println!("Batch per sender:   {batch_per_sender} (mempool cap: {MEMPOOL_MAX_PER_SENDER})");
    println!("Drain strategy:     nonce-poll (timeout: {drain_timeout_secs}s)");
    println!("Concurrency:        {}", cli.concurrency);
    println!("Gas price:          {} gwei", cli.gas_price_gwei);
    match cli.fanout {
        Fanout::All => println!(
            "Fan-out:            ALL {} endpoints (no mempool gossip)",
            rpc_urls.len()
        ),
        Fanout::Pinned => println!(
            "Fan-out:            pinned (sender i -> endpoint i % {})",
            rpc_urls.len()
        ),
    }
    println!(
        "Est. throughput:    ~{} tx/block ({} gas/block)",
        max_txs_per_block,
        max_txs_per_block * 21_000
    );
    println!();

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(256)
        .pool_idle_timeout(Duration::from_secs(60))
        .tcp_keepalive(Duration::from_secs(30))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client");

    println!("Loaded {} accounts:", accounts.len());
    for (i, acc) in accounts.iter().enumerate() {
        println!("  #{i:02}: {}", acc.address);
    }
    println!();

    // Fetch starting nonces (pinned: from each sender's own endpoint)
    match cli.fanout {
        Fanout::All => println!("Fetching nonces from {} ...", rpc_urls[0]),
        Fanout::Pinned => println!("Fetching nonces from each sender's endpoint ..."),
    }
    let mut nonces = Vec::with_capacity(num_accounts);
    let mut nonce_ok = true;
    for (i, acc) in accounts.iter().enumerate() {
        let addr_hex = format!("0x{}", hex::encode(acc.address.as_slice()));
        match fetch_nonce(&client, home_url(cli.fanout, &rpc_urls, i), &addr_hex).await {
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
                &acc.key,
                chain_id,
                base_nonce + seq,
                to,
                value,
                gas_price,
                21_000,
                Bytes::new(),
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
        println!(
            "Dry run — not sending. First tx: {}",
            &signed_txs[0][0][..40]
        );
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
                    let avg_txs = if blocks_seen > 0 {
                        total_block_txs as f64 / blocks_seen as f64
                    } else {
                        0.0
                    };
                    format!("blk #{num}: {txs} txs ({pct:.0}%) | avg: {avg_txs:.1} tx/blk")
                } else {
                    format!("blk #{num} (same)")
                }
            } else {
                "blk: ?".to_string()
            };

            let rate = if elapsed > 0.0 {
                acc as f64 / elapsed
            } else {
                0.0
            };
            println!(
                "[{elapsed:6.1}s] sub: {sub} | ok: {acc} | err: {rej} | {rate:.0} accepted/s | {block_str}"
            );
        }
    });

    println!(
        "Drip-feeding txs in batches of {batch_per_sender}/sender ({} fan-out over {} endpoints)...",
        cli.fanout.as_str(),
        rpc_urls.len()
    );
    println!();

    let sender_url_lists: Vec<Arc<Vec<String>>> = (0..num_accounts)
        .map(|i| Arc::new(sender_urls(cli.fanout, &rpc_urls, i)))
        .collect();
    let client = Arc::new(client);
    let mut cursor = vec![0u64; num_accounts]; // next tx index per sender

    loop {
        // Check if all txs have been submitted
        let all_done = cursor
            .iter()
            .enumerate()
            .all(|(i, &c)| c >= signed_txs[i].len() as u64);
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

            #[allow(clippy::needless_range_loop)]
            for seq in start..end {
                let raw_hex = signed_txs[sender_idx][seq].clone();
                let permit = semaphore.clone().acquire_owned().await.unwrap();
                let client = client.clone();
                let urls = sender_url_lists[sender_idx].clone();
                let accepted = accepted.clone();
                let rejected = rejected.clone();
                let submitted = submitted.clone();
                let tx_id = (sender_idx * cli.txs_per_account as usize + seq) as u64;

                batch_handles.push(tokio::spawn(async move {
                    let _permit = permit;
                    submitted.fetch_add(1, Ordering::Relaxed);
                    match send_to_all(&client, &urls, &raw_hex, tx_id).await {
                        Ok(_) => {
                            accepted.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(_) => {
                            rejected.fetch_add(1, Ordering::Relaxed);
                        }
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
                let addr_hex =
                    format!("0x{}", hex::encode(accounts[sender_idx].address.as_slice()));
                if let Ok(current) =
                    fetch_nonce(&client, &sender_url_lists[sender_idx][0], &addr_hex).await
                {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// `bench-throughput gen-accounts --offset 20 --count 1 --secret-keys`
    /// is not reproduced here: a hardhat key and its well-known address stand in.
    const HH0_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const HH0_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

    /// The error of a result whose Ok side holds keys (no `Debug` on purpose).
    fn err_of<T>(r: Result<T, String>) -> String {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e,
        }
    }

    #[test]
    fn key_lines_parse_both_formats_and_check_the_address() {
        let a = parse_key_line(&format!("20 {HH0_ADDR} {HH0_KEY}"), 7)
            .unwrap()
            .unwrap();
        assert_eq!(a.index, 20);
        assert_eq!(format!("{:#x}", a.address), HH0_ADDR);
        let upper = HH0_ADDR.to_uppercase().replace("0X", "0x");
        assert!(parse_key_line(&format!("3 {upper} {HH0_KEY}"), 0)
            .unwrap()
            .is_some());
        let bare = parse_key_line(HH0_KEY, 7).unwrap().unwrap();
        assert_eq!(bare.index, 7, "a bare key is labelled by its position");
        assert_eq!(bare.address, a.address);
        assert!(parse_key_line("", 0).unwrap().is_none());
        assert!(parse_key_line("  # comment", 0).unwrap().is_none());
        let wrong = format!("20 0x70997970c51812dc3a010c7d01b50e0d17dc79c8 {HH0_KEY}");
        assert!(err_of(parse_key_line(&wrong, 0)).contains("does not match"));
        assert!(parse_key_line("20 0xabc", 0).is_err(), "two fields");
        assert!(parse_key_line("0xzz", 0).is_err(), "bad hex");
        assert!(
            parse_key_line(&format!("x {HH0_ADDR} {HH0_KEY}"), 0).is_err(),
            "bad index"
        );
    }

    #[test]
    fn keys_file_loads_every_key_in_order() {
        let dir = std::env::temp_dir().join(format!("tx-flood-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("keys.txt");
        let k1 = HARDHAT_KEYS[1];
        std::fs::write(
            &path,
            format!("# keys\n20 {HH0_ADDR} {HH0_KEY}\n\n0x{k1}\n"),
        )
        .unwrap();
        let accts = load_keys_file(&path).unwrap();
        assert_eq!(accts.len(), 2);
        assert_eq!((accts[0].index, accts[1].index), (20, 1));
        assert_eq!(accts[1].address, load_accounts(2)[1].address);
        std::fs::write(&path, "# nothing\n").unwrap();
        assert!(err_of(load_keys_file(&path)).contains("no keys"));
        std::fs::write(&path, "1 2 3 4\n").unwrap();
        assert!(
            err_of(load_keys_file(&path)).contains(":1:"),
            "line number named"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pinned_fanout_sends_each_sender_to_one_endpoint() {
        let urls: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(sender_urls(Fanout::All, &urls, 4), urls);
        for (i, want) in [(0, "a"), (1, "b"), (2, "c"), (3, "a"), (40, "b")] {
            assert_eq!(
                sender_urls(Fanout::Pinned, &urls, i),
                vec![want.to_string()]
            );
        }
    }

    /// Codex review: a sender's nonces are read where its pending txs are,
    /// in both modes (batch mode read them from endpoint 0 under `pinned`).
    #[test]
    fn nonces_are_read_from_the_senders_own_endpoint() {
        let urls: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        for i in 0..7 {
            assert_eq!(home_url(Fanout::All, &urls, i), "a");
            assert_eq!(
                home_url(Fanout::Pinned, &urls, i),
                sender_urls(Fanout::Pinned, &urls, i)[0]
            );
        }
    }

    /// Codex review: `--dry-run` never sends, so it does not combine with the
    /// paced mode; the paced-only flags need `--rate`.
    #[test]
    fn flag_combinations_are_checked_before_anything_is_sent() {
        let cli = |args: &[&str]| Cli::parse_from([&["torus-tx-flood"], args].concat());
        assert!(check_flags(&cli(&[])).is_ok(), "the old drip-feed default");
        assert!(check_flags(&cli(&["--dry-run"])).is_ok());
        assert!(check_flags(&cli(&["--rate", "5", "--duration-secs", "9"])).is_ok());
        let err = |args: &[&str]| check_flags(&cli(args)).unwrap_err();
        assert!(err(&["--rate", "5", "--dry-run"]).contains("--dry-run"));
        assert!(err(&["--rate=-1"]).contains("--rate must be"));
        assert!(err(&["--rate", "NaN"]).contains("--rate must be"));
        assert!(err(&["--duration-secs", "9"]).contains("need --rate"));
        assert!(err(&["--stats-file", "/tmp/x.json"]).contains("need --rate"));
    }

    #[test]
    fn recipient_is_deterministic_and_never_the_sender() {
        for n in 2..6usize {
            for i in 0..n {
                let mut seen = std::collections::BTreeSet::new();
                for nonce in 0..20u64 {
                    let r = recipient(i, nonce, n);
                    assert!(r < n && r != i, "n={n} i={i} nonce={nonce} -> {r}");
                    assert_eq!(r, recipient(i, nonce, n));
                    seen.insert(r);
                }
                assert_eq!(seen.len(), n - 1, "every other sender receives");
            }
        }
        assert_eq!(recipient(0, 5, 1), 0, "a lone sender pays itself");
    }

    #[test]
    fn refusals_are_classified_from_the_node_texts() {
        let err = |s: &str| Err::<String, String>(s.to_string());
        assert_eq!(classify(&Ok("0x1".into())), Outcome::Accepted);
        assert_eq!(
            classify(&err("rpc: mempool error: duplicate tx: 0xab")),
            Outcome::Duplicate
        );
        assert_eq!(
            classify(&err(
                "rpc: mempool error: nonce too low for 0x01: have 3, minimum 7"
            )),
            Outcome::NonceTooLow(Some(7))
        );
        assert_eq!(
            classify(&err("rpc: nonce too low")),
            Outcome::NonceTooLow(None)
        );
        let retry = |r: &Result<String, String>| match classify(r) {
            Outcome::Retry { reason, .. } => reason,
            o => panic!("{o:?}"),
        };
        assert_eq!(
            retry(&err("rpc: mempool error: mempool full")),
            "mempool_full"
        );
        assert_eq!(
            retry(&err("rpc: rate limit exceeded for sender")),
            "rate_limited"
        );
        assert_eq!(
            retry(&err(
                "rpc: mempool error: nonce too far in future for 0x01: have 90, max 64"
            )),
            "nonce_too_far"
        );
        assert_eq!(retry(&err("http: error sending request")), "http");
        assert_eq!(retry(&err("timeout: no answer in 12 s")), "timeout");
        assert_eq!(retry(&err("parse: expected value")), "http");
        assert_eq!(
            retry(&err("rpc: mempool error: insufficient balance")),
            "other"
        );
        assert_eq!(retry(&err("null result")), "other");
    }

    #[test]
    fn pick_skips_busy_and_backing_off_senders_round_robin() {
        let now = Instant::now();
        let mut s = vec![SenderState::default(); 3];
        assert_eq!(pick_sender(&mut s, 0, now), Some(0));
        assert_eq!(pick_sender(&mut s, 0, now), Some(1), "0 is busy");
        s[2].backoff_until = Some(now + Duration::from_secs(1));
        assert_eq!(
            pick_sender(&mut s, 2, now),
            None,
            "0, 1 busy and 2 backing off"
        );
        assert_eq!(
            pick_sender(&mut s, 2, now + Duration::from_secs(1)),
            Some(2),
            "the back-off is over"
        );
        settle(&mut s[0], 0, Outcome::Accepted, now);
        assert_eq!(
            pick_sender(&mut s, 1, now + Duration::from_secs(2)),
            Some(0),
            "wraps"
        );
    }

    #[test]
    fn settle_moves_the_nonce_only_when_it_was_used() {
        let now = Instant::now();
        let mut s = SenderState {
            next_nonce: 5,
            busy: true,
            ..Default::default()
        };
        settle(
            &mut s,
            5,
            Outcome::Retry {
                reason: "mempool_full",
                backoff_ms: 1_000,
            },
            now,
        );
        assert_eq!((s.next_nonce, s.busy, s.rejected), (5, false, 1));
        assert_eq!(s.backoff_until, Some(now + Duration::from_secs(1)));
        settle(&mut s, 5, Outcome::Accepted, now);
        assert_eq!((s.next_nonce, s.accepted, s.backoff_until), (6, 1, None));
        settle(&mut s, 6, Outcome::Duplicate, now);
        assert_eq!(s.next_nonce, 7);
        settle(&mut s, 7, Outcome::NonceTooLow(Some(12)), now);
        assert_eq!(s.next_nonce, 12, "the node's minimum");
        settle(&mut s, 12, Outcome::NonceTooLow(None), now);
        assert_eq!(s.next_nonce, 13);
        settle(&mut s, 3, Outcome::NonceTooLow(Some(4)), now);
        assert_eq!(s.next_nonce, 13, "never back");
    }

    #[test]
    fn a_late_scheduler_skips_the_missed_ticks() {
        let ms = Duration::from_millis;
        assert_eq!(catch_up(10, ms(1_000), 10.0), (10, 0), "on time");
        assert_eq!(
            catch_up(10, ms(1_999), 10.0),
            (10, 0),
            "late, within MAX_LATE"
        );
        assert_eq!(
            catch_up(10, ms(3_050), 10.0),
            (30, 20),
            "2 s late: 20 ticks skipped"
        );
        assert_eq!(catch_up(0, ms(0), 0.5), (0, 0));
    }

    /// A JSON-RPC endpoint on 127.0.0.1 that pools every raw tx; counts sends.
    async fn stub_rpc(sends: Arc<AtomicU64>, refuse_first: u64) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let sends = sends.clone();
                tokio::spawn(async move {
                    let mut buf: Vec<u8> = Vec::new();
                    loop {
                        let (end, len) = loop {
                            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                                let len = head
                                    .lines()
                                    .find_map(|l| l.strip_prefix("content-length:"))
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                                    .unwrap_or(0);
                                break (p + 4, len);
                            }
                            let mut chunk = [0u8; 4096];
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        };
                        while buf.len() < end + len {
                            let mut chunk = [0u8; 4096];
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let req: serde_json::Value =
                            serde_json::from_slice(&buf[end..end + len]).unwrap_or_default();
                        buf.drain(..end + len);
                        let reply = match req["method"].as_str() {
                            Some("eth_getTransactionCount") => {
                                serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": "0x0"})
                            }
                            Some("eth_sendRawTransaction") => {
                                let n = sends.fetch_add(1, Ordering::SeqCst);
                                if n < refuse_first {
                                    serde_json::json!({"jsonrpc": "2.0", "id": req["id"],
                                        "error": {"code": -32603, "message": "mempool error: mempool full"}})
                                } else {
                                    serde_json::json!({"jsonrpc": "2.0", "id": req["id"],
                                        "result": format!("0x{}", "00".repeat(32))})
                                }
                            }
                            _ => {
                                serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": null})
                            }
                        };
                        let body = reply.to_string();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                            body.len()
                        );
                        if sock.write_all(resp.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        url
    }

    fn paced_cli(urls: &[String], rate: f64, secs: u64, fanout: Fanout) -> Cli {
        let mut cli = Cli::parse_from(["torus-tx-flood"]);
        cli.rpc_urls = urls.join(",");
        cli.rate = rate;
        cli.duration_secs = secs;
        cli.fanout = fanout;
        cli
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    /// The knob: `--rate R` for D seconds sends ~R*D txs, each sender only to
    /// its own endpoint under `pinned`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paced_mode_holds_the_rate_and_pins_each_sender() {
        let (a, b) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let urls = vec![stub_rpc(a.clone(), 0).await, stub_rpc(b.clone(), 0).await];
        let cli = paced_cli(&urls, 40.0, 2, Fanout::Pinned);
        let stats = run_paced(
            &cli,
            load_accounts(4),
            urls.clone(),
            client(),
            PACED_SEND_TIMEOUT,
        )
        .await
        .unwrap();
        let num = |k: &str| stats[k].as_u64().unwrap();
        let sent = num("sent");
        let ticks = num("ticks");
        assert!(
            (76..=80).contains(&ticks),
            "40 tx/s for 2 s: ticks 0..80, never more (a late last wake drops one): {stats}"
        );
        assert_eq!(
            sent + num("throttled") + num("missed"),
            ticks,
            "every tick sent or counted"
        );
        assert_eq!(num("unfinished"), 0);
        assert!(
            sent >= 60,
            "a loaded host may throttle a few, not most: {stats}"
        );
        assert_eq!(num("accepted"), sent);
        assert_eq!(stats["stopped_by"], "duration");
        assert_eq!(
            a.load(Ordering::SeqCst) + b.load(Ordering::SeqCst),
            sent,
            "one send per tx"
        );
        let per = stats["per_sender"].as_array().unwrap();
        let by = |idx: &[usize]| {
            idx.iter()
                .map(|&i| per[i]["sent"].as_u64().unwrap())
                .sum::<u64>()
        };
        assert_eq!(
            a.load(Ordering::SeqCst),
            by(&[0, 2]),
            "senders 0 and 2 -> endpoint 0 only"
        );
        assert_eq!(
            b.load(Ordering::SeqCst),
            by(&[1, 3]),
            "senders 1 and 3 -> endpoint 1 only"
        );
        for (i, s) in per.iter().enumerate() {
            assert_eq!(s["rpc"], urls[i % 2]);
            assert_eq!(
                s["next_nonce"].as_u64(),
                s["accepted"].as_u64(),
                "start nonce 0"
            );
        }
        assert_eq!(stats["per_rpc"][&urls[0]]["senders"], 2);
    }

    /// `all` keeps the old fan-out: every tx reaches every endpoint.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paced_mode_all_fanout_sends_every_tx_everywhere() {
        let (a, b) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let urls = vec![stub_rpc(a.clone(), 0).await, stub_rpc(b.clone(), 0).await];
        let cli = paced_cli(&urls, 20.0, 1, Fanout::All);
        let stats = run_paced(&cli, load_accounts(2), urls, client(), PACED_SEND_TIMEOUT)
            .await
            .unwrap();
        let ticks = stats["ticks"].as_u64().unwrap();
        assert!((18..=20).contains(&ticks), "{stats}");
        let accepted = stats["accepted"].as_u64().unwrap();
        assert!(accepted >= 10, "{stats}");
        assert_eq!(a.load(Ordering::SeqCst), accepted);
        assert_eq!(b.load(Ordering::SeqCst), accepted);
        assert_eq!(stats["fanout"], "all");
    }

    /// A refused tx is sent again with the SAME nonce after the back-off, so
    /// the sender never leaves a nonce gap.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_tx_is_resent_with_the_same_nonce() {
        let sends = Arc::new(AtomicU64::new(0));
        let urls = vec![stub_rpc(sends.clone(), 1).await];
        let cli = paced_cli(&urls, 20.0, 2, Fanout::Pinned);
        let stats = run_paced(&cli, load_accounts(1), urls, client(), PACED_SEND_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(stats["rejected"], 1, "{stats}");
        assert_eq!(stats["reject_reasons"]["mempool_full"], 1);
        let acc = stats["accepted"].as_u64().unwrap();
        assert!(acc > 0);
        assert_eq!(
            stats["per_sender"][0]["next_nonce"].as_u64(),
            Some(acc),
            "no gap"
        );
        assert!(
            stats["throttled"].as_u64().unwrap() > 0,
            "the back-off throttles a lone sender"
        );
    }

    /// `--duration-secs` is the window: a rate whose next tick falls after the
    /// deadline still runs until the deadline (elapsed = the window), it does
    /// not stop right after the last send. RED before: 0.5 tx/s for 2 s sent
    /// at t = 0, saw the next tick due at 2 s, and stopped at once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paced_mode_runs_until_the_deadline_between_ticks() {
        let sends = Arc::new(AtomicU64::new(0));
        let urls = vec![stub_rpc(sends.clone(), 0).await];
        let cli = paced_cli(&urls, 0.5, 2, Fanout::Pinned);
        let t0 = Instant::now();
        let stats = run_paced(&cli, load_accounts(1), urls, client(), PACED_SEND_TIMEOUT)
            .await
            .unwrap();
        let elapsed = stats["elapsed_s"].as_f64().unwrap();
        assert!(elapsed >= 1.95, "ran the whole 2 s window: {stats}");
        assert!(t0.elapsed() >= Duration::from_millis(1_950));
        assert_eq!(stats["ticks"], 1, "one tick due inside the window (t = 0)");
        assert_eq!(stats["sent"], 1);
        assert_eq!(stats["stopped_by"], "duration");
        assert_eq!(sends.load(Ordering::SeqCst), 1);
    }

    /// An endpoint that accepts connections and never answers.
    async fn silent_rpc() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        url
    }

    /// Codex review 3: every send has an outcome when the run ends. Under
    /// `--fanout all` a send asks its endpoints one after another; one that
    /// never answers is cut at the send timeout and recorded as a `timeout`
    /// refusal, not left running past the shutdown grace and dropped from
    /// the stats. RED before: no bound on a send; the run waited out its 15 s
    /// grace and reported neither send.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unanswered_send_is_cut_and_counted() {
        let sends = Arc::new(AtomicU64::new(0));
        let urls = vec![stub_rpc(sends.clone(), 0).await, silent_rpc().await];
        let cli = paced_cli(&urls, 1.0, 2, Fanout::All);
        let slow = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        let t0 = Instant::now();
        let stats = run_paced(&cli, load_accounts(2), urls, slow, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(t0.elapsed() < Duration::from_secs(8), "{:?}", t0.elapsed());
        assert_eq!(stats["sent"], 2, "{stats}");
        assert_eq!(stats["reject_reasons"]["timeout"], 2);
        assert_eq!(stats["unfinished"], 0);
        assert_eq!(
            sends.load(Ordering::SeqCst),
            2,
            "the live endpoint got both"
        );
    }
}
