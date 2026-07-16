/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Thread that drives the event-driven implementations of the [HotStuff](crate::hotstuff),
//! [Pacemaker](crate::pacemaker), and [BlockSync](crate::block_sync) subprotocols.

use std::{
    sync::mpsc::{Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ed25519_dalek::VerifyingKey;

use crate::{
    app::App,
    block_sync::client::{BlockSyncClient, BlockSyncClientConfiguration},
    block_tree::{accessors::internal::BlockTreeSingleton, pluggables::KVStore},
    events::*,
    hotstuff::implementation::{HotStuff, HotStuffConfiguration},
    networking::{
        messages::ProgressMessage,
        network::{Network, ValidatorSetUpdateHandle},
        receiving::{ProgressMessageReceiveError, ProgressMessageStub},
        sending::SenderHandle,
    },
    pacemaker::implementation::{Pacemaker, PacemakerConfiguration},
    types::data_types::{BufferSize, ChainID, ViewNumber},
};

/// Instance of the algorithm thread.
///
/// This struct's `Drop` destructor gracefully shuts down the algorithm thread.
pub(crate) struct Algorithm<N: Network + 'static, K: KVStore, A: App<K> + 'static> {
    chain_id: ChainID,
    pm_stub: ProgressMessageStub,
    block_tree: BlockTreeSingleton<K>,
    app: A,
    hotstuff: HotStuff<N>,
    pacemaker: Pacemaker<N>,
    block_sync_client: BlockSyncClient<N>,
    shutdown_signal: Receiver<()>,
}

impl<N: Network + 'static, K: KVStore, A: App<K> + 'static> Algorithm<N, K, A> {
    const MAX_SYNC_BLOCKS_PER_TICK: usize = 128;

    /// Create an instance of the algorithm thread.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        chain_id: ChainID,
        hotstuff_config: HotStuffConfiguration,
        pacemaker_config: PacemakerConfiguration,
        block_sync_client_config: BlockSyncClientConfiguration,
        block_tree: BlockTreeSingleton<K>,
        app: A,
        network: N,
        progress_msg_receiver: Receiver<(VerifyingKey, ProgressMessage)>,
        progress_msg_buffer_capacity: BufferSize,
        worker_commands: Sender<super::block_sync::worker::SyncCommand>,
        worker_results: Receiver<super::block_sync::worker::SyncResult>,
        shutdown_signal: Receiver<()>,
        event_publisher: Option<Sender<Event>>,
    ) -> Self {
        let pm_stub = ProgressMessageStub::new(progress_msg_receiver, progress_msg_buffer_capacity);
        let msg_sender: SenderHandle<N> = SenderHandle::new(network.clone());
        let validator_set_update_handle = ValidatorSetUpdateHandle::new(network);

        let init_view = match block_tree
            .highest_view_with_progress()
            .expect("Cannot retrieve the highest view with progress!")
            .int()
        {
            0 => ViewNumber::new(0),
            v => ViewNumber::new(v + 1),
        };

        let pacemaker = Pacemaker::new(
            pacemaker_config,
            msg_sender.clone(),
            init_view,
            &block_tree
                .validator_set_state()
                .expect("Cannot retrieve the validator set state!"),
            event_publisher.clone(),
        )
        .expect("Failed to create a new Pacemaker!");

        let init_view_info = pacemaker.query();

        let hotstuff = HotStuff::new(
            hotstuff_config,
            init_view_info.clone(),
            msg_sender.clone(),
            validator_set_update_handle.clone(),
            block_tree
                .validator_set_state()
                .expect("Cannot retrieve the validator set state!")
                .clone(),
            event_publisher.clone(),
        );

        let block_sync_client = BlockSyncClient::new(
            block_sync_client_config,
            worker_commands,
            worker_results,
            validator_set_update_handle,
            event_publisher.clone(),
        );

        Self {
            chain_id,
            pm_stub,
            block_tree,
            app,
            hotstuff,
            pacemaker,
            block_sync_client,
            shutdown_signal,
        }
    }

    /// Start an instance of the algorithm thread.
    pub(crate) fn start(self) -> JoinHandle<()> {
        thread::spawn(move || self.execute())
    }

    fn execute(mut self) {
        // S444 BOOT RECONCILE: deliver any committed-but-never-delivered blocks
        // to the app before entering the loop. `update()` durably advances
        // `highest_committed` BEFORE its caller fires `on_committed_block`, so a
        // crash in that window (t15 tight kills; widest during block-sync
        // catch-up jumps) leaves heights the commit walk will never revisit —
        // without this replay the app starves on them forever (the min-height
        // exec-feed gap: v3 committed 962 / fed 678).
        match crate::committed_feed::feed_committed_blocks_to_app(
            &mut self.block_tree,
            &mut self.app,
        ) {
            Ok(0) => {}
            Ok(n) => log::warn!(
                "app feed: boot reconciliation delivered {} committed height(s) the app had \
                 never received (crash window between commit write and callbacks)",
                n
            ),
            Err(e) => log::error!("app feed: boot reconciliation failed: {:?}", e),
        }
        // S444 LIVE RECONCILE watchdog cadence (see step 9 in the loop).
        let mut last_feed_reconcile = Instant::now();

        loop {
            // 1. Check whether the library user has issued a shutdown command. If so, break.
            match self.shutdown_signal.try_recv() {
                Ok(()) => return,
                Err(TryRecvError::Empty) => (),
                Err(TryRecvError::Disconnected) => {
                    panic!("Algorithm thread disconnected from main thread")
                }
            }

            // 2. Let the pacemaker update its internal state if needed.
            if let Err(e) = self.pacemaker.tick(&self.block_tree) {
                log::error!("Pacemaker tick error: {:?} — continuing", e);
            }

            // 3. Query the pacemaker for potential updates to the current view.
            let view_info = self.pacemaker.query();

            // 4. In case the view has been updated, update HotStuff's internal view and perform
            // the necessary protocol steps.
            if self.hotstuff.is_view_outdated(view_info) || self.hotstuff.has_deferred_proposal() {
                if let Err(e) =
                    self.hotstuff
                        .enter_view(view_info.clone(), &mut self.block_tree, &mut self.app)
                {
                    log::error!("HotStuff enter_view error (view={}): {:?} — will retry after polling messages", view_info.view.int(), e);
                }
            }

            // 5. Poll the sync worker for fetched blocks (non-blocking).
            self.block_sync_client.poll_worker_results();

            // 6. Process pending sync blocks — drain the batch for faster catch-up.
            for _ in 0..Self::MAX_SYNC_BLOCKS_PER_TICK {
                match self
                    .block_sync_client
                    .process_pending_block(&mut self.block_tree, &mut self.app)
                {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(e) => {
                        log::error!(
                            "BlockSync process_pending_block error: {:?} — continuing",
                            e
                        );
                        break;
                    }
                }
            }

            // 6b. Poll the dedicated block-data channel for body responses.
            if let Err(e) = self
                .hotstuff
                .poll_block_data_responses(&mut self.block_tree, &mut self.app)
            {
                log::error!("HotStuff poll_block_data_responses error: {:?}", e);
            }

            // 6c. Retry stale body fetches (proposer first, then rotate across the
            // other validators); trigger sync after max retries.
            self.hotstuff.tick_pending_body_retries(&self.block_tree);
            // 6d. S426: retry by-hash fetches for unknown justify blocks (a QC that
            // formed on an undisseminated block); fall back to sync on exhaustion.
            self.hotstuff.tick_justify_fetch_retries(&self.block_tree);
            // 6e. S432: follower body-starvation heal — if our (view-current)
            // highest_pc points at a block whose body we never obtained, by-hash
            // fetch it (or walk back a parked block's missing parent) instead of
            // waiting for the 60s no-progress sync timeout. Self-throttled.
            if let Err(e) = self.hotstuff.tick_missing_pc_block_fetch(&self.block_tree) {
                log::error!("HotStuff tick_missing_pc_block_fetch error: {:?}", e);
            }
            if self.hotstuff.take_sync_needed() {
                if let Err(e) = self.block_sync_client.trigger_sync(&mut self.block_tree) {
                    log::error!(
                        "BlockSync trigger_sync (body retry exhausted) error: {:?}",
                        e
                    );
                }
            }

            // 7. Poll the network for incoming messages.
            // Use a short deadline during active sync so we don't park for 500ms between batches.
            let recv_deadline = if self.block_sync_client.has_pending_sync()
                || self.hotstuff.has_deferred_proposal()
                || self.hotstuff.has_pending_body_fetches()
                || self.hotstuff.has_pending_justify_fetches()
            {
                std::cmp::min(
                    view_info.deadline,
                    Instant::now() + Duration::from_millis(10),
                )
            } else {
                view_info.deadline
            };
            match self
                .pm_stub
                .recv(self.chain_id, view_info.view, recv_deadline)
            {
                Ok((origin, msg)) => match msg {
                    ProgressMessage::HotStuffMessage(msg) => {
                        if let Err(e) = self.hotstuff.on_receive_msg(
                            msg,
                            &origin,
                            &mut self.block_tree,
                            &mut self.app,
                        ) {
                            log::error!(
                                "HotStuff on_receive_msg error: {:?} — dropping message",
                                e
                            );
                        }
                        if self.hotstuff.take_sync_needed() {
                            if let Err(e) =
                                self.block_sync_client.trigger_sync(&mut self.block_tree)
                            {
                                log::error!("BlockSync trigger_sync error: {:?}", e);
                            }
                        }
                    }
                    ProgressMessage::PacemakerMessage(msg) => {
                        if let Err(e) =
                            self.pacemaker
                                .on_receive_msg(msg, &origin, &mut self.block_tree)
                        {
                            log::error!(
                                "Pacemaker on_receive_msg error: {:?} — dropping message",
                                e
                            );
                        }
                    }
                    ProgressMessage::BlockSyncAdvertiseMessage(msg) => {
                        if let Err(e) = self.block_sync_client.on_receive_msg(
                            msg,
                            &origin,
                            &mut self.block_tree,
                        ) {
                            log::error!(
                                "BlockSync on_receive_msg error: {:?} — dropping message",
                                e
                            );
                        }
                    }
                },
                Err(ProgressMessageReceiveError::Disconnected) => {
                    panic!("The poller has disconnected!")
                }
                Err(ProgressMessageReceiveError::Timeout) => {}
            }

            // 8. Let the block sync client update its internal state, and trigger sync if needed.
            if let Err(e) = self.block_sync_client.tick(&mut self.block_tree) {
                log::error!("BlockSync tick error: {:?} — continuing", e);
            }

            // 9. S444 LIVE RECONCILE watchdog (throttled): every commit path is
            // supposed to feed the app synchronously, so finding undelivered
            // committed heights here means some path advanced `highest_committed`
            // without feeding — an unknown cousin of the min-height exec-feed
            // gap. Surface it LOUDLY, then heal it (the feed delivers in order).
            // Cost when healthy: three point reads per second.
            if last_feed_reconcile.elapsed() >= Duration::from_secs(1) {
                last_feed_reconcile = Instant::now();
                match crate::committed_feed::feed_committed_blocks_to_app(
                    &mut self.block_tree,
                    &mut self.app,
                ) {
                    Ok(0) => {}
                    Ok(n) => log::error!(
                        "app feed: LIVE reconcile found and delivered {} committed height(s) \
                         that never reached the app — a commit path advanced highest_committed \
                         without feeding (self-healed; investigate the cousin path)",
                        n
                    ),
                    Err(e) => log::error!("app feed: live reconcile failed: {:?}", e),
                }
                // Give the app its periodic tick AFTER the feed so any height
                // the reconcile just delivered is already enqueued. Hosts use
                // this to retry deferred LOCAL work (e.g. draining a
                // backed-up execution dispatch queue) between commits; the
                // default impl is a no-op.
                self.app.on_reconcile_tick();
            }
        }
    }
}
