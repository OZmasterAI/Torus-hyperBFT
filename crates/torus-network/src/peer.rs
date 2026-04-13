use std::collections::HashMap;

use ed25519_dalek::VerifyingKey;
use libp2p::PeerId;

/// Bidirectional mapping between hotstuff_rs VerifyingKey and libp2p PeerId.
#[derive(Default, Debug)]
pub struct PeerMap {
    vk_to_peer: HashMap<[u8; 32], PeerId>,
    peer_to_vk: HashMap<PeerId, VerifyingKey>,
}

impl PeerMap {
    pub fn insert(&mut self, vk: VerifyingKey, peer_id: PeerId) {
        self.vk_to_peer.insert(vk.to_bytes(), peer_id);
        self.peer_to_vk.insert(peer_id, vk);
    }

    pub fn remove_by_vk(&mut self, vk: &VerifyingKey) -> Option<PeerId> {
        if let Some(peer_id) = self.vk_to_peer.remove(&vk.to_bytes()) {
            self.peer_to_vk.remove(&peer_id);
            Some(peer_id)
        } else {
            None
        }
    }

    pub fn get_peer_id(&self, vk: &VerifyingKey) -> Option<&PeerId> {
        self.vk_to_peer.get(&vk.to_bytes())
    }

    pub fn get_vk(&self, peer_id: &PeerId) -> Option<&VerifyingKey> {
        self.peer_to_vk.get(peer_id)
    }

    pub fn contains_vk(&self, vk: &VerifyingKey) -> bool {
        self.vk_to_peer.contains_key(&vk.to_bytes())
    }
}
