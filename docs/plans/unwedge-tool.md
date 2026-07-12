# Design: Unwedge Tool — consensus-state surgery for QC'd-poison-ancestry wedges

Session 459 (2026-07-12). Follows the S458 testnet wedge postmortem (commit f24e334,
forensics devnet/forensics-s458-wedge/). Diagnosis memory: "S459 WEDGE DIAGNOSIS FINAL".

## Problem

Testnet is permanently wedged at committed height 761,993. Two session-registration
block headers were QC'd into the ancestry during the direct-to-leader canary; their
bodies existed only in val2's mempool (T2.2 DA-mirror buffer never flushed at tiny
volume, gossip pre-spread was off) and were destroyed by val2's restart. Every node's
`cf_consensus_meta` now persists an uncommitted tree above 761,993 with a hole in it:

- `commit()` refuses across the hole forever (`internal.rs:719-744`, 47k+ refusals
  logged on seed): lowest uncommitted block's `justify.block` ≠ `HIGHEST_COMMITTED_BLOCK`.
- Block-sync can never backfill: the missing bodies exist nowhere in the universe.
- Tip keeps extending empty blocks (~768k heights), views race (~787-790k).
- No restart, rebuild, or resync can fix this. Snapshots are not wired into the node
  (SnapshotManager has no production caller; no snapshot dirs on disk).

All three nodes verified in identical macro state (seed + 18c: eth_blockNumber
0xba089 = 761,993, views 787-790k, validator_set_size gauge 0; val2 per friend2 relay:
761,991 committed, view 786k+).

## Context (from memory + exploration)

Everything below verified by direct read of `crates/hotstuff_rs/src/` (S459 explore
agent report; key file:line cites inline).

**Key layout** (`block_tree/variables.rs`): 24 variables in `cf_consensus_meta`.
Blocks stored per-hash with 5 field keys (`BLOCKS + hash + {HEIGHT,JUSTIFY,DATA_HASH,
DATA_LEN,DATA[i]}`). `BLOCK_AT_HEIGHT` maps height→hash **for committed blocks only**
— it is the authoritative committed/uncommitted discriminator.

**Deletion primitives already exist** and run on every commit (`delete_siblings` →
`delete_branch` → `blocks_in_branch` DFS + `delete_block` / `delete_children` /
`delete_pending_app_state_updates` / `delete_block_validator_set_updates`,
`internal.rs:826-876, 1174-1191`). The surgery is these primitives aimed at the
uncommitted remainder instead of committed siblings.

**Startup walks no blocks**: `Replica::start` reads only singletons
(`committed_validator_set` — panics if absent; `highest_view_with_progress` =
max(HIGHEST_VIEW_ENTERED, HIGHEST_PC.view, HIGHEST_TC.view)). A pruned tree boots
fine **provided** singletons deserialize and the `BLOCKS` entry for
`HIGHEST_COMMITTED_BLOCK` remains (needed by the commit-hole check itself).

**Recovery PC**: to propose/vote again from the committed tip, `HIGHEST_PC` (and
`LOCKED_PC`) must be a `PhaseCertificate` whose `.block` == committed tip hash — legal
per `pc_to_lock`/`block_to_commit` (`invariants.rs:468-538, 565-669`). Such a PC is
persisted as the `BLOCK_JUSTIFY` of any block whose parent is the committed tip.
CRITICAL NUANCE: on seed the hole means no such block may exist locally (the poison
header at 761,994 is exactly what's missing from its `BLOCKS`). But a PC is
validator-signed public data — harvest it from whichever node has that header (val2
authored the poison blocks; 761,993 committed everywhere, which required a QC'd
761,994 to exist at commit time) and import it on the others. Borsh-serialize to a
file, verify `.block == tip && .phase ∈ {Generic, Decide}` on import
(`is_block_justify()`, `hotstuff/types.rs:217-219`).

**Singleton dispositions after prune** (all cites from explore report):

| Variable | Action | Why |
|---|---|---|
| HIGHEST_PC [9] | ← recovery PC(761,993) | must reference a live block |
| LOCKED_PC [7] | ← recovery PC(761,993) | same; legal per pc_to_lock |
| NEWEST_BLOCK [11] | ← committed tip hash | sync-server walk starts here; deleted target ⇒ silently empty speculative serves (`public.rs:100-109`) |
| LOCAL_TIP [17] | delete (None) | pure pacemaker hint; `Option` is the legal resting state, readers degrade gracefully |
| SPECULATIVE_COMMITS [19] | clear | hash-equality reads only, no deref — clearing is hygiene, stale entries would be confusing forensics |
| HIGHEST_TC [12] | delete | its `high_tip` references deleted 768k blocks; reproposal path would try to repropose a deleted tip and could re-wedge. View continuity is preserved by HIGHEST_VIEW_ENTERED (≈ same view) |
| HIGHEST_VIEW_ENTERED [8], HIGHEST_VIEW_PHASE_VOTED [16], LAST_VOTED_PROPOSAL [18] | KEEP | per-node vote-safety — never lower; equality-only reads, no deref |
| HIGHEST_COMMITTED_BLOCK [10], BLOCK_AT_HEIGHT, committed BLOCKS, COMMITTED_* , PREVIOUS_VALIDATOR_SET, VS flags | untouched | committed side is clean |
| LEADER_REPUTATION [20], EQUIVOCATION_EVIDENCE [21], BLOCK_TREE_PRUNED_HEIGHT [22], APP_FED_BLOCK_HEIGHT [23] | untouched | orthogonal; app feed resumes at-least-once from 761,993 |

**Prune set** = every hash in `BLOCKS` not equal to `BLOCK_AT_HEIGHT[h]` for any
retained h. Cannot reuse `blocks_in_branch` DFS alone: the hole means uncommitted
blocks form ≥1 floating branch not connected to the committed tip, and children-list
walks from the tip won't reach them. Enumerate by full `BLOCKS` prefix scan instead
(committed set is the filter), delete each with the existing per-block primitives,
then reset the committed tip's `BLOCK_TO_CHILDREN` to empty.

**Why all-nodes-same-window**: uncommitted branches are served over block-sync
(`blocks_from_newest_to_committed`, `public.rs:89-128`) and advertised; any node
retaining the branch re-poisons freshly cleaned peers. All 3 must be stopped, cleaned,
then restarted together.

## Options

### Option A (recommended): `recovery` module in hotstuff_rs + `torus-node unwedge` subcommand
Surgery logic lives in `crates/hotstuff_rs/src/block_tree/recovery.rs` (or
`accessors/recovery.rs`) where the key layout, `BlockTreeWriteBatch`, and deletion
primitives are already in scope — mirroring the S391 pruner precedent. Public API:
`inspect(kv) -> WedgeReport` (read-only: committed frontier, uncommitted count,
holes, recovery-PC candidates) and `prune_uncommitted(kv, recovery_pc) -> SurgeryReport`.
torus-node grows a `unwedge` subcommand: `--inspect [--json]`,
`--export-pc <file>`, `--apply --pc-file <file> [--dry-run]`. Inspect can open the
DB read-only/secondary → runnable against LIVE nodes for pre-window recon; apply
requires exclusive open (node stopped) and RocksDB LOCK enforces that for free.
- Files: `crates/hotstuff_rs/src/block_tree/recovery.rs` (+ mod wiring),
  `crates/torus-node/src/main.rs` (subcommand), tests in hotstuff_rs.
- Pros: reuses the exact production key layout + primitives; testable against a
  synthetically wedged in-memory/temp-db tree; inspect mode doubles as forensics.
- Cons: touches vendored hotstuff_rs (acceptable — already heavily modified).
- Effort: Medium. Risk: Low-Medium (mitigated by dry-run + devnet proof + invariant
  verification pass after apply).

### Option B (rejected): standalone bin parsing raw cf_consensus_meta keys
No hotstuff_rs changes; a separate crate re-implements key encodings and Borsh types.
Duplicates layout knowledge outside the library (the exact anti-pattern S391 rejected
for the pruner), silently rots when variables change, and cannot reuse
`delete_block`/`BlockTreeWriteBatch`. Rejected.

### Option C (rejected): full cf_consensus_meta re-initialization from executed state
Drop the CF and `Replica::initialize` with app-state/validator-set snapshotted from
the executed side. Rejected: `Replica::initialize` targets genesis (hotstuff heights
restart at 0 while EVM state is at 761,993 — TorusBlock height continuity, genesis
guard, and app-feed anchoring all break in unexplored ways), and the committed
validator set / VS-update flags would need manual reconstruction. Far larger blast
radius for no benefit.

## Recommendation

Option A. The one genuine unknown is which nodes hold a harvestable PC(761,993);
`--inspect` (read-only, safe on live nodes) answers it before anything is touched —
run on seed and 18c immediately once built, and friend2 runs one command on val2.
If — worst case — NO node holds one, escalate to a decided fallback (roll committed
frontier back one block **with** a matching execution-side rollback) as a separate
design; do not improvise it inside this tool.

## Procedure (testnet window, after devnet proof)

1. Pre-window recon: `unwedge --inspect` on all 3 (read-only, nodes still running).
   Confirm identical committed frontier hash; locate recovery-PC source.
2. `--export-pc` on the source node → tiny file → distribute (user relays to/from
   friend2).
3. Stop all 3 nodes. No restarts until step 6.
4. `unwedge --apply --pc-file pc-761993.bin --dry-run` on each; compare reports
   (same frontier, sane prune counts). Then `--apply` for real. Tool re-runs its
   inspect pass post-apply and hard-fails on any invariant violation.
5. (Optional, cheap insurance) `cp -r` / rocksdb checkpoint of each data dir before
   step 4 if disk allows.
6. Restart all 3 together (existing binaries: seed/18c 1e2fe8c5, val2 ae80be57 —
   NO binary changes in this window). Verify: commits advance past 761,993,
   validator_set_size returns 3, empty-proposal catch-up gate clears, then friend2's
   pending rate-ramp resumes as planned.

## Open Questions

1. Which nodes hold PC(761,993)? (answered by inspect; val2 near-certain)
2. Does `--inspect` need rocksdb secondary-open plumbing in torus-state, or is
   read-only open sufficient given the running node holds the primary? (RocksDB
   read-only open works on a live DB dir; verify against running seed.)
3. Devnet repro: reuse S458 recipe (no-gossip flip + single tiny action at low
   volume so the DA-mirror buffer never flushes) — confirm it produces the same
   commit-hole signature before trusting the proof.
