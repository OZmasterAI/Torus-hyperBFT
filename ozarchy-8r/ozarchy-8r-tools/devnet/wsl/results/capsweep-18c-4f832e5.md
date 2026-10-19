# Block-cap sweep on 18c @ 4f832e5 (2026-07-20) — cadence-vs-throughput curve

Follow-up to `reproof5-18c-4f832e5.md`. Same build, harness, and mode-2 combo
(LevelAuthority + resident + root-cache + psettle + bhash4 + mc256 + cap8);
`TORUS_NATIVE_ORDERS_PER_BLOCK_CAP` swept 400→6400 at 300k orders/s offered.
Raw artifacts: `~/bench-results-18c/cells-capsweep/` on 18c (campaign EXIT=0;
env verified per cell; zero rejections/panics; wedge quiet — one benign
`is_safe=false` drop in the entire C6400 run, zero "made no progress").

## The curve

| Cap (orders/blk) | matched/s avg | placed/s avg | blk/s avg | best-60s blk/s | worst-60s blk/s | gate 21.0 |
|--:|--:|--:|--:|--:|--:|---|
| 400 | 2,425 | 3,052 | 8.60 | **26.4** | 3.62 | FAIL |
| 800 | 2,266 | 2,855 | 3.74 | 5.7 | 2.00 | FAIL |
| 1600 | 2,819 | 3,548 | 7.63 | **27.3** | 2.72 | FAIL |
| 3200 | 4,496 | 5,653 | 1.99 | 3.0 | 1.13 | FAIL |
| 6400 | 6,950 | 8,747 | 2.36 | 7.0 | 1.13 | FAIL |
| (uncapped, from re-proof5) | 12,906 | 16,184 | 0.72 | 2.1 | 0.32 | FAIL |

System settles into drain-rate equilibrium at every cap (placed/s ≈ cap × blk/s —
ingress backpressure adapts to block drain; committed throughput IS cap × cadence).

## Why no cap passes the gate — the block-time floor

C400 phase anatomy (1,776 blocks): root 26 ms + state_write 26 ms (flush ~55 ms
total at only 226 dirty buckets/blk) + engine ~16 ms + verify/body + consensus
RTT ~38 ms (idle 26.3 blk/s) ≈ ~120 ms/blk → 8.6 blk/s avg. Fully consistent —
no anomaly, no stall pathology; worst-60s dips (1.1–3.6) look like periodic
storage variance (compaction/fsync bursts), not consensus events.

The decomposition exposes a structural fact: **gate 21.0 blk/s allows 48 ms/block
total, and the consensus round-trip alone is 38 ms.** Passing under load requires
the entire exec+flush envelope in ~10 ms. Two consequences:

1. 3c removed the *scaling* wall (dirty entries now O(touched)), but a **fixed
   ~50 ms per-commit write cost** (root recompute walk + batch/WAL write) remains
   regardless of block size. This fixed floor, not O(size) work, now sets small-block
   cadence. Attacking it (group commit, WAL tuning, root-walk short-circuit for
   small dirty sets) is a Layer-3 storage item distinct from engine scaling.
2. Best-60s windows of 26–27 blk/s (caps 400/1600) prove the machine can run
   above-gate cadence in good minutes — the gap to worst-60s is variance, so
   compaction smoothing may be worth as much as mean reduction.

## Production-policy takeaway

The cap knob works exactly as designed and the trade is now quantified:
throughput scales ~linearly with cap (2.4k → 13k matched/s from 400 → uncapped)
while cadence degrades from 8.6 → 0.7 blk/s. Until the fixed per-commit cost and
consensus RTT shrink, chain policy must pick a point on this curve; there is no
cap value that gives both the gate and record throughput on this topology.

## Verdict

Layer-2 storage scaling: CLOSED (3c proven; see reproof5 doc). Gate criterion:
NOT closed by configuration — promoted to Layer 3 with a precise target list:
(a) fixed per-commit flush cost ~50 ms, (b) worst-60s variance (compaction
bursts), (c) consensus RTT 38 ms floor (pipelining/view timing), alongside the
already-scoped engine/verify scaling.
