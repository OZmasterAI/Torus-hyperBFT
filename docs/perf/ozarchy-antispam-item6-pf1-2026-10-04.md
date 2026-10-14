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
- **ADL root cause (s18, profiled):** every ADL step runs `adl_candidates`,
  which walks the first `ADL_MAX_SCAN_ROWS` = 65,536 rows of
  `CF_NATIVE_POSITIONS` from the empty key whatever the market. Microbench
  `ubench_adl` (real `run_liquidations` with R): 12-16 ms per step once the
  CF holds > 65,536 rows, 38 ms without R; perf: `adl_candidates` = 97.7% of
  the step. Also a fairness defect: counterparties come only from the ~200
  lowest-address traders. `LIQ_ACT_PER_BLOCK` = 64 counts accounts, not
  steps. Design + options: `docs/plans/adl-budget.md` (`perf/adl-budget`).
- **The vault deficit at S=750 is the expected finding** (nothing refills
  it): identical in `torus_liquidator_vault_deficit`, `torus_getLiquidatorVault`
  on all 3 nodes and the digest.
- **S=400 went straight to backstop** (0 stage 1), although a 4% loss against
  a 5% initial margin was expected to land in stage 1. **Explained (s18):**
  stage 1 is only `2/3 MM <= AV < MM` (`liquidation.rs` `classify`). A thin
  account holds ~20M notional (19.89-20.07M over 257-279 markets, from the
  S=750 ADL lines) on 1M collateral, MM = notional / 40 ~ 500k, so AV/MM ~ 2.0
  before the shock and ~0.40 after a 400 bp loss (1M - 0.04 x 20M = 200k),
  below 2/3. Measured: vault +19,984,975.69 over 100 backstops = mean AV
  199,850 at liquidation; the per-account spread (~0.38-0.42) is derived (the
  logs print counts only). The walk (cap +-80 bp) alone never reaches the band;
  a shock of 251-333 bp would land in it. At S=750 AV ~ -500k, hence all ADL.
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

## 24. ADL budget proof cells (`perf/adl-budget` @ `6a25e20`, 2026-10-07)

Node `18bf0759` (= `6a25e20`: A1-A8, review fixes, bit-identical drain
caches), bench `ec4e14a9` (`bench/liq-stress` @ `b49e40b`). Same shape and
shock as section 23.2 (N=4 + budget 900, 300 markets, walk 10 bp,
`ORACLE_FEED_DRAIN=1`, `LIQ_THIN=200`, parity-signed shock at round 45).
Cells: warm, s400, s750 with `TORUS_LIQ_VALUE_SUM` off (timed), s400vs and
s750vs with it on (conservation). Driver
`~/bench-results-matched/ozarchy-adlcells-campaign.sh` (unit
`bench-adlcells`), notes `ozarchy-adlcells-FINDINGS-partial.md`.

| | warm | S=400 | S=750 |
|---|---|---|---|
| verdict | rc 0, ACCEPT | rc 0, ACCEPT, liveness PASS | **rc 0, ACCEPT, liveness PASS** |
| AGREE (incl. vault + thin) | - | AGREE | AGREE |
| matched/s | - | 161,875 | 150,855 |
| backstop / ADL | - | 100 / 0 | 0 / **100** ("ADL to escrow" 26,790, escrow dust 2) |
| `adl_queue` | - | never used | max 15,462 at h799, empty after 27 blocks (h796-h823); 73 blocks shock to zero |
| liquidation step per block | - | - | window max 1,232 / 1,111 / 1,158 ms (val0-2), window avg ~50-54, baseline ~17 |
| feed-live drain | - | - | **22 s** (section 23.2: 749 s of 780) |
| vault (identical on all nodes) | - | +19,998,661.35, deficit 0, 300 positions | **-0.00084125**, 0 positions (section 23.2: deficit 26,516,805.13) |

- **The S=750 stall is fixed:** the ADL work is spread over 27 blocks with
  the queue carrying the rest, the chain keeps committing, and the vault ends
  at ~0 instead of -26.5M. S=400 vault differs from section 23.2
  (+19,984,975.69): runs are not block-identical (wall-clock load + feed).
- **Still above the HL target:** the heaviest liquidation block is ~1.1-1.2 s
  against ~250 ms. That is what C2 (node-local per-market holder list) and the
  W re-size are for (18c s96 decision).
- **Log noise only:** WARN "skipping duplicate/replayed native action on live
  commit path" (s400 4, s750 1); no liquidation errors.

### 24.1 Conservation with the value sum on

`value_sum` is `FixedPoint` (i128, scale 1e8) and its log line prints it
exactly; only the Prometheus gauge `liquidation_value_sum` is f64
(`liquidation_step.rs:144-181` at `6a25e20`). Compare the log values as
decimals, never as floats.

- **Correction (s24):** the first version of this section said s400vs changed
  only 3 times by 1 f64 ULP. That came from comparing the values as floats.
  Exact recount (`partial-at-stop/vs-val*.txt`): unchanged h100-h350, then
  557-568 changes from h351, net -0.0208 by ~h2780 (69,980,199,999,999.97924),
  the same on all 3 validators (the files differ only in where they stop).
- **s400vs drain:** exec lag sat at 65-66 for the whole 772 s feed-live drain
  (`drain-samples.jsonl`; exec ~0.34 s per block with the value sum on), so
  the criterion (exec lag <= 2) was never met. Not a liveness failure (h574
  at bench end to ~h2780). Stopped by the owner at 02:34; no rc / verdict
  file. Not rerun: S=400 has no ADL or escrows.
- **Drain fix:** `bench/liq-stress` @ `95f50cdb` adds
  `ORACLE_FEED_DRAIN_MAX_LAG` (default 2, needs `ORACLE_FEED_DRAIN=1`), passed
  as `--max-lag` to the feed-live drain only. Timed cells are unchanged. Value-sum
  cells use 1000. Harness tests 171 passed.

### 24.2 s750vs (value sum on, S=750)

Driver `ozarchy-adlcells-s750vs-campaign.sh` (s750vs only), same node and
bench as section 24. Dir `ozarchy-adlcells-300m-s750vs`.

- **Verdict:** rc 0, AGREE, ACCEPT, liveness PASS; feed-live drain 57 s
  (max exec lag 66). The drain after the feed pause did not go quiet within
  its 60 s (exec queue 0 at 58 s); the digest was still quiescent.
- **Liquidations:** 100 ADL, 0 backstop, 0 stage 1 on all 3 validators;
  "ADL to escrow" 26,666 (h613 8,777, h614 12,563, h615 5,326). `adl_queue`
  max 14,731 at h614, 0 at h619 (7 drain blocks, 74 blocks from the shock at
  h545). Heaviest liquidation block 1,400 / 1,293 / 1,264 ms.
- **Vault:** -0.00074019 on all 3 validators, 0 open positions.
- **Value sum** (`value-sum-val{0,1,2}.txt`, identical on all 3, 201
  changes): h100 69,980,200,000,000.00000000 unchanged to h404, then:

| heights | change | fills | per fill (1e-8 units) |
|---|---|---|---|
| h404-437 | -0.00502494 | 2.98M | -0.17 |
| h482-524 | -0.00803519 | 2.90M | -0.28 |
| h547-611 | -0.00451432 | 1.87M | -0.24 |
| h611-616 (ADL) | -0.00135589 | 264k | -0.51 |
| h437-482, h524-547 | +0.00011 each | ~20k each | ~0 |
| h700-810 | -0.00000374 | 0 | - |
| h810-1402 | 0 | 0 | - |
| h1402-1500 | **-1,740.68816344** | 0 | - |
| h1500-1702 | 0 | 0 | - |

- **Dust verdict (18c's rule: conserved = dust, drifts with the ADL count =
  trace):** the drift follows trade fills (~-0.2 units of 1e-8 per fill, net
  -0.0209 by h1402), not ADL. ADL adds ~2-3 units per op above the fill
  rate, the same order as the vault's -0.00074 (s750: -0.00084). So the vault
  figure is dust. The per-fill drift is a separate rounding bias in trade
  settlement (not traced).
- **The -1,740.69 step** (09:58:20-26Z) follows the feed pause at 09:57:12Z.
  Marks expire after 60 s; the sum drops markets without a usable mark
  (`liquidation_step.rs:321-324`) and unmarked positions count at entry, and
  the feed's last mark check shows markPrice 0. Most likely a measuring
  artifact (inferred, not proved per market). Compare the sum only while marks
  are fresh. Fix (c) of the C2 round (one common price per market) targets it.
- **Escrow dust lines** (2 per validator, identical): s750 step h803 long
  +0.00132290, short -0.00216415 (sum -0.00084125 = h803 `adl_dust` = vault
  deficit); s750vs step h620 long +0.00147665, short -0.00221684 (sum
  -0.00074019 = vault deficit). At the end escrow notional, `adl_queue` and
  `adl_queue_deficit` are 0 on all validators. The sweep (~:899) moves an
  escrow's whole collateral to the vault once it holds no positions; the
  escrows are not digest accounts, so their 0 balance has no digest-level
  proof. 18c (s99): explain and fix as part of C2.
- Harness nit: `funnel-val*.csv` has a 14-name header and 15 values per row
  (columns after `rej_margin` shifted by one).

## 25. Phase 2 step 0 (Gate 0) (`perf/item6-phase2` @ `707f132f`, 2026-10-08)

Step 0 node = `707f132f` (= `1a6573dc` + docs: counters, harness columns,
cancel-by-id cell), node `0a742915`; base = main `d3ba3c0a` (plan 9.7), node
`193ae781`. Both arms: bench `fff899ca` (from `707f132f`, byte-identical; the
cancel-by-id flags off unless set), the step 0.2 `run-cell.sh` /
`summarize.py` (detached copy `wt/p2s0b-707f132f`), oracle feed 30000 / 2000 ms,
walk 0, trie off by default. Builds: `--release`, line-tables-only, mold,
frame pointers; the base in an unseeded target dir (a reflink-seeded first
attempt kept step 0 crates as fresh and gave a mixed binary; discarded).
Standard shape (section 21.4): N=4 + budget 900, 300 markets uniform, cap 400,
rate 76,000, `RETRY_BUSY=1`, 120 s. Order: s-warm (60 s), overhead check
ABBA s-r1, b-r1, b-r2, s-r2 (no perf), s-prof (perf on val0, `cycles:u` 499 Hz,
45 s from 35 s into the load), s-byid (`CANCEL_BY_ID_FRACTION=0.1
MODIFY_FRACTION=0.05`), s-w10 (`ORACLE_WALK_BP=10` + `ORACLE_FEED_DRAIN=1`, no
perf); 03:10-04:03. All 8 cells rc 0, AGREE, liveness PASS, accepted, no
deaths, exe md5 3/3 as staged, oracle stale 0 / fresh 300, block tail 0 errors
/ 0 missed, no tail WARNING; cancel-alls 37.4-39.7% of actions. Each cell ran
as its own unit through `detach.sh`; the root-only `signal_generate` trace was
not running (no sudo on the agent side; the death watcher saw no death). Open
files: max 657 per validator (soft limit in the `detach.sh` units 65,536, hard
524,288), so `LimitNOFILE` was not changed. Driver
`~/bench-results-matched/ozarchy-p2s0b-campaign.sh`, tables
`ozarchy-p2s0b-cells.txt`, `ozarchy-p2s0b-pernode.txt`, tools
`ozarchy-p2s0b-tools/`.

| cell | matched/s | native blk/s | fills/blk | engine ms/blk \| ms/1k | phase 1 | margin | match | settle | cache flush | flush worker | chain |
|---|---|---|---|---|---|---|---|---|---|---|---|
| s-warm (60 s) | 166,464 | 4.887 | 26,424 | 115.2 \| 4.36 | 21.3 | 26.6 | 13.6 | 40.8 | 11.3 | 52.5 | 146.0 |
| s-r1 | 165,622 | 4.869 | 26,878 | 122.6 \| 4.56 | 21.7 | 28.0 | 15.2 | 43.8 | 12.1 | 51.1 | 155.7 |
| b-r1 | 169,295 | 5.131 | 26,861 | 121.1 \| 4.51 | 21.5 | 28.4 | 14.8 | 43.7 | 11.6 | 50.1 | 152.2 |
| b-r2 | 159,922 | 4.902 | 27,358 | 129.6 \| 4.74 | 22.8 | 28.7 | 16.2 | 46.6 | 12.5 | 54.1 | 164.2 |
| s-r2 | 163,861 | 4.762 | 27,230 | 125.7 \| 4.61 | 21.8 | 29.3 | 15.5 | 46.4 | 12.6 | 51.2 | 159.9 |
| s-prof (perf) | 155,194 | 3.637 | 36,586 | 170.8 \| 4.67 | 29.9 | 38.8 | 23.4 | 62.4 | 16.1 | 71.5 | 217.6 |
| s-byid | 160,460 | 4.762 | 26,505 | 125.6 \| 4.74 | 25.1 | 27.9 | 14.7 | 43.7 | 11.9 | 51.8 | 158.6 |
| s-w10 (walk 10, drain) | 164,322 | 4.813 | 34,141 | 160.6 \| 4.70 | 26.6 | 34.1 | 18.6 | 55.8 | 14.7 | 63.0 | 199.6 |

ms per native block, val0, from the node's timers over bench + drain (harness
`phase_by_node`). New columns (step 0.2):

| cell | cancel-alls / blk | books visited / hit per cancel-all | spawns / blk (match, settle, save books, end_resident) | spawns / min | user \| sys CPU ms / blk | user \| sys CPU ms / 1k | oracle-only blocks: n, avg, p50, p95 ms |
|---|---|---|---|---|---|---|---|
| s-warm | 52.2 | 298.8 / 93.1 | 42.7 (13.8, 13.0, 15.0, 1.0) | 14,793 | 1,031 \| 84.9 | 30.3 \| 2.49 | 2, 3.6, 3.4, 4.9 |
| s-r1 | 58.7 | 299.5 / 86.8 | 52.4 (17.1, 15.9, 18.4, 1.0) | 17,977 | 1,014 \| 76.7 | 29.8 \| 2.26 | 8, 39.9, 7.6, 112 |
| b-r1 | - | - | - | - | 968 \| 75.5 | 29.3 \| 2.29 | - |
| b-r2 | - | - | - | - | 983 \| 74.2 | 30.1 \| 2.28 | - |
| s-r2 | 56.8 | 299.3 / 88.6 | 47.5 (15.2, 14.5, 16.9, 1.0) | 15,816 | 1,020 \| 77.2 | 29.6 \| 2.24 | 5, 40.4, 4.6, 124 |
| s-prof | 72.2 | 299.3 / 92.3 | 54.9 (17.4, 16.9, 19.6, 1.0) | 13,423 | 1,286 \| 398 | 30.1 \| 9.33 | 12, 18.7, 6.6, 77.9 |
| s-byid | 57.0 | 299.3 / 88.8 | 48.2 (15.6, 14.5, 17.1, 1.0) | 16,260 | 1,012 \| 78.0 | 30.0 \| 2.32 | 4, 48.7, 7.6, 121 |
| s-w10 | 72.1 | 299.7 / 88.8 | 58.2 (18.8, 17.5, 21.0, 1.0) | 16,820 | 1,020 \| 72.9 | 29.9 \| 2.14 | 41, 19.3, 4.0, 100 |

Process CPU is the whole node (all ~190 threads), load window. The base has no
counters (-). Spawn sites `margin_prepare`, `open_orders`, `flush_digest`,
`root_buckets` and `load_books` read 0 per native block on this shape.

- **Overhead check: the counters cost nothing measurable.** Step 0 / base
  (ABBA, 2 cells per arm): matched/s 164,741 / 164,608 = **1.001x**; engine
  ms/1k 0.991x, phase 1 0.982x, chain 0.998x, user CPU / 1k 0.999x, sys CPU /
  1k 0.986x. The two base cells alone differ by 5.6% (169,295 / 159,922), so
  the resolution is the usual ~5%.
- **Gate 0 met:** counters in the harness, standard-shape cells on the step 0
  node with the new columns (s-r1, s-r2), estimates in the plan.
- **Cancel-all:** 57-59 per native block (72-79 in the perf and walk-10 cells,
  whose blocks are larger). Every call visits all ~300 books; the sender has
  orders or stops in **87-93 of them (29-31%)**.
- **Thread spawns: 47.5-52.4 per native block, 15.8-18.0k per minute** (exact
  node counters; section 22's perf lower bound was 15.6k per minute; in s-prof
  perf saw 11.5k distinct exec tids per minute vs the counter's 13.4k). Three
  sites spawn ~15-19 threads each per block (match, settle, save books) plus
  one end-resident thread. A C microbench on this host (pthread create + join,
  2 MiB stacks, idle host, `ozarchy-p2s0b-spawn/`): 16.7 us per thread in a
  scope of 16 (0.27 ms per scope), 30 us for a single thread. So ~0.8 ms per
  native block of exec-thread wall at idle-host cost (estimate; more under
  load). Its CPU (~50 x 17 us) is ~1% of the process sys time (77 ms per
  native block), so sys CPU per 1k fills cannot show the pool's effect.
- **Sys CPU:** 2.14-2.32 ms per 1k fills on both arms without perf; 9.33 with
  perf on (section 22: 2 -> 9).
- **Oracle-only blocks (node timer):** few in the standard cells (2-8, the feed
  is paused for the drain), mean 40 ms, p95 112-124 ms: the first blocks after
  the load, which wait on the flush backlog (row 77). With walk 10 and the feed
  live through the drain: 41 blocks, mean 19.3, p50 4.0, p95 100 ms (section
  22.3 from perf / logs: 15 blocks, mean 14.4, p50 4.7, p90 48). p50 / p95 are
  bucket interpolations.
- **Walk 10 vs walk 0** (s-w10 vs mean s-r1 / s-r2): matched/s 0.997x, engine
  ms/1k 1.025x (section 22.2: 0.975x, 1.07x).

### 25.1 0.3: what the cancel-all time is spent on (s-prof, load window)

perf on val0 in the load window: 175 native blocks, 37,633 fills and 79.4
cancel-alls per native block, 300.0 books visited / 94.7 hit per cancel-all.
Inline-expanded (`llvm-addr2line -i`) exec-thread samples under
`exec_cancel_all_run` / `exec_cancel_all` / `cancel_orders_and_stops`, split
by line (`ozarchy-p2s0b-tools/split.py`, lines at `707f132f`). "Scan" = paid
per (book x run member) or per (action x market) whether or not the sender has
anything there; "work" = per cancelled order, stop or hit.

| part | ms per native block | kind |
|---|---|---|
| cancel-all inclusive | **30.0** (0.80 ms/1k) | |
| `apply_cancel_all_many`: removal from the levels, journals, index | 10.3 | work |
| plan: locate targets (`order_index` / `order_seq` get + `partition_point`, cancel_batch.rs:352-361) | 9.7 | work |
| plan: `trader_orders.get(sender)` probe (:338) | 3.5 | scan |
| run: store / drop per book (`per_action`, the `cancel_all_many` result; native_executor.rs:9131-9137, 9159-9160) | 2.0 | scan |
| plan: sort + levels; per-target push | 1.2 | work |
| margin of cancelled orders, hit bookkeeping, release | 1.5 | work |
| run: results loop over all markets per action (:9141-9149) | 0.9 | scan |
| plan: first-occurrence sort + allocs, probe loop (:323-346) | 0.5 | scan |
| run: `get_mut` / `margin_configs.get`, allocs, members filter per book (:9108-9122) | 0.3 | scan |
| `take_pending_stops` (`any` over the book's stops + take) | 0.05 | scan / work |
| sequential fallback (`owners < 2`) | 0.01 | |
| **scan / work** | **7.3 / 22.7** | |

- **Section 22's "book scan 18.2 ms" was mostly work.** It was the self time
  of `cancel_all_many` with the plan and apply code inlined into it; per line,
  three quarters of cancel-all is removing orders that are really cancelled
  (locate, `partition_point`, removal), which scales with orders cancelled.
  The members filter (0.05) and `take_pending_stops` (0.05) cost almost
  nothing; the probes cost 3.5.
- **P2-1 estimate: ~5.0 ms per native block in this window** (7.3 ms of scan
  x (1 - 94.7 / 300) visited-but-empty books), **~3.7 ms on the standard
  no-perf cells** (58 instead of 79 cancel-alls per block). The index's own
  upkeep (one insert per resting order or per (trader, market) per block) is
  not in that number. The plan's 5-18 ms range assumed most of the 18.2 ms was
  scan.
- **Cache flush (for P2-2, same window, `flush_all` inclusive 17.6 ms per
  native block):** `put_position` 9.7 (overlay `BTreeMap` insert of the owned
  key, backend.rs:2056: 7.2; `intern_cf` 0.8; serialise + buffer 1.2), the
  second lookup `map.get` (position.rs:684) 3.5, the key sort (:682) 3.05,
  balance cache 0.5 (`ozarchy-p2s0b-tools/flushsplit.py`). The batch API
  removes the second lookup, `intern_cf` and the per-row lock: ~4.6 ms here,
  ~3.2 ms on the standard cells (cache flush 12.1-12.6 ms there) (estimate).
  The per-row `BTreeMap` insert (7.2) stays unless the CF map is built in bulk
  from the sorted rows (not in the plan).

### 25.2 Cancel / modify by id (P2-1b, s-byid)

- **Delivered share is small:** the bench sent 658 cancels and 300 modifies,
  all accepted (1.22 by-id actions per native block), not the 10% / 5% of
  fires asked: 16,445 of 17,361 `torus_getOpenOrders` lookups (one market per
  lookup) found no own order in that market. Lookup errors 0.
- **Books probed per by-id action: 457** (val0; 456-459 on all validators),
  i.e. 1.5 passes over the 300 books (cancel probes twice, modify once, both
  stop at the order).
- **Phase 1: 25.1-25.8 ms per native block on the three validators vs
  21.7-23.4 in s-r1 / s-r2: +3.1 ms per block, ~2.5 ms per by-id action.** The
  probes themselves are ~457 SipHash lookups (~10-20 us). The rest is not
  split (no perf in this cell); likely cause (inferred, not measured): a single
  cancel or modify between cancel-alls splits a cancel-all run, and each run
  pays its per-book work over all 300 books again (25.1). P2-1's index cuts
  that too; P2-1b's map cuts only the probes. A perf cell with a larger by-id
  share is needed before P2-1b's gate is read.

### 25.3 Hasher microbench (P2-5)

`ubench_hasher` from `707f132f` (release, same flags), 10 processes x best of
30 reps of 2M ops, pinned to CPU 8; median [min-max] over the processes,
`ozarchy-p2s0b-hasher/summary.txt`:

| case | SipHash ns | foldhash ns | ratio |
|---|---|---|---|
| `hash_one` Address | 16.57 [16.56-16.58] | 10.06 [10.05-10.06] | 0.607 [0.607-0.607] |
| `hash_one` order id | 14.32 [14.31-14.35] | 1.87 [1.87-1.87] | 0.131 [0.130-0.131] |
| Address map get | 23.88 [23.73-24.45] | 12.86 [12.76-13.34] | 0.540 [0.536-0.551] |
| (Address, market) map get | 49.46 [47.16-50.76] | 28.11 [27.88-28.27] | 0.566 [0.551-0.597] |
| order-id map get | 21.84 [21.23-22.34] | 6.28 [5.98-6.46] | 0.287 [0.282-0.291] |
| order-id churn | 33.70 [33.33-34.13] | 13.74 [9.86-16.04] | 0.408 [0.296-0.470] |
| Address set insert | 22.46 [22.31-22.83] | 21.70 [21.50-21.84] | 0.964 [0.946-0.974] |

- `hash_one` mean ratio 0.369 (spread < 0.1%); the test's estimate is **15.8
  ms per native block** at section 22's 190 ms engine and 13.2% hash share;
  at the standard cells' engine (124 ms per native block) the same share gives
  **~10.3 ms** (estimate). Map-level ratios (0.29-0.57) are the better guide
  per map; order-id churn is the noisiest case (foldhash 9.9-16.0 ns).

### 25.4 Verdict

- Counters: no measurable cost (1.001x); later Phase 2 arms can carry them.
- **Step 0 checkpoint: P2-1 + P2-2 come out below 13 ms per native block:**
  ~5.0 + ~4.6 = ~9.6 ms in the perf window (37.6k fills per block, close to
  section 22's 39.2k), ~3.7 + ~3.2 = ~6.9 ms on the standard no-perf cells
  (27k fills, engine 124 ms). As a share of engine time that is 4.7-5.6%,
  against the ~6.8% that 13 ms of 190 stood for. Owner decision (plan 9.1):
  pull P2-5 (~10 ms on the standard cells, estimate) into the gate set, or
  accept a lower gate.
- P2-3: ~50 spawns per native block, ~0.8 ms of exec-thread wall at idle-host
  spawn cost (estimate); its gate should read the spawn counter and matched/s,
  not sys CPU (the spawns are ~1% of it).
- **Unexplained: both arms are ~9% below section 23.1's `35e69b3` cells on the
  same shape** (164.7k vs 181.5k matched/s; engine 4.51-4.74 vs 4.29 ms/1k;
  native blocks 4.76-5.13 vs 5.45 per s). Not interleaved: bench binary,
  base and day differ. The step 0 / base comparison is not affected.
  **Checked in section 26:** interleaved, `d3ba3c0a` is 6.3% below `35e69b3`
  (not the 4 MiB SSTs, not the build style); the rest was the day or window.

## 26. Regression check: main `d3ba3c0a` vs `35e69b3` (2026-10-08)

The ~9% gap of section 25.4 (not interleaved), checked with interleaved arms
(owner s101). Arms, all with bench `fff899ca` (from `707f132f`), the step 0.2
harness (`wt/p2s0b-707f132f`), oracle feed 30000 / 2000 ms walk 0, trie off by
default, standard shape (N=4 + budget 900, 300 markets, cap 400, rate 76,000,
`RETRY_BUSY=1`, 120 s), no perf:

- **A** = main `35e69b3` (the section 23.1 node), node `9f5c53bb`. The 23.1
  binary (`c2ea1ff8`) was pruned; rebuilt as 23.1 did (same worktree path and
  target-dir path, same flags, `-p torus-node -p bench-throughput`, unseeded,
  every workspace crate compiled from the 35e69b3 tree). Not bit-identical to
  `c2ea1ff8` (cause not found); A reproduces 23.1's matched/s (below).
- **B** = main `d3ba3c0a`, node-only build `193ae781` (the section 25 base).
- **C** = B's binary + `TORUS_BOOK_CF_TARGET_FILE_MB=64` (the book CF SST size
  before `d3ba3c0a`). The harness passes it through `EXTRA_ENV` unfiltered; the
  driver checked the variable on all 3 nodes and `target_file_size_base=67108864`
  for `cf_native_order_books` in each validator's RocksDB OPTIONS (B: 4194304;
  A: 67108864, the old default).
- **D** = `d3ba3c0a` built like A (`-p torus-node -p bench-throughput`), node
  `d76b4427`. Added because the combined build gives a different node binary
  than B's node-only build (bench-side `reqwest` / `hyper-rustls` / `tower`
  features unify into the node). It isolates build style.

Order: B warm (60 s), then A B C D D C B A; 04:59-05:51. All 9 cells rc 0,
AGREE, liveness PASS, accepted, no deaths, exe md5 3/3 as staged, oracle stale
0, no tail WARNING, max 675 open fds. Driver
`~/bench-results-matched/ozarchy-p2s0r-campaign.sh`, table
`ozarchy-p2s0r-table.txt`. (A first launch at 04:38 never started a cell: five
looping `gh api graphql` processes from another app held the load at ~7, and
the driver's quiet-host check gave up after 900 s; they were stopped by the
owner and the campaign relaunched.)

| cell | matched/s | submit/s | placed/s | native blk/s | fills/blk | engine ms/blk \| ms/1k | phase 1 | margin | match | settle | flush worker | chain | exec busy |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| B warm | 166,867 | 1,046 | 221,292 | 5.000 | 23,699 | 104.5 \| 4.41 | 17.4 | 24.1 | 13.0 | 37.9 | 43.0 | 131.1 | 0.942 |
| A r1 | 181,933 | 1,044 | 241,887 | 5.361 | 26,822 | 115.0 \| 4.29 | 21.3 | 26.9 | 13.6 | 40.6 | 49.1 | 142.4 | 0.967 |
| B r1 | 167,115 | 983 | 222,106 | 4.992 | 27,953 | 125.8 \| 4.50 | 22.5 | 29.4 | 15.6 | 44.4 | 56.6 | 159.7 | 0.965 |
| C r1 | 168,040 | 994 | 223,104 | 5.049 | 27,564 | 124.4 \| 4.51 | 23.1 | 28.9 | 14.7 | 44.9 | 51.5 | 156.2 | 0.961 |
| D r1 | 172,577 | 989 | 228,932 | 5.106 | 27,579 | 121.4 \| 4.40 | 22.3 | 28.4 | 14.5 | 43.4 | 52.3 | 153.6 | 0.967 |
| D r2 | 169,287 | 989 | 224,794 | 5.164 | 26,695 | 117.6 \| 4.40 | 20.6 | 26.6 | 14.1 | 42.5 | 50.2 | 150.8 | 0.965 |
| C r2 | 170,337 | 1,000 | 226,048 | 5.163 | 28,180 | 125.2 \| 4.44 | 22.0 | 29.0 | 14.8 | 45.2 | 51.5 | 157.9 | 0.966 |
| B r2 | 170,145 | 991 | 225,919 | 4.869 | 28,839 | 127.5 \| 4.42 | 21.9 | 30.3 | 15.9 | 45.8 | 52.8 | 161.0 | 0.959 |
| A r2 | 178,052 | 1,032 | 236,876 | 5.244 | 27,410 | 119.0 \| 4.34 | 21.1 | 27.6 | 14.3 | 42.4 | 52.6 | 148.5 | 0.968 |

ms per native block (val0, bench + drain window); submit/s = load-generator
accepted actions per s; exec busy = execution-thread busy fraction (load
window).

| mean of r1, r2 | A | B | C | D | B/A | C/A | D/A | C/B | D/B |
|---|---|---|---|---|---|---|---|---|---|
| matched/s | 179,993 | 168,630 | 169,188 | 170,932 | **0.937** | 0.940 | 0.950 | **1.003** | 1.014 |
| submit/s | 1,038 | 987 | 997 | 989 | 0.951 | 0.961 | 0.953 | 1.010 | 1.002 |
| placed/s | 239,382 | 224,013 | 224,576 | 226,863 | 0.936 | 0.938 | 0.948 | 1.003 | 1.013 |
| native blk/s | 5.30 | 4.93 | 5.11 | 5.13 | 0.930 | 0.963 | 0.968 | 1.036 | 1.041 |
| engine ms/1k | 4.31 | 4.46 | 4.47 | 4.40 | 1.034 | 1.037 | 1.020 | 1.003 | 0.987 |
| chain ms/blk | 145.4 | 160.4 | 157.1 | 152.2 | 1.103 | 1.080 | 1.047 | 0.979 | 0.949 |

- **Verdict (owner rules): B < A and C ≈ B.** `d3ba3c0a` is 6.3% below
  `35e69b3` on matched/s (both B cells below both A cells), and the 64 MiB
  book SSTs do not bring it back (C/B 1.003x). The 4 MiB SSTs are not the
  cause. Next: the owner's bisect of the merges in between (ADL budget / dirty
  check, exact cost basis, row 50 follow-ups), as a separate job.
- **Not the build style:** D/B 1.014x, within the ~5% cell resolution (D sits
  between B and A on engine ms/blk: 119.5 vs 126.7 / 117.0, a hint worth one
  more pair if the bisect lands on nothing).
- **Not the load generator:** with `MAX_IN_FLIGHT=4` the generator is closed
  loop (a slot frees when its action commits), so submit/s follows the chain.
  The execution thread is 96-97% busy on every arm and placed per submitted
  action is the same (A 231, B 227), so the chain, not the generator, sets the
  rate. The cost is on the node: engine +3.4% per 1k fills, chain +10% per
  native block.
- **Size of the gap:** A matches section 23.1's `35e69b3` cells (179,993 vs
  181,500 / 180,501), and B is 2.4% above section 25's base cells (168,630 vs
  164,608): of the ~9% in section 25.4, ~6% is the code and ~2-3% was the day
  or window.
- **For Phase 2:** the +7% gate is measured against `d3ba3c0a` (plan 9.7), so
  the gate's reference does not move; the regression is for 18c / the owner.

## 27. Bisect of the section 26 regression (campaign `ozarchy-p2s0x`, 2026-10-08)

Five more node-only builds on the first-parent path from `35e69b3` to
`d3ba3c0a`, each from its own detached worktree into its own fresh, unseeded
target dir, flags as section 26 (`--release`, line-tables-only, mold, frame
pointers; `ozarchy-p2s0x-build.sh`). B is section 26's `193ae781`. Same bench
`fff899ca`, harness, oracle feed and standard shape as section 26.

| arm | commit | node | merges since the previous arm (first parent) |
|---|---|---|---|
| a | `35e69b3` | `7a66c678` | - |
| p0 | `8582e827` (`a746c408^1`) | `58ebd01a` | B batch, row 74 storage halt, row 50 rejected status, StateDb compaction join, test flakes, dev debuginfo, docs |
| p1 | `a746c408` | `41b606d3` | `perf/adl-budget` (ADL work budget, C2 holder lists) |
| p2 | `2ebe1a14` (`9e695364^1`) | `b7798354` | ADL dirty check, 9.11 counter, governance params, item 7 step 0, read-precompile gas, antispam eviction metric, row 80, row 50 follow-ups |
| p3 | `9e695364` | `2549ecdf` | `fix/exact-cost-basis` (Position v2, cost basis) |
| b | `d3ba3c0a` | `193ae781` | `fix/test-dir-leak`, `fix/read-gas-followup` (4 MiB book SSTs) |

Order: b warm (60 s), then a p0 p1 p2 p3 b b p3 p2 p1 p0 a (120 s each);
13:23-14:39. All 13 cells rc 0, AGREE, liveness PASS, accepted, no deaths, exe
md5 3/3 as staged, trie off, oracle stale 0, no tail WARNING, max 439 open
fds; load before each cell 1.6-2.1. Driver
`~/bench-results-matched/ozarchy-p2s0x-campaign.sh` (copy of the section 26
driver), log `ozarchy-p2s0x.log`, cells `ozarchy-p2s0x-300m-<tag>/`.

| cell | matched/s | native blk/s | fills/blk | engine ms/1k | chain | settle | settle pass B | match | end_resident | `apply` | `apply` ms / 1k fills | end_resident wait |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| b warm | 175,335 | 4.68 | 24,920 | 4.29 | 133.7 | 38.7 | 22.3 | 12.7 | 21.4 | 9.13 | 0.366 | 7.7 |
| a r1 | 185,496 | 5.44 | 26,698 | 4.20 | 139.1 | 40.1 | 23.1 | 13.8 | 16.9 | 5.28 | 0.198 | 4.6 |
| p0 r1 | 183,252 | 5.46 | 27,880 | 4.22 | 146.2 | 41.8 | 24.0 | 13.9 | 18.0 | 5.60 | 0.201 | 5.3 |
| p1 r1 | 179,509 | 5.46 | 25,650 | 4.20 | 137.4 | 38.7 | 22.4 | 12.6 | 21.0 | 9.57 | 0.373 | 8.8 |
| p2 r1 | 171,552 | 5.17 | 28,280 | 4.32 | 156.3 | 42.0 | 23.7 | 14.8 | 25.2 | 11.24 | 0.397 | 10.7 |
| p3 r1 | 167,547 | 5.05 | 26,730 | 4.45 | 152.9 | 42.7 | 24.9 | 14.9 | 23.9 | 10.68 | 0.400 | 10.7 |
| b r1 | 171,985 | 4.83 | 27,920 | 4.44 | 155.5 | 45.1 | 26.8 | 15.3 | 23.7 | 10.47 | 0.375 | 9.6 |
| b r2 | 168,985 | 5.03 | 27,870 | 4.42 | 157.1 | 44.7 | 26.8 | 14.4 | 23.8 | 10.41 | 0.374 | 10.0 |
| p3 r2 | 170,863 | 5.13 | 28,040 | 4.46 | 157.4 | 44.6 | 26.2 | 15.1 | 23.5 | 10.47 | 0.374 | 9.8 |
| p2 r2 | 178,989 | 5.36 | 28,320 | 4.24 | 152.6 | 42.8 | 24.5 | 14.3 | 23.1 | 10.52 | 0.372 | 9.5 |
| p1 r2 | 173,415 | 5.04 | 27,380 | 4.37 | 151.9 | 42.6 | 24.5 | 14.4 | 23.1 | 10.43 | 0.381 | 9.8 |
| p0 r2 | 180,164 | 5.37 | 27,340 | 4.28 | 145.3 | 40.3 | 22.9 | 15.1 | 18.8 | 5.83 | 0.213 | 5.5 |
| a r2 | 182,789 | 5.34 | 27,740 | 4.24 | 145.3 | 40.4 | 22.7 | 14.5 | 17.9 | 5.49 | 0.198 | 4.8 |

ms per native block (val0) unless per 1k fills. `apply` is the
`exec_end_resident_positions_seconds` timer (the `TraderPositions::apply`
call in `native_executor.rs`), not a count of positions (18c s104;
`summarize.py` labels it `end_resident_positions`).

| mean of r1, r2 | matched/s | r1/r2 spread | vs a | vs b | engine ms/1k | chain ms / 1k fills | settle pass B ms / 1k fills | `apply` ms / 1k fills |
|---|---|---|---|---|---|---|---|---|
| a | 184,142 | 1.5% | 1.000 | 1.080 | 4.22 | 5.22 | 0.840 | 0.198 |
| p0 | 181,708 | 1.7% | 0.987 | 1.066 | 4.25 | 5.28 | 0.848 | 0.207 |
| p1 | 176,462 | 3.5% | 0.958 | 1.035 | 4.29 | 5.46 | 0.884 | 0.377 |
| p2 | 175,270 | 4.2% | 0.952 | 1.028 | 4.28 | 5.46 | 0.851 | 0.384 |
| p3 | 169,205 | 2.0% | 0.919 | 0.992 | 4.46 | 5.66 | 0.933 | 0.387 |
| b | 170,485 | 1.8% | **0.926** | 1.000 | 4.43 | 5.60 | 0.959 | 0.374 |

| step | matched/s | share of the a -> b gap | what moves |
|---|---|---|---|
| a -> p0 | -1.3% | 18% | nothing clear; within noise |
| **p0 -> p1 (`a746c408`)** | **-2.9%** | **38%** | `TraderPositions::apply` ms per 1k fills 0.207 -> 0.377 (1.8x), end_resident wait +3.9 ms per block; engine per fill unchanged |
| p1 -> p2 | -0.7% | 9% | within noise (the p1 and p2 cells overlap) |
| **p2 -> p3 (`9e695364`)** | **-3.5%** | **44%** | engine ms/1k 4.28 -> 4.46 (+4%), settle pass B per 1k fills 0.851 -> 0.933 (+10%) |
| p3 -> b | +0.8% | -9% | within noise |

- **Verdict: two merges, `a746c408` (ADL budget) and `9e695364` (exact cost
  basis), together ~80% of the gap.** The rest is spread over steps within
  noise. B/A 0.926 repeats section 26 (0.937).
- **Confidence:** matched/s alone does not settle it. The anchors' r1/r2
  spread is 1.5-1.8% (a, b) and up to 4.2% (p1, p2), and the two culprit steps
  are only ~2x that. The per-fill metrics split cleanly at the same two steps
  in every cell: `TraderPositions::apply` ms per 1k fills 0.198-0.213 in all a
  and p0 cells, 0.372-0.400 in every cell from p1 on; engine ms/1k 4.32 / 4.24 in
  the p2 cells, 4.45 / 4.46 in p3; settle pass B per 1k fills 0.839 / 0.863 in
  p2, 0.932 / 0.934 in p3. Chain ms per 1k fills rises at the same two steps
  (+3.4%, +3.8%) and is flat elsewhere. High confidence that both merges cost
  throughput, moderate on the exact split.
- **`TraderPositions::apply` time per fill** rises 1.8x at `a746c408` (a -> b
  0.198 -> 0.374 ms per 1k fills; 0.203 -> 0.386 in the section 26 cells). 18c
  s104 (code reading): C2's `set_holder` runs in `apply` on every written
  position key (~2 per fill), a SipHash map lookup plus a `BTreeSet` insert,
  even when nothing changes and no ADL is queued; holder lists are read only
  in `adl_candidates_of`.
- **Against section 26:** a is 2.3% higher (184,142 vs 179,993), b 1.1%
  higher (170,485 vs 168,630). Settle pass B per 1k fills a -> b is +14% here
  vs +6.9% in `ozarchy-p2s0r-phasecmp-ab.txt`; match per 1k fills +2% here vs
  +6% there.
- **Next (18c s104):** ozarchy fixes C2 (`set_holder` only when a key appears
  or disappears, non-SipHash hasher for the market map; node-local,
  bit-identical) and A/Bs it against `d3ba3c0a` as its own row, outside Phase
  2's +7%. Position v2 (96 -> 112 B) is not explained by the code (pass B
  touches Position only through `pos_cache.merge_disjoint`, and match, which
  never touches Position, rose too): ozarchy runs a perf A/B p2 vs p3 (perf
  diff of pass B and match, perf stat cache misses); 18c builds bit-identical
  savings meanwhile. The Phase 2 gate stays measured against `d3ba3c0a`.

## 28. Perf A/B of the exact cost basis merge: p2 `2ebe1a14` vs p3 `9e695364` (campaign `ozarchy-p2s0y`, 2026-10-08)

Asked by 18c (s104) after reading the code could not explain section 27's
p2 -> p3 step. The section 27 binaries (p2 `b7798354`, p3 `2549ecdf`), bench
`fff899ca`, harness and standard shape as section 27. Driver
`~/bench-results-matched/ozarchy-p2s0y-campaign.sh`, sidecar
`ozarchy-p2s0y-tools/perf2-sidecar.sh`, analysis `ozarchy-p2s0y-tools/p2s0y.py`
(`cells`, `rec`, `cmp`, `stat`); reports `ozarchy-p2s0y-{cells,xstat-cmp,prof-cmp,stat}.txt`
and `ozarchy-p2s0y-perfdiff-prof{1,2}.txt`. All cells rc 0, exe md5 3/3 as
staged, trie off, oracle stale 0, no deaths; load 1.7-1.98 before each cell;
`perf_event_paranoid=2`, all events `:u`, no sysctl change.

Cells, each mirrored p2 p3 p3 p2 after a 60 s p2 warm-up (15:58-16:52, unit
`bench-ozarchy-p2s0y-2.service`):

- **prof:** W1 whole-process `cycles:u` at 499 Hz with frame-pointer call
  graphs (35-80 s, the section 22 / 25 setting); W2 `perf stat` on the
  execution thread only (`-t <exec tid> --no-inherit`, 82-102 s). Costs ~3-6%
  of matched/s.
- **xstat:** W1 a 5-event `perf record` on the execution thread only, no
  inherit (cycles, instructions, L1d load misses, DRAM demand fills
  `ls_dmnd_fills_from_sys.mem_io_local`, one more); W2 whole-process `perf
  stat`. Costs ~0-2%: the closest to clean cells.
- The execution thread is the `torus-execution` task with the most CPU
  (~2,700 ticks vs 3 for the next).

| cell | node | matched/s | fills/blk | engine ms/1k | settle | pass B | match | pass B / 1k fills | match / 1k fills |
|---|---|---|---|---|---|---|---|---|---|
| p2 warm2 | b7798354 | 170,799 | 29,722 | 4.19 | 43.6 | 25.2 | 14.1 | 0.848 | 0.475 |
| p2 prof1 | b7798354 | 165,524 | 37,455 | 4.45 | 58.9 | 32.2 | 22.0 | 0.861 | 0.587 |
| p3 prof1 | 2549ecdf | 166,816 | 38,353 | 4.41 | 59.9 | 32.9 | 22.6 | 0.858 | 0.589 |
| p3 prof2 | 2549ecdf | 162,123 | 38,042 | 4.47 | 61.9 | 33.9 | 23.6 | 0.892 | 0.620 |
| p2 prof2 | b7798354 | 165,228 | 37,990 | 4.37 | 59.0 | 31.8 | 22.8 | 0.836 | 0.601 |
| p2 xstat1 | b7798354 | 172,823 | 27,274 | 4.34 | 41.0 | 23.5 | 14.5 | 0.861 | 0.531 |
| p3 xstat1 | 2549ecdf | 169,545 | 27,562 | 4.44 | 44.4 | 25.9 | 14.7 | 0.939 | 0.534 |
| p3 xstat2 | 2549ecdf | 173,465 | 28,161 | 4.32 | 43.7 | 25.7 | 14.5 | 0.912 | 0.516 |
| p2 xstat2 | b7798354 | 177,606 | 28,020 | 4.25 | 42.2 | 24.3 | 14.6 | 0.867 | 0.520 |

ms per native block (val0) unless per 1k fills. xstat means: matched/s p2
175,215 vs p3 171,505 (**-2.1%**, r1/r2 spread 2.7% / 2.3%), pass B per 1k
fills 0.864 vs 0.926 (**+7.1%**), match per 1k fills 0.526 vs 0.525 (flat),
engine ms/1k 4.30 vs 4.38.

### 28.1 Pass B: all in `PositionCache::merge_disjoint`

Execution thread, xstat W1, inline-expanded, mean of 2 cells per arm, per 1k
fills:

| | Mcycles p2 -> p3 | Minstr | IPC | DRAM fills (k) |
|---|---|---|---|---|
| pass B total | 2.465 -> 2.622 (**+6.4%**) | 2.645 -> 2.656 (+0.4%) | 1.07 -> 1.01 | 8.23 -> 9.33 (+13%) |
| `PositionCache::merge_disjoint` (`self.map.extend`) | **0.815 -> 0.979 (+20%)** | 1.08 -> 1.16 | 1.33 -> 1.19 | **2.33 -> 3.49 (+50%)** |
| of which `reserve_rehash_inner` | 0.265 -> 0.334 | 0.57 -> 0.56 | 2.17 -> 1.69 | 0.51 -> 1.11 (2.2x) |
| `persist_trade` | 0.361 -> 0.363 | flat | 0.81 / 0.80 | flat |
| `bal_cache` / hashing | 0.75 -> 0.71 | | | |

- **Verdict: memory traffic in one function, not more work.** Pass B
  instructions per fill are unchanged; `merge_disjoint` alone is +0.164
  Mcycles per 1k fills, the whole pass B delta (+0.157). Per cell it splits
  cleanly: 0.817 / 0.812 Mcycles in the p2 cells, 0.987 / 0.971 in p3 (DRAM
  fills 2.35 / 2.30 vs 3.77 / 3.20). The rest of the execution thread is
  +2.4% cycles (margin +0.09, cache flush +0.02), instructions flat (+-1%)
  in every class, DRAM fills +7%.
- The whole-process cycles profile (prof) agrees: pass B 2.128 -> 2.338
  Mcycles per 1k fills (+9.9%), `merge_disjoint` 0.718 -> 0.830,
  `find_or_find_insert_index` and `RawTable` +25-33%.
- **Likely mechanism (inferred from the layout, not measured):** the map
  entry `((Address, u64), Option<Position>)` with 16-byte-aligned `i128`
  FixedPoint is 32 + 96 = 128 B in p2 (two cache lines) and 32 + 112 = 144 B
  in p3: +12.5% bytes per insert and rehash, and entries no longer line up
  with cache lines. The 2.2x rehash DRAM fills are more than the bytes alone;
  a table crossing a cache or allocator threshold may add to it (not tested).
- **Fix (18c, `perf/position-v2-savings` `37b28dd6`, off `bf2edda6`):**
  `5d197a3b` reserves the batch `PositionCache` (map and dirty set) for the
  summed entry count before pass B's merges, so `extend` no longer rehashes
  (18c microbench 1,179-1,359 -> 665-1,018 ns per entry); `37b28dd6` updates
  the cached position in place on the fill path instead of cloning the 144 B
  entry (pass A). A u64 fast path for FixedPoint multiply was dropped (the
  bench's products are all above 2^64). If reserve leaves most of pass B's
  +7%, 18c's next steps are boxing Position in the cache map, or keeping the
  per-market caches separate and sorting their dirty keys once at flush.

### 28.2 Match: no regression

- Whole-process `cycles:u`: match workers do slightly less work per fill in
  p3 (7.145 -> 6.759 Mcycles per 1k fills, 0.95x); the match share on the
  execution thread (spawn and join) is 0.636 -> 0.681. Self-time movers
  (`match_at_level`, `match_market`, `place_order_with_accounts`,
  `__divti3`) all sit at 0.91-1.0x, none consistent across pairs.
- Timers in the xstat cells: 0.550 vs 0.551 ms per 1k fills. Section 27's
  match rise does not reproduce: noise.
- Stock `perf diff` (`--comms torus-execution`, relative) is not usable
  here: the two pairs disagree in direction for every symbol above +-0.15%,
  and the `HashMap<(Address,u64),Option<Position>>` frames move only because
  inlining changed (`extend` is its own frame only in p3). The inline-aware
  tables above are the reliable view.

### 28.3 perf stat (per 1k fills, mean of 2 cells, p3 / p2)

| event | execution thread (prof W2) | spread p2 / p3 | whole process (xstat W2) |
|---|---|---|---|
| cycles | 1.017 | 4.5% / 1.5% | 1.027 |
| instructions | 1.003 | 2.4% / 0.9% | 1.035 |
| IPC | 1.537 -> 1.516 | | 1.699 -> 1.712 |
| L1d load misses | 1.012 | 2.8% / 0.5% | 1.067 |
| DRAM fills | 1.025 | 3.8% / 0.5% | - |
| fills from another CCX | 1.008 | 10.5% / 5.6% | - |
| cache-references / cache-misses | - | | 1.040 / 1.049 |

The whole-process r1/r2 spread is 7-10%, larger than the effect, so that
column cannot resolve a 3% change (cache-misses per 1k instructions 3.64 vs
3.69). The execution-thread table's header in `ozarchy-p2s0y-stat.txt` says
"val0 whole process"; it is the execution thread only.

### 28.4 Profiling notes for this shape

- **Do not reuse the 5-event inherited `perf record`** (section 9.2's
  `ozarchy-ipc-tools/ipc-sidecar.sh`) on the standard shape: every
  short-lived worker thread (~15k spawns per minute) inherits all 5
  sampling events. The first p2s0y layout (cells `p2-rec1`, `p3-rec1`,
  unit `bench-ozarchy-p2s0y.service`) ran at 109-111k matched/s (~-37%),
  821 s of system CPU in a 50 s window, match and pass A timers ~10x. Kept
  for reference only; profile the execution thread with `-t <tid>
  --no-inherit` instead, or use plain `cycles:u` whole-process.
- **Zen 3 events:** `LLC-loads` / `LLC-load-misses` are not supported on this
  host; use `ls_dmnd_fills_from_sys.mem_io_local` (DRAM demand fills). With
  the NMI watchdog on only 5 counters are free: 5 events per window keeps
  them unmultiplexed. `ls_dmnd_fills_from_sys.ext_cache_local` is a fill from
  another CCX's cache, not an L3 hit (the section 9 sidecar's comment calls
  it "L3 fill"; section 9.2's tables do not use it).

## 29. C2 holder-index fix and Position v2 savings A/B (campaign `ozarchy-c2h`, 2026-10-08)

The two fixes for section 27's regression, A/B'd against `d3ba3c0a` as 18c
asked (s104), outside Phase 2's +7% gate. Node-only builds as section 27
(`--release`, line-tables-only, mold, frame pointers), each from its own
worktree into its own fresh, unseeded target dir (`ozarchy-c2h-build.sh`,
`ozarchy-c2h2-build.sh`); same bench `fff899ca`, harness and standard shape
as section 27, no perf.

| arm | commit | node | what |
|---|---|---|---|
| b | `d3ba3c0a` | `193ae781` | the Phase 2 reference (section 26's B) |
| base | `bf2edda6` | `29860dc0` | main, parent of both fixes (over `d3ba3c0a`: the open-file limit raise, a test flake fix, stress tests in every suite) |
| fix | `perf/c2-set-holder` `d7bd1c36` (built from `c051872b`, docs only on top) | `b843f522` | C2 holder index moved only when a position key appears or disappears; foldhash `holders` map (ozarchy; 18c s104 review: merge as is) |
| sav | `perf/position-v2-savings` `37b28dd6` | `4fcbf7cb` | 18c: `5d197a3b` reserves the batch `PositionCache` before pass B's merges; `37b28dd6` updates the cached position in place on the fill path |
| both | `f1ab2166` (local merge of `37b28dd6` + `d7bd1c36`, not pushed) | `17ac7525` | both fixes |

Order: b warm (60 s), then b base fix sav both both sav fix base b (120 s
each), 17:46-18:52, unit `bench-c2h.service`; then base warm (60 s), sav r3,
base r3, 18:57-19:13, unit `bench-c2hx.service` (`ozarchy-c2h-campaign-x.sh`),
because sav r1 failed. All other cells rc 0, AGREE, liveness PASS, accepted,
exe md5 3/3 as staged, trie off, 4 MiB book CF on every arm. Analysis
`ozarchy-c2h-tools/c2h.py`, table `ozarchy-c2h-table.txt`.

**sav r1 failed (rc 2, liveness unverified, excluded):** AGREE and no node
died, but all three nodes' RPC stalled for 6-10 s twice (about 38 s and 55 s
into the cell); `state_write_db` 68 ms per block (~23 elsewhere), flush 93
ms, 3.37 native blk/s, node CPU 299% vs ~385% (waiting, not busy). sav r2 and
r3 are normal: a host-wide stall, not the arm.

| cell | matched/s | native blk/s | fills/blk | engine ms/1k | chain | settle | pass B | pass B / 1k fills | match | end_resident | `apply` | `apply` / 1k fills | end_resident wait |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| b r1 | 173,407 | 5.163 | 25,718 | 4.39 | 142.3 | 41.5 | 24.4 | 0.947 | 13.6 | 21.9 | 9.34 | 0.363 | 8.61 |
| base r1 | 177,394 | 5.303 | 28,590 | 4.28 | 154.8 | 44.1 | 25.9 | 0.906 | 14.3 | 22.3 | 9.99 | 0.349 | 9.18 |
| fix r1 | 173,912 | 5.220 | 27,624 | 4.40 | 151.0 | 43.3 | 25.2 | 0.912 | 14.5 | 19.7 | 6.30 | 0.228 | 6.31 |
| ~~sav r1~~ | 135,694 | 3.369 | 26,522 | 3.98 | 169.4 | 35.5 | 19.7 | 0.741 | 13.2 | 20.6 | 9.03 | 0.340 | 6.70 |
| both r1 | 180,619 | 5.301 | 28,149 | 4.23 | 148.9 | 40.3 | 22.4 | 0.796 | 15.4 | 19.7 | 6.32 | 0.225 | 6.38 |
| both r2 | 177,660 | 5.148 | 27,880 | 4.30 | 150.4 | 39.6 | 21.8 | 0.780 | 15.2 | 21.3 | 6.92 | 0.248 | 6.93 |
| sav r2 | 178,037 | 5.138 | 28,381 | 4.17 | 150.4 | 40.3 | 22.6 | 0.798 | 14.6 | 22.8 | 10.17 | 0.358 | 9.32 |
| fix r2 | 177,892 | 5.320 | 27,586 | 4.38 | 149.3 | 43.2 | 25.1 | 0.911 | 14.4 | 19.1 | 6.35 | 0.230 | 6.37 |
| base r2 | 173,528 | 5.146 | 27,615 | 4.33 | 151.5 | 42.1 | 24.5 | 0.886 | 14.9 | 23.6 | 10.29 | 0.373 | 9.80 |
| b r2 | 174,680 | 5.131 | 28,069 | 4.34 | 154.8 | 44.5 | 26.2 | 0.932 | 14.4 | 23.5 | 10.66 | 0.380 | 10.00 |
| sav r3 | 176,912 | 5.114 | 28,843 | 4.23 | 154.8 | 41.1 | 23.1 | 0.802 | 15.1 | 24.4 | 10.83 | 0.375 | 9.69 |
| base r3 | 176,774 | 5.270 | 27,589 | 4.27 | 150.2 | 42.5 | 25.1 | 0.911 | 14.6 | 22.1 | 9.62 | 0.349 | 9.13 |

ms per native block (val0) unless per 1k fills; `apply` is the
`TraderPositions::apply` timer (section 27).

| mean (valid cells) | n | matched/s | spread | vs base | vs b | pass B / 1k fills | vs base | `apply` / 1k fills | vs base | end_resident wait | settle |
|---|---|---|---|---|---|---|---|---|---|---|---|
| b | 2 | 174,044 | 0.7% | 0.989 | 1.000 | 0.940 | 1.043 | 0.371 | 1.041 | 9.30 | 43.0 |
| base | 3 | 175,899 | 2.2% | 1.000 | 1.011 | 0.901 | 1.000 | 0.357 | 1.000 | 9.37 | 42.9 |
| fix | 2 | 175,902 | 2.3% | 1.000 | 1.011 | 0.911 | 1.011 | **0.229** | **0.642** | 6.34 | 43.3 |
| sav | 2 | 177,475 | 0.6% | 1.009 | 1.020 | **0.800** | **0.888** | 0.367 | 1.028 | 9.50 | 40.7 |
| both | 2 | 179,140 | 1.7% | **1.018** | **1.029** | **0.788** | **0.875** | **0.236** | **0.662** | 6.65 | 40.0 |

- **fix: `apply` per fill -36%** (0.357 -> 0.229 ms per 1k fills, both cells
  within 0.9%), end_resident wait 9.37 -> 6.34 ms per block. Most of the
  a746c408 step, not all: the p0 cells of section 27 were 0.201 / 0.213, so
  ~8-14% is left. Throughput alone: 1.000x base.
- **sav: pass B per fill -11%** (0.901 -> 0.800 ms per 1k fills, cells 0.798
  / 0.802), below p2's level in section 27 (0.839 / 0.863; today's b also
  reads ~2% below section 27's b). Reserve plus the in-place update recover
  the whole +7% of section 28 and more; no leftover from the 144 B entries
  moved by `extend` is visible at this resolution, so 18c's next steps
  (boxing Position, per-market caches) are not needed for this regression.
  Settle 0.949x base; `apply` unchanged, as expected. Throughput 1.009x base.
- **both: the gains add up.** `apply` per fill as fix (0.236), pass B per
  fill as sav (0.788), end_resident wait 6.65; the best arm at 1.018x base,
  1.029x b.
- **base vs b: neutral** (matched/s 1.011x, pass B 0.959x, `apply` 0.961x per
  fill).
- **Throughput is within noise.** Both fixes together take ~3 ms per block
  off the end_resident wait plus ~3 ms of pass B, ~4% of a ~150 ms chain,
  and matched/s moves +1.8% vs base, about base's 2.2% cell spread. The
  per-fill timers are unambiguous; a firmer matched/s number needs ~4 cells
  per arm. 18c s104: both branches go to main as separate `--no-ff` merges
  if each recovers its share.

## 30. P2-1 gate cell: cancel-all index `2ecc2bdf` vs main `e934fa0e` (campaign `ozarchy-p21g`, 2026-10-08)

Phase 2 step 1 (C1, the trader -> markets cancel-all index; plan P2-1, review log rows 17-23)
against the Phase 2 reference main `e934fa0e` (phase2 plan 9.11). Gate (18c s104): phase 1 down
by >= ~1.9 ms per native block (half of the ~3.7 ms no-perf estimate, section 25.1), 2 cells per
arm mirrored, 2 more if the drop lands within ~1.4-2.4 ms. Node-only builds from new detached
worktrees into fresh target dirs (c2h method; ref `28479dd1`, p21 `6f42e5c4`); both arms run the
same bench `84e73617` (rebuilt from `2ecc2bdf`; `tools/bench-throughput` has no diff since
`707f132f`, the `fff899ca` copy was pruned) and the `2ecc2bdf` harness (it samples the new index
gauges). Standard shape as sections 26-29, no perf. Order: ref warm (60 s), then ref p21 p21 ref,
23:01-23:30, unit `bench-p21g.service`. All cells rc 0, AGREE, liveness PASS, accepted, exe md5
3/3, 4 MiB book CF, trie off, oracle stale 0, no deaths. Driver `ozarchy-p21g-campaign.sh`,
analysis `ozarchy-p21g-tools/p21g.py`, table `ozarchy-p21g-table.txt`.

| cell | matched/s | native blk/s | fills/blk | engine ms/1k | chain | **phase 1** | phase 1 / 1k fills | match / 1k | settle / 1k | pass B / 1k | `apply` / 1k | end_resident wait |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ref r1 | 177,160 | 5.179 | 26,943 | 4.28 | 146.1 | **21.64** | 0.803 | 0.539 | 1.460 | 0.807 | 0.221 | 6.91 |
| p21 r1 | 177,372 | 5.230 | 28,303 | 4.29 | 151.8 | **22.10** | 0.781 | 0.534 | 1.452 | 0.808 | 0.224 | 5.91 |
| p21 r2 | 178,050 | 5.423 | 27,027 | 4.31 | 144.3 | **20.68** | 0.765 | 0.531 | 1.473 | 0.835 | 0.218 | 5.63 |
| ref r2 | 174,826 | 5.333 | 27,746 | 4.31 | 151.7 | **22.02** | 0.794 | 0.514 | 1.510 | 0.846 | 0.233 | 7.11 |

ms per native block (val0) unless per 1k fills; ref warm (180,752 matched/s) excluded.

| mean | matched/s | native blk/s | chain | phase 1 | phase 1 / 1k fills | match / 1k | settle / 1k |
|---|---|---|---|---|---|---|---|
| ref | 175,993 | 5.256 | 148.9 | 21.83 | 0.798 | 0.526 | 1.485 |
| p21 | 177,711 | 5.327 | 148.0 | 21.39 | 0.773 | 0.533 | 1.463 |
| p21 / ref | 1.010x | 1.013x | 0.994x | **-0.44 ms** | 0.968x | 1.012x | 0.985x |

| p21 cell | cancel-alls / blk | books visited / hit per cancel-all | index entries / traders at end |
|---|---|---|---|
| r1 | 59.2 | 189.1 / 92.4 | 252,457 / 1,317 |
| r2 | 56.4 | 188.6 / 92.4 | 347,123 / 1,701 |

Identical on all three validators. ref (main) has no cancel-all counters.
**`torus_exec_cancel_all_books_visited` changed meaning at P2-1:** it now counts only the books
the index visits (step 0 cells: ~300 = every book), so do not compare it with older cells.

- **Verdict: gate missed.** Phase 1 -0.44 ms per native block (val0) vs >= 1.9 ms; all three
  validators -0.30 ms; per 1k fills -3.2% (~0.7 ms per block at equal fills). Pairwise drops
  -0.46 to +1.34 ms, below the 1.4-2.4 ms band, so no extra cells. Spread: ref 0.38 ms, p21
  1.42 ms. Plan rule (section 4): the miss goes into the review log (row 24) and the work
  continues.
- **Why (from the counters, not profiled):** each cancel-all visits 189 books, not the ~87-95
  where the sender has something (books hit 92.4, as in step 0). About half the visits are stale
  index entries: "never removed eagerly" (phase2 plan 9.12) keeps a market listed after the
  trader's orders there have filled. The index swings between ~135k and ~634k entries during a
  cell (~690-2,700 traders, ~200 markets per trader) rather than growing. Scaling the ~3.7 ms
  estimate by the empty visits actually avoided (111 of ~208) gives ~2.0 ms, still above the
  0.44 ms measured, so the per-book saving also looks smaller than section 25.1's split implied.
- **Upkeep:** none visible (match per fill +1.2%, settle -1.5%, within noise). end_resident wait
  7.01 -> 5.77 ms, not established at n = 2.
- Options for 18c: (A) accept, continue; (B) remove a market from the trader's set when the
  trader's last order or stop in that book leaves it, then one more gate cell; (C) bring the
  9.12 background prune forward.

## 31. Cancel-by-id cost with P2-1 in: bdd5b470 vs e934fa0e (campaign ozarchy-p2byid, 2026-10-09)

Plan review log row 27 (d), phase2 plan 9.15: what does one CancelOrder / ModifyOrder by id cost
in phase 1 now that the P2-1 cancel-all index is in (used with resident books)? Step 0 (section
25.2, s-byid on `707f132f`): +3.1 ms per native block, ~2.5 ms per by-id action, 457 books probed
per action. Input to the priority of consensus item B (market in the order id); not a gate. Arms:
ref = main `e934fa0e` (node `8d7d596c`; rebuilt, the p21g `28479dd1` binary was gone), p2 =
`perf/item6-phase2` `bdd5b470` (node `e28bb121`). Node-only builds from new detached worktrees
(`wt/p2byid-e934fa0e`, `wt/p2byid-bdd5b470`) into fresh target dirs (p21g method,
`ozarchy-p2byid-build.sh`); both arms run the same bench `6c7ad1a7` (built from `bdd5b470`) and
the `bdd5b470` harness. Standard shape as section 30, no perf; by-id cells add
`CANCEL_BY_ID_FRACTION=0.1 MODIFY_FRACTION=0.05`. Order: ref warm (60 s, excluded), then
ref-byid-r1 p2-byid-r1 p2-std-r1 ref-std-r1 p2-byid-r2 ref-byid-r2, 00:37-01:18, units
`bench-ozarchy-p2byid-300m-<tag>.service`. All cells rc 0, AGREE, liveness PASS, accepted, node md5 =
staged md5 of the arm, 4 MiB book CF, oracle stale 0, 0 panic / fail-stop lines, no deaths. Driver
`/home/oz/bench-results-matched/ozarchy-p2byid-campaign.sh`, analysis
`ozarchy-p2byid-tools/p2byid.py` (from `p21g.py`), table `ozarchy-p2byid-tools/p2byid-table.txt`.

| cell | matched/s | native blk/s | fills/blk | **phase 1** | phase 1 val0-2 | phase 1 / 1k fills | by-id / blk | books probed / by-id | cancel-alls / blk | visited / hit per cancel-all |
|---|---|---|---|---|---|---|---|---|---|---|
| ref byid r1 | 179,228 | 5.352 | 26,647 | **22.71** | 22.82 | 0.852 | 0.903 | - | - | - |
| p2 byid r1 | 176,114 | 5.057 | 27,719 | **24.25** | 24.62 | 0.875 | 1.058 | 454.1 | 58.6 | 184.3 / 89.7 |
| p2 std r1 | 179,851 | 5.455 | 27,264 | **21.15** | 21.49 | 0.776 | - | - | 57.9 | 186.4 / 90.5 |
| ref std r1 | 179,192 | 5.528 | 26,417 | **20.73** | 21.20 | 0.785 | - | - | - | - |
| p2 byid r2 | 177,731 | 5.333 | 27,670 | **24.28** | 24.33 | 0.878 | 1.031 | 439.6 | 57.6 | 186.9 / 91.0 |
| ref byid r2 | 173,816 | 5.455 | 25,742 | **22.91** | 22.88 | 0.890 | 1.163 | - | - | - |

ms per native block (val0) unless noted; "phase 1 val0-2" = mean of the three validators.
By-id / blk = bench-side accepted cancels + modifies (bench.log `Cancel-by-id (load-gen)` line,
same bench binary both arms) / val0 phase-window native blocks; used for both arms because main has
no by-id or cancel-all node counters. On p2 the node count equals the bench count (881, 863).
Every by-id action sent was accepted; ~18.9-19.0k lookups per cell, ~18.0-18.2k found no own order,
0 errors (delivered 0.90-1.16 per block vs step 0's 1.22). ref warm (182,390 matched/s) excluded.

| mean | n | matched/s | native blk/s | fills/blk | phase 1 (r1/r2 spread) | phase 1 val0-2 (spread) | phase 1 / 1k fills | by-id / blk |
|---|---|---|---|---|---|---|---|---|
| ref byid | 2 | 176,522 | 5.404 | 26,195 | 22.81 (0.20) | 22.85 (0.06) | 0.871 | 1.033 |
| ref std | 1 | 179,192 | 5.528 | 26,417 | 20.73 | 21.20 | 0.785 | - |
| p2 byid | 2 | 176,922 | 5.195 | 27,694 | 24.27 (0.03) | 24.48 (0.30) | 0.876 | 1.044 |
| p2 std | 1 | 179,851 | 5.455 | 27,264 | 21.15 | 21.49 | 0.776 | - |
| p2 / ref byid | | 1.002x | 0.961x | 1.057x | +1.46 ms | +1.63 ms | 1.006x | |
| p2 / ref std | | 1.004x | 0.987x | 1.032x | +0.42 ms | +0.30 ms | 0.989x | |

| ms per by-id action = (phase 1 byid - phase 1 std) / by-id per blk | ref | p2 | p2 - ref |
|---|---|---|---|
| val0: step per native block | +2.08 | +3.12 | |
| val0: per action (r1, r2 against the one std cell) | **2.01** (2.19, 1.88) | **2.98** (2.93, 3.04) | +0.97 |
| mean val0-2: per action | 1.60 (1.79, 1.45) | 2.86 (2.96, 2.75) | +1.26 |
| per 1k fills step x byid fills: per action | 2.19 | 2.66 | +0.47 |

- **Verdict on (d): P2-1 did not cut the by-id cost.** One by-id action costs ~2.0 ms of phase 1
  on main and ~3.0 ms on `bdd5b470` (val0), on either side of step 0's ~2.5 ms. p2 is not
  lower; the +0.97 ms (p2 - ref) is not resolved. The input to item B's priority stays at
  step 0's level: ~2-3 ms of phase 1 per by-id action, ~1 action per native block in this shape.
- **Noise:** the by-id cells repeat closely (phase 1 r1/r2 0.20 ms ref, 0.03 ms p2; per action
  0.31 / 0.11 ms), but both lean on one std cell per arm. On this shape section 30's std pairs
  differ by 0.38 ms (ref) and 1.42 ms (p21), pooled sd ~0.74 ms per cell, so ~0.7 ms per action
  per arm and ~1.0 ms on p2 - ref: the +0.97 is ~1 sd. A P2-1 cut to below ~1 ms per action
  (~2 ms under p2's 2.98, ~2-3 sd) would have shown. Against section 30's std means (other
  binaries, reference only) the figures are ref 0.95, p2 2.75 ms per action: p2 is still not lower.
  The fills-normalised variant narrows p2 - ref to +0.47 ms (p2 by-id cells ran 5.7% more fills
  per block).
- **Why it did not move (sized, not profiled):** the probes are 454 / 440 books per action (all
  validators within 440-458, step 0 457), ~10-20 us (section 25.2), <1% of the cost. P2-1 cut the
  cancel-all walk to ~185 visited books (vs ~300 in step 0), but at ~0.07 us per visit (section 30)
  even one extra full cancel-all pass per by-id action is ~13-21 us, also <1%. So the "a by-id
  action splits a cancel-all run" mechanism (25.2, inferred), if it is the cause, does not cost
  through book visits; the 2-3 ms is elsewhere and these cells do not split it (no perf).
- **Harness limit:** it exposes cancel-alls per native block (by-id cells 58.6 / 57.6 vs 57.9 std
  on p2) and visited / hit per cancel-all (184-187 / 90-91, unchanged by by-id), not the number of
  cancel-all runs or how many runs the by-id actions split.
- **Throughput:** matched/s p2 / ref 1.002x (by-id), 1.004x (std), inside the ref by-id r1/r2
  spread (5,412, 3.1%). By-id lowers native blk/s on both arms (ref -0.12, p2 -0.26), within or
  near the r1/r2 spread (0.10, 0.28).
- **Caveats:** std cells n = 1 per arm; the delivered by-id share is ~1 per block (most lookups
  find no own order), so the per-action figure rests on a ~2-3 ms step; consensus timeouts 5-8 per
  cell in every cell, both arms. Next if 18c wants p2 - ref resolved: one more std cell per arm
  (~6 min each); to find where the 2-3 ms goes: a perf cell on p2 with a larger by-id share.

## 32. P2-5 gate cell: hasher 631becaa vs bdd5b470 (campaign ozarchy-p25g, 2026-10-09)

Phase 2 P2-5 (plan 9.9): execution-path `HashMap` / `HashSet` from std SipHash to alloy's map
(std `HashMap` + `foldhash::fast::RandomState`; node-local, bit-identical). Gate: engine ms per
native block down by >= ~5 ms (half of the ~10.3 ms microbench estimate, section 25.3) with
AGREE; matched/s reported because of the s82 caveat (an ahash A/B cut hash CPU with no matched/s
gain). Arms: ref = `bdd5b470` (node `e28bb121`, the p2byid staged binary, reused), p25 =
`631becaa` (node `2587e57f`, node-only build from detached worktree `wt/p25g-631becaa` into a
fresh target dir). Both arms run bench `6c7ad1a7` (from `bdd5b470`) and the `bdd5b470` harness
(`tools/` has no diff to `631becaa`). Standard shape as sections 26-31, no perf. Order: ref warm
(60 s, excluded), then ref p25 p25 ref, 01:42-02:11, units `bench-ozarchy-p25g-300m-<tag>.service`.
All cells rc 0, AGREE, liveness PASS, accepted, node md5 = staged md5 of the arm (3/3), 4 MiB book
CF, trie off (default), oracle stale 0, 0 panic / fail-stop lines, no deaths. The driver's
extension band (3.5-6.5 ms) read 8.59 ms, so no r3 cells. Driver
`/home/oz/bench-results-matched/ozarchy-p25g-campaign.sh`, tables `ozarchy-p25g-handoff-tables.txt`.

| cell | matched/s | native blk/s | fills/blk | **engine** | engine / 1k | chain | phase 1 | margin | match | settle | pass B | end_resident wait |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| ref r1 | 180,589 | 5.293 | 27,980 | **119.27** | 4.26 | 148.4 | 21.40 | 27.59 | 15.33 | 39.80 | 21.93 | 5.98 |
| p25 r1 | 186,414 | 5.648 | 27,157 | **108.05** | 3.98 | 139.7 | 18.32 | 23.77 | 13.47 | 38.56 | 20.92 | 6.86 |
| p25 r2 | 187,902 | 5.610 | 28,548 | **115.25** | 4.04 | 145.3 | 19.32 | 25.10 | 14.20 | 40.76 | 21.81 | 6.36 |
| ref r2 | 180,763 | 5.238 | 28,259 | **121.21** | 4.29 | 149.9 | 22.32 | 28.00 | 15.01 | 41.14 | 22.84 | 5.87 |

ms per native block (val0) unless per 1k fills; end_resident wait is the mean
(`phases.end_resident_wait.ms`; its p50 is 0.4 in every cell). ref warm (180,472 matched/s) excluded.

| mean | matched/s | native blk/s | fills/blk | engine (r1/r2 spread) | engine / 1k | chain | phase 1 / 1k | margin / 1k | match / 1k | settle / 1k |
|---|---|---|---|---|---|---|---|---|---|---|
| ref | 180,676 | 5.266 | 28,120 | 120.24 (1.94) | 4.275 | 149.1 | 0.777 | 0.988 | 0.540 | 1.439 |
| p25 | 187,158 | 5.629 | 27,853 | 111.65 (7.20) | 4.010 | 142.5 | 0.676 | 0.877 | 0.497 | 1.424 |
| p25 / ref | 1.036x | 1.069x | 0.990x | **-8.59 ms** | 0.938x | 0.955x | 0.869x | 0.888x | 0.921x | 0.989x |

- **Verdict: gate met.** Engine -8.59 ms per native block (val0; all three validators -8.43),
  -7.5 ms at equal fills (per 1k fills -6.2%), vs >= ~5 ms; AGREE on every cell. That is ~73%
  of the ~10.3 ms estimate. Pairwise -4.02 to -13.16 ms raw (one pair under 5 ms: p25 r2 ran 5%
  more fills, the busiest host load of the campaign); per 1k fills -0.22 to -0.31 (-6.2 to
  -8.7 ms at ref fills), arms do not overlap (ref 4.26-4.29, p25 3.98-4.04).
- **Where:** phase margin -3.36, phase 1 -3.04, match -1.34, settle -0.81 (pass B -1.02),
  post-engine tail -0.22, untimed +0.18; these sum to the engine step. Per 1k fills phase 1 -13%,
  margin -11%, match -8% separate cleanly, settle -1% is flat. Exec thread on-CPU per committed
  block 145.1 -> 132.5 ms; user CPU per 1k fills -1.6%, sys flat.
- **Throughput (s82 caveat):** matched/s 1.036x (pairwise +3.1 to +4.0%; spread ref 174, p25
  1,488), native blk/s 1.069x. Unlike s82 it moved: the exec thread is busy 0.964 of the load
  window in both arms, so engine time is on the critical path in this shape. Chain -6.7 ms, less
  than the engine step (end_resident wait +0.7, verify +0.3, residual +1.0, each 1-2.7 sd).
- **Caveats:** n = 2 per arm; raw engine spread 7.2 ms on p25 (fills and host load), so the raw
  step is ~2.3 pooled sd and the fills-normalised one ~8 sd; end_resident wait +0.7 ms not
  established; no perf, so the per-map split is inferred from the phase deltas. ref matches the
  earlier `e28bb121` cell (p2byid p2 std r1, 179,851 matched/s).

## 33. P2-2 gate cell and cumulative vs e934fa0e (campaign ozarchy-p22g, 2026-10-09)

Phase 2 P2-2 (plan section 3): batch cache flush, `StateBackend::write_cf_raw_many` with one batch
per `PositionCache` and `BalanceCache` flush (node-local, bit-identical). Gate: `cache_flush_ns`
per native block down by >= 5 ms vs the P2-1 + P2-5 arm with AGREE; the step 0 estimate was
~3.2 ms on the standard cells (plan row 14), so a miss was expected. The campaign also measures
the cumulative Phase 2 gain head to head against main `e934fa0e` (plan rows 31 and 33). Arms:
base = `e934fa0e` (node `8d7d596c`, the p2byid staged ref binary, reused), p25 = `029581e5`
(node `2587e57f`, the p25g staged binary, built from `631becaa`; the two differ only in docs),
p22 = `29320f6b` (node `5639b084`, node-only build of `b7e66ce5` + the test-only reference from
detached worktree `wt/p22g-29320f6b` into a fresh target dir, same RUSTFLAGS as p25g). All arms
run bench `6c7ad1a7` and the `bdd5b470` harness (`tools/` has no diff to `29320f6b`). Standard
shape as sections 26-32, no perf. Order: base warm (60 s, excluded), then base p25 p22 p22 p25
base, 02:52-03:32, units `bench-ozarchy-p22g-300m-<tag>.service`. All cells rc 0, AGREE (block
hash, state digest and counters equal on val0-2), liveness PASS, accepted, node md5 = staged md5
of the arm (3/3), 4 MiB book CF, trie off (default), oracle stale 0 (396 / 396 accepted), 0 panic
/ fail-stop / error lines, no deaths, fds max under 800. Driver
`/home/oz/bench-results-matched/ozarchy-p22g-campaign.sh`, tables `ozarchy-p22g-handoff-tables.txt`.

| cell | matched/s | native blk/s | fills/blk | **engine** | engine / 1k | chain | phase 1 | margin | match | settle | pass B | **cache flush** | cache flush / 1k | end_resident wait |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base r1 | 180,886 | 5.244 | 27,891 | **120.17** | 4.31 | 149.0 | 22.02 | 28.66 | 14.61 | 41.78 | 23.43 | **12.28** | 0.440 | 6.17 |
| p25 r1 | 190,715 | 5.836 | 26,929 | **107.23** | 3.98 | 135.2 | 17.75 | 23.36 | 13.66 | 38.22 | 20.41 | **11.64** | 0.432 | 6.09 |
| p22 r1 | 189,882 | 5.545 | 28,115 | **112.87** | 4.01 | 143.1 | 18.52 | 24.20 | 13.43 | 42.56 | 21.69 | **14.74** | 0.524 | 6.02 |
| p22 r2 | 186,740 | 5.590 | 26,719 | **108.77** | 4.07 | 137.3 | 18.35 | 22.40 | 12.27 | 41.62 | 21.38 | **14.67** | 0.549 | 5.49 |
| p25 r2 | 189,881 | 5.626 | 27,600 | **109.70** | 3.97 | 138.3 | 18.23 | 24.15 | 13.41 | 38.51 | 20.67 | **12.08** | 0.438 | 5.95 |
| base r2 | 181,749 | 5.467 | 27,124 | **114.46** | 4.22 | 143.7 | 21.06 | 27.56 | 14.17 | 39.12 | 21.83 | **11.57** | 0.427 | 6.41 |

ms per native block (val0) unless per 1k fills; the cache flush is timed inside settle (plan
step 0 notes), so settle includes it. base warm (183,626 matched/s) excluded.

| mean (r1/r2 spread) | matched/s | native blk/s | fills/blk | engine | engine / 1k | chain | phase 1 | margin | match | settle | pass B | cache flush | cache flush / 1k | end_resident wait |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| base | 181,318 (864) | 5.355 (0.223) | 27,507 | 117.32 (5.71) | 4.264 (0.089) | 146.3 | 21.54 | 28.11 | 14.39 | 40.45 | 22.63 | 11.93 (0.71) | 0.433 (0.014) | 6.29 |
| p25 | 190,298 (834) | 5.731 (0.210) | 27,265 | 108.47 (2.47) | 3.978 (0.007) | 136.7 | 17.99 | 23.76 | 13.54 | 38.37 | 20.54 | 11.86 (0.44) | 0.435 (0.005) | 6.02 |
| p22 | 188,311 (3,142) | 5.567 (0.045) | 27,417 | 110.82 (4.10) | 4.043 (0.056) | 140.2 | 18.44 | 23.30 | 12.85 | 42.09 | 21.54 | 14.71 (0.07) | 0.537 (0.025) | 5.76 |
| p22 / p25 | 0.990x | 0.972x | 1.006x | +2.36 ms | 1.016x | 1.025x | 1.025x | 0.981x | 0.949x | 1.097x | 1.048x | **+2.85 ms (1.240x)** | 1.234x | 0.956x |
| p25 / base | **1.0495x** | 1.070x | 0.991x | -8.85 ms | 0.933x | 0.934x | 0.835x | 0.845x | 0.941x | 0.949x | 0.908x | -0.07 ms | 1.003x | 0.957x |
| p22 / base | **1.039x** | 1.040x | 0.997x | -6.50 ms | 0.948x | 0.958x | 0.856x | 0.829x | 0.893x | 1.041x | 0.952x | +2.78 ms | 1.238x | 0.915x |

- **Verdict P2-2: gate missed, and the sign is reversed.** Cache flush per native block +2.85 ms
  (11.86 -> 14.71, +24%) instead of -5 ms (gate) or -3.2 ms (estimate); per 1k fills 0.435 ->
  0.537. Pairwise +2.59 to +3.10 ms; pooled sd 0.22 ms, step ~13 sd; all three validators in
  both replicates (p22 14.43-15.04, p25 11.44-12.08, base 11.37-12.48 ms) do not overlap. base
  and p25 are equal (-0.07 ms, 0.2 sd), as expected (P2-1 and P2-5 do not touch the flush).
- **Where:** settle +3.73 ms (7.6 sd) = cache flush +2.85 plus pass B +1.00 (4.9 sd; pass B
  per 1k fills 0.753 -> 0.785); pass A, post-engine tail and untimed are flat. Engine +2.36 ms
  (1.0 sd; +1.8 ms at equal fills, per 1k fills 1.016x, 2.3 sd), the rest offset by match
  -0.69 and margin -0.46, both inside their noise. The pass B step is not explained by the
  change (pass B runs before the flush); a carry-over from the batch (allocator / cache state
  for the next block) is a possibility, not established.
- **Throughput p22 / p25:** matched/s 0.990x (-1,987, 1.2 pooled sd; pairwise 0.979-1.000x),
  native blk/s 0.972x (1.5 sd): **not resolved**. At P2-5's measured rate (~0.4% matched/s per
  ms of engine, plan row 31) the +2.85 ms flush step predicts ~-1.1%, so the measured -1.0% is
  in line with it but the cells cannot separate it from zero.
- **Mechanism (hypothesis, no perf in this campaign):** the batch removes the second lookup,
  `intern_cf` and the per-row lock (~3.2 ms in the step 0 split), but it encodes every row into
  an owned `(Vec<u8>, Option<Vec<u8>>)` list first (key `to_vec` and value buffer allocated in
  one pass, consumed in a second, so cold by the time the overlay inserts them; the rows `Vec`
  grows from a `filter_map` without a size hint) and holds the overlay write lock for the whole
  batch. The measured step is ~6 ms worse than the estimate; which part costs it needs a
  profile or a `flush_all` microbench (old vs new at the cell's dirty-row count).
- **Cumulative vs `e934fa0e` (head to head):** p25 / base **1.0495x** (pairwise 1.045-1.054x,
  +8,980 matched/s, 15 pooled sd; native blk/s 1.070x), at or just above the top of the chained
  estimate ~1.040-1.046x (row 31). p22 / base **1.039x** (pairwise 1.027-1.050x, 4.3 sd), below
  the ~1.053-1.060x expected after P2-2 (row 31) because P2-2 regressed instead of saving
  ~3.2 ms. Against the +7% phase gate: ~2.0% short with p25, ~3.0% short with p22. Engine per
  1k fills p25 / base 0.933x (6.4 sd); phase 1 -3.55 ms and margin -4.36 ms (both ~6.5 sd).
- **Caveats:** n = 2 per arm; p22's matched/s spread (3,142) is ~3.7x the other arms', so the
  p22 throughput ratios are the weakest numbers here. The host ran ~1-3% faster than in earlier
  campaigns (base 181,318 vs `e934fa0e` 175,993 in section 30 and 179,192 in section 31; p25's
  node 190,298 vs 187,158 in section 32), so only the interleaved ratios carry; no perf, so the
  mechanism above is inferred. Plan row 33 (1): keep P2-2 on a non-overlapping gain, drop it
  only if it regresses; its gate metric regresses cleanly, matched/s is not resolved.

## 34. R01/R02 cost check and Phase 3 step 0 baseline: main 3efff0d6 vs 1b389700 (campaign ozarchy-p3s0, 2026-10-09)

18c asked, after the Phase 2 merge, whether main with the R01 / R02 fail-stop checks costs anything
against the Phase 2 bench head, and for the Phase 3 step 0 baseline. 18c's stop rule: "if 3efff0d6
is more than noise below 1b389700, stop and report before (d)" ((d) = the sync-point items of plan
9.16). Arms: A = `perf/item6-phase2` `1b389700` (the Phase 2 bench head, P2-1 + P2-5; node
`86477b00`), B = main `3efff0d6` (the Phase 2 merge; over A it adds R01 `19f8f534`, every failed
serial state or marker write fail-stops; R02 branches 1-2 `4bef6406` / `ed2354fb`, CoreWriter
drain errors and replay-guard / session / applied-height read errors fail-stop; and the eth_call /
estimateGas typing `9793f1ec`, RPC only; node `0c100f3b`). Both nodes built fresh (node-only, from
detached worktrees `wt/p3s0-1b389700` / `wt/p3s0-3efff0d6` into fresh target dirs, same flags as
p22g, one after the other): the p25g node (`2587e57f`) is not `1b389700`'s code, which differs from
`029581e5` / `631becaa` in 7 files of `crates/torus-mempool` (main `f5f28f89` merged on the
branch). Both arms run bench `6c7ad1a7` (the p2byid staged binary, reused) and the `bdd5b470`
harness (`tools/matched-bench` is identical in `bdd5b470`, `1b389700` and `3efff0d6`; `3efff0d6`
adds only `tools/r01-fault/`). Standard shape as sections 26-33, no perf. Order: A warm (60 s,
excluded), then A B B A, 07:22-07:53, units `bench-ozarchy-p3s0-300m-<tag>.service`. All cells rc
0, AGREE (block hash, state digest and counters equal on val0-2), liveness PASS, accepted, node md5
= staged md5 of the arm (3/3), 4 MiB book CF, trie off (default), oracle stale 0 (396 / 396
accepted; warm 216 / 216), 0 panic / ERROR lines, no exit 70, no deaths, fds max 724. The only
fail-stop match in the node logs is the startup INFO line `running state hash fail-stop
(TORUS_STATE_HASH_FAILSTOP) on=false`, on both arms. Driver
`/home/oz/bench-results-matched/ozarchy-p3s0-campaign.sh` (build `ozarchy-p3s0-build.sh`), tables
`ozarchy-p3s0-handoff-tables.txt`.

| cell | **matched/s** | native blk/s | fills/blk | txs/blk | engine | engine / 1k | chain | phase 1 | margin | match | settle | pass B | cache flush | end_resident wait | end_resident wait / 1k |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A r1 | **189,569** | 5.748 | 26,325 | 127.1 | 106.06 | 4.03 | 133.3 | 17.63 | 23.52 | 12.92 | 38.21 | 20.65 | 11.81 | 5.73 | 0.218 |
| B r1 | **189,485** | 5.492 | 27,862 | 138.4 | 110.14 | 3.95 | 141.3 | 18.77 | 24.30 | 13.23 | 39.41 | 21.10 | 12.27 | 6.36 | 0.228 |
| B r2 | **184,666** | 5.602 | 28,674 | 134.4 | 115.58 | 4.03 | 146.6 | 19.13 | 25.02 | 14.52 | 41.13 | 22.24 | 12.48 | 6.61 | 0.231 |
| A r2 | **188,914** | 5.805 | 26,145 | 129.6 | 105.53 | 4.04 | 133.0 | 18.53 | 23.11 | 12.79 | 36.88 | 19.84 | 11.25 | 5.80 | 0.222 |

ms per native block (val0) unless per 1k fills; txs/blk is the headline (bench window) figure. A
warm (182,495 matched/s) excluded.

| mean (r1/r2 spread) | matched/s | native blk/s | fills/blk | txs/blk | engine | engine / 1k | chain | phase 1 | margin | match | settle | pass B | cache flush | end_resident wait | end_resident wait / 1k |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A (`1b389700`) | 189,241 (655) | 5.777 (0.057) | 26,235 (180) | 128.4 | 105.80 (0.53) | 4.035 (0.010) | 133.1 | 18.08 | 23.32 | 12.86 | 37.55 | 20.25 | 11.53 (0.56) | 5.77 (0.07) | 0.220 (0.004) |
| B (`3efff0d6`) | 187,075 (4,819) | 5.547 (0.110) | 28,268 (812) | 136.4 | 112.86 (5.44) | 3.990 (0.080) | 143.9 | 18.95 | 24.66 | 13.88 | 40.27 | 21.67 | 12.38 (0.21) | 6.49 (0.25) | 0.229 (0.002) |
| B / A | **0.9886x** | 0.9603x | 1.0775x | 1.063x | +7.07 ms (1.067x) | 0.9888x | 1.081x | 1.048x | 1.058x | 1.079x | 1.073x | 1.070x | 1.073x | 1.125x | 1.044x |

- **Verdict: matched/s within noise; per-fill cost flat.** matched/s B / A 0.9886x (-2,166, -0.9
  pooled sd of 2,432; pairwise B1/A1 0.9996x, B2/A2 0.9775x). Per fill: engine per 1k fills
  0.989x (-1.1 sd; pairwise 0.980x / 0.998x), chain per 1k fills 5.075 -> 5.092 ms (+0.3%, 0.7
  sd), cache flush and pass B per 1k fills 0.996x / 0.993x.
- **Native blk/s 0.9603x (-3.7 sd; pairwise 0.9555x / 0.9650x), with bigger blocks:** fills per
  native block +7.8% (+2,033, +4.9 sd; pairwise 1.058x / 1.097x), txs/blk ~136 vs ~128, actions
  per block ~152.6 vs ~140.9, wall ms per committed block ~180.3 vs ~173.2. The per-block times
  grow with the block: engine +7.07 ms (2.6 sd), chain +10.8 ms (4.1 sd), margin +1.35 ms (3.3
  sd), settle +2.73 ms (2.5 sd), cache flush +0.85 ms (2.8 sd); per fill each of them is flat.
  Whether B's slower block rate makes its blocks bigger (more actions waiting per block) or the
  bigger blocks slow the rate is not resolved at n = 2. The -0.23 blk/s step is about one r1/r2
  spread of section 33's base and p25 arms (0.21-0.22).
- **Where (what rose per fill):** end_resident wait +0.72 ms per native block (5.77 -> 6.49,
  +12.5%, +5.5 sd; pairwise 1.110x / 1.140x) and +4.4% per 1k fills (0.220 -> 0.229, ~4 sd;
  pairwise 1.049x / 1.039x). It is the exec thread's join on the previous block's end_resident
  worker and sits on the write path R01 touches (the flush + applied-marker write whose failure
  R01 now fail-stops). Residual untimed exec-thread time also rose: +1.90 ms per native block
  (9.56 -> 11.45, 4.9 sd; val0-2 of both B cells 10.67-12.15 vs A 8.83-9.74), +11% per 1k fills
  (0.364 -> 0.405, 2.1 sd; pairwise 1.19x / 1.04x); save_books per 1k fills +3.3% (2.2 sd).
  verify, replay guard and the end_resident worker per 1k fills are flat (0.99-1.00x). A cause in
  R01 / R02 is **not established**: no perf, and at n = 2 the rise cannot be split from the
  bigger blocks (cause or effect of them).
- **Caveats:** n = 2 per arm; B's matched/s spread (4,819, b-r2 at 184,666) is ~7x A's (655),
  so B's matched/s and the B2/A2 pair are the weakest numbers here. Host level: A here 189,241
  matched/s vs the p25 arm's 190,298 in section 33 (0.994x); not the same binary (A includes the
  main `f5f28f89` mempool merge, p25 does not), so only the interleaved ratios carry.
- **Stop rule: pending 18c.** Facts for it: matched/s -0.9 sd (inside noise); native blk/s -3.7
  sd with blocks +7.8% bigger at flat per-fill cost; end_resident wait per 1k fills +4.4% (~4
  sd). Not decided here: accept B (`3efff0d6`, node `0c100f3b`: 187,075 matched/s, 5.547 native
  blk/s) as the Phase 3 step 0 baseline and go on to (d), or a 4-pair rerun (plan review log row
  38). (Decided in section 35: accepted.)

## 35. R01 / R02 split of the p3s0 step: 1b389700 / 3aa516e0 / 3efff0d6 (campaign ozarchy-p3s1, 2026-10-09)

18c's follow-up to section 34: split the p3s0 step (`1b389700` -> `3efff0d6`) into its two code
steps with three arms. A = `perf/item6-phase2` `1b389700` (the Phase 2 bench head; node
`86477b00`, the staged p3s0 A binary). B = `3aa516e0` (Phase 2 + main `95b01af2` merged on the
branch: R02 branches 1-2 `4bef6406` / `ed2354fb` and the eth_call / estimateGas typing `9793f1ec`,
no R01; node `db344840`, built fresh for this campaign: node-only, detached worktree
`wt/p3s1-3aa516e0`, fresh target dir, same flags as p3s0). C = main `3efff0d6` (B + R01
`19f8f534`; the other merge on the way, `1720e823`, is docs-only; node `0c100f3b`, the staged
p3s0 B binary). Code diffs (outside `docs/`): A -> B touches `native_executor.rs`, `app.rs`,
`torus-core` `error.rs`, `torus-rpc` (`eth.rs`, `types.rs`) and `torus-types` `lib.rs` (plus
tests); B -> C touches `app.rs`, `torus-state` `backend.rs` and `running_hash.rs` (plus
`tools/r01-fault/`). All arms run bench `6c7ad1a7` and the `bdd5b470` harness. Standard shape as
sections 26-34, no perf. Order: A warm (60 s, excluded), then rounds ABC, BCA, CAB, ACB (n = 4 per
arm), 09:21-10:40, units `bench-ozarchy-p3s1-300m-<tag>.service`. All 13 cells rc 0, AGREE,
liveness PASS, accepted, node md5 = staged md5 of the arm (3/3), oracle stale 0, 0 panic / ERROR
lines, no exit 70, no deaths, fds max 726; oracle 396 / 396 accepted in every counted cell except
a-r1 (392 / 396; warm 216 / 216). The startup INFO line `running state hash fail-stop
(TORUS_STATE_HASH_FAILSTOP) on=false` appears once in every node log, on all three arms. Driver
`/home/oz/bench-results-matched/ozarchy-p3s1-campaign.sh` (build `ozarchy-p3s1-build.sh`,
analysis `ozarchy-p3s1-analysis.py`), tables `ozarchy-p3s1-handoff-tables.txt`.

| cell | **matched/s** | native blk/s | fills/blk | txs/blk | engine / 1k | chain | end_resident wait | end_resident wait / 1k | end_resident wait / 1k (val0-2) | residual untimed | residual untimed / 1k | residual untimed / 1k (val0-2) | view timeouts | oracle |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A warm (excl.) | **189,575** | 4.935 | 25,293 | 112.2 | 3.91 | 125.6 | 6.18 | 0.244 | 0.245 | 9.96 | 0.394 | 0.381 | 1 | 216/216 |
| A r1 (outlier, excl.) | **172,338** | 4.333 | 28,868 | 135.2 | 3.90 | 157.7 | 5.65 | 0.196 | 0.193 | 25.89 | 0.897 | 0.877 | 6 | 392/396 |
| B r1 | **186,859** | 5.648 | 25,959 | 125.5 | 4.00 | 133.1 | 5.99 | 0.231 | 0.240 | 10.58 | 0.408 | 0.411 | 1 | 396/396 |
| C r1 | **184,015** | 5.504 | 27,105 | 132.2 | 4.05 | 139.9 | 6.49 | 0.239 | 0.243 | 10.99 | 0.405 | 0.405 | 1 | 396/396 |
| B r2 | **186,917** | 5.705 | 26,547 | 126.9 | 4.02 | 135.9 | 6.06 | 0.228 | 0.252 | 10.72 | 0.404 | 0.408 | 1 | 396/396 |
| C r2 | **188,821** | 5.902 | 25,544 | 128.2 | 3.94 | 129.3 | 5.71 | 0.224 | 0.234 | 11.41 | 0.447 | 0.439 | 2 | 396/396 |
| A r2 | **190,894** | 5.740 | 27,298 | 127.9 | 3.98 | 137.3 | 6.00 | 0.220 | 0.226 | 10.54 | 0.386 | 0.387 | 2 | 396/396 |
| C r3 | **186,727** | 5.697 | 26,345 | 132.3 | 4.01 | 135.1 | 6.16 | 0.234 | 0.233 | 10.95 | 0.416 | 0.411 | 0 | 396/396 |
| A r3 | **189,614** | 5.642 | 28,066 | 136.3 | 3.93 | 140.5 | 5.81 | 0.207 | 0.223 | 11.08 | 0.395 | 0.394 | 2 | 396/396 |
| B r3 | **187,295** | 5.025 | 32,320 | 144.5 | 4.03 | 162.8 | 6.60 | 0.204 | 0.205 | 11.55 | 0.357 | 0.358 | 2 | 396/396 |
| A r4 | **186,557** | 5.358 | 28,434 | 131.2 | 3.99 | 144.4 | 6.38 | 0.224 | 0.226 | 11.73 | 0.413 | 0.397 | 0 | 396/396 |
| C r4 | **185,703** | 5.228 | 27,956 | 132.8 | 4.05 | 142.5 | 6.86 | 0.245 | 0.246 | 9.60 | 0.343 | 0.347 | 1 | 396/396 |
| B r4 | **189,383** | 5.639 | 29,536 | 141.3 | 3.94 | 148.5 | 6.38 | 0.216 | 0.222 | 12.25 | 0.415 | 0.406 | 0 | 396/396 |

Run order, top to bottom. ms per native block (val0) unless per 1k fills; (val0-2) = mean over
the three validators; txs/blk is the headline (bench window) figure; view timeouts = val0, load
window.

| mean ± sd | matched/s | native blk/s | fills/blk | txs/blk | end_resident wait | end_resident wait / 1k | end_resident wait / 1k (val0-2) | residual untimed | residual untimed / 1k | residual untimed / 1k (val0-2) |
|---|---|---|---|---|---|---|---|---|---|---|
| A (`1b389700`, n = 3, a-r1 excluded) | 189,022 ± 2,228 | 5.580 ± 0.198 | 27,933 ± 579 | 131.8 | 6.063 ± 0.290 | 0.217 ± 0.009 | 0.225 ± 0.002 | 11.117 ± 0.596 | 0.398 ± 0.013 | 0.393 ± 0.005 |
| A incl. a-r1 (n = 4) | 184,851 ± 8,538 | 5.268 ± 0.644 | 28,167 ± 665 | 132.7 | 5.960 ± 0.314 | 0.212 ± 0.013 | 0.217 ± 0.016 | 14.810 ± 7.403 | 0.523 ± 0.250 | 0.514 ± 0.242 |
| B (`3aa516e0`, n = 4) | 187,614 ± 1,195 | 5.504 ± 0.321 | 28,590 ± 2,938 | 134.6 | 6.258 ± 0.285 | 0.220 ± 0.012 | 0.230 ± 0.021 | 11.275 ± 0.778 | 0.396 ± 0.026 | 0.396 ± 0.025 |
| C (`3efff0d6`, n = 4) | 186,317 ± 2,010 | 5.583 ± 0.287 | 26,737 ± 1,033 | 131.4 | 6.305 ± 0.489 | 0.236 ± 0.009 | 0.239 ± 0.007 | 10.738 ± 0.786 | 0.403 ± 0.043 | 0.401 ± 0.039 |

Steps: ratio of means (difference in pooled sd, pooled sd = sqrt((sd1^2 + sd2^2) / 2)); A is the
n = 3 arm (a-r1 excluded).

| metric | B / A (R02 + EVM typing) | C / B (R01) | C / A (the p3s0 step) |
|---|---|---|---|
| **matched/s** | **0.9926x (-0.79)** | **0.9931x (-0.78)** | **0.9857x (-1.27)** |
| native blk/s | 0.9864x (-0.28) | 1.0143x (+0.26) | 1.0005x (+0.01) |
| fills/blk | 1.0235x (+0.31) | 0.9352x (-0.84) | 0.9572x (-1.43) |
| engine / 1k fills | 1.0078x (+0.85) | 1.0038x (+0.32) | 1.0116x (+1.06) |
| end_resident wait (ms / blk) | 1.0320x (+0.68) | 1.0076x (+0.12) | 1.0399x (+0.60) |
| end_resident wait / 1k | 1.0127x (+0.26) | 1.0716x (+1.45) | 1.0852x (+2.02) |
| end_resident wait / 1k (val0-2) | 1.0212x (+0.32) | 1.0407x (+0.61) | 1.0627x (+2.93) |
| residual untimed (ms / blk) | 1.0142x (+0.23) | 0.9523x (-0.69) | 0.9659x (-0.54) |
| residual untimed / 1k | 0.9952x (-0.09) | 1.0175x (+0.19) | 1.0125x (+0.16) |
| residual untimed / 1k (val0-2) | 1.0080x (+0.17) | 1.0118x (+0.14) | 1.0200x (+0.28) |

- **Residual untimed: p3s0's +11% per 1k fills does not hold up.** Flat at both steps: B / A
  0.995x (-0.09 sd), C / B 1.017x (+0.19 sd), C / A 1.012x (+0.16 sd); per native block 11.12
  (A), 11.28 (B), 10.74 ms (C).
- **end_resident wait per 1k fills: the rise holds up, and it sits in the R01 step (C / B).** C / B
  1.072x (+1.45 sd; per round 1.04, 0.98, 1.15, 1.14), B / A (R02 + EVM typing) 1.013x (+0.26 sd),
  flat; C / A 1.085x (+2.02 sd; val0-2 1.063x, +2.93 sd), vs +4.4% in p3s0. It is small per block:
  6.06 -> 6.31 ms per native block over both steps (+0.24 ms, 0.6 sd), ~0.13% of the ~179 ms wall
  time per native block (A 179.3, C 179.5). Much of the per-fill rise at C / B comes from C's
  smaller blocks: per block that step is +0.05 ms (1.008x, +0.12 sd) while fills per block fall to
  0.935x; per block the larger share of the +0.24 ms is at B / A (+0.19 ms, +0.68 sd). Neither
  per-block step is resolved. Consistent with R01's change on the flush + applied-marker write
  that runs before the end_resident hand-off (its failure now fail-stops); the mechanism is not
  profiled.
- **Native blk/s flat: p3s0's -4% did not reproduce.** B / A 0.986x (-0.28 sd), C / B 1.014x
  (+0.26 sd), C / A 1.0005x (+0.01 sd), vs 0.9603x (-3.7 sd) in p3s0. Fills per block went the
  other way this time: C / A 0.957x (-1.43 sd; paired by round 0.94, 0.94, 0.94, 0.98), where p3s0
  had +7.8%.
- **matched/s: each step about -0.7% (-0.8 sd), C / A -1.4% (-1.3 sd).** B / A 0.9926x (-0.79 sd),
  C / B 0.9931x (-0.78 sd; per round 0.98, 1.01, 1.00, 0.98), C / A 0.9857x (-2,705 matched/s,
  -1.27 sd). Not resolved at either step, but the same direction as p3s0 (0.9886x, -0.9 sd): two
  campaigns now show a ~1-1.4% drift A -> C inside noise; if it is real it is split across both
  steps, not in one. With a-r1 included, A's mean drops to 184,851 (sd 8,538) and the sign flips
  (B / A 1.0149x, C / A 1.0079x), which is why a-r1 is excluded (below).
- **Caveats.** a-r1 is valid by every criterion but an outlier: 6 view timeouts on val0 (9 on
  val1, 9 on val2; every other cell 0-2 on val0, 0-3 on any node), residual untimed 25.89 ms per
  native block (the others 9.60-12.25), 172,338 matched/s, 4.333 native blk/s, wall 230.8 ms per
  native block (the other counted cells 169-199), oracle 392 / 396 (4 rejected, a send error to
  val2). Host load was not higher: `cpu.csv` load1 mean 20.1 vs 18.8-32.0 in the other cells. It
  is the first counted cell (right after the warm cell); cause not established. The main tables
  use A at n = 3, with the a-r1-included row given for comparison. Fills per block shift between
  arms again (b-r3 32,320, the other counted cells 25,544-29,536), so per-1k-fill ratios carry a
  denominator effect.
- **18c decision: accepted** (s107; plan review log row 39). `3efff0d6` (node `0c100f3b`: 186,317
  matched/s, 5.583 native blk/s at n = 4) is the Phase 3 step 0 baseline. The end_resident wait is
  recorded as a known small cost of the `1b389700` -> `3efff0d6` step (+0.24 ms/blk, mostly at R02
  branches 1-2 + EVM; R01 +0.05 ms); not resolved per step (18c s107, corrected from "R01 cost"
  after the per-block split above); the ~1% matched/s drift (two campaigns, -1.3 sd, spread over both steps) as unresolved.
  Excluding a-r1 accepted; both tables kept. Next (18c): the Phase 3 step 0 re-profile runs on the
  head Phase 3 builds on (after (d), with R01b if merged), with matched/s reported next to this
  baseline as a third campaign for the drift.

## 36. Classic vs mode 3 book layout control (campaign ozarchy-bkm, 2026-10-09)

18c's request (s107), as input to the owner's decision on the book mode in genesis (mode 3 vs
Classic on testnet): no Classic vs mode 3 measurement existed. The only earlier comparison is an
August mode 3 vs mode 2 run (10 markets, a different shape; per project memory, mode 3's
save_books was ~40% below mode 2's early in the run); it is not in this doc and not comparable.
One node for both arms: main `1eced05c` (`3efff0d6` + R02 branch 3; `b88b0c90` on top is docs
only; node `3fdad0ae`), built node-only from detached worktree `wt/bkm-1eced05c` into a fresh
target dir, same flags as p3s1. Bench `6c7ad1a7` (the p2byid staged binary, reused) and the
`bdd5b470` harness (`tools/matched-bench` identical in `bdd5b470` and `1eced05c`). The arms differ
only by env. A = Classic: `EXTRA_ENV='TORUS_BOOK_ROWS=0'` (`run-cell.sh` exports RECORD_ENV's
`TORUS_BOOK_ROWS=3` first and EXTRA_ENV after it, so 0 wins). B = mode 3 (`LevelAuthorityChunked`):
RECORD_ENV's `TORUS_BOOK_ROWS=3`, the bench config of sections 26-35. Standard shape as sections
26-35 (N = 4 + budget 900, cap 400, rate 76,000, RETRY_BUSY=1, 120 s, oracle feed 30000 / 2000 ms
walk 0, trie off, no perf) at 300 markets, then the same settings at 10 markets (as the p2s0r
10-market smoke cells). Per shape: A warm (60 s, excluded), then A B B A (n = 2 per arm),
12:20-13:14, units `bench-ozarchy-bkm-<mk>-<tag>.service`. Book mode verified per validator in
every cell: the summary's node env, `/proc/<pid>/environ` `TORUS_BOOK_ROWS`, the `order books
loaded from DB (level authority)` load_books line (modes 2/3 only), and the `__book_mode__` marker
byte read from the WAL at bench start (`ozarchy-bkm-walmarker.py`). A: 0, 0/0/0, 0/0/0 lines, byte
0/0/0; B: 3, 3/3/3, 1/1/1 lines, byte 3/3/3; no `book-mode marker mismatch` line. All 10 cells rc
0, AGREE, liveness PASS, accepted, node md5 = staged md5 (3/3), 4 MiB book CF, oracle stale 0 with
every sent mark accepted (300m 396 / 396, b-r1 402 / 402; 10m 198 / 198; warm 216 / 216 and
108 / 108), 0 panic / ERROR lines, no exit 70, no deaths, fds max 657. The driver unit held the
suite lock (`flock /tmp/claude-1000/torus-suite.lock`) from 12:18:45 to the end, so no test suite
ran during the cells; every cell started with no cargo / rustc running and load1 < 2 (1.69-1.95).
Host processes other than the nodes and the load generator are not recorded during a cell. Driver
`/home/oz/bench-results-matched/ozarchy-bkm-campaign.sh` (build `ozarchy-bkm-build.sh`, analysis
`ozarchy-bkm-analysis.py`, WAL marker reader `ozarchy-bkm-walmarker.py`), tables
`ozarchy-bkm-handoff-tables.txt`.

### 36.1 300 markets

| cell | book mode (env: /proc, la, wal) | **matched/s** | native blk/s | fills/blk | txs/blk | block | engine / 1k | save_books | save_books drain | save_books write (W) | save_books / 1k | flush | flush / 1k | flush / 1k (val0-2) | handoff wait | view timeouts | load1 max | oracle |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A warm (excl.) | 0: 0/0/0, la 0/0/0, wal 0/0/0 | **183,408** | 5.317 | 25,163 | 107.1 | 125.90 | 4.01 | 4.54 | n/a | n/a | 0.180 | 33.00 | 1.311 | 1.278 | 0.41 | 7 | 25.1 | 216/216 |
| A r1 | 0: 0/0/0, la 0/0/0, wal 0/0/0 | **192,143** | 5.837 | 26,852 | 133.6 | 134.12 | 4.01 | 4.96 | n/a | n/a | 0.185 | 31.18 | 1.161 | 1.186 | 0.64 | 5 | 31.6 | 396/396 |
| B r1 | 3: 3/3/3, la 1/1/1, wal 3/3/3 | **173,470** | 4.886 | 28,929 | 130.2 | 162.90 | 4.43 | 5.48 | 5.18 | 17.94 | 0.189 | 60.58 | 2.094 | 2.143 | 3.59 | 5 | 52.8 | 402/402 |
| B r2 | 3: 3/3/3, la 1/1/1, wal 3/3/3 | **190,422** | 5.574 | 27,856 | 134.7 | 141.06 | 3.99 | 4.39 | 4.20 | 16.08 | 0.158 | 54.53 | 1.958 | 1.955 | 3.57 | 5 | 53.2 | 396/396 |
| A r2 | 0: 0/0/0, la 0/0/0, wal 0/0/0 | **185,148** | 5.500 | 29,438 | 141.7 | 150.17 | 4.09 | 6.00 | n/a | n/a | 0.204 | 37.70 | 1.281 | 1.293 | 0.67 | 6 | 42.7 | 396/396 |

Run order, top to bottom. ms per native block (val0) unless per 1k fills; (val0-2) = mean over
the three validators; txs/blk is the headline (bench window) figure. book mode = the summary's
node env: `/proc` environ val0-2, level-authority load lines (la) val0-2, WAL marker byte val0-2.
save_books drain / write = mode 3's two-pass save: pass 1 (drain) runs on the exec thread inside
save_books; pass 2 (write) runs on the flush worker W inside the flush wall (deferred book save,
`exec_pipeline.rs`), so write is not part of save_books. Classic has no two-pass save (its split
counters stay 0): n/a, it is not instrumented that way. handoff wait = the exec thread blocked
handing the block's job to W. view timeouts = val0, load window; load1 max = `cpu.csv` (summary
`cpu.load1_max`).

| mean (r1/r2 spread) | matched/s | native blk/s | fills/blk | txs/blk | block | block / 1k | engine / 1k | save_books | save_books drain | save_books write (W) | save_books / 1k | save_books / 1k (val0-2) | flush | flush / 1k | flush / 1k (val0-2) | handoff wait | state write (W) | wall / committed blk |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A (Classic, `TORUS_BOOK_ROWS=0`) | 188,646 (6,994) | 5.668 (0.337) | 28,145 (2,586) | 137.6 (8.1) | 142.14 (16.05) | 5.048 (0.106) | 4.05 (0.08) | 5.48 (1.04) | n/a | n/a | 0.194 (0.019) | 0.203 (0.023) | 34.44 (6.52) | 1.221 (0.119) | 1.240 (0.107) | 0.66 (0.03) | 34.28 (6.54) | 176.6 (10.5) |
| B (mode 3, `TORUS_BOOK_ROWS=3`) | 181,946 (16,952) | 5.230 (0.688) | 28,392 (1,073) | 132.4 (4.5) | 151.98 (21.84) | 5.347 (0.567) | 4.21 (0.44) | 4.94 (1.09) | 4.69 (0.98) | 17.01 (1.86) | 0.174 (0.032) | 0.177 (0.030) | 57.55 (6.05) | 2.026 (0.137) | 2.049 (0.188) | 3.58 (0.02) | 34.64 (4.09) | 192.1 (25.3) |
| B / A | **0.9645x** | 0.9226x | 1.0088x | 0.9622x | 1.0692x | 1.0593x | 1.0395x | 0.9005x | n/a | n/a | 0.8932x | 0.8748x | 1.6712x | 1.6593x | 1.6532x | 5.4656x | 1.0104x | 1.0878x |
| B / A pairwise (r1 / r2) | 0.9028 / 1.0285 | 0.837 / 1.013 | 1.077 / 0.946 | 0.975 / 0.951 | 1.215 / 0.939 | 1.127 / 0.993 | 1.105 / 0.976 | 1.105 / 0.732 | n/a | n/a | 1.026 / 0.773 | 1.005 / 0.758 | 1.943 / 1.446 | 1.803 / 1.529 | 1.807 / 1.512 | 5.609 / 5.328 | 1.183 / 0.868 | 1.195 / 0.987 |

A warm (183,408 matched/s) excluded. state write (W) = the flush's state write, wall / committed
blk = headline `wall_ms_per_committed_block`. Spread = |r1 - r2|; ratios are ratios of means,
pairwise = b-r1 / a-r1 and b-r2 / a-r2.

### 36.2 10 markets

| cell | book mode (env: /proc, la, wal) | **matched/s** | native blk/s | fills/blk | txs/blk | block | engine / 1k | save_books | save_books drain | save_books write (W) | save_books / 1k | flush | flush / 1k | flush / 1k (val0-2) | handoff wait | view timeouts | load1 max | oracle |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A warm (excl.) | 0: 0/0/0, la 0/0/0, wal 0/0/0 | **267,447** | 12.889 | 16,342 | 80.7 | 48.41 | 2.24 | 2.35 | n/a | n/a | 0.144 | 9.17 | 0.561 | 0.552 | 0.62 | 15 | 31.6 | 108/108 |
| A r1 | 0: 0/0/0, la 0/0/0, wal 0/0/0 | **265,844** | 12.756 | 18,026 | 89.5 | 52.96 | 2.22 | 2.67 | n/a | n/a | 0.148 | 9.64 | 0.535 | 0.538 | 0.59 | 18 | 30.9 | 198/198 |
| B r1 | 3: 3/3/3, la 1/1/1, wal 3/3/3 | **270,635** | 11.959 | 19,421 | 96.3 | 57.82 | 2.20 | 2.87 | 2.78 | 6.53 | 0.148 | 19.89 | 1.024 | 1.030 | 1.74 | 22 | 33.1 | 198/198 |
| B r2 | 3: 3/3/3, la 1/1/1, wal 3/3/3 | **271,231** | 12.293 | 19,251 | 94.3 | 54.64 | 2.11 | 2.66 | 2.62 | 6.45 | 0.138 | 19.72 | 1.024 | 1.028 | 1.39 | 22 | 24.2 | 198/198 |
| A r2 | 0: 0/0/0, la 0/0/0, wal 0/0/0 | **269,275** | 13.211 | 18,198 | 89.3 | 53.11 | 2.21 | 2.60 | n/a | n/a | 0.143 | 9.42 | 0.518 | 0.518 | 0.63 | 19 | 32.6 | 198/198 |

| mean (r1/r2 spread) | matched/s | native blk/s | fills/blk | txs/blk | block | block / 1k | engine / 1k | save_books | save_books drain | save_books write (W) | save_books / 1k | save_books / 1k (val0-2) | flush | flush / 1k | flush / 1k (val0-2) | handoff wait | state write (W) | wall / committed blk |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| A (Classic, `TORUS_BOOK_ROWS=0`) | 267,559 (3,431) | 12.983 (0.455) | 18,112 (172) | 89.4 (0.2) | 53.03 (0.15) | 2.928 (0.020) | 2.21 (0.01) | 2.63 (0.07) | n/a | n/a | 0.145 (0.005) | 0.146 (0.006) | 9.53 (0.22) | 0.526 (0.017) | 0.528 (0.020) | 0.61 (0.04) | 9.45 (0.22) | 77.1 (2.7) |
| B (mode 3, `TORUS_BOOK_ROWS=3`) | 270,933 (596) | 12.126 (0.334) | 19,336 (171) | 95.3 (2.0) | 56.23 (3.18) | 2.908 (0.139) | 2.16 (0.09) | 2.77 (0.21) | 2.70 (0.16) | 6.49 (0.08) | 0.143 (0.010) | 0.143 (0.010) | 19.80 (0.17) | 1.024 (0.000) | 1.029 (0.002) | 1.56 (0.35) | 11.66 (0.16) | 82.4 (2.3) |
| B / A | **1.0126x** | 0.9340x | 1.0676x | 1.0660x | 1.0602x | 0.9930x | 0.9729x | 1.0493x | n/a | n/a | 0.9827x | 0.9816x | 2.0782x | 1.9465x | 1.9490x | 2.5656x | 1.2339x | 1.0701x |
| B / A pairwise (r1 / r2) | 1.0180 / 1.0073 | 0.938 / 0.931 | 1.077 / 1.058 | 1.076 / 1.056 | 1.092 / 1.029 | 1.013 / 0.973 | 0.991 / 0.955 | 1.075 / 1.023 | n/a | n/a | 0.998 / 0.967 | 0.997 / 0.966 | 2.063 / 2.093 | 1.915 / 1.979 | 1.915 / 1.984 | 2.949 / 2.206 | 1.228 / 1.240 | 1.066 / 1.074 |

Columns as in 36.1. A warm (267,447 matched/s) excluded.

- **Verdict: no throughput case for mode 3 over Classic at this shape; flush per fill is the one
  consistent effect.** matched/s B / A 1.0126x at 10m (+3,374; pairwise 1.0180 / 1.0073, both
  up, small) and 0.9645x at 300m (-6,700; pairwise 0.9028 / 1.0285: not resolved, b-r1 at
  173,470 carries it). Mode 3's flush per 1k fills is 1.95x at 10m (0.526 -> 1.024; pairwise
  1.92 / 1.98) and 1.66x at 300m (1.221 -> 2.026; pairwise 1.80 / 1.53); per native block
  9.53 -> 19.80 ms (2.078x; pairwise 2.06 / 2.09) and 34.44 -> 57.55 ms (1.671x; pairwise
  1.94 / 1.45). Flush is off-chain (pipelined on W), so it costs throughput only if it becomes the
  bottleneck. save_books on the exec thread is flat per fill: 0.98x at 10m (pairwise 1.00 /
  0.97), 0.89x at 300m (pairwise 1.03 / 0.77; r1/r2 spreads 10-18%, noisy). Any case for mode 3
  rests on its state root / design properties, which this campaign does not measure.
- **Native blk/s lower with mode 3, blocks bigger; per-fill block time flat at 10m.** 10m: native
  blk/s 0.934x (pairwise 0.938 / 0.931) with fills/blk +6.8% (pairwise 1.077 / 1.058), txs/blk
  95.3 vs 89.4; val0 block ms 1.060x (53.03 -> 56.23), per 1k fills 0.993x; engine per 1k fills
  0.973x. 300m: native blk/s 0.923x (pairwise 0.837 / 1.013), block ms 1.069x (142.14 -> 151.98;
  pairwise 1.215 / 0.939), per 1k fills 1.059x (pairwise 1.127 / 0.993), fills/blk 1.009x, engine
  per 1k fills 1.040x (pairwise 1.105 / 0.976): only b-r1 moves, so not resolved.
- **Where (the flush rise):** mode 3's deferred book save writes its pass 2 on the flush worker,
  inside the flush wall (save_books write 17.01 ms per native block at 300m, 6.49 at 10m), ~74% /
  ~63% of the flush rise (+23.12 / +10.28 ms). The flush's state write is flat at 300m (34.28 ->
  34.64 ms, 1.010x) and +2.21 ms at 10m (1.234x, both pairs); the rest of the rise (~5.8 ms at
  300m, ~1.6 ms at 10m) is not split by the counters. It already shows a little on the exec
  thread: handoff wait 0.66 -> 3.58 ms per native block at 300m (pairwise 5.6x / 5.3x), 0.61 ->
  1.56 ms at 10m (2.9x / 2.2x), ~2.4% / ~2.8% of mode 3's block ms. Flush is ~38% / ~35% of mode
  3's block ms (Classic ~24% / ~18%), so W still has headroom at this shape. On the exec thread,
  mode 3's save_books is almost all pass 1 (drain 4.69 of 4.94 ms at 300m, 2.70 of 2.77 at 10m);
  Classic's single-pass save_books (4.96-6.00 / 2.60-2.67 ms) has no split to compare against.
- **Caveats.** n = 2 per arm and shape. At 300m the r1/r2 spreads are large (B's matched/s 16,952
  vs A's 6,994; block ms 11-14%; save_books 19-22%): b-r1 is the slow cell (173,470 matched/s,
  block 162.90 ms, engine per 1k fills 4.43, wall 204.7 ms per committed block), so the 300m
  matched/s, native blk/s, block and save_books ratios are not resolved; the 10m ratios and the
  flush ratios hold in both pairs. Host load: at 300m both mode 3 cells peaked at load1 52.8 /
  53.2 vs 31.6 / 42.7 for Classic (mean 29.4 / 30.3 vs 20.2 / 23.2); at 10m there is no arm
  pattern (B 33.1 / 24.2, A 30.9 / 32.6). Whether mode 3's extra W work raises the load is not
  separated. View timeouts (val0): 5-6 in every counted 300m cell (warm 7), no arm pattern; at 10m
  18 / 19 Classic vs 22 / 22 mode 3 (warm 15). Host level: mode 3 here 181,946 matched/s at 300m
  vs section 35's C arm 186,317 (node `0c100f3b`, not interleaved), so only the interleaved
  ratios carry.
- **Owner decision (via 18c s107): Classic on testnet; the book mode does not go into genesis
  now** (plan 9.13, review log row 41). Basis: on throughput, mode 3 is +1.3% at 10m and
  unresolved at 300m; it raises flush per fill (1.95x / 1.66x), off-chain, with +0.95 / +2.9 ms
  of exec-thread handoff wait per native block; save_books per fill is flat. Classic stays the
  compiled default (`feat/compiled-defaults`). Revisit mode 3 only with a deep-book shape, before
  mainnet. For the record: the Phase 1 / 2 numbers in this doc were measured with mode 3 set by
  env; Classic performs about the same at this shape.

## 37. Phase 3 step 0 re-profile: main `9b7e29b2`, mode 3 and Classic (campaigns ozarchy-p3s0r, ozarchy-p3s0c, ozarchy-p3s0cf, 2026-10-10)

Three campaigns on one node binary. **p3s0r** (37.1-37.4): the main arm ran mode 3
(`TORUS_BOOK_ROWS=3`, the bench config of sections 26-35, kept for comparability with p3s1), with a
Classic side arm of n = 2 without perf. The owner chose Classic for testnet (section 36), and
Classic is the node default, so two follow-ups ran the same day: **p3s0c** (37.5), Classic n = 4
no-perf (2 new + the 2 p3s0r side cells) and 2 perf cells, and **p3s0cf** (37.6), 2 Classic and 2
mode 3 cells that save each validator's RocksDB `LOG`, for a per-column-family split of RocksDB's
background work. Reading in 37.7.

Node `6a71ba5f` and bench `001e3176`, both built from main `9b7e29b2` (compiled defaults
`8f131571`, R02 branch 4, the reduce-only modify sweep fix; every cell generates a fresh genesis).
Harness `tools/matched-bench` at `9b7e29b2`. Arm m: the six node-local plan-9.13 knobs exported
empty (empty parses as unset, so the compiled defaults apply), `TORUS_BOOK_ROWS=3` and
`TORUS_COMMIT_LAG_BACKOFF_CAP=8` from RECORD_ENV (cap 8 by env = the testnet genesis value; the
generated genesis carries 0). Arm k: arm m with `TORUS_BOOK_ROWS` empty (Classic). Standard shape
as sections 26-36 (N = 4 + budget 900, cap 400, rate 76,000, RETRY_BUSY=1, 120 s, oracle feed
30000 / 2000 ms walk 0) at 300 markets. Cells, 08:31-09:23: m-warm (60 s, excluded), m-r1 m-p1
m-r2 k-r1 m-r3 m-p2 m-r4 k-r2. Perf on m-p1 / m-p2 only: cycles:u 499 Hz, frame pointers, val0
whole process, 45 s from 35 s after bench start. All 9 cells rc 0, AGREE, liveness PASS, oracle
stale 0, no deaths, node md5 = staged md5 (3/3). Driver
`/home/oz/bench-results-matched/ozarchy-p3s0r-campaign.sh` (build `ozarchy-p3s0r-build.sh`,
analysis `ozarchy-p3s0r-analysis.sh`, tables `ozarchy-p3s0r-analysis.txt`).

### 37.1 Throughput (no-perf cells, val0)

| arm | n | matched/s | native blk/s | fills/blk | block ms | engine / 1k | cache_flush ms | flush ms | flush / 1k |
|---|---|---|---|---|---|---|---|---|---|
| m mode 3 | 4 | **186,504 ± 3,163** | 5.649 ± 0.164 | 27,104 | 138.29 ± 4.63 | 4.014 ± 0.058 | 12.17 ± 0.52 | 53.09 ± 2.75 | 1.958 ± 0.055 |
| k Classic | 2 | **190,958 ± 1,147** | 5.771 ± 0.022 | 27,475 | 136.84 ± 1.85 | 3.980 ± 0.015 | 12.07 ± 0.11 | 31.30 ± 0.08 | 1.139 ± 0.016 |
| p3s1 C `3efff0d6` (section 35) | 4 | 186,317 ± 2,010 | 5.583 ± 0.287 | | | | | | |

m vs p3s1 C: 1.001x matched/s. Different node and genesis, so this is a third data point for the
open ~1% drift item (it does not repeat the drift), not an A/B. The perf cells ran 177,931 ±
2,067 matched/s (-4.6%) at 35,175 / 31,961 fills per block, and perf multiplies sys CPU about 4x
(2.25 -> 8.5-9.1 CPU-s per 1M fills), so the per-block numbers in this section come from the
no-perf cells; the perf cells only rank costs.

### 37.2 Node CPU (val0, CPU-s per 1M fills, no-perf, threads.py over the load window)

| group | m mode 3 (n = 4) | k Classic (n = 2) |
|---|---|---|
| total (user + sys) | **29.87 ± 0.26** | **27.59** (0.924x) |
| short-lived exec workers (exited threads) | 7.93 | 6.33 |
| exec main thread | 4.11 | 4.27 |
| gossip (signature recovery) | 4.22 | 4.23 |
| ingress (verify + nonce read) | 3.80 | 3.74 |
| RocksDB background | 3.77 | 3.87 |
| tokio | 1.96 | 1.95 |
| flush worker | 1.74 | 0.97 |
| other | 1.54 | 1.55 |
| rpc | 0.80 | 0.71 |

On mode 3, execution (main + workers) is 12.0 (40%), signature work (gossip + ingress) 8.0 (27%),
and flush worker + RocksDB background 5.5 (18%). Counting the trade writer and hotstuff threads in
whole (per-block thread table: 18.2 and 28.7 ms per native block, ~1.7 per 1M) lifts the
persistence group to ~7.2 (~24%), an upper bound. **Phase 3's gate of about -10% node CPU-s/1M is
~3.0 CPU-s/1M: more than half of flush worker + RocksDB.** In the flush worker (m-p1), RocksDB
writes are 45% (memtable insert 36%) and the mode-3 deferred book save (`write_drained_book`) 29%.

### 37.3 Disk writes (val0, write_bytes)

| | m mode 3 (n = 4) | k Classic (n = 2) |
|---|---|---|
| GB per 1M fills | **0.704 ± 0.005** | **0.940 ± 0.011** (1.34x) |
| MB per native block | 23.26 ± 0.35 | 31.11 ± 0.42 |
| flush worker | 5.19 | 12.83 |
| RocksDB background | 7.39 | 7.12 |
| trade writer | 4.43 | 4.44 |
| gossip | 2.83 | 2.86 |
| hotstuff | 2.62 | 2.58 |
| rpc | 0.78 | 0.75 |

MB per native block by thread group, val0-2 pooled. Only the state path can coalesce: the flush
worker's batch and the part of RocksDB's background writes (memtable flush + compaction) that
belongs to state CFs. Trade rows, gossip and hotstuff writes are append-only. On mode 3 that is at
most (5.19 + 7.39) / 23.26 = **54% of the bytes**, less the non-state share of the background
writes (split by CF in 37.6). Classic writes 2.5x the flush worker bytes (whole books per block), so
its coalescable share is larger (37.5).

### 37.4 Phase 2 backlog candidates (plan review log row 37), perf m-p1

ms per native block at 35,541 fills per block, converted from cycles at the rate that maps
`PositionCache::flush_all` to its timed 15.17 ms (~2.77 GHz).

- **`delta_sums` decode reuse: not confirmed.** `delta_sums` is 0.63 ms of
  `run_liquidations_with`'s 9.96 ms; the borsh decode is 19% of it (~0.12 ms). The liquidation
  pass's cost is the view: `liq_view` 62%, `resident_changes` 33%, `traders_after` 30%
  (inclusive, overlapping).
- **Node-local cooldown / pending row index: not confirmed.** `clear_cooldown` 0.13 ms +
  `set_pending` 0.22 ms per native block.
- **Streaming P2-2: the cost is there.** `cache_flush` 12.17 ± 0.52 ms per native block (no-perf,
  0.449 per 1k fills, 11% of engine); in perf `put_position` is 56% of `flush_all`. The saving
  still depends on its microbench.

- **C2 `TraderPositions::apply` (row 40 asked for it):** 45.7% / 44.7% of `flush_all` cycles in
  m-p1 / m-p2, i.e. ~6.9 / ~6.1 ms per native block at 35,175 / 31,961 fills, **~0.19 ms per 1k
  fills** (perf cells; row 40 expected ~0.215-0.222 from ~0.229).

Restart -> ready (the third Phase 3 gate) was not measured.

### 37.5 Classic follow-up (campaign ozarchy-p3s0c)

Same node `6a71ba5f` and bench `001e3176` (reused from `ozarchy-p3s0r-stage/m`), same knobs as p3s0r
arm k (`TORUS_BOOK_ROWS` empty, six node-local knobs empty, cap 8). Cells, 10:43-11:10: k-warm (60 s,
excluded), k-r1 k-p1 k-r2 k-p2; perf as p3s0r. All 5 cells rc 0, AGREE, liveness PASS, oracle stale
0, no deaths, node md5 `6a71ba5f` (3/3); no `order books loaded from DB (level authority)` line in any
validator log (modes 2 / 3 print one), so every cell ran Classic. Classic no-perf n = 4 = p3s0c
k-r1, k-r2 + p3s0r k-r1, k-r2 (same binaries, node environments identical); m = p3s0r m-r1..r4.
Driver `ozarchy-p3s0c-campaign.sh`, tables `ozarchy-p3s0c-handoff-tables.txt`.

| no-perf, val0 | m mode 3 (n = 4) | k Classic (n = 4) | k / m |
|---|---|---|---|
| matched/s | 186,504 ± 3,163 | **189,611 ± 1,879** | 1.017x (+1.2 sd, not resolved) |
| native blk/s | 5.65 ± 0.16 | 5.75 ± 0.03 | |
| fills/blk | 27,104 | 27,305 | |
| chain ms | 138.28 ± 4.63 | 136.56 ± 2.35 | |
| engine ms / 1k fills | 4.01 ± 0.06 | 4.00 ± 0.03 | |
| cache_flush ms per native block | 12.17 ± 0.52 | 11.93 ± 0.39 | flat |

The two Classic pairs differ by 1.4% (p3s0c 188,264, p3s0r 190,958): campaign-to-campaign drift
of the size of the step, so matched/s does not separate the modes here (as in section 36).

Node CPU (val0, CPU-s per 1M fills, no-perf, threads.py over the load window):

| group | m mode 3 (n = 4) | k Classic (n = 4) | step (step / sd) |
|---|---|---|---|
| total (user + sys) | 29.87 ± 0.26 | **27.48 ± 0.14** | **-2.38, -8.0% (11)** |
| short-lived exec workers | 7.93 | 6.25 | -1.67 (21) |
| exec main thread | 4.11 | 4.28 | +0.17 (4.5) |
| gossip (signature recovery) | 4.21 | 4.23 | 0 |
| ingress (verify + nonce read) | 3.80 | 3.75 | 0 |
| RocksDB background | 3.77 | 3.80 | 0 |
| tokio | 1.96 | 1.95 | 0 |
| flush worker | 1.74 | 0.96 | -0.77 (17) |

Every Classic cell (27.30-27.61) is below every mode 3 cell (29.61-30.22). On Classic, execution is
38% of node CPU, signature work 29%, persistence (flush worker + RocksDB background + trade writer
0.60) 5.36 = 19.5%, 22.6% with the hotstuff thread: the same order as mode 3. -10% of Classic's node
CPU is **2.75 CPU-s/1M**.

Disk writes (GB per 1M fills, val0-2 pooled):

| | m mode 3 | k Classic |
|---|---|---|
| total | 0.704 ± 0.005 | **0.931 ± 0.019** |
| flush worker (state rows into the WAL / memtable) | 0.157 | 0.383 |
| RocksDB background (memtable flush + compaction) | 0.224 | 0.213 |
| trade rows / DA / block bodies (append-only) | 0.134 / 0.085 / 0.079 | 0.134 / 0.086 / 0.078 |

Of Classic's +0.227 GB/1M, the flush worker is +0.226 (write batch 10.0 vs 4.1 MB per native
block). The extra bytes do not reach the SST files: memtable flush rate is flat (15.4 vs 15.0 MB/s)
and compaction writes rise 2.5 MB/s. They are state rows rewritten every block that already
collapse in the memtable, i.e. exactly what Phase 3's coalescing removes. Coalescable share on
Classic 41-64%, append-only ~32%.

Phase 2 backlog on Classic (perf k-p1 / k-p2, 36,436 / 40,469 fills per native block): `delta_sums`
0.77 / 0.69 ms per native block, all of it deserialization: **not confirmed** (as mode 3).
`clear_cooldown` + `set_pending` 0.20 / 0.38 ms: **not confirmed**. Streaming P2-2: `flush_all`
15.8 / 17.3 ms under perf (`put_position` 53% / 55%), 11.93 ± 0.39 ms no-perf (0.437 per 1k fills):
**the cost is there**, same as mode 3.

### 37.6 Per-column-family split of RocksDB background work (campaign ozarchy-p3s0cf)

p3s0r / p3s0c saved whole-DB RocksDB counters only, and each cell's data dir is overwritten by the
next. One Classic `LOG` survived (p3s0c k-p2, copied to that cell dir as `rocksdb-LOG-val{0,1,2}.txt`)
and gave a first split; p3s0cf repeats it with a `LOG` copy per cell. Same node, bench and knobs;
driver `ozarchy-p3s0cf-campaign.sh` (the p3s0c driver plus the `LOG` copy at cell end). Cells,
11:32-12:00: k-warm (excluded), k-r1 m-r1 k-r2 m-r2, no perf. All 5 cells rc 0, AGREE, PASS, oracle
stale 0, no deaths, md5 `6a71ba5f` (3/3). In every cell and validator the `LOG` event sums equal the
node's own counters (compaction CPU 65.96-83.48 s, compaction writes 2.31-2.95 GB, flush writes
2.10-2.26 GB). k-p2 is included in the Classic RocksDB means (its RocksDB numbers sit inside the k
range) but not in node CPU (perf). matched/s: k 190,309 ± 2,831, m 191,412 ± 2,914 (n = 2, not
resolved). Node user CPU-s/1M: k 27.41, m 29.00 (+1.59, 4.4 sd).

Compaction by CF group (load window, CPU-s per 1M fills; k = r1 / r2 / p2, m = r1 / r2):

| CF group | k Classic | share | m mode 3 | share | step / sd | GB/1M k -> m | flush time share k -> m |
|---|---|---|---|---|---|---|---|
| trades (`native_trades`, `user_trades`) | 1.69 (1.74 / 1.64 / 1.62) | 54% | 1.15 (1.19 / 1.11) | 42% | -8 | 0.066 -> 0.047 | 32% -> 25% |
| DA (`cf_native_pending`) | 0.74 | 24% | 0.53 | 19% | -31 | 0.025 -> 0.017 | 14% -> 12% |
| **state rows** (positions, balances, order books, markets, liquidation, oracle) | **0.52** | 17% | **0.92** | 34% | +68 | 0.012 -> 0.024 | 40% -> 53% |
| block bodies + headers | 0.15 | 5% | 0.13 | 5% | -2.5 | 0.006 -> 0.005 | 11% -> 9% |
| hotstuff, nonces, other | 0.03 | 1% | 0.025 | 1% | | | |
| **total** | **3.12** | | **2.75** | | -7.0 | 0.110 -> 0.094 | |

Memtable flush writes by group are the same in both modes (trades 38%, DA 15%, state 35%, bodies
11%). The first, single-cell split (p3s0c k-p2) agreed: state rows 16.4% / 0.48 CPU-s/1M.

**Why mode 3 compacts less.** The -0.37 is trades -0.54 and DA -0.21, partly offset by state rows
+0.40. On the state side mode 3 runs ~10k tiny manual compactions of `cf_native_order_books` (0.44)
plus `book_order_rows` (0.09). The big append-only CFs get ~28% fewer flush rounds (88 -> 62 over
3 nodes for the same flushed bytes; val0 ~475 -> ~367 "WAL full" flushes), so fewer L0 files and
fewer L0 -> bottom rewrites (`user_trades` 24 -> 17 jobs of ~0.145 GB each). That the order book
manual flushes cause the fewer WAL-full rounds is likely, not shown.

**What Phase 3 can remove at most** (all of the flush worker + state-row compaction + state-row
share of memtable flush time):

| | k Classic | m mode 3 |
|---|---|---|
| flush worker | 0.97 | 1.69 |
| state-row compaction | 0.52 | 0.92 |
| state-row memtable flush | 0.27 | 0.43 |
| **total** | **1.76** (6.4% of Classic) | **3.04** |
| + exec cache flush (streaming P2-2) | 2.20 (8.0%) | 3.48 |
| node CPU after full removal | ~25.7 | ~26.0 |
| -10% of Classic: 24.67, needs | 2.75 | |

Even counting all memtable flush CPU as state, Classic reaches 2.60. Append-only compaction
(trades + DA) is 2.43 CPU-s/1M on Classic (8.9% of node CPU), 1.68 (6%) on mode 3. These
compactions go from L0 (LZ4) to the bottom level (ZSTD) and write nearly what they read (trades
1.37 GB in, 1.19 GB out): mostly re-compression.

### 37.7 Reading

- **On Classic, the testnet configuration, Phase 3 cannot reach its -10% node CPU gate** (high
  confidence): removing all state-row persistence work is ~1.76 CPU-s/1M (6.4%), 2.20 (8.0%) with
  streaming P2-2, against 2.75 needed; the margin is far above the replicate spread (~0.05).
  Persistence is the third cost in both modes (Classic ~20-23% of node CPU), behind execution
  (38%) and signature work (29%).
- **The larger persistence lever on Classic is append-only compaction** (trades + DA, 2.43
  CPU-s/1M, 78% of compaction), mostly LZ4 -> ZSTD re-compression; its compression or compaction
  style is outside Phase 3 as planned.
- **Classic vs mode 3 at this shape:** Classic 8.0% less node CPU (resolved), flush worker 0.56x,
  disk bytes 1.32x (the extra is state rows that collapse in the memtable), matched/s not
  resolved. Mode 3 compacts 0.37 CPU-s/1M less (fewer WAL-full flushes of the big CFs) but spends
  more in the flush worker and on order book compactions. On mode 3, full Phase 3 removal reaches
  -10% of mode 3's own CPU only just (3.04 vs 2.90, medium confidence: n = 2, flush worker from
  val0 only) and stays above Classic's -10% target.
- **Backlog:** `delta_sums` decode reuse and the cooldown / pending index are not confirmed in
  either mode; streaming P2-2's cost is there (~12 ms per native block).
- **Decision for 18c / the owner:** (1) keep Phase 3 with the gate lowered to about -6%, (2) add
  trades / DA compaction tuning to Phase 3, or (3) do the compaction tuning first as its own item;
  and pin the gate's baseline (Classic `9b7e29b2`; against p3s0r mode 3, Classic alone is already
  -8%). Restart -> ready is still not measured.

## 38. Trades + DA compaction tuning, screen (campaign ozarchy-acc, 2026-10-10)

Owner decision s108 (via 18c) after section 37: option 3, tune the append-only CFs' compaction first
as its own small item; gate baseline Classic on `9b7e29b2` (node `6a71ba5f`). Design
`docs/plans/append-cf-compaction.md` on `perf/append-cf-compaction`. The branch (`fa8b646f`, on main
`62896827`) adds two node-local env knobs, both exact-today when unset:
`TORUS_ROCKSDB_APPEND_CF_CODEC` = `zstd1` (bottommost ZSTD level 1) or `lz4` (LZ4 at every level) on
`cf_native_trades`, `cf_native_user_trades`, `cf_native_pending` only; `TORUS_ROCKSDB_WAL_COMPRESSION`
= `zstd`. The existing `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` (default 512) is the third lever. Tests:
OPTIONS shows each codec on the three CFs only; a data dir opens default -> new -> default -> new and
back, reading every row; Codex quick review no findings. Owner (18c, during the run): no disk-space
limit; prefer ZSTD level 1, LZ4 only if it clearly saves more CPU; report disk growth per day.

Setup as p3s0cf (Classic: `TORUS_BOOK_ROWS` empty, the six node-local knobs empty, cap 8, 300
markets, standard shape, fresh genesis per cell, RocksDB `LOG` copied per cell), no perf. New node
`2ede76eb` / bench `a33d82f5` from `fa8b646f`; control c on the kept stage `6a71ba5f` (`9b7e29b2`).
Arms: c control (n = 1); b new binary, knobs unset; z `zstd1`; l `lz4`; w
`TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048`; d `TORUS_ROCKSDB_WAL_COMPRESSION=zstd` (n = 2 each). Cells
13:19-14:27: b-warm (excluded), then interleaved rounds. All 11 cells rc 0, AGREE, PASS, ACCEPT,
oracle stale 0, no deaths, 0 panic / ERROR lines; node md5 3/3 per arm; the knob reached all three
validators (`/proc` environ) and the OPTIONS file shows the intended codec in every cell (z: ZSTD
level 1 on the three CFs only; l: LZ4; d: `wal_compression` ZSTD). Driver
`ozarchy-acc-campaign.sh`, tables `ozarchy-acc-run/handoff-tables.txt`, analysis scripts in
`/tmp/claude-1000/ozarchy-acc-analysis/` (to be copied to the bench archive with the confirm run).

### 38.1 Node CPU and throughput

Node CPU-s per 1M fills = val0-2 mean of `summary.json` `proc_cpu` over the load window (not the
val0 thread-table figure of 37.2 / 37.5, ~27.4 on Classic; compare arms only within this table).
↓ = lower is better, ↑ = higher is better.

| arm | node CPU-s/1M ↓ (r1 / r2) | step vs b (sd) | resolved? | matched/s ↑ (r1 / r2) |
|---|---|---|---|---|
| c control `6a71ba5f` | 29.83 (n = 1) | +0.50 | n = 1 | 188,523 |
| b new, knobs unset | **29.33** (29.25 / 29.40) | - | - | 190,674 (190,887 / 190,461) |
| z `zstd1` | 29.33 (29.92 / 28.73) | 0.00 (0.84) | no | 190,411 (188,484 / 192,338) |
| l `lz4` | 28.31 (28.59 / 28.02) | **-1.02, -3.5%** (0.40) | yes, 3.4 sd | 187,111 (185,035 / 189,187) |
| w WAL budget 2048 MiB | 27.65 (27.81 / 27.48) | **-1.68, -5.7%** (0.23) | yes, 9 sd | 193,013 (189,537 / 196,490) |
| d WAL compression | 29.72 (29.77 / 29.66) | **+0.39, +1.3%** (0.08) | yes, 4 sd | 197,486 (195,394 / 199,577) |

matched/s is not resolved in any arm (r1 / r2 spreads 2-3.6%; d's +3.6% is 3.2 sd but inside the
spreads of other arms). c is 0.50 above b on one cell, not explained; every arm is compared with b of
the same campaign. In val0's thread table the savings sit in the RocksDB background threads (l -1.54,
w -1.65, z -0.19 at 1.3 sd); d adds to the write path (flush worker +0.34, trade writer +0.24); exec,
gossip, ingress and tokio move 0.2 or less in every arm.

### 38.2 Compaction CPU by CF group (CPU-s per 1M fills, val0-2, from the LOG; ↓)

| arm | trades | DA | state rows | bodies | all |
|---|---|---|---|---|---|
| b | 1.76 | 0.89 | 0.60 | 0.15 | 3.43 |
| z | 1.60 | 0.85 | 0.58 | 0.15 | 3.22 |
| l | 0.74 | 0.41 | 0.60 | 0.17 | 1.95 |
| w | 0.84 | 0.52 | 0.35 | 0.11 | 1.83 |
| d | 1.74 | 0.85 | 0.56 | 0.15 | 3.33 |

l halves trades + DA compaction (no LZ4 -> ZSTD re-compression; -1.48 compaction, -1.02 node: part of
the saving comes back elsewhere). w cuts every group: the 512 MiB WAL budget forces flushes of all CFs
when the WAL fills (val0: 97 in b, 27 in w), so b runs 557 flushes and 170 compactions, w 236 and 79.
The compaction backlog is not larger in w, so the saving does not look like compaction pushed past
the 120 s window, but 120 s cannot rule that out.

### 38.3 Disk (per validator)

| arm | all writes MB/s ↓ | WAL writes MB/s ↓ | flush / compaction SST MB/s ↓ | GB per 1M fills ↓ | data dir at end GB ↓ | SST growth GB/day ↓ | trades + DA GB/day ↓ |
|---|---|---|---|---|---|---|---|
| b | 180 | ~139 | 17.4 / 22.2 | 1.09 | 1.40 | 680 | 511 |
| z | 181 | ~138 | 17.4 / 22.2 | 1.08 | 1.22 | 672 | 498 |
| l | 191 | ~138 | 17.3 / **30.7** | 1.17 | 1.72 | **912** | **754** |
| w | 168 | ~136 | 16.8 / **12.3** | 1.00 | **2.71** (1.73 WAL) | 657 | 503 |
| d | **56** | **13.1** | 17.9 / 23.0 | **0.32** | 1.04 | 705 | 528 |

- Writes are per-thread `write_bytes` over the load window. The RocksDB WAL ticker
  (`torus_rocksdb_wal_bytes`) counts bytes before compression, so it does not show d's saving: d's
  ticker reads 143 MB/s while the WAL threads wrote 13.1.
- GB/day extrapolates the live SST set's growth over a 120 s window at bench load (76,000 / s offered,
  ~190k matched / s) to a day. The chain is young (L0 compacts straight into the bottom level), so this
  is not a steady-state rate; compare arms, not absolutes. Append-CF SST per 1M fills: b 0.036 GB, l
  0.054 GB (+50%).
- w keeps up to 2 GiB of WAL on disk (1.73 GB at the end), and memtables average 405 MB vs 230. Its
  restart -> ready (WAL replay) was not measured; the harness has no restart step. The 512 MiB
  default came from the s74 crash A/B (restart DB open 20.5 s -> 7.9 s), so a larger budget can give
  back restart time.
- d adds ~9 ms to the flush per native block and ~1.4 ms to commit persist.

### 38.4 Reading

- **No single setting reaches the -10% gate (2.75 CPU-s/1M).** w (an existing knob) is -1.68 (61% of
  it, about the -6% alternative), l -1.02, z nothing resolved, d +0.39.
- **ZSTD level 1 vs LZ4 (owner rule):** l - z on node CPU is -1.02 at 1.5 sd (z's replicates are
  wide): not resolved. On compaction CPU the gap is -1.27 at 7.5 sd, so the mechanism is real, at +48%
  append-CF SST growth. Not "clearly" more CPU, so ZSTD level 1 stands; on its own it changes nothing
  measurable.
- **w + a codec was not measured.** Both cut the same trades / DA compaction, so the combination is
  likely -1.8 to -2.4 (estimate), still short of 2.75.
- **d is the SSD-wear lever:** -69% of all bytes written per validator (WAL 139 -> 13 MB/s) for +1.3%
  node CPU and ~9 ms more flush per block. A separate decision for the owner; it also bears on the
  Phase 3 coalescing question (section 37: the per-block state-row rewrite is 56% of the WAL).
- **Next, before any default changes:** n = 4 confirm of b, w, w + z (and w + z + d if the owner
  wants the full combination), with a restart -> ready step for the w arms (WAL replay of up to
  2 GiB, against the s74 restart result).

## 39. Trades + DA compaction tuning, confirm (campaign ozarchy-acc2, 2026-10-10)

Owner (18c, after section 38; screen merged as `233c026a`): ZSTD level 1 changed nothing measurable
(CPU, matched/s and SST growth all equal to b), so it is dropped from the plan, with no w + z or
w + z + d arms. Confirm n = 4 per arm, interleaved: b, w, w + d, and b with trade history off.
matched/s is reported with its resolution (d's +3.6% in the screen needed confirming). Restart ->
ready is measured for both WAL 2048 arms. Trade history is node-local (RPC / explorer only), and
validators do not need it; the owner wants its cost before setting validator defaults.

Setup as section 38 (Classic, cap 8, 300 markets, standard shape, fresh genesis per cell, RocksDB
`LOG` copied per cell, no perf), same node `2ede76eb` / bench `a33d82f5` (`fa8b646f`, stage
`ozarchy-acc-stage/n`), no old-binary control. Arms:
- **b:** knobs unset.
- **w:** `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048`.
- **wd:** w + `TORUS_ROCKSDB_WAL_COMPRESSION=zstd`.
- **t:** `TORUS_TRADE_HISTORY=0`.

Cells 18:32-21:21: b-warm (excluded), then 4 rounds with the arm order rotated each round (b w wd t /
w wd t b / wd t b w / t b w wd), then a restart block (9 cells, below). All 16 throughput cells rc 0,
AGREE, PASS, ACCEPT, oracle stale 0, no panic or ERROR line. Node md5 3/3 per cell, the knobs in every
validator's `/proc` environ, `max_total_wal_size` in the `LOG` 512 MiB for b / t and 2048 MiB for w /
wd, and `wal_compression` ZSTD on wd only. Trade history off in t: `cf_native_trades` and
`cf_native_user_trades` hold 0 data rows on all 3 validators in all 4 t cells. Each holds only the
range tombstone that `ensure_trade_history_format` writes once on a fresh DB, and every history-on
cell shows rows as a positive control. The first launch stopped at t-r1 (18:59) on a stricter
version of that check (zero table files, which counted the tombstone file); the check was corrected
to count data rows and the campaign resumed at t-r1 (19:02). Driver `ozarchy-acc2-campaign.sh`,
tables `ozarchy-acc2-run/handoff-tables.txt`, analyst scripts copied to `docs/perf/bench-archive/tools/ozarchy-acc2-tools/`.
The screen's `/tmp/claude-1000/ozarchy-acc-analysis/` (section 38) was gone from `/tmp` by the
time of this write-up, so it is not in the bench archive.

### 39.1 Node CPU and throughput (n = 4; ↓ lower is better, ↑ higher is better)

Node CPU-s per 1M fills is the val0-2 mean of `summary.json` `proc_cpu`, as in 38.1 (val2 runs ~2.7
above val0 / val1 in every cell; b's val0 alone is 27.89, in line with the 27.48 of 37.5).

| arm | matched/s ↑ mean (sd) | vs b | step / sd | node CPU-s/1M ↓ mean (sd) | vs b | step / sd |
|---|---|---|---|---|---|---|
| b | 188,229 (1,678) | - | - | 28.80 (0.06) | - | - |
| w WAL 2048 MiB | 192,732 (1,547) | +2.4% | 2.8, marginal | 26.97 (0.10) | **-1.83, -6.3%** | 22 |
| wd WAL 2048 + WAL ZSTD | 198,071 (1,914) | **+5.2%** | 5.5 | 27.42 (0.14) | **-1.38, -4.8%** | 13 |
| t trade history off | 203,669 (2,991) | **+8.2%** | 6.4 | 25.36 (0.32) | **-3.44, -11.9%** | 15 |

- w confirms the screen (-1.68 there, -1.83 here).
- wd vs w: +0.45 CPU-s/1M (3.7 sd), the cost of compressing the WAL, and +2.8% matched/s (3.1 sd).
  The screen's d arm (+3.6%, n = 2) had WAL compression without the 2048 cap, so it is not the same
  arm, but the direction is confirmed. CPU does not explain wd's gain (ZSTD adds CPU); less I/O
  contention on the shared disk is the likely cause, not measured.
- t alone passes the Phase 3 -10% gate (2.75): -3.44.
- b rose from 186.6k to 190.3k across its four cells: ~1% drift is part of the noise, and matched/s
  is medium confidence.

### 39.2 Disk (per validator; ↓ lower is better in every column)

| arm | all writes MB/s | WAL writes MB/s | RocksDB flush + compaction MB/s | data dir at end GB (SST / WAL) | SST growth GB/day | trades + DA GB/day |
|---|---|---|---|---|---|---|
| b | 163.8 | 133.3 | 40.0 | 1.35 (0.94 / 0.40) | 634 | 476 |
| w | 160.9 | 138.3 | 31.4 | 2.71 (0.96 / 1.74) | 661 | 499 |
| wd | **43.3 (-74%)** | **13.0 (-90%)** | 32.0 | 1.20 (1.03 / 0.17) | 703 (+11%) | 538 |
| t | 130.7 (-20%) | 116.1 | 21.3 | **0.81 (0.41 / 0.40)** | **292 (-54%)** | **129** (DA only) |

- All writes: `io-ticks.txt` over the bench window, 3 node processes summed, / 3. WAL writes: the
  threads not named `rocksdb:*` (`io-threads-*.txt`); flush + compaction: `rocksdb:low` / `high`.
  The thread window runs ~6 s into the drain, so the last two columns are close to, not on, the
  first column's window.
- SST growth: live SST at the end of the bench window minus at its start (`LOG` `time_micros`),
  extrapolated from 120 s to 24 h at bench load: a load-test rate, not a production rate.
  `handoff-tables.txt` divides end-of-cell live SST (including pre-bench and drain data) by the bench
  seconds instead (662 / 682 / 728 / 291).
- w's change in all writes and SST growth is not resolved; wd's +11% SST growth is (5.4 sd).
- t removes 33 MB/s of writes, 0.53 GB of data dir, 342 GB/day of SST growth and 347 GB/day of
  append-CF growth (-73%) per validator.
- **All 3 validators write to one disk on the bench host.** The write cut from wd, and possibly its
  matched/s gain, may be smaller with a disk per validator; not measured.

### 39.3 Restart (n = 3 per arm; ↓ lower is better)

Restart cells: 180 s, `SIGKILL` of val1 at bench + 60 s, restart on the same data dir with the same
env (`CRASH_KILL_AT_S=60`, the s74 crash path of `run-cell.sh`). "Ready" = restart until val0's first
commit after it. Scored on restart metrics, not matched/s.

| arm | DB open s | WAL files replayed | WAL MB replayed (on disk) | restart -> first commit s | max commit gap s |
|---|---|---|---|---|---|
| b | 1.32 (0.23) | 4.0 | 424 | 16.75 (1.89) | 17.63 |
| w | 3.99 (0.12) | 16.0 | 1,961 | 16.44 (0.65) | 17.61 |
| wd | 3.04 (0.77) | 13.7 | 153 (compressed) | 16.25 (1.27) | 16.97 |

- DB open: w +2.67 s vs b (~15 sd), wd +1.72 s (3 sd); wd vs w -0.95 s (1.7 sd, not resolved). Open
  time follows the number of WAL files more than their bytes: wd's files are ~13x smaller on disk
  and it opens almost as slowly as w.
- Restart -> first commit is not resolved (steps -0.3 / -0.5 s, sd 1.3-1.9 s): it is dominated by
  the ~13-15 s view-sync freeze, not by the DB open.
- The crash gate reads FAIL (rc 2) in all 9 cells: its "panic / fail-stop after the restart" pattern
  matches the INFO config line `running state hash fail-stop ... on=false`; no panic line exists.
  Rewind 63-66 blocks. Liveness of the restarted val1 reads UNKNOWN (not measured).
- rs-wd-r1: the restarted val1 logged one ERROR, `FIX 6 / F-1 ... crash-in-hole ... parking the
  manifest-backed heights for heal` (applied 746, manifest max 747, 1 height parked): the kill
  landed between the durable commit manifest and execution, and the existing heal path handled it.
  The cell reads AGREE; it is the only ERROR in the 75 node logs of the campaign.

### 39.4 Reading

- **Trade history off is the largest lever measured on Classic:** -11.9% node CPU (alone past the
  -10% gate), +8.2% matched/s, -54% SST growth, -40% data dir per validator, and it only turns off
  node-local trade storage (RPC / explorer). The owner's validator-default decision.
- **WAL budget 2048 MiB confirmed:** -6.3% node CPU, matched/s +2.4% (marginal), at 2x the data dir
  (up to 2 GiB WAL kept) and +2.7 s DB open on restart; restart -> first commit unchanged at n = 3.
- **wd is the best WAL arm on throughput and disk:** -4.8% node CPU, +5.2% matched/s, -74% bytes
  written per validator, data dir below b. It costs +0.45 CPU-s/1M against w, +11% SST growth, and
  +1.7 s DB open vs b. Part of the gain may come from the shared disk.
- The knobs are node-local and need no consensus change; t and w / wd stack on different paths (t
  removes the trade CF rows, w / wd change WAL and flush behaviour), but the combination was not
  measured.
- **Not measured:** a disk per validator; production-like (old chain) growth; a "caught up" signal
  after restart; the uncompressed WAL bytes wd replays. n = 6 of rs-w and rs-wd (~1 h) would
  resolve wd vs w on DB open and restart -> first commit.

## 40. Phase 3 baseline candidates: trade history off + WAL 2048, with and without WAL compression (campaign ozarchy-acc3, 2026-10-10)

Owner decisions (18c, after section 39; `docs/acc-confirm` merged as `73503705`): validators run trade
history off (`TORUS_TRADE_HISTORY=0`; RPC / explorer nodes keep it on) and WAL budget 2048 MiB. WAL
compression is decided by this run, with no more restart timing runs and no ZSTD level 1 arm (it
changed nothing in the screen, and with trade history off there is even less to compress). One
kill-restart on t + w + d as a recovery check only. The winner becomes the Phase 3 baseline.

Setup as section 39 (Classic, cap 8, 300 markets, standard shape, fresh genesis and data dir per
cell, RocksDB `LOG` copied per cell, no perf), node `2ede76eb` / bench `a33d82f5` (`fa8b646f`, stage
`ozarchy-acc-stage/n`). Arms:
- **b:** knobs unset.
- **tw:** `TORUS_TRADE_HISTORY=0` + `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048`.
- **twd:** tw + `TORUS_ROCKSDB_WAL_COMPRESSION=zstd`.

Cells 21:54-23:11: the warm-up `warm-twd-rs` (the recovery check, excluded from every number below),
then 4 rounds (b tw twd / tw twd b / twd b tw / twd tw b). All 12 measured cells rc 0, AGREE, PASS,
ACCEPT, oracle stale 0, 6 hard checks OK each, no ERROR or panic line in the 36 node logs. Node md5
`2ede76eb` in every cell; env and `LOG` settings match each arm (`max_total_wal_size` 512 / 2048 MiB,
`wal_compression` ZSTD on twd only); tw and twd hold 0 trade data rows on every validator, b ~1.9M
(positive control). Every measured cell runs `launch-3val.sh` with `CLEAN=1`, which wipes the data
dirs, so the warm-up kill cannot carry into a measured cell. Driver `ozarchy-acc3-campaign.sh`, tables
`ozarchy-acc3-run/handoff-tables.txt` (the tables script reproduces section 39.1 on the acc2 cells),
analysis scripts in `ozarchy-acc3-tools/` (archived in `tools/`).

### 40.1 Node CPU and throughput (n = 4; ↓ lower is better, ↑ higher is better)

| arm | node CPU-s/1M ↓ mean (sd) | vs b | matched/s ↑ mean (sd) | r1..r4 range | vs b |
|---|---|---|---|---|---|
| b | 28.75 (0.24) | - | 187,202 (2,171) | 185,297-189,822 | - |
| tw | **24.93** (0.07) | **-3.82, -13.3%** (21.9 sd) | 203,992 (1,144) | 202,622-205,405 | **+9.0%** (9.7 sd) |
| twd | 25.53 (0.18) | **-3.22, -11.2%** (15.2 sd) | 207,676 (4,197) | 201,877-211,919 | **+10.9%** (6.1 sd) |

- **twd vs tw, CPU:** +0.59 CPU-s/1M (+2.4%, 4.3 sd; the r1..r4 ranges do not overlap): resolved,
  the cost of compressing the WAL. Section 39 measured the same cost without trade history off
  (+0.45).
- **twd vs tw, matched/s:** +3,684 (+1.8%, 1.2 sd; paired by round t = 1.55; twd-r3 is below every
  tw cell): not resolved. Section 39's wd vs w was +2.8% at 3.1 sd, so the direction is the same.
- b reproduces section 39 (28.75 vs 28.80 CPU-s/1M; 187.2k vs 188.2k matched/s).
- Both arms clear the Phase 3 -10% gate (2.75 CPU-s/1M) against b.

### 40.2 Disk (per validator; ↓ lower is better in every column)

| arm | all writes MB/s | WAL writes MB/s | flush + compaction MB/s | data dir at end GB (SST / WAL) | SST growth GB/day | trades + DA GB/day |
|---|---|---|---|---|---|---|
| b | 168.1 | 137.1 | 40.7 | 1.33 (0.96 / 0.36) | 647 | 489 |
| tw | 131.3 (-22%) | 118.7 | 19.1 | 2.01 (0.44 / 1.57) | 322 (-50%) | 147 |
| twd | **28.0 (-83%)** | **10.2 (-93%)** | 18.9 | **0.60 (0.45 / 0.15)** | 323 | 136 |

- Columns and methods as section 39.2 (SST growth by the window method; `handoff-tables.txt` uses
  end-of-cell live SST instead: 677 / 309 / 317).
- **twd vs tw:** all writes -103 MB/s (40 sd), WAL writes -109 MB/s (45 sd), data dir -1.41 GB
  (5.1 sd): resolved. Flush + compaction, SST growth and trades + DA growth: not resolved (0.1-0.8 sd).
- tw's data dir is noisy because its WAL on disk ranges 1.04-2.00 GB across the cells; twd's WAL
  stays 0.12-0.17 GB.
- **All 3 validators write to one disk on the bench host.** twd's write cut, and any matched/s gain
  that comes from less disk contention, may be smaller with a disk per validator; not measured.

### 40.3 Recovery check (warm-twd-rs): pass

120 s cell on twd, `SIGKILL` of val1 at bench + 40 s (80 s of load left after the restart), restart on
the same data dir with the same env. No timing was scored (owner); the numbers are recorded only.
- **val1 rejoined and committed:** 1,843 post-restart `on_committed_block` lines, heights 643-2,485
  (down 1.01 s, DB open 2.09 s, 12 WAL files / 107 MB replayed, 66 blocks rewound, first commit after
  17.7 s).
- **All 3 validators agree:** `state-digest-val0/1/2.txt` identical; agreement AGREE.
- **No ERROR or panic line** in the restarted node's log.
- The crash gate reads FAIL (cell rc 2, validity REJECT, liveness UNKNOWN), for the same reason as
  section 39.3: its fail-stop pattern matches the INFO config line `running state hash fail-stop
  (TORUS_STATE_HASH_FAILSTOP) on=false`. Liveness UNKNOWN is a harness gap (val1 progress metrics
  missing after the restart), not a node fault.

### 40.4 Reading

- **tw vs twd is CPU against disk.** tw is 0.59 CPU-s/1M (2.4%) cheaper, resolved. twd writes 4.7x
  fewer bytes per validator (28 vs 131 MB/s) and keeps a 3.4x smaller, steady data dir. matched/s is
  a tie in this run (twd +1.8%, not resolved), leaning to twd across sections 39-40.
- Against b both are large: -13.3% / -11.2% node CPU, +9.0% / +10.9% matched/s, -50% SST growth.
- **The WAL compression knob is only on `perf/append-cf-compaction` (`fa8b646f`), not on main.**
  A twd baseline needs that branch merged; a tw baseline runs on main with existing knobs.
- Not measured: a disk per validator; production-like growth; matched/s of the warm-up cell. About 4
  more interleaved tw / twd pairs (~25 min) would bring the matched/s standard error near 1,500.

## 41. Phase 3 step 0 sizing on tw: coalescing factor, layer size, restart budget, CPU ceiling (campaign ozarchy-p3s, 2026-10-11)

**Owner decision needed before step 1.** The CPU ceiling as the plan defines it (flush worker +
state-row memtable flush + state-row compaction, digest excluded) is 1.55 CPU-s/1M = **6.2%** of tw
node CPU. That is above the ~5% stop rule and only 0.06 CPU-s/1M above the -6% gate (1.50). It counts
all of that work as removed. Phase 3 still writes one checkpoint per window, and the measured
coalescing factors (15 s: bytes 5.95x, keys 1.58x) scale the removable part to about **0.82 CPU-s/1M
= 3.3%**, below 5% and below the gate (41.5). Disk (-83% state-row WAL bytes) and restart (41.4) pass.

Owner request (via 18c, plan `item6-phase3-impl.md` step 0 plus three add-ons): on the owner-chosen
baseline tw (`TORUS_TRADE_HISTORY=0` + `TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=2048`; WAL compression and
append-CF codec unset), measure (1) the coalescing factor per window (1 block, 5 s, 15 s, 30 s, 100
blocks) per CF with Classic book blobs separate, and the disk-write gate; (2) the in-memory layer /
checkpoint batch size per window, capped at 100 blocks; (3) the restart budget from serial replay
speed against the <= 60 s gate; (4) the CPU ceiling on tw, with the per-block state-hash digest as its
own line.

Setup as section 40 (Classic, cap 8, 300 markets, standard shape, fresh genesis and data dir per
cell, RocksDB `LOG` copied per cell). Node `fb460a0c` / bench `af556aec` built from main `fd9e5dfa`
(stage `ozarchy-p3s-stage/n`); `fd9e5dfa` differs from the acc3 node (`fa8b646f`, `2ede76eb`) only in
`crates/torus-bridge/src/native_executor.rs`. Cells 00:06-01:08 (driver `ozarchy-p3s-campaign.sh`,
tables `ozarchy-p3s-run/handoff-tables.txt`, analysis scripts `ozarchy-p3s-tools/a-*.py` and
`waldec/waldec.c`, outputs `ozarchy-p3s-run/analyst/`):
- **tw-r1, tw-r2:** 120 s, no perf; tw-r1 keeps val0's WAL (118 files hard-linked as they appear,
  `wal-val0-keep/`, 14.63 GB, no file missed).
- **tw-old1:** same-day control on the acc3 node `2ede76eb` (bench `a33d82f5`), 120 s.
- **tw-c1..c3:** 420 s, `SIGKILL` of val1 at bench + 60 / 180 / 300 s, restart on the same data dir
  (9 restarts).
- **tw-p1, tw-p2:** perf (`cycles:u` 499 Hz, val0 whole process, 45 s from bench + 35 s).

All 8 cells AGREE (block hash and state digest equal on all 3 validators), 0 ERROR / panic lines,
node md5 as staged in every cell, 6-7 hard checks OK each, knobs and `max_total_wal_size` 2048 MiB
verified on every validator, 0 trade data rows. The 5 throughput / perf cells are rc 0, PASS, ACCEPT.
The 3 crash cells end rc 2 / INVALID / REJECT: the crash gate's fail-stop pattern matches the INFO
config line `running state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false` once per restart (the
only match; sections 39.3 / 40.3), so this is not a node fault. tw-c2 kill 3 also reads "rewind 9 blocks
beyond the exec queue (max 2)": 41 blocks rewound against an exec queue of 32 sampled just before the
kill. That is a harness heuristic; the digests agree. No cell is excluded.

Throughput and CPU match the acc3 reference: node CPU-s/1M (val0-2 `proc_cpu`, whole run, as
section 40) tw-r1 / tw-r2 24.90 / 24.94, tw-old1 24.84, acc3 tw 24.93 (sd 0.07); matched/s 202,804
/ 204,384, tw-old1 206,283, acc3 tw 203,992 (sd 1,144). The `fd9e5dfa` vs `2ede76eb` difference is
not resolved on either metric. **The -6% gate is taken against section 40's tw 24.93 (-> 23.43, a
step of 1.50)**; the same-day control (24.84 -> 1.49) gives the same answer.

### 41.1 Coalescing factor (tw-r1, val0 WAL)

Method. A RocksDB WAL decoder (`waldec.c`, C; `ldb` is not installed) read all 118 kept WAL files:
237,376 WAL records, 17,680,190 entries, 0 CRC errors, every record's entry count equal to its header,
sequence numbers continuous from 1 to 17,680,190 across all files (no file or record missing). Each
block's state batch carries `cf_consensus_meta/native_applied_height` = h. 2,518 records carry it,
heights 1-2,518 consecutive, one per `execution pipeline: block done` line in `val0.log.gz`. Other
records (bodies, headers, commit manifest, DA `cf_native_pending`, hotstuff meta) are assigned to the
last state batch before them. Windows run on execution time (`block done` timestamps), tumbling from
`t_bench0`, complete windows of the load window only (heights 374-1,143, 770 blocks). The "plan"
windows follow the plan's trigger: checkpoint at the end of block n when the window's execution time
reaches the interval or n % 100 == 0, i.e. min(window, 100 blocks). Bytes = key + value, a delete
counts its key; last-value = the last write of each distinct (CF, key) in the window; factor =
written / last-value. Phase 3 coalesces state rows only, so the factors below are for the state
batches (65% of val0's WAL bytes in the load window: 9,130 of 14,011 MB).

| window | windows | blocks / window | state keys written -> distinct (factor) | state bytes written -> last (factor) | book blobs | positions | balances | oracle | nonces | state excl. books | all WAL bytes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 block | 770 | 1 | 16.11M -> 16.11M (1.00) | 9,130 -> 9,130 MB (1.00) | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 | 1.02 |
| 5 s | 24 | 31.5 | 15.84M -> 12.15M (1.30) | 8,995 -> 2,038 MB (4.41) | 18.3 | 1.23 | 4.95 | 6.32 | 1.00 | 1.25 | 2.39 |
| 15 s | 8 | 94.6 | 15.84M -> 10.06M (1.57) | 8,995 -> 1,563 MB (**5.75**) | 37.2 | 1.48 | 9.13 | 19.0 | 1.00 | 1.51 | 2.71 |
| 30 s | 4 | 189.2 | 15.84M -> 6.04M (2.62) | 8,995 -> 905 MB (9.93) | 83.5 | 2.48 | 18.0 | 37.9 | 1.00 | 2.53 | 3.11 |
| 100 blocks (h / 100) | 7 | 100 | 13.74M -> 8.38M (1.64) | 8,002 -> 1,255 MB (6.38) | 56.9 | 1.54 | 9.04 | 20.0 | 1.00 | 1.57 | 2.86 |
| plan 15 s = min(15 s, 100 blocks) | 9 | 77.8 | 13.69M -> 8.65M (1.58) | 7,966 -> 1,338 MB (**5.95**) | 39.7 | 1.49 | 8.09 | 19.1 | 1.00 | 1.52 | 2.79 |
| plan 30 s = min(30 s, 100 blocks) | 7 | 100 | 13.69M -> 8.36M (1.64) | 7,966 -> 1,256 MB (**6.34**) | 54.7 | 1.54 | 9.02 | 19.9 | 1.00 | 1.57 | 2.85 |

- **Book blobs carry the byte factor.** `cf_native_order_books` (Classic, one ~44 KB blob per market,
  ~199 written per block) is 77% of the state bytes (7,016 of 9,130 MB) and coalesces 37-83x; at most
  300 distinct keys per window.
- **Positions carry the key count.** `cf_native_positions` (139 B per entry) is 92% of the state
  entries (14.8M of 16.1M) and 23% of the bytes, and coalesces only 1.48x at 15 s and 1.54x at 100
  blocks. So the state key factor stays at 1.57-1.64.
- Nonces never coalesce (a new key per action). Balances 8-9x and oracle ~19x at 15 s / 100 blocks.
  Markets, liquidation and meta coalesce 10-100x but are < 0.2 MB per window.
- **The 100-block cap binds at 30 s.** At tw's 6.33 native blocks/s (tw-r1 / tw-r2), 15 s is ~95
  blocks, so the cap barely changes 15 s (5.75 -> 5.95). 30 s is ~190 blocks, so the capped 30 s
  window is the 100-block window (6.34, vs 9.93 uncapped). The plan's "~23 s per 100 blocks" assumed
  4.3 blocks/s; on tw 100 blocks is ~15.8 s.
- **Disk-write gate** (plan step 4: state-row bytes written down by the factor): val0 state-row WAL
  writes 74.8 MB/s in the load window (9,130 MB / 122 s) -> 12.6 MB/s at plan 15 s (/ 5.95), 11.8 MB/s
  at plan 30 s (/ 6.34), a cut of 83-84%. All WAL writes 115.4 MB/s -> 53.2 / 52.4 MB/s (-54% /
  -55%). Bodies, DA (`cf_native_pending`, 3,085 MB, 1.7x within a window) and headers are not in
  Phase 3. Memtable flush and compaction of the state CFs fall too (41.5).
- One cell, one validator, one load shape. The state batches are consensus writes, identical on every
  validator. RocksDB's group commit merged a few small non-state records into state records (0.3 MB of
  `cf_native_pending` counted as state, 0.003%).

### 41.2 In-memory layer and checkpoint batch (state last-value bytes per window, tw-r1 val0)

| window | peak MB | mean MB | min MB | peak distinct keys | written per window, peak MB |
|---|---|---|---|---|---|
| 1 block | 51.0 | 11.9 | 0.0 | 92k | 51.0 |
| 5 s | 125.6 | 84.9 | 54.5 | 617k | 574 |
| 15 s | **216.5** | 195.4 | 177.3 | 1.36M | 1,342 |
| 30 s (uncapped) | **235.1** | 226.4 | 217.9 | 1.52M | 2,619 |
| 100 blocks | **218.3** | 179.2 | 138.6 | 1.38M | 1,441 |
| plan 15 s = min(15 s, 100 blocks) | **213.9** | 148.7 | 44.2 | 1.37M | 1,332 |
| plan 30 s = min(30 s, 100 blocks) | **219.4** | 179.4 | 138.6 | 1.39M | 1,468 |

- Layer L and one checkpoint batch are ~214-219 MB of key + value at the peak with the 100-block
  cap; positions are ~185 MB of it, book blobs up to ~41 MB. Uncapped 30 s adds only ~16 MB, because
  the set of touched keys saturates.
- These are payload bytes. Per-entry map and allocation overhead for ~1.4M keys is not in them.
  **(estimate)** ~100-150 B per key adds ~140-210 MB per layer, and C (one checkpoint in flight) can
  hold a second copy. Size the plan's RAM gate ("layer-size figure + 25%") against the step 4 RSS,
  not against 214 MB alone.

### 41.3 Restart today on tw (9 kills of val1, n = 3 cells)

Markers from val1's restart tails (`crash-restart-tail{,-2,-3}.log`), times from `crash-kill*.json`.
DB open = `loaded node key` -> `state database opened`. Replay = WARN `execution gap detected,
replaying ... gap=G` -> INFO `exec pipeline ENABLED ... after replay applied=H`. **The replay end is an
inference** (no replay-complete line in `fd9e5dfa`). It is backed in every kill: `applied=` equals the
committed height, every replayed height has a `block done` line with `pipelined=false` (serial), and the
last one comes 0-6 ms before the ENABLED line. First commit = first `on_committed_block: sending to
execution pipeline` (height H + 1).

| cell | kill | committed / applied / gap | DB open s | replay s | blocks/s | 1st replayed block s | restart -> first commit s |
|---|---|---|---|---|---|---|---|
| tw-c1 | 1 | 764 / 698 / 66 | 2.38 | 8.51 | 7.76 | 2.45 | 11.87 |
| tw-c1 | 2 | 1748 / 1687 / 61 | 2.08 | 12.46 | 4.89 | 2.38 | 14.64 |
| tw-c1 | 3 | 2580 / 2521 / 59 | 2.26 | 10.47 | 5.63 | 2.52 | 12.82 |
| tw-c2 | 1 | 772 / 707 / 65 | 1.82 | 10.54 | 6.17 | 2.31 | 12.50 |
| tw-c2 | 2 | 1703 / 1639 / 64 | 3.62 | 11.80 | 5.42 | 2.35 | 15.52 |
| tw-c2 | 3 | 2499 / 2458 / 41 | 2.52 | 11.13 | 3.68 | 2.21 | 13.75 |
| tw-c3 | 1 | 808 / 744 / 64 | 2.11 | 11.05 | 5.79 | 2.50 | 13.29 |
| tw-c3 | 2 | 1759 / 1720 / 39 | 3.67 | 11.22 | 3.47 | 2.59 | 16.44 |
| tw-c3 | 3 | 2531 / 2503 / 28 | 3.68 | 7.10 | 3.94 | 2.03 | 10.91 |
| **mean (sd)** | | gap 54.1 | **2.68** (0.76) | 10.48 (1.67) | 5.20 (1.37) | 2.37 | **13.53** (1.77) |

- **Serial replay speed:** pooled 487 blocks in 94.29 s = **5.16 blocks/s** (194 ms per block). The
  first replayed block is cold (2.03-2.59 s, mean 2.37 s), so short gaps replay slower. Without it,
  the pooled steady rate is 6.55 blocks/s (slowest kill 4.40). Replayed blocks average 174 native
  actions (84,613 / 487) vs 135 transactions per block under load (tw-r1 `txs_per_block_avg`), so this
  rate is conservative for average blocks.
- Restart -> replay start (process start + DB open + 0.08 s) is 2.77 s mean (max 3.77). Replay end ->
  first commit is 0.28 s mean (max 1.46). Today's 13.5 s restart -> first commit is mostly the replay
  of the exec queue (28-66 blocks). The harness's `crash-freeze.json` (from val0's log) reads 0.2 s
  lower for kill 1 (11.66 vs 11.87 s in tw-c1).

### 41.4 Restart budget per interval (vs restart -> ready <= 60 s)

Budget = restart -> replay start + replay of the window's blocks at the serial speed above + replay
end -> first commit. Blocks per window from tw's native rate (6.33 blocks/s). Central = pooled 5.16
blocks/s and mean overheads. Pessimistic = max overheads + the slowest cold first block + the slowest
steady rate (4.40 blocks/s). After a Phase 3 crash the replay is (S, committed]: the window since the
last checkpoint **plus** the exec-queue lag that is replayed today (up to 66 blocks here), so both rows
are shown.

| window | blocks replayed | replay at 5.16 blocks/s | total central | total pessimistic | gate 60 s |
|---|---|---|---|---|---|
| 15 s | 95 | 18.4 s | **21.4 s** | 29.2 s | pass |
| 15 s + 66 exec lag | 161 | 31.2 s | 34.2 s | 44.1 s | pass |
| min(30 s, 100 blocks) = 100 blocks | 100 | 19.4 s | **22.4 s** | 30.3 s | pass |
| 100 blocks + 66 exec lag | 166 | 32.1 s | 35.2 s | 45.3 s | pass |
| 30 s uncapped | 190 | 36.8 s | 39.8 s | 50.7 s | pass |
| 30 s uncapped + 66 exec lag | 256 | 49.6 s | 52.6 s | **65.7 s** | pass central, over pessimistic |

- With the plan's 100-block cap every interval passes, even in the pessimistic case (<= 45 s). Only an
  uncapped 30 s plus a full exec queue at the slowest replay rate goes over 60 s.
- DB open may get shorter with Phase 3 (less WAL to recover: 41.1). Not measured.

### 41.5 CPU ceiling on tw

Method as section 37 (p3s0cf), on val0 over `[t_bench0, t_drain]`, CPU-s per 1M val0 fills.
Flush worker = `torus-flush-wor` thread (`tasks.txt`). State-row memtable flush = `rocksdb:high` x the
state CFs' share of flush wall time (RocksDB `LOG` flush jobs). State-row compaction = the state CFs'
`compaction_time_cpu_micros` (LOG compaction jobs; the LOG total matches the `rocksdb:low` thread
within ~1% in every cell). The acc3 tw cells hold no perf data, so tw-p1 / tw-p2 were run. Their thread CPU for
these threads matches the no-perf cells, and perf is used for shares only.

| cells | flush worker | state memtable flush | state compaction | **ceiling** | % of 24.93 | factor-scaled (plan 15 s) | % |
|---|---|---|---|---|---|---|---|
| fd9e5dfa tw-r1, tw-r2 | 0.939 | 0.282 | 0.334 | **1.556** | 6.24% | 0.818 | 3.28% |
| fd9e5dfa incl. tw-p1, tw-p2 (n = 4) | 0.929 (sd 0.016) | 0.285 | 0.339 | **1.553** (sd 0.026) | 6.23% | 0.818 | 3.28% |
| acc3 tw-r1..r4 (`2ede76eb`) | 0.956 | 0.279 | 0.337 | 1.572 (sd 0.047) | 6.30% | 0.826 | 3.31% |
| tw-old1 (`2ede76eb`, same day) | 0.918 | 0.258 | 0.352 | 1.528 | 6.13% | 0.787 | 3.16% |

- **Digest line (block_digest / chain_step running hash): 0.00 CPU-s/1M measured.** The bench genesis
  does not activate the running state hash (`running state hash activation (chain config)
  activation=None` in every node log; `torus_state_hash_seconds_sum` 0.0 over 1,099 flushes; no
  `block_digest` / `Sha256` sample in either perf cell). So the digest is not inside the ceiling, and
  removable = ceiling - 0 = **1.55 CPU-s/1M = 6.2%** of tw node CPU. On a chain with the hash active,
  **(estimate)** SHA-256 over the hashed state writes (~9.3 GB per 24.7M fills in tw-r1 at 1.67 GB/s on
  this host, plus per-entry framing) costs ~0.26 CPU-s/1M (~1%). It is added on W, Phase 3 keeps it,
  and the removable part is unchanged (6.1% of a 25.2 node).
- **What the flush worker does** (perf, tw-p1 / tw-p2): 76-77% RocksDB write (WAL CRC32C 37-38%,
  memtable insert 35-36%), 23% batch build (`append_to_batch`). About 46% of it scales with bytes (CRC,
  string append), the rest with entries.
- **Factor-scaled estimate** (what Phase 3 can remove when it still writes the coalesced
  checkpoint): flush worker x (1 - (0.46 / byte factor + 0.54 / key factor)) + per-CF state flush and
  compaction x (1 - 1 / CF byte factor). Plan 15 s: 0.54 + 0.13 + 0.14 = **0.82 CPU-s/1M (3.3%)**.
  100 blocks / plan 30 s 0.85 (3.4%); uncapped 30 s 1.11 (4.5%). Positions set the limit: 92% of
  the entries coalesce only ~1.5x. Not counted (each lowers the saving further): E-side merging into L
  and the bounded W channel. Not measured: whether memtable flush and compaction of positions shrink
  by the full factor (RocksDB already drops overwritten versions within a memtable).
- **Against the -6% gate (1.50 CPU-s/1M):** the ceiling clears it by 0.06 (4% margin), with all of
  the work removed. The factor-scaled estimate (0.82) is about half the gate. The step 0 estimate
  before tw (Classic, 1.76) is lower on tw (1.55) because flush worker and state compaction did not
  change while node CPU fell 13%.

### 41.6 Reading

- **Coalescing:** state bytes 5.95x at the plan's 15 s, 6.34x at 30 s capped (= 100 blocks), 9.93x
  only without the cap. Keys 1.58-1.64x. Disk gate: state-row WAL writes -83%, all WAL writes -54%.
- **Layer / checkpoint:** ~214 MB (15 s) and ~219 MB (30 s capped / 100 blocks) payload at the peak,
  plus per-key overhead.
- **Restart:** serial replay 5.16 blocks/s; DB open 2.7 s; restart -> first commit today 13.5 s. The
  budget is 21-22 s central and <= 45 s pessimistic for every capped interval, so the 60 s gate passes.
- **CPU:** ceiling 1.55 CPU-s/1M (6.2%, digest 0 in bench) is just above the -6% gate (1.50); factor-
  scaled 0.82 (3.3%) is not. **Owner decision needed before step 1** (plan stop rule options: build
  for the disk and W-ceiling gains with a lower CPU gate, or drop Phase 3).
- Confidence: coalescing and layer size high for this shape (exact decode, validated; one cell);
  restart medium (n = 9, extrapolated from 28-66 to 95-190 blocks); CPU ceiling high (9 cells,
  1.53-1.62); factor-scaled estimate low-medium (model, not a measurement).
- Shared-disk caveat as section 40. Perf inflates val0's CPU (tw-p1 / tw-p2 val0 28.9 / 28.3 vs
  24.1); only thread CPU of the flush / RocksDB threads and perf shares are used from them.

## 42. Phase 3 checkpoint cost, measured with a throwaway prototype: all rows vs book blobs vs control (campaign ozarchy-p3p, 2026-10-11)

**Owner rule outcome: shelve Phase 3 and keep the disk-write numbers on file. This is a borderline
call.** The all-rows prototype cuts node CPU-s/1M by **4.90%** vs control (a vs c, n = 4, 95% CI
about 2.4-7.4%) and by **5.07%** with the running state hash on (aS vs cS, CI about 4.0-6.1%). Both
are well clear of zero. Neither is clear of the 5% line. Node CPU per 120 s cell is the same in every
arm (655-661 CPU-s per validator), so most of the per-fill saving comes from **+4.7% matched/s**, which
spreads the same CPU over more fills. At equal fills the all-rows saving is **2.1-2.5%** (42.2).
That is the same order as section 41's model (3.3%), a little below it. Book blobs alone save 2.98% (1.0% at equal
fills), so the books-only branch of the rule does not apply either. Disk: bytes written per
validator **-51%** (all) / **-45%** (books).

Owner request (18c s111, after section 41): measure the checkpoint cost with a throwaway prototype
before step 1. Arm a (all state rows, one batch per 100 blocks), arm k (Classic book blobs only), and
the same pair with the running state hash active, to measure the digest. Rule: all rows >= 5% node
CPU vs control and clearing the noise -> start step 1; else if only books gets close -> re-scope Phase
3 to book blobs; else shelve Phase 3 and keep the disk-write numbers.

Setup as section 41 (tw baseline: trade history off + WAL 2048 MiB; Classic, cap 8, 300 markets,
standard shape, fresh genesis and data dir per cell, RocksDB `LOG` copied per cell). **One node binary
for every arm:** `proto/p3-coalesce` `682a8df5` (worktree `wt/p3proto`, off main `de6768cc`, not for
merge), node `6e40fa3c`, bench `19f7ac94`, harness `9b7e29b2` (`crash-kill.sh` copy patched only for
`KILL_SIG`). The prototype defers the state CFs into an in-memory layer inside `StateDb` (reads merge
it) and writes the layer as one `WriteBatch` on a `p3-ckpt` thread at every height divisible by 100.
`all` also defers META (marker + running hash), so a crash rewinds to the last checkpoint and replays
from the bodies. `books` defers only `cf_native_order_books` and records its own checkpoint marker. Arms:
- **c:** `TORUS_P3_PROTO` unset (control). **a:** `TORUS_P3_PROTO=all`. **k:** `TORUS_P3_PROTO=books`.
- **cS / aS:** c / a with `STATE_HASH_ACTIVATION=1` in genesis (the running state hash active from
  height 1; section 41 had it off).

Cells 03:00-06:00 (driver `ozarchy-p3p-campaign.sh`, tables `ozarchy-p3p-run/handoff-tables.txt`,
analyst scripts `tools/ozarchy-p3p-tools/a-*.py`): warm-up w0 (60 s, excluded), then 4 rotated rounds
of 120 s cells (c a k cS aS / a k cS aS c / k cS aS c a / cS aS c a k), then the graceful restart
cells g1-c g1-a g1-k g2-k g2-a g2-c (120 s, `SIGTERM` to val1 at bench + 60 s) and the multi-crash
cells x1-c x1-a (420 s, `SIGKILL` to val1 at bench + 60 / 180 / 300 s).

All 29 cells AGREE. Node md5 `6e40fa3c` on all 3 validators in every cell. Every cell passed its hard
checks (9 per throughput cell, 11 per restart cell: p3 knob and layer-enable lines per arm, genesis
`state_hash_activation` per arm, `max_total_wal_size` 2048 MiB, 0 trade rows, codec, book CF, trie).
The host had no `cargo`, `rustc` or `rust-analyzer` running before any cell. The 21 throughput cells
(w0 + r1-r4) are rc 0, PASS, ACCEPT. The 8 restart cells are rc 2 / INVALID / REJECT, and none is a node
fault: in each restart the only panic / fail-stop match is the INFO config line `running state hash
fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false` (sections 39.3 / 40.3 / 41). x1-a also reads "rewind 58
/ 66 blocks beyond the exec queue (max 2)". That is the all-arm design (rewind to the last checkpoint),
not a fault. No cell is excluded from the analysis.

### 42.1 Node CPU and throughput (r1-r4, n = 4 per arm; ↓ lower is better, ↑ higher is better)

Node CPU-s/1M = val0-2 `proc_cpu` whole run / fills, mean of the 3 validators (sections 40 / 41).

| arm | node CPU-s/1M ↓ mean (sd) | r1..r4 | vs control | step / pooled sd (paired t) | matched/s ↑ mean (sd) | vs control |
|---|---|---|---|---|---|---|
| c | 24.996 (0.412) | 25.45 / 24.82 / 24.52 / 25.20 | - | - | 204,486 (6,455) | - |
| a | **23.770** (0.115) | 23.90 / 23.68 / 23.67 / 23.83 | **-1.226, -4.90%** | 4.1 (8.1) | **214,120** (517) | **+4.71%** (2.1 sd; every a cell above every c cell) |
| k | 24.251 (0.071) | 24.26 / 24.33 / 24.16 / 24.26 | -0.745, -2.98% | 2.5 (3.8) | 210,382 (1,775) | +2.88% (1.2 sd, not resolved) |
| cS | 25.335 (0.173) | 25.25 / 25.48 / 25.13 / 25.47 | (vs c) +0.339, +1.36% | 1.1 (1.7) | 200,994 (3,490) | (vs c) -1.71% (0.7 sd) |
| aS | **24.049** (0.104) | 24.07 / 24.19 / 23.95 / 24.00 | **(vs cS) -1.285, -5.07%** | 9.0 (18.6) | **213,652** (2,346) | **(vs cS) +6.30%** (4.3 sd) |

- c reproduces section 40's tw (24.93 CPU-s/1M, 203,992 matched/s) within its spread. c is the noisiest
  arm (r1-c 25.45 / 196,916 is its slowest cell).
- **Against the 5% line.** a vs c: -4.90%, Welch t 5.7 vs zero; the 95% interval (df ~3.4) is about
  -2.4% to -7.4%. aS vs cS: -5.07%, t 12.7; interval about -4.0% to -6.1%. Pooled over both pairs:
  -4.99%. The saving is resolved against zero, but **not** against the 5% line.
- Load-window CPU (`proc_cpu` load, bench window only) reads a little higher: a vs c -5.56%, k vs c
  -3.03%, aS vs cS -6.05%. Sections 40 / 41 use the whole run, so the whole run is the rule's figure.

### 42.2 Where the saving comes from (load window; CPU-s per 1M val0 fills, per validator)

Per-thread CPU from `io-threads-start/end.txt` (utime + stime, bench start -> bench exit, mean of
val0-2). "Exited threads" = process load CPU minus the live threads. It holds the `p3-ckpt` threads
(one per checkpoint) and the worker-pool threads.

| arm | flush worker | `rocksdb:high` | `rocksdb:low` | exec thread | other live threads | exited threads | total (load) |
|---|---|---|---|---|---|---|---|
| c | 0.931 | 0.449 | 1.030 | 3.961 | 11.814 | 7.574 | 25.758 |
| a | 0.478 | 0.287 | 0.842 | 3.820 | 11.238 | 7.660 | 24.326 |
| k | 0.716 | 0.354 | 0.951 | 3.888 | 11.555 | 7.512 | 24.977 |
| cS | 0.952 | 0.456 | 1.032 | 3.961 | 11.652 | 8.207 | 26.261 |
| aS | 0.498 | 0.303 | 0.818 | 3.812 | 11.198 | 8.043 | 24.672 |

- **Work Phase 3 removes** (flush worker + RocksDB memtable flush + compaction), a vs c: -0.453 -
  0.162 - 0.188 = **-0.80 CPU-s/1M (3.1%)**; aS vs cS -0.82. The flush worker keeps ~0.48: the
  prototype's per-block absorb into the layer (42.4) runs there. The checkpoint thread is not in
  these columns. Its build + write wall time is 7.3 s per validator per cell (42.4), so at most ~0.27
  CPU-s/1M. Net removed work: about **0.53-0.80 (2.1-3.1%)**. Section 41's model: 0.82 (3.3%), with a
  ceiling of 1.55. About half the ceiling goes.
- **The rest is throughput.** The exec thread (-0.14) and the other live threads (-0.58) do the same
  work per second, spread over 4.7% more fills. Node CPU per cell is flat across the arms: c 656.2
  (sd 5.6), a 655.3, k 658.7, cS 661.3, aS 656.8 CPU-s per validator. A common-slope fit over the 20
  cells (CPU-s per validator vs fills, arm intercepts) gives a marginal cost of 10.1 ± 2.1 CPU-s per
  1M extra fills (the average is ~24-25). At equal fills: **a vs c -0.51 ± 0.12 CPU-s/1M (-2.1%), k vs
  c -0.24 ± 0.10 (-1.0%), aS vs cS -0.61 ± 0.11 (-2.5%), cS vs c +0.26**.
- **Why matched/s rises** (a / k vs c): the harness's `pipelined_ms` per block 29.6 -> 17.7 / 21.2
  (4.2 / 2.9 sd), native blocks/s 6.05 -> 6.42 / 6.53, commit interval 153.5 -> 145.9 / 142.7 ms. The
  flush worker writes 78 MB/s less to storage (42.3), and **all 3 validators share one disk on the
  bench host** (section 40.2). With a disk per validator the throughput gain, and the CPU-s/1M saving
  that rides on it, may be smaller. Not measured.

### 42.3 Disk (per validator, bench window; ↓ lower is better)

All writes = process `write_bytes` from `io-ticks.txt` over `[t_bench0, t_bench1]`. WAL = process
writes minus the `rocksdb:*` threads (from `io-threads-*.txt`). Flush + compaction = the `rocksdb:*`
threads. These are the methods of section 40.2. They count total bytes written, not the
end-of-cell `data-du.txt` snapshot.

| arm | all writes MB/s (sd) | total written in the window GB | WAL MB/s | flush + compaction MB/s | flush worker MB/s | SST created per cell GB | peak RSS per node MB |
|---|---|---|---|---|---|---|---|
| c | 131.0 (7.0) | 16.0 | 115.8 | 18.7 | 78.4 | 2.21 | 3,517 |
| a | **64.5** (0.6), **-50.8%** | 7.9 | **51.4, -55.6%** | 14.6, -21.8% | 0 | 1.81 | **4,331 (+814)** |
| k | 72.4 (1.0), -44.8% | 8.9 | 57.4, -50.4% | 16.1, -14.1% | 18.7 | 1.93 | 3,563 |
| cS | 128.7 (2.5) | 15.8 | 114.8 | 18.6 | 77.9 | 2.19 | 3,515 |
| aS | 63.5 (1.0), -50.7% vs cS | 7.8 | 50.7, -55.8% | 14.5, -21.9% | 0 | 1.77 | 4,429 |

- Every disk step is resolved (8-35 sd). Section 41.1 predicted all WAL writes -54% at plan 15 s
  from the WAL decode. The prototype measures -55.6% at 100 blocks.
- **Book blobs carry most of the bytes.** k gets 88% of a's cut in all writes (58.7 of 66.5 MB/s)
  and 91% of its WAL cut, for 0.11 ms per block of absorb (42.4).
- **RAM:** a holds ~0.8 GB more RSS per node (+23%): the active layer plus the flushing copy while a
  checkpoint is written. That is section 41.2's estimate (214-219 MB payload + ~140-210 MB per-key
  overhead per layer, up to two layers). k: +46 MB.

### 42.4 Prototype stalls (`p3proto checkpoint` lines, val0-2 logs, load-window checkpoints of 100 blocks)

| arm | checkpoints | rows min / median / max | MB min / median / max | build ms (median, max) | write ms (median, max) | absorb ms per block (median, max) | build + write per validator per cell |
|---|---|---|---|---|---|---|---|
| a | 111 | 0.20M / 1.22M / 1.42M | 43 / 188 / 233 | 262, 379 | 552, 692 | 9.5, 13.4 | 7.3 s |
| aS | 108 | 0.34M / 1.30M / 1.40M | 58 / 192 / 211 | 261, 387 | 588, 1,289 | 9.9, 13.0 | 7.5 s |
| k | 114 | 300 / 300 / 300 | 1.1 / 13.0 / 38.0 | 6.0, 26.5 | 17.1, 74.1 | 0.11, 0.18 | 0.29 s |

- Build and write run on the `p3-ckpt` thread. Absorb runs on the flush worker, under the per-CF
  write lock of the layer (all: ~9.5 ms per block; books: ~0.1 ms). The smoke test's figures (build
  0.1-0.55 s, write 0.3-0.74 s, 120-230 MB, absorb 4-11 ms) hold under load, with one aS write at 1.29 s.
- **Bias.** The absorb CPU is in the flush worker column and the checkpoint CPU is in the process
  total, so both are in the measured CPU; neither favours the prototype. They do not cost throughput
  either: a is the fastest arm. The exec -> flush handoff wait is higher in a (1.31 vs 0.76 ms per
  block, 1.5 sd) and aS (2.35 vs 1.40, 1.2 sd); neither step is resolved. What the prototype leaves out
  (task 1.1 pruner clamp, h(n-1) in memory on W, an interval setting) is small next to the 0.8 CPU-s/1M
  moved here.

### 42.5 Digest cost (running state hash on, vs off)

- **cS - c: +0.339 CPU-s/1M (+1.36%; 1.1 sd, paired t 1.7; not resolved alone).** aS - a: +0.279
  (+1.18%; 2.6 sd, paired t 3.5). Both pairs point the same way: **~0.3 CPU-s/1M (~1.2%)**. This
  **replaces section 41's ~0.26 estimate** (consistent with it). At equal fills, cS - c is +0.26.
- The cost lands mostly in exited (worker) threads (+0.63 cS vs c, +0.38 aS vs a), and it raises
  the exec -> flush handoff wait (0.76 -> 1.40 ms per block, 3.4 sd).
- Phase 3 keeps the digest. Against an aS-like chain the all-rows saving is the same size (-5.07% vs
  -4.90%).

### 42.6 Restart (val1; markers from `crash-restart-tail{,-2,-3}.log` as section 41.3; times from `crash-kill*.json`)

| cell | signal | down s | DB open s | rewind / replay | replay s (blocks/s) | restart -> first commit s | kill -> first commit s |
|---|---|---|---|---|---|---|---|
| g1-c | TERM (= kill, see below) | 1.01 | 2.59 | 64 blocks (681 -> 745) | 10.07 (6.4) | 12.75 | 13.8 |
| g2-c | TERM (= kill) | 1.01 | 2.54 | 64 (686 -> 750) | 9.77 (6.6) | 12.46 | 13.5 |
| g1-a | TERM, graceful | 8.03 | 3.97 | 0 | - | 20.74 | 28.8 |
| g2-a | TERM, graceful | 8.03 | 4.53 | 0 | - | 10.76 | 18.8 |
| g1-k | TERM, graceful | 8.03 | 3.52 | 0 | - | 13.35 | 21.4 |
| g2-k | TERM, graceful | 8.03 | 4.01 | 0 | - | 11.13 | 19.2 |
| x1-c kill 1 / 2 / 3 | KILL | 1.01 | 2.48 / 3.54 / 2.94 | 64 / 52 / 44 | 9.47 / 7.68 / 9.77 (6.8 / 6.8 / 4.5) | 12.08 / 11.35 / 12.82 (mean 12.1) | 13.1 / 12.4 / 13.8 |
| x1-a kill 1 / 2 / 3 | KILL | 1.01 | 4.28 / 2.80 / 5.29 | **120 / 130 / 34** (700 -> 820, 1500 -> 1630, 2300 -> 2334) | 21.59 / 21.88 / 7.12 (5.6 / 5.9 / 4.8) | **26.57 / 24.83 / 12.50 (mean 21.3)** | 27.6 / 25.8 / 13.5 |

- **Control `SIGTERM` is a kill.** `torus-node` waits for SIGTERM only when the p3 layer is on, so
  g*-c die at once and replay the 64-block exec queue, the same as `SIGKILL`.
- **Graceful a / k:** the shutdown flush itself takes 1.04-1.09 s (all) / 0.06-0.08 s (books). Then the
  node keeps executing its exec queue (64 blocks, written per block with the layer closed) before it
  exits (g1-a: applied 666 at the signal, 730 at exit). That is why down is 8 s and no block is
  replayed. The 3-validator devnet commits nothing while val1 is down (g1-a: val0's last commit before
  the stop, height 730, is at 03:12:24 UTC, the next, 731, at 03:12:52), so the time to the first commit is
  the chain's stall. Restart -> first commit 10.8-20.7 s (a) and 11.1-13.4 s (k); kill -> first
  commit 18.8-28.8 s vs 13.5-13.8 s for the control's kill path. The graceful drain moves the queue's
  execution in front of the exit, so it lengthens the outage instead of shortening it.
- **SIGKILL, all rows:** the replay is the blocks since the last checkpoint plus the exec queue (58 +
  62, 66 + 64, 1 + 33 blocks). It runs at 4.8-5.9 blocks/s, including a checkpoint written during
  the replay of kills 1 and 2. Restart -> first commit is 21.3 s mean, 26.6 s max, vs 12.1 s for
  control. That is inside section 41.4's budget (100 blocks + 66 exec lag: 35.2 s central, 45.3 s
  pessimistic) and the 60 s gate.
- DB open is ~1.3 s longer with the layer on (a 4.17 s mean of 5, k 3.77 of 2, c 2.82 of 5;
  section 41.3: 2.68). This is not explained here; a possible cause is RocksDB recovering the
  200 MB checkpoint batch from the WAL.
- The harness's `restart-metrics.json` agrees within 0.1-0.4 s (it times the first commit from the
  harness's own markers).

### 42.7 Reading

- **Rule:** all rows -4.90% / -5.07% node CPU-s/1M (pooled -4.99%). This clears the noise against
  zero but sits on the 5% line, so the first branch ("≥ 5% and clears the noise") is not met. At equal
  throughput the saving is 2.1-2.5%, a little below section 41's model (3.3%). Books only: -2.98% (-1.0% at
  equal fills). It is below all rows, not close to 5%. **-> Shelve Phase 3; keep the disk-write
  numbers** (all rows: bytes written per validator -51%, WAL -56%, flush + compaction -22%, ~0.8 GB
  more RAM per node; books only: -45% / -50% / -14% for +46 MB). The owner may read the pooled
  4.99% differently. The numbers above are the inputs for that call.
- **Throughput:** all rows +4.7% matched/s (aS vs cS +6.3%), resolved. Its mechanism (less flush
  worker work and fewer bytes written to a disk the 3 validators share) is plausible, but this host
  cannot separate it from shared-disk contention.
- **Digest:** ~0.3 CPU-s/1M (~1.2%) measured, replacing the 0.26 estimate.
- **Restart:** SIGKILL with all rows 21.3 s mean / 26.6 s max to the first commit (control 12.1 s),
  within the 60 s gate. A graceful stop replays nothing, but it drains the exec queue before the exit,
  so the outage is longer (kill -> first commit 18.8-28.8 s vs 13.5-13.8 s).
- **Confidence:** medium for the verdict. The 5% line falls inside both CIs; the equal-fills
  analysis rests on a common slope fitted from small within-arm spreads (10.1 ± 2.1); the throughput
  gain may be host-specific. High for the disk and stall numbers (8-35 sd, every cell). Medium for
  restart (n = 3 + 2 per arm).
- **If the owner wants the 5% line resolved** (not run): (1) 4 more interleaved c / a pairs (~50
  min). The c spread (sd 0.41) dominates, so n = 8 puts the a vs c 95% interval at about ±1.4%;
  this only helps if the true value is not ~5%. (2) A fixed-load pair: offered load capped below both
  arms' capacity (e.g. ~190k matched/s), so CPU-s/1M is compared at equal fills without the fit (4
  pairs, ~50 min). (3) A disk per validator: not possible on this host.

## 43. Phase 3 prototype vs control with the disk taken out: shared NVMe vs tmpfs upper bound (campaign ozarchy-p3t, 2026-10-11)

**The CPU saving is work removed. Most of the blocks/s gain was relief from disk contention.** The
all-rows prototype cuts node CPU-s/1M by **5.2%** on the shared NVMe (CI 4.3-6.1%) and by **4.8%** with
every data dir on tmpfs (CI 2.2-7.4%). The two are equal within noise (paired difference +0.4 pp,
CI -3.9 to +4.7). The matched/s gain shrinks from **+5.8%** (shared NVMe, CI 3.5-8.1%) to **+3.1%** (tmpfs,
CI -1.0 to +7.3%, not resolved). The native blocks/s gain drops from **+14.0%** to **+1.6%** (CI -4.5 to
+7.6). On tmpfs the paired difference in the blocks/s gain is -12.8 pp (CI -23.3 to -2.4), so that part
of the p3p result came from the shared disk. **The tmpfs arms are an UPPER BOUND (no device I/O at all),
not a separate-disk result.** ozarchy has one NVMe. A disk per validator could not be tested here.

**Inference (not measured): a production validator with its own disk should land between the
two rows below.** Its disk carries only its own writes, so it has less contention than the shared
NVMe but more than tmpfs. Expect a prototype gain of about +3% to +6% matched/s, between +2% and +14%
native blocks/s (the blocks/s gain depends on how loaded the disk is), and a CPU-s/1M saving of about
5% in either case.

Owner request (Phase 3 un-shelved, s111, gate: matched/s and blocks/s up, no CPU regression,
restart <= 60 s, crash correctness): repeat the p3p a vs c comparison with the validators not
sharing a disk, to test whether +4.7% matched/s / +6.1% blocks/s holds.

Setup as section 42 (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900,
fresh genesis and data dir per cell, RocksDB `LOG` copied per cell). **One node binary for every arm:**
`proto/p3-coalesce` `682a8df5` (node `6e40fa3c`, bench `19f7ac94`; the staged copy from p3p, md5 checked;
its build dir is gone), harness `9b7e29b2` (the p3p tools copy). Arms:
- **cd / ad:** control / `TORUS_P3_PROTO=all`, data dirs on the shared NVMe (btrfs `/home`, zstd:3;
  = p3p arms c / a).
- **cm / am:** the same, with all 3 validators' data dirs on tmpfs (`DATA_ROOT=/dev/shm/ozarchy-p3t-devnet`;
  `/dev/shm` is a 32 GB tmpfs, no sudo needed). Only the data dirs move. `run/` is a symlink back to
  `/home/oz/torus-wsl-devnet/run`, so node logs, the pids file and every sampler path are the same
  in both modes. Every cell checks each node's `--data-dir` filesystem type (`fschk`, tmpfs or btrfs).
- **Fit check before launch:** p3p data dirs ended at 1.4-2.5 GB per validator, the WAL peaked at 2.27 GB
  (p3p restart cells' 1 s WAL samples), node RSS 4-5 GB. Estimate: ~3.5 GB per validator, ~10.5 GB for
  three; 53 GB available. Measured peak (2 s `du` samples): 2.68-2.77 GB per validator; MemAvailable never
  below 33.3 GB.

Cells 16:07-17:46 (driver `ozarchy-p3t-campaign.sh`, analyst scripts `tools/ozarchy-p3t-tools/a-*.py`):
warm-up w0 (cd, 60 s, excluded), then 4 rounds of 120 s cells in a 4x4 Williams square (cd ad am cm / ad cm
cd am / cm am ad cd / am cd cm ad; each arm follows every other arm once). All 17 cells are rc 0, PASS,
AGREE, ACCEPT, with no deaths. Every cell passed its hard checks (as section 42 plus the data-dir
filesystem). The host had no `cargo`, `rustc` or `rust-analyzer` running before any cell. Caveat: the
gated shell here cannot delete the tmpfs data after a tmpfs cell. The next tmpfs cell's `CLEAN=1`
wipes it, so up to one cell's tmpfs data (~7 GB) stayed in RAM during the disk cells that followed
(MemAvailable was logged and stayed >= 33 GB). It was deleted after the campaign.

### 43.1 Per arm (r1-r4, n = 4; ↓ lower is better, ↑ higher is better)

Node CPU-s/1M = val0-2 `proc_cpu` whole run / fills, mean of the 3 validators (section 42). Writes per
validator over the bench window from `io-ticks.txt`: device = `write_bytes` (bytes sent to the block
layer; 0 on tmpfs by definition), wchar = bytes passed to `write()` (file and socket; mode-independent).
RSS = per-node maximum of `cpu.csv`, mean of the 3 nodes.

| arm | matched/s ↑ mean (sd) | native blk/s ↑ mean (sd) | node CPU-s/1M ↓ mean (sd) | user / sys per 1M | device writes MB/s/val | wchar MB/s/val | RSS GB | data peak GB/val |
|---|---|---|---|---|---|---|---|---|
| cd (shared NVMe) | 200,770 (1,048) | 5.70 (0.43) | 25.090 (0.152) | 23.37 / 1.73 | 131.4 | 144.7 | 3.62 | 2.68 |
| ad (shared NVMe) | **212,362** (3,117) | **6.50** (0.21) | **23.787** (0.085) | 22.29 / 1.50 | 64.0 | 78.6 | 4.51 | 2.77 |
| cm (tmpfs, upper bound) | 211,117 (4,787) | 6.67 (0.20) | 25.068 (0.423) | 23.20 / 1.87 | 0 | 148.6 | 3.58 | 2.75 |
| am (tmpfs, upper bound) | **217,742** (5,318) | **6.77** (0.26) | **23.861** (0.317) | 22.24 / 1.62 | 0 | 78.8 | 4.67 | 2.77 |

Per cell (r1..r4): matched/s cd 200,108 / 199,669 / 201,433 / 201,868; ad 209,397 / 211,182 / 212,158 /
216,711; cm 211,607 / 216,328 / 211,808 / 204,724; am 219,374 / 220,062 / 209,893 / 221,640. Native blk/s
cd 6.22 / 5.56 / 5.82 / 5.20; ad 6.64 / 6.25 / 6.38 / 6.71; cm 6.75 / 6.88 / 6.63 / 6.42; am 6.93 / 6.73 /
6.42 / 7.01.

cd reproduces p3p's c (24.996 CPU-s/1M, 204,486 matched/s), and ad reproduces p3p's a (23.770, 214,120),
both within their spreads.

### 43.2 a vs c in each storage mode, and the storage effect (Welch 95% intervals)

| comparison | matched/s | native blk/s | node CPU-s/1M (whole run) | CPU at equal fills (ANCOVA) | wchar | RSS |
|---|---|---|---|---|---|---|
| ad vs cd, shared NVMe | **+5.77%** [+3.50, +8.05] | **+13.98%** [+2.26, +25.70] | **-5.19%** [-6.08, -4.30] | **-2.98%** [-4.61, -1.36] | -45.7% | +0.89 GB (+24.8%) |
| am vs cm, tmpfs UPPER BOUND | +3.14% [-1.01, +7.29] | +1.60% [-4.45, +7.64] | **-4.82%** [-7.39, -2.24] | **-4.19%** [-5.34, -3.04] | -47.0% | +1.09 GB (+30.4%) |
| cm vs cd (control: tmpfs vs disk) | +5.15% [+1.27, +9.04] | +16.97% [+5.43, +28.51] | -0.09% [-2.57, +2.40] | +2.12% [+0.53, +3.72] | +2.7% | -0.9% |
| am vs ad (prototype: tmpfs vs disk) | +2.53% [-1.20, +6.27] | +4.26% [-2.14, +10.66] | +0.31% [-1.89, +2.50] | +0.82% [-0.38, +2.02] | +0.2% | +3.5% |
| p3p a vs c (section 42, shared NVMe) | +4.71% | +6.1% | -4.90% [-2.4, -7.4] | -2.1% | (writes -51%) | +0.8 GB |

- **Paired by round (gain on tmpfs minus gain on disk, df 3):** matched/s -2.58 pp [-7.63, +2.47];
  blocks/s **-12.80 pp [-23.25, -2.35]**; CPU-s/1M +0.40 pp [-3.87, +4.67]. Per-round a/c matched/s gain:
  disk 4.6 / 5.8 / 5.3 / 7.4%, tmpfs 3.7 / 1.7 / -0.9 / 8.3%.
- **Equal fills:** common within-arm slope 12.87 ± 2.79 CPU-s per 1M fills (section 42: 10.1 ± 2.1), residual
  sd 4.87 CPU-s per node (df 11). Node CPU per validator per cell: cd 652, ad 649, cm 682, am 658 CPU-s.
- **Blocks/s on the shared disk is noisy for the control** (cd sd 0.43, r4-cd 5.20). This control
  variance is why the disk blocks/s gain here (+14%) is larger than in p3p (+6.1%), with a wide CI.
  Moving the control to tmpfs gives +17% blocks/s, about as much as the prototype gives on disk (+14%).
- **The tmpfs control costs a little more CPU at equal fills (+2.1%)** and has more sys time (1.87 vs
  1.73 per 1M). On tmpfs the node's `write()` copies into shmem and is charged to the node. On btrfs,
  writeback and zstd compression run in kernel workers that are not charged to the node. So disk-arm
  node CPU excludes the kernel's writeback / compression CPU, which favours the arm that writes more
  (the control). That makes the disk-arm CPU saving conservative, not inflated.
- RSS: +0.9-1.1 GB per node for the prototype in both modes (section 42: +0.8 GB). The tmpfs arms hold
  the data dirs in shmem too (2.7-2.8 GB per validator, outside node RSS).

### 43.3 Reading

- **CPU-s/1M (the "no CPU regression" gate):** the saving is the same with or without device I/O
  (-5.2% shared NVMe, -4.8% tmpfs; difference +0.4 pp, CI ±4.3). It is work removed from the node
  (flush worker + RocksDB, section 42.2), not a side effect of disk contention. At equal fills it is
  -3.0% (disk) / -4.2% (tmpfs). Both are clear of zero.
- **matched/s:** the gain persists in direction but shrinks: +5.8% with the shared disk, +3.1% (not
  resolved, n = 4) with no device I/O. About half of the shared-disk gain is consistent with disk
  contention relief and about half with work removed. The per-round tmpfs gains are noisy (-0.9 to
  +8.3%).
- **Native blocks/s:** most of the gain is disk contention. The control gains +17% blocks/s just from
  tmpfs. With tmpfs the prototype adds only +1.6% (not resolved). The paired difference (-12.8 pp)
  excludes zero.
- **For the Phase 3 gate (inference, see the line at the top):** on a validator with its own disk,
  expect matched/s up by roughly +3% to +6% and CPU-s/1M down by ~5%. A blocks/s gain above +2% is
  likely only on a busy disk.
- **Confidence:** high for the CPU saving (both modes, every cell). Medium for the matched/s split (n = 4
  per arm, tmpfs CI crosses zero). Medium for blocks/s (control on disk is noisy).
- Not measured: a real per-validator disk; restart / crash in tmpfs (not needed: no restart cells).

## 44. P2-3 worker pool sizing: thread spawns, spawn/join cost and what a pool would save (campaign ozarchy-p23, 2026-10-11)

**A persistent pool would save about 0.05-0.15 node CPU-s/1M (0.2-0.6%; hard ceiling 0.28, 1.1%) and
about 0.9-1.4 ms of execution-thread wall per native block (~0.5-0.8% of a ~170 ms block).** Main
`4e7fa146` spawns **41.0 threads per native block** (36.6-43.4 over 4 cells), all from the execution
thread, in about 2.5 scopes per block. Section 25 estimated ~0.8 ms per block, and these numbers
confirm it; the pool remains a small win.

Owner request (after Phase 3 un-shelved): size P2-3 (`item6-phase2-impl.md` P2-3, backlog since 9.10)
from a profile only. Questions: spawns per native block, call sites, CPU-s/1M in spawn/join (clone,
thread start, stack mmap/munmap, join/futex), and ms per native block on the execution thread.

Setup: main `4e7fa146` (worktree `wt/p23-4e7fa146`, fresh target `~/.cargo-target-p23`, frame pointers +
line tables + mold as p3s), node `169567f2`, bench `74cef98f`; harness `9b7e29b2`; tw defaults, Classic,
300 markets, the standard shape (MAX_IN_FLIGHT 4, budget 900), data on the shared NVMe. Cells 17:48-18:40 (driver
`ozarchy-p23-campaign.sh`): w0 (60 s, excluded), then p1 u1 p2 u2 p3 u3 p4 u4 (120 s). The p cells run
`prof-sidecar.sh` (the Phase 2 step 0 method: `perf record -e cycles:u -F 499 -g --call-graph fp` on val0,
45 s from bench + 35 s). All 9 cells are rc 0, AGREE, ACCEPT, with no deaths, all hard checks OK, and the
host quiet before every cell. No sudo (`perf_event_paranoid` 2, no tracefs): no kernel samples and no
sched/clone tracepoints.

| cells | matched/s | native blk/s | node CPU-s/1M |
|---|---|---|---|
| u1-u4 (unprofiled) | 197,867 / 197,743 / 200,742 / 199,591 | 5.91 / 6.02 / 5.45 / 6.13 | 25.13 / 25.32 / 25.18 / 25.30 |
| p1-p4 (perf on val0) | 191,658 / 196,034 / 191,698 / 191,875 | 4.46 / 4.81 / 4.60 / 4.57 | 26.58 / 26.17 / 26.55 / 26.33 |

### 44.1 Spawns per native block and call sites (exact counts)

`perf record -p` inherits its events into every new thread and writes a `PERF_RECORD_FORK` / `EXIT`
for each one, so `perf script --show-task-events` gives **every** thread creation in val0's
window. These are exact counts, not a lower bound. Blocks = `torus_exec_native_blocks_total`
delta over the window (`prof-metrics-before/after`).

| cell | window | native blocks | spawns | per native block | scopes per block (forks < 2 ms apart) | child lifetime median / p90 |
|---|---|---|---|---|---|---|
| p1 | 48.3 s | 205 | 8,897 | 43.4 | 2.63 | 6.9 / 19.5 ms |
| p2 | 47.8 s | 238 | 8,708 | 36.6 | 2.32 | 7.3 / 18.1 ms |
| p3 | 48.0 s | 211 | 9,098 | 43.1 | 2.60 | 6.9 / 17.2 ms |
| p4 | 48.3 s | 203 | 8,291 | 40.8 | 2.53 | 8.6 / 19.5 ms |

- **Parent: the execution thread (`torus-execution`) for 100% of spawns.** The flush worker spawned
  nothing in these windows (state hash off, so there is no digest spawn).
- Scope sizes per native block: one single-thread spawn (0.99-1.01 per block: `end_resident_on_worker`),
  one full 32-thread scope (0.91-1.04 per block; 32 = host parallelism) and 0.42-0.60 smaller scopes (mean
  15-16 threads).
- **Sites** (the outermost torus frame of each spawned thread's samples; share of spawned-thread
  cycles / threads seen): `MarketWorkerPool::match_parallel_capped` 56.5-58.6% / ~3,500-4,500 threads per
  cell; `NativeExecutor::settle_market_results` 26.9-30.7% / ~3,100-4,100; `end_resident_on_worker`
  12.1-14.3% / ~1 per block. About 3,000-7,300 threads per cell had no torus frame in their samples
  (spawn glue only). No `drain_books_parallel`, `phase2_parallel_prepare`, `open_order_counts` or digest
  spawns appear. Section 25 counted 47.5-52.4 per block including save-books, so save books now spawns
  nothing on the execution thread on this shape.
- Section 22 / the P2-3 text quote ">= 62 per native block" from the crab-era 1 Hz tid count (lower
  bound). The exact figure on main is 41.

### 44.2 Cost per spawn

- **User side (perf, `cycles:u`):** spawn/join glue as the leaf frame (`pthread_create`, `clone3`,
  `mmap` / `mprotect` / `munmap`, `sigaltstack`, `thread_start`, scope / join, TLS dtors) is
  **0.038-0.045%** of val0 user cycles (0.08-0.15% of the execution thread's), i.e. ~0.01 CPU-s/1M.
- **Kernel side, in the node (unprofiled u cells, 1 Hz task sampler):** CPU of the threads that were not
  alive at two samples (= the spawned workers) is user 156-160 s and **sys 3.6-3.8 s per 140 s window =
  0.142-0.147 CPU-s/1M**, ~100-120 us sys per spawned thread (at 41 spawns per block). This is an upper bound for the
  worker-side kernel cost, because it also holds the workers' own syscalls. The parent-side
  `clone` lands in the execution thread's sys (3.3-3.5 s per window, 0.13 CPU-s/1M, together with all
  its other syscalls).
- **Microbench** (`ozarchy-p23-tools/spawnbench.rs`, Rust `std::thread::scope` like the node, 190 parked
  threads, 1 GiB touched heap, 3 runs, idle host): scope spawn + join **47-60 us CPU per thread** (user
  4-11, sys 37-50) vs a persistent pool **5-9 us per task**. Caller wall in `spawn()` 15-20 us per thread
  (25-33 us when the workers are busy). One 32-thread scope with empty jobs takes 0.72-0.74 ms vs
  0.04 ms on the pool. With 3 ms of work per job it takes 3.91-3.96 ms vs 3.13-3.19 ms (+0.75 ms).
  A single spawn costs 0.042 vs 0.009 ms.
- **Perf trap:** with perf attached, val0 sys rises from 1.7 to 7.0-7.6 CPU-s/1M (other validators
  and unprofiled cells stay at 1.6-1.8). Inherited perf events on ~185 new threads per second are
  charged as sys. Profiled-cell sys time therefore says nothing about spawn cost. p cells run 3-4%
  fewer matched/s and ~22% fewer native blocks/s than u cells.

### 44.3 What a persistent pool would save

Spawns per 1M fills: 41.0 per block / ~31,200 fills per native block (u cells, 29.8-34.0k) ≈ **1,310**.

| | low | high | basis |
|---|---|---|---|
| node CPU-s/1M | **0.05** (1,310 x 38 us) | **0.15** (1,310 x (~105 us sys + ~10 us user glue)) | microbench saving per spawn (scope - pool) / in-node exited-thread sys + perf user glue |
| hard ceiling | | 0.28 (1.1%) | all exited-thread sys + all execution-thread sys |
| execution-thread wall ms per native block | **0.9** (1.0 x 0.70 + 0.5 x 0.33 + 1.0 x 0.03) | **1.4** (busy workers: 41 x 33 us) | microbench scope - pool per scope size x scopes per block |
| share of a native block (~170 ms at 5.9 blk/s) | 0.5% | 0.8% | |

Counted: the clone / thread start / stack mmap-mprotect-munmap / sigaltstack / TLS setup and teardown /
join wake-up of every spawn, on both the parent and the worker side, and the critical-path delay of
spawning 32 workers serially from the execution thread. Not counted: cold caches and TLB on a new
thread (a pool keeps them warm, so the true saving may be a little higher). A pool's own wake-up
latency under load is counted only as the microbench pool figure. Kernel time inside the node is
measured only as an upper bound (no kernel samples without sudo).

**Reading:** P2-3 stays a small win. About 41 spawns per block cost ~0.05-0.15 CPU-s/1M and ~0.9-1.4 ms per
native block on the execution thread, so a pool is worth +0.5-1% matched/s at most (section 25 said
+0.5-1%). If it is built, the three sites are `match_parallel_capped`, `settle_market_results`
(32-way scopes) and the per-block `end_resident_on_worker` thread. The P2-3 gate's "sys CPU per 1k
fills down" is still unreadable at cell resolution (the saving is ~3-9% of process sys, 1.7 CPU-s/1M).
Read the fork count (perf task events, ~0 per block after warm-up) and matched/s instead.

## 45. Phase 3 step 1 (checkpoint interval 15 s) vs control on shared NVMe and tmpfs (campaign ozarchy-p3c1, 2026-10-11)

**Step 1 cuts node CPU-s/1M by 7.9% on the shared NVMe and 8.3% on tmpfs, and doubles native blocks/s. It
lowers matched/s by 7.0% / 10.6%. The matched/s drop is a load-generator artifact of the known step 1 read lag,
not lost node capacity.** Under step 1 `eth_blockNumber` advances only at checkpoint heights (in steps of 100
blocks). The bench releases its in-flight cap from that block tail, so every sender stalls between
checkpoints: no new submission for 37 s of each 120 s cell (control 2-3 s), and submit/s falls 13%. The node
side is not the limit: the exec thread is less busy (0.89 vs 0.95) and node-side RPC submit latency barely moves. Against
the revised gate: **CPU-s/1M down: met. Native blocks/s no regression: met (+96% / +86%). matched/s up: not met
as measured, and cannot be judged with this load generator until step 2 makes RPC reads current (or the
bench releases on another signal).** Restart (<= 60 s) and crash correctness are **not tested here** (no
restart or crash cells). The tmpfs arms are an **UPPER BOUND** (no device I/O), not a separate-disk result.

**Also valid for step 1 `a44bdde6`** (18c, s111): its only change after `7c178637` is the byte-cap fix (the cap
fired 2 blocks late; it now counts blocks n-1 and n), which acts only when `TORUS_CHECKPOINT_MAX_BYTES` is set; it
was unset in every step 1 cell here. Restart and crash cells ride along with the step 2 bench.

Owner request (s111, Phase 3 gate revised: matched/s up, CPU-s/1M down, native blocks/s no regression,
restart <= 60 s, crash correctness): measure the real step 1 code (18c, `feat/item6-phase3-step1` `7c178637`,
off main `2725e321`; plan `item6-phase3-impl.md` tasks 1.2-1.7) with the checkpoint interval on at 15 s, against
its branch point, on the shared NVMe and on tmpfs, in the p3t shape (section 43).

Setup as section 43 (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, fresh genesis
and data dir per cell, RocksDB `LOG` copied per cell, harness `9b7e29b2`). **Two node binaries**, built the same
way (fresh target dirs, frame pointers + line tables + mold): control = `2725e321` (node `18fb490d`, the step 1
merge-base with origin/main, checked by the driver) and step 1 = `7c178637` (node `c161a337`). One bench for
every arm (`c4559781`, from the control build). Arms:
- **cd / sd:** control / step 1 with `TORUS_CHECKPOINT_INTERVAL_MS=15000` (`TORUS_CHECKPOINT_MAX_BYTES` unset),
  data dirs on the shared NVMe (btrfs `/home`).
- **cm / sm:** the same with all 3 data dirs on tmpfs (`/dev/shm/ozarchy-p3c1-devnet`, `run/` symlinked back,
  as section 43).
- Per-cell hard checks as section 43 (md5 per arm, env, book CF, codec/WAL, trie, WAL cap, zero trade rows, state
  hash off, data-dir filesystem), plus: the interval knob on the 3 step 1 nodes only; the line `item 6 Phase 3:
  coalesced state checkpoints ON` on every step 1 validator and on no control; `torus_exec_checkpoints` >= 1 per
  step 1 node and absent on the control; `TORUS_EXEC_PIPELINE` not 0 (default on) with the `bl2 exec pipeline
  ENABLED` line on every validator in every arm (the interval is ignored when the pipeline is off).
- New sampler: the 3 validators' `/metrics` (ports 9161-9163) every 2 s for the checkpoint family and the
  reject / failure counters (`ckpt-samples.txt`), and a per-cell post step (`ozarchy-p3c1-post.py`) that flags any
  nonzero `torus_exec_checkpoint_layer_shared` as a hard finding.

Cells 19:28-21:23 (driver `ozarchy-p3c1-campaign.sh`, analyst scripts `tools/ozarchy-p3c1-tools/a-*.py`): warm-up w0
(cd, 60 s, excluded), then 120 s cells in the section 43 Williams square (sd for ad, sm for am). **Round 4 was
rerun:** the harness worktree `wt/p3s0r-9b7e29b2` was removed by a worktree cleanup (`git worktree remove`) at
20:48:08, during the first r4-sm. That cell's teardown found no `stop-3val.sh`, so the nodes ran on until the
cell's systemd unit ended at 20:50:27 and stopped its cgroup. The watcher then logged a false "DEATH val0": all 3
node logs stop together at an idle 1.3 s burst boundary, with no panic, no fail-stop and summary ACCEPT. r4-cd
failed on the missing `detach.sh`. The worktree was recreated at the same commit, and round 4 was rerun as a
warm-up w4 (cd, 60 s, excluded) + r4b-sm r4b-cd r4b-cm r4b-sd (same rotation). **The first r4-sm is excluded**
(its files are kept). r1-r3 had ended before 20:48 (r3-cd at 20:44), so they used no file from the deleted dir.
All 16 analysed cells are rc 0, PASS, AGREE, ACCEPT, with no deaths and every hard check OK (216 CHECK OK over the
18 completed cells, no CHECK FAIL, no hard finding). No `cargo`, `rustc` or `rust-analyzer` ran before any cell.

### 45.1 Per arm (r1, r2, r3, r4b; n = 4; ↓ lower is better, ↑ higher is better)

Columns as section 43.1.

| arm | matched/s ↑ mean (sd) | native blk/s ↑ mean (sd) | node CPU-s/1M ↓ mean (sd) | user / sys per 1M | device writes MB/s/val | wchar MB/s/val | RSS GB | data peak GB/val |
|---|---|---|---|---|---|---|---|---|
| cd (shared NVMe) | 198,992 (2,245) | 6.14 (0.09) | 25.311 (0.124) | 23.53 / 1.78 | 131.4 | 144.5 | 3.61 | 2.66 |
| sd (shared NVMe) | 185,121 (1,941) | **12.03** (0.57) | **23.306** (0.188) | 21.81 / 1.50 | 56.1 | 68.3 | 3.76 | 2.61 |
| cm (tmpfs, upper bound) | 210,267 (1,617) | 6.34 (0.23) | 25.216 (0.128) | 23.33 / 1.88 | 0 | 150.7 | 3.61 | 2.76 |
| sm (tmpfs, upper bound) | 188,042 (1,512) | **11.82** (0.32) | **23.136** (0.051) | 21.58 / 1.56 | 0 | 68.6 | 3.84 | 2.61 |

Per cell (r1, r2, r3, r4b): matched/s cd 195,668 / 200,586 / 199,969 / 199,746; sd 184,343 / 185,534 / 183,016 /
187,590; cm 211,960 / 211,230 / 209,450 / 208,428; sm 187,171 / 187,192 / 190,298 / 187,509. Native blk/s cd 6.09 /
6.07 / 6.25 / 6.16; sd 12.42 / 11.20 / 12.36 / 12.15; cm 6.61 / 6.24 / 6.46 / 6.07; sm 11.56 / 12.27 / 11.82 / 11.63.
cd and cm reproduce section 43's cd / cm (25.09 / 25.07 CPU-s/1M, 200,770 / 211,117 matched/s) within noise.

### 45.2 Step 1 vs control in each storage mode (Welch 95% intervals)

| comparison | matched/s | native blk/s | node CPU-s/1M (whole run) | CPU at equal fills (ANCOVA) | wchar | RSS |
|---|---|---|---|---|---|---|
| sd vs cd, shared NVMe | -6.97% [-8.80, -5.15] | **+95.8%** [+81.0, +110.7] | **-7.92%** [-9.06, -6.78] | -9.67% [-13.10, -6.24] | -52.7% | +0.15 GB (+4.1%) |
| sm vs cm, tmpfs UPPER BOUND | -10.57% [-11.86, -9.28] | **+86.3%** [+78.3, +94.4] | **-8.25%** [-9.00, -7.49] | -10.37% [-14.59, -6.15] | -54.5% | +0.23 GB (+6.2%) |
| cm vs cd (control: tmpfs vs disk) | +5.67% [+3.88, +7.45] | +3.3% [-2.4, +8.9] | -0.38% [-1.24, +0.48] | +0.41% [-1.15, +1.97] | +4.2% | 0% |
| sm vs sd (step 1: tmpfs vs disk) | +1.58% [-0.05, +3.20] | -1.8% [-8.7, +5.2] | -0.73% [-2.06, +0.60] | -0.52% [-1.66, +0.63] | +0.5% | +2.1% |
| p3t prototype vs control, shared NVMe (section 43) | +5.77% | +14.0% | -5.19% | -2.98% | -45.7% | +0.89 GB |
| p3t prototype vs control, tmpfs (section 43) | +3.14% | +1.6% | -4.82% | -4.19% | -47.0% | +1.09 GB |

- **Paired by round (step 1 gain on tmpfs minus on disk, df 3):** matched/s -3.60 pp [-7.05, -0.15]; blocks/s -9.3 pp
  [-36.6, +18.1]; CPU-s/1M -0.33 pp [-1.14, +0.48]. Per-round CPU gain: disk -6.8 / -8.8 / -7.5 / -8.6%, tmpfs -7.7 /
  -8.6 / -8.1 / -8.7%.
- **Equal fills:** common within-arm slope 21.0 ± 3.4 CPU-s per 1M fills, residual sd 3.10 CPU-s per node (df 11).
  The step 1 arms ran 10-13% fewer fills (load-generator stalls, 45.4), so the adjustment extrapolates across that
  gap. Read it as "at least the whole-run saving", not as a sharper number. Node CPU per validator per cell: cd 649,
  sd 536, cm 678, sm 544 CPU-s.
- **Where the CPU goes (per-thread CPU-s/1M, mean of the 4 disk cells, cd -> sd):** flush worker 0.97 -> 0.24, exec
  thread 4.03 -> 3.65, RocksDB low (compaction) 0.98 -> 0.77, RocksDB high (flush) 0.45 -> 0.31, other threads 11.98
  -> 11.29. Device writes per validator -57% (131.4 -> 56.1 MB/s), WAL `write()` 131 -> 57 MB/s.
- **Exec / chain (mean of 4 cells, cd / sd / cm / sm):** exec-thread busy fraction 0.953 / 0.895 / 0.959 / 0.885;
  chain_ms 124.1 / 63.8 / 122.9 / 65.7; pipelined_ms 30.2 / 3.4 / 30.9 / 3.7; hand-off wait 0.93 / 0.01 / 0.87 / 0.01 ms;
  txs per block 128 / 68 / 134 / 71; commit interval 151 / 80 / 146 / 82 ms. The per-block flush hand-off is gone, so
  blocks close in about half the time and carry about half the actions.
- **Compared with the prototype (p3t):** step 1 removes more CPU (-7.9% vs -5.2% on disk, -8.3% vs -4.8% on tmpfs;
  the CIs do not overlap the prototype's point estimates), writes about as much less (-53% vs -46% wchar), and costs
  far less RAM (+0.15-0.23 GB RSS vs +0.9-1.1 GB). Blocks/s doubles where the prototype added +14% / +1.6%. matched/s
  moves the other way (-7% / -11% vs +5.8% / +3.1%) because of the read lag below. The prototype did not hold back
  the RPC head.

### 45.3 Checkpoint metrics (step 1 arms; control arms export none, as expected)

Per node per cell, from the harness's metrics before / after (whole cell incl. drain) and the 2 s series (bench window).

| arm | checkpoints / node / cell | per min (bench window) | wait s / cell | wait ms / checkpoint | layer MB mean / peak | flush-worker checkpoint s / cell | s / checkpoint | layer_shared |
|---|---|---|---|---|---|---|---|---|
| sd | 19.8 (18-21) | 7.7 | 0.28 | 14.1 | 80.3 / 214 | 5.92 | 0.30 | **0** |
| sm | 19.5 (19-20) | 7.4 | 0.17 | 8.7 | 82.8 / 226 | 6.24 | 0.32 | **0** |

- **`torus_exec_checkpoint_layer_shared` stayed 0** in every sample of every step 1 cell (24 node-cells, ~2,700
  scrapes): no hard finding.
- Wait = `torus_exec_checkpoint_wait_seconds` (E blocked handing off a checkpoint while the previous one is still in
  flight): 0.17-0.28 s per node per cell, < 0.3% of the bench window. Peak layer bytes = largest per-checkpoint layer
  between two samples (the histogram has no max).
- **Which trigger fires: the attest height (`h % 100 == 0`), not the 15 s interval.** In the bench window 351 of 375
  checkpoints (94%) fall on a 100-block crossing. 24 (6%, about one per node per cell, at the start of the bench while
  blocks are still slow) were fired by the 15 s interval. None came from a barrier or the byte cap (unset). The median
  gap is 8.2 s (max 18.6 s). At ~12 native blocks/s, 100 blocks take ~8.3 s, under the 15 s interval, so the interval
  only binds below ~6.7 blocks/s. This matches the design (`item6-phase3-impl.md` section 3: the triggers are the
  interval, the byte cap, `n % 100 == 0` and shutdown; code: `checkpoint_upkeep` in `app.rs`,
  `STATE_HASH_CHECKPOINT_INTERVAL` = 100). The prep smoke cell's 12 checkpoints per node in 60 s had the same cause.
  The effective checkpoint interval here is 100 blocks, not 15 s.

### 45.4 Rejections and the read lag

Mean per cell (node counters: mean of the 3 nodes; deltas before -> after the bench).

| count | cd | sd | cm | sm |
|---|---|---|---|---|
| bench submitted actions | 138,398 | 119,832 | 144,730 | 121,988 |
| bench sent - accepted (place + cancel-all) | 1.0 | 0.2 | 2.0 | 0 |
| in-flight released by RPC refusal / by timeout | 1.0 / 0 | 0.2 / 0 | 2.0 / 0 | 0 / 0 |
| block-tail errors / missed bodies | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| mempool nonce-expired evictions (sum, 4 cells x 3 nodes) | 0 | 0 | 0 | 0 |
| oracle rejected / stale marks at bench end | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| `torus_exec_action_rejections_total` | 356 | 291 | 353 | 331 |
| `torus_orders_rejected_cancelled_total` | 21,378 | 21,161 | 21,317 | 21,873 |
| `torus_rpc_submit_admit_rejects_total{backlog_preverify}` | 18,938 | 3,257 | 14,232 | 5,589 |
| `torus_missing_action_rejections_total` | 0.8 | 0.6 | 0.9 | 0.8 |
| RPC `torus_submitNativeActionsBin` status=error | 0.3 | 0.1 | 0.7 | 0 |
| WARN "skipping duplicate/replayed native action on live commit path" per node | 4 | 62 | 51 | 51 |

- **No new rejection class with step 1.** No nonce, balance or insufficient-funds errors appear anywhere (bench log,
  RPC, node logs). Margin, open-limit, book and "other" order rejects are 0 in every arm. The backlog-preverify
  admission rejects fall with step 1 because less load is offered.
- **Flag (read lag, expected until step 2): the RPC head moves only at checkpoints.** The bench's progress lines
  show `eth_blockNumber` advancing by a median of 99-100 blocks per step under step 1 (control: 2), i.e. at checkpoint
  heights. The in-flight watcher (`in_flight.rs`: one `eth_blockNumber`, then every new body) therefore releases slots
  in bursts. Submissions stop for 36.9 s per 120 s cell in both step 1 modes (control 2.2 / 3.0 s), submit/s falls
  from 1,134 / 1,186 to 982 / 1,000, and matched/s follows. Node-side RPC barely changes (batch submit verify 9.2 -> 10.4 ms on disk, 9.3 -> 9.3 ms on
  tmpfs; permit wait 4-6 ms in every arm), and the exec thread has more idle time.
- **Flag (mempool lag, expected until step 2):** the deterministic duplicate-skip WARN (identical count on all 3
  nodes) rises on the shared disk from ~4 to ~62 per node per cell with step 1 (~0.05% of actions). On tmpfs it is
  unchanged (51 vs 51), and the tmpfs control is already high, so it tracks how fast committed actions leave the
  mempool. It is not a correctness problem: the skipped actions are dropped at commit, the same on every node.

### 45.5 Reading

- **CPU-s/1M (gate: down): met.** -7.9% (disk, CI -9.1 to -6.8) and -8.3% (tmpfs, CI -9.0 to -7.5), equal within
  noise across the two modes (paired difference -0.3 pp). It is work removed (flush worker, exec thread, RocksDB
  flush and compaction), about 1.5-1.7 times the prototype's saving.
- **Native blocks/s (gate: no regression): met.** +96% / +86%. Blocks close in half the time because the exec thread
  no longer waits on the per-block flush hand-off; each block carries half the actions.
- **matched/s (gate: up): not met as measured (-7.0% / -10.6%), and not decidable with this bench.** The drop follows
  the offered load (submitted -13% / -16%), which the step 1 RPC head lag (one checkpoint, ~8 s) throttles through the
  bench's in-flight cap. The node has headroom (exec busy 0.89 vs 0.95). To judge this gate, re-measure after step 2
  (RPC and mempool reads current), or with a bench that releases in-flight slots on a signal that is not held back.
  Inference, not measured: matched/s should then at least reach the control, since CPU per fill is 8% lower and the
  exec thread is less busy.
- **Disk:** device writes per validator -57% on the shared NVMe (131 -> 56 MB/s).
- **RAM:** +0.15 GB (disk) / +0.23 GB (tmpfs) RSS per node; layer peak ~0.2 GB per checkpoint.
- **Not tested here:** restart <= 60 s and crash correctness (no restart or crash cells; 18c's step 1 tests cover crash
  and replay at interval > 0, task 1.7). A validator with its own disk was not available (ozarchy has one NVMe).
- **Confidence:** high for CPU (every cell, both modes, tight CIs) and blocks/s. High that the matched/s drop is the
  read-lag throttle (the submit stalls line up with the 100-block RPC head steps in every step 1 cell). Medium for the
  equal-fills numbers (extrapolated across a 10-13% fills gap).
## 46. Phase 3 step 1 restart and crash cells: SIGKILL, SIGTERM and SIGINT at interval 15 s vs off (campaign ozarchy-p3r1, 2026-10-12)

Owner request (via 18c, s111): the restart <= 60 s and crash-correctness part of the Phase 3 gate, measured on
step 1 `a44bdde6` (`feat/item6-phase3-step1`, off main `2725e321`; differs from the p3c1 commit `7c178637` only by
the byte-cap fix, which is inactive here because `TORUS_CHECKPOINT_MAX_BYTES` is unset). Measurement only, no node
change. Section 45 (p3c1) and review log row 50 are on the unmerged branch `docs/p3-step1-bench`, not yet on main,
so this is section 46 and row 51.

### 46.1 Setup

- **One binary in both arms:** `a44bdde6`, node `7d1a0874` (md5 `7d1a08743778d315ed0c7a21eaeb8f6c`), built in
  its own worktree and target dir (`~/.cargo-target-p3r1-a44b`, `ozarchy-p3r1-build/md5s.txt`). The bench is
  the p3c1 control bench `c4559781` (the load generator the p3c1 sd arm ran). Harness `9b7e29b2` (run-cell.sh
  unchanged; the p3p tools copy, `crash-kill.sh` patched only for `KILL_SIG`). tw (trade history off + WAL
  2048 MiB), Classic, 300 markets, standard shape (MAX_IN_FLIGHT 4, OPEN_ORDER_BUDGET 900), shared NVMe.
- **Arms:** o = `TORUS_CHECKPOINT_INTERVAL_MS` unset (today's per-block flush) and s = `15000`. In
  `exec_pipeline.rs` unset and `0` both parse to `None` (checkpoints off), so the o arm leaves it unset.
  `TORUS_EXEC_PIPELINE` was unset (default on) and checked in every cell, on all 3 validators and on every
  restarted val1 (env + `bl2 exec pipeline ENABLED`). The s arm's enable line and checkpoint counter were
  checked per cell: counter >= 1 on s and 0 on o.
- **State hash on in every cell:** `STATE_HASH_ACTIVATION=1` (genesis `state_hash_activation = 1`, checked).
  `TORUS_STATE_HASH_FAILSTOP` was unset (default off), so a mismatch would show as an ERROR line and a counter
  and the cell would keep its evidence. **The devnet passes no `--state-hash-attest-key`, so no validator
  submits `AttestStateHash`** (`votes: []`, `quorumHash: null` everywhere). The running hash is computed and
  checkpointed, but the on-chain quorum comparison does not run. Agreement was checked by the campaign instead:
  `torus_getStateHash` read every 100-block checkpoint's `localHash` on each validator (polled every 15 s until
  the harness stop) and compared the three.
- **Cells:** 12 restart cells of 540 s, each with 4 kills of val1, restarted from the same data dir: x at bench
  + 60 / 180 / 300 / 420 s and y at + 65 / 187 / 309 / 431 s. Checkpoints come every ~100 blocks (~8-15 s), so
  the kills land at different points of the window (from 7 to 162 blocks of rewind, below). Two cells per arm x
  signal, giving 8 kills each and 48 in total. Interleaved order: x1 o-KILL s-KILL s-TERM o-TERM o-INT s-INT,
  then y1 in reverse. A 60 s warm-up (w0, o, no kill) came first; the prep smoke cell was s INT, 150 s, kill at 60.
- **Signals:** KILL (`kill -9`) and TERM, as requested, plus INT. **On `a44bdde6` SIGTERM is not a clean stop.**
  `main.rs` waits only on `tokio::signal::ctrl_c()` (SIGINT), so SIGTERM is the default action: immediate exit,
  no shutdown line, the same as SIGKILL for the data on disk. SIGINT is the node's real clean-stop path
  (`shutdown signal received`; the `ExecutionContext` Drop closes the open window into a final checkpoint), so
  it was added as the clean-stop arm. `KILL_WAIT=60` for every signal.
- **Samplers added to p3c1's:** every validator's `/metrics` every 1 s (block height, exec queue depth, state
  hash height, checkpoint family), the state hash poller above, and a per-cell scan of WARN/ERROR lines for
  correctness evidence.
- **Host:** quiet before every cell (no rust-analyzer, cargo or rustc; load1 < 2). The coordinator reported one
  outside `cargo test -p bench-throughput` (another agent's Codex review) from 00:33:29 to 00:35:10. It
  overlapped only the last ~2 s of x1-s-KILL's end-of-cell state digest and its teardown. That was after the
  load window (00:22:59-00:32:03) and after all four kills and rejoins (last restart 00:30:00), so no timing is
  affected and the cell is kept.
- **Validity:** all 13 cells ran to the end. Every hard check passed (CHECK OK; no CHECK FAIL, no node death,
  no HARD FINDING line). The 12 restart cells have rc 2, which is the known false FAIL from the INFO line
  `running state hash fail-stop ... on=false`; summarize's DISAGREE on 9 of them is explained in 46.4.

### 46.2 Method (per kill; scripts in `bench-archive/tools/ozarchy-p3r1-tools/`, `a-restart.py`)

Times come from `crash-kill{,-k}.json` (kill and restart, epoch) and val1's restart tail
`crash-restart-tail{,-k}.log` (UTC log timestamps).
- down = kill -> restart: the process exit plus `crash-kill.sh`'s 1 s poll.
- restart -> DB open = restart -> `state database opened`.
- Replay: WARN `crash recovery: execution gap detected` gives committed C, applied A (the durable marker) and
  the gap. Replay s = that line -> `exec pipeline ENABLED ... applied=`. When there is no gap line, A = C = the
  ENABLED value.
- **Restart -> first commit (chain)** = restart -> val0's first commit after the restart (`crash-freeze.py`,
  val0 log). kill -> first commit = down + that. This is the primary number: val1's own first
  `on_committed_block` can be a block that val0 had already committed (smoke cell: val1 at +2.4 s, the chain
  at +15.7 s). val1's line is reported as well.
- **Rejoin** = restart -> first 1 s sample in which val1's committed height >= min(val0, val2) - 2 and val1's
  executed height (block height - exec queue depth) >= min(val0, val2) executed - 2.
- **Correctness per kill:** A <= C; the first replayed `block done` height = A + 1; the replayed heights are
  exactly A+1..C, each once; ENABLED `applied` = C; A >= val1's durable state-hash height in the last 1 s
  sample before the kill (the replay never starts below what was already durable). **Per cell:** block hashes
  equal; the RPC state digest (300 books + OI + 50 accounts); every state-hash checkpoint's `localHash` equal
  on the 3 validators.

### 46.3 Restart vs the 60 s gate (mean / max)

| arm | signal | kills | down s | restart -> DB open s | replay blocks (C - A) | replay s | replay blocks/s | restart -> first commit s | kill -> first commit s | restart -> rejoin s |
|---|---|---|---|---|---|---|---|---|---|---|
| o (off) | KILL | 8 | 1.0 / 1.0 | 3.0 / 4.3 | 49 / 65 | 9.5 / 12.8 | 5.1 / 6.7 | 13.3 / 15.9 | 14.3 / 16.9 | 13.2 / 15.4 |
| s (15 s) | KILL | 8 | 1.0 / 1.1 | 2.4 / 3.6 | 92 / 162 | 7.8 / 20.4 | 13.1 / 21.2 | 13.4 / 22.2 | 14.4 / **23.2** | 11.0 / 22.8 |
| o | TERM (= kill) | 8 | 1.0 / 1.0 | 3.3 / 4.3 | 53 / 65 | 8.9 / 10.9 | 5.9 / 8.6 | 13.7 / 14.5 | 14.7 / 15.5 | 12.8 / 15.0 |
| s | TERM (= kill) | 6 (+2, see below) | 1.0 / 1.0 | 2.5 / 3.6 | 56 / 142 | 5.3 / 12.5 | 11.0 / 26.0 | 14.7 / 20.7 | 15.7 / 21.7 | 8.3 / 15.9 |
| o | INT (clean) | 8 | 9.0 / 12.0 | 3.9 / 4.6 | 0 / 0 | - | - | 10.3 / 13.5 | 19.4 / 22.6 | 24.4 / 125.6 (9.9 / 15.2 without k1 of y1) |
| s | INT (clean) | 8 | 3.9 / 8.0 | 3.1 / 4.0 | 0 / 0 | - | - | 9.9 / 18.2 | 13.8 / 21.2 | 9.7 / 16.7 |

Per cell (kills 1 / 2 / 3 / 4):

| cell | A (marker) ; C (committed) | replay blocks | replay s | restart -> first commit s | kill -> first commit s | rejoin s |
|---|---|---|---|---|---|---|
| x1-o-KILL | 689 / 1554 / 2419 / 3218 ; 754 / 1612 / 2475 / 3249 | 65 / 58 / 56 / 31 | 12.8 / 11.6 / 8.4 / 8.1 | 15.9 / 14.5 / 13.5 / 12.6 | 16.9 / 15.5 / 14.5 / 13.6 | 15.4 / 15.1 / 13.0 / 12.9 |
| y1-o-KILL | 782 / 1696 / 2558 / 3443 ; 818 / 1755 / 2620 / 3470 | 36 / 59 / 62 / 27 | 6.8 / 9.3 / 11.7 / 7.3 | 10.3 / 13.0 / 14.8 / 11.5 | 11.3 / 14.0 / 15.8 / 12.5 | 10.3 / 14.6 / 14.4 / 9.7 |
| x1-s-KILL | 1000 / 2300 / 3700 / 5100 ; 1102 / 2417 / 3862 / 5176 | 102 / 117 / 162 / 76 | 7.0 / 7.3 / 20.4 / 6.8 | 11.4 / 2.6 / 22.2 / 13.7 | 12.4 / 3.6 / 23.2 / 14.7 | 11.6 / 10.7 / 22.8 / 10.2 |
| y1-s-KILL | 1200 / 2400 / 2800 / 3200 ; 1255 / 2494 / 2834 / 3296 | 55 / 94 / 34 / 96 | 5.8 / 4.4 / 5.4 / 5.3 | 17.7 / 18.0 / 11.3 / 9.9 | 18.8 / 19.1 / 12.3 / 10.9 | 7.6 / 8.7 / 7.7 / 8.7 |
| x1-o-TERM | 629 / 1489 / 2429 / 3388 ; 693 / 1546 / 2476 / 3404 | 64 / 57 / 47 / 16 | 10.8 / 10.4 / 8.8 / 4.8 | 13.8 / 14.2 / 13.2 / 12.0 | 14.8 / 15.2 / 14.2 / 13.0 | 13.1 / 15.0 / 12.5 / 9.1 |
| y1-o-TERM | 627 / 1529 / 2408 / 3298 ; 689 / 1587 / 2473 / 3355 | 62 / 58 / 65 / 57 | 7.2 / 10.9 / 9.0 / 9.5 | 14.5 / 13.7 / 14.1 / 14.2 | 15.5 / 14.8 / 15.1 / 15.2 | 11.1 / 13.9 / 13.9 / 14.0 |
| x1-s-TERM | 1000 / 2300 / 2339 / 2354 ; 1073 / 2339 / 2354 / 2354 | 73 / 39 / 15 / 0 | 2.8 / 4.8 / 6.0 / 0.0 | 13.8 / 8.9 / - / - | 14.8 / 9.9 / - / - | 5.0 / 7.2 / 7.5 / 312.6 |
| y1-s-TERM | 1100 / 2200 / 3700 / 4099 ; 1146 / 2342 / 3731 / 4106 | 46 / 142 / 31 / 7 | 5.3 / 12.5 / 3.2 / 3.1 | 17.4 / 18.0 / 9.4 / 20.7 | 18.4 / 19.0 / 10.4 / 21.7 | 7.6 / 15.9 / 7.7 / 6.6 |
| x1-o-INT | A = C: 786 / 1185 / 1431 / 1600 | 0 | 0 | 13.5 / 5.8 / 12.6 / 7.2 | 21.5 / 15.8 / 22.6 / 13.2 | 11.4 / 8.0 / 10.1 / 6.1 |
| y1-o-INT | A = C: 708 / 1229 / 1487 / 1721 | 0 | 0 | 11.4 / 9.8 / 9.6 / 12.8 | 19.4 / 21.9 / 20.6 / 19.8 | 125.6 / 10.3 / 8.2 / 15.2 |
| x1-s-INT | A = C: 1149 / 1539 / 1732 / 1945 | 0 | 0 | 0.2 / 18.2 / 8.4 / 13.1 | 3.2 / 21.2 / 10.4 / 16.1 | 16.7 / 7.5 / 7.5 / 10.9 |
| y1-s-INT | A = C: 1135 / 1631 / 1920 / 2437 | 0 | 0 | 12.0 / 12.3 / 4.7 / 10.7 | 16.0 / 16.3 / 12.7 / 14.7 | 7.9 / 7.5 / 5.5 / 14.3 |

- **Gate met in every measured kill:** kill -> first commit max 23.2 s (s, SIGKILL; the 162-block rewind),
  restart -> first commit max 22.2 s. Compare p3p (section 42.6): prototype all rows SIGKILL 21.3 s mean /
  26.6 s max restart -> first commit, control 12.1 s. Step 1 SIGKILL: 13.4 s mean / 22.2 s max. Today's path
  (o) SIGKILL: 13.3 / 15.9.
- **What sets the replay:** on o, A trails the pre-kill commit height by the exec queue (rewind = queue,
  16-65 blocks). On s, A is the last checkpoint: a multiple of 100 (the attest-height trigger) in 13 of the 14
  measured KILL/TERM kills; the other (4099) and the excluded kill 3 of x1-s-TERM (2339) are 15 s time-triggered
  checkpoints. The rewind is the blocks since that checkpoint plus the queue: 7-162 blocks, mean 92 (KILL) / 56
  (TERM). The serial replay ran faster per block on s (13.1 vs 5.1 blocks/s mean; not analysed further), so the
  longer rewind costs at most ~8 s (replay max 20.4 s vs 12.8 s).
  Mean restart -> first commit is the same in both arms (13.4 vs 13.3 s); the s maximum is 6 s higher.
- **SIGTERM = SIGKILL on this binary** (down 1.0 s, no shutdown line, same rewind shape), as expected from
  `main.rs`.
- **SIGINT (clean stop):** no replay on either arm (A = C every time; the restarted node attaches at the
  committed height). The node first executes its exec queue and then exits, so down is longer: o 9.0 s mean /
  12.0 max (queue up to 66 blocks executed per block), s 3.9 / 8.0 (smaller queues at the kill; the window
  closes into a final checkpoint, smoke cell: `flush worker thread shutting down` 0.59 s after the signal).
  kill -> first commit: s INT 13.8 s mean, about the same as s KILL (14.4); o INT 19.4 s, longer than o KILL
  (14.3), because the drain sits in front of the exit (as p3p's graceful arms).
- **DB open** is 2.4-3.9 s mean (p3p: control 2.8, all rows 4.2); not higher on s.
- **Rejoin:** 8-13 s mean. Outlier y1-o-INT kill 1 (125.6 s): val1 committed at once but its exec queue stayed
  at ~65 for 2 minutes under load while the others ran near 0. That is execution lag, not a stall.
- **Excluded: x1-s-TERM kills 3 and 4.** From 22:41:23 UTC (23 s after kill 2's restart, before kill 3) the
  chain committed nothing on any validator until 22:50:10 (during the drain), stuck at height 2354. val1 logged
  `Failed to publish consensus message: NoPeersSubscribedToTopic` and `mesh watchdog: validator connected but
  unsubscribed past grace — forcing reconnect`, and block-sync made no progress. This is the gossipsub
  subscription wedge after an in-place restart (memory S395: an instant kill + start leaves the subscriptions
  un-exchanged; seen before on the testnet, independent of the binary). Kills 3 and 4 landed inside the stall,
  so they measure the wedge, not the restart. The wedge signature (watchdog reconnects in val1's log) shows in
  both arms after 20 of the 48 restarts (o 7, s 13; per signal KILL 3, TERM 6, INT 11). This is the only full
  halt: in every other kill window the longest commit gap is <= 37.6 s (`crash-freeze.py`). Cell blocks/s is
  lowest in the INT cells (2.5-4.2, vs 5.9-10.9 in the KILL / TERM cells other than x1-s-TERM). It is a harness /
  network finding (in-place restart pattern), not a Phase 3 one, and it is listed under Open.

### 46.4 Correctness (hard checks): no finding

- **Per kill (all 48):** A <= C; the replay starts at A + 1 and replays exactly A+1..C once; ENABLED
  `applied` = C; A >= the durable hash height sampled before the kill. No flag on any kill. Nothing starts past
  the marker or below it.
- **No divergence, panic or fail-stop:** no `STATE HASH MISMATCH`, `FATAL`, `panicked`, `could not be fully
  replayed`, `replay stopped` or fail-stop line at WARN/ERROR level on any validator in any cell. The only
  matches are the INFO config line (the known rc 2).
- **State hash checkpoints:** every 100-block checkpoint's `localHash` was read on all 3 validators. Equal at
  every height in every cell (28-79 heights per cell, all 3 validators at all but 2 heights; 0 disagreements),
  including the heights val1 re-executed after each restart.
- **End-of-cell RPC digest:** block hashes equal in all 12 cells. Books + open interest (600 lines) and every
  mark-independent account field (cash, native, EVM balance, stake) are equal on the 3 validators in every
  cell. **The DISAGREE in 9 cells is a harness artifact:** only `accountValue`, `freeMargin`, the margin fields and
  `withdrawable` differ, and they split by time. The oracle feed is paused (SIGSTOP) before the digest, and
  oracle prices are usable for 60 s (`DEFAULT_MAX_ORACLE_AGE_SECS`). Every node whose digest ended <= 67.1 s after
  the pause shows mark-based values, and every node that ended >= 70.0 s after it shows the stale-mark values
  (all 36 node-digests fit; the odd node is val0, never restarted, in 3 cells). The clock the age is measured on
  was not checked.
  The digest took 31-246 s here, against ~20 s in p3c1. The control-path cells (o) show it as much as s.
- **Lost or duplicated actions:** none visible. Books, OI and balances are equal, and the state-hash checkpoints
  (which cover every state write) are equal. The `skipping duplicate/replayed native action on live commit
  path` warnings (3,000-3,800 per kill cell) have identical counts on all 3 validators: deterministic
  deduplication of actions the bench resubmitted around the kill. p3p's kill cells show the same.

### 46.5 Reading

- Step 1 at 15 s meets the restart gate: SIGKILL kill -> first commit 14.4 s mean, 23.2 s max (gate 60 s); the
  same mean as today's path (14.3 s), max +6 s from rewinds up to 162 blocks. Better than the prototype in
  p3p (21.3 s mean).
- Crash correctness holds on 48 kills at varied points of the window, with the running state hash on: the
  marker is honoured, the replay is complete and in order, and the validators agree on every checkpoint hash,
  book and balance.
- SIGTERM is a hard kill on this binary; the clean stop is SIGINT. If operators are expected to stop nodes with
  `systemctl stop` (SIGTERM), main.rs should also wait on SIGTERM. That is a node change for the owner, not
  measured here.
- Caveats: one host, three validators on one NVMe; no on-chain state hash attestations (no attest key in the
  devnet); kills at bench-relative times, not at instrumented points (e.g. inside a checkpoint write); the
  gossipsub wedge removed 2 of the 48 kills.
## 47. Phase 3 step 2 (ingress view) vs main, both with the bench in-flight tail on committed bodies (campaign ozarchy-p3s2g, 2026-10-12)

**Step 2 cuts node CPU-s/1M by 7.4% (-9.2% at equal fills) and device writes by 53%. matched/s is a tie
(-2.2%, not resolved) and native blocks/s is 9.4% lower (just resolved). RSS is +1.5 GB peak / +1.1 GB mean
(+35% / +31%), far above the layer-size gate.** Both arms ran the bench in commit-tail mode
(`IN_FLIGHT_TAIL=commit`), so the step 1 / step 2 read lag no longer throttles the load generator. In that mode
**both arms run at an exec ceiling** (exec thread busy 0.96 control, 0.98 step 2). The blocks/s drop seen in the
step 2 smoke (11.9 -> 3.3) **is on the control too** (6.1 in head mode -> 3.5): it is batching, not a per-fill
slowdown. Same fills/s, same per-fill cost, blocks twice as large. The DISAGREE seen in the smoke **also
appears on a control cell**: it is mark-only and comes from the digest timing. The cancel-all estimate
concern is **zero** in this run: no cancel-all failed on chain.

Owner request (18c, s112; plan `item6-phase3-impl.md` step 4): gate the step 2 head (`feat/item6-phase3-step2`
`2296be67`, validator ingress readers on a published view + the RPC submit follow-up) at interval 15 s against
main `c97f56f9`, with the bench in-flight tail released on the committed body (`bench/inflight-commit-tail`
`86d2b2bb`, unmerged), n = 4, the p3c1 shape (section 45, if merged; branch `docs/p3-step1-bench`), shared disk only.

**Confound (reported, not fixed):** step 2 is off `2725e321`, not `c97f56f9`. The control adds 16 commits
(first-parent: `fa875c3d` R18 governance snapshot, `c9ed0ba7` trade-history cutoff read fault fails startup,
`0049c806` + `9ac02c63` docs only, `c97f56f9` CoreWriter undecodable row = failed result; 1,376 / 113 lines in
`crates/`). None of them is on the native order / exec hot path, so they are expected to be perf-neutral. Indirect
check: the commit-mode control here has 25.20 CPU-s/1M and 3.78 engine ms per 1k fills, against 25.31 / 3.80 for
the head-mode `2725e321` control in p3c1. That is a different tail mode, so it is only an indication.

Setup: as section 45 (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s cells,
fresh genesis and data dir per cell, RocksDB `LOG` copied, the 2 s `/metrics` sampler `ckpt-samples.txt`, now
also scraping `torus_exec_cancel_all*`), data dirs on the shared NVMe only. **Binaries** (fresh target dirs,
frame pointers + line tables + mold, one package per build): control node `c97f56f9` (`f35fab7d`), step 2 node
`2296be67` (`1c02893b`), and **one** bench `bench-throughput` from `86d2b2bb` (`3af99b73`), copied beside both
nodes. **Harness** = the `86d2b2bb` worktree (`wt/inflight-commit-tail`): its `tools/matched-bench` differs from
`9b7e29b2` only by the commit-tail commits (`--in-flight-tail commit`, `IN_FLIGHT_TAIL`, `run_cell.py`
allowlist, summary fields). Step 2 adds **no new knob** (`TORUS_CHECKPOINT_INTERVAL_MS=15000` as step 1, same
enable line) and **no new metric**. Arms: **cd** = control, knob unset; **sd** = step 2 + knob. Per-cell hard
checks as section 45 (md5 per arm, env, book CF, codec/WAL, trie, WAL cap, zero trade rows, state hash off, data
dir on btrfs, knob only on sd, enable line only on sd, `torus_exec_checkpoints` >= 1 per sd node, absent on cd,
`TORUS_EXEC_PIPELINE` default on with the `bl2 exec pipeline ENABLED` line on every validator) **plus `tailchk`**:
`in_flight_tail='commit'` in run.log, `--in-flight-tail commit` in the bench command, `tail=commit` in the bench
In-flight line, and summary.json `cell.in_flight_tail == ingest.in_flight.tail_mode == commit`. Every cell
passed it.

Cells 12:30-13:26 (driver `ozarchy-p3s2g-campaign.sh`, analyst `tools/ozarchy-p3s2g-tools/a-*.py`): warm-up w0
(cd, 60 s, excluded), then r1 cd sd, r2 sd cd, r3 cd sd, r4 sd cd (alternating ABBA). Prep smoke
`ozarchy-p3s2g-smoke-300m-s0` (sd, 60 s): all checks OK. 117 CHECK OK, no CHECK FAIL, no hard finding,
`layer_shared` 0 in every sample, no deaths. No `cargo`, `rustc` or `rust-analyzer` ran before any cell. Every
cell rc 0, ACCEPT, except **r3-cd: rc 2, REJECT on agreement (mark-only, 47.2)**. Its throughput data is valid
and is kept in the tables (n = 4). Dropping it gives the "n = 3" figures noted below.

### 47.1 Q1: is the blocks/s drop on the control too? Batching or exec ceiling?

Per arm (n = 4, mean; commit interval and exec wall per block from the node logs, all 3 validators, bench window;
exec busy from `phase_by_node.exec_thread_busy_fraction`):

| | cd control `c97f56f9` | sd step 2 `2296be67` | sd vs cd [95% CI] |
|---|---|---|---|
| native blocks/s | 3.49 | 3.16 | -9.4% [-18.6, -0.2] (n = 3 cd: -6.7% [-14.9, +1.4]) |
| commit interval p50 / p95 / p99 ms (log) | 140 / 814 / 1,099 | 174 / 828 / 1,303 | p99 +18.5% [+9.6, +27.4] |
| commit interval p50 / p95 ms (summary, histogram) | 91 / 1,262 | 81 / 1,391 | |
| exec chain_ms per native block | 257.7 | 281.2 | +9.1% [+0.3, +18.0] |
| exec wall per block (log) mean / p50 / p99 ms | 334 / 350 / 644 | 370 / 376 / 700 | mean +10.8% [+4.4, +17.2] |
| exec thread busy | 0.957 | 0.982 | |
| exec lag commit -> done, mean (blocks behind at bench end) | 14.5 s (~63) | 15.8 s (~63) | |
| actions / fills per native block | 290 / 55,684 | 297 / 57,320 | +2.6% / +2.9% |
| matched/s | 199,667 | 195,333 | -2.2% [-7.1, +2.7] |
| submit/s | 1,272 | 1,220 | -4.1% [-9.5, +1.2] |
| submit-stall s per 120 s cell | 2.2 | 1.9 | |
| engine ms per 1k fills | 3.78 | 3.47 | -8.2% [-13.5, -2.9] |
| exec wall per 1k fills (log) | 6.00 ms | 6.46 ms | +7.6% [+0.7, +14.5] |
| node CPU per fill (CPU-s/1M = CPU-ms per 1k fills) | 25.20 | 23.32 | -7.4% [-9.5, -5.4] |

Head mode vs commit mode (the head-mode rows are p3c1, section 45, and the ict smokes; different binaries for the control):

| set | exec busy | chain_ms | actions/blk | fills/blk | engine ms/1k | CPU-s/1M | matched/s | native blk/s | submit/s | stall s/cell |
|---|---|---|---|---|---|---|---|---|---|---|
| control `2725e321`, head (p3c1 cd) | 0.953 | 124.1 | 141 | 25,984 | 3.80 | 25.31 | 198,992 | 6.14 | 1,134 | 2.2 |
| control `c97f56f9`, **commit** (cd) | 0.957 | 257.7 | 290 | 55,684 | 3.78 | 25.20 | 199,667 | 3.49 | 1,272 | 2.2 |
| step 1, head (p3c1 sd) | 0.895 | 63.8 | 69 | 13,094 | 3.45 | 23.31 | 185,121 | 12.03 | 982 | 36.9 |
| step 2 `12e11c7e`, head (ict smoke, 1 cell) | 0.905 | 66.4 | 71 | 13,564 | 3.37 | 23.16 | 185,220 | 11.88 | 963 | 33.5 |
| step 2 `12e11c7e`, commit (ict smoke, 1 cell) | 0.989 | 272.5 | 301 | 57,888 | 3.34 | 22.88 | 206,561 | 3.25 | 1,268 | 2.0 |
| step 2 `2296be67`, **commit** (sd) | 0.982 | 281.2 | 297 | 57,320 | 3.47 | 23.32 | 195,333 | 3.16 | 1,220 | 1.9 |

**Answer: yes, the control drops too (6.14 -> 3.49 native blk/s), and it is batching under an exec ceiling.**
The control's exec thread was already at ~0.95 busy in head mode. In commit mode the bench releases a slot when
the block commits, not when it has executed. Consensus runs ~15 s (~63 blocks) ahead of exec (the commit-lag
backoff), the mempool holds ~5-6k actions (val0 mean, both arms), and each native block carries twice the
actions. Fills/s, engine ms per 1k fills (3.78 vs 3.80) and CPU per fill (25.20 vs 25.31) do not move. The
step 2 head-mode 11.9 blk/s was the inverse artifact: `eth_blockNumber` moves in checkpoint steps, the bench
stalled ~34 s per cell, and blocks were half size.

**Step 2 vs control inside commit mode:** the exec thread spends +36 ms more per block (+10.8%) while its
engine time is lower (-11.6 ms per block). The extra time is `residual_untimed`: 14.0 -> 56.0 ms per block,
i.e. 4.8 -> 18.8 ms per 100 actions. Step 1 in head mode had the same ratio (19.1 vs 5.3 ms per 100 actions
in p3c1), so this is **Phase 3 exec-thread work that the timers do not cover (step 1 and step 2 alike)**, scaling
with block size. Step 2's view publication adds nothing measurable on top. Net: exec wall per fill +7.6%, so
-9.4% blocks/s and -2.2% matched/s (not resolved). The CPU saving (-7.4%) comes from the flush (state write 72 ->
0.6 ms per block, off the exec thread).

### 47.2 Q2: does the digest DISAGREE appear on control cells?

**Yes, once: r3-cd (control), val1. Every difference in every DISAGREE cell is mark-based. No cash, position,
book or OI field differs anywhere. No hard finding.**

| cell | verdict | differing node | fields | digest wall s (val0/1/2) | digest end, s after the oracle pause (val0/1/2) | accounts with accountValue == cash (val0/1/2) |
|---|---|---|---|---|---|---|
| r1-cd | AGREE | - | - | 21.7 / 21.5 / 21.7 | 55 / 54 / 55 | 0 / 0 / 0 |
| r1-sd | AGREE | - | - | 89.5 / 92.8 / 92.8 | 122 / 125 / 125 | 49 / 49 / 49 |
| r2-sd | AGREE | - | - | 87.4 / 88.2 / 92.1 | 120 / 121 / 125 | 37 / 37 / 37 |
| r2-cd | AGREE | - | - | 23.1 / 23.0 / 22.7 | 58 / 58 / 58 | 0 / 0 / 0 |
| **r3-cd** | **DISAGREE** | **val1** (vs val0 = val2) | accountValue, freeMargin, maintenanceMarginUsed, totalMarginUsed, withdrawable on all 50 accounts; mark-only | 33.8 / **37.6** / 34.1 | 69 / **73** / 69 | 0 / **50** / 0 |
| r3-sd | AGREE | - | - | 22.1 / 22.2 / 22.5 | 55 / 55 / 56 | 0 / 0 / 0 |
| r4-sd | AGREE | - | - | 82.6 / 85.1 / 82.9 | 115 / 117 / 115 | 43 / 43 / 43 |
| r4-cd | AGREE | - | - | 21.4 / 21.8 / 20.2 | 54 / 55 / 53 | 0 / 0 / 0 |
| ict smoke commit (step 2) | DISAGREE | val2 (vs val0 = val1) | the same 5 mark fields, 50 accounts | 80.8 / 83.9 / **21.5** | 113 / 116 / **54** | 49 / 49 / **0** |
| ict smoke head (step 2) | DISAGREE | val2 | the same 5 mark fields, 50 accounts | 19.6 / 20.5 / **93.8** | 34 / 34 / **108** | 0 / 0 / **35** |

Mechanism: the harness pauses the oracle feed (SIGSTOP) at bench end and starts the digest after the drain,
about 27-35 s later. The 50 accounts are read last. A node whose account reads finish more than about 60-70 s
after the pause values positions with the mark treated as too old (accountValue == cashBalance, unrealized PnL
0). A node that finishes earlier uses the live mark. The 60 s oracle price age falls in that window, counted from
the last accepted price, which is before the pause. A cell disagrees only when that cutoff falls between the
nodes' finish times: r3-cd 69 / 73 / 69 s, and both smokes. Whether a node is slow is not tied to the arm or the
node: digests take ~21 s or ~85-93 s (w0 cd 67 s, r3-sd 22 s, the slow node in the smokes was val2 in one and
val0/val1 in the other). The digest heights differ by 1 between nodes in every cell, AGREE cells included, so the
height alone is not the cause. This is a harness artifact (digest after the oracle stops). A fix on the bench
side (keep the feed running through the digest, or digest the cash/position fields only) is a bench-branch
question.

### 47.3 Q3: cancel-all estimate in commit mode

| arm | cancel-alls sent | executed (`torus_exec_cancel_all_total` delta, each node) | failed on chain (`torus_exec_action_failures_total` delta, every node) | not executed: mempool nonce-expired evictions | RPC-refused (sent - accepted) | `orders_rejected_open_limit` delta |
|---|---|---|---|---|---|---|
| cd (4 cells) | 228,009 | 228,006 | **0** | 0 | 3 | 0 |
| sd (4 cells) | 219,044 | 216,717 | **0** | 2,327 (r2 1,351, r3 433, r4 543, r1 0) | 0 | 0 |

**Negligible: 0 of 447,053 cancel-alls failed on chain** (also by construction: `exec_cancel_all` returns ok
unconditionally at `2296be67`; a pre-dispatch failure would be a `Failed` status and counted in
`torus_exec_action_failures`, which stayed 0 on all 3 nodes in all 8 cells). So no failed cancel-all could
reset a sender's open-order estimate. Budget effect: **0 open-limit rejections** in every cell. Place
rejections (all `Rejected` outcomes, IOC zero-fill and book refusals) are 377 (cd) vs 399 (sd) per cell per node.

**New, step 2 only (not a node fault, but worth the owner's eye):** in 3 of the 4 sd cells, 0.8-2.6% of the
cancel-alls (433-1,351 per cell, identical on the 3 nodes) aged past the 60 s nonce window in the mempool and
were evicted at bench + 110-116 s, never executed. Control: 0 in every cell. Every eviction is a cancel-all
(sent - executed = evicted, exactly). Likely cause: the proposer gives cancels at most 25% of a block ahead of
other actions (`native_cancel_block_share_pct`, item C), the bench mix is 36.7% cancel-alls, blocks are full,
and the step 2 arm commits fewer blocks per second, so the leftover cancels wait longer. For the bench, an
evicted cancel-all is released by timeout. In `in_flight.rs` only a **committed** cancel-all resets the
estimate, so the estimate stays high (the conservative side) and no budget overrun follows.

### 47.4 Gate table (shared NVMe, commit-tail mode, n = 4 per arm)

| gate | cd control | sd step 2 | sd vs cd | gate |
|---|---|---|---|---|
| matched/s ↑ | 199,667 (sd 2,622) | 195,333 (sd 6,560) | -2.2% [-7.1, +2.7]; n = 3 cd -1.9% [-7.0, +3.2] | up: **not met** (tie) |
| native blocks/s | 3.49 (0.20) | 3.16 (0.17) | -9.4% [-18.6, -0.2]; n = 3 cd -6.7% [-14.9, +1.4] | no regression: **not met** (borderline) |
| node CPU-s/1M ↓ (whole run) | 25.20 (0.28) | 23.32 (0.31) | **-7.4%** [-9.5, -5.4] | met |
| CPU-s/node at equal fills | 753.5 | 668.1 | **-9.2%** [-10.4, -8.0] (slope 14.0 CPU-s per 1M fills) | met |
| device writes MB/s per val (bench window) | 140.5 | 65.9 | **-53%** [-58, -48]; 17.2 -> 8.1 GB per val per cell | met |
| wchar MB/s per val | 156.4 | 81.9 | -48% | |
| RSS per node, peak GB | 4.41 | 5.93 | **+1.53 GB (+35%)** | layer figure + 25%: **not met** |
| RSS per node, mean GB (whole cell) | 3.69 | 4.82 | +1.13 GB (+31%) | |

RSS: the gate is "RSS growth within archy's layer-size figure + 25%". Step 0 measured a 214 MB layer peak at 15 s
(section 41), so the bound is ~270 MB. The 18c estimate for the step 2 view was ~150-200 MB per 15 s window.
Measured: **+1.5 GB peak**. In p3c1, step 1 in head mode was +0.15 GB. RSS climbs through the cell in both arms
(val0, r1: cd 3.2 / 3.5 / 3.8 / 4.0 GB at 30 / 60 / 90 / 120 s; sd 4.2 / 5.1 / 5.4 / 5.7 GB). Commit mode itself
adds ~0.8 GB on the control (4.41 vs 3.61 in head mode), from the ~63-block exec queue and the larger mempool. The
step 1 layer vs step 2 view split **is not measured here**: it would need a step-1-only commit-mode arm or a heap
profile.

### 47.5 Checkpoint metrics (sd; control exports none)

| cell | checkpoints per node | `layer_shared` | E wait per checkpoint (`exec_checkpoint_wait`) | layer bytes mean | flush-worker checkpoint wall mean |
|---|---|---|---|---|---|
| r1-sd | 16 | 0 | 0.02 ms | 103 MB | 0.45 s |
| r2-sd | 14 | 0 | 0.02 ms | 106 MB | 0.43 s |
| r3-sd | 15 | 0 | 0.03 ms | 114 MB | 0.43 s |
| r4-sd | 15 | 0 | 0.07 ms | 109 MB | 0.45 s |

Histograms (`metrics-after`, whole cell incl. drain, 17-19 checkpoints per node; bucket upper bounds):
checkpoint wall p50 <= 0.26-0.51 s, p99 <= 1.0-2.0 s; E wait p50 <= 0.5 ms, p99 <= 0.5-4 ms; layer bytes p50 <=
128 MiB, p99 <= 256 MiB. In p3c1 (head mode) the layer mean was 80 MB at ~20 checkpoints per cell. Here blocks
are twice as large and the interval, not h % 100, drives most checkpoints. Step 2 adds no metric (the view has no
counter or gauge). Exec pipelining: 98.8% (cd) / 99.2% (sd) of native blocks ran `pipelined=true`.

### 47.6 Rejections and warnings (per cell, per node)

Place rejections (`torus_exec_action_rejections`) cd 292-470, sd 299-647. Action failures 0. Open-limit
rejections 0. In-flight `released_refused` cd 0-4, sd 0-1. `released_timeout` cd 0, sd 0 / 40 / 433 / 543 (the
evictions above). Tail errors 0, missed 0. Duplicate-skip WARN on the live commit path: cd 0-4, sd 0-7. The other
node WARN lines (16-45 per node) are the known "active validator set below BFT minimum" (3 validators) and the
deprecated `--validator-key` line. No ERROR and no panic line; the one "fail-stop" match per node is the INFO config
line `running state hash fail-stop ... on=false`.

### 47.7 Caveats

- Commit-tail mode is not the mode of sections 42-45. Absolute blocks/s and chain_ms are not comparable with
  head-mode rows (blocks are twice as large). matched/s, CPU per fill and engine ms per 1k fills are comparable.
- The control is main `c97f56f9`, not the step 2 base `2725e321` (16 non-Phase-3 commits, expected neutral; see above).
- The exec thread is saturated in both arms. These are capacity numbers at the exec ceiling, not at a fixed
  offered load. The step 2 arm offers 4% less load (-4.1% submit/s, not resolved) because each block takes longer.
- r3-cd is REJECT on agreement (mark-only, harness timing). It is kept: dropping it moves the cd means by
  < 1% (matched/s) and the blocks/s gap to -6.7% (not resolved).
- One host, one NVMe, 3 validators sharing it, as in every ozarchy campaign.

## 48. Phase 3 step 2 RSS split: commit-mode exec backlog, step 1 layer, step 2 view (campaign ozarchy-p3rss, 2026-10-12)

**Step 2's +1.55 GB RSS peak over the control splits into three parts of similar size: about +0.83 GB
from the commit-mode exec backlog (control head mode -> commit mode), +0.88 GB from the step 1
since-checkpoint layer, and +0.68 GB from the step 2 view. Only the last two are Phase 3. Phase 3 alone
is +1.55 GB peak / +1.12 GB mean over the commit-mode control.** That is far above the ~270 MB gate bound.
A heap profile (jemalloc, below) finds the live data behind the Phase 3 part: the layer `L` (316-406 MB
live per node, against a 147 MB logical layer per checkpoint), the step 2 ingress slices (260 MB), and
RocksDB memtables (+60 MB). Step 1 also frees the 157 MB of frozen pending sets the control's pipeline
holds. About half of each Phase 3 RSS delta appears only under glibc (allocator retention): under jemalloc,
the same three steps add +0.51 / +0.45 / +0.33 GB peak. RSS still grows at the end of every 120 s cell
(0.7-1.9 GB per minute), so all peaks depend on the cell length.

Owner request (18c, s112; the Open entry for section 47, item (a)): split step 2's +1.5 GB RSS (section 47:
control 4.41 / 3.69 GB peak / mean, step 2 5.93 / 4.82) into the step 1 layer, the step 2 view and the
commit-mode exec backlog, with RSS time series, queue depth and mempool size, and a no-sudo heap profile
of control vs step 1 vs step 2.

Setup: the section 47 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900,
120 s cells, fresh genesis and data dir per cell, shared NVMe). Harness and **one** bench `bench-throughput`
(`05d44313`) from origin/main `8efcb07a` (worktree `wt/p3rss-8efcb07a`; it has `IN_FLIGHT_TAIL`).
**Nodes** (frame pointers + line tables + mold): control `2725e321` (`18fb490d`, the p3c1 build), step 1
`a44bdde6` (`7d1a0874`, the p3r1 build), step 2 `2296be67` (`1c02893b`, the p3s2g build), and main `8efcb07a`
(`09827336`, built here). md5s were checked against each build record. **The split is clean:**
`2725e321` = merge-base(step 1, main), step 1 = control + 13 commits, step 2 = step 1 + 5 commits (a linear
chain). Arms: **cth** = control, `IN_FLIGHT_TAIL=head`; **ctl** = control, commit; **s1** = step 1, commit,
`TORUS_CHECKPOINT_INTERVAL_MS=15000`; **s2** = step 2, commit, 15000; **mn** = main `8efcb07a`, commit (2 cells,
cross-check only: main adds CoreWriter results, RPC and action_status changes, so it is not the clean control).
Heap-profiled arms **hcth / hctl / hs1 / hs2** are the same nodes with jemalloc preloaded (below). Cells
15:02-17:43 (driver `ozarchy-p3rss-campaign.sh`, CAMP=A): w0 (ctl, 60 s, excluded), r1-r4 in a rotated order
(r1 cth ctl s1 s2, r2 ctl s1 s2 cth, r3 s1 s2 cth ctl, r4 s2 cth ctl s1), then m1 h1-ctl h1-s1 h1-s2 after r2 and
m2 h2-s2 h2-s1 h2-ctl h1-cth after r4. **All 26 cells rc 0, ACCEPT, AGREE**, 364 CHECK OK and no CHECK FAIL.
The checks were those of section 47 plus a per-arm tail-mode check (head or commit, in run.log, the bench
command, bench.log and summary.json), knob, enable line and `torus_exec_checkpoints` on the s arms only, and
`heapchk` (jemalloc preloaded and mapped on all 3 nodes of the h arms, on none elsewhere). No cargo, rustc
or rust-analyzer ran before any cell, there were no deaths, and no node swapped (VmSwap 0 in every sample;
the host had 5.6 GB of older swap in use).

Memory sampling (every cell, `memsamp`): per node every 5 s `/proc/<pid>/smaps_rollup` + `/proc/<pid>/status`
(Rss, anon / file, swap, HWM, threads) plus the memory / queue gauges from `/metrics` (memtables, block cache,
mempool size, exec queue depth, flush-worker depth, resident rows, resting orders, layer bytes), and val0's
full `smaps` every 30 s. RSS peak / mean in the tables = the harness `cpu.csv` definition of section 47
(per node over the whole cell, mean of the 3 nodes); the window rows use the 5 s samples in the bench window.

**Heap profiler without sudo:** no heaptrack / bytehound / valgrind, the node has no jemalloc, and
`ptrace_scope` = 1 (no gdb attach). The system `libjemalloc.so.2` (5.3.1) is built with profiling and
`jeprof` is installed, so a wrapper `torus-node` (stage `<key>-heap`) sets `LD_PRELOAD` +
`MALLOC_CONF=prof:true,lg_prof_sample:19,lg_prof_interval:33` and execs the real binary (same pid, same
exe md5, comm `torus-node`). Each node writes a sampled heap dump every 8 GiB allocated (~every 4 s, ~60
per node per cell). `jeprof` truncates Rust names, so `a-heap.py` parses the heap_v2 dumps itself
(jeprof's unbiasing, readelf LOAD segments for PIE addresses, `llvm-symbolizer --inlining`). `a-heapcat.py`
then sorts every stack into components by frame regex (first match wins: view slices, `CheckpointLayer`,
`FrozenPending`, `torus_mempool`, decoded actions / blocks, order books, exec working set, resident positions /
rows, RocksDB memtables / block cache / other, overlay, network). The figures are live bytes per node,
averaged over the last 3 dumps at or before the bench end on all 3 nodes. Caveat: jemalloc is a different
allocator, the h cells ran 3.5-3.8 native blk/s, with blocks 7-10% smaller than the glibc cells of the same arm (hcth 14% larger) and n is 2
(1 for hcth).

### 48.1 RSS per arm and the three deltas (glibc, n = 4)

| | cth control head | ctl control commit | s1 step 1 | s2 step 2 | mn main (n = 2) |
|---|---|---|---|---|---|
| RSS peak / mean GB (cpu.csv, section 47 definition) | 3.56 / 3.17 | 4.38 / 3.64 | 5.26 / 4.27 | 5.94 / 4.76 | 4.38 / 3.67 |
| bench window: peak / mean / last 15 s GB (5 s samples) | 3.56 / 3.21 / 3.53 | 4.29 / 3.60 / 4.25 | 5.10 / 4.14 / 5.07 | 5.76 / 4.59 / 5.72 | 4.33 / 3.68 / 4.31 |
| RSS growth over the bench window, GB per minute | 0.73 | 1.07 | 1.57 | 1.86 | 1.00 |
| VmHWM GB | 3.83 | 4.56 | 5.50 | 6.11 | 4.60 |
| anon / file-backed GB (window mean) | 3.18 / 0.03 | 3.57 / 0.03 | 4.11 / 0.03 | 4.56 / 0.03 | 3.65 / 0.03 |
| matched/s | 200,946 | 195,633 | 199,313 | 198,030 | 192,625 |
| native blocks/s | 6.00 | 3.52 | 3.29 | 3.21 | 3.45 |
| fills per native block | 26,298 | 54,001 | 56,060 | 57,008 | 53,867 |
| exec backlog, committed - executed, mean (blocks) | 49.8 | 54.6 | 56.9 | 56.7 | 54.0 |
| exec queue depth, mean (gauge) | 51.1 | 56.2 | 58.5 | 57.4 | 56.6 |
| exec lag commit -> done, s | 7.9 | 14.1 | 15.5 | 15.9 | 14.6 |
| mempool native size, mean / max | 1,328 / 5,361 | 5,418 / 11,012 | 5,742 / 11,558 | 5,870 / 11,613 | 5,660 / 11,521 |
| resting orders (exec gauge, mean) | 179,510 | 229,374 | 267,999 | 263,153 | 246,992 |
| RocksDB memtables MB (mean) | 288 | 275 | 219 | 222 | 275 |
| layer bytes per checkpoint MB (`torus_exec_checkpoint_layer_bytes`) | - | - | 147 | 147 | - |
| node CPU-s/1M | 25.08 | 25.35 | 23.37 | 23.34 | 25.64 |

The val0 `smaps` snapshots (bench window, peak snapshot) put 86-91% of RSS in anonymous mappings of 32 MiB
or more (glibc arenas), 4-11% in smaller anonymous mappings (thread stacks, small mmaps; s2 617 MB at its
peak snapshot), 3-4% in `[heap]`, and 28 MB in the binary's file pages. There are no file-backed data mappings.
So RSS here is heap.

| delta (glibc, n = 4 vs 4) | RSS peak GB [95% CI] | RSS mean GB [95% CI] | what moved with it |
|---|---|---|---|
| **backlog** = ctl - cth | **+0.83** [+0.73, +0.92] | **+0.47** [+0.40, +0.53] | mempool 1.3k -> 5.4k actions (4.1x), blocks twice as large (26k -> 54k fills) with ~5 more of them queued (49.8 -> 54.6, gauge 51 -> 56), resting orders +28% |
| **layer** = s1 - ctl | **+0.88** [+0.76, +1.00] | **+0.63** [+0.56, +0.70] | layer 147 MB logical per checkpoint, memtables -56 MB, backlog +2.3 blocks, resting orders +17% |
| **view** = s2 - s1 | **+0.68** [+0.54, +0.82] | **+0.49** [+0.40, +0.58] | nothing else moves (backlog, mempool, layer bytes, memtables, CPU all within noise) |
| s2 - ctl (Phase 3 total) | +1.55 [+1.43, +1.68] | +1.12 [+1.03, +1.21] | section 47 (vs main `c97f56f9`): +1.53 / +1.13 |
| mn - ctl (main vs control) | +0.00 [-0.06, +0.06] | +0.03 [-0.01, +0.07] | main's extra commits do not move RSS; section 47's control is equivalent for RSS |

### 48.2 Heap profile: live heap by component (jemalloc, MB per node, mean of the last 3 dumps <= bench end)

| component | hcth (n = 1) | hctl (n = 2) | hs1 (n = 2) | hs2 (n = 2) | backlog hctl - hcth | layer hs1 - hctl | view hs2 - hs1 |
|---|---|---|---|---|---|---|---|
| step 2 view slices (`FrozenPending::slice`, read view) | 0 | 0 | 0 | 260 | 0 | 0 | **+260** |
| step 1 layer `L` (`CheckpointLayer::merge / insert`) | 0 | 0 | 316 | 406 | 0 | **+316** | +90 |
| frozen pending sets (pipeline hand-off to the flush worker) | 163 | 157 | 0 | 1 | -6 | **-157** | +1 |
| mempool (`torus_mempool`: actions held for proposal) | 381 | 724 | 761 | 743 | **+343** | +37 | -18 |
| decoded actions / committed blocks / exec queue | 155 | 381 | 451 | 385 | **+226** | +70 | -66 |
| order books (resident + save buffers) | 333 | 379 | 347 | 404 | +46 | -32 | +57 |
| resident positions / rows (`TraderPositions`, `ResidentRows`) | 644 | 722 | 724 | 687 | +78 | +2 | -37 |
| exec working set (`MatchResult`) | 10 | 31 | 15 | 16 | +21 | -16 | +1 |
| RocksDB memtables | 92 | 85 | 111 | 145 | -7 | +26 | +34 |
| RocksDB block cache / reads + other | 15 | 16 | 11 | 9 | +1 | -5 | -2 |
| state overlay, network / RPC, other | 96 | 103 | 112 | 116 | +7 | +9 | +4 |
| **live heap total** | **1,889** | **2,597** | **2,847** | **3,171** | **+708** | **+250** | **+324** |
| RSS peak under jemalloc GB (cpu.csv) | 3.66 | 4.17 | 4.62 | 4.94 | +0.51 | +0.45 | +0.33 |
| RSS peak under glibc GB (48.1) | 3.56 | 4.38 | 5.26 | 5.94 | +0.83 | +0.88 | +0.68 |

Reading:
- **Backlog (not Phase 3):** in commit mode the bench keeps 4.1x more actions in the mempool, and each block
  carries twice the actions, with about the same number of blocks queued between commit and execution. The
  live heap grows by +0.71 GB, about half in the mempool (+343 MB) and a third in decoded committed blocks
  waiting for execution (+226 MB). The rest is larger state (resting orders +28%). This part comes from the
  bench's in-flight release, not from the node.
- **Layer (step 1):** `L` holds 316 MB live for a 147 MB logical layer (2.1x: hash map and key/value
  allocation overhead). Step 1 also drops the 157 MB of frozen pending sets the control holds in the flush
  hand-off, so live heap rises only +250 MB, while glibc RSS rises +0.88 GB. The difference is allocator
  retention: `L` is rebuilt and freed every checkpoint (15 s), and its frees land in glibc arenas that are
  not returned (jemalloc: +0.45 GB).
- **View (step 2):** the ingress slices hold 260 MB live, the `CheckpointLayer` frames hold 90 MB more
  (cause not separated), and memtables are +34 MB. Live +324 MB,
  glibc RSS +0.68 GB.
- **Allocator:** under glibc, step 2 peaks at 5.94 GB; under jemalloc at 4.94 GB (-1.0 GB), while the
  control moves only 4.38 -> 4.17. For both Phase 3 parts the jemalloc RSS delta is about half the glibc
  one (layer 0.45 vs 0.88 GB, view 0.33 vs 0.68 GB). This contrasts two allocators on n = 2 cells and is
  not a proposal. A jemalloc or `MALLOC_ARENA_MAX` / trim A/B would be its own bench.
- **Growth:** every arm is still growing at the bench end (cth 0.73 -> s2 1.86 GB per minute), and the
  Phase 3 arms grow faster (s2 - ctl +0.79 GB/min [+0.59, +0.99]). The 120 s peaks are not steady-state
  values. A longer cell is needed for the plateau (or to show that there is none).

### 48.3 Attribution and confidence

| component of s2 - cth (+2.38 GB peak, glibc) | GB peak (mean) | inside it (heap profile, live MB) | confidence |
|---|---|---|---|
| commit-mode exec backlog (bench, not Phase 3) | +0.83 (+0.47) | mempool +343, decoded blocks / exec queue +226, larger state +124 | **high** for the size (n = 4 vs 4, CI ±0.1 GB); **medium** for the split inside it (heap n = 2 vs 1, jemalloc) |
| step 1 layer | +0.88 (+0.63) | `L` +316, minus frozen pending -157, decoded blocks +70, memtables +26; ~0.4 GB allocator retention | **high** for the size; **medium** for the live part; **low-medium** for the retention share (inferred from the jemalloc contrast) |
| step 2 view | +0.68 (+0.49) | slices +260, `L` +90, memtables +34; ~0.35 GB allocator retention | **high** for the size; **medium** for the live part; **low-medium** for the retention share |
| main vs control | +0.00 | - | high (n = 2) |

Other numbers, for reference (step 1 / step 2 vs control, commit mode): node CPU-s/1M -7.8% / -8.0% (at equal
fills -7.7% / -8.3%), device writes -51% / -50%, matched/s +1.9% / +1.2% (ties), native blocks/s -6.7% / -8.8%,
the `residual_untimed` exec time 14.0 -> 47.8 -> 55.4 ms per block (the section 47 Phase 3 residual; s2 - s1 +7.6
ms), commit interval p99 +15% / +14%. Step 1 shows the same cancel-all nonce-window evictions as step 2 in
section 47 (r1-s1 391 and r3-s1 57-71 per node, r3-s2 440; zero on every control cell).

**Verdict:** the RSS gate (~270 MB) misses by ~1.3 GB peak against the clean commit-mode control: +0.88 GB
from the layer and +0.68 GB from the view. Of that, 0.57 GB is more live data, the jemalloc RSS rise is
0.78 GB, and the other ~0.78 GB appears only under glibc (retention).
The +0.83 GB backlog belongs to the commit-tail bench mode and is in the control too. Levers, by size: allocator
retention around the per-checkpoint rebuild of `L` and the slices (~0.8 GB, glibc only), the layer's 2.1x in-memory
overhead over its logical bytes (~170 MB), and the view's slices (260 MB). The gate rerun on step 2 `51a67f57` (frees
off the exec thread) follows in section 49.

Analysis scripts (`tools/ozarchy-p3rss-tools/analyst/`): `a-cells.py` (per cell, incl. `memsamp` parsing and
the exec backlog from the node logs), `a-arms.py` (Welch 95% CI, ANCOVA at equal fills), `a-smaps.py` (val0 RSS
by mapping class), `a-heap.py` + `a-heapcat.py` (heap profile). Outputs in
`ozarchy-p3rss-run/analyst/` on ozarchy (`cells.json`, `arms.txt`, `heapcat.txt`, `smaps.txt`).

## 49. Phase 3 gate rerun: step 1 `b1605417` and step 2 `51a67f57` vs main `8efcb07a`, commit-tail mode, with the checkpoint timers (campaign ozarchy-p3g2, 2026-10-12)

**Step 2 `51a67f57` vs a clean main control: node CPU-s/1M -7.0% (-6.8% at equal fills), device writes -51%,
matched/s +2.9% [-3.1, +8.9] (tie), native blocks/s -5.5% [-10.4, -0.7] (just resolved), RSS +1.55 GB
peak / +1.13 GB mean (gate ~270 MB: not met, unchanged from sections 47-48).** The untimed exec residual of
section 47 is now mostly timed: `checkpoint_settle_wait` (the exec thread joining the previous block's
merge into `L`) is 23.9 ms per native block and `checkpoint_upkeep` is 1.0 ms. What is left over the control
is +3.5 ms (residual 16.9 vs 13.4 ms). The layer frees now run on the `torus-ckpt-drop` thread, 5.0-5.5 CPU-s
per cell on val0. The cancel-all expiry is down to 0-27 per node per cell. All 13 measured cells ACCEPT,
except the heap-profiled h1-g2 (mark-only DISAGREE, the known slow-digest artifact).

Owner request (18c, s112): rerun the gate on the new heads, step 1 `b1605417` (`feat/item6-phase3-step1`, main
`8efcb07a` merged in) and step 2 `51a67f57` (`feat/item6-phase3-step2`: step 1 + the exec-thread checkpoint
timers `7fe4d420`, the layer merge handing `L` back first with releases freed off the exec thread `97cf1d07`,
the dropper queue cap `7938dbec`, an ignored cancel-all expiry repro and docs), against main `8efcb07a`.
merge-base(each, origin/main) = `8efcb07a`, so **the control is clean this time** (section 47's 16-commit
confound is gone). Step 2 head `6d9b78af` adds only the `torus_exec_checkpoint_drop_inline` counter and docs, so
these results hold for it.

Setup: the section 47 / 48 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s
cells, commit tail, shared NVMe). Nodes (fresh target dirs, frame pointers + line tables + mold): main `8efcb07a`
(`09827336`, the section 48 build), step 1 `b1605417` (`c2cfb1c9`), step 2 `51a67f57` (`448bd3ac`). One bench
`05d44313` (section 48). **Harness `bench/summarize-ckpt-timers` `218ad251`** (now merged as `b648bbf2`): `summarize.py` adds
`checkpoint_settle_wait` and `checkpoint_upkeep` (sub `checkpoint_wait`) as exec-thread phases, subtracted from
`residual_untimed`, with per-observation p50 / p99 from their buckets; `run-cell.sh` samples them (test
`main_checkpoint_timers`, RED on main; harness pytest 171 passed). Step 1 `b1605417` does not have the new timers (its
Phase 3 time stays in `residual_untimed`). Cells 17:52-19:17 (driver `ozarchy-p3g2-campaign.sh`): w0 (main, 60 s,
excluded), r1 gm g1 g2, r2 g1 g2 gm, r3 g2 gm g1, r4 g2 g1 gm (n = 4 per arm), then h1-g2: step 2 with the section 48
jemalloc heap profiler, for the free-off-exec-thread check. 182 CHECK OK, no CHECK FAIL, no deaths, the host quiet before
every cell. All 12 throughput cells rc 0, ACCEPT, AGREE. h1-g2 rc 2, REJECT on agreement: val2 differs from val0 = val1 in
the 5 mark fields of all 50 accounts only (`accountValue == cash` on 22). val2's digest took 47.7 s vs 19-20 s on the others,
which is the slow-digest mark artifact of sections 46-47. No cash, position, book or OI field differs: no hard finding.

### 49.1 Gate table (n = 4 per arm, mean; 95% CI vs main)

| | gm main `8efcb07a` | g1 step 1 `b1605417` | g2 step 2 `51a67f57` | g1 vs gm | g2 vs gm |
|---|---|---|---|---|---|
| matched/s | 195,884 | 201,862 | 201,533 | +3.1% [-2.3, +8.4] | **+2.9% [-3.1, +8.9]** |
| native blocks/s | 3.56 | 3.31 | 3.37 | -7.2% [-12.4, -2.1] | **-5.5% [-10.4, -0.7]** |
| node CPU-s/1M (whole run) | 25.33 | 23.13 | 23.56 | -8.7% [-10.7, -6.7] | **-7.0% [-9.1, -4.9]** |
| node CPU at equal fills | | | | -8.6% [-9.7, -7.5] | **-6.8% [-8.1, -5.5]** |
| device writes MB/s per validator | 137.9 | 69.4 | 67.8 | -50% | **-51% [-53, -48]** |
| RSS peak / mean GB (cpu.csv) | 4.40 / 3.67 | 5.16 / 4.23 | 5.95 / 4.80 | +0.76 [+0.64, +0.88] / +0.55 | **+1.55 [+1.40, +1.71] / +1.13 [+1.03, +1.23]** |
| RSS growth over the bench window GB/min | 1.15 | 1.44 | 1.78 | +0.29 | +0.63 [+0.49, +0.77] |
| exec per native block (logs) mean / p50 / p99 ms | 342 / 355 / 654 | 353 / 363 / 679 | 348 / 360 / 662 | p50 +2.1% [-4.0, +8.2] | p50 +1.3% [-5.1, +7.8], p99 +1.1% [-7.9, +10.1] |
| block time (commit interval, logs) p50 / p99 ms | 147 / 1,074 | 141 / 1,239 | 164 / 1,211 | p99 +15.3% [+11.2, +19.3] | p99 +12.7% [+9.5, +15.9] |
| exec block_ms (summary) | 251.6 | 270.4 | 265.9 | +7.5% | +5.7% [+2.3, +9.1] |
| exec busy | 0.949 | 0.967 | 0.963 | | |
| exec backlog committed - executed (blocks) / lag s | 55.0 / 14.3 | 57.2 / 15.5 | 56.7 / 15.1 | | |
| actions / fills per native block | 278 / 53,490 | 296 / 56,504 | 291 / 55,831 | | |
| engine ms per 1k fills | 3.84 | 3.45 | 3.52 | -10.0% | -8.2% |

Gate reading (section 47's criteria): CPU-s/1M down **met**; device writes **met**; matched/s up **not shown** (a tie,
+2.9%, CI crosses 0); native blocks/s no regression **not met** at the 95% level (-5.5%, the upper end -0.7%; the blocks
are 4.4% larger at the same exec ceiling, so fills/s are flat); RAM (~270 MB) **not met** (+1.55 GB peak, +1.13 GB mean;
the split is in section 48). Restart is not in scope here (SIGTERM cells on the next step 2 head follow).

### 49.2 Exec-thread checkpoint time: the residual of section 47, now timed

| ms per native block (summary phases, mean of 3 nodes) | gm | g1 | g2 | section 48 ctl / s2 (`2725e321` / `2296be67`) |
|---|---|---|---|---|
| `checkpoint_settle_wait` (join of the previous block's merge into `L`) | 0 | (untimed) | **23.9** | - / (untimed) |
| `checkpoint_upkeep` (trigger, merges, hand-off, view publish) | 0 | (untimed) | **1.0** (of which `checkpoint_wait` 0.1) | - / (untimed) |
| `residual_untimed` | 13.4 | 48.6 | **16.9** | 14.0 / 55.4 |
| exec `block_ms` | 251.6 | 270.4 | 265.9 | 255.8 / 277.7 |

Per observation: `checkpoint_settle_wait` p50 1.3 ms, p99 95 ms (mean 14.7 ms over ~860 observations per node per cell:
most blocks wait little, some wait for a long merge); `checkpoint_upkeep` p50 0.1 ms, p99 6.4 ms. Checkpoints: 14-16 per
node per cell. **The ~36-41 ms untimed residual of sections 47-48 is now 25 ms timed (almost all the merge join) +
3.5 ms still untimed.** Phase 3's exec time per block over its control went from ~22 ms (section 48: s2 277.7 vs ctl 255.8)
to ~14 ms (g2 265.9 vs gm 251.6), across campaigns, so this is only an indication. The difference fits the frees that
moved off the exec thread. **Free-off-exec-thread check:** in every g2 cell, val0's `torus-ckpt-drop` thread used
5.0-5.5 CPU-s per 120 s cell (task sampler). That is work the exec thread no longer does (~12 ms per native block). The
heap-profiled h1-g2 cell shows the same picture under jemalloc: `torus-ckpt-drop` active, residual 17.9 ms,
`checkpoint_settle_wait` 16.4 ms per block, exec mean 299 ms per block at 4.1 blk/s (smaller blocks: 48,747 fills). Its
live heap at the bench end: view slices 214 MB, `L` 280 MB, mempool 774 MB, total 2.86 GB per node (one cell; section
48's hs2 on `2296be67`: 260 / 406 / 743 / 3.17 GB).

### 49.3 Cancel-all mempool expiry and other checks

Nonce-window evictions per node per cell (`mempool_nonce_expired_evictions`): gm 0 in all 4 cells; g1 0 / 0 / 0 /
(0, 0, 20); g2 0 / 0 / (4, 4, 27) / 0; h1-g2 0. Section 47 had 433-1,351 per cell in 3 of 4 step 2 cells, and section 48
up to 440 (s2) and 391 (s1). The expiry is **rare in this run but not fixed in these binaries**: `51a67f57` carries
only the ignored repro test `bd0ecc6e` (owner decision pending). Executed cancel-alls equal those sent in every cell
(within 0-8 per cell still in flight at the end). No `layer_shared`, no open-limit rejects. RSS is all anonymous heap
(no swap), as in section 48.

**Verdict:** with a clean control, step 2 `51a67f57` keeps the CPU (-7.0%) and disk (-51%) wins and ties on matched/s.
Native blocks/s is -5.5% (larger blocks at the exec ceiling) and RAM is +1.55 GB peak: the gate's RAM and blocks/s
lines are not met. The untimed residual is resolved: 25 ms per block is the merge join (timed now) and +3.5 ms is left.
Next on the owner's list: allocator A/B (glibc vs jemalloc, section 48's retention), SIGTERM restart cells and a soak
on the next step 2 head.

Analysis (`tools/ozarchy-p3g2-tools/analyst/`): the section 48 scripts (`a-cells.py`, `a-arms.py`, `a-heap.py`,
`a-heapcat.py`); outputs in `ozarchy-p3g2-run/analyst/` on ozarchy.

## 50. P2-3 persistent exec worker pool vs its merge-base, commit-tail mode (campaign ozarchy-p23b, 2026-10-12)

**The pool removes every per-block thread spawn: perf sees 0 forks per native block on the pool arm against 44-46 on
the control. The throughput and CPU effects are inside the noise, as section 44 predicted:** matched/s -1.2% [-5.0,
+2.6], node CPU-s/1M +0.4% [-1.8, +2.6] (-0.7% [-1.5, +0.0] at equal fills), exec wall per native block +0.3% [-3.0,
+3.7]. The one resolved saving is the spawned threads' own CPU: exited-thread sys 0.12 -> 0.01 CPU-s/1M (val0). That
is ~0.11 CPU-s/1M (0.4%), inside section 44's 0.05-0.15 band and below what a 120 s cell can resolve in total CPU.

Owner request (18c, s112): A/B the P2-3 pool `perf/p2-3-worker-pool` `efcd1548` against its merge-base `c97f56f9`. Main
`8efcb07a` is not a clean control: it adds CoreWriter results on the exec path. Commit tail, n = 4. Hard check: perf fork
count ~0 per native block after warm-up on the pool arm, plus the spawn counters.

Setup: the section 47-49 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s cells,
commit tail, shared NVMe). Nodes: control `c97f56f9` (`f35fab7d`, the p3s2g build), pool `efcd1548` (`dacdeec8`, built
for this run; frame pointers + line tables + mold); bench `05d44313` and harness `8efcb07a` (as section 48). Cells
19:18-20:41 (driver `ozarchy-p3rss-campaign.sh` CAMP=B; held during sections 48-49, then relaunched): w0 (control, 60 s,
excluded), u cells r1 k p, r2 p k, r3 k p, r4 p k (ABBA, n = 4, unprofiled), and perf cells x1-pp x1-pk after r2, x2-pk
x2-pp after r4. The perf cells use the section 44 method (`prof-sidecar.sh`: `perf record -e cycles:u -g --call-graph fp`
on val0, 45 s from bench + 35 s; `perf script --show-task-events` -> PERF_RECORD_FORK). **All 12 cells rc 0, ACCEPT,
AGREE**, 182 CHECK OK, no CHECK FAIL, no deaths.

### 50.1 Forks and spawn counters (hard check)

| cell | window | native blocks | PERF_RECORD_FORK | per native block | parent |
|---|---|---|---|---|---|
| x1-pp (pool) | 47.6 s | 180 | **0** | **0.000** | - |
| x2-pp (pool) | ~48 s | 166 | **0** | **0.000** | - |
| x1-pk (control) | 47.9 s | 165 | 7,331 | 44.4 | `torus-execution` (2.82 scopes per block, 165 32-way) |
| x2-pk (control) | 47.5 s | 152 | 7,058 | 46.4 | `torus-execution` (2.99 scopes per block, 155 32-way) |

`torus_exec_thread_spawns_*` deltas per native block over each u cell (`metrics-before/after`, all 3 nodes): **control**
`match` 19.4-21.2, `settle` 19.3-20.9, `end_resident` 1.00, all other sites 0; **pool** 0 at every site, including
`end_resident`. The pool's `end_resident` counter is 1 per process (one long-lived holder thread, efcd1548 `spawn_count.rs`),
not 1 per block. So the "end_resident = 1 per native block" expectation in the request applies to the control only.
**Hard check passed:** ~0 forks per native block on the pool after warm-up (0 in both 45 s windows; the smoke saw 1 fork in 45 s,
from the flush worker).

### 50.2 Throughput, CPU and exec time (u cells, n = 4 per arm, 95% CI)

| | k control `c97f56f9` | p pool `efcd1548` | p vs k |
|---|---|---|---|
| matched/s | 198,392 | 196,035 | -1.2% [-5.0, +2.6] |
| native blocks/s | 3.48 | 3.49 | +0.3% [-7.6, +8.2] |
| node CPU-s/1M (whole run) | 25.19 | 25.29 | +0.4% [-1.8, +2.6] |
| node CPU at equal fills (ANCOVA) | | | -0.7% [-1.5, +0.0] |
| val0 exited-thread sys CPU-s/1M (1 Hz task sampler, the spawned workers) | 0.111-0.128 | 0.006-0.013 | **-0.11** |
| val0 process sys CPU-s/1M | 1.56-1.64 | 1.54-1.71 | not resolved |
| exec chain_ms per native block (summary) | 260.9 | 259.9 | -0.4% [-4.8, +4.0] |
| exec wall per native block (logs) mean / p50 / p99 ms | 331.9 / 349.8 / 639.9 | 333.0 / 352.9 / 651.6 | mean +0.3% [-3.0, +3.7], p50 +0.9% [-2.2, +4.0], p99 +1.8% [-4.4, +8.0] |
| exec thread busy | 0.955 | 0.956 | +0.2% [-0.7, +1.0] |
| fills per native block | 56,126 | 54,555 | -2.8% [-6.5, +0.9] |
| engine ms per 1k fills | 3.79 | 3.85 | +1.5% [-1.4, +4.4] |
| `end_resident` (off-chain) / `end_resident_wait` ms per block | 38.3 / 6.9 | 37.1 / 6.4 | |
| `residual_untimed` ms per block | 14.1 | 18.2 (r4-p 31.5, others 13.2-14.5) | not resolved |
| commit interval p99 ms (logs) | 1,103 | 1,088 | -1.4% [-4.2, +1.4] |
| RSS peak GB | 4.41 | 4.42 | |

The perf cells: pp 25.58 vs pk 26.65 CPU-s/1M. That gap is the perf trap of section 44.2 (inherited per-thread events are
charged on every new thread, and the pool creates none), not a pool saving.

**Verdict:** the pool works as designed: 0 spawns per block at match, settle and end_resident, and 0 forks in perf. Its
expected gain (~0.05-0.15 CPU-s/1M, ~1 ms of ~330 ms exec wall per block) does **not resolve** at n = 4 x 120 s in commit
mode. The CIs on CPU (±2.2%) and exec wall (±3.3%) are 5-10x the expected effect. Only the spawned threads' own sys CPU
shows the saving (0.11 CPU-s/1M). No regression is visible either. Resolving +0.4% would need ~25x the cells, or a
paired fixed-load microbench (section 44.2's `spawnbench.rs` already gives 0.7 ms per 32-way scope).

Analysis (`tools/ozarchy-p23b-tools/analyst/`): `a-cells.py` / `a-arms.py` (section 48), `p23-spawn.py` and `p23-exited.py`
(section 44, copied, the empty-fork guard added), `forks.txt`. Outputs in `ozarchy-p23b-run/analyst/` on ozarchy.

## 51. Phase 3 allocator A/B: glibc vs jemalloc on step 2 `7b006739` and main `219f0c20`, commit-tail mode (campaign ozarchy-p3alloc, 2026-10-12)

**jemalloc lowers step 2's RSS and its growth, but the Phase 3 RSS delta stays far above the ~270 MB gate under both
allocators, and at equal fills jemalloc costs step 2 CPU.** The deltas (step 2 - control) are glibc +1.55 GB peak / +1.08 GB mean and
jemalloc +0.97 GB peak / +0.71 GB mean. Under jemalloc, step 2's peak RSS is -0.32 GB [-0.53, -0.12] and its growth is
1.01 vs 1.64 GB/min. Its matched/s is +3.6% [+2.2, +5.0] and its exec wall per native block -13%. Whole-run CPU-s/1M is
-1.1% [-2.6, +0.5], but at equal fills it is +4.0% [+1.6, +6.3]. The offered load is not the same in the two
allocators: under jemalloc, blocks commit ~15% faster, so the closed-loop bench (4 in flight per sender, released on
commit) submits +13-14% more (51.2). On main, jemalloc costs CPU (+1.5% [+0.9, +2.2] whole run, +1.9% [-0.4, +4.1] at
equal fills) and leaves matched/s and RSS unresolved. **Soak winner: glibc**, because jemalloc fails the CPU condition at
equal fills (rule in 51.3).
All 16 measured cells are valid for throughput. 3 step 2 cells are REJECT on agreement, all from the known mark-only digest artifact (51.4).

Owner request (18c, s112, after section 49): an allocator A/B of section 48's retention question on the next step 2 head,
then a soak on the winner. Step 2 = `7b006739` (`feat/item6-phase3-step2`: section 49's `51a67f57` + the drop_inline
counter `43cbcce5` + step 1 with main `219f0c20` merged in + a SIGTERM zero-replay test). Control = main `219f0c20`
(SIGTERM graceful shutdown, `fix/sigterm-shutdown`), the merge-base of the two, so the control is clean.

Setup: the section 47-50 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s
cells, commit tail, shared NVMe). Nodes: control `219f0c20` (`987f4eb6`), step 2 `7b006739` (`ca388315`, interval
15 s), both built for this run (`ozarchy-p3next-build`, frame pointers + line tables + mold). Bench `05d44313`, harness
`bench/summarize-ckpt-timers` `218ad251`. Arms: gm = control glibc, g2 = step 2 glibc, jgm / jg2 = the same binaries
started through a wrapper that only sets `LD_PRELOAD=/usr/lib/libjemalloc.so.2`. That is system jemalloc 5.3.1, with no
`MALLOC_CONF` and no `/etc/malloc.conf`, so library defaults (in `ozarchy-p3alloc-tools/jemalloc-defaults-stats.txt`),
and profiling off. The bench is not preloaded (same bench md5 in every arm). Cells 20:49-22:31 (driver
`ozarchy-p3alloc-campaign.sh` CAMP=J): w0 (step 2, 60 s, excluded), r1 gm g2 jgm jg2, r2 g2 jgm jg2 gm, r3 jgm jg2 gm g2,
r4 jg2 gm g2 jgm (n = 4 per arm, rotated). 238 CHECK OK, no CHECK FAIL, no memguard stop, no deaths, the host quiet
before every cell. **jemalloc check:** in all 8 je cells, `node-preload.txt` shows the preload on all 3 node pids and
`node-maps-jemalloc.txt` shows `libjemalloc.so.2` mapped in all 3. In all 8 glibc cells, no node has the preload or the
mapping.

### 51.1 Per arm (n = 4, mean; RSS from cpu.csv, growth from mem-samples.txt over the bench window)

| | gm main glibc | g2 step 2 glibc | jgm main jemalloc | jg2 step 2 jemalloc |
|---|---|---|---|---|
| matched/s | 206,597 | 210,835 | 207,822 | 218,439 |
| native blocks/s | 3.49 | 3.48 | 3.79 | 3.80 |
| node CPU-s/1M (whole run) | 25.06 | 23.44 | 25.44 | 23.18 |
| RSS peak / mean GB | 4.12 / 3.52 | 5.67 / 4.60 | 4.38 / 3.58 | 5.34 / 4.29 |
| RSS growth over the bench window MB/min | 867 | 1,643 | 696 | 1,007 |
| RSS first / last 15 s of the bench window GB | 1.95 / 4.06 | 1.99 / 5.49 | 2.64 / 4.25 | 2.77 / 5.09 |
| exec per native block (logs) mean / p50 / p99 ms | 334 / 347 / 665 | 356 / 360 / 719 | 310 / 335 / 605 | 310 / 329 / 607 |
| exec busy | 0.947 | 0.970 | 0.967 | 0.960 |
| exec backlog committed - executed (blocks) | 42.5 | 40.9 | 53.4 | 55.7 |
| bench submit rate (accepted, per s) | 1,163 | 1,163 | 1,317 | 1,331 |
| fills per native block | 53,957 | 54,765 | 53,886 | 54,025 |
| engine ms per 1k fills | 3.75 | 3.48 | 3.64 | 3.25 |
| `checkpoint_settle_wait` / `checkpoint_upkeep` ms per block | 0 / 0 | 23.4 / 1.3 | 0 / 0 | 18.5 / 0.7 |
| device writes MB/s per validator | 136.1 | 62.6 | 146.0 | 71.7 |

The je r1 cells (r1-jgm, r1-jg2) ran 4.4 native blocks/s with smaller blocks (~46k fills); the other 14 cells ran
3.4-3.7. That is the sd of the je blocks/s rows (0.4), and no blocks/s difference resolves.

### 51.2 Contrasts (Welch 95% CI; RSS in GB)

| | Phase 3 (s2 - ctl), glibc | Phase 3 (s2 - ctl), jemalloc | allocator (je - glibc), step 2 | allocator (je - glibc), main |
|---|---|---|---|---|
| RSS peak | **+1.55 [+1.35, +1.75]** | **+0.97 [+0.66, +1.27]** | **-0.32 [-0.53, -0.12]** | +0.26 [-0.08, +0.60] |
| RSS mean (whole cpu.csv) | +1.08 [+0.93, +1.23] | +0.71 [+0.42, +1.00] | -0.31 [-0.50, -0.13] | +0.06 [-0.25, +0.37] |
| RSS mean, bench window (mem-samples) | +0.96 [+0.86, +1.05] | +0.72 [+0.34, +1.10] | +0.8% [-2.0, +3.5] | +7.7% [-4.5, +19.8] |
| RSS growth GB/min | +0.78 [+0.52, +1.03] | +0.31 [+0.16, +0.46] | **-0.64 [-0.87, -0.40]** | -0.17 [-0.38, +0.03] |
| matched/s | +2.1% [+0.2, +3.9] | +5.1% [+3.9, +6.3] | **+3.6% [+2.2, +5.0]** | +0.6% [-1.2, +2.4] |
| node CPU-s/1M, whole run | -6.5% [-7.4, -5.5] | -8.9% [-10.4, -7.4] | **-1.1% [-2.6, +0.5]** | +1.5% [+0.9, +2.2] |
| node CPU at equal fills (ANCOVA) | -6.4% [-7.1, -5.6] | -8.8% [-10.0, -7.7] | +4.0% [+1.6, +6.3] (extrapolated, see below) | +1.9% [-0.4, +4.1] |
| exec wall per native block, mean | +6.6% [+2.8, +10.3] | +0.0% [-9.7, +9.8] | -13.1% [-16.3, -9.9] | -7.4% [-16.6, +1.8] |
| exec p99 | +8.1% [+0.4, +15.7] | +0.4% [-3.8, +4.7] | -15.5% [-22.2, -8.9] | -9.1% [-12.3, -5.9] |

**Against the ~270 MB gate:** Phase 3 costs +1.55 GB peak under glibc and +0.97 GB under jemalloc. Neither CI comes near
0.27 GB. jemalloc removes ~0.6 GB of the delta (about the glibc retention that section 48 estimated, "about half of each
Phase 3 delta"). The rest is live heap (section 48.2: view, `L`, and the commit-mode backlog).

**Where the extra submits under jemalloc come from (both binaries, +13-14%).** The load generator is the same binary
in every arm (md5 `05d44313`, no preload; `node-preload.txt` shows `LD_PRELOAD` only on the 3 node pids). It is closed-loop:
each sender keeps at most 4 actions in flight, and in commit-tail mode a slot is released only when the action's block
commits. Every arm shows `released_committed` only, with 0 timeouts and 0-10 refusals per cell. So the submit rate follows
the commit cadence, and the commit cadence is what changes:

| (bench window, mean of 4 cells) | gm | jgm | g2 | jg2 |
|---|---|---|---|---|
| blocks committed per s (logs, 3 nodes) | 3.82 | 4.39 (+15%) | 3.67 | 4.34 (+18%) |
| commit interval mean ms | 262 | 231 | 271 | 229 |
| bench submits per s | 1,163 | 1,317 | 1,163 | 1,331 |
| RPC submit admit / verify / permit wait ms per call (node metrics) | 0.91 / 12.1 / 6.6 | 0.99 / 13.4 / 7.9 | 0.87 / 10.7 / 6.5 | 0.84 / 10.7 / 6.2 |
| `torus_view_proposal_arrival` ms | 67.8 | 59.9 | 65.3 | 53.4 |
| `torus_block_build` ms (select) | 25.4 (13.3) | 33.3 (18.1) | 20.8 (9.5) | 24.1 (11.5) |

The node-side RPC path is **not** faster under jemalloc: admit, verify and permit wait per call are flat or slightly
higher. What is faster is the consensus round: proposals arrive 12-18% sooner, and blocks commit 15-18% more often.
Block build is slower per block (more mempool to select from). Which allocation-heavy step of the round gains the
most is not resolved here (no profile). The extra admitted actions cannot be executed: exec is at its ceiling (busy
0.96-0.97). They go into the exec backlog (+11-15 blocks) and the mempool. Bench-window matched/s therefore moves only
+0.6% on main and +3.6% on step 2, while over the whole run, drain included, the je cells execute ~14% more fills
(`a-submit.py`, `submit.txt`).

**This confounds the allocator comparison.** The offered (admitted) load is ~14% higher under jemalloc in both
binaries. Whole-run CPU-s/1M then mixes the allocator with a different load, so the verdict uses CPU at equal fills
(ANCOVA on node CPU-s vs fills per cell): step 2 **+4.0% [+1.6, +6.3]**, main +1.9% [-0.4, +4.1]. Caveat: the ~14% fills
gap is ~10x the within-arm spread (sd ~1-2%), so this adjustment extrapolates a common slope (14-24 CPU-s per 1M fills).
It is the better of the two estimates, not an exact one. The RSS comparison is conservative for jemalloc, since it
carries the larger backlog and still has the lower peak.

### 51.3 Soak winner

Rule (the owner's criterion, made explicit): the winner is the allocator with the lower step 2 RSS peak (Welch CI
excludes 0), provided that neither step 2 matched/s nor step 2 CPU-s/1M **at equal fills** (the offered load differs,
above) has a 95% CI lying entirely on the regression side. If jemalloc does not qualify, glibc stays (the production
default). jemalloc has the lower peak (-0.32 GB [-0.53, -0.12]) and no matched/s regression (+3.6% [+2.2, +5.0]). But its
step 2 CPU at equal fills is +4.0% [+1.6, +6.3], a CI entirely on the regression side. **jemalloc does not qualify, so the
soak winner is glibc** (`SOAK_ALLOC=glibc`). On whole-run CPU-s/1M (-1.1% [-2.6, +0.5]) jemalloc would have qualified:
the decision rests on the equal-fills adjustment and its extrapolation caveat. Also: jemalloc raises early RSS (first
15 s +0.7-0.8 GB on both binaries) and lowers growth, and the bench-window mean RSS is flat (+0.8% [-2.0, +3.5]). A
jemalloc soak is the follow-up if the owner wants the slope question answered for it too.

### 51.4 Digest artifact in 3 step 2 cells

r3-jg2, r3-g2 and r4-g2 are rc 2 REJECT "validator agreement failed". In each, exactly one node differs, and only in the
5 mark fields (`accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable`) of all 50
accounts (250 fields). Every book, OI and cash line is identical on all 3 nodes (checked). This is the slow-digest mark
artifact of sections 46-49 (Open, restart path item 3), not a divergence, so their throughput is kept.

| cell | odd node | digest wall s val0 / val1 / val2 | digest heights |
|---|---|---|---|
| r3-jg2 | val0 | 38.9 / 41.3 / 40.9 | 1097 / 1098 / 1098 |
| r3-g2 | val2 | 72.3 / 73.5 / **17.1** | 1267 / 1266 / 1267 |
| r4-g2 | val2 | 84.9 / 84.9 / **20.6** | 1274 / 1274 / 1273 |

Digest wall per node over all 16 cells (mean, min-max): gm 51.1 s (48-59), jgm 42.0 s (25-56), jg2 40.2 s (24-54),
g2 57.3 s (17-95). **The step 2 digest is not systematically slower. Under glibc it is bimodal**: r2-g2 ran 17-19 s on
all nodes, r1-g2 92-95 s, and r3-g2 / r4-g2 split 17-21 s vs 72-85 s within the cell. A within-cell spread of 56-64 s is
what produces the mark DISAGREE (the odd node reads the accounts at a different time after the oracle pause). The other
13 cells spread 0.4-8 s. r3-jg2 is the exception: its spread is 2.4 s, and its odd node is the one that sits 1 block lower
(1097). 0 of 8 control cells disagree; 3 of 8 step 2 cells do.

**Verdict:** under either allocator, Phase 3's RAM cost is ~1-1.5 GB at 120 s against a ~270 MB bound, so the gate's RAM
line is not met. jemalloc takes ~0.6 GB of retention off step 2 and lowers its growth by 39%. It also speeds the
consensus round (+15-18% commits/s), which raises the closed-loop bench's admitted load by 13-14%. At equal fills it costs
CPU: step 2 +4.0% [+1.6, +6.3], main +1.9% [-0.4, +4.1]. Phase 3's CPU and write wins hold under both allocators
(CPU-s/1M -6.5% / -8.9%, writes -54% / -51%). Next: the soak on glibc (`SOAK_ALLOC=glibc`, 600 s per arm), to see whether
step 2's RSS levels off.

Analysis (`tools/ozarchy-p3alloc-tools/analyst/`): `a-cells.py` / `a-arms.py` (section 48, unchanged), `a-archive.py`,
`digest.txt` (per node digest wall / height / digest), `a-submit.py` / `submit.txt` (RPC and proposal latency per arm). Outputs in `ozarchy-p3alloc-run/analyst/` on ozarchy.
## 52. SIGTERM restart cells on step 2 `7b006739` (interval 15 s) and main `219f0c20` with the graceful SIGTERM shutdown (campaign ozarchy-p3term, 2026-10-12)

**SIGTERM is now a clean stop on both binaries: 12 of 12 kills replay 0 blocks (durable marker = committed height), no
forced stop, no shutdown timeout, every state hash and digest agrees.** SIGTERM -> process exit: step 2 7.0 s mean /
10.7 s max (p95 10.2 s), control 3.7 / 8.8 s. Almost all of it is draining the exec queue (~0.3 s per queued block). The
memtable flush + WAL sync is 0.55 s mean on step 2 vs 0.41 s on control, so the checkpoint adds ~0.15 s. **1 of 8 step 2
kills ran past Docker's 10 s grace** (10.69 s, 29 blocks queued). All are far inside `TORUS_SHUTDOWN_TIMEOUT_SECS` (unset
on every node = the 60 s default, checked). **Restart gate (<= 60 s) met, barely in one kill:** kill -> first chain commit
is at most 59.8 s (step 2) and 21.9 s (control). The 59.8 s kill, and 56-75 s chain-wide stalls in all 3 cells (control
too), come from the gossipsub resubscribe problem after an in-place restart (Open, restart path item 2), not from the
shutdown path.

Owner request (via 18c, s112): SIGTERM restart cells on the next step 2 head, now that main has the graceful SIGTERM
shutdown (`fix/sigterm-shutdown`, merged as `219f0c20`: a pre-started shutdown thread drops the replica, so consensus
stops and the exec queue drains, then flushes every CF and syncs the WAL, within `TORUS_SHUTDOWN_TIMEOUT_SECS`). Section
46 found SIGTERM unhandled on `a44bdde6` (= SIGKILL, replay from the marker).

### 52.1 Setup

The section 46 method, in commit-tail mode. Nodes: step 2 `7b006739` (`ca388315`; carries `219f0c20` and the
zero-replay SIGTERM test `7b006739`) at `TORUS_CHECKPOINT_INTERVAL_MS=15000` (arm s), and control main `219f0c20`
(`987f4eb6`, knob unset, arm c), the section 51 builds. Bench `05d44313`, harness `218ad251`, with `crash-kill.sh`
patched (`ozarchy-p3term-tools/matched-bench`): `KILL_SIG`, process-exit polling every 0.1 s (`exit_s`), and a stop still
running after `KILL_WAIT=75` s would be SIGKILLed and flagged `exit_forced`. Restart: val1 starts in place right after it
exits. `STATE_HASH_ACTIVATION=1` in every cell (the state hash poller compares every 100-block checkpoint on the 3 nodes).
tw, Classic, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, shared NVMe, glibc. Cells 22:44-23:26 (driver
`ozarchy-p3term-campaign.sh`): w0 (s, 60 s, no kill, excluded), t1-s-TERM and t1-c-TERM (540 s, SIGTERM val1 at bench
+60 / 180 / 300 / 420 s), t2-s-TERM (+65 / 187 / 309 / 431 s): 8 step 2 kills and 4 control kills. 62 CHECK OK, no CHECK
FAIL, no HARD FINDING, the host quiet before every cell. The 3 kill cells are rc 2 REJECT for three reasons. The crash
gate matches the INFO line `running state hash fail-stop ... on=false` (section 46's known false FAIL; 0 panics). Liveness
FAIL and "drain not established" come from the chain stalls in 52.4. t2-s-TERM also has "dissemination failures" (body
fetch exhausted after the stalled restart). Their matched/s (94-121k) is not a throughput measurement.

### 52.2 Per kill (times in s; exit at 0.1 s resolution; phases from val1's log after the signal)

| cell | kill | exec queue at kill (blocks) | SIGTERM -> queue drained | -> memtables flushed + WAL synced | -> `shutdown complete` (elapsed_ms) | -> process exit | marker A ; committed C (replay) | restart -> DB open | restart -> first chain commit | kill -> first chain commit | restart -> rejoin |
|---|---|---|---|---|---|---|---|---|---|---|---|
| t1-s | 1 | 28 | 8.42 | 8.90 | 8.90 (8,895) | 9.28 | 700 ; 700 (0) | 0.11 | 19.5 | 28.8 | 25.0 |
| t1-s | 2 | 29 | 9.88 | 10.37 | 10.37 (10,365) | **10.69** | 977 ; 977 (0) | 0.08 | 11.7 | 22.4 | 10.9 |
| t1-s | 3 | 14 | 4.31 | 4.85 | 4.85 (4,845) | 5.24 | 1144 ; 1144 (0) | 0.14 | 4.3 | 9.5 | 7.3 |
| t1-s | 4 | 12 | 4.33 | 5.38 | 5.38 (5,374) | 5.90 | 1327 ; 1327 (0) | 0.11 | 3.4 | 9.3 | 6.2 |
| t2-s | 1 | 26 | 7.73 | 8.47 | 8.47 (8,471) | 8.85 | 678 ; 678 (0) | 0.08 | 16.2 | 25.1 | 4.2 |
| t2-s | 2 | 21 | 7.20 | 7.44 | 7.44 (7,441) | 7.76 | 968 ; 968 (0) | 0.46 | 1.2 | 9.0 | 4.5 |
| t2-s | 3 | 10 | 3.69 | 4.31 | 4.31 (4,303) | 4.70 | 1147 ; 1147 (0) | 0.07 | **55.1** | **59.8** | 36.2 |
| t2-s | 4 | 0 | 3.20 | 3.41 | 3.41 (3,410) | 3.65 | 1154 ; 1154 (0) | 0.13 | 26.9 | 30.5 | 33.4 |
| t1-c | 1 | 33 | 8.02 | 8.43 | 8.43 (8,427) | 8.75 | 646 ; 646 (0) | 0.37 | 13.2 | 21.9 | 6.5 |
| t1-c | 2 | 3 | 1.40 | 1.71 | 1.71 (1,708) | 2.02 | 901 ; 901 (0) | 0.45 | 16.8 | 18.8 | 5.2 |
| t1-c | 3 | 2 | 1.21 | 1.61 | 1.61 (1,610) | 1.91 | 1088 ; 1088 (0) | 0.30 | 10.4 | 12.3 | 6.8 |
| t1-c | 4 | 3 | 1.40 | 1.95 | 1.95 (1,947) | 2.24 | 1147 ; 1147 (0) | 0.07 | 8.2 | 10.4 | 3.9 |

`shutdown signal received` is logged at +0.00-0.01 s in every kill. `flush worker thread shutting down` (step 2: the
window closes into a final checkpoint) comes 0.5-1.1 s before `shutdown complete` (t1-s). The process exits 0.24-0.52 s after
`shutdown complete`. On restart, val1 logs `coalesced state checkpoints ON ... applied=<C>`, i.e. it attaches at the
committed height, ~0.6 s after the old process completed.

### 52.3 Summary vs the gates (mean / max; p95 by linear interpolation)

| | step 2 (8 kills) | control (4 kills) |
|---|---|---|
| SIGTERM -> exit | 7.01 / **10.69** (p95 10.2) | 3.73 / 8.75 (p95 7.8) |
| of which queue drain | 6.09 / 9.88 | 3.01 / 8.02 |
| of which flush + WAL sync (flushed - drained) | 0.55 / 1.04 | 0.41 / 0.55 |
| exits over 10 s (Docker default grace) | **1** (t1-s k2) | 0 |
| forced stops (alive after 75 s) / shutdown-timeout lines | 0 / 0 | 0 / 0 |
| replay blocks after restart | **0** in all 8 | **0** in all 4 |
| restart -> DB open | 0.15 / 0.46 | 0.30 / 0.45 |
| restart -> first chain commit | 17.3 / 55.1 | 12.1 / 16.8 |
| kill -> first chain commit (gate 60 s) | 24.3 / **59.8** | 15.9 / 21.9 |
| restart -> rejoin | 16.0 / 36.2 | 5.6 / 6.8 |

- **Exit time is the exec queue.** Drain time per queued block is ~0.25-0.3 s (the exec wall per block, section 51), so
  exit ~ 0.3 s x queue + ~0.5 s. Step 2's longer mean comes from its deeper queues at the kill times (10-29 vs 2-33
  blocks on control; control k1 with 33 blocks took 8.75 s). The checkpoint's own share is the flush step: +0.14 s mean,
  1.04 s max. The 10 s Docker grace is at risk whenever the exec queue holds more than ~30 blocks: in commit-tail mode
  under this load the queue sits at 40-60 blocks for most of a cell (section 51: backlog 41-56), so the kills here, at
  10-33 blocks, were lighter than average. Under Docker, an operator should set `stop_grace_period` well above 10 s
  (e.g. >= `TORUS_SHUTDOWN_TIMEOUT_SECS` = 60 s).
- **Correctness:** A = C in all 12 kills (no gap line, the restart attaches at C, replay 0). A >= val1's durable
  state-hash height before the kill (`marker_lo`) every time. Digests equal and AGREE in all 3 cells (heights 1582 / 1191 /
  1421). Every state-hash checkpoint agrees on the 3 nodes (18 / 12 / 17 heights; t2-s has one height without val1,
  which was down then, and no disagreement). No hard finding.
- **Restart gate:** met. The one kill near 60 s (t2-s k3, 59.8 s) and the two slow t2-s rejoins (33-36 s) are the
  gossipsub problem below; on the kills it spared, the first chain commit came 1-20 s after the restart.

### 52.4 Chain stalls after the restart (not the shutdown path)

Each kill cell had a chain-wide stall (all 3 nodes, no commits) of 56-75 s:

| cell | stall (val0) | start relative to the nearest restart |
|---|---|---|
| t1-s-TERM | 74 s | 26 s after k4's restart |
| t1-c-TERM (control) | 75 s | 32 s after k3's restart |
| t2-s-TERM | 56 s, then 62 s | at k3 (-1.8 s), then 84 s after k3's restart |

In 9 of 12 restarts (kills 2-4 of every cell, both arms) val1's log shows hundreds to thousands of
`NoPeersSubscribedToTopic` lines (476-1,468) and the mesh watchdog's `validator connected but unsubscribed past grace —
forcing reconnect` 70-80 s after the restart. After each first kill there are only 13-24 such lines and no watchdog
reconnect. In t2-s k3, val1 also logged `body fetch exhausted 9 retries` and the catch-up gate (empty payloads). This is
section 46's Open item 2 (in-place restart leaves gossipsub subscriptions un-exchanged): it is in the control too, and it
does not come from the shutdown path or the checkpoint. It is what puts t2-s k3 at 59.8 s kill -> first commit and what
fails the cells' liveness check.

**Verdict:** the SIGTERM shutdown works as designed on step 2. Replay is 0, nothing is forced, and state agrees. Exit
takes 0.3 s per queued exec block + ~0.5 s, of which the Phase 3 checkpoint adds ~0.15 s. With a deep commit-mode
backlog this can pass Docker's 10 s default grace (1 of 8 here, 10.7 s), never the 60 s node bound. The restart gate
holds (max 59.8 s), but the margin is eaten by the gossipsub resubscribe stall, which needs its own fix (stop / wait /
start restart in the harness, or explicit gossipsub peers).

Analysis (`tools/ozarchy-p3term-tools/`): `a-term.py` (section 46's `a-restart.py` + the shutdown phases and exit
times), output `ozarchy-p3term-run/analyst/a-term.txt`; `matched-bench/crash-kill.sh` (patched).
## 53. Phase 3 soak: step 2 `7b006739` vs main `219f0c20`, 600 s per arm, glibc, commit-tail mode (campaign ozarchy-p3soak, 2026-10-12)

**Step 2's RSS does not level off within 10 min: it still grows +40 to +67 MB/min per node over the last 5 min (95% CIs
exclude 0: GROWTH FINDING).** Main grows +3 to +11 MB/min (2 of 3 node CIs exclude 0, a smaller GROWTH FINDING on the
control). The Phase 3 delta at 10 min is +1.98 GB per node (mean of 3; +1.67 to +2.27). It is larger than the +1.55 GB
peak of the 120 s cells, and ~7x the ~270 MB gate. **The load generator degrades in both arms after ~5 min:** cancel-all /
nonce-window expiry starts at minute 2 (~18k actions per node per cell, both arms) and matched/s drops from ~200k to
~135k. The last-5-min numbers are therefore at ~30% less load than the first 5 min.

Owner request (via 18c, s112): a fixed 10 min soak per arm on the section 51 winner (glibc), with no extensions, to see
whether step 2's RSS levels off. Setup: the section 51 shape (tw, Classic, 300 markets, standard shape, MAX_IN_FLIGHT 4,
budget 900, commit tail, shared NVMe), nodes control `219f0c20` (`987f4eb6`) and step 2 `7b006739` (`ca388315`, interval
15 s), bench `05d44313`, harness `218ad251`. Driver `ozarchy-p3alloc-campaign.sh` CAMP=S SOAK_ALLOC=glibc. Cells 23:32-00:05:
w0 (control, 60 s, excluded), k1-ctl (600 s), k1-s2 (600 s). 42 CHECK OK, no CHECK FAIL, no memguard stop (host
MemAvailable stayed >= ~32 GB), no deaths, the host quiet before every cell. k1-ctl is ACCEPT, AGREE. k1-s2 is rc 2 REJECT on
agreement only, from the mark-only digest artifact: val2's digest took 17.1 s vs 64.8 / 67.6 s, the 5 mark fields differ
on 50 accounts (`withdrawable` on 38), and books, OI and cash are identical. Liveness PASS.

### 53.1 RSS per node (VmRSS from mem-samples.txt, 5 s; bench window 603 s)

| | node | RSS @ 5 min MB | RSS @ 10 min MB | peak MB | slope, last 5 min MB/min (95% CI) | slope, whole window MB/min |
|---|---|---|---|---|---|---|
| main | val0 | 3,760 | 3,796 | 3,835 | +3.3 [-4.2, +10.8] | +81 |
| main | val1 | 3,907 | 3,950 | 4,032 | **+11.1 [+5.5, +16.7]** | +95 |
| main | val2 | 4,712 | 4,769 | 4,788 | **+6.4 [+2.3, +10.5]** | +113 |
| step 2 | val0 | 5,663 | 6,070 | 6,070 | **+63.1 [+14.6, +111.6]** (n_eff 3) | +195 |
| step 2 | val1 | 5,559 | 5,934 | 5,934 | **+67.3 [+47.1, +87.4]** | +182 |
| step 2 | val2 | 6,338 | 6,442 | 6,660 | **+39.8 [+19.1, +60.5]** | +207 |
| **Phase 3 delta** | mean | **+1.73 GB** | **+1.98 GB** | +2.00 GB | +50 MB/min | |

Slope CIs: OLS over the 5 s samples, with the standard error widened by an AR(1) effective sample size (lag-1 residual
autocorrelation 0-0.9). val0 of step 2 has n_eff 3, so its CI is wide. The cpu.csv definition of sections 47-51 gives
RSS peak / mean 4.31 / 4.04 GB (main) and 6.36 / 5.69 GB (step 2). Step 2's checkpoints: 54 per node (one per ~11 s), mean layer
96 MB per checkpoint.

### 53.2 Throughput, CPU and the load generator

| | main | step 2 |
|---|---|---|
| matched/s whole window / last 5 min | 168,384 / 139,804 | 172,911 / 141,893 |
| matched/s per minute (min 1-10) | 191k 211k 202k 191k 188k 165k 132k 139k 135k 134k | 196k 218k 212k 199k 193k 162k 139k 138k 138k 133k |
| matched/s drift (OLS over the 10 minutes, per min) | -9.4k [-13.7k, -5.1k] | -10.3k [-15.5k, -5.1k] |
| node CPU-s/1M whole run / last 5 min (cpu-ticks) | 30.35 / 36.35 | 28.47 / 35.00 |
| native blocks/s | 2.95 | 3.03 |
| exec per native block mean ms, exec busy | 324, 0.89 | 327, 0.92 |
| exec backlog mean (blocks) | 27 | 28 |
| nonce-expired evictions per node | 17,956 (x3) | 18,615 / 18,615 / 18,694 |
| bench: released on commit / by 70 s timeout / refused | 659,030 / 17,956 / 1 | 679,956 / 17,378 / 67 |
| bench tail errors / missed | 0 / 0 | 0 / 0 |

Expiry per minute (val0 log, `evicted nonce-expired`): main 0, 0, 2.9k, 3.3k, 1.1k, 3.3k, 2.3k, 2.6k, 2.2k, 0.3k; step 2 0,
0, 3.4k, 3.6k, 2.4k, 3.4k, 1.2k, 2.0k, 1.4k, 0.8k. Every expired action holds its sender's in-flight slot until the
bench's 70 s timeout. Matched/s falls from minute 6, after ~4 min of expiry, while the exec queue drains to 0-5 blocks
(the chain is no longer exec-bound) and the mempool holds 2.7-7.9k actions. So the **soak load degrades by itself, on main as
much as on step 2**: this is the cancel-all expiry of sections 47-49 (sender-ordered cancel share), which reproduces on
main once the run passes ~2 min. It also explains the higher last-5-min CPU-s/1M (fixed per-block CPU over fewer fills).
Phase 3 at the soak: CPU-s/1M -6.2% whole run, -3.7% over the last 5 min; matched/s +2.7% / +1.5% (single cells, no CI).

**Verdict:** within 10 min, step 2's RSS does not reach a plateau. It is still rising +40-67 MB/min per node at 10 min,
while main's growth is down to +3-11 MB/min, and the Phase 3 delta is +1.98 GB at 10 min. That is a **GROWTH FINDING
for 18c**, with two caveats. (a) The last 5 min ran at ~30% lower matched/s in both arms (load-generator expiry). (b) The
slope is ~1/30 of the 120 s cells' growth (1.64 GB/min). A longer run would show whether it flattens, but per the owner's
rule it was not extended. The cancel-all expiry also hits main in long cells, so the cancel-share fix A/B has a control
that reproduces it.

Analysis (`tools/ozarchy-p3soak-tools/analyst/`): `a-soak.py` (RSS at 5 / 10 min, last-5-min slopes with AR(1)-widened CIs,
per-minute matched/s and drift, last-5-min CPU-s/1M, ingest health), `a-cells.py` (section 48); outputs `soak.txt`,
`cells.json` in `ozarchy-p3soak-run/analyst/` on ozarchy.

## 53b. Phase 3 soak under jemalloc: step 2 `7b006739` vs main `219f0c20`, 600 s per arm (campaign ozarchy-p3soakje, 2026-10-13)

**Under jemalloc, step 2's RSS levels off by ~minute 2 and then stays flat for the rest of the 10 min. So the continued
growth of the glibc soak (section 53) is consistent with glibc allocator retention rather than live data growth.** The step 2 mean RSS over
minutes 5-10 vs minutes 2-5 is -133 / -139 / -27 MB per node (glibc: +349 / +329 / +364). The last-5-min slopes are +15 /
+33 / +28 MB/min, and only one CI excludes 0 (glibc: +63 / +67 / +40, all three exclude 0). The slopes are most likely
jemalloc's decay (RSS swings by +-300 MB around a flat level, see figure 53b-1), but slow growth inside those swings is not
ruled out by one 10 min pair. The **Phase 3 delta under jemalloc is +0.68 GB per
node** (mean over minutes 5-10; +0.52 to +0.79). It is +0.69 GB at 10 min and +1.01 GB at the peak (val2 +1.29). Under
glibc it is +1.86 GB (minutes 5-10), +1.98 GB at 10 min and +2.00 GB at the peak. **Owner RAM gate (levels off, delta <=
~1 GB/node): met under jemalloc on the level and on the 5-10 min mean, borderline at the peak. Not met under glibc.**
RSS follows neither the Phase 3 layer metrics nor the resting-order (book) gauge: neither of them grows during the soak
(53b.2). **The matched/s drop from ~minute 6 comes from the workload, not from the generator's send rate and not from a
node slowdown.** From minute 6 the chain rejects 105-155k orders/s for margin in all four cells, while bench submits,
processed actions/s, node CPU and (under jemalloc) exec busy hold or rise. Section 53's link to the cancel-all expiry is
not supported: expiry starts 3 min earlier (53b.3).

Owner request (18c, queue item 1 after section 53): the section 53 soak pair repeated on jemalloc. Flat RSS under
jemalloc means allocator retention; RSS that still grows means data growth. The binaries, bench, harness and shape are
those of section 53: control `219f0c20` (`987f4eb6`), step 2 `7b006739` (`ca388315`, interval 15 s), bench `05d44313`,
harness `218ad251`, tw, Classic, 300 markets, MAX_IN_FLIGHT 4, budget 900, commit tail, shared NVMe. Arms jgm / jg2 are
section 51's: a wrapper that only sets `LD_PRELOAD=/usr/lib/libjemalloc.so.2` (system jemalloc 5.3.1, `MALLOC_CONF`
unset, no `/etc/malloc.conf`, so library defaults, profiling off) and then execs the real node (`ozarchy-p3q-stage/{m,g2}-je`,
same as `ozarchy-p3next-stage`'s). Driver `ozarchy-p3soakje-campaign.sh`: a copy of `ozarchy-p3q-campaign.sh` whose only
change is that CAMP=S SOAK_ALLOC=je writes `ozarchy-p3soakje-*` (so the glibc soak's dirs stay untouched). Cells 00:48-01:20:
w0 (jgm, 60 s, excluded), k1-ctl (jgm, 600 s), k1-s2 (jg2, 600 s). 42 CHECK OK (incl. jemalloc preloaded and mapped on 3
nodes, `MALLOC_CONF` unset), no CHECK FAIL, no memguard stop, no deaths, the host quiet before every cell. k1-s2 is ACCEPT,
AGREE. k1-ctl is rc 2 REJECT on agreement only, from the known mark-only digest artifact: val0's digest took 38 s vs 54 / 53 s,
the 5 mark fields differ on 50 accounts, and books, OI and cash are identical. Liveness PASS. The heap profile at minute 5 vs 10
(18c, mid-run) was skipped because the campaign was already running.

### 53b.1 RSS per node, both allocators (VmRSS, mem-samples.txt, 5 s; bench window 602-603 s)

![RSS per node over time](ozarchy-p3soakje-figs/fig-rss.png)

*Figure 53b-1: VmRSS per node, main dashed, step 2 solid; left glibc (section 53), right jemalloc.*

| alloc | arm | node | mean min 2-5 MB | mean min 5-10 MB | change | @10 min MB | peak MB | slope min 2-5 MB/min | slope last 5 min MB/min (95% CI) |
|---|---|---|---|---|---|---|---|---|---|
| glibc | main | val0 | 3,706 | 3,761 | +55 | 3,796 | 3,835 | +19 | +3.3 [-4.2, +10.8] |
| glibc | main | val1 | 3,879 | 3,943 | +64 | 3,950 | 4,032 | +34 | **+11.1 [+5.5, +16.7]** |
| glibc | main | val2 | 4,664 | 4,735 | +71 | 4,769 | 4,788 | +54 | **+6.4 [+2.3, +10.5]** |
| glibc | step 2 | val0 | 5,536 | 5,885 | **+349** | 6,070 | 6,070 | +132 | **+63.1 [+14.6, +111.6]** |
| glibc | step 2 | val1 | 5,371 | 5,700 | **+329** | 5,934 | 5,934 | +120 | **+67.3 [+47.1, +87.4]** |
| glibc | step 2 | val2 | 6,064 | 6,428 | **+364** | 6,442 | 6,660 | +185 | **+39.8 [+19.1, +60.5]** |
| je | main | val0 | 4,032 | 3,953 | -79 | 3,830 | 4,258 | -22 | -16.5 [-41.4, +8.3] |
| je | main | val1 | 4,015 | 4,060 | +45 | 4,154 | 4,287 | -9 | +8.0 [-11.3, +27.3] |
| je | main | val2 | 4,765 | 4,798 | +33 | 4,958 | 5,245 | -39 | +26.2 [-10.1, +62.6] |
| je | step 2 | val0 | 4,834 | 4,701 | -133 | 4,665 | 5,158 | -87 | +15.3 [-20.6, +51.2] |
| je | step 2 | val1 | 4,717 | 4,578 | -139 | 4,470 | 5,115 | -54 | **+32.7 [+2.7, +62.7]** |
| je | step 2 | val2 | 5,610 | 5,583 | -27 | 5,888 | 6,539 | +4 | +27.9 [-5.6, +61.5] |
| **Phase 3 delta** | glibc | mean | +1.57 GB | **+1.86 GB** | | +1.98 GB | +2.00 GB | | step 2 +57 vs main +7 |
| **Phase 3 delta** | je | mean | +0.78 GB | **+0.68 GB** | | +0.69 GB | +1.01 GB | | step 2 +25 vs main +6 |

Slopes are OLS over the 5 s samples. The CI uses an AR(1) effective n, as in section 53 (`a-soak.py`). Under jemalloc the
last-5-min slope is dominated by the swings (step 2 val0 / val1 dip at minute 6 and partly recover), so the window
means are the better level measure: 5 of 6 jemalloc nodes are flat or lower in minutes 5-10 than in minutes 2-5. Main
under jemalloc sits +63 to +192 MB above main under glibc (minutes 5-10). Step 2 under jemalloc sits 0.85-1.18 GB below
step 2 under glibc. Throughput under jemalloc (single cells): matched/s step 2 199,134 vs main 191,756 (+3.8%),
CPU-s/1M 27.78 vs 29.93 (-7.2%). Under glibc: 172,911 vs 168,384, 28.47 vs 30.35. Both jemalloc arms run ~14-15% more
matched/s than their glibc arms, as in section 51.2 (blocks commit faster, the closed-loop bench offers more).

### 53b.2 RSS next to the Phase 3 layer / checkpoint metrics and the data gauges (both soaks)

![glibc soak: RSS and layer metrics per node](ozarchy-p3soakje-figs/fig-layer-glibc.png)

*Figure 53b-2: glibc soak (section 53 cells), per node (columns), step 2 solid, main grey dashed: VmRSS, flush-worker
queue depth, exec handoff wait, checkpoint drop_inline rate, resting orders, cancel-all index entries, checkpoints,
layer MB per checkpoint, settle wait per block, flush-worker checkpoint time, mempool / exec queue.*

![jemalloc soak: RSS and layer metrics per node](ozarchy-p3soakje-figs/fig-layer-je.png)

*Figure 53b-3: the same for the jemalloc soak.*

Per minute, val0 (val1 / val2 behave the same; all three nodes are in `series.txt` in the tools dir). Cells: RSS MB at
minute end / resting orders M (gauge, minute end) / [step 2: layer MB per checkpoint in the minute / drop_inline in the
minute] / flush-worker depth (mean) / exec handoff wait ms per block. Last column: matched/s glibc main / glibc step 2 /
je main / je step 2.

| min | glibc main | glibc step 2 | je main | je step 2 | matched/s |
|---|---|---|---|---|---|
| 1 | 3,343 / 0.19 / 0.2 / 0.15 | 4,556 / 0.07 / 134 / 235 / 0.1 / 0.01 | 3,805 / 0.38 / 0.2 / 0.49 | 4,535 / 0.13 / 131 / 285 / 0.1 / 0.01 | 191k / 196k / 203k / 216k |
| 2 | 3,657 / 0.33 / 0.3 / 0.19 | 5,267 / 0.60 / 146 / 173 / 0.1 / 0.01 | 4,006 / 0.10 / 0.3 / 0.37 | 4,925 / 0.47 / 165 / 155 / 0.1 / 0.01 | 211k / 218k / 214k / 232k |
| 3 | 3,686 / 0.50 / 0.4 / 0.01 | 5,499 / 0.44 / 109 / 143 / 0.1 / 0.01 | 4,067 / 0.49 / 0.5 / 0.68 | 4,729 / 0.52 / 133 / 168 / 0.1 / 0.01 | 202k / 212k / 216k / 229k |
| 4 | 3,729 / 0.45 / 0.4 / 0.01 | 5,604 / 0.44 / 96 / 166 / 0.0 / 0.01 | 4,100 / 0.33 / 0.5 / 0.01 | 4,858 / 0.42 / 94 / 184 / 0.1 / 0.01 | 191k / 199k / 210k / 233k |
| 5 | 3,743 / 0.41 / 0.4 / 0.03 | 5,673 / 0.29 / 96 / 147 / 0.1 / 0.02 | 4,021 / 0.32 / 0.5 / 0.01 | 4,958 / 0.43 / 82 / 110 / 0.1 / 0.03 | 188k / 193k / 217k / 235k |
| 6 | 3,751 / 0.27 / 0.3 / 0.01 | 5,835 / 0.24 / 92 / 142 / 0.1 / 0.01 | 3,916 / 0.45 / 0.3 / 0.01 | 4,423 / 0.26 / 100 / 170 / 0.1 / 0.01 | 165k / 162k / 208k / 189k |
| 7 | 3,717 / 0.27 / 0.2 / 0.01 | 5,843 / 0.16 / 68 / 137 / 0.0 / 0.01 | 3,879 / 0.31 / 0.3 / 0.16 | 4,713 / 0.25 / 102 / 218 / 0.1 / 0.01 | 132k / 139k / 156k / 177k |
| 8 | 3,753 / 0.31 / 0.2 / 0.13 | 5,872 / 0.20 / 84 / 183 / 0.1 / 0.01 | 4,083 / 0.53 / 0.4 / 0.56 | 4,732 / 0.41 / 112 / 86 / 0.1 / 0.02 | 139k / 138k / 183k / 170k |
| 9 | 3,749 / 0.30 / 0.1 / 0.01 | 5,976 / 0.37 / 105 / 160 / 0.1 / 0.01 | 3,961 / 0.38 / 0.4 / 0.41 | 4,655 / 0.30 / 121 / 199 / 0.0 / 0.04 | 135k / 138k / 160k / 154k |
| 10 | 3,794 / 0.28 / 0.2 / 0.01 | 6,071 / 0.24 / 108 / 159 / 0.0 / 0.01 | 3,899 / 0.37 / 0.3 / 0.24 | 4,664 / 0.27 / 128 / 216 / 0.1 / 0.01 | 134k / 133k / 154k / 160k |

What the metrics show (both soaks, all nodes):

* **Layer bytes do not grow.** Step 2 checkpoints ~5 times per minute (one per ~12 s; 47-49 per node per cell). The
  layer per checkpoint is 130-170 MB in minutes 1-2 (start-up books) and 70-140 MB afterwards, with no upward trend
  (glibc 116 -> 93 MB mean, first vs last 5 min; jemalloc 117 -> 112 MB). `layer_shared` is 0. drop_inline is 1.5-4 per s
  after the first minute (more in minute 1). Settle wait is 18-29 ms per native block (glibc 26.9 -> 18.0 ms on
  val0, first vs last 5 min; jemalloc 21.2 -> 18.8). The flush-worker checkpoint time is 0.4-0.7 s.
* **The book does not grow either.** The resting-orders gauge is 0.2-0.6 M per node in both arms. It peaks at minute 2-3
  and falls after minute 5, when the margin rejections start (53b.3); minute 3 -> 10 it goes 0.44 -> 0.24 M (glibc step
  2 val0) and 0.52 -> 0.27 M (jemalloc). The cancel-all index entries (0.5-1.1 M) and the resident rows (214 MB, flat from
  minute 1) behave the same way.
* **Flush-worker queue and handoff:** on step 2 the flush-worker depth averages 0.0-0.1 blocks (main 0.1-0.5). The exec
  handoff wait is 0.01-0.04 ms per block (main 0.01-0.7, with two ~5 ms minutes on jemalloc main). So the flush-worker
  queue holds no backlog of blocks (and none of their books) in these runs.
* **RSS tracks neither.** Over minutes 3-10, glibc step 2's RSS rises +572 / +680 / +491 MB while its resting orders fall
  (Pearson r of RSS level vs resting-orders level -0.44 to -0.60) and its layer size is flat or falling. Under jemalloc,
  with the same layer and book profiles, RSS is flat. The only correlation is under glibc: the per-minute RSS increase
  correlates with that minute's layer size (r +0.62 to +0.73; jemalloc -0.12 to +0.15). Minutes with larger layers push
  the glibc high-water mark further, which is the pattern of allocator retention around the per-checkpoint rebuild
  (section 48: ~half of each Phase 3 delta is glibc-only). It is not data that accumulates.
* **Not scraped / not exported:** there is no gauge of live layer bytes or layer entries (`exec_checkpoint_layer_bytes`
  is a histogram of each new layer's size, not of the layers held), no read-view count, pin or age metric, and no per-book
  copy count. So the "layers / views stay capped at 1-2 windows" result of 18c's retention test cannot be checked from
  these cells directly. It is consistent with everything above.

### 53b.3 The matched/s drop after minute 6: generator or node?

![generator vs node per minute](ozarchy-p3soakje-figs/fig-load.png)

*Figure 53b-4: per minute, val0 counters, both soaks, both arms: bench submit vs processed actions, placed vs
margin-rejected orders, matched/s, nonce-expired actions, exec queue and busy, mempool, CPU (node mean, bench).*

| min | margin-rejected orders/s (val0): glibc main / step 2, je main / step 2 | placed accepted k/s (same order) | processed actions/s (same order) | expired per min (same order) | exec busy (same order) |
|---|---|---|---|---|---|
| 1-5 | 0 / 0 / 0 / 0 | 248-276 / 254-286 / 267-285 / 284-308 | 978-1,081 / 1,005-1,127 / 1,018-1,146 / 1,112-1,253 | 0-3.3k / 0-3.6k / 0-3.2k / 0-3.1k | 0.85-0.98 / 0.89-1.00 / 0.96-0.99 / 0.98-1.00 |
| 6 | 18.7k / 40.2k / 4.7k / 67.5k | 226 / 226 / 276 / 267 | 1,007 / 1,067 / 1,127 / 1,367 | 3.3k / 3.4k / 1.8k / 3.1k | 0.82 / 0.86 / 0.99 / 1.00 |
| 7-10 | 106-135k / 106-139k / 89-139k / 124-154k | 175-184 / 178-188 / 202-240 / 204-233 | 1,153-1,247 / 1,187-1,273 / 1,287-1,362 / 1,433-1,465 | 0.3-2.6k / 0.8-2.0k / 0.1-2.5k / 0.3-1.1k | 0.78-0.82 / 0.81-0.93 / 0.97-0.99 / 0.97-1.01 |

* **What changes at minute 6 is the chain's margin rejections.** `torus_orders_rejected_margin_total` is 0 for 5 min in
  all four cells. It starts in minute 6 and reaches 105-155k orders/s in minutes 7-10, i.e. ~40% of the orders sent.
  Placed-accepted orders fall by the same amount, and fills per processed action fall from ~190 to ~105-140. No other
  reject counter moves: open-limit, book and other are 0, and cancelled is < 0.1k/s after minute 1.
* **The generator is not the limit.** Bench submits rise after minute 6 (glibc ~1,000 -> ~1,250/s, jemalloc ~1,100-1,300 ->
  ~1,250-1,500/s), and so do the processed actions. Bench CPU is 0.9-1.4 cores, flat. The bench is not saturated: it is
  closed-loop and sends faster once blocks carry fewer fills.
* **The node is not slower.** Node CPU is flat at 4.3-6.5 cores per node. Under jemalloc the exec thread stays saturated
  (busy 0.97-1.00, exec queue at its ~60 cap) before and after the drop, so per-block capacity is unchanged. Each block
  just yields fewer fills. The mempool stays at ~5-6k.
* **The cancel-all expiry is not the direct cause.** Nonce-window expiry starts in minute 3 (2-3.6k actions per minute in
  all cells; per cell ~18k per node under glibc, ~13.4-13.7k under jemalloc), 3 min before the drop, and it declines
  while the drop persists. Expired cancel-alls also do not show up as a growing book: the resting-orders gauge falls after
  minute 5.
* **Answer (calibrated):** high confidence that the drop is workload-side. The econ workload grows each sender's positions
  one way (`econ_side_is_buy`, no closing fills), so after ~5 min of trading (minute 6 in both
  allocators and both arms) a growing share of senders run out of free margin, and the chain rejects their orders. It is
  neither the generator's send rate nor a node slowdown. What this data cannot settle is whether the margin is used up by
  positions or by open-order reservations that an expired cancel-all failed to release. The end-of-run digest covers only 50
  sampled accounts (6 of 50 with < 5% free margin, p10 1.4%, median 75%). **Metric that would settle it:** per sender,
  the position margin vs the open-order reservation (`totalMarginUsed` split) for all senders at minutes 5 and 7, or margin
  rejects counted per sender by the bench. The cancel-share soak pair (next in the queue) gives a partial test: if fewer
  cancel-alls expire and the minute-6 drop stays, it is positions.

Other observations: (a) under glibc, val0 / val1's exec queue drains to ~2 blocks by minute 4. Exec busy falls to
0.78-0.90 and native blocks/s to ~2.5 in minutes 4-6, so the glibc chain was input-bound well before the margin drop. Under jemalloc the
queue stays at the ~60 cap and blocks/s are 2.7-3.7 (single cells; consistent with section 51.2's faster commits under
jemalloc). (b) val2's exec queue gauge sits at ~65 in all four cells (glibc too), so val2 stays at the queue cap
throughout. That backlog is the commit-mode part of its higher RSS (+0.5-1.0 GB over val0 / val1, all four cells; section 48).

**Verdict:** the jemalloc soak points to **allocator retention under glibc** as the main cause of section 53's growth
(consistent with it, not proof: one 10 min pair, and no live-layer or read-view gauge). Under jemalloc step 2's RSS level
is flat after ~2 min (minutes 5-10 mean below minutes 2-5 on all three nodes), but the last-5-min slopes are still positive
(+15 / +33 / +28 MB/min; val1's CI [+3, +63] excludes 0), so slow data growth within jemalloc's +-300 MB swings is not
ruled out; a longer soak would settle it. Phase 3 delta **+0.68 GB per node (minutes 5-10
mean; +1.01 GB at the peak)**. That meets the owner's "levels off, delta <= ~1 GB/node" rule on the level and on the mean,
and is borderline at the peak (val2 +1.29 GB). It is still ~2.5x the old ~270 MB gate. Neither the layer size nor the book
(resting orders) grows over the soak. The minute-6 matched/s drop is margin exhaustion in the econ workload, not
the generator's rate and not the node.

Analysis (`tools/ozarchy-p3soakje-tools/analyst/`): `a-series.py` (per-minute generator vs node table, per node per
minute RSS / resting / flush-worker depth / handoff / layer / drop_inline, figures as SVG -> PNG via `rsvg-convert`),
`a-soak.py` (section 53, unchanged), `a-cells.py` (digest classes), `a-archive.py`; outputs `series.txt`, `soak.txt`.
Full outputs (incl. `series.json`, `cells.json`, figure SVGs) in `ozarchy-p3soakje-run/analyst/` on ozarchy.

## 53c. Phase 3 soak with the cancel-share fix: `41a95355` vs step 2 `7b006739`, 600 s per arm, glibc (campaign ozarchy-p3cssoak, 2026-10-13)

**With the cancel-share fix no cancel-all expires, and the minute-6 margin rejections do not appear at all.** On the fixed
arm, 0 actions were evicted for the nonce window on each of the 3 nodes, and 199,812 of the 199,813 cancel-alls sent were
executed. On step 2 in the same campaign, 16,168 expired per node, which is section 53's ~18k again. Margin-rejected
orders: 0 over the whole fixed cell. On step 2 they are 37k/s in minute 6 and 107-134k/s in minutes 7-10. matched/s on
the fixed arm is at 179-187k/s in minutes 6-10. On step 2 it goes from 209k/s (minutes 1-5) to 144k/s (minutes 6-10).
**Under glibc, step 2's RSS does not level off with the fix. It grows faster:** the last-5-min slopes are +129 / +178 /
+138 MB/min per node, and all three CIs exclude 0. Step 2 in this pair: +41 / +42 / +85. Against section 53's main the
Phase 3 delta is **+2.76 GB per node** (minutes 5-10 mean; +3.07 GB at 10 min), vs +1.85 GB for step 2 in this pair.
**That RAM comparison is not like for like.** The fixed arm carries the full workload for all 10 min: resting orders are
0.64-0.73 M vs 0.15-0.36 M in minutes 6-10, and the layer per checkpoint is 220-240 MB vs 100-110 MB. The main reference cell (section 53)
ran the degraded workload. **The margin user behind the minute-6 drop is positions, not open-order reservations.** In every
soak cell the end-of-run digest shows order margin at <= 0.5% of account value on all 50 sampled senders. Under the old
cancel order the positions concentrate on low-address senders: their position IM is 74-88% of account value in the lowest
address quartile vs 6-9% in the highest. With the fix it is 35-39% in every quartile (53c.4). One pair, single cells: the
expiry and margin results are large and clean, while the RSS and mechanism readings are "consistent with".

Owner queue item 2 (via 18c): the soak pair, cancel-share fix vs step 2, at interval 15 s on both arms, glibc (the build
allocator), commit tail. The questions: does the fix stop the cancel-all expiry and the book growth, and does step 2's RSS
then level off under glibc? Nodes: step 2 `7b006739` (`ca388315`, the section 53 binary, `ozarchy-p3q-stage/g2-run`), and
cs2 = `test/p3-cancel-share` `41a95355` (`2655ab56`, build id `92529c89`). cs2 is `fix/cancel-share-oldest-first`
`de97682f` merged onto step 2 `7b006739`, 4 commits, `native_pool.rs` only. It was built clean in
`wt/p3q-cs2-41a95355` -> `~/.cargo-target-p3q-cs2-41a95355` with the p3q flags (frame pointers, line tables, mold;
`ozarchy-p3cssoak-build.sh` = `ozarchy-p3q-build.sh` + arm key cs2). Bench `05d44313`, harness `218ad251`, tw, Classic,
300 markets, MAX_IN_FLIGHT 4, budget 900, shared NVMe. Driver `ozarchy-p3cssoak-campaign.sh`: a copy of
`ozarchy-p3soakje-campaign.sh` with CAMP=X, arm cs2 (knob on, no preload), run dir / prefix `ozarchy-p3cssoak-*`, and a
preflight chain check (step 2 = ancestor of cs2). Cells 01:38-02:10: w0 (step 2, 60 s, excluded), k1-g2 (step 2, 600 s),
k1-cs2 (cs2, 600 s), fixed 10 min, no extensions. 42 CHECK OK (incl. knob on 3 nodes, enable line, checkpoints 54-56 / 47-48 per node, glibc with no preload), no CHECK FAIL, no memguard stop, no deaths, host quiet before every cell. k1-cs2 and
w0 are ACCEPT, AGREE. k1-g2 is rc 2 REJECT on agreement only. This is the known mark-only digest artifact: val2's digest
took 51.6 s vs 34.3 / 33.6 s, the 5 mark fields differ on 50 accounts, and books, OI and cash are identical. Liveness PASS.
Comparison cells: section 53's glibc soak (main `219f0c20` = the RAM reference, step 2 `7b006739`), same binaries for step 2.

### 53c.1 Cancel-all expiry (the main result)

| cell | cancel-alls sent | executed (`torus_exec_cancel_all_total` delta, each node) | sent - executed | nonce-expired evictions per node (node logs) | first expiry | bench releases by 70 s timeout |
|---|---|---|---|---|---|---|
| step 2, this pair (k1-g2) | 271,266 | 255,098 | 16,168 | 16,168 / 16,168 / 16,168 | bench + 132-133 s | 14,711 |
| **cs2 (k1-cs2)** | 199,813 | 199,812 | **1** | **0 / 0 / 0** | none | **0** |
| step 2, section 53 | 272,157 | 253,540 | 18,617 | 18,615 / 18,615 / 18,694 | bench + 134 s | 17,378 |
| main, section 53 | 263,841 | 245,885 | 17,956 | 17,956 (x3) | bench + 138-139 s | 17,956 |

On the step 2 and main cells, sent - executed equals the evictions, so every evicted action is a cancel-all, as in
section 47.3. The one cs2 cancel-all not executed is the RPC-refused one (sent 199,813, accepted 199,812). Expiry per
minute on step 2 (val0, minutes 3-10): 3.1k, 3.3k, 2.7k, 1.6k, 1.2k, 2.0k, 0.7k, 1.5k. On cs2: 0 in every minute on every
node. Cancel-alls executed per second: cs2 284-413, step 2 375-469. cs2 sends fewer: 34.8% of the mix vs 38.5%. The bench
is closed-loop, and cs2's senders send fewer actions (953/s vs 1,171/s) consistent with its slower block commits (2.60 vs 3.03 blocks/s, below).
**Book growth: the fix does not shrink the book. The book is larger with it.** The resting-orders gauge on cs2 is 0.65-0.73
M per node from minute 3 to 10, with one dip to 0.47 M at minute 7. Step 2 is at 0.26-0.56 M in minutes 2-5, then 0.15-0.36 M
once the margin rejections start. 18c has withdrawn the book-growth / expiry link. These cells are consistent with that:
without expiry, the book is still bigger, because placement never collapses.

### 53c.2 RSS per node (VmRSS, mem-samples.txt, 5 s; bench window 602-603 s)

![RSS per node: cs2 vs step 2, with section 53's cells](ozarchy-p3cssoak-figs/fig-rss.png)

*Figure 53c-1: VmRSS per node. cs2 blue, step 2 of this pair red, section 53's step 2 orange dashed and main grey dashed
(all glibc).*

| cell | node | mean min 2-5 MB | mean min 5-10 MB | change | @10 min MB | peak MB | slope min 2-5 MB/min | slope last 5 min MB/min (95% CI) | vs main (s53) mean min 5-10 / @10 / peak GB |
|---|---|---|---|---|---|---|---|---|---|
| main (s53) | val0 | 3,706 | 3,761 | +55 | 3,796 | 3,835 | +19 | +3.3 [-4.2, +10.8] | reference |
| main (s53) | val1 | 3,879 | 3,943 | +64 | 3,950 | 4,032 | +34 | **+11.1 [+5.5, +16.7]** | reference |
| main (s53) | val2 | 4,664 | 4,735 | +71 | 4,769 | 4,788 | +54 | **+6.4 [+2.3, +10.5]** | reference |
| step 2 (s53) | val0 | 5,536 | 5,885 | +349 | 6,070 | 6,070 | +132 | **+63.1 [+14.6, +111.6]** | +2.07 / +2.22 / +2.18 |
| step 2 (s53) | val1 | 5,371 | 5,700 | +329 | 5,934 | 5,934 | +120 | **+67.3 [+47.1, +87.4]** | +1.72 / +1.94 / +1.86 |
| step 2 (s53) | val2 | 6,064 | 6,428 | +364 | 6,442 | 6,660 | +185 | **+39.8 [+19.1, +60.5]** | +1.65 / +1.63 / +1.83 |
| step 2 (this pair) | val0 | 5,484 | 5,769 | +285 | 5,833 | 6,009 | +103 | **+40.8 [+33.7, +48.0]** | +1.96 / +1.99 / +2.12 |
| step 2 (this pair) | val1 | 5,345 | 5,531 | +186 | 5,696 | 5,750 | +145 | **+42.0 [+16.9, +67.1]** | +1.55 / +1.71 / +1.68 |
| step 2 (this pair) | val2 | 6,389 | 6,815 | +426 | 6,929 | 7,011 | +134 | **+84.5 [+69.5, +99.5]** | +2.03 / +2.11 / +2.17 |
| **cs2** | val0 | 5,995 | 6,540 | **+545** | 6,895 | 6,969 | +198 | **+129.3 [+112.9, +145.7]** | **+2.71 / +3.03 / +3.06** |
| **cs2** | val1 | 5,848 | 6,719 | **+871** | 7,126 | 7,193 | +361 | **+178.0 [+160.0, +196.1]** | **+2.71 / +3.10 / +3.09** |
| **cs2** | val2 | 6,892 | 7,650 | **+757** | 7,940 | 8,137 | +359 | **+137.6 [+121.8, +153.5]** | **+2.85 / +3.10 / +3.27** |
| **Phase 3 delta vs main (s53), mean of 3** | step 2 s53 | | | | | | | | +1.81 / +1.93 / +1.96 |
| | step 2 this pair | | | | | | | | +1.85 / +1.93 / +1.99 |
| | **cs2** | | | | | | | | **+2.76 / +3.07 / +3.14** |

Slopes: OLS over the 5 s samples. The CI uses an AR(1) effective n, as in sections 53 / 53b (`a-series.py`). cs2 minus
step 2 (this pair): +0.75 / +1.16 / +0.82 GB (minutes 5-10 mean), +1.04 / +1.40 / +0.99 GB at 10 min. Step 2 in this pair
reproduces section 53's step 2 closely (delta +1.85 vs +1.81 GB, expiry 16.2k vs 18.6k, margin drop from minute 6). The
replication supports both cells. On the RAM rule (levels off, delta <= ~1 GB/node vs main's glibc soak), cs2 fails on both
counts. But the reference is main **with** the expiry and the margin collapse. The like-for-like control is main + the
fix, and that binary already exists: `de97682f`, `ozarchy-p3q-stage/cs-run`, md5 `e4b34063`.

### 53c.3 Per minute per node (val0; val1 / val2 are the same within a few %, all in `series.txt`)

![per node per minute: expiry, cancel-alls, margin rejections, matched/s, book, pool, select time, layer, flush queue](ozarchy-p3cssoak-figs/fig-node.png)

*Figure 53c-2: per node (columns), per minute: nonce-expired evictions, cancel-alls executed/s, margin-rejected orders/s,
matched/s, resting orders, cancel-all index entries, mempool size, block-build select ms, layer MB per checkpoint and
flush-worker depth. Same colours as figure 53c-1.*

Cells: RSS MB at minute end / matched/s / expired / margin-rejected orders/s / resting orders M (minute end) / layer MB
per checkpoint / drop_inline in the minute / mempool (mean) / select ms per build.

| min | step 2 (this pair), val0 | cs2, val0 |
|---|---|---|
| 1 | 4,519 / 205k / 0 / 0 / 0.09 / 128 / 283 / 3.8k / 14.9 | 4,768 / 201k / 0 / 0 / 0.24 / 145 / 246 / 4.6k / 13.0 |
| 2 | 5,202 / 227k / 0 / 0 / 0.56 / 140 / 132 / 7.3k / 21.3 | 5,528 / 202k / 0 / 0 / 0.30 / 191 / 188 / 6.7k / 17.6 |
| 3 | 5,479 / 216k / 3,119 / 0 / 0.48 / 122 / 179 / 5.4k / 21.0 | 5,991 / 215k / 0 / 0 / 0.73 / 186 / 126 / 5.9k / 23.1 |
| 4 | 5,540 / 194k / 3,346 / 0 / 0.26 / 97 / 140 / 5.3k / 20.3 | 6,065 / 191k / 0 / 0 / 0.72 / 234 / 156 / 6.0k / 22.0 |
| 5 | 5,630 / 204k / 2,666 / 0 / 0.43 / 112 / 129 / 5.9k / 20.4 | 6,242 / 187k / 0 / 0 / 0.71 / 224 / 119 / 6.0k / 20.3 |
| 6 | 5,685 / 152k / 1,629 / 37.5k / 0.15 / 92 / 146 / 5.5k / 22.1 | 6,308 / 182k / 0 / 0 / 0.64 / 237 / 121 / 6.0k / 20.4 |
| 7 | 5,780 / 150k / 1,167 / 107.1k / 0.36 / 101 / 176 / 6.0k / 20.4 | 6,430 / 179k / 0 / 0 / 0.47 / 226 / 124 / 6.3k / 20.2 |
| 8 | 5,822 / 144k / 2,035 / 120.2k / 0.24 / 98 / 131 / 5.5k / 20.9 | 6,617 / 187k / 0 / 0 / 0.69 / 219 / 119 / 6.3k / 22.1 |
| 9 | 5,800 / 134k / 749 / 133.6k / 0.23 / 88 / 175 / 5.5k / 20.8 | 6,650 / 180k / 0 / 0 / 0.65 / 236 / 125 / 5.9k / 22.3 |
| 10 | 5,833 / 140k / 1,457 / 132.7k / 0.16 / 108 / 186 / 5.2k / 24.6 | 6,895 / 183k / 0 / 0 / 0.69 / 238 / 123 / 5.9k / 20.4 |

* **Flush-worker depth and handoff:** 0.0-0.1 blocks and 0.01 ms per block on both arms, every minute, every node (cs2 0.5 in
  minute 1). No backlog of blocks, and so none of their books, is held in the flush queue on either arm.
* **Layer per checkpoint:** cs2 186-238 MB from minute 2, mean 212 MB (early 191-195, late 229-231 MB per node). Step 2: 88-141
  MB, mean 108-112 MB. cs2 checkpoints 42-43 times per node in the window (one per ~14 s), step 2 48-51 times. Settle wait per
  native block: cs2 29-33 ms early, 28-44 ms late. Step 2: 25-28 ms early, 17-23 ms late. `layer_shared` 0 on both arms.
  drop_inline: cs2 ~120 per minute, step 2 130-186.
* **Pool and selection:** mempool 6.0-6.1k (cs2) vs 5.6-5.7k (step 2), means over minutes 2-10. Block-build select time per
  build (`torus_block_build_select_seconds`, proposer builds only, minutes 2-10): cs2 20.8 / 24.5 / 19.4 ms, step 2 21.3 /
  23.5 / 19.6 ms per node. The oldest-first cancel merge shows no selection cost at this pool size.
* **Throughput (single cells, no CI):** matched/s whole window cs2 190,776 vs step 2 176,544 (+8.1%). Minutes 1-5: 199k vs
  209k (-4.7%). Minutes 6-10: 182k vs 144k. Native blocks/s 2.60 vs 3.03 (minutes 6-10: 2.23 vs 2.99). Exec per block p50
  388 vs 327 ms, p99 640 vs 596 ms. Fills per processed action: cs2 187-214 throughout. Step 2: 184-202 in minutes 1-5, then 108-144 from minute 6. Node CPU-s/1M (cpu-ticks): 24.46 vs 27.78. Step 2 also spends CPU on ~32M margin-rejected orders that never fill.
  cs2's slower blocks carry more fills (bigger books, no rejects), so it is not a like-for-like throughput A/B.
* **What RSS tracks on cs2:** resting orders and layer size are flat from minute ~3 (0.65-0.73 M, 220-240 MB), yet RSS keeps
  rising +130-180 MB/min. That matches section 53b's glibc pattern (flat data, rising RSS), now with layers ~2x larger.
  It is consistent with glibc retention scaling with the layer size, not proof. There is no live-layer-bytes or read-view
  gauge (53b.2). A cs2 cell under jemalloc would separate retention from data growth.

### 53c.4 The minute-6 margin rejections: by sender and by reason (18c request)

**What the data records, and what it does not.**

* Node: `torus_orders_rejected_margin_total` is one chain-wide counter. At `41a95355` it is incremented by
  `RejectReason::count` (`crates/torus-bridge/src/native_executor.rs:842`, phase-2 prepare path, reject built at
  `:7218-7223`, counted at `:7336`) and at the serial path `:9423`. It carries no reason split and no sender. The reject
  text "insufficient margin: need N, have F (account)" (`account_check`, `:8618-8621`) goes only into the action's result,
  not into a log or a metric. Its `have` is already net of the reservations: `free = bal.available + pos_net` (`:9412`;
  `bal.available + pn + credit - st.committed` at `:7215`), where `available` excludes the order margin and `pos_net` = UPnL
  - position IM. So even the result text cannot split the cause. The node logs carry no per-reject line.
* Bench: commit-tail mode releases in-flight slots from block bodies, not action results. It scrapes only the chain-wide
  counter (`tools/bench-throughput/src/main.rs:2936`). There is no per-sender reject count.
* Harness: the state digest (`tools/matched-bench/digest-node.sh:66`, `torus_getBalances`) samples 50 of the 5,000 senders,
  once, after the bench and the drain. So **no per-minute and no per-sender reject split exists for minutes 5 vs 7**.
  Per minute there is only the chain-wide count, identical on the 3 nodes: step 2 0 (minute 5) -> 37.5k/s (6) ->
  107.1k/s (7) -> 133k/s (10); cs2 0 in every minute.

**What the end-of-run digest does show (50 senders, val0; `a-margin.py`).** Per account, `torus-rpc` `account.rs` gives
`native_balance = available + order_margin`, `cash_balance = available`, and `total_margin_used = order_margin + position
IM`. So order margin = nativeBalance - cashBalance, and position IM = totalMarginUsed - order margin.

| cell | free margin p10 / median (% of account value) | order margin median / max | position IM median / max | senders < 5% free | ... would be >= 5% free without their reservations | position IM by address quartile (low -> high) |
|---|---|---|---|---|---|---|
| main s53 (glibc) | 23.0 / 85.3% | 0.00 / 0.17% | 16 / 100% | 2 | 0 | 75 / 31 / 8 / 6% |
| step 2 s53 (glibc) | 16.6 / 82.4% | 0.00 / 0.21% | 19 / 100% | 5 | 0 | 79 / 30 / 9 / 6% |
| main 53b (jemalloc) | 15.2 / 81.1% | 0.00 / 0.46% | 20 / 100% | 5 | 0 | 85 / 37 / 14 / 9% |
| step 2 53b (jemalloc) | 1.4 / 74.8% | 0.00 / 0.28% | 28 / 100% | 6 | 0 | 88 / 41 / 13 / 8% |
| step 2, this pair | 25.3 / 79.3% | 0.00 / 0.14% | 22 / 100% | 2 | 0 | 74 / 34 / 11 / 6% |
| **cs2** | **61.0 / 63.5%** | 0.00 / 0.18% | **37 / 40%** | **0** | 0 | **39 / 37 / 36 / 35%** |

* **Reason: positions.** The open-order reservation is <= 0.5% of account value on every sampled sender in all six soak
  cells. Every sender below 5% free margin sits at 96-100% position IM, and none would clear 5% without its reservations. So
  margin is used up by positions, not by reservations that an expired cancel-all failed to release. Caveats: this is the
  state after the drain, not minutes 5 / 7, and 50 senders. But the same 50 addresses are sampled in every cell, and the
  pattern is the same in all five cells with the drop.
* **By sender: low addresses.** The exhausted senders are the lowest addresses in the sample (address / 2^160 = 0.04-0.17).
  Under the old cancel share (cancels walked in sender address order, section 47.3 / `de97682f`), position IM falls
  steeply with the address: 74-88% in the lowest quartile, 6-9% in the highest. With oldest-first cancels it is flat at
  35-39%. A mechanism consistent with this, not proven: low-address senders' cancel-alls always commit, so they cycle
  their 4 in-flight slots at full rate. High-address senders' cancel-alls wait and then expire, and each expired one holds
  a slot for up to 70 s. So order flow, fills and one-way positions (the econ workload has no closing fills)
  concentrate on the low addresses. Those reach full IM around minute 5-6. After that their fast-cycling orders are
  rejected, which is 35-43% of all orders in minutes 7-10. With the fix, the same position build-up is spread over all 5,000 senders: 37%
  median IM at 10 min, and nobody is exhausted. If the build-up is linear, the uniform book would reach exhaustion much
  later (~25-30 min). That is an extrapolation, not measured.
* **Answer to 18c's partial test:** fewer cancel-alls expired (0), and the drop did **not** stay. Read naively, that points
  at reservations. But the digest shows the reservations are ~0 and the margin is position IM. So the cause is the old
  cancel order's unfairness concentrating positions, not reservations held by expired cancel-alls.

**The smallest instrumentation that would settle minutes 5 vs 7 directly** (no consensus or format change):

1. Node, reason split (two counters): in the two margin-reject arms (`native_executor.rs:7218-7223`, phase-2 prepare, and
   `:9418-9425`, serial path), on the reject path only, re-run `account_check` with `free + bal.order_margin` (phase 2:
   also `+ st.committed`, the same block's earlier orders). If it passes, count
   `torus_orders_rejected_margin_reserved_total`; otherwise `torus_orders_rejected_margin_position_total`. Register both
   next to `orders_rejected_margin` (`crates/torus-telemetry/src/lib.rs:135`, `:1218-1222`, `:2623`), and add them to the
   sampler's `WIDE_COLS` (`tools/matched-bench/run-cell.sh:781`) for per-second series.
2. Per sender, harness only: run the digest's `torus_getBalances` call (`digest-node.sh:66`) for all 5,000 senders at bench
   t = 300 s and 420 s, into `balances-t300.jsonl` / `-t420.jsonl`. That gives position IM vs order margin per sender at
   minutes 5 and 7. Per-sender reject counts would also need a bench-side read of the action results, or a sender label.
   A Prometheus label for 5,000 senders is not advisable.

### 53c.5 Verdict

* **Expiry: fixed in this soak.** 0 cancel-all expiries on 3 of 3 nodes over 600 s (step 2: 16,168 per node), and all
  sent cancel-alls but the one RPC-refused were executed. One cell. The effect is large and binary.
* **Minute-6 drop: gone with the fix in this soak.** 0 margin rejections and matched/s 179-187k/s in minutes 6-10. The margin
  user behind the drop is positions, concentrated on low-address senders by the old cancel order. Reservations are ruled out
  for the 50 sampled senders at the end of the run. The per-minute / per-sender split needs the instrumentation above.
* **RSS under glibc with the fix: does not level off in 10 min.** The slopes are +129 / +178 / +138 MB/min (CIs exclude 0),
  and the delta vs section 53's main is +2.76 GB per node (minutes 5-10 mean; +3.07 at 10 min, +3.14 peak). The owner's rule
  (levels off, <= ~1 GB/node) is **not met against that reference**. The comparison is confounded, though: the fixed arm runs
  ~2x the resting orders and ~2x the layer size for the whole 10 min, while main's reference cell collapsed to the margin-
  rejected workload. Next measurement that would settle it (not started; merge-wait): a glibc soak pair cs2 `41a95355` vs
  main + fix `de97682f` (`cs-run`, already staged). With a cs2 jemalloc cell, that separates retention from data.

Analysis (`tools/ozarchy-p3cssoak-tools/analyst/`): `a-series.py` (copy of 53b's, plus per node per minute expiry from each
node log, cancel-alls executed, margin-rejected and placed orders/s, mempool, select ms, and the sent / executed cancel-all
balance; figures SVG -> PNG via `rsvg-convert`), `a-margin.py` (digest margin split and address quartiles), `a-cells.py`
(53b's, digest classes, throughput), `a-archive.py`. Outputs `series.txt`, `margin.txt`. Full outputs (incl. `series.json`,
`cells.json`, SVGs) are in `ozarchy-p3cssoak-run/analyst/` on ozarchy. Driver and build script:
`tools/ozarchy-p3cssoak-tools/`.

## 53d. Fair Phase 3 RAM soak, the cancel-share fix in every arm: `41a95355` (glibc and jemalloc) vs main + fix `de97682f`, 600 s per arm (campaign ozarchy-p3fsoak, 2026-10-13)

**With the fix in every arm, all three cells run the full workload for 10 min: 0 cancel-all expiries, 0 margin rejections
and the same book (0.54-0.76 M resting orders per node from minute 3) in each cell. Under glibc, step 2 + fix RSS does not
level off against main + fix. The Phase 3 delta is +2.62 GB per node (minutes 5-10 mean; +2.92 GB at 10 min, +3.02 GB
peak). The delta itself still grows +120 to +158 MB/min over the last 5 min, and all three CIs exclude 0. Under jemalloc,
the same binary levels off: the delta vs main + fix is flat (last-5-min slopes -18 to +11 MB/min, all CIs span 0) at
+1.10 GB per node (minutes 5-10 mean; +1.16 GB at 10 min, +1.49 GB peak).** Owner RAM rule (levels off, delta <= ~1
GB/node): **not met under glibc. Under jemalloc it is met on the level and at about the bound on the mean, and not met at
the peak.** The jemalloc delta also contains the allocator (main + fix ran on glibc) and val0 / val1's exec backlog (see
53d.1). With the same book and the same layer size, glibc RSS grows while jemalloc RSS is flat. So the glibc growth is
consistent with allocator retention, not with data growth, now at full load. One cell per arm: the deltas and slopes are
large and their CIs are clean, but this is "consistent with", not proof.

Owner go (via 18c): a fair soak of Phase 3's RAM cost with the cancel-share fix in both arms, so that the control runs
the full workload too (section 53c's reference, main, had collapsed to the margin-rejected workload). Arms: **mf** = main +
fix `de97682f` (`fix/cancel-share-oldest-first`, main `219f0c20` + 3 commits; node `e4b34063`, build id `90c6f729`,
staged `ozarchy-p3q-stage/cs-run`; built in s50, unused until now), glibc, no checkpoint knob (control). **cs2** = `41a95355`
(`de97682f` merged onto step 2 `7b006739`; `2655ab56`, the section 53c binary), glibc, `TORUS_CHECKPOINT_INTERVAL_MS=15000`.
**jcs2** = the same `41a95355` node through a new wrapper `ozarchy-p3q-stage/cs2-je` that only sets
`LD_PRELOAD=/usr/lib/libjemalloc.so.2` (system jemalloc 5.3.1, `MALLOC_CONF` unset, profiling off) and execs the real
node, exactly as section 53b's jg2. Knob 15 s. The fix is in both binaries: the oldest-first cancel merge's function names
(`cancel_queue_head`, `take_cancels`) are in the line tables of `cs-run` and `cs2-run`, and in neither `m-run` (main) nor
`g2-run` (step 2). The de97682f worktree had been removed after its s50 build (the staged binary survived; md5 and build
id match `ozarchy-p3q-build/md5s-all.txt`). It was re-created clean at `de97682f` (`wt/p3q-cs-de97682f`) for the
preflight's source check. Bench `05d44313`, harness `218ad251`, tw, Classic, 300 markets, MAX_IN_FLIGHT 4, budget 900,
commit tail, shared NVMe, memguard (MemAvailable < 6 GB stops the cell). Driver `ozarchy-p3fsoak-campaign.sh`: a copy of
`ozarchy-p3cssoak-campaign.sh` with CAMP=F, arms mf (key cs, knob off, glibc) and jcs2 (key cs2, knob on, je), run dir /
prefix `ozarchy-p3fsoak-*`, and a preflight chain check (main -> main + fix -> step 2 + fix). Cells 02:38-03:24: w0 (mf,
60 s, excluded), k1-mf, k1-cs2, k1-jcs2 (600 s each, fixed, no extensions). **All 4 cells rc 0, ACCEPT, AGREE** (no
digest artifact this time). 56 CHECK OK (incl. no knob and no enable line on mf, knob + enable line + 42-48 checkpoints
per node on cs2 / jcs2, glibc with no preload on mf / cs2, jemalloc preloaded and mapped on 3 nodes with `MALLOC_CONF`
unset on jcs2), no CHECK FAIL, no memguard stop, no deaths, the host quiet before every cell.

### 53d.1 RSS per node (VmRSS, mem-samples.txt, 5 s; bench window 602 s)

![RSS per node: mf, cs2 glibc, cs2 jemalloc](ozarchy-p3fsoak-figs/fig-rss.png)

*Figure 53d-1: VmRSS per node. Main + fix (mf) grey dashed, step 2 + fix on glibc (cs2) blue, step 2 + fix on jemalloc
(jcs2) green.*

| cell | node | mean min 2-5 MB | mean min 5-10 MB | change | @10 min MB | peak MB | slope min 2-5 MB/min | slope last 5 min MB/min (95% CI) |
|---|---|---|---|---|---|---|---|---|
| mf (glibc) | val0 | 3,900 | 3,984 | +84 | 3,958 | 4,072 | +43 | +2.5 [-6.1, +11.0] |
| mf (glibc) | val1 | 3,935 | 3,974 | +38 | 4,038 | 4,063 | +42 | +4.7 [-2.0, +11.4] |
| mf (glibc) | val2 | 4,958 | 5,041 | +82 | 5,133 | 5,133 | +70 | **+26.2 [+17.7, +34.7]** |
| cs2 (glibc) | val0 | 5,816 | 6,428 | **+612** | 6,735 | 6,934 | +193 | **+142.4 [+128.9, +155.9]** |
| cs2 (glibc) | val1 | 6,016 | 6,698 | **+682** | 6,985 | 7,140 | +271 | **+124.4 [+112.3, +136.5]** |
| cs2 (glibc) | val2 | 6,856 | 7,936 | **+1,080** | 8,374 | 8,483 | +457 | **+181.3 [+161.9, +200.8]** |
| jcs2 (je) | val0 | 5,312 | 5,173 | -139 | 5,334 | 5,645 | -8 | +10.0 [-35.7, +55.7] |
| jcs2 (je) | val1 | 5,117 | 5,184 | +67 | 5,305 | 5,679 | +40 | -9.6 [-71.3, +52.2] |
| jcs2 (je) | val2 | 5,880 | 6,014 | +135 | 6,065 | 6,506 | +71 | +11.2 [-45.3, +67.8] |

Phase 3 delta per node vs mf (GB: mean min 5-10 / @10 min / peak) and the OLS slope of the RSS difference series over the
last 5 min (5 s grid, MB/min, 95% CI with the AR(1) effective n of `ols()`):

| delta | val0 | val1 | val2 | mean of 3 |
|---|---|---|---|---|
| **cs2 - mf** (Phase 3, glibc) | +2.39 / +2.71 / +2.80; **+141 [+128, +154]** | +2.66 / +2.88 / +3.00; **+120 [+110, +130]** | +2.83 / +3.16 / +3.27; **+158 [+130, +185]** | **+2.62 / +2.92 / +3.02** |
| **jcs2 - mf** (Phase 3 + jemalloc) | +1.16 / +1.34 / +1.54; +10 [-38, +59] | +1.18 / +1.24 / +1.58; -14 [-77, +48] | +0.95 / +0.91 / +1.34; -18 [-74, +37] | **+1.10 / +1.16 / +1.49** |
| jcs2 - cs2 (allocator, same node) | -1.23 / -1.37 / -1.26; **-131 [-180, -81]** | -1.48 / -1.64 / -1.43; **-135 [-200, -70]** | -1.88 / -2.25 / -1.93; **-176 [-242, -110]** | -1.53 / -1.75 / -1.54 |

* **Glibc:** cs2 reproduces section 53c's cs2 (last-5-min slopes +142 / +124 / +181 vs +129 / +178 / +138 MB/min; delta
  +2.62 GB vs mf here, +2.76 GB vs section 53's main there). mf itself levels off: +3 / +5 MB/min on val0 / val1 (CIs span
  0) and +26 on val2 (CI excludes 0), with the same full book as cs2. So the like-for-like control confirms 53c: under
  glibc step 2 + fix does not level off in 10 min, and the gap to the control keeps widening.
* **jemalloc:** jcs2's RSS level is flat after ~minute 3. It swings +-300-400 MB around it (jemalloc decay, as in 53b). The
  minutes 5-10 mean is within -139 to +135 MB of minutes 2-5, and all last-5-min slopes, per cell and of the difference to
  mf, have CIs that span 0. The CIs are wide (+-46-62 MB/min), so slow growth of up to ~+50 MB/min is not excluded by
  one cell.
* **What the jcs2 - mf delta contains (owner point d).** (1) The allocator: mf is glibc. In section 53b, main ran +63 to
  +192 MB higher under jemalloc than under glibc (minutes 5-10). Removing that would put Phase 3 alone under jemalloc at
  ~+0.9-1.0 GB, which is an estimate across campaigns, not measured here. (2) The exec backlog: val0 / val1 on jcs2 hold a
  full exec queue (mean depth 65 / 62 blocks over minutes 5-10) vs 1.6 / 1.2 on mf and 1.8 / 1.2 on cs2. On glibc the
  chain is input-bound by then (see 53d.3). val2 sits at the ~65 cap in all three cells. Section 48 puts a commit-mode
  backlog at up to ~+0.8 GB. So val2 is the like-for-like node on queue depth: there jcs2 - mf is **+0.95 / +0.91 / +1.34
  GB**, under ~1 GB on the mean and at 10 min, over it at the peak.

### 53d.2 Per minute per node (val0; val1 / val2 within a few % except as noted, all in `series.txt`)

![per node per minute: expiry, cancel-alls, margin rejections, matched/s, book, pool, select time, layer, flush queue, handoff](ozarchy-p3fsoak-figs/fig-node.png)

*Figure 53d-2: per node (columns), per minute: nonce-expired evictions, cancel-alls executed/s, margin-rejected orders/s,
matched/s, resting orders, mempool size, block-build select ms, layer MB per checkpoint, flush-worker depth and exec
handoff wait. Same colours as figure 53d-1.*

Cells: RSS MB at minute end / matched/s / resting orders M (minute end) / layer MB per checkpoint (`torus_exec_checkpoint_layer_bytes`
sum / count delta in the minute) / checkpoints / drop_inline in the minute / mempool (mean) / select ms per build. Expiries
(nonce-expired evictions) and margin-rejected orders are **0 in every minute on every node in all three cells**, so they are
not repeated per row.

| min | mf, val0 | cs2, val0 | jcs2, val0 |
|---|---|---|---|
| 1 | 3,443 / 194k / 0.13 / - / - / - / 4.3k / 16.2 | 4,628 / 197k / 0.18 / 128 / 6 / 295 / 4.7k / 12.6 | 4,438 / 205k / 0.29 / 143 / 5 / 294 / 3.7k / 16.1 |
| 2 | 3,867 / 209k / 0.66 / - / - / - / 7.3k / 30.2 | 5,447 / 211k / 0.65 / 205 / 4 / 177 / 6.9k / 18.2 | 5,107 / 211k / 0.30 / 176 / 4 / 160 / 6.8k / 22.6 |
| 3 | 3,889 / 199k / 0.73 / - / - / - / 6.2k / 27.3 | 5,754 / 206k / 0.67 / 203 / 5 / 139 / 6.3k / 22.5 | 5,559 / 220k / 0.72 / 191 / 6 / 178 / 5.9k / 21.9 |
| 4 | 3,906 / 186k / 0.70 / - / - / - / 5.8k / 35.0 | 5,966 / 198k / 0.67 / 221 / 4 / 107 / 6.4k / 22.0 | 5,045 / 217k / 0.68 / 212 / 5 / 144 / 6.0k / 20.5 |
| 5 | 3,990 / 179k / 0.65 / - / - / - / 5.8k / 26.8 | 6,032 / 186k / 0.73 / 223 / 4 / 116 / 6.0k / 22.4 | 4,953 / 218k / 0.69 / 225 / 4 / 132 / 5.8k / 22.2 |
| 6 | 3,971 / 179k / 0.73 / - / - / - / 5.9k / 35.0 | 6,187 / 181k / 0.54 / 239 / 4 / 125 / 6.4k / 32.6 | 5,103 / 215k / 0.56 / 216 / 5 / 143 / 6.1k / 21.5 |
| 7 | 3,965 / 177k / 0.74 / - / - / - / 6.0k / 27.6 | 6,336 / 178k / 0.62 / 220 / 4 / 119 / 6.3k / 21.0 | 5,236 / 214k / 0.69 / 231 / 5 / 122 / 6.0k / 23.8 |
| 8 | 4,039 / 174k / 0.67 / - / - / - / 6.1k / 34.0 | 6,506 / 183k / 0.67 / 228 / 4 / 121 / 6.1k / 20.8 | 5,291 / 215k / 0.64 / 207 / 4 / 117 / 6.2k / 22.9 |
| 9 | 3,957 / 174k / 0.73 / - / - / - / 5.9k / 38.4 | 6,607 / 182k / 0.70 / 227 / 4 / 124 / 6.1k / 19.5 | 5,111 / 210k / 0.65 / 217 / 6 / 167 / 6.4k / 25.2 |
| 10 | 3,958 / 179k / 0.75 / - / - / - / 5.9k / 25.5 | 6,733 / 184k / 0.72 / 233 / 4 / 122 / 6.0k / 22.8 | 5,225 / 216k / 0.71 / 221 / 4 / 149 / 6.6k / 24.0 |

* **Expiry and rejections:** cancel-alls sent / executed per node: mf 193,027 / 193,024, cs2 199,214 / 199,214, jcs2
  233,908 / 233,898. The few not executed are RPC-refused at submit (the bench's refused releases: 26 / 0 / 31). 0
  nonce-expired evictions and 0 bench releases by the 70 s timeout in every cell. `torus_orders_rejected_margin_total` 0
  over every cell. The end-of-run digest (50 senders, `a-margin.py`): no sender below 5% free margin, order margin 0.00%
  max, position IM median 36 / 37 / 44% (mf / cs2 / jcs2), flat by address quartile (33-37 / 34-39 / 40-45%). So the
  section 53c fairness result holds on main too.
* **Reject reason split:** none in this campaign. `torus_orders_rejected_margin_total` is still one counter; 18c is
  splitting it by reason at `native_executor.rs:7218` / `:9418` (sha to follow), and these binaries predate that.
  With 0 margin rejections in all three cells there was nothing to split.
* **Book and layer: the same data in both step 2 + fix cells.** Resting orders are 0.65-0.76 M (mf), 0.54-0.73 M (cs2) and
  0.56-0.73 M (jcs2) per node from minute 3 to 10, flat. The layer per checkpoint is 204-209 MB mean on cs2 (early ->
  late 5 min: 182-191 -> 229-230 MB) and 204 MB on jcs2 (190 -> 218-219 MB), with 42-43 vs 48 checkpoints per node.
  `layer_shared` 0. drop_inline 1,759-1,838 (cs2) vs 1,933-1,943 (jcs2) per node per cell, ~90-180 per minute after minute 1.
  Settle wait per native block: cs2 29.5-33.5 ms early, 29-44 ms late; jcs2 26.6-26.9 -> 34.0-35.3 ms.
* **Flush worker and handoff:** flush-worker depth 0.0-0.2 blocks on cs2 / jcs2 every minute (mf 0.2-0.5), exec handoff
  wait <= 0.05 ms per block on cs2 / jcs2 (mf <= 0.3 ms, 0.5 ms in minute 1). No backlog of blocks, and so none of their
  books, sits in the flush queue on any arm.
* **Pool and selection:** mempool 6.0-6.3k on all arms and nodes (minutes 2-10 mean). Block-build select time per build
  (`torus_block_build_select_seconds`, proposer builds only, minutes 2-10): cs2 22.3 / 23.4 / 19.3 ms, jcs2 22.7 / 24.3 /
  24.8 ms, **mf 31.0 / 29.1 / 56.6 ms**. Main + fix selects +39% / +24% / +193% slower than step 2 + fix (cs2) at the same pool size. This
  was not seen before because 53c had no main cell. It is not explained here (the step 2 range adds 34 commits on main).
* **What RSS tracks:** not the book. Resting orders are flat from minute 3 in all arms, and Pearson r of RSS vs resting
  orders (5 s samples, minutes 2-10) is +0.21 to +0.39 (mf), +0.21 to +0.33 (cs2) and +0.06 to +0.09 (jcs2). Against the
  layer size per checkpoint (60 s window) r is +0.69 to +0.74 on cs2 but +0.01 to +0.40 on jcs2. The layer grows +28 to +47 MB per
  checkpoint over the run in both arms (~15-25%), and under glibc RSS rises with it at +124-181 MB/min, while under
  jemalloc the same layer growth leaves RSS flat. So the glibc correlation is a shared upward trend, consistent with
  glibc retention that scales with the per-checkpoint rebuild size (53b.2). It is not live data that accumulates. There
  is still no live-layer-bytes or read-view gauge (53b.2).

### 53d.3 matched/s: minutes 0-2 (the 120 s A/B window) vs minutes 6-10, every soak cell (owner ask)

val0's `torus_orders_matched_total` (sampler, 1 s) over bench seconds 0-120, 120-360 and 360-600. Minutes 0-2 equal the
harness's `matched_s_first120`. The 120 s A/B cells (sections 49-51) measure only this window.

| section | cell | min 0-2 | min 2-6 | min 6-10 | 6-10 vs 0-2 | whole window (harness) |
|---|---|---|---|---|---|---|
| 53d | **mf** main + fix `de97682f`, glibc | 201,476 | 185,861 | 176,031 | **-12.6%** | 185,054 |
| 53d | **cs2** `41a95355`, glibc | 204,027 | 192,917 | 181,995 | **-10.8%** | 190,602 |
| 53d | **jcs2** `41a95355`, jemalloc | 207,969 | 217,500 | 213,669 | **+2.7%** | 214,299 |
| 53 | main `219f0c20`, glibc | 200,881 | 186,653 | 135,177 | -32.7% | 168,384 |
| 53 | step 2 `7b006739`, glibc | 206,925 | 191,340 | 137,008 | -33.8% | 172,910 |
| 53b | main `219f0c20`, jemalloc | 208,224 | 212,613 | 163,200 | -21.6% | 191,756 |
| 53b | step 2 `7b006739`, jemalloc | 223,954 | 221,481 | 165,169 | -26.2% | 199,134 |
| 53c | step 2 `7b006739`, glibc | 215,840 | 191,401 | 142,051 | -34.2% | 176,544 |
| 53c | cs2 `41a95355`, glibc | 201,516 | 193,866 | 182,401 | -9.5% | 190,776 |

* Without the fix, every soak cell loses 22-34% from minutes 0-2 to 6-10 (the margin collapse, 53b.3 / 53c.4).
* With the fix the collapse is gone, but **the glibc cells still lose 9.5-12.6%**. On mf / cs2 the val0 exec queue drains
  from 36-41 blocks (minute 1) to ~2 by minute 5, exec busy falls from 0.94-1.00 to 0.83-0.89, native blocks/s from
  4.4-4.8 (minute 1) and ~3.0 (minute 2) to 2.1-2.3, and bench submits fall from ~1,270/s to ~850-920/s at constant fills
  per action (~205). This is the
  input-bound glibc pattern of 53b's other observation (a). Under jemalloc the queue stays at its ~65 cap, exec busy 1.00,
  and matched/s holds (+2.7%). The mechanism (why the closed-loop bench offers less under glibc) is not established here.
* So the 120 s A/B cells overstate the sustained glibc rate by ~11-13% with the fix (and by ~30% without). The jemalloc
  rate is sustained. Whole-window matched/s: jcs2 +15.8% vs mf and +12.4% vs cs2 (single cells). This is the same size as
  section 51's / 53b's jemalloc gain, with the same offered-load caveat (closed-loop bench, faster commits).

### 53d.4 Verdict

* **Fair control obtained.** All arms run the full workload for 10 min (0 expiries, 0 margin rejections, the same book).
* **Glibc: RAM rule not met.** Phase 3 delta +2.62 GB per node (minutes 5-10 mean; +2.92 at 10 min, +3.02 peak). It
  still grows +120 to +158 MB/min per node (CIs exclude 0) while mf levels off. This replicates section 53c against the
  right control.
* **jemalloc: levels off.** The jcs2 - mf delta is flat (slopes -18 to +10 MB/min, CIs span 0) at +1.10 GB per node mean
  (+1.16 at 10 min, +1.49 peak). That is at about the ~1 GB bound on the mean and over it at the peak. It overstates Phase 3
  alone by the allocator part (~0.1-0.2 GB from 53b) and, on val0 / val1, by jcs2's full exec backlog. On val2, where the
  queue depth matches, it is +0.95 GB. Rule: **met on the level, borderline on the mean (~1.0-1.1 GB), not met at the peak.**
* **Glibc vs jemalloc at full load: retention, not data.** The same node with the same book (0.54-0.73 M) and the same
  layer size (204-209 MB per checkpoint) grows +124-181 MB/min under glibc and is flat under jemalloc. jcs2 sits 1.23-1.88 GB
  below cs2 over minutes 5-10, and the gap widens at -131 to -176 MB/min. This is consistent with glibc retention scaling
  with the layer size, not with growing live data (one cell per arm).
* **RSS tracks neither resting orders nor layer bytes as data.** Both are flat or slowly rising in both step 2 + fix arms.
  RSS follows the layer trend only under glibc.

### 53d.5 Follow-up checks on the same cells (18c, no new runs)

**Check 1: select time, mf 29-57 ms vs cs2 19-23 ms per proposed block.** The selection code is the same in both. The
select timer also covers `flush_da_mirrors` (a database write), and `torus_block_build_mirror_seconds` times the mirror
step separately. Hypothesis: the write contends with main's per-block flush. Minutes 6-10, ms per proposed block:

| node | mf select | cs2 select | mf mirror | cs2 mirror |
|---|---|---|---|---|
| val0 | 31.3 | 21.1 | 20.5 | 18.8 |
| val1 | 26.8 | 23.0 | 22.2 | 19.0 |
| val2 | 60.1 | 19.3 | 28.4 | 21.8 |

The mirror step is only 1.6-6.6 ms slower on mf, against a select gap of up to 40.8 ms (val2). The gap is a tail: val2
select p50 19.8 vs 15.3 ms, p90 160 vs 30 ms. jcs2 select is 19-25 ms. **Contention is not confirmed by the mirror timer.**
What remains is a rare 100-250 ms stall inside select. That fits a stall in the `flush_da_mirrors` write, but the mirror
timer mostly doesn't see that path, so it can't confirm it (confidence medium-low).

**Check 2: the glibc -10..-13% matched/s after minute 2.** From minute 3 every block is full (400 actions), so
matched/s = blocks/s. Minutes 6-10:

* **val2's exec sets the block rate.** Its exec is busy 0.95 (mf), 0.99 (cs2) and 1.00 (jcs2) with its queue at the cap
  (65), and chain blocks/s tracks 1000 / val2 exec ms per block (per-minute correlation -0.90 / -0.92 / -1.00).
* **val2 is slower per action under glibc:** 110.5 (mf) and 111.4 (cs2) exec ms per 100 actions vs 99.3 / 97.7 on val0 /
  val1. Under jemalloc all three nodes are at 95.4.
* **The extra cost is user CPU, not system time** (so not page-fault work). System share 5.6-7.4% under glibc vs
  7.0-7.8% under jemalloc. val2 on-CPU ms per block: 347 (mf), 308 (cs2), 267 (jcs2).
* **Why only after minute 2:** in minutes 0-2 the chain runs ahead of val2 while val2's queue fills (3.65 vs 3.35
  blocks/s). Once the queue is at the cap, val2's slower exec caps the chain.
* Exec cost per action rises 6-9% over the cell in every arm, jemalloc included, so that part is not allocator-specific.

One cell per arm: consistent with, not proof. Tables: `docs/perf/bench-archive/tools/ozarchy-p3fsoak-tools/analyst/checks-18c.txt`.

Analysis (`tools/ozarchy-p3fsoak-tools/analyst/`): `a-series.py` (53c's copy with the cells mf / cs2 / jcs2, deltas vs mf,
the OLS slope of each RSS difference series, RSS vs resting / layer correlations, and the minute 0-2 vs 6-10 matched/s of
all soak cells incl. 53 / 53b / 53c; figures SVG -> PNG via `rsvg-convert`), `a-margin.py` (53c's, unchanged),
`a-archive.py`. Outputs `series.txt`, `margin.txt`. Full outputs (incl. `series.json`, SVGs) in `ozarchy-p3fsoak-run/analyst/`
on ozarchy. Driver and the jcs2 wrapper: `tools/ozarchy-p3fsoak-tools/`.

## 54. Phase 3 merge-wait fix: `3cf8f72d` vs step 2 `7b006739`, both at 15 s, glibc, commit-tail mode (campaign ozarchy-p3mw, 2026-10-13)

**The exec thread no longer waits for the whole previous layer merge. `checkpoint_settle_wait` falls from 23.4 to 3.1 ms
per native block (-20.3 ms [-20.9, -19.6], paired, n = 4; p99 per observation 95 -> 16 ms). No read blocks on the
deferred part (`torus_exec_checkpoint_deferred_read_waits` 0 on every node of every mw cell), and the end-of-block join
of the deferred part costs 0.19 ms per native block, inside upkeep. Exec `block_ms` falls by about the same amount (-18.0 ms,
-6.9% [-11.3, -2.5]), and native blocks/s rises +11.3% [+8.4, +14.1].** matched/s is a tie (+1.8% [-1.9, +5.4]): the
extra blocks are smaller (fills per native block -4.4% [-9.4, +0.6]). Block time p99 is a tie (-2.9% [-10.2, +4.3]),
p50 is down 12.7% [-15.5, -10.0]. RSS is neutral (peak -0.12 GB [-0.35, +0.12]). Node CPU-s/1M is +1.35% [+0.09, +2.61]:
small, just resolved in the paired test and not at equal fills (+1.1% [-3.0, +5.3]). It is consistent with the per-block
cost of 11% more, smaller blocks, but that is not established here. All 8 measured cells pass every hard check. 3 of
them are REJECT on agreement, all the known mark-only digest artifact (54.3).

Owner-approved queue item (merge-wait): A/B of `perf/p3-merge-wait` `3cf8f72d` (key mw; step 2 `7b006739` + 4 commits:
`b32ca313` the merge thread hands `L` back without positions and balances and merges those two CFs into a deferred part
while the next block runs; E joins it at the end of the block, `torus_exec_checkpoint_deferred_join_seconds`, inside upkeep;
a read that reaches `L`'s deferred CFs waits, `torus_exec_checkpoint_deferred_read_waits`; `1e7854db` / `3cf8f72d`
parent hits skip the deferred part, every blocked read counts) against step 2 `7b006739` (g2), both with
`TORUS_CHECKPOINT_INTERVAL_MS=15000`, glibc, commit tail, n = 4 ABBA. Expected: settle wait a few ms per block (step 2:
23.9 ms in section 49), 0 deferred read waits, the join ~0.

Setup: the section 47-51 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s cells
+ 60 s warm-up, commit tail, shared NVMe). Nodes: g2 `7b006739` (`ca388315`, the section 51 / 53 build, staged
`ozarchy-p3q-stage/g2-run`), mw `3cf8f72d` (`8bb0dc8a`, build id `8f16c0f2`, built 2026-10-12 23:29 in
`wt/p3q-mw-3cf8f72d`, staged `ozarchy-p3q-stage/mw-run`, unused until now). Bench `05d44313`, harness `218ad251`. Driver
`ozarchy-p3q-campaign.sh` CAMP=M (preflight chain: mw = step 2 + 4 commits). A 60 s mw smoke cell first
(`ozarchy-p3mw-smoke-300m-s0-mw`, 03:32-03:35: rc 0, ACCEPT, AGREE, all checks OK, the new metrics in the scrape). Cells
03:37-04:30: w0 (g2, 60 s, excluded), r1 g2 mw, r2 mw g2, r3 g2 mw, r4 mw g2. 126 CHECK OK (knob + enable line +
13-17 checkpoints per node in every measured cell, glibc with no preload, commit tail), no CHECK FAIL, no deaths, no memguard stop,
the host quiet before every cell. Cells are 120 s long, so **matched/s is minutes 0-2 only** (section 53d.3: the glibc rate falls
~11-13% by minutes 6-10; this A/B says nothing about the sustained rate).

### 54.1 Per arm (n = 4, mean; paired mw - g2 over the 4 rounds, 95% CI t(3))

| | g2 step 2 `7b006739` | mw `3cf8f72d` | mw - g2 | % [95% CI] |
|---|---|---|---|---|
| matched/s (120 s = minutes 0-2) | 213,819 | 217,658 | +3,838 | +1.8% [-1.9, +5.4] |
| native blocks/s | 3.51 | 3.90 | +0.40 | **+11.3% [+8.4, +14.1]** |
| block time (commit interval, logs) p50 / p99 ms | 164 / 1,321 | 143 / 1,282 | -20.9 / -38.7 | p50 **-12.7% [-15.5, -10.0]**, p99 -2.9% [-10.2, +4.3] |
| exec `block_ms` (summary) | 261.8 | 243.8 | -18.0 [-29.5, -6.5] | **-6.9% [-11.3, -2.5]** |
| exec per native block (logs) mean / p50 / p99 ms | 345 / 355 / 669 | 321 / 332 / 659 | -23.8 / -22.8 / -9.5 | mean -6.9% [-13.3, -0.5], p50 -6.4% [-11.2, -1.6], p99 -1.4% [-9.5, +6.7] |
| exec busy | 0.965 | 0.960 | -0.005 | -0.5% [-1.8, +0.7] |
| exec backlog (blocks) / lag s | 43.2 / 11.6 | 40.2 / 10.0 | -3.0 / -1.6 | -7% [-25, +11] / -14% [-30, +2] |
| fills / actions per native block | 55,411 / 288 | 52,974 / 275 | | -4.4% [-9.4, +0.6] / -4.4% [-9.2, +0.4] |
| engine ms per 1k fills | 3.39 | 3.51 | +0.11 | +3.4% [+0.1, +6.7] |
| node CPU-s/1M (whole run) | 23.29 | 23.61 | +0.31 | +1.35% [+0.09, +2.61] |
| node CPU at equal fills (ANCOVA, Welch) | | | | +1.1% [-3.0, +5.3] |
| RSS peak / mean per node GB (cpu.csv, mean of 3) | 5.67 / 4.64 | 5.56 / 4.53 | -0.12 [-0.35, +0.12] / -0.11 [-0.34, +0.12] | -2.1% / -2.3% |
| RSS growth over the bench window GB/min | 1.66 | 1.47 | -0.19 [-0.50, +0.12] | -11% [-30, +7] |
| layer MB per checkpoint | 153 | 154 | +1.2 | +0.8% [-7.3, +8.8] |
| device writes MB/s per validator | 65.1 | 66.8 | +1.7 | +2.6% [-0.3, +5.5] |
| bench submits/s | 1,194 | 1,241 | +46.5 | +3.9% [+1.9, +5.9] |

The bench is closed-loop: faster commits let it submit +3.9% more (the section 51 offered-load caveat, smaller here).
Welch intervals (`arms.txt`) agree in sign and resolution with the paired ones for every bolded row (blocks/s +11.3%
[+4.1, +18.4], `block_ms` -6.9% [-13.1, -0.6]); CPU-s/1M is +1.35% [+0.07, +2.63] there too.

### 54.2 Exec-thread checkpoint time and the new counters

| ms per native block (mean of 3 nodes) | g2 | mw | mw - g2 [95% CI] |
|---|---|---|---|
| `checkpoint_settle_wait` (summary phase; section 49: 23.9) | 23.36 | **3.09** | **-20.27 [-20.92, -19.62]** |
| `checkpoint_settle_wait`, from the metrics delta (`_sum` / native blocks) | 20.12 | 2.72 | |
| per observation p50 / p99 (summary quantiles) | 0.46 / 95.5 | 0.29 / 15.8 | |
| `checkpoint_upkeep` (incl. the deferred join) | 1.16 | 1.23 | +0.07 [-0.34, +0.48] |
| upkeep per observation p50 / p99 | 0.09 / 10.0 | 0.10 / 23.9 | |
| `torus_exec_checkpoint_deferred_join_seconds` | absent (not in `7b006739`) | **0.19** (0.13 per observation, ~923 per node per cell) | |
| `torus_exec_checkpoint_deferred_read_waits_total` | absent (not in `7b006739`) | **0 on all 12 node-cells** | |
| `residual_untimed` | 17.2 | 17.6 | +0.4 [-0.8, +1.6] |
| checkpoints per node per cell / `drop_inline` | 14.9 / 839 | 15.6 / 885 | |

Both new metrics are in every mw scrape (`metrics-after-val*.txt`, `ckpt-samples.txt`). The summary phase and the
metrics delta differ by ~3 ms in g2 (different windows: summary phases vs bench start -> end); the drop is ~87% either
way. **The deferred read counter stays 0: no native block read reached `L`'s deferred positions / balances, as
designed. The join at the block end is ~0.2 ms, so the deferred merge is done by then almost every time.** The upkeep
p99 per observation rises 10 -> 24 ms while its mean per block is flat (+0.07 ms). That is consistent with the deferred
join landing in a few upkeeps (checkpoint triggers take `L` whole), but the harness has no per-observation join
quantiles to confirm it.

### 54.3 Agreement and other checks

r3-mw, r4-mw and r4-g2 are rc 2, REJECT on agreement. In each, one node differs from the other two in the 5 mark fields
(`accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable`) of all 50 accounts only
(`accountValue == cash` on 39 / 49 / 50), and that node's digest took longest: val2 42.8 s and 50.8 s vs 18-26 s, val0
88.5 s vs 22-24 s. Block hashes and counters are equal on all 3 nodes, and no cash, position, book or OI field differs.
This is the slow-digest mark artifact of sections 46-51, in both arms: **no hard finding.** (r3-g2 took 92-95 s on all
three nodes and agreed.) Nonce-window evictions 0 in every cell; no tail warnings, no tail errors, no in-flight
timeouts; in-flight RPC refusals 0 except r1-g2 (18) and r3-g2 (7); executed cancel-alls equal those sent (within 0-3
per cell, the same on all 3 nodes).

**Verdict:** the merge-wait change does what it claims. Exec waits ~3 ms per block for its share of the merge instead of
~23 ms. No read ever waits for the deferred part, and the join costs ~0.2 ms. RSS and writes are unchanged. The freed exec
time shows up as +11% native blocks/s at a ~7% lower `block_ms`. Exec is still the ceiling (busy 0.96), the blocks get
smaller, and matched/s over minutes 0-2 is a tie. Block time p99 is a tie, so section 49's +12.7% p99 vs main is not
shown to be recovered. CPU-s/1M +1.35% is the only possible cost, and it does not resolve at equal fills. n = 4 x 120 s:
these are "consistent with", except settle wait and blocks/s, which are decisive.

Analysis (`tools/ozarchy-p3mw-tools/analyst/`): `a-cells.py`, `a-arms.py` (section 51's, unchanged), `a-paired.py`
(new: paired mw - g2 per round over `a-arms.py`'s rows, t(3) CI, + device writes), `a-mw.py` (settle / upkeep / deferred
join per native block and the deferred read waits from `metrics-before/after`), `a-archive.py`. Outputs `arms.txt`,
`paired.txt`, `mw.txt`, `digest.txt`, `cells.txt`. Full outputs (incl. `cells.json`) in `ozarchy-p3mw-run/analyst/` on
ozarchy. Driver: `tools/ozarchy-p3mw-tools/ozarchy-p3q-campaign.sh` (CAMP=M).

## 55. Phase 3 checkpoint interval 5 s vs 15 s on step 2 `7b006739`, glibc, commit-tail mode (campaign ozarchy-p3iv, 2026-10-13)

**At 5 s the step 2 node holds about 0.3 GB less per node over minutes 0-2: RSS peak -0.33 GB [-0.65, -0.01] (-5.8%),
mean -0.28 GB [-0.50, -0.06] (-6.0%), in-window anon -0.36 GB [-0.53, -0.19] (-7.9%), high-water mark -0.47 GB [-0.72,
-0.21] (paired g5 - g2, n = 4, 95% CI t(3)). The growth rate over the bench window is unchanged (1.55 vs 1.54 GB/min).
It costs node CPU-s/1M +1.10% [+0.44, +1.75] (+1.03% [+0.36, +1.70] at equal fills) and device writes +10.8% [+7.0,
+14.6] per validator (64.7 -> 71.7 MB/s), almost all on the WAL / foreground path (+13.6% [+10.5, +16.6]).** Native
blocks/s (+0.2% [-2.6, +2.9]), matched/s (-0.9% [-2.5, +0.6]), `block_ms` and block time p99 (-4.7% [-20.6, +11.3]) are
ties. The checkpoint rate in the bench window goes from 4.4 to 11.6 per node per minute (2.6x), the layer per checkpoint
shrinks only 35% (114 -> 74 MB), so layer bytes per minute rise 58% (660 -> 1,044 MB per node). All 8 measured cells pass every hard check, and the
interval took effect in every cell. 3 cells (all g2) are REJECT on agreement, all the known mark-only digest artifact (55.3).

Owner-approved queue item (interval): the same step 2 binary `7b006739` (`ca388315`, the section 51 / 53 / 54 build,
staged `ozarchy-p3q-stage/g2-run`) in both arms, only `TORUS_CHECKPOINT_INTERVAL_MS` differs: g2 = 15000 (the setting of
sections 47-54), g5 = 5000. Question: does a shorter interval (smaller layer held between checkpoints) cut the Phase 3
RSS, and what does it cost in CPU, writes and blocks/s?

Setup: the section 54 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s cells
+ 60 s warm-up, commit tail, glibc, shared NVMe). Bench `05d44313`, harness `218ad251`. Driver `ozarchy-p3iv-campaign.sh`
CAMP=I, a copy of `ozarchy-p3q-campaign.sh` with a per-arm knob value (`AKV[g5]`; the knob check now checks each arm's
own value) and a new hard check: every validator's Phase 3 enable line must show the arm's interval
(`CheckpointPolicy { interval: Some(5s) ... }` / `Some(15s)`). A 60 s g5 smoke cell first (`ozarchy-p3iv-smoke-300m-s0-g5`,
04:38-04:42: rc 0, ACCEPT, AGREE, all checks OK, 22-23 checkpoints per node). Cells 04:44-05:34: w0 (g2, 60 s,
excluded), r1 g2 g5, r2 g5 g2, r3 g2 g5, r4 g5 g2. 135 CHECK OK, no CHECK FAIL, no deaths, no memguard stop, the host
quiet before every cell. Cells are 120 s long, so **RSS and matched/s here are minutes 0-2 only, not the soak plateau**
(section 53d: on glibc the step 2 RSS was still growing at minutes 5-10).

### 55.1 Interval check per cell

| cell | enable line (val0-2) | in-bench checkpoints per node per min | median in-bench gap s (2 s scrape) | checkpoints per node, metrics window |
|---|---|---|---|---|
| r1-g2 / r2-g2 / r3-g2 / r4-g2 | `Some(15s)` on all 3 | 4.01 / 4.85 / 4.59 / 4.18 | 14.5 / 14.4 / 14.5 / 14.5 | 13-14 / 15 / 14-16 / 13-15 |
| r1-g5 / r2-g5 / r3-g5 / r4-g5 | `Some(5s)` on all 3 | 11.70 / 11.51 / 11.66 / 11.53 | 6.2 / 6.2 / 6.2 / 6.2 | 32-33 / 33 / 32-33 / 33-35 |

The env knob was on all 3 node environs in every cell (`node-env-all.txt`). Mean in-bench spacing is 13.6 s (g2) and
5.2 s (g5) (rate = counter change between the first and last in-bench scrape / the time between them); the median gaps sit on the 2 s scrape grid. Over the whole metrics window (bench start - 4 s to the end of
the drain) the ratio is 2.3x (14.3 -> 32.9 per node, +18.7 [+17.1, +20.2]), not 3x: outside the bench window (setup
~20 s, drain and digest 60-120 s) both arms checkpoint every ~4-6 s, so something other than the interval triggers
checkpoints there. That is not investigated here; the knob governs the loaded window, which is what the A/B measures.

### 55.2 Per arm (n = 4, mean; paired g5 - g2 over the 4 rounds, 95% CI t(3))

| | g2 (15 s) | g5 (5 s) | g5 - g2 | % [95% CI] |
|---|---|---|---|---|
| RSS peak / mean per node GB (cpu.csv, mean of 3) | 5.68 / 4.61 | 5.35 / 4.34 | **-0.33 [-0.65, -0.01] / -0.28 [-0.50, -0.06]** | -5.8% [-11.5, -0.2] / -6.0% [-10.7, -1.2] |
| RSS peak / mean in the bench window GB (mem-samples) | 5.57 / 4.58 | 5.22 / 4.22 | **-0.35 [-0.59, -0.12] / -0.36 [-0.53, -0.19]** | -6.4% / -7.9% [-11.5, -4.2] |
| VmHWM / anon (window mean) GB | 5.96 / 4.54 | 5.50 / 4.18 | **-0.47 [-0.72, -0.21] / -0.36 [-0.53, -0.19]** | -7.8% / -7.9% |
| RSS first / last 15 s of the bench window GB | 2.09 / 5.50 | 1.80 / 5.18 | -0.29 [-0.71, +0.13] / -0.33 [-0.61, -0.04] | |
| RSS growth over the bench window GB/min | 1.55 | 1.54 | -0.01 [-0.38, +0.36] | -0.6% [-24.6, +23.5] |
| memtables MB | 229 | 263 | +34 [+15, +52] | +14.7% [+6.7, +22.8] |
| layer MB per checkpoint (metrics `layer_bytes` sum / count) | 114.0 | 73.8 | -40.2 [-46.2, -34.2] | **-35.3% [-40.5, -30.0]** |
| layer MB per checkpoint (gauge, bench window, a-arms.py) | 155 | 90 | -65 [-86, -44] | -41.9% [-55.4, -28.4] |
| layer MB per node per minute (bench window, 2 s scrapes, a-ckgap.py) | 660 | 1,044 | +384 [+357, +410] | **+58.1% [+54.1, +62.1]** |
| layer MB per node per minute (metrics window, scrape-time denominator, a-iv.py) | 616 | 923 | +306 [+278, +335] | +49.8% [+45.1, +54.4] |
| node CPU-s/1M (whole run) | 23.36 | 23.61 | +0.26 [+0.10, +0.41] | **+1.10% [+0.44, +1.75]** |
| node CPU at equal fills (ANCOVA, Welch) | | | | +1.03% [+0.36, +1.70] |
| flush-worker checkpoint s per node per cell | 6.23 | 9.49 | +3.26 [+3.03, +3.48] | +52.2% [+48.6, +55.8] |
| device writes MB/s per validator (io-ticks, bench window) | 64.7 | 71.7 | +6.97 [+4.50, +9.44] | **+10.8% [+7.0, +14.6]** |
| of which WAL + foreground (all but RocksDB background threads) | 50.8 | 57.7 | +6.89 [+5.35, +8.44] | +13.6% [+10.5, +16.6] |
| of which RocksDB flush + compaction threads | 14.8 | 15.7 | +0.89 [-0.36, +2.15] | +6.0% [-2.5, +14.6] |
| native blocks/s | 3.48 | 3.49 | +0.01 | +0.2% [-2.6, +2.9] |
| matched/s (120 s = minutes 0-2) | 211,652 | 209,656 | -1,996 | -0.9% [-2.5, +0.6] |
| block time (commit interval, logs) p50 / p99 ms | 167 / 1,293 | 164 / 1,233 | -3.0 / -60 | -1.8% [-7.9, +4.3] / -4.7% [-20.6, +11.3] |
| exec `block_ms` (summary) | 260.5 | 258.9 | -1.6 [-8.3, +5.0] | -0.6% [-3.2, +1.9] |
| exec per native block (logs) mean / p99 ms | 347 / 676 | 350 / 682 | +3.9 / +6.2 | +1.1% [-2.9, +5.1] / +0.9% [-4.4, +6.2] |
| exec busy / backlog (blocks) | 0.961 / 42.0 | 0.956 / 42.8 | | -0.4% [-2.1, +1.2] / +1.8% [-14.7, +18.2] |
| fills per native block | 55,042 | 54,779 | | -0.5% [-4.0, +3.1] |
| bench submits/s | 1,184 | 1,175 | -8.8 | -0.7% [-5.6, +4.1] |

Welch intervals (`arms.txt`) agree in sign and resolution with the paired ones for every bolded row (RSS peak -5.8%
[-9.9, -1.8], mean -6.0% [-9.7, -2.2], CPU-s/1M +1.10% [+0.42, +1.77], writes +10.8% [+7.4, +14.2]).

### 55.3 Exec-thread checkpoint time, agreement and other checks

| ms per native block (mean of 3 nodes) | g2 | g5 | g5 - g2 [95% CI] |
|---|---|---|---|
| `checkpoint_settle_wait` (summary phase) | 23.16 | 21.76 | -1.40 [-2.48, -0.32] |
| `checkpoint_settle_wait`, from the metrics delta (`_sum` / native blocks) | 19.73 | 19.02 | -0.71 [-1.42, +0.00] |
| settle wait per observation p50 / p99 (summary quantiles) | 2.46 / 95.6 | 0.02 / 95.8 | |
| `checkpoint_upkeep` (summary phase) | 1.14 | 3.70 | **+2.57 [+2.21, +2.93]** |
| `checkpoint_upkeep`, from the metrics delta | 0.97 | 3.24 | +2.27 [+2.06, +2.48] |
| upkeep per observation p50 / p99 | 0.10 / 13.1 | 0.08 / 99.6 | |
| `drop_inline` per node per cell | 774 | 709 | -8.5% [-16.2, -0.8] |

Exec's total settle wait per block hardly moves with 2.6x more checkpoints (-3.6% to -6%). Each wait is shorter but there
are more of them, so the wait is consistent with scaling with the merged data, not with the checkpoint count. Upkeep
per block rises +2.3-2.6 ms and its p99 per observation 13 -> 100 ms. Exec time per block is still a tie, so the extra
upkeep does not show in `block_ms` here.

r1-g2, r2-g2 and r4-g2 are rc 2, REJECT on agreement (the warm-up w0 too). In each, val2 differs from val0 / val1 in the
5 mark fields (`accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable`) of all 50
accounts only. In r1-g2 and r4-g2, val2's digest took longest (43.1 s and 46.7 s vs 17-22 s). In r2-g2, val0 and val1
took 88 s and val2 21 s, so there the odd node is the fast one. Block hashes, compared heights and matched / placed / resting
/ action counters are equal on all 3 nodes, with no panic or fail-stop lines. This is the slow-digest mark artifact of sections
46-54: **no hard finding.** All three fell in g2. With 3 REJECTs in 8 cells, 3-of-3 in one arm happens by chance with
probability ~0.14, and section 54 had them in both arms on this binary. Nonce-window evictions 0, no tail warnings or
errors, in-flight timeouts 0, in-flight RPC refusals ("server overloaded") 6 / 1 / 2 / 1 in r1-g2 / r1-g5 / r2-g5 /
r4-g2 and 0 elsewhere, executed cancel-alls equal on all 3 nodes, action failures 0.

**Verdict:** 5 s cuts the Phase 3 RSS level by ~0.3 GB per node (~6%) over minutes 0-2. The paired CIs resolve this on
peak, mean, window mean and HWM, though the cpu.csv peak only just clears 0. Next to the step 2 - main delta at 15 s in
section 51 (+1.55 GB peak / +1.08 GB mean, glibc, 120 s, other campaign), that is about a fifth to a quarter of it. The
growth rate over the window is unchanged, so this A/B gives no sign that 5 s changes the glibc growth that kept the soak
over the RAM rule (section 53d: +2.62 GB, still growing). That needs a soak to say. The cost is +1.1% CPU-s/1M (resolved,
also at equal fills), +11% device writes per validator (the WAL path; layer bytes per minute +58%: the layer shrinks less
than the interval, consistent with hot rows being rewritten in every layer) and +52% flush-worker checkpoint time.
Throughput, exec time and block time are ties. n = 4 x 120 s: "consistent with" throughout, except the checkpoint rate,
layer size, writes and upkeep, which are decisive.

Analysis (`tools/ozarchy-p3iv-tools/analyst/`): `a-cells.py`, `a-arms.py`, `a-paired.py` (section 54's, unchanged; `--io`
for device writes), `a-mw.py` (settle / upkeep per native block; the deferred metrics are absent on `7b006739`), new
`a-iv.py` (checkpoint count, layer bytes per checkpoint and per minute, settle / upkeep per native block, flush-worker
checkpoint time, and the WAL vs RocksDB background write split of section 42, paired) and `a-ckgap.py` (in-bench
checkpoint rate, layer MB per minute and gaps from `ckpt-samples.txt`), `a-archive.py`. Outputs `arms.txt`, `paired.txt`, `iv.txt`,
`ckgap.txt`, `mw.txt`, `digest.txt`, `cells.txt`. Full outputs (incl. `cells.json`) in `ozarchy-p3iv-run/analyst/` on
ozarchy. Driver: `tools/ozarchy-p3iv-tools/ozarchy-p3iv-campaign.sh` (CAMP=I) + `env.sh`.

## 56. jemalloc compiled in: feat/jemalloc-allocator `88ddc4c2` vs main `deeacdb3` (glibc), saturated and fixed offered load, commit-tail mode (campaign ozarchy-p3je, 2026-10-13)

**Compiled-in jemalloc does not raise the matched/s ceiling: saturated matched/s -0.3% [-1.5, +0.9] (paired ja - jb, n = 4,
95% CI t(3)). It does make consensus faster: native blocks/s +14.9% [+3.2, +26.6], smaller blocks, block time p99 -7.2%
[-12.6, -1.9], exec `block_ms` -14.3% [-25.2, -3.4]. The closed-loop bench then submits more (+6.3% [+1.8, +10.9] over
the matching window). At fixed offered load (RATE 907 actions/s) it costs node CPU: CPU-s/1M +4.26% [+3.26, +5.25]
(+4.62% [+2.41, +6.82] at equal fills). It lowers the whole-cell RSS: mean -0.22 GB [-0.30, -0.14] (-6.6%), peak -0.14 GB
[-0.18, -0.10] (-3.6%), last 15 s of the bench window -0.36 GB [-0.43, -0.29]. It halves the growth over the bench window
(1.05 -> 0.49 GB/min).** Saturated, CPU-s/1M is +2.66% [+1.83, +3.49], the RSS peak is a tie, the mean is -3.8%
[-4.6, -2.9] and the growth is -44%. The same direction as section 51's LD_PRELOAD cells on main. There, the "+13-15%"
was the submit rate and blocks/s, not matched/s: main + jemalloc gave +0.6% matched/s and +1.5% CPU-s/1M. **Caveat on the
fixed-load mode:** at 907 actions/s the executor is still the bottleneck in both arms (busy 0.95 / 0.97, backlog 44 / 55
blocks, 780-798 actions/s executed vs 895-907 submitted), so FIX equalises offered load and total work, not an idle
executor (56.3). 16 measured cells (+ 2 warm-ups + the smoke cell), all rc 0 or rc 2 from the known mark-only digest artifact. Every hard check passed,
and the allocator was verified in every cell.

Owner-approved plan (18c, part A of the jemalloc batch): `feat/jemalloc-allocator` `88ddc4c2` (tikv-jemallocator 0.7 /
jemalloc 5.3.1 as the process allocator, default feature `jemalloc`, unprefixed so RocksDB's C++ `malloc` goes through it
too, plus `torus_jemalloc_{allocated,active,resident,retained,mapped}_bytes` gauges) vs its parent main `deeacdb3`
(`deeacdb3..88ddc4c2` = that one commit). Arms: ja = `88ddc4c2` (node `f2fbe17b`), jb = `deeacdb3` (node `e901fd1d`), no
LD_PRELOAD in either arm, no checkpoint knob (both main, no Phase 3), commit tail. Built for this run with frame pointers,
line tables and mold, each in a fresh target dir (`~/.cargo-target-p3je-<key>-<sha8>`) from a clean detached worktree
(`/home/oz/projects/wt/p3je-<key>-<sha8>`). Script `ozarchy-p3je-build.sh` (copy of `ozarchy-p3cssoak-build.sh`), md5s in
`ozarchy-p3je-build/md5s-all.txt`, staged in `ozarchy-p3je-stage/{ja,jb}-run`. Bench `05d44313` (`8efcb07a`), harness
`218ad251`. Part B's two je-vs-je binaries were built in the same run but not used here: jcs = `e7be8460`
(`test/p3-cs-je`, `4521dd5f`) and jmf = `0360c840` (`test/main-fix-je`, `9d6e57c1`).

**Allocator check.** Preflight: the ja, jcs and jmf binaries define `malloc` in `.dynsym` (section 30, jemalloc linked in).
In jb, `malloc` is `UND` (`malloc@GLIBC_2.2.5`). Per cell (new `jechk` hard check): on ja, `torus_jemalloc_allocated /
resident / retained_bytes` > 0 on all 3 nodes in every cell (smoke included). On jb, no `torus_jemalloc_` series on any
node. No node had LD_PRELOAD or a mapped `libjemalloc.so` (the old `heapchk`).

Setup: the section 51-55 shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, 120 s cells
+ 60 s jb warm-up, shared NVMe). Driver `ozarchy-p3je-campaign.sh` (copy of `ozarchy-p3iv-campaign.sh`) has two modes.
CAMP=SAT uses the usual harness RATE 76000 actions/s. That is only an offered ceiling: the CONC semaphore and the
in-flight cap bound the bench. CAMP=FIX uses RATE 907 (run-cell.sh arg 5 -> bench `--rate-total`, paced, no catch-up).
Both modes run cells in this order: w0 (jb), r1 jb ja, r2 ja jb, r3 jb ja, r4 ja jb. A 60 s ja smoke cell ran first
(`ozarchy-p3je-sat-smoke-300m-s0-ja`, 06:00-06:04, rc 0, ACCEPT AGREE). SAT cells ran 06:06-06:59 and FIX cells
07:01-07:53. Each campaign logged 144 CHECK OK, with no CHECK FAIL, no deaths and no memguard stop. The host was quiet
before every cell (no cargo / rustc / rust-analyzer, load < 2, no analysis python). 120 s cells mean **all RSS figures are
minutes 0-2, not a soak plateau** (part B has the soaks).

**How RATE was chosen.** The jb submit rate in SAT was measured over the matching window [10 s, 110 s] of each cell. Count
and elapsed time come from the same `bench.log` cumulative line: elapsed = submitted / cumulative rate. The four values
were 1,200.1 / 1,215.6 / 1,200.1 / 1,221.6 actions/s, mean 1,209.35. 75% of that is 907.0, so RATE = 907
(`ozarchy-p3je-fix-run/RATE.txt`).

### 56.1 Submit-rate check per FIX cell (window [10 s, 110 s]; gate: within 2% of RATE 907)

| cell | submit actions/s | vs RATE | | cell | submit actions/s | vs RATE |
|---|---|---|---|---|---|---|
| w0 (jb, 60 s, [10, 50] s) | 907.0 | +0.00% | | | | |
| r1-jb | 897.4 | -1.06% | | r1-ja | 907.3 | +0.03% |
| r2-jb | 891.9 | -1.67% | | r2-ja | 906.2 | -0.09% |
| r3-jb | 897.5 | -1.05% | | r3-ja | 907.3 | +0.03% |
| r4-jb | 893.1 | -1.53% | | r4-ja | 906.2 | -0.09% |

All 9 cells pass the 2% gate, so none is LOADGEN-SUSPECT. ja holds RATE to 0.1% in every cell. jb runs 1.0-1.7% under it
in every cell (paired +1.32% [+0.91, +1.72]), so the glibc arm lets slightly fewer paced fires through the in-flight cap.
The fixed-load comparison is therefore at +1.3% more offered work on ja, and per-fill CPU is the fair metric.

### 56.2 Per arm (n = 4, mean; paired ja - jb over the 4 rounds, 95% CI t(3))

| | SAT jb | SAT ja | SAT ja - jb % [95% CI] | FIX jb | FIX ja | FIX ja - jb % [95% CI] |
|---|---|---|---|---|---|---|
| matched/s (bench window) | 215,255 | 214,571 | -0.3% [-1.5, +0.9] | 162,468 | 158,902 | **-2.2% [-3.5, -0.9]** |
| bench submit actions/s (window [10, 110] s) | 1,209 | 1,286 | **+6.3% [+1.8, +10.9]** | 895 | 907 | +1.3% [+0.9, +1.7] |
| actions/s executed (val0, bench window) | 1,087 | 1,102 | +1.4% [-1.2, +3.9] | 798 | 780 | -2.2% [-3.6, -0.8] |
| node CPU-s/1M fills (whole run) | 24.74 | 25.40 | **+2.66% [+1.83, +3.49]** | 29.00 | 30.24 | **+4.26% [+3.26, +5.25]** |
| node CPU at equal fills (ANCOVA, Welch) | | | +3.66% [-0.27, +7.58] | | | **+4.62% [+2.41, +6.82]** |
| fills per node, whole run (M) / node CPU-s | 28.30 / 700 | 30.85 / 784 | | 22.84 / 662 | 23.22 / 702 | |
| RSS peak per node GB (cpu.csv, mean of 3) | 4.07 | 4.08 | +0.4% [-3.4, +4.1] | 3.93 | 3.79 | **-3.6% [-4.7, -2.5]** |
| RSS mean per node GB (cpu.csv) | 3.48 | 3.35 | **-3.8% [-4.6, -2.9]** | 3.34 | 3.12 | **-6.6% [-8.9, -4.2]** |
| RSS mean, bench window GB (mem-samples) | 3.51 | 3.68 | +4.8% [+1.0, +8.5] | 3.33 | 3.34 | +0.4% [-3.5, +4.4] |
| RSS first / last 15 s of the window GB | 1.96 / 4.01 | 2.82 / 3.97 | +0.86 [+0.30, +1.42] / -0.03 [-0.21, +0.14] | 1.40 / 3.88 | 2.23 / 3.52 | +0.84 [+0.32, +1.36] / **-0.36 [-0.43, -0.29]** |
| RSS growth over the window GB/min | 0.85 | 0.48 | **-44% [-69, -19]** | 1.05 | 0.49 | **-54% [-70, -37]** |
| VmHWM GB | 4.21 | 4.25 | +0.9% [-2.4, +4.1] | 4.05 | 3.95 | -2.6% [-4.3, -0.8] |
| native blocks/s | 3.61 | 4.15 | **+14.9% [+3.2, +26.6]** | 3.80 | 4.18 | **+10.2% [+6.9, +13.5]** |
| actions / fills per native block | 282 / 54,526 | 251 / 48,450 | -10.9% / -11.1% | 198 / 40,204 | 173 / 35,147 | -12.7% / -12.6% (resolved) |
| block time (commit interval) p50 ms | 163 | 105 | **-35.5% [-41.3, -29.8]** | 132 | 122 | -7.5% [-22.9, +7.8] |
| block time p99 ms | 1,128 | 1,047 | **-7.2% [-12.6, -1.9]** | 1,161 | 1,059 | **-8.8% [-16.3, -1.3]** |
| exec `block_ms` (summary) | 248.8 | 213.2 | **-14.3% [-25.2, -3.4]** | 236.0 | 210.9 | **-10.6% [-14.4, -6.9]** |
| exec per native block (logs) mean / p99 ms | 335 / 644 | 291 / 589 | -13.1% [-17.7, -8.5] / -8.5% [-14.9, -2.1] | 260 / 583 | 236 / 541 | -9.4% [-12.1, -6.8] / -7.3% [-13.8, -0.7] |
| engine ms per 1k fills | 3.67 | 3.56 | -2.9% [-4.0, -1.7] | 4.30 | 4.22 | -1.8% [-4.0, +0.5] |
| residual untimed ms per block | 13.7 | 13.0 | -0.7 [-2.1, +0.8] | 17.7 | 27.0 | **+9.3 [+8.8, +9.9]** |
| exec busy / backlog (blocks) | 0.951 / 41.3 | 0.968 / 53.1 | +1.8% / +11.9 blocks | 0.947 / 44.0 | 0.973 / 55.5 | +2.7% / +11.4 blocks |
| mempool wait s (Little: mempool mean / executed actions/s) | 5.00 | 3.89 | -22% [-39, -5] | 1.89 | 1.20 | **-37% [-49, -24]** |
| exec lag s (commit -> executed, logs) | 11.18 | 11.69 | +4.5% [-1.7, +10.8] | 11.20 | 12.37 | **+10.4% [+3.9, +17.0]** |
| device writes MB/s per validator (io-ticks) | 138.0 | 144.8 | +5.0% [+1.1, +8.9] | 209.0 | 218.2 | +4.4% [+1.8, +7.1] |
| memtables MB | 285 | 286 | +0.5% | 273 | 273 | -0.1% |

Welch intervals (`arms.txt`) agree in sign and resolution with the paired ones on every bolded row (e.g. FIX CPU-s/1M
+4.26% [+3.66, +4.85], SAT blocks/s +14.9% [+1.6, +28.2], FIX RSS mean -6.6% [-8.5, -4.7]). Fill latency: the harness records no per-action submit -> commit time. The two
parts it does record point in opposite directions. ja spends less time in the mempool (-37% in FIX) and more in the
exec queue (+10%). Their sum, mempool wait + exec lag, is 13.1 s (jb) vs 13.6 s (ja) in FIX, consistent with a tie or
slightly slower on ja. Block time p99 is shorter on ja in both modes.

**jemalloc gauges (ja, bench-window mean per node; max of resident in brackets):**

| | allocated GB | resident GB | retained GB | resident - allocated GB |
|---|---|---|---|---|
| SAT | 2.14 | 3.33 [3.95] | 2.47 | 1.19 |
| FIX | 2.03 | 2.99 [3.60] | 2.61 | 0.96 |

The live heap is ~2.0-2.1 GB per node at minutes 0-2. jemalloc's own resident pages add another ~1.0-1.2 GB (dirty and
muzzy pages not yet purged, plus metadata). ~2.5 GB of virtual memory is retained (not RSS). Window-mean process RSS (3.68 / 3.34 GB) is
resident plus ~0.35 GB outside jemalloc (file mappings, thread stacks, other non-heap memory).

### 56.3 What the fixed-load mode did and did not equalise

RATE 907 equalised offered load (56.1) and total work: whole-run fills per node 22.84 vs 23.22 M (+1.7%, following
the +1.3% offered). It did not take the executor out of saturation. At the slower pace the books are deeper (resting
orders 398k vs 241k in SAT), and each fill costs more (29.0 vs 24.7 CPU-s/1M, engine 4.3 vs 3.7 ms per 1k fills). Exec
capacity therefore drops to ~780-800 actions/s, below 907. Both arms build a 44-55 block backlog and an 11-12 s exec lag,
as in SAT. ja's faster consensus (+10% blocks/s, smaller blocks) runs further ahead of its executor (+11 blocks of
backlog), which is why its in-window matched/s is -2.2% while its total work is +1.7%. The per-fill CPU cost is
unaffected by this, since it is measured over the whole run including the drain. An operating point where the executor
keeps up would need RATE below ~750 on this shape. That was not run.

The FIX-only residual untimed +9.3 ms per block on ja (SAT: tie) is not explained here. It is consistent with
per-block allocator work (more, smaller blocks) that falls outside the timed phases.

### 56.4 Agreement and other checks

Three cells are rc 2 / REJECT on agreement: SAT r4-ja, FIX r1-ja and FIX r3-jb. In each, val1 differs from val0 / val2
in the 5 mark fields (`accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable`) of all 50
accounts only. This is the slow-digest mark-only artifact of sections 46-55, in both arms: **no hard finding.** Digest
times vary across cells in both directions: SAT ja 26-42 s vs jb 51-67 s, FIX ja 41-60 s vs jb 21-45 s. That is not
an allocator effect we can claim. Nonce-window evictions 0, action failures 0, in-flight timeouts 0, in-flight RPC
refusals 1 / 1 / 2 (SAT r1-ja / r3-jb / r4-ja), 0 elsewhere, no tail warnings.

**Verdict:** compiled-in jemalloc on main reproduces section 51's LD_PRELOAD picture for main. Consensus is faster (blocks/s
+10-15%, block time p99 -7 to -9%, exec per block -10 to -14% on smaller blocks). Throughput is not higher (matched/s tie
saturated, -2.2% in the window at fixed load). CPU per fill is higher: +2.7% saturated and +4.3% at equal offered load,
+4.6% at equal fills, resolved. The RSS level over minutes 0-2 is lower or equal: mean -4 to -7%, end of window up to
-0.36 GB, growth rate about halved. It starts higher (+0.85 GB in the first 15 s), consistent with jemalloc's arenas
holding pages early and growing less later. For the Phase 3 RAM gate the halved growth is the relevant figure. Whether
that holds as a plateau is part B's question (10 min soaks ja / jb and jcs / jmf). n = 4 x 120 s: "consistent with"
throughout. Decisive: CPU-s/1M at fixed load, blocks/s, exec `block_ms`, RSS growth, and the FIX RSS mean / peak.

Analysis (`tools/ozarchy-p3je-tools/analyst/`): `a-cells.py`, `a-arms.py`, `a-paired.py` (section 55's, plus the windowed
submit rate, the `torus_jemalloc_*` gauges from the 5 s mem samples, executed actions/s and the Little-law mempool wait),
`a-archive.py`. Outputs per mode: `sat-` / `fix-` `cells.txt`, `arms.txt`, `paired.txt`. Full outputs (incl. `cells.json`)
in `ozarchy-p3je-{sat,fix}-run/analyst/` on ozarchy. Driver `tools/ozarchy-p3je-tools/ozarchy-p3je-campaign.sh`
(CAMP=SAT|FIX, `jechk`, `ratechk`, the dynsym allocator preflight) + `env.sh`. Build scripts `ozarchy-p3je-build.sh` and
`ozarchy-p3je-build-all.sh`.

## 57. jemalloc batch part B: 10 min soaks with jemalloc compiled in, Phase 3 je vs je (`e7be8460` vs `0360c840`) and main je vs glibc (`88ddc4c2` vs `deeacdb3`) (campaign ozarchy-p3jesoak, 2026-10-13)

**RAM gate, je vs je (both arms have the cancel fix and jemalloc compiled in; the pair differs only by Phase 3): the
Phase 3 delta is +0.94 GB per node (minutes 5-10 mean; +1.00 GB at 10 min, +1.27 GB peak; per node +0.86 to +0.98 /
+0.78 to +1.32 / +1.05 to +1.55). It levels off: the last-5-min slope of the RSS difference is +43 [-4, +90], -80 [-134,
-27] and +8 [-49, +65] MB/min (val0 / val1 / val2), so no node shows positive growth that excludes 0. Both arms hold the exec queue at its
~66-block cap on every node (minutes 5-10 mean 58-65 in both), so unlike section 53d no exec backlog and no allocator
difference sits in the delta. Of the +0.99 GB jemalloc-resident delta, +0.54 GB is live allocations
(`torus_jemalloc_allocated_bytes`) and ~+0.45 GB is allocator pages (resident - allocated). Non-heap RSS is equal
(-0.01 to -0.07 GB).** Owner RAM rule (levels off, delta <= ~1 GB/node): **met on the level and on the minutes 5-10 mean
(0.86-0.98 GB on every node), at the bound at 10 min (mean 1.00, val1 1.32), not met at the peak (1.05-1.55 GB).** This
matches section 53d's estimate for Phase 3 alone under jemalloc (~0.9-1.0 GB), now measured without its confounds. One cell
per arm: "consistent with", with the CIs above.

**Secondary, main je vs glibc without the cancel fix (ja `88ddc4c2` vs jb `deeacdb3`):** both lose the workload from
minute ~3 (cancel-all expiries from 152-154 s, margin rejections from minute 6, as in sections 53 / 53b), so neither is
the full workload. Under that load ja's RSS is +0.15 GB per node above jb on the minutes 5-10 mean (+0.06 GB at 10 min,
+0.39 GB peak). jb's RSS grows +7 to +39 MB/min over the last 5 min and ja's +38 to +52 MB/min (all CIs exclude 0). ja holds the exec queue at the cap on
all nodes while jb's val0 / val1 drain to ~2 blocks (input-bound). matched/s minutes 6-10 vs 0-2: ja -19.9%, jb -35.1%.

Owner-approved (18c's plan): part B of the jemalloc batch, four 10 min soaks, full workload, commit tail, saturated (the
harness RATE 76000). Arms: **jmf** = `test/main-fix-je` `0360c840` (main + cancel fix `de97682f` + compiled-in jemalloc;
node `9d6e57c1`), no checkpoint knob (RAM-gate control). **jcs** = `test/p3-cs-je` `e7be8460` (step 2 + fix `41a95355` +
compiled-in jemalloc; `4521dd5f`), `TORUS_CHECKPOINT_INTERVAL_MS=15000`. **ja** = `88ddc4c2` (main `deeacdb3` + jemalloc;
`f2fbe17b`, section 56's ja) and **jb** = `deeacdb3` (glibc; `e901fd1d`), neither with the cancel fix, no knob. No
LD_PRELOAD anywhere: the preflight found `malloc` defined in `.dynsym` (index 30) of jmf / jcs / ja and `UND` (glibc) in jb,
and every je cell exported `torus_jemalloc_{allocated,resident,retained}_bytes` > 0 on all 3 nodes (jb none). Chain
(preflight): jmf's parent is `de97682f`, jcs's parent `41a95355`, merge-base(jmf, jcs) = `de97682f`, jmf has no Phase 3 commit
and no commit outside jcs except its own jemalloc commit. Binaries built and staged by part A (`ozarchy-p3je-stage`,
md5s `ozarchy-p3je-build/md5s-all.txt`), worktrees `wt/p3je-*` clean at their shas. Bench `05d44313`, harness `218ad251`,
tw, Classic, 300 markets, MAX_IN_FLIGHT 4, budget 900, commit tail, shared NVMe, memguard (MemAvailable < 6 GB stops the
cell). Driver `ozarchy-p3jesoak-campaign.sh` (copy of part A's with CAMP=SOAK, arms jmf / jcs, `jechk` and the no-preload
check generalised to the three je arms, the allocator check on all four binaries). A 60 s jcs smoke cell (08:04-08:07)
showed the enable line `item 6 Phase 3: coalesced state checkpoints ON` (interval 15 s) once per validator, 11
checkpoints per node, ACCEPT AGREE. Cells 08:09-09:09: w0 (jmf, 60 s, excluded), k1-jmf, k1-jcs, k1-jb, k1-ja (600 s
each, fixed, no extensions; each pair adjacent). **w0, k1-jmf, k1-jcs, k1-jb: rc 0, ACCEPT, AGREE. k1-ja: rc 2, REJECT
DISAGREE = the mark-only digest artifact** (57.4). 80 CHECK OK (+16 in the smoke), no CHECK FAIL, no memguard stop, no deaths, the host quiet
before every cell.

### 57.1 RSS per node and the jemalloc split (VmRSS, mem-samples.txt, 5 s; bench window 602-604 s)

![RSS and jemalloc allocated per node: jcs vs jmf, ja vs jb](ozarchy-p3jesoak-figs/fig-rss.png)

*Figure 57-1: VmRSS (solid) and `torus_jemalloc_allocated_bytes` (thin dashed) per node. Top: jmf grey dashed, jcs green.
Bottom: jb (glibc, no gauge) orange dashed, ja blue.*

| cell | node | mean min 2-5 MB | mean min 5-10 MB | change | @10 min MB | peak MB | slope last 5 min MB/min (95% CI) | je allocated MB | je resident MB | je retained MB | exec queue (min 5-10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| jmf | val0 | 4,165 | 4,157 | -8 | 4,188 | 4,383 | -19.2 [-42.9, +4.5] | 2,752 | 3,794 | 3,923 | 58.5 |
| jmf | val1 | 4,134 | 4,153 | +19 | 4,048 | 4,329 | -2.3 [-19.9, +15.4] | 2,785 | 3,770 | 3,600 | 59.0 |
| jmf | val2 | 4,892 | 4,964 | +72 | 5,241 | 5,299 | -9.5 [-35.1, +16.0] | 2,939 | 4,566 | 4,669 | 65.3 |
| jcs | val0 | 5,027 | 5,159 | +132 | 5,103 | 5,622 | +22.2 [-27.2, +71.6] | 3,313 | 4,805 | 5,241 | 58.1 |
| jcs | val1 | 5,286 | 5,161 | -125 | 5,403 | 5,921 | **-78.3 [-132.1, -24.5]** | 3,374 | 4,853 | 7,817 | 63.5 |
| jcs | val2 | 5,842 | 5,840 | -1 | 6,039 | 6,377 | +3.5 [-53.9, +60.8] | 3,464 | 5,493 | 6,800 | 65.4 |
| jb | val0 | 3,837 | 3,927 | +90 | 4,002 | 4,002 | **+13.8 [+10.9, +16.8]** | - | - | - | 2.0 |
| jb | val1 | 3,796 | 3,865 | +69 | 3,891 | 3,909 | **+7.1 [+3.6, +10.6]** | - | - | - | 1.6 |
| jb | val2 | 4,617 | 4,690 | +73 | 4,769 | 4,862 | **+39.0 [+31.8, +46.2]** | - | - | - | 65.4 |
| ja | val0 | 3,955 | 4,039 | +84 | 4,145 | 4,262 | **+37.7 [+14.6, +60.9]** | 2,610 | 3,616 | 3,498 | 64.9 |
| ja | val1 | 3,987 | 4,057 | +70 | 4,000 | 4,426 | **+50.3 [+22.4, +78.2]** | 2,572 | 3,602 | 3,579 | 59.2 |
| ja | val2 | 4,737 | 4,842 | +105 | 4,693 | 5,289 | **+52.3 [+23.8, +80.8]** | 2,685 | 4,405 | 4,286 | 62.2 |

jemalloc columns: minutes 5-10 mean of the 5 s gauges. `retained` is address space jemalloc keeps mapped but has
returned to the OS (not in RSS). Exec queue: `torus_exec_queue_depth`, 1 s sampler. The cap is ~66 blocks (max seen 66 in
every arm).

Deltas per node (GB: mean min 5-10 / @10 min / peak) and the OLS slope of the RSS difference series over the last 5 min
(5 s grid, MB/min, 95% CI with the AR(1) effective n of `ols()`):

| delta | val0 | val1 | val2 | mean of 3 |
|---|---|---|---|---|
| **jcs - jmf** RSS (Phase 3, je vs je) | +0.98 / +0.89 / +1.21; +43 [-4, +90] | +0.98 / +1.32 / +1.55; **-80 [-134, -27]** | +0.86 / +0.78 / +1.05; +8 [-49, +65] | **+0.94 / +1.00 / +1.27** |
| jcs - jmf je allocated (live) | +0.55 / +0.87 / +1.12; -39 [-110, +31] | +0.57 / +0.72 / +0.94; +12 [-58, +82] | +0.51 / +0.79 / +0.80; +45 [-18, +108] | +0.54 / +0.79 / +0.95 |
| jcs - jmf je resident | +0.99 / +1.08 / +1.47; +31 [-21, +83] | +1.06 / +0.82 / +1.43; **-79 [-148, -10]** | +0.91 / +0.79 / +1.33; +5 [-57, +68] | +0.99 / +0.90 / +1.41 |
| jcs - jmf RSS - je resident (non-heap) | -0.01 | -0.07 | -0.05 | -0.04 |
| ja - jb RSS (je vs glibc, no fix) | +0.11 / +0.14 / +0.25; +24 [-1, +49] | +0.19 / +0.11 / +0.50; **+44 [+15, +73]** | +0.15 / -0.07 / +0.42; +17 [-9, +43] | +0.15 / +0.06 / +0.39 |

* **Levels off.** jmf is flat (last-5-min slopes -19 to -2 MB/min, CIs span 0). jcs is flat or falling (+22 / -78 / +4,
  val1's CI below 0), and so is the difference to jmf. jcs swings around its level, up to ~+0.5 GB above its minutes 5-10
  mean (the 15 s checkpoint rebuild and jemalloc decay; allocated swings with it). The CIs are +-50-70 MB/min wide, so slow growth of up to
  ~+50-90 MB/min on val0 is not excluded by one cell. val0's difference slope (+43 [-4, +90]) is the closest to growth.
* **Exec backlog vs Phase 3.** In section 53d the jcs2 - mf delta (+1.10 GB) carried jcs2's full exec queue on val0 / val1
  (65 / 62 blocks vs ~1.5 on mf) and the allocator. Here both arms are je and both sit at the cap (58-65 blocks on every
  node, minutes 5-10). So the exec backlog contributes ~0 to jcs - jmf. The delta is Phase 3: `41a95355` is `de97682f` +
  35 commits, all on the Phase 3 step 1 / step 2 branches (incl. their merges and one ignored test repro).
* **Live data vs allocator.** Of the jcs - jmf delta, live allocations are +0.51 to +0.57 GB per node (minutes 5-10) and
  allocator-resident pages (resident - allocated) another +0.40 to +0.49 GB. Live Phase 3 data is about 2.5-3 x the layer
  per checkpoint (203 MB mean, 57.2). RocksDB memtables are 50-60 MB *lower* on jcs (224-246 vs 274-293 MB), so the Phase 3
  structures are ~+0.6 GB live. jemalloc's resident / allocated ratio is 1.35-1.55 on jmf and 1.44-1.59 on jcs. jcs also
  retains more address space (5.2-7.8 vs 3.6-4.7 GB), outside RSS.
* **ja vs jb (no fix).** On the mean ja is +0.11 to +0.19 GB above jb, and on val2 (both at the exec cap) +0.15 / -0.07 /
  +0.42. On val0 / val1, ja holds ~60-65 blocks in the exec queue vs ~2 on jb, which section 48 puts at up to ~+0.8 GB. So
  jemalloc does not lower main's RSS at 10 min under this collapsed workload. Both arms still grow slowly in the last 5 min,
  ja faster (difference +17 to +44 MB/min; CI excludes 0 on val1 only). ja's live allocations rise +34 to +54 MB/min (CIs
  exclude 0) while its book falls (resting 0.31-0.35 -> 0.19-0.27 M). What grows is not identified here. Part A's lower
  early RSS growth on ja is not visible over 10 min: ja starts higher (+0.48 GB at 1 min on val0) and the gap narrows to
  +0.1-0.2 GB by minute 3.

### 57.2 Per minute per node (val0; val1 / val2 within a few % except as noted, all in `series.txt`)

![per node per minute: expiry, cancel-alls, margin rejections, matched/s, book, exec queue / busy / ms per block, je overhead, layer, flush queue](ozarchy-p3jesoak-figs/fig-node.png)

*Figure 57-2: per node (columns), per minute: nonce-expired evictions, cancel-alls executed/s, margin-rejected orders/s,
matched/s, resting orders, exec queue depth, exec busy, exec ms per block, jemalloc resident - allocated, layer MB per
checkpoint and flush-worker depth. Same colours as figure 57-1.*

Cells: RSS MB at minute end / jemalloc allocated / resident MB (minute mean) / matched/s / resting orders M (minute
end) / exec queue (mean) / exec ms per block (`torus_exec_block_seconds` sum delta / count delta in the minute) / expiries
(nonce-expired evictions in the minute) / margin-rejected orders/s. Exec busy is 0.95-1.00 in every minute on jmf, jcs and
ja; on jb it falls to 0.73-0.85 from minute 4.

| min | jmf, val0 | jcs, val0 | jb, val0 | ja, val0 |
|---|---|---|---|---|
| 1 | 3,597 / 1,871 / 2,756 / 200k / 0.22 / 49 / 198 / 0 / 0 | 4,266 / 2,146 / 3,196 / 205k / 0.17 / 50 / 193 / 0 / 0 | 3,290 / - / - / 208k / 0.07 / 39 / 224 / 0 / 0 | 3,733 / 1,904 / 2,750 / 204k / 0.16 / 52 / 255 / 0 / 0 |
| 2 | 3,968 / 2,530 / 3,544 / 199k / 0.02 / 62 / 298 / 0 / 0 | 4,605 / 2,991 / 4,458 / 212k / 0.13 / 63 / 292 / 0 / 0 | 3,743 / - / - / 222k / 0.44 / 29 / 318 / 0 / 0 | 4,035 / 2,463 / 3,416 / 222k / 0.25 / 64 / 288 / 0 / 0 |
| 3 | 4,247 / 2,688 / 3,773 / 203k / 0.69 / 62 / 397 / 0 / 0 | 5,159 / 3,266 / 4,590 / 222k / 0.71 / 62 / 380 / 0 / 0 | 3,805 / - / - / 209k / 0.49 / 15 / 356 / 3,431 / 0 | 3,979 / 2,661 / 3,580 / 218k / 0.58 / 65 / 360 / 2,124 / 0 |
| 4 | 4,193 / 2,857 / 3,852 / 192k / 0.72 / 62 / 418 / 0 / 0 | 5,079 / 3,371 / 4,720 / 220k / 0.65 / 62 / 366 / 0 / 0 | 3,863 / - / - / 186k / 0.41 / 2 / 342 / 3,471 / 0 | 3,938 / 2,552 / 3,502 / 212k / 0.35 / 65 / 351 / 1,772 / 0 |
| 5 | 4,117 / 2,850 / 3,859 / 192k / 0.66 / 61 / 417 / 0 / 0 | 5,009 / 3,467 / 4,778 / 220k / 0.62 / 61 / 371 / 0 / 0 | 3,888 / - / - / 192k / 0.32 / 2 / 332 / 1,190 / 0 | 3,893 / 2,531 / 3,497 / 213k / 0.36 / 65 / 345 / 3,404 / 0 |
| 6 | 4,360 / 2,766 / 3,858 / 194k / 0.74 / 60 / 419 / 0 / 0 | 5,367 / 3,472 / 4,815 / 220k / 0.62 / 60 / 371 / 0 / 0 | 3,901 / - / - / 169k / 0.33 / 2 / 315 / 2,321 / 22.7k | 4,049 / 2,515 / 3,523 / 192k / 0.27 / 65 / 336 / 2,097 / 28.6k |
| 7 | 4,046 / 2,773 / 3,808 / 189k / 0.63 / 59 / 422 / 0 / 0 | 4,857 / 3,330 / 4,816 / 215k / 0.54 / 59 / 375 / 0 / 0 | 3,910 / - / - / 127k / 0.12 / 2 / 255 / 3,220 / 96.6k | 4,047 / 2,578 / 3,530 / 175k / 0.32 / 65 / 290 / 1,957 / 107.2k |
| 8 | 4,207 / 2,731 / 3,748 / 191k / 0.71 / 59 / 420 / 0 / 0 | 5,220 / 3,208 / 4,706 / 221k / 0.73 / 58 / 376 / 0 / 0 | 3,931 / - / - / 141k / 0.37 / 2 / 260 / 1,980 / 120.3k | 4,028 / 2,598 / 3,598 / 177k / 0.39 / 65 / 291 / 0 / 108.0k |
| 9 | 4,056 / 2,776 / 3,812 / 193k / 0.69 / 58 / 419 / 0 / 0 | 5,005 / 3,258 / 4,839 / 213k / 0.66 / 58 / 380 / 0 / 0 | 3,982 / - / - / 154k / 0.45 / 2 / 279 / 989 / 92.9k | 4,123 / 2,686 / 3,686 / 172k / 0.28 / 65 / 293 / 0 / 100.8k |
| 10 | 4,147 / 2,714 / 3,738 / 192k / 0.69 / 57 / 419 / 0 / 0 | 5,137 / 3,263 / 4,831 / 221k / 0.71 / 57 / 374 / 0 / 0 | 3,988 / - / - / 135k / 0.34 / 3 / 260 / 755 / 120.4k | 4,051 / 2,671 / 3,741 / 159k / 0.11 / 65 / 288 / 0 / 120.1k |

* **Expiries and rejections.** jmf and jcs: 0 nonce-expired evictions, 0 margin rejections and 0 action failures on every
  node, 0 bench in-flight timeouts. Cancel-alls sent / executed per node: jmf 211,681 / 211,678, jcs 235,134 / 235,105 (the
  rest not accepted at submit; the bench's refused releases, places included, are 86 / 111). **jb: 17,357 nonce-expired evictions per node, the first at
  153.5-154.2 s (minute 3), the last in minute 10; 16,602 bench releases by the 70 s timeout; 27.39 M margin-rejected
  orders per node from minute 6 (93-137k/s). ja: 11,354 evictions per node, first at 152.3-152.9 s, none after minute 7;
  11,354 timeouts; 31.11 M margin-rejected orders from minute 6 (100-124k/s).** Cancel-alls sent - executed = the
  evictions in both (jb 17,357, ja 11,365 incl. 11 refused). The same pattern as sections 53 / 53b without the fix.
* **Book and layer (jcs).** Resting orders 0.62-0.73 M (jmf) and 0.54-0.73 M (jcs) per node from minute 3, flat. jb / ja
  fall from minute 6 (to 0.21-0.30 / 0.19-0.27 M at the end). jcs: 47-48 checkpoints per node, layer 202.6-202.8 MB mean
  (early -> late 5 min 193 -> 212 MB), `layer_shared` 0, drop_inline 1,910-2,039 per node, settle wait 26 -> 34 ms per
  native block. Same as 53d's jcs2 (204 MB, 48 checkpoints).
* **Flush worker and exec.** Flush-worker depth 0.1-0.2 (jcs) and 0.4-0.6 (jmf) every minute: no block backlog in the flush
  queue. Exec ms per block (minutes 5-10): jmf 419-421, jcs 376-377, ja 297-299, jb 273-324. Native blocks/s (val0,
  minutes 3-10): jmf 2.32-2.47, jcs 2.63-2.73 (+6 to +15% per minute).
* **Block-build select time** (proposer builds, minutes 2-10 mean, val0 / val1 / val2): jmf 33.6 / 32.9 / 33.6 ms, jcs 22.6
  / 28.0 / 24.2 ms at mempool 6.1-6.4k on both. This is 53d's "main + fix selects slower" observation again, now je vs je.
  It is consistent with step 2's validator ingress readers on a published view, but not established.

### 57.3 matched/s: minutes 0-2 vs 6-10, every soak cell (val0 `torus_orders_matched_total`, 1 s sampler)

Each rate is the counter delta over the window divided by the same window's length (bench seconds 0-120, 120-360 and
360-600; the bench window is 602-604 s).

| section | cell | min 0-2 | min 2-6 | min 6-10 | 6-10 vs 0-2 | whole window (harness) |
|---|---|---|---|---|---|---|
| 57 | **jmf** main + fix `0360c840`, je | 199,262 | 195,252 | 191,138 | **-4.1%** | 194,684 |
| 57 | **jcs** step 2 + fix `e7be8460`, je | 208,457 | 220,588 | 217,531 | **+4.4%** | 216,765 |
| 57 | **jb** main `deeacdb3`, glibc, no fix | 214,934 | 188,983 | 139,385 | **-35.1%** | 174,556 |
| 57 | **ja** main `88ddc4c2`, je, no fix | 213,025 | 208,839 | 170,704 | **-19.9%** | 194,248 |
| 53d | mf `de97682f`, glibc | 201,476 | 185,861 | 176,031 | -12.6% | 185,054 |
| 53d | cs2 `41a95355`, glibc | 204,027 | 192,917 | 181,995 | -10.8% | 190,602 |
| 53d | jcs2 `41a95355`, je (LD_PRELOAD) | 207,969 | 217,500 | 213,669 | +2.7% | 214,299 |
| 53b | main `219f0c20`, je (LD_PRELOAD) | 208,224 | 212,613 | 163,200 | -21.6% | 191,756 |
| 53 | main `219f0c20`, glibc | 200,881 | 186,653 | 135,177 | -32.7% | 168,384 |

* With the fix and jemalloc in both arms the sustained rate holds: jmf -4.1%, jcs +4.4% from minutes 0-2 to 6-10 (glibc
  with the fix lost 11-13% in 53d). The exec queue stays at the cap, exec busy 0.97-1.00, submits ~930 (jmf) / ~1,050
  (jcs) actions/s from minute 4.
* Without the fix, jemalloc compiled in loses -19.9% vs glibc's -35.1%, like 53b's LD_PRELOAD main vs 53's glibc main
  (-21.6% vs -32.7%).
  ja keeps the exec queue full while jb goes input-bound after minute 3.
* jcs runs +11.3% matched/s over the whole window vs jmf (exec ms per block -10%, blocks/s +6 to +15% from minute 3),
  one cell per arm. Section 53d had cs2 vs mf (glibc) at +3.0% and jcs2 (je) vs mf (glibc) at +15.8%. Which part of Phase 3
  steps 1-2 gives it is not established here.

### 57.4 Agreement and other checks

k1-ja is rc 2 / REJECT DISAGREE on agreement: all three nodes agree on the block hash at the compared height 2,669 and on
matched / placed / resting / action counts. val1's state digest (taken at height 2,702) differs from val0 / val2 in the 5
mark fields (`accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable`) of all 50 sampled
accounts only; val0 and val2 agree (`a-digest.py`, part A's classifier). This is the slow-digest mark-only artifact of sections 46-56 (part A's
SAT r4-ja had the same shape): **no hard finding.** The other four cells and the smoke cell are ACCEPT AGREE. Per cell:
node exe md5 = the arm's md5 on 3 nodes, trie off, Classic env and cap 8, 4 MiB book CF, no trade/WAL knob, codec and LSM
defaults, the checkpoint knob only on jcs (enable line with `interval: Some(15s)` once per validator, 47-48 checkpoints per
node), exec pipeline on, genesis state hash off, btrfs data dirs, commit tail in run.log / bench cmd / bench.log /
summary.json, no LD_PRELOAD and no jemalloc mapped from a library on any node, allocator gauges on the je arms only. Max
open fds 385-459 per node.

### 57.5 Verdict

* **RAM gate under jemalloc (je vs je): Phase 3 levels off at +0.94 GB per node (minutes 5-10 mean; +0.86 to +0.98 per
  node), +1.00 GB at 10 min, +1.27 GB peak.** Rule (levels off, delta <= ~1 GB/node): met on the level and the mean, at the
  bound at 10 min, over it at the peak (1.05-1.55 GB per node). No exec-backlog or allocator confound in this pair. One
  cell per arm.
* **What the delta is.** ~+0.55 GB live allocations (Phase 3 layers and view, ~2.5-3 layers of ~203 MB; memtables 50-60 MB
  lower) and ~+0.45 GB jemalloc-resident pages not in use (resident - allocated). The peak overshoot is the 15 s rebuild
  cycle (+-300-500 MB swings). Shrinking the layer (~2.1x its logical size, section 53b) or tighter jemalloc decay would
  act on these two parts; neither is measured here.
* **Main, je vs glibc, without the fix:** no RSS saving at 10 min (+0.15 GB on the mean, partly ja's full exec queue on
  val0 / val1); both still grow slowly. jemalloc keeps more of the throughput through the margin collapse (-19.9% vs
  -35.1%). The workload is not the full one, so this is not the allocator's answer for main at full load. jmf vs 53d's mf
  (glibc, same source + fix) is the closest such comparison: RSS minutes 5-10 4.16 / 4.15 / 4.96 vs 3.98 / 3.97 / 5.04 GB,
  across campaigns.

Analysis (`tools/ozarchy-p3jesoak-tools/analyst/`): `a-series.py` (53d's copy with the cells jmf / jcs / jb / ja, pairs
jcs - jmf and ja - jb, the `torus_jemalloc_*` gauges per node and minute, exec busy / ms per block / queue depth, and the
difference slopes of RSS and of jemalloc allocated / resident; figures SVG -> PNG via `rsvg-convert`), `a-digest.py` (part
A's digest classifier), `a-archive.py`. Outputs `series.txt`, `digest.txt`. Full outputs (incl. `series.json`, SVGs) in
`ozarchy-p3jesoak-run/analyst/` on ozarchy. Driver: `tools/ozarchy-p3jesoak-tools/ozarchy-p3jesoak-campaign.sh`.

## 58. Gossipsub resubscribe-wedge fix: `fix/mesh-watchdog-resubscribe` `e3bb7cca` vs main `925fd962`, SIGKILL restart cells and a saturated A/B, commit-tail mode (campaign ozarchy-wedge, 2026-10-13)

**Kill cells: the wedge never formed in either arm, so this campaign cannot show the fix's effect on it.** In all 32
SIGKILL restarts (16 per arm, val1, in-place, same key), both survivors dropped the dead QUIC connection 9.98-10.11 s after
the kill (idle timeout). The restarted node's p2p came up later, 11.2-29.7 s after the kill: RocksDB open ~4.5 s plus
the replay of the exec queue at the kill. Every survivor saw it as a new first connection (`num_established=1`, 0 of 64
survivor-kill pairs with a ghost connection, `a-conn.txt`), so gossipsub exchanged subscriptions normally. The chain stall
per kill is therefore the restart time: the longest val0 commit gap is 19.7 s mean / 29.6 s max (wm) and 18.6 / 27.2 s (wf),
and it ends 1.4 s (wm) / 0.8 s (wf) on average after the restarted node's network starts. Paired per kill (same round
and kill index), wf - wm: longest gap -1.1 s [-3.7, +1.4], kill -> chain resumes -0.9 s [-3.4, +1.5], gap after
network start -0.8 s [-2.3, +0.8]. No difference is resolved. Mesh watchdog disconnects 0 in both arms, stale-connection
kicks 0 (wf). `NoPeersSubscribedToTopic` lines: 9 (wm) and 4 (wf) in total, all on the restarted node, 0-1 per kill.
`Failed to publish tx:` and `failed to publish native batch:` 0. Section 52's wedge needed the network back within the
~10 s window. SIGTERM gave that: the old process held its connection through the 3.7-10.7 s shutdown, then the restart
took 3-4 s. A SIGKILL can give it too, if the replay is short. Section 46's p3r1 y1-s-KILL cell (step 1, replay from a
recent checkpoint) brought the network back 8.1-8.8 s after each kill. 3 of its 4 kills left ghost connections on both
survivors (`num_established=2`), with 157-291 `NoPeersSubscribedToTopic` lines, 6 watchdog reconnects in the cell and a
37.5 s stall starting 47 s after kill 3. p3r1's other SIGKILL cells (network 10.3-22.8 s) had none. The precondition is
the network start, not the signal.

**Saturated A/B (no kill): no regression resolved.** wf - wm, paired over 4 ABBA rounds (95% CI, t(3)): matched/s
-0.86% [-2.42, +0.69], CPU-s/1M -0.39% [-1.79, +1.02], native blocks/s -0.42% [-9.1, +8.3], commit interval p99 +1.2%
[-10.9, +13.4], exec p99 -1.3% [-1.8, -0.8] (-8.7 ms). In every healthy cell, both arms showed watchdog disconnects 0,
stale-connection kicks 0 (wf), `NoPeersSubscribedToTopic` 0 and publish failures 0. wf's subscribe-delay histogram had
2 observations per node per cell (the startup connections), all in the 50 ms bucket (sum 1-2 ms).

Owner-approved (18c): bench the resubscribe-wedge fix. **wf** = `fix/mesh-watchdog-resubscribe` `e3bb7cca` (= wm + 3
commits: `df33fe75` RED test, `c9bcd951` connection-denying behaviours first (no ghost connections), `e3bb7cca`
stale-connection kick + mesh watchdog grace 5 s / tick 1 s, new `torus_mesh_stale_conn_disconnects` and
`torus_consensus_subscribe_delay_seconds`; node `a16b9ad2`). **wm** = main `925fd962` (watchdog grace 60 s on the 10 s mesh
tick; node `ea4445ca`). Both glibc (`malloc` UND in `.dynsym`), no Phase 3 (no checkpoint knob, no enable line, no
checkpoint series), `TORUS_MESH_*` / `TORUS_CONSENSUS_DIRECT_FAN` / `_GOSSIP_MIRROR` / `TORUS_GOSSIP_QUEUE_LEN` unset (node
defaults) on every node, checked per cell. Fresh target dirs and clean detached worktrees
(`ozarchy-wedge-build.sh`, the p3je build flags), md5s in `ozarchy-wedge-build/md5s-all.txt`. Bench `05d44313`, harness
`218ad251`, tw, Classic, 300 markets, MAX_IN_FLIGHT 4, budget 900, **commit tail** (as p3term and the recent A/Bs), shared NVMe.

### 58.1 Kill cells (section 52 method, SIGKILL)

Driver `ozarchy-wedge-kill-campaign.sh` (copy of `ozarchy-p3term-campaign.sh`): 540 s cells, state hash on, SIGKILL of
val1 at bench + 60, 180, 300, 420 s, same-key in-place restart (patched `crash-kill.sh`, KILL_WAIT 75 s), n = 4 ABBA
(r1 wm wf, r2 wf wm, r3 wm wf, r4 wf wm) + w0 (wm, 60 s, excluded). The sampler scraped each node's `/metrics` every 1 s,
including `torus_consensus_subscribed_validators`, `torus_mesh_watchdog_disconnects_total`, `torus_mesh_stale_conn_disconnects_total`
and the subscribe-delay histogram. The plan killed val2 in rounds 2 and 4. The first launch stopped after r2-wf: the
bench's commit-mode block tail reads val2's RPC (`127.0.0.1:8647`), so a down val2 gives 469 tail errors and the
driver's tail-WARNING stop. r2-wf (val2) is kept as an extra outside the ABBA set. The rest ran with val1 kills under the
tags r2b-* / r3-* / r4-* (`RESUME=1`). A 150 s wf smoke cell (one kill) checked the samplers and the grep first.

Per arm (16 kills each; mean / median / min / max, s from the kill; chain = val0's `on_committed_block` lines):

| | wm `925fd962` | wf `e3bb7cca` |
|---|---|---|
| kill -> restarted node's p2p started | 18.6 / 17.8 / 11.3 / 29.7 | 18.2 / 18.7 / 11.2 / 27.1 |
| kill -> chain resumes (end of the longest gap) | 20.0 / 19.6 / 12.1 / 29.9 | 19.1 / 20.2 / 11.5 / 27.3 |
| longest chain-wide commit gap | 19.7 / 19.5 / 12.1 / 29.6 | 18.6 / 19.5 / 11.5 / 27.2 |
| kill -> restarted node's first commit | 19.2 / 18.9 / 11.4 / 29.9 | 18.5 / 19.8 / 11.4 / 27.2 |
| gaps > 15 s / > 30 s | 13 / 0 | 12 / 0 |
| `NoPeersSubscribedToTopic` lines (val0 / val1 / val2) | 0 / 9 / 0 | 0 / 4 / 0 |
| tx / native batch publish failures | 0 / 0 | 0 / 0 |
| watchdog disconnects / stale kicks | 0 / n.a. | 0 / 0 |
| subscribe delay (wf, all nodes) | n.a. | 32 obs., all <= 50 ms, sum 0.26 s |

By kill index the longest gap is 13.9 / 21.2 / 24.8 / 19.1 s (wm) and 13.2 / 21.0 / 20.9 / 19.4 s (wf) for kills 1-4.
Kill 1 is shortest because the exec queue (= the replay) is still short 60 s into the bench. Section 52's "first chain
commit after the restart" is not used here. After a SIGKILL that restarts in 0.3 s, it is the block already voted before
the kill (0.4-1.3 s), not the end of the stall.

`consensus_subscribed_validators` per scrape: in wf the survivors show 1 from 10.9-11.9 s after the kill (the QUIC drop at
10.0 s plus the 1 s watchdog tick). The restarted node shows 0 at its first scrape, then all three show 2 again 1.6 s
after its network start on average (19.8 s after the kill). wm updates this gauge only on the 10 s mesh tick, so its drop
and recovery show up 0-10 s late (all-2 again 29.1 s after the kill on average, while the chain resumed at 20.0 s). The two
arms' gauge timelines are not comparable for this reason. The chain commit times above are.

Extra cell r2-wf (val2 killed, outside the ABBA set): longest gaps 16.5 / 28.4 / 27.9 / 21.3 s, network start 17.1-29.0 s,
1 `NoPeersSubscribedToTopic` line, no watchdog disconnect or stale kick. Its load ran with 469 block-tail errors (in-flight
slots freed by the 70 s timeout), so its throughput is not used.

Correctness: every kill cell rc 2 = the known crash-gate false FAIL (it matches the INFO line `running state hash fail-stop
(TORUS_STATE_HASH_FAILSTOP) on=false` in the restart tail). There were no HARD FINDING and no CHECK FAIL lines, and every
state-hash checkpoint was equal on the 3 validators (29-39 heights per cell, 0 disagree). Block hashes were equal. 4 of 8
cells (wm r2b, r3; wf r1, r3) end DISAGREE on the state digest. The restarted val1 differs only in the mark-based fields
(`accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable`) of 50 of 650 accounts. Cash,
balances and stake are equal. This is section 46 Open item 3's mark-only artifact (oracle paused before the account reads),
in both arms.

### 58.2 Saturated A/B (no kill)

Driver `ozarchy-wedge-sat-campaign.sh` (copy of `ozarchy-p3je-campaign.sh`, CAMP=WSAT): harness RATE 76000, 120 s cells,
n = 4 ABBA + w0 (wm, 60 s, excluded), 11:39-12:33. All 9 cells: rc 0, ACCEPT, AGREE. Analysis: section 56's
`a-cells.py` / `a-paired.py`.

| metric | wm | wf | wf - wm (95% CI, paired, t(3)) |
|---|---|---|---|
| matched/s | 200,292 | 198,561 | -1,731 [-4,839, +1,377] = -0.86% [-2.42, +0.69] |
| CPU-s / 1M matched | 25.86 | 25.76 | -0.39% [-1.79, +1.02] |
| native blocks/s | 3.546 | 3.531 | -0.42% [-9.14, +8.30] |
| commit interval p50 / p99 (ms) | 157 / 1,181 | 150 / 1,196 | p99 +1.24% [-10.94, +13.41] |
| exec per block mean / p99 (ms) | 342 / 675 | 346 / 666 | p99 -1.29% [-1.78, -0.79] |
| bench submit (window, /s) | 1,144 | 1,157 | +1.16% [-1.07, +3.40] |
| RSS peak / mean (GB, per node) | 4.11 / 3.49 | 4.17 / 3.53 | +1.5% / +1.1% (CIs include 0) |

Per round matched/s wf - wm: -472, -1,015, -4,642, -794. The resolved exec p99 difference (-1.3%) is small, and this
change does not touch the exec path. Read it as noise until repeated.

### 58.3 Verdict

* **No throughput or CPU regression** at saturation (matched/s -0.86% [-2.42, +0.69], CPU-s/1M -0.39% [-1.79, +1.02]),
  and no false-positive kicks or watchdog disconnects in healthy cells or in the 32 kill restarts.
* **The fix's benefit is not measured here.** With SIGKILL and this harness's restart path on main-based binaries (DB open
  + exec-queue replay before the network starts, 11-30 s), the survivors' 10 s QUIC idle timeout always clears the old
  connection first. Neither arm wedged, and the stalls (~19-20 s mean, max 30 s) are the restart time in both. The wedge
  needs the restarted node's network back within ~10 s of the kill. Section 52's SIGTERM cells (9 of 12 restarts on main)
  and section 46's y1-s-KILL cell (network at 8.1-8.8 s) had that, and so does 18c's in-process repro (75.4 s -> 0.4-0.7 s).
  A bench of the benefit needs section 52's SIGTERM cells or a SIGKILL restart whose p2p starts within ~10 s (short
  replay, e.g. a Phase 3 step binary or kills while the exec queue is short). Owner's call.
* Trap for the harness: with `IN_FLIGHT_TAIL=commit` only val1 may be killed (the tail reads val2's RPC).

Analysis (`tools/ozarchy-wedge-tools/`): `a-wedge.py` (per kill: network start, survivor drop, chain resume / longest gap
from val0's commit lines, restarted node's first commit, publish-failure lines per window, mesh counter and histogram
deltas from the 1 s samples, subscribed-validators recovery), `analyst/a-conn.py` (ghost-connection check from the
survivors' `peer connected` / `peer disconnected` lines), `analyst/paired-kills.txt` (per-kill paired deltas). Outputs
`analyst/a-wedge.txt`, `a-conn.txt`, `kills.json`, `sat-paired.txt`, `sat-cells.txt`. Drivers
`ozarchy-wedge-kill-campaign.sh`, `ozarchy-wedge-sat-campaign.sh`, builds `ozarchy-wedge-build*.sh`, md5s `analyst/md5s-all.txt`.

## 59. Cloid cost A/B: `CLOID_SHARE=100` vs `0` on one binary, main `ef812b02` (glibc), saturated, commit-tail mode (campaign ozarchy-p3cloid, 2026-10-13)

**When every placed order carries a client order id (cloid), the node matches 11.7% fewer orders per second: matched/s
-11.7% [-13.8, -9.6] (paired c100 - c0, n = 4, 95% CI t(3)), placed/s -11.8% [-13.9, -9.6]. Each fill costs more node
CPU: CPU-s/1M +8.8% [+7.4, +10.3] (+6.4% [+3.1, +9.7] at equal fills). The cost sits in the executor. Exec `block_ms` is
+11.8% [+1.7, +22.0] (+31 ms per native block), engine ms per 1k fills +13.9% [+10.6, +17.2] and signature verify +2.8 ms
per native block [+1.8, +3.9] (+25%). Exec per native block p99 is +18.9% [+9.4, +28.4].** Exec is the bottleneck in both
arms (busy 0.952 / 0.955, backlog ~43 blocks, mempool equally full), so the load generator is not what limits c100. The
closed-loop bench submits 6.9% less [-9.6, -4.1], following the slower commits. Native blocks/s -7.0% [-15.2, +1.2] is
not resolved. RSS peak is +0.32 GB [+0.14, +0.49] (+7.5%), mean +0.24 GB [+0.13, +0.35] (+6.6%). Device writes are flat
(+1.8% [-4.0, +7.7]). The cloid share reached the chain exactly as set: stamped / placed 0 and 1.0000, `share_pct` 0 / 100,
and 0 `duplicate_cloid` rejects on all 3 nodes in every cell. All 8 measured cells, the warm-up and the smoke cell are
rc 0, ACCEPT, AGREE (no mark-only digest REJECT this time). **100% is the worst case. The cost at a realistic cloid share
was not measured, and this A/B does not show that it scales linearly with the share.**

Requested by 18c (s116) as optional filler: what does the cloid feature (`feat/cloid` `46a11c14`: u128 client order ids,
unique across a trader's open orders via the `CloidMarkets` / `BookCloids` indices, consensus format change) cost per
order? One binary in both arms: main `ef812b02`, node and bench built together, because cloid changed the signed-order
format and the bench must match the node commit. The arms differ only in run-cell.sh `CLOID_SHARE` (bench
`--cloid-share`, `ba02c933`). c0 = `CLOID_SHARE=0` (by construction the same actions, byte for byte, as a bench without
the flag) and c100 = `CLOID_SHARE=100` (every placed order carries a cloid, `(run start ms << 64) | per-sender counter`,
nonzero and unique per sender).

Setup: the section 54 (p3mw) shape (tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, harness
RATE 76000 = saturated, 120 s cells + 60 s warm-up, commit tail, shared NVMe), no Phase 3 (no checkpoint knob, no enable
line, no checkpoint series), glibc. `ef812b02`'s default build has no jemalloc feature: `malloc` is `UND`
(`malloc@GLIBC_2.2.5`) in `.dynsym`, and no preload or mapped `libjemalloc` on any node in any cell. Built 13:14 with the
p3q build flags (frame pointers, line tables, mold) into a fresh target dir (`~/.cargo-target-p3cloid-ef812b02`) from a
clean detached worktree (`/home/oz/projects/wt/p3cloid-ef812b02`): node `6f363582` (build id `016bed06`), bench
`9fda6c84` (build id `3cdf34c8`). Script `ozarchy-p3cloid-build.sh`, md5s in `ozarchy-p3cloid-build/md5s.txt`, staged in
`ozarchy-p3cloid-stage/cl-run/release`. Harness = the same `ef812b02` worktree (run-cell.sh with `CLOID_SHARE`,
summarize.py with `ingest.cloids`, `2eedbce7`). Driver `ozarchy-p3cloid-campaign.sh` CAMP=CLOID (copy of
`ozarchy-p3q-campaign.sh`) adds a hard check per cell, `cloidchk`. It requires `cloid_share='P'` in run.log,
`--cloid-share P` in the bench command, `summary.json` `ingest.cloids` `share_pct` = P with placed > 0 and stamped / placed
= 0 (P = 0) or >= 0.99 (P = 100), and `torus_orders_rejected_reason_total{reason="duplicate_cloid"}` = 0 on all 3 nodes
(`metrics-after`). A 60 s c100 smoke cell ran first (`ozarchy-p3cloid-smoke-300m-s0-c100`, 13:16-13:19: rc 0, ACCEPT,
AGREE, all checks OK). Cells ran 13:22-14:14: w0 (c0, 60 s, excluded), r1 c0 c100, r2 c100 c0, r3 c0 c100, r4 c100 c0.
The log has 135 CHECK OK, no CHECK FAIL, no deaths and no memguard stop. The host was quiet before every cell (no
cargo / rustc / rust-analyzer, load 1.74-1.94). Cells are 120 s long, so **every figure here is minutes 0-2 only**.

### 59.1 Cells and cloid checks

| cell | arm | rc / agreement | `share_pct` | stamped | placed (orders) | stamped / placed | `duplicate_cloid` rejects val0 / val1 / val2 | matched/s | submit/s |
|---|---|---|---|---|---|---|---|---|---|
| smoke s0-c100 (60 s) | c100 | 0, ACCEPT AGREE | 100 | 18,461,600 | 18,461,600 | 1.0000 | 0 / 0 / 0 | 171,674 | 1,196 |
| w0 (60 s, excluded) | c0 | 0, ACCEPT AGREE | 0 | 0 | 19,174,800 | 0.0000 | 0 / 0 / 0 | 179,134 | 1,254 |
| r1-c0 / r1-c100 | | 0 / 0, ACCEPT AGREE | 0 / 100 | 0 / 31,262,800 | 33,318,000 / 31,262,800 | 0 / 1.0000 | 0 / 0 / 0 both | 188,242 / 169,297 | 1,113 / 1,038 |
| r2-c0 / r2-c100 | | 0 / 0, ACCEPT AGREE | 0 / 100 | 0 / 31,139,600 | 33,492,400 / 31,139,600 | 0 / 1.0000 | 0 / 0 / 0 both | 185,643 / 162,832 | 1,098 / 1,033 |
| r3-c0 / r3-c100 | | 0 / 0, ACCEPT AGREE | 0 / 100 | 0 / 31,641,200 | 33,140,400 / 31,641,200 | 0 / 1.0000 | 0 / 0 / 0 both | 194,876 / 169,909 | 1,105 / 1,046 |
| r4-c0 / r4-c100 | | 0 / 0, ACCEPT AGREE | 0 / 100 | 0 / 29,617,600 | 32,258,400 / 29,617,600 | 0 / 1.0000 | 0 / 0 / 0 both | 185,856 / 164,167 | 1,081 / 979 |

The `duplicate_cloid` series is present (at 0) on every node of every cell, c0 included. The bench never reused a cloid
among a trader's open orders, and no node rejected one. Per round, matched/s c100 - c0 is -18,945 / -22,811 / -24,967 /
-21,689: all four rounds point the same way.

### 59.2 Per arm (n = 4, mean; paired c100 - c0 over the 4 rounds, 95% CI t(3))

| | c0 (`CLOID_SHARE=0`) | c100 (`CLOID_SHARE=100`) | c100 - c0 | % [95% CI] |
|---|---|---|---|---|
| matched/s (120 s = minutes 0-2) | 188,654 | 166,551 | -22,103 [-26,091, -18,115] | **-11.72% [-13.83, -9.60]** |
| placed/s | 246,747 | 217,751 | -28,997 | **-11.75% [-13.91, -9.59]** |
| bench submits/s | 1,099 | 1,024 | -75 | **-6.85% [-9.60, -4.09]** |
| native blocks/s | 3.41 | 3.17 | -0.24 | -6.99% [-15.20, +1.22] |
| actions / fills per native block | 270 / 50,613 | 267 / 50,332 | | -1.0% [-7.9, +5.8] / -0.6% [-9.3, +8.1] |
| exec `block_ms` (summary, mean of 3 nodes) | 262.3 | 293.3 | +31.1 [+4.5, +57.6] | **+11.84% [+1.72, +21.97]** |
| exec `chain_ms` (summary headline, val0) | 257.4 | 288.3 | +30.9 [+5.0, +56.7] | **+11.99% [+1.96, +22.03]** |
| exec per native block (logs) mean / p50 / p99 ms | 366 / 370 / 720 | 406 / 417 / 856 | +40.1 / +47.0 / +136.0 | **mean +11.0% [+5.7, +16.3], p50 +12.7% [+5.4, +20.0], p99 +18.9% [+9.4, +28.4]** |
| engine ms per 1k fills | 4.12 | 4.69 | +0.57 [+0.44, +0.71] | **+13.90% [+10.59, +17.22]** |
| block time (commit interval, logs) p50 / p99 ms | 159 / 1,251 | 155 / 1,402 | -3.6 / +151 | p50 -2.3% [-16.3, +11.7], p99 **+12.1% [+2.7, +21.5]** |
| exec busy | 0.952 | 0.955 | +0.002 | +0.2% [-0.6, +1.1] |
| exec backlog mean / max (blocks) / lag s | 42.5 / 62.7 / 12.3 | 43.5 / 63.0 / 13.2 | +1.1 / +0.3 / +1.0 | +2.5% [-11.9, +16.8] / +0.5% / +7.9% [-3.6, +19.4] |
| mempool mean / max (actions) | 5,705 / 11,858 | 5,686 / 11,786 | | -0.3% [-7.3, +6.6] / -0.6% [-6.3, +5.1] |
| node CPU-s/1M (whole run) | 26.43 | 28.77 | +2.34 [+1.95, +2.73] | **+8.84% [+7.37, +10.31]** |
| node CPU at equal fills (ANCOVA, Welch) | | | | **+6.37% [+3.05, +9.70]** |
| node CPU-s / fills per node, whole run (M) | 668 / 25.27 | 680 / 23.64 | | |
| RSS peak / mean per node GB (cpu.csv, mean of 3) | 4.25 / 3.62 | 4.57 / 3.86 | **+0.32 [+0.14, +0.49] / +0.24 [+0.13, +0.35]** | +7.50% [+3.40, +11.61] / +6.63% [+3.55, +9.70] |
| RSS peak / mean in the bench window GB (mem-samples) | 4.24 / 3.66 | 4.46 / 3.76 | +0.22 [+0.07, +0.38] / +0.10 [-0.20, +0.39] | +5.2% [+1.5, +8.9] / +2.7% [-5.4, +10.8] |
| VmHWM GB / RSS last 15 s of the window GB | 4.38 / 4.19 | 4.72 / 4.43 | +0.34 [+0.06, +0.62] / +0.23 [+0.07, +0.39] | +7.7% / +5.6% |
| RSS growth over the bench window GB/min | 0.86 | 1.11 | +0.25 [-0.11, +0.60] | +29% [-13, +70] |
| memtables MB / resident rows MB | 292 / 195 | 276 / 190 | | -5.6% [-15.5, +4.3] / -2.9% [-8.5, +2.7] |
| device writes MB/s per validator (io-ticks, bench window) | 145.1 | 147.7 | +2.7 [-5.8, +11.2] | +1.8% [-4.0, +7.7] |

Welch intervals (`arms.txt`) agree in sign and resolution with the paired ones for matched/s (-11.72% [-15.35, -8.08]),
CPU-s/1M (+8.84% [+6.38, +11.30]), engine ms per 1k fills, exec p99 (+18.87% [+10.31, +27.43]), `block_ms` (+11.84%
[+4.16, +19.52]) and RSS peak / mean (+7.50% [+4.19, +10.81] / +6.63% [+4.39, +8.87]). Two rows differ. The block time
p99 is just not resolved under Welch (+12.1% [-0.4, +24.6]). The RSS growth is resolved under Welch (+28.7% [+2.9,
+54.5]) but not paired, so read it as "consistent with faster growth", not established. Node CPU-s per cell is nearly flat
(668 vs 680), while fills per node fall 6.5%: the executor spends the same CPU and gets fewer fills done.

### 59.3 Where the exec time goes (per native block, ms, mean of 3 nodes; paired c100 - c0, 95% CI t(3))

| phase | c0 | c100 | c100 - c0 [95% CI] |
|---|---|---|---|
| `verify` (signature verify) | 11.48 | 14.30 | **+2.82 [+1.78, +3.86]** (+24.6%) |
| `replay_guard` | 2.84 | 2.62 | -0.23 [-0.70, +0.25] |
| `load_books` | 0.33 | 0.30 | -0.03 [-0.08, +0.02] |
| `engine` | 213.49 | 240.41 | **+26.92 [+5.12, +48.72]** (+12.6%) |
| `action_status` | 0.28 | 0.26 | -0.02 [-0.05, +0.01] |
| `save_books` | 13.22 | 13.56 | +0.34 [-1.44, +2.12] |
| `end_resident_wait` | 7.36 | 3.93 | **-3.44 [-4.54, -2.33]** |
| `residual_untimed` | 13.26 | 17.95 | **+4.70 [+4.09, +5.30]** (+35.4%) |
| `flush` (off the chain path) | 75.22 | 82.33 | +7.11 [-3.47, +17.69] |
| `end_resident` (off the chain path) | 37.69 | 36.39 | -1.29 [-6.12, +3.53] |

The on-chain phases add up to the `block_ms` delta: +2.82 (verify) + 26.92 (engine) + 4.70 (residual) - 3.44
(end_resident wait) + 0.06 (the rest) = +31.06 ms. **The engine carries ~87% of the extra exec time, signature verify ~9%
and the untimed residual ~15%, partly offset by a shorter wait for the end_resident worker.** At about the same actions
and fills per block (-1.0% / -0.6%, ties), that points at per-order work. Verify +25% is consistent with hashing and
verifying larger signed orders (the u128 cloid in the canonical bytes). The engine +13.9% per 1k fills is consistent
with the per-order cloid index work (`CloidMarkets` insert / remove, `BookCloids` per book) on place, fill and
cancel-all. No profile was taken, so the split inside the engine is not shown. The shorter `end_resident_wait` is consistent
with the end_resident worker having more time to finish while the longer engine phase runs.

### 59.4 Agreement and other checks

All 8 measured cells, w0 and the smoke cell: rc 0, ACCEPT, AGREE. In every cell the 3 nodes' digests took within
~3.4 s of each other (24-79 s per cell), so no node was slow enough for the mark-only digest artifact of sections 46-56,
and no cell had that REJECT. Nonce-window evictions were 0, in-flight timeouts 0, block-tail errors 0 and action
failures 0 in every cell. Executed cancel-alls are equal on all 3 nodes and equal to those the load generator accepted. Action rejections (108-633 per cell) are equal on all 3 nodes in
every cell. In-flight RPC refusals: c0 3 / 0 / 3 / 0, c100 10 / 83 / 6 / 135 (r1-r4). That is more in c100, and small
next to the 115-135k released on commit per cell. The place / cancel-all mix is 61.1-62.3% place in both arms.

**Verdict:** at 100% share, cloids cost ~12% of the matched/s ceiling and ~9% CPU per fill (6% at equal fills) on this
glibc main. The cost is executor time per order: engine +14% per 1k fills, signature verify +25%, residual +35%, exec
`block_ms` +12% and exec p99 +19%. RSS is +0.2-0.3 GB per node (+7%) over minutes 0-2. Writes do not move. Exec is
saturated in both arms, so the drop is the node's, not the bench's. n = 4 x 120 s, but all four rounds agree:
decisive for matched/s, placed/s, CPU-s/1M (also at equal fills), engine per 1k fills, verify, exec `block_ms`, exec p99
and RSS peak / mean; "consistent with" for block time p99 and RSS growth; native blocks/s not resolved.

**Caveats.** (1) **100% share is the worst case.** Real traffic would stamp a share of orders. The cost at a realistic
share is not measured here. A lower share should cost less, but by how much is not known: this A/B has only the two end
points and does not show that the cost scales linearly with the share. A c25 / c50 arm would. (2) **glibc only.** `ef812b02` predates the
jemalloc merge (`655e874a`, now the default allocator on main). The cloid cost under compiled-in jemalloc is not measured.
(3) 120 s cells: minutes 0-2, not a soak. (4) c0 runs the same cloid-capable node with no cloids on the wire. The cost of
the cloid code at share 0 against a pre-cloid main is not measured. Section 58's wm (main `925fd962`, before `46a11c14`,
200,292 matched/s) is another campaign and is not comparable with c0's 188,654. (5) The engine attribution in 59.3 has
no profile behind it (section 60 profiles it).

Analysis (`tools/ozarchy-p3cloid-tools/analyst/`): `a-cells.py` (p3mw's, plus `ingest.cloids` and the `duplicate_cloid`
reject deltas per node), `a-arms.py`, `a-paired.py` (p3mw's, unchanged, with `--io` for device writes), `a-archive.py`.
Outputs `cells.txt`, `arms.txt`, `paired.txt`, `phases.txt` (per-phase paired deltas and the per-cell digest /
`duplicate_cloid` / action-rejection lines) and `cloids.txt` (per-cell share check). The analyst printed `phases.txt` and
`cloids.txt` from `cells.json` without a saved script; the verify / engine / residual / end_resident_wait / flush rows were
recomputed from `cells.json` for this write-up and match. Full outputs (incl. `cells.json`) in
`ozarchy-p3cloid-run/analyst/` on ozarchy. Driver `tools/ozarchy-p3cloid-tools/ozarchy-p3cloid-campaign.sh` (CAMP=CLOID,
`cloidchk`), build `ozarchy-p3cloid-build.sh`, md5s `md5s-all.txt`.

## 60. Cloid engine cost by perf profile: `CLOID_SHARE=100` vs `0` on main `ef812b02` (glibc), perf on val1, saturated, commit-tail mode (campaign ozarchy-p3cloidprof, 2026-10-13)

**The cloid engine cost is hash-map work, mostly on the exec thread: index upkeep and the duplicate check ~36% each,
cleanup ~14%.** Profiled on val1 (perf user space, call graphs, 3 ABBA rounds), the engine threads spend +1.00 ms CPU
per 1k fills more at c100: exec thread +0.71 (+16%), market workers +0.29 (+5%). Split by cloid site (exec + workers):
index upkeep +0.36 (`CloidMarkets::absorb` -> `OrderBook::drain_cloid_log` +0.32 on the exec thread, `index_cloid` +0.04
in the workers), duplicate check +0.36 (the serial pre-pass: claimed set +0.14 incl. `reserve_rehash` 0.06,
`duplicate_cloid` -> `CloidMarkets::live` +0.14; the per-book `contains_key` in place, `order_book.rs:1397-1402`, +0.08
in the workers), cleanup `forget_cloid` +0.14, and +0.14 not under any cloid site. 93% of the exec thread's extra CPU
sits at cloid sites. **All exec-thread cloid sites run outside the engine's phase timers, so the timers put the whole
engine increase in `engine_untimed`: 0.21 -> 0.78 ms per 1k fills (+0.57; the timed phases move -0.05 together).** The
extra DRAM demand fills of the exec thread (+12.7% per fill) are all at cloid sites, and those sites run at low IPC
(index 0.54, duplicate check 1.01, cleanup 0.76, against 1.18 for the c0 exec thread): the index maps miss the cache.
Hashing (`make_hash`, foldhash over the 36-byte `(Address, u128)` key) is ~27% of the exec thread's extra CPU. No lock
frames show on the engine threads. Outside the engine, val1's other threads spend +3.19 ms CPU per 1k fills more (+22%):
Keccak +0.91 (all threads; its callers are not known, see 60.5), the signature pre-verify `verify_one_action_with` +0.80
and RocksDB background threads +0.82 (mostly zstd compression). The profiled A/B reproduces section 59 within its
intervals (matched/s -12.5% [-21.6, -3.5] vs -11.7%, engine ms per 1k fills +13.1% vs +13.9%, CPU-s/1M +8.9% vs +8.8%).

Requested by 18c after section 59 (c100 vs c0: matched/s -11.7%, engine ms per 1k fills +13.9%, no profile behind the
engine split): where inside the engine does the cloid cost go? Same binary and arms as 59: the kept p3cloid stage
(`ozarchy-p3cloid-stage/cl-run/release`), node `6f363582` (build id `016bed06`, symbols present, not rebuilt) and bench
`9fda6c84` from main `ef812b02`, glibc. c0 = run-cell.sh `CLOID_SHARE=0`, c100 = `CLOID_SHARE=100`. Same shape as 59
(tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, RATE 76000 = saturated, 120 s cells + 60
s warm-up, commit tail, shared NVMe, no Phase 3), harness = the `ef812b02` worktree. Driver
`ozarchy-p3cloidprof-campaign.sh` CAMP=CLOID (copy of `ozarchy-p3cloid-campaign.sh` with the perf sidecar on both arms,
w0 + 3 rounds, a stop at a cell boundary on request). Cells ran 15:07-15:46: w0 (c0, 60 s, excluded), r1 c0 c100, r2
c100 c0, r3 c0 c100. 105 CHECK OK, no CHECK FAIL, all 7 cells rc 0, ACCEPT, AGREE. The host was quiet before every cell
(no cargo / rustc / rust-analyzer, load 1.71-1.98). `perf_event_paranoid` is 2, so the profile is user space only (`:u`,
no sudo, no kernel frames).

Profile (`cloidprof-sidecar.sh`, a copy of the p2s0y perf2 sidecar moved from val0 to val1, because val0 serves the
bench RPC and the headline numbers). W1 = 35-80 s after the bench start: `perf record -e cycles:u -F 499 -g --call-graph
fp -p <val1>`, the whole process with inherit (exec thread plus the short-lived market workers), 144-151k samples per
cell, none lost. W2 = 82-102 s: the exec thread only (`-t <exec tid> --no-inherit`), five sampled events: cycles,
instructions, L1-dcache-load-misses, `ls_dmnd_fills_from_sys.mem_io_local` (demand fills from local DRAM, used as the
LLC miss count because `LLC-load-misses:u` is not supported on Zen 3) and `ls_dmnd_fills_from_sys.ext_cache_local`
(fills from the other CCD's L3). Every figure below is ms of CPU (or events) per 1k val1 fills over the window: a sample
set's share of the period x val1 utime over the window / val1 fills over the window. Frames are inline-expanded with
`llvm-addr2line -i`. Thread groups: exec = the `torus-execution` task with the most CPU at W1 start (`exec-tid.txt`),
workers = comm `torus-exec-pool` (rayon market workers), other = the rest (RPC, consensus, RocksDB, flush). A sample's
cloid site is the innermost frame on its logical stack that matches the site list in `a-prof.py` (functions, plus line
ranges `native_executor.rs:6879-6940` for the pre-pass and `order_book.rs:1397-1402` for the per-book check), so a site
includes its callees (hashbrown, foldhash, memcpy). The W1 windows cover 47-48 s of each cell (`windows.txt`). **Every
figure here is minutes 0-2 only.**

### 60.1 Cells and the whole-cell A/B (n = 3, paired c100 - c0, 95% CI t(2))

| cell | arm | rc / agreement | `share_pct` | stamped / placed (orders) | `duplicate_cloid` rejects val0 / val1 / val2 | matched/s | native blk/s |
|---|---|---|---|---|---|---|---|
| w0 (60 s, excluded) | c0 | 0, ACCEPT AGREE | 0 | 0 / 18,635,200 | 0 / 0 / 0 | 190,622 | 4.48 |
| r1-c0 / r1-c100 | | 0 / 0, ACCEPT AGREE | 0 / 100 | 0 / 35,455,200; 31,727,200 / 31,727,200 | 0 / 0 / 0 both | 196,575 / 167,806 | 3.53 / 3.11 |
| r2-c100 / r2-c0 | | 0 / 0, ACCEPT AGREE | 100 / 0 | 31,670,000 / 31,670,000; 0 / 31,836,400 | 0 / 0 / 0 both | 172,126 / 188,140 | 3.24 / 3.51 |
| r3-c0 / r3-c100 | | 0 / 0, ACCEPT AGREE | 0 / 100 | 0 / 32,734,000; 30,456,400 / 30,456,400 | 0 / 0 / 0 both | 189,981 / 162,777 | 3.39 / 3.01 |

| | c0 | c100 | c100 - c0 | % [95% CI] | section 59 (n = 4, no perf) |
|---|---|---|---|---|---|
| matched/s (whole cell, val0 headline) | 191,565 | 167,570 | -23,996 [-41,277, -6,714] | **-12.53% [-21.55, -3.50]** | -11.72% [-13.83, -9.60] |
| placed/s | 250,428 | 219,216 | -31,212 | **-12.46% [-21.45, -3.47]** | -11.75% |
| bench submits/s | 1,113 | 1,037 | -76 | -6.80% [-19.30, +5.70] | -6.85% |
| native blocks/s | 3.48 | 3.12 | -0.36 | **-10.24% [-16.12, -4.37]** | -6.99% (n.s.) |
| exec `block_ms` (mean of 3 nodes) | 258.6 | 291.8 | +33.2 [+17.4, +49.0] | **+12.83% [+6.73, +18.93]** | +11.84% |
| engine ms per 1k fills (summary) | 4.07 | 4.60 | +0.53 [+0.11, +0.95] | **+13.10% [+2.81, +23.40]** | +13.90% |
| exec per native block p99 ms | 708 | 837 | +129 | **+18.20% [+10.33, +26.06]** | +18.9% |
| residual untimed ms per native block | 13.48 | 17.73 | +4.25 [+2.82, +5.68] | **+31.56% [+20.95, +42.16]** | +35.4% |
| exec busy | 0.951 | 0.953 | +0.002 | +0.20% [-1.64, +2.04] | +0.2% |
| node CPU-s/1M (whole run) | 26.35 | 28.70 | +2.36 [+0.91, +3.81] | **+8.94% [+3.44, +14.44]** | +8.84% |
| node CPU at equal fills (ANCOVA, Welch) | | | | **+7.37% [+4.02, +10.73]** | +6.37% |
| RSS peak / mean per node GB (cpu.csv) | 4.25 / 3.59 | 4.54 / 3.88 | **+0.29 [+0.28, +0.31] / +0.29 [+0.23, +0.34]** | +6.93% / +8.04% | +0.32 / +0.24 |
| device writes MB/s per validator | 144.0 | 148.5 | +4.4 [-6.3, +15.2] | +3.09% [-4.36, +10.53] | +1.8% |

All three rounds point the same way (matched/s -28,769 / -16,014 / -27,204). With n = 3 the intervals are about twice as
wide as 59's, and every row agrees with 59 within them. **Perf on val1:** val1's engine ms per 1k fills inside W1
is ~4% above its whole-cell value in both arms (c0 4.24 vs 4.08, c100 4.80 vs 4.59; `metrics.txt`). This is an observed
window-versus-cell difference, not a measured perf overhead (there is no unprofiled control, and the whole-cell value
includes the profiled window); it is the same in both arms, so it does not change the c100 - c0 picture. The headline matched/s comes from val0, which is not
profiled.

### 60.2 Where the extra CPU goes (W1, val1, ms CPU per 1k fills, mean of 3 cells per arm)

| thread group | c0 | c100 | c100 - c0 |
|---|---|---|---|
| whole process | 24.51 | 28.70 | +4.19 (+17.1%) |
| exec thread | 4.44 | 5.16 | **+0.71 (+16.1%)** |
| market workers (`torus-exec-pool`) | 5.83 | 6.11 | **+0.29 (+4.9%)** |
| other threads | 14.25 | 17.44 | +3.19 (+22.4%) |

By cloid site (`cloid-sites.txt`; inclusive of callees; c0 is 0.00-0.02 at every site):

| candidate | site (file:line at `ef812b02`) | exec | workers | exec + workers | share of +1.00 |
|---|---|---|---|---|---|
| 1 index upkeep | `CloidMarkets::absorb` -> `OrderBook::drain_cloid_log` (absorb loop after matching, `native_executor.rs:7309`); `OrderBook::index_cloid` (place, in the worker) | +0.324 | +0.038 | **+0.362** | 36% |
| 2 duplicate check | serial pre-pass (`native_executor.rs:6882-6930`): claimed `HashSet<(Address, Cloid)>` insert and `retain` +0.140, `duplicate_cloid` -> `CloidMarkets::live` +0.136; per-book `contains_key` in `OrderBook::place` (`order_book.rs:1397-1402`) | +0.276 | +0.081 | **+0.357** | 36% |
| 3 cleanup | `OrderBook::forget_cloid` (`order_book.rs:2946`; maker fills, cancels, stops) +0.143; `forget_cloids` (batch) +0.002 | +0.064 | +0.080 | **+0.144** | 14% |
| none | not under a cloid site | +0.048 | +0.088 | +0.136 | 14% |
| total | | +0.713 | +0.286 | +0.999 | |

What the sites spend it on (exec thread, `frames-children.txt` / `frames-self.txt`). Inclusive: hashbrown `get` +0.43,
`find` +0.32 (0.65 -> 0.97), `make_hash` / `hash_one` +0.19 (27% of the exec delta), the claimed-set `HashMap<(Address,
u128), ()>::insert` +0.14, of which `reserve_rehash` +0.055 (the set grows from empty in every block; computed from
`folded-*.txt` for this write-up). Self (leaf): `_mm_movemask_epi8` (hashbrown group probe) +0.189, `spec_eq` (key
compare) +0.141, foldhash `hash_bytes_long` +0.080 (the 36-byte key takes foldhash's long path), `write_u128` +0.018.
Locks: frames matching futex / `Mutex` / `RwLock` / `parking_lot` are 0.000-0.006 ms per 1k fills on the exec thread and
the workers in both arms (also from `folded-*.txt`; time blocked in the kernel is not visible to `cycles:u`, but exec
busy is 0.95 in both arms). The non-cloid +0.14 is net: on the exec thread Keccak +0.120 (its samples carry no call
stack, see 60.5; consistent with section 59's verify +25%) against small decreases (`dealloc_nonnull` -0.025, a
`native_executor.rs` closure -0.014); in the workers `place_order_with_accounts` self +0.055 and a btree `or_default`
+0.033 against `get_position` -0.033 and `reserve_price` -0.024 (not attributed further).

Engine phase timers (`metrics.txt`, whole cell, mean of val0-2, ms per 1k fills): engine 4.18 -> 4.69 (+0.51, +12.1%),
of which `engine_untimed` 0.211 -> 0.777 (+0.566; per round +0.570 / +0.537 / +0.592; per native block 9.3 -> 33.7 ms),
`phase1_actions` +0.041, `phase_margin` -0.033, `phase_match` -0.001, `phase_settle` -0.056. In the `ef812b02` source
the pre-pass runs after the Phase 1 timer stops and before the margin timer starts, and the absorb loop runs after the
match timer stops and before the settle timer starts, so both are untimed. The worker sites (per-book check,
`index_cloid`, worker `forget_cloid`) run inside the parallel match phase, whose wall time does not move.

### 60.3 Cache behaviour on the exec thread (W2, events per 1k val1 fills, mean of 3 cells per arm; `llc.txt`)

| event | c0 | c100 | % | at cloid sites (c100 - c0): index / dup check / cleanup | not at a site |
|---|---|---|---|---|---|
| cycles | 13.28 M | 15.17 M | +14.2 | +0.98 M / +0.84 M / +0.18 M | -0.11 M |
| instructions | 15.71 M | 19.07 M | +21.4 | +0.53 M / +0.86 M / +0.14 M | +1.83 M |
| L1-dcache-load-misses | 291,278 | 310,551 | +6.6 | +6,818 / +13,143 / +2,397 | -3,085 |
| DRAM demand fills (`mem_io_local`) | 33,084 | 37,288 | **+12.7** | **+2,785 / +961 / +512** | -53 |
| other-CCD L3 fills (`ext_cache_local`) | 225 | 275 | +22.4 | +22 / +23 / +6 | 0 |

IPC (c100, instructions / cycles at the site): index 0.54, duplicate check 1.01, cleanup 0.76; the rest of the exec
thread 1.33 (c0: 1.18). Whole exec thread IPC 1.18 -> 1.26 and DRAM fills per 1k instructions 2.11 -> 1.96, because
Keccak adds +1.53 M instructions at high IPC outside the cloid sites. **The extra DRAM fills are all at cloid sites, and
the index upkeep site runs at half the thread's IPC: draining the per-book cloid logs into the cross-market index
touches memory that is no longer in cache.** The top DRAM-fill leaves are the hashbrown probe (`_mm_movemask_epi8`
+1,282), the key compare (`spec_eq` +1,191) and foldhash (+614).

### 60.4 18c's hypotheses (read-only code check before the run)

| hypothesis | result |
|---|---|
| frame `CloidMarkets::live` (pre-pass duplicate check) | **confirmed**, +0.133 exec (all of `duplicate_cloid` +0.136) |
| frame `CloidMarkets::absorb` | **confirmed**, +0.324 inclusive, almost all in `drain_cloid_log` |
| frame `drain_cloid_log` | **confirmed**, +0.323, the largest single site (45% of the exec delta) |
| frame `reserve_rehash` | **confirmed but small**, +0.055 in the claimed-set insert; other rehash time is the same in both arms |
| frame `find` | **confirmed**, hashbrown `find` +0.32 inclusive on exec (+0.12 in the workers) |
| site `native_executor.rs:6882` (pre-pass) | **confirmed**, serial block +0.140 + `duplicate_cloid` +0.136 = +0.276 exec |
| site `native_executor.rs:7309` (`CloidMarkets::absorb` call) | **confirmed**, = the absorb / drain cost, +0.324 |
| site `order_book.rs:1400` (per-book `contains_key` in place) | **confirmed**, +0.081 in the workers |
| site `order_book.rs:2946` (`forget_cloid`) | **confirmed**, +0.143 (exec 0.063, workers 0.080); the batch `forget_cloids` only +0.002 |

### 60.5 Outside the engine

val1's other threads: +3.19 ms CPU per 1k fills (+22.4%). The largest parts:

- Keccak (`__KeccakF1600`) self +0.91 over all threads (4.14 -> 5.05; +0.79 outside the exec thread, +0.12 on it). Its
  samples carry no caller: the assembly Keccak keeps no frame-pointer chain, so `--call-graph fp` stops at the leaf
  (~99% of the Keccak samples outside the exec thread in `folded-*.txt` are a bare `other;__KeccakF1600` stack). Where
  it is called from (signature hashing, action hashes, block or DA hashes) is therefore not known from this profile.
- The rayon signature pre-verify `torus_rpc::torus::verify_one_action_with` +0.80 inclusive (2.59 -> 3.39, +31%), not
  counting the unattributed Keccak above; k256 `mul_inner` +0.08 of it.
- RocksDB background threads (`BGThread`) +0.82 inclusive (1.49 -> 2.31, +55%), mostly zstd compression (zstd leaves
  under the background threads 0.61 -> 1.17, computed from `folded-*.txt` for this write-up).

These threads do not lengthen the exec thread's block time, but they are part of the +8.9% CPU-s/1M. A cost that does
not scale with fills rises ~12% per fill from the normalisation alone (W1 val1 fills/s 187,635 -> 167,559, -10.7%);
verify (+31%) and RocksDB background (+55%) rise more than that. Larger signed orders (the u128 cloid in the canonical
bytes) and larger stored order rows are consistent with both; not checked here.

**Verdict:** the cloid engine cost at 100% share is per-order hash-map work. On the exec thread it is the index upkeep
(drain the per-book cloid logs into `CloidMarkets` after matching) and the serial duplicate pre-pass (claimed set plus
`CloidMarkets::live`), each ~0.3 ms CPU per 1k fills; in the workers it is the per-book check, `index_cloid` and
`forget_cloid`. The index upkeep is cache-bound (IPC 0.54, most of the extra DRAM fills). Both exec-thread parts are
untimed by the phase timers, which is why section 59 saw only `engine` and `residual_untimed` move. Decisive for the
split (all exec sites are 0.00-0.02 in c0); n = 3 x 120 s for the whole-cell A/B, consistent with 59.

**Possible levers (suggestions only, not measured):** (1) pre-size the claimed set (`HashSet::with_capacity` from the
number of cloid placements in the batch) to remove the ~0.06 of `reserve_rehash`; (2) a smaller key or a cheaper hash
for the cloid maps (for example a 64-bit digest of `(Address, u128)`, or a hash over a prefix, with the full key kept
for the compare), since foldhash's long path and the key compare are ~0.3 of the exec delta; (3) batch the absorb /
drain work, or let the workers build each book's index delta so the exec thread only merges it, and (4) skip the
pre-pass's cross-market lookup for a sender with no live cloids. Each needs its own A/B.

**Caveats.** (1) 100% share is the worst case (as in 59); the split at a realistic share is not measured. (2) One node
(val1), user space only: kernel time (syscalls, page faults) is not attributed. (3) W1 and W2 are 45 s and 20 s windows
inside minutes 0-2. (4) Per-1k-fills normalisation spreads every cost over fewer fills in c100 (W1 fills/s -10.7%),
which inflates costs that do not scale with fills; the cloid sites are 0 in c0, so their deltas are not affected. (5)
glibc only. (6) Per-site figures are arm means of 3 cells; per-cell variation of the split is not shown (in
`prof-cells.json`; 60.6 adds it).

Analysis (`tools/ozarchy-p3cloidprof-tools/analyst/`): `run-analysis.sh` (perf script / perf report per cell, then the
scripts below), `a-prof.py` (W1 / W2 attribution, cloid sites, thread groups; writes `frames-self.txt`,
`frames-self-loc.txt`, `frames-children.txt`, `cloid-sites.txt`, `llc.txt`, `windows.txt`, `prof-cells.json` and the
folded stacks), `fg.py` (flame graph SVG from the folded stacks), `a-metrics.py` (engine phase timers, `metrics.txt`),
and the p3cloid analyst scripts `a-cells.py`, `a-arms.py`, `a-paired.py` (`cells.txt`, `arms.txt`, `paired.txt`;
`run-analysis.sh` writes their stdout) plus `a-archive.py`. The archived `run-analysis.sh` differs from the analyst's
copy on ozarchy in one place: it passes the cell prefix to `a-cells.py` and `a-paired.py` with its trailing `-`
(`"$P-"`), as those scripts build cell paths as prefix + tag; the copy on ozarchy passes `"$P"`, which would open
`ozarchy-p3cloidprof-300mw0/`. With the fix, `a-paired.py` reproduces `paired.txt` byte for byte and `a-cells.py`
reproduces the w0 entry of `cells.json`, so the outputs were made with the hyphenated prefix. `frames-engine-delta.txt`
was printed in the analyst session without a saved script: it is the exec + workers sum of `frames-self.txt` per frame
(checked for the top rows). The `reserve_rehash` and lock figures in 60.2 and the Keccak / zstd attribution in 60.5 were
computed from `folded-*.txt` for this write-up, and the per-site IPC in 60.3 from the `llc.txt` counts. Flame graphs
(FlameGraph-style, us CPU per 1k fills, arm mean, cloid frames in blue):
`ozarchy-p3cloidprof-run/analyst/flamegraph-c0.svg` and `flamegraph-c100.svg` (0.5 MB each) on ozarchy, not copied into
the repo (no earlier campaign committed an SVG). Not copied either: the folded stacks (41-42 MB each) and the per-cell
`perf-report-*.txt` (118-146 KB each, regenerable with `run-analysis.sh`). Driver
`tools/ozarchy-p3cloidprof-tools/ozarchy-p3cloidprof-campaign.sh`, sidecar `cloidprof-sidecar.sh`, md5s `md5s-all.txt`
(the p3cloid build's).

### 60.6 Addendum: per-cell noise and Keccak callers (re-analysis of the existing perf.data, 2026-10-13, no new run)

18c asked two follow-ups after 60: how much do the per-site figures vary across the 3 cells per arm (60's caveat 6), and
who calls Keccak? Nothing was rerun. The inputs are the same W1 data, read-only: `prof-cells.json` (from `a-prof.py`),
the cells' `prof.script.gz`, `exec-tid.txt` and `perf.data`. For each frame the table gives the arm mean ± SD of the 3
per-cell values and the paired c100 - c0 delta (pairs r1-r3) with a 95% CI t(2) (t = 4.303, n = 3 pairs). Resolved
means the CI excludes 0. Units are as in 60.2: W1, val1, ms CPU per 1k val1 fills. Per-cell values are in
`noise-frames.txt`.

**Every cloid-site delta is resolved with tight intervals (c0 ~0, c100 SD <= 0.02). The workers' group total (+0.29),
their hashbrown probe and their key compare are within noise. The exec thread total is barely resolved. Keccak callers
can only be split by thread: 98% of Keccak samples have no caller. Gossip verify (+0.30, resolved) and RPC ingress
verify (+0.17, not resolved) are the largest parts. In cores, total Keccak rises only +0.072 and both verify pools are
flat, so most of the per-fill rise comes from the denominator (W1 fills/s -10.7% at c100).**

| frame (thread group, self / inclusive / site as in 60.2) | c0 mean ± SD | c100 mean ± SD | c100 - c0 [95% CI t(2)] | resolved |
|---|---|---|---|---|
| exec thread total | 4.442 ± 0.193 | 5.155 ± 0.209 | +0.713 [+0.040, +1.387] | yes (barely) |
| market workers total | 5.825 ± 0.129 | 6.110 ± 0.260 | +0.286 [-0.221, +0.792] | **no** |
| other threads total | 14.248 ± 0.510 | 17.438 ± 0.757 | +3.190 [+0.589, +5.792] | yes |
| whole process | 24.514 ± 0.795 | 28.704 ± 1.221 | +4.189 [+0.440, +7.938] | yes |
| exec site `OrderBook::drain_cloid_log` | 0.000 ± 0.000 | 0.323 ± 0.019 | +0.323 [+0.275, +0.371] | yes |
| exec incl `CloidMarkets::absorb` | 0.000 ± 0.000 | 0.324 ± 0.020 | +0.324 [+0.275, +0.373] | yes |
| exec site `CloidMarkets::absorb` excl. drain | 0.000 ± 0.000 | 0.001 ± 0.000 | +0.001 [+0.000, +0.002] | yes |
| exec incl `CloidMarkets::live` | 0.000 ± 0.000 | 0.133 ± 0.008 | +0.133 [+0.112, +0.154] | yes |
| exec site `duplicate_cloid` | 0.000 ± 0.000 | 0.136 ± 0.007 | +0.136 [+0.117, +0.154] | yes |
| exec site pre-pass serial block (claimed set, `retain`) | 0.015 ± 0.001 | 0.155 ± 0.008 | +0.140 [+0.117, +0.163] | yes |
| exec incl claimed-set `reserve_rehash` | 0.000 ± 0.000 | 0.060 ± 0.001 | +0.060 [+0.058, +0.061] | yes |
| exec site `forget_cloid` | 0.000 ± 0.000 | 0.063 ± 0.002 | +0.063 [+0.058, +0.068] | yes |
| workers site `forget_cloid` | 0.000 ± 0.000 | 0.080 ± 0.008 | +0.080 [+0.059, +0.100] | yes |
| workers site per-book `contains_key` (`order_book.rs:1397-1402`) | 0.002 ± 0.001 | 0.083 ± 0.004 | +0.081 [+0.072, +0.089] | yes |
| workers site `index_cloid` | 0.000 ± 0.000 | 0.038 ± 0.003 | +0.038 [+0.030, +0.046] | yes |
| **exec + workers, all cloid sites** | 0.018 ± 0.002 | 0.881 ± 0.043 | **+0.863 [+0.753, +0.973]** | yes |
| exec incl hashbrown `find_inner` | 0.522 ± 0.011 | 0.841 ± 0.015 | +0.319 [+0.307, +0.330] | yes |
| workers incl hashbrown `find_inner` | 0.562 ± 0.004 | 0.666 ± 0.030 | +0.104 [+0.031, +0.178] | yes |
| exec incl `make_hash` | 0.283 ± 0.019 | 0.474 ± 0.025 | +0.191 [+0.090, +0.292] | yes |
| workers incl `make_hash` | 0.522 ± 0.009 | 0.581 ± 0.017 | +0.059 [+0.039, +0.079] | yes |
| exec self `_mm_movemask_epi8` (probe) | 0.121 ± 0.009 | 0.310 ± 0.015 | +0.189 [+0.172, +0.206] | yes |
| workers self `_mm_movemask_epi8` (probe) | 0.195 ± 0.009 | 0.229 ± 0.008 | +0.035 [-0.006, +0.076] | **no** |
| exec self `spec_eq` (key compare) | 0.216 ± 0.007 | 0.357 ± 0.013 | +0.141 [+0.117, +0.165] | yes |
| workers self `spec_eq` (key compare) | 0.248 ± 0.005 | 0.281 ± 0.022 | +0.033 [-0.010, +0.076] | **no** |
| exec self foldhash (all foldhash leaves) | 0.239 ± 0.019 | 0.395 ± 0.020 | +0.156 [+0.066, +0.246] | yes |
| workers self foldhash (all foldhash leaves) | 0.433 ± 0.009 | 0.466 ± 0.011 | +0.033 [+0.018, +0.049] | yes |
| exec self `foldhash::hash_bytes_long` | 0.086 ± 0.005 | 0.166 ± 0.010 | +0.080 [+0.045, +0.116] | yes |
| exec self Keccak (F1600, absorb / squeeze, wrappers) | 0.343 ± 0.004 | 0.466 ± 0.005 | +0.123 [+0.099, +0.147] | yes |
| other threads self Keccak (same) | 3.961 ± 0.154 | 4.788 ± 0.199 | +0.827 [+0.130, +1.524] | yes |
| all threads self `__KeccakF1600` only | 4.138 ± 0.142 | 5.050 ± 0.199 | +0.911 [+0.240, +1.583] | yes |
| other threads incl `verify_one_action_with` | 2.588 ± 0.113 | 3.391 ± 0.288 | +0.803 [-0.008, +1.613] | **no** |

**Keccak callers.** The call graphs are frame-pointer only (`--call-graph fp`). In each cell 97.8-98.1% of Keccak leaf
samples are a bare leaf with no user frame below it, because the assembly `__KeccakF1600` breaks the chain. (60.5's ~99%
counted only the threads outside the exec thread, from `folded-*.txt`.) The other ~2% are wrapper and absorb leaves
(`KECCAK_ASM_SHA3_absorb`, `keccak256_impl`) whose chains are intact. `a-keccak-comm.py` therefore splits Keccak self
time (F1600, absorb / squeeze and wrappers; total +0.947) by thread: comm, plus the exec tid from `exec-tid.txt`. Then
`kc-raw.sh` / `a-kc.py` list the caller and the caller's caller of the intact chains for each thread
(`keccak-intact-chain-callers.txt`). The function names in the second column come from that 2%. Assigning a thread's
bare samples to those callers is an inference: plausible for the verify threads, which do little else, and not shown
for the others.

| thread | intact chains name (caller <- its caller) | Keccak ms per 1k fills, c100 - c0 [95% CI] | Keccak cores, c100 - c0 [95% CI] |
|---|---|---|---|
| `torus-gossip-verify` | `eip712_struct_hash` <- `SignedNativeAction::recover_sender` (57-60% of all intact chains); also the domain separator, `verified_cache_key` | **+0.302 [+0.053, +0.550]** | +0.007 [-0.027, +0.040], flat |
| `torus-ingress-verify` (RPC submit) | `eip712_struct_hash` <- `SignedNativeAction::validate_with_sessions` (32-34%), under `verify_one_action_with` | +0.172 [-0.153, +0.496], n.s. | +0.005 [-0.017, +0.028], flat |
| `hotstuff-algo` (consensus) | `compute_action_hash_with_scratch` <- `TorusApp::dispatch_to_exec` | **+0.157 [+0.065, +0.248]** | **+0.018 [+0.011, +0.026]** |
| exec thread | `sort_native_actions_indexed` <- `execute_committed_block_with`; also `verified_cache_key_with_scratch` | **+0.124 [+0.101, +0.146]** | +0.014 [-0.000, +0.028], n.s. |
| `rpc-worker` | `verified_cache_key` <- `add_native_action_presigned`; `compute_action_hash` <- `NativePool::insert_with_restash_key` / `NativeDaStore::put_batch` | **+0.128 [+0.048, +0.209]** | **+0.016 [+0.012, +0.021]** |
| `torus-node` (pre-proposal broadcast) | `compute_action_hash_with_scratch` <- `LibP2PNetwork::broadcast_pre_proposal_actions`; c100 only (c0 0.000) | **+0.065 [+0.059, +0.072]** | **+0.011 [+0.011, +0.011]** |
| total | | **+0.947 [+0.260, +1.634]** | **+0.072 [+0.047, +0.097]** |

Cores = ms per 1k fills x W1 fills/s / 1e6. Per second, both verify pools spend the same Keccak time in both arms. Their
per-fill rise is what a cost that does not grow with load shows when fills/s fall 10.7% (gossip verify c0 2.191 x 0.12
= +0.26 of its +0.30). The resolved per-second rises are consensus action hashing, `rpc-worker` and the pre-proposal
broadcast (+0.045 cores together), plus the exec thread (+0.014, n.s.). As whole threads (all samples, cores): gossip
verify -0.010 [-0.046, +0.027] (flat), ingress verify +0.085 [+0.006, +0.165] (resolved; not Keccak, which is flat
there). The `verify_one_action_with` stacks are all on the ingress thread. In ms per 1k fills they are +0.802
[-0.007, +1.611], not resolved (`keccak-by-thread.txt`).

**What changes against 60's text.**

- The cloid-site split (60.2, 60.4) stands: every site delta is resolved. So are the all-sites total (+0.863
  [+0.753, +0.973]) and the exec hashing figures (`find_inner`, `make_hash`, probe, key compare, foldhash).
- The worker-side aggregates are within noise: market workers +0.29 [-0.22, +0.79] (60's "+0.29 (+5%)"), the workers'
  probe +0.035 and key compare +0.033. The three worker cloid sites (`forget_cloid`, per-book `contains_key`,
  `index_cloid`) are resolved one by one, and so are the workers' `find_inner`, `make_hash` and foldhash. The exec thread
  total +0.71 is barely resolved ([+0.04, +1.39]).
- 60's separate `verify_one_action_with` +0.80 (60.5, inclusive, excluding the bare Keccak) is not resolved per cell
  ([-0.01, +1.61]). So 60.5's "verify (+31%) ... rise more than [the normalisation]" is not shown at n = 3.
- 60 never assigned Keccak to RPC verify: 60.5 says its callers are not known from this profile, and that still holds
  for the bare 98%. This addendum only adds the split by thread and the callers named by the intact 2%, as in the table
  above. If the callers matter, a rerun with `--call-graph dwarf` or LBR call stacks (`--call-graph lbr`) could recover
  them (suggestion only, not run).

Files: `ozarchy-p3cloidprof-run/analyst/noise/` on ozarchy, archived in `tools/ozarchy-p3cloidprof-tools/analyst/noise/`:
`a-noise.py` (-> `noise-frames.txt`, reads `prof-cells.json`), `a-keccak-comm.py` (-> `keccak-by-thread.txt`, reads
`prof-cells.json` and the cells' `prof.script.gz` / `exec-tid.txt`), `kc-raw.sh` (-> `keccak-intact-chain-callers.raw`)
and `a-kc.py` (-> `keccak-intact-chain-callers.txt`), and `perf-report-comm-sym.sh` (per-cell `perf report --sort
comm,sym`, `perf-report-comm-sym-<cell>.txt`, 50-66 KB each). The analyst ran the commands in `kc-raw.sh` and
`perf-report-comm-sym.sh` inline. They are saved as scripts for the archive with the commands unchanged and only the
paths parameterised. The perf reports are not in the repo: their header line carries several KB of trailing blanks, and
the script regenerates them. Reproduction check (rerun with `nice -n 19` into a temp dir, then diffed):
`noise-frames.txt`, `keccak-by-thread.txt` and `keccak-intact-chain-callers.txt` are byte-identical.
`keccak-intact-chain-callers.raw` is identical after `sort`, because awk's output order is unspecified. The 6 perf
reports differ only in perf's random "Tip" footer line.

## 61. Final Phase 3 gate: step 2 `c88d3073` vs main `de1a97c1`, both jemalloc compiled in, SAT n = 4 + 600 s soak + SIGTERM restart per arm, commit-tail mode (campaign ozarchy-p3gate, 2026-10-13)

**5 of 6 measured gates pass (restart on SIGTERM only, see below); the RAM size gate fails in the soak.** Phase 3 (s2, 15 s) vs its own main parent, both
jemalloc compiled in, full workload:

| gate (section 1 of `item6-phase3-impl.md`) | target | measured | result |
|---|---|---|---|
| CPU | no CPU-s/1M regression beyond noise | SAT **-9.72% [-10.46, -8.98]** (26.51 -> 23.94); equal fills -8.39% [-9.26, -7.51]; soak -12.4% (one cell per arm) | **PASS** |
| disk | state-row bytes written down | device writes SAT **-54.8% [-56.9, -52.7]** (159.0 -> 71.9 MB/s per validator); soak -64.2% (231.6 -> 82.9) | **PASS** |
| throughput | matched/s up beyond noise; native blocks/s no regression | SAT matched/s **+8.8% [+5.2, +12.4]**, native blocks/s **+20.1% [+10.3, +30.0]**; soak +13.3% / +13.6% | **PASS** |
| restart | restart -> ready <= 60 s (plan: SIGKILL val1, multi-crash cells) | SIGTERM val1 only: kill -> first chain commit **s2 35.0 s** (19.1 s shutdown + 16.0 s up), m 37.3 s; replay 0 in both; one kill per arm. No SIGKILL / multi-crash cell in this campaign | **PASS for SIGTERM**; SIGKILL not re-measured on `c88d3073` |
| RAM, levels off | RSS levelling off in a 10 min soak | s2 last-5-min RSS slopes -77 / -66 / -10 MB/min per node; s2 - m difference slopes -53 / -44 / -34 MB/min, all CIs include 0 | **PASS** |
| RAM, size | Phase 3 delta <= ~1 GB per node | soak **+1.43 GB** per node (minutes 5-10 mean; +1.31 at 10 min, **+1.80 peak**, val0 peak +2.04); SAT (minutes 0-2) mean +0.99 [+0.67, +1.30], peak +1.28 [+0.82, +1.74] | **FAIL** (soak mean and peak) |

The correctness gate (write sets, `h_n`, CF dumps) is the step 1 test suite, not part of this campaign. The plan's restart
gate names SIGKILL multi-crash cells; the last such measurement is section 46 (step 1 `a44bdde6`: SIGKILL kill -> first
commit 14.4 s mean / 23.2 s max, replay from the marker correct), not this head, so the restart PASS here covers SIGTERM only. **What the extra RAM
is:** of the +1.43 GB soak delta, live allocations (`torus_jemalloc_allocated_bytes`) are +0.50 / +0.54 / +0.56 GB per node,
the same as section 57's je vs je soak (+0.51 to +0.57). The rest, +0.67 to +1.18 GB per node, is jemalloc-resident pages not
in live use (resident - allocated; section 57: +0.40 to +0.49), and non-heap RSS is equal (-0.11 to +0.01 GB). The live
heap does not grow (s2 - m allocated difference slopes +30 to +42 MB/min, CIs include 0), so the delta over section 57's
+0.94 GB is allocator retention, not a leak. Caveats: one soak cell per arm (no CI on the soak numbers), and s2 ran +13%
matched/s in the soak (more work per minute, so more allocator churn). Block time p99 (commit interval) is higher on s2:
+17.7% [+6.7, +28.7] SAT and +23% in the soak (not a gate; 61.2). **The decision is the owner's (pending).**

The FINAL Phase 3 gate run (18c, 2026-10-13). Arms: **s2** = `feat/item6-phase3-step2` `c88d3073` (step 1 + step 2 +
merge-wait `91a10875`, merged with main `de1a97c1`; node `4e0d8b84`, bench `ae96e0c1`) with
`TORUS_CHECKPOINT_INTERVAL_MS=15000`. **m** = main `de1a97c1` (`c88d3073`'s second parent; node `f1081574`, bench
`42365459`), knob unset. Each arm's node and bench were built from its own commit into a fresh target dir
(`ozarchy-p3gate-build.sh`; cloid changed the signed-order format, so a bench only matches its own node). Both binaries
have jemalloc compiled in (default feature; preflight nm count 1,168 jemalloc symbols in each node, no LD_PRELOAD, no
`libjemalloc.so` mapped). CLOID_SHARE=0 (no cloids), default (full) workload with the cancel-share fix in both (it is on
main), tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, RATE 76000 (saturated), commit tail,
shared NVMe, btrfs data dirs. Harness = the `c88d3073` worktree (its `summarize.py` reports `checkpoint_deferred_join`),
tools = c88d3073's `tools/matched-bench` with p3term's `crash-kill.sh` (SIGTERM, 0.1 s exit polling), genesis state hash off
(`TORUS_STATE_HASH_FAILSTOP` unset). Driver `ozarchy-p3gate-campaign.sh` CAMP=GATE (copy of `ozarchy-p3cloid-campaign.sh`
plus the restart cells of `ozarchy-p3term-campaign.sh`). Smoke 15:58-16:08: s0-s2-TERM (90 s, SIGTERM val1 at 40 s) and
s0-m (60 s). Cells 16:10-17:51: w0 (s2, 60 s, excluded); SAT r1-r4 ABBA (r1-s2 r1-m r2-m r2-s2 r3-s2 r3-m r4-m r4-s2, 120 s
each); soak k1-m, k1-s2 (600 s each, fixed); restart t1-s2-TERM, t1-m-TERM (300 s, one SIGTERM of val1 at bench + 180 s,
KILL_WAIT 75 s, the bench reads blocks from val2). 225 CHECK OK (+36 in the smoke), no CHECK FAIL, no memguard stop; the
host was quiet before every cell (no cargo / rustc / rust-analyzer, load 1.72-1.96). Enable line once per validator in
every s2 cell (twice on the restarted val1), 16 checkpoints per node per SAT cell, 54 per node in the soak.

**Validity.** w0, all 8 SAT cells, both soak cells and smoke s0-m: rc 0, ACCEPT, AGREE (12 cells; `validity.txt` says
"11" but lists the same 12). Three rc 2 cells are known artifacts, used for their measurements: t1-s2-TERM and smoke
s0-s2-TERM are REJECT AGREE with one reason, the crash gate's restart-tail scan matching the INFO config echo `running
state hash fail-stop (TORUS_STATE_HASH_FAILSTOP) on=false` of the restarted val1 (no WARN / ERROR panic or fail-stop line,
hardscan clean, digests and block hash equal on 3 nodes). t1-m-TERM is REJECT DISAGREE: the same INFO-line false FAIL plus
the mark-only digest artifact (val0's digest took 64.2 s vs 21.5 / 23.4 s; 50 lines differ, fields `accountValue`,
`freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed`, `withdrawable` only; block hash equal at height 1,692). No hard
finding.

### 61.1 SAT, n = 4 ABBA, 120 s (paired s2 - m, 95% CI t(3); `paired.txt`, `arms.txt`)

| metric | m | s2 | delta | % [95% CI] |
|---|---|---|---|---|
| matched/s | 198,189 | 215,590 | +17,401 | **+8.78% [+5.17, +12.39]** |
| submits/s (closed loop) | 1,252 | 1,339 | +87 | +6.93% [+4.40, +9.45] |
| native blocks/s | 3.82 | 4.58 | +0.77 | **+20.11% [+10.28, +29.95]** |
| node CPU-s/1M fills | 26.51 | 23.94 | -2.58 | **-9.72% [-10.46, -8.98]**; equal fills (ANCOVA, `arms.txt`) -8.39% [-9.26, -7.51] |
| device writes MB/s per validator | 159.0 | 71.9 | -87.1 | **-54.80% [-56.87, -52.73]** |
| engine ms per 1k fills | 3.84 | 3.44 | -0.40 | -10.35% [-12.22, -8.49] |
| exec `block_ms` (mean of nodes) | 234.4 | 200.6 | -33.8 | -14.43% [-20.61, -8.25] |
| exec per native block p50 / p99 ms | 354 / 621 | 307 / 590 | -47 / -30 | -13.38% [-15.47, -11.29] / -4.90% [-12.23, +2.42] |
| block time (commit interval) p50 / p99 ms | 112 / 1,138 | 76 / 1,340 | -37 / +202 | -32.72% [-55.28, -10.17] / **+17.74% [+6.74, +28.74]** |
| fills / actions per native block | 49,134 / 262 | 44,895 / 241 | | -8.63% [-14.64, -2.61] / -7.94% [-14.12, -1.76] |
| RSS mean / peak GB per node (cpu.csv) | 3.69 / 4.63 | 4.68 / 5.91 | +0.99 / +1.28 | +0.99 GB [+0.67, +1.30] / +1.28 GB [+0.82, +1.74] |
| RSS HWM / anon GB | 4.80 / 4.02 | 6.25 / 4.98 | +1.45 / +0.96 | +1.45 [+0.97, +1.94] / +0.96 [+0.62, +1.30] |
| RSS growth GB/min (whole cell) | 0.68 | 1.01 | +0.33 | +0.33 [+0.18, +0.49] |
| RSS first 15 s / last 15 s GB | 2.95 / 4.51 | 3.15 / 5.50 | +0.21 / +0.99 | +0.21 [-0.04, +0.45] / +0.99 [+0.59, +1.39] |
| RocksDB memtables MB | 286 | 264 | -22 | -7.69% [-24.07, +8.68] |
| exec lag / backlog (blocks) | 13.3 / 53.5 | 11.0 / 53.5 | | -17.35% [-25.99, -8.72] / tie |
| exec busy | 0.968 | 0.963 | | tie |

Checkpoint timers (`ckpt-timers.txt`, s2 only; main exports none): E's `checkpoint_settle_wait` 3.48 ms per native block on
the summary phase basis (3.07-4.20 per cell), **3.14 ms per native block on the metrics basis** (`torus_exec_native_blocks_total`;
p50 / p99 per observation 0.28 / 21.0 ms, cell means); upkeep 0.84 ms per block; **deferred join p50 0.0 ms, p99 0.0-3.2 ms per
node** (SAT mean 0.66; from the new `summary.json` fields `phase_by_node.val*.checkpoint_deferred_join_ms_p50` / `_p99`,
written by the `c88d3073` harness's `summarize.py`, s2 only); `ck_wait` 0.39 ms (one cell 1.55). The exec settle phase (`torus_exec_phase_settle_seconds`) is
**49.0 ms per native block on s2 vs 57.1 on m**. Layer 153 MB per checkpoint (SAT). This is section 54's merge-wait result
(settle wait 23.4 -> 3.1 ms) on the merged head.

### 61.2 Soak, 600 s per arm, one cell each (`arms-soak.txt`, `series.txt`)

![RSS and jemalloc allocated per node: s2 vs m](ozarchy-p3gate-figs/fig-rss.png)

*Figure 61-1: VmRSS (solid s2, dashed m) and `torus_jemalloc_allocated_bytes` (thin dashed) per node, 5 s samples. s2
green, m grey.*

| cell | node | mean min 2-5 MB | mean min 5-10 MB | change | end MB | peak MB | slope last 5 min MB/min (95% CI) | je allocated MB | je resident MB | je retained MB | exec queue (min 5-10) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| m | val0 | 4,377 | 4,410 | +33 | 4,530 | 4,606 | -20.3 [-45.9, +5.3] | 2,986 | 3,919 | 3,631 | 60.9 |
| m | val1 | 4,319 | 4,334 | +15 | 4,347 | 4,544 | -13.8 [-34.0, +6.4] | 2,963 | 3,888 | 3,441 | 59.7 |
| m | val2 | 5,239 | 5,280 | +41 | 5,325 | 5,584 | **+28.0 [+1.0, +55.0]** | 3,070 | 4,808 | 5,132 | 65.2 |
| s2 | val0 | 6,184 | 6,022 | -162 | 5,875 | 6,692 | **-76.8 [-137.6, -15.9]** | 3,497 | 5,642 | 5,168 | 55.4 |
| s2 | val1 | 5,728 | 5,563 | -165 | 5,244 | 6,230 | **-65.9 [-120.3, -11.5]** | 3,517 | 5,131 | 6,501 | 60.7 |
| s2 | val2 | 6,753 | 6,844 | +91 | 7,123 | 7,341 | -10.2 [-59.8, +39.5] | 3,645 | 6,364 | 7,948 | 65.3 |

jemalloc columns: minutes 5-10 mean of the 5 s gauges; `retained` is address space kept mapped but returned to the OS (not
in RSS). Deltas per node (GB: mean min 5-10 / at 10 min / peak) and the OLS slope of the difference series over the last
5 min (MB/min, 95% CI, AR(1) effective n):

| delta s2 - m | val0 | val1 | val2 | mean of 3 |
|---|---|---|---|---|
| **RSS** | +1.57 / +1.31 / +2.04; -52.6 [-119.2, +13.9] | +1.20 / +0.88 / +1.65; -43.9 [-106.2, +18.5] | +1.53 / +1.76 / +1.72; -33.5 [-94.5, +27.6] | **+1.43 / +1.31 / +1.80** |
| je allocated (live) | +0.50 / +0.44 / +1.09; +42.1 [-35.2, +119.3] | +0.54 / +0.66 / +0.80; +32.0 [-15.8, +79.9] | +0.56 / +1.07 / +0.91; +30.1 [-34.6, +94.9] | +0.53 / +0.72 / +0.93 ¹ |
| je resident | +1.68 / +1.73 / +2.30; -55.3 [-125.8, +15.2] | +1.21 / +1.42 / +1.74; -30.8 [-93.2, +31.7] | +1.52 / +1.74 / +1.89; -42.8 [-116.7, +31.1] | +1.47 / +1.63 / +1.98 ¹ |
| je resident - allocated (min 5-10) | +1.18 | +0.67 | +0.96 | +0.94 ¹ |
| je retained (min 5-10, not in RSS) | +1.50 | +2.99 | +2.75 | |
| RSS - je resident (non-heap) | -0.11 | -0.01 | +0.01 | |

¹ Means of the printed per-node values, and resident - allocated as the difference of the printed rows, computed for this
write-up (`series.txt` prints only the RSS mean of 3).

| metric (whole 600 s cell) | m | s2 | s2 vs m |
|---|---|---|---|
| matched/s | 193,398 | 219,139 | +13.3% |
| matched/s minutes 0-2 / 2-6 / 6-10 (val0 counter) | 201,679 / 191,823 / 190,516 | 210,202 / 223,560 / 218,481 | 6-10 vs 0-2: m -5.5%, s2 +3.9% |
| native blocks/s | 2.61 | 2.97 | +13.6% |
| node CPU-s/1M fills | 27.93 | 24.46 | **-12.4%** |
| device writes MB/s per validator | 231.6 | 82.9 | **-64.2%** |
| engine ms per 1k fills | 3.97 | 3.44 | -13.4% |
| exec `block_ms` / per native block p99 ms | 364.6 / 614 | 327.4 / 570 | -10.2% / -7.2% |
| block time p50 / p99 ms | 275 / 1,196 | 237 / 1,473 | -13.6% / **+23.1%** |
| fills / actions per native block | 72,090 / 361 | 72,146 / 361 | tie |
| settle wait ms per native block (summary phase / metrics) | n/a | 9.04 / 8.8 (p50 / p99 per observation 8.3 / 30.5 ms) | |
| exec settle phase ms per native block | 91.7 | 82.7 | -9.8% |

![per node per minute: expiry, cancel-alls, margin rejections, matched/s, book, exec queue / busy / ms per block, je overhead, layer, flush queue](ozarchy-p3gate-figs/fig-node.png)

*Figure 61-2: per node (columns), per minute: nonce-expired evictions, cancel-alls executed/s, margin-rejected orders/s,
matched/s, resting orders, exec queue depth, exec busy, exec ms per block, jemalloc resident - allocated, layer MB per
checkpoint and flush-worker depth. s2 green, m grey dashed.*

* **Full workload in both arms.** 0 nonce-expired evictions, 0 margin rejections, 0 action failures on every node, 0 bench
  in-flight timeouts; cancel-alls sent / executed m 211,145 / 211,136, s2 236,644 / 236,639. Resting orders 0.54-0.74 M per
  node at the end in both. Exec busy 0.99-1.00 and the exec queue at its ~66 cap on both (minutes 5-10 mean 55-65 blocks), so
  the exec backlog is not a confound (as in section 57).
* **Layer and checkpoints (s2):** 49 checkpoints per node in the bench window, layer 199 MB mean per checkpoint (early ->
  late 5 min 183 -> 216 MB), `layer_shared` 0, drop_inline 1,800-1,922 per node; settle ms per block 7.1-7.6 early, 11.7-12.3
  late. Flush-worker depth 0.1-0.2 (s2) vs 0.3-0.6 (m).
* **Levels off.** s2's RSS falls over the last 5 min on val0 / val1 (CIs below 0) and is flat on val2; the s2 - m difference
  slopes are all negative with CIs that include 0. s2's RSS swings by several hundred MB around its level (the 15 s
  checkpoint rebuild and jemalloc decay; figure 61-1).
* **Against section 57** (je vs je, `e7be8460` vs `0360c840`, without merge-wait and the later main): the RSS delta is +1.43
  vs +0.94 GB per node, the live-heap delta is the same (+0.53 vs +0.54 GB mean), and the allocator-resident slack is about
  double (+0.94 vs ~+0.45 GB). matched/s minutes 6-10 vs 0-2 is close (s2 +3.9% vs jcs +4.4%; m -5.5% vs jmf -4.1%).

### 61.3 SIGTERM restart, one kill per arm (`a-term.txt`)

| arm | kill at height (exec queue) | SIGTERM -> exit s | drained / flushed s | replay | DB open s | up -> val1 first commit s | up -> chain commit s | **kill -> first chain commit s** |
|---|---|---|---|---|---|---|---|---|
| s2 | 1,057 (61 blocks) | 19.07 | 18.15 / 18.91 | 0 (A = C = 1,119) | 0.07 | 15.85 | 15.95 | **35.02** |
| m | 966 (64 blocks) | 21.54 | 20.98 / 21.33 | 0 (A = C = 1,030) | 0.05 | 15.75 | 15.76 | **37.30** |

Both shut down gracefully (not forced; `shutdown complete` logged), replay 0 blocks, digests and block hash agree (t1-m-TERM:
mark-only digest artifact, above). No SIGKILL kill was made, so the replay path after a hard crash is not exercised on
`c88d3073` here (section 46 is the last SIGKILL evidence, on step 1). The shutdown time is the exec-queue drain (61-64 queued blocks, ~0.3 s each, as in
section 52); the checkpoint / memtable flush adds 0.76 s on s2 vs 0.34 s on m. Both exits exceed Docker's 10 s default grace
(row 57's `stop_grace_period` >= 60 s applies to both arms). The survivors val0 / val2 each kicked one stale connection
after the restart (`torus_mesh_stale_conn_disconnects_total` 1, the section 58 fix, now on main); no chain stall. One kill
per arm, no CI.

### 61.4 Verdict

* **Gates: CPU, disk, throughput, restart (SIGTERM only; no SIGKILL multi-crash cells on this head) and RAM-levels-off pass; RAM size (<= ~1 GB per node) fails in the soak:** +1.43
  GB minutes 5-10 mean and +1.80 GB peak per node (val0 peak +2.04). In the 120 s SAT cells the mean is +0.99 [+0.67, +1.30]
  and the peak +1.28 [+0.82, +1.74].
* **RAM interpretation:** the live Phase 3 heap is +0.50-0.56 GB per node, unchanged from section 57, and it does not grow.
  The rest of the delta is jemalloc-resident pages not in live use (allocator retention), larger here than in 57. One soak
  cell per arm (no CI), and s2 ran +13% matched/s in the soak.
* **Other changes:** block time p99 +17.7% [+6.7, +28.7] SAT, +23% soak (p50 -33% / -14%); fills per native block -8.6%
  SAT (more, smaller blocks).
* **The merge decision is the owner's (pending).** Possible next steps, suggestions only (not measured): (1) judge the RAM
  gate on the live heap (`torus_jemalloc_allocated_bytes`) instead of RSS; (2) tune jemalloc decay (e.g. `dirty_decay_ms` /
  `muzzy_decay_ms`, or a background thread) and rerun the soak; (3) n = 2-3 soaks per arm for a CI on the soak delta.

Analysis (`tools/ozarchy-p3gate-tools/analyst/`): `a-cells.py` (p3cloid's, per-cell extraction -> `cells.json`, stdout
`cells.txt`), `a-arms.py` (`arms.txt` on the 8 SAT cells with `--io` for device writes; `arms-soak.txt` on the 2 soak
cells), `a-paired.py` (`paired.txt`), `a-series.py` (section 57's copy with the cells k1-m / k1-s2: `series.txt`,
`series.json`, figures SVG -> PNG via `rsvg-convert`), `a-term.py` (p3term's: `a-term.txt`) and `a-archive.py`.
`cells-abba.json` / `cells-soak.json` are the SAT / soak subsets of `cells.json`. `ckpt-timers.txt` and `validity.txt` have
no saved script: the first was computed in the analyst session from the cells' `summary.json` phases and metrics deltas,
the second written by the analyst from `cells.txt`, `a-term.txt` and the run logs. **Re-run check for this write-up:** the
archived scripts, run in a scratch dir against the cells, reproduce `cells.txt`, `cells.json` (all 12 cells), the
`cells-abba.json` / `cells-soak.json` subsets, `arms.txt`, `arms-soak.txt`, `paired.txt`, `series.txt`, `series.json`, both
SVGs and `a-term.txt` byte for byte. **The analyst's `a-term.py` (p3term's copy) has two hardcoded p3term paths:** it
writes its per-kill `kills.json` to `ozarchy-p3term-run/kills.json`, so the analyst's run (17:53) overwrote p3term's file
with p3gate's 2 kills (p3term's cells and its `a-term.py` are still on ozarchy, so it can be regenerated), and it reads
the `HARD FINDING` lines for `hard_findings` from `ozarchy-p3term-run/campaign.log`, so `a-term.txt`'s `hard_findings: []`
does not cover p3gate. Checked directly: p3gate's `campaign.log` and smoke `campaign.log` contain 0 `HARD FINDING`
lines (hardscan ran on all 13 cells and both smoke cells). The archived copy here writes
`ozarchy-p3gate-run/analyst/kills.json` and reads `ozarchy-p3gate-run/campaign.log` (its only two changes); it reproduces
`a-term.txt` byte for byte. Full outputs (incl. `series.json`, the SVGs) in
`ozarchy-p3gate-run/analyst/` on ozarchy. Driver `tools/ozarchy-p3gate-tools/ozarchy-p3gate-campaign.sh`, build script
`ozarchy-p3gate-build.sh`, `matched-bench/crash-kill.sh` (p3term's; the rest of the run's `matched-bench/` copy is
`c88d3073`'s `tools/matched-bench` byte for byte), md5s `md5s-all.txt`.

## 62. Phase 3 SIGKILL restart gate, step 2 `c88d3073` vs main `de1a97c1` (4 SIGKILLs per arm, state hash on), and a jemalloc decay soak pair on `c88d3073` (defaults vs `background_thread:true,dirty_decay_ms:1000`), commit-tail mode (campaign ozarchy-p3kd, 2026-10-13)

**The SIGKILL restart gate passes in both arms; the jemalloc decay setting removes most of the soak RSS gap from section 61.**
Requested by 18c after section 61. The owner (s118, via 18c) accepted the RAM gate on the live heap (section 61.4
suggestion 1, option a). Phase 3 merges once the SIGKILL restart gate passes on `c88d3073`. Section 61 measured restart
on SIGTERM only.

| gate (section 1 of `item6-phase3-impl.md`) | target | measured | result |
|---|---|---|---|
| restart | restart -> ready <= 60 s (SIGKILL val1, multi-crash cells) | SIGKILL val1 under load, 2 cells x 2 kills per arm (ABBA), state hash on. Kill -> val1's first commit after the restart: **s2 22.3 / 28.4 / 29.5 / 39.5 s (mean 29.9, max 39.5)**; m 32.0 / 25.3 / 21.8 / 29.5 s (mean 27.2, max 31.95). Replay from the durable marker + 1, contiguous, in all 8 kills | **PASS** (both arms) |
| RAM (owner: judged on the live heap) | Phase 3 delta <= ~1 GB per node, levels off | not re-measured as s2 vs m here. Section 61: live heap +0.50-0.56 GB per node, does not grow. Part B below: with the decay setting, s2's soak RSS equals section 61's main soak (-0.03 GiB per node, cross-session) | accepted by the owner (s118) |

**Part B (not a gate):** same s2 binary, 600 s soak, jemalloc defaults vs `MALLOC_CONF=background_thread:true,dirty_decay_ms:1000`.
RSS per node (minutes 5-10 mean) drops **6.25 -> 4.53 GiB (-1.72)** and the peak 6.91 -> 5.19 GiB. jemalloc resident -
allocated drops 2.35 -> 0.67 GiB. Live allocations are unchanged (3.46 -> 3.49 GiB). RSS levels off in both cells. Cost,
n = 1 per arm: node CPU-s/1M **+4.6%** (24.47 -> 25.60), matched/s -2.5%, native blocks/s +2.6%. The setting is an
environment variable, not compiled in (see 62.4).

**Setup.** Binaries are reused from the p3gate stage, not rebuilt: **s2** = `feat/item6-phase3-step2` `c88d3073` (node
`4e0d8b84`, bench `ae96e0c1`) with `TORUS_CHECKPOINT_INTERVAL_MS=15000`; **m** = main `de1a97c1` (node `f1081574`, bench
`42365459`), knob unset. Both have jemalloc compiled in (preflight nm: 1,168 jemalloc symbols in each node). The rest is
as section 61: CLOID_SHARE=0, full workload, tw, Classic, cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900,
saturated, commit tail, shared NVMe, btrfs data dirs. Harness = the `c88d3073` worktree. Tools = p3gate's
`matched-bench` copy (c88d3073's plus p3term's `crash-kill.sh`), byte for byte. Driver `ozarchy-p3kd-campaign.sh` CAMP=KD
(copy of `ozarchy-p3gate-campaign.sh`).
* **Part D (restart):** cells d1-s2-KILL, d1-m-KILL, d2-m-KILL, d2-s2-KILL (ABBA), 300 s each. SIGKILL of val1 at bench + 60
  / 180 s (d1) and + 65 / 187 s (d2), the first two offsets of section 46's two sets. KILL_WAIT 60 s. As section 46:
  `STATE_HASH_ACTIVATION=1`, 1 s checkpoint / head / state-hash gauge samples, and `torus_getStateHash` read at every
  retained 100-block checkpoint on each validator.
* **Part B (soak):** k1-s2def (s2, `MALLOC_CONF` unset = jemalloc defaults) then k1-s2dec (s2 + `MALLOC_CONF` on all 3
  nodes), 600 s each, state hash off, workload as p3gate's k1 cells.
* **jemalloc proof:** the binary is tikv-jemallocator built unprefixed (nm: `T malloc`, `V malloc_conf`), so it reads
  `MALLOC_CONF`, not `_RJEM_MALLOC_CONF`. Preflight: `MALLOC_CONF=...,stats_print:true torus-node --version` prints
  `opt.background_thread: true`, `opt.dirty_decay_ms: 1000` (`jeconf-proof.txt`); the defaults print `false` / 10000,
  `muzzy_decay_ms` 0, `narenas` 128 (`jeconf-default.txt`). Live check in each cell: `MALLOC_CONF` in each node's
  `/proc/<pid>/environ` (`node-preload.txt`), and 4 `jemalloc_bg_thd` threads per node in k1-s2dec vs 0 in every other
  cell (`node-jethreads.txt`).
* **Timing and checks:** smoke 18:56-19:05 (s0-m-KILL, 120 s, one SIGKILL at 40 s; s0-s2dec, 60 s). Cells 19:07-20:17: w0
  (s2, 60 s, excluded), then Part D, then Part B. 134 CHECK OK (+38 in the smoke), no CHECK FAIL, no memguard stop, 0 `HARD
  FINDING` lines. The host was quiet before every cell (no cargo / rustc / rust-analyzer, 1-min load 1.70-1.88).

**Validity.** w0 and k1-s2def: rc 0, ACCEPT, AGREE. The 4 kill cells are rc 2 REJECT, all from known artifacts; they are
used for their restart measurements.
* **Crash gate:** the restart-tail scan matches the INFO config echo `running state hash fail-stop
  (TORUS_STATE_HASH_FAILSTOP) on=false` once per restart (8 of 8 tails; no WARN / ERROR panic or fail-stop line). On s2 it
  also reports "rewind N blocks beyond the exec queue (max 2)". That is the Phase 3 design: replay from the last checkpoint
  (section 46.3).
* **Digests:** d1-m-KILL and d2-s2-KILL are DISAGREE, the mark-only digest artifact (sections 46.4, 61). 50 account lines
  differ in `accountValue`, `freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed` and `withdrawable` only. The odd
  node's digest was the fast one: val2 21.5 s vs 69.8 / 39.9 s; val1 18.3 s vs 69.1 / 70.5 s.
* **Dissemination:** d1-m-KILL also has "dissemination failures": val2 logged one exhausted body fetch, recovered by sync
  fallback.
* **k1-s2dec** is rc 2 REJECT DISAGREE on agreement only, the same mark-only artifact. val2's digest took 17.9 s vs 86.5 /
  87.0 s; the 5 mark fields differ on 50 lines; block hash equal; liveness PASS. Its soak measurements are used.

Smoke: s0-m-KILL rc 2 (the same INFO line); s0-s2dec ACCEPT AGREE with 4 bg threads per node.

### 62.1 SIGKILL restart, 4 kills per arm (`restart.txt`, `a-kill.txt`, `kills.json`)

| cell | kill | height at kill (exec queue) | down s | marker A ; committed C | replayed (C - A) | first replayed, contiguous | DB open s | replay s (blocks/s) | restart -> val1 first commit s | **kill -> val1 first commit s** | kill -> chain commit s | restart -> rejoin s |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| d1-s2 | k1 | 707 (65) | 0.35 | 700 ; 772 | 72 | 701, yes | 3.24 | 17.89 (4.03) | 21.93 | **22.28** | 21.85 | 21.48 |
| d1-s2 | k2 | 1,213 (61) | 0.33 | 1,200 ; 1,274 | 74 | 1,201, yes | 3.46 | 23.83 (3.10) | 28.05 | **28.38** | 28.03 | 27.55 |
| d1-m | k1 | 658 (65) | 0.23 | 658 ; 723 | 65 | 659, yes | 3.97 | 26.97 (2.41) | 31.72 | **31.95** | 31.54 | 31.35 |
| d1-m | k2 | 1,112 (61) | 0.35 | 1,112 ; 1,173 | 61 | 1,113, yes | 3.47 | 21.38 (2.85) | 24.99 | **25.34** | 25.60 | 25.77 |
| d2-m | k1 | 666 (64) | 0.26 | 666 ; 730 | 64 | 667, yes | 1.76 | 19.64 (3.26) | 21.52 | **21.78** | 22.02 | 22.36 |
| d2-m | k2 | 1,077 (61) | 0.33 | 1,077 ; 1,139 | 62 | 1,078, yes | 3.99 | 25.09 (2.47) | 29.21 | **29.54** | 0.34 ¹ | 29.85 |
| d2-s2 | k1 | 633 (65) | 0.33 | 600 ; 698 | 98 | 601, yes | 2.71 | 26.32 (3.72) | 29.15 | **29.48** | 29.72 | 30.12 |
| d2-s2 | k2 | 1,053 (60) | 0.36 | 1,000 ; 1,113 | 113 | 1,001, yes | 3.71 | 35.26 (3.20) | 39.11 | **39.47** | 39.96 | 39.17 |

¹ Artifact of the chain measure (`crash-freeze.py`: kill -> val0's first commit after the restart). val0 committed 0.01 s
after val1's restart. The gate uses val1's own first commit. On the chain measure (section 61's basis) s2 is 29.9 / 40.0 s
mean / max; m is 26.4 / 31.5 s without this kill.

| arm (n kills) | kill -> val1 first commit mean / max s | replayed blocks mean / max | replay blocks/s mean | DB open mean / max s | result |
|---|---|---|---|---|---|
| **s2** `c88d3073` (4) | **29.9 / 39.5** | 89 / 113 | 3.5 | 3.28 / 3.71 | **PASS** (<= 60 s) |
| **m** `de1a97c1` (4) | **27.2 / 31.95** | 63 / 65 | 2.7 | 3.30 / 3.99 | **PASS** |
| section 46 s (step 1 `a44bdde6`, 15 s), SIGKILL (8) | 14.0 / 22.8 | 92 / 162 | 13.1 | | PASS |
| section 46 o (`a44bdde6`, knob unset), SIGKILL (8) | 14.1 / 16.6 | 49 / 65 | 5.1 | | PASS |

The section 46 rows are recomputed by `a-tables.py` from p3r1's `kills.json` with the same definition (down + restart ->
val1's first commit). Section 46.3's table gives kill -> first chain commit: 14.4 / 23.2 s for s, 14.3 / 16.9 s for o.

* **Gate met in all 8 kills**, max 39.5 s (d2-s2 k2, the 113-block replay). s2 is 2.7 s slower than m on the mean, from
  longer replays (89 vs 63 blocks); n = 4 per arm, no CI.
* **What sets the replay** (as section 46.3). On m the marker A trails the committed height by the exec queue at the kill:
  61-65 blocks replayed, the queue was 61-65. On s2, A is the last 100-block checkpoint (700, 1,200, 600, 1,000), so the
  replay is the queue plus the blocks since that checkpoint (7 / 13 / 33 / 53 beyond the queue).
* **About 2x section 46's times, in both arms.** The replay runs at 2.4-4.0 blocks/s here vs 13.1 (s) / 5.1 (o) blocks/s
  in section 46. That is the workload. These cells run 221-246 transactions per native block (summary headline,
  whole cell) at 3.3-4.3 blocks/s; p3r1's SIGKILL cells ran 76-143 per block at 5.9-10.9 blocks/s. The replay rate is
  about one block per exec time per block in these cells (exec p50 309-350 ms, `cells.txt`). m is as slow (2.7 blocks/s),
  so this is not Phase 3.
* **Down** 0.23-0.36 s (patched `crash-kill.sh`, 0.1 s polling; section 46: 1.0 s with 1 s polling). **DB open** 1.8-4.0 s.
  **Rejoin** 21-39 s after the restart, about when val1 first commits.

### 62.2 Restart correctness: no finding (`a-kill.txt`)

* **Per kill (8 of 8):** A <= C. The replay starts at A + 1 and covers A+1..C exactly once (contiguous). A >= the durable
  state-hash height sampled in the last 1 s before the kill (`marker_lo`). No flag on any kill.
* **State-hash checkpoints:** `localHash` was read at every retained 100-block checkpoint on all 3 validators: 122
  heights in the 4 cells (34 / 33 / 25 / 30; up to height 3,400). 121 have all 3 validators; d2-m-KILL has one height
  with 2. **0 disagreements**, including the heights val1 re-executed after each restart.
* **End of cell:** block hash equal in all 4 cells. The digest DISAGREE in d1-m / d2-s2 is mark-only (above). No `HARD
  FINDING` line (hardscan ran on every cell; `a-kill.py` reads p3kd's own `campaign.log`).
* **Other:** d2-m-KILL has nonce-expired evictions 0 / 17 / 71 (val0 / val1 / val2); every other cell has 0. Three kill
  cells show 6-10 executed-action failures per node, equal on val0 / val2 (val1's counter restarts). The soak cells show
  0. Neither is analysed: the kill cells' throughput is not a measurement.

### 62.3 jemalloc decay soak pair, s2 `c88d3073`, 600 s, one cell each (`soak.txt`, `series.txt`)

![RSS and jemalloc allocated per node: defaults vs decay setting, and against p3gate's soaks](ozarchy-p3kd-figs/fig-rss.png)

*Figure 62-1: VmRSS (solid) and `torus_jemalloc_allocated_bytes` (dashed) per node, 5 s samples. Row 1: def (red) vs dec
(blue). Row 2: def vs p3gate's k1-s2 (gs2, green; same binary, cross-session). Row 3: dec vs p3gate's main k1-m (gm,
grey; cross-session).*

| cell | node | RSS mean min 5-10 MB | end MB | peak MB | RSS slope last 5 min MB/min (95% CI) | je allocated MB | je resident MB | je retained MB | exec queue (min 5-10) |
|---|---|---|---|---|---|---|---|---|---|
| def | val0 | 5,730 | 5,413 | 6,601 | -7.8 [-44.2, +28.6] | 3,501 | 5,270 | 4,823 | 56.6 |
| def | val1 | 6,497 | 6,331 | 6,910 | -11.9 [-55.2, +31.3] | 3,507 | 6,070 | 5,415 | 59.3 |
| def | val2 | 6,964 | 6,401 | 7,704 | +6.9 [-43.3, +57.0] | 3,629 | 6,529 | 5,673 | 65.4 |
| dec | val0 | 4,569 | 4,611 | 5,180 | +3.3 [-49.1, +55.8] | 3,598 | 4,195 | 6,882 | 65.3 |
| dec | val1 | 4,514 | 4,876 | 5,084 | +7.4 [-53.8, +68.6] | 3,499 | 4,131 | 6,690 | 58.1 |
| dec | val2 | 4,840 | 4,911 | 5,690 | -14.3 [-71.0, +42.4] | 3,625 | 4,468 | 7,496 | 63.5 |

jemalloc columns are the minutes 5-10 mean of the 5 s gauges. `retained` is address space kept mapped but returned to
the OS (not in RSS). Deltas below are from `series.txt` (GB there = MB / 1024 = GiB, as in section 61): mean min 5-10 /
at 10 min / peak per node, and the slope of the difference series over the last 5 min.

| delta | val0 | val1 | val2 | mean of 3 |
|---|---|---|---|---|
| **dec - def RSS** | -1.13 / -0.78 / -1.39; -1.9 [-73.9, +70.0] | -1.94 / -1.42 / -1.78; +7.6 [-82.6, +97.9] | -2.07 / -1.46 / -1.97; -34.7 [-132.1, +62.7] | **-1.72 / -1.22 / -1.71** |
| dec - def je allocated (min 5-10) | +0.10 | -0.01 | -0.00 | |
| dec - def je resident (min 5-10) | -1.05 | -1.89 | -2.01 | |
| dec - def je retained (min 5-10, not in RSS) | +2.01 | +1.24 | +1.78 | |
| dec - def non-heap (RSS - je resident) | -0.08 | -0.04 | -0.06 | |
| def - gs2 RSS (p3gate s2, cross-session) | -0.29 / -0.45 / -0.09 | +0.91 / +1.06 / +0.66 | +0.12 / -0.70 / +0.36 | +0.25 / -0.03 / +0.31 |
| dec - gm RSS (p3gate main, cross-session) | +0.15 / +0.08 / +0.56 | +0.18 / +0.52 / +0.53 | -0.43 / -0.40 / +0.10 | **-0.03 / +0.06 / +0.40** |
| dec - gm je allocated (min 5-10) | +0.60 | +0.52 | +0.54 | |

| metric (whole 600 s cell) | def | dec | dec vs def | gs2 (p3gate s2) | gm (p3gate m) |
|---|---|---|---|---|---|
| RSS mean min 5-10 / peak GiB per node (`soak.txt`) | 6.25 / 6.91 | 4.53 / 5.19 | -1.72 / -1.71 | 6.00 / 6.60 | 4.57 / 4.80 |
| je allocated / resident / resident - allocated GiB | 3.46 / 5.82 / 2.35 | 3.49 / 4.16 / 0.67 | | 3.47 / 5.58 / 2.11 | 2.94 / 4.11 / 1.17 |
| matched/s | 217,605 | 212,075 | -2.5% | 219,139 | 193,398 |
| matched/s minutes 0-2 / 2-6 / 6-10 (val0 counter) | 210,278 / 221,468 / 217,519 | 207,428 / 215,692 / 210,248 | | 210,202 / 223,560 / 218,481 | 201,679 / 191,823 / 190,516 |
| native blocks/s | 2.95 | 3.02 | +2.6% | 2.97 | 2.61 |
| node CPU-s/1M fills | 24.47 | 25.60 | **+4.6%** | 24.46 | 27.93 |
| block time p50 / p99 ms (mean of nodes, `cells.json`) | 239 / 1,398 | 230 / 1,425 | -3.8% / +1.9% | 237 / 1,473 | 275 / 1,196 |
| exec ms per block (minutes 5-10, per node) | 375-379 | 384-386 | +2.1% | 371-375 | 423-426 |
| settle wait ms per native block (summary phase) | 9.2 | 8.9 | | 9.0 | n/a |

* **Full workload in both cells.** 0 nonce-expired evictions, 0 margin rejections, 0 action failures, 0 bench in-flight
  timeouts. Cancel-alls sent / executed: def 234,445 / 234,442, dec 236,764 / 236,749. The exec queue is at its ~66 cap
  in both (minutes 5-10 mean 57-65), so the exec backlog is not a confound. 48-49 checkpoints per node, layer 196-203 MB
  mean per checkpoint.
* **What the setting does:** live allocations are unchanged (+0.10 / -0.01 / -0.00 GiB per node). jemalloc returns dirty
  pages within ~1 s from its background threads instead of keeping them for 10 s. Resident drops by 1.05-2.01 GiB per
  node and retained (mapped, not in RSS) rises by about as much. Non-heap RSS is equal.
* **Levels off** in both cells: every per-node last-5-min slope is within +-15 MB/min, with CIs spanning 0.
  The dec - def difference slopes all include 0.
* **Cost (n = 1 per arm, no CI):** node CPU-s/1M +4.6%, matched/s -2.5% (minutes 6-10 -3.3%), native blocks/s +2.6%,
  exec ms per block +2.1%. For scale, the default cell repeated p3gate's s2 soak (same binary, other session) to within
  +0.1% CPU-s/1M and -0.7% matched/s. Its RSS was +0.25 GiB per node higher on the mean (val1 +0.91), so cross-session
  RSS varies by a few tenths of a GiB. That is small next to the -1.72 effect, but a CPU cost of a few percent is not
  resolved by one cell per arm.
* **Against section 61's main soak** (gm, cross-session, rough): tuned s2 RSS is -0.03 GiB per node on the mean and +0.40
  on the peak. CPU-s/1M is -8.4% and matched/s +9.7%. The live heap is still +0.52-0.60 GiB above main (61: +0.50-0.56).
  So with this setting, Phase 3's soak RSS delta is about its live heap, inside the ~1 GB bound, mean and peak.

![per node per minute: expiry, cancel-alls, margin rejections, matched/s, book, exec queue / busy / ms per block, je overhead, layer, flush queue](ozarchy-p3kd-figs/fig-node.png)

*Figure 62-2: per node (columns), per minute: nonce-expired evictions, cancel-alls executed/s, margin-rejected orders/s,
matched/s, resting orders, exec queue depth, exec busy, exec ms per block, jemalloc resident - allocated, layer MB per
checkpoint and flush-worker depth. def red, dec blue, gs2 green dashed, gm grey dashed.*

### 62.4 Verdict

* **Restart gate (SIGKILL val1, multi-crash cells): PASS** on `c88d3073`. Kill -> val1's first commit was 29.9 s mean /
  39.5 s max over 4 kills (m 27.2 / 31.95), with replay complete and contiguous from the marker. 121 of 122 state-hash
  checkpoints were read on all 3 validators, with 0 disagreements. This completes the restart line that section 61 left at
  "SIGTERM only". With the owner's live-heap RAM decision, every Phase 3 gate is now met.
* **Times are about 2x section 46's** because of the replay rate at this workload, not Phase 3: m is as slow.
  s2's longer replays (from the last 100-block checkpoint) add ~3 s on the mean.
* **jemalloc decay:** `background_thread:true,dirty_decay_ms:1000` cuts s2's soak RSS by 1.72 GiB per node (mean) and
  1.71 GiB (peak), to main's level. The cost is +4.6% CPU-s/1M and -2.5% matched/s, from one cell per arm.
* **Caveats:**
  * One soak cell per arm, so no CI.
  * The setting is an environment variable; nothing is compiled in. A compiled default (e.g. a `malloc_conf` symbol, or
    the tikv-jemallocator build configuration) is a suggestion for 18c, not done or measured.
  * Main was not run with the setting.

Analysis (`tools/ozarchy-p3kd-tools/analyst/`):
* `a-cells.py`: p3gate's, per cell -> `cells.json`, stdout `cells.txt`.
* `a-kill.py`: p3gate's `a-term.py` with the p3kd paths -> `a-kill.txt` on stdout, `kills.json`.
* `a-series.py`: p3gate's, with the cells def / dec / gs2 / gm -> `series.txt`, `series.json`, the figures (SVG -> PNG
  via `rsvg-convert`).
* `a-tables.py`: `restart.txt` from `kills.json` and p3r1's `kills.json`; `soak.txt` from `series.json`, `cells.json`
  and p3gate's `cells-soak.json`.
* `a-archive.py`.

The analyst's `a-kill.py` writes `kills.json` to `ozarchy-p3kd-run/` (not `analyst/`). The archived copies of `a-kill.py`
and `a-tables.py` differ from the analyst's in one way only: optional output paths (`a-kill.py --kills PATH`;
`a-tables.py [OUTDIR [KILLS]]`). With no arguments they use the analyst's paths, so the archive smoke check can run them
without writing into the run dir. **Re-run check for this write-up:** the archived scripts, run in a scratch dir against
the cells, reproduce byte for byte:
* `cells.txt` and `cells.json` (all 7 cells);
* `a-kill.txt` and `kills.json`;
* `series.txt`, `series.json` and both SVGs;
* `restart.txt`.

`soak.txt` matches in every number; only its header label differs ("per node GiB" in the analyst's file, "GB" from the
script; the values are MB / 1024). The archive's smoke entries (`meta/p3kd.toml`) rerun `a-cells.py` on the two soak
cells, `a-kill.py` on the 4 kill cells, `a-series.py` and `a-tables.py`, and diff them against `cells.txt`, `a-kill.txt`,
`series.txt`, `restart.txt` and `soak.txt`. `restart.txt` / `soak.txt` read p3r1's `kills.json` and p3gate's
`cells-soak.json`, which stay on ozarchy.

The run dir also holds `jeconf-proof.txt` / `jeconf-default.txt` (the stats_print proof) and the smoke log. The two
jeconf files (~0.5 MB of allocator statistics) are not copied into the archive; the option lines are quoted in this
section. Full outputs
are in `ozarchy-p3kd-run/analyst/` on ozarchy. The driver is `tools/ozarchy-p3kd-tools/ozarchy-p3kd-campaign.sh`. The
`matched-bench/` copy is p3gate's (`crash-kill.sh` archived again, the rest is `c88d3073`'s).

## 63. Cloid engine-cost fixes: perf/cloid-engine-cost `bc44a845` vs `c88d3073`, both jemalloc compiled in, CLOID_SHARE 100 (n = 4 ABBA, perf on val1) and 0, commit-tail mode (campaign ozarchy-p3cloidec, 2026-10-13)

**At 100% cloid share the fixes raise matched/s by +5.6% (ABBA paired +10,120 [+256, +19,984], 95% CI t(3); 180,373 ->
190,493) and recover about a third of B's own cloid cost (34%, CI 1-68%).** On B (`c88d3073`) c100 runs -14.1% below c0
here. The engine-side views put the recovery at 32-52%. Engine ms per 1k fills drop -0.235 [-0.286, -0.183]. All of that
is `engine_untimed`, which drops 0.806 -> 0.485 (-0.321 [-0.360, -0.282]; 52% of B's +0.622). Phase 1 and match do not
move. Exec `block_ms` drops -12.7% [-22.0, -3.4]. On the profiled node (val1, W1), the exec thread spends -0.285 [-0.411,
-0.159] ms CPU per 1k fills less, and the exec-thread cloid sites drop 0.725 -> 0.364:
* index upkeep: drain 0.332 -> 0.140;
* duplicate pre-pass: 0.323 -> 0.167 (`duplicate_cloid` 0.183 -> 0.073, claimed set 0.140 -> 0.094, its `reserve_rehash`
  0.058 -> 0);
* cleanup: `forget_cloid` 0.071 -> 0.057.

The market workers are flat (+0.084 [-0.262, +0.429]). Their cloid sites shift: the per-book `contains_key` is gone, and
the cloid-log resolve (0.057) moved there from the exec thread. Node CPU-s/1M is flat (-0.49% [-1.29, +0.30]), and so is
RSS. At c0 (n = 2 per arm, a regression check only) F and B tie: matched/s -0.04%.

Requested by 18c after section 60 (where the cloid engine cost goes: index upkeep and the duplicate pre-pass on the exec
thread, the per-book check / `index_cloid` / `forget_cloid` in the workers). Does 18c's `perf/cloid-engine-cost` remove
it? **F** = `bc44a845`, 6 commits on `c88d3073`:
* `d026fa2d`: pinning tests;
* fix 1 `3f649036`: the pre-pass claimed set is pre-sized for the call's cloid placements;
* fix 2 `f0144c6f`: the cloid log is resolved in the worker, one lookup per record;
* fix 3 `927c0721`: the batch workers skip the book's repeated cloid lookup;
* fix 4 `a2c5bd25`: a packed lossless 40-byte `CloidKey` with a cheaper hash (two `write_u128` and one `write_u32`
  instead of foldhash's long path over the 36-byte `(Address, u128)`), and a smaller index value;
* extra `bc44a845`: `forget_cloid` in one lookup.

Node `19ac8761` (build id `de85ebf3`), bench `e562e616`, built fresh by `ozarchy-p3cloidec-build.sh` into
`/home/oz/.cargo-target-p3cloidec-bc44a845` (21:21). **B** = `c88d3073`, F's merge-base (node `4e0d8b84`, build id
`77deb7af`, bench `ae96e0c1`): p3gate's s2 build (15:53) reused, not rebuilt. **The build flags are the same in both arms**
(line 1 of `ozarchy-p3cloidec-build/F.build.log` and `ozarchy-p3gate-build/s2.build.log`):
`RUSTFLAGS=-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes`,
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, release profile. Both have jemalloc compiled in (preflight nm 1,168
jemalloc symbols in each node, no LD_PRELOAD, no MALLOC_CONF). `TORUS_CHECKPOINT_INTERVAL_MS=15000` was set per cell on
both arms. Neither sha has step 6 (unset = 15 s is on main `d3cba5e2` only), so this matches the shipped default. md5s
(`md5s.txt`): F node `19ac8761473462128bae9f3fe94a2b05`, bench `e562e6163937c6cf9d93a836a44500e0`; B node
`4e0d8b84f54f53d5b6db2d5e696cd51c`, bench `ae96e0c13f568213822754da8074ed80`.

Workload as section 61 plus cloids: CLOID_SHARE=100 (c100, every placed order stamped) and 0 (c0), full workload, tw, Classic,
cap 8, 300 markets, standard shape, MAX_IN_FLIGHT 4, budget 900, RATE 76000 (saturated), 120 s cells, commit tail, shared
NVMe, btrfs data dirs. Harness = the `c88d3073` worktree (run-cell.sh, summarize.py, crash-kill.sh, detach.sh); the
per-cell step files (cell-step.sh, cell_step.py, campaign-template.sh) are main `d3cba5e2`'s. Driver
`ozarchy-p3cloidec-campaign.sh` CAMP=CLOIDEC (from the campaign template: after every cell, `cell_step` validates it and
appends a `cells.tsv` row). `cloidprof-sidecar.sh` (section 60's) records perf on val1 in every cell:
* W1, 35-80 s: `cycles:u`, fp call graphs, the whole process;
* W2, 82-102 s: the exec thread only, 5 events incl. DRAM demand fills.

Smoke 21:50-21:58 (s0-F100, s0-B0, 60 s; both ACCEPT AGREE). A first smoke attempt was stopped by the quiet-host gate
(rust-analyzer running) and moved aside (`ozarchy-p3cloidec-smoke1-blocked`, not used). Cells 21:59-23:18:
* w0 (B100, 60 s, excluded);
* c100 r1-F r2-B r3-B r4-F r5-B r6-F r7-F r8-B (ABBA x 2, pairs r1-r2, r4-r3, r6-r5, r7-r8);
* then c0 q1-F q2-B q3-B q4-F (ABBA).

221 CHECK OK, no CHECK FAIL, 0 HARD FINDING. The host was quiet before every cell (no cargo / rustc / rust-analyzer,
1-min load 1.67-1.99). The node md5 matched the arm's staged md5 in every cell (`node-environ-trie.txt` x 3 and
`summary.json`; `cell-step/*.json`). The cloid checks pass in every cell:
* c100: stamped / placed 1.0000, `share_pct` 100;
* c0: stamped 0, `share_pct` 0;
* `duplicate_cloid` rejects: 0 on all 3 nodes in all cells.

Checkpoints: 13-16 per node per 120 s cell in both arms.

**Validity.** 12 of 13 cells are rc 0, ACCEPT, AGREE. **q1-F0 is rc 2, REJECT DISAGREE: the mark-only digest artifact**
(sections 46.4, 61, 62). val2's digest differs from val0's at compare height 1,178 on 50 lines, in `accountValue`,
`freeMargin`, `maintenanceMarginUsed`, `totalMarginUsed` and `withdrawable` only. The block hash and the counters are equal
on all 3 nodes, there are 0 panic / fail-stop / error lines, and liveness passes. val2's digest was the fast one (18.4 s vs
87.4 / 90.7 s). `cell_step` classifies it `mark-only-digest` and keeps the row ok by design. **It is counted in F0 here,**
as earlier sections count such cells for their measurements. The analyst left it out as a "harness bug" (F0 n = 1): that
is corrected below (63.6). **perf:** on q4-F0 the sidecar's W1 `perf record` exited rc 255 after ~1 s (0.3 MB
`perf.data`; W2 is fine). q1-F0's W1 and W2 recordings are complete (27 MB, 4.7 MB), but the analyst did not process them
(the cell was excluded). So no F c0 profile is analysed in this section. The profile compares F100 with B100 (n = 4 each)
and uses B0 (q2, q3) as the zero line.

### 63.1 Cells and the c100 A/B (n = 4 ABBA, paired F100 - B100, 95% CI t(3); `paired.txt`, `cells-all.txt`, `ec-metrics.txt`)

| cell | arm | rc / verdict | matched/s | native blk/s | node CPU-s/1M | engine ms per 1k fills | `engine_untimed` per 1k fills |
|---|---|---|---|---|---|---|---|
| w0 (60 s, excluded) | B100 | 0, ACCEPT AGREE | 174,713 | 4.25 | 25.98 | | |
| r1 / r2 | F100 / B100 | 0 / 0, ACCEPT AGREE | 191,625 / 176,292 | 4.30 / 3.58 | 26.19 / 26.34 | 3.952 / 4.203 | 0.509 / 0.857 |
| r4 / r3 | F100 / B100 | 0 / 0, ACCEPT AGREE | 188,133 / 180,715 | 3.67 / 3.44 | 26.20 / 26.31 | 3.936 / 4.164 | 0.488 / 0.808 |
| r6 / r5 | F100 / B100 | 0 / 0, ACCEPT AGREE | 191,918 / 176,821 | 3.80 / 3.53 | 25.99 / 26.28 | 3.863 / 4.130 | 0.477 / 0.802 |
| r7 / r8 | F100 / B100 | 0 / 0, ACCEPT AGREE | 190,297 / 187,665 | 3.89 / 3.54 | 25.95 / 25.92 | 3.821 / 4.014 | 0.466 / 0.755 |
| q1 / q4 | F0 | 2 (mark-only digest, counted) / 0 | 212,046 / 207,604 | 3.98 / 3.86 | 23.52 / 24.15 | 3.451 / 3.593 | 0.181 / 0.190 |
| q2 / q3 | B0 | 0 / 0, ACCEPT AGREE | 211,903 / 207,902 | 4.07 / 3.83 | 23.79 / 23.76 | 3.456 / 3.511 | 0.182 / 0.186 |

Engine columns: whole cell, mean of val0-2, from the phase timer metrics (`a-ec-metrics.py`).

| metric | B100 | F100 | F100 - B100 [95% CI] | % [95% CI] |
|---|---|---|---|---|
| matched/s | 180,373 | 190,493 | +10,120 [+256, +19,984] | **+5.61% [+0.14, +11.08]**; Welch (`arms.txt`) +5.61% [+1.36, +9.86] |
| placed/s | 236,154 | 249,506 | +13,352 | +5.65% [+0.13, +11.18] |
| bench submits/s (closed loop) | 1,197 | 1,201 | +4 | +0.36% [-3.98, +4.69] |
| native blocks/s | 3.53 | 3.92 | +0.39 | **+11.11% [+1.10, +21.11]** |
| fills per native block | 49,856 | 45,590 | -4,266 | -8.56% [-16.84, -0.27] |
| exec `block_ms` (mean of 3 nodes) | 263.2 | 229.7 | -33.5 | **-12.71% [-21.98, -3.44]** |
| exec per native block p50 / p99 ms | 382 / 730 | 356 / 709 | -26 / -21 | -6.69% [-9.72, -3.67] / -2.89% [-6.28, +0.50] |
| engine ms per 1k fills (summary) | 4.115 | 3.880 | -0.235 | **-5.71% [-6.73, -4.69]** |
| residual untimed ms per native block | 21.6 | 19.2 | -2.4 | -11.08% [-37.28, +15.12] |
| exec busy | 0.963 | 0.964 | +0.001 | tie |
| node CPU-s/1M | 26.21 | 26.08 | -0.13 | -0.49% [-1.29, +0.30]; equal fills (ANCOVA) -0.46% [-1.42, +0.49] |
| block time (commit interval) p99 ms | 1,422 | 1,413 | -9 | -0.61% [-9.33, +8.12] |
| RSS mean / peak GB per node (bench window) | 5.16 / 6.35 | 5.14 / 6.39 | -0.02 / +0.04 | -0.35% [-7.49, +6.79] / +0.57% [-6.25, +7.39] |
| device writes MB/s per validator | 69.3 | 70.8 | +1.5 | +2.16% [-0.15, +4.48] |

Per pair, matched/s rises +15,333 / +7,418 / +15,097 / +2,632 (r8-B100, 187,665, is the highest B cell). The exec thread is
saturated in both arms (busy 0.96), so the lower exec time per fill becomes throughput. This is the same mechanism as in
59/60, in reverse.

### 63.2 How much of B's own cloid cost the fixes recover (`writeup.txt`, `ec-metrics.txt`, `prof-paired.txt`)

| view | B c100 - c0 | F100 - B100 (paired) | recovered |
|---|---|---|---|
| matched/s | -29,529 (-14.07%) | +10,120 [+256, +19,984] | **34% [1, 68]** (CI from the paired delta only) |
| engine ms per 1k fills (metrics) | +0.644 | -0.235 [-0.286, -0.183] | 36% |
| `engine_untimed` ms per 1k fills | +0.622 | -0.321 [-0.360, -0.282] | **52%** |
| W1 exec thread, all cloid sites | +0.711 | -0.361 [-0.409, -0.312] | 51% |
| W1 exec thread total | +0.903 | -0.285 [-0.411, -0.159] | 32% |
| W1 exec + workers, all cloid sites | +0.901 | -0.380 [-0.440, -0.320] | 42% |

F100 is still -9.25% below B0. **B's c100 cost is larger here than in sections 59/60:** matched/s -14.1% vs -11.7% / -12.5%,
engine per 1k fills +18.3% vs +13.9% / +13.1%, `engine_untimed` +0.622 vs +0.566, CPU-s/1M +10.2% vs +8.8% / +8.9%. The
build is different (`c88d3073`, Phase 3 at 15 s, jemalloc compiled in; 59/60 ran `ef812b02` on glibc without Phase 3). The
B gap is also not interleaved: all c0 cells ran after all c100 cells (22:54-23:18 vs 22:04-22:52), and the four c0 cells
drift down over that time (212.0k, 211.9k, 207.9k, 207.6k). So the denominator carries cross-block uncertainty that the
34% CI does not include.

### 63.3 Engine phase timers (whole cell, mean of val0-2, ms per 1k fills; `ec-metrics.txt`, `writeup.txt`)

| phase | B0 (n = 2) | B100 | F100 | F0 (n = 2, q1 counted) | F100 - B100 paired [95% CI] |
|---|---|---|---|---|---|
| engine | 3.484 | 4.128 | 3.893 | 3.522 | **-0.235 [-0.286, -0.183]** |
| `engine_untimed` | 0.184 | 0.806 | 0.485 | 0.186 | **-0.321 [-0.360, -0.282]** |
| `phase1_actions` | 0.540 | 0.595 | 0.590 | 0.554 | -0.005 [-0.041, +0.031] |
| `phase_margin` | 0.861 | 0.841 | 0.880 | 0.888 | +0.040 [+0.020, +0.059] |
| `phase_match` | 0.459 | 0.467 | 0.483 | 0.459 | +0.016 [+0.010, +0.022] |
| `phase_settle` | 1.216 | 1.202 | 1.227 | 1.216 | +0.024 [+0.003, +0.045] |

The whole engine drop is in `engine_untimed`, where section 60 placed both exec-thread cloid sites (the pre-pass and the
absorb / drain run outside the phase timers). Phase 1 and match are flat. Margin, match and settle per 1k fills rise
slightly in F (+0.016 to +0.040, resolved) while F runs 8.6% fewer fills per native block. Per native block all three are
lower in F (not resolved; `ec-metrics.txt`). The +0.016 in match is consistent with fix 2 moving the cloid-log resolve into
the workers, inside the match phase (63.4); not tested separately.

### 63.4 Profile: where the exec thread saves (W1, val1, ms CPU per 1k val1 fills; `prof-compare.txt`, `prof-paired.txt`, `prof-*/cloid-sites.txt`)

Same method as section 60 (`a-prof.py`, run per arm through `a-prof-arm.py` with the arm's own staged node for
`llvm-addr2line` and the arm's own line ranges for the two line-based sites):
* pre-pass `native_executor.rs` 6888-6935 at `c88d3073`, 6924-6973 at `bc44a845`;
* per-book check `order_book.rs` 1396-1403 / 1451-1465.

Extra function sites for F's new functions: `resolve_cloid_log`, `drain_cloid_keys`, `order_id_by_cloid_key`. B0 is
q2/q3 (n = 2); F100 and B100 are n = 4. The deltas are ABBA paired, 95% CI t(3).

| thread group / site | B0 | B100 | F100 | F100 - B100 [95% CI] |
|---|---|---|---|---|
| exec thread total | 3.669 | 4.572 | 4.287 | **-0.285 [-0.411, -0.159]** |
| market workers total | 5.443 | 5.720 | 5.804 | +0.084 [-0.262, +0.429] |
| other threads | 14.847 | 14.722 | 14.162 | -0.560 [-1.818, +0.698] |
| whole process | 23.960 | 25.014 | 24.252 | -0.761 [-2.380, +0.858] |
| exec: index upkeep (B `CloidMarkets::absorb` -> `drain_cloid_log`; F `drain_cloid_keys`) | 0.000 | 0.332 | 0.140 | **-0.192 [-0.206, -0.178]** |
| exec: duplicate pre-pass, all | 0.014 | 0.323 | 0.167 | **-0.156 [-0.201, -0.110]** |
| of which `duplicate_cloid` -> `CloidMarkets::live` | 0.000 | 0.183 | 0.073 | -0.110 |
| of which serial block (claimed set, `retain`) | 0.014 | 0.140 | 0.094 | -0.046 |
| of which claimed-set `reserve_rehash` (incl.) | 0.000 | 0.058 | 0.000 | -0.058 |
| exec: cleanup `forget_cloid` / `forget_cloids` | 0.001 | 0.071 | 0.057 | -0.013 [-0.026, -0.001] |
| **exec: all cloid sites** | 0.015 | 0.725 | 0.364 | **-0.361 [-0.409, -0.312]** |
| exec: not at a cloid site | 3.654 | 3.847 | 3.923 | +0.076 [-0.005, +0.157] |
| workers: `index_cloid` | 0.000 | 0.045 | 0.063 | +0.018 |
| workers: `resolve_cloid_log` (F only, fix 2) | 0.000 | 0.000 | 0.057 | +0.057 |
| workers: per-book `contains_key` in place | 0.002 | 0.073 | 0.004 | **-0.069 [-0.081, -0.057]** |
| workers: `forget_cloid` | 0.000 | 0.075 | 0.050 | -0.024 [-0.037, -0.012] |
| workers: all cloid sites | 0.002 | 0.193 | 0.174 | -0.019 [-0.047, +0.008] |

`reserve_rehash` (`codex-notes.txt`): F's claimed set is a `HashSet<CloidKey>` and shows no rehash. All exec-thread
`reserve_rehash_inner` time is 0.062 on F100, 0.119 on B100 and 0.062 on B0, so fix 1 removes the claimed-set rehash
completely.

Hash and compare leaves (self, exec thread; `prof-compare.txt`), B0 / B100 / F100:
* foldhash `hash_bytes_long` 0.079 / 0.183 / 0.083 (fix 4: the 40-byte key no longer takes the long path);
* the hashbrown probe `_mm_movemask_epi8` 0.109 / 0.341 / 0.243;
* the array key compare `spec_eq` 0.197 / 0.355 / 0.216, plus the new `CloidKey` `eq` (`order_book/cloid_key.rs`) 0.074
  on the exec thread and 0.065 in the workers.

In the workers, `place_order_with_accounts` self (B100 0.120) appears in F as `place_order_checked` (0.136; fix 3's batch
path).

**Read:** fixes 1 and 4 and the drain change roughly halve the exec-thread cloid time. The rest of it (0.364) is index
upkeep 0.140, the pre-pass 0.167 and cleanup 0.057. The workers do not get cheaper: the per-book check (-0.069) and
`forget_cloid` (-0.024) savings are spent on the moved log resolve (+0.057) and `index_cloid` (+0.018). The exec thread is
the bottleneck, so moving work off it is what counts.

### 63.5 Cache behaviour on the exec thread (W2, events per 1k val1 fills, arm means; `prof-compare.txt`)

| event | B0 | B100 | F100 | F100 vs B100 |
|---|---|---|---|---|
| cycles | 11.38 M | 13.26 M | 12.75 M | -3.9% (-1.8 sd) |
| instructions | 15.51 M | 18.42 M | 17.64 M | -4.2% (-1.8 sd) |
| IPC (per-cell mean) | 1.363 | 1.389 | 1.384 | tie |
| L1-dcache-load-misses | 266,908 | 290,372 | 287,812 | -0.9% (-0.3 sd) |
| DRAM demand fills (`mem_io_local`) | 28,498 | 31,833 | 31,677 | **-0.5% (-0.2 sd), noise** |

Per site, B100 -> F100 (arm means; per-site per-cell spread not printed):

| site | cycles | instructions | IPC | L1 misses | DRAM fills |
|---|---|---|---|---|---|
| index upkeep | 932 k -> 404 k | 495 k -> 209 k | 0.53 -> 0.52 | 6,081 -> 5,345 | 2,298 -> 1,219 |
| duplicate check | 997 k -> 501 k | 879 k -> 306 k | 0.88 -> 0.61 | 15,036 -> 10,186 | 1,731 -> 2,240 |
| cleanup | 152 k -> 158 k | 178 k -> 159 k | 1.17 -> 1.00 | 3,096 -> 2,914 | 216 -> 277 |
| not at a site | 11.18 M -> 11.68 M | 16.87 M -> 16.97 M | 1.51 -> 1.45 | 266,159 -> 269,367 | 27,588 -> 27,940 |

The thread's DRAM fills do not move overall. The index site loses about half of its fills (-1,078), and the duplicate
check gains +509. The duplicate check's IPC falls from 0.88 to 0.61 because its instructions fall by 65% (fewer, cheaper
hashes and compares), while its cycles fall only 50% and its DRAM fills rise. What is left at that site is more bound by
memory misses.

### 63.6 c0 regression check (n = 2 per arm, q1-F0 counted; `writeup.txt`)

| metric | F0 (q1 / q4) | B0 (q2 / q3) | F0 - B0 | Welch 95% CI |
|---|---|---|---|---|
| matched/s | 209,825 (212,046 / 207,604) | 209,902 (211,903 / 207,902) | **-0.04%** | [-6.17, +6.09] |
| node CPU-s/1M | 23.83 | 23.78 | +0.24% | [-16.7, +17.2] |
| native blocks/s | 3.92 | 3.95 | -0.85% | [-43.9, +42.2] |
| exec per native block p99 ms | 614 | 609 | +0.72% | [-43.7, +45.2] |
| engine / `engine_untimed` ms per 1k fills (metrics) | 3.522 / 0.186 | 3.484 / 0.184 | +0.038 / +0.002 | |
| RSS mean GB per node (bench window) | 5.55 | 5.10 | +8.7% | [-4.5, +21.9] |
| jemalloc allocated GB (bench-window mean of node means) | 2.889 (2.872 / 2.905) | 2.760 (2.733 / 2.788) | +0.13 | |

**No regression at c0 on throughput, CPU or engine time.** The c0 cells are only a regression check: n = 2 per arm, with
intervals far too wide to resolve a few percent. Without q1-F0 (the analyst's F0 n = 1), the same comparison read
matched/s -1.1% and engine +0.110 (+3.2%) per 1k fills. Both come from q4-F0 alone, the last cell of the run. F0's RSS and
jemalloc allocated are higher in both of its cells (+0.44 GB RSS mean, +0.13 GB allocated); with n = 2 that is not
resolved (the RSS first-15-s figure, +0.66 GB, is). At c100, where the changed code runs, neither moves
(63.7 c). Not explained; noted.

### 63.7 Codex notes (18c's review of `bc44a845`; `codex-notes.txt`, `jemalloc-rss.txt`)

* **(a) The per-batch cloid count visits every placement** (`native_executor.rs:6929` at `bc44a845`, a `count()` where B
  had `any()`): self time at F's count lines (6924-6933) is 0.0004 ms per 1k fills at c100. Upper bound: B0's `any()`
  scans every placement when no order has a cloid, at 0.0127. So the count costs **<= 0.013 ms per 1k fills**.
* **(b) One vector write per logged cloid change in matching** (`order_book.rs:2453`): F's log push / resolve lines have
  no self samples. The workers' `index_cloid` rises +0.018 (0.045 -> 0.063); that is the upper bound visible in this
  profile.
* **(c) The log vectors keep their capacity (40-80 B per slot):** no retention is visible at c100 in 120 s. jemalloc
  allocated is F100 2.845 (sd 0.039) vs B100 2.891 (sd 0.083) GB, resident 4.669 vs 4.677 GB, and RSS mean in the bench
  window 5.138 vs 5.156 GB (paired -0.018 [-0.386, +0.350]). A soak would show slow growth; none was run.

### 63.8 Verdict

* **The fixes work: +5.6% matched/s at 100% cloid share** (paired [+0.1, +11.1]), native blocks/s +11%, exec `block_ms`
  -12.7%, engine -0.235 ms per 1k fills (all `engine_untimed`), no CPU, RSS or write cost, and no regression at c0.
* **About a third of B's cloid cost is recovered** (matched/s 34%, CI 1-68%; engine-side views 32-52%). F at c100 is still
  9.3% below B at c0. What is left on the exec thread is index upkeep 0.140, the pre-pass 0.167 (`duplicate_cloid` 0.073,
  claimed set 0.094) and cleanup 0.057 ms CPU per 1k fills, and the duplicate check is now miss-bound (IPC 0.61).
* **What's left:** the analyst suggests 2 more c100 pairs (~25 min) to narrow the recovery CI (now 1-68%). That is the
  owner's call. An F c0 profile, if wanted, is already recorded in q1-F0 (not analysed).

**Caveats.**
1. 100% share is the worst case, as in 59/60.
2. n = 4 pairs at c100. The recovery fraction's CI covers only the paired delta, not the non-interleaved c0 denominator
   (63.2).
3. c0 is n = 2 per arm and a regression check only.
4. The profile is val1 only, user space only, minutes 0-2.
5. W2 per-site figures are arm means without a per-cell spread.
6. One profiled cell (q4-F0 W1) failed, and one (q1-F0) was not analysed.

**Method note: perf symbol-cache trap.** Both arms run from one path: the harness copies the arm's node to the worktree's
`target/release/torus-node` at each cell start (the sidecar logs `exe=` that path with md5 `19ac8761` in F cells). perf
resolves a recording's DSO through the build-id cache first. On ozarchy the `~/.debug` entries for B's build id
`77deb7af` and F's `de85ebf3` are hard links (link count 3) to the same inode as that path. After the in-place copy at
q4-F0's start (23:14:12, the entry's mtime) both hold F's binary (md5 `19ac8761`). A plain `perf script` on a B cell
therefore resolves B's samples against F's binary: on r2-B100's first 20,000 lines of output, 17,277 lines are `[unknown]`
without `--symfs`, against 5,184 with it (the kernel frames, `[unknown]` in both). The analyst made every dump with
`perf script --symfs analyst/symfs-<arm>`, where `symfs-<arm>/<recorded path>` links to the arm's staged node. Checked for
this write-up: the first 20,000 lines of r2-B100's `prof.script.gz` are byte-identical to a fresh `--symfs symfs-B` run.
The analyst's saved `prep-prof.sh` lacks the `--symfs` (the dumps came from an inline loop). The archived copy adds it. For
any A/B that profiles two binaries from one path, run `perf script --symfs` with the staged binary (or `perf buildid-cache
--add` each staged binary) and do not trust the cache. Side effect: the `c88d3073` harness worktree's
`target/release/torus-node` now holds F's node (md5 `19ac8761`), not `c88d3073`'s.

Analysis (`tools/ozarchy-p3cloidec-tools/analyst/`; outputs in `ozarchy-p3cloidec-run/analyst/` on ozarchy):
* The analyst's scripts: `a-cells.py` (section 60's) -> `cells-all.json` / `cells-all.txt`; `a-arms.py` (section 60's,
  unchanged, run from p3cloidprof's dir on `cells-valid.json`, i.e. without w0 and q1-F0) -> `arms.txt`, whose last
  comparison, F0 vs B0 with F0 n = 1, ends in a ZeroDivisionError in the ANCOVA; `a-ec-metrics.py` -> `ec-metrics.txt`
  (lists q1-F0 as `F0-BAD`, left out of the arm means); `prep-prof.sh` + `a-prof-arm.py` (runs p3cloidprof's `a-prof.py`
  unchanged on disk with the arm's binary and line ranges; its `SRC` path points at the p3cloidprof run dir on ozarchy,
  archived as `tools/ozarchy-p3cloidprof-tools/analyst/a-prof.py`) -> `prof-F/`, `prof-B/`; `a-prof-compare.py` ->
  `prof-compare.txt`.
* Added for this write-up (read-only on the cells): `a-writeup.py` -> `paired.txt` (c100 ABBA pairs, a-paired.py's
  format) and `writeup.txt` (recovery, c0 with q1-F0 counted, engine timers with q1-F0 counted); `a-prof-paired.py` ->
  `prof-paired.txt`; `a-arms.py` is copied next to `a-writeup.py`, which uses its `row()` and `welch()`.
* The analyst's inline queries, saved as scripts with the query unchanged and only the paths parameterised:
  `a-codex-notes.py` -> `codex-notes.txt`, and `a-jemalloc-rss.py` -> `jemalloc-rss.txt`.

**Re-run check for this write-up** (scratch dir, against the cells): `a-cells.py` reproduces `cells-all.txt` and
`cells-all.json` byte for byte. The `cells-valid.json` cut reproduces byte for byte. `a-arms.py` reproduces `arms.txt`
except the 3 traceback path lines (the copy's location). `a-ec-metrics.py`, `a-prof-compare.py`, `a-jemalloc-rss.py`
(against the analyst's `tee` output) and `a-codex-notes.py` (against the inline output in the analyst session) reproduce
byte for byte, as does `a-writeup.py` (`paired.txt`, `writeup.txt`, run twice). `a-prof-arm.py` was not rerun: it needs
the Tier A perf data and takes minutes. The per-arm `a-prof.py` outputs are archived under `analyst/prof-F/` and
`analyst/prof-B/` (`cloid-sites.txt`, `frames-*.txt`, `windows.txt`, `run.log`; `llc.txt` stays on ozarchy, its W2
figures are in `prof-compare.txt`). Not copied into the repo: the folded stacks, `prof-cells.json`, `a2l-cache.json` and
the perf script dumps (Tier A on ozarchy), and the run's `matched-bench/` copy (byte for byte `c88d3073`'s
`tools/matched-bench` plus `d3cba5e2`'s cell-step files). Driver
`tools/ozarchy-p3cloidec-tools/ozarchy-p3cloidec-campaign.sh`, build script `ozarchy-p3cloidec-build.sh`, sidecar
`cloidprof-sidecar.sh`, md5s `md5s-all.txt`.

## 64. Phase 4 step 0 re-profile and exec-time p99 causes on main `d3cba5e2` (jemalloc compiled in, 15 s checkpoints, oracle on), 300 markets, commit-tail mode (campaign ozarchy-p4s0, 2026-10-14)

**The step 0 items cost about 4% of exec-thread wall time (sum 4.06%; p1 3.84, p2 4.27): above the plan's ~2% stop
rule, so Phase 4 goes ahead as planned.** The margin is thin, and most of it depends on what is counted:
* `open_order_counts` (step 5) is 1.45% (3.5 ms per native block). Without it the sum is 2.61%.
* The `BlockSums` memo lock and `Arc<OnceLock>` are 0.18%. That is too small for decision 4 (b), so (a) stands and the memo
  stays: this 0.18% is not removed by Phase 4. R reads plus `open_order_counts` = 3.88%; R reads alone = 2.43%
  (p1 2.27).
* Half of the R-read share (1.21 of 2.43%) is a wall-equivalent estimate: misses on the settle pool's `PositionCache`
  (`update_in_place` -> `PositionManager::get_position` -> overlay `get_cf_raw`) that the exec thread waits on. The exec
  thread's own R reads are 1.21%.
* All of these are gross costs. The cost of the typed replacements (step 4's typed loads, step 5's counter upkeep per rest,
  cancel, fill and stop) is not measured and not netted.

Confidence medium (n = 2 profiled cells, val1 only, user space).

**Exec time per native block, p99 on val1: 581 / 596 ms SAT (u1 / u2), 585 ms in the 600 s soak (k1);** p50 313 / 312 /
355 ms. Of the 9 SAT blocks above p99, 6 are in the arrival / fill burst 3-6 s after bench start, 2 are at the first
checkpoint (~14 s) and 1 is unexplained. Of the 17 soak blocks: 5 epoch-boundary blocks (height % 100 = 0, not pipelined), 4
with a checkpoint write in flight, 3 in the start burst, 3 in a fill burst at ~76 s, 2 unexplained. No cell had a RocksDB
write stall. **This is exec time per native block, not section 61's block time (commit interval).** Section 61's block-time
p99 rose +17.7% SAT and +23.1% soak on Phase 3 (p50 -32.7% / -13.6%); its exec p99 per native block was 590 SAT / 570 soak ms
on s2, close to this campaign's 581-596 / 585. The block-time p99 rise is not decomposed here.

Requested by 18c (owner change of plan: Phase 4 step 0 before Gate 3), per `docs/plans/item6-phase4-impl.md` step 0: on the
Phase 3 head, 300 markets uniform, oracle on, 15 s interval, measure the exec-thread share of what Phase 4 removes. One
binary, two cell kinds:
* **m** = main `d3cba5e2` (Phase 3 step 2 + merge-wait + step 6), node `c2fda80f` (build id `5c8e43a2`), bench `ff08433d`.
  Built by ozarchy-g3s0's build (`ozarchy-g3s0-build/build-m.sh`, target `/home/oz/.cargo-target-g3s0-d3cba5e2`;
  `RUSTFLAGS=-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes`, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`,
  release). Staged in `ozarchy-g3s0-stage/m-run` (shared with Gate 3, not rebuilt; md5s in `ozarchy-g3s0-build/md5s.txt`,
  line `M`). jemalloc compiled in (preflight: 700 `_rjem_` symbols; no `LD_PRELOAD`, no `MALLOC_CONF`).
  `TORUS_CHECKPOINT_INTERVAL_MS` unset = the 15 s validator default (step 6; the enable line logs `interval: Some(15s)`).
* **mperf** = the same binary and settings with the perf sidecar on val1 (`perf-sidecar.sh`, byte for byte section 60's
  `cloidprof-sidecar.sh`): W1 35-80 s `cycles:u`, fp call graphs, the whole process; W2 82-102 s the exec thread only,
  5 events.

Workload: oracle feed on (`ORACLE_FEED=1`, price 30000, 2,000 ms interval, walk 0; every update accepted except 1 of 1,848 in k1,
val2 "server overloaded"; all 300 marks fresh at bench end), CLOID_SHARE=0, Classic book rows, 300 markets, 5,000 senders, block cap 400, MAX_IN_FLIGHT
4, budget 900, RATE 76000 (saturated), commit tail, shared NVMe. Harness = the `d3cba5e2` worktree
`/home/oz/projects/wt/g3s0-d3cba5e2`; the run's `matched-bench/` copy is byte for byte `d3cba5e2`'s `tools/matched-bench`
(top-level files checked against the git blobs). Driver `ozarchy-p4s0-campaign.sh` CAMP=P4S0 (from the campaign template,
config by `patch-p4s0.py`; after every cell `cell_step` validates it and appends a `cells.tsv` row).

Smoke 00:36 (s0-m 60 s, 218,555 matched/s; s0-mperf 120 s, 216,668 and a complete `perf.data`; both ACCEPT AGREE). Cells
00:52-01:32: w0 (60 s warm-up), p1, p2 (mperf, 120 s), u1, u2 (m, 120 s), k1 (m, 600 s soak).

**Validity.** 6 of 6 cells rc 0, ACCEPT, AGREE, node md5 = staged md5, 0 CELL-FLAG; 78 CHECK OK, no CHECK FAIL, 0 HARD
FINDING. The host was quiet before every cell (no cargo / rustc / rust-analyzer, no analysis process; 1-min load
0.18-1.88). `exec_resident_rebuilds` 1 per node.

### 64.1 Cells (`cells.tsv`, `handoff-tables.txt`, the cells' `summary.json`)

| cell | kind | rc / verdict | matched/s | native blk/s | fills per native block | engine ms per 1k fills |
|---|---|---|---|---|---|---|
| w0 (60 s, warm-up) | m | 0, ACCEPT AGREE | 216,548 | 4.91 | 35,057 | 3.40 |
| p1 (120 s) | mperf | 0, ACCEPT AGREE | 213,357 | 4.14 | 49,108 | 3.47 |
| p2 (120 s) | mperf | 0, ACCEPT AGREE | 214,409 | 4.39 | 48,880 | 3.44 |
| u1 (120 s) | m | 0, ACCEPT AGREE | 216,560 | 4.26 | 48,140 | 3.42 |
| u2 (120 s) | m | 0, ACCEPT AGREE | 216,974 | 4.16 | 50,031 | 3.44 |
| k1 (600 s soak) | m | 0, ACCEPT AGREE | 220,066 | 2.97 | 72,126 | 3.42 |

SAT (u1, u2) mean 216,767 matched/s; the perf cells run 1.3% lower (213,883), the usual recording cost on val1. No A/B in
this campaign: there is no comparison arm.

### 64.2 Step 0: what Phase 4 removes, % of exec-thread wall time (W1, val1; `step0-table.txt`, `step0-shares.txt`)

| item (plan step 0) | p1 | p2 | mean | removed by |
|---|---|---|---|---|
| wall ms per native block (W1) | 246.6 | 242.1 | 244.4 | |
| exec on-CPU ms per native block (perf samples) | 187.9 | 188.1 | 188.0 | |
| R reads of the two CFs, exec thread | 1.12 | 1.31 | 1.21 | steps 2 + 4 |
| R reads, settle-pool `PositionCache` misses the exec thread waits on | 1.15 | 1.28 | 1.21 | steps 2 + 4 |
| **R reads, total** | **2.27** | **2.58** | **2.43** | |
| of which `BalanceCache::load` miss (exec) | 0.29 | 0.30 | 0.29 | |
| `BlockSums` memo lock + `Arc<OnceLock>` | 0.16 | 0.21 | 0.18 | decision 4 (b) only; (a) stands |
| `open_order_counts` | 1.41 | 1.48 | 1.45 | step 5 |
| `open_order_counts` ms per native block | 3.48 | 3.59 | 3.54 | |
| **sum (R + memo + `open_order_counts`)** | **3.84** | **4.27** | **4.06** | |
| sum without `open_order_counts` | 2.43 | 2.79 | 2.61 | |
| R + `open_order_counts` (without the memo, which stays) | 3.68 | 4.06 | 3.88 | |
| upper: sum + end_resident wait + `resident_changes` R get / clone | 5.30 | 5.88 | 5.59 | |
| exec on-CPU share, strict union, W1 (% of exec samples) | 3.53 | 3.86 | 3.69 | |
| same, W2 cycles (exec thread only, 82-102 s) | 3.90 | 3.61 | 3.76 | |

How the shares are made: `step0.py` tags each W1 sample of the exec thread (tid from the sidecar's `exec-tid.txt`) by the
frames on its stack (R reads = `get_native_balance`, `get_cum_volume`, `get_position` outside the caches, `has_key` /
`positions_for_trader`; memo = `pos_sums` residual or a `memo` / `OnceLock` frame; `open_order_counts` /
`add_open_order_counts`). A share of exec samples times on-CPU over wall per native block gives % of exec wall. The settle-pool
row comes from `timeline.py`: 2 ms bins where the pool runs `compute_market_settle_plan` and the exec thread has no sample
(it waits), weighted by the pool's R-read sample fraction in each bin, as ms per native block. W2 (exec thread only, cycles)
agrees with W1 on the exec-thread union: 3.76 vs 3.69% of exec samples.

The rest of step 0's list:
* **`PositionCache::load` / `BalanceCache::load` misses per native block:** this build has no counter for them (no cache-miss
  metric in `metrics-*-val1.txt`), so there is no count. The perf proxy: the `BalanceCache::load` miss path is 0.37 / 0.39% of
  exec samples (0.29% of exec wall, in the R row above); the `PositionCache::load` miss path on the exec thread is 0.02 /
  0.01% of exec samples. The position misses that matter run in the settle pool (`update_in_place`; 1.21% wall-equivalent
  above).
* **`open_order_counts`:** 3.5 ms per native block, all on the exec thread (`torus_exec_thread_spawns_open_orders` 0). Per 1k
  fills that is 0.071 / 0.073 ms (p1 / p2), against 0.26-0.36 in section 11 (older build) that decision 2's "~6-8%"
  estimate used. 1.45% is below the ~5% at which decision 2's recommendation would prefer (b).
* **`end_resident` (whole W1, val1, metrics deltas per native block):** rows 13.01 / 13.68 ms and positions 9.81 / 10.11 ms
  on the end_resident worker; the exec thread waits for it 1.95 / 2.23 ms (0.79 / 0.92% of exec wall, in the upper row).
  Phase 4 removes the raw apply of the two CFs from the rows part. Whole-cell values agree (p1: rows 9.5, positions 7.4,
  wait 2.1 ms per engine block over all 804 blocks incl. ramp and drain).
* **`exec_resident_rows_bytes`:** 214,046,039 B (204 MiB) on val1 in the five 120 s and 600 s cells (w0: 214,038,394).

**Rough throughput size (estimate, gross).** With the plans' rule of thumb (`item6-phase2-impl.md` section 9) (+0.4-0.55% matched/s per ms saved per native
block): R + `open_order_counts` 3.88% of ~244 ms = ~9.5 ms -> +3.8-5.2%; R alone ~5.9 ms -> +2.4-3.3%; `open_order_counts`
alone 3.5 ms -> +1.4-1.9%. All before the typed replacements' own cost.

### 64.3 Exec-time p99 per native block, val1 (`p99-attribution.txt`, `p99-phases.txt`, `p99-causes.txt`, `p99-*.json`)

`blocks.py` reads each executed block's start and end from val1's log (`executing finalized block` / `block done`) into
`blocks-<cell>-val1.csv`. `attrib.py` takes the native blocks in the bench window, sets p50 / p99 and tags every block with
features: first 8 s (ramp), checkpoint settle wait > 25.6 ms in that second, a checkpoint write in flight (`ckpt-samples.txt`,
2 s samples), end_resident wait > 64 ms, RocksDB compaction or flush overlap (`rocksdb-LOG-val1.txt`), a RocksDB stall,
matched/s above 1.2 x the median, the previous block oracle-only, and epoch boundary / not pipelined. `phases.py` adds the
engine phase timers per engine block in the 1 s sampler interval that holds the block. These are approximate: a 1 s interval
holds about 2 blocks.

| cell | native blocks | p50 ms | p99 ms | above p99 | causes |
|---|---|---|---|---|---|
| u1 (SAT) | 405 | 313 | 581 | 5 | 4 start burst (h 439-442, 4.1-6.0 s); 1 first checkpoint (h 463, 14.4 s: checkpoint settle wait > 25.6 ms, phase 1 58 vs 24 ms median) |
| u2 (SAT) | 397 | 312 | 596 | 4 | 2 start burst (h 410, 412; 3.1, 4.3 s); 1 first checkpoint (h 436, 14.0 s, 733 ms: write in flight, upkeep 53 ms per block in that second); 1 unexplained (h 681, 58.1 s, 627 ms) |
| k1 (soak) | 1,681 | 355 | 585 | 17 | 5 epoch boundary (h 600, 1000, 1500, 1700, 1900; 593-676 ms); 4 checkpoint (h 436 first, with a 2x fill burst; h 677, upkeep 86 ms per block; h 2087 / 2088, upkeep 21); 3 start burst (h 409, 412, 413; 3.3-5.7 s); 3 fill burst (h 745, 746, 749; 75.8-78.0 s); 2 unexplained (h 1547 at 585 = the p99 itself, h 1847 at 634) |

* **Start burst (6 of 9 SAT blocks, 3 of 17 soak):** the first seconds carry 2-2.4x the median fills per engine block (u1
  h 439-440: 219k vs 91k; margin 243 vs 73 ms, settle 430 vs 102 ms, end_resident wait 28 vs 0.7 ms). The bench's arrival
  ramp, not a node effect. Full blocks in the first 8 s average 563 / 531 / 519 ms against 335 / 332 / 359 ms after.
* **Epoch boundary (5 of 17 soak):** every 100th height executes unpipelined (rewards). The 16 full epoch / unpipelined blocks
  of k1 average 538 ms against 358 ms for the rest. SAT cells have 2-3 such blocks, none above p99.
* **Checkpoint (2 SAT, 4 soak):** the first checkpoint after bench start (~14 s) and later ones with a long upkeep. Over all
  full blocks, "checkpoint write in flight" is not slower on average (k1 372 vs 356 ms, u1 306 vs 352): it is the occasional
  long upkeep or settle wait, not the write itself.
* **Fill burst at ~76 s (3 soak):** fills 181k vs 91k median at h 746 (margin 232, match 172 ms), with compaction overlap.
* **No RocksDB write stall** in any cell (the stall feature is 0 everywhere). Compaction and flush overlap do not separate the
  tail consistently (full blocks with vs without: -88 to +64 ms, mixed sign).

`p99-causes.txt` (`phases.py`'s automatic rule: the largest excess component over the median, > 15 ms) labels some of the
same blocks differently: the start-burst blocks with normal fills (u1 h 441-442) and the checkpoint blocks with a fills or
phase 1 excess become "other engine work at normal fills". The grouping above uses the time and the feature tags from
`p99-attribution.txt` with the phase timers, as the analyst did.

### 64.4 Verdict and owner decision

* **Step 0 is above the stop rule: Phase 4 goes ahead as planned** (steps 1-6). The plan's stop rule only asks for an owner
  decision below ~2% (build now as Phase 5's base, or fold into Phase 5).
* **The margin is thin.** The sum is 4.06% (3.84-4.27) with the memo and 3.88% without it (the memo stays under decision 4
  a). It is 2.43% without `open_order_counts` (p1 2.27), and only 1.21% if the settle-pool wait share does not move with the
  exec thread. These are gross costs; the typed loads and the counter upkeep take part of them back.
* **Owner decisions this feeds:**
  * Decision 2 (`open_order_counts`, step 5): the number the plan sends to the owner before step 5 is **1.45% of exec wall,
    3.5 ms per native block (0.07 ms per 1k fills)**, gross. That is below the ~5% the recommendation named for (b), and well
    below section 11's estimate. (b) is decided; whether to keep step 5 in Phase 4 is the owner's call.
  * Decision 4 (memo): 0.18%; (a) stands.
* **p99:** the exec tail is a start-of-bench ramp, epoch-boundary blocks and checkpoint upkeep spikes, not RocksDB stalls.
  Phase 4 does not target any of these. The commit-interval p99 rise of section 61 is a separate question and is not answered
  here.

**Caveats.**
1. n = 2 profiled cells, val1 only, user space (`cycles:u`), W1 = 35-80 s of a 120 s cell.
2. "% of exec wall" uses perf's sample count as on-CPU time (187.9 / 188.1 ms per native block). The exec tid's `/proc`
   ticks over W1 give 199.8 / 199.2 ms (user 194.1 / 194.3), so the exec-thread rows may be 3-6% (relative) low; the verdict
   does not change.
3. The settle-pool row is a wall-equivalent estimate from 2 ms bins, not a measured wait.
4. Stack tags are by frame name; inlined frames can move samples between rows. The analyst also made `--inline` dumps
   (`*-inline.script.gz`; 5 `addr2line` read errors each in `p*-w1-inl.err`).
5. p99 attribution: one node, 1 s phase intervals, 2 s checkpoint samples; 3 blocks stay unexplained.
6. No cache-miss counter for `PositionCache` / `BalanceCache` in the build: step 0's "misses per native block" is a perf
   share, not a count.

**Method note.** The sidecar picks the exec thread per cell (the `torus-execution` task with the most CPU at W1 start): tid
78864 in p1, 121714 in p2 (`exec-tid.txt`); every per-thread filter uses that cell's tid. The perf dumps were made with
`perf script --symfs analyst/symfs`, whose `<recorded path>` links to the staged node; build id `5c8e43a2` = the staged
binary (md5 `c2fda80f`), so the symbol-cache trap of section 63 does not apply.

Analysis (`tools/ozarchy-p4s0-tools/analyst/`; outputs in `ozarchy-p4s0-run/analyst/` on ozarchy):
* `scripts/stacks.py` (perf script reader), `scripts/step0.py` -> one JSON line per cell and window (`step0-shares.txt`,
  W1 = all exec samples, W2cyc = W2's cycles event); `scripts/timeline.py` (settle-pool wait), `scripts/pool.py`,
  `scripts/allthreads.py`, `scripts/q.py`, `scripts/tree.py` (inline queries); `step0-table.txt` is assembled from these and
  the W1 metric deltas inline (no script).
* `scripts/blocks.py` -> `blocks-*-val1.csv`; `scripts/attrib.py` -> `p99-attribution.txt`, `p99-<cell>.json`;
  `scripts/phases.py` -> `p99-phases.txt` and `p99-causes.txt` (the attribution part was added after `p99-phases.txt` was
  written).
* **Re-run check for this write-up** (light, niced; the host was running a bench): `phases.py` on u1 reproduces u1's lines of
  `p99-phases.txt` and `p99-causes.txt` byte for byte (the archive smoke check). The perf scripts were not rerun (no perf or
  heavy analysis on the host during the Gate 3 campaign).
* Not copied into the repo: `nm.txt` (15.8 MB, the node's symbol table), the perf script dumps (`*.script.gz`, Tier A with
  `perf.data` / `perf-w2.data`) and `blocks-*-val1.csv` (rebuilt by `blocks.py` from `val1.log.gz`, which stays). Driver
  `tools/ozarchy-p4s0-tools/ozarchy-p4s0-campaign.sh`, config patcher `patch-p4s0.py`, sidecar `perf-sidecar.sh`, md5s
  `md5s-all.txt` (g3s0's `md5s.txt`); `analyst/a-cells.py` is section 60's copy from the tools dir, not run here.

## 65. Item 6 Gate 3 checkpoint: main `d3cba5e2` vs pre-item-6 main `199bdc97` (backported bench), 300 markets, oracle off (primary) and on ("as shipped"), plus `199bdc97` + LD_PRELOAD jemalloc, commit-tail mode (campaign ozarchy-g3, 2026-10-14)

**Like for like, main `d3cba5e2` with the oracle off runs 1.93x the matched/s of pre-item-6 main `199bdc97` with the
oracle off: 1.931 [1.877, 1.985].** The Gate 3 target is >= 1.4x "after all phases"
(`docs/plans/market-scaling-in-memory-design.md`, stage gates). This is a checkpoint after Phases 1-3 + step 6, so it is
above the target before Phases 4-5. **"As shipped"** (M oracle on vs G oracle off) is **1.835 [1.774, 1.898]**. All 22
cells are rc 0, ACCEPT, AGREE, liveness PASS.

| ratio of arm means, matched/s (Welch 95% CI on log matched/s; `stats.py`) | owner treatment (r2-moff replaced by its rerun) | r2-moff kept + rerun | owner treatment without the 5 localsearch cells |
|---|---|---|---|
| **primary: M off / G off** (Gate 3 like for like; target >= 1.4x after all phases) | **1.931 [1.877, 1.985]** | 1.876 [1.717, 2.042] | 1.940 [1.905, 1.975] |
| **"as shipped": M on / G off** | **1.835 [1.774, 1.898]** | 1.835 [1.774, 1.898] | 1.831 [1.716, 1.954] |
| allocator: GJ off / G off (LD_PRELOAD jemalloc on `199bdc97`) | 1.108 [1.088, 1.128] | 1.108 [1.088, 1.128] | 1.104 [1.081, 1.128] |
| M off / GJ off (item 6 with both on jemalloc) | 1.743 [1.695, 1.792] | 1.694 [1.552, 1.842] | 1.757 [1.718, 1.798] |
| marks: M on / M off | 0.951 [0.918, 0.984] | 0.978 [0.900, 1.067] | 0.944 [0.889, 1.002] |

**Read the primary with its confound:** it is not a node-only comparison. The bench binaries differ (G `a951801c`, the
backport; M `ff08433d`) and so does the action mix: place share 0.28 on G off vs 0.61 on M off, and the mix is partly an
outcome of node speed (closed-loop bench). See 65.4.

Gate 3 checkpoint requested by 18c (owner design s54, 2026-10-14), after the Phase 4 step 0 re-profile (section 64). Arms:
* **goff (G)** = **`199bdc97`**, pre-item-6 main, glibc as shipped (preflight `nm`: 0 `_rjem_` symbols), node `d6b05d8e`,
  oracle feed off. **The base sha stays `199bdc97`; its bench is backported:** branch `bench/g3-base-199bdc97` @
  `22d3cc03` (pushed, not merged) changes only `tools/bench-throughput` (oracle-feed, `--max-in-flight`,
  `--in-flight-tail`, `--open-order-budget`, `--retry-busy` ported from main). Signing is `199bdc97`'s own (the old order
  format, no cloids), and `--cloid-share` accepts only 0. The node built at `22d3cc03` has md5 `d6b05d8e`, the same as a
  plain `199bdc97` build (`md5s.txt`: `G` = `bG`, same worktree path and target dir). Bench `a951801c`.
* **moff (M)** = **main `d3cba5e2`** (Phases 1-3 + merge-wait + step 6, 15 s checkpoints by default), jemalloc compiled in
  (700 `_rjem_` symbols; no preload, no `libjemalloc.so` mapped), node `c2fda80f`, bench `ff08433d`, oracle feed off. The
  same staged build as section 64.
* **mon** = M with the oracle feed on (`ORACLE_FEED=1`, price 30000, 2,000 ms interval, walk 0), as in section 64.
* **gjoff (GJ)** = the G node started through a bash wrapper (`ozarchy-g3s0-stage/g-je/release/torus-node`, md5
  `e676f703`) that sets `LD_PRELOAD=/usr/lib/libjemalloc.so.2` (system jemalloc 5.3.1, defaults, `MALLOC_CONF` unset) and
  `exec`s the real G binary. Oracle feed off.

**Allocators as shipped:** `199bdc97` uses glibc malloc; `d3cba5e2` has jemalloc compiled in (default feature `jemalloc`,
`88ddc4c2`, section 56). GJ separates the allocator from item 6.

**Why the primary is oracle off on both arms:** pre-item-6 mains have no mark aggregation. At `199bdc97`
`NativeExecutor::aggregate_oracle_prices` exists but has no caller (`git grep` at `199bdc97`: only the definition), so a
live `199bdc97` node never computes marks even when fed prices. Per-block aggregation is `454a9bba`, which reached main in
`59fa407b` (the Phase 1 merge, 2026-10-06). The like-for-like ratio is therefore oracle off on both arms. "As shipped"
compares M with the oracle on against the same G oracle-off cells, because the old node never computed marks. Gate 2's
base was set up the same way (section 19: oracle feed on the crab arm only). The design's gate text says "oracle on"; here
only the "as shipped" ratio has it.

Build (ozarchy-g3s0, 2026-10-13/14, `ozarchy-g3s0-build/`): `build-g.sh` / `build-bg.sh` (target
`/home/oz/.cargo-target-g3s0-199bdc97`) and `build-m.sh` (`/home/oz/.cargo-target-g3s0-d3cba5e2`), all with `RUSTFLAGS=-C
link-arg=-fuse-ld=mold -C force-frame-pointers=yes`, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`, release; staged in
`ozarchy-g3s0-stage/{g-run,g-je,m-run}` (`gj-run` is a symlink to `g-je`). Harness = the `d3cba5e2` worktree
(`/home/oz/projects/wt/g3s0-d3cba5e2`); every arm, including G, runs under its `run-cell.sh`. The run's `matched-bench/`
copy matches `d3cba5e2`'s `tools/matched-bench` byte for byte (25 top-level files checked against the git blobs). Driver
`ozarchy-g3-campaign.sh` CAMP=G3, generated by `make-g3-driver.py` from `ozarchy-p4s0-campaign.sh` (only the config,
per-cell checks and preflight differ).

Workload in every cell: 300 markets uniform, 5,000 senders, CLOID_SHARE=0, Classic book rows, tw, block cap 400,
MAX_IN_FLIGHT 4, budget 900, RATE 76000 (saturated), commit tail, `TORUS_CHECKPOINT_INTERVAL_MS` unset (M: the 15 s
validator default, 14-16 checkpoints per node per cell; G / GJ: no checkpoint enable line, no checkpoint series), genesis
state hash off, shared NVMe, btrfs data dirs.

Smoke 02:03-02:30 (60 s each, context only): s0-goff 124,191, s0-moff 221,478, s0-gjoff 127,533 (md5 flag, see below) and
its queued rerun 130,836. s0-mon was rc 2 DISAGREE: `agreement.jsonl` has block hash `ERR` on val0 / val1 at compare
height 1,104 (the hash was not read) and the hash read on val2. The state digests were identical on all 3 nodes. Its rerun
s0-mon-rerun was AGREE (216,343). Cells 02:33-04:42: w0 (moff, 60 s, warm-up, excluded), then r1-r4 in rotated balanced
rounds, 120 s each (r1: goff moff mon gjoff; r2: moff mon gjoff goff; r3: mon gjoff goff moff; r4: gjoff goff moff mon).
Then the rerun pass 04:14-04:42: r1-r4-gjoff-rerun (queued by `cell_step` for the md5 flag) and r2-moff-rerun (appended to
`rerun-queue.tsv` by hand mid-campaign, owner decision; `cell_step` had not queued it).

**Validity.** 22 of 22 cells (w0 + 16 + 5 reruns) rc 0, ACCEPT, AGREE, liveness PASS. Block hash and state digest are
equal on 3 nodes in every cell, with 0 panic / fail-stop and 0 error lines (`agreement.jsonl`). The running exe md5
(`node-environ-trie.txt`) is the arm's node on all 3 nodes in all 22 cells (G / GJ `d6b05d8e`, M `c2fda80f`). 286 CHECK OK
(+78 in the smoke), no CHECK FAIL, 0 HARD FINDING. The host check before every cell found no cargo / rustc / rust-analyzer
and no analysis process (1-min load 1.70-1.99). The 8 CELL-FLAG lines are all `md5` on GJ cells: **false positives.** The
GJ arm's staged `torus-node` is the wrapper, so `summary.json` records the wrapper's md5 (`e676f703`). The running exe is
`d6b05d8e` on all 3 nodes, and in every GJ cell `node-preload.txt` shows `LD_PRELOAD=/usr/lib/libjemalloc.so.2` with
`MALLOC_CONF` unset on 3 nodes and `node-maps-jemalloc.txt` shows libjemalloc mapped on 3 nodes (`extract.py`: 3 / 0 / 3).
The driver's `jemalloc` check passed in all 8. (Cosmetic: the G cells' "no preload" CHECK OK line says "jemalloc compiled
in", text copied from the M check; preflight's `nm` found 0 `_rjem_` symbols in the G node.)

### 65.1 Cells (`cells.tsv`, `analyst/extract.txt`, `analyst/writeup.txt`, the cells' `summary.json` / `cpu.csv`)

| cell (run order) | arm | bench window | matched/s | CPU-s/1M | node %CPU | load1 | native blk/s | place share | note |
|---|---|---|---|---|---|---|---|---|---|
| w0 (60 s) | moff | 02:33:37-02:34:40 | 223,630 | 26.03 | 331 | 20.6 | 3.63 | 0.628 | warm-up, excluded |
| r1-goff | goff | 02:38:06-02:40:09 | 115,666 | 47.05 | 386 | 19.5 | 3.95 | 0.286 | |
| r1-moff | moff | 02:44:18-02:46:20 | 225,609 | 25.74 | 399 | 20.3 | 3.49 | 0.612 | |
| r1-mon | mon | 02:50:06-02:52:08 | 208,993 | 27.11 | 379 | 20.0 | 4.06 | 0.617 | |
| r1-gjoff | gjoff | 02:55:43-02:57:45 | 128,667 | 47.19 | 419 | 25.3 | 6.49 | 0.204 | md5 flag (false) |
| **r2-moff** | moff | 03:02:29-03:04:32 | **190,808** | 26.81 | **357** | **36.9** | 2.89 | 0.611 | **excluded (owner): Codex stop review 03:01-03:04** |
| r2-mon | mon | 03:08:29-03:10:32 | 207,521 | 26.93 | 378 | 28.3 | 3.77 | 0.622 | |
| r2-gjoff | gjoff | 03:14:19-03:16:21 | 124,727 | 46.40 | 407 | 28.2 | 6.61 | 0.185 | md5 flag (false) |
| r2-goff | goff | 03:20:22-03:22:24 | 115,076 | 46.06 | 381 | 24.2 | 3.98 | 0.269 | localsearch window |
| r3-mon | mon | 03:26:35-03:28:37 | 210,081 | 26.58 | 379 | 25.0 | 3.66 | 0.623 | localsearch window |
| r3-gjoff | gjoff | 03:32:43-03:34:45 | 127,123 | 45.22 | 403 | 28.9 | 5.88 | 0.218 | localsearch window; md5 flag (false) |
| r3-goff | goff | 03:38:56-03:41:00 | 114,246 | 46.87 | 381 | 31.4 | 3.73 | 0.283 | localsearch window |
| r3-moff | moff | 03:45:06-03:47:09 | 216,505 | 26.39 | 400 | 18.7 | 3.42 | 0.613 | localsearch window; -4.0% vs r1-moff |
| r4-gjoff | gjoff | 03:50:55-03:52:57 | 132,016 | 45.57 | 419 | 24.1 | 5.71 | 0.223 | md5 flag (false) |
| r4-goff | goff | 03:56:56-03:58:59 | 115,280 | 46.24 | 377 | 23.5 | 3.76 | 0.272 | |
| r4-moff | moff | 04:03:08-04:05:11 | 224,399 | 25.67 | 401 | 19.6 | 3.42 | 0.613 | |
| r4-mon | mon | 04:09:04-04:11:07 | 217,950 | 25.97 | 360 | 20.5 | 3.94 | 0.623 | |
| r1-gjoff-rerun | gjoff | 04:14:29-04:16:32 | 123,338 | 49.25 | 427 | 38.3 | 9.10 | 0.144 | extra GJ replicate |
| r2-gjoff-rerun | gjoff | 04:20:55-04:22:58 | 127,783 | 45.75 | 420 | 31.9 | 7.09 | 0.185 | extra GJ replicate |
| r3-gjoff-rerun | gjoff | 04:26:53-04:28:55 | 126,657 | 46.75 | 416 | 33.9 | 7.65 | 0.177 | extra GJ replicate |
| **r2-moff-rerun** | moff | 04:33:09-04:35:11 | **222,015** | 26.00 | 402 | 27.8 | 3.48 | 0.612 | **replaces r2-moff** |
| r4-gjoff-rerun | gjoff | 04:38:45-04:40:48 | 129,323 | 45.41 | 413 | 28.0 | 6.22 | 0.198 | extra GJ replicate |

CPU-s/1M = node (user + sys) CPU over the load phase per 1M fills, mean of 3 nodes (`extract.py`). Node %CPU =
`summary.json` `cpu.pcpu_avg_by_proc`, mean of the 3 nodes. load1 = `cpu.csv` mean in the bench window (10 s samples; the
host has 32 CPUs). Place share = place / (place + cancel-all) executed (`ingest.econ_mix`). The G and GJ cells'
`summary.json` has `ingest.cloids: null` (the backported bench stamps no cloids); the M cells report 0 stamped.

### 65.2 Per arm (owner treatment: n = 4 G off / M off / M on, n = 8 GJ off; `secondary.txt`, `writeup.txt`)

| metric | goff (G, glibc) | moff (M) | mon (M, marks) | gjoff (G + LD_PRELOAD je) | M off / G off [95% CI] |
|---|---|---|---|---|---|
| **matched/s** | 115,067 | **222,132** | 211,136 | 127,454 | **1.931 [1.877, 1.985]** |
| CV across cells | 0.52% | 1.82% | 2.21% | 2.12% | |
| node CPU-s/1M fills | 46.55 | **25.95** | 26.65 | 46.44 | **0.557 [0.546, 0.569]** (-44.3%) |
| node %CPU (mean of 3 nodes) | 381 | 400 | 374 | 415 | 1.050 [1.035, 1.065] |
| engine ms per 1k fills | 4.70 | 3.33 | 3.48 | 4.58 | 0.709 [0.675, 0.746] |
| native blocks/s | 3.86 | 3.45 | 3.86 | 6.84 | 0.895 [0.851, 0.941] |
| fills per native block | 33,138 | 63,096 | 53,010 | 21,326 | 1.904 [1.813, 2.001] |
| bench actions submitted (120 s) | 207,602 | 170,573 | 165,423 | 367,552 | |
| place share (executed) | 0.278 | 0.613 | 0.621 | 0.192 | |
| nonce-expired evictions per node | 0 | 0 | 0 | 6,465 (5,179-7,767) | |
| block time (commit interval) p50 / p95 / p99 ms, context only | 79 / 981 / 2,043 | 74 / 882 / 1,625 | 103 / 899 / 1,645 | 41 / 672 / 1,226 | |
| RSS mean / peak per node, GB (kB / 10^6, as 61-63) | 3.00 / 3.63 | 4.83 / 6.26 | 4.89 / 5.94 | 2.69 / 3.38 | 1.610 [1.536, 1.686] / 1.724 [1.581, 1.876] |
| jemalloc allocated / resident at cell end, GB (bytes / 10^9) | n/a | 1.75 / 2.82 | 1.70 / 3.45 | not exported | |

Block time comes from the `torus_commit_interval_seconds` histogram in `buckets.csv` (bench window, interpolated in the
bucket, mean of 3 nodes; `extract.py`). It is context only: the attribution of the commit-interval p99 is a separate
follow-up analysis (running); no p99 cause is read from this campaign. RSS peak is the mean of the 3 per-node peaks in the
bench window (the analyst's handoff gives the largest node's peak in MiB: goff 4.0, moff 6.8, mon 6.4, gjoff 4.0; mean 2.9
/ 4.7 / 4.8 / 2.6).

### 65.3 Excluded and sensitive cells

* **r2-moff (190,808, -14.1% vs the other four M-off cells' mean, -15.4% vs r1-moff) is EXCLUDED by the owner.** The
  parent session's Codex stop review ran 03:01-03:04, overlapping r2-moff's bench window (03:02:29-03:04:32). That review
  is now off. In the window load1 averaged 36.9 (`cpu.csv`; `summary.json` `load1_avg` 36.1), against 18.7-20.3 in r1 / r3
  / r4-moff and 27.8 in the rerun. The nodes got less CPU: 357% vs 399-402% in the other M-off cells, and 2.89 native
  blocks/s vs 3.42-3.49. **r2-moff-rerun (222,015) replaces it.** Both treatments are in the ratio table. With r2-moff
  kept beside its rerun (n = 5), M off / G off is 1.876 [1.717, 2.042]. With only the 16 main-round cells (r2-moff in, no
  reruns) it is 1.863 [1.642, 2.104], and without the localsearch cells 1.850 [1.456, 2.336] (G off n = 2, M off n = 3).
  Every treatment is above 1.4x, including the lower CI bounds.
* **The localsearch indexer ran 03:19-03:47** (about 0.25 core on average; now masked) during r2-goff, r3-mon, r3-gjoff,
  r3-goff and r3-moff. Their values are inside the replicate spread and they are kept. Without them (owner treatment): M
  off / G off 1.940 [1.905, 1.975], M on / G off 1.831 [1.716, 1.954], GJ off / G off 1.104 [1.081, 1.128], M off / GJ off
  1.757 [1.718, 1.798] (table above; G off is then n = 2). **r3-moff is borderline: -4.0% vs r1-moff** (216,505 vs
  225,609), with normal node CPU (400%) and the lowest load1 of the campaign (18.7). Its window began 03:45, near the
  indexer's end.
* **Load in the GJ reruns.** load1 was 28.0-38.3 in the four GJ reruns (04:14-04:41) and 24.1-28.9 in the main GJ cells.
  No outside job is recorded for that time. load1 counts runnable threads, and GJ runs 6-9 native blocks/s with about 1.8x
  G's submitted actions, so part of it is the arm's own. Unlike r2-moff, the GJ nodes were not short of CPU (403-427%, the
  highest of any arm). r1-gjoff-rerun has the highest load (38.3) and the lowest GJ value (123,338). Without it, GJ off /
  G off is 1.113 [1.094, 1.132] and M off / GJ off 1.735 [1.688, 1.783] (`writeup.txt`). With the 4 main-round GJ cells
  only, GJ off / G off is 1.114 [1.073, 1.155].

### 65.4 Confounds and what the allocator split shows

* **Bench binaries differ.** G and GJ run the backported bench `a951801c` (old signing, `199bdc97`'s order format); M runs
  `ff08433d` (main's bench). The old node cannot take main's signed orders and the reverse, so one bench for both arms is
  not possible. The backport keeps the load shape (oracle-feed, in-flight cap and tail, budget, busy retry from main), but
  the two binaries are not proven to generate identical streams.
* **The action mix differs, partly as an outcome of node speed.** Place share 0.28 (G off) vs 0.61 (M off), and 0.19 on GJ
  off. The bench is closed loop (in-flight cap 4 per sender, released on commit; open-order budget 900), so what it sends
  next depends on what the node has committed. G off submits 22% more actions than M off (207,602 vs 170,573); 72% of the
  actions it executes are cancel-alls (M off 39%), and M packs 1.9x the fills into a native block. The matched/s ratio is
  the end-to-end result for this bench setup, not a node-only speed-up at a fixed action stream.
* **GJ is a different workload again.** It ran 1.8x G's native blocks/s at 0.64x the fills per block and submitted 1.8x
  G's actions. It is the only arm with nonce-expired evictions (5,179-7,767 per node per cell; in r1-gjoff the bench's
  7,122 in-flight timeouts match the 7,122 evictions per node, about 2% of the actions submitted). It is not a pure
  allocator swap at a fixed load.
* **Allocator split (as the analyst noted).** LD_PRELOAD jemalloc on `199bdc97` gives +10.8% matched/s [+8.8, +12.8] but
  no lower CPU per fill: CPU-s/1M 46.44 vs 46.55 (0.998 [0.973, 1.023]). Its nodes used 9% more CPU (415% vs 381%, 1.090
  [1.070, 1.109]). The GJ gain is more node CPU in use, not cheaper fills. The CPU-per-fill saving of M (-44%; 0.559
  [0.544, 0.574] vs GJ) is item 6's own. M off / GJ off, both on jemalloc, is 1.743 [1.695, 1.792].
* **Marks cost about 5% of throughput** on M: M on / M off 0.951 [0.918, 0.984]. Block time p50 rises 74 -> 103 ms (1.378
  [1.135, 1.694]), CPU-s/1M +2.7% (1.027 [0.998, 1.057]) and native blocks/s +11.8% at 16% fewer fills per block. There is
  no G-on arm to compare against (the old node computes no marks).
* **Memory.** M uses more RSS than G (mean 4.83 vs 3.00 GB per node, peak 6.26 vs 3.63), with jemalloc compiled in and
  15 s checkpoints. GJ is below G (2.69 / 3.38). The item 6 RAM gate was settled in sections 61-62 (live heap); this campaign
  does not change it.

### 65.5 Verdict

* **Gate 3 checkpoint: M off / G off 1.931 [1.877, 1.985]**, against the >= 1.4x target that the design sets for after all
  phases. This checkpoint (Phases 1-3 + step 6, main `d3cba5e2`) is already 1.93x like for like. The lower CI bound is
  above 1.4x in every treatment (lowest 1.456: the main-round cells only, without the localsearch cells, n = 2 vs 3). "As
  shipped" (M with marks vs G, which never computed marks) is **1.835 [1.774, 1.898]**.
* **Not node-only:** the bench binaries and the action mix differ between the arms (65.4). The ratio is end-to-end at this
  bench setup.
* **Allocator:** LD_PRELOAD jemalloc alone gives G about 1.11x matched/s, by using more CPU. The -44% CPU per
  fill and 1.74x over GJ are item 6.
* **Block time p99** (G off 2,043, M off 1,625, M on 1,645, GJ off 1,226 ms) is context only; the commit-interval p99
  attribution is a separate follow-up analysis.
* **Confidence: high** on the direction and on "above 1.4x" (n = 4 per arm, CV <= 2.2%, every treatment and sensitivity
  above 1.4x at its lower CI bound). **Medium** on the exact ratio, because of the bench and mix confound.

**Method.** The ratio is the ratio of arm means; the CI is a Welch t interval on the difference of mean log matched/s
(unpaired: rotated balanced rounds, not ABBA pairs; the design asks for paired alternating cells, and here every round
holds every arm in a rotated order). The secondary ratios in 65.2-65.4 use the same method on the per-cell values.

Analysis (`tools/ozarchy-g3-tools/analyst/`; outputs in `ozarchy-g3-run/analyst/` on ozarchy):
* `extract.py` (the analyst's per-cell extraction from each cell's `summary.json`, `agreement.jsonl`, state digests,
  `node-environ-trie.txt`, `node-preload.txt`, `node-maps-jemalloc.txt`, `cpu.csv`, `mem-samples.txt`, `metrics-after-*`,
  `buckets.csv` -> `cells.json`, stdout `extract.txt`), `stats.py` (ratios with Welch CIs in the three treatments, each
  with and without the localsearch cells -> `stats.txt`) and `secondary.py` (per-arm secondary metrics, load per cell ->
  `secondary.txt`). `writeup.py` (written for this section) adds the per-cell table, RSS in the sections 61-63 convention,
  jemalloc GB, evictions, the Welch ratios of the secondary metrics and the r1-gjoff-rerun sensitivity (`writeup.txt`).
* **Re-run check for this write-up:** the analyst saved the scripts and `cells.json` but no text outputs. `extract.py`,
  run with its output redirected to a scratch dir, reproduces `cells.json` byte for byte. `stats.py` and `secondary.py`,
  run on it, give `stats.txt` and `secondary.txt`. Those match the analyst's handoff numbers except for the units noted in
  65.2 (RSS and jemalloc "GB" there are MiB / 1000). The archive smoke check (`meta/g3.toml`) runs `stats.py`,
  `secondary.py` and `writeup.py` from the archived copy against the cells and matches `stats.txt`, `secondary.txt` and
  `writeup.txt` byte for byte (3/3; fence check 131/131).
* Driver `tools/ozarchy-g3-tools/ozarchy-g3-campaign.sh` with its generator `make-g3-driver.py`; md5s `md5s-all.txt`
  (ozarchy-g3s0's `md5s.txt`); `analyst/a-cells.py` is section 60's copy from the tools dir, not run here. The run's
  `matched-bench/` copy (= `d3cba5e2`'s) is not archived. The backport is on branch `bench/g3-base-199bdc97`.

## Open

- **Restart path (section 46, review log row 51):** (1) `a44bdde6` handles only SIGINT; SIGTERM (`systemctl
  stop`, `kill`) is a hard kill that replays from the marker. Owner: should main.rs also wait on SIGTERM?
  (2) The harness restarts val1 in place within ~1 s, which in 20 of 48 restarts left the gossipsub
  subscriptions un-exchanged (`NoPeersSubscribedToTopic`, mesh watchdog reconnects). Once (x1-s-TERM) this
  halted the chain for ~9 min (memory S395 wedge, arm-independent). Fix candidates: a stop / wait / start
  restart in `crash-kill.sh`, or explicit gossipsub peers among the validators. (3) The end-of-cell digest
  compares mark-based account fields more than 60 s after the oracle pause (false DISAGREE in 9 of 12 cells):
  digest accounts before books, or keep the feed alive until the account reads.
  **SIGTERM measured (section 52, review log row 57):** (1) is fixed on main `219f0c20` (graceful SIGTERM). Step 2
  `7b006739` and main replay 0 blocks in 12 of 12 SIGTERM kills; exit 7.0 s mean / 10.7 s max on step 2 (0.3 s per queued
  exec block; 1 of 8 over Docker's 10 s grace; never near the 60 s bound), checkpoint share ~0.15 s. (2) still open: the
  resubscribe stall hit 9 of 12 restarts (watchdog reconnect at 70-80 s) and stalled the chain 56-75 s once per cell
  (control too); it set the one 59.8 s kill -> first commit.
  **Fix benched (section 58, review log row 66):** `e3bb7cca` (stale-connection kick + 5 s / 1 s watchdog) shows no
  saturated regression (matched/s -0.86% [-2.42, +0.69], CPU-s/1M -0.39% [-1.79, +1.02]) and no false kicks. Its benefit
  was not measurable with these SIGKILL cells: the restarted node's network starts 11-30 s after the kill, after the survivors'
  10 s QUIC idle drop, so neither arm wedged (0 ghost connections in 64 survivor-kill pairs). A benefit bench needs
  section 52's SIGTERM cells or a SIGKILL restart with p2p up within ~10 s (section 46 y1-s-KILL wedged that way).

- **Mode 3 revisit, before mainnet, deep-book shape only** (section 36, plan 9.13, review log row
  41; owner via 18c s107: Classic on testnet, no book mode in genesis now). At the standard shape
  Classic vs mode 3 on one node (`1eced05c`), n = 2: matched/s 1.013x at 10 markets, 0.9645x at
  300 (not resolved); mode 3's flush per 1k fills 1.95x / 1.66x (off-chain; exec-thread handoff
  wait +0.95 / +2.9 ms per native block); save_books per fill flat. A mode 3 default then needs
  the book mode in genesis and the s450 mode-2 test gaps checked against mode 3.
- **Superseded (Phase 3 un-shelved by the owner, s111; see the entries below). Was: Phase 3: by the owner's rule, shelve
  (borderline; owner to confirm)** (sections 41 and 42, plan review log rows 46-47). The throwaway prototype (`proto/p3-coalesce` `682a8df5`, one checkpoint
  per 100 blocks) cuts node CPU-s/1M by 4.90% (all rows vs control) and by 5.07% (with the state hash
  on), pooled 4.99%. That clears zero, not the 5% line. At equal fills the saving is 2.1-2.5%,
  because most of the headline comes from +4.7% matched/s on a disk the 3 validators share. Books
  only: -2.98%. Disk numbers kept on file: bytes written per validator -51% (all) / -45% (books),
  WAL -56% / -50%; all rows hold ~0.8 GB more RAM per node. Digest measured ~0.3 CPU-s/1M. SIGKILL
  restart with all rows 21.3 s mean to the first commit (gate 60 s). To resolve the 5% line instead:
  4 more c / a pairs, or a fixed-load pair (section 42.7).
- **Phase 3 un-shelved (owner s111); disk isolation measured** (section 43, plan review log row 48). With
  every data dir on tmpfs (an UPPER BOUND, not a separate-disk result; ozarchy has one NVMe) the
  prototype still saves 4.8% node CPU-s/1M (5.2% on the shared NVMe): the CPU saving is work removed.
  The matched/s gain shrinks from +5.8% to +3.1% (not resolved), and the blocks/s gain from +14% to
  +1.6% (disk contention). Inference: a validator with its own disk lands between the two.
- **Phase 3 step 1: matched/s gate not decided yet** (section 45, plan review log row 50). Step 1 (`7c178637`,
  interval 15 s) meets CPU-s/1M down (-7.9% shared NVMe / -8.3% tmpfs upper bound) and native blocks/s no
  regression (+96% / +86%). matched/s measured -7.0% / -10.6%, but the bench is throttled by the step 1 RPC head
  lag (`eth_blockNumber` moves only at checkpoints, ~100 blocks; submissions stall 37 s per 120 s cell). Re-measure
  matched/s after step 2 (current RPC and mempool reads) or with a bench that releases in-flight slots on another
  signal. Restart <= 60 s and crash correctness not benched yet (they ride along with the step 2 bench). Results
  also valid for `a44bdde6` (byte-cap fix only; the cap was unset).
- **P2-3 worker pool (backlog): sized** (section 44, plan review log row 49). Main `4e7fa146` spawns 41
  threads per native block (exact perf FORK counts), all on the execution thread (match 32-way, settle,
  end-resident). A pool saves ~0.05-0.15 CPU-s/1M (ceiling 0.28) and ~0.9-1.4 ms per native block.
  **A/B done (section 50, review log row 55):** `perf/p2-3-worker-pool` `efcd1548` vs its merge-base `c97f56f9`, commit
  tail, n = 4: 0 forks per native block (perf) vs 44-46, all spawn counters 0; exited-thread sys -0.11 CPU-s/1M; matched/s
  -1.2% [-5.0, +2.6], CPU-s/1M +0.4% [-1.8, +2.6] (-0.7% at equal fills), exec wall per block +0.3% [-3.0, +3.7]: the
  expected ~0.4% gain does not resolve at this n, no regression seen. Merge decision is the owner's.
- **Phase 3 step 2 gate (commit-tail mode): owner decision needed** (section 47, plan review log row 52). Step 2
  `2296be67` vs main `c97f56f9`, n = 4, shared NVMe: CPU-s/1M -7.4% (-9.2% at equal fills, met), device writes -53% (met),
  matched/s -2.2% [-7.1, +2.7] (tie, not up), native blocks/s -9.4% [-18.6, -0.2] (borderline; -6.7% [-14.9, +1.4] without
  r3-cd), RSS +1.53 GB peak / +1.13 GB mean (RAM gate ~270 MB: not met). Both arms sit at the exec ceiling (busy 0.96 / 0.98,
  ~63 blocks behind commit). Open: (a) **RSS split done (section 48, review log row 53):**
  against the clean commit-mode control `2725e321` the layer adds +0.88 GB peak and the view +0.68 GB (Phase 3 +1.55 GB;
  live heap +0.25 / +0.32 GB: `L` 316 MB for a 147 MB logical layer, minus the control's 157 MB of frozen pending sets,
  and the view's slices 260 MB; about half of each delta appears only under glibc, i.e. allocator retention). The
  commit-mode backlog is another +0.83 GB (mempool and queued decoded blocks) and is in the control too. RSS still grows
  at the end of the 120 s cells. The RAM gate is still not met; (b) cancel-all nonce-window expiry: 0-27 per node per cell in the gate rerun
  (section 49; step 1 too, control 0), up to 1,351 in section 47; not fixed in `51a67f57` (repro only); (c) **resolved
  (section 49, row 54):** the untimed residual is now 23.9 ms `checkpoint_settle_wait` (the merge join) + 1.0 ms upkeep,
  +3.5 ms left untimed; frees run on `torus-ckpt-drop`; (d) **resolved:** the rerun's control is main `8efcb07a` = the
  merge-base. **Gate rerun (section 49, `51a67f57` vs `8efcb07a`):** CPU-s/1M -7.0% (-6.8% at equal fills), writes -51%,
  matched/s +2.9% [-3.1, +8.9] (tie), native blocks/s -5.5% [-10.4, -0.7], RSS +1.55 GB peak: RAM and blocks/s not met.
  The DISAGREE verdicts are the known slow-digest mark artifact (control too), not a divergence.
- **Phase 3 RAM: allocator A/B done, gate still not met** (section 51, plan review log row 56). Step 2 `7b006739` vs
  main `219f0c20`, glibc and jemalloc 5.3.1 (LD_PRELOAD, defaults), commit tail, n = 4: the Phase 3 RSS delta is +1.55 GB
  peak under glibc and +0.97 GB under jemalloc (bound ~270 MB). jemalloc on step 2: peak -0.32 GB [-0.53, -0.12], growth
  1.64 -> 1.01 GB/min, matched/s +3.6% [+2.2, +5.0], CPU-s/1M -1.1% [-2.6, +0.5]; on main it costs +1.5% [+0.9, +2.2]
  CPU-s/1M; offered load differs (je commits +15-18% faster, so the closed-loop bench submits +13-14% more), so the
  verdict uses CPU at equal fills: step 2 +4.0% [+1.6, +6.3] -> soak winner glibc (`SOAK_ALLOC=glibc`). Owner: whether to ship jemalloc (a global
  allocator or a packaging preload) is a separate decision. 3 of 8 step 2 cells hit the mark-only digest artifact (0 of 8
  control): the glibc step 2 digest is bimodal (17-21 s or 72-95 s per node).
- **Phase 3 soak: GROWTH FINDING (18c)** (section 53, plan review log row 58). Step 2 `7b006739` vs main `219f0c20`, glibc,
  600 s each, commit tail: step 2 RSS still +40 to +67 MB/min per node over the last 5 min (CIs exclude 0), main +3 to +11
  (2 of 3 CIs exclude 0); Phase 3 delta +1.98 GB per node at 10 min (gate ~270 MB). Both arms lose ~30% matched/s from
  minute 6: cancel-all nonce-window expiry (~18k per node per cell) starts at minute 2 **on main too**.
- **Phase 3 soak under jemalloc: growth = glibc retention** (section 53b, plan review log row 59). Same pair on jemalloc
  5.3.1 (defaults, LD_PRELOAD): step 2 RSS flat after ~2 min (minutes 5-10 mean vs 2-5: -133 / -139 / -27 MB per node; glibc
  +349 / +329 / +364), Phase 3 delta +0.68 GB per node (minutes 5-10 mean; +1.01 GB peak; glibc +1.86 / +2.00). Layer
  size and resting orders do not grow; RSS tracks neither. The minute-6 matched/s drop is margin rejections in the econ
  workload (105-155k orders/s from minute 6, all four cells), not the generator's rate, the node or the cancel-all expiry
  (which starts at minute 3). Open: positions vs open-order reservations as the margin user (per-sender split).
- **Phase 3 soak with the cancel-share fix** (section 53c, plan review log row 60). cs2 `41a95355` (= `de97682f` on step 2)
  vs step 2 `7b006739`, glibc, 600 s each: cancel-all expiry 0 per node (step 2 16,168), margin rejections 0 for the whole
  cell (step 2 107-134k/s from minute 7), matched/s 179-187k/s in minutes 6-10 (step 2 144k). The margin user is position
  IM, not reservations (order margin <= 0.5% of account value on all 50 digest senders in six soak cells), concentrated on
  low-address senders by the old address-ordered cancel share. **Open:** step 2 + fix RSS under glibc does not level off
  (+129 to +178 MB/min, delta +2.76 GB vs section 53's main), but the reference is confounded (main ran the collapsed
  workload). Next: glibc soak cs2 vs main + fix `de97682f` (staged), plus a jemalloc cs2 cell. Per-minute reason split
  needs the counters in 53c.4.
- **Fair Phase 3 RAM soak (fix in every arm)** (section 53d, plan review log row 61). main + fix `de97682f` (mf, glibc,
  control) vs `41a95355` on glibc (cs2) and on jemalloc 5.3.1 (jcs2), 600 s each, all ACCEPT AGREE, 0 expiries and 0 margin
  rejections in every cell (same book). Glibc: Phase 3 delta +2.62 GB per node (minutes 5-10 mean; +3.02 peak), still
  +120 to +158 MB/min (CIs exclude 0) while mf levels off: RAM rule not met. jemalloc: flat (difference slopes -18 to +10
  MB/min, CIs span 0) at +1.10 GB mean / +1.49 peak vs glibc mf (val2, equal exec queue: +0.95 / +1.34): met on the level,
  borderline on the mean, not at the peak. Same book and layer, glibc grows and jemalloc does not: consistent with glibc
  retention. **Open:** allocator choice vs the ~1 GB rule (owner); main + fix block-build select time +24-193% vs step 2 +
  fix (not explained); glibc cells still lose 10-13% matched/s from minutes 0-2 to 6-10 (input-bound exec), jemalloc holds.
- **Phase 3 baseline: tw** (sections 37-40, plan review log rows 42-45; owner s109): validators run
  trade history off + WAL budget 2048 MiB. The WAL compression knob is merged (`4a3f6546`) but off;
  it is decided again by a tw vs tw + WAL compression A/B after Phase 3. Section 40 (n = 4, vs b
  28.75 CPU-s/1M): tw -13.3% node CPU / +9.0% matched/s.
- **Unresolved: ~1% matched/s drift `1b389700` -> `3efff0d6`** (sections 34-35, plan review log
  rows 38-39): p3s0 0.9886x (-0.9 sd), p3s1 C / A 0.9857x (-1.3 sd), about -0.7% at each of the
  two steps, inside noise in each campaign. `3efff0d6` is accepted as the Phase 3 step 0 baseline
  (18c, section 35). Third data point (section 37): main `9b7e29b2` on mode 3 ran 186,504 ±
  3,163, 1.001x of p3s1 C (different node and genesis, not an A/B).
- **P2-1 gate missed** (section 30): phase 1 -0.44 ms per native block vs >= 1.9 ms; cancel-alls
  still visit 189 books, the sender has something in 92 (stale index entries, "never removed
  eagerly"). 18c s104: **A, accept and continue** (~0.07 us per skipped visit, 0.44 ms /
  (59.2 x 111), so B's further ~97 visits are worth ~0.4 ms, still below the gate); the background prune stays before mainnet
  (phase2 plan 9.14, review log row 25).
- **`d3ba3c0a` is 6.3% below `35e69b3`** on the standard shape (section 26,
  interleaved, same bench): not the 4 MiB book SSTs (C/B 1.003x), not the
  build style (D/B 1.014x), not the load generator. **Bisected** (section
  27): `a746c408` (ADL budget; `TraderPositions::apply` 1.8x per fill from C2's
  `set_holder`, ~38% of the gap) and `9e695364` (exact cost basis; settle
  pass B +10% and engine +4% per fill, ~44%). Profiled (section 28): pass B's
  extra time is all `PositionCache::merge_disjoint` (more DRAM fills, same
  instructions); match does not regress. Fixes A/B'd (section 29):
  `perf/c2-set-holder` `d7bd1c36` (`apply` per fill -36%, ~8-14% above the
  pre-a746c408 level; 18c review: merge as is) and 18c's
  `perf/position-v2-savings` `37b28dd6` (pass B per fill -11%, below p2's
  level); together 1.018x matched/s vs base, within noise. **Merged** to
  main (18c s104: merge as is) as `c8d25db8` (c2-set-holder) and
  `b8e3b606` (position-v2-savings), ozarchy s29.
- **Low priority, after Phase 2 (18c s104):** `TraderPositions::apply` per
  1k fills is still 0.229 after the C2 fix vs 0.201 / 0.213 before
  `a746c408` (section 29): find what is left of that step when convenient.
  The Phase 2 +7% gate stays measured against `d3ba3c0a`. **C2 holder sets
  (s107, `perf/c2-holder-hashset`):** per-market holder sets as foldhash
  `HashSet`, sorted only in `holders_with` (ADL). µbench
  `ubench_trader_positions_apply` (ABBA x3): `apply` 0.881x at 30% of the
  writes opening / closing a key, 0.944x at 5%, neutral at 0% (control);
  `ubench_adl` drain unchanged. Node estimate ~0.229 -> ~0.215-0.222 ms per
  1k fills, not measured in a campaign (phase2 impl review log row 40).
  18c s107: approved (Codex torus-adversarial: approve, no findings); no
  separate node bench, the Phase 3 step 0 re-profile reports the apply
  figure.
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
- Read precompile gas: **built and reviewed** (`bench/read-precompile-gas` `ae767806`, 18c s100: merge as is,
  merge pending after 18c's suites; `docs/perf/read-precompile-gas.md`). Owner s99 final: base 16,400
  (getPosition 16,500; cold 30M-gas block 170.7 -> 33.7 ms); scans 500 gas per scanned row + 20 per word or
  32 B blob; `getOpenOrders` removed (never worked on a live chain: nothing writes `cf_native_orders`);
  `getOrderBook` 64 best levels per side in modes 2/3 (worst block 18.7-24.5 ms), mode 1 reverts, classic
  unchanged; RocksDB tombstones never charged; node-local per-market compaction (threshold 64,
  `ForceOptimized`). Stall / SST campaign (`bench/read-gas-stall` `8e3326c6`): no write stalls; owner s100:
  book CF SST target 4 MiB (1-2 MiB only with a raised `LimitNOFILE` or bounded `max_open_files`).
  **Follow-up branch after the merge** (ozarchy): the 4 MiB default, the write-time book layout check (the
  EVM reader skips it, owner s100), 18c's lows (compaction errors counted as success, shared-DB cancel on
  drop) and 3 missing tests, nits, decisions in the doc.
- **Before mainnet: `getOrderBook` from the in-memory book** (owner s100). The previous block's tombstones
  (up to 200k cancels in one block, uncharged) are re-accepted for testnet only. The in-memory reader removes
  them: byte-identical on every validator (the same top 64 per side in modes 2/3, the block's own changes
  included, correct rebuild after restart and crash replay, no lazy row-scan fallback). Fallback if it slips:
  a per-market cap on level deletes per block (a new validity rule).
- **Done** (`f7fe17f3` on main): test hygiene, the torus-consensus crash-test fixture (`app.rs:7356`)
  created a ~6 MB RocksDB dir per run under `/tmp` (`torus-crash-test-<pid>-<n>`) and almost never removed
  it. Test scratch DBs now live under `/tmp/torus-consensus-test-dbs/<pid>/`, and the dirs of exited
  processes are swept once per process.
- **Item 7 EVM lanes** (owner s99): every Torus block can carry 30M EVM gas
  next to trading, and any EVM tx sends its block down the serial path. HL
  rations EVM (2M-gas small blocks every few seconds, 30M-gas big blocks about
  once a minute). Design by 18c (`docs/plans/item7-evm-lanes.md`); build after
  item 6 Phase 2, before Phase 3 and before testnet.
- Liquidator vault capital (owner s99): seed it through genesis on devnet and
  testnet; HL-style user deposits (deposit, withdraw, pro-rata PnL) before
  mainnet; one vault, no separate insurance fund.
- **Margin currency: OPEN owner decision.** Margin is TRS counted as $1 while
  markets are quoted in USD (no conversion, no stablecoin collateral). Options:
  USDC margin like HL (18c recommendation), TRS with a TRS-USD price and a
  haircut, or markets quoted in TRS (item6 plan 9.16).
- **9.11 real fix: OPEN, before mainnet.** Maker over-commit is possible under
  flat tiers: split each maker's free margin per book, tighten the 50 % band,
  or serialise makers resting in several markets. The
  `torus_maker_offmark_charged_fills` counter gives the data.
- **P0 before testnet: ADL has no per-block work budget** (section 23.2): at
  S=750, 100 accounts x ~270 markets of ADL took 332 / 123 / 241 s on three
  blocks and froze consensus ~11.6 min. Needs a budget (account-markets or
  closes per block) with carry-over, and cheaper per-close work. Profiled and
  designed in s18 (`docs/plans/adl-budget.md`, owner decisions Q1-Q6 pending).
  **Budget built** (`perf/adl-budget` @ `6a25e20`, section 24): S=750 passes
  (rc 0, AGREE, 22 s drain, vault -0.00084). Still open: the heaviest
  liquidation block is ~1.1-1.2 s vs ~250 ms. **Merged** to main as
  `a746c408` (18c s99): C2 holder lists, W = 100,000, ranking charged by the
  market's holders, block B charged into W (`docs/plans/adl-budget.md`
  sections 10-13); main needs a fresh devnet genesis. Re-measure (section
  13): the only block over ~250 ms rig was HL 100k 10 % block B (135-138 ms
  ozarchy). **Fixed by `perf/adl-dirty-check` @ `f859907f`** (node-local,
  units identical; 18c accepted at 128.8 ms, s99, merge pending): A/B block B
  149.6 -> 128.8 ms (`docs/plans/adl-dirty-check.md`).
- Optional ADL speedups (18c s99: not now). The largest remaining shares of HL
  100k 10 % block B after the dirty check: the `adl_rank` sort (~28 %),
  `get_native_balance` for the candidates (~13 %), and the records binary
  search in `resident_positions` (~12 %; the holder list could hand the
  ranking R's record slice). All node-local.
- ADL proof cells (section 24.2), 18c s99: vault -0.00074 / -0.00084 accepted
  as dust; the escrow dust lines are the sweep itself (both escrows end at 0
  positions and 0 balance, the vault gets exactly the dust; pinned by a test on
  `perf/adl-budget`). The value sum drifts ~-0.2 units of 1e-8 per trade fill
  (likely `fill_transition`'s truncating average entry): 18c backlog; until
  then value-sum checks across fills allow ~fills x 1 unit. Stale marks moved
  the old sum (-1,740.69 after the feed pause); C2's fix (c) prices at 0. Harness: the post-pause drain (60 s) is too short for value-sum
  cells; `funnel-val*.csv` header is one column short.
- Liquidation stress at S=400: all backstop is explained (section 23.2, AV/MM
  ~0.40 after the shock). A stage-1 cell needs a shock of ~290 bp on this
  shape.
- Harness: fix `liq_stress.py` (`_count` column), keep harness tests from
  writing `testnet/genesis-weighted-full.json` into the worktree, and avoid
  stale binaries from reflink-seeded target dirs (section 23.2).
- Shared-DB shutdown path untested (18c s101, not blocking): in production the DB is shared, so when
  RocksDB is mid-compaction the drop waits for it, and a slow range at shutdown could run past
  systemd's `TimeoutStopSec`.
- Nit (`crates/torus-state/src/db.rs:164-192`): if the background-errors read before a range run fails
  (`unwrap_or(0)`) and older errors are seen after it, the run counts as a false failure.
- Nits (`crates/torus-bridge/tests/book_read_modes_tests.rs`): the assert message at :485 says "root
  CF" but the check is level mode; `key[..8]` at :463 panics on a key shorter than 8 bytes.
- Flaky test `liquidation_tests::p2_counterparties_are_paid_at_the_stored_price` ("one close per row",
  6 vs 8): once in a full `cargo test` run; passes alone and under nextest. Likely cause: the
  `Captured` event capture listens only on its own thread, and tracing caches per-callsite interest for
  the whole process. Possible fix: `register_callsite` -> `Interest::sometimes()`.
- **Done** (merged to main as `35953ff8`, 18c s101): `fix/raise-nofile-limit`. 4 MiB book SSTs are
  ~256 files per GB; torus-node raises its soft `RLIMIT_NOFILE` to the hard limit at start-up.
