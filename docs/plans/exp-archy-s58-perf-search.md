# exp/archy-s58-perf-search: integration of the s58 experiments and bench plan

Branch `exp/archy-s58-perf-search` (worktree `/home/oz/projects/wt/exp-s58-perf-search`, local only, not pushed).
Six default-off experiments were merged with `git merge --no-ff` onto main `a44000e0` (section 74, 71.8 option A),
in the order of table (a). The branch then moved to current main: revert `78373203` drops the s58 epoch experiment
(a duplicate of main's `efe9aa47`), and merge `c7d9d367` brings in origin/main `5e0fbbfa`. Five s58 experiments
remain, on top of main `5e0fbbfa`. The code head is merge `c7d9d367`; this doc is the commit after it.

## Status (2026-10-15, archy): on main `5e0fbbfa`, built and tested, bench-ready

* **Moved to main `5e0fbbfa`.** `efe9aa47` on main merges 18c's own epoch-boundary fast path
  (`perf/epoch-boundary-fast-path`, `7c8717d0`) under **the same flag, `TORUS_EPOCH_FAST_PATH`**, with the same design:
  no-EVM, no-slash boundaries skip `pipeline_barrier`, and a boundary term in the checkpoint `due` rule. Owner
  decision: drop the s58 copy. Revert `78373203` (`git revert -m 1 484a02e0`) removes its app.rs change and its plan
  doc; both stay on branch `exp/archy-s58-epoch-boundary-fast-path`. After the revert, app.rs equals `a44000e0`'s.
  Merge `c7d9d367` (`git merge --no-ff origin/main`) then brings in item 7 step 1, EVM lanes (`853fed28`, a large
  app.rs rewrite), `efe9aa47`, the campaign driver generator (`26f9acd2`, `ff043606`) and `docs/perf/bench-queue.md`.
  There were no conflicts: app.rs was the only file changed on both sides. The tree now differs from origin/main only
  in the five experiments' 15 files (`torus-bridge` code and tests, plan docs); app.rs is main's, byte for byte.
* **All six original merges were clean**: no textual conflicts, nothing skipped. A read-only review of the merged
  tree found no item-name collision (each flag has its own parse fn, pin, OnceLock and log line) and no duplicate
  top-level `use`. The overlap points in `execute_batch_phases` (stitch, top-ups, match setup, pass A, pass B, save)
  read as coherent.
* **Suite on the merged tree.** Commands are from TESTING.md "Before every commit". The run used the shared
  `~/.cargo-target-exp-s58`, after `git ls-files -z crates tools Cargo.toml Cargo.lock | xargs -0 touch`, so that no
  sibling worktree's rlib could count as fresh (memory `f59a3792` / `abeb79d9`). The torus_bridge and torus_consensus
  artifacts embed only this worktree's path.
  * `cargo nextest run --workspace`: 3,902 tests run, 3,902 passed, 0 failed, 40 skipped, 0 flaky (112 s).
  * `cargo test --doc`: rc 0, passed 1, failed 0 (7 ignored).
  * All five bridge flags on together (`TORUS_SETTLE_DEFER_RESULTS=1 TORUS_PARALLEL_MATCH_SETUP=1
    TORUS_FAST_SELL_TOP_UPS=1 TORUS_EXEC_POOL_GRAIN=4 TORUS_PARALLEL_BOOK_SAVE=1`), with
    `cargo nextest run -E 'rdeps(torus-bridge)'`: 2,031 tests run, 2,031 passed, 18 skipped, 0 flaky (72 s). Each
    experiment's own flag-on sets ran on its single branch (table (a)); they were not rerun one flag at a time here.
  * `cargo clippy --workspace --all-targets`: rc 0, 272 warnings. Origin/main `5e0fbbfa` gives the same 272 with the
    same command (in its own target dir), and the (file, message) pairs are identical: **0 new**. The only warning in
    a file this branch changes (`native_executor.rs`, `sort_by` in `sort_deterministic`) is in main's code.
  * `cargo fmt --all --check` fails on origin/main itself: 3,678 hunks in 195 files, because main is not fmt-clean.
    The merged tree has 3,677 hunks in the same 195 files. With indentation normalized, every line it flags is
    flagged the same way on main: **0 new**. Two of main's hunks are gone, because s58 code rewrote those lines. Never
    run `cargo fmt --all` here: it would reformat main's 195 files.
* **Release build.** `cargo build --release -p torus-node` took 2 min 30 s. Binary:
  `/home/oz/.cargo-target-exp-s58/release/torus-node`, **md5 `c74dea5242024274430677780018dcce`** (53,728,800 bytes,
  2026-10-15 19:35). Every workspace crate in it was compiled from this worktree. The binary holds the five s58 flag
  names and main's `epoch fast path ON (TORUS_EPOCH_FAST_PATH=1)` line. Pin it as the campaign's
  `EXPECT_NODE_MD5`. `tools/bench-throughput` is identical to main `5e0fbbfa`, so the harness's own bench-throughput
  build from main can be used.
* **Disk** (/home, after the builds): 86 GB free. That is not enough for 600 s cells that go through
  `campaign/run_cell.py`: it refuses a cell unless max(14, 6 + 0.2 x duration) GiB are free, which is 126 GiB at
  600 s. The hsdb-style driver's preflight wants >= 20 GB free under `~/bench-results-matched`. The s58 builders'
  private target dirs (`~/.cargo-target-exp-s58-pcbs`, `-grain`, `-stu`, `-pms`, about 99 GiB) are already gone.
  Cleanup candidates, which need the owner's OK (btrfs exclusive usage, measured after this run):
  * `target/` in four per-experiment worktrees: `wt/exp-s58-epoch-boundary-fast-path` 2.13 GiB,
    `wt/exp-s58-parallel-classic-book-save` 2.16 GiB, `wt/exp-s58-parallel-match-setup` 2.31 GiB and
    `wt/exp-s58-settle-passb-deferred-results-scatter` 1.87 GiB (about 8.5 GiB together);
  * `~/.cargo-target-exp-s58-mainref` 1.13 GiB, the reflink-seeded target dir of this run's clippy reference on main;
  * after the bench: the shared `~/.cargo-target-exp-s58` (19.5 GiB exclusive), which holds the bench binary.

## Open items (before or during the bench)

1. **The grain does not reach the setup-in-jobs match path** (not fixed). With `TORUS_PARALLEL_MATCH_SETUP=1`,
   matching runs through `MarketWorkerPool::match_parallel_setup_with`
   (`crates/torus-bridge/src/market_workers.rs`, line 238: `let workers = max_workers.max(1).min(n);`). That function
   does not apply `grain_chunk_cap`. `match_parallel_capped_with` (line 147) and settle pass A (`native_executor.rs`,
   line 9723) do apply it. So with setup and `TORUS_EXEC_POOL_GRAIN=K` both on, the grain splits settle pass A only,
   not matching. Proposed fix, test first: `let workers = grain_chunk_cap(max_workers.max(1)).min(n);`. It is
   identical with the grain flag unset, because `grain_chunk_cap(cap) == cap` for K <= 1. Red test: with both pinned
   (`with_parallel_match_setup(true, ..)` + `with_exec_pool_grain(4, ..)`), the setup path's job count equals
   `chunk_indices(.., grain_chunk_cap(cap)).len()`. Also fix that function's doc comment, which says the layout is
   "the one `match_parallel_capped_with` would pick". Until the fix lands, the ALL arm measures the grain on settle
   pass A only (see the ALL rows in (e)).
2. **Disk for the bench**: see Status, Disk.

## (a) Merged experiments

All six: one commit each on `a44000e0`, merged in this order, default off, flag-off identical to main by the
builder's tests and by audit 1. #2 (epoch) was reverted in `78373203` when the branch moved to main `5e0fbbfa`; arm E
now runs 18c's implementation from main (`efe9aa47`, same flag). Each has a quick Codex review
(`torus-codex review --base exp/archy-s58-perf-search`, outputs `/tmp/claude-1000/codex-s58-<slug>.md`) that reported
"no actionable regressions", with 0 P0/P1/P2 findings.
Each also has audit 1, read-only, with no blocking issue. Gains are 300m 600 s per native block unless noted. They
are the builders' estimates, not measured. "Tests" are the builder's single-branch runs.

| # | slug (merge) | flag | what | expected gain | files | tests (single branch) | Codex / audit |
|---|---|---|---|---|---|---|---|
| 1 | settle-passb-deferred-results-scatter (`74b04223`, head `95e2d48e`) | `TORUS_SETTLE_DEFER_RESULTS=1` (only trimmed `1`) | Pass B stores each order's placed result as a 1-byte deferred record, then writes all results in one ascending pass after the market loop, removing the scattered 56-byte write and its drop-glue miss | pass B 41.5 -> 33-35 ms (-6.5 to -9 ms, about -2.5% block_ms); engine ms/1k fills -2.7 to -3.8%; matched/s +1.5 to +2.8% (central +2%); CPU-s/1M -0.4 to -0.5%; 10m about 0 | `native_executor.rs`; `tests/settle_defer_results_tests.rs` | differential flag off/on/env at caps 1/2/8, record_fills on and off: 31/31 per flag; `rdeps(torus-bridge)` 1968/1968 on and off; workspace 3821/3821 (flag unset), doc 1/0; 2 mutation checks caught | no findings / pass, not benched |
| 2 | epoch-boundary-fast-path (`484a02e0`, head `bb62bab1`), **reverted in `78373203`** | `TORUS_EPOCH_FAST_PATH=1` (read once, stored in the ExecutionContext field) | No-EVM, no-slash epoch-boundary blocks take the pipelined path instead of the checkpoint-drain barrier. The `due` term forces the boundary's checkpoint, so the validator-set plan is durable before H+L | matched/s +1.1 to +1.7% (central +1.4%); exec block_ms -1.4 to -1.7%; device writes about -1 GB per node per 600 s; CPU-s/1M 0 to -0.3%; needs short epochs (bench and testnet use 100); 10m depth 1 about 0 | `crates/torus-consensus/src/app.rs` | torus-consensus 397/397 flag off; workspace 3823, 0 flaky; doc 1/0; flag on 393/397 (the 4 failures pin serial boundaries, expected); new tests (a)-(f) cover state identity, EVM and slash boundaries, plan-not-ready and crash replay | no findings / pass. **Duplicate of origin/main `efe9aa47` (18c's, same flag name, same design): reverted. Arm E runs main's implementation** (see Status) |
| 3 | parallel-match-setup (`b80ed605`, head `6954e735`) | `TORUS_PARALLEL_MATCH_SETUP=1` | Each market's reduce-only map, account margins and match requests are built inside its pool match job (`match_parallel_setup_with`), not serially on the exec thread. Book take/create stays on the exec thread | match_ms -9 to -11.5 ms (120 s: -6.5 to -8); matched/s +2 to +2.8% (central +2.5%); native blk/s +3-5%; CPU-s/1M about 0 (work moves); 10m depth 4 +2 to +3.5%, depth 1 about 0 | `market_workers.rs`, `native_executor.rs`; `tests/parallel_match_setup_tests.rs` | 20/20 targeted; flag on: 207/207 on 6 binaries, `rdeps(torus-bridge)` 1970/1970; workspace 3823/3823 on rerun (first run: 1 timing failure in a checkpoint crash test, 5/5 in isolation); doc 1/0 | no findings / pass |
| 4 | sell-top-up-stitch-list (`43091381`, head `6d1100b8`) | `TORUS_FAST_SELL_TOP_UPS=1` | B-blind sell top-ups from a candidate list recorded at stitch time, with a per-sender run cache, replacing the full scan and sort. The s92 margin-cut counters are summed per market (one `inc_by`) | margin -5 to -11 ms; engine_untimed -2 to -4 ms; matched/s +1.5 to +3% (central +2.3%); CPU-s/1M -0.3 to -0.8%; 10m depth 1 about 0, depth 4 +1 to +2% | `native_executor.rs`; `src/sell_top_up_stitch_tests.rs`; `tests/sell_top_up_stitch_diff_tests.rs` | 12/12 targeted; flag on 180/180 on 4 binaries, `rdeps(torus-bridge)` 1969/1969 on and off; workspace 3822/3822; doc 1/0; mutation caught | no findings (quick review, tests not rerun by Codex) / pass; nonblocking: the res_qty basis rule in `TopUpCand::record` is not reliably test-guarded |
| 5 | exec-pool-fine-grain-jobs (`4b908f83`, head `aaf0b804`) | `TORUS_EXEC_POOL_GRAIN=K` (K 2..=16; unset, garbage, 0 or 1 = off) | Match and settle pass-A work split into K x cap LPT chunks, so idle pool threads pick up work from late starters (3 nodes share 32 threads) | K=4: match pool -3.5 to -6 ms, pass A -1.5 to -2.5 ms (block_ms -1.6 to -2.2%); matched/s +1 to +2% (central +1.6%, near the CI); CPU about 0; 10m about 0 | `market_workers.rs`, `native_executor.rs` (8 lines); `tests/exec_pool_grain_tests.rs` | 23/23; K=4 50/50 incl. GOLDEN digests; `rdeps(torus-bridge)` 1971/1971 unset and K=4; workspace 3824/3824; doc 1/0 (private target dir) | no findings / pass. With `TORUS_PARALLEL_MATCH_SETUP=1` also on, the grain skips matching (Open items, 1) |
| 6 | parallel-classic-book-save (`089be79d`, head `8525822b`) | `TORUS_PARALLEL_BOOK_SAVE=1` (min 16,384 orders) | Classic book blobs encoded as per-book exec-pool jobs into presized buffers; the puts stay serial in ascending market order; a job panic latches a save fault (fail-stop) | save_books 21.7 -> about 6-9 ms (late cell 23-26 -> 7-10); matched/s +1.5 to +3.5% (central +2.5%; 120 s +1 to +2.5%); native blk/s +3-5%; CPU-s/1M neutral to -0.3%; RSS +-0.1-0.3 GB; 10m depth 1 about 0, depth 4 +3 to +6% | `native_executor.rs`; `tests/parallel_book_save_tests.rs`; `tests/classic_book_save_ubench.rs` | 14 targeted (11 inline, 3 integration); flag on 60/60, `rdeps(torus-bridge)` 1973/1973; workspace 3826/3826; doc 1/0; 2 mutations caught; 300-book microbench gate passes | no findings / pass. Classic only: no-op under `TORUS_BOOK_ROWS=2/3` |

Per-experiment notes, with the full test lists and bench recipes: `docs/plans/exp-archy-s58-<slug>.md`. The epoch
note left this branch with the revert; it is still on branch `exp/archy-s58-epoch-boundary-fast-path`.

## (b) Skipped or failed

* **Skipped at merge: none.** All six merged without conflicts.
* **Reverted after merge: epoch-boundary-fast-path** (`78373203`), a duplicate of origin/main `efe9aa47` (same flag).
* **Not mergeable (input list): none.**
* **Integration findings:**
  * Open: the grain does not reach the setup-in-jobs match path (Open items, 1). It is a coverage gap, not a
    correctness issue: each flag alone keeps its semantics, and state does not depend on the chunk layout.
  * Resolved: the epoch duplicate. It was reverted, and the branch is on main `5e0fbbfa`.
  * Resolved: the merged tree is built and tested, also with all five bridge flags on together (Status).

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
| epoch-boundary-fast-path | **merged** (#2), then **reverted** (`78373203`) as a duplicate of main `efe9aa47`; arm E runs main's | about 0.5 s recoverable per epoch (drain 0.38 s + serial flush 0.12 s); all lenses keep |
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

Precondition: the suite is green and the release md5 is recorded (both done, see Status), disk is free for the
driver (not yet for `run_cell.py` at 600 s, see Status, Disk), and the host is quiet. The epoch question is decided:
arm E runs 18c's implementation from main.

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
  `TORUS_PARALLEL_MATCH_SETUP: ... enabled=true`, `TORUS_FAST_SELL_TOP_UPS (s58): ... enabled=true`,
  `TORUS_EXEC_POOL_GRAIN: ... grain=4`, `TORUS_PARALLEL_BOOK_SAVE: ... enabled=true`. The control arm must show the
  off values.
* **Arm E is main's code** (`efe9aa47`). It logs `epoch fast path ON (TORUS_EPOCH_FAST_PATH=1)` once at startup, only
  when the flag is on and the exec pipeline runs: `zgrep -c 'epoch fast path ON' val*.log.gz` gives 1 per node in E
  and ALL, 0 in the other arms. The warning `TORUS_EPOCH_FAST_PATH is set but the exec pipeline is off` means the
  arm has no effect. Main's parse takes `1` or `true` (any case); any other value logs a warning and stays off.
* **Arm B also needs a per-block check**: the parallel save only runs when at least 2 dirty books with at least
  16,384 resting orders in total are present. See the B row below for the metric.

**Arms.**

| arm | EXTRA_ENV (on all 3 validators) |
|---|---|
| C (control) | all six flags unset |
| D | `TORUS_SETTLE_DEFER_RESULTS=1` |
| E | `TORUS_EPOCH_FAST_PATH=1`: 18c's implementation from main `efe9aa47` (same flag; the s58 version was reverted in `78373203`). Same code and flag as bench-queue item 1 (main's J vs F A/B). Whether E also counts for that item is the owner's call: this binary carries the five s58 flags as well, all off in E |
| M | `TORUS_PARALLEL_MATCH_SETUP=1` |
| T | `TORUS_FAST_SELL_TOP_UPS=1` |
| G | `TORUS_EXEC_POOL_GRAIN=4` |
| B | `TORUS_PARALLEL_BOOK_SAVE=1` |
| ALL | all six (grain 4; E is main's implementation). **Limitation**: with `TORUS_PARALLEL_MATCH_SETUP=1` on, matching runs through `match_parallel_setup_with`, which ignores the grain (`market_workers.rs` line 238). So in ALL the grain splits settle pass A only, not matching (Open items, 1) |

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
| E (main's `efe9aa47`) | `execution pipeline: executing finalized block` at heights h % 100 == 0: `pipelined=false` about 18-19 -> 0 per node per 600 s, `pipelined=true` 0 -> about 18-19. Gap from 'block done' at h-1 to 'executing finalized block' at h: about 500-530 -> about 1 ms. Boundary block run time (C: 364-479 ms). `torus_exec_flush_seconds` count about 18-19 -> about 0 under native-only load (only serial blocks observe it). 'validator-set plan not ready' 0. Also `exec_checkpoint_upkeep_seconds`, `flush_worker_checkpoint_seconds` and commit-interval p99 (bench-queue item 1) | -1.4 to -1.7% | +1.1 to +1.7% (s58 estimate; bench-queue item 1 expects +1.2 to +2%) / about +0.5% | device writes about -1 GB per node; CPU-s/1M 0 to -0.3% |
| M | phase_match_ms per native block -9 to -11.5 ms (match ms per 1k placed 0.33 -> 0.22-0.24) | about -3% | +2 to +2.8% / about +0.9% | native blk/s +3-5%; CPU-s/1M about 0; watch hotstuff runqueue wait |
| T | margin_ms -5 to -11 ms; engine_untimed -2 to -4 ms; match_ms may rise slightly (lost warming) | about -2 to -5% | +1.5 to +3% / about +0.7 to +1.4% | CPU-s/1M -0.3 to -0.8% |
| G | phase_match_ms -3.5 to -6 ms; settle_pass_a_ms -1.5 to -2.5 ms | -1.6 to -2.2% | +1 to +2% / about +0.5-0.7% | CPU-s/1M about 0 |
| B | save_books per native block (`torus_exec_save_books_seconds` sum / count) 21.7 -> 6-9 ms (late 60 s 23-26 -> 7-10). **Engagement**: the `torus_exec_save_books_drain_seconds` and `torus_exec_save_books_write_seconds` sums rise above 0. In Classic mode app.rs observes both once per native block, and only the parallel path adds to them (drain = the parallel blob encode, write = the serial puts; `SaveSplitAccum`). The serial Classic loop records 0, so in C both sums stay 0, and in B the drain sum / count is the encode ms per native block. Blocks under the gate (2 dirty books, 16,384 resting orders) observe 0. Also: share of blocks at the 400 cap | about -4 to -5% | +1.5 to +3.5% / about +1.2 to +1.5% | native blk/s +3-5%; RSS within +-0.3 GB; CPU-s/1M 0 to -0.3% |
| ALL | sum of the above timers: about -41 to -59 ms per native block (central about -50) of about 313-323. That sum includes grain's match part (-3.5 to -6 ms), which ALL does not get while the setup path ignores the grain (Open items, 1); without it, about -37 to -54 ms | about -13 to -18% | +4 to +5.5% by the 67.4 ratio (the arms' own claims sum to about +10-12%; expect the lower figure) | CPU-s/1M -1 to -2%; block p99 not expected to move; AGREE and 0 fail-stops |

Reject an arm if AGREE fails in any cell, if any node logs a fail-stop or panic, or if consensus timeouts rise
outside C's range. Report each arm's paired delta with its 95% CI and the number of rounds with the same sign.
