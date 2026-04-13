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
use crate::peer::PeerMap;
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
    mut swarm: Swarm<TorusBehaviour>,
    mut command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    mut tx_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    shared: Arc<SharedState>,
    local_key: VerifyingKey,
) {
    let consensus_topic = gossipsub::IdentTopic::new(CONSENSUS_TOPIC);
    let tx_topic = gossipsub::IdentTopic::new(TX_TOPIC);

    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&consensus_topic) {
        warn!("Failed to subscribe to consensus topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&tx_topic) {
        warn!("Failed to subscribe to tx topic: {e:?}");
    }

    let mut tx_gossip_state = TxGossipState::new(60, 100);

    loop {
        let action = tokio::select! {
            event = swarm.select_next_some() => SwarmAction::Event(Box::new(event)),
            cmd = command_rx.recv() => SwarmAction::Command(cmd.map(Box::new)),
            tx = tx_rx.recv() => SwarmAction::Tx(tx),
        };

        match action {
            SwarmAction::Event(event) => {
                handle_event(*event, &mut swarm, &shared, &mut tx_gossip_state);
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
) {
    match event {
        SwarmEvent::Behaviour(TorusBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            propagation_source,
            message,
            ..
        })) => {
            let consensus_hash = gossipsub::IdentTopic::new(CONSENSUS_TOPIC).hash();
            if message.topic == consensus_hash {
                handle_consensus_gossip(&message.data, shared);
            } else {
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
            ..
        })) => {
            if let Ok(msg) =
                hotstuff_rs::networking::messages::Message::try_from_slice(&request.payload)
            {
                if let Ok(sender_vk) = VerifyingKey::from_bytes(&request.sender_key) {
                    shared.inbound.lock().unwrap().push_back((sender_vk, msg));
                }
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
            for addr in info.listen_addrs {
                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
            }
        }
        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {address}"),
        SwarmEvent::ConnectionEstablished { peer_id, .. } => debug!("Connected to {peer_id}"),
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
            shared
                .inbound
                .lock()
                .unwrap()
                .push_back((*local_key, message.clone()));
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

fn handle_consensus_gossip(data: &[u8], shared: &SharedState) {
    if data.len() < 33 {
        warn!("Consensus message too short ({} bytes)", data.len());
        return;
    }
    let sender_bytes: [u8; 32] = data[..32].try_into().unwrap();
    let msg_bytes = &data[32..];
    let sender_vk = match VerifyingKey::from_bytes(&sender_bytes) {
        Ok(vk) => vk,
        Err(_) => {
            warn!("Invalid sender key in consensus gossip");
            return;
        }
    };
    match hotstuff_rs::networking::messages::Message::try_from_slice(msg_bytes) {
        Ok(msg) => {
            shared.inbound.lock().unwrap().push_back((sender_vk, msg));
        }
        Err(e) => {
            warn!("Failed to deserialize consensus message: {e}");
        }
    }
}
