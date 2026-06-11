# Implementation Plan: Ingress Verify Fix — Option A

## Design Decision
From docs/plans/ingress-verify-fix.md: Option A — rayon par_iter inside the
existing verify spawn_blocking + admit sub-timers. No semantic change to
per-item results; no metric renames.

## Verified anchors
- Sequential closure: torus-rpc/src/torus.rs:372-391
  (`to_verify.into_iter().map(verify_one_action_with)`); `decode` is a fn
  pointer (Copy+Send), `state_db: StateDb` already shared across rpc/exec
  threads (Sync), `current_time_ms` computed once per batch.
- Admit loop: torus.rs:401-462 — `add_native_action_presigned` +
  `forward_to_leader` per action; one `rpc_submit_admit_seconds` observe.
- rayon: torus-types declares `rayon = "1"` directly (NOT workspace);
  torus-rpc Cargo.toml has no rayon yet.
- Tests: `submit_native_actions_batch_per_item_results` (lib.rs:635) —
  3-item order check; `submit_records_phase_histograms` (lib.rs:683) —
  metrics harness with set_metrics + HTTP client.
- verify_cpu semantics: serial ⇒ "CPU ≈ in-closure wall". With rayon the
  in-closure span is parallel wall — keep the metric NAME, fix HELP +
  rustdoc (torus-telemetry/src/lib.rs:84-86, registration :312-317).

## Success Criteria
- Strengthened order-pinning test green before AND after par_iter (12
  interleaved valid/invalid items, exact per-index results).
- Full torus-rpc suite green; telemetry suite green.
- `torus_rpc_submit_admit_insert_seconds` + `_admit_forward_seconds`
  registered and observing once per batch.
- Live probe rerun: ack RTT 1.9s → ≤0.8s, submit rate ≥ 2.5x (39 → 100+
  actions/s), exec_queue_depth re-checked under the new ingress rate.

## Tasks

### Task 1: Order-pinning test (GREEN-first guard for parallel verify)
- **Test first** (torus-rpc/src/lib.rs tests, beside :635): new
  `submit_batch_order_preserved_with_interleaved_failures` — 12 items:
  indexes 2,5,8,11 invalid (`"0xzz"`, bad hex), others valid ClaimRewards
  from the two funded keys with distinct nonces. Assert results.len()==12
  and exact hash-some/error-some per index, pool size == 8. This PINS
  order so the par_iter swap cannot silently reorder. (GREEN-first by
  design — it must stay green in Task 2; the RED concept doesn't apply to
  a behavior-preserving change.)
- **Verify**: `cargo test -p torus-rpc submit_batch_order_preserved`
- **Depends on**: —

### Task 2: par_iter the verify closure
- **Implementation**:
  - torus-rpc/Cargo.toml: `rayon = "1"` (match torus-types style).
  - torus.rs:372-391: `use rayon::prelude::*;` inside the closure;
    `to_verify.into_par_iter().map(...)` — rest unchanged (collect
    preserves index order).
  - torus-telemetry/src/lib.rs:84-86 + :315: HELP/rustdoc for
    `rpc_submit_verify_cpu_seconds` → "in-closure span (parallel wall
    since Option A); verify_seconds − this ≈ blocking-pool queue".
- **Verify**: `cargo test -p torus-rpc` (Task 1 test + :635 + full suite)
- **Depends on**: Task 1.

### Task 3: Admit sub-timers
- **Test first** (torus-telemetry tests): extend
  `exec_phase_metrics_register`-style assertion with a new test
  `admit_subphase_metrics_register` asserting
  `torus_rpc_submit_admit_insert_seconds` and
  `torus_rpc_submit_admit_forward_seconds` in encode() → RED.
  Then extend `submit_records_phase_histograms` (torus-rpc lib.rs:683)
  to assert both names appear with `_count 1` after one batch → RED.
- **Implementation**:
  - torus-telemetry: two `Histogram::new(exponential_buckets(0.001, 2.0,
    14))` + registrations + struct fields/init.
  - torus.rs admit loop: accumulate `insert_dur` around
    `add_native_action_presigned`, `forward_dur` around
    `forward_to_leader`; after the loop observe both once per batch
    (inside the existing `if let Some(ref m)` block at :459-462).
- **Verify**: `cargo test -p torus-telemetry -p torus-rpc`
- **Depends on**: —

### Task 4: Live probe rerun + delta verdict
- Rebuild (nice -19, -j2), exact-PID bounce (binary backup first), 30s
  baseline, identical bs500 solo (10 senders, sb10, 30s) with 3s
  queue-depth sampling, post snapshot, `exec_phase_table.py` + submit-
  phase deltas. Compare against s351 baseline (39/s, RTT 1.9s). Append
  delta verdict to docs/plans/ingress-verify-fix.md; save to memory.
- **Verify**: probe table; chain cadence healthy after.
- **Depends on**: Tasks 1-3.

## Verification (end-to-end)
`cargo test -p torus-rpc -p torus-telemetry` green; live probe shows
RTT ≤ 0.8s and ≥ 2.5x submit rate; admit sub-timers name the next
bottleneck; chain healthy.

## Rollback
All additive/behavior-preserving. Task 2 is one-line revert
(into_par_iter → into_iter); binary backup at torus-node.pre-<tag>
allows instant node rollback.
