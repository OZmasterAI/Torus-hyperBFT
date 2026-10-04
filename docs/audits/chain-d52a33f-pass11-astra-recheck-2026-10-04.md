# Pass 11 — independent client and ordinary-state recheck

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, starting HEAD
`0c967cac7f8e30bb79d98390948e9299e6ac3feb`. Read-only comparison found no
production differences from `d52a33f51038105a8d0e102fc2e1a5c51f43fb09` in
`crates`, `tools`, `monitoring`, Cargo manifests/lockfile or `.cargo`.

**Result: the proposed F44 wallet candidate survives independent falsification
within its pooled, sequential-send fixture. No further production finding is
proposed.** The settlement test-path qualification also survives; existing
state-view tests provide useful but narrower composition evidence than their
names alone suggest. F32 and F41–F43 retain their earlier provenance.

The [audit policy](README.md), earlier consolidated/detailed reports, and the
pass-11 [client](chain-d52a33f-pass11-sol-client-2026-10-04.md),
[state-view](chain-d52a33f-pass11-sol-state-views-2026-10-04.md) and
[settlement](chain-d52a33f-pass11-astra-settlement-2026-10-04.md) reports were
read. No applicable `AGENTS.md` was found. Cargo and rustc are unavailable on
PATH: **Rust tests were read, not run**. No source edit, Git mutation, dependency
installation, live-chain/network experiment, key operation, Torus write or
interrupted pass-5 work was performed. Only this report was added; the parent
owns numbering, persistence and the documentation commit.

## F44: retained with a precise admission fixture

[Send](../../tools/wallet/src/commands/transfer.rs#L16) loads one nonce, quotes
one gas price, signs, submits, prints and returns. Its
[nonce helper](../../tools/wallet/src/rpc.rs#L72) hardcodes `latest`.
[RPC](../../crates/torus-rpc/src/eth.rs#L670) reads the account nonce for that
selector, while `pending` calls the pool-aware helper. The
[broadcast handler](../../crates/torus-rpc/src/eth.rs#L702) acknowledges
successful local admission without waiting for execution. Neither caller
contains an inclusion wait or a retry using a newly allocated nonce.

Use one funded sender at an ordinary small nonce N, correct chain ID, two
plain EOA transfers to different recipients, no other sender activity and one
RPC endpoint. Keep the head/account state and quoted/admission fee at
1,000,000,000 wei per gas, preserve free byte/count capacity, and retain the
first transaction in that node's pool until after the second submission.
The second command may start after the first returns; concurrent commands
are unnecessary. Sufficient funding for the sum of both transfers and gas
avoids relying on admission's individual-affordability behavior.

The [builder](../../tools/wallet/src/sign.rs#L94) emits N with a fee cap of
1,000,000,000 both times. Changing recipient makes distinct unsigned
payloads, avoiding the duplicate-envelope branch. The
[admission validator](../../crates/torus-mempool/src/validate.rs#L93) compares
nonce against executed state, so N is not already too low. The
[pool replacement check](../../crates/torus-mempool/src/evm_pool.rs#L139), with
the [default 10% bump](../../crates/torus-mempool/src/lib.rs#L107), requires
1,100,000,000 and returns `ReplacementUnderpriced`. The first remains pooled;
the intended second transfer is not admitted. The
[RPC limiter's default](../../crates/torus-rpc/src/lib.rs#L318) is 50
submissions per ten seconds per sender, so a fresh two-submission fixture is
not stopped earlier by that limiter.

| Attempted counterexample | Disposition |
| --- | --- |
| The first send already executed | Avoids this failure: latest now yields N+1. This is why no universal failure of serial sends is claimed. |
| The first send was drained into a proposal | Outside the retained fixture. The earlier [R2 lifecycle issue](chain-cea1254-pass3-surface-2026-10-04.md) addresses missing in-flight nonce accounting; F44 does not need it. |
| Identical recipient/value and signing inputs | May return a duplicate-hash error before replacement. Retain distinct recipients. |
| The second quote rises sufficiently | Can change the replacement outcome. The fixed positive quote isolates nonce selection; no silent replacement or fund loss is demonstrated here. |
| Read `pending` instead | For this still-pooled fixture, [consecutive nonce accounting](../../crates/torus-mempool/src/lib.rs#L1333) supplies N+1. It does not atomically allocate nonces for simultaneous processes or fix R2. |
| Insufficient balance, capacity or wrong chain | Can reject earlier and obscure the candidate. These are fixture prerequisites, not evidence against the nonce mismatch. |

This is a separate first-party client defect: even when the existing pending
RPC mechanism has the right answer, the wallet selects executed state.
P2 is defensible as transaction-workflow remediation priority. No financial
loss, consensus failure, deployed incidence rate or measured collision window
is established. Waiting for execution avoids the issue; the
[Send interface](../../tools/wallet/src/main.rs#L101) has recipient/value
arguments, without nonce selection or an inclusion-wait option.

### What a production-code regression must exercise

The [wallet builder test](../../tools/wallet/src/sign.rs#L279) supplies nonce
zero itself and checks the type byte and encoded length. The
[transfer roundtrips](../../tools/wallet/src/commands/transfer.rs#L97) recover
signers for native actions; they neither call EVM Send nor establish collateral
movement. The [pool replacement test](../../crates/torus-mempool/src/lib.rs#L2566)
does assert rejection of a 5% fee bump and acceptance of 20%, while
[pending_nonce_consecutive](../../crates/torus-mempool/src/lib.rs#L3201)
asserts zero, one and two across admitted nonce-0/nonce-1 envelopes. These
are useful component assertions, not a wallet request-level nonce test.

Use a local fixture that calls the actual wallet workflow against RPC backed
by a real pool and a funded test account, with no proposer draining it. Before
changing production behavior, assert the current second-send error and that
only the first hash remains admitted. The desired post-repair assertions are
two successful admissions, decoded nonces N and N+1, exact distinct recipients
and values, and `pending` request selection. Add a completed-first-send control
and keep concurrent allocation/in-flight lifecycle regressions separate.
A mock that merely returns different nonces without checking the selector
would not expose this defect. No such test was written or executed here.

## F32 extension: valid direct queries can select another market

The client report's direct-query extension is supported. CLI
[Orderbook/Position](../../tools/wallet/src/main.rs#L132) accept strings;
the [RPC helpers](../../tools/wallet/src/rpc.rs#L147) forward them unchanged;
the node [parser](../../crates/torus-rpc/src/types.rs#L83) uses radix 16 even
without `0x`. Thus decimal-looking `10` selects numeric market 16. A user
placing an order with numeric market 10 can query different state with that
string. Explicit `0xa` and IDs zero through nine are useful controls.

Do not count this separately from F32. The independent test should create
different valid books/positions at markets 10 and 16 and invoke the direct
wallet queries, asserting the outgoing selector and returned identity. Fixing
the earlier market-list response parsing alone would not repair these direct
queries. Nothing here establishes that the native RPC's wire parser violates
its own hexadecimal selector contract.

## Ordinary state views: substantive coverage and remaining boundaries

The [real-save RPC fixture](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L83)
funds traders, executes five ordinary orders, asserts successful results, then
saves books before starting its local RPC server. Its
[depth assertions](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L147)
pin bid prices 100/99, remaining quantities 3/7, first-level count one, and ask
105 with quantity four. This is actual executor/save/HTTP-reader composition,
not just a hand-serialized book. The
[open-order assertions](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L162)
pin counts three/four and inclusion of market two; they do not compare every
order field. The [slot-count test](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L196)
adds a pending stop and pins five maker slots, zero taker slots and volume 200
on both sides. Resting-order count four versus slot count five is intentional.

**Mode qualification:** the test's
[`MODES` list](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L126)
contains Classic, OrderRows and LevelAuthority, not LevelAuthorityChunked.
The [reader explicitly maps marker 3 to the LevelAuthority layout](../../crates/torus-core/src/book_reader.rs#L62),
and the separate [mode-3 save matrix](../../crates/torus-bridge/tests/save_books_parallel_tests.rs#L493)
checks worker equivalence, real parallel engagement and reconstructed-context
boot verification. Its reload uses `par.db.clone()`; it is not a clean RocksDB
close/reopen or mode-3 HTTP fixture. This limits the test claim, rather than
establishing a broken mode-3 reader.

Likewise, [Classic open-orders RPC](../../crates/torus-rpc/src/torus.rs#L1778)
uses [orders_for_trader](../../crates/torus-core/src/order_book.rs#L1669), whose
[sequence contract](../../crates/torus-core/src/order_book/trader_orders.rs#L1)
is arrival/decode order. A cross-layout array-index mismatch alone is not
missing order data; compare keyed identity/field sets absent an explicit common
RPC ordering promise. The separate EVM unused-order-index candidate F37 remains
distinct from this working native RPC path.

The [basic position RPC test](../../crates/torus-rpc/src/lib.rs#L3877)
directly seeds an isolated position and pins side, size, entry, realized PnL
and margin mode. Its assertions do not establish ordinary isolated allocation
or liquidation-price correctness. The
[user-trade ordering test](../../crates/torus-rpc/src/lib.rs#L4176) pins exact
newest-block/ascending-index/role/market tuples and limit-after-filter behavior,
but starts from the row codec rather than matching. These reader tests should
not be relabeled full client-to-execution economic regressions.

For a stronger ordinary-flow test, fund valid makers/takers, complete saves at
a fixed executed state, then compare independently expected balances,
positions, open interest, depth, order fields and own-side fills. Include a
partial close, flip, full close and cancellation, then close/reopen the DB.
Use aligned prices/quantities, a valid known mark, and preserve required
node-local rows. Do not introduce an unsupported writer-mode migration or
concurrent snapshot guarantee into this fixture. The existing source/test
evidence does not close previously reported lifecycle or snapshot issues.

## Settlement: the exact-PnL parallel-path qualification is real

The fixture [pnl_realizing_cross_market_flow_exact](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L503)
passes `false` and `true`, but the closing batch contains only two market-2
orders: A buys four at 200 and sells four at 195. Its exact balance is 9,980
with zero order margin. The prior batch touches two markets but only opens
positions. The [production selection gate](../../crates/torus-bridge/src/native_executor.rs#L4811)
requires at least two markets with work before inspecting `SettleMode::Force`.
Consequently **the fixture's nonzero PnL is settled sequentially in both runs**.
The comments about closing across two markets do not override the action list.

This is a test-coverage qualification, not a production defect or evidence
that all parallel PnL coverage is absent. The
[six-market differential scenario](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L230)
feeds repeated [byte-equivalence assertions](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L325)
with a greater-than-20-trades non-vacuity check, and
[worker-cap comparisons](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L367).
Those are meaningful determinism checks. Their inspected assertions do not
independently pin a specified trader's nonzero closing PnL in the parallel plan.

The [A5 test](../../crates/torus-bridge/tests/parallel_settle_tests.rs#L579)
does put four markets into its consuming batch and asserts exact raw-unit
reservation outcomes on the parallel result. The
[PnL-then-release test](../../crates/torus-bridge/tests/position_cache_exec_tests.rs#L158)
also independently checks a +10 close followed by partial-release accounting,
plus the resulting long-one position at 85, on a single-market path. Neither
should be dismissed as only differential or vacuous coverage.

Extend the exact-PnL closing batch with a valid close in another market and
assert actual parallel-path selection, nonzero closing events, the net cash
change, reservations and final position rows. Independently calculate expected
values; sequential equality alone cannot detect an arithmetic mistake shared
by both paths. For broader conservation, the
[existing accounting assertion](../../crates/torus-bridge/tests/account_margin_tests.rs#L1419)
includes cash plus position value at a common mark and allows 100 raw units of
drift. Cash-only conservation while counterparties retain positions is not the
same invariant, and that fixture does not prove universal exact conservation.

## Limited pass-10 countercheck

| Existing candidate | Independent retained evidence and boundary |
| --- | --- |
| F41 | [U256 formatting](../../crates/torus-rpc/src/types.rs#L21) removes whole zero bytes, so one becomes `0x01`; zero and 42 are controls. The [existing roundtrip](../../crates/torus-rpc/src/lib.rs#L1040) uses 42 and the permissive local parser. No real strict-client rejection was run. |
| F42 | [CallRequest](../../crates/torus-rpc/src/types.rs#L220) has no access-list field, and [environment construction](../../crates/torus-rpc/src/eth.rs#L292) leaves it in defaults. Preserving a response-side [access-list serializer](../../crates/torus-rpc/src/eth.rs#L306) does not populate the simulation request. No universal estimate direction is claimed. |
| F43 | The [registered counter](../../crates/torus-telemetry/src/lib.rs#L1120), [timeout callback](../../crates/torus-node/src/main.rs#L954), and historical [double-suffix emitted spelling](../perf/cap100-gap-attribution-2026-09-10.md#L23) retain the mismatch with the shipped [alert](../../monitoring/alerts/consensus.yml#L70) and [panel](../../monitoring/dashboards/consensus.json#L249). A custom alias can bridge it. No fresh scrape or rule evaluation was performed. |

This bounded recheck preserves source-supported candidates and separates
component tests, composed fixtures and missing path assertions. It reports no
runtime reproduction, passing Rust verification, verified fix or whole-chain
correctness result. All 53 local links resolve to existing files with in-range
line anchors; the repository whitespace check returned no errors.
