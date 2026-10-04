# ozarchy 2026-10-04: anti-spam, item 6 sync 2, C1 full node, 10-market profile, PF1, C3 + PF1, 14236fa baseline, trie and gap analyses

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

## Open

- Native trie maintenance off by default: owner question on the
  `*_with_native` paths (section 8.1).
- IPC / LLC-miss measurement on the execution thread, crab vs main, to
  explain the ~1.6 ms/1k of shared-code inflation (section 8.2).
- E1 (oracle step) postponed, low priority: 5.8 ms per empty block on
  ozarchy (section 7.3).
- Margin and liquidation at 300 markets: `positions_for_trader` overlay
  range scans per account (section 6.2), the target for the next item 6
  step.
- Optional: cheaper shed path (peek the action tag or const-hex) and the
  1.22-1.27x signature-verify cost per admitted action on crab (section
  6.4).
- Gate 4 after C4 + PF1: ubench tail at 300 markets, `ubench_epoch
  UB_DRAIN=fresh` empty block (target <= 20 ms).
- Anti-spam D (per-IP RPC limit) has no validator exemption; no metric for
  oracle submissions evicted inside the pool.
