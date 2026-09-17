# Async-validation flag-OFF rework — 2026-09-17

## Status: implementation tested; performance gate BLOCKED

Branch: `perf/async-validate-rework`, based on `7bc3ff9` (`perf/matched-200k`).
No enablement, merge, push, or testnet deployment. `TORUS_ASYNC_VALIDATE` stays
**OFF by default**. Do not proceed to flag-ON benchmarking/rollout on the strength
of the unit tests below.

The branch isolates only the item-3 changes from `3e4f0c2^..a69e37b`, including
the exec-applied-height cache invalidation correction. It does **not** import
`fd1fbba` (write grouping) or `4298728` (exec sidecar/cancel changes).

## What was actually found and changed

The isolated old item-3 diff added these costs even with the flag OFF:

| Old candidate | Rework |
| --- | --- |
| `RwLock` read on every inbound network message to inspect an absent tee | Startup-once `OnceLock` callback; absent-hook check does not acquire a lock or clone the callback |
| Compact decode allocates an outer note vector and clones the action hashes before updating the ledger | Generic borrowed-slice callback; sync caller updates the ledger directly with `iter().copied()`. Only the worker buffers owned notes |
| Production validation loads fail-stop twice | One production check before structural validation; direct test entry retains its check |
| Body-check helper clones the metrics Arc even for empty native bodies | Borrow metrics; clone only when entering the native-action timer |

Compact hashes are still noted **before** reconstruction, including MissingData.
Tests cover this ordering, failed reconstruction notes, no-worker OFF behavior,
validation/cache safety, absent-hook FIFO delivery, and rejecting repeat hook
installation across network clones. Comments no longer promise byte-identical or
zero-cost disabled code: presence checks and refactoring still exist.

These are source-level overhead removals, **not an explanation of all the
historical 63 vs ~35 ms/block difference**. The old s50 observation was cap 25,
120 seconds, composite `a69e37b`, repetition 2. It included items 1 and 6 and
had only two repetitions. A 100-action hash vector is about 3.2 KB, not the
orders inside those actions. No additional OFF-path body verification, DA write,
applied-height DB lookup, worker, or cache lock was found in the isolated diff.

## Functional verification

Separate invocations, with `TORUS_ASYNC_VALIDATE` unset:

```sh
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo test --locked --offline --release -p torus-consensus
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo test --locked --offline --release -p torus-network
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo test --locked --offline --release -p torus-node
```

- Consensus: 146 unit + 2 integration passed; 1 ignored.
- Network: 109 unit + 9 integration passed.
- Node: 21 passed.
- Total: **287 passed, 1 ignored**; `git diff --check` passed.
- Torus receipt: `aa7933b6-79ba-400a-9d7c-fa148d2a4c58` (source before this report).

Initial verification failed on stale telemetry artifacts: compiler claimed
`Histogram` was private and current metrics fields did not exist, while current
source exported them. Scoped `cargo clean --release -p torus-telemetry` fixed the
build without a source workaround.

## Benchmark design and provenance

Local evidence root (retained, not tracked):
`/home/18c/projects/wt/async-off-20260917/`.

- Fresh detached `base/` and `cand/` worktrees; existing `wt/matched-bench` and its
  local genesis modification were untouched.
- `candidate.patch`, build logs, compiler versions, binary hashes, harness patches,
  `bench.sh`, `compare.py`, `thread_window.py`, and helper tests are retained there.
- Node and load generator built with **separate** per-package invocations; one
  common load-generator executable for both arms.
- Baseline node SHA256:
  `121a77c0dd71b5b4023bd17602acd75bc02d6225090d2eec0a31ff1267a9b134`.
- Candidate node SHA256:
  `7b8375e1de4f3cec4c814cfec9b554f39b3e3eed591fd0f0511513f7f24bcfa8`.
- Candidate initially returned Cargo “Fresh” in 1.54 seconds and reused the
  **identical baseline executable**, despite changed source. Rejected before any
  cell; scoped invalidation of consensus/network/node release artifacts and a
  separate candidate node build corrected it. Async startup string absent in
  baseline, present in corrected candidate. A successful Cargo command alone is
  not adequate provenance with this shared target.
- Plan: B1/C1, C2/B2, B3/C3; cap 100, 10 markets, duration 120, batch 400,
  5,000 senders, rate 76,000, timeout 500 ms, identical record environment,
  `TORUS_ASYNC_VALIDATE=0 TORUS_EXEC_PIPELINE=0`, no CPU pinning.
- Unique data root per cell, loopback-only listeners, no testnet processes touched.
- Predeclared coarse screen: clean agreement/drain/dissemination for all cells;
  median paired matched/s ratio >=0.90, CPU/block and wall/block ratios <=1.10.
  This engineering screen is **not** statistical equivalence or causal attribution.

### Steady-window instrumentation discrepancy

Contrary to the old handoff, committed `gap_attr.py` uses the **whole load window**
and whole-run stage/tail snapshots. The runner has no bench+10 schedstat boundary.
Therefore it cannot provide the claimed steady HotStuff CPU/block directly.

Only the disposable baseline harness used for both arms was instrumented. It
captures full metrics and schedstat at bench+10 and bench end, concurrently across
nodes. Each scrape is bracketed by schedstat reads; the report retains raw
boundaries, actual timestamps, midpoint CPU estimates, and endpoint uncertainty.
Missing/reset counters, changed thread identity, short windows, and >1-second
scrapes fail closed. This is not an atomic snapshot. Four synthetic helper tests
pass, including a /proc thread-disappearance race discovered by the first trial.
Integration of this instrumentation into the repository runner remains separate
work; no misleading whole-load CPU / steady-block quotient was used.

## Why there is no performance verdict

1. `base-off-cap100-r1`: invalid instrumentation trial. An unrelated short-lived
   thread exited during `/proc/<pid>/task` enumeration. Snapshot collection failed;
   runner stopped the nodes. Helper now skips disappeared entries but still
   requires one live HotStuff thread. This cell was retained, not overwritten.
2. `base-off-cap100-v2-r1`: **baseline**, not candidate, failed validity:
   - 5,370 / 7,052 / 8,031 `Send Queue full` log lines across validators.
   - Steady val0: about 6,648 matched/s, 28 committed/native blocks over 119.0 s;
     161.4 ms HotStuff on-CPU and 86.7 ms runqueue wait per committed block.
   - Drain initially reported quiet, but funnel counters moved during the digest.
     Digest hashes differed; this is **unverified agreement, not proof of a fork**.
   - No panic/fail-stop lines in the agreement snapshot. All three local validators
     were stopped by the runner; raw logs and database directories retained.
   - Post-stop log collection materialized multi-GB log strings in Bash and took
     several additional minutes. It was terminated after invalidity was established;
     Bash aborted in its signal trap. No completed `summary.json`/comparison is
     claimed. Verification run `083c0fe9-25de-48c0-b3a4-096fcde382f4` records failure.

The second trial invalidated the control before any candidate cell could run.
**Zero valid paired comparisons; n>=3 gate not met.** Do not quote its throughput
as a baseline, or infer a candidate improvement/regression from it.

The symptoms resemble historical combined-build failures, but this node was built
separately and relevant local crates were rebuilt. Shared artifact provenance,
build configuration, host contention, network behavior, and baseline runtime
remain investigation targets—not established explanations.

## Next action

First establish a clean current baseline: inspect retained QUIC/backpressure and
progress logs, verify build provenance independently of the shared-target cache,
and require quiescent agreement. Then run all three paired OFF comparisons using
new labels and the same frozen methodology. If OFF parity holds, proceed to step 2.

Flag-ON remains experimental with the existing unresolved gates:
1. Network tee precedes chain-id/view/QC admission.
2. Speculative pre-verification DA durable writes amplify untrusted input.
3. Worker/fallback races can double-observe validation histograms.

The historical applied-frontier rail and existing cache tests were preserved;
this change is not a new proof of all flag-ON concurrency/state-coherence safety.
