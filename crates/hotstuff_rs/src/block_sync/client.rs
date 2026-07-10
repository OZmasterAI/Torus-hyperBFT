/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Implements the [`BlockSyncClient`], which helps a replica catch up with the head of the blockchain
//! by requesting blocks from the sync server of another replica.
//!
//! The client is responsible for:
//! 1. Triggering Block Sync when:
//!     1. A replica has not made progress for a configurable amount of time.
//!     2. Or, sees evidence that others are ahead.
//! 2. Managing the list of peers available as sync servers and a blacklist for sync servers that have
//!    provided incorrect information in the past.
//! 3. Selecting a peer to sync with from the list of available peers.
//! 4. Dispatching sync fetch requests to the background [`BlockSyncWorker`](super::worker::BlockSyncWorker)
//!    and processing fetched blocks one at a time without blocking the algorithm loop.

use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc::{Receiver, Sender, TryRecvError},
    time::{Duration, Instant, SystemTime},
};

use ed25519_dalek::VerifyingKey;
use rand::seq::IteratorRandom;

use crate::{
    app::{App, ValidateBlockRequest, ValidateBlockResponse},
    block_sync::messages::{AdvertiseBlock, AdvertisePC, BlockSyncAdvertiseMessage},
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton, UpdateResult},
        invariants::{safe_pc, safe_pc_lock_clause},
        pluggables::KVStore,
    },
    events::{EndSyncEvent, Event, InsertBlockEvent, StartSyncEvent},
    hotstuff::types::PhaseCertificate,
    networking::network::{Network, ValidatorSetUpdateHandle},
    types::{
        block::Block,
        data_types::{BlockHeight, ChainID, CryptoHash, ViewNumber},
        signed_messages::{Certificate, SignedMessage},
        update_sets::ValidatorSetUpdates,
        validator_set::ValidatorSetUpdatesStatus,
    },
};

use super::worker::{SyncCommand, SyncResult};

const MAX_SYNC_ITERATIONS: u32 = 1000;
const SYNC_SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// s428 BACKFILL FIX. How soon to re-attempt a sync trigger that found ZERO
/// available sync servers. After a crash-restart the `available_sync_servers`
/// map is EMPTY (it is populated only by peers' periodic `AdvertiseBlock`
/// broadcasts — see `server.rs` `advertise_time`), so the first few triggers
/// after restart legitimately find no server. The pre-fix behaviour reset the
/// no-progress clock on those no-op triggers and let `futile_backoff` stretch
/// the next attempt to 60s..480s — the node then went permanently silent while
/// looking "live" at the tip, leaving unbackfilled holes (observed iter-3: the
/// "no available sync servers, cannot sync" log fired 3× then never again).
/// Instead, a no-server trigger schedules a SHORT, bounded retry so the client
/// re-attempts as soon as the first advertisement round arrives.
const NO_SERVER_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// s428 BACKFILL FIX. Throttle for the loud "still cannot backfill" warnings so
/// an unrecoverable gap (e.g. every peer has pruned the range) keeps screaming
/// at a readable cadence instead of either spamming every tick or — the old
/// failure — going silent.
const BACKFILL_STUCK_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// s428 BACKFILL FIX. Cadence for re-attempting a gap-driven backfill after a
/// futile attempt (peer pruned the range / returned nothing). Long enough not
/// to hammer a peer, short enough that a genuine hole heals in seconds once an
/// archival peer is reachable. Independent of the tip no-progress timeout.
const BACKFILL_RETRY_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct BlockSyncClient<N: Network> {
    config: BlockSyncClientConfiguration,
    worker_commands: Sender<SyncCommand>,
    worker_results: Receiver<SyncResult>,
    validator_set_update_handle: ValidatorSetUpdateHandle<N>,
    block_sync_client_state: BlockSyncClientState,
    event_publisher: Option<Sender<Event>>,
    pending_sync: Option<PendingSyncSession>,
}

struct PendingSyncSession {
    peer: VerifyingKey,
    pending_blocks: VecDeque<Block>,
    highest_pc: Option<PhaseCertificate>,
    blocks_synced: u64,
    init_height: BlockHeight,
    fetch_iterations: u32,
    session_start: Instant,
    awaiting_fetch: bool,
    /// Start height of the most recent fetch — a next fetch at the SAME height
    /// would return the identical range again (no commit progress was made from
    /// a full batch), so the session ends instead of looping (s350 FIX C).
    last_fetch_start_height: Option<BlockHeight>,
    /// s428 BACKFILL FIX. True when this session was started to fill a hole
    /// STRICTLY BELOW the committed frontier (rather than ordinary tip-sync).
    /// Backfill sessions legitimately insert far fewer blocks than the
    /// `advertised_height - init_height` span (most of the fetched range is
    /// already present and skipped), so the shortfall-blacklist in
    /// [`BlockSyncClient::check_commitment_and_end_session`] must NOT punish the
    /// serving peer for a backfill.
    is_backfill: bool,
}

impl<N: Network> BlockSyncClient<N> {
    pub(crate) fn new(
        config: BlockSyncClientConfiguration,
        worker_commands: Sender<SyncCommand>,
        worker_results: Receiver<SyncResult>,
        validator_set_update_handle: ValidatorSetUpdateHandle<N>,
        event_publisher: Option<Sender<Event>>,
    ) -> Self {
        Self {
            config,
            worker_commands,
            worker_results,
            validator_set_update_handle,
            block_sync_client_state: BlockSyncClientState::initialize(),
            event_publisher,
            pending_sync: None,
        }
    }

    pub(crate) fn has_pending_sync(&self) -> bool {
        self.pending_sync.is_some()
    }

    pub(crate) fn trigger_sync<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        if self.pending_sync.is_some() {
            return Ok(());
        }
        log::info!("block_sync: proposal-drop trigger fired (justify_block_known=false)");
        // s428 BACKFILL FIX: only consume the no-progress budget when a session
        // actually started. A no-server no-op keeps the short retry gate (set
        // inside `request_sync`) so we re-attempt promptly instead of resetting
        // the long timeout and going silent.
        if self.request_sync(block_tree)? == RequestOutcome::Started {
            self.block_sync_client_state.last_progress_or_sync_time = Instant::now();
        }
        Ok(())
    }

    /// Process a received [`BlockSyncAdvertiseMessage`].
    pub(crate) fn on_receive_msg<K: KVStore>(
        &mut self,
        msg: BlockSyncAdvertiseMessage,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        match msg {
            BlockSyncAdvertiseMessage::AdvertiseBlock(advertise_block) => {
                self.on_receive_advertise_block(advertise_block, origin, block_tree)
            }
            BlockSyncAdvertiseMessage::AdvertisePC(advertise_pc) => {
                self.on_receive_advertise_pc(advertise_pc, origin, block_tree)
            }
        }
    }

    /// Update internal state, trigger sync if timeout reached, and process worker results.
    pub(crate) fn tick<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        self.block_sync_client_state
            .remove_expired_blacklisted_servers();

        let highest_pc_view = block_tree.highest_pc()?.view;
        if highest_pc_view > self.block_sync_client_state.highest_pc_view {
            self.block_sync_client_state.highest_pc_view = highest_pc_view;
        }
        let committed_height = block_tree.highest_committed_block_height()?;
        if committed_height != self.block_sync_client_state.last_committed_height {
            self.block_sync_client_state.last_committed_height = committed_height;
            self.block_sync_client_state.last_progress_or_sync_time = Instant::now();
            // Real commit progress: restore the fast trigger (s350 FIX C).
            self.block_sync_client_state.consecutive_futile_sessions = 0;
        }

        // s350 FIX C: futile sessions stretch the re-trigger exponentially (60s →
        // up to 480s) — without this, a commit famine put every replica on a
        // permanent 30s-fetch/60s-cycle storm against its peers.
        let now = Instant::now();
        let scaled_trigger_timeout = self.config.block_sync_trigger_timeout
            * futile_backoff_multiplier(self.block_sync_client_state.consecutive_futile_sessions);

        // s428 BACKFILL FIX: three independent reasons to (re)attempt a sync, any
        // of which must break the post-restart silence:
        //  (a) the ordinary tip no-progress timeout;
        //  (b) a scheduled SHORT retry after a no-server no-op (advertisements
        //      arrive within `advertise_time` — do NOT wait out the long,
        //      backoff-scaled timeout, the iter-3 "3× then silent" failure);
        //  (c) a hole below the committed frontier (backfill), on its OWN cadence
        //      even while the node commits at the tip — timeout (a) is continually
        //      reset by tip progress and so, alone, NEVER heals a historical hole.
        if self.pending_sync.is_none() {
            let timeout_due = now
                .duration_since(self.block_sync_client_state.last_progress_or_sync_time)
                >= scaled_trigger_timeout;
            let no_server_due =
                no_server_retry_due(self.block_sync_client_state.no_server_retry_at, now);
            let backfill_due =
                self.backfill_attempt_due(now) && self.has_committed_gap(block_tree)?;

            if timeout_due || no_server_due || backfill_due {
                log::info!(
                    "block_sync: trigger (timeout={}, no_server_retry={}, backfill={}), available_servers={}, committed_height={:?}",
                    timeout_due,
                    no_server_due,
                    backfill_due,
                    self.block_sync_client_state.available_sync_servers.len(),
                    committed_height,
                );
                match self.request_sync(block_tree)? {
                    RequestOutcome::Started => {
                        self.block_sync_client_state.last_progress_or_sync_time = now;
                        self.block_sync_client_state.next_backfill_attempt_at =
                            Some(now + BACKFILL_RETRY_INTERVAL);
                    }
                    RequestOutcome::NoServer => {
                        // `request_sync` already set the short no-server retry
                        // gate; space out the backfill cadence too so we do not
                        // spin every tick while no server is available.
                        self.block_sync_client_state.next_backfill_attempt_at =
                            Some(now + BACKFILL_RETRY_INTERVAL);
                    }
                    RequestOutcome::AlreadyPending => {}
                }
            }
        };

        Ok(())
    }

    /// s428 BACKFILL FIX. Whether the throttled gap-driven backfill trigger may
    /// fire now.
    fn backfill_attempt_due(&self, now: Instant) -> bool {
        match self.block_sync_client_state.next_backfill_attempt_at {
            None => true,
            Some(t) => now >= t,
        }
    }

    /// s428 BACKFILL FIX. Whether the committed index has a hole strictly below
    /// the frontier that backfill should target. Advances the contiguous cursor
    /// as a side effect (cheap — resumes from the cursor, not the prune floor).
    fn has_committed_gap<K: KVStore>(
        &mut self,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<bool, BlockSyncClientError> {
        Ok(self.compute_sync_start(block_tree)?.1)
    }

    /// Non-blocking poll for worker results. Call this every algorithm loop iteration.
    pub(crate) fn poll_worker_results(&mut self) {
        let session = match &mut self.pending_sync {
            Some(s) if s.awaiting_fetch => s,
            _ => return,
        };

        match self.worker_results.try_recv() {
            Ok(SyncResult::Blocks {
                peer,
                blocks,
                highest_pc,
            }) => {
                if peer != session.peer {
                    return;
                }
                log::info!("block_sync: worker returned {} blocks", blocks.len());
                session.pending_blocks.extend(blocks);
                session.highest_pc = Some(highest_pc);
                session.awaiting_fetch = false;
            }
            Ok(SyncResult::Empty { peer }) => {
                if peer != session.peer {
                    return;
                }
                log::info!("block_sync: worker returned empty (sync complete)");
                self.check_commitment_and_end_session();
            }
            Ok(SyncResult::Error { peer }) => {
                if peer != session.peer {
                    return;
                }
                log::warn!("block_sync: worker fetch error (timeout/disconnect)");
                self.check_commitment_and_end_session();
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                log::error!("block_sync: worker channel disconnected");
                self.end_session();
            }
        }
    }

    /// Process one pending sync block. Returns true if work was done.
    pub(crate) fn process_pending_block<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<bool, BlockSyncClientError> {
        let has_work = self
            .pending_sync
            .as_ref()
            .is_some_and(|s| !s.awaiting_fetch && !s.pending_blocks.is_empty());
        if !has_work {
            // If session exists, not awaiting, and blocks empty → request next batch
            if let Some(session) = &self.pending_sync {
                if !session.awaiting_fetch && session.pending_blocks.is_empty() {
                    self.request_next_batch(block_tree)?;
                }
            }
            return Ok(false);
        }

        let session = self.pending_sync.as_mut().unwrap();

        // Skip blocks already in the tree
        while session
            .pending_blocks
            .front()
            .is_some_and(|b| block_tree.contains(&b.hash))
        {
            session.pending_blocks.pop_front();
        }

        let block = match session.pending_blocks.pop_front() {
            Some(b) => b,
            None => {
                // All remaining blocks were already known
                self.request_next_batch(block_tree)?;
                return Ok(true);
            }
        };

        let peer = session.peer;
        let chain_id = self.config.chain_id;

        // Validate cryptographic correctness only (hashes, signatures).
        // safe_block() is intentionally skipped: synced blocks come from the peer's
        // committed chain which may diverge from our locked_pc branch. The lock
        // rule prevents conflicting votes in live consensus but must not reject
        // valid committed blocks during catch-up.
        if !block.is_correct(block_tree)? {
            log::warn!("block_sync: block failed is_correct, blacklisting peer");
            self.block_sync_client_state
                .blacklist_sync_server(peer, self.config.blacklist_expiry_time);
            self.end_session();
            return Ok(true);
        }

        // P0 SAFETY: a synced block must never REPLACE a block we have already
        // COMMITTED. `safe_block`/the lock clause is deliberately skipped for
        // synced blocks (legitimate catch-up may extend a branch our lock trails
        // — the normal v3-restart case), but that relaxation may only advance us
        // FORWARD along a chain that extends our committed history. If a synced
        // block sits at a height we already finalized yet carries a DIFFERENT
        // hash, the peer's committed chain conflicts with ours: a local safety
        // violation (two honest quorums committed conflicting blocks). Refuse to
        // adopt it, tear down the session, and surface it loudly. `block_at_height`
        // is set ONLY by the commit walk, so this fires solely on committed
        // conflicts — never on uncommitted siblings during normal catch-up.
        if conflicts_with_committed(block_tree.block_at_height(block.height)?, block.hash) {
            let committed_hash = block_tree.block_at_height(block.height)?;
            log::error!(
                "block_sync: SAFETY VIOLATION — synced block {:?} at height {} conflicts with our \
                 COMMITTED block {:?} at the same height. Refusing to adopt the peer's divergent \
                 committed chain and halting this sync session.",
                block.hash,
                block.height.int(),
                committed_hash,
            );
            // FIX C2: surface the fatal safety violation to the host so it can
            // halt the process (torus-consensus latches `exec_failed`, which the
            // node binary watches and turns into `exit(70)`). Ending the sync
            // session alone would let the node keep voting/finalizing over an
            // already-forked chain. This is the ONLY block-sync condition wired as
            // fatal: the lock-clause observability warning below is LEGAL during
            // catch-up (no vote is cast on synced blocks) and is deliberately NOT
            // escalated.
            app.on_fatal_safety_violation();
            self.end_session();
            return Err(BlockSyncClientError::ConflictingCommittedChain {
                height: block.height,
                local: committed_hash,
                synced: block.hash,
            });
        }

        // T1.3 observability: since safe_pc is skipped above, its lock clause is skipped too. A
        // synced block whose justify conflicts with our locked_pc means the peer's committed
        // chain supersedes a branch this replica was locked on. That is legal during catch-up
        // (the lock rule exists to prevent conflicting *votes*, and no vote is cast on synced
        // blocks), but it must be visible when it happens. Log-only: errors are swallowed and
        // behavior is unchanged.
        if !safe_pc_lock_clause(&block.justify, block_tree).unwrap_or(true) {
            if let Ok(locked_pc) = block_tree.locked_pc() {
                log::warn!(
                    "block_sync: synced block {:?} justify (block={:?}, view={}) conflicts with local locked_pc (block={:?}, view={}) — proceeding with catch-up, lock superseded by peer's committed chain",
                    block.hash,
                    block.justify.block,
                    block.justify.view.int(),
                    locked_pc.block,
                    locked_pc.view.int(),
                );
            }
        }

        let parent_block = if block.justify.is_genesis_pc() {
            None
        } else {
            Some(&block.justify.block)
        };

        let validate_block_request =
            ValidateBlockRequest::new(&block, block_tree.app_view(parent_block)?);

        // A `MissingData` response means we lack this block's out-of-band data
        // (native-action bodies), NOT that the block or the serving peer is bad -- so
        // fetch the data instead of blacklisting. Blacklisting on a missing body
        // exhausted the peer set and wedged sync (livelock root cause, mem 28e1a821).
        let validation = app.validate_block_for_sync(validate_block_request);
        let should_blacklist = warrants_blacklist(&validation);

        if let ValidateBlockResponse::Valid {
            app_state_updates,
            validator_set_updates,
        } = validation
        {
            block_tree.insert(
                &block,
                app_state_updates.as_ref(),
                validator_set_updates.as_ref(),
            )?;
            Event::InsertBlock(InsertBlockEvent {
                timestamp: SystemTime::now(),
                block: block.clone(),
            })
            .publish(&self.event_publisher);

            let update_result = block_tree
                .update(&block.justify, &self.event_publisher)
                .unwrap_or_else(|e| {
                    log::warn!("block_sync: block_tree.update failed: {:?}", e);
                    UpdateResult {
                        validator_set_updates: None,
                        committed_block_hashes: vec![],
                    }
                });

            // Call on_committed_block for each newly committed block during sync.
            for committed_hash in &update_result.committed_block_hashes {
                if let Ok(Some(committed_block)) = block_tree.block(committed_hash) {
                    app.on_committed_block(&committed_block, *committed_hash);
                }
            }
            if let Some(vs_updates) = update_result.validator_set_updates {
                self.validator_set_update_handle
                    .update_validator_set(vs_updates)
            }

            let session = self.pending_sync.as_mut().unwrap();
            session.blocks_synced += 1;

            // Apply highest_pc from the response if valid
            if let Some(ref highest_pc) = session.highest_pc.clone() {
                if highest_pc.is_correct(block_tree)? && safe_pc(highest_pc, block_tree, chain_id)?
                {
                    let update_result2 = block_tree
                        .update(highest_pc, &self.event_publisher)
                        .unwrap_or(UpdateResult {
                            validator_set_updates: None,
                            committed_block_hashes: vec![],
                        });
                    for committed_hash in &update_result2.committed_block_hashes {
                        if let Ok(Some(committed_block)) = block_tree.block(committed_hash) {
                            app.on_committed_block(&committed_block, *committed_hash);
                        }
                    }
                    if let Some(vs_updates) = update_result2.validator_set_updates {
                        self.validator_set_update_handle
                            .update_validator_set(vs_updates);
                    }
                }
            }

            log::info!(
                "block_sync: block inserted, blocks_synced={}",
                self.pending_sync.as_ref().unwrap().blocks_synced
            );
        } else if should_blacklist {
            log::warn!("block_sync: block failed app validation (invalid), blacklisting peer");
            self.block_sync_client_state
                .blacklist_sync_server(peer, self.config.blacklist_expiry_time);
            self.end_session();
            return Ok(true);
        } else {
            log::warn!(
                "block_sync: block data missing during sync -- NOT blacklisting peer (will fetch)"
            );
            self.end_session();
            return Ok(true);
        }

        Ok(true)
    }

    fn on_receive_advertise_block<K: KVStore>(
        &mut self,
        advertise_block: AdvertiseBlock,
        origin: &VerifyingKey,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        if advertise_block.chain_id != self.config.chain_id || !advertise_block.is_correct(origin) {
            return Ok(());
        }
        if !is_sync_server_address(origin, block_tree)? {
            return Ok(());
        }
        if self
            .block_sync_client_state
            .blacklist_contains_server_address(origin)
        {
            return Ok(());
        }
        self.block_sync_client_state.register_or_update_sync_server(
            *origin,
            advertise_block.highest_committed_block_height,
        );
        Ok(())
    }

    fn on_receive_advertise_pc<K: KVStore>(
        &mut self,
        advertise_pc: AdvertisePC,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        let local_highest_pc_view = block_tree.highest_pc()?.view;
        if self
            .block_sync_client_state
            .blacklist_contains_server_address(origin)
        {
            return Ok(());
        }
        if advertise_pc.highest_pc.view < local_highest_pc_view {
            return Ok(());
        }
        let view_difference = (advertise_pc.highest_pc.view - local_highest_pc_view) as u64;
        if self.pending_sync.is_none()
            && view_difference >= self.config.block_sync_trigger_min_view_difference
            && advertise_pc.highest_pc.is_correct(block_tree)?
        {
            log::info!(
                "block_sync: advertise_pc trigger fired, remote_pc_view={}, local_pc_view={}, diff={}",
                advertise_pc.highest_pc.view.int(),
                local_highest_pc_view.int(),
                view_difference,
            );
            // s428 BACKFILL FIX: only consume the no-progress budget when a
            // session actually started (see `trigger_sync`).
            if self.request_sync(block_tree)? == RequestOutcome::Started {
                self.block_sync_client_state.last_progress_or_sync_time = Instant::now();
            }
        };
        Ok(())
    }

    /// s428 BACKFILL FIX. Compute the next fetch start height from the committed
    /// index, preferring the lowest hole below the frontier (backfill) over
    /// ordinary tip-sync, and advancing the contiguous-scan cursor as a side
    /// effect. Returns `(start_height, is_backfill)`.
    ///
    /// The scan starts just above `max(prune_floor, cursor)` so heights pruned
    /// below the retention horizon are never mistaken for holes and repeated
    /// ticks are O(new/unfilled heights), not O(chain).
    fn compute_sync_start<K: KVStore>(
        &mut self,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<(BlockHeight, bool), BlockSyncClientError> {
        let highest = block_tree.highest_committed_block_height()?;
        let floor = block_tree.block_tree_pruned_height()?;
        let scan_from = self
            .block_sync_client_state
            .backfill_contiguous_through
            .or(floor)
            .map(|h| h.int() + 1)
            .unwrap_or(0);

        let (start, is_backfill) = match highest {
            None => (0u64, false),
            Some(top) => {
                let scan = scan_first_gap(scan_from, top.int(), |h| {
                    block_tree
                        .block_at_height(BlockHeight::new(h))
                        .ok()
                        .flatten()
                        .is_some()
                });
                if let Some(c) = scan.contiguous_through {
                    let prev = self
                        .block_sync_client_state
                        .backfill_contiguous_through
                        .map(|h| h.int())
                        .unwrap_or(0);
                    if c > prev {
                        self.block_sync_client_state.backfill_contiguous_through =
                            Some(BlockHeight::new(c));
                    }
                }
                select_start_height(Some(top.int()), scan.first_gap)
            }
        };
        Ok((BlockHeight::new(start), is_backfill))
    }

    /// Select a peer and start a sync session by sending the first fetch command
    /// to the worker.
    ///
    /// s428 BACKFILL FIX: returns a [`RequestOutcome`] so callers keep the
    /// no-progress clock and no-server retry schedule correct — a no-server
    /// no-op must NOT consume the long timeout budget (that is exactly what made
    /// the client go silent after a crash-restart, iter-3). The start height is
    /// the lowest hole below the frontier when one exists (backfill), else the
    /// ordinary tip-sync `highest_committed + 1`.
    fn request_sync<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<RequestOutcome, BlockSyncClientError> {
        if self.pending_sync.is_some() {
            return Ok(RequestOutcome::AlreadyPending);
        }

        let highest_committed_block_height = block_tree.highest_committed_block_height()?;
        let (start_height, is_backfill) = self.compute_sync_start(block_tree)?;

        // The peer must be at/above our committed frontier (it may then also
        // retain the backfill range — unless it has pruned it, which surfaces at
        // fetch time as an Empty/futile response, not here). Backfill and
        // tip-sync use the same min-height filter.
        let peer = match self
            .block_sync_client_state
            .random_sync_server(&highest_committed_block_height)
        {
            Some(p) => p,
            None => {
                // s428 BACKFILL FIX: do NOT go silent. Schedule a short bounded
                // retry (advertisements arrive within `advertise_time`), count
                // the streak, and log loudly but throttled.
                self.block_sync_client_state.no_server_retry_at =
                    Some(Instant::now() + NO_SERVER_RETRY_INTERVAL);
                self.note_backfill_stuck(is_backfill, start_height, "no available sync servers");
                return Ok(RequestOutcome::NoServer);
            }
        };

        // A session is starting: clear the no-server retry gate.
        self.block_sync_client_state.no_server_retry_at = None;

        // `init_height` is the height just below the first fetched block (the
        // parent frontier for this session); BlockHeight has no saturating_sub,
        // so go through the raw int.
        let init_height = BlockHeight::new(start_height.int().saturating_sub(1));

        log::info!(
            "block_sync: starting {} with peer, committed_height={:?}, start_height={}",
            if is_backfill { "BACKFILL" } else { "sync" },
            highest_committed_block_height,
            start_height.int(),
        );

        Event::StartSync(StartSyncEvent {
            timestamp: SystemTime::now(),
            peer,
        })
        .publish(&self.event_publisher);

        let _ = self.worker_commands.send(SyncCommand::Fetch {
            peer,
            chain_id: self.config.chain_id,
            start_height,
            limit: self.config.request_limit,
        });

        self.pending_sync = Some(PendingSyncSession {
            peer,
            pending_blocks: VecDeque::new(),
            highest_pc: None,
            blocks_synced: 0,
            init_height,
            fetch_iterations: 1,
            session_start: Instant::now(),
            awaiting_fetch: true,
            last_fetch_start_height: Some(start_height),
            is_backfill,
        });

        Ok(RequestOutcome::Started)
    }

    /// s428 BACKFILL FIX. Throttled loud logging + no-progress streak accounting
    /// for a backfill that cannot proceed (no server, or a peer that keeps
    /// skipping the range because it pruned it). NEVER silent, NEVER a fail-stop:
    /// a lagging node with an unrecoverable local gap must keep retrying (an
    /// operator can re-provision an archival peer) while screaming visibly.
    fn note_backfill_stuck(&mut self, is_backfill: bool, start_height: BlockHeight, reason: &str) {
        self.block_sync_client_state.consecutive_no_progress_backfill = self
            .block_sync_client_state
            .consecutive_no_progress_backfill
            .saturating_add(1);

        let now = Instant::now();
        let due = match self.block_sync_client_state.last_backfill_stuck_log {
            None => true,
            Some(t) => now.duration_since(t) >= BACKFILL_STUCK_LOG_INTERVAL,
        };
        if due {
            self.block_sync_client_state.last_backfill_stuck_log = Some(now);
            let attempts = self.block_sync_client_state.consecutive_no_progress_backfill;
            if is_backfill {
                log::warn!(
                    "block_sync: BACKFILL BLOCKED at height {} ({}) — {} consecutive failed \
                     attempts; still retrying (will NOT go silent). If this persists no reachable \
                     peer retains this range; re-provision an archival peer.",
                    start_height.int(),
                    reason,
                    attempts,
                );
            } else {
                log::warn!(
                    "block_sync: cannot sync ({}) — {} consecutive attempts; retrying shortly.",
                    reason,
                    attempts,
                );
            }
        }
    }

    /// Send the next fetch command to the worker for the current session.
    fn request_next_batch<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        // Guard + per-batch iteration/deadline accounting (needs `&mut session`,
        // dropped before the committed-index scan below to satisfy the borrow
        // checker — `compute_sync_start` borrows `&mut self`).
        match &self.pending_sync {
            Some(s) if !s.awaiting_fetch => {}
            _ => return Ok(()),
        }
        {
            let session = self.pending_sync.as_mut().unwrap();
            session.fetch_iterations += 1;
            if session.fetch_iterations > MAX_SYNC_ITERATIONS {
                log::warn!("sync session hit max iteration limit");
                self.end_session();
                return Ok(());
            }
            if Instant::now() >= session.session_start + SYNC_SESSION_TIMEOUT {
                log::warn!("sync session hit time deadline");
                self.end_session();
                return Ok(());
            }
        }

        // s428 BACKFILL FIX: continue from the lowest remaining hole (or the
        // tip), NOT blindly from `highest_committed+1`. During an INTERIOR
        // backfill the committed frontier may not move as we insert gap blocks,
        // so the next batch must be anchored on the committed-index scan (which
        // advances its cursor as holes fill). For ordinary tip-sync this is
        // identical to the old `highest_committed+1`.
        let (start_height, _is_backfill) = self.compute_sync_start(block_tree)?;

        let last_start = self
            .pending_sync
            .as_ref()
            .and_then(|s| s.last_fetch_start_height)
            .map(|h| h.int());

        // s350 FIX C: the previous fetch started at the same height — a full
        // batch yielded zero forward progress (every block already
        // known/uncommittable, or the peer skips a pruned range), and refetching
        // the identical range can only loop until the deadline (observed s339:
        // the same 128-block range refetched >1000× per session, re-triggered
        // every 60s, indefinitely). End the session; the server was honest, so no
        // blacklist. For a backfill this also correctly stops when the target
        // hole is unfillable through the normal pipeline (see design notes).
        if refetch_would_repeat(last_start, start_height.int()) {
            log::info!(
                "block_sync: batch at height {} made no progress — ending session",
                start_height.int()
            );
            self.end_session();
            return Ok(());
        }

        let session = self.pending_sync.as_mut().unwrap();
        session.last_fetch_start_height = Some(start_height);
        let peer = session.peer;
        session.awaiting_fetch = true;

        let _ = self.worker_commands.send(SyncCommand::Fetch {
            peer,
            chain_id: self.config.chain_id,
            start_height,
            limit: self.config.request_limit,
        });

        Ok(())
    }

    /// Check if the peer met its advertised commitment, blacklist if not, then end the session.
    fn check_commitment_and_end_session(&mut self) {
        if let Some(ref session) = self.pending_sync {
            let peer = session.peer;
            // s428 BACKFILL FIX: a backfill fetches a range that is mostly already
            // present (only the hole is inserted), so `blocks_synced` is
            // legitimately far below `advertised_height - init_height`. Do NOT
            // blacklist an honest peer for a backfill "shortfall" — that would
            // exhaust the peer set and re-wedge sync (mem 28e1a821).
            if !session.is_backfill {
                if let Some(&advertised_height) = self
                    .block_sync_client_state
                    .available_sync_servers
                    .get(&peer)
                {
                    let min_blocks_expected = advertised_height - session.init_height;
                    if session.blocks_synced < min_blocks_expected {
                        self.block_sync_client_state
                            .blacklist_sync_server(peer, self.config.blacklist_expiry_time);
                    }
                }
            }
        }
        self.end_session();
    }

    fn end_session(&mut self) {
        if let Some(session) = self.pending_sync.take() {
            // s350 FIX C: zero-block sessions feed the trigger backoff; any
            // productive session resets it so a lagging replica stays fast.
            if session.blocks_synced == 0 {
                self.block_sync_client_state.consecutive_futile_sessions = self
                    .block_sync_client_state
                    .consecutive_futile_sessions
                    .saturating_add(1);
                // s428 BACKFILL FIX: a futile backfill re-arms its OWN cadence and
                // logs (throttled) so a hole no reachable peer can serve keeps
                // retrying loudly — never silent, never a fail-stop.
                if session.is_backfill {
                    let start = session
                        .last_fetch_start_height
                        .unwrap_or_else(|| BlockHeight::new(0));
                    self.block_sync_client_state.next_backfill_attempt_at =
                        Some(Instant::now() + BACKFILL_RETRY_INTERVAL);
                    self.note_backfill_stuck(true, start, "peer returned no fillable blocks");
                }
            } else {
                self.block_sync_client_state.consecutive_futile_sessions = 0;
                // s428 BACKFILL FIX: backfill made progress — clear the stuck
                // counters/gates so the next hole (if any) is attacked at once.
                self.block_sync_client_state.consecutive_no_progress_backfill = 0;
                self.block_sync_client_state.next_backfill_attempt_at = None;
                self.block_sync_client_state.no_server_retry_at = None;
            }
            Event::EndSync(EndSyncEvent {
                timestamp: SystemTime::now(),
                peer: session.peer,
                blocks_synced: session.blocks_synced,
            })
            .publish(&self.event_publisher);
        }
    }
}

pub(crate) struct BlockSyncClientConfiguration {
    pub(crate) chain_id: ChainID,
    pub(crate) request_limit: u32,
    pub(crate) response_timeout: Duration,
    pub(crate) blacklist_expiry_time: Duration,
    pub(crate) block_sync_trigger_min_view_difference: u64,
    pub(crate) block_sync_trigger_timeout: Duration,
}

struct BlockSyncClientState {
    available_sync_servers: HashMap<VerifyingKey, BlockHeight>,
    blacklist: VecDeque<(VerifyingKey, Instant)>,
    last_progress_or_sync_time: Instant,
    highest_pc_view: ViewNumber,
    last_committed_height: Option<BlockHeight>,
    /// Sessions in a row that synced zero blocks — scales the timeout trigger
    /// via [`futile_backoff_multiplier`]; reset on any commit progress or any
    /// session that inserts a block (s350 FIX C).
    consecutive_futile_sessions: u32,
    /// s428 BACKFILL FIX. When the most recent trigger found ZERO available sync
    /// servers, the next moment at which to re-attempt (a SHORT, bounded retry —
    /// see [`NO_SERVER_RETRY_INTERVAL`]). `None` when the last attempt actually
    /// started a session (or none has been attempted). This is deliberately
    /// separate from `last_progress_or_sync_time`/`futile_backoff` so a no-op
    /// no-server trigger can never consume the long timeout budget and go silent.
    no_server_retry_at: Option<Instant>,
    /// s428 BACKFILL FIX. Consecutive triggers that found no server / made no
    /// backfill progress — used only to throttle the loud "still stuck" log.
    consecutive_no_progress_backfill: u32,
    /// s428 BACKFILL FIX. Last time the loud "cannot backfill" warning was
    /// emitted (throttled by [`BACKFILL_STUCK_LOG_INTERVAL`]).
    last_backfill_stuck_log: Option<Instant>,
    /// s428 BACKFILL FIX. Monotonic cursor: the highest height H such that the
    /// committed index (`block_at_height`) is known contiguous through H (from
    /// the prune floor). Lets [`scan_first_gap`] resume from the cursor instead
    /// of rescanning from the floor every tick — O(new heights) amortised.
    backfill_contiguous_through: Option<BlockHeight>,
    /// s428 BACKFILL FIX. Throttle gate for the gap-driven backfill trigger.
    /// Unlike the ordinary no-progress timeout (which is continually reset while
    /// the node makes tip progress and therefore NEVER fires for a historical
    /// hole — the iter-3 failure), this fires on its own cadence so a rejoined
    /// node backfills below the frontier even while it commits at the tip. Set
    /// after a futile backfill attempt; cleared on backfill progress.
    next_backfill_attempt_at: Option<Instant>,
}

impl BlockSyncClientState {
    fn initialize() -> Self {
        Self {
            available_sync_servers: HashMap::new(),
            blacklist: VecDeque::new(),
            last_progress_or_sync_time: Instant::now(),
            highest_pc_view: ViewNumber::new(0),
            last_committed_height: None,
            consecutive_futile_sessions: 0,
            no_server_retry_at: None,
            consecutive_no_progress_backfill: 0,
            last_backfill_stuck_log: None,
            backfill_contiguous_through: None,
            next_backfill_attempt_at: None,
        }
    }

    fn blacklist_contains_server_address(&self, sync_server: &VerifyingKey) -> bool {
        self.blacklist.iter().any(|(vk, _)| vk == sync_server)
    }

    fn register_or_update_sync_server(
        &mut self,
        sync_server: VerifyingKey,
        highest_committed_block_height: BlockHeight,
    ) {
        let _ = self
            .available_sync_servers
            .insert(sync_server, highest_committed_block_height);
    }

    fn blacklist_sync_server(
        &mut self,
        sync_server: VerifyingKey,
        blacklist_expiry_time: Duration,
    ) {
        let _ = self.available_sync_servers.remove(&sync_server);
        self.blacklist
            .push_back((sync_server, Instant::now() + blacklist_expiry_time))
    }

    fn remove_expired_blacklisted_servers(&mut self) {
        let now = Instant::now();
        while self
            .blacklist
            .front()
            .is_some_and(|(_, expiry)| expiry <= &now)
        {
            let _ = self.blacklist.pop_front();
        }
    }

    fn random_sync_server(
        &self,
        min_highest_committed_block_height: &Option<BlockHeight>,
    ) -> Option<VerifyingKey> {
        match min_highest_committed_block_height {
            None => self
                .available_sync_servers
                .keys()
                .choose(&mut rand::thread_rng())
                .copied(),
            Some(min_height) => self
                .available_sync_servers
                .keys()
                .filter(|vk| {
                    self.available_sync_servers
                        .get(vk)
                        .is_some_and(|height| height >= min_height)
                })
                .choose(&mut rand::thread_rng())
                .copied(),
        }
    }
}

/// Whether a non-`Valid` sync validation response warrants blacklisting the serving
/// peer. Only a cryptographically/semantically `Invalid` block does. A `MissingData`
/// response means we simply lack the block's out-of-band data (e.g. native-action
/// bodies referenced by a CompactBlock): the peer served a correct block, so we must
/// FETCH the data and retry -- not punish the peer. Blacklisting on a missing body
/// exhausted the peer set and wedged sync (livelock root cause, mem 28e1a821).
/// P0 SAFETY predicate: does a synced block CONFLICT with our locally COMMITTED
/// chain?
///
/// `committed_at_height` is `block_tree.block_at_height(block.height)` — the hash
/// of the block we have already COMMITTED at that height (`block_at_height` is
/// written only by the commit walk), or `None` if we have not committed anything
/// at that height yet.
///
/// * `None` → the block is at (or above) our committed frontier: normal
///   catch-up / legitimate extension. NOT a conflict.
/// * `Some(h)` with `h == synced_block_hash` → a block we already committed is
///   being re-served. NOT a conflict.
/// * `Some(h)` with `h != synced_block_hash` → the peer committed a DIFFERENT
///   block at a height we already finalized. CONFLICT.
///
/// The criterion is conflict-with-COMMITTED, never conflict-with-lock, so a lock
/// that merely trails the peer's legitimately-advanced committed chain (the
/// v3-restart catch-up case) is not flagged.
fn conflicts_with_committed(
    committed_at_height: Option<CryptoHash>,
    synced_block_hash: CryptoHash,
) -> bool {
    matches!(committed_at_height, Some(h) if h != synced_block_hash)
}

fn warrants_blacklist(response: &ValidateBlockResponse) -> bool {
    matches!(response, ValidateBlockResponse::Invalid)
}

/// Whether issuing the next batch fetch at `next_start` would repeat the previous
/// fetch of this session — i.e. the last batch yielded NOTHING that advanced our
/// committed height (every block already known / uncommittable), so the same range
/// would be served again, forever.
///
/// s339/s350 livelock: post-load, every replica held the full speculative chain but
/// commits were famined; sessions refetched the identical 128-block range hundreds
/// of times per second until the 30s session deadline, re-triggering every 60s —
/// indefinitely. A repeat fetch can never make progress the previous one didn't:
/// end the session instead (the server was honest — do NOT blacklist).
fn refetch_would_repeat(prev_start: Option<u64>, next_start: u64) -> bool {
    prev_start == Some(next_start)
}

/// Trigger-timeout multiplier after `consecutive_futile` sync sessions that synced
/// zero blocks: 1, 2, 4, then capped at 8 (60s base → 480s max). Any session that
/// inserts a block — or any commit progress — resets the counter, so a genuinely
/// lagging replica keeps its fast trigger. Prevents the cluster-wide every-60s
/// fetch storm while commits are famined (s350).
fn futile_backoff_multiplier(consecutive_futile: u32) -> u32 {
    1u32 << consecutive_futile.min(3)
}

/// s428 BACKFILL FIX. Outcome of an attempt to (re)start a sync session, so the
/// caller can keep the no-progress clock and the no-server retry schedule
/// correct. The pre-fix code let a no-server no-op reset the long timeout clock
/// (see [`NO_SERVER_RETRY_INTERVAL`]) and then go silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestOutcome {
    /// A session was started (a fetch command was dispatched to the worker).
    Started,
    /// No available (non-blacklisted, high-enough) sync server — nothing was
    /// dispatched; a short bounded retry is scheduled instead of going silent.
    NoServer,
    /// A session was already in progress; the request was a no-op.
    AlreadyPending,
}

/// s428 BACKFILL FIX. Result of scanning the committed index for the lowest
/// hole below the frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GapScan {
    /// Lowest height in `(scan_from-1, highest]` whose committed block is absent,
    /// i.e. a hole below the committed frontier that must be backfilled. `None`
    /// when the committed prefix is contiguous through `highest`.
    first_gap: Option<u64>,
    /// Highest height proven contiguously present during this scan (the new
    /// cursor value). `None` when nothing at/above `scan_from` was present.
    contiguous_through: Option<u64>,
}

/// s428 BACKFILL FIX (pure, testable core). Scan `scan_from..=highest` over the
/// committed index probe `present(height)` and return the lowest absent height
/// (the backfill target) plus the highest contiguous height (the resume cursor).
///
/// Callers pass `scan_from = max(prune_floor, last_cursor) + 1` so pruned-away
/// heights below the retention horizon are NEVER mistaken for holes, and so the
/// scan is O(new/unfilled heights) rather than O(chain) every tick.
fn scan_first_gap(scan_from: u64, highest: u64, present: impl Fn(u64) -> bool) -> GapScan {
    let mut contiguous_through: Option<u64> = None;
    let mut h = scan_from;
    while h <= highest {
        if present(h) {
            contiguous_through = Some(h);
            h += 1;
        } else {
            return GapScan {
                first_gap: Some(h),
                contiguous_through,
            };
        }
    }
    GapScan {
        first_gap: None,
        contiguous_through,
    }
}

/// s428 BACKFILL FIX (pure, testable). Pick the height at which the next sync
/// fetch should start, and whether it is an interior BACKFILL (a hole strictly
/// below the frontier) rather than ordinary tip-sync.
///
/// * `highest_committed` — our committed frontier height (`None` = nothing
///   committed yet: start at genesis, height 0).
/// * `first_gap` — lowest missing committed height at/below the frontier, if any.
///
/// When a gap exists we fetch the GAP first (ascending, contiguous) — the whole
/// point of the fix: the pre-fix code always anchored at `highest_committed+1`
/// and so could never revisit a height below the frontier. When there is no
/// gap we tip-sync from `highest_committed+1` exactly as before.
fn select_start_height(highest_committed: Option<u64>, first_gap: Option<u64>) -> (u64, bool) {
    match (highest_committed, first_gap) {
        // A real hole below the frontier → backfill it first.
        (Some(_), Some(gap)) => (gap, true),
        // Contiguous prefix → ordinary tip-sync from the next height.
        (Some(top), None) => (top + 1, false),
        // Nothing committed yet → genesis.
        (None, _) => (0, false),
    }
}

/// s428 BACKFILL FIX (pure, testable). Whether a scheduled no-server retry is
/// due. A no-server trigger sets `retry_at = now + NO_SERVER_RETRY_INTERVAL`;
/// this returns true once that instant passes so the client re-attempts rather
/// than waiting out the (much longer, backoff-scaled) no-progress timeout.
fn no_server_retry_due(retry_at: Option<Instant>, now: Instant) -> bool {
    matches!(retry_at, Some(t) if now >= t)
}

#[cfg(test)]
mod sync_livelock_guard_tests {
    use super::{futile_backoff_multiplier, refetch_would_repeat};

    /// s350 FIX C (RED first): a batch request at the same start height as the
    /// previous fetch means zero commit progress was made from a full batch — the
    /// session must end, not refetch the identical range until the deadline.
    /// MUST fail before FIX C lands (no `refetch_would_repeat`).
    #[test]
    fn repeat_fetch_ends_session() {
        // First fetch of a session: nothing to repeat.
        assert!(!refetch_would_repeat(None, 25813));
        // Commit progress advanced the start height: keep syncing.
        assert!(!refetch_would_repeat(Some(25813), 25941));
        // Same start as the last fetch: the famine loop — stop here.
        assert!(refetch_would_repeat(Some(25813), 25813));
    }

    /// s350 FIX C: futile sessions back the timeout trigger off exponentially,
    /// capped at 8× (60s → 480s); productive sessions reset to 1×.
    #[test]
    fn futile_sessions_back_off_capped() {
        assert_eq!(futile_backoff_multiplier(0), 1, "healthy: base trigger");
        assert_eq!(futile_backoff_multiplier(1), 2);
        assert_eq!(futile_backoff_multiplier(2), 4);
        assert_eq!(futile_backoff_multiplier(3), 8);
        assert_eq!(futile_backoff_multiplier(10), 8, "capped — never unbounded");
        assert_eq!(futile_backoff_multiplier(u32::MAX), 8, "no shift overflow");
    }
}

#[cfg(test)]
mod committed_conflict_tests {
    use super::conflicts_with_committed;
    use crate::types::data_types::CryptoHash;

    /// P0 SAFETY (RED first — `conflicts_with_committed` did not exist): the sync
    /// adoption guard must fire ONLY when a synced block conflicts with a block
    /// we have already COMMITTED at the same height, never on legitimate
    /// catch-up.
    #[test]
    fn conflict_criterion_is_conflict_with_committed_not_extension() {
        let synced = CryptoHash::new([7u8; 32]);

        // Nothing committed at this height yet → legitimate extension / catch-up
        // (the v3-restart-from-peers case). NOT a conflict.
        assert!(
            !conflicts_with_committed(None, synced),
            "a height at/above our committed frontier must be treated as catch-up, not conflict"
        );

        // The very block we committed is re-served (duplicate). NOT a conflict.
        assert!(
            !conflicts_with_committed(Some(synced), synced),
            "re-serving our own committed block is not a conflict"
        );

        // A DIFFERENT block at an already-committed height → conflict (halt).
        let other = CryptoHash::new([8u8; 32]);
        assert!(
            conflicts_with_committed(Some(other), synced),
            "a different block at a height we already finalized must be flagged as a conflict"
        );
    }
}

#[cfg(test)]
mod backfill_detection_tests {
    //! s428 BACKFILL FIX (RED first — `scan_first_gap`, `select_start_height`,
    //! and `no_server_retry_due` did not exist). These pure functions are the
    //! testable core of the backfill trigger: detect the lowest hole below the
    //! committed frontier, construct the fetch start height for it, and re-attempt
    //! (rather than go silent) when no sync server is available.
    use super::{no_server_retry_due, scan_first_gap, select_start_height, GapScan};
    use std::time::{Duration, Instant};

    /// TRIGGER-ON-HOLE DETECTION: a contiguous committed prefix has no gap; a
    /// hole below the frontier is found at its lowest height; and the scan
    /// reports the highest contiguous height as the resume cursor.
    #[test]
    fn scan_finds_lowest_hole_below_frontier() {
        // Committed 0..=239 with a hole 180..=238 (present again at 239): the
        // exact iter-3 shape (rejoined at the tip, missing the middle).
        let present = |h: u64| !(180..=238).contains(&h);

        // Full contiguous prefix 0..=179 → no gap, cursor at 179.
        assert_eq!(
            scan_first_gap(0, 179, present),
            GapScan { first_gap: None, contiguous_through: Some(179) }
        );

        // Scanning across the hole returns the LOWEST missing height (180) and
        // the contiguous cursor just below it (179).
        assert_eq!(
            scan_first_gap(0, 239, present),
            GapScan { first_gap: Some(180), contiguous_through: Some(179) }
        );

        // Genuinely contiguous chain → no gap at all.
        assert_eq!(
            scan_first_gap(0, 179, |_h| true),
            GapScan { first_gap: None, contiguous_through: Some(179) }
        );
    }

    /// The prune floor / resume cursor means heights below the retained window
    /// are NEVER mistaken for holes, and repeated scans are cheap.
    #[test]
    fn scan_respects_floor_and_cursor() {
        // Heights below 100 are pruned away (absent) but must NOT be flagged: we
        // start scanning just above the floor.
        let present = |h: u64| h >= 100;
        // Resume from cursor 149 (scan_from = 150): all present → no gap.
        assert_eq!(
            scan_first_gap(150, 200, present),
            GapScan { first_gap: None, contiguous_through: Some(200) }
        );
        // An empty scan window (scan_from > highest) yields no gap, no cursor.
        assert_eq!(
            scan_first_gap(201, 200, present),
            GapScan { first_gap: None, contiguous_through: None }
        );
    }

    /// BACKFILL-RANGE REQUEST CONSTRUCTION: a hole below the frontier makes the
    /// next fetch start AT the hole (is_backfill = true); a contiguous prefix
    /// tip-syncs from `frontier + 1`; nothing committed starts at genesis.
    #[test]
    fn start_height_prefers_the_hole_then_the_tip() {
        // Hole at 180 while committed up to 239 → backfill from 180.
        assert_eq!(select_start_height(Some(239), Some(180)), (180, true));
        // Frontier stuck at 179, no interior hole → tip-sync from 180 (identical
        // to the pre-fix behaviour).
        assert_eq!(select_start_height(Some(179), None), (180, false));
        // Nothing committed yet → genesis, not a backfill.
        assert_eq!(select_start_height(None, None), (0, false));
        // The hole's parent (start-1 = 179) is by construction within the
        // contiguous committed prefix, so the first backfilled block's parent is
        // present. (Asserted here as the invariant the design relies on.)
        let (start, is_backfill) = select_start_height(Some(239), Some(180));
        assert!(is_backfill);
        assert_eq!(start - 1, 179, "parent of the backfill start must be committed/present");
    }

    /// RETRY-NOT-SILENCE ON NO-SERVERS: a scheduled no-server retry is not due
    /// before its instant and is due after it — the client re-attempts instead
    /// of resetting the long timeout and going silent (the iter-3 failure).
    #[test]
    fn no_server_retry_fires_after_the_short_interval() {
        let now = Instant::now();
        // Never scheduled → nothing to retry.
        assert!(!no_server_retry_due(None, now));
        // Scheduled 2s out → not yet due.
        let retry_at = now + Duration::from_secs(2);
        assert!(!no_server_retry_due(Some(retry_at), now));
        // Once the instant passes → due (re-attempt, do not go silent).
        assert!(no_server_retry_due(Some(retry_at), retry_at));
        assert!(no_server_retry_due(Some(retry_at), retry_at + Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod blacklist_decision_tests {
    use super::warrants_blacklist;
    use crate::app::ValidateBlockResponse;

    #[test]
    fn missing_body_does_not_blacklist() {
        // A block we cannot fully validate only because its out-of-band data
        // (native-action bodies) is missing must NOT blacklist the serving peer --
        // fetch the data instead (livelock root cause, mem 28e1a821).
        assert!(!warrants_blacklist(&ValidateBlockResponse::MissingData));
        // A valid block is not blacklisted.
        assert!(!warrants_blacklist(&ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates: None,
        }));
        // Only a genuinely invalid (crypto/structurally bad) block is blacklisted.
        assert!(warrants_blacklist(&ValidateBlockResponse::Invalid));
    }
}

#[derive(Debug)]
pub enum BlockSyncClientError {
    BlockTreeError(#[allow(dead_code)] BlockTreeError),
    /// P0 SAFETY: a synced block conflicts with our own COMMITTED chain (a
    /// different block hash at an already-finalized height). This is the
    /// strongest fail-stop signal available inside the (app-agnostic)
    /// `hotstuff_rs` crate — the block is refused (never inserted/adopted), the
    /// sync session is torn down, and this hard error propagates up so the host
    /// can react. hotstuff_rs has no channel to the app-layer `exec_failed`
    /// latch; the app's own conflicting-commit guard (torus-consensus `app.rs`)
    /// is the durable halt if a conflict ever reached execution.
    #[allow(dead_code)]
    ConflictingCommittedChain {
        height: BlockHeight,
        local: Option<CryptoHash>,
        synced: CryptoHash,
    },
}

impl From<BlockTreeError> for BlockSyncClientError {
    fn from(value: BlockTreeError) -> Self {
        BlockSyncClientError::BlockTreeError(value)
    }
}

fn is_sync_server_address<K: KVStore>(
    verifying_key: &VerifyingKey,
    block_tree: &BlockTreeSingleton<K>,
) -> Result<bool, BlockSyncClientError> {
    let committed_validator_set = block_tree.committed_validator_set()?;
    if committed_validator_set.contains(verifying_key) {
        return Ok(true);
    }

    match block_tree.highest_committed_block()? {
        Some(block) => {
            let mut speculative_vs_updates = block_tree
                .blocks_in_branch(block)
                .filter(|block| {
                    block_tree
                        .validator_set_updates_status(block)
                        .is_ok_and(|vsu_status| vsu_status.is_pending())
                })
                .map(|block| {
                    if let Ok(ValidatorSetUpdatesStatus::Pending(vs_updates)) =
                        block_tree.validator_set_updates_status(&block)
                    {
                        vs_updates
                    } else {
                        ValidatorSetUpdates::new()
                    }
                });

            Ok(speculative_vs_updates
                .any(|vs_updates| vs_updates.get_insert(verifying_key).is_some()))
        }
        None => Ok(false),
    }
}

#[cfg(test)]
mod fatal_violation_wiring_tests {
    //! FIX C2: block sync, on detecting that a peer's COMMITTED chain conflicts
    //! with a block we already committed at the same height, must surface the
    //! fatal event to the app via `App::on_fatal_safety_violation` (the seam the
    //! host uses to latch its fail-stop and halt the process). RED first:
    //! `App::on_fatal_safety_violation` did not exist, and the conflict branch
    //! never called it.

    use std::collections::{HashMap, VecDeque};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::{
        BlockSyncClient, BlockSyncClientConfiguration, BlockSyncClientError, PendingSyncSession,
    };
    use crate::app::{
        App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest,
        ValidateBlockResponse,
    };
    use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
    use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
    use crate::hotstuff::types::PhaseCertificate;
    use crate::networking::messages::Message;
    use crate::networking::network::{Network, ValidatorSetUpdateHandle};
    use crate::types::block::Block;
    use crate::types::crypto_primitives::{SigningKey, VerifyingKey};
    use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power};
    use crate::types::update_sets::{AppStateUpdates, ValidatorSetUpdates};
    use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

    const CHAIN_ID: ChainID = ChainID::new(0);

    // ---- Minimal in-memory KVStore ----
    #[derive(Clone, Default)]
    struct MemKV {
        map: HashMap<Vec<u8>, Vec<u8>>,
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

    #[derive(Clone)]
    struct NullNetwork;
    impl Network for NullNetwork {
        fn init_validator_set(&mut self, _vs: ValidatorSet) {}
        fn update_validator_set(&mut self, _u: ValidatorSetUpdates) {}
        fn broadcast(&mut self, _m: Message) {}
        fn send(&mut self, _p: VerifyingKey, _m: Message) {}
        fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
            None
        }
    }

    /// Records whether the fatal-safety-violation callback fired.
    struct FatalRecordingApp {
        fatal_called: bool,
    }
    impl App<MemKV> for FatalRecordingApp {
        fn produce_block(&mut self, _r: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
            unreachable!("produce_block not reached on the conflict path")
        }
        fn validate_block(&mut self, _r: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
            unreachable!("validate_block not reached on the conflict path")
        }
        fn validate_block_for_sync(
            &mut self,
            _r: ValidateBlockRequest<MemKV>,
        ) -> ValidateBlockResponse {
            // The committed-conflict check fires BEFORE block validation, so this
            // must never be reached on the conflict path.
            unreachable!("validate_block_for_sync not reached on the conflict path")
        }
        fn on_fatal_safety_violation(&mut self) {
            self.fatal_called = true;
        }
    }

    fn validator_set() -> ValidatorSet {
        let mut vs = ValidatorSet::new();
        vs.put(&SigningKey::from_bytes(&[1u8; 32]).verifying_key(), Power::new(1));
        vs
    }

    fn client() -> BlockSyncClient<NullNetwork> {
        let (wc_tx, _wc_rx) = mpsc::channel();
        let (_wr_tx, wr_rx) = mpsc::channel();
        let config = BlockSyncClientConfiguration {
            chain_id: CHAIN_ID,
            request_limit: 32,
            response_timeout: Duration::from_secs(1),
            blacklist_expiry_time: Duration::from_secs(1),
            block_sync_trigger_min_view_difference: 2,
            block_sync_trigger_timeout: Duration::from_secs(60),
        };
        BlockSyncClient::new(
            config,
            wc_tx,
            wr_rx,
            ValidatorSetUpdateHandle::new(NullNetwork),
            None,
        )
    }

    /// A synced block whose hash differs from the block we already COMMITTED at
    /// the same height must (a) return `ConflictingCommittedChain` and (b) invoke
    /// `App::on_fatal_safety_violation` so the host can halt the process.
    #[test]
    fn committed_conflict_invokes_fatal_callback() {
        let set = validator_set();
        let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
        let mut block_tree = BlockTreeSingleton::new(MemKV::default());
        block_tree
            .initialize(&AppStateUpdates::new(), &vss)
            .expect("block tree init");

        // Seed a COMMITTED block hash at height 5 that differs from the synced one.
        let committed_hash = CryptoHash::new([99u8; 32]);
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        wb.set_block_at_height(BlockHeight::new(5), &committed_hash)
            .expect("seed committed block-at-height");
        block_tree.write(wb);

        // A crypto-correct (genesis-justified) synced block at the SAME height 5,
        // but with a DIFFERENT hash — a conflicting committed chain.
        let synced = Block::new(
            BlockHeight::new(5),
            PhaseCertificate::genesis_pc(),
            CryptoHash::new([7u8; 32]),
            Data::new(vec![]),
        );
        assert_ne!(synced.hash, committed_hash, "precondition: hashes must differ");

        let mut cli = client();
        let peer = SigningKey::from_bytes(&[1u8; 32]).verifying_key();
        cli.pending_sync = Some(PendingSyncSession {
            peer,
            pending_blocks: VecDeque::from(vec![synced]),
            highest_pc: None,
            blocks_synced: 0,
            init_height: BlockHeight::new(5),
            fetch_iterations: 1,
            session_start: Instant::now(),
            awaiting_fetch: false,
            last_fetch_start_height: None,
            is_backfill: false,
        });

        let mut app = FatalRecordingApp { fatal_called: false };
        let result = cli.process_pending_block(&mut block_tree, &mut app);

        assert!(
            matches!(result, Err(BlockSyncClientError::ConflictingCommittedChain { .. })),
            "a committed-chain conflict must return the hard ConflictingCommittedChain error"
        );
        assert!(
            app.fatal_called,
            "the committed-conflict branch must invoke App::on_fatal_safety_violation \
             so the host can latch fail-stop and halt the process"
        );
    }

    /// s428 BACKFILL FIX (RED first — `request_sync` returned `()` and reset the
    /// no-progress clock even on a no-server no-op, so a crash-restarted node
    /// with an empty `available_sync_servers` map went silent). After restart the
    /// map is empty (filled only by peers' periodic advertisements), so a trigger
    /// must NOT go silent: it returns `NoServer`, starts no session, and arms the
    /// SHORT retry gate so the next tick re-attempts.
    #[test]
    fn no_server_trigger_does_not_go_silent() {
        let set = validator_set();
        let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
        let mut block_tree = BlockTreeSingleton::new(MemKV::default());
        block_tree
            .initialize(&AppStateUpdates::new(), &vss)
            .expect("block tree init");

        let mut cli = client();
        let outcome = cli
            .request_sync(&mut block_tree)
            .expect("request_sync must not error on an empty server set");
        assert_eq!(
            outcome,
            super::RequestOutcome::NoServer,
            "a trigger with no available server must report NoServer, not silently no-op"
        );
        assert!(
            cli.pending_sync.is_none(),
            "no sync session may start when there is no server"
        );
        assert!(
            cli.block_sync_client_state.no_server_retry_at.is_some(),
            "a no-server trigger MUST schedule a short retry (never go silent) — the iter-3 bug"
        );
    }

    /// s428 BACKFILL FIX: `compute_sync_start` detects the lowest hole below the
    /// committed frontier and targets it (is_backfill = true), instead of always
    /// anchoring at `highest_committed + 1` (which could never revisit a height
    /// below the frontier). Seeds a committed index 0..=9 with a hole at 6.
    #[test]
    fn compute_sync_start_targets_interior_hole() {
        let set = validator_set();
        let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
        let mut block_tree = BlockTreeSingleton::new(MemKV::default());
        block_tree
            .initialize(&AppStateUpdates::new(), &vss)
            .expect("block tree init");

        // Seed block metadata + block_at_height for 0..=9 EXCEPT the hole at 6,
        // and mark the height-9 block as highest committed so the frontier sits
        // above the hole.
        let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
        let mut top_hash = CryptoHash::new([0u8; 32]);
        for h in 0u64..=9 {
            if h == 6 {
                continue;
            }
            let block = Block::new(
                BlockHeight::new(h),
                PhaseCertificate::genesis_pc(),
                CryptoHash::new([h as u8; 32]),
                Data::new(vec![]),
            );
            wb.set_block(&block).expect("seed block metadata");
            wb.set_block_at_height(BlockHeight::new(h), &block.hash)
                .expect("seed block-at-height");
            if h == 9 {
                top_hash = block.hash;
            }
        }
        wb.set_highest_committed_block(&top_hash)
            .expect("seed highest committed");
        block_tree.write(wb);

        let mut cli = client();
        let (start, is_backfill) = cli
            .compute_sync_start(&block_tree)
            .expect("compute_sync_start");
        assert_eq!(start.int(), 6, "the fetch must start AT the interior hole");
        assert!(
            is_backfill,
            "a hole below the committed frontier must be flagged as backfill"
        );
    }
}
