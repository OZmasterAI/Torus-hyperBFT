# ozarchy 2026-10-04: anti-spam, item 6 sync 2, C1 full node, 10-market profile, PF1, C3 + PF1, 14236fa baseline, trie and gap analyses, C6 + C7, margin phase breakdown, `239ff69`, per-action results (C), M1 (`90a752c`)

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
  `BlockSums::into_cache` re-decoding positions; untimed, needs a metric
  (section 14.2).
- A main pair interleaved with crab cells, trie off, to tighten the
  0.638x / 0.648x / 0.760x ratios (sections 10.1, 12.1, 14.1).
- Cheaper hasher for the Address-keyed maps: the +0.46 ms/1k SipHash of
  `239ff69` (section 12.2) is gone after M1 (section 14.3);
  `hash_one<&Address>` is still 0.94 ms/1k.
- C: zero-fill IOC and crossing post-only orders still show "executed",
  and reason codes are parsed from error text; both need
  `native_executor.rs` (section 13.2).
- Optional: cheaper shed path (peek the action tag or const-hex) and the
  1.22-1.27x signature-verify cost per admitted action on crab (section
  6.4).
- Gate 4 after C4 + PF1: ubench tail at 300 markets, `ubench_epoch
  UB_DRAIN=fresh` empty block (target <= 20 ms).
- Anti-spam D (per-IP RPC limit) has no validator exemption; no metric for
  oracle submissions evicted inside the pool.
