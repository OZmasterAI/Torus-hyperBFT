# Torus chain audit, third pass: network and client surfaces

Reviewed the checked-out source at `cea1254e34625e6b09c58f794de8793b5c12713c`, branch `merge/item6-sync2`, on 2026-10-04. This review covers network ingestion, native authorization and mempool admission, RPC, wallet, and standalone explorer code across the project; it is not restricted to the last commit's diff.

**Two additional source-supported availability findings and seven revalidated historical defects are recorded below.** N1 and N2 extend the earlier reports' unresolved network-ingress lead. R1–R7 were already identified in the September Astra reports and remain present in this checkout; they are not claimed as novel discoveries. Findings F01–F11 from the first two October reports are excluded from this report's count.

All findings are **static candidates**, pending failing production-code regression tests. Cargo/rustc are unavailable and were not installed. No Rust execution, signature verification, deployed-node experiment, network flood, remote test, or measured exploit occurred. Small Python/SQLite counterexample models were run locally; their limitations are stated below. Only this Markdown report was added by this audit worker. No production files, Git history, branches, or services were changed.

## Findings

| ID | Severity | Status | Finding | Prerequisite |
| --- | --- | --- | --- | --- |
| N1 | P1 high | Additional | Identified public peers can fill unbounded transaction-ingress queues before validation | Reachable P2P port and sustained ingress faster than validation |
| N2 | P1 high | Additional | Invalid, unfunded, and stale unsolicited native bodies reach permanent DA storage before admission | Reachable P2P port and many distinct decodable action bodies |
| R1 | P1 high | Historical, revalidated | Wallet `send --dry-run` still broadcasts a signed transfer | User runs Send with a funded key and the accepted global flag |
| R2 | P2 medium | Historical, revalidated | EVM selection removes nonce-gap transactions and has no production reinsertion for abandoned proposals | Queued future nonce, or a proposal that does not become canonical |
| R3 | P2 medium | Historical, revalidated | EVM admission accepts gas below intrinsic requirements | Valid signed, funded transaction with too little gas |
| R4 | P2 medium | Historical, revalidated | Explorer inserts completion evidence before indexing succeeds and cannot repair it | A receipt/body/indexing failure after block insertion |
| R5 | P2 medium | Historical, revalidated | Explorer interprets a signed envelope as an action variant | Any block containing native actions |
| R6 | P2 medium | Historical, revalidated | Mined dynamic-fee transaction `gasPrice` exposes the cap rather than effective price | Included type-2 transaction whose cap exceeds effective price |
| R7 | P2 medium | Historical, revalidated | Wallet governance input silently truncates u64 values to u32 before signing | Governance parameters above `u32::MAX` |

Severity is conditional on the listed trigger and impact. These observations do not establish an incident on a deployed network.

## N1: Public direct messages bypass bounded transaction admission

**Invariant.** Network-reachable untrusted work must consume bounded memory before expensive parsing, cryptography, storage, or mempool admission. A cap on jobs executing in a worker pool does not bound a separate producer queue.

**Evidence and reachability.** `crates/torus-network/src/bridge.rs:218–219` creates both native and EVM inbound channels with `mpsc::unbounded_channel`. The node installs their consumers at `crates/torus-node/src/main.rs:695` and `:749`.

An ordinary ed25519 libp2p peer can identify itself. `crates/torus-network/src/swarm.rs:1704–1710` deliberately registers nonvalidators in `peer_map`. The direct handler authenticates the claimed ed25519 identity at `:1481`; `verify_sender_key` at `:2863–2865` checks only that it agrees with the registered authenticated peer. This does not require validator membership, a valid native/EVM transaction signature, ownership of the claimed native sender, funding, or being the current leader.

After this identity check, pre-proposal/forward batches enqueue every decoded pair at `swarm.rs:1498–1504`; individually forwarded native actions enqueue at `:1541–1545`; forwarded EVM bytes enqueue at `:1551–1555`. The EVM marker parser at `:148–151` requires only the marker and a nonempty body. These branches have no direct-message transaction rate check before enqueue. The per-author consensus limiter at `:1326` belongs to the separate gossipsub consensus path.

The native consumer caps Rayon verification jobs at 1,024 (`main.rs:716`). Upon overload it performs ingestion inline (`:730–734`), slowing this consumer while the swarm's unbounded sender remains able to enqueue. The serial EVM consumer at `:752–756` likewise cannot impose backpressure through an unbounded channel. Invalid native actions can additionally perform the storage work in N2 before being rejected.

**Counterexample.** Establish an identified nonvalidator connection, send correctly framed direct native batches with unique invalid-signature bodies, and let their arrival rate exceed verification/storage throughput. Every accepted network frame adds queue entries before rejection. In the simple queue model, retained bytes grow as `(arrival rate − drain rate) × elapsed time × average queued size`; choosing one extra retained 1 MiB message per step for 1,000 steps yields 1,048,576,000 queued bytes. These rates are illustrative model inputs, not measurements or an exploit threshold.

**Existing mitigations and limits.** Direct frames are capped at 8 MiB (`crates/torus-network/src/caps.rs:23`); codecs enforce that per-frame cap (`codec.rs:74`, `:98–105`). Ban checks reject already-banned nonvalidators (`swarm.rs:1468–1476`). Request timeouts and libp2p request concurrency constrain in-flight network streams. None bounds the sum of already-delivered work retained in the downstream channels over time. Invalid transaction rejection occurs downstream and does not return a peer penalty to the swarm. Mempool capacity and funding rules apply after this queue.

**Impact.** Sustained public ingress can grow resident memory until the process is killed or other validator work is starved. Required rates, duration, and exact consensus impact need an isolated runtime load test. No signature forgery or validator key is required for reachability.

**Fix and regression.** Replace the handoff with byte-bounded queues and nonblocking overload rejection before retaining transaction bodies. Add separate per-peer budgets for transaction-forward and pre-proposal protocols; ensure validator consensus traffic does not share a starvation-prone queue. Preserve DA recovery for legitimately referenced native bodies when dropping unsolicited traffic. With a deliberately blocked consumer, drive the real direct handler from an identified nonvalidator and assert a fixed retained-byte limit, explicit drops, continued consensus traffic, and peer-budget enforcement on every marker branch.

## N2: Unauthenticated unsolicited bodies are persisted indefinitely

**Invariant.** Durable DA retention for proposal-referenced data must not give every public peer unlimited disk writes for unrelated invalid data.

**Evidence.** The publicly reachable N1 native paths call `Mempool::add_native_action_from_gossip`. At `crates/torus-mempool/src/lib.rs:574–579`, its first operation is `self.mirror_to_da(&action)`, before signature recovery (`:594`), session lookup (`:603`), claimed-sender comparison (`:613`), batch-size/nonce gates (`:661`, `:669–677`), or funding/pool admission (`:745`). An invalid signature, fake owner, obsolete nonce, or empty/unfunded action therefore does not prevent its body from being mirrored.

`mirror_to_da` clones the action into the pending mirror batch (`lib.rs:1115–1118`). At 128 actions or an age threshold it writes that batch (`:1122–1134`). `NativeDaStore::put_batch` hashes and serializes each body, writes each key into `CF_NATIVE_PENDING`, and commits the RocksDB batch (`crates/torus-state/src/native_da.rs:95–112`). Native action hashes commit to the nonce and signature (`crates/torus-types/src/lib.rs:1047–1070`), so distinct decoded envelopes need not collide with one another to create distinct retained rows.

`NativeDaStore::remove` exists at `native_da.rs:249`, but source caller search found test calls (`native_da.rs:471` and `crates/torus-state/tests/native_da_tests.rs:113`, `:121`), not a production eviction/garbage-collection path. `remove_committed_native` at `mempool/lib.rs:1297–1311` prunes the in-memory selection pool. The regression at `:1700–1738` explicitly requires committed DA bodies to survive that pruning. No quota or age policy protects unsolicited rejected bodies in the reviewed path. If a DA write fails, `lib.rs:1136–1138` requeues the failed batch, so a disk-full incident can also retain retry work in memory.

**Counterexample.** Send 128 distinct, well-encoded native envelopes with invalid EIP-712 signatures over the authenticated peer connection. Vary their nonces or signature bytes to change their body hashes. The mirror batch becomes due and persists before each action's eventual rejection. There need be zero admitted transactions and no committed proposal referencing those bodies. Repeat with further distinct envelopes. The abstract unique-key model retained 128 rows while accepted-action count stayed zero; actual Rust hashing and RocksDB persistence were not exercised.

**Impact and qualification.** An unfunded nonvalidator can force permanent storage growth and RocksDB write load without paying native fees. This remains a disk-exhaustion path even if N1's queues are fixed: a slower sustained stream can stay below bounded memory throughput while accumulating durable rows. Coalescing limits write-call frequency, not total retained disk bytes. The duration to exhaust a given disk is unmeasured; this report does not claim a specific amplification factor.

**Fix and regression.** Separate unsolicited admission from consensus-referenced DA custody. Authenticate/admit ordinary forwarded actions before durable retention, or retain them in a quota-limited transient store. Permit invalid/stale bodies needed by a verified proposal manifest through a separate reference-aware custody path. Do not simply remove required historical bodies or make reconstruction depend on current funding/nonce validity. Drive the real gossip/direct ingest with unique invalid, stale, mismatched-sender, and unfunded envelopes and assert no unbounded permanent-row growth. Include a referenced-invalid-body control proving that deterministic execution can still reconstruct and skip an invalid action.

## R1: Send ignores the accepted dry-run flag

Historical source: Astra round 2, assignment 22.

`tools/wallet/src/main.rs:47–49` defines `dry_run` as a global option promising to build/sign without submission. `Command::Send` dispatches directly to `cmd_send` at `:378`. That function builds the signed EIP-1559 bytes and unconditionally calls `rpc.send_raw_transaction` at `tools/wallet/src/commands/transfer.rs:35–37`; it never examines `cli.dry_run`. The RPC wrapper issues `eth_sendRawTransaction` at `tools/wallet/src/rpc.rs:88–90`. Native submission has a separate dry-run path, which does not protect EVM Send.

**Counterexample/invariant.** `wallet --dry-run send --to <address> --value 1` is accepted by the parser yet submits the transfer when the RPC and key are usable. A no-submit option must prevent any broadcast. This can spend real funds contrary to the selected mode; no exploit or actual transfer was performed.

**Fix/regression.** Handle dry-run in the EVM command before `send_raw_transaction`, producing the raw signed transaction and local hash. Use a mock RPC recording method names: Send dry-run must make zero `eth_sendRawTransaction` calls, normal Send exactly one. Querying nonce/chain/fees for constructing a dry-run transaction can remain explicit behavior.

## R2: EVM drain breaks transaction lifecycle across nonce gaps and lost proposals

Historical source: Astra round 2, assignment 1.

Admission allows future nonces through a gap of 64 (`crates/torus-mempool/src/validate.rs:102–121`). `EvmPool::drain` seeds each sender from the smallest pooled nonce (`crates/torus-mempool/src/evm_pool.rs:288–297`), without the executed or proposal-parent state nonce. It removes all selected entries at `:369–370`. `Mempool::drain_evm` invokes this destructive drain at `lib.rs:479–480`; the live proposer places those bytes in its block at `crates/torus-consensus/src/app.rs:5215`.

**Counterexample.** State nonce is zero and only a funded, valid transaction with nonce one is pooled. Admission accepts it, selection includes/removes it, and execution cannot accept nonce one while nonce zero is missing. In the local predicate model the pool is empty immediately after selection. Submitting nonce zero later does not resurrect the removed transaction.

The same ownership design loses an otherwise executable transaction if a locally produced proposal is abandoned. `pending_proposals` can be overwritten/retained away at `app.rs:5243–5244`. `reinsert_evm` exists at `mempool/lib.rs:451`, but a whole-source call search found only its unit test at `:2924`, not a live abandonment/recovery caller. `pending_nonce` at `:1338–1343` sees executed state plus remaining pool entries, so drained in-flight transactions are invisible to clients. This increases nonce collisions before execution advances.

**Fix/regression.** Select from the executable nonce on the proposal's parent branch, retaining future-gap entries. Track pooled/in-flight/committed ownership and requeue transactions when their proposal is abandoned. Test nonce one arriving before zero, two proposals before execution, and a valid transaction in a lost-view proposal. Each transaction should remain discoverable until final execution or an explicit terminal rejection. RPC acceptance alone does not promise successful execution, but internal selection must not silently discard recoverable queued work.

## R3: Intrinsic gas is not enforced by EVM admission

Historical source: Astra round 2, assignment 3.

`validate_evm_tx` checks gas only against an upper block limit (`crates/torus-mempool/src/validate.rs:72–79`), then checks fee, sender, nonce, and balance and returns an admitted entry (`:85–143`). It has no lower intrinsic-gas calculation for transfer, calldata, access-list, or creation requirements. A signed funded transfer with gas limit one and valid chain/nonce passes those predicates. Zero-value with gas one still has a minimal max-fee balance requirement; this is not free execution.

`EvmPool::drain` charges only the advertised gas limit to selection (`evm_pool.rs:340`), so such entries can consume less proposal budget than their eventual validation cost. Revm rejects them at execution; Torus's permissive catch-up/live execution mode (`crates/torus-bridge/src/validator.rs:102–108`) skips transaction-validation errors before constructing receipts (`crates/torus-evm/src/executor.rs:305–314`). The transactions do not execute or pay gas. This is observable as an admitted hash that disappears from the executable pool and never becomes a normal mined receipt. An attacker could supply many distinct nonces/accounts, subject to existing pool and admission caps; no quantitative throughput attack is claimed.

**Fix/regression.** Use the pinned fork's actual intrinsic-gas calculator at admission and defensively before block selection, including calldata, access lists, and contract creation. Test gas just below/at each boundary through `Mempool::add_evm_tx`, not a duplicate hand-written predicate. Proposal-side validation should separately bound the work of rejected EVM payloads.

## R4: Explorer treats a partial block row as completed indexing

Historical source: Astra round 1, assignment 6. The current executed-head improvements address ordinary execution lag, but do not fix partial-write recovery.

`Indexer::index_block` inserts the block row at `crates/torus-explorer/src/indexer.rs:84–88`, before requesting the native body (`:93`), receipts (`:108`), transaction/log rows (`:111–121`), or native rows (`:137–139`). A receipt or SQLite failure returns an error after the row is durable. Retrying the same block checks the existing hash and immediately returns success at `:71–73`; it does not retry incomplete descendants. `backfill` logs errors and continues to later heights (`:36–39`), and later successful blocks advance `last_indexed_height` at `:157–159`. The cursor can therefore skip the failed height permanently.

Body RPC errors are also swallowed with `.await.ok().flatten()` (`:93`). A transient body failure can mark a block successfully indexed with no native rows. `fill_unknown_status` queries only already-existing unknown-status native rows; it is not a repair for absent native actions. Trade RPC errors at `:146–148` similarly permit cursor advance without candle updates.

**Counterexample/invariant.** Insert block ten, then simulate a receipt RPC failure before its first transaction row. Retry block ten after RPC recovers: same hash produces an early return, leaving no transaction rows. The local SQLite ordering model reproduced exactly this control flow with one persisted block row and zero tx rows. It did not execute the Rust indexer. A completion cursor must attest to completed indexing, not merely header insertion.

**Fix/regression.** Fetch required data first, then transactionally persist all indexed rows and the completion cursor. Track optional/retryable body/trade phases explicitly instead of silently completing them. Permit same-hash repair until a stored completion marker is present. Inject a one-shot receipt/body/SQLite failure, retry, restart, and assert complete transactions, native actions, logs, and candles with no duplicates and no skipped height.

## R5: Native envelope parsing loses action identity and fields

Historical source: Astra round 1, assignment 6.

Node RPC serializes each `SignedNativeAction` intact at `crates/torus-rpc/src/torus.rs:1589–1593`: the JSON object contains `action`, `nonce`, and `signature`. Explorer `parse_native_action_row` instead takes the first object key as the enum variant at `crates/torus-explorer/src/indexer.rs:367–369`. Even if the first key is `action`, it stores `action_type="action"`, not the inner `PlaceOrder`/`Delegate`/etc. Variant-specific extractors at `:385–422` consequently return absent metadata. Other map-order choices likewise cannot yield the inner enum tag. `parse_block_row` also hardcodes `native_action_count: 0` at `:290` despite the body count returned by the node (`torus.rs:1627`).

**Counterexample/invariant.** A real signed Delegate envelope must produce action type Delegate, its validator and amount, and a nonzero block action count. The parser instead chooses an envelope key and loses those fields. Raw payload text remains stored, so this does not itself alter chain execution; it makes the explorer's indexed projections and counts incorrect.

**Fix/regression.** Decode a typed signed envelope, then inspect its inner NativeAction. Populate block metadata from the node's typed body. Use actual node serialization fixtures for every action variant, including unit variants and PlaceOrderBatch, with nonce/signature metadata preserved separately where needed.

## R6: Mined type-2 gasPrice remains the signed fee cap

Historical source: Astra round 2, assignment 23. The projection and execution-type defects require coordinated correction; the consolidated audit groups this projection under its typed-transaction finding rather than counting it as another independent overcharge.

`build_rpc_tx` sets type-2 `gas_price=None` and `max_fee=tx.max_fee_per_gas` at `crates/torus-rpc/src/eth.rs:370–382`. Its display fallback at `:416–418` then emits the maximum fee. The builder has neither the block base fee nor the actual receipt effective price as an argument (`:325–331`). It fills a mined block hash/number/index, so the value is presented as a mined transaction's `gasPrice`, not a pending cap. The receipt path separately exposes `receipt.effective_gas_price` at `:854`.

**Counterexample/invariant.** Base fee one, priority cap two, and maximum fee 100 imply effective price three under Ethereum type-2 semantics; the transaction object displays 100 while the receipt calculation advertises three. The parent audit separately verified that `crates/torus-bridge/src/decode.rs` leaves the execution `TxEnv.tx_type` at the pinned dependency's legacy default, causing the executor to charge the cap in this checkout. Therefore R6 establishes disagreement with type-2 semantics and receipt projection, not independent evidence that the transaction object's cap overstates the current actual debit. Correcting execution typing alone would leave this RPC projection inconsistent.

**Fix/regression.** Supply the mined effective price from the receipt or derive it consistently from the included header, keeping `maxFeePerGas` as its separate signed field. Test transaction-by-hash and full block projections against their receipts for capped and uncapped type-2 fees, plus legacy and type-1 controls.

## R7: Governance CLI signs silently wrapped parameters

Historical source: Astra round 2, assignment 21.

`tools/wallet/src/commands/governance.rs:67–70` accepts numeric JSON parameters as u64. List-market uses unchecked `as u32` conversions at `:83–84`; update-market-params repeats them at `:94–96`. This constructs and signs a different value from the input whenever it exceeds `u32::MAX`. For example, `max_leverage=4294967298` becomes two. On-chain checks may accept the wrapped two, but cannot recover the user's original intended value.

**Fix/regression.** Use checked conversion with a field-specific error before signing or submission. Test `u32::MAX`, `u32::MAX+1`, and `2^32+2`, and assert rejected inputs cause zero signing/submission calls. The separate malformed-sign FixedPoint parser issue from the historical report appears addressed: `torus-types/src/lib.rs:193–198` now insists on decimal digits after its single leading-minus handling. That old issue is not re-counted here.

## Additional revalidation and leads not promoted

- Wallet valid `result:null` still becomes an error: `tools/wallet/src/rpc.rs:4–7` models result as `Option<Value>` and `:49` rejects None. Serde represents explicit null as None, preventing query commands' intended not-found handling. This is a historical P3 client error-reporting defect, separate from R1.
- Ethereum's committed-versus-executed head issue has meaningful new guards: `crates/torus-rpc/src/eth.rs:176` uses the applied frontier and `:184` gates higher headers. The explorer now queries that executed head (`indexer.rs:47–56`). This review does not reassert the old ordinary-lag omission scenario as unchanged; R4 is specifically a transient-error and partial-write recovery problem.
- RPC call/estimate still operate on the live `StateDb` without pinning a read snapshot across header selection and execution/probes (`eth.rs:965–1065`). This revalidates the historical absence of state binding, but a concrete concurrent-commit mixed-result reproduction remains necessary before claiming a particular wrong result. The initial and binary-search probes' nonce defaults differ; simulation disables nonce checks (`torus-evm/src/executor.rs:195–197`), so this was not promoted as an independent nonce defect.
- Native domain is fixed to 7778, but ordinary admission explicitly rejects any other configured chain ID (`torus-types/src/eip712.rs:931`, `:959`). Therefore an RPC cross-chain replay bypass is not established. Startup acceptance of alternate config values is an integration/configuration lead, not a new replay finding here.
- Native session authorization is batch-resolved from block-start state before signatures are stripped into sender/action pairs (`torus-consensus/src/app.rs:1998`, `:2105`). Same-block CreateSession/RevokeSession behavior deserves a specified activation policy and a test; absent that policy, this review does not claim that block-start authorization itself violates the protocol. Previously reported millisecond/second expiry and storage-error swallowing are F03/F04 and remain excluded.
- Full sessions deliberately authorize some governance/validator actions according to `SessionScope::Full` (`torus-types/src/lib.rs:288–299`). That broad documented scope was not recast as an unauthorized privilege escalation. Mandatory EIP-712 actions are independently checked in batch verification (`eip712.rs:1223`).
- Native signature-committing hashes, locally derived verified-sender cache provenance, session signature verification, and claimed-sender comparison are present. These mitigate signature substitution and sender spoofing after ingress; they do not undo N1/N2's pre-validation resource costs.

## Validation scope

The source review traversed the direct/gossip authentication and rate boundaries, codec cap ladder, inbound consumers, DA mirror persistence and pruning, EVM admission/drain ownership, native domain/session/cache rules, RPC submit controls and projection, executed-frontier call reads, wallet signing/submission/numeric parsing, and explorer ingestion/SQLite completion behavior. It also read the first two October reports and both historical Astra reports before counting findings.

The locally executed models observed: unbounded producer/consumer imbalance under chosen rates; unique rejected-body retention in an abstract DA map; a partial SQLite block row defeating same-hash repair; destructive selection of nonce one while state nonce is zero; signed-envelope projection choosing `action`; and fee cap 100 versus effective price three. These models explain counterexamples only. They do not execute Rust, establish actual attack throughput, form certificates, verify signatures, or certify deployed behavior. Production regression tests and isolated adversarial load/failure testing remain outstanding.
