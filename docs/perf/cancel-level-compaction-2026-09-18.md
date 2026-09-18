# Adaptive cancel-all queue compaction candidate

Current follow-up: `perf/cancel-levels-viewbound` at base `47f3a30` contains a
new, unverified per-level hybrid described at the end of this document. The
measurements below belong to their named historical revisions; none verifies
this source or establishes a live throughput gain.

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

## Four-length retain threshold: verified semantics, performance not accepted

Rejected multilevel source and its receipt are preserved in `ca30eea`.
The next experiment changes only the retain-versus-removal cost threshold:
`min(forward_shifts, reverse_shifts) > queue.len().saturating_mul(4)`.
Classification, grouping, outputs, journal effects and benchmark cases are
unchanged. It introduces no new probes or allocations.

The existing estimate counts moved orders, not CPU time. `VecDeque::remove`
can use bulk memory movement; `retain` evaluates a branch per surviving order.
That makes a higher crossover plausible, without proving this caused the
observed dispersed regression. Exact estimator arithmetic for16 targets:

| Depth | Layout | Min estimated shifts | Shifts/depth | Retain at4x |
| ---: | --- | ---: | ---: | --- |
| 8192 | dispersed | 32725 | 3.994751 | no |
| 8192 | middle | 65408 | 7.984375 | yes |
| 32768 | dispersed | 131029 | 3.998688 | no |
| 32768 | middle | 262016 | 7.996094 | yes |

These are cost-model calculations, not measured timings. The factor4 is an
experimental margin between the two observed shapes, not a calibrated general
CPU model. Existing dense32/200 cases remain eligible for retain; shallow,
sparse and rejected multilevel sets still follow the same paths. The same11
differential tests and actual-gate AB/AA/BB cases must be rerun, including all
front/back/dispersed and declined shapes. The full core suite and the same benchmark have now run; results below do
not justify promotion.

Torus attempt: `7f9b3698-771c-4d88-bac8-36ad01e8248c`. Its initial evidence text
mistyped the8192/dispersed estimate as32711; the exact value is32725 as above.


Receipt `ce7b778d-5d7a-4a00-bc6d-c22c6ee9f8fb` passed the full release core
suite (121 unit tests and 89 integration tests; seven ignored tests total),
the isolated benchmark command, and diff checks with unchanged source.
Artifact: `cancel-multilevel-crossover4-r1.log` and its source/binary manifest.
Ratios are paired baseline/candidate medians; above one favors the candidate.

| Case | AB | AA | BB |
| --- | ---: | ---: | ---: |
| Five levels, middle, depth 8192 | 1.674 | 0.970 | 1.013 |
| Five levels, middle, depth 32768 | 1.839 | 1.020 | 1.046 |
| Five levels, front, depth 8192 | 1.684 | 0.890 | 1.047 |
| Five levels, back, depth 8192 | 1.364 | 1.164 | 1.050 |
| Five levels, dispersed, depth 8192 | 0.987 | 0.966 | 0.960 |
| Declined uneven 15-target group | 1.006 | 0.982 | 1.053 |
| Declined shallow group | 0.989 | 0.972 | 1.018 |
| Declined sixth level | 0.938 | 1.030 | 1.005 |

The earlier dispersed regression is reduced to near parity in this run, and
middle-layout gains persist. However, the sixth-level fallback is about 6%
slower despite substantially closer identical-code controls. Front/back
controls also remain noisy. This does not establish whether the fallback
cost is stable; the previous gate run measured 0.984 for that case. Keep the
candidate isolated pending repeated fallback diagnosis and live eligibility
frequency. Passing correctness tests and a successful benchmark exit do not
constitute performance acceptance. No main-branch merge or chain-throughput
gain is claimed.

## Sustained-depth follow-up: bounded per-level hybrid (unverified)

New scoped issue `b9b82951-1b48-4104-9cad-6f3ff190dad0`, attempt
`bbbbb094-ab12-4cf2-b5d5-8c51d9e20cdc`, follows the original cancellation issue
`63bb4ed3-4665-44c1-b902-b7d51d195f23`. The old failed and incomplete microbenchmark
acceptance remains in force. This is a new source hypothesis, with no tests,
benchmarks, runtime artifact, or live result yet.

The sustained control `s60-sustained-base-10m-r1` ran nominal300/actual310 seconds
with BAND5, CROSS0.5, CANCEL0.05, uniform10 markets,5000 senders and batch400.
Its four depth snapshots reported0/832100/1375197/1789380 resting orders. At
nonzero offsets,40 queues (two per side per market) contained98.42/99.72/99.50%
of all orders. There were99/98/98 occupied levels overall, and individual sides
had2..8 occupied levels. At offset300, market1 bid counts were
`[5,10,12,23,26,787,38142,47903]`. Source evidence is the small raw artifact
`/home/18c/bench-results-matched/s60-sustained-base-10m-r1/depth/depth.jsonl`.

Fills slowed across the three100-second windows and phase1 cost grew, but that
correlation does not identify cancellation as the cause. The control was
rejected for a validator body-exhaustion/sync-fallback event; it later drained
in107 seconds with agreement. Aggregate snapshots expose neither trader IDs
nor queue positions. The mean late resting ownership was35.8 orders per
sender/market, and the generator fixes one side per sender/market. This does
not measure how often any cancel gate activates.

The historical classifier rejects a whole batch if it has fewer than80 targets
across levels, any touched queue is too shallow, any group has fewer than16
orders, or a sixth group appears. Its rejected probing work precedes the old
per-order loop. For the six-level96-target fixture, the classifier performs82
index gets and five level lookups before falling back to96 index removals.
That is real repeated work; it is only a possible explanation of the historical
0.938 ratio, not an attribution established by measurement.

The new normal-size path applies to32..200 targets when the global index has
at least1024 entries. It removes each authoritative index entry exactly once
while collecting at most five deferred queues in a fixed metadata array.
Queues below1024 depth do not consume a slot. Thin queues and queues beyond the
five-slot limit use ordinary removal; they no longer veto deferred queues.
Every target from an already deferred queue joins that queue, so it remains
untouched until its positions are resolved from `order_seq`. Helpers never
look up an index entry that the collection pass already removed.

Each deferred queue independently qualifies for the existing removal-cost
comparison at32 targets/depth1024 or16 targets/depth8192. Below those guards,
its targets use ordinary removal in their original relative order. Qualifying
queues retain the previous ascending/descending shift estimate and compact only
when the cheaper estimate exceeds four queue lengths. No crossover constant
has been lowered. Output slots restore original cross-queue cancellation order.
Per-order row/level journal effects, epoch increments, sequence removal and dirty
chunk marks remain attached to successful removals. Pending stops retain the
same public-method behavior. The original <32/small-book loop and the historical
single-deep-level path for synthetic states above200 targets remain available.

Logical target storage is bounded by the production200-target limit, with at
most five target vectors plus the result slots. This removes the whole-batch
lookup probe, but adds bounded classification, vector/result-slot allocation
and some level lookups on batches that ultimately use ordinary removal. There
is no claim of zero fallback overhead. Earlier low-target deep queues can fill
the five slots before later hot queues; this conservative bound is deliberate
and must be evaluated, not hidden as an unconditional optimization.

New differential fixtures use actual Mode3 state and two late observed depths,
38142/47903, together with eight thin levels. Ownership is synthetic:16 targets
per deep queue, one per thin level,40 total. Front/back/middle/spread positions,
interleaved reversed trader-index order, a fully removed thin level and active
stops check exact returned orders, survivor FIFO, auxiliary maps, epochs, dirty
chunks, journals and encoded row/level images. Incrementally emitted Mode3
level hashes are also compared with from-scratch `level_row_data_chunked`, then
a following append/save checks cache validity. Other fixtures cover15/17 partial
eligibility, six-deep-group overflow, larger batches with only under-count
groups, original small batches, depth/count boundaries and duplicate/missing
index IDs. Legacy gate witnesses are named as historical evidence rather than
as assertions that the hybrid chooses the same path.

The new ignored `cancel_all_compaction_hybrid_microbenchmark` reuses the same
balanced16-sample paired AB/AA/BB machinery and shared-RandomState fixture clones.
Seven Mode3 cases cover ten touched levels: two deep queues at8192 and at the
observed38142/47903 depths with16 middle targets; observed-depth dispersed16;
8192-depth15/17 partial eligibility;8192-depth8-target groups plus enough thin
targets to reach32 overall; ten ordinary shallow queues with an unrelated deep
queue to keep the global-size fast rejection inactive; and six deep groups plus
four thin groups. CSV `depth_per_level`/`targets_per_level` describe configured
deep groups, not every queue; layout names and this fixture description specify
the unequal depths/counts. Historical micro entrypoints remain available, but
running them on this source measures the new hybrid arm, not an old revision.

Root-scheduled commands (not run by the implementing agent):

```sh
cargo test -p torus-core cancel_all_compaction -- --test-threads=1
cargo test -p torus-core
cargo test --release -p torus-core cancel_all_compaction_hybrid_microbenchmark -- --ignored --nocapture --test-threads=1
```

Semantic verification, control/order-stratum review, repeated fallback cost
checks, and actual live activation and duration-matched chain evidence are
still required before acceptance or integration.

### First hybrid qualification and mechanism screen

Root corrected the stale-ID fixture to `OrderId::MAX` after its initial compile
failure, then qualified 124 core library tests, 89 core integration tests and
33 bridge persistence/row/resident tests in release mode. Receipt
`5ec6b86f-95c8-4ad3-a0a3-30b0a9ce25fc`; eight ignored cases were not included.
The production implementation did not change in that correction.

The idle-host seven-case screen completed in 85.20 seconds, with a separately
prepared test binary SHA256
`5ba3d5d6f5d8d2ede907687186516868890e090da0e1763d58f71fc2a84a1209`.
Manifest and full strata are retained as `cancel-hybrid-micro-r1.*` under the
campaign directory. Paired left/right ratios above one favor the hybrid in AB;
AA and BB compare each implementation with itself.

| Shape | AB hybrid | AA | BB hybrid |
| --- | ---: | ---: | ---: |
| 8192-deep, 16 middle targets, mixed thin levels | 1.819 | 0.916 | 0.932 |
| Observed deep sizes, 16 middle targets | 4.573 | 1.107 | 1.004 |
| Observed deep sizes, dispersed targets | 1.203 | 1.044 | 1.012 |
| 8192-deep, 15/17 partial eligibility | 1.319 | 0.996 | 0.924 |
| Deep queues below target-count threshold | 0.949 | 0.954 | 1.007 |
| Ten shallow queues, unrelated deep liquidity | 0.837 | 1.078 | 0.996 |
| Six deep queues, deferred-slot overflow | 2.030 | 1.020 | 0.998 |

Deep middle cases have substantial mechanism headroom, while shallow fallback
regresses. Control noise also matters, especially in the first and partial cases.
No node binary or live gain is claimed. The next revision should remove eager
result-slot allocation and redundant shallow-queue lookup where no grouping is
needed, preserving once-only index removal and every tested state invariant.

### Follow-up: lazy deferred results and one ordinary queue lookup

Issue `b08510cb-380b-42f8-9305-ddb4407826cc`, attempt
`bc939e56-6903-4430-b3f6-7d37165ba777`, records the shallow0.837 ratio as a
separate performance finding following the semantic resolution of `b9b82951`.
The first-run table and artifacts above remain evidence for `1367cb6`, not this
revision. Eager optional result slots/result collection and a repeated shallow
level lookup are observed source costs; their contribution to the measured
regression is a hypothesis, not isolated attribution.

The revision starts with a baseline-shaped `Vec<Order>` and accumulates ordinary
successful removals directly. The first selected deep queue lazily creates the
deferred slots, moving the successful prefix into their beginning. Missing or
duplicate IDs can make that prefix shorter than the original index position;
this is safe because no earlier target was deferred, all prefix outputs precede
all later targets, and the prefix length cannot exceed the current position.
Later targets keep their original-position slots. After deferred removal, results
extend the original vector using its retained capacity. With no deferred queue,
there is no slot allocation, promotion or final flattening pass.

Classification and ordinary removal now share the same mutable queue lookup.
Removing an emptied price level remains a separate tree operation. A shared
record-removal helper preserves sequence deletion, row/level journals, per-order
epoch increment and dirty chunk marks without an index relookup. No whole-batch
classifier, queue-count limit, depth/count guard or movement threshold changes.

Two additional Mode3 differentials cover all-shallow touched queues with
unrelated deep liquidity, and late promotion after32 successful shallow removals
interspersed with missing IDs, a duplicate, and an index pointing to an absent
queue. Late promotion exercises both one-target ordinary removal and the existing
16-target deep eligibility. They reuse ordered-output, FIFO, metadata, journal,
incremental/from-scratch level commitment and following-save comparisons.
The seven benchmark cases and balanced AB/AA/BB schedule remain unchanged.

This removes the extra representation on all-shallow batches, not on every
all-ordinary batch: a selected deep queue that ultimately has too few targets
still causes promotion. Deep cases now retain both result-buffer capacity and
slot storage during processing; repeated measurements must check their cost as
well as shallow fallback. Independent review found no blocker. The revised
implementation passed 126 core library, 89 core integration and 33 bridge tests
(248 total; receipt `2bed8e38-848d-415b-afc5-e987b1814386`). A separate node-only
build after cleaning affected packages passed receipt
`bed520bd-7481-4ec4-87f2-eee39e38913a`; the new production
`cancel_all_record_removal` symbol was positively identified.

The unchanged seven-case screen completed in 81.73 seconds on an idle host.
Artifacts: `cancel-fallback-micro-r1.*`; test binary SHA256
`6450c15aa03d37c4ddd41522990be00eaecfa07bd26d55a7338a8a23cc893624`.

| Shape | AB hybrid | AA | BB hybrid |
| --- | ---: | ---: | ---: |
| 8192-deep, 16 middle targets, mixed thin levels | 2.017 | 0.978 | 1.180 |
| Observed deep sizes, 16 middle targets | 3.515 | 0.924 | 0.965 |
| Observed deep sizes, dispersed targets | 1.188 | 1.039 | 1.008 |
| 8192-deep, 15/17 partial eligibility | 1.552 | 0.929 | 1.067 |
| Deep queues below target-count threshold | 0.989 | 0.974 | 0.967 |
| Ten shallow queues, unrelated deep liquidity | 0.982 | 0.924 | 1.050 |
| Six deep queues, deferred-slot overflow | 1.948 | 1.051 | 0.877 |

Deep-case mechanism gains persist. Shallow fallback is closer to parity, with
noisy controls; this does not prove absence of small regressions. A live
equal-duration comparison is required before promotion or any chain-speed claim.
