# Leader block_build phases (s68, 2026-09-26)

Binary `04a60f2` (main `42cde1b` + phase timers), campaign
`~/bench-results-matched/s68-bbtimers-20260926`: one warm-up cell, then two scored
cells. Cap 200, 300 s, rate 76000, 10 markets, `TORUS_BODY_FETCH_TRACE=1`, driver
worktree `wt/s68-build-timers`. All three cells were accepted, AGREE, PASS.

## LOAD-window means (ms, per leader proposal)

| cell / node | block_build | select | mirror | encode | attest | parent | propose_build | arrival | view |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| r1 val0 | 164.95 | 69.24 | 64.89 | 23.47 | 7.29 | 0.11 | 232.97 | 305.20 | 515.20 |
| r1 val1 | 153.98 | 59.91 | 65.20 | 21.92 | 6.91 | 0.08 | 226.85 | 298.60 | 507.82 |
| r1 val2 | 156.87 | 60.12 | 67.30 | 21.46 | 7.98 | 0.22 | 234.34 | 306.67 | 513.44 |
| r2 val0 | 169.17 | 66.56 | 71.62 | 23.45 | 7.63 | 0.14 | 226.51 | 311.21 | 514.59 |
| r2 val1 | 154.12 | 58.42 | 67.19 | 21.66 | 6.85 | 0.09 | 242.97 | 288.69 | 507.62 |
| r2 val2 | 164.51 | 64.70 | 68.45 | 23.23 | 8.12 | 0.08 | 239.99 | 310.34 | 512.98 |
| **mean** | **160.6** | **63.2** | **67.4** | **22.5** | **7.5** | **0.1** | **233.9** | 303.5 | 511.9 |

The phases sum to 160.7 ms, so block_build is fully attributed. The warm-up
cell had the same shape, but its mirror phase was slower (93–104 ms).

## Reading

- **mirror, 42%.** `mirror_or_drop_native` clones every selected body and
  writes all of them to the DA store again in one `put_batch`. Ingest has
  already mirrored those bodies (`mirror_to_da`, flushed before selection).
  Skipping this rewrite is blocked on durability evidence:
  `flush_da_mirrors` returns `()`, so the proposer cannot tell which bodies
  are durable (Codex issue d7129204).
- **select, 39%.** This covers `select_block_payload`: the in-flight set,
  mempool selection under the pool lock, and the EVM drain. It is not split
  further yet, so lock wait against 76k/s ingest and actual selection work
  cannot be told apart.
- encode (14%) and attest (5%) are second-order. Parent read is negligible.
- About 73 ms of `propose_build` (233.9 − 160.6) is HotStuff-side, outside
  block_build.

## Caveat: throughput is not comparable to s67

The scored cells reached 77.5k and 74.0k matched/s, against about 61.7k for
the s67 control under the same settings. Timers cannot plausibly add ~20%.
There is no same-day control cell, so this run supports no throughput claim.
Run `main42c` back to back with this binary before reading anything into it.
