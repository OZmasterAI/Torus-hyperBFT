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

/// The one bulk request for `ex` this cycle. Binance and Kraken take the
/// symbol list; the other venues return all spot tickers.
pub fn url_for(ex: Exchange, base_url: &str, symbols: &[String], mode: QuoteMode) -> String {
    let base = base_url.trim_end_matches('/');
    match ex {
        Exchange::Binance => {
            let list = symbols.iter().map(|s| format!("%22{s}%22")).collect::<Vec<_>>().join("%2C");
            format!("{base}/api/v3/ticker/bookTicker?symbols=%5B{list}%5D")
        }
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
            "https://api.binance.com/api/v3/ticker/bookTicker?symbols=%5B%22BTCUSDT%22%2C%22ETHUSDT%22%5D"
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
}
