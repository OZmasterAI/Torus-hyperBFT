# exp/archy-s58-perf-search: integration of the s58 experiments and bench plan

Branch `exp/archy-s58-perf-search` (worktree `/home/oz/projects/wt/exp-s58-perf-search`), base main `a44000e0`
(section 74, 71.8 option A). Six default-off experiments are merged with `git merge --no-ff`, in the order below.
The code head is merge `089be79d`; this doc is the commit after it.

## Status (2026-10-15, archy): merged, NOT bench-ready yet

* **All six merges were clean**: no textual conflicts, nothing skipped. A read-only review of the merged tree found no
  item-name collision (each flag has its own parse fn, pin, OnceLock and log line) and no duplicate top-level `use`.
  The overlap points in `execute_batch_phases` (stitch, top-ups, match setup, pass A, pass B, save) read as coherent.
* **Not built, not tested.** `/home` had 16 GB free (`df -h /home`: 254G size, 237G used), below the 25 GB gate for
  the first build, so no cargo command ran: no workspace nextest, doc tests, clippy, fmt or release build on the
  merged tree. The per-branch results in table (a) are the builders' results on each single branch; they do not
  prove the combination.
* **Disk.** btrfs exclusive usage of the finished s58 builders' private target dirs (`btrfs filesystem du -s`):
  `~/.cargo-target-exp-s58-pcbs` 33.6 GiB, `-grain` 24.2 GiB, `-stu` 22.1 GiB, `-pms` 19.1 GiB (about 99 GiB together).
  The shared `~/.cargo-target-exp-s58` has 16.3 GiB exclusive. Deleting any of them needs the owner's OK. The bench
  also needs space: the hsdb-style driver's preflight wants >= 20 GB free under `~/bench-results-matched`, and
  `campaign/run_cell.py` wants max(14, 6 + 0.2 x duration) GiB, which is 126 GiB for a 600 s cell.
* **origin/main has moved** to `5e0fbbfa` (22 commits past `a44000e0`). Two of those commits matter here:
  * `efe9aa47` merges 18c's own `perf/epoch-boundary-fast-path` (`7c8717d0`). It uses **the same flag name,
    `TORUS_EPOCH_FAST_PATH`**, and the same design: no-EVM, no-slash boundaries skip `pipeline_barrier`, and a
    boundary term in the checkpoint `due` rule. It also touches `exec_pipeline.rs`. That makes s58's
    epoch-boundary-fast-path a duplicate. When this branch moves to the new main, drop the s58 epoch merge and keep
    main's. `docs/perf/bench-queue.md` on origin/main already lists main's version as bench item 1 (log line
    `epoch fast path ON`; s58's version logs `TORUS_EPOCH_FAST_PATH: ... enabled=true`).
  * Item 7 step 1, EVM lanes (`853fed28`), rewrites large parts of `crates/torus-consensus/src/app.rs`. The diff
    `bb62bab1..7c8717d0` on app.rs is 1,655+ / 675-.

### To finish (next builder run, in this worktree, through the lock)

1. Free disk to at least 25 GB (owner decision, see above).
2. The shared target dir holds rlibs built from sibling worktrees, and cargo judges freshness by mtime (memory
   `f59a3792` / `abeb79d9`). Before the first build, run
   `git ls-files -z crates tools Cargo.toml Cargo.lock | xargs -0 touch` so every workspace crate rebuilds from this
   tree.
3. Integration fix, test first: `MarketWorkerPool::match_parallel_setup_with` (`market_workers.rs`, the
   `let workers = max_workers.max(1).min(n);` line) does not apply `grain_chunk_cap`. So when both
   `TORUS_PARALLEL_MATCH_SETUP=1` and `TORUS_EXEC_POOL_GRAIN=K` are set, the grain covers settle pass A only, not
   matching. The proposed fix is `let workers = grain_chunk_cap(max_workers.max(1)).min(n);`. It is identical when
   the grain flag is unset, because `grain_chunk_cap(cap) == cap` for K <= 1. Write the red test first: with both
   pinned (`with_parallel_match_setup(true, ..)` + `with_exec_pool_grain(4, ..)`), the setup path's job count
   equals `chunk_indices(.., grain_chunk_cap(cap)).len()`. Also fix the doc comment on that function, which says
   the layout is "the one `match_parallel_capped_with` would pick". Until this fix lands, the all-on arm measures
   grain for pass A only.
4. TESTING.md "Before every commit": `cargo nextest run --workspace` with the output flags (the count must exceed every
   single branch's 3,821-3,826, since the merged tree carries all six branches' new tests), `cargo test --doc`,
   `cargo clippy --workspace --all-targets` (0 new warnings vs `a44000e0`), `cargo fmt --check`. Also run the flag-on
   sets from each experiment's notes, plus one all-on run of `rdeps(torus-bridge)` with all five bridge flags set.
   Expected failures with `TORUS_EPOCH_FAST_PATH=1`: the 4 torus-consensus tests named in the epoch notes, which
   pin serial boundaries.
5. `cargo build --release -p torus-node` (and `bench-throughput` for the bench); record the md5s.

## (a) Merged experiments

All six: one commit each on `a44000e0`, merged in this order, default off, flag-off identical to main by the
builder's tests and by audit 1. Each has a quick Codex review (`torus-codex review --base exp/archy-s58-perf-search`,
outputs `/tmp/claude-1000/codex-s58-<slug>.md`) that reported "no actionable regressions", with 0 P0/P1/P2 findings.
Each also has audit 1, read-only, with no blocking issue. Gains are 300m 600 s per native block unless noted. They
are the builders' estimates, not measured. "Tests" are the builder's single-branch runs.

| # | slug (merge) | flag | what | expected gain | files | tests (single branch) | Codex / audit |
|---|---|---|---|---|---|---|---|
| 1 | settle-passb-deferred-results-scatter (`74b04223`, head `95e2d48e`) | `TORUS_SETTLE_DEFER_RESULTS=1` (only trimmed `1`) | Pass B stores each order's placed result as a 1-byte deferred record, then writes all results in one ascending pass after the market loop, removing the scattered 56-byte write and its drop-glue miss | pass B 41.5 -> 33-35 ms (-6.5 to -9 ms, about -2.5% block_ms); engine ms/1k fills -2.7 to -3.8%; matched/s +1.5 to +2.8% (central +2%); CPU-s/1M -0.4 to -0.5%; 10m about 0 | `native_executor.rs`; `tests/settle_defer_results_tests.rs` | differential flag off/on/env at caps 1/2/8, record_fills on and off: 31/31 per flag; `rdeps(torus-bridge)` 1968/1968 on and off; workspace 3821/3821 (flag unset), doc 1/0; 2 mutation checks caught | no findings / pass, not benched |
| 2 | epoch-boundary-fast-path (`484a02e0`, head `bb62bab1`) | `TORUS_EPOCH_FAST_PATH=1` (read once, stored in the ExecutionContext field) | No-EVM, no-slash epoch-boundary blocks take the pipelined path instead of the checkpoint-drain barrier. The `due` term forces the boundary's checkpoint, so the validator-set plan is durable before H+L | matched/s +1.1 to +1.7% (central +1.4%); exec block_ms -1.4 to -1.7%; device writes about -1 GB per node per 600 s; CPU-s/1M 0 to -0.3%; needs short epochs (bench and testnet use 100); 10m depth 1 about 0 | `crates/torus-consensus/src/app.rs` | torus-consensus 397/397 flag off; workspace 3823, 0 flaky; doc 1/0; flag on 393/397 (the 4 failures pin serial boundaries, expected); new tests (a)-(f) cover state identity, EVM and slash boundaries, plan-not-ready and crash replay | no findings / pass. **Duplicate of origin/main `efe9aa47`, same flag name** (see Status) |
| 3 | parallel-match-setup (`b80ed605`, head `6954e735`) | `TORUS_PARALLEL_MATCH_SETUP=1` | Each market's reduce-only map, account margins and match requests are built inside its pool match job (`match_parallel_setup_with`), not serially on the exec thread. Book take/create stays on the exec thread | match_ms -9 to -11.5 ms (120 s: -6.5 to -8); matched/s +2 to +2.8% (central +2.5%); native blk/s +3-5%; CPU-s/1M about 0 (work moves); 10m depth 4 +2 to +3.5%, depth 1 about 0 | `market_workers.rs`, `native_executor.rs`; `tests/parallel_match_setup_tests.rs` | 20/20 targeted; flag on: 207/207 on 6 binaries, `rdeps(torus-bridge)` 1970/1970; workspace 3823/3823 on rerun (first run: 1 timing failure in a checkpoint crash test, 5/5 in isolation); doc 1/0 | no findings / pass |
| 4 | sell-top-up-stitch-list (`43091381`, head `6d1100b8`) | `TORUS_FAST_SELL_TOP_UPS=1` | B-blind sell top-ups from a candidate list recorded at stitch time, with a per-sender run cache, replacing the full scan and sort. The s92 margin-cut counters are summed per market (one `inc_by`) | margin -5 to -11 ms; engine_untimed -2 to -4 ms; matched/s +1.5 to +3% (central +2.3%); CPU-s/1M -0.3 to -0.8%; 10m depth 1 about 0, depth 4 +1 to +2% | `native_executor.rs`; `src/sell_top_up_stitch_tests.rs`; `tests/sell_top_up_stitch_diff_tests.rs` | 12/12 targeted; flag on 180/180 on 4 binaries, `rdeps(torus-bridge)` 1969/1969 on and off; workspace 3822/3822; doc 1/0; mutation caught | no findings (quick review, tests not rerun by Codex) / pass; nonblocking: the res_qty basis rule in `TopUpCand::record` is not reliably test-guarded |
| 5 | exec-pool-fine-grain-jobs (`4b908f83`, head `aaf0b804`) | `TORUS_EXEC_POOL_GRAIN=K` (K 2..=16; unset, garbage, 0 or 1 = off) | Match and settle pass-A work split into K x cap LPT chunks, so idle pool threads pick up work from late starters (3 nodes share 32 threads) | K=4: match pool -3.5 to -6 ms, pass A -1.5 to -2.5 ms (block_ms -1.6 to -2.2%); matched/s +1 to +2% (central +1.6%, near the CI); CPU about 0; 10m about 0 | `market_workers.rs`, `native_executor.rs` (8 lines); `tests/exec_pool_grain_tests.rs` | 23/23; K=4 50/50 incl. GOLDEN digests; `rdeps(torus-bridge)` 1971/1971 unset and K=4; workspace 3824/3824; doc 1/0 (private target dir) | no findings / pass. With both flags on, see "To finish" step 3 |
| 6 | parallel-classic-book-save (`089be79d`, head `8525822b`) | `TORUS_PARALLEL_BOOK_SAVE=1` (min 16,384 orders) | Classic book blobs encoded as per-book exec-pool jobs into presized buffers; the puts stay serial in ascending market order; a job panic latches a save fault (fail-stop) | save_books 21.7 -> about 6-9 ms (late cell 23-26 -> 7-10); matched/s +1.5 to +3.5% (central +2.5%; 120 s +1 to +2.5%); native blk/s +3-5%; CPU-s/1M neutral to -0.3%; RSS +-0.1-0.3 GB; 10m depth 1 about 0, depth 4 +3 to +6% | `native_executor.rs`; `tests/parallel_book_save_tests.rs`; `tests/classic_book_save_ubench.rs` | 14 targeted (11 inline, 3 integration); flag on 60/60, `rdeps(torus-bridge)` 1973/1973; workspace 3826/3826; doc 1/0; 2 mutations caught; 300-book microbench gate passes | no findings / pass. Classic only: no-op under `TORUS_BOOK_ROWS=2/3` |

Per-experiment notes, with the full test lists and bench recipes: `docs/plans/exp-archy-s58-<slug>.md`.

## (b) Skipped or failed

* **Skipped at merge: none.** All six merged without conflicts.
* **Not mergeable (input list): none.**
* **Integration findings, to resolve before benching:**
  * Grain does not reach the setup-in-jobs match path (Status, step 3). It is a coverage gap, not a correctness
    issue: each flag alone keeps its semantics, and state does not depend on the chunk layout.
  * The epoch fast path duplicates origin/main `efe9aa47` (same flag). Keep it here only until the branch moves to
    the new main.
  * The merged tree is unbuilt (disk), so a compile or test break from the combination is still possible.

## (c) Zero-code knob arms (no build of this branch needed, unless noted)

| arm | env / build | why | expected | run as |
|---|---|---|---|---|
| wal-compression-post-phase3 | `TORUS_ROCKSDB_WAL_COMPRESSION=zstd` (state DB and consensus DB, `DbTuning::from_env`) | Owner decision s109: re-decide WAL compression after Phase 3 with a tw vs tw+zstd A/B. Phase 3 and the own-DB split are merged; that A/B has not run. acc3 (pre-Phase 3): +1.7% matched/s, +2.4% CPU, -83% bytes | device writes sharply down (WAL about 35 + 26 MB/s per node); CPU-s/1M +1 to +3%; risk: longer in-window stall (the flush worker compresses the ~200 MB batch) and zstd on commit_persist | 300m 600 s ABBA n=3-4 + 1 SIGKILL restart cell; separate from the push-threshold arm |
| push-threshold-production-default | `TORUS_HASH_ONLY_PUSH_THRESHOLD=524288` (compiled default; env.sh pins 8,000,000) | Followers ingest each action twice (pre-spread + full-body push), about 470k duplicates per node per 600 s | CPU-s/1M -3.8 to -4.9%; device writes -12 to -15 MB/s per node; matched/s about +0.2% (tie) | 300m 600 s ABBA n=3-4 + one 10m pair; owner call (moves the baseline to the production path); guards: AGREE, dissemination_clean, DA pull / MissingData, gossip dropped_full 0 |
| header-resend-250ms | `TORUS_HEADER_RESEND_MS=250` | The 3,000 ms default never fires before the 1.2 s view timeout. The direct path skips the 50 msg/s per-author gossip limit | 10m depth 1: block p99 about 1.33 -> 0.55-0.8 s, timeouts 20-25 -> 0-3 per 120 s; matched/s about 0 (maybe -1%); 300m unchanged | 10m 120 s ABBA n=4 + 300m 600 s guard pair; before any hold-light-proposals code |
| rocksdb-hw-crc32c-build | build only: `CXXFLAGS_x86_64_unknown_linux_gnu='-msse4.2 -mpclmul'`, own target dir, pin `EXPECT_NODE_MD5` | Every RocksDB LOG says "Fast CRC32 supported: Not supported on x86" | checkpoint DB write 0.60 -> 0.40-0.45 s; commit_persist_write about 8 -> 4 ms; CPU-s/1M -0.8 to -1.0%; matched/s 0 to +0.5% | check `nm` (crc32c_3way) and both LOGs first; changes the baseline for every later A/B; 1 SIGKILL restart and cross-build reopen before adopting |
| parallel-verify-serial-probe | `TORUS_PARALLEL_VERIFY=1` (`VerifyMode::Serial`) | At 300m the exec trust cache hits about 100%, yet batch_verify still forks and joins on rayon every block | exec_verify 16.8 -> 14.5-15.5 ms; matched/s +0.3 to +0.8% (below CI) | cheap third arm in any 300m campaign; report exec_verify_seconds per native block and end_resident_wait |
| release-thin-lto-build | build only: `CARGO_PROFILE_RELEASE_LTO=thin` (optional fat + `CODEGEN_UNITS=1`), keep RUSTFLAGS and panic=unwind, own target dir | No `[profile.release]`; FixedPoint ops and torus-core margin helpers are cross-crate calls | matched/s +0.9 to +2.0% (central +1.4%); CPU-s/1M -0.6 to -1.3%; low priority (gain lens refuted the larger claim) | check `nm` first; 600 s n >= 4; judge on CPU-s/1M and engine ms per 1k fills; the fat build holds the lock 10-30 min |
| cloid-share-sizing | harness only: `CLOID_SHARE=25` and `=50` on main | Realistic cloid shares are unmeasured (c100 is 9.3% below c0); parallel-sharded-cloid-upkeep gains 0 at c0 | sizes the cloid cost curve; no direct gain | 300m 120 s, n=2 per share vs c0; report matched/s, engine_untimed and phase1 per 1k fills |

## (d) Every candidate of the search and its verdict

Lenses: gain / safety / novelty (keep or refute); the reason column condenses the lens notes.

**Survived (23)**

| candidate | verdict | reason |
|---|---|---|
| parallel-classic-book-save | **merged** (#6) | save_books is memory-bandwidth-bound serialization on the exec thread; 2.5-3.5x expected on the pool; all lenses keep |
| settle-passb-deferred-results-scatter | **merged** (#1) | results-write stall 3.2-4.0% of exec on-CPU (8.5-10 ms per native block); all lenses keep |
| parallel-match-setup | **merged** (#3) | setup 7.9-8.6 ms per native block serial on the exec thread (verified); all lenses keep |
| epoch-boundary-fast-path | **merged** (#2), duplicate of main `efe9aa47` | about 0.5 s recoverable per epoch (drain 0.38 s + serial flush 0.12 s); all lenses keep |
| sell-top-up-stitch-list | **merged** (#4) | scan, sort, params deref and cold PreparedOrder read removed (about 300 ns per candidate); all lenses keep |
| exec-pool-fine-grain-jobs | **merged** (#5) | 3-8 ms start skew of pool jobs on the shared host; all lenses keep; gain near the n=4 CI |
| ingress-market-spec-cache | survived, not built this round | CPU only: CPU-s/1M -4 to -5%, ingress-verify CPU -30 to -37%; matched/s about 0 at 300m (exec-bound) |
| bench-push-manifest-not-full-body | survived, not built; measured first by the push-threshold knob arm | -0.20 to -0.26 cores per node, CPU-s/1M -3 to -4.5% |
| da-cf-blob-files | survived, not built | BlobDB for cf_native_pending: DA compaction 113 CPU-s per node per 600 s; CPU-s/1M -2.6 to -3.8% |
| hold-light-proposals-under-consensus-rate-limit | survived, not built; header-resend-250ms first | 10m block p99 -55 to -60%; matched/s about 0 |
| exec-carried-verified-senders | survived, not built; parallel-verify-serial-probe sizes its fork-join part | exec_verify 16.8 -> about 0.3 ms per native block; matched/s +1.3% (sec 67.4 ratio) up to +4.5%; overlaps s123 exec-action-keys |
| rpc-block-body-raw-json | survived (gain lens refutes throughput), low priority | matched/s about 0 at 300m; CPU only on val2 (body server), 2-3x serve-cost cut unproven without a microbench |
| release-lto-cgu1-build-ab | gain lens refutes the larger claim; kept as knob arm release-thin-lto-build | removable cost smaller than claimed; central +1.4% at the CI |
| ecrecover-libsecp256k1-backend | survived, not built | CPU only: about -0.3 cores per node in gossip / ingress verify; matched/s tie |
| rocksdb-hw-crc32c-cxxflags | knob arm rocksdb-hw-crc32c-build | software CRC on every WAL byte; CPU-s/1M -0.8 to -1.0%; matched/s about 0 |
| eip712-batch-item-keccak-x4 | survived, not built | CPU-s/1M -4 to -5% at 300m (item-hash Keccak about 0.45 cores per node); matched/s 0 to +1% |
| parallel-sharded-cloid-upkeep | survived, gated on the cloid-share-sizing arm | gains 0 at CLOID_SHARE 0 (every campaign shape); -0.2 ms per 1k fills only at c100 |
| ingress-view-skip-position-slices | survived, not built | RSS -0.2 to -0.4 GB mean; matched/s about 0; CPU-s/1M -0.4 to -1% |
| header-resend-250ms | knob arm | latency only (10m block p99, timeouts) |
| native-prespread-direct-zstd | survived, not built | CPU only: about -0.2 cores per node (3.7%) |
| compact-body-record-from-da | survived, not built | CPU-s/1M -1.4 to -1.6%, bytes written down; matched/s 0 |
| verify-pools-parking-executor | survived, not built, small | -0.05 to -0.07 cores per node (about 1%) |
| pass-a-hot-market-trader-shards | gain lens refutes for the tracked cells | 0 at 300m uniform (about 280 fills per market); helps only a skewed hot-market shape the bench does not run |
| proposer-commit-on-advance-view | survived, not built, latency only | val0 out-of-window p99 about 1.2 -> 0.7-0.85 s; matched/s 0 |

**Rejected or dropped (6)**

| candidate | stage | reason |
|---|---|---|
| by-id-heavy-bench-cell | triage | measurement only; at most 0.4 ms per by-id action and about 1 by-id action per native block (< 0.2% matched) |
| lazy-shard-custody-serve-time | verify (gain) | premise true in code; gain unconfirmed and 0 on every measured configuration |
| bulk-thread-nice | verify (gain) | refuted: the exec thread loses only 2.5-2.6% of wall to run delay; renicing the verify pools does not recover it |
| quic-prefer-aes128-gcm | triage | -0.4 to -0.6% CPU-s/1M, at or below n=4 resolution, about -0.05% after correction |
| match-epilogue-batched-trader-index | verify (gain) | premise holds; the post-match epilogue is too small to move engine_untimed measurably |
| small-place-order-charged-block-budget | verify (gain) | 0 on every tracked cell (all 856 archived cells use `--batch-size 400`) |

**Triaged, not verified (1)**: compact-exec-channel-handoff (triage score 13): queue committed blocks to exec as
compact references and rebuild them just in time, freeing about 0.4 GB of decoded blocks in the 64-deep exec
channel. Not verified this round.

## (e) Bench plan (for the bench agents)

Precondition: steps 1-5 of "To finish" are done (suite green, release md5 recorded), disk is free for the driver,
the host is quiet, and the owner has decided the epoch question (arm E below).

**Reference points, re-checked in `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`:**
* Sec 67.3: the n=4 paired matched/s CI half-width is +-1.32% at 300m and +-0.98% at 10m. The native blk/s CI is
  wide (+-13.6% paired), so native blk/s cannot resolve single-flag effects.
* Sec 67.3/67.4: at 300m, block_ms -10.16% (CI [-21.4, +1.1]) went with matched/s +3.01%, a ratio of about 0.3% per
  1% of block_ms. **Most single-arm estimates in (a) imply a ratio of 0.8-1.0**, so by the 67.4 ratio each single
  flag's matched/s gain is +0.4 to +0.9%, inside the CI. For single arms the primary readout is therefore the
  mechanism timer and block_ms; matched/s is secondary. The all-on arm is the one expected to resolve matched/s.
* 300m is exec-bound (sec 67.3: exec busy 0.97). 10m is wave-shaped and paced by the bench tail (sec 70.5).

**Setup.** Same shape as `ozarchy-hsdb` (sec 74):
* 300 markets, Classic, oracle on (30,000 / 2,000 ms, walk 0), 15 s checkpoint role default, commit tail depth 1,
  BLOCK_CAP 400, MAX_IN_FLIGHT 4, budget 900, rate 76,000, 5,000 senders, jemalloc defaults.
* Node env KNOBS blank `TORUS_BOOK_ROWS=` and the other p4j4/hsdb knobs. **Required**: run-cell.sh alone exports
  `TORUS_BOOK_ROWS=3`, which turns checkpoints off (so the epoch arm has nothing to measure) and makes
  `TORUS_PARALLEL_BOOK_SAVE` a no-op.
* Driver: start from `~/bench-results-matched/ozarchy-hsdb-tools/ozarchy-hsdb-campaign.sh` or origin/main's
  `campaign/make-driver.py`. The harness worktree may sit on origin/main; the node binary is this branch's.
* **One node binary** (this branch's release build) for every arm; arms differ only in EXTRA_ENV, set on all 3
  validators. Flags are node-local and state is byte-identical, so AGREE must hold in every cell.
* List every unset flag of an arm as absent: `TORUS_SETTLE_DEFER_RESULTS TORUS_EPOCH_FAST_PATH
  TORUS_PARALLEL_MATCH_SETUP TORUS_FAST_SELL_TOP_UPS TORUS_EXEC_POOL_GRAIN TORUS_PARALLEL_BOOK_SAVE`.
  Keep `TORUS_PARALLEL_ENGINE` unset in all arms.
* **Took-effect checks** (1 line per node per process, logged at first use): `TORUS_SETTLE_DEFER_RESULTS: ... on=true`,
  `TORUS_EPOCH_FAST_PATH: ... enabled=true`, `TORUS_PARALLEL_MATCH_SETUP: ... enabled=true`,
  `TORUS_FAST_SELL_TOP_UPS (s58): ... enabled=true`, `TORUS_EXEC_POOL_GRAIN: ... grain=4`,
  `TORUS_PARALLEL_BOOK_SAVE: ... enabled=true`. The control arm must show the off values.

**Arms.**

| arm | EXTRA_ENV (on all 3 validators) |
|---|---|
| C (control) | all six flags unset |
| D | `TORUS_SETTLE_DEFER_RESULTS=1` |
| E | `TORUS_EPOCH_FAST_PATH=1` (owner choice: run here, or leave it to bench-queue item 1 on main `efe9aa47` and drop E) |
| M | `TORUS_PARALLEL_MATCH_SETUP=1` |
| T | `TORUS_FAST_SELL_TOP_UPS=1` |
| G | `TORUS_EXEC_POOL_GRAIN=4` |
| B | `TORUS_PARALLEL_BOOK_SAVE=1` |
| ALL | all six (grain 4) |

**Cells.** 600 s each. 300m first, then 10m (depth 1). Each round runs every arm once; the order rotates and
alternate rounds are reversed (ABBA generalised). Use n >= 4 rounds (n = 6 if G is kept: its gain is near the CI).
Add one 60 s warm-up and one SIGKILL restart cell on ALL (book-save fault latch, epoch replay, WAL replay).
* Size: 8 arms x 4 rounds = 32 cells per market count. At sec 74's pace (about 12-13 min per 600 s cell including
  drain and digest), that is about 7 h at 300m and about 7 h at 10m.
* If time is short, run 300m in full and, at 10m, only C vs ALL (n=4, about 1.7 h), bisecting by single arms only if
  ALL regresses.

**Metrics, every cell:** matched/s (paired CI vs C), native blk/s, exec block_ms (and block_ms with fills per block),
block p99 (all, in and out of checkpoint windows), CPU-s/1M (ANCOVA at equal fills), RSS mean / peak (minutes 5-10),
consensus timeouts, device MB/s, ACCEPT AGREE, plus the mechanism timer of each arm.

**What should move, per arm (300m 600 s; the 10m expectation is about 0 at depth 1 for every arm):**

| arm | primary (mechanism) | block_ms | matched/s (arm's claim / by the 67.4 ratio) | other |
|---|---|---|---|---|
| D | settle_pass_b_ms per native block 41.5 -> 33-35; engine ms/1k fills -2.7 to -3.8% | about -2.5% | +1.5 to +2.8% / about +0.75% | CPU-s/1M -0.4 to -0.5% |
| E | 'executing finalized block' with pipelined=false at h%100: about 18-19 -> 0 per node; gap before h%100 about 500 -> about 0 ms; exec_flush_seconds_count about 0; 'validator-set plan not ready' 0 | -1.4 to -1.7% | +1.1 to +1.7% / about +0.5% | device writes about -1 GB per node; CPU-s/1M 0 to -0.3% |
| M | phase_match_ms per native block -9 to -11.5 ms (match ms per 1k placed 0.33 -> 0.22-0.24) | about -3% | +2 to +2.8% / about +0.9% | native blk/s +3-5%; CPU-s/1M about 0; watch hotstuff runqueue wait |
| T | margin_ms -5 to -11 ms; engine_untimed -2 to -4 ms; match_ms may rise slightly (lost warming) | about -2 to -5% | +1.5 to +3% / about +0.7 to +1.4% | CPU-s/1M -0.3 to -0.8% |
| G | phase_match_ms -3.5 to -6 ms; settle_pass_a_ms -1.5 to -2.5 ms | -1.6 to -2.2% | +1 to +2% / about +0.5-0.7% | CPU-s/1M about 0 |
| B | save_books per native block 21.7 -> 6-9 ms (late 60 s 23-26 -> 7-10); torus_exec_save_books_workers > 1; share of blocks at the 400 cap | about -4 to -5% | +1.5 to +3.5% / about +1.2 to +1.5% | native blk/s +3-5%; RSS within +-0.3 GB; CPU-s/1M 0 to -0.3% |
| ALL | sum of the above timers: about -41 to -59 ms per native block (central about -50) of about 313-323 (grain's match part is missing until step 3 lands) | about -13 to -18% | +4 to +5.5% by the 67.4 ratio (the arms' own claims sum to about +10-12%; expect the lower figure) | CPU-s/1M -1 to -2%; block p99 not expected to move; AGREE and 0 fail-stops |

Reject an arm if AGREE fails in any cell, if any node logs a fail-stop or panic, or if consensus timeouts rise
outside C's range. Report each arm's paired delta with its 95% CI and the number of rounds with the same sign.
