# Writing Plan: Grafana Monitoring Dashboards + Alerting

**Spec:** [tech-req-grafana-monitoring.md](./tech-req-grafana-monitoring.md)
**Date:** 2026-04-16 (revised 2026-04-18)
**Estimated scope:** ~200 lines Rust (new metrics) + ~800 lines JSON/YAML (new dashboards + config)

---

## Step 0: Audit existing monitoring infrastructure

**Directory:** `monitoring/` (already exists — do NOT create `infra/`)

The repo already has a monitoring stack. Catalog before adding:

**Existing files:**
- `monitoring/prometheus.yml` — scrapes 4 validators on `:9090`, references `alerts/*.yml`
- `monitoring/dashboards/consensus.json` — consensus panels
- `monitoring/dashboards/execution.json` — EVM/block execution panels
- `monitoring/dashboards/network.json` — peer/gossip panels
- `monitoring/alerts/consensus.yml` — MissedBlocks, ConsensusStall, SlowBlockBuild, HighConsensusRounds, MempoolBacklog
- `monitoring/alerts/node.yml` — LowPeerCount, LowPeerCountCritical, DatabaseLargeWarning, DatabaseLargeCritical
- `monitoring/alerts/infrastructure.yml` — DiskSpaceHigh, DiskSpaceCritical, MemoryHigh, HighCPU (requires node_exporter)

**Already covered by existing alerts (do NOT duplicate):**
- NodeNotProducingBlocks → `MissedBlocks` in consensus.yml
- ConsensusStalledViews → `ConsensusStall` in consensus.yml
- NoPeers → `LowPeerCountCritical` in node.yml
- HighBlockBuildTime → `SlowBlockBuild` in consensus.yml
- DiskSpaceCritical → exists in infrastructure.yml
- MempoolBacklog → exists in consensus.yml

**Verify:** Review each existing file for coverage gaps before proceeding.

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

Metrics are currently hardcoded at `"0.0.0.0:9090"` (line ~378). Replace
with a CLI flag, keeping the same default to avoid breaking existing configs:

```rust
/// Metrics (Prometheus) listen address
#[arg(long, default_value = "0.0.0.0:9090")]
metrics_addr: SocketAddr,
```

Wire it to the existing `torus_telemetry::serve_metrics()` call. The existing
`monitoring/prometheus.yml` scrapes `:9090` — do NOT change the default port.

**Verify:** `cargo test -p torus-node`

---

## Step 6: Add Docker Compose for monitoring stack

**File:** `monitoring/docker-compose.yml`

Add a compose file to the existing `monitoring/` directory. Prometheus
mounts the existing `prometheus.yml` and `alerts/` directory. Grafana
auto-provisions dashboards and datasource from new provisioning configs.

```yaml
# monitoring/docker-compose.yml
services:
  prometheus:
    image: prom/prometheus:v2.51.0
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml:ro
      - ./alerts:/etc/prometheus/alerts:ro
    ports: ["9090:9090"]

  grafana:
    image: grafana/grafana:10.4.0
    volumes:
      - ./grafana/provisioning:/etc/grafana/provisioning:ro
      - ./dashboards:/var/lib/grafana/dashboards:ro
    ports: ["3000:3000"]
    environment:
      GF_AUTH_ANONYMOUS_ENABLED: "true"
      GF_AUTH_ANONYMOUS_ORG_ROLE: Viewer
```

**Verify:** `docker compose -f monitoring/docker-compose.yml config`

---

## Step 7: Grafana provisioning configs

**Files:**
- `monitoring/grafana/provisioning/datasources/prometheus.yaml`
- `monitoring/grafana/provisioning/dashboards/default.yaml`

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

Three dashboards already exist in `monitoring/dashboards/`. Extend those
and add the missing ones:

**Extend (add new panels for Step 1 metrics):**
- `monitoring/dashboards/consensus.json` — add `consensus_timeout_total` panel
- `monitoring/dashboards/network.json` — add `gossip_messages_received/sent` panels

**Create new:**
- `monitoring/dashboards/node-overview.json` — top-level summary: block height, peers, epoch, validator set size, mempool, DB size
- `monitoring/dashboards/trading.json` — orders_matched, liquidations_triggered, pruner_blocks_removed
- `monitoring/dashboards/rpc.json` — rpc_requests_total (by method/status), rpc_request_duration_seconds (p50/p95/p99)

Each dashboard follows Grafana JSON model. Panel layout per tech-req
Sections 2.1–2.5. Use `uid` based on dashboard name for stable links.

**Build approach:** Start from a minimal Grafana dashboard template,
add panels programmatically. Each panel references the `Prometheus`
datasource and uses the metric names from Step 1.

~800 lines of JSON across 3 new files + panel additions to 2 existing files.
Use the Grafana JSON model directly — no grafonnet or other generators.

**Verify:** `docker compose -f monitoring/docker-compose.yml up`, open
`localhost:3000`, verify all 5 dashboards load and show panels (data only
appears when a node is running).

---

## Step 9: Add missing alerting rules

Existing alerts already cover 10 of the 13 rules from the tech-req (see
Step 0 audit). Only add rules for the NEW metrics from Step 1.

**File:** `monitoring/alerts/consensus.yml` — append to existing group:
```yaml
      - alert: ConsensusTimeoutSpike
        expr: rate(torus_consensus_timeout_total[5m]) > 0.1
        for: 5m
        labels: { severity: warning }
        annotations:
          summary: "Consensus timeouts increasing"
```

**File:** `monitoring/alerts/node.yml` — append to existing group:
```yaml
      - alert: BlockHeightLag
        expr: torus_block_height < on() group_left max(torus_block_height) - 10
        for: 5m
        labels: { severity: warning }
        annotations:
          summary: "Node lagging >10 blocks behind cluster"
```

**New file:** `monitoring/alerts/trading.yml`
```yaml
groups:
  - name: torus_trading
    rules:
      - alert: LiquidationSpike
        expr: rate(torus_liquidations_triggered[5m]) > 10
        for: 2m
        labels: { severity: warning }
        annotations:
          summary: "High liquidation rate ({{ $value }}/s)"
```

~30 lines YAML total (not 80 — most rules already exist).

**Verify:** `docker run --rm -v ./monitoring/alerts:/rules prom/prometheus promtool check rules /rules/*.yml`

---

## Step 10: Smoke test

Start the full stack:
```
cargo run -p torus-node -- --genesis devnet-genesis.json --data-dir /tmp/torus-test &
docker compose -f monitoring/docker-compose.yml up -d
```

1. Verify Prometheus targets page (`localhost:9090/targets`) shows torus-node as UP
2. Verify all 5 dashboards load in Grafana (`localhost:3000`) without errors
3. Verify at least `torus_block_height` and `torus_peers_connected` show data
4. Trigger a test alert (stop the node, wait 30s, verify `MissedBlocks` fires)

---

## Dependency Chain

```
Step 0 (audit existing) ── all other steps

Step 1 (new metrics) ───┬── Step 2 (instrument epoch/trading/pruner)
                        ├── Step 3 (instrument RPC)
                        ├── Step 4 (instrument network)
                        └── Step 5 (--metrics-addr flag)

Step 6 (docker-compose) ── Step 7 (provisioning) ── Step 8 (dashboards)
                                                         │
Step 9 (alert rules) ─ independent of dashboards ────────┤
                                                         │
                                              Step 10 (smoke test)
```

Steps 1-5 (Rust) and Steps 6-9 (infra) are independent tracks.
Step 0 is a prerequisite for everything. Step 10 needs everything.
