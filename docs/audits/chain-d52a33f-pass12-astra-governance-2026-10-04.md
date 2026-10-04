# Pass 12 — ordinary governance lifecycle and effective payloads

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`11756e8bdd8db4fe95a18a42a25d37e6aca536b8`. Production source remains
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`; a scoped Git diff showed no
`crates/` changes against that revision.

**Result: no additional production correctness candidate is promoted.** This
review follows ordinary proposal submission, voting, finalization, execution,
and supported payload readers. It adds precise activation boundaries and
coverage qualifications without recounting F09, F27, F29, F34 or the historical
unsupported market operations. It does not close those findings.

The audit README, September Astra reports, October main reports through pass
11, governance/economics companions from passes 4/7, pass-8 lifecycle review,
and historical economic-audit/fix notes were compared for provenance. No
applicable ancestor or repository `AGENTS.md` was found. Cargo and rustc were
not on PATH. All Rust tests below were **read, not run**; no runtime reproduction,
passing regression, deployed-chain observation or verified fix is claimed.
Only this report was written. No source edits, Git mutations, Torus writes,
installation, network/live-chain or key actions were performed. The parent
owns report reconciliation and persistence. Blocked pass-5 certificate,
malformed-input and adversarial assignments were not resumed.

## Ordinary timing contract

Submission requires real delegated/permanent principal meeting
`min_proposal_stake`, then persists an Active proposal with
`end_block = current_block + voting_period_blocks` and snapshots voter weights
([submission](../../crates/torus-economics/src/governance.rs#L727),
[stored heights](../../crates/torus-economics/src/governance.rs#L762)).
This is an immediate per-action snapshot, not a separately reconstructed
historical end-of-block snapshot. A voting test should hold stake fixed when
isolating timing, so F09's known weight behavior does not confound it.

The actual boundaries are internally consistent:

| Stage | Production predicate and effect |
| --- | --- |
| Yes/no or abstain at `end_block` | Allowed while Active: rejection uses `current_block > end_block` in [yes/no](../../crates/torus-economics/src/governance.rs#L808) and [abstain](../../crates/torus-economics/src/governance.rs#L869). Both share the same per-proposal/voter duplicate record. |
| Finalization | Requires `current_block > end_block`; status becomes Rejected or Passed. Passed stores `executable_after = actual_finalization_block + timelock_blocks` ([finalization](../../crates/torus-economics/src/governance.rs#L911)). |
| Payload execution | Requires Passed and `current_block >= executable_after`; only successful payload execution changes status to Executed ([execution](../../crates/torus-economics/src/governance.rs#L969)). |
| Text-only result | Remains Passed, and the scheduler excludes payload-less proposals from repeated execution ([scheduler](../../crates/torus-economics/src/governance.rs#L1028)). |

There is no evidence here of an ordinary off-by-one bypass of the timelock.
The scheduler finalizes all due Active proposals before scanning Passed ones
([ordering](../../crates/torus-economics/src/governance.rs#L1019)). With a positive
timelock, a newly finalized proposal cannot execute during that same scheduler
call. Production governance runs after user actions/liquidation, before fee
distribution and the epoch hook
([block tail](../../crates/torus-consensus/src/app.rs#L2305)). A valid vote at the
last permitted height is therefore included before later finalization.

This qualification does **not** resolve F29. The native-phase gate still omits
governance due-work from its triggers
([gate](../../crates/torus-consensus/src/app.rs#L1945)); idle nonboundary blocks
with no other native trigger can delay both transitions. The delay starts from
actual finalization, not the scheduled end height. The existing
[F29/E03 report](chain-cea1254-pass4-economics-2026-10-04.md) already covers that
root. A later valid but expired Vote can cause the native phase to run and then
be rejected by the voting deadline; delayed scheduler execution does not
extend the voting window.

## Supported payloads and their consumers

The signed [ProposalAction enum](../../crates/torus-types/src/lib.rs#L1178) and
[native conversion](../../crates/torus-bridge/src/native_executor.rs#L8024)
are the relevant reachability boundary. The broader economic payload enum is
not proof that every payload can be submitted by an ordinary signed action.

| Submitted action | Result and real consumer | Qualification |
| --- | --- | --- |
| ParameterChange | Writes the allowed ASCII key in `CF_FEE_CONFIG`, then reports Executed ([write](../../crates/torus-economics/src/governance.rs#L1049)). Governance consumers and the [configuration RPC](../../crates/torus-rpc/src/torus.rs#L1459) read the serialized `gov_params` record through [get_governance_params](../../crates/torus-economics/src/governance.rs#L1209). | Existing F27. Validated/executed does not mean the parameter reached its consumer. This review found no repair of that disconnect. |
| ListMarket | Signed conversion requests auto ID zero and converts leverage to initial-margin percent. Execution allocates an actual ID and writes the registry row ([conversion](../../crates/torus-bridge/src/native_executor.rs#L8031), [allocation/write](../../crates/torus-economics/src/governance.rs#L1090)). | The next native context loads listed-market [margin configurations](../../crates/torus-bridge/src/native_executor.rs#L2624), with the [row decoder](../../crates/torus-core/src/margin.rs#L233); oracle inputs load [listed IDs](../../crates/torus-bridge/src/native_executor.rs#L8308). Listing alone does not supply a usable oracle mark. |
| ValidatorRegistration | Creates an address whitelist whose approval/expiry heights start at execution ([write](../../crates/torus-economics/src/governance.rs#L1128)). The later RegisterValidator handler checks that entry, registers the candidate and consumes it ([consumer](../../crates/torus-bridge/src/native_executor.rs#L7658)). | Approval authorizes registration; it does not register the address or immediately install consensus membership. Successful registration creates [Candidate status](../../crates/torus-economics/src/staking.rs#L84). |
| UpdateMarketParams / DelistMarket | Maps to `None`, derives TextProposal and eventually remains Passed ([conversion](../../crates/torus-bridge/src/native_executor.rs#L8052), [type derivation](../../crates/torus-economics/src/governance.rs#L748)). | Historical inactive path, explicitly documented in the [parity limitations](../parity-audit-fixes-s515.md#L210) and [pass-8 lifecycle review](chain-d52a33f-pass8-astra-lifecycle-2026-10-04.md). No new claim that these update live risk configuration. |

ListMarket's activation at the block tail does not create a subsequent
same-block user action using stale configuration: user actions and liquidations
have already finished. The next block reloads the configuration. This agrees
with pass 8; it does not certify unknown-market handling, F25's tick/lot defaults,
ignored maintenance-margin input, or future live market updates.

Whitelist expiry is inclusive at its recorded height
([check](../../crates/torus-economics/src/governance.rs#L1232)). Because the
whitelist is created at the tail, a RegisterValidator action in that same block
cannot use the newly approved entry unless a prior approval already exists.
Registration in a subsequent block can use it, with expiry measured from
execution rather than proposal submission/finalization. This is consistent
with the stored record's purpose; it does not establish premature or shortened
on-chain approval.

## What the existing tests actually assert

| Test or group | Actual assertion read | Missing lifecycle evidence |
| --- | --- | --- |
| [process_pending_proposals_selective](../../crates/torus-economics/tests/governance_tests.rs#L578) | At height 150, an end-100 text proposal passes while end-150/end-200 proposals remain Active. | No consensus execution gate, quiet-block progression, executable payload or persistence/restart coverage. |
| [execute_parameter_change](../../crates/torus-economics/tests/governance_tests.rs#L346) | Direct manager submission/vote/finalize/execute produces the ASCII `max_leverage="50"` row and Executed status. | No production reader or changed subsequent governance behavior. Its comment says timelock 5, while [setup](../../crates/torus-economics/tests/governance_tests.rs#L48) supplies 10; execution at 120 is after both, so the assertion cannot distinguish them. |
| [proposal_has_executable_after_and_snapshot](../../crates/torus-economics/src/governance.rs#L1621) | Serializes/deserializes a manually constructed proposal and checks its two height fields. | No snapshot lookup, voting-boundary or early-execution rejection. |
| [two_auto_id_listings_get_distinct_ids](../../crates/torus-economics/tests/governance_tests.rs#L722) | Both approved manager-level listings execute through the scheduler at 120 and leave IDs 1 and 2. | No signed admission, block-tail execution or next-block production reader. This is substantive allocator coverage, not evidence of those additional stages. |
| [submit_listing_maps_initial_margin_from_max_leverage](../../crates/torus-bridge/tests/market_governance_exec_tests.rs#L154) | NativeExecutor dispatch produces a pending payload with auto ID 0 and 5% initial margin for 20x leverage. | It does not vote, finalize, execute the listing or place a later order. The sender is supplied directly, not recovered from a signed envelope. |
| [test_validator_registration_via_governance](../../crates/torus-integration-tests/tests/dynamic_validators.rs#L111) | The manager helper executes approval, then direct StakingManager registration produces Candidate status and the requested self stake. | Bypasses NativeExecutor's whitelist enforcement/consumption and its full-balance stake selection. It cannot prove registration consumed the approval. |
| [test_registration_without_governance_rejected](../../crates/torus-integration-tests/tests/dynamic_validators.rs#L140) | Checks `!is_whitelisted` twice. | No registration attempt is executed; the test comments explicitly acknowledge the executor boundary. |
| [test_whitelist_expiry](../../crates/torus-integration-tests/tests/dynamic_validators.rs#L158) | Checks a positive lookup and rejection at execution height plus expiry plus one. | No executor registration exactly at expiry, consumption, or chronological pre-approval check. |

The whitelist helper
[executes at `block + 16`](../../crates/torus-integration-tests/tests/dynamic_validators.rs#L99),
but the positive tests then query height 115 after execution at 116. The manager
only checks the upper expiry bound, so those assertions pass against the later
state. They are not a chronological chain fixture and do not demonstrate that
approval existed at block 115. In ordinary chronological execution the entry
does not exist before its execution write. This is a test qualification, not
a newly promoted early-activation defect.

The reviewed tests include useful successful execution and exact finalization
boundary assertions. They do not collectively establish the entire signed
proposal-to-consumer lifecycle. No Rust result is inferred from test names.

## Rejected leads and deduplication

- **A second pending explicit-ID listing does not establish a new ordinary
  signed failure path.** The economic API accepts explicit IDs and rechecks
  collisions at execution. The
  [collision test](../../crates/torus-economics/tests/governance_tests.rs#L797)
  proves its intended error and preservation of the existing row by source
  inspection. However, signed ListMarket always supplies zero, and
  execution-time allocation sees earlier same-block writes. The historical
  [failure-aborts-later-proposals limitation](../parity-audit-fixes-s515.md#L203)
  remains a valid manager/scheduler concern through
  [error propagation](../../crates/torus-economics/src/governance.rs#L1032), but
  this explicit-ID scenario is not an arbitrary signed caller for it.
- **TreasurySpend and PermanentUnlock tests do not broaden signed reachability.**
  The economic handlers and direct tests exist, but the signed ProposalAction
  enum has neither variant. A hypothetical insufficient treasury or vanished
  permanent stake must not be described as a presently reachable ordinary
  signed proposal without another caller. This retains pass 4's qualification.
- **Snapshot and abstention issues are existing F09-family work.** Yes/no
  [missing-row fallback](../../crates/torus-economics/src/governance.rs#L1312),
  abstention's [live weight](../../crates/torus-economics/src/governance.rs#L881),
  and finalization's [current quorum basis](../../crates/torus-economics/src/governance.rs#L928)
  remain distinct required assertions within the existing finding, not three
  additional findings. Abstain leaves yes/no totals unchanged but contributes
  its stored weight to quorum; the historical claim that it directly adds to
  opposition is not current tally behavior.
- **Genesis governance multiplier mismatch is F34.** Initial configuration
  propagation is already separate from F27's approved-parameter write. Neither
  is closed by successful serialization, configuration RPC output or manager
  setter tests.

## Focused next regressions

Use initialized ordinary state, funded valid signers and unchanged stake for
the timing/activation fixture. Set a short period through fixture setup, then
execute signed actions through the real block runner. Assert voting succeeds
at the inclusive end height, finalization first occurs later, the payload remains unapplied
at one below the recorded timelock and applies exactly at it, and subsequent
blocks do not reapply the payload. Include a reopen/restart between Passed and
execution and both serial/pipelined runners. Separately remove other native
triggers to exercise F29 rather than accidentally masking it.

For ValidatorRegistration, assert no whitelist before tail execution, its
recorded execution-based expiry, same-block versus next-block registration,
expiry-minus-one/exact/plus-one behavior, successful consumption, and Candidate
status before the epoch membership policy runs. For ListMarket, connect the
existing allocator and conversion tests to the following block's actual
margin/oracle readers, supplying the necessary valid oracle reports before
asserting order acceptance. For F27, assert an approved quorum change alters
the canonical configuration and a subsequent proposal's outcome, rather than
checking only an ASCII row. Keep F09's changing-stake/abstention cases in their
own fixture so successful timing coverage cannot conceal the known snapshot
limitations.

These are recommended production regressions, not tests added or executed in
this pass. Relative source links and line-anchor ranges were checked locally;
that document check is not runtime correctness verification.
