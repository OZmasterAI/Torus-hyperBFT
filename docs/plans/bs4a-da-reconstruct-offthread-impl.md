# Implementation Plan: BS-4a — DA reconstruct off the consensus thread (+BS-4b)

## Design Decision

Option B from docs/plans/bs4a-da-reconstruct-offthread.md: fail-fast
`MissingData` after one ≤20ms local wait slice + a dedicated
`DaRecoveryWorker` thread (O3 `BackgroundCfWriter` pattern) that runs the pull
loop off-thread with a ~1s event-driven budget. BS-4b (event-driven mid-budget
re-fetch) piggybacks in the worker loop. Per-node liveness policy only — no
consensus-validity/wire change, no lockstep, NOT part of the relaunch binary.

Branch: `perf/bs4a-da-recovery` off `sprint/blockspeed-orders-s395` @ 6e03294
(keeps the fleet-pinned relaunch commit untouched).

## Success Criteria

- `reconstruct_native_actions_hot` worst case on the consensus thread drops
  from ~260ms to ≤~40ms (one 20ms wake-on-arrival slice + bookkeeping), test-
  enforced (const budget test).
- A push-missed body is still recovered — now off-thread within ~1s
  (worker), test-enforced with the existing `LateFetcher` harness.
- Pre-warm behavior unchanged: a body sitting in the fetcher inbound is
  absorbed by the local slice with zero `fetch()` calls
  (`prewarmed_bodies_absorbed_without_redundant_hot_fetch` still green).
- Sync path (`pull_missing_bodies`, ~1–8s budgets) untouched; its tests and
  `crates/torus-integration-tests/tests/native_da_pull_fallback.rs` still pass.
- `cargo clippy --workspace -D warnings` stays green; nextest suite shows no
  new failures vs the S420 baseline (1066/1069, known non-passes only).

## Tasks

### Task 1: RED — hot path hands off and returns Err fast; recovery lands in background

- **Test first** (app.rs test module, alongside the existing #4 Task 1 tests;
  reuses `LateFetcher`):

  ```rust
  /// BS-4a (RED first): a true push miss must NOT block the consensus thread on the
  /// in-line pull budget. reconstruct returns Err (MissingData) within the shrunken
  /// local slice, and the RECOVERY WORKER pulls + absorbs the body in the background
  /// so the re-proposed view finds it locally. MUST fail before BS-4a lands (today
  /// the hot path blocks ~260ms and returns Ok via the in-line pull).
  #[test]
  fn hot_path_hands_off_and_recovers_in_background() {
      let (config, state_db) = make_test_config_and_db();
      let mempool = Arc::new(torus_mempool::Mempool::new(
          state_db.clone(),
          torus_mempool::MempoolConfig::default(),
      ));
      let mut app = TorusApp::new(state_db.clone(), &config, None, Some(mempool.clone()), None);

      let action = sign_claim_rewards(13);
      let hash = torus_types::compute_action_hash(&action);
      let body = bincode::serialize(&action).expect("serialize body");
      let fetcher = Arc::new(LateFetcher {
          body,
          deliver_on_drain: 3,
          drains: std::sync::atomic::AtomicUsize::new(0),
      });
      app.set_native_da_fetcher(fetcher);

      let start = std::time::Instant::now();
      let result = app.reconstruct_native_actions_hot(&[hash]);
      let elapsed = start.elapsed();

      assert_eq!(result.err(), Some(1), "a push miss fails THIS view immediately");
      assert!(
          elapsed < std::time::Duration::from_millis(80),
          "consensus thread must not run the pull budget in-line: took {elapsed:?}",
      );
      // The worker recovers the body off-thread well before the next view.
      let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
      while mempool.get_native_da(&hash).is_none() {
          assert!(std::time::Instant::now() < deadline, "worker never recovered the body");
          std::thread::sleep(std::time::Duration::from_millis(10));
      }
  }
  ```

- **Implementation**: none (RED).
- **Verify**: `cargo test -p torus-consensus hot_path_hands_off_and_recovers_in_background`
  → MUST FAIL (today: returns Ok after in-line pull, elapsed ~60ms+ but Ok not Err).
- **Depends on**: —

### Task 2: Extract a free recovery fn shared by sync path and worker

- **Test first**: existing sync tests are the harness —
  `cargo test -p torus-consensus pull_recovers_body_delivered_after_old_budget`
  green before AND after (pure refactor).
- **Implementation**: in app.rs, move the body of
  `pull_missing_bodies_bounded` (app.rs:1041-1106) into a free function:

  ```rust
  /// Shared bounded fetch-wait-absorb loop: used by the SYNC path (in-line, ~1-8s)
  /// and the BS-4a recovery worker (off-thread, ~1s). Wake-on-arrival (S391),
  /// deadline-bounded (S395).
  fn recover_bodies_bounded(
      mempool: &Mempool,
      fetcher: &dyn NativeDaFetcher,
      missing: &[torus_types::B256],
      retries: usize,
      delay: std::time::Duration,
      metrics: Option<&torus_telemetry::Metrics>,
  ) -> bool
  ```

  `TorusApp::pull_missing_bodies_bounded` becomes a thin wrapper resolving
  `self.mempool`/`self.da_fetcher`/`self.metrics` and delegating. No behavior
  change; `absorb_fetched_bodies` already takes `(mempool, fetcher)`.
- **Verify**: `cargo test -p torus-consensus pull_ -- --nocapture` (all sync pull
  tests green) and `cargo clippy -p torus-consensus -- -D warnings`.
- **Depends on**: —

### Task 3: DaRecoveryWorker (channel + thread, drop-clean)

- **Test first** (unit test, app.rs test module):

  ```rust
  /// BS-4a: the recovery worker, fed a missing hash, pulls + absorbs the body into
  /// the durable DA store off-thread within its budget.
  #[test]
  fn recovery_worker_recovers_late_body_off_thread() {
      // Mempool + LateFetcher(deliver_on_drain: 3) as in Task 1, no TorusApp:
      let worker = DaRecoveryWorker::spawn(mempool.clone(), fetcher, None);
      worker.submit(vec![hash]);
      // poll mempool.get_native_da(&hash) with a 2s deadline as in Task 1
  }
  ```

- **Implementation** (app.rs, below the `NativeDaFetcher` trait):

  ```rust
  /// BS-4a: owns the off-thread recovery of push-missed native-action bodies.
  /// The consensus thread hands missing hashes here and votes MissingData
  /// immediately; this thread runs the (event-driven, deadline-bounded) pull
  /// loop with the roomier WORKER budget, so the re-proposed view finds the
  /// bodies locally. Sender-drop => recv Err => thread exits (O3 pattern).
  struct DaRecoveryWorker {
      tx: std::sync::mpsc::Sender<Vec<torus_types::B256>>,
      handle: Option<std::thread::JoinHandle<()>>,
  }
  ```

  - `spawn(mempool: Arc<Mempool>, fetcher: Arc<dyn NativeDaFetcher>, metrics: Option<Arc<Metrics>>)`:
    `std::thread::Builder::new().name("torus-da-recovery")` looping on
    `rx.recv()`; per batch: drop hashes already in the store, then
    `recover_bodies_bounded(&mempool, fetcher.as_ref(), &missing, WORKER_PULL_RETRIES, WORKER_PULL_DELAY, metrics)`.
  - Consts: `WORKER_PULL_RETRIES: usize = 50`, `WORKER_PULL_DELAY = 20ms` (~1s,
    sync-parity; doc-comment the open question on 2s).
  - `submit(&self, hashes: Vec<B256>)` = non-blocking `tx.send`.
  - `impl Drop`: drop `tx` (take), `handle.join()` — bounded because recv errs
    immediately and any in-flight batch is deadline-bounded ≤1s.
  - Dedup: batches are per-view and the store-check drops already-recovered
    hashes; duplicate in-flight fetches are chunked + rare — acceptable, noted.
- **Verify**: `cargo test -p torus-consensus recovery_worker_recovers_late_body_off_thread`
- **Depends on**: Task 2.

### Task 4: Wire the worker; shrink the hot path (GREEN for Task 1)

- **Test first**: Task 1's test is the RED driver; also keep
  `prewarmed_bodies_absorbed_without_redundant_hot_fetch` green (absorb must
  stay in the local slice).
- **Implementation** (all app.rs):
  1. Field `da_recovery: Option<DaRecoveryWorker>` on TorusApp (init None at
     :838). In `set_native_da_fetcher` (:852): keep storing the fetcher; if
     `self.mempool` is Some, spawn the worker
     (`DaRecoveryWorker::spawn(mempool.clone(), fetcher.clone(), self.metrics.clone())`).
  2. Consts: `RECONSTRUCT_RETRIES: 5 → 1` (one 20ms wake-on-arrival slice;
     update the doc-comment: the racing-push window measurement is an open
     question — tune on devnet). DELETE `HOT_PULL_RETRIES`/`HOT_PULL_DELAY`
     (in-line hot pull is gone; the worker consts replace them).
  3. `reconstruct_native_actions_hot`: keep phase (1) local retry EXACTLY as
     is (loop structure, absorb, wake-on-arrival — now one slice via the
     const). REPLACE phase (2) (the `pull_missing_bodies_bounded` block,
     :1190-1205) with the handoff:

     ```rust
     // (2) BS-4a: a true push miss no longer burns the consensus thread on an
     // in-line pull. Hand the misses to the recovery worker (off-thread, roomier
     // budget) and fail THIS view — MissingData, re-proposed next view, by which
     // point the worker has the bodies durably local. mem 7efe7062.
     if !missing.is_empty() {
         if let Some(ref worker) = self.da_recovery {
             worker.submit(missing.iter().map(|&i| hashes[i]).collect());
             if let Some(ref m) = self.metrics {
                 m.native_da_recovery_handoffs.inc();
             }
         }
         return Err(missing.len());
     }
     ```

     (metrics counter added in Task 6; guard with `#[cfg]`-free Option as
     elsewhere. If Task 6 is deferred, land without the metrics lines.)
- **Verify**: `cargo test -p torus-consensus hot_path_hands_off_and_recovers_in_background prewarmed_bodies_absorbed_without_redundant_hot_fetch`
- **Depends on**: Tasks 1, 3.

### Task 5: Update the superseded #4 Task-1 tests + budget test

- **Test first**: this task IS tests. Rewrite:
  - `hot_path_pulls_missing_body_within_budget` (:2883) → DELETE (superseded
    by Task 1's test — same scenario, new contract). Keep `LateFetcher`.
  - `hot_path_fails_view_fast_when_body_never_arrives` (:2973): tighten the
    elapsed bound from `timeout_base_ms` to 80ms (fail-fast is now the
    contract, not just under-timeout).
  - `hot_pull_budget_under_view_timeout` (:3006): recompute as
    `RECONSTRUCT_RETRY_DELAY * RECONSTRUCT_RETRIES` and assert `< 50ms`
    (consensus-thread budget), plus
    `WORKER_PULL_DELAY * WORKER_PULL_RETRIES < 2s` (worker never outlives a
    couple of views).
- **Verify**: `cargo test -p torus-consensus -- hot_path hot_pull_budget`
- **Depends on**: Task 4.

### Task 6: Metrics for the relaunch A/B

- **Test first**: extend the telemetry registration test (grep
  `native_da_pull_requests` in torus-telemetry for the pattern/registry test)
  with the new counters, RED first.
- **Implementation**: torus-telemetry (same block as `native_da_pull_requests`
  / `native_da_pull_recovered`): `native_da_recovery_handoffs` (counter),
  `native_da_recovery_timeouts` (counter, worker budget exhausted —
  incremented in the worker when `recover_bodies_bounded` returns false).
  Wire in app.rs per Task 4 snippet + worker loop.
- **Verify**: `cargo test -p torus-telemetry && cargo test -p torus-consensus`
- **Depends on**: Task 4.

### Task 7: BS-4b piggyback — event-driven mid-budget re-fetch

- **Test first** (unit test on `recover_bodies_bounded` with a fetcher stub
  counting `fetch()` calls): a body that never arrives triggers exactly ONE
  re-fetch (initial + one mid-budget), not one per tick; a body arriving
  before the midpoint triggers NO re-fetch.
- **Implementation**: in `recover_bodies_bounded`, after the initial
  `fetcher.fetch(...)`, compute `refetch_at = deadline - (delay * retries as u32) / 2`;
  inside the wait loop, if `now >= refetch_at` and still-missing and not yet
  re-fetched: `fetcher.fetch(still_missing)` once (covers a lost
  request/response; the fan-out itself already rotates across all
  validators — bridge.rs:390). Wake-on-arrival cadence unchanged (already
  event-driven, S391).
- **Verify**: `cargo test -p torus-consensus recover_bodies -- --nocapture`
- **Depends on**: Task 2 (touches only the shared fn; independent of 3-6).

### Task 8: Full verification sweep

- **Verify**:
  1. `cargo fmt --all -- --check`
  2. `cargo clippy --workspace --all-targets -- -D warnings` (warm ~5min)
  3. `cargo nextest run --workspace` — compare against S420 baseline
     1066/1069 (known non-passes: pacemaker load-flake + 3 capped hangers;
     mem 843092a5). Integration test
     `crates/torus-integration-tests/tests/native_da_pull_fallback.rs` must
     be green (it exercises `set_native_da_fetcher` + sync pull).
- **Depends on**: Tasks 1-7.

## Verification (end-to-end)

Devnet A/B when convenient (NOT gating the merge to the BS-4a branch, gating
any fleet rollout): O2 bs400 single-first-leg protocol (512KB pinned, 4-market
genesis — mem b24fb95c/ea3901d4), old vs new binary; watch p99
`torus_view_duration_seconds`, `missing_action_rejections` rate,
`native_da_recovery_handoffs`/`timeouts`, orders/s. Success = p99 view time
down under body-miss injection, no orders/s regression on clean runs. The
collapse-repro harness (devnet/o2-collapse-repro-s419/) is the stress rig —
if the ~2/7 stochastic spiral reproduces, compare survival old vs new.

## Rollback

Single branch (`perf/bs4a-da-recovery`); each task is one commit — revert the
branch or cherry-pick out Task 4 (the only behavior change on the hot path;
Tasks 2/3/6/7 are inert without it). No persistence/wire/consensus format
changes anywhere, so rollback is binary-swap only, per-node.
