# Pass 11 — ordinary first-party client workflows

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting at
documentation commit `0c967ca`; production source remains
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`. Scope: shipped wallet request
builders and response readers, explorer RPC ingestion/projection, units,
integer widths, defaults, and supported native-action command dispatch.
No standalone first-party JavaScript/TypeScript SDK or trading frontend was
found in the inspected source inventory; planning documents are not treated
as shipped clients.

The [audit policy](README.md), September [round one](astra-round1-2026-09-24.md)
and [round two](astra-round2-2026-09-24.md), October consolidated reports
through pass ten, and the detailed
[pass-seven client review](chain-cea1254-pass7-sol-rpc-2026-10-04.md),
[pass-eight integration review](chain-d52a33f-pass8-sol-integration-2026-10-04.md),
[pass-nine transaction review](chain-d52a33f-pass9-sol-transactions-2026-10-04.md)
and [history review](chain-d52a33f-pass9-sol-history-2026-10-04.md) were used
for deduplication. F01–F43 and September supplements retain their earlier
provenance. No applicable `AGENTS.md` was found in the repository or its
ancestor directories.

**Result: one additional source-supported client candidate, C01, plus an
ordinary direct-query qualification of F32.** IDs here are temporary; parent
review owns final numbering and Torus persistence. Cargo/rustc remain
unavailable under the task constraints. Rust tests were read, not run; no
toolchain installation, network request, live-chain action, key operation,
adversarial experiment, production edit, or Git mutation occurred.

## C01 — P2: sequential wallet sends reuse a nonce while an earlier send is queued

The supported [Send command](../../tools/wallet/src/main.rs#L101) reaches
[cmd_send](../../tools/wallet/src/main.rs#L378), which
[fetches the account nonce and gas price](../../tools/wallet/src/commands/transfer.rs#L32)
before signing and broadcasting. Its
[get_transaction_count request](../../tools/wallet/src/rpc.rs#L72) hardcodes
`"latest"`. The node's
[latest branch](../../crates/torus-rpc/src/eth.rs#L670) reads the executed
account nonce; its separate `"pending"` branch already accounts for the
consecutive transactions still in the pool.

An ordinary, bounded failure sequence follows:

1. Use a funded account with executed nonce N, sufficient balance for both
   plain transfers and gas, free pool capacity, and a stable positive quoted
   fee such as 1,000,000,000 wei/gas. Send to recipient A successfully.
2. Keep that first transaction in the receiving node's pool, before proposal
   draining or execution. Run a second Send with a different recipient or
   value through the same endpoint. The commands can be sequential: the
   first returns after admission rather than waiting for inclusion.
3. The second latest query still returns N. The wallet
   [sets both nonce and fee cap directly from those queries](../../tools/wallet/src/sign.rs#L94)
   and makes a distinct signed transaction with nonce N and the same fee cap.
4. Admission reaches the pool's
   [same-sender/nonce replacement branch](../../crates/torus-mempool/src/evm_pool.rs#L139).
   The [default replacement bump is 10%](../../crates/torus-mempool/src/lib.rs#L107),
   so the second fee cap of 1,000,000,000 is below the required
   1,100,000,000. The node returns `ReplacementUnderpriced`; the second intended
   transfer is not admitted.

The second request is a legitimate new transfer, but the wallet constructs it
as a competing replacement. The CLI offers
[only recipient and value for Send](../../tools/wallet/src/main.rs#L102),
with no nonce override or included-transaction wait. Waiting until the first
executes avoids this fixture; that is a timing workaround rather than queued
transfer support. The source does not establish a failed transfer after
ordinary confirmed sends, accidental loss of funds, or a successful silent
replacement at a stable fee.

**Falsification and distinctness.** If the first send has already executed,
latest supplies the next nonce and this sequence does not fail. Identical
recipient/value/nonce/fee sends can hit the duplicate-hash check instead;
the retained fixture changes the recipient or value. If fees change enough,
replacement behavior can differ. The retained fixture fixes a positive fee
and free capacity to isolate nonce selection. It also explicitly keeps the
first transaction **pooled**: the historical R2 in-flight nonce gap after
destructive drain is unnecessary. The pool
[documents pending nonce as the way to avoid these queued collisions](../../crates/torus-mempool/src/lib.rs#L1333),
and [walks consecutive pooled nonces](../../crates/torus-mempool/src/evm_pool.rs#L227).
Thus the wallet fails even in the case where the existing server-side
pending mechanism has the correct next nonce.

**Evidence and gap.** Whole-wallet source search found no request-level Send
nonce regression. The
[wallet EIP-1559 builder test](../../tools/wallet/src/sign.rs#L279) supplies
nonce zero explicitly and checks the encoded type byte and length; it does
not invoke the RPC nonce selection. Existing
[replacement_by_fee](../../crates/torus-mempool/src/lib.rs#L2566) and
[pending_nonce_consecutive](../../crates/torus-mempool/src/lib.rs#L3201)
tests exercise the relevant server mechanisms, but do not exercise wallet
Send. All were read only. No failing production regression or runtime timing
measurement is claimed.

**Regression and repair direction.** Use a controlled local RPC/mempool
fixture with proposal draining held back, not a live chain. Invoke two Send
workflows for distinct transfers from one funded test account and inspect
the decoded submitted envelopes: they should have nonces N and N+1, and
both should remain admitted. Assert the nonce selector is `"pending"` and
that a completed first transfer also yields the next nonce. Using pending
addresses this pooled fixture; it does not by itself repair R2's missing
in-flight accounting or atomically allocate nonces for concurrent wallet
processes. Those are separate lifecycle/coordination requirements and should
not be represented as fixed by a selector change.

## F32 qualification — direct market queries already expose the radix trap

Pass seven documented F32's incompatible market-list response parsing and
warned that repairing `Positions` with decimal `mid.to_string()` would query
the wrong market for IDs at least ten. The direct commands already have a
related ordinary inconsistency: [Orderbook and Position accept an untyped
market string](../../tools/wallet/src/main.rs#L132), which the wallet
[forwards unchanged](../../tools/wallet/src/rpc.rs#L147). The node
[parses market selectors in radix 16 even without a prefix](../../crates/torus-rpc/src/types.rs#L83)
in both [book](../../crates/torus-rpc/src/torus.rs#L914) and
[position](../../crates/torus-rpc/src/torus.rs#L945) queries.

Consequently `place-order --market 10` uses the numeric decimal market 10 in
the [signed action](../../tools/wallet/src/commands/trading.rs#L76), while
`orderbook 10` reads numeric market 16. This can display an empty or unrelated
book while labeling it with the user's string `10`; if market 16 has another
book, the wrong data is a successful response. `position <address> 10` has the
same selector inconsistency, independently of the valid-null result issue.
An explicit wire selector `0xa` correctly selects market 10; IDs 0–9 do not
expose the decimal/hex difference. This is retained as an **F32 client
contract extension, not a newly counted defect**.

Extend the F32 response-contract regression with direct book/position queries
against distinct state for IDs 10 and 16. Decide and document CLI decimal/hex
syntax, then parse locally and encode a canonical hexadecimal wire selector.
Preserve returned node wire identifiers when enumerating market pages. No
direct-command request regression was found in the inspected wallet tests.

## Units, supported native actions, and rejected leads

| Checked path | Disposition and regression boundary |
| --- | --- |
| Native collateral versus EVM/staking units | [Transfer builders](../../tools/wallet/src/commands/transfer.rs#L60) use [eight-decimal native units](../../tools/wallet/src/parse.rs#L52); [staking builders](../../tools/wallet/src/commands/staking.rs#L11) use wei; [lockbox conversion](../../crates/torus-core/src/lockbox.rs#L255) scales a native raw unit by 10^10. The historical September wallet/lockbox mismatch is not reintroduced by the checked builders. Existing [parser assertions](../../tools/wallet/src/parse.rs#L78) cover one TRS and one native raw unit. Signing-only transfer tests use large raw fixtures and do not establish economic movement. |
| Decimal width | `parse_trs_to_wei` parses the whole part into u128 before widening and scaling in U256. Its largest accepted whole part times 10^18 still fits U256, so an alleged U256 overflow in that wallet multiplication was rejected. Oversized/out-of-domain quantities do not establish an ordinary funded workflow failure. Native eight-decimal parsing rejects negative raw amounts before widening. |
| Native submission | The [shared submitter](../../tools/wallet/src/sign.rs#L143) signs a millisecond nonce, serializes the shared signed type, and the [RPC helper](../../tools/wallet/src/rpc.rs#L135) hex-encodes those JSON bytes. Native dry run exits before submitting. F20 concerns the separate EVM Send path. A returned native hash acknowledges admission, not business execution; this is the prior transaction-review qualification. |
| Trading constructors | [PlaceOrder](../../tools/wallet/src/commands/trading.rs#L12) preserves trigger/cap, quantity, side, TIF, reduce-only and client ID; [cancel/modify](../../tools/wallet/src/commands/trading.rs#L90) retain u128 order IDs. No new field-width loss was established for valid supported CLI inputs. Protocol matching, stop and book-initialization issues retain their existing provenance. |
| Unbond completion | The wallet exposes [ClaimUnbonded](../../tools/wallet/src/main.rs#L128), [dispatches it](../../tools/wallet/src/main.rs#L386), and the native executor [handles it](../../crates/torus-bridge/src/native_executor.rs#L4110). The September statement that no production completion action exists is not true of this source. This is source reachability, not proof of all maturity/accounting behavior. |
| Oracle signer registration | [Signer-key resolution](../../tools/wallet/src/commands/validator.rs#L92) requires exactly one supported source or clear; [action construction](../../tools/wallet/src/commands/validator.rs#L79) includes the signer's proof bound to the validator, and [submission](../../tools/wallet/src/commands/validator.rs#L127) uses the validator key. Existing [tests](../../tools/wallet/src/commands/validator.rs#L191) check proof recovery and source exclusivity. No missing ordinary proof path was established. |
| Chain ID | The native signer uses the protocol's [fixed EIP-712 domain](../../crates/torus-types/src/eip712.rs#L23). The wallet's [chain-id override](../../tools/wallet/src/commands/transfer.rs#L28) affects EVM Send. Treating it as an arbitrary native-domain override would contradict the existing fixed-domain admission contract. |
| Wallet query defaults | Missing native balances, zeroed validator/epoch displays, empty `Positions`, one-page market enumeration and swallowed per-position errors remain F32 or its existing coverage notes. [Valid result:null](../../tools/wallet/src/rpc.rs#L49) remains the September client finding; unreachable downstream not-found branches are not counted again. |
| Governance narrowing | [Unchecked u64-to-u32 casts](../../tools/wallet/src/commands/governance.rs#L78) remain historical R7. Normal in-range parameters do not show a new cast error. |
| Contract send gas | [The builder fixes gas at 21,000](../../tools/wallet/src/sign.rs#L94), with empty calldata. Arbitrary contract-call support is outside the [wallet requirements](../../research/tech-req-cli-wallet.md#L48); no new general contract-call claim is made. Plain EOA transfers remain the C01 fixture. |
| Explorer requests and reads | The [typed RPC client](../../crates/torus-explorer/src/rpc_client.rs#L24) handles optional block/receipt responses and numeric block-body heights. [Indexer body positions](../../crates/torus-explorer/src/indexer.rs#L90) intentionally differ from dense Ethereum indices. Envelope parsing, partial ingestion, candle gaps and offset pagination remain historical/F26 issues; no new ordinary first-party request schema mismatch was established. |

Recommended next implementation work is the wallet-level C01 regression and
the full F32 request/response contract fixture. This review establishes
bounded static source support and test gaps, not a passing client-to-node
integration suite or resolution of earlier candidates.
