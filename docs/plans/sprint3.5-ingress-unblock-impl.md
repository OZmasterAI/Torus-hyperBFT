# Implementation Plan: Sprint 3.5 — Ingress Unblock

**Design:** docs/plans/sprint3.5-ingress-unblock.md (Option A) · **Status:** PLANNED s338
**Baseline:** torus-mempool --lib 47/47 green (s338, pre-plan verification)

## Design Decision
Option A: instrument the ingress/gossip path end-to-end + cut the redundant
leader-forward body path, ship as ONE deploy, then re-run the T5 dual-box sweep
with metrics snapshots. No wire-format changes (mixed-version safe w/ friend2).
Facts locked by s338 exploration: both nodes ALREADY serve Prometheus on
127.0.0.1:9090 (flag defaults on); `native_da_pull_requests/recovered` counters
already exist; RpcState and swarm already hold `Option<Arc<Metrics>>`.

## Success Criteria
- Channel-full gossip drops are countable (no more silent debug-only loss).
- Submit ack latency is decomposed: permit-wait vs verify vs admit histograms.
- With gossip on, NO full-body leader-forward traffic (gossip + pull-on-miss
  carry bodies); with `--native-gossip=false` behavior unchanged.
- All touched crates green (`--lib`!); release builds on both boxes.
- Re-sweep either clears 50k o/s @ bs500 OR the new metrics conclusively name
  the next wall (verify-p95 → Option C; drops → loop tuning; pulls → Option B).
- bs1000 regression gate: no wedge, ingress ≥ Sprint-2 level.

## Tasks

### Task 1: Gossip-path counters (publish / receive / drop)
- **Test first** (torus-mempool): `gossip_drop_increments_counter` — build
  Mempool with a `native_gossip_tx` of capacity 1 via `set_native_gossip_tx`,
  enable gossip, admit two valid actions; assert
  `metrics.native_gossip_dropped_full` == 1. (Fails: field doesn't exist.)
- **Implementation**:
  - torus-telemetry/src/lib.rs: add `native_gossip_published_actions: Counter`,
    `native_gossip_received_actions: Counter`, `native_gossip_dropped_full:
    Counter` to `Metrics` (struct ~L61 gossip block) + 3 `registry.register`
    calls in `Metrics::new()` (idiom at L84+).
  - torus-mempool/src/lib.rs: `metrics: OnceCell<Arc<Metrics>>` + `set_metrics`
    (mirror the `native_gossip_tx` OnceCell pattern); in
    `gossip_native_action` (L358-375) `TrySendError::Full` arm: `inc()` the
    drop counter (keep debug! line).
  - torus-network/src/swarm.rs: both publish sites (L458, L481):
    `m.native_gossip_published_actions.inc_by(count as u64)` next to the
    existing `gossip_messages_sent.inc()`; inbound batch handler (~L600):
    `m.native_gossip_received_actions.inc_by(count as u64)`.
  - torus-node/src/main.rs: `mempool.set_metrics(metrics.clone())` beside
    `set_native_gossip_tx` (~L425).
- **Verify**: `cargo test -p torus-mempool --lib && cargo test -p torus-network --lib`
- **Depends on**: —

### Task 2: Submit-ack phase histograms
- **Test first** (torus-rpc): `submit_records_phase_histograms` — same harness
  as `submit_native_actions_batch_per_item_results` (lib.rs:624) with
  `set_metrics` wired; after one successful batch call, `Metrics::encode()`
  output contains `rpc_submit_verify_seconds_count 1` and
  `rpc_submit_admit_seconds_count 1`. (Fails: metrics don't exist.)
- **Implementation**:
  - torus-telemetry/src/lib.rs: `rpc_submit_permit_wait_seconds`,
    `rpc_submit_verify_seconds`, `rpc_submit_admit_seconds` — all
    `Histogram::new(exponential_buckets(0.001, 2.0, 14))` (1ms→8s), registered
    in `Metrics::new()`.
  - torus-rpc/src/torus.rs `submit_native_actions` (L700-763): wrap the three
    segments with `std::time::Instant` — (a) around `acquire_submit_permit`
    (L714), (b) around the `spawn_blocking(...).await` (L722-735), (c) around
    the admission/results loop (L737-760); `observe()` each into
    `self.metrics` when present.
- **Verify**: `cargo test -p torus-rpc --lib`
- **Depends on**: Task 1 (same telemetry struct — apply after to avoid edit conflicts)

### Task 3: Gate leader-forward behind gossip-off
- **Test first** (torus-rpc): `no_leader_forward_when_gossip_on` — wire
  `set_leader_forwarding(.., forward_bodies=false)` + fwd channel, submit a
  valid action, assert the fwd rx is EMPTY; sibling test with
  `forward_bodies=true` asserts payload `[20B sender||action_bytes]` arrives
  (today's format, torus.rs:244-247). (Fails: param doesn't exist.)
- **Implementation**:
  - torus-rpc/src/lib.rs: add `forward_bodies: bool` to the leader-forwarding
    state (set in `set_leader_forwarding`, L216 area).
  - torus-rpc/src/torus.rs `forward_to_leader` (L238-251): early-return unless
    `forward_bodies`.
  - torus-node/src/main.rs L558: pass `!cli.native_gossip` — gossip carries
    bodies (flush bounded by `NATIVE_BATCH_INTERVAL_MS` tick, swarm.rs:361/378,
    or the 1024 size cap under load); pull-on-miss remains the correctness net.
    `--native-gossip=false` restores full-body forwarding exactly as today.
- **Verify**: `cargo test -p torus-rpc --lib`
- **Depends on**: —

### Task 4: Failure counters at the storm sites
- **Test first**: compile-gate (sites live deep in the swarm event loop; the
  live proof is Task 5's before/after snapshots): write the swarm `inc()`
  lines first — `cargo build -p torus-network` fails until the telemetry
  fields exist — then add the fields.
- **Implementation**:
  - torus-telemetry/src/lib.rs: `native_da_pull_failures: Counter`,
    `direct_send_failures_untracked: Counter`, registered.
  - torus-network/src/swarm.rs: `inc()` at the OUTBOUND FAILURE warn (L924)
    and the untracked-payload warn (L757), guarded by `shared.metrics`.
- **Verify**: `cargo test -p torus-network --lib`
- **Depends on**: Task 2 (telemetry struct ordering)

### Task 5: Deploy + instrumented re-sweep (PROOF)
- **Test first**: full gates locally — `cargo fmt --check`, `cargo clippy -p
  torus-rpc -p torus-mempool -p torus-network -p torus-telemetry`, per-crate
  `--lib` tests (NEVER `-p hotstuff_rs` without `--lib` — block_sync_test hangs).
- **Implementation**: release build; deploy our box (relaunch cmd unchanged) +
  smallserver (scp binary, systemctl restart torus-hbft-validator); wait for
  re-convergence (height advancing, view sync). Snapshot
  `curl 127.0.0.1:9090/metrics` on BOTH boxes; dual-box bs500 (ours senders
  0-9, smallserver 10-19, submit-batch 10, 30s, simultaneous); snapshot again;
  20s health wait + height-delta check; bs1000 round; final snapshots.
- **Verify**: PASS = bs500 ≥ 50k o/s combined OR metrics name the wall
  (`rpc_submit_verify_seconds` p95 dominating → Option C next;
  `native_gossip_dropped_full` > 0 → publish-loop/channel tuning;
  `native_da_pull_failures` storm persisting → Option B). bs1000: no wedge AND
  ingress ≥ Sprint-2 level. Record verdict + numbers to memory.
- **Depends on**: Tasks 1-4

### Task 6 (parallel, bench-only): Pre-generate signed actions
- **Test first**: existing bench tests stay green (`cargo test -p
  bench-throughput`) + new unit asserting the pregen pool is consumed in order.
- **Implementation**: tools/bench-throughput/src/main.rs — sign/serialize the
  full action set for the window BEFORE the clock starts (senders × duration ×
  rate envelope), submit from the pregen pool; removes client signing CPU from
  the bs1000 measurement (s334: 4-core box managed only ~160 calls/30s).
- **Verify**: `cargo test -p bench-throughput`
- **Depends on**: — (independent; do before Task 5 if time allows)

## Verification (end-to-end)
Task 5 IS the end-to-end proof. Expected metric deltas during bs500:
`native_gossip_published_actions` ≈ admitted actions on each ingest box;
`received` ≈ peer's published; `dropped_full` = 0 (else loop tuning next);
`native_da_pull_requests` LOW (its doc contract); leader-forward direct-send
failures ≈ 0 (path now off with gossip on).

## Rollback
All counters are additive (no behavior). Task 3 is flag-symmetric:
`--native-gossip=false` restores full-body leader-forward with zero code
change. Worst case: revert the single commit; binaries from af17cd6 redeploy.
