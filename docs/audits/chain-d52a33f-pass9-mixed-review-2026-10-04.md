# Chain audit pass 9 — two Sol 6.1 and two Astra reviews

Date: 2026-10-04. Branch: `perf/item6-phase1`. Revision:
`d52a33f51038105a8d0e102fc2e1a5c51f43fb09`.

This pass examines ordinary transaction inclusion, trade-history recovery,
snapshot/archive behavior and collateral accounting across the project.
The checkout is unchanged from pass 8; no new fetch or source update was part
of this pass. Prior F01–F37 and historical supplements are deduplicated.
Interrupted pass-five scopes were not resumed. No production source, keys,
live-chain state, installed software or Git history was changed.

**Result: all four reviews completed and saved reports.** Three additional P2
source-supported candidates are recorded below; the collateral review promoted
no additional defect. Per the [audit policy](README.md),
runtime regressions are required before treating them as reproduced defects.
Cargo/rustc are unavailable; tests were read, not run.

| Model | Scope | Report |
| --- | --- | --- |
| GPT-6.1-sol | Transaction lifecycle and mempool accounting | [Transaction review](chain-d52a33f-pass9-sol-transactions-2026-10-04.md) |
| GPT-6.1-sol | Trade history, streams and explorer projections | [History review](chain-d52a33f-pass9-sol-history-2026-10-04.md) |
| GPT-6-astra | Snapshot/archive lifecycle and clean reopening | [Storage review](chain-d52a33f-pass9-astra-storage-2026-10-04.md) |
| GPT-6-astra | Collateral accounting and normal settlement lifecycle | [Collateral review](chain-d52a33f-pass9-astra-collateral-2026-10-04.md) |

## Additional candidates

| ID | Priority | Ordinary failure and limits | Evidence |
| --- | --- | --- | --- |
| F38 | P2 | Restarting a previously pruned DB with retention disabled loses RPC knowledge of the durable pruning frontier. An old log query can return successful empty/partial data instead of DataPruned. This does not claim archive mode should restore deleted history. | [Storage S01](chain-d52a33f-pass9-astra-storage-2026-10-04.md#s01--p2-archive-restart-forgets-previously-pruned-history-in-rpc) |
| F39 | P2 | EVM byte admission checks gross incoming bytes before replacement/eviction credit and before taking the pool lock. A fee bump whose net retained bytes fit can be rejected; simultaneous ordinary admissions can pass stale checks. No runtime overshoot or RSS measurement is claimed. | [Admission](../../crates/torus-mempool/src/lib.rs#L417), [replacement](../../crates/torus-mempool/src/evm_pool.rs#L139) |
| F40 | P2 | Documented user-fill reconnect backfill cannot reach older same-market entries beyond getUserTrades' 1,000-entry cap. It has no block/trade cursor. The underlying records can still exist; market-wide RPCs do not expose user ownership/role. | [User query](../../crates/torus-rpc/src/torus.rs#L1913), [recovery contract](../api/streams.md#L133) |

F38 results from [RPC construction](../../crates/torus-rpc/src/lib.rs#L317)
initializing the frontier to zero, while only
[StatePruner construction](../../crates/torus-state/src/pruner.rs#L117) loads
persisted metadata. The [archive startup branch](../../crates/torus-node/src/main.rs#L1169)
does not construct a pruner. All removed blocks can be fully applied, so this
is distinct from F31's removal of unapplied replay inputs. Initialize historical
availability before serving RPC, independently of future pruning policy.
Regress real prune → clean close/reopen → RPC without pruner, retaining the
same chain settings and requiring the same DataPruned response.

F39 concerns the configured retained-serialized-byte budget, not a measured
bound on process memory. With usage 900, cap 1,000, old entry 200 and replacement
200, gross admission checks 1,100 even though net retention would remain 900.
The source already credits freed bytes after insertion; the guard must assess
the intended net mutation under the same lock without discarding the old entry
if validation/admission fails. Regress fitting and oversized replacements,
eviction, and coordinated ordinary concurrent admissions. This is separate from
the previously repaired missing freed-byte decrement and unbounded ingress queues.

F40 is specifically a user-history recovery limitation. The market range
endpoint also caps responses, but public
[getBlockTrades](../../crates/torus-rpc/src/torus.rs#L1636) provides an uncapped
per-block market-wide fallback. Thus claiming all market trades are inaccessible
would be wrong. Those market rows omit ownership and role, so they cannot replace
user-history rows. Add a stable `(block, trade, role)` continuation contract and
regress more than 1,000 same-market own fills across a reconnect, with all pages
matching the retained user rows exactly once. Stream-only extra fields remain
a separately documented limitation.

## Existing findings and coverage qualifications

- **F26 also has an ordinary history-writer timing consequence.** The applied
  state marker can be visible before the background writer persists trade rows.
  Explorer then accepts an empty or partial successful trade response and advances
  its cursor; same-hash retries skip repair. A later successful history write
  cannot repair the missing candle updates. The history report traces this as
  an extension of F26, not another counted root cause. Regression needs to
  coordinate the real writer, RPC and explorer, including delayed chunks.
- **Native and EVM status fields have different meanings.** Admission, dispatch,
  handler success, EVM receipt status and delayed CoreWriter completion must be
  checked separately. Existing tests cover useful parts of these contracts;
  their use of manually seeded status records limits end-to-end claims.
- **Intact snapshot copying and verification are distinct.** Current snapshot
  copies include all CF files. Production restore is wired, while periodic
  snapshot export remains a documented library/test feature. The manager tests
  chiefly count directories rather than verifying every retained snapshot.
- **Some tests really close and reopen RocksDB; others reconstruct contexts.**
  The storage report identifies both precisely. The prune-metadata restart test
  constructs another pruner, which hides F38's archive branch. Running-hash
  context reconstruction against a shared open DB does not cover node startup.
- **Storage-mode compatibility has explicit limits.** Readers recognize the
  persisted layout, but changing the consensus-visible writer mode on an existing
  chain is documented as unsupported. Lack of arbitrary schema migration coverage
  is not counted as another failure.
- **Isolated-position helpers do not establish a public allocation lifecycle.**
  The current action schema has no isolated-margin allocation action, and scalar,
  cached and parallel production fills select Cross. A manually seeded isolated
  close concern was not promoted without ordinary reachability. Existing exact
  collateral, queued-credit, PnL/release and tombstone tests retain their value;
  a complete real-fill-to-withdrawal/reopen test remains a prior recommendation.

## Saved discoveries

| Finding | Torus issue ID |
| --- | --- |
| F38 | `184a5fc1-7ea2-4dc4-9499-d45630e4e11e` |
| F39 | `15081875-c55f-4065-b939-97ed5afbab87` |
| F40 | `cc29bf5b-4dfe-436d-8190-6d690fa3459e` |

All three discovery writes were acknowledged as saved. Shared-state sync was
skipped because no shared project-state file exists. Parent review read and
reconciled all four reports and checked the principal source paths of F38–F40,
including correcting the market-history fallback claim before promotion.
Local links and source line bounds were checked; report whitespace checks passed.
HEAD remains `d52a33f`, tracked/staged diffs are empty, and five new pass-nine
Markdown files accompany the 29 preserved earlier audit artifacts. Rust tests
were not run. No production finding has been resolved.
