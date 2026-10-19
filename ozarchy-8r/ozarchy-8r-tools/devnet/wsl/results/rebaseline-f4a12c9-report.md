# Fixed-harness re-baseline — s470 Phase 0 (perf/funnel-truth)

**Date:** 2026-07-19 · **Box:** VPS 95.111.231.121 (8 cores / 23 GiB) · **Branch:** `perf/funnel-truth` @ **`f4a12c9`** (Phase 0 harness fixes: #32 node-counter headline, #34/#35 in-window body-sweep removed, #40/#41 env-gated RPC caps).
**Devnet:** 3 validators, bare-metal WSL scripts, **`CLEAN=1` fresh chain per cell** (each CSV opens at placed=matched=resting=0 → verified no state carryover), weighted 100k-account genesis, bulk senders funded at **100,000,000 TRS** native available (A4), 10 markets, 3 deterministic devnet validators.

**Node env (exact, captured from `/proc/<pid>/environ` per cell — identical across all 4 cells):**
```
TORUS_SHARD_CUSTODY=0            # perf gate (skip shard custody in execute_batch)
TORUS_RPC_MAX_RESPONSE_MB=64     # #40 — replaces the old silent 10 MiB response cap
TORUS_RPC_MAX_CONNS=1024         # #41 — was an unmeasured default
TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000
TORUS_NATIVE_TOTAL_BLOCK_CAP=100
```

**Load shape (all cells, mirrors 22c3cf0 exactly, only `--rate-total` varies):**
`bench-throughput consensus --rpc-urls http://127.0.0.1:8645 --econ --senders 5000 --sender-offset 60 --markets 10 --batch-size 400 --submit-batch 1 --format bin --concurrency 256 --duration 300 --target-margin 1500 --cross-fraction 0.5 --cancel-fraction 0.05 --band 5 --rate-total <R>` **plus the mandatory NEW flag** `--metrics-urls http://127.0.0.1:9161/metrics,9162,9163` (node-counter headline scraper). `--sweep-bodies` intentionally OFF.

**Measurement:** #32 harness scrapes the 3 nodes' Prometheus counters at the timed-window start/end; the **HEADLINE** placed/s, matched/s deltas are ground truth (never load-gen x-batch). A parallel 1 Hz `scrape.sh` on val0 (:9161) writes `rebaseline-cell*.csv` for worst-60s / best-60s via sliding-window `win60.awk`. Idle re-measured (20 s block-height delta) before each cell.

## Headline

> **The fixed harness confirms and slightly *raises* the honest ceiling, and does NOT change the verdict: the chain cannot stay healthy under ANY offered econ load from 76k to 400k orders/s.** Sustained node-counter throughput is **flat at ~3.1k–4.1k placed orders/s and ~2.4k–3.2k matched fills/s** regardless of offered rate, with **zero rejections of any kind** (A5 margin-release intact) and the **block-rate health gate FAILing in every cell** (window-avg 0.2–0.3 blk/s; **worst-60s = 0.000 blk/s in all four** — there is always a >=60 s stretch with zero block advance). The wall is unchanged: `torus_exec_queue_depth` **pegs at its 66 cap in every cell**, block height freezes for tens of seconds (2.7–6.7 s/blk), RPC ingress backpressures to ~100–120 submitted actions/s vs 190–1000 offered. This is the valid pre-Package-B ground truth.

## Per-cell table (node-counter HEADLINE over the ~330 s timed window)

| cell | offered orders/s | rate-total | placed/s | matched/s | resting | rejects | window blk/s | worst-60s blk/s | idle blk/s | gate |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| cell750  | 300,000 | 750  | **3,136** | **2,483** | 624,864 | 0 | 0.17 (57 blk/334s) | 0.000 | 29.90 | **FAIL** |
| cell1000 | 400,000 | 1000 | **3,842** | **3,052** | 750,465 | 0 | 0.29 (94 blk/328s) | 0.000 | 28.75 | **FAIL** |
| cell375  | 150,000 | 375  | **4,067** | **3,231** | 794,726 | 0 | 0.19 (62 blk/329s) | 0.000 | 29.70 | **FAIL** |
| cell190  |  76,000 | 190  | **3,086** | **2,447** | 613,943 | 0 | 0.19 (63 blk/334s) | 0.000 | 14.30* | **FAIL** |

Block time (harness): cell750 6650 ms/blk · cell1000 2663 ms/blk · cell375 6108 ms/blk · cell190 5243 ms/blk. Peak `exec_queue_depth` = **66 (the cap) in every cell**. Best-60s (CSV, includes 90 s drain tail): cell750 9,920 placed / 7,853 matched; cell1000 8,975 / 7,153; cell375 8,123 / 6,465; cell190 12,847 / 10,255 — brief post-offer executor-drain bursts, not sustainable.

`*` **cell190 anomaly:** its pre-load idle measured only 14.3 blk/s (vs ~29–30 for the other three). Its fresh chain was clean (CSV opens at 0/0/0), but the box had not fully returned to idle health when cell190's warm-up ran — most plausibly residual CPU from cell375's node teardown/rocksdb flush overlapping the launch. The loaded headline (3,086/2,447) still sits squarely in the same band as every other cell, so the flat-ceiling conclusion is unaffected; only cell190's absolute idle reference is caveated.

## Old broken-harness baseline vs fixed harness (both are node-counter deltas)

| offered | 22c3cf0 (eed446b) placed/matched | f4a12c9 placed/matched | delta |
|---|---:|---:|---:|
| 300k (r750) | 3,006 / 2,392 | 3,136 / 2,483 | +4.3% / +3.8% |
| 150k (r375) | 3,529 / 2,800 | 4,067 / 3,231 | +15% / +15% |
| 76k  (r190) | 2,358 / 1,871 | 3,086 / 2,447 | +31% / +31% |

**Did the fixed harness move the measured ceiling?** Yes — modestly *upward*, most at lower offered rates. The counter-based headline was never the ~190x `included x batch` inflation (that was always a separate debug figure); the true mover is #34/#35 **removing the in-window full-block-body sweep**, which in the old harness fetched every block body from val0's RPC *during the timed window*, stealing val0 CPU from consensus/exec and depressing the counters. With that gone (and the 10 MiB -> 64 MiB response cap, #40, so the fullest blocks are no longer silently dropped), val0 spends its cycles executing and the honest ceiling reads slightly higher. **The verdict is unchanged:** ~2.5–3.2k matched/s, gate FAIL at every offered rate.

## Exec-wall picture — still holds

Fully intact. `torus_exec_queue_depth` pegs at 66 within seconds in every cell; the bounded queue backpressures the commit path (height freezes tens of seconds — e.g. cell750 stuck at #770 for the entire back half) and RPC ingress (submitted 100–120 actions/s vs 190–1000 offered, hence the ~1.1x dup-resubmit factor). Serial execution of 400-order econ `PlaceOrderBatch` actions (~5–15 actions/s ≈ 2–6k orders/s executed) is the bottleneck, same as 22c3cf0. Zero economic rejection — the offered→executed gap is pure ingress/execution backpressure.

## The single Phase-1 reference number

> **Base cell `--rate-total 750` (300,000 orders/s offered) → 2,483 matched fills/s (3,136 placed/s), node counters over a 334 s window, health gate FAIL (worst-60s 0.000 blk/s).** Every Package-B / Phase-1 delta is measured against this. Secondary reference: matched clusters 2.4–3.2k/s across the whole 76k–400k offered ladder — a flat exec-bound ceiling, not a rate-sensitive curve.

## Files
- `rebaseline-cell{750,1000,375,190}.csv` — 1 Hz val0 funnel counter scrapes (fresh chain each).
- `scrape.sh` (reused), `win60.awk` (sliding-60s analysis).
- Harness stdout preserved in the bench agent scratch (`~/torus-bench-scratch/bench-cell*.log`, `ladder.log`).
