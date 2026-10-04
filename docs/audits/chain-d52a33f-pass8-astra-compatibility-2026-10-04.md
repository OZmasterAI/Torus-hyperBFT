# Pass 8 — ordinary native/EVM read-contract compatibility

Reviewed 2026-10-04 on `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: one additional P2 source-supported candidate, F37.** The ordinary
EVM open-orders reader queries an index that production order execution never
populates. This is independent of the new C2 mark table. Under the audit
[policy](README.md), it remains a candidate until a failing production-code
regression reproduces it.

Scope was valid ordinary native orders and native/EVM read representations:
precompile registration, persisted order readers, balances, positions, oracle
time and staking encodings. The audit README, relevant October pass-3/4/6/7
reports, September Astra summaries, current pass-8 reports and associated design
notes were checked for overlap. No applicable `AGENTS.md` was found. Cargo/rustc
are unavailable; tests below were **read, not run**. No keys were generated,
production files edited, Git state mutated, software installed, network or
live-chain actions performed, or Torus records written. Only this report was
added. Interrupted pass-five certificate, malformed-input and adversarial work
was not resumed.

## F37 — P2: EVM getOpenOrders reads an index without a production writer

**Contract and normal reachability.** The public
[`getOpenOrders(address,bytes32)` selector](../../crates/torus-core/src/precompiles.rs#L413)
dispatches to `read_open_orders` at precompile address `0x0800`. The actual EVM
executor installs this provider for both
[call simulation](../../crates/torus-evm/src/executor.rs#L203) and
[block execution](../../crates/torus-evm/src/executor.rs#L280).
The [provider](../../crates/torus-evm/src/precompile_provider.rs#L126) allows
reader precompiles in simulation and static calls, and returns successful
output bytes. This is a shipped query path, not a test-only helper or dormant
selector. Valid zero-value calldata and sufficient ordinary call gas suffice.

The reader constructs `trader(20) | market(8)` and
[scans only `CF_NATIVE_ORDERS`](../../crates/torus-core/src/precompiles.rs#L546),
decoding `StoredOrder` rows into four dynamic arrays. It never calls the shared
layout-aware book reader or examines the actual book CFs. If this index has no
entries, it returns success with four empty arrays.

Production book persistence uses other data:

- [Classic save](../../crates/torus-bridge/src/native_executor.rs#L3471) serializes
  the real `OrderBook` under its market key in `CF_NATIVE_ORDER_BOOKS`.
- [OrderRows save](../../crates/torus-bridge/src/native_executor.rs#L3491) writes
  tagged order rows into that same book CF.
- [Level-authority save](../../crates/torus-bridge/src/native_executor.rs#L3749)
  writes individual order rows to `CF_BOOK_ORDER_ROWS`; price levels, stops and
  metadata remain in the root book CF. Mode 3 shares this reader layout.

A whole-repository search for `CF_NATIVE_ORDERS`, `cf_native_orders` and
`write_stored_order` found no production order writer for this separate index.
Its [writer helper](../../crates/torus-core/src/precompiles.rs#L1596) is called
only by the direct precompile test. Generic database/root/pruning code naming
the CF does not derive order rows from successful trading. The earlier
[running-state-hash inventory](../plans/running-state-hash-impl.md#L268)
already notes that this CF has no live writer; this report adds the concrete
public-query consequence. It does not claim that the missing writer itself was
previously unknown.

By contrast, [`torus_getOpenOrders`](../../crates/torus-rpc/src/torus.rs#L1738)
detects the persisted layout and reads the real order rows or classic book.
Thus on a fresh database, after an ordinarily funded trader places a valid
noncrossing GTC limit order in a listed market and the native block finishes
persisting, RPC can return that resting order while the EVM selector returns
four empty arrays. Use price/quantity aligned with both the configured and
effective book settings, so this fixture does not depend on F25. Query after
the applied block is fully saved, or in a subsequent EVM block; no concurrent
read, execution lag, crash or unusual input is required.

The impact is incorrect contract-visible order state. An EVM integration cannot
discover its ordinary native resting orders through the advertised selector.
This does not demonstrate unauthorized trading, balance loss or an affected
deployed consumer. P2 reflects the public integration failure, not a measured
incident. It is distinct from F22's returned CoreWriter order handle and from
the deliberately preserved classic `getOrderBook` snapshot-decoding limitation:
this selector returns an empty result even for the classic layout, and uses
the wrong source in row layouts too.

**Why current tests do not exclude it.**
[`order_book_reader_get_open_orders`](../../crates/torus-core/tests/precompile_tests.rs#L251)
manually calls `write_stored_order` twice, then checks only that response length
exceeds 128. This establishes encoding of the test index, not production
population or exact returned IDs. The substantial
[RPC layout regression](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L162)
does use [real executor/save fixtures](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L91)
and checks three orders in market 1/four across markets, but calls the RPC
endpoint rather than this precompile selector. The shared-reader
[open-orders test](../../crates/torus-bridge/tests/book_read_modes_tests.rs#L289)
also exercises `book_reader::read_open_orders`, which this selector does not use.
These tests are useful; their coverage must not be transferred to another
read implementation.

**Focused repair and regression.** Establish one authoritative read contract
and make the EVM selector project the actual persisted book/order state. An
EVM-visible behavior change needs the project's compatibility/activation policy;
simply populating another index introduces a synchronization obligation across
placement, partial fill, modification, cancellation, full fill and reload.
Use the existing real-save fixture in Classic, OrderRows and LevelAuthority
(plus mode-3 compatibility) and call the actual selector through `eth_call` or
the EVM provider. Decode all four ABI arrays and require the actual resting ID,
price, remaining quantity and side to agree with the RPC representation after
unit conversion. Cover partial fill, cancel/full removal and a clean reopen;
also assert an unrelated trader/market remains empty. Start with a fresh empty
`CF_NATIVE_ORDERS`, and never seed it using the helper. No such regression was
added or executed in this audit.

## Position, balance and oracle outputs require explicit field mapping

The [position reader](../../crates/torus-core/src/precompiles.rs#L492) uses the
canonical Borsh `Position`, returns signed raw size (negative for shorts), and
sign-extends signed PnL via [ABI helpers](../../crates/torus-core/src/precompiles.rs#L165).
The [RPC](../../crates/torus-rpc/src/torus.rs#L945) instead returns decimal
magnitude and a separate `long`/`short` side. Missing position means five zero
ABI words versus RPC `null`; these are distinct documented shapes, not evidence
of missing storage. For a stale/missing usable mark, precompile UPnL is zero and
RPC values at entry, also yielding zero. Existing
[precompile boundary assertions](../../crates/torus-core/tests/precompile_tests.rs#L418)
check the 60/61-second edge, independently expected +5000 UPnL and a clock behind
the row. The matched short/negative-PnL and close-to-absence compositions remain
useful additions, rather than claims that basic freshness has no tests.

The [balance reader](../../crates/torus-core/src/precompiles.rs#L595) correctly
locates EVM wei in the first 32 bytes of the current
[account serialization](../../crates/torus-state/src/db.rs#L1010). Its native
fields are eight-decimal raw values, while its EVM field is wei; native RPC
returns decimal strings for native quantities and hex wei for EVM quantities.
The two APIs are not field-for-field equal: the precompile's first and fourth
words both report nonnegative-clamped `available`, while
[RPC nativeBalance](../../crates/torus-rpc/src/torus.rs#L1036) sums available and
order margin and retains signed available. The existing
[negative-available test](../../crates/torus-core/tests/precompile_tests.rs#L333)
explicitly pins the ABI behavior. RPC total margin also adds isolated-position
margin. This pass records these projection differences without claiming that
a test-pinned ABI must silently acquire RPC semantics. Neither first-word output
should be assumed to be a universal account equity calculation.

The scalar/array oracle readers decode the current aggregate layout and age it
using block timestamp. RPC uses the
[executed-head timestamp](../../crates/torus-rpc/src/torus.rs#L422).
`getMarkPrice.timestamp` contains the aggregate **block number**, explicitly
preserved by the [oracle design](../plans/oracle-aggregation.md#L147), so it must
not be compared directly with the aggregate's Unix timestamp or trade timestamps.
The C2/lifecycle reports already qualify EVM-before-native aggregation order;
this pass does not demand newly aggregated native and EVM marks within the same
block to be identical. Concurrent snapshot coherence remains outside the
settled-read fixture used for F37.

## Serialization and staking: no additional ordinary mismatch established

`FixedPoint` is explicitly eight-decimal fixed point. Its
[Display implementation](../../crates/torus-types/src/lib.rs#L162) preserves
negative sub-unit values as `-0.00000001`, and
[`dec_fp`](../../crates/torus-rpc/src/types.rs#L247) uses it directly. The existing
[round-trip test](../../crates/torus-types/src/lib.rs#L1566) includes positive,
negative, zero, raw -1 and large representable endpoints. No new ordinary
decimal-unit/sign mismatch was found. This is not exhaustive extreme-value
parser or arithmetic verification.

The staking reader's manually interpreted delegation prefix agrees with the
actual [Delegation serializer](../../crates/torus-economics/src/types.rs#L175):
two raw 20-byte addresses followed by a 32-byte **big-endian** U256 amount.
The unbonding vector follows that prefix and therefore does not shift the
active delegated amount. Permanent stake and pending rewards likewise agree
with their [typed serializers](../../crates/torus-economics/src/types.rs#L250).
The [reader](../../crates/torus-core/src/precompiles.rs#L778) sums active delegated
amounts and exposes a first nonzero validator, whereas
[RPC](../../crates/torus-rpc/src/torus.rs#L1182) gives separate delegation and
unbonding arrays. Amounts remain wei in both, without native-scale conversion.
`getValidators` already uses typed ValidatorState decoding. No present offset
or normal-denomination defect is promoted.

Coverage needs a qualification: the
[cross-VM delegation fixture](../../crates/torus-integration-tests/tests/cross_vm_read.rs#L222)
manually writes just the 72-byte prefix, omitting even the empty unbonding-vector
length required by the full typed record. It proves that this prefix reader
accepts its fixture, not that a real delegation/undelegation serialization round
trip was executed. Add ordinary manager/native-executor delegation to two
validators, partial undelegation, permanent stake and reward claim, then compare
actual precompile/RPC fields before and after clean persistence. Retain the
different aggregation/absence contracts. The ABI's u128 range and existing
saturating/truncating policies do not prove arbitrary U256 round trips, and no
new high-value/boundary scenario is asserted here.

This bounded review stops with F37 and these coverage qualifications. It does
not close prior findings, certify the full interfaces, report passing Rust
tests, or claim a runtime reproduction or verified fix.
