use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use borsh::BorshSerialize;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hotstuff_rs::hotstuff::messages::{BlockDataRequest, BlockDataResponse, HotStuffMessage};
use hotstuff_rs::networking::messages::Message;
use hotstuff_rs::networking::network::Network;
use hotstuff_rs::types::block::Block;
use hotstuff_rs::types::data_types::{CryptoHash, ViewNumber};
use zeroize::Zeroize;
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;
use hotstuff_rs::types::validator_set::ValidatorSet;
use libp2p::{identity, multiaddr::Protocol, Multiaddr, PeerId, SwarmBuilder};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::behaviour::TorusBehaviour;
use crate::config::NetworkConfig;
use crate::peer::PeerMap;
use crate::swarm::{run_swarm_with_config, NetworkCommand, SharedState};
use crate::tx_gossip::{NativeGossipHandle, TxGossipHandle};

/// Derive a libp2p PeerId from a validator's ed25519 VerifyingKey.
///
/// Every validator uses this same derivation, so peers agree on each other's
/// PeerIds without any runtime exchange — the consensus validator set is the
/// single source of truth for peer identity.
pub fn peer_id_from_verifying_key(vk: &VerifyingKey) -> PeerId {
    let pk = identity::ed25519::PublicKey::try_from_bytes(vk.as_bytes())
        .expect("ed25519 VerifyingKey bytes are always valid libp2p ed25519 pubkey");
    identity::PublicKey::from(pk).to_peer_id()
}

/// libp2p-based Network implementation for hotstuff_rs.
///
/// Bridges async libp2p with the synchronous Network trait using channels
/// and shared state protected by `Arc<Mutex/RwLock>`.
pub struct LibP2PNetwork {
    command_tx: mpsc::UnboundedSender<NetworkCommand>,
    shared: Arc<SharedState>,
    native_inbound_rx: Option<mpsc::UnboundedReceiver<(torus_types::Address, torus_types::SignedNativeAction)>>,
}

impl Clone for LibP2PNetwork {
    fn clone(&self) -> Self {
        Self {
            command_tx: self.command_tx.clone(),
            shared: self.shared.clone(),
            native_inbound_rx: None,
        }
    }
}

impl LibP2PNetwork {
    /// Create a new LibP2PNetwork. Must be called from within a tokio runtime.
    /// Spawns a background task to drive the libp2p swarm.
    ///
    /// The libp2p identity is derived deterministically from the validator
    /// signing key: every validator computes the same PeerId for any given
    /// VerifyingKey via [`peer_id_from_verifying_key`].
    pub async fn new(
        config: NetworkConfig,
        signing_key: SigningKey,
    ) -> Result<(Self, TxGossipHandle, NativeGossipHandle), Box<dyn std::error::Error>> {
        Self::with_metrics(config, signing_key, None).await
    }

    /// Create a new LibP2PNetwork with optional Prometheus metrics instrumentation.
    /// Must be called from within a tokio runtime.
    /// Spawns a background task to drive the libp2p swarm.
    pub async fn with_metrics(
        config: NetworkConfig,
        signing_key: SigningKey,
        metrics: Option<Arc<torus_telemetry::Metrics>>,
    ) -> Result<(Self, TxGossipHandle, NativeGossipHandle), Box<dyn std::error::Error>> {
        let local_key = signing_key.verifying_key();

        let mut secret_bytes = signing_key.to_bytes();
        let libp2p_secret = identity::ed25519::SecretKey::try_from_bytes(&mut secret_bytes)
            .map_err(|e| {
                secret_bytes.zeroize();
                format!("libp2p ed25519 secret: {e}")
            })?;
        secret_bytes.zeroize();
        let libp2p_keypair =
            identity::Keypair::from(identity::ed25519::Keypair::from(libp2p_secret));

        let (native_inbound_tx, native_inbound_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(SharedState {
            inbound: Mutex::new(VecDeque::new()),
            peer_map: RwLock::new(PeerMap::default()),
            validators: RwLock::new(HashSet::new()),
            metrics,
            block_store: RwLock::new(HashMap::new()),
            block_data_inbound: Mutex::new(VecDeque::new()),
            native_action_inbound: Some(native_inbound_tx),
        });

        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (tx_tx, tx_rx) = mpsc::unbounded_channel();
        let (native_tx, native_rx) = mpsc::channel(8192);

        let max_peers = config.max_peers;
        let mut swarm = SwarmBuilder::with_existing_identity(libp2p_keypair)
            .with_tokio()
            .with_quic()
            .with_dns()
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
            .with_behaviour(|key| {
                TorusBehaviour::with_limits(key, max_peers)
                    .expect("failed to create TorusBehaviour")
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
            let dial_addr = addr.clone().with(Protocol::P2p(*peer_id));
            info!(%peer_id, %dial_addr, "dialing bootstrap peer");
            if let Err(e) = swarm.dial(dial_addr.clone()) {
                warn!("Failed to dial bootstrap {dial_addr}: {e}");
            }
        }

        let shared_clone = shared.clone();
        let config_clone = config.clone();
        tokio::spawn(async move {
            run_swarm_with_config(
                swarm, command_rx, tx_rx, native_rx, shared_clone, local_key, &config_clone,
            ).await
        });

        let network = Self { command_tx, shared, native_inbound_rx: Some(native_inbound_rx) };
        let tx_handle = TxGossipHandle { tx_sender: tx_tx };
        let native_handle = NativeGossipHandle { sender: native_tx };
        Ok((network, tx_handle, native_handle))
    }

    /// Take the inbound native action receiver. Called once at startup to
    /// spawn a task that drains gossip-received actions into the mempool.
    pub fn take_native_action_rx(&mut self) -> Option<mpsc::UnboundedReceiver<(torus_types::Address, torus_types::SignedNativeAction)>> {
        self.native_inbound_rx.take()
    }

    /// Register a peer's VerifyingKey to PeerId mapping.
    ///
    /// Normally unnecessary — `init_validator_set` and `update_validator_set`
    /// keep the peer map in sync with the consensus validator set. Kept for
    /// ad-hoc registration (observers, tests).
    pub fn register_peer(&self, vk: VerifyingKey, peer_id: PeerId) {
        let _ = self
            .command_tx
            .send(NetworkCommand::RegisterPeer { vk, peer_id });
    }

    /// Dial a multiaddr to connect to a peer.
    pub fn dial(&self, addr: Multiaddr) {
        let _ = self.command_tx.send(NetworkCommand::Dial { addr });
    }

    /// Forward a native action directly to a specific peer (leader).
    /// Payload format: sender_address(20) + serde_json(SignedNativeAction).
    pub fn forward_native_action(&self, target: VerifyingKey, payload: Vec<u8>) {
        let _ = self.command_tx.send(NetworkCommand::ForwardNativeAction { target, payload });
    }
}

impl Network for LibP2PNetwork {
    fn init_validator_set(&mut self, validator_set: ValidatorSet) {
        let mut validators = self.shared.validators.write().unwrap();
        let mut peer_map = self.shared.peer_map.write().unwrap();
        validators.clear();
        peer_map.clear();
        for vk in validator_set.validators() {
            validators.insert(vk.to_bytes());
            let peer_id = peer_id_from_verifying_key(vk);
            peer_map.insert(*vk, peer_id);
        }
        drop(peer_map);
        drop(validators);
        for vk in validator_set.validators() {
            let peer_id = peer_id_from_verifying_key(vk);
            let _ = self.command_tx.send(NetworkCommand::DialPeer { peer_id });
        }
    }

    fn update_validator_set(&mut self, updates: ValidatorSetUpdates) {
        let mut validators = self.shared.validators.write().unwrap();
        let mut peer_map = self.shared.peer_map.write().unwrap();
        for (vk, _power) in updates.inserts() {
            validators.insert(vk.to_bytes());
            let peer_id = peer_id_from_verifying_key(vk);
            peer_map.insert(*vk, peer_id);
        }
        for vk in updates.deletes() {
            validators.remove(&vk.to_bytes());
            peer_map.remove_by_vk(vk);
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

    fn request_block_data(&mut self, peer: VerifyingKey, request: BlockDataRequest) {
        // Route through the proven HotStuffMessage direct-message path instead of
        // the dedicated /torus/block-data/1.0 protocol which silently fails in devnet.
        let msg: Message = HotStuffMessage::BlockDataRequest(request).into();
        self.send(peer, msg);
    }

    fn recv_block_data(&mut self) -> Option<(VerifyingKey, BlockDataResponse)> {
        let (vk, view, block_bytes) = self.shared.block_data_inbound.lock().unwrap().pop_front()?;
        match borsh::BorshDeserialize::try_from_slice(&block_bytes) {
            Ok(block) => Some((vk, BlockDataResponse { view: ViewNumber::new(view), block })),
            Err(e) => {
                tracing::warn!(
                    view,
                    bytes_len = block_bytes.len(),
                    %e,
                    "block-data recv: borsh deserialization FAILED — dropping response"
                );
                None
            }
        }
    }

    fn store_block_for_serving(&mut self, hash: CryptoHash, block: Block) {
        if let Ok(block_bytes) = block.try_to_vec() {
            let _ = self.command_tx.send(NetworkCommand::StoreBlock {
                hash: hash.bytes(),
                block_bytes,
            });
        }
    }
}
