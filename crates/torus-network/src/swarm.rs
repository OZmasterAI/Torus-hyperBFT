use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::VerifyingKey;
use libp2p::futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, request_response, Multiaddr, PeerId, Swarm};
use sha3::{Digest, Keccak256};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::behaviour::{TorusBehaviour, TorusBehaviourEvent, CONSENSUS_TOPIC, TX_TOPIC};
use crate::codec::{DirectRequest, DirectResponse};
use crate::config::NetworkConfig;
use crate::peer::PeerMap;
use crate::peer_scoring::{
    ConsensusRateLimiter, PeerScoring, PENALTY_INVALID_CONSENSUS_MSG, PENALTY_INVALID_TX,
    PENALTY_EXCESSIVE_RATE, REWARD_BLOCK_RELAY,
};
use crate::tx_gossip::TxGossipState;

pub enum NetworkCommand {
    Broadcast {
        message: hotstuff_rs::networking::messages::Message,
    },
    Send {
        target: VerifyingKey,
        message: hotstuff_rs::networking::messages::Message,
    },
    RegisterPeer {
        vk: VerifyingKey,
        peer_id: PeerId,
    },
    Dial {
        addr: Multiaddr,
    },
}

pub struct SharedState {
    pub inbound: Mutex<VecDeque<(VerifyingKey, hotstuff_rs::networking::messages::Message)>>,
    pub peer_map: RwLock<PeerMap>,
    pub validators: RwLock<HashSet<[u8; 32]>>,
}

enum SwarmAction {
    Event(Box<SwarmEvent<TorusBehaviourEvent>>),
    Command(Option<Box<NetworkCommand>>),
    Tx(Option<Vec<u8>>),
}

pub async fn run_swarm(
    swarm: Swarm<TorusBehaviour>,
    command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    tx_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    shared: Arc<SharedState>,
    local_key: VerifyingKey,
) {
    run_swarm_with_config(swarm, command_rx, tx_rx, shared, local_key, &NetworkConfig::default()).await
}

pub async fn run_swarm_with_config(
    mut swarm: Swarm<TorusBehaviour>,
    mut command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    mut tx_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    shared: Arc<SharedState>,
    local_key: VerifyingKey,
    config: &NetworkConfig,
) {
    let consensus_topic = gossipsub::IdentTopic::new(CONSENSUS_TOPIC);
    let tx_topic = gossipsub::IdentTopic::new(TX_TOPIC);

    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&consensus_topic) {
        warn!("Failed to subscribe to consensus topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&tx_topic) {
        warn!("Failed to subscribe to tx topic: {e:?}");
    }

    let mut tx_gossip_state = TxGossipState::new(
        config.tx_dedup_window_secs,
        config.tx_rate_limit_per_peer,
    );
    let mut consensus_rate_limiter = ConsensusRateLimiter::new(config.consensus_rate_limit_per_peer);
    let mut peer_scoring = PeerScoring::new(config.ban_list_path.clone());

    // Block any previously banned peers
    for peer_id in peer_scoring.permanently_banned_peers() {
        swarm.behaviour_mut().block_list.block_peer(peer_id);
    }

    let max_consensus_msg_size = config.max_consensus_message_size;
    let max_tx_msg_size = config.max_tx_message_size;

    // Periodic cleanup interval for unbounded structures (Batch EK: CONS-FIND-21-24).
    let mut cleanup_interval = tokio::time::interval(std::time::Duration::from_secs(60));
    cleanup_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let stale_age = std::time::Duration::from_secs(3600); // 1 hour

    loop {
        let action = tokio::select! {
            event = swarm.select_next_some() => SwarmAction::Event(Box::new(event)),
            cmd = command_rx.recv() => SwarmAction::Command(cmd.map(Box::new)),
            tx = tx_rx.recv() => SwarmAction::Tx(tx),
            _ = cleanup_interval.tick() => {
                peer_scoring.cleanup_stale(stale_age);
                consensus_rate_limiter.cleanup_stale();
                continue;
            }
        };

        match action {
            SwarmAction::Event(event) => {
                handle_event(
                    *event,
                    &mut swarm,
                    &shared,
                    &mut tx_gossip_state,
                    &mut consensus_rate_limiter,
                    &mut peer_scoring,
                    max_consensus_msg_size,
                    max_tx_msg_size,
                );
            }
            SwarmAction::Command(Some(cmd)) => {
                handle_command(*cmd, &mut swarm, &shared, &local_key, &consensus_topic);
            }
            SwarmAction::Command(None) => {
                info!("Command channel closed, shutting down swarm");
                return;
            }
            SwarmAction::Tx(Some(tx_bytes)) => {
                if let Err(e) = swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(tx_topic.clone(), tx_bytes)
                {
                    warn!("Failed to publish tx: {e:?}");
                }
            }
            SwarmAction::Tx(None) => {}
        }
    }
}

fn handle_event(
    event: SwarmEvent<TorusBehaviourEvent>,
    swarm: &mut Swarm<TorusBehaviour>,
    shared: &SharedState,
    tx_gossip_state: &mut TxGossipState,
    consensus_rate_limiter: &mut ConsensusRateLimiter,
    peer_scoring: &mut PeerScoring,
    max_consensus_msg_size: usize,
    max_tx_msg_size: usize,
) {
    match event {
        SwarmEvent::Behaviour(TorusBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            propagation_source,
            message,
            ..
        })) => {
            // Check if peer is banned (Phase 3: 3.1.7)
            if peer_scoring.is_banned(&propagation_source) {
                return;
            }

            let consensus_hash = gossipsub::IdentTopic::new(CONSENSUS_TOPIC).hash();
            if message.topic == consensus_hash {
                // Message size validation (Phase 3: 3.1.7)
                if message.data.len() > max_consensus_msg_size {
                    warn!(
                        peer = %propagation_source,
                        size = message.data.len(),
                        "oversized consensus message rejected"
                    );
                    peer_scoring.penalize(
                        &propagation_source,
                        PENALTY_INVALID_CONSENSUS_MSG,
                        "oversized consensus message",
                    );
                    return;
                }

                // Consensus rate limiting (Phase 3: 3.1.7)
                if !consensus_rate_limiter.check_and_increment(&propagation_source) {
                    peer_scoring.penalize(
                        &propagation_source,
                        PENALTY_EXCESSIVE_RATE,
                        "consensus message rate exceeded",
                    );
                    return;
                }

                // Use the cryptographic author (gossipsub-verified in Strict mode)
                // rather than the last-hop forwarder — gossip meshes relay messages,
                // so propagation_source can be a different validator than the signer.
                let author = match message.source {
                    Some(pid) => pid,
                    None => {
                        warn!(
                            peer = %propagation_source,
                            "consensus gossip without source (strict mode violation)"
                        );
                        peer_scoring.penalize(
                            &propagation_source,
                            PENALTY_INVALID_CONSENSUS_MSG,
                            "consensus message missing source",
                        );
                        return;
                    }
                };
                if peer_scoring.is_banned(&author) {
                    return;
                }
                handle_consensus_gossip(&message.data, shared, peer_scoring, &author);
            } else {
                // TX message size validation (Phase 3: 3.1.7)
                if message.data.len() > max_tx_msg_size {
                    warn!(
                        peer = %propagation_source,
                        size = message.data.len(),
                        "oversized tx message rejected"
                    );
                    peer_scoring.penalize(
                        &propagation_source,
                        PENALTY_INVALID_TX,
                        "oversized tx message",
                    );
                    return;
                }

                let tx_hash: [u8; 32] = Keccak256::digest(&message.data).into();
                if tx_gossip_state.should_accept(tx_hash, propagation_source) {
                    debug!("Received new tx from {propagation_source}");
                }
            }
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::Direct(request_response::Event::Message {
            message:
                request_response::Message::Request {
                    request, channel, ..
                },
            peer,
            ..
        })) => {
            // Check if peer is banned (Phase 3: 3.1.7)
            if peer_scoring.is_banned(&peer) {
                let _ = swarm
                    .behaviour_mut()
                    .direct
                    .send_response(channel, DirectResponse);
                return;
            }

            // FIX 2 (CONS-FIND-04): Verify claimed sender matches authenticated peer.
            let sender_vk = match verify_sender_key(
                &request.sender_key, &peer, shared, peer_scoring, "direct",
            ) {
                Some(vk) => vk,
                None => {
                    let _ = swarm
                        .behaviour_mut()
                        .direct
                        .send_response(channel, DirectResponse);
                    return;
                }
            };
            if let Ok(msg) =
                hotstuff_rs::networking::messages::Message::try_from_slice(&request.payload)
            {
                enqueue_inbound(&shared.inbound, sender_vk, msg);
                peer_scoring.reward(&peer, REWARD_BLOCK_RELAY);
            } else {
                peer_scoring.penalize(&peer, PENALTY_INVALID_CONSENSUS_MSG, "malformed direct message");
            }
            let _ = swarm
                .behaviour_mut()
                .direct
                .send_response(channel, DirectResponse);
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) => {
            // Don't add banned peers to Kademlia
            if peer_scoring.is_banned(&peer_id) {
                return;
            }
            for addr in info.listen_addrs {
                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
            }
        }
        // FIX 6 (CONS-FIND-25-32): Sync protocol events.
        // TODO: When sync serving is implemented, handle sync requests in a
        // spawned task (tokio::spawn) to avoid blocking the consensus event loop.
        SwarmEvent::Behaviour(TorusBehaviourEvent::SyncProto(_)) => {
            debug!("sync protocol event (not yet handled)");
        }
        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {address}"),
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            if peer_scoring.is_banned(&peer_id) {
                // BUG FIX (3.1): Disconnect banned peers that slip through
                let _ = swarm.disconnect_peer_id(peer_id);
                debug!("Disconnected banned peer {peer_id}");
            } else {
                debug!("Connected to {peer_id}");
            }
        }
        SwarmEvent::ConnectionClosed { peer_id, .. } => debug!("Disconnected from {peer_id}"),
        _ => {}
    }
}

fn handle_command(
    cmd: NetworkCommand,
    swarm: &mut Swarm<TorusBehaviour>,
    shared: &SharedState,
    local_key: &VerifyingKey,
    consensus_topic: &gossipsub::IdentTopic,
) {
    match cmd {
        NetworkCommand::Broadcast { message } => {
            // Self-delivery (hotstuff_rs expects proposer to receive its own broadcast)
            enqueue_inbound(&shared.inbound, *local_key, message.clone());
            let mut envelope = local_key.to_bytes().to_vec();
            if let Ok(msg_bytes) = message.try_to_vec() {
                envelope.extend_from_slice(&msg_bytes);
                if let Err(e) = swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(consensus_topic.clone(), envelope)
                {
                    warn!("Failed to publish consensus message: {e:?}");
                }
            }
        }
        NetworkCommand::Send { target, message } => {
            let peer_map = shared.peer_map.read().unwrap();
            if let Some(peer_id) = peer_map.get_peer_id(&target) {
                if let Ok(payload) = message.try_to_vec() {
                    let request = DirectRequest {
                        sender_key: local_key.to_bytes(),
                        payload,
                    };
                    swarm.behaviour_mut().direct.send_request(peer_id, request);
                }
            } else {
                warn!("Cannot send: no PeerId for target validator");
            }
        }
        NetworkCommand::RegisterPeer { vk, peer_id } => {
            shared.peer_map.write().unwrap().insert(vk, peer_id);
            debug!("Registered peer mapping: {peer_id}");
        }
        NetworkCommand::Dial { addr } => {
            if let Err(e) = swarm.dial(addr.clone()) {
                warn!("Failed to dial {addr}: {e}");
            }
        }
    }
}

/// Maximum number of inbound consensus messages before backpressure (Batch EK: CONS-FIND-10).
const MAX_INBOUND_QUEUE: usize = 10_000;

/// Verify that a claimed sender key matches the authenticated peer identity.
/// Returns the VerifyingKey on success, or None if verification fails (with penalty applied).
fn verify_sender_key(
    claimed_key: &[u8; 32],
    authenticated_peer: &PeerId,
    shared: &SharedState,
    peer_scoring: &mut PeerScoring,
    context: &str,
) -> Option<VerifyingKey> {
    let vk = match VerifyingKey::from_bytes(claimed_key) {
        Ok(vk) => vk,
        Err(_) => {
            warn!(%authenticated_peer, "{context}: invalid sender key");
            peer_scoring.penalize(
                authenticated_peer,
                PENALTY_INVALID_CONSENSUS_MSG,
                &format!("{context}: invalid sender key"),
            );
            return None;
        }
    };
    let peer_map = shared.peer_map.read().unwrap();
    match peer_map.get_vk(authenticated_peer) {
        Some(registered_vk) if registered_vk.to_bytes() == *claimed_key => Some(vk),
        Some(_) => {
            warn!(
                %authenticated_peer,
                "{context}: claimed sender key does not match registered peer identity"
            );
            peer_scoring.penalize(
                authenticated_peer,
                PENALTY_INVALID_CONSENSUS_MSG,
                &format!("{context}: sender key mismatch"),
            );
            None
        }
        None => {
            warn!(%authenticated_peer, "{context}: peer not in peer map");
            peer_scoring.penalize(
                authenticated_peer,
                PENALTY_INVALID_CONSENSUS_MSG,
                &format!("{context}: unregistered peer"),
            );
            None
        }
    }
}

/// Push a message to the inbound queue with capacity enforcement.
fn enqueue_inbound(
    inbound: &Mutex<VecDeque<(VerifyingKey, hotstuff_rs::networking::messages::Message)>>,
    sender: VerifyingKey,
    msg: hotstuff_rs::networking::messages::Message,
) {
    let mut queue = inbound.lock().unwrap();
    if queue.len() >= MAX_INBOUND_QUEUE {
        warn!("inbound queue full ({MAX_INBOUND_QUEUE}), dropping incoming message");
        return;
    }
    queue.push_back((sender, msg));
}

fn handle_consensus_gossip(
    data: &[u8],
    shared: &SharedState,
    peer_scoring: &mut PeerScoring,
    source: &PeerId,
) {
    if data.len() < 33 {
        warn!("Consensus message too short ({} bytes)", data.len());
        peer_scoring.penalize(source, PENALTY_INVALID_CONSENSUS_MSG, "consensus message too short");
        return;
    }
    let sender_bytes: [u8; 32] = data[..32].try_into().unwrap();
    let msg_bytes = &data[32..];

    // FIX 1 (CONS-PF-12): Verify claimed sender matches authenticated peer.
    let sender_vk = match verify_sender_key(
        &sender_bytes, source, shared, peer_scoring, "gossip",
    ) {
        Some(vk) => vk,
        None => return,
    };

    match hotstuff_rs::networking::messages::Message::try_from_slice(msg_bytes) {
        Ok(msg) => {
            enqueue_inbound(&shared.inbound, sender_vk, msg);
            peer_scoring.reward(source, REWARD_BLOCK_RELAY);
        }
        Err(e) => {
            warn!("Failed to deserialize consensus message: {e}");
            peer_scoring.penalize(source, PENALTY_INVALID_CONSENSUS_MSG, "malformed consensus message");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vk(seed: u8) -> VerifyingKey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        let sk = ed25519_dalek::SigningKey::from_bytes(&bytes);
        sk.verifying_key()
    }

    fn test_peer(id: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    fn test_shared() -> SharedState {
        SharedState {
            inbound: Mutex::new(VecDeque::new()),
            peer_map: RwLock::new(PeerMap::default()),
            validators: RwLock::new(HashSet::new()),
        }
    }

    #[test]
    fn verify_sender_key_accepts_registered_peer() {
        let vk = test_vk(1);
        let peer = test_peer(1);
        let shared = test_shared();
        shared.peer_map.write().unwrap().insert(vk, peer);
        let mut scoring = PeerScoring::new(None);
        let result = verify_sender_key(&vk.to_bytes(), &peer, &shared, &mut scoring, "test");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), vk);
    }

    #[test]
    fn verify_sender_key_rejects_mismatched_peer() {
        let vk_a = test_vk(2);
        let vk_b = test_vk(3);
        let peer = test_peer(2);
        let shared = test_shared();
        shared.peer_map.write().unwrap().insert(vk_a, peer);
        let mut scoring = PeerScoring::new(None);
        // Claim to be vk_b but authenticated as peer mapped to vk_a
        let result = verify_sender_key(&vk_b.to_bytes(), &peer, &shared, &mut scoring, "test");
        assert!(result.is_none());
        // Should have been penalized
        assert!(scoring.score(&peer) < 100);
    }

    #[test]
    fn verify_sender_key_rejects_unregistered_peer() {
        let vk = test_vk(4);
        let peer = test_peer(4);
        let shared = test_shared();
        // Don't register the peer
        let mut scoring = PeerScoring::new(None);
        let result = verify_sender_key(&vk.to_bytes(), &peer, &shared, &mut scoring, "test");
        assert!(result.is_none());
        assert!(scoring.score(&peer) < 100);
    }

    #[test]
    fn verify_sender_key_rejects_invalid_key_bytes() {
        let peer = test_peer(5);
        let shared = test_shared();
        let mut scoring = PeerScoring::new(None);
        // All zeros is not a valid ed25519 key
        let result = verify_sender_key(&[0u8; 32], &peer, &shared, &mut scoring, "test");
        assert!(result.is_none());
    }

    #[test]
    fn inbound_queue_capacity_constant() {
        assert_eq!(MAX_INBOUND_QUEUE, 10_000);
    }
}
