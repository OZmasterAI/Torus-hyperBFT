# tools/matched-bench — reusable devnet cell runner (matched/s campaign)

One command = one bench cell on the bare-metal WSL 3-validator devnet
(`devnet/wsl`, RPC 8645-8647, metrics 9161-9163). Builds nothing; measures
matched/s from NODE Prometheus counters only; checks 3-validator agreement;
writes a machine-readable `summary.json`.

```
tools/matched-bench/run-cell.sh <worktree> <label> [MARKETS=10] [DUR=120] [RATE=76000] [EXTRA_ENV='K=V ...']
```

Example (record-cell shape, 5 min, 10 markets):

```
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo build --release -p torus-node -p bench-throughput   # in the worktree, BEFORE — never while a bench runs
tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench base-10m-r1 10 300 76000
tools/matched-bench/run-cell.sh /home/18c/projects/wt/matched-bench nosettle-r1 10 300 76000 'TORUS_PARALLEL_SETTLE=0'
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
   TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=4
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
8. Drain: waits until placed/matched/actions/resting counters are UNCHANGED on all
   3 nodes for `QUIET_S` (10) consecutive seconds with exec queue 0 (the mempool
   / exec-queue gauges alone read 0 while actions are still in flight — the
   first smoke run proved that).
9. Agreement (`agreement.jsonl`): heights, block hash + header stateRoot at
   `min(height)-5` via `eth_getBlockByNumber` on every node, a sha256 state
   digest per node (every market's `torus_getOrderBook` + `torus_getOpenInterest`
   + `torus_getBalances` of 50 bench senders), matched/placed/resting/actions
   counters (must be identical), `panicked|FAIL-STOP` and `ERROR` line counts.
   `validators_agree` = spread<=5 AND hashes equal AND digests equal AND
   counters equal AND zero panic/fail-stop AND drained. NOTE: the executed
   native state root is not exposed by RPC/metrics (headers carry the parent's
   root, 0x0 on this branch), so the RPC state digest is the determinism check.
10. `stop-3val.sh`; node logs gzipped into the result dir + an excerpt.
11. `summarize.py` -> `summary.json` (+ `analysis-valN.txt` from the awk scripts).

## Result dir `/home/18c/bench-results-matched/<label>/`

`summary.json` keys: `headline` (matched_s_avg = window average over the bench
window on val0, matched_s_best60 = best sliding 60 s over the whole sample,
placed_s_avg, blk_s_avg, blk_s_worst60, peak_exec_queue_depth, validators_agree),
`funnel_by_node`, `phase_by_node` (per-NATIVE-block ms and % of block / % of wall
for evm, verify, replay_guard, load_books, engine[margin/match/settle],
save_books, flush[root/state_write/evm_resync], body_persist, residual;
exec_thread_busy_fraction; wall ms per committed block), `agreement`, `cpu`,
`cell` (env, bench cmd), `binaries`, `genesis`, `timing`, `bench_log_tail`.

## Rules baked in

- matched/s ONLY from `torus_orders_matched_total` deltas; never bench-side math,
  never placed/s (cheap resting orders — the p3 trap).
- n=1 is not a result: run >=2 reps per cell.
- Never two benches at once; never build while a bench runs (pre-flight guards).
- Determinism is sacred: `validators_agree=false` = candidate REJECTED.

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

Node knobs the cell can A/B via `EXTRA_ENV` (all node-local, format-neutral):
`TORUS_BG_WRITER_CHUNK_KVS` (default 2048; `0` = one trade batch per block as
before r3), `TORUS_BG_WRITER_LOW_PRI` (default 1), `TORUS_ROCKSDB_STATS`
(0/1/2, default 1), `TORUS_ROCKSDB_L0_SLOWDOWN` / `TORUS_ROCKSDB_L0_STOP`
(RocksDB 20/36 when unset), `TORUS_ROCKSDB_MAX_WRITE_BUFFERS` (4),
`TORUS_ROCKSDB_PIPELINED_WRITE` (0), `TORUS_ROCKSDB_STATS_INTERVAL_SECS` (5).
