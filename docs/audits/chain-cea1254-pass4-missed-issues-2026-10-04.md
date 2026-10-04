# Torus chain audit — fourth pass: missed issues

Audited 2026-10-04 on `merge/item6-sync2` at `cea1254e34625e6b09c58f794de8793b5c12713c`.

Two additional **GPT-6.1-sol** agents reviewed areas underexplored in the preceding passes: economics/governance/genesis lifecycles, and cross-VM atomicity/crash recovery/pruning. The coordinating review independently checked the promoted source paths and compared their mechanisms with earlier findings.

**Five additional P2 candidates were identified, plus two extensions of historical findings.** None is represented as a runtime-confirmed vulnerability. The severe consequence of the malformed-genesis candidate requires operator-supplied invalid configuration; the crash/pruning findings also have explicit failure/configuration preconditions. These priorities should not be read as five permissionless remote attacks.

Cargo and rustc remain unavailable. Per the user's instruction to continue past that blocker, no toolchain was installed. Source-guarded Python models explain the counterexamples; they do not execute the Rust implementation. No production files, branch, commits, external services, or live-chain state were changed.

## Detailed reports and relationship to earlier passes

- [Economics, governance, and genesis](chain-cea1254-pass4-economics-2026-10-04.md): E01–E03, detailed production paths, preconditions, fixes, and regressions.
- [Atomicity, crash recovery, and pruning](chain-cea1254-pass4-atomicity-2026-10-04.md): A01–A02 and historical extensions H01–H02.
- [Economics models](chain-cea1254-pass4-economics-models.py) and [atomicity/recovery models](chain-cea1254-pass4-atomicity-models.py).

Earlier reports are preserved: [pass 1](chain-cea1254-2026-10-04.md), [pass 2](chain-cea1254-pass2-2026-10-04.md), and [project-wide pass 3](chain-cea1254-pass3-project-wide-2026-10-04.md). F01–F26 and the earlier supplemental candidates are not counted again. Numbering below continues the consolidated list; absence from the reviewed reports is not a claim that nobody previously knew the issue.

| ID | Priority | Local ID | Additional finding | Required conditions |
| --- | --- | --- | --- | --- |
| F27 | P2 | E01 | Executed governance parameter changes do not update effective governance configuration | An approved allowlisted parameter-change proposal |
| F28 | P2 | E02 | Invalid genesis commission can wrap reward allocation into enormous claimable liabilities | Genesis accepts commission above 100%; a real delegator and positive inflation emission |
| F29 | P2 | E03 | Governance deadlines are processed late on otherwise idle chains | Pending proposal; no other native-phase trigger until an epoch boundary |
| F30 | P2 | A01 | Applied account state can survive a crash with a stale incremental EVM trie | Crash between account/marker flush and separate mirror resync; existing nonempty trie |
| F31 | P2 | A02 | Pruning can delete bodies still needed for crash replay | Pruning enabled with retention below execution lag; crash before those bodies are applied |

## F27 — Governance updates can be recorded without taking effect

The live native `ParameterChange` action maps into an executable governance payload. After approval and timelock, execution validates the key/value, writes ASCII bytes under `CF_FEE_CONFIG[param_key]`, and marks the proposal Executed. Governance decisions instead deserialize the separate `gov_params` row. No production reader of those standalone changed keys was found.

For example, executing `quorum_bps=5000` leaves the effective default quorum at 3300. A subsequent vote with 40% participation can still satisfy the old threshold, despite a successful proposal purporting to require 50%. Voting period, timelock, permanent-stake multipliers, and permanent-unlock threshold have the same configuration disconnect. The related unread market-risk parameter keys are already documented as deferred and are not counted as new here.

Primary anchors: [native action mapping](../../crates/torus-bridge/src/native_executor.rs#L7932), [payload execution](../../crates/torus-economics/src/governance.rs#L1051), and [effective configuration read/write](../../crates/torus-economics/src/governance.rs#L1209). Existing tests inspect the raw changed row, which does not establish changed behavior.

**Fix/regression:** update the typed authoritative configuration rather than an unused key, explicitly define activation for existing proposals, and test the behavior of a subsequent proposal after an approved change. Do not merely assert that storage contains the proposed string.

## F28 — Genesis commission bypasses reward arithmetic's assumptions

Genesis deserialization accepts `commission_bps` as a u16, and initialization copies it directly into an Active validator's row. It bypasses the normal registration commission bound. Commission 20,000 therefore reaches epoch inflation, which computes commission `2E` and delegator pool `E−2E` for positive validator emission E.

The exact locked `ruint 1.17.2` source implements unsigned subtraction with wrapping behavior. Thus the pool becomes `2^256−E`. With one real delegator, the last-row remainder branch assigns that full pool to its pending rewards. ClaimRewards credits the account; native admission explicitly exempts ClaimRewards from the funding check, so the delegator need not retain an EVM gas balance to claim. Modular summation of validator and delegator allocations can still equal E, concealing the excess liabilities.

Primary anchors: [genesis parse](../../crates/torus-genesis/src/lib.rs#L313), [commission initialization](../../crates/torus-genesis/src/lib.rs#L398), [reward subtraction](../../crates/torus-economics/src/rewards.rs#L216), [last-delegator allocation](../../crates/torus-economics/src/rewards.rs#L238), and [claim](../../crates/torus-economics/src/staking.rs#L406). The companion report documents the dependency checksum and a four-validator fixture.

This is a bootstrap validation defect under malformed operator configuration, not a demonstrated way for an ordinary user to change commission beyond the existing live limits. **Fix/regression:** validate all genesis validator constraints before writing state, defensively reject commission above its permitted range in reward calculation, and use checked allocation/conservation arithmetic. Test malformed JSON through actual initialization and the valid-boundary reward path.

## F29 — Governance processing depends on unrelated activity

The [native-phase gate](../../crates/torus-consensus/src/app.rs#L1945) considers native actions, EVM fee revenue, CoreWriter work, epoch boundaries, and oracle/liquidation work. It does not consider pending governance. The sole production [governance processing call](../../crates/torus-consensus/src/app.rs#L2302) is inside that gate.

On an otherwise quiet chain, an active proposal can remain unfinalized after its voting end. Finalization eventually runs at an epoch boundary; the timelock starts from that delayed finalization and may then wait for another boundary to execute. In the default-parameter model with proposal height 1 and epoch length 100,000, expected finalization 302,402 becomes 400,000, and expected execution 345,602 becomes 500,000. Ordinary activity or persistent oracle/liquidation work can shorten this delay; it is not an unconditional permanent governance freeze.

**Fix/regression:** make due governance work an explicit deterministic phase trigger, ideally with a bounded next-deadline index. Test a submitted and voted proposal across empty blocks through both deadlines without injecting unrelated activity. The old historical empty-epoch-boundary omission is already addressed by the current epoch-boundary gate and is not reasserted here.

## F30 — The applied marker precedes incremental account mirror repair

Native operations can alter EVM account balances. The [main atomic batch](../../crates/torus-consensus/src/app.rs#L2489) includes those account writes and the applied-height marker, but [incremental account resync](../../crates/torus-consensus/src/app.rs#L2558) occurs afterward in a separate [database write](../../crates/torus-state/src/incremental.rs#L480). A normal process crash between these operations leaves the plain account state current and its hashed mirror/trie stale.

On restart, [trie initialization](../../crates/torus-state/src/incremental.rs#L131) only checks whether the hashed-account family is nonempty; it does not validate a matching applied frontier. Replay skips the already-applied block. An unrelated subsequent bundle need not touch the affected address, so it does not guarantee repair. The incremental route is default-on despite older nearby comments saying otherwise.

The demonstrated source-level invariant failure is persisted root state disagreeing with authoritative account state. Current commit-then-execute validation skips the expected-header-root comparison; therefore this report does not claim an observed consensus split. Optional debug/full-scan comparison can detect divergence, while normal routing can continue using the stale base. Earlier pipeline design notes acknowledge the write window but describe incremental roots as off by default and lazily repaired. The current default and an untouched recipient invalidate those mitigations; this is additional audit evidence, not a claim that the write ordering was undocumented. See A01 for exact consumer/recovery limits.

**Fix/regression:** include account mirror/trie updates in the same atomic applied batch, or persist a dirty frontier that boot must repair before enabling incremental reads. Inject termination between the two writes, reopen, and compare full/incremental roots before any unrelated transaction can mask the gap. Exercise both serial and background-flush paths.

## F31 — Pruning follows committed height rather than replay safety

The optional background pruner receives the commit-visible latest height and calculates `cutoff = current − retention`. It does not clamp deletion to the durable applied frontier. The CLI accepts small positive retention values, and execution can lag commit delivery.

With commit/fed height 1,000, applied height 997, and retention 1, body 998 lies below cutoff 999 and can be deleted while still awaiting execution. Dispatch already removed that height's commit manifest after saving the body. A crash now discards the in-memory execution item. On restart, the local replay body is missing, the manifest-based DA reconstruction input is gone, and the fallback is a local-body-only recovery placeholder.

The separately retained HotStuff datum does not automatically repair this schedule: the committed feed sees its fed marker at the committed head and sends nothing, while sync's tree-gap test sees the existing tree entry. Explicit redelivery, repair, or resynchronization may still recover the node; universal irrecoverability is not claimed. Default archive mode is unaffected.

Primary anchors: [pruner wiring](../../crates/torus-node/src/main.rs#L1169), [cutoff](../../crates/torus-state/src/pruner.rs#L145), [body deletion](../../crates/torus-state/src/pruner.rs#L212), [manifest removal](../../crates/torus-consensus/src/app.rs#L996), [fed-frontier shortcut](../../crates/hotstuff_rs/src/committed_feed.rs#L105), and [local-only materialization](../../crates/torus-consensus/src/app.rs#L5942).

**Fix/regression:** protect all unapplied replay inputs regardless of user history retention; clamp the deletion horizon to a durable applied frontier and retain manifests until execution or equivalent recoverability is durable. Test delayed execution plus pruning, termination, and automatic restart recovery at the first missing height.

## Historical extensions, not additional new findings

**H01 — CoreWriter error propagation stops one caller too early.** `drain_core_writer` returns a queue read/decode error, but the application discards that Result. Its later `fatal_error` check does not catch this early return. The block can advance without draining due work, including delayed deposits whose EVM value was previously burned. The queue uses an exact target-height prefix, so later blocks do not simply pick up the skipped height. This extends F04's storage-error category and the historical EVM-FIND-12 trace; it is not counted as a sixth new finding.

**H02 — Snapshot replacement still has a two-rename crash window.** Restore renames the active directory to `.old`, then separately renames `.restoring` to the active name. Each rename is atomic, but the pair is not. A crash between them leaves the active path absent. Normal startup without the restore option can create/open that path afresh; supplied genesis can initialize it despite valid old/staged directories still being present. This is a residual of historical EVM-FIND-06, distinct from F21's snapshot-content verification gap. The model exercises only temporary directories; it never touches a real node database. Recovery-aware startup and an explicitly crash-safe replacement protocol are required; no claim of unavoidable total data loss or observed equivocation is made.

## Checks and remaining work

```sh
python3 docs/audits/chain-cea1254-pass4-economics-models.py
python3 docs/audits/chain-cea1254-pass4-atomicity-models.py
```

The economics script models configuration key separation, unsigned reward allocation, and quiet-chain scheduling. The atomicity script models persisted account/mirror ordering, prune/feed/replay frontiers, and the two historical extensions. These are source/revision-guarded counterexamples, not executions of the Rust code or a validator network. The filesystem rename experiment uses a disposable temporary directory and is not a RocksDB restore test.

The reviews also found functioning protections: nested-call native journaling and writer CALL/static restrictions, lockbox value burn/queued-credit coupling, serial barriers around EVM-produced CoreWriter writes, atomic claim-unbonded updates, and current epoch-boundary scheduling. These observations narrowed the findings; they do not certify all behavior in those areas. Known reward policy choices, unslashable unbonding, failed-proposal queue blocking, and unsupported governance actions were kept out of the new count.

Production regressions and fixes remain outstanding. No prior issue was closed. Reports, source anchors, model scripts, and all five promoted discoveries were saved; this pass adds evidence rather than changing chain behavior.
