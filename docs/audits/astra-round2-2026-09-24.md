# Astra exploration round 2 — completed 2026-09-24

**30 read-only assignments completed; 26 candidate findings across 20 passes; 10 passes established no new defect.**

This was performed by three Astra workers in waves: two new threads and one existing worker. The runtime rejected creation of another agent thread, so this is **not 30 distinct new subagents**. All workers finished their assigned passes.

Target: `/home/18c/projects/wt/s63-body-fetch`, `main` at `f05b20e`, with the pre-existing untested A–C changes preserved. Claude's 271 recorded findings and unfinished assignment labels, plus our first-round report, were used as exclusions. The previously blocked bridge/native-EVM rollback route was not retried.

No code edits, deletions, git mutations, builds, tests, benchmarks, node restarts, or services were performed. No private wallet data was read. Only parent-owned report/exclusion files outside the repository were written. The final worktree status showed the same pre-existing changed-file list; this is not a byte-for-byte snapshot comparison.

All findings are **static candidates, not runtime-confirmed bugs or verified fixes**. Performance improvements were not measured; no matched/s percentage is claimed. Some findings depend on uncommon inputs, storage failures, or policy interpretation and should not be treated as equal in severity.

## Highest-value candidates to verify first

1. Honor wallet `--dry-run` before any EVM transfer submission (assignment22).
2. Preserve Ethereum transaction type into revm and reconcile actual charges with receipts (2); preserve original deployed code bytes (12).
3. Enforce ordinary reduce-only orders against evolving maker/taker positions (5), and reject Market+PostOnly (8).
4. Keep EVM transactions recoverable through nonce gaps and abandoned proposals (1); check intrinsic gas before inclusion (3).
5. Run epoch processing regardless of boundary-block activity (17), and advance the production epoch consistently (19).
6. Make completed jail-vote processing idempotent (15), and propagate storage failures before advancing execution (29).

These are proposed verification priorities only. No fixes or reproductions have been attempted. Wallet/API input validation and smaller response corrections can follow independently; economic accounting/policy changes require precise expected behavior first.

## Assignment coverage and results

| # | Angle | Candidates | Result and primary source |
|---|---|---:|---|
| 1 | EVM pool lifecycle | 2 | Nonce-gap transactions are drained before executable; abandoned proposals lack live reinsertion and pending nonce ignores in-flight transactions. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-mempool/src/evm_pool.rs:288) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-mempool/src/lib.rs:368) |
| 2 | Ethereum transaction types | 1 | TxEnv conversion drops transaction type. Type-2 execution can use the fee cap while its receipt advertises the lower effective price. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/decode.rs:96) |
| 3 | Intrinsic gas admission | 1 | Pool/vote-time checks permit transactions below intrinsic gas; committed execution later skips them without execution, charge or receipt. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-mempool/src/validate.rs:73) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-consensus/src/app.rs:4466) |
| 4 | FOK/IOC | 0 | No new defect: precheck and matching price/liquidity conditions agree; reserve-all-before-match is documented policy.  |
| 5 | ReduceOnly | 1 | Normal orders store/sign/display reduce_only but matching and settlement do not enforce it. Closing orders can open or reverse exposure. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-core/src/order_book.rs:1100) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:3627) |
| 6 | Self-trade prevention | 0 | No new defect: cancel-resting-maker policy and reservation release already implemented and covered by existing source tests.  |
| 7 | Position arithmetic | 1 | Rounded weighted entry price loses cost-basis remainder, making PnL depend on fill fragmentation. Example difference is four raw native units; no material scale claimed. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-core/src/position.rs:413) |
| 8 | Market order flags | 1 | Market+PostOnly is accepted. PostOnly tests submitted price but Market matching ignores it, allowing taker fills. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-core/src/order_book.rs:498) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-core/src/order_book.rs:975) |
| 9 | Scalar/batch parity | 0 | No new mismatch: flattening, order IDs, market ordering, client IDs and result-index mapping agree.  |
| 10 | Read-only query ABI | 1 | getMarkets reads activity from metadata byte zero, which is actually the low byte of the Borsh base-name length. Impact demonstrated only for unusual accepted names. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-core/src/precompiles.rs:558) |
| 11 | Fee history | 2 | gasUsedRatio remains zero from placeholder committed headers after execution; missing/future headers become fabricated zero-valued history. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:1011) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:1027) |
| 12 | EVM contract persistence | 1 | Code persistence saves revm analyzed/padded bytes rather than original deployment bytes; reload can change code size/getCode and hash consistency. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-state/src/incremental.rs:306) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/committer.rs:242) |
| 13 | Economics query projection | 1 | Proposal queries omit stored executable payload, execution deadline and snapshot height. Dedicated endpoints cannot show authoritative proposed action/timing. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/torus.rs:1851) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/types.rs:440) |
| 14 | Commission lifecycle | 0 | No new execution issue: immediate bounded/cooldown-protected updates and reward/read paths agree. Documentation drift was not promoted.  |
| 15 | JailVote lifecycle | 1 | Completed votes remain stored and jailed targets remain eligible, so a fresh vote can retrigger slashing and extend jail using the old quorum. Repeat-punishment policy needs explicit confirmation. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/staking.rs:524) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/staking.rs:551) |
| 16 | Unjail/top-up | 1 | TopUpSelfStake accepts and debits a permanently tombstoned validator without restoring eligibility. High source confidence, medium confidence acceptance is unintended. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-economics/src/staking.rs:683) |
| 17 | Permanent-stake scheduling | 1 | Empty epoch-boundary blocks skip epoch processing, and later nonboundary blocks do not catch up scheduled rewards. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-consensus/src/app.rs:1624) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:5838) |
| 18 | Developer reward pool | 0 | No novel live defect: attribution/distribution/reset machinery has test-only callers. Production credits the configured pool address.  |
| 19 | Epoch progression | 1 | Production headers copy the parent epoch forever; execution uses that stale value for labels and fee schedules while height-derived epoch advances elsewhere. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-consensus/src/app.rs:4684) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-bridge/src/native_executor.rs:5817) |
| 20 | Depth/top-N reads | 0 | No new mismatch: supported layouts agree on level order, remaining quantities and readable metadata; the RPC has no limit parameter.  |
| 21 | Numeric client input | 2 | Malformed signs are accepted by the decimal parser; governance u64 inputs silently wrap through unchecked u32 casts before signing. [source1](/home/18c/projects/wt/s63-body-fetch/tools/wallet/src/parse.rs:57) [source2](/home/18c/projects/wt/s63-body-fetch/tools/wallet/src/commands/governance.rs:83) |
| 22 | Wallet submission | 2 | EVM Send ignores accepted global --dry-run and broadcasts; valid result:null becomes a client error, bypassing not-found handling. [source1](/home/18c/projects/wt/s63-body-fetch/tools/wallet/src/commands/transfer.rs:35) [source2](/home/18c/projects/wt/s63-body-fetch/tools/wallet/src/rpc.rs:49) |
| 23 | Transaction RPC projection | 1 | Mined type-2 gasPrice reports maxFeePerGas instead of effective price. Independent of the execution type defect in assignment2. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:325) |
| 24 | Log filtering | 2 | Empty/null-containing topic OR arrays fail wildcard semantics; unsupported address JSON types silently remove filtering while malformed strings silently return no matches. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:512) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/eth.rs:481) |
| 25 | Native batch RPC results | 0 | No new defect: partial acceptance is explicit and response order/counts/admitted-pool relationships agree.  |
| 26 | Native wire schema | 0 | No present-day mismatch in shipped normal paths. Shared types/serializers align; permissive unknown fields alone were not reported as defects.  |
| 27 | Native overlay iteration | 0 | No new issue: DB/parent/child precedence, sorted merging, tombstones and point reads agree. No data deletion was executed.  |
| 28 | Book restoration | 0 | No new ordinary-order mismatch: FIFO/sequences, IDs, client IDs and remaining quantities round-trip across supported layouts.  |
| 29 | State-read failures | 2 | Session lookup errors become absence and can drop a valid action; consumed-nonce read errors become absence and can execute it again. Conditional local-failure scenarios, not reproduced divergence. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-consensus/src/app.rs:1650) [source2](/home/18c/projects/wt/s63-body-fetch/crates/torus-consensus/src/app.rs:1737) |
| 30 | JSON-RPC envelope | 1 | Pinned jsonrpsee uses unsigned numeric IDs; negative IDs fail request parsing and can fall through to notification handling without a response. Dependency-level compatibility candidate. [source1](/home/18c/projects/wt/s63-body-fetch/crates/torus-rpc/src/lib.rs:345) |

## Qualifications and proposed corrections

- **Transaction lifecycle:** selection should begin at the executable parent-branch nonce, with explicit queued/in-flight ownership and reinsertion for abandoned proposals. A transaction in a committed body but skipped by execution is not proof of successful execution.
- **Typed fee charging vs RPC projection:** assignments2 and23 are independent. Fixing TxEnv typing does not correct the RPC gasPrice fallback. While typing remains wrong, do not assume account debits equal the receipt's advertised effective price.
- **Code persistence:** use original bytecode bytes rather than analyzed buffers. Existing records require deliberate migration handling; stripping trailing zeros would corrupt legitimate bytecode.
- **ReduceOnly:** a preparation-time check alone is insufficient for multiple reductions and stale resting maker orders. The reduction budget must evolve deterministically as fills occur.
- **PnL rounding:** the worked example is long1 at100, then buy3 at101 and close4 at101. Buying3 as one fill yields1.00000000 PnL; three unit fills yield1.00000004 under repeated truncation. Preserve cost basis/residuals explicitly. This is deterministic accounting dust, not observed validator divergence or quantified material loss.
- **Market metadata query:** ordinary short asset names happen to produce active=true. The demonstrated mismatch needs an empty or256-byte accepted name; no affected deployed market was established.
- **Governance queries:** omitted action data exists on-chain; this is a DTO/projection gap. It does not establish unauthorized governance execution.
- **Jail/top-up policy:** completed vote reuse is distinct from deliberate new punishment rounds. Tombstoned top-ups are demonstrably debited, but acceptance may be intentional; confirm policy before changing it.
- **Epochs:** empty-boundary omission and stale header epoch are separate mechanisms. Neither report establishes repeated inflation payouts or consensus membership divergence. Historical zero-epoch blocks need compatible replay treatment.
- **Numeric inputs:** validate decimal lexical grammar before signing and use checked integer narrowing. The reported wrap changes the submitted proposal; it does not automatically approve that proposal.
- **Query completeness:** zero placeholders and missing rows must not masquerade as completed historical data. Several RPC candidates relate to the previously identified committed-vs-executed frontier and should be addressed coherently.
- **State-read failures:** session `.ok().flatten()` and nonce `.unwrap_or(None)` erase storage errors. Only actual missing records should become absence. These are conditional source scenarios; no DB corruption, I/O failure or state divergence was induced.
- **Negative JSON-RPC IDs:** the local dependency candidate concerns signed-number IDs being misclassified as notifications. The specification permits Number IDs and defines notifications by absence of an ID. [JSON-RPC 2.0 specification](https://www.jsonrpc.org/specification). Source paths: locked `jsonrpsee-types-0.26.0/src/params.rs:343`, `request.rs:120`, and `jsonrpsee-server-0.26.0/src/server.rs:1282`. This needs an HTTP/WebSocket reproduction before selecting a dependency fix.

## Existing behavior deliberately not promoted

FOK/IOC reserve-all-before-match, fixed cancel-resting-maker STP, immediate commission activation, unsupported cleanly rejected variants, documented fee-history reward stubs, dormant DevPool integration, known classic-layout precompile limitations, and unknown-field permissiveness without a concrete conflicting client were not labeled new defects. Existing tests were inspected, not run.

## Related records

- [Chronological round-two notes](astra-round2-progress-2026-09-24.md)
- [First Astra round](astra-round1-2026-09-24.md)
- [Claude route exclusions](/tmp/torus-astra-exploration-20260924/exclusions.txt)
- [Detailed Claude findings](/tmp/torus-astra-exploration-20260924/claude-findings.json)

No previously requested implementation or benchmark work was resumed. The earlier A–C changes remain in their prior untested state.
