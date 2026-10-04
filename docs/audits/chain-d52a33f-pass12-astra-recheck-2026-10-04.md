# Pass 12 — independent falsification and test-path recheck

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`11756e8bdd8db4fe95a18a42a25d37e6aca536b8`. Production source is
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: F45 survives as a narrow P3 reward-recipient candidate. The feeder
partial-admission lead remains an unnumbered operational limitation.** This
review adds no separate finding count. The ordinary governance review's
reachability and test qualifications also survive the independent spot checks
below.

The [audit policy](README.md), September reports, relevant October catalogue
and economics/lifecycle reports, and all three saved pass-12 peer reports were
checked for overlap. No applicable ancestor or repository `AGENTS.md` was
found. Torus status identified this worktree and no newer related checkpoint;
its records were treated as historical data. Cargo and rustc are absent from
PATH. Rust tests were **read, not run**. One inline Python calculation checked
the bounded F45 arithmetic; it is not a production-code reproduction. Only this
report was written. No source edits, Git mutations, Torus writes, installation,
services, live-chain calls or key operations occurred. Blocked pass-5
certificate, malformed-input and adversarial assignments were not resumed.

## F45: the zero-active recipient is reachable through normal exit

The [staking peer report](chain-d52a33f-pass12-sol-staking-2026-10-04.md)
describes full undelegation followed by epoch inflation. Four potential
counterarguments do not remove this fixture:

1. **Full exit does not delete the row.**
   [Undelegate](../../crates/torus-economics/src/staking.rs#L142) subtracts
   the active amount, appends an unbonding entry, persists the row and reduces
   `total_delegated`. The zero-amount row is necessary to preserve principal.
   The native [Undelegate handler](../../crates/torus-bridge/src/native_executor.rs#L7563)
   invokes that transition directly with the actual block height.
2. **The recipient scan does not filter active amounts.**
   [delegations_for_validator](../../crates/torus-economics/src/staking.rs#L1070)
   filters the validator suffix only. The
   [backend contract](../../crates/torus-state/src/backend.rs#L38),
   [RocksDB iteration](../../crates/torus-state/src/backend.rs#L151), and
   [overlay merge](../../crates/torus-state/src/backend.rs#L1848)
   establish sorted delegator-first keys, including pending overlay writes.
   The last address in this validator's rows can therefore be the exited user.
3. **The last row bypasses pro-rata multiplication.**
   [Validator inflation](../../crates/torus-economics/src/rewards.rs#L232)
   gives all earlier rows `pool * amount / total_delegated`, but gives the
   last row `pool - distributed`. Its zero active amount is never checked in
   that branch. [credit_rewards](../../crates/torus-economics/src/staking.rs#L982)
   writes an ordinary pending liability for the resulting nonzero dust.
4. **The ordinary boundary hook runs independently of F07.**
   [The native gate](../../crates/torus-consensus/src/app.rs#L1945)
   explicitly includes epoch boundaries. The
   [epoch hook](../../crates/torus-bridge/src/native_executor.rs#L8411)
   distributes inflation using the current Active set before rotation. This
   is not dependent on a positive validator fee share or on fixing inherited
   header epochs.

The independently recalculated fixture is four Active validators with
10,000 TRS self stake each, V last in validator address order and commission
zero, and A < B < C among V's delegators. A and B retain 1 TRS each; C fully
undelegates its 1 TRS and has not claimed it. Use an actual configured epoch
length of 43,200, no other delegations and no intervening membership/stake
change. All accounts have sufficient funding for their valid actions. The
[rate/year constants](../../crates/torus-economics/src/types.rs#L347) and
[validator allocation](../../crates/torus-economics/src/rewards.rs#L183)
produce:

| Quantity | Exact wei |
| --- | ---: |
| Global emission on 40,002 TRS active stake | 5,479,726,027,397,260,273 |
| Each of the three earlier validators | 1,369,863,013,698,630,136 |
| V's post-commission pool | 1,370,136,986,301,369,865 |
| A's reward delta | 685,068,493,150,684,932 |
| B's reward delta | 685,068,493,150,684,932 |
| C's reward delta despite zero active delegation | 1 |

The inline integer calculation asserted every value and the exact global sum.
This shows a one-wei recipient error while conservation still holds. It does
not establish material financial loss, extra minting, a live incident or a
passing/failing Rust test. Historical self-stake-versus-external-delegator
allocation policy remains separate: the fixture follows the existing pool
policy and challenges only the zero-active recipient.

The counterexamples constrain both the claim and its regression. One positive
delegator consumes the entire pool before a trailing zero row; a divisible
pool leaves no dust; an earlier zero row multiplies by zero; and
[`total_delegated == 0`](../../crates/torus-economics/src/rewards.rs#L224)
routes the pool to the validator. With m positive rows summing to the recorded
total, the residual assigned to a final zero row is less than m wei per
distribution. Maturity alone does not remove the row.
[ClaimUnbonded](../../crates/torus-economics/src/staking.rs#L249) deletes it only
when the active amount is zero and no unbonding entries remain; a partial
maturity claim can leave it eligible for the same wrong residual branch.

Preserve the exit row and principal in any repair. Filter eligible reward
recipients before choosing the final remainder recipient. A regression should
execute normal delegation/full exit and the production boundary hook, assert
the individual **reward deltas**, and retain the unchanged global emission
assertion. B should receive the extra wei and C zero. Exact deltas avoid
confusion with rewards already pending before the fixture. Include address
order, divisibility, one/zero positive delegation, full/partial claim and
clean-reopen controls. Related fee loops are the same root, not extra findings;
inflation provides the current ordinary caller without F07's fee prerequisite.

## Feeder partial admission: observable limitation, no new defect number

The [feeder peer report](chain-d52a33f-pass12-sol-feeder-2026-10-04.md)
correctly identifies a possible monitoring blind spot. In a configured set of
257 listed matching markets with valid fresh source aggregates, the
[builder](../../tools/price-feeder/src/submit.rs#L12) creates two ordered
chunks. [Aggregation](../../tools/price-feeder/src/feeder.rs#L199) installs
`omitted = None` before submission. In
[the submit loop](../../tools/price-feeder/src/feeder.rs#L305), success refreshes
one global timestamp, while failure increments counters and records an error
without changing the affected markets' status.

Use an explicit pre-admission rejection for the second chunk, such as the
single-submit [semaphore overload](../../crates/torus-rpc/src/torus.rs#L1330),
after the first returns a hash. A timeout does not prove non-admission. With
healthy venues and no authorization error,
[health::level](../../tools/price-feeder/src/health.rs#L32) then returns Ok and
the failed chunk's market still has `omitted = None`. Repeating this split
outcome can keep the global liveness timestamp current indefinitely.

However, the [README contract](../../tools/price-feeder/README.md#L93) and
[design health contract](../plans/oracle-feeder.md#L286) define Down using any
recent accepted submission and Degraded using market omissions/venue failures.
Neither explicitly promises per-market admission tracking. The
[`None = submitted` comment](../../tools/price-feeder/src/feeder.rs#L28)
does not unambiguously mean accepted; both chunks were attempted. The
implementation follows the narrow global liveness rule. A recent success
followed by one rejected cycle also remains within the documented
three-interval grace period. That grace period is not a fresh defect.

The proper result is an **unnumbered low-priority monitoring enhancement**:
make per-market attempted/accepted status explicit, or add partial-cycle
failure state if that is the desired operational contract. Exact failed-chunk
market assertions become regression requirements after choosing that stronger
contract. Existing logs, `CycleReport.errors`, success/failure counts and
`feeder_submit_total{result}` already expose rejection; no claim of complete
error concealment is supported. A success hash is admission, not execution,
quorum or finality. Other validators may still update the same market.

Config validation has no total-market ceiling, but that fact alone does not
establish a current 257-market deployment or recurring overload schedule.
The separate five-chunk/four-pending scaling sequence is likewise conditional:
a new fifth pending admission can replace the oldest before selection. Without
an established ordinary 1,025-usable-market deployment and the relevant
selection/propagation timing, this review does not promote market starvation
or permanent price loss from that sequence.

## Tests: asserted behavior versus inferred coverage

All entries below were inspected as source. None was executed in this pass.

| Test | Actual assertion or path | Limit relevant to this review |
| --- | --- | --- |
| [validator_inflation_with_delegators](../../crates/torus-economics/src/rewards.rs#L710) | Two positive delegators; exact commission, conserved total, and larger delegation gets more. | No full exit or zero-active row. Total conservation alone passes F45. |
| [validator_inflation_zero_stake](../../crates/torus-economics/src/rewards.rs#L774) | Creates no validators and asserts zero emission. | Its name is not evidence of zero-amount delegation coverage. |
| [undelegate_and_unbonding_timing](../../crates/torus-economics/src/staking.rs#L1338) | Partial undelegation, zero early release, exact release at maturity. | No inflation between exit and claim; active amount remains positive. |
| [chunks_at_the_cap](../../tools/price-feeder/src/submit.rs#L115) | 600 synthetic valid prices yield chunk sizes 256/256/88. | No fetching, config/listing validation, RPC admission or health composition. |
| [every_built_submission_passes_exec_rules](../../tools/price-feeder/src/submit.rs#L126) | Checks cap, unique IDs, listed membership, valid prices and total retained count. | Does not invoke the actual executor or establish signer/time/admission validity despite its broad name. |
| [busy_is_retried_next_cycle_with_higher_nonce](../../tools/price-feeder/src/feeder.rs#L506) | One failed chunk, retry classification, increased nonce and later success/counters. | Does not compose mixed success/rejection within a cycle or per-market admission state. |
| [status_ok_degraded_down](../../tools/price-feeder/src/health.rs#L198) | Hand-built status covers omission, venue error, exact 9,000-ms grace boundary, expiry and authorization error. | This explicitly supports the global grace contract; it does not drive the real feeder loop. |
| [two_auto_id_listings_get_distinct_ids](../../crates/torus-economics/tests/governance_tests.rs#L722) | Two manager proposals execute and leave IDs 1 and 2. | Substantive allocator coverage, without signed admission or next-block market consumption. |
| [test_registration_without_governance_rejected](../../crates/torus-integration-tests/tests/dynamic_validators.rs#L140) | Calls `is_whitelisted` twice and asserts false. | No rejected registration action is executed. |

The [governance peer report](chain-d52a33f-pass12-astra-governance-2026-10-04.md)
also correctly rejects a new signed explicit-ID collision story. The signed
[ListMarket conversion](../../crates/torus-bridge/src/native_executor.rs#L8031)
always supplies auto ID zero. The
[explicit-ID collision test](../../crates/torus-economics/tests/governance_tests.rs#L797)
does establish manager-level rejection and row preservation, but does not
supply ordinary signed reachability for that broader API.

The whitelist test helper
[executes approval at block + 16](../../crates/torus-integration-tests/tests/dynamic_validators.rs#L99),
then its positive test queries height 115 after execution at 116. That is a
later-state manager query with an earlier supplied height, not a chronological
chain demonstration of approval before execution. The peer's coverage
qualification is appropriate; no new early-activation finding follows.

## Evidence and remaining work

F45 is source-supported and independently arithmetically checked, still an
unreproduced candidate under the audit convention. The feeder observation does
not establish a violated per-market acceptance contract. No existing finding
is closed by this report. The next useful correctness check is F45's exact
recipient regression through ordinary native actions and the real epoch hook.
Relative local links and source-line bounds in this report were validated;
that document check is not runtime correctness verification.
