/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Thread that drives the event-driven implementations of the [HotStuff](crate::hotstuff),
//! [Pacemaker](crate::pacemaker), and [BlockSync](crate::block_sync) subprotocols.

use std::{
    sync::mpsc::{Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
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
            if self.hotstuff.is_view_outdated(view_info) {
                if let Err(e) = self
                    .hotstuff
                    .enter_view(view_info.clone(), &mut self.block_tree, &mut self.app)
                {
                    log::error!("HotStuff enter_view error (view={}): {:?} — skipping view", view_info.view.int(), e);
                    continue;
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
                        log::error!("BlockSync process_pending_block error: {:?} — continuing", e);
                        break;
                    }
                }
            }

            // 7. Poll the network for incoming messages.
            match self
                .pm_stub
                .recv(self.chain_id, view_info.view, view_info.deadline)
            {
                Ok((origin, msg)) => match msg {
                    ProgressMessage::HotStuffMessage(msg) => {
                        if let Err(e) = self
                            .hotstuff
                            .on_receive_msg(msg, &origin, &mut self.block_tree, &mut self.app)
                        {
                            log::error!("HotStuff on_receive_msg error: {:?} — dropping message", e);
                        }
                    }
                    ProgressMessage::PacemakerMessage(msg) => {
                        if let Err(e) = self
                            .pacemaker
                            .on_receive_msg(msg, &origin, &mut self.block_tree)
                        {
                            log::error!("Pacemaker on_receive_msg error: {:?} — dropping message", e);
                        }
                    }
                    ProgressMessage::BlockSyncAdvertiseMessage(msg) => {
                        if let Err(e) = self
                            .block_sync_client
                            .on_receive_msg(msg, &origin, &mut self.block_tree)
                        {
                            log::error!("BlockSync on_receive_msg error: {:?} — dropping message", e);
                        }
                    }
                },
                Err(ProgressMessageReceiveError::Disconnected) => {
                    panic!("The poller has disconnected!")
                }
                Err(ProgressMessageReceiveError::Timeout) => {}
            }

            // 8. Let the block sync client update its internal state, and trigger sync if needed.
            if let Err(e) = self
                .block_sync_client
                .tick(&mut self.block_tree)
            {
                log::error!("BlockSync tick error: {:?} — continuing", e);
            }
        }
    }
}
