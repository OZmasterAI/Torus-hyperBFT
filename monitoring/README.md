# Torus Monitoring

Grafana dashboards for monitoring Torus hyperBFT nodes.

## Dashboards

| File | Description |
|------|-------------|
| `dashboards/consensus.json` | Block height, commit rate, build time, consensus view/rounds, state root compute time |
| `dashboards/execution.json` | EVM tx rate, native action rate, combined throughput, mempool sizes |
| `dashboards/network.json` | Connected peers, CPU/memory/disk (requires node_exporter) |

## Prerequisites

- **Prometheus** scraping the Torus node's `/metrics` endpoint
- **Grafana** (v10+) with a Prometheus datasource configured
- **node_exporter** (optional) for CPU, memory, and disk panels in the Network dashboard

## Prometheus Scrape Config

Add to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: 'torus'
    scrape_interval: 5s
    static_configs:
      - targets: ['localhost:9090']  # Torus node telemetry port

  # Optional: for infrastructure panels
  - job_name: 'node'
    static_configs:
      - targets: ['localhost:9100']  # node_exporter
```

## Grafana Setup

### Option 1: File Provisioning

Create `/etc/grafana/provisioning/dashboards/torus.yaml`:

```yaml
apiVersion: 1
providers:
  - name: Torus
    folder: Torus
    type: file
    options:
      path: /path/to/monitoring/dashboards
```

### Option 2: UI Import

1. Open Grafana (default: http://localhost:3000)
2. Go to Dashboards > Import
3. Upload each JSON file from `dashboards/`
4. Select your Prometheus datasource when prompted

## Metrics Reference

All metrics are exported by the `torus-telemetry` crate at `GET /metrics`.

| Metric | Type | Description |
|--------|------|-------------|
| `torus_block_height` | Gauge | Current block height |
| `torus_blocks_committed` | Counter | Total committed blocks |
| `torus_block_build_seconds` | Histogram | Block build time |
| `torus_consensus_view` | Gauge | Current consensus view |
| `torus_consensus_rounds` | Counter | Total consensus rounds |
| `torus_state_root_compute_seconds` | Histogram | State root computation time |
| `torus_evm_txs_processed` | Counter | Total EVM transactions processed |
| `torus_native_actions_processed` | Counter | Total native actions processed |
| `torus_mempool_evm_size` | Gauge | Pending EVM transactions |
| `torus_mempool_native_size` | Gauge | Pending native actions |
| `torus_peers_connected` | Gauge | Number of connected peers |
