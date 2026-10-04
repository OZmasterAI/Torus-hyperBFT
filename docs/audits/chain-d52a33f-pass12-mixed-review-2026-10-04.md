# Pass 12 — feeder, staking and governance

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`11756e8bdd8db4fe95a18a42a25d37e6aca536b8`. Production source remains
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`. This batch adds review documents
without changing production code or fetching a newer revision.

**Result: one additional P3 source-supported candidate, F45.** Independent
review supports a small reward-recipient allocation error. Feeder observations
remain operational and scaling qualifications; governance adds caller and
test-coverage clarifications without another finding. The
[audit policy](README.md) still requires failing production-code regressions.
Cargo/rustc are unavailable, so Rust tests were read, not run.

| Reviewer | Scope | Report |
| --- | --- | --- |
| GPT-6.1-sol | Price-feeder configuration, aggregation, submission and health | [Feeder review](chain-d52a33f-pass12-sol-feeder-2026-10-04.md) |
| GPT-6.1-sol | Delegation, unbonding, reward allocation and claims | [Staking review](chain-d52a33f-pass12-sol-staking-2026-10-04.md) |
| GPT-6-astra | Governance timing and supported payload consumers | [Governance review](chain-d52a33f-pass12-astra-governance-2026-10-04.md) |
| GPT-6-astra | Independent falsification of the leads and test claims | [Independent recheck](chain-d52a33f-pass12-astra-recheck-2026-10-04.md) |

## F45 — P3: a zero-active delegation can receive reward rounding residue

A normal [full undelegation](../../crates/torus-economics/src/staking.rs#L143)
persists a delegation row with `amount = 0` and queued unbonding principal.
The [validator's delegation scan](../../crates/torus-economics/src/staking.rs#L1070)
includes that row. During
[validator inflation distribution](../../crates/torus-economics/src/rewards.rs#L231),
each nonfinal row receives its floored proportional share, while the final row
receives the residual without checking whether its active amount is positive.
The [backend's sorted iteration contract](../../crates/torus-state/src/backend.rs#L38)
and delegator-first keys make the greatest-address matching delegator the final
row. Consequently a fully exiting delegator can receive reward dust while
active delegators lose the corresponding residual.

Use four ordinary Active validators with distinct valid keys and 10,000 TRS
self stake each. Let V be the greatest validator address and have zero
commission. A and B each actively delegate 1 TRS to V; C, whose address is
greater than A and B, previously delegated 1 TRS and fully undelegated it.
Leave C's unbonding row present. At an epoch length of 43,200 blocks, the
source formula produces these amounts:

| Allocation | Wei |
| --- | ---: |
| Global validator inflation | 5,479,726,027,397,260,273 |
| Each of the three preceding validators | 1,369,863,013,698,630,136 |
| V's emission after validator-level rounding | 1,370,136,986,301,369,865 |
| A's reward | 685,068,493,150,684,932 |
| B's reward | 685,068,493,150,684,932 |
| C's reward despite zero active delegation | 1 |

The coordinator independently recalculated these integers with Python. This
checks arithmetic only, not the Rust reward path or a running chain. The
[production epoch hook](../../crates/torus-bridge/src/native_executor.rs#L8426)
calls validator inflation before rotation; the example does not need a
positive validator transaction-fee share or resolution of F07's inherited
header epoch. The related fee-sharing loops have the same recipient-selection
pattern, but epoch inflation supplies the primary production path.

This is a bounded recipient-allocation error, not extra total emission,
principal loss or demonstrated material loss. Conservation alone still passes.
At least two positive delegations and a nonzero rounding residual are needed
for this fixture. No residual means no wrongful credit; a positive final row
receives the residual legitimately. A successful mature
[ClaimUnbonded](../../crates/torus-economics/src/staking.rs#L267) removes C's
fully drained zero-active row and ends this prerequisite. Re-delegation also
changes the fixture.

Filter reward recipients to positive active amounts before selecting the
residual recipient, while retaining rows needed for principal claims. Regress
real delegation, full undelegation, the epoch hook and reward claims with
independent exact recipient expectations: C receives zero, the final eligible
delegator receives the residual, and the total emitted amount is unchanged.
Include exact-division, partial-undelegation and completed-principal-claim
controls. Existing tests check useful totals/proportions but do not establish
this eligibility rule; the test named `validator_inflation_zero_stake` creates
no validators rather than a retained zero-active delegation.

## Leads retained without new finding numbers

- **Feeder partial admission visibility.** Successful aggregation sets each
  market's omission field to `None` before submission. With two chunks, one
  accepted and the other explicitly rejected as overloaded, the rejected
  markets retain that field and any recent success keeps overall health `ok`.
  Logs, cycle errors and submission-result counters still expose the failure.
  The README defines health using any recent accepted submission plus
  aggregation omissions/venue failures; it does not clearly promise accepted
  submissions for every market. Independent review therefore treats this as
  a monitoring limitation and a proposed contract improvement, not a new
  numbered defect. Admission and on-chain execution are separate events.
- **More than four disjoint feeder chunks.** The builder can produce five
  chunks while one node retains only four oracle submissions per validator.
  If all five arrive before selection, the newest can evict a different
  market chunk. A realistic supported set of over 1,024 quoted markets and
  repeated producer timing were not established. Other nodes may already
  hold or select earlier submissions. Keep this as a conditional scaling
  regression; no indefinite market starvation is claimed.
- **Governance reachability narrows failure scenarios.** Signed ListMarket
  always requests execution-time automatic ID allocation. Two manager-level
  explicit-ID listings do not establish an ordinary signed collision path.
  TreasurySpend and PermanentUnlock likewise have economic handlers but no
  corresponding signed proposal variants. The historical scheduler's error
  propagation limitation remains open without overstating these callers.
- **Whitelist tests are not complete registration workflows.** Some tests
  create approval at height 116, then query height 115 against that later
  state. This is not chronological evidence of early on-chain approval.
  Direct StakingManager registration also bypasses NativeExecutor's whitelist
  consumption. Production writes approval at execution and measures expiry
  from it; boundary, consumption and restart regressions remain useful.
- **Earlier roots retain their provenance.** Governance snapshot weights,
  disconnected approved settings, quiet-block scheduling and ignored genesis
  settings remain F09/F27/F29/F34. Current ClaimUnbonded has real production
  callers, and empty epoch boundaries run rewards. Older contrary reports
  must not be treated as current behavior.

## Verification and handoff

All four reviewers completed their assigned reports. The coordinator traced
the reward recipient path, checked both arithmetic examples, and reconciled
the feeder health classification with its documented contract. Local links,
source-line bounds and documentation whitespace were checked before commit.
There were no Rust builds/tests, exchange calls, live-chain transactions,
service or key operations, production edits or resumed blocked pass-5 work.

F45 was saved as Torus discovery `e31a778f-1389-4dcb-88bc-63b63504b60f`.
Its initial arithmetic note used a single-validator illustration; the report's
four-validator fixture strengthens the same source claim. No finding was
resolved. The next implementation step is the focused production regression
and then a repair that preserves unbonding principal and total rewards.
