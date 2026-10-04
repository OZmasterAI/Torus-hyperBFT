# Pass 4: persistence and cross-VM atomicity

Audited checkout: `/home/oz/projects/Torus-hyperBFT`, branch `merge/item6-sync2`,
`cea1254e34625e6b09c58f794de8793b5c12713c`, 2026-10-04. This report records two
additional P2 source-supported candidates, two historical extensions, and negative
results. It does not count F01–F26 or supplemental R2/R3/R5/R7 again.

This is a source audit. Cargo/rustc are absent; no Rust tests, RocksDB fault
injection, node processes, transactions, toolchain installation, Git mutations, or
production edits were performed. The companion Python script checks source
patterns and executes small control-flow models. It is not a production-code
reproduction or a proof of consensus safety. The snapshot model exercises ordinary
filesystem rename/mkdir only inside a temporary directory.

Read exclusions included the October first/second/third reports and companion
trading/consensus/surface reports, both historical Astra summaries and progress
notes, `research/audit-3.4.3-evm-correctness.md`,
`research/re-audit-3.4.6-critical-findings.md`, and relevant atomicity/pipeline plans.
No applicable `AGENTS.md` was found. The parent audit owns Torus memory writes.

| Local ID | Priority | Candidate | Trigger / qualification |
| --- | --- | --- | --- |
| A01 | P2 | Applied marker can outrun native EVM-account mirror/trie resync | Process crash between successful writes; persistent EVM trie already exists |
| A02 | P2 | Pruning can remove bodies still required for crash replay | Nonarchive retention smaller than execution lag, prune tick, ordinary process crash |
| H01 | Historical extension | Ignored CoreWriter drain errors can strand burned deposits | Storage read error; overlaps F04 and historical EVM-FIND-12 |
| H02 | Historical residual | Interrupted snapshot replacement can reopen a fresh active DB | Process crash between two directory renames; ordinary restart without restore option |

## A01 — Applied state and the EVM trie mirror have a crash window

**Invariant.** If an applied marker makes replay skip height N, the persisted
state used by default root computation must also represent height N, or startup
must detect and repair the gap before using that root.

**Reachable path.** A signed native `Withdraw` credits its chosen EVM recipient:
`NativeExecutor::execute_action` dispatches it at
[native_executor.rs:4043](../../crates/torus-bridge/src/native_executor.rs#L4043),
`exec_withdraw_to` performs the margin check and calls the lockbox at
[native_executor.rs:8051](../../crates/torus-bridge/src/native_executor.rs#L8051),
and [lockbox.rs:181](../../crates/torus-core/src/lockbox.rs#L181) stages the native
debit and recipient `CF_ACCOUNTS` credit atomically. The amount is not a creation
of value: the counterexample only needs an ordinary authorized transfer.

The serial execution path writes the native overlay, EVM prefix if any, and
applied marker together at
[app.rs:2489](../../crates/torus-consensus/src/app.rs#L2489). It then obtains the
dirty EVM accounts and calls `resync_evm_accounts` separately at
[app.rs:2556](../../crates/torus-consensus/src/app.rs#L2556). That resync updates
`CF_HASHED_ACCOUNTS` and the trie nodes in another `db.write` at
[incremental.rs:480](../../crates/torus-state/src/incremental.rs#L480).
The pipeline worker has the same ordering: the state/marker flush at
[exec_pipeline.rs:426](../../crates/torus-consensus/src/exec_pipeline.rs#L426)
precedes mirror resync at
[exec_pipeline.rs:485](../../crates/torus-consensus/src/exec_pipeline.rs#L485).

On startup [app.rs:3834](../../crates/torus-consensus/src/app.rs#L3834) calls
`ensure_trie_built`. Its only eligibility check is whether
`CF_HASHED_ACCOUNTS` contains at least one entry:
[incremental.rs:131](../../crates/torus-state/src/incremental.rs#L131) and
[incremental.rs:143](../../crates/torus-state/src/incremental.rs#L143). It does not
check an applied-height stamp or plain/mirror equality. Replay begins after the
durable applied height at
[app.rs:4227](../../crates/torus-consensus/src/app.rs#L4227), so the interrupted
height does not run again and the transient dirty-address list has been lost.

**Counterexample.** Begin with a nonempty, correct EVM mirror. At height N, a
funded account with no positions withdraws five native units to a distinct EOA R
whose EVM balance is seven corresponding units. Let the atomic state/marker batch
succeed, then terminate the process before resync writes. Restart the same DB.
Plain `CF_ACCOUNTS[R]` now has twelve; its hashed mirror still has seven. The
mirror passes the nonempty boot check, and the applied marker suppresses replay
of N. Execute a later EVM transaction touching only different accounts, with fee
recipients distinct from R. Neither that bundle's hashed updates nor its native
dirty-address resync includes R. The stale entry can persist indefinitely until
R is touched or an explicit repair happens.

**Concrete consumer and limits.** This checkout selects incremental root
computation **by default** at
[state_root.rs:19](../../crates/torus-bridge/src/state_root.rs#L19). The flag-off
comments at the post-marker resync and in older pipeline docs are stale. The
catchup validator calls the routed root computation after executing the EVM
bundle at [validator.rs:196](../../crates/torus-bridge/src/validator.rs#L196).
Its root is therefore calculated from inconsistent root inputs even though
plain balances remain correct. The existing
`resync_evm_accounts_repairs_native_post_commit_drift` test at
[incremental.rs:909](../../crates/torus-state/src/incremental.rs#L909) already
asserts that bypassed plain writes make incremental and full roots differ before
repair; that test was read, not run.

Under debug builds or `TORUS_INCREMENTAL_ORACLE`,
[state_root.rs:51](../../crates/torus-bridge/src/state_root.rs#L51) returns an
error on the divergence. The catchup caller then encounters the known F04
error-swallowing path at
[app.rs:1875](../../crates/torus-consensus/src/app.rs#L1875), potentially discarding
the later EVM section while continuing native execution. In normal release mode
the stale root can be returned and used to produce trie updates. The live CTE
catchup path skips comparison with the advertised expected state root; this
report **does not claim a demonstrated release-mode conflicting commit, incorrect
plain balance after the original withdrawal, or a stale RPC proof endpoint**.
The source-established new failure is durable derived-state drift after a
healthy process-crash seam, with a default-on consumer.

**Provenance.** `docs/design-flush-pipeline.md` crash row W2 and
`docs/perf/design-exec-pipeline-2026-08-20.md` row C9 mention this window, but
dismiss it as off-by-default and lazily self-healing. The current default and
unrelated-recipient schedule invalidate those mitigations. This is an additional
audit finding, not a claim that the ordering was never documented. It differs
from F04's local storage failure trigger and from an incremental-batch build
fallback: both writes can be healthy, and the initial native update and marker
can commit successfully.

**Fix/regression.** Fold final native-written EVM account mirror/trie updates
into the state/marker batch, or atomically mark the derived base stale and
persist enough repair state to rebuild it before default root use after restart.
A rebuild must correctly remove old mirrored entries, not just overlay current
ones. Add a crash hook immediately after the serial/native-worker state flush
and before resync; reopen with an existing trie, require plain/full/incremental
agreement, then execute an unrelated bundle. Cover both a new recipient and an
existing recipient, deletion/empty-account cases, default release root selection,
and the oracle-enabled behavior. Existing after-EVM-before-native crash tests do
not reach this seam.

## A02 — Pruning follows committed height rather than durable execution

**Invariant.** Every nonempty committed but unapplied height must retain a
locally recoverable body or an authoritative recoverable datum until its applied
marker becomes durable.

The node's commit event updates the shared RPC latest-height counter at
[main.rs:958](../../crates/torus-node/src/main.rs#L958)–966. The background pruner
reads that same counter at
[main.rs:1175](../../crates/torus-node/src/main.rs#L1175)–1186. It never reads the
native applied marker. `StatePruner::maybe_prune` computes
`cutoff = current_height - retention_blocks` at
[pruner.rs:145](../../crates/torus-state/src/pruner.rs#L145) and deletes old block
bodies at [pruner.rs:215](../../crates/torus-state/src/pruner.rs#L215). The CLI's
`Option<u64>` retention at
[main.rs:151](../../crates/torus-node/src/main.rs#L151) accepts small positive
values and zero without an execution-safety floor.

At dispatch, the manifest is deleted in the atomic header/body batch at
[app.rs:1002](../../crates/torus-consensus/src/app.rs#L1002); that happens before
the execution channel applies the block. Live execution trusts its dispatch
`DurableRows` and skips rewriting the body at
[app.rs:2664](../../crates/torus-consensus/src/app.rs#L2664). These mechanisms are
individually reasonable but pruning invalidates their durability assumption.

**Counterexample with positive retention.** Enable `--retention-blocks 1`.
Let the commit and app-feed frontiers reach 1000 while execution is at 997,
with nonempty dispatched bodies 998, 999 and 1000 queued. This lag fits the
bounded execution channel and requires no malformed block or failed DB
operation. On an eligible prune tick, cutoff is 999 and body 998 is deleted.
Its dispatch manifest has already been deleted. Terminate the process before
height 998's state/marker flush.

On restart, `replay_gap` requires the body because the header says the block is
nonempty at [app.rs:1311](../../crates/torus-consensus/src/app.rs#L1311); it stops
at 998. `recovery_exec_source` finds no manifest and falls back to
`ExecSource::Durable(998)` at
[app.rs:1132](../../crates/torus-consensus/src/app.rs#L1132)–1144. That source
only rereads the absent local body, returning an empty missing-hash set at
[app.rs:5954](../../crates/torus-consensus/src/app.rs#L5954)–5972, so even existing
native DA bodies are not recoverable through that lookup.

**Why other recovery guards do not automatically close it.** The node separately
clamps HotStuff tree retention to at least 1000 at
[main.rs:825](../../crates/torus-node/src/main.rs#L825); the committed datum can
still exist in that tree. However, the durable `APP_FED_BLOCK_HEIGHT` advanced
after dispatch callbacks, rather than after execution:
[committed_feed.rs:135](../../crates/hotstuff_rs/src/committed_feed.rs#L135)–145.
With fed=committed=1000, boot reconciliation returns zero at
[committed_feed.rs:105](../../crates/hotstuff_rs/src/committed_feed.rs#L105).
It does not refeed 998 merely because native applied=997.
Automatic block-sync backfill scans `BLOCK_AT_HEIGHT`, which remains populated,
at [client.rs:661](../../crates/hotstuff_rs/src/block_sync/client.rs#L661)–683;
it does not see this `CF_BLOCK_BODIES` hole. Reconcile ticks only retry the same
local source at [app.rs:5612](../../crates/torus-consensus/src/app.rs#L5612).

**Impact/preconditions.** A healthy nonarchive node can lose its automatic local
crash-replay path and park execution, eventually fail-stopping when the hole
budget is checked/expires at
[app.rs:6235](../../crates/torus-consensus/src/app.rs#L6235). This does not silently
apply a missing block. Default archive mode is unaffected. A longer retention
than the actual lag avoids this schedule; the problem is accepting retention
as an unrestricted storage policy without protecting unapplied blocks. No
universal destruction of all recoverable data is claimed: an operator can
extract the retained datum, and a suitable block redelivery may heal it. The
ordinary startup/reconcile paths do not perform that repair.

**Fix/regression.** Clamp the pruning cutoff to the durable applied frontier
(for an exclusive upper bound, at most `applied + 1`) with a conservative result
on missing/error markers. Historical-retention policy must not remove future
execution inputs. Keep the necessary committed datum until execution is durable,
or make boot hole recovery consult the retained local committed tree. Gate a
worker before applying 998; dispatch/feed through 1000, prune with retention 1,
crash/reopen, and require replay to 1000 without a peer or operator repair.
Repeat with retention zero, a delayed worker, native and EVM bodies, and a
normal large-retention configuration.

## H01 — Historical F04 extension: the live caller ignores CoreWriter errors

At [native_executor.rs:8136](../../crates/torus-bridge/src/native_executor.rs#L8136),
`drain_core_writer` uses `CoreWriterQueue::drain(...)?`, propagating an error in
the return value without setting `ctx.fatal_error`. The live caller discards
that return value at [app.rs:2283](../../crates/torus-consensus/src/app.rs#L2283).
Its subsequent failure check tests only `fatal_error`, so a drain read error
does not trip that guard.

`CoreWriterQueue::drain` scans exactly the current-height prefix at
[precompiles.rs:1176](../../crates/torus-core/src/precompiles.rs#L1176).
If height 11's scan errors once, the rest of block 11 can flush successfully
and mark 11 applied while its queue rows remain. Height 12 and later drain
different prefixes; boot replay also skips 11. For a lockbox deposit whose
EVM value was burned at height 10, this strands the native credit despite
retaining its queue row. There is no automatic overdue-prefix catchup here.
Malformed queue values are also silently omitted and deleted by
[precompiles.rs:1183](../../crates/torus-core/src/precompiles.rs#L1183), but this
report does not claim a user can create such a malformed row through the
checked precompile serializer.

This revalidates historical EVM-FIND-12 at the production caller and adds a
precise lost-credit consequence to F04. It is **not a third new finding**.
Fail-stop on the returned typed infrastructure error before writing the
block marker; test a one-shot exact-height queue scan failure followed by
restart, and require the same credit/queue removal as a healthy execution.

## H02 — Historical EVM-FIND-06 residual: snapshot swap recovery is incomplete

The improved restore code preserves directories while copying, but
[snapshot.rs:203](../../crates/torus-state/src/snapshot.rs#L203) renames the
active data directory to `.old`, followed by a separate rename of `.restoring`
to the active path at line 205. Each rename is atomic; their pair is not one
atomic publication. A process termination between them leaves a valid old DB
and snapshot copy alongside an **absent active directory**.

On a subsequent ordinary startup without the one-shot restore argument,
[main.rs:523](../../crates/torus-node/src/main.rs#L523) creates that absent active
directory and opens a new RocksDB. With a genesis file supplied, the empty
accounts check at [main.rs:534](../../crates/torus-node/src/main.rs#L534) can
initialize it from genesis. There is no inspected startup guard that selects
or rejects the `.old`/`.restoring` state. Retrying restore instead first removes
both leftover directories at
[snapshot.rs:190](../../crates/torus-state/src/snapshot.rs#L190), before copying
again. The filesystem model demonstrates the missing-active-directory seam;
it does not start RocksDB or a validator.

This is a residual of historical EVM-FIND-06, whose original remove-before-copy
implementation was changed. It differs from F21's omitted-state commitment
coverage. It is recorded for follow-up, **not counted as wholly novel**. The
preserved directories make operator recovery possible; this report does not
claim that every copy is destroyed or that a validator actually equivocates.
Add an explicit interrupted-restore state machine before `create_dir_all`, or
use a single atomic pointer/exchange with durable publication semantics. Inject
termination before/after each rename and require ordinary startup to resume a
valid selected DB or fail clearly instead of opening fresh state.

## Negative results and bounded coverage

- **Normal cross-VM block commit:** EVM execution keeps a block-scoped native
  journal; it returns pending queue writes rather than writing them per
  transaction. The live app builds the EVM batch, seeds its native overlay
  from the bundle, and prefixes the combined native/applied-marker flush.
  The historical before-EVM/after-native split is not re-reported.
- **Caught inner reverts:** the inspector opens/closes native checkpoints for
  both CALL and CREATE frames at
  [executor.rs:39](../../crates/torus-evm/src/executor.rs#L39)–68; transaction
  scopes retain earlier successful transactions and undo later reverted
  writes at [backend.rs:1119](../../crates/torus-state/src/backend.rs#L1119).
  No additional reachable revert-leak defect was established from this code.
  Existing tests were read; no new runtime certification is claimed.
- **Lockbox authority and value legs:** writer precompiles reject delegate,
  callcode and static contexts at
  [precompile_provider.rs:135](../../crates/torus-evm/src/precompile_provider.rs#L135).
  Plain CALL uses `inputs.caller`; accepted payable deposits burn transferred
  wei through revm's frame journal, paired with delayed native credit. Native
  withdrawals retain margin checks, sender debits, chosen-recipient credits,
  and exact 8-to-18-decimal scaling. No new caller-substitution or ordinary
  two-leg mint was found beyond already reported fee/solvency findings.
- **CoreWriter due check versus in-flight parent:** the due check uses raw DB,
  but queue producers in the inspected live path are EVM precompiles, while
  EVM blocks are expressly excluded from pipelining at
  [app.rs:1736](../../crates/torus-consensus/src/app.rs#L1736). Their serial
  state/queue batch finishes before the next block executes. Native-only
  pipelined blocks only drain these rows. A healthy in-flight native parent
  therefore does not introduce invisible new queue work; this was not
  promoted as a race.
- **Speculative/legacy surfaces:** `execute_block_with_overlay` still bases
  precompile reads on the underlying DB, and public single-transaction
  execution can commit a journal before returning its EVM bundle. Caller
  searches found legacy bridge/test surfaces rather than the current CTE
  production proposal/commit path for these concerns. Without a reachable
  live caller, they were not counted as production branch-isolation defects.
- **Frontiers and failures:** worker ordering, rendezvous backpressure,
  logical versus durable markers, replay hole guards, manifest/body codecs,
  local feed reconciliation, and sync gap detection were traced. Existing
  F04/F05/F08 and F21 were not closed; native-only serial flush failures and
  marker errors remain within the prior F04 fault category. Receipt metadata
  visibility before state application also belongs to earlier frontier/RPC
  concerns; no additional finalized-value exploit was established here.

Run the companion models from the repository root:

```sh
python3 docs/audits/chain-cea1254-pass4-atomicity-models.py
```

All source guards and four models passed on the audited checkout. Production
regressions at the two new crash seams, real signature-bearing withdrawals,
default/release/oracle root routing, and interrupted RocksDB restore/startup
remain required before treating any candidate as reproduced or fixed.
