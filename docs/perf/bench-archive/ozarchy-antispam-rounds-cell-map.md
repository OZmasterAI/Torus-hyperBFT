# Anti-spam rounds 1-3 and the no-regression A/B: which dir is which doc row (2026-10-04, ozarchy)

Dirs: `ozarchy-as-*`, `ozarchy-as2-*`, `ozarchy-as3-*` (+ `ozarchy-as3-spam-funded256-*.sys`), `ozarchy-s2-sync2-spam256-cap20` (documented under the item 6 sync point 2).
Recorded in the docs: **yes**, `docs/plans/native-antispam-2026-10-04.md` section 5 "Bench results so far" (Round-1, Round-2, Round-3 tables) and
`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` section 1. The docs give the numbers without the dir names. This file maps the dirs to the rows and lists
the cells the tables leave out (warm-ups and one replicate), so the raw dirs can go.

All cells: 10 markets, rate 76,000, 5,000 senders, `--retry-busy`, node = worktree `/home/oz/projects/wt/antispam` unless the table says otherwise. Spam cells add
`SPAM_CANCEL_KEYS` / `SPAM_CANCEL_RATE=2000` / `SPAM_CANCEL_FUNDED` through the harness (set per cell in the `<dir>.campaign.log` header).

## Round 1 (campaign5.log, 2026-10-04 09:48-10:11; `a2054be2`, node `dabaa5e0`)

The doc's "main (without the branch)" row (175,233 at cap 400, 75,425 at cap 20) is not from these dirs: it is `ozarchy-rb-cap400-10m-r1` and `ozarchy-rb-cap20-10m-r1`
(main `3d26f7f2`, `bench-busy-retry` worktree; see `ozarchy-harness-baseline-cap-ab.md`).

| cell dir | role | node / commit | block cap | dur s | matched/s avg | best60 | figure in the doc |
|---|---|---|---|---|---|---|---|
| `ozarchy-as-warm-60s` | warm-up (60 s) | antispam @ a2054be2 (node dabaa5e0) | 400 | 60 | 201,383.0 | 203,678.9 | not tabulated |
| `ozarchy-as-ctl-cap400-r1` | branch, control (only C active), cap 400 | antispam @ a2054be2 (node dabaa5e0) | 400 | 120 | 178,123.6 | 206,802.9 | 178,124 |
| `ozarchy-as-ctl-cap20-r1` | branch, control, cap 20 | antispam @ a2054be2 (node dabaa5e0) | 20 | 120 | 75,578.9 | 80,273.6 | 75,579 |
| `ozarchy-as-on-cap400-r1` | branch, `ANTISPAM=1`, cap 400 | antispam @ a2054be2 (node dabaa5e0) | 400 | 120 | 179,217.6 | 209,314.8 | 179,218 |

## Round 2 (campaign6.log 11:45-12:26 and campaign7.log 12:30-12:39; `062393d3`, node `53f32761`)

| cell dir | role | node / commit | block cap | dur s | matched/s avg | best60 | figure in the doc |
|---|---|---|---|---|---|---|---|
| `ozarchy-as2-warm-60s` | warm-up (60 s) | antispam @ 062393d3 (node 53f32761) | 400 | 60 | 200,099.9 | 208,708.3 | not tabulated |
| `ozarchy-as2-off-cap400` | cap 400, limits off | antispam @ 062393d3 (node 53f32761) | 400 | 120 | 175,048.0 | 209,188.7 | 175,048 |
| `ozarchy-as2-on-cap400` | cap 400, `ANTISPAM=1` | antispam @ 062393d3 (node 53f32761) | 400 | 120 | 178,075.5 | 210,430.1 | 178,076 |
| `ozarchy-as2-throttle-cap400` | intended: `ANTISPAM=1` + `TORUS_ADDR_RATE_BUFFER=1000`; the env was not applied (see below) | antispam @ 062393d3 (node 53f32761) | 400 | 120 | 177,881.4 | 210,545.0 | not tabulated |
| `ozarchy-as2-throttle-cap400-x` | cap 400, `ANTISPAM=1`, `TORUS_ADDR_RATE_BUFFER=1000` | antispam @ 062393d3 (node 53f32761) | 400 | 120 | 163,120.7 | 185,184.6 | 163,121 |
| `ozarchy-as2-spam-unfunded-cap20` | cap 20, 64 unfunded spam keys, `ANTISPAM=1` | antispam @ 062393d3 (node 53f32761) | 20 | 120 | 75,406.4 | 80,625.7 | 75,406 |
| `ozarchy-as2-spam-funded-c25-cap20` | cap 20, 64 funded spam keys, C at 25% | antispam @ 062393d3 (node 53f32761) | 20 | 120 | 5,813.7 | 11,821.1 | 5,814 |
| `ozarchy-as2-spam-funded-c100-cap20` | intended C at 100%; the env was not applied, so C at 25% (see below) | antispam @ 062393d3 (node 53f32761) | 20 | 120 | 8,273.4 | 16,822.7 | (replicate 8,273) |
| `ozarchy-as2-spam-funded-c100-cap20-x` | cap 20, 64 funded spam keys, C at 100% (`TORUS_CANCEL_BLOCK_SHARE_PCT=100`) | antispam @ 062393d3 (node 53f32761) | 20 | 120 | 141.9 | 288.5 | 142 |

Naming trap. `campaign6.log` logs the start of `ozarchy-as2-throttle-cap400` with `EXTRA_ENV=TORUS_ADDR_RATE_BUFFER=1000` and of `ozarchy-as2-spam-funded-c100-cap20` with
`EXTRA_ENV=TORUS_CANCEL_BLOCK_SHARE_PCT=100`, but in both cells' `summary.json` `extra_env` is empty and the node env digest (`<dir>.procenv.txt`) has no such variable. `campaign7.failed-launch`
holds `exit=126` (a failed launch at 12:11). `campaign7.log` re-ran both as the `-x` dirs, whose `summary.json` carries the variable. So the doc's rows follow the **`-x`** dirs
(163,121 = B throttle; 142 = C at 100%), and "replicate 8,273" is `ozarchy-as2-spam-funded-c100-cap20` (C at the default 25%).
`ozarchy-as2-throttle-cap400` (`ANTISPAM=1`, buffer unset) is a replicate of the "cap 400, `ANTISPAM=1`" row and is not in the table.

## Round 3 (campaign8.log 13:10-13:23 and the 256-key cells 13:36-13:46; `6398374` / `0aac9b17`, node `ce0d9e0a`)

| cell dir | role | node / commit | block cap | dur s | matched/s avg | best60 | figure in the doc |
|---|---|---|---|---|---|---|---|
| `ozarchy-as3-warm-60s` | warm-up (60 s) | antispam @ 63983742 (node ce0d9e0a) | 400 | 60 | 204,405.7 | 208,273.4 | not tabulated |
| `ozarchy-as3-spam-funded-c25-cap20` | cap 20, 64 funded spam keys, C at 25%, 60 s | antispam @ 63983742 (node ce0d9e0a) | 20 | 60 | 72,689.8 | 74,094.9 | 72,690 |
| `ozarchy-as3-spam-funded-on-cap20` | same, `ANTISPAM=1`, 60 s | antispam @ 63983742 (node ce0d9e0a) | 20 | 60 | 73,408.7 | 75,689.9 | 73,409 |
| `ozarchy-as3-spam-funded256-cap20` | cap 20, 256 funded keys, C at 25%, 120 s | antispam @ 0aac9b17 (node ce0d9e0a) | 20 | 120 | 72,881.2 | 76,156.1 | 72,881 |
| `ozarchy-as3-spam-funded256-B1000-cap20` | same, `ANTISPAM=1`, `TORUS_ADDR_RATE_BUFFER=1000` | antispam @ 0aac9b17 (node ce0d9e0a) | 20 | 120 | 58,428.6 | 72,032.6 | 58,429 |

The two 256-key dirs also have `<dir>.sys/` (`sar.txt`, `pidstat-proc.txt`, `pidstat-t.txt`), named in the doc ("each with `.poll.txt` and `.sys/`"); the doc notes the per-thread `pidstat` capture is empty.

## No-regression A/B (campaign9.log 14:49-15:16; section 1 of the ozarchy doc)

Main = worktree `/home/oz/projects/wt/main-79a3752` (`79a37524`, node `6bba4c78`); branch = `antispam` `92a02ed6` (node `ce0d9e0a`). Order: warm, main r1, branch r1, main r2, branch r2.

| cell dir | role | node / commit | block cap | dur s | matched/s avg | best60 | figure in the doc |
|---|---|---|---|---|---|---|---|
| `ozarchy-as3-nr-warm` | warm-up (60 s) | antispam @ 92a02ed6 (node ce0d9e0a) | 400 | 60 | 195,052.4 | 201,941.8 | not tabulated |
| `ozarchy-as3-nr-main-r1` | main `79a3752` r1 | main-79a3752 @ 79a37524 (node 6bba4c78) | 400 | 120 | 174,180.7 | 204,567.7 | 174,181 |
| `ozarchy-as3-nr-branch-r1` | branch `92a02ed` r1 | antispam @ 92a02ed6 (node ce0d9e0a) | 400 | 120 | 170,987.5 | 199,636.3 | 170,988 |
| `ozarchy-as3-nr-main-r2` | main `79a3752` r2 | main-79a3752 @ 79a37524 (node 6bba4c78) | 400 | 120 | 174,945.7 | 204,386.1 | 174,946 |
| `ozarchy-as3-nr-branch-r2` | branch `92a02ed` r2 | antispam @ 92a02ed6 (node ce0d9e0a) | 400 | 120 | 175,157.3 | 206,672.4 | 175,157 |

The doc's table also lists CPU-s/1M and submit/s per cell (estimated from `cpu.csv`); those two columns are not repeated here.

## Files

Per-cell dirs follow the standard layout (see `INDEX.md`). Dirs: `ozarchy-as-warm-60s`, `ozarchy-as-ctl-cap400-r1`, `ozarchy-as-ctl-cap20-r1`, `ozarchy-as-on-cap400-r1`, `ozarchy-as2-warm-60s`, `ozarchy-as2-off-cap400`, `ozarchy-as2-on-cap400`, `ozarchy-as2-throttle-cap400`, `ozarchy-as2-throttle-cap400-x`, `ozarchy-as2-spam-unfunded-cap20`, `ozarchy-as2-spam-funded-c25-cap20`, `ozarchy-as2-spam-funded-c100-cap20`, `ozarchy-as2-spam-funded-c100-cap20-x`, `ozarchy-as3-warm-60s`, `ozarchy-as3-spam-funded-c25-cap20`, `ozarchy-as3-spam-funded-on-cap20`, `ozarchy-as3-spam-funded256-cap20`, `ozarchy-as3-spam-funded256-B1000-cap20`, `ozarchy-as3-nr-warm`, `ozarchy-as3-nr-main-r1`, `ozarchy-as3-nr-branch-r1`, `ozarchy-as3-nr-main-r2`, `ozarchy-as3-nr-branch-r2`.

Common to all 23 dirs (42 files): `agreement.jsonl`, `analysis-val0.txt`, `analysis-val1.txt`, `analysis-val2.txt`, `bench.log`, `buckets.csv`, `cpu.csv`, `digest-accounts.txt`, `digest-val0.out`, `digest-val1.out`, `digest-val2.out`, `drain-samples.jsonl`, `drain.json`, `funnel-val0.csv`, `funnel-val1.csv`, `funnel-val2.csv`, `log-summary.json`, `metrics-after-val0.txt`, `metrics-after-val1.txt`, `metrics-after-val2.txt`, `metrics-before-val0.txt`, `metrics-before-val1.txt`, `metrics-before-val2.txt`, `phase-val0.csv`, `phase-val1.csv`, `phase-val2.csv`, `run.log`, `sampler-diagnostics.jsonl`, `sampler.csv`, `sampler.log`, `schedstat.json`, `schedstat.raw`, `state-digest-val0.txt`, `state-digest-val1.txt`, `state-digest-val2.txt`, `summary.json`, `val0.log.excerpt`, `val0.log.gz`, `val1.log.excerpt`, `val1.log.gz`, `val2.log.excerpt`, `val2.log.gz`
Files of 1 MiB or more (all dirs): `ozarchy-as3-spam-funded256-cap20/val0.log.gz` 6.6 MiB; `ozarchy-as3-spam-funded256-cap20/val1.log.gz` 6.6 MiB; `ozarchy-as3-spam-funded256-cap20/val2.log.gz` 6.6 MiB; `ozarchy-as3-spam-funded256-B1000-cap20/val1.log.gz` 6.6 MiB; `ozarchy-as3-spam-funded256-B1000-cap20/val2.log.gz` 6.6 MiB; `ozarchy-as3-spam-funded256-B1000-cap20/val0.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c100-cap20-x/val0.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c100-cap20-x/val2.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c100-cap20-x/val1.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c100-cap20/val2.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c100-cap20/val1.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c100-cap20/val0.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c25-cap20/val0.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c25-cap20/val2.log.gz` 6.6 MiB; `ozarchy-as2-spam-funded-c25-cap20/val1.log.gz` 6.6 MiB; `ozarchy-as-ctl-cap20-r1/val1.log.gz` 6.5 MiB; `ozarchy-as-ctl-cap20-r1/val2.log.gz` 6.5 MiB; `ozarchy-as-ctl-cap20-r1/val0.log.gz` 6.5 MiB; `ozarchy-as2-spam-unfunded-cap20/val2.log.gz` 6.5 MiB; `ozarchy-as2-spam-unfunded-cap20/val1.log.gz` 6.5 MiB; `ozarchy-as2-spam-unfunded-cap20/val0.log.gz` 6.5 MiB; `ozarchy-as3-spam-funded-c25-cap20/val1.log.gz` 6.4 MiB; `ozarchy-as3-spam-funded-c25-cap20/val2.log.gz` 6.4 MiB; `ozarchy-as3-spam-funded-c25-cap20/val0.log.gz` 6.4 MiB; `ozarchy-as3-spam-funded-on-cap20/val2.log.gz` 6.4 MiB; `ozarchy-as3-spam-funded-on-cap20/val0.log.gz` 6.4 MiB; `ozarchy-as3-spam-funded-on-cap20/val1.log.gz` 6.4 MiB; `ozarchy-as3-nr-main-r1/val0.log.gz` 6.3 MiB; `ozarchy-as3-nr-main-r1/val2.log.gz` 6.3 MiB; `ozarchy-as3-nr-main-r1/val1.log.gz` 6.3 MiB; `ozarchy-as3-nr-branch-r1/val2.log.gz` 6.3 MiB; `ozarchy-as2-throttle-cap400/val0.log.gz` 6.3 MiB; `ozarchy-as2-throttle-cap400/val1.log.gz` 6.3 MiB; `ozarchy-as-on-cap400-r1/val0.log.gz` 6.3 MiB; `ozarchy-as2-throttle-cap400/val2.log.gz` 6.3 MiB; `ozarchy-as3-nr-branch-r1/val0.log.gz` 6.3 MiB; `ozarchy-as2-off-cap400/val1.log.gz` 6.3 MiB; `ozarchy-as3-nr-branch-r1/val1.log.gz` 6.3 MiB; `ozarchy-as2-off-cap400/val0.log.gz` 6.3 MiB; `ozarchy-as-on-cap400-r1/val2.log.gz` 6.3 MiB ...

Top-level files: `campaign5.done` (7 B), `campaign5.log` (660 B), `campaign6.done` (7 B), `campaign6.log` (1 KiB), `campaign7.done` (7 B), `campaign7.failed-launch` (9 B), `campaign7.log` (420 B), `campaign8.done` (7 B), `campaign8.log` (657 B), `campaign9.done` (7 B), `campaign9.log` (1 KiB), and per cell `<dir>.campaign.log`, `<dir>.poll.txt`, `<dir>.procenv.txt`, `<dir>.stopPoll`.
