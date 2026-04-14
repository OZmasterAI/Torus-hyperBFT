# Torus Alerting Rules

Prometheus alerting rules for monitoring Torus-hyperBFT nodes.

## Files

| File | Metrics Source | Description |
|------|---------------|-------------|
| `consensus.yml` | torus-telemetry | Block production, consensus stalls, mempool backlog |
| `node.yml` | torus-telemetry | Peer count, database size |
| `infrastructure.yml` | node_exporter | Disk, memory, CPU (requires separate install) |

## Setup

### 1. Configure Prometheus

Add the alerting rules to your Prometheus config:

```yaml
# prometheus.yml
rule_files:
  - "/path/to/monitoring/alerts/consensus.yml"
  - "/path/to/monitoring/alerts/node.yml"
  - "/path/to/monitoring/alerts/infrastructure.yml"

scrape_configs:
  - job_name: "torus"
    static_configs:
      - targets: ["localhost:9090"]  # torus-telemetry endpoint

  # Optional: node_exporter for infrastructure alerts
  - job_name: "node"
    static_configs:
      - targets: ["localhost:9100"]  # node_exporter endpoint
```

### 2. Install AlertManager

Download from: https://prometheus.io/download/#alertmanager

```yaml
# alertmanager.yml
route:
  receiver: "default"
  group_by: ["alertname"]
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h

  routes:
    - match:
        severity: critical
      receiver: "critical"
      repeat_interval: 1h

receivers:
  - name: "default"
    # Choose one or more notification channels below

  - name: "critical"
    # Higher-priority channel for critical alerts
```

### 3. Notification Channels

#### Email

```yaml
receivers:
  - name: "email"
    email_configs:
      - to: "ops@example.com"
        from: "alertmanager@example.com"
        smarthost: "smtp.example.com:587"
        auth_username: "alertmanager@example.com"
        auth_password: "<password>"
```

#### Slack

```yaml
receivers:
  - name: "slack"
    slack_configs:
      - api_url: "https://hooks.slack.com/services/T00/B00/XXXX"
        channel: "#torus-alerts"
        title: '{{ .GroupLabels.alertname }}'
        text: '{{ range .Alerts }}{{ .Annotations.description }}{{ end }}'
```

#### PagerDuty

```yaml
receivers:
  - name: "pagerduty"
    pagerduty_configs:
      - service_key: "<integration-key>"
        severity: '{{ if eq .GroupLabels.severity "critical" }}critical{{ else }}warning{{ end }}'
```

### 4. Connect Prometheus to AlertManager

```yaml
# prometheus.yml
alerting:
  alertmanagers:
    - static_configs:
        - targets: ["localhost:9093"]  # AlertManager endpoint
```

### 5. Silence and Acknowledge

Use the AlertManager web UI (`http://localhost:9093`) or CLI:

```bash
# Silence an alert for 2 hours
amtool silence add alertname=DatabaseLargeWarning --duration=2h --comment="Expanding disk"

# List active silences
amtool silence query

# Expire a silence
amtool silence expire <silence-id>
```

## Alert Summary

### Consensus (torus-telemetry)

| Alert | Condition | Severity |
|-------|-----------|----------|
| MissedBlocks | block_height unchanged 30s | warning |
| ConsensusStall | consensus_view unchanged 60s | critical |
| SlowBlockBuild | block_build p99 > 2s for 5m | warning |
| HighConsensusRounds | round rate spikes > 2x | warning |
| MempoolBacklog | mempool growing 10+ min | warning |

### Node (torus-telemetry)

| Alert | Condition | Severity |
|-------|-----------|----------|
| LowPeerCount | peers < 3 for 5m | warning |
| LowPeerCountCritical | peers < 1 for 2m | critical |
| DatabaseLargeWarning | db > 100 GB | warning |
| DatabaseLargeCritical | db > 200 GB | critical |

### Infrastructure (node_exporter)

| Alert | Condition | Severity |
|-------|-----------|----------|
| DiskSpaceHigh | root fs < 20% free | warning |
| DiskSpaceCritical | root fs < 5% free | critical |
| MemoryHigh | available memory < 20% | warning |
| HighCPU | CPU > 90% for 5m | warning |
