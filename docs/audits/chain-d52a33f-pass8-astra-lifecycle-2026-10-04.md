# Pass 8 — ordinary market, oracle and reward lifecycle

Reviewed 2026-10-04 on `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: no additional production correctness candidate is promoted.** This
bounded review establishes current lifecycle behavior and narrows regression
recommendations. It does not close F01–F34, certify C2, or establish that the
chain is generally correct. Another pass-8 worker owns the precise C2 diff.

The audit README, October passes 3/4/6/7 and their relevant trading, economics,
accounting, persistence and execution companions, September Astra reports, and
existing market/oracle plans were compared for prior provenance. No applicable
`AGENTS.md` was found. Cargo/rustc were absent from PATH; all Rust tests discussed
below were **read, not executed**. No production files, Git state, services,
installed software or live-chain state were changed. Only this report was added;
the parent owns Torus records. Certificate admission, malformed-input and
interrupted pass-5 adversarial assignments were not resumed.

## Market changes: distinguish listing from deferred update operations

Ordinary signed market listing is reachable through
[SubmitProposal conversion](../../crates/torus-bridge/src/native_executor.rs#L8029).
An approved listing executes in the end-of-native-block governance step,
[after actions and liquidations](../../crates/torus-consensus/src/app.rs#L2305).
The [economic handler](../../crates/torus-economics/src/governance.rs#L1096)
allocates an ID at execution and writes the market row. Two listings in that
step see each other's writes. The existing
[two-listing test](../../crates/torus-economics/tests/governance_tests.rs#L722)
asserts distinct IDs, and the
[existing-market test](../../crates/torus-economics/tests/governance_tests.rs#L744)
includes non-market metadata rows in the same CF. The September ID-zero
overwrite description is not current behavior.

The newly listed market first enters the next block's loaded
[margin configuration](../../crates/torus-bridge/src/native_executor.rs#L2624)
and [listed-market oracle inputs](../../crates/torus-bridge/src/native_executor.rs#L8304).
There are no further user actions or liquidations after listing in its execution
block. Thus a block-start configuration snapshot is not by itself evidence of a
normally reachable stale-listing action in the same block. The market still
needs valid oracle reports; successful listing does not imply an immediately
available mark or successful order placement.

`UpdateMarketParams` and `DelistMarket` have a different contract. Their direct
native variants reject, with a
[test requiring unchanged market state](../../crates/torus-bridge/tests/market_governance_exec_tests.rs#L62).
Their proposal variants map to
[no execution payload](../../crates/torus-bridge/src/native_executor.rs#L8052).
The economic scheduler leaves a passed text-only proposal
[Passed, rather than setting Executed](../../crates/torus-economics/src/governance.rs#L992).
No live parameter-update operation was found that invalidates C2's block-start
configuration during subsequent actions. This is a documented
[C9 limitation](../plans/liquidation.md#L280), with historical ECON-PF-05
provenance; it is not a new cache regression. F27's individually written but
unread governance settings and F25's hardcoded first-book tick/lot values remain
separate existing findings.

Listing's ignored maintenance-margin field is also explicitly
[C3](../plans/liquidation.md#L270), not a newly discovered parameter-loss issue.
Future support for effective updates will need an activation boundary and a
policy for existing orders/reservations, rather than merely accepting the type.

## Funding is outside the implemented lifecycle

The types expose
[`max_funding_rate_bps`](../../crates/torus-types/src/lib.rs#L1224), but the scoped
source search found no funding accrual/settlement scheduler or engine in core,
bridge or economics. The current
[oracle plan explicitly says funding does not exist](../plans/oracle-aggregation.md#L105),
and [margin scope](../plans/account-level-margin-f1.md#L59) says the same.
Consequently this review makes no claim about missed funding boundaries,
funding conservation or funding persistence. An encoded parameter is not
evidence that those operations have shipped. This is a product limitation,
not an additional runtime defect candidate.

## Oracle timing already has observable end-to-end coverage

The native-phase gate explicitly includes epoch boundaries and reads
[oracle due work through the parent-aware overlay](../../crates/torus-consensus/src/app.rs#L1937).
The [block-start oracle step](../../crates/torus-bridge/src/native_executor.rs#L8286)
prunes old submissions and aggregates before native actions. Inputs use the
then-current Active validator records and whole-token stakes; current-block
delegation/status actions occur later and affect subsequent block inputs.
Submission handlers do not independently move the aggregate. The application
[sequence](../../crates/torus-consensus/src/app.rs#L2250) makes current submissions
first eligible for the following block's native mark.

Freshness has two clocks: submission eligibility is at most ten seconds from
its submitting block timestamp, and the aggregate remains usable through sixty
seconds after its last successful aggregation. An unchanged quorum can be
reaggregated during the submission window, advancing the aggregate timestamp;
quorum loss then preserves that timestamp. These are explicit
[implementation rules](../../crates/torus-core/src/oracle.rs#L323), not evidence
that a failed aggregation silently refreshes stale data. The
[prune/read paths](../../crates/torus-core/src/oracle.rs#L386) use the block-time
clock and do not require mutating an aggregate row merely to observe staleness.

Existing tests are materially stronger than comparing two final marks:

- [Whole-block clock coverage](../../crates/torus-consensus/src/app.rs#L16131)
  checks stored submission timestamps through direct execution, dispatch and
  replay. [Signed rounds](../../crates/torus-consensus/src/app.rs#L16176) assert
  that new prices take effect from the next block, including an empty block.
- [Window/freshness boundaries](../../crates/torus-bridge/tests/oracle_block_tests.rs#L217)
  assert age ten versus eleven and aggregate age sixty versus sixty-one.
  The [application version](../../crates/torus-consensus/src/app.rs#L16202)
  also checks the persisted applied height and the absence of submission rows.
- [Same-block native readers](../../crates/torus-bridge/tests/oracle_block_tests.rs#L237)
  assert an independently expected margin rejection after same-block submissions,
  rather than just cache/reference equality.
- [Serial/pipelined/replay coverage](../../crates/torus-consensus/src/app.rs#L16318)
  compares full CF dumps and roots with an expected final price and reporter-row
  count. The [additional parent-layer test](../../crates/torus-consensus/src/app.rs#L16340)
  deliberately makes the intermediate aggregate survive, so later writes cannot
  hide a skipped oracle-only block.

The oracle replay fixture reuses the same open database and execution context;
[its helper](../../crates/torus-consensus/src/app.rs#L16265) does not perform an OS
restart or RocksDB close/reopen. Its fixture sets epoch length 1000 and excludes
an actual validator transition. These are coverage limits, not observations of
data loss or wrong weighting. The EVM executes before this native phase, so
native block-start mark coverage must not be described as proof that EVM
precompiles and all native readers see a newly aggregated mark at the same
instant; the [application documents that ordering](../../crates/torus-consensus/src/app.rs#L2287).

## Rewards: current boundary state and persisted liabilities

The [epoch hook](../../crates/torus-bridge/src/native_executor.rs#L8411) rewards
the current Active set before applying its planned rotation. It is called
after ordinary native actions, governance processing and fee distribution.
Thus ClaimRewards in a boundary block claims previously accumulated liabilities;
that block's later inflation creates new liabilities for a subsequent claim.
This follows the explicit production order and
[claim method](../../crates/torus-economics/src/staking.rs#L406), not an inferred
failure of the claim transaction.

The reward routines use current stake amounts multiplied by the full configured
epoch length. [Delegation](../../crates/torus-economics/src/staking.rs#L103) updates
stake immediately; [permanent staking](../../crates/torus-economics/src/staking.rs#L312)
records an initial lock height, but the
[permanent reward calculation](../../crates/torus-economics/src/rewards.rs#L130)
does not use it for time weighting. The
[validator calculation](../../crates/torus-economics/src/rewards.rs#L166) likewise
uses the current Active stake. This is an observed boundary-snapshot policy.
No new defect is promoted without a specified stake-activation/time-weighting
contract. It should be explicit in economic tests, particularly for legitimate
stake changes in the boundary block. This observation does not close the
historical self-stake allocation question, F07, F28 or F34.

On the reviewed successful execution path, reward liability/account writes use
the same execution overlay as the rest of native state; the serial
[flush folds the applied marker with state](../../crates/torus-consensus/src/app.rs#L2462).
Boundary blocks are excluded from the
[pipelined fast path](../../crates/torus-consensus/src/app.rs#L1733), which drains
prior work. No separate successful reward commit was found that inherently
requires replaying an already applied epoch to restore its liabilities. F30's
later EVM mirror repair remains an existing, distinct persistence seam.

The [empty-boundary regression](../../crates/torus-consensus/src/app.rs#L15840)
does exercise production execution and compare empty/nonempty boundary outcomes.
Its [fixture](../../crates/torus-consensus/src/app.rs#L15749) checks nonzero
validator pending rewards, validator status/stake and consensus update results.
It does **not** compare exact reward amounts: `EpochOutcome.rewarded` is a
boolean, and this fixture has no permanent staker or claim. This qualification
does not undo the source-level fix of September Astra round-2 item 17.

## Focused follow-up tests

1. Extend successful governance listing through signed execution, the following
   block's margin load, valid oracle submissions and first order. Assert the
   effective listed parameters and exact reservation independently of the
   implementation; retain F25 as the known first-book failure to fix.
2. Combine an ordinary planned validator transition with valid oracle rounds
   before/on/after the boundary. Assert which current Active stake set weights
   each native mark and which set receives the boundary reward, using the stated
   activation policy. Existing isolated oracle and epoch tests do not establish
   this combined contract.
3. Extend the empty-boundary fixture with permanent stake, an external delegator,
   exact validator/delegator liabilities, the cumulative inflation row and a
   claim before versus after distribution. Specify the boundary stake policy
   explicitly instead of deriving expected amounts from the same helper.
4. Cleanly close/reopen a database containing these successful lifecycle effects
   under unchanged configuration. Compare canonical market/oracle/reward rows,
   balances and the applied frontier; then redeliver the applied boundary and
   require no additional credit. This is ordinary persistence coverage, separate
   from fault injection or the existing F19/F30/F31 work.

No failing production regression, executed model, measured performance result,
verified fix or additional numbered finding is claimed.
