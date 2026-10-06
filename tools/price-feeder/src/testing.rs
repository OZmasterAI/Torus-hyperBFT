//! Test doubles for the two network seams (`HttpGet`, `NodeApi`). Public so
//! the end-to-end test (`tests/rpc_e2e.rs`) can drive a real node with fake
//! venues. Nothing here touches the network.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use torus_types::SignedNativeAction;

use crate::config::Exchange;
use crate::exchange::HttpGet;
use crate::node::{MarketInfo, NodeApi, ValidatorInfo};

/// Recorded venue responses (s517, trimmed to BTC/ETH/POL).
pub fn fixture(ex: Exchange) -> &'static str {
    match ex {
        Exchange::Binance => include_str!("../tests/fixtures/binance.json"),
        Exchange::Okx => include_str!("../tests/fixtures/okx.json"),
        Exchange::Bybit => include_str!("../tests/fixtures/bybit.json"),
        Exchange::Kraken => include_str!("../tests/fixtures/kraken.json"),
        Exchange::Kucoin => include_str!("../tests/fixtures/kucoin.json"),
        Exchange::Gate => include_str!("../tests/fixtures/gate.json"),
        Exchange::Mexc => include_str!("../tests/fixtures/mexc.json"),
    }
}

/// [`fixture`] with the venue timestamps (OKX rows, Bybit `time`, KuCoin
/// `data.time`) set to `now_ms`, for tests that run on the wall clock.
pub fn fixture_at(ex: Exchange, now_ms: u64) -> String {
    let mut v: serde_json::Value = serde_json::from_str(fixture(ex)).expect("fixture json");
    match ex {
        Exchange::Okx => {
            for row in v["data"].as_array_mut().into_iter().flatten() {
                row["ts"] = serde_json::Value::String(now_ms.to_string());
            }
        }
        Exchange::Bybit => v["time"] = now_ms.into(),
        Exchange::Kucoin => v["data"]["time"] = now_ms.into(),
        _ => {}
    }
    v.to_string()
}

type Route = (String, Result<String, String>, Duration);

#[derive(Default)]
struct HttpInner {
    routes: Vec<Route>,
    calls: Vec<String>,
}

/// Routes by URL prefix (e.g. `fake://binance`); unknown URLs fail.
#[derive(Clone, Default)]
pub struct FakeHttp {
    inner: Arc<Mutex<HttpInner>>,
}

impl FakeHttp {
    /// [`Self::with_fixtures`] with venue timestamps at `now_ms` ([`fixture_at`]).
    pub fn with_fixtures_at(now_ms: u64) -> Self {
        let h = FakeHttp::default();
        for ex in Exchange::ALL {
            h.route(&format!("fake://{}", ex.name()), Ok(fixture_at(ex, now_ms)));
        }
        h
    }

    /// Every venue at `fake://<venue>` answering with its recorded fixture.
    pub fn with_fixtures() -> Self {
        let h = FakeHttp::default();
        for ex in Exchange::ALL {
            h.route(&format!("fake://{}", ex.name()), Ok(fixture(ex).to_string()));
        }
        h
    }

    pub fn route(&self, prefix: &str, body: Result<String, String>) {
        self.route_delayed(prefix, body, Duration::ZERO);
    }

    pub fn route_delayed(&self, prefix: &str, body: Result<String, String>, delay: Duration) {
        let mut g = self.inner.lock().unwrap();
        g.routes.retain(|r| r.0 != prefix);
        g.routes.push((prefix.to_string(), body, delay));
    }

    pub fn calls(&self) -> Vec<String> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn calls_to(&self, prefix: &str) -> usize {
        self.inner.lock().unwrap().calls.iter().filter(|u| u.starts_with(prefix)).count()
    }
}

impl HttpGet for FakeHttp {
    async fn get(&self, url: String, _timeout: Duration) -> Result<String, String> {
        let route = {
            let mut g = self.inner.lock().unwrap();
            g.calls.push(url.clone());
            g.routes.iter().filter(|r| url.starts_with(&r.0)).max_by_key(|r| r.0.len()).cloned()
        };
        let Some((_, body, delay)) = route else {
            return Err(format!("no route for {url}"));
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        body
    }
}

#[derive(Default)]
struct NodeInner {
    markets: Option<Result<Vec<MarketInfo>, String>>,
    validators: Vec<ValidatorInfo>,
    validators_err: Option<String>,
    submit_results: VecDeque<Result<String, String>>,
    submitted: Vec<SignedNativeAction>,
    market_calls: usize,
}

/// An in-memory node. Submissions are recorded; results are queued
/// (`push_submit_result`), defaulting to accepted.
#[derive(Clone, Default)]
pub struct FakeNode {
    inner: Arc<Mutex<NodeInner>>,
}

impl FakeNode {
    pub fn set_markets(&self, m: Result<Vec<MarketInfo>, String>) {
        self.inner.lock().unwrap().markets = Some(m);
    }
    pub fn set_validators(&self, v: Vec<ValidatorInfo>) {
        let mut g = self.inner.lock().unwrap();
        g.validators = v;
        g.validators_err = None;
    }
    /// `getValidators` fails (node unreachable) until the next `set_validators`.
    pub fn set_validators_err(&self, e: &str) {
        self.inner.lock().unwrap().validators_err = Some(e.to_string());
    }
    pub fn push_submit_result(&self, r: Result<String, String>) {
        self.inner.lock().unwrap().submit_results.push_back(r);
    }
    /// Every submission attempt, accepted or not.
    pub fn submitted(&self) -> Vec<SignedNativeAction> {
        self.inner.lock().unwrap().submitted.clone()
    }
    pub fn market_calls(&self) -> usize {
        self.inner.lock().unwrap().market_calls
    }
}

impl NodeApi for FakeNode {
    async fn listed_markets(&self) -> Result<Vec<MarketInfo>, String> {
        let mut g = self.inner.lock().unwrap();
        g.market_calls += 1;
        g.markets.clone().unwrap_or(Ok(Vec::new()))
    }

    async fn validators(&self) -> Result<Vec<ValidatorInfo>, String> {
        let g = self.inner.lock().unwrap();
        match &g.validators_err {
            Some(e) => Err(e.clone()),
            None => Ok(g.validators.clone()),
        }
    }

    async fn submit(&self, signed: &SignedNativeAction) -> Result<String, String> {
        let mut g = self.inner.lock().unwrap();
        g.submitted.push(signed.clone());
        let n = g.submitted.len();
        g.submit_results.pop_front().unwrap_or_else(|| Ok(format!("0x{n:064x}")))
    }
}
