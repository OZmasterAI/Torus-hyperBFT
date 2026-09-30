# s77-s80: CPU profile, trade-history cost, packed rows, trade streams — 2026-09-30

All cells: 3-validator loopback devnet on one 18-vCPU host, 10 markets, 300 s,
cap 400, s76 defaults (512 RPC conns, bench on all 3 validators, CONC 256),
env `TORUS_BODY_FETCH_TRACE=1 TIMEOUT_BASE_MS=1200 TORUS_BODY_SERVE_THREAD=1
TORUS_DEFER_PARENT_FEED=0`. Rate 76,000 (full load) unless noted.
CPU-s/1M = node CPU-seconds per 1M matched orders per node over bench +
drain, from the host sampler's `pidstat -t` (`score.py`).

## Summary

- **Profile (s77).** Trade history, not signature checks, was the largest
  removable cost. Storage threads were ~38% of node CPU; Keccak was 17%, most
  of it EIP-712 struct hashing on the verify threads.
- **Hash dedup (s77, merged `0ff946e`).** Gossip-verify CPU/1M -25%,
  ingress-verify -19% (disjoint ranges, 4 cells per arm). Node CPU/1M -5%,
  within the overlap of the two arms.
- **Trade-history cost (s77).** Full history: 68.4k matched/s, 70.4
  CPU-s/1M (5-cell mean). History off: 102.8k, 41.9. Per-market rows only:
  92.7k, 50.8. The cost follows the number of rows, not key order.
- **Packed rows (s78, merged `d930405`).** 99.1k matched/s, 46.0 CPU-s/1M
  with full history on: the history cost falls from 28.5 to 4.1 CPU-s/1M.
  One cell only.
- **Latency (s77).** At half load, fills are visible at p50 241 ms and p99
  3.7 s. At full load the exec queue sits at its 64-block bound, so exec
  age is ~71 s of queueing. The exec throttle (`8,16,24`) is not adoptable
  (-44% total matched).
- **Streams (s80, merged `7ebcfa3`).** The valid interleaved A/B (s80ab2,
  3+3 cells + warm-up) shows branch = main: +2.1% matched/s (p=0.67), -3.7%
  CPU-s/1M (p=0.40). The earlier s80ab "main 45% slower" came from the load
  generator submitting too little, not from node code.

## 1. CPU profile (s77, `s77-prof-20260929`)

One cell on main `07a34c2` (before hash dedup). `perf record -a -F 99` for 60 s
over the whole host, started 90 s after the bench began, plus a 15 s dwarf
call-graph capture on the 3 node pids. During the 60 s window the nodes used
11.56 cores. They matched 63.9k orders/s (mean of 3 nodes; val0 64.3k), so
~60 CPU-s/1M. That is lower than the whole-cell 79.7 because the window
leaves out ramp and drain.

| thread (3 nodes summed) | cores | share of node CPU | top symbol in thread |
|---|---|---|---|
| torus-execution | 2.87 | 24.8% | libc (unresolved) 17.6%, `__KeccakF1600` 10.7% |
| rocksdb:low (compaction) | 1.91 | 16.6% | `ZSTD_compressBlock_doubleFast` 14.1% |
| torus-gossip-verify | 1.72 | 14.9% | `__KeccakF1600` 57.7% |
| torus-trade-writer | 1.17 | 10.1% | libc 23.8%, skiplist `RecomputeSpliceLevels` 17.5% |
| torus-ingress-verify | 0.87 | 7.5% | `__KeccakF1600` 48.8% |
| tokio-rt-worker | 0.83 | 7.2% | `sha512_compress` 14.9% |
| torus-flush-worker | 0.70 | 6.1% | libc 36.4% |
| rocksdb:high (flush) | 0.67 | 5.8% | `LZ4_compress_fast_continue` 13.2% |
| hotstuff-algo | 0.36 | 3.1% | `__KeccakF1600` 33.1% |
| rpc-worker | 0.35 | 3.0% | `__KeccakF1600` 21.2% |

Top node symbols (flat, all node threads): `__KeccakF1600` 17.03%, unresolved
13.45%, `ZSTD_compressBlock_doubleFast` 2.45%, skiplist
`RecomputeSpliceLevels` 2.09%, k256 `ProjectivePoint::add` 1.48%,
`OrderBook::chunked_top` 1.38%, `crc32c` 1.15%, `sha512_compress` 1.07%.
Keccak by thread: gossip-verify 1.04 cores, ingress-verify 0.44, execution
0.32, hotstuff-algo 0.13, rpc 0.08.

- Storage threads (rocksdb:low + trade-writer + flush-worker + rocksdb:high)
  are 38.6% of node CPU. The two verify threads are 22.4%.
- On the verify threads the cost is Keccak, not secp256k1. The top k256
  symbols (point add, double, field square) are ~11% of gossip-verify. The
  memory record from the session puts this at 1.0 ms per 400-order action
  (EIP-712 struct hashing of the order batch). That timing is not
  reproduced here.
- On the execution thread, std `RandomState` SipHash (`hash_one` 5.0% +
  `DefaultHasher::write` 3.8%) is ~9%.
- The dwarf capture lost 17.28% of its events (`dwarf.err`), so only the
  flat profile is used for the shares above.

## 2. Hash dedup A/B (s77, `s77-hash-20260929`)

main `07a34c2` vs `perf/s77-hash-dedup` `74d488f`: hash the batch-item
typehash once instead of per order, and compute one trust-cache key per
gossip ingest. ABBA order, 4 cells per arm plus a warm-up on the hash arm
(`hash-w1`, 58.0k, 80.5 CPU-s/1M, excluded). 8/8 cells accepted, AGREE, PASS.

| metric | main (r1-r4) | hash (r1-r4) | change |
|---|---|---|---|
| matched/s | 59.1 / 59.7 / 69.2 / 55.2k (mean 60.8k) | 70.7 / 67.7 / 68.4 / 67.7k (mean 68.6k) | +13%, ranges overlap |
| total matched | 24.0 / 25.7 / 29.3 / 23.2M (mean 25.6M) | 29.5 / 29.4 / 27.4 / 27.8M (mean 28.5M) | +11% |
| node CPU-s/1M | 76.5 / 73.8 / 69.3 / 79.3 (mean 74.7) | 73.7 / 68.4 / 68.9 / 73.1 (mean 71.0) | -5%, ranges overlap |
| gossip-verify CPU-s/1M | 8.5-9.9 (mean 9.33) | 6.8-7.2 (mean 7.03) | -25%, disjoint |
| ingress-verify CPU-s/1M | 4.3-5.1 (mean 4.73) | 3.7-3.9 (mean 3.83) | -19%, disjoint |

The throughput gain is larger than the CPU saving can explain, so part of it
is probably cell noise. Merged and pushed as `0ff946e`.

## 3. Trade-history cost (s77-s78)

Every fill wrote three RocksDB rows: one per-market row and two per-user rows
(maker and taker). Both column families are node-local and outside the
consensus root. Reference "full history" = s77-hash hash-r1..r4 +
s77-lat-lat-r1 (main with hash dedup, 5 cells).

| setup | rows | cells | matched/s | total matched | best 60 s | node CPU-s/1M | ms / committed block |
|---|---|---|---|---|---|---|---|
| full history | 3 per fill | 5 | 67.5-70.7k (mean 68.4k) | 27.4-29.5M | 96.0-115.2k | 67.9-73.7 (mean 70.4) | 826-1128 |
| per-market rows only (`TORUS_TRADE_HISTORY=trades`, exp `523c26c`) | 1 per fill | 1 | 92.7k | 37.0M | 122.7k | 50.8 | 691 |
| packed rows (s78, `b9b0c5a`) | 1 per market / block / 1024-fill chunk + 1 per trader / block | 1 | 99.1k | 38.1M | 121.2k | 46.0 | 749 |
| history off (`TORUS_TRADE_HISTORY=0`, `01a04fb`) | 0 | 1 | 102.8k | 40.4M | 158.6k | 41.9 | 642 |

Per-thread CPU-s/1M:

| setup | rocksdb:low | trade-writer | execution | rocksdb:high |
|---|---|---|---|---|
| full history (5 cells) | 11.9-13.3 | 6.2-7.6 | 6.1-8.9 | 3.5-4.2 |
| per-market rows only | 6.9 | 1.3 | 5.0 | 1.9 |
| packed rows | 5.0 | 1.5 | 4.7 | 1.7 |
| history off | 2.6 | 0.0 | 4.4 | 1.0 |

- Full history costs 70.4 - 41.9 = 28.5 CPU-s/1M, ~40% of node CPU. Most
  of it is compaction (`rocksdb:low`) and the trade writer.
- Per-user rows (2 per fill) cost 70.4 - 50.8 = 19.6, and per-market rows
  (1 per fill) cost 50.8 - 41.9 = 8.9. That is ~9-10 CPU-s/1M per row in
  both tables. The per-market keys are sequential and the per-user keys are
  random, so key order barely matters. What matters is the row count. The
  planned sort-before-write option (`trade-history-writes.md` option B) was
  therefore dropped for packing.
- Packed rows cost 46.0 - 41.9 = 4.1 CPU-s/1M, so ~86% of the history cost
  is gone while every RPC history method keeps working.
- The bench is closed-loop, so it offered more to the faster cells: 90.7k-97.3k
  actions submitted in the full-history cells, against 126.0k (split),
  128.1k (packed) and 135.7k (off). Part of the matched/s gain comes from
  that.
- Execution still saturates in every one of these cells. From 130 s until
  the bench ended, the exec queue averaged 60.6-64.8 blocks (off),
  59.6-65.0 (packed) and 63.6-64.5 (s77-lat).
- The history-off cell evicted 1,259-1,287 nonce-expired actions per node,
  packed 1,462-1,512 and split 3,389-3,421. The full-history cells evicted
  0-73.

Packed-row format and decisions (`docs/plans/packed-trade-rows-impl.md`):
per-market rows are chunked at ≤ 1024 fills, and each trader gets one row
per block. Rows are encoded off the exec thread. A format marker
(`trade_history_format = 2`) wipes the old-format history once on first
start. A review found no production bugs. Four fixes (`d62a121`) went in
before the merge.

## 4. Order latency (s77)

`torus_order_age_*` histograms (merged `01d2ee5`) measure age since the order
nonce at 5 stages. The values below are for val0, in the order p50 / p90 / p99.

| stage | full load, `s77-lat-lat-r1` (67.5k matched/s) | half load, `s77-low-low-r1` | exec throttle 8,16,24, `s77-thr-thr-r1` |
|---|---|---|---|
| admit | 16.2 / 31.5 / 40.0 s | 8 / 24 / 127 ms | 12.0 / 18.8 / 20.3 s |
| commit | 18.3 / 35.9 / 41.5 s | 160 / 568 / 2201 ms | 14.0 / 24.0 / 73.1 s |
| exec | 71.5 / ≥81.9 / ≥81.9 s | 213 / 673 / 2462 ms | 22.9 / 38.8 / 73.1 s |
| durable | 72.3 / ≥81.9 / ≥81.9 s | 256 / 806 / 2641 ms | 23.6 / 38.9 / 73.1 s |
| fills visible | ≥81.9 s (all quantiles) | 241 / 900 / 3721 ms | 31.2 / 61.9 / 79.9 s |

- **Full load.** The exec queue first reaches 60 at t≈128 s. From 130 s to
  the end it stays at 60-66 on every node, so it is pinned at the 64-block
  exec channel (`app.rs` `sync_channel(64)`). Exec trails commit by ~53 s
  at p50. That is queueing, not pipeline latency. The s77-lat binary capped
  the histogram at 81.92 s (`≥81.9`); `f936792` widened the buckets to
  ~11 min.
- **Admit age at full load is a bench artifact.** 5,000 senders share 256
  in-flight requests (`--senders 5000 --concurrency 256`), and each sender
  signs ahead into a depth-4 buffer. A sender's turn therefore comes round
  about every 16 s.
- **Half load.** Rate 100 actions/s is ~40k orders/s offered, with
  `SENDERS=50`. The cell ran 24.3k matched/s at 22.1 blk/s, with 4.3 txs
  per block and 14 view timeouts, and drained in 13 s. Pipeline latency end
  to end is ~0.25 s at p50. The p99 tail of 2-3.7 s appears from commit
  onward, which fits view timeouts (base 1200 ms). Its CPU-s/1M (130.2) is
  dominated by per-block fixed costs and is not comparable with
  loaded cells.
- **Trade writer.** It had 60-84 batches queued under full load. An earlier
  note that 74 batches were still queued at drain end was wrong: the gauge
  was stale and has since been fixed.

### Exec throttle (`TORUS_EXEC_THROTTLE_WATERMARKS=8,16,24`, one cell): not adoptable

| | full history reference (5 cells) | throttle |
|---|---|---|
| matched/s | 67.5-70.7k (mean 68.4k) | 50.5k (-26% vs mean) |
| total matched | 27.4-29.5M (mean 28.5M) | 15.9M (-44%) |
| node CPU-s/1M | 67.9-73.7 | 89.5 |
| blk/s, actions per exec block | 0.9-1.2, 240-271 | 8.3, 28.2 |
| nonce-expired evictions / node | 0-73 | 9,011-9,060 |
| exec queue, mean over 130 s..end | 58.6-64.5 (hash-r1, lat) | 43.5-46.7 (max 66, touches 0) |

The throttle counts exec backlog in **blocks** and shrinks each proposal as
the backlog grows: half caps at 8, quarter at 16, cancels only at 24.
Smaller blocks mean more blocks, and each block pays fixed exec costs. The
queue depth barely falls while throughput collapses. Exec age did improve
(p50 71.5 → 22.9 s).

`TORUS_PROPOSER_EXEC_WATERMARK` (off by default) is not the same lever. It
watches the consensus-side body backlog (`pending_headers +
deferred_bodies` in `hotstuff_rs/.../implementation.rs`), not the exec
queue.

## 5. Trade streams (s80)

`newTrades` / `userFills` websocket streams are fed from execution fills on
the exec thread and sent as one message per block. Fill extras are recorded
only while someone is subscribed. All-markets `newTrades` is off on
validators unless `TORUS_ALL_MARKET_TRADES=1` is set. See
`docs/api/streams.md` and `docs/plans/trade-streams.md`.

### 5.1 Task 9 cells (`s80-streams-20260930`, branch `7340255`)

| cell | when | matched/s | total | CPU-s/1M | exec CPU-s/1M | engine ms / 1k fills | status |
|---|---|---|---|---|---|---|---|
| s78-packed-packed-r1 (reference) | 09-30 01:31 | 99.1k | 38.1M | 46.0 | 4.7 | 6.29 | accepted |
| streams-r1 (no subscriber) | 05:48 | 89.8k | 36.3M | 49.0 | 5.3 | 6.93 | accepted, AGREE |
| sub-r1 (1 all-markets `newTrades` + 5 `userFills` on val0) | 12:27 | 37.0k | 12.7M | 92.9 | 8.8 | 9.83 | INVALID (dissemination failures), AGREE |
| streams-r2 (no subscriber, control) | 12:39 | 75.0k | 31.4M | 54.6 | 6.0 | 8.0 | INVALID (dissemination failures), AGREE |

- streams-r1 missed the plan's bounds (≥ ~94k matched/s, ≤ ~48 CPU-s/1M).
  The only comparison was a cell from another time of day.
- The subscriber cell is confounded: the no-subscriber control run right
  after it was also rejected. In the subscriber cell, `newTrades` arrived
  p50 0.9 s / p99 12.7 s after exec finished the block. The largest message
  was 33.2 MB of JSON, and 3.2 GB were sent in total. `userFills` arrived
  p50 28 ms / p99 0.50 s after exec done. rpc-worker CPU-s/1M (averaged
  per node) went to 12.3, against 2.0 in r1.
- Two follow-up fixes came from this cell. `e80d08a` records fill extras only
  while subscribed. `7117269` serializes each payload once per filter and
  gates all-markets `newTrades`. No subscriber cell has been run since
  these fixes.

### 5.2 First A/B (`s80-ab-20260930`): false "main 45% slower"

main `d930405` vs branch `6950f21`, interleaved, no subscriber. The owner
stopped it after 2 pairs.

| cell | start | matched/s | CPU-s/1M | blk/s | fills / native block | bench submitted actions/s | first ≥60 s progress line |
|---|---|---|---|---|---|---|---|
| warmup-r0 (main) | 14:26 | 61.5k | 68.1 | 1.2 | — | 257 | — |
| main-r1 | 14:37 | 61.3k | 65.5 | 1.8 | 43,119 | 262 | 185 |
| branch-r1 | 14:48 | 86.0k | 53.0 | 1.3 | 76,253 | 373 | 467 |
| main-r2 | 14:59 | 60.3k | 61.8 | 1.8 | 46,554 | 291 | 224 |
| branch-r2 | 15:09 | 90.8k | 49.0 | 1.3 | 81,367 | 421 | 408 |

The main cells had half-full blocks, a higher block rate, and gossipsub
"Send Queue full" warnings (in both main cells only). On its face, that
reads as a +45% branch gain. The slow main cells got little load from the
start. From 5 s to 60 s, their cumulative submit rate (bench progress
lines) stayed between 119 and 276 actions/s. From ~10 s on, the fast cells
(s80ab branch, s80diag main and noscan r1) ran at 390-546/s. The warm-up
(also the main binary, 257/s) was slow too.

### 5.3 Diagnosis (`s80-diag-20260930`): not reproduced

Three arms, interleaved, 2 cells each plus a warm-up:

- main `d930405`: the same node binary as s80ab (md5 `055f5e04`).
- noscan `5270a3f`: main without the commit-time `scan_trades_for_block`
  (experiment only).
- s78bin `b9b0c5a`: the s78-packed binary.

| arm | matched/s | mean | CPU-s/1M | bench submitted actions/s |
|---|---|---|---|---|
| main | 94.4k, 89.4k | 91.9k | 47.9, 48.3 | 429, 396 |
| noscan | 97.9k, 90.7k | 94.3k | 45.5, 49.5 | 447, 414 |
| s78bin | 84.3k, 92.1k | 88.2k | 52.3, 49.2 | 377, 417 |

All 7 cells were accepted, AGREE. No pairwise matched/s difference is
significant (Welch p 0.37-0.64, n=2). The same main binary that ran
~61k in s80ab ran 89-94k here. The warm-up ran 87.3k.

The cause of the s80ab slow state is still unknown. Both campaigns used
the same bench-throughput binary (md5 `2c01ddb8`). In s80ab, all three
main-binary cells were slow and both interleaved branch cells were fast.
With 5 cells, that alignment may be chance. None of the 14 later cells
(s80diag, s80ab2) submitted below 373 actions/s. One of them (s80ab2
branch-r1) was slow at 60 s (322/s) but recovered to 396/s.

### 5.4 Valid A/B (`s80-ab2-20260930`): equal

main `d930405` vs branch `6950f21`, interleaved r1..r3, plus a warm-up on
main (92.9k, excluded). All 7 cells were accepted, AGREE, with 0
exhaustions. None was LOADGEN-SUSPECT: the campaign's median submit rate
was 404 actions/s and the threshold 323/s.

| metric | main (n=3) | branch (n=3) | branch - main |
|---|---|---|---|
| matched/s | 89.4 / 85.3 / 93.4k (mean 89.4k, CV 4.5%) | 86.2 / 90.1 / 97.3k (mean 91.2k, CV 6.2%) | +2.1%, Welch p=0.67; paired -3.6 / +5.6 / +4.2% |
| matched/s incl. drain | mean 89.5k | mean 95.2k | +6.4%, p=0.23 |
| total matched | mean 36.2M | mean 37.0M | +2.1%, p=0.57 |
| node CPU-s/1M | 49.4 / 52.1 / 48.1 (mean 49.9) | 50.5 / 48.5 / 45.1 (mean 48.0) | -3.7%, p=0.40 |
| exec-thread CPU-s/1M | mean 5.11 | mean 4.99 | -2.3%, p=0.61 |
| bench submitted actions/s | 398 / 373 / 409 | 396 / 412 / 443 | +6.0%, p=0.25 |

The streams branch without a subscriber costs nothing measurable. Merged as
`7ebcfa3`. Across the s80ab2 cells, submit rate and matched/s correlate
strongly (r = 0.93, from the s80ab2 memory record; not recomputed here).
Part of that is backpressure, because the bench is closed-loop. Against
the s80ab2 threshold, both s80ab main cells (262 and 291/s) are
LOADGEN-SUSPECT.

## Decisions

- Merged: hash dedup (`0ff946e`), order-latency histograms (`01d2ee5`),
  `TORUS_TRADE_HISTORY=0` switch (`826bcf3`), packed trade rows (`d930405`),
  trade streams (`7ebcfa3`).
- Not merged: exec throttle watermarks as configured (the env var remains,
  off by default); `TORUS_TRADE_HISTORY=trades` (experiment `523c26c`
  only); noscan (experiment `5270a3f` only).
- Bench practice from s80:
  - Record bench submit rate (the final `Submitted (load-gen accepted)`
    line in `bench.log`) and `placed_s_avg` for every cell.
  - Flag cells below 80% of the campaign median.
  - Compare only interleaved arms from the same window, ≥3 cells each, plus
    a throwaway warm-up cell.
  - Never compare against a cell from another campaign.

## Caveats

- Single 18-vCPU host, CPU-saturated. Nodes and bench share the box, and
  load is closed-loop (CONC 256), so offered load follows node speed.
- Cells before s76 are not comparable with later cells: s76 changed the
  RPC max connections and the `BENCH_RPCS` default. All cells here are
  post-s76.
- Cell-to-cell variation within one campaign window is ~4-6% CV (s80ab2
  4.5% / 6.2%, s80diag 3.9-6.2%). The same binary ran 60.3k at 14:59
  (s80ab main-r2) and 94.4k at 15:49 (s80diag main-r1).
- The trade-history split, packed, history-off, throttle and latency
  results are single cells, compared with 5 reference cells from another
  campaign window (29 Sep evening vs 30 Sep night).
- The packed cell (99.1k) is higher than every later cell with the packed
  code: the maximum was 97.9k, and the same s78 binary ran 84.3k / 92.1k in
  s80diag. The s78 point is on the high side of the rig's spread, so treat
  its matched/s gain with caution. The CPU-s/1M comparison is less
  sensitive: s80diag s78bin ran 49.2-52.3.
- The profile ran on main before hash dedup, so its Keccak share is higher
  than main's today.

## Open items

- **Load-generator slow state.** Cause unknown. Proposal: every stats.py
  should run the LOADGEN-SUSPECT check (submit and placed rate below 80% of
  the campaign median, plus the early-slow marker at 60 s) by default,
  before any arm comparison. Investigate the next slow cell as it happens
  (bench CPU, RPC latency, sender state).
- **Remaining CPU targets from the s77 profile.**
  - Exec-thread SipHash (`RandomState`, ~9% of the execution thread).
    Tried in s82 (ahash): ~3% of node CPU in profiles, below A/B
    resolution, parked; see `s82-exec-hasher-2026-09-30.md`.
  - `compute_action_hash` runs twice per action, once at pool insert and
    once at DA `put_batch` (~75 µs per action, per the s77 profile memory
    record; not re-measured).
  - ZSTD bottommost compression on compaction (`ZSTD_compressBlock_doubleFast`
    14.1% of `rocksdb:low`; all CFs share `cf_opts` with LZ4 and ZSTD
    bottommost, `torus-state/src/db.rs:326-327`). The trade-history share
    of compaction is now much smaller (5.0 vs 11.9-13.3 CPU-s/1M on
    `rocksdb:low`).
- **S78 test gap.** No test checks the trade tables after the
  parallel → sequential settle fallback. The review confirmed that the
  fallback happens before pass B, but no test pins the rows written on
  that path.
- **Streams with a subscriber.** Not measured since fixes `e80d08a` and
  `7117269`. Run it as an interleaved A/B.

## Reproduction

- Campaign dirs under `~/bench-results-matched/`:
  - `s77-prof-20260929` (profile: `perf/flat.txt`, `perf/agg.py`,
    `perf/stacks.py`, `perf/incl.py`)
  - `s77-hash-20260929`, `s77-lat-20260929`, `s77-low-20260929`,
    `s77-th-20260929`, `s77-thr-20260929`, `s77-split-20260930`,
    `s78-packed-20260930`
  - `s80-streams-20260930` (subscriber analysis:
    `s80-streams-sub-r1-streams/analysis.json`)
  - `s80-ab-20260930`, `s80-diag-20260930`, `s80-ab2-20260930`

  Each has `arms.conf` (binary, env and cell order) and `progress.tsv`.
  Cell data is in `~/bench-results-matched/<label>/` (`summary.json`,
  `sampler.csv`, `bench.log`).
- CPU-s/1M and per-thread tables: `python3
  ~/bench-results-matched/s77-hash-20260929/score.py <campaign_dir>
  <label>...`. This is read-only and uses `<campaign_dir>/<label>-host/threads.log`.
- Profile tables: run `python3 agg.py` in `s77-prof-20260929/perf/`. It
  reads `flat.txt` only.
- s80 statistics: `stats.txt` / `stats.json` in `s80-diag-20260930` and
  `s80-ab2-20260930`, made by the `stats.py` next to them (it reads
  `progress.tsv`). The s80ab `stats.json` has per-cell values only.
- Order latency: `summary.json` → `phase_by_node.valN.order_age_ms`. Exec
  queue: `sampler.csv` column `torus_exec_queue_depth`.
