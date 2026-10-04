# Pass 13 — execution, core trading and economic lifecycle

Reviewed 2026-10-04 against **`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`**,
branch `merge/item6-c3-pf1`, in the read-only source checkout
`/home/oz/projects/Torus-hyperBFT`. The documentation destination is the separate
`audit/chain-findings-2026-10-04` worktree, starting at
`e8d9181194dec987a07e44a17d4931828e0f3e18`. Source links below are pinned to the
reviewed production commit; the audit branch's older code is not the authority.

**Result: no additional production defect is promoted in this bounded scope.**
C3's cache lifecycle and PF1's ask-depth calculation have explicit safeguards
and substantive source-defined comparisons. Existing economic candidates remain
open, including F12's fill-solvency sequence and F45's exited reward recipient.
Cache equivalence does not resolve an economic invariant shared by both paths.

This review consulted the [audit catalogue](README.md), September reports,
October finding summaries and the relevant trading, accounting, collateral,
execution, action, mark, staking and governance reports from passes 1–12. No
applicable ancestor or repository `AGENTS.md` was found. Both source and
destination were initially clean; the source HEAD matched the requested commit.
Cargo/rustc were absent from PATH. **Rust tests were read, not run.** No builds,
models, benchmarks, installations, Torus writes, source edits, Git mutations,
services, live-chain calls or key operations were performed by this reviewer.
Only this report was written. Blocked pass-5 certificate, malformed-input and
adversarial assignments were not resumed. The parent handles documentation
consolidation and committing.

## Coverage and the reference contract

| Flow | Current source traced | Bounded conclusion |
| --- | --- | --- |
| Placement and batch preparation | [Phase-2 reservation and sender folds][phase2], [exclusive pools and market matching][pools], [same-batch top-ups][topups] | Each sender's running free pool is assigned to its first checked market. Top-ups are taken before that pool is handed to matching. These existing policies are preserved; independently spending the same pool in every market is not the implemented reference. |
| Account valuation and withdrawals | [AccountReader/C3][reader], [AccountView][view], [withdrawal gate][withdraw] | Position-dependent sums are separate from current cash and reservation fields. Entry fallback, Cross filtering and withdrawal exclusion of resting reservations are retained. |
| Settlement, reservation and positions | [maker release][makerrelease], [taker release][takerrelease], [shared fill transition][fill] | Release is based on the difference between reservations at pre/post remaining quantities. Full close deletes the position; event PnL is distinct from accumulated historical `realized_pnl`. |
| Stops, cancellation and queue conversion | [trigger runner][stops], [cancel-all][cancel], [CoreWriter conversion][convert] | Triggered stops release the pending reservation before ordinary revalidation. CoreWriter retains its historical ID/stop-conversion limitations. |
| Oracle and liquidation | [submission gate][submit], [aggregation][aggregate], [liquidation account/action loop][liq] | Active reporter resolution, sample-time/listed-market/entry validation and post-outlier strict stake quorum remain. Liquidation acts only on marked listed positions and has explicit pending/cursor handling. |
| Staking and governance | [native staking callers][stakecall], [unbonding and claim][unbond], [reward recipients][rewards], [native governance conversion][govcall], [scheduler][scheduler] | Claims have actual native/CoreWriter paths. Boundary inflation precedes rotation. Unsupported proposal variants, parameter consumers and snapshot policy retain earlier qualifications. |

The production [native-phase gate][gate] attaches resident rows independently of
the resident-book toggle, and the application [captures the delta after the
last context action][delta]. The current production tail still runs liquidation,
governance, fee distribution and the epoch boundary in that order. Its batch API
runs non-placement actions in Phase 1 before placements; a scalar loop over the
original mixed action list is not a universal economic reference. The
[pass-11 settlement report](chain-d52a33f-pass11-astra-settlement-2026-10-04.md)
and [pass-10 action report](chain-d52a33f-pass10-astra-actions-2026-10-04.md)
remain the provenance for these ordering and result-contract qualifications.

## C3: no ordinary stale-sums counterexample established

[C3][reader] changes account valuation to cache only the position-dependent
`upnl`, IM, notional and maintenance sums. `PosSums::view` supplies the caller's
current `available` and `order_margin`. Consequently a balance-only write does
not require evicting a position sum and does not return an old cash balance.
The removed per-batch maker-free memo is replaced by position-sum caching;
maker cash is still read from the frozen matching backend. During Phase 3 the
unflushed preparation/settlement caches are not that backend's authority. This
preserves the previously documented maker snapshot policy rather than making
Phase-2 balance reservations newly visible to every maker check.

The inspected invalidation chain addresses the natural ordinary counterexamples:

- A position opened, resized, flipped or deleted in the current overlay makes
  [the trader prefix dirty][touches]. `pos_sums` bypasses both persistent cache
  and block memo and rebuilds from visible rows. The trait default is `true`
  (assume dirty), so an unrelated backend cannot silently opt into reuse.
- [Block exit][end] first updates resident rows and then removes cached sums
  for every trader prefix in the block's position delta. Tombstones count as
  dirtied keys. An unchanged mark in the successor therefore does not preserve
  a pre-fill sum for a changed trader.
- [Mark-table construction][marks] compares both the table's usable values and
  the whole loaded margin-config map. A moved/stale/reappearing mark or changed
  tier takes a new process-wide version; cache entries at another version are
  not read. The previous table is an equality reference, not next-block price
  authority. A position market outside the table makes the result uncacheable.
- A skipped native height, mismatched applied marker or rebuilt resident slot
  [starts a new resident lifecycle][begin], with empty sums on rebuilding.
  Failed handoff/flush or outstanding shared resident ownership does not stash
  a supposedly valid slot. The existing slot/overlay contract matters as much
  as the cache map itself.

The [C3 sequence test][sumtests] uses six seeds over forty blocks. It compares
cached readers with uncached readers and per-read-oracle readers, exercises
direct position/balance writes, orders, withdrawals and liquidation, and
compares result success/errors plus position/balance rows between cached and
reference runs. It also requires nonzero persistent/memo/computed/dirty paths,
overflow observations and moved versions. Its explicit guard test verifies
trader eviction and skipped-height rebuilding. These are substantial cache
tests, not merely equality of two empty final roots.

Their precise limit is orchestration: they construct contexts/overlays manually,
write some positions/configs/oracle rows directly, carry books in local maps,
freeze a parent and flush it one block later. They do not submit signed actions
through a node, close/reopen RocksDB, run startup configuration/replay, or prove
an independently chosen solvency policy. The main output comparison also does
not by itself compare all consensus CFs, receipts, book rows and gas fields.
Other existing application/golden fixtures provide broader state comparisons;
this report does not infer absence of that coverage from this one test.

The adjusted [five-market maker test][makeronce] asserts five actual fills and
one position scan with the resident rows/sums attached, in serial and forced
engine choices. Thus its read-count assertion is nonvacuous. It is a count and
path regression, not an independent multi-market economic-budget expectation.
No storage-fault closure is inferred from either fixture: F04 keeps its prior
source/error-path provenance.

## PF1: prefix reuse preserves the former sum and decision

[AskDepth][depth] stores each resting ask level's saturating prefix sum in
ascending level order, folding individual orders in queue order. It lazily
extends that sequence only when a query reaches new levels. A later lower
threshold uses `partition_point` into completed prefixes, so increasing query
prices are not a hidden precondition. [The top-up caller][topups] retains the
separate earlier-batch ask map and adds it after resting-book depth, in the
former order. PostOnly, GTC, quantity/depth and best-ask cap predicates are
unchanged. This is sufficient source evidence for the prefix optimization,
without assuming associativity of arbitrary saturating regrouping.

[PF1 tests][depthtests] compare against a verbatim old top-up implementation,
including count, each prepared order's reserved margin, cached balances and
dirty state. Random query order includes thresholds below, at, between and
above levels, and separate saturation cases query downwards after filling a
larger prefix. The deterministic 50,000-order level case independently asserts
that a bid equal to depth gives no bound while one lot more gives a bound.
It also checks the number of stored prefixes after repeated queries. The
3,000-case whole-function comparison requires more than 100 top-ups.

These tests support preserving the former calculation; they do not establish
that the former soft top-up policy is the best economic rule. In particular,
the documented priority of a later non-pool top-up over an earlier pool-market
taker persists. Constructed prepared orders, huge quantities and inserted book
rows in differential fixtures are not evidence that all those shapes pass
ordinary signed admission. No production regression, speed claim or live
measurement was executed in this pass.

## Relevant earlier findings at the new head

The core, economics, genesis and application files inspected below have no diff
from `d52a33f`; native executor lines moved with C3/PF1. This is a source recheck
of the stated predicates/callers, not a fresh runtime reproduction or resolution.
Earlier priorities are retained: F06/F09/F10/F12 are P1; F11/F22–F25/F27/F29/F34
are P2; F45 is P3. This pass does not regrade them or count their repetition as
additional defects.

| Earlier finding/provenance | Current recheck and constraint |
| --- | --- |
| **F12 / known D5**, [pass-3 trading T01](chain-cea1254-pass3-trading-2026-10-04.md#t01--p1-post-fill-losses-can-become-withdrawable-profit-backed-by-a-flat-negative-vault) | [Match need][need] still charges IM delta without opening-fill mark PnL. [ADL][adl] can close at the unfavorable prior mark and move signed collateral to the vault; [flat vault valuation][liqview] remains `None`. [Withdrawal][withdraw] still allows a flat positive account. C3 reproduces the same AccountView, so it does not close this sequence. |
| **F06**, [initial ADL scan finding](chain-cea1254-2026-10-04.md#f06-adl-never-advances-beyond-its-first-scan-window) | [Counterparty collection][adlscan] initializes an empty scan start on each call and consumes at most `max_rows` globally before market filtering. Caching candidate valuation does not advance that window. The prerequisite remains counterparties beyond the relevant prefix; this is not starvation demonstrated on an ordinary small book. |
| **F22–F25**, [pass-3 trading T02–T05](chain-cea1254-pass3-trading-2026-10-04.md) | [Queue conversion][convert] still does not carry its synthetic return ID into native allocation and maps codes other than 0/1 to Limit. [Batch missing-book construction][pools] still uses ONE tick/lot, as does scalar construction. No execution registry check was found to eliminate the earlier ordinary CoreWriter unknown-market path. Existing RPC listing checks and correct restoration of existing metadata do not repair these source roots. |
| **F09 and September quorum policy**, [pass-7 qualification](chain-cea1254-pass7-sol-governance-2026-10-04.md#historical-f09-qualification--abstain-ignores-even-present-positive-snapshots) | [Missing snapshots][snapshot] still fall back to live stake; zero/absent new-proposal voters are not marked complete. [Abstain/finalization][ballots] still use live ballot weight/current total stake respectively. C3 has no role in these EVM staking/vote CFs. A serialized snapshot height is not a regression for those predicates. |
| **F10/F11**, [pass 2](chain-cea1254-pass2-2026-10-04.md) | [Registration][register] still checks existing address/hot-signer use rather than consensus-key uniqueness; [rotation][rotation] checks current keys, not another pending target. [Rotation cap][cap] still counts arrival plus departure as two changes and splits odd budgets. Existing approvals, valid keys and the small fixed full set remain necessary conditions from the original findings. No new certificate/safety experiment was undertaken. |
| **F27/F29**, [pass 4](chain-cea1254-pass4-missed-issues-2026-10-04.md) | [Parameter execution][scheduler] writes ASCII per-key config, while [effective governance loading][params] reads the separate serialized params row. [Native phase selection][gate] still lacks a governance-deadline trigger. Oracle submissions, liquidation work, transactions/fees and epoch boundaries are controls that cause the scheduler to run; no indefinite delay is claimed when such triggers recur. |
| **F34**, [pass-7 genesis configuration](chain-cea1254-pass7-sol-governance-2026-10-04.md#g01--accepted-genesis-staking-settings-are-disconnected-from-effective-staking-rules) | [Undelegate][unbond] still uses compiled `UNBONDING_PERIOD`; [shipped genesis][genesis] still specifies 604,800. The current [claim caller][stakecall] and maturity check make the mismatch actionable through ordinary principal recovery. C3 does not propagate those typed settings. |
| **F45**, [pass-12 staking/recheck](chain-d52a33f-pass12-astra-recheck-2026-10-04.md#f45-the-zero-active-recipient-is-reachable-through-normal-exit) | Full [undelegation][unbond] retains a zero-active row while principal is queued. [Reward distribution][rewards] still assigns the final scanned row the residual without requiring positive active amount. [Boundary inflation][boundary] still calls it before rotation. This remains a small recipient-allocation error; the earlier four-validator, two-active-delegator, trailing exited-row fixture and one-wei result retain their provenance. |

For F12 the earlier exact fixture remains useful: a listed 20x market with mark
100, distinct ordinarily funded A/B with 60 each, B's resting ask of 1 at 1,000,
and A's capped market buy. IM-only need permits the fill; the earlier ADL path
at 100 leaves A flat at zero, B flat at 960 and the vault at −840. The report's
signed conservation equation still passes. A control fill at the mark does not
create that 900 loss; sufficiently funded A is a different fixture; a flat
negative *user* cannot withdraw. C3 dirty-prefix rebuilding after settlement
returns the same economic values. A regression must assert backed redeemable
proceeds and post-fill equity, not only equality of cached/uncached signed sums.
These numbers are inherited from T01, not recalculated or executed here.

For F45, exact division, one positive delegator, positive last-row eligibility,
or a completed maturity claim removing the zero-active row are counterexamples
to a universal wrongful-reward claim. Maturity alone is not deletion. The
existing reward test with two positive delegators tests proportions/totals; its
conservation assertion can pass a wrong residual recipient. Keep exact reward
*deltas* through ordinary delegation/full exit, real boundary inflation and
reward claim as the proposed regression, with no additional finding number.

Signed ListMarket still uses [automatic ID zero][govcall] and allocation at
execution; the September ID-zero overwrite claim is not restored by this pass.
UpdateMarketParams/DelistMarket are text-only in this native conversion. The
C3 sequence's direct delisting/config changes therefore exercise cache
robustness, not an implemented signed delisting/update lifecycle. Native
ClaimUnbonded is real and atomic at the manager transition, so the old September
"no completion path" statement is obsolete. Positive isolated allocations in
seeded tests likewise do not establish a normal isolated trading product.

## Test assertions and remaining coverage

Preserve the exact-value [partial fill/cancel][release-test] and
[PnL-before-release][pnl-test] regressions. They independently inspect economic
amounts and already refute a generic reservation dust/PnL overwrite lead.
The [reduce-only triggered stop tests][stop-test] independently require removal,
zero reservation and correct position closure; C3 does not erase that coverage.
Weighted-entry remainder loss remains September's position-arithmetic finding,
not a new cache defect.

The [pass-11 exact-PnL qualification](chain-d52a33f-pass11-astra-settlement-2026-10-04.md#one-exact-pnl-fixture-does-not-enter-parallel-settlement-for-its-closing-batch)
still applies: [its PnL-producing batch][parallel-test] contains only market 2,
so the at-least-two-market settlement gate chooses the sequential loop even
with the forced option. Broader parallel equality tests remain useful; this
one fixture does not independently prove closing PnL in parallel planning.

The focused follow-up is to execute existing C3/PF1 suites with the pinned
toolchain, then preserve an ordinary signed application fixture that joins
deposit, real partial fills/close, cancellation, withdrawal and a clean database
reopen under unchanged configuration. Assert cash, reservations, absent closed
positions, queued liabilities and counterparties independently. Separately
regress F12/F45 and the established governance/configuration findings. No new
regression was added, no historical finding was closed, and no comprehensive
whole-chain correctness certification follows from this source-only scope.

[phase2]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L4771
[pools]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L4908
[topups]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L6727
[reader]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L780
[view]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/margin.rs#L155
[withdraw]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8437
[makerrelease]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L6258
[takerrelease]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L6939
[fill]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/position.rs#L487
[stops]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L7001
[cancel]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L7475
[convert]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8889
[submit]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8108
[aggregate]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/oracle.rs#L286
[liq]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/liquidation_step.rs#L58
[stakecall]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L7822
[unbond]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/staking.rs#L142
[rewards]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/rewards.rs#L231
[govcall]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8290
[scheduler]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/governance.rs#L1007
[gate]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1945
[delta]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2421
[touches]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/backend.rs#L1978
[end]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2227
[marks]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8595
[begin]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2156
[sumtests]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/sums_cache_tests.rs#L217
[makeronce]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/maker_snapshot_once_tests.rs#L124
[depth]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L339
[depthtests]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/same_batch_depth_tests.rs#L335
[need]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/order_book.rs#L227
[adl]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/liquidation_step.rs#L261
[liqview]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/liquidation_step.rs#L149
[adlscan]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/liquidation.rs#L340
[snapshot]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/governance.rs#L1306
[ballots]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/governance.rs#L882
[register]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/staking.rs#L43
[rotation]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/staking.rs#L1090
[cap]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/epoch.rs#L140
[params]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/governance.rs#L1209
[genesis]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/devnet/genesis.json#L42
[boundary]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8689
[release-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/maker_margin_release_tests.rs#L134
[pnl-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/position_cache_exec_tests.rs#L158
[stop-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/reduce_only_tests.rs#L422
[parallel-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/parallel_settle_tests.rs#L527
