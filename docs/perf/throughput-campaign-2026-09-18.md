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

| Stage | Current evidence | Remaining work |
| --- | --- | --- |
| 1. Body retrieval and recovery | Authenticated body/sync fixes plus view-bound header voting and corrected future-buffer accounting are committed and tested. One later restart drained and agreed; an earlier run replayed one block but stalled. Outbound timing captured late body requests/responses and local handoff delays. | Identify the earlier stall's cause and complete healthy positive-replay/C1 qualification. Queued-response scheduling did not fix late arrivals. Actual swarm-poll and receiver-stage diagnostics have passed source review but remain untested. |
| 2. Healthy baseline | Corrected collection produced accepted current-runtime controls, but results vary. Five-minute 10- and 50-market controls both failed dissemination despite completed drain and agreement. | Three accepted repeats on the final selected runtime; diagnose longer-run body-fetch exhaustion. |
| 3. Proposal construction | Historical hash-cache result remains 51,608.1 fills/s. Current DA OFF/ON and body-reuse screening did not establish a throughput gain. | Repeat hash comparison and any promising combination before promotion. |
| 4. Largest remaining cost | Revised cancellation passed 248 tests and an accepted five-minute cell at 38,142.7 fills/s. Integer-margin follow-up passed 47 tests and an accepted 38,610.6 cell. Fixed-key trade routing passed 56 test executions and an accepted same-binary OFF/ON pair at 32,786.3 / 40,819.2 fills/s. | Repeat the promising fixed-key pair in reversed order; a single pair does not establish a repeatable gain. The feature remains default off. |
| 5. Sustained and varied load | Five-minute 10-market baseline/cache and 50-market baseline completed with depth observations. Cache arm accepted at 34,563.5 fills/s; both baselines failed dissemination. | Locality50/MPS3 completed at an accepted 33,233.1 fills/s. Deeper books, bursts and healthy matched-duration repeats remain. |
| 6. Separate machines | Owner confirmed no remote hosts are available; continue on this machine only. | Cross-machine validation is unavailable and cannot be inferred from local results. |
| 7. Architecture | Flush-pipeline recovery evidence, cancellation experiments and a default-off WAL-budget candidate are being assessed. | Promote only after deterministic state/recovery and repeated throughput evidence; no 200k claim. |

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
The unchanged generator is retained. `s60-viewbound-replay45-r1` used
`TORUS_WEDGE_DIAG=1`, nominal 45 s / actual 60 s, and kill at +15 s. It completed drain
in 57 s with final AGREE and clean dissemination. No replay gap occurred
(worker attached at 586, pre-kill execution/flush queues both 0), so its strict
crash verdict is FAIL and overall verdict REJECT. Liveness is UNKNOWN after
the restart counter reset. This is observed healthy restart/drain in one run,
not positive-replay qualification, proof of the older stall's cause, or an
accepted throughput comparison. The pipeline remains default OFF.

The cancellation mechanism experiment now has two independent fixture-seed
runs: five deep levels with 16 middle targets per level gave paired baseline/
forced-grouping medians 1.648/1.721 at depth 8192 and 1.971/1.957 at depth 32768.
Identical-code controls were near parity in those cases; sparse one-order-per-
level controls remained skewed and inconclusive. A bounded allocation-free
multilevel gate is isolated on `perf/cancel-level-compaction`, with 11 passing
differential tests (receipt `68ec19a1-b0b6-4b67-90b2-c6735d8e6e2d`). Actual gated-path timing subsequently found a dispersed-layout regression;
the refinement below reduced that issue but did not earn acceptance. Whole-chain
validation remains pending. These microbench ratios are not matched-throughput gains.


After retaining the second short recovery cell, free space is about 13 GiB.
The live-run capacity guard now prevents further cells. No campaign database
has been deleted or flushed. A separate default-off WAL-budget candidate for
newly created databases is verified and frozen; it cannot reclaim existing
retained data and its soft flush trigger is not a guaranteed disk bound. Larger repeat
and varied-load matrices remain pending storage capacity/retention and remote
host details.


## Fresh-database WAL budget

Commit `b0a57ce` on `perf/wal-budget` adds the optional
`TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` setting on top of runtime `b60097a`.
Unset/zero preserves RocksDB's automatic policy. A positive value triggers
ordinary flushing of column families that retain old WAL files; it does not
disable WAL or change atomic state/marker batches or fsync policy. More flushes
could increase compaction and latency, so no throughput benefit is claimed.

The benchmark records the setting and atomically refuses existing data paths
for positive-budget runs. Receipt `8d837342-6089-427c-9d9f-96895e08ed9a`
covers 76 Python tests, summarizer checks, four WAL tests, 131 state tests
(two ignored), shell checks and a separate node-only release build. New temporary
fixtures observed an automatic cold-CF flush, then preserved acknowledged rows,
a cold sentinel and the atomic state/applied marker after SIGKILL. This does
not establish power-loss durability or pipeline C1 qualification.

The frozen artifact is `artifacts/wal-budget/release/torus-node`, SHA256
`b629b6c424922d4253f6dd002326114cecad8ccce1286b39dc903a30cafef14d`.
It retains the unchanged generator. A same-binary automatic/1024-MiB comparison
with fresh databases is pending capacity. No existing campaign DB was opened
for maintenance, flushed or deleted.


## Final cancellation comparison in this local pass

The first actual multilevel gate (`ca30eea`) was rejected: its dispersed case
had baseline/candidate ratio 0.915 with near-parity identical-code controls.
A four-length scan/removal crossover retained dense-middle gains of 1.674 and
1.839 at depths 8192 and 32768, and brought dispersed cancellation to 0.987.
But the sixth-level fallback measured 0.938 (AA 1.030 / BB 1.005), while edge
controls were noisy. The candidate remains on `perf/cancel-level-compaction`,
unmerged and without performance acceptance. Further threshold tuning without
repeat diagnosis and live activation measurements would overfit these fixtures.

Receipt `ce7b778d-5d7a-4a00-bc6d-c22c6ee9f8fb` passed 121 core unit tests,
89 integration tests, and the isolated benchmark command. Seven ordinary-suite
tests were ignored; the selected multilevel microbenchmark was run explicitly.
Results and manifests are retained as `cancel-multilevel-crossover4-r1.*`.
Semantic verification is distinct from performance acceptance.

## Resume order

1. Keep sufficient local capacity without deleting future retained runs unless
   authorized. The owner restored 89 GiB and specified local-only work; remote
   hosts are unavailable.
2. Rebaseline the latest frozen recovery runtime with three accepted controls.
3. Rebase and verify the isolated hash/DA candidates onto that runtime; freeze
   separate node-only builds and run interleaved same-workload comparisons.
4. Test the fresh-DB WAL budget with storage and latency measurements; qualify
   positive replay plus healthy drain and the deterministic C1 scenario before
   enabling the execution pipeline.
5. Use measured costs and cancellation eligibility to choose the next change,
   then run the sustained, depth, market-count and burst matrix on local and
   separate-machine configurations.

Best accepted full-load result remains 51,608.1 fills/s from one hash-cache cell
versus one accepted 49,242.8 control. This is not a repeat-proven improvement,
a stable hardware ceiling, or an apples-to-apples 200k cross-chain comparison.
Stages 1–7 remain partially completed; no new repository is justified by the
current evidence, and no branch was pushed.


## Resumed after owner cleanup

The owner executed the supplied explicit six-directory deletion command and
reported 89 GiB free. The six old campaign data paths are absent; summaries,
logs, metrics and frozen binaries remain. This removed physical DB/WAL evidence
for those six runs; their saved observations remain historical evidence. The owner subsequently authorized cleanup of completed campaign databases
after their results are saved: “Yes, clean up completed campaign databases.”
Logs, metrics, digests, summaries and frozen binaries must be retained. Only
finished campaign database directories are in scope; active data is excluded.

A fresh 120-second, ten-market, cap-200 control on frozen runtime `b60097a`
completed as `s60-viewbound-cap200-r1`, with unchanged generator and
`TORUS_BODY_FETCH_TRACE=1`. No runtime settings or acceptance rules were relaxed.

Owner scope clarification: “Use this machine for now; no remote hosts available.”
Continue local optimization and report its shared-host limitations. Do not wait
for SSH details or provision remote machines. Stage 6 is unavailable, not passed.

This supersedes the earlier pending cleanup decision. Record each exact
deletion through Torus authorization using the owner quote; verify saved
evidence and stopped processes before removing a completed run’s data.


### Current-condition rebaseline

| Cell | Runtime | Actual load s | Fills/s | Drain s | Verdict |
| --- | --- | ---: | ---: | ---: | --- |
| s60-viewbound-cap200-r1 | b60097a | 131 | 19,579.2 | 50 | ACCEPT: liveness PASS, AGREE, clean dissemination |
| s60-recovery-cap200-r3 | 9af0eea | 126 | 11,210.5 | 205 timeout | REJECT: liveness FAIL, dissemination failures, no drain |

The new accepted control is much slower than the earlier 49–52k results.
Generator, genesis and workload settings match. Broad execution-phase and
DB-write inflation accompanies fewer generator acknowledgments. Scheduler
runqueue delay per executed native block did not rise. This is not yet an
attribution to the header-view or buffer fix, or a measured hardware limit.
The old runtime comparison stalled at commit count 61 despite successful sync
responses and empty execution queues; its throughput cannot establish a valid
speed ranking. Issue `aea43673-9a35-4439-a16f-c9f79b26fbc6` tracks this gap.

Both completed database directories were removed under the owner's cleanup
authorization after required evidence was checked and per-run retention/storage
inventories saved. `rm -r -- EXACT_PATH` succeeded. The tool rejected `rm -rf`
style commands, so forced deletion is unnecessary. Torus authorization must use
the launch cwd `/home/18c/projects/Torus-hyperBFT` because the Bash hook sees that
scope even when the execution tool specifies a different workdir. This is a
scope-recording detail; permission came from the owner's explicit instruction.

Latest-base integrations are prepared without changing their original patches:
hash `3495652` on `perf/cached-proposal-hashes-viewbound`, DA/timers `5ddc427` on
`perf/proposal-attribution-viewbound`. Both are based on main `d3e1370` and retain
the latest consensus fixes. Fresh integration verification and live comparisons
are required; original candidate branches and frozen artifacts remain intact.


### Build identity and host observations

Hash integration `3495652` passed 149 consensus tests (one ignored) and a separate
node build, receipt `247ed19c-d694-492b-a191-b557cc180422`. Its frozen node SHA256
is `4e28a137ccd640842c415b1e8584b57b1309ac1115444a2c47bb94957bdb2fd5`.
Hash docs-only follow-up `bdeed23` corrects historical cache descriptions.

DA integration source tests passed receipt `2778f6a7-7dbd-4071-8036-a68792303435`,
but artifact inspection found its node was byte-identical to the hash-only node
and lacked the DA flag. The shared Cargo target had reused a stale executable.
`artifacts/proposal-attribution-viewbound/manifest.json` is explicitly invalid;
no DA cell used it. Rebuild verification cleans only relevant generated package
outputs, then requires both the DA flag and producer metric in the node binary.
Do not treat a successful Cargo exit alone as artifact provenance.

The first hash driver invocation was stopped during cooldown before creating a
label or launching nodes. The host's hourly `fstrim /` was active around 08:18 UTC,
with I/O wait; this is an observed current wait cause, not proof of the earlier
throughput drop. Driver startup now waits for both load1 < 1.5 and no active
`fstrim`, and records its source SHA256.

A separate read-only host observer records raw CPU/steal/iowait, PSI,
MemAvailable/Dirty/Writeback and root-device diskstats every two seconds. Six
fixture tests passed and independent review found no blocker for fresh labels.
Its runtime overhead has not been measured; future comparisons should all use
it consistently. Earlier cells lack these observations. It writes exclusively
to a fresh per-label JSONL, stops on completed manifest/deadline, and flags
missing/reset data instead of substituting zero.


The repaired DA artifact is now frozen separately at
`artifacts/proposal-attribution-viewbound-rebuild1`, node SHA256
`a4be85236cf8f7ee6746a3d2a27f69cbbb9d724ddc8f9523e66e406e81641ef8`.
Forced rebuild receipt `147896d0-028e-4f3c-a49d-f5b21086f090` and positive
identity receipt `80f4f186-d888-4a56-97c3-80c373011ddb` passed. The original
invalid artifact remains marked invalid; no DA cell has run yet.

### Hash integration first live cell: unverified sampling

`s60-hashes-viewbound-cap200-r1` ran runtime `3495652` for an actual 132-second
load window: 30,942.3 fills/s, first120 30,733.4, drain 52 seconds, AGREE and
clean dissemination. It is **UNVERIFIED**, not accepted: all three validators
have a common six-second sampler gap from epoch 1789720203 to 1789720209.
Commit counts advance by two/one/one during that gap. The independent host
observer continued with a maximum interval of 2.099 seconds, so this does not
look like a machine-wide six-second observation pause. No acceptance rule
was relaxed and the historical run remains unverified.

The existing collector timestamps a cycle before three serial scrapes, each
allowed three seconds, then runs multiple shell/awk parsers per node. A slow
endpoint can delay and backdate observations for all nodes. A bounded independent
per-node collector is being prepared, with explicit invalid timeout rows,
accurate response-completion timestamps, diagnostics and complete subprocess
cleanup. New repeats will use the corrected collector on both arms.

The host observer covers about 130 seconds wholly inside the load interval:
reported CPU ticks were 54.2% user, 19.9% system, 23.5% idle, 1.3% iowait and
zero steal; minimum available RAM was 67.1 GiB. These counters do not identify
a cause or prove unused effective CPU capacity. During the sampling gap there
was writeback and increased I/O pressure. The run's DB was cleaned only after
its evidence and retention inventory were saved; raw node and host logs remain.


### C1 pending-parent fixture qualification

Commit `93b94ce` on `test/c1-pending-parent` implements the default-off
`TORUS_C1_QUALIFICATION` hook and deterministic subprocess fixture. Receipt
`70a3bccf-e6a4-4724-99fe-939ad6c4eafe` passed 132 state tests (two ignored),
152 consensus tests (one ignored), and a separate node build. The node's C1
flag was positively checked before freezing; SHA256 is
`bea89230c7edd2bab2781f24ccaeea86188b4fd42625072c0bed64d35a7b49e1`.

The fixture parks the flush worker before writing N, proves that actual execution
of N+1 uses nonce state supplied only by pending N, then SIGKILLs the process
before wall and boot-clock deadlines. Restart explicitly disables the hook,
replays from the durable marker, and matches every CF row and native root
against serial execution. Invalidation, timeout and teardown never release a
parked write. Independent review closed a deadline race and preserved the
public WorkerEnv API.

This qualifies the exercised nonce/replay-guard dependency and subprocess
recovery. It is not a three-validator C1 run, proof of every economic read path,
power-loss test, or throughput measurement. Both hook and execution pipeline
remain default-off; live C1 orchestration is a separate follow-up.

### Independent sampler qualification

Commit `f969b3d` replaces serial shell sampling with independent bounded node
requests and response-completion timestamps. CSV integer timestamps and health
rules remain unchanged; precise timing and curl failures are retained in
`sampler-diagnostics.jsonl`. Partial or failed HTTP responses cannot count as
valid samples. Shutdown cancels and reaps in-flight curl children.

Receipt `0a3af7cf-7480-4f99-910d-8a363c1f605e` passed 81 Python harness tests,
summarizer regression, shell syntax and diff checks. The first verification
timed out in a Python 3.12 mock-server teardown and hit an obsolete shell-parser
test; both test defects were corrected before the passing run. Coverage includes
a delayed endpoint while other nodes advance, malformed required metrics,
legacy AWK parser parity, partial responses, full pipe buffers and CLI SIGTERM.
This is collector qualification, not a performance gain or retrospective change
to previous UNVERIFIED runs.

A separate historical phase CSV defect was found: six state-write detail fields
were inserted into PHASE_COLS while its header and positional phase60.awk stayed
unchanged. This shifts the old per-phase analysis for EVM resync, flush, dirty
buckets and queue depth. Named-column wide CSV and headline/scoring are unaffected.
Preserve old artifacts; use wide CSV for those historical phase measurements.

### Collector live follow-up and digest correction

Phase mapping fix `8c142cb` passed the distinct-value collector-to-AWK fixture,
summarizer and shell checks, receipt `a8d4c97f-4ebe-4b46-9ac5-f6de3b9892b8`.

`s60-viewbound-asyncsample-cap200-r1` used frozen b60097a and harness 8c142cb:
37,078.5 fills/s over 131 seconds, drain 66 seconds, liveness PASS, **REJECT**
for one val2 body-fetch exhaustion/sync fallback. Collector samples were all
valid with at most two seconds between observations on each validator.

This cell also exposed a harness regression: removing the old extract helper
left a call in funnel_snapshot. Both failed snapshots became empty strings;
their equality falsely marked digest quiescence. The reported AGREE therefore
lacks a valid quiescence proof. Original artifacts remain untouched, with a
separate campaign audit JSON explaining the defect. This run is ineligible for
performance comparison. Its roughly 12 GiB databases were cleaned after evidence
and inventory were saved under the owner's standing authorization.

Fix `f7a1b68` makes digest snapshots require successful HTTP, four unique finite
counters on every node, and two complete nonempty equal snapshots. Receipt
`02e6a14e-5bcb-4160-a8fb-7ddcee5c1a5b` passed 88 Python tests plus summarizer,
shell and diff checks. The new tests execute the production shell functions and
comparison, including failed responses with valid-looking bodies, missing helpers,
empty observations and failures on any validator. Independent review passed.
The sampler issue was reopened for this regression and resolved only after these
checks; a new live cell is still required.

### Isolated proposer body reuse

Runtime `2f1b832` on `perf/proposal-body-reuse` retains the action vector cloned
for DA mirroring and moves it into the proposal, removing a second deep clone.
The original sender/action pairs remain separately owned for preproposal push.
Both DA modes preserve ordering, custody and full-batch write-failure requeue.

Full release consensus tests, a targeted mempool requeue regression and a separate
node build passed receipt `fdd7a4a0-bcbb-477a-b298-cc71c8dccd58`. The five changed
generated packages were cleaned before testing/building to avoid stale target
reuse. Two source reviews found no blocker. Frozen artifact `proposal-body-reuse`
has node SHA256 `8a644d2f46bd4eeec434e8c79ba886e15ba3a0ad90a9a351824bb9fefaf3bc60`
and the unchanged generator. This remains an unmeasured candidate, not a promoted
performance improvement.

### Repaired-harness live cells and additional qualification

`s60-hashes-asyncsample-cap200-r1` (frozen3495652, runner7556118) is **ACCEPT**:
27,658.5 fills/s over132seconds, first12028,634.5, drain48seconds, peakexecqueue65,
commit-intervalp953805.0ms. Digest snapshots are valid/quiescent; livenessPASS,
AGREE and clean dissemination. This is the first accepted latest-base hash run
with the corrected sampler. It does not reproduce the historical51.6k result.

`s60-viewbound-asyncsample-cap200-r2` (b60097a) passes the automated health gates
at32,681.7fills/s over133seconds withdrain40, but is **excluded from performance
comparison**: root created the engine-mode3-matrix worktree during active load.
Checkout CPU/metadata IO impact was not measured. A separate audit JSON records
this interference; retain the health result and repeat cleanly. Both cells'
databases were cleaned after saved evidence and retention inventories.

Scheduled-accounting/report commit `0b6f9fa` passed49Rust and98Python tests,
summarizer/shell checks and a separate generator-onlybuild, receipt
`6b760859-9d41-400b-ae4d-02b3e3fcb361`. Frozen `viewbound-scheduled-generator`
combines verifiedb60097a with generatorSHA256
`9c210f31cefdb4febb111e7f21d62628f98fdedf727b59e6372028c9cc73d4a6`.
Current control/hash comparisons continue using the old frozen generator.

Test-only `0d928aa` on `test/engine-mode3-matrix` closes the resident mode3
parallel-engine gap: each mode2/3 compares its own serial baseline with2/4/8
workers across resident handoff, roots/CFbytes/results/errors/gas/IDs/trades,
including interleaved cross-market margin exhaustion. Full8-test engine suite
passed receipt `947e3f01-aee7-43c3-abb8-30a5d1d1dc17`. No default flag changed.
Later local worker trials must keep settlement fixed: control ENGINE=0,
MATCH_WORKERS=18, SETTLE_WORKERS=18; treatment changes only MATCH_WORKERS=4.
Matching already runs in parallel by market; these are caps, not new parallelism.

### Clean control and proposal-storage pair

`s60-viewbound-asyncsample-cap200-r3` is the clean replacement control: **ACCEPT**,
30,480.6 fills/s over 134 seconds, first120 30,167.0, drain 32 seconds, peak
execution queue 19, commit interval p95 5233.8 ms. Valid quiescent agreement and
clean dissemination; runtime b60097a and runner 13a1944. Its 8.90 GiB databases
were cleaned after preserving evidence.

The same frozen `proposal-attribution-viewbound-rebuild1` binary and b03004d
runner produced this first DA pair (old generator, 120s nominal, 10 markets,
cap 200, requested 76,000 actions/s, BODY_FETCH_TRACE=1):

| Run | DA ensure | Verdict | Fills/s | Actual load seconds | Drain seconds | Peak execution queue | Commit p95 ms |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| s60-da-off-cap200-r2 | 0 | ACCEPT | 38,273.9 | 130 | 46 | 60 | 3407.6 |
| s60-da-on-cap200-r1 | 1 | ACCEPT | 37,966.3 | 133 | 48 | 64 | 3682.6 |

Both established liveness, quiescent agreement and clean dissemination. No
throughput gain is established; DA ensure remains default OFF. Per-validator
load-window mirror time/proposal was 49.70/47.73/53.57 ms OFF versus
44.05/41.19/53.01 ms ON. Counts differ (94/95/94 versus 74/76/71), and ON fills
per native block grew from 44,743 to 53,942. These means do not isolate storage
cost. Selection and encode remained substantial; the next body-copy trial uses
DA OFF to keep that setting fixed.

The first `s60-da-off-cap200-r1` attempt failed before node launch: a missing
weighted genesis exposed inherited exported OUT in its child generator. No
database was created. The successful pair reused the exact control weighted
base and produced genesis MD5 `478d698b5b331837fd7d0c378ece9f46`.
Fix `1038e62` isolates child output; three actual-script regression fixtures and
shell/diff checks passed receipt `0692d82b-b8fd-4dda-abe4-abea21d25495`.

Optional depth observer integration `998d1eb` on the workload branch passed five
actual Bash lifecycle fixtures, receipt `6aee4cb0-12a5-48c2-a5c2-a54f5e94d4ac`.
It defaults OFF, observes val1 on the declared nominal schedule, preserves
partial status, and stops/reaps on failure or EXIT. It does not affect acceptance.

### Body-copy and WAL-budget screening

`s60-body-reuse-daoff-cap200-r1` (runtime2f1b832, runner526d9b7) is **ACCEPT**:
30,944.6 fills/s over131seconds, first12030,422.8, drain35seconds, peakqueue65,
commitp953710.5ms. It did not outperform the preceding DA-OFF control38,273.9;
no promotion. Retained evidence includes quiescent agreement and clean
dissemination; 9.87GiB completed databases were removed after inventory.

The first same-binary WAL pair uses runtimeb0a57ce, runner8030686, frozen
`wal-budget` node SHA256 `b629b6c424922d4253f6dd002326114cecad8ccce1286b39dc903a30cafef14d`,
old generator, BODY_FETCH_TRACE=1, and the same nominal120s/10market/cap200 shape.

| Run | WAL threshold MiB | Verdict | Fills/s | Actual load seconds | Drain seconds | Peak queue | Commit p95 ms | Completed database GiB |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| s60-wal-auto-cap200-r1 | 0 (automatic) | ACCEPT | 29,116.7 | 128 | 42 | 6 | 7065.6 | 10.47 |
| s60-wal-1024-cap200-r1 | 1024 | ACCEPT | 35,392.9 | 130 | 42 | 58 | 3884.4 | 2.61 |

Both passed liveness, quiescent agreement and dissemination. Automatic control
had an unusually slow idle cadence (later probes8.1/4.4blocks/s); treatment had
27.8blocks/s. The throughput difference requires repetition, not promotion.
The soft threshold did reduce retained log-file bytes:10.47billion to1.78billion
across all validators, while SST bytes rose0.77billion to1.02billion. During load,
per-node WAL bytes written actually rose from2.53–2.60GB to2.77–2.95GB, with more
flush/compaction output. This is reduced retention, not reduced bytes written.
Completed database inventories and all measurement artifacts were retained;
only databases were cleaned under standing authorization.

The external campaign driver now accepts an explicit recorded `--runner-env`
allowlist for BAND/CROSS_FRACTION/CANCEL_FRACTION/MPS/RATE_SCHEDULE/DEPTH_OBSERVER.
Five parser fixtures covered accepted/default mappings and unsupported/malformed/
duplicate rejection before launch. Both WAL arms used the same updated driver
SHA256 `f2f7d1e9b6dd065e3c3a86171b0294b03142af0d70e23a83b7a50df64a887e95`.

### Worker screening and balance-cache qualification

The worker trials use the same frozenb60097a binary and MAIN8bfe6d7 runner,
old generator, nominal120s/10market/cap200/76000requested actions/s, and
BODY_FETCH_TRACE=1. Settlement stays explicitly capped at18 throughout.

| Run | Matching cap | Preparation workers | Verdict | Fills/s | Load seconds | Drain seconds | Peak queue | Commit p95 ms |
| --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| s60-workers18-engine0-cap200-r1 | 18 | 0 | ACCEPT | 43,619.4 | 128 | 68 | 57 | 3690.7 |
| s60-workers4-engine0-cap200-r1 | 4 | 0 | ACCEPT | 38,213.0 | 129 | 41 | 63 | 3883.3 |
| s60-workers18-engine4-cap200-r1 | 18 | 4 | ACCEPT | 34,202.9 | 129 | 68 | 14 | 4437.3 |

All passed liveness, quiescent agreement and clean dissemination. Neither
treatment establishes an improvement. ENGINE4 had slow idle probes0.3/1.7blocks/s,
versus25.9 and29.6 in the other cells; its best60 of55,752.4 does not describe
full-window throughput. Keep matching's existing cap and preparation disabled.
The same baseline runtime now has widely varying healthy results; these single
screening arms do not establish causal regressions or a hardware ceiling.

An isolated additional candidate, `e4340d8` on `perf/balance-cache-inplace`, updates
one borrowed cached balance entry and stores its dirty flag beside the value,
removing repeated map reinsertion and dirty-set hashing. It retains the small
temporary scalar copy to preserve panic atomicity, every mutation's canonical
order, read-error retries, clean rejected reservations, zero-event writes,
disjoint sender merges and full-set retry after partial flush failure.

Five focused and37 integration release tests plus a separate node-only build
passed receipt `fc9af5df-634d-4b13-8823-a02c02fd5485`; independent review found no
blocker. Frozen `balance-cache-inplace` node SHA256
`cb54f9bf150af6f2e33e608d8921ebb58944fbdb7ca00669bb7d1b3c1b82c48c` is recorded in the artifact
manifest; the new CachedBalance production
symbol was confirmed with nm. No speed result yet. The main tradeoff is scanning
all cached entries at flush, including clean rejected senders, and possible
extra entry padding; rejection-heavy workloads need consideration.

### Cache screening and first sustained pair

Short `s60-balance-cache-cap200-r1` is ACCEPT at38,620.0fills/s over131s,
drain29s, peakqueue66, commitp953686.4ms. Bracketing control
`s60-workers18-engine0-cap200-r2` is ACCEPT at38,764.1over130s, drain42,
peak63, p953205.1. Both agree quiescently with clean dissemination. The cache
change has not established an overall throughput gain. Against worker-controlr1,
load-window pass-B milliseconds/1000fills decreased from2.613/3.004/3.034 across
validators to2.281/2.714/2.701, but total engine cost did not consistently improve.

The sustained pair uses stage5 runner998d1eb, new generator0b6f9fa, nominal300s,
10markets, requested76000actions/s, cap200, ENGINE0/MATCH18/SETTLE18/BODYFETCH1,
uniform senders, cross.5/cancel.05/band5 and optional val1 depth observer ON.
The label suffix10m denotes ten markets, not ten minutes.

| Run | Runtime | Verdict | Fills/s | Actual seconds | Drain seconds | First120 fills/s | Commit p95 ms |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: |
| s60-sustained-base-10m-r1 | b60097a | REJECT | 30,773.4 | 310 | 107 | 40,272.0 | 5063.1 |
| s60-sustained-cache-10m-r1 | e4340d8 | ACCEPT | 34,563.5 | 313 | 167 | 40,358.4 | 4064.7 |

Both maintained liveness and established quiescent agreement. The baseline has
one val2 body-fetch exhaustion/sync fallback, so it cannot establish a healthy
speed comparison. Cache has clean dissemination but a longer drain; no promotion.
Complete four-point depth observations succeeded on both arms. Approximate
cross-market resting counts (sequential RPC reads, not atomic snapshots) were:

| Nominal offset seconds | Baseline resting orders | Cache resting orders |
| ---: | ---: | ---: |
| 0 | 0 | 0 |
| 100 | 832,100 | 840,688 |
| 200 | 1,375,197 | 1,503,449 |
| 300 | 1,789,380 | 1,941,510 |

Val0 actual sampled100s windows yielded baseline41,596/27,653/25,233fills/s and
cache40,518/36,248/27,713. Phase1 milliseconds/1000fills grew1.09/4.13/9.88 in
baseline and1.02/4.55/10.87 in cache. This supports investigating cancellation
growth, not attributing every Phase1 cost to it. Baseline whole-run engine960.65ms
included329.71ms Phase1; save236.55ms and flush442.29ms were also substantial.

In baseline snapshots100/200/300, two deep queues per side per market held
98.42%/99.72%/99.50% of resting orders alongside many thin queues. The old
all-batch cancellation gate can reject useful deep-level work when a thin or
sixth group is present; exact live activation is unmeasured. A new isolated
`perf/cancel-levels-viewbound` branch carries the prior candidate on the current
recovery base for a bounded per-level revision; no untested runtime is promoted.

Baseline val2 expired prefixiwk8LDI in current view1030 at11:24:02.415UTC and
handled matching-prefix responses about2.157s later. Handler timestamps do not
prove transport arrival timing. Ordinary body traffic shares the progress queue;
expiry precedes its next dequeue. Peer excerpts and a separate diagnostic branch
will distinguish remote serving from local queue delay before any routing change.

All completed databases above were inventoried then cleaned under authorization;
sustained baseline22.17GiB and cache27.13GiB. Logs, raw counters, digests, depth
snapshots, failure excerpts and frozen binaries remain retained.

### Sustained 50-market control and diagnostic qualification

`s60-sustained-base-50m-r1` used the same sustained baseline artifact, stage-5
runner and settings, changing only market count to 50. It measured 31,247.1
fills/s over 307 seconds, first120 34,653.2 and best60 38,870.1. Drain completed
in 169 seconds, liveness passed and quiescent state agreed. Val2 recorded one
body-fetch exhaustion and sync fallback, so the verdict is REJECT. Commit p95
was 4,608 ms; this is diagnostic evidence, not an accepted gain.

Val0 bench-plus-drain phase means per native block were engine 705.07 ms,
save-books 191.31 ms and flush 664.52 ms, against total block 1,659.83 ms.
Flush accounted for 40% of that measured total. These phase means include drain
and must not be described as load-only timings. The depth observer completed
all four snapshots. The completed databases occupied 30.37 GiB; inventory and
measurement artifacts were retained before their authorized deletion.

The isolated `diag/body-fetch-queue-time` candidate passed independent source
review. With the existing trace flag enabled, it records ordinary queue
admission, successful/missing serve lookup timing and every response-handler
entry using a shared process clock and full identities. Routing, retries and
validation are unchanged. Duplicate messages and future-view buffer redelivery
can make pairing ambiguous. Qualification is in progress; neither the failure
cause nor a performance improvement has been established.

### Instrumented body-fetch reproduction

Diagnostic runtime `bb3409b` passed all 95 HotStuff and 111 network library tests
and a separate node build (receipt `5053b605-8b19-4dff-8e40-8161e6d78ec3`). Frozen
node SHA256 is `7562c4cb4dac5dff7c1f46c49cdc5b6cf349bd859f399c5e5b6c05b77fd9e528`;
the scheduled generator remains unchanged. `s60-fetch-timing-10m-r1` uses the
same five-minute ten-market stage-5 shape with tracing and depth observation.
It measured 36,295.5 fills/s over 310 seconds, first120 46,390.4, best60 54,746.5,
drain 158 seconds and commit p95 4,020.9 ms. Liveness passed and state agreed,
but val0 and val1 each exhausted one body fetch: REJECT. Extra tracing makes
this a diagnostic cell, not an isolated throughput comparison. Its 30.16 GiB
completed databases were inventoried and cleaned; all evidence was retained.

The external `analyze_fetch_timing.py` passed 18 synthetic fixtures with stable
source hashes (receipt `c2bc8544-80d0-4be7-ac54-80dbb30478d3`). Complete logs parsed
without errors and all three process scopes were eligible. Unique observed
response pairs numbered 830/827/787; their admission-to-handler p95 was
14.139/13.226/14.200 ms. These statistics include startup, idle and drain and
exclude ambiguous repeated keys, so they do not characterize failure tails.
Selected body-message admissions had no drops and maximum ordinary queue depths
7/9/7; this does not measure the separate poller-to-algorithm queue.

The retained `s60-fetch-timing-10m-r1.expiry-traces.txt` gives two distinct cases:

- Val0 admitted response `GEu17YN...`, view 768, at 12:10:06.685894 UTC, expired
  its tracker at 06.694304, and handled the response at 06.694428. The next
  admission for that key was at 07.471367. A response was queued before expiry;
  the generic parser still conservatively excludes the repeated key from its
  latency distribution. This supports investigating message/timer ordering.
- Val1 expired `CaEbsdC...` in its current view 770 at 12:10:09.980130 UTC.
  Its first response admission followed at 10.060822, with handler entry at
  10.102073. Reordering already-queued messages alone cannot explain or prevent
  this case. The earlier serving/send/transport delay is not yet isolated.

Do not infer that either change is fixed, increase a timeout from these two
examples alone, or bypass the ordinary chain/genesis/view filters by rerouting
messages. A bounded scheduling experiment and separate delay attribution remain
under review. Cancellation correctness tests are being qualified independently.

### Locality control and cancellation candidate ready for live qualification

`s60-locality-base-50m-mps3-r1` used frozen baseline `b60097a`, the scheduled
generator and stage-5 runner `998d1eb`, with nominal 300 seconds, 50 markets,
`MPS=3`, depth observation and the same ENGINE0/MATCH18/SETTLE18/BODYFETCH1
settings. It is ACCEPT at 33,233.1 fills/s over 313 seconds, first120 43,064.2,
best60 48,046.8, drain 35 seconds and commit p95 4,746.5 ms. Liveness, quiescent
agreement and dissemination all passed. Locality changes the workload, and the
preceding uniform 50-market control failed dissemination; this is not a clean
speed-gain comparison.

All four depth snapshots completed. Their sequential cross-market resting
counts were 92 / 989,493 / 1,512,425 / 1,890,089 at nominal 0/100/200/300 seconds.
These are not atomic snapshots. Val0 bench-plus-drain phase means per native
block were engine 500.99 ms (phase1 52.75 ms), save-books 232.65 ms and flush
328.74 ms against block 1,172.56 ms. Completed databases occupied 22.07 GiB;
they were inventoried and cleaned while retaining the measurement evidence.

Cancellation runtime `f890e06` on `perf/cancel-levels-viewbound` is now tested,
committed and frozen as `artifacts/cancel-levels-lazy`. It independently groups
eligible deep queues and lazily allocates deferred output only when a deep
queue is selected; shallow removals share one queue lookup. The revised code
passed 248 core/integration/persistence tests (receipt `2bed8e38`) and a separate
node build (`bed520bd`). Positive production-symbol verification identifies the
new helper. Node SHA256:
`ce4c332c3590395f390706688ad228b92a1bb18245e20d0ae4dca4dfee8128ac`.

The unchanged seven-case micro screen retains deep-case headroom: paired ratios
2.017/3.515 for middle removals, 1.188 dispersed, 1.552 partial eligibility and
1.948 six-deep overflow. Deep under-count and shallow cases measured 0.989/0.982.
Shallow AA/BB controls were 0.924/1.050; noisy near-parity is not proof that all
regressions are gone. Full tables and provenance are in the candidate's
`docs/perf/cancel-level-compaction-2026-09-18.md`. No live cancellation throughput
result exists yet. The body-before-expiry candidate has separately passed
source review and is undergoing root-scheduled tests/build; no runtime is
promoted or combined on the basis of these mechanism results.


### Cancellation live screen and qualified body-receive experiment

`s60-cancel-lazy-10m-r1` is ACCEPT: 38,142.7 fills/s over 310 measured seconds
(nominal 300), first120 46,205.6, best60 48,825.8, drain 138 seconds and commit
p95 4,403.2 ms. It uses frozen `f890e06`, the scheduled generator and runner
`998d1eb`, with ten markets, cap200/rate76000, depth observation and unchanged
ENGINE0/MATCH18/SETTLE18/BODYFETCH1 settings. Liveness, quiescent agreement and
body dissemination passed. No baseline-relative gain is established yet; a
fresh identically shaped baseline is the next cell.

Four depth snapshots completed with resting counts 0 / 902,430 / 1,551,186 /
2,009,214. Val0 bench-plus-drain native-block means were engine 817.16 ms,
phase1 276.36 ms, save-books 243.62 ms and flush 339.86 ms, against block
1,501.93 ms. Completed databases occupied 29.33 GiB and were inventoried and
cleaned; logs, metrics, digests, provenance and frozen binaries remain.

The body-before-expiry experiment is separately committed at `110ebfc` and
frozen as `artifacts/body-before-expiry`, node SHA256
`f8bfeec80686abf4385a6907aef0c6146797e6822a56a7e98a77c68e850e168d`.
Receipt `3bcb0276` covers 112 HotStuff and 111 network tests plus a separately
built and positively identified node. The fixtures include the actual algorithm
path and deterministic empty-channel arrival, timeout and disconnect cases.
The default-off `TORUS_BODY_BEFORE_EXPIRY=1` policy gives a bounded receive
opportunity before pending-body expiry; it preserves message filters and
ordinary handlers. A same-binary OFF/ON live diagnostic remains pending.
It targets already-queued responses, not responses that first arrive after
expiry, and is not yet promoted.


The fresh `s60-cancel-control-10m-r1` baseline measured 32,072.9 fills/s over
309 seconds, first120 41,030.9, best60 46,285.2, drain 130 seconds and commit
p95 6,114.7 ms. Liveness and agreement passed, but val1 exhausted one body
fetch and val2 exhausted two, each with sync fallback: REJECT. Consequently,
the accepted cancellation candidate versus this rejected baseline does not
establish a healthy A/B throughput gain. All four depth snapshots completed
(0 / 885,806 / 1,415,629 / 1,842,191 resting orders). Val0 bench-plus-drain means
were block 1,799.25 ms, engine 1,004.78 ms (phase1 379.44 ms), save-books
264.18 ms and flush 415.70 ms. Evidence is retained. Recurring dissemination
failures make the already-qualified body-before-expiry same-binary OFF/ON
experiment the next priority.


### Body-before-expiry OFF/ON diagnostic pair

Both cells use the same frozen `110ebfc` binary and scheduled generator, with
the same 300-second ten-market stage-5 workload, cap200/rate76000,
ENGINE0/MATCH18/SETTLE18, BODYFETCH1 and depth observation. Only
`TORUS_BODY_BEFORE_EXPIRY` changes from explicit0 to1.

| Cell | Verdict | Full-load fills/s | Measured seconds | First120 | Best60 | Drain seconds | Commit p95 ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `s60-body-before-off-10m-r1` | REJECT: dissemination | 27,237.8 | 312 | 39,765.4 | 46,150.9 | 39 | 8,059.5 |
| `s60-body-before-on-10m-r1` | REJECT: dissemination | 35,749.6 | 313 | 44,308.8 | 50,485.0 | 148 | 3,763.7 |

Both passed liveness and quiescent agreement; val1 exhausted one body fetch
and used sync fallback in each. OFF idle probes were 2.5/2.8/0.8 blocks/s,
whereas ON started at25.2. This pair does not establish a healthy throughput
gain or resolve the dissemination issue. The policy remains default-off.

The unchanged conservative timing analyzer parsed both complete log sets with
zero errors and all process scopes eligible. Every expiry prefix resolved to
one observed full hash; repeated request/response keys remain ambiguous for
pairing. Small full-hash chronology excerpts are retained alongside the full
analysis reports:

- OFF val1 expired `oNntict...`, view308, at13:30:25.114178 UTC. Its first
  response admission stamp was25.557292, 443.114 ms later; handler entry was
 25.561744. Val2 had already completed a successful lookup for val1 at
 23.899721, but duplicates prevent assigning that serve to this response.
  The 1.658-second wall gap is not established transport latency.
- ON val1 expired `+5+sMYm...`, view842, at13:40:16.181039 UTC. Its first
  response admission stamp was16.202283, 21.244 ms later; the first handler
  stamp was16.342949. Again, the response was not admitted before expiry.

This pair reproduces the late-arrival class that the bounded receive-order
policy cannot alone prevent; it does not invalidate the exercised queued-body
behavior. The next diagnostic separates command enqueue/dequeue, serialization
and send-request initiation. The network loop's biased preference for commands
also leaves actual swarm polling/codec/delivery unresolved after initiation;
source alone does not prove starvation. Completed OFF/ON databases occupied
19.10/29.28 GiB and were inventoried and cleaned; all evidence remains.


### Finer settlement attribution qualified

Runtime `c43909f` on `diag/settle-passb-attribution` adds the default-off
`TORUS_SETTLE_PASSB_DIAG=1` diagnostic: disjoint position-merge, balance-apply
and trade-routing spans, residual validity and actual work counts. Planned
sender concentration is inventoried outside the existing pass-B timer; it is
not successful work or CPU share. Receipt `e56bcce8` passed five actual-loop
OFF/ON fixtures and sixteen parallel-engine/settlement integration tests, plus
a separate node build after cleaning alternate-worktree packages. Independent
review found no blocker. The identified node is frozen in
`artifacts/settle-passb-attribution` with SHA256
`d14e0c7f10c64bbdef0b61dba09d7f1aa65d8b8aa193fb75e86a8239a909f4c9`.
Live attribution is still pending; no performance improvement is claimed.


The separate cancel-all integer-margin candidate is committed at `f2a7164` on
`perf/cancel-margin-integer`, based on accepted queue-compaction `f890e06`.
It hoists borrowed config lookup once per nonempty cancelled market and
replaces scaled general division with exactly equivalent raw integer-leverage
division. Per-order multiplication, tier selection, rounding, ordered addition,
clamps and zero-leverage panic/mutation boundaries remain unchanged. Receipt
`3a69134a` passed 47 arithmetic, real-StateDb oracle, matching and persistence
tests plus a separate node build. Independent review found no blocker. The
identified node is frozen in `artifacts/cancel-margin-integer`, SHA256
`9d5072ae89c4709c0715e6dfddc4f02f9dadfadc02e191ea045a2fe1e305c327`.
No microbenchmark or live performance gain has yet been measured for this step.


### Accepted pass-B attribution and outbound diagnostic readiness

`s60-settle-passb-10m-r1` is ACCEPT at 35,933.6 fills/s over 312 seconds,
first120 40,271.9, best60 44,491.2, drain 185 seconds and commit p95 3,865.1 ms.
All validators passed liveness, agreed and had clean dissemination. This is
an instrumented nominal 300-second ten-market run of frozen `c43909f` with
`TORUS_SETTLE_PASSB_DIAG=1`, BODYFETCH1, DEPTH1 and unchanged ENGINE0/MATCH18/SETTLE18,
cap200/rate76000. It is diagnostic evidence, not a speedup claim.

The qualified offline transform (`f0bda1ff`, script SHA256 prefix `df596ce3`) parsed 271
complete invocations on each validator with zero parse errors or invalid timing
partitions. Bench-plus-drain completion-log windows give:

| Validator | Whole pass B ms/invocation | Position merge | Balance apply | Trade route | Residual | Inventory outside B |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| val0 | 166.54 | 5.51 | 30.80 | 81.40 | 48.83 | 15.19 |
| val1 | 182.95 | 8.08 | 28.21 | 92.43 | 54.22 | 13.12 |
| val2 | 194.09 | 7.66 | 31.64 | 94.96 | 59.83 | 11.56 |

Trade routing is 48.9–50.5% of pass B; balances 15.4–18.5%, merging 3.3–4.4%,
and residual 29.3–30.8%. Each validator reports 15,524,527 routed fills and
16,035,318 balance attempts with no read errors or failed orders. Median
per-invocation planned top-one/top-four sender event fractions are 0.57%/2.26%;
these describe planned event concentration, not CPU share or a sharding speedup.
Inventory and detailed clocks perturb execution. Completion-log boundaries
differ from metric snapshots. The 271 invocations also differ from 285 native
blocks: val0's existing metric reports pass B 158.42 ms/native block inside engine
993.80 ms, with phase 1 482.02 ms. Phase1 remains a larger total-engine cost.

Source review identifies three fixed-key-to-Vec allocations per deferred fill
in trade routing. Preserving fixed-size keys through the existing writer is
therefore the next scoped settlement experiment; aggregate routing time does
not prove allocation is its whole cost. The 29.21 GiB completed databases were
inventoried and cleaned, retaining all diagnostic evidence.

Outbound diagnostic runtime `14449bc` is now qualified, committed and frozen in
`artifacts/body-send-stages`, node SHA256
`248cb4ba552cd65e23d5f5207e6736bbefb6ac63dfac4e4a207cc0ebc737536e`.
Receipt `249cf648` passed 118 network and 112 HotStuff tests plus a separate node build.
The default-off `TORUS_BODY_SEND_TRACE=1` records local enqueue/dequeue, inner
serialization, request initiation and tracking, without changing wire/routing
or biased selection. Its offline analyzer passed 17 synthetic fixtures with
explicit pre/post external hashes under receipt `f0bda1ff`. Producer emission
may follow consumer completion; local IDs and startup scopes identify joins.
Actual queue residence has only a conservative zero lower bound, and initiation
is not delivery. A live outbound diagnostic follows the completed settlement run.

### Outbound diagnostic result and next candidates

`s60-body-send-stages-10m-r1` completed with PASS/AGREE and a 134-second drain,
but strict REJECT: val2 exhausted two body fetches and fell back twice. Its
33,631.9 fills/s over 312 seconds, first120 42,903.3, best60 47,987.3 and commit
p95 4,037.5 ms are diagnostic observations, not an accepted performance result.
This nominal 300-second ten-market run used frozen `14449bc`, scheduled generator
`9c210f31`, runner `998d1eb`, cap200/rate76000, BODYFETCH1/BODY_SEND_TRACE1,
BODY_BEFORE_EXPIRY0, DEPTH1 and ENGINE0/MATCH18/SETTLE18. Completed databases
occupied 27.22 GiB; logs, metrics, digests, binaries and analyses were retained
before authorized cleanup.

The qualified outbound analyzer parsed 19,610 diagnostic records without parse
errors. Whole-log command counts were 3,212/3,215/3,379 for val0/1/2; one accepted
enqueue on each of val1 and val2 lacked a consumer record and remains censored.
These windows include idle and drain. Observed pre-enqueue to dequeue p95 was
12.648/10.177/16.060 ms, with maxima 410.411/164.690/521.373 ms. Inner encode p95
was 18/23/22 microseconds and send-request initiation p95 7/8/7 microseconds.
The handoff interval includes preparation and possible post-pop descheduling;
it is an upper bound, not an exact queue residence measurement.

For the val2 view-700 expiry (`SnfiUus...`) at 14:30:05.530491 UTC, the first
observed response admission was 5.542 ms later. Val0 response command 17414
spent 410.411 ms between pre-enqueue and observed dequeue, then completed
send-request initiation at 14:30:04.763468. Another response was initiated at
04.766797. Repeated responses prevent unique wire pairing, and the remaining
gap includes unmeasured swarm polling, outer codec, delivery and receiver
pre-admission work. It is not established transport latency or proof of
biased-select starvation. For view 702 (`ptxdd2e...`), expiry was 14:30:08.310256
and first observed response admission 08.509603, 199.347 ms later; the first
retained response enqueue toward val2 was already after expiry at 08.489888.
The full analyses and the 266-line expiry excerpt remain in the campaign.

Independent excerpt review further finds that val2 initiated view-702 requests
toward val1 at 14:30:07.188390, 07.296100 and 07.396046. Val1 had successfully
served the same body to val0 by 07.438821, yet its first retained request admission
from val2 was 08.486709, after expiry. Lookup then took four microseconds. Val0
had admitted a request from val2 at 07.528335, but its first retained serve to
val2 began at 08.591044 (12-microsecond lookup). These repeated-key endpoint
chronologies support neither global body unavailability nor slow lookup as the
explanation; they do not uniquely pair individual request instances. A next
diagnostic should observe actual swarm poll attempts, including Pending, and
receiver-local decode/admission boundaries before changing scheduling policy.

The separately qualified `f2a7164` integer-margin candidate is next in the same
300-second ten-market workload. A source candidate on top of it,
`perf/deferred-trade-fixed-keys`, preserves fixed-size trade keys through the
background writer under exact default-off `TORUS_DEFERRED_TRADE_FIXED_KEYS=1`.
Independent review found no blocker; runtime qualification and same-binary
OFF/ON measurements remain pending. It preserves the existing durability
contract and has no abrupt-crash qualification or speed claim.

### Integer-margin live result

`s60-cancel-margin-10m-r1`, frozen `f2a7164`, is ACCEPT: 38,610.6 fills/s over
307 seconds, first120 44,369.9, best60 54,776.2, drain 134 seconds and commit
p95 3,931.3 ms. All validators passed liveness, agreed and had clean dissemination.
The nominal 300-second ten-market workload retained cap200/rate76000,
BODYFETCH1/DEPTH1, ENGINE0/MATCH18/SETTLE18, runner `998d1eb` and generator
`9c210f31`. Peak execution queue was 60. Completed databases occupied 30.98 GiB
and were cleaned after evidence retention.

The prior accepted `f890e06` cancellation run was 38,142.7 fills/s over 310 seconds.
This single subsequent result is only about 1.2% higher; it does not establish
a repeatable margin-arithmetic gain. Val0 bench-plus-drain engine cost was
752.68 ms/native block, phase1 272.97, passB 137.65, save-books 212.91 and flush
320.79. Late-window phase1 still grew to 457.4 ms, so sustained deep-book cost
remains. The 54.8k minute is not a full-window record, and this five-minute cell
is not directly comparable to the historical 51.6k approximately two-minute run.

Fixed-key runtime qualification now follows this cell. The next network
diagnostic remains a separate branch; no network scheduling policy is changed.

Fixed-key candidate `0571fba` is now qualified and committed. Receipt `fc92801b`
passed five focused bridge tests, 12 background-writer tests, 19 bridge integration
tests OFF and the same 19 ON, and the consensus deferred-writer test ON (56 test
executions, 37 unique tests), followed by a separate node build. Independent
review found no blocker. Frozen `artifacts/deferred-trade-fixed-keys` has node
SHA256 `b6f804a23bcbcf10b5f57018c0d94b86af6947231fbb236161e0a4d3dbfd9d1c`
and scheduled generator `9c210f31`. Exact flag plus typed-writer and integer-margin
helper symbols establish binary identity; the optimized drain helper has no
standalone symbol. Same-binary OFF/ON live evaluation follows. The feature stays
default off, with no abrupt-crash qualification or performance gain asserted.

### Fixed-key OFF/ON result and session stop

Both `s60-fixed-keys-off-10m-r1` and `s60-fixed-keys-on-10m-r1` are ACCEPT:
liveness PASS, AGREE, completed drain and clean dissemination on all validators.
The frozen `0571fba` node and scheduled generator hashes, runner `998d1eb`,
ten-market genesis, nominal 300-second workload, cap200/rate76000,
BODYFETCH1/DEPTH1 and ENGINE0/MATCH18/SETTLE18 match. Only
`TORUS_DEFERRED_TRADE_FIXED_KEYS=0/1` differs in the node configuration.

| Observation | OFF | ON |
| --- | ---: | ---: |
| Full-load fills/s | 32,786.3 | 40,819.2 |
| Actual load window (s) | 312 | 308 |
| First120 fills/s | 40,209.9 | 41,764.7 |
| Best60 fills/s | 46,574.5 | 56,509.4 |
| Drain (s) | 105 | 104 |
| Commit interval p95 (ms) | 4,187.0 | 3,779.7 |
| Peak execution queue | 66 | 54 |
| Val0 engine ms/native block, bench+drain | 816.68 | 651.81 |
| Val0 pass-B ms/native block, bench+drain | 155.80 | 104.01 |
| Val1 pass-B ms/native block, bench+drain | 163.76 | 119.40 |
| Val2 pass-B ms/native block, bench+drain | 162.74 | 114.85 |
| Val0 engine ms/1,000 fills, bench+drain | 15.70 | 11.97 |

The observed full-window difference is +24.5%. This is a promising single pair,
not a repeatable gain claim: matching, save-books and flush costs also changed,
and the preceding separate `f2a7164` margin run was already 38,610.6 fills/s.
Do not attribute the entire difference to allocation removal. Repeat ON then
OFF before promotion. The 56,509.4 best minute is supplemental; it is not a
full-window record or an apples-to-apples comparison to historical 51,608.1 over
about two minutes. No default switch or abrupt-crash qualification was made.

Sampled trade-writer queue maxima were 2 OFF and 3 ON; final snapshots were 1
and 2 respectively on every node. This gauge is updated at application handoff,
not continuously by the writer, and is not part of the harness's execution-drain
gate. These values neither prove stuck writes nor establish actual writer
completion; orderly writer drain/reopen is covered by the qualification tests.
OFF/ON databases occupied 25.81/31.66 GiB and were cleaned after evidence retention.

The owner requested stopping after this next step and wrapping up. No additional
benchmark or build was started after ON. Resume with a reversed fixed-key repeat,
then deeper books (`CROSS_FRACTION=.25`) and the 300-second burst schedule
`0:30000,60:120000,120:0,240:30000`. Use generator phase timestamps and recovery
phase `index == 2` to measure catch-up during the pause, not final drain.

`/home/18c/projects/wt/swarm-poll-stages` on `diag/swarm-poll-stages` retains four
uncommitted diagnostic source/doc paths based on `14449bc`. Independent review
found no blocker, but its nine authored runtime fixtures, full network suite and
node build are UNRUN. The external `analyze_swarm_poll.py` and
`test_analyze_swarm_poll.py` have 20 authored fixtures, source review passed, and
are also UNRUN. The campaign's `pending-swarm-poll-source.tar.gz` and
`pending-swarm-poll-source-manifest.json` preserve the exact six files and hashes.
Runtime issue `8428153c`, attempt `52de45d1`; parser issue `2c565549`, latest
attempt `31f400d8`. Qualify these before any live use. Shared-target contents are
the fixed-key build, so clean affected alternate packages including `torus-state`
before the next network build. Keep node and generator builds separate.

### Resumed campaign: 2026-09-19

The owner resumed steps 1–8 with a nine-hour limit, starting 01:50:04 UTC and
ending no later than 10:50:04 UTC (12:50 Berlin), or earlier if usage runs out.
Project-only stale-build/log/database cleanup is explicitly authorized.

The first reversed-order cell, `s60-fixed-keys-on-10m-r2`, is REJECT:
3,431.5 fills/s over 305 seconds, first120 6,241.4, best60 12,482.8.
It used the same frozen runtime, generator, runner and workload as r1. Liveness
failed on all nodes, including observed commit gaps up to 181 seconds. Body-fetch
exhaustions/fallbacks were 7/7, 6/6 and 7/7 on val0/1/2. The generator exited
successfully and final validator state agreed, but these do not establish
healthy execution of the offered load. Nonce-expired mempool evictions were
144,228 / 112,011 / 141,996 of 152,512 submitted actions.

The live drain probe reported quiet advancing counters after about 66 seconds
(70-second harness interval). The summary deliberately marks effective drain
false when load-plus-drain liveness fails; this is an existing acceptance rule,
not contradictory raw measurements. Keep the REJECT verdict unchanged.
Completed databases occupied 10.34 GiB and were removed after evidence retention.
The OFF r2 control follows. This failed ON cell does not isolate fixed keys as
the cause, and the feature remains default OFF.

`s60-fixed-keys-off-10m-r2` is also REJECT: 36,646.7 fills/s over 307 seconds,
first120 52,703.3, best60 58,836.9. Liveness PASS, drain 70 seconds, AGREE,
but dissemination failures prevent acceptance. Thus the reversed pair contains
no accepted performance comparison and provides no basis for default promotion.
Its completed databases occupied 30.09 GiB. Deeper-book and burst screening
will use fixed keys OFF while the network diagnostic is qualified separately.

`s60-deeper-fixed-off-10m-r1` is ACCEPT: 17,625.2 fills/s over 306 seconds,
first120 24,948.6, best60 30,925.8; drain 176 seconds, PASS/AGREE and clean
dissemination. Only the crossing fraction changes to 0.25 from the fixed-key OFF
workload; fixed keys remain OFF. This is a distinct workload, not a comparable
optimization gain. The 23.57 GiB completed databases were retained then removed.

Val0 bench-plus-drain engine averaged 1,050.41 ms/native block, including
711.10 ms in phase1 actions; save-books was 262.00 ms and flush 288.01 ms.
Early-to-late 60-second load windows showed phase1 increasing 122.5→805.4 ms
while matching fell 110.1→83.3 and settlement 140.4→100.5 ms/native block.
The early and late windows contained 64 and 41 native blocks respectively;
resting-order gauges at their ends were 1,903,852 and 4,459,037. These observed
phase timings prioritize finer cancellation/action attribution, not a claim
that every phase1 nanosecond is book removal. No RocksDB write-stall time was
reported. The writer queue gauge remains outside the strict drain gate.

`s60-burst-fixed-off-10m-r1` is ACCEPT: 32,008.5 fills/s over its entire
304-second load interval (including the intentional pause), first120 44,021.8,
best60 59,763.3; final drain 86 seconds, PASS/AGREE, clean dissemination.
The configured schedule is `0:30000,60:120000,120:0,240:30000` with fixed keys
OFF and the ordinary 0.5 crossing fraction. Requested rates are not achieved
admission or matching rates. Completed databases occupied 23.34 GiB.

Generator-anchored phase index 2 has valid sampled recovery evidence: confirmation
48.47 seconds after pause start, with a common quiet span of 10.99 seconds and
commit deltas 253/252/254 on val0/1/2 wholly inside that span. No renewed activity
was observed before the pause ended. This establishes one sampled pause recovery,
not permanent absence of in-flight work, writer completion, or sustained 120k
execution. The phase evidence does not change the ordinary acceptance gate.

### Swarm diagnostic qualification and first live result

Runtime `080c4fa` passed 127 network library tests and a separate node build
under receipt `cb235577`. Its external analyzer passed 20 fixtures with hashes
checked before/after; the receipt in the analyzer issue's scope is `8a1d4cb6`.
The frozen node SHA256 is
`4c325452a0df310776c56a7e47b523af6906d4a4022e9f091144a3e20497f386`.
It is based on the body-send diagnostic branch, not the later fixed-key runtime;
do not attribute a cross-binary throughput difference to this instrumentation.

`s60-swarm-poll-10m-r1` is ACCEPT: 41,723.7 fills/s over 306 seconds, drain
176 seconds, PASS/AGREE and clean dissemination. BODY_FETCH_TRACE,
BODY_SEND_TRACE and SWARM_POLL_TRACE are ON; BODY_BEFORE_EXPIRY is OFF.
No network selection policy was changed. The parser accepted all 21,063 records
with zero errors and all three process scopes eligible. Its full analysis and
separate load-window extraction are retained beside the manifest. Completed
databases occupied 34.92 GiB and were removed after retention.

For fully contained load-window poll aggregates, maximum completed interpoll
gaps were 453.165 / 451.592 / 503.732 ms on val0/1/2. These maxima describe
previous-poll-return to next-poll-entry, not pure CPU or transport latency.
Actual poll-call maxima were 352.121 / 294.779 / 338.398 ms, including any
descheduling inside calls. Sampled event-to-admission p95 was 52 / 46 / 48 us;
maxima were 1.768 / 9.416 / 37.203 ms. Queue-lock and validation-tee p95 were
at most one microsecond. The receiver observations are rate-limited, and their
start excludes outer codec/dispatch. Whole-log gaps near four seconds include
quiet drain intervals. This healthy cell does not reproduce the earlier expiry
failure or prove biased-selection starvation; scheduling defaults stay unchanged.

### Cancellation attribution and next experiment

Default-off `TORUS_CANCEL_ALL_DIAG` runtime `cad99c7`, based on `0571fba`,
passed 49 test executions (39 unique) and a separate node build under receipt
`bc2febfa`. Frozen node SHA256:
`c99af0289f10fb79810e4fd14954c12c7f6d53c78cf7f51068475ce5f97740f2`.
`s60-cancel-attribution-deeper-r1` is ACCEPT: 17,009.1 fills/s over 308 seconds,
drain 137 seconds, PASS/AGREE, clean dissemination, crossing fraction 0.25 and
fixed keys OFF. The 27.46 GiB completed databases were removed after retention.

Across complete logs, each node recorded 7,329 completed cancel-all calls in
275 blocks and 4,327,789 removed orders. Every timing partition was valid and
no balance-read/write error was recorded. Book removal accounts for
99.39% / 99.33% / 99.38% of cancel-all elapsed time on val0/1/2; margin
calculation accounts for 0.24% / 0.33% / 0.26%, balance handling 0.26% / 0.24% /
0.21%. These are fractions of completed cancellation calls, not total engine
time or CPU-only samples. Logging follows each elapsed timestamp.

A default-off deep-compaction candidate is being tested on a separate branch.
It changes eligibility and movement-cost thresholds only for queues at least
32,768 orders deep, using the existing stable compaction algorithm. Correctness,
balanced microbenchmarks and same-binary chain comparisons are required before
any promotion. No gain has been established.

### Resource isolation and the route toward 200k

Builds, test suites, database deletion, and full-log analysis are scheduled outside
live load/drain intervals. The burst run ended at 02:32:04 UTC before network
verification began at 02:32:36; the swarm run ended at 02:47:12 before cancellation
verification began at 02:47:43. Source inspection/editing during a run is light
activity, but this remains a shared host, not an exclusive-hardware experiment.
Offline analysis did overlap compilation after the swarm benchmark had ended.

The current evidence does not establish sustained 200k fills/s or a hardware
ceiling. Longer queues increase serial phase1 cost in the deeper workload;
completed cancel-all calls overwhelmingly spend time removing book orders.
Changing margin/balance handling alone therefore cannot materially shorten those
calls. Matching and settlement already have parallel execution paths; increasing
those worker counts is not a demonstrated fix for this serial action cost.

The next architectural experiment should target queue removal representation or
safe action partitioning across markets. A representation change must preserve
FIFO, canonical cancellation output, row journals, incremental commitments and
recovery. Market partitioning must account for shared trader balances, order and
error precedence, and deterministic merge order; treating markets as independent
without those checks would be unsound. Neither design is implemented or qualified
by this campaign. Prototype against the retained workload shapes and serial
oracle before chain comparisons, then require matched-duration/order repeats,
deep/burst screening, forced replay and snapshot/state-sync checks.

Dedicated validators on separate machines remain a separate measurement needed
to distinguish shared-host contention from algorithmic cost. No machines were
provisioned. Network expiry remains a reliability gate: a healthy diagnostic run
is not a reproduced fix for the failed reversed pair. Keep fixed keys and the
execution pipeline default OFF pending their own acceptance and recovery gates.


### Positive replay with a remaining liveness failure

`s60-pipeline-replay-120-r1` used the frozen fixed-key runtime with fixed keys OFF
and pipeline ON, duration 120 seconds and guarded val1 kill at +60. Restart
observed durable height 692 and committed height 693, replayed one block, then
attached the flush worker at 693. No replay holes, panic/fail-stop or error lines
were recorded. The crash-specific gate is PASS and final state agreement is
AGREE: all three nodes had digest
`55dbac96f5a5713bff80c96c79cb7029fd4ce3dcddb285a18ee11eb66ff1fbf1`.
The restarted node's reset counters are excluded only from counter comparison.

The complete cell remains REJECT: val0 had a 30-second commit gap and val1 a
51-second gap. The raw drain probe reached quiet advancing counters after
66.83 seconds (71-second harness interval), but effective summary drain remains
false under the liveness rule. Its 34,245.1 fills/s over 125 seconds is not an
accepted throughput result. This is positive replay plus final convergence,
not a healthy recovery qualification, controlled C1 pending-parent proof, or
power-loss durability test. Pipeline stays default OFF. The 12.54 GiB completed
databases were removed after evidence retention.


### Deep cancellation threshold experiments

The first candidate, isolated commit `c2526d9`, combined an eight-target gate at
depth >=32768 with a movement budget of one queue length. It passed 127 core and
10 enabled margin tests (`2997f617`), but the balanced release microbenchmark
rejected the combined policy. Current/candidate paired ratios for dispersed
8/16/32 targets were 0.728/0.908/0.948; endpoint eight-target cases were about
2.1. Two independent fixture seeds, 16 pairs, all construction/preparation/
execution orders and AA/BB controls were retained. No node was built for it.

The second candidate preserves the existing four-length movement budget and
changes only the deep eight-target eligibility. Its full core suite passed
127 tests (`a960e68c`). Fresh balanced microbenchmark ratios for dispersed
8/16/32 targets were 0.999/1.000/0.973; endpoints were 1.798/1.597. Endpoint
controls remain noisy (back AA 0.782), so these are screening signals, not robust
chain gains. Micro logs, manifests, frozen test binaries, hashes and all order
strata are retained in `deep-compaction-micro-r1` and `deep-compaction-micro-r2`.
The second candidate will receive enabled integration checks and a separate
node build before a same-binary live OFF/ON comparison. It remains default OFF.
