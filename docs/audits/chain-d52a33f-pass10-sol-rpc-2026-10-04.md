# Pass 10 — ordinary Ethereum RPC and simulation contracts

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, checkout revision
`feb01b60f28e2111611c68947c7063eac9e06f41`. Production source basis is
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`: the committed differences between
these revisions are audit documents and their models. This report is a bounded
source review of valid call/estimate requests, block selectors, supported
transaction projections and the actual test assertions. It does not establish
runtime behavior by executing Rust.

**Result: two additional P2 source-supported compatibility candidates, F41 and
F42.** Under the [audit policy](README.md), both still need failing regressions
against production code. September Astra rounds and the relevant October
pass-3/6/7/8/9 reports were checked for overlap with F01–F40. Historical state
selection, supplied estimation gas ceilings, typed fee execution/projection,
log publication/history and EVM open-order readers are not counted again.
No applicable `AGENTS.md` was found. Cargo/rustc are unavailable; tests were
read, not run. No production files, keys, Git state, live chain, network test or
installed software were changed. Parent coordinates Git and Torus persistence.

## F41 — P2: U256 RPC Quantities retain a leading zero nibble

The shared [`hex_u256`](../../crates/torus-rpc/src/types.rs#L21) helper removes
whole zero bytes and then hex-encodes the remaining bytes. It does not remove a
zero high nibble of the first nonzero byte. Consequently the source mapping is
`1 → "0x01"`, `15 → "0x0f"`, `256 → "0x0100"`, while zero uses the explicit
correct `"0x0"` branch. Values whose minimal representation has an even number
of hex digits, such as 16 or 255, are unaffected.

This violates the minimal hexadecimal encoding of a Quantity in
[EIP-1474](https://eips.ethereum.org/EIPS/eip-1474#quantity); its invalid
examples include a nonzero value with an extra leading zero. This is about
numeric wire fields, not fixed-width hashes, storage words, bytecode or ABI
return data, which have different byte-encoding requirements.

**Ordinary reachability.** A valid account containing one wei, read using
[`eth_getBalance(address,"latest")`](../../crates/torus-rpc/src/eth.rs#L618),
returns this helper's `"0x01"` result. A normal executed supported transaction
carrying one wei likewise projects
[`value`](../../crates/torus-rpc/src/eth.rs#L427) through this helper in mined
transaction views. The same function formats
[`r` and `s`](../../crates/torus-rpc/src/eth.rs#L432). There is no requirement
for a malformed request, historical selector, pruning, concurrent execution,
typed-fee corner case or malicious block. The defect changes formatting rather
than the numerical balance or executed value. A conforming strict consumer can
reject these responses; an actual failing external client was not demonstrated.
The P2 assessment reflects a public interoperability failure, not asset loss.

**Why the tests do not exclude it.**
[`hex_encoding_roundtrip`](../../crates/torus-rpc/src/lib.rs#L1039) tests
U256 value 42 by parsing the generated string with the project's own
[`parse_u256`](../../crates/torus-rpc/src/types.rs#L75), which accepts leading
zeroes; it does not compare a minimal expected encoding. The
[`eth_get_balance`](../../crates/torus-rpc/src/lib.rs#L2957) endpoint test also
parses the returned string with that helper and compares the numerical balance.
Thus these tests can establish numerical roundtrip while missing the wire
contract. No failing Rust test was run in this audit.

**Focused regression and repair.** Encode U256 quantities with minimal hex
digits, retaining `"0x0"` for zero. Assert exact independently specified
strings at 0, 1, 15, 16, 255, 256, 4095 and 4096 and at a full-width maximum;
compare the HTTP balance result and an actual mined transaction's value with
literal expected Quantities. Check signature components against the same
minimal rule without assuming every random signature exhibits it. Keep
`eth_getStorageAt` and byte/ABI fields at their required widths; changing all
hex helpers would introduce a different incompatibility. The analogous native
RPC U256 fields also use this shared helper, but no undocumented native format
contract is needed to establish the Ethereum issue.

## F42 — P2: call and estimation discard a valid access list

The call request's deserialized
[`CallRequest`](../../crates/torus-rpc/src/types.rs#L220) has fee, value, data,
input and nonce fields, but no `accessList`. It has no `deny_unknown_fields`
annotation. A valid object containing `accessList` therefore loses that field
while being deserialized. The
[`build_call_tx_env`](../../crates/torus-rpc/src/eth.rs#L255) constructor never
receives or sets an access list and leaves it at `TxEnv::default()`'s empty
value. This constructor supplies
[`eth_call`](../../crates/torus-rpc/src/eth.rs#L981), the
[`initial estimation probe`](../../crates/torus-rpc/src/eth.rs#L1032) and every
[`binary-search probe`](../../crates/torus-rpc/src/eth.rs#L1063).

The primary Ethereum execution API
[`eth_call` / `eth_estimateGas` schemas](https://github.com/ethereum/execution-apis/blob/main/src/eth/execute.yaml)
both accept `GenericTransaction`, whose
[`transaction schema`](https://github.com/ethereum/execution-apis/blob/main/src/schemas/transaction.yaml)
includes `accessList`. The lost input determines both intrinsic gas and
initially warm accounts/slots under
[EIP-2930](https://eips.ethereum.org/EIPS/eip-2930#specification).
A simulation that accepts the caller's transaction arguments should preserve
that list rather than silently simulate another transaction. This is separate
from the absence of the explicitly unimplemented
[`eth_createAccessList`](../../crates/torus-rpc/src/eth.rs#L9) method: generating
a list and consuming an already supplied one are different contracts.

**Ordinary fixture without any mined typed transaction.** Use an empty-code,
non-precompile destination such as `0xbbbb…bbbb`, zero value, empty calldata,
and an ordinary caller, at a stable latest applied header with the normal 30M
block limit. Omit fee fields so the fixture does not require a 30M-gas balance
or depend on F17's fee accounting. Ask `eth_estimateGas` with and without a
valid access list containing that destination and one 32-byte storage key.
Both objects currently deserialize into identical `CallRequest` values and
produce identical empty-list environments. Under the access-list transaction
contract, the listed request has intrinsic gas of `21,000 + 2,400 + 1,900 =
25,300`, even though the destination executes no code. The current no-list
search instead converges to 21,001 because its
[`lower bound`](../../crates/torus-rpc/src/eth.rs#L1030) is 21,000 and it stops
at adjacent bounds. Returning the same no-list estimate for the listed request
cannot account for the supplied intrinsic cost. This exact search result is a
source inference, not an observed RPC response. The one-gas upper margin is
not counted as a separate defect.

The same request loss prevents the list from warming a slot for an ordinary
storage-reading contract call. Do not claim every contract's return bytes must
change: many return the same bytes with different gas, while gas-sensitive code
can behave differently. The simple estimate fixture already isolates the
missing input without requiring such a contract or a mined transaction.

**Relationship to F17.**
[`signed transaction decoding`](../../crates/torus-bridge/src/decode.rs#L81)
does copy supported type-1/type-2 access lists, unlike this request path. F17
already covers the missing execution `tx_type` on that separate decoded path.
Here the request data is lost before any executor receives it, and fixing mined
envelope typing cannot recover it. Both problems nevertheless interact: the
call constructor also leaves type at the default, so simply adding a request
field would not finish the repair. The previously locally extracted pinned
dependency sources show `revm-context-interface 16.0.0`
`src/cfg/gas.rs:163–179` counts access-list intrinsic gas only for nonlegacy
types, and `revm-handler 17.0.0` `src/pre_execution.rs:48–60` warms the list only
for nonlegacy types. These local dependency reads support the repair warning;
the request-loss evidence is in committed repository source and does not rely
on a dependency download or runtime experiment. Dependency versions are pinned
in [Cargo.lock](../../Cargo.lock#L5556).

**Test gap and regression.** The
[`access-list compliance test`](../../crates/torus-rpc/tests/eth_compliance_tests.rs#L238)
manually creates an `RpcTransaction` response and checks that its JSON contains
an array. It does not deserialize a call object or execute/estimate a list-bearing
request. The
[`bare-call test`](../../crates/torus-rpc/tests/eth_compliance_tests.rs#L139)
has no access list and checks only successful responses. Regress the real RPC
deserialization → transaction-environment → execution path with the EOA
intrinsic-gas fixture, then a deployed storage-reading contract to verify
warming. Omitted and empty lists should retain ordinary no-list behavior;
nonempty lists must survive every estimation probe. Map/derive the appropriate
supported simulation transaction type along with the list, and keep malformed
list validation separate from these valid-input assertions. No tests or fix
were added here.

## Established safeguards, rejected leads and remaining coverage

- **Applied-height selection exists.**
  [`eth_head`](../../crates/torus-rpc/src/eth.rs#L176) caps the durable applied
  marker by committed height. Call and estimation resolve the tag against that
  head and reject heights different from it
  ([call](../../crates/torus-rpc/src/eth.rs#L965),
  [estimate](../../crates/torus-rpc/src/eth.rs#L1012)); latest/pending and an
  explicit equal height choose the same header. The historical read/snapshot
  issue and unsupported tag/object forms are not new findings here.
- **Bare calls are already protected from fee-floor and nonce validation.**
  [`execute_call`](../../crates/torus-evm/src/executor.rs#L163) disables base-fee
  and nonce checks; the
  [RPC test](../../crates/torus-rpc/tests/eth_compliance_tests.rs#L84) explicitly
  seeds an applied height-1 header with nonzero base fee. The
  [executor tests](../../crates/torus-evm/tests/evm_tests.rs#L133) also distinguish
  simulation from consensus execution. Describing all empty-account calls as
  failing the fee floor would be stale.
- **The inner-probe nonce difference was not promoted.** The initial
  [probe defaults](../../crates/torus-rpc/src/eth.rs#L1033) to the stored nonce,
  while rebuilt inner probes leave omitted nonce at zero. Pinned revm's
  `pre_execution.rs:104–118` only checks transaction nonce when checks are
  enabled, and its `frame.rs:288–297` derives a CREATE address from the loaded
  caller account nonce. With checks disabled and a stable database, this
  difference does not establish a different creation address or result.
- **A lower-block-limit RPC failure lacks the proposed ordinary reachability.**
  A synthetic applied header with gas below 30M would reject fixed 30M call
  defaults under pinned revm's block-limit check. But a lower supported genesis
  setting does not prove such a header: the
  [`genesis parent`](../../crates/torus-bridge/src/proposer.rs#L325) hardcodes
  30M, [replay](../../crates/torus-consensus/src/app.rs#L1289) begins with it,
  and ordinary [proposal construction](../../crates/torus-consensus/src/app.rs#L5212)
  inherits the parent's limit. The suggested valid genesis → lower applied
  header → failing bare call chain was therefore not established. No third
  RPC candidate is counted from it.
- **Revert test naming overstates its assertion.**
  [`estimate_gas_returns_error_on_revert`](../../crates/torus-rpc/tests/eth_compliance_tests.rs#L164)
  calls an empty-code destination, binds the result to `_result`, and checks
  neither an error nor revert data. The implementation does
  [return code-3 revert errors](../../crates/torus-rpc/src/eth.rs#L1050) with
  [the output data](../../crates/torus-rpc/src/error.rs#L68), but this test is not
  a runtime regression for that behavior. Deploy a real known reverting
  contract and assert the exact code/data through both methods, while checking
  an ordinary successful contract remains successful.
- **Simulation writer denial is intentional.** The call executor installs the
  [read-only precompile mode](../../crates/torus-evm/src/executor.rs#L203), whose
  [default-deny guard](../../crates/torus-core/src/precompiles.rs#L365) permits
  reader precompiles and rejects writers. Writer-call estimation failures cannot
  automatically be reported as an accidental generic compatibility regression.

The review is complete at the stated source basis. Exact Quantity strings and
valid access-list request preservation require executable regressions; no prior
finding was resolved, no Rust tests passed in this audit, and no chain-safety
claim is made.
