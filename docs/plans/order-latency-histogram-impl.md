# Implementation Plan: end-to-end order latency histogram

## Design Decision

Option A of `order-latency-histogram.md`: node-side histograms of
`now_ms - nonce` for every native action at five stages. The bench stamps
`nonce = now_ms` at signing (streaming mode), so the age is time since submit.

| stage | metric | where | what it means |
|---|---|---|---|
| admit | `torus_order_age_admit_seconds` | `Mempool::add_native_action`, `add_native_action_presigned` (after a successful submit) | RPC node accepted it |
| commit | `torus_order_age_commit_seconds` | `TorusApp::dispatch_to_exec`, once per height | block committed and body in hand |
| exec | `torus_order_age_exec_seconds` | `execute_committed_block_with`, after `execute_batch` | executed on the exec thread |
| durable | `torus_order_age_durable_seconds` | serial flush end; flush worker job success | balances/positions/orders readable over RPC |
| fills_visible | `torus_order_age_fills_visible_seconds` | trade-writer batch written (or the sync fallback) | fills readable via `getUserTrades` |

`fills_visible` is observed for all of a block's actions when that block's
trade rows land; blocks without fills contribute nothing to it. A nonce ahead
of the node clock counts as age 0 and increments
`torus_order_age_future_nonce_total`.

## Success Criteria

1. Each stage histogram is observed once per native action of each native
   block, on both the serial path and the pipelined (flush worker) path.
2. `fills_visible` is observed only after the rows are readable from RocksDB.
3. `summarize.py` reports p50/p90/p99 per stage per node over the bench window,
   and cells from older binaries (no series) report `None`, not an error.
4. No state, consensus or format change: existing workspace tests pass.

## Tasks

### Task 1: telemetry — stage histograms and `observe_order_ages`
- Test first (`crates/torus-telemetry/src/lib.rs` tests): `observe_order_ages_at(OrderStage::Admit, 3_000, [1_000, 2_500, 5_000])`
  gives `torus_order_age_admit_seconds_count 3`, `_sum 2.5`, and
  `torus_order_age_future_nonce_total 1`; the other four stage series exist with count 0.
- Implementation: `pub enum OrderStage { Admit, Commit, Exec, Durable, FillsVisible }`;
  five `Histogram` fields with `exponential_buckets(0.005, 2.0, 15)` (5 ms to ~82 s),
  one `Counter`; `Metrics::observe_order_ages_at(stage, now_ms, nonces)` and
  `observe_order_ages(stage, nonces)` (reads the wall clock).
- Verify: `cargo test --release -p torus-telemetry order_age`

### Task 2: trade writer completion hook
- Test first (`crates/torus-state/src/bg_writer.rs` tests): `send_packed_then(batch, cb)`;
  the callback fires once, and when it fires the row is readable from the DB.
- Implementation: `CfBatch::Packed(PackedCfBatch, Option<OnWritten>)`, callback
  run after a successful `write`; `send_packed` = `send_packed_then` without one.
  On handback (writer gone) the callback is returned to the caller unrun.
- Verify: `cargo test --release -p torus-state bg_writer`

### Task 3: admit stage in the mempool
- Test first (`crates/torus-mempool/src/lib.rs` tests): with metrics set,
  one admitted `add_native_action_presigned` gives admit count 1; a rejected
  duplicate does not add a second observation.
- Implementation: observe after `submit_native_action` succeeds in both entry points.
- Verify: `cargo test --release -p torus-mempool order_age`

### Task 4: exec, serial durable and fills_visible in the exec context
- Test first (`crates/torus-consensus/src/app.rs` tests): serial ctx with metrics
  and a trade writer; execute the pipeline fixture blocks; after dropping the
  ctx (writer drained), exec count == durable count == total native actions, and
  fills_visible count > 0 and equals the actions of the blocks that filled.
- Implementation: collect the block's nonces once; observe exec after
  `execute_batch`; durable at the end of the serial flush; `send_packed_then`
  with a closure observing fills_visible; observe directly after the sync fallback.
- Verify: `cargo test --release -p torus-consensus order_age`

### Task 5: pipelined durable (flush worker)
- Test first: same fixture with the flush worker attached (metrics set before
  attach); after drop, durable count == exec count == total native actions.
- Implementation: `Job::Flush { nonces: Vec<u64>, .. }`; the worker observes
  durable on both success arms (`Ok(Ok)` and `TrieStale`).
- Verify: `cargo test --release -p torus-consensus order_age exec_pipeline`

### Task 6: commit stage in `dispatch_to_exec`
- Implementation: observe inside the once-per-height block with
  `torus_block.native_actions` nonces.
- Test: covered by an existing dispatch test if one reaches `dispatch_to_exec`
  with metrics; otherwise verified by the bench cell (series count > 0).
- Verify: `cargo test --release -p torus-consensus` and the cell.

### Task 7: harness
- Test first (`tools/matched-bench/test_summarize.py`): a synthetic buckets.csv
  with the five series produces per-stage p50/p90/p99 in the summary, and a
  cell without them yields `None`.
- Implementation: add the five `_bucket` series to `BUCKET_METRICS` in
  `run-cell.sh`; `summarize.py` computes the percentiles with `bucket_deltas`
  + `hist_quantile` over the bench window and prints an `ORDER_AGE` line per node.
- Verify: `python3 -m pytest tools/matched-bench/test_summarize.py -q`

## Verification (end-to-end)

- Workspace tests pass (`cargo test --release --workspace`).
- One matched cell on the branch: all five series populated on every node,
  admit on the three RPC nodes; p50 ordering admit < commit < exec ≤ durable,
  fills_visible close to durable; throughput within cell noise of main.

## Rollback

Observability only. Revert the merge; no data or format migration.
