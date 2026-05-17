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
    block_sync::messages::{
        AdvertiseBlock, AdvertisePC, BlockSyncAdvertiseMessage,
    },
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton},
        invariants::safe_pc,
        pluggables::KVStore,
    },
    events::{EndSyncEvent, Event, InsertBlockEvent, StartSyncEvent},
    hotstuff::types::PhaseCertificate,
    networking::network::{Network, ValidatorSetUpdateHandle},
    types::{
        block::Block,
        data_types::{BlockHeight, ChainID, ViewNumber},
        signed_messages::{Certificate, SignedMessage},
        update_sets::ValidatorSetUpdates,
        validator_set::ValidatorSetUpdatesStatus,
    },
};

use super::worker::{SyncCommand, SyncResult};

const MAX_SYNC_ITERATIONS: u32 = 1000;
const SYNC_SESSION_TIMEOUT: Duration = Duration::from_secs(30);

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
        self.request_sync(block_tree)?;
        self.block_sync_client_state.last_progress_or_sync_time = Instant::now();
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
        }

        if self.pending_sync.is_none()
            && Instant::now() - self.block_sync_client_state.last_progress_or_sync_time
                >= self.config.block_sync_trigger_timeout
        {
            log::info!(
                "block_sync: timeout trigger fired, available_servers={}, committed_height={:?}",
                self.block_sync_client_state.available_sync_servers.len(),
                committed_height,
            );
            self.request_sync(block_tree)?;
            self.block_sync_client_state.last_progress_or_sync_time = Instant::now();
        };

        Ok(())
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
                log::info!(
                    "block_sync: worker returned {} blocks",
                    blocks.len()
                );
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
        let has_work = self.pending_sync.as_ref().map_or(false, |s| {
            !s.awaiting_fetch && !s.pending_blocks.is_empty()
        });
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
            .map_or(false, |b| block_tree.contains(&b.hash))
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
            self.block_sync_client_state.blacklist_sync_server(
                peer,
                self.config.blacklist_expiry_time,
            );
            self.end_session();
            return Ok(true);
        }

        let parent_block = if block.justify.is_genesis_pc() {
            None
        } else {
            Some(&block.justify.block)
        };

        let validate_block_request =
            ValidateBlockRequest::new(&block, block_tree.app_view(parent_block)?);

        if let ValidateBlockResponse::Valid {
            app_state_updates,
            validator_set_updates,
        } = app.validate_block_for_sync(validate_block_request)
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

            let committed_validator_set_updates =
                block_tree.update(&block.justify, &self.event_publisher)?;

            if let Some(vs_updates) = committed_validator_set_updates {
                self.validator_set_update_handle
                    .update_validator_set(vs_updates)
            }

            let session = self.pending_sync.as_mut().unwrap();
            session.blocks_synced += 1;

            // Apply highest_pc from the response if valid
            if let Some(ref highest_pc) = session.highest_pc.clone() {
                if highest_pc.is_correct(block_tree)?
                    && safe_pc(highest_pc, block_tree, chain_id)?
                {
                    block_tree.update(highest_pc, &self.event_publisher)?;
                }
            }

            log::info!(
                "block_sync: block inserted, blocks_synced={}",
                self.pending_sync.as_ref().unwrap().blocks_synced
            );
        } else {
            log::warn!("block_sync: block failed app validation, blacklisting peer");
            self.block_sync_client_state.blacklist_sync_server(
                peer,
                self.config.blacklist_expiry_time,
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
            self.request_sync(block_tree)?;
            self.block_sync_client_state.last_progress_or_sync_time = Instant::now();
        };
        Ok(())
    }

    /// Select a peer and start a sync session by sending the first fetch command to the worker.
    fn request_sync<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        if self.pending_sync.is_some() {
            return Ok(());
        }

        let highest_committed_block_height = block_tree.highest_committed_block_height()?;
        let peer = match self
            .block_sync_client_state
            .random_sync_server(&highest_committed_block_height)
        {
            Some(p) => p,
            None => {
                log::warn!("block_sync: no available sync servers, cannot sync");
                return Ok(());
            }
        };

        let init_height = highest_committed_block_height.unwrap_or(BlockHeight::new(0));
        let start_height = highest_committed_block_height
            .map(|h| h + 1)
            .unwrap_or(BlockHeight::new(0));

        log::info!(
            "block_sync: starting sync with peer, our committed_height={:?}",
            highest_committed_block_height
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
        });

        Ok(())
    }

    /// Send the next fetch command to the worker for the current session.
    fn request_next_batch<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), BlockSyncClientError> {
        let session = match &mut self.pending_sync {
            Some(s) if !s.awaiting_fetch => s,
            _ => return Ok(()),
        };

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

        let start_height = block_tree
            .highest_committed_block_height()?
            .map(|h| h + 1)
            .unwrap_or(BlockHeight::new(0));

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
            if let Some(&advertised_height) = self
                .block_sync_client_state
                .available_sync_servers
                .get(&peer)
            {
                let min_blocks_expected = advertised_height - session.init_height;
                if session.blocks_synced < min_blocks_expected {
                    self.block_sync_client_state.blacklist_sync_server(
                        peer,
                        self.config.blacklist_expiry_time,
                    );
                }
            }
        }
        self.end_session();
    }

    fn end_session(&mut self) {
        if let Some(session) = self.pending_sync.take() {
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
}

impl BlockSyncClientState {
    fn initialize() -> Self {
        Self {
            available_sync_servers: HashMap::new(),
            blacklist: VecDeque::new(),
            last_progress_or_sync_time: Instant::now(),
            highest_pc_view: ViewNumber::new(0),
            last_committed_height: None,
        }
    }

    fn blacklist_contains_server_address(&self, sync_server: &VerifyingKey) -> bool {
        self.blacklist
            .iter()
            .find(|(vk, _)| vk == sync_server)
            .is_some()
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

#[derive(Debug)]
pub enum BlockSyncClientError {
    BlockTreeError(BlockTreeError),
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
                .find(|vs_updates| vs_updates.get_insert(verifying_key).is_some())
                .is_some())
        }
        None => Ok(false),
    }
}
