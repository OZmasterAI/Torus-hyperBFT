# Design: Block-Processing (Execution) Ceiling

**Status:** BRAINSTORM (s351) — option pending user pick
**Trigger:** Sprint-5 A/B exposed inclusion flat at ~31–67 actions/s under
bs500 load while ingress accepted 2.24x more (mem 4e624042).

## Problem

Under heavy native load the chain's block cadence collapses from ~10 blk/s
idle to 2–40 blk/min, with blocks carrying ~90–100 actions (~45–50K orders).
Throughput cap measured: ~15.6k orders/s solo, ~31k dual — far below the
wire or ingress limits. Heavier accepted ingress makes cadence WORSE
(bin leg: 8 blk/min vs json 38).

## Verified anatomy (read + probed, s351)

- CTE pipeline: consensus commits → `sync_channel(64)` → single execution
  thread (`execution_loop`, app.rs:432; channel bound app.rs:652). When
  exec needs 1–2s per heavy block, the 64-deep buffer fills in seconds and
  `on_committed_block`'s send BLOCKS the consensus thread → views stall →
  cadence inherits exec speed. The channel is doing its job; exec is the
  ceiling.
- Exec per block (`execute_committed_block`, app.rs:176): rayon batch
  sig-verify (single pass, double-recovery already FIXED on this branch) →
  per-action nonce replay-guard (one RocksDB point read per action,
  app.rs:333) + full `signed.action.clone()` → `NativeExecutor::execute_batch`
  (matching engine, ~50K orders serial) → `save_order_books()` (serializes
  ENTIRE books; books GROW across the day — cadence degraded 41→8 blk/min
  from morning sweep to afternoon A/B) → consumed-nonce writes → atomic
  flush + incremental native trie maintenance (Phase A A2.2).
- ELIMINATED: DA reconstruction misses (0 in full log), gossip publish
  failures during load (0), validate-path sig cost (attestation +
  rayon), ingress (Sprint 5 A→C→B shipped).
- Metrics exist but are DEAD: `block_build_seconds`,
  `state_root_compute_seconds`, `native_actions_processed` all 0 across
  1,393 committed blocks. No phase visibility inside exec.
- Caps: NATIVE_PER_BLOCK_CAP=64 is per-SENDER; total block cap 1000
  (rate_limit.rs:33/37) — observed ~100 actions/block with 10 senders.

## Options

### Option A: Instrument the exec thread (phase timers + queue gauge)
Wire the dead metrics and add: per-phase histograms inside
`execute_committed_block` (verify / replay-guard / execute_batch /
save_order_books / flush+trie), an exec-queue-depth gauge (sender side),
and `native_actions_processed`. One bs500 probe then names the dominant
phase with numbers.
- Files: torus-consensus/src/app.rs, torus-telemetry/src/lib.rs.
- Trade-offs: no throughput gain itself; turns B/C/D from guesses into a
  one-line verdict. Proven pattern (paid off twice today).
- Effort: Small. Risk: Low.

### Option B: Order-book persistence cost (dirty-only / periodic snapshot)
If `save_order_books` dominates (strong prior: O(total book size) per
block, books grow unboundedly under bench load): persist only dirty
markets, and/or snapshot every N blocks with replay-from-actions recovery
in between, and/or switch the book CF to per-level keys so writes are
O(touched levels) not O(book).
- Files: torus-core (order book store), torus-consensus/src/app.rs
  (save_order_books call), recovery path.
- Trade-offs: big win if hot; recovery semantics must stay exact
  (crash between snapshots ⇒ replay or WAL). Per-level keys is the real
  fix but touches the read path too.
- Effort: Small (dirty-only) → Medium (per-level). Risk: Medium
  (crash-recovery correctness).

### Option C: Pipeline exec phases across blocks
Verify(block N+1) ∥ execute(block N) ∥ flush(block N−1): three stages on
three threads, deterministic order preserved per stage. Multiplies exec
throughput by ~the number of balanced stages without touching the engine.
- Files: torus-consensus/src/app.rs (execution_loop → staged channels).
- Trade-offs: keeps determinism (single executor stage) while overlapping
  crypto and I/O; complexity in error/replay paths (applied-height
  tracking per stage). Doesn't help if ONE stage is 90% of the time —
  needs A's data to size the win.
- Effort: Medium. Risk: Medium (replay/restart correctness).

### Option D: Cap/channel tuning (palliative)
Lower total per-block action cap (e.g. 1000→256) to bound worst-case
block weight, and/or deepen the exec channel. Smooths latency spikes;
does NOT raise sustained orders/s (steady-state still equals exec rate).
- Files: rate_limit.rs constants, app.rs:652.
- Trade-offs: one-line, reversible, useful as a guard regardless; zero
  effect on the ceiling itself.
- Effort: Trivial. Risk: Low.

## Recommendation

A first (hours, decides everything), then B if save_order_books dominates
(the growth-correlated cadence decay points there), else C if it's spread
across phases. D optionally rides along as a safety bound. The matching
engine itself is LAST suspect — it was benched standalone far above these
rates.

## Open Questions

1. Which phase owns the 1–2s? (A answers.)
2. Book size right now on market 1 after today's ~2M orders — and does
   `save_order_books` serialize per-market or all-markets every block?
3. Does exec-queue depth actually hit 64 under load (gauge will show
   the backpressure directly)?
4. bench cleanup: do bench orders ever cancel/fill, or do books grow
   without bound (test-realism question that affects B's priority)?
