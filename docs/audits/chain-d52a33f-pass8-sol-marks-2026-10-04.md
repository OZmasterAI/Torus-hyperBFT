# Pass 8 — C2 block marks and ordinary consumer correctness

Reviewed 2026-10-04 at `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`, against previous audit revision
`cea1254e34625e6b09c58f794de8793b5c12713c`. The intervening source delta is C2
(`ccdb59b`) and its merge: a block mark table replaces the per-batch mark memo.

**Result: no additional ordinary production correctness candidate is promoted.**
The checked production ordering rebuilds prices at each native block's timestamp
and preserves the former mark rule. The previous table is an equality/version
reference, not the next block's price authority. This is a bounded source result,
not runtime proof, closure of F01–F34, or a claim that the wider margin engine is
correct.

The audit README and pass-3 consolidated/trading, pass-4 consolidated/atomicity,
pass-6 correctness/accounting, and pass-7 consolidated/execution/accounting reports
were consulted for overlap. No applicable `AGENTS.md` was found in the repository
or ancestor directories. The updated implementation plan was read directly from
`origin/docs/market-scaling-design:docs/plans/item6-phase1-impl.md`; its review log
11–15 records the aggregate/book-key additions and fallback. Its C3/C4/PF1 sections
describe future work, not implementation at this HEAD.

Cargo/rustc are unavailable as established for this session; **tests were read,
not run**. No production change, Git mutation, toolchain installation, network or
live-chain test, Torus write, or blocked pass-five certificate/input-boundary
analysis was performed. Only this report was added. Parent handles persistence.

## Production ordering and mark authority

The [application](../../crates/torus-consensus/src/app.rs#L2232) attaches the
previous resident block's mark state, then calls
[`begin_block_oracle`](../../crates/torus-consensus/src/app.rs#L2250) before either
native batch. CoreWriter drain and liquidation follow both batches; governance
execution and epoch processing follow liquidation
([tail](../../crates/torus-consensus/src/app.rs#L2286)). Thus the production
application does not place an action between aggregation and table construction.

[`oracle_inputs`](../../crates/torus-bridge/src/native_executor.rs#L8304) prunes
submissions, lists markets, and obtains Active whole-token validator stakes.
[`begin_block_oracle`](../../crates/torus-bridge/src/native_executor.rs#L8286)
aggregates those markets, then fills the table. The ordinary aggregate writer is
[`OracleManager::aggregate_price`](../../crates/torus-core/src/oracle.rs#L357);
the repository-wide production caller trace reaches the block-start aggregation
loop ([caller](../../crates/torus-bridge/src/native_executor.rs#L8363)). Oracle
submission actions write submission rows, not aggregate rows. No second
production aggregate writer was located after table construction.

The [table read](../../crates/torus-bridge/src/native_executor.rs#L772) is the
former per-read formula: `get_price(m, ctx.timestamp).ok().and_then(usable)`.
[`get_price`](../../crates/torus-core/src/oracle.rs#L374) retains time staleness,
and [`usable`](../../crates/torus-core/src/oracle.rs#L119) excludes nonpositive
prices. The block timestamp is fixed during ordinary execution. C2 therefore
does not make a usable price last an extra block: the table is rebuilt, and a
timestamp crossing the 60-second edge changes a value to `None` before actions.

All three constructed `AccountReader` forms now use the context table:
the [shared constructor](../../crates/torus-bridge/src/native_executor.rs#L815),
[batch preparation](../../crates/torus-bridge/src/native_executor.rs#L4542), and
[scalar placement](../../crates/torus-bridge/src/native_executor.rs#L6854).
`mark_price`, used by placement/modify reservation calculations, delegates to
[the same reader](../../crates/torus-bridge/src/native_executor.rs#L6212).
Withdrawal uses [its account view](../../crates/torus-bridge/src/native_executor.rs#L8183).
Matching's per-batch maker snapshot continues to cache free margin locally
([ownership](../../crates/torus-bridge/src/native_executor.rs#L902)); C2 does not
move account balances or position-dependent values into a cross-block memo.

## Key coverage, liquidation, and fallback

[`fill_block_marks`](../../crates/torus-bridge/src/native_executor.rs#L8330)
unions listed markets, loaded margin-config markets, aggregate-row markets, and
loaded-book markets. The aggregate scan decodes exact `agg` + u64 keys
([scan](../../crates/torus-core/src/oracle.rs#L406)); it includes a delisted
market whose last aggregate is still fresh. Both aggregate scan and price reads
use the context backend: overlay
[point reads](../../crates/torus-state/src/backend.rs#L1780) and
[prefix scans](../../crates/torus-state/src/backend.rs#L1848) honor own/parent
writes and tombstones. C2 does not bypass pending predecessor oracle rows by
reading the bare database.

[`AccountReader::mark`](../../crates/torus-bridge/src/native_executor.rs#L832)
distinguishes a cached `None` from a missing key. Cached `None` is returned without
another read; missing keys and absent tables fall back to the former direct
oracle read. Consequently adding a new book after the table fill does not require
an eager mark-key insertion for correctness on readable, unchanged aggregates.
Contexts that never call the oracle step continue to return the former values,
although they lose the old per-batch memo's performance benefit.

Liquidation uses [the same reader](../../crates/torus-bridge/src/liquidation_step.rs#L61)
then retains its existing listed-only filter. Its account view deliberately values
unmarked positions at entry
([view](../../crates/torus-bridge/src/liquidation_step.rs#L144)). General account
readers can therefore value a fresh delisted aggregate differently from the
liquidation-specific view. This difference predates C2 and is explicitly recorded
in the design; C2 includes those aggregates rather than accidentally replacing
them with `None`. `delisted_marked` currently has only a test caller, so its presence
is not evidence that the planned C4 sums-cache guard already exists.

The aggregate-scan error fallback clears the current table and consumes previous
state. A later successful fill obtains a new version. This is a source safeguard,
not fault-injection verification; storage errors are not developed into a new
finding in this ordinary-success review.

## Cross-block ownership and versions

The [rows slot](../../crates/torus-bridge/src/native_executor.rs#L1743) owns the
previous mark state. [`begin_resident`](../../crates/torus-bridge/src/native_executor.rs#L1921)
takes it and transfers marks only when successor-height and applied-marker guards
accept the slot. Context [attach](../../crates/torus-bridge/src/native_executor.rs#L2689)
moves that state into `prev_marks`. A fresh fill always reads current prices before
comparing both the full table and loaded configs
([comparison](../../crates/torus-bridge/src/native_executor.rs#L8347)). An equal
pair retains the version; every changed/rebuilt/no-slot pair takes a process-wide
counter value.

Margin configs are [loaded once per context](../../crates/torus-bridge/src/native_executor.rs#L2622).
No ordinary production mutation of `ctx.margin_configs` was located mid-block.
The [detach](../../crates/torus-consensus/src/app.rs#L2421) occurs after all actions
and moves the current table plus those configs into the block handle.
[`end_resident`](../../crates/torus-bridge/src/native_executor.rs#L1983) stashes
them only with successfully handed-off/flushed rows whose `Arc` can be recovered.
Early return, rejected guard, missing rows, failed flush, or shared ownership does
not retain that block's table as an accepted slot.

An untouched block can advance the slot's height
([advance](../../crates/torus-bridge/src/native_executor.rs#L1820)), retaining the
last native table as a comparison reference. The next native block still rebuilds
prices at its own timestamp, including staleness. Version equality across empty
blocks therefore does not itself imply reuse of stale prices.

At this HEAD a repository-wide caller search found no production use of
[`mark_version`](../../crates/torus-bridge/src/native_executor.rs#L2705) outside
its definition. It is not serialized into authoritative state or the golden
digest. Process-dependent counter values consequently do not create a present
consensus-state discrepancy. C3's planned cache invalidation must be reviewed
when actual consumers land; current version plumbing alone cannot verify it.

## Existing tests and focused expectations

The current tests provide meaningful source coverage:

- [Table equivalence](../../crates/torus-bridge/src/block_marks_tests.rs#L68)
  compares direct oracle values and reader values, including absent and aging
  prices, the exact 60-second edge, delisted aggregates and outside-table keys.
  [Loaded book](../../crates/torus-bridge/src/block_marks_tests.rs#L124) and
  [no-table](../../crates/torus-bridge/src/block_marks_tests.rs#L138) cases pin fallback.
- [Version lifecycle](../../crates/torus-bridge/src/block_marks_tests.rs#L181)
  checks unchanged marks with changed aggregate timestamps, moved/stale marks,
  config changes, new listings, rebuilds, skipped heights and an intervening
  untouched block. [No-slot parity](../../crates/torus-bridge/src/block_marks_tests.rs#L242)
  checks table equality and fresh versions.
- [Random account-reader comparison](../../crates/torus-bridge/src/native_executor.rs#L8933)
  compares table/direct `mark`, account view, position sums and maker accounts,
  including four-thread access. This is stronger than checking table entries alone.
- [Maker counter](../../crates/torus-bridge/tests/maker_snapshot_once_tests.rs#L97)
  asserts one maker-position scan and zero mark reads during matching.
  [Fed-block counter](../../crates/torus-bridge/tests/storage_reads_tests.rs#L263)
  asserts one aggregate point read per market across aggregation through liquidation
  on a fully fed fixture. This is not a universal one-read bound: insufficient
  reporter aggregation can additionally read its last aggregate as fallback.
- The [golden runner](../../crates/torus-bridge/tests/perf_equivalence_golden.rs#L129)
  carries parent overlays and mark state. Its
  [checks](../../crates/torus-bridge/tests/perf_equivalence_golden.rs#L486) compare
  serial/forced-engine and resident on/off against pinned per-block digests.
  C2's diff leaves the expected digests unchanged. Both resident choices build
  the new mark table; resident-off is not a table-disabled differential mode.
  The unchanged pre-C2 constants and direct-reader unit comparisons still supply
  separate baseline evidence.

Useful next assertions are a valid composed application fixture using both native
batches plus queued withdrawal/order work, with independently specified mark and
margin outcomes at 60/61 seconds; an untouched interval long enough to age a mark;
and unchanged-config clean reconstruction. Add distinct-holder and successful
shared-ownership-rebuild version assertions before C3 begins caching by version.
Preserve direct-reader comparisons and unchanged historical goldens. These are
coverage recommendations, not newly established failures or tests added here.

The bounded review settles without a new F-number. Existing margin/solvency,
CoreWriter identity, market initialization, configuration and persistence findings
retain their prior provenance. No passing Rust suite, observed runtime equality,
performance result, or historical-finding resolution is claimed.
