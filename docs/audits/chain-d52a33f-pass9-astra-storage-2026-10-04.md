# Pass 9 — ordinary storage lifecycle and read contracts

Reviewed 2026-10-04 on `perf/item6-phase1`, HEAD
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

**Result: one additional P2 source-supported candidate, local S01.** Disabling
future pruning on a previously pruned database loses the RPC's knowledge of
already removed history. The final audit coordinator owns global numbering.
Under the [audit policy](README.md), this remains a candidate pending a failing
production-code regression. No runtime reproduction or verified fix is claimed.

Scope was valid snapshot creation/restore, ordinary archive/retention settings,
persisted schema readers, and clean database close/reopen. The October
[pass 3](chain-cea1254-pass3-project-wide-2026-10-04.md),
[pass 4](chain-cea1254-pass4-atomicity-2026-10-04.md),
[pass 6](chain-cea1254-pass6-persistence-review-2026-10-04.md),
[pass 7](chain-cea1254-pass7-mixed-review-2026-10-04.md),
[pass 8](chain-d52a33f-pass8-mixed-review-2026-10-04.md), their relevant
companions, and September Astra summaries were checked for prior provenance.
No applicable `AGENTS.md` was found. Cargo/rustc were absent from PATH; tests
below were **read, not executed**. No production edits, Git mutations,
installation, network requests, key operations or live-chain actions occurred.
Only this report was added; the parent owns Torus records. Interrupted pass-five
certificate/malformed-input scopes and fault construction were not resumed.

## S01 — P2: archive restart forgets previously pruned history in RPC

**Normal reachability.** An operator may stop deleting additional history by
restarting the same database with no `retention_blocks` in either CLI or TOML,
or with explicit `--archive` and no retention setting. The
[configuration reference](../configuration-reference.md#L34) describes these
ordinary options, and [startup](../../crates/torus-node/src/main.rs#L469) only
rejects simultaneously selecting archive and retention. It does not reject a
database previously pruned under another configuration.

The source separates durable pruning progress from its RPC representation:

1. Successful [pruning](../../crates/torus-state/src/pruner.rs#L179) deletes
   bodies, action statuses and receipts, then stores the exclusive pruning
   frontier in `CF_BLOCK_HEADERS`. Headers and transaction-location indexes
   survive. [The metadata reader](../../crates/torus-state/src/pruner.rs#L251)
   can recover that frontier from the intact database.
2. [`RpcServer::new`](../../crates/torus-rpc/src/lib.rs#L300) always creates
   `pruned_up_to` with zero. It scans the latest header independently and does
   not initialize pruning progress from the database.
3. The node shares the [RPC atomic](../../crates/torus-node/src/main.rs#L1079)
   with [`StatePruner::new`](../../crates/torus-state/src/pruner.rs#L117), which
   does load persisted progress. But the node constructs that pruner only in
   the [retention-enabled branch](../../crates/torus-node/src/main.rs#L1169).
   Archive startup takes the other branch, leaving the RPC frontier zero for
   the entire run. Repository caller search found no alternate startup load.
4. [`check_pruned`](../../crates/torus-rpc/src/eth.rs#L131) depends solely on
   this atomic. With zero it permits a query below the durable pruning frontier.
   [`eth_getLogs`](../../crates/torus-rpc/src/eth.rs#L1075) then scans surviving
   headers; its [receipt helper](../../crates/torus-rpc/src/eth.rs#L225) skips
   absent receipt rows and successfully returns an empty vector.

For a concrete ordinary sequence, finish applying through height 2000, retain
1000 blocks and let a successful prune cycle record frontier 1000. A normal
log-emitting transaction in block 500 previously had a receipt; the permitted
retention policy has removed it. A query for `[500, 500]` before restart returns
the explicit [DataPruned error](../../crates/torus-rpc/src/error.rs#L33).
Cleanly reopen the same database with unchanged chain/genesis/book settings
and retention disabled. The identical query now passes the pruning check and
returns successful `[]`. A range spanning the frontier can similarly return
only retained logs without indicating its known missing prefix. The query is
within the 10,000-block bound and entirely below the applied head.

This is loss of historical-read completeness information. It can make an
indexer interpret unavailable events as an empty result. No affected deployed
indexer, balance loss, or extra deletion is established. **Archive mode cannot
restore previously deleted data; that is not the claimed defect.** It must
continue reporting the existing hole while preserving future history. An intact
snapshot of a pruned database restored into archive mode has the same metadata
initialization consequence; the snapshot copy itself need not omit anything.

**Deduplication.** F31 concerns deleting execution inputs above the applied
frontier. Here every deleted block is already applied and pruning does exactly
what the operator requested; the failure is later RPC initialization. F19 and
F35 are also unnecessary: keep the same genesis, remove retention from both
configuration sources, and use the same explicit nondefault data directory.
This is unrelated to F21 root-verification coverage or a restore interruption.

**Coverage read.**
[`prune_meta_persists_across_restart`](../../crates/torus-state/src/pruner.rs#L460)
really closes/reopens RocksDB, but constructs another `StatePruner` afterward,
which repopulates the atomic and bypasses the failing archive branch.
[`pruned_rpc_query_returns_none`](../../crates/torus-state/src/pruner.rs#L493)
directly checks database rows; despite its name, it does not call JSON-RPC.
[`find_latest_height_skips_prune_meta_key`](../../crates/torus-rpc/src/lib.rs#L1000)
checks only that the metadata key does not obscure the latest header, not that
RPC loads its value. No matching archive-transition RPC regression was found.

**Repair direction and focused regression.** Initialize the RPC's historical
availability frontier from durable state for every startup, independently of
whether future pruning is enabled. Do this before serving requests; enabling
the pruner should reuse the initialized shared frontier. Keep the stored
frontier monotonic when changing retention policy, and do not reset it merely
because archive mode is selected.

Use valid persisted headers, an applied marker and a real receipt containing a
log, invoke the real pruner, then release every DB owner and cleanly reopen.
Build the ordinary RPC server without constructing a pruner and query the old
single block and a range crossing the frontier. Require `DataPruned` in both
cases, exact logs for retained blocks, and unchanged persisted pruning metadata.
Also cover retention staying enabled, increasing retention, a fresh archive DB,
and intact snapshot restoration of the pruned fixture into archive mode. Do not
manually seed the RPC atomic in the test: that would hide the missing load.

## Intact snapshots: export API, actual restore caller, and test limits

[`create_snapshot`](../../crates/torus-state/src/snapshot.rs#L78) uses the RocksDB
checkpoint API and writes supplied metadata alongside it. The checkpoint is
database-wide; [restore copying](../../crates/torus-state/src/snapshot.rs#L294)
recurses over its files, including CFs outside the verifier's commitment. No
ordinary successful path was found that selectively loses nonce, session,
governance, queue or node-local book rows during that copy. F21 remains the
different question of what successful verification establishes.

Caller reachability matters: source search found `SnapshotManager`,
`create_and_prune` and snapshot-export calls only in library/unit/integration
tests. The [existing operational plan](../plans/unwedge-tool.md#L18) already
documents absent production automatic-snapshot wiring. `SnapshotConfig` is
therefore not evidence of an active periodic node feature. In contrast,
[`--restore-from-snapshot`](../../crates/torus-node/src/main.rs#L513) is wired
and invokes restore before opening the node's writable `StateDb`. The selected
snapshot must already exist. No new finding is counted for the known missing
automatic export caller.

The [valid unit restore](../../crates/torus-state/src/snapshot.rs#L414) closes
the source handle, restores into a fresh target and checks one account.
The [integration lifecycle](../../crates/torus-integration-tests/tests/snapshot_dos_keys.rs#L50)
seeds 50 account/storage pairs, restores into another directory while the
source DB remains open, checks all balances and recomputes the EVM root.
Neither executes 50 actual blocks, replaces a populated target, or seeds
authoritative native families. The source DB staying open is not itself a
problem for a checkpoint copied to another path.

The [manager integration test](../../crates/torus-integration-tests/tests/snapshot_dos_keys.rs#L152)
checks directory count after three exports and retention two. It supplies an
EVM-only root in metadata and never verifies/restores those outputs; it cannot
establish that every retained snapshot is a valid composite-root export. The
[unit manager test](../../crates/torus-state/src/snapshot.rs#L467) uses the
composite root but still checks count rather than the exact retained heights
and subsequent restores. These are coverage qualifications, not claims that
the production exporter computes a wrong root: metadata belongs to its caller.

A useful next regression creates a valid quiescent applied-state checkpoint
with all relevant families populated through supported operations, verifies
it, restores it over a closed populated destination, and compares canonical
rows and frontier metadata before continuing ordinary execution. Include
level-authority node-local order rows and the pruning frontier. Separately
verify each retained manager output and its exact height. Leave adversarial
metadata and the known pass-four two-rename interruption window to their
existing scopes.

## Schema/read compatibility has explicit boundaries

Writable [DB open](../../crates/torus-state/src/db.rs#L349) requests all names
in [`ALL_CF_NAMES`](../../crates/torus-state/src/cf.rs#L236) with missing-family
creation enabled. [Snapshot verification](../../crates/torus-state/src/snapshot.rs#L119)
instead uses the current name list with creation disabled and a read-only open.
The supplied metadata type has no schema-version/migration dispatch. Thus
current-revision round-trip tests are not evidence for arbitrary older snapshot
schemas or binary downgrades. No concrete supported upgrade sequence was
established to fail here, so lack of broad compatibility certification is not
promoted to another defect.

Book readers have a stronger local contract: they derive layout from persisted
markers/content, [not process flags](../../crates/torus-core/src/book_reader.rs#L16).
Modes 2/3 share decoding, while their level-hash definitions differ. The
[executor checks the exact writer marker](../../crates/torus-bridge/src/native_executor.rs#L2594)
and explicitly requires [fresh genesis for a mode change](../../crates/torus-bridge/src/native_executor.rs#L1040).
An ordinary same-mode reopen belongs in regression coverage; changing this
flag on an existing chain is a documented unsupported migration, not a new
restart defect. Preserve F37's separate unused-index EVM selector finding.
The [real-save RPC tests](../../crates/torus-rpc/tests/book_read_modes_rpc_tests.rs#L162)
exercise successful order reads across layouts, but do not establish that
every EVM selector shares their implementation or that a DB close occurred.

Archive retention also does not imply historical EVM account-state support.
[`eth_call`](../../crates/torus-rpc/src/eth.rs#L966) explicitly rejects resolved
heights other than the applied head. Keeping historical bodies/receipts does
not create historical account snapshots. S01 concerns a different implemented
contract: correctly identifying intentionally removed historical event data.

## Clean close/reopen evidence is real but narrow

The [WAL test](../../crates/torus-state/src/db.rs#L1207) closes/reopens a DB and
checks one raw consensus row. The
[write-controller tuning test](../../crates/torus-state/src/db.rs#L1454)
opens with nondefault local options, closes, reopens with defaults and checks
one raw account-CF row. The
[native DA round trip](../../crates/torus-state/tests/native_da_tests.rs#L80)
releases the store and DB, reopens, and confirms body presence. These are
substantive library persistence tests; they do not execute a node shutdown,
recover a process crash, or cover the combined chain state.

Conversely, the substantial
[running-hash restart matrix](../../crates/torus-consensus/src/app.rs#L14605)
drops/recreates execution contexts against the same open `StateDb`. It checks
mode and execution-path consistency, not database close/reopen. `StateDb` itself
is an [Arc-backed cloneable handle](../../crates/torus-state/src/db.rs#L132),
so dropping one context is insufficient to close the DB. Future clean-restart
fixtures should explicitly release RPC/mempool/store/worker owners before open,
then resume from the same configuration and compare exact balances, nonces,
orders, rewards, queue effects, receipts and applied height to a continuous run.

No additional successful-path data-loss finding was established. Existing
F18 bytecode serialization, F19 restart configuration, F21 verification coverage,
F30 marker/mirror ordering, F31 replay-input pruning and the known interrupted
snapshot swap remain open without being recounted. This report supplies one
new read-contract candidate and bounded coverage findings, not chain safety
certification or a passing Rust test result.
