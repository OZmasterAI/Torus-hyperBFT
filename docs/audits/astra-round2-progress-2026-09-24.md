# Astra exploration round 2 — progress

Read-only source exploration. No code edits, deletions, tests, builds, benchmarks, or services. Parent-owned notes outside the repository only. Two new Astra threads were created; another new thread was rejected by the runtime thread limit. Existing Astra workers are reused for 30 separate assignments, three concurrently. These are not 30 distinct new agents.

All findings are static and unverified by execution. Do not duplicate the first-round report, Claude exclusions, or the new mechanisms listed below.

## Completed assignments

1. EVM mempool: nonce-gap transactions can be selected and drained before their predecessor; proposal abandonment has no live reinsertion path and pending nonce ignores in-flight transactions. Main sources: torus-mempool/src/evm_pool.rs:288, :369; torus-consensus/src/app.rs:4643; torus-mempool/src/lib.rs:368, :1030.
2. Typed Ethereum transactions: bridge TxEnv conversion omits tx_type, defaulting type-1/type-2 to Legacy. Type-2 execution can use max fee while receipt advertises the lower effective fee. Main source: torus-bridge/src/decode.rs:96. Raw typed decoding concern was discarded after checking locked Alloy behavior.
3. Intrinsic gas: pool/vote-time validation lacks intrinsic-gas checks, allowing inherently unexecutable low-gas legacy transactions to commit and then be skipped without receipts/charging. Sources: torus-mempool/src/validate.rs:73; torus-consensus/src/app.rs:4466; torus-evm/src/executor.rs:293. Not a demonstrated consensus divergence.

## Current assignments

4. Ordinary FOK/IOC behavior and settlement.
5. Ordinary ReduceOnly behavior.
6. Ordinary self-trade-prevention modes and margin release.

27 additional assignments remain after the first three completed passes. Later entries will be appended as workers report.

## Wave 2 results

4. FOK/IOC: no novel defect established. Price/liquidity checks and margin release align; reserve-all-before-match behavior is documented policy, not promoted as a bug.
5. ReduceOnly: ordinary placement stores/signs/displays the flag but preparation, matching and settlement never enforce it. Multiple reducing orders or stale resting makers can open/flip exposure. One shared missing-enforcement mechanism. Sources: torus-core/src/order_book.rs:1100; torus-core/src/position.rs:394, :439; torus-bridge/src/native_executor.rs:3627. High source confidence, untested.
6. Self-trade prevention: no new finding; cancel-resting-maker and reservation release already implemented and documented as the S470 A5 fix.

Assignments 7–9 dispatched: ordinary fill position arithmetic, Market-order reserve price, scalar-vs-batch result/metadata parity.

## Wave 3 results

7. Position accounting: rounded weighted entry price loses cost-basis residuals and makes PnL depend on fill fragmentation. Source position.rs:413; example long 1@100 plus 3@101 then close4@101 gives PnL1.00000000 for one fill versus1.00000004 when the added3 are three fills. Deterministic dust/accounting drift, not validator nondeterminism or a quantified large economic loss.
8. Market+PostOnly is accepted; PostOnly checks supplied price, Market matching ignores it. A zero-price market buy can pass the guard and take asks. Sources order_book.rs:498, :975, :1219; wallet trading.rs:35/:58 permits the combination. Ordinary placement, distinct from known ModifyOrder issue.
9. Scalar-vs-batch: no new mismatch. Flattening, assigned IDs, per-market order and result index restoration agree; documented batch semantics retained.

Assignments10–12 dispatched: read-only precompile ABI queries, fee-discovery RPC, ordinary EVM contract storage lifecycle.

## Query/API results

10. Read-only getMarkets precompile reads active from metadata byte0, but metadata starts with Borsh base-asset string length. Typical names yield true; empty or256-byte names yield false. Source precompiles.rs:558; genesis lib.rs:432. Narrow layout mismatch, no deployed affected market established. Other queried ABI layouts checked consistently.
11. feeHistory gasUsedRatio reads committed CTE header gas_used=0 even after executed receipts exist (eth.rs:1011, app.rs:4678). Also missing/future headers are fabricated as zero-valued history (eth.rs:1027). Reward-percentile stub and frozen-basefee policy were not promoted as new findings.
13. Proposal RPC projection omits stored execution_payload, executable_after and snapshot_block (torus.rs:1851, types.rs:440), so dedicated queries cannot show authoritative proposed action/timing. Data exists in governance.rs:325. Presentation gap, not unauthorized execution.

Assignments12,14,15 ongoing: ordinary EVM state persistence, commission updates, JailVote lifecycle.

## Additional completed assignments

12. Ordinary EVM contract code persistence writes Bytecode.bytes() (analyzed/padded execution buffer) instead of original bytes. On reload padded bytes become original code, affecting getCode/code size and hash consistency. Sources torus-state/incremental.rs:306; committer.rs:242; locked revm-bytecode bytes/original_bytes. No runtime reproduction; do not blindly strip trailing zeros.
14. Commission lifecycle: no novel execution defect. Rate changes are immediate and bounded/cooldown-protected; reward/read paths use current state. Minor documentation drift only, not promoted.
15. JailVote lifecycle: completed quorum remains stored and jailed targets stay eligible. A fresh vote can retrigger slash/jail and extend deadline using old quorum; votes clear only upon unjail. Sources staking.rs:524, :551, :494, :670. Intended repeat-punishment policy requires clarification; no tests run.

15/30 assignments complete.16–18 running: Unjail/TopUp, PermanentStake accounting, DevPool attribution.

## Staking and lifecycle results

16. TopUpSelfStake debits even Tombstoned validators without restoring eligibility or a discovered self-stake withdrawal path. Source staking.rs:683/:831; UnjailSelf rejects Tombstoned at:642. High source confidence, medium confidence unintended policy.
17. Empty epoch-boundary blocks skip mandatory epoch processing because app.rs:1624 gates the whole handler on native actions or EVM fee revenue; process_epoch_boundary at:1892 is inside. Later nonboundary blocks return immediately(native_executor.rs:5838), so no reward catch-up. One scheduling finding with multiple consequences.
18. DevPool: no novel live issue. Gas attribution/distribution/reset have test-only callers; production credits configured pool address. Dormant integration not promoted.

18/30 complete;19–21 running: epoch rollover distinct from empty-boundary gate, top-N market-data queries, numeric API serialization.

## Further completed assignments

19. Header epoch copies parent forever(app.rs:4684), execution ctx uses it(:1840), yet height-based boundaries still fire. Epoch labels repeat1 and fee scheduling sees stale epoch(native_executor.rs:5817/:5865/:5902), while RPC/consensus epoch advances. Not a claim of repeated inflation or membership divergence. Separate from17 skipped empty boundaries.
20. Depth/top-N semantics: no new defect. RPC has no limit parameter; ordering and aggregate fields match across supported persistence layouts. Classic precompile limitation already documented.
21. Numeric wallet boundary: malformed decimal signs accepted (`--1`=>1; `1.-5`=>0.95) because components parsed signed(parse.rs:57/:79/:89). Governance numeric fields use unchecked u64->u32 casts(governance.rs:83/:94), silently wrapping a signed proposal's value. Two lexical/range mechanisms, not lockbox units or PnL rounding.
22. Wallet: Send ignores accepted global --dry-run and unconditionally broadcasts(transfer.rs:35, main.rs:47/:357). JSON RPC result:null becomes missing Option and client error(rpc.rs:7/:49), making explicit not-found handlers unreachable. No private wallet data accessed or commands executed.
23. Mined type2 RPC gasPrice uses maxFeePerGas instead of effective fee(eth.rs:279/:325). Distinct response-projection defect from2 lost execution type; do not assume account currently pays the expected effective price while2 remains.

23/30 complete.24–26 running: exact log filtering, native RPC submit-batch responses, native action schema compatibility.

## Final wave dispatched

24. Log filtering: [] or null-containing topic OR arrays are treated as no-match rather than wildcard per locked Alloy contract(eth.rs:512). Invalid address JSON types default to unrestricted match, malformed strings become silent empty matches(eth.rs:481/:472; types.rs:240). Two validation/matching mechanisms; independent of known blockHash/frontier issues.
25. Native RPC submit batch: no novel defect; partial acceptance explicitly documented, index/count mapping and local-admission-before-success agree.
26. Native wire schema: no present-day mismatch in shipped normal paths; shared serializer/types align. Unknown-field permissiveness alone not promoted.
27. Native overlay iteration: no novel issue; DB/parent/child precedence, tombstones, sorted keys and point reads agree. Tombstone/deletion SOURCE was inspected; no data was deleted.

All30 assignments dispatched,27 complete.28–30 remain: book restore fidelity, native typed-read errors, JSON-RPC protocol response handling.

## Final persistence/error results

28. Book restore: no novel ordinary-order mismatch. FIFO/sequence order, IDs/client IDs, partial quantities and independent global ID counter persistence agree across supported formats; existing tests inspected, not executed.
29. Two conditional storage-error classifications in execution: session lookup .ok().flatten()(app.rs:1650) can treat malformed/I/O-failed existing session as absent and permanently skip its action; nonce marker .unwrap_or(None)(app.rs:1737) can treat failed lookup of consumed action as fresh. Normal local reads succeeding on peers could yield different effects. Source scenarios only; no database read/corruption experiment or reproduced divergence. Propagate errors and avoid advancing applied height on failure.

29/30 complete. Last pending assignment:30 JSON-RPC protocol response behavior.

## Complete

30. Negative numeric JSON-RPC IDs are unsupported by pinned unsigned-ID representation and can fall through to notification handling. Dependency-level compatibility candidate; source traced, not reproduced.

All30 assignments complete.26 candidate findings across20 passes;10 passes established no new defect. Final consolidated report: round2-report.md. No code edits, deletions or tests.
