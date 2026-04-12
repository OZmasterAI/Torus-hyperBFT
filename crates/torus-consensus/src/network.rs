//! In-process channel-based `Network` implementation for testing.
//!
//! All validators share a [`ChannelNetwork`] mesh connected by `Arc<Mutex<VecDeque>>`
//! message queues. No serialization — messages are cloned directly.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use ed25519_dalek::VerifyingKey;

use hotstuff_rs::networking::messages::Message;
use hotstuff_rs::networking::network::Network;
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;
use hotstuff_rs::types::validator_set::ValidatorSet;

type Inbox = Arc<Mutex<VecDeque<(VerifyingKey, Message)>>>;

/// In-process channel network for testing consensus with multiple validators
/// in a single process.
///
/// Each validator gets its own `ChannelNetwork` instance. All instances share
/// the same inbox map, enabling zero-copy message passing via `Clone`.
#[derive(Clone)]
pub struct ChannelNetwork {
    me: VerifyingKey,
    my_inbox: Inbox,
    all_inboxes: Arc<HashMap<[u8; 32], Inbox>>,
}

impl ChannelNetwork {
    /// Create a mesh of connected networks for the given validator keys.
    ///
    /// Returns one `ChannelNetwork` per key, in the same order.
    pub fn create_test_network(keys: &[VerifyingKey]) -> Vec<Self> {
        let mut inboxes = HashMap::new();
        for key in keys {
            inboxes.insert(key.to_bytes(), Arc::new(Mutex::new(VecDeque::new())));
        }
        let all_inboxes = Arc::new(inboxes);

        keys.iter()
            .map(|key| {
                let my_inbox = all_inboxes.get(&key.to_bytes()).unwrap().clone();
                ChannelNetwork {
                    me: *key,
                    my_inbox,
                    all_inboxes: all_inboxes.clone(),
                }
            })
            .collect()
    }
}

impl Network for ChannelNetwork {
    fn init_validator_set(&mut self, _validator_set: ValidatorSet) {
        // All inboxes are pre-registered at construction time.
    }

    fn update_validator_set(&mut self, _updates: ValidatorSetUpdates) {
        // Static validator set for testing.
    }

    fn broadcast(&mut self, message: Message) {
        // hotstuff_rs expects the proposer to receive its own broadcast
        // (the algorithm thread waits for a Proposal even if it is the proposer).
        for (_key_bytes, inbox) in self.all_inboxes.iter() {
            inbox.lock().unwrap().push_back((self.me, message.clone()));
        }
    }

    fn send(&mut self, peer: VerifyingKey, message: Message) {
        if let Some(inbox) = self.all_inboxes.get(&peer.to_bytes()) {
            inbox.lock().unwrap().push_back((self.me, message));
        }
    }

    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        self.my_inbox.lock().unwrap().pop_front()
    }
}
