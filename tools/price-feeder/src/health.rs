//! `/health` (JSON, 200 or 503) and `/metrics` (Prometheus text) on a tiny
//! loopback TCP server (the faucet pattern, without CORS).

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::feeder::FeederStatus;
use crate::fetch::Clock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Degraded,
    Down,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Degraded => "degraded",
            Level::Down => "down",
        }
    }
}

/// Down: a NotAuthorized error, or no accepted submission within
/// 3 x interval. Degraded: a market left out or a venue failing. Else ok.
pub fn level(st: &FeederStatus, now_ms: u64) -> Level {
    let window = st.interval_ms.saturating_mul(3);
    let recent = st.last_submit_ok_ms.is_some_and(|t| now_ms.saturating_sub(t) <= window);
    if st.not_authorized.is_some() || !recent {
        Level::Down
    } else if st.markets.values().any(|m| m.omitted.is_some()) || st.venues.values().any(|v| !v.ok) {
        Level::Degraded
    } else {
        Level::Ok
    }
}

pub fn health_json(st: &FeederStatus, now_ms: u64) -> (u16, String) {
    let lvl = level(st, now_ms);
    let markets: Map<String, Value> = st
        .markets
        .iter()
        .map(|(id, m)| {
            (
                id.to_string(),
                json!({
                    "lastPrice": m.last_price.map(|p| p.to_string()),
                    "sources": m.sources,
                    "weight": m.weight,
                    "omitted": m.omitted,
                }),
            )
        })
        .collect();
    let venues: Map<String, Value> = st
        .venues
        .iter()
        .map(|(ex, v)| {
            (
                ex.name().to_string(),
                json!({
                    "ok": v.ok,
                    "latencyMs": v.latency_ms,
                    "lastError": v.last_error,
                    "backoffFailures": v.backoff_failures,
                    "fetchedAtMs": v.fetched_at_ms,
                }),
            )
        })
        .collect();
    let body = json!({
        "status": lvl.as_str(),
        "signer": format!("{:#x}", st.signer),
        "validator": format!("{:#x}", st.validator),
        "lastSubmitOkMs": st.last_submit_ok_ms,
        "notAuthorized": st.not_authorized,
        "idle": st.idle,
        "cycles": st.cycles_total,
        "markets": markets,
        "venues": venues,
    });
    (if lvl == Level::Down { 503 } else { 200 }, body.to_string())
}

pub fn render_metrics(st: &FeederStatus) -> String {
    let mut m = String::new();
    let _ = writeln!(m, "# TYPE feeder_cycles_total counter\nfeeder_cycles_total {}", st.cycles_total);
    let _ = writeln!(m, "# TYPE feeder_submit_total counter");
    for (result, n) in &st.submit_total {
        let _ = writeln!(m, "feeder_submit_total{{result=\"{result}\"}} {n}");
    }
    let _ = writeln!(m, "# TYPE feeder_source_errors_total counter");
    for (ex, n) in &st.source_errors_total {
        let _ = writeln!(m, "feeder_source_errors_total{{exchange=\"{}\"}} {n}", ex.name());
    }
    let _ = writeln!(m, "# TYPE feeder_market_omitted_total counter");
    for ((market, reason), n) in &st.market_omitted_total {
        let _ = writeln!(m, "feeder_market_omitted_total{{market=\"{market}\",reason=\"{reason}\"}} {n}");
    }
    let _ = writeln!(
        m,
        "# TYPE feeder_last_submit_ok_unixtime_ms gauge\nfeeder_last_submit_ok_unixtime_ms {}",
        st.last_submit_ok_ms.unwrap_or(0)
    );
    m
}

/// `(status, content type, body)` for one raw request.
pub fn route(request: &str, st: &FeederStatus, now_ms: u64) -> (u16, &'static str, String) {
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return (400, "application/json", r#"{"error":"bad request"}"#.into());
    };
    match (method, path) {
        ("GET", "/health") => {
            let (code, body) = health_json(st, now_ms);
            (code, "application/json", body)
        }
        ("GET", "/metrics") => (200, "text/plain; version=0.0.4", render_metrics(st)),
        _ => (404, "application/json", r#"{"error":"not found"}"#.into()),
    }
}

/// Accept loop on `listener` (bind it to loopback). One request per connection.
pub async fn serve<C: Clock + Clone + 'static>(
    listener: tokio::net::TcpListener,
    status: Arc<Mutex<FeederStatus>>,
    clock: C,
) -> std::io::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let (status, clock) = (status.clone(), clock.clone());
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let (code, ct, body) = {
                let st = status.lock().unwrap();
                route(&req, &st, clock.now_ms())
            };
            let text = match code {
                200 => "OK",
                400 => "Bad Request",
                404 => "Not Found",
                503 => "Service Unavailable",
                _ => "Unknown",
            };
            let resp = format!(
                "HTTP/1.1 {code} {text}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Exchange;
    use crate::feeder::{FeederStatus, MarketStatus, VenueStatus};
    use torus_types::FixedPoint;

    const NOW: u64 = 1_790_000_000_000;

    fn healthy() -> FeederStatus {
        let mut st = FeederStatus { interval_ms: 3_000, cycles_total: 5, last_submit_ok_ms: Some(NOW - 1_000), ..Default::default() };
        st.markets.insert(1, MarketStatus { last_price: Some(FixedPoint::from_raw(8_349_768_500_000)), sources: 7, weight: 11, omitted: None });
        st.venues.insert(Exchange::Binance, VenueStatus { ok: true, latency_ms: Some(40), ..Default::default() });
        st.submit_total.insert("ok".into(), 5);
        st.source_errors_total.insert(Exchange::Binance, 0);
        st
    }

    #[test]
    fn status_ok_degraded_down() {
        let st = healthy();
        assert_eq!(level(&st, NOW), Level::Ok);
        // Degraded: a market omitted, or a venue failing.
        let mut d = healthy();
        d.markets.get_mut(&1).unwrap().omitted = Some("too few sources (2 < 3)".into());
        assert_eq!(level(&d, NOW), Level::Degraded);
        let mut d = healthy();
        d.venues.insert(Exchange::Okx, VenueStatus { ok: false, last_error: Some("timeout".into()), ..Default::default() });
        assert_eq!(level(&d, NOW), Level::Degraded);
        // Down: no accepted submission within 3 x interval (boundary: exactly 9000 ms is still up).
        let mut d = healthy();
        d.last_submit_ok_ms = Some(NOW - 9_000);
        assert_eq!(level(&d, NOW), Level::Ok);
        d.last_submit_ok_ms = Some(NOW - 9_001);
        assert_eq!(level(&d, NOW), Level::Down);
        d.last_submit_ok_ms = None;
        assert_eq!(level(&d, NOW), Level::Down);
        // Down: NotAuthorized, even with a recent success.
        let mut d = healthy();
        d.not_authorized = Some("not an active validator or its signer".into());
        assert_eq!(level(&d, NOW), Level::Down);
    }

    #[test]
    fn health_json_shape() {
        let (code, body) = health_json(&healthy(), NOW);
        assert_eq!(code, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["lastSubmitOkMs"], NOW - 1_000);
        assert_eq!(v["markets"]["1"]["lastPrice"], "83497.68500000");
        assert_eq!(v["markets"]["1"]["sources"], 7);
        assert_eq!(v["markets"]["1"]["weight"], 11);
        assert!(v["markets"]["1"]["omitted"].is_null());
        assert_eq!(v["venues"]["binance"]["ok"], true);
        assert_eq!(v["venues"]["binance"]["latencyMs"], 40);
        assert!(v["signer"].as_str().unwrap().starts_with("0x"));
        let mut down = healthy();
        down.not_authorized = Some("x".into());
        let (code, body) = health_json(&down, NOW);
        assert_eq!(code, 503);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["status"], "down");
        assert_eq!(v["notAuthorized"], "x");
    }

    #[test]
    fn metrics_text_contains_counters() {
        let mut st = healthy();
        st.submit_total.insert("retryable".into(), 2);
        st.source_errors_total.insert(Exchange::Okx, 3);
        st.market_omitted_total.insert((9, "too_few_sources".into()), 4);
        let m = render_metrics(&st);
        for line in [
            "feeder_cycles_total 5",
            "feeder_submit_total{result=\"ok\"} 5",
            "feeder_submit_total{result=\"retryable\"} 2",
            "feeder_source_errors_total{exchange=\"okx\"} 3",
            "feeder_source_errors_total{exchange=\"binance\"} 0",
            "feeder_market_omitted_total{market=\"9\",reason=\"too_few_sources\"} 4",
            &format!("feeder_last_submit_ok_unixtime_ms {}", NOW - 1_000),
        ] {
            assert!(m.lines().any(|l| l == line), "missing {line:?} in\n{m}");
        }
        assert!(m.contains("# TYPE feeder_cycles_total counter"));
        assert!(m.contains("# TYPE feeder_last_submit_ok_unixtime_ms gauge"));
    }

    #[test]
    fn http_routes() {
        let st = healthy();
        let (code, ct, body) = route("GET /health HTTP/1.1\r\nHost: x\r\n\r\n", &st, NOW);
        assert_eq!((code, ct), (200, "application/json"));
        assert!(body.contains("\"status\":\"ok\""));
        let (code, ct, body) = route("GET /metrics HTTP/1.1\r\n\r\n", &st, NOW);
        assert_eq!((code, ct), (200, "text/plain; version=0.0.4"));
        assert!(body.contains("feeder_cycles_total 5"));
        assert_eq!(route("GET /nope HTTP/1.1\r\n\r\n", &st, NOW).0, 404);
        assert_eq!(route("POST /health HTTP/1.1\r\n\r\n", &st, NOW).0, 404);
        assert_eq!(route("garbage", &st, NOW).0, 400);
    }

    #[tokio::test]
    async fn serves_over_tcp() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let status = std::sync::Arc::new(std::sync::Mutex::new(healthy()));
        let clock = crate::fetch::FakeClock::new(NOW);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, status, clock));
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert!(out.starts_with("HTTP/1.1 200 OK\r\n"), "{out}");
        assert!(out.contains("feeder_cycles_total 5"));
    }
}
