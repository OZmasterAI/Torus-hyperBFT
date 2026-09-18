# Throughput campaign: evidence and remaining gates

Objective: demonstrate at least **200,000 actual matched orders/s** under a
declared sustained workload with healthy consensus, completed execution, and
validator agreement. This document is a campaign roadmap and report skeleton,
not a claim that the target or a repeatable throughput improvement has been reached.
The current counter measures fill events; see the metric definition below before
comparing the target with another chain's order-processing claims.

## Starting evidence

The starting revision is `3cc958b` on `perf/matched-200k-next` in
`/home/18c/projects/wt/matched-200k-next`. Historical reports describe different
revisions and workloads; their timing values identify hypotheses to remeasure.

The four retained cap 200 cells produced 41,871.8 / 25,959.6 / 22,787.3 /
48,622.9 matched orders/s. **All four are rejected for dissemination failures**;
the third also stalled with pending actions. Equal final state did not prove
liveness or completed drain. See [retained-log diagnosis](stall-584-diagnosis-2026-09-18.md)
and [repeat provenance and timing](cap200-loadwin-repeats-2026-09-18.md).

In the three advancing runs, steady-window block construction measured
200.3–384.5 ms across validators; proposal construction measured 274.5–553.4 ms.
These nested timers have different observation counts and must not be summed.
Early empty-block ramp changes rankings even within the full load window.
This supports investigating construction, not attributing its whole cost to
hashing or DA writes. No accepted stable baseline or hardware ceiling follows.

## Seven stages

| Stage | Work and evidence needed | Status at report creation |
| --- | --- | --- |
| 1. Body retrieval and sync | Trace missing body/QC ancestry, reproduce demonstrated failure paths, preserve certificate/validation checks, test bounded recovery and collect a fresh live result. | Retained stall diagnosed only to retrieval/recovery; runtime fixes are under development/review. Initial live cause and successful recovery are not established here. |
| 2. Healthy cap 200 baseline | Freeze node, generator, genesis, configuration and scorer; repeat the same 120s/10-market/cap 200 shape at least three times with full acceptance. Retain failed attempts. | All four retained cells rejected. Fresh baseline work is in progress; no result incorporated yet. |
| 3. Proposal construction | Measure and A/B cached action hashes; avoid duplicate DA writes only if body availability and durability obligations remain satisfied. | Hash-cache work is under development. Duplicate DA write removal is a conditional candidate, not an implemented or measured gain. |
| 4. Largest remaining cost | Remeasure load and steady windows after accepted stage-3 changes; select the largest limiting stage with queues, work counts and CPU/I/O evidence. | Proposed. Sustained cancellation, settlement pass B/cache flush, root maintenance and DB writes are candidates, not current proven bottlenecks. |
| 5. Sustained and varied load | Longer runs, larger books, 10/100/300 markets, explicit locality, bursts and recovery; retain actual load shape and all acceptance evidence. | Proposed; historical results are not fresh campaign acceptance. |
| 6. Separate machines | Compare the accepted configuration with one validator per host and independently provisioned load generation; record hardware, network and storage. | Host/access details have been requested; no response received as of report creation. No remote runs performed. |
| 7. Architecture | Choose worker, persistence or execution experiments from the measured limiting stage; prove deterministic state and recovery before accepting speed. | Conditional roadmap only. No claim that new sharding or a particular hardware size is necessary or sufficient for 200k. |

Stages 3–7 do not replace stage-1/2 acceptance. A passing regression test proves
its exercised behavior; it does not prove that the retained live stall is fixed.

## Measurement and stage-5 matrix

Actual throughput comes from `torus_orders_matched_total` deltas, with an explicit
measurement interval; neither offered rate, order placement, nor best60 is the
target metric. The existing headline uses val0; retain counters and progress
from every validator. Use the current [harness acceptance rules](../../tools/matched-bench/README.md#liveness-and-benchmark-acceptance):
liveness PASS, established drain, successful generator, AGREE, clean complete
dissemination evidence, and passing crash gate when applicable.

| Question | Proposed cells | Comparison and observations |
| --- | --- | --- |
| Is the baseline healthy and repeatable? | Same frozen 120s cap 200/10-market shape, at least three accepted repeats. | Paired/interleaved control and candidate; all attempts retained; full-load and bench+10 windows reported separately. |
| Does throughput persist as books grow? | Equal-duration 300s controls/candidates, then 900s screening and a longer declared target interval. | Early/middle/late matched rate, resting depth, cancellation mix, pending work, commit/execution lag, memory, dirty bytes and write latency. |
| Does market count change the cost? | 10, 100 and 300 markets, uniform workload; separately declare `MPS=3` locality. | Fixed total offered orders, sender count, batching and intended action mix; report actual fills/block and dirty state. Locality is a separate workload. |
| Where is the saturation knee? | Increase offered rate around the observed knee, then probe toward/above the target. | Actual matches, admission rejection, backlog slope, latency, commit/execution progress, generator CPU and completed drain. |
| Does the system recover from bursts? | Declared low/high offered-rate intervals followed by a return to sustainable load. | Peak backlog and time to clear it, progress during the burst, dissemination and final agreement. Do not count drain work in load-window throughput. |
| Is 200k sustained? | Repeat the selected declared workload for a predefined sustained interval. | Full-interval actual matched rate at least 200k, health/consistency acceptance and no growing execution backlog; best60 supplemental only. |

Durations must match for average-throughput A/Bs. The harness's first120 metric
can compare initial windows across durations, but cannot establish sustained
performance. Seed/depth preparation and its exclusion from the measurement
interval must be explicit. Do not silently change cancellations, fills/order,
sender locality or active-market distribution to meet a headline.

The generator already supports locality through `MPS`/`--markets-per-sender`.
The older campaign reports that its 300-market measurements were uniform, not
the declared locality shape; see [campaign sections 16–17](matched-200k-campaign-2026-08-18.md).
Worker sweeps should be small and sequential, followed by repeats of promising
settings, rather than a large Cartesian product on a shared host.

Crash/replay tests remain separate correctness cells: counter resets currently
make throughput liveness UNKNOWN. Forced replay must demonstrate that replay
actually happened, not just that a restarted node agreed without rewind.

## Existing parallelism and stage-4/7 experiments

The engine is not wholly serial. Current source already implements:

- Capped, LPT-assigned per-market matching in
  `crates/torus-bridge/src/market_workers.rs::match_parallel_capped`.
- Optional sender-sharded Phase-2 preparation through `TORUS_PARALLEL_ENGINE`
  (at least two workers; default OFF) in `native_executor.rs::phase2_parallel_prepare`.
- Optional parallel settlement pass A, capped independently by
  `TORUS_SETTLE_WORKERS`, with plans restored to canonical market order.
  Pass B remains serial to preserve cross-market balance/PnL and margin clamps.
- Parallel dirty-book save drain, parallel dirty-bucket hashing, resident books,
  and root/member caches. Bucket hashing defaults to eight workers; the
  benchmark record environment also enables parallel settlement and caches.
- A depth-one flush worker behind `TORUS_EXEC_PIPELINE=1`, default OFF, in
  `crates/torus-consensus/src/exec_pipeline.rs`. The atomic flush contains state,
  trie/mirror changes and the applied-height marker together in
  `crates/torus-state/src/backend.rs::flush_pending_with_native_trie_stats`.

After fresh attribution, candidate experiments are:

| Candidate | Why investigate | Required constraint |
| --- | --- | --- |
| Cancellation grouped by price level | `OrderBook::cancel_all` still removes orders individually from queues; historical duration/depth attribution identifies repeated queue work. | Preserve cancelled-result order, remaining price/time order, indices, journals and chunk/hash invalidation; test deep mixed-trader levels. Reassess the previously regressing combined candidate rather than assuming its mechanism wins alone. |
| Settlement pass-B/cache-flush reduction | Historical 300-market execution retains substantial serial work after pass-A parallelism. | Subdivide the timer first; preserve per-trader event order, margin clamps, error behavior and canonical trade indices. |
| Joint worker-budget sweep | Matching, settlement, save and root workers compete with three validators on this host. | Record effective counts and CPU scheduling evidence; more threads are not assumed faster. A persistent pool is warranted only if spawning/scheduling is material. |
| Existing flush pipeline OFF/ON | Can overlap flush(N) with execution(N+1) when execution/flush limits progress. | Preserve one pending parent layer, atomic marker fence, serial barriers, failure latch and actual forced-replay crash coverage. Historical default-on prerequisite remains unresolved in the cited report. |
| Root/DB work reduction | Historical uniform 300-market cells show root and DB write cost exceeding batch construction. | Measure dirty buckets, cache misses, batch bytes, write latency and compaction first; do not weaken durability/root semantics as an unreported optimization. |
| Parallel pass B or persistent execution shards | Potential next architecture if serial balance application/engine work remains limiting after simpler changes. | Explicit ordering/dependency design for shared traders, deterministic trade indices and canonical root merge; node agreement, restart, snapshot and cross-market tests before acceptance. |

Existing settlement worker-cap tests compare persisted state/root across caps
(`crates/torus-bridge/tests/parallel_settle_tests.rs`); they are a starting point,
not proof for new sharding, every book format or every failure path. Consecutive
engines cannot simply overlap while the next reads mutable prior state. Root
and state durability cannot be separated without a new recovery design.

Sources: [execution pipeline design](design-exec-pipeline-2026-08-20.md),
[implemented flush worker](exec-pipeline-bl2-impl-2026-08-20.md),
[historical remaining-cost/crash gates](block-latency-campaign-2026-08-20.md),
and [historical multimarket attribution](matched-200k-campaign-2026-08-18.md).

## Results to append after execution

For each change record: commit/source fingerprint; binary hashes; complete
configuration; run labels and immutable artifact paths; workload and actual
window; offered/placed/matched rate; per-validator acceptance; stage observations
and denominators; repeated control/candidate comparison; regression/crash checks;
decision and remaining uncertainty. Keep rejected results visible.

Subsequent sections record local results. No remote-host measurement, accepted
throughput improvement or target achievement has been established.

## First fresh control and implementation review

`s60-control-cap200-r1` used the frozen s58 node/generator, nominal 120 s,
10 markets, cap 200, rate 76,000. It measured 31,214.3 matched/s, AGREE, liveness PASS,
and REJECT (drain not established; dissemination failures). Its actual load
window is 125 s. Early independent log scanning overlapped this diagnostic run,
so it is not a clean comparative performance baseline. Evidence is under
`/home/18c/bench-results-matched/s60-control-cap200-r1/`; driver, manifest and
raw node data/logs are in `s60-campaign-20260918/` beside it.

During drain all flow counters were equal and quiet while commit counters kept
advancing, yet val0's native mempool gauge stayed 35 for the 200 s timeout. Source
inspection found its only writer after nonempty native commits; admissions,
expiry and drains did not refresh it. The fix publishes size under the pool
mutation lock and removes the external stale snapshot writer. This is a metric
correctness fix; the running control's actual in-memory pool was not queried.
Three focused lifecycle/expiry tests and the 83-test mempool library suite pass.

Source review also found recovery defects, each tested independently of whether
it initiated the retained height 584 stall: an all-known sync batch omitted its
terminal certificate; deferred bodies were not retried after synced parents;
and the known-hash fast path trusted the received block's other fields. A
nonzero-chain receive filter also dropped genesis-justified body responses,
whose legacy envelope derives chain ID 0 from the universal genesis certificate.
The narrow exception requires height 0, exact genesis PC and hash-bound metadata;
normal header/body validation remains required. These findings do not prove a
live performance gain. Fresh candidate runs are still required.

The proposal hash candidate is isolated on `perf/cached-proposal-hashes`.
A lazy cache avoids adding an unconditional follower-validation hash pass;
ordered matching and authoritative committed fields remain unchanged. Explicit
DA mirroring remains: successful pool selection does not prove the buffered
mirror has become durable.

An independent cancel-all candidate is isolated on
`perf/cancel-level-compaction`. Its initial release microbenchmark suggested faster deep
dense cancellation but regressed shallow/sparse cases; that unrestricted policy
was rejected. Later review found setup-order, hash-seed and codegen confounds,
so those speedup estimates are not accepted evidence. A narrower production
gate and balanced A/A, A/B and B/B diagnostics are under test.
The initial 114-test core suite passed with 4 ignored tests. These microbenchmarks
are not chain throughput results and do not justify accepting the candidate.

Recovery review additionally found that a requested body hash did not by itself
bind all received block metadata. Tracked replies now match their authenticated
header; by-hash recovery verifies the outer hash and certificate independently.
Malformed replies leave the legitimate fetch active. An explicit immediate-parent
presence check prevents the application-view iterator from concealing a missing
parent. Payload-free recovery tracing is opt-in with `TORUS_BODY_FETCH_TRACE=1`.

Compact consensus-body push (`TORUS_BODY_PUSH_MAX_BYTES=65536`) already exists and
remains default OFF. It is a separate experiment from the multi-megabyte native
body pre-push. A candidate may test it after the genesis transport correction;
no performance or safety default is promoted from inspection alone.

## First accepted recovery control

Runtime fixes are committed as `3bd7c01` (mempool gauge) and `9af0eea`
(authenticated body recovery, genesis transport and sync certificates).
The final recovery verification passed 85 HotStuff tests and a separate node-only
release build; the unchanged metric/app integration had passed 83 mempool and
146 consensus tests (one ignored).

`s60-recovery-cap200-r1` used the frozen `9af0eea` node, unchanged s58 generator,
120 s nominal / 127 s actual load, ten markets, cap 200, rate 76,000, and
`TORUS_BODY_FETCH_TRACE=1`. It **ACCEPTED**: 49,242.8 actual matched orders/s
throughout the load window, liveness PASS, clean dissemination (zero exhausted,
sync fallback, outbound DA failures or starvation), drain established in 57 s,
and final validator AGREE. Best60 was 71,670.7; that is supplemental and does not
establish sustained 71.7k. One accepted cell does not establish repeatability or
isolate which recovery change prevented the older failure. The original height
584 stall remains unproven as a specific root cause.

Fresh startup was healthy after seven seconds. The trace observed genesis-body
serving; all three nodes later reached zero pending work and equal native flow
counts. Native execution queue depth peaked at 56. Load-window construction
averaged 176–179 ms across nodes. Execution engine averaged 450–471 ms per native
block and flush 233–241 ms over the separate bench-plus-drain window; these are
not directly comparable timer boundaries. Cancellation/phase1 rose with book
depth, supporting a separate sustained-execution experiment. No hardware ceiling
or architecture requirement follows from this single result.

Artifacts: `/home/18c/bench-results-matched/s60-recovery-cap200-r1/`, with raw logs,
manifest and frozen binaries under `s60-campaign-20260918/`. Node SHA256:
`49282cf7794d67137fade41b52da5d712ebb0245fad20cce3e5cf024696d93e7`.
Generator SHA256: `ce06befbab9e87f9e98d0f45a7c18e506d7764f1709be532550f5b342c064528`.
Temporary database retention is tracked separately; logs, metrics, digests,
summaries and binary/configuration provenance remain the comparison evidence.

Source review found a generator locality defect: parity of global sender index
can give a market only one side when sender assignment repeats with an even
period (including MPS=1/10 markets and MPS=3/300 markets). A separate candidate
alternates actual per-market owners, preserving the uniform default workload.
Locality cells must use the corrected, explicitly identified generator.

## Hash-cache comparison in progress

The unchanged `torus_orders_matched_total` counter measures **fill events**, not
unique orders or two order legs. See [metric definitions and load generation](matched-fill-units-and-generator-2026-09-18.md).
Historical values above retain that counter definition. They do not establish
an equivalent cross-chain order-processing rate.

| Cell | Frozen runtime | Actual load | Matched fills/s | Verdict | Drain |
| --- | --- | --- | --- | --- | --- |
| `s60-recovery-cap200-r1` | `9af0eea` | 127 s | 49,242.8 | ACCEPT | 57 s |
| `s60-hashes-cap200-r1` | `388f7cd` | 127 s | 51,608.1 | ACCEPT | 93 s |
| `s60-recovery-cap200-r2` | `9af0eea` | 128 s | 48,226.3 | UNVERIFIED | 46 s |

The hash candidate passed 149 consensus tests (one ignored); its final narrow
cold-cache refinement passed three focused regressions and a separate node build.
The first live candidate reduced load-window construction means to 127–147 ms
from 176–179 ms. Its execution queue peaked at 61 versus 56, and its longer drain
prevents treating faster construction as an equivalent sustained-throughput gain.
The candidate remains isolated pending repeated comparisons.

The second control had clean dissemination, completed drain, and AGREE. Its
sampler has a six-second gap on all nodes at 03:08:25–03:08:31 UTC, exceeding the
five-second evidence limit; each observed commit counter increased by one across
that gap. No stall was demonstrated, but the original UNVERIFIED verdict is
retained rather than changing acceptance to admit this result.

Each cell's `summary.commit` identifies the harness worktree. Its frozen runtime
is identified by the campaign `artifacts/*/manifest.json` and per-cell
`*.artifact-provenance.json`, tied to the measured binary hash. The hash node's
SHA256 is `d6dfe515cb76175817056447407ff606957317f3653f705ee60ccf531cb7c6f7`.

Producer-stage attribution plus default-off exact-byte DA reuse is prepared on
`perf/proposal-attribution` at `0444b7e`. All four changed library suites,
health/harness/summarizer tests, shell syntax, and a separate node build pass.
The frozen artifact is ready for a same-binary OFF/ON comparison; it has no live
performance result. Offline retention tooling, if later present on that branch,
is separate from this already-frozen runtime.


## Current readiness and remaining constraints

As of the latest local work, stage 1 has tested runtime fixes and one accepted
control; stage 2 still lacks three accepted control repeats. Stage 3 has one
accepted hash-cache cell and a verified, frozen DA attribution candidate, with
repeat and OFF/ON comparisons pending. These are prerequisites for choosing a
production optimization, not completed throughput stages.

Stage-5 preparation is committed as `a8d9542` on
`perf/bench-locality-balance`: corrected actual-owner side balance, unchanged
default uniform action bytes, workload manifests, configurable price/cancel
shape, and optional economic rate schedules with strict provenance. Forty-six
Rust tests and 76 Python tests passed; no live burst/locality result exists.
Depth seeding and phase-specific achieved-rate/recovery scoring remain open.

Strict replay qualification is on `test/strict-crash-replay` (`441e967`), with
the fresh-worktree nested-genesis output fix `f99e15c`. Harness verification
passed, as did three tiny genesis fixture regressions. A first 45-second label
was rejected before launch for an invalid kill offset; the second stopped
before validator launch because the weighted-genesis child inherited final
`OUT`. Neither is live recovery evidence. The corrected third attempt, `s60-pipeline-replay45-r3`, used kill offset
15 seconds and the frozen recovery node. It observed positive replay:
applied height 783, committed height 784, gap 1, worker attached at 784.
All validators agreed at height 790, with no panic/fail-stop. The separate
strict crash verdict is PASS, but the full cell is REJECT: progress stopped
with pending mempools and empty execution/flush queues, and drain timed out
after 208 seconds. This is evidence of one successful replay, not healthy
post-restart liveness or deterministic pending-parent C1 coverage. The pipeline
remains default OFF. Stall issue `879b6055-5220-4352-a25c-74ff0e807bd9`
is under investigation; transport and consensus causes remain hypotheses.

Retained databases currently limit the larger matrix: about 21 GiB remained
before the short replay attempt. A cleanup decision is pending; no retained
campaign database has been deleted or flushed. Offline all-CF flushing would
change physical WAL evidence and also requires that retention decision.
Separate-machine testing remains pending host/access details; no remote run
or infrastructure provisioning has occurred.


## Follow-up consensus fixes after the short replay stall

The retained replay cell did not remain completely disconnected: QUIC links
recovered, and later sync requests succeeded on all validators. This rules
against permanent total transport isolation. Missing consecutive progress
remains unexplained; the configured exponential timeout can reach 128 seconds,
and the retained logs do not identify each vote's signer and certified view.

Source review found two independent defects, now committed on the main campaign
branch: `09f3449` binds header votes and proposal-status updates to the header's
matching local view; `b60097a` releases future-message buffer occupancy on
delivery/expiry, rejects oversized entries before eviction, and evicts only the
actual deficit. Out-of-view headers still retain authenticated certificate,
lock and body-recovery handling. A receiver-level regression exercises early
future delivery followed by its cached matching-view replay and exactly one
vote. Existing accounted-message sizes are unchanged; this is not a new
heap-memory sizing policy.

All 92 HotStuff library tests and a separate node-only release build passed
(receipt `7ce69082-16c3-407f-9625-86d3d4579fae`). Neither source fix proves
the cause of the retained stall. Frozen node SHA256:
`0c45c479bdeac981f48aa0f016d99d3e8d42376c131fc1400f1c49b0a02bcdd7`.
The unchanged generator is retained. `s60-viewbound-replay45-r1` is the pending
short crash qualification with `TORUS_WEDGE_DIAG=1`; its result must be assessed
separately from throughput acceptance.

The cancellation mechanism experiment now has two independent fixture-seed
runs: five deep levels with 16 middle targets per level gave paired baseline/
forced-grouping medians 1.648/1.721 at depth8192 and 1.971/1.957 at depth32768.
Identical-code controls were near parity in those cases; sparse one-order-per-
level controls remained skewed and inconclusive. A bounded allocation-free
multilevel gate is isolated on `perf/cancel-level-compaction`, with 11 passing
differential tests (receipt `68ec19a1-b0b6-4b67-90b2-c6735d8e6e2d`). Its actual
gated-path timing and whole-chain acceptance remain pending. These microbench
ratios are not matched-throughput gains.
