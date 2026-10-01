//! Feeder config: TOML schema + validation (fatal at startup).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use alloy_primitives::Address;
use serde::Deserialize;

/// The 7 venues (HL set). Order = `ALL` = default-weight order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Exchange {
    Binance,
    Okx,
    Bybit,
    Kraken,
    Kucoin,
    Gate,
    Mexc,
}

impl Exchange {
    pub const ALL: [Exchange; 7] = [
        Exchange::Binance,
        Exchange::Okx,
        Exchange::Bybit,
        Exchange::Kraken,
        Exchange::Kucoin,
        Exchange::Gate,
        Exchange::Mexc,
    ];

    /// Config / metrics name.
    pub fn name(self) -> &'static str {
        match self {
            Exchange::Binance => "binance",
            Exchange::Okx => "okx",
            Exchange::Bybit => "bybit",
            Exchange::Kraken => "kraken",
            Exchange::Kucoin => "kucoin",
            Exchange::Gate => "gate",
            Exchange::Mexc => "mexc",
        }
    }

    pub fn from_name(s: &str) -> Option<Exchange> {
        Exchange::ALL.into_iter().find(|e| e.name() == s)
    }

    /// HL weights: Binance 3, OKX 2, Bybit 2, the rest 1.
    pub fn default_weight(self) -> u32 {
        match self {
            Exchange::Binance => 3,
            Exchange::Okx | Exchange::Bybit => 2,
            _ => 1,
        }
    }

    /// Kraken symbols quote USD by default, every other venue USDT.
    pub fn default_quote(self) -> Quote {
        match self {
            Exchange::Kraken => Quote::Usd,
            _ => Quote::Usdt,
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Exchange::Binance => "https://api.binance.com",
            Exchange::Okx => "https://www.okx.com",
            Exchange::Bybit => "https://api.bybit.com",
            Exchange::Kraken => "https://api.kraken.com",
            Exchange::Kucoin => "https://api.kucoin.com",
            Exchange::Gate => "https://api.gateio.ws",
            Exchange::Mexc => "https://api.mexc.com",
        }
    }
}

/// Quote currency of a venue symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Quote {
    Usd,
    Usdt,
    Usdc,
}

/// How USDT-quoted mids become USD.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteMode {
    /// USDT and USDC count as USD 1:1.
    #[default]
    Par,
    /// USDT mids are multiplied by Kraken's USDT/USD mid (`USDTZUSD`).
    KrakenUsdt,
}

/// Review L5: the largest per-venue weight a config may set.
pub const MAX_EXCHANGE_WEIGHT: u32 = 1_000;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeOverride {
    pub enabled: Option<bool>,
    pub weight: Option<u32>,
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolFull {
    symbol: String,
    quote: Quote,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum SymbolCfg {
    Plain(String),
    Full(SymbolFull),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMarket {
    market_id: u64,
    base_asset: String,
    symbols: BTreeMap<String, SymbolCfg>,
}

fn d_interval() -> u64 {
    3000
}
fn d_timeout() -> u64 {
    2000
}
fn d_age() -> u64 {
    5000
}
fn d_min_sources() -> usize {
    3
}
fn d_bps() -> u32 {
    5000
}
fn d_listen() -> String {
    "127.0.0.1:9466".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    rpc_url: String,
    validator_address: Address,
    signer_keystore: Option<PathBuf>,
    passphrase_file: Option<PathBuf>,
    signer_key_file: Option<PathBuf>,
    #[serde(default = "d_interval")]
    interval_ms: u64,
    #[serde(default = "d_timeout")]
    fetch_timeout_ms: u64,
    #[serde(default = "d_age")]
    max_source_age_ms: u64,
    #[serde(default = "d_min_sources")]
    min_sources: usize,
    #[serde(default = "d_bps")]
    min_weight_bps: u32,
    #[serde(default)]
    quote_mode: QuoteMode,
    #[serde(default = "d_listen")]
    health_listen: String,
    #[serde(default)]
    exchanges: BTreeMap<String, ExchangeOverride>,
    #[serde(default)]
    markets: Vec<RawMarket>,
}

/// One configured market: the on-chain id + base asset and its venue symbols.
#[derive(Clone, Debug)]
pub struct MarketCfg {
    pub market_id: u64,
    pub base_asset: String,
    symbols: BTreeMap<Exchange, (String, Quote)>,
}

impl MarketCfg {
    /// The configured symbol on `ex` (ignores whether the venue is enabled).
    pub fn symbol(&self, ex: Exchange) -> Option<(&str, Quote)> {
        self.symbols.get(&ex).map(|(s, q)| (s.as_str(), *q))
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub rpc_url: String,
    pub validator_address: Address,
    pub signer_keystore: Option<PathBuf>,
    pub passphrase_file: Option<PathBuf>,
    pub signer_key_file: Option<PathBuf>,
    pub interval_ms: u64,
    pub fetch_timeout_ms: u64,
    pub max_source_age_ms: u64,
    pub min_sources: usize,
    pub min_weight_bps: u32,
    pub quote_mode: QuoteMode,
    pub health_listen: String,
    pub exchanges: BTreeMap<Exchange, ExchangeOverride>,
    pub markets: Vec<MarketCfg>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        Config::parse(&s).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parse + validate. Every error is fatal at startup.
    pub fn parse(toml_str: &str) -> Result<Config, String> {
        let raw: RawConfig = toml::from_str(toml_str).map_err(|e| format!("config: {e}"))?;
        let mut exchanges = BTreeMap::new();
        for (name, o) in raw.exchanges {
            let ex = Exchange::from_name(&name).ok_or_else(|| format!("unknown exchange {name:?}"))?;
            exchanges.insert(ex, o);
        }
        let mut markets = Vec::with_capacity(raw.markets.len());
        for m in raw.markets {
            let mut symbols = BTreeMap::new();
            for (name, s) in m.symbols {
                let ex = Exchange::from_name(&name)
                    .ok_or_else(|| format!("market {}: unknown exchange {name:?}", m.market_id))?;
                let (sym, quote) = match s {
                    SymbolCfg::Plain(sym) => (sym, ex.default_quote()),
                    SymbolCfg::Full(f) => (f.symbol, f.quote),
                };
                symbols.insert(ex, (sym, quote));
            }
            markets.push(MarketCfg { market_id: m.market_id, base_asset: m.base_asset, symbols });
        }
        let c = Config {
            rpc_url: raw.rpc_url,
            validator_address: raw.validator_address,
            signer_keystore: raw.signer_keystore,
            passphrase_file: raw.passphrase_file,
            signer_key_file: raw.signer_key_file,
            interval_ms: raw.interval_ms,
            fetch_timeout_ms: raw.fetch_timeout_ms,
            max_source_age_ms: raw.max_source_age_ms,
            min_sources: raw.min_sources,
            min_weight_bps: raw.min_weight_bps,
            quote_mode: raw.quote_mode,
            health_listen: raw.health_listen,
            exchanges,
            markets,
        };
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.interval_ms < 1000 {
            return Err(format!("interval_ms must be >= 1000, got {}", self.interval_ms));
        }
        if self.fetch_timeout_ms == 0 || self.fetch_timeout_ms >= self.interval_ms {
            return Err(format!(
                "fetch_timeout_ms must be in 1..interval_ms ({}), got {}",
                self.interval_ms, self.fetch_timeout_ms
            ));
        }
        if self.max_source_age_ms < self.fetch_timeout_ms {
            return Err(format!(
                "max_source_age_ms ({}) must be >= fetch_timeout_ms ({})",
                self.max_source_age_ms, self.fetch_timeout_ms
            ));
        }
        if self.min_sources == 0 {
            return Err("min_sources must be >= 1".into());
        }
        if self.min_weight_bps > 10_000 {
            return Err(format!("min_weight_bps must be <= 10000, got {}", self.min_weight_bps));
        }
        match (&self.signer_keystore, &self.signer_key_file) {
            (Some(_), None) => {
                if self.passphrase_file.is_none() {
                    return Err("signer_keystore needs passphrase_file".into());
                }
            }
            (None, Some(_)) => {}
            _ => return Err("set exactly one of signer_keystore or signer_key_file".into()),
        }
        for (ex, o) in &self.exchanges {
            if o.weight.is_some_and(|w| w == 0 || w > MAX_EXCHANGE_WEIGHT) {
                return Err(format!("exchange {}: weight must be in 1..={MAX_EXCHANGE_WEIGHT}", ex.name()));
            }
        }
        if self.markets.is_empty() {
            return Err("no markets configured".into());
        }
        let mut ids = BTreeSet::new();
        for m in &self.markets {
            if !ids.insert(m.market_id) {
                return Err(format!("duplicate market {}", m.market_id));
            }
            if m.base_asset.trim().is_empty() {
                return Err(format!("market {}: empty base_asset", m.market_id));
            }
            for (ex, (sym, _)) in &m.symbols {
                if sym.trim().is_empty() {
                    return Err(format!("market {}: empty symbol for {}", m.market_id, ex.name()));
                }
            }
            let enabled = self.venues_for(m).len();
            if enabled < self.min_sources {
                return Err(format!(
                    "market {}: {enabled} enabled symbols < min_sources {}",
                    m.market_id, self.min_sources
                ));
            }
        }
        Ok(())
    }

    pub fn enabled(&self, ex: Exchange) -> bool {
        self.exchanges.get(&ex).and_then(|o| o.enabled).unwrap_or(true)
    }

    pub fn weight(&self, ex: Exchange) -> u32 {
        self.exchanges.get(&ex).and_then(|o| o.weight).unwrap_or_else(|| ex.default_weight())
    }

    pub fn base_url(&self, ex: Exchange) -> String {
        self.exchanges
            .get(&ex)
            .and_then(|o| o.base_url.clone())
            .unwrap_or_else(|| ex.default_base_url().to_string())
    }

    /// The market's symbols on ENABLED venues.
    pub fn venues_for<'a>(&self, m: &'a MarketCfg) -> Vec<(Exchange, &'a str, Quote)> {
        m.symbols
            .iter()
            .filter(|(ex, _)| self.enabled(**ex))
            .map(|(ex, (s, q))| (*ex, s.as_str(), *q))
            .collect()
    }

    /// Configured weight per enabled venue of market `m` (the 50 % base).
    pub fn weights_for(&self, m: &MarketCfg) -> BTreeMap<Exchange, u32> {
        self.venues_for(m).into_iter().map(|(ex, _, _)| (ex, self.weight(ex))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../feeder.example.toml");

    fn minimal(extra: &str) -> String {
        format!(
            r#"
rpc_url = "http://127.0.0.1:8545"
signer_key_file = "/tmp/k"
validator_address = "0x1111111111111111111111111111111111111111"
{extra}
[[markets]]
market_id = 1
base_asset = "BTC"
symbols = {{ binance = "BTCUSDT", okx = "BTC-USDT", bybit = "BTCUSDT" }}
"#
        )
    }

    #[test]
    fn example_config_parses_and_validates() {
        let c = Config::parse(EXAMPLE).unwrap();
        assert_eq!(c.markets.len(), 3);
        let matic = c.markets.iter().find(|m| m.market_id == 9).unwrap();
        assert_eq!(matic.base_asset, "MATIC");
        assert_eq!(matic.symbol(Exchange::Binance), Some(("POLUSDT", Quote::Usdt)));
        assert_eq!(matic.symbol(Exchange::Kraken), Some(("POLUSD", Quote::Usd)));
        assert_eq!(matic.symbol(Exchange::Okx), Some(("POL-USDT", Quote::Usdt)));
        assert!(c.signer_keystore.is_some() && c.passphrase_file.is_some());
        assert!(c.signer_key_file.is_none());
    }

    #[test]
    fn defaults_are_hl() {
        let c = Config::parse(&minimal("")).unwrap();
        assert_eq!(
            (c.interval_ms, c.fetch_timeout_ms, c.max_source_age_ms, c.min_sources, c.min_weight_bps),
            (3000, 2000, 5000, 3, 5000)
        );
        assert_eq!(c.quote_mode, QuoteMode::Par);
        assert_eq!(c.health_listen, "127.0.0.1:9466");
        let w: Vec<u32> = Exchange::ALL.iter().map(|e| c.weight(*e)).collect();
        assert_eq!(w, vec![3, 2, 2, 1, 1, 1, 1]);
        assert!(Exchange::ALL.iter().all(|e| c.enabled(*e)));
        assert_eq!(c.base_url(Exchange::Binance), "https://api.binance.com");
    }

    #[test]
    fn overrides_and_symbol_quote() {
        let c = Config::parse(&minimal(
            "quote_mode = \"kraken_usdt\"\n[exchanges.okx]\nweight = 5\nbase_url = \"fake://okx\"\n[exchanges.mexc]\nenabled = false\n",
        ))
        .unwrap();
        assert_eq!(c.quote_mode, QuoteMode::KrakenUsdt);
        assert_eq!(c.weight(Exchange::Okx), 5);
        assert_eq!(c.base_url(Exchange::Okx), "fake://okx");
        assert!(!c.enabled(Exchange::Mexc));
        let toml = minimal("").replace(
            "bybit = \"BTCUSDT\"",
            "bybit = { symbol = \"BTCUSDC\", quote = \"USDC\" }",
        );
        let c = Config::parse(&toml).unwrap();
        assert_eq!(c.markets[0].symbol(Exchange::Bybit), Some(("BTCUSDC", Quote::Usdc)));
    }

    #[test]
    fn disabled_venue_symbols_are_not_used() {
        let toml = minimal("[exchanges.okx]\nenabled = false\n");
        let err = Config::parse(&toml).unwrap_err();
        assert!(err.contains("enabled symbols"), "{err}");
    }

    #[test]
    fn validation_rejects() {
        let base = minimal("");
        let cases: Vec<(String, &str)> = vec![
            (minimal("interval_ms = 999"), "interval_ms"),
            (minimal("interval_ms = 2000\nfetch_timeout_ms = 2000"), "fetch_timeout_ms"),
            (minimal("max_source_age_ms = 1999"), "max_source_age_ms"),
            (minimal("min_sources = 0"), "min_sources"),
            (minimal("min_weight_bps = 10001"), "min_weight_bps"),
            (base.split("[[markets]]").next().unwrap().to_string(), "no markets"),
            (format!("{base}\n[[markets]]\nmarket_id = 1\nbase_asset = \"X\"\nsymbols = {{ binance = \"A\", okx = \"B\", bybit = \"C\" }}\n"), "duplicate market"),
            (base.replace("okx = \"BTC-USDT\"", "okx = \"\""), "empty symbol"),
            (base.replace(", bybit = \"BTCUSDT\"", ""), "enabled symbols"),
            (minimal("[exchanges.okx]\nweight = 0\n"), "weight"),
            (minimal("[exchanges.okx]\nweight = 1001\n"), "weight"), // review L5: capped
            (base.replace("signer_key_file = \"/tmp/k\"", "signer_key_file = \"/tmp/k\"\nsigner_keystore = \"/tmp/s\"\npassphrase_file = \"/tmp/p\""), "exactly one"),
            (base.replace("signer_key_file = \"/tmp/k\"", ""), "exactly one"),
            (base.replace("signer_key_file = \"/tmp/k\"", "signer_keystore = \"/tmp/s\""), "passphrase_file"),
            (minimal("bogus = 1"), "unknown field"),
            (base.replace("base_asset = \"BTC\"", "base_asset = \"\""), "base_asset"),
        ];
        for (toml, needle) in cases {
            let err = Config::parse(&toml).expect_err(needle);
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }
}
