# tools/matched-bench — reusable devnet cell runner (matched/s campaign)

One command = one bench cell on the bare-metal WSL 3-validator devnet
(`devnet/wsl`, RPC 8645-8647, metrics 9161-9163). Builds nothing; measures
matched/s from NODE Prometheus counters only; checks 3-validator agreement;
writes a machine-readable `summary.json`.

```
tools/matched-bench/run-cell.sh <worktree> <label> [MARKETS=10] [DUR=120] [RATE=76000] [EXTRA_ENV='K=V ...']
```

The devnet is launched from `<worktree>/devnet/wsl` and, since bl4, the cell is
also SCORED by `<worktree>/tools/matched-bench/summarize.py` — whichever copy of
`run-cell.sh` you invoked. Before that, a harness candidate handed to the
integration repo's runner was silently scored with the integration repo's
summarizer, so its whole change was a no-op (bl3 10m-r1). `TOOLS_FROM_WORKTREE=0`
restores the old behaviour, `TOOLS_DIR=<dir>` pins it, and
`RUN_CELL_PRINT_PATHS=1` prints the resolution and exits without touching the
devnet. `resummarize.sh <cell-dir>` re-scores an existing cell in place from its
own `summary.json` provenance.

Example (record-cell shape, 5 min, 10 markets):

```
# Build separately: a combined build unifies features and changes the node.
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo build --release -p torus-node
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo build --release -p bench-throughput
tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench base-10m-r1 10 300 76000
tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench nosettle-r1 10 300 76000 'TORUS_PARALLEL_SETTLE=0'
```

300-market cell (locality shape, 3 markets per sender):

```
MPS=3 tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench loc3-300m-r1 300 120 76000
```

Economic workload overrides retain the default command when unset:
`BAND=5`, `CROSS_FRACTION=0.5`, `CANCEL_FRACTION=0.05`. Bands must be integers
in `1..29999`; fractions must be finite and within `[0,1]`. Effective values
are saved to `workload.json` and `summary.json` under `cell.workload`.

Optional `RATE_SCHEDULE='0:76000,30:120000,60:0,90:76000'` appends
`--rate-schedule` to the economic generator. Offsets are absolute integer seconds
from its timed-window start; rates are aggregate **actions/s**. The first phase
must start at zero, offsets must strictly increase and precede `DUR`, rates must
be finite and nonnegative, and there may be at most 64 phases. This overrides
the scalar `RATE` argument. The runner requires schedule text without whitespace
so its recorded command is unambiguous. Scheduled zero pauses new dispatch
(in-flight work can continue); zero without a schedule keeps the existing
unbounded behavior.
The built benchmark must support the new flag. Planned boundaries are in the
workload manifest; observed timer boundaries are JSON lines in `bench.log` and
`cell.rate_schedule_observed`. These are requested load phases, not achieved
throughput. Scheduled acceptance requires complete, consistent phase records
observed within their intended intervals (`cell.rate_schedule_provenance`).
Late queued HTTP tasks are skipped at their first poll, while in-flight requests
may complete. Scheduled first-round spacing also replaces the legacy random
jitter draw, so use the same declared schedule for scheduled A/B comparisons;
scalar and scheduled action streams are not byte-identical.
Phase-specific throughput/recovery analysis remains separate from
the existing whole-cell acceptance checks.

For example, a proposed deep-book burst cell (not a measured result):

```
BAND=1 CROSS_FRACTION=0.2 CANCEL_FRACTION=0.05 \
RATE_SCHEDULE='0:76000,30:120000,60:0,90:76000' \
tools/matched-bench/run-cell.sh /path/to/worktree deep-burst 10 120 76000
```

Harness self-test (offline, ~8 s, no devnet / no cargo):

```
python3 tools/matched-bench/test_harness.py
python3 tools/matched-bench/test_workload.py
```

## What it does, in order

1. Pre-flight: refuses to run if a bench, a `cargo build`, or a devnet is
   already running; refuses to overwrite a non-empty result dir (`OVERWRITE=1`).
   Never touches the live testnet validator (8555/9090/30333, `~/.cargo-target`).
2. Stages `$TARGET_DIR/release/torus-node` (default `/home/18c/.cargo-target-matched`)
   into `<worktree>/target/release/torus-node` (`devnet/wsl/env.sh` hardcodes that
   path) and logs md5 of source and copy — abort on mismatch (stale-binary trap).
   Records the worktree commit + dirty count. NOTE: the runner cannot prove the
   binary was built from that commit — build first, then run.
3. Generates a `MARKETS`-market genesis via `devnet/wsl/gen-3val-genesis.sh`
   (`MARKETS=N OUT=<worktree>/devnet/wsl/genesis-3val.json`) from THIS repo's
   weighted 100k-account genesis: first N pre-seeded markets (ids 1..N), or
   synthetic `S<k>-USD` rows past the base's 100. Funded accounts untouched.
4. `CLEAN=1 launch-3val.sh` with the RE-PROOF5 record-cell env
   (`TORUS_BOOK_ROWS=3 TORUS_RESIDENT_BOOKS=1 TORUS_NATIVE_ROOT_CACHE=1
   TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=8
   TORUS_BUCKET_MEMBER_CACHE_MB=256 TORUS_COMMIT_LAG_BACKOFF_CAP=8`; R190 ran
   `TORUS_BOOK_ROWS=2` — the harness moved to mode 3 with the r2 merge, pass
   `EXTRA_ENV='TORUS_BOOK_ROWS=2'` for a mode-2 control) — ambient
   `TORUS_*` vars are unset first, `EXTRA_ENV` is applied last (so it overrides).
   Verifies via `/proc/<pid>/environ` that all 3 nodes got the same env.
   `BLOCK_CAP=N` (block-cap-raise sweep) exports the coherent proposer-local
   bundle between the two: `TORUS_NATIVE_TOTAL_BLOCK_CAP=N`,
   `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=max(50000, N*BATCH*1.25)`,
   `TORUS_VERIFIED_SENDER_CACHE_CAP=max(16384, 64*N*2.5)`,
   `TORUS_NATIVE_BLOCK_BYTES_CAP=clamp(N*BATCH*150, 6 MB, 12 MB)`; `BLOCK_CAP=100`
   reproduces the compiled node defaults exactly (control cell). Since the r3
   merge the harness DEFAULTS to `BLOCK_CAP=200` (the sweep winner: +28% matched/s
   vs the same-binary cap-100 control); the node-code default stays 100 (WAN
   dissemination guard, env-only raise until a full-mesh bench earns it). The summary gains
   `cell.block_cap`, `headline.txs_per_block_avg`, `headline.consensus_timeouts`,
   `headline.dissemination_clean` and a `dissemination` block (per-node counts of
   HASH-ONLY manifest pushes, body-fetch exhaustion, sync fallbacks, DA outbound
   failures, body starvation, exec-backlog pacing lines) — a raised cap is
   rejected when `dissemination_clean` is false, whatever matched/s says.
5. Waits for health (all 3 committing, peers>=2), probes idle blk/s for 10 s.
6. Samplers: 1 Hz scrape of all 3 nodes' `/metrics` -> `sampler.csv` (wide,
   one row per node per second: funnel counters + every `torus_exec_*` phase
   histogram sum/count + consensus/mempool gauges), plus per-node
   `funnel-valN.csv` / `phase-valN.csv` in the RE-PROOF5 `scrape.sh` /
   `scrape-phase.sh` layouts so `win60.awk` / `phase60.awk` (copied here from
   `devnet/wsl/results/`) run unchanged. `cpu.csv` every 10 s (load, %CPU/RSS
   per node + bench).
7. Bench (record-cell shape): `bench-throughput consensus --rpc-urls
   http://127.0.0.1:8645 --econ --senders 5000 --sender-offset 60 --markets M
   --batch-size 400 --submit-batch 1 --format bin --concurrency 256 --duration D
   --target-margin 1500 --cross-fraction 0.5 --cancel-fraction 0.05 --band 5
   --rate-total R`. Override `SENDERS`, `CONC`, `BATCH`, `SUBMIT` via env.
   `MPS=K` appends `--markets-per-sender K` (LOCALITY shape, see below) and is
   recorded as `cell.markets_per_sender`; unset = flag omitted = the uniform
   shape every campaign cell so far used. `MPS` needs a `bench-throughput` built
   at or after `cand/r6-harness-300m-digest-and-parity` — **rebuild the bench
   binary before using it.**
8. Drain: requires complete scrapes and unchanged placed/matched/actions/resting
   counters on all three nodes for `QUIET_S` (10) wall-clock seconds, plus empty
   native mempools, idle flush workers, execution queues <=2, and at least one
   new commit on **every** node during that quiet interval. Idle empty blocks
   can leave 1–2 execution entries in flight. Missing metrics, counter resets,
   or gaps over 5 seconds restart the interval. `drain-samples.jsonl` retains
   observations and `drain.json` records the result. `DRAIN_TIMEOUT` defaults to
   `180 + 2*MARKETS` seconds; reaching it does not establish drain.
9. Agreement (`agreement.jsonl`): heights, block hash + header stateRoot at
   `min(height)-5` via `eth_getBlockByNumber` on every node, a sha256 state
   digest per node (every market's `torus_getOrderBook` + `torus_getOpenInterest`
   + `torus_getBalances` of 50 bench senders), matched/placed/resting/actions
   counters (must be identical), `panicked|FAIL-STOP` and `ERROR` line counts.
   NOTE: the executed native state root is not exposed by RPC/metrics (headers
   carry the parent's root, 0x0 on this branch), so the RPC state digest is the
   determinism check.

   **The digest is only evidence if all 3 nodes are digested over the same
   state.** `digest-node.sh` fans one node's per-market/per-account RPCs out
   `DIGEST_PAR` (8) ways while keeping the stream in market-id order (part files
   concatenated in glob order, never completion order — pinned by
   `test_harness.py`), `run-cell.sh` runs all three nodes CONCURRENTLY, and the
   funnel counters are snapshotted either side of that window. 300 markets: ~4
   min/node serial -> well under 1 min/node, all three inside one window.
   Per-RPC timeout is `RPC_TIMEOUT` (60 s, was 10 s).

   Verdict (`agreement.agreement_verdict`, mirrored in `headline`):

   | verdict | meaning | `validators_agree` |
   |---|---|---|
   | `AGREE` | spread<=5, hashes equal, counters equal, **digests equal**, no panic/fail-stop | `true` |
   | `DIGEST_UNVERIFIED` | all of the above except the digest, AND the digest window was not quiescent (counters moved) or the cell never drained | `null` |
   | `DIGEST_UNVERIFIED` | hash + header root + digest all equal, matched/placed/resting equal, and ONLY `native_actions` apart with the digests taken <= 2 blocks apart | `null` |
   | `DISAGREE` | anything else — hash/counter/height divergence, panic, or unequal digests taken over a pinned state | `false` |
   | `INCOMPLETE` | fewer than 3 node rows | `false` |

   `AGREE` is never reported without an equal digest. `DIGEST_UNVERIFIED` is the
   r6-base-300m-r1 shape (equal block hash + equal counters, digests sampled
   minutes apart while the chain still moved): a harness artifact, so it is NOT
   `false`, but it is NOT proof of determinism either — such a cell must be
   re-run before any determinism claim. The second `DIGEST_UNVERIFIED` row is
   the same idea one counter down: `metrics-after-valN.txt` is ONE scrape per
   node while the three digests are taken concurrently, so digests landing a
   block or two apart move `torus_native_actions_processed_total` alone. A
   settled-state counter apart (`matched`/`placed`/`resting`), or an action
   counter apart with the digest heights further than 2 blocks, is still
   `DISAGREE`. `agreement` also carries `state_digest_quiescent`,
   `state_digest_seconds_per_node`, `state_digest_heights`,
   `digest_height_spread` and `action_counter_skew_only`; `timing` carries
   `drained` + `drain_timeout_s`.

   **Crash cells** (`CRASH_KILL_AT_S`, below) score the counters over the
   SURVIVORS only: Prometheus counters are process-lifetime, so the SIGKILLed
   node restarts them at zero and can never match. `counters_equal` is the
   survivors, `counters_equal_all_nodes` / `counters_excluded_node` /
   `counters_compared_nodes` record exactly what was compared. The killed
   node's *state* is not excused — see the crash gate.
10. `stop-3val.sh`; node logs gzipped into the result dir + an excerpt.
11. `summarize.py` -> `summary.json` (+ `analysis-valN.txt` from the awk scripts).

## Result dir `/home/18c/bench-results-matched/<label>/`

`summary.json` keys: `headline` (matched_s_avg = window average over the bench
window on val0, matched_s_first120 = the SAME average restricted to the first
120 s of the bench window, matched_s_best60 = best sliding 60 s over the whole
sample, placed_s_avg, blk_s_avg, blk_s_worst60, peak_exec_queue_depth,
validators_agree, agreement_verdict),
`funnel_by_node`, `phase_by_node` (per-NATIVE-block ms and % of block / % of wall
for evm, verify, replay_guard, load_books, engine[phase1_actions/margin/match/
settle(pass_a/pass_b/cache_flush)/post_engine_tail/engine_untimed],
save_books, flush[root/state_write{build,db}/evm_resync], body_persist, residual;
exec_thread_busy_fraction; wall ms per committed block), `agreement`, `cpu`,
`cell` (env, bench cmd), `binaries`, `genesis`, `timing`, `bench_log_tail`.
Also written: `buckets.csv` (1 Hz histogram BUCKET samples in long format
`ts,node,metric,le,count`) — the source of the p50/p95 columns below.

## Cell-duration parity (`first120`)

A 300 s cell's `matched_s_avg` is dragged down by the late deep-book regime a
120 s cell never reaches, so the two durations are NOT comparable on `avg`.
Every summary therefore also carries `matched_s_first120` / `placed_s_first120`
/ `blk_s_first120` (same counter deltas, window `[t_bench0, t_bench0+120]`) —
compare cells of different durations on `first120`, never on `avg`. On a 120 s
cell `first120 == avg` by construction. `resummarize.sh <dir>` backfills these
onto any existing result dir.

## Load-shape locality (`--markets-per-sender` / `MPS`)

By default every ORDER draws its market uniformly from `1..=MARKETS`, so all
5000 senders touch all 300 books and every block dirties every book. `MPS=K`
gives sender `i` the fixed set `((i*K + j) mod MARKETS) + 1, j in 0..K` instead:
deterministic (pure function of the sender index, identical across reps and
processes), K distinct ids, and every market owned by within-one the same number
of senders — so no book goes dead. `K >= MARKETS` degenerates to the uniform
shape; `K = 0` (default) IS the uniform shape. Pinned by `market_plan_tests` in
`tools/bench-throughput/src/main.rs`.
## Engine sub-phase attribution (r6 engine-untimed-attribution)

`phase_by_node.<val>.phases.engine` now decomposes the engine share that
`phase_margin`/`phase_match`/`phase_settle` never covered. The six new
histograms are observed ONCE PER NATIVE BLOCK (app.rs sums nanosecond
accumulators carried on the exec context across both `execute_batch` calls),
so every `_count` equals `torus_exec_engine_seconds_count` and `_sum/_count`
is ms-per-block directly:

- `phase1_actions_ms` — the Phase-1 non-PlaceOrder action loop (cancels,
  modifies, transfers); a cancel-heavy block pays here and nowhere else.
- `settle_pass_a_ms` / `settle_pass_b_ms` — the parallel settle split
  (scoped-thread per-market plan compute vs the serial deterministic apply).
  `pass_a = 0` means the cell ran the canonical sequential settle.
- `cache_flush_ms` — `pos_cache`/`bal_cache` `flush_all` into the overlay.
  NESTED inside `phase_settle_ms` along with pass A / pass B, so never add
  those three back into the block total.
- `post_engine_tail_ms` — core-writer drain + governance + fees + epoch.
- `engine_untimed_ms` — the residual: engine minus (phase1 + margin + match +
  settle + tail). Whatever is left is the next thing worth instrumenting.

Identity the phase table satisfies by construction:
`phase1_actions + phase_margin + phase_match + phase_settle +
post_engine_tail + engine_untimed == engine`.
`summarize.py` prints them on an `ENGINE val0:` line. All six read `0.0` on a
pre-r6 node binary — which is itself the 'binary is stale' tell.
Harness self-test: `python3 tools/matched-bench/test_summarize.py`.

## Rules baked in

- matched/s ONLY from `torus_orders_matched_total` deltas; never bench-side math,
  never placed/s (cheap resting orders — the p3 trap).
- n=1 is not a result: run >=2 reps per cell.
- Never two benches at once; never build while a bench runs (pre-flight guards).
- Determinism is sacred: `agreement_verdict=DISAGREE` = candidate REJECTED.
  `DIGEST_UNVERIFIED` is not a rejection and not an acceptance — re-run the cell.
- Compare cells of equal duration on `matched_s_avg`; compare across durations
  on `matched_s_first120`.

## RocksDB write-stall attribution (r3 exec-write-stall-attribution)

`phase_by_node.<val>.rocksdb` in `summary.json` (and the matching
`torus_rocksdb_*` columns of `sampler.csv`) carry the DB-wide picture over the
window: `stall_ms_per_s` / `stall_ms_per_native_block` (RocksDB
`rocksdb.stall.micros` — write-controller stalls: L0 / memtable / pending-
compaction triggers), `writes_per_s_self` vs `writes_per_s_other` (write-group
leaders vs followers: a high `other` share = writes queueing behind another
thread's batch), WAL / flush / compaction MB/s and compaction CPU cores, memtable
/ immutable / L0-max / pending-compaction gauges, `delayed_write_rate`,
`write_stopped`, `trade_writer_queued_batches`. With
`EXTRA_ENV='TORUS_ROCKSDB_STATS=2'` the node also exports RocksDB's own
`db.write.micros` / `db.write.stall` histograms (`db_write_ms_avg`,
`write_stall_ms_avg`, `db_write_p99_ms_last`). Two split timers:
`exec_body_persist_put_ms_per_call` (the exec-time body put alone, encode
excluded) and `commit_persist_ms_per_call` (consensus-thread FIX 1a batch).

## Flush `state_write` split (r7 state-write-build-vs-db-split)

`phase_by_node.<val>.phases.flush` splits the old lumped `state_write_ms` into:

- `state_write_build_ms` — serializing the pending overlay maps into the
  `WriteBatch` (`append_to_batch`). CPU, parallelizable -> a parallel
  bucket-encode lever.
- `state_write_db_ms` — the atomic `rocksdb::write(batch)` alone (WAL +
  memtable, where a write stall lands) -> a `WriteOptions`/WAL/memtable lever.
  The trie/mirror puts appended during the root phase ride in this batch; their
  *append* cost stays in `root_ms`.
- `state_write_batch_kb` — mean bytes handed to RocksDB per native block, and
  `state_write_db_mb_per_s` — the throughput that implies.

`state_write_build_ms + state_write_db_ms == state_write_ms` to rounding by
construction; `state_write_ms` is unchanged so the series stays comparable
across the split. On a pre-r7 binary the three new fields read 0.0 / null.
Metric names: `torus_exec_state_write_build_seconds`,
`torus_exec_state_write_db_seconds`, `torus_exec_state_write_batch_bytes`.

Node knobs the cell can A/B via `EXTRA_ENV` (all node-local, format-neutral):
`TORUS_BG_WRITER_CHUNK_KVS` (default 2048; `0` = one trade batch per block as
before r3), `TORUS_BG_WRITER_LOW_PRI` (default 1), `TORUS_ROCKSDB_STATS`
(0/1/2, default 1), `TORUS_ROCKSDB_L0_SLOWDOWN` / `TORUS_ROCKSDB_L0_STOP`
(RocksDB 20/36 when unset), `TORUS_ROCKSDB_MAX_WRITE_BUFFERS` (4),
`TORUS_ROCKSDB_PIPELINED_WRITE` (0), `TORUS_ROCKSDB_STATS_INTERVAL_SECS` (5).

## Exec critical chain vs 100 ms (bl1 exec-chain-sub-100-attribution)

`block_ms` is the exec thread's wall per native block **including the cost of
the empty blocks in the window**, so it is not the number the campaign is
driving to 100 ms. `phase_by_node.<val>` now also carries the chain itself:

- `chain_ms` — the SERIAL CRITICAL CHAIN on the exec thread (E) per NATIVE
  block. `torus_exec_chain_seconds` is observed once per native block on the
  same clock `exec_block_seconds` uses, so on a serial binary
  `chain_ms + empty_block_ms == block_ms` exactly.
- `empty_block_ms` — the exec-thread cost of the window's empty blocks,
  expressed per native block (`block_ms - chain_ms`).
- `pipelined_ms` — wall a flush worker (W) spent per native block.
  **0.0** when the binary has no worker (the `torus_flush_worker_seconds`
  series exists but was never observed); `worker_present` says which.
- `handoff_wait_ms` — time E spent blocked handing a job to W. 0.0 on a serial
  binary, where the series still exists with `_count == chain _count` — that
  is how "zero wait" is told apart from "series absent".
- `gap_to_100ms` = `chain_ms - 100`.
- `fills_per_native_block` and `engine_ms_per_1k_fills` — **mandatory next to
  any chain number.** Engine ms scales with fills, so a thinner block reads as
  a chain win unless the denominator is on the same line.
- `native_blk_s` / `empty_blk_s` — the cadence split (native vs empty blocks
  per second), from `exec_block_seconds_count` vs `exec_engine_seconds_count`
  and the `torus_exec_native_blocks_total` counter.
- `commit_interval_ms_p50` / `_p95`, `chain_ms_p50` / `_p95`,
  `handoff_wait_ms_p95`, `pipelined_ms_p95` — from the histogram buckets in
  `buckets.csv` (the existing `commit_interval_ms_avg` hides the tail).
- `phases.save_books.save_books_drain_ms` / `save_books_write_ms` — production
  timers around pass 1 (journal DRAIN + level digests; reads the LIVE book
  levels the next block's engine mutates, so it can never leave the exec
  thread) and pass 2 (overlay WRITES; the only half a flush worker could take).
  Both 0.0 in book modes 0/1, which have no two-pass save.
- `chain_identity` — the ruler's own per-node-cell gate: `chain_covers_e_phases`
  (the chain must cover every phase still on E — including `flush` when
  `worker_present` is false) and `chain_le_block`.

**Every one of these reads `null`, never 0.0, on a pre-bl1 node binary** — a
0 ms chain would read as the campaign's goal reached instead of a stale
binary. That is itself the "your node build is old" tell.

The drain step additionally waits for `torus_flush_worker_depth == 0` on all
three nodes before the state digest, so the determinism digest is never taken
while a block's state batch is still undurable on a worker. Identically true
on a serial (and on a pre-bl1) binary, so it changes no existing cell.

Metric names: `torus_exec_chain_seconds`, `torus_exec_handoff_wait_seconds`,
`torus_flush_worker_seconds`, `torus_flush_worker_depth`,
`torus_exec_save_books_drain_seconds`, `torus_exec_save_books_write_seconds`,
`torus_exec_native_blocks_total`.

Fixture test: `python3 tools/matched-bench/test_summarize.py` covers a SERIAL
and a PIPELINED binary shape plus the pre-r6 and pre-bl1 fallbacks.

## Worker-aware phase accounting (bl3)

`block_ms` and every `phases.<k>.ms` come from timers on the **exec thread**.
With `TORUS_EXEC_PIPELINE=1` the flush stage is observed on the **flush worker
(W)** instead, so its ms are wall time on another thread and are NOT part of
that block wall. summarize.py therefore reports flush as an **off-chain** line
when `worker_present`:

- `phases.flush.off_chain = true`, `pct_of_block = null` (it has no share of the
  exec block), `pct_of_wall` kept (that is W's load), `ms` unchanged;
- flush is excluded from the per-block sum, so `residual_untimed` is the exec
  thread's genuinely untimed remainder and the percentages close at 100 %;
- `phase_by_node.<val>.off_chain_phases` and `chain_identity.off_chain_phases`
  name what moved.

Before this, a pipelined cell reported `residual_untimed = -212 ms` and phase
percentages summing to 134 % (bl2 `on-10m-r2`). Re-running the four bl2 cells
through the new summarizer moves ONLY that number:
`on-10m-r2` −212.02 → +42.35, `on-10m-r3` −201.06 → +36.82, both OFF cells
byte-identical (+35.54 / +32.23); `block_ms`, `chain_ms`, `pipelined_ms`,
`chain_identity` and every matched/s figure are unchanged.

## Crash gate (bl3) — `CRASH_KILL_AT_S`

The gate that has to pass before `TORUS_EXEC_PIPELINE` can default to ON. With
the flush worker attached, block N's state batch **and** its applied-height
marker are written by W while E is already executing N+1; the marker is the
crash fence, so a `kill -9` must rewind no further than the work that was
committed-but-unexecuted anyway (+ the depth-1 hand-off), and the restarted node
must still converge byte-identically with the two survivors.

```
CRASH_KILL_AT_S=50 KILL_NODE=val1 \
  tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench \
  bl3-crash-on-r1 10 120 76000 'TORUS_EXEC_PIPELINE=1'
```

- `CRASH_KILL_AT_S=N` — SIGKILL the target N s into the bench window, then
  restart it from the **same data dir** with the **same argv**
  (`devnet/wsl/start-node.sh` is the single implementation, shared with
  `launch-3val.sh`), appending to the same log. Must sit inside the load window
  (`>= 10` and `<= DUR-30`): a kill during the drain hangs the agreement probe.
  40-60 on a 120 s cell.
- `KILL_NODE` — `val1` (default) or `val2`. **Never val0**: it serves the bench
  RPC and every headline/phase number.

Safety: `crash-kill.sh` refuses to signal anything that is not this devnet's
val1/val2 — the pid must be listed in the devnet's own `pids` file, its
`/proc` cmdline must carry `--data-dir=$DATA_ROOT/data/val<idx>`, and any
testnet-shaped marker (`/testnet/data`, `.cargo-target/release/torus-node`,
`--keystore`, `:8555`, `:9090`, `:30333`) or a pid in `$TORUS_PROTECTED_PIDS`
is an immediate refusal. The live validator's real cmdline is a test fixture in
`test_harness.py`. The restarted pid REPLACES its line in the pids file, so
`stop-3val.sh` still stops the whole devnet (an orphan node would hold 8646 and
break every later cell).

`summary.json` gains a `crash` section and `headline.crash_gate`
(`PASS` / `FAIL`, `null` on a cell that did not run the gate):

- `rewind_blocks` — `committed - applied` from the node's own replay line
  (`crash recovery: execution gap detected, replaying committed_height=… applied_height=… gap=…`);
  `applied_height` IS the durable marker at the instant of the kill.
- `exec_queue_depth_at_kill` — committed-but-unexecuted blocks already queued
  on E when it died (that part of the rewind is inherent, pipeline or not).
- `rewind_beyond_exec_queue` = the two subtracted — **what the pipeline cost.
  Must be <= 2** (depth-1 hand-off + the in-flight batch).
- `pipeline_flag_confirmed` — the restarted node logged
  `bl2 exec pipeline ENABLED`. Required when the cell env has
  `TORUS_EXEC_PIPELINE=1`, otherwise the gate crash-tested the serial path and
  proves nothing about the flag it is meant to unblock.
- `fail_reasons` — why a FAIL failed. A panic/fail-stop line, an unhealed
  execution hole, or a node that never came back also fails the gate.

**How the killed node is judged.** Its Prometheus counters reset on restart, so
they are excluded from the comparison (`counters_excluded_node`) — and nothing
else is. The gate checks by name, across ALL THREE nodes including the killed
one: `block_hash_equal`, `header_state_root_equal`, `state_digest_equal`, a
quiescent digest, zero panic/fail-stop lines fleet-wide, and survivor counters
that still match each other. A forked killed node therefore still FAILs, and
the gate keeps its teeth independently of `agreement_verdict`.

Artifacts: `crash-kill.json` (record at kill time), `crash-restart-tail.log`
(everything the node logged after the restart), `crash.json` (the merged input
to summarize.py).

## Liveness and benchmark acceptance

`AGREE` describes state consistency independently of performance acceptance.
`liveness.verdict` checks all three validators over bench start through drain:
30 seconds of unchanged commits with continuously pending work is `FAIL`, even
if progress later resumes. Missing metrics, counter resets, gaps over 5 seconds,
and incomplete window coverage are `UNKNOWN` unless a stall is already proven.
A complete window with no commits fails even with empty pending-work gauges.
The 30-second threshold is an operational rejection rule, not a calibrated
performance-noise screen or a claim that shorter pauses are harmless.

`validity.verdict` is `ACCEPT`, `REJECT`, or `UNVERIFIED`. Acceptance requires
liveness PASS, established drain, successful load generation, AGREE, complete
clean dissemination evidence, and a passing crash gate when present. The runner
preserves results and exits **2** for rejected/unverified cells. Report creation
and `resummarize.sh` can still succeed; use `health.py accept <summary.json>`
when a report's acceptance should control a script's exit status. Crash-induced
counter resets currently make performance liveness UNKNOWN; the separate crash
verdict remains available.

`timing.drained_reported` preserves the original runner observation; a proven
stall clears `timing.drained`. Re-scoring cannot recover missing scrape evidence
that an older sampler already replaced with zero. Copy historical results before
re-scoring if their original summaries must be retained.

`collect_logs.py` streams logs once for dissemination counts and records queue
warning counts/bytes in `log-summary.json`. It avoids whole-log shell variables;
it does not change node logging or repair dissemination failures.

Regression checks (no devnet or build):

```sh
python3 tools/matched-bench/test_health.py
python3 tools/matched-bench/test_summarize.py
python3 tools/matched-bench/test_harness.py
```
