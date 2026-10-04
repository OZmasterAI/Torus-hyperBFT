# Audits

Static, read-only correctness reviews. Findings are candidates until a failing
test reproduces them.

- `astra-round1-2026-09-24.md`, `astra-round2-2026-09-24.md`: 43 candidates
  against `main` at `f05b20e`. They cover markets, units, governance, staking,
  genesis, EVM, RPC, the explorer and the wallet.
  `astra-round2-progress-2026-09-24.md` holds the round-two notes. The
  exclusion list and raw findings were not copied (1.4 MB). They remain in
  `/tmp/torus-astra-exploration-20260924/` for as long as that directory
  survives.
- The performance explorer synthesis is in
  `../perf/s65-explorer-synthesis-2026-09-27.md`.

## October 4 chain reviews

The reports preserve their reviewed revisions and verification limits. Passes
1–7 review `cea1254`; passes 8–12 review production source at `d52a33f`.
Passes 10, 11 and 12 start from documentation-only commits `feb01b6`,
`0c967ca` and `11756e8`.
Pass 13 reviews `merge/item6-c3-pf1` at `d9ef4f7` from a separate source
checkout. Its documents are committed to `audit/chain-findings-2026-10-04`
without merging that source branch; source links pin the reviewed commit.
Pass 14 continues at the same source revision from documentation commit
`f39a352`, using only GPT-6.1-sol reviewers as requested.
Pass 15 follows up on C3/PF1 at `d9ef4f7`. Pass 16 reviews C4 on
`perf/item6-phase1` at `0a25560`. Later passes add qualifications and
corrections; a repeated finding is not another defect.

| Pass | Main report |
| --- | --- |
| 1 | [Initial chain audit](chain-cea1254-2026-10-04.md) |
| 2 | [Second audit](chain-cea1254-pass2-2026-10-04.md) |
| 3 | [Project-wide audit](chain-cea1254-pass3-project-wide-2026-10-04.md) |
| 4 | [Missed-issues audit](chain-cea1254-pass4-missed-issues-2026-10-04.md) |
| 5 | [Incomplete pass status](chain-cea1254-pass5-status-2026-10-04.md) |
| 6 | [Correctness review](chain-cea1254-pass6-correctness-review-2026-10-04.md) |
| 7 | [Mixed-model review](chain-cea1254-pass7-mixed-review-2026-10-04.md) |
| 8 | [Updated-branch review](chain-d52a33f-pass8-mixed-review-2026-10-04.md) |
| 9 | [Lifecycle and history review](chain-d52a33f-pass9-mixed-review-2026-10-04.md) |
| 10 | [RPC, operations and independent recheck](chain-d52a33f-pass10-mixed-review-2026-10-04.md) |
| 11 | [Clients, state views and settlement](chain-d52a33f-pass11-mixed-review-2026-10-04.md) |
| 12 | [Feeder, staking and governance](chain-d52a33f-pass12-mixed-review-2026-10-04.md) |
| 13 | [Whole-project review at the C3/PF1 integration head](chain-d9ef4f7-pass13-project-wide-2026-10-04.md) |
| 14 | [Sol 6.1 follow-up: history, economics, storage and client recovery](chain-d9ef4f7-pass14-sol-review-2026-10-04.md) |
| 15 | [C3/PF1 integration and critical-path follow-up](chain-d9ef4f7-pass15-follow-up-2026-10-04.md) |
| 16 | [C4 liquidation cache review at the latest item 6 head](chain-0a25560-pass16-c4-review-2026-10-04.md) |
| 17 | [Staking, rewards and validator rotation](chain-0a25560-pass17-staking-rotation-2026-10-04.md) |
| 18 | [Consensus validation and crash recovery](chain-0a25560-pass18-consensus-recovery-2026-10-04.md) |
| 19 | [Execution durability and flush pipeline](chain-0a25560-pass19-execution-durability-2026-10-04.md) |
| 20 | [DA body custody and async validation](chain-0a25560-pass20-da-async-validation-2026-10-04.md) |
| 21 | [Cross-path action accounting and replay](chain-0a25560-pass21-action-accounting-replay-2026-10-04.md) |
| 22 | [Staking unbonding configuration](chain-0a25560-pass22-staking-unbonding-config-2026-10-04.md) |
| 23 | [Delegation exit and reward eligibility](chain-0a25560-pass23-delegation-reward-eligibility-2026-10-04.md) |
| 24 | [Staking claim atomicity](chain-0a25560-pass24-staking-claim-atomicity-2026-10-04.md) |
| 25 | [Commission genesis bounds](chain-0a25560-pass25-commission-genesis-bounds-2026-10-04.md) |
| 26 | [Validator inflation units](chain-0a25560-pass26-validator-inflation-units-2026-10-04.md) |
| 27 | [Permanent-stake rewards and unlock](chain-0a25560-pass27-permanent-stake-rewards-2026-10-04.md) |
| 28 | [Fee split allocation and rounding](chain-0a25560-pass28-fee-split-allocation-2026-10-04.md) |
| 29 | [Fee burn and supply reporting](chain-0a25560-pass29-fee-burn-supply-tracker-2026-10-04.md) |
| 30 | [EVM fee revenue and beneficiary accounting](chain-0a25560-pass30-evm-fee-revenue-double-credit-2026-10-04.md) |
| 31 | [Repeated jail-vote slashing](chain-0a25560-pass31-repeated-jail-vote-slashing-2026-10-04.md) |
| 32 | [Lockbox boundary and delayed settlement](chain-0a25560-pass32-lockbox-boundaries-2026-10-04.md) |
| 33 | [CoreWriter queue and staking roundtrip](chain-0a25560-pass33-corewriter-staking-roundtrip-2026-10-04.md) |
| 34 | [Perpetual funding configuration and settlement](chain-0a25560-pass34-perpetual-funding-config-2026-10-04.md) |
| 35 | [Backstop with unmarked positions](chain-0a25560-pass35-backstop-unmarked-collateral-2026-10-04.md) |
| 36 | [Fee split write failure atomicity](chain-0a25560-pass36-fee-split-error-atomicity-2026-10-04.md) |
| 37 | [Stage-1 cooldown full-position semantics](chain-5c756ff-pass37-liquidation-cooldown-full-orders-2026-10-04.md) |
| 38 | [Cooldown begins after an unfilled chunk attempt](chain-5c756ff-pass38-liquidation-zero-fill-cooldown-2026-10-04.md) |
| 39 | [Chunk quantity rounding and minimum-lot fallback](chain-5c756ff-pass39-liquidation-chunk-lot-rounding-2026-10-04.md) |
| 40 | [Stage-1 cross-market liquidation priority](chain-5c756ff-pass40-liquidation-market-priority-2026-10-04.md) |
| 41 | [Liquidation health classification at the two-thirds boundary](chain-5c756ff-pass41-liquidation-health-boundaries-2026-10-04.md) |
| 42 | [Liquidation slippage cap by margin tier](chain-5c756ff-pass42-liquidation-slippage-cap-2026-10-04.md) |
| 43 | [Reduce-only allowance at matching time](chain-5c756ff-pass43-reduce-only-match-allowance-2026-10-04.md) |
| 44 | [Empty-block scheduling for pending liquidations](chain-5c756ff-pass44-liquidation-empty-block-due-2026-10-04.md) |
| 45 | [Round-robin liquidation cursor boundary](chain-5c756ff-pass45-liquidation-cursor-round-robin-2026-10-04.md) |
| 46 | [ADL candidate scan window and progress](chain-5c756ff-pass46-adl-candidate-scan-progress-2026-10-04.md) |
| 47 | [ADL candidate ranking total-order determinism](chain-5c756ff-pass47-adl-ranking-total-order-2026-10-04.md) |
| 48 | [ADL bankruptcy price rounding](chain-5c756ff-pass48-adl-bankruptcy-price-rounding-2026-10-04.md) |
| 49 | [Previous-mark reset across oracle outages](chain-5c756ff-pass49-adl-previous-mark-outage-2026-10-04.md) |
| 50 | [Oracle aggregate freshness boundary](chain-5c756ff-pass50-oracle-staleness-boundary-2026-10-04.md) |
| 51 | [Oracle quorum after outlier rejection](chain-5c756ff-pass51-oracle-quorum-after-outliers-2026-10-04.md) |
| 52 | [Session expiry units on latest branch](chain-5c756ff-pass52-session-expiry-recheck-latest-2026-10-04.md) |
| 53 | [Duplicate native nonce within a committed block](chain-5c756ff-pass53-native-duplicate-nonce-live-guard-2026-10-04.md) |
| 54 | [Reservation release when a stop triggers](chain-5c756ff-pass54-triggered-stop-margin-release-2026-10-04.md) |
| 55 | [Stop trigger equality semantics](chain-5c756ff-pass55-stop-trigger-equality-2026-10-04.md) |
| 56 | [Triggered-stop cascade ordering and termination](chain-5c756ff-pass56-triggered-stop-cascade-order-2026-10-04.md) |
| 57 | [Slashing of pending undelegations](chain-5c756ff-pass57-slash-pending-unbondings-2026-10-04.md) |

Main reports link their detailed agent reports and supporting source models.
Pass 5 did not complete. Models illustrate bounded source-derived behavior;
they do not replace failing production-code regressions. No production fixes
are included in these audit documents.
