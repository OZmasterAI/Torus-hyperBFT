/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Background worker thread that performs block sync network I/O without blocking
//! the consensus algorithm loop.
//!
//! The worker receives fetch commands from the [`BlockSyncClient`](super::client::BlockSyncClient),
//! sends [`BlockSyncRequest`]s to peers, collects [`BlockSyncResponse`]s, and forwards the
//! fetched blocks back to the algorithm thread for validation and insertion.

use std::{
    sync::mpsc::{Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ed25519_dalek::VerifyingKey;

use crate::{
    block_sync::messages::BlockSyncRequest,
    hotstuff::types::PhaseCertificate,
    networking::{network::Network, receiving::BlockSyncClientStub, sending::SenderHandle},
    types::{
        block::Block,
        data_types::{BlockHeight, ChainID},
    },
};

/// Command sent from the algorithm thread to the sync worker.
pub(crate) enum SyncCommand {
    /// Fetch blocks from a peer starting at the given height.
    Fetch {
        peer: VerifyingKey,
        chain_id: ChainID,
        start_height: BlockHeight,
        limit: u32,
    },
}

/// Result sent from the sync worker back to the algorithm thread.
pub(crate) enum SyncResult {
    /// Successfully fetched blocks from a peer.
    Blocks {
        peer: VerifyingKey,
        blocks: Vec<Block>,
        highest_pc: PhaseCertificate,
    },
    /// The peer returned an empty response (no more blocks).
    Empty { peer: VerifyingKey },
    /// An error occurred (timeout, disconnect).
    Error { peer: VerifyingKey },
}

pub(crate) struct BlockSyncWorker<N: Network + 'static> {
    receiver: BlockSyncClientStub,
    sender: SenderHandle<N>,
    response_timeout: Duration,
    commands: Receiver<SyncCommand>,
    results: Sender<SyncResult>,
    shutdown_signal: Receiver<()>,
}

impl<N: Network + 'static> BlockSyncWorker<N> {
    pub(crate) fn new(
        receiver: BlockSyncClientStub,
        network: N,
        response_timeout: Duration,
        commands: Receiver<SyncCommand>,
        results: Sender<SyncResult>,
        shutdown_signal: Receiver<()>,
    ) -> Self {
        Self {
            receiver,
            sender: SenderHandle::new(network),
            response_timeout,
            commands,
            results,
            shutdown_signal,
        }
    }

    pub(crate) fn start(mut self) -> JoinHandle<()> {
        thread::spawn(move || loop {
            match self.shutdown_signal.try_recv() {
                Ok(()) => return,
                Err(TryRecvError::Empty) => (),
                Err(TryRecvError::Disconnected) => return,
            }

            match self.commands.recv_timeout(Duration::from_millis(100)) {
                Ok(SyncCommand::Fetch {
                    peer,
                    chain_id,
                    start_height,
                    limit,
                }) => {
                    let request = BlockSyncRequest {
                        chain_id,
                        start_height,
                        limit,
                    };
                    self.sender.send(peer, request);

                    let deadline = Instant::now() + self.response_timeout;
                    match self.receiver.recv_response(peer, deadline) {
                        Ok(response) => {
                            if response.blocks.is_empty() {
                                let _ = self.results.send(SyncResult::Empty { peer });
                            } else {
                                let _ = self.results.send(SyncResult::Blocks {
                                    peer,
                                    blocks: response.blocks,
                                    highest_pc: response.highest_pc,
                                });
                            }
                        }
                        Err(_) => {
                            let _ = self.results.send(SyncResult::Error { peer });
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    thread::yield_now();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        })
    }
}
