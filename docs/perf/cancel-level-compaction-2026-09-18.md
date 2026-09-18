# Adaptive cancel-all queue compaction candidate

Based on `3cc958b`, isolated branch `perf/cancel-level-compaction`. This is an
independent execution-path hypothesis, not evidence that cancel-all currently
limits chain throughput.

The baseline performs one `VecDeque::remove` per target, repeatedly moving
survivors when a trader has many orders in a deep level. The earlier `4298728`
candidate always swept touched levels, hashed every survivor and allocated a
replacement queue. It shipped together with deferred book-save sidecars, and
the combined candidate regressed; its measurements do not isolate cancellation.

The frozen single-level baseline `f2654bc` groups only sets of at least32 targets whose index locations all
prove they share a single level with at least 1024 orders. Small cancellations
and globally shallow books reject the gate with two length checks. A sparse
first target level rejects after one index and level lookup, before any grouping
allocation or scan of remaining IDs. Mixed levels conservatively use the baseline
path even if some subset could benefit. The 32-target minimum keeps the measured
winning cases; the 1024-depth threshold is still a hypothesis to measure.

Eligible sets are grouped with their original return-vector positions.
Queue positions are resolved once. For each level, it estimates the element
movement from ascending removals and descending removals, accounting for the
shrinking queue. If the cheaper estimate exceeds the level length, it uses
`VecDeque::retain` to compact in place. Otherwise it removes in the cheaper
direction. Front prefixes and tail suffixes therefore need no full scan.
Compaction clones only canceled orders; survivors need no hash-table lookup or
replacement queue allocation.

Both paths emit canceled orders in the original trader-index order. FIFO
survivors, index cleanup, stop retention, per-order epoch increments, row/level
journals and dirty chunks retain baseline semantics. Missing order-index IDs
are skipped. No persistence formats, matching policy or consensus inputs change.

The differential oracle is the baseline algorithm, retained from the earlier
candidate's tests. Tests compare ordered returns, queues, indexes, sequence
numbers, epochs, dirty chunks, stop rows and encoded row/level images. The random
matrix checks after each cancellation, in flat, cached-flat and chunked modes.
Shape tests cover sparse/dense front, back, middle and dispersed removals from
wrapped queues; reverse the trader index; prime caches; and append/save afterward.
These compare exact root-input bytes, not an end-to-end chain state-root run.

The ignored release microbenchmark now uses paired AB measurements plus AA
and BB controls, where A is the test-local baseline and B is the frozen
candidate. Each comparison gets sixteen samples: two independently seeded
fixtures times all eight combinations of construction, preparation, and
execution order. Comparison order rotates too. A test-only logical clone copies
all book fields and preserves each map/set RandomState from a shared unprepared
fixture; flat sponge entries must still be empty. Both clones then undergo
persistence preparation, with its order balanced independently. Hash seeds vary
across the two fixtures and across invocations, not between paired arms.

Symmetric non-inlined wrappers and a shared timer call black-boxed function
pointers. This reduces call-site specialization; it does not establish identical
internal codegen or turn the test-local oracle into a production baseline
artifact. Both result vectors survive until both timers stop. Setup, encoding,
assertions and drops remain outside timing. CSV reports arm medians, median
paired left/right ratios, and separate construction/preparation/execution order
strata. AA and BB should be near one before interpreting AB. Material control or
order-stratum skew is evidence to investigate, not a compaction win/loss.

Eight representative scenarios replace the broad Cartesian matrix: historical
flat-cache depth8192 one-target/distinct-level and 32-target/middle cases;
chunked mode3 shallow32/four targets; chunked depth8192 one-target/distinct-level,
32-target/middle and 200-target/middle; and five deep levels each8192 orders with
8 or16 middle targets PER LEVEL (40/80 cancellations total). The five-level
cases asserted the baseline all-one-level gate was inactive. At the revised
source, the16-target case asserts the new gate is active; the8-target case
remains fallback. Historical r1 timings refer exclusively to `f2654bc`. This remains an engine microbenchmark, not a
simulation of the full ten-market workload.

A separate ignored `cancel_all_compaction_forced_grouping_microbenchmark`
adds experimental arm C. It performs the same initial trader-index removal,
then directly invokes the existing `cancel_all_by_level`, bypassing the
production all-one-level gate in test code only. Four mode3 cases use five
levels with8/16 targets per level at8192/32768 depth per level. Comparisons are
`AC_forced`, `AA`, `CC_forced`, using the same balanced sixteen-sample machinery.
This separate entrypoint keeps the deeper experiment explicitly schedulable;
its results cannot be labelled as production candidate behavior.

`cancel_all_compaction_forced_five_levels_matches_naive` checks those four
shapes before timing, covering bids and asks, reversed cross-level target order,
primed row/chunk persistence, target and other-trader pending stops, exact
cancelled-order returns, remaining FIFO queues, indices, epochs, dirty chunks,
journals and complete persisted row/level images. Baseline gate inactivity was asserted for each fixture; the revised tests
expect activation only at16 targets per level. These new checks passed in the parent-run focused suite described below.

Background trader addresses encode their group in eight bytes with a distinct
prefix, avoiding the prior u8 wrap/collision above50800 orders. Each background
trader still owns at most200 fixture orders. The ordinary differential fixtures
use this address helper too; the production cancellation algorithm is unchanged.

The first revision (group every set of at least four) passed the parent-run core
suite: 114 passed, 4 ignored. Its isolated microbenchmark failed performance
acceptance. At depth 32 with at least four targets, old/new median time ratios
were 0.648–0.892; depth 8192/four targets gave 0.834 for middle and 0.910 for
dispersed layouts. Depth 8192/middle gained 2.518x at 32 targets and 5.906x at 200.
These are local cancel-all measurements, not chain throughput.

Evidence: `/home/18c/bench-results-matched/s60-campaign-20260918/` files
`cancel-core-tests.log` and `cancel-microbench-r1.log`. Torus records the first
attempt as failed acceptance and the conservative gate as a new unverified
attempt. The later gated r2/r3 matrix included the 1024-depth boundary and
8/16-target fallback cases; that matrix has now been replaced as described above.

Parent-run gated microbenchmarks r2/r3 repeated deep middle gains, but also
reported apparent improvements on untouched fallback paths: depth1024/count1/
middle old/new2.176/2.054, and depth1024/count32/distinct-levels1.720/1.470.
Depth8192/count1/distinct-levels regressed0.830/0.864. These ratios are confounded
by the prior fixed construction/preparation order, independent hash seeds, odd
sample count and oracle/codegen differences. They do not isolate compaction and
support no production performance claim. Preserve both original logs as evidence.
The parent-run focused suite passed all eight differential tests, receipt
`a10d44c9-a33d-4d2b-a573-4b8584ae6c6b`. The two ignored microbenchmarks each
completed successfully; their diagnostic timings are recorded below.

Torus fairness issue `55637b2f-82d9-4f0b-90a9-e6e536a924bc`, attempt
`64564f15-1d3f-4e1e-aea5-a8af579dcfcd`. The parent coordinates all test and benchmark execution:

```sh
cargo test -p torus-core --lib cancel_all_compaction -- --test-threads=1
cargo test -p torus-core --test level_rows_core_tests --test determinism --test fuzz_tests
cargo test --release -p torus-core --lib cancel_all_compaction_microbenchmark -- --ignored --nocapture --test-threads=1
cargo test --release -p torus-core --lib cancel_all_compaction_forced_grouping_microbenchmark -- --ignored --nocapture --test-threads=1
```

No performance acceptance or chain throughput win is claimed. Production
adoption still needs a demonstrated benefit on the default five-level workload
and actual matched/s, not only a concentrated one-level mechanism win.

## Frozen single-level baseline: fair r1 measurements

Artifacts under `/home/18c/bench-results-matched/s60-campaign-20260918/`:
`cancel-fair-r1.log` and `cancel-forced-five-r1.log`. The second repeat was still
running when this baseline was recorded; none of its results are claimed here.
Values below are median PAIRED left/right time ratios, not ratios of the
separate arm medians.

| Case (depth/targets per level) | Comparison | Ratio | AA | BB or CC |
| --- | --- | ---: | ---: | ---: |
| mode3 single level8192/32 middle | AB | 2.657 | 0.969 | 1.056 |
| mode3 single level8192/200 middle | AB | 5.868 | 1.009 | 1.028 |
| mode3 five levels8192/8 middle | AC forced | 1.134 | 1.047 | 1.035 |
| mode3 five levels8192/16 middle | AC forced | 1.648 | 0.999 | 0.973 |
| mode3 five levels32768/8 middle | AC forced | 1.277 | 1.002 | 1.078 |
| mode3 five levels32768/16 middle | AC forced | 1.971 | 1.028 | 1.036 |

The frozen baseline production gate rejected both five-level shapes: AB ratios were
1.059 and1.032 at8192/8 and8192/16. Forced C bypasses this gate in test code only.
Its16-target gains warrant an actual gated experiment, not promotion. Eight-
target gains are smaller and exposed to control variation. Sparse/distinct-
level controls remain badly skewed: flat-cache AA1.156/BB1.814, mode3
AA1.348/BB1.175. Their apparent AB regression/gain is unresolved. This does not
establish default-band5 activation frequency, sustained engine improvement or
chain matched/s. The exact r1 source is committed as a reviewable baseline
before the separate multilevel gate change.

## Bounded multilevel extension: rejected on performance

A separate attempt preserves the >=32/depth1024 single-level gate. At the first
mismatching level, it considers multilevel grouping only for80..200 total
cancellations and first-level depth>=8192. A fixed stack array counts at most
five `(side, price)` groups; every new level must have depth>=8192, and every
group must contain>=16 targets. Missing locations, a shallow group or a sixth
group reject. There is no allocation during classification. At most201 index
probes occur for a multilevel attempt (the first mismatch is re-read), and at
most five level lookups; resulting grouped allocations remain bounded by200
targets and five groups. Existing synthetic single-level fixtures retain their
prior behavior independently of the new multilevel cap.

The40-target/five-level case still rejects at its first mismatch. This narrow
extension tests the stronger16-per-level forced signal; it does not assume
activation is frequent under default band5 or improve the8-per-level case.

New differential tests compare front/back/dispersed five-level cancellations
against the exact baseline, and fallback cases with15/16/16/16/17 targets,
one8191-deep later level, and six deep levels. Gate-only checks cover depth
8191/8192, targets per level15/16, total79/80/81/200/201 and two/five/six groups.
The separate `cancel_all_compaction_multilevel_microbenchmark` measures the
actual gated AB path with AA/BB controls on five-level middle8192/32768, front,
back, dispersed and the three fallback shapes. Parent verification receipt
`d30c35c4-821e-43be-99c6-393ed1ab5966` passed all11 focused tests. Its actual-gate
microbenchmark completed; the performance result below rejects this revision.
Earlier fair/forced r1 logs remain tied exclusively to baseline `f2654bc`.

Torus attempt `0bab8fae-aab5-4b25-be0f-e79c9fcd03d5`:

```sh
cargo test -p torus-core --lib cancel_all_compaction -- --test-threads=1
cargo test --release -p torus-core --lib cancel_all_compaction_multilevel_microbenchmark -- --ignored --nocapture --test-threads=1
```

This experiment was REJECTED on performance, not promoted. The actual-gate
artifact is `cancel-multilevel-gated-r1.log` in the campaign directory above.
Mode3 five-level middle16 paired AB ratios were1.634 atdepth8192 and1.895 at32768.
However, depth8192/five-level dispersed16 regressed to0.915 with clean controls
AA0.993 andBB0.999. Declined uneven15/shallow/sixth-level cases were near parity.
Root recorded performance rejection event `79a6fb54`. Passing semantic tests and
middle-only speedups do not override this representative-layout regression.
This source is committed before the separate retain-cost threshold experiment.

The earlier forced second repeat also completed: middle8192/16 AC1.721,
AA0.975, CC0.973; middle32768/16 AC1.957, AA1.002, CC1.070. At8 targets per level,
AC was1.159 at8192 and1.225 at32768. These remain test-only middle-layout results
and do not establish live throughput or production acceptance.
