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
//!   the lowest height the tree still serves (its durable prune pointer,
//!   converted, see below) is at least `h + DA_GC_MARGIN_BLOCKS`
//!   ([`keep_from_for_tree_frontier`]).
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
//! # Two height systems
//!
//! The GC counts Torus heights (the committed header's `height`, the key of
//! `CF_BLOCK_BODIES` and of every GC row). The block tree counts hotstuff
//! heights: hotstuff numbers blocks from 0, the Torus header from 1 (hotstuff
//! block 0 extends the genesis header, Torus 0, and `check_parent_link`
//! makes every header its parent's plus one), so Torus `h` sits at hotstuff
//! `h - 1`. Every tree read and every comparison with a tree height goes
//! through one conversion ([`hotstuff_height_of`], [`torus_height_of`],
//! [`tree_served_from`]; Codex P1, round 2: the back-fill read hotstuff `h`
//! for Torus `h`, the NEXT block, and missed the re-inclusion pin): the
//! back-fill's block reads, the prune pointer (the tree serves Torus
//! `pointer + 1` on: the tree bound and the back-fill's pruned heights), and
//! the committed tip. Overflows saturate toward keeping more.
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
//!   `torus_da_gc_held_passes{reason="gap"}`, and the next pass retries. A failed record is
//!   therefore never dropped: the cursor stays behind until it lands.
//! - A height dispatch could not make durable (its `CF_BLOCK_BODIES` write
//!   failed) is not recorded at all, never as "no actions" (Codex P1, round
//!   2: that dropped its pins and moved the cursor past it): the cursor stays
//!   at it, the pass deletes nothing (`reason="unrecorded"`) and the next
//!   pass back-fills it from the tree. Its bodies then follow
//!   the normal window (crash replay re-executes from the DA store only above
//!   the applied height, and execution rewrites a failed body row); before
//!   round 2 they were kept for good, at the cost of the pins.
//! - Nothing is deleted while the tree holds committed heights above the
//!   dispatched one (Codex P1, round 2): a multi-block commit or boot
//!   reconciliation commits them all before their callbacks run, and one of
//!   them may re-include a due row's body; deleting it before its own pass
//!   pins it leaves its compact block unreconstructable. The tip is read only
//!   when something is due; an unreadable tip holds too. Holding (not
//!   pinning the undispatched blocks' hashes) is the simpler rule: in
//!   blocking dispatch (the default) dispatch reaches the tip at the last
//!   block of every commit, where the held rows go (up to [`DA_GC_LIMITS`]
//!   rows per pass). In the opt-in non-blocking dispatch, steady
//!   backpressure can hold it for long: visible as `reason="dispatch_behind"`.
//! - A due body a block ABOVE the committed tip references stays (claude
//!   review r4 / Codex r3 P2, torus issue 282de5bb): the tree holds and
//!   block-syncs its uncommitted tail, and a tail block may commit on a node
//!   without its cached proposal (after a restart, or a same-height
//!   re-proposal overwrote the cache entry), which rebuilds the compact block
//!   from the DA store; dispatch's restore runs only after that rebuild. At
//!   the first due row the pass reads the tail ([`BlockTreeHistory`]:
//!   [`uncommitted_native_hashes_in_tree`], every branch, normally 2-3
//!   blocks) and skips the hashes it references: the row goes, the body, its
//!   reference and shards stay. If the tail block commits, its record owns
//!   the body; if it never does, the body stays for good (it takes a faulty
//!   leader re-including a ~window-old action as its row expires). A tail
//!   block that cannot be read or decoded (or a tail over
//!   [`MAX_TAIL_BLOCKS`]) holds the whole pass (`reason="tail_unreadable"`).
//! - Heights below the first one the tree serves are skipped (pruned),
//!   but deletion waits until the last skipped height is itself due (window
//!   and tree bound): one of them may have re-included an older row's body.
//!   Such a gap means the GC was off or failing for longer than the tree
//!   window; the first-inclusion bodies of the skipped heights stay for good.
//!
//! Every pass that deletes nothing counts `torus_da_gc_held_passes` once,
//! labelled with the first rule that held it
//! ([`torus_state::DaGcHoldReason`]: `gap`, `backfilling`, `unrecorded`,
//! `pre_cursor`, `pruned_gap`, `dispatch_behind`, `tip_unreadable`,
//! `tail_unreadable`, `store_error`; P3, claude-review r4).
//!
//! A fresh store (no GC state, so no row a lost pin could matter for)
//! establishes the cursor at its first recorded height. A store a pre-cursor
//! binary wrote (owner, s128) may already have lost a pin that way: its
//! cursor starts where the tree's retained range starts (the first Torus
//! height the tree serves, never below the GC frontier), so the normal back-fill re-reads every
//! height the tree still serves (256 per pass, deletion held until it caught
//! up; heights the tree already pruned held as above). Its replays still
//! record nothing, and no pass deletes before the cursor exists. With the GC
//! off (archive node, `TORUS_DA_BODY_RETENTION=0`) no pass runs and nothing
//! is written.
//!
//! Not cleaned (owner, s128): bodies a node stored before its first GC pass
//! (a store upgraded from a binary without RH4: no GC state, so it starts as
//! a fresh store at its first dispatched height) have no row and stay for
//! good. Testnet starts from a fresh genesis, so none exist there.
//!
//! # Design alternative not chosen (s128)
//!
//! After three review rounds (each finding a way the cursor + record
//! bookkeeping could diverge from what the block tree still serves) the
//! owner chose targeted fixes over a redesign that would derive "may this
//! body go?" directly from the block tree at deletion time: scan every block
//! the tree still serves, the uncommitted tail included, for the hash,
//! instead of keeping rows, references, markers and a cursor. Revisit it if
//! another bookkeeping/tree divergence is found.

use hotstuff_rs::block_tree::accessors::internal::BlockTreeError;
use hotstuff_rs::block_tree::accessors::public::BlockTreeCamera;
use hotstuff_rs::block_tree::pluggables::{KVGetError, Key};
use hotstuff_rs::block_tree::variables::BLOCK_TREE_PRUNED_HEIGHT;
use hotstuff_rs::hotstuff::types::PhaseCertificate;
use hotstuff_rs::types::data_types::BlockHeight;
use torus_state::cf::CF_CONSENSUS_META;
use torus_state::{DaGcHistory, DaGcLimits, StateDb, StateError};
use torus_types::{compute_action_hash, CompactBlock, SignedNativeAction, TorusBlock, B256};

use crate::kv_store::RocksKVStore;

/// Blocks a body outlives its block in the block tree by: the window derived
/// from `--retention-blocks` is the block-tree retention plus this, and a body
/// goes only once the first height the tree serves is this far past it. It
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

/// The block tree's durable prune pointer in `db` (the lowest committed
/// HOTSTUFF height the tree still holds; `None` = never pruned; in Torus
/// heights: [`tree_served_from`]), as hotstuff's pruner persists
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
/// for the tree's prune pointer `frontier` (a hotstuff height, as
/// [`read_block_tree_frontier`] returns it): the body of Torus height `h` may
/// go once `h + DA_GC_MARGIN_BLOCKS <= served`, `served` = the lowest Torus
/// height the tree still serves ([`tree_served_from`]). `None` (never
/// pruned) = `0`, nothing goes. With the derived window this matches it
/// exactly in steady state (`served` = tip - tree retention), so it only
/// bites while the tree lags.
pub fn keep_from_for_tree_frontier(frontier: Option<u64>) -> u64 {
    tree_served_from(frontier)
        .and_then(|f| f.checked_sub(DA_GC_MARGIN_BLOCKS))
        .map_or(0, |f| f.saturating_add(1))
}

/// The Torus header height of the block the hotstuff tree holds at
/// `hotstuff` height: `hotstuff + 1` (`None` past `u64::MAX`). Hotstuff
/// numbers its blocks from 0, the Torus header from 1: hotstuff block 0
/// extends the genesis header (Torus 0), and every block's header is its
/// parent's plus one (`check_parent_link`). One of the two conversions every
/// block-tree read and prune comparison of the GC goes through (Codex P1,
/// round 2: the back-fill read hotstuff `H` for Torus `H`, the next block).
pub fn torus_height_of(hotstuff: u64) -> Option<u64> {
    hotstuff.checked_add(1)
}

/// The hotstuff height of the block carrying Torus header height `torus`:
/// `torus - 1`. `None` for the genesis header (Torus 0), which no tree block
/// carries. See [`torus_height_of`].
pub fn hotstuff_height_of(torus: u64) -> Option<u64> {
    torus.checked_sub(1)
}

/// The lowest Torus height the block tree still serves, for its prune
/// pointer (the lowest hotstuff height it holds; `None` = never pruned):
/// `pointer + 1`, saturating (an overflow only keeps more).
pub fn tree_served_from(pointer: Option<u64>) -> Option<u64> {
    pointer.map(|p| torus_height_of(p).unwrap_or(u64::MAX))
}

/// The Torus height of the highest committed block in the block tree in
/// `db` (`None` = nothing committed). An unreadable tip is an error, never
/// "nothing committed".
pub fn read_tree_committed_tip(db: &StateDb) -> Result<Option<u64>, StateError> {
    let tree_err = |e| StateError::InvalidData(format!("block tree committed tip: {e:?}"));
    let camera = BlockTreeCamera::new(RocksKVStore::new(db.db_arc()));
    let snapshot = camera.snapshot();
    let Some(hash) = snapshot.highest_committed_block().map_err(tree_err)? else {
        return Ok(None);
    };
    let Some(height) = snapshot.block_height(&hash).map_err(tree_err)? else {
        return Err(StateError::InvalidData(
            "block tree: the highest committed block has no height".into(),
        ));
    };
    Ok(Some(torus_height_of(height.int()).unwrap_or(u64::MAX)))
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
/// Torus `height` (its block sits at hotstuff height `height - 1`,
/// [`hotstuff_height_of`]), decoded like `TorusApp::on_committed_block`
/// decodes the datum: `(hashes, Some(actions))` for a full datum,
/// `(hashes, None)` for a compact one. A block without a datum or with an
/// undecodable one carries none (the commit path executes nothing from it),
/// nor does the genesis header (Torus 0). `None` = the tree has no committed
/// block at `height`.
#[allow(clippy::type_complexity)]
pub fn committed_native_actions_in_tree(
    db: &StateDb,
    height: u64,
) -> Result<Option<(Vec<B256>, Option<Vec<SignedNativeAction>>)>, StateError> {
    let Some(hotstuff) = hotstuff_height_of(height) else {
        return Ok(Some((Vec::new(), None)));
    };
    let tree_err = |e| {
        StateError::InvalidData(format!(
            "block tree read at Torus {height} (hotstuff {hotstuff}): {e:?}"
        ))
    };
    let camera = BlockTreeCamera::new(RocksKVStore::new(db.db_arc()));
    let snapshot = camera.snapshot();
    let Some(block) = snapshot
        .block_at_height(BlockHeight::new(hotstuff))
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
    match decode_native_datum(datum.bytes()) {
        Some(decoded) => Ok(Some(decoded)),
        None => {
            tracing::warn!(
                height,
                "DA body GC back-fill: undecodable committed datum, no actions"
            );
            Ok(Some((Vec::new(), None)))
        }
    }
}

/// A block datum's native actions, decoded like `TorusApp::on_committed_block`
/// decodes it: `(hashes, Some(actions))` for a full datum, `(hashes, None)`
/// for a compact one, `None` when it is neither.
#[allow(clippy::type_complexity)]
fn decode_native_datum(bytes: &[u8]) -> Option<(Vec<B256>, Option<Vec<SignedNativeAction>>)> {
    if let Ok(full) = bincode::deserialize::<TorusBlock>(bytes) {
        let hashes = full
            .native_actions
            .iter()
            .map(compute_action_hash)
            .collect();
        return Some((hashes, Some(full.native_actions)));
    }
    bincode::deserialize::<CompactBlock>(bytes)
        .ok()
        .map(|compact| (compact.native_action_hashes, None))
}

/// Most blocks [`uncommitted_native_hashes_in_tree`] reads above the
/// committed tip (normally 2-3); a larger tail is an error, so the pass holds.
pub const MAX_TAIL_BLOCKS: usize = 256;

/// The native-action hashes the blocks of the tree in `db` ABOVE its
/// highest committed block reference (the uncommitted tail: every
/// descendant of that block, on every branch; with nothing committed, every
/// block). Such a block may still commit and is block-synced meanwhile, so
/// the GC keeps the due bodies it references (claude-review r4 / Codex r3
/// P2). A tail block without data, with an undecodable datum, or more than
/// [`MAX_TAIL_BLOCKS`] of them is an error, never "no actions": the pass
/// then holds. Normally 2-3 blocks, read only when a row is due.
pub fn uncommitted_native_hashes_in_tree(db: &StateDb) -> Result<Vec<B256>, StateError> {
    let tree_err = |e| StateError::InvalidData(format!("block tree tail read: {e:?}"));
    let camera = BlockTreeCamera::new(RocksKVStore::new(db.db_arc()));
    let snapshot = camera.snapshot();
    let root = snapshot
        .highest_committed_block()
        .map_err(tree_err)?
        .unwrap_or(PhaseCertificate::genesis_pc().block);
    let mut stack = vec![root];
    let mut hashes = Vec::new();
    let mut read = 0usize;
    while let Some(block) = stack.pop() {
        let children = match snapshot.children(&block) {
            Ok(children) => children,
            // No children list: a leaf (hotstuff writes the list of a block
            // when its first child is inserted; a read error panics in
            // `RocksKVStore`, never reads as absent).
            Err(BlockTreeError::KVGetError(KVGetError::ValueExpectedButNotFound {
                key: Key::BlockChildren { .. },
            })) => continue,
            Err(e) => return Err(tree_err(e)),
        };
        for child in children.iter() {
            read += 1;
            if read > MAX_TAIL_BLOCKS {
                return Err(StateError::InvalidData(format!(
                    "block tree tail: more than {MAX_TAIL_BLOCKS} blocks above the committed tip"
                )));
            }
            let Some(data) = snapshot.block_data(child).map_err(tree_err)? else {
                return Err(StateError::InvalidData(format!(
                    "block tree tail: block {child:?} has no data"
                )));
            };
            if let Some(datum) = data.vec().first() {
                let Some((block_hashes, _)) = decode_native_datum(datum.bytes()) else {
                    return Err(StateError::InvalidData(format!(
                        "block tree tail: block {child:?} has an undecodable datum"
                    )));
                };
                hashes.extend(block_hashes);
            }
            stack.push(*child);
        }
    }
    Ok(hashes)
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
    /// In Torus heights: the tree serves Torus `pointer + 1` on.
    fn pruned_below(&self) -> Result<u64, StateError> {
        Ok(tree_served_from(read_block_tree_frontier(self.db)?).unwrap_or(0))
    }

    fn committed_hashes(&self, height: u64) -> Result<Option<Vec<B256>>, StateError> {
        let Some((hashes, actions)) = committed_native_actions_in_tree(self.db, height)? else {
            return Ok(None);
        };
        (self.restore)(height, &hashes, actions.as_deref());
        Ok(Some(hashes))
    }

    fn committed_through(&self) -> Result<Option<u64>, StateError> {
        read_tree_committed_tip(self.db)
    }

    fn uncommitted_hashes(&self) -> Result<Vec<B256>, StateError> {
        uncommitted_native_hashes_in_tree(self.db)
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

    /// hotstuff `prune_old_committed_blocks_with` keeps hotstuff heights
    /// `highest - retention ..= highest` (block_tree/accessors/internal.rs).
    /// Heights here are Torus heights: the tree holds Torus height `h` at
    /// hotstuff height `h - 1` (written out, not via the code under test).
    fn tree_serves(height: u64, tip: u64, retention: u64) -> bool {
        let (hs, highest) = (height - 1, tip - 1);
        highest <= retention || hs >= highest - retention
    }

    /// The pruner's pointer (a hotstuff height) once it kept up with Torus
    /// tip `tip`: `highest - retention`, `None` while inside the window.
    fn tree_frontier(tip: u64, retention: u64) -> Option<u64> {
        (tip - 1).checked_sub(retention).filter(|&f| f > 0)
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
        // Torus heights 1..=400 (height 0 is the genesis header).
        for (tip, a) in actions.iter().enumerate() {
            let tip = tip as u64 + 1;
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
            for (h, b) in actions[..tip as usize].iter().enumerate() {
                let h = h as u64 + 1;
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
    ///
    /// Heights are Torus heights; the tree holds Torus height `h` at hotstuff
    /// height `h - 1` (spelled out here, not via the code under test).
    #[test]
    fn a_retention_drop_never_deletes_a_body_the_tree_still_serves() {
        const TIP: u64 = 600;
        const NEW_TREE: u64 = hotstuff_rs::block_tree::MIN_BLOCK_TREE_RETENTION;
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        let store = NativeDaStore::new(db.clone());
        // Torus 1..=TIP at hotstuff 0..TIP.
        let (mut bt, mut tip_hash) = committed_tree(&db, TIP);
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
            tip_hash = insert_child(&mut bt, tip - 1, Some(tip_hash));
            let mut wb = BlockTreeWriteBatch::new_unsafe();
            bt.commit(&mut wb, &tip_hash).unwrap();
            bt.write(wb);
            bt.prune_old_committed_blocks_with(NEW_TREE).unwrap();
            gc(tip, &[], window);
            for (i, &h) in at.iter().enumerate() {
                let served = bt
                    .block_at_height(BlockHeight::new(h - 1))
                    .unwrap()
                    .is_some();
                let body = store.contains(&compute_action_hash(&actions[i]).0).unwrap();
                assert!(
                    body || !served,
                    "tip {tip}: the tree still serves block {h} but its body is gone"
                );
            }
        }
        // The tree caught up (pointer 631 = hotstuff tip 639 - 8, so it
        // serves Torus 632 on): 300, 301 and 450 went; 590 is pruned too but
        // inside the margin (590 + 128 > 632).
        assert_eq!(read_block_tree_frontier(&db).unwrap(), Some(631));
        assert!(bt.block_at_height(BlockHeight::new(589)).unwrap().is_none());
        for a in &actions[..3] {
            assert!(!store.contains(&compute_action_hash(a).0).unwrap());
        }
        assert!(store.contains(&compute_action_hash(&actions[3]).0).unwrap());
    }

    /// Codex P1 (round 2): the one conversion between the two height
    /// systems, at both edges. Hotstuff numbers blocks from 0, the Torus
    /// header from 1 (hotstuff block 0 extends the genesis header, Torus 0):
    /// Torus `h` is hotstuff `h - 1`. Torus 0 has no tree block; hotstuff
    /// `u64::MAX` has no Torus height. The prune pointer (a hotstuff height)
    /// says the tree serves Torus heights from `pointer + 1` (saturating: an
    /// overflow only keeps more).
    #[test]
    fn torus_and_hotstuff_heights_convert_at_both_edges() {
        assert_eq!(hotstuff_height_of(0), None, "genesis header: no tree block");
        assert_eq!(hotstuff_height_of(1), Some(0));
        assert_eq!(hotstuff_height_of(1228), Some(1227));
        assert_eq!(hotstuff_height_of(u64::MAX), Some(u64::MAX - 1));
        assert_eq!(torus_height_of(0), Some(1));
        assert_eq!(torus_height_of(1227), Some(1228));
        assert_eq!(torus_height_of(u64::MAX - 1), Some(u64::MAX));
        assert_eq!(torus_height_of(u64::MAX), None);
        for h in [1, 2, 1228, u64::MAX] {
            assert_eq!(hotstuff_height_of(h).and_then(torus_height_of), Some(h));
        }
        assert_eq!(tree_served_from(None), None, "never pruned");
        assert_eq!(tree_served_from(Some(0)), Some(1));
        assert_eq!(tree_served_from(Some(227)), Some(228));
        assert_eq!(tree_served_from(Some(u64::MAX)), Some(u64::MAX));
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
        // Torus height h may go once h + 128 <= the first Torus height the
        // tree serves (pointer + 1).
        assert_eq!(keep_from_for_tree_frontier(Some(126)), 0);
        assert_eq!(keep_from_for_tree_frontier(Some(127)), 1);
        assert_eq!(keep_from_for_tree_frontier(Some(128)), 2);
        assert_eq!(keep_from_for_tree_frontier(Some(1000)), 874);
        assert_eq!(
            keep_from_for_tree_frontier(Some(u64::MAX)),
            u64::MAX - 127,
            "saturates: the top height stays"
        );
        db.put_cf_raw(CF_CONSENSUS_META, &BLOCK_TREE_PRUNED_HEIGHT, &[1, 2, 3])
            .unwrap();
        assert!(read_block_tree_frontier(&db).is_err());
    }
}
