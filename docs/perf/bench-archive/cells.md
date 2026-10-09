# Per-cell results (every cell dir with a `summary.json`)

Copied by script from each cell's `summary.json` (`headline`, `cell`, `binaries`, `liveness`, `validity`) with no rounding beyond one decimal (two for ms columns). `matched/s avg` is `headline.matched_s_avg`; `first120`, `best60` as in the harness. Empty = field absent in that `summary.json`. Cells with status FAILED were interrupted before the summary was written. The skipped dirs are not in this table. Updated 2026-10-08 (s29): added the cells of `p2s0b`, `p2s0r`, `p2s0x` and `p2s0y` (45 cells; the 4 dirs skipped at archive time have no `summary.json`). After the c2h campaign finished: added `c2h` (14 cells). 2026-10-09: added `p2byid` (7 cells).


## 14236fa: Baseline `14236fa` (C3 + C4 + PF1 + cooldown fix) at 300 markets, with perf on r1/r2

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-14236fa-300m-r1` | OK | 2026-10-05 00:22 | item6-phase1 @ 14236fa5 | 7da38063 | 300 | 120 | 400 | - | 48,219.6 | 47,584.2 | 80,518.9 | 69,600.1 | 0.6 | 134.8 | 1,007.33 | 14.41 | AGREE | PASS | ACCEPT |
| `ozarchy-14236fa-300m-r2` | OK | 2026-10-05 00:29 | item6-phase1 @ 14236fa5 | 7da38063 | 300 | 120 | 400 | - | 50,650.8 | 50,464.5 | 79,503.4 | 73,092.0 | 0.9 | 136.3 | 827.42 | 14.11 | AGREE | PASS | ACCEPT |
| `ozarchy-14236fa-300m-warm` | OK | 2026-10-05 00:16 | item6-phase1 @ 14236fa5 | 7da38063 | 300 | 60 | 400 | - | 60,386.7 | 60,386.7 | 83,264.1 | 87,472.0 | 0.7 | 126.1 | 791.85 | 11.85 | AGREE | PASS | ACCEPT |

## 239ff69: `239ff69` (P1-P4 + fix A) at 300 markets, trie off by default, perf on r1/r2

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-239ff69-r1` | OK | 2026-10-05 18:36 | item6-phase1 @ 239ff698 | 88e2ddfe | 300 | 120 | 400 | - | 65,620.3 | 64,941.7 | 100,163.4 | 93,975.6 | 1.1 | 166.2 | 691.05 | 9.92 | AGREE | PASS | ACCEPT |
| `ozarchy-239ff69-r2` | OK | 2026-10-05 18:43 | item6-phase1 @ 239ff698 | 88e2ddfe | 300 | 120 | 400 | - | 64,750.9 | 63,614.5 | 100,722.4 | 92,953.3 | 1.1 | 171.1 | 664.06 | 9.93 | AGREE | PASS | ACCEPT |
| `ozarchy-239ff69-warm` | OK | 2026-10-05 18:30 | item6-phase1 @ 239ff698 | 88e2ddfe | 300 | 60 | 400 | - | 76,710.5 | 76,710.5 | 99,592.8 | 110,934.3 | 0.9 | 138.4 | 586.83 | 8.35 | AGREE | PASS | ACCEPT |

## 4acdc59: Step 2 (`4acdc59`: end_resident on a worker) vs main at 300 markets (run 4, valid)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-4acdc59-300m-crab-r1` | OK | 2026-10-06 01:16 | item6-4acdc59 @ 4acdc594 | b5a2cad4 | 300 | 120 | 400 | - | 82,865.4 | 82,563.9 | 105,306.9 | 119,231.9 | 1.4 | 186.8 | 532.73 | 7.95 | AGREE | PASS | ACCEPT |
| `ozarchy-4acdc59-300m-crab-r2` | OK | 2026-10-06 01:30 | item6-4acdc59 @ 4acdc594 | b5a2cad4 | 300 | 120 | 400 | - | 79,754.9 | 78,915.9 | 104,753.4 | 114,724.1 | 1.3 | 190.9 | 564.05 | 8.21 | AGREE | PASS | ACCEPT |
| `ozarchy-4acdc59-300m-main-r1` | OK | 2026-10-06 01:23 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 101,165.1 | 100,167.6 | 130,493.9 | 131,936.9 | 1.4 | 197.4 | 580.39 | 7.15 | AGREE | PASS | ACCEPT |
| `ozarchy-4acdc59-300m-main-r2` | OK | 2026-10-06 01:38 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 94,481.8 | 92,962.3 | 134,550.3 | 123,188.2 | 1.4 | 188.0 | 621.27 | 7.49 | AGREE | PASS | ACCEPT |
| `ozarchy-4acdc59-300m-warm` | OK | 2026-10-06 01:10 | item6-4acdc59 @ 4acdc594 | b5a2cad4 | 300 | 60 | 400 | - | 98,000.6 | 98,000.6 | 111,743.3 | 141,288.5 | 1.7 | 137.7 | 409.03 | 6.68 | AGREE | PASS | ACCEPT |

## 4acdc59-void: Voided runs 1-3 of the step 2 (`4acdc59`) 300-market campaign (a process was SIGKILLed in each)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-4acdc59-300m-crab-r1-VOID-run2` | FAILED |  |  |  |  |  |  | - |  |  |  |  |  |  |  |  | - | - | - |
| `ozarchy-4acdc59-300m-crab-r1-VOID-run3` | INVALID | 2026-10-06 00:58 | item6-4acdc59 @ 4acdc594 | b5a2cad4 | 300 | 120 | 400 | - | 105,009.4 | 105,009.4 | 119,208.5 | 151,248.0 | 1.3 | 166.4 | 426.32 | 6.17 | AGREE | PASS | REJECT |
| `ozarchy-4acdc59-300m-warm-VOID-run2` | OK | 2026-10-06 00:33 | item6-4acdc59 @ 4acdc594 | b5a2cad4 | 300 | 60 | 400 | - | 98,708.8 | 98,708.8 | 110,938.6 | 142,006.7 | 1.4 | 147.0 | 457.62 | 6.73 | AGREE | PASS | ACCEPT |
| `ozarchy-4acdc59-300m-warm-VOID-run3` | OK | 2026-10-06 00:53 | item6-4acdc59 @ 4acdc594 | b5a2cad4 | 300 | 60 | 400 | - | 97,640.8 | 97,640.8 | 110,838.7 | 141,121.8 | 1.3 | 173.2 | 469.30 | 6.74 | AGREE | PASS | ACCEPT |
| `ozarchy-4acdc59-300m-warm-VOID-val1died` | FAILED |  |  |  |  |  |  | - |  |  |  |  |  |  |  |  | - | - | - |

## 5524646: Gate 2 at 10 markets without perf: `5524646` vs main `92a02ed`

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-5524646-10m-crab-r1` | OK | 2026-10-05 23:19 | item6-5524646 @ 55246466 | a2088294 | 10 | 120 | 400 | - | 153,295.3 | 152,620.7 | 188,106.1 | 193,033.7 | 2.0 | 229.2 | 393.55 | 4.19 | AGREE | PASS | ACCEPT |
| `ozarchy-5524646-10m-crab-r2` | OK | 2026-10-05 23:31 | item6-5524646 @ 55246466 | a2088294 | 10 | 120 | 400 | - | 152,327.2 | 151,595.9 | 181,510.0 | 191,901.3 | 2.0 | 210.4 | 385.78 | 4.16 | AGREE | PASS | ACCEPT |
| `ozarchy-5524646-10m-main-r1` | OK | 2026-10-05 23:25 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 174,754.6 | 174,051.6 | 208,847.9 | 219,245.5 | 2.5 | 227.4 | 365.65 | 3.81 | AGREE | PASS | ACCEPT |
| `ozarchy-5524646-10m-main-r2` | OK | 2026-10-05 23:37 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 178,100.0 | 177,405.8 | 215,500.3 | 223,498.5 | 2.5 | 227.2 | 370.00 | 3.80 | AGREE | PASS | ACCEPT |
| `ozarchy-5524646-10m-warm` | OK | 2026-10-05 23:14 | item6-5524646 @ 55246466 | a2088294 | 10 | 60 | 400 | - | 186,805.3 | 186,805.3 | 192,679.6 | 235,181.5 | 2.1 | 138.1 | 327.46 | 3.49 | AGREE | PASS | ACCEPT |

## c7: C6 + C7 (`82bd1a4`) at 300 markets, trie off, with perf on r1/r2

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-82bd1a4-c7-r1` | OK | 2026-10-05 15:54 | item6-phase1 @ 82bd1a41 | 2783579b | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 65,111.3 | 64,170.2 | 97,624.3 | 93,544.3 | 1.0 | 164.7 | 691.84 | 10.17 | AGREE | PASS | ACCEPT |
| `ozarchy-82bd1a4-c7-r2` | OK | 2026-10-05 16:01 | item6-phase1 @ 82bd1a41 | 2783579b | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 63,296.4 | 63,166.7 | 97,941.2 | 91,043.7 | 1.0 | 170.3 | 713.33 | 10.25 | AGREE | PASS | ACCEPT |
| `ozarchy-82bd1a4-c7-warm` | OK | 2026-10-05 15:47 | item6-phase1 @ 82bd1a41 | 2783579b | 300 | 60 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 71,355.3 | 71,355.3 | 98,432.1 | 102,998.9 | 0.8 | 141.1 | 655.55 | 8.85 | AGREE | PASS | ACCEPT |

## 90a752c: M1 (`90a752c`) at 300 markets, trie off by default, perf on r1/r2

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-90a752c-r1` | OK | 2026-10-05 20:55 | item6-phase1 @ 90a752ce | 50dcee7e | 300 | 120 | 400 | - | 74,732.8 | 74,366.9 | 103,867.3 | 107,395.8 | 1.2 | 144.5 | 579.13 | 7.82 | AGREE | PASS | ACCEPT |
| `ozarchy-90a752c-r2` | OK | 2026-10-05 21:02 | item6-phase1 @ 90a752ce | 50dcee7e | 300 | 120 | 400 | - | 78,185.7 | 77,318.0 | 102,323.9 | 112,275.0 | 1.3 | 192.0 | 565.27 | 7.62 | AGREE | PASS | ACCEPT |
| `ozarchy-90a752c-warm` | OK | 2026-10-05 20:49 | item6-phase1 @ 90a752ce | 50dcee7e | 300 | 60 | 400 | - | 91,885.6 | 91,885.6 | 106,210.9 | 132,519.7 | 1.2 | 146.7 | 475.55 | 6.47 | AGREE | PASS | ACCEPT |

## action-results: Per-action execution results (`9195c32`) at 300 markets: warm + r1

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-action-results-r1` | OK | 2026-10-05 19:52 | action-results @ 9195c324 | 8db911fb | 300 | 120 | 400 | - | 67,435.8 | 66,362.4 | 94,806.6 | 96,907.3 | 1.1 | 188.2 | 668.83 | 9.57 | AGREE | PASS | ACCEPT |
| `ozarchy-action-results-warm` | OK | 2026-10-05 19:46 | action-results @ 9195c324 | 8db911fb | 300 | 60 | 400 | - | 77,425.2 | 77,425.2 | 96,296.9 | 111,790.3 | 0.9 | 127.3 | 598.47 | 8.25 | AGREE | PASS | ACCEPT |

## adlcells: ADL budget proof cells (`perf/adl-budget` @ `6a25e20`): warm, S=400, S=750 with the value sum off

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-adlcells-300m-s400` | OK | 2026-10-07 02:11 | adl-cells-6a25e20 @ 6a25e201 | 18bf0759 | 300 | 120 | 400 | - | 161,875.0 | 162,101.0 | 179,445.0 | 222,757.5 | 5.2 | 185.8 | 186.95 | 4.89 | AGREE | PASS | ACCEPT |
| `ozarchy-adlcells-300m-s400vs` | FAILED |  |  |  |  |  |  | - |  |  |  |  |  |  |  |  | - | - | - |
| `ozarchy-adlcells-300m-s750` | OK | 2026-10-07 02:17 | adl-cells-6a25e20 @ 6a25e201 | 18bf0759 | 300 | 120 | 400 | - | 150,855.4 | 151,807.2 | 181,102.5 | 207,655.6 | 4.7 | 192.1 | 202.62 | 5.25 | AGREE | PASS | ACCEPT |
| `ozarchy-adlcells-300m-s750vs` | OK | 2026-10-07 11:58 | adl-cells-6a25e20 @ 6a25e201 | 18bf0759 | 300 | 120 | 400 | TORUS_LIQ_VALUE_SUM=1 | 65,293.8 | 65,872.9 | 81,035.2 | 86,390.7 | 1.7 | 204.9 | 564.70 | 14.14 | AGREE | PASS | ACCEPT |
| `ozarchy-adlcells-300m-warm` | OK | 2026-10-07 02:04 | adl-cells-6a25e20 @ 6a25e201 | 18bf0759 | 300 | 60 | 400 | - | 164,329.3 | 164,329.3 | 183,613.7 | 217,639.1 | 4.4 | 197.0 | 208.32 | 4.59 | AGREE | PASS | ACCEPT |

## as-r1: Anti-spam round 1 (branch antispam): control vs `ANTISPAM=1`, cap 400 and cap 20, no spam

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-as-ctl-cap20-r1` | OK | 2026-10-04 10:04 | antispam @ a2054be2 | dabaa5e0 | 10 | 120 | 20 | - | 75,578.9 | 74,104.7 | 80,273.6 | 94,786.2 | 14.3 | 18.0 | 57.49 | 5.94 | AGREE | PASS | ACCEPT |
| `ozarchy-as-ctl-cap400-r1` | OK | 2026-10-04 09:58 | antispam @ a2054be2 | dabaa5e0 | 10 | 120 | 400 | - | 178,123.6 | 177,554.6 | 206,802.9 | 223,405.9 | 2.3 | 228.2 | 367.28 | 3.77 | AGREE | PASS | ACCEPT |
| `ozarchy-as-on-cap400-r1` | OK | 2026-10-04 10:11 | antispam @ a2054be2 | dabaa5e0 | 10 | 120 | 400 | - | 179,217.6 | 179,116.1 | 209,314.8 | 224,864.8 | 2.3 | 220.0 | 362.61 | 3.74 | AGREE | PASS | ACCEPT |
| `ozarchy-as-warm-60s` | OK | 2026-10-04 09:50 | antispam @ a2054be2 | dabaa5e0 | 10 | 60 | 400 | - | 201,383.0 | 201,383.0 | 203,678.9 | 252,468.1 | 2.4 | 174.3 | 317.19 | 3.23 | AGREE | PASS | ACCEPT |

## as-r2: Anti-spam round 2: limits off/on, B throttle, 64 unfunded / funded cancel-spam keys at cap 20, C share 25% vs 100%

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-as2-off-cap400` | OK | 2026-10-04 11:54 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 400 | - | 175,048.0 | 174,590.0 | 209,188.7 | 219,628.8 | 2.2 | 227.2 | 372.44 | 3.81 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-on-cap400` | OK | 2026-10-04 12:00 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 400 | - | 178,075.5 | 177,237.4 | 210,430.1 | 223,389.8 | 2.3 | 223.6 | 369.73 | 3.76 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-spam-funded-c100-cap20` | OK | 2026-10-04 12:26 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 20 | - | 8,273.4 | 8,411.3 | 16,822.7 | 10,383.6 | 19.4 | 18.7 | 6.20 | 4.78 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-spam-funded-c100-cap20-x` | OK | 2026-10-04 12:39 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 20 | TORUS_CANCEL_BLOCK_SHARE_PCT=100 | 141.9 | 144.2 | 288.5 | 196.7 | 19.3 | 18.5 | 2.17 | 15.23 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-spam-funded-c25-cap20` | OK | 2026-10-04 12:19 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 20 | - | 5,813.7 | 5,910.6 | 11,821.1 | 7,308.2 | 18.9 | 18.8 | 4.94 | 4.85 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-spam-unfunded-cap20` | OK | 2026-10-04 12:13 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 20 | - | 75,406.4 | 73,860.9 | 80,625.7 | 94,660.2 | 14.2 | 18.1 | 58.33 | 5.94 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-throttle-cap400` | OK | 2026-10-04 12:06 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 400 | - | 177,881.4 | 177,798.0 | 210,545.0 | 223,119.2 | 2.5 | 213.6 | 362.18 | 3.77 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-throttle-cap400-x` | OK | 2026-10-04 12:33 | antispam @ 062393d3 | 53f32761 | 10 | 120 | 400 | TORUS_ADDR_RATE_BUFFER=1000 | 163,120.7 | 161,418.6 | 185,184.6 | 204,728.5 | 2.7 | 212.6 | 339.49 | 4.14 | AGREE | PASS | ACCEPT |
| `ozarchy-as2-warm-60s` | OK | 2026-10-04 11:47 | antispam @ 062393d3 | 53f32761 | 10 | 60 | 400 | - | 200,099.9 | 200,099.9 | 208,708.3 | 250,927.1 | 2.2 | 180.1 | 334.59 | 3.26 | AGREE | PASS | ACCEPT |

## as-nr: Anti-spam no-regression A/B: main `79a3752` vs `feat/native-antispam` (`92a02ed`), cap 400, no spam

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-as3-nr-branch-r1` | OK | 2026-10-04 15:04 | antispam @ 92a02ed6 | ce0d9e0a | 10 | 120 | 400 | - | 170,987.5 | 170,164.6 | 199,636.3 | 214,547.3 | 2.4 | 212.6 | 369.09 | 3.84 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-nr-branch-r2` | OK | 2026-10-04 15:16 | antispam @ 92a02ed6 | ce0d9e0a | 10 | 120 | 400 | - | 175,157.3 | 174,498.0 | 206,672.4 | 219,770.4 | 2.5 | 235.8 | 369.87 | 3.81 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-nr-main-r1` | OK | 2026-10-04 14:58 | main-79a3752 @ 79a37524 | 6bba4c78 | 10 | 120 | 400 | - | 174,180.7 | 173,653.5 | 204,567.7 | 218,510.5 | 2.4 | 198.8 | 361.50 | 3.77 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-nr-main-r2` | OK | 2026-10-04 15:10 | main-79a3752 @ 79a37524 | 6bba4c78 | 10 | 120 | 400 | - | 174,945.7 | 174,567.0 | 204,386.1 | 219,482.1 | 2.4 | 228.9 | 368.04 | 3.81 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-nr-warm` | OK | 2026-10-04 14:51 | antispam @ 92a02ed6 | ce0d9e0a | 10 | 60 | 400 | - | 195,052.4 | 195,052.4 | 201,941.8 | 244,564.5 | 2.1 | 163.5 | 356.08 | 3.40 | AGREE | PASS | ACCEPT |

## as-r3-64: Anti-spam round 3: 64 funded cancel-spam keys at cap 20, 60 s, C at 25%, `ANTISPAM` off / on

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-as3-spam-funded-c25-cap20` | OK | 2026-10-04 13:17 | antispam @ 63983742 | ce0d9e0a | 10 | 60 | 20 | - | 72,689.8 | 72,689.8 | 74,094.9 | 91,041.2 | 17.0 | 17.1 | 37.76 | 4.46 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-spam-funded-on-cap20` | OK | 2026-10-04 13:23 | antispam @ 63983742 | ce0d9e0a | 10 | 60 | 20 | - | 73,408.7 | 73,408.7 | 75,689.9 | 92,105.2 | 16.8 | 17.7 | 38.81 | 4.49 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-warm-60s` | OK | 2026-10-04 13:12 | antispam @ 63983742 | ce0d9e0a | 10 | 60 | 400 | - | 204,405.7 | 204,405.7 | 208,273.4 | 256,221.9 | 2.4 | 192.0 | 320.09 | 3.22 | AGREE | PASS | ACCEPT |

## as-r3-256: Anti-spam round 3: 256 funded cancel-spam keys at cap 20, 120 s (plain, and `ANTISPAM=1` with `TORUS_ADDR_RATE_BUFFER=1000`)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-as3-spam-funded256-B1000-cap20` | OK | 2026-10-04 13:46 | antispam @ 0aac9b17 | ce0d9e0a | 10 | 120 | 20 | TORUS_ADDR_RATE_BUFFER=1000 | 58,428.6 | 58,834.8 | 72,032.6 | 73,298.8 | 16.8 | 19.2 | 36.07 | 6.24 | AGREE | PASS | ACCEPT |
| `ozarchy-as3-spam-funded256-cap20` | OK | 2026-10-04 13:39 | antispam @ 0aac9b17 | ce0d9e0a | 10 | 120 | 20 | - | 72,881.2 | 71,615.9 | 76,156.1 | 91,293.0 | 17.6 | 18.7 | 37.38 | 5.05 | AGREE | PASS | ACCEPT |

## bblind: Gate 2 with B-blind (`31cea69`) vs main `92a02ed`, 300 and 10 markets

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-bblind-31cea69-10m-crab-r1` | OK | 2026-10-06 06:54 | bblind-31cea69 @ 31cea698 | 3a3c8951 | 10 | 120 | 400 | - | 175,815.5 | 174,311.3 | 206,930.0 | 220,576.6 | 2.3 | 218.3 | 336.72 | 3.87 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-10m-crab-r2` | OK | 2026-10-06 07:06 | bblind-31cea69 @ 31cea698 | 3a3c8951 | 10 | 120 | 400 | - | 172,079.2 | 171,007.8 | 202,130.8 | 215,869.9 | 2.2 | 217.5 | 333.35 | 3.88 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-10m-main-r1` | OK | 2026-10-06 07:00 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 172,830.8 | 172,444.0 | 205,425.1 | 216,776.8 | 2.4 | 219.3 | 370.84 | 3.81 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-10m-main-r2` | OK | 2026-10-06 07:12 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 176,017.0 | 174,728.4 | 211,035.8 | 220,837.5 | 2.5 | 218.9 | 365.29 | 3.79 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-10m-warm` | OK | 2026-10-06 06:48 | bblind-31cea69 @ 31cea698 | 3a3c8951 | 10 | 60 | 400 | - | 208,227.8 | 208,227.8 | 212,229.0 | 261,156.4 | 2.4 | 173.9 | 255.03 | 3.34 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-300m-crab-r1` | OK | 2026-10-06 06:24 | bblind-31cea69 @ 31cea698 | 3a3c8951 | 300 | 120 | 400 | - | 110,592.9 | 109,479.1 | 131,359.1 | 144,520.7 | 1.5 | 196.3 | 500.21 | 6.31 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-300m-crab-r2` | OK | 2026-10-06 06:37 | bblind-31cea69 @ 31cea698 | 3a3c8951 | 300 | 120 | 400 | - | 110,111.6 | 109,067.6 | 133,905.3 | 143,773.0 | 1.5 | 210.0 | 493.68 | 6.20 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-300m-main-r1` | OK | 2026-10-06 06:30 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,317.3 | 99,145.9 | 133,575.4 | 130,819.8 | 1.5 | 183.4 | 573.66 | 7.08 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-300m-main-r2` | OK | 2026-10-06 06:44 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,869.2 | 100,137.7 | 134,726.3 | 131,534.8 | 1.6 | 194.4 | 569.42 | 7.04 | AGREE | PASS | ACCEPT |
| `ozarchy-bblind-31cea69-300m-warm` | OK | 2026-10-06 06:17 | bblind-31cea69 @ 31cea698 | 3a3c8951 | 300 | 60 | 400 | - | 124,721.8 | 124,721.8 | 146,465.8 | 163,375.5 | 1.4 | 175.1 | 433.78 | 5.36 | AGREE | PASS | ACCEPT |

## bd: s94 batch cost: main `35e69b3` vs `92a02ed`, 300 markets, N=4 + budget 900

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-bd-300m-crab-r1` | OK | 2026-10-06 19:20 | main-35e69b3 @ 35e69b3d | c2ea1ff8 | 300 | 120 | 400 | - | 181,500.1 | 181,500.0 | 190,535.3 | 240,977.2 | 5.5 | 134.4 | 145.98 | 4.29 | AGREE | PASS | ACCEPT |
| `ozarchy-bd-300m-crab-r2` | OK | 2026-10-06 19:33 | main-35e69b3 @ 35e69b3d | c2ea1ff8 | 300 | 120 | 400 | - | 180,501.3 | 179,940.7 | 190,865.4 | 240,335.9 | 5.4 | 122.2 | 139.62 | 4.28 | AGREE | PASS | ACCEPT |
| `ozarchy-bd-300m-crab-warm` | OK | 2026-10-06 19:15 | main-35e69b3 @ 35e69b3d | c2ea1ff8 | 300 | 60 | 400 | - | 185,846.1 | 185,846.1 | 195,220.1 | 245,363.1 | 4.7 | 114.1 | 139.29 | 4.22 | AGREE | PASS | ACCEPT |
| `ozarchy-bd-300m-main-r1` | OK | 2026-10-06 19:27 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 171,745.8 | 170,977.0 | 182,667.1 | 228,276.4 | 5.4 | 123.7 | 264.90 | 4.61 | AGREE | PASS | ACCEPT |
| `ozarchy-bd-300m-main-r2` | OK | 2026-10-06 19:40 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 159,788.3 | 159,626.9 | 170,126.9 | 211,336.6 | 4.7 | 128.9 | 282.23 | 4.96 | AGREE | PASS | ACCEPT |
| `ozarchy-bd-smoke-main` | OK | 2026-10-06 19:10 | main @ 1cd786da | 31a95c65 | 300 | 30 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 177,373.1 | 177,373.1 | 0.0 | 233,054.5 | 4.5 | 73.9 | 291.98 | 4.17 | AGREE | PASS | ACCEPT |

## c1ab: C1 full-node A/B: `9c4be2c` (before C1) vs `81a9567` (C1), 10 markets

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-c1ab-c1-r1` | OK | 2026-10-04 18:03 | item6-phase1 @ 81a95677 | 252ce826 | 10 | 120 | 400 | - | 63,853.3 | 63,703.1 | 80,147.2 | 80,349.5 | 0.6 | 141.1 | 1,365.12 | 16.60 | AGREE | PASS | ACCEPT |
| `ozarchy-c1ab-c1-r2` | OK | 2026-10-04 18:16 | item6-phase1 @ 81a95677 | 252ce826 | 10 | 120 | 400 | - | 64,468.1 | 64,646.8 | 81,810.8 | 81,174.7 | 0.7 | 136.3 | 1,346.40 | 16.51 | AGREE | PASS | ACCEPT |
| `ozarchy-c1ab-pre-r1` | OK | 2026-10-04 17:56 | crab-9c4be2c @ 9c4be2cc | c3480413 | 10 | 120 | 400 | - | 60,405.7 | 60,898.9 | 79,131.1 | 75,985.0 | 0.6 | 156.6 | 1,405.76 | 16.89 | AGREE | PASS | ACCEPT |
| `ozarchy-c1ab-pre-r2` | OK | 2026-10-04 18:10 | crab-9c4be2c @ 9c4be2cc | c3480413 | 10 | 120 | 400 | - | 63,792.9 | 63,934.3 | 81,047.2 | 80,310.4 | 0.6 | 148.5 | 1,375.50 | 16.58 | AGREE | PASS | ACCEPT |
| `ozarchy-c1ab-pre-smoke` | OK | 2026-10-04 17:45 | crab-9c4be2c @ 9c4be2cc | c3480413 | 10 | 30 | 400 | - | 99,420.6 | 99,420.6 | 99,844.7 | 125,267.6 | 0.9 | 141.3 | 831.47 | 12.03 | AGREE | PASS | ACCEPT |
| `ozarchy-c1ab-warm` | OK | 2026-10-04 17:50 | item6-phase1 @ 81a95677 | 252ce826 | 10 | 60 | 400 | - | 81,027.8 | 81,027.8 | 82,089.8 | 102,016.0 | 0.8 | 119.7 | 1,078.59 | 13.64 | AGREE | PASS | ACCEPT |

## c3pf1-10m: C3 + PF1 (`d9ef4f7`) vs main at 10 markets, with and without `RETRY_BUSY`

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-c3pf1-10m-retry0-c3pf1-r1` | OK | 2026-10-04 21:39 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 10 | 120 | 400 | - | 122,501.9 | 120,692.6 | 162,695.3 | 154,232.7 | 1.6 | 225.0 | 442.90 | 5.53 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry0-c3pf1-r2` | OK | 2026-10-04 21:53 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 10 | 120 | 400 | - | 123,957.4 | 121,559.6 | 162,300.6 | 156,215.0 | 1.6 | 215.0 | 441.54 | 5.59 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry0-main-r1` | OK | 2026-10-04 21:46 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 188,043.6 | 185,094.4 | 206,528.8 | 235,816.5 | 2.2 | 214.4 | 375.56 | 3.73 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry0-main-r2` | OK | 2026-10-04 22:00 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 185,757.0 | 183,145.8 | 203,859.7 | 232,904.0 | 2.1 | 245.2 | 383.22 | 3.71 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry1-c3pf1-r1` | OK | 2026-10-04 21:13 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 10 | 120 | 400 | - | 129,692.6 | 128,855.4 | 154,600.0 | 163,300.7 | 1.7 | 215.9 | 451.33 | 5.51 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry1-c3pf1-r2` | OK | 2026-10-04 21:25 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 10 | 120 | 400 | - | 131,669.8 | 130,683.4 | 156,146.1 | 165,847.1 | 1.7 | 215.8 | 452.06 | 5.47 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry1-main-r1` | OK | 2026-10-04 21:19 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 175,826.8 | 176,316.0 | 209,193.9 | 220,567.0 | 2.3 | 223.8 | 372.74 | 3.84 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-retry1-main-r2` | OK | 2026-10-04 21:33 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 172,248.1 | 171,320.8 | 197,996.3 | 216,089.3 | 2.4 | 227.4 | 367.59 | 3.83 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-10m-warm` | OK | 2026-10-04 21:07 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 10 | 60 | 400 | - | 149,108.6 | 149,108.6 | 165,263.0 | 187,762.0 | 1.7 | 201.6 | 390.53 | 4.59 | AGREE | PASS | ACCEPT |

## c3pf1-300m: C3 + PF1 (`d9ef4f7`) vs main at 300 markets (`RETRY_BUSY=1`); both r1 cells carry perf data; main-r1 is the profile used in section 11

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-c3pf1-300m-c3pf1-r1` | OK | 2026-10-04 22:14 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 300 | 120 | 400 | - | 49,441.8 | 49,146.8 | 77,907.1 | 71,100.8 | 0.7 | 104.4 | 950.65 | 14.68 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-300m-c3pf1-r2` | OK | 2026-10-04 22:28 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 300 | 120 | 400 | - | 49,727.3 | 48,956.4 | 78,278.4 | 71,642.4 | 0.7 | 117.2 | 984.83 | 14.75 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-300m-main-r1` | OK | 2026-10-04 22:20 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | - | 87,791.7 | 87,362.4 | 125,374.9 | 114,546.2 | 1.2 | 160.5 | 763.97 | 7.24 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-300m-main-r2` | OK | 2026-10-04 22:34 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | - | 91,022.6 | 90,957.4 | 121,136.4 | 118,693.9 | 1.3 | 171.1 | 722.34 | 6.86 | AGREE | PASS | ACCEPT |
| `ozarchy-c3pf1-300m-warm` | OK | 2026-10-04 22:07 | item6-c3-pf1 @ d9ef4f7a | e226ec7c | 300 | 60 | 400 | - | 51,620.6 | 51,620.6 | 82,208.5 | 74,895.6 | 1.1 | 113.7 | 690.56 | 12.51 | AGREE | PASS | ACCEPT |

## c58775f: Gate 2 at 10 markets: `c58775f` vs main `92a02ed`, both trie off (crab cells with perf)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-c58775f-10m-crab-r1` | OK | 2026-10-05 22:10 | item6-phase1 @ c58775f9 | 494d8812 | 10 | 120 | 400 | - | 157,906.8 | 157,222.4 | 191,106.0 | 198,901.2 | 2.0 | 225.4 | 381.89 | 4.12 | AGREE | PASS | ACCEPT |
| `ozarchy-c58775f-10m-crab-r2` | OK | 2026-10-05 22:22 | item6-phase1 @ c58775f9 | 494d8812 | 10 | 120 | 400 | - | 154,490.3 | 154,432.8 | 182,156.2 | 194,589.1 | 2.0 | 236.9 | 377.46 | 4.14 | AGREE | PASS | ACCEPT |
| `ozarchy-c58775f-10m-main-r1` | OK | 2026-10-05 22:16 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 178,517.9 | 177,776.0 | 211,907.9 | 223,982.1 | 2.3 | 216.6 | 363.81 | 3.77 | AGREE | PASS | ACCEPT |
| `ozarchy-c58775f-10m-main-r2` | OK | 2026-10-05 22:28 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 171,506.0 | 171,096.8 | 205,051.1 | 215,185.5 | 2.4 | 226.0 | 368.87 | 3.83 | AGREE | PASS | ACCEPT |
| `ozarchy-c58775f-10m-warm` | OK | 2026-10-05 22:04 | item6-phase1 @ c58775f9 | 494d8812 | 10 | 60 | 400 | - | 184,735.1 | 184,735.1 | 188,108.2 | 232,615.0 | 2.1 | 190.0 | 325.24 | 3.51 | AGREE | PASS | ACCEPT |

## feeddrain: Live-feed idle check: oracle feed live through the drain (`5584880`), 300 markets

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-feeddrain-5584880-300m-r1` | OK | 2026-10-06 09:05 | feed-drain @ ffc245e4 | 721e48d7 | 300 | 120 | 400 | - | 111,439.9 | 110,541.8 | 131,435.0 | 145,662.5 | 1.5 | 218.3 | 283.85 | 6.29 | AGREE | PASS | ACCEPT |

## ipc: IPC / cache-miss profile (perf record, one event at a time) crab vs main at 300 markets, trie off

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-ipc-crab` | OK | 2026-10-05 01:32 | item6-phase1 @ 51051c97 | 7da38063 | 300 | 120 | 400 | - | 45,261.4 | 45,603.0 | 76,595.3 | 65,251.7 | 0.6 | 138.1 | 1,010.46 | 14.86 | AGREE | PASS | ACCEPT |
| `ozarchy-ipc-main` | OK | 2026-10-05 01:39 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | - | 79,065.2 | 79,913.2 | 109,244.7 | 103,179.8 | 1.1 | 159.0 | 828.18 | 7.67 | AGREE | PASS | ACCEPT |
| `ozarchy-ipc-warm` | OK | 2026-10-05 01:25 | item6-phase1 @ 51051c97 | 7da38063 | 300 | 60 | 400 | - | 56,110.5 | 56,110.5 | 84,713.4 | 81,181.9 | 0.6 | 131.5 | 827.35 | 12.02 | AGREE | PASS | ACCEPT |

## liq: Liquidation stress (`bench/liq-stress` @ `af8529e`): thin accounts + parity-signed shock, S=400 and S=750

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-liq-300m-s400` | OK | 2026-10-06 20:08 | liq-stress @ af8529e8 | c2ea1ff8 | 300 | 120 | 400 | - | 161,564.6 | 161,928.8 | 184,443.8 | 222,267.9 | 5.1 | 188.6 | 185.82 | 4.84 | AGREE | PASS | ACCEPT |
| `ozarchy-liq-300m-s750` | INVALID | 2026-10-06 20:27 | liq-stress @ af8529e8 | c2ea1ff8 | 300 | 120 | 400 | - | 127,720.3 | 129,849.0 | 179,404.8 | 169,902.5 | 3.8 | 185.4 | 190.13 | 4.69 | AGREE | FAIL | REJECT |
| `ozarchy-liq-300m-warm` | OK | 2026-10-06 20:02 | liq-stress @ af8529e8 | c2ea1ff8 | 300 | 60 | 400 | - | 171,299.4 | 171,299.4 | 180,562.8 | 227,112.1 | 4.6 | 196.7 | 203.65 | 4.48 | AGREE | PASS | ACCEPT |

## harness-cap-ab: First matched-bench cells on ozarchy and the block-cap sweep on main (cap 400 default, 200, 50, 20); 10 markets, 120 s, rate 76,000

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-main-10m-r1` | OK | 2026-10-04 06:54 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 400 | - | 184,789.6 | 182,058.2 | 215,020.1 | 231,689.4 | 2.2 | 234.7 | 373.39 | 3.62 | AGREE | PASS | ACCEPT |
| `ozarchy-main-10m-r2` | OK | 2026-10-04 07:01 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 400 | - | 188,986.7 | 185,238.5 | 212,847.8 | 236,902.4 | 2.2 | 236.7 | 377.50 | 3.63 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap20-10m-r1` | OK | 2026-10-04 07:26 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 20 | - | 221.5 | 225.1 | 450.3 | 308.2 | 21.9 | 18.8 | 3.75 | 12.39 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap200-10m-r1` | OK | 2026-10-04 07:07 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 200 | - | 143,889.2 | 140,849.4 | 169,131.4 | 180,677.5 | 4.9 | 142.1 | 182.28 | 4.35 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap200-10m-r2` | OK | 2026-10-04 07:14 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 200 | - | 145,327.0 | 141,723.3 | 169,719.3 | 182,350.0 | 4.8 | 138.3 | 187.03 | 4.40 | AGREE | PASS | ACCEPT |
| `ozarchy-main-cap50-10m-r1` | OK | 2026-10-04 07:32 | harness @ 3d26f7f2 | 56c943ef | 10 | 120 | 50 | - | 39,972.5 | 36,697.7 | 72,427.4 | 50,514.3 | 16.5 | 44.5 | 39.50 | 5.35 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap20-10m-r1` | OK | 2026-10-04 07:56 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 20 | - | 75,425.4 | 74,262.6 | 80,550.1 | 94,603.6 | 14.2 | 17.9 | 57.79 | 5.89 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap400-10m-r1` | OK | 2026-10-04 08:15 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 400 | - | 175,232.5 | 173,799.2 | 206,656.8 | 219,842.6 | 2.4 | 221.0 | 372.39 | 3.84 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap50-10m-r1` | OK | 2026-10-04 08:02 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 50 | - | 105,125.7 | 104,658.3 | 117,999.1 | 131,902.6 | 8.8 | 41.5 | 96.69 | 5.28 | AGREE | PASS | ACCEPT |
| `ozarchy-rb-cap65-10m-r1` | OK | 2026-10-04 08:08 | bench-busy-retry @ 3d26f7f2 | 56c943ef | 10 | 120 | 65 | - | 110,207.0 | 109,457.4 | 126,384.2 | 138,233.8 | 7.3 | 51.5 | 115.17 | 5.21 | AGREE | PASS | ACCEPT |

## hostcheck: Host check: main `92a02ed` alone at 300 markets, do validators die on this host?

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-main-300m-hostcheck-c1` | OK | 2026-10-06 00:47 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 98,205.6 | 97,286.9 | 135,933.8 | 128,125.4 | 1.2 | 190.5 | 595.73 | 7.18 | AGREE | PASS | ACCEPT |

## 10m-gap: Main `92a02ed` 10-market profile cell (+ warm) for the gap-outside-the-engine analysis

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-main-prof-10m` | OK | 2026-10-06 00:03 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 176,150.1 | 174,662.6 | 210,063.1 | 220,991.1 | 2.2 | 234.7 | 369.41 | 3.80 | AGREE | PASS | ACCEPT |
| `ozarchy-main-prof-10m-warm` | OK | 2026-10-05 23:57 | main @ 92a02ed6 | 31a95c65 | 10 | 60 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 205,318.5 | 205,318.5 | 212,050.4 | 257,483.2 | 2.8 | 191.2 | 308.80 | 3.22 | AGREE | PASS | ACCEPT |

## mif: Bench in-flight cap sweep (`--max-in-flight` N = 1/2/4, `OPEN_ORDER_BUDGET` 900) on main, 300 markets

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-mif-300m-base` | OK | 2026-10-06 11:16 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 98,944.6 | 97,759.3 | 134,104.3 | 129,110.6 | 1.5 | 190.2 | 589.55 | 7.12 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-base-b900` | OK | 2026-10-06 11:30 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,303.4 | 98,174.1 | 144,795.7 | 130,972.7 | 1.6 | 206.3 | 509.05 | 7.14 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-base-r2` | OK | 2026-10-06 12:17 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 92,391.6 | 91,587.6 | 137,003.1 | 120,536.4 | 1.2 | 184.5 | 614.29 | 7.41 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-base-r3` | OK | 2026-10-06 12:30 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 97,726.2 | 96,838.1 | 132,801.7 | 127,442.1 | 1.5 | 184.4 | 576.31 | 7.15 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n1` | OK | 2026-10-06 11:36 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 144,128.5 | 145,241.8 | 167,434.9 | 187,948.9 | 7.1 | 55.4 | 357.60 | 5.07 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n1-b900` | OK | 2026-10-06 11:41 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 150,351.7 | 150,570.9 | 160,481.9 | 196,314.8 | 10.1 | 61.2 | 276.06 | 5.05 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n2` | OK | 2026-10-06 11:47 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 157,174.9 | 157,805.0 | 182,682.3 | 204,481.8 | 4.0 | 108.6 | 450.24 | 5.19 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n2-b900` | OK | 2026-10-06 11:53 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 172,970.9 | 173,411.8 | 182,706.8 | 225,671.5 | 7.3 | 94.9 | 259.98 | 4.49 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n4` | OK | 2026-10-06 11:59 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 149,793.3 | 150,196.2 | 171,161.2 | 194,969.5 | 2.3 | 173.5 | 461.79 | 5.49 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n4-b900` | OK | 2026-10-06 12:05 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 176,532.8 | 176,646.1 | 186,325.0 | 233,790.2 | 5.1 | 135.7 | 297.68 | 4.51 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n4-b900-r2` | OK | 2026-10-06 12:11 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 174,134.7 | 173,486.2 | 181,354.0 | 231,209.8 | 4.9 | 132.4 | 278.83 | 4.56 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-n4-b900-r3` | OK | 2026-10-06 12:24 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 172,239.5 | 171,962.5 | 185,344.3 | 228,465.0 | 5.0 | 134.5 | 266.88 | 4.55 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-tailcost` | OK | 2026-10-06 11:23 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,916.5 | 99,695.3 | 134,619.2 | 131,645.4 | 1.5 | 201.0 | 589.93 | 7.04 | AGREE | PASS | ACCEPT |
| `ozarchy-mif-300m-warm` | OK | 2026-10-06 11:09 | main @ 1cd786da | 31a95c65 | 300 | 60 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 110,489.0 | 110,489.0 | 133,938.0 | 144,289.8 | 1.3 | 169.2 | 507.97 | 6.19 | AGREE | PASS | ACCEPT |

## mif2a: In-flight cap sweep block A: N = 8 / 16 + budget 900, base on main

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-mif2-300m-base` | OK | 2026-10-06 13:13 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,711.3 | 98,965.3 | 130,940.3 | 131,419.6 | 1.3 | 204.9 | 580.36 | 7.12 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-n16-b900` | OK | 2026-10-06 13:25 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 172,348.7 | 171,370.1 | 181,551.7 | 229,002.3 | 5.4 | 120.3 | 251.46 | 4.61 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-n8-b900` | OK | 2026-10-06 13:20 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 176,249.7 | 176,491.4 | 186,780.6 | 233,272.1 | 5.4 | 131.7 | 285.33 | 4.52 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-warm` | OK | 2026-10-06 13:07 | main @ 1cd786da | 31a95c65 | 300 | 60 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 172,574.6 | 172,574.6 | 188,130.6 | 227,509.7 | 5.0 | 107.6 | 297.65 | 4.34 | AGREE | PASS | ACCEPT |

## mif2b: Crab (`59fa407`) vs main at N = 2 + budget 900, 300 markets

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-mif2-300m-crab-r1` | OK | 2026-10-06 13:37 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 179,860.7 | 178,565.8 | 188,963.6 | 234,639.3 | 7.4 | 96.3 | 110.16 | 4.16 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-crab-r2` | OK | 2026-10-06 13:48 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 183,357.4 | 182,409.8 | 190,402.2 | 239,310.2 | 7.7 | 91.3 | 102.50 | 4.14 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-crab-warm` | OK | 2026-10-06 13:31 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 60 | 400 | - | 185,026.7 | 185,026.7 | 192,922.1 | 241,546.4 | 7.5 | 79.2 | 89.53 | 4.07 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-main-r1` | OK | 2026-10-06 13:43 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 170,413.9 | 170,330.2 | 185,526.8 | 222,318.0 | 7.3 | 92.2 | 252.48 | 4.48 | AGREE | PASS | ACCEPT |
| `ozarchy-mif2-300m-main-r2` | OK | 2026-10-06 13:54 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 174,142.7 | 173,465.8 | 183,640.2 | 227,134.4 | 7.2 | 91.2 | 276.85 | 4.44 | AGREE | PASS | ACCEPT |

## mif3: Crab (`59fa407`) plateau at N = 4 / 8 + budget 900 (the standard shape)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-mif3-300m-crab-n4-b900` | OK | 2026-10-06 14:17 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 183,952.7 | 182,836.6 | 195,153.2 | 244,316.1 | 5.3 | 132.0 | 144.83 | 4.17 | AGREE | PASS | ACCEPT |
| `ozarchy-mif3-300m-crab-n8-b900` | OK | 2026-10-06 14:23 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 186,453.5 | 185,582.0 | 195,853.1 | 247,435.3 | 5.3 | 129.7 | 141.64 | 4.14 | AGREE | PASS | ACCEPT |
| `ozarchy-mif3-300m-crab-warm` | OK | 2026-10-06 14:11 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 60 | 400 | - | 189,669.7 | 189,669.7 | 197,460.3 | 250,504.9 | 4.8 | 108.9 | 128.39 | 4.08 | AGREE | PASS | ACCEPT |

## p2s0: Phase 2 step 0 profile at N=4 + budget 900: crab `59fa407` vs main, walk 0 / walk 10, perf on r2 / w10

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p2s0-300m-crab-r1` | OK | 2026-10-06 14:36 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 184,785.6 | 184,378.5 | 195,026.9 | 245,133.7 | 5.5 | 125.2 | 141.53 | 4.22 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0-300m-crab-r2` | OK | 2026-10-06 14:49 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 176,609.5 | 175,255.1 | 197,454.0 | 233,379.1 | 4.4 | 143.6 | 185.00 | 4.26 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0-300m-crab-w10` | OK | 2026-10-06 15:01 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 120 | 400 | - | 172,112.5 | 170,381.7 | 183,065.8 | 227,683.4 | 4.3 | 216.2 | 227.82 | 4.56 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0-300m-crab-warm` | OK | 2026-10-06 14:30 | main-59fa407 @ 59fa407b | 1cf9f647 | 300 | 60 | 400 | - | 183,555.4 | 183,555.4 | 198,359.6 | 243,020.3 | 5.0 | 115.6 | 133.72 | 4.12 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0-300m-main-r1` | OK | 2026-10-06 14:42 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 174,529.6 | 174,606.6 | 188,677.4 | 231,657.4 | 5.2 | 132.0 | 275.58 | 4.52 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0-300m-main-r2` | OK | 2026-10-06 14:55 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 167,084.2 | 165,862.7 | 179,709.0 | 220,547.5 | 4.5 | 137.8 | 319.73 | 4.66 | AGREE | PASS | ACCEPT |

## p2s0b: Phase 2 step 0 (Gate 0): step 0 `707f132f` vs base main `d3ba3c0a`, standard shape (N=4 + budget 900): counter overhead ABBA, s-prof (perf), s-byid, s-w10; plus a 10-market smoke cell

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p2s0b-300m-b-r1` | OK | 2026-10-08 03:26 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 169,294.6 | 169,189.1 | 179,506.5 | 226,250.6 | 5.1 | 126.1 | 152.16 | 4.51 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-b-r2` | OK | 2026-10-08 03:33 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 159,922.0 | 160,493.6 | 173,530.0 | 211,680.6 | 4.9 | 128.8 | 164.22 | 4.74 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-s-byid` | OK | 2026-10-08 03:56 | p2s0b-707f132f @ 707f132f | 0a742915 | 300 | 120 | 400 | - | 160,459.5 | 159,320.2 | 166,928.2 | 212,687.4 | 4.8 | 126.1 | 158.62 | 4.74 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-s-prof` | OK | 2026-10-08 03:49 | p2s0b-707f132f @ 707f132f | 0a742915 | 300 | 120 | 400 | - | 155,194.3 | 153,462.0 | 181,577.4 | 204,943.4 | 3.6 | 144.7 | 217.57 | 4.67 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-s-r1` | OK | 2026-10-08 03:19 | p2s0b-707f132f @ 707f132f | 0a742915 | 300 | 120 | 400 | - | 165,622.1 | 165,417.7 | 179,733.6 | 219,678.5 | 4.9 | 124.9 | 155.65 | 4.56 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-s-r2` | OK | 2026-10-08 03:42 | p2s0b-707f132f @ 707f132f | 0a742915 | 300 | 120 | 400 | - | 163,860.6 | 163,725.6 | 174,092.5 | 217,347.1 | 4.8 | 130.5 | 159.94 | 4.61 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-s-w10` | OK | 2026-10-08 04:03 | p2s0b-707f132f @ 707f132f | 0a742915 | 300 | 120 | 400 | - | 164,322.3 | 164,110.8 | 172,025.3 | 217,493.6 | 4.8 | 185.8 | 199.60 | 4.70 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-300m-s-warm` | OK | 2026-10-08 03:13 | p2s0b-707f132f @ 707f132f | 0a742915 | 300 | 60 | 400 | - | 166,463.9 | 166,463.9 | 181,175.1 | 219,533.8 | 4.9 | 102.7 | 145.98 | 4.36 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0b-smoke-10m-byid` | OK | 2026-10-08 03:08 | p2s0b-707f132f @ 707f132f | 0a742915 | 10 | 30 | 400 | - | 209,870.5 | 209,870.5 | 0.0 | 265,119.8 | 12.3 | 57.8 | 42.21 | 2.84 | AGREE | PASS | ACCEPT |

## p2s0r: Regression check main `d3ba3c0a` vs `35e69b3`: A `35e69b3`, B `d3ba3c0a`, C = B + 64 MiB book SSTs, D = `d3ba3c0a` combined build; A B C D D C B A, plus two 10-market smoke cells

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p2s0r-300m-a-r1` | OK | 2026-10-08 05:08 | main-35e69b3 @ 35e69b3d | 9f5c53bb | 300 | 120 | 400 | - | 181,932.9 | 181,608.1 | 193,919.6 | 241,886.9 | 5.4 | 129.2 | 142.38 | 4.29 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-a-r2` | OK | 2026-10-08 05:51 | main-35e69b3 @ 35e69b3d | 9f5c53bb | 300 | 120 | 400 | - | 178,052.2 | 176,997.8 | 191,220.8 | 236,876.5 | 5.2 | 127.1 | 148.51 | 4.34 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-b-r1` | OK | 2026-10-08 05:14 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 167,115.2 | 166,499.8 | 177,980.2 | 222,106.5 | 5.0 | 130.8 | 159.71 | 4.50 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-b-r2` | OK | 2026-10-08 05:45 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 170,144.9 | 168,992.4 | 180,547.3 | 225,919.0 | 4.9 | 137.8 | 161.03 | 4.42 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-b-warm` | OK | 2026-10-08 05:02 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 60 | 400 | - | 166,866.9 | 166,866.9 | 182,174.7 | 221,292.1 | 5.0 | 100.8 | 131.09 | 4.41 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-c-r1` | OK | 2026-10-08 05:20 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | TORUS_BOOK_CF_TARGET_FILE_MB=64 | 168,039.8 | 167,857.5 | 179,268.0 | 223,103.8 | 5.0 | 130.0 | 156.23 | 4.51 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-c-r2` | OK | 2026-10-08 05:39 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | TORUS_BOOK_CF_TARGET_FILE_MB=64 | 170,336.9 | 170,402.0 | 180,021.2 | 226,048.3 | 5.2 | 125.5 | 157.89 | 4.44 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-d-r1` | OK | 2026-10-08 05:26 | main-d3ba3c0a @ d3ba3c0a | d76b4427 | 300 | 120 | 400 | - | 172,577.4 | 171,838.4 | 181,040.6 | 228,932.3 | 5.1 | 133.0 | 153.64 | 4.40 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-300m-d-r2` | OK | 2026-10-08 05:33 | main-d3ba3c0a @ d3ba3c0a | d76b4427 | 300 | 120 | 400 | - | 169,287.3 | 169,471.8 | 179,836.0 | 224,793.7 | 5.2 | 120.7 | 150.84 | 4.40 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-smoke-10m-a` | OK | 2026-10-08 04:36 | main-35e69b3 @ 35e69b3d | 9f5c53bb | 10 | 30 | 400 | - | 233,487.4 | 233,487.4 | 0.0 | 294,462.8 | 8.4 | 72.4 | 61.67 | 2.91 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0r-smoke-10m-c` | OK | 2026-10-08 04:38 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 10 | 30 | 400 | TORUS_BOOK_CF_TARGET_FILE_MB=64 | 227,623.1 | 227,623.1 | 0.0 | 287,784.5 | 8.8 | 67.1 | 55.77 | 3.05 | AGREE | PASS | ACCEPT |

## p2s0x: Bisect of the section 26 regression: a `35e69b3`, p0 `8582e827`, p1 `a746c408`, p2 `2ebe1a14`, p3 `9e695364`, b `d3ba3c0a`; mirrored order

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p2s0x-300m-a-r1` | OK | 2026-10-08 13:32 | main-35e69b3 @ 35e69b3d | 7a66c678 | 300 | 120 | 400 | - | 185,495.5 | 185,470.0 | 193,507.1 | 245,607.8 | 5.4 | 136.5 | 139.07 | 4.20 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-a-r2` | OK | 2026-10-08 14:39 | main-35e69b3 @ 35e69b3d | 7a66c678 | 300 | 120 | 400 | - | 182,789.3 | 181,290.5 | 191,239.8 | 243,547.6 | 5.3 | 131.5 | 145.35 | 4.24 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-b-r1` | OK | 2026-10-08 14:02 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 171,985.0 | 170,650.2 | 180,215.4 | 227,799.4 | 4.8 | 130.8 | 155.48 | 4.44 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-b-r2` | OK | 2026-10-08 14:09 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 168,984.8 | 168,367.6 | 181,507.4 | 224,336.1 | 5.0 | 125.0 | 157.07 | 4.42 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-b-warm` | OK | 2026-10-08 13:27 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 60 | 400 | - | 175,334.5 | 175,334.5 | 186,183.3 | 232,058.6 | 4.7 | 111.6 | 133.66 | 4.29 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p0-r1` | OK | 2026-10-08 13:38 | bisect-8582e827 @ 8582e827 | 58ebd01a | 300 | 120 | 400 | - | 183,251.8 | 183,029.9 | 194,873.4 | 243,420.1 | 5.5 | 128.5 | 146.16 | 4.22 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p0-r2` | OK | 2026-10-08 14:34 | bisect-8582e827 @ 8582e827 | 58ebd01a | 300 | 120 | 400 | - | 180,164.3 | 179,530.0 | 191,366.3 | 238,908.9 | 5.4 | 129.1 | 145.29 | 4.28 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p1-r1` | OK | 2026-10-08 13:44 | bisect-a746c408 @ a746c408 | 41b606d3 | 300 | 120 | 400 | - | 179,508.8 | 177,495.3 | 190,848.6 | 238,591.1 | 5.5 | 122.9 | 137.45 | 4.20 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p1-r2` | OK | 2026-10-08 14:27 | bisect-a746c408 @ a746c408 | 41b606d3 | 300 | 120 | 400 | - | 173,414.7 | 172,473.1 | 185,907.2 | 231,089.6 | 5.0 | 129.9 | 151.95 | 4.37 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p2-r1` | OK | 2026-10-08 13:50 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 171,552.4 | 171,231.9 | 184,611.4 | 227,297.8 | 5.2 | 136.4 | 156.28 | 4.32 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p2-r2` | OK | 2026-10-08 14:22 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 178,988.5 | 177,640.5 | 189,515.7 | 237,865.1 | 5.3 | 134.2 | 152.57 | 4.24 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p3-r1` | OK | 2026-10-08 13:57 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 167,546.6 | 167,100.9 | 178,333.1 | 222,723.6 | 5.0 | 127.9 | 152.87 | 4.45 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0x-300m-p3-r2` | OK | 2026-10-08 14:15 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 170,863.4 | 170,862.2 | 179,684.5 | 226,892.5 | 5.1 | 135.4 | 157.37 | 4.46 | AGREE | PASS | ACCEPT |

## p2s0y: Perf A/B p2 `2ebe1a14` vs p3 `9e695364` (prof and xstat cells); `p2-warm`, `p2-rec1`, `p3-rec1` are the first launch (5-event inherited `perf record`, distorted, reference only)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p2s0y-300m-p2-prof1` | OK | 2026-10-08 16:09 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 165,523.9 | 165,309.8 | 188,566.4 | 218,708.8 | 4.1 | 150.3 | 210.08 | 4.45 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p2-prof2` | OK | 2026-10-08 16:28 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 165,228.0 | 165,196.1 | 191,308.4 | 218,385.0 | 4.0 | 156.0 | 212.88 | 4.37 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p2-rec1` | OK | 2026-10-08 15:51 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 109,445.5 | 110,030.9 | 159,640.7 | 144,102.6 | 2.6 | 156.1 | 314.41 | 5.64 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p2-warm` | OK | 2026-10-08 15:44 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 60 | 400 | - | 175,115.7 | 175,115.7 | 187,748.3 | 231,473.4 | 4.7 | 99.2 | 136.02 | 4.24 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p2-warm2` | OK | 2026-10-08 16:02 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 60 | 400 | - | 170,799.2 | 170,799.2 | 188,076.0 | 225,985.8 | 4.6 | 110.4 | 157.61 | 4.19 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p2-xstat1` | OK | 2026-10-08 16:34 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 172,823.4 | 172,405.6 | 185,544.1 | 230,168.4 | 5.2 | 137.2 | 150.04 | 4.34 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p2-xstat2` | OK | 2026-10-08 16:52 | bisect-2ebe1a14 @ 2ebe1a14 | b7798354 | 300 | 120 | 400 | - | 177,606.5 | 177,373.2 | 188,438.0 | 235,633.7 | 5.2 | 138.9 | 151.28 | 4.25 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p3-prof1` | OK | 2026-10-08 16:15 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 166,815.9 | 166,556.2 | 192,503.0 | 220,490.9 | 4.0 | 158.0 | 214.33 | 4.41 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p3-prof2` | OK | 2026-10-08 16:22 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 162,123.3 | 160,762.4 | 193,712.0 | 213,780.2 | 3.8 | 149.1 | 216.56 | 4.47 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p3-rec1` | OK | 2026-10-08 15:57 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 110,853.3 | 108,984.6 | 160,763.6 | 145,679.9 | 2.4 | 149.8 | 321.82 | 5.68 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p3-xstat1` | OK | 2026-10-08 16:41 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 169,545.4 | 168,473.1 | 179,766.7 | 224,968.8 | 4.9 | 129.9 | 154.62 | 4.44 | AGREE | PASS | ACCEPT |
| `ozarchy-p2s0y-300m-p3-xstat2` | OK | 2026-10-08 16:46 | bisect-9e695364 @ 9e695364 | 2549ecdf | 300 | 120 | 400 | - | 173,464.7 | 173,072.6 | 180,380.9 | 230,372.4 | 5.0 | 129.1 | 154.41 | 4.32 | AGREE | PASS | ACCEPT |

## c2h: C2 holder-index fix and Position v2 savings A/B vs `d3ba3c0a`: b `d3ba3c0a`, base `bf2edda6`, fix `d7bd1c36` (built from `c051872b`), sav `37b28dd6`, both `f1ab2166`; b base fix sav both both sav fix base b, plus c2h-x pair sav-r3 base-r3; `sav-r1` failed (rc=2, host-wide RPC stall), excluded from the arm means

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-c2h-300m-b-r1` | OK | 2026-10-08 17:57 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 173,407.1 | 172,204.7 | 184,308.5 | 229,829.5 | 5.2 | 119.4 | 142.31 | 4.39 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-b-r2` | OK | 2026-10-08 18:52 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 120 | 400 | - | 174,680.5 | 174,554.7 | 186,820.3 | 231,920.0 | 5.1 | 126.2 | 154.79 | 4.34 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-b-warm` | OK | 2026-10-08 17:51 | main-d3ba3c0a @ d3ba3c0a | 193ae781 | 300 | 60 | 400 | - | 173,007.6 | 173,007.6 | 187,010.4 | 229,321.2 | 4.7 | 101.8 | 133.68 | 4.24 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-base-r1` | OK | 2026-10-08 18:03 | c2-set-holder-base @ bf2edda6 | 29860dc0 | 300 | 120 | 400 | - | 177,394.3 | 176,203.4 | 186,606.4 | 235,641.9 | 5.3 | 134.5 | 154.84 | 4.28 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-base-r2` | OK | 2026-10-08 18:45 | c2-set-holder-base @ bf2edda6 | 29860dc0 | 300 | 120 | 400 | - | 173,527.9 | 172,727.2 | 186,905.1 | 230,250.1 | 5.1 | 126.3 | 151.45 | 4.33 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-base-r3` | OK | 2026-10-08 19:12 | c2-set-holder-base @ bf2edda6 | 29860dc0 | 300 | 120 | 400 | - | 176,773.9 | 175,904.1 | 185,885.3 | 235,050.9 | 5.3 | 135.4 | 150.21 | 4.27 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-base-warm2` | OK | 2026-10-08 19:00 | c2-set-holder-base @ bf2edda6 | 29860dc0 | 300 | 60 | 400 | - | 173,138.1 | 173,138.1 | 184,856.4 | 229,103.3 | 4.6 | 98.5 | 135.51 | 4.29 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-both-r1` | OK | 2026-10-08 18:21 | c2-v2-both @ f1ab2166 | 17ac7525 | 300 | 120 | 400 | - | 180,618.7 | 179,826.4 | 191,706.9 | 240,305.8 | 5.3 | 136.8 | 148.87 | 4.23 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-both-r2` | OK | 2026-10-08 18:27 | c2-v2-both @ f1ab2166 | 17ac7525 | 300 | 120 | 400 | - | 177,660.3 | 177,354.6 | 190,390.0 | 235,624.8 | 5.1 | 124.8 | 150.43 | 4.30 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-fix-r1` | OK | 2026-10-08 18:09 | c2-set-holder @ c051872b | b843f522 | 300 | 120 | 400 | - | 173,911.6 | 173,507.6 | 186,159.6 | 231,733.2 | 5.2 | 131.8 | 150.97 | 4.40 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-fix-r2` | OK | 2026-10-08 18:40 | c2-set-holder @ c051872b | b843f522 | 300 | 120 | 400 | - | 177,891.7 | 176,949.1 | 189,261.7 | 235,688.6 | 5.3 | 134.2 | 149.31 | 4.38 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-sav-r1` | UNVERIFIED | 2026-10-08 18:16 | v2-savings @ 37b28dd6 | 4fcbf7cb | 300 | 120 | 400 | - | 135,694.4 | 133,782.9 | 171,276.0 | 178,545.7 | 3.4 | 126.0 | 169.37 | 3.98 | AGREE | UNKNOWN | UNVERIFIED |
| `ozarchy-c2h-300m-sav-r2` | OK | 2026-10-08 18:33 | v2-savings @ 37b28dd6 | 4fcbf7cb | 300 | 120 | 400 | - | 178,036.6 | 177,179.0 | 189,469.0 | 236,627.5 | 5.1 | 131.6 | 150.37 | 4.17 | AGREE | PASS | ACCEPT |
| `ozarchy-c2h-300m-sav-r3` | OK | 2026-10-08 19:06 | v2-savings @ 37b28dd6 | 4fcbf7cb | 300 | 120 | 400 | - | 176,912.5 | 176,991.8 | 188,521.7 | 234,400.6 | 5.1 | 130.9 | 154.80 | 4.23 | AGREE | PASS | ACCEPT |

## p2byid: Cancel-by-id cost with P2-1 in: p2 `bdd5b470` vs ref main `e934fa0e`, by-id cells (bench flags `--cancel-by-id-fraction 0.1 --modify-fraction 0.05`, not in extra env) and std cells; `ref-warm` excluded

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p2byid-300m-p2-byid-r1` | OK | 2026-10-09 00:54 | p2byid-bdd5b470 @ bdd5b470 | e28bb121 | 300 | 120 | 400 | - | 176,114.0 | 175,254.7 | 185,940.9 | 232,517.3 | 5.1 | 139.4 | 150.19 | 4.40 | AGREE | PASS | ACCEPT |
| `ozarchy-p2byid-300m-p2-byid-r2` | OK | 2026-10-09 01:11 | p2byid-bdd5b470 @ bdd5b470 | e28bb121 | 300 | 120 | 400 | - | 177,730.6 | 177,967.5 | 187,942.1 | 235,498.8 | 5.3 | 127.5 | 149.06 | 4.37 | AGREE | PASS | ACCEPT |
| `ozarchy-p2byid-300m-p2-std-r1` | OK | 2026-10-09 01:00 | p2byid-bdd5b470 @ bdd5b470 | e28bb121 | 300 | 120 | 400 | - | 179,851.2 | 177,949.1 | 187,550.4 | 238,403.1 | 5.5 | 129.9 | 146.09 | 4.27 | AGREE | PASS | ACCEPT |
| `ozarchy-p2byid-300m-ref-byid-r1` | OK | 2026-10-09 00:48 | p2byid-e934fa0e @ e934fa0e | 8d7d596c | 300 | 120 | 400 | - | 179,228.2 | 178,558.5 | 190,142.6 | 238,251.5 | 5.4 | 127.8 | 143.40 | 4.33 | AGREE | PASS | ACCEPT |
| `ozarchy-p2byid-300m-ref-byid-r2` | OK | 2026-10-09 01:18 | p2byid-e934fa0e @ e934fa0e | 8d7d596c | 300 | 120 | 400 | - | 173,816.0 | 172,096.1 | 186,523.8 | 230,660.5 | 5.5 | 127.9 | 140.64 | 4.38 | AGREE | PASS | ACCEPT |
| `ozarchy-p2byid-300m-ref-std-r1` | OK | 2026-10-09 01:05 | p2byid-e934fa0e @ e934fa0e | 8d7d596c | 300 | 120 | 400 | - | 179,192.3 | 177,854.4 | 189,745.5 | 238,030.9 | 5.5 | 122.1 | 140.57 | 4.29 | AGREE | PASS | ACCEPT |
| `ozarchy-p2byid-300m-ref-warm` | OK | 2026-10-09 00:42 | p2byid-e934fa0e @ e934fa0e | 8d7d596c | 300 | 60 | 400 | - | 182,389.8 | 182,389.8 | 193,182.4 | 241,955.8 | 5.0 | 100.2 | 126.34 | 4.17 | AGREE | PASS | ACCEPT |

## p25g: P2-5 hasher gate cell: p25 `631becaa` vs ref `bdd5b470`; `ref-warm` excluded

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p25g-300m-p25-r1` | OK | 2026-10-09 01:59 | p25g-631becaa @ 631becaa | 2587e57f | 300 | 120 | 400 | - | 186,413.8 | 186,233.2 | 194,309.7 | 248,015.2 | 5.6 | 129.3 | 139.70 | 3.98 | AGREE | PASS | ACCEPT |
| `ozarchy-p25g-300m-p25-r2` | OK | 2026-10-09 02:05 | p25g-631becaa @ 631becaa | 2587e57f | 300 | 120 | 400 | - | 187,902.0 | 187,707.6 | 200,267.7 | 249,839.6 | 5.6 | 138.7 | 145.27 | 4.04 | AGREE | PASS | ACCEPT |
| `ozarchy-p25g-300m-ref-r1` | OK | 2026-10-09 01:53 | p25g-bdd5b470 @ bdd5b470 | e28bb121 | 300 | 120 | 400 | - | 180,588.6 | 179,833.6 | 191,864.7 | 239,965.3 | 5.3 | 129.6 | 148.38 | 4.26 | AGREE | PASS | ACCEPT |
| `ozarchy-p25g-300m-ref-r2` | OK | 2026-10-09 02:11 | p25g-bdd5b470 @ bdd5b470 | e28bb121 | 300 | 120 | 400 | - | 180,762.5 | 180,347.8 | 192,192.0 | 239,741.7 | 5.2 | 136.6 | 149.90 | 4.29 | AGREE | PASS | ACCEPT |
| `ozarchy-p25g-300m-ref-warm` | OK | 2026-10-09 01:46 | p25g-bdd5b470 @ bdd5b470 | e28bb121 | 300 | 60 | 400 | - | 180,472.0 | 180,472.0 | 192,347.5 | 238,689.9 | 4.7 | 111.2 | 139.95 | 4.17 | AGREE | PASS | ACCEPT |

## p22g: P2-2 batch cache flush gate cell: p22 `29320f6b` vs p25 `029581e5`, plus base main `e934fa0e` (cumulative); `base-warm` excluded

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-p22g-300m-base-r1` | OK | 2026-10-09 03:01 | p22g-e934fa0e @ e934fa0e | 8d7d596c | 300 | 120 | 400 | - | 180,885.8 | 180,119.7 | 191,451.5 | 240,014.3 | 5.2 | 128.4 | 149.00 | 4.31 | AGREE | PASS | ACCEPT |
| `ozarchy-p22g-300m-base-r2` | OK | 2026-10-09 03:32 | p22g-e934fa0e @ e934fa0e | 8d7d596c | 300 | 120 | 400 | - | 181,749.3 | 180,960.8 | 193,540.8 | 241,509.1 | 5.5 | 125.6 | 143.65 | 4.22 | AGREE | PASS | ACCEPT |
| `ozarchy-p22g-300m-base-warm` | OK | 2026-10-09 02:55 | p22g-e934fa0e @ e934fa0e | 8d7d596c | 300 | 60 | 400 | - | 183,625.6 | 183,625.6 | 191,948.6 | 242,706.7 | 5.0 | 111.9 | 138.19 | 4.23 | AGREE | PASS | ACCEPT |
| `ozarchy-p22g-300m-p22-r1` | OK | 2026-10-09 03:14 | p22g-29320f6b @ 29320f6b | 5639b084 | 300 | 120 | 400 | - | 189,882.1 | 188,346.7 | 198,899.2 | 251,758.8 | 5.5 | 134.8 | 143.07 | 4.01 | AGREE | PASS | ACCEPT |
| `ozarchy-p22g-300m-p22-r2` | OK | 2026-10-09 03:20 | p22g-29320f6b @ 29320f6b | 5639b084 | 300 | 120 | 400 | - | 186,740.1 | 185,188.5 | 198,284.5 | 247,552.2 | 5.6 | 127.2 | 137.34 | 4.07 | AGREE | PASS | ACCEPT |
| `ozarchy-p22g-300m-p25-r1` | OK | 2026-10-09 03:07 | p22g-029581e5 @ 029581e5 | 2587e57f | 300 | 120 | 400 | - | 190,714.8 | 189,834.1 | 204,010.7 | 253,241.3 | 5.8 | 130.7 | 135.15 | 3.98 | AGREE | PASS | ACCEPT |
| `ozarchy-p22g-300m-p25-r2` | OK | 2026-10-09 03:26 | p22g-029581e5 @ 029581e5 | 2587e57f | 300 | 120 | 400 | - | 189,881.2 | 188,869.5 | 197,942.2 | 251,612.5 | 5.6 | 137.1 | 138.33 | 3.97 | AGREE | PASS | ACCEPT |

## pf1: PF1 gate at 10 markets: `0ebfd71` (crab + PF1) vs main `92a02ed`

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-pf1-r1` | OK | 2026-10-04 20:02 | item6-pf1 @ 0ebfd71d | 518464bd | 10 | 120 | 400 | - | 127,917.4 | 126,881.7 | 150,556.0 | 161,060.3 | 1.7 | 242.2 | 461.77 | 5.55 | AGREE | PASS | ACCEPT |
| `ozarchy-pf1-r2` | OK | 2026-10-04 20:15 | item6-pf1 @ 0ebfd71d | 518464bd | 10 | 120 | 400 | - | 127,003.3 | 125,254.9 | 151,512.7 | 159,874.4 | 1.7 | 226.7 | 454.15 | 5.58 | AGREE | PASS | ACCEPT |
| `ozarchy-pf1-warm` | OK | 2026-10-04 19:56 | item6-pf1 @ 0ebfd71d | 518464bd | 10 | 60 | 400 | - | 158,702.5 | 158,702.5 | 167,316.1 | 199,842.0 | 1.8 | 181.6 | 388.46 | 4.55 | AGREE | PASS | ACCEPT |
| `ozarchy-pf1main-r1` | OK | 2026-10-04 20:09 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 178,156.0 | 176,985.0 | 207,028.4 | 223,482.7 | 2.5 | 212.2 | 362.23 | 3.73 | AGREE | PASS | ACCEPT |
| `ozarchy-pf1main-r2` | OK | 2026-10-04 20:21 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 173,650.8 | 173,948.7 | 207,329.1 | 217,835.8 | 2.3 | 221.5 | 373.83 | 3.86 | AGREE | PASS | ACCEPT |

## prof10: Exec-path CPU profile at 10 markets: crab `d52a33f` vs main `92a02ed` (perf on val0)

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-prof10-crab` | OK | 2026-10-04 19:07 | item6-phase1 @ d52a33f5 | ea86be76 | 10 | 120 | 400 | - | 65,912.7 | 65,769.1 | 84,164.7 | 82,981.2 | 0.7 | 145.0 | 1,347.81 | 16.44 | AGREE | PASS | ACCEPT |
| `ozarchy-prof10-main` | OK | 2026-10-04 19:12 | main @ 92a02ed6 | 31a95c65 | 10 | 120 | 400 | - | 174,778.4 | 173,786.8 | 208,506.4 | 219,263.8 | 2.3 | 220.0 | 379.56 | 3.88 | AGREE | PASS | ACCEPT |
| `ozarchy-prof10-warm` | OK | 2026-10-04 19:01 | item6-phase1 @ d52a33f5 | ea86be76 | 10 | 60 | 400 | - | 85,097.5 | 85,097.5 | 86,462.3 | 107,113.5 | 0.8 | 130.0 | 1,029.58 | 12.92 | AGREE | PASS | ACCEPT |

## s2: Item 6 sync point 2: `perf/item6-phase1` (`81a9567`) vs `merge/item6-sync2` (`cea1254`), plus a 256-key spam cell at cap 20

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-s2-item6-r1` | OK | 2026-10-04 16:38 | item6-phase1 @ 81a95677 | 252ce826 | 10 | 120 | 400 | - | 63,797.3 | 63,915.4 | 81,712.6 | 80,297.4 | 0.7 | 140.7 | 1,345.42 | 16.57 | AGREE | PASS | ACCEPT |
| `ozarchy-s2-item6-r2` | OK | 2026-10-04 16:51 | item6-phase1 @ 81a95677 | 252ce826 | 10 | 120 | 400 | - | 63,970.7 | 64,188.2 | 81,180.4 | 80,558.7 | 0.7 | 131.4 | 1,348.50 | 16.60 | AGREE | PASS | ACCEPT |
| `ozarchy-s2-sync2-r1` | OK | 2026-10-04 16:44 | item6-sync2 @ cea1254e | 8c70d658 | 10 | 120 | 400 | - | 64,019.8 | 63,714.3 | 82,729.8 | 80,626.9 | 0.8 | 148.1 | 1,164.07 | 16.19 | AGREE | PASS | ACCEPT |
| `ozarchy-s2-sync2-r2` | OK | 2026-10-04 16:57 | item6-sync2 @ cea1254e | 8c70d658 | 10 | 120 | 400 | - | 64,585.3 | 64,581.6 | 82,622.6 | 81,269.1 | 0.7 | 141.4 | 1,284.46 | 15.97 | AGREE | PASS | ACCEPT |
| `ozarchy-s2-sync2-spam256-cap20` | OK | 2026-10-04 17:02 | item6-sync2 @ cea1254e | 8c70d658 | 10 | 120 | 20 | - | 40,558.2 | 40,421.7 | 46,142.7 | 50,943.5 | 9.4 | 18.5 | 76.57 | 20.46 | AGREE | PASS | ACCEPT |
| `ozarchy-s2-warm` | OK | 2026-10-04 16:31 | item6-sync2 @ cea1254e | 8c70d658 | 10 | 60 | 400 | - | 77,687.6 | 77,687.6 | 81,783.7 | 97,887.1 | 0.7 | 127.1 | 1,132.37 | 13.72 | AGREE | PASS | ACCEPT |

## trie0: Trie maintenance off at 300 markets: crab `51051c9` vs main `92a02ed`

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-trie0-crab-r1` | OK | 2026-10-05 02:08 | item6-phase1 @ 51051c97 | 7da38063 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 51,871.1 | 51,627.2 | 80,659.0 | 75,023.6 | 0.7 | 150.3 | 920.39 | 13.86 | AGREE | PASS | ACCEPT |
| `ozarchy-trie0-crab-r2` | OK | 2026-10-05 02:21 | item6-phase1 @ 51051c97 | 7da38063 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 50,398.2 | 49,855.9 | 80,708.5 | 73,077.3 | 0.9 | 133.9 | 850.17 | 14.09 | AGREE | PASS | ACCEPT |
| `ozarchy-trie0-main-r1` | OK | 2026-10-05 02:15 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,744.1 | 100,068.4 | 129,760.6 | 131,455.5 | 1.6 | 185.6 | 559.50 | 7.05 | AGREE | PASS | ACCEPT |
| `ozarchy-trie0-main-r2` | OK | 2026-10-05 02:28 | main @ 92a02ed6 | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 100,401.3 | 98,961.2 | 132,426.3 | 131,034.3 | 1.6 | 184.2 | 568.45 | 7.07 | AGREE | PASS | ACCEPT |
| `ozarchy-trie0-warm` | OK | 2026-10-05 02:01 | item6-phase1 @ 51051c97 | 7da38063 | 300 | 60 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 57,056.8 | 57,056.8 | 86,379.4 | 82,705.3 | 1.1 | 134.3 | 644.59 | 11.67 | AGREE | PASS | ACCEPT |

## walk: Moving prices (walk 10 bp vs 0) at 300 markets, crab-side `5584880` vs main

| cell dir | status | generated | worktree @ commit | node md5 | markets | dur s | block cap | extra env | matched/s avg | first120 | best60 | placed/s | blk/s | txs/blk | chain ms | engine ms/1k fills | agreement | liveness | validity |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `ozarchy-walk-5584880-300m-m-r1` | OK | 2026-10-06 10:02 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 99,799.1 | 99,309.8 | 133,513.1 | 130,188.3 | 1.3 | 191.6 | 579.61 | 7.03 | AGREE | PASS | ACCEPT |
| `ozarchy-walk-5584880-300m-m-r2` | OK | 2026-10-06 10:22 | main @ 1cd786da | 31a95c65 | 300 | 120 | 400 | TORUS_NATIVE_TRIE_MAINTENANCE=0 | 97,025.2 | 95,746.1 | 135,704.8 | 126,497.9 | 1.5 | 201.4 | 586.05 | 7.14 | AGREE | PASS | ACCEPT |
| `ozarchy-walk-5584880-300m-w0-r1` | OK | 2026-10-06 09:56 | feed-drain @ ffc245e4 | 721e48d7 | 300 | 120 | 400 | - | 110,139.2 | 109,570.2 | 132,075.5 | 143,936.2 | 1.5 | 174.5 | 226.48 | 6.27 | AGREE | PASS | ACCEPT |
| `ozarchy-walk-5584880-300m-w0-r2` | OK | 2026-10-06 10:15 | feed-drain @ ffc245e4 | 721e48d7 | 300 | 120 | 400 | - | 112,585.7 | 111,467.0 | 132,006.2 | 147,163.3 | 1.5 | 197.9 | 256.65 | 6.21 | AGREE | PASS | ACCEPT |
| `ozarchy-walk-5584880-300m-w10-r1` | OK | 2026-10-06 09:49 | feed-drain @ ffc245e4 | 721e48d7 | 300 | 120 | 400 | - | 107,806.4 | 106,593.8 | 123,931.8 | 140,906.4 | 1.4 | 179.1 | 246.37 | 6.62 | AGREE | PASS | ACCEPT |
| `ozarchy-walk-5584880-300m-w10-r2` | OK | 2026-10-06 10:09 | feed-drain @ ffc245e4 | 721e48d7 | 300 | 120 | 400 | - | 106,297.4 | 106,281.3 | 125,707.2 | 138,962.1 | 1.5 | 134.7 | 181.48 | 6.65 | AGREE | PASS | ACCEPT |
| `ozarchy-walk-5584880-300m-warm` | OK | 2026-10-06 09:43 | feed-drain @ ffc245e4 | 721e48d7 | 300 | 60 | 400 | - | 121,905.2 | 121,905.2 | 144,727.2 | 159,743.8 | 1.4 | 163.6 | 192.26 | 5.43 | AGREE | PASS | ACCEPT |
