# RE-PROOF — depth-scaling bench (perf/re-proof @ cc8e39e)

**Date:** 2026-07-19 · **Box:** VPS 95.111.231.121 (8 cores / 23 GiB) · **Branch:** `perf/re-proof` @ **`cc8e39e`** (= early-proof C+D + rank8 resident books + rank-root trie cache + profiler phase timers; hand-merged `torus-consensus/src/app.rs`, compiled clean — warnings only, 3m22s).
**Reference:** re-baseline cell750 = **2,483 matched/s**; re-baseline four-cell band **2,483–3,231 matched/s** (flat exec-bound ceiling). Load shape identical to re-baseline (`--senders 5000 --markets 10 --batch-size 400 --econ --rate-total 750`, `--metrics-urls` all 3 nodes, no `--sweep-bodies`, ~330 s window, fresh chain per cell, fleet-uniform env).
**New instrumentation:** per-observation exec phase timers (Prometheus histograms `_sum`/`_count`, per-block avg = Δsum/Δcount) scraped at 1 Hz on val0, reported EARLY window (first 60 s of load, books shallow) vs LATE window (last 60 s, books deep). This is the depth-scaling test: R0 should degrade early→late (O(depth)); the win combo should flatten it (toward O(actions)).

## Verdict

> **The depth-scaling FIX is demonstrably working at the phase-timer level — but it has NOT moved the headline throughput or the health gate.** The win combo flattens the per-block cost's *slope* vs book depth (root 5.5× → 2.0×, state_write 5.7× → 1.8×, flush 5.6× → 1.9× late/early; load_books late cost collapses to ~0 with resident books). So per-block cost became **markedly less O(depth)**. However matched/s stays flat (R0 3,224 · R1 2,920 · R2 2,568 — all inside/below the 2,483–3,231 band) and **the gate FAILs in every cell** (worst-60s 0.000 blk/s). Two reasons: (1) `TORUS_BOOK_ROWS=1` row-per-order persistence explodes `dirty_buckets` per block 8–10× (549→~1,800 in R0 vs 4,900→18,000 in R1/R2), a large new *constant* that offsets the depth-scaling win; (2) the residual absolute per-block cost (root ~0.85–1.2 s, flush ~1.25–1.8 s, state_write ~0.36–0.5 s) is still far too high for the 42 ms/block (23.8 blk/s) the gate needs. Real mechanism progress; not yet a throughput breakthrough.

## Headline throughput (node-counter, ~330 s window)

| cell | config | placed/s | matched/s | vs 2,483 | blk time | worst-60s | idle | gate |
|---|---|---:|---:|---:|---:|---:|---:|---|
| **R0** merge-sanity | all new flags OFF, cap 100 | **4,070** | **3,224** | +29.8% | 5428 ms | 0.000 | 30.6 | FAIL |
| **R1** win combo | BOOK_ROWS, RESIDENT_BOOKS, NATIVE_ROOT_CACHE, PARALLEL_SETTLE, MIN_FILLS=64; cap 100 | **3,679** | **2,920** | +17.6% | 6167 ms | 0.000 | 30.5 | FAIL |
| **R2** win combo + capacity | R1 + NATIVE_TOTAL_BLOCK_CAP=400 + VERIFIED_SENDER_CACHE_CAP=25600 | **3,240** | **2,568** | +3.4% | 5790 ms | 0.000 | 29.6 | FAIL |

Zero rejections everywhere (A5 intact). `exec_queue_depth` peaks 66 (saturated) in all three. All idle references healthy (29.6–30.6 blk/s) — no confounded cells this run. Best-60s matched rose with block size (R0 7,851 → R1 9,397 → R2 11,260 /s) — burst capacity improves, sustained does not.

## Phase timers — EARLY vs LATE per-block (the depth-scaling story)

**R0 (flags off) — textbook O(depth): everything balloons as books fill (resting 123k → 1.27M).**

| phase (ms/block) | EARLY | LATE | ×late/early |
|---|---:|---:|---:|
| load_books | 30.6 | 393.0 | **12.9×** |
| root | 183.6 | 1011.2 | 5.5× |
| state_write | 135.9 | 767.9 | 5.7× |
| flush | 319.7 | 1779.4 | 5.6× |
| dirty_buckets/obs | 549 | 1813 | 3.3× |

**R1 (win combo, cap 100) — slope flattened, but dirty_buckets constant explodes.**

| phase (ms/block) | EARLY | LATE | ×late/early | vs R0 late |
|---|---:|---:|---:|---|
| load_books | 5.5 | 115.1 | 20.9× † | 393 → **115** (resident books, ⅓ cost) |
| root | 427.4 | 854.2 | **2.0×** | 1011 → 854 |
| state_write | 198.4 | 357.6 | **1.8×** | 768 → **358** (½) |
| flush | 662.3 | 1254.2 | **1.9×** | 1779 → 1254 |
| dirty_buckets/obs | 4900 | 14201 | 2.9× | **8–10× higher constant** |

† load_books early is ~5 ms so the ratio is noise; the meaningful figure is the late absolute (115 ms, ⅓ of R0's 393 ms).

**R2 (win combo + cap 400) — resident books zero out load_books, but bigger blocks rescale root/flush/dirty.**

| phase (ms/block) | EARLY | LATE | ×late/early |
|---|---:|---:|---:|
| load_books | 27.3 | **0.09** | ~0 (resident, no reload) |
| root | 318.3 | 1212.4 | 3.8× |
| state_write | 228.9 | 491.0 | 2.2× |
| flush | 576.0 | 1772.1 | 3.1× |
| evm_resync | 0.0 | 0.6 | negligible |
| dirty_buckets/obs | 4240 | 18263 | 4.3× |

## Honest answers to the three questions

1. **Did per-block cost become O(actions) instead of O(depth)?** *Partially — yes for the slope.* The late/early growth ratio dropped from ~5.5× (R0) to ~1.8–2.0× (R1) for root/state_write/flush, and resident books collapsed load_books' late cost (393 ms → 115 ms at cap 100, → 0.09 ms at cap 400). The depth-sensitivity is materially reduced. **But** BOOK_ROWS row-per-order persistence introduced a large depth-*independent* constant (dirty_buckets 8–10× higher), so the *absolute* per-block cost did not fall enough to matter.

2. **Does matched/s finally move?** *No.* R0 3,224 · R1 2,920 · R2 2,568 — flat-to-slightly-lower vs the 2,483–3,231 re-baseline band. Flattening the depth slope did not raise sustained matched throughput at 300 k offered; the remaining constant per-block cost (root+flush+state_write ≈ 2–2.5 s/block) still bounds it.

3. **Does the gate hold anywhere?** *No.* worst-60s = 0.000 blk/s in R0, R1, R2; block time 5.4–6.2 s/block everywhere, nowhere near the 42 ms/block the ≥23.8 blk/s gate requires. Periodic multi-second flush/root stalls persist.

## Where the cost now sits (post-win-combo)
With depth-scaling largely neutralized, the residual per-block wall is **root computation + flush**, inflated by the row-per-order dirty-bucket churn from `TORUS_BOOK_ROWS=1`. The obvious next lever is reducing dirty-bucket count per block (coarser book rows, or batching row writes) so `NATIVE_ROOT_CACHE`'s flattened slope sits on a *lower* constant. Raising the block cap (R2) is counterproductive alone — it rescales root/flush/dirty with block size and lowered matched/s.

## Files
- `reproof-R{0,1,2}.csv` — 1 Hz val0 funnel + phase-timer scrapes (fresh chain each).
- `scrape-phase.sh`, `phase60.awk` — phase capture + early/late analysis tooling.
- Node env captured per cell; harness stdout in `~/torus-bench-scratch/bench-R*.log`.
