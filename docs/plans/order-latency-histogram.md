# Design: end-to-end order latency histogram (s77)

## Problem

No metric measures how long an order takes from submission to a visible fill.
Block time, commit interval and exec-chain time are measured, but execution
trails commit by an inferred 11-16 blocks, and nobody can see what a user
experiences. Every latency lever (commit feed after header, batched propose
writes, body request before vote, async validate, the EVM cadence) needs this
number to be judged.

## Context (from memory + exploration)

- An action's `nonce` is a wall-clock millisecond timestamp, bounded to ±60 s
  by the node (`eip712.rs` `NONCE_WINDOW_MS`). The matched bench streams: it
  stamps `nonce = now_ms` when it signs, just before sending
  (`tools/bench-throughput/src/main.rs` `sign_payload_batch`, `AmmoPlan::Stream`
  for 300 s cells). So on the bench, `now - nonce` is the order's age since
  submission, on the same host clock.
- Stage points in the node:
  - RPC admit: `Mempool::add_native_action*` (the ingress node only).
  - Commit: `on_committed_block` (every node, consensus thread).
  - Executed: after `execute_batch` in `execute_committed_block_with` (exec thread).
  - State durable (balances, positions, orders readable over RPC): end of the
    serial flush, or the flush worker's job end (`exec_pipeline.rs` `worker_loop`).
  - Fills readable over RPC: the background trade writer has written the
    block's `CF_NATIVE_TRADES` / `CF_NATIVE_USER_TRADES` rows; `getUserTrades`
    and `getBlockTrades` read those CFs straight from RocksDB.
- Metrics pipeline: histograms in `torus-telemetry` (`exponential_buckets`),
  `run-cell.sh` `BUCKET_METRICS` samples bucket series into `buckets.csv`,
  `summarize.py` already has `hist_quantile` and `bucket_deltas` for windowed
  percentiles.

## Options

### Option A: node-side nonce-age histograms per stage (recommended)
One histogram per stage, `torus_order_age_seconds{stage=...}` or one metric per
stage, observing `now_ms - nonce` for every native action of the block at that
stage: `admit`, `commit`, `exec`, `durable`, `fills_visible`. The flush job and
the trade-writer batch carry the block's nonces (a `Vec<u64>`, ≤ block cap).
The trade writer gets a per-batch completion hook so the consensus side
observes `fills_visible` after the rows are written. `summarize.py` reports
p50/p90/p99 per stage over the bench window, per node.
- Files: `torus-telemetry/src/lib.rs`, `torus-mempool/src/lib.rs` (admit),
  `torus-consensus/src/app.rs` (commit, exec, serial durable),
  `torus-consensus/src/exec_pipeline.rs` (pipelined durable),
  `torus-state/src/bg_writer.rs` (completion hook), `tools/matched-bench/run-cell.sh`,
  `tools/matched-bench/summarize.py` (+ its tests).
- Pros: no bench change; works on the testnet too; every node reports; cheap
  (a few hundred observations per block per stage); stages decompose the
  latency so a lever's effect is attributable.
- Cons: trusts the client's nonce clock. Exact on the bench (same host);
  on a real network it includes client clock skew (bounded ±60 s by the node,
  typically ms with NTP). Counted per action, not per order (the bench's batch
  size is fixed, so the shapes are the same).
- Effort: Medium. Risk: Low (observability only, no consensus or state change).

### Option B: bench-side end-to-end
The bench records each action's send time and polls RPC (`getUserTrades` /
order status) until the fill or resting order appears.
- Pros: true client view, including RPC read path.
- Cons: polling adds RPC and CPU load on an already saturated box, perturbing
  what it measures; bench-only; no stage breakdown.
- Effort: Medium. Risk: Medium (measurement perturbation).

### Option C: sampled per-action trace log
The node logs 1 in N actions with a timestamp at each stage; a collector joins
them by action hash across stages and nodes.
- Pros: exact per-action joins, cross-node view (for example, admit on val0 and
  exec on val2).
- Cons: log volume and parsing work; more moving parts than needed to answer
  "is latency going down".
- Effort: Large. Risk: Low.

## Recommendation

Option A. It answers the question with existing plumbing, costs nothing
measurable, decomposes latency by stage, and also runs on the testnet. Option B
can come later as a spot check once nodes run on separate hosts.

## Not Building (YAGNI)

- An env gate: observation is a few hundred histogram updates per block.
- A new timestamp field in actions or blocks: the nonce already is one.
- Per-order weighting: the bench batch size is fixed.
- An inclusion (proposal) stage on the leader: commit covers it, since commit
  follows proposal by about two views.

## Open Questions

- Buckets: `exponential_buckets(0.005, 2.0, 15)` covers 5 ms to ~82 s. Enough
  resolution near the expected 1-15 s range?
- Future nonces (client clock ahead): clamp the age to 0 and count them in a
  separate counter so skew is visible, not hidden.
