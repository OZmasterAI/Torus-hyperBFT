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

**Start every cell detached:** `run-cell.sh` refuses to run outside a
`bench-*.service` unit. Start it, or the script that drives several cells,
with `campaign/detach.sh NAME LOGFILE CMD...`: the command then runs as its own
transient systemd --user service, parented by the user manager rather than
your shell. On 2026-10-06 three 300-market runs on ozarchy, started as
descendants of an agent's shell, each lost one process to an outside SIGKILL
(no OOM kill, no kill/tkill/tgkill). `systemd-run --scope` is not enough: a
scope keeps the caller as parent. `BENCH_ALLOW_UNDETACHED=1` overrides for a
deliberate one-off. Wait for the cell through its files (`summary.json`, a
done marker), and stop it with `systemctl --user stop bench-NAME.service`.
Started from outside a bench unit, detach.sh holds the shared build lock
(`flock -o "$HOME/projects/wt/.cargo-heavy.lock"`, the one builders take with
`flock -w 60 -o`) for the whole run, so wrapped builds wait; while a build holds
it the log shows only `build lock: waiting`. Cells a campaign starts from inside
its own bench unit take no lock (the campaign holds it); no `~/projects/wt`, no lock.

Example (record-cell shape, 5 min, 10 markets):

```
# Build separately: a combined build unifies features and changes the node.
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo build --release -p torus-node
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo build --release -p bench-throughput
tools/matched-bench/campaign/detach.sh base-10m-r1 ~/bench-results-matched/base-10m-r1.log \
  tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench base-10m-r1 10 300 76000
```

300-market cell (locality shape, 3 markets per sender):

```
MPS=3 tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench loc3-300m-r1 300 120 76000
```

A/B campaigns of several builds (driver, warm-up, rounds, scoring, pruning):
`campaign/README.md`.

Harness self-test (offline, ~8 s, no devnet / no cargo):

```
python3 tools/matched-bench/test_harness.py
```

## What it does, in order

1. Pre-flight: refuses to run if a bench, a `cargo build`, or a devnet is
   already running; refuses to overwrite a non-empty result dir (`OVERWRITE=1`).
   Never touches the live testnet validator (8555/9090/30333, `~/.cargo-target`).
2. Stages `$TARGET_DIR/release/torus-node` (default `$HOME/.cargo-target-matched`)
   into `<worktree>/target/release/torus-node` (`devnet/wsl/env.sh` hardcodes that
   path) and logs md5 of source and copy — abort on mismatch (stale-binary trap).
   `EXPECT_NODE_MD5=H` / `EXPECT_BENCH_MD5=H` (full md5 or 8+ hex prefix, as
   recorded by the build) abort when the node / bench binary differs, printing
   both (s17: a reflink-seeded target dir kept a stale bench).
   Records the worktree commit + dirty count. NOTE: the runner cannot prove the
   binary was built from that commit — build first, then run.
3. Generates a `MARKETS`-market genesis via `devnet/wsl/gen-3val-genesis.sh`
   (`MARKETS=N OUT=<worktree>/devnet/wsl/genesis-3val.json`) from THIS repo's
   weighted 100k-account genesis: first N pre-seeded markets (ids 1..N), or
   synthetic `S<k>-USD` rows past the base's 100. Funded accounts untouched.
   The generator reuses an existing (gitignored) `testnet/genesis-weighted-full.json`
   as-is, so the cell first refuses one with a non-`0x`+40-hex address or fewer
   `bulk-test` rows than `SENDERS` (s17: a stub-bench test run left 61 rows with
   address `oracle-feed`); move it away or set `FORCE=1` to rebuild it.
4. `CLEAN=1 launch-3val.sh` with the RE-PROOF5 record-cell env
   (`TORUS_BOOK_ROWS=3 TORUS_RESIDENT_BOOKS=1 TORUS_NATIVE_ROOT_CACHE=1
   TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=8
   TORUS_BUCKET_MEMBER_CACHE_MB=256 TORUS_COMMIT_LAG_BACKOFF_CAP=8`; R190 ran
   `TORUS_BOOK_ROWS=2` — the harness moved to mode 3 with the r2 merge, pass
   `EXTRA_ENV='TORUS_BOOK_ROWS=2'` for a mode-2 control) — ambient
   `TORUS_*` vars are unset first, `EXTRA_ENV` is applied last (so it overrides).
   Since plan 9.13 (`feat/compiled-defaults`) the node's compiled defaults equal
   these values except `TORUS_BOOK_ROWS` (consensus-visible, stays Classic until
   the book mode is in genesis; the cell needs the explicit `=3`) and
   `TORUS_COMMIT_LAG_BACKOFF_CAP` (genesis-sourced, default 0); the other
   exports are kept so older binaries run the same configuration.
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
   With `ORACLE_FEED=1 ORACLE_FEED_DRAIN=1` the feed keeps running and the
   drain uses `--feed-live`: only placed/matched/resting must stay unchanged,
   the native mempool may hold the feed's own entries (`--feed-mempool-max`,
   6 per 256 markets) and the execution queue (= exec lag) must stay <=
   `--max-lag` (2) on every sample. No metric splits the mempool by action
   kind, so "bench pool empty" is that proxy. `drain-feed-live.tsv` holds the
   per-sample exec lag and mean ms per native block (`torus_exec_chain_seconds`);
   `drain.json .feed_live` its summary over the final quiet window. The feed
   is then paused and the default drain re-run (`feed-stop-settle/`, up to
   60 s), so this step can take `DRAIN_TIMEOUT` + ~60 s.
   **Liquidation stress** (all off by default; default cells unchanged):
   `LIQ_THIN=N` sets `available` to `LIQ_THIN_AVAIL` (default 1,000,000 TRS)
   for genesis notes `bulk-test 60` .. `bulk-test 60+N-1` before the genesis
   md5 (the cell fails unless exactly N rows match). `ORACLE_SHOCK_BP=S`
   `ORACLE_SHOCK_ROUND=R` (need `ORACLE_FEED=1`) make the feed add +S bp to odd
   market ids and -S bp to even ones from round R on: on the uniform shape the
   even-index senders lose on every position (with `MPS` set, sides follow
   owner rank instead; run-cell warns). With either set, the protocol accounts
   join the state digest (see step 9): the liquidator vault and both ADL
   escrows, `vault-val<i>.json` holds each node's
   `torus_getLiquidatorVault`, and `sampler.csv` gains the liquidation counter,
   vault deficit and (feat/liq-telemetry) the step timer, per-class / scanned /
   acted counters, the pending / deferred gauges, the post-engine tail `_count`
   and the ADL-budget gauges `torus_liquidation_adl_queue(_deficit)` (reported
   only when the node exports them: max, last, drain blocks to queue 0).
   `liq_stress.py <cell> [out.json]` writes `liq-stress.json` (or out.json;
   every sampler column optional, the tail divisor falls back to
   `torus_exec_engine_seconds_count`) (liquidations per node and per class, vault, shock
   height, first / last liquidation, step ms per block (baseline, window,
   window max), the pending timeline and `blocks_shock_to_pending_zero`, exec
   lag around the shock). `torus_liquidation_pending` is an upper bound
   (pending rows plus the scan-window candidates the act budget left
   unclassified; see `docs/monitoring-setup.md`). On a binary without the
   telemetry it falls back to the lower bound from the triggered counter.
   ADL counterparties are in the node logs (info `liquidation: ADL to escrow`
   per account and market, info `liquidation: ADL escrow pairing`; each escrow
   close at debug, `liquidation: ADL close`), not parsed.
9. Agreement (`agreement.jsonl`): heights, block hash + header stateRoot at
   `min(height)-5` via `eth_getBlockByNumber` on every node, a sha256 state
   digest per node (every market's `torus_getOrderBook` + `torus_getOpenInterest`
   + `torus_getBalances` of 50 bench senders), matched/placed/resting/actions
   counters (must be identical), `panicked|FAIL-STOP` and `ERROR` line counts.
   **Liquidation-stress cells only** (`LIQ_THIN>0` or `ORACLE_SHOCK_BP>0`): the
   digest also covers the three protocol accounts of
   `torus_core::liquidation` — `LIQUIDATOR_VAULT` (`0x746f...766c74`,
   "torus-liquidator-vlt") and the two ADL escrows `ADL_ESCROW_LONG`
   (`0x746f...6c6e67`, "torus-adl-escrow-lng") and `ADL_ESCROW_SHORT`
   (`0x746f...736874`, "torus-adl-escrow-sht"; fixed addresses, one per side,
   not per market or derived). Their `torus_getBalances` follow the 50 senders
   (`digest-accounts.txt`), then their `torus_getPosition` in every market
   (`digest-position-accounts.txt`, passed to `digest-node.sh` as a 7th
   argument; account by account, markets in id order; +3 x MARKETS RPCs per
   node, ~900 at 300 markets). `test_harness.py` pins the hex addresses against
   the Rust constants. Default cells pass no positions file and append no
   protocol accounts, so their digest stream and value are byte-identical to
   tools without escrow support (old and new tools stay comparable); the
   escrows hold nothing there, so including them would only cost RPCs.
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
`TORUS_ROCKSDB_PIPELINED_WRITE` (1 since plan 9.13; `0` = off), `TORUS_ROCKSDB_STATS_INTERVAL_SECS` (5).

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

**Multi-crash (s75).** `CRASH_KILL_AT_S` also takes a comma list — several
rejoins in one cell instead of one per cell:

```
CRASH_KILL_AT_S=60,180,300,420,540 tools/matched-bench/run-cell.sh <wt> <label> 10 660
tools/matched-bench/crash-freeze.py /home/18c/bench-results-matched/<label>
```

- Pre-flight: every offset passes the single-kill window rule, offsets strictly
  increase, consecutive kills >= 90 s apart (`crash_kill_list_ok`).
- Kills run sequentially, one `crash-kill.sh` invocation (and one
  `crash_target_ok` check) per kill. A failed kill stops the sequence and fails
  the cell. A kill that would land < 60 s after the previous restart (the
  kill+restart overran) is SKIPPED with the rest — logged in run.log and
  `crash-kill.skipped` (first skipped k); never back-to-back kills.
- Kill 1 keeps the single-kill files; kill k>=2 writes `crash-kill-<k>.json`
  (`kill_seq`: k), `crash-restart-tail-<k>.log`, `crash-<k>.json`. Each tail is
  cut at the next kill's `log_bytes`, so `crash.json` then covers kill 1's
  window only (a single-kill cell scans exactly as before).
- `summary.json` gains `crash_kills` (per kill: `kill_seq`, `kill_ts`,
  `restart_ts`, `down_s`, rewind fields, panic/hole/error counts, `verdict`,
  `fail_reasons`); `headline.crash_gate` FAILs if ANY kill fails.
- `crash-freeze.py` — per-kill chain freeze over `[kill_ts_k, kill_ts_{k+1})`
  (last window to bench end): `down_s`, restart -> first new val0 commit, and the
  longest val0 commit gap. Commit times come from `val0.log.gz` (µs), falling
  back to the 1 Hz `sampler.csv` counter.

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

## Item 6 Phase 2 step 0.2 columns

Node-local counters (never hashed, no state change), sampled in `WIDE_COLS`
and reported per node in `summary.json`. All read 0 / None on an older node
binary.

- `phase_by_node.<val>.cancel_all` (P2-1): `cancel_alls`, `per_native_block`,
  `books_visited_per_cancel_all`, `books_hit_per_cancel_all` (books where the
  sender had orders or stops). Counts user cancel-alls and the liquidation
  step's cancels. Series `torus_exec_cancel_all_total`,
  `torus_exec_cancel_all_books_visited_total`,
  `torus_exec_cancel_all_books_hit_total`. Also `index_entries_end` /
  `index_traders_end`: the cancel-all index size at the window's end
  ((trader, market) entries and traders; 0 while not built), gauges
  `torus_exec_cancel_all_index_entries` / `_traders` set once per native
  block.
- `phase_by_node.<val>.by_id` (P2-1b): `CancelOrder` / `ModifyOrder` executed
  and `books_probed_per_action` (`torus_exec_by_id_actions_total`,
  `torus_exec_by_id_books_probed_total`).
- `phase_by_node.<val>.thread_spawns` (P2-3): threads spawned per site per
  native block (`match`, `settle`, `save_books`, `margin_prepare`,
  `open_orders`, `end_resident`, `flush_digest`, `root_buckets`,
  `load_books`), their total per native block and per minute. Series
  `torus_exec_thread_spawns_<site>`: gauges holding the process total, set
  after every executed block.
- `phase_by_node.<val>.oracle_only_blocks`: exec ms per oracle-only block
  (every native action a `SubmitOraclePrices`, no EVM tx) from the node's own
  timer over bench + drain: `blocks`, `ms_avg`, `ms_p50`, `ms_p95`
  (`torus_exec_oracle_only_block_seconds`, buckets in `buckets.csv`).
- `proc_cpu_by_node.<val>`: user / sys CPU of the node process from
  `/proc/<pid>/stat` (`procstat.raw`, taken with the schedstat snapshots):
  `load` (before -> bench end, divided by the sampler's load-window native
  blocks and fills) and `whole_run` (before -> after, divided by the
  metrics-before/after deltas): `*_ms`, `*_ms_per_native_block`,
  `*_ms_per_1k_fills`.
- `ingest.cancel_by_id`: the bench's `Cancel-by-id` line (cancel / modify
  sent and accepted, id lookups, lookup errors, draws without an own order)
  when the cell sets `CANCEL_BY_ID_FRACTION` / `MODIFY_FRACTION`.

Self-tests: `test_summarize.py` (`main_phase2_step0`),
`test_phase_mapping.py` (`test_phase2_step0_series_are_sampled`),
`test_harness.py` (`test_cancel_by_id_*`).

## In-flight cap block tail (`MAX_IN_FLIGHT`, `IN_FLIGHT_TAIL`)

With `MAX_IN_FLIGHT=N` each bench sender keeps at most N signed actions in
flight. A slot frees when the action is refused by the RPC, times out (nonce +
70 s), or is seen committed by ONE shared block-body tail on val2
(`tools/bench-throughput/src/in_flight.rs`, `run_tail`). `IN_FLIGHT_TAIL`
(bench `--in-flight-tail`) picks how that tail finds new bodies:

| `IN_FLIGHT_TAIL` | tail reads | releases when |
|---|---|---|
| unset (default) or `head` | `eth_blockNumber`, then `torus_getBlockBody` for each height up to it | the node's eth head passes the block |
| `commit` | `torus_getBlockBody(next)` every 200 ms; a `null` result = not committed yet | the committed body is on disk |

Why `commit` exists: `eth_blockNumber` is `eth_head` in
`crates/torus-rpc/src/eth.rs`, min(on-disk applied marker, committed height).
Under item 6 Phase 3 (step 1 and 2) the applied marker reaches the disk only at
checkpoints (`TORUS_CHECKPOINT_INTERVAL_MS`, ~100 blocks at 15 s), so the head
moves in steps of ~100 blocks, slots free in bursts, and the senders stall
(~37 s per 120 s cell in campaign ozarchy-p3c1, results doc section 45.4).
The body is written at COMMIT, in one RocksDB batch with the header
(`persist_committed_block_durably_timed`, called from `dispatch_to_exec`
before execution), and `torus_getBlockBody` reads `CF_BLOCK_BODIES` with no
head or applied-marker check, so the `commit` tail follows the committed chain.

Differences in `commit` mode:

- The body is read before it executes, so `nativeActionStatus` is usually
  absent and every action counts as executed: a cancel-all that later fails
  still resets the sender's open-order estimate (`OPEN_ORDER_BUDGET`).
- A height that stays without a readable body (null or error) for 5 polls
  is skipped (counted `missed`, its actions free by timeout) once it is known
  committed: the next height has a body, or `eth_blockNumber` has passed it
  (it lags under Phase 3 but catches up at each checkpoint). Errors alone never skip a height in this mode
  (it may not be committed yet); head mode skips after 5 errors because the
  height is at or below `eth_blockNumber`.
- bench.log's `In-flight cap ...` line ends in ` | tail=commit`.
  summary.json: `cell.in_flight_tail` (`null` = unset = head) and
  `ingest.in_flight.tail_mode` (from bench.log).

The default stays `head`, so cells run before the knob reproduce. Use `commit`
for Phase 3 nodes (or any node whose eth head lags commit). Needs a bench built
from `bench/inflight-commit-tail` or later; an older bench rejects the flag.

Self-tests: `test_harness.py` (`test_in_flight_tail_*`,
`test_commit_tail_cell_records_the_tail_mode`); bench crate
`in_flight::tests` (`commit_tail_*`, `body_probe_*`, `tail_mode_*`).

## Campaign driver generator (`campaign/make-driver.py`)

A campaign driver is generated from a short JSON spec instead of being copied from an
earlier ~1,000-line driver:

```
python3 -I tools/matched-bench/campaign/make-driver.py SPEC.json -o $T/ozarchy-<camp>-campaign.sh
$T/ozarchy-<camp>-campaign.sh dry            # preflight: paths, tools, staged md5s; prints the cells
$T/ozarchy-<camp>-campaign.sh show           # arms, node env, harness env, env checks, cells (tables)
$T/ozarchy-<camp>-campaign.sh launch smoke   # one 60 s cell with every check (unit bench-<campaign>-smoke)
$T/ozarchy-<camp>-campaign.sh launch         # the campaign (unit bench-<campaign>)
```

The driver = a generated config block + `campaign/driver-body.sh` (the standard parts,
tested once in `test_make_driver.py`). Do not edit a generated driver: change the spec
and regenerate (the spec is embedded in the driver header). Standard parts:

- **Launch** through `campaign/detach.sh` (`launch`): the campaign runs in its own
  `bench-<campaign>.service` unit, which holds the build lock for its whole run; `run`
  refuses to start outside a bench unit. Each cell runs `run-cell.sh` in its own
  `bench-<label>` unit, with `OVERWRITE=1`, as Job 4: the driver's own records (`sysctl.txt`,
  the samplers) are in the cell dir before run-cell.sh starts, and the driver itself refuses
  an existing cell dir. A spec may only repeat `OVERWRITE=1`. The exported env is scrubbed
  to the basics first (no `TORUS_*`, `MALLOC_CONF`, `LD_PRELOAD` or run-cell.sh knob from
  the launching shell). Driver names
  (`DRIVER_VARS`: `RUN`, `P`, `ARMS`, `BDIR`, ...) are owned by the driver: a caller that
  exported one gets the driver's value, un-exported (the generated config `unset`s them
  before assigning; `scrub_env` un-exports them and never unsets them).
- **Per-cell records** once the bench runs, per node pid: `node-environ-trie.txt`
  (`exe_md5` = md5 of `/proc/PID/exe`, the RUNNING exe: a wrapper script has exec'd the
  real node by then), `node-env-all.txt` (`TORUS_*`), `node-preload.txt` (`LD_PRELOAD`,
  `MALLOC_CONF` and every other non-`TORUS_` name the spec sets).
- **Per-cell checks** (CHECK OK / CHECK FAIL lines; a failure flags the row `bad:check` and
  stops the campaign): node exe md5 = the arm's staged md5 on all 3 nodes; the arm's node
  env (`node_env` + arm `env`) on all 3 nodes and every other name the spec mentions
  (any arm's env, `absent_env`) on none, `TORUS_*` read from `node-env-all.txt`, any other
  name from `node-preload.txt`; `summary.json` `cell.markets`; the spec's `expect` lines.
- **Runtime watchers** (every cell; copied verbatim from the hand-written Job 4 driver
  `testdata/make-driver/ozarchy-p4j4-{h,d}-campaign.sh`, which the golden test compares).
  Once the cell unit runs, the driver finds the harness pid (run-cell.sh, the child of the
  unit's main process; none within 30 s = `ABORT`, `bad:abort-no-pid`) and starts
  `watch_nodes` and `memguard`. `watch_nodes` polls the 3 node pids of run.log's `node pids:`
  line, the load generator and the oracle feed every 0.5 s until the harness stop (`stopping
  pid`). A death writes `<cell>/node-death.txt` (`DEATH node val1 comm=torus-node pid=<pid>
  detected at <time> (0.5 s poll; ...); last val1 log line: ...`, also in `node-life.txt`) and
  TERMs the harness (its trap stops the rest). Such a cell is a `CELL FAIL <label> rc=<rc>
  death: ... -> continuing`: no checks run, cell_step flags its row `death` and queues a
  rerun, and the campaign goes on. `memguard` stops the cell unit when MemAvailable < 6 GB.
  After the checks, `hardscan` prints `HARD FINDING` lines for panic, fail-stop and state hash
  mismatch lines logged at WARN or ERROR (it never fails a cell).
- **Samplers** (spec `samplers`, default `true`): `data-du-samples.txt` (data dir sizes,
  MemAvailable, /dev/shm; 2 s), `ckpt-samples.txt` (each node's checkpoint and reject metrics;
  2 s), `mem-samples.txt` + `smaps/` (smaps_rollup, status and memory gauges every 5 s; val0's
  full smaps every 30 s), `node-nofile.txt` + `fds.txt` (open fds; 5 s), `vmstat-1hz.txt`,
  and the hand drivers' 1 Hz soak sidecar `python3 -I $TOOLS/soak-sampler.py <cell> 9161 9162
  9163` (`tools/matched-bench/soak-sampler.py`, copied unchanged from the s58rl / p4soak10
  tools): `metrics-1hz.txt.gz` (per node and second `T <epoch> val<i> ok <scrape_s>|ERR` + the
  /metrics lines without comments or buckets, except the slow-tail buckets le >= 0.128 of its
  latency families: per-second consensus drops and timeouts) and `host-1hz.tsv` (meminfo
  Dirty / Writeback / MemAvailable / Cached and per node RSS, context switches, CPU ticks). It
  starts with the other samplers, waits for run.log's `node pids:` line, ends when the nodes
  are gone and is stopped with them. A frozen `tools_dir` must carry `soak-sampler.py` (else
  `PREFLIGHT FAIL ... soak-sampler.py missing`; also when it does not compile or lacks the
  slow-tail buckets). After the cell `rocksdb-LOG-val<i>.txt`, `data-du.txt`, a `[<label>] fds
  max/end ...` line and a `CHECK SOAK` coverage line, which adds `CHECK WARN <label>:
  metrics-1hz has N ok scrapes (< 3 x dur)` / `host-1hz has N rows (< dur)` when the 1 Hz
  sidecar fell short (recorded, never a failure, as in the hand drivers). A node that held
  more than 16384 open fds stops the campaign after its cell (`STOP`, `bad:stop-fd`).
  `DRIVER show`: `SAMPLERS on du mem metrics fd soak-1hz vmstat rocksdb-log fd_stop=16384` and
  `SOAK on <path> metrics-1hz.txt.gz host-1hz.tsv`; `"samplers": false` turns all of them off.
- **Host checks** (spec `host_checks`, default on): the `run` preflight requires MemAvailable
  >= `min_mem_avail_gb` (40) and every `sysctl` value the spec gives. The dirty limits
  (`vm.dirty_bytes`, `vm.dirty_background_bytes`) that the spec does not give are pinned to the
  host's values at that preflight. Each cell records the writeback and perf sysctls at its start
  and end (`sysctl.txt`) and must still have those values: a change before the start aborts
  (`bad:abort-sysctl`), a change before the end fails the checks (`bad:check`). `quiet()` also
  waits for heavy analysis processes (perf, inferno, flamegraph, jeprof, addr2line).
- **Warm-up gate** (spec `warmup.gate`, default `true`): the warm-up cell and the smoke cell
  (`DRIVER show`: `WARMGATE on w1 s0`) must end with run-cell rc 0 and a summary.json with
  `validity.verdict` ACCEPT, `headline.agreement_verdict` AGREE, `liveness.verdict` PASS and
  `agreement.panic_or_failstop_lines` 0, and no node may die in it; then `CHECK OK <label>
  warm-up gate: validity=... agreement=... liveness=... panic_or_failstop_lines=...`. Else
  `CHECK FAIL <label>: warm-up gate: ...`, its row is `bad:...check`, and the campaign stops
  (`CAMPAIGN STOP at w1`, exit 1) before any measured cell. `"warmup": {..., "gate": false}`
  turns it off for both (`WARMGATE off`): a failed warm-up is then only a bad row and the
  measured cells run. `"warmup": false` keeps the gate on the smoke cell.
- **Campaign checks** (spec `checks`, optional): hard checks that do not fit an `expect`
  grep (metric deltas, took-effect lines per arm, ...), as bash functions in a file you
  write: `"checks": {"source": "/abs/path/checks.sh", "per_cell": ["flagchk", "fanchk"]}`
  (function names `^[a-z_][a-z0-9_]*$`, not a driver function; else SPEC ERROR). Preflight
  (`dry`, `launch`, `run`) sources the file (missing, unreadable or a non-zero return =
  `PREFLIGHT FAIL`), requires each function defined (`declare -F`), and fails when the file
  redefines a driver function or a driver variable (`DRIVER_VARS`). The file is sourced
  inside a function: a top-level `declare -A` / `declare -a` in it needs `-g` (else it is a
  local of that function, gone after the preflight, with no error from bash); preflight prints
  `PREFLIGHT WARN: <file>:<line>: ...` for each one without `-g` (column 0 only) and goes on. After every cell (warm-up,
  smoke, measured; after the standard checks, before the gate) each function is called as
  `fn <label> <arm>` (label = `<label_prefix>-<tag>`, the cell dir is `$R/<label>`), in a
  subshell, with the driver's variables available: `R` (results root), `AENV[arm]` (the
  arm's own node env, `K=V K=V`), `AXE[arm]` (node_env + arm env, run-cell.sh EXTRA_ENV),
  `AHENV[arm]` (harness env), `ABIN[arm]` / `BMD5[bin]`, `RUN`, `P`, `MARKETS`, `RATE`,
  `NODES`, `BASELINE`. It prints its own `CHECK OK` / `CHECK FAIL` lines; a non-zero return
  adds `CHECK FAIL <label>: campaign check <fn> returned <rc> (arm <arm>)` and is a hard
  failure like a failing `expect`: every check still runs, the row is `bad:check`, the
  campaign stops. `DRIVER show` lists them (`CHECKS <source> <fn>...`), `PREFLIGHT OK`
  carries `checks=<fn>,...`. Keep the file beside the frozen tools so it is part of the
  campaign record.
- **cells.tsv rows, CELL-FLAG lines, rerun queue**: `run_spec` + `cell-step.sh` as in the
  template (below). Wrapper arms pass `--no-summary-md5` to the step (run-cell.sh hashes
  the staged file, which is the wrapper).
- **Preflight**: tools present, harness worktree present, the genesis inputs (below), each
  binary's staged md5 (a
  `wrapper` binary: the md5 of the absolute path its last `exec` line runs), bench md5,
  analysis scripts; in `run`: no build, node, bench or other bench unit, no old attempt
  (cells.tsv, rerun-queue.tsv, label dirs), 20 GB free.
- **Done marker**: `$RUN/campaign.done` (`exit=<rc> <time>`) on every exit of `run`,
  including a preflight failure; smoke: `$RUN/smoke/campaign.done`.

**Spec** (JSON; any unknown key, a missing required key, a duplicate arm, a baseline that is
not an arm, an env value with whitespace/quotes/`$`, or a harness key the driver sets
itself is a `SPEC ERROR`, exit 2, nothing written). Required: `campaign` (unit name),
`run_dir`, `label_prefix`, `harness_worktree` (run-cell.sh's `<worktree>`), `tools_dir` (the
frozen tools copy: run-cell.sh, cell-step.sh, cell_step.py, campaign/detach.sh; its root
`tools_dir/../..` must carry the genesis inputs, see below),
`binaries`, `arms`, `baseline`, `duration_s`, `markets`, and `replicates` unless `order` is a
list. Optional: `comment`, `results_root` (`~/bench-results-matched`), `devnet_dir`
(`~/torus-wsl-devnet`), `rate` (76000), `order` (`"rotate"` default: replicate r starts at
arm r-1, as Job 4; `"fixed"`; or a list `["r1:A", "r1:B", ...]`), `warmup` (`{arm, tag,
duration_s, gate}`, default baseline / `w1` / `duration_s` / `true`; `false` = none), `smoke` (`{arm,
duration_s}`, default baseline / 60), `node_env` (node env for every arm), `harness_env`
(run-cell.sh env for every cell, e.g. `BLOCK_CAP`, `MAX_IN_FLIGHT`, `ORACLE_FEED`),
`absent_env` (names that must be unset unless an arm sets them), `expect` (every arm),
`analysis` (`{cmd, metric}` = `CELL_STEP_ANALYSIS_CMD` / `_METRIC`), `max_load` (2),
`checks` (`{source, per_cell}`, campaign checks, below), `samplers` (`true` default, or `false`), `host_checks` (`true` default, `false`, or
`{min_mem_avail_gb: 40, sysctl: {"vm.dirty_bytes": "268435456", ...}}`; unknown keys are a
SPEC ERROR too). The runtime watchers have no switch.
`binaries.<key>`: `stage_dir` (holds `release/torus-node` [+ `release/bench-throughput`]),
`node_md5` or `md5_file` + `node_row` (`"N bdd59888"`: the first two md5s.txt columns),
optional `bench_md5` / `bench_row`, `wrapper` (true: `release/torus-node` is a script that
execs the real node; `node_md5` is the real node's). `arms[]`: `label`, `binary` (required
when there are several), `env` (node env: `TORUS_*` knobs and process env such as
`MALLOC_CONF`), `harness_env` (overrides), `expect`, `note`. `expect[]`: `{file, regex, min=1,
max}`: `grep -E -c regex <cell>/<file>` must be in `[min, max]`. Example:

```json
{"campaign": "ozarchy-p4j4d", "run_dir": "/home/oz/bench-results-matched/ozarchy-p4j4-run/d",
 "label_prefix": "ozarchy-p4j4d-300m", "harness_worktree": "/home/oz/projects/wt/p4j4-bdd59888",
 "tools_dir": "/home/oz/bench-results-matched/ozarchy-p4j4-tools/tools/matched-bench",
 "binaries": {"b": {"stage_dir": "/home/oz/bench-results-matched/ozarchy-p4j4-stage/b-run",
   "md5_file": "/home/oz/bench-results-matched/ozarchy-p4j4-build/md5s.txt",
   "node_row": "N bdd59888", "bench_row": "bench-F ff822b95"}},
 "markets": 300, "duration_s": 120, "replicates": 3, "baseline": "J",
 "harness_env": {"BLOCK_CAP": "400", "MAX_IN_FLIGHT": "4", "ORACLE_FEED": "1", "IN_FLIGHT_TAIL": "commit"},
 "arms": [{"label": "J"},
          {"label": "M1", "env": {"MALLOC_CONF": "dirty_decay_ms:1000,muzzy_decay_ms:1000"}},
          {"label": "M0", "env": {"MALLOC_CONF": "dirty_decay_ms:0"}}]}
```

Full example: `campaign/driver-spec.example.json`; the Job 4 golden specs:
`testdata/make-driver/p4j4-{h,d}.json`.

**Against a frozen or pilot harness.** The generator does not need the current tools: point
`harness_worktree` at the pilot worktree (it launches the nodes) and `tools_dir` at the
frozen copy's `tools/matched-bench` (its run-cell.sh, cell-step.sh, cell_step.py and
campaign/detach.sh run the cells; its root `tools_dir/../..` must carry the genesis inputs,
next paragraph). The campaign-specific hard checks of a hand-written pilot driver go into
`checks`, the warm-up gate is on by default:

```json
{"campaign": "ozarchy-s58x", "run_dir": "/home/oz/bench-results-matched/ozarchy-s58x-run",
 "label_prefix": "ozarchy-s58x-300m",
 "harness_worktree": "/home/oz/projects/wt/s58-pilot-857b9770",
 "tools_dir": "/home/oz/bench-results-matched/ozarchy-s58x-tools/tools/matched-bench",
 "binaries": {"b": {"stage_dir": "/home/oz/bench-results-matched/ozarchy-s58x-stage/b-run",
   "node_md5": "5480e116...", "bench_md5": "..."}},
 "markets": 300, "duration_s": 120, "replicates": 3, "baseline": "C",
 "warmup": {"arm": "C", "gate": true},
 "checks": {"source": "/home/oz/bench-results-matched/ozarchy-s58x-tools/s58-checks.sh",
            "per_cell": ["s58chk", "rlchk"]},
 "arms": [{"label": "C"}, {"label": "ALLfan", "env": {"TORUS_CONSENSUS_DIRECT_FAN": "1"}}]}
```

Then `DRIVER dry` (preflight: the frozen tools, the genesis inputs equal to the pilot
worktree's, the staged md5s, the checks file and its functions) and `DRIVER show` (the
`WARMGATE` and `CHECKS` lines) before `DRIVER launch smoke`.

**Genesis inputs beside the tools copy.** run-cell.sh generates each cell's genesis from
ITS OWN repo root (`MAINREPO` = `<dir of run-cell.sh>/../..`), not from the harness worktree
it is handed (that one only launches the nodes). In a standalone tools copy without them
every cell dies at genesis (`gen-3val-genesis.sh` not found, exit 127).
Freeze them as Job 4 did (`ozarchy-p4j4-tools/{devnet,testnet}` beside `tools/`):

```
T=$R/ozarchy-<camp>-tools; mkdir -p "$T/tools" "$T/devnet" "$T/testnet/lib"
cp -r <harness-wt>/tools/matched-bench "$T/tools/"
cp -a <harness-wt>/devnet/wsl "$T/devnet/"; cp <harness-wt>/devnet/genesis.json "$T/devnet/"
cp <harness-wt>/testnet/gen-weighted-genesis.sh "$T/testnet/"; cp <harness-wt>/testnet/lib/cargo-bin.sh "$T/testnet/lib/"
cp <the earlier cells' testnet/genesis-weighted-full.json> "$T/testnet/"   # untracked, ~29 MB
```

Preflight (`dry`, `launch`, `run`) fails unless `devnet/wsl/gen-3val-genesis.sh`
(executable), `devnet/wsl/bench-validator-keys.json`, `devnet/genesis.json`,
`testnet/lib/cargo-bin.sh`, `testnet/gen-weighted-genesis.sh` and a non-empty
`testnet/genesis-weighted-full.json` are under `tools_dir/../..`, and unless `devnet/wsl`
(except the per-cell output `genesis-3val.json`) and the other tracked inputs equal the
harness worktree's. `PREFLIGHT OK` names the `genesis_root`.

**Not supported** (hand-write the driver from the closest earlier one, as before, and say
which shape did not fit): crash / restart cells (`CRASH_KILL_AT_S`, `KILL_*`: a SPEC
ERROR); per-arm market count, duration or rate; samplers beyond the Job 4 driver's
in-driver ones above (perf off-CPU windows, the cpu, io and task samplers: scripts outside
tools/matched-bench); campaign-specific hard checks
that need more than `fn <label> <arm>` after a cell (checks that run during a cell or
compare cells); more than 3 nodes; several campaigns in one unit (pilot + grid: generate one
driver each and launch them in turn); post-campaign steps (off-CPU processing).

Tests: `python3 tools/matched-bench/test_make_driver.py` (golden: the Job 4 horizon and
decay specs give the arms, env, harness env, cell order and env checks of
`ozarchy-p4j4-{h,d}-campaign.sh`, copied into `testdata/make-driver/`, and their watcher
and sampler functions verbatim, the cell() wiring in order, and their sysctl, MemAvailable
and fd-stop values; env and md5 checks on fixture cells and a fake /proc; the generated
cell() run for real against a fake detach.sh, systemctl and run-cell.sh: a stub node that
exits gives node-death.txt and a `bad:...death` row, memguard stops the unit on a stubbed
meminfo, the soak sampler scrapes fake /metrics servers in the hand drivers' format and is gone
when the cell ends, samplers and host checks off leave no records; the campaign loop against the same
fakes: a failing warm-up gate (each condition, a node death, the smoke cell) stops before the
measured cells, `gate: false` goes on, campaign checks run on every cell with label, arm, `R`
and `AENV`, a failing one is `bad:check` + stop, a missing function or source fails preflight;
bad specs).

**Why launchers copied old drivers instead of the template** (s1156): the template is the
p3gate driver, and its CAMPAIGN-SPECIFIC blocks are most of the file (config, samplers,
~20 hard checks, preflight helpers) built around one checkpoint KNOB per arm (`AKNOB`
0/1), with no per-arm env table and no `MALLOC_CONF` handling; it calls tools that live
in run dirs outside the repo (`$T/cpu-sampler.sh`, `$T3/io-sampler.sh`, the acc/acc3
codec, LSM and restart scripts); and checks added by later campaigns (heap, sysctl, soak,
depth, cloid) were never folded back. The newest driver was always the more complete
start, so each launcher read and edited ~1,000 lines.

## New campaign from the template (per-cell write-up step, rerun queue)

`campaign-template.sh` is the ozarchy-p3gate driver (`docs/perf/bench-archive/tools/`)
with two additions: a write-up step after every cell and a rerun queue at the end.
Start a new campaign from it:

1. Copy the harness tools (they include `cell-step.sh` and `cell_step.py`) and the
   template, and copy the campaign's analyst scripts (at least `a-cells.py`) into
   the tools dir before the launch:
   ```
   T=$R/ozarchy-<camp>-tools; mkdir -p "$T/analyst"
   cp -r <harness-wt>/tools/matched-bench "$T/"
   cp <harness-wt>/tools/matched-bench/campaign-template.sh "$T/ozarchy-<camp>-campaign.sh"
   cp <previous analyst>/a-cells.py "$T/analyst/"
   ```
2. Edit only the blocks between `# >>> CAMPAIGN-SPECIFIC` and `# <<< CAMPAIGN-SPECIFIC`:
   config (paths, shas, knob, arms, `CELLS`, `RUN`, `P`, `ANALYST`,
   `CELL_STEP_ANALYSIS_CMD`), per-cell samplers / harness env, per-cell hard
   checks, preflight. `DRY_RUN=1` runs the preflight only.
3. Launch it detached with its log at `$RUN/campaign.log` (the step appends its
   `CELL-FLAG` lines there):
   ```
   tools/matched-bench/campaign/detach.sh ozarchy-<camp> $RUN/campaign.log "$T/ozarchy-<camp>-campaign.sh" 4
   ```

**The step.** `run_spec` runs one cell, then `cell_step` (from `cell-step.sh`)
in the foreground; the next cell starts only after it returns. It appends one row
to `$RUN/cells.tsv` (header once; tab-separated
`label arm rep rc agree md5_ok metric flag`):

- `agree`: `yes` iff `validity.verdict` is ACCEPT and the agreement verdict is AGREE (raw result).
- `md5_ok`: every running process's `exe_md5` (`node-environ-trie.txt`,
  `node-restart-env.txt`) equals the arm's staged md5; at least one process record is
  required. `summary.json` `binaries.torus_node_md5` is checked too when present; a
  missing one is skipped (the process records are what actually ran).
- `metric`: matched/s from the analysis row (`a-cells.py` `ms`), else
  `summary.json` `headline.matched_s_avg`, else 0.
- `flag`: `ok` or `bad:<reasons>`. Reasons are comma-joined, in this order:
  `md5`, `no-summary`, `death`, `rc-missing`/`rc=<n>`, `disagree`/`agreement-<v>`,
  `verdict-<v>`, the campaign's own failure of the cell (`check`, `stop-tail`,
  `stop-fd`, `abort-*`), `analysis`/`analysis-timeout`, `step-timeout`/`step-error`.

Known artifacts stay `ok` (with `agree=no`). The first is rc 2 from the crash
gate counting the restarted node's INFO line `running state hash fail-stop ...
on=false`; every panic-pattern line in `crash-restart-tail*.log` must be that
line. The second is the mark-only digest: the differing node's digest lines differ
only in the 5 mark fields, and block hash, header root and survivor counters are
equal. A restart cell's `liveness unverified` is also excused. The step records
the evidence in `$RUN/cell-step/<label>.json`.

**Analysis check.** `CELL_STEP_ANALYSIS_CMD` (placeholders `{out} {prefix} {tag}
{arm} {label} {cell} {run}`) must exit 0 within `CELL_STEP_ANALYSIS_TIMEOUT`
(600 s) and write the cell's row (key `{tag}`) to `{out}`. When it does not (an
unparsable command, e.g. unbalanced quotes, included), the cell is flagged `bad:analysis` (or `bad:analysis-timeout`, after the analysis
process group is killed) and gets a rerun. The output goes to
`$RUN/cell-step/<label>.analysis.log`. If the script itself is broken, every cell
is flagged: fix the script, and empty `rerun-queue.tsv` before the main loop ends
if those reruns are not wanted.

**Timeout.** `CELL_STEP_TIMEOUT` (900 s) bounds the whole step. On expiry
`timeout` sends TERM, and KILL 15 s later. `cell_step.py` kills its analysis
child and exits 124. `cell_step` then writes `agree=no md5_ok=no` (unknown),
the metric from `summary.json` (or 0), the flag `bad:step-timeout`, the
CELL-FLAG line and the rerun entry, and returns 124. The campaign continues.

**Step error.** Any other step exit that leaves no row for the label (a crash,
an unexpected error: `cell_step.py` exits 5) gets the same fallback row with
`bad:step-error` and the CELL-FLAG line and rerun; `cell_step` returns 1. A
finished cell is never lost.

**Campaign check failures.** When `cell()` fails a started cell (`return 1`),
`run_spec` passes the reason to the step (`cell_step`'s 9th argument, the
step's `--extra-reasons`): `check` for a failed hard check (the default), or the
`CELL_FAIL` value a failure site outside the hard-check block sets before its
`return 1` (`stop-tail`, `stop-fd`, `abort-detach`, `abort-no-pid`,
`abort-node-left`). The row is `bad:<reason>` (with the step's own reasons
first) even when the run itself was clean, with the CELL-FLAG line and a queued
rerun.

**Bad cells and reruns.** A bad cell's row stays as written. The step adds
`CELL-FLAG <label> <reasons>` to `campaign.log` and queues `<label>-rerun` in
`$RUN/rerun-queue.tsv` (`rerun_label label arm rep spec`). After the main loop,
`cell_step_rerun_pass` runs each pending rerun once: the same spec with tag
`<tag>-rerun`, the same rep, and its own row. A bad rerun is flagged and is not
requeued. A label that already has a row is refused: exit 3, a
`CELL-STEP DUPLICATE` line, and nothing written. When `cell()` returns 1 (ABORT,
CHECK FAIL, STOP), a started cell still gets its row (flagged as above); then the campaign stops,
and the queued reruns stay in the queue unrun.

Self-test: `python3 tools/matched-bench/test_cell_step.py` (fixtures: trimmed
copies of three real p3gate cells in `testdata/cell-step/`).

## EVM load in a cell (`EVM_LOAD_RATE`)

`EVM_LOAD_RATE=R` (transfers/s, default 0 = off) runs `tools/tx-flood` beside the native
load for the cell's load window: R EIP-1559 transfers per second in aggregate (1 TRS each,
21,000 gas, 100 gwei). Off, the cell is unchanged: no tx-flood, no extra file or log line, and
the same bench, oracle-feed and node command lines (pinned in `test_harness.py`).

How it runs:

- **Senders.** `EVM_LOAD_SENDERS=S` (1..40, default 40) of the genesis accounts
  "bench sender 20" .. "bench sender 59" (`testnet/genesis-weighted-base.json`, 1M TRS of EVM
  balance each). Their keys come from `bench-throughput gen-accounts --offset 20 --count S
  --secret-keys`, are written beside the devnet's pids (`$DATA_ROOT/run/evm-load-keys.txt`)
  and are deleted when tx-flood stops. They are none of the load senders (`--sender-offset
  60`), the funded spam keys (the top of the bulk range) or the digest accounts. Pre-flight
  (`evm_load.py funded`) fails the cell unless the genesis funds every sender.
- **One validator per sender, like a wallet.** The k-th sender sends only to val `k % 3`
  (`--fanout pinned`); that node pools the tx and forwards it to the next EVM leader.
  tx-flood's default (`--fanout all`, every tx to every RPC) is not used.
- **Paced.** `--rate R` ticks every 1/R s, round-robin over the senders, one tx in flight per
  sender. A refused tx is sent again with the same nonce after a back-off (1 s for `mempool
  full`, `nonce too far` and HTTP errors; 2 s for the RPC's per-sender rate limit; 5 s for
  anything else), so a sender never leaves a nonce gap. A tick that finds every sender busy or
  backing off counts as `throttled`; a scheduler more than 1 s late skips ticks (`missed`).
  The recipient is another sender, chosen by nonce, so repeated cells send the same txs.
- **Window.** tx-flood starts right after the bench and is stopped (SIGTERM) right after the
  bench exits, before the drain; `--duration-secs DUR` also bounds it.
- **Node limits.** Per sender, a node's RPC admits 50 txs per 10 s, its pool holds 16, and a
  nonce may run at most 64 ahead of the executed one (which a validator reads at its last
  checkpoint). Keep R / S at or below ~2 tx/s. An EVM block holds at most 3M / 21,000 = 142
  transfers, and comes every N = 11 heights: ~41 transfers/s at 3.2 blocks/s.

What it records:

| file / field | content |
|---|---|
| `evm-load.log` | tx-flood's output (a progress line every 10 s) |
| `evm-load-stats.json` | tx-flood `--stats-file`: `sent`, `accepted`, `rejected`, `reject_reasons` (`timeout` = a send unanswered for 12 s), `throttled`, `missed`, `unfinished` (sends still running at the end: aborted, outcome unknown), `per_rpc`, `per_sender` (address, RPC, start and next nonce) |
| `evm-load-scan.json` | `evm_load.py scan` on val0 after the digest: the executed EVM txs of every height from the EVM load's start to the committed head (`eth_getBlockTransactionCountByNumber`; it first waits up to 60 s for val0's eth head, which moves at checkpoints). `evm_txs` (landed), `evm_blocks`, `evm_block_txs` (`[height, txs]`), `off_cadence_blocks` / `off_cadence_heights` (EVM txs at a height with `height % N != 0`, N = the genesis `evm.block_interval`; must be 0), `cadence_heights`, and per sender `landed_by_nonce` / `accepted_not_landed` (`eth_getTransactionCount` latest) |
| `summary.json` `.evm_load` | the above, plus per node `exec_evm_seconds`: the `torus_exec_evm_seconds` sum and count deltas (metrics-before to metrics-after) and ms per EVM block, and `ok` / `problems` (below) |
| `run.log` | `EVM load ON: ...`, the tx-flood command, its stop (rc, height), the scan line and the stats line |

`torus_exec_evm_seconds_sum` is also sampled at 1 Hz in `sampler.csv`, as before (the `evm`
phase in `summarize.py`).

A cell that asked for EVM load and did not get it is rejected, whatever the native verdict:
`.evm_load.problems` lists why (tx-flood rc != 0, stats or scan missing, scan incomplete or with
unreadable heights, nothing accepted, nothing landed, EVM txs off the cadence) and is folded into
`.validity` (REJECT, reason `EVM load failed: ...`), `.status` (INVALID) and
`.headline.benchmark_accepted` (false), so `health.py accept` exits 2 as for any rejected cell.
`resummarize.sh` re-derives `.evm_load` (and the fold) from the retained `evm-load-*` files.

The cell runs at the N of its genesis. `gen-3val-genesis.sh` reuses an existing
`testnet/genesis-weighted-full.json` (untracked), so one generated before N = 11 keeps N = 5:
regenerate it once (`FORCE=1`) and check `jq .evm devnet/wsl/genesis-3val.json` and
`.evm_load.interval` in summary.json.

Build `cargo build --release -p torus-tx-flood`; run-cell.sh reads
`$TARGET_DIR/release/torus-tx-flood`, or `TX_FLOOD=<path>`. Generated campaign drivers pass the
knob in `harness_env` (`"EVM_LOAD_RATE": "20"`, plus `TX_FLOOD` when the stage dir has no
tx-flood); `campaign/run_cell.py` takes `--runner-env EVM_LOAD_RATE=20` and
`--runner-env EVM_LOAD_SENDERS=40`. One cell:

```
EVM_LOAD_RATE=20 TARGET_DIR=<dir with release/{torus-node,bench-throughput,torus-tx-flood}> \
    tools/matched-bench/campaign/detach.sh evm-smoke /tmp/evm-smoke.log \
    tools/matched-bench/run-cell.sh <worktree> evm-smoke 10 60 20000
jq .evm_load ~/bench-results-matched/evm-smoke/summary.json
```

Self-tests: `test_harness.py` (`EvmLoadHarnessTest`, `EvmLoadScanTest`) and the tx-flood crate
(`cargo nextest run -p torus-tx-flood`: rate, pinning, re-send with the same nonce, key file).
