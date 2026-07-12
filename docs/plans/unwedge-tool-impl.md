# Implementation Plan: Unwedge Tool (consensus-state surgery)

## Design Decision

Option A from `docs/plans/unwedge-tool.md`, with one refinement: the CLI is a
**separate binary** `torus-unwedge` (`crates/torus-node/src/bin/unwedge.rs`), not a
subcommand on torus-node's flat clap `Parser` — existing systemd command lines are
untouchable during the recovery window, and friend2 can `cargo build --release
--bin torus-unwedge` without touching his running node binary (ETXTBSY lesson,
commit 3fd35e9).

Core surgery lives in `crates/hotstuff_rs/src/block_tree/recovery.rs` next to the
key layout and deletion primitives it reuses.

## Success Criteria

- `cargo test -p hotstuff_rs recovery` green; every test written RED-first.
- Full `cargo test -p hotstuff_rs -p torus-consensus -p torus-node` still green
  (torus-consensus crash-recovery flakes: confirm with `--test-threads 1`).
- `torus-unwedge --inspect` runs read-only against a LIVE node's data dir (verify
  on running seed) and reports: committed frontier (height+hash), uncommitted block
  count, hole count, recovery-PC candidates.
- `--apply` is idempotent, refuses bad PCs, hard-fails if post-apply verification
  finds any uncommitted block or dangling singleton.
- Devnet E2E (separate task): S458-recipe wedge repro → stop → apply on all nodes →
  restart → commits advance past former hole.

## Tasks

### Task 1: `recovery::inspect` + `WedgeReport`
- **Test first** (`recovery.rs` `#[cfg(test)]`, MemKV pattern copied from
  `hotstuff/pc_discard_regression_test.rs:75-110`): build a wedged tree in MemKV
  via `BlockTreeSingleton`/`BlockTreeWriteBatch` — committed chain h0..h3
  (`insert` + commit-side writes: `BLOCK_AT_HEIGHT`, `HIGHEST_COMMITTED_BLOCK`=h3),
  a floating uncommitted branch h5..h7 (h5's justify references a hash absent from
  BLOCKS = the hole), plus one uncommitted child h4' of h3 (justify = PC(h3), the
  harvestable candidate). Assert `inspect()` returns: frontier (3, hash_h3),
  uncommitted_count = 4, holes = 1 (missing parent hash listed),
  pc_candidates = [PC(h3) from h4'.justify].
- **Implementation**: `pub struct WedgeReport { committed_frontier: (BlockHeight,
  CryptoHash), committed_retained: u64, uncommitted: Vec<CryptoHash>, holes:
  Vec<CryptoHash /*missing parent*/>, pc_candidates: Vec<PhaseCertificate>,
  singletons: SingletonSnapshot }`. Enumerate via KV prefix iteration over
  `BLOCKS` (add an `iter_prefix` to `KVGet`? NO — pluggable KVGet has no
  iteration. Instead: enumerate committed set from `BLOCK_AT_HEIGHT`
  (`block_tree_pruned_height()`..`highest_committed_block_height()`), then walk
  uncommitted via `children()` lists from every committed block + from
  `NEWEST_BLOCK` backward via justify (`blocks_from_newest_to_committed`-style,
  tolerating gaps) + `SPECULATIVE_COMMITS` entries; union = uncommitted set.
  Every uncommitted block whose `block_justify(b).block` is absent from BLOCKS ⇒
  hole; whose `.block` == frontier hash ⇒ pc_candidate.)
  NOTE: if walk-based enumeration proves leaky for floating branches, add an
  optional `KVIterate` trait implemented only by `RocksKVStore` (prefix scan) and
  MemKV — decide in-task, test pins behavior either way.
- **Verify**: `cargo test -p hotstuff_rs recovery::tests::inspect_classifies_wedged_tree`
- **Depends on**: —

### Task 2: `recovery::prune_uncommitted`
- **Test first**: on the Task-1 tree, after
  `prune_uncommitted(&mut kv, &report, &recovery_pc)`: (a) all 4 uncommitted
  hashes gone from BLOCKS (all 5 field keys + data datums), (b) committed h0..h3
  intact incl. h3's BLOCK_JUSTIFY, (c) h3's children list == empty, (d)
  PENDING_APP_STATE_UPDATES / Pending-variant VALIDATOR_SET_UPDATES_STATUS gone
  for pruned hashes, Committed-variant on committed blocks untouched.
- **Implementation**: for each uncommitted hash reuse `delete_block`,
  `delete_children`, `delete_pending_app_state_updates`,
  `delete_block_validator_set_updates` (`internal.rs:1174-1191, 1318-1323`) via
  `BlockTreeWriteBatch`; then `set_children(frontier_hash, empty)`. Single
  atomic write batch.
- **Verify**: `cargo test -p hotstuff_rs recovery::tests::prune_removes_only_uncommitted`
- **Depends on**: 1

### Task 3: singleton resets + PC validation
- **Test first**: after apply — HIGHEST_PC == LOCKED_PC == PC(h3);
  NEWEST_BLOCK == hash_h3; LOCAL_TIP absent; HIGHEST_TC absent;
  SPECULATIVE_COMMITS empty; HIGHEST_VIEW_ENTERED / HIGHEST_VIEW_PHASE_VOTED /
  LAST_VOTED_PROPOSAL byte-identical to pre-surgery; LEADER_REPUTATION /
  EQUIVOCATION_EVIDENCE / BLOCK_TREE_PRUNED_HEIGHT / APP_FED_BLOCK_HEIGHT
  untouched. Negative tests: PC whose `.block != frontier` rejected; PC with
  phase Precommit rejected (must satisfy `is_block_justify()`: Generic|Decide);
  `pc.is_correct`-style signature check against COMMITTED_VALIDATOR_SET rejected
  on a PC signed by a different keypair set.
- **Implementation**: same write batch as Task 2 (one atomic apply). Validation
  before any write: `recovery_pc.block == frontier_hash`,
  `recovery_pc.is_block_justify()`, signature quorum verifies against
  `committed_validator_set()` (reuse `Certificate::is_correct` machinery).
- **Verify**: `cargo test -p hotstuff_rs recovery::tests::singletons_reset_and_safety_vars_kept`
- **Depends on**: 2

### Task 4: post-apply verification + idempotency
- **Test first**: `verify_after(kv)` returns Ok on the surgically-repaired tree;
  running the full apply twice is a no-op second time (reports 0 pruned); a
  deliberately corrupted repair (delete frontier's BLOCKS entry) makes
  `verify_after` return Err.
- **Implementation**: `verify_after` = re-run inspect, assert uncommitted==0,
  holes==0, HIGHEST_PC.block==frontier present in BLOCKS, boot-critical
  singletons deserialize (COMMITTED_VALIDATOR_SET, HIGHEST_COMMITTED_BLOCK).
  Apply = inspect → validate → prune+reset → verify_after, else Err (no partial
  state possible — single write batch).
- **Verify**: `cargo test -p hotstuff_rs recovery::tests::{verify_after_ok,apply_is_idempotent,verify_detects_corruption}`
- **Depends on**: 3

### Task 5: `torus-unwedge` binary
- **Test first**: `cargo build --release --bin torus-unwedge` + bash smoke on a
  temp copy of a devnet data dir: `--inspect --json` exits 0 and emits parseable
  JSON; `--apply` without `--yes` prints plan and exits non-zero; `--export-pc`
  writes a file that `--apply --pc-file` on a second dir accepts (Borsh
  roundtrip).
- **Implementation**: `crates/torus-node/src/bin/unwedge.rs`, clap Parser:
  `--data-dir <path>` (required), `--inspect` (default) | `--export-pc <file>` |
  `--apply --pc-file <file> [--yes]`, `--json`. Inspect/export open RocksDB
  read-only (`DB::open_cf_descriptors_read_only`, pattern in
  `torus-state/src/snapshot.rs:123-128` — works against a live primary); apply
  opens exclusive via `StateDb::open`/`RocksKVStore::new(state_db.db_arc())`
  (RocksDB LOCK enforces node-stopped). PC file = Borsh bytes of
  PhaseCertificate.
- **Verify**: smoke script above (add `devnet/unwedge-smoke.sh`)
- **Depends on**: 4

### Task 6: full-suite regression + clippy
- **Test first**: n/a (gate)
- **Implementation/Verify**: `cargo test -p hotstuff_rs -p torus-consensus -p
  torus-node` (crash-recovery flakes: re-run `--test-threads 1`), `cargo clippy
  -p hotstuff_rs -p torus-node -- -D warnings` (match repo's lint posture).
- **Depends on**: 5

## Verification (end-to-end)

Devnet proof (tracked separately as session task #4): reproduce S458 wedge with
the tiny-action/no-gossip recipe, confirm the `REFUSING to commit across a hole`
signature, run `torus-unwedge --inspect` on all devnet nodes live, stop all, apply
everywhere (PC harvested from the authoring node), restart together, assert
commits advance past the former hole and a short bench runs clean.

## Rollback

- Apply is a single atomic RocksDB write batch preceded by validation; a failed
  apply writes nothing.
- Testnet window: optional `cp -r`/checkpoint of each data dir before apply is
  the true rollback (disk permitting; seed data ~900MB).
- The tool never touches executed state, other CFs, keys, or genesis.
