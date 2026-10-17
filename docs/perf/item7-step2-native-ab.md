# Item 7 step 2: archy native-only A/B (bench queue Q7, campaigns ozarchy-q7m3 + ozarchy-q7cl, 2026-10-17)

**Throughput: no cost in either shape.** Paired B/A matched/s: mode 3 1.0082 [0.9646, 1.0538], Classic 0.9963
[0.9896, 1.0031]; both 95% CIs include 1.0 (pass). **save_books: Classic passes** (B - A -0.22 ms per native block
[-1.49, +1.05]); **mode 3 fails the 0.3 ms bound by its point estimate** (+0.52 ms [-0.92, +1.96]; 3 of 4 rounds
positive) with a CI too wide to decide either way. Per the spec a fail blocks the step 2 merge (follow-up: B16's
`saved_bbo` hit rate): **owner / 18c decision.** +0.52 ms is ~0.14% of the ~385 ms exec block. n = 4 ABBA per shape,
one host, 120 s cells.

Spec: `docs/plans/item7-step2-impl.md` "Archy native-only A/B" and `docs/perf/bench-queue.md` Items Q7. Question: do
step 2's best bid / ask rows (one write per dirty market per block) cost native throughput or save_books time?

## 1. Setup

| | |
|---|---|
| Arm A | main at the step 2 merge base `db84a1ad`: torus-node md5 `35d9b0c0`, bench-throughput `ca9811fc`, tx-flood `1bd82be8` |
| Arm B | `feat/item7-step2` head `c567b1b3`: torus-node `8e941a60`, bench-throughput `7e771173`, tx-flood `6ba37902` |
| Build | release, `--locked`, mold + frame pointers + line tables (s64 recipe); worktrees `wt/q7-a-db84a1ad`, `wt/q7-b-c567b1b3`; staged `ozarchy-q7-stage/{a-run,b-run}`, md5s `ozarchy-q7-stage/build/md5s.txt` |
| Tools | frozen from main `f54b8839` (`ozarchy-q7-tools/FROZEN.txt`); `tools/bench-throughput` and `tools/tx-flood` identical across A, B and main |
| Genesis | N = 11 weighted-full, md5 `01168566`, one file for both shapes; no step 2 keys (`big_block_interval`, `big_gas_limit` absent: M = 0, big blocks off). B's smoke cells prove B loads it |
| Load | 300 markets, rate 76,000, native only (EVM load off), oracle feed off (harness default), 120 s cells |
| Shape 1, mode 3 | harness default `TORUS_BOOK_ROWS=3` (checkpoints off; the deferred save path writes the bbo rows) |
| Shape 2, Classic | `TORUS_BOOK_ROWS=` plus the template's other empty Classic knobs (its `envok` check), `TORUS_CHECKPOINT_INTERVAL_MS=15000` (the shipped validator shape; bbo puts go through `save_order_books`) |
| BLOCK_CAP | **400** in both shapes (owner s66: the spec's "harness defaults ... `BLOCK_CAP=200`" predates the s76 default of 400; recent benches use 400) |
| Order | per shape: smoke (B), warm-up w1 (A), then r1 A B, r2 B A, r3 A B, r4 B A |

Validity: both smokes and 9 / 9 cells per shape rc 0, ACCEPT AGREE PASS, md5 = the arm's staged binary on all 3 nodes,
100 CHECK OK lines (book mode from the node logs, genesis md5 and no step 2 keys, EVM off, save_books metric on all 3
nodes), 0 CHECK FAIL, 0 CELL-FLAG, 0 HARD FINDING, no reruns. Smokes (B, 60 s): mode 3 158.2k, Classic 142.5k matched/s.

Method: block time p50 / p99 from `torus_commit_interval_seconds` over the load window, mean of 3 nodes (p50 matches
`summary.json`); CPU-s/1M = node user+sys seconds per 1M fills over the load window, mean per node; save_books =
`torus_exec_save_books_seconds` per native block; paired ratios = geometric mean of per-round B/A with a t interval
on the log ratios (n = 4, 3 df); save_books = mean of per-round B - A with a t interval.

## 2. Shape 1, mode 3 (`ozarchy-q7m3`)

| cell | arm | rc | agree | md5_ok | flag | matched/s | native blk/s | save_books ms/nblk | exec block_ms | block p50/p99 ms | CPU-s/1M | timeouts | no_proposal |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| w1 | A | 0 | AGREE/PASS | yes | ok | 149,404.6 | 1.798 | 26.14 | 415.2 | 315 / 1,763 | 42.56 | 1.0 | 0 |
| r1 | A | 0 | AGREE/PASS | yes | ok | 151,133.7 | 1.878 | 24.00 | 383.1 | 311 / 1,821 | 42.81 | 0.0 | 0 |
| r1 | B | 0 | AGREE/PASS | yes | ok | 156,787.1 | 1.878 | 24.70 | 390.7 | 319 / 1,594 | 41.72 | 0.0 | 0 |
| r2 | B | 0 | AGREE/PASS | yes | ok | 156,631.7 | 1.960 | 24.36 | 383.9 | 320 / 1,699 | 41.79 | 0.0 | 0 |
| r2 | A | 0 | AGREE/PASS | yes | ok | 156,884.5 | 1.927 | 25.12 | 393.6 | 320 / 1,834 | 42.00 | 0.0 | 0 |
| r3 | A | 0 | AGREE/PASS | yes | ok | 155,593.8 | 1.943 | 23.85 | 388.2 | 314 / 1,786 | 42.27 | 0.0 | 0 |
| r3 | B | 0 | AGREE/PASS | yes | ok | 159,304.2 | 1.967 | 24.62 | 379.7 | 316 / 1,799 | 41.01 | 0.0 | 0 |
| r4 | B | 0 | AGREE/PASS | yes | ok | 154,048.7 | 1.887 | 24.78 | 388.3 | 315 / 1,760 | 42.05 | 1.0 | 0 |
| r4 | A | 0 | AGREE/PASS | yes | ok | 158,104.5 | 1.951 | 23.41 | 373.4 | 314 / 1,776 | 41.43 | 0.0 | 0 |

    arm A mean (n=4): matched/s 155,429  native blk/s 1.925  save_books 24.09  block_ms 384.6  p50/p99 315/1804  CPU-s/1M 42.13
    arm B mean (n=4): matched/s 156,693  native blk/s 1.923  save_books 24.62  block_ms 385.7  p50/p99 317/1713  CPU-s/1M 41.64
    paired B/A matched/s: per round 1.0374 0.9984 1.0238 0.9743  geomean 1.0082 95% CI [0.9646, 1.0538]
    paired B/A native blk/s: per round 1.0000 1.0171 1.0124 0.9672  geomean 0.9990 95% CI [0.9635, 1.0357]
    paired B/A exec block_ms: per round 1.0200 0.9755 0.9781 1.0399  geomean 1.0030 95% CI [0.9539, 1.0546]
    paired B/A CPU-s/1M: per round 0.9747 0.9951 0.9700 1.0151  geomean 0.9885 95% CI [0.9563, 1.0218]
    paired B/A block p50: per round 1.0238 0.9990 1.0047 1.0028  geomean 1.0075 95% CI [0.9902, 1.0252]
    paired B/A block p99: per round 0.8754 0.9267 1.0074 0.9912  geomean 0.9487 95% CI [0.8560, 1.0515]
    paired B-A save_books ms/nblk: per round +0.70 -0.76 +0.77 +1.38  mean +0.522 95% CI [-0.918, +1.963]

Arm A rises in run order (151.1k in r1 to 158.1k in r4) while B stays flat (154.0-159.3k); pairing by round absorbs
the drift (r1 B/A 1.037, r4 0.974). save_books per round B - A: +0.70, -0.76, +0.77, +1.38 ms.

## 3. Shape 2, Classic (`ozarchy-q7cl`)

| cell | arm | rc | agree | md5_ok | flag | matched/s | native blk/s | save_books ms/nblk | exec block_ms | block p50/p99 ms | CPU-s/1M | timeouts | no_proposal |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| w1 | A | 0 | AGREE/PASS | yes | ok | 140,075.1 | 1.805 | 37.43 | 377.9 | 336 / 1,831 | 41.58 | 3.0 | 0 |
| r1 | A | 0 | AGREE/PASS | yes | ok | 137,151.4 | 1.789 | 37.30 | 375.4 | 331 / 1,928 | 42.93 | 1.0 | 0 |
| r1 | B | 0 | AGREE/PASS | yes | ok | 136,004.8 | 1.764 | 36.50 | 375.6 | 320 / 1,905 | 42.23 | 0.0 | 0 |
| r2 | B | 0 | AGREE/PASS | yes | ok | 140,175.8 | 1.844 | 35.63 | 375.8 | 299 / 1,951 | 42.45 | 3.0 | 0 |
| r2 | A | 0 | AGREE/PASS | yes | ok | 140,555.7 | 1.836 | 36.10 | 374.0 | 304 / 1,899 | 42.32 | 0.0 | 0 |
| r3 | A | 0 | AGREE/PASS | yes | ok | 141,002.6 | 1.870 | 36.32 | 381.9 | 310 / 1,741 | 41.62 | 1.0 | 0 |
| r3 | B | 0 | AGREE/PASS | yes | ok | 140,258.5 | 1.837 | 37.28 | 384.4 | 308 / 1,897 | 42.08 | 3.0 | 0 |
| r4 | B | 0 | AGREE/PASS | yes | ok | 143,063.5 | 1.894 | 35.25 | 373.5 | 314 / 1,856 | 41.73 | 2.0 | 0 |
| r4 | A | 0 | AGREE/PASS | yes | ok | 142,817.0 | 1.852 | 35.82 | 380.9 | 298 / 1,907 | 41.55 | 2.0 | 0 |

    arm A mean (n=4): matched/s 140,382  native blk/s 1.837  save_books 36.39  block_ms 378.0  p50/p99 311/1869  CPU-s/1M 42.11
    arm B mean (n=4): matched/s 139,876  native blk/s 1.835  save_books 36.17  block_ms 377.3  p50/p99 310/1902  CPU-s/1M 42.12
    paired B/A matched/s: per round 0.9916 0.9973 0.9947 1.0017  geomean 0.9963 95% CI [0.9896, 1.0031]
    paired B/A native blk/s: per round 0.9860 1.0044 0.9824 1.0227  geomean 0.9987 95% CI [0.9697, 1.0286]
    paired B/A exec block_ms: per round 1.0007 1.0047 1.0067 0.9805  geomean 0.9981 95% CI [0.9791, 1.0175]
    paired B/A CPU-s/1M: per round 0.9836 1.0031 1.0110 1.0043  geomean 1.0004 95% CI [0.9818, 1.0194]
    paired B/A block p50: per round 0.9671 0.9825 0.9937 1.0513  geomean 0.9982 95% CI [0.9421, 1.0576]
    paired B/A block p99: per round 0.9879 1.0271 1.0895 0.9736  geomean 1.0185 95% CI [0.9403, 1.1033]
    paired B-A save_books ms/nblk: per round -0.80 -0.47 +0.96 -0.57  mean -0.221 95% CI [-1.492, +1.051]

Timeouts 0-3 per cell on both arms (A 1-3 incl. w1, B 0-3); no_proposal 0 in every cell.

## 4. Verdict against the spec's rule

| shape | B/A matched/s 95% CI includes 1.0 or above | save_books rise <= 0.3 ms per native block | result |
|---|---|---|---|
| mode 3 | yes: 1.0082 [0.9646, 1.0538] | **no by point estimate**: +0.52 [-0.92, +1.96] | throughput pass; save_books FAIL (unresolved by the CI) |
| Classic | yes: 0.9963 [0.9896, 1.0031] | yes: -0.22 [-1.49, +1.05] | pass |

Every other paired metric's CI includes 1.0 in both shapes (native blk/s, exec block_ms, CPU-s/1M, block p50 / p99).

Open for the owner / 18c: accept mode 3's +0.52 ms (Classic is the shipped shape and passes; the effect is ~0.14% of
the exec block and not resolved at n = 4), or run the B16 follow-up (`saved_bbo` hit rate) and / or more mode 3
rounds before the merge.

## 5. Data

Cells `~/bench-results-matched/ozarchy-q7{m3,cl}-300m-{smoke-s0,w1,r1-A,r1-B,r2-B,r2-A,r3-A,r3-B,r4-B,r4-A}`; campaign
files `ozarchy-q7-run/{m3,cl}/` (`campaign.log`, `cells.tsv`, `smoke/`); tools `ozarchy-q7-tools/` (`q7m3-spec.json`,
`q7cl-spec.json`, `ozarchy-q7{m3,cl}-campaign.sh`, `q7-checks.sh`, `FROZEN.txt`); analysis
`ozarchy-q7-run/analyst/` (`q7-analyse.py`, `m3.txt`, `cl.txt`).
