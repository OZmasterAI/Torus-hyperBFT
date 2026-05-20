//! Functions and types for sending messages to the P2P network.

use ed25519_dalek::VerifyingKey;

use crate::hotstuff::messages::{BlockDataRequest, BlockDataResponse};
use crate::types::{block::Block, data_types::CryptoHash};
use super::{messages::Message, network::Network};

/// Handle for sending and broadcasting messages to the [`Network`].
///
/// It can be used to send or broadcast instances of any type that implement the [`Into<Message>`]
/// trait.
#[derive(Clone)]
pub(crate) struct SenderHandle<N: Network> {
    network: N,
}

impl<N: Network> SenderHandle<N> {
    pub(crate) fn new(network: N) -> Self {
        Self { network }
    }

    pub(crate) fn send<S: Into<Message>>(&mut self, peer: VerifyingKey, msg: S) {
        self.network.send(peer, msg.into())
    }

    pub(crate) fn broadcast<S: Into<Message>>(&mut self, msg: S) {
        self.network.broadcast(msg.into())
    }

    pub(crate) fn request_block_data(&mut self, peer: VerifyingKey, request: BlockDataRequest) {
        self.network.request_block_data(peer, request)
    }

    pub(crate) fn recv_block_data(&mut self) -> Option<(VerifyingKey, BlockDataResponse)> {
        self.network.recv_block_data()
    }

    pub(crate) fn store_block_for_serving(&mut self, hash: CryptoHash, block: Block) {
        self.network.store_block_for_serving(hash, block)
    }
}
