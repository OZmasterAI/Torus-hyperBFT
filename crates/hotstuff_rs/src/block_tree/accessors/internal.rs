//! Internal read-and-write handle used by the algorithm thread to mutate the Block Tree.
//!
//! # Initializing the Block Tree
//!
//! All variables in the Block Tree start out empty except eight. These eight variables, which must be
//! initialized using the [`initialize`](BlockTree::initialize) function before doing anything else with
//! the Block Tree, are:
//!
//! |Variable|Initial value|
//! |---|---|
//! |Committed App State|Provided to [`initialize`](crate::replica::Replica::initialize).|
//! |Committed Validator Set|Provided to [`initialize`](crate::replica::Replica::initialize).|
//! |Previous Validator Set|Provided to [`initialize`](crate::replica::Replica::initialize).|
//! |Validator Set Update Block Height|Provided to [`initialize`](crate::replica::Replica::initialize).|
//! |Validator Set Update Complete|Provided to [`initialize`](crate::replica::Replica::initialize).|
//! |Locked PC|The [Genesis PC](crate::hotstuff::types::PhaseCertificate::genesis_pc)|
//! |Highest View Entered|0|
//! |Highest Phase Certificate|The [Genesis PC](crate::hotstuff::types::PhaseCertificate::genesis_pc)|
//!
//! # Mutating the Block Tree directly from user code
//!
//! In normal operation, HotStuff-rs code will internally be making all writes to the
//! `BlockTreeSingleton`, while users can get a `BlockTreeCamera` through which they can read from the
//! block tree by calling `Replica`'s [`block_tree_camera`](crate::replica::Replica::block_tree_camera)
//! method.
//!
//! Sometimes, however, users may want to manually mutate the Block Tree, for example, to recover from
//! an error that has corrupted some of its invariants. For this purpose, one can unsafe-ly get an
//! instance of BlockTree using [`BlockTree::new_unsafe`] and an instance of the corresponding
//! [`BlockTreeWriteBatch`] using [`BlockTreeWriteBatch::new_unsafe`].

use std::{cmp::max, iter::successors, sync::mpsc::Sender, time::SystemTime};

use borsh::BorshSerialize;
use ed25519_dalek::VerifyingKey;

use crate::{
    events::{
        CommitBlockEvent, Event, PruneBlockEvent, UpdateHighestPCEvent, UpdateLockedPCEvent,
        UpdateValidatorSetEvent,
    },
    hotstuff::types::PhaseCertificate,
    pacemaker::types::TimeoutCertificate,
    types::{
        block::Block,
        data_types::{BlockHeight, ChildrenList, CryptoHash, Data, DataLen, Datum, ViewNumber},
        update_sets::{AppStateUpdates, ValidatorSetUpdates},
        validator_set::{
            ValidatorSet, ValidatorSetBytes, ValidatorSetState, ValidatorSetUpdatesStatus,
            ValidatorSetUpdatesStatusBytes,
        },
    },
};

use super::super::{
    invariants,
    pluggables::{KVGetError, KVStore, Key, WriteBatch},
    variables::{self, concat},
};

use super::{app::AppBlockTreeView, public::BlockTreeSnapshot};

/// Result of a [`BlockTreeSingleton::update`] call, containing both the
/// validator set updates (if any) and the list of newly committed block hashes
/// (oldest to newest). The caller uses `committed_block_hashes` to invoke
/// `App::on_committed_block` for each finalized block.
pub struct UpdateResult {
    pub validator_set_updates: Option<ValidatorSetUpdates>,
    pub committed_block_hashes: Vec<CryptoHash>,
}

/// Read and write handle into the block tree that should be owned exclusively by the algorithm thread.
///
/// ## Categories of methods
///
/// `BlockTreeSingleton` has a large number of methods. To improve understandability, these methods are grouped
/// into five categories, with methods in each separate category being defined in a separate `impl`
/// block. These five categories are:
/// 1. [Lifecycle methods](#impl-BlockTreeSingleton<K>).
/// 2. [Top-level state updaters](#impl-BlockTreeSingleton<K>-1).
/// 3. [Helper functions called by `BlockTree::update`](#impl-BlockTreeSingleton<K>-2).
/// 4. [Basic state getters](#impl-BlockTreeSingleton<K>-3).
/// 5. [Extra state getters](#impl-BlockTreeSingleton<K>-4).
/// `self.0` is the backing key-value store; `self.1` is a write-through in-memory
/// cache of the deserialized `LeaderReputation` (MonadBFT B3). It is lazily loaded
/// on first read and refreshed on every reputation write, eliminating the ~9 KV
/// get + borsh deserialize per consensus round on the hot path. `RefCell` gives
/// interior mutability for the lazy load behind `&self`; a `BlockTreeSingleton` is
/// owned/borrowed by the single consensus thread (`KVStore` requires `Send`, not
/// `Sync`), so being `!Sync` from the `RefCell` is fine.
pub struct BlockTreeSingleton<K: KVStore>(
    K,
    core::cell::RefCell<Option<crate::hotstuff::types::LeaderReputation>>,
);

/// Lifecycle methods.
///
/// These are methods for creating and initializing a `BlockTreeSingleton`, as well as for using it to create and
/// consume other block tree-related types, namely, [`BlockTreeSnapshot`], [`BlockTreeWriteBatch`], and
/// [`AppBlockTreeView`].
impl<K: KVStore> BlockTreeSingleton<K> {
    /// Create a new instance of `BlockTreeSingleton` on top of `kv_store`.
    ///
    /// This constructor is private (`pub(crate)`). To create an instance of `BlockTreeSingleton` as a
    /// library user, use [`new_unsafe`](Self::new_unsafe).
    pub(crate) fn new(kv_store: K) -> Self {
        BlockTreeSingleton(kv_store, core::cell::RefCell::new(None))
    }

    /// Create a new instance of `BlockTreeSingleton` on top of `kv_store`.
    ///
    /// ## Safety
    ///
    /// Read
    /// [mutating the block tree directly from user code](#mutating-the-block-tree-directly-from-user-code).
    pub unsafe fn new_unsafe(kv_store: K) -> Self {
        Self::new(kv_store)
    }

    /// Initialize the block tree variables listed in [initial state](#initial-state).
    ///
    /// This function must be called exactly once on a `BlockTreeSingleton` with an empty backing
    /// `kv_store`, before any of the other functions (except the constructors `new` or `new_unsafe`) are
    /// called.
    pub fn initialize(
        &mut self,
        initial_app_state: &AppStateUpdates,
        initial_validator_set_state: &ValidatorSetState,
    ) -> Result<(), BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();

        wb.apply_app_state_updates(initial_app_state);

        let committed_validator_set = initial_validator_set_state.committed_validator_set();
        let previous_validator_set = initial_validator_set_state.previous_validator_set();
        let update_height = initial_validator_set_state.update_height();
        let update_decided = initial_validator_set_state.update_decided();

        wb.set_committed_validator_set(&committed_validator_set)?;
        wb.set_previous_validator_set(&previous_validator_set)?;
        if let Some(height) = *update_height {
            wb.set_validator_set_update_block_height(height)?
        }
        wb.set_validator_set_update_decided(update_decided)?;

        wb.set_locked_pc(&PhaseCertificate::genesis_pc())?;

        wb.set_highest_view_entered(ViewNumber::init())?;

        wb.set_highest_pc(&PhaseCertificate::genesis_pc())?;

        self.write(wb);

        Ok(())
    }

    /// Create a `BlockTreeSnapshot`.
    pub fn snapshot(&self) -> BlockTreeSnapshot<K::Snapshot<'_>> {
        BlockTreeSnapshot::new(self.0.snapshot())
    }

    /// Atomically write the changes in `write_batch` into the `BlockTreeSingleton`.
    pub fn write(&mut self, write_batch: BlockTreeWriteBatch<K::WriteBatch>) {
        self.0.write(write_batch.0)
    }

    /// Create an `AppBlockTreeView` which sees the app state as it will be right after `parent` becomes
    /// committed.
    pub fn app_view<'a>(
        &'a self,
        parent: Option<&CryptoHash>,
    ) -> Result<AppBlockTreeView<'a, K>, BlockTreeError> {
        let highest_committed_block_height = self.highest_committed_block_height()?;
        let parent = match parent {
            None => None,
            Some(&b) => Some(b),
        };

        // Obtain an iterator over the ancestors starting from the parent, all the way until genesis,
        // from newest (parent) to oldest.
        let ancestors_iter = successors(parent, |b| {
            self.block_justify(b)
                .ok()
                .map(|pc| {
                    if !pc.is_genesis_pc() {
                        Some(pc.block)
                    } else {
                        None
                    }
                })
                .flatten()
        });

        let ancestors_heights_iter = ancestors_iter
            .clone()
            .map(|block| {
                self.block_height(&block).map(|res| {
                    if res.is_none() {
                        Err(BlockTreeError::BlockExpectedButNotFound {
                            block: block.clone(),
                        })
                    } else {
                        Ok(res.unwrap())
                    }
                })
            })
            .flatten()
            .flatten();

        // Obtain an iterator over the uncomitted ancestors starting from the parent,
        // ending at the lowest uncommitted ancestor.
        let uncommitted_ancestors_iter = ancestors_iter
            .zip(ancestors_heights_iter)
            .take_while(|(_, height)| {
                highest_committed_block_height.is_none()
                    || highest_committed_block_height.is_some_and(|h| height > &h)
            })
            .map(|(b, _)| b);

        // Obtain a vector of optional app state updates associated with ancestors
        // starting from the parent, ending at the oldest uncommitted ancestor.
        let pending_ancestors_app_state_updates: Vec<Option<AppStateUpdates>> =
            uncommitted_ancestors_iter
                .map(|block| self.pending_app_state_updates(&block))
                .flatten()
                .collect();

        Ok(AppBlockTreeView {
            block_tree: self,
            pending_ancestors_app_state_updates,
        })
    }
}

/// Top-level state updaters.
///
/// These are the methods that mutate the block tree that are called directly by code in the
/// subprotocols (i.e., [`hotstuff`](crate::hotstuff), [`block_sync`](crate::block_sync), and
/// [`pacemaker`](crate::pacemaker)). Mutating methods outside of this `impl` and the lifecycle methods
/// `impl` above are only used internally in this module.
impl<K: KVStore> BlockTreeSingleton<K> {
    /// Insert into the block tree a `block` that will cause the provided `app_state_updates` and
    /// `validator_set_updates` to be applied when it is committed in the future.
    ///
    /// ## Relationship with `update`
    ///
    /// `insert` does not internally call [`update`](Self::update). Calling code is responsible for
    /// calling `update` on `block.justify` after calling `insert`.
    ///
    /// ## Precondition
    ///
    /// [`safe_block`](invariants::safe_block) is `true` for `block`.
    pub fn insert(
        &mut self,
        block: &Block,
        app_state_updates: Option<&AppStateUpdates>,
        validator_set_updates: Option<&ValidatorSetUpdates>,
    ) -> Result<(), BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();

        // Set block, which entails setting block's fields in separate key-value pairs.
        wb.set_block(block)?;

        // Set the block as the newest inserted block.
        wb.set_newest_block(&block.hash)?;

        // Insert the block's pending app state updates and validator set updates.
        if let Some(app_state_updates) = app_state_updates {
            wb.set_pending_app_state_updates(&block.hash, app_state_updates)?;
        }
        if let Some(validator_set_updates) = validator_set_updates {
            wb.set_pending_validator_set_updates(&block.hash, validator_set_updates)?;
        }

        // Mark the block as a child of its parent block.
        let mut siblings = self
            .children(&block.justify.block)
            .unwrap_or(ChildrenList::default());
        siblings.push(block.hash);
        wb.set_children(&block.justify.block, &siblings)?;

        // Atomically write the above changes to persistent storage.
        self.write(wb);

        Ok(())
    }

    /// Update the block tree upon seeing a safe `justify` in a [`Nudge`](crate::hotstuff::messages::Nudge)
    /// or a [`Block`].
    ///
    /// ## Updates
    ///
    /// Depending on the specific Phase Certificate received and the state of the Block Tree, the updates
    /// that this function performs will include:
    /// 1. Updating the Highest PC if `justify.view > highest_pc.view`.
    /// 2. Updating the Locked PC if appropriate, as determined by the [`pc_to_lock`](invariants::pc_to_lock)
    ///    helper.
    /// 3. Committing a block and all of its ancestors if appropriate, as determined by the
    ///    [`block_to_commit`](invariants::block_to_commit) helper.
    /// 4. Marking the latest validator set updates as decided if `justify` is a Decide PC.
    ///
    /// ## Preconditions
    ///
    /// The `Block` or `Nudge` containing `justify` must satisfy [`safe_block`](invariants::safe_block) or
    /// [`safe_nudge`](invariants::safe_nudge), respectively.
    pub(crate) fn update(
        &mut self,
        justify: &PhaseCertificate,
        event_publisher: &Option<Sender<Event>>,
    ) -> Result<UpdateResult, BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();

        let mut update_locked_pc: Option<PhaseCertificate> = None;
        let mut update_highest_pc: Option<PhaseCertificate> = None;
        let mut committed_blocks: Vec<(CryptoHash, Option<ValidatorSetUpdates>)> = Vec::new();

        // 1. Update highestPC if needed.
        if justify.view > self.highest_pc()?.view {
            wb.set_highest_pc(justify)?;
            update_highest_pc = Some(justify.clone())
        }

        // 2. Update lockedPC if needed.
        if let Some(new_locked_pc) = invariants::pc_to_lock(justify, &self)? {
            wb.set_locked_pc(&new_locked_pc)?;
            update_locked_pc = Some(new_locked_pc)
        }

        // 3. Commit block(s) if needed (MonadBFT 2-chain irrevocable commit).
        if let Some(block) = invariants::block_to_commit(justify, &self)? {
            committed_blocks = self.commit(&mut wb, &block)?;
        }

        // 4. Set validator set updates as decided if needed.
        if justify.phase.is_decide() {
            wb.set_validator_set_update_decided(true)?
        }

        self.write(wb);

        // MonadBFT B3: Record leader success on QC advancement.
        // Every new highest QC is a positive reputation signal — the leader of that
        // view successfully got a supermajority vote. This fires much more frequently
        // than commit-only success, preventing the reputation death spiral where
        // commits require consecutive QC views but reputation degradation prevents them.
        if update_highest_pc.is_some() && !justify.is_genesis_pc() {
            if let Ok(vs) = self.committed_validator_set() {
                let qc_leader = match self.leader_reputation() {
                    Ok(ref rep) => crate::pacemaker::implementation::select_leader_with_reputation(
                        justify.view, &vs, rep,
                    ),
                    Err(_) => crate::pacemaker::implementation::select_leader(justify.view, &vs),
                };
                let _ = self.record_leader_success(&qc_leader);
            }
        }

        // MonadBFT B2: Promote irrevocably committed blocks from speculative list.
        // MonadBFT B3: Record leader success for reputation tracking (on commit).
        for (block_hash, _) in &committed_blocks {
            let _ = self.promote_speculative_to_irrevocable(block_hash);
            // The block's justify tells us the view in which it was proposed.
            // The leader of that view gets a reputation success.
            if let Ok(block_justify) = self.block_justify(block_hash) {
                if !block_justify.is_genesis_pc() {
                    // The block was proposed in justify.view + 1 (the view after its QC).
                    // But actually, block_justify.view is the QC view. The block itself
                    // was proposed by the leader of the view that follows. We need to find
                    // who proposed the committed block. The committed block's view is in its
                    // height context. For reputation, we use the committed validator set
                    // leader selection.
                    let committed_vs = self.committed_validator_set();
                    if let Ok(vs) = committed_vs {
                        // The block's proposer is the leader of the view where the block was inserted.
                        // For pipelined mode, blocks are committed by 2-chain: the grandparent.
                        // The committed block was proposed in a view we can approximate from
                        // block_justify.view + 1 (the view this block was proposed in).
                        let proposed_view = block_justify.view + 1;
                        let leader = match self.leader_reputation() {
                            Ok(ref rep) => crate::pacemaker::implementation::select_leader_with_reputation(
                                proposed_view, &vs, rep,
                            ),
                            Err(_) => crate::pacemaker::implementation::select_leader(proposed_view, &vs),
                        };
                        let _ = self.record_leader_success(&leader);
                    }
                }
            }
        }

        Self::publish_update_block_tree_events(
            event_publisher,
            update_highest_pc,
            update_locked_pc,
            &committed_blocks,
        );

        // Collect committed block hashes (oldest to newest) for on_committed_block callbacks.
        let committed_block_hashes: Vec<CryptoHash> = committed_blocks
            .iter()
            .map(|(hash, _)| *hash)
            .collect();

        // Safety: a block that updates the validator set must be followed by a block that contains a decide
        // pc. A block becomes committed immediately if its commitPC or decidePC is seen. Therefore, under normal
        // operation, at most 1 validator-set-updating block can be committed at a time.
        let resulting_vs_update = committed_blocks
            .into_iter()
            .rev()
            .find_map(|(_, validator_set_updates_opt)| validator_set_updates_opt);

        Ok(UpdateResult {
            validator_set_updates: resulting_vs_update,
            committed_block_hashes,
        })
    }

    /// Set the highest `TimeoutCertificate` to be `tc`.
    ///
    /// ## Preconditions
    ///
    /// TODO.
    pub fn set_highest_tc(&mut self, tc: &TimeoutCertificate) -> Result<(), BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();
        wb.set_highest_tc(tc)?;
        self.write(wb);
        Ok(())
    }

    /// Update highest_pc and validator-set-decided flag from a received PC
    /// without triggering commit processing. Used by the pacemaker when it
    /// receives an AdvanceView containing a PC whose block may not yet be
    /// in the tree (header pipeline: body still in flight).
    pub fn advance_highest_pc_from_remote(
        &mut self,
        pc: &PhaseCertificate,
    ) -> Result<(), BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();
        if pc.view > self.highest_pc()?.view {
            wb.set_highest_pc(pc)?;
        }
        if pc.phase.is_decide() {
            wb.set_validator_set_update_decided(true)?;
        }
        self.write(wb);
        Ok(())
    }

    /// Set the highest view entered to be `view`.
    ///
    /// ## Preconditions
    ///
    /// `view >= self.highest_view_entered()`.
    pub fn set_highest_view_entered(&mut self, view: ViewNumber) -> Result<(), BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();
        wb.set_highest_view_entered(view)?;
        self.write(wb);
        Ok(())
    }

    /// Set the highest view phase-voted to be `view`.
    ///
    /// ## Preconditions
    ///
    /// `view >= self.highest_view_voted()`.
    pub fn set_highest_view_phase_voted(&mut self, view: ViewNumber) -> Result<(), BlockTreeError> {
        let mut wb = BlockTreeWriteBatch::new();
        wb.set_highest_view_phase_voted(view)?;
        self.write(wb);
        Ok(())
    }

    /// FIX CONS-FIND-16: Atomically set both highest_view_voted and last_voted_proposal
    /// in a single WriteBatch to prevent inconsistency on crash between writes.
    pub fn set_vote_state_atomic(
        &mut self,
        view: ViewNumber,
        block: CryptoHash,
    ) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
        wb.set_highest_view_phase_voted(view)?;
        wb.0.set(
            &variables::LAST_VOTED_PROPOSAL,
            &(view, block).try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        );
        self.write(wb);
        Ok(())
    }
}

/// Helper functions called by [BlockTree::update].
impl<K: KVStore> BlockTreeSingleton<K> {
    /// Commit `block` and all of its ancestors, if they have not already been committed.
    ///
    /// ## Return value
    ///
    /// Returns the hashes of the newly committed blocks, along with the updates each caused to the
    /// validator set, in order from the newly committed block with the lowest height to the newly committed
    /// block with the highest height.
    ///
    /// ## Preconditions
    ///
    /// [`block_to_commit`](invariants::block_to_commit) returns `block`.
    pub fn commit(
        &mut self,
        wb: &mut BlockTreeWriteBatch<K::WriteBatch>,
        block: &CryptoHash,
    ) -> Result<Vec<(CryptoHash, Option<ValidatorSetUpdates>)>, BlockTreeError> {
        // Obtain an iterator over the "block" and its ancestors, all the way until genesis, from newest ("block") to oldest.
        let blocks_iter = successors(Some(*block), |b| {
            self.block_justify(b)
                .ok()
                .map(|pc| {
                    if !pc.is_genesis_pc() {
                        Some(pc.block)
                    } else {
                        None
                    }
                })
                .flatten()
        });

        // Newest committed block height, we do not consider the blocks from this height downwards.
        let min_height = self.highest_committed_block_height()?;

        // Obtain an iterator over the uncomitted blocks among "block" and its ancestors from oldest to newest,
        // the newest block being "block".
        // This is required because we want to commit blocks in correct order, applying updates from oldest to
        // newest.
        let uncommitted_blocks_iter = blocks_iter.take_while(|b| {
            min_height.is_none()
                || min_height.is_some_and(|h| {
                    self.block_height(b)
                        .ok()
                        .flatten()
                        .map_or(false, |bh| bh > h)
                })
        });
        let uncommitted_blocks = uncommitted_blocks_iter.collect::<Vec<CryptoHash>>();
        let uncommitted_blocks_ordered_iter = uncommitted_blocks.iter().rev();

        // Helper closure that
        // (1) commits block b, applying all related updates to the write batch,
        // (2) extends the vector of blocks committed so far (accumulator) with b together with the optional
        //     validator set updates associated with b,
        // (3) returns the extended vector of blocks committed so far (updated accumulator).
        let commit =
            |committed_blocks_res: Result<
                Vec<(CryptoHash, Option<ValidatorSetUpdates>)>,
                BlockTreeError,
            >,
             b: &CryptoHash|
             -> Result<Vec<(CryptoHash, Option<ValidatorSetUpdates>)>, BlockTreeError> {
                let mut committed_blocks = committed_blocks_res?;

                let block_height = self
                    .block_height(b)?
                    .ok_or(BlockTreeError::BlockExpectedButNotFound { block: b.clone() })?;
                // Work steps:

                // Set block at height.
                wb.set_block_at_height(block_height, b)?;

                // Delete all of block's siblings.
                self.delete_siblings(wb, b)?;

                // Apply pending app state updates.
                if let Some(pending_app_state_updates) = self.pending_app_state_updates(b)? {
                    wb.apply_app_state_updates(&pending_app_state_updates);
                    wb.delete_pending_app_state_updates(b);
                }

                // Apply pending validator set updates.
                if let ValidatorSetUpdatesStatus::Pending(validator_set_updates) =
                    self.validator_set_updates_status(b)?
                {
                    let mut committed_validator_set = self.committed_validator_set()?;
                    let previous_validator_set = committed_validator_set.clone();
                    committed_validator_set.apply_updates(&validator_set_updates);

                    wb.set_committed_validator_set(&committed_validator_set)?;
                    wb.set_previous_validator_set(&previous_validator_set)?;
                    wb.set_validator_set_update_block_height(block_height)?;
                    wb.set_validator_set_update_decided(false)?;
                    wb.set_committed_validator_set_updates(block)?;

                    committed_blocks.push((*b, Some(validator_set_updates.clone())));
                } else {
                    committed_blocks.push((*b, None));
                }

                // Update the highest committed block.
                wb.set_highest_committed_block(b)?;

                // Return the blocks committed so far together with their corresponding validator set updates.
                Ok(committed_blocks)
            };

        // Iterate over the uncommitted blocks from oldest to newest,
        // (1) applying related updates (by mutating the write batch), and
        // (2) building up the vector of committed blocks (by pushing the newely committed blocks to
        //     the accumulator vector).
        // Finally, return the accumulator.
        uncommitted_blocks_ordered_iter.fold(Ok(Vec::new()), commit)
    }

    /// Delete the "siblings" of the specified block, along with all of its associated data (e.g., pending
    /// app state updates, validator set updates).
    ///
    /// "Siblings" refer to other blocks that share the same parent as the specified block.
    ///
    /// ## Precondition
    ///
    /// `block` is in its parents' (or the genesis) children list.
    ///
    /// ## Error
    ///
    /// Returns an error if the block is not in the block tree, or if the block's parent (or genesis) does not have a
    /// children list.
    pub fn delete_siblings(
        &mut self,
        wb: &mut BlockTreeWriteBatch<K::WriteBatch>,
        block: &CryptoHash,
    ) -> Result<(), BlockTreeError> {
        let parent_or_genesis = self.block_justify(block)?.block;
        let parents_or_genesis_children = self.children(&parent_or_genesis)?;
        let siblings = parents_or_genesis_children
            .iter()
            .filter(|sib| *sib != block);
        for sibling in siblings {
            self.delete_branch(wb, sibling);
        }

        wb.set_children(&parent_or_genesis, &ChildrenList::new(vec![*block]))?;
        Ok(())
    }

    /// Deletes all data of blocks in a branch starting from (and including) a given root block.
    pub fn delete_branch(
        &mut self,
        wb: &mut BlockTreeWriteBatch<K::WriteBatch>,
        root: &CryptoHash,
    ) {
        for block in self.blocks_in_branch(*root) {
            wb.delete_children(&block);
            wb.delete_pending_app_state_updates(&block);
            wb.delete_block_validator_set_updates(&block);

            if let Ok(Some(data_len)) = self.block_data_len(&block) {
                wb.delete_block(&block, data_len)
            }
        }
    }

    /// Perform depth-first search to collect the hashes of all blocks in the branch rooted at `root` into
    /// a single iterator.
    pub fn blocks_in_branch(&self, root: CryptoHash) -> impl Iterator<Item = CryptoHash> {
        let mut stack: Vec<CryptoHash> = vec![root];
        let mut branch: Vec<CryptoHash> = vec![];

        while let Some(block) = stack.pop() {
            if let Ok(children) = self.children(&block) {
                for child in children.iter() {
                    stack.push(*child)
                }
            };
            branch.push(block)
        }
        branch.into_iter()
    }

    /// Publish all events resulting from calling [`update`](Self::update). These events have to do with
    /// changing persistent state, and  possibly include: [`UpdateHighestPCEvent`], [`UpdateLockedPCEvent`],
    /// [`PruneBlockEvent`], [`CommitBlockEvent`], [`UpdateValidatorSetEvent`].
    ///
    /// Invariant: this method must only be invoked after the associated changes are persistently written to
    /// the [`BlockTreeSingleton`].
    fn publish_update_block_tree_events(
        event_publisher: &Option<Sender<Event>>,
        update_highest_pc: Option<PhaseCertificate>,
        update_locked_pc: Option<PhaseCertificate>,
        committed_blocks: &Vec<(CryptoHash, Option<ValidatorSetUpdates>)>,
    ) {
        if let Some(highest_pc) = update_highest_pc {
            Event::UpdateHighestPC(UpdateHighestPCEvent {
                timestamp: SystemTime::now(),
                highest_pc: highest_pc,
            })
            .publish(event_publisher)
        };

        if let Some(locked_pc) = update_locked_pc {
            Event::UpdateLockedPC(UpdateLockedPCEvent {
                timestamp: SystemTime::now(),
                locked_pc,
            })
            .publish(event_publisher)
        };

        committed_blocks
            .iter()
            .for_each(|(b, validator_set_updates_opt)| {
                Event::PruneBlock(PruneBlockEvent {
                    timestamp: SystemTime::now(),
                    block: b.clone(),
                })
                .publish(event_publisher);
                Event::CommitBlock(CommitBlockEvent {
                    timestamp: SystemTime::now(),
                    block: b.clone(),
                })
                .publish(event_publisher);
                if let Some(validator_set_updates) = validator_set_updates_opt {
                    Event::UpdateValidatorSet(UpdateValidatorSetEvent {
                        timestamp: SystemTime::now(),
                        cause_block: *b,
                        validator_set_updates: validator_set_updates.clone(),
                    })
                    .publish(event_publisher);
                }
            });
    }
}

/// "Basic" state getters.
///
/// Each basic state getter calls a corresponding provided method of [`KVGet`](super::kv_store::KVGet) and
/// return whatever they return.
///
/// The exact same set of basic state getters are also defined on `BlockTreeSnapshot`.
impl<K: KVStore> BlockTreeSingleton<K> {
    pub fn block(&self, block: &CryptoHash) -> Result<Option<Block>, BlockTreeError> {
        Ok(self.0.block(block)?)
    }

    pub fn block_height(&self, block: &CryptoHash) -> Result<Option<BlockHeight>, BlockTreeError> {
        Ok(self.0.block_height(block)?)
    }

    pub fn block_data_hash(
        &self,
        block: &CryptoHash,
    ) -> Result<Option<CryptoHash>, BlockTreeError> {
        Ok(self.0.block_data_hash(block)?)
    }

    pub fn block_justify(&self, block: &CryptoHash) -> Result<PhaseCertificate, BlockTreeError> {
        Ok(self.0.block_justify(block)?)
    }

    pub fn block_data_len(&self, block: &CryptoHash) -> Result<Option<DataLen>, BlockTreeError> {
        Ok(self.0.block_data_len(block)?)
    }

    pub fn block_data(&self, block: &CryptoHash) -> Result<Option<Data>, BlockTreeError> {
        Ok(self.0.block_data(block)?)
    }

    pub fn block_datum(&self, block: &CryptoHash, datum_index: u32) -> Option<Datum> {
        self.0.block_datum(block, datum_index)
    }

    pub fn block_at_height(
        &self,
        height: BlockHeight,
    ) -> Result<Option<CryptoHash>, BlockTreeError> {
        Ok(self.0.block_at_height(height)?)
    }

    pub fn children(&self, block: &CryptoHash) -> Result<ChildrenList, BlockTreeError> {
        Ok(self.0.children(block)?)
    }

    pub fn committed_app_state(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.committed_app_state(key)
    }

    pub fn pending_app_state_updates(
        &self,
        block: &CryptoHash,
    ) -> Result<Option<AppStateUpdates>, BlockTreeError> {
        Ok(self.0.pending_app_state_updates(block)?)
    }

    pub fn committed_validator_set(&self) -> Result<ValidatorSet, BlockTreeError> {
        Ok(self.0.committed_validator_set()?)
    }

    pub fn validator_set_updates_status(
        &self,
        block: &CryptoHash,
    ) -> Result<ValidatorSetUpdatesStatus, BlockTreeError> {
        Ok(self.0.validator_set_updates_status(block)?)
    }

    pub fn locked_pc(&self) -> Result<PhaseCertificate, BlockTreeError> {
        Ok(self.0.locked_pc()?)
    }

    pub fn highest_view_entered(&self) -> Result<ViewNumber, BlockTreeError> {
        Ok(self.0.highest_view_entered()?)
    }

    pub fn highest_pc(&self) -> Result<PhaseCertificate, BlockTreeError> {
        Ok(self.0.highest_pc()?)
    }

    pub fn highest_committed_block(&self) -> Result<Option<CryptoHash>, BlockTreeError> {
        Ok(self.0.highest_committed_block()?)
    }

    pub fn highest_tc(&self) -> Result<Option<TimeoutCertificate>, BlockTreeError> {
        Ok(self.0.highest_tc()?)
    }

    pub fn validator_set_state(&self) -> Result<ValidatorSetState, BlockTreeError> {
        Ok(self.0.validator_set_state()?)
    }

    pub fn highest_view_voted(&self) -> Result<Option<ViewNumber>, BlockTreeError> {
        Ok(self.0.highest_view_phase_voted()?)
    }
}

/// "Extra" state getters.
///
/// Extra state getters call [basic state getters](#impl-BlockTree<K>-3) and aggregate or modify what
/// they return into forms that are more convenient to use.
///
/// Unlike basic state getters, these functions are not defined on `BlockTreeSnapshot`.
impl<K: KVStore> BlockTreeSingleton<K> {
    /// Check whether `block` exists on the block tree.
    pub fn contains(&self, block: &CryptoHash) -> bool {
        self.block(block).is_ok_and(|block_opt| block_opt.is_some())
    }

    /// Get the maximum of:
    /// - [`self.highest_view_entered()`](Self::highest_view_entered).
    /// - [`self.highest_pc()`](Self::highest_pc).
    /// - [`self.highest_tc()`](Self::highest_tc).
    ///
    /// This is useful for deciding which view to initially enter after starting or restarting a replica.
    pub fn highest_view_with_progress(&self) -> Result<ViewNumber, BlockTreeError> {
        Ok(max(
            self.highest_view_entered()?,
            max(
                self.highest_pc()?.view,
                self.highest_tc()?
                    .map(|tc| tc.view)
                    .unwrap_or(ViewNumber::init()),
            ),
        ))
    }

    /// Get the height of the highest committed block.
    pub fn highest_committed_block_height(&self) -> Result<Option<BlockHeight>, BlockTreeError> {
        let highest_committed_block = self.highest_committed_block()?;
        if let Some(block) = highest_committed_block {
            Ok(self.block_height(&block)?)
        } else {
            Ok(None)
        }
    }
}

/// Errors that may be encountered when reading or writing to the [`BlockTreeSingleton`].
#[derive(Debug)]
pub enum BlockTreeError {
    /// Error when trying to get a value from the block tree's underlying [key value store][KVStore].
    KVGetError(KVGetError),

    /// Error when trying set a value into block tree's underlying key value store.
    KVSetError(KVSetError),

    /// Unable to find a block with the specific `CryptoHash`, even though an invariant that the block tree
    /// expects to be maintained suggests that the block should exist.
    BlockExpectedButNotFound { block: CryptoHash },
}

impl From<KVGetError> for BlockTreeError {
    fn from(value: KVGetError) -> Self {
        BlockTreeError::KVGetError(value)
    }
}

impl From<KVSetError> for BlockTreeError {
    fn from(value: KVSetError) -> Self {
        BlockTreeError::KVSetError(value)
    }
}

pub struct BlockTreeWriteBatch<W: WriteBatch>(pub(super) W);

impl<W: WriteBatch> BlockTreeWriteBatch<W> {
    pub(crate) fn new() -> BlockTreeWriteBatch<W> {
        BlockTreeWriteBatch(W::new())
    }

    pub fn new_unsafe() -> BlockTreeWriteBatch<W> {
        Self::new()
    }

    /* ↓↓↓ Block ↓↓↓  */

    pub fn set_block(&mut self, block: &Block) -> Result<(), BlockTreeError> {
        let block_prefix = concat(&variables::BLOCKS, &block.hash.bytes());

        self.0.set(
            &concat(&block_prefix, &variables::BLOCK_HEIGHT),
            &block
                .height
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::BlockHeight {
                        block: block.hash.clone(),
                    },
                    source: err,
                })?,
        );
        self.0.set(
            &concat(&block_prefix, &variables::BLOCK_JUSTIFY),
            &block
                .justify
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::BlockJustify {
                        block: block.hash.clone(),
                    },
                    source: err,
                })?,
        );
        self.0.set(
            &concat(&block_prefix, &variables::BLOCK_DATA_HASH),
            &block
                .data_hash
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::BlockDataHash {
                        block: block.hash.clone(),
                    },
                    source: err,
                })?,
        );
        self.0.set(
            &concat(&block_prefix, &variables::BLOCK_DATA_LEN),
            &block
                .data
                .len()
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::BlockDataLength {
                        block: block.hash.clone(),
                    },
                    source: err,
                })?,
        );

        // Insert datums.
        let block_data_prefix = concat(&block_prefix, &variables::BLOCK_DATA);
        for (i, datum) in block.data.iter().enumerate() {
            let datum_key = concat(
                &block_data_prefix,
                &(i as u32)
                    .try_to_vec()
                    .map_err(|err| KVSetError::SerializeValueError {
                        key: Key::BlockData {
                            block: block.hash.clone(),
                        },
                        source: err,
                    })?,
            );
            self.0.set(&datum_key, datum.bytes());
        }

        Ok(())
    }

    pub fn delete_block(&mut self, block: &CryptoHash, data_len: DataLen) {
        let block_prefix = concat(&variables::BLOCKS, &block.bytes());

        self.0
            .delete(&concat(&block_prefix, &variables::BLOCK_HEIGHT));
        self.0
            .delete(&concat(&block_prefix, &variables::BLOCK_JUSTIFY));
        self.0
            .delete(&concat(&block_prefix, &variables::BLOCK_DATA_HASH));
        self.0
            .delete(&concat(&block_prefix, &variables::BLOCK_DATA_LEN));

        let block_data_prefix = concat(&block_prefix, &variables::BLOCK_DATA);
        for i in 0..data_len.int() {
            let datum_key = concat(&block_data_prefix, &i.try_to_vec().unwrap());
            self.0.delete(&datum_key);
        }
    }

    /* ↓↓↓ Block at Height ↓↓↓ */

    pub fn set_block_at_height(
        &mut self,
        height: BlockHeight,
        block: &CryptoHash,
    ) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &concat(&variables::BLOCK_AT_HEIGHT, &height.try_to_vec().unwrap()),
            &block
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::BlockAtHeight { height },
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Block to Children ↓↓↓ */

    pub fn set_children(
        &mut self,
        block: &CryptoHash,
        children: &ChildrenList,
    ) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &concat(&variables::BLOCK_TO_CHILDREN, &block.bytes()),
            &children
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::BlockChildren {
                        block: block.clone(),
                    },
                    source: err,
                })?,
        ))
    }

    pub fn delete_children(&mut self, block: &CryptoHash) {
        self.0
            .delete(&concat(&variables::BLOCK_TO_CHILDREN, &block.bytes()));
    }

    /* ↓↓↓ Committed App State ↓↓↓ */

    pub fn set_committed_app_state(&mut self, key: &[u8], value: &[u8]) {
        self.0
            .set(&concat(&variables::COMMITTED_APP_STATE, key), value);
    }

    pub fn delete_committed_app_state(&mut self, key: &[u8]) {
        self.0.delete(&concat(&variables::COMMITTED_APP_STATE, key));
    }

    /* ↓↓↓ Pending App State Updates ↓↓↓ */

    pub fn set_pending_app_state_updates(
        &mut self,
        block: &CryptoHash,
        app_state_updates: &AppStateUpdates,
    ) -> Result<(), KVSetError> {
        Ok(self.0.set(
            &concat(&variables::PENDING_APP_STATE_UPDATES, &block.bytes()),
            &app_state_updates
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::PendingAppStateUpdates {
                        block: block.clone(),
                    },
                    source: err,
                })?,
        ))
    }

    pub fn apply_app_state_updates(&mut self, app_state_updates: &AppStateUpdates) {
        for (key, value) in app_state_updates.inserts() {
            self.set_committed_app_state(key, value);
        }

        for key in app_state_updates.deletes() {
            self.delete_committed_app_state(key);
        }
    }

    pub fn delete_pending_app_state_updates(&mut self, block: &CryptoHash) {
        self.0.delete(&concat(
            &variables::PENDING_APP_STATE_UPDATES,
            &block.bytes(),
        ));
    }

    /* ↓↓↓ Commmitted Validator Set */

    pub fn set_committed_validator_set(
        &mut self,
        validator_set: &ValidatorSet,
    ) -> Result<(), BlockTreeError> {
        let validator_set_bytes: ValidatorSetBytes = validator_set.into();
        Ok(self.0.set(
            &variables::COMMITTED_VALIDATOR_SET,
            &validator_set_bytes
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::CommittedValidatorSet,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Pending Validator Set Updates */

    pub fn set_pending_validator_set_updates(
        &mut self,
        block: &CryptoHash,
        validator_set_updates: &ValidatorSetUpdates,
    ) -> Result<(), BlockTreeError> {
        let block_vs_updates_bytes =
            ValidatorSetUpdatesStatusBytes::Pending(validator_set_updates.into());
        Ok(self.0.set(
            &concat(&variables::VALIDATOR_SET_UPDATES_STATUS, &block.bytes()),
            &block_vs_updates_bytes.try_to_vec().map_err(|err| {
                KVSetError::SerializeValueError {
                    key: Key::ValidatorSetUpdatesStatus {
                        block: block.clone(),
                    },
                    source: err,
                }
            })?,
        ))
    }

    pub fn set_committed_validator_set_updates(
        &mut self,
        block: &CryptoHash,
    ) -> Result<(), BlockTreeError> {
        let block_vs_updates_bytes = ValidatorSetUpdatesStatusBytes::Committed;
        Ok(self.0.set(
            &concat(&variables::VALIDATOR_SET_UPDATES_STATUS, &block.bytes()),
            &block_vs_updates_bytes.try_to_vec().map_err(|err| {
                KVSetError::SerializeValueError {
                    key: Key::ValidatorSetUpdatesStatus {
                        block: block.clone(),
                    },
                    source: err,
                }
            })?,
        ))
    }

    pub fn delete_block_validator_set_updates(&mut self, block: &CryptoHash) {
        self.0.delete(&concat(
            &variables::VALIDATOR_SET_UPDATES_STATUS,
            &block.bytes(),
        ))
    }

    /* ↓↓↓ Locked PC ↓↓↓ */

    pub fn set_locked_pc(&mut self, pc: &PhaseCertificate) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::LOCKED_PC,
            &pc.try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::LockedPC,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Highest View Entered ↓↓↓ */

    pub fn set_highest_view_entered(&mut self, view: ViewNumber) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::HIGHEST_VIEW_ENTERED,
            &view
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Highest Phase Certificate ↓↓↓ */

    pub fn set_highest_pc(&mut self, pc: &PhaseCertificate) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::HIGHEST_PC,
            &pc.try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Highest Committed Block ↓↓↓ */

    pub fn set_highest_committed_block(
        &mut self,
        block: &CryptoHash,
    ) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::HIGHEST_COMMITTED_BLOCK,
            &block
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestCommittedBlock,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Newest Block ↓↓↓ */

    pub fn set_newest_block(&mut self, block: &CryptoHash) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::NEWEST_BLOCK,
            &block
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::NewestBlock,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Highest Timeout Certificate ↓↓↓ */

    pub fn set_highest_tc(&mut self, tc: &TimeoutCertificate) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::HIGHEST_TC,
            &tc.try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Previous Validator Set  ↓↓↓ */
    pub fn set_previous_validator_set(
        &mut self,
        validator_set: &ValidatorSet,
    ) -> Result<(), BlockTreeError> {
        let validator_set_bytes: ValidatorSetBytes = validator_set.into();
        Ok(self.0.set(
            &variables::PREVIOUS_VALIDATOR_SET,
            &validator_set_bytes
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::PreviousValidatorSet,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Validator Set Update Block Height ↓↓↓ */
    pub fn set_validator_set_update_block_height(
        &mut self,
        height: BlockHeight,
    ) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::VALIDATOR_SET_UPDATE_BLOCK_HEIGHT,
            &height
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::ValidatorSetUpdateHeight,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Validator Set Update Decided ↓↓↓ */

    pub fn set_validator_set_update_decided(
        &mut self,
        update_complete: bool,
    ) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::VALIDATOR_SET_UPDATE_DECIDED,
            &update_complete
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::ValidatorSetUpdateDecided,
                    source: err,
                })?,
        ))
    }

    /* ↓↓↓ Highest View Phase-Voted ↓↓↓ */

    pub fn set_highest_view_phase_voted(&mut self, view: ViewNumber) -> Result<(), BlockTreeError> {
        Ok(self.0.set(
            &variables::HIGHEST_VIEW_PHASE_VOTED,
            &view
                .try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestViewPhaseVoted,
                    source: err,
                })?,
        ))
    }
}

/// Error when writing a key-value pair to the [write batch][BlockTreeWriteBatch].
/// The error may arise when the value cannot be serialized, and hence cannot be
/// written to the write batch.
#[derive(Debug)]
pub enum KVSetError {
    SerializeValueError { key: Key, source: std::io::Error },
}

/// MonadBFT methods on BlockTreeSingleton.
impl<K: KVStore> BlockTreeSingleton<K> {
    /// Get the validator's local_tip: the header of the latest fresh proposal it voted for.
    pub fn local_tip(&self) -> Result<Option<crate::pacemaker::types::TipInfo>, BlockTreeError> {
        use borsh::BorshDeserialize;
        if let Some(bytes) = self.0.get(&variables::LOCAL_TIP) {
            let tip = crate::pacemaker::types::TipInfo::deserialize(&mut bytes.as_slice())
                .map_err(|err| KVGetError::DeserializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?;
            Ok(Some(tip))
        } else {
            Ok(None)
        }
    }

    /// Set the validator's local_tip after voting for a fresh proposal.
    pub fn set_local_tip(&mut self, tip: &crate::pacemaker::types::TipInfo) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
        wb.0.set(
            &variables::LOCAL_TIP,
            &tip.try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        );
        self.write(wb);
        Ok(())
    }

    /// Get the highest QC for inclusion in a timeout vote.
    pub fn highest_qc_for_timeout(&self) -> Result<Option<crate::hotstuff::types::PhaseCertificate>, BlockTreeError> {
        let highest_pc = self.highest_pc()?;
        if highest_pc.is_genesis_pc() {
            return Ok(None);
        }
        let local_tip = self.local_tip()?;
        match local_tip {
            Some(tip) if highest_pc.view < tip.view => Ok(None),
            _ => Ok(Some(highest_pc)),
        }
    }

    /// MonadBFT B2: Get the (view, block_hash) of the last proposal this validator voted for.
    pub fn last_voted_proposal(&self) -> Result<Option<(ViewNumber, CryptoHash)>, BlockTreeError> {
        use borsh::BorshDeserialize;
        if let Some(bytes) = self.0.get(&variables::LAST_VOTED_PROPOSAL) {
            let pair = <(ViewNumber, CryptoHash)>::deserialize(&mut bytes.as_slice())
                .map_err(|err| KVGetError::DeserializeValueError {
                    key: Key::HighestTC, // reuse key enum
                    source: err,
                })?;
            Ok(Some(pair))
        } else {
            Ok(None)
        }
    }

    /// MonadBFT B2: Record the (view, block_hash) of the proposal we just voted for.
    pub fn set_last_voted_proposal(&mut self, view: ViewNumber, block: CryptoHash) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
        wb.0.set(
            &variables::LAST_VOTED_PROPOSAL,
            &(view, block).try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        );
        self.write(wb);
        Ok(())
    }

    /// MonadBFT B2: Get the list of speculatively committed blocks.
    pub fn speculative_commits(&self) -> Result<Vec<CryptoHash>, BlockTreeError> {
        use borsh::BorshDeserialize;
        if let Some(bytes) = self.0.get(&variables::SPECULATIVE_COMMITS) {
            let hashes = Vec::<CryptoHash>::deserialize(&mut bytes.as_slice())
                .map_err(|err| KVGetError::DeserializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?;
            Ok(hashes)
        } else {
            Ok(Vec::new())
        }
    }

    /// MonadBFT B2: Add a block to the speculative commits list.
    pub fn add_speculative_commit(&mut self, block: CryptoHash) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut commits = self.speculative_commits()?;
        if !commits.contains(&block) {
            commits.push(block);
            let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
            wb.0.set(
                &variables::SPECULATIVE_COMMITS,
                &commits.try_to_vec()
                    .map_err(|err| KVSetError::SerializeValueError {
                        key: Key::HighestTC,
                        source: err,
                    })?,
            );
            self.write(wb);
        }
        Ok(())
    }

    /// MonadBFT B2: Remove irrevocably committed blocks from speculative list.
    pub fn promote_speculative_to_irrevocable(&mut self, block: &CryptoHash) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut commits = self.speculative_commits()?;
        commits.retain(|b| b != block);
        let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
        wb.0.set(
            &variables::SPECULATIVE_COMMITS,
            &commits.try_to_vec()
                .map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
        );
        self.write(wb);
        Ok(())
    }

    /// MonadBFT B2: Query whether a block is speculatively committed (but not yet irrevocable).
    pub fn is_speculatively_committed(&self, block: &CryptoHash) -> Result<bool, BlockTreeError> {
        Ok(self.speculative_commits()?.contains(block))
    }

    /// MonadBFT B2: Query whether a block is irrevocably committed.
    pub fn is_irrevocably_committed(&self, block: &CryptoHash) -> Result<bool, BlockTreeError> {
        if let Some(highest) = self.highest_committed_block()? {
            if let (Some(block_height), Some(highest_height)) = (
                self.block_height(block)?,
                self.block_height(&highest)?,
            ) {
                return Ok(block_height <= highest_height);
            }
        }
        Ok(false)
    }

    /// MonadBFT B2: Get the latest speculatively committed block not yet irrevocable.
    /// B3 will use this for rollback.
    pub fn latest_speculative_non_irrevocable(&self) -> Result<Option<CryptoHash>, BlockTreeError> {
        let commits = self.speculative_commits()?;
        // Return the last one added (most recent).
        Ok(commits.last().cloned())
    }

    // ========================================================================
    // MonadBFT B3: Speculative Rollback
    // ========================================================================

    /// Remove a block from the speculative commits list (rollback).
    ///
    /// Called when equivocation is detected for a speculatively committed block.
    /// The block itself is NOT deleted from the block tree (it's needed as evidence).
    /// Only the speculative commit status is reverted.
    pub fn rollback_speculative_block(
        &mut self,
        block: &CryptoHash,
    ) -> Result<bool, BlockTreeError> {
        use borsh::BorshSerialize;
        let mut commits = self.speculative_commits()?;
        let was_speculative = commits.contains(block);
        if was_speculative {
            commits.retain(|b| b != block);
            let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
            wb.0.set(
                &variables::SPECULATIVE_COMMITS,
                &commits.try_to_vec().map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
            );
            self.write(wb);
        }
        Ok(was_speculative)
    }

    /// Persist equivocation evidence in the block tree.
    ///
    /// Evidence survives rollback — it's stored separately from the rolled-back
    /// block's state and is needed for slashing and audit.
    pub fn store_equivocation_evidence(
        &mut self,
        evidence: &crate::hotstuff::types::EquivocationEvidence,
    ) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut existing = self.get_equivocation_evidence()?;
        let entry = (
            evidence.view,
            evidence.leader.to_bytes(),
            evidence.block_a,
            evidence.block_b,
        );
        if !existing.contains(&entry) {
            existing.push(entry);
            let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
            wb.0.set(
                &variables::EQUIVOCATION_EVIDENCE,
                &existing.try_to_vec().map_err(|err| KVSetError::SerializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?,
            );
            self.write(wb);
        }
        Ok(())
    }

    /// Get all stored equivocation evidence.
    pub fn get_equivocation_evidence(
        &self,
    ) -> Result<Vec<(ViewNumber, [u8; 32], CryptoHash, CryptoHash)>, BlockTreeError> {
        use borsh::BorshDeserialize;
        if let Some(bytes) = self.0.get(&variables::EQUIVOCATION_EVIDENCE) {
            let evidence =
                Vec::<(ViewNumber, [u8; 32], CryptoHash, CryptoHash)>::deserialize(
                    &mut bytes.as_slice(),
                )
                .map_err(|err| KVGetError::DeserializeValueError {
                    key: Key::HighestTC,
                    source: err,
                })?;
            Ok(evidence)
        } else {
            Ok(Vec::new())
        }
    }

    // ========================================================================
    // MonadBFT B3: Leader Reputation
    // ========================================================================

    /// Get the current leader reputation scores.
    pub fn leader_reputation(
        &self,
    ) -> Result<crate::hotstuff::types::LeaderReputation, BlockTreeError> {
        use borsh::BorshDeserialize;
        // Write-through cache: serve warm reads from memory (the hot path reads this
        // ~9x/round). Cold cache -> load from the KV (or the default) once and
        // memoize. Every write goes through `set_leader_reputation`, which refreshes
        // the cache in lock-step with the KV, so a HIT equals a fresh KV deserialize.
        {
            let cached = self.1.borrow();
            if let Some(rep) = cached.as_ref() {
                return Ok(rep.clone());
            }
        }
        let rep = if let Some(bytes) = self.0.get(&variables::LEADER_REPUTATION) {
            crate::hotstuff::types::LeaderReputation::deserialize(&mut bytes.as_slice()).map_err(
                |err| KVGetError::DeserializeValueError {
                    key: Key::HighestTC,
                    source: err,
                },
            )?
        } else {
            crate::hotstuff::types::LeaderReputation::new(100)
        };
        *self.1.borrow_mut() = Some(rep.clone());
        Ok(rep)
    }

    /// Set the leader reputation scores.
    pub fn set_leader_reputation(
        &mut self,
        reputation: &crate::hotstuff::types::LeaderReputation,
    ) -> Result<(), BlockTreeError> {
        use borsh::BorshSerialize;
        let mut wb: BlockTreeWriteBatch<K::WriteBatch> = BlockTreeWriteBatch::new();
        wb.0.set(
            &variables::LEADER_REPUTATION,
            &reputation.try_to_vec().map_err(|err| KVSetError::SerializeValueError {
                key: Key::HighestTC,
                source: err,
            })?,
        );
        self.write(wb);
        // Write-through: keep the in-mem cache identical to what we just persisted,
        // so subsequent reads are served from memory without diverging from the KV.
        *self.1.borrow_mut() = Some(reputation.clone());
        Ok(())
    }

    /// Record a successful block production for reputation tracking.
    /// Called when a block is irrevocably committed.
    pub fn record_leader_success(
        &mut self,
        leader: &VerifyingKey,
    ) -> Result<(), BlockTreeError> {
        let mut rep = self.leader_reputation()?;
        rep.record_success(leader);
        self.set_leader_reputation(&rep)
    }

    /// Record a timeout for reputation tracking.
    /// Called when a TC is formed (view timed out).
    pub fn record_leader_timeout(
        &mut self,
        leader: &VerifyingKey,
    ) -> Result<(), BlockTreeError> {
        let mut rep = self.leader_reputation()?;
        rep.record_timeout(leader);
        // Decay periodically: every `window_size` total events.
        let total_events: u32 = rep.entries.iter().map(|(_, e)| e.total).sum();
        if total_events > 0 && total_events % rep.window_size == 0 {
            rep.decay();
        }
        self.set_leader_reputation(&rep)
    }
}

#[cfg(test)]
mod leader_rep_cache_tests {
    use super::*;
    use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
    use borsh::BorshDeserialize;
    use ed25519_dalek::SigningKey;
    use std::collections::HashMap;

    /// Minimal in-memory `KVStore` for block-tree unit tests. Counts reads of the
    /// `LEADER_REPUTATION` key so a test can prove the in-mem cache (not the KV)
    /// serves repeated reads.
    #[derive(Clone, Default)]
    struct MemKV {
        map: HashMap<Vec<u8>, Vec<u8>>,
        lr_gets: std::cell::Cell<usize>,
    }

    struct MemWb {
        sets: Vec<(Vec<u8>, Vec<u8>)>,
        deletes: Vec<Vec<u8>>,
    }

    #[derive(Clone)]
    struct MemSnap(HashMap<Vec<u8>, Vec<u8>>);

    impl WriteBatch for MemWb {
        fn new() -> Self {
            Self { sets: Vec::new(), deletes: Vec::new() }
        }
        fn set(&mut self, key: &[u8], value: &[u8]) {
            self.sets.push((key.to_vec(), value.to_vec()));
        }
        fn delete(&mut self, key: &[u8]) {
            self.deletes.push(key.to_vec());
        }
    }

    impl KVGet for MemKV {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            if key == &variables::LEADER_REPUTATION[..] {
                self.lr_gets.set(self.lr_gets.get() + 1);
            }
            self.map.get(key).cloned()
        }
    }

    impl KVGet for MemSnap {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.get(key).cloned()
        }
    }

    impl KVStore for MemKV {
        type WriteBatch = MemWb;
        type Snapshot<'a> = MemSnap;
        fn write(&mut self, wb: MemWb) {
            for (k, v) in wb.sets {
                self.map.insert(k, v);
            }
            for k in wb.deletes {
                self.map.remove(&k);
            }
        }
        fn clear(&mut self) {
            self.map.clear();
        }
        fn snapshot<'b>(&'b self) -> MemSnap {
            MemSnap(self.map.clone())
        }
    }

    fn vk(seed: u8) -> VerifyingKey {
        SigningKey::from_bytes(&[seed; 32]).verifying_key()
    }

    /// Write-through cache invariant: a recorded reputation bump is visible via the
    /// (cached) `leader_reputation()`, the cached value byte-equals a fresh borsh
    /// deserialize straight from the KV, AND repeated reads are served from memory
    /// (no KV get for the reputation key after warm-up).
    #[test]
    fn leader_rep_cache_write_through() {
        let mut bt = BlockTreeSingleton::new(MemKV::default());
        let leader = vk(1);

        bt.record_leader_success(&leader).unwrap();

        // (1) The read reflects the bump.
        let cached = bt.leader_reputation().unwrap();
        let entry = cached
            .entries
            .iter()
            .find(|(k, _)| *k == leader.to_bytes())
            .expect("leader recorded");
        assert_eq!((entry.1.successes, entry.1.total), (1, 1));

        // (2) cache == KV: a fresh borsh deserialize straight from the KV equals the
        // cached value (write-through kept them identical).
        let raw = bt
            .0
            .map
            .get(&variables::LEADER_REPUTATION[..])
            .expect("reputation persisted to KV")
            .clone();
        let from_kv =
            crate::hotstuff::types::LeaderReputation::deserialize(&mut raw.as_slice()).unwrap();
        assert_eq!(from_kv, cached, "cache must byte-equal a fresh KV deserialize");

        // (3) Reads are served from the in-mem cache: after warm-up, repeated
        // `leader_reputation()` calls issue NO KV get for the reputation key.
        let _ = bt.leader_reputation().unwrap(); // ensure warm
        bt.0.lr_gets.set(0);
        for _ in 0..5 {
            let _ = bt.leader_reputation().unwrap();
        }
        assert_eq!(
            bt.0.lr_gets.get(),
            0,
            "warm reads must be served from the in-mem cache, not the KV"
        );
    }

    /// GATE (leader-rep-kv-cache T2): the write-through cache must be fork-safe.
    /// (a) a cold cache (fresh `BlockTreeSingleton` over the same KV = a restart)
    /// yields scores byte-identical to the warm cache; (b) reputation-weighted
    /// leader selection picks the same leader warm vs cold for a fixed
    /// (view, validator_set); (c) periodic `decay()` fires identically through the
    /// cache.
    #[test]
    fn leader_rep_cache_deterministic() {
        use crate::pacemaker::implementation::select_leader_reputation_weighted;
        use crate::types::data_types::{Power, ViewNumber};
        use crate::types::update_sets::ValidatorSetUpdates;
        use crate::types::validator_set::ValidatorSet;

        let v0 = vk(10);
        let v1 = vk(11);
        let v2 = vk(12);
        let validator_set = {
            let mut vs = ValidatorSet::new();
            let mut up = ValidatorSetUpdates::new();
            up.insert(v0, Power::new(100));
            up.insert(v1, Power::new(100));
            up.insert(v2, Power::new(100));
            vs.apply_updates(&up);
            vs
        };

        let mut bt = BlockTreeSingleton::new(MemKV::default());
        // Fixed event sequence crossing the decay boundary (window_size default 100):
        // v0 mostly succeeds, v1 mostly times out -> divergent, then decayed scores.
        for i in 0..120u32 {
            if i % 3 == 0 {
                bt.record_leader_timeout(&v1).unwrap();
            } else {
                bt.record_leader_success(&v0).unwrap();
            }
        }
        let warm = bt.leader_reputation().unwrap();

        // (a) Cold cache over the SAME KV (simulates a restart) == warm scores.
        let cold_bt = BlockTreeSingleton::new(bt.0.clone());
        let cold = cold_bt.leader_reputation().unwrap();
        assert_eq!(
            warm, cold,
            "cold-start reputation (loaded fresh from KV) must equal the warm cache"
        );

        // (b) Reputation-weighted leader selection is identical warm vs cold for a
        // fixed (view, validator_set), including reputation-influenced views (>=20).
        for view in [25u64, 99, 333, 1000] {
            let vn = ViewNumber::new(view);
            assert_eq!(
                select_leader_reputation_weighted(vn, &validator_set, &warm),
                select_leader_reputation_weighted(vn, &validator_set, &cold),
                "leader selection must be identical warm vs cold at view {view}"
            );
        }

        // (c) decay() fired at the window boundary (without decay v0 would have
        // exactly 80 successes/total): proves periodic decay ran through the cache.
        let e0 = warm
            .entries
            .iter()
            .find(|(k, _)| *k == v0.to_bytes())
            .expect("v0 present");
        assert!(
            e0.1.total < 80,
            "decay must have halved counters at the window boundary (v0.total={})",
            e0.1.total
        );
    }
}
