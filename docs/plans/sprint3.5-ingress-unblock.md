# Design: Sprint 3.5 — Ingress Unblock (de-duplicate body paths + measure)

**Session:** 338 · **Branch:** `fix/native-da-push-hardening` · **Status:** DESIGN

## Problem
Sprint 3 T5 proof (s338) FAILED on throughput: bs500 = 35.2k o/s combined (==
Sprint 2's 34.5k, zero gain from the 6MB cap), bs1000 = 1.06k o/s (ingress
collapse). Hardening PASSED (no wedge; blocks flowed throughout). The limiter
moved from byte-cap to ingress: ~90% of submit acks timed out; blocks were
mempool-starved (~31 native combined vs ~92 that fit in 6MB).

## Context (memory 0140001d + 55d8e647, code exploration s338)
Per admitted native action, bodies can cross the validator link via FOUR paths:
1. **Gossip publish** — `(sender,action)` bincode → bounded(8192) channel →
   batched publish loop (`NATIVE_BATCH_MAX_SIZE=1024`, swarm.rs:447-491).
   On channel-full it drops SILENTLY at debug level (lib.rs:364-372).
2. **Leader-forward** — full body direct-send to the current leader per action
   (torus.rs:743 → fwd channel → main.rs:592 `forward_native_action`).
   Unbounded channel, "untracked payload" sends.
3. **Pull-on-miss** — validator missing a referenced body fan-outs
   `NativeDaNetRequest{hashes}` (swarm.rs:699) + hotstuff retry rotation (Sprint3 T3).
4. **Pre-proposal hash push** — compact, ~3KB, healthy (sent=2 backpressured=0).

Sweep evidence: the us↔smallserver link saturated — ours logged 155 native-da
OUTBOUND (pull) timeouts + 118 INBOUND + 238 max-sub-streams, ALL peer=smallserver;
smallserver mirrors 331 vs us. The pulls mean gossip pre-spread was NOT
delivering bodies under load (drops invisible). Feedback loop: swarm task busy
→ gossip channel fills/drops → more misses → pull fan-out + leader-forward
storm → substreams exhausted → swarm busier.

Ack path: RPC ack awaits semaphore (64 permits, not binding) + ONE
spawn_blocking that per action does hex→**serde_json parse** (~150-250KB at
bs1000) → sig verify → **full serde_json RE-serialize** (torus.rs:229) →
keccak. Cost scales with batch-size; prime ack-latency suspect. All
post-admission sends are non-blocking (try_send / unbounded / local put).

Observability gaps: no --metrics-addr on our node; no per-topic gossip
counters; no ack-latency histogram; gossip drops at debug! level.

Constraint: friend2 still on pre-Sprint-3 binary — any wire-format change
(zstd, topic encoding) must be mixed-version safe (v2 topic or defer).

## Options

### Option A: Instrument-first + de-duplicate paths (two-phase)
**How:** (1) Add the missing telemetry: submit-ack histogram split by phase
(permit / verify / admission), native-gossip counters (published, received,
channel-full drops → WARN + counter), pull-request counters; add
`--metrics-addr` to our node launch. (2) Cut redundant body sends: when
`native_gossip_enabled`, leader-forward sends HASH-ONLY (gossip carries the
body; pull-on-miss remains the correctness net); skip the verify-path
re-serialize by hashing canonical input bytes (determinism test required).
(3) Re-run T5 dual-box sweep; let the histogram name the next wall.
**Files:** torus-telemetry/src/lib.rs, torus-rpc/src/torus.rs + lib.rs,
torus-mempool/src/lib.rs, torus-network/src/swarm.rs, torus-node/src/main.rs,
testnet launch cmd + smallserver unit file.
**Trade-offs:** + attacks both proven pathologies with small diffs; + telemetry
is permanent value; + mixed-version safe (no wire change); − two deploys
(instrument, then fix) if done strictly phased — can ship as one.
**Effort:** Medium · **Risk:** Low

### Option B: Swarm backpressure/QoS (per-peer in-flight caps, priority lanes)
**How:** Bound native-da pulls per peer, budget substreams, prioritize
consensus > gossip > DA traffic, add retry backoff+jitter.
**Files:** torus-network/src/swarm.rs (large), behaviour config.
**Trade-offs:** + addresses the storm mechanism directly; − big surgery in the
hairiest file, hard to validate on 2 small boxes, premature before Option A's
telemetry shows where the queues actually build.
**Effort:** Large · **Risk:** High

### Option C: Ingress wire-format fix (kill JSON on the hot path)
**How:** Accept bincode/borsh submit payloads (new RPC param or endpoint),
lazy-parse orders, eliminate parse+re-serialize double cost.
**Trade-offs:** + directly attacks bs1000 ack cost; − client+protocol change,
bench rewrite, hash-identity rules need care; the cheap half (skip
re-serialize) is already in Option A.
**Effort:** Medium · **Risk:** Medium

## Recommendation
**Option A**, shipping instrumentation + path-dedup in ONE deploy, then
re-sweep. B only if A's telemetry shows queue buildup persists after dedup;
C's full wire change only if the histogram still blames verify after A.

## Open Questions
1. friend2 upgrade timing (owner action) — gates any future wire change; not
   required for A.
2. Leader-forward hash-only: keep full-body when `--native-gossip=false`
   (fallback mode) — flag-conditional.
3. Is execution a second wall near 50k+? The post-fix re-sweep answers it
   (exec lag is transient today: 437-blk during drain, 2 at idle).
4. Bench client signing is CPU-bound at bs1000 — pre-generate actions before
   the window (bench-only task, keeps measurement honest).
5. zstd DA/gossip compression (3-5x): defer to post-3.5 with a v2 topic once
   friend2 upgrades (mixed-version constraint).
