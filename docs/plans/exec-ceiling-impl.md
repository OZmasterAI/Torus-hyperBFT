# Implementation Plan: Exec-Ceiling Option A — Instrument the Execution Thread

## Design Decision
From docs/plans/exec-ceiling.md: Option A — phase timers inside
`execute_committed_block`, exec-queue-depth gauge, wire dead counters.
Zero behavior change; output is a verdict table naming the dominant phase.

## Verified anchors
- `ExecutionContext` has `metrics: Option<Arc<torus_telemetry::Metrics>>`
  (app.rs:101-111); `execute_committed_block` at app.rs:176; phases:
  rayon batch verify :285-293, replay-guard loop :315-349 (RocksDB point
  read per action), engine `execute_batch`×2 + governance + fees + epoch
  :368-373, `save_order_books` :374, nonce writes + atomic flush + trie
  :376-389+.
- Exec channel: `std::sync::mpsc::sync_channel(64)` app.rs:652; consumer
  `execution_loop` app.rs:432; producer send in `on_committed_block`
  (app.rs:1468 region). Both sides can reach a metrics handle
  (TorusApp.metrics :472, ExecutionContext.metrics :110).
- Test harness (app.rs tests mod): `make_test_config_and_db()` +
  `make_exec_ctx(&config, &state_db)` (:1992-2010, builds ExecutionContext
  with `metrics: None` — same-module tests can overwrite the field) +
  block-construction pattern in
  `duplicate_committed_native_action_executes_once` (:2042+), which drives
  `execute_committed_block` synchronously.
- Telemetry (torus-telemetry/src/lib.rs): plain `Histogram` style
  (:298-310), `Gauge` in use (block_height), Counter `native_actions_processed`
  EXISTS but is never incremented (renders `_total`). prometheus_client;
  histograms render with `_count 0` when registered → registration is
  testable without observations.
- Proven runner: `cargo test -p torus-consensus --lib` (memory 2328645c).

## Success Criteria
- `metrics.encode()` exposes: `torus_exec_verify_seconds`,
  `torus_exec_replay_guard_seconds`, `torus_exec_engine_seconds`,
  `torus_exec_save_books_seconds`, `torus_exec_flush_seconds`,
  `torus_exec_block_seconds`, `torus_exec_queue_depth`.
- Executing one native block in the unit harness observes each phase
  histogram exactly once and increments `native_actions_processed`.
- `block_build_seconds` observes on the produce path (was dead).
- Live bs500 probe yields a phase decomposition table; dominant phase named.
- All existing torus-consensus / torus-telemetry tests stay green.

## Tasks

### Task 1: Register exec phase histograms + queue gauge in telemetry
- **Test first** (torus-telemetry/src/lib.rs tests mod — create if absent):
  ```rust
  #[test]
  fn exec_phase_metrics_register() {
      let m = Metrics::new();
      let text = m.encode();
      for name in [
          "torus_exec_verify_seconds", "torus_exec_replay_guard_seconds",
          "torus_exec_engine_seconds", "torus_exec_save_books_seconds",
          "torus_exec_flush_seconds", "torus_exec_block_seconds",
          "torus_exec_queue_depth",
      ] {
          assert!(text.contains(name), "{name} not registered:\n{text}");
      }
  }
  ```
  → fails (none exist).
- **Implementation**: six `Histogram::new(exponential_buckets(0.001, 2.0, 14))`
  fields + registrations (mirror :298-310) + `pub exec_queue_depth: Gauge`
  (mirror block_height). Struct init entries.
- **Verify**: `cargo test -p torus-telemetry`
- **Depends on**: —

### Task 2: Phase timers + actions counter in execute_committed_block
- **Test first** (app.rs tests mod, mirroring
  `duplicate_committed_native_action_executes_once`):
  ```rust
  #[test]
  fn exec_phase_histograms_observe_per_block() {
      let (config, state_db) = make_test_config_and_db();
      let mut exec_ctx = make_exec_ctx(&config, &state_db);
      let metrics = Arc::new(torus_telemetry::Metrics::new());
      exec_ctx.metrics = Some(metrics.clone());
      // one signed native action, one block — copy the construction from
      // duplicate_committed_native_action_executes_once (:2042+)
      exec_ctx.execute_committed_block(&block, vec![]);
      let text = metrics.encode();
      for name in ["torus_exec_verify_seconds", "torus_exec_replay_guard_seconds",
                   "torus_exec_engine_seconds", "torus_exec_save_books_seconds",
                   "torus_exec_flush_seconds", "torus_exec_block_seconds"] {
          assert!(text.contains(&format!("{name}_count 1")), "{name}:\n{text}");
      }
      assert!(text.contains("torus_native_actions_processed_total 1"));
  }
  ```
  → fails (timers absent).
- **Implementation** in `execute_committed_block` (app.rs:176+): one
  `Instant::now()` per phase boundary; observe into the matching histogram
  via `if let Some(ref m) = self.metrics`. Boundaries: total (after the
  applied-height skip check, so skipped replays don't pollute), verify
  (:285-293), replay-guard loop (:315-349), engine block (:368-373),
  save_order_books (:374), flush/trie (:376 to end of native section).
  Increment `native_actions_processed` by `sender_actions.len()`.
  EVM-only and empty blocks: observe total only (phases at 0 observations
  keeps per-phase counts == native-block count, simplifying the probe math).
- **Verify**: `cargo test -p torus-consensus --lib exec_phase_histograms_observe_per_block`
  then full `cargo test -p torus-consensus --lib`
- **Depends on**: Task 1.

### Task 3: Exec queue depth gauge
- **Test first**: extend Task 2's test — after the synchronous call the
  gauge must read 0 (`torus_exec_queue_depth 0` in encode) → trivially
  green for the direct-call path, so the REAL check is compile-level
  wiring at the channel: add a unit assertion that `Gauge::inc/dec` calls
  exist by testing TorusApp's send path is impractical in-unit — accept
  registration test (T1) + live probe as the verification for the
  inc/dec sites, and document that.
- **Implementation**: in `on_committed_block` (app.rs:1468 region), before
  `exec_tx.send(...)`: `m.exec_queue_depth.inc()`; in `execution_loop`
  (app.rs:432-438) after `execute_committed_block` returns:
  `m.exec_queue_depth.dec()` — pass the metrics handle the loop already
  reaches via `ctx.metrics`.
- **Verify**: `cargo build -p torus-consensus` + live probe (Task 5 shows
  depth pinning at 64 under load if backpressure theory holds).
- **Depends on**: Task 1.

### Task 4: Wire block_build_seconds on the produce path
- **Test first**: none unit-feasible without a mempool-backed produce
  harness; rely on live probe (`block_build_seconds_count > 0` after
  blocks produced) + regression suite.
- **Implementation**: `Instant::now()` at top of the native-block build in
  `produce_block` (app.rs ~:1219-1250, around `drain_evm`/`drain_native` →
  `TorusBlock {`), observe into the existing dead
  `block_build_seconds` histogram before returning the block.
- **Verify**: `cargo test -p torus-consensus --lib` (regressions) + probe.
- **Depends on**: —

### Task 5: Live probe + verdict (the deliverable)
- **Procedure**: rebuild node (serial, niced), restart seed (exact-PID
  bounce, famine watch armed), 30s idle baseline snapshot, bs500 solo run
  (10 senders, sb10), snapshot metrics; compute per-phase sum/count and
  share of `exec_block_seconds`; read `exec_queue_depth` during load
  (expect pin at 64 if backpressure confirmed).
- **Verify**: a verdict table in chat + memory: dominant phase named with
  numbers; decision row for Option B (books) vs C (pipeline) vs "engine".
- **Depends on**: Tasks 1-4.

## Verification (end-to-end)
`cargo test -p torus-telemetry -p torus-consensus --lib` green; node runs
the instrumented binary on the live testnet; probe table produced; verdict
+ next-option decision saved to memory; chain healthy after (lockstep
cadence restored post-probe).

## Rollback
All additive (new metrics, timer reads). One commit per task; revert =
drop commit. No consensus/wire/state change — single-node deploy, no
validator coordination.
