# Monitoring Setup

## Overview

Torus-hyperBFT exports Prometheus metrics on port 9090 (hardcoded). The
telemetry server provides two endpoints:

- `GET /health` — Returns `200 OK` (liveness probe)
- `GET /metrics` — Returns OpenMetrics text format

## Prometheus Configuration

### Scrape Config

```yaml
# prometheus.yml
scrape_configs:
  - job_name: "torus"
    scrape_interval: 15s
    static_configs:
      - targets: ["<node-ip>:9090"]

  # Optional: node_exporter for infrastructure metrics
  - job_name: "node"
    scrape_interval: 15s
    static_configs:
      - targets: ["<node-ip>:9100"]
```

### Available Metrics

All metrics are prefixed with `torus_`:

| Metric | Type | Description |
|--------|------|-------------|
| `torus_blocks_committed_total` | Counter | Total committed blocks |
| `torus_block_height` | Gauge | Current block height |
| `torus_block_build_seconds` | Histogram | Block build time distribution |
| `torus_evm_txs_processed_total` | Counter | Total EVM transactions processed |
| `torus_native_actions_processed_total` | Counter | Total native actions processed |
| `torus_consensus_rounds_total` | Counter | Total consensus rounds |
| `torus_consensus_view` | Gauge | Current consensus view number |
| `torus_state_root_compute_seconds` | Histogram | State root computation time |
| `torus_mempool_evm_size` | Gauge | Pending EVM transactions in mempool |
| `torus_mempool_native_size` | Gauge | Pending native actions in mempool |
| `torus_peers_connected` | Gauge | Number of connected P2P peers |
| `torus_db_size_bytes` | Gauge | Total RocksDB data directory size in bytes |

## Grafana Dashboards

### Import

1. Open Grafana web UI
2. Go to Dashboards > Import
3. Upload JSON files from `monitoring/dashboards/`:
   - `consensus.json` — Block production and consensus metrics
   - `execution.json` — EVM execution and state root metrics
   - `network.json` — Peer connectivity and network metrics
4. Select your Prometheus data source

### Dashboard Panels

#### Consensus Dashboard
- Block height over time
- Consensus view progression
- Consensus rounds rate
- Block build time percentiles

#### Execution Dashboard
- EVM transactions per block
- Gas usage over time
- State root computation time
- Mempool size

#### Network Dashboard
- Connected peer count
- Peer churn rate

## Alerting

### Setup AlertManager

See `monitoring/alerts/README.md` for detailed setup instructions.

### Alert Rule Files

| File | Source | Description |
|------|--------|-------------|
| `monitoring/alerts/consensus.yml` | torus-telemetry | Block stalls, consensus issues |
| `monitoring/alerts/node.yml` | torus-telemetry | Peer count, database size |
| `monitoring/alerts/infrastructure.yml` | node_exporter | Disk, memory, CPU |

### Load Rules in Prometheus

```yaml
# prometheus.yml
rule_files:
  - "/path/to/monitoring/alerts/*.yml"

alerting:
  alertmanagers:
    - static_configs:
        - targets: ["localhost:9093"]
```

### Verify Rules

If `promtool` is installed:

```bash
promtool check rules monitoring/alerts/consensus.yml
promtool check rules monitoring/alerts/node.yml
promtool check rules monitoring/alerts/infrastructure.yml
```

## Health Checks

### Liveness Probe

```bash
curl -s http://localhost:9090/health
# Returns: OK
```

### Readiness Check

Check that the node is producing or tracking blocks:

```bash
curl -s http://localhost:8545 \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'
```

### Kubernetes Probes

```yaml
livenessProbe:
  httpGet:
    path: /health
    port: 9090
  initialDelaySeconds: 10
  periodSeconds: 30

readinessProbe:
  httpGet:
    path: /health
    port: 9090
  initialDelaySeconds: 30
  periodSeconds: 10
```
