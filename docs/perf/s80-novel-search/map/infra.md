# System map: local experimentation infrastructure (infra)

Scope: what exists for SMALL local A/B experiments on ozarchy (no queued benches): microbenches,
timing tests, `bench-throughput`, the one-cell devnet runner, the build setup. Repo
`/home/oz/projects/torus-economy/Torus-hyperBFT` at main `eb993e3f` (read-only here). Measured numbers
come from existing result dirs under `~/bench-results-matched` (named per line). Estimates are marked "est.".

Host (live check 2026-10-22 05:19): `ozarchy`, AMD Ryzen 9 5950X 16C/32T, 1 NUMA node, 64 MiB L3 (2 CCDs),
62 GiB RAM, `/home` 254G with 131G free, load1 1.6, no `bench-*` unit and no torus-node running. Kernel is
BORE (memory 0baab823). Tools: `perf` (paranoid=2), `pidstat`, `iostat`, `jq`, `cargo-nextest`, `sccache`,
`mold`, `clang`. Not installed: hyperfine, samply, flamegraph, critcmp. rustc/cargo 1.98.1
(`rust-toolchain.toml` = unpinned `stable`).

## 1) Architecture and data flow

Three tiers, from cheapest to most realistic:

| Tier | Entry point | What runs | Wall per run | Fidelity |
|---|---|---|---|---|
| T0 criterion | `cargo bench -p <crate> --bench <name>` | one function or `execute_batch` in-process | 10 s to a few min | narrow and old shapes (see 5) |
| T1 `#[ignore]` ubench tests | `cargo test --release -p <crate> --test <ubench> -- --ignored --nocapture` | multi-block exec over `NativeStateOverlay` on a temp RocksDB | ~32 s (ubench_econ, 3 runs) | closest in-process proxy for the exec engine |
| T1' `bench-throughput matching-engine` | `<target>/release/bench-throughput matching-engine ...` | one or two `execute_batch` calls | seconds | engine only, random shape |
| T2 one devnet cell | `tools/matched-bench/run-cell.sh <wt> <label> MARKETS DUR RATE EXTRA_ENV` under `campaign/detach.sh` | 3 validators + load generator + oracle feed on one host | 120 s cell = 3m05s-3m17s | the scoring instrument (matched/s from node counters) |
| T3 campaign | `campaign/make-driver.py` / `ab-driver.sh` | interleaved arms, quiet checks, pruning | ~12.6 min per 300 s cell (campaign README) | queue-only; out of scope here |

Data flow of a T2 cell (`tools/matched-bench/run-cell.sh`, step markers at the line numbers):
1. Pre-flight `:816-1090`: binaries exist (`$TARGET_DIR/release/{torus-node,bench-throughput}`, default
   `TARGET_DIR=$HOME/.cargo-target-matched` `:498`), tool checks, refuses if a `bench-throughput consensus`
   is running (`:1263`), refuses outside a `bench-*.service` unit (`:492-496`, override
   `BENCH_ALLOW_UNDETACHED=1`), refuses a non-empty results dir unless `OVERWRITE=1` (`:1278`).
2. Stage binary `:1413`: copies `torus-node` into `<wt>/target/release/torus-node` (`DST_NODE :583`, because
   `devnet/wsl/env.sh` hardcodes `BIN=$REPO/target/release/torus-node`), md5-proves src == dst, optional
   `EXPECT_NODE_MD5` / `EXPECT_BENCH_MD5`.
3. Genesis `:1428`: `$MAINREPO/devnet/wsl/gen-3val-genesis.sh` writes `<wt>/devnet/wsl/genesis-3val.json`
   (`GENESIS :584`). `MAINREPO = <run-cell.sh dir>/../..` (`:466`), so genesis inputs, including the
   gitignored `testnet/genesis-weighted-full.json` (built if absent; absent today in main and in
   `wt/9k-*`, `wt/9g-*`), come from the repo the SCRIPT lives in. `GENESIS_CACHE_DIR` reuses a keyed
   genesis (`genesis_cache.py`).
4. Launch `:1554`: wipes `TORUS_*`, exports `RECORD_ENV` (`:613-621`), the `BLOCK_CAP` bundle
   (`:700-719`), optional ANTISPAM / attest, then `EXTRA_ENV`; `CLEAN=1 devnet/wsl/launch-3val.sh` (wipes
   `$DATA_ROOT/data`, default `~/torus-wsl-devnet`). Fixed ports: RPC 8645-8647, metrics 9161-9163, P2P
   UDP/QUIC 30401-30403 (`devnet/wsl/env.sh`). Checks that the TORUS_* env digest is one value on all nodes.
5. Health `:1601` (all 3 committing +5, peers >= 2), 10 s idle blk/s probe, optional pinning
   (`HOTSTUFF_CPUS`), oracle feed (`ORACLE_FEED=1`) until marks are fresh on all markets, optional PRELOAD.
6. Samplers `:1753` (1 Hz `/metrics` of 3 nodes -> `sampler.csv`, `funnel-val*.csv`, `phase-val*.csv`,
   `buckets.csv`, `metrics-1hz.txt.gz`; schedstat/procstat snapshots of named threads).
7. Bench `:1813`: `bench-throughput consensus --econ --senders 5000 --sender-offset 60 --markets M
   --batch-size 400 --submit-batch 1 --format bin --concurrency 256 --duration DUR --target-margin 1500
   --cross-fraction 0.5 --cancel-fraction 0.05 --band 5 --rate-total RATE [+ budget/in-flight flags]`.
8. Drain `:1943` (`health.py drain`, quiet counters for 10 s), after-snapshots + agreement (`:2046`, same
   block hash at a common height), state digest (`:2099`, `digest-node.sh`, all nodes concurrently),
   stop + collect logs (`:2334`), analysis + `summarize.py` -> `$RESULTS_ROOT/<label>/summary.json` (`:2383`).

Scoring rules (README "Rules baked in"): matched/s only from `torus_orders_matched_total` deltas (never
bench-side math, never placed/s); n=1 is not a result; DISAGREE = reject; DIGEST_UNVERIFIED = rerun;
cells of different durations compare on `matched_s_first120`, never `avg`.

## 2) Threads / pipeline and what is serial

Harness level (everything serial on one host):
- One cell at a time per host: fixed ports and data root, `pgrep` guard; campaigns add a global flock
  (`$RESULTS_ROOT/.bench-global.lock`, `campaign/bench-guard.sh`).
- Build lock: `$HOME/projects/wt/.cargo-heavy.lock`. Framework gate 26
  (`~/.torus/hooks/gates/gate_26_build_lock.py`) blocks heavy cargo commands (build/check/clippy/test/
  nextest/bench/run/doc/install) unless they run as `flock -w N -o "$HOME/projects/wt/.cargo-heavy.lock"
  cargo ...` (`-o` so sccache does not inherit the lock; `-w` required in the foreground). `campaign/detach.sh`
  holds the same lock (`flock -o`, no timeout) for the whole cell when started outside a bench unit, so a
  build waits behind a cell and a cell waits behind a build ("build lock: waiting").
- Cell step order is strictly sequential: genesis -> launch -> health -> idle probe -> feed -> load ->
  drain -> agreement -> digest -> stop -> summarize. Measured split of a 120 s, 300-market cell
  (`ozarchy-9j2-std-smoke-s0/run.log`): genesis build 7 s (0 when cached), launch+health 7 s, idle probe
  10 s, oracle freshness 3 s, load 123 s, drain 12 s, digest 7 s, attest/log scans/summary 16 s -> 192 s.

Node level (what the cell observes; thread names from `crates/*`):
- `hotstuff-algo` (consensus, `crates/hotstuff_rs/src/algorithm.rs:153`): propose/vote/QC, block build
  (select + mirror + attest, `crates/torus-consensus/src/app.rs:10289`, `:10311`), validate, commit persist.
- `torus-execution` (`app.rs:8335`): the serial per-block exec chain. Per native block, in order: verify
  (EIP-712 batch verify, `crates/torus-types/src/eip712.rs:1372`) -> replay_guard -> load_books ->
  engine (`NativeExecutor::execute_batch_phases`, `crates/torus-bridge/src/native_executor.rs:8281`:
  Phase 1 non-place actions `:8371` serial; Phase 2 margin reserve/ids/partition `:8554` serial unless
  `TORUS_PARALLEL_ENGINE>=2` (default OFF, `:1092-1125`); Phase 3 matching `:8819` parallel per market on
  `torus-exec-pool-*`; Phase 4 settle `:8939`: pass A parallel, pass B serial canonical apply, then the
  serial cache flush) -> action_status -> save_books -> end_resident_wait. Off the chain: `end_resident`
  (`torus-end-resident`, `native_executor.rs:3963`), flush (`torus-flush-worker`,
  `crates/torus-consensus/src/exec_pipeline.rs:830`), checkpoint merge/drop threads (`app.rs:437`, `:493`).
- Pools per node: `torus-exec-pool-*` sized max(host parallelism, `TORUS_MATCH_WORKERS`,
  `TORUS_SETTLE_WORKERS`) = 32 (`crates/torus-bridge/src/exec_pool.rs:20-35`); `verify-*`
  (`eip712.rs:1256`), `torus-ingress-verify-*` (`crates/torus-rpc/src/torus.rs:678`),
  `torus-gossip-verify-*` (`crates/torus-node/src/main.rs:1004`), `rpc-worker` (`main.rs:1479`).
  Measured 127 threads per node (`mem-samples.txt`, 9j2 smoke). Three nodes share 32 hardware threads.
- Exec is the binding stage in every standard cell: `exec_thread_busy_fraction` 0.825, peak exec queue
  depth 66 (queue ~64), commit cadence bursty (commit interval p50 10 ms, p95 367 ms).

## 3) Hot paths with measured cost

Source A = `ozarchy-9j2-std-smoke-s0` (120 s, 300 markets, CL21 env, binary 29bfc705 md5 f4f2679f).
Source B = `ozarchy-9j2-std-r1-R` (300 s, Classic hash-off reference, same binary). Source C =
`ozarchy-9j2-deep-r1-R` (300 s, PRELOAD 1M resting). All val0 `summary.json`, ms per native block.

| Path | A (std 120 s) | B (std 300 s) | C (deep 300 s) | Notes |
|---|---|---|---|---|
| exec chain_ms (serial critical chain) | 40.9 (p50 3.7, p95 240) | 41.6 | 54.1 | heavy-tailed |
| engine total | 34.1 (83% of block) | 35.0 | 45.4 | `exec_engine_seconds` |
| . phase_margin (Phase 2, serial) | 11.7 | 10.1 | 13.3 | largest single engine leg |
| . phase_settle (incl. pass A/B, cache_flush) | 12.2 | 11.9 | 15.5 | |
| .. cache_flush (serial) | 5.9 | 5.3 | 6.8 | pos/bal cache -> overlay |
| .. settle pass B (serial) | 4.2 | 3.8 | 5.0 | |
| .. settle pass A (parallel) | 0.8 | 2.5 | 3.2 | |
| . phase_match (parallel) | 2.5 | 4.5 | 5.8 | |
| . post_engine_tail | 3.8 | 3.6 | 4.5 | core writer, governance, fees, epoch |
| . phase1_actions (cancels etc.) | 2.3 | 2.3 | 3.0 | 24.6-30.6 cancel-alls/blk, ~197 books visited each |
| . engine_untimed | 1.7 | 2.7 | 3.3 | |
| verify | 2.9 (7%) | 2.6 | 3.4 | |
| end_resident (off chain) / its wait | 4.5 / 1.5 | 3.9 / n/a | 5.3 / n/a | |
| save_books / checkpoint_upkeep | 0.55 / 0.7 | 0.54 / n/a | 0.74 / n/a | checkpoint_upkeep p99 16.3 ms |
| engine ms per 1k fills | 2.56 | 3.0 | 3.0 | fills/blk 13.3k / 11.6k / 15.2k |
| hotstuff: view / block_build / qc_collect | 52.5 / 9.7 / 39.7 | 45.0 / 9.3 / 32.9 | 59.4 / 12.0 / 40.4 | whole-run window |
| . build select / mirror / attest | 4.9 / 3.9 / 0.85 | | | |
| . insert_persist / on_committed / DA reconstruct / commit_persist | 7.1 / 7.9 / 5.5 / 4.7 | 8.5 / 5.9 / 4.7 / 4.6 | 9.2 / 10.4 / 5.8 / 7.8 | |
| thread on-CPU per committed blk: exec / hotstuff | 37.0 / 13.0 | 30.9 / 11.8 | 41.1 / 14.9 | schedstat |
| whole node user CPU per native blk / per 1k fills | 289 / 19.3 | 252 / 20.8 | 344 / 21.4 | procstat; + sys 22-30 ms/blk |
| node CPU (avg cores) | 3.8-4.2 per node, bench 1.4 | | | `cpu.pcpu_avg_by_proc`; load1 avg 24.6 |

Headline outputs: A matched/s 270.3k, blk/s 18.1, placed/s 357.7k; B 266.6k / 22.0; C 262.0k / 16.3.
Order age (now - nonce, `torus_order_age_*_seconds`, `crates/torus-telemetry/src/lib.rs:2953-2985`),
val0, A: admit p50 82 ms / p99 2.34 s; commit p50 1.12 s / p99 8.09 s; exec p50 2.62 s / p99 9.93 s;
durable p50 6.78 s / p99 19.5 s. B: commit p50 0.31 s / p99 4.91 s; exec p99 9.22 s; durable p99 18.3 s.
Actual ingest ~1,440 actions/s (~44 PlaceOrderBatch x 400 orders + ~27 CancelAll per block).

Run-to-run spread (9j2, n=4 per arm, 300 s std): CL21 matched/s 263.9k-266.7k (range 1.1%), blk/s
21.3-22.8; M15 matched/s 250.4k-255.9k, blk/s 14.6-18.7 (range 25%), commit p99 5.1-8.6 s. matched/s is
tight; blk/s and p99 are noisy and need n >= 4.

Microbench and build reference values (not comparable to cells in absolute terms):
- `ubench_econ` (UB_MARKS=1, UB_MARKETS=300, binary 14236fa, `~/bench-results-matched/ozarchy-14236fa-ubench/a-new-1.log`):
  engine_ms/1k_fills 10.05-10.58 (exec 7.0-7.3, margin 2.0, match 1.3, settle 2.3-2.4, liq tail 3.0-3.3),
  fills/blk ~10.2k, 31.7 s per invocation of 3 runs.
- Release build times (fresh target, `~/bench-results-matched/ozarchy-9k-stage/build/build.log`):
  torus-node 2m29s, then bench-throughput 1m54s, torus-tx-flood 14 s; 279 s total; target dir 2.8 GB.
  A RocksDB C++ sccache miss adds ~10 min (s65 explorer note, `docs/perf/s65-explorers/killed-runs-finished.json:255`).
  Incremental rebuild after a torus-bridge edit: est. 40-90 s (unmeasured).

## 4) Knobs and current defaults

Build:
- No `[profile.release]` anywhere (`Cargo.toml`; `.cargo/config.toml` only has `profile.dev`), so release =
  cargo defaults: opt-level 3, codegen-units 16, no LTO, no target-cpu. Linker clang + mold
  (`.cargo/config.toml` target section); `CC/CXX_librocksdb_sys = sccache cc/c++`; `~/.cargo/config.toml`
  `jobs = 16`, `profile.dev debug=false`. `alloy-primitives` `asm-keccak` on (workspace `Cargo.toml`).
- Node allocator: jemalloc, unprefixed (also serves RocksDB) via default feature
  (`crates/torus-node/Cargo.toml:61-69`, `crates/torus-node/src/allocator.rs`).
- Build `torus-node` and `bench-throughput` in SEPARATE cargo invocations (a combined build unifies features
  and changes the node; tools/matched-bench/README.md). torus-bridge dev-deps turn on
  `test-reference-paths` for torus-core/torus-state in test/bench builds (`crates/torus-bridge/Cargo.toml:41,54,57`).
- One `CARGO_TARGET_DIR` per worktree: two worktrees sharing a target dir can run each other's binaries
  (`tools/dev/torus-target-dir` header, memory bd0f09b4). Existing dirs: only `~/.cargo-target-9k` (2.8 GB).

run-cell.sh (positional MARKETS=10 DUR=120 RATE=76000 EXTRA_ENV=''; env defaults `:498-541`):
SENDERS 5000, CONC 256, BATCH 400, SUBMIT 1, BAND 5, CROSS_FRACTION 0.5, CANCEL_FRACTION 0.05,
BLOCK_CAP 400 (bundle: TOTAL_BLOCK_CAP 400, ORDERS_PER_BLOCK 200k, VERIFIED_SENDER_CACHE 64k, BLOCK_BYTES 12 MB),
BENCH_RPCS all, BULK_COUNT 100000, HEALTH_TIMEOUT 240, DRAIN_TIMEOUT 180+2*MARKETS, DIGEST_PAR 8,
RESULTS_ROOT `~/bench-results-matched`, DATA_ROOT `~/torus-wsl-devnet`, TOOLS_FROM_WORKTREE 1. Off by
default: MPS, OPEN_ORDER_BUDGET, RETRY_BUSY, MAX_IN_FLIGHT, IN_FLIGHT_TAIL, ORACLE_FEED, STATE_HASH_ATTEST,
PRELOAD, CRASH_KILL_AT_S, KILL_NO_RESTART, EVM_LOAD_RATE, HOTSTUFF_CPUS, ANTISPAM, CLOID_SHARE.
`RECORD_ENV` (`:613`): ROCKSDB_PIPELINED_WRITE=1, RESIDENT_BOOKS=1, NATIVE_ROOT_CACHE=1, PARALLEL_SETTLE=1,
PARALLEL_BUCKET_HASH=8, BUCKET_MEMBER_CACHE_MB=256, COMMIT_LAG_BACKOFF_CAP=8 (recent campaigns blank the
first six via EXTRA_ENV to use compiled defaults). `devnet/wsl/env.sh`: HASH_ONLY_PUSH_THRESHOLD 512 KB,
SHARD_CUSTODY 0, anti-spam off.

The "standard" shape used by the latest campaigns (9j2, `~/bench-results-matched/ozarchy-9j2-tools/ozarchy-9j2-std-campaign.sh:400-416`):
MARKETS 300, RATE 76000, DUR 120 or 300, run-cell env `BLOCK_CAP=400 RETRY_BUSY=1 MAX_IN_FLIGHT=4
OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 ORACLE_PRICE=30000 ORACLE_INTERVAL_MS=2000 ORACLE_WALK_BP=0
IN_FLIGHT_TAIL=commit CLOID_SHARE=0`, EXTRA_ENV `TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS=
TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB=
TORUS_ROCKSDB_STATS=2 TORUS_ROCKSDB_STATS_INTERVAL_SECS=1 TORUS_SHARD_CUSTODY=0 TORUS_CHECKPOINT_INTERVAL_MS=15000`.
Arm R (hash-off Classic) adds nothing; arm CL21 (closest to the owner-decided defaults) adds
`BOOK_WINDOW_COMMIT=1 STATE_HASH_ACTIVATION=1 STATE_HASH_ATTEST=1` (run-cell env) and
`TORUS_SETTLE_STREAMED=1` (EXTRA_ENV). Deep shape adds `PRELOAD=1000000 BULK_COUNT=1000000`.

bench-throughput (`tools/bench-throughput/src/main.rs`): subcommands `matching-engine` (`:64`; --orders
10000, --markets 1, --warmup 1000, --genesis devnet/genesis.json (relative: run from a repo root),
--overlay false, --defer-trades false, --senders 0 = unique address per order), `consensus` (`:89`, cell
load), `combined` (stub, `:4484`), `state-root` (`:4492`), `gen-accounts`, `preload`, `oracle-feed`.

Microbench env: ubench_econ `UB_SENDERS 600, UB_MARKETS 300, UB_ACTIONS 60, UB_WARM 40, UB_MEASURE 6,
UB_RUNS 3, UB_BATCH 400, UB_BUDGET 900, UB_MARKS 0, UB_MARK_WALK 0, UB_NO_R 0, UB_R_WORKER 0,
UB_REAL_MARKETS 1, UB_DEEP_LEVELS 0` (`crates/torus-bridge/tests/ubench_econ.rs:86-124`,
`tests/common/econ_load.rs:68,146`); book_encode `REPS 11, SHAPE plain|mixed|p1m|p0, SPREAD 0`;
load_books_ubench `TORUS_LOAD_BOOKS_WORKERS`; ubench_hasher `UB_HASH_REPS/UB_HASH_OPS`; ubench_adl
`UB_ADL_*`; ubench_users_rss `UB_USERS, UB_MM_USERS, UB_SHAPES`. Criterion: `CRITERION_HOME` sets the
baseline dir (criterion 0.5.1 `src/lib.rs:139`), so baselines can be shared across worktrees.

Inventory of T0/T1 (every `#[ignore]` ubench test needs `--release -- --ignored --nocapture`):
- criterion: torus-core `order_book` (place/match/sweep/cancel/mixed/deep-insert), `book_encode` (Classic
  whole-book encode, jemalloc); torus-bridge `exec_batch_margin` (400 resting orders, 1 market, 100
  senders), `exec_batch_trades` (200x200 fills, inline vs deferred trades), `exec_place_batch` (400 singles
  vs 1 batch, verify + exec); torus-state `erasure`.
- ubench tests: torus-bridge `ubench_econ` (econ cell shape, multi-block, pipelined overlay layering),
  `engine_parallel_bench` (cap-400, 10 markets, threads 0/2/4/8), `exec_hash_once_ubench` (verify +
  anti-MEV sort), `l3_savebooks_ubench` (needs `--features save-timings`), `load_books_ubench` (cold boot
  load), `ubench_epoch` (empty blocks at epoch boundaries), `ubench_adl`, `ubench_hasher` (SipHash vs
  foldhash), `ubench_read_precompile_gas`, `ubench_users_rss`; torus-core `ubench_position_cache`,
  `open_order_cost_probe`; torus-state `bg_writer_hol_probe`.
- Determinism guards for exec prototypes: `crates/torus-bridge/tests/perf_equivalence_golden.rs` (pinned
  per-block digests; perf changes must keep them), `tools/golden-repin-diff.py`, app-level differentials
  in `crates/torus-consensus/src/app.rs` (`assert_dumps_equal`). Full suite: `cargo nextest run
  --workspace` ~5-10 min (TESTING.md).

## 5) Known limits and failure modes

- Where writes land: run-cell.sh writes into the worktree it is given (`target/release/torus-node`,
  `devnet/wsl/genesis-3val.json`) and into its OWN repo root (`testnet/genesis-weighted-full.json` if
  missing). Never invoke it from the main repo or point it at the main repo; run each arm's own
  `<wt>/tools/matched-bench/run-cell.sh` on that worktree.
- Must run inside a `bench-*.service` unit (`campaign/detach.sh NAME LOG CMD...`). On 2026-10-06, cells
  started as descendants of an agent shell lost processes to an outside SIGKILL. Stop a cell with
  `systemctl --user stop bench-NAME.service`.
- Single host: 3 validators + load generator + oracle feed share 32 threads. Every node sizes its exec pool
  to 32 (`exec_pool.rs:20-35`), so the pools are 3x oversubscribed. Exec-thread runqueue wait is 3.4 ms per
  committed block (9% of its on-CPU, source A). A cell measures this contended regime, not one node per host.
- Disk: per cell 39 MB (std 120 s) to 221 MB (deep 120 s) kept in results, plus ~1.35 GB RocksDB per
  node during the cell (`data-du.txt`); `campaign/run_cell.py:157` wants max(14, 6+0.2*DUR) GiB free.
  `~/bench-results-matched` is already 65 GB. Node peak RSS (VmHWM) ~6.4 GB; campaign memguard stops a cell
  at MemAvailable < 6 GB.
- Stale binary trap: the md5 is logged and can be enforced (`EXPECT_NODE_MD5`); shared target dirs between
  worktrees are unsafe; a pre-r6 binary shows 0.0 in the engine sub-phase fields.
- Duration parity: a 300 s `avg` includes the late deep-book regime; compare across durations on
  `first120` only. A 120 s cell never reaches deep-book state unless `PRELOAD` is used (deep 120 s cell:
  6m11s-7m54s end to end, `ozarchy-9g-deep-w1`, `ozarchy-9ghn-deep-w1`).
- Closed-loop load: `--rate-total 76000` never binds (actual ~1,440 actions/s); `MAX_IN_FLIGHT=4` x 5000
  senders and `OPEN_ORDER_BUDGET=900` set the offered load, so latency percentiles are partly queueing
  created by the load generator.
- Order-age histograms are exponential, a factor of 2 per bucket from 5 ms (`lib.rs:2957`: edges ... 2.56,
  5.12, 10.24, 20.48 s). Exec p99 in all 20 9j2 cells sits at 8.9-10.2 s, just under the 10.24 s edge: p99
  is interpolated inside a 2x-wide bucket.
- `sched_by_node.*.threads.torus-flush-worker` is null in 280 of 280 recent cells while the flush worker is
  attached (node log "flush worker attached"). Likely cause: `find_tid` (`run-cell.sh:754-760`) compares the
  full name with `/proc/<pid>/task/<tid>/comm`, which Linux truncates to 15 chars
  (`torus-flush-worker` -> `torus-flush-wor`; same class as memory d8a1528f for `bench-throughput`).
  The same truncation hits `torus-end-resident`, `torus-exec-pool-*`, `torus-ingress-verify-*` and
  `torus-gossip-verify-*` in pidstat/perf output.
- Microbench fidelity gaps: (1) only `book_encode` and `ubench_users_rss` install jemalloc; every other
  criterion bench, ubench test and `bench-throughput matching-engine` runs glibc malloc, while the node
  (and its RocksDB) runs jemalloc. (2) `matching-engine` uses unseeded `rand::thread_rng()` (`main.rs:1734`),
  prices U[55k,65k] x qty U[1,100] (`:452-475`), placeholder `b"listed"` market rows, raw StateDb and
  unique senders by default, and its per-market breakdown always uses fresh random senders (`:1886-1897`);
  it times only `execute_batch` (no verify, save_books, end_resident or flush). (3) `exec_batch_margin`
  (1 market, 400 resting) and `engine_parallel_bench` (10 markets, 1200 resting) predate the 300-market,
  ~17.6k orders/blk cell shape. (4) Test/bench builds compile `test-reference-paths` into torus-core/state.
- Noise: campaign README quotes ~4% cell-to-cell (older); 9j2 shows a ~1% matched/s range for one arm but a
  25% blk/s range and a 1.7x commit-p99 range for another. The box must be quiet (`campaign/quiet-check.sh`:
  load1 < 1.0, no cargo/rustc, no process > 40% CPU).
- Owner-parked build levers (owner list): opt-level A/B, test-binary merge + hakari, sccache cap. LTO,
  PGO/BOLT and target-cpu were already proposed in `docs/perf/s65-explorers/*.json`; memory 38e871cb lists
  "release profile LTO/codegen-units never tried". Not novel.

## 6) Corners that look unexamined (observations only)

1. CPU attribution gap: whole-process user CPU is 252-344 ms per native block and the two snapshotted
   threads account for ~43-56 ms. About 80-85% of node CPU sits in pools, RPC, libp2p, RocksDB and
   workers, and `summary.json` has no per-thread-group split (the campaign `hostsampler.sh` pidstat has
   one; plain run-cell does not).
2. Exec block cost is heavy-tailed: chain_ms p50 3.7 ms vs mean 40.9 ms and p95 240 ms (source A). The
   summary has no per-block breakdown of which blocks are heavy (action mix, cancel-all count, checkpoint
   or not).
3. Phase 2 margin is serial by default and is the largest engine leg (10-13 ms/blk, ~30% of engine) at the
   current shape. `TORUS_PARALLEL_ENGINE` was called "not the lever" in `docs/re-proof5-context.md:57`
   under an older shape; memory 38e871cb lists it as "revisit (baseline changed)".
4. Serial tail of settle: cache_flush 5.3-6.8 ms plus pass B 3.8-5.0 ms per block, together ~27% of engine.
5. No calibration between microbench and cell: the last recorded ubench_econ reads ~10.4 ms/1k fills (with
   marks and the liquidation tail, older binary) vs 2.6-3.0 ms/1k fills in cells. No microbench is pinned
   to the current cell shape or allocator.
6. Cancel-all heavy shape: 24.6-30.6 CancelAll per native block, each visiting ~197 of 300 books and hitting
   ~96 (`phase_by_node.*.cancel_all`), mostly driven by the harness's OPEN_ORDER_BUDGET=900 policy.
   Measured cost so far is small (phase1_actions 2.3-3.0 ms).
7. Pool sizing ignores co-location: each node's rayon and verify pools assume the whole host. Memory
   0baab823: exec-pool phases are start-skewed (static 32-chunk LPT; first-sample p50 3.4-4.0 ms after
   phase start), MATCH/SETTLE worker caps never A/B'd on ozarchy, verify-pool renice never tried. CPU
   pinning was tried (-5.7%, memory 38e871cb).
8. `torus_rocksdb_block_cache_bytes` reads 87 during load (`mem-samples.txt`, source A), memtable ~213 MB
   mean, `db_write_ms_avg` 6.9. This map does not explain the block cache's role in this regime.
9. Commit cadence is bursty (commit interval p50 10 ms / p95 367 ms; idle probe 26.2 blk/s) while the exec
   queue rides near its cap (peak depth 66): blk/s under load is set by exec back-pressure, not by views.
10. No open-loop latency curve: every standard cell runs saturated, and no cell shape measures order age at
    a fixed sub-saturation offered load. This is relevant to p99, though T5.3 p99 attribution itself is on
    the owner's exclusion list.

## Recipe: A/B a prototype branch locally

Conventions: worktrees under `/home/oz/projects/wt/novel-s80/proto/`, one target dir per arm, results under
`/home/oz/projects/wt/novel-s80/proto/results` (keeps `~/bench-results-matched` untouched), nothing
pushed or merged.

```bash
# 0. worktrees (the only write to the main repo is git's worktree metadata)
git -C /home/oz/projects/torus-economy/Torus-hyperBFT worktree add --detach /home/oz/projects/wt/novel-s80/proto/base eb993e3f
git -C /home/oz/projects/torus-economy/Torus-hyperBFT worktree add -b s80/<idea> /home/oz/projects/wt/novel-s80/proto/cand eb993e3f
# 1. build each arm alone, under the lock, own target dir (~4.7 min fresh, est. 1-2 min incremental)
for a in base cand; do cd /home/oz/projects/wt/novel-s80/proto/$a
  CARGO_TARGET_DIR=$HOME/.cargo-target-s80-$a flock -w 60 -o "$HOME/projects/wt/.cargo-heavy.lock" cargo build --release --locked -p torus-node
  CARGO_TARGET_DIR=$HOME/.cargo-target-s80-$a flock -w 60 -o "$HOME/projects/wt/.cargo-heavy.lock" cargo build --release --locked -p bench-throughput
done   # give both arms the SAME bench-throughput unless the load generator is the change
```

(a) Microbench A/B (minutes):

```bash
# T1 engine proxy: build the test binaries, then run them alternately (ABAB) under the lock so no build skews them
for a in base cand; do cd /home/oz/projects/wt/novel-s80/proto/$a
  CARGO_TARGET_DIR=$HOME/.cargo-target-s80-$a flock -w 60 -o "$HOME/projects/wt/.cargo-heavy.lock" \
    cargo test -p torus-bridge --release --test ubench_econ --no-run   # prints "Executable ... (.../deps/ubench_econ-<hash>)"
done
# BIN_base / BIN_cand = those executables; 3 alternating rounds, each prints a "MEDIAN engine_ms/1k_fills" line
for i in 1 2 3; do for a in base cand; do eval B=\$BIN_$a
  flock -w 600 -o "$HOME/projects/wt/.cargo-heavy.lock" env UB_MARKETS=300 UB_RUNS=3 "$B" --ignored --nocapture \
    > /home/oz/projects/wt/novel-s80/proto/results/ub-$a-$i.log 2>&1; done; done
grep -h "MEDIAN" /home/oz/projects/wt/novel-s80/proto/results/ub-*.log
# T0 criterion with a baseline shared across worktrees
export CRITERION_HOME=/home/oz/projects/wt/novel-s80/proto/criterion
(cd /home/oz/projects/wt/novel-s80/proto/base && CARGO_TARGET_DIR=$HOME/.cargo-target-s80-base flock -w 60 -o "$HOME/projects/wt/.cargo-heavy.lock" cargo bench -p torus-bridge --bench exec_batch_trades -- --save-baseline base)
(cd /home/oz/projects/wt/novel-s80/proto/cand && CARGO_TARGET_DIR=$HOME/.cargo-target-s80-cand flock -w 60 -o "$HOME/projects/wt/.cargo-heavy.lock" cargo bench -p torus-bridge --bench exec_batch_trades -- --baseline base)
# bench-throughput engine-only (unseeded: >= 5 alternating runs, compare medians); run from the worktree root
cd /home/oz/projects/wt/novel-s80/proto/cand && $HOME/.cargo-target-s80-cand/release/bench-throughput matching-engine \
  --orders 20000 --markets 300 --warmup 2000 --overlay --defer-trades --senders 5000
```

Keep the determinism guard green on the candidate: `cargo nextest run -p torus-bridge -E 'test(perf_equivalence_golden)'`
(under the lock), plus the crate rule in TESTING.md.

(b) Short local cell A/B (each 120 s, 300-market cell takes ~3.2 min end to end; 2 per arm ABBA ~13 min):

```bash
P=/home/oz/projects/wt/novel-s80/proto; mkdir -p $P/results $P/genesis-cache
XE='TORUS_ROCKSDB_PIPELINED_WRITE= TORUS_RESIDENT_BOOKS= TORUS_NATIVE_ROOT_CACHE= TORUS_PARALLEL_SETTLE= TORUS_PARALLEL_BUCKET_HASH= TORUS_BUCKET_MEMBER_CACHE_MB= TORUS_ROCKSDB_STATS=2 TORUS_ROCKSDB_STATS_INTERVAL_SECS=1 TORUS_SHARD_CUSTODY=0 TORUS_CHECKPOINT_INTERVAL_MS=15000'
cell() { # $1=arm $2=round ; starts one detached cell
  local a=$1 r=$2 W=$P/$1 L=s80-$1-$2
  cd "$W" && env TARGET_DIR=$HOME/.cargo-target-s80-$a RESULTS_ROOT=$P/results GENESIS_CACHE_DIR=$P/genesis-cache \
    BLOCK_CAP=400 RETRY_BUSY=1 MAX_IN_FLIGHT=4 OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 ORACLE_PRICE=30000 \
    ORACLE_INTERVAL_MS=2000 IN_FLIGHT_TAIL=commit CLOID_SHARE=0 \
    "$W/tools/matched-bench/campaign/detach.sh" "$L" "$P/results/$L.log" \
    "$W/tools/matched-bench/run-cell.sh" "$W" "$L" 300 120 76000 "$XE"
}
# run ONE at a time: start it, wait until $P/results/<label>/summary.json exists (or the unit is gone), then the next
#   order: base r1, cand r1, cand r2, base r2
# decided-defaults variant (CL21-like): add BOOK_WINDOW_COMMIT=1 STATE_HASH_ACTIVATION=1 STATE_HASH_ATTEST=1
#   to the env and TORUS_SETTLE_STREAMED=1 to XE (both arms)
jq -r '[.label,.validity.verdict,.headline.agreement_verdict,.headline.matched_s_avg,.headline.blk_s_avg,
  .headline.chain_ms,.headline.engine_ms_per_1k_fills,.phase_by_node.val0.order_age_ms.commit.p99,
  .phase_by_node.val0.order_age_ms.exec.p99,.proc_cpu_by_node.val0.load.user_ms_per_1k_fills]|@tsv' \
  $P/results/s80-*/summary.json
```

Notes: the first cell of a fresh worktree builds `testnet/genesis-weighted-full.json` (100k accounts, est.
< 1 min) inside that worktree. A shared `GENESIS_CACHE_DIR` gives both arms the same genesis when their
tools are identical. A cell started with detach.sh holds the build lock for its whole run, so do not start
a build in between. matched/s deltas of ~1-2% need n >= 2 per arm (n=1 is not a result); blk/s and p99 need
n >= 4. Any DISAGREE rejects the candidate.
