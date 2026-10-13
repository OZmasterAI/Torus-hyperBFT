# Main chain review: safety, exchange behavior, performance, and venue parity

**Reviewed source:** `main` and `origin/main` at `bf2edda6510bf46826e1cc3aec812e895a7de768` (`bf2edda6`). **Review date:** 2026-10-08. **Report branch:** `docs/main-chain-deep-review-2026-10-08`, created directly from that commit. This is a source review; no implementation files were edited and no builds, tests, fault injections, or benchmarks ran. Existing test sources and historical measurements are evidence of intent or prior observations, not fresh verification.

## Status update (2026-10-13, main `32dfc8b1`)

This section was added after the review; the rest of the document is unchanged and still describes `bf2edda6`. Source line links in the rest of the document point at the reviewed commit, not at current main. Status per finding, with the merge commits on main that address it (each checked with `git merge-base --is-ancestor`):

**Counts:** Fixed 8, Partly fixed 3, Planned 6, Decided not to fix 1, Open 14. Statuses marked † rest on backlog notes rather than an explicit owner decision on the finding.

| ID | Status | Evidence (commits on main) | Note |
|---|---|---|---|
| R01 | Fixed | 19f8f534, d2b2736b, 5d7075be, e58b92b0 | Failed serial/marker writes and thread panics exit 70; fault tests passed |
| R02 | Fixed | 4bef6406, ed2354fb, 1eced05c, f4c57d01, 5a308acb, 602c72c7, 12b2cfec, 969ab0c3, 23ae7fc1, c9ed0ba7 | Storage read faults fail-stop in every branch; a decode error still reads as absent (owner s111) |
| R03 | Fixed | 12979d4b, 9793f1ec, 0c5f10f7 | Typed EVM transactions keep their type; EIP-1559 senders pay base fee + tip |
| R04 | Fixed | 50808e7b | Original bytecode stored at all 5 CF_CODE writers |
| R05 | Planned † | none (counter added s99) | Plan 9.11 backlog; options 4/6/7 to compare |
| R06 | Open | none | Round-robin liquidation caps unchanged |
| R07 | Open | 85857762 (speed only) | ADL holder sets faster; no funded backstop policy |
| R08 | Open | none | Stale-mark policy unchanged |
| R09 | Open | none | Oracle still the mark; stops still trigger on last trade |
| R10 | Planned | none | Funding in backlog; mechanism undecided |
| R11 | Planned † | none | Native trading fees in backlog |
| R12 | Planned | none | Unknown market ids create books (consensus) and Delist/UpdateMarketParams, after Phase 3 |
| R13 | Open | none | Production still one flat tier |
| R14 | Open | none | Equal parent/child timestamps still allowed |
| R15 | Open | none | Commitment and fail-stop defaults unchanged |
| R16 | Open | none | Consensus execution work still unbounded |
| R17 | Decided not to fix † | 50808e7b, 12979d4b | Policy: every launch is a fresh genesis, no activation heights (s104/s111) |
| R18 | Fixed | fa875c3d | Per-proposal snapshot weight and quorum, no live fallback |
| R19 | Fixed | adcc6239, 684961ac | Odd-length hex, alias keys, accounts/evm_alloc overlap, duplicate validators refused |
| R20 | Open | none | Devnet and testnet genesis still both chain_id 7778 |
| R21 | Planned | 0c5f10f7 (related only) | EVM pool still drains before inclusion; fix after Phase 3 |
| R22 | Fixed | 6b9a01f1 | State reads follow the block tag; historical blocks refused |
| R23 | Partly fixed | e8b75162 | getBalances fixed; getPosition cross liquidation price still 0 |
| R24 | Open | none | Full-scan RPC paths unchanged |
| R25 | Open | 7c4856ca, 959fe3bc (related) | Trade history still async; no durability watermark |
| R26 | Open | none | Modify still rejects crossing prices |
| R27 | Partly fixed | 46a11c14, 0c14906f | u128 client order ids, cancel and lookup by cloid; no dead-man switch, expiry or TWAP |
| R28 | Partly fixed | 3efff0d6, 56914dd1, 85857762 | Cancel-all index, worker pool; EVM serial barrier remains, Phase 3 not merged |
| R29 | Open | none | Snapshot restore authenticity unchanged |
| R30 | Fixed | 57170d68 | Explorer indexes a block whole or retries it |
| R31 | Open | 3f54704e (related) | Only the mempool cancel share is oldest-first; in-block sort unchanged |
| R32 | Planned † | none | Margin currency in backlog, undecided |

## Assessment

The chain has substantial working infrastructure: ordered committed execution, parallel market matching, cross-margin accounting, oracle aggregation, staged liquidation and ADL, staking/governance, EVM/native bridging, and separate execution/history persistence. Several serious defects described by older audits have already been addressed. Those older reports are not a reliable inventory of current bugs.

I would prioritize failure containment and account-wide risk before throughput optimization or claims of Hyperliquid parity. The most important current source findings are inconsistent handling of native persistence failures, storage errors being converted into valid-looking default state, and a shared maker margin budget being reused across books. Two EVM integration findings also deserve focused reproduction before relying on typed transactions or deployed contract bytecode. The exchange lacks funding, native execution fees, complete market lifecycle controls, and several risk/API capabilities necessary for mature perpetual trading.

This is **not a formal audit, an exploitation demonstration, or a production readiness certification**. No live validator configuration, balances, oracle feed, deployment topology, private frontend, or operating procedures were inspected. The findings below describe the pinned repository; deployment choices can mitigate or amplify them.

## Navigation

- [Scope, evidence, and metric rules](#scope-evidence-and-metric-rules)
- [Priority map](#priority-map)
- [Execution and economic safety: R01–R08](#execution-and-economic-safety)
- [Trading model and protocol controls: R09–R17](#trading-model-and-protocol-controls)
- [Governance, bootstrap, EVM, and RPC: R18–R24](#governance-bootstrap-evm-and-rpc)
- [Exchange operations, performance, and recovery: R25–R32](#exchange-operations-performance-and-recovery)
- [Already addressed findings and remaining coverage limits](#already-addressed-findings-and-remaining-coverage-limits)
- [Measurement and remediation sequence](#measurement-and-remediation-sequence)
- [External sources](#external-sources)

## Scope, evidence, and metric rules

Reviewed the consensus application and proposal/commit boundary, native executor and matching book, margin/oracle/liquidation paths, EVM decoding/execution/commit, state overlays and running hash, genesis, staking and governance, native/EVM mempools, RPC/streams, snapshot restore, explorer indexing, and relevant current tests/plans. This was not a line-by-line proof of every crate or dependency. Vendor cryptography, networking implementations, and all HotStuff safety proofs were not independently audited.

**Evidence labels:** **S** = mechanism directly observed in current source; its example can still be illustrative. **C** = candidate consequence requiring runtime reproduction, with a concrete source mechanism. **G** = capability gap or policy choice, not automatically a security bug. **P** = performance hypothesis or historical profile, not a current measurement. **O** = operational/configuration boundary. Labels can coexist. “Critical candidate” describes the potential consequence under the stated conditions, not a demonstrated exploit.

Source links point to files in the reviewed repository with line anchors. The pinned SHA above is the source of truth if line numbers change later. External comparisons use public primary documentation retrieved on the review date. Hyperliquid documentation describes its intended behavior and advertised performance; it is not independent verification of its implementation. “CEX” is qualified by product: Coinbase Exchange is used for spot matching, Kraken Derivatives for perpetual risk and protection. They do not represent every exchange.

**Metric conventions used throughout:**

| Metric | Meaning and comparison boundary |
|---|---|
| Blockspeed | Distinguish proposal/consensus interval, time to execute, and time to durably apply. Faster certification with a growing execution queue is not faster usable settlement. Report p50/p95/p99 and lag, not just a mean. |
| Torus matched/s | The harness uses the rate of `torus_orders_matched_total`; executor increments this by `result.fills.len()`. It measures fill records, not two order sides per fill, submitted orders/sec, notional/sec, or unique completed orders/sec. See [increments](../../crates/torus-bridge/src/native_executor.rs#L6962) and [summarizer](../../tools/matched-bench/summarize.py#L95). |
| Hyperliquid performance | Public docs advertise approximately 200k **orders/sec** and co-located end-to-end latency figures. These are different workloads/units from Torus fill records/sec. There is no measured parity ratio in this report. [HL architecture][HL-O] |
| CEX performance | No verified comparable capacity or latency dataset was obtained. Private risk/matching architectures cannot be inferred from an API. Compare observable behavior; numerical performance parity remains unknown. |
| Good throughput | Count accepted, valid, durably settled fills separately from rejected, cancelled, deferred, or insolvency-generating work. Tightening risk checks can lower matched/s while improving useful throughput. |
| Directional estimates | `≈` means expected little healthy-path effect; `↓`/`↑` describe likely direction, not magnitude. Every numerical example below is hypothetical unless explicitly marked historical. No fix has a measured speedup here. |

Storage fail-stop can reduce service availability during faults while preserving correctness. Funding, fees, or stronger risk controls can reduce headline matched/s by adding real economic work. Neither is evidence that the fix is a regression in financial correctness. If a fixed workload generates F fill records and the bottleneck takes T seconds, its upper-bound rate is F/T; improvements elsewhere may have zero throughput effect.

## Priority map

Severity depends on deployment and exposure. The order below is a review recommendation, not an implementation or launch authorization.

| ID | Finding | Label | Priority / consequence |
|---|---|---|---|
| R01 | Native serial write failures and marker-only failures continue | S/C | P0; potential durable state/height inconsistency |
| R02 | Local read failures become absent/default state | S/C | P0; deterministic execution boundary risk |
| R03 | Typed EVM envelopes retain default legacy transaction type | S/C | P0 reproduction; potential fee/receipt mismatch |
| R04 | Analyzed bytecode bytes are persisted as original code | S/C | P0 reproduction; cold-reload code semantics risk |
| R05 | Maker free margin reused per market | S | P0; account-wide risk overcommit, explicit test-source evidence |
| R06 | Round-robin liquidation latency and action caps | S/G | P1; risk backlog during correlated moves |
| R07 | Bounded ADL and unfunded backstop obligations | S/G | P1; losses recorded without a capital/unwind policy |
| R08 | Stale/unavailable marks permit partial or absent risk valuation | S/G | P1; outage exposure |
| R09 | Oracle price doubles as mark; stops use last traded price | S/G | P1; protective-order/risk mismatch |
| R10 | Funding is absent | G | P1 before claiming perpetual economic parity |
| R11 | Native trading fees are absent | S/G | P1; revenue and backstop capitalization gap |
| R12 | Unknown markets execute; market governance has no-op operations | S/G | P1; registry and market lifecycle gap |
| R13 | Flat risk tiers and incomplete isolated/leverage controls | S/G | P1; risk product limitations |
| R14 | Equal/old block timestamps remain admissible | S/G | P1; stale protocol time under adverse leadership |
| R15 | State commitment/proofs and fail-stop defaults | S/O | P1; detection and external verification limitations |
| R16 | Honest proposer limits are not complete consensus work limits | S/C | P1; adversarial execution backlog risk |
| R17 | No general versioned activation/migration framework | G/O | P1 before incompatible upgrades |
| R18 | Governance snapshots use live fallbacks/quorum | S | P1; mutable voting eligibility and denominator |
| R19 | Genesis validation and canonical key collisions | S/C | P1; nondeterministic or inconsistent bootstrap inputs |
| R20 | Devnet/testnet chain ID reuse | S/G | P2; conditional cross-network replay |
| R21 | EVM pool drains before durable inclusion; nonce readiness | S/C | P1; disappearing or unexecutable transactions |
| R22 | Historical ETH reads silently use current state | S/G | P2; incorrect client answers and estimation bounds |
| R23 | Risk RPC omits cross margin and useful liquidation estimates | S | P1 client correctness |
| R24 | Full-scan public RPC work shares resources with validators | S/C | P1 exposure-dependent; read-load amplification |
| R25 | Async history and incomplete lifecycle/stream surface | S/G | P1 durability; P2 product coverage |
| R26 | Crossing modify is rejected | S/G | P2; intentional restriction, amend parity gap |
| R27 | Missing dead-man/expiry and client-order controls | G | P1 trading protection; P2 advanced features |
| R28 | Broad scans, flush work, moving-mark revaluation, EVM barriers | P/S | P2; measured-again optimization candidates |
| R29 | Snapshot integrity is not trusted checkpoint authenticity | S/O | P1 operational recovery |
| R30 | Explorer partial indexing can become permanent | S | P2; audit/UI completeness |
| R31 | In-block deterministic sorting differs from arrival priority | S/G | P2; fairness policy disclosure |
| R32 | TRS collateral versus stablecoin perpetual economics | S/G | P1 product/solvency model |

## Execution and economic safety

### R01 — Native serial persistence failures do not consistently halt execution

**S/C; P0.** The serial state+applied-marker flush logs a failed write, but latches `exec_failed` only when the block contains EVM transactions. Native-only execution can proceed after that failure. The marker-only helper also logs write failure without latching. By contrast, the background flush worker fail-stops. Pipelining is on by default, but serial execution remains reachable at epoch boundaries, with pending slashes, replay/configuration paths, and when the pipeline is disabled. This is not just a default-off corner case.

Evidence: [serial flush](../../crates/torus-consensus/src/app.rs#L2735), [native/EVM error distinction](../../crates/torus-consensus/src/app.rs#L2762), [marker-only flush](../../crates/torus-consensus/src/app.rs#L692), [pipeline eligibility](../../crates/torus-consensus/src/app.rs#L1796), [worker fail-stop](../../crates/torus-consensus/src/exec_pipeline.rs#L1).

| Comparison | Today | A: fail-stop all durable write failures | B: retry the same immutable batch, then fail-stop |
|---|---|---|---|
| Example | Native transfer at H fails to persist; H+1 later persists its own state and a higher applied marker. Replay can skip H. This fault sequence is not runtime-tested here. | Failure at H prevents execution/voting advancement; restart reconciles from the last durable marker. | A temporary failure retries H's exact batch; H+1 waits until H is durable. |
| Improvement / reasoning | Continuation preserves apparent availability but loses the write/height invariant. | Uniform fail-stop matches the EVM/worker invariant and keeps recovery understandable. | Tolerates short storage faults without abandoning ordered durability. |
| Negative / regression | A logged error can conceal missing economic transitions; streams can imply success. | A sick disk stops a validator; enough sick validators can stop the chain. | Retry tail latency and queue memory; needs a deadline and no mutation of the queued batch. |
| Blockspeed / matched/s / other | Healthy speed unaffected by the latent fault; reported progress during fault is not useful throughput. | Healthy ≈; fault throughput becomes zero on that node, correctly. Recovery/correctness improve. | Healthy ≈; fault durable latency ↑ and matched/s temporarily ↓; fewer unnecessary restarts. |
| HL / CEX parity | Cannot establish storage invariants of either venue from public APIs. | Supports the observable expectation of durable, non-skipped settlement at both. | Same parity goal with better transient-fault availability. |

**Recommendation:** A first; add B only with immutable batch retention and explicit durable acknowledgment. Re-executing against partially mutated caches is not a safe retry strategy. Required validation: write failures at native-only serial, marker-only, epoch and replay boundaries; restart and compare all economic rows, applied height, and running hash against an uninterrupted reference.

### R02 — Storage errors are converted into ordinary execution state

**S/C; P0.** Native session lookup uses `.ok().flatten()`, replay protection uses `unwrap_or(None)`, market shape lookup falls back to one-unit lot/tick, and maker account reads can fall back to zero/flat. A missing row can be legitimate; a local I/O or decode error is different. Mapping both to the same value means validators can make different decisions from the same committed inputs when one has a faulty local database. An assertion that a fallback is “deterministic” does not make local storage faults deterministic.

Evidence: [session verification callback](../../crates/torus-consensus/src/app.rs#L2091), [nonce check](../../crates/torus-consensus/src/app.rs#L2196), [maker reader](../../crates/torus-bridge/src/native_executor.rs#L1728), [market fallback](../../crates/torus-bridge/src/native_executor.rs#L8293).

| Comparison | Today | A: propagate typed storage errors and halt | B: validated prefetch/read view plus A at its boundary |
|---|---|---|---|
| Example | One node cannot read a consumed nonce and treats it as absent; another reads it and rejects the replay. Divergent execution is a candidate consequence. | Both distinguish “missing nonce” from “cannot determine nonce”; the faulty node stops rather than executes a guess. | Resolve the block's required rows into a consistent validated view before account/book mutation. |
| Positives / reasoning | Cheap fallback code. | Restores the consensus fault boundary; ordinary user rejection remains deterministic. | Can batch reads and reduce repeated lookups; errors detected earlier. |
| Negatives / regressions | Silent defaults can reject valid actions or authorize invalid ones. | More `Result` plumbing; malformed rows now cause visible outages needing repair. | Prefetch memory and dependency enumeration; dynamic touched accounts still require checked reads. |
| Blockspeed / matched/s / other | Fast on faults by doing wrong or incomplete work. | Healthy ≈; fault progress stops; divergence risk ↓. | Healthy may improve through fewer reads or worsen for over-prefetch; must measure read amplification. |
| HL / CEX parity | APIs do not reveal internal DB error policy. | Consistent settlement is necessary at both venues. | Same behavioral parity; no claimed architecture equivalence. |

**Recommendation:** A is mandatory even if B is adopted. Audit every consensus-path `.ok()`, `unwrap_or`, and error-catching default; preserve intentional protocol defaults only for verified absence. Fault injection must cover sessions, nonces, balances, positions, and market rows independently.

### R03 — Typed EVM conversion leaves the transaction type at the default

**S/C; P0 reproduction.** `envelope_to_tx_env` fills legacy, EIP-2930, and EIP-1559 fields using struct literals with `..Default::default()`, without setting `tx_type`. Pinned `revm-context 15.0.0` defaults to type 0; pinned `revm-context-interface 16.0.0` chooses gas price behavior using that type. The executor passes the resulting environment to `inspect_tx_commit`; no explicit type derivation appears in the reviewed Torus path. Separately, receipts compute an effective price from fee cap, base fee, and tip. The mechanism is observed; the exact debit/proposer/receipt mismatch needs a runtime reproducer against this dependency set.

Evidence: [conversion](../../crates/torus-bridge/src/decode.rs#L96), [execution](../../crates/torus-evm/src/executor.rs#L298), [receipt price](../../crates/torus-evm/src/executor.rs#L361), [pinned dependencies](../../Cargo.lock). Dependency evidence inspected locally: `revm-context-15.0.0/src/tx.rs:91`, `revm-context-interface-16.0.0/src/transaction.rs:147`. EIP-1559 specifies capped base fee plus priority fee behavior. [Specification][EIP-1559]

| Comparison | Today | A: set the exact type from the signed envelope | B: construct with validated typed builders |
|---|---|---|---|
| Example | Type-2 cap=100, tip=2, base=1 wei/gas: expected effective price 3; retaining type 0 can select 100 while the receipt says 3. | Explicit type 2 selects the intended pricing rules. Explicit type 1 stays type 1 even with an empty access list. | A typed constructor carries all type-dependent fields together and rejects inconsistent combinations. |
| Positives / reasoning | Envelope fields are mostly preserved, but classification is missing. | Small targeted change; faithful to signed transaction identity. | Reduces future field/type drift when adding supported transaction types. |
| Negatives / regressions | Potential excess debit, incorrect GASPRICE/accounting, or wrong validation behavior. | Must verify every supported type and direct test-created `TxEnv` path. | Builder inference based only on populated fields can misclassify type 1 with an empty list; use an explicit type. |
| Blockspeed / matched/s / other | Native matched/s unrelated; EVM correctness at risk. | Healthy speed ≈; possible EVM acceptance/debit changes are intended corrections. | Small construction overhead; no justified throughput percentage. |
| HL / CEX parity | HyperEVM compatibility is a separate product from HyperCore trading; no exact implementation comparison. | Restores Ethereum typed-transaction semantics. CEX trading API fee schedules are not EVM gas fees. | Same compatibility goal with safer future extension. |

**Recommendation:** A with focused type-0/type-1/type-2 cases; compare sender debit/refund, proposer revenue, GASPRICE, receipt price, revert behavior, and restart. Do not call the financial consequence proven until those cases run.

### R04 — Persisting analyzed bytecode can retain execution padding

**S/C; P0 reproduction.** Bridge committers and incremental state storage call `bytecode.bytes()`. Pinned `revm-bytecode 9.0.0` documents that analyzed legacy bytecode includes padding in these bytes; `original_bytes()` removes it using the original length. State reload constructs `Bytecode::new_raw` from stored bytes. If a returned creation bundle contains analyzed code, padded bytes can become the apparent original code after reload. The exact cold/warm behavior needs a deployed-contract reproducer.

Evidence: [committer](../../crates/torus-bridge/src/committer.rs#L84), [second committer](../../crates/torus-bridge/src/committer.rs#L258), [incremental writer](../../crates/torus-state/src/incremental.rs#L307), [reload](../../crates/torus-state/src/db.rs#L1304). Dependency evidence: `revm-bytecode-9.0.0/src/bytecode/mod.rs:294–325` distinguishes `bytes()` and `original_bytes()`.

| Comparison | Today | A: store original bytecode in every writer | B: version raw code and optional analysis separately |
|---|---|---|---|
| Example | Deploy short runtime code; analyzed STOP padding is stored. Restart can expose extra bytes through code reads or EXTCODESIZE while using the old code-hash key. | Stored bytes exactly equal deployed runtime bytes before and after restart. | Raw bytecode/hash remain authoritative; cached analysis has its own version/length and can be rebuilt. |
| Positives / reasoning | Simple persistence API but wrong abstraction if code is analyzed. | Minimal durable representation; validate `keccak(raw) == code_hash`. | Potentially faster cold execution while preserving original-code semantics. |
| Negatives / regressions | Possible code length/hash inconsistency and warm/cold differences. | Existing rows need investigation; blindly trimming trailing zero bytes would corrupt legitimate code. | More storage, version compatibility and corruption paths. |
| Blockspeed / matched/s / other | Native matching independent; cold EVM correctness uncertain. | Native ≈; EVM load cost ≈ or slightly ↑ if analysis is rebuilt. | Cold EVM may improve; extra write bytes/storage ↑. |
| HL / CEX parity | Ethereum code semantics are the relevant standard, not CEX order matching. | Compatibility improves; do not infer HyperEVM behavior without testing it. | Same standard with an explicit cache boundary. |

**Recommendation:** A, plus a migration policy based on verified deployment artifacts or replay. Reproduce original runtime bytes, code hash, EXTCODESIZE/EXTCODEHASH, `eth_getCode`, calls and state commitments across warm execution and restart.

### R05 — Parallel books reuse the same maker account's free budget

**S; P0.** Market tasks snapshot account free margin after phase 1 and perform per-book maker charges. An account participating in several markets can spend the same residual free amount independently in each. This is deterministic overcommit, not merely a scheduling race: the existing test source explicitly expects the same behavior with one and four workers.

Evidence: [market execution](../../crates/torus-bridge/src/native_executor.rs#L6133), [maker free reader](../../crates/torus-bridge/src/native_executor.rs#L1728), [test `one_batch_charges_a_makers_snapshot_free_once_per_book`](../../crates/torus-bridge/tests/offmark_bad_debt_tests.rs#L498), [plan acknowledgment](../../docs/plans/item6-phase1-impl.md#L711).

The test's example: three books at mark 100; maker bids 10 at 103 in each. Each reservation is 51.50 and initial cash is 158, leaving 3.50 free. Each off-mark fill charges 3.50 against its local snapshot. All three fill; the recorded final account has equity 68, free −82, and stage-1 liquidation health. With only 3.49 snapshot free, all three are cancelled. These are source assertions, not tests executed during this review.

| Comparison | Today | A: deterministic split of shared free across books | B: serialize account-conflicting components | C: reserve off-mark exposure up front plus global fill budgets |
|---|---|---|---|---|
| Example | Each of three books sees all 3.50. | Allocate ≈1.16 per book; all three charges fail, or use a stable allocation favoring one book. | One stable first fill consumes 3.50; subsequent fills see the reduced global account budget. | Reservations include each order's off-mark risk; insufficient collateral rejects/rests fewer orders before the match wave. |
| Positives / reasoning | Good independent-market parallelism. | Simple safety bound without shared mutable hot-path state. | Maintains disjoint-market parallelism while enforcing account-wide affordability. | Prevents some risk from entering the book; improves predictability for makers. |
| Negatives / regressions | Account can become unhealthy from reuse of collateral. | Stranded collateral and avoidable cancellations; allocation policy affects liquidity. | Popular cross-market makers couple tasks and can reduce parallelism substantially. | More collateral, revaluation on price changes, and risk-model complexity; needs a shared budget even after reservation. |
| Blockspeed / matched/s / other | Headline fills include overcommitted risk. | Blocktime ≈ or slightly ↓ from fewer fills; matched/s ↓ when budget is fragmented. | Conflict-heavy blocktime ↑ / matched/s ↓; disjoint markets ≈. | Per-order and mark-update cost ↑; unsafe fills ↓, healthy resting depth may ↓. |
| HL / CEX parity | Cross-margin checks should respect one account's actual capacity. | Conservative approximation; poorer capital efficiency than expected cross-margin behavior. | Closer behavioral parity to shared-account risk; private venue implementations are unknown. | Closer reservation/risk parity if capital is reused correctly; not proof of HL's internal algorithm. |

**Recommendation:** B for exact shared-budget semantics, with A as an explicit temporary safety policy if needed. C complements B. A tighter price band alone is insufficient; post-fill liquidation does not undo an unaffordable trade. [HL margin rules][HL-M] provide the behavioral comparison, not an architecture prescription. Measure conflict-component size, rejected maker fills, free-margin overcommit, and healthy durable matched/s.

### R06 — Liquidation latency scales with a round-robin backlog

**S/G; P1.** A pass scans at most 2,048 traders and acts on at most 64. The cursor provides progress, but touched accounts or users affected by a price shock do not automatically get a risk-priority pass. Reaching the action cap can stop scanning before the scan cap. One trader can also have many positions, so neither cap is a strict millisecond limit.

Evidence: [constants](../../crates/torus-core/src/liquidation.rs#L58), [scan/action loop](../../crates/torus-bridge/src/liquidation_step.rs#L400).

| Comparison | Today | A: deterministic affected-account priority plus fair sweep | B: persistent deterministic urgency queue | C: increase caps only |
|---|---|---|---|---|
| Example | 100,000 eligible accounts take at least 49 passes to scan if no action cap intervenes; acting on all takes at least 1,563 passes. At an illustrative 0.2s/pass those are 9.8s and 312.6s, not SLAs. | Accounts changed by fills/withdrawals/mark moves get bounded priority; reserve sweep capacity so idle users cannot starve. | Highest-risk queued accounts are processed first, with stable tie-breaks and aged entries. | Doubling caps reduces nominal passes but can double expensive work per block. |
| Positives / reasoning | Bounded account count and predictable fair cursor. | Fast response where risk was just created; no need to reprioritize all healthy users. | Better correlated-shock response and observable pending obligations. | Small implementation change. |
| Negatives / regressions | Backlog can outlive the price move and enlarge bad debt. | Needs market-holder indexes; a broad oracle move can enqueue most users. | Stale priorities, update costs and adversarial churn; queue ordering must be consensus deterministic. | Blocktime cliffs remain, with no prioritization or service guarantee. |
| Blockspeed / matched/s / other | Throughput can look strong while liquidation latency deteriorates. | Routine block cost ↑ modestly; shock fills/s ↓ as safety work takes capacity. | Queue/index writes ↑; improves risk age, potentially worsens normal p99. | Heavy block p99 ↑; sustainable matched/s can ↓ despite more liquidations per block. |
| HL / CEX parity | Staged liquidation exists, but timely reaction is not established at scale. | Closer to timely margin intervention described by [HL][HL-L] and [Kraken][K-L]. | Same behavioral goal; no verified private scheduler parity. | Improves capacity only; timing parity remains unproven. |

**Recommendation:** A then B if measured risk age demands it. Track scanned/acted/deferred counts, oldest unhealthy age, queue depth, per-account position work, and debt created before versus after intervention. Use moving marks and many holders, not only a fixed-price benchmark.

### R07 — Backstop accounting exists without a complete funded loss/unwind policy

**S/G; P1.** The code has stage-1 book liquidation, backstop transfers, escrow obligations, ADL ranking/caches, and a bounded ADL work budget. It is incorrect to call ADL absent. However, positions and deficits can move to a protocol vault without a defined funded insurance reserve, inventory strategy, or automatic unwind policy. The module explicitly notes no insurance fund/socialized-loss mechanism. Stage-B transfer work itself is not cut by the ADL ranking/transfer budget; work can be deferred after obligations are created. A negative vault balance is recorded/observable, not proof it has been paid.

Evidence: [liquidation model](../../crates/torus-core/src/liquidation.rs#L5), [ADL budget](../../crates/torus-core/src/liquidation.rs#L72), [backstop/escrow step](../../crates/torus-bridge/src/liquidation_step.rs#L433), [flat deficit handling](../../crates/torus-core/src/liquidation.rs#L467).

| Comparison | Today | A: capitalize a backstop and define inventory/unwind rules | B: insurance-first loss waterfall, ADL for residuals |
|---|---|---|---|
| Example | A liquidated trader's residual −100 cash and position enter the vault; ADL obligations can wait for later work. Recording −100 does not finance it. | A funded reserve absorbs −100 and unwinds the position under explicit exposure/slippage caps. | Insurance covers available losses; a precisely specified residual obligation closes eligible opposing exposure deterministically. |
| Positives / reasoning | Preserves accounting evidence and implements bounded ADL machinery. | Actual loss-absorbing capital; inventory and obligations become explicit operating risks. | Defines who pays, in what order, and what happens when reserve capital is insufficient. |
| Negatives / regressions | Losses can accumulate faster than work drains; vault inventory creates market risk. | Requires capital, replenishment, governance, and controls against draining the reserve. | ADL harms otherwise healthy counterparties; rounding, rank and settlement rules are difficult. |
| Blockspeed / matched/s / other | Work budget is not a wall-clock or gas bound; debt age can grow. | Book/unwind work ↑; normal matching may ↓ during stress; solvency buffer ↑. | Stress work and writes ↑; debt age ↓ if funded/serviced; healthy users may lose exposure. |
| HL / CEX parity | Partial liquidation mechanics parity, incomplete capital model. | Closer to a capitalized backstop such as HL's liquidation vault structure. [HL liquidation][HL-L] | Closer to a defined ADL policy. HL describes residual close-out at its previous mark; equivalence requires explicit Torus rules. [HL ADL][HL-A] CEX loss waterfalls vary. |

**Recommendation:** Specify and fund the loss waterfall before increasing leverage/markets. A deterministic reduce-only circuit breaker can limit aged obligations but is not a replacement for funding them. Measure vault equity/inventory, deficit, oldest obligation, ADL work units versus milliseconds, and stress-time user losses.

### R08 — Stale marks produce incomplete account valuation rather than a full outage policy

**S/G; P1.** Oracle inputs have freshness and reporter thresholds. With no usable marks, liquidation skips account valuation. For a mixed account with at least one marked cross position, current code can build a view using entry price for an unmarked position. An isolated position causes this cross-liquidation path to skip the account. Thus the current behavior is **not** “one missing mark always skips the entire account.” It is a combination of partial entry-price fallback and entirely unvalued accounts. Placement has fallback price-band/reference paths; there is no comprehensive outage rule for all risk-increasing actions.

Evidence: [oracle defaults](../../crates/torus-core/src/oracle.rs#L24), [liquidation actual conditions](../../crates/torus-bridge/src/liquidation_step.rs#L790), [walk valuation](../../crates/torus-bridge/src/liquidation_step.rs#L826), [account price fallback](../../crates/torus-core/src/margin.rs), [book reference fallback](../../crates/torus-core/src/order_book.rs#L592).

| Comparison | Today | A: explicit stale-risk circuit breaker | B: bounded conservative stale valuation plus A |
|---|---|---|---|
| Example | Entry=100, real market=70, feed expires: an unmarked long can still be valued at 100 or omitted from liquidation. Another marked position does not repair its valuation. | Block risk-increasing orders and collateral withdrawals against material stale exposure; preserve cancels and carefully bounded reduce-only actions. | Use the last trusted mark plus a deterministic, capped adverse haircut while stale, then follow explicit outage/close rules. |
| Positives / reasoning | Avoids liquidating blindly against absent data. | Prevents risk expansion during an information outage. | Gives a conservative risk bound instead of pretending entry equals market. |
| Negatives / regressions | Existing losses remain hidden; withdrawal/order behavior can be inconsistent across risk paths. | Trading/withdrawal availability ↓; does not eliminate losses on existing positions. | Haircuts can over-liquidate or unfairly restrict users; cannot guarantee real-market accuracy. |
| Blockspeed / matched/s / other | Outage can make execution faster by skipping risk work. | Matched/s ↓ in affected markets; deterministic checks cheap; unsafe exposure ↓. | Valuation work ↑; liquidation pressure ↑; tail loss may ↓ at cost of false restrictions. |
| HL / CEX parity | Oracle aggregation exists, but robust-outage semantics are incomplete. | Protective risk policy improves venue parity without claiming exact HL/CEX outage behavior. | Closer to multi-input mark/risk controls; compare [HL robust indices][HL-R], not an assumption that their outage handling is identical. |

**Recommendation:** A first with one consistent freshness predicate across margin, withdrawal, matching, stops, liquidation and RPC. B requires economic simulation and an explicit fairness policy. All consensus freshness uses agreed block time, not validator-local time; R14 therefore matters.

## Trading model and protocol controls

### R09 — Mark price and stop triggers do not match the risk model of a mature perpetual venue

**S/G; P1.** RPC exposes the same oracle aggregate as mark and index. Stop triggering uses last traded price and runs from the fill path, so an oracle-only move does not necessarily activate a protective stop. Oracle submissions are consumed at block start from earlier committed state, adding a designed block delay. These are distinct mechanisms: adding a richer mark alone does not fix stop scheduling.

Evidence: [mark/index RPC](../../crates/torus-rpc/src/torus.rs#L2055), [oracle block start](../../crates/torus-bridge/src/native_executor.rs#L10073), [stop trigger](../../crates/torus-core/src/order_book.rs#L2872), [fill-driven trigger invocation](../../crates/torus-core/src/order_book.rs#L1731).

| Comparison | Today | A: explicit mark/index/last trigger selector and block-start evaluation | B: robust mark construction plus A |
|---|---|---|---|
| Example | Sell stop at 90, last trade 100, oracle drops to 85 with no fills: the stop can remain pending while risk valuation moves toward liquidation. | A mark-triggered stop enters the deterministic trigger queue at 85 even without another trade; a last-triggered stop preserves today's chosen semantics. | Construct a mark from signed external index and bounded book/basis inputs, then evaluate mark-triggered protection. |
| Positives / reasoning | Simple feed and matching-path trigger. | Makes protective behavior explicit and aligns the selected trigger with risk accounting. | Resists one-source manipulation and local transient book distortions. |
| Negatives / regressions | Users can confuse “stop price” with liquidation's reference price. | Trigger storms add work; queue limits need defined delays and settlement semantics. | More oracle/book state, deterministic EMA/median rules, parameter governance and adversarial input cases. |
| Blockspeed / matched/s / other | Cheap during no-trade oracle moves but protection is delayed. | Active-trigger blocktime ↑; benign blocks can use indexed thresholds; stop-to-execution latency ↓. | Mark-update CPU/state ↑; matched/s may ↓ if mark/risk work becomes the bottleneck. |
| HL / CEX parity | HL documents a separate robust mark and mark-based TP/SL. [Indices][HL-R], [orders][HL-T] | Moves toward that protective-order behavior; CEX trigger sources vary and should be explicit. | Closer HL economic parity, without copying parameters blindly or claiming the same internals. |

**Recommendation:** A independently of B. Use incremental stop threshold indexes, a deterministic bounded trigger queue, and the normal settled-fill path. Test no-trade mark moves, partial fills, gap/slippage, stale feeds, and a stop/liq race. Display RPC timestamps in their actual units: the current mark timestamp field is sourced from an oracle block number, not a wall-clock timestamp.

### R10 — Perpetual funding is not implemented

**G; P1.** Market action types carry `max_funding_rate_bps`, but the reviewed execution/risk paths do not implement a funding rate sampler, cumulative funding index, periodic transfers, or accrued funding liability. This is an economic gap rather than an incorrect existing funding algorithm.

Evidence: [market parameters](../../crates/torus-types/src/lib.rs#L1252), [native executor](../../crates/torus-bridge/src/native_executor.rs), [margin](../../crates/torus-core/src/margin.rs).

| Comparison | Today | A: cumulative market index with lazy account settlement | B: periodic eager settlement of all holders |
|---|---|---|---|
| Example | Long and short each have 10,000 notional; an illustrative +0.01% interval transfers nothing. | Market index advances; the long owes 1 and short receives 1. Include the accrued amount in risk before either interacts. | At the scheduled interval, write the 1 debit/credit to both accounts directly. |
| Positives / reasoning | Minimal cost; no funding accounting errors yet. | O(markets) scheduled work; holders materialize only when needed. | Straightforward visible balances and audit trail. |
| Negatives / regressions | No funding anchor between perpetual and underlying; balance/risk differs from perpetual expectations. | Every margin/withdrawal/liquidation/RPC path must include unmaterialized accrual; rounding must conserve value. | O(holders) writes and interval spikes; chunking must not give users different economic accrual times. |
| Blockspeed / matched/s / other | Faster by omitting a real product cost. | Small regular market cost; touched-account cost ↑; basis/funding liabilities become measurable. | Funding-block p99 ↑ and matched/s ↓; writes/storage ↑ materially at scale. |
| HL / CEX parity | Not parity with perpetual funding. | HL pays hourly using a premium-related rate and oracle notional; match its observable accrual/payment semantics if that is the target. [Funding][HL-F] | Can match payment semantics with higher scaling cost. Kraken also exposes accrued funding in portfolio accounting. [Portfolio][K-P] |

**Recommendation:** A, with conservative overflow/rounding rules, an explicit rate source/cap and all risk views sharing one accrued-equity definition. A funding index that appears only on user action would permit stale-risk withdrawals. Measure per-market update cost, lazy settlement cost, long/short conservation, basis, and accrued funding before interaction.

### R11 — Native fills do not charge a trading fee

**S/G; P1.** `total_native_fees` is initialized to zero and reaches distribution without an observed increment/debit on native fills. Nominal native action `gas_used` is not a native trading-fee charge. EVM fee collection/distribution is separate and implemented; this finding does not say all chain revenue is missing.

Evidence: [context accumulator](../../crates/torus-bridge/src/native_executor.rs#L3608), [initialization](../../crates/torus-bridge/src/native_executor.rs#L4117), [distribution](../../crates/torus-bridge/src/native_executor.rs#L10173).

| Comparison | Today | A: flat deterministic maker/taker rates per fill | B: A plus rolling-volume tiers and funded rebates |
|---|---|---|---|
| Example | A 10,000-notional fill transfers no native fee. | Illustrative taker 4bp and maker 1bp debit 4 and 1; credit 5 to defined fee buckets atomically. These are proposed example rates, not HL's rates. | An account's volume tier changes future rates; a maker rebate is paid from a bounded fee/rebate reserve. |
| Positives / reasoning | Low trading cost and accounting work. | Revenue and backstop capitalization; simple auditable conservation. | More commercial flexibility and maker incentives. |
| Negatives / regressions | No native revenue to support insurance or sustainable operation. | Net PnL changes; affordability must include fees before filling; tiny trades need careful rounding. | Window maintenance, affiliate/tier state and rebate solvency complexity. |
| Blockspeed / matched/s / other | Fewer balance writes/arithmetic. | Fill cost and bytes ↑; matched/s likely modestly ↓ unless writes dominate; exact amount unmeasured. | Additional state/lookups ↑; maker depth may improve while raw CPU capacity declines. |
| HL / CEX parity | Fee-free policy is permitted, but is not fee schedule parity. | Basic venue accounting parity. | Closer HL's documented volume tiers/rebates and common CEX fee products. [HL fees][HL-FE] |

**Recommendation:** A first with a declared fee denomination and destination. Do not mix `FixedPoint`, native raw units and EVM wei conversion. Test affordability, flips, liquidation, self-trade controls, dust, conservation and rebates under zero treasury; record net fees and treasury/vault capital in performance cells.

### R12 — Unknown market IDs can execute, and governance market operations can be text-only

**S/G; P1.** Placement can auto-create an in-memory book, and absent market metadata falls back to one-unit lot/tick and default margin settings. Thus an otherwise valid signed action can reach matching without an authoritative listed-market gate. RPC checking alone cannot protect direct consensus or EVM/CoreWriter entry points. Direct market mutations are refused as governance-only, but governance update/delist proposals can produce no execution payload and be treated as text-only. List has a real implementation; do not describe all market governance as absent.

Evidence: [shape fallback](../../crates/torus-bridge/src/native_executor.rs#L8293), [book insertion](../../crates/torus-bridge/src/native_executor.rs#L8647), [batch insertion](../../crates/torus-bridge/src/native_executor.rs#L6183), [direct governance-only handling](../../crates/torus-bridge/src/native_executor.rs#L5487), [proposal payload construction](../../crates/torus-bridge/src/native_executor.rs#L9832).

| Comparison | Today | A: execution-level active-market gate and full lifecycle payloads | B: strict registry gate; reject unsupported governance actions |
|---|---|---|---|
| Example | A funded account places on market 999 with no listing; defaults can admit it. A voted delist proposal need not disable its market. | Unknown 999 rejects before reservation/ID allocation. Delist transitions active→reduce-only→defined settlement/closed state. | Unknown 999 rejects; delist/update proposal submission explicitly says unsupported until implemented. |
| Positives / reasoning | Convenient fallback for tests; listing works. | One authoritative lifecycle across all entry paths, matching and risk. | Small, clear safety improvement without pretending no-op governance performs changes. |
| Negatives / regressions | Trades can exist outside approved product definitions; governance result can be misleading. | Delisting open positions/orders requires a fair price, funding/fee handling and dependency ordering. | Product capability remains missing and manual coordinated changes are still needed. |
| Blockspeed / matched/s / other | Default lookups cheap. | Cached registry lookup ≈; migration/settlement blocks can be expensive; invalid work ↓. | Normal ≈; unknown-market matched/s disappears appropriately. |
| HL / CEX parity | Missing core listing/delisting enforcement. | Closer controlled contract/product lifecycle at both venues. | Better honesty and safety; capability parity deferred. |

**Recommendation:** B's gate/rejection first, then A. A storage read error must not become “unknown with defaults” (R02). Test native single/batch, CoreWriter, stop conversion, liquidation, replay, and governance status; ensure market IDs and tick/lot rules remain stable throughout lifecycle changes.

### R13 — Production risk settings flatten tiers; isolated and leverage controls are incomplete

**S/G; P1.** Core margin code supports tiers, but the executor derives one unlimited-notional tier from each market's initial-margin parameter (with default 20x behavior), and maintenance margin is tied to that simple model. A type-level `MarginType::Isolated` exists; ordinary fill handling and public controls do not provide a complete isolated-margin/user-leverage product. Do not infer API support from enum fields. Reduce-only and cross-margin checks are implemented.

Evidence: [market margin derivation](../../crates/torus-core/src/margin.rs#L384), [tier representation](../../crates/torus-core/src/margin.rs#L16), [action surface](../../crates/torus-types/src/lib.rs), [fill and liquidation handling](../../crates/torus-bridge/src/native_executor.rs), [isolated account skip](../../crates/torus-bridge/src/liquidation_step.rs#L826).

| Comparison | Today | A: explicit limited cross-only product with conservative caps | B: full tiers, user leverage, and isolated lifecycle |
|---|---|---|---|
| Example | Small and very large notional can use the same market-derived leverage; a client cannot fully manage an isolated position just because the type exists. | Publish one cross tier and enforce account/market notional/OI caps to bound concentration. | As notional grows, required margin rises through tiers; users select valid leverage and add/remove isolated collateral under checks. |
| Positives / reasoning | Simple fast account arithmetic. | Honest launch surface with bounded concentration; easier validation. | Better risk scaling and user control; closer perpetual venue capabilities. |
| Negatives / regressions | Large concentrations share small-position leverage; unsupported isolated state is risky. | Capital efficiency and feature set remain limited. | New state transitions, tier-boundary rounding, maintenance rules and migration complexity. |
| Blockspeed / matched/s / other | Minimal tier lookup. | Extra O(1) caps ≈; fills ↓ at limits. | Lookup/arithmetic/writes ↑; impact depends on tier count and touched accounts, not necessarily all holders per fill. |
| HL / CEX parity | Partial cross-margin parity, incomplete isolated/leverage/tier parity. | Deliberately narrower product. | Moves toward [HL margining][HL-M] and [Kraken margining][K-M]; exact schedules and defaults require product decisions. |

**Recommendation:** A now; B only with R05's correct shared budget and R08's freshness policy. Test tier boundaries, position flips, partial closes, isolated transfer constraints and liquidation. Account-wide leverage exposure cannot be solved by independently validating each market.

### R14 — Block timestamps can remain equal to an old parent timestamp

**S/G; P1.** Parent-link validation requires timestamp monotonicity but permits equality. Pre-vote drift rejects excessive future time, not sufficiently old time. Honest production chooses current time versus parent time, so honest leadership usually advances it; an adversarial proposer can submit equal/old-but-monotonic time under the present rule. Sustained freeze depends on leadership/control, not merely one malicious transaction. Applying validator-local wall-clock rules during replay would create a different problem and must be avoided.

Evidence: [parent checks](../../crates/torus-consensus/src/app.rs#L1475), [future-only pre-vote drift](../../crates/torus-consensus/src/app.rs#L1579).

| Comparison | Today | A: bounded past drift at pre-vote only | B: protocol-defined higher-resolution/logical time rules |
|---|---|---|---|
| Example | Parent time remains T across successive allowed proposals; a 10s oracle expiry or timed session/unbond lock advances more slowly than real time. | Honest validators refuse proposals older than the agreed tolerance relative to synchronized clocks before voting; certified replay uses protocol rules only. | Agreed millisecond/slot progression and bounded real-time anchoring define freshness without a seconds-resolution minimum-block trap. |
| Positives / reasoning | Catchup deterministic; equal seconds accommodate fast blocks. | Small extension of the existing proposal-time safety check. | More explicit subsecond timing and timer semantics. |
| Negatives / regressions | Protocol time can lag real time under adverse leaders. | Clock skew/partitions can hurt liveness; needs operational clock discipline. | Header/signature migration and careful bound design; pure logical ticks alone do not track elapsed real time. |
| Blockspeed / matched/s / other | Fast blocks within a second work normally. | Healthy ≈; stale proposal retries may reduce consensus throughput. | Can preserve fast blocks with ms resolution; strict +1 second per block would regress subsecond blocks and is not recommended. |
| HL / CEX parity | Timed protections cannot promise real elapsed-time behavior under timestamp freeze. | Better expiry/freshness behavior; no assertion of identical HL consensus-time rules. | More precise timers; CEX uses its own server clocks rather than a replicated block clock. |

**Recommendation:** A with documented skew/tolerance and deterministic insertion/catchup behavior; B when versioned upgrades exist. Exercise stale, equal-second, future, clock-skew, pause/resume and long replay cases before tying funding/dead-man rules to these timestamps.

### R15 — Header roots, write-history commitments, and divergence fail-stop have different guarantees

**S/O; P1.** Production headers copy the parent's `state_root` rather than computing a fresh executed-state commitment for the proposed block. Native root machinery exists, but that does not make the copied header a current membership-proof anchor. The running state hash is a chained logical write-set history commitment, not a Merkle proof of current state. It covers economic/native and EVM CFs while intentionally excluding local/history data. Its activation is configurable, with no universal active default in the reviewed launch configuration; divergence fail-stop is explicitly off unless `TORUS_STATE_HASH_FAILSTOP=1`. Checkpoints occur every 100 heights.

Evidence: [header construction](../../crates/torus-consensus/src/app.rs#L5450), [running hash](../../crates/torus-state/src/running_hash.rs), [100-height interval](../../crates/torus-state/src/running_hash.rs#L187), [fail-stop default](../../crates/torus-consensus/src/state_hash.rs#L25), [activation-sensitive observation](../../crates/torus-consensus/src/state_hash.rs#L175).

| Comparison | Today | A: explicitly activate attestations and fail-stop operationally | B: bind an executed-state commitment with proofs/checkpoint verification |
|---|---|---|---|
| Example | A client sees repeated header roots; local hash mismatch can be logged while a node keeps participating if fail-stop is off. | Agree activation, require verified comparable checkpoints, and stop a demonstrably diverged validator. | A certified checkpoint identifies the executed height and commitment; a client verifies a balance/position proof against it. |
| Positives / reasoning | Efficient pipelined execution; diagnostic hash avoids full-tree maintenance. | Improves detection/containment using existing mechanisms. | External verifiability and stronger recovery anchors. |
| Negatives / regressions | Header field can imply stronger guarantees than supplied; unverified nodes cannot safely attest. | Simultaneous faults can stop enough stake; activation/recovery must be coordinated. | Tree/proof storage and computation; commitment timing requires a protocol design. |
| Blockspeed / matched/s / other | Fast by decoupling certification from executed roots. | Healthy hash cost mostly already present when active; operational halt reduces fault-time availability. | Extra hashing/writes may ↓ matched/s; waiting for full execution before every vote can seriously regress block interval. Delayed checkpoints can preserve pipelining. |
| HL / CEX parity | “Blockchain” does not establish proof parity by itself. | Better distributed consistency; HL public architecture does not establish identical root format. | Improves chain-verifiable state; CEX trust/account statements are not equivalent on-chain proofs. |

**Recommendation:** A with a tested mismatch/recovery runbook; define B around executed-height checkpoints before promising client proofs. Do not present the running hash as a membership proof or set a global fail-stop flag without an understood quorum/activation procedure. Measure hash cost, execution/durable lag, mismatch detection latency and recovery time.

### R16 — Proposer sizing limits do not fully bound consensus execution work

**S/C; P1.** Honest production limits native action count, expanded order count and payload bytes; a single native batch has an execution cap. These controls do not collectively form one versioned consensus rule bounding all native work accepted from an adversarial proposer. Matching depth, cancel-all, trigger cascades and liquidation work are not represented by a simple action count. Existing EVM declared-gas/header validation and network frame limits do bound parts of the attack surface: this is not a claim of unlimited EVM gas or unlimited wire bytes.

Evidence: [proposal sizing](../../crates/torus-mempool/src/rate_limit.rs#L95), [payload selection](../../crates/torus-consensus/src/app.rs#L5963), [proposal checks](../../crates/torus-consensus/src/app.rs), [native batch cap](../../crates/torus-types/src/lib.rs#L570), [batch paths](../../crates/torus-bridge/src/native_executor.rs#L5535). Current proposer constants include 400 native actions, 200,000 expanded orders and 12,000,000-byte payload targeting; per-batch execution cap is 1,024. Transport limits differ by message type and are not an economic-work budget.

| Comparison | Today | A: consensus-enforced aggregate deterministic resource ceilings | B: A plus metered variable matching/cancellation work |
|---|---|---|---|
| Example | A compact set of signed batches exceeds honest native sizing or sweeps unusually deep books; certification can outpace execution. Exploit cost/feasibility was not measured. | Every vote/insertion/sync path checks the same active-version aggregate count/bytes/gas/weight rules. | Matching stops or defers at a deterministic work budget with defined continuation and order semantics. |
| Positives / reasoning | Honest leaders can manage normal load without a complex gas model. | Makes block admissibility reproducible and limits admitted work. | Addresses deep-book/cascade work that count limits miss. |
| Negatives / regressions | Honest limits do not guarantee worst-case execution service time. | Fixed limits reject some useful batches; rollout must not reject already certified old-version blocks. | Continuations, IOC/atomic batch semantics and partial execution become difficult; cannot use nondeterministic wall-clock timeouts. |
| Blockspeed / matched/s / other | High normal matched/s can hide adversarial lag. | Worst-case blocktime/lag ↓; peak bulk throughput may ↓ if ceilings are conservative. | Predictable tails; additional metering cost and deferrals can ↓ fills/block. |
| HL / CEX parity | Public rate limits are admission policy, not proof of consensus work bounds. | Complements documented venue capacity controls. [HL limits][HL-RL] | CEX can use centralized time/queue controls; a chain must specify deterministic outcomes. |

**Recommendation:** A with explicit activation (R17), then profile worst-case work for B. Local execution-aware admission helps honest overload but cannot substitute for adversarial consensus validity rules. Measure execution backlog, deferred work, fill density, expensive cancel/stop cases, and accepted goodput under signed adversarial blocks.

### R17 — General protocol activation and storage migration are not formalized

**G/O; P1 before incompatible upgrades.** Running-hash activation exists; the repository does not provide a general application-version/feature-height framework covering every consensus-visible execution and schema change. Position v2 decoding rejects older layouts rather than migrating them automatically. Some historical changes assume fresh genesis. An ordinary rolling binary upgrade can therefore change accepted actions or persisted state semantics unless the release is carefully coordinated.

Evidence: [header/action types](../../crates/torus-types/src/lib.rs), [chain configuration and activation](../../crates/torus-genesis/src/lib.rs), [position serialization](../../crates/torus-core/src/position.rs#L93), [execution configuration](../../crates/torus-consensus/src/app.rs).

| Comparison | Today | A: feature heights and versioned deterministic migrations | B: explicit flag-day restart/checkpoint release policy |
|---|---|---|---|
| Example | Old/new binaries interpret a changed position layout or resource rule differently. | Both apply old semantics until H; incompatible nodes refuse service before H; migration has an agreed marker/hash and restart-safe completion. | Halt at an agreed checkpoint, verify every validator upgraded/migrated, then resume together; devnet may use an explicitly fresh genesis. |
| Positives / reasoning | Low framework cost for rapid development. | Safer rolling upgrades and auditable semantic transitions. | Simpler immediate operational containment. |
| Negatives / regressions | Informal coordination is a hidden safety dependency. | Migration complexity, storage growth and mixed-version test matrix. | Downtime, coordination burden, and weak protection against a missed operator or incompatible client. |
| Blockspeed / matched/s / other | Normal path fast. | Version check ≈; migration block/boot time ↑; avoids accidental forks. | Steady-state ≈; maintenance throughput zero and recovery time matters. |
| HL / CEX parity | No claim about HL's private upgrade machinery. | Supports durable chain lifecycle; compare availability promises, not unknown internals. | CEX also has maintenance windows, but does not require stake-quorum binary agreement. |

**Recommendation:** B as the documented current policy and A for sustained networks. Specify old/new schema readers, feature signaling, genesis/config hashes, rollback limits and client compatibility. Never “fix” consensus behavior by relying only on a node-local environment flag that differs across validators.

## Governance, bootstrap, EVM, and RPC

### R18 — Governance snapshots do not freeze all voting inputs

**S; P1.** Vote weight lookup falls back to live state if a voter has no snapshot row; this is intended for older proposals but does not distinguish an old proposal from a new voter absent at creation. Abstain/quorum paths still use current inputs, and finalization uses current total stake and governance parameters. A partial snapshot does not fix the denominator or eligibility.

Evidence: [snapshot fallback](../../crates/torus-economics/src/governance.rs#L1416), [abstain path](../../crates/torus-economics/src/governance.rs#L921), [live quorum and parameters](../../crates/torus-economics/src/governance.rs#L973).

| Comparison | Today | A: complete proposal snapshot, including denominator/parameters | B: historical stake checkpoints keyed by proposal height |
|---|---|---|---|
| Example | Illustratively, quorum is 33%: 200 votes fail against total 1,000 but pass if live total falls to 500 before finalization. A newly staked voter without a row can receive live weight. | Freeze eligible weight, total eligible supply, quorum, threshold and abstain semantics; absent voters in a new snapshot have zero weight. | Query all those values at the proposal's recorded checkpoint height rather than today's state. |
| Positives / reasoning | Backward compatibility with proposals predating snapshots. | Easy to explain and audit one proposal's exact electorate. | Avoids repeated eager snapshots and supports historical queries. |
| Negatives / regressions | Eligibility and outcome can change without new votes. | O(eligible voters) creation cost and storage; old proposals need explicit version treatment. | Checkpoint retention, stake mutation indexing and history pruning complexity. |
| Blockspeed / matched/s / other | Occasional governance work; no normal-fill benefit from the inconsistency. | Proposal block cost ↑; normal matching ≈. | Stake update writes ↑; proposal cost ↓ relative to full snapshots at scale. |
| HL / CEX parity | Governance mechanisms are not directly comparable to a centralized exchange's corporate decisions. | Improves Torus's own voting guarantees; no claimed HL electorate parity. | Same correctness goal with historical infrastructure. |

**Recommendation:** A with a snapshot-format/version marker and a matched numerator/denominator definition, then B if scale warrants it. Include delegated/permanent/self stake consistently; test new voters, redelegation, unbonding, abstain, parameter changes, and legacy proposals. Current payload+status atomicity is a separate already-improved property.

### R19 — Genesis accepts noncanonical and conflicting identity inputs

**S/C; P1.** Genesis parsing validates only part of the economic configuration before initialization. Raw JSON map keys are parsed later into addresses/storage slots; distinct strings can become the same canonical key, with `HashMap` iteration determining the last write. Validator account and consensus-key representations also need duplicate validation rather than relying on overwrites. `decode_hex` slices two characters without first rejecting odd lengths, so malformed input can panic. The shipped genesis files are not alleged to contain the illustrated collisions.

Evidence: [hex decoder](../../crates/torus-genesis/src/lib.rs#L300), [parse checks](../../crates/torus-genesis/src/lib.rs#L333), [initialization](../../crates/torus-genesis/src/lib.rs#L383), [EVM allocation/storage iteration](../../crates/torus-genesis/src/lib.rs#L423), [validator staking rows](../../crates/torus-genesis/src/lib.rs#L459), [consensus validator map](../../crates/torus-genesis/src/lib.rs#L645).

| Comparison | Today | A: normalize and reject duplicate/conflicting canonical keys before writes | B: deterministic canonical ordering and documented overwrite precedence |
|---|---|---|---|
| Example | Slot strings `0` and `0x0` parse to the same slot with different values; random map order can select different final rows. Odd hex `abc` can panic. | Both aliases are rejected as a duplicate before any state mutation; odd hex returns a normal validation error. | Sort inputs and apply an explicit winner so every process obtains the same result. |
| Positives / reasoning | Flexible human input. | Removes ambiguity and prevents conflicting stake/identity definitions; typed ordered representation can be hashed. | Eliminates iteration-dependent output while preserving permissive input formats. |
| Negatives / regressions | Potential different roots from the same ambiguous input; partial initialization on later error. | Some existing permissive files require correction; needs full prevalidation or atomic/journaled initialization. | Silent economic overrides remain hazardous even when deterministic. |
| Blockspeed / matched/s / other | Bootstrap-only path. | Steady-state ≈; startup validation time/memory ↑ modestly, recovery quality ↑. | Steady-state ≈; sorting startup cost ↑; still weak input safety. |
| HL / CEX parity | A chain's genesis is not analogous to a CEX order API. | Improves replicated bootstrap integrity; HL's internal genesis validation was not inspected. | Determinism improves, semantic safety less than A. |

**Recommendation:** A plus atomic initialization or a restart-safe initialization journal. Validate unique account addresses and consensus keys, allocation overlap, decimals, tick/lot, leverage, fee splits, stake/power ranges and chain config before writing. A multi-process randomized-order root test is required to prove the alias consequence, not performed here.

### R20 — Tracked devnet and testnet reuse chain ID 7778

**S/G; P2.** Both tracked devnet and weighted-base testnet genesis use 7778. Dynamic chain-ID derivation for signatures is already implemented; that fix cannot distinguish two networks configured with the same ID. Cross-network replay still depends on the same funded signer, compatible action domain/nonce window or EVM nonce, and transaction validity. No mainnet collision is claimed.

Evidence: [devnet genesis](../../devnet/genesis.json), [testnet genesis](../../testnet/genesis-weighted-base.json), [native domain implementation](../../crates/torus-bridge/src/validator.rs).

| Comparison | Today | A: unique registered IDs and wallet discovery/verification | B: A plus genesis/network identity in the native domain |
|---|---|---|---|
| Example | The same signed transfer can be valid on two 7778 networks when account state permits. | Testnet and devnet signatures differ because IDs differ; clients query and verify the intended network. | Even accidentally reused IDs do not share the native domain when genesis identity differs. |
| Positives / reasoning | Familiar local setup. | Standard EVM/wallet-compatible isolation, minimal ongoing cost. | Stronger native network binding. |
| Negatives / regressions | Accidental replay risk across similarly initialized environments. | Existing networks require a coordinated reset or domain migration; outstanding signatures/sessions change. | Signature/schema migration and client work; EVM replay protection still relies on chain ID. |
| Blockspeed / matched/s / other | No performance benefit worth relying on. | Runtime ≈; operational/client compatibility work. | Cached domain hash ≈; migration risk ↑. |
| HL / CEX parity | Separate environments should not accept each other's authorization accidentally. | Better chain environment separation; CEX usually separates credentials/endpoints rather than chain IDs. | Stronger native binding; no blanket CEX/HL security equivalence. |

**Recommendation:** A and an environment inventory before deploying new networks. Test signatures across two distinct IDs and two same-ID/different-genesis domains; do not change IDs on a running network as an uncoordinated configuration tweak.

### R21 — EVM selection removes transactions before their inclusion is durable

**S/C; P1.** `EvmPool::drain` removes selected entries and starts each sender at the lowest nonce present, without receiving the durable account nonce. Production selection drains before proposal success. A reinsertion API exists but no production caller was found in the reviewed path. An abandoned proposal or nonce-gap selection can therefore lose local visibility/availability even though no economic inclusion occurred. Peer copies and client retries can mitigate this; a universal permanent-loss claim would be too strong.

Evidence: [drain](../../crates/torus-mempool/src/evm_pool.rs#L288), [reinsertion API](../../crates/torus-mempool/src/lib.rs#L477), [proposal selection](../../crates/torus-consensus/src/app.rs#L6006).

| Comparison | Today | A: retain until durable inclusion with in-flight bookkeeping | B: proposal leases with explicit recovery/reinsertion |
|---|---|---|---|
| Example | Sender's durable nonce is 6, pool holds only 7: 7 can be selected, skipped by execution, and removed locally. A failed proposal can similarly drain a valid transaction. | Nonce 7 stays waiting for 6; selected transactions remain tracked until included, invalidated, replaced or explicitly expired. | Lease ready nonce-6 transactions to a proposal; on abandonment restore them exactly once. |
| Positives / reasoning | Cheap destructive selection. | Matches transaction lifecycle and supports accurate pending/in-flight nonce answers. | Reuses more of the existing drain structure. |
| Negatives / regressions | Misleading pending nonce and extra retries; wasted EVM/block capacity. | More memory, replacement rules and certified/in-flight state transitions. | Timeout races, duplicate proposals and crash recovery can still be difficult; gap readiness must be added too. |
| Blockspeed / matched/s / other | Wasted EVM gas selection can indirectly block native pipelining. | Selection/state checks ↑; useful EVM inclusion ↑; native mixed-work goodput may improve. | Bookkeeping ↑; retries ↓; stale leases can temporarily lower throughput. |
| HL / CEX parity | Ethereum transaction lifecycle, not HyperCore order lifecycle, is the relevant comparison. | Better pending/inclusion reliability; CEX order acceptance requires explicit lifecycle outcomes. | Can approach that reliability if leases are fully accounted, not just timed out optimistically. |

**Recommendation:** A with executed/durable nonce readiness and replacement deduplication; include pending selected transactions in nonce RPC semantics. Test failed proposals, leader changes, duplicates, gap fill, replacements, certified-but-unexecuted blocks and restart.

### R22 — Historical Ethereum state reads silently return current state

**S/G; P2.** Balance/code/storage/transaction-count RPCs resolve block arguments but use the current state database. By contrast, `eth_call` and `eth_estimateGas` explicitly reject unavailable historical state. That asymmetry can give a plausible incorrect historical answer. The estimator also uses a fixed default block-gas ceiling and overwrites a caller gas constraint during search, rather than deriving a supported ceiling from the requested and current block limits. This is an estimation-policy gap, not a claim that EVM consensus gas validation is absent.

Evidence: [balance/code/storage/nonce reads](../../crates/torus-rpc/src/eth.rs#L618), [historical call rejection](../../crates/torus-rpc/src/eth.rs#L967), [estimator](../../crates/torus-rpc/src/eth.rs#L1012). Ethereum JSON-RPC defines block-selected state queries. [API specification][ETH-RPC]

| Comparison | Today | A: reject unsupported state tags; bound estimator correctly | B: versioned/archive state with A's estimator bounds |
|---|---|---|---|
| Example | Balance was 10 at H and 20 today; querying H returns 20. A request constrained to 30k gas can be searched at a larger default limit. | H returns a clear “historical state unavailable”; estimator searches within supported current/request limits. | H returns 10 from a height-consistent historical view. |
| Positives / reasoning | Low storage footprint. | Honest, consistent API contract; small implementation surface. | Real Ethereum tooling/archive compatibility. |
| Negatives / regressions | Indexers and accounting can consume wrong history without an error. | Some clients that previously received a number now receive an error. | Storage/write amplification and pruning, historical native/EVM consistency. |
| Blockspeed / matched/s / other | Latest reads cheap. | Normal execution ≈; constrained estimate work can ↓; native fills unaffected directly. | Archive overhead ↑; use dedicated RPC/archive nodes to reduce validator contention. |
| HL / CEX parity | Ethereum compatibility is the direct standard; CEX historical ledger APIs are different. | Better correctness, partial capability parity. | Full historical state capability, not proof of exact HyperEVM configuration parity. |

**Recommendation:** A first, including unknown/future tags and explicit executed-height semantics. For B, pin a read snapshot/watermark rather than combine a current DB with an old header. Verify latest/pending behavior separately; pending is not an arbitrary historical archive.

### R23 — Balance and position RPCs do not expose true cross-margin risk

**S; P1 client correctness.** `get_balances` sums order margin plus isolated margin, omitting cross-position initial margin, and reports raw native cash availability. `get_position` returns zero liquidation price for cross positions; the isolated estimate is simplified. Execution's `AccountView` has better equity/free/transfer-required math, so this is not proof withdrawals bypass margin checks. It is a misleading risk presentation and integration gap.

Evidence: [position response](../../crates/torus-rpc/src/torus.rs#L1122), [balance response](../../crates/torus-rpc/src/torus.rs#L1160), [authoritative account arithmetic](../../crates/torus-core/src/margin.rs#L320).

| Comparison | Today | A: new authoritative account-risk endpoint | B: version/change current fields to risk semantics |
|---|---|---|---|
| Example | Cash 1,000; cross notional 10,000 at 20x, no PnL/orders: current fields can show available 1,000 and margin used 0. Actual IM=500, free=500, and 10%-notional transfer requirement leaves withdrawable 0. | Return cash, equity, IM, MM, free, withdrawable, freshness and executed height separately; use null/unsupported for an undefined liquidation estimate. | Existing “available” becomes a precisely defined free/withdrawable value and margin fields include cross IM; clients migrate by response version. |
| Positives / reasoning | Simple cash view. | Keeps existing cash semantics and adds one coherent risk source. | Familiar API can become correct without a separate endpoint. |
| Negatives / regressions | Bots/UI can overestimate capacity or interpret zero as no liquidation risk. | Clients must adopt new fields; cross liquidation price is portfolio/scenario-dependent. | Breaking semantic change; cached older clients can make worse decisions during migration. |
| Blockspeed / matched/s / other | Little query work but poor answers. | Query valuation cost ↑; consensus ≈ if reads isolated; confidence/safety ↑. | Similar query cost; integration migration is the main regression. |
| HL / CEX parity | Less complete risk surface. | Closer [HL account feeds][HL-WS] and [Kraken portfolio][K-P]; represent missing/stale estimates honestly. | Can reach similar parity with disciplined API versioning. |

**Recommendation:** A built from the same margin functions as execution and one consistent snapshot/height. Cross liquidation estimates must state which market is shocked and what other marks are held constant; a single zero or unqualified scalar is not portfolio risk.

### R24 — Public read RPCs can do expensive full scans before response caps help

**S/C; P1 when exposed on validators.** Open interest scans all native position rows and filters by market, using a synchronous iterator inside the async RPC path. Book readers can materialize large books. Connection/response limits and submit semaphores do not necessarily bound the CPU/DB work performed before a response is rejected. Exposure, request rates and actual latency were not tested.

Evidence: [OI scan](../../crates/torus-rpc/src/torus.rs#L2005), [book/RPC implementations](../../crates/torus-rpc/src/torus.rs), [server limits](../../crates/torus-rpc/src/lib.rs).

| Comparison | Today | A: atomic incremental aggregates and bounded indexed reads | B: cost-based read admission and dedicated RPC resources |
|---|---|---|---|
| Example | A small OI response repeatedly walks one million positions on the same DB used by execution. | OI query reads a maintained aggregate; book query has depth/page/work limits and an explicit snapshot. | Read semaphore/rate weights and blocking workers restrict concurrent scans; public RPC nodes absorb the load. |
| Positives / reasoning | Simple authoritative scan; no aggregate drift. | Query cost becomes proportional to output or O(1); holder indexes also help risk processing. | Limits interference quickly without changing financial state layout. |
| Negatives / regressions | Small requests can create large work and cache/IO contention. | Every position mutation, flip, liquidation/backstop/ADL must update/rebuild aggregates correctly. | Scans remain costly; admission can reject legitimate clients and operational cost increases. |
| Blockspeed / matched/s / other | Under read load, block p99 may ↑ and matched/s ↓; magnitude unknown. | Per-fill/index write cost ↑, read cost sharply ↓ at scale; net depends on workload. | Validator isolation improves tail predictability; standalone RPC capacity becomes a separate metric. |
| HL / CEX parity | Basic data available, scale behavior unproven. | Closer efficient market data behavior; not a claim of private venue database design. | Closer operational API protection; [HL limits][HL-RL] and separate market-data interfaces provide comparison. |

**Recommendation:** B immediately where exposed, A with shadow recomputation against raw rows. A response-byte cap alone is not a work limit. Measure query CPU/rows/IO, concurrency, rejection rates, execution/durable lag and p99 blocks under read traffic.

## Exchange operations, performance, and recovery

### R25 — Trade history is asynchronous; order lifecycle and streams are incomplete

**S/G; P1 durability, P2 surface.** Economic state and the applied marker are committed together, while trade/history materialization runs on a separate queue/writer. A crash or queue failure can leave derived history behind state; replay skips already-applied blocks unless explicit repair supplies the missing events. Live fill publication and durable history need separate watermarks, particularly around pipeline handoff. Streams currently expose new trades and user fills, not a complete book/order/account lifecycle. Native action failure/status codes exist; the gap is finer per-order lifecycle and resumable history, not “all failures look successful.”

Evidence: [post-execution fill/history handling](../../crates/torus-consensus/src/app.rs#L2836), [stream types](../../crates/torus-rpc/src/streams.rs#L24), [action statuses](../../crates/torus-consensus/src/action_results.rs), [trade APIs](../../crates/torus-rpc/src/torus.rs#L2105).

| Comparison | Today | A: durable fill/event outbox and resumable idempotent writer | B: A plus complete order/book/account lifecycle feeds |
|---|---|---|---|
| Example | State at H records a fill; process dies before the history writer stores it. Restart's applied marker skips H; UI history can omit a real settlement. | Atomic H commit includes replayable canonical events/outbox pointers; writer resumes from a durable cursor and safely deduplicates. | Clients also see received/rejected/resting/partial/cancelled/triggered events and account/book snapshots with sequence numbers. |
| Positives / reasoning | Keeps secondary indexing off the economic hot path. | Preserves async throughput while making eventual completeness repairable. | Reduces polling, ambiguities and missed maker-order state transitions. |
| Negatives / regressions | A successful state commit does not imply complete customer history. | More durable event bytes and retention/cursor management; body alone may not reconstruct fills without execution state. | Fanout/backpressure and schema complexity; account data permissions and resubscription matter. |
| Blockspeed / matched/s / other | Low synchronous index cost. | WAL/write bytes ↑; matched/s may ↓ if storage-bound, but async indexing remains possible. | RPC CPU/network ↑; polling ↓; validator impact depends on separation. |
| HL / CEX parity | Partial trade feed parity, incomplete lifecycle/recovery parity. | Better durable account audit evidence. | Closer [HL book/order/account feeds][HL-WS] and [Coinbase lifecycle][CB-M]. |

**Recommendation:** A with explicitly named consensus, executed, durable-state and history watermarks, then B. Store sufficient canonical event data; “just decode the block body again” does not recreate fills against historical book/account state. Validate queue saturation, abrupt crash, replay, duplicate events, writer restarts and snapshot resubscription.

### R26 — Modify cannot cross the spread

**S/G; P2, deliberate restriction.** Current modify validates ownership, tick/lot, margin and changes before mutation; those prior audit defects are improved. It rejects crossing modifications rather than matching and settling them. Same-price reductions preserve priority; repricing/increasing follows the implemented priority policy. A caller must otherwise cancel/place, which is not automatically an atomic replacement.

Evidence: [modify path](../../crates/torus-bridge/src/native_executor.rs#L9115), [crossing rejection](../../crates/torus-bridge/src/native_executor.rs#L9295).

| Comparison | Today | A: extend modify through atomic replace/match/settle | B: explicit atomic-replace action, preserve restrictive modify |
|---|---|---|---|
| Example | Bid 99, ask 100; modify bid to 101 rejects and keeps the original. | Validate replacement, then match at resting prices and settle fills, with specified ID/priority behavior. | AtomicReplace cancels 99 and submits 101 as one validated transition with a new order ID; original remains if prevalidation fails. |
| Positives / reasoning | Clear safety restriction and small settlement surface. | Familiar trader amend behavior and no cancel/place exposure gap. | Explicit replacement semantics; less surprise for existing modify clients. |
| Negatives / regressions | Limited amend functionality; separate cancel/place can fail between operations. | Margin reuse, ALO/IOC, partial fill, rollback and lifecycle rules must be exact. | Another action/schema and client migration; retaining old ID versus issuing new ID is a product choice. |
| Blockspeed / matched/s / other | Crossing attempts rejected cheaply. | Crossing modify adds normal matching work; latency ↓ versus two transactions; measured matched/s includes newly enabled fills. | Similar fill cost, extra event/action bookkeeping; client round trips ↓. |
| HL / CEX parity | HL's modify API accepts full order parameters; CEX amend rules vary. [HL exchange][HL-X] | Potentially closer expressive parity, but the docs alone do not prove every HL crossing amend outcome. | Clear atomic cancel-replace parity where supported; no universal CEX priority rule is assumed. |

**Recommendation:** B if preserving today's modify contract matters, otherwise A. Both must use the normal affordability/matching/settlement path and test rejection leaves the original intact. An unrelated sequence of cancel and place calls is not the same fix.

### R27 — Server-side trading protection and client-order identity need expansion

**G; P1 protection, P2 advanced features.** There is a stored optional 64-bit client order ID, but no complete unique/query/cancel-by-client-ID contract equivalent to HL's client ID surface. Current actions do not provide scheduled cancel-all/dead-man protection, signed action expiry equivalent to `expiresAfter`, full grouped position TP/SL lifecycle, or native TWAP. Working stop and reduce-only behavior should be credited; these missing capabilities do not mean no protective orders exist.

Evidence: [order/action types](../../crates/torus-types/src/lib.rs), [native handler surface](../../crates/torus-bridge/src/native_executor.rs), [RPC surface](../../crates/torus-rpc/src/torus.rs).

| Comparison | Today | A: dead-man/expiry/client-ID core protections | B: A plus grouped TP/SL and TWAP lifecycle |
|---|---|---|---|
| Example | Market-maker bot disconnects with open quotes; an offline client cannot send cancel-all. Duplicate retried client IDs are not a full idempotent lookup contract. | Bot renews a signed on-chain deadline; expiry cancels remaining quotes. Unique client IDs identify retries/query/cancel, with a declared scope/retention policy. | A position TP and SL are linked; filling/closing updates or cancels the other. TWAP slices follow a bounded deterministic schedule. |
| Positives / reasoning | Small feature surface; external bots can automate while connected. | Protects users precisely when the client is unavailable. | Better position management and execution strategies. |
| Negatives / regressions | Client watchdogs cannot protect during their own outage. | Timer queues, cancellation storms, expiry units and replay rules; R14 affects elapsed-time guarantees. | Many partial-fill/OCO/risk cases; scheduled work competes for block capacity. |
| Blockspeed / matched/s / other | No server timer cost. | Routine cost small with indexed due timers; outage/cancel block p99 ↑, risk exposure ↓. | Slices/trigger work ↑; raw matching capacity may ↓, client execution quality may ↑. |
| HL / CEX parity | Protection/API parity incomplete. | Closer HL `scheduleCancel`, expiry, cancelByCloid. [Exchange API][HL-X] Kraken documents server-side cancel-all-after. [Dead-man API][K-D] | Closer HL advanced order capabilities. [Orders][HL-T] CEX availability differs by product. |

**Recommendation:** A before B, especially dead-man and coherent client-ID semantics. Client order IDs need not be copied byte-for-byte from HL, but the compatibility difference (64-bit versus HL's 128-bit format) must be explicit. Test disconnect, renewal races, timestamp lag, mass expiry, filled/rejected retries and session-key cancellation authority.

### R28 — Main retains broad scan/flush costs and a serial barrier for EVM blocks

**P/S; P2.** Current native execution still contains broad cancel-all book scans, per-row cache materialization/flush work, and moving-mark revaluation costs. It also creates task work per block. Pipelined native execution requires an EVM-empty block, so any EVM transaction routes the block through the serial boundary. This can matter even if its gas consumption is small. None of these observations yields a current measured speedup.

Evidence: [cancel-all](../../crates/torus-bridge/src/native_executor.rs#L8947), [execution/cache phases](../../crates/torus-bridge/src/native_executor.rs), [pipeline eligibility](../../crates/torus-consensus/src/app.rs#L1796), [historical Phase 2 plan](../../docs/plans/item6-phase2-impl.md#L41).

**Historical profile, not bf2edda6 performance:** the plan uses Phase-2 base `35e69b3` and a crab r2 workload with about 39.2k fills/native block: engine/chain 190/246ms; cancel-all 26.7ms, of which book scan 18.2ms; cache flush 15.9ms nested inside 71.8ms settlement; flush worker 74.4ms on a separate thread. These rows overlap and must not be added. They are hypothesis generators; recent safety changes and workload differences invalidate automatic transfer of the numbers.

| Comparison | Today | A: sparse touched indexes and dirty-row/cache folding | B: incremental revaluation and persistent workers | C: dependency-aware EVM/native execution design |
|---|---|---|---|---|
| Example | Sender touches 2 of 300 markets; cancel-all probes broadly. One EVM tx blocks the native pipeline for that block. | Visit exact sender-market/order indexes, including pending stops; flush only touched state with byte-identical semantics. | Update cached position terms on changed marks/positions rather than rebuild unnecessary account views; reuse workers. | Permit overlap only where native/EVM read/write dependencies, nonce order and durable atomicity are proven safe. |
| Positives / reasoning | Simpler broad reference paths and ordered EVM/native boundary. | Removes avoidable work without weakening risk semantics. | Targets moving-mark and scheduling costs. | Can improve mixed-workload settlement and block pipeline utilization. |
| Negatives / regressions | Scaling work can grow with all books/accounts rather than touched state. | Stale indexes/caches can miss cancels or writes; every mutation/stop path must maintain them. | Cache invalidation/rounding errors can change consensus; worker complexity may save little. | Large design change; read precompiles/CoreWriter, locks, fees and epoch paths create cross-lane dependencies. |
| Blockspeed / matched/s / other | Current baseline unknown; old hotspots known only historically. | Likely blocktime ↓ / matched/s ↑ if scans/flush dominate; memory/index writes ↑. | Moving-mark cost may ↓; CPU efficiency ↑; no justified percentage. | Mixed-work p99/throughput may improve; pure-native workload can regress from coordination overhead. |
| HL / CEX parity | No numerical throughput parity established. | Moves scalability in the right direction without claiming 200k orders/sec equivalence. | Measure actual fills/sec and latency; CEX matching may not include replicated clearing. | Closer ability to sustain mixed execution, not evidence of HL's private scheduling design. |

**Recommendation:** A first with differential state/hash/fill comparison, then B based on a fresh profile; C is a separate protocol/execution project. Amdahl illustration only: saving 10ms from an actually limiting 190ms phase at fixed fill count would give 190/180−1 ≈5.6%; saving it from 246ms gives ≈4.2%. Neither number is a measured prediction for main, and a parallel flush bottleneck can erase the gain entirely.

### R29 — Snapshot restore checks self-consistency, not independent chain authenticity

**S/O; P1 recovery.** Snapshot verification compares recomputed contents with metadata supplied alongside the snapshot. That detects corruption relative to that metadata; it does not independently authenticate the advertised height, block hash, validator set or root against a trusted certified chain checkpoint. Restore is a local operator/CLI action, not an observed remote unauthenticated P2P import exploit. The reviewed repository does not show a complete production certified-checkpoint state-sync pipeline.

Evidence: [snapshot verification](../../crates/torus-state/src/snapshot.rs#L111), [operator restore entry](../../crates/torus-node/src/main.rs#L557).

| Comparison | Today | A: trusted manifest/checkpoint and complete restore runbook | B: certified checkpoint sync with proof/epoch-chain verification |
|---|---|---|---|
| Example | A modified snapshot plus matching modified metadata passes integrity checks, if an operator chooses to trust it. | Verify genesis identity, trusted checkpoint certificate/hash, executed/durable watermarks and snapshot manifest before import. | A node downloads data from untrusted peers but authenticates the commitment/validator transition chain and every chunk. |
| Positives / reasoning | Useful corruption detection and simple local backups. | Improves current operational restore safety without requiring a new network protocol. | Faster scalable recovery with less trust in the download source. |
| Negatives / regressions | Correct-looking metadata is not proof of a legitimate chain state. | Needs an independent trust anchor, rollback prevention and compatible consensus safety-vote handling. | Proof format, trust bootstrap, epoch transitions, DoS and partial-download recovery complexity. |
| Blockspeed / matched/s / other | Normal blocks ≈; recovery time depends on backup/replay. | Normal ≈; verification/restore time ↑ modestly, bad restores ↓. | Snapshot generation/network/storage ↑; recovery time may sharply ↓ versus full replay. |
| HL / CEX parity | No established recovery parity from a root comparison alone. | Better backup provenance; CEX trusted backup controls are an operational analogy. | Chain-authenticated recovery has no direct centralized exchange equivalent. |

**Recommendation:** A before B. Specify which DB rows and local vote/safety records may be restored together; a stale economic DB plus newer consensus safety state is not resolved by checking a sidecar root. Drill crash-consistent backup, partial restore, bad manifest, rollback and validator rejoin.

### R30 — Explorer can permanently accept a partially indexed block

**S; P2.** The indexer inserts the block row before all subordinate rows. Reindexing the same hash returns immediately. Body fetch errors are swallowed, trade-fetch errors can be skipped, and backfill logs an error then proceeds. Using the highest stored block as the next cursor can advance past a gap. Displayed native counts/epoch/set metadata also start as placeholders. This is derived data correctness, not a change to chain settlement.

Evidence: [early same-hash return and block insert](../../crates/torus-explorer/src/indexer.rs#L65), [body fetch/error handling](../../crates/torus-explorer/src/indexer.rs#L101), [backfill/cursor](../../crates/torus-explorer/src/indexer.rs#L35), [placeholder fields](../../crates/torus-explorer/src/indexer.rs#L290).

| Comparison | Today | A: atomic block indexing with contiguous completion cursor | B: explicit partial stages and gap-repair queue |
|---|---|---|---|
| Example | H's row stores, receipt fetch fails, H+1 succeeds. A retry sees H's same hash and skips it; latest height hides incomplete H. | Fetch required data, then atomically write block/tx/action/trade rows and mark H complete; completion frontier stops at the gap. | Mark H incomplete and retry its missing stages idempotently; higher heights may index but do not imply complete history. |
| Positives / reasoning | Simple best-effort forward scan. | Easy all-or-nothing local invariant and honest completed height. | Supports parallel ingestion and delayed history availability from R25. |
| Negatives / regressions | UI/audit history can silently omit real activity forever. | Large DB transactions and temporary backlog when one RPC dependency fails. | More stage/state machinery and deduplication; placeholders must be labeled or repaired. |
| Blockspeed / matched/s / other | Chain unaffected except RPC load. | Ingestion latency/storage batching changes; better accuracy, likely fewer repeated partial writes. | Higher parallel ingestion throughput possible; bounded retry traffic needed to avoid R24. |
| HL / CEX parity | An explorer is not the matching engine, but users rely on complete audit history. | Better observable history parity. | Better resumability; no claim about venues' private indexers. |

**Recommendation:** A for core block/tx/action rows and B for asynchronously available trade/candle stages. A highest-seen height is not a completed contiguous frontier. Test mid-block failures, restart, missing body, delayed trades, same-hash partial rows and placeholder replacement.

### R31 — In-block execution priority differs from network arrival and nonce order

**S/G; P2 fairness policy.** Native actions are sorted deterministically by category, address and canonical action-content hash, within the executor's phase rules. Selection order and wall-clock receipt order are not the final priority. Existing resting orders follow book insertion/price-time priority, but a new order's in-block insertion can be determined by address/hash rather than first arrival or signed nonce. Cancels and certain non-GTC work also have phase priority. This is a policy, not evidence the book fails its implemented comparator.

Evidence: [action sort key](../../crates/torus-bridge/src/native_executor.rs#L10308), [sort application](../../crates/torus-bridge/src/native_executor.rs#L10388), [book](../../crates/torus-core/src/order_book.rs).

| Comparison | Today | A: documented batch priority with harder-to-grind deterministic seed | B: consensus-preserved sequence plus explicit fairness evidence |
|---|---|---|---|
| Example | Alice submits a same-price order before Bob; Bob's lower address can insert first in the batch. Two Alice actions can be ordered by content hash rather than nonce. | Stable seeded priority reduces always-favorable addresses; within-account ordering uses an explicitly defined sequence if intended. | Preserve agreed sequencer/body sequence, potentially with verifiable receipt/commitment rules for fairness challenges. |
| Positives / reasoning | Validators agree without relying on inconsistent local arrival clocks. | Clear expectations; can reduce simple address grinding and define cancel/replace dependencies. | Closer observable first-in ordering when the sequencer's receive sequence is the contract. |
| Negatives / regressions | Arrival-based strategy assumptions can be wrong; address/hash grinding and censorship remain concerns. | Parent-derived seeds may be predictable/manipulable; changing sorting changes fills and needs activation. | Leader ordering/MEV/censorship trust; true distributed first-arrival fairness is hard and not solved by retaining body order alone. |
| Blockspeed / matched/s / other | Sorting cost; good deterministic batch execution. | Hash/order work ≈ or ↑ slightly; fill distribution changes; fairness metrics matter more than raw count. | Potentially less sorting but receipt/commit protocols ↑ latency and coordination. |
| HL / CEX parity | HL promises one consistent transaction order, not a globally first-arrival network clock. [Architecture][HL-O] | Can be a coherent chain policy with explicit differences. | Coinbase documents continuous price-time receipt ordering. [Matching][CB-M] Blockchain sequencing still has a different trust/latency boundary. |

**Recommendation:** Publish today's policy first. Do not use each validator's local arrival timestamp to determine execution. Any A/B change needs fairness/adversarial tests, within-account cancel/order examples, protocol activation and a clear statement of what is and is not guaranteed.

### R32 — Native TRS collateral is not a stablecoin perpetual settlement model

**S/G; P1 product/solvency model.** The reviewed Lockbox converts native EVM token units into native `FixedPoint` cash. Market base/quote strings are metadata, not a multi-asset settlement ledger. There is no complete ERC20 stablecoin collateral bridge or multi-collateral valuation/haircut engine in these paths. A UI calling a balance “USD” would not make TRS cash stable collateral; no frontend claim was independently inspected here. This is a denomination/product decision, not proof an existing promise was violated.

Evidence: [Lockbox and unit conversion](../../crates/torus-core/src/lockbox.rs#L257), [native balances/margin](../../crates/torus-core/src/margin.rs), [market metadata](../../crates/torus-genesis/src/lib.rs), [bridge settlement](../../crates/torus-bridge/src/native_executor.rs).

| Comparison | Today | A: explicit single stablecoin settlement/collateral | B: multi-collateral valuation with haircuts/concentration limits | C: retain TRS and explicitly price/denominate its risk |
|---|---|---|---|---|
| Example | 100 TRS has illustrative external value 200 at price 2, then 100 at price 1; counting 100 cash units is not counting a fixed USD value unless contracts are expressly TRS-denominated. | 100 admitted stablecoin units back 100 quote units under a defined bridge/issuer model. | TRS and stablecoin are valued in quote units with conservative haircuts, freshness and correlation limits. | Contracts, PnL and collateral explicitly use TRS units, or TRS-to-quote conversion is part of every risk view. |
| Positives / reasoning | Simple native cash/bridge accounting. | Familiar stable-quote perpetual accounting and clearer units. | Capital flexibility and richer risk controls. | Smaller product scope and honest denomination; no new stablecoin dependency if purely TRS-denominated. |
| Negatives / regressions | Wrong-way token/collateral risk if treated as stable quote; confusing price/PnL units. | Issuer/depeg, bridge custody, supply reconciliation and withdrawal risk. | Oracle/correlation/depeg liquidation complexity and substantial accounting expansion. | Token-volatility exposure remains; TRS-denominated contracts differ from expected USD perpetuals. |
| Blockspeed / matched/s / other | Fast single-cash arithmetic. | Normal single-asset arithmetic ≈; bridge transfers add work; exact units/conservation need migration. | Per-account valuation/state ↑; matched/s may ↓ materially for many assets without caching. | Pure native units ≈; adding quote conversion/mark dependencies costs more and ties to R08. |
| HL / CEX parity | Not stablecoin contract/collateral parity by metadata alone. | Closer HL crypto contract collateral convention (USDC collateral, documented price/quanto conventions). [Contracts][HL-C] | Closer multi-collateral derivatives products such as Kraken's, with different haircuts/rules. [Margining][K-M] | Deliberately different product; correctness can improve without claiming stablecoin parity. |

**Recommendation:** Choose and document A or C before representing balances/notional/PnL as USD. B is a later risk program. Test 18-decimal↔8-decimal dust/overflow, exact escrow/supply conservation, deposits/withdrawals, collateral price crashes and stale conversion prices. Existing Lockbox journaling and decimal fixes deserve credit but do not supply a stablecoin risk engine.

## Already addressed findings and remaining coverage limits

Older reports under `docs/audits/` and `docs/reports/hl-parity-2026-10-01/` cover different source revisions. Source review found the following improvements in current main; this is not a fresh test pass or a declaration that every variant is impossible.

| Area | Current source improvement | Residual boundary in this review |
|---|---|---|
| Modify ownership/affordability | Ownership and prevalidation precede mutation; tick/lot and margin checks are present. | Crossing replacement is still restricted (R26); test every failure leaves the original intact. |
| Stop fills | Triggered fills use settlement paths; working stop/reduce-only machinery exists. | Trigger price/scheduling and lifecycle parity remain (R09/R27). |
| Cross margin and withdrawal | `AccountView` accounts for cross IM/UPnL and protects withdrawals with transfer-required math. | Shared snapshot reuse, missing marks and RPC presentation remain (R05/R08/R23). |
| Market order affordability | Reserve/recheck and price-band/off-mark-loss controls exist. | Do not confuse these with global maker budget correctness (R05). |
| Lockbox | Decimal conversions and inner/outer transaction journaling/revert handling have been improved. | Collateral economic model and cold EVM persistence need separate review (R04/R32). |
| Sessions/native nonces | Trade-only session scope, millisecond session semantics, chain-derived domains and block-window replay guards exist. | Fault-to-default reads and shared environment IDs remain (R02/R20). |
| EVM fees | Previously described double-credit handling has been corrected in current accounting paths. | Type-driven revm semantics are a separate candidate (R03); native fees remain absent (R11). |
| Staking/unbonding | Self-stake accounting and `ClaimUnbonded` paths exist. | No comprehensive adversarial stake/epoch proof was performed; governance uses live inputs (R18). |
| Governance execution | Payload/status atomic handling and execution-failure status are improved; local storage faults are treated more strictly in that path. | Market no-op payloads and complete electorate snapshot remain (R12/R18). |
| Commit/replay | Economic overlay and applied marker use an atomic batch; epoch plans have a single execution writer. | An atomic batch is only safe if its failure stops advancement (R01). |
| Consensus voting/DA | Parent linkage, pre-vote body availability, lock/parent monotonicity and vote persistence protections are present. | Not a formal HotStuff proof; native work budgets, timestamp lower bounds and operational recovery remain (R14/R16/R29). |
| Exact position math | Exact cost basis and value-conservation stress test sources have been added, with four stress tests recently unignored. | Test sources were read, not run here; new funding/fees/tier math would require new conservation coverage. |
| Read precompiles | Recent follow-up metering fixes are in main. | Full dependency/DoS review of every precompile is outside this report; public RPC work is a separate boundary (R24). |

Additional capability boundaries: no complete parity review was performed for spot assets, vault/subaccount products, multisignature custody, external bridging, validator decentralization, frontend safety, institutional APIs, legal obligations, custody controls, or disaster recovery operations. Lack of review is not proof of absence. Four validators or one local benchmark cannot establish a network's decentralization or WAN latency. Monitor validator stake concentration, correlated host/disk failures and oracle signer independence before assigning deployment risk.

Historical plans and comments sometimes still mark already-addressed items as open, or claim flat-tier maker safety despite the later counterexample. Treat current code/tests as stronger evidence than an old checkbox. Maintain a source-SHA-bound findings inventory so subsequent changes can be reviewed without carrying obsolete severity forward.

### Behavior parity snapshot

“Partial” means working pieces with the limitations above; “gap” means missing product behavior; “unknown” means no comparable verification. This matrix is a navigation aid, not a numerical score.

| Capability | Torus at reviewed main | Hyperliquid public reference | Traditional CEX reference / boundary |
|---|---|---|---|
| Matching | Price/book FIFO plus deterministic in-block batch policy; R31 differences | One consistent on-chain ordering; no assumed global arrival guarantee [HL-O] | Coinbase continuous price-time receipt ordering [CB-M] |
| Cross margin | Implemented; R05/R08 shared-budget/freshness limitations | Cross and isolated, margin checked through trading/risk [HL-M] | Portfolio risk varies by derivatives product [K-M] |
| Isolated/user leverage/tiers | Partial type/core support, incomplete production product | Supported product surface [HL-M] | Product-specific; not all CEX offer the same modes |
| Mark and liquidation | Aggregated oracle, staged liquidation/backstop/ADL; R06–R09 limits | Robust mark, book/backstop and ADL [HL-R], [HL-L], [HL-A] | Mark-driven derivatives risk [K-L] |
| Funding | Gap | Hourly funding [HL-F] | Kraken derivatives accrued funding [K-P] |
| Fees | EVM fees; native trading-fee gap | Volume-related trading fees/rebates [HL-FE] | Commercial exchange fee schedules vary |
| Protection/amend | Stops/reduce-only; no crossing modify/dead-man/full advanced lifecycle | Rich order/modify/dead-man/client-ID surface [HL-T], [HL-X] | Kraken dead-man and Coinbase lifecycle [K-D], [CB-M] |
| Market/account data | Basic RPC/fills; risk fields and lifecycle/scale limitations | Book/order/account/funding subscriptions [HL-WS] | Mature market/account APIs vary by exchange |
| Historical state/proofs | Current-state RPC limits; copied header root and diagnostic write-history hash | No proof-format equivalence established here | CEX history/audits are trusted-service data, not chain membership proofs |
| Stable collateral | Native TRS cash path | Documented stablecoin collateral convention [HL-C] | Stablecoin/fiat/multi-collateral varies [K-M] |
| Throughput/latency | Current-main workload measurements unavailable in this review | Advertised orders/sec and co-located latency [HL-O] | Comparable verified capacity unavailable; unknown |

## Measurement and remediation sequence

The user authorized review and a documentation commit, not implementation. The following is a proposed future validation program; **none of it was executed here**. It turns performance predictions and source candidates into reviewable evidence.

### Suggested dependency order

1. Contain durable/read faults (R01/R02); reproduce EVM type/bytecode candidates (R03/R04). Do not build higher-level conclusions on uncertain base accounting.
2. Enforce shared risk and authoritative registry (R05/R12), then agree mark freshness/stop/risk-backlog/loss policy (R06–R09). Define collateral units and capital model (R32/R07).
3. Establish release activation and recovery procedures (R17/R29), time semantics (R14), deterministic work limits (R16), and commitments/fail-stop activation (R15). These changes need compatibility decisions before rollout.
4. Complete governance snapshots and bootstrap domains (R18–R20), EVM lifecycle and honest RPC answers (R21–R24), and durable history/protection (R25/R27/R30).
5. Add funding/fees/richer risk only on the corrected foundation (R10/R11/R13), with explicit conservation and capital policy. Finish amend/fairness behavior as chosen product semantics (R26/R31).
6. Optimize measured hotspots (R28) using byte-identical differential references. Do not remove margin/liquidation/fee/funding work to win a throughput cell.

An operational mitigation (for example isolating public RPC or documenting limited cross-only trading) can precede a larger implementation, but must not be described as resolving its underlying protocol finding.

### What to measure together

| Test/workload | Primary assertions | Blockspeed / matched/s | Other essential metrics |
|---|---|---|---|
| Healthy fixed workload, paired baseline/fix | Same state/fills when semantics should not change; intentional changes explained | Consensus interval, executed/durable p50/p95/p99; window matched/s including drain; accepted fills/block | CPU-seconds/million fills, bytes/WAL per fill, resident memory, cache misses, flush backlog |
| Cross-market maker concentration | Account-wide budget conservation; no safety gained solely by cancelling everything | Conflict-heavy versus disjoint-market matched/s, maker rejection/cancellation rates | Free/equity after every deterministic stage, exposure filled, useful liquidity, conflict-component sizes |
| Moving oracle, many accounts/positions | Risk valuation freshness; deterministic priority; debt obligations conserved | Worst block p99/lag and liquidation work share; healthy trading goodput | Oldest unhealthy/pending age, bad debt, vault equity, ADL work and queue age, stale-mark restrictions |
| Oracle outage/restart/time skew | All risk/withdraw/stop paths use the declared policy; replay deterministic | Consensus liveness versus rejected stale proposals; execution tails | Stop trigger-to-fill age, feed confidence/staleness, blocked withdrawals, loss/false liquidation counts |
| Native storage faults at each phase | No progress past unpersisted state; exact restart equivalence | Fault-time halts/retries are separately reported, not counted as goodput | Applied/executed/certified watermarks, state hash, stream/history completeness, RTO/RPO |
| Typed EVM and code restart | Actual fees/GASPRICE/receipt consistency; raw code and code hash preserved | EVM gas/sec, tx/sec, mixed native matched/s; serial-barrier duty cycle | Sender/proposer deltas, revert effects, code length/hash, skipped txs, pending/in-flight nonce accuracy |
| Signed worst-case native blocks | Versioned resource rules identical at vote/sync/replay; bounded deterministic work | Execution lag, maximum expensive block time, accepted/deferred work | Deep book fills, cancel-all and stop-cascade cost, bytes/action, queue memory, denial/retry rates |
| Funding/fees/tier boundaries | Exact long-short/treasury conservation and accrued risk even without user action | Normal/funding/mark-block latency, fee/funding cost per fill | Basis, tier changes, fee rounding/dust, reserve capitalization, unrealized/accrued liabilities |
| Public RPC load and history writer crash | Accurate snapshot answers; work admission; recoverable event/index gaps | Block p99 and matched/s under reads, versus isolated RPC | RPC p99, scan rows/CPU/IO, rejections, event lag, contiguous completed height, duplicate/missing events |
| Upgrade/snapshot/governance adversarial cases | Frozen electorate, canonical genesis, migration/rejoin authenticity | Upgrade downtime and catchup service rate; no artificial healthy speed claim | Certificate/config identity, root/hash equivalence, restored safety state, old/new client compatibility |

For benchmark comparisons, pin both source SHAs, binary/config, chain genesis, hardware and validator topology. Use separate fixed and moving marks, market counts and holder counts, sender/maker concentration, cancel intensity, order-size/depth distribution, EVM gas mix, oracle traffic and public read load. Interleave paired cells; report variability and repeated runs, not a best 60-second burst. Existing project measurement procedures and owner authorization remain applicable before running campaigns.

A benchmark with F fills over W seconds reports F/W fill records/sec. To compare with an orders/sec claim, separately count received actions, expanded orders, accepted orders, unique orders partially/fully filled and fill records; one order can create many fills. Latency must use the same endpoint: client receipt, consensus commit, executed result, durable acknowledgment, or history availability. WAN replication and risk/clearing responsibilities make a microbench-versus-CEX matching-engine comparison particularly misleading.

### Acceptance criteria for later fixes

- R01/R02: source defaults are replaced by explicit fault handling; relevant injected errors prove no missing/divergent state after restart. A log change alone does not resolve them.
- R03/R04: a failing pinned-dependency reproducer precedes the fix and passes afterward across restart. If reproduction falsifies the candidate consequence, update severity and retain the source evidence honestly.
- R05: compare one and multiple workers with concentrated makers; define the exact intended allocation/sequence and prove global account affordability, not merely fewer fills.
- Economic/product changes: quantify user-visible differences (fees, funding, margin, priority, timer/trigger source) and migration/backward compatibility. Matching counts can legitimately change.
- Performance-only changes: identical fills, economic rows and running hash across randomized/differential cases, followed by paired measured improvement in the actual bottleneck. Include memory and tail latency regressions.
- RPC/history/recovery changes: explicit unsupported states and repairable gaps are preferable to plausible but incorrect numbers. Verify completed watermarks and crash/retry behavior.

## External sources

Primary documentation retrieved 2026-10-08. Links support behavioral comparisons; they do not prove live performance, private architecture or implementation safety. Rates and product availability may change. No long excerpts are reproduced.

[HL-O]: https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview "HyperCore architecture and advertised performance"
[HL-M]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margining "Hyperliquid margining"
[HL-L]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations "Hyperliquid liquidations"
[HL-A]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/auto-deleveraging "Hyperliquid auto-deleveraging"
[HL-R]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/robust-price-indices "Hyperliquid robust price indices"
[HL-F]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/funding "Hyperliquid funding"
[HL-FE]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees "Hyperliquid fees"
[HL-T]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/order-types "Hyperliquid order types"
[HL-X]: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/exchange-endpoint "Hyperliquid exchange endpoint"
[HL-WS]: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions "Hyperliquid WebSocket subscriptions"
[HL-RL]: https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits "Hyperliquid API and user limits"
[HL-C]: https://hyperliquid.gitbook.io/hyperliquid-docs/trading/contract-specifications "Hyperliquid contract specifications"
[CB-M]: https://docs.cdp.coinbase.com/exchange/concepts/matching-engine "Coinbase Exchange matching engine"
[K-D]: https://docs.kraken.com/api/docs/futures-api/trading/cancel-all-orders-after "Kraken cancel-all-after"
[K-L]: https://support.kraken.com/hc/articles/4402283092244-liquidation-faq-derivatives "Kraken Derivatives liquidation FAQ"
[K-M]: https://support.kraken.com/in/articles/360022835651-leverage-margining-summary-derivatives "Kraken Derivatives leverage and margining"
[K-P]: https://support.kraken.com/gb/articles/4871775312276-portfolio-management-derivatives "Kraken Derivatives portfolio management"
[EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559 "EIP-1559"
[ETH-RPC]: https://ethereum.org/developers/docs/apis/json-rpc/ "Ethereum JSON-RPC"

Public source links above and repository evidence under each finding are the evidence catalog. No fixes were implemented, no runtime candidates were resolved, and no current-main speed or parity ratio was measured by this review.
