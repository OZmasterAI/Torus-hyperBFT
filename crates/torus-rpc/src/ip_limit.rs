//! Per-IP request weight limit on the RPC server (anti-spam item D).
//!
//! Token bucket per client IP (Hyperliquid: 1200 weight per minute): the
//! bucket holds up to `weight_per_min` and refills at `weight_per_min / 60`
//! per second. Each JSON-RPC call costs [`method_weight`]; a JSON-RPC batch
//! costs the sum of its calls and is refused whole.
//!
//! Weights (HL: exchange 1 + floor(batch / 40), info 2 or 20):
//! - submits: `torus_submitNativeAction`, `eth_sendRawTransaction` = 1;
//!   `torus_submitNativeActions[Bin]` = 1 + floor(items / 40) (items = the
//!   payload strings in the batch; the per-ORDER count is item B's job);
//! - cheap point reads ([`CHEAP_READS`]) = 2;
//! - `*unsubscribe` = 1 (cleanup must never be refused for cost);
//! - every other method (scans, history, eth_call, logs, subscribe) = 20.
//!
//! IPv6 clients are keyed by their /64 (one host gets a whole /64), IPv4 by
//! address. Memory is bounded to [`DEFAULT_MAX_IPS`] buckets
//! ([`torus_mempool::bounded_map::TwoGen`]).
//!
//! Exemptions: `TORUS_RPC_IP_EXEMPT` (comma-separated CIDRs or IPs). Unset =
//! loopback only (127.0.0.0/8, ::1) — local tooling such as an oracle feeder.
//! Private ranges are NOT exempt by default: on a shared or cloud network
//! they are other tenants. A reverse proxy on the same host makes every
//! client look like loopback; run such a setup with an explicit
//! `TORUS_RPC_IP_EXEMPT=` (empty = no exemptions) and limit at the proxy.

use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use torus_mempool::bounded_map::TwoGen;

/// Default `TORUS_RPC_IP_WEIGHT_PER_MIN` (HL: 1200).
pub const DEFAULT_WEIGHT_PER_MIN: u32 = 1200;
/// Bound on tracked client buckets (~64 B each => ~6 MB).
pub const DEFAULT_MAX_IPS: usize = 100_000;
/// Default `TORUS_RPC_MAX_SUBS_PER_CONN` (jsonrpsee's own default is 1024).
pub const DEFAULT_MAX_SUBS_PER_CONN: u32 = 100;
/// JSON-RPC error code for a refused call (EIP-1474 "limit exceeded").
pub const RATE_LIMITED_CODE: i32 = -32005;

/// Methods that are single point reads (weight 2).
pub const CHEAP_READS: &[&str] = &[
    "eth_chainId",
    "eth_blockNumber",
    "eth_gasPrice",
    "eth_maxPriorityFeePerGas",
    "eth_getBalance",
    "eth_getTransactionCount",
    "eth_getCode",
    "eth_getTransactionByHash",
    "eth_getTransactionReceipt",
    "net_version",
    "net_listening",
    "net_peerCount",
    "web3_clientVersion",
    "web3_sha3",
    "torus_getBalances",
    "torus_getPosition",
    "torus_getUserLimits",
    "torus_getLiquidatorVault",
    "torus_getMarkPrice",
    "torus_getOpenInterest",
    "torus_getLeader",
    "torus_getEpoch",
    "torus_getStateHash",
    "torus_getOrderBook",
];

/// The client IP of the connection a request arrived on. Inserted into every
/// HTTP request's extensions by the accept loop in [`crate::RpcServer::start`];
/// jsonrpsee copies HTTP extensions into each JSON-RPC request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerIp(pub IpAddr);

/// Weight of one call. `params` is the raw JSON params (only read for the
/// batch submit endpoints).
pub fn method_weight(method: &str, params: Option<&str>) -> u32 {
    match method {
        "torus_submitNativeAction" | "eth_sendRawTransaction" => 1,
        "torus_submitNativeActions" | "torus_submitNativeActionsBin" => {
            // Payload items are hex strings (no inner quotes): count the
            // string literals with one byte scan instead of a JSON parse.
            let items = params.map_or(0, |p| p.bytes().filter(|b| *b == b'"').count() / 2);
            1 + (items / 40) as u32
        }
        m if m.ends_with("unsubscribe") => 1,
        m if CHEAP_READS.contains(&m) => 2,
        _ => 20,
    }
}

/// Start of every canonical-JSON oracle submission: `SignedNativeAction`
/// serializes `action` first and `NativeAction` is externally tagged.
const ORACLE_JSON_HEAD: &[u8] = br#"{"action":{"SubmitOraclePrices""#;

/// True when a submit call carries at least one oracle submission
/// (`SubmitOraclePrices`). Only labels a per-IP refusal
/// (`torus_rpc_ip_rejects_total{action}`), so it reads each payload's head
/// (bincode tag, or the canonical-JSON prefix [`ORACLE_JSON_HEAD`]) and
/// never decodes an action: a JSON payload with other key order or
/// whitespace counts as `other`.
pub fn is_oracle_call(method: &str, params: Option<&str>) -> bool {
    let bin = match method {
        "torus_submitNativeAction" | "torus_submitNativeActions" => false,
        "torus_submitNativeActionsBin" => true,
        _ => return false,
    };
    // Payloads are hex strings (no inner quotes): every second `"` piece.
    params.is_some_and(|p| {
        p.split('"').skip(1).step_by(2).any(|s| {
            if bin {
                crate::torus::peek_bin_tag(s) == Some(crate::torus::TAG_SUBMIT_ORACLE_PRICES)
            } else {
                let hex_head = s.strip_prefix("0x").unwrap_or(s);
                let mut head = [0u8; ORACLE_JSON_HEAD.len()];
                hex_head
                    .get(..2 * head.len())
                    .is_some_and(|h| hex::decode_to_slice(h, &mut head).is_ok())
                    && head == ORACLE_JSON_HEAD
            }
        })
    })
}

/// One CIDR block (`addr/prefix`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Parse `a.b.c.d/n`, `x::y/n` or a bare IP (full-length prefix).
    pub fn parse(s: &str) -> Option<Self> {
        let (ip, prefix) = match s.split_once('/') {
            Some((ip, p)) => (
                ip.trim().parse::<IpAddr>().ok()?,
                Some(p.trim().parse::<u8>().ok()?),
            ),
            None => (s.trim().parse::<IpAddr>().ok()?, None),
        };
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        (prefix <= max).then_some(Self { addr: ip, prefix })
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        match (self.addr, canonical(*ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

/// v4-mapped v6 (`::ffff:a.b.c.d`) as plain v4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Bucket key: IPv4 address, or the IPv6 /64 (one host is usually given a
/// whole /64, so per-address keying would be free to evade).
fn bucket_key(ip: IpAddr) -> IpAddr {
    match canonical(ip) {
        IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & (u128::MAX << 64)).into()),
        v4 => v4,
    }
}

/// Parse `TORUS_RPC_IP_WEIGHT_PER_MIN` (`0` = off; unset/unparsable = default).
pub fn parse_weight_per_min(raw: Option<String>) -> u32 {
    raw.and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_WEIGHT_PER_MIN)
}

/// Parse `TORUS_RPC_MAX_SUBS_PER_CONN` (at least 1; unset/unparsable = default).
pub fn parse_max_subs_per_conn(raw: Option<String>) -> u32 {
    raw.and_then(|v| v.trim().parse::<u32>().ok())
        .map(|v| v.max(1))
        .unwrap_or(DEFAULT_MAX_SUBS_PER_CONN)
}

/// Parse `TORUS_RPC_IP_EXEMPT`. Unset => loopback only; set (even empty) =>
/// exactly the listed blocks. Returns the invalid entries for logging.
pub fn parse_exempt(raw: Option<String>) -> (Vec<Cidr>, Vec<String>) {
    let Some(raw) = raw else {
        return (
            vec![
                Cidr::parse("127.0.0.0/8").expect("valid"),
                Cidr::parse("::1/128").expect("valid"),
            ],
            Vec::new(),
        );
    };
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match Cidr::parse(item) {
            Some(c) => ok.push(c),
            None => bad.push(item.to_string()),
        }
    }
    (ok, bad)
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// The per-IP limiter. Share behind an `Arc`.
pub struct IpLimiter {
    weight_per_min: u32,
    exempt: Vec<Cidr>,
    buckets: Mutex<TwoGen<IpAddr, Bucket>>,
}

impl IpLimiter {
    pub fn new(weight_per_min: u32, exempt: Vec<Cidr>, max_ips: usize) -> Self {
        Self {
            weight_per_min,
            exempt,
            buckets: Mutex::new(TwoGen::new(max_ips)),
        }
    }

    pub fn weight_per_min(&self) -> u32 {
        self.weight_per_min
    }

    pub fn exempt(&self) -> &[Cidr] {
        &self.exempt
    }

    /// Charge `weight` to `ip` at `now`; false = refuse.
    pub fn check(&self, ip: IpAddr, weight: u32, now: Instant) -> bool {
        if self.exempt.iter().any(|c| c.contains(&ip)) {
            return true;
        }
        let cap = f64::from(self.weight_per_min);
        let mut buckets = self.buckets.lock().unwrap();
        let b = buckets.entry_with(bucket_key(ip), || Bucket {
            tokens: cap,
            last: now,
        });
        let refill = now.saturating_duration_since(b.last).as_secs_f64() * cap / 60.0;
        b.tokens = (b.tokens + refill).min(cap);
        b.last = now;
        let w = f64::from(weight);
        if b.tokens >= w {
            b.tokens -= w;
            true
        } else {
            false
        }
    }

    /// Number of tracked buckets.
    pub fn tracked(&self) -> usize {
        self.buckets.lock().unwrap().len()
    }
}

/// Build the node's limiter from the env, or `None` when
/// `TORUS_RPC_IP_WEIGHT_PER_MIN=0`. Returns the invalid exempt entries.
pub fn from_env() -> (Option<IpLimiter>, Vec<String>) {
    let per_min = parse_weight_per_min(std::env::var("TORUS_RPC_IP_WEIGHT_PER_MIN").ok());
    let (exempt, bad) = parse_exempt(std::env::var("TORUS_RPC_IP_EXEMPT").ok());
    if per_min == 0 {
        return (None, bad);
    }
    (Some(IpLimiter::new(per_min, exempt, DEFAULT_MAX_IPS)), bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn weights_follow_the_table() {
        assert_eq!(method_weight("torus_submitNativeAction", None), 1);
        assert_eq!(
            method_weight("eth_sendRawTransaction", Some(r#"["0x00"]"#)),
            1
        );
        let items = |n: usize| {
            let v: Vec<String> = (0..n).map(|i| format!("0x{i:04x}")).collect();
            serde_json::to_string(&vec![v]).unwrap()
        };
        assert_eq!(
            method_weight("torus_submitNativeActions", Some(&items(1))),
            1
        );
        assert_eq!(
            method_weight("torus_submitNativeActions", Some(&items(39))),
            1
        );
        assert_eq!(
            method_weight("torus_submitNativeActions", Some(&items(40))),
            2
        );
        assert_eq!(
            method_weight("torus_submitNativeActionsBin", Some(&items(100))),
            3
        );
        assert_eq!(method_weight("torus_submitNativeActions", None), 1);
        assert_eq!(method_weight("eth_blockNumber", None), 2);
        assert_eq!(method_weight("torus_getBalances", Some(r#"["0x1"]"#)), 2);
        assert_eq!(method_weight("torus_getLiquidatorVault", None), 2);
        assert_eq!(method_weight("torus_getTradeHistory", None), 20);
        assert_eq!(method_weight("eth_getLogs", None), 20);
        assert_eq!(method_weight("torus_subscribe", None), 20);
        assert_eq!(method_weight("torus_unsubscribe", None), 1);
        assert_eq!(method_weight("eth_unsubscribe", None), 1);
    }

    /// s104: a refused call is labelled `action="oracle"` when any of its
    /// payloads is a `SubmitOraclePrices`, read from the payload head only
    /// (canonical-JSON prefix or bincode tag), in all three submit formats.
    #[test]
    fn oracle_calls_are_recognised() {
        use torus_types::{NativeAction, OracleSubmission};
        let key = k256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap();
        let sign = |a| torus_types::eip712::sign_native_action(a, 1, &key);
        let oracle = sign(NativeAction::SubmitOraclePrices(OracleSubmission {
            prices: vec![],
            timestamp: 0,
        }));
        let other = sign(NativeAction::ClaimRewards);
        let json = |s: &torus_types::SignedNativeAction| {
            format!("0x{}", hex::encode(serde_json::to_vec(s).unwrap()))
        };
        let bin = |s: &torus_types::SignedNativeAction| {
            format!("0x{}", hex::encode(bincode::serialize(s).unwrap()))
        };
        let one = |p: String| serde_json::to_string(&vec![p]).unwrap();
        let many = |v: Vec<String>| serde_json::to_string(&vec![v]).unwrap();

        let single = "torus_submitNativeAction";
        assert!(is_oracle_call(single, Some(&one(json(&oracle)))));
        assert!(!is_oracle_call(single, Some(&one(json(&other)))));
        // No `0x` prefix is accepted too (as `parse_bytes` does).
        let bare = json(&oracle).trim_start_matches("0x").to_string();
        assert!(is_oracle_call(single, Some(&one(bare))));

        let batch = "torus_submitNativeActions";
        assert!(is_oracle_call(
            batch,
            Some(&many(vec![json(&other), json(&oracle)]))
        ));
        assert!(!is_oracle_call(
            batch,
            Some(&many(vec![json(&other), json(&other)]))
        ));
        // A bincode payload sent to the JSON endpoint is not read as JSON.
        assert!(!is_oracle_call(batch, Some(&many(vec![bin(&oracle)]))));

        let bin_batch = "torus_submitNativeActionsBin";
        assert!(is_oracle_call(
            bin_batch,
            Some(&many(vec![bin(&other), bin(&oracle)]))
        ));
        assert!(!is_oracle_call(bin_batch, Some(&many(vec![bin(&other)]))));

        // Not a submit, no params, or junk: never oracle.
        assert!(!is_oracle_call("eth_chainId", Some(&one(json(&oracle)))));
        assert!(!is_oracle_call(single, None));
        assert!(!is_oracle_call(single, Some(r#"["0x7b","zz",""]"#)));
        assert!(!is_oracle_call(bin_batch, Some(r#"[["0x0f"]]"#)));
    }

    #[test]
    fn cidr_parse_and_match() {
        let c = Cidr::parse("10.1.0.0/16").unwrap();
        assert!(c.contains(&ip("10.1.200.3")));
        assert!(!c.contains(&ip("10.2.0.1")));
        assert!(!c.contains(&ip("::1")));
        let one = Cidr::parse("192.168.1.7").unwrap();
        assert!(one.contains(&ip("192.168.1.7")) && !one.contains(&ip("192.168.1.8")));
        let v6 = Cidr::parse("2001:db8::/32").unwrap();
        assert!(v6.contains(&ip("2001:db8:ffff::1")));
        assert!(!v6.contains(&ip("2001:db9::1")));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(&ip("8.8.8.8")));
        // v4-mapped v6 matches the v4 block.
        assert!(c.contains(&ip("::ffff:10.1.2.3")));
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("nope").is_none());
    }

    #[test]
    fn env_parsers() {
        assert_eq!(parse_weight_per_min(None), 1200);
        assert_eq!(parse_weight_per_min(Some("0".into())), 0);
        assert_eq!(parse_weight_per_min(Some("x".into())), 1200);
        assert_eq!(parse_max_subs_per_conn(None), 100);
        assert_eq!(parse_max_subs_per_conn(Some("0".into())), 1);
        let (def, _) = parse_exempt(None);
        assert!(def.iter().any(|c| c.contains(&ip("127.0.0.1"))));
        assert!(def.iter().any(|c| c.contains(&ip("::1"))));
        assert!(
            !def.iter().any(|c| c.contains(&ip("10.0.0.1"))),
            "private not exempt"
        );
        assert!(!def.iter().any(|c| c.contains(&ip("192.168.0.1"))));
        let (none, bad) = parse_exempt(Some(String::new()));
        assert!(none.is_empty() && bad.is_empty());
        let (some, bad) = parse_exempt(Some("10.0.0.0/8, bogus ,::1".into()));
        assert_eq!(some.len(), 2);
        assert_eq!(bad, vec!["bogus".to_string()]);
    }

    #[test]
    fn bucket_spends_then_refills() {
        let l = IpLimiter::new(1200, Vec::new(), 100);
        let t0 = Instant::now();
        let a = ip("203.0.113.5");
        // 600 cheap reads (weight 2) drain the 1200 bucket.
        for _ in 0..600 {
            assert!(l.check(a, 2, t0));
        }
        assert!(!l.check(a, 2, t0), "bucket empty");
        // Another client has its own bucket.
        assert!(l.check(ip("203.0.113.6"), 20, t0));
        // 1200/min = 20 per second: after 1 s, 20 weight is back.
        let t1 = t0 + Duration::from_secs(1);
        assert!(l.check(a, 20, t1));
        assert!(!l.check(a, 1, t1));
        // Never above the cap, however long idle.
        let t2 = t0 + Duration::from_secs(3_600);
        assert!(l.check(a, 1200, t2));
        assert!(!l.check(a, 1, t2));
    }

    #[test]
    fn exempt_and_ipv6_prefix_keying() {
        let l = IpLimiter::new(10, parse_exempt(None).0, 100);
        let t0 = Instant::now();
        for _ in 0..1_000 {
            assert!(l.check(ip("127.0.0.1"), 20, t0));
        }
        assert_eq!(l.tracked(), 0, "exempt clients are not tracked");
        // Two addresses in one /64 share a bucket.
        assert!(l.check(ip("2001:db8:1:2::1"), 10, t0));
        assert!(!l.check(ip("2001:db8:1:2::ffff"), 1, t0));
        assert!(l.check(ip("2001:db8:1:3::1"), 10, t0));
    }

    #[test]
    fn memory_is_bounded() {
        let l = IpLimiter::new(10, Vec::new(), 64);
        let t0 = Instant::now();
        for i in 0..10_000u32 {
            l.check(IpAddr::from(i.to_be_bytes()), 1, t0);
        }
        assert!(l.tracked() <= 64, "{}", l.tracked());
    }
}
