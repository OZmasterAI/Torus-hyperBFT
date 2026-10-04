# Pass 13 — ordinary execution, ownership and persistence lifecycle

Reviewed 2026-10-04 against `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd` in the read-only source checkout.
This report belongs to the separate `audit/chain-findings-2026-10-04` worktree;
all source links pin the reviewed revision because that branch contains older
production source.

**Result: no additional numbered finding is promoted.** The new margin-sums
cache has a coherent successful-path lifecycle through resident rows, serial
flush and pipelined handoff. Existing persistence and mempool candidates below
remain open; cache/reference equality does not close them. This is a bounded
whole-project lifecycle review beyond the C3 diff, not a correctness certificate.

The [audit index](README.md), [pass 3](chain-cea1254-pass3-project-wide-2026-10-04.md),
[pass 6 persistence review](chain-cea1254-pass6-persistence-review-2026-10-04.md),
[pass 8 lifecycle review](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md),
[pass 9 storage review](chain-d52a33f-pass9-astra-storage-2026-10-04.md), and later
consolidated finding lists were used for provenance. No applicable `AGENTS.md`
was found. Cargo and rustc were absent from PATH; **all Rust tests cited here
were read, not run**. Only this document was written. No source edits, Git
mutations, Torus writes, installation, keys, services or live-chain operations
were performed. Certificate admission, malformed input, adversarial networking
and the interrupted pass-5 assignments were not resumed. Prior interruption
findings are retained by source comparison, not newly exercised fault schedules.

## Execution order and durable ownership

The actual [dispatch caller](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L6004)
persists a fully materialized committed header/body before execution-channel
handoff. It removes native hashes from the pool during once-per-height
preparation, before a nonblocking send can return Full. That timing is safe on
the scoped successful-write path because the committed durable body becomes
the execution input; the mempool is no longer its owner. The method's comment
saying bookkeeping happens when a block is “actually executed” is less precise
than its implementation. A channel-full retry preserves preparation and returns
the block for ordered parking; it does not require native-pool reinsertion.

The [execution gate](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1668)
uses the maximum of durable applied height and the pipeline's logical applied
watermark to avoid applying the same in-flight height twice. The fast path
requires durable header/body, no EVM, no pending slashes and no epoch boundary;
other blocks [drain the worker](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1725)
before serial execution. Native work is also due on otherwise empty blocks
with next-height writer actions, an epoch boundary, oracle submissions or
liquidation continuation state. The latter checks use the
[parent-aware overlay](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1911).

EVM executes before native execution. The native
[phase order](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2246)
is block-start oracle aggregation, sorted action batches, writer-queue drain,
liquidation, governance, fee distribution and epoch processing. “Pre-EVM” and
“post-EVM” native batch variable names do not mean that EVM executes between
those calls. A successful listing late in this block affects the next context's
configuration; a boundary reward is created after that block's ClaimRewards
actions. These observations retain the earlier lifecycle qualifications and
are not new missing-scheduler candidates.

[Replay](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L4222)
starts at durable applied + 1 and traverses the committed gap. Missing nonempty
bodies park recovery; the implementation does not deliberately mark their
execution complete. Existing
[whole-gap](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L7579)
and [missing-body](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L7624)
regressions assert ordering and stopping below the hole. Those protections do
not prove pruning preserves every needed input.

## C3: memory cache, durable rows and deferred books

“Persistent” in [`SumsCache`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L822)
means retained across blocks in RAM. It has no separate RocksDB encoding or
snapshot family. The [`RowsSlot`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L1982)
owns positions/balances, mark state and sums together. Invalidation drops the
whole slot. [`begin_resident`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2158)
takes the slot, checks successor height and the overlay-visible applied marker,
and otherwise reconstructs rows from DB plus the parent layer with an empty
sums cache. It accepts an absent marker; this is not claimed as a fault-proof
marker-validation contract.

After context clones are dropped, [`end_resident`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2234)
applies the block's own delta and removes cached sums for every trader whose
position keys were written or deleted. The new
[`ResidentDelta::keys`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/resident_rows.rs#L55)
includes tombstones. Balance-only changes preserve position sums; callers still
supply the current balance. Changed mark/config versions prevent reuse of old
valuations. The production caller takes the delta before freeze/flush, detaches
the context's sums state, and stashes only after successful serial flush or
pipeline handoff at [the application boundary](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2419).
A pipelined slot can therefore represent logical state ahead of durable state;
that is intentional and distinct from claiming a durable commit.

Deferred book writes do not contain positions/balances and therefore do not
introduce an unseen second position delta into C3. The
[application defers book save](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2351)
only for pipelined resident-book contexts in supported level-authority modes.
The worker computes the [book sidecar](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/exec_pipeline.rs#L381)
and folds it into the state/applied-marker batch. The next block uses resident
books; if that holder cannot be reused, the
[rebuild barrier](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2173)
waits for worker persistence before loading books. On clean process restart,
both resident holders and C3 sums start empty; intact durable rows remain the
source of truth. No requirement to serialize the sums map was found.

The tests establish different, complementary contracts:

- [C3 seeded sequences](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/sums_cache_tests.rs#L320)
  compare cached and reference results/position-balance rows for six seeds of
  forty blocks. They assert nonzero persistent/memo/dirty/computed use, overflow
  coverage and version changes. Consumer shadow checks and explicit account
  reads provide more than a final-root comparison. However the
  [harness](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/sums_cache_tests.rs#L208)
  directly writes positions, market/config and oracle fixtures, keeps books in
  a HashMap, compares overlay rows before persistence, and flushes only the
  previous frozen overlay. The final pending block is not flushed. This is
  useful cache equivalence coverage, not a signed governance sequence, durable
  final-state round trip or deferred-book worker test.
- [Targeted invalidation](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/sums_cache_tests.rs#L346)
  checks exact retained traders and hit counts after deletion and a skipped
  height. It establishes empty-cache reconstruction without closing the DB.
- [Application resident tests](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L16897)
  compare resident rows against drained durable state after each block; the
  adjacent matrix compares all CF dumps, per-block write sets and running hashes
  across four book modes, serial/pipelined execution and holder recreation.
  The [helper](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L16853)
  retains the same open StateDb when it “restarts” execution contexts. It waits
  after each block in the resident-row comparisons, so this matrix alone does
  not establish the maximum-lag overlap case.
- [Deferred-book end-to-end coverage](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L14337)
  asserts all-CF/root equality, exact deferred-job heights, applied height 13
  and nonempty book/trade data. The
  [rebuild test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L14419)
  parks a real worker, invalidates its resident holder, asserts execution waits,
  then compares final state to serial. These assertions support the sidecar
  barrier contract; they do not themselves prove all C3 cache branches execute.

## Prior findings revalidated against current callers

| Existing finding | Current source and disposition |
| --- | --- |
| F18, bytecode identity | [Incremental persistence](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/incremental.rs#L304) still stores `bytecode.bytes()`; [reload](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/db.rs#L985) constructs raw code from stored bytes. Retain the earlier pinned-dependency analysis; that dependency was not downloaded/reverified here. This can occur on a DB round trip within one process, not only restart. |
| F19, restart configuration | [Node startup](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L529) still selects supplied genesis configuration or defaults independently of existing-account initialization. Retain; omission means absent from both CLI and TOML. Logs exist; the issue is missing canonical mismatch rejection. |
| F21, snapshot verification | [Checkpoint creation](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/snapshot.rs#L78) includes all CFs; [verification](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/snapshot.rs#L111) compares the composite root with supplied metadata. Its [seven native-root CFs](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/native_trie.rs#L47) still omit authoritative families. Retain coverage limitation, not a claim that intact restore drops excluded rows. |
| F30, state/mirror seam | [Serial state-marker flush](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2489) precedes [mirror resync](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2558); the [worker](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/exec_pipeline.rs#L481) retains that ordering. C3 does not repair the existing interruption window. Successful clean execution can complete both writes. |
| F31, pruning unapplied inputs | [Pruner cutoff](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/pruner.rs#L139) still depends on current height and retention, without applied-frontier protection. Its [node caller](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L1169) uses the commit-visible shared height. Retain optional-pruning/recovery gap; missing-body parking is not input preservation. |
| F38, archive restart availability | [RPC construction](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/lib.rs#L317) still starts the pruning frontier at zero; only [pruner construction](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/pruner.rs#L117) reloads persisted progress. Archive startup does not construct it. Retain successful empty/partial old-log response candidate after a clean retention-to-archive restart. |
| F39, byte-budget admission | [EVM admission](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L417) still checks gross incoming bytes before pool locking and replacement credit. Retain; this pass does not measure concurrent overshoot or memory use. |

F01/F05/F13–F16 and other adversarial/fault scopes were not independently
re-audited here. Economics/trading/RPC candidates outside this report's lifecycle
scope are neither closed nor counted again. In particular C3's equivalence to
the reference does not establish that the reference economic policy is correct.

## Ordinary EVM pool ownership remains a historical open issue

Historical surface R2 is still supported without adversarial input. Valid
[admission](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/validate.rs#L103)
allows a modest future nonce. [Selection](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/evm_pool.rs#L280)
starts at each sender's lowest *pooled* nonce, not its executable state nonce,
and removes selected entries. The application really calls
[`drain_evm`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L5763).
Thus receiving nonce 1 before nonce 0 can select an unexecutable transaction;
this is not evidence that EVM silently executes it with the wrong nonce.

Repository caller search finds `reinsert_evm` only at its definition and its
unit test, with no production caller. The
[test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L2905)
proves explicit manual reinsertion restores pool size; it cannot establish an
automatic abandoned-proposal recovery path. The native pool's non-destructive
selection and in-flight exclusion ledger must not be generalized to EVM.
This remains the already recorded R2 candidate, not a new F46 or a claim that
all lost-view transactions become permanently unavailable across every peer.

## Remaining regression work

1. Exercise ordinary signed position changes, a balance-only claim/transfer,
   fresh-to-stale mark transition, empty native-maintenance blocks and a boundary
   block through actual execution with C3 enabled. Park the worker to obtain
   real parent-layer overlap, then drain it. Compare canonical rows, receipts,
   write sets and roots against a cache-disabled reference; assert cache hits
   and dirty bypass actually occurred.
2. Release every StateDb owner and reopen RocksDB under identical configuration
   after that successful sequence, across supported book modes. Compare exact
   durable state, reconstruct holders cold, continue ordinary execution, and
   redeliver an applied block to assert no additional credit. Context recreation
   against an existing DB handle is insufficient for this clean-reopen contract.
3. Retain the prior real-pruner regression: commit/dispatched execution lag,
   actual pruning, then clean reopen and replay without injected bodies. The
   existing `crash_replay_runs_after_pruning` seeds a metadata key and empty
   blocks; it does not invoke the pruner against required nonempty bodies.
4. Cover ordinary EVM nonce gaps, proposal abandonment and later nonce arrival
   through production selection/reconciliation; require retained or explicitly
   requeued transactions and correct pending-nonce ownership. Manual
   `reinsert_evm` calls inside the test would bypass the missing integration.

These are recommendations, not added tests or verified fixes. No executed Rust
suite, measured performance improvement or additional production failure is
claimed.
