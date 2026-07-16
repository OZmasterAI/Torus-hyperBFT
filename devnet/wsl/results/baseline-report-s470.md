# Honest re-baseline post A4/A5 — s470 (perf/funnel-truth)

**Date:** 2026-07-16 · **Box:** VPS 95.111.231.121 (8 cores / 24 GB) · **Branch:** `perf/funnel-truth` @ `eed446b` (includes A5 `98c76c6`, A4 `37a0958`, 100M bulk `944fa78`)
**Devnet:** 3 validators, bare-metal WSL scripts, `CLEAN=1` fresh genesis per cell, weighted 100k-account genesis, bulk senders verified at **100,000,000 TRS native available**.
**Load shape (all cells):** `bench-throughput consensus --econ --senders 5000 --sender-offset 60 --markets 10 --batch-size 400 --submit-batch 1 --format bin --concurrency 256 --duration 300 --target-margin 1500 --cross-fraction 0.5 --cancel-fraction 0.05 --band 5`, RPC = val0 only (127.0.0.1:8645). Only `--rate-total` varies.
**Measurement:** 1 Hz Prometheus counter scrape on val0 (:9161) → `baseline-cell{1..4}.csv`. All rates below are **counter deltas** (never the loadgen's orders/s line). Idle canonical baseline: **29.78 blk/s**; re-measured idle before each cell: 28.5–29.6 blk/s. Health gate: loaded ≥ 23.8 blk/s AND zero "Send Queue full".

## Headline

> **Post-A4/A5 the funnel no longer rejects anything — 0 margin/book/other rejections in ~6.0M executed orders across all 4 cells (A5 margin-release works) — but the chain cannot stay healthy under ANY offered econ load from 3.2k to 300k orders/s.** Honest sustained figures: **~1.7k–3.5k placed orders/s, ~1.4k–2.8k matched fills/s over 300 s windows at < 1 blk/s worst-60s block rate** (gate: 23.8). The wall is the execution pipeline: ~5–10 batch-actions/s (≈ 2–4k orders/s) per the 400-order econ action, `torus_exec_queue_depth` pegged at its 66 cap in every cell, backpressuring RPC ingress and starving consensus commit. This is the valid "before Package B" ground truth.

## Per-cell funnel tables (counter deltas over the full loaded+drain window)

### Cell 1 — `--rate-total 750` (300,000 orders/s offered)

| metric | delta | rate/s |
|---|---:|---:|
| actions_processed | 4,805 | 9.5 |
| placed_accepted | 1,518,400 | **3,006.7** |
| matched | 1,207,982 | **2,392.0** |
| resting | 902,915 | 1,788.0 |
| rejected (all reasons) | 0 | 0.0 |
| self_trade_cancels / cancelled_partial_fill | 0 | 0.0 |
| blocks_committed | 55 | **0.11** |

Window 505 s. Best 60 s: matched 7,910/s, placed 9,900/s, blk 0.32/s. Loadgen: submitted 123 actions/s (offered 750 — RPC backpressure), dup ×1.08, 2,089 ms/blk, height frozen at #5448 for the final ~60 s. **Health: FAIL** (blk/s ≪ 23.8; Send-Queue-full = 0 on all 3 vals).

### Cell 2 — `--rate-total 375` (150,000 orders/s offered)

| metric | delta | rate/s |
|---|---:|---:|
| actions_processed | 4,342 | 10.2 |
| placed_accepted | 1,503,600 | **3,529.6** |
| matched | 1,192,844 | **2,800.1** |
| resting | 894,594 | 2,100.0 |
| rejected (all reasons) | 0 | 0.0 |
| blocks_committed | 67 | **0.16** |

Window 426 s. Best 60 s: matched 10,738/s, placed 13,522/s, blk 0.42/s. Loadgen: submitted 76 actions/s, dup ×1.10, 2,976 ms/blk. **Health: FAIL.** Mid-run CPU: bench-throughput 255%, box ~99% busy (caveat: another team's cargo test compiling concurrently).

### Cell 3 — `--rate-total 190` (76,000 orders/s offered)

| metric | delta | rate/s |
|---|---:|---:|
| actions_processed | 3,986 | 7.8 |
| placed_accepted | 1,212,000 | **2,358.0** |
| matched | 961,863 | **1,871.3** |
| resting | 721,529 | 1,403.8 |
| rejected (all reasons) | 0 | 0.0 |
| blocks_committed | 54 | **0.11** |

Window 514 s. Best 60 s: matched 8,173/s, placed 10,333/s, blk 0.38/s. Loadgen: submitted 134 actions/s, dup ×1.10, 498 ms/blk avg. **Health: FAIL.** Mid-run CPU: **torus-node val0 415%, val2 368%** — the nodes themselves saturate the box (other-team compile also present).

### Cell 4 — `--rate-total 8` (3,200 orders/s offered; box fully quiet)

| metric | delta | rate/s |
|---|---:|---:|
| actions_processed | 4,771 | 4.6 |
| placed_accepted | 1,816,400 | **1,738.2** |
| matched | 1,447,539 | **1,385.2** |
| resting | 1,078,833 | 1,032.4 |
| rejected (all reasons) | 0 | 0.0 |
| blocks_committed | 5,999 | 5.7 avg |

Window 1,045 s (long executor drain past the 300 s offered window). Worst 60 s: **0.37 blk/s**; best 60 s: matched 3,016/s, placed 3,807/s, blk 0.77/s. Loadgen: submitted at full 8 actions/s (no ingress backpressure), included drop 0.5%, dup ×1.17. Mid-run CPU: torus-nodes 215/200/184%, **kswapd0 53%** (nodes ~2 GB RSS each, swap active — >1M resting orders of book state). **Health: FAIL** even at 1% of Cell-1's offered load.

## Health verdicts

| cell | offered orders/s | placed/s | matched/s | blk/s (loaded) | Send-Queue-full | verdict |
|---|---:|---:|---:|---:|---:|---|
| 1 | 300,000 | 3,007 | 2,392 | 0.11 (best60 0.32) | 0 | FAIL |
| 2 | 150,000 | 3,530 | 2,800 | 0.16 (best60 0.42) | 0 | FAIL |
| 3 | 76,000 | 2,358 | 1,871 | 0.11 (best60 0.38) | 0 | FAIL |
| 4 | 3,200 | 1,738 | 1,385 | 5.7 avg (worst60 0.37) | 0 | FAIL |

Acceptance ratio of **executed** orders: 100% in every cell (zero rejections of any kind — A5 margin release + 100M funding fully removed the Phase-2 margin pre-reserve collapse documented in A3). The offered→executed gap is pure ingress/execution backpressure, not economic rejection.

## CPU attribution

- Under saturating load (Cell 3): torus-node processes at ~415% + ~368% + (val1 similar) — the 3 nodes alone consume nearly all 8 cores.
- At paced load (Cell 4, quiet box): nodes at 215/200/184% with kswapd0 at 53% — execution + state growth (swap pressure at ~2 GB RSS/node) dominate even at 3.2k orders/s.
- Loadgen (bench-throughput) peaked at ~255% only when trying to push 375–750 actions/s through a backpressured RPC; at 8/s it used ~15%. The loadgen is NOT the bottleneck at the rates that matter.
- Caveat: cells 2–3 ran with a concurrent third-party rocksdb/cargo build on the box (cell 1 mostly clean, cell 4 fully clean). The cross-cell consistency (executed ceiling ~5–10 actions/s in all cells) shows contention was not the primary cause.

## Top observed bottleneck (before Package B)

**Serial execution of large PlaceOrderBatch actions.** A 400-order econ action takes ~100–200 ms to execute (~5–10 actions/s ≈ 2–4k orders/s). `torus_exec_queue_depth` saturates at its 66-entry cap within seconds at every offered rate; the bounded queue backpressures the commit path (block height freezes for tens of seconds — e.g. #5448 in Cell 1) and RPC ingress (submitted 76–134 actions/s vs 190–750 offered), producing the ~1.10 duplicate-resubmission factor and the "skipping duplicate/replayed native action" WARN spam. Secondary: memory pressure from unbounded resting-order state (swap + kswapd activity by end of each cell).

**Implication:** the 250k–400k matched/s mission target needs Package B to attack per-order execution cost and the exec-queue/commit coupling; offered-rate shaping alone cannot reach a healthy operating point above ~1.4k matched/s on this 8-core box.

## Files

- `devnet/wsl/results/baseline-cell{1,2,3,4}.csv` — 1 Hz funnel counter scrapes (val0)
- `devnet/wsl/results/scrape.sh`, `cpusample.sh` — measurement tooling
- `devnet/wsl/results/cpu-cell{2,3,4}.txt` — mid-run `top` snapshots
