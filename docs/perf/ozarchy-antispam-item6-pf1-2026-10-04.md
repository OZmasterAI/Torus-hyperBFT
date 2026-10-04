# ozarchy 2026-10-04: anti-spam, item 6 sync 2, C1 full node, 10-market profile, PF1, C3 + PF1

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

## Open

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
