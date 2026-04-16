# Technical Requirements: Grafana Monitoring Dashboards (Section 3.5.5 + 3.5.8)

**Date:** 2026-04-16
**Status:** Draft v1.0
**Parent:** [implementation-plan.md](./implementation-plan.md) Tasks 3.5.5, 3.5.8
**Depends on:** torus-telemetry (1.1.4)

---

## Summary

Create Grafana dashboard JSON files and Prometheus alerting rules for Torus
node operators. The telemetry crate already exports 13 Prometheus metrics via
a `/metrics` HTTP endpoint. This task adds the visualization and alerting
layer on top.

**Key decisions:**
- Dashboard JSON files committed to `infra/grafana/dashboards/`
- Alert rules as Prometheus YAML in `infra/prometheus/rules/`
- Docker Compose additions for Prometheus + Grafana (dev/testnet use)
- No new Rust code in torus-telemetry unless gaps found during dashboard design
- Provisioning via Grafana file provisioning (no API, no manual import)

---

## 1. Existing Metrics Inventory

### 1.1 Current Metrics (torus-telemetry/src/lib.rs)

| Metric | Type | Description |
|---|---|---|
| `blocks_committed` | Counter | Total blocks committed |
| `block_height` | Gauge | Current block height |
| `block_build_seconds` | Histogram | Block build time distribution |
| `evm_txs_processed` | Counter | Total EVM transactions |
| `native_actions_processed` | Counter | Total native actions |
| `consensus_rounds` | Counter | Total consensus rounds |
| `consensus_view` | Gauge | Current consensus view number |
| `state_root_compute_seconds` | Histogram | State root computation time |
| `mempool_evm_size` | Gauge | EVM mempool pending count |
| `mempool_native_size` | Gauge | Native mempool pending count |
| `peers_connected` | Gauge | Connected P2P peers |
| `db_size_bytes` | Gauge | RocksDB total disk usage |

### 1.2 Missing Metrics (to add in torus-telemetry)

| Metric | Type | Where to Instrument | Why |
|---|---|---|---|
| `epoch_number` | Gauge | native_executor.rs (epoch boundary) | Track epoch transitions |
| `validator_set_size` | Gauge | native_executor.rs (epoch boundary) | Monitor validator count |
| `orders_matched` | Counter | order_book.rs (match engine) | Trading volume signal |
| `liquidations_triggered` | Counter | liquidation.rs | Risk monitoring |
| `pruner_blocks_removed` | Counter | pruner.rs | Pruner health |
| `rpc_request_duration_seconds` | Histogram | torus-rpc (middleware) | RPC latency |
| `rpc_requests_total` | Counter | torus-rpc (middleware) | RPC load |
| `gossip_messages_received` | Counter | torus-network (behaviour.rs) | Network health |
| `gossip_messages_sent` | Counter | torus-network (behaviour.rs) | Network health |
| `block_transactions_count` | Histogram | bridge (commit) | Tx density per block |
| `consensus_timeout_total` | Counter | hotstuff_rs (pacemaker) | Consensus stall signal |

---

## 2. Dashboard Specifications

### 2.1 Node Overview Dashboard

**Audience:** Node operators. At-a-glance health check.

**Panels (2x4 grid):**

| Panel | Metric(s) | Visualization |
|---|---|---|
| Block Height | `block_height` | Stat (big number) |
| Epoch | `epoch_number` | Stat |
| Peers | `peers_connected` | Gauge (0-30 scale, green >3) |
| DB Size | `db_size_bytes` | Stat (auto-format GB) |
| Blocks/min | `rate(blocks_committed[5m])` | Time series graph |
| TPS | `rate(evm_txs_processed[1m]) + rate(native_actions_processed[1m])` | Time series |
| Block Build Time | `block_build_seconds` (p50, p95, p99) | Time series |
| State Root Time | `state_root_compute_seconds` (p50, p95) | Time series |

### 2.2 Consensus Dashboard

**Audience:** Validators and protocol developers.

**Panels:**

| Panel | Metric(s) | Visualization |
|---|---|---|
| Consensus View | `consensus_view` | Stat |
| Rounds/min | `rate(consensus_rounds[5m])` | Time series |
| View Gaps | `consensus_view - block_height` | Time series (should be small) |
| Timeouts | `rate(consensus_timeout_total[5m])` | Time series (should be ~0) |
| Validator Set Size | `validator_set_size` | Stat |
| Block Height vs Peers | `block_height` per instance | Multi-series (detect lag) |

### 2.3 Trading Dashboard

**Audience:** Protocol team monitoring exchange health.

**Panels:**

| Panel | Metric(s) | Visualization |
|---|---|---|
| Orders Matched/min | `rate(orders_matched[5m])` | Time series |
| Liquidations | `rate(liquidations_triggered[5m])` | Time series |
| Native Actions/min | `rate(native_actions_processed[5m])` | Time series |
| Mempool Depth | `mempool_evm_size` + `mempool_native_size` | Stacked area |
| Native Mempool | `mempool_native_size` | Gauge |

### 2.4 RPC Dashboard

**Audience:** Node operators and API consumers.

**Panels:**

| Panel | Metric(s) | Visualization |
|---|---|---|
| RPC QPS | `rate(rpc_requests_total[1m])` | Time series |
| RPC Latency | `rpc_request_duration_seconds` (p50, p95, p99) | Time series |
| RPC Errors | `rate(rpc_requests_total{status="error"}[5m])` | Time series |

### 2.5 Network Dashboard

**Audience:** Node operators debugging connectivity.

**Panels:**

| Panel | Metric(s) | Visualization |
|---|---|---|
| Peers | `peers_connected` | Gauge |
| Gossip In/Out | `rate(gossip_messages_received[1m])` / `rate(gossip_messages_sent[1m])` | Dual time series |

---

## 3. Alerting Rules (3.5.8)

### 3.1 Critical Alerts

| Alert | Condition | For | Severity |
|---|---|---|---|
| `NodeNotProducingBlocks` | `rate(blocks_committed[5m]) == 0` | 5m | critical |
| `ConsensusStalledViews` | `increase(consensus_view[10m]) == 0` | 10m | critical |
| `NoPeers` | `peers_connected == 0` | 2m | critical |
| `DiskSpaceCritical` | `db_size_bytes > 0.9 * <disk_total>` | 5m | critical |

### 3.2 Warning Alerts

| Alert | Condition | For | Severity |
|---|---|---|---|
| `HighBlockBuildTime` | `histogram_quantile(0.95, block_build_seconds) > 1.0` | 5m | warning |
| `HighStateRootTime` | `histogram_quantile(0.95, state_root_compute_seconds) > 0.5` | 5m | warning |
| `MempoolBacklog` | `mempool_evm_size + mempool_native_size > 10000` | 5m | warning |
| `PeerCountLow` | `peers_connected < 3` | 5m | warning |
| `ConsensusTimeouts` | `rate(consensus_timeout_total[5m]) > 0.1` | 5m | warning |
| `DiskSpaceWarning` | `db_size_bytes > 0.8 * <disk_total>` | 5m | warning |
| `BlockHeightLag` | node `block_height` < max(`block_height`) across cluster - 10 | 5m | warning |

---

## 4. Infrastructure Files

### 4.1 Directory Layout

```
infra/
  grafana/
    provisioning/
      dashboards/
        default.yaml            # file provisioner config
      datasources/
        prometheus.yaml         # auto-configure Prometheus datasource
    dashboards/
      node-overview.json
      consensus.json
      trading.json
      rpc.json
      network.json
  prometheus/
    prometheus.yml              # scrape config
    rules/
      torus-alerts.yml          # alerting rules
  docker-compose.monitoring.yml # Prometheus + Grafana containers
```

### 4.2 Docker Compose Addition

```yaml
# docker-compose.monitoring.yml
services:
  prometheus:
    image: prom/prometheus:v2.51.0
    volumes:
      - ./infra/prometheus:/etc/prometheus
    ports:
      - "9090:9090"

  grafana:
    image: grafana/grafana:10.4.0
    volumes:
      - ./infra/grafana/provisioning:/etc/grafana/provisioning
      - ./infra/grafana/dashboards:/var/lib/grafana/dashboards
    ports:
      - "3000:3000"
    environment:
      GF_AUTH_ANONYMOUS_ENABLED: "true"
      GF_AUTH_ANONYMOUS_ORG_ROLE: Viewer
```

### 4.3 Prometheus Scrape Config

```yaml
# prometheus.yml
global:
  scrape_interval: 15s

scrape_configs:
  - job_name: torus-node
    static_targets:
      - targets: ['host.docker.internal:9100']  # torus-node metrics port
    # For multi-node testnet, use service discovery or static list

rule_files:
  - /etc/prometheus/rules/torus-alerts.yml
```

---

## 5. Metrics Port

`torus-telemetry` currently serves `/metrics` and `/health` on a TCP listener.
Verify the port is configurable via CLI flag. If not, add:

```
--metrics-addr <SocketAddr>    # default 0.0.0.0:9100
```

to `torus-node` CLI args. Separate from `--rpc-addr` (8545) so metrics
can be firewalled differently.

---

## 6. Testing

| Test | Verifies |
|---|---|
| `prometheus_scrape_format` | `/metrics` endpoint returns valid Prometheus text format |
| `grafana_dashboard_json_valid` | Each dashboard JSON passes Grafana schema validation |
| `alert_rules_valid` | `promtool check rules torus-alerts.yml` passes |
| `docker_compose_up` | `docker compose -f docker-compose.monitoring.yml up` starts cleanly |
| `new_metrics_exported` | Each new metric appears in `/metrics` output after triggering its code path |

---

## 7. Files

| File | Change | Scope |
|---|---|---|
| `crates/torus-telemetry/src/lib.rs` | Add 11 new metrics | Medium |
| `crates/torus-core/src/order_book.rs` | Instrument `orders_matched` counter | Small |
| `crates/torus-core/src/liquidation.rs` | Instrument `liquidations_triggered` counter | Small |
| `crates/torus-bridge/src/native_executor.rs` | Instrument `epoch_number`, `validator_set_size` | Small |
| `crates/torus-state/src/pruner.rs` | Instrument `pruner_blocks_removed` | Small |
| `crates/torus-rpc/src/lib.rs` | RPC middleware for `rpc_request_*` metrics | Medium |
| `crates/torus-network/src/behaviour.rs` | Instrument `gossip_messages_*` | Small |
| `crates/torus-node/src/main.rs` | Add `--metrics-addr` flag if missing | Small |
| `infra/grafana/dashboards/*.json` | 5 dashboard files | Medium |
| `infra/grafana/provisioning/**` | Provisioning configs | Small |
| `infra/prometheus/**` | Scrape config + alert rules | Small |
| `infra/docker-compose.monitoring.yml` | Monitoring stack | Small |
