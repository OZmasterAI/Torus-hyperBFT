# Torus project-wide chain audit — third pass

Date: 2026-10-04. Branch: `merge/item6-sync2`. Revision: `cea1254e34625e6b09c58f794de8793b5c12713c`.

This pass reviewed the current project across consensus, synchronization, execution, trading, liquidation, persistence, restart, networking, RPC, wallet, and explorer. It was not limited to commits on this branch. Three GPT-6.1-sol agents independently examined consensus/recovery, trading/economics, and ingress/client surfaces; the coordinating review examined EVM dependency behavior, state restoration, restart configuration, historical overlap, and cross-component consequences.

**Result: 15 priority findings (9 P1, 6 P2), plus four supplemental P2 candidates.** These include previously documented gaps that remain present; they are not 19 claims of novel discovery. The most serious source-derived sequence turns an off-mark fill into redeemable profit backed only by a flat negative protocol-vault balance. Other high-priority candidates concern persistent consensus stalls, an uncertified sync response terminating a node, unbounded pre-validation resource use, and EVM execution/persistence correctness.

**Evidence limit:** all findings remain static candidates under this repository's audit policy. Cargo and rustc were unavailable; the requested audit continued without installing the toolchain. Seven saved Python counterexample models pass, but do not execute Torus, revm, RocksDB, real signatures, or a validator network. No live-chain transactions, runtime exploit, throughput measurement, or security certification is claimed. No production source was changed, and no fixes, commits, pushes, branch changes, or pulls were performed.

## Reports and numbering

This is the consolidated entry point for pass 3. Detailed schedules, guard traces, source locations, and proposed regressions are in:

- [Consensus and recovery](chain-cea1254-pass3-consensus-2026-10-04.md): C01–C02.
- [Networking, RPC, wallet, and explorer](chain-cea1254-pass3-surface-2026-10-04.md): N1–N2 and R1–R7.
- [Trading, liquidation, and CoreWriter](chain-cea1254-pass3-trading-2026-10-04.md): T01–T05.
- [EVM, snapshot, and restart models](chain-cea1254-pass3-models.py), and [trading models](chain-cea1254-pass3-trading-models.py).

The [first pass](chain-cea1254-2026-10-04.md) and [second pass](chain-cea1254-pass2-2026-10-04.md) remain separate and unchanged. F01–F11 are not counted again here. F12–F26 below map the new consolidated list to the agents' local identifiers. P1 means high remediation priority; P2 means material correctness or availability impact with narrower consequences/preconditions. Priority is not runtime confirmation.

| ID | Priority | Finding | Detailed source / provenance |
| --- | --- | --- | --- |
| F12 | P1 | Off-mark fill can fund a profitable withdrawal against flat vault debt | Trading T01; known D5 margin root, extended solvency sequence |
| F13 | P1 | False validator-update hint can poison highest PC and strand consensus | Consensus C01; additional to F01–F11 |
| F14 | P1 | Uncertified sync conflict triggers fatal node shutdown | Consensus C02; additional to F01–F11 |
| F15 | P1 | Direct transaction ingress has unbounded pre-validation queues | Surface N1 |
| F16 | P1 | Rejected native transactions are persisted before authentication, without normal cleanup | Surface N2 |
| F17 | P1 | Typed EVM envelopes execute with legacy transaction type | Coordinating review; revalidated Astra round 2 assignment 2; surface R6 grouped here |
| F18 | P1 | Persisted analyzed bytecode changes deployed code on reload | Coordinating review; revalidated Astra round 2 assignment 12 |
| F19 | P1 | Restart without genesis substitutes consensus-relevant configuration without rejecting a mismatch | Coordinating review |
| F20 | P1 | Wallet `--dry-run send` still broadcasts | Surface R1; historical finding revalidated |
| F21 | P2 | Snapshot verification does not cover all restored authoritative state | Coordinating review |
| F22 | P2 | CoreWriter returns an order ID different from the created order | Trading T02 |
| F23 | P2 | Accepted stop-order codes execute as ordinary limit orders | Trading T03 |
| F24 | P2 | CoreWriter reaches unlisted-market execution | Trading T04; known ECON-PF-05 / deferred O2 |
| F25 | P2 | New order books ignore the market's listed tick/lot sizes | Trading T05; known ECON-PF-05 / Astra round 1 |
| F26 | P2 | Explorer treats partially indexed blocks as complete | Surface R4; historical partial-write defect revalidated |

## F12 — Off-mark fill, ADL, and withdrawal form a solvency gap

The detailed T01 trace follows actual placement, maker/taker match checks, liquidation selection, ADL pricing, signed collateral transfer, and lockbox withdrawal. It does not require validator control or a forged oracle. Preconditions are a listed 20x market, usable current/previous mark 100, no other opposite positions, and two distinct admitted accounts with 60 collateral each.

The maker rests a sell of size 1 at 1,000, reserving 50. A capped market buy matches it because the taker check charges opening initial margin 50 against a budget of 60, without including the immediate 900 mark loss. At mark 100 the long is bankrupt at −840 equity and the short has 960 equity. The implemented ADL closes both at 100; the long's residual −840 collateral moves into the protocol vault. That vault is flat, so its liquidation view has no position to resolve. The counterparty is flat with 960 available and can pass the next block's withdrawal gate, receiving 960 native-equivalent EVM units from an initial combined 120 collateral.

Signed conservation still holds: `0 + 960 − 840 = 120`. That does not establish redemption solvency when the negative vault row has no backing. The Python model demonstrates the arithmetic and guard inequalities, not a production exploit. Existing signed-conservation tests are insufficient to exclude this sequence.

**Fix/regression:** enforce maker and taker post-fill equity, including fill-versus-mark PnL and remaining position/order margin; ensure loss handling is backed before unrestricted positive claims become redeemable. Reproduce the full two-account, real matching → liquidation → next-block withdrawal path in Rust, including serial/parallel execution and actual oracle fixtures. See T01 for precise source anchors and guard exclusions.

## F13 — A wrong-phase certificate can persistently stop automatic progress

A scheduled Byzantine proposer can mark an ordinary valid block as having validator updates. The hint is not part of the block hash, and header voting chooses Prepare from the hint instead of the held body's actual update result. A vote collector with a temporarily pending body admits the resulting certificate through a lock-only shortcut. Pacemaker admission can propagate and persist that certificate using signature validity without phase/body compatibility.

C01 supplies a finite four-validator schedule: the attacker plus two honest body holders sign Prepare; an honest collector receives their votes before its delayed body. Once the body arrives and all subsequent delivery is timely, the wrong highest PC remains. Nudges fail the proper phase/update invariant, while stale-Prepare reproposal again advertises the wrong hint before timeout-certificate recovery. Restart preserves this frontier. This is a liveness finding, not a demonstrated conflicting commit or irreparable database loss; explicit operator tree surgery is a separate recovery mechanism.

**Fix/regression:** derive vote phase from deterministic application results, keep unknown-body certificates provisional, and apply compatibility checks consistently to collector and pacemaker admission. Run C01's bounded-delay schedule and require several subsequent honest commits without manual intervention.

## F14 — Uncertified sync content is treated as a finalized fork

A selected Byzantine eligible sync server can return a block at an already committed local height with a different hash and a valid parent certificate. `Block::is_correct` proves that parent certificate and the supplied block hash, not certification/finality of the supplied child. The client invokes the fatal safety callback on the conflicting height before application validation or validation of the response's terminal certificate. The node watcher exits with code 70. Response heights are not first restricted to the requested range.

This requires a malicious eligible server selected for sync, not an anonymous peer or an actual conflicting quorum. An existing unit-test fixture deliberately expects fatal handling of a genesis-justified conflicting block; the test was inspected, not executed.

**Fix/regression:** reject/penalize uncertified or out-of-range peer content; reserve global fail-stop behavior for independently validated contradictory commit proofs. Test a retained committed height, an out-of-range conflicting child, and an invalid terminal certificate. See C02 for the complete admission order.

## F15 and F16 — Pre-validation memory and disk exposure

**F15 / N1:** native and EVM transaction channels are unbounded. Identified non-validator peers can submit direct transaction batches into them before signature, funding, or transaction-rate validation. Existing frame/stream caps bound individual work, not total queued bytes. Native worker saturation falls back to inline processing; it slows the consumer without backpressuring the unbounded producer. Sustained input faster than verification can grow memory toward exhaustion. Actual throughput was not measured.

**F16 / N2:** native admission calls the DA mirror before signature recovery, sender/nonce checks, and funding validation. Unique invalid bodies can become durable `CF_NATIVE_PENDING` entries. Normal committed-pool removal does not remove these DA entries, and the reviewed pruning path does not supply a retention bound. Varying signed-envelope bytes gives distinct action hashes, so repeated rejected input can grow disk state independently of whether it ever becomes an executable transaction.

**Fix/regression:** use byte-bounded queues, per-peer budgets, and deliberate consensus-priority/backpressure policies. Separately enforce bounded authenticated/quarantined DA admission and retention. Preserve availability for manifest-referenced bodies, including invalid bodies needed for deterministic validation; merely dropping every invalid body is not a complete design. Run isolated adversarial ingress tests that measure bounded resident memory and disk usage after rejection/pruning.

## F17 — Typed EVM transactions retain the legacy TxEnv type

[Envelope conversion](../../crates/torus-bridge/src/decode.rs#L66) builds Legacy, EIP-2930, and EIP-1559 `TxEnv` struct literals using `..Default::default()`. It never sets `tx_type` or derives it after assigning envelope-specific fields. For EIP-1559, `gas_price` becomes the maximum fee and `gas_priority_fee` becomes Some, but the already constructed default transaction type remains legacy.

This was checked against exact downloaded dependency sources whose archive SHA-256 values match `Cargo.lock`: `revm-context 15.0.0`, `revm-context-interface 16.0.0`, and `revm-handler 17.0.0`. In those sources, `TxEnv::default` builds type zero from default fields; later `set_tx` assigns the environment without deriving its type. `Transaction::effective_gas_price` returns `gas_price` directly for legacy/type-1 transactions. Revm's fee accounting uses that trait. The pre-execution access-list path also treats this environment as legacy.

Torus then [executes the environment](../../crates/torus-evm/src/executor.rs#L300) but [computes receipt effective price](../../crates/torus-evm/src/executor.rs#L361) separately as the intended type-2 price. With cap 100, base 10, tip 2, and 21,000 gas, the source-derived debit is 2,100,000 fee units while the receipt advertises 252,000. This is independent of the earlier F02 duplicate-tip distribution issue, though their consequences can interact.

Surface R6 is grouped here: [mined transaction RPC projection](../../crates/torus-rpc/src/eth.rs#L370) falls back to the signed maximum fee rather than included effective price. Under this checkout's execution-type bug the cap agrees with the modeled debit but disagrees with type-2 semantics and receipt projection. It must not be described as a second independent overcharge, and fixing execution alone leaves the projection wrong.

**Fix/regression:** explicitly map or derive transaction type for every supported envelope, preserve typed access-list semantics, and align fee execution, receipts, and mined RPC projections. Execute signed legacy/type-1/type-2 transactions against the pinned revm version; check sender balance deltas, gas, receipts, access-list warming, and transaction-by-hash/full-block output. Include both binding and nonbinding fee caps.

## F18 — Bytecode persistence includes analysis padding

[Incremental persistence](../../crates/torus-state/src/incremental.rs#L306) and both [committer paths](../../crates/torus-bridge/src/committer.rs#L84) store `bytecode.bytes()`. [Database reload](../../crates/torus-state/src/db.rs#L985) treats those bytes as original input via `Bytecode::new_raw`.

The exact `revm-bytecode 9.0.0` source archive was checked against `Cargo.lock`. Its `bytes()` returns analyzed backing bytes; `original_bytes()` respects original length. Analysis adds missing PUSH data and/or a final STOP when required. This version does not append a fixed amount to every program, so older generic padding assumptions were not used.

Concrete example: runtime `3860005260206000f3` is nine bytes and returns its own CODESIZE. Analysis appends a STOP, making backing storage ten bytes. Persisting backing bytes and reloading with `new_raw` resets original length to ten. A later call can therefore observe 10 rather than the deployed 9; retrieved code can disagree with the recorded original code hash. No node execution of this example was performed.

**Fix/regression:** persist original bytecode in every commit path and verify stored-code/hash consistency. Deploy the example and check exact code, code hash, and CODESIZE in the deployment block, a subsequent block, and after restart. Existing corrupted storage needs an explicit recovery strategy: blindly stripping trailing zero bytes would corrupt legitimate original code.

## F19 — Restart can change chain configuration without rejecting a mismatch

The node describes [genesis as required only on first run](../../crates/torus-node/src/main.rs#L98). During [startup](../../crates/torus-node/src/main.rs#L529), a supplied genesis file supplies its chain configuration even for an existing database; without it, `default_chain_config` is used. The reviewed path does not reload a persisted canonical configuration or reject a configuration mismatch.

[Defaults](../../crates/torus-node/src/main.rs#L341) include epoch length 100,000, zero treasury/developer addresses, and no state-hash activation. [Execution context](../../crates/torus-consensus/src/app.rs#L3785) receives this configuration, and [fee distribution](../../crates/torus-bridge/src/native_executor.rs#L8260) uses its recipients. A chain initially using nonzero recipients can restart without the file and redirect the epoch-zero 45/45 fee credits to zero. A nondefault epoch schedule can change too, producing different state from peers for the same inputs. Separately, [running-hash activation configuration](../../crates/torus-state/src/running_hash.rs#L254) can delete prior hash/checkpoint state when activation changes instead of rejecting the mismatch.

This is an operator/configuration-triggered consensus correctness issue on a nondefault chain, not a remotely triggered configuration edit. Omission here means no genesis supplied through either CLI or config file; config defaults can populate the CLI field. Re-supplying the identical genesis avoids the omission case. Startup logs the default fallback, and activation changes warn; the concern is acceptance without a configuration-identity mismatch rejection, not an absence of diagnostics.

**Fix/regression:** persist canonical chain configuration and genesis identity, load it on restart, and reject inconsistent supplied configuration. Defaults should initialize a genuinely new chain only. Preserve explicitly supported upgrade transitions, including planned state-hash activation rollout, rather than indiscriminately rejecting every changed field. Boot a nondefault chain, execute, restart with omitted and altered genesis, and require identical configuration/state or an explicit mismatch failure before execution.

## F20 — Wallet dry run sends funds

The wallet exposes a global dry-run option, but the Send dispatch reaches `cmd_send`, whose transfer path unconditionally calls `send_raw_transaction` / `eth_sendRawTransaction`. The source contains no dry-run branch preventing that submission. This is a historical finding still present, with user-funds impact when an operator reasonably expects simulation. No transaction was sent during this audit.

**Fix/regression:** ensure the send path honors dry run before network submission; test with a mock RPC that dry run makes zero broadcast calls and normal send makes one. See R1 for exact CLI and RPC locations.

## F21 — Snapshot verification leaves restored security state uncommitted

[Snapshot verification](../../crates/torus-state/src/snapshot.rs#L111) recomputes EVM and full native roots and compares them to the metadata state root; [restore](../../crates/torus-state/src/snapshot.rs#L165) then restores all column families. However, [the native root's CF list](../../crates/torus-state/src/native_trie.rs#L47) includes only seven families: balances, order books, positions, oracle, staking delegations, staking validators, and liquidation. Sessions, consumed native nonces, CoreWriter queue, governance, and market registry are among the authoritative state omitted from that computation.

The model deletes a consumed nonce while holding every verified native-root input and all EVM state constant. This changes replay protection after restore without changing the verifier's computed root inputs. Altering session authorization is another excluded-state class. The [EVM root](../../crates/torus-state/src/trie.rs#L50) also relies on account code hashes rather than rehashing each stored bytecode value. A separate [running hash](../../crates/torus-state/src/running_hash.rs#L35) includes more write categories but is not recomputed by this snapshot verifier, so it does not close this validation gap.

Precondition is restoration of an operator-selected corrupted or modified snapshot. This report does not claim remote snapshot selection, authenticated trustless bootstrap, or that RocksDB checksums fail to detect ordinary file corruption. The issue is the logical state coverage advertised by successful verification, even relative to unchanged metadata.

**Fix/regression:** bind all restored authoritative state to a complete trusted commitment/manifest, verify stored code against account hashes, and specify snapshot frontier/chain identity binding. Mutate one nonce/session/queue/market entry at a time while preserving metadata and require rejection. A root-only check cannot prove excluded state integrity.

## F22–F25 — CoreWriter and market initialization gaps

**F22 / T02:** CoreWriter returns `((block+1)<<64)|queue_seq` as an order ID, but queue draining creates an ordinary PlaceOrder whose execution allocates `next_global_order_id`. With block 20, sequence zero, and an empty allocator, the returned value is 387381625547900583936 while the actual order ID is 1. Cancelling the returned ID misses the resting order. Preserve the reserved ID end-to-end or expose an explicitly resolvable action handle; test real precompile → drain → placement → cancellation.

**F23 / T03:** the ABI accepts order-type codes 2/3 as StopMarket/StopLimit but supplies no trigger, while execution decoding maps every accepted code except Market(1) to Limit. Such orders become active immediately rather than waiting for a stop condition. Reject unsupported codes or implement a complete trigger representation and lifecycle; test all accepted ABI types through production drain.

**F24 / T04:** native RPC's listed-market gate does not protect CoreWriter, which accepts arbitrary market IDs. Execution can create a missing book without first checking the market registry. This allows state outside the registry/oracle/liquidation universe and can collide with later market allocation. Enforce registry membership at deterministic execution before any writes across all submission paths. This is a known deferred execution-side issue, not a new RPC bypass discovery.

**F25 / T05:** first-book creation in both scalar and batch execution uses ONE tick/lot instead of the registered values. A listed tick 0.01/lot 0.001 market can reject valid size 0.1 and price 100.25 against a one-unit book. Reload preserves the book's wrong metadata. Initialize from the registry, account for already-created incorrect books, and test first orders plus persistence/reload on scalar and batch paths.

## F26 — Explorer cannot repair partial ingestion

The indexer inserts a block row before fallible receipt/body/transaction/log work, then treats an existing matching block hash as completed indexing. A receipt or SQLite failure after insertion leaves a partial block that a retry skips. Body RPC failures are also converted to absence and can silently complete the block. Later heights/cursors can advance beyond the missing data.

The new executed-head guard helps ordinary execution lag; it does not address this independent failure ordering. Surface R4 describes an SQLite control-flow model with one durable block row and no transaction rows after retry. **Fix/regression:** persist required rows and completion transactionally, distinguish incomplete optional phases, and permit same-hash repair. Inject one-shot receipt/body/storage failures followed by retry and restart; require complete, duplicate-free transactions, actions, logs, and candles.

## Supplemental candidates

These source-supported P2 items are preserved in the surface report, but not included in the 15 priority findings above. They are historical revalidations, not additions to F01–F11.

| Local ID | Remaining issue | Focused regression |
| --- | --- | --- |
| R2 | Destructive EVM pool selection loses future-nonce transactions and lacks a live abandoned-proposal requeue path | Nonce 1 before nonce 0; lost-view proposal; queued/in-flight nonce visibility |
| R3 | Admission omits intrinsic-gas lower bounds; execution rejects/skips admitted payloads without normal receipts | Transfer/calldata/access-list/create gas just below and at fork-specific boundaries |
| R5 | Explorer parses the signed native envelope's first key as the action variant and hardcodes native action count to zero | Actual node serialization for each action, preserving fields and count |
| R7 | Governance CLI uses unchecked u64-to-u32 casts before signing | Reject values above u32 maximum with zero signing/submission calls |

R6 is already grouped under F17. The wallet's valid-`result:null` handling remains a lower-priority client issue in the surface notes. Concurrent RPC read-snapshot behavior and same-block session activation policy need further specification/reproduction; they were not promoted into proven failures. Fixed-domain admission checks, explicit Full-session authority, recent market-ID collision fixes, reduce-only enforcement, order ownership, and executed-head guards were considered rather than automatically re-reporting old findings.

## Validation, limits, and next work

Saved models can be rerun from the repository root:

```sh
python3 docs/audits/chain-cea1254-pass3-models.py
python3 docs/audits/chain-cea1254-pass3-trading-models.py
```

The first script covers typed-fee arithmetic, bytecode original-length drift, snapshot root-input coverage, and restart recipient drift. The second covers the solvency sequence's arithmetic/guard inequalities, mismatched order IDs, and stop-type decoding. Both pin the audited revision and check relevant source patterns; these guards do not make the models production-code tests. Dependency archives were inspected at the exact lockfile versions, not inferred from current online API documentation.

Coverage was broad, but it is not a proof over every path or dependency. Production Rust regressions, signature-bearing consensus fault schedules, full matching/ADL/withdrawal execution, crash/restart testing with RocksDB, and bounded adversarial load tests remain outstanding. Existing tests were read where relevant; no passing Rust suite is claimed. Recommended remediation order is F12, F13/F14, F15/F16, then EVM persistence/typing and restart correctness, with the destructive wallet dry-run behavior fixed promptly as a small independent change.

The first two passes remain relevant: this review does not close their gas-limit, fee, session-expiry, storage-error, WAL, ADL-scan, epoch, height-binding, governance-snapshot, validator-key, or worker-cap findings. No issue was marked resolved. This report and the three companion reports preserve both new evidence and prior provenance so that subsequent fixes can be validated against the actual failure sequence.
