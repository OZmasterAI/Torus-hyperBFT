# Sprint S395: blockspeed + native orders/s + height-drag

Branch: `sprint/blockspeed-orders-s395` (from f800bce / b96f058 code).

## Goals
1. **Blockspeed ~100ms on testnet.** Physics: WAN legs 227–502ms are genuine RTT
   (mem 2b492ba356ea9285); chain-wide sub-100ms needs rotation-neighbor RTT ≤80–90ms.
   Deliverable split: (a) shave the 75–93ms non-RTT fresh-chain floor so the *fast leg*
   and *local nets* go sub-100ms; (b) leader-rotation biasing (env-gated, default off —
   consensus-visible, needs all validators) for the geo chain-wide number.
2. **Native orders/s ≥150k (record 68.9k, s379 bs400).** Levers (mem 6e0b028f/e1c8efd0):
   dedupe double secp256k1 recovery at finalize (~2x), rayon-parallelize verify (~6–8x on
   8 cores), raise NATIVE_PER_BLOCK_CAP 64→256 / NATIVE_TOTAL_BLOCK_CAP 1000→4000
   (env-gated), order-book delta persist (native_executor.rs:120-207 full
   reload+rewrite per block, O(all resting orders)).
3. **Height-correlated slowdown.** Root cause found + fixed pre-sprint (cf_consensus_meta
   never pruned; f99d07a block-tree pruner). Sprint validates on live testnet: relaunch at
   b96f058 → DB 528.9MB baseline → watch torus_db_size_bytes fall ~244MB + view_duration
   flatten at h≥603k. If drag remains, profile further.

## Constraints
- Live testnet = seed (local) + val1 (ssh) + val3 (friend, no access, older build).
  Only node-local changes are deployable mid-sprint; consensus-visible changes
  (leader schedule) must be env-gated default-off.
- Determinism: exec-path changes must be bit-identical across validators
  (parallel verify must not reorder observable effects or error selection).
- State back-compat: order-book persistence changes must read existing CF contents.

## Plan
- Phase 0 (done): relaunch seed+val1 at b96f058; watchers for liveness + DB drain.
- Phase 1 (running): 4 explore agents — exec-verify path, order-book persist,
  per-view latency floor, bench harness inventory.
- Phase 2: implement (tests first): verify dedupe+rayon; caps env knobs; floor shaves;
  order-book delta persist; optional leader-rotation bias (gated).
- Phase 3: local 3-val bench before/after; deploy safe subset to seed+val1; live re-bench.
- Phase 4: report + wrap-up (memories, state, commits).

## Measurement
- orders/s: tools/bench-throughput consensus mode (bs300/400 sweep params in
  testnet/bench-s388/), inclusion-verified.
- blockspeed: torus_commit_interval_seconds / torus_view_duration_seconds deltas
  windowed per run; local net for code-effect isolation.
- height-drag: prune-watch.csv (30s samples: height, view, db_bytes, commit/view sums).
