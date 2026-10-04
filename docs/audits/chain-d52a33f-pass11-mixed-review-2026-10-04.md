# Pass 11 — clients, state views and settlement

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`0c967cac7f8e30bb79d98390948e9299e6ac3feb`. Production source is unchanged
from `d52a33f51038105a8d0e102fc2e1a5c51f43fb09`. This round adds documentation,
not production fixes. No fetch or branch update was part of this round.

**Result: one additional P2 source-supported candidate, F44.** The other
reviews add counterchecks and coverage qualifications without increasing the
defect count. Under the [audit policy](README.md), source candidates need
failing production-code regressions before they are treated as reproduced
defects. Cargo/rustc are unavailable; Rust tests were read, not run.

| Reviewer | Scope | Report |
| --- | --- | --- |
| GPT-6.1-sol | First-party client request/response contracts | [Client review](chain-d52a33f-pass11-sol-client-2026-10-04.md) |
| GPT-6.1-sol | Native state readers and persisted layout projections | [State-view review](chain-d52a33f-pass11-sol-state-views-2026-10-04.md) |
| GPT-6-astra | Ordinary fill settlement and conservation | [Settlement review](chain-d52a33f-pass11-astra-settlement-2026-10-04.md) |
| GPT-6-astra | Independent falsification and test-path review | [Independent recheck](chain-d52a33f-pass11-astra-recheck-2026-10-04.md) |

## F44 — P2: wallet Send reuses an executed nonce while an earlier transfer is pooled

The wallet's [Send command](../../tools/wallet/src/commands/transfer.rs#L32)
queries the account nonce, signs and submits a transaction, then returns after
admission. Its [RPC wrapper](../../tools/wallet/src/rpc.rs#L72) hardcodes
`eth_getTransactionCount(address, "latest")`. The node deliberately distinguishes
[pending from latest](../../crates/torus-rpc/src/eth.rs#L670): pending includes
consecutive pool entries, while latest reads executed account state.

Consider a sufficiently funded sender with executed nonce N. Its first ordinary
transfer has been accepted and remains in the pool. Before proposal selection
or execution, the user sends a second transfer to a different recipient, with
the head and positive gas quote unchanged. Both commands select nonce N and
the same maximum fee. The wallet's [signer](../../tools/wallet/src/sign.rs#L96)
preserves those inputs; the pool treats the second transaction as a replacement
and [requires a fee bump](../../crates/torus-mempool/src/evm_pool.rs#L139).
At the default 10% bump and a 1-gwei quote, the second transaction offers 1 gwei
where 1.1 gwei is required, so it is rejected instead of queued at N+1.

This is a source-derived ordinary fixture, not a sent transaction or runtime
failure. The first transfer is not lost. Identical transfers may instead hit
duplicate-hash rejection; inclusion before the second query removes this
particular collision. Sufficient funding and available admission capacity
exclude unrelated rejection paths. The finding concerns a repeated-send client
workflow, not consensus safety or an arbitrary guarantee of transaction inclusion.

The [pool's pending-nonce contract](../../crates/torus-mempool/src/lib.rs#L1333)
explicitly exists for clients submitting faster than blocks commit. Using it
addresses this pooled fixture, but does not solve prior R2's missing in-flight
nonce accounting or races between concurrent wallet processes. Those need
separate coordination. F44 is distinct because the first transaction remains
visible in the pool throughout the fixture.

The wallet tests inspect signing and serialization, not this command-to-RPC
nonce choice. A focused regression should keep one accepted transaction pooled,
run a second distinct Send through the actual command path, and assert nonce
N+1 and successful admission while preserving the first hash. An independently
specified mock RPC can check the selected tag, but should not replace the
real admission regression. Include the no-pending case and distinguish pooled,
selected-but-not-executed, and concurrent-process cases.

## Qualifications without additional finding counts

- **F32 has a direct-command radix consequence.** Wallet placement parses a
  numeric market ID, whereas direct `orderbook`/`position` commands forward a
  string. Node `parse_u64` interprets unprefixed strings as hexadecimal. Thus
  `place-order --market 10` and `orderbook 10` refer to different numeric IDs;
  explicit `0xa` selects market 10 in the query. The CLI help does not specify
  a decimal query contract, so this is retained as a client-consistency
  extension, not a new numbered defect. See the client report for exact paths.
- **One exact-PnL fixture bypasses parallel settlement in its closing batch.**
  The named cross-market test requests parallel mode, but the PnL-producing
  batch has only one market and the implementation always requires at least
  two. Its exact balance assertion therefore exercises sequential settlement.
  Other multi-market differential tests remain useful; this is not proof of
  wrong parallel balances or of a total absence of parallel PnL coverage.
- **Economic assertions need the right conserved quantity.** Cash alone can
  change while counterparties retain unrealized PnL. The settlement review
  traces event PnL credit, exact reservation release and stop revalidation,
  and distinguishes existing cash-plus-position-value tests from exact cash
  conservation. Native trading fees/rebates and funding remain deferred;
  `TransferToSpot` is an EVM withdrawal, not a spot matching engine.
- **Reader tests have different strengths.** Real saved-book RPC fixtures
  cover Classic, OrderRows and LevelAuthority depth; mode 3 shares the mode-2
  reader but is not a separate case in that HTTP fixture. Shared-reader tests
  additionally assert exact order tuples. Classic and row readers need not return identical
  sequences without a public ordering promise. Synthetic position fixtures
  do not establish ordinary isolated-margin allocation. Zero-limit edges and
  capped history recovery are already documented and are not counted again.
- **Initial stream gaps are already documented.** Parent review traced the
  once-per-block subscriber sample and absent extras, then checked the
  [delivery contract](../api/streams.md#L123) and prior pass-9 coverage. A block
  executing when the first subscriber joins may be missed; this is not a new
  finding against the stated stream contract.

## Verification and handoff

The coordinator checked F44's client, RPC and pool paths and the exact-PnL
test's parallel-selection gate. The independent reviewer separately checked
the candidate's prerequisites and counterexamples. Local report links, source
line bounds and documentation whitespace were validated before the commit.
No build, runtime transaction, live chain, service, key operation or production
edit occurred. The previously interrupted pass-5 scope was not resumed.

F44 was saved as Torus discovery `06aeb3d9-fbc3-40ec-bb2c-862a731c74e3`.
No finding was resolved. The next implementation step is a focused failing
regression, followed by the selected repair and its validation.
