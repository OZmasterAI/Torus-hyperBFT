# Implementation Plan: Sprint 5 Task 1 — Ingress Verify (A→C→B)

## Design Decision
From docs/plans/sprint5-binary-ingress.md: full sequence
**A (instrument) → C (cheap-first admission) → B (binary ingress)**.
D (rayon) deferred pending A's data. Hash identity (keccak256 of canonical
serde_json bytes) unchanged throughout.

## Verified anchors (read, not assumed)
- Metrics: `torus_telemetry::Metrics` — field `rpc_submit_verify_seconds:
  Histogram` (torus-telemetry/src/lib.rs:83), buckets at :298, registration
  :300-302, struct init :411, `encode()` :427.
- Hot path: `verify_one_action` torus-rpc/src/torus.rs:214-233; batch
  endpoint :703-780 (serial `.map()` in one `spawn_blocking` :736-741);
  metrics observed :723-726 (permit), :745-748 (verify wall), :775-777 (admit).
- Admission: `Mempool::add_native_action_presigned` (torus-mempool/src/lib.rs:271)
  → `NativePool::insert` (native_pool.rs:64): dedup → per-sender cap →
  pool cap w/ cancel eviction. `native_pool_size()` lib.rs:435.
  `bincode::serialized_size` already used at native_pool.rs:74.
- Test harness (torus-rpc/src/lib.rs `mod tests`): `let (_dir, state,
  mempool, executor) = setup();` + `RpcServer::new(state, mempool, executor,
  TORUS_CHAIN_ID, 100, BlockNotifier::new())` + `server.set_metrics(...)` +
  jsonrpsee HttpClient (:685-699). Signing recipe :705-715:
  `sign_native_action(action, nonce_ms, &SigningKey::from_slice(hardhat0))`,
  payload `format!("0x{}", hex::encode(serde_json::to_vec(&signed)?))`.
  Existing test `submit_records_phase_histograms` (:684-739) asserts
  `{name}_count 1` in `metrics.encode()`.
- Bench client: tools/bench-throughput/src/main.rs — encode at :157
  (`serde_json::to_vec(&signed)`), batch RPC at :337-349
  (`torus_submitNativeActions`), arg defs near :75.
- verify_one_action is in-crate; the lib.rs tests mod reaches it without
  visibility changes.

## Success Criteria
- New `torus_rpc_submit_verify_cpu_seconds` histogram separates compute from
  the existing wall metric (queue ≈ wall − cpu).
- Micro-bench prints per-phase µs for bs 1/100/500 PlaceOrderBatch.
- `torus_rpc_submit_admit_rejects_total{reason}` shows which limit fires.
- Pool-full non-cancel batches reject before ecrecover (decode-only path);
  cancels still fully verified (eviction preserved).
- `torus_submitNativeActionsBin` (hex of bincode) yields byte-identical
  hashes vs JSON path; bench `--format bin` works end-to-end.
- Entire existing suites stay green: torus-rpc, torus-mempool, torus-types.

## Tasks

### Task 1 (A): verify CPU histogram
- **Test first**: in `submit_records_phase_histograms`
  (torus-rpc/src/lib.rs:728-732) add `"torus_rpc_submit_verify_cpu_seconds"`
  to the asserted name list → fails (metric doesn't exist).
- **Implementation**:
  1. torus-telemetry/src/lib.rs: add field `pub
     rpc_submit_verify_cpu_seconds: Histogram` (:83 block), construct with
     `Histogram::new(exponential_buckets(0.001, 2.0, 14))` + register
     `"torus_rpc_submit_verify_cpu_seconds"` (:298-302 pattern), init (:411).
  2. torus.rs:731-744: closure returns `(results, cpu_elapsed)`:
     ```rust
     let (verified, cpu) = tokio::task::spawn_blocking(move || {
         let t0 = std::time::Instant::now();
         let out = signed_actions.into_iter()
             .map(|sa| verify_one_action(&sa, chain_id, &state_db, current_time_ms))
             .collect::<Vec<_>>();
         (out, t0.elapsed())
     }).await.map_err(...)?;
     ```
     observe `cpu` into the new histogram next to the wall observe (:745-748).
     Same change in single-action `submit_native_action` (:652) if it
     observes the verify histogram (check at implement).
- **Verify**: `cargo test -p torus-rpc submit_records_phase_histograms`
- **Depends on**: —

### Task 2 (A): verify_one_action micro-bench breakdown
- **Test first**: new `#[test] fn verify_breakdown_by_batch_size()` in the
  torus-rpc tests mod. For n in [1, 100, 500]: build
  `NativeAction::PlaceOrderBatch(vec![place_order_params(); n])`, sign via
  the :705-715 recipe, hex-encode, then time separately: `parse_bytes`,
  `serde_json::from_slice::<SignedNativeAction>`,
  `signed.validate_with_sessions(now, TORUS_CHAIN_ID, |_| None)`,
  `serde_json::to_vec`, `keccak256`; print µs each (run with --nocapture);
  assert `verify_one_action(...)` over the same payload returns Ok using the
  `setup()` state_db. (PlaceOrderParams literal: copy a valid one from
  existing mempool/eip712 tests at implement time — must be a real order.)
- **Implementation**: test-only.
- **Verify**: `cargo test -p torus-rpc verify_breakdown_by_batch_size -- --nocapture`
- **Depends on**: —

### Task 3 (C): admit drop-reason counters
- **Test first**: extend `submit_records_phase_histograms`-style test: submit
  the SAME valid action twice in one batch → second must error; then assert
  `metrics.encode()` contains
  `torus_rpc_submit_admit_rejects_total{reason="duplicate"} 1` → fails.
- **Implementation**:
  1. torus-telemetry: `pub rpc_submit_admit_rejects: IntCounterVec` labeled
     `["reason"]`, registered as `torus_rpc_submit_admit_rejects_total`.
  2. torus.rs admit loop (:753-773): on `Err(e)` from
     `add_native_action_presigned`, increment with reason mapped from the
     concrete `MempoolError` BEFORE `format!`: DuplicateNativeAction →
     "duplicate", NativeSenderQueueFull → "sender_queue_full",
     NativePoolFull → "pool_full", RateLimited → "rate_limited", _ →
     "other"; verify-failure arm (`Err(msg)`) → "verify_failed".
- **Verify**: `cargo test -p torus-rpc`
- **Depends on**: Task 1 (telemetry edit proximity, keep commits separate).

### Task 4 (C): pool-saturation pre-verify gate
- **Test first** (two):
  1. torus-mempool unit: fill native pool to its cap via
     `add_native_action_presigned`, assert new `native_pool_is_full()` true;
     one below cap → false.
  2. torus-rpc behavioral: `setup()` with pool cap reachable (fill it), then
     batch-submit a non-cancel action → expect
     `error: Some("mempool: pool full (pre-verify)")` and
     `torus_rpc_submit_admit_rejects_total{reason="pool_full_preverify"} 1`.
- **Implementation**:
  1. torus-mempool/src/lib.rs: `pub fn native_pool_is_full(&self) -> bool`
     (native pool size >= max_size; expose max from NativePool — add
     `pub fn is_full(&self) -> bool` on NativePool using existing fields).
     Export `is_cancel` predicate from native_pool.rs (it exists at :70 use
     site) as `pub(crate)`→`pub` via lib.rs re-export.
  2. torus.rs batch path after permit (:726), before spawn_blocking: if
     `self.mempool.native_pool_is_full()`, run a decode-only spawn_blocking:
     hex + `serde_json::from_slice` + `torus_mempool::is_cancel(&a.action)`.
     Non-cancel → immediate reject ("mempool: pool full (pre-verify)",
     reason counter "pool_full_preverify"). Cancels → forward into the
     normal full-verify path (eviction works). Mixed batches supported
     (per-item routing).
- **Verify**: `cargo test -p torus-rpc && cargo test -p torus-mempool`
- **Depends on**: Task 3 (reason counters).

### Task 5 (B): bincode hash-identity determinism test (gate for Task 6)
- **Test first**: `#[test] fn bincode_roundtrip_preserves_action_hash()` in
  torus-rpc tests mod (same crate has both deps already): for signed actions
  covering ClaimRewards, PlaceOrder, PlaceOrderBatch(500), CancelOrder,
  CreateSession:
  ```rust
  let json_hash = keccak256(&serde_json::to_vec(&signed).unwrap());
  let wire = bincode::serialize(&signed).unwrap();
  let back: torus_types::SignedNativeAction = bincode::deserialize(&wire).unwrap();
  let bin_hash = keccak256(&serde_json::to_vec(&back).unwrap());
  assert_eq!(json_hash, bin_hash);
  assert!(wire.len() < serde_json::to_vec(&signed).unwrap().len());
  ```
  (bincode 1.x default config — the same defaults `serialized_size` uses at
  native_pool.rs:74.)
- **Implementation**: none if green. Any failing variant → STOP, re-decide
  (fix serde shape compatibly or switch to borsh) before Task 6.
- **Verify**: `cargo test -p torus-rpc bincode_roundtrip_preserves_action_hash`
- **Depends on**: —

### Task 6 (B): torus_submitNativeActionsBin endpoint
- **Test first**: client-server test (existing pattern): same signed action
  → JSON path hash == bin path hash; garbage bincode → per-item error, call
  succeeds; > SUBMIT_BATCH_MAX items → InvalidParams. Assert pool size after.
- **Implementation** (torus.rs):
  1. Trait (:133 area): `#[method(name = "submitNativeActionsBin")] async fn
     submit_native_actions_bin(&self, payloads: Vec<String>) ->
     RpcResult<Vec<RpcSubmitResult>>;`
  2. `fn verify_one_action_bin(...)`: `parse_bytes` →
     `bincode::deserialize::<SignedNativeAction>` → `validate_batch_size` →
     `validate_with_sessions` → `serde_json::to_vec` (canonical) → keccak —
     mirror of :214-233 with only the decode swapped.
  3. Extract :714-780 (permit → spawn_blocking verify → admit/forward →
     histograms) into `async fn run_submit_pipeline<F>(&self, items:
     Vec<String>, verify: F)` where F = fn(&str, u64, &StateDb, u64) ->
     Result<...>; both endpoints call it (one body, two decoders). The
     pre-verify gate from Task 4 lives inside the pipeline → bin path gets
     it for free.
- **Verify**: `cargo test -p torus-rpc`
- **Depends on**: Tasks 1, 3, 4 (pipeline shape), 5 (identity proven).

### Task 7 (B): bench-throughput --format bin
- **Test first**: `cargo build --release -p bench-throughput` and smoke
  against the running local node:
  `./target/release/bench-throughput consensus --rpc-urls
  http://127.0.0.1:8545 --senders 1 --duration 2 --batch-size 10
  --submit-batch 5 --format bin` → nonzero Included.
- **Implementation**: tools/bench-throughput/src/main.rs: clap arg
  `--format` (json|bin, default json) near :75; encode branch at :157
  (`bincode::serialize(&signed)` instead of `serde_json::to_vec`); method
  name switch at :345 (`torus_submitNativeActionsBin`). Add `bincode`
  to tools/bench-throughput/Cargo.toml.
- **Verify**: smoke command above.
- **Depends on**: Task 6.

## Verification (end-to-end)
1. `cargo test -p torus-rpc -p torus-mempool -p torus-types` green.
2. Rebuild, restart our seed only (RPC-local change; no wire/consensus
   impact; watch the known post-restart gossip famine — bounce if needed).
3. bs500 stage twice: `--format json` vs `--format bin`; read
   verify cpu-vs-wall split, `admit_rejects_total` reasons, included orders/s.
4. Done when the bottleneck share is NAMED (queue vs parse vs eip712 vs
   ecrecover vs serialize) and saturated batches stop paying crypto.

## Rollback
One commit per task, all additive (new metrics, new method, new flag).
Revert = drop commit. No state/wire/consensus format changes; validators
unaffected; no coordinated upgrade.
