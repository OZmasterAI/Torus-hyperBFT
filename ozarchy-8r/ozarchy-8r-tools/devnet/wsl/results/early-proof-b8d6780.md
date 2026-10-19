# EARLY PROOF — mission go/no-go bench (perf/early-proof @ b8d6780)

**Date:** 2026-07-19 · **Box:** VPS 95.111.231.121 (8 cores / 23 GiB) · **Branch:** `perf/early-proof` @ **`b8d6780`** (= funnel-truth@`7e7a8f2` + Package C `e62b1cc` + Package D `b8d6780`, merged clean).
**Reference:** the f4a12c9 re-baseline cell750 = **3,136 placed/s · 2,483 matched/s**, gate FAIL 0.000 blk/s. The re-baseline's own four cells spanned **2,483–3,231 matched/s** (flat exec-bound ceiling, box run-to-run variance) — that band is the yardstick, not the single low point.
**Method:** fresh chain per cell (`CLEAN=1`, every CSV opens 0/0/0), ~330 s window, load shape identical to re-baseline (`--senders 5000 --markets 10 --batch-size 400 --econ --rate-total 750`, `--metrics-urls` all 3 nodes, no `--sweep-bodies`), node-counter HEADLINE = ground truth, parallel 1 Hz val0 scrape for worst-60s. Each cell flock-held for its whole window, torn down at cell end (ports freed).

## Verdict

> **NO-GO on the headline metric at these settings.** Neither Package C nor Package C+D lifts sustained matched throughput out of the flat exec-bound ceiling (~2.5–3.0k matched/s), and **the block-rate health gate FAILs in every cell** (worst-60s = 0.000 blk/s — a >=60 s zero-advance freeze persists in all four runs). The exec wall is **not** broken. Package D does measurably improve *average* block cadence (block time 6.7 s -> 2.7 s, more blocks committed) and eliminates nothing of the periodic long freezes that fail the gate — it reshapes block timing without raising the executed-work ceiling. Package C alone is neutral-to-slightly-negative (per-order-row book persistence adds write cost, block time 6.7 s -> 9.5 s).

## Cells (node-counter HEADLINE over the ~330 s timed window)

| cell | config (on top of re-baseline env) | placed/s | matched/s | vs 2,483 ref | blk time | worst-60s blk/s | idle blk/s | gate |
|---|---|---:|---:|---:|---:|---:|---:|---|
| **A** merge-sanity | all C/D flags OFF | **3,800** | **3,024** | +21.8% | 6743 ms | 0.000 | 30.30 | FAIL |
| **B** +C | PARALLEL_SETTLE=1, BOOK_ROWS=1, PARALLEL_SETTLE_MIN_FILLS=64 | **3,181** | **2,517** | +1.4% | 9511 ms | 0.000 | 28.80 | FAIL |
| **C** +C+D | B + NATIVE_TOTAL_BLOCK_CAP=400, VERIFIED_SENDER_CACHE_CAP=25600, EXEC_THROTTLE_WATERMARKS=16,32,48, EXEC_NONBLOCKING_DISPATCH=1 | 2,743 | 2,180 | -12.2% | 7549 ms | 0.000 | **9.85** * | FAIL |
| **C2** +C+D (clean re-run) | identical to C | **3,372** | **2,678** | +7.9% | **2708 ms** | 0.000 | 30.05 | FAIL |

Zero rejections of any kind in every cell (rej-margin/book/cancelled/partial/self-trade/other all 0 — A5 margin-release intact). Peak `exec_queue_depth` = 64–66 (saturated) in every cell, **including the D cells** (65) — nonblocking dispatch moves overflow into `deferred_exec` but the exec channel still pegs.

`*` **Cell C anomaly (why C2 was run):** C's pre-load idle measured 9.85 blk/s (vs ~29–30 elsewhere). Watermarks/nonblocking cannot degrade *idle* block production (backlog≈0 → tier 0 → no throttle), and no external heavy process was on the box (only idle surrealdb + idle sccache), so C's chain was transiently unhealthy from launch — its loaded number is depressed by that, not by D. During C the matched counter flatlined at 245,634 for ~190 s then burst: with `exec_backlog` pegged at ~65 and watermarks `16,32,48`, the proposer sat in permanent tier-3 (cancels-only, backlog>=48), starving new-order flow. **C2 (clean idle 30.05) is the representative Package-D result: 2,678 matched/s.**

## Honest read — did C move the exec wall? Did D?

**Package C (parallel settle + per-order-row book persistence): no.** Matched 2,517/s (Cell B) sits mid-band; block time got *worse* (9,511 ms vs Cell A's 6,743 ms), consistent with `TORUS_BOOK_ROWS=1` adding per-row write cost. C touches settlement parallelism and book persistence, not the single-HotStuff-thread exec dispatch that is the wall — so unchanged, as expected.

**Package D (raised block cap + trust-cache + rank1 watermark pacing + rank2 nonblocking dispatch): partially — it changes block *shape*, not the *ceiling*.**
- **Block shape DID change.** With `NATIVE_TOTAL_BLOCK_CAP=400` + pacing + nonblocking dispatch, Cell C2's average block time fell to **2,708 ms** (from ~6,743 ms at cap 100) and more blocks committed (55 vs 43–64 counter, 133 vs 102 height-delta over the CSV span). Rank2's nonblocking dispatch removes the hard blocking-send stall (the old "0 blk/s consensus-freeze" mode); rank1 pacing scales proposer caps down as backlog deepens. Together they keep QCs forming more steadily *on average*.
- **The ceiling did NOT move.** Matched stayed **2,678/s** (C2) — inside the same 2.5–3.2k band as the re-baseline and Cell A. The exec engine's serial execution of 400-order `PlaceOrderBatch` actions is still the bottleneck; pacing/nonblocking reshape *when* blocks are thin vs full but do not increase total executed work per second.
- **The gate still FAILs.** worst-60s = 0.000 blk/s in C and C2 both — periodic multi-second freezes still produce a full zero-advance minute, so the >=23.8 blk/s health gate is missed at 300k offered exactly as before.

**Does the gate behave differently under D?** Only in average cadence (2.7 s/blk vs 6.7 s/blk), not in the pass/fail outcome or the worst-case tail — the freezes that fail the gate persist.

## Caveats / unexplored
- **Watermark tuning is unexplored.** `16,32,48` (the code's illustrative example) forces permanent tier-3 (cancels-only) once `exec_backlog` (=`exec_queue_len` + `deferred_exec.len`) exceeds 48, which under nonblocking dispatch it does within seconds and stays there (deferred grows). A higher/wider set, or a bounded `deferred_exec` policy, could change D's behavior materially — this is the obvious next experiment before a final D verdict.
- Single clean pass per config on a shared 8-core box; the ~2.5–3.4k placed / ~2.2–3.0k matched spread across A/B/C2 is within observed run-to-run variance. No config produced a statistically distinct improvement.

## Reference for downstream
Compared to the mission reference **2,483 matched/s** (re-baseline cell750): A +21.8%, B +1.4%, C2 (clean D) +7.9% — all within the re-baseline's own 2,483–3,231 band, all gate FAIL. **No config clears the health gate or breaks the ~3k matched/s exec ceiling at 300k orders/s offered.**

## Files
- `rebaseline-proof{A,B,C,C2}.csv` — 1 Hz val0 funnel scrapes (fresh chain each).
- Node env captured per cell (identical to re-baseline + the listed C/D flags); harness stdout in `~/torus-bench-scratch/`.
