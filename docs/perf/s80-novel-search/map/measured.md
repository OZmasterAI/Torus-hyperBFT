# novel-s80 system map: MEASURED cost picture (prep only, nothing run)

Built 2026-10-22 from existing results only. Repo main `eb993e3f` (read-only). Abbreviations: RD = results doc
`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` (sN = section N); BR = `~/bench-results-matched`.
Owner exclusions: `/home/oz/projects/wt/novel-s80/ledger/owner-list.md` (TC, dead-leader, batch 10, 9k-9p, T5.3 p99
attribution, A1/Q6 slicing, PE, Block-STM, T5.1/T5.2, co-location, ingress dispersal, Phase 5, mode 3 etc.).
Prior-art filter (read before proposing anything): `docs/perf/8k-explore-survey.md` (all ideas up to s96, with
verdicts), `docs/perf/8k-explore-ideas.md` (L1.1-L6.3), RD s97-s124.

**Binary caveat.** The newest numbers (s124, campaign 9j2, 2026-10-21/22) come from `origin/perf/h2h-combined`
`29bfc705`, which is NOT an ancestor of main. L2.1 (`TORUS_SETTLE_STREAMED`) and L6.1 (`book_window_commit`) exist
only there and on `origin/merge/s136-batch10` (`native_executor.rs:970,1106`; `app.rs:121-150` on that branch).
Code refs below are main `eb993e3f` unless marked "batch10".

## 0. Headline numbers per shape

Load (all 300-market cells): rate 76,000, 5,000 senders, PlaceOrderBatch of 400 orders, MAX_IN_FLIGHT 4 per sender,
released on commit ("commit tail"), cloid share 0, oracle feed 2 s, 3 validators + bench on one 32-thread host
(archy). Econ mix 61.5% place / 38.5% cancel-all actions (BR `ozarchy-9j2-std-r2-CL21/summary.json` `.ingest`).

| shape (source) | arm | matched/s | native blk/s | actions/blk | fills/blk | block p50 / p99 ms | CPU-s/1M fills | cores/node | exec-thread on-CPU ms/1k fills | timeouts /1k blk | RSS mean / max MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 300m standard (100k accts), s124 | CL21 (n=4) | 265,183 | 21.96 | 63.4 | 11,544 | 11.6 / 475 | 23.15 | 6.14 | 2.633 | 0.04 | 3,214 / - |
| 300m standard, s124 | R hash-off (n=2) | 265,536 | 22.22 | 63.1 | 11,437 | 11.3 / 472 | 23.42 | 6.22 | 2.541 | 0.22 | 3,137 / - |
| deep (8i P1M: 1M accts + 1M preloaded resting), s124 | CL21 (n=4) | 257,395 | 21.49 | 62.7 | 11,377 | 11.8 / 478 | 24.34 | 6.27 | 2.780 | 0.46 | 4,367 / 5,678 |
| deep, s124 | R hash-off (n=2) | 263,022 | 15.95 | 82.0 | 15,320 | 14.0 / 544 | 23.81 | 6.26 | 2.546 | 1.45 | 4,656 / 5,677 |
| deep, s124 (rejected) | M15 mode 3 (n=4) | 247,938 | 15.36 | 81.8 | 15,146 | 14.3 / 632 | 28.20 | 6.99 | 2.552 | 1.18 | 4,911 / 6,092 |
| 10 markets, s67 A (main `21d238e0`, 2026-10-14, **stale**) | main (n=4) | 299,930 | 11.11 | 118.6 | 23,388 | 13.4 (ci p50) / 1,322 | 17.18 | ~3.3 | engine 1.967 ms/1k | - | 2,750 / 3,330 |

Sources: RD s124.3-124.4; BR `ozarchy-9j2-tools/analyst/9j2-{deep,std}.txt`, `9j2-steps.txt`; actions/blk and
per-node rows aggregated from each cell's `summary.json` (`phase_by_node.valN.actions_per_exec_block`); 10m from RD
s67.3 and BR `ozarchy-p4gate-10m-m{1..4}-a/summary.json`. Bench process: 1.33-1.41 cores; host load1 avg 35-45
on 32 threads (`summary.json .cpu`). 10m rows predate the s58 bundle, 8g, L2.2, L3.1/L3.3, T5.3, L2.1, L6.1: treat
as a different system (300m at that date: 218.8k matched/s, 4.04 blk/s, p99 1,224 ms, RD s67.3).

## 1. Architecture and data flow (one native action, ~10.8 KB canonical bytes, 400 orders)

1. Bench -> JSON-RPC batch submit (hex payloads) -> `torus-ingress-verify-*` rayon pool, size cores/2
   (`crates/torus-rpc/src/torus.rs:668-680`, `verify_one_action_with` :599): EIP-712 Keccak over every order item
   + k256 ecrecover, trust-cache insert -> mempool (`crates/torus-mempool`, pool cap 65,536 `rate_limit.rs:115`).
2. Gossip pre-spread over gossipsub (`MessageAuthenticity::Signed`, `ValidationMode::Strict`,
   `crates/torus-network/src/behaviour.rs:94,135`) -> peers' `torus-gossip-verify-*` pool, cores/2, inflight cap 1,024
   (`crates/torus-node/src/main.rs:996-1007`) -> verify again (hash dedup since s77) -> peer mempool.
3. Leader (hotstuff-algo, `crates/hotstuff_rs/src/algorithm.rs:153`): `produce_block` (`app.rs:10433`): select
   (`app.rs:10289`) + mirror (`:10311`) -> hash-only CompactBlock header; bodies via DA push / 512 KB manifest +
   by-hash pull; header re-send 250 ms (`hotstuff/implementation.rs:121,331`).
4. Followers: `validate_block` (`app.rs:10483`), DA reconstruct (`:9865`), insert+persist, vote; QC; 2-chain commit.
5. Commit hook on hotstuff-algo: `on_committed_block` (`app.rs:10601`) -> `enqueue_for_execution` (`:11211`) ->
   `dispatch_to_exec` (`:11421`): submit header/body to `torus-commit-persist` writer (T5.3, `:724`; manifest prune
   thread `:2439`), Keccak `compute_action_hash` of every action for mempool removal (`:11505-11516`), DA-GC hand-off,
   then `send` on the exec channel, `sync_channel(EXEC_QUEUE_DEPTH=64)` (`app.rs:8333`, `rate_limit.rs:355`); blocking
   by default (`TORUS_EXEC_NONBLOCKING_DISPATCH`, `app.rs:1199`), so a full channel stalls consensus.
6. `torus-execution` thread (`app.rs:8335`, `execution_loop` `:6814`, `execute_committed_block_with` `:4506`): wait
   persist ticket -> verify (trust-cache key Keccak) -> replay guard -> load books -> engine
   `execute_batch_phases` (`crates/torus-bridge/src/native_executor.rs:8281`): phase 1 actions (`:8375`), phase 2
   margin pre-reserve / prepare, serial by default (`:8555`; parallel only with `TORUS_PARALLEL_ENGINE>=2`,
   `:1105-1119`), match on `torus-exec-pool` (`:8820`; `market_workers.rs:429`), settle (`:8984`; pass A `:9879`,
   pass B `:9574/:10028`, plan `compute_market_settle_plan` `:10237`), cache flush (`:9047-9097`), post-engine tail
   (liquidation `liquidation_step.rs:153-158`, oracle, fees, epoch) -> action status -> save_books (L3.1 coalesced,
   `exec_pipeline.rs:259`) -> end_resident hand-off (`torus-end-resident`, `native_executor.rs:3963`).
7. Checkpoint close (`app.rs:4083-4098`): first of `height % 100 == 0` (`STATE_HASH_CHECKPOINT_INTERVAL=100`,
   `crates/torus-state/src/running_hash.rs:192`, unconditional, also with the hash off), epoch boundary, every-N,
   15 s interval (`exec_pipeline.rs:150-178`), byte cap -> layer to `torus-ckpt-merge` (`app.rs:437`) ->
   `torus-flush-worker` (`exec_pipeline.rs:830`): state write batch + running hash -> RocksDB (state DB; HotStuff and
   DA stores in their own DBs since s74). `torus-ckpt-drop` (`app.rs:493`) frees layers.

## 2. Threads, pipeline and what is serial

| thread (spawn site) | count / node | measured load (std CL21 unless noted) | serial? |
|---|---|---|---|
| `torus-execution` (`app.rs:8335`) | 1 | on-CPU 31.8 ms per committed blk (0.70 core), runqueue wait 3.0 ms/blk, busy fraction 0.88 (deep 0.91, R 0.93-0.95) | **yes: the ceiling** |
| `torus-exec-pool-*` (`exec_pool.rs:20-37`) | host parallelism (32) or `TORUS_MATCH/SETTLE_WORKERS` | 23.3% of node user cycles (q8re); exec thread waits on it 16.9-18.9 ms/blk (s92) | fork-join under exec |
| `hotstuff-algo` (`algorithm.rs:153`) | 1 | on-CPU 11.7 ms/blk (0.26 core), rq 1.8 ms/blk; 3.1% of user cycles, 42.6% of it Keccak (q8re) | yes, blocked by full exec channel |
| `torus-gossip-verify-*` (`main.rs:1004`) | cores/2 = 16 | 13.6% of user cycles; 45.5% Keccak, ~24% k256 | parallel |
| `torus-ingress-verify-*` (`torus.rs:678`) | cores/2 = 16 | 13.2%; 29% Keccak, ~13% k256, 6% hex decode | parallel |
| `rpc-worker` (`main.rs:1479`) | tokio blocking pool | 13.8%; Mutex `lock_contended` 8.7%, ReadView drop 4.7% | parallel |
| `tokio-rt-worker` | runtime default | 8.3%; sha512 23.7% (ed25519 gossipsub sign/verify), chacha20poly1305 8% (QUIC) | parallel |
| rayon global pool ("exec-workers" class) | 32 | 5.2%; 47% unresolved + crossbeam epoch/steal 35% (idle spin), `par_recover` frames | parallel |
| `torus-flush-worker` (`exec_pipeline.rs:830`) | 1 | busy share 0.133 std / 0.205 deep (wall, incl. waits); state write 4.2 / 7.3 ms per blk | serial, off exec |
| `torus-end-resident` (`native_executor.rs:3963`) | 1 | 4.07 ms/blk; exec waits 1.53 ms/blk on it | serial, off exec |
| `torus-commit-persist` (`app.rs:724`) | 1 | commit persist 4.6 std / 6.2 deep ms per call; exec waits ~0.02 ms (s115) | serial, off consensus |
| `torus-ckpt-merge` / `-drop`, rocksdb low/high, `torus-da-gc` | 1 each + RocksDB | 1.8% / 0.5% / 2.4% / 1.3% of user cycles (q8re) | background |

Thread shares: BR `ozarchy-q8re-run/analysis/ozarchy-q8re-300m-r{1,2,3}-ONP-w1.json` (`samples`, cycles:u, val1,
main `fbe36391` 2026-10-18, 512,892 samples / 135 s), aggregated here. Ingest side (rpc + gossip-verify +
ingress-verify + tokio) = 48.9% of user cycles; exec side (exec + exec-pool + global rayon) = 37.8%.
Serial chain at saturation: one exec thread per node; consensus commit is paced by it through the 64-deep channel.
Exec queue depth (val0 1 Hz `sampler.csv`, load window minus 30 s): std CL21 p50 9 / p90 42 / max 66, full (>= 60)
in 4% of seconds; deep CL21 p50 17 / p90 41; std R p50 18 / p90 47. Mempool p50 ~275 actions, p90 1.2-2.1k.
`peak_exec_queue_depth` = 66 in every 9j2 cell and every 10m p4gate cell.

## 3. Hot paths with measured cost

### 3.1 Exec thread, ms per native block (3 nodes x n cells, `summary.json .phase_by_node.valN.phases`)

| phase | std CL21 | deep CL21 | std R | deep R | note |
|---|---|---|---|---|---|
| exec block ms (wall) | 38.35 | 40.35 | 40.95 | 54.58 | s124.4 |
| verify (trust-cache key Keccak, `par_recover` skip) | 2.62 | 2.60 | 2.58 | 3.45 | 8g cut 5.95 -> 3.68 at 16.8k fills (s94) |
| replay guard | 0.55 | 0.55 | 0.54 | 0.76 | |
| engine total | 31.68 | 32.49 | 34.35 | 45.82 | 80-83% of block |
| . phase 1 actions | 2.28 | 2.55 | 2.22 | 3.06 | |
| . margin pre-reserve / prepare (serial) | **10.29** | **10.45** | 9.90 | 13.52 | largest engine phase, 0.89 ms/1k fills |
| . match (exec waits on pool) | 2.68 | 2.80 | 4.34 | 5.81 | L2.1 fuses match + plan |
| . settle total | **11.25** | **11.35** | 11.63 | 15.63 | pass A 0.93 / pass B 3.71 / cache flush **5.35** (std CL21) |
| . post-engine tail (liquidation scan 2,048 accts/blk, oracle, fees) | 3.65 | 3.77 | 3.59 | 4.44 | `LIQ_SCAN_PER_BLOCK` `torus-core/src/liquidation.rs:58` |
| . engine untimed | 1.54 | 1.56 | 2.66 | 3.36 | |
| save_books (L3.1 coalesced) | 0.57 | 1.22 | 0.52 | 0.72 | was 56.4 ms/blk deep before L3.1 (s117) |
| end_resident_wait | 1.53 | 1.51 | 1.42 | 1.59 | |
| checkpoint settle wait / upkeep | 0.18 / 0.72 | 0.20 / 1.24 | 0.20 / 0.83 | 0.24 / 1.43 | upkeep p99 16.7 std / 30.0 deep (val0 r2) |
| exec on-CPU (schedstat) / runqueue | 31.8 / 3.0 | 33.3 / 3.4 | 30.4 / 1.6 | 42.0 / 2.3 | |
| off-thread: flush state write (MB per checkpoint) | 4.21 (86.8) | 7.26 (174.5) | 4.14 (86.2) | 7.95 (195.5) | DB 276-372 MB/s |
| off-thread: end_resident; hash per native blk | 4.07; 1.48 | 4.06; 2.15 | 3.82; 0 | 5.31; 0 | |

Early vs late (std r2 CL21 val0, `early_60s`/`late_60s`): block 53.6 vs 35.9 ms, placed per blk 23.3k vs 14.0k,
resting 398k vs 104k, margin 15.2 vs 9.3, settle 16.4 vs 10.1, cache flush 8.0 vs 4.7. Deep: 59.4 vs 37.4 ms, resting
1.30M vs 1.11M. Cancel-all: 24 per native block, 197 books visited / 97 hit per cancel-all (`.cancel_all`).

Exec on-CPU decomposition by profile (most recent exec-thread profile, BR `ozarchy-8u-run/analyst/8u-w2report.txt`,
deep 1M, main `0e514a5c` 2026-10-20, pre-L3.1/L6.1/L2.1; ms per 1k fills): on-CPU 3.536 = engine 2.151 (margin/prepare
0.669, settle 0.575, cache flush 0.372, other engine 0.317, liquidation 0.166, cancel-all 0.033, match 0.004) +
save_books 1.058 + other 0.328. Leaves: libc memmove/memcpy 0.393, page faults 0.153 (60 faults per 1k fills),
kernel other 0.108, jemalloc 0.028. Off-CPU 1.078: pool waits match 0.278 / settle 0.151 / cancel-all 0.145,
preempted 0.14. IPC 1.18-1.23 (perf stat). Engine fit (deep, A arm): ~16 ms fixed per block + 2.47 ms per 1k fills.
Exec inclusive patterns, std shape (q8re W1 EXEC, `...-r1-ONP-w1.txt:67-86`, pre-8g/L2.2/L3.1): cache `flush_all`
13.9% (10.4 ms/blk), hash 11.7% (8.8), liquidation 7.8% (5.9), save_books 7.5% (5.6), overlay `put_cf_raw*` 6.7%
(5.0), borsh 6.3%, malloc/free 6.3%, `merge_disjoint` 4.6% (gone with L2.2), BTreeMap insert 4.1%, prepare_one 16.6%.
Pool waits (s92): match setup 7.3-8.1, settle plan 4.1-4.6, cancel-all 3.6-4.4 (~5 jobs, 3.2-3.6 workers busy),
`par_recover` 1.9-2.2 ms/blk (skipped since 8g); last worker preempted 15-38% of the wait.

### 3.2 Consensus thread, ms per call (load window, 3-node mean, std CL21; deep within 10%)

view 45.6 | propose build 10.4 (select 5.15, mirror 4.0, attest 0.87, encode 0.09; leader only) | qc collect 33.7 |
vote gather 22.4 | insert+persist 9.05 | vote delay 8.71 | validate 2.26 | DA reconstruct 4.81 | on_committed_block 5.89
| mempool remove committed 1.62 | qc_to_advance 8.23 | commit persist 4.83 (writer thread). Source:
`summary.json .consensus_by_node.valN.load`. In s118, `commit_feed_app` (decode, manifest, enqueue incl. the blocking
exec send) held 94-96% of `qc_to_advance`, scaling 1.36-1.37 ms per 1k fills. Survey s0: hotstuff Keccak at dispatch
11.2-12.5 ms per native block at older (bigger) blocks; commit-time body record ~12-15 ms per full block.
M15 for contrast: view 61.0, on_committed_block 12.0, qc_to_advance 15.6.

### 3.3 Whole-process top-30 self (user cycles, val1; q8re r1-r3 W1 aggregated, main `fbe36391`, 2026-10-18)

| % | function [main threads] |
|---|---|
| 13.65 | `__KeccakF1600` [gossip-verify 45%, ingress-verify 29%, hotstuff 10%] |
| 11.55 | `[unknown]` unresolved [rpc-worker 35%, rayon global 21%, exec-pool 14%] |
| 7.70 | `[unknown]` in libc (memmove/memcpy class) [exec-pool 20%, exec 16%, tokio 14%] |
| 2.96 / 1.13 / 0.76 | k256 `ProjectivePoint::add` / `double` / `FieldElement5x52::square` [gossip 64-66%, ingress 34-35%] |
| 2.01 | `sha512_compress_x86_64_avx2` [tokio 100%: ed25519 gossipsub] |
| 1.83 | `futex::Mutex::lock_contended` [rpc-worker 64%] |
| 1.77 / 0.98 / 0.54 | crossbeam `Global::try_advance` / `Stealer::steal` / `with_handle` (rayon spin) |
| 1.43 | `AccountReader::get_position` [exec-pool 74%, exec 26%] |
| 1.28 | `Users::position` (TypedRows) [exec-pool] |
| 1.03 | `compute_market_settle_plan` [exec-pool] |
| 0.99 | `execute_batch_phases` self [exec] |
| 0.98 / 0.87 / 0.51 | foldhash `hash_bytes_long` / `[u8;20]::hash` / `hash_one<&Address>` [exec-pool] |
| 0.88 | `NativeAction::encode_place_order_params` [gossip 43%, hotstuff 21%, rpc 17%] |
| 0.88 / 0.84 / 0.83 / 0.77 / 0.49 | `place_order_checked` / `match_market` / `cancel_all_many` / `match_at_level` / `account_fill` [exec-pool] |
| 0.82 | `hex::FromHex` [ingress-verify 99%] |
| 0.76 | `taker_margin_release_cfg` [exec-pool] |
| 0.70 | rocksdb `crc32c::ExtendImpl<DefaultCRC32>` [hotstuff 36%, flush 34%, gossip 17%] |
| 0.67 / 0.52 | `ZSTD_compressBlock_doubleFast` [rocksdb:low] / `LZ4_compress_fast_continue` [rocksdb:high] |
| 0.64 | `drop_glue<ReadView>` [rpc-worker] |
| 0.53 / 0.52 | `malloc` / `[vdso]` |

Cross-check, older (BR `ozarchy-p3cloidprof-run/analyst/frames-self.txt`, `ef812b02` glibc, ms CPU per 1k fills, all
threads, c0): total 24.51; Keccak 4.14, k256 `mul_inner` 1.24, CRC32C 0.54, alloc 0.54, `get_position` 0.44,
`select_unpredictable` 0.44, spec_eq 0.48, foldhash 0.33. No whole-process profile exists after 2026-10-18; 9m
(the next exec profile) is HOLD and owner-excluded.

### 3.4 Latency (order age = node clock minus the action's ms nonce; `phase_by_node.valN.order_age_ms`)

| ms | admit p50 / p99 | commit p50 / p99 | exec p50 / p99 | durable p50 / p99 |
|---|---|---|---|---|
| std CL21 | 40 / 2,066 | 344 / 5,022 | 1,152 / 9,255 | 4,064 / 16,459 |
| deep CL21 | 44 / 2,102 | 384 / 6,157 | 1,204 / 9,525 | 4,509 / 19,078 |
| deep M15 | 91 / 2,278 | 803 / 7,944 | 3,657 / 10,027 | 8,305 / 20,108 |
| 10m (s67 A, stale) | - | 2,315 / 9,687 | 3,395 / 10,072 | - |

Commit interval p50 11.6 / mean 45.2 / p95 270 / p99 475 ms; exec `chain_ms` p50 6.0 / p95 210-218 ms (std / deep
CL21). Tail history: in-window block p99 1,071 vs 542 ms out of window (s106); T5.3 cut G 452 -> 100 ms (s115).

### 3.5 Storage and network rates (std / deep CL21, per node)

WAL 37.3 / 55.1 MB/s, flush-write 7.4 / 10.0 MB/s, compaction 0.08-0.09 cores, 0 write stalls, memtable mean 214-223
MB (`.rocksdb`). Checkpoints 66.2 / 65.2 per 300 s = one per 100 blocks (~4.5 s). Body push max 520 KB, ~600
manifests and ~500 body pushes per node per cell, 0 dissemination failures (`.dissemination`). Survey s0: tokio
0.34-0.44 cores/node (QUIC 19.5%, ed25519 verify 19%, AEAD 11%, sign 11%, gossipsub 12%, bincode 7.6%, zstd 6.6%).

## 4. Knobs and current defaults (main unless noted; bench env from 9j2 `summary.json .cell.node_env`)

| knob | default | where | status / evidence |
|---|---|---|---|
| `EXEC_QUEUE_DEPTH` | 64 (compiled) | `rate_limit.rs:355` | propose-defer / exec watermarks: loss or parked (survey s5) |
| `TORUS_EXEC_NONBLOCKING_DISPATCH` | off (blocking send) | `app.rs:1199` | |
| `TORUS_NATIVE_TOTAL_BLOCK_CAP` | 400 actions | `rate_limit.rs:97` | blocks carry ~63; not binding |
| `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP` / `_BLOCK_BYTES_CAP` | 200,000 / 12 MB | `rate_limit.rs:250,309` | not binding (15.5k placed/blk) |
| `TORUS_CHECKPOINT_INTERVAL_MS` | 15 s validators, 0 rpc-only | `exec_pipeline.rs:150-178` | the %100 term fires first at >= 6.7 blk/s |
| `STATE_HASH_CHECKPOINT_INTERVAL` | 100 blocks | `running_hash.rs:192`, `app.rs:4083` | unconditional checkpoint trigger |
| `TORUS_CHECKPOINT_MAX_BYTES` | unset | `exec_pipeline.rs:264` | 50 MB cap was a loss (s89) |
| `TORUS_BOOK_COALESCE` (L3.1) | on | `exec_pipeline.rs:259` | needs hash off unless L6.1 |
| `book_window_commit` (L6.1), `TORUS_SETTLE_STREAMED` (L2.1), running hash, fail-stop | on in batch10 only | batch10 `app.rs:138`, `native_executor.rs:970` | owner-decided (batch 10) |
| `TORUS_SETTLE_NO_MERGE` (L2.2) | on | `native_executor.rs:804-824` | shipped s108 |
| `TORUS_BOOK_FAST_ENCODE` (L3.3) | on | `native_executor.rs:646` | |
| `TORUS_PARALLEL_ENGINE` (PE) | off (0) | `native_executor.rs:1105-1119` | owner list: excluded |
| `TORUS_MATCH_WORKERS` / `TORUS_SETTLE_WORKERS` | host parallelism | `market_workers.rs:298-313`, `exec_pool.rs:20-30` | never A/B'd on archy (survey s4) |
| cancel-all pass workers (PC) | 8 | survey s4 | shipped +4.5% |
| gossip / ingress verify pools | cores/2 each, min 2 | `main.rs:996-1005`, `torus.rs:668-680` | |
| `LIQ_SCAN_PER_BLOCK` / `LIQ_ACT_PER_BLOCK` / `ADL_WORK_PER_BLOCK` | 2,048 / 64 / 100,000 | `torus-core/src/liquidation.rs:58-72` | per block, not per second |
| `TORUS_HEADER_RESEND_MS` | 250 | `hotstuff/implementation.rs:121` | fixed s77 lost headers |
| push-manifest threshold | 512 KB (env.sh); code const 8 MB | `torus-network/src/caps.rs:76` | owner (WAN) |
| `TORUS_VERIFIED_SENDER_CACHE_CAP` | bench 64,000 | `rate_limit.rs:402` | |
| `TORUS_COMMIT_LAG_BACKOFF_CAP` | bench 8; genesis default 0 | `torus-genesis/src/lib.rs:124` | |
| `TORUS_RETENTION_BLOCKS` | 100,000 (prune by default) | RH4, s121 | |
| allocator | jemalloc compiled in | s56/s61 | decay tuning: loss (s62, s73) |
| bench: `MAX_IN_FLIGHT`, senders, batch, rate | 4, 5,000, 400, 76,000 | `.cell` | closed loop on commit |

## 5. Known limits and failure modes

1. **Exec thread is the ceiling on one host.** Busy 0.88-0.97; ~5.4 of 6.1 node cores are elsewhere. A few % more
   exec work per fill halves the block rate (s91: Phase 5 step 2 +5% exec CPU -> 10.8 -> 5.2 blk/s).
2. **Cadence equilibrium, not a fixed rate.** Same env, other binary: O 14.4 -> 8.0 blk/s (s118); deep R 15.95 vs CL21
   21.5 at equal matched/s, unexplained (s124.5); bistable 11-16 blk/s (survey s0). blk/s CIs are wide (M15 14.2-18.7).
3. **Tail.** Block p99 ~475 ms = 40x p50; exec block p95 ~210 ms. Attribution of the rest (T5.3 p99) is owner-excluded.
4. **Order latency is queueing.** Exec age p50 1.15 s (0.8 s more than commit age), p99 9.3 s; durable p99 16-19 s.
   The bench releases in-flight at commit, so offered load overshoots exec in waves (10m: 13-14 s waves, mempool
   <= 1,000 in 32-36% of seconds, s69-s70).
5. **Deep book.** Pre-L3.1: 3.5 blk/s, p50 191 / p99 1,344-1,456 ms, save_books 56 ms/blk, timeouts 3.3-3.8/1k, RSS
   6.5 GB (s116, s117). CL21 fixes the collapse (21.5 blk/s). Residual: %100 full-book save 59-64 ms and window close
   68 ms mean / ~155 ms p99 on deep (s117, s120); upkeep p99 30 ms; -2.9% matched/s and +5.6% exec CPU per fill vs
   standard (s124). Resting depth cost under per-block Classic save: +0.61 ms/1k fills per 1M resting (s96).
   9n (deep re-run on batch10) is HOLD / excluded.
6. **Liveness with n = 3.** Any validator down halts the chain: SIGKILL -> first commit 27-40 s (s62), 48 s at 1M
   (s96.4). Timeouts 0.04-0.46/1k (CL21), 1.2-1.3 (M15). TC / dead-leader excluded.
7. **Binary drift.** `53a6d83c` -> `29bfc705` hash-off: matched/s -1.7/-3.0%, exec CPU per fill +5.3/+8.7% (s124.5);
   9l excluded. Hash on + attest + L6.1 + L2.1 cost +9.2% (deep) / +3.6% (std) exec CPU per fill vs R.
8. **Shared-host artefacts.** 3 nodes + bench on 32 threads; pinning was a loss (-5.7%); foreign CPU < 0.26 core.
9. **Bench-only:** attestation / oracle-feed ms-nonce collision (s124.2); margin collapse fixed by the cancel fix (s53).
10. **10m:** Phase 4 node build -2.5% at 10 markets, cause not found (s70); never re-measured since 2026-10-14.

## 6. Corners that look unexamined (observations only)

- O1. The checkpoint trigger is block-count driven: `height % 100` (`app.rs:4083-4086`) fires before 15 s whenever
  blk/s > 6.7, hash on or off. At 22 blk/s: 65-69 closes per 300 s, 87 MB (std) / 175 MB (deep) each, vs the 20 the
  15 s knob implies. s55 / s87 compared intervals at 3-11 blk/s; no result isolates the per-100-block cadence at 22
  blk/s. (Adjacent to A1/Q6 slicing, which is excluded.)
- O2. Per-block fixed costs grew in weight as blocks shrank 4x (44k -> 11.5k fills): verify 2.6 + replay 0.55 + phase 1
  2.3 + tail 3.65 + end_resident_wait 1.5 + upkeep 0.7-1.2 = ~11-12 of ~38 ms (~30%). The liquidation scan is per
  block (2,048 accounts), so its rate scales with blk/s (~45k accounts/s at 22 blk/s). L1.3 exists only as an idea.
- O3. Ingest side = 48.9% of node user cycles (q8re), on all 3 nodes. Each action is Keccak'd at ingress or gossip
  verify, again at dispatch on hotstuff-algo (`app.rs:11512`), again for the exec trust-cache key. Keccak is 13.7% of
  user cycles; hex decode 0.8%. Ideas in this area already exist (L5.1-L5.3, K1, T2.4, libsecp): check the ledger.
- O4. `rpc-worker` is 13.8% of user cycles with mutex contention and ReadView drops; the share caused by the
  bench's own submit and commit-tail polling against validators is not separated anywhere.
- O5. Thread supply: per node ~32 exec-pool + 32 rayon global + 16 + 16 verify + tokio + RocksDB threads; x3 on 32
  hardware threads, load1 35-45. Rayon idle spin (crossbeam epoch / steal) ~3.3% of user cycles; exec runqueue
  wait 3.0 ms/blk. Worker caps never A/B'd on archy (survey s4).
- O6. Exec block time is heavy-tailed (chain_ms p95/p50 ~35x) but no per-block attribution exists of which blocks are
  slow (checkpoint heights, %100 full saves, oracle-only blocks: 35-39 per cell, 4-7 ms avg, p95 15-22 ms).
- O7. Early-window blocks cost ~1.5x late ones (53.6 vs 35.9 ms) at 1.7x placed per block and 4x resting; 300 s
  cell means mix a transient and a steady state. matched_s_best60 290.8k vs avg 264.5k (std r2 CL21 headline).
- O8. Margin / prepare is the largest engine phase (10.3 ms/blk, 27% of the exec block) and runs serially by default;
  its parallel path is PE (excluded). Inside it (q8re EXEC inclusive): prepare_one 16.6%, Projections 6.5%,
  get_position 5.0%, take_open_slot 1.3%, Users::row 1.2%.
- O9. Cache `flush_all` 5.35 ms/blk (14%), unchanged per fill since s81; covered by Phase 5 (parked) and 9m (excluded).
- O10. Exec thread memory behaviour: IPC 1.18-1.23, 60 page faults per 1k fills, libc memmove 0.39 ms/1k (8u, deep,
  pre-L3.1). THP / huge pages, arena layout or prefetching appear in no result.
- O11. Network crypto: sha512 2.0% of user cycles on tokio = ed25519 over full gossipsub payloads (Signed + Strict);
  QUIC AEAD ~0.5%. Related open ideas: direct-protocol pre-spread, L4.1, L4.2.
- O12. RocksDB software-path CRC32C 0.70% (36% on hotstuff-algo); "hardware CRC32C" open in survey s7.
- O13. The 10-market shape has no data on any binary after 2026-10-14 (none of s58 bundle, 8g, L2.x, L3.x, T5.3, L6.1);
  it was bench-wave bound (exec busy 0.80), not exec bound.
- O14. Commit-age p50 344 ms vs block p50 11.6 ms: a 2-chain commit at a 45 ms mean view explains ~100 ms; the rest
  (mempool wait, views stalled behind exec dispatch) has no breakdown.
