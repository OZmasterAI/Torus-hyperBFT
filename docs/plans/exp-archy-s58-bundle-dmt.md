# s58 bundle D + M + T on main (exp/archy-s58-bundle-dmt)

Branch `exp/archy-s58-bundle-dmt` in worktree `/home/oz/projects/wt/exp-s58-bundle-dmt`, based on origin/main
`5e6f331a` (the brief named `19fd0c28`; main moved by three docs-only commits, `65a8dc11`, `3a8c363f`, `5e6f331a`).
Not pushed. Target dir `/home/oz/projects/.cargo-target-s58dmt`.

The owner's s58 bundle (perf doc section 78.5): D, M, T and E, with the 250 ms header re-send. E is already on main
(`TORUS_EPOCH_FAST_PATH`, `efe9aa47`, read at `crates/torus-consensus/src/exec_pipeline.rs:65`). This branch adds
D, M and T with their flags off by default (flag off = main), the consensus rate-limit drop counter, a ported
crash-replay test for E, flag-on vs flag-off state-equality tests for the bundle, and the 250 ms re-send in the
devnet / bench node env. Not merged: B (parallel-classic-book-save), G (exec-pool-fine-grain-jobs) and archy's E
(`bb62bab1`, a duplicate of main's).

## What's in the branch

| commit | what |
|---|---|
| `9c550e1e` | Merge D `exp/archy-s58-settle-passb-deferred-results-scatter` `95e2d48e` (`TORUS_SETTLE_DEFER_RESULTS=1`) |
| `bcdef6f7` | Merge M `exp/archy-s58-parallel-match-setup` `6954e735` (`TORUS_PARALLEL_MATCH_SETUP=1`) |
| `b787d4a4` | Merge T `exp/archy-s58-sell-top-up-stitch-list` `6d1100b8` (`TORUS_FAST_SELL_TOP_UPS=1`) |
| `0af69a76` | Cherry-pick of `857b9770` (`exp/archy-s58-consensus-ratelimit-drops`): metric `torus_consensus_rate_limited_dropped_total`, `crates/torus-telemetry/src/lib.rs`, `crates/torus-network/src/swarm.rs`, 2 tests |
| `6d1dbb1e` | Port of `epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` (+ `ckpt_crash_replay_with`) from `bb62bab1` onto main's E |
| `b710f976` | Bundle state-equality tests (D+M+T, D+M+T+E vs all off) |
| `87bbdd22` | `TORUS_HEADER_RESEND_MS=250` in `devnet/wsl/env.sh` and `devnet/docker-compose.yml` |
| `3c4bb127` | rustfmt of the drop counter test's two asserts (formatting only) |

Conflicts: none. All three merges and the cherry-pick were clean. D, M and T were written on main `a44000e0`; main
has since gained the parallel cancel-all pass (`7f9bb3a9`: `run_contained` made generic, the cancel-all pass in
`native_executor.rs`), exec parallel keys (`fec7ebfe`, `torus-types/src/eip712.rs` only), defer book blobs
(`495d1de8`, `torus-state` only) and 18c's epoch fast path (`efe9aa47`, `torus-consensus`). None of these touch the
code D, M or T change (settle pass B, the Phase 3 match setup, the Phase 2 stitch / sell top-ups / s92 counters).
Checked after each merge: pass B on main still only writes `results[...]` inside the market loop (no reads before
D's deferred scatter); the workspace tests compile after each merge.

Same combination as benched: the D+M+T change lines in `crates/torus-bridge/src` (`git diff 5e6f331a b787d4a4`) are
identical, line for line (2631 lines), to the D+M+T stack on `exp/archy-s58-perf-search` (`git diff a44000e0
43091381`), which ran as arms D, M, T and in ALL-G-B (sections 75-78).

## Flags

All node-local, read on the executing thread (a thread pin, else the env read once per process and logged once).

| name | values | default | took-effect log line (once per node) |
|---|---|---|---|
| `TORUS_SETTLE_DEFER_RESULTS` (D) | only the trimmed value `1` turns it on | off (unset) | `TORUS_SETTLE_DEFER_RESULTS: settle pass B deferred results write on=true` |
| `TORUS_PARALLEL_MATCH_SETUP` (M) | only the trimmed value `1` turns it on | off (unset) | `TORUS_PARALLEL_MATCH_SETUP: per-market match setup in the pool match jobs enabled=true` |
| `TORUS_FAST_SELL_TOP_UPS` (T) | only the trimmed value `1` turns it on | off (unset) | `TORUS_FAST_SELL_TOP_UPS (s58): B-blind top-ups from the stitch list enabled=true` |
| `TORUS_EPOCH_FAST_PATH` (E, main's) | `1` or `true` (any case) on; `0`, `false` or unset off; anything else warns and stays off | off | `epoch fast path ON (TORUS_EPOCH_FAST_PATH=1)` at startup (needs the exec pipeline) |
| `TORUS_HEADER_RESEND_MS` | ms; `0` = off; unparsable = 3000 | code 3000 (unchanged); devnet / bench env 250 | none |

Flag off = main:
* D off: `deferred` is an empty `Vec` (no allocation) and pass B writes `results[prep.index]` inline, as main.
* M off: `setup_items` stays an empty `Vec`; the setup runs on the exec thread through `match_setup`, which is main's
  inline block moved into a function unchanged, then main's `match_parallel_with`.
* T off: the candidate list is `None` (no allocation, `TopUpCand::record` is a no-op), main's `sell_top_ups` scan and
  per-order `record_margin_cuts` loop run. `stitch_outcome` now returns the candidate's position (one `then_some`
  per order), ignored when off.
* Extra cost when off: one thread-local read per flag per `execute_batch` call.

## Tests

### Crash-replay port (E)

`epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` (`crates/torus-consensus/src/app.rs`, crash_recovery_tests).
Main has the same context field (`ExecutionContext::epoch_fast_path`), so the port is the test plus harness plumbing
as on `bb62bab1`: `p2_run_full` (= main's `p2_run_seeded` with a `fast: Option<bool>` that sets the field on every
context; `p2_run_seeded` delegates with `None`, so no existing caller changes), `p2_run_fast`, and
`ckpt_crash_replay_with` (crash points + fast path; `ckpt_crash_replay_on` delegates with main's `CKPT_CRASHES`).
The test adds crashes at 49 (W parked on the checkpoint boundary 50 hands off) and 99 (W parked on 99's digest while
E runs boundary 100) to the six standard crashes; the crashed run has the flag on, the uncrashed reference off.
Per-block write sets, running hash, CF dump equal; one guard rebuild per crash. It passes. It is meaningful on
main's version: with the crashed run flipped to flag off it fails `crash at 49: E blocked` (the boundary's barrier
waits for the parked W).

### State equality: existing (per flag, on the branches)

| flag | test (crates/torus-bridge) | compares |
|---|---|---|
| D | `tests/settle_defer_results_tests.rs` (4 tests: 3 fuzz seeds x caps 1/2/8 x record_fills, flag flip between calls) + 5 unit tests in `native_executor.rs` | every `NativeActionResult` field, gas, native CFs, fills and extras, counters, state root; pinned off == pinned on == env |
| M | `tests/parallel_match_setup_tests.rs` (2 tests, 4 batch paths, repeated) + `market_workers.rs` `setup_in_jobs_tests` + flag tests | results, gas, CFs, fills, trade index, next order id, state root |
| T | `tests/sell_top_up_stitch_diff_tests.rs` (2 tests, 3 modes) + `src/sell_top_up_stitch_tests.rs` (unit differential incl. faults) | CFs, results, gas, trade index, next order id, state root, top-up and margin-cut counters |
| E | main's `epoch_fast_path_boundaries_pipelined_and_match_serial`, `..._evm_or_slash_boundary_stays_serial`, `..._plan_durable_before_next_boundary` + the port above | write sets, running hash, native root, every CF, durable markers |

### State equality: added (the bundle)

* `s58_bundle_dmt_identical_to_flags_off` in each of the three bridge files: D, M and T pinned on together vs off
  together, on that file's scenarios (each built for its own flag's inputs), every cap / path / mode, full
  fingerprint.
* `s58_bundle_dmt_and_epoch_fast_path_state_equals_flags_off` (torus-consensus): the 130 P2 blocks (boundaries 50 /
  100), pipelined, checkpoint every 5 blocks. D+M+T (E off) and D+M+T+E each equal all off per block (consensus write
  sets), in the running hash, every CF and the persisted native root. Non-vacuous: T granted top-ups (15 full); E
  pipelined 2 more blocks than off (`torus_exec_checkpoint_upkeep_seconds_count`).

How they were checked to bite (temporary mutations, each reverted; `crates/torus-bridge/src` diff empty after):

| mutation | 3 bridge tests | consensus P2 test |
|---|---|---|
| D: the deferred scatter writes `placed_reject(None)` (a success) for every placed order | fail | passes: the P2 blocks put no placed book reject through the parallel pass B |
| M: the in-job setup reverses each market's requests | fail | fail |
| T: a fast-path grant debits the balance twice | fail | fail |
| E (port): crashed run flag off | | `crash at 49: E blocked` |

## Test results

See "Build and test results" at the end: workspace 3922 / 3922 flags off and 3922 / 3922 with D, M, T and E on.

## Header re-send 250 ms (config, not code)

The compiled default stays 3 s (`crates/hotstuff_rs/src/hotstuff/implementation.rs:121`, parse at :3643).
* `devnet/wsl/env.sh`: `export TORUS_HEADER_RESEND_MS="${TORUS_HEADER_RESEND_MS:-250}"`. env.sh is the node runtime
  env of the bare 3-validator devnet: `launch-3val.sh` sources it, and `tools/matched-bench/run-cell.sh` calls
  `launch-3val.sh` after exporting its record env and EXTRA_ENV, so every matched-bench cell gets 250 ms unless the
  arm sets its own value (`TORUS_HEADER_RESEND_MS=3000` = the node default, `0` = off). For an arm that must have
  the name absent (`absent_env`), set the harness knob `HEADER_RESEND_UNSET=1` (campaign `harness_env`, campaign-wide;
  env.sh then exports nothing).
* `devnet/docker-compose.yml`: `TORUS_HEADER_RESEND_MS=${TORUS_HEADER_RESEND_MS:-250}` on all 5 validators.
* Not touched (owner to decide): the testnet node launch configs `testnet/seed-relaunch-cmd-s448.txt`,
  `testnet/val2-launch-s448.md`, `testnet/val3-launch-s448.md`, `testnet/cap-probe/set-cap.sh`,
  `testnet/p3-testnet/measure-leg.sh`, `testnet/p3-testnet/devnet-ladder/{run-leg.sh,compose.rp5.yml}`; also
  `devnet/docker-compose.{bake,legA,legB}.yml` (special-purpose legs) and the per-experiment `devnet/*-s4*.sh`
  scripts.
* Caveat: a campaign whose harness worktree (`WT_H`) is not this branch uses its own `devnet/wsl/env.sh`; then
  set `TORUS_HEADER_RESEND_MS=250` in `node_env` instead.

## Running it as campaign A arms (docs/perf/bench-queue.md)

One binary (this branch's release build, md5 below) for every arm; arms differ only in env on all 3 validators.
Shape as campaign A (hsdb-300m: Classic, 15 s checkpoints, oracle on; NOT the harness default `TORUS_BOOK_ROWS=3`).

| arm | env |
|---|---|
| base | D, M, T, E flags absent (`absent_env`: `TORUS_SETTLE_DEFER_RESULTS TORUS_PARALLEL_MATCH_SETUP TORUS_FAST_SELL_TOP_UPS TORUS_EPOCH_FAST_PATH`) |
| DMT | `TORUS_SETTLE_DEFER_RESULTS=1 TORUS_PARALLEL_MATCH_SETUP=1 TORUS_FAST_SELL_TOP_UPS=1` (E absent) |
| DMTE | `TORUS_SETTLE_DEFER_RESULTS=1 TORUS_PARALLEL_MATCH_SETUP=1 TORUS_FAST_SELL_TOP_UPS=1 TORUS_EPOCH_FAST_PATH=1` |

* Every arm gets `TORUS_HEADER_RESEND_MS=250` from env.sh (as in section 78, where every arm ran it). To measure the
  re-send itself, add an arm with `TORUS_HEADER_RESEND_MS=3000`, or run with `HEADER_RESEND_UNSET=1` and list it in
  `absent_env`.
* Took-effect checks: the log lines in the flags table, 1 per node in the arms that set the flag, 0 elsewhere; for E
  also `executing finalized block height=..00 ... pipelined=true` (bench-queue Q1).
* Watch: `torus_consensus_rate_limited_dropped_total` per node (new counter), consensus timeouts by kind, AGREE.

## References

`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` on branch `docs/s58flags` (`9bb98b10`):
* 75: s58 pilot, all five s58 flags plus E (ALL) vs C.
* 76: single-flag screen (D, M, T, G, B alone), 76.5 E alone, 76.6 leave one out.
* 77: ALL's extra timeouts: proposal headers lost at the consensus gossip rate limiter; the drop counter; the
  direct fan and 250 ms re-send fixes.
* 78: bundle pick at 600 s (C vs ALL-G vs ALL-G-B, all with the 250 ms re-send); 78.5 the owner's decision
  (D, M, T, E + 250 ms); 78.6 parked options (B, G, direct fan, limiter exemption, E's duplicate); 78.7 next steps.
Per-flag plans: `docs/plans/exp-archy-s58-settle-passb-deferred-results-scatter.md`,
`docs/plans/exp-archy-s58-parallel-match-setup.md`, `docs/plans/exp-archy-s58-sell-top-up-stitch-list.md`.

## Open questions

* 78.7 asks to make 250 ms "the default" and to decide where (code default vs genesis / config). This branch only
  sets it in the devnet / bench env; the code default and testnet configs are unchanged.
* `HEADER_RESEND_UNSET=1` is campaign-wide (harness env), not per arm. A per-arm absent value would need a harness
  change.
* Codex review of the branch (78.7) not done here.

## Build and test results

All on this branch, target `/home/oz/projects/.cargo-target-s58dmt`, ozarchy, 2026-10-16.

| check | result |
|---|---|
| `cargo nextest run --workspace`, flags unset (final head `3c4bb127`) | 3922 / 3922 passed, 38 skipped, 0 flaky |
| same, `TORUS_SETTLE_DEFER_RESULTS=1 TORUS_PARALLEL_MATCH_SETUP=1 TORUS_FAST_SELL_TOP_UPS=1 TORUS_EPOCH_FAST_PATH=1` (final head) | 3922 / 3922 passed, 38 skipped, 0 flaky |
| earlier full runs (head `87bbdd22`, same code but for formatting) | flags unset 3921 / 3922; flags on 3921 / 3922 twice. Failures: `checkpoint_crash_replay_with_cloids_matches_uncrashed_{order_rows,classic}` "crash at N latched" (pre-existing race, below; 5 / 5 in isolation) and one SIGKILL at 0.08 s in `torus-economics` `submit_proposal_sufficient_stake` (external kill, passes alone, unrelated to these flags) |
| flag-sensitive pinned tests with the four flags on | none. D, M, T tests pin their flag per thread; main's test contexts set `epoch_fast_path: false` explicitly, so `TORUS_EPOCH_FAST_PATH=1` in the env reaches no test context (E is covered by the explicit tests) |
| `cargo test --doc` | 1 passed, 0 failed |
| `cargo clippy --workspace --all-targets --locked` | 345 warnings at 259 sites, identical by file and message to main `5e6f331a` (345 / 259, separate target dir); 0 on lines this branch adds |
| `rustfmt --check` (edition 2021) on the 9 touched .rs files | no diff in lines the branch adds, except 3 lines of M's `match_setup` that are main's unformatted inline block moved verbatim (main's file has the same lines; the file's diff count is 556 vs main 557). The 4 new test files are clean. Main itself is not rustfmt-clean (e.g. `app.rs` 401 diffs on main and here) |

Pre-existing race behind "crash at N latched" (main `exec_pipeline.rs:754-755`, `:762-763`): on a write failure or
panic the flush worker stores `shared.failed` before `env.exec_failed`, and `FlushWorker::wait_idle` returns as soon
as `failed()` is true, so under full-suite load the test can read `exec_failed == false` between the two stores.
Seen on the M branch before (memory, 2026-10-15). Not touched here: candidate fix is to store `exec_failed` first.

Release build (same recipe as `~/bench-results-matched/ozarchy-s58rl-stage/build/build.sh`: `cargo build --release
--locked -p torus-node`, default features (jemalloc), `RUSTFLAGS="-C link-arg=-fuse-ld=mold -C
force-frame-pointers=yes"`, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`):
* head `3c4bb127` (code-final; later commits are docs only), rustc 1.98.1, fresh release dir (537 crates compiled).
* `/home/oz/projects/.cargo-target-s58dmt/release/torus-node`: **md5 `4574bcbf26a40459e0d6c993cd005889`**,
  588,015,960 bytes (s58rl's same-recipe binary: 587,521,600), linked by mold 2.42.0.
* Not staged for a bench. The bench binary (`bench-throughput`) is not rebuilt; s58rl / s58b600 used `ff822b95`
  (md5 `9b725823487b8ac7f42cfc556609e9aa`).
