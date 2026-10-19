# RE-PROOF5 — Layer-2 proof on 18c @ 4f832e5 (2026-07-20)

First devnet proof of the 3c preimage round (`perf/level-rows` = level-rows-as-authority +
hash-only mirror + journal-in-book, merged into `perf/re-proof4` → `perf/re-proof5`).
Machine: 18c (18 cores, 94GB). Harness: identical to `baseline-18c-95cd399.md` run 2
(same scripts, same load shape, node-Prometheus counters only; new 1Hz `dirty.csv`
scraper for the 3c per-CF dirty-entry telemetry). Raw artifacts:
`~/bench-results-18c/cells-rp5/` on 18c. Campaign log: `campaign-rp5.log` (EXIT=0).

Cells: RI (idle) + R190/R750/R1000 = the mode-2 combo at 76k / 300k / 400k orders/s
offered. Combo env (fleet-uniform, verified via /proc/environ + startup lines per cell):
`TORUS_BOOK_ROWS=2` (LevelAuthority, fresh genesis per cell via CLEAN=1) +
`TORUS_RESIDENT_BOOKS=1` + `TORUS_NATIVE_ROOT_CACHE=1` + `TORUS_PARALLEL_SETTLE=1` +
`TORUS_PARALLEL_BUCKET_HASH=4` + `TORUS_BUCKET_MEMBER_CACHE_MB=256` +
`TORUS_COMMIT_LAG_BACKOFF_CAP=8`.

## Headline table

| Cell | offered | matched/s avg | matched/s best-60 | placed/s avg | blk/s avg | worst-60 blk/s | gate 18.7* |
|---|--:|--:|--:|--:|--:|--:|---|
| RI idle | — | — | — | — | **26.26** | — | PASS (idle) |
| R190 | 76k/s | **13,659** | **23,160** | 17,123 | 0.622 | 0.267 | FAIL |
| R750 | 300k/s | 12,906 | 18,718 | 16,184 | 0.716 | 0.317 | FAIL |
| R1000 | 400k/s | 13,371 | 19,570 | 16,777 | 0.756 | 0.350 | FAIL |

\* machine-normalized gate = 0.8 × box idle (26.26) ≈ 21.0 on this build; the
cross-build 18c reference gate from the baseline doc is 18.7. FAIL either way — see verdict.

Zero rejections (margin/book/other), zero panics, zero fail-stops, exec_queue pegged
64–66 in all loaded cells. **NEW MISSION RECORDS: 13,659 matched/s window-avg and
23,160 matched/s best-60s** (mode-1 records were 9,804 / 15,618).

## What 3c did to the flush (per-block, from phase.csv finals; baseline = mode-1 S4/W cells)

| Phase | mode-1 baseline | mode-2 R190/R750/R1000 | change |
|---|--:|--:|--:|
| root | ~750ms | 100 / 113 / 104 ms | **~7× down** |
| state_write | ~480ms | 113 / 99 / 102 ms | **~4.5× down** |
| flush total | ~1.4s | 231 / 229 / 220 ms | **~6× down** |
| dirty buckets/blk | 4,900–18,263 | 1,988 / 1,654 / 1,665 | **~10× down** |
| load_books | (rank8: ~0) | 0.1 / 10 / 19 ms | resident holder working |

Design's dirty-bucket arithmetic confirmed almost exactly (predicted ~1.8k/blk).

## Per-CF dirty composition (R750, 200 loaded blocks — the new 3c telemetry)

| CF | entries/blk | share |
|---|--:|--:|
| positions | 1,318 | **79%** |
| balances | 238 | 14% |
| order_books (levels+meta+stops) | 124 | 7% |
| levels written/blk | ~96 | — |

The design's caveat materialized as predicted: **positions/balances are now the dirty
floor**, books are no longer the driver (124/blk vs ~16k order rows in mode 1).

## Verdict

1. **3c works as designed.** Storage/flush is no longer the exec-envelope wall: flush
   fell from ~1.4s to ~0.23s/blk while matched/s rose +36–39% to new records. Idle also
   rose 23.4 → 26.26 blk/s (+12%; plausibly the d2l sweep-gate fix — the gossip-mode
   re-forward churn is gone).
2. **Gate still FAILS — the wall moved to compute, not storage.** Blocks are huge
   (~27k orders each, ~1.3–1.6s wall): with flush at 0.23s, the residual ~1.1s/blk is
   engine + verify + body_persist (not covered by exec-phase timers) + consensus.
   This is the handoff's predicted Layer-3 frontier ("engine share grows as storage
   shrinks").
3. **Offered-rate insensitivity persists**: 76k→400k offered all land at ~16–17k
   placed/s (backpressure-paced ingress; frontier findings #1–6 unchanged).
4. **Capped-block cells are now the rational gate play** (previously pointless: the old
   ~2.5s fixed flush floor dominated any small block). Marginal compute is now
   ~40µs/order and the consensus floor is ~38ms (idle 26.3 blk/s). Arithmetic sketch:
   cap ≈ 1,600 orders → ~8–12 blk/s; cap ≈ 400 → possibly ~15–18 blk/s at reduced
   matched/s. A `TORUS_NATIVE_TOTAL_BLOCK_CAP` sweep would map the cadence-vs-throughput
   curve and show whether gate-pass and record throughput can coexist.

## Provenance

- Build: `perf/re-proof5` @ `4f832e5` (= re-proof4 95cd399 ⊕ level-rows 2d319d7 merge
  21755f2 + sweep-gate fix 9916b17 + ChainConfig test fix 7625199 + dedup-scope fix
  4f832e5). Full suite green on VPS (98 suites; torus-network 105/105; known
  validator-set flake passes standalone).
- Two latent B2-verifier bugs fixed en route (both also cherry-picked to
  `perf/dissemination` @ 076c217): d2l re-forward sweep ran in gossip mode
  (exact-today drift, live in ALL prior proof benches); dual-path dedup suppressed
  pacemaker/block-sync timer rebroadcasts (liveness hazard with the fan on). Root cause
  of the masking: torus-node/consensus **test targets never compiled** on the
  re-proof3/4 lineage.
- VPS idle probe @ 95cd399 (same day): 29.7–29.9 blk/s — reproduces the historical
  29.78; the 18c idle gap is machine-inherent, motivating the normalized gate.
- Known cosmetic issue: `analysis.txt` phase60 sections are empty (awk anchored on
  early all-zero rows); phase.csv itself is complete and is what this doc uses.
