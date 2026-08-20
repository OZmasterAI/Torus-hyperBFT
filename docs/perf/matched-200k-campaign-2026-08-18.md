# matched-200k campaign report — 2026-08-18/19

Branch `perf/matched-200k` (off `perf/book-digest-3c` @ `f186cc4`). Five rounds, 108 bench
cells, five merges in ~17 h of wall clock. Every number below is from NODE Prometheus
counters (`torus_orders_matched_total` deltas) on a 3-validator bare-metal devnet with
3-validator agreement checked after every cell; bench-side numbers and placed/s were never
used as the headline.

## 1. Goal

* **>= 200,000 matched/s** sustained (window-average over the bench window, node counters),
* **>= 300 markets**,
* with **byte-identical state on all 3 validators** (block hash, RPC state digest, counters) —
  a divergence or fail-stop rejects a candidate regardless of speed,
* matching-engine semantics (price-time priority, margin checks, fills, fees) unchanged.

**Outcome:** 200k was not reached and — on this rig — cannot be (section 7). The campaign
moved the 300 s / 10-market record cell from **~15–24k to ~41–42k matched/s window-avg and
from 29–33k to 52.8k best-60 s** (3.0x the 2026-07-20 RE-PROOF5 record window-avg, 2.3x its
best-60), with 3-validator agreement on every merged confirm. 300-market cells run and agree;
they are ~40 % slower than 10-market cells because of the O(markets) root/settle terms
(section 6).

## 2. Rig

| item | value |
|---|---|
| host | 18 × AMD EPYC (1 thread/core, single socket), 94 GB RAM, 338 GB SATA root (60 % full), Linux 6.8.0-106 |
| shared with | a LIVE testnet validator (`~/.cargo-target/release/torus-node`, ports 8555/9090/30333) — never touched; all builds went to `CARGO_TARGET_DIR=/home/18c/.cargo-target-matched` |
| devnet | `devnet/wsl` bare-metal 3 validators (RPC 8645-8647, metrics 9161-9163), no docker/sudo; bench process runs on the same box |
| load during a cell | load1 avg 24–29 / max 34–41 on 18 cores; torus-node %CPU ≈ 360/310/310, bench ≈ 165–200 |
| perf(1) | unavailable (`perf_event_paranoid=4`) → all attribution from in-node `torus_exec_*` timers |

## 3. Harness / runner usage

`tools/matched-bench/run-cell.sh <worktree> <label> [MARKETS=10] [DUR=120] [RATE=76000] [EXTRA_ENV]`
(README in `tools/matched-bench/`). One command = one cell: md5-proved binary staging into
`<worktree>/target/release/torus-node` (env.sh hardcodes that path — the stale-binary trap),
`MARKETS=N` genesis via `devnet/wsl/gen-3val-genesis.sh`, `CLEAN=1 launch-3val.sh` with the
record env, 1 Hz scrape of all three `/metrics`, the record-cell bench, drain on counter
quiescence, agreement (`agreement.jsonl`), log gzip, `summary.json` via `summarize.py`.

Record-cell shape (RE-PROOF5 R190, unchanged):
```
bench-throughput consensus --rpc-urls http://127.0.0.1:8645 --econ --senders 5000 --sender-offset 60
  --markets 10 --batch-size 400 --submit-batch 1 --format bin --concurrency 256 --duration 300
  --target-margin 1500 --cross-fraction 0.5 --cancel-fraction 0.05 --band 5 --rate-total 76000
```
Node env applied by the runner (all still **opt-in env, not compiled defaults** — verified in
code: `TORUS_BOOK_ROWS` unset = Classic, `TORUS_RESIDENT_BOOKS`/`TORUS_NATIVE_ROOT_CACHE`/
`TORUS_PARALLEL_SETTLE` only on for `"1"`, `TORUS_PARALLEL_BUCKET_HASH` unset = 1,
`TORUS_BUCKET_MEMBER_CACHE_MB` unset = 0, `NATIVE_TOTAL_BLOCK_CAP` compiled = 100):
`TORUS_BOOK_ROWS=3` (was 2 until the r2 merge) `TORUS_RESIDENT_BOOKS=1 TORUS_NATIVE_ROOT_CACHE=1
TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=4 TORUS_BUCKET_MEMBER_CACHE_MB=256
TORUS_COMMIT_LAG_BACKOFF_CAP=8`, plus since the r3 merge the `BLOCK_CAP=200` bundle
(`TORUS_NATIVE_TOTAL_BLOCK_CAP=200 TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=100000
TORUS_NATIVE_BLOCK_BYTES_CAP=12000000 TORUS_VERIFIED_SENDER_CACHE_CAP=32000`).

> **Post-campaign (2026-08-19):** two of these were promoted to compiled defaults on
> `perf/matched-200k` — the cap-200 bundle (r4 commits: `NATIVE_TOTAL_BLOCK_CAP` 200 /
> `ORDERS_PER_BLOCK` 100_000 / `BLOCK_BYTES` 12 MB / trust-cache 32_000 / 8 MB direct-push
> floor) and `TORUS_PARALLEL_BUCKET_HASH` unset = **8** (`DEFAULT_BUCKET_HASH_THREADS`; `"1"`
> = serial opt-out). The runner still exports both so env-only cells on older binaries stay
> equivalent; `BLOCK_CAP=100` / `EXTRA_ENV='TORUS_PARALLEL_BUCKET_HASH=4'` are the pre-flip
> controls. Mode 3 / resident books / root cache / parallel settle / member cache remain env.

Cell discipline: candidate cells 10 markets / 120 s, >= 2 reps, same-binary control where the
change is env-gated; the merged head is confirmed with one 300 s cell; markets-track items add
300-market / 120 s cells. Results: `/home/18c/bench-results-matched/<label>/summary.json`.

Agreement rule as implemented: `validators_agree` = height spread <= 5 AND block hash equal at
`min(height)-5` AND RPC state digest equal AND matched/placed/resting/actions counters equal
AND zero panic/fail-stop lines AND drained. Note: the executed native state root is not in the
header on this branch (0x0), so the RPC state digest (every book + OI + 50 balances) is the
determinism check.

## 4. Baseline (commit `f186cc4`, built 17:34 08-18)

| cell | markets | dur | matched/s avg | best-60 | placed/s | blk/s | exec q peak | agree |
|---|---|---|---|---|---|---|---|---|
| smoke-30s (the "baseline" key carried in the orchestrator state) | 10 | 20 s | **24,288** | 32,525 | 30,736 | 4.3 | 18 | yes |
| base-10m-r1 | 10 | 300 s | 13,166 | 30,220 | 16,551 | 0.8 | 66 | yes |
| base-10m-r2 | 10 | 300 s | 16,680 | 28,550 | 21,024 | 0.8 | 66 | yes |
| base-100m-r1 | 100 | 120 s | 17,370 | 30,679 | 22,512 | 0.9 | 66 | yes |
| base-100m-r2 | 100 | 120 s | 16,482 | 39,122 | 21,293 | 1.0 | 64 | yes |

Honesty note: the orchestrator's "baseline 24,288 / 32,525" is the 20 s smoke cell, not a 300 s
cell. The real like-for-like 300 s baseline is **14.9k avg / 29.4k best-60 (mean of r1, r2)**.
Both are quoted below; the campaign multiple is larger against the honest one.

Baseline phase breakdown, val0, ms per NATIVE block (base-10m-r2, 300 s, mode 2):
block 1258 (wall 1311/native block, exec thread busy 0.96, exec queue pegged 64–66 ⇒ exec-bound):
save_books 673 (54 %), engine 257 (20 %: margin 12 / match 35 / settle 72 / ~140 untimed),
flush 118 (root 42 + state_write 70), evm(header put only) 78, body_persist 66, verify 37,
load_books 11, residual 16. save_books grew 253 → 545 ms early→late as resting went 345k → 803k;
everything else flat. 100 markets: block 1218, flush 384 (root 166 + state_write 197),
engine 310 (settle 233), save_books 265, body_persist 112, evm 94; dirty buckets/flush 9,479
vs ~1,500 at 10 markets. 74–81 % of offered actions were shed by the 60 s nonce window
because exec could not drain (121.8k submitted / 25.4k processed at base-10m-r1).

## 5. Ceiling analysis (written before round 1, held up)

Operating point: one exec worker thread is THE wall (busy 0.91–0.96, queue pegged);
consensus/dispatch/ingress are not. Exec cost ≈ 52 µs per matched order at baseline; 200k
matched/s on one exec thread needs ≤ 5 µs — a 10x cut of the serial chain.

Phase-removal arithmetic at baseline shape (24k fills/blk): drop save_books entirely → 41k;
parallel save_books → 32k; + body/header persist removal → 39k; + flush pipelined off-thread →
49k; + verify prefetch → 53k; + engine sharding/tombstones/i128 → 68–72k. ⇒ **single-exec-thread
ceiling on this rig ≈ 60–75k window-avg, ~90–110k best-60** after every critical-path item in
the backlog lands. Observed after five rounds: 41–42k avg / 53k best-60 at 300 s, 46–53k avg /
63–84k best-60 at 120 s — on the predicted curve (~60 % of the ceiling consumed).

Walls, in order: (1) save_books depth-proportional level hashing — FELL (mode 3 + parallel drain:
673 → 152 ms); (2) redundant exec-time body/header persistence — FELL (215+78 → 0.06 ms);
(3) flush root+state_write on the exec chain — still there, 272 ms (21 %) and growing with
resting depth; (4) the untimed ~140 ms inside engine + serial Phase-1/2/pass-B — still there,
engine is now 726 ms (57 %) because blocks are 2.4x bigger (67k placed / 54k matched per exec
block at cap 200); (5) box oversubscription — unchanged (3 validators × tokio/rayon/match/settle
pools + RocksDB bg on 18 cores; measured ~2.6x engine inflation vs µbench).

## 6. Rounds

Each row: candidate key, delta % of matched/s avg vs the round's control/baseline pool (120 s
reps, as judged), verdict, reason. Cells are in `/home/18c/bench-results-matched/r<N>-<key>-*`.

| rd | candidate | Δ avg | verdict | reason |
|---|---|---|---|---|
| 1 | save-books-parallel-levels | +2.3 % (24.9k vs 24.3k; 300 s 22.3k vs w1-control 16.9k = +32 %) | **MERGED** `fec3cff` | parallel mode-2 journal drain across dirty books, node-local, byte-identical; 300 s confirm 22.0k / 31.5k best-60 (flagged "regressed on confirm" vs the 20 s smoke, but +47 % vs the honest 300 s baseline) |
| 1 | level-hash-seq-chunked (mode 3) | +11.2 % | restack | consensus-visible chunked level digest; needs fresh genesis — judged a round-2 restack |
| 1 | body-persist-skip | −6.6 % | rejected | same-binary ctl-rewrite 24.5k vs 22.7k; no gain at cap 100 — redundant write was not yet on the critical path |
| 1 | bench-env-bounded-pools | −67.7 % | rejected | TORUS_CORE_BUDGET=5 / cpusets / tokio18 starve exec and consensus on the shared box (tokio18 diag: agree=false by digest timing, 1 cell) |
| 2 | level-hash-seq-chunked-restack | +37.0 % (33.3k vs 24.3k; same-binary rows2 ctl 28.6k ⇒ +16 %) | **MERGED** `e271291`/`3bf5a31` | TORUS_BOOK_ROWS=3 default for the cell; 300 s confirm 29.2k / 39.7k; save_books 336 → 73 ms/blk |
| 2 | block-cap-raise-sweep | +38.2 % | restack | env-only cap 150/200/300 all beat cap 100 (28.5k); 200 best; one rep agree=false (height spread 6 at sample time, hashes/digest/counters equal — a sampling artefact, not a fork) |
| 2 | consensus-view-legs-trim | +20.7 % vs baseline, **−6 % vs same-binary nosplit control** (29.3k vs 31.2k) | rejected | no real gain; one rep agree=false by height spread 6, hashes equal |
| 2 | resident-books-stale-rebuild | +0.5 % | restack | fixes a real multi-second stall but no measurable throughput in-window yet |
| 3 | block-cap-raise-sweep-restack | +42.0 % (41.4k vs ctl-cap100 32.3k = +28 %) | **MERGED** `d950a84`/`b97a8b0` | BLOCK_CAP=200 bundle is now the harness default (node compiled default stays 100 pending a WAN full-mesh bench); dissemination counters clean; 300 s confirm 32.7k / 47.0k |
| 3 | exec-write-stall-attribution | +21.2 % | restack | RocksDB stall/write-group telemetry + chunked low-pri bg trade writer; good best-60 (64.6k) |
| 3 | resident-books-stale-rebuild-restack | +10.5 % | restack | still below the +8 %-over-pool bar once cap 200 moved the pool |
| 3 | empty-block-fill-under-backlog | — | not implemented | dropped for time |
| 4 | commit-persist-reuse-wire-body | +35.2 % (44.2k) | **MERGED** `55da979` | consensus-thread commit persist as one WriteBatch of the bin wire body (FIX 1a), exec skips the rewrite; body_persist 215 → 0.09 ms, exec-time header put 0; 300 s confirm **42.3k / 50.7k** |
| 4 | exec-write-stall-attribution-restack | +30.8 % (42.8k, best-60 70.0k) | restack | within noise of the winner on avg, better best-60 |
| 4 | block-cap-200-default-and-push-floor | +18.9 % | rejected | compiled default 200 + push floor: no gain over the env bundle; cap 300 inconsistent (44.1k / 35.3k) |
| 4 | resident-books-stale-rebuild-restack-restack | +10.2 % (one rep 17.4k — stall landed in-window) | restack | variance, not throughput |
| 5 | exec-write-stall-attribution-restack-restack | +21.7 % (51.5k, best-60 81.2k) | **MERGED** `49f0130` | persist_block_header folded into the flush batch, FIX 1a body/DA writes decoupled, torus_rocksdb_* stats; 300 s confirm **40.9k / 52.8k** (avg −3 % vs r4 confirm, inside rep sd ≈ 2.9k; best-60 +4 %) |
| 5 | root-and-save-workers-sweep | +13.9 % (`PARALLEL_BUCKET_HASH=8`: 48.2k n=3 vs ctl 43.5k n=3 = **+10.8 %**; pbh12 +3 %; sbw8 +3 %; **sbw4 −34 %**; pbh8+mc512 mixed) | env finding, not merged | free +10 % to adopt as the harness default next session; TORUS_SAVE_BOOKS_WORKERS=4 is a cliff |
| 5 | resident-books-stale-rebuild-restack³ | +9.3 % (46.0k n=3, stable reps) | restack | 300-market rep 24.4k (best-60 44.9k) agree=yes — best 300 m cell so far |
| 5 | matched-cell-duration-parity | +9.8 % (tooling only) | restack | first-120 s window for every run; keep |

Cells that read `validators_agree=false` (7 of 108) were each inspected: six are sampling
artefacts (height spread 6 > 5 at the moment of the probe, or RPC state digests taken while
heights were still advancing — block hashes at the common height, counters and logs all equal,
zero fail-stop). **One is real and pre-existing:** `r1-level-hash-seq-chunked-ctl-rows2-r2`
(mode-2 *control*, commit `3aa3425`) — val2 latched `hotstuff_rs::block_sync ConflictingCommittedChain`
at height 191 after "body fetch exhausted 9 retries" / "no available sync servers", while val0/val1
agreed with each other. Same signature as the 2026-07-21 re-proof5 S19 incident (memory
`844652eb79a2fd5b`); the candidate diff touched no consensus/network crate. It is the only cell of
108 with a fail-stop line and must stay on the consensus backlog (see section 9).

## 7. What merged (perf/book-digest-3c..perf/matched-200k)

```
49f0130 perf(matched): merge exec-write-stall-attribution-restack-restack   (r5)
b9fec65 test(state): ignored write-group head-of-line probe
b8c88b8 tools(matched-bench): scrape torus_rocksdb_* + split write timers
602b88e perf(consensus): fold the not-yet-durable native-block header into the flush batch
4563003 feat(node): sample DB-wide RocksDB runtime stats into torus_rocksdb_* every 5 s
fd2aa8b feat(telemetry): torus_rocksdb_* gauges + commit/body-put latency histograms
2eb62ce perf(state): chunked low-pri background trade writes + RocksDB stats/write-controller knobs
55da979 perf(matched): merge commit-persist-reuse-wire-body                  (r4)
a309b88 tools(matched-bench): scrape + summarize the commit-persist timers
5216c88 fix(rpc,bridge): read/write CF_BLOCK_BODIES through the shared body-record codec
1adb76e perf(consensus): commit-time persist as one WriteBatch of the bin body record
d803e5e feat(telemetry): torus_commit_persist / commit_body_encode / commit_persist_write
5109ce7 feat(state): CF_BLOCK_BODIES record codec — tagged bin (wire) record + legacy JSON decode
b97a8b0 tools(matched-bench): default cell BLOCK_CAP=200 after the r3 merge
d950a84 perf(matched): merge block-cap-raise-sweep-restack                   (r3)
4c6f96a test(consensus): de-flake parked_durable_hole_fail_stops_on_budget_exhaustion
3627b56 tools(matched-bench): BLOCK_CAP=N bundle + dissemination/pacing accounting
bd18ba4 perf(mempool): derive the exec trust-cache default from the native block cap
3bf5a31 tools(matched-bench): default cell env to TORUS_BOOK_ROWS=3 after the r2 merge
e271291 perf(matched): merge level-hash-seq-chunked-restack                   (r2)
079cf45 docs(levelhash): §7.1 restack notes
f5c66da perf(core): locate orders in their level by binary search on seq
d8220ef test(bridge): mode 3 rides the two-pass parallel save drain
5de4b9e test(bridge): '3' is now a valid TORUS_BOOK_ROWS value
f29212f feat(bridge): BookMode::LevelAuthorityChunked — TORUS_BOOK_ROWS=3, marker byte 3
a59dfaf perf(core): chunked level digest — depth-independent level_hash maintenance (mode 3)
fec3cff perf(matched): merge save-books-parallel-levels                       (r1)
b253aac docs(devnet): note TORUS_SAVE_BOOKS_WORKERS / MIN_OPS in env.sh
b4afbe0 perf(save-books): parallelise the mode-2 journal drain across dirty books
765b84f perf(save-books): expose journaled_levels + crate-visible LPT chunker
ae8b272 tools(matched-bench): early/late 60s phase windows + resummarize helper
8494b4c tools(matched-bench): sample rocksdb memtable/L0/compaction gauges too
3b88156 tools(matched-bench): reusable devnet cell runner
```
Consensus-visible change: only mode 3 (`TORUS_BOOK_ROWS=3`, level digest = keccak over per-64-seq
chunk digests; fresh genesis, marker byte 3). Everything else is node-local and byte-identical.

## 8. Final best vs baseline vs prior record (10 markets, 300 s record cell)

| | commit | matched/s avg | best-60 | placed/s | blk/s | agree |
|---|---|---|---|---|---|---|
| prior record RE-PROOF5 R190 (2026-07-20, mode 2, cap 100) | 4f832e5 | 13,659 | 23,160 | — | — | yes |
| baseline, honest 300 s mean (base-10m-r1/r2) | f186cc4 | 14,923 | 29,385 | 18,788 | 0.8 | yes |
| baseline as carried by the orchestrator (20 s smoke) | f186cc4 | 24,288 | 32,525 | 30,736 | 4.3 | yes |
| r4 merged confirm | 55da979 | **42,320** | 50,678 | 53,029 | 0.9 | yes |
| r5 merged confirm = **final head** | 49f0130 | 40,946 | **52,840** | 51,301 | 1.2 | yes |
| final head, 120 s cells (r5 winner reps) | b9fec65 | 51,509 (n=2) | 81,175 | 64,630 | 1.3 | yes |

Multiples: final vs prior record **3.0x avg / 2.3x best-60**; vs honest 300 s baseline 2.7–2.8x
avg / 1.8x best-60; vs the smoke-cell baseline 1.7x avg / 1.6x best-60. Per-native-block phase at
the final head (val0, 300 s): block 1281 ms for 67k placed / 54k matched per exec block (cap 200
binding, 188 actions/blk); engine 726 (57 %: margin 32 / match 105 / settle 200 / ~390 untimed),
flush 272 (21 %: root 103 + state_write 157), save_books 152 (12 %), load_books 58 (13 stale
rebuilds), verify 32, residual 36, body_persist 0.06, evm 0. Exec thread busy 0.91, queue peak 66,
commit_persist 24 ms/commit, RocksDB stall 0 ms, WAL 26 MB/s, 3 nodes: heights within 2, block
hash + state digest + counters identical, 0 panic/fail-stop.

## 9. Markets status (>= 300 markets)

| cell | markets | matched/s avg | best-60 | blk/s | agree | notes |
|---|---|---|---|---|---|---|
| base-100m-r1 (f186cc4) | 100 | 17,370 | 30,679 | 0.9 | yes | genesis md5 085eb353…, --senders 5000 (50/market), incl-drain 19,617, worst-60 blk/s 0.667, exec queue 66 / busy 0.948, 200 native blocks, 32.4k placed/blk, 1285 ms wall/native blk, dirty buckets/flush 9,479 (vs 1.3–1.5k at 10 m), rejected_book 0, 74 % of submitted actions nonce-evicted, agreement identical (spread 0) |
| base-100m-r2 | 100 | 16,482 | 39,122 | 1.0 | yes | |
| r2-resident-books-stale-rebuild-300m-r2 (b903cac) | 300 | 14,327 | 27,999 | 0.7 | yes | r1 PARTIAL: RPC state digest at 300 markets takes ~4 min/node and the caller timed out — digest now needs a longer budget |
| r3-…-restack-300m-r1 (3e2b134) | 300 | 14,630 | 27,306 | 1.2 | yes | |
| r5-…-restack³-300m-r2 (4aaef1d, cap 200, mode 3) | 300 | **24,439** | 44,853 | 0.6 | yes | block 1605 ms: flush 769 (48 %: root 368 + state_write 359), engine 637 (settle 516), save_books 126; dirty buckets/flush **27,332**; r1 interrupted by the caller |

300-market genesis + bench work (gen-3val-genesis.sh MARKETS=N, synthetic S<k>-USD beyond the
100 base rows; `--markets N`). Determinism holds at 300 markets. Throughput at 300 markets is
~60 % of the 10-market figure on the same head; the gap is exactly the predicted O(markets)
terms: uniform-bench dirty position rows (27k dirty buckets/flush → root 368 ms), settle pass A
one-thread-per-market (settle 516 ms), state_write 359 ms. None of the markets-track candidates
(locality knob, capped pass A, order index, stops flag, trader-bucketed positions) were reached.

## 10. Remaining gap to 200k — box vs code

Final head: 41k avg / 53k best-60 (300 s), 52k / 81k (120 s). 200k is **4.9x the 300 s avg and
3.8x the best-60**.

What the **box** caps (cannot be coded around on this rig):
* Each validator has ~5 cores of an 18-core box that also runs the other two validators, the
  bench (EIP-712 signing of 600 batches/s at 200k), and a live testnet validator. At 200k matched/s
  (≈ 240k placed/s) each validator does per second ≈ 400k position RMWs, ~40k level rehashes,
  25k+ dirty trie buckets, 60 MB/s of trade rows, 15 MB/s of block bodies to gossip/persist,
  600 batch ecrecovers. Even at an aggressive all-in 25 µs CPU per fill that is 5 core-seconds
  per second — the entire per-validator share with zero headroom. CPU-budget limit ≈ 100–120k
  matched/s theoretical; measured ~2.6x engine deschedule inflation vs µbench says it is lower.
* Agreement checks need blk/s >= ~2; the record cells run 0.6–1.3 blk/s at 50–80k-order blocks.

What the **code** caps (real, on the backlog):
* One serial exec thread: engine 726 ms/blk (57 %) with ~390 ms untimed, flush 272 ms (root +
  state_write) serial after the engine, save_books 152 ms. Single-exec-thread ceiling after the
  whole backlog ≈ 60–75k avg / 90–110k best-60 on this rig; ~150k+ only with market-sharded
  execution + pipelined flush on a dedicated >= 16-core validator.
* At 300 markets the O(markets) terms (uniform dirty rows → root, pass-A spawns, state_write)
  add ~60 % to the block; the locality/pass-A-cap/order-index/trader-bucket items are the fix.

**Honest mission statement:** 200k matched/s with 3 validators in byte-identical state is a
multi-node / dedicated-hardware number, not an 18-core-shared-box number. On this rig the
achievable target is max matched/s with agreement (realistic 50–70k avg, ~100k best-60), plus
two demonstrations: (i) linear scaling of matched/s with cores per validator (1-validator + bench
with TORUS_MATCH_WORKERS/RAYON sweeps, or taskset 3 validators 5/5/5 vs 8/8/2); (ii) flat
per-block cost from 10 → 300 markets (root/settle/save_books within ±20 % at equal fills/blk
after the markets-track items, `--markets-per-sender` locality as the declared 300-market shape).

## 11. Ranked backlog for the next session

Ranked by expected matched/s per unit of risk on this rig; the first three are near-free.

1. **Adopt `TORUS_PARALLEL_BUCKET_HASH=8` as the cell default** (r5 sweep: +10.8 % n=3 vs n=3,
   agreement clean) and re-confirm at 300 s; keep `TORUS_SAVE_BOOKS_WORKERS` at the host default
   (4 is a −34 % cliff). Then **block-cap-resweep-on-fast-commit** (cap 300/400, backoff cap 4/16)
   now that commit interval is 265–300 ms — +5–10 %, or it reveals the 76k rate cap is binding
   (run a rate-100k probe first).
2. **flush-pipeline-1deep / flush-root-and-batch-overlap**: flush(N) root+state_write under
   engine(N+1); −270 ms/blk at 10 markets (21 %), −770 ms at 300 markets (48 %) off the exec chain.
   Biggest single code lever left; needs the split timer first.
3. **engine-untimed-attribution** (sub-timers for Phase-1 cancels, Phase-2 prepare, settle
   plan/apply/flush, cache misses) — the ~390 ms/blk untimed engine share is now the largest
   unexplained term; measurement before more engine work. Follow with **phase1-cancel-tombstones**,
   **fixedpoint-i128-fastpath**, **exec-thread-trims**, **passb-serial-diet** (each −15–45 ms/blk).
4. **markets-300-cell-locality** + **settle-passA-capped-chunks** + **positions-bucket-by-trader**
   + **stops-dirty-flag-range-scan** + **order-index-map**: the markets deliverable; root 368 →
   ~100 ms and settle 516 → ~200 ms at 300 markets. Also raise the runner's state-digest timeout
   (4 min/node at 300 markets).
5. **resident-books-stale-rebuild (restack⁴)**: 13 mid-run full rebuilds in the r5 confirm
   (load_books 58 ms/blk avg, multi-second stalls); removes variance (17.4k / 26.2k outlier reps)
   more than average throughput; prerequisite for fair 300 s comparisons.
6. **matched-cell-duration-parity**: first-120 s window in every summary + 2×120 s + 1×300 s
   merged-confirm, so the +8 % rule stops producing false winners/rejects (120 s pool sd ≈ 2.9k).
7. **Storage**: trade-kvs-arena-and-user-index-diet, bg-trade-writer-wal-off, history-db-split,
   rocksdb-pipelined-write-and-bg-chunk-sweep (+2–8 % each, mostly late-window at 300 s where
   state_write grows 106 → 158 ms/blk).
8. **Consensus side**: view-timeouts-under-load (163–180 timeouts per 300 s cell, each >= 500 ms
   of dead view), exec-idle-and-commit-interval-attribution, per-thread-cpu-attribution.
9. **Open incident (consensus, not perf):** the val2 `ConflictingCommittedChain` fail-stop in
   `r1-level-hash-seq-chunked-ctl-rows2-r2` (pre-existing signature from re-proof5 S19). Reproduce
   under degraded 1 blk/s start-up with body-fetch exhaustion; until understood, every campaign
   cell must keep grepping for it (the runner does).
10. **save-books-pass2-and-shards**, **resident-caches-hoist-prefetch**: smaller follow-ons.

Beyond this rig: the only path to 200k is market-sharded execution (one exec thread per shard
with deterministic cross-shard settlement), pipelined flush, and one validator per >= 16-core
host; the scaling demonstrations in section 10 are how to make that case with numbers.

---

## Rounds 6+ (2026-08-19)

Four more rounds (6, 7, 8, 9) on top of the round-5 tip, run by the same workflow: 3 candidates
implemented per round, benched on the same rig, one `--no-ff` merge per round. 12 candidates,
~40 bench cells, 4 merges, `perf/matched-200k` `49f0130 → e44a38c → 484115e → 997d59f → d1a99b2
→ d7a073f`. Nothing pushed. Every number below is again from node Prometheus counters with
3-validator agreement checked per cell; each figure names its cell (markets, duration).

### 12. Baseline on the new-defaults head (`e44a38c`)

`e44a38c` is the first head where the cap-200 bundle (`NATIVE_TOTAL_BLOCK_CAP 200` /
`ORDERS_PER_BLOCK 100k` / `BLOCK_BYTES 12 MB` / trust-cache 32k / 8 MB direct-push floor) and
`TORUS_PARALLEL_BUCKET_HASH=8` are **compiled defaults** rather than cell env. Rounds 6–9 are
measured against it.

| cell | markets | dur | matched/s avg | best-60 | blk/s | agreement | block ms breakdown (val0) |
|---|---|---|---|---|---|---|---|
| `defaults-flip-noenv-r1` | 10 | 120 s | 34,946 | 66,450 | 0.7 | yes | first no-env cell on the flipped defaults |
| `r6-base-10m-r2` | 10 | 120 s | **42,203** | 65,177 | 1.5 | yes | block 830.1 = engine 395.1 (settle 177.2 / match 97.4 / margin 31.3) + flush 240.0 (root 94.9 + state_write 133.3) + save_books 122.9 + verify 30.7 + replay_guard 5.5 + residual 34.4; busy 0.777 |
| `r6-base-300m-r1` | 300 | 120 s | **17,531** | 30,154 | 1.3 | **digest-unverified** (known 300 m harness artifact; hash + counters equal, 0 fail-stop) | block 1891.5 = engine 871.2 (settle 734.3 / match 71.1 / margin 35.2) + flush 801.2 (root 388.6 + state_write 369.9) + save_books 141.9 + verify 25.8 + replay_guard 4.2 + load_books 3.7 + residual 43.1; busy 0.958 |

Carried record cells for the deltas below: **10 m 40,900 avg / 52,800 best-60** (300 s on
`49f0130`), **300 m 24,439 avg / 44,853 best-60** (`r5-restack³-300m-r2` on `4aaef1d`). Note the
same-head 300-market number (17.5 k) is far below the carried 24.4 k — the carried cell is on an
older head with different env; this is why round 6 judged on both.

### 13. Rounds 6–9 — per-candidate table

`d10 %` / `d300 %` are the candidate's cell mean vs **that round's carried best** (10 m: 38,574 →
39,054 → 41,679 → 41,679; 300 m: 24,439 → 24,958 → 25,132 → 26,515). All cells 120 s unless said.

| rd | candidate | track | d10 % | d300 % | verdict | reason |
|---|---|---|---|---|---|---|
| 6 | `harness-300m-digest-and-parity` (`eab02ff`) | harness | +26.05 (n=2) | −29.25 (n=1) | stacked → r7 | Tools-only, disjoint files. Made the 300 m digest concurrent (53 s vs ~4 min/node) and turned `AGREE` into a real verdict. d300 is against the *older-head* carried cell, not a code regression. |
| 6 | `engine-untimed-attribution` (`2a8d6a1`) | instrumentation | +31.2 (n=2) | −16.4 (n=1) | stacked → r7 | Timers only. Showed settle pass A = 463 of settle 699 ms at 300 m; timers were mis-placed for the new chunked pass A, so restack first. |
| 6 | **`settle-passa-capped-chunks`** (`583badd`) | markets | +10.78 (n=1) | **+8.24 (n=2)** | **MERGED `484115e`** | Only candidate positive on the 300 m track. Settle 375/342 ms/blk vs 516 carried (−30 %) and 699–734 same-head (−50 %); engine 526/476 vs 857–871. Full agreement incl. pinned digest on both reps. |
| 7 | **`harness-300m-digest-and-parity-restack`** (`26680ac`) | harness | +28.42 (n=2) | −1.23 (n=2) | **MERGED `997d59f`** | Unblocks the primary deliverable: `AGREE` + equal digests on 4/4 cells, `DRAIN_TIMEOUT=180+2·M`, `matched_s_first120` in every summary, `--markets-per-sender` passthrough (K=0 = byte-identical RNG draw, verified in diff). Throughput-neutral by construction. |
| 7 | `engine-untimed-attribution-restack` (`9f4dd3a`) | instrumentation | +28.93 (n=2) | +5.04 (n=1) | stacked → r8 | Timers exact; 300 m engine 473 = phase1 30 + margin 32 + match 74 + settle 333 [passA 115 / passB 165 / cache_flush 50]. Merge conflict tree with the harness candidate → restack. |
| 7 | `state-write-build-vs-db-split` (`8a6ce07`) | instrumentation | +25.73 (n=2) | −11.02 (n=1) | stacked → r8 | Split proved 300 m state_write 350 = build 45 + DB 305, batch 10.8 MB, 36 MB/s, `stall_micros 0`. But the split keys only populated from the candidate's own runner → restack onto the merged harness. |
| 8 | `engine-untimed-attribution-restack²` (`555e182`) | instrumentation | +17.47 (n=2) | −2.53 (n=1) | stacked → r9 | Clean, but the flush split was the more urgent measurement (flush had become the 300 m wall). |
| 8 | **`state-write-build-vs-db-split-restack`** (`2073028`) | instrumentation | +7.10 (n=2) | −7.70 (n=1) | **MERGED `d1a99b2`** | Lowest deltas of the round and merged anyway: it closes the harness trap (split keys now populate from the merged main-repo runner) and answers "where is state_write?" — **86 % is the RocksDB write, 14 % the WriteBatch build**, which redirects the next lever from parallelising the build to WriteOptions/WAL. |
| 8 | `stops-dirty-flag-range-scan` (`b58c358`) | markets | +11.30 (n=1) | +1.09 (n=2) | stacked → r9 | Positive but n=1 on 10 m and inside noise on 300 m. |
| 9 | **`engine-untimed-attribution-restack³`** (`15557a1`) | instrumentation | +10.15 (n=2) | −1.19 (n=1) | **MERGED `d7a073f`** | First binary carrying **both** the r6 engine sub-timers and the r7 flush build/db split in one `summary.json`; identities exact on all 9 node-cells; overhead 3–7 ms/blk. Both deltas are noise, as expected for timers. |
| 9 | `stops-dirty-flag-range-scan-restack` (`a79afcd`) | markets | +16.11 (n=2) | −3.25 (n=3) | held | **Mechanism confirmed**: save_books 151.1 → 123.8 ms at 300 m (−18.1 %), 5.756 → 4.901 µs per dirty bucket (−14.9 %). Matched/s flat (sd 1040, ~1.4 σ). Below the "≥ 30 % of a phase *and* visible in matched/s" bar; carry as a stackable. |
| 9 | `state-write-db-writeopts-waloff` (`79ea483`, default **OFF**) | markets | +26.35 (n=2) | −2.80 (n=2) | held | Largest single phase win measured in these rounds: state_write DB 323 → 210 ms at 300 m (−35 %), 63–67 MB/s. **Crash-recovery gate passed** (kill −9 val2 mid-load; restart logged an 11-block execution gap, replayed 80..90 in ~7 s, digest identical to val0/val1) — but 11 blocks of state are rewound per crash, and matched/s is flat. Needs same-binary ON/OFF control cells before any throughput credit. |

Two confirm cells were **discarded as rig events, not code results**, and re-run:
`r8-merged-confirm-10m` (native-DA dissemination collapse: 60 "body fetch exhausted 9 retries"
lines, 16 blocks in 304 s, exec `block_ms` 541 against `wall/committed` 19,750 ms — binary md5
byte-identical to a clean 44.9 k cell) and `r9-merged-confirm-10m` (launched 21 s after a 47 MB
link; idle blk/s 1.5–2.4 against a healthy 12–28, `wall/committed` 1541 ms vs block 806 ms,
124 k nonce-expiries/node). Both preserved on disk. Rig noise cost 2 of 9 confirm cells.

### 14. What merged (rounds 6–9)

```
d7a073f  perf(matched): merge engine-untimed-attribution-restack-restack-restack   (r9)
  15557a1  test(matched-bench): guard the r6-engine / r7-flush sub-timer coexistence
  d8a6c1f  test(exec): name the signed-action tuple in exec_phase_accum_tests
  7474ede  tools(matched-bench): scrape + report the engine sub-phase attribution
  6333eaa  perf(exec): sub-timers for the ~390 ms/blk untimed engine share
d1a99b2  perf(matched): merge state-write-build-vs-db-split-restack               (r8)
  2073028  tools(matched-bench): carry the state_write split into the early/late windows
  ca159f3  perf(obs): split flush state_write into WriteBatch build vs RocksDB write
997d59f  perf(matched): merge harness-300m-digest-and-parity-restack              (r7)
  26680ac  tools(matched-bench): pinned-state 300m digest, first120 window, MPS passthrough
  121c940  tools(bench): --markets-per-sender K locality load shape (default 0 = unchanged)
484115e  perf(matched): merge settle-passa-capped-chunks                          (r6)
  583badd  perf(settle): LPT-chunk pass A across capped workers instead of one thread per market
```

**One of the four merges is a throughput mechanism** (r6 settle pass A). The other three are a
harness fix and two instrumentation merges — deliberately, because after r6 the block-time mass
moved to a phase nobody could attribute. No merged change alters matching semantics, the state
root layout, or a consensus-visible default; `waloff` and `markets-per-sender` are both gated OFF
by default.

### 15. Best cells before vs after rounds 6–9

**10 markets, 300 s confirm cells** (all `AGREE`: block hash + header root + RPC state digest +
counters equal on all 3 nodes):

| head | cell | avg | best-60 | first-120 | block ms breakdown (val0) |
|---|---|---|---|---|---|
| carried (`49f0130`) | r5 record | 40,900 | 52,800 | — | — |
| `484115e` (r6) | `r6-merged-confirm-10m` | 39,054 | 50,039 | — | block 1326.1 = engine 779.4 (settle 228.0 / match 117.6 / margin 34.6) + flush 308.0 (root 107.3 + state_write 188.6) + save_books 160.1 + verify 32.7 + residual 39.5 |
| `997d59f` (r7) | `r7-merged-confirm-10m` | 41,679 | 57,357 | 51,296 | block 1275.3 = engine 731.5 (settle 194.1) + flush 263.4 (root 91.7 + state_write 159.3) + save_books 149.2 + load_books 55.7 + verify 31.5 |
| `d1a99b2` (r8) | `r8-merged-confirm-10m-r2` | 39,551 | 66,137 | 45,829 | block 1211.3 = engine 636.8 (settle 192.5) + flush 253.8 (root 86.9 + state_write 154.9 = build 16.5 + db 138.5; 3.76 MB/blk, 27.8 MB/s) + save_books 146.6 + load_books 102.4 |
| **`d7a073f` (r9)** | **`r9-merged-confirm-10m-r2`** | **42,284** | **56,707** | 51,862 | block 1168.0 = engine 708.4 (**phase1_actions 391.5** + settle 191.3 [passA 63.2 / passB 120.3 / cache_flush 4.3] + match 88.6 + margin 30.4 + untimed 6.3) + flush 261.8 (root 88.0 + state_write 162.2 = build 14.8 + db 147.4; 3.97 MB/blk, 27.6 MB/s) + save_books 130.7 + verify 27.2 + residual 34.8; busy 0.913, dirty buckets/flush 3131 |

10 m net: **40,900 → 42,284 avg (+3.4 %) and 52,800 → 56,707 best-60 (+7.4 %)** — inside the
~2.9 k pool sd, i.e. **parity, not a win**. The four confirms span 39.1–42.3 k with no trend.
The 10-market track did not move in rounds 6–9 and was never the target after r6.

**300 markets, 120 s confirm cells:**

| head | cell | avg | best-60 | digest | block ms breakdown (val0) |
|---|---|---|---|---|---|
| `e44a38c` (base) | `r6-base-300m-r1` | 17,531 | 30,154 | unverified (artifact) | block 1891.5 = engine 871.2 (settle 734.3) + flush 801.2 (root 388.6 + sw 369.9) + save_books 141.9 |
| carried (`4aaef1d`) | r5 record | 24,439 | 44,853 | yes | block 1605 = flush 769 (root 368 + sw 359) + engine 637 (settle 516) + save_books 126 |
| `484115e` (r6) | `r6-merged-confirm-300m` | 24,958 | 37,515 | yes | block 1556.4 = flush 819.0 (root 399.9 + sw 375.0) + engine 506.6 (settle 361.4) + save_books 148.0 |
| `997d59f` (r7) | `r7-merged-confirm-300m` | 25,132 | 38,996 | **yes, parallel (54 s/node)** | block 1829.8 = flush 933.3 (root 457.5 + sw 423.5) + engine 583.7 (settle 410.1) + save_books 172.6 |
| `d1a99b2` (r8) | `r8-merged-confirm-300m` | **26,515** | 43,694 | yes (73 s wall) | block 1533.4 = flush 811.7 (root 387.4 + sw 376.4 = build 52.9 + db 323.5; 12.24 MB/blk, 38.8 MB/s) + engine 487.4 (settle 337.7) + save_books 151.1 |
| **`d7a073f` (r9)** | **`r9-merged-confirm-300m`** | **26,181** | 40,506 | yes (63 s/node) | block 1615.7 = **flush 852.7 (52.8 %: root 415.1 + state_write 390.5 = build 53.4 + db 337.2; 12.24 MB/blk, 37.2 MB/s)** + engine 522.6 (settle 369.5 [passA 125.2 / passB 185.1 / cache_flush 55.4] + phase1 34.1 + match 77.4 + margin 36.5) + save_books 155.8 + verify 29.0 + residual 48.7; busy 0.935, dirty buckets/flush 26,386 |

300 m net: **+7.1 % vs the carried record (24,439 → 26,181)** and **+49.3 % vs the same-head
`e44a38c` baseline (17,531 → 26,181)**. The like-for-like same-head figure is the honest one:
almost all of it is r6's settle pass A (settle 734 → 370 ms/blk, −50 %; engine 871 → 523, −40 %).
Best 300 m cell of the campaign is r8's 26,515; r9's 26,181 is −1.3 % from it, n=1, noise.

**The wall moved.** At 300 markets flush is now **52.8 %** of the block against engine's 32.3 %,
exactly inverted from the round-5 picture. Inside flush: root 415 ms (unattributed — no
sub-timers exist) and the RocksDB write 337 ms of state_write's 391. That is where round 10 goes.

### 16. "Flat cost 10 → 300 markets" — not reached

The deliverable is: per-block cost within ±20 % from 10 to 300 markets **at equal fills/block**,
with `--markets-per-sender` locality as the declared 300-market shape. Status on the final head:

| phase (val0, ms/native block) | 10 m (`r9-…-10m-r2`) | 300 m (`r9-…-300m`) | Δ |
|---|---|---|---|
| fills / native block | 52,014 | 48,912 | −6 % (near-matched) |
| **block** | **1168.0** | **1615.7** | **+38 %** |
| flush root | 88.0 | 415.1 | **+372 %** |
| flush state_write (db) | 162.2 (147.4) | 390.5 (337.2) | +141 % (+129 %) |
| settle | 191.3 | 369.5 | +93 % |
| — pass A / pass B / cache_flush | 63.2 / 120.3 / 4.3 | 125.2 / 185.1 / 55.4 | +98 % / +54 % / **+1189 %** |
| save_books | 130.7 | 155.8 | +19 % |
| verify / replay_guard / load_books | 27.2 / 4.9 / 0.2 | 29.0 / 4.2 / 2.1 | flat |
| phase1_actions | 391.5 | 34.1 | −91 % (load shape, not markets) |
| state_write batch | 3.97 MB/blk @ 27.6 MB/s | 12.24 MB/blk @ 37.2 MB/s | 3.1× bytes |

Per fill, the 300-market block costs **~+47 %**. The residual O(markets) mass is now, in order:
**root (+327 ms), state_write db (+190 ms), settle (+178 ms, of which cache_flush +51 ms is pure
per-market overhead), save_books (+25 ms)**. Engine total *falls* from 10 m to 300 m only because
`phase1_actions` collapses (391 → 34 ms) — that is a difference in the submitted action mix
between the two cells, not a market-count effect, and it is why the engine line must not be read
as evidence of flatness.

**Declared shape vs uniform control — the honest caveat.** `--markets-per-sender K` shipped in
r7 (`121c940`, default K=0 = byte-identical RNG draw), but **no cell in rounds 6–9 ran with
K ≥ 1** — verified by grepping `cell.markets_per_sender` across every `summary.json` under
`/home/18c/bench-results-matched/`: zero hits. **Every 300-market number in this report is the
uniform control**, i.e. the *hardest* shape (each sender touches all 300 markets, so every block
dirties positions across the whole market set: 26.4 k dirty buckets/flush vs 3.1 k at 10 m). The
locality shape the deliverable declares has still never been measured. Flat cost is therefore
**not reached and not yet fairly tested** — running `markets-300-locality-cell` is a
prerequisite, not an optimisation.

### 17. Remaining backlog after round 9 (ranked)

1. **`root-subphase-attribution`** (instrumentation) — root is 415 ms/blk at 300 m, 26 % of the
   block, and completely unattributed; it is now the largest single unexplained term in the
   system. Measure before any more markets-track work, same as r6 did for the engine.
2. **`flush-root-overlap-1deep`** (throughput) — biggest code lever left: overlap flush(N)
   root + state_write with engine(N+1). Worth 853 ms/blk at 300 m (52.8 %) and 262 ms at 10 m
   off the serial exec chain. The r8 split and a root split are its prerequisites.
3. **`settle-passb-parallel-shard-apply`** (markets) — pass B is 185 ms serial at 300 m, now the
   largest engine term; needs a deterministic shard-apply order (determinism review required).
4. **`state-write-db-writeopts-waloff-restack`** (markets) — measured −35 % on state_write db
   (323 → 210 ms, 63–67 MB/s) with the crash-recovery gate already passed; blocked only on
   same-binary ON/OFF control cells at 300 m. Keep default OFF; an 11-block rewind per crash is
   a real cost the operator must opt into.
5. **`stops-dirty-flag-range-scan-restack²`** (markets) — mechanism proven (save_books −18 %,
   −14.9 % per dirty bucket at 300 m), throughput flat. Merge for the phase win once it stacks.
6. **`settle-serial-diet-passb-cacheflush`** (markets) — `cache_flush` 4.3 → 55.4 ms from 10 to
   300 markets is the cleanest pure-O(markets) term left in the engine.
7. **`state-write-batch-cf-breakdown-and-memtable-sweep`** (markets) — which column families own
   the 12.24 MB/block, and whether memtable sizing moves the 37 MB/s effective write rate.
8. **`markets-300-locality-cell`** (markets) — run the declared shape (K ≥ 1). Deliverable, not
   an optimisation; see section 16.
9. **`phase1-actions-fastpath-10m`** (throughput) — 391 ms/blk avg at 10 m, 316 ms in `late_60s`;
   only bites the 10-market track.
10. **`late-cell-drift-compaction-tuning`**, **`bucket-hash-threads-300m-sweep`**,
    **`order-index-map`**, **`positions-bucket-by-trader`** (markets) — the long tail.

Carried forward unchanged from section 11: the `ConflictingCommittedChain` fail-stop incident
(never re-observed in rounds 6–9; the runner still greps for it every cell) and the campaign-wide
`dissemination_clean=false` residue (2 exhausted / ~21 sync_fallback per cell) which appears on
**every** cell of **every** round and has never correlated with a candidate.

### 18. Honest statement: rig vs code, after nine rounds

* **The rig did not change and it is still the binding constraint.** 18 cores shared by 3 bench
  validators + the bench process + a live testnet validator; ~5 cores per validator; `perf(1)`
  unavailable. Two of nine confirm cells in rounds 6–9 were lost to rig events (a dissemination
  collapse and a link-contended launch), both proven non-code by md5-identical binaries. A ~2.9 k
  matched/s pool sd on 120 s cells means **any single-cell delta under ~8 % is unreadable**, and
  most of the deltas in section 13 are under 8 %.
* **What the code actually gained in rounds 6–9:** one mechanism, settle pass A, worth **+49 % at
  300 markets on the same head** (17.5 k → 26.2 k, settle −50 %). The 10-market track moved
  **+3.4 %**, i.e. not at all. Three of four merges bought *measurement*, not speed — and that was
  the right trade, because the block-time mass had moved to a phase (flush, and inside it root)
  that no timer could see.
* **What is now provable and was not before:** state_write is 86 % RocksDB write / 14 % batch
  build; engine time at 300 m is settle pass B > pass A > cache_flush with `phase1_actions`
  collapsed; 300-market cells reach full byte-identical 3-validator agreement including the RPC
  state digest, verified concurrently in ~60 s/node instead of ~4 min/node serial.
* **What is still not proven:** flat cost 10 → 300 markets (section 16 — and never tested on the
  declared locality shape), and any throughput credit for `waloff` or `stops-dirty-flag`, both of
  which show a real phase win with a flat matched/s.
* **The 200 k target is unchanged and still out of reach on this box.** 42.3 k avg / 56.7 k
  best-60 at 10 markets means 200 k is **4.7× the average and 3.5× the best-60**. The ceiling is
  structural, not tuning: **one serial exec thread** where flush (853 ms at 300 m) runs after the
  engine (523 ms) on the same thread, on ~5 cores. Pipelining flush is the last large single-box
  lever and is worth maybe 1.5×; beyond that the only path remains market-sharded execution with
  deterministic cross-shard settlement and one validator per ≥ 16-core host. Rounds 6–9 did not
  change that conclusion — they made it measurable.
