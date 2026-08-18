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
   (`TORUS_BOOK_ROWS=2 TORUS_RESIDENT_BOOKS=1 TORUS_NATIVE_ROOT_CACHE=1
   TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=4
   TORUS_BUCKET_MEMBER_CACHE_MB=256 TORUS_COMMIT_LAG_BACKOFF_CAP=8`) — ambient
   `TORUS_*` vars are unset first, `EXTRA_ENV` is applied last (so it overrides).
   Verifies via `/proc/<pid>/environ` that all 3 nodes got the same env.
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

## P1 pool bounding: `TORUS_CORE_BUDGET` + `CPUSETS` (shared-rig cells)

Every validator used to size its pools from `available_parallelism()` = 18 on
this box: tokio 18, global rayon 18, match workers 18, ingress verify 9, gossip
verify 9, RocksDB bg 4 — x3 validators + the bench on the same 18 cores.
Prior pin-vs-unpin proof (S372) put 75-85% of the in-vivo exec "ceiling" on
contention, not algorithm. Two node-side knobs (both node-local, never
consensus-visible — thread counts change wall-clock only) make that testable
in cells with zero code changes per cell:

| knob | effect |
|---|---|
| `TORUS_CORE_BUDGET=N` | size every pool as if the host had N cores: tokio workers N, global rayon N (unless `RAYON_NUM_THREADS` set), match workers N, ingress/gossip verify `max(2,N/2)`, RocksDB bg jobs `clamp(N/2,1,4)`, RPC runtime `min(4,N)`. Unset/`0` = host (exact-today). |
| `CPUSETS=0-5/6-11/12-17` | `launch-3val.sh` runs val0/1/2 under `taskset -c` on those disjoint lists (`/` or space separated). Unset = no pinning. run-cell logs each pid's `Cpus_allowed_list` + thread count. |
| per-pool overrides | `TORUS_TOKIO_WORKERS`, `TORUS_MATCH_WORKERS`, `TORUS_MAX_BG_JOBS`, `TORUS_INGRESS_VERIFY_THREADS`, `TORUS_GOSSIP_VERIFY_THREADS`, `TORUS_RPC_WORKERS`, `RAYON_NUM_THREADS` — each wins over the budget-derived default. |

Every node logs one `core budget: thread-pool sizing` line at startup
(host_parallelism / core_budget / tokio_workers / rayon_global / rayon_current)
and `rpc runtime workers`; both land in `valN.log.excerpt`.

Suggested cell matrix (2 reps each, 10 markets / 300 s, record shape):

```
# (a) bounded pools, unpinned: 3 x 5 cores of pool budget, 3 cores left for the bench
run-cell.sh $WT r1-budget5-r1 10 300 76000 'TORUS_CORE_BUDGET=5'
# (a') bounded + pinned to disjoint 5-core sets (bench floats on 15-17)
run-cell.sh $WT r1-budget5-pin-r1 10 300 76000 'TORUS_CORE_BUDGET=5 CPUSETS=0-4/5-9/10-14'
# (b) (a) + Phase-2 sharded prepare
run-cell.sh $WT r1-budget5-eng-r1 10 300 76000 'TORUS_CORE_BUDGET=5 TORUS_PARALLEL_ENGINE=4'
# (c) (a) + bigger blocks (ORDERS cap is the intended knob; TOTAL cap 100 binds silently by default)
run-cell.sh $WT r1-budget5-cap-r1 10 300 76000 'TORUS_CORE_BUDGET=5 TORUS_NATIVE_TOTAL_BLOCK_CAP=10000 TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=12000'
```

Compare per-block `exec_engine` / `exec_flush` ms, matched/s (window avg and
best-60), worst-60 blk/s and `validators_agree` against the unbounded baseline
cells. If a bounded combination wins on >=2 reps, promote it into `RECORD_ENV`
here (fleet-uniform — the digest check already proves all 3 nodes got it).

## Rules baked in

- matched/s ONLY from `torus_orders_matched_total` deltas; never bench-side math,
  never placed/s (cheap resting orders — the p3 trap).
- n=1 is not a result: run >=2 reps per cell.
- Never two benches at once; never build while a bench runs (pre-flight guards).
- Determinism is sacred: `validators_agree=false` = candidate REJECTED.
