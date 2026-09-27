# Astra read-only exploration — 2026-09-24

Target: `/home/18c/projects/wt/s63-body-fetch`, `main` at `f05b20e`, including existing uncommitted A–C. The primary checkout remains on `perf/async-validate-rework`.

Scope: explore mechanisms outside Claude's prior work. The exclusion material contains 271 prior findings (including the earlier s64 round) and 126 assignment labels, including unfinished Claude assignments. See adjacent `exclusions.txt` and `claude-findings.json`.

Execution: seven distinct Astra workers were created. The runtime rejected creation of worker eight with `agent thread limit reached`; completed workers were reused for assignments eight through ten. Nine assignments returned reports; the bridge assignment stopped with an automated security restriction and was not retried. One completed pass established no novel actionable defect. The remaining eight reports contain 17 findings, including related manifestations of the same underlying issue.

Restrictions honored: no source edits, deletions, git mutations, builds, tests, benchmarks, or services. Only the parent wrote this report and exclusion material outside the repository. All findings below are static inspection results, not runtime-confirmed bugs. Proposed changes have not been applied. Performance gains are unquantified.

## 1. Market lifecycle — two findings

**Market listings reuse ID zero.** Native governance ListMarket conversion supplies `market_id: 0`, while governance execution writes metadata under that ID without allocation or collision rejection. A later listing can replace a market's advertised identity while its book and positions retain the same numeric identifier. Fix direction: deterministic persisted ID allocation and immutable asset identity. High source confidence; the trading consequence is inferred.

- [Native conversion](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:5554)
- [Metadata write](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/governance.rs:1088)

**Fresh books ignore configured tick/lot sizes.** Genesis stores and RPC advertises configured settings, but both execution paths initialize missing books with `FixedPoint::ONE` tick and lot. Book persistence retains those values. Fix direction: construct books from canonical market metadata and explicitly handle existing mismatched books. High source confidence.

- [Batch path](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:3700)
- [Single path](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:4812)
- [Advertised settings](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/torus.rs:913)

## 2. Asset units — one finding

**Wallet and lockbox disagree by a factor of 10^10.** Wallet transfers parse TRS at 18 decimals; lockbox conversion directly reinterprets that integer as an eight-decimal native FixedPoint value. One wallet TRS becomes 10^10 native units, and the reverse raw identity conversion has the reciprocal denomination mismatch. This does not demonstrate a round-trip mint: raw integers are conserved. Existing tests use the same conversion to fund fixtures, hiding the unit contract. Fix direction: specify protocol denominations, separate raw encoding from conversion, handle dust, and make a compatibility decision for existing balances. High confidence in numeric mismatch; economic interpretation requires confirming intended collateral denomination.

- [Wallet parsing](/home/18c/projects/wt/s63-body-fetch/tools/wallet/src/commands/transfer.rs:63)
- [Raw conversion](/home/18c/projects/wt/s63-body-fetch/crates/torus-core/src/lockbox.rs:238)
- [Fixture setup](/home/18c/projects/wt/s63-body-fetch/crates/torus-integration-tests/tests/lockbox_e2e.rs:42)

## 3. Bridge lifecycle — incomplete

The explorer reported that an external-chain bridge was absent and began reviewing native/EVM boundary behavior. Its final report was blocked by an automated security restriction. Preliminary comments are not promoted into findings here, and this route was not retried.

## 4. Governance — two findings

**Proposal voter snapshots are incomplete.** Missing snapshot rows fall back to current stake, admitting voters who were unstaked at proposal creation; Abstain always uses current stake. Fix direction: proposal-level snapshot-version/completeness marker, zero weight for absent voters on new snapshots, and uniform snapshot lookup for all ballot types. High source confidence.

- [Snapshot fallback](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/governance.rs:1293)
- [Abstain weight](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/governance.rs:873)

**Quorum denominator changes after votes are cast.** Finalization compares recorded votes with current active stake. Undelegation lowers that denominator immediately, while recorded voting weight remains. Additional delegation can have the opposite effect. Fix direction: snapshot quorum supply and voting parameters alongside voting weights. High source confidence; no executed governance reproduction.

- [Finalization](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/governance.rs:922)
- [Undelegation](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/staking.rs:160)

## 5. Ethereum RPC state semantics — three findings

**Historical balance/code/storage requests silently use current state.** These handlers resolve a selector into an unused variable, then access the current database, including for earlier or nonexistent future heights. Fix direction: implement the selected state or reject unsupported selectors, as eth_call already does. High source confidence.

- [Account handlers](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:540)
- [Storage handler](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:580)

**eth_call/estimateGas do not pin state to the accepted height.** RPC height follows committed headers, which can lead execution. Calls use the live database; repeated estimation probes do not share a pinned snapshot. Fix direction: pair an executed frontier and header with a consistent state snapshot for the entire request. Missing binding is source-established; timing-dependent mixed results are untested.

- [Header publication](/home/18c/projects/wt/s63-body-fetch/crates/torus-node/src/main.rs:874)
- [Call setup](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:819)
- [Execution before snapshot binding](/home/18c/projects/wt/s63-body-fetch/crates/torus-evm/src/executor.rs:178)

**Gas estimation ignores supplied gas ceiling.** Search starts at the global maximum and overwrites parsed request gas. Estimates may exceed the supplied ceiling; maximum-gas affordability checks may reject otherwise affordable transactions before searching. Fix direction: bound by request gas, applicable block limit, and affordability. High confidence in ignored ceiling; affordability outcome not reproduced.

- [Estimator bounds](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:884)

## 6. Standalone explorer — three findings

**Partial ingestion can become permanently complete.** Block rows precede receipts and dependent rows; same-hash retries skip the block; backfill continues after errors and later heights advance the cursor. Subscription startup/reconnect also lacks reconciliation of missing ranges. Fix direction: atomic block ingestion with a contiguous completion cursor, completion-aware idempotency, and gap repair. High source confidence.

- [Ingestion](/home/18c/projects/wt/s63-body-fetch/crates/torus-explorer/src/indexer.rs:41)
- [Same-hash skip](/home/18c/projects/wt/s63-body-fetch/crates/torus-explorer/src/indexer.rs:64)

**Native action parser expects the wrong JSON contract.** Node returns signed envelopes; explorer interprets the first envelope key as the action variant. Variant-specific fields are lost; block native-action count is independently hardcoded to zero. Fix direction: typed signed-envelope parsing and fixtures from real node serialization. High source confidence.

- [Node output](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/torus.rs:1360)
- [Explorer parser](/home/18c/projects/wt/s63-body-fetch/crates/torus-explorer/src/indexer.rs:288)

**Pagination shifts with new records and offset multiplication can overflow.** Descending numeric OFFSET pagination repeats rows when new blocks arrive; u32 page arithmetic can wrap or panic for large page values. Fix direction: keyset cursors with a fixed snapshot boundary and checked wide arithmetic. High source confidence.

- [Pagination arithmetic](/home/18c/projects/wt/s63-body-fetch/crates/torus-explorer/src/db.rs:540)

## 7. Genesis input contract — two findings

**Canonical allocation-key collisions can make identical JSON initialize different state.** Raw string keys are stored in HashMaps, then normalized while writing state. For example, storage keys `0` and `0x0` refer to the same slot; randomized iteration determines the final value. Fix direction: normalize and reject canonical duplicates before any writes. High confidence in the source mechanism; no multi-process reproduction performed.

- [Allocation iteration](/home/18c/projects/wt/s63-body-fetch/crates/torus-genesis/src/lib.rs:348)
- [Storage iteration](/home/18c/projects/wt/s63-body-fetch/crates/torus-genesis/src/lib.rs:373)

**Duplicate validator identities produce different staking and consensus sets.** Staking initialization keys by address, while HotStuff membership keys by public key. Duplicate addresses or duplicate keys overwrite different entries without a joint uniqueness check. Fix direction: validate unique canonical addresses and decoded keys, then derive both sets from one validated collection. High confidence in initial inconsistency; downstream failure scenarios untested.

- [Staking initialization](/home/18c/projects/wt/s63-body-fetch/crates/torus-genesis/src/lib.rs:384)
- [Consensus membership](/home/18c/projects/wt/s63-body-fetch/crates/torus-genesis/src/lib.rs:560)

## 8. Staking lifecycle — two findings

**Unbonded principal has no production completion path.** Undelegate creates delayed entries, but process_unbonding has only test callers. ClaimRewards does not release principal, and no native completion action exists. Fix direction: explicit completion action or bounded deterministic maturity processing through execution. High source confidence; repository-wide static caller search, not runtime proof.

- [Native undelegation](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:5250)
- [Unbond release implementation](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/staking.rs:189)

**Staking inflation allocation changes discontinuously with a minimal external delegation.** Emission is calculated from self-stake plus delegated stake, but all post-commission emission goes exclusively to external delegation records once any exist. A sole tiny delegator therefore receives nearly all post-commission emission associated with a large self-stake. Fix direction: define the intended self-stake reward share and pro-rata allocation, conserving rounding residue. High source confidence, but current tests explicitly encode commission-only validator rewards; policy must be confirmed before calling it unintended.

- [Emission calculation](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/rewards.rs:183)
- [Distribution denominator](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/rewards.rs:215)
- [Existing policy expectation](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/rewards.rs:731)

## 9. EVM block environment — no new defect established

Committed number, timestamp, beneficiary, gas limit and base fee are mapped consistently to revm and RPC. BLOCKHASH uses the canonical stored header hash. Frozen basefee is an explicitly documented policy; PREVRANDAO defaults to zero and RPC consistently advertises zero mixHash. These were not inflated into novel findings. This pass is not opcode or replay-equivalence testing.

- [Environment setup](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/validator.rs:143)
- [Documented fee policy](/home/18c/projects/wt/s63-body-fetch/docs/plans/evm-blocker-set-decisions.md:53)

## 10. EVM receipts/logs — two findings

**eth_getLogs silently ignores blockHash.** LogFilter lacks the field and does not reject unknown fields. A hash-only request defaults to the latest block rather than selecting the requested hash. Fix direction: typed hash selector, mutual exclusion with ranges, and hash resolution. High source confidence.

- [Filter type](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/types.rs:237)
- [Default range](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:926)

**Log ranges can report false completeness during execution lag.** RPC publishes committed headers before receipts exist; receipt retrieval silently skips missing rows, so getLogs succeeds with partial or empty data that later changes. A cursor-based client can miss events permanently. This is an additional consequence of assignment 5's commit/execution-frontier issue, not a wholly separate root cause. Fix direction: receipt-complete execution frontier and reject/defer ranges beyond it. High source confidence; no timing reproduction. The completeness rule must account for intentionally skipped transactions.

- [Receipt retrieval](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:134)
- [Log response](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:957)

## Proposed follow-up, not executed

First verify the economic/identity findings with focused reproductions: unit conversion, market ID aliasing, unbond completion and genesis consistency. Confirm staking reward policy separately. Then validate governance snapshots and RPC/explorer consistency. Existing A–C remain uncompiled/untested from the prior session; this exploration changed neither their status nor the benchmark plan. No gain percentages are claimed for this correctness-focused round.
