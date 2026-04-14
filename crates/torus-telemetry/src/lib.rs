//! Torus-hyperBFT telemetry: Prometheus metrics registry, tracing subscriber, /health endpoint.

use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Global metrics for the Torus node.
///
/// Metric handles are `Clone + Send + Sync` (internally Arc-based).
/// Distribute cloned handles to subsystems; the registry stays here for encoding.
pub struct Metrics {
    registry: Registry,

    // Block metrics
    pub blocks_committed: Counter,
    pub block_height: Gauge,
    pub block_build_seconds: Histogram,

    // Transaction metrics
    pub evm_txs_processed: Counter,
    pub native_actions_processed: Counter,

    // Consensus metrics
    pub consensus_rounds: Counter,
    pub consensus_view: Gauge,

    // State metrics
    pub state_root_compute_seconds: Histogram,

    // Mempool metrics
    pub mempool_evm_size: Gauge,
    pub mempool_native_size: Gauge,

    // Network metrics
    pub peers_connected: Gauge,

    // Database metrics
    pub db_size_bytes: Gauge,
}

impl Metrics {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let blocks_committed = Counter::default();
        registry.register(
            "torus_blocks_committed",
            "Total number of committed blocks",
            blocks_committed.clone(),
        );

        let block_height = Gauge::default();
        registry.register(
            "torus_block_height",
            "Current block height",
            block_height.clone(),
        );

        let block_build_seconds = Histogram::new(exponential_buckets(0.001, 2.0, 15));
        registry.register(
            "torus_block_build_seconds",
            "Time to build a block",
            block_build_seconds.clone(),
        );

        let evm_txs_processed = Counter::default();
        registry.register(
            "torus_evm_txs_processed",
            "Total EVM transactions processed",
            evm_txs_processed.clone(),
        );

        let native_actions_processed = Counter::default();
        registry.register(
            "torus_native_actions_processed",
            "Total native actions processed",
            native_actions_processed.clone(),
        );

        let consensus_rounds = Counter::default();
        registry.register(
            "torus_consensus_rounds",
            "Total consensus rounds",
            consensus_rounds.clone(),
        );

        let consensus_view = Gauge::default();
        registry.register(
            "torus_consensus_view",
            "Current consensus view number",
            consensus_view.clone(),
        );

        let state_root_compute_seconds = Histogram::new(exponential_buckets(0.0001, 2.0, 15));
        registry.register(
            "torus_state_root_compute_seconds",
            "Time to compute state root",
            state_root_compute_seconds.clone(),
        );

        let mempool_evm_size = Gauge::default();
        registry.register(
            "torus_mempool_evm_size",
            "Number of pending EVM transactions",
            mempool_evm_size.clone(),
        );

        let mempool_native_size = Gauge::default();
        registry.register(
            "torus_mempool_native_size",
            "Number of pending native actions",
            mempool_native_size.clone(),
        );

        let peers_connected = Gauge::default();
        registry.register(
            "torus_peers_connected",
            "Number of connected peers",
            peers_connected.clone(),
        );

        let db_size_bytes = Gauge::default();
        registry.register(
            "torus_db_size_bytes",
            "Total RocksDB data directory size in bytes",
            db_size_bytes.clone(),
        );

        Self {
            registry,
            blocks_committed,
            block_height,
            block_build_seconds,
            evm_txs_processed,
            native_actions_processed,
            consensus_rounds,
            consensus_view,
            state_root_compute_seconds,
            mempool_evm_size,
            mempool_native_size,
            peers_connected,
            db_size_bytes,
        }
    }

    /// Encode all registered metrics in OpenMetrics text format.
    pub fn encode(&self) -> String {
        let mut buf = String::new();
        encode(&mut buf, &self.registry).expect("metrics encoding should not fail");
        buf
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize the global tracing subscriber.
///
/// - Reads `RUST_LOG` env var for filtering (default: `info`).
/// - `json = true` emits structured JSON logs (for production).
/// - `json = false` emits human-readable logs (for development).
pub fn init_tracing(json: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if json {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json())
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .init();
    }
}

/// Serve `/health` and `/metrics` endpoints over HTTP.
///
/// - `GET /health` — returns 200 OK (liveness probe).
/// - `GET /metrics` — returns Prometheus/OpenMetrics text format.
pub async fn serve_metrics(
    addr: std::net::SocketAddr,
    metrics: Arc<Metrics>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "telemetry server listening");

    loop {
        let (mut stream, _peer) = listener.accept().await?;
        let metrics = metrics.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = match stream.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };
            let request = String::from_utf8_lossy(&buf[..n]);

            let response = if request.starts_with("GET /health") {
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 3\r\n\r\nOK\n"
                    .to_string()
            } else if request.starts_with("GET /metrics") {
                let body = metrics.encode();
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/openmetrics-text; version=1.0.0; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body,
                )
            } else {
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
            };

            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_encode_nonempty() {
        let m = Metrics::new();
        m.blocks_committed.inc();
        let encoded = m.encode();
        assert!(encoded.contains("torus_blocks_committed"));
    }
}
