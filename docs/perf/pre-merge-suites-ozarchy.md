# Pre-merge suites on ozarchy

Owner rule (18c s99, 2026-10-07): the pre-merge suites run on ozarchy. Updated the same day: **each
machine runs the suites for what it builds or merges** (18c for 18c's branches, ozarchy for ozarchy's);
big builds and all benches stay on ozarchy. Each ozarchy run is added here, newest first.

The suites:
1. `cargo nextest run --workspace --cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar` (TESTING.md)
2. `cargo test --workspace --doc -q`
3. `cargo test --workspace --no-fail-fast -q`
4. `ci/run-local.sh check`. Clippy and fmt carry old debt, so only findings that are new against the previous main are reported.
5. `ci/run-local.sh uniswap` (devnet end to end, fresh genesis). It runs on 18c, which has Foundry;
   Foundry on ozarchy is optional (owner s99).
6. `tools/matched-bench` Python tests (`python3 -I -m pytest tools/matched-bench`, plus each `test_*.py` as a script as the README runs them)

Host: ozarchy (Ryzen 9 5950X, 32 threads, 62 GB). One cargo build at a time; each worktree has its own
`CARGO_TARGET_DIR`.

## main `b8e3b606` (2026-10-08, s104 merges)

The s104 merges on top of the owner's local commits, never tested together before this run: `91f51ca0`
(bench-launcher agent) and `4a641d15` (`detach.sh` secrets, `test_harness.py`), the merge of main
`bf2edda6` (`93af4402`), then `perf/c2-set-holder` (`c8d25db8`) and `perf/position-v2-savings`
(`b8e3b606`). Both branches: 18c review "merge as is". Run in the main checkout,
`CARGO_TARGET_DIR=~/.cargo-target-c2-set-holder`; suite 4 baseline `bf2edda6` in a detached worktree
(the differing `.rs` files touched; removed afterwards).

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,071 passed, 0 failed**, 33 skipped, 0 flaky | 124 s (82.5 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 3 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,072 passed, 0 failed**, 40 ignored (160 result lines) | 403 s |
| 4 | clippy (no `-D`) / fmt vs `bf2edda6` | – | clippy 268 = 268, **0 new**; fmt 3,366 vs 3,354, **12 new** (below) | 12 s / 11 s |
| 5 | uniswap | – | 18c (no Foundry on ozarchy) | – |
| 6 | matched-bench | 0 | **165 passed**, 18 subtests; 8 of 8 scripts OK (`test_harness.py` 107 tests) | 30 s |

Suite 4 fmt: all 12 new hunks are in lines `perf/position-v2-savings` added:
`torus-core/tests/position_cache_tests.rs` +10 (the new tests at lines 405-549),
`torus-core/src/position.rs` +1 (the new `PositionCache` code at 668/682), and
`torus-core/tests/ubench_position_cache.rs` +1 (new file, line 70). Fixed in `67a93a93` (rustfmt of
only the lines that branch added, no logic change): afterwards no fmt hunk falls in a branch-added line,
the workspace fmt count is 3,352 (the 2 below the base are old hunks inside code the branch rewrote),
and `cargo nextest -p torus-core -p torus-bridge` passes 1,076 / 0 (20 skipped). The base fmt log
shows 3,355 because `touch` created an empty `ubench_position_cache.rs` in the base worktree (1 hunk);
the clean `bf2edda6` count is 3,354 (as in the `d7bd1c36` run below). The `test_harness.py`
`ResourceWarning` (unclosed socket) lines are the same as in earlier runs; the script exits 0.

Logs: `~/bench-results-matched/presuite-b8e3b606/`.

## `perf/c2-set-holder` `d7bd1c36` (2026-10-08)

C2 holder index moved only when a position key appears or disappears, foldhash market map (18c s104
request, results doc section 27). Base main `bf2edda6`. Worktree `wt/c2-set-holder`,
`CARGO_TARGET_DIR=~/.cargo-target-c2-set-holder`; suite 4 baseline `bf2edda6` in a detached worktree
`wt/c2-set-holder-base` (same target dir, the 2 differing `.rs` files touched: base clippy re-checked
torus-bridge and its 5 dependents). Suite 4 ran as `cargo clippy --workspace --all-targets` and
`cargo fmt --all --check` compared with `norm.py`, not through `run-local.sh check`.

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,068 passed, 0 failed**, 31 skipped, 0 flaky | 200 s (82.2 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 3 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,069 passed, 0 failed**, 38 ignored (159 result lines) | 406 s |
| 4 | clippy (no `-D`) / fmt vs `bf2edda6` | – | clippy 268 = 268, **0 new**; fmt 3,354 = 3,354, **0 new** | 7 s / – |
| 5 | uniswap | – | 18c (no Foundry on ozarchy) | – |
| 6 | matched-bench | – | not run (no `tools/` change) | – |

Logs: `~/bench-results-matched/presuite-c2-set-holder/`.

## `fix/read-gas-followup` `105a6e28` (2026-10-08, s101)

`b26bd3b3` plus the review fixes (docs, comments, `lock_db` in `request_locked`, a chmod drop guard in
the error test, a deferred-first market and strict key tags in the layout test). Same worktree and
target dir; suite 4 baseline main `1914609f` in a detached worktree created after the candidate's
clippy, with the 6 differing `.rs` files touched (base clippy checked 21 crates, candidate 14).

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,057 passed, 0 failed**, 35 skipped, 0 flaky | 110 s (83.2 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 6 s |
| 3 | `cargo test --workspace --no-fail-fast` | 101, rerun 0 | first run 3,057 passed, **1 failed** (flaky, below); rerun **3,058 passed, 0 failed**, 42 ignored (159 result lines) | 409 s / ~400 s |
| 4 | clippy (no `-D`) / fmt vs `1914609f` | – | clippy 270 = 270, **0 new**; fmt 3,349 = 3,349, **0 new** | 19 s / 18 s |
| 5 | uniswap | – | 18c (no Foundry on ozarchy) | – |
| 6 | matched-bench | 0 | **164 passed**; 8 of 8 scripts OK | 29 s |

Suite 3 flake: `torus-bridge` `liquidation_tests::p2_counterparties_are_paid_at_the_stored_price`
("one close per row", 6 vs 8). Neither the test nor the liquidation code changed on this branch; it
passed in nextest, in 8 of 8 runs of its binary alone and in the full rerun. Likely cause: its
`Captured` subscriber is thread-local (`with_default`, no `register_callsite`), and tracing caches
callsite interest process-wide, so parallel tests in one binary can make it miss events. Not fixed
here. Logs: `~/bench-results-matched/presuite-105a6e28/` (`3-fulltest.log`, `3-fulltest-rerun.log`).

## `fix/read-gas-followup` `b26bd3b3` (2026-10-08, s101)

Read-precompile gas follow-up on main `1914609f` with `bench/read-gas-stall` (`8e3326c6`) merged: book CF
SST target 4 MiB by default, range-compaction error accounting, last-drop cancel only on the last DB
reference, layout pin test, review tests, s100 decisions. Worktree `wt/read-gas-followup`,
`CARGO_TARGET_DIR=~/.cargo-target-read-gas`; suite 4 baseline main `1914609f` (detached worktree, same
target dir).

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,057 passed, 0 failed**, 35 skipped, 0 flaky | 131 s (83.7 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 5 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,058 passed, 0 failed**, 42 ignored (159 result lines) | 400 s |
| 4 | clippy (no `-D`) / fmt vs `1914609f` | – | clippy 270 = 270, **0 new**; fmt 3,349 = 3,349, **0 new** | 18 s / – |
| 5 | uniswap | – | 18c (no Foundry on ozarchy) | – |
| 6 | matched-bench | 0 | **164 passed**; 8 of 8 scripts OK | 29 s |

Suite 4 note: the two worktrees share one target dir, and cargo keys workspace units by path relative
to the workspace, so the first base clippy replayed the candidate's cached results (1 s, nothing
checked). Touching the 6 differing `.rs` files in the base worktree forced a real check (14 crates,
17 s). Logs: `~/bench-results-matched/presuite-b26bd3b3/`.

## `bench/read-precompile-gas` `ae767806` (2026-10-07, s26)

`fc1fb25a` (s99 final read decisions + review fixes) merged with main `f1e41975`. Worktree `wt/read-gas-bench`,
`CARGO_TARGET_DIR=~/.cargo-target-read-gas`; suite 4 baseline main `f1e41975`. 18c review: merge as is.

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,028 passed, 0 failed**, 35 skipped, 0 flaky | 100 s (81.7 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 4 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,029 passed, 0 failed**, 42 ignored (159 result lines) | 409 s |
| 4 | clippy (no `-D`) / fmt vs `f1e41975` | – | clippy 271 = 271, **0 new**; fmt **0 new**, 21 fewer hunks (3,361 vs 3,382); `run-local.sh check` fails only at the known `-D warnings` stop | 16 s / – / 27 s |
| 5 | uniswap | – | 18c | – |
| 6 | matched-bench | 0 | **164 passed**; 8 of 8 scripts OK | 30 s |

Logs: `~/bench-results-matched/presuite-ae767806/`.

## `fix/evm-vote-checks` `4164382d` (2026-10-07, s26; 18c's branch, run before the rule update)

Item 7 step 0 (vote-time EVM header checks) on main `2174d3bf`. Merged as `7c15b5aa`.

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,017 passed, 0 failed**, 34 skipped, 0 flaky | 106 s (82 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 4 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,018 passed, 0 failed**, 41 ignored | 403 s |
| 4 | clippy (no `-D`) / fmt vs `2174d3bf` | – | clippy 271 = 271, fmt 3,382 = 3,382: **0 new** | 130 s / – |
| 5 | uniswap | – | 18c | – |
| 6 | matched-bench | 0 | **164 passed**; 8 of 8 scripts OK | 59 s |

The known flaky `fill_sink_not_wanted_records_nothing_with_history_off` did not fire. Logs:
`~/bench-results-matched/presuite-4164382d/`.

## `bench/read-precompile-gas` `76eb081c` (2026-10-07, s26)

The first a-c build, before the review fixes. Baseline main `2174d3bf`.

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,022 passed, 0 failed**, 35 skipped, 0 flaky | 84 s |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 4 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,023 passed, 0 failed**, 42 ignored | 400 s |
| 4 | clippy (no `-D`) / fmt vs `2174d3bf` | – | clippy 271 = 271, **0 new**; fmt **0 new**, 17 fewer hunks | 12 s / – |
| 5 | uniswap | – | not run | – |
| 6 | matched-bench | 0 | **164 passed** | 29 s |

Logs: `~/bench-results-matched/presuite-76eb081c/`.

## main `b8bf3e8a` (2026-10-07, s26)

Two merges since `98f035ee`, never tested together before this run:
* `perf/adl-dirty-check` `cdfe3a64` (merge `87a05374`)
* `fix/governance-params` `a33ca502`

Worktree `wt/main-b8bf3e8a`, `CARGO_TARGET_DIR=~/.cargo-target-main`. The baseline for suite 4 is
`98f035ee` in its own worktree.

| # | suite | exit | totals | wall |
|---|---|---|---|---|
| 1 | nextest `--workspace` | 0 | **3,013 passed, 0 failed**, 34 skipped, 0 flaky | 209 s (82 s of tests) |
| 2 | doc tests | 0 | **1 passed, 0 failed**, 7 ignored | 4 s |
| 3 | `cargo test --workspace --no-fail-fast` | 0 | **3,014 passed, 0 failed**, 41 ignored (157 result lines) | 401 s |
| 4 | `ci/run-local.sh check` | 1 | the same failures as `98f035ee` (old debt), plus the new findings below | 121 s (base 114 s) |
| 5 | `ci/run-local.sh uniswap` | 0 (on 18c) | **PASS on 18c** at `7547f2b1` (= `b8bf3e8a` + docs): 3 pairs deployed, D6 swap check passed, Multicall3 live. On ozarchy: not run (`missing: forge`) | 132 s (18c) |
| 6 | matched-bench Python tests | 0 | **164 passed**, 18 subtests; each script exits 0 | ~40 s |

### Suite 4: check / clippy / fmt / test

* `check`, `clippy` and `test` fail on both trees. `-D warnings` stops the build at two dead-code
  errors in `torus-network/src/swarm.rs` (`enqueue_body_fetch_traced`, `handle_consensus_direct`), so
  the script's own `-D warnings` test step never compiles. Suites 1 and 3 are the test results.
* Clippy without `-D warnings` (workspace, so every crate is linted): 47 findings on `98f035ee`, 48 on
  `b8bf3e8a`. **New:** `clippy::type_complexity` at `crates/torus-bridge/src/trader_positions.rs:419`,
  the return type of `dirty_by_market_and_traders` (adl-dirty-check).
* `cargo fmt --check`: 3,383 hunks on `98f035ee`, 3,392 on `b8bf3e8a`. **About 9 new**, all in files the
  merges touched: 8 from adl-dirty-check (`liquidation_l1_tests.rs` +3, `liquidation_step.rs` +4,
  `trader_positions.rs` +1) and 1 from governance-params (`economics/tests/governance_tests.rs` +1).
  Those files already had fmt debt, so a whole-file `cargo fmt` would also reformat old code.
* **Fixed** on `fix/adl-dirty-check-lint`: the `ByMarket` alias clears `type_complexity`, and rustfmt
  was applied only to the lines the dirty-check and governance commits introduced (`55da48a8`, merged
  as `6af5e28c`; the governance hunks are in `474bb1ac`). The old fmt debt is unchanged.

### Suite 5: uniswap

On ozarchy `forge` / `cast` are missing, so the script exits before building or starting a node. Ports
18545 and 30933 were free, and the empty temp dir it left was removed. 18c ran it at `7547f2b1`: PASS
in 132 s (3 pairs deployed, D6 swap check passed, Multicall3 live).

### Failures

None in suites 1, 2, 3 and 6.

Logs: `~/bench-results-matched/presuite-b8bf3e8a/` on ozarchy (`1-nextest.log`, `2-doc.log`,
`3-cargotest.log`, `4-{check,clippy-nodeny,fmt}-{new,base}.log`, `5-uniswap.log`, `6-pytest.log`,
`6-test_*.log`).
