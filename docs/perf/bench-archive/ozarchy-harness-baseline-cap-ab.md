# ozarchy first matched-bench cells and the block-cap sweep (2026-10-04)

Dirs: `ozarchy-main-10m-r1`, `ozarchy-main-10m-r2`, `ozarchy-main-cap200-10m-r1`, `ozarchy-main-cap200-10m-r2`, `ozarchy-main-cap50-10m-r1`, `ozarchy-main-cap20-10m-r1`, `ozarchy-rb-cap20-10m-r1`, `ozarchy-rb-cap50-10m-r1`, `ozarchy-rb-cap65-10m-r1`, `ozarchy-rb-cap400-10m-r1`.
Recorded in the docs: **partly**. The `rb-cap20` and `rb-cap400` numbers are the "main (without the branch)" row of
`docs/plans/native-antispam-2026-10-04.md` section 5 "Round-1 binary (120 s cells)" (75,425 and 175,233). Nothing else here is in a doc.

## Purpose

Not stated in a README. From the cell names, the campaign logs and the harness, these are the first matched-bench cells run on ozarchy
(`main-10m-r1` is the first cell dir by date) and a sweep of the node's native block cap (`BLOCK_CAP` 400 = harness default, 200, 50, 20; the rb series adds 65) on main,
at 10 markets. The `rb-` series repeats the sweep with the bench's `--retry-busy` flag (worktree `bench-busy-retry`, bench md5 `4bad58d9`); the `main-` series has no resend flag in its bench command.
The numbers of `rb-cap20` and `rb-cap400` appear in the native-antispam doc as the "main" control of the anti-spam round 1 (`ozarchy-antispam-rounds-cell-map.md`).

## Commit and setup

* Node: main `3d26f7f2` (summary.json `commit` `3d26f7f220bebee5787f8a7492cd382b12f204e8`), node md5 `56c943efd344bb8b497219ab898eb1dd` in every cell.
* `main-*` cells: worktree `/home/oz/projects/wt/harness`, `dirty_files` 0, bench-throughput md5 `cd3734640314714f5148dc6d7e3c8179`.
* `rb-*` cells: worktree `/home/oz/projects/wt/bench-busy-retry`, `dirty_files` 3, bench-throughput md5 `4bad58d931e2060610c00f52f416294b`.
* Shape (all cells): 3 validators + bench on one host, 10 markets, 5,000 senders, rate 76,000, 120 s, batch 400, `--econ`, `--cross-fraction 0.5`, `--cancel-fraction 0.05`, `--band 5`; block cap through the harness `BLOCK_CAP` env.
* Bench command of `main-10m-r1`: `/home/oz/.cargo-target-matched/release/bench-throughput consensus --rpc-urls http://127.0.0.1:8645,http://127.0.0.1:8646,http://127.0.0.1:8647 --econ --senders 5000 --sender-offset 60 --markets 10 --batch-size 400 --submit-batch 1 --format bin --concurrency 256 --duration 120 --target-margin 1500 --cross-fraction 0.5 --cancel-fraction 0.05 --band 5 --rate-total 76000`;
  the `rb-` cells use `/home/oz/.cargo-target-busyretry/release/bench-throughput` with `--retry-busy` appended.
* Drivers: the cells ran in sequences logged in `campaign2.log` (`main-10m-r2`, `main-cap200-10m-r1`, `main-cap200-10m-r2`), `campaign3.log` (`main-cap20-10m-r1`, `main-cap50-10m-r1`) and `campaign4.log`
  (`rb-cap20`, `rb-cap50`, `rb-cap65`, `rb-cap400`); each cell waited for a quiet host (the log prints `quiet:` with the load averages) before it started.

## Results (copied from each cell's summary.json)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-main-10m-r1` | OK | 2026-10-04 06:54 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 400 | - | 184,789.6 | 182,058.2 | 215,020.1 | 231,689.4 | 2.2 | 234.7 | 373.39 | 3.62 | AGREE | PASS | ACCEPT |
| `ozarchy-main-10m-r2` | OK | 2026-10-04 07:01 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 400 | - | 188,986.7 | 185,238.5 | 212,847.8 | 236,902.4 | 2.2 | 236.7 | 377.50 | 3.63 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap200-10m-r1` | OK | 2026-10-04 07:07 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 200 | - | 143,889.2 | 140,849.4 | 169,131.4 | 180,677.5 | 4.9 | 142.1 | 182.28 | 4.35 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap200-10m-r2` | OK | 2026-10-04 07:14 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 200 | - | 145,327.0 | 141,723.3 | 169,719.3 | 182,350.0 | 4.8 | 138.3 | 187.03 | 4.40 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap50-10m-r1` | OK | 2026-10-04 07:32 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 50 | - | 39,972.5 | 36,697.7 | 72,427.4 | 50,514.3 | 16.5 | 44.5 | 39.50 | 5.35 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap20-10m-r1` | OK | 2026-10-04 07:26 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 20 | - | 221.5 | 225.1 | 450.3 | 308.2 | 21.9 | 18.8 | 3.75 | 12.39 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap20-10m-r1` | OK | 2026-10-04 07:56 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 20 | - | 75,425.4 | 74,262.6 | 80,550.1 | 94,603.6 | 14.2 | 17.9 | 57.79 | 5.89 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap50-10m-r1` | OK | 2026-10-04 08:02 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 50 | - | 105,125.7 | 104,658.3 | 117,999.1 | 131,902.6 | 8.8 | 41.5 | 96.69 | 5.28 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap65-10m-r1` | OK | 2026-10-04 08:08 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 65 | - | 110,207.0 | 109,457.4 | 126,384.2 | 138,233.8 | 7.3 | 51.5 | 115.17 | 5.21 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap400-10m-r1` | OK | 2026-10-04 08:15 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 400 | - | 175,232.5 | 173,799.2 | 206,656.8 | 219,842.6 | 2.4 | 221.0 | 372.39 | 3.84 | AGREE | PASS | ACCEPT |

Verbatim SUMMARY / VALIDITY lines (from the cells' campaign logs):

```
SUMMARY ozarchy-main-10m-r1: matched/s avg=184789.6 first120=182058.2 best60=215020.1 placed/s=231689.4 blk/s=2.2 txs/blk=234.7 timeouts=6.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-main-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-main-10m-r2: matched/s avg=188986.7 first120=185238.5 best60=212847.8 placed/s=236902.4 blk/s=2.2 txs/blk=236.7 timeouts=5.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.3, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-main-10m-r2: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-main-cap200-10m-r1: matched/s avg=143889.2 first120=140849.4 best60=169131.4 placed/s=180677.5 blk/s=4.9 txs/blk=142.1 timeouts=8.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-main-cap200-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-main-cap200-10m-r2: matched/s avg=145327.0 first120=141723.3 best60=169719.3 placed/s=182350.0 blk/s=4.8 txs/blk=138.3 timeouts=9.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-main-cap200-10m-r2: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-main-cap50-10m-r1: matched/s avg=39972.5 first120=36697.7 best60=72427.4 placed/s=50514.3 blk/s=16.5 txs/blk=44.5 timeouts=8.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-main-cap50-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-main-cap20-10m-r1: matched/s avg=221.5 first120=225.1 best60=450.3 placed/s=308.2 blk/s=21.9 txs/blk=18.8 timeouts=51.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-main-cap20-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-rb-cap20-10m-r1: matched/s avg=75425.4 first120=74262.6 best60=80550.1 placed/s=94603.6 blk/s=14.2 txs/blk=17.9 timeouts=8.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-rb-cap20-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-rb-cap50-10m-r1: matched/s avg=105125.7 first120=104658.3 best60=117999.1 placed/s=131902.6 blk/s=8.8 txs/blk=41.5 timeouts=6.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-rb-cap50-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-rb-cap65-10m-r1: matched/s avg=110207.0 first120=109457.4 best60=126384.2 placed/s=138233.8 blk/s=7.3 txs/blk=51.5 timeouts=6.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-rb-cap65-10m-r1: ACCEPT liveness=PASS reasons=[]
SUMMARY ozarchy-rb-cap400-10m-r1: matched/s avg=175232.5 first120=173799.2 best60=206656.8 placed/s=219842.6 blk/s=2.4 txs/blk=221.0 timeouts=3.0 dissem_clean=True agree=AGREE (True) resident_rebuilds=[1, 1, 1] digest_s=[0.2, 0.2, 0.2] drained=True bench_rc=0
VALIDITY ozarchy-rb-cap400-10m-r1: ACCEPT liveness=PASS reasons=[]
```

Derived here from the table (mean of the two cells): cap 400 (`main-10m-r1`, `-r2`) 186,888.2 matched/s; cap 200 (`main-cap200-10m-r1`, `-r2`) 144,608.1 matched/s;
ratio cap 400 / cap 200 = 1.292.

What the table shows (no interpretation beyond the numbers):

* Without `--retry-busy`, cap 20 gives 221.5 matched/s and cap 50 gives 39,972.5; with `--retry-busy`, cap 20 gives 75,425.4, cap 50 105,125.7, cap 65 110,207.0, cap 400 175,232.5.
* All ten cells have liveness PASS and validity ACCEPT (see the columns above).
* All cells are n=1 or n=2; the `rb-` series has one cell per cap.

## Files

Common to all 10 dirs (42 files): `agreement.jsonl`, `analysis-val0.txt`, `analysis-val1.txt`, `analysis-val2.txt`, `bench.log`, `buckets.csv`, `cpu.csv`, `digest-accounts.txt`, `digest-val0.out`, `digest-val1.out`, `digest-val2.out`, `drain-samples.jsonl`, `drain.json`, `funnel-val0.csv`, `funnel-val1.csv`, `funnel-val2.csv`, `log-summary.json`, `metrics-after-val0.txt`, `metrics-after-val1.txt`, `metrics-after-val2.txt`, `metrics-before-val0.txt`, `metrics-before-val1.txt`, `metrics-before-val2.txt`, `phase-val0.csv`, `phase-val1.csv`, `phase-val2.csv`, `run.log`, `sampler-diagnostics.jsonl`, `sampler.csv`, `sampler.log`, `schedstat.json`, `schedstat.raw`, `state-digest-val0.txt`, `state-digest-val1.txt`, `state-digest-val2.txt`, `summary.json`, `val0.log.excerpt`, `val0.log.gz`, `val1.log.excerpt`, `val1.log.gz`, `val2.log.excerpt`, `val2.log.gz`
Files of 1 MiB or more (all dirs): `ozarchy-main-cap20-10m-r1/val1.log.gz` 6.6 MiB; `ozarchy-main-cap20-10m-r1/val2.log.gz` 6.6 MiB; `ozarchy-main-cap20-10m-r1/val0.log.gz` 6.6 MiB; `ozarchy-main-cap50-10m-r1/val2.log.gz` 6.5 MiB; `ozarchy-main-cap50-10m-r1/val1.log.gz` 6.5 MiB; `ozarchy-main-cap50-10m-r1/val0.log.gz` 6.5 MiB; `ozarchy-rb-cap20-10m-r1/val1.log.gz` 6.5 MiB; `ozarchy-rb-cap20-10m-r1/val0.log.gz` 6.5 MiB; `ozarchy-rb-cap20-10m-r1/val2.log.gz` 6.5 MiB; `ozarchy-rb-cap50-10m-r1/val1.log.gz` 6.4 MiB; `ozarchy-rb-cap50-10m-r1/val2.log.gz` 6.4 MiB; `ozarchy-rb-cap50-10m-r1/val0.log.gz` 6.4 MiB; `ozarchy-rb-cap65-10m-r1/val0.log.gz` 6.4 MiB; `ozarchy-rb-cap65-10m-r1/val2.log.gz` 6.4 MiB; `ozarchy-rb-cap65-10m-r1/val1.log.gz` 6.4 MiB; `ozarchy-main-cap200-10m-r2/val2.log.gz` 6.4 MiB; `ozarchy-main-cap200-10m-r2/val1.log.gz` 6.4 MiB; `ozarchy-main-cap200-10m-r2/val0.log.gz` 6.4 MiB; `ozarchy-main-cap200-10m-r1/val0.log.gz` 6.4 MiB; `ozarchy-main-cap200-10m-r1/val1.log.gz` 6.4 MiB; `ozarchy-main-cap200-10m-r1/val2.log.gz` 6.4 MiB; `ozarchy-main-10m-r1/val1.log.gz` 6.3 MiB; `ozarchy-main-10m-r1/val0.log.gz` 6.3 MiB; `ozarchy-main-10m-r1/val2.log.gz` 6.3 MiB; `ozarchy-rb-cap400-10m-r1/val2.log.gz` 6.3 MiB; `ozarchy-rb-cap400-10m-r1/val0.log.gz` 6.3 MiB; `ozarchy-rb-cap400-10m-r1/val1.log.gz` 6.3 MiB; `ozarchy-main-10m-r2/val0.log.gz` 6.3 MiB; `ozarchy-main-10m-r2/val2.log.gz` 6.3 MiB; `ozarchy-main-10m-r2/val1.log.gz` 6.3 MiB; `ozarchy-main-cap20-10m-r1/buckets.csv` 5.0 MiB; `ozarchy-rb-cap400-10m-r1/buckets.csv` 4.5 MiB; `ozarchy-main-10m-r1/buckets.csv` 4.4 MiB; `ozarchy-main-10m-r2/buckets.csv` 4.4 MiB; `ozarchy-main-cap200-10m-r2/buckets.csv` 4.2 MiB; `ozarchy-main-cap50-10m-r1/buckets.csv` 4.2 MiB; `ozarchy-main-cap200-10m-r1/buckets.csv` 4.2 MiB; `ozarchy-rb-cap20-10m-r1/buckets.csv` 4.2 MiB; `ozarchy-rb-cap65-10m-r1/buckets.csv` 4.2 MiB; `ozarchy-rb-cap50-10m-r1/buckets.csv` 4.1 MiB

Top-level files for these cells: `campaign2.done` (7 B), `campaign2.log` (431 B), `campaign3.done` (7 B), `campaign3.log` (287 B), `campaign4.done` (7 B), `campaign4.log` (572 B), plus `<dir>.campaign.log` and `<dir>.done` next to each dir.
