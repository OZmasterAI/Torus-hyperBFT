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
| 1. Body retrieval and recovery | Authenticated body/sync fixes plus view-bound header voting and corrected future-buffer accounting are committed and tested. One later restart drained and agreed; an earlier run replayed one block but stalled. | Identify the earlier stall's cause and complete healthy positive-replay/C1 qualification. |
| 2. Healthy baseline | One accepted recovery control at 49,242.8 fills/s; a second was UNVERIFIED because of a sampling gap. | Three accepted repeats on the final frozen runtime; latest view fixes have no full baseline cell yet. |
| 3. Proposal construction | One accepted hash-cache candidate at 51,608.1 fills/s, with lower construction time but larger backlog. Default-off DA reuse and finer timers are tested and frozen. | Repeated hash comparison and same-binary DA OFF/ON cells before promotion. |
| 4. Largest remaining cost | Execution/flush timing and growing cancellation cost motivate a bounded cancellation candidate; local mechanism gains repeated. | Resolve gated-path fallback regressions/control noise, then measure live eligibility and matched-rate/latency A/Bs. |
| 5. Sustained and varied load | Balanced locality generator, workload manifests and burst schedules passed tests. | Depth preparation, phase scoring, and fresh sustained/deep/multimarket/burst runs. |
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
