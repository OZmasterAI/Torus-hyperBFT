# O1 — Per-block sender-balance cache in execute_batch (margin phase)

**Status:** planned S412 · **Branch:** sprint/blockspeed-orders-s395 · **Scope:** devnet-only until proven
**Goal:** exec_phase_margin_seconds ~13µs/order → ~2–4µs at bs400 (native-order-flood, 100 senders). Expected ~1.6× orders/s (mem `2484d5d60814db2b`).

## Problem

Every balance touch in `execute_batch` goes through `NativeStateOverlay::{get,put}_cf_raw`
(`crates/torus-state/src/backend.rs:366`): an `RwLock` acquisition + `(String, Vec<u8>)` key
allocation + `BTreeMap` lookup + value `Vec` clone + Borsh decode — per touch, plus a RocksDB
point-get on first touch per sender per block (overlay is created fresh per block,
`app.rs:385`). At bs400 with 100 flood senders each block re-pays ~4 balance get/put pairs
per sender. Margin phase (Phase 2) is one get+put per order and is the top exec cost.

## Design — write-back (revised S412 after write-through measured no-win)

**Write-through was tried first (per the original directive) and did not help:** micro-bench
median 1.18 ms → 1.22 ms (within noise). Mechanism: the margin phase's cost is the per-order
overlay **PUT** (`put_cf_raw` = `String` CF-name alloc + key/value `Vec`s + Borsh encode +
`RwLock` + `BTreeMap` insert), not the reads — repeat reads already hit the overlay's
in-memory pending map cheaply. Write-through leaves all ~4×N PUTs per block and *adds* HashMap
overhead, so it can't win. The lever is reducing PUT count.

**Write-back** cache, scoped to ONE `execute_batch` call (created at Phase 2 entry, flushed at
end of Phase 4):

```rust
struct BalanceCache { map: HashMap<Address, NativeBalance>, dirty: HashSet<Address> }
// load(positions, addr)            — hit: clone; miss: overlay read + insert (clean)
// set(addr, bal)                   — mutate cache, mark dirty (NO overlay PUT)
// flush_and_evict(positions, addr) — if dirty: PUT once, then evict (before apply_fill)
// flush_all(positions)             — PUT every dirty entry once (end of call)
```

- **Reserve (P2) + release (P4)** call `set` → cache-only, deferred. A sender hit 4× in a
  block is one PUT at flush, not four.
- **Coherence**: the overlay must be authoritative wherever another reader observes it. The
  only in-call bypassing reader is `apply_fill` (credits realized PnL straight to the
  overlay). Before each `apply_fill`, `flush_and_evict(taker/maker)` writes the post-release
  balance and hands authority to the overlay, so the credit isn't clobbered by `flush_all`.
  `flush_all` at end-of-call materializes everything before the *next* `execute_batch` call
  and post-batch consumers (`drain_core_writer`, `save_order_books`, block-end flush) run.
- **Determinism**: flush keys are distinct per sender → final overlay state is order-
  independent; flushed in sorted address order anyway. Map/set never iterated elsewhere.
- Expected PUT reduction ~4×N → ~N per block; net overlay ops ~800 → ~200 in the 400-order/
  100-sender bench.

### Routed sites (complete set inside the P2→P4 window)

1. Phase 2 margin reserve — `native_executor.rs:494-508`
2. Phase 4 margin release — `native_executor.rs:614-617`
3. Phase 4 fill PnL credit — `apply_fill` at `:626`/`:641` calls `credit_realized_pnl`
   internally (direct overlay write → would leave the cache stale). **Chosen approach:
   invalidate, not split.** Because the overlay is authoritative (write-through), the
   batch path simply calls `bal_cache.invalidate(taker)` + `invalidate(maker)` before
   each `apply_fill` pair; the next `load` re-reads the credited balance from the overlay.
   This leaves `PositionManager::apply_fill` and `credit_realized_pnl` untouched — zero
   risk to liquidation.rs and the single-order path — and `apply_fill` is not in the
   margin phase anyway, so the O1 target's speedup is unaffected. (Rejected the
   `apply_fill_positions_only` split as more invasive for no perf gain on the hot path.)

### Explicitly NOT touched (audited S412)

- Phase 1 actions (lockbox deposit/withdraw via `atomic_write`, cancel/modify margin
  release, liquidation) — run before Phase 2, before the cache exists.
- `MarginEngine::check_*` — only called from liquidation.rs (Phase 1).
- Phase 3 parallel matching — order books only, no balance/position access.
- Post-batch (governance, distribute_fees, epoch boundary) — cache already dropped.
- Positions (`get_position`/`put_position` in apply_fill) — **O1b follow-up**, only if
  settle phase shows hot under fill-heavy load after O1.
- Overlay data structures (`BTreeMap<(String, Vec<u8>)>`) — separate bench-gated candidate.

## Tests (green pre-change; encode current semantics)

In `parallel_matching_tests.rs`:
1. Same-sender N-order batch → final `available`/`order_margin` equals N single-order batches.
2. Stale-cache detector: same sender, two markets, Phase-4 sequence = release(mkt A) →
   realized-PnL fill credit → release(mkt B); exact final balance assert. Fails if any of
   the three sites bypasses the cache.
3. `apply_fill` split unit tests: open/increase → no credit; partial/full/flip close →
   credit (incl. zero-PnL close still writes the balance key).

## Proof

1. Full suite: torus-bridge, torus-core (determinism.rs, fuzz_tests.rs), integration.
   Known pre-existing failures: hotstuff_rs pacemaker cumulative-deadline, crash_recovery.
2. Micro A/B: execute_batch, 400 resting limit orders / 100 senders over
   `NativeStateOverlay` (prod shape), before/after.
3. Devnet flood A/B: `exec_phase_margin_seconds` under `native-order-flood.py` at bs400,
   same harness as S395 baseline (margin ~13µs/order). Success = ≤4µs/order, no
   settle-phase regression, determinism tests green.

## Results (S412)

- **Correctness**: 242 tests pass across torus-bridge + torus-core (zero failures),
  including both coherence tests (`batch_same_sender_orders_equal_individual_batches`,
  `same_sender_multi_market_settle_balance_exact` — the stale-cache detector).
- **Micro-bench** (`exec_batch_margin`, 400 resting orders / 100 senders over
  `NativeStateOverlay`): no-cache baseline **1177 µs** median → write-through 1224 µs
  (null, in noise) → **write-back 831 µs (−29%)**. Baseline CI [1073, 1299] vs write-back
  [729, 959] do not overlap → statistically real. ~800 overlay ops → ~200 (600 saved:
  300 cache-hit reads + 300 flush-coalesced writes).
- **Caveat**: the micro-bench runs on a warm, tiny DB where overlay ops are cheap, so it
  understates the prod win (S395 measured margin ≈13 µs/order on a contended 8-core with a
  colder RocksDB). Real proof deferred to the live `exec_phase_margin_seconds` flood A/B,
  gated on the testnet relaunch.
