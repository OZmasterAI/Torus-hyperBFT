//! Trait for pluggable peer-to-peer (P2P) networking.
//!
//! Main trait: [`Network`].

use ed25519_dalek::VerifyingKey;

use crate::hotstuff::messages::{BlockDataRequest, BlockDataResponse};
use crate::types::{
    block::Block, data_types::CryptoHash, update_sets::ValidatorSetUpdates,
    validator_set::ValidatorSet,
};

use super::messages::Message;

/// Trait for pluggable peer-to-peer (P2P) networking.
pub trait Network: Clone + Send {
    /// Inform the network provider the validator set on wake-up.
    fn init_validator_set(&mut self, validator_set: ValidatorSet);

    /// Inform the networking provider of updates to the validator set.
    fn update_validator_set(&mut self, updates: ValidatorSetUpdates);

    /// Send a message to all peers (including listeners) without blocking.
    fn broadcast(&mut self, message: Message);

    /// Send a message to the specified peer without blocking.
    fn send(&mut self, peer: VerifyingKey, message: Message);

    /// Receive a message from any peer. Returns immediately with a None if no message is available now.
    fn recv(&mut self) -> Option<(VerifyingKey, Message)>;

    /// Request block data from a peer via the dedicated block-data protocol.
    /// Default no-op for backward compatibility.
    fn request_block_data(&mut self, _peer: VerifyingKey, _request: BlockDataRequest) {}

    /// Receive a block-data response. Non-blocking, returns None if nothing available.
    /// Responses arrive via a channel separate from the consensus message queue.
    fn recv_block_data(&mut self) -> Option<(VerifyingKey, BlockDataResponse)> {
        None
    }

    /// Publish a block to the network-thread store so it can serve body requests
    /// without touching the algorithm thread.
    fn store_block_for_serving(&mut self, _hash: CryptoHash, _block: Block) {}
}

/// Handle for informing the [`Network`] implementation about validator set updates.
///
/// It is important for the network provider to know about validator set updates because, for example,
/// if a validator set update adds new validators into the validator set, the network provider may want
/// to establish connections to these new validators.
#[derive(Clone)]
pub(crate) struct ValidatorSetUpdateHandle<N: Network> {
    network: N,
}

impl<N: Network> ValidatorSetUpdateHandle<N> {
    /// Create a new update handle.
    pub(crate) fn new(network: N) -> Self {
        Self { network }
    }

    /// Inform the network provider of new validator set `updates` that have been committed.
    pub(crate) fn update_validator_set(&mut self, updates: ValidatorSetUpdates) {
        self.network.update_validator_set(updates)
    }
}
