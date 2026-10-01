//! Venue URL builders, PURE response parsers, and the `HttpGet` seam.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use crate::config::{Exchange, QuoteMode};
use crate::price::{parse_price, FixedPoint};

/// symbol -> (bid, ask) as quoted by the venue.
pub type Quotes = HashMap<String, (FixedPoint, FixedPoint)>;

/// Kraken's USDT/USD pair, fetched in `kraken_usdt` mode.
pub const KRAKEN_USDT_USD: &str = "USDTZUSD";

/// Response bodies above this are rejected (bulk venues return ~0.5-1 MB).
pub const MAX_BODY_BYTES: usize = 4 << 20;

/// One GET with a timeout. The only network seam of the feeder (tests use
/// `testing::FakeHttp`).
pub trait HttpGet: Send + Sync {
    fn get(&self, url: String, timeout: Duration) -> impl Future<Output = Result<String, String>> + Send;
}

/// The one bulk request for `ex` this cycle. Kraken takes the pair list
/// (with a per-pair fallback, `fetch`); every other venue returns all spot
/// tickers. Review M2: Binance is unfiltered too — a `symbols=[..]` filter
/// fails the whole request when one symbol is unknown or delisted.
pub fn url_for(ex: Exchange, base_url: &str, symbols: &[String], mode: QuoteMode) -> String {
    let base = base_url.trim_end_matches('/');
    match ex {
        Exchange::Binance => format!("{base}/api/v3/ticker/bookTicker"),
        Exchange::Kraken => {
            let mut pairs: Vec<&str> = symbols.iter().map(String::as_str).collect();
            if mode == QuoteMode::KrakenUsdt && !pairs.contains(&KRAKEN_USDT_USD) {
                pairs.push(KRAKEN_USDT_USD);
            }
            format!("{base}/0/public/Ticker?pair={}", pairs.join(","))
        }
        Exchange::Okx => format!("{base}/api/v5/market/tickers?instType=SPOT"),
        Exchange::Bybit => format!("{base}/v5/market/tickers?category=spot"),
        Exchange::Kucoin => format!("{base}/api/v1/market/allTickers"),
        Exchange::Gate => format!("{base}/api/v4/spot/tickers"),
        Exchange::Mexc => format!("{base}/api/v3/ticker/bookTicker"),
    }
}

/// Kraken's error for a request naming an unknown pair (the whole batch fails).
pub const KRAKEN_UNKNOWN_PAIR: &str = "Unknown asset pair";

/// Review M2: every configured Kraken pair must exist under its CANONICAL
/// AssetPairs key — the Ticker response is keyed by it, so an altname
/// (`XBTUSD`) would silently never match. Fatal at startup (`check` / `run`).
pub async fn check_kraken_pairs<H: HttpGet>(
    http: &H,
    base_url: &str,
    pairs: &[String],
    timeout: Duration,
) -> Result<(), String> {
    if pairs.is_empty() {
        return Ok(());
    }
    let base = base_url.trim_end_matches('/');
    let keys = |body: Result<String, String>| -> Result<Vec<String>, String> {
        let v: Value = serde_json::from_str(&body?).map_err(|e| format!("bad json: {e}"))?;
        match v.get("error").and_then(Value::as_array) {
            Some(e) if e.is_empty() => {}
            _ => return Err(format!("error {}", truncate(&v))),
        }
        let r = v.get("result").and_then(Value::as_object).ok_or("no result")?;
        Ok(r.keys().cloned().collect())
    };
    let url = |p: &str| format!("{base}/0/public/AssetPairs?pair={p}");
    match keys(http.get(url(&pairs.join(",")), timeout).await) {
        Ok(found) => {
            let bad: Vec<&String> = pairs.iter().filter(|p| !found.contains(p)).collect();
            if bad.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "kraken pairs {bad:?} are not canonical AssetPairs keys (Kraken returned {found:?}); \
                     configure the canonical names"
                ))
            }
        }
        Err(e) if e.contains(KRAKEN_UNKNOWN_PAIR) => {
            let mut unknown = Vec::new();
            for p in pairs {
                if keys(http.get(url(p), timeout).await).is_err() {
                    unknown.push(p.clone());
                }
            }
            Err(format!("kraken: unknown pair(s) {unknown:?} in the config"))
        }
        Err(e) => Err(format!("kraken AssetPairs: {e}")),
    }
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Collect `(symbol, bid, ask)` rows; a row with a missing or unparsable field
/// is dropped alone.
fn rows<'a>(items: impl Iterator<Item = (Option<&'a str>, Option<&'a str>, Option<&'a str>)>) -> Quotes {
    items
        .filter_map(|(sym, bid, ask)| Some((sym?.to_string(), (parse_price(bid?)?, parse_price(ask?)?))))
        .collect()
}

fn array<'a>(v: &'a Value, what: &str) -> Result<&'a Vec<Value>, String> {
    v.as_array().ok_or_else(|| format!("{what}: expected an array, got {}", truncate(v)))
}

fn truncate(v: &Value) -> String {
    let mut t = v.to_string();
    t.truncate(200);
    t
}

/// Parse one venue response. Error envelopes and malformed JSON are `Err`.
pub fn parse_quotes(ex: Exchange, body: &str) -> Result<Quotes, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("{}: bad json: {e}", ex.name()))?;
    let name = ex.name();
    Ok(match ex {
        Exchange::Binance | Exchange::Mexc => {
            let a = array(&v, name)?;
            rows(a.iter().map(|t| (s(t, "symbol"), s(t, "bidPrice"), s(t, "askPrice"))))
        }
        Exchange::Okx => {
            if s(&v, "code") != Some("0") {
                return Err(format!("okx: error {}", truncate(&v)));
            }
            let a = v.get("data").ok_or("okx: no data")?;
            rows(array(a, name)?.iter().map(|t| (s(t, "instId"), s(t, "bidPx"), s(t, "askPx"))))
        }
        Exchange::Bybit => {
            if v.get("retCode").and_then(Value::as_i64) != Some(0) {
                return Err(format!("bybit: error {}", truncate(&v)));
            }
            let a = v.pointer("/result/list").ok_or("bybit: no result.list")?;
            rows(array(a, name)?.iter().map(|t| (s(t, "symbol"), s(t, "bid1Price"), s(t, "ask1Price"))))
        }
        Exchange::Kraken => {
            match v.get("error").and_then(Value::as_array) {
                Some(e) if e.is_empty() => {}
                _ => return Err(format!("kraken: error {}", truncate(&v))),
            }
            let r = v.get("result").and_then(Value::as_object).ok_or("kraken: no result")?;
            let first = |t: &Value, k: &str| t.get(k).and_then(|x| x.get(0)).and_then(Value::as_str).map(str::to_owned);
            r.iter()
                .filter_map(|(k, t)| {
                    let (b, a) = (first(t, "b")?, first(t, "a")?);
                    Some((k.clone(), (parse_price(&b)?, parse_price(&a)?)))
                })
                .collect()
        }
        Exchange::Kucoin => {
            if s(&v, "code") != Some("200000") {
                return Err(format!("kucoin: error {}", truncate(&v)));
            }
            let a = v.pointer("/data/ticker").ok_or("kucoin: no data.ticker")?;
            rows(array(a, name)?.iter().map(|t| (s(t, "symbol"), s(t, "buy"), s(t, "sell"))))
        }
        Exchange::Gate => {
            let a = array(&v, name)?;
            rows(a.iter().map(|t| (s(t, "currency_pair"), s(t, "highest_bid"), s(t, "lowest_ask"))))
        }
    })
}

/// The production `HttpGet`: rustls reqwest, HTTP status checked, body capped.
#[derive(Clone)]
pub struct ReqwestGet {
    client: reqwest::Client,
}

impl ReqwestGet {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("torus-price-feeder/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

impl HttpGet for ReqwestGet {
    async fn get(&self, url: String, timeout: Duration) -> Result<String, String> {
        let mut resp = self.client.get(&url).timeout(timeout).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        if resp.content_length().is_some_and(|n| n > MAX_BODY_BYTES as u64) {
            return Err(format!("body over {MAX_BODY_BYTES} bytes"));
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(format!("body over {MAX_BODY_BYTES} bytes"));
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Exchange, QuoteMode};

    use crate::testing::fixture as fx;

    fn p(s: &str) -> FixedPoint {
        crate::price::parse_price(s).unwrap()
    }

    /// Recorded once with curl (s517); BTC bid/ask per venue.
    #[test]
    fn every_venue_parses_its_fixture() {
        let btc = [
            (Exchange::Binance, "BTCUSDT", "83497.68", "83497.69"),
            (Exchange::Okx, "BTC-USDT", "83496", "83496.1"),
            (Exchange::Bybit, "BTCUSDT", "83515", "83515.1"),
            (Exchange::Kraken, "XXBTZUSD", "83451.8", "83451.9"),
            (Exchange::Kucoin, "BTC-USDT", "83495.3", "83495.4"),
            (Exchange::Gate, "BTC_USDT", "83500.1", "83500.2"),
            (Exchange::Mexc, "BTCUSDT", "83505.02", "83505.03"),
        ];
        for (ex, sym, bid, ask) in btc {
            let q = parse_quotes(ex, fx(ex)).unwrap_or_else(|e| panic!("{ex:?}: {e}"));
            assert_eq!(q.get(sym), Some(&(p(bid), p(ask))), "{ex:?}");
            assert!(q.len() >= 3, "{ex:?}: {q:?}");
        }
        let k = parse_quotes(Exchange::Kraken, fx(Exchange::Kraken)).unwrap();
        assert_eq!(k.get(KRAKEN_USDT_USD), Some(&(p("0.99943"), p("0.99944"))));
        let b = parse_quotes(Exchange::Binance, fx(Exchange::Binance)).unwrap();
        assert_eq!(b.get("POLUSDT"), Some(&(p("0.11164"), p("0.11165"))));
    }

    #[test]
    fn error_envelopes_are_errors() {
        let cases = [
            (Exchange::Okx, r#"{"code":"50011","msg":"rate limit","data":[]}"#),
            (Exchange::Bybit, r#"{"retCode":10006,"retMsg":"too many","result":{"list":[]}}"#),
            (Exchange::Kraken, r#"{"error":["EGeneral:Too many requests"],"result":{}}"#),
            (Exchange::Kucoin, r#"{"code":"429000","msg":"busy","data":null}"#),
            (Exchange::Binance, r#"{"code":-1121,"msg":"Invalid symbol."}"#),
            (Exchange::Gate, r#"{"label":"INVALID","message":"x"}"#),
            (Exchange::Mexc, r#"{"code":700002,"msg":"x"}"#),
        ];
        for (ex, body) in cases {
            assert!(parse_quotes(ex, body).is_err(), "{ex:?}");
        }
        for ex in Exchange::ALL {
            assert!(parse_quotes(ex, "{not json").is_err(), "{ex:?} malformed");
        }
    }

    #[test]
    fn a_bad_price_drops_only_that_symbol() {
        let body = r#"[{"symbol":"AUSDT","bidPrice":"1.5","askPrice":"1e3"},
                       {"symbol":"BUSDT","bidPrice":"2","askPrice":"2.1"},
                       {"symbol":"CUSDT","bidPrice":null,"askPrice":"2.1"}]"#;
        let q = parse_quotes(Exchange::Mexc, body).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q.get("BUSDT"), Some(&(p("2"), p("2.1"))));
    }

    #[test]
    fn urls() {
        let syms = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        assert_eq!(
            url_for(Exchange::Binance, "https://api.binance.com", &syms, QuoteMode::Par),
            "https://api.binance.com/api/v3/ticker/bookTicker",
            "review M2: unfiltered (one unknown symbol must not fail the venue)"
        );
        let k = vec!["XXBTZUSD".to_string(), "POLUSD".to_string()];
        assert_eq!(
            url_for(Exchange::Kraken, "https://api.kraken.com", &k, QuoteMode::Par),
            "https://api.kraken.com/0/public/Ticker?pair=XXBTZUSD,POLUSD"
        );
        assert_eq!(
            url_for(Exchange::Kraken, "https://api.kraken.com", &k, QuoteMode::KrakenUsdt),
            "https://api.kraken.com/0/public/Ticker?pair=XXBTZUSD,POLUSD,USDTZUSD"
        );
        // kraken_usdt with no Kraken market symbols still fetches the rate.
        assert_eq!(
            url_for(Exchange::Kraken, "fake://kraken", &[], QuoteMode::KrakenUsdt),
            "fake://kraken/0/public/Ticker?pair=USDTZUSD"
        );
        assert_eq!(url_for(Exchange::Okx, "fake://okx", &syms, QuoteMode::Par), "fake://okx/api/v5/market/tickers?instType=SPOT");
        assert_eq!(url_for(Exchange::Bybit, "fake://bybit", &syms, QuoteMode::Par), "fake://bybit/v5/market/tickers?category=spot");
        assert_eq!(url_for(Exchange::Kucoin, "fake://kucoin", &syms, QuoteMode::Par), "fake://kucoin/api/v1/market/allTickers");
        assert_eq!(url_for(Exchange::Gate, "fake://gate", &syms, QuoteMode::Par), "fake://gate/api/v4/spot/tickers");
        assert_eq!(url_for(Exchange::Mexc, "fake://mexc", &syms, QuoteMode::Par), "fake://mexc/api/v3/ticker/bookTicker");
    }

    fn fake_kraken(routes: &[(&str, &str)]) -> crate::testing::FakeHttp {
        let h = crate::testing::FakeHttp::default();
        for (q, body) in routes {
            h.route(&format!("fake://kraken/0/public/AssetPairs?pair={q}"), Ok(body.to_string()));
        }
        h
    }

    const KAP: &str = include_str!("../tests/fixtures/kraken_assetpairs.json");
    const KAP_ALT: &str = include_str!("../tests/fixtures/kraken_assetpairs_alt.json");
    const KUNKNOWN: &str = include_str!("../tests/fixtures/kraken_unknown_pair.json");

    /// Review M2: configured Kraken pairs are validated at startup against
    /// AssetPairs (recorded fixtures): every pair must exist under its
    /// canonical key (the Ticker response is keyed by it).
    #[tokio::test]
    async fn kraken_pairs_are_validated_at_startup() {
        let t = std::time::Duration::from_secs(2);
        let pairs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ok = fake_kraken(&[("XXBTZUSD,XETHZUSD,POLUSD,USDTZUSD", KAP)]);
        check_kraken_pairs(&ok, "fake://kraken", &pairs(&["XXBTZUSD", "XETHZUSD", "POLUSD", "USDTZUSD"]), t)
            .await
            .unwrap();
        // An altname works for AssetPairs but the ticker is keyed canonically.
        let alt = fake_kraken(&[("XBTUSD", KAP_ALT)]);
        let e = check_kraken_pairs(&alt, "fake://kraken", &pairs(&["XBTUSD"]), t).await.unwrap_err();
        assert!(e.contains("XBTUSD") && e.contains("XXBTZUSD"), "{e}");
        // Unknown pair: named precisely (per-pair probe after the batch error).
        let bad = fake_kraken(&[("XXBTZUSD,NOPEUSD", KUNKNOWN), ("XXBTZUSD", KAP_ALT), ("NOPEUSD", KUNKNOWN)]);
        let e = check_kraken_pairs(&bad, "fake://kraken", &pairs(&["XXBTZUSD", "NOPEUSD"]), t).await.unwrap_err();
        assert!(e.contains("NOPEUSD") && !e.contains("XXBTZUSD"), "{e}");
        // No Kraken pairs configured: nothing to check, no request.
        let none = crate::testing::FakeHttp::default();
        check_kraken_pairs(&none, "fake://kraken", &[], t).await.unwrap();
        assert!(none.calls().is_empty());
    }
}
