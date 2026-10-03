//! `oracle-feed` (s87): keep oracle MARK prices fresh on the bench devnet.
//!
//! A mark is fresh only while all 3 bench validators report within a 10 s
//! window (>=3 reporters, >2/3 stake), and goes stale 60 s after the last fresh
//! aggregate. So every `--interval-ms` each validator signs one fixed price for
//! every bench market (ids `1..=N`, the ids `consensus` trades) and sends it to
//! its own node: ceil(N/256) `SubmitOraclePrices` chunks, sample time = now,
//! nonce strictly increasing. Building, nonces and signing are the
//! price-feeder's own (`torus_price_feeder::submit`), so the payload is exactly
//! what a production feeder sends.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::Address;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use torus_price_feeder::node::{NodeApi, RpcNode};
use torus_price_feeder::submit::{build_submissions, sign, NonceGen};
use torus_types::{FixedPoint, MarketId, SignedNativeAction};

/// How often the stats line is printed and `--stats-file` rewritten.
const STATS_EVERY: Duration = Duration::from_secs(10);

pub struct ValidatorKey {
    pub index: usize,
    pub address: Address,
    pub key: SigningKey,
}

#[derive(Deserialize)]
struct KeyFile {
    validators: Vec<KeyEntry>,
}

#[derive(Deserialize)]
struct KeyEntry {
    index: usize,
    address: String,
    private_key: String,
}

/// `devnet/wsl/bench-validator-keys.json`, sorted by index. Indices must be
/// exactly `0..n` and every key must derive its listed address.
pub fn load_validator_keys(path: &Path) -> Result<Vec<ValidatorKey>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_validator_keys(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_validator_keys(raw: &str) -> Result<Vec<ValidatorKey>, String> {
    let file: KeyFile = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let mut keys = file
        .validators
        .into_iter()
        .map(|v| {
            let hex_key = v.private_key.trim_start_matches("0x");
            let bytes = hex::decode(hex_key).map_err(|e| format!("validator {}: bad key hex: {e}", v.index))?;
            let key = SigningKey::from_slice(&bytes).map_err(|e| format!("validator {}: bad key: {e}", v.index))?;
            let address: Address =
                v.address.parse().map_err(|e| format!("validator {}: address {:?}: {e}", v.index, v.address))?;
            let derived = crate::key_address(&key);
            if derived != address {
                return Err(format!("validator {}: key derives {derived:#x}, file says {address:#x}", v.index));
            }
            Ok(ValidatorKey { index: v.index, address, key })
        })
        .collect::<Result<Vec<_>, String>>()?;
    keys.sort_by_key(|k| k.index);
    if keys.is_empty() || keys.iter().enumerate().any(|(i, k)| k.index != i) {
        return Err("validator indices must be exactly 0..n".into());
    }
    Ok(keys)
}

/// The same fixed price (whole TRS) for every market.
pub fn market_prices(markets: &BTreeSet<MarketId>, price_whole: u64) -> BTreeMap<MarketId, FixedPoint> {
    let p = FixedPoint::from_raw(price_whole as i128 * FixedPoint::SCALE);
    markets.iter().map(|&m| (m, p)).collect()
}

/// One validator's submissions for one interval: chunks of <= 256 prices,
/// all sampled at `now_ms`, each with the next nonce from `nonces`.
pub fn round(
    prices: &BTreeMap<MarketId, FixedPoint>,
    listed: &BTreeSet<MarketId>,
    key: &SigningKey,
    nonces: &mut NonceGen,
    now_ms: u64,
) -> Vec<SignedNativeAction> {
    build_submissions(prices, listed, now_ms)
        .into_iter()
        .map(|sub| sign(sub, nonces.next(now_ms), key))
        .collect()
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ValidatorStats {
    pub index: usize,
    pub address: String,
    pub sent: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub last_error: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub sent: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub last_error: String,
    pub rounds: u64,
    pub markets: usize,
    /// `torus_getMarkPrice` of the first market on rpc url 0 (or the error):
    /// markPrice "0" = no usable (fresh) aggregate.
    pub mark_probe: serde_json::Value,
    pub per_validator: Vec<ValidatorStats>,
}

impl Stats {
    pub fn new(keys: &[ValidatorKey], markets: usize) -> Self {
        let per_validator = keys
            .iter()
            .map(|k| ValidatorStats { index: k.index, address: format!("{:#x}", k.address), ..Default::default() })
            .collect();
        Self { markets, per_validator, ..Default::default() }
    }

    /// One submission's RPC outcome (Ok = the node admitted it).
    pub fn record(&mut self, validator: usize, outcome: &Result<String, String>) {
        let v = &mut self.per_validator[validator];
        self.sent += 1;
        v.sent += 1;
        match outcome {
            Ok(_) => {
                self.accepted += 1;
                v.accepted += 1;
            }
            Err(e) => {
                self.rejected += 1;
                v.rejected += 1;
                let msg = format!("val{validator}: {e}");
                v.last_error = msg.clone();
                self.last_error = msg;
            }
        }
    }

    pub fn line(&self) -> String {
        let per: Vec<String> = self
            .per_validator
            .iter()
            .map(|v| format!("val{} {}/{}/{}", v.index, v.sent, v.accepted, v.rejected))
            .collect();
        format!(
            "[oracle-feed] rounds={} sent={} accepted={} rejected={} ({}) mark={} last_error={:?}",
            self.rounds,
            self.sent,
            self.accepted,
            self.rejected,
            per.join(", "),
            self.mark_probe,
            self.last_error
        )
    }

    /// Write via a temp file + rename, so readers never see a partial file.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock before epoch").as_millis() as u64
}

async fn mark_probe(client: &reqwest::Client, url: &str, market: MarketId) -> serde_json::Value {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "method": "torus_getMarkPrice", "params": [format!("{market:#x}")], "id": 1
    });
    let resp = match client.post(url).json(&body).send().await {
        Ok(r) => r.json::<serde_json::Value>().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match resp {
        Ok(v) => v.get("result").cloned().unwrap_or_else(|| serde_json::json!({ "error": v.get("error") })),
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// Markets to feed: `1..=n` restricted to what rpc 0 lists (an unlisted id
/// would make exec reject its whole chunk AFTER admission, invisibly). If the
/// node can't be asked, feed `1..=n` and say so.
async fn listed_markets(node: &RpcNode, n: u64) -> Result<BTreeSet<MarketId>, String> {
    let want: BTreeSet<MarketId> = (1..=n).collect();
    match node.listed_markets().await {
        Ok(on_chain) => {
            let on_chain: BTreeSet<MarketId> = on_chain.iter().map(|m| m.id).collect();
            let listed: BTreeSet<MarketId> = want.intersection(&on_chain).copied().collect();
            if listed.is_empty() {
                return Err(format!("none of markets 1..={n} is listed on rpc 0"));
            }
            if listed.len() < want.len() {
                eprintln!("[oracle-feed] WARN: only {}/{n} of markets 1..={n} are listed; feeding those", listed.len());
            }
            Ok(listed)
        }
        Err(e) => {
            eprintln!("[oracle-feed] WARN: torus_getMarkets failed ({e}); feeding 1..={n} unchecked");
            Ok(want)
        }
    }
}

pub struct FeedArgs<'a> {
    pub rpc_urls: &'a str,
    pub validator_keys: &'a Path,
    pub markets: u64,
    pub price: u64,
    pub interval_ms: u64,
    pub stats_file: &'a Path,
}

/// Run until SIGTERM/SIGINT; Err = startup misconfiguration.
pub async fn run(a: FeedArgs<'_>) -> Result<(), String> {
    let urls: Vec<&str> = a.rpc_urls.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
    let keys = load_validator_keys(a.validator_keys)?;
    if urls.len() != keys.len() {
        return Err(format!("{} rpc urls but {} validator keys (validator i sends to url i)", urls.len(), keys.len()));
    }
    if a.markets == 0 || a.price == 0 || a.interval_ms == 0 {
        return Err("--markets, --price and --interval-ms must be > 0".into());
    }
    let nodes: Vec<RpcNode> = urls.iter().map(|u| RpcNode::new(u)).collect::<Result<_, _>>()?;
    let listed = listed_markets(&nodes[0], a.markets).await?;
    let prices = market_prices(&listed, a.price);
    let probe_market = *listed.first().expect("non-empty");
    let probe_client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().map_err(|e| e.to_string())?;
    let mut nonces = vec![NonceGen::default(); keys.len()];
    let mut stats = Stats::new(&keys, listed.len());
    for k in &keys {
        println!("[oracle-feed] val{} {:#x} -> {}", k.index, k.address, urls[k.index]);
    }
    println!(
        "[oracle-feed] {} markets at {} TRS, {} chunk(s)/validator every {} ms",
        listed.len(),
        a.price,
        listed.len().div_ceil(torus_core::oracle::MAX_ORACLE_PRICES_PER_SUBMISSION),
        a.interval_ms
    );

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("SIGTERM handler: {e}"))?;
    let shutdown = async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    tokio::pin!(shutdown);
    let mut tick = tokio::time::interval(Duration::from_millis(a.interval_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_stats = Instant::now();

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = tick.tick() => {}
        }
        let now = now_ms();
        let mut set = tokio::task::JoinSet::new();
        for (i, k) in keys.iter().enumerate() {
            let signed = round(&prices, &listed, &k.key, &mut nonces[i], now);
            let node = nodes[i].clone();
            set.spawn(async move {
                let mut out = Vec::with_capacity(signed.len());
                for s in &signed {
                    out.push(node.submit(s).await);
                }
                (i, out)
            });
        }
        let results = tokio::select! {
            _ = &mut shutdown => break,
            r = set.join_all() => r,
        };
        for (i, outcomes) in results {
            for o in &outcomes {
                stats.record(i, o);
            }
        }
        stats.rounds += 1;
        if last_stats.elapsed() >= STATS_EVERY {
            last_stats = Instant::now();
            stats.mark_probe = mark_probe(&probe_client, urls[0], probe_market).await;
            println!("{}", stats.line());
            if let Err(e) = stats.write(a.stats_file) {
                eprintln!("[oracle-feed] WARN: stats file: {e}");
            }
        }
    }

    stats.mark_probe = mark_probe(&probe_client, urls[0], probe_market).await;
    println!("{} (final)", stats.line());
    stats.write(a.stats_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::U256;
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_core::oracle::MAX_ORACLE_SAMPLE_SKEW_MS;
    use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
    use torus_state::cf::CF_NATIVE_MARKETS;
    use torus_state::StateDb;
    use torus_types::eip712::TORUS_CHAIN_ID;
    use torus_types::NativeAction;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_slice(&[n; 32]).unwrap()
    }

    fn markets(n: u64) -> BTreeSet<MarketId> {
        (1..=n).collect()
    }

    fn sub(s: &SignedNativeAction) -> &torus_types::OracleSubmission {
        match &s.action {
            NativeAction::SubmitOraclePrices(sub) => sub,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn three_hundred_markets_are_two_chunks_256_and_44() {
        let m = markets(300);
        let signed = round(&market_prices(&m, 30_000), &m, &key(7), &mut NonceGen::default(), 1_000);
        let sizes: Vec<usize> = signed.iter().map(|s| sub(s).prices.len()).collect();
        assert_eq!(sizes, vec![256, 44]);
        let ids: Vec<MarketId> = signed.iter().flat_map(|s| sub(s).prices.iter().map(|p| p.0)).collect();
        assert_eq!(ids, (1..=300).collect::<Vec<_>>(), "every market exactly once, in order");
        let want = FixedPoint::from_raw(30_000 * FixedPoint::SCALE);
        assert!(signed.iter().all(|s| sub(s).prices.iter().all(|p| p.1 == want)));
    }

    #[test]
    fn nonces_strictly_increase_across_chunks_and_rounds() {
        let m = markets(600);
        let prices = market_prices(&m, 1);
        let mut g = NonceGen::default();
        let mut last = 0;
        // Same ms twice and a clock step back: never a repeat.
        for now in [5_000, 5_000, 4_000, 9_000] {
            for s in round(&prices, &m, &key(7), &mut g, now) {
                assert!(s.nonce > last, "nonce {} after {last}", s.nonce);
                last = s.nonce;
            }
        }
    }

    #[test]
    fn every_chunk_is_sampled_at_now() {
        let m = markets(300);
        for s in round(&market_prices(&m, 1), &m, &key(7), &mut NonceGen::default(), 1_700_000_000_123) {
            assert_eq!(sub(&s).timestamp, 1_700_000_000_123);
        }
    }

    #[test]
    fn key_file_parses_and_checks_addresses() {
        let entry = |i: usize, k: &SigningKey, addr: Address| {
            serde_json::json!({"index": i, "address": format!("{addr:#x}"), "private_key": format!("0x{}", hex::encode(k.to_bytes()))})
        };
        let (k0, k1) = (key(1), key(2));
        let (a0, a1) = (crate::key_address(&k0), crate::key_address(&k1));
        let ok = serde_json::json!({"note": "x", "validators": [entry(1, &k1, a1), entry(0, &k0, a0)]}).to_string();
        let keys = parse_validator_keys(&ok).unwrap();
        assert_eq!(keys.iter().map(|k| (k.index, k.address)).collect::<Vec<_>>(), vec![(0, a0), (1, a1)]);

        let wrong_addr = serde_json::json!({"validators": [entry(0, &k0, a1)]}).to_string();
        assert!(parse_validator_keys(&wrong_addr).err().unwrap().contains("derives"));
        let gap = serde_json::json!({"validators": [entry(0, &k0, a0), entry(2, &k1, a1)]}).to_string();
        assert!(parse_validator_keys(&gap).err().unwrap().contains("0..n"));
    }

    /// Part A's committed key file (or `$BENCH_VALIDATOR_KEYS`), when present:
    /// 3 validators whose keys derive their addresses.
    #[test]
    fn bench_key_file_loads_if_present() {
        let path = std::env::var("BENCH_VALIDATOR_KEYS").map(std::path::PathBuf::from).unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/wsl/bench-validator-keys.json")
        });
        if !path.exists() {
            eprintln!("skip: {} absent", path.display());
            return;
        }
        let keys = load_validator_keys(&path).unwrap();
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn stats_count_rejections_per_validator() {
        let keys: Vec<ValidatorKey> =
            (0..3).map(|i| ValidatorKey { index: i, address: crate::key_address(&key(i as u8 + 1)), key: key(i as u8 + 1) }).collect();
        let mut s = Stats::new(&keys, 300);
        s.record(0, &Ok("0xab".into()));
        s.record(2, &Err("oracle pending cap 4 reached".into()));
        s.record(2, &Ok("0xcd".into()));
        assert_eq!((s.sent, s.accepted, s.rejected), (3, 2, 1));
        assert_eq!((s.per_validator[2].sent, s.per_validator[2].rejected), (2, 1));
        assert!(s.last_error.contains("val2") && s.last_error.contains("pending cap"));
        let v: serde_json::Value = serde_json::to_value(&s).unwrap();
        for f in ["sent", "accepted", "rejected", "last_error", "per_validator"] {
            assert!(v.get(f).is_some(), "stats JSON lacks {f}");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        s.write(&path).unwrap();
        let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back["rejected"], 1);
    }

    /// The node's own path end to end: JSON wire decode (as torus_submitNativeAction),
    /// EIP-712 validate (as mempool admission) recovers the validator, exec
    /// (`exec_submit_oracle_prices`) accepts every chunk at a block 1 s later,
    /// and the next block-start aggregation makes every mark fresh at the price.
    #[test]
    fn signed_rounds_pass_the_node_and_make_every_mark_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let keys: Vec<SigningKey> = (1..=3).map(key).collect();
        for k in &keys {
            let a = crate::key_address(k);
            StakingManager::new(db.clone())
                .put_validator(&a, &ValidatorState {
                    address: a,
                    pubkey: [0; 32],
                    commission_bps: 0,
                    self_stake: MIN_SELF_DELEGATION,
                    total_delegated: U256::ZERO,
                    status: ValidatorStatus::Active,
                    jailed_until: None,
                    last_commission_change_block: None,
                    oracle_signer: None,
                })
                .unwrap();
        }
        let m = markets(300);
        for id in &m {
            db.put_cf_raw(CF_NATIVE_MARKETS, &id.to_be_bytes(), b"listed").unwrap();
        }
        let prices = market_prices(&m, 30_000);
        let block_s = 1_700_000_000u64;
        let now = block_s * 1_000 - 1_000; // sampled 1 s before the block
        assert!(block_s * 1_000 - now <= MAX_ORACLE_SAMPLE_SKEW_MS);

        let ctx_at = |h: u64, ts: u64| {
            NativeExecContext::new(db.clone(), h, ts, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO)
        };
        let mut ctx = ctx_at(5, block_s);
        for k in &keys {
            for s in round(&prices, &m, k, &mut NonceGen::default(), now) {
                let wire = hex::encode(serde_json::to_vec(&s).unwrap());
                let decoded: SignedNativeAction = serde_json::from_slice(&hex::decode(wire).unwrap()).unwrap();
                let sender = decoded.validate(now, TORUS_CHAIN_ID).unwrap();
                assert_eq!(sender, crate::key_address(k));
                let r = NativeExecutor::execute(&mut ctx, &sender, &decoded.action);
                assert!(r.success, "{:?}", r.error);
            }
        }

        let mut next = ctx_at(6, block_s + 1);
        let agg = NativeExecutor::begin_block_oracle(&mut next);
        assert!(next.fatal_error.is_none(), "{:?}", next.fatal_error);
        assert_eq!(agg.len(), 300);
        assert!(agg.iter().all(|r| r.success), "{:?}", agg.iter().find(|r| !r.success));
        let want = FixedPoint::from_raw(30_000 * FixedPoint::SCALE);
        for id in [1, 256, 257, 300] {
            let p = next.oracle.get_price(id, next.timestamp).unwrap();
            assert_eq!(p.usable(), Some(want), "market {id}");
            assert_eq!(p.num_reporters, 3, "market {id}");
        }
    }
}
