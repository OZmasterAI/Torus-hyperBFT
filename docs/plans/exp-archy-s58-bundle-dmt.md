# s58 bundle D + M + T on main (exp/archy-s58-bundle-dmt)

Branch `exp/archy-s58-bundle-dmt` in worktree `/home/oz/projects/wt/exp-s58-bundle-dmt`, based on origin/main
`5e6f331a` (the brief named `19fd0c28`; main moved by three docs-only commits, `65a8dc11`, `3a8c363f`, `5e6f331a`).
Not pushed. Target dir `/home/oz/projects/.cargo-target-s58dmt`.

The owner's s58 bundle (perf doc section 78.5): D, M, T and E, with the 250 ms header re-send. E is already on main
(`TORUS_EPOCH_FAST_PATH`, `efe9aa47`, read at `crates/torus-consensus/src/exec_pipeline.rs:65`). This branch adds
D, M and T with their flags ON by default since the s62 owner decision ("Default on" below; `=0` turns each
off, flag off = main), the consensus rate-limit drop counter, a ported
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
| `26f45c81` | D, M, T on by default; `=0` turns each off ("Default on" below) |
| (this commit) | D / M / T parse ignores case; empty = unset (on, no warning) ("Default on" below) |

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
D, M and T share one parse (`on_by_default_value` / `on_by_default_flag`, `crates/torus-bridge/src/native_executor.rs`):
values are trimmed and matched in any case (`TRUE`, `False` are valid); an empty value (`VAR=`, whitespace only) is
the same as unset: on, no warning (drivers blank a knob with `VAR=` to mean "not set"; unlike E, where `""` is off).
`on`, `off`, `yes`, `2`, ... are invalid.

| name | values | default | took-effect log line (once per node) |
|---|---|---|---|
| `TORUS_SETTLE_DEFER_RESULTS` (D) | unset, empty, `1` or `true` (any case) on; `0` or `false` (any case) off; anything else warns once and stays on | **on** (unset) | `TORUS_SETTLE_DEFER_RESULTS: settle pass B deferred results write on=true` (`on=false` at `0`) |
| `TORUS_PARALLEL_MATCH_SETUP` (M) | unset, empty, `1` or `true` (any case) on; `0` or `false` (any case) off; anything else warns once and stays on | **on** (unset) | `TORUS_PARALLEL_MATCH_SETUP: per-market match setup in the pool match jobs enabled=true` (`enabled=false` at `0`) |
| `TORUS_FAST_SELL_TOP_UPS` (T) | unset, empty, `1` or `true` (any case) on; `0` or `false` (any case) off; anything else warns once and stays on | **on** (unset) | `TORUS_FAST_SELL_TOP_UPS (s58): B-blind top-ups from the stitch list enabled=true` (`enabled=false` at `0`) |
| `TORUS_EPOCH_FAST_PATH` (E, main's) | `1` or `true` (any case) on; `0`, `false` or unset off; anything else warns and stays off | off | `epoch fast path ON (TORUS_EPOCH_FAST_PATH=1)` at startup (needs the exec pipeline) |
| `TORUS_HEADER_RESEND_MS` | ms; `0` = off; unparsable = 3000 | code 3000 (unchanged); devnet / bench env 250 | none |

Flag off (`=0`) = main:
* D off: `deferred` is an empty `Vec` (no allocation) and pass B writes `results[prep.index]` inline, as main.
* M off: `setup_items` stays an empty `Vec`; the setup runs on the exec thread through `match_setup`, which is main's
  inline block moved into a function unchanged, then main's `match_parallel_with`.
* T off: the candidate list is `None` (no allocation, `TopUpCand::record` is a no-op), main's `sell_top_ups` scan and
  per-order `record_margin_cuts` loop run. `stitch_outcome` now returns the candidate's position (one `then_some`
  per order), ignored when off.
* Extra cost when off: one thread-local read per flag per `execute_batch` call.

### Default on (s62 owner decision, `26f45c81`)

D, M and T were benched at +8-9% matched/s at neutral CPU (sections 76-78, check campaign ozarchy-s58dmt), so they
are on by default; benches that need the old baseline set each to `0`.
* Parse (`crates/torus-bridge/src/native_executor.rs`): `on_by_default_value` (trim, then ASCII-lowercase as E's
  `parse_epoch_fast_path` does; unset, `""`, `1`, `true` = on; `0`, `false` = off; `None` = invalid) and
  `on_by_default_flag` (an invalid value: `tracing::warn!` "`<NAME>`: not empty, 1, true, 0 or false (any case);
  using the default (on)", then on). Case and empty handling owner-approved after `26f45c81` (that commit was
  case-sensitive and warned on `""`). The env is read once per process (OnceLock), so the warning is logged once. The per-flag parses
  `parse_settle_defer_results`, `parse_parallel_match_setup`, `parse_fast_sell_top_ups` take `Option<&str>` and are
  `pub`, `#[doc(hidden)]` (the tests below use them). Pins (`with_*`) and the info lines are unchanged.
* E (`TORUS_EPOCH_FAST_PATH`) is unchanged: default off.
* Tests: `on_by_default_value_classifies`, `parse_on_by_default_zero_or_false_turns_off` (D and M, in
  `native_executor.rs`), `fast_sell_top_ups_toggle_parse` (T, `sell_top_up_stitch_tests.rs`): unset, `1`, `true`
  (trimmed, any case: `TRUE`, `True`, `tRuE`, `" TRUE "`) and empty / whitespace only (`""`, `" "`, `"   "`, `"\t"`,
  ...; classified valid, so no warning) on; `0`, `false` (`FALSE`, `False`, `fAlSe`, `" FALSE "`, `" false "`) off;
  15 invalid values (`2`, `01`, `on`, `off`, `yes`, `f`, `t`, `truee`, `1 0`, ...) on. The case / empty extension
  failed first: all 4 parse tests failed on the old parse (`"FALSE"` read as on; `""` classified invalid). The four
  bundle state-equality tests now pin each flag to its parse of the unset env (on) vs its parse of `0` (off), with a
  guard that those are `[true; 3]` and `[false; 3]`. All 8 failed against the old parse (kept behind the new
  signature) before the change: the parse tests on their assertions, the 4 bundle tests on the guard.
* Bite (temporary mutations, reverted; no mutation text left in the diff): T's fast grant debits the balance twice:
  all 4 bundle tests fail (consensus: "D+M+T: write set of height 26 differs"). D's deferred scatter writes
  `placed_reject(None)`: the 3 bridge tests fail, the consensus test passes (as before: the P2 blocks put no placed
  book reject through the parallel pass B).
* Nothing else in the workspace depended on the old default: every flag-sensitive test pins its flag.

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
| base | `TORUS_SETTLE_DEFER_RESULTS=0 TORUS_PARALLEL_MATCH_SETUP=0 TORUS_FAST_SELL_TOP_UPS=0`, E absent (`absent_env`: `TORUS_EPOCH_FAST_PATH`) |
| DMT | `TORUS_SETTLE_DEFER_RESULTS=1 TORUS_PARALLEL_MATCH_SETUP=1 TORUS_FAST_SELL_TOP_UPS=1` (E absent) |
| DMTE | `TORUS_SETTLE_DEFER_RESULTS=1 TORUS_PARALLEL_MATCH_SETUP=1 TORUS_FAST_SELL_TOP_UPS=1 TORUS_EPOCH_FAST_PATH=1` |

* **D, M and T are on by default** (since the "Default on" commit): `absent_env` no longer means off for them. The
  base arm (and any other old-baseline arm or bench) must set all three to `0`. Listing them in `absent_env` gives
  the DMT configuration. The `=1` in the DMT / DMTE arms is now the default, kept explicit so the arms read the same
  on a binary built before the change.

* Every arm gets `TORUS_HEADER_RESEND_MS=250` from env.sh (as in section 78, where every arm ran it). To measure the
  re-send itself, add an arm with `TORUS_HEADER_RESEND_MS=3000`, or run with `HEADER_RESEND_UNSET=1` and list it in
  `absent_env`.
* Took-effect checks: the D / M / T log lines in the flags table, 1 per node in every arm (`on=true` /
  `enabled=true` in DMT and DMTE, `false` in base); no `not empty, 1, true, 0 or false` warning; for E
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

* Resolved (owner): D / M / T ignore case like E; an empty value is unset (on, no warning), not E's off.
* Default on: `tools/matched-bench` has no D / M / T specific defaults (`run-cell.sh`, `make-driver.py`,
  `README.md`: `absent_env` is a generic "unset" list). Any campaign spec or doc that lists D / M / T in `absent_env`
  for a flags-off baseline now gets them on; for this binary use `=0`. `docs/perf/bench-queue.md` campaign A
  ("base: all flags absent") does not name D / M / T, but on a binary with this bundle its base arm has them on.

* 78.7 asks to make 250 ms "the default" and to decide where (code default vs genesis / config). This branch only
  sets it in the devnet / bench env; the code default and testnet configs are unchanged.
* `HEADER_RESEND_UNSET=1` is campaign-wide (harness env), not per arm. A per-arm absent value would need a harness
  change.
* Codex review of the branch (78.7) not done here.

## Build and test results

All on this branch, target `/home/oz/projects/.cargo-target-s58dmt`, ozarchy, 2026-10-16.

Case-insensitive parse, empty = unset (the commit after `23df02d2`):

| check | result |
|---|---|
| parse tests before the change (new values in `ON` / `OFF`, `""` / whitespace classified valid) | 4 / 4 failed (`"FALSE"` read as on; `""` classified invalid) |
| after: the 4 parse tests + the 4 bundle state-equality tests | 8 / 8 passed |
| `cargo nextest run --workspace`, D, M, T unset (= on) | 3923 / 3923 passed, 38 skipped, 0 flaky |
| `cargo test --doc` | 1 passed, 0 failed |
| `cargo clippy --workspace --all-targets --locked` | warning set identical to `23df02d2`'s sources (A/B in the same target dir, normalized diff: only cargo's "N duplicates" attribution in unrelated test targets differs); none in the touched lines |
| `rustfmt --check` (stdin, edition 2021) on the 2 touched .rs files | `native_executor.rs` 169 hunks (= `23df02d2`), `sell_top_up_stitch_tests.rs` 0; none in the changed lines |
| release `torus-node` (recipe below, incremental on the `26f45c81` release dir) | **md5 `325b37f961526364ff614f6e3b76fcd7`**, 588,020,960 bytes, rustc 1.98.1 |

Default on (`26f45c81`, D, M, T on when unset):

| check | result |
|---|---|
| `cargo nextest run --workspace`, D, M, T unset (= on), final code | 3923 / 3923 passed, 38 skipped, 0 flaky (+1 test: `on_by_default_value_classifies`) |
| same, `TORUS_SETTLE_DEFER_RESULTS=0 TORUS_PARALLEL_MATCH_SETUP=0 TORUS_FAST_SELL_TOP_UPS=0` | 3923 / 3923 passed, 38 skipped, 0 flaky (run before 3 doc-comment-only edits) |
| an earlier unset run (same code) | 1 fail, the known `checkpoint_crash_replay_with_cloids_matches_uncrashed_classic` "crash at 23 latched" (race below); 3 / 3 passed singly; the rerun of the whole suite passed (row 1) |
| `cargo test --doc` | 1 passed, 0 failed |
| `cargo clippy --workspace --all-targets --locked` | 345 warning lines (same count as `3c4bb127`), 266 file:line sites; 0 on lines `26f45c81` adds |
| `rustfmt --check` on the touched .rs files | hunk counts unchanged (`native_executor.rs` 169, `app.rs` 401, the 4 test files 0); none in the changed lines |
| release `torus-node` (recipe below, incremental on the `3c4bb127` release dir: torus-bridge, torus-consensus, torus-node rebuilt) | **md5 `6fbbb1a81f085357703f21b508593fc8`**, 588,018,976 bytes, head `26f45c81`, rustc 1.98.1 |

Before default on (`3c4bb127`, flags off when unset):

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
* Case-insensitive parse, empty = unset: **md5 `325b37f961526364ff614f6e3b76fcd7`** (the current
  `/home/oz/projects/.cargo-target-s58dmt/release/torus-node`; table above).
* Default on: head `26f45c81`, **md5 `6fbbb1a81f085357703f21b508593fc8`** (overwritten by the build above).
* Before default on: head `3c4bb127` (later commits docs only), rustc 1.98.1, fresh release dir (537 crates compiled).
* `/home/oz/projects/.cargo-target-s58dmt/release/torus-node` at `3c4bb127` (overwritten by the default-on build): **md5 `4574bcbf26a40459e0d6c993cd005889`**,
  588,015,960 bytes (s58rl's same-recipe binary: 587,521,600), linked by mold 2.42.0.
* Not staged for a bench. The bench binary (`bench-throughput`) is not rebuilt; s58rl / s58b600 used `ff822b95`
  (md5 `9b725823487b8ac7f42cfc556609e9aa`).
