# Writing Plan: Grafana Monitoring Dashboards + Alerting

**Spec:** [tech-req-grafana-monitoring.md](./tech-req-grafana-monitoring.md)
**Date:** 2026-04-16
**Estimated scope:** ~200 lines Rust (new metrics) + ~1500 lines JSON/YAML (dashboards + config)

---

## Step 1: Add missing metrics to torus-telemetry

**File:** `crates/torus-telemetry/src/lib.rs`

Add 11 new metrics to the `Metrics` struct (after line ~44):

```rust
// Epoch metrics
pub epoch_number: Gauge,
pub validator_set_size: Gauge,

// Trading metrics
pub orders_matched: Counter,
pub liquidations_triggered: Counter,

// Pruner metrics
pub pruner_blocks_removed: Counter,

// RPC metrics
pub rpc_requests_total: Counter,       // labeled by method + status
pub rpc_request_duration_seconds: Histogram,

// Network metrics
pub gossip_messages_received: Counter,
pub gossip_messages_sent: Counter,

// Block detail metrics
pub block_transactions_count: Histogram,

// Consensus metrics
pub consensus_timeout_total: Counter,
```

Register each in `Metrics::new()` following the existing pattern.
The labeled counter (`rpc_requests_total`) uses `Family<Vec<(String, String)>, Counter>`.

**Verify:** `cargo check -p torus-telemetry`

---

## Step 2: Instrument epoch + trading + pruner

**Files:**
- `crates/torus-bridge/src/native_executor.rs` — in `process_epoch_boundary()`,
  after successful rotation: `metrics.epoch_number.set(new_epoch)`,
  `metrics.validator_set_size.set(new_set.validators.len())`
- `crates/torus-core/src/order_book.rs` — in the match loop,
  increment `metrics.orders_matched` per fill
- `crates/torus-core/src/liquidation.rs` — in liquidation trigger,
  increment `metrics.liquidations_triggered`
- `crates/torus-state/src/pruner.rs` — after pruning a batch,
  add count to `metrics.pruner_blocks_removed`

Each instrumentation site needs a `Metrics` handle passed in. Check if the
call sites already receive a `Metrics` reference — if not, thread it through
the constructor. The `Metrics` struct is `Clone` (Arc-backed).

**Verify:** `cargo check -p torus-bridge -p torus-core -p torus-state`

---

## Step 3: Instrument RPC middleware

**File:** `crates/torus-rpc/src/lib.rs`

jsonrpsee supports middleware via `RpcServiceBuilder`. Add a layer that:
1. Starts a timer before each call
2. After the call, records `rpc_requests_total` (method, status) and
   `rpc_request_duration_seconds`

```rust
use jsonrpsee::server::middleware::rpc::RpcServiceT;

pub struct MetricsLayer { metrics: Metrics }

impl<S: RpcServiceT> tower::Layer<S> for MetricsLayer {
    // wrap each call: record method name, duration, success/error
}
```

~50 lines. Register the layer in `RpcServer::start()`.

**Verify:** `cargo check -p torus-rpc`

---

## Step 4: Instrument network gossip

**File:** `crates/torus-network/src/behaviour.rs`

In the GossipSub message handler:
- On `GossipsubEvent::Message { .. }` → increment `gossip_messages_received`
- On successful `publish()` → increment `gossip_messages_sent`

Thread `Metrics` handle into the network behaviour constructor.

**Verify:** `cargo check -p torus-network`

---

## Step 5: Add --metrics-addr flag

**File:** `crates/torus-node/src/main.rs`

Check if a `--metrics-addr` flag already exists. If not, add:

```rust
/// Metrics (Prometheus) listen address
#[arg(long, default_value = "0.0.0.0:9100")]
metrics_addr: SocketAddr,
```

Wire it to the telemetry HTTP listener that serves `/metrics` and `/health`.

**Verify:** `cargo test -p torus-node`

---

## Step 6: Create infra directory + Docker Compose

**Files:**
- `infra/docker-compose.monitoring.yml`
- `infra/prometheus/prometheus.yml`

Create the monitoring stack. Prometheus scrapes `host.docker.internal:9100`
(or configurable targets). Grafana auto-provisions dashboards and datasource.

```yaml
# docker-compose.monitoring.yml
services:
  prometheus:
    image: prom/prometheus:v2.51.0
    volumes:
      - ./prometheus:/etc/prometheus
    ports: ["9090:9090"]

  grafana:
    image: grafana/grafana:10.4.0
    volumes:
      - ./grafana/provisioning:/etc/grafana/provisioning
      - ./grafana/dashboards:/var/lib/grafana/dashboards
    ports: ["3000:3000"]
    environment:
      GF_AUTH_ANONYMOUS_ENABLED: "true"
      GF_AUTH_ANONYMOUS_ORG_ROLE: Viewer
```

**Verify:** `docker compose -f infra/docker-compose.monitoring.yml config`

---

## Step 7: Grafana provisioning configs

**Files:**
- `infra/grafana/provisioning/datasources/prometheus.yaml`
- `infra/grafana/provisioning/dashboards/default.yaml`

**datasources/prometheus.yaml:**
```yaml
apiVersion: 1
datasources:
  - name: Prometheus
    type: prometheus
    access: proxy
    url: http://prometheus:9090
    isDefault: true
```

**dashboards/default.yaml:**
```yaml
apiVersion: 1
providers:
  - name: default
    type: file
    options:
      path: /var/lib/grafana/dashboards
```

Small config files, ~10 lines each.

**Verify:** Files exist and are valid YAML.

---

## Step 8: Dashboard JSON files

**Files:**
- `infra/grafana/dashboards/node-overview.json`
- `infra/grafana/dashboards/consensus.json`
- `infra/grafana/dashboards/trading.json`
- `infra/grafana/dashboards/rpc.json`
- `infra/grafana/dashboards/network.json`

Each dashboard follows Grafana JSON model. Panel layout per tech-req
Sections 2.1–2.5. Use `uid` based on dashboard name for stable links.

**Build approach:** Start from a minimal Grafana dashboard template,
add panels programmatically. Each panel references the `Prometheus`
datasource and uses the metric names from Step 1.

This is the largest step (~1200 lines of JSON across 5 files). Use the
Grafana JSON model directly — no grafonnet or other generators.

**Verify:** `docker compose up`, open `localhost:3000`, verify all 5
dashboards load and show panels (data only appears when a node is running).

---

## Step 9: Prometheus alerting rules

**File:** `infra/prometheus/rules/torus-alerts.yml`

```yaml
groups:
  - name: torus-critical
    rules:
      - alert: NodeNotProducingBlocks
        expr: rate(blocks_committed[5m]) == 0
        for: 5m
        labels: { severity: critical }
        annotations:
          summary: "Node {{ $labels.instance }} not producing blocks"

      - alert: ConsensusStalledViews
        expr: increase(consensus_view[10m]) == 0
        for: 10m
        labels: { severity: critical }

      - alert: NoPeers
        expr: peers_connected == 0
        for: 2m
        labels: { severity: critical }

  - name: torus-warnings
    rules:
      - alert: HighBlockBuildTime
        expr: histogram_quantile(0.95, rate(block_build_seconds_bucket[5m])) > 1.0
        for: 5m
        labels: { severity: warning }

      # ... (7 warning rules from tech-req Section 3.2)
```

~80 lines YAML.

**Verify:** `promtool check rules infra/prometheus/rules/torus-alerts.yml`
(install promtool or run via Docker: `docker run prom/prometheus promtool check rules ...`)

---

## Step 10: Smoke test

Start the full stack:
```
cargo run -p torus-node -- --genesis devnet-genesis.json --data-dir /tmp/torus-test &
docker compose -f infra/docker-compose.monitoring.yml up -d
```

1. Verify Prometheus targets page shows torus-node as UP
2. Verify all 5 Grafana dashboards load without errors
3. Verify at least `block_height` and `peers_connected` show data
4. Trigger a test alert (stop the node, wait 5m, verify `NodeNotProducingBlocks` fires)

---

## Dependency Chain

```
Step 1 (new metrics) ───┬── Step 2 (instrument epoch/trading/pruner)
                        ├── Step 3 (instrument RPC)
                        ├── Step 4 (instrument network)
                        └── Step 5 (--metrics-addr)

Step 6 (docker-compose) ── Step 7 (provisioning) ── Step 8 (dashboards)
                                                         │
Step 9 (alert rules) ─ independent of dashboards ────────┤
                                                         │
                                              Step 10 (smoke test)
```

Steps 1-5 (Rust) and Steps 6-9 (infra) are independent tracks.
Step 10 needs everything.
