# Design: Sprint 1 — Throughput Rails (byte-cap, mempool TTL, ingress queue, size-aware sync pull)

**Session:** 334 · **Branch:** `fix/native-da-push-hardening` · **Status:** DESIGN

## Problem

The s334 off-box bs-sweep (bs500 = 28,219 orders/s record) found two walls:

1. **bs1000 wedged the live chain**: blocks hit the 50k-order cap (~7.5MB of bodies),
   which cannot cross the WAN inside a view. Every leader re-proposed mega-blocks from
   stuffed mempools; with no pool expiry there was no self-heal (manual restarts required).
   Worse: after `NONCE_WINDOW_MS` (60s) those pooled actions could **never** validate
   again, yet stayed selectable forever.
2. **Ingress ceiling**: `submit_semaphore = 16` (torus-rpc/lib.rs:175) with `try_acquire`
   → instant "overloaded" rejections under burst; one tunnel delivered only 35–92 act/s.

## Context (memory + code)

- Hot-path DA pull is deliberately bounded ≤260ms (RECONSTRUCT 5×20ms + HOT_PULL 8×20ms,
  app.rs:556-571) to stay under the 500ms view timeout — a body that can't arrive
  in-budget fails the view *by design*. Do NOT extend hot budgets; cap body size instead.
- Sync-path pull budget is flat ~1s (PULL_RETRIES 20 × PULL_DELAY 50ms, app.rs:553-554)
  and MAY block safely (off the voting path).
- `NativePoolEntry` (native_pool.rs:14) has no size or age metadata; selection
  (`select_for_block_with_senders_excluding`, native_pool.rs:204) caps by action count
  + per-sender count + order count only.
- Admission already rejects nonce-stale actions (`NONCE_WINDOW_MS`, lib.rs:264) — but
  only at ingress, never retroactively in the pool.
- Wedge memory: b725a697 (bs1000 collapse), 2842da1d (view divergence/recovery).

## Options

### T1 — Byte-cap block selection
- **A. Constant estimate** (`orders × 150B`): zero storage; drifts from real encoding.
- **B. Store `encoded_len` at insert** (bincode len of `SignedNativeAction`), sum during
  selection against `NATIVE_BLOCK_BYTES_CAP`: exact, O(1) per entry, one `usize` each. ✅
- **C. Serialize at selection**: exact but O(MB) work per proposal.

### T2 — Mempool TTL
- **A. Background sweeper task**: extra thread + locking for little gain.
- **B. Lazy expiry from the nonce already stored**: entry expired iff
  `action.nonce + NONCE_WINDOW_MS < now_ms` — evict during selection/drain. Zero new
  state; pool liveness exactly matches protocol validity (an expired entry could never
  be included anyway). ✅

### T3 — Ingress semaphore
- **A. Bump 16→64, keep `try_acquire`**: still rejects bursts.
- **B. 64 permits + `acquire` with 250ms timeout**: absorbs bursts as a short bounded
  queue, still sheds load when saturated. ✅
- **C. Unbounded acquire**: no overload backstop — rejected.

### T4 — Size-aware sync pull budget
- **A. Flat 1s** (today): too small for multi-MB body sets on sync path.
- **B. Scale retries with missing-body count**: `retries = clamp(20, missing/2, 160)`
  (~1s floor, ~8s ceiling at 50ms delay). Sync path may block; hot path untouched. ✅

## Recommendation

B across the board. All four changes are **validator-local** (no protocol/wire change):
deployable to our node + friend1 without friend2 upgrading.

Initial constants (tunable, same style as existing caps):
- `NATIVE_BLOCK_BYTES_CAP: usize = 2_000_000` (~2MB ≈ 13k orders ≈ what pre-warm push +
  pull demonstrably moved at bs500; bs1000-class load now degrades to more, smaller
  blocks instead of wedging).
- `SUBMIT_PERMITS: 64`, `SUBMIT_QUEUE_TIMEOUT_MS: 250`.
- Sync pull ceiling 160 retries (~8s).

## Open Questions
- Exact `encoded_len` source: bincode of the action as embedded in the block body
  (consensus datum encoding) — verify encoding used by `block_bytes` at app.rs:540.
- Whether `select_*` order-cap loop (app.rs:1201 caller) also needs the byte budget
  threaded through `Mempool` wrapper signatures (yes — additive parameter or struct cap).
