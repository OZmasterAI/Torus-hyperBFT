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
