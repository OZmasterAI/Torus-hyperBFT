# Testing (Torus-hyperBFT)

Rules for agents and sessions running tests in this repo (s1106 decision).
Goal: catch the same failures as a full `cargo test` with ~1/3 of the time
and ~1k test-output tokens per task.

## Output flags (every nextest run)

```bash
F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"
```

Prints failures in full plus one summary line. Never read a raw test log;
never pipe test output through `grep` (it hides the exit code and the
assertion message).

## While iterating

Core crates (most of the workspace depends on them):
`torus-types`, `torus-state`, `torus-economics`, `torus-core`.

| changed crate | run |
|---|---|
| leaf crate (anything else) | `cargo nextest run -E 'rdeps(<crate>)' $F` |
| core crate | `cargo nextest run -p <crate> $F` and `cargo check --workspace --tests -q` |
| big refactor of a core crate | `cargo nextest run -E 'rdeps(<crate>)' $F` |

Add a test-name filter only for very fast inner loops.

## Before every commit (mandatory)

```bash
cargo nextest run --workspace $F
cargo test --doc -q > doc.log 2>&1; echo "rc=$?"; grep -E -A6 'panicked at|^error' doc.log | head -60; grep '^test result' doc.log | awk '{p+=$4;f+=$6} END{print "doc: passed="p" failed="f}'
```

- Both must pass. Show their output as proof.
- A `flaky` count above 0 in the nextest summary is reported, not treated as a pass.
- If the full run breaks a dependent crate, fix the cause in the changed crate
  when that is where it belongs, then rerun the full suite.
- A full run takes ~5–10 min: start it with `run_in_background: true`, output to a file.

## Before merging to main (or nightly)

One full `cargo test` — the only run that catches bugs from tests sharing one
process (nextest runs each test in its own process):

```bash
cargo test --workspace -q > full.log 2>&1; echo "rc=$?"; grep -E -A6 'panicked at|^error' full.log | head -60; grep '^test result' full.log | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}'
```

## Benches

Not covered here. Bench and perf runs keep full output (`SLOW` lines, timings)
and go to the `bench-runner` agent.
