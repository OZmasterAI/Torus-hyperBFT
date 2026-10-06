//! The local node: `NodeApi` seam, JSON-RPC client, startup checks.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::Duration;

use alloy_primitives::Address;
use serde_json::Value;
use torus_types::{MarketId, SignedNativeAction};

use crate::config::Config;

/// `torus_getMarkets` page size (the node's maximum).
pub const MARKETS_PAGE: u32 = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketInfo {
    pub id: MarketId,
    pub base_asset: String,
    pub quote_asset: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatorInfo {
    pub address: Address,
    pub status: String,
    pub oracle_signer: Option<Address>,
}

/// What the feeder needs from its node (tests use `testing::FakeNode`).
pub trait NodeApi: Send + Sync {
    fn listed_markets(&self) -> impl Future<Output = Result<Vec<MarketInfo>, String>> + Send;
    fn validators(&self) -> impl Future<Output = Result<Vec<ValidatorInfo>, String>> + Send;
    /// `torus_submitNativeAction`; Ok = the action hash.
    fn submit(&self, signed: &SignedNativeAction) -> impl Future<Output = Result<String, String>> + Send;
}

fn hex_u64(v: &Value, what: &str) -> Result<u64, String> {
    let s = v.as_str().ok_or_else(|| format!("{what}: not a string"))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| format!("{what} {s:?}: {e}"))
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| format!("missing {k}"))
}

pub fn parse_markets_page(v: &Value) -> Result<Vec<MarketInfo>, String> {
    v.as_array()
        .ok_or("getMarkets: expected an array")?
        .iter()
        .map(|m| {
            Ok(MarketInfo {
                id: hex_u64(m.get("marketId").unwrap_or(&Value::Null), "marketId")?,
                base_asset: field(m, "baseAsset")?.to_string(),
                quote_asset: field(m, "quoteAsset")?.to_string(),
            })
        })
        .collect()
}

pub fn parse_validators(v: &Value) -> Result<Vec<ValidatorInfo>, String> {
    let addr = |s: &str| s.parse::<Address>().map_err(|e| format!("address {s:?}: {e}"));
    v.as_array()
        .ok_or("getValidators: expected an array")?
        .iter()
        .map(|x| {
            Ok(ValidatorInfo {
                address: addr(field(x, "address")?)?,
                status: field(x, "status")?.to_string(),
                oracle_signer: match x.get("oracleSigner").and_then(Value::as_str) {
                    Some(s) => Some(addr(s)?),
                    None => None,
                },
            })
        })
        .collect()
}

/// Page through `torus_getMarkets` until a short page.
pub async fn collect_market_pages<F, Fut>(mut page: F) -> Result<Vec<MarketInfo>, String>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<Vec<MarketInfo>, String>>,
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

/// JSON-RPC client for the LOCAL node (2 s timeout per call).
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
}

impl NodeApi for RpcNode {
    async fn listed_markets(&self) -> Result<Vec<MarketInfo>, String> {
        collect_market_pages(|offset| async move {
            parse_markets_page(&self.call("torus_getMarkets", serde_json::json!([offset, MARKETS_PAGE])).await?)
        })
        .await
    }

    async fn validators(&self) -> Result<Vec<ValidatorInfo>, String> {
        parse_validators(&self.call("torus_getValidators", serde_json::json!([])).await?)
    }

    async fn submit(&self, signed: &SignedNativeAction) -> Result<String, String> {
        // The node does hex::decode -> serde_json::from_slice (wallet rpc.rs).
        let json = serde_json::to_vec(signed).map_err(|e| e.to_string())?;
        let r = self
            .call("torus_submitNativeAction", serde_json::json!([format!("0x{}", hex::encode(json))]))
            .await?;
        r.as_str().map(str::to_owned).ok_or_else(|| "submit: result not a string".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// Registered but not submitting (validator not active).
    Idle(String),
}

#[derive(Clone, Debug)]
pub struct CheckReport {
    pub readiness: Readiness,
    /// Configured markets that are listed with a matching listing.
    pub listed: BTreeSet<MarketId>,
    pub warnings: Vec<String>,
}

/// The registration command for an operator (review M3: the wallet signs the
/// signer key's proof of possession, so it needs the signer keystore too).
pub fn register_hint(signer: Address) -> String {
    format!(
        "torus-wallet --keystore <validator EVM keystore> set-oracle-signer \
         --signer-keystore <keystore of signer {signer:#x}>"
    )
}

/// Validator + signer registration, then every configured market's listing.
/// Err = fatal (misconfiguration or node unreachable).
pub async fn startup_check<N: NodeApi>(cfg: &Config, node: &N, signer: Address) -> Result<CheckReport, String> {
    let vals = node.validators().await.map_err(|e| format!("getValidators: {e}"))?;
    let v = vals
        .iter()
        .find(|v| v.address == cfg.validator_address)
        .ok_or_else(|| format!("validator {:#x} not found in getValidators", cfg.validator_address))?;
    if v.oracle_signer != Some(signer) {
        let reg = match v.oracle_signer {
            Some(s) => format!("registers signer {s:#x}"),
            None => "has no oracle signer".to_string(),
        };
        return Err(format!(
            "validator {:#x} {reg}, not this feeder's signer {signer:#x}; register it with: {}",
            cfg.validator_address,
            register_hint(signer)
        ));
    }
    let readiness = if v.status == "active" {
        Readiness::Ready
    } else {
        Readiness::Idle(format!("validator {:#x} is {}", cfg.validator_address, v.status))
    };
    let markets = node.listed_markets().await.map_err(|e| format!("getMarkets: {e}"))?;
    let mut listed = BTreeSet::new();
    let mut warnings = Vec::new();
    for m in &cfg.markets {
        match markets.iter().find(|x| x.id == m.market_id) {
            None => warnings.push(format!("market {} ({}) is not listed; skipped", m.market_id, m.base_asset)),
            Some(x) => {
                check_listing(m.market_id, &m.base_asset, x)?;
                listed.insert(m.market_id);
            }
        }
    }
    Ok(CheckReport { readiness, listed, warnings })
}

/// A listed market must match the config: same base (case-insensitive), USD quote.
pub fn check_listing(id: MarketId, base: &str, x: &MarketInfo) -> Result<(), String> {
    if !x.base_asset.eq_ignore_ascii_case(base) {
        return Err(format!("market {id}: on-chain baseAsset {:?} != configured {base:?}", x.base_asset));
    }
    if x.quote_asset != "USD" {
        return Err(format!("market {id}: on-chain quoteAsset {:?} is not \"USD\"", x.quote_asset));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::testing::FakeNode;
    use serde_json::json;

    const V: &str = "0x1111111111111111111111111111111111111111";
    const S: &str = "0x2222222222222222222222222222222222222222";

    fn a(s: &str) -> Address {
        s.parse().unwrap()
    }

    #[test]
    fn parses_markets_page() {
        let page = json!([
            {"marketId": "0x1", "baseAsset": "BTC", "quoteAsset": "USD", "lotSize": "0x1", "tickSize": "0x1", "status": "active"},
            {"marketId": "0x1f", "baseAsset": "ETH", "quoteAsset": "USD", "lotSize": "0x1", "tickSize": "0x1", "status": "active"}
        ]);
        let m = parse_markets_page(&page).unwrap();
        assert_eq!(m, vec![
            MarketInfo { id: 1, base_asset: "BTC".into(), quote_asset: "USD".into() },
            MarketInfo { id: 31, base_asset: "ETH".into(), quote_asset: "USD".into() },
        ]);
        assert!(parse_markets_page(&json!({"x": 1})).is_err());
        assert!(parse_markets_page(&json!([{"marketId": "zz", "baseAsset": "B", "quoteAsset": "USD"}])).is_err());
    }

    #[tokio::test]
    async fn pages_until_short_page() {
        let calls = std::sync::Mutex::new(Vec::new());
        let all = collect_market_pages(|offset| {
            calls.lock().unwrap().push(offset);
            let n = if offset == 0 { MARKETS_PAGE } else { 3 };
            let page: Vec<MarketInfo> = (0..n as u64)
                .map(|i| MarketInfo { id: offset as u64 + i, base_asset: "X".into(), quote_asset: "USD".into() })
                .collect();
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(all.len(), MARKETS_PAGE as usize + 3);
        assert_eq!(*calls.lock().unwrap(), vec![0, MARKETS_PAGE]);
        let err = collect_market_pages(|_| async { Err::<Vec<MarketInfo>, _>("down".to_string()) }).await;
        assert_eq!(err.unwrap_err(), "down");
    }

    #[test]
    fn parses_validators_with_oracle_signer() {
        let v = json!([
            {"address": V, "pubkey": "0x00", "power": "0x1", "commissionBps": 0, "status": "active", "oracleSigner": S},
            {"address": "0x3333333333333333333333333333333333333333", "pubkey": "0x00", "power": "0x1", "commissionBps": 0, "status": "jailed"}
        ]);
        let vs = parse_validators(&v).unwrap();
        assert_eq!(vs[0], ValidatorInfo { address: a(V), status: "active".into(), oracle_signer: Some(a(S)) });
        assert_eq!(vs[1].oracle_signer, None);
        assert_eq!(vs[1].status, "jailed");
        assert!(parse_validators(&json!([{"address": "nope", "status": "active"}])).is_err());
    }

    fn cfg() -> Config {
        Config::parse(&format!(
            r#"
rpc_url = "http://127.0.0.1:8545"
signer_key_file = "/tmp/k"
validator_address = "{V}"
[[markets]]
market_id = 1
base_asset = "BTC"
symbols = {{ binance = "BTCUSDT", okx = "BTC-USDT", bybit = "BTCUSDT" }}
[[markets]]
market_id = 9
base_asset = "MATIC"
symbols = {{ binance = "POLUSDT", okx = "POL-USDT", bybit = "POLUSDT" }}
"#
        ))
        .unwrap()
    }

    fn market(id: u64, base: &str, quote: &str) -> MarketInfo {
        MarketInfo { id, base_asset: base.into(), quote_asset: quote.into() }
    }

    fn node(status: &str, signer: Option<&str>, markets: Vec<MarketInfo>) -> FakeNode {
        let n = FakeNode::default();
        n.set_validators(vec![ValidatorInfo { address: a(V), status: status.into(), oracle_signer: signer.map(a) }]);
        n.set_markets(Ok(markets));
        n
    }

    #[tokio::test]
    async fn startup_check_table() {
        let ok_markets = || vec![market(1, "BTC", "USD"), market(9, "matic", "USD"), market(5, "DOGE", "USD")];
        // Ready; base asset compare is case-insensitive; unconfigured market 5 ignored.
        let r = startup_check(&cfg(), &node("active", Some(S), ok_markets()), a(S)).await.unwrap();
        assert_eq!(r.readiness, Readiness::Ready);
        assert_eq!(r.listed, [1, 9].into_iter().collect());
        assert!(r.warnings.is_empty());

        // Validator missing.
        let n = FakeNode::default();
        n.set_markets(Ok(ok_markets()));
        let e = startup_check(&cfg(), &n, a(S)).await.unwrap_err();
        assert!(e.contains("not found"), "{e}");

        // Signer not registered / a different signer: fatal, with the command.
        for reg in [None, Some("0x4444444444444444444444444444444444444444")] {
            let e = startup_check(&cfg(), &node("active", reg, ok_markets()), a(S)).await.unwrap_err();
            assert!(e.contains("set-oracle-signer --signer-keystore"), "{e}");
            assert!(e.contains(&format!("signer {S}")), "{e}");
            assert!(e.contains("torus-wallet"), "{e}");
        }

        // Not active: idle, not fatal.
        let r = startup_check(&cfg(), &node("jailed", Some(S), ok_markets()), a(S)).await.unwrap();
        assert!(matches!(r.readiness, Readiness::Idle(ref m) if m.contains("jailed")), "{:?}", r.readiness);

        // Listed market with another base asset: fatal.
        let e = startup_check(&cfg(), &node("active", Some(S), vec![market(1, "ETH", "USD")]), a(S)).await.unwrap_err();
        assert!(e.contains("baseAsset"), "{e}");

        // Quote not USD: fatal.
        let e = startup_check(&cfg(), &node("active", Some(S), vec![market(1, "BTC", "USDT")]), a(S)).await.unwrap_err();
        assert!(e.contains("quoteAsset"), "{e}");

        // Configured market not listed: warn and skip.
        let r = startup_check(&cfg(), &node("active", Some(S), vec![market(1, "BTC", "USD")]), a(S)).await.unwrap();
        assert_eq!(r.listed, [1].into_iter().collect());
        assert!(r.warnings.iter().any(|w| w.contains("market 9")), "{:?}", r.warnings);

        // Node down: error.
        let n = node("active", Some(S), vec![]);
        n.set_markets(Err("connection refused".into()));
        assert!(startup_check(&cfg(), &n, a(S)).await.is_err());
    }
}
