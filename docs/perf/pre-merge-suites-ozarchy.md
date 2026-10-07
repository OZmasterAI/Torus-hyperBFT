# Pre-merge suites on ozarchy

Owner rule (18c s99, 2026-10-07): from now on the pre-merge suites run on ozarchy. Each run is added
here, newest first.

The suites:
1. `cargo nextest run --workspace --cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar` (TESTING.md)
2. `cargo test --workspace --doc -q`
3. `cargo test --workspace --no-fail-fast -q`
4. `ci/run-local.sh check`. Clippy and fmt carry old debt, so only findings that are new against the previous main are reported.
5. `ci/run-local.sh uniswap` (devnet end to end, fresh genesis)
6. `tools/matched-bench` Python tests (`python3 -I -m pytest tools/matched-bench`, plus each `test_*.py` as a script as the README runs them)

Host: ozarchy (Ryzen 9 5950X, 32 threads, 62 GB). One cargo build at a time; each worktree has its own
`CARGO_TARGET_DIR`.

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
| 5 | `ci/run-local.sh uniswap` | 1 | **not run**: `missing: forge` (Foundry is not installed on ozarchy) | 0 s |
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

### Suite 5: uniswap

`forge` / `cast` are missing, so the script exits before building or starting a node. Ports 18545 and
30933 were free, and the empty temp dir it left was removed. Running it on ozarchy needs Foundry
installed.

### Failures

None in suites 1, 2, 3 and 6.

Logs: `~/bench-results-matched/presuite-b8bf3e8a/` on ozarchy (`1-nextest.log`, `2-doc.log`,
`3-cargotest.log`, `4-{check,clippy-nodeny,fmt}-{new,base}.log`, `5-uniswap.log`, `6-pytest.log`,
`6-test_*.log`).
