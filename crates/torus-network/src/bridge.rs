use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::networking::messages::Message;
use hotstuff_rs::networking::network::Network;
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;
use hotstuff_rs::types::validator_set::ValidatorSet;
use libp2p::{Multiaddr, PeerId, SwarmBuilder};
use tokio::sync::mpsc;
use tracing::warn;

use crate::behaviour::TorusBehaviour;
use crate::config::NetworkConfig;
use crate::peer::PeerMap;
use crate::swarm::{run_swarm, NetworkCommand, SharedState};
use crate::tx_gossip::TxGossipHandle;

/// libp2p-based Network implementation for hotstuff_rs.
///
/// Bridges async libp2p with the synchronous Network trait using channels
/// and shared state protected by `Arc<Mutex/RwLock>`.
#[derive(Clone)]
pub struct LibP2PNetwork {
    command_tx: mpsc::UnboundedSender<NetworkCommand>,
    shared: Arc<SharedState>,
}

impl LibP2PNetwork {
    /// Create a new LibP2PNetwork. Must be called from within a tokio runtime.
    /// Spawns a background task to drive the libp2p swarm.
    pub async fn new(
        config: NetworkConfig,
        local_key: VerifyingKey,
    ) -> Result<(Self, TxGossipHandle), Box<dyn std::error::Error>> {
        let shared = Arc::new(SharedState {
            inbound: Mutex::new(VecDeque::new()),
            peer_map: RwLock::new(PeerMap::default()),
            validators: RwLock::new(HashSet::new()),
        });

        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (tx_tx, tx_rx) = mpsc::unbounded_channel();

        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_quic()
            .with_behaviour(|key| {
                TorusBehaviour::new(key).expect("failed to create TorusBehaviour")
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(std::time::Duration::from_secs(60))
            })
            .build();

        swarm.listen_on(config.listen_addr.clone())?;

        for (peer_id, addr) in &config.bootstrap_peers {
            swarm
                .behaviour_mut()
                .kademlia
                .add_address(peer_id, addr.clone());
            if let Err(e) = swarm.dial(addr.clone()) {
                warn!("Failed to dial bootstrap {addr}: {e}");
            }
        }

        let shared_clone = shared.clone();
        tokio::spawn(run_swarm(swarm, command_rx, tx_rx, shared_clone, local_key));

        let network = Self { command_tx, shared };
        let tx_handle = TxGossipHandle { tx_sender: tx_tx };
        Ok((network, tx_handle))
    }

    /// Register a peer's VerifyingKey to PeerId mapping.
    pub fn register_peer(&self, vk: VerifyingKey, peer_id: PeerId) {
        let _ = self
            .command_tx
            .send(NetworkCommand::RegisterPeer { vk, peer_id });
    }

    /// Dial a multiaddr to connect to a peer.
    pub fn dial(&self, addr: Multiaddr) {
        let _ = self.command_tx.send(NetworkCommand::Dial { addr });
    }
}

impl Network for LibP2PNetwork {
    fn init_validator_set(&mut self, validator_set: ValidatorSet) {
        let mut validators = self.shared.validators.write().unwrap();
        validators.clear();
        for vk in validator_set.validators() {
            validators.insert(vk.to_bytes());
        }
    }

    fn update_validator_set(&mut self, updates: ValidatorSetUpdates) {
        let mut validators = self.shared.validators.write().unwrap();
        for (vk, _power) in updates.inserts() {
            validators.insert(vk.to_bytes());
        }
        for vk in updates.deletes() {
            validators.remove(&vk.to_bytes());
        }
    }

    fn broadcast(&mut self, message: Message) {
        let _ = self.command_tx.send(NetworkCommand::Broadcast { message });
    }

    fn send(&mut self, peer: VerifyingKey, message: Message) {
        let _ = self.command_tx.send(NetworkCommand::Send {
            target: peer,
            message,
        });
    }

    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        self.shared.inbound.lock().unwrap().pop_front()
    }
}
