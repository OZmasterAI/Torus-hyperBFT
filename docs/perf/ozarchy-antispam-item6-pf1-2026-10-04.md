# ozarchy 2026-10-04: anti-spam, item 6 sync 2, C1 full node, 10-market profile, PF1, C3 + PF1, 14236fa baseline, trie and gap analyses, C6 + C7, margin phase breakdown, `239ff69`, per-action results (C), M1 (`90a752c`), step 2 window, Gate 2 at 10 markets (`c58775f`, `5524646`), 10-market gap outside the engine, step 2 at 300 markets (`4acdc59`), Gate 2 with B-blind (`31cea69`), live-feed idle check and reject share (`5584880`), moving prices and the bench in-flight cap (`5584880`, `59fa407`), Phase 2 step 0 profile, s94 batch cost and liquidation stress (`35e69b3`)

Host ozarchy (Ryzen 9 5950X, 32 threads, 62 GB; 3 validators + bench on one
host). Raw data in `~/bench-results-matched/` on ozarchy (paths per section).
All cells 3 validators, 10 markets, rate 76,000, `RETRY_BUSY=1`, 120 s unless
noted. All cells AGREE, liveness PASS, dissemination clean. With 2 cells per
arm only differences above ~5% are resolved (per-cell noise ~1-4%).

## Summary

| # | Question | Answer |
|---|---|---|
| 1 | Does native anti-spam (main `92a02ed`) cost throughput? | No: -0.85% vs `79a3752` at cap 400 |
| 2 | Does merging anti-spam into item 6 cost throughput? Do oracle prices survive cancel spam? | No: +0.7%. Yes: 198/198 submissions in, marks 0 blocks old with the pool full |
| 3 | Does C1's ubench gain show on a full node at 10 markets? | No: +3.3% (within noise); engine 16.7 vs 16.6 ms/1k fills |
| 4 | Where does crab's extra ~10 ms/1k fills go at 10 markets? | One function, `same_batch_bid_top_ups`: 11.2 ms CPU/1k, 49% of crab's exec path |
| 5 | Does PF1 (`0ebfd71`) remove it? | Yes: 11.2 -> 0.02 ms/1k; matched/s 65.9k -> 127.5k (1.93x), 0.72x main |
| 6 | C3 + PF1 (`d9ef4f7`): is the 10-market gap the bench's resend loop? Where does 300 markets go? Gate 3? | Not the resend loop (0.75x with retry, 0.66x without). 300 markets 0.555x main: margin + liquidation = `positions_for_trader` scans. Gate 3 margin FAIL (2.15 > 1.5), match PASS (1.32) |
| 7 | Baseline `14236fa` (C4): what changed, what is left? | 300 markets unchanged (49.4k, 0.553x main); liquidation 3.1-3.4 ms/1k (-15%), biggest crab-only cost `maker_fill_fits` 4.2-4.5. ubench ratios to `9c4be2c` all past their fail lines except settle. Empty block 30.0 ms, 10.7 of it trie flush |
| 8 | Is the native trie root used? What is left after C6 / C7? | Trie root: no production reader; off by default saves ~17 ms per empty block and ~4 ms/1k on the flush worker (owner question open). After C6 / C7: crab ~0.58x main estimated; ~6 ms/1k of named fixes reach ~0.78x, the rest needs an IPC measurement |
| 9 | Does the crab / main ratio change with the trie off? Is the shared-code inflation cache misses? | Trie off: crab +3.4%, main +12.5%, ratio 0.553x -> 0.508x (main was partly flush-bound). Inflation is 1.2-2.5x more instructions per fill, not lower IPC |
| 10 | What do C6 + C7 (`82bd1a4`) give at 300 markets, trie off? | 64.2k matched/s, +25.7% vs `14236fa`: **0.638x main** (was 0.508x). Engine 14.0 -> 10.2 ms/1k; state reads 8.3 -> 1.7, liquidation 3.2 -> 0.9, `maker_fill_fits` 4.3 -> 1.5 ms/1k. Left: margin phase 216-221 vs ~95 ms/blk on main |
| 11 | Where does C7's margin phase go vs main? What can be cut? | 5.09 vs 1.47 ms CPU/1k fills (+3.6; ENGINE wall 3.96 vs 1.46). 2.9 is crab-only code: the F1 account check in `prepare_one` 1.77, top-ups / pool takers / bid floors 1.15. `HashMap` work is +1.6 of the gap. `AccountReader::get_position` is ~0.15 margin, ~1.1 match timer. Named cuts ~1.7-2.7 ms/1k (est.), almost all in 18c's files |
| 12 | What do P1-P4 + fix A (`239ff69`) give at 300 markets? | 65.2k matched/s, **0.648x main** (was 0.638x): +1.5%, within 82bd1a4's cell spread; engine 10.2 -> 9.93 ms/1k (-2.7%, both cells). `checked_mul` -77%, matching -15%, margin buckets -14%, settle -17%; +0.5 ms/1k SipHash from the new Address-keyed `TraderMargins` map. Margin still 207-212 vs ~96 ms/blk on main |
| 13 | What does C (per-action failure records, `9195c32`) cost? How many failures does the bench produce? | No measurable cost (warm pair: matched/s +0.9%, engine -1.2%, pipelined 128 vs 127 ms). Failures are **not** ~0: 147 failed actions per native block (52% of actions, nearly all batches with open-limit rejects), ~10 KB per block, ~0.14% of the flush batch; identical on all 3 validators. Zero-fill IOCs and crossing post-only orders still show "executed" |
| 14 | What does M1 (`90a752c`) give at 300 markets? What does end_resident cost? | 76.5k matched/s, **+17%, 0.760x main** (was 0.648x); engine 9.93 -> 7.72 ms/1k (1.09x main). Margin 210 -> 125 ms/blk (-38% per fill), match 92 -> 45 (-49%). end_resident is untimed; from the profile **+29.7 ms/blk** (67 -> 97), ~3.5x the expected 8.6, mostly `BlockSums::into_cache` re-decoding positions |
| 15 | How much of end_resident can move off the exec thread (step 2)? Does `c58775f` pass Gate 2 at 10 markets? | Today's window end_resident(N) -> begin_resident(N+1) is ~23 ms wall, hiding only 10-24 of ~106 ms; moving begin_resident(N+1) to just before `new_env` widens it to ~100 ms and hides 76-95. The bench is exec-bound (no idle). 10 markets: **0.893x main** (both trie off, interleaved; pairs 0.885x / 0.901x), up from PF1's 0.72x; **Gate 2 (>= 0.9x) not met by ~1%**, within the cell spread |
| 16 | Does `5524646` (end_resident timers, sums reuse decoded positions) pass Gate 2 at 10 markets without perf on either arm? | **0.866x main** (pairs 0.877x / 0.855x; both trie off, interleaved): **Gate 2 not met**. Perf off did not help (crab 152.8k vs 156.2k profiled in 15). Gap unchanged: native blk/s 2.01 vs 2.24, views 395 vs 335 ms. end_resident ~5.8 ms/blk (rows 3.4, positions 1.8), ~6 of the ~18 ms/blk untimed excess. `run-cell.sh` `WIDE_COLS` lacks the timer columns |
| 17 | Where does the 10-market gap outside the engine go (views, RPC / gossip / ingress CPU, untimed exec)? | Nowhere outside execution: in the load window crab's exec block is +51.5 ms (485.5 vs 434.0) at equal fills per block. Views are exec-paced (64-deep channel fills 25-30 s earlier; free-running views 314-330 vs 325-348 ms). RPC +3.5 ms/1k is 2.3x more refused requests (equal CPU per request); signature verify equal per action. Untimed +23 ms/blk is mostly C's `native_failures` (`canonical_bytes`, ~20 ms CPU) plus end_resident (~7). Cutting those two and two small items: ~0.92x main (est.) |
| 18 | Does step 2 (`4acdc59`: end_resident on a worker, begin_resident before `new_env`) pass Gate 2 at 300 markets? | **0.832x main** (pairs 0.819x / 0.844x; interleaved, both trie off), up from 0.760x (section 14): **Gate 2 not met**. Step 2 hides ~92% of end_resident (exec wait 4.8-5.3 ms per block, p50 0.4, p90 16-22, vs ~15 estimated); `residual_untimed` -56 ms per block; no visible cost to verify. Per block crab is now level with or faster than main (chain 548 vs 601 ms); the gap is 0.77x fills per native block (matched / placed 0.70 vs 0.77, fewer placed per action), present since section 9 |
| 19 | Does B-blind (`31cea69`: non-pool sell top-up replaces the same-batch bound, on top of cuts 1/2/5/6) pass Gate 2 at both shapes? Is option A needed? | **Yes at both: 1.097x main at 300 markets** (pairs 1.102x / 1.092x; was 0.832x) **and 0.997x at 10 markets** (1.017x / 0.978x; was 0.866x); interleaved, both trie off, no perf. Fills per native block 0.956x at 300 markets (was 0.77x): matched / placed 0.766 vs 0.767. Non-pool zero-fill sell cuts 0.063% of placed (threshold 0.5%): **option A not needed**. All top-ups full; margin cancels and reduce-only cuts 0. 10-market margin +0.067 ms/1k vs main |
| 20 | With the oracle feed live through the drain (plan Step 6), does `5584880` drain, and what does an oracle-only block cost to execute? How large is the bench's open-limit reject share? Does the pre-merge `cargo test` pass? | **Drains in 38.2 s with the feed live; AGREE, liveness PASS. Oracle-only blocks 4.6 ms p50, 5.92 ms max (target <= 20 ms)**, exec lag 0-1 in the quiet window; walk 0 (prices static). Reject share unchanged: 52-54% of actions fail, 39-43% of orders are open-limit rejects (only reject reason), `OPEN_ORDER_BUDGET` unset. `cargo test --workspace`: 2806 passed, 0 failed |
| 21 | Do moving prices (walk 10 bp) change the merge gate? Does a per-sender in-flight cap (`--max-in-flight`) with `OPEN_ORDER_BUDGET` remove the open-limit rejects without losing throughput? What is crab / main on that shape? | Walk 10: **1.088x main** (walk 0 1.132x; walk costs ~4% matched/s), 0 liquidations, drains with the feed live. Cap: the budget (counting in-flight places) takes open-limit rejects 40% -> **0%** at every N; main matched/s 96-101k -> ~173k; plateau from N=2 on main and crab. Crab (`59fa407`) / main at N=2: **1.054x** (section 19 uncapped 1.097x); margin per fill 1.32x main. Standard shape from now on: **N=4 + budget 900** (21.4) |
| 22 | Phase 2 step 0: on the standard shape (N=4 + budget 900), are the Phase 2 targets still the top costs? What do moving prices add? Rows 7, 77, 78? | Crab / main **1.059x** (r1). Targets not in the planned order: **cancel-all book scan 27 ms/block** (plan 105-140), **>= 14-16k thread spawns / min** (plan ~1,300), flush 16 ms on exec + 74 ms flush worker, **stops diff ~0.06 ms (drop)**. Margin per fill 1.26x main. Walk 10: +13 ms engine per block (re-value). Row 77: first block after load waits on the flush worker's backlog. Row 78: empty blocks = liquidation sweep rebuilding `pos_sums`. Row 7: 0.33-0.37 s per 1M rows at start (est. ~0.56 s at 1.6M rows) |
| 23 | Does the s94 batch (main `35e69b3`) cost throughput? Does a liquidation storm stay live? | Batch: **1.057x main** at r1 (section 22: 1.059x), no measurable cost, 0 off-mark band refusals. Liquidation stress: S=400 **pass** (100 accounts, all backstop, step <= 115 ms, vault +19.98M). S=750 **liveness FAIL**: 100 accounts all ADL over ~270 markets each, liquidation step 332 / 123 / 241 s on 3 blocks, **consensus frozen ~11.6 min**; state agrees, vault deficit 26.5M (expected) |

The crab stack (account-level margin, oracle, liquidation) at 10 markets ran
~64k matched/s vs ~175k on main before PF1, 127.5k after. Section 5 put the
rest of the gap outside the execution path; section 6 corrects that: with
the resend loop removed the gap stays, and it is execution (~1.4-1.5x main).
Crab's ~1.8x RPC CPU per fill is a consequence of that, not a separate cost:
equal CPU per request, 1.85x more (mostly refused) requests per fill
(section 6.4).

## 1. Anti-spam no-regression A/B (main vs `feat/native-antispam`)

`79a3752` (main before) vs `92a02ed` (anti-spam, code `6398374`), cap 400,
limits at defaults (no `ANTISPAM`), no spam. Warm-up cell, then interleaved.
Cells `ozarchy-as3-nr-{warm,main-r1,branch-r1,main-r2,branch-r2}`.

| cell | matched/s | best60 | submit/s | CPU-s/1M* |
|---|---|---|---|---|
| main-r1 | 174,181 | 204,568 | 1,496 | 116.0 |
| branch-r1 | 170,988 | 199,636 | 1,469 | 114.7 |
| main-r2 | 174,946 | 204,386 | 1,498 | 116.9 |
| branch-r2 | 175,157 | 206,672 | 1,445 | 119.7 |
| **mean** | main 174,563 / branch 173,072 (**-0.85%**) | | | 116.5 / 117.2 |

\* estimated from per-node `cpu.csv` pcpu x run time, summed over 3 nodes.
Workspace tests on `92a02ed`: 2294 pass / 0 fail / 37 ignored. The 256-key
spam cells and B-against-spam cells are in
`docs/plans/native-antispam-2026-10-04.md` section 5.

## 2. Item 6 sync point 2: `perf/item6-phase1` vs `merge/item6-sync2`

`81a9567` (crab + item 6 C1) vs `cea1254` (+ anti-spam merge, oracle lane
under C, oracle-signer exemption). Cap 400, `ORACLE_FEED=1` (price 30000,
2000 ms, walk 0), default limits. Same bench binary for both arms.
Cells `ozarchy-s2-*`.

| cell | matched/s | best60 | commit ms | CPU-s/1M |
|---|---|---|---|---|
| item6-r1 | 63,797 | 81,713 | 837 | 231.9 |
| sync2-r1 | 64,020 | 82,730 | 720 | 233.6 |
| item6-r2 | 63,971 | 81,180 | 840 | 231.6 |
| sync2-r2 | 64,585 | 82,623 | 821 | 235.9 |
| **mean** | item6 63,884 / sync2 64,303 (**+0.7%**) | | | 231.8 / 234.8 |

Spam cell (`ozarchy-s2-sync2-spam256-cap20`, cap 20, 256 funded keys sending
cancel-alls at 2,000/s): 40,558 matched/s, orders in 68% of native blocks.
The pool stayed at 64,944-65,536 for ~75 s. All 198 oracle submissions
(66 rounds x 3 validators) were accepted; every 15 s poll during the full
pool showed the newest mark 0 blocks old. The node has no metric for oracle
submissions evicted inside the pool.

## 3. C1 full-node A/B: `9c4be2c` (before C1) vs `81a9567` (C1)

Same setup as section 2. Cells `ozarchy-c1ab-*`.

| arm | matched/s (mean of 2) | CPU-s/1M | commit ms | engine ms/1k fills |
|---|---|---|---|---|
| pre-C1 `9c4be2c` | 62,099 | 239.8 | 856 | 16.74 |
| C1 `81a9567` | 64,161 | 235.5 | 835 | 16.56 |
| C1 vs pre | +3.3% | -1.8% | -2.5% | -1.1% |

The +3.3% rests on one low pre-C1 cell (60,406; the other was 63,793).
Same-binary repeat against section 2: +0.4%. C1's ubench gain (54.3 -> 35.3
ms/1k at 300 markets) does not show here. Section 4 shows why: the 10-market
cost is not in account reads.

## 4. Exec-path CPU profile at 10 markets: crab `d52a33f` vs main `92a02ed`

One steady-state cell per arm (`ozarchy-prof10-{crab,main}`), crab with the
oracle feed, main without. Both nodes built with
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` and
`RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"`.
`perf record -e cycles:u -F 499 --call-graph fp -p <val0>` for 45 s from
35 s into the load. Exec path = threads named `torus-execution` (includes the
short-lived scoped workers, which inherit the name). Samples are weighted by
period: counting samples overstates short-lived threads ~1.7x. Each sample
goes to the first matching bucket in this order: liquidation, oracle,
top-ups, maker checks, matching, settle, margin-other, books, verify,
`execute_batch_phases` glue; the rest by leaf frame. `cycles:u` misses
kernel time.

| bucket, ms CPU per 1k fills | crab | main | crab - main |
|---|---|---|---|
| `same_batch_bid_top_ups` | **11.21** | 0 | **+11.21** |
| margin checks, other | 1.57 | 0.10 | +1.47 |
| maker checks in match | 0.89 | 0.04 | +0.85 |
| order-book matching | 3.57 | 2.97 | +0.61 |
| settle | 1.52 | 1.41 | +0.11 |
| liquidation | 0.17 | - | +0.17 |
| oracle (`begin_block_oracle`) | 0.006 | - | ~0 |
| `execute_batch_phases` glue | 1.94 | 1.96 | ~0 |
| books drain/save/load | 0.92 | 1.40 | -0.49 |
| other exec | 1.12 | 2.07 | -0.95 |
| **exec path total** | **22.91** | **9.95** | **+12.96** |

Cause: for every GTC bid at or above the best ask, `same_batch_bid_top_ups`
(`native_executor.rs:6484` on `d52a33f`, from `3d2dcd8` / `ef5eab7`) summed
`remaining_qty` over every resting ask order up to the bid price:
O(crossing bids x resting asks). The best ask levels held ~50k orders each.
Account-level margin itself costs ~2.3 ms/1k here.

Load: ~103k fills per block in the window (77k cell average); 5.55 positions
per account on crab (200 sampled senders: each held all 10 markets or none).

Outside the exec path, two thirds of crab val0's user CPU was the RPC worker
(75 ms/1k; `from_hex` alone 42.9 ms/1k), with 1.29M `backlog_preverify`
refusals on val0 vs 191k on main: shed batches resent by `RETRY_BUSY`.

### Harness "engine ms/1k fills"

`summarize.py:449`: delta `torus_exec_engine_seconds_sum` x 1e6 / delta
`torus_orders_matched_total` on val0, bench start to drain end. Wall time,
not CPU. On crab the timer (`app.rs:2245-2309`) covers
`begin_block_oracle`, both `execute_batch` calls (including waits on scoped
workers), `drain_core_writer`, `run_liquidations` and governance/fees/epoch;
it equals the ubench's `execute_batch + run_liquidations` plus a ~0.19 ms/1k
tail. On main it covers only the two `execute_batch` calls. Compare crab and
main by profile CPU or matched/s, not by this timer.

## 5. PF1 gate: `0ebfd71` (crab + PF1) vs main `92a02ed`

PF1 replaces the per-order walk with `AskDepth`: per market per batch, each
ask level is summed once (lazily, in the old walk's order), and each bid
gets a binary search. Bit-identical results. Same flags, perf method and
shape as section 4; crab arm with the oracle feed. Cells
`ozarchy-pf1-{warm,r1,r2}`, `ozarchy-pf1main-{r1,r2}`; r1 cells profiled.

| cell | matched/s | best60 | commit ms | engine ms/1k | CPU-s/1M | val0 backlog_preverify |
|---|---|---|---|---|---|---|
| pf1-r1 (perf) | 127,917 | 150,556 | 449 | 5.55 | 48.5 | 538,612 |
| main-r1 (perf) | 178,156 | 207,028 | 334 | 3.73 | 39.0 | 180,452 |
| pf1-r2 | 127,003 | 151,513 | 449 | 5.58 | 49.9 | 436,990 |
| main-r2 | 173,651 | 207,329 | 351 | 3.86 | 39.6 | 216,496 |

| | PF1 | main | before PF1 (section 4) |
|---|---|---|---|
| matched/s | 127,460 | 175,904 | 65,913 |
| vs main | **0.72x** | 1 | 0.37x |
| `same_batch_bid_top_ups`, ms CPU/1k | **0.02** (0.09 incl. inlined lines) | - | 11.21 |
| exec path, ms CPU/1k | **12.49** | 10.05 | 22.91 |
| val0 RPC worker, ms/1k | 23.0 | 4.2 | 74.9 |
| `from_hex`, ms/1k (share of val0 user CPU) | 12.8 (21%) | 1.9 (4.5%) | 42.9 (39%) |
| sys CPU, ms/1k | 17.5 | 6.1 | 53.1 |

Gate: top-ups < 0.5 ms/1k PASS; exec path ~11.7 expected, 12.49 measured
(PASS approx.); ~2x matched/s vs before PF1 met (1.93x); ~0.85x main missed
(0.72x).

Remaining crab-only exec cost (~2.4 ms/1k): `prepare_one` 1.37,
`place_order_with_accounts` 3.08 incl. callees (main: `place_order_with_margin`),
`maker_fill_fits` 0.60, liquidation 0.23. The reduce-only growth fold near
`native_executor.rs:6419` had 0 samples.

Exec CPU per fill is now 1.24x main, but throughput is 0.72x: the gap is
mostly outside the exec path. Shed-and-resent batches fell 2.6x but remain
~2.5x main, and the RPC worker and kernel CPU compete for cores with
execution on the shared host (exec wall per fill 1.5x main vs CPU 1.24x).
That last link is inferred, not measured; section 6 tests it and rules the
resend loop out.

## 6. C3 + PF1 (`d9ef4f7`) vs main `92a02ed`: resend loop, 300 markets, Gate 3

`merge/item6-c3-pf1` @ `d9ef4f7` = C3 `424d030` + `perf/item6-pf1`
`9e10441` (measurement branch; workspace tests 2697 / 0 / 39, golden 3/3).
Both nodes built with the section 4 profiling flags (c3pf1 node md5
`e226ec7c`, main `31a95c65`), same load generator (c3pf1 bench `ee934fdf`).
Cap 400, rate 76,000, 120 s, crab arm with the oracle feed, 2+2 interleaved
after a warm-up. All cells AGREE / PASS / dissemination clean, crab marks
fresh, `pool_full` 0. Cells `ozarchy-c3pf1-10m-*`, `ozarchy-c3pf1-300m-*`;
driver `ozarchy-c3pf1-campaign.sh`.

Per-thread CPU comes from a 1 Hz `/proc` sampler. RPC worker threads live
48-71 s, so their numbers are accurate; scoped exec workers live under 1 s
and are mostly missed, so "exec (+exited)" is process CPU minus the live
threads, an upper bound.

### 6.1 10 markets, with and without `RETRY_BUSY`

| mode | arm | matched/s | engine ms/1k | CPU-s/1M | val0 rpc ms/1k | val0 exec (+exited) ms/1k | val0 sys ms/1k | val0 backlog_preverify |
|---|---|---|---|---|---|---|---|---|
| retry | c3pf1 | 130,700 | 5.49 | 48.9 | 7.83 | 5.36 (13.4) | 4.52 | 309k |
| retry | main | 174,000 | 3.84 | 39.1 | 4.39 | 3.79 (11.0) | 3.18 | 187k |
| retry | **ratio** | **0.751** | 1.43 | 1.25 | 1.78 | 1.41 (1.22) | 1.42 | 1.65 |
| no retry | c3pf1 | 123,200 | 5.56 | 39.7 | 6.06 | 4.76 (11.7) | 3.88 | 254k |
| no retry | main | 186,900 | 3.72 | 33.0 | 3.38 | 3.24 (9.4) | 2.67 | 152k |
| no retry | **ratio** | **0.659** | 1.50 | 1.20 | 1.79 | 1.47 (1.25) | 1.46 | 1.68 |

The no-retry cells at rate 76,000 were healthy (cancels went through: 99k vs
113k cancelled), so no lower-rate fallback was needed. Without the resend
loop crab loses 6% and main gains 7%: the loop is not the gap. Crab's RPC
worker cost (1.8x) and extra `backlog_preverify` refusals (1.65x) stay in
both modes. Section 6.4 explains the 1.8x: it is shedding volume, not a
per-request cost.

### 6.2 300 markets (`RETRY_BUSY=1`)

| arm | matched/s | best60 | commit ms | engine ms/1k | CPU-s/1M | fills/block | positions/account (sampled) |
|---|---|---|---|---|---|---|---|
| c3pf1 | 49,600 | 78,100 | 824 | 14.72 | 108.3 | 57.0k | 191.5 avg (median 298) |
| main | 89,400 | 123,300 | 564 | 7.05 | 68.1 | 76.7k | 230.3 avg (median 300) |
| **ratio** | **0.555** | 0.633 | 1.46 | 2.09 | 1.59 | 0.74 | |

All four cells drained in 49-82 s (timeout 780 s); no drain artefact.

Profile of val0 in the r1 cells (same method as section 4; margin split
into its functions), ms CPU per 1k fills:

| bucket | c3pf1 | main |
|---|---|---|
| margin: `maker_fill_fits` | **5.08** | 0 |
| margin: `prepare_one` | **3.94** | 0 |
| margin: `place_order_with_accounts` (non-matching) | 0.16 | 0.02 |
| margin: `same_batch_bid_top_ups` | 0.04 | 0 |
| margin, other | 0.40 | 0.12 |
| liquidation | **3.81** | 0 |
| matching | 3.39 | 1.72 |
| settle | 2.43 | 3.41 |
| books drain/save/load | 1.45 | 0.64 |
| oracle (`begin_block_oracle`) | 0.14 | 0 |
| `execute_batch_phases` glue | 7.48 | 5.88 |
| other (hashing / compute / alloc) | 1.75 | 1.54 |
| **exec path total** | **30.10** | **13.45** |
| whole process (RPC worker) | 176.9 (60.0) | 112.5 (29.7) |

Top self-time on crab's exec thread (% of exec): `NativeStateOverlay::get_cf_raw`
8.7, `execute_batch_phases` 6.6, BTreeMap `Range` / `TakeWhile` map 5.3,
`Vec<(Vec, Vec)>` collect 4.4, `positions_for_trader` 3.6, `__KeccakF1600`
3.5, `FixedPoint::checked_mul` 3.3, `BTreeMap::get` 3.0, SipHash `write` 2.9,
`get_position` 2.7, `DefaultHasher::write` 2.3.

Margin and liquidation at 300 markets go mostly into
`PositionManager::positions_for_trader`: a BTreeMap range scan over the
overlay of every position an account holds, repeated per check. Inside
`maker_fill_fits`: range scan 16%, Vec collect 10%, `get_cf_raw` 10.6%,
`positions_for_trader` 9.6%, the function itself 5%. `liq_view`'s
AccountView build and `prepare_one` (position reads plus the `PosSums`
OnceLock init) follow the same pattern. Matching is 2x main
(`place_order_with_accounts`, `match_market`, `match_at_level`). `from_hex`
in RPC is 22.5% of crab's process user CPU.

### 6.3 ubench Gate 3 (300 markets, marks on)

`cargo test -p torus-bridge --release --test ubench_econ -- --ignored
--nocapture`, default release flags, each commit in its own target dir,
3 alternating runs per shape; median [range], ms per 1k fills, 10,184
fills/block in all runs. Raw logs `ozarchy-c3pf1-ubench/`.

| shape | commit | margin | match | settle | tail (liq) | total |
|---|---|---|---|---|---|---|
| `UB_MARKS=1 UB_MARKETS=300` | `d52a33f` | 2.11 [2.11-2.13] | 1.32 [1.32-1.33] | 2.29 | 4.21 | 11.44 [11.44-11.46] |
| same | `d9ef4f7` | 2.15 [2.12-2.16] | 1.32 [1.32-1.33] | 2.28 | 4.07 | 11.36 [11.24-11.51] |
| + `UB_MARK_WALK=10` | `d52a33f` | 2.14 [2.14-2.16] | 1.34 [1.32-1.35] | 2.33 | 4.21 | 11.52 [11.49-11.56] |
| same | `d9ef4f7` | 2.19 [2.17-2.19] | 1.33 [1.32-1.33] | 2.31 | 4.25 | 11.52 [11.47-11.53] |

Gate 3 (margin <= 1.5, match <= 2.0 ms/1k): margin **FAIL** (2.15-2.19),
match **PASS** (1.32-1.33). C3 + PF1 shows no change against `d52a33f` on
this ubench. ozarchy's absolute ubench numbers are 2-4x below 18c's sanity
run (margin 5.2, match 5.6); compare ratios across machines, not values.

### 6.4 Why crab's RPC costs ~1.8x main per fill at 10 markets

From the existing section 6.1 cells (1 Hz `/proc` sampler) and the section 5
perf data; no new cells. Full analysis: `ozarchy-rpc18-analysis.md`.

| val0, retry / no retry | crab | main | ratio |
|---|---|---|---|
| RPC worker ms CPU per 1k fills | 7.83 / 6.06 | 4.39 / 3.38 | 1.78 / 1.79 |
| submit requests per 1k fills | 17.4 / 14.4 | 9.4 / 7.6 | 1.84 / 1.90 |
| refused (`backlog_preverify`) per 1k fills | 15.1 / 12.0 | 7.2 / 5.5 | 2.11 / 2.18 |
| RPC worker µs CPU per request | 452 / 422 | 467 / 446 | 0.97 / 0.95 |

- **Not a crab code cost.** CPU per request is the same on both arms. A
  refused request costs ~0.5 ms of RPC CPU on both (0.495 crab, 0.519
  main); one shared cost per refused request plus one per admitted request
  reproduces all 8 cells within 6%, with no crab-specific term.
- **Mechanism.** The load generator is closed-loop (256 requests in flight
  per node), so its request rate is 256 / mean round trip. Crab's slower
  execution keeps the pool backlogged more often, busy refusals return
  fast, and the generator sends again. The 1.8x is a symptom of the
  execution gap and should shrink as execution gets faster.
- **The 5.5x in section 5** is the same mechanism at peak shedding (59.3 vs
  12.0 requests per 1k fills in that 45 s window; 388 vs 350 µs per
  request).
- **No crab-only RPC frames.** The `torus.rs` diff between main and crab
  touches only oracle paths, which `PlaceOrderBatch` never runs. The
  oracle feed's own RPC load is under 0.03% of requests (64 submits and
  21 mark reads per cell vs 300-400k batch submits). Sys CPU per request
  is equal (75-78 vs 80-84 µs).
- **Side item, open:** the signature-verify pool (`torus-ingress-v`) costs
  1.22-1.27x per admitted action on crab.
- **Optional, both arms:** the shed path decodes the whole hex payload
  before refusing (`parse_bytes`, `crates/torus-rpc/src/types.rs:93-96`,
  `hex` 0.4; the decode-only shed task `torus.rs:507-528` on `d9ef4f7` is
  63% of crab's RPC cycles under shedding, `from_hex` alone 50%).
  Classifying a request by peeking the bincode variant tag (the first 8
  hex characters) would remove ~60% of RPC CPU under shedding; switching
  to `alloy_primitives::hex` (const-hex, already in `Cargo.lock`) cuts
  roughly the `from_hex` half. Either makes an overloaded node cheaper to
  keep busy.
- **For gates,** report RPC CPU per request and per admitted action, not
  only per fill.

## 7. Baseline `14236fa` (C3 + C4 + PF1 + cooldown fix), 2026-10-05

Crab only, no main arm (main references are the section 6 cells, so allow
for session drift). Same profiling flags and harness as section 6; node md5
`7da38063`. 300 markets, cap 400, rate 76,000, `RETRY_BUSY=1`, oracle feed
on, a warm-up and 2 measured cells, both profiled. All cells AGREE / PASS,
marks fresh. Cells `ozarchy-14236fa-300m-{warm,r1,r2}`, ubench logs
`ozarchy-14236fa-ubench/`.

### 7.1 Full node, 300 markets

| cell | matched/s | engine ms/1k | CPU-s/1M | val0 exec ms CPU/1k | positions/account avg (median) |
|---|---|---|---|---|---|
| r1 | 48,200 | 14.41 | 107.5 | 28.25 | 188 (296) |
| r2 | 50,700 | 14.11 | 104.6 | 27.34 | 206 (298) |
| C3 + PF1 (section 6.2) | 49,600 | 14.72 | 108.3 | 30.10 | 191.5 (298) |
| main (section 6.2) | 89,400 | 7.05 | 68.1 | 13.45 | - |

C4 leaves full-node throughput unchanged: 49.4k matched/s, 0.553x main.

| bucket, ms CPU per 1k fills | r1 | r2 | C3 + PF1 | main |
|---|---|---|---|---|
| `execute_batch_phases` glue | 7.76 | 7.16 | 7.48 | 5.88 |
| margin: `maker_fill_fits` | 4.21 | 4.49 | 5.08 | 0 |
| margin: `prepare_one` | 4.06 | 3.65 | 3.94 | 0 |
| liquidation | 3.11 | 3.36 | 3.81 | 0 |
| matching | 3.03 | 3.05 | 3.39 | 1.72 |
| settle | 2.42 | 2.02 | 2.43 | 3.41 |
| books drain/save/load | 1.39 | 1.29 | 1.45 | 0.64 |
| oracle (`begin_block_oracle`) | 0.13 | 0.15 | 0.14 | - |
| **exec total** | **28.25** | **27.34** | **30.10** | **13.45** |

Liquidation is still 3.1-3.4 ms/1k (-15% from C4). 94% of it is
`liq_view -> AccountReader::pos_sums -> direct_sums` rebuilding traders
written in the block; two thirds of that is `positions_for_trader` range
scans over the overlay (`iterate_cf` and the `Vec<(Vec, Vec)>` collect).
Crab-only margin and liquidation make up ~12 of the ~14.4 ms/1k gap to
main: the targets of C6 / C7 in the plan. RPC CPU per request is flat
(435-450 µs), as in section 6.4.

### 7.2 ubench, ratio to `9c4be2c` on ozarchy

Default release flags, each commit in its own target dir, 3 alternating
runs, median [range], ms per 1k fills.

| shape | commit | margin | match | settle | tail (liq) | total |
|---|---|---|---|---|---|---|
| 300 mk, marks | `9c4be2c` | 2.99 | 2.31 | 3.02 | 7.56 | 17.31 [17.06-17.57] |
| 300 mk, marks | `14236fa` | 2.02 | 1.30 | 2.33 | 3.15 | 10.24 [10.17-10.40] |
| **ratio** | | **0.68** | **0.56** | 0.77 | **0.42** | **0.59** |
| gate / fail line | | 0.20 / 0.40 | 0.21 / 0.43 | 1.0 | 0.06 / 0.12 | 0.31 / 0.36 |
| 300 mk, marks, walk 10 | `14236fa` | 2.12 | 1.32 | 2.31 | 4.31 | 11.64 |
| 10 mk, marks | `9c4be2c` | 1.40 | 0.38 | 1.03 | 0.62 | 3.99 |
| 10 mk, marks | `14236fa` | 0.99 | 0.32 | 0.95 | 0.18 | 2.97 |
| **ratio** | | 0.71 | 0.84 | 0.92 | 0.29 | 0.74 |

Every ratio gate except settle is past its fail line. `9c4be2c` has no
`UB_MARK_WALK` (added in `3924b76`, not an ancestor), so the walk shape ran
on `14236fa` only. ozarchy's `9c4be2c` total is 17.3 (18c's: 46.7):
compare ratios across machines, not values.

### 7.3 Empty block (`ubench_epoch`, `UB_DRAIN=fresh`, 4,937 holders)

| | total ms | ctx | oracle [prune / inputs / agg / marks] | liquidation | flush (W) |
|---|---|---|---|---|---|
| default | 30.04 | 4.53 | 5.84 [1.96 / 0.11 / 3.07 / 0.97] | 4.81 | 10.67 |
| `TORUS_NATIVE_TRIE_MAINTENANCE=0` | 13.49 | 4.45 | 4.16 [0.90 / 0.09 / 2.00 / 0.81] | 4.58 | 0.33 |

The oracle step is 5.8 ms here, already below E1's ~8 ms target (set from
18c's ~19 ms); E1 is postponed as low priority. Trie maintenance (flush
10.7 ms) is the biggest single cost of the empty block.

## 8. Read-only analyses on `51051c9` (no builds)

### 8.1 Native trie root: used in production?

No. `CF_NATIVE_TRIE`, `CF_NATIVE_HASHED` and the stored root are written
but never read on a production path (same finding as s450 and the s83
reader check). Full analysis: `ozarchy-trie-analysis.md`.

- **The flag:** read once per process at
  `crates/torus-state/src/native_trie.rs:321-331`; only `0` turns it off.
- **On:** `apply_native_dirty` (`backend.rs:1520-1567`, on
  `torus-flush-worker`, `exec_pipeline.rs:205` / `:426`) rehashes the
  changed buckets of the 65,536-bucket Merkle and writes nodes, mirror rows
  and root into the block's write batch.
- **Off:** it writes one `META_NATIVE_TRIE_STALE` key. At boot,
  `app.rs:3848-3854` rebuilds a missing or stale trie only when the flag is
  on.

| reader | file:line | production? | with the flag off |
|---|---|---|---|
| `flagged_native_root` / `native_root_routed` | `torus-bridge/src/state_root.rs:75-101` | only via the two rows below | falls back to a full scan |
| `build_block_with_native` (proposer) | `proposer.rs:187`, `:269` | no, tests only (`native_bridge_tests.rs:665`) | - |
| `validate_block_with_native` (validator) | `validator.rs:342`, `:460` | no, tests only (`native_bridge_tests.rs:414`) | - |
| live block validation | `app.rs:1814` (EVM blocks only) | yes | EVM root + empty native root (`state_root.rs:119-130`); never reads the trie |
| block header `state_root` | `app.rs:5207` (parent's); genesis EVM-only (`torus-genesis/src/lib.rs:497`) | yes | independent |
| running state hash / `getStateHash` | `backend.rs:1430-1460`; `torus-rpc/src/torus.rs:1215` | yes | independent; trie CFs and the stale key excluded (`running_hash.rs:470-484`, `:586`) |
| snapshot verify | `snapshot.rs:139` | yes | full scan, never the stored trie |
| RPC proofs | none (`torus-rpc/src/lib.rs:5053` is test code) | - | - |
| explorer, bench digest / AGREE | `torus-explorer/src/indexer.rs:293`, `digest-node.sh` | - | read the header root or books / balances |

**Turning it off by default** breaks nothing in consensus, mixed fleets,
restart / replay, snapshots or existing databases. Turning it back on costs
one rebuild at boot. What breaks is tests:
- the default assertion in `native_trie.rs:1827-1832`;
- tests that flush through the env-reading wrappers and then read the stored
  root: `torus-bridge/tests/{root_cache,level_rows,deferred_save,save_books_parallel}_tests.rs`,
  `torus-integration-tests/tests/chaos.rs`, and the `app.rs` crash-recovery
  tests at 13302, 13415, 13476, 14075, 14136, 14325, 15780, 16313. They
  would need maintenance forced on.

**Savings:**
- Empty block at 300 markets: flush 11.1 -> 0.4 ms, total 30.4 -> 13.3 ms.
- Full node at 300 markets: trie maintenance is 156-163 ms per block, about
  42% of the flush worker's wall time, 4.0 ms per 1k fills, and 4.5-5% of
  the node's user cycles.
- Execution (~870 ms per block) is the limit at this load, not the flush
  worker (~387 ms), so throughput will not rise one for one. s83 measured
  +31% at 300 markets when the flush worker was the limit.

**Open owner question (from s450):** were the two `*_with_native` paths
abandoned on purpose? If so, make off the default (one line in
`parse_native_trie_maintenance`), force maintenance on in the trie tests
and keep `=1` as an opt-in. If the root is needed later: lazily on request
(`native_root_full`), every N blocks, or async (Option A in
`docs/plans/async-native-trie-maintenance.md`).

### 8.2 Crab vs main gap left after C6 / C7 (300 markets)

From the section 7 crab profile (r1; r2 agrees) and the section 6.2 main
profile, source lines via `llvm-addr2line -i` on the build-id-cache
binaries. Full analysis: `ozarchy-gap-after-c7-analysis.md`. Workload per
fill is the same: 1.76 vs 1.80 submitted orders per fill, ~84k fills per
block, identical `Fill` / `Order` / `MatchRequest` layouts.

**Matching, 3.03 vs 1.72 ms/1k (+1.31):**

| part | crab | main | delta |
|---|---|---|---|
| `maker_fill_fits` code inlined (`order_book.rs:2015`), crab-only | 0.16 | 0 | +0.16 |
| post-match account update (`set_free`, `need()`, `checked_mul`; 1193-1222), crab-only | 0.16 | 0 | +0.16 |
| `MatchMargin` built with an `AccountMargins` lookup (1051), crab-only | 0.07 | 0 | +0.07 |
| touched set + `sweep_reduce_only` (1128-1147) | 0.18 | 0.12 | +0.06 |
| `match_at_level` shared bookkeeping | 0.50 | 0.33 | +0.17 |
| `insert_order` | 0.55 | 0.35 | +0.20 |
| `match_market` self | 0.41 | 0.21 | +0.20 |
| execution thread (`cancel_all_many`, sort, drain) | 0.72 | 0.54 | +0.18 |

Only ~0.40 is crab-only code. The other ~0.9 is the same code costing
1.3-2x more per operation (e.g. `sort_native_actions` 1.43x with the same
work per fill). The likely cause is cache / IPC; this data cannot prove it.

**Glue, 7.76 vs 5.88 ms/1k (+1.88):**

| region (crab line / main line) | crab | main | delta |
|---|---|---|---|
| reduce-only tracking (4975 / 4241), shared | 2.10 | 2.49 | -0.39 |
| `AccountMargins` setup (4986-5005), crab-only | 0.93 | 0 | +0.93 |
| `same_batch_bid_top_ups` inlined part, crab-only | 0.71 | 0 | +0.71 |
| `settle_market_results_parallel` inlined | 1.54 | 1.12 | +0.42 |
| `PositionCache` `flush_all` | 0.82 | 0.67 | +0.15 |
| `phase2_reservation_basis` / open order counts | 0.89 | 0.73 | +0.16 |
| Phase 2 serial loop | 0.33 | 0.61 | -0.28 |
| `phase2_bid_floors` (4809), crab-only | 0.06 | 0 | +0.06 |
| other | 0.39 | 0.27 | +0.12 |

The books bucket (1.34 vs 0.64) is not a books slowdown: 0.67 of it is
`end_resident` (crab-only, applying the block's writes to the resident
rows), counted there by the bucket rules. `drain_book` itself is +11%.

**Crab-only parts and ideas (not built):**

| part | ms/1k | removed by C6 / C7? | idea |
|---|---|---|---|
| `AccountMargins` setup | 0.93 | ~0.05 | `position_px` (1134) re-reads the row already read at 4975; reuse it (~0.6) |
| same-batch top-ups (inlined) | 0.74 | no | drop the i128 tick `%` in `can_rest_shape` (0.28; Phase 2 already checked the tick) |
| F1 code in matching | 0.40 | no | `AccountMargins` as a Vec by sender slot; skip the hold-price `checked_mul` when nothing rests |
| `end_resident` apply | 0.67 | C7 changes the layout | apply the delta off the execution thread's critical path |
| shared-code inflation | ~1.6 | maybe indirectly | measure IPC / LLC misses first |

**Estimate after C6 / C7:**

| component, ms/1k | now | after C6 / C7 |
|---|---|---|
| liquidation | 3.23 | ~0.5 |
| `maker_fill_fits` (C7 cuts the `positions_for_trader` scans, 1.51) | 4.35 | ~3.0 |
| `prepare_one` | 3.85 | ~3.4 |
| glue / matching / books delta | +1.57 / +1.32 / +0.70 | ~+1.5 / +1.3 / +0.7 |
| settle delta (crab cheaper) | -1.19 | -1.19 |
| other | +0.48 | +0.45 |
| **total gap** | **+14.35** (27.8 - 13.45) | **~+9.7: crab ~23.1, ~0.58x main** |

This assumes throughput scales with 1 / (exec ms per 1k fills), which fits
the observed 0.55x at 2.07x the cost. Gate 2 (0.9x) needs ~14.9, so ~8.2
more ms/1k must go. A plausible path, about 6.0 (~0.78x):
- incremental per-trader margin sums for makers (`maker_fill_fits`) -2.2;
- a slimmer `prepare_one` -1.5 to -2;
- the glue fixes above -1.2;
- `end_resident` off the critical path -0.6;
- `AccountMargins` as a Vec in matching -0.2.

The last ~2 would have to come from the per-operation inflation, so the
next measurement is `perf stat` IPC and LLC misses on the execution thread.

## 9. Trie maintenance on vs off, and IPC / cache misses (300 markets, 2026-10-05)

Same binaries as section 7: crab `14236fa` (node md5 `7da38063`), main
`92a02ed` (`31a95c65`), the `14236fa` load generator. 300 markets, cap
400, rate 76,000, `RETRY_BUSY=1`, 120 s; crab with the oracle feed. All
cells AGREE, liveness PASS, ACCEPT. Analyses: `ozarchy-trie0-analysis.md`,
`ozarchy-ipc-analysis.md`.

### 9.1 Trie maintenance on vs off

The section 6 and 7 cells did not set `TORUS_NATIVE_TRIE_MAINTENANCE`, so
they ran with maintenance **on** (the default). 18c's campaign
`arms.conf.example` sets `=0` on every arm, so cells from that harness are
trie-off cells. For the trie-off cells `=0` was passed through `EXTRA_ENV`:
all 3 nodes had it in their environment, every node log has the
"maintenance DISABLED ... trie marked stale" boot line, and no rebuild ran.

| cell | trie | matched/s | best60 | engine ms/1k | chain ms | pipelined ms | handoff wait ms | commit ms avg |
|---|---|---|---|---|---|---|---|---|
| crab r1 (section 7) | on | 48,220 | 80,519 | 14.41 | 1,007 | 583 | 1.83 | 847 |
| crab r2 (section 7) | on | 50,651 | 79,503 | 14.11 | 827 | 489 | 1.78 | 687 |
| main r1 (section 6.2) | on | 87,792 | 125,375 | 7.24 | 764 | 689 | 77.87 | 572 |
| main r2 (section 6.2) | on | 91,023 | 121,136 | 6.86 | 722 | 660 | 98.94 | 556 |
| crab `ozarchy-trie0-crab-r1` | off | 51,871 | 80,659 | 13.86 | 920 | 167 | 0.01 | 852 |
| main `ozarchy-trie0-main-r1` | off | 100,744 | 129,761 | 7.05 | 560 | 172 | 0.24 | 477 |
| crab `ozarchy-trie0-crab-r2` | off | 50,398 | 80,709 | 14.09 | 850 | 149 | 0.01 | 724 |
| main `ozarchy-trie0-main-r2` | off | 100,401 | 132,426 | 7.07 | 568 | 174 | 0.23 | 485 |

| arm | trie on (mean) | trie off (mean) | change | CPU-s per 1M fills on -> off |
|---|---|---|---|---|
| crab | 49,435 | 51,134 | +3.4% | 106 -> 100 |
| main | 89,407 | 100,573 | +12.5% | 68 -> 60 |
| **crab / main** | **0.553x** | **0.508x** | worse | |

- **The ratio gets worse with the trie off.** With it on, main is partly
  held back by the flush worker: its execution thread waited 78-99 ms per
  block for the previous flush (handoff wait). Crab is limited by its own
  execution thread (handoff wait ~2 ms), so removing the trie frees main
  far more than crab.
- **Engine time per fill does not change** on either arm; the flush side
  (pipelined time) drops from ~490-690 to ~150-175 ms per block on both.
- **Drift:** the trie-on cells are from earlier sessions (main's with a
  different load generator, the C3 + PF1 bench, same node binary). The
  trie-off pair ran interleaved in one window. The direction holds; the
  size (0.553 -> 0.508) is about +-0.02. A trie-on pair in the same session
  would remove the caveat.
- **For comparisons with 18c:** cells from 18c's campaign harness (trie
  off) put main ~12% higher than ours, and crab ~3% higher.

### 9.2 IPC and cache misses: why shared code costs more on crab

One crab cell and one main cell (`ozarchy-ipc-{crab,main}`) under
`perf record -F 499 --call-graph fp` on val0, each event sampled on its own:
cycles, instructions, L1d load misses, loads served from DRAM
(`ls_dmnd_fills_from_sys.mem_io_local`; this Zen 3 host has no per-process
`LLC-load-misses`) and branch misses, for 45 s from 35 s into the load.
`perf record -p` follows threads created after it attaches (checked), so
the short-lived execution workers are counted. Throughput under 5 events:
crab 45,261, main 79,065 matched/s (0.92x / 0.88x of the unprofiled cells;
ratio 0.57x). Cycles are converted at ~3.3 GHz.

| group, per 1k fills (crab / main) | Mcycles | Minstr | IPC | L1d misses / 1k instr | DRAM fills / 1k instr | branch misses / 1k instr |
|---|---|---|---|---|---|---|
| whole process | 412 / 260 | 603 / 421 | 1.46 / 1.62 | 9.6 / 8.9 | 0.55 / 0.43 | 1.47 / 1.23 |
| execution (incl. workers) | 95.5 / 43.3 | 98.8 / 48.4 | 1.03 / 1.12 | 15.2 / 14.7 | 1.53 / 1.14 | 1.73 / 1.62 |
| flush worker | 42.8 / 34.6 | 61.0 / 49.8 | 1.42 / 1.44 | 6.1 / 5.9 | 0.85 / 0.85 | 1.23 / 1.16 |
| RPC workers | 222 / 103 | 353 / 168 | 1.59 / 1.62 | 8.1 / 8.1 | 0.31 / 0.30 | 1.54 / 1.49 |

Shared functions, extra cycles on crab split into more instructions and
lower IPC:

| function | Mcycles / 1k fills (crab / main) | instr ratio | IPC (crab / main) | extra = from instructions + from IPC |
|---|---|---|---|---|
| `match_market` (self) | 1.05 / 0.55 | 2.12 | 1.20 / 1.08 | +0.50 = +0.62 - 0.12 |
| `match_at_level` (self) | 1.07 / 0.58 | 2.47 | 1.30 / 0.96 | +0.48 = +0.86 - 0.37 |
| `insert_order` (self) | 0.43 / 0.30 | 1.58 | 1.07 / 0.98 | +0.13 = +0.17 - 0.04 |
| `cancel_all_many` (incl.) | 1.30 / 0.96 | 1.39 | 0.57 / 0.55 | +0.34 = +0.38 - 0.04 |
| `sort_native_actions` (incl.) | 0.40 / 0.32 | 1.37 | 0.75 / 0.70 | +0.09 = +0.12 - 0.03 |
| `drain_book` (incl.) | 3.71 / 2.67 | 1.40 | 1.04 / 1.03 | +1.04 = +1.06 - 0.02 |
| `PositionCache::flush_all` (incl.) | 2.22 / 1.83 | 1.22 | 1.36 / 1.35 | +0.39 = +0.40 - 0.01 |
| `execute_batch_phases` self (incl. inlined settle) | 5.24 / 3.09 | 1.60 | 0.67 / 0.71 | +2.15 = +1.85 + 0.29 |
| `compute_market_settle_plan` (self) | 1.30 / 0.91 | 1.05 | 0.78 / 1.06 | +0.39 = +0.04 + 0.34 |
| **sum** | | | | **+5.50 = +5.51 - 0.01 (~1.7 ms/1k)** |

- **Verdict: more instructions, not cache misses.** The shared code runs
  1.2-2.5x more instructions per fill on crab; the IPC part nets to about
  zero. Misses per instruction (L1d, DRAM, dTLB) are the same within
  noise, and matching even runs at a higher IPC on crab. Only
  `compute_market_settle_plan` and the glue lose IPC (~0.2 ms/1k together).
- `match_market` / `match_at_level` include crab-only margin code inlined
  into them (~0.4 ms/1k, section 8.2), so not all of their extra
  instructions are shared code.
- **Lower IPC does cost the execution path ~7.2 Mcycles (~2.2 ms/1k)**, but
  in crab-only code: `positions_for_trader` IPC 0.65 with 2.6 DRAM fills
  per 1k instructions; `get_cf_raw` and `maker_fill_fits` IPC 0.82. Fewer
  range scans (the C7 layout, `AccountMargins` as a Vec by sender slot) is
  the cache fix, worth up to ~2 ms/1k.
- **Next:** count calls per fill and `perf annotate` the instructions event
  for `drain_book`, `cancel_all_many`, `insert_order` and `match_market` in
  both builds, to separate "called more often" from "more instructions per
  call", then cut the per-call work.

## 10. C6 + C7 (`82bd1a4`) at 300 markets, trie off (2026-10-05)

Crab only, `perf/item6-phase1` @ `82bd1a4`, which holds everything after
`14236fa`: the liquidation rule-B fix (`51051c9`), C6a (`d1ca548`), C6b
(`41b0d4b`), C6c (`3df3e15`) and C7 (`82bd1a4`). The deltas below are C6
and C7 together, not C7 alone. Same run shape as section 7 (300 markets,
cap 400, rate 76,000, `RETRY_BUSY=1`, 120 s, oracle feed 30000 / 2000 ms /
walk 0: 402/402 accepted, marks fresh 300/300) with
`TORUS_NATIVE_TRIE_MAINTENANCE=0` through `EXTRA_ENV`: in the environment
of all 3 nodes in every cell, one "maintenance DISABLED" boot line in each
of the 9 node logs, no rebuild. Order: warm (60 s, no perf), r1, r2; r1 and
r2 ran `perf record` (cycles:u, 499 Hz, fp) on val0 for 45 s from 35 s into
the load. All cells AGREE, liveness PASS, ACCEPT. Build flags as `14236fa`
(mold, frame pointers, `line-tables-only`); node md5 `2783579b`, the
`82bd1a4` load generator (md5 `89744d01`). Driver and analysis:
`ozarchy-82bd1a4-c7-campaign.sh`, `ozarchy-82bd1a4-c7-analysis.md`.

### 10.1 Full node

| cell | matched/s | best60 | engine ms/1k | flush phase ms | exec_root ms/blk | handoff wait ms | CPU-s per 1M fills |
|---|---|---|---|---|---|---|---|
| C7 r1 (perf val0) | 65,111 | 97,624 | 10.17 | 165 | 0.56 | 0.21 | 81.4 |
| C7 r2 (perf val0) | 63,296 | 97,941 | 10.25 | 171 | 0.61 | 0.02 | 81.8 |
| **C7 mean** | **64,204** | 97,783 | **10.21** | 168 | 0.6 | 0.1 | **81.6** |
| crab `14236fa` trie off (9.1) | 51,134 | 80,684 | 13.98 | 159 | 0.55 | 0.01 | 100 |
| main `92a02ed` trie off (9.1) | 100,573 | 131,094 | 7.06 | 174 | 0.96 | 0.24 | 60 |
| warm C7 (60 s, no perf) | 71,355 | 98,432 | 8.85 | 159 | 0.49 | 0.08 | 63.6 |
| warm `14236fa` trie off (60 s) | 57,057 | 86,379 | 11.67 | 116 | 0.39 | 0.21 | 75.3 |

| ratio | `14236fa` trie off | C7 trie off |
|---|---|---|
| matched/s, crab / main | 0.508x | **0.638x** |
| engine ms/1k, crab / main | 1.97x | 1.44x |
| CPU-s per 1M fills, crab / main | 1.68x | 1.36x |

- **+25.7% vs `14236fa`** (64.2k vs 51.1k); the unprofiled warm cells agree
  (+25%, 71.4k vs 57.1k). Engine time per fill -27%, CPU per fill -18%.
- **Flush side unchanged:** flush phase, `exec_root` and handoff wait match
  the `14236fa` trie-off cells. Crab is still limited by its execution
  thread.
- **Caveats:** the main and `14236fa` references are the section 9.1 cells
  (same day, not interleaved, `14236fa` load generator). The C7 round
  cells profiled val0 and the references did not, which biases C7 low, so
  0.638x is if anything conservative.

Chain and commit cadence, same columns as the 9.1 table, from each cell's
`summary.json` headline (blk/s: val0 committed height over the bench
window; native blk/s: non-empty blocks per second from the val0 phase
log):

| cell | trie | matched/s | best60 | engine ms/1k | chain ms | pipelined ms | handoff wait ms | commit ms avg / p50 | blk/s | native blk/s |
|---|---|---|---|---|---|---|---|---|---|---|
| C7 warm (60 s, no perf) | off | 71,355 | 98,432 | 8.85 | 656 | 158 | 0.08 | 543 / 350 | 0.8 | 0.81 |
| C7 r1 (perf val0) | off | 65,111 | 97,624 | 10.17 | 692 | 164 | 0.21 | 645 / 312 | 1.0 | 1.05 |
| C7 r2 (perf val0) | off | 63,296 | 97,941 | 10.25 | 713 | 169 | 0.02 | 657 / 443 | 1.0 | 0.99 |
| `14236fa` `ozarchy-trie0-warm` (60 s) | off | 57,057 | 86,379 | 11.67 | 645 | 116 | 0.21 | 451 / 202 | 1.1 | 1.13 |
| `14236fa` `ozarchy-trie0-crab-r1` | off | 51,871 | 80,659 | 13.86 | 920 | 167 | 0.01 | 852 / 299 | 0.7 | 0.66 |
| `14236fa` `ozarchy-trie0-crab-r2` | off | 50,398 | 80,709 | 14.09 | 850 | 149 | 0.01 | 724 / 220 | 0.9 | 0.85 |
| main `ozarchy-trie0-main-r1` | off | 100,744 | 129,761 | 7.05 | 560 | 172 | 0.24 | 477 / 271 | 1.6 | 1.33 |
| main `ozarchy-trie0-main-r2` | off | 100,401 | 132,426 | 7.07 | 568 | 174 | 0.23 | 485 / 400 | 1.6 | 1.33 |

- **Chain time per block falls ~885 -> ~703 ms** (r1 / r2 means) and the
  average commit interval ~788 -> ~651 ms, so crab commits ~1.0 blocks/s
  vs ~0.8 for `14236fa` and 1.6 for main, at a similar ~53-59k fills per
  native block (main ~65k). Pipelined (flush) time is unchanged.
- The 60 s warm cells do not follow this: the `14236fa` warm cell had
  about the same chain time (645 vs 656 ms) and more blocks/s (1.1 vs 0.8)
  despite fewer fills/s. Not investigated; use the 120 s r1 / r2 pairs for
  the comparison.

Engine phases on val0 (ms per block):

| | total | phase 1 | margin | match | settle | tail (incl. liquidation) | untimed |
|---|---|---|---|---|---|---|---|
| C7 r1 / r2 | 553 / 573 | 45 / 47 | 216 / 221 | 96 / 102 | 128 / 135 | **48 / 46** | 20 / 21 |
| `14236fa` trie off r1 / r2 | 812 / 746 | 42 / 40 | 265 / 245 | 178 / 162 | 136 / 122 | **170 / 159** | 20 / 17 |
| main trie off r1 / r2 | 459 / 466 | 51 / 50 | 95 / 97 | 149 / 151 | 155 / 158 | 0.05 | 9 |

C7 runs more fills per block (about 1 block/s at 65k vs 51k fills/s), so
the per-block drops understate the per-fill drops.

### 10.2 val0 execution-thread profile

ms CPU per 1k fills over the perf window, mean of r1 / r2. The only
`14236fa` profile is section 7's (trie **on**), so the flush-worker drop
(15.0 -> 3.8) is mostly the trie being off and cannot be split from C6 /
C7. The execution-thread buckets below are not trie work.

| bucket | `14236fa` | C7 | delta |
|---|---|---|---|
| execution thread total | 27.8 | 20.8 | -7.0 |
| leaf kind: state reads (overlay / RocksDB) | 8.30 | 1.70 | **-6.6** |
| `execute_batch_phases` glue | 7.46 | 5.35 | -2.1 |
| margin: `maker_fill_fits` | 4.35 | 1.46 | -2.9 |
| margin: `prepare_one` | 3.86 | 2.66 | -1.2 |
| liquidation | 3.24 | 0.89 | **-2.3 (-72%)** |
| matching | 3.04 | 2.98 | 0 |
| settle | 2.22 | 2.04 | -0.2 |
| margin (other; `AccountView::build_with` and sums land here) | 0.33 | 1.28 | +0.95 |
| books drain / save / load | 1.34 | 1.90 | +0.6 |

| `14236fa` | C7 | delta | self symbol |
|---|---|---|---|
| 2.51 | 0.53 | -1.97 | `NativeStateOverlay::get_cf_raw` |
| 1.38 | 0 | -1.38 | BTreeMap range iterator (`positions_for_trader` scan) |
| 1.13 | 0 | -1.13 | `Vec<(Vec<u8>, Vec<u8>)>` collect of the scan |
| 0.89 | 0 | -0.89 | `PositionManager::positions_for_trader` |
| 0.79 | 0.02 | -0.77 | `PositionManager::get_position` |
| 0.91 | 0.25 | -0.66 | `BTreeMap<Vec<u8>, Vec<u8>>::get` |
| 0.28 | 0.03 | -0.25 | `Position::deserialize` (borsh) |
| 0 | **1.27** | +1.27 | **`AccountReader::get_position`** (decoded-slot lookup) |
| 0.45 | 0.27 | -0.18 | `AccountView::build` -> `build_with` |
| 2.7 | 2.7 | 0 | hashing (SipHash + Keccak) |

- **C6 + C7 remove the range scans, overlay reads and borsh decodes**
  behind the per-trader position and sum rebuilds (about 5.5 ms/1k),
  replaced by `AccountReader::get_position` at 1.27. This is also the
  low-IPC crab-only code section 9.2 named (`positions_for_trader`,
  `get_cf_raw`, `maker_fill_fits`).
- **Left of the engine gap (10.2 vs 7.1 ms/1k):** mostly the margin phase
  (216-221 vs ~95 ms per block on main); match and settle are already at
  or below main per block. Then glue (5.3 ms/1k) and hashing (~2.7,
  untouched by C6 / C7).

## 11. Margin phase breakdown: crab 82bd1a4 vs main 92a02ed (300 markets)

Read-only, from existing profiles; nothing built. Crab: the section 10
cells (`ozarchy-82bd1a4-c7-r1` / `-r2`, binary md5 `2783579b`, build-id
`be880ac0`). Main: `ozarchy-c3pf1-300m-main-r1` (section 6.2), the only
cycles profile of main `92a02ed` at 300 markets; its binary
(`wt/main/target/release/torus-node`, build-id `0c1dc011`, the one in
`perf.data`) is still on disk. Every execution-thread sample is expanded
into its inlined frames with `llvm-addr2line -i`; a sample is in the
margin phase when the line of `execute_batch_phases` on its stack is inside
the ENGINE margin timer (crab `native_executor.rs` 5101-5302, Phase 2 from
`open_order_counts` to the pools; main 3937-4216). That phase runs serially
on the execution thread on both (every `prepare_one` sample comes from the
serial loop at 5246; the sharded prepare never ran), so its CPU is its wall
time. ms CPU per 1k fills as in section 10.2. Tools
`ozarchy-margin-c7-tools/`, analysis `ozarchy-margin-c7-analysis.md`.

**Caveats:** the main profile ran with the trie **on**, the C3 + PF1 load
generator, one cell, 79k fills/s under perf. Margin code is not trie work:
that cell's ENGINE margin is 1.35 ms/1k fills vs 1.46 in the trie-off main
cells. Profile ms/1k read 1.29x the ENGINE margin on crab (5.09 vs 3.96)
and 1.09x on main (1.47 vs 1.35); the reason is not resolved, so crab
profile savings convert to ENGINE wall at about 0.78x (estimate).

| margin phase, ms/1k fills | crab r1 | crab r2 | main | delta (mean) |
|---|---|---|---|---|
| ENGINE wall (ms/blk / fills per blk) | 3.97 | 3.95 | 1.46 (trie-off cells) | **+2.50** |
| profile CPU, perf window | 5.07 | 5.12 | 1.47 | **+3.62** |

### 11.1 By region and by function

| region (crab line) | crab r1 / r2 | main | kind |
|---|---|---|---|
| per-order prepare (`prepare_one`, 5246; main: inline loop) | 2.74 / 2.69 | 0.59 | mixed, below |
| `same_batch_bid_top_ups` (5278) | 0.78 / 0.83 | 0 | crab-only |
| `phase2_reservation_basis` (5156) | 0.60 / 0.59 | 0.47 | shared |
| `open_order_counts` (5111) | 0.35 / 0.36 | 0.26 | shared |
| `d2_pool_takers` (5275) | 0.24 / 0.24 | 0 | crab-only |
| order id + `market_batches` push (`stitch_outcome`, 5247) | 0.20 / 0.22 | 0.13 | shared |
| `phase2_bid_floors` (5158) | 0.08 / 0.08 | 0 | crab-only |
| loop, collect, `excess_by_sender` | 0.07 / 0.11 | 0.03 | |
| **total** | **5.07 / 5.12** | **1.47** | |

`prepare_one` by source-line group (mean r1 / r2; "maps" = `HashMap` probe,
SipHash and rehash inside the group):

| group (lines) | ms/1k | maps | crab-only? |
|---|---|---|---|
| `proj.get` 5616, `AccountReader::position_px` 5618, `proj.entry` 5619 | 0.63 | 0.47 | yes |
| `take_open_slot` 5548 + `open_slots.entry` 5549 | 0.40 | 0.21 | shared (main 0.31) |
| `pos_nets.get` 5603 + `AccountReader::pos_net` 5605 (valuation) | 0.33 | 0.17 | yes |
| `account_check` 5639 (`placement_need`) | 0.32 | 0.05 | yes |
| projection 5679-5706 (`proj.get_mut`, release `checked_mul` x2, `im_delta`) | 0.28 | 0.13 | yes |
| `BalanceCache` load 5597 / set 5668 | 0.21 | 0.17 | shared (main 0.14) |
| `try_reserve_for_qty_cfg` 5583 | 0.17 | 0 | shared (main 0.13) |
| bid floor, Option B (5576-5578) | 0.11 | 0.06 | yes |
| `open_slots.insert` 5709 + `pool.entry` 5712 | 0.10 | 0.10 | yes |
| `margin_configs.get` 5568, `basis.get`, other | 0.12 | 0.06 | shared |
| **total** | **2.66** | **1.42** | crab-only 1.77 |

| gap part | crab | main | delta |
|---|---|---|---|
| crab-only per order (F1 account check in `prepare_one`) | 1.77 | 0 | +1.77 |
| crab-only per batch (top-ups 0.81, pool takers 0.24, bid floors 0.08, excess 0.02) | 1.15 | 0 | +1.15 |
| shared per-order steps (open slot, reservation, `BalanceCache`, id + push) | 1.20 | 0.73 | +0.47 |
| shared batch passes (`phase2_reservation_basis`, `open_order_counts`) | 0.97 | 0.75 | +0.22 |
| **total** | **5.09** | **1.47** | **+3.62** |

| what the innermost frames do (r1 / r2) | crab | main |
|---|---|---|
| SipHash (std `HashMap` hashing) | 1.39 / 1.32 | 0.58 |
| `HashMap` probe / insert / rehash | 1.15 / 1.14 | 0.35 |
| FixedPoint / i128 arithmetic | 0.65 / 0.64 | 0.12 |
| alloc / free, `format!` | 0.23 / 0.24 | 0.16 |
| other (compares, loads, first touch of order params) | 1.66 / 1.78 | 0.27 |

- **About 80% of the gap is crab-only code** (2.9 of 3.6); the shared
  steps cost +0.7 more, mostly more map lookups per order.
- **`HashMap` work is +1.6 of the +3.6:** 2.5 ms/1k on crab vs 0.9 on
  main. Per order, `prepare_one` does about 10-18 map lookups, keyed by
  sender, (sender, market) or market: `open_slots` (entry, then a second
  `insert`), `basis`, `margin_configs`, `bid_floors`, `pos_nets`, `proj`
  (get, entry, get_mut), `released`, `committed`, `pool` (get, entry),
  `BalanceCache` (load, set) and `tiers` (twice), plus the
  `market_batches` entry; main's loop does about 6 including that entry. Every `SenderFold` map starts empty, so they rehash as they
  grow (0.10 at 5619; 0.14 in the basis `closing` map, which main has too).
  The s82 A/B (main, 2026-09-30) swapped the exec maps to ahash with no
  measurable gain, so the lever is fewer lookups, not another hasher.
- **FixedPoint:** `checked_mul` is an i256 multiply and an i256 division by
  10^8 (ethnum `idivmod4`). Inclusive `checked_mul` in the margin phase
  0.51 vs 0.10 on main; on the whole execution thread 1.07-1.16 vs 0.27.
  Margin-phase callers: `placement_need` (margin.rs 115-117, 0.15),
  the projection release (5689 / 5691, 0.12), `try_reserve_for_qty_cfg`
  (6603, then `price * qty` again in `reserve_for_qty_cfg` 6589: the same
  product twice, also on main) and `position_terms` in the memo build.

### 11.2 Hot lines (r1, r2 in brackets)

- **`prepare_one` 2.69 [2.63].** 5619 0.27 [0.28]: `proj.entry` after a
  miss (0.15 entry incl. rehash, 0.12 first touch of the new slot). 5605
  0.27 [0.24]: `pos_net` -> `pos_sums` -> `cached_sums` -> the block memo's
  `get_or_init` -> `sums_of` -> `AccountView::build_with`, i.e. a full
  valuation the first time a sender appears in the block. 5639 0.25 [0.26]:
  `account_check` -> `placement_need`, three `checked_mul` (0.06 / 0.05 /
  0.05) and two `im_delta`. 5549 0.23 [0.18]: `open_slots.entry`. 5618 0.20
  [0.22]: `position_px` = `get_position` 0.14 + `mark` 0.07. 5548 0.18
  [0.17]: `take_open_slot`, 0.11 of it the `format!` of the "open order
  limit reached" reject (6757; main pays the same 0.10).
- **`same_batch_bid_top_ups` 0.78 [0.83].** `can_rest_shape` 0.22 [0.29],
  all on 7195 (`o.order_type`): the first read of each order's params, a
  cold load of ~150 ns per order. The i128 tick `%` (7199) is 0.003, so the
  section 8.2 idea of dropping it saves nothing. 7168 0.15 [0.16] (`basis`
  lookup and `p.params.quantity`, again a first touch), 7157 0.13
  (`AskDepth::upto` 0.11 and the `batch_asks` range fold).
- **`phase2_reservation_basis` 0.60 [0.59]** (main 0.47, same code): 7035
  `closing.entry` 0.32 (rehash 0.14), 7045 0.12 (first touch of the entry),
  6995 `order_books.get` 0.11 [0.09].
- **`open_order_counts` 0.35 [0.36]** (main 0.26): `trader_orders.get` per
  (book, sender) at `order_book.rs:1696` 0.25 [0.24].
- **`d2_pool_takers` 0.24:** collect of every checked taker (7214) 0.08,
  sort (7215) 0.07, `BTreeSet` dedup and collect (7221) 0.10.
- **Valuation memo (`cached_sums`, all phases) 1.01 [0.93]:** 0.94 is the
  memo build (`build_with` margin.rs:212 -> `position_terms`: 171
  `size.checked_mul(px)` 0.17, 174 `diff.checked_mul(size)` 0.16, 176
  `order_initial_margin` 0.13, mark / tiers closures 0.33). Only 0.25 of it
  is in the margin phase; the rest is `maker_free` in the match workers and
  `liq_view` (0.75 [0.71]). The persistent cache drops every trader the
  block wrote, and active traders write every block, so they are rebuilt.

### 11.3 `AccountReader::get_position` and `AccountMargins` are match-timer cost

| `AccountReader::get_position` caller | timer | r1 | r2 |
|---|---|---|---|
| `reduce_only_positions_for` (7287), Phase 3 setup on the execution thread | match | 0.73 | 0.77 |
| `AccountMargins` setup, `position_px` (5346) | match | 0.17 | 0.18 |
| `maker_position_px` (1485), match workers | match | 0.23 | 0.25 |
| `prepare_one`, `position_px` (5618) | margin | 0.14 | 0.17 |
| **total** | | **1.27** | **1.37** |

- **Only ~0.15 of section 10.2's 1.27 is margin phase.** Inside it,
  `trader_positions::find` (binary search of the trader's
  `Vec<Position>` by market) is 0.99 [1.09], the `Position` clone 0.16,
  `resident_positions` (dirty set + records map) 0.10 [0.12].
- **The same (sender, market) row is read up to three times per batch**
  (5618, 7287, 5346). Main's reduce-only tracking reads it from the overlay
  (`signed_position` 2.21 ms/1k), so crab is already ~1.1 cheaper there.
- **`AccountMargins` setup (5342-5355) is 0.43 [0.44]** (was 0.93 at
  `14236fa`): `position_px` 0.19, `insert_taker_only` 0.12 [0.10],
  `am.get` 0.07 [0.10], `pools.get` 0.03.

### 11.4 What to cut

Estimates (not measured), profile ms/1k fills on crab; ENGINE wall is about
0.78x. 18c is editing `order_book.rs`, torus-types, `native_executor.rs`
and the off-tick reject in `prepare_one` / `place_order_inner`, so almost
every candidate is in 18c's files.

1. **One sender entry per order in `prepare_one`:** merge `open_slots`,
   `pos_nets`, `released`, `committed` and `pool` into one per-sender
   entry, one `proj` entry instead of get + entry + get_mut, pre-sized fold
   maps. **-0.5 to -0.7.** `native_executor.rs` `prepare_one` /
   `SenderFold` (18c's files).
2. **`same_batch_bid_top_ups`:** skip a market with no candidate sell
   (count them in `prepare_one`), or keep the rest shape, price and
   quantity in `PreparedOrder` so the scan does not touch cold params.
   **-0.3 to -0.8** (the share of markets with a candidate is not
   measured). `native_executor.rs` (18c's files).
3. **`FixedPoint::checked_mul` i128 fast path** when both operands fit in
   63 bits, and one price x qty in `try_reserve_for_qty_cfg`. **Margin -0.3
   to -0.4**, execution thread ~-0.6. torus-types `lib.rs:87`,
   `native_executor.rs` 6589 / 6603 (18c's files).
4. **Keep traders' sums across blocks:** update the slot entry with the C6b
   delta at `end_resident` instead of dropping every trader the block
   wrote. **Margin -0.25**, up to -0.6 more in `maker_free` / `liq_view`.
   `native_executor.rs` `BlockSums::into_cache` / `cached_sums` (18c's
   files).
5. **`d2_pool_takers` from the fold** (`pool` and `pos_nets` already hold
   each sender's first checked market and pos_net). **-0.2.**
   `native_executor.rs` (18c's files).
6. **Pre-size the `phase2_reservation_basis` maps** (`closing`, `growth`,
   `out`). **-0.14** (main would gain ~0.10). `native_executor.rs` (18c's
   files).
7. **Market-indexed arrays for the block's marks and tiers**
   (`AccountReader::mark` / `tiers`, `margin_configs.get`). **-0.15.**
   `native_executor.rs` (18c's files).
8. **Match timer, same thread:** pass the pre-batch (signed, px) that
   `prepare_one` read into Phase 3, so `reduce_only_positions_for` and the
   `AccountMargins` setup stop re-reading it. **-0.6 to -0.8.**
   `native_executor.rs` (18c's files). A (trader, market) index in
   `TraderPositions` would cut `find` instead (`trader_positions.rs`, not
   18c's; low confidence, -0.3).

Items 1-7 add up to about 1.7-2.7 ms/1k (1.3-2.1 ENGINE wall): margin
~216 -> ~105-145 ms per block at ~55k fills per block, against main's
95-97 (estimate). The account check itself (`placement_need`, 0.32) and
the first valuation of a sender new to the block stay.

## 12. `239ff69` (P1-P4 + fix A) at 300 markets, trie off by default (2026-10-05)

Crab only, `perf/item6-phase1` @ `239ff69`: `82bd1a4` plus P1-P4
(matching per-fill fixes, `FixedPoint::checked_mul` fast path), fix A
(off-tick and dust rejected before the book), and the merges of section
11, trie maintenance off by default (`db6c9de`) and the RPC tick / lot
check (`44b7473`). The deltas below are all of these together. Same run
shape and build flags as section 10 (300 markets, cap 400, rate 76,000,
`RETRY_BUSY=1`, 120 s, oracle feed 30000 / 2000 ms / walk 0: 402/402
accepted in r1 and r2; warm 60 s without perf, then r1 and r2 with val0
perf). Node md5 `88e2ddfe`, load generator md5 `ecd6bf45` (both built from
`239ff69`). All cells AGREE, liveness PASS, ACCEPT. Driver and analysis:
`ozarchy-239ff69-campaign.sh`, `ozarchy-239ff69-analysis.md`.

- **Trie off without `EXTRA_ENV`:** no node had a `TORUS_NATIVE_TRIE*`
  variable. `db6c9de` reworded the boot line: the old "maintenance
  DISABLED" text is gone, and the new `native trie maintenance off
  (default; TORUS_NATIVE_TRIE_MAINTENANCE=1 enables)` line
  (`app.rs:3852`) appears once in each of the 9 node logs. No rebuild ran.
- **Load is comparable:** no new rejection class. `rejected_other` (where
  fix A counts) and `rejected_book` stay 0; the open-limit and cancelled
  rejects are the same order as `82bd1a4` (13.2-14.7M and 1.48M vs
  12.2-13.2M and 1.44-1.46M per cell). RPC call-level errors are 0-80 per
  node (`82bd1a4`: 0-20) against 0.2-1.0M successful calls, and the bench
  log has no tick or lot message.

### 12.1 Full node

| cell | matched/s | best60 | engine ms/1k | CPU-s per 1M fills | chain ms | pipelined ms | handoff wait ms | commit ms avg / p50 | blk/s | native blk/s |
|---|---|---|---|---|---|---|---|---|---|---|
| `239ff69` r1 (perf val0) | 65,620 | 100,163 | 9.92 | 80.5 | 691 | 146 | 0.18 | 631 / 408 | 1.1 | 1.06 |
| `239ff69` r2 (perf val0) | 64,751 | 100,722 | 9.93 | 80.7 | 664 | 139 | 0.02 | 628 / 414 | 1.1 | 1.07 |
| **`239ff69` mean** | **65,186** | 100,443 | **9.93** | **80.6** | 678 | 143 | 0.10 | 630 / 411 | 1.1 | 1.06 |
| `82bd1a4` mean (10.1) | 64,204 | 97,783 | 10.21 | 81.6 | 703 | 167 | 0.1 | 651 / 378 | 1.0 | 1.02 |
| main `92a02ed` trie off (9.1) | 100,573 | 131,094 | 7.06 | 60 | 564 | 173 | 0.24 | 481 / 336 | 1.6 | 1.33 |
| warm `239ff69` (60 s, no perf) | 76,710 | 99,593 | 8.35 | 62.2 | 587 | 127 | 0.17 | 520 / 358 | 0.9 | 0.89 |
| warm `82bd1a4` (60 s, no perf) | 71,355 | 98,432 | 8.85 | 63.6 | 656 | 158 | 0.08 | 543 / 350 | 0.8 | 0.81 |

| ratio to main | `82bd1a4` | `239ff69` |
|---|---|---|
| matched/s | 0.638x | **0.648x** |
| engine ms/1k | 1.44x | 1.41x |
| CPU-s per 1M fills | 1.36x | 1.34x |

- **+1.5% matched/s** (65.2k vs 64.2k), within the `82bd1a4` r1 / r2
  spread (65.1k vs 63.3k), so one round does not resolve it. The
  unprofiled warm cells give +7.5%. **Engine time per fill -2.7%** is
  consistent (9.92 / 9.93 vs 10.17 / 10.25); CPU per fill -1.2%, chain
  time -3.5%, commit interval -3%.
- **Caveat:** as in section 10, the main reference is the section 9.1 pair
  (not interleaved, no perf, `14236fa` load generator).

Engine phases on val0 (ms per block):

| | total | phase 1 | margin | match | settle | tail (incl. liquidation) | untimed | fills per native block |
|---|---|---|---|---|---|---|---|---|
| `239ff69` r1 / r2 | 550 / 526 | 50 / 47 | 212 / 207 | 94 / 89 | 134 / 126 | 44 / 42 | 16 / 15 | 55.4k / 52.9k |
| `82bd1a4` r1 / r2 | 553 / 573 | 45 / 47 | 216 / 221 | 96 / 102 | 128 / 135 | 48 / 46 | 20 / 21 | 54.4k / 55.8k |
| main trie off r1 / r2 | 459 / 466 | 51 / 50 | 95 / 97 | 149 / 151 | 155 / 158 | 0.05 | 9 | 65.1k / 66.0k |

Per 1k fills: margin 3.87 vs 3.96 (-2%), match 1.69 vs 1.80 (-6%), settle
2.39 vs 2.39, tail 0.79 vs 0.86. Margin is still 2.2x main per block.

### 12.2 val0 execution-thread profile

ms CPU per 1k fills over the perf window, mean of r1 / r2 (both builds
trie off, so unlike 10.2 the flush side is comparable too).

| bucket | `82bd1a4` | `239ff69` | delta |
|---|---|---|---|
| execution thread total | 20.8 | 19.4 | **-1.4 (-7%)** |
| matching | 2.98 | 2.54 | **-0.44 (-15%)** |
| margin, all buckets | 5.56 | 4.78 | **-0.79 (-14%)** |
| margin: `maker_fill_fits` | 1.46 | 1.13 | -0.33 |
| margin: `prepare_one` | 2.66 | 2.47 | -0.19 |
| settle | 2.04 | 1.69 | **-0.35 (-17%)** |
| `execute_batch_phases` glue | 5.35 | 5.51 | +0.15 |
| books drain / save / load | 1.90 | 1.93 | 0 |
| liquidation | 0.89 | 0.86 | 0 |
| leaf kind: compute | 14.0 | 12.2 | -1.9 |
| leaf kind: hashing | 4.00 | 4.52 | **+0.5** |
| leaf kind: state reads | 1.70 | 1.57 | -0.1 |
| `torus-flush-worker` thread | 3.8 | 3.3 | -0.5 |

| `82bd1a4` | `239ff69` | delta | symbol |
|---|---|---|---|
| 1.12 | 0.26 | **-0.86** | `FixedPoint::checked_mul` (inclusive) |
| 0.28 | 0 | -0.28 | ethnum `idivmod4` (the old 256-bit divide) |
| 0.12 | 0.27 | +0.16 | `__divti3` (the new i128 divide; 0.10 of it under `checked_mul`) |
| 2.08 | 1.56 | -0.52 | `OrderBook::match_at_level` (inclusive) |
| 2.05 | 1.71 | -0.35 | `settle_market_results_parallel` (inclusive) |
| 1.62 | 1.36 | -0.26 | `AccountReader::pos_sums` (inclusive) |
| 1.27 | 1.17 | -0.11 | `AccountReader::get_position` (self) |
| 0.83 | 1.29 | **+0.46** | `RandomState::hash_one<&Address>` (inclusive) |

- **P1-P4 hit what they target:** the `checked_mul` fast path removes the
  256-bit divide (-77%), and matching, margin and settle all drop 14-17%.
- **About a third of that comes back as SipHash** on `Address` keys
  (+0.46): mostly the new `RawTable<(Address, TraderMargins)>` from P1,
  plus `prepare_one`, `place_order_with_accounts` and
  `HashMap<Address, FixedPoint>`. A cheaper hasher on the Address-keyed
  maps is a small follow-up; s82 found SipHash -> ahash below A/B
  resolution node-wide, but this map is new.
- **The profile gain (-7%) is bigger than the engine-timer gain (-2.7%).**
  Not explained here; section 11 also found the crab profile reads 1.29x
  the engine timer.
- **Left:** the margin phase (207-212 vs 95-97 ms per block), the target of
  M1 (the section 11 cuts, plus books created from the market row's tick
  and lot).

## 13. C: per-action execution results (`9195c32`) at 300 markets (2026-10-05)

`feat/action-results` @ `9195c32` = `239ff69` + C: the execution result of
each action is mapped back to its position in the block body, and failed
actions are stored (reason code + message up to 96 bytes) in a v2
`CF_BLOCK_ACTION_STATUS` record, written on the flush worker. A block with
no failures writes the old v1 bytes. A `PlaceOrderBatch` with any failed
order is one failure entry (first failing order, its reason, how many
failed). The record is not hashed (not in `HASHED_CFS`, `NATIVE_ROOT_CFS`
or the EVM state root). Counters: `torus_exec_action_failures_total`,
`torus_exec_action_status_bytes_total`.

Same run shape and build flags as section 12, trie off by default. Node md5
`8db911fb`. A warm cell (60 s) and one 120 s cell, **both without perf**.
All cells AGREE, liveness PASS, ACCEPT. Driver
`ozarchy-action-results-campaign.sh`, cells `ozarchy-action-results-{warm,r1}`.

### 13.1 Full node

| cell | matched/s | best60 | engine ms/1k | chain ms | pipelined ms | handoff wait ms | commit ms avg / p50 | blk/s | native blk/s | fills per native block |
|---|---|---|---|---|---|---|---|---|---|---|
| C warm (60 s) | 77,425 | 96,297 | 8.25 | 598 | 128 | 0.05 | 521 / 348 | 0.9 | 0.89 | 57.1k |
| `239ff69` warm (60 s) | 76,710 | 99,593 | 8.35 | 587 | 127 | 0.17 | 520 / 358 | 0.9 | 0.89 | 56.5k |
| C r1 (no perf) | 67,436 | 94,807 | 9.57 | 669 | 140 | 0.03 | 621 / 492 | 1.1 | 1.11 | 53.9k |
| `239ff69` r1 / r2 (perf val0, 12.1) | 65,620 / 64,751 | 100,163 / 100,722 | 9.92 / 9.93 | 691 / 664 | 146 / 139 | 0.18 / 0.02 | 631 / 628 avg | 1.1 | 1.06 / 1.07 | 55.4k / 52.9k |

- **No measurable cost.** The warm cells are the like-for-like pair (no
  perf on either): matched/s +0.9%, engine ms/1k -1.2%, pipelined (flush)
  128 vs 127 ms per block, chain +2%.
- **C r1 is not directly comparable** with the `239ff69` round cells, which
  profiled val0. Its best60 is lower (94.8k vs ~100k); one cell, not
  resolved.

### 13.2 Failure records

From `metrics-{before,after}-val0.txt` (each cell boots fresh; native
blocks from `torus_exec_native_blocks_total`, including the drain):

| cell | actions | failed actions | failed share | native blocks | failures per block | status-CF bytes | bytes per block | bytes per failure |
|---|---|---|---|---|---|---|---|---|
| C warm | 57,823 | 25,790 | 45% | 237 | 108.8 | 1,762,736 | 7,438 | 68 |
| C r1 | 91,683 | 47,359 | 52% | 322 | **147.1** | 3,234,773 | **10,046** | 68 |

- **Failures are not ~0 on this load.** The bench's open-limit rejects
  (~41-45k orders per native block) happen at execution (Phase 2
  `prepare_one` -> `take_open_slot`), so nearly every batch that contains
  one becomes a failure entry. 68 bytes per failure matches the open-limit
  message length; a per-reason count per block was not taken.
- **Per action, not per order:** ~10 KB per block, ~0.14% of the ~7.2 MB
  flush batch. One entry per order would be ~45k entries per block.
- **Deterministic:** failure counts and byte totals are identical on all 3
  validators in both cells.
- **Still reported as executed:** IOC orders with zero fills (the bench's
  ~1.5M "rej cancelled" per cell) and crossing post-only orders; the
  executor reports both as success. Recording them needs the settle loop
  in `native_executor.rs`.
- **Reason codes come from the executor's error text** (margin,
  open_limit, tick, lot, price, batch_cap, fill, other); a changed message
  falls back to `other` until the executor returns a typed reason.

## 14. M1 (`90a752c`) at 300 markets, trie off by default (2026-10-05)

Crab only, `perf/item6-phase1` @ `90a752c` = `239ff69` + `7c365d4`
(off-tick stop-limit and non-positive limit rejected before the book; one
tick / lot text) + `e81aa2e` (books created from the market row's tick and
lot) + `49df3eb` (M1 margin phase: one entry per sender, per-batch market
table, fold pools, Phase 3 reuses Phase 2 reads) + `b9959e2` (M1: carry
written traders' sums across stable-mark blocks; dense mark / tier
indexes). It does **not** contain C (section 13). Same run shape and build
flags as section 12; node md5 `50dcee7e`, load generator md5 `22d30038`.
Warm (60 s, no perf), then r1 and r2 with val0 perf. All cells AGREE,
liveness PASS, ACCEPT; oracle 402/402, marks fresh 300/300. Driver and
analysis: `ozarchy-90a752c-campaign.sh`, `ozarchy-90a752c-analysis.md`.

- **Trie off:** no node had a `TORUS_NATIVE_TRIE*` variable; each of the 9
  node logs has one `native trie maintenance off (default; ...)` line and
  no rebuild.
- **Load is comparable:** the bench genesis has tick / lot 1.0 / 1.0 on all
  300 markets, so books built from the market row match the old
  auto-created ones. `rejected_book`, `rejected_other` and
  `rejected_margin` stay 0; matched / placed (0.700) and cancelled /
  placed (0.080) are unchanged. Open-limit rejects are higher (16.2-16.8M,
  0.81-0.83 of placed, vs 13.2-14.7M, 0.71-0.79): an existing class that
  grows with throughput (it already moved 13.2 -> 14.7M between the
  `239ff69` cells).

### 14.1 Full node

| cell | matched/s | best60 | engine ms/1k | CPU-s per 1M fills | chain ms | pipelined ms | handoff wait ms | commit ms avg / p50 | blk/s | native blk/s | fills per native block |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `90a752c` r1 (perf val0) | 74,733 | 103,867 | 7.82 | 73.3 | 579 | 144 | 0.06 | 569 / 444 | 1.2 | 1.22 | 52.0k |
| `90a752c` r2 (perf val0) | 78,186 | 102,324 | 7.62 | 72.3 | 565 | 143 | 0.03 | 550 / 500 | 1.3 | 1.29 | 51.8k |
| **`90a752c` mean** | **76,459** | 103,096 | **7.72** | **72.8** | 572 | 144 | 0.05 | 560 / 472 | 1.25 | 1.26 | 51.9k |
| `239ff69` mean (12.1) | 65,186 | 100,443 | 9.93 | 80.6 | 678 | 143 | 0.10 | 630 / 411 | 1.1 | 1.06 | 54.2k |
| main `92a02ed` trie off (9.1) | 100,573 | 131,094 | 7.06 | 60 | 564 | 173 | 0.24 | 481 / 336 | 1.6 | 1.33 | 65.5k |
| warm `90a752c` (60 s, no perf) | 91,886 | 106,211 | 6.47 | 58.1 | 476 | 121 | 0.07 | 447 / 368 | 1.2 | 1.22 | 51.3k |
| warm `239ff69` (60 s, no perf) | 76,710 | 99,593 | 8.35 | 62.2 | 587 | 127 | 0.17 | 520 / 358 | 0.9 | 0.89 | 56.5k |

| ratio to main | `239ff69` | `90a752c` |
|---|---|---|
| matched/s | 0.648x | **0.760x** |
| engine ms/1k | 1.41x | 1.09x |
| CPU-s per 1M fills | 1.34x | 1.21x |

- **+17.3% matched/s** (76.5k vs 65.2k), far outside either build's cell
  spread (74.7-78.2k vs 64.8-65.6k). The unprofiled warm cells give
  +19.8% (91.9k, 0.91x main). Engine time per fill -22%, CPU per fill
  -10%, chain time -16%, native blocks per second +19%; pipelined (flush)
  time and handoff wait unchanged.
- **Caveat:** as before, the main reference is the section 9.1 pair (not
  interleaved, no perf, `14236fa` load generator).

Engine phases on val0 (ms per block):

| | total | phase 1 | margin | match | settle | tail (incl. liquidation) | untimed |
|---|---|---|---|---|---|---|---|
| `90a752c` r1 / r2 | 407 / 395 | 54 / 52 | **126 / 125** | **46 / 45** | 130 / 128 | 35 / 30 | 16 / 16 |
| `239ff69` r1 / r2 | 550 / 526 | 50 / 47 | 212 / 207 | 94 / 89 | 134 / 126 | 44 / 42 | 16 / 15 |
| main trie off r1 / r2 | 459 / 466 | 51 / 50 | 95 / 97 | 149 / 151 | 155 / 158 | 0.05 | 9 |

Per 1k fills: **margin 2.41 vs 3.87 (-38%)**, **match 0.87 vs 1.69
(-49%)**, settle 2.49 vs 2.39 (+4%), tail 0.62 vs 0.79. Margin per block
is now 1.3x main (was 2.2x). The match timer halves although matching CPU
does not drop: it is execution-thread wall time and includes the
maker-side margin work done during matching (`maker_fill_fits`,
`maker_account`), which M1 halves (14.3).

### 14.2 end_resident (end-of-block upkeep)

**`end_resident` is not timed by any metric.** It runs after the flush
handoff (`app.rs:2588`) with no timer around it; the function
(`native_executor.rs:2903`) only sets the resident-rows gauges. Its time
lands in summarize.py's PHASE `residual_untimed`. The numbers below are
inclusive CPU on the execution thread from the val0 profile (it runs
single-threaded there, so CPU ~ wall), cross-checked against
`residual_untimed`.

| | end_resident ms per block | ms per 1k fills | `BlockSums::into_cache` | `ResidentRows::apply` | `TraderPositions::apply` | end_resident self |
|---|---|---|---|---|---|---|
| `239ff69` r1 / r2 | 64.8 / 69.8 (mean 67.3) | 1.00 / 1.03 | - | 27.8 / 32.2 | 19.1 / 20.7 | 13.3 |
| `90a752c` r1 / r2 | 102.0 / 92.0 (mean 97.0) | 1.61 / 1.48 | 20.4 / 15.1 | 30.4 / 28.3 | 20.3 / 19.0 | 26.0 |
| delta | **+29.7** | **+0.53** | +17.8 | -0.6 | -0.2 | +12.7 |

- **Cut 4 costs ~30 ms per block here, about 3.5x the ~8.6 ms expected**
  (marks steady, oracle walk 0, ~52k fills per block).
- **`BlockSums::into_cache` (~18 ms):** ~40% borsh decode of `Position`
  rows and ~30% `position_terms`: it re-decodes and re-sums every written
  trader's positions. end_resident's own time (+12.7 ms) is the inlined
  walk of the block's position rows that feeds it.
- **Cross-check:** `residual_untimed` on val0 is 88.8 / 86.5 ms per block
  in `239ff69` and 120.0 / 116.6 in `90a752c` (+30.7; warm +26.4),
  matching the profile. The other exec-thread stages outside the engine
  (verify, save_books) are unchanged.
- **Well paid for** (margin -84 and match -46 ms per block), but it is now
  the largest new single cost. Next trim: avoid the re-decode by carrying
  the positions `TraderPositions` already decoded, or summing from the
  delta's decoded rows. A timer (metric) around `end_resident` would make
  it visible without a profile.

### 14.3 val0 execution-thread profile

ms CPU per 1k fills over the perf window, mean of r1 / r2 (all execution
threads).

| bucket | `239ff69` | `90a752c` | delta |
|---|---|---|---|
| execution threads total | 19.39 | 16.52 | **-2.87 (-15%)** |
| margin, all buckets | 4.78 | 2.33 | **-2.45 (-51%)** |
| margin: `prepare_one` | 2.47 | 1.45 | -1.02 |
| margin: `maker_fill_fits` | 1.13 | 0.68 | -0.46 |
| margin (other) | 1.10 | 0.13 | -0.97 |
| margin: `same_batch_bid_top_ups` | 0.04 | 0.03 | 0 |
| `execute_batch_phases` glue | 5.51 | 4.15 | -1.36 |
| matching | 2.54 | 2.72 | +0.18 |
| settle | 1.69 | 1.74 | +0.05 |
| books drain / save / load (contains end_resident) | 1.93 | 2.42 | **+0.49** |
| liquidation | 0.86 | 0.73 | -0.12 |
| leaf kind: hashing | 4.52 | 3.51 | **-1.0** |
| leaf kind: compute | 12.2 | 10.4 | -1.8 |
| `torus-flush-worker` thread | 3.28 | 3.01 | -0.27 |
| process CPU ms per 1k (user + sys) | 152 | 137 | -15 (-10%) |

| `239ff69` | `90a752c` | delta | symbol (inclusive unless noted) |
|---|---|---|---|
| 1.81 | 1.16 | -0.65 | `AccountReader::get_position` (self 1.18 -> 0.52) |
| 1.36 | 0.58 | -0.77 | `AccountReader::pos_sums` |
| 0.94 | 0.49 | -0.45 | `AccountReader::maker_account` |
| 1.29 | 0.94 | -0.36 | `RandomState::hash_one<&Address>` |
| 0.95 | 0.35 | **-0.59** | SipHash write (self) |
| 0.25 | inlined | -0.24 visible | `d2_pool_takers` (M1 fold pools) |
| 1.56 | 1.17 | -0.39 | `OrderBook::match_at_level` (`maker_fill_fits` under it) |
| 0.27 | 0.18 | -0.08 | `FixedPoint::checked_mul` |
| 1.02 | 1.55 | **+0.53** | `end_resident` |
| 0.05 | 0.17 | +0.12 | `Position` borsh decode (mostly `into_cache`) |
| 1.17 | 1.34 | +0.17 | `__KeccakF1600` (self) |

- **M1 hits its targets:** margin CPU halves; the SipHash P1 added in
  `239ff69` (+0.5) is more than removed; position reads drop 0.45-0.77
  each (one entry per sender, Phase 3 reusing Phase 2 reads).
- `reduce_only_positions_for` and `phase2_reservation_basis` have no frame
  in either build (inlined), so their share shows in the callers.
- **Left:** per block the engine total is already below main (401 vs 463
  ms), but per fill it is still 1.09x (main runs more fills per block at
  1.6 blk/s) and CPU per fill 1.21x. Outside the engine, end_resident
  (14.2) is the biggest item to cut next.

## 15. Step 2 window and Gate 2 at 10 markets (`c58775f`, 2026-10-05)

### 15.1 Step 2 feasibility: how much of end_resident can move off the execution thread

No build. From the section 14 `90a752c` val0 profiles (300 markets):
`perf script` timestamps on the main execution thread, aligned to val0's
"executing finalized block" / "block done" log lines (clock offset pinned
to within 0.5 ms per run). Analysis and per-block CSVs:
`ozarchy-step2-window-analysis.md`, `ozarchy-step2-tools/`.

- **The window is shorter than assumed.** At `90a752c`, begin_resident
  of block N+1 runs **before** signature verification (`app.rs:1985`;
  verify starts at `app.rs:1996`), so verify, nonces and
  `sort_native_actions` are not between end_resident(N) and
  begin_resident(N+1). Body decode is not on the execution thread at
  all. Today's window holds the end of block N (fills hand-off, metrics,
  state-hash check, overlay drop; ~10 ms CPU) and block N+1's preamble
  (height / parent checks, overlay setup).
- **The bench is exec-bound.** At every native block's "block done" the
  next block was already queued (49/49 and 54/54 in the perf windows,
  302/304 over the load); execution runs 35-42 s (p50) behind consensus,
  and waiting-for-work idle is 0 ms. Idle time hides nothing here; case
  (b) below only adds time the thread spends without user-mode samples
  (kernel, blocking).

end_resident here: 109.7 / 102.0 ms CPU per block (wall 121 / 116). (Section
14.2 gives 102 / 92 because it divides by a different block count.)

| ms per block, r1 / r2 | today: window up to begin_resident(N+1) | begin_resident(N+1) moved to just before `NativeExecContext::new_env` |
|---|---|---|
| window, wall (mean / p10 / p50 / p90) | 25.2 / 16.8 / 24.6 / 37.9 and 21.9 / 12.7 / 20.3 / 31.7 | 107.1 / 92.2 / 102.9 / 127.2 and 99.5 / 84.1 / 99.2 / 117.5 |
| window, busy CPU (mean) | 12.2 / 10.2 | 87.3 / 81.0 |
| (a) hidden, busy work only | **11.8 / 10.2** | **82.8 / 76.1** |
| (b) hidden, including no-sample time | **24.1 / 21.9** | **95.3 / 87.3** |
| blocks where all of end_resident is hidden | 1/37, 0/38 | 16/37, 16/38 |
| end_resident still exposed (wall) | ~83 | ~15 (p50 8-11, p90 ~38) |

- **Dependency:** block N+1 needs end_resident's output (resident rows
  with block N applied, the `into_cache` sums, decoded positions, marks)
  before (1) the resident rows are attached to the overlay, before its
  first clone in `new_env`, and (2) `attach_resident_block` hands the sums
  and marks to the context before `begin_block_oracle` and margin.
  Everything between end_resident(N) and that point (verify, which reads
  sessions; the replay guard, which reads nonces; the action-status
  write; `sort_native_actions`) reads no positions or balances.
- **To check before building it:** the resident rows share the
  `resident_books` mutex with the books, which `new_env` locks, so a
  worker must take the rows out of the holder rather than hold the lock
  through the window; verify's parallel signature recovery would compete
  for cores (headroom: ~8.6 busy cores of 32 in the perf window); and
  end_resident cannot start much earlier, because its input
  (`own_pending_delta`) is ready only after the book save, with ~2 ms of
  CPU after it.

### 15.2 Gate 2 at 10 markets: `c58775f` vs main `92a02ed`, both trie off

Crab `c58775f` = `90a752c` (M1) + C + typed reject reasons; trie off by
default. Main `92a02ed` (node md5 `31a95c65`, as sections 5 and 9.1) with
`TORUS_NATIVE_TRIE_MAINTENANCE=0` through `EXTRA_ENV`, because crab now
defaults to off and main to on. Section 5 shape: 10 markets, rate 76,000,
`RETRY_BUSY=1`, 120 s; crab with the oracle feed (10/10 marks fresh). Both
arms use the `c58775f` load generator (md5 `265b8969`); crab node md5
`494d8812`. Order: crab warm (60 s), crab r1 (perf val0), main r1, crab r2
(perf val0), main r2 -- interleaved, main without perf. All cells AGREE,
liveness PASS, ACCEPT. Checks: no `TORUS_NATIVE_TRIE*` on crab nodes and
one "off (default" boot line per crab log; `=0` in every main node's
environment and one "DISABLED" line per main log; no rebuilds. Driver and
analysis: `ozarchy-c58775f-10m-campaign.sh`,
`ozarchy-c58775f-10m-analysis.md`. The cells ran after 15.1 finished, so
the analysis did not load the host.

| cell | matched/s | best60 | engine ms/1k | CPU-s per 1M fills | chain ms | pipelined ms | commit ms avg / p50 | blk/s | native blk/s | fills per native block | val0 backlog_preverify |
|---|---|---|---|---|---|---|---|---|---|---|---|
| crab r1 (perf val0) | 157,907 | 191,106 | 4.12 | 41.1 | 382 | 71 | 396 / 320 | 2.0 | 2.01 | 64.7k | 381,867 |
| main r1 | 178,518 | 211,908 | 3.77 | 38.4 | 364 | 78 | 350 / 301 | 2.3 | 2.29 | 68.8k | 277,457 |
| crab r2 (perf val0) | 154,490 | 182,156 | 4.14 | 41.8 | 377 | 72 | 395 / 329 | 2.0 | 2.02 | 63.3k | 400,879 |
| main r2 | 171,506 | 205,051 | 3.83 | 39.1 | 369 | 77 | 338 / 248 | 2.4 | 2.21 | 68.6k | 289,899 |
| **crab mean** | **156,198** | 186,631 | **4.13** | **41.5** | 380 | 72 | 395 / 325 | 2.0 | 2.01 | 64.0k | 391,373 |
| **main mean (trie off)** | **175,012** | 208,480 | **3.80** | **38.8** | 366 | 78 | 344 / 275 | 2.35 | 2.25 | 68.7k | 283,678 |
| crab warm (60 s, no perf) | 184,735 | 188,108 | 3.51 | 33.8 | 325 | 63 | 333 / 236 | 2.1 | 2.14 | 62.6k | 116,741 |
| section 5 PF1 `0ebfd71` mean | 127,460 | 151,035 | 5.57 | 49.2 | 458 | 159 | 449 / 369 | 1.7 | 1.71 | 64.1k | 487,801 |
| section 5 main mean (trie on) | 175,904 | 207,179 | 3.80 | 39.3 | 368 | 175 | 343 / 264 | 2.4 | 2.22 | 68.8k | 198,474 |

| crab / main | section 5 (PF1 vs main, trie on) | `c58775f` vs main (both trie off, interleaved) |
|---|---|---|
| matched/s | 0.72x | **0.893x** (r1 pair 0.885x, r2 pair 0.901x) |
| best60 | 0.73x | 0.895x |
| engine ms/1k | 1.47x | 1.09x |
| CPU-s per 1M fills | 1.25x | 1.07x |
| val0 backlog_preverify | 2.46x | 1.38x |

- **Gate 2 at 10 markets (>= 0.9x): 0.893x, ~1% short.** The two pairs
  fall on either side of the line, so the result is within the cell
  spread (crab 154.5-157.9k, main 171.5-178.5k).
- **All of the change since section 5 is crab:** +22.5% (156.2k vs
  127.5k). Main is unchanged (175.0k trie off vs 175.9k trie on): at 10
  markets main's flush does not limit it (handoff wait ~0.1 ms either
  way), unlike at 300 markets (9.1).
- **Perf bias:** both measured crab cells ran under perf, the main cells
  did not. In section 5 the profiled main cell was not below the
  unprofiled one, so the bias is taken as < ~1-2%; not ruled out. The
  crab warm cell (184.7k, no perf) is above main's 120 s cells, but 60 s
  warm cells always run higher (section 5 PF1 warm 158.7k vs 127.5k), so
  it is not a comparison.
- **Load is comparable:** matched / placed 0.794 (crab) and 0.797 (main)
  as in section 5; crab cancelled / placed 0.0047 (was 0.0043); book,
  other and margin rejects 0 everywhere, so C and the typed reasons add no
  reject class. Crab's open-limit share fell from 0.78 to 0.67-0.69 of
  placed as placed rose 16%.

Engine phases on val0:

| | total | phase 1 | margin | match | settle | tail | untimed | PHASE residual_untimed |
|---|---|---|---|---|---|---|---|---|
| crab r1 / r2, ms/blk | 266.5 / 262.0 | 58.0 / 54.6 | 88.2 / 86.9 | 46.0 / 45.3 | 60.9 / 62.4 | 9.4 / 8.9 | 3.9 / 3.9 | 53.2 / 54.1 |
| main r1 / r2, ms/blk | 259.4 / 263.0 | 65.0 / 67.3 | 66.8 / 66.2 | 51.8 / 53.0 | 68.4 / 68.9 | 0.1 / 0.1 | 7.3 / 7.3 | 36.7 / 37.1 |
| section 5 PF1 r1 / r2, ms/blk | 360.1 / 353.2 | 44.8 / 47.1 | 163.1 / 157.0 | 63.3 / 61.3 | 63.7 / 62.7 | 17.2 / 16.7 | 8.0 / 8.4 | 40.1 / 40.3 |
| **crab, ms/1k fills** | 4.13 | 0.88 | **1.37** | 0.71 | 0.96 | 0.14 | 0.06 | 0.84 |
| **main, ms/1k fills** | 3.80 | 0.96 | **0.97** | 0.76 | 1.00 | 0.00 | 0.11 | 0.54 |
| section 5 PF1, ms/1k fills | 5.56 | 0.72 | 2.50 | 0.97 | 0.99 | 0.26 | 0.13 | 0.63 |

- Margin per fill -45% vs PF1 and now 1.41x main; match is below main and
  settle level. The engine gap per fill (+0.33 ms/1k) is margin (+0.40)
  and tail (+0.14), partly offset by phase 1 and match.
- **end_resident is small at 10 markets:** 7.2 / 7.5 ms per block (0.10
  ms/1k; `into_cache` 1.6 / 1.9) from the profile, as `c58775f` has no
  timer. It explains ~7 of crab's +17 ms per block `residual_untimed`
  over main. Its 300-market cost (97 ms, 14.2) scales with written
  traders' positions; here a trader holds at most 10.

val0 execution-thread profile, ms CPU per 1k fills (main column: section
5's profiled main r1, trie on; this run's main cells were not profiled):

| bucket | section 5 PF1 r1 | `c58775f` mean | main (section 5) |
|---|---|---|---|
| execution threads total | 12.49 | **10.68** | 10.05 |
| margin, all buckets | 2.38 | **1.05** | 0.16 |
| margin: `prepare_one` | 1.37 | 0.68 | - |
| margin: `maker_fill_fits` | 0.60 | 0.28 | - |
| matching | 3.72 | 3.59 | 3.38 |
| settle | 1.33 | 1.06 | 1.52 |
| books drain / save / load (incl. end_resident) | 1.00 | 1.21 | 1.18 |
| `FixedPoint::checked_mul` | 0.84 | 0.16 | |
| `torus-flush-worker` thread | 2.39 | 1.02 | 2.47 |
| rpc-worker threads | 23.0 | **7.4** | 4.2 |
| `torus-gossip-ve` thread | 7.5 | 9.9 | 8.5 |
| `torus-ingress-v` thread | 4.6 | 6.1 | 5.2 |
| process CPU (user / sys / total) | 60.0 / 17.5 / 77.5 | 47.8 / 9.1 / **56.8** | 41.8 / 6.1 / 47.9 |

- **Left at 10 markets:** per block, crab's engine (264 vs 261 ms) and
  chain time (380 vs 366 ms) are close to main's. The gap shows up
  instead as fewer native blocks per second (2.01 vs 2.25) with fewer
  fills per block (64.0k vs 68.7k), longer consensus views on val0
  during the load (396 vs 342 ms; wall per committed block 498 vs 419),
  and more CPU per fill in the RPC, gossip-verify and ingress threads.
  These are observations; this run does not show the cause.
- The 300-market half of Gate 2 was not run here (section 14: 0.760x at
  `90a752c`).

## 16. Gate 2 at 10 markets without perf (`5524646`, 2026-10-05)

Crab `5524646` = `c58775f` + `b2bcfaa` (end_resident timers) + `1242d80`
(the sums carry reuses C7's decoded positions; `into_cache` is gone).
Same shape as 15.2, but **neither arm runs perf**: 10 markets, rate
76,000, `RETRY_BUSY=1`, 120 s; crab with the oracle feed (10/10 fresh);
main `92a02ed` (node md5 `31a95c65`) with `TORUS_NATIVE_TRIE_MAINTENANCE=0`
through `EXTRA_ENV`. Crab node built with main's flags (mold, frame
pointers, `line-tables-only`): md5 `a2088294`; both arms use the
`5524646` load generator (md5 `8edea436`). Order: crab warm (60 s), crab
r1, main r1, crab r2, main r2. All cells AGREE, liveness PASS, ACCEPT;
trie checks as in 15.2 (one "off (default" line per crab log, `=0` and
one "DISABLED" line per main log, no rebuilds); exe md5 checked on every
node pid. Driver and analysis: `ozarchy-5524646-10m-campaign.sh`,
`ozarchy-5524646-10m-analysis.md`.

| cell | matched/s | best60 | engine ms/1k | CPU-s per 1M fills | chain ms | view ms (val0, load) | native blk/s | fills per native block | submit act/s | val0 backlog_preverify |
|---|---|---|---|---|---|---|---|---|---|---|
| crab r1 | 153,295 | 188,106 | 4.19 | 41.3 | 394 | 393 | 2.008 | 66.0k | 1,331 | 353k |
| main r1 | 174,755 | 208,848 | 3.81 | 38.8 | 366 | 335 | 2.242 | 68.8k | 1,437 | 275k |
| crab r2 | 152,327 | 181,510 | 4.16 | 41.9 | 386 | 396 | 2.008 | 64.3k | 1,323 | 423k |
| main r2 | 178,100 | 215,500 | 3.80 | 38.3 | 370 | 336 | 2.244 | 70.1k | 1,435 | 255k |
| **crab mean** | **152,811** | 184,808 | **4.18** | **41.6** | 390 | 395 | 2.008 | 65.1k | 1,327 | 388k |
| **main mean (trie off)** | **176,428** | 212,174 | **3.81** | **38.6** | 368 | 335 | 2.243 | 69.4k | 1,436 | 265k |
| crab warm (60 s) | 186,805 | 192,680 | 3.49 | 33.1 | 327 | 328 | 2.109 | 64.5k | 1,550 | 106k |
| 15.2 crab `c58775f` (perf val0) | 156,198 | 186,631 | 4.13 | 41.5 | 380 | 396 | 2.01 | 64.0k | 1,297 | 391k |
| 15.2 main | 175,012 | 208,480 | 3.80 | 38.8 | 366 | 342 | 2.25 | 68.7k | 1,438 | 284k |

| crab / main | r1 pair | r2 pair | mean of pairs | 15.2 |
|---|---|---|---|---|
| matched/s | 0.877x | 0.855x | **0.866x** | 0.893x (0.885 / 0.901) |
| best60 | 0.901x | 0.842x | 0.871x | 0.895x |
| matched incl. drain | 0.897x | 0.881x | 0.889x | 0.909x / 0.918x |
| engine ms/1k | 1.10x | 1.10x | 1.10x | 1.09x |
| CPU-s per 1M fills | 1.06x | 1.09x | 1.08x | 1.07x |

- **Gate 2 at 10 markets (>= 0.9x): 0.866x, missed.** Removing perf did
  not help: crab is 2.2% below 15.2's profiled crab (152.8k vs 156.2k),
  main is level (176.4k vs 175.0k). The 15.2 perf bias was therefore not
  holding crab down; the 0.893x -> 0.866x move is about one cell spread
  (crab 152.3-153.3k, main 174.8-178.1k) and is not attributed to
  `1242d80`.
- **Load is comparable:** no slow load-generator cell (submit follows
  node throughput under `RETRY_BUSY`); matched / placed 0.794 crab and
  0.797 main as in 15.2; margin, book and other rejects 0 everywhere.
  Crab's drain is longer (33-34 s vs 30-31 s), so more of its work lands
  after the bench window.
- **The gap has not moved:** native blk/s 2.008 (as 15.2) vs 2.24; crab's
  load-window views (395 ms) equal its chain time while main's (335 ms)
  are shorter than its chain (368 ms); margin 1.40 vs 0.95 ms/1k;
  backlog_preverify refusals 1.46x main (15.2: 1.38x).

end_resident from the new timers (metrics before / after deltas divided
by the engine count; see the harness note below):

| cell | native blocks | end_resident ms/blk val0 / val1 / val2 | rows | positions |
|---|---|---|---|---|
| crab r1 | 446 | 4.76 / 4.49 / 4.86 | 2.75 / 2.65 / 2.88 | 1.51 / 1.39 / 1.49 |
| crab r2 | 427 | 5.15 / 5.09 / 4.96 | 3.04 / 2.97 / 2.97 | 1.56 / 1.56 / 1.51 |

- Whole run (idle + bench + drain) ~4.9 ms/blk: rows ~59%, positions
  ~31%, rest (memo merge, drops) ~0.5 ms. Scaled to the bench + drain
  window by the engine ratio: **~5.8-5.9 ms/blk** (rows ~3.4, positions
  ~1.8), ~0.09 ms per 1k fills, ~1.5% of chain. 15.2's 7.2-7.5 ms was a
  profile (CPU) estimate, so the lower figure is not a clean `1242d80`
  effect.
- end_resident is ~6 of crab's ~18 ms/blk `residual_untimed` excess over
  main; it does not explain the 10-market gap.
- **Harness gap:** `summary.json` reports end_resident 0.0 in every crab
  cell. `summarize.py` has the new phase, but `WIDE_COLS` in
  `tools/matched-bench/run-cell.sh` does not list
  `torus_exec_end_resident{,_rows,_positions}_seconds_sum`, so the
  sampler never records them and `residual_untimed` still contains
  end_resident. Fix: add the three `_sum` columns.

## 17. Where the 10-market gap outside the engine goes (2026-10-05)

Read-only analysis of the section 15 and 16 cells plus one new profiled main
cell, `ozarchy-main-prof-10m`: main `92a02ed` (node md5 `31a95c65`) with
`TORUS_NATIVE_TRIE_MAINTENANCE=0`, same shape and perf as the 15.2 crab cells
(10 markets, rate 76,000, `RETRY_BUSY=1`, 120 s, val0 `cycles:u` 45 s from
35 s), after a 60 s warm cell. AGREE, liveness PASS, ACCEPT, trie checks OK;
176,150 matched/s and 433 ms per native block, level with the unprofiled main
cells. Crab profiles: 15.2's `c58775f` r1 / r2. Driver
`ozarchy-main-prof-10m-campaign.sh`; analysis and scripts
`ozarchy-10m-gap-analysis.md`, `ozarchy-10m-gap-tools/`.

Sections 15.2 and 16 average engine, chain and fills per block over bench +
drain. Over the load window alone the gap is on the execution thread (val0-2,
4 cells per arm):

| ms per native block, load window | crab (`c58775f`, `5524646`) | main | crab - main |
|---|---|---|---|
| fills per native block | 76.7k | 78.3k | -2% |
| exec block (= chain) | **485.5** | **434.0** | **+51.5** |
| engine (margin / tail / rest) | 344.2 (112.5 / 11.2 / 220.5) | 314.4 (75.6 / 0.1 / 238.7) | +29.8 (+36.9 / +11.1 / -18.2) |
| verify + replay guard + save_books | 77.1 | 78.3 | -1.2 |
| residual untimed | 64.2 | 41.2 | **+23.0** |
| exec wall per 1k fills | 6.33 | 5.54 | 1.14x (-> 0.875x) |

- Exec thread busy 98% on both arms. The scoped workers' CPU per block is
  equal (430-456 vs 443 ms); the exec main thread has +56 ms CPU per block
  in the profile, which is the whole gap.

### 17.1 Views

Once the 64-deep exec channel (`EXEC_QUEUE_DEPTH`) is full,
`on_committed_block` -> `dispatch_to_exec` blocks the HotStuff thread on
`exec_tx.send` (`app.rs:6177` at `c58775f`), and views run at the
execution rate. Crab fills the channel 25-30 s earlier.

| val0 | crab (4 cells) | main (4 cells + main-prof) |
|---|---|---|
| channel full at (s into the load) | 70-78 | 97-106, one cell never |
| ramp (20 s to fill): view ms | **314-330** | **325-348** |
| ramp: views/s; executed native blk/s | 3.0-3.2; 2.02-2.06 | 2.9-3.1; 2.28-2.36 |
| ramp: proposal build ms | 107-115 | 107-115 |
| channel full: view ms; exec block ms | 512-556; 513-554 | 444-490; 444-496 |
| channel full: `on_committed_block` ms (val0, `c58775f`) | 367-386 (ramp ~80) | - |
| load-window view ms | 393-397 | 335-360 |

- **No view phase is slower on crab while consensus runs free:** proposal
  build (select 55-60, mirror 42-46, attest 9-10 ms), DA reconstruct
  (14 vs 12-13), insert/persist, vote delay and QC times match main.
  Dissemination is clean in every cell.
- **It is a wait, not CPU contention:** 395 vs 335 ms is the mix of ramp
  views (~320 ms) and exec-paced views (~530 ms), with crab ~45 s of the
  122 s exec-paced vs ~15-25 s on main. hotstuff-algo CPU is +10% (126-131
  vs 112-125 ms per committed block) but off the critical path.
- No exec-watermark pacing (`TORUS_EXEC_THROTTLE_WATERMARKS` unset): 396-399
  vs 400-401 actions per native block.

### 17.2 CPU per fill

Perf window, ms CPU per 1k fills, crab r1 / r2 vs main-prof (trie off):

| thread | crab | main | delta | per unit of work |
|---|---|---|---|---|
| rpc-worker | 7.27 / 7.51 | 3.89 | **+3.49** | 344 vs 363 µs per request |
| `torus-gossip-ve` | 9.91 / 9.97 | 9.19 | +0.75 | 1,582 vs 1,579 µs per gossip-received action |
| `torus-ingress-v` | 6.10 / 6.01 | 5.32 | +0.73 | 963 vs 914 µs per gossip-received action |
| torus-execution | 10.40 / 10.96 | 9.60 | +1.08 | see 17.3 |
| process total (user / sys) | 55.7 / 58.0 | 46.1 | +10.7 | +7.6 / +3.2 |

| rpc-worker class | crab | main | delta |
|---|---|---|---|
| hex decode (`hex::FromHex` in `parse_bytes`) | 3.10 | 1.18 | **+1.92** |
| JSON / HTTP / jsonrpsee / `method_weight` | 2.51 | 1.44 | +1.07 |
| bincode (`decode_action_bin`) | 0.52 | 0.25 | +0.27 |
| Keccak, mempool admit, other | 1.26 | 1.03 | +0.23 |

- **RPC: refusals, not crab code.** CPU per request is equal; crab gets 2.0x
  the requests per fill (21.5 vs 10.7 per 1k) because 2.3x are refused by
  `backlog_preverify` (18.5 vs 7.9 per 1k). Admitted requests per fill are
  equal over the run (2.19-2.23 vs 2.25-2.26 per 1k). The full-hex shed
  decode is the biggest item (+1.92), the HTTP/JSON layer the second.
  Oracle feed: 30 of ~146k requests.
- **Signature verify is not a cause:** Keccak and secp256k1 per action are
  equal (gossip 957 / 403 vs 958 / 398 µs; ingress 453 / 188 vs 447 / 187).
  Section 6.4's 1.22-1.27x is not reproduced; ingress is 1.05x per action,
  mostly `__sched_yield`.
- **The gossip / ingress per-fill excess is a window effect:** in the 45 s
  window crab verifies 1.63 actions per executed action vs 1.54 (admission
  runs ahead of the slower execution); over the whole run 1.185 vs 1.187,
  with equal nonce-expiry (15.8% vs 15.7% of submitted).
- Contention bound: exec main thread run-queue wait +4-7 ms per committed
  block (37.5-40.3 vs 30.6-35.2, schedstat). Inferred upper bound for what
  the extra CPU costs execution.

### 17.3 Untimed exec time

Exec main thread, inclusive ms CPU per native block by direct callee of
`execute_committed_block_with` (perf window, ~69k fills per block):

| callee | crab r1 / r2 | main | crab - main |
|---|---|---|---|
| **`action_results::native_failures`** (`{closure#8}`) | **19.5 / 21.1** | 0 | **+20.3** |
| of which `NativeAction::canonical_bytes` | 14.2 / 15.6 | 0 | +14.9 |
| `end_resident` | 7.2 / 7.5 | 0 | +7.3 |
| `own_pending_delta` | 1.2 / 1.1 | 0 | +1.2 |
| context drop (`drop_glue`) | 1.2 / 2.2 | 0.2 | +1.5 |
| `sort_native_actions` | 10.5 / 9.2 | 9.2 | +0.7 |
| block fn self + `drop_slow` | 0.7 / 1.1 | 4.3 | -3.4 |
| `run_liquidations_with` (engine tail) | 10.6 / 10.5 | 0 | +10.6 |
| `execute_batch_phases` (engine) | 226.8 / 237.2 | 215.5 | +16.5 |

- **Most of the untimed excess is C (`9195c32`), not end_resident.**
  `native_failures` sits between the engine and save_books timers and runs
  for 152 failures per native block (68,786 over 451 blocks in crab r1;
  52% of actions), ~134 µs each. `body_position` re-encodes the failing
  action and every same-sender, same-category action in the batch list and
  in `executed` with `canonical_bytes()` whenever a sender has more than one
  such action, which the backlogged bench makes common. At 300 markets
  (section 13) this was within noise; at 10 markets it is ~4% of the block.
- Crab-only untimed CPU ~30 ms, ~27 net of main-only frames, covers the
  wall excess (+23 ms load window, +17.5 bench + drain). end_resident is
  7.3 ms CPU here, 5.8 ms wall from the `5524646` timers.
- Engine, for reference: `prepare_one` +46.7 against -40 of main-only
  frames (`take_open_slot`, `get_position`, `try_reserve_for_qty_cfg`,
  `rustc_entry`); `stitch_outcome` +8.8; liquidation 10.6 (`pos_sums` 3.5,
  `traders_after` 1.9, cooldown / pending reads 3.2) with no liquidations
  in the bench.

### 17.4 What to cut

Estimates against crab's 485.5 ms per native block (load window):

| # | cut | saves (est.) | where |
|---|---|---|---|
| 1 | Map failures to body positions by index: carry each action's original index through the sort, drop `canonical_bytes` from `body_position` | ~20 ms/blk (~0.27 ms/1k), crab only | `sort_native_actions` / `sort_deterministic` (`native_executor.rs:9749-9792`), `native_failures` / `body_position` (`action_results.rs:99-170`), `app.rs:2359` |
| 2 | Margin (engine) | up to ~37 ms/blk | section 11.4 (`prepare_one`) |
| 3 | Liquidation scan when nothing can be liquidatable (design check; marks constant in this bench) | up to ~10 ms/blk | `run_liquidations_with`, `liq_view` `pos_sums`, `traders_after` |
| 4 | end_resident off the exec thread (step 2) | ~6 ms/blk | `end_resident`, section 15.1 |
| 5 | `own_pending_delta` and crab's context drop | ~2.7 ms/blk | `NativeStateOverlay::own_pending_delta`, `drop(ctx)` |
| 6 | Shed by peeking the bincode tag before the hex decode (both arms) | ~3.5 of crab's 7.4 rpc ms/1k; exec only via contention, <= 4-7 ms/blk | `parse_bytes` (`crates/torus-rpc/src/types.rs:93-96`), shed task in `torus.rs` |
| - | `sort_native_actions` clones every action (both arms) | ~4 ms/blk on both; do with 1 | `native_executor.rs:9755` |

- 1 + 4 + 5 (~29 ms) put crab at ~456 ms per block: **~0.92x main**
  (est.); with 3, ~0.94x. Signature verify, gossip verify, the oracle feed
  and consensus are not cuts.
- Instrumentation: a timer around `native_failures`. (The end_resident
  columns in `run-cell.sh` `WIDE_COLS`, section 16, are in `e65411d`.)

## 18. Step 2 (`4acdc59`) at 300 markets (2026-10-06)

Crab `perf/item6-phase1` @ `4acdc59` = `5524646` + `2333ba4` (step 2:
end_resident runs on a `torus-end-resident` worker; begin_resident(N+1)
moves to just before `NativeExecContext::new_env`; new metric
`torus_exec_end_resident_wait_seconds`) + `e65411d` (`run-cell.sh` samples
the end_resident timers). Node md5 `b5a2cad4`; trie off by default. Main
`92a02ed` (node md5 `31a95c65`) with `TORUS_NATIVE_TRIE_MAINTENANCE=0`
through `EXTRA_ENV`. Both arms use the `4acdc59` load generator (md5
`1de55ded`) and the same build flags (mold, frame pointers,
line-tables-only).

The shape is section 14's: 300 markets, cap 400, rate 76,000,
`RETRY_BUSY=1`, 120 s, oracle feed on crab only. Order: crab warm (60 s),
crab r1, main r1, crab r2, main r2, interleaved. Perf runs on val0 in the
r2 pair only, on both arms, with the same command as sections 14 and 15.
r1 therefore gives a ratio without perf, and r2 gives the breakdown.

All cells AGREE, liveness PASS, ACCEPT, bench rc 0.
- **Trie and binary checks:** every node pid had the expected exe md5 and
  trie setting, with one off / DISABLED boot line per log and no rebuilds.
- **Death watcher:** it checked the nodes, load generator and oracle feed
  every 0.5 s, and all of them lived until the harness stopped them.
- **Earlier runs:** three earlier runs were voided by SIGKILLed processes
  (val1, val2, then the load generator, each 45-60 s into the load).
  `kill`/`tkill`/`tgkill` auditing showed no sender for the third. This
  run ran under a system-wide `perf record -e signal:signal_generate`
  (`sig == 9`): every SIGKILL in it came from the harness, a helper
  before the first cell, or a desktop app killing its own children. None
  hit a bench process during a cell. The cause of the three deaths is
  unknown (no OOM kill, no panic, no core dump).
- **Driver and analysis:** `ozarchy-4acdc59-300m-campaign.sh`,
  `ozarchy-4acdc59-300m-analysis.md`.

### 18.1 Gate 2 at 300 markets

| cell | matched/s | best60 | incl drain | engine ms/1k | CPU-s per 1M fills | chain ms | commit ms avg / p50 | blk/s | native blk/s | fills per native block |
|---|---|---|---|---|---|---|---|---|---|---|
| crab r1 | 82,865 | 105,307 | 87,511 | 7.95 | 71.9 | 533 | 531 / 535 | 1.4 | 1.37 | 51.5k |
| main r1 | 101,165 | 130,494 | 105,911 | 7.15 | 58.7 | 580 | 518 / 404 | 1.4 | 1.33 | 66.8k |
| crab r2 (perf val0) | 79,755 | 104,753 | 85,778 | 8.21 | 70.4 | 564 | 550 / 419 | 1.3 | 1.29 | 53.0k |
| main r2 (perf val0) | 94,482 | 134,550 | 103,021 | 7.49 | 59.0 | 621 | 522 / 389 | 1.4 | 1.21 | 68.7k |
| **crab mean** | **81,310** | 105,030 | 86,645 | **8.08** | **71.2** | 548 | 540 / 477 | 1.35 | 1.33 | 52.2k |
| **main mean (trie off)** | **97,824** | 132,522 | 104,466 | **7.32** | **58.9** | 601 | 520 / 397 | 1.4 | 1.27 | 67.8k |
| crab warm (60 s, no perf) | 98,001 | 111,743 | 98,961 | 6.68 | 56.8 | 409 | 368 / 236 | 1.7 | 1.71 | 47.2k |
| section 14 `90a752c` mean (perf) | 76,459 | 103,096 | | 7.72 | 72.8 | 572 | 560 / 472 | 1.25 | 1.26 | 51.9k |
| section 14 main ref (9.1) | 100,573 | 131,094 | | 7.06 | 60 | 564 | 481 / 336 | 1.6 | 1.33 | 65.5k |

| crab / main | r1 pair | r2 pair | mean | section 14 |
|---|---|---|---|---|
| matched/s | 0.819x | 0.844x | **0.832x** | 0.760x |
| best60 | 0.807x | 0.779x | 0.793x | 0.79x |
| matched/s incl drain | 0.826x | 0.833x | 0.829x | |
| engine ms/1k | 1.11x | 1.10x | 1.10x | 1.09x |
| fills per native block | 0.77x | 0.77x | 0.77x | 0.79x |
| native blk/s | 1.03x | 1.06x | 1.04x | 0.95x |

- **Gate 2 at 300 markets (>= 0.9x): 0.832x, not met.** It was 0.760x in
  section 14.
  - Crab is +6.3% vs section 14 (81.3k vs 76.5k; with perf on both,
    r2 79.8k vs 78.2k, +2%).
  - Warm: 98.0k vs 91.9k.
- **Perf bias:** perf costs main ~7% here (94.5k vs 101.2k) and crab 3.8%,
  so the r2 pair does not favour main.
- **Per block, crab now matches or beats main:** chain 548 vs 601 ms,
  native blk/s 1.33 vs 1.27, wall per native block 731 / 777 vs
  750 / 827 ms.
- **The gap is fills per block (0.77x).**
  - Actions per block are equal (383 vs 389).
  - But crab places 192-198 orders per action vs 223-231, matches 0.698 of
    placed vs 0.767, and rejects 0.81-0.87 of placed at the open limit
    vs 0.63-0.68.
  - Crab's matched / placed has been 0.70 in every crab cell since
    section 9; main's is 0.767. This run does not show the cause.
- **Engine time:** per fill it is 1.10x main (margin), but per block it is
  below main (409 / 435 vs 478 / 515 ms). Main's engine/1k is 1-6% above
  9.1 tonight, so part of crab's +4% over section 14 is the host.
- **Load is comparable:**
  - No slow load-generator cell: every bench ran its full 122-124 s, and
    submit rates follow node throughput (crab 1,023 / 981 actions/s, main
    993 / 937).
  - Margin, book and other rejects are 0 everywhere.

### 18.2 end_resident on the worker, and the execution thread's wait

val0, ms per native block over bench + drain (summary.json timers):

| | end_resident (worker) | rows | positions | rest | first / last 60 s | wait mean | wait p50 / p90 | `residual_untimed` |
|---|---|---|---|---|---|---|---|---|
| crab r1 | 65.0 | 34.1 | 22.7 | 8.2 | 93 / 47 | **4.75** | **0.4 / 15.6** | 61.9 |
| crab r2 | 68.4 | 35.6 | 23.8 | 9.0 | 97 / 50 | **5.28** | **0.4 / 22.3** | 63.9 |
| crab warm | 49.2 | 25.7 | 18.2 | 5.3 | 70 / 52 | 3.71 | 0.3 / 14.6 | 48.3 |
| main r1 / r2 | - | - | - | - | - | - | - | 47.0 / 48.7 |
| section 14 `90a752c` (inline) | 102 / 92 (profile) | 30.4 / 28.3 | 20.3 / 19.0 | | | all exposed | | 120.0 / 116.6 |
| section 15.1 estimate | ~100 | | | | | ~15 | 8-11 / ~38 | |

- **~92% of end_resident is hidden.** The execution thread waits about 5 ms
  per block for 65-68 ms of worker time, a third of section 15.1's ~15 ms
  estimate; the median block does not wait at all (0.4 ms).
- **`residual_untimed` is down 56 ms per block** vs section 14. Crab's
  excess over main is ~15 ms per block (was ~70).
- **The worker total (65-68 ms) is below section 14.2's 97 ms** (the sums
  carry reuses the decoded positions, `5524646`). The first 60 s costs
  about twice the last 60 s.

### 18.3 Does the worker slow signature verify?

| | crab r1 | main r1 | crab r2 | main r2 | section 14 r1 / r2 |
|---|---|---|---|---|---|
| verify phase, ms per native block | 28.2 | 28.9 | 28.7 | 29.1 | 26.2 / 27.3 |
| verify phase, ms per action | 0.074 | 0.074 | 0.075 | 0.075 | 0.070 / 0.072 |
| `torus-gossip-ve` ms CPU per action (whole run) | 1.09 | 1.07 | 1.11 | 1.11 | 1.11 / 1.09 |
| `torus-ingress-v` ms CPU per action (whole run) | 1.63 | 1.51 | 1.81 | 2.00 | 2.12 / 2.15 |
| execution thread run-queue wait, ms per block | 85.8 (14.1%) | 85.2 (13.6%) | 105.9 (16.4%) | 106.5 (15.6%) | 99.4 / 93.0 (14.1%) |

val0 profile, r2 pair (45 s window, cycles:u):

| | crab r2 | main r2 | section 14 r2 |
|---|---|---|---|
| verify phase ms per native block (window) | 40.0 | 41.1 | 34.8 |
| `torus-gossip-ve` ms user CPU per action | 1.31 | 1.14 | 0.98 |
| `torus-ingress-v` ms user CPU per action | 1.19 | 1.16 | 1.11 |
| `torus-end-resid` ms per 1k fills | 1.28 | - | - |
| execution threads ms per 1k fills | 15.54 | 14.15 | 16.33 |
| books drain / save / load ms per 1k fills | **1.10** | 0.71 | 2.36 |

- **No visible slowdown.** Per action, verify time and verify-thread CPU
  match main's in both pairs, and the execution thread's run-queue wait
  equals main's.
- In the perf window only, gossip-verify CPU per action is +15% vs main;
  the whole-run counters for the same cell show no difference.
- **The execution thread waits for a CPU 14-16% of its time on both arms**
  (load ~40 on 32 threads), on every exec-bound comparison on this host.
- **Books drain / save / load (which contained end_resident) is
  1.10 ms/1k, down from 2.36.** The execution-thread total is 15.54, down
  from 16.33; margin buckets are unchanged.

## 19. Gate 2 with B-blind (`31cea69`), 300 and 10 markets (2026-10-06)

Crab `perf/item6-phase1` @ `31cea69` = `ab12c75` (cuts 1, 2, 5 and 6, RPC
shed by action tag, `detach.sh`) + `5e39ae8` (margin-cut counters: sell cuts
by pool / non-pool, zero / partial fill and ticks above the reservation
price; sell top-ups full / partial / none; maker margin cancels; reduce-only
cuts) + `31cea69` (B-blind: non-pool sell top-up to B0 + 10 bps, partial,
replaces the s89 same-batch bound). Node md5 `3a3c8951`; trie off by
default. Main `92a02ed` (node md5 `31a95c65`) with
`TORUS_NATIVE_TRIE_MAINTENANCE=0`. Both arms use the `31cea69` load
generator (md5 `91ef7ffa`) and the same build flags.

Shape per block: crab warm (60 s), crab r1, main r1, crab r2, main r2,
interleaved; 120 s cells, cap 400, rate 76,000, `RETRY_BUSY=1`, oracle feed
on crab only. The 300-market block (section 14 / 18 shape) ran first, then
the 10-market block (section 15 / 16 shape). **No perf on any cell.**

All 10 cells: rc 0, AGREE, liveness PASS, accepted.
- **Binary and trie checks:** crab 18/18 node pids exe `3a3c8951` with no
  trie variable; main 12/12 exe `31a95c65` with maintenance 0. Oracle stale
  0, fresh 300/300 and 10/10.
- **Process deaths:** none. Every node, the load generator and the oracle
  feed lived until the harness stopped them.
- **SIGKILL trace:** the system-wide `signal_generate` (`sig == 9`) trace
  (section 18) ran through the whole campaign.
- **Launch:** each cell ran as its own systemd user unit through
  `detach.sh`, 06:14-07:12; no cargo / rustc on the host.
- **Driver and analysis:** `ozarchy-bblind-gate2-campaign.sh`,
  `ozarchy-bblind-31cea69-gate2-analysis.md`,
  `ozarchy-bblind-31cea69-gate2-counters.txt`.

### 19.1 Gate 2 at 300 markets

| cell | matched/s | best60 | incl drain | native blk/s | fills per native block | chain ms | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills | actions per block |
|---|---|---|---|---|---|---|---|---|---|---|---|
| crab warm | 124,722 | 146,466 | 128,454 | 1.387 | 65.2k | 434 | 5.36 | 1.50 | 0.68 | 44.0 | 373 |
| crab r1 | 110,593 | 131,359 | 113,400 | 1.475 | 63.6k | 500 | 6.31 | 1.75 | 0.76 | 55.0 | 382 |
| main r1 | 100,317 | 133,575 | 105,414 | 1.301 | 66.7k | 574 | 7.08 | 1.45 | 2.34 | 59.6 | 394 |
| crab r2 | 110,112 | 133,905 | 114,405 | 1.520 | 63.6k | 494 | 6.20 | 1.72 | 0.77 | 54.5 | 388 |
| main r2 | 100,869 | 134,726 | 107,282 | 1.328 | 66.4k | 569 | 7.04 | 1.45 | 2.28 | 59.6 | 396 |

| crab / main | r1 pair | r2 pair | mean | section 18 (`4acdc59`) |
|---|---|---|---|---|
| matched/s | 1.102x | 1.092x | **1.097x** | 0.832x |
| best60 | 0.983x | 0.994x | 0.989x | 0.793x |
| matched/s incl drain | 1.076x | 1.066x | 1.071x | 0.829x |
| fills per native block | 0.953x | 0.959x | 0.956x | 0.77x |
| native blk/s | 1.13x | 1.14x | 1.14x | 1.04x |
| chain ms | 0.87x | 0.87x | 0.87x | 0.91x |
| engine ms/1k | 0.89x | 0.88x | 0.89x | 1.10x |
| CPU-s per 1M fills | 0.92x | 0.91x | 0.92x | 1.21x |

- **Gate 2 at 300 markets (>= 0.9x): 1.097x, met.** It was 0.832x in
  section 18.
  - Crab mean 110.4k vs 81.3k in section 18 (+36%).
  - Main mean 100.6k, equal to section 14's reference (100.6k); section
    18's 97.8k had perf on the r2 pair.
- **The fills-per-block gap (section 18) is nearly closed: 0.956x, from
  0.77x.** Crab's matched / placed is 0.766 vs main's 0.767; it was 0.698 in
  every crab cell since section 9. Cancelled orders fell from 82 to 0.63 per
  1k placed. The section 18 gap was the same-batch sell bound.
- **Per fill, crab is now cheaper than main:** engine 6.26 vs 7.06 ms/1k,
  match phase 0.77 vs 2.3 ms/1k. Margin is still 1.20x main (1.73 vs
  1.45 ms/1k; 2.56 on `4acdc59` r1).
- **best60 is 0.99x main;** crab's lead is sustained throughput (native
  blk/s 1.14x, chain 0.87x).
- **Not B-blind alone:** `4acdc59..31cea69` also carries cuts 1, 2, 5 and 6.
  This run does not split the gain between them.

### 19.2 Gate 2 at 10 markets

| cell | matched/s | best60 | incl drain | native blk/s | fills per native block | chain ms | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills | actions per block |
|---|---|---|---|---|---|---|---|---|---|---|---|
| crab warm | 208,228 | 212,229 | 187,185 | 2.381 | 56.1k | 255 | 3.34 | 0.96 | 0.65 | 31.3 | 323 |
| crab r1 | 175,816 | 206,930 | 170,471 | 2.268 | 63.7k | 337 | 3.87 | 1.015 | 0.69 | 38.0 | 370 |
| main r1 | 172,831 | 205,425 | 170,877 | 2.195 | 69.4k | 371 | 3.81 | 0.961 | 0.78 | 38.8 | 396 |
| crab r2 | 172,079 | 202,131 | 167,862 | 2.226 | 63.1k | 333 | 3.88 | 1.031 | 0.71 | 38.3 | 364 |
| main r2 | 176,017 | 211,036 | 171,368 | 2.260 | 68.7k | 365 | 3.79 | 0.951 | 0.74 | 38.8 | 395 |

| crab / main | r1 pair | r2 pair | mean | section 16 (`5524646`) | section 15 (`c58775f`) |
|---|---|---|---|---|---|
| matched/s | 1.017x | 0.978x | **0.997x** | 0.866x | 0.893x |
| best60 | 1.007x | 0.958x | 0.983x | 0.871x | 0.895x |
| matched/s incl drain | 0.998x | 0.980x | 0.989x | 0.889x | 0.91x |
| fills per native block | 0.918x | 0.918x | 0.918x | 0.94x | 0.93x |
| native blk/s | 1.03x | 0.98x | 1.01x | 0.90x | 0.89x |
| chain ms | 0.91x | 0.91x | 0.91x | 1.06x | 1.04x |
| engine ms/1k | 1.02x | 1.02x | 1.02x | 1.10x | 1.09x |

- **Gate 2 at 10 markets (>= 0.9x): 0.997x, met.** It was 0.866x in section
  16; crab mean 173.9k vs 152.8k.
- **Margin: crab 1.023 vs main 0.956 ms/1k, +0.067 (r1 +0.054, r2
  +0.080).** In line with 18c's +0.1 ms/1k for B-blind; ozarchy has no
  `ab12c75`-only run to isolate it. The margin gap was +0.45 ms/1k in
  section 17 (1.40 vs 0.95).
- **What is left:** crab carries fewer actions per block (367 vs 396), so
  fills per native block are 0.92x; chain time (0.91x) and native blk/s
  (1.01x) offset it. matched / placed is 0.797 on both arms.

### 19.3 B-blind counters

Per 1k placed, mean of crab r1 / r2; all three nodes survived, so the
process-lifetime counters cover the whole cell. Raw values are per node.

| counter | 300 markets | raw | 10 markets | raw |
|---|---|---|---|---|
| non-pool zero-fill sell cuts, all buckets (option A metric) | **0.630 (0.063% of placed; r1 0.084%, r2 0.042%)** | 15.1k | **0.038 (0.0038%)** | 1,233 |
| by ticks above reservation: t0 / t1_2 / t3_5 / t6_10 / t11_30 / t31p | 0 / 0.182 / 0.267 / 0.181 / 0 / 0 | 0 / 4,374 / 6,399 / 4,344 / 0 / 0 | 0 / 0.021 / 0.012 / 0.005 / 0 / 0 | 0 / 680 / 395 / 158 / 0 / 0 |
| non-pool partial-fill cuts | 0 | 0 | 0 | 0 |
| pool zero / partial cuts | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| sell top-ups full / partial / none | 492.4 / 0 / 0 | 11.86M / 0 / 0 | 396 / 0 / 0 | 12.77M / 0 / 0 |
| maker margin cancels | 0 | 0 | 0 | 0 |
| reduce-only cuts | 0 | 0 | 0 | 0 |

- **Option A (threshold 0.5% of placed) is not needed:** the cut rate is
  0.063% at 300 markets and 0.0038% at 10 markets.
- **Every remaining cut is non-pool, zero-fill, 1-10 ticks above the
  reservation price.** At 300 markets the cut total equals
  `rejected_cancelled` (the section 18 proxy, 82 per 1k on `4acdc59`).
- **Every top-up was full;** 49% of placed orders got one at 300 markets,
  40% at 10 markets.
- The counters are exported with a `_total` suffix.

## 20. Live-feed idle check, reject share, pre-merge tests (`5584880`, 2026-10-06)

`perf/item6-phase1` @ `5584880` (= `31cea69` + test-only
`ResidentBooks::drop_sums_cache()` + docs). Node md5 `721e48d7` (not
`3a3c8951`; the code diff is test-only, the cause of the md5 change is not
checked); bench-throughput `cc12451c`. Same build flags as section 19.

### 20.1 Harness: `ORACLE_FEED_DRAIN=1` (`bench/oracle-feed-drain` @ `ffc245e`)

With `ORACLE_FEED=1`, `run-cell.sh` SIGSTOPs the oracle feed for the drain,
so marks go stale and the liquidation walk takes its cheap path. On a real
chain the feed never stops. The opt-in `ORACLE_FEED_DRAIN=1` (needs
`ORACLE_FEED=1`; default 0 runs exactly as before) keeps the feed running
through the drain, then pauses it, runs a second (settle) drain of up to
60 s, and only then takes the after-snapshots and the digest.

`health.py drain --feed-live` is done when, for the quiet window:
- the order counters (placed, matched, resting) do not move and every node
  keeps committing;
- `torus_mempool_native_size` <= the feed's own entries (2 rounds x 3
  validators x ceil(markets / 256) = 12 at 300 markets). No metric splits
  the mempool by action kind, so this is a proxy for "no bench action
  pending";
- no flush or trade-writer work is pending;
- `torus_exec_queue_depth` (committed blocks not yet executed) <= 2 on
  every sample.

It writes `drain-feed-live.tsv` and `drain.json .feed_live`. Commits:
`37ff2af` (mode + 10 tests) and `ffc245e` (review fix: the window filter
compared a rounded sample time against an unrounded window start, so about
half the time the sample before the window leaked into p95 / max; plus a
test that runs the real `stop_oracle_feed` on the paused feed). Matched-bench
tests: `test_health.py` 29 OK, `test_harness.py` 101 OK, the other files OK.

### 20.2 The cell

One cell, section 19's 300-market crab shape (cap 400, rate 76,000,
`RETRY_BUSY=1`, 120 s, feed 30000 / 2000 ms / walk 0, trie variable unset,
no perf) plus `ORACLE_FEED_DRAIN=1`; own systemd unit through `detach.sh`,
09:01-09:05; no warm-up cell, no main reference, no SIGKILL trace. OUT
`~/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/`, driver
`ozarchy-feeddrain-5584880-cell.sh`.

| check | result |
|---|---|
| run-cell rc | 0, VALIDITY ACCEPT |
| feed-live drain | **drained in 38.2 s** (timeout 780 s), feed live |
| settle drain (feed paused) | drained after 11 s |
| agreement / liveness | AGREE / PASS |
| node exe | 3/3 pids `721e48d7`, no trie variable |
| process deaths | none (3 nodes, load generator rc 0, feed rc 0 at stop) |
| oracle | 510 sent / 510 accepted / 0 rejected; stale 0, fresh 300/300 |
| matched/s | 111,440 (best60 131,435), 1.008x section 19 crab r1 (110,593) |

Exec ms per oracle-only block, timed per block from the node logs
(`execution pipeline: executing finalized block` -> `block done`), quiet
window (drain s 28.2-38.2):

| node | oracle-only blocks | p50 ms | p95 ms | max ms | empty block p50 ms |
|---|---|---|---|---|---|
| val0 | 4 | 4.21 | 4.91 | 4.91 | 2.78 |
| val1 | 4 | 4.58 | 5.92 | 5.92 | 2.90 |
| val2 | 4 | 4.76 | 4.93 | 4.93 | 2.83 |
| pooled | 12 | ~4.6 | 5.92 | **5.92** | |

- **Target <= 20 ms: met.** Every oracle-only block after the load (heights
  682-1160) was <= 5.92 ms. The only slow blocks were the last two bench
  blocks (h680 / h681, 400 / 292 actions, 135-189 ms).
- **The harness's own figure is empty-block time.** It reports a mean per
  1 s sample from `torus_exec_chain_seconds`; the idle chain runs bursts of
  ~27 blocks/s with 1-3 s gaps, so 0 of 21 intervals held one block and its
  p50 2.80 / max 3.24 ms are empty blocks. Exact numbers need per-block
  timing in the node.
- **Exec lag** (`torus_exec_queue_depth`, 1 s samples): 61-66 while the load
  backlog drains (0-26 s), then 0 on all nodes from 27-28 s (the backlog
  cleared in one sample); quiet window min / median / max 0 / 0 / 1.
- **Walk 0:** prices did not move. Marks were fresh (not the stale cheap
  path), but nothing pushed accounts toward liquidation. The s89
  `ubench_epoch` probe (~548 ms per empty block with a live feed, 18c) is a
  different measurement; this cell confirms the E2-E4 cut on a real node
  for this shape only. With `ORACLE_WALK_BP` > 0, liquidations that fill
  orders would move the order counters and the feed-live drain would time
  out (fails safe).

### 20.3 Open-limit reject share (section 19 cells, no new cells)

Deltas `metrics-after` - `metrics-before` on val0 of each section 19 cell
(`~/bench-results-matched/ozarchy-bblind-31cea69-<shape>-<cell>/`), as in
section 13.2.

| cell | actions | failed actions (share) | open-limit order rejects (share of orders) | open-limit rejects per native block |
|---|---|---|---|---|
| 300 mk crab r1 | 109,542 | 57,112 (52.1%) | 17.20M (41.9%) | 46.7k |
| 300 mk crab r2 | 113,608 | 60,097 (52.9%) | 18.24M (42.8%) | 52.3k |
| 300 mk main r1 | 105,289 | n/a | 16.34M (41.3%) | 61.2k |
| 300 mk main r2 | 106,927 | n/a | 16.81M (41.9%) | 61.8k |
| 10 mk crab r1 | 149,312 | 80,539 (53.9%) | 23.70M (42.3%) | 48.9k |
| 10 mk crab r2 | 148,158 | 79,698 (53.8%) | 23.35M (42.0%) | 47.8k |
| 10 mk main r1 | 147,383 | n/a | 22.99M (41.5%) | 61.6k |
| 10 mk main r2 | 149,758 | n/a | 23.55M (41.9%) | 62.0k |

- **Unchanged since section 13.2** (45-52% failed actions at `239ff69`).
- `torus_exec_action_failures_total` has no reason label and is absent on
  main `92a02ed`. At the order level `torus_orders_rejected_open_limit_total`
  is the only non-zero reject reason (margin, book, other are 0), so nearly
  all failed actions are open-limit batches; the counters cannot show it
  per action.
- `OPEN_ORDER_BUDGET` was unset in all 10 cells (`open_order_budget='unset'`).
- Main's higher rejects per block are its larger blocks (no feed: ~394 vs
  ~300-330 actions per block), not a higher reject rate.

### 20.4 Pre-merge tests

`cargo test --workspace -q` on `5584880` (TESTING.md "Before merging to
main"): rc 0, **2806 passed, 0 failed**, no panics, 565 s.

## 21. Moving prices, bench in-flight cap, crab / main on the new shape (2026-10-06)

### 21.1 Moving prices (walk 10 bp) at 300 markets

Crab `5584880` (node `721e48d7`) with oracle feed 30000 / 2000 ms and
`ORACLE_FEED_DRAIN=1` (section 20), walk 10 bp (W10) or 0 (W0); main
`92a02ed` (`31a95c65`, `TORUS_NATIVE_TRIE_MAINTENANCE=0`, no feed). Both arms
run bench-throughput `cc12451c`. Section 19 shape, trie off, no perf. Order:
crab warm (W0), W10 r1, W0 r1, main r1, W10 r2, W0 r2, main r2; 09:39-10:22.
Driver `~/bench-results-matched/ozarchy-walk-5584880-campaign.sh`, analysis
`ozarchy-walk-5584880-analysis.txt` and `-loadwin.txt`.

All 7 cells: rc 0, AGREE, liveness PASS, drained (37-52 s; settle 10-11 s),
no deaths, exe md5 as staged, oracle stale 0 / fresh 300.

| cell | matched/s | best60 | native blk/s | fills per native block (load) | chain ms (load) | engine ms/1k | margin ms/1k | match ms/1k |
|---|---|---|---|---|---|---|---|---|
| W10 r1 | 107,806 | 123,932 | 1.434 | 75.2k | 667 | 6.62 | 1.69 | 0.81 |
| W0 r1 | 110,139 | 132,076 | 1.463 | 75.3k | 647 | 6.27 | 1.74 | 0.75 |
| main r1 | 99,799 | 133,513 | 1.293 | 77.2k | 753 | 7.03 | 1.43 | 2.35 |
| W10 r2 | 106,297 | 125,707 | 1.455 | 73.0k | 650 | 6.65 | 1.72 | 0.81 |
| W0 r2 | 112,586 | 132,006 | 1.488 | 75.7k | 636 | 6.21 | 1.71 | 0.77 |
| main r2 | 97,025 | 135,705 | 1.268 | 76.5k | 770 | 7.14 | 1.46 | 2.36 |

| mean of r1 / r2 | W10 / main | W0 / main | W10 / W0 |
|---|---|---|---|
| matched/s | **1.088x** | 1.132x | 0.961x |
| best60 | 0.927x | 0.981x | 0.945x |
| engine ms/1k | 0.937x | 0.881x | 1.063x |
| margin ms/1k | 1.18x | 1.19x | 0.99x |
| match ms/1k | 0.34x | 0.32x | 1.06x |

- **The merge gate holds with moving prices:** W10 is 1.088x main. Walk
  costs ~4% matched/s and ~6% engine ms per fill (match +6%); margin per
  fill does not change.
- **No liquidations:** `torus_liquidations_triggered_total` stayed 0 on every
  node in every cell, sampled every 2 s through the drain (`liq-drain.tsv`).
  The walk stays within +-80 bp, which never pushed an account below
  maintenance. This pair tests moving marks, not liquidations firing.
- **Fills per native block and chain ms are load-window values** from val0's
  `sampler.csv`: in `ORACLE_FEED_DRAIN=1` mode `summary.json` counted bench +
  drain, and the live feed adds 350-630 oracle-only blocks per node. Fixed on
  `bench/max-in-flight` (`3a75fe7`, `0d6f2c3`): in that mode the whole
  exec-chain ruler uses the load window.
- **Oracle-only blocks in the drain** (node logs, `executing finalized block`
  -> `block done`, pooled over 3 nodes): W10 p50 4.01, p95 24.35, max 48.62
  ms; W0 3.29 / 4.95 / 33.57 ms. Steady state (from 2 s after the last load
  block): W10 p50 4.07, max 4.92; W0 3.60 / 5.35. Exec lag 65-66 at drain
  start, 0 after 25-30 s, 0-2 in the quiet window.
- **One slow oracle-only block per node right after the load** (20-49 ms,
  within ~1 s of the last load block): both W10 runs and W0 r1, not W0 r2 or
  the warm cell. Every other oracle-only block was <= 6.9 ms.
- **Empty blocks cost ~2.5-2.8 ms with the feed live after the load**, vs
  ~0.03 ms before the load, on main, and ~0.06 ms with the feed paused
  (section 19 cell). Likely M1's mark carry passing over positions on every
  block while marks are fresh (inferred from timings only).

### 21.2 Bench in-flight cap (`bench/max-in-flight` @ `0d6f2c3`)

Section 13.2 / 20.3: ~40% of orders are refused at the 1000 open-order limit.
Cause (s83): the bench offers more than the chain executes, actions wait
~16 s in the mempool (~16.5k nonce-expired evictions per node), and the
mempool puts a sender's cancel-all ahead of its earlier places, so places
sent before a cancel-all commit after it.

`--max-in-flight N` (opt-in; `MAX_IN_FLIGHT=N` in `run-cell.sh`): a sender
fires only while it has < N signed native actions in flight. Slots are freed
on commit (one shared tail fetches every block body with `torus_getBlockBody`
from val2 and matches nonce + signature), on an RPC refusal (including
per-item errors in a partly accepted batch), or after nonce + 70 s. BUSY
retries keep the slot; transport errors do not free it. With the cap,
`OPEN_ORDER_BUDGET`'s estimate is orders placed since the sender's last
**committed** cancel-all plus its in-flight places (fills ignored: it
overestimates, which costs extra cancel-alls, not refusals). When the cap is
set, `run-cell.sh` exports `TORUS_RPC_MAX_RESPONSE_MB=64` (loaded bodies exceed
the 10 MiB default). New `summary.json` fields: `cell.max_in_flight`,
`cell.open_order_budget`, `cell.rpc_max_response_mb`,
`ingest.bench_submit_rate`, `ingest.econ_mix`, `ingest.in_flight`.

Sweep: main `92a02ed` only (node `31a95c65`), bench `9b32d897`, section 19
shape, trie off, no perf. Drivers `ozarchy-mif-campaign.sh`,
`ozarchy-mif2-campaign.sh`; tables `ozarchy-mif-analysis.py`,
`ozarchy-mif2-blockA-analysis.txt`. All cells rc 0, AGREE, liveness PASS, no
deaths; the tail had 0 errors and 0 missed blocks in every capped cell.

| setting | matched/s (x base) | open-limit rejects / orders | actions per native block | native blk/s | place / cancel-all | nonce-expired evictions per node | order age at commit p95 |
|---|---|---|---|---|---|---|---|
| base (4 cells) | 97.4k mean (1.00) | 39-42% | 389-395 | 1.19-1.32 | 95 / 5% | ~16.5k | ~63 s |
| budget 900, no cap | 100.3k (1.03) | 30% | 394 | 1.37 | 64 / 36% | ~47k | 68 s |
| N=1 | 144.1k (1.48) | 5.95% | | 2.40 | 95 / 5% | 0 | 2.7 s |
| N=2 | 157.2k (1.61) | 17.1% | | 2.11 | 95 / 5% | 0 | 6.1 s |
| N=4 | 149.8k (1.54) | 30.7% | | 1.98 | 95 / 5% | 0 | 14.7 s |
| N=1 + 900 | 150.4k (1.54) | **0%** | 222 | 3.32 | 66 / 34% | 0 | 2.4 s |
| N=2 + 900 | 173.0k (1.78) | **0%** | 237 | 3.59 | 66 / 34% | 0 | 4.0 s |
| N=4 + 900 (3 cells) | 174.3k (1.79) | **0%** | 261-296 | 3.17-3.52 | 61 / 39% | 0 | ~5 s |
| N=8 + 900 | 176.3k (1.81) | **0%** | 281 | 3.35 | 62 / 38% | 0 | 5.0 s |
| N=16 + 900 | 172.3k (1.77) | **0%** | 246 | 3.72 | 61 / 39% | 0 | 4.9 s |

- **The budget, not N=1, removes the open-limit rejects:** 0% at every N with
  budget 900; without it N=1 still has 5.95% and more in flight is worse.
  The cap is what makes the budget's estimate accurate: the budget alone
  reaches only 30%.
- **Plateau from N=2:** N=2 is 98.1% of the best (N=8), N=4 98.9%, N=16
  97.8%; N=1 is 85.3%. N=2 was the first pick (smallest N within 3% of the
  best); after crab's own plateau the standard shape is N=4 (21.4). Main at
  N=2 repeats at 170.4k / 174.1k in 21.3.
- **Blocks are smaller without the backlog** (220-300 vs ~392 actions); full
  blocks only came from the backlog. No idle gaps mid-run in any capped cell
  (the only 1-3 s gaps are before the first native block).
- **34-39% of actions are cancel-alls with the budget** (5% without). Phase
  2's cancel scan over all books will look bigger partly because of this
  shape.
- **Tail cost not measurable:** cap 1,000,000 (never binds) gave 1.02x base,
  inside base's 3.6% spread; val2 (the tailed node) used ~10% more CPU than
  val0 / val1. The capped and tail-cost cells ran with
  `TORUS_RPC_MAX_RESPONSE_MB=64` and the base cells did not, so tail cost vs
  base measures both together.
- **The baseline moves ~1.8x:** matched/s on this shape is not comparable
  with earlier sections.

### 21.3 Crab (`59fa407`) / main at N=2 + budget 900

Crab = main `59fa407` (item 6 Phase 1 merged; node `1cf9f647`, built in a
fresh worktree, fresh genesis per cell, trie off by default), oracle feed as
in section 19 (30000 / 2000 ms / walk 0, paused for the drain). Main
`92a02ed` as in 21.2. Both arms bench `9b32d897`, `MAX_IN_FLIGHT=2`,
`OPEN_ORDER_BUDGET=900`. Order: crab warm, crab r1, main r1, crab r2, main r2.
All cells rc 0, AGREE, liveness PASS, no deaths, oracle stale 0 / fresh 300.
Table `ozarchy-mif2-blockB-analysis.txt`.

| cell | matched/s | best60 | native blk/s | actions per native block | fills per native block | chain ms | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills |
|---|---|---|---|---|---|---|---|---|---|---|
| crab r1 | 179,861 | 188,964 | 7.418 | 96 | 20.6k | 110 | 4.16 | 0.94 | 0.49 | 29.9 |
| main r1 | 170,414 | 185,527 | 3.648 | 230 | 46.3k | 252 | 4.48 | 0.72 | 1.41 | 31.5 |
| crab r2 | 183,357 | 190,402 | 7.730 | 93 | 19.5k | 102 | 4.14 | 0.93 | 0.49 | 29.6 |
| main r2 | 174,143 | 183,640 | 3.352 | 255 | 51.3k | 277 | 4.44 | 0.70 | 1.38 | 31.5 |

| crab / main | r1 pair | r2 pair | mean | section 19 (uncapped) |
|---|---|---|---|---|
| matched/s | 1.055x | 1.053x | **1.054x** | 1.097x |
| best60 | 1.019x | 1.037x | 1.028x | 0.989x |
| engine ms/1k | 0.93x | 0.93x | 0.93x | 0.89x |
| margin ms/1k | 1.30x | 1.34x | 1.32x | 1.20x |
| match ms/1k | 0.35x | 0.35x | 0.35x | 0.33x |
| CPU-s per 1M fills | 0.95x | 0.94x | 0.94x | 0.92x |

- **Crab leads main by 1.054x on the new shape** (1.097x uncapped). Main
  gains more from the cap (~98k -> ~172k) than crab (~110k -> ~182k): with
  the cap both arms are paced by the load, so the ratio compresses.
- **Per fill crab is cheaper** (engine 0.93x, match 0.35x, CPU 0.94x); margin
  is 1.32x main (0.93 vs 0.71 ms/1k), the same direction as section 19.
- **Per-block figures are not like for like:** crab commits ~2.2x the native
  blocks at ~0.4x the size. Part of the difference is the oracle feed (crab
  only), which adds small native blocks; matched/s and per-fill costs are
  unaffected.

### 21.4 Crab plateau and the standard shape (N=4 + budget 900)

Crab `59fa407` as in 21.3, one cell each at N=4 and N=8 + budget 900 (after
a 60 s warm cell at N=4); N=2 from 21.3. All cells rc 0, AGREE, liveness
PASS, oracle stale 0 / fresh 300, tail 0 errors / 0 missed. Driver
`ozarchy-mif3-campaign.sh`, table `ozarchy-mif3-analysis.txt`.

| N + 900 | crab matched/s | % of crab best | main matched/s | % of main best | crab native blk/s | crab actions per native block | crab cancel-all share |
|---|---|---|---|---|---|---|---|
| 2 | 181.6k (r1 179.9k, r2 183.4k) | 97.4% (r1 96.5%) | 173.0k | 98.1% | 7.4-7.7 | 93-96 | 34% |
| 4 | 184.0k | 98.7% | 174.3k (3 cells) | 98.9% | 5.32 | 139 | 39% |
| 8 | 186.5k | 100% | 176.3k | 100% | 5.28 | 136 | 38% |

- **Standard shape from now on, both arms (including the Phase 2 step 0
  profile): `MAX_IN_FLIGHT=4`, `OPEN_ORDER_BUDGET=900`** (18c, s94). N=2
  passes the rule (smallest N with both arms within 3% of their own best)
  only by 0.4 points, and crab r1 alone fails it; N=4 passes both arms with
  room, and its block shape (~138 actions per native block on crab) is
  closer to a loaded chain, which matters for per-block numbers (cancel
  scan, flush).
- **Per-fill costs are flat across N** on crab (engine 4.14-4.17, margin
  0.93-0.95, match 0.48-0.50 ms/1k; CPU 29.6-29.9 s per 1M fills), so the
  N=2 crab / main ratios in 21.3 carry over per fill.
- N=4 vs N=8 (1.4%) is inside N=2's own r1 / r2 spread (1.9%); one cell
  each.

## 22. Phase 2 step 0 profile at N=4 + budget 900 (2026-10-06)

Crab = main `59fa407` (node `1cf9f647`, oracle feed 30000 / 2000 ms, walk 0,
paused for the drain); main `92a02ed` (`31a95c65`,
`TORUS_NATIVE_TRIE_MAINTENANCE=0`, no feed). Both arms bench `9b32d897`,
`MAX_IN_FLIGHT=4`, `OPEN_ORDER_BUDGET=900` (section 21.4), 300 markets
uniform, cap 400, rate 76,000, `RETRY_BUSY=1`, 120 s, trie off. Order: crab
warm (60 s), crab r1, main r1 (no perf), crab r2, main r2 (perf on val0:
`cycles:u`, 499 Hz, frame pointers, 45 s from 35 s into the load), crab-w10
(walk 10 bp + `ORACLE_FEED_DRAIN=1`, perf in the load window plus a 25 s,
1999 Hz drain window from load end); 14:27-15:01. All 6 cells rc 0, AGREE,
liveness PASS, no deaths, exe md5 as staged, oracle stale 0 / fresh 300,
tail 0 errors / 0 missed; cancel-alls 37.4-38.8% of actions. Driver
`~/bench-results-matched/ozarchy-p2s0-campaign.sh`, tools
`ozarchy-p2s0-tools/`, output `ozarchy-p2s0-analysis.txt`.

- **Crab / main matched/s:** r1 (no perf) 184,786 / 174,530 = **1.059x**;
  r2 (perf) 1.057x. Perf costs ~4.4% on both arms.

### 22.1 Exec costs (load window)

ms per native block | ms per 1k fills; crab r2 has 39.2k fills per native
block, main r2 52.1k, so compare arms per 1k fills. Phases from the node's
timers; the sub-rows from perf (inclusive).

| crab rank | item | crab | main |
|---|---|---|---|
| 1 | settle phase | 71.8 \| 1.83 | 98.6 \| 1.89 |
| 2 | margin phase | 37.6 \| 0.96 | 39.7 \| 0.76 |
| 3 | match phase | 33.5 \| 0.86 | 100.5 \| 1.93 |
| 4 | phase 1 (actions) | 31.8 \| 0.81 | 44.2 \| 0.85 |
| | of which cancel-all | **26.7 \| 0.68** | 37.8 \| 0.73 |
| | of which the book scan | 18.2 \| 0.46 | 26.5 \| 0.51 |
| 5 | save books | 23.9 \| 0.61 | 30.2 \| 0.58 |
| | of which `diff_stop_rows` | **0.06** | 0.14 |
| 6 | cache flush on the exec thread | 15.9 \| 0.41 | 22.5 \| 0.43 |
| | flush worker (own thread) | **74.4 \| 1.90** | 122.9 \| 2.36 |
| 7 | verify | 10.1 \| 0.26 | 13.0 |
| 8 | end_resident wait | 7.1 \| 0.18 | n/a |
| 9 | sums re-value (walk 0) | 6.5 \| 0.17 | n/a |
| | engine / chain | 190 / 246 | 286 / 354 |

- **Cancel-all is the largest Phase 2 target on the exec thread:** 26.7 ms
  per block (~84% of phase 1; the plan's 105-140 ms was the old backlogged
  shape). On this shape every cancel is a cancel-all (37-39% of actions);
  `exec_cancel_all_run` loops over all 300 books and calls
  `take_pending_stops` and `cancel_all_many` per book; hot lines
  `trader_orders.get(sender)` and `partition_point`
  (`cancel_batch.rs:338-359`). An OrderId / trader -> MarketId index removes
  the scan.
- **Thread spawns: at least 14-16k per minute** (crab r2 15,651, main
  14,000, w10 14,047; ~62 per native block on crab, 78 on main; 99.7% live
  under 1 s), from `match_parallel_capped_with`,
  `settle_market_results_parallel` and `drain_books_parallel`. Lower bound:
  perf only counts threads that got a sample (~2 ms of run time); the 1 Hz
  task sampler sees 20-129 / min. The cost is sys time and latency, which
  `cycles:u` does not rank (spawn / sync 0.011 ms per 1k in user cycles; sys
  CPU 2 -> 9 ms per 1k with perf on). The plan's ~1,300 / 60 s is an order of
  magnitude low.
- **Cache flush:** 15.9 ms on the exec thread (`put_position` 8.7, `get`
  4.2, sort 2.8; `position.rs:659-662`) plus 74.4 ms on the flush worker,
  which matches the plan's 65-81 ms. The worker's backlog also causes row 77.
- **Stops dirty flag: drop it from Phase 2.** `diff_stop_rows` is ~0.06 ms
  per block; save books is now the book drain itself (23.4 ms).
- **Margin per fill:** crab 0.961 vs main 0.762 ms per 1k, **1.26x** (1.32x in
  21.3); per block 0.95x.
- **Top self time** (crab r2, every `torus-execution` thread including
  workers): `execute_batch_phases` 8.1%, SipHash `DefaultHasher::write` 6.6%,
  Keccak 5.5%, `hash_one<Address>` 3.5%, `cancel_all_many` 3.4%,
  `AccountReader::get_position` 3.4%, `hash_one<u128>` 3.1%,
  `place_order_with_accounts` 3.0%, `match_market` 2.9%,
  `compute_market_settle_plan` 2.6%. Hashing is ~17% of self time. On main,
  RocksDB `get_position` reads in settle are 32.6% inclusive.

### 22.2 Moving prices (walk 10 vs walk 0, crab load windows)

| ms per native block | walk 0 | walk 10 |
|---|---|---|
| sums re-value | 6.5 | 24.2 |
| liquidation (`pos_sums` / `build_sums`) | 10.7 | 24.7 |
| margin | 45.3 | 48.9 |
| matching | 99 | 95 |
| engine | 190 | 203 (1.07x) |

Chain 1.04x, matched/s 0.975x: re-valuing with moving prices adds ~13 ms of
engine time per block (plan 9.12).

### 22.3 Rows 77, 78 and 7

- **Row 77, the slow first block after the load:** h956 took 59 / 80 / 62 ms
  wall on val0 / val1 / val2, but only 1.75 ms of main-thread CPU (mostly
  `run_liquidations` / `pos_sums`). ~119 ms of exec-side CPU in that interval
  is the flush worker writing the load backlog to RocksDB (crc32c,
  `trade_rows::encode_block`, memtable insert): the block waits on the
  backlog flush, not on its own work. Dropping `NativeStateOverlay` /
  `PendingState` between blocks (BTreeMap drop) cost ~38 ms in total.
- **Row 78, empty blocks with the feed live:** 15 oracle-only blocks in the
  drain window, mean 14.4 ms (p50 4.7, p90 48); of 16.5 ms main-thread CPU,
  14.5 is `run_liquidations_with -> liq_view -> pos_sums -> build_sums`
  (`position_terms`, `__divti3`, `tiers`). 417 blocks with no native
  action: mean 6.0 ms (p50 3.0), `run_liquidations` 5.25 of 6.35 ms,
  `begin_block_oracle` 0.83. The empty-block cost is the liquidation sweep
  rebuilding `pos_sums` every block while marks are fresh.
- **Row 7, R rebuild:** measured at process start, 100,360 rows in 32-37 ms
  (0.33-0.37 s per 1M rows) on every crab validator. At the cell-end size
  (1.606M rows) a restart is ~0.56 s at that rate, ~3.3 s at row 7's 2.07 s
  per 1M; both estimates, no restart measured. Well under tens of seconds, so
  no restart cell (18c).

### 22.4 Verdict

The Phase 2 targets are still on the list but not in the planned order:
1. **Cancel-all book scan** (27 ms per block on the exec thread);
2. **worker pool** (>= 14-16k thread spawns per minute; cost in sys time and
   latency, not ranked by `cycles:u`);
3. **cache flush** (16 ms on the exec thread + the 74 ms flush worker, which
   also stalls the first block after a backlog);
4. then caching sums for the liquidation sweep (rows 77-78, +13 ms per block
   with moving prices) and margin per fill (1.26x main).
5. **Drop the stops dirty flag** (~0.06 ms).

Settle, margin and match are larger than any Phase 2 target but are not
Phase 2 items.

## 23. s94 batch cost and liquidation stress (`35e69b3`, 2026-10-06)

### 23.1 Batch cost: main `35e69b3` vs `92a02ed`

Main `35e69b3` = the s94 batch (EVM fee, read-precompile gas, CoreWriter,
inflation self-stake, off-mark bad debt / +-50% band, governance atomic
writes, auth replay nonce window, liquidation telemetry), node `c2ea1ff8`,
oracle feed as the crab arm (walk 0, paused for the drain); main `92a02ed`
(`31a95c65`, `TORUS_NATIVE_TRIE_MAINTENANCE=0`, no feed). Both arms bench
`361cf3ef` (from `35e69b3`), N=4 + budget 900, 300 markets, 120 s, no perf.
Fresh genesis per cell (`b83180e1`, chain id 7778, regenerated in each cell's
worktree). Order: candidate warm, cand r1, main r1, cand r2, main r2. All
cells rc 0, AGREE, liveness PASS, no deaths, exe md5 3/3 per arm, oracle
stale 0. Driver `~/bench-results-matched/ozarchy-bd-campaign.sh`, table
`ozarchy-bd-analysis.txt`.

| cell | matched/s | best60 | native blk/s | engine ms/1k | margin ms/1k | match ms/1k | CPU-s per 1M fills |
|---|---|---|---|---|---|---|---|
| cand r1 | 181,500 | 190,535 | 5.451 | 4.29 | 0.98 | 0.53 | 30.2 |
| main r1 | 171,746 | 182,667 | 3.585 | 4.61 | 0.73 | 1.43 | 31.8 |
| cand r2 | 180,501 | 190,865 | 5.369 | 4.28 | 1.01 | 0.52 | 30.2 |
| main r2 | 159,788 | 170,127 | 3.358 | 4.96 | 0.78 | 1.48 | 33.2 |

- **No measurable cost:** r1 1.057x (section 22, `59fa407`: 1.059x). r2 1.130x
  is a low main outlier (-7%). Per fill the candidate is within 1.5-3% of
  `59fa407` (engine +1.7%, margin +1.7%, match +3%). Both arms are ~1.7%
  below their section 22 values (bench / host drift, not the node).
- **Off-mark band refusals: 0** (`torus_orders_rejected_other_total` and
  `torus_exec_action_failures_total` 0 on every node).

### 23.2 Liquidation stress (`bench/liq-stress` @ `af8529e`)

Node `c2ea1ff8` (= `35e69b3`; the branch changes only `tools/`), bench
`ec4e14a9`. Standard shape (N=4 + budget 900, 300 markets), walk 10 bp,
`ORACLE_FEED_DRAIN=1`; stress cells add `LIQ_THIN=200` (bulk senders 60-259
on 1M TRS) and a parity-signed shock at round 45 (`ORACLE_SHOCK_BP` S: odd
markets +S bp, even -S bp, so the 100 even-index thin senders lose on every
position). The vault and 50 thin senders are in the digest. Driver
`ozarchy-liq-campaign.sh`; per cell `liq-stress.json`, `liq-lines-val*.txt`,
`thin-snap.jsonl`.

| | warm | S=400 | S=750 |
|---|---|---|---|
| verdict | rc 0, ACCEPT | rc 0, ACCEPT | **rc 2, REJECT (liveness FAIL)** |
| AGREE (incl. vault + thin) | AGREE | AGREE | AGREE |
| matched/s (best60) | 171,299 (180,563) | 161,565 (184,444) | 127,720 (179,405) |
| liquidations (val0 / 1 / 2) | 0 | 100 / 100 / 100 | 100 / 100 / 100 |
| stage 1 / backstop / ADL | - | **0 / 100 / 0** | 0 / 0 / **100** |
| acted per block (3 blocks) | - | 47 / 20 / 33 | 47 / 20 / 33 |
| liquidation step on those blocks | ~18 ms baseline | 98 / 46 / 115 ms | **331.8 / 123.4 / 241.1 s** |
| pending / deferred | - | 0 throughout | 0 throughout |
| feed-live drain | 23 s | 33 s | 749 s of 780 |
| vault (identical on all nodes) | - | +19,984,975.69, 300 positions | **deficit 26,516,805.13**, 0 positions |

- **S=750 stalls the chain:** all 100 accounts go to ADL, each with positions
  in ~257-279 markets: 26,778 ADL (account, market) steps per node and
  27,410 counterparty closes (26,309 single, 327 with 2, 121 with 3, 21 with
  4), ~26 ms per account-market, all inside 3 blocks. Consensus height stayed
  at 787-789 from 18:12:37 to 18:24:14 (~11.6 min). State stayed identical
  (AGREE). ADL has no per-block work budget: one large move can halt block
  production for minutes. **P0 before testnet.**
- **The vault deficit at S=750 is the expected finding** (nothing refills
  it): identical in `torus_liquidator_vault_deficit`, `torus_getLiquidatorVault`
  on all 3 nodes and the digest.
- **S=400 went straight to backstop** (0 stage 1), although a 4% loss against
  a 5% initial margin was expected to land in stage 1. Unexplained: the thin
  accounts may be deeper than modelled, or classification differs from the
  design.
- **Who:** exactly the 100 even-index thin senders (balances 0 after S=400;
  the ADL lines name the same 100 at S=750); no ADL'd account outside the
  thin set (counterparty identities are only in the debug-level close lines).
- **Row 76:** pending never rose above 0 and all 100 accounts were acted on
  within 3 blocks (budget 64 per block), so "liquidate touched accounts
  first" is not needed at this size. Exec lag was already 65-66 from the load,
  so the shock adds nothing measurable to it at S=400.
- **Harness issues found:** an untracked, gitignored
  `testnet/genesis-weighted-full.json` in the worktree (61 balances, written
  by a harness test) made the first attempt's node exit with `invalid
  address: oracle-feed` and the `LIQ_THIN` patch match 0 rows (attempt
  archived in `ozarchy-liq-attempt1/`); a target dir reflink-seeded from
  another build kept a stale bench binary; `liq_stress.py` raises KeyError on
  `torus_exec_post_engine_tail_seconds_count` (`sampler.csv` has only
  `_sum`). Results above use a patched copy (`ozarchy-liq-tools/`).

## Open

- Native trie maintenance is off by default since `db6c9de` (owner
  decision); only `TORUS_NATIVE_TRIE_MAINTENANCE=1` enables it (section
  12).
- Shared-code inflation (~1.7 ms/1k) is extra instructions, not cache
  misses (section 9.2): next, calls per fill and `perf annotate` of
  `drain_book`, `cancel_all_many`, `insert_order`, `match_market`.
- A trie-on crab / main pair in one session, to tighten the 0.553 -> 0.508
  trie on/off comparison (section 9.1).
- E1 (oracle step) postponed, low priority: 5.8 ms per empty block on
  ozarchy (section 7.3).
- Margin at 300 markets after M1: 125 vs ~96 ms per block on main (was
  216-221 after C6 + C7; section 14.1). Cut list in section 11.
- end_resident after M1: +29.7 ms per block, mostly
  `BlockSums::into_cache` re-decoding positions (section 14.2). Timed since
  `b2bcfaa`; `1242d80` reuses the decoded positions (18c ubench, 300
  markets: 21.1 -> 14.2 ms per block). 10 markets: ~5.8 ms per block
  (section 16). On the step 2 worker at 300 markets: 65-68 ms per block
  (section 18.2).
- Step 2 (end_resident off the execution thread, `2333ba4`): hides ~92%
  at 300 markets (exec wait ~5 ms per block, p90 16-22); no visible cost
  to verify (section 18).
- Gate 2 met at `31cea69` (B-blind, section 19): 1.097x main at 300
  markets, 0.997x at 10 markets. The 300-market fills-per-block gap
  (section 18) was the same-batch sell bound. Option A not needed (cuts
  0.063% of placed). Margin per fill is still 1.20x main at 300 markets
  (1.73 vs 1.45 ms/1k) and +0.067 ms/1k at 10 markets. B-blind's own share
  is not split from cuts 1, 2, 5 and 6 (no `ab12c75`-only run here).
- Bench host: three 300-market runs (00:16-00:57, 2026-10-06) lost a
  process (val1, val2, load generator) to a SIGKILL that is not an OOM
  kill and did not come through `kill`/`tkill`/`tgkill`; cause unknown
  (section 18). Run heavy cells under the `signal_generate` trace. The
  section 19 campaign (10 cells under the trace, each its own systemd unit
  through `detach.sh`) lost no process.
- 10-market gap is all on the exec thread (section 17): +51.5 ms per block
  in the load window; views are exec-paced. Cheapest cut: C's
  `native_failures` re-encodes actions with `canonical_bytes` (~20 ms per
  block at 10 markets); map failures by original index instead. Also
  untimed: no timer around `native_failures`.
- Shed path: `parse_bytes` decodes the whole hex payload before refusing
  (~1.9 ms/1k on crab, ~1.2 on main; section 17.2).
- A main pair interleaved with crab cells, trie off, to tighten the
  0.638x / 0.648x / 0.760x ratios (sections 10.1, 12.1, 14.1).
- Cheaper hasher for the Address-keyed maps: the +0.46 ms/1k SipHash of
  `239ff69` (section 12.2) is gone after M1 (section 14.3);
  `hash_one<&Address>` is still 0.94 ms/1k.
- C: zero-fill IOC and crossing post-only orders still show "executed"
  (section 13.2); reason codes are typed since `4a26653`.
- Optional: cheaper shed path (peek the action tag or const-hex) and the
  1.22-1.27x signature-verify cost per admitted action on crab (section
  6.4).
- Gate 4 after C4 + PF1: ubench tail at 300 markets, `ubench_epoch
  UB_DRAIN=fresh` empty block (target <= 20 ms).
- Live-feed idle check (plan Step 6) met at `5584880` with walk 0: oracle-only
  blocks <= 5.92 ms with the feed live through the drain (section 20). Not
  covered: a moving walk (`ORACLE_WALK_BP` > 0), and per-block exec timing
  in the node (the harness's per-interval mean only sees empty blocks).
- Bench open-limit reject share is still 52-54% of actions (section 20.3):
  per-sender in-flight cap in the generator (18c) before the Phase 2 step 0
  profile.
- Anti-spam D (per-IP RPC limit) has no validator exemption; no metric for
  oracle submissions evicted inside the pool.
- Liquidation-stress cell before testnet: no full-node cell has fired a
  liquidation (walk 10 bp stays within +-80 bp, section 21.1). No walk can
  on the standard shape (each sender 150 long / 150 short over 300 markets on
  100M TRS; the walk is bounded at +-8 steps). Proposed design (thin senders
  + a parity-signed price shock + the vault in the digest) awaits 18c:
  `docs/buildlist-fixes-s17.md`.
- Slow first oracle-only block after the load (20-49 ms) and ~2.5 ms empty
  blocks while the feed is live (section 21.1): look at both in the Phase 2
  step 0 profile.
- Standard shape (Phase 2 step 0 and later): `MAX_IN_FLIGHT=4`,
  `OPEN_ORDER_BUDGET=900` on both arms (section 21.4); 38-39% of actions are
  cancel-alls on this shape.
- Feed-drain mode reports exec ms per oracle-only block as a per-interval
  mean that only sees empty blocks; exact numbers need per-block exec timing
  in the node (section 20.2).
- Phase 2 order from the step 0 profile (section 22): cancel-all book scan,
  worker pool (>= 14-16k thread spawns per minute), cache flush (exec part and
  flush worker), sums caching in the liquidation sweep; stops dirty flag
  dropped. Thread spawn cost needs a sys-time / latency measurement
  (`cycles:u` cannot rank it).
- Perf backlog: `delegations_for_validator` (`staking.rs`) scans the whole
  `CF_STAKING_DELEGATIONS` table on every call; with the self-stake reward
  split (`fix/inflation-self-stake`) that is per block once the validator fee
  share is above 0 bps, and once per active validator at epoch boundaries.
- Read precompile gas (`fix/read-precompile-gas`, 50 gas per unit is a
  placeholder): before testnet, microbench ns per unit (row read / 32 B blob /
  32 B returned) and size it so a 30M-gas block of reads fits the block exec
  budget.
- **P0 before testnet: ADL has no per-block work budget** (section 23.2): at
  S=750, 100 accounts x ~270 markets of ADL took 332 / 123 / 241 s on three
  blocks and froze consensus ~11.6 min. Needs a budget (account-markets or
  closes per block) with carry-over, and cheaper per-close work.
- Liquidation stress at S=400 classified every account as backstop, none as
  stage 1 (section 23.2): check the thin accounts' margin at the shock and
  the classification.
- Harness: fix `liq_stress.py` (`_count` column), keep harness tests from
  writing `testnet/genesis-weighted-full.json` into the worktree, and avoid
  stale binaries from reflink-seeded target dirs (section 23.2).
