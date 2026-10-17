# Item 7 step 2: archy native-only A/B (bench queue Q7)

Two runs. **Run 2 (standard load, campaigns ozarchy-q7m3s + ozarchy-q7cls, 2026-10-17) decides the merge.** Run 1
(ozarchy-q7m3 + ozarchy-q7cl) ran `run-cell.sh`'s bare load defaults and is kept below for the record only.

**Run 2: throughput passes in both shapes; save_books per native block fails the 0.3 ms bound by its point estimate in
both, CI unresolved, and per fill it is unchanged.** Paired B/A matched/s: mode 3 1.0264 [0.9497, 1.1093], Classic
1.0115 [0.9942, 1.0290]. save_books B - A per native block: mode 3 +0.54 ms [-0.60, +1.69], Classic +0.62 ms [-0.34,
+1.58]. B runs bigger native blocks (+7.5% / +7.8% fills per block), and save_books follows block size (r = 0.96 /
0.98 across cells, ~0.23-0.25 ms per 1k fills); per 1k fills B - A is +0.0033 ms [-0.0123, +0.0189] (mode 3) and
+0.0003 ms [-0.0074, +0.0081] (Classic), i.e. ~0. Reading: the per-block rise is B's bigger blocks, not the bbo writes
(INFERRED from the per-fill numbers). Under the spec's literal rule (save_books per native block <= +0.3 ms) this is
an **owner / 18c decision.** n = 4 ABBA per shape, one host, 120 s cells.

Spec: `docs/plans/item7-step2-impl.md` "Archy native-only A/B" and `docs/perf/bench-queue.md` Items Q7 + "Standard
load". Question: do step 2's best bid / ask rows (one write per dirty market per block) cost native throughput or
save_books time?

## Run 2: standard load (decides the merge)

### R2.1 Setup

Same binaries, genesis and frozen tools as run 1 (section R1.1 below): A = `db84a1ad` (torus-node `35d9b0c0`), B =
`c567b1b3` (`8e941a60`), tools main `f54b8839`, genesis `01168566` without the step 2 keys. Load = the standard load
(bench queue, s127): 300 markets, rate 76,000, 5,000 senders, band 5, cross 0.5, cancel 0.05, `BLOCK_CAP=400
RETRY_BUSY=1 MAX_IN_FLIGHT=4 OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 IN_FLIGHT_TAIL=commit CLOID_SHARE=0`, oracle
30000 / 2000 ms / walk 0 (Q8's values, = harness defaults), native only. Shape 1 mode 3 = that load +
`TORUS_BOOK_ROWS=3`; shape 2 Classic = that load, `TORUS_BOOK_ROWS=` (+ the template's empty Classic knobs),
`TORUS_CHECKPOINT_INTERVAL_MS=15000`. Order per shape: smoke (B), w1 (A), r1 A B, r2 B A, r3 A B, r4 B A.
`IN_FLIGHT_TAIL_DEPTH=1` was not set: neither the frozen harness nor either bench binary reads it (no
`--in-flight-tail-depth` flag); depth 1 is the only behaviour.

Validity: both smokes and 9 / 9 cells per shape rc 0, ACCEPT AGREE, liveness PASS, staged md5 on all 3 nodes; new load
check (`q7loadchk`: every cell's `cell=` line against the standard load) passed on every cell; 0 CHECK FAIL, 0
CELL-FLAG, 0 HARD FINDING, no reruns. Sanity: A's Classic cells 241.1k matched/s vs Q8's oracle-on cells 240.7k.

### R2.2 Mode 3 (`ozarchy-q7m3s`)

| cell | arm | rc | agree | md5_ok | flag | matched/s | native blk/s | fills/nblk | save_books ms/nblk | exec block_ms | block p50/p99 ms | CPU-s/1M | timeouts | no_proposal |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| w1 | A | 0 | AGREE/PASS | yes | ok | 231,136.7 | 5.944 | 31,473 | 5.47 | 128.7 | 60 / 1,005 | 28.03 | 0.0 | 0 |
| r1 | A | 0 | AGREE/PASS | yes | ok | 232,038.6 | 5.911 | 33,326 | 6.05 | 136.8 | 79 / 973 | 28.25 | 0.0 | 0 |
| r1 | B | 0 | AGREE/PASS | yes | ok | 232,948.6 | 5.476 | 35,255 | 6.35 | 144.2 | 73 / 1,008 | 27.97 | 0.0 | 0 |
| r2 | B | 0 | AGREE/PASS | yes | ok | 233,768.7 | 5.496 | 36,001 | 6.60 | 147.2 | 75 / 987 | 28.20 | 0.0 | 0 |
| r2 | A | 0 | AGREE/PASS | yes | ok | 211,866.0 | 4.672 | 30,079 | 5.03 | 131.4 | 72 / 1,747 | 27.11 | 9.0 | 1.0 |
| r3 | A | 0 | AGREE/PASS | yes | ok | 235,381.9 | 5.645 | 33,918 | 5.82 | 137.6 | 80 / 969 | 27.79 | 0.0 | 0 |
| r3 | B | 0 | AGREE/PASS | yes | ok | 233,483.2 | 5.593 | 35,173 | 6.20 | 143.5 | 73 / 1,089 | 28.03 | 0.0 | 0 |
| r4 | B | 0 | AGREE/PASS | yes | ok | 234,297.4 | 5.561 | 33,907 | 5.86 | 138.8 | 76 / 1,067 | 28.00 | 0.0 | 0 |
| r4 | A | 0 | AGREE/PASS | yes | ok | 231,966.3 | 5.886 | 33,188 | 5.96 | 135.7 | 67 / 1,039 | 28.20 | 0.0 | 0 |

| metric | arm A (n=4) | arm B (n=4) | B-A | B/A % |
|---|---|---|---|---|
| matched/s | 227,813 | 233,624 | +5,811 | +2.55% |
| native blk/s | 5.528 | 5.532 | +0.003 | +0.05% |
| fills per native block | 32,628 | 35,084 | +2,456 | +7.53% |
| save_books ms per native block | 5.71 | 6.25 | +0.54 | +9.50% |
| exec block_ms | 135.4 | 143.4 | +8.1 | +5.96% |
| block time p50 ms | 74 | 74 | -0 | -0.27% |
| block time p99 ms | 1,182 | 1,038 | -144 | -12.20% |
| CPU-s/1M fills | 27.83 | 28.05 | +0.21 | +0.77% |
| timeouts total | 9.0 | 0.0 | -9.0 | -100.00% |
| no_proposal total | 1 | 0 | -1 | -100.00% |

    paired B/A matched/s: per round 1.0039 1.1034 0.9919 1.0100  geomean 1.0264 95% CI [0.9497, 1.1093]
    paired B/A native blk/s: per round 0.9264 1.1764 0.9908 0.9448  geomean 1.0050 95% CI [0.8454, 1.1947]
    paired B/A fills/nblk: per round 1.0579 1.1969 1.0370 1.0216  geomean 1.0762 95% CI [0.9593, 1.2074]
    paired B/A exec block_ms: per round 1.0547 1.1199 1.0427 1.0233  geomean 1.0595 95% CI [0.9959, 1.1273]
    paired B/A CPU-s/1M: per round 0.9902 1.0401 1.0088 0.9931  geomean 1.0078 95% CI [0.9724, 1.0446]
    paired B/A block p50: per round 0.9218 1.0423 0.9203 1.1302  geomean 0.9998 95% CI [0.8522, 1.1730]
    paired B/A block p99: per round 1.0361 0.5650 1.1234 1.0275  geomean 0.9067 95% CI [0.5468, 1.5032]
    paired B-A save_books ms/nblk: per round +0.30 +1.58 +0.38 -0.09  mean +0.542 95% CI [-0.603, +1.688]

r2-A is an outlier: 211.9k matched/s with 9 view timeouts and 1 no_proposal (its best 60 s was a normal 241.9k; host
clean). Without round 2: B/A ~1.002, save_books B - A ~+0.20 ms.

### R2.3 Classic (`ozarchy-q7cls`)

| cell | arm | rc | agree | md5_ok | flag | matched/s | native blk/s | fills/nblk | save_books ms/nblk | exec block_ms | block p50/p99 ms | CPU-s/1M | timeouts | no_proposal |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| w1 | A | 0 | AGREE/PASS | yes | ok | 247,246.1 | 5.885 | 35,482 | 7.48 | 140.2 | 64 / 999 | 23.95 | 0.0 | 0 |
| r1 | A | 0 | AGREE/PASS | yes | ok | 241,234.5 | 5.667 | 39,080 | 8.37 | 156.2 | 72 / 1,111 | 24.36 | 1.0 | 0 |
| r1 | B | 0 | AGREE/PASS | yes | ok | 246,440.9 | 5.541 | 40,304 | 8.50 | 158.9 | 75 / 1,091 | 24.10 | 0.0 | 0 |
| r2 | B | 0 | AGREE/PASS | yes | ok | 243,600.6 | 5.846 | 36,624 | 7.77 | 144.9 | 60 / 1,031 | 24.18 | 0.0 | 0 |
| r2 | A | 0 | AGREE/PASS | yes | ok | 240,325.1 | 5.878 | 36,635 | 7.64 | 146.6 | 60 / 1,004 | 24.35 | 0.0 | 0 |
| r3 | A | 0 | AGREE/PASS | yes | ok | 241,973.6 | 5.967 | 35,350 | 7.34 | 141.4 | 62 / 1,028 | 24.31 | 0.0 | 0 |
| r3 | B | 0 | AGREE/PASS | yes | ok | 241,005.1 | 5.569 | 40,783 | 8.69 | 163.0 | 66 / 993 | 24.30 | 0.0 | 0 |
| r4 | B | 0 | AGREE/PASS | yes | ok | 244,412.0 | 5.410 | 41,567 | 8.78 | 165.3 | 69 / 1,097 | 24.27 | 0.0 | 0 |
| r4 | A | 0 | AGREE/PASS | yes | ok | 240,850.7 | 5.803 | 36,662 | 7.90 | 147.7 | 58 / 1,087 | 24.34 | 0.0 | 0 |

| metric | arm A (n=4) | arm B (n=4) | B-A | B/A % |
|---|---|---|---|---|
| matched/s | 241,096 | 243,865 | +2,769 | +1.15% |
| native blk/s | 5.829 | 5.591 | -0.237 | -4.07% |
| fills per native block | 36,932 | 39,820 | +2,888 | +7.82% |
| save_books ms per native block | 7.81 | 8.43 | +0.62 | +7.95% |
| exec block_ms | 148.0 | 158.0 | +10.0 | +6.77% |
| block time p50 ms | 63 | 68 | +5 | +7.65% |
| block time p99 ms | 1,057 | 1,053 | -4 | -0.40% |
| CPU-s/1M fills | 24.34 | 24.21 | -0.13 | -0.53% |
| timeouts total | 1.0 | 0.0 | -1.0 | -100.00% |
| no_proposal total | 0 | 0 | +0 | n/a |

    paired B/A matched/s: per round 1.0216 1.0136 0.9960 1.0148  geomean 1.0115 95% CI [0.9942, 1.0290]
    paired B/A native blk/s: per round 0.9778 0.9946 0.9333 0.9323  geomean 0.9591 95% CI [0.9102, 1.0105]
    paired B/A fills/nblk: per round 1.0313 0.9997 1.1537 1.1338  geomean 1.0776 95% CI [0.9637, 1.2050]
    paired B/A exec block_ms: per round 1.0172 0.9882 1.1522 1.1193  geomean 1.0670 95% CI [0.9487, 1.2001]
    paired B/A CPU-s/1M: per round 0.9891 0.9930 0.9996 0.9972  geomean 0.9947 95% CI [0.9874, 1.0021]
    paired B/A block p50: per round 1.0421 1.0116 1.0724 1.1908  geomean 1.0772 95% CI [0.9621, 1.2060]
    paired B/A block p99: per round 0.9823 1.0268 0.9659 1.0099  geomean 0.9959 95% CI [0.9535, 1.0403]
    paired B-A save_books ms/nblk: per round +0.12 +0.13 +1.35 +0.88  mean +0.621 95% CI [-0.339, +1.581]

In Classic r2, where A and B had the same block size (36.6k fills), save_books B - A was +0.13 ms.

### R2.4 save_books per fill (`sb-per-fill.txt`)

| shape | save_books ms per 1k fills A / B | paired B - A per 1k fills [95% CI] | save_books vs fills per block across 8 cells |
|---|---|---|---|
| mode 3 | 0.1749 / 0.1782 | +0.0033 [-0.0123, +0.0189] | slope 0.245 ms per 1k fills, r = 0.958 |
| Classic | 0.2115 / 0.2118 | +0.0003 [-0.0074, +0.0081] | slope 0.223 ms per 1k fills, r = 0.984 |

### R2.5 Verdict against the spec's rule

| shape | B/A matched/s 95% CI includes 1.0 or above | save_books rise <= 0.3 ms per native block | result |
|---|---|---|---|
| mode 3 | yes: 1.0264 [0.9497, 1.1093] | **no by point estimate**: +0.54 [-0.60, +1.69] | throughput pass; save_books FAIL (CI unresolved); per fill ~0 |
| Classic | yes: 1.0115 [0.9942, 1.0290] | **no by point estimate**: +0.62 [-0.34, +1.58] | throughput pass; save_books FAIL (CI unresolved); per fill ~0 |

Open for the owner / 18c: the rule is per native block, and B's blocks are ~7.7% bigger; per fill save_books is
unchanged in both shapes. Options: accept on the per-fill evidence, or run B16's `saved_bbo` hit rate first.

### R2.6 Data

Cells `~/bench-results-matched/ozarchy-q7{m3s,cls}-300m-{smoke-s0,w1,r1-A,r1-B,r2-B,r2-A,r3-A,r3-B,r4-B,r4-A}`;
campaign files `ozarchy-q7s-run/{m3,cl}/`; tools `ozarchy-q7s-tools/` (specs, drivers, checks with `q7loadchk`);
analysis `ozarchy-q7s-run/analyst/` (`q7s-analyse.py`, `m3.txt`, `cl.txt`, `sb-per-fill.txt`).

## Run 1: bare load defaults (s66, superseded; does not decide the merge)

Ran `run-cell.sh`'s bare load defaults because the spec said "harness defaults": MAX_IN_FLIGHT, IN_FLIGHT_TAIL,
OPEN_ORDER_BUDGET, RETRY_BUSY, CLOID_SHARE unset, oracle off. ~140-156k matched/s instead of ~240k, deeper books, ~76k
fills per native block. The A/B was fair (same load, interleaved) but not at the load every other bench uses.

**Throughput: no cost in either shape.** Paired B/A matched/s: mode 3 1.0082 [0.9646, 1.0538], Classic 0.9963
[0.9896, 1.0031]; both 95% CIs include 1.0 (pass). **save_books: Classic passes** (B - A -0.22 ms per native block
[-1.49, +1.05]); **mode 3 fails the 0.3 ms bound by its point estimate** (+0.52 ms [-0.92, +1.96]; 3 of 4 rounds
positive) with a CI too wide to decide either way. Per the spec a fail blocks the step 2 merge (follow-up: B16's
`saved_bbo` hit rate): **owner / 18c decision.** +0.52 ms is ~0.14% of the ~385 ms exec block. n = 4 ABBA per shape,
one host, 120 s cells.

Spec: `docs/plans/item7-step2-impl.md` "Archy native-only A/B" and `docs/perf/bench-queue.md` Items Q7. Question: do
step 2's best bid / ask rows (one write per dirty market per block) cost native throughput or save_books time?

### R1.1 Setup

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

### R1.2 Shape 1, mode 3 (`ozarchy-q7m3`)

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

### R1.3 Shape 2, Classic (`ozarchy-q7cl`)

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

### R1.4 Verdict against the spec's rule

| shape | B/A matched/s 95% CI includes 1.0 or above | save_books rise <= 0.3 ms per native block | result |
|---|---|---|---|
| mode 3 | yes: 1.0082 [0.9646, 1.0538] | **no by point estimate**: +0.52 [-0.92, +1.96] | throughput pass; save_books FAIL (unresolved by the CI) |
| Classic | yes: 0.9963 [0.9896, 1.0031] | yes: -0.22 [-1.49, +1.05] | pass |

Every other paired metric's CI includes 1.0 in both shapes (native blk/s, exec block_ms, CPU-s/1M, block p50 / p99).

Open for the owner / 18c: accept mode 3's +0.52 ms (Classic is the shipped shape and passes; the effect is ~0.14% of
the exec block and not resolved at n = 4), or run the B16 follow-up (`saved_bbo` hit rate) and / or more mode 3
rounds before the merge.

### R1.5 Data

Cells `~/bench-results-matched/ozarchy-q7{m3,cl}-300m-{smoke-s0,w1,r1-A,r1-B,r2-B,r2-A,r3-A,r3-B,r4-B,r4-A}`; campaign
files `ozarchy-q7-run/{m3,cl}/` (`campaign.log`, `cells.tsv`, `smoke/`); tools `ozarchy-q7-tools/` (`q7m3-spec.json`,
`q7cl-spec.json`, `ozarchy-q7{m3,cl}-campaign.sh`, `q7-checks.sh`, `FROZEN.txt`); analysis
`ozarchy-q7-run/analyst/` (`q7-analyse.py`, `m3.txt`, `cl.txt`).
