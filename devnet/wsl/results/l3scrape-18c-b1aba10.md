# L3 pegged-cell scrape + async-post-flush A/B — 18c @ b1aba10 (2026-07-21)

Campaign `run-l3scrape.sh`, cells in `~/bench-results-18c/cells-l3scrape/` on 18c.
Merged tip b1aba10 (= re-proof5 ⊕ l3-verify-par ⊕ l3-engine-par ⊕ l3-flush-pipe, all
flags default-off). Bench-standard env + cap8 + cap-400, 300s bench @ rate 750
(300k/s offered), 3-node devnet, node-Prometheus ground truth. First-ever scrape of
the FULL `exec_*` stage set at a pegged cap-400 cell.

## Cells

| cell | flags | blk/s | matched/s | view mean | verdict |
|---|---|---|---|---|---|
| S400_OFF | (bench-std) | 0.32 | 183 | 1171ms | **INVALID — first-cell anomaly** (see §3) |
| S400_ON | +ASYNC_POST_FLUSH=1 | 7.46 | 2,448 | 120.6ms | valid |
| S400_OFF2 | (bench-std, rerun) | 8.43 | 2,403 | 109.2ms | valid control |

## 1. THE HEADLINE: save_books is the dominant exec consumer (~107ms/loaded block)

Per-loaded-block stage means (9161, S400_OFF2 control, n=1748 loaded of 4528 blocks;
stages are sequential/disjoint on the one exec worker = summable):

| stage | ms | note |
|---|---:|---|
| **save_books** | **106.7** | est. 1-3ms in l3-work-budget — never measured before; flag-independent (108.8 in ON cell) |
| engine | 43.0 | match 27.6 + settle 13.1 + margin 1.0; µbench serial ~16ms ⇒ ~2.7× deschedule inflation |
| flush | 35.8 | root 9.1 + state_write 23.8 |
| verify | 7.3 | already-parallel path |
| body_persist | 3.3 | ON cell: 0.8 (deferral works, is just too small to matter) |
| evm + resync + load + replay | ~3.0 | |
| **loaded-block chain total** | **~199** | exec_block overall mean 80.9ms (incl. ~2,780 near-empty blocks) |

Arithmetic closes: (199 × 1748 + ~5 × 2780) / 4528 ≈ 81ms ≈ exec_block mean; view
109.2ms = exec drain + slack. **The old "~40-50ms untimed residual" was mostly
save_books plus the loaded/empty cadence** — per-view work averages ~81ms of exec
because only ~39% of blocks are loaded; loaded blocks cost ~199ms.

Queue is **bimodal**: pegged 49-66 during bursts, drains to 0-4 between (85/300
samples at 0) — blocks alternate heavy/empty; consensus outruns exec then waits.

save_books is "serialize dirty books" (app.rs ~:1360-1365). At cap-400 with
resident books + rows mode + journaled saves (rank8: write counts 201→≤3 proven)
and resting≈0, 107ms is unexplained by design intent — **attribution round
dispatched** (suspects: span covers more than serialization; per-market loop over
all markets regardless of dirty; journal fold cost; serialization of full book
images; deschedule inflation share unknown).

## 2. Async-post-flush A/B: NEUTRAL — stays default-off

OFF2 8.43 blk/s / 2,403 matched/s / view 109.2 vs ON 7.46 / 2,448 / 120.6. Deltas
inside single-cell shape noise; matched/s equal. Matches the exec-chain µbench
(−22% at small synthetic shape). The deferral verifiably works in vivo
(body_persist 3.3→0.8ms) but the stage is too small to matter and the win is
absorbed per the work-conservation law. Keep default-off; deprioritize.

## 3. S400_OFF first-cell anomaly (invalid, diagnosed)

0.32 blk/s with view 1171ms, NO wedge signatures (3 total drop/timeout matches),
exec_queue **0 throughout** (exec starved), RPC so slow load-gen throttled to
237/s. Node globally sluggish with no exec backlog — profile matches cold-start
(first devnet after the 30GB test-suite churn: cold page cache/RocksDB) not a code
regression: identical env rerun (OFF2) is textbook-healthy. LESSON for campaigns:
either warm up the first cell longer or treat cell-1 as a throwaway sacrificial
warmup cell.

## 4. Verify µbench (from the verify branch acceptance, for the record)

N=400: serial 81.3ms / global-pool 18.2ms (today's default) — the already-banked
win. Cold-cache verify at cap-400 in vivo measured 7.3ms (trust-cache active).

## Next

1. **save_books attribution** (dispatched): code-level span breakdown + fix.
   Winning it at even 50% ≈ −50ms/loaded block ⇒ loaded chain ~150ms ⇒ at 39%
   loaded ratio, per-view work → ~60ms; combined with engine's deschedule relief
   this is the first credible path toward the ≤48ms gate budget.
2. Then re-run gate cells (worst-60s vs 21.0).
