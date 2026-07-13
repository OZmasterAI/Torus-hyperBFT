# Native-orders throughput grid — raw results (baseline)

Fixed harness `testnet/bench-native-orders-grid.sh` (env `SENDER_OFFSET=20`,
`METRICS=:29090`, funded-range guard + prechecks). Single-version 3-val fleet,
node commit `9a0806a`, genesis `5042dbdb` (era s459). Captured 2026-07-13.

**Config:** MARKETS={1,10} x BATCH={400,1000} x SENDERS={20,60}, sign=session,
dur 20s, rate 30/sender, pre-sign 55. Idle baseline **21.5 blk/s**.

**Files**
- `results.csv` — 8-cell summary
- `m{1,10}_b{400,1000}_s{20,60}_session.txt` — per-cell run logs

`node_actions_s` (Prometheus `torus_native_actions_processed_total`, peak window)
is the reliable throughput number; the tool's RPC-polled `orders_s`/`included`
freezes under the read-RPC choke, so ignore those columns under load.

**Headline:** peak committed ~74k orders/s (m10/b400/s20, node 185 act/s). Block-rate
crashes under every loaded cell (21.5 -> 1.15-6.9/s) with `gossipsub: Send Queue full`
— the ceiling is gossip send-queue saturation, config-independent.

Analysis + reads: PR #2 comment
https://github.com/OZmasterAI/Torus-hyperBFT/pull/2#issuecomment-4962165642
