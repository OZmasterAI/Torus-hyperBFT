//! Concurrent per-venue fetch with a timeout, per-venue backoff, and a clock seam.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{Config, Exchange};
use crate::exchange::{parse_quotes, url_for, HttpGet, Quotes, KRAKEN_UNKNOWN_PAIR};

/// Wall clock in unix ms (the only time source of a cycle).
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A settable clock for tests (shared between clones).
#[derive(Clone, Debug, Default)]
pub struct FakeClock(pub Arc<AtomicU64>);

impl FakeClock {
    pub fn new(ms: u64) -> Self {
        FakeClock(Arc::new(AtomicU64::new(ms)))
    }
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Per-venue backoff: after k consecutive failures wait `min(1 s · 2^(k-1), 60 s)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Backoff {
    pub failures: u32,
    /// Earliest unix ms of the next attempt (0 = now).
    pub next_ms: u64,
}

impl Backoff {
    pub fn delay_ms(failures: u32) -> u64 {
        if failures == 0 {
            return 0;
        }
        (1_000u64 << (failures - 1).min(6)).min(60_000)
    }
    pub fn fail(&mut self, now_ms: u64) {
        self.failures = self.failures.saturating_add(1);
        self.next_ms = now_ms.saturating_add(Self::delay_ms(self.failures));
    }
    pub fn ok(&mut self) {
        *self = Backoff::default();
    }
    pub fn ready(&self, now_ms: u64) -> bool {
        now_ms >= self.next_ms
    }
}

/// Last known state of one venue. A failing venue keeps its last good quotes
/// (stamped `fetched_at_ms`); they age out by the freshness rule.
#[derive(Clone, Debug, Default)]
pub struct VenueState {
    pub quotes: Quotes,
    pub fetched_at_ms: Option<u64>,
    pub backoff: Backoff,
    pub last_error: Option<String>,
    pub latency_ms: Option<u64>,
    pub errors_total: u64,
}

impl VenueState {
    pub fn is_fresh(&self, now_ms: u64, max_age_ms: u64) -> bool {
        self.fetched_at_ms.is_some_and(|t| now_ms.saturating_sub(t) <= max_age_ms)
    }
}

#[derive(Debug, Default)]
pub struct Fetcher {
    venues: BTreeMap<Exchange, VenueState>,
}

impl Fetcher {
    pub fn venue(&self, ex: Exchange) -> Option<&VenueState> {
        self.venues.get(&ex)
    }

    pub fn venues(&self) -> &BTreeMap<Exchange, VenueState> {
        &self.venues
    }

    /// One request per venue that is not backed off, all concurrently, each
    /// bounded by `timeout`. Results are stamped with `clock` at completion.
    pub async fn fetch_all<H: HttpGet + 'static, C: Clock>(
        &mut self,
        http: &Arc<H>,
        clock: &C,
        requests: &[(Exchange, String)],
        timeout: Duration,
    ) {
        let start = clock.now_ms();
        let mut set = tokio::task::JoinSet::new();
        for (ex, url) in requests {
            let st = self.venues.entry(*ex).or_default();
            if !st.backoff.ready(start) {
                continue;
            }
            let (ex, url, h) = (*ex, url.clone(), http.clone());
            set.spawn(async move {
                let t0 = Instant::now();
                let r = match get_quotes(&h, ex, url.clone(), timeout).await {
                    Err(e) if ex == Exchange::Kraken && e.contains(KRAKEN_UNKNOWN_PAIR) => {
                        kraken_per_pair(&h, &url, timeout).await.ok_or(e)
                    }
                    r => r,
                };
                (ex, r, t0.elapsed().as_millis() as u64)
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok((ex, r, latency)) = joined else { continue };
            let now = clock.now_ms();
            let st = self.venues.entry(ex).or_default();
            st.latency_ms = Some(latency);
            match r {
                Ok(q) => {
                    st.quotes = q;
                    st.fetched_at_ms = Some(now);
                    st.last_error = None;
                    st.backoff.ok();
                }
                Err(e) => {
                    st.last_error = Some(e);
                    st.errors_total += 1;
                    st.backoff.fail(now);
                }
            }
        }
    }
}

async fn get_quotes<H: HttpGet>(h: &Arc<H>, ex: Exchange, url: String, timeout: Duration) -> Result<Quotes, String> {
    match tokio::time::timeout(timeout, h.get(url, timeout)).await {
        Ok(Ok(body)) => parse_quotes(ex, &body),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("timeout after {} ms", timeout.as_millis())),
    }
}

/// Review M2: Kraken fails a whole batch for ONE unknown pair; retry each pair
/// of `batch_url` (`…?pair=A,B,C`) concurrently and merge the good ones.
/// `None` if no pair succeeds (or the batch had a single pair).
async fn kraken_per_pair<H: HttpGet + 'static>(h: &Arc<H>, batch_url: &str, timeout: Duration) -> Option<Quotes> {
    let (prefix, list) = batch_url.split_once("pair=")?;
    let pairs: Vec<&str> = list.split(',').collect();
    if pairs.len() < 2 {
        return None;
    }
    let mut set = tokio::task::JoinSet::new();
    for p in pairs {
        let (h, url) = (h.clone(), format!("{prefix}pair={p}"));
        set.spawn(async move { get_quotes(&h, Exchange::Kraken, url, timeout).await });
    }
    let mut merged = Quotes::new();
    let mut any = false;
    while let Some(r) = set.join_next().await {
        if let Ok(Ok(q)) = r {
            merged.extend(q);
            any = true;
        }
    }
    any.then_some(merged)
}

/// The enabled Kraken pairs of `cfg` (plus `USDTZUSD` in `kraken_usdt` mode):
/// what `check_kraken_pairs` validates at startup.
pub fn kraken_pairs(cfg: &Config) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if !cfg.enabled(Exchange::Kraken) {
        return v;
    }
    for m in &cfg.markets {
        if let Some((sym, _)) = m.symbol(Exchange::Kraken) {
            if !v.iter().any(|s| s == sym) {
                v.push(sym.to_string());
            }
        }
    }
    if cfg.quote_mode == crate::config::QuoteMode::KrakenUsdt && !v.iter().any(|s| s == crate::exchange::KRAKEN_USDT_USD) {
        v.push(crate::exchange::KRAKEN_USDT_USD.to_string());
    }
    v
}

/// This cycle's requests: one per enabled venue that has a configured symbol
/// (Kraken also in `kraken_usdt` mode, for the USDT/USD rate).
pub fn requests(cfg: &Config) -> Vec<(Exchange, String)> {
    let mut syms: BTreeMap<Exchange, Vec<String>> = BTreeMap::new();
    for m in &cfg.markets {
        for (ex, sym, _) in cfg.venues_for(m) {
            let v = syms.entry(ex).or_default();
            if !v.iter().any(|s| s == sym) {
                v.push(sym.to_string());
            }
        }
    }
    if cfg.quote_mode == crate::config::QuoteMode::KrakenUsdt && cfg.enabled(Exchange::Kraken) {
        syms.entry(Exchange::Kraken).or_default();
    }
    syms.into_iter()
        .map(|(ex, s)| (ex, url_for(ex, &cfg.base_url(ex), &s, cfg.quote_mode)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Exchange;
    use crate::testing::FakeHttp;

    const BIN: &str = include_str!("../tests/fixtures/binance.json");

    #[test]
    fn backoff_doubles_to_60s_and_resets() {
        let mut b = Backoff::default();
        assert!(b.ready(0));
        let mut delays = Vec::new();
        for _ in 0..9 {
            b.fail(10_000);
            delays.push(b.next_ms - 10_000);
        }
        assert_eq!(delays, vec![1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 60_000]);
        assert_eq!(b.failures, 9);
        assert!(!b.ready(10_000 + 59_999));
        assert!(b.ready(10_000 + 60_000));
        b.ok();
        assert_eq!((b.failures, b.next_ms), (0, 0));
        assert!(b.ready(0));
        assert_eq!(Backoff::delay_ms(u32::MAX), 60_000, "no shift overflow");
    }

    fn req(ex: Exchange) -> (Exchange, String) {
        (ex, format!("fake://{}/q", ex.name()))
    }

    #[tokio::test]
    async fn fetch_all_skips_backed_off_venues() {
        let http = Arc::new(FakeHttp::default());
        http.route("fake://binance", Err("boom".into()));
        http.route("fake://okx", Ok(BIN.to_string())); // any body: okx parse fails -> also a failure
        let clock = FakeClock::new(0);
        let mut f = Fetcher::default();
        let reqs = vec![req(Exchange::Binance)];
        let t = Duration::from_millis(2_000);
        f.fetch_all(&http, &clock, &reqs, t).await;
        assert_eq!(http.calls_to("fake://binance"), 1);
        assert_eq!(f.venue(Exchange::Binance).unwrap().backoff.failures, 1);
        clock.set(500);
        f.fetch_all(&http, &clock, &reqs, t).await;
        assert_eq!(http.calls_to("fake://binance"), 1, "backed off at +500 ms");
        clock.set(1_000);
        f.fetch_all(&http, &clock, &reqs, t).await;
        assert_eq!(http.calls_to("fake://binance"), 2, "retried at +1000 ms");
        assert_eq!(f.venue(Exchange::Binance).unwrap().backoff.failures, 2);
        assert_eq!(f.venue(Exchange::Binance).unwrap().errors_total, 2);
        // A parse failure is a venue failure too.
        f.fetch_all(&http, &clock, &[req(Exchange::Okx)], t).await;
        let okx = f.venue(Exchange::Okx).unwrap();
        assert_eq!(okx.backoff.failures, 1);
        assert!(okx.last_error.is_some());
    }

    #[tokio::test]
    async fn failed_venue_keeps_last_quotes_which_age_out() {
        let http = Arc::new(FakeHttp::default());
        http.route("fake://binance", Ok(BIN.to_string()));
        let clock = FakeClock::new(1_000);
        let mut f = Fetcher::default();
        let reqs = vec![req(Exchange::Binance)];
        let t = Duration::from_millis(2_000);
        f.fetch_all(&http, &clock, &reqs, t).await;
        let v = f.venue(Exchange::Binance).unwrap();
        assert_eq!(v.fetched_at_ms, Some(1_000));
        assert!(v.quotes.contains_key("BTCUSDT"));
        assert!(v.last_error.is_none());
        assert!(v.latency_ms.is_some());

        http.route("fake://binance", Err("down".into()));
        clock.set(4_000);
        f.fetch_all(&http, &clock, &reqs, t).await;
        let v = f.venue(Exchange::Binance).unwrap();
        assert_eq!(v.fetched_at_ms, Some(1_000), "last good quotes kept with their stamp");
        assert!(v.quotes.contains_key("BTCUSDT"));
        assert_eq!(v.last_error.as_deref(), Some("down"));
        // Ageing is by the stamp: fresh through 6000, gone at 6001.
        assert!(v.is_fresh(6_000, 5_000));
        assert!(!v.is_fresh(6_001, 5_000));
        let never = VenueState::default();
        assert!(!never.is_fresh(0, 5_000));

        // Success resets the backoff.
        http.route("fake://binance", Ok(BIN.to_string()));
        clock.set(10_000);
        f.fetch_all(&http, &clock, &reqs, t).await;
        let v = f.venue(Exchange::Binance).unwrap();
        assert_eq!((v.backoff.failures, v.fetched_at_ms), (0, Some(10_000)));
    }

    #[tokio::test]
    async fn timeout_counts_as_failure() {
        let http = Arc::new(FakeHttp::default());
        http.route_delayed("fake://binance", Ok(BIN.to_string()), Duration::from_millis(500));
        let clock = FakeClock::new(0);
        let mut f = Fetcher::default();
        let started = std::time::Instant::now();
        f.fetch_all(&http, &clock, &[req(Exchange::Binance)], Duration::from_millis(30)).await;
        assert!(started.elapsed() < Duration::from_millis(400), "did not wait for the slow venue");
        let v = f.venue(Exchange::Binance).unwrap();
        assert_eq!(v.backoff.failures, 1);
        assert!(v.last_error.as_deref().unwrap().contains("timeout"), "{:?}", v.last_error);
        assert_eq!(v.fetched_at_ms, None);
    }

    #[tokio::test]
    async fn venues_are_fetched_concurrently() {
        let http = Arc::new(FakeHttp::default());
        for ex in [Exchange::Binance, Exchange::Mexc] {
            http.route_delayed(&format!("fake://{}", ex.name()), Ok(BIN.to_string()), Duration::from_millis(200));
        }
        let clock = FakeClock::new(0);
        let mut f = Fetcher::default();
        let started = std::time::Instant::now();
        f.fetch_all(&http, &clock, &[req(Exchange::Binance), req(Exchange::Mexc)], Duration::from_secs(2)).await;
        assert!(started.elapsed() < Duration::from_millis(390), "{:?}", started.elapsed());
        assert!(f.venue(Exchange::Mexc).unwrap().fetched_at_ms.is_some());
    }

    /// Review M2: Kraken answers a batch with ONE unknown pair by failing the
    /// whole request; the venue then falls back to one request per pair and
    /// keeps the good ones (recorded fixtures).
    #[tokio::test]
    async fn kraken_unknown_pair_falls_back_per_pair() {
        let unknown = include_str!("../tests/fixtures/kraken_unknown_pair.json");
        let one = include_str!("../tests/fixtures/kraken_ticker_one.json");
        let http = Arc::new(FakeHttp::default());
        let base = "fake://kraken/0/public/Ticker?pair=";
        http.route(&format!("{base}XXBTZUSD,NOPEUSD"), Ok(unknown.to_string()));
        http.route(&format!("{base}XXBTZUSD"), Ok(one.to_string()));
        http.route(&format!("{base}NOPEUSD"), Ok(unknown.to_string()));
        let clock = FakeClock::new(1_000);
        let mut f = Fetcher::default();
        let req = (Exchange::Kraken, format!("{base}XXBTZUSD,NOPEUSD"));
        f.fetch_all(&http, &clock, &[req], Duration::from_secs(2)).await;
        let v = f.venue(Exchange::Kraken).unwrap();
        assert!(v.quotes.contains_key("XXBTZUSD"), "{:?}", v.last_error);
        assert_eq!(v.backoff.failures, 0, "a partial success is a success");
        assert_eq!(v.fetched_at_ms, Some(1_000));
        assert_eq!(http.calls().len(), 3, "batch + one per pair");
        // Every pair unknown: still a failure.
        let http = Arc::new(FakeHttp::default());
        http.route(&format!("{base}NOPEUSD"), Ok(unknown.to_string()));
        let mut f = Fetcher::default();
        f.fetch_all(&http, &clock, &[(Exchange::Kraken, format!("{base}NOPEUSD"))], Duration::from_secs(2)).await;
        assert_eq!(f.venue(Exchange::Kraken).unwrap().backoff.failures, 1);
    }
}
