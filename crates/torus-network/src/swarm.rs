use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use borsh::{BorshDeserialize, BorshSerialize};
use ed25519_dalek::VerifyingKey;
use libp2p::futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, kad, request_response, Multiaddr, PeerId, Swarm};
use sha3::{Digest, Keccak256};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use torus_state::NativeDaStore;

use crate::behaviour::{TorusBehaviour, TorusBehaviourEvent, CONSENSUS_TOPIC, NATIVE_ACTION_TOPIC, TX_TOPIC};
use crate::codec::{
    BlockDataNetRequest, BlockDataNetResponse, DirectRequest, DirectResponse, NativeDaNetRequest,
    NativeDaNetResponse,
};
use crate::config::NetworkConfig;
use crate::peer::PeerMap;
use crate::pending_send::PendingSendQueue;
use crate::peer_scoring::{
    ConsensusRateLimiter, PeerScoring, PENALTY_INVALID_CONSENSUS_MSG, PENALTY_INVALID_TX,
    REWARD_BLOCK_RELAY,
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
    DialPeer {
        peer_id: PeerId,
    },
    /// Request block data via the dedicated `/torus/block-data/1.0` protocol.
    BlockDataRequest {
        target: VerifyingKey,
        block_hash: [u8; 32],
        view: u64,
    },
    /// Store a block so the network thread can serve it to requesters.
    StoreBlock {
        hash: [u8; 32],
        block_bytes: Vec<u8>,
    },
    /// Forward a native action directly to the leader node.
    ForwardNativeAction {
        target: VerifyingKey,
        payload: Vec<u8>,
    },
    /// Proposer pushes batched actions to all validators via req/res before CompactBlock proposal.
    BroadcastNativeActions {
        payload: Vec<u8>,
    },
    /// RARE pull-fallback: fetch native-action bodies by-hash from `target` via the
    /// dedicated `/torus/native-da/1.0` protocol when a CompactBlock body is absent
    /// locally (Phase C Task 5/6). Push covers the common case; this fires on a miss.
    FetchNativeActions {
        target: VerifyingKey,
        hashes: Vec<[u8; 32]>,
    },
}

/// Marker byte prefixed to forwarded native action payloads in DirectRequest.
const FORWARD_ACTION_MARKER: u8 = 0xFE;
/// Marker byte for batched pre-proposal action payloads (CompactBlock dissemination).
const PRE_PROPOSAL_BATCH_MARKER: u8 = 0xFD;
/// How many recent pre-proposal bundles to retain for re-push on (re)connect (Task 4).
const RECENT_NATIVE_BUNDLES_CAP: usize = 3;

/// Push `item` into a bounded ring, evicting the oldest when at `cap`.
fn push_bounded(ring: &mut VecDeque<Vec<u8>>, item: Vec<u8>, cap: usize) {
    if ring.len() >= cap {
        ring.pop_front();
    }
    ring.push_back(item);
}

/// Serve native-action bodies for the requested hashes from the durable DA store
/// (Task 5). Returns one entry per requested hash, **in request order**; an empty
/// `Vec` means the body was not found (or no store is attached). The stored
/// `bincode(SignedNativeAction)` bytes are shipped verbatim. Pure over the store
/// handle so the serve semantics are unit-testable without a live swarm.
fn serve_native_da_bodies(store: Option<&NativeDaStore>, hashes: &[[u8; 32]]) -> Vec<Vec<u8>> {
    let Some(store) = store else {
        return vec![Vec::new(); hashes.len()];
    };
    hashes
        .iter()
        .map(|h| match store.get_raw(h) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => Vec::new(),
            Err(e) => {
                warn!(?e, "native-da serve: DA store read failed");
                Vec::new()
            }
        })
        .collect()
}

pub struct SharedState {
    pub inbound: Mutex<VecDeque<(VerifyingKey, hotstuff_rs::networking::messages::Message)>>,
    pub peer_map: RwLock<PeerMap>,
    pub validators: RwLock<HashSet<[u8; 32]>>,
    pub metrics: Option<Arc<torus_telemetry::Metrics>>,
    /// Block store for serving block-data requests on the network thread.
    /// Maps block_hash → borsh-encoded Block bytes.
    pub block_store: RwLock<HashMap<[u8; 32], Vec<u8>>>,
    /// Inbound block-data responses (separate from consensus inbound queue).
    /// Entries: (sender_vk, view, borsh-encoded Block bytes).
    pub block_data_inbound: Mutex<VecDeque<(VerifyingKey, u64, Vec<u8>)>>,
    /// Inbound native actions received from gossip (deserialized by swarm, consumed by mempool task).
    /// Tuple: (pre-verified sender address, signed action) — receivers skip ECDSA recovery.
    pub native_action_inbound: Option<tokio::sync::mpsc::UnboundedSender<(torus_types::Address, torus_types::SignedNativeAction)>>,
    /// Consensus messages buffered for a validator that is in the peer map but
    /// not currently connected (Task 3 — corrected seam). A validator is mapped
    /// from genesis via `init_validator_set`, so the real gap is connectivity,
    /// not registration. Flushed on `SwarmEvent::ConnectionEstablished`.
    pub pending_sends: Mutex<PendingSendQueue<hotstuff_rs::networking::messages::Message>>,
    /// In-flight direct consensus sends, keyed by request id, so a `Direct`
    /// `OutboundFailure` (previously swallowed by `_ => {}`) can re-enqueue the
    /// message for the next reconnect flush instead of dropping it (Task 3).
    pub outbound_direct: Mutex<HashMap<request_response::OutboundRequestId, (VerifyingKey, hotstuff_rs::networking::messages::Message)>>,
    /// Bounded ring of recent pre-proposal action bundle envelopes (Task 4).
    /// Re-pushed to a validator that (re)connects after the original push, so its
    /// mempool catches up before the next CompactBlock it must reconstruct.
    pub recent_native_bundles: Mutex<VecDeque<Vec<u8>>>,
    /// Durable native-action DA store handle, used to SERVE bodies by-hash on the
    /// `/torus/native-da/1.0` protocol (Task 5). `None` until attached at startup
    /// via `LibP2PNetwork::set_native_da_store` (a cheap clone over the same
    /// StateDb). Tests/observers may leave it unset → serve returns not-found.
    pub native_da: RwLock<Option<NativeDaStore>>,
    /// Inbound native-action bodies received via the pull-fallback response
    /// (each = `bincode(SignedNativeAction)`), drained by the consensus app on a
    /// reconstruction miss (Task 6). Bounded by `MAX_INBOUND_QUEUE`.
    pub native_da_inbound: Mutex<VecDeque<Vec<u8>>>,
    /// Per-validator queue of pre-proposal native-action push envelopes whose target
    /// was not connected at push time (Task 7). Flushed on the validator's
    /// `ConnectionEstablished` so a push is DELIVERED on (re)connect instead of
    /// dropped under load — hardening the push-primary that feeds every validator's
    /// DA store (push covers the common case so the pull-fallback stays rare).
    pub pending_native_pushes: Mutex<PendingSendQueue<Vec<u8>>>,
}

enum SwarmAction {
    Event(Box<SwarmEvent<TorusBehaviourEvent>>),
    Command(Option<Box<NetworkCommand>>),
    Tx(Option<Vec<u8>>),
    NativeAction(Option<Vec<u8>>),
    FlushNativeBatch,
}

const NATIVE_BATCH_INTERVAL_MS: u64 = 50;
const NATIVE_BATCH_MAX_SIZE: usize = 1024;
const NATIVE_BATCH_MARKER: u8 = 0xFF;

fn serialize_native_batch(actions: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = 1 + 4 + actions.iter().map(|a| 4 + a.len()).sum::<usize>();
    let mut buf = Vec::with_capacity(total);
    buf.push(NATIVE_BATCH_MARKER);
    buf.extend_from_slice(&(actions.len() as u32).to_le_bytes());
    for action in actions {
        buf.extend_from_slice(&(action.len() as u32).to_le_bytes());
        buf.extend_from_slice(action);
    }
    buf
}

fn deserialize_native_batch(data: &[u8]) -> Option<Vec<&[u8]>> {
    if data.first() != Some(&NATIVE_BATCH_MARKER) || data.len() < 5 {
        return None;
    }
    let count = u32::from_le_bytes(data[1..5].try_into().ok()?) as usize;
    let mut actions = Vec::with_capacity(count);
    let mut offset = 5;
    for _ in 0..count {
        if offset + 4 > data.len() {
            return None;
        }
        let len = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
        offset += 4;
        if offset + len > data.len() {
            return None;
        }
        actions.push(&data[offset..offset + len]);
        offset += len;
    }
    Some(actions)
}

pub async fn run_swarm(
    swarm: Swarm<TorusBehaviour>,
    command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    tx_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    native_action_rx: mpsc::Receiver<Vec<u8>>,
    shared: Arc<SharedState>,
    local_key: VerifyingKey,
) {
    run_swarm_with_config(swarm, command_rx, tx_rx, native_action_rx, shared, local_key, &NetworkConfig::default()).await
}

pub async fn run_swarm_with_config(
    mut swarm: Swarm<TorusBehaviour>,
    mut command_rx: mpsc::UnboundedReceiver<NetworkCommand>,
    mut tx_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut native_action_rx: mpsc::Receiver<Vec<u8>>,
    shared: Arc<SharedState>,
    local_key: VerifyingKey,
    config: &NetworkConfig,
) {
    let consensus_topic = gossipsub::IdentTopic::new(CONSENSUS_TOPIC);
    let tx_topic = gossipsub::IdentTopic::new(TX_TOPIC);
    let native_action_topic = gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC);

    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&consensus_topic) {
        warn!("Failed to subscribe to consensus topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&tx_topic) {
        warn!("Failed to subscribe to tx topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&native_action_topic) {
        warn!("Failed to subscribe to native action topic: {e:?}");
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

    let mut mesh_interval = tokio::time::interval(std::time::Duration::from_secs(10));
    mesh_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut native_batch: Vec<Vec<u8>> = Vec::with_capacity(256);
    let mut batch_timer = tokio::time::interval(Duration::from_millis(NATIVE_BATCH_INTERVAL_MS));
    batch_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let action = tokio::select! {
            biased;

            // P1: consensus commands (outbound) — highest priority to prevent starvation
            cmd = command_rx.recv() => SwarmAction::Command(cmd.map(Box::new)),

            // P2: swarm events (consensus inbound, connections, gossip)
            event = swarm.select_next_some() => SwarmAction::Event(Box::new(event)),

            // P3: EVM tx publish
            tx = tx_rx.recv() => SwarmAction::Tx(tx),

            // P4: flush native batch on timer
            _ = batch_timer.tick(), if !native_batch.is_empty() => SwarmAction::FlushNativeBatch,

            // P5: buffer native actions (lowest priority)
            na = native_action_rx.recv() => SwarmAction::NativeAction(na),

            _ = cleanup_interval.tick() => {
                peer_scoring.cleanup_stale(stale_age);
                consensus_rate_limiter.cleanup_stale();
                continue;
            }
            _ = mesh_interval.tick() => {
                let local_pid = *swarm.local_peer_id();
                let peer_map = shared.peer_map.read().unwrap();
                let to_dial: Vec<PeerId> = peer_map.peer_ids()
                    .filter(|pid| **pid != local_pid && !swarm.is_connected(pid))
                    .copied()
                    .collect();
                drop(peer_map);
                if !to_dial.is_empty() {
                    let _ = swarm.behaviour_mut().kademlia.bootstrap();
                    for pid in &to_dial {
                        if let Err(e) = swarm.dial(*pid) {
                            debug!(%pid, %e, "mesh maintenance dial failed");
                        }
                    }
                    info!(missing = to_dial.len(), "mesh: dialing unconnected validators");
                }
                continue;
            }
        };

        match action {
            SwarmAction::Event(event) => {
                handle_event(
                    *event,
                    &mut swarm,
                    &shared,
                    &local_key,
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
                match swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(tx_topic.clone(), tx_bytes)
                {
                    Ok(_) => {
                        if let Some(ref m) = shared.metrics {
                            m.gossip_messages_sent.inc();
                        }
                    }
                    Err(e) => {
                        warn!("Failed to publish tx: {e:?}");
                    }
                }
            }
            SwarmAction::Tx(None) => {}
            SwarmAction::FlushNativeBatch => {
                if !native_batch.is_empty() {
                    let batch_bytes = serialize_native_batch(&native_batch);
                    let count = native_batch.len();
                    native_batch.clear();
                    match swarm
                        .behaviour_mut()
                        .gossipsub
                        .publish(native_action_topic.clone(), batch_bytes)
                    {
                        Ok(_) => {
                            debug!(count, "published native action batch to gossipsub");
                            if let Some(ref m) = shared.metrics {
                                m.gossip_messages_sent.inc();
                            }
                        }
                        Err(e) => {
                            warn!(count, "failed to publish native batch: {e:?}");
                        }
                    }
                }
            }
            SwarmAction::NativeAction(Some(action_bytes)) => {
                native_batch.push(action_bytes);
                if native_batch.len() >= NATIVE_BATCH_MAX_SIZE {
                    let batch_bytes = serialize_native_batch(&native_batch);
                    let count = native_batch.len();
                    native_batch.clear();
                    match swarm
                        .behaviour_mut()
                        .gossipsub
                        .publish(native_action_topic.clone(), batch_bytes)
                    {
                        Ok(_) => {
                            debug!(count, "published native action batch (size cap) to gossipsub");
                            if let Some(ref m) = shared.metrics {
                                m.gossip_messages_sent.inc();
                            }
                        }
                        Err(e) => {
                            warn!(count, "failed to publish native batch: {e:?}");
                        }
                    }
                }
            }
            SwarmAction::NativeAction(None) => {}
        }
    }
}

fn handle_event(
    event: SwarmEvent<TorusBehaviourEvent>,
    swarm: &mut Swarm<TorusBehaviour>,
    shared: &SharedState,
    local_key: &VerifyingKey,
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

            // Increment gossip received counter
            if let Some(ref m) = shared.metrics {
                m.gossip_messages_received.inc();
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

                // Rate-limit by cryptographic author, not forwarder — prevents
                // a banned node from consuming honest relayers' rate-limit tokens.
                if !consensus_rate_limiter.check_and_increment(&author) {
                    debug!(%author, "consensus rate limit exceeded, dropping message");
                    return;
                }
                handle_consensus_gossip(&message.data, shared, peer_scoring, &author);
            } else if message.topic == gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC).hash() {
                if message.data.len() > max_tx_msg_size {
                    warn!(
                        peer = %propagation_source,
                        size = message.data.len(),
                        "oversized native action message rejected"
                    );
                    peer_scoring.penalize(
                        &propagation_source,
                        PENALTY_INVALID_TX,
                        "oversized native action message",
                    );
                    return;
                }
                if let Some(actions) = deserialize_native_batch(&message.data) {
                    let count = actions.len();
                    let mut ok = 0usize;
                    for action_bytes in actions {
                        match bincode::deserialize::<(torus_types::Address, torus_types::SignedNativeAction)>(action_bytes) {
                            Ok(pair) => {
                                if let Some(ref tx) = shared.native_action_inbound {
                                    let _ = tx.send(pair);
                                }
                                ok += 1;
                            }
                            Err(e) => {
                                debug!(peer = %propagation_source, %e, "malformed action in batch");
                            }
                        }
                    }
                    debug!(count, ok, "received native action batch from {propagation_source}");
                } else {
                    match bincode::deserialize::<(torus_types::Address, torus_types::SignedNativeAction)>(&message.data) {
                        Ok(pair) => {
                            if let Some(ref tx) = shared.native_action_inbound {
                                let _ = tx.send(pair);
                            }
                            debug!("received single native action from {propagation_source}");
                        }
                        Err(e) => {
                            warn!(peer = %propagation_source, %e, "malformed native action gossip");
                            peer_scoring.penalize(
                                &propagation_source,
                                PENALTY_INVALID_TX,
                                "malformed native action",
                            );
                        }
                    }
                }
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
            if request.payload.first() == Some(&PRE_PROPOSAL_BATCH_MARKER) {
                let batch_bytes = &request.payload[1..];
                match bincode::deserialize::<Vec<(torus_types::Address, torus_types::SignedNativeAction)>>(batch_bytes) {
                    Ok(pairs) => {
                        let count = pairs.len();
                        if let Some(ref tx) = shared.native_action_inbound {
                            for pair in pairs {
                                let _ = tx.send(pair);
                            }
                        }
                        tracing::info!(count, %peer, "received pre-proposal action batch");
                    }
                    Err(e) => {
                        warn!(%peer, %e, "pre-proposal batch deserialization failed");
                    }
                }
            } else if request.payload.first() == Some(&FORWARD_ACTION_MARKER) {
                let action_bytes = &request.payload[1..];
                if action_bytes.len() > 20 {
                    let sender_addr = torus_types::Address::from_slice(&action_bytes[..20]);
                    if let Ok(action) = serde_json::from_slice::<torus_types::SignedNativeAction>(&action_bytes[20..]) {
                        if let Some(ref tx) = shared.native_action_inbound {
                            let _ = tx.send((sender_addr, action));
                        }
                    } else {
                        warn!(%peer, "forwarded native action: deserialization failed");
                    }
                }
            } else if let Ok(msg) =
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
        // Direct protocol response = delivery ack for a tracked consensus send.
        SwarmEvent::Behaviour(TorusBehaviourEvent::Direct(request_response::Event::Message {
            message: request_response::Message::Response { request_id, .. },
            ..
        })) => {
            shared.outbound_direct.lock().unwrap().remove(&request_id);
        }
        // Direct protocol outbound failure — previously swallowed by `_ => {}`
        // (Task 1 finding b). Re-enqueue the consensus message so the next
        // ConnectionEstablished flush re-delivers it; nudge a dial. Untracked
        // payloads (native push / forward) are logged only — T4 re-pushes those.
        SwarmEvent::Behaviour(TorusBehaviourEvent::Direct(
            request_response::Event::OutboundFailure { peer, request_id, error, .. }
        )) => {
            let tracked = shared.outbound_direct.lock().unwrap().remove(&request_id);
            if let Some((target, message)) = tracked {
                warn!(%peer, ?error, "direct send failed — re-enqueueing consensus message for reconnect flush");
                shared.pending_sends.lock().unwrap().enqueue(&target, message);
                if let Some(ref m) = shared.metrics {
                    m.pending_sends_enqueued.inc();
                }
                let _ = swarm.dial(peer);
            } else {
                warn!(%peer, ?error, "direct send failed (untracked payload)");
            }
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::Direct(
            request_response::Event::InboundFailure { peer, error, .. }
        )) => {
            warn!(%peer, ?error, "direct inbound failure");
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) => {
            if peer_scoring.is_banned(&peer_id) {
                return;
            }
            // Extract ed25519 key before consuming other fields.
            let maybe_vk = info.public_key.try_into_ed25519().ok()
                .and_then(|ed_pk| VerifyingKey::from_bytes(&ed_pk.to_bytes()).ok());
            for addr in info.listen_addrs {
                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
            }
            // Register non-validator peers (e.g. RPC nodes) so verify_sender_key
            // accepts their direct messages (block sync requests).
            if let Some(vk) = maybe_vk {
                let mut pm = shared.peer_map.write().unwrap();
                if !pm.contains_vk(&vk) {
                    pm.insert(vk, peer_id);
                    info!(%peer_id, "registered peer from identify exchange");
                }
            }
        }
        // Block-data fetch protocol: inbound request — serve from network-thread block store.
        SwarmEvent::Behaviour(TorusBehaviourEvent::BlockData(request_response::Event::Message {
            message: request_response::Message::Request { request, channel, .. },
            peer,
            ..
        })) => {
            if peer_scoring.is_banned(&peer) {
                let _ = swarm.behaviour_mut().block_data.send_response(
                    channel,
                    BlockDataNetResponse { view: request.view, payload: Vec::new() },
                );
                return;
            }
            let store = shared.block_store.read().unwrap();
            let store_len = store.len();
            let payload = store.get(&request.block_hash).cloned().unwrap_or_default();
            drop(store);
            if payload.is_empty() {
                warn!(
                    %peer,
                    view = request.view,
                    hash = ?request.block_hash,
                    store_size = store_len,
                    "block-data request: block NOT FOUND in store"
                );
            } else {
                info!(
                    %peer,
                    view = request.view,
                    payload_bytes = payload.len(),
                    "block-data request: serving block"
                );
            }
            if let Err(resp) = swarm.behaviour_mut().block_data.send_response(
                channel,
                BlockDataNetResponse { view: request.view, payload },
            ) {
                warn!(view = resp.view, "block-data send_response FAILED (channel dead)");
            }
        }
        // Block-data fetch protocol: inbound response — forward to algorithm thread.
        SwarmEvent::Behaviour(TorusBehaviourEvent::BlockData(request_response::Event::Message {
            message: request_response::Message::Response { response, .. },
            peer,
            ..
        })) => {
            if response.payload.is_empty() {
                warn!(%peer, "block-data response: empty payload (block not found on server)");
                return;
            }
            let sender_vk = shared.peer_map.read().unwrap().get_vk(&peer).copied();
            if let Some(vk) = sender_vk {
                info!(
                    %peer,
                    view = response.view,
                    payload_bytes = response.payload.len(),
                    "block-data response: queuing body for algorithm thread"
                );
                let mut queue = shared.block_data_inbound.lock().unwrap();
                if queue.len() < MAX_INBOUND_QUEUE {
                    queue.push_back((vk, response.view, response.payload));
                }
            } else {
                warn!(%peer, "block-data response from peer not in peer_map — dropping");
            }
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::BlockData(
            request_response::Event::OutboundFailure { peer, error, .. }
        )) => {
            warn!(%peer, ?error, "block-data OUTBOUND FAILURE");
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::BlockData(
            request_response::Event::InboundFailure { peer, error, .. }
        )) => {
            warn!(%peer, ?error, "block-data INBOUND FAILURE");
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::BlockData(_)) => {}
        // Native-DA fetch protocol: inbound request — serve bodies by-hash from the
        // durable DA store (Task 5). The RARE pull-fallback; push covers the common case.
        SwarmEvent::Behaviour(TorusBehaviourEvent::NativeDa(request_response::Event::Message {
            message: request_response::Message::Request { request, channel, .. },
            peer,
            ..
        })) => {
            let bodies = if peer_scoring.is_banned(&peer) {
                vec![Vec::new(); request.hashes.len()]
            } else {
                let store = shared.native_da.read().unwrap();
                serve_native_da_bodies(store.as_ref(), &request.hashes)
            };
            let found = bodies.iter().filter(|b| !b.is_empty()).count();
            debug!(%peer, requested = request.hashes.len(), found, "native-da request: serving bodies");
            if swarm
                .behaviour_mut()
                .native_da
                .send_response(channel, NativeDaNetResponse { bodies })
                .is_err()
            {
                warn!(%peer, "native-da send_response FAILED (channel dead)");
            }
        }
        // Native-DA fetch protocol: inbound response — queue bodies for the
        // consensus app to insert into its DA store and retry reconstruct (Task 6).
        // Responses are solicited (request_response only delivers for our own
        // outbound request), so no sender verification is needed.
        SwarmEvent::Behaviour(TorusBehaviourEvent::NativeDa(request_response::Event::Message {
            message: request_response::Message::Response { response, .. },
            peer,
            ..
        })) => {
            let mut queue = shared.native_da_inbound.lock().unwrap();
            let mut queued = 0usize;
            for body in response.bodies {
                if body.is_empty() {
                    continue; // not-found entry
                }
                if queue.len() >= MAX_INBOUND_QUEUE {
                    warn!(%peer, "native-da inbound queue full — dropping body");
                    break;
                }
                queue.push_back(body);
                queued += 1;
            }
            if queued > 0 {
                debug!(%peer, queued, "native-da response: queued bodies for app");
            }
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::NativeDa(
            request_response::Event::OutboundFailure { peer, error, .. },
        )) => {
            warn!(%peer, ?error, "native-da OUTBOUND FAILURE");
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::NativeDa(
            request_response::Event::InboundFailure { peer, error, .. },
        )) => {
            warn!(%peer, ?error, "native-da INBOUND FAILURE");
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::NativeDa(_)) => {}
        // FIX 6 (CONS-FIND-25-32): Sync protocol events.
        // TODO: When sync serving is implemented, handle sync requests in a
        // spawned task (tokio::spawn) to avoid blocking the consensus event loop.
        SwarmEvent::Behaviour(TorusBehaviourEvent::SyncProto(_)) => {
            debug!("sync protocol event (not yet handled)");
        }
        SwarmEvent::NewListenAddr { address, .. } => info!("Listening on {address}"),
        SwarmEvent::ConnectionEstablished { peer_id, num_established, .. } => {
            if peer_scoring.is_banned(&peer_id) {
                let _ = swarm.disconnect_peer_id(peer_id);
                debug!("Disconnected banned peer {peer_id}");
            } else {
                info!(%peer_id, %num_established, "peer connected");
                if let Some(ref m) = shared.metrics {
                    let count = swarm.connected_peers().count() as i64;
                    m.peers_connected.set(count);
                }
                let _ = swarm.behaviour_mut().kademlia.bootstrap();
                // Corrected reconnect seam (Task 1a): a validator stays mapped in
                // peer_map across disconnects, so ConnectionEstablished — not
                // RegisterPeer/Identify — is what fires when the link returns.
                // Flush any consensus messages buffered while it was unreachable.
                let reconnected_vk = shared.peer_map.read().unwrap().get_vk(&peer_id).copied();
                if let Some(vk) = reconnected_vk {
                    let pending = shared.pending_sends.lock().unwrap().flush(&vk);
                    if !pending.is_empty() {
                        info!(%peer_id, count = pending.len(), "flushing pending consensus sends on (re)connect");
                        for message in pending {
                            if let Some(ref m) = shared.metrics {
                                m.pending_sends_flushed.inc();
                            }
                            send_direct(swarm, shared, local_key, &vk, message);
                        }
                    }
                    // T7: deliver native-action pushes that were QUEUED while this
                    // validator was disconnected (targeted, in enqueue order) — so a
                    // push is never dropped just because its target was offline.
                    let queued_pushes = shared.pending_native_pushes.lock().unwrap().flush(&vk);
                    if !queued_pushes.is_empty() {
                        info!(%peer_id, count = queued_pushes.len(), "flushing queued native pushes on (re)connect");
                        for envelope in queued_pushes {
                            let req = DirectRequest {
                                sender_key: local_key.to_bytes(),
                                payload: envelope,
                            };
                            swarm.behaviour_mut().direct.send_request(&peer_id, req);
                        }
                    }
                    // T4: re-push recent native-action bundles so a (re)connecting
                    // validator's mempool catches up before the next CompactBlock it
                    // must reconstruct. Validator-gated; dedup is free on the receiver.
                    if shared.validators.read().unwrap().contains(&vk.to_bytes()) {
                        let bundles: Vec<Vec<u8>> =
                            shared.recent_native_bundles.lock().unwrap().iter().cloned().collect();
                        if !bundles.is_empty() {
                            info!(%peer_id, count = bundles.len(), "re-pushing recent native bundles on (re)connect");
                            for envelope in bundles {
                                let req = DirectRequest {
                                    sender_key: local_key.to_bytes(),
                                    payload: envelope,
                                };
                                swarm.behaviour_mut().direct.send_request(&peer_id, req);
                                if let Some(ref m) = shared.metrics {
                                    m.native_bundle_repushed.inc();
                                }
                            }
                        }
                    }
                }
            }
        }
        SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
            info!(%peer_id, ?cause, "peer disconnected");
            if let Some(ref m) = shared.metrics {
                let count = swarm.connected_peers().count() as i64;
                m.peers_connected.set(count);
            }
            // Clean up non-validator peers from peer_map to prevent unbounded growth.
            let validators = shared.validators.read().unwrap();
            let mut pm = shared.peer_map.write().unwrap();
            if let Some(vk) = pm.get_vk(&peer_id).copied() {
                if !validators.contains(&vk.to_bytes()) {
                    pm.remove_by_vk(&vk);
                    debug!(%peer_id, "removed non-validator peer from peer map");
                }
            }
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            warn!(?peer_id, %error, "outgoing connection failed");
        }
        SwarmEvent::IncomingConnectionError { error, .. } => {
            warn!(%error, "incoming connection failed");
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::Kademlia(
            kad::Event::OutboundQueryProgressed { result: kad::QueryResult::Bootstrap(Ok(_)), .. }
        )) => {
            let local_pid = *swarm.local_peer_id();
            let peer_map = shared.peer_map.read().unwrap();
            let to_dial: Vec<PeerId> = peer_map.peer_ids()
                .filter(|pid| **pid != local_pid && !swarm.is_connected(pid))
                .copied()
                .collect();
            drop(peer_map);
            for pid in to_dial {
                info!(%pid, "kademlia bootstrap: dialing validator");
                let _ = swarm.dial(pid);
            }
        }
        SwarmEvent::Behaviour(TorusBehaviourEvent::Kademlia(_)) => {}
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
                match swarm
                    .behaviour_mut()
                    .gossipsub
                    .publish(consensus_topic.clone(), envelope)
                {
                    Ok(_) => {
                        if let Some(ref m) = shared.metrics {
                            m.gossip_messages_sent.inc();
                        }
                    }
                    Err(e) => {
                        warn!("Failed to publish consensus message: {e:?}");
                    }
                }
            }
        }
        NetworkCommand::Send { target, message } => {
            send_direct(swarm, shared, local_key, &target, message);
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
        NetworkCommand::DialPeer { peer_id } => {
            if *swarm.local_peer_id() != peer_id && !swarm.is_connected(&peer_id) {
                if let Err(e) = swarm.dial(peer_id) {
                    debug!(%peer_id, "DialPeer failed: {e}");
                }
            }
        }
        NetworkCommand::BlockDataRequest { target, block_hash, view } => {
            let peer_id = shared.peer_map.read().unwrap().get_peer_id(&target).copied();
            if let Some(pid) = peer_id {
                let req = BlockDataNetRequest { block_hash, view };
                swarm.behaviour_mut().block_data.send_request(&pid, req);
            } else {
                warn!("BlockDataRequest target not in peer map — dropping");
            }
        }
        NetworkCommand::StoreBlock { hash, block_bytes } => {
            shared.block_store.write().unwrap().insert(hash, block_bytes);
        }
        NetworkCommand::ForwardNativeAction { target, payload } => {
            let peer_id = shared.peer_map.read().unwrap().get_peer_id(&target).copied();
            if let Some(pid) = peer_id {
                let mut envelope = vec![FORWARD_ACTION_MARKER];
                envelope.extend_from_slice(&payload);
                let req = DirectRequest {
                    sender_key: local_key.to_bytes(),
                    payload: envelope,
                };
                swarm.behaviour_mut().direct.send_request(&pid, req);
            } else {
                warn!("ForwardNativeAction: leader not in peer map");
            }
        }
        NetworkCommand::BroadcastNativeActions { payload } => {
            let mut envelope = vec![PRE_PROPOSAL_BATCH_MARKER];
            envelope.extend_from_slice(&payload);
            // Resolve every mapped peer (validators + RPC nodes that also reconstruct
            // bodies), tagging validator membership and skipping self.
            let targets: Vec<(VerifyingKey, PeerId, bool)> = {
                let validators = shared.validators.read().unwrap();
                let peer_map = shared.peer_map.read().unwrap();
                peer_map
                    .peer_ids()
                    .filter_map(|pid| {
                        let vk = *peer_map.get_vk(pid)?;
                        if vk == *local_key {
                            return None; // never push to self
                        }
                        let is_validator = validators.contains(&vk.to_bytes());
                        Some((vk, *pid, is_validator))
                    })
                    .collect()
            };
            // T7: send to every connected peer now; for a DISCONNECTED validator, QUEUE
            // the push so it is delivered on (re)connect instead of dropped under load
            // (and nudge a dial). The receiver mirrors bodies to its durable DA store
            // unconditionally (T2), so a delivered push keeps the pull-fallback rare.
            let mut sent = 0usize;
            let mut queued = 0usize;
            for (vk, pid, is_validator) in targets {
                if swarm.is_connected(&pid) {
                    let req = DirectRequest {
                        sender_key: local_key.to_bytes(),
                        payload: envelope.clone(),
                    };
                    swarm.behaviour_mut().direct.send_request(&pid, req);
                    sent += 1;
                } else if is_validator {
                    shared
                        .pending_native_pushes
                        .lock()
                        .unwrap()
                        .enqueue(&vk, envelope.clone());
                    let _ = swarm.dial(pid);
                    queued += 1;
                }
            }
            // T4: also retain this bundle (bounded ring) for a broad re-push to any
            // validator that (re)connects after this push. Dedup is free on the receiver.
            push_bounded(
                &mut shared.recent_native_bundles.lock().unwrap(),
                envelope,
                RECENT_NATIVE_BUNDLES_CAP,
            );
            tracing::info!(sent, queued, bytes = payload.len(), "broadcast pre-proposal actions to validators");
        }
        NetworkCommand::FetchNativeActions { target, hashes } => {
            let peer_id = shared.peer_map.read().unwrap().get_peer_id(&target).copied();
            if let Some(pid) = peer_id {
                let req = NativeDaNetRequest { hashes };
                swarm.behaviour_mut().native_da.send_request(&pid, req);
            } else {
                warn!("FetchNativeActions target not in peer map — dropping");
            }
        }
    }
}

/// Send a consensus message directly to `target`, buffering instead of dropping
/// when the peer is unreachable (Task 3 — corrected delivery seam).
///
/// A validator is in `peer_map` from genesis (`init_validator_set`), so the real
/// gap is connectivity, not registration: a send to a mapped-but-disconnected
/// peer races into a (previously silent) `OutboundFailure`. We instead enqueue +
/// nudge a dial, and the `ConnectionEstablished` arm flushes the queue. The send
/// is tracked in `outbound_direct` so a post-dial failure can also re-enqueue.
fn send_direct(
    swarm: &mut Swarm<TorusBehaviour>,
    shared: &SharedState,
    local_key: &VerifyingKey,
    target: &VerifyingKey,
    message: hotstuff_rs::networking::messages::Message,
) {
    if target == local_key {
        enqueue_inbound(&shared.inbound, *local_key, message);
        return;
    }
    let peer_id = shared.peer_map.read().unwrap().get_peer_id(target).copied();
    let pid = match peer_id {
        Some(pid) => pid,
        None => {
            // Not mapped yet (rare for validators) — buffer until registration.
            shared.pending_sends.lock().unwrap().enqueue(target, message);
            if let Some(ref m) = shared.metrics {
                m.pending_sends_enqueued.inc();
            }
            return;
        }
    };
    if !swarm.is_connected(&pid) {
        // Mapped but disconnected — buffer + nudge a dial; flush on reconnect.
        shared.pending_sends.lock().unwrap().enqueue(target, message);
        if let Some(ref m) = shared.metrics {
            m.pending_sends_enqueued.inc();
        }
        let _ = swarm.dial(pid);
        return;
    }
    if let Ok(payload) = message.try_to_vec() {
        let req = DirectRequest {
            sender_key: local_key.to_bytes(),
            payload,
        };
        let req_id = swarm.behaviour_mut().direct.send_request(&pid, req);
        shared
            .outbound_direct
            .lock()
            .unwrap()
            .insert(req_id, (*target, message));
        if let Some(ref m) = shared.metrics {
            m.gossip_messages_sent.inc();
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
            metrics: None,
            block_store: RwLock::new(HashMap::new()),
            block_data_inbound: Mutex::new(VecDeque::new()),
            native_action_inbound: None,
            pending_sends: Mutex::new(PendingSendQueue::new(256)),
            outbound_direct: Mutex::new(HashMap::new()),
            recent_native_bundles: Mutex::new(VecDeque::new()),
            native_da: RwLock::new(None),
            native_da_inbound: Mutex::new(VecDeque::new()),
            pending_native_pushes: Mutex::new(PendingSendQueue::new(8)),
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

    #[test]
    fn recent_native_bundles_ring_keeps_last_n() {
        let mut ring: VecDeque<Vec<u8>> = VecDeque::new();
        // Push CAP+2 items; ring must retain only the last CAP, oldest evicted.
        for i in 0..(RECENT_NATIVE_BUNDLES_CAP as u8 + 2) {
            push_bounded(&mut ring, vec![i], RECENT_NATIVE_BUNDLES_CAP);
        }
        assert_eq!(ring.len(), RECENT_NATIVE_BUNDLES_CAP);
        assert_eq!(ring.front().unwrap(), &vec![2u8]);
        assert_eq!(ring.back().unwrap(), &vec![RECENT_NATIVE_BUNDLES_CAP as u8 + 1]);
    }

    /// Task 5: the `/torus/native-da/1.0` serve path returns the stored body bytes
    /// for a known hash (in request order), an empty entry for an unknown hash, and
    /// all-empty when no DA store is attached (tests/observers).
    #[test]
    fn serve_native_da_returns_known_and_skips_unknown() {
        use torus_state::db::StateDb;
        use torus_state::NativeDaStore;
        use torus_types::{
            compute_action_hash, ActionSignature, NativeAction, Signature, SignedNativeAction,
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let db = StateDb::open(dir.path()).expect("open db");
        let store = NativeDaStore::new(db);

        let action = SignedNativeAction {
            action: NativeAction::ClaimRewards,
            nonce: 7,
            signature: ActionSignature::Eip712(Signature { v: 27, r: [0u8; 32], s: [0u8; 32] }),
        };
        let known: [u8; 32] = compute_action_hash(&action).0;
        let unknown = [9u8; 32];
        store.put(&action).expect("put");

        // Known + unknown, order preserved: bodies[0] is the stored body, bodies[1] empty.
        let bodies = serve_native_da_bodies(Some(&store), &[known, unknown]);
        assert_eq!(bodies.len(), 2);
        assert!(!bodies[0].is_empty(), "known hash served");
        assert!(bodies[1].is_empty(), "unknown hash -> empty (not found)");
        let got: SignedNativeAction =
            bincode::deserialize(&bodies[0]).expect("served bytes deserialize");
        assert_eq!(compute_action_hash(&got).0, known, "served body round-trips to its hash");

        // No store attached -> one empty entry per requested hash.
        let none = serve_native_da_bodies(None, &[known, unknown]);
        assert_eq!(none, vec![Vec::<u8>::new(), Vec::<u8>::new()]);
    }

    /// Task 7: a native-action push to a validator that is not yet connected is
    /// QUEUED (not dropped) and delivered when the peer (re)connects. Exercises the
    /// `pending_native_pushes` lifecycle that `BroadcastNativeActions` (enqueue on
    /// disconnect) and `ConnectionEstablished` (flush on connect) rely on.
    #[test]
    fn push_queues_until_peer_connected() {
        let shared = test_shared();
        let vk = test_vk(7);
        let env1 = vec![PRE_PROPOSAL_BATCH_MARKER, 1, 2, 3];
        let env2 = vec![PRE_PROPOSAL_BATCH_MARKER, 4, 5, 6];

        // Two pushes while the target is disconnected -> both queued, none dropped.
        shared.pending_native_pushes.lock().unwrap().enqueue(&vk, env1.clone());
        shared.pending_native_pushes.lock().unwrap().enqueue(&vk, env2.clone());

        // On (re)connect -> flushed in enqueue order, then the queue is cleared.
        let flushed = shared.pending_native_pushes.lock().unwrap().flush(&vk);
        assert_eq!(flushed, vec![env1, env2], "queued pushes delivered in order on connect");
        assert!(
            shared.pending_native_pushes.lock().unwrap().flush(&vk).is_empty(),
            "queue cleared after flush"
        );

        // A different validator's queue is independent (no cross-delivery).
        assert!(shared.pending_native_pushes.lock().unwrap().flush(&test_vk(8)).is_empty());
    }
}
