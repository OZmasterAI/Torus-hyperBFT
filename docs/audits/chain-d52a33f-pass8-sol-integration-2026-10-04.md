# Pass 8 — ordinary operator and client integration review

Reviewed 2026-10-04 on `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`. This bounded source review covers
node CLI/TOML merging, startup/stop wiring, default keystore lifecycle, wallet
submission/query contracts, and the price-feeder's node integration. It is a
whole-project integration review, not a claim that the new mark-table commit
introduced the candidates below. No applicable `AGENTS.md` was found in the
repository or the inspected ancestor directories.

The audit [policy](README.md), September Astra reports/progress, October
pass-three consolidated/surface reports, pass-four consolidated reports,
pass-six correctness/persistence reports and pass-seven consolidated/client
reports were inspected for provenance. F01–F34 and their supplements are not
counted again. In particular F19 configuration identity on restart, F20 Send
dry run, F26 partial explorer ingestion, F32 wallet response schema and F33
subscription wiring remain existing candidates.

**Result: two additional P2 source-supported candidates.** P2 is remediation
priority with the stated ordinary operator preconditions; neither candidate is
a reproduced runtime defect. Cargo and rustc were absent and were not installed.
Rust tests were read, not run. No live node, transaction, network query,
benchmark, cryptographic experiment or destructive filesystem reproduction was
performed. Only this report was added by this worker; no production source or
Git state was changed. Parent review owns Torus persistence and numbering.

## I01 — P2: explicit CLI defaults lose precedence to TOML values

The [README promises](../../README.md#L86) that CLI flags always override config
file values, as does the [CLI option documentation](../../crates/torus-node/src/main.rs#L94).
The real path [parses CLI first and merges TOML next](../../crates/torus-node/src/main.rs#L414).
For default-valued scalar options, however,
[apply_config_defaults](../../crates/torus-node/src/main.rs#L303) checks value
equality with the compiled default instead of whether the operator supplied a
flag. It overwrites an explicitly supplied default as readily as an omitted
option. The same mechanism affects data directory, P2P listener, RPC listener,
log level and metrics listener.

A concrete valid example is a TOML file containing
`data-dir = "./other-data"` and an invocation with
`--config torus.toml --data-dir ./data`. Clap's
[default declaration](../../crates/torus-node/src/main.rs#L102) and the ordinary
explicit flag both produce the value compared at line 304. Merging replaces it
with `./other-data`; [startup opens that directory](../../crates/torus-node/src/main.rs#L523).
An operator deliberately selecting `./data` can therefore read/write the other
database. No malformed TOML, restart omission, alternative chain identifier or
storage failure is needed. With both directories existing this is an identity
selection failure; with the configured directory absent it can instead create
and initialize that directory, subject to the normal supplied-genesis rules.
The report does not assert corruption of either database or successful joining
of an unintended chain.

Likewise, TOML `rpc-addr = "127.0.0.1:18545"` plus explicit
`--rpc-addr 0.0.0.0:8545` selects 18545, causing an integration expecting the
explicit endpoint to contact the wrong address or fail. This is the same root
cause, not a separately counted listener defect. Explicit values different
from the compiled defaults are preserved. Optional genesis/keystore/peer fields
use `is_none()` and do not have this exact scalar problem.

This differs from F19: F19 concerns choosing consensus configuration for
existing state even when all options were merged as intended; I01 concerns
losing the operator's explicit option before startup selects its resources.

**Regression needed:** parse actual argument lists, then merge TOML and assert
that every explicitly supplied scalar value, including its compiled default,
wins. Assert omitted options acquire TOML values and omitted-both options get
compiled defaults. For the directory case, test selection with two temporary
database paths without operating a live chain. No `apply_config_defaults` or
CLI/TOML precedence regression was found in the node source. Existing
[commit-lag override tests](../../crates/torus-node/src/main.rs#L384) cover a
different resolver; they were read and do not establish this contract. Preserve
argument provenance (for example Clap value sources) or merge `Option` values
before assigning defaults rather than inferring omission from equality.

## I02 — P2: ordinary key creation silently replaces an existing keystore

The wallet's [keygen default](../../tools/wallet/src/main.rs#L58) is
`./wallet.keystore`, also the [import default](../../tools/wallet/src/main.rs#L64).
[cmd_keygen](../../tools/wallet/src/commands/keys.rs#L10) asks for a new passphrase
and confirmation, then calls
[generate_keystore](../../tools/wallet/src/keystore.rs#L39), which generates a
fresh random key. Its [writer](../../tools/wallet/src/keystore.rs#L86) uses
`std::fs::write` at the chosen path with no existing-file refusal, explicit
replacement switch or retained previous key. Ordinary
[import](../../tools/wallet/src/commands/keys.rs#L31) reaches the same writer.
Passphrase confirmation confirms the new encryption passphrase; it does not
ask whether to replace an existing signing identity.

The validator binary has the same lifecycle: its `keygen` command defaults to
`./validator.keystore` ([definition](../../crates/torus-node/src/main.rs#L202)),
[dispatch](../../crates/torus-node/src/main.rs#L427) generates a fresh key and
reports success, and the
[node writer](../../crates/torus-node/src/keystore.rs#L92) uses `fs::write`.
Thus rerunning a syntactically valid creation command in a directory already
containing its default keystore replaces that file with another identity.
This requires a local key-management action; no remote party or unusual input
is involved.

The material consequence requires that the overwritten file held a funded
wallet key or registered validator key and that no independent backup of the
old key exists. The wallet can then no longer sign for the old account, and
normal node restart loads a new consensus identity. No on-chain balance is
deleted, and no claim is made that validator rotation or fund recovery is
impossible when an old-key backup exists. Intentional replacement may be a
valid policy, but it needs an explicit CLI choice so creating another key
does not silently discard the previous one.

There is a relevant repository precedent: price-feeder
[keygen](../../tools/price-feeder/src/keyfile.rs#L54) explicitly refuses to
overwrite and installs a newly written keystore without replacing an existing
destination. Its
[keygen_refuses_to_overwrite test](../../tools/price-feeder/src/keyfile.rs#L140)
asserts the original bytes survive a repeated creation attempt. This safeguard
does not extend to the general wallet/node commands merely because the feeder
uses the wallet's keystore format.

**Regression needed:** create a temporary wallet/node keystore, retain its bytes
and public identity, invoke the CLI's ordinary creation command again at the
same output path, and require an explicit refusal with original bytes and
identity intact. Cover wallet import too. If replacement is supported, test it
only under an explicit option and specify backup/atomic-write behavior. The
existing wallet [keygen_and_load](../../tools/wallet/src/keystore.rs#L139) and
node [keygen_write_and_load](../../crates/torus-node/src/keystore.rs#L164) tests
use fresh paths and exercise round trips; neither tests repeated destination
creation. All mentioned tests were read, not executed. No keys were created or
overwritten during this audit. This is a client/operator lifecycle candidate,
not an assessment of cryptographic algorithms or key confidentiality.

## Stop wiring — retained operational qualification, no new numbered finding

The WSL [stop script](../../devnet/wsl/stop-3val.sh#L14) sends ordinary `kill`
(TERM) then gives processes time to “flush rocksdb.” The relaunch
[runbook](../ops/s442-relaunch/relaunch-runbook.md#L53) calls `kill -TERM` a clean
RocksDB shutdown. The node's
[sole explicit shutdown wait](../../crates/torus-node/src/main.rs#L1259) awaits
`tokio::signal::ctrl_c()`; no Unix terminate-signal registration or combined
INT/TERM wait was found in the node. Consequently these operator procedures do
not establish that the source's return/drop path runs for TERM. Merely waiting
after delivery does not invoke it.

The code does contain substantive shutdown mechanisms:
[Replica::drop](../../crates/hotstuff_rs/src/replica.rs#L675) sends shutdown and
joins owned threads, and
[TorusApp::drop](../../crates/torus-consensus/src/app.rs#L4644) closes the
execution sender and joins its execution thread. Their presence is not proof
that either signal reaches all drains successfully. Historical
[roadmap notes](../plans/blockspeed-orders-roadmap-s405.md#L31) already retain a
SIGTERM-hang investigation; this review does not claim discovery of that issue
or conflate it with the absent normal TERM handler. No lost durable state,
corruption, hanging process, or complete SIGINT shutdown was demonstrated.
Keep this as a runbook/stop-contract follow-up: run the real binary under a
temporary local setup, signal INT and TERM independently with work queued,
assert shutdown/drain completion and bounded exit, then reopen and verify.

## Source safeguards and candidates not promoted

- **Wallet transfer units agree with the corrected lockbox contract.**
  [Native transfer/withdraw commands](../../tools/wallet/src/commands/transfer.rs#L58)
  parse eight-decimal native units, while EVM Send parses wei. The old September
  blanket factor-of-10^10 wallet claim was not repeated. Transfer tests in that
  file construct actions directly and establish signing round trips rather
  than command parsing or full lockbox execution; they were read, not run.
- **Decimal block selection is converted deliberately.**
  [cmd_block](../../tools/wallet/src/commands/query.rs#L111) parses a decimal u64
  and hex-encodes it before invoking Ethereum RPC. Raw pass-through of a
  decimal height as a hexadecimal selector was considered and rejected as a
  candidate. Proposal/open-order filter method arguments also agree with the
  server's numeric argument types in the inspected paths.
- **Native dry run has a real early return.**
  [The signing helper](../../tools/wallet/src/sign.rs#L147) serializes and
  returns before native submission. This does not fix F20's separately wired
  EVM Send path. F32's response parsing and the historical valid-null result
  issue remain prior candidates, not new observations in this count.
- **The feeder consumes the current wire schema.**
  [Market and validator parsers](../../tools/price-feeder/src/node.rs#L48)
  expect camelCase fields, hexadecimal market IDs and textual validator
  status. [Market collection](../../tools/price-feeder/src/node.rs#L84) pages
  with the node's 500-item cap rather than silently stopping at the default
  first page. Existing parser, paging and startup-check table tests were read,
  not run. This prevents extending F32 automatically to every shipped client.
- **Feeder readiness and dynamic listing checks are real.**
  [run_cycle](../../tools/price-feeder/src/feeder.rs#L225) retries startup-check
  errors on the next cycle, waits on inactive readiness and re-fetches markets
  each cycle. A successful submission response establishes admission; this
  review does not assume it proves later on-chain aggregation/execution.
- **Bare-metal restart argv is centralized.**
  [start-node.sh](../../devnet/wsl/start-node.sh#L34) supplies genesis, data path,
  key, listeners, private-address allowance, peers and gossip settings through
  one function used by launch and crash restart. The comment identifies an
  existing harness parity assertion. No launch/restart divergence was
  established here; the harness itself was not run.

This pass is bounded. It does not cover every historical benchmark launcher,
every external exchange payload, dependency signal internals, stateful wallet
nonce management under concurrent Send, process crash recovery, or adversarial
certificate/input analysis. It does not close any earlier finding. The two
new candidates need focused production-code regressions before runtime
confirmation; the stop note needs a concrete process-level contract test.
