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
| `torus_mempool_oracle_dropped_total{reason}` | Counter | Oracle submissions evicted from or refused by the native pool (plan 9.14 C). `replaced_by_newer`: a newer submission evicted the validator's oldest pooled one at the per-validator cap (4) (replaces the unlabelled `torus_mempool_oracle_evicted_total`, removed s104); `cap_rejected`: refused at the per-validator cap, older than every pooled one; `pool_full`: refused by a pool holding only oracle submissions; `expired`: aged out of the nonce window while pooled. Committed submissions and normal entries an oracle submission evicts are not counted; node-local |
| `torus_rpc_ip_rejects_total{kind,action}` | Counter | RPC calls refused by the per-IP weight limit (anti-spam item D). `kind`: `call` (one JSON-RPC call) or `batch` (a JSON-RPC batch, refused whole, counted once). `action`: `oracle` when the call, or any call of the batch, submits a `SubmitOraclePrices` (read from the payload head: bincode tag or canonical-JSON prefix), else `other`. Sum over `action` for the per-`kind` total; node-local |
| `torus_liquidator_vault_deficit` | Gauge | Liquidator vault's negative cash in tokens (0 when not negative); set after each liquidation pass, reads 0 after a restart until the next pass |
| `torus_liquidation_step_seconds` | Histogram | Wall time of the liquidation step (`run_liquidations`) per native block; `_sum`/`_count` deltas give ms per block |
| `torus_liquidations_stage1_total` | Counter | Accounts acted on by stage 1 (reduce-only IOC orders into the book); an account under maintenance over several blocks counts once per block |
| `torus_liquidations_backstop_total` | Counter | Accounts backstopped (marked positions and collateral moved to the liquidator vault) |
| `torus_liquidations_adl_total` | Counter | ADL runs per block: +1 per account classified ADL and acted on, +1 when the liquidator vault is ADL'd. Under adl-budget P2 the account's marked positions move to the ADL escrows in that block. Logs: one info line `liquidation: ADL to escrow` per (account, market) with size, base, bankruptcy price and price; each escrow close at debug (`liquidation: ADL close`) |
| `torus_liquidation_scanned_total` | Counter | Accounts the step classified (vault excluded) |
| `torus_liquidation_acted_total` | Counter | Accounts acted on within the 64-per-block act budget (vault excluded; equals `torus_liquidations_triggered_total`) |
| `torus_liquidation_pending` | Gauge | After each step: accounts holding a pending row (acted on and still under maintenance, carried over until rescanned; the vault while ADL-able) UNION the scan-window candidates the act budget left unclassified. An upper bound (the unclassified ones may be healthy); liquidatable accounts the round-robin has not reached and no budget cut deferred are not counted. 0 after a restart until the next step. The node re-counts the pending rows only on a step that changed one (and on its first step) |
| `torus_liquidation_deferred` | Gauge | After each step: the scan-window candidates left unclassified because the act budget ran out (0 when it held) |
| `torus_liquidation_adl_queue` | Gauge | After each step: ADL obligation rows (`0x07`) the escrows still owe (rows, not accounts); > 0 keeps the step running in empty blocks. Reads 0 after a restart until the next step |
| `torus_liquidation_adl_queue_deficit` | Gauge | After each step: Σ over both ADL escrows of available + UPnL at the step's marks (signed tokens): what the queued obligations cost at the mark |
| `torus_liquidation_adl_escrow_notional` | Gauge | After each step: notional of both ADL escrows' positions at the step's marks (tokens; unmarked: entry) |
| `torus_liquidation_adl_work_total` | Counter | ADL drain work units (rows visited + traders a ranking examined + candidates read + escrow-pairing rows); the per-block budget is `ADL_WORK_PER_BLOCK`. Each step's `adl_work` is also on the `liquidation step` info line |
| `torus_liquidation_adl_dust` | Gauge | Cumulative (since start) escrow dust swept to the liquidator vault, signed tokens; each sweep logs `liquidation: ADL escrow dust to the vault` (error line at ≥ 1 token) |
| `torus_liquidation_adl_pairing` | Gauge | Cumulative (since start) liquidator-vault amounts of escrow-vs-escrow pairings (real counterparties exhausted), signed tokens; each logs `liquidation: ADL escrow pairing` |
| `torus_liquidation_value_sum` | Gauge | Proof-only, `TORUS_LIQ_VALUE_SUM=1`: Σ over all accounts of available + order margin + UPnL at the step's marks (tokens), after each step, also logged as `liquidation: value sum` (height, value_sum). Walks every balance and position row per native block: off in production |
| `torus_exec_evm_blocks_total` | Counter | Executed blocks with EVM txs, on either path (an all-skipped EVM section included); node-local |
| `torus_exec_evm_fast_path_blocks_total` | Counter | Blocks with EVM txs executed pipelined (`TORUS_EVM_FAST_PATH` with the exec pipeline; no drain). Divided by `torus_exec_evm_blocks_total`: the pipelined share at EVM heights. 0 with the flag off |
| `torus_exec_evm_idle_before_seconds` | Histogram | Per EVM block: the exec thread's time from the previous block's end to this block's execution start, its drain (forced checkpoint + flush-worker wait) included; also the wait for the block when execution is caught up |
| `torus_exec_evm_block_seconds` | Histogram | `torus_exec_block_seconds` of the blocks with EVM txs only (exec time at EVM heights vs the rest) |
| `torus_exec_evm_seconds` | Histogram | The EVM section on the exec thread (validate, plus on the serial path the bundle commit and block metadata), observed every block |
| `torus_flush_worker_evm_trie_seconds` | Histogram | The flush worker's EVM trie step (hashed mirror + trie nodes) per fast-path EVM block at interval 0, or per checkpoint whose window holds one. Never observed with `TORUS_EVM_FAST_PATH` off (the EVM root runs on the exec thread) |
| `torus_feature_active{feature,id}` | Gauge | P0.0 feature gating: 1 when the last applied block's header carries the feature, else 0; one series per feature this binary knows (`feature`: genesis name, `id`: bit). Follows the header, not the node's schedule |
| `torus_feature_activation_height{feature,id}` | Gauge | P0.0: the height a scheduled feature switches on in this node's schedule (genesis `consensus.activations` + the binary's built-in table); unscheduled features have no series |
| `torus_feature_next_activation_height` | Gauge | P0.0: the next activation above the applied height in this node's schedule (0 = none) |
| `torus_feature_blocks_until_activation` | Gauge | P0.0: blocks from the applied height to that activation (-1 = none) |
| `torus_upgrade_required` | Gauge | P0.0: 1 once a header carried a feature this binary does not know (`UPGRADE REQUIRED`); stays 1 until the restart. Alert `UpgradeRequired` |
| `torus_upgrade_required_feature` / `torus_upgrade_required_height` | Gauge | P0.0: the lowest height met with an unknown feature and its lowest unknown feature id (-1 / 0 = none) |
| `torus_upgrade_required_refusals_total{site}` | Counter | P0.0: `UPGRADE REQUIRED` refusals by `site`: `vote`, `insert` (also block sync), `propose`, `execute` (the process then exits 70) |
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
| `monitoring/alerts/consensus.yml` | torus-telemetry | Block stalls, consensus issues, `UpgradeRequired`, `FeatureActivationApproaching` |
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
