# Pass 10 — independent falsification of recent integration findings

Reviewed 2026-10-04 on `audit/chain-findings-2026-10-04`, HEAD
`feb01b60f28e2111611c68947c7063eac9e06f41`. Read-only Git comparison found no
production diff against `d52a33f51038105a8d0e102fc2e1a5c51f43fb09` in the
crates, tools, Cargo configuration/dependencies, devnet or monitoring paths.

**Result: F35–F40 retain their bounded source support. No additional finding
is proposed.** The review tested ordinary callers, prerequisite state,
alternative readers and recovery paths, and evidence for severity. “Supported”
below means the source supports the stated candidate; it does not mean a
runtime failure was reproduced. P2 remains remediation priority, not evidence
of asset loss, consensus failure or a deployed incident.

The [audit policy](README.md), pass-8 integration/compatibility reports and
pass-9 storage/transaction/history reports were read before tracing the source.
No applicable `AGENTS.md` was found in the checkout or inspected ancestors.
Only this document was written. No production edit, Git mutation, dependency
installation, network/live-chain action, key generation, fault experiment or
interrupted pass-5 certificate/malformed-input work occurred. The unavailable
Rust toolchain was not installed; cited tests were read, not run. The parent
owns shared-memory writes, final numbering and the documentation commit.

## Disposition

| Candidate | Disposition | Decisive source and retained limit |
| --- | --- | --- |
| F35: explicit CLI defaults lose to TOML | Supported | [Main parses and then merges](../../crates/torus-node/src/main.rs#L414); [merge tests scalar equality](../../crates/torus-node/src/main.rs#L303), not argument provenance. Explicit `--data-dir ./data` with TOML `data-dir = "./other-data"` selects the latter. The [documented CLI precedence](../../README.md#L86) makes this a contract violation. Nondefault explicit scalars and optional fields are counterexamples to a blanket “TOML always wins” claim. |
| F36: key creation replaces an existing output | Supported, with recovery prerequisite retained | [Wallet creation](../../tools/wallet/src/commands/keys.rs#L10), [import](../../tools/wallet/src/commands/keys.rs#L31) and [node dispatch](../../crates/torus-node/src/main.rs#L427) reach ordinary overwriting [wallet](../../tools/wallet/src/keystore.rs#L89)/[node](../../crates/torus-node/src/keystore.rs#L95) writes. No destination refusal or replacement confirmation intervenes. Permanent signing-access loss additionally requires no recoverable old key. |
| F37: EVM open-order reader queries an unused index | Supported | [Reader](../../crates/torus-core/src/precompiles.rs#L545) scans `CF_NATIVE_ORDERS`; [production Classic save](../../crates/torus-bridge/src/native_executor.rs#L3471) and [RPC](../../crates/torus-rpc/src/torus.rs#L1738) use actual book state. Symbol/string search found the [stored-order writer](../../crates/torus-core/src/precompiles.rs#L1598) called only by its direct precompile test. A persisted valid resting order suffices; no row-layout migration is needed. |
| F38: archive restart forgets an existing pruning frontier | Supported | [RPC construction](../../crates/torus-rpc/src/lib.rs#L317) initializes zero. [Pruner construction](../../crates/torus-state/src/pruner.rs#L117) loads metadata, but [node startup](../../crates/torus-node/src/main.rs#L1169) constructs it only with retention enabled. [Log queries](../../crates/torus-rpc/src/eth.rs#L1095) consult that atomic before skipping missing receipts. Disabling future deletion must not imply recovery of deleted history. |
| F39: EVM byte admission uses gross/stale occupancy | Supported | [Capacity check](../../crates/torus-mempool/src/lib.rs#L417) precedes the write lock and [replacement/eviction credit](../../crates/torus-mempool/src/lib.rs#L430). [Replacement](../../crates/torus-mempool/src/evm_pool.rs#L139) really supports the same sender/nonce with a sufficient fee bump. Source permits false rejection and a two-caller overshoot schedule; no memory exhaustion or measured overshoot is established. |
| F40: user-fill recovery cannot continue beyond its window | Supported, endpoint scope retained | [User history](../../crates/torus-rpc/src/torus.rs#L1913) always begins at the newest block, caps at 1,000 matching entries and has no continuation. [Reconnect instructions](../api/streams.md#L133) promise backfill. [Uncapped public block history](../../crates/torus-rpc/src/lib.rs#L768) lacks user ownership/role, so it does not supply equivalent user recovery. Independent replay/indexers are outside this narrower claim. |

## Counterexamples and prerequisites that matter

**Configuration and keys.** F35 is avoided by omitting the conflicting TOML
field, supplying a genuinely different explicit scalar or using the optional
fields whose merge checks `is_none()`. Those workarounds do not preserve the
advertised precedence for an explicit default. The data-directory example
does not prove that a node joins an unintended chain or corrupts either DB.

The wallet [prints the newly generated private key and tells the operator to
store it securely](../../tools/wallet/src/commands/keys.rs#L20). An old key saved
from an earlier invocation is therefore a real recovery path. Importing the
same old key can also intentionally replace its encryption without changing
identity. F36's harmful example must rerun **fresh generation**, or import a
different key, at an existing path holding a needed identity without an
independent backup. Passphrase confirmation verifies the new password, not
permission to replace that identity. The [feeder's refusal test](../../tools/price-feeder/src/keyfile.rs#L140)
is a useful repository precedent, not a safeguard applied by these commands.

**Order visibility.** Generic column-family registration, running-hash
inventory and tests naming `CF_NATIVE_ORDERS` are not production population.
The [precompile fixture](../../crates/torus-core/tests/precompile_tests.rs#L251)
manually supplies that index, explaining why it cannot falsify F37. Actual
EVM [provider installation](../../crates/torus-evm/src/executor.rs#L203) and
[reader dispatch](../../crates/torus-evm/src/precompile_provider.rs#L126)
make the selector reachable by normal simulation/static calls. Use a funded,
aligned, noncrossing GTC order and query after save completes. The conclusion
is an incorrect empty contract-visible view; no existing deployed contract
depending on it was established.

**Pruning.** An ordinary retention-enabled restart is a counterexample to the
persistent archive-mode failure because `StatePruner::new` restores the
frontier. The existing [reopen test](../../crates/torus-state/src/pruner.rs#L460)
uses precisely that path. For F38, remove retention from both CLI and TOML,
preserve genesis and the explicit data directory, finish pruning already
applied blocks, close cleanly, and reopen without constructing a pruner.
The [durable metadata reader](../../crates/torus-state/src/pruner.rs#L251)
still sees the old hole; [RPC's check](../../crates/torus-rpc/src/eth.rs#L131)
does not. A valid old single-block log query returning `[]` is the narrow
consequence. This requires no storage error, in-flight pruning or missing
consensus configuration.

**Byte budget.** Let retained bytes be R, cap C, replaced size O and new size N.
The source rejects when R+N>C even if R-O+N<=C. The later subtraction is present;
calling the counter permanently unadjusted after replacement would be wrong.
Count/per-sender/gas limits constrain ordinary admitted inputs but do not make
the default 64 MiB byte limit unreachable: up to 4,096 entries with 20 KiB data
can exceed it while each call stays within the 5M gas admission ceiling, given
sufficient distinct senders, correct nonces and balances. This is a possible
legitimate backlog, not measured frequency or an assertion that small transfers
normally reach the cap.

For the concurrent branch, two funded distinct senders can both pass the
pre-lock test with room for only one envelope, then serialize insertions
without rechecking. The [four-worker RPC runtime](../../crates/torus-node/src/main.rs#L1087)
and [RPC admission caller](../../crates/torus-rpc/src/eth.rs#L710) supply ordinary
concurrent callers. Count eviction, replacement or proposer drainage may
prevent a particular overshoot; the candidate requires neither insertion to
free bytes and the proposer not to drain between them. No unbounded pool,
process-RSS guarantee or OOM follows from this source schedule.

**History.** F40 needs retained, fully written history and a disconnected gap
exceeding 1,000 entries for one user in the **same market**. Distribute fills
across ordinary blocks and stop new activity for a simple stable fixture.
Repeating the request yields the same prefix; reducing its positive limit
shrinks that prefix, increasing it is clamped, and a market filter cannot
reach the older entry in that same market. No assertion about a single block's
achievable fill count is necessary. Zero is not an uncapped bypass: the
[post-push limit check](../../crates/torus-rpc/src/torus.rs#L1977) stops after
the first match. That edge is not counted as another finding here.

The [user codec](../../crates/torus-state/src/trade_rows.rs#L191) records the
relevant role/address association. The [public projection](../../crates/torus-rpc/src/lib.rs#L819)
contains trade/market IDs, price, quantity, taker side, height and timestamp,
but no participant identity or order ID. Fetching every public block therefore
does not by itself identify the missing user fill. The market-range 5,000-row
limit has the real uncapped block fallback and must not be described as making
all public market history inaccessible.

## Pass-10 peer candidates checked

The RPC candidates were independently traced from current source, then the
completed [RPC report](chain-d52a33f-pass10-sol-rpc-2026-10-04.md) and revised
[operations report](chain-d52a33f-pass10-sol-operations-2026-10-04.md) were read:

- **F41 quantity encoding: supported source candidate.**
  [`hex_u256`](../../crates/torus-rpc/src/types.rs#L21) removes zero bytes, not
  the high zero nibble of the first retained byte. Values 1 and 256 therefore
  become `0x01` and `0x0100`; [balance RPC](../../crates/torus-rpc/src/eth.rs#L626)
  uses it. Zero is already correct, and values such as 42 have no extra nibble.
  The [roundtrip test](../../crates/torus-rpc/src/lib.rs#L1040) uses 42 and the
  repository's own parser, so it does not establish canonical encoding for all
  quantities. Strict-client rejection needs a real client regression; universal
  wallet failure is not established.
- **F42 omitted access lists: supported request-path candidate.**
  [`CallRequest`](../../crates/torus-rpc/src/types.rs#L220) has no `accessList`
  field; [environment construction](../../crates/torus-rpc/src/eth.rs#L255)
  leaves the corresponding transaction field at its default.
  [Call](../../crates/torus-rpc/src/eth.rs#L981) and
  [estimation](../../crates/torus-rpc/src/eth.rs#L1032) share this builder.
  [Simulation](../../crates/torus-evm/src/executor.rs#L169) receives the built
  environment and disables fee/nonce checks without reconstructing a list.
  An ordinary supplied list is consequently absent from the simulated
  environment. Gas-direction claims must account for both intrinsic list
  cost and warmed accesses; estimates do not universally understate gas.
- **Operations O02 timeout consumer: supported, P3 scope.**
  The [counter registration](../../crates/torus-telemetry/src/lib.rs#L1120),
  [real timeout callback](../../crates/torus-node/src/main.rs#L954), historical
  [emitted-series record](../perf/cap100-gap-attribution-2026-09-10.md#L23),
  [alert](../../monitoring/alerts/consensus.yml#L70) and
  [panel](../../monitoring/dashboards/consensus.json#L249) agree on the source
  mismatch described by the operations report. A custom alias/recording rule
  could bridge it, and other alerts remain available. No rule evaluation or
  current exporter capture was run here.
- **Operations O01 missing sccache: not established; withdrawn this round.**
  The [.cargo variables](../../.cargo/config.toml#L23) and
  [full image recipe](../../devnet/Dockerfile.full-build#L3) establish configured
  strings and absent explicit installation. Promoting an inevitable compiler
  failure additionally requires the actual locked dependency build path to
  consume `CC_librocksdb_sys` / `CXX_librocksdb_sys` in that form. Environment
  keys existing in Cargo configuration do not alone establish that caller.
  The final operations report withdraws this lead because that consumption
  link remains unproven. This reviewer did not inspect/download dependency
  source or build an image.

The lower-configured-gas-limit RPC lead is also **not established and withdrawn
this round**. The [genesis parent](../../crates/torus-bridge/src/proposer.rs#L325)
hardcodes 30M; [replay initializes from it](../../crates/torus-consensus/src/app.rs#L1289)
and [ordinary proposal headers inherit their parent limit](../../crates/torus-consensus/src/app.rs#L5212).
A lower executor/genesis configuration alone therefore does not establish
the claimed ordinary lower-header fixture. A manually inserted lower header
cannot supply that missing production caller.

All 56 local document/source links were checked for existing targets and
in-range line anchors. Existing supported findings remain candidates pending
focused failing production-code regressions; this pass closes none of them.
