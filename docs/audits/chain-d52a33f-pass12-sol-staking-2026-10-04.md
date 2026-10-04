# Pass 12 — ordinary staking rewards, exit and persisted queries

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`11756e8bdd8db4fe95a18a42a25d37e6aca536b8`. Production source remains
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`; the intervening committed
changes are audit documentation and audit models.

**Result: one additional P3 source-supported candidate, S01 / F45:** a fully
undelegated account can receive the rounding remainder from validator rewards
because its unbonding-only delegation row remains in the reward recipient list.
The exact example below misallocates one wei per distribution. No material
loss, extra emission, failing Rust regression or live reproduction is claimed.

The [audit policy](README.md), September reports and October catalogue through
F44 were checked for overlap, especially the
[pass-4 economics review](chain-cea1254-pass4-economics-2026-10-04.md),
[pass-7 staking configuration review](chain-cea1254-pass7-sol-governance-2026-10-04.md),
[pass-8 reward lifecycle review](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md)
and [pass-10 action review](chain-d52a33f-pass10-astra-actions-2026-10-04.md).
No applicable ancestor/repository `AGENTS.md` was found. Cargo/rustc are absent
from PATH. Every Rust test cited here was **read, not executed**. Only this
report was written; the parent owns Git and Torus records. Certificate admission,
malformed inputs and interrupted pass-5 adversarial work were not resumed.

## S01 / F45 — P3: unbonding-only rows receive validator reward dust

**Preconditions.** A normal Active validator has at least two positive external
delegations. An additional delegator has fully undelegated but has not deleted
its row through a completed ClaimUnbonded. That account sorts after all positive
delegators for this validator. The post-commission pool has a nonzero remainder
after flooring the positive delegators' shares. All stake and actions are real,
affordable and valid; no failed write or malformed state is required.

**Caller and state proof.** Signed native Delegate/Undelegate variants reach
the ordinary [staking dispatch](../../crates/torus-bridge/src/native_executor.rs#L4100)
and [handlers](../../crates/torus-bridge/src/native_executor.rs#L7551).
[Undelegate](../../crates/torus-economics/src/staking.rs#L143) reduces active
`Delegation.amount` to zero on a full exit, adds the principal to `unbonding`,
persists the delegation and subtracts it from `ValidatorState.total_delegated`.
Keeping that row is necessary to preserve the delayed principal claim.

However, [delegations_for_validator](../../crates/torus-economics/src/staking.rs#L1070)
returns every row with the matching validator suffix, including amount-zero
rows. The backend contract requires
[sorted key order](../../crates/torus-state/src/backend.rs#L38), and delegation
keys order by delegator before validator. The
[production overlay merge](../../crates/torus-state/src/backend.rs#L1848)
also preserves that order and sees the current block's undelegation writes.

The application runs the native phase on every
[height-based epoch boundary](../../crates/torus-consensus/src/app.rs#L1945),
then [calls epoch processing after actions](../../crates/torus-consensus/src/app.rs#L2307).
The [epoch hook](../../crates/torus-bridge/src/native_executor.rs#L8411) calls
validator inflation for the current Active set before applying rotation.
[Inflation](../../crates/torus-economics/src/rewards.rs#L232) obtains the
unfiltered delegation list, chooses its final row as the remainder recipient,
and credits `delegator_pool - del_distributed` there without testing its active
amount. Earlier zero rows multiply by zero; the last row bypasses that formula.

**Exact source-derived example.** Use epoch length 43,200 and four Active
validators with distinct valid consensus keys and self-stake 10,000 TRS each.
Let V sort last among those validators and have zero commission. A and B each
delegate 1 TRS to V. C also delegates 1 TRS, then normally undelegates its full
1 TRS before the observed boundary. A < B < C in delegator address order, and
no other delegation to V exists. Observe the first distribution after C's exit,
before its claim. Unchanged membership and sufficient liquid funding remove
unrelated admission and rotation conditions.

The [compiled rate and block-year constants](../../crates/torus-economics/src/types.rs#L347)
and [emission/validator allocation](../../crates/torus-economics/src/rewards.rs#L183)
give the following exact wei amounts:

```text
total active stake = 40,002 * 10^18
global emission = floor(40,002 * 10^18 * 500 * 43,200
                        / (15,768,000 * 10,000))
                = 5,479,726,027,397,260,273
each earlier validator = floor(global emission * 10,000 / 40,002)
                       = 1,369,863,013,698,630,136
V's pool = global emission - 3 * earlier validator
         = 1,370,136,986,301,369,865
A receives floor(V's pool / 2) = 685,068,493,150,684,932
B receives floor(V's pool / 2) = 685,068,493,150,684,932
C receives V's pool - A - B    = 1
```

Filtering zero active stake would make B the last eligible row, giving B
685,068,493,150,684,933 and C zero. C's new liability is real:
[credit_rewards](../../crates/torus-economics/src/staking.rs#L982) persists it,
[ClaimRewards](../../crates/torus-bridge/src/native_executor.rs#L7592) reaches
the [balance-credit/delete transition](../../crates/torus-economics/src/staking.rs#L406),
and [claim admission is funding-exempt](../../crates/torus-mempool/src/funded.rs#L73).
This does not require C to retain liquid EVM funds after its delegation.

**Consequence and bounds.** Active delegators lose the remainder to an account
with zero active stake. Total allocations still equal the pool; aggregate
conservation tests therefore pass. With m positive delegation rows whose
amounts sum to `total_delegated`, the misplaced remainder is an integer smaller
than m wei per distribution. This is deterministic dust allocation, not an
unbounded payout, mint or principal loss. The same state can produce the same
small error at later boundaries, including after principal maturity if the
user has not claimed: maturity alone does not remove the row.

The [fee-share loop](../../crates/torus-economics/src/rewards.rs#L106) and
[FeeSplitter helper](../../crates/torus-economics/src/rewards.rs#L423) repeat
the same final-row logic. They are the same root, not extra findings. A positive
fee validator share is an additional prerequisite; existing F07 keeps ordinary
zero-genesis-epoch headers on the zero-validator-share schedule. The production
inflation fixture above uses height boundaries and does not depend on repairing
F07 or invoking a dormant helper.

**Counterexamples.** An amount-zero row before the last positive delegator
receives zero. A pool divisible across the positive amounts leaves no dust.
One positive delegator receives the whole pool exactly before a trailing zero
row, so that simpler fixture does not fail. With no positive delegated stake,
the explicit `total_delegated == 0` branch gives the pool to the validator.
A completed [ClaimUnbonded](../../crates/torus-economics/src/staking.rs#L268)
deletes a fully drained zero-amount row, making the final positive delegator the
remainder recipient again. ClaimRewards deletes only the reward liability;
it does not delete the unbonding row. If C has another unmatured unbonding entry,
claiming only the matured entry retains the zero-active row and this condition.

**Regression gap and focused repair.** The existing
[validator inflation delegation test](../../crates/torus-economics/src/rewards.rs#L710)
uses two positive delegators, checks commission and total conservation, and
compares the larger delegation's reward. The
[exact fee commission test](../../crates/torus-integration-tests/tests/staking_lifecycle.rs#L279)
also keeps both delegators positive. Neither composes full undelegation with
a later reward distribution. The
[multi-validator exit test](../../crates/torus-integration-tests/tests/staking_lifecycle.rs#L375)
checks principal release without distributing rewards between exit and claim.

Add a failing regression through successful native actions and the actual
epoch hook using the four-validator example. Assert C's active amount is zero,
its unbonding principal remains 1 TRS, V's `total_delegated` is 2 TRS, and the
individual reward deltas equal the eligible allocation above. Check that all
liabilities still sum to the same global emission. Include C before versus
after B in address order, a divisible pool, one positive delegator, zero active
delegated stake, and completed versus partial ClaimUnbonded. Repeat the exact
recipient assertions after a clean database reopen with identical configuration.
Filter the reward recipient list to positive active amounts before choosing
the last row; retain the unbonding row itself for principal. Apply that rule
consistently to each maintained distribution implementation.

## Claim and persistence paths: coverage qualifications

The historical no-completion-path statement is obsolete for this source.
The [wallet](../../tools/wallet/src/commands/staking.rs#L44),
[native handler](../../crates/torus-bridge/src/native_executor.rs#L7604) and
[queued writer conversion](../../crates/torus-bridge/src/native_executor.rs#L8648)
reach ClaimUnbonded. Its [manager implementation](../../crates/torus-economics/src/staking.rs#L249)
partitions each row by persisted release height, sums principal with checked
arithmetic, checks the liquid account credit and submits the revised rows plus
account in one atomic operation. The
[executor maturity fixture](../../crates/torus-bridge/tests/claim_unbonded_tests.rs#L147)
asserts exact principal at maturity, deletion and a failed repeat claim;
[partial maturity](../../crates/torus-bridge/tests/claim_unbonded_tests.rs#L168)
and [multiple validators](../../crates/torus-bridge/tests/claim_unbonded_tests.rs#L204)
have substantive coverage. The
[atomic success test](../../crates/torus-economics/tests/claim_unbonded_atomic_tests.rs#L140)
also asserts exact 300-unit release while preserving active amounts and
unmatured entries. No additional clean-success claim defect was established.

Reward credit and claim writes use the same StateBackend as delegation.
Successful serial execution
[flushes native state with the applied-height marker](../../crates/torus-consensus/src/app.rs#L2462).
The reward candidate persists because the stored recipient liability is wrong,
not because reload changes its encoding. No separate successful staking commit
requiring repeated applied-boundary execution was found. This does not close
F19's configuration substitution or F30's later EVM mirror seam, and no process
restart or intact snapshot restoration was executed in this pass.

## Settled RPC composition: no additional projection defect established

[get_staking_info](../../crates/torus-economics/src/queries.rs#L49) exposes
active delegation amounts separately from unbonding entries, permanent principal
and pending reward liabilities. The
[RPC projection](../../crates/torus-rpc/src/torus.rs#L1182) keeps those values
separate. A zero active delegation alongside nonzero unbonding is therefore a
legitimate exit state. The
[staking precompile](../../crates/torus-core/src/precompiles.rs#L780) sums active
delegated amounts and selects its first validator only from a positive row;
it does not mistake unbonding principal for active delegation. Existing F41's
Quantity encoding concern remains separate from the numeric state projection.

Permanent rewards [credit liquid balances directly](../../crates/torus-economics/src/rewards.rs#L153),
whereas validator/delegator rewards enter pending liabilities. Thus a zero
`pendingRewards` after a permanent-stake reward does not demonstrate a missing
payout. ClaimUnbonded releases principal without consuming pending rewards;
ClaimRewards consumes pending rewards without releasing principal. In a
boundary block, actions precede the later inflation hook, so a successful
ClaimRewards can be followed by a fresh liability in the same block's poststate.
The [prior pass-8 review](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md)
already describes this ordering and the observed boundary-snapshot policy.

The [HTTP staking fixture](../../crates/torus-rpc/tests/torus_staking_gov_rpc_tests.rs#L135)
does use serialized manager rows, but checks one delegation and merely nonzero
permanent/pending values, with no unbonding. A useful composed regression should
execute ordinary stake, partial/full exit, reward, separate claims and clean
close/reopen, then compare exact RPC values with canonical rows and balances
at a settled height. Include a permanent staker and two validator delegations.
This is a precise coverage recommendation, not evidence of a second defect or
a claim that all existing persistence tests are absent.

Historical self-stake reward policy, unslashable unbonding principal, jail-vote
repetition, tombstoned top-ups, F07/F09/F28/F34 and quiet-boundary handling retain
their prior provenance. The arithmetic shown above was checked with a small
inline Python calculation only; it did not execute production Rust, signatures
or consensus. The bounded review ends with S01/F45 and these qualifications.
