//! RH4: durable DA body GC policy: how long a committed body must stay, and
//! how much one GC pass may do.
//!
//! Committed native-action bodies live in the consensus DB's
//! `cf_native_pending`, and with shard custody on (`TORUS_SHARD_CUSTODY=1`,
//! off by default since s128) as erasure shards in `cf_native_shards`. Before RH4 nothing deleted them.
//! The GC itself is [`torus_state::NativeDaStore::gc_committed`], run once per
//! dispatched committed height from `TorusApp::dispatch_to_exec`.
//!
//! # Why bodies follow the block tree
//!
//! A peer that falls behind catches up by block sync, which ships hotstuff
//! blocks verbatim: compact datums that carry only the action hashes. It then
//! pulls the bodies by hash (`/torus/native-da/1.0`, or `k` shards). Sync
//! servers and pull targets are validators only, and there is no state sync.
//! So a body must stay as long as its block can still be served by block sync:
//! otherwise a peer syncing that block finds the body nowhere, opens an
//! execution hole and fail-stops after the heal budget. A validator serves the
//! blocks its block tree keeps: all of them on an archive node (no
//! `--retention-blocks`), the last `max(N, 1000)` heights with
//! `--retention-blocks N` (hotstuff `prune_old_committed_blocks_with`). Hence
//! two bounds, both must pass before a body goes:
//!
//! - the window, in dispatched heights: block-tree retention +
//!   [`DA_GC_MARGIN_BLOCKS`]; `TORUS_DA_BODY_RETENTION=N` overrides it
//!   (`0` = off, floor [`MIN_DA_BODY_RETENTION`]),
//! - the block tree itself (Codex P1): the body of height `h` goes only once
//!   the tree's durable prune pointer (the lowest height it still holds) is
//!   at least `h + DA_GC_MARGIN_BLOCKS` ([`keep_from_for_tree_frontier`]).
//!   The window alone runs ahead of the tree: the pruner removes at most 64
//!   heights per commit, so after `--retention-blocks` drops (restart with a
//!   smaller window) the tree serves old blocks for many commits, and rows
//!   are sparse (an empty block records none), so one GC pass can reach far.
//!
//! An archive node never prunes its tree, so it runs no GC
//! ([`resolve_da_body_retention`] = `None`, even with the env set) and stays
//! a full sync source. A window below the block-tree window no longer
//! shortens anything: the tree bound keeps the bodies.
//!
//! The pointer is read from the consensus DB, the DB the DA store writes to:
//! one WAL, recovered as a prefix after a crash, so a GC batch that deleted a
//! body never survives without the prune batch that allowed it.
//!
//! The window counts DISPATCHED heights (the block's body row is durable in
//! the state DB's `CF_BLOCK_BODIES`), not consensus commits: a committed block
//! still parked behind an execution hole is re-read from the DA store, so its
//! bodies are not even recorded for GC until it is dispatched.
//!
//! # Never delete while the bookkeeping is behind
//!
//! A body re-included by a later block is pinned by that block's GC record
//! (the record moves the hash out of the earlier row). Dispatch persists the
//! block body in the state DB, then runs the GC pass in the consensus DB: a
//! crash in between, or a failed GC write, leaves a dispatched height without
//! its record. Boot replay then applies the block and skips its re-delivery,
//! so no pass records it live, and the earlier row would delete the body
//! while the tree still serves the re-including block (Codex P1, pre-merge
//! review). Hence the cursor rule:
//!
//! - The GC state holds a "recorded through" cursor, written in the same
//!   batch as the records it covers: a crash or a failed write only leaves it
//!   behind.
//! - Every pass (boot or steady state) first checks it. Behind the dispatched
//!   height, the missing heights are back-filled from the block tree
//!   ([`BlockTreeHistory`]: the committed blocks it still holds, at least the
//!   window while unpruned), re-inclusion pins included, before anything is
//!   deleted. Records are idempotent, so a re-write is harmless.
//! - A height the tree cannot supply (missing block, read error) stops the
//!   back-fill: the pass deletes NOTHING, logs, counts
//!   `torus_da_gc_held_passes`, and the next pass retries. A failed record is
//!   therefore never dropped: the cursor stays behind until it lands.
//! - Heights below the tree's prune pointer are skipped (no longer served),
//!   but deletion waits until the last skipped height is itself due (window
//!   and tree bound): one of them may have re-included an older row's body.
//!   Such a gap means the GC was off or failing for longer than the tree
//!   window; the first-inclusion bodies of the skipped heights stay for good.
//!
//! A fresh store (no GC state, so no row a lost pin could matter for)
//! establishes the cursor at its first recorded height. A store a pre-cursor
//! binary wrote (owner, s128) may already have lost a pin that way: its
//! cursor starts where the tree's retained range starts (the prune pointer,
//! never below the GC frontier), so the normal back-fill re-reads every
//! height the tree still serves (256 per pass, deletion held until it caught
//! up; heights the tree already pruned held as above). Its replays still
//! record nothing, and no pass deletes before the cursor exists. With the GC
//! off (archive node, `TORUS_DA_BODY_RETENTION=0`) no pass runs and nothing
//! is written.

use hotstuff_rs::block_tree::accessors::public::BlockTreeCamera;
use hotstuff_rs::block_tree::variables::BLOCK_TREE_PRUNED_HEIGHT;
use hotstuff_rs::types::data_types::BlockHeight;
use torus_state::cf::CF_CONSENSUS_META;
use torus_state::{DaGcHistory, DaGcLimits, StateDb, StateError};
use torus_types::{compute_action_hash, CompactBlock, SignedNativeAction, TorusBlock, B256};

use crate::kv_store::RocksKVStore;

/// Blocks a body outlives its block in the block tree by: the window derived
/// from `--retention-blocks` is the block-tree retention plus this, and a body
/// goes only once the tree's prune pointer is this far past its height. It
/// covers block-sync responses in flight when the tree prunes (in steady state
/// the pointer moves one height per commit; while the pruner drains a backlog,
/// up to 64).
pub const DA_GC_MARGIN_BLOCKS: u64 = 128;

/// Smallest window `TORUS_DA_BODY_RETENTION` may set (the July RH4 default):
/// well past the push-race and normal follower lag.
pub const MIN_DA_BODY_RETENTION: u64 = 128;

/// Per-pass bounds on the consensus thread. Steady state is one row per pass
/// (one block: at most the proposer cap of 400 actions by default, each one
/// body key plus one key per shard). The bounds only bite on a backlog (a
/// smaller window after a restart, or a burst of dispatches), which then
/// drains at up to 7 heights per dispatch.
pub const DA_GC_LIMITS: DaGcLimits = DaGcLimits {
    max_rows: 8,
    max_deletes: 16_384,
};

/// The DA body GC window in blocks, `None` = GC off.
///
/// `env` is `TORUS_DA_BODY_RETENTION`; `block_tree_retention` is the active
/// hotstuff block-tree window (`None` = archive: GC off whatever `env` says,
/// the tree never prunes, so no body could ever go). See the module docs. An
/// unparsable `env` falls back to the derived window, so a typo neither turns
/// the GC on for an archive node nor off for a pruning one.
pub fn resolve_da_body_retention(
    env: Option<&str>,
    block_tree_retention: Option<u64>,
) -> Option<u64> {
    let tree = block_tree_retention?;
    match env.map(str::trim).map(str::parse::<u64>) {
        Some(Ok(0)) => None,
        Some(Ok(window)) => Some(window.max(MIN_DA_BODY_RETENTION)),
        Some(Err(_)) | None => Some(tree.saturating_add(DA_GC_MARGIN_BLOCKS)),
    }
}

/// The block tree's durable prune pointer in `db` (the lowest committed height
/// the tree still holds; `None` = never pruned), as hotstuff's pruner persists
/// it (`BLOCK_TREE_PRUNED_HEIGHT` in `cf_consensus_meta`, a borsh `BlockHeight`:
/// 8 bytes little-endian).
pub fn read_block_tree_frontier(db: &StateDb) -> Result<Option<u64>, StateError> {
    let Some(bytes) = db.get_cf_raw(CF_CONSENSUS_META, &BLOCK_TREE_PRUNED_HEIGHT)? else {
        return Ok(None);
    };
    let le: [u8; 8] = bytes.as_slice().try_into().map_err(|_| {
        StateError::InvalidData(format!(
            "block-tree prune pointer is {} bytes, expected 8",
            bytes.len()
        ))
    })?;
    Ok(Some(u64::from_le_bytes(le)))
}

/// The `keep_from` bound of a GC pass ([`torus_state::NativeDaStore::gc_committed`])
/// for the tree's prune pointer `frontier`: the body of height `h` may go
/// once `h + DA_GC_MARGIN_BLOCKS <= frontier`. `None` (never pruned) = `0`,
/// nothing goes. With the derived window this matches it exactly in steady
/// state (pointer = tip - tree retention), so it only bites while the tree lags.
pub fn keep_from_for_tree_frontier(frontier: Option<u64>) -> u64 {
    frontier
        .and_then(|f| f.checked_sub(DA_GC_MARGIN_BLOCKS))
        .map_or(0, |f| f.saturating_add(1))
}

/// RH4 DA body GC settings of a node ([`crate::app::TorusApp::set_da_body_gc`]).
#[derive(Clone)]
pub struct DaBodyGc {
    /// The window in dispatched heights ([`resolve_da_body_retention`]).
    pub retention: u64,
    /// The DB holding the hotstuff block tree, whose prune pointer bounds the
    /// deletions. Must be the DB the DA store writes to (the node's consensus
    /// DB): see the module docs on crash ordering.
    pub block_tree_db: StateDb,
}

impl DaBodyGc {
    /// This pass's `keep_from`, from the tree's durable prune pointer.
    pub fn keep_from(&self) -> Result<u64, StateError> {
        read_block_tree_frontier(&self.block_tree_db).map(keep_from_for_tree_frontier)
    }
}

/// The native actions the hotstuff block tree in `db` holds for committed
/// `height`, decoded like `TorusApp::on_committed_block` decodes the datum:
/// `(hashes, Some(actions))` for a full datum, `(hashes, None)` for a compact
/// one. A block without a datum or with an undecodable one carries none (the
/// commit path executes nothing from it). `None` = the tree has no committed
/// block at `height`.
#[allow(clippy::type_complexity)]
pub fn committed_native_actions_in_tree(
    db: &StateDb,
    height: u64,
) -> Result<Option<(Vec<B256>, Option<Vec<SignedNativeAction>>)>, StateError> {
    let tree_err = |e| StateError::InvalidData(format!("block tree read at {height}: {e:?}"));
    let camera = BlockTreeCamera::new(RocksKVStore::new(db.db_arc()));
    let snapshot = camera.snapshot();
    let Some(block) = snapshot
        .block_at_height(BlockHeight::new(height))
        .map_err(tree_err)?
    else {
        return Ok(None);
    };
    let Some(data) = snapshot.block_data(&block).map_err(tree_err)? else {
        return Ok(None);
    };
    let Some(datum) = data.vec().first() else {
        return Ok(Some((Vec::new(), None)));
    };
    let bytes = datum.bytes();
    if let Ok(full) = bincode::deserialize::<TorusBlock>(bytes) {
        let hashes = full
            .native_actions
            .iter()
            .map(compute_action_hash)
            .collect();
        return Ok(Some((hashes, Some(full.native_actions))));
    }
    if let Ok(compact) = bincode::deserialize::<CompactBlock>(bytes) {
        return Ok(Some((compact.native_action_hashes, None)));
    }
    tracing::warn!(
        height,
        "DA body GC back-fill: undecodable committed datum, no actions"
    );
    Ok(Some((Vec::new(), None)))
}

/// [`BlockTreeHistory`]'s restore hook: `(height, hashes, full datum's
/// actions)`.
pub type RestoreBodies<'a> = &'a dyn Fn(u64, &[B256], Option<&[SignedNativeAction]>);

/// The block tree as the GC's [`DaGcHistory`] (Codex P1, pre-merge review):
/// heights the GC has no record for (a crash between dispatch's body write
/// and its GC pass, a failed record write) are re-read from the committed
/// blocks the tree still holds. The tree keeps at least the GC window's
/// heights while it is not pruned past them, and its prune pointer says which
/// heights it no longer serves.
pub struct BlockTreeHistory<'a> {
    db: &'a StateDb,
    restore: RestoreBodies<'a>,
}

impl<'a> BlockTreeHistory<'a> {
    /// `db` holds the block tree. `restore(height, hashes, actions)` runs for
    /// each back-filled height before its record (`actions`: the full
    /// datum's, if it is one), so a body the block references but the DA
    /// store lost is put back, as dispatch does.
    pub fn new(db: &'a StateDb, restore: RestoreBodies<'a>) -> Self {
        Self { db, restore }
    }
}

impl DaGcHistory for BlockTreeHistory<'_> {
    fn pruned_below(&self) -> Result<u64, StateError> {
        Ok(read_block_tree_frontier(self.db)?.unwrap_or(0))
    }

    fn committed_hashes(&self, height: u64) -> Result<Option<Vec<B256>>, StateError> {
        let Some((hashes, actions)) = committed_native_actions_in_tree(self.db, height)? else {
            return Ok(None);
        };
        (self.restore)(height, &hashes, actions.as_deref());
        Ok(Some(hashes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_state::{NativeDaStore, StateDb};
    use torus_types::{compute_action_hash, SignedNativeAction};

    #[test]
    fn archive_node_keeps_every_body_by_default() {
        assert_eq!(resolve_da_body_retention(None, None), None);
    }

    #[test]
    fn pruning_node_derives_the_window_from_the_block_tree() {
        assert_eq!(resolve_da_body_retention(None, Some(1000)), Some(1128));
        assert_eq!(
            resolve_da_body_retention(None, Some(100_000)),
            Some(100_128)
        );
        assert_eq!(
            resolve_da_body_retention(None, Some(u64::MAX)),
            Some(u64::MAX)
        );
    }

    #[test]
    fn env_overrides_the_derived_window() {
        assert_eq!(
            resolve_da_body_retention(Some("0"), Some(1000)),
            None,
            "0 = off"
        );
        assert_eq!(
            resolve_da_body_retention(Some(" 5000 "), None),
            None,
            "an archive tree never prunes: no body could ever go, so no GC (Codex P1)"
        );
        assert_eq!(
            resolve_da_body_retention(Some(" 5000 "), Some(1000)),
            Some(5000)
        );
        assert_eq!(
            resolve_da_body_retention(Some("5000"), Some(1000)),
            Some(5000)
        );
        assert_eq!(
            resolve_da_body_retention(Some("5"), Some(1000)),
            Some(MIN_DA_BODY_RETENTION),
            "explicit windows are floored"
        );
    }

    #[test]
    fn unparsable_env_never_silently_disables_or_enables_gc() {
        for raw in ["", "  ", "off", "-1", "12k"] {
            assert_eq!(
                resolve_da_body_retention(Some(raw), Some(1000)),
                Some(1128),
                "{raw:?}"
            );
            assert_eq!(resolve_da_body_retention(Some(raw), None), None, "{raw:?}");
        }
    }

    fn action(nonce: u64) -> SignedNativeAction {
        use torus_types::{ActionSignature, NativeAction, Signature};
        SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce,
            signature: ActionSignature::Eip712(Signature {
                v: 27,
                r: [0u8; 32],
                s: [0u8; 32],
            }),
        }
    }

    /// hotstuff `prune_old_committed_blocks_with` keeps heights
    /// `highest - retention ..= highest` (block_tree/accessors/internal.rs).
    fn tree_serves(height: u64, highest: u64, retention: u64) -> bool {
        highest <= retention || height >= highest - retention
    }

    /// The pruner's pointer once it kept up: `highest - retention`, `None`
    /// while the chain is inside the window.
    fn tree_frontier(highest: u64, retention: u64) -> Option<u64> {
        highest.checked_sub(retention).filter(|&f| f > 0)
    }

    /// A lagging peer can always finish what block sync hands it: at every
    /// tip, every block the block tree still serves has its body in the DA
    /// store (served by `/torus/native-da/1.0`), while bodies older than the
    /// window are gone: with the tree keeping up, the tree bound and the
    /// window agree exactly. Uses the smallest block-tree window hotstuff allows.
    #[test]
    fn every_block_the_tree_serves_keeps_its_body() {
        let tree = hotstuff_rs::block_tree::MIN_BLOCK_TREE_RETENTION;
        let window = resolve_da_body_retention(None, Some(tree)).expect("pruning => GC on");
        let dir = tempfile::tempdir().unwrap();
        let store = NativeDaStore::new(StateDb::open(dir.path()).unwrap());
        let actions: Vec<_> = (0..400).map(action).collect();
        store.put_batch(&actions).unwrap();
        for (tip, a) in actions.iter().enumerate() {
            let tip = tip as u64;
            store
                .gc_committed(
                    tip,
                    &[compute_action_hash(a)],
                    0,
                    window,
                    keep_from_for_tree_frontier(tree_frontier(tip, tree)),
                    DA_GC_LIMITS,
                )
                .unwrap();
            for (h, b) in actions[..=tip as usize].iter().enumerate() {
                let h = h as u64;
                let present = store.contains(&compute_action_hash(b).0).unwrap();
                if tree_serves(h, tip, tree) {
                    assert!(
                        present,
                        "tip {tip}: block {h} is served, its body must be too"
                    );
                }
                assert_eq!(present, h + window > tip, "tip {tip}: body of {h}");
            }
        }
    }

    use crate::kv_store::RocksKVStore;
    use hotstuff_rs::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
    use hotstuff_rs::hotstuff::types::{Phase, PhaseCertificate};
    use hotstuff_rs::types::block::Block;
    use hotstuff_rs::types::data_types::{
        BlockHeight, ChainID, CryptoHash, Data, SignatureSet, ViewNumber,
    };

    /// The real hotstuff block tree over `db` (the node keeps it and the DA
    /// store in one consensus DB), with a committed chain at heights `0..len`.
    fn committed_tree(db: &StateDb, len: u64) -> (BlockTreeSingleton<RocksKVStore>, CryptoHash) {
        // SAFETY: test-only direct tree access, no replica runs on this store.
        let mut bt = unsafe { BlockTreeSingleton::new_unsafe(RocksKVStore::new(db.db_arc())) };
        let mut parent = None;
        for h in 0..len {
            parent = Some(insert_child(&mut bt, h, parent));
        }
        let tip = parent.expect("len > 0");
        let mut wb = BlockTreeWriteBatch::new_unsafe();
        bt.commit(&mut wb, &tip).unwrap();
        bt.write(wb);
        (bt, tip)
    }

    fn insert_child(
        bt: &mut BlockTreeSingleton<RocksKVStore>,
        height: u64,
        parent: Option<CryptoHash>,
    ) -> CryptoHash {
        let justify = match parent {
            None => PhaseCertificate::genesis_pc(),
            Some(block) => PhaseCertificate {
                chain_id: ChainID::new(0),
                view: ViewNumber::new(height),
                block,
                phase: Phase::Generic,
                signatures: SignatureSet::new(0),
            },
        };
        let mut data_hash = [0u8; 32];
        data_hash[..8].copy_from_slice(&height.to_be_bytes());
        let block = Block::new(
            BlockHeight::new(height),
            justify,
            CryptoHash::new(data_hash),
            Data::new(vec![]),
        );
        bt.insert(&block, None, None).unwrap();
        block.hash
    }

    /// Codex P1 (finding 1): `--retention-blocks` drops (a restart with a
    /// smaller window). The hotstuff pruner removes at most 64 heights per
    /// commit, so the block tree keeps serving old blocks for a while; with
    /// sparse rows (empty blocks record none) a window derived from the NEW
    /// retention would delete their bodies at once. Every block the tree still
    /// holds must keep its body, and the bodies go once the tree pruned them.
    #[test]
    fn a_retention_drop_never_deletes_a_body_the_tree_still_serves() {
        const TIP: u64 = 600;
        const NEW_TREE: u64 = hotstuff_rs::block_tree::MIN_BLOCK_TREE_RETENTION;
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let store = NativeDaStore::new(db.clone());
        let (mut bt, mut tip_hash) = committed_tree(&db, TIP + 1);
        let at = [300u64, 301, 450, 590];
        let actions: Vec<_> = at.iter().map(|&h| action(h)).collect();
        store.put_batch(&actions).unwrap();
        let hashes_at = |h: u64| -> Vec<torus_types::B256> {
            at.iter()
                .position(|&x| x == h)
                .map(|i| vec![compute_action_hash(&actions[i])])
                .unwrap_or_default()
        };
        // Before: a large block-tree window (nothing pruned), rows recorded at
        // every dispatched height, nothing due.
        let old = resolve_da_body_retention(None, Some(10_000)).unwrap();
        let gc = |height: u64, hashes: &[torus_types::B256], retention: u64| {
            let keep_from = keep_from_for_tree_frontier(read_block_tree_frontier(&db).unwrap());
            store
                .gc_committed(height, hashes, 0, retention, keep_from, DA_GC_LIMITS)
                .unwrap()
        };
        for h in 1..=TIP {
            gc(h, &hashes_at(h), old);
        }
        // After the drop: one commit + one prune pass + one DA pass per height.
        let window = resolve_da_body_retention(None, Some(NEW_TREE)).unwrap();
        for tip in TIP + 1..=TIP + 40 {
            tip_hash = insert_child(&mut bt, tip, Some(tip_hash));
            let mut wb = BlockTreeWriteBatch::new_unsafe();
            bt.commit(&mut wb, &tip_hash).unwrap();
            bt.write(wb);
            bt.prune_old_committed_blocks_with(NEW_TREE).unwrap();
            gc(tip, &[], window);
            for (i, &h) in at.iter().enumerate() {
                let served = bt.block_at_height(BlockHeight::new(h)).unwrap().is_some();
                let body = store.contains(&compute_action_hash(&actions[i]).0).unwrap();
                assert!(
                    body || !served,
                    "tip {tip}: the tree still serves block {h} but its body is gone"
                );
            }
        }
        // The tree caught up (pointer 632 = tip - 8): 300, 301 and 450 went;
        // 590 is pruned too but inside the margin (590 + 128 > 632).
        assert_eq!(read_block_tree_frontier(&db).unwrap(), Some(632));
        assert!(bt.block_at_height(BlockHeight::new(590)).unwrap().is_none());
        for a in &actions[..3] {
            assert!(!store.contains(&compute_action_hash(a).0).unwrap());
        }
        assert!(store.contains(&compute_action_hash(&actions[3]).0).unwrap());
    }

    /// The pointer the GC reads is the one hotstuff's pruner persists (key and
    /// encoding), and an unreadable one is an error, never "pruned".
    #[test]
    fn block_tree_frontier_is_the_pruners_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let (mut bt, _) = committed_tree(&db, 300);
        assert_eq!(read_block_tree_frontier(&db).unwrap(), None);
        assert_eq!(keep_from_for_tree_frontier(None), 0);
        bt.prune_old_committed_blocks_with(8).unwrap();
        assert_eq!(read_block_tree_frontier(&db).unwrap(), Some(64));
        assert_eq!(
            bt.block_tree_pruned_height().unwrap().map(|h| h.int()),
            Some(64)
        );
        let mut wb = BlockTreeWriteBatch::new_unsafe();
        wb.set_block_tree_pruned_height(BlockHeight::new(0x0102_0304_0506_0708))
            .unwrap();
        bt.write(wb);
        assert_eq!(
            read_block_tree_frontier(&db).unwrap(),
            Some(0x0102_0304_0506_0708)
        );
        // h may go once h + 128 <= pointer.
        assert_eq!(keep_from_for_tree_frontier(Some(127)), 0);
        assert_eq!(keep_from_for_tree_frontier(Some(128)), 1);
        assert_eq!(keep_from_for_tree_frontier(Some(1000)), 873);
        db.put_cf_raw(CF_CONSENSUS_META, &BLOCK_TREE_PRUNED_HEIGHT, &[1, 2, 3])
            .unwrap();
        assert!(read_block_tree_frontier(&db).is_err());
    }
}
