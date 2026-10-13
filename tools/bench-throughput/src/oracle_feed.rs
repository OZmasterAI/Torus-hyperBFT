//! `oracle-feed` (s87): keep oracle MARK prices fresh on the bench devnet.
//!
//! BACKPORT (bench/g3-base-199bdc97): main's `oracle-feed` (b33df265 +
//! 084a698b `--walk-bp`) on the 199bdc97 node types. main builds, nonces and
//! signs through `torus_price_feeder::submit`, which does not exist at
//! 199bdc97; the few lines it uses are inlined here ([`build_submissions`],
//! [`NonceGen`], [`sign`], [`RpcNode`]). The signed payload is 199bdc97's own:
//! `NativeAction::SubmitOraclePrices(OracleSubmission { prices, timestamp })`
//! EIP-712-signed by `torus_types::eip712::sign_native_action` (chain id
//! 7778), sent JSON-hex via `torus_submitNativeAction`. The 256-price chunk
//! and the `0 < price <= 10^12` filter are main's exec rules mirrored on the
//! bench side (199bdc97's exec has neither), so the load shape is main's.
//!
//! NOTE: a 199bdc97 node stores every submission (one row per market,
//! validator and block) but never aggregates them (`aggregate_oracle_prices`
//! has no caller), so `torus_getMarkPrice` stays 0 whatever this feed sends.
//!
//! A mark is fresh only while all 3 bench validators report within a 10 s
//! window (>=3 reporters, >2/3 stake), and goes stale 60 s after the last fresh
//! aggregate. So every `--interval-ms` each validator signs one fixed price for
//! every bench market (ids `1..=N`, the ids `consensus` trades) and sends it to
//! its own node: ceil(N/256) `SubmitOraclePrices` chunks, sample time = now,
//! nonce strictly increasing. `--walk-bp N` (item 6 step 0.5) moves every
//! market's price `±N` bp per round around `--price` ([`PriceWalk`]); 0 (the
//! default) is the fixed price.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::Address;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use torus_types::{FixedPoint, MarketId, NativeAction, OracleSubmission, SignedNativeAction};

/// How often the stats line is printed and `--stats-file` rewritten.
const STATS_EVERY: Duration = Duration::from_secs(10);

/// Prices per `SubmitOraclePrices` (main's `MAX_ORACLE_PRICES_PER_SUBMISSION`).
pub const MAX_PRICES_PER_SUBMISSION: usize = 256;
/// Largest price sent (main's `MAX_ORACLE_PRICE_RAW`: 10^12 units).
pub const MAX_PRICE_RAW: i128 = 1_000_000_000_000 * FixedPoint::SCALE;
/// `torus_getMarkets` page size (the node's maximum).
pub const MARKETS_PAGE: u32 = 500;

/// main's `valid_oracle_price`: `0 < price <= MAX_PRICE_RAW`.
pub fn valid_price(price: FixedPoint) -> bool {
    price > FixedPoint::ZERO && price.raw() <= MAX_PRICE_RAW
}

/// The submissions for this cycle: only LISTED markets with a valid price,
/// one entry per market (BTreeMap keys), split into chunks of
/// [`MAX_PRICES_PER_SUBMISSION`]. Empty if nothing is left.
pub fn build_submissions(
    prices: &BTreeMap<MarketId, FixedPoint>,
    listed: &BTreeSet<MarketId>,
    sample_ts_ms: u64,
) -> Vec<OracleSubmission> {
    let ok: Vec<(MarketId, FixedPoint)> = prices
        .iter()
        .filter(|(m, p)| listed.contains(m) && valid_price(**p))
        .map(|(m, p)| (*m, *p))
        .collect();
    ok.chunks(MAX_PRICES_PER_SUBMISSION)
        .map(|c| OracleSubmission { prices: c.to_vec(), timestamp: sample_ts_ms })
        .collect()
}

/// Nonce = `max(now_ms, last + 1)`: strictly increasing even if the clock
/// stalls or steps back, and inside the node's ±60 s nonce window otherwise.
#[derive(Clone, Debug, Default)]
pub struct NonceGen {
    last: u64,
}

impl NonceGen {
    pub fn next(&mut self, now_ms: u64) -> u64 {
        self.last = now_ms.max(self.last.saturating_add(1));
        self.last
    }
}

/// EIP-712-sign a submission with the validator key (199bdc97's signing).
pub fn sign(sub: OracleSubmission, nonce: u64, key: &SigningKey) -> SignedNativeAction {
    torus_types::eip712::sign_native_action(NativeAction::SubmitOraclePrices(sub), nonce, key)
}

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

/// Item 6 step 0.5 (`--walk-bp`): a deterministic, bounded, mean-reverting
/// walk of every market's price, one step per round, so the aggregated mark
/// changes every round. Each step moves a market's offset by exactly `walk_bp`
/// basis points, toward the base price with probability
/// `1/2 + |offset| / (2 * bound)`, `bound = 8 * walk_bp`, so it never leaves
/// `base ± bound`. The draw depends only on (round, market): every run and
/// every validator sees the same prices. `walk_bp = 0` is exactly
/// [`market_prices`] every round.
pub struct PriceWalk {
    /// [`market_prices`]: round 0 and every round of a 0 bp walk.
    base: BTreeMap<MarketId, FixedPoint>,
    walk_bp: i64,
    offsets_bp: BTreeMap<MarketId, i64>,
    rounds: u64,
}

/// SplitMix64 finaliser: one round's draw for one market.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl PriceWalk {
    /// Bound of the walk as a multiple of the step.
    pub const BOUND_STEPS: i64 = 8;

    /// Err when the bound would reach the price itself (`8 * walk_bp >= 10_000`).
    pub fn new(markets: &BTreeSet<MarketId>, price_whole: u64, walk_bp: u64) -> Result<Self, String> {
        if walk_bp.saturating_mul(Self::BOUND_STEPS as u64) >= 10_000 {
            return Err(format!("--walk-bp {walk_bp}: the walk's bound (8 x) must stay below 10000 bp"));
        }
        Ok(Self {
            base: market_prices(markets, price_whole),
            walk_bp: walk_bp as i64,
            offsets_bp: markets.iter().map(|&m| (m, 0)).collect(),
            rounds: 0,
        })
    }

    /// Advance one round and return its prices.
    pub fn next_round(&mut self) -> BTreeMap<MarketId, FixedPoint> {
        self.rounds += 1;
        let (w, bound) = (self.walk_bp, Self::BOUND_STEPS * self.walk_bp);
        if w > 0 {
            for (&m, x) in self.offsets_bp.iter_mut() {
                let draw = mix(self.rounds.wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ m) % (2 * bound) as u64;
                let toward_base = (draw as i64) < bound + x.abs();
                let down = if *x == 0 { toward_base } else { toward_base == (*x > 0) };
                *x += if down { -w } else { w };
            }
        }
        self.base
            .iter()
            .map(|(&m, p)| (m, FixedPoint::from_raw(p.raw() * i128::from(10_000 + self.offsets_bp[&m]) / 10_000)))
            .collect()
    }
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

/// One `torus_getMarkets` page: the market ids (`marketId` hex).
pub fn parse_markets_page(v: &Value) -> Result<Vec<MarketId>, String> {
    v.as_array()
        .ok_or("getMarkets: expected an array")?
        .iter()
        .map(|m| {
            let s = m.get("marketId").and_then(Value::as_str).ok_or("marketId: not a string")?;
            u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| format!("marketId {s:?}: {e}"))
        })
        .collect()
}

/// Page through `torus_getMarkets` until a short page.
pub async fn collect_market_pages<F, Fut>(mut page: F) -> Result<Vec<MarketId>, String>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<Vec<MarketId>, String>>,
{
    let mut all = Vec::new();
    let mut offset = 0u32;
    loop {
        let p = page(offset).await?;
        let n = p.len() as u32;
        all.extend(p);
        if n < MARKETS_PAGE {
            return Ok(all);
        }
        offset = offset.saturating_add(n);
    }
}

/// JSON-RPC client for one validator's node (2 s timeout per call).
#[derive(Clone)]
pub struct RpcNode {
    url: String,
    client: reqwest::Client,
}

impl RpcNode {
    pub fn new(url: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { url: url.to_string(), client })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params, "id": 1});
        let resp = self.client.post(&self.url).json(&body).send().await.map_err(|e| format!("{method}: {e}"))?;
        let v: Value = resp.json().await.map_err(|e| format!("{method}: bad response: {e}"))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            let msg = err.get("message").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| err.to_string());
            return Err(msg);
        }
        v.get("result").cloned().ok_or_else(|| format!("{method}: no result"))
    }

    pub async fn listed_markets(&self) -> Result<Vec<MarketId>, String> {
        collect_market_pages(|offset| async move {
            parse_markets_page(&self.call("torus_getMarkets", serde_json::json!([offset, MARKETS_PAGE])).await?)
        })
        .await
    }

    /// `torus_submitNativeAction`; Ok = the action hash.
    pub async fn submit(&self, signed: &SignedNativeAction) -> Result<String, String> {
        // The node does hex::decode -> serde_json::from_slice.
        let json = serde_json::to_vec(signed).map_err(|e| e.to_string())?;
        let r = self
            .call("torus_submitNativeAction", serde_json::json!([format!("0x{}", hex::encode(json))]))
            .await?;
        r.as_str().map(str::to_owned).ok_or_else(|| "submit: result not a string".into())
    }
}

/// Markets to feed: `1..=n` restricted to what rpc 0 lists. If the node can't
/// be asked, feed `1..=n` and say so.
async fn listed_markets(node: &RpcNode, n: u64) -> Result<BTreeSet<MarketId>, String> {
    let want: BTreeSet<MarketId> = (1..=n).collect();
    match node.listed_markets().await {
        Ok(on_chain) => {
            let on_chain: BTreeSet<MarketId> = on_chain.into_iter().collect();
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
    /// [`PriceWalk`] step per round in basis points (0 = fixed `price`).
    pub walk_bp: u64,
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
    let mut walk = PriceWalk::new(&listed, a.price, a.walk_bp)?;
    let probe_market = *listed.first().expect("non-empty");
    let probe_client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().map_err(|e| e.to_string())?;
    let mut nonces = vec![NonceGen::default(); keys.len()];
    let mut stats = Stats::new(&keys, listed.len());
    for k in &keys {
        println!("[oracle-feed] val{} {:#x} -> {}", k.index, k.address, urls[k.index]);
    }
    println!(
        "[oracle-feed] {} markets at {} TRS (walk ±{} bp/round, bound ±{} bp), {} chunk(s)/validator every {} ms",
        listed.len(),
        a.price,
        a.walk_bp,
        a.walk_bp as i64 * PriceWalk::BOUND_STEPS,
        listed.len().div_ceil(MAX_PRICES_PER_SUBMISSION),
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
        let prices = walk.next_round();
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
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
    use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
    use torus_state::StateDb;
    use torus_types::eip712::TORUS_CHAIN_ID;

    fn key(n: u8) -> SigningKey {
        SigningKey::from_slice(&[n; 32]).unwrap()
    }

    fn markets(n: u64) -> BTreeSet<MarketId> {
        (1..=n).collect()
    }

    fn fp(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
    }

    fn sub(s: &SignedNativeAction) -> &OracleSubmission {
        match &s.action {
            NativeAction::SubmitOraclePrices(sub) => sub,
            other => panic!("{other:?}"),
        }
    }

    // ---- ported from torus-price-feeder submit.rs / node.rs (main) ----

    #[test]
    fn builds_only_listed_valid_markets() {
        let prices: BTreeMap<MarketId, FixedPoint> = [
            (1, fp(65_000)),
            (2, FixedPoint::ZERO),
            (3, FixedPoint::from_raw(MAX_PRICE_RAW + 1)),
            (4, fp(100)),
            (5, fp(7)),
        ]
        .into_iter()
        .collect();
        let listed: BTreeSet<MarketId> = [1, 2, 3, 4].into_iter().collect();
        let subs = build_submissions(&prices, &listed, 42);
        assert_eq!(subs.len(), 1);
        let got: Vec<(MarketId, i128)> = subs[0].prices.iter().map(|(m, p)| (*m, p.raw())).collect();
        assert_eq!(got, vec![(1, fp(65_000).raw()), (4, fp(100).raw())]);
        assert_eq!(subs[0].timestamp, 42);
    }

    #[test]
    fn nothing_to_send_is_empty() {
        let listed: BTreeSet<MarketId> = [1].into_iter().collect();
        assert!(build_submissions(&BTreeMap::new(), &listed, 0).is_empty());
        let prices: BTreeMap<MarketId, FixedPoint> = [(2, fp(1))].into_iter().collect();
        assert!(build_submissions(&prices, &listed, 0).is_empty());
    }

    #[test]
    fn chunks_at_the_cap() {
        let prices: BTreeMap<MarketId, FixedPoint> = (1..=600).map(|m| (m, fp(1))).collect();
        let listed: BTreeSet<MarketId> = prices.keys().copied().collect();
        let sizes: Vec<usize> = build_submissions(&prices, &listed, 0).iter().map(|s| s.prices.len()).collect();
        assert_eq!(sizes, vec![256, 256, 88]);
    }

    /// Every built submission: 1..=256 entries, no duplicates, listed
    /// markets only, valid prices, nothing valid dropped.
    #[test]
    fn every_built_submission_passes_main_exec_rules() {
        let mut rng = StdRng::seed_from_u64(517);
        for case in 0..500 {
            let n = rng.gen_range(0..700usize);
            let prices: BTreeMap<MarketId, FixedPoint> = (0..n)
                .map(|_| {
                    let raw = match rng.gen_range(0..6) {
                        0 => 0,
                        1 => -rng.gen_range(1..1_000_000i128),
                        2 => MAX_PRICE_RAW + rng.gen_range(0..2i128),
                        3 => i128::MAX,
                        _ => rng.gen_range(1..MAX_PRICE_RAW),
                    };
                    (rng.gen_range(0..800u64), FixedPoint::from_raw(raw))
                })
                .collect();
            let listed: BTreeSet<MarketId> = (0..800u64).filter(|_| rng.gen_bool(0.7)).collect();
            let subs = build_submissions(&prices, &listed, 1);
            let mut seen = BTreeSet::new();
            for s in &subs {
                assert!((1..=MAX_PRICES_PER_SUBMISSION).contains(&s.prices.len()), "case {case}");
                let mut in_sub = BTreeSet::new();
                for &(m, p) in &s.prices {
                    assert!(in_sub.insert(m), "case {case}: duplicate {m}");
                    assert!(seen.insert(m), "case {case}: {m} in two chunks");
                    assert!(listed.contains(&m), "case {case}: unlisted {m}");
                    assert!(valid_price(p), "case {case}: invalid {p:?}");
                }
            }
            let expect = prices.iter().filter(|(m, p)| listed.contains(m) && valid_price(**p)).count();
            assert_eq!(seen.len(), expect, "case {case}: nothing valid dropped");
        }
    }

    #[test]
    fn nonce_is_strictly_monotonic() {
        let mut g = NonceGen::default();
        assert_eq!(g.next(1_000), 1_000);
        assert_eq!(g.next(1_000), 1_001);
        assert_eq!(g.next(900), 1_002, "clock going back never reuses a nonce");
        assert_eq!(g.next(5_000), 5_000);
        let mut last = 0;
        for now in [5_000, 5_000, 4_000, 6_000, 6_000] {
            let n = g.next(now);
            assert!(n > last);
            last = n;
        }
    }

    #[test]
    fn signed_by_the_signer_key() {
        let key = key(21);
        let s = OracleSubmission { prices: vec![(1, fp(100))], timestamp: 3 };
        let signed = sign(s.clone(), 77, &key);
        assert_eq!(signed.nonce, 77);
        assert_eq!(signed.recover_sender().unwrap(), crate::key_address(&key));
        match signed.action {
            NativeAction::SubmitOraclePrices(got) => {
                assert_eq!(got.prices.len(), 1);
                assert_eq!((got.prices[0].0, got.prices[0].1.raw()), (1, fp(100).raw()));
                assert_eq!(got.timestamp, 3);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_markets_page() {
        let page = serde_json::json!([
            {"marketId": "0x1", "baseAsset": "BTC", "quoteAsset": "USD", "lotSize": "0x1", "tickSize": "0x1", "status": "active"},
            {"marketId": "0x1f", "baseAsset": "ETH", "quoteAsset": "USD", "lotSize": "0x1", "tickSize": "0x1", "status": "active"}
        ]);
        assert_eq!(parse_markets_page(&page).unwrap(), vec![1, 31]);
        assert!(parse_markets_page(&serde_json::json!({"x": 1})).is_err());
        assert!(parse_markets_page(&serde_json::json!([{"marketId": "zz"}])).is_err());
    }

    #[tokio::test]
    async fn pages_until_short_page() {
        let calls = std::sync::Mutex::new(Vec::new());
        let all = collect_market_pages(|offset| {
            calls.lock().unwrap().push(offset);
            let n = if offset == 0 { MARKETS_PAGE } else { 3 };
            let page: Vec<MarketId> = (0..n as u64).map(|i| offset as u64 + i).collect();
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(all.len(), MARKETS_PAGE as usize + 3);
        assert_eq!(*calls.lock().unwrap(), vec![0, MARKETS_PAGE]);
        let err = collect_market_pages(|_| async { Err::<Vec<MarketId>, _>("down".to_string()) }).await;
        assert_eq!(err.unwrap_err(), "down");
    }

    // ---- main's oracle_feed.rs tests ----

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

    /// The committed key file (or `$BENCH_VALIDATOR_KEYS`), when present:
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

    /// `--walk-bp 0` (the default) is the fixed price.
    #[test]
    fn walk_zero_is_the_fixed_price_every_round() {
        let m = markets(300);
        let mut w = PriceWalk::new(&m, 30_000, 0).unwrap();
        for round in 0..100 {
            assert_eq!(w.next_round(), market_prices(&m, 30_000), "round {round}");
        }
    }

    /// Same (round, market) -> same price on every run; every round moves every
    /// market by exactly the step; never past ±8 steps; each market its own path.
    #[test]
    fn walk_is_deterministic_bounded_and_moves_every_round() {
        let m = markets(300);
        let (mut a, mut b) = (PriceWalk::new(&m, 30_000, 10).unwrap(), PriceWalk::new(&m, 30_000, 10).unwrap());
        let base = 30_000 * FixedPoint::SCALE;
        let step = base * 10 / 10_000;
        let (lo, hi) = (base - 8 * step, base + 8 * step);
        let mut prev = market_prices(&m, 30_000);
        let (mut at_bound, mut spread) = (false, false);
        for round in 0..5_000 {
            let p = a.next_round();
            assert_eq!(p, b.next_round(), "round {round}");
            assert_eq!(p.keys().copied().collect::<BTreeSet<_>>(), m);
            for (id, px) in &p {
                assert!((lo..=hi).contains(&px.raw()), "round {round} market {id}: {px:?}");
                assert_eq!((px.raw() - prev[id].raw()).abs(), step, "round {round} market {id}");
                at_bound |= px.raw() == lo || px.raw() == hi;
            }
            spread |= p.values().map(|v| v.raw()).collect::<BTreeSet<_>>().len() > 1;
            prev = p;
        }
        assert!(at_bound && spread, "the walk must reach its bound and differ across markets");
    }

    /// Offsets (bp) of market 1 over the first 12 rounds of a 10 bp walk
    /// (pinned on main, same as the ubench's `UB_MARK_WALK`).
    const WALK_10BP_MARKET_1: [i128; 12] = [10, 0, -10, -20, -30, -20, -10, 0, -10, -20, -10, 0];

    #[test]
    fn walk_offsets_are_pinned() {
        let m = markets(3);
        let mut w = PriceWalk::new(&m, 30_000, 10).unwrap();
        let base = 30_000 * FixedPoint::SCALE;
        let got: Vec<i128> = (0..12).map(|_| (w.next_round()[&1].raw() - base) * 10_000 / base).collect();
        assert_eq!(got, WALK_10BP_MARKET_1);
    }

    #[test]
    fn walk_rejects_a_bound_at_or_past_the_price() {
        let m = markets(3);
        assert!(PriceWalk::new(&m, 30_000, 1_249).is_ok());
        assert!(PriceWalk::new(&m, 30_000, 1_250).is_err());
        assert!(PriceWalk::new(&m, 30_000, u64::MAX).is_err());
    }

    /// 199bdc97's own node path (adapted from main's end-to-end test): JSON
    /// wire decode (as torus_submitNativeAction), EIP-712 validate recovers the
    /// validator, exec (`exec_submit_oracle_prices`) accepts every chunk, and
    /// `aggregate_oracle_prices` (no node caller at 199bdc97, so called
    /// directly here) yields every mark at the price with 3 reporters.
    #[test]
    fn signed_rounds_pass_the_199bdc97_node_path() {
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
                })
                .unwrap();
        }
        let m = markets(300);
        let prices = market_prices(&m, 30_000);
        let block_s = 1_700_000_000u64;
        let now = block_s * 1_000 - 1_000;

        let mut ctx = NativeExecContext::new(db.clone(), 5, block_s, 0, 1_000, 10, Address::ZERO, Address::ZERO, Address::ZERO);
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

        let stakes: Vec<(Address, FixedPoint)> = keys.iter().map(|k| (crate::key_address(k), fp(1))).collect();
        let ids: Vec<MarketId> = m.iter().copied().collect();
        let agg = NativeExecutor::aggregate_oracle_prices(&mut ctx, &ids, &stakes);
        assert_eq!(agg.len(), 300);
        assert!(agg.iter().all(|r| r.success), "{:?}", agg.iter().find(|r| !r.success).map(|r| &r.error));
        let want = FixedPoint::from_raw(30_000 * FixedPoint::SCALE);
        for id in [1, 256, 257, 300] {
            let p = ctx.oracle.get_price(id, 5).unwrap();
            assert_eq!(p.price, want, "market {id}");
            assert_eq!(p.num_reporters, 3, "market {id}");
            assert!(!p.stale, "market {id}");
        }
    }
}
