# Pass 7 — ordinary execution modes and restart consistency

Reviewed 2026-10-04 at `merge/item6-sync2`, HEAD
`cea1254e34625e6b09c58f794de8793b5c12713c`.

**Result: no additional production correctness candidate is promoted.** The
ordinary paths inspected preserve explicit batch ordering, deterministic worker
application and predecessor-state visibility. This is a bounded source review,
not proof that every transaction shape or restart is correct. Existing F18/F19,
F30/F31 and prior atomicity findings retain their provenance and priority.

The audit README, pass-3 consolidated/trading material, pass-4 consolidated and
atomicity material, pass-6 accounting/persistence/correctness reviews, and existing
pass-7 accounting/governance/RPC reports were used for deduplication. No applicable
`AGENTS.md` was found. Cargo/rustc were unavailable: **tests were read, not run**.
No production edit, toolchain install, Git mutation, live-chain action, adversarial
input construction, or Torus write was performed. Only this report was added.

## Reachability and the correct equivalence contract

The production application invokes two native batches and then drains CoreWriter
at [app.rs:2248](../../crates/torus-consensus/src/app.rs#L2248) and
[app.rs:2283](../../crates/torus-consensus/src/app.rs#L2283). Its optimization modes
are separate choices:

| Mode | Actual selection and scope |
| --- | --- |
| Pipelined flush | Default on; only `TORUS_EXEC_PIPELINE=0` disables it. The [fast-path predicate](../../crates/torus-consensus/src/app.rs#L1733) requires durable header/body, no EVM transactions or pending slashes, and no epoch boundary. Other blocks drain the worker first. [Parser](../../crates/torus-consensus/src/exec_pipeline.rs#L43). |
| Parallel preparation | `TORUS_PARALLEL_ENGINE=N`, N >= 2, capped at 32; default off. Production additionally needs at least two senders and the placement-count gate, default 64. [Configuration](../../crates/torus-bridge/src/native_executor.rs#L499), [dispatch](../../crates/torus-bridge/src/native_executor.rs#L4458). |
| Parallel settlement | Explicit settlement toggle plus fill gate (default 1024), or enabled engine plus its fill gate (default 64); at least two markets. Tests can force this below production thresholds. [Dispatch](../../crates/torus-bridge/src/native_executor.rs#L4726). |
| Resident books | `TORUS_RESIDENT_BOOKS=1`; default off. [Parser](../../crates/torus-bridge/src/native_executor.rs#L1636). |
| Resident balance/position rows | Production attaches these whenever the native phase runs, independently of resident books. Only tests have the `test_no_resident_rows` bypass. [Application](../../crates/torus-consensus/src/app.rs#L1969). |
| Book persistence format | Classic default; `TORUS_BOOK_ROWS=1/2/3` selects other formats. These are **consensus-visible formats**, requiring compatible history/fleet configuration, rather than interchangeable local caches. [Contract](../../crates/torus-bridge/src/native_executor.rs#L958), [mode check](../../crates/torus-bridge/src/native_executor.rs#L2528). |

Thus “identical in every BookMode” in the cited tests means equivalence of
execution/cache choices **within each format**. It does not assert equal roots
between Classic, OrderRows and level formats or authorize changing a populated
node's format on ordinary restart. These are source defaults, not inspection of
a deployed node's environment.

## Ordering: no new scalar-versus-batch defect established

[The batch contract](../../crates/torus-bridge/src/native_executor.rs#L4093)
deliberately executes every non-placement action in Phase 1, then reserves all
placements, matches, and settles. The
[actual Phase-1 loop](../../crates/torus-bridge/src/native_executor.rs#L4312)
implements that policy. A scalar loop over mixed cancel/withdraw/place actions is
therefore not a universal reference for the production batch API. Pass 6's
qualification is retained.

Parallel preparation groups actions by sender while preserving each sender's
flat order; its [serial stitch](../../crates/torus-bridge/src/native_executor.rs#L4494)
assigns global IDs in original placement order. The scalar preparation branch
uses the same `prepare_one`. Matching owns one book per market. Results are
[sorted by market ID](../../crates/torus-bridge/src/native_executor.rs#L4702)
before settlement, preserving the order observed by cross-account release clamps
and trade indices. Parallel settlement computes per-market plans and applies
them canonically; its [contract](../../crates/torus-bridge/src/native_executor.rs#L5371)
explicitly separates worker scheduling from apply order. No ordinary successful
read/write path was found that changes these choices with worker scheduling.

The documentation's deferred batch-margin policies remain relevant: shared
free collateral is assigned to the first eligible market instead of independently
spendable in every worker. This is an explicit
[allocation rule](../../crates/torus-bridge/src/native_executor.rs#L4543), not a new
race finding. IO-error behavior is outside this ordinary-success conclusion and
does not close F04.

Coverage is substantive: [parallel settlement](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L326)
compares repeated runs, including subscriber/no-subscriber variants;
[worker-cap cases](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L368)
vary scheduling; [engine cases](../../crates/torus-bridge/tests/engine_parallel_tests.rs#L378)
include one trader across markets and margin exhaustion;
[combined modes](../../crates/torus-bridge/tests/engine_parallel_tests.rs#L488)
include level authority and resident books. These test definitions were inspected,
not executed. Forced mode entry points establish algorithmic comparisons but do
not by themselves exercise production environment parsing and automatic gates.

## Predecessor visibility and resident-state safeguards

The worker uses a [zero-capacity channel](../../crates/torus-consensus/src/exec_pipeline.rs#L195).
Execution's handoff completes after the worker receives the job; the worker
receives its next job only after finishing the previous one. Consequently, when
execution begins h+1, only h can still be pending. The application
[carries h's frozen writes](../../crates/torus-consensus/src/app.rs#L1580)
into the next overlay and advances a logical applied watermark. That watermark
also guards ordinary redelivery before the DB marker catches up.

Both [point reads](../../crates/torus-state/src/backend.rs#L1779) and
[prefix iteration](../../crates/torus-state/src/backend.rs#L1849) honor own writes,
parent writes/tombstones, then resident rows or DB. Resident absence is
authoritative for its two CFs, so a deleted resident position does not fall back
to an old DB row. [Begin](../../crates/torus-bridge/src/native_executor.rs#L1846)
takes the holder and checks height/marker compatibility; [end](../../crates/torus-bridge/src/native_executor.rs#L1918)
applies the block's own delta after successful flush/handoff. Failed or shared
ownership paths leave the slot unavailable and require rebuilding. Empty blocks
advance the holder only as direct successors.

Deferred book serialization is a real exception to parent-overlay completeness:
its bytes ride the worker's sidecar. The production caller has an explicit
[barrier before book rebuilding](../../crates/torus-consensus/src/app.rs#L2165)
when that predecessor used deferred saving. I did not promote a stale-book
candidate by ignoring this guard. The global ID allocator also has a
[persisted high-water row](../../crates/torus-bridge/src/native_executor.rs#L2540)
and [save](../../crates/torus-bridge/src/native_executor.rs#L3421); draining all
resting books is not evidence that ordinary restart resets IDs to one.

## Existing tests are broader than context-only book parity, but narrower than a process restart

These application-level fixtures supplement the tests already discussed in pass 6:

- [Per-block write-set determinism](../../crates/torus-consensus/src/app.rs#L14535)
  compares straight execution with context reconstruction at several heights,
  serial/pipelined, separately in all four book formats. The test requires all
  13 heights and nontrivial writes, not merely equal final roots.
- [Running-hash determinism](../../crates/torus-consensus/src/app.rs#L14599)
  folds captured write sets independently and compares the stored chained hash
  across the same execution/reconstruction choices within each format.
- [Resident rows on/off](../../crates/torus-consensus/src/app.rs#L16914)
  compares full CF dumps, each block's captured write set and running hash, with
  context reconstruction after heights 3 and 7. The
  [helper](../../crates/torus-consensus/src/app.rs#L16833) also checks resident rows
  against DB after every block. It drains the worker before those checks, so
  this specific equality test should not be represented as testing every overlap
  schedule.
- [Parked-worker coverage](../../crates/torus-consensus/src/app.rs#L13641)
  separately holds a write pending while the successor executes. The
  [failure/replay fixture](../../crates/torus-consensus/src/app.rs#L13718)
  injects a worker failure, asserts unchanged durable marker, reconstructs the
  context, calls `replay_committed`, and compares the result with serial state.
  It uses a test gate and the same open `StateDb`; it is not an OS process kill,
  RocksDB close/reopen, WAL recovery or disk-failure experiment.
- The existing [lockbox/order fixture](../../crates/torus-consensus/src/app.rs#L17008)
  includes **explicit expected economic timing**, not just mode equality: a
  deposit at block 1 is queued; block 2's order runs before its drain and has
  zero reserved margin; the credited account rests an order at block 3. It
  compares resident on/off and serial/pipelined outcomes. This narrows the
  broader suggestion that no expected batch-policy assertions exist.

Ordinary boot [replays before attaching the worker](../../crates/torus-consensus/src/app.rs#L3857),
and [replay starts after the durable applied marker](../../crates/torus-consensus/src/app.rs#L4227).
The context-reconstruction fixtures therefore meaningfully cover cold in-memory
books/rows and replay orchestration, while omitting actual DB reopen and startup
configuration loading. They do not close F19, F30's post-marker EVM mirror seam,
or F31's pruning/replay-input gap. Comparing CFs or running hashes after clean
draining also does not prove crash consistency at those seams.

## Follow-up priority and limits

No new P1/P2 production repair is recommended from this pass. Preserve the
existing exact-state/write-set tests. The useful additional coverage is an
ordinary valid mixed-action fixture with independently stated expected outcomes,
followed by clean DB close/reopen and startup with unchanged configuration, plus
focused production regressions for the already-known persistence seams. Keep
same-format comparisons distinct from format migration and forced-worker tests
distinct from production gate coverage. No benchmark, passing Rust result, actual
restart, historical-finding closure, or comprehensive correctness certification
is claimed.
