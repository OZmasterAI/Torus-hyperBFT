//! One feeder cycle: re-check, listed markets, fetch, aggregate, build, sign,
//! submit, status. `run` drives it on a fixed interval.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_primitives::Address;
use k256::ecdsa::SigningKey;
use torus_types::{FixedPoint, MarketId, NativeAction, SignedNativeAction};

use crate::config::{Config, Exchange, MarketCfg};
use crate::exchange::{HttpGet, KRAKEN_USDT_USD};
use crate::fetch::{requests, Clock, Fetcher};
use crate::node::{check_listing, startup_check, NodeApi, Readiness};
use crate::price::{aggregate, mid, Outcome, Rules, Sample};
use crate::submit::{build_submissions, classify_submit_error, sign, NonceGen, SubmitClass};

/// Validator / signer registration is re-checked this often while running.
pub const RECHECK_MS: u64 = 60_000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarketStatus {
    pub last_price: Option<FixedPoint>,
    pub sources: usize,
    pub weight: u32,
    /// Why the market was left out of the last cycle (`None` = submitted).
    pub omitted: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VenueStatus {
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub last_error: Option<String>,
    pub backoff_failures: u32,
    pub fetched_at_ms: Option<u64>,
}

/// Shared with the health server.
#[derive(Clone, Debug, Default)]
pub struct FeederStatus {
    pub signer: Address,
    pub validator: Address,
    pub interval_ms: u64,
    pub cycles_total: u64,
    /// `feeder_submit_total{result}`: ok / retryable / not_authorized / other.
    pub submit_total: BTreeMap<String, u64>,
    pub source_errors_total: BTreeMap<Exchange, u64>,
    /// `(market, reason label)` -> count.
    pub market_omitted_total: BTreeMap<(MarketId, String), u64>,
    pub last_submit_ok_ms: Option<u64>,
    /// The last NotAuthorized error; cleared by the next accepted submission.
    pub not_authorized: Option<String>,
    /// Why the feeder is idle (validator not active, signer not registered).
    pub idle: Option<String>,
    pub markets: BTreeMap<MarketId, MarketStatus>,
    pub venues: BTreeMap<Exchange, VenueStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CycleOutcome {
    Idle(String),
    MarketsUnavailable(String),
    NothingToSend,
    Submitted { ok: usize, failed: usize },
}

#[derive(Clone, Debug)]
pub struct CycleReport {
    pub outcome: CycleOutcome,
    pub prices: BTreeMap<MarketId, FixedPoint>,
    pub omitted: BTreeMap<MarketId, String>,
    pub errors: Vec<(SubmitClass, String)>,
}

impl CycleReport {
    fn new(outcome: CycleOutcome) -> Self {
        CycleReport { outcome, prices: BTreeMap::new(), omitted: BTreeMap::new(), errors: Vec::new() }
    }
}

/// The aggregation rules of `cfg`.
pub fn rules(cfg: &Config) -> Rules {
    Rules {
        min_sources: cfg.min_sources,
        min_weight_bps: cfg.min_weight_bps,
        max_age_ms: cfg.max_source_age_ms,
        quote_mode: cfg.quote_mode,
    }
}

/// The fetched Kraken USDT/USD mid, if fresh.
pub fn usdt_usd(cfg: &Config, fetcher: &Fetcher, now_ms: u64) -> Option<FixedPoint> {
    let v = fetcher.venue(Exchange::Kraken)?;
    if !v.is_fresh(now_ms, cfg.max_source_age_ms) {
        return None;
    }
    let &(b, a) = v.quotes.get(KRAKEN_USDT_USD)?;
    mid(b, a)
}

/// Market `m`'s samples from the last fetched quotes (any age; `aggregate`
/// applies freshness), plus the `venue:symbol`s with no usable quote.
pub fn market_samples(cfg: &Config, fetcher: &Fetcher, m: &MarketCfg) -> (Vec<Sample>, Vec<String>) {
    let mut samples = Vec::new();
    let mut missing = Vec::new();
    for (ex, sym, quote) in cfg.venues_for(m) {
        let s = fetcher.venue(ex).and_then(|v| {
            let &(b, a) = v.quotes.get(sym)?;
            Some(Sample { exchange: ex, mid: mid(b, a)?, quote, fetched_at_ms: v.fetched_at_ms? })
        });
        match s {
            Some(s) => samples.push(s),
            None => missing.push(format!("{}:{sym}", ex.name())),
        }
    }
    (samples, missing)
}

pub struct Feeder<H, N, C> {
    cfg: Config,
    http: Arc<H>,
    node: N,
    clock: C,
    key: SigningKey,
    signer: Address,
    fetcher: Fetcher,
    nonce: NonceGen,
    last_check_ms: Option<u64>,
    idle: Option<String>,
    status: Arc<Mutex<FeederStatus>>,
}

/// Why a market was left out of a cycle, with what it had.
struct Omission {
    reason: String,
    sources: usize,
    weight: u32,
}

fn omit_label(reason: &str) -> String {
    reason.split(" (").next().unwrap_or(reason).replace(' ', "_")
}

impl<H: HttpGet + 'static, N: NodeApi, C: Clock> Feeder<H, N, C> {
    pub fn new(cfg: Config, http: Arc<H>, node: N, clock: C, key: SigningKey) -> Self {
        let signer = torus_wallet::keystore::address_from_key(&key);
        let status = FeederStatus {
            signer,
            validator: cfg.validator_address,
            interval_ms: cfg.interval_ms,
            ..Default::default()
        };
        Feeder {
            cfg,
            http,
            node,
            clock,
            key,
            signer,
            fetcher: Fetcher::default(),
            nonce: NonceGen::default(),
            last_check_ms: None,
            idle: None,
            status: Arc::new(Mutex::new(status)),
        }
    }

    pub fn signer(&self) -> Address {
        self.signer
    }

    pub fn status(&self) -> Arc<Mutex<FeederStatus>> {
        self.status.clone()
    }

    /// Aggregate every configured market that is listed (and matches its listing).
    fn aggregate_all(
        &self,
        listed: &BTreeSet<MarketId>,
        now: u64,
    ) -> (BTreeMap<MarketId, FixedPoint>, BTreeMap<MarketId, Omission>) {
        let rules = rules(&self.cfg);
        let rate = usdt_usd(&self.cfg, &self.fetcher, now);
        let mut prices = BTreeMap::new();
        let mut omitted = BTreeMap::new();
        for m in &self.cfg.markets {
            if !listed.contains(&m.market_id) {
                omitted.insert(m.market_id, Omission { reason: "not listed".into(), sources: 0, weight: 0 });
                continue;
            }
            let (samples, _) = market_samples(&self.cfg, &self.fetcher, m);
            match aggregate(&samples, &self.cfg.weights_for(m), rate, now, &rules) {
                Outcome::Price { price, sources, weight } => {
                    prices.insert(m.market_id, price);
                    let mut st = self.status.lock().unwrap();
                    st.markets.insert(m.market_id, MarketStatus { last_price: Some(price), sources, weight, omitted: None });
                }
                o => {
                    let (sources, weight) = match o {
                        Outcome::TooFewSources { have, .. } => (have, 0),
                        Outcome::TooLittleWeight { have, .. } => (0, have),
                        Outcome::Price { .. } => unreachable!(),
                    };
                    omitted.insert(m.market_id, Omission { reason: o.reason().unwrap_or_default(), sources, weight });
                }
            }
        }
        (prices, omitted)
    }

    /// One cycle. Never panics on node or venue errors; they land in the report
    /// and the status.
    pub async fn run_cycle(&mut self) -> CycleReport {
        let now = self.clock.now_ms();
        self.status.lock().unwrap().cycles_total += 1;

        // 1. Registration re-check (startup and every 60 s).
        if self.last_check_ms.is_none_or(|t| now.saturating_sub(t) >= RECHECK_MS) {
            self.last_check_ms = Some(now);
            self.idle = match startup_check(&self.cfg, &self.node, self.signer).await {
                Ok(r) => {
                    for w in &r.warnings {
                        tracing::warn!("{w}");
                    }
                    match r.readiness {
                        Readiness::Ready => None,
                        Readiness::Idle(m) => Some(m),
                    }
                }
                Err(e) => Some(e),
            };
            if let Some(m) = &self.idle {
                tracing::warn!("feeder idle: {m}");
            }
        }
        if let Some(m) = self.idle.clone() {
            self.status.lock().unwrap().idle = Some(m.clone());
            return CycleReport::new(CycleOutcome::Idle(m));
        }
        self.status.lock().unwrap().idle = None;

        // 2. Listed markets, every cycle.
        let markets = match self.node.listed_markets().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("getMarkets failed, cycle skipped: {e}");
                return CycleReport::new(CycleOutcome::MarketsUnavailable(e));
            }
        };
        let listed: BTreeSet<MarketId> = self
            .cfg
            .markets
            .iter()
            .filter(|m| {
                markets
                    .iter()
                    .find(|x| x.id == m.market_id)
                    .is_some_and(|x| check_listing(m.market_id, &m.base_asset, x).is_ok())
            })
            .map(|m| m.market_id)
            .collect();

        // 3. Venues.
        let timeout = Duration::from_millis(self.cfg.fetch_timeout_ms);
        let reqs = requests(&self.cfg);
        self.fetcher.fetch_all(&self.http, &self.clock, &reqs, timeout).await;
        let now = self.clock.now_ms();
        self.record_venues();

        // 4. Aggregate.
        let (prices, omitted) = self.aggregate_all(&listed, now);
        let mut report = CycleReport::new(CycleOutcome::NothingToSend);
        {
            let mut st = self.status.lock().unwrap();
            for (m, Omission { reason, sources, weight }) in &omitted {
                *st.market_omitted_total.entry((*m, omit_label(reason))).or_default() += 1;
                let e = st.markets.entry(*m).or_default();
                e.sources = *sources;
                e.weight = *weight;
                e.omitted = Some(reason.clone());
            }
        }
        report.omitted = omitted.into_iter().map(|(m, o)| (m, o.reason)).collect();
        report.prices = prices.clone();

        // 5. Build (never invalid).
        let subs = build_submissions(&prices, &listed, now);
        if subs.is_empty() {
            return report;
        }

        // 6. Sign + submit + classify.
        let (mut ok, mut failed) = (0, 0);
        for sub in subs {
            let nonce = self.nonce.next(self.clock.now_ms());
            let signed: SignedNativeAction = sign(sub, nonce, &self.key);
            debug_assert!(matches!(signed.action, NativeAction::SubmitOraclePrices(_)));
            match self.node.submit(&signed).await {
                Ok(hash) => {
                    ok += 1;
                    tracing::debug!(%hash, nonce, "oracle submission accepted");
                    let mut st = self.status.lock().unwrap();
                    *st.submit_total.entry("ok".into()).or_default() += 1;
                    st.last_submit_ok_ms = Some(self.clock.now_ms());
                    st.not_authorized = None;
                }
                Err(e) => {
                    failed += 1;
                    let class = classify_submit_error(&e);
                    tracing::warn!(?class, nonce, "oracle submission rejected: {e}");
                    let mut st = self.status.lock().unwrap();
                    *st.submit_total.entry(class.label().into()).or_default() += 1;
                    if class == SubmitClass::NotAuthorized {
                        st.not_authorized = Some(e.clone());
                    }
                    report.errors.push((class, e));
                }
            }
        }
        report.outcome = CycleOutcome::Submitted { ok, failed };
        report
    }

    fn record_venues(&self) {
        let mut st = self.status.lock().unwrap();
        for (ex, v) in self.fetcher.venues() {
            st.source_errors_total.insert(*ex, v.errors_total);
            st.venues.insert(
                *ex,
                VenueStatus {
                    ok: v.last_error.is_none() && v.fetched_at_ms.is_some(),
                    latency_ms: v.latency_ms,
                    last_error: v.last_error.clone(),
                    backoff_failures: v.backoff.failures,
                    fetched_at_ms: v.fetched_at_ms,
                },
            );
        }
    }

    /// Drive cycles every `interval_ms` (missed ticks are skipped) until
    /// `shutdown` resolves.
    pub async fn run(mut self, shutdown: impl Future<Output = ()>) {
        let mut iv = tokio::time::interval(Duration::from_millis(self.cfg.interval_ms));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = iv.tick() => {
                    let r = self.run_cycle().await;
                    tracing::info!(outcome = ?r.outcome, prices = r.prices.len(), omitted = r.omitted.len(), "cycle");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Exchange};
    use crate::fetch::FakeClock;
    use crate::node::{MarketInfo, ValidatorInfo};
    use crate::testing::{FakeHttp, FakeNode};
    use crate::price::parse_price;

    const V: &str = "0x1111111111111111111111111111111111111111";

    fn key() -> SigningKey {
        SigningKey::from_slice(&[21u8; 32]).unwrap()
    }

    fn signer() -> Address {
        torus_wallet::keystore::address_from_key(&key())
    }

    /// Markets 1 BTC and 2 ETH on all 7 venues (+ optional extra TOML).
    fn cfg(extra_markets: &str) -> Config {
        let mut t = format!("rpc_url = \"x\"\nsigner_key_file = \"/tmp/k\"\nvalidator_address = \"{V}\"\n");
        for ex in Exchange::ALL {
            t.push_str(&format!("[exchanges.{}]\nbase_url = \"fake://{}\"\n", ex.name(), ex.name()));
        }
        t.push_str(
            "[[markets]]\nmarket_id = 1\nbase_asset = \"BTC\"\nsymbols = { binance = \"BTCUSDT\", okx = \"BTC-USDT\", bybit = \"BTCUSDT\", kraken = \"XXBTZUSD\", kucoin = \"BTC-USDT\", gate = \"BTC_USDT\", mexc = \"BTCUSDT\" }\n\
             [[markets]]\nmarket_id = 2\nbase_asset = \"ETH\"\nsymbols = { binance = \"ETHUSDT\", okx = \"ETH-USDT\", bybit = \"ETHUSDT\", kraken = \"XETHZUSD\", kucoin = \"ETH-USDT\", gate = \"ETH_USDT\", mexc = \"ETHUSDT\" }\n",
        );
        t.push_str(extra_markets);
        Config::parse(&t).unwrap()
    }

    fn markets(ids: &[(u64, &str)]) -> Vec<MarketInfo> {
        ids.iter().map(|&(id, b)| MarketInfo { id, base_asset: b.into(), quote_asset: "USD".into() }).collect()
    }

    fn node(status: &str) -> FakeNode {
        let n = FakeNode::default();
        n.set_validators(vec![ValidatorInfo { address: V.parse().unwrap(), status: status.into(), oracle_signer: Some(signer()) }]);
        n.set_markets(Ok(markets(&[(1, "BTC"), (2, "ETH")])));
        n
    }

    type F = Feeder<FakeHttp, FakeNode, FakeClock>;

    fn feeder(c: Config, n: &FakeNode, http: &Arc<FakeHttp>, clock: &FakeClock) -> F {
        Feeder::new(c, http.clone(), n.clone(), clock.clone(), key())
    }

    fn prices(s: &SignedNativeAction) -> Vec<(MarketId, FixedPoint)> {
        match &s.action {
            NativeAction::SubmitOraclePrices(sub) => sub.prices.clone(),
            other => panic!("{other:?}"),
        }
    }

    const T0: u64 = 1_790_000_000_000;

    /// Fixture weighted medians (par): BTC = Binance mid 83497.685 (cum weight 7
    /// of 11), ETH = Bybit mid 2683.325.
    #[tokio::test]
    async fn happy_cycle_submits_weighted_medians_once() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        let r = f.run_cycle().await;
        assert_eq!(r.outcome, CycleOutcome::Submitted { ok: 1, failed: 0 }, "{r:?}");
        let sent = n.submitted();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].recover_sender().unwrap(), signer(), "signed by the hot signer");
        assert_eq!(prices(&sent[0]), vec![(1, parse_price("83497.685").unwrap()), (2, parse_price("2683.325").unwrap())]);
        assert_eq!(sent[0].nonce, T0);
        let st = f.status();
        let st = st.lock().unwrap();
        assert_eq!(st.last_submit_ok_ms, Some(T0));
        assert_eq!(st.markets[&1].sources, 7);
        assert_eq!(st.markets[&1].weight, 11);
        assert_eq!(st.cycles_total, 1);
        assert!(st.not_authorized.is_none());
        for ex in Exchange::ALL {
            assert_eq!(http.calls_to(&format!("fake://{}", ex.name())), 1, "{ex:?}: one bulk request");
        }
    }

    #[tokio::test]
    async fn omits_only_the_starved_market() {
        let extra = "[[markets]]\nmarket_id = 3\nbase_asset = \"DOGE\"\nsymbols = { binance = \"BTCUSDT\", okx = \"NOPE-USDT\", bybit = \"NOPEUSDT\" }\n";
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        n.set_markets(Ok(markets(&[(1, "BTC"), (2, "ETH"), (3, "DOGE")])));
        let mut f = feeder(cfg(extra), &n, &http, &clock);
        let r = f.run_cycle().await;
        assert_eq!(r.outcome, CycleOutcome::Submitted { ok: 1, failed: 0 });
        assert!(r.omitted[&3].contains("sources"), "{:?}", r.omitted);
        assert_eq!(prices(&n.submitted()[0]).iter().map(|p| p.0).collect::<Vec<_>>(), vec![1, 2]);
        let st = f.status();
        let st = st.lock().unwrap();
        assert!(st.markets[&3].omitted.as_deref().unwrap().contains("sources"));
        assert_eq!(st.market_omitted_total.values().sum::<u64>(), 1);
    }

    #[tokio::test]
    async fn unlisted_or_all_omitted_sends_nothing() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        n.set_markets(Ok(markets(&[(7, "DOGE")])));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        assert_eq!(f.run_cycle().await.outcome, CycleOutcome::NothingToSend);
        assert!(n.submitted().is_empty());

        let http = Arc::new(FakeHttp::default()); // every venue errors
        let n = node("active");
        let mut f = feeder(cfg(""), &n, &http, &clock);
        let r = f.run_cycle().await;
        assert_eq!(r.outcome, CycleOutcome::NothingToSend);
        assert_eq!(r.omitted.len(), 2);
        assert!(n.submitted().is_empty());
        assert_eq!(f.status().lock().unwrap().source_errors_total.values().sum::<u64>(), 7);
    }

    #[tokio::test]
    async fn stale_cache_is_not_used() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        f.run_cycle().await;
        assert_eq!(n.submitted().len(), 1);
        for ex in Exchange::ALL {
            http.route(&format!("fake://{}", ex.name()), Err("down".into()));
        }
        clock.set(T0 + 3_000); // cached quotes (age 3000) are still fresh
        assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::Submitted { ok: 1, .. }));
        clock.set(T0 + 5_001); // age 5001: stale
        assert_eq!(f.run_cycle().await.outcome, CycleOutcome::NothingToSend);
        assert_eq!(n.submitted().len(), 2);
    }

    #[tokio::test]
    async fn busy_is_retried_next_cycle_with_higher_nonce() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        n.push_submit_result(Err("RPC error: node busy, retry".into()));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        let r = f.run_cycle().await;
        assert_eq!(r.outcome, CycleOutcome::Submitted { ok: 0, failed: 1 });
        assert_eq!(r.errors[0].0, SubmitClass::Retryable);
        assert!(f.status().lock().unwrap().not_authorized.is_none());
        clock.set(T0); // even with a stuck clock the nonce advances
        f.run_cycle().await;
        let sent = n.submitted();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].nonce > sent[0].nonce);
        assert_eq!(f.status().lock().unwrap().last_submit_ok_ms, Some(T0));
        assert_eq!(f.status().lock().unwrap().submit_total.get("retryable"), Some(&1));
    }

    #[tokio::test]
    async fn not_authorized_marks_health_down() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        n.push_submit_result(Err("oracle submission from 0x..: not an active validator or its signer".into()));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        let r = f.run_cycle().await;
        assert_eq!(r.errors[0].0, SubmitClass::NotAuthorized);
        assert!(f.status().lock().unwrap().not_authorized.is_some());
        f.run_cycle().await; // the next accepted submission clears it
        assert!(f.status().lock().unwrap().not_authorized.is_none());
    }

    #[tokio::test]
    async fn not_active_validator_idles() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("jailed"));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::Idle(ref m) if m.contains("jailed")));
        assert_eq!(http.calls().len(), 0, "an idle feeder fetches nothing");
        assert!(n.submitted().is_empty());
        assert!(f.status().lock().unwrap().idle.is_some());
        // Activated: picked up at the next 60 s re-check, not before.
        n.set_validators(vec![ValidatorInfo { address: V.parse().unwrap(), status: "active".into(), oracle_signer: Some(signer()) }]);
        clock.set(T0 + 59_999);
        assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::Idle(_)));
        clock.set(T0 + 60_000);
        assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::Submitted { ok: 1, .. }));
        assert!(f.status().lock().unwrap().idle.is_none());
        // Signer deregistered: idle at the next re-check, with the fix command.
        n.set_validators(vec![ValidatorInfo { address: V.parse().unwrap(), status: "active".into(), oracle_signer: None }]);
        clock.set(T0 + 120_000);
        assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::Idle(ref m) if m.contains("set-oracle-signer")));
    }

    #[tokio::test]
    async fn markets_refreshed_each_cycle() {
        let (http, clock, n) = (Arc::new(FakeHttp::with_fixtures()), FakeClock::new(T0), node("active"));
        let mut f = feeder(cfg(""), &n, &http, &clock);
        f.run_cycle().await;
        let calls = n.market_calls();
        n.set_markets(Ok(markets(&[(1, "BTC")]))); // market 2 delisted
        clock.set(T0 + 3_000);
        f.run_cycle().await;
        assert_eq!(n.market_calls(), calls + 1, "listed markets fetched every cycle");
        assert_eq!(prices(&n.submitted()[1]).iter().map(|p| p.0).collect::<Vec<_>>(), vec![1]);
        // Node unreachable for markets: the cycle is skipped.
        n.set_markets(Err("connection refused".into()));
        clock.set(T0 + 6_000);
        assert!(matches!(f.run_cycle().await.outcome, CycleOutcome::MarketsUnavailable(_)));
        assert_eq!(n.submitted().len(), 2);
    }
}
