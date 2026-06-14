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

use crate::behaviour::{TorusBehaviour, TorusBehaviourEvent, CONSENSUS_TOPIC, NATIVE_ACTION_TOPIC, NATIVE_ACTION_TOPIC_V2, TX_TOPIC};
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
    /// Forward a raw EVM transaction directly to the leader node (Option B — EVM tx
    /// dissemination). Payload is raw RLP; the leader full-validates via `add_evm_tx`.
    ForwardEvmTx {
        target: VerifyingKey,
        payload: Vec<u8>,
    },
    /// Proposer pushes batched actions to all validators via req/res before CompactBlock proposal.
    BroadcastNativeActions {
        payload: Vec<u8>,
    },
    /// Proposer pushes only the action HASHES (a tiny manifest) when the body set is too
    /// big to disseminate within the view; validators PULL the bodies by-hash (pre-warm).
    /// Phase 2.3 (#5) — un-wedges bs≈500 (VIEW TIMEOUT on big-body dissemination).
    BroadcastNativeActionHashes {
        hashes: Vec<[u8; 32]>,
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
/// Marker byte for a HASH-ONLY pre-proposal manifest (`bincode(Vec<[u8;32]>)`): the body
/// set was too big to disseminate within the view, so the proposer pushes only the hashes
/// and validators PULL the bodies (pre-warm). Phase 2.3 (#5) — un-wedges bs≈500.
const PRE_PROPOSAL_HASHES_MARKER: u8 = 0xFC;
/// Marker byte prefixed to forwarded EVM transactions in DirectRequest (Option B — EVM tx
/// direct-to-leader dissemination). Distinct from the native/pre-proposal markers so the
/// receive path routes the payload to `add_evm_tx`, never the native pool.
const FORWARD_EVM_MARKER: u8 = 0xFB;
/// How many recent pre-proposal bundles to retain for re-push on (re)connect (Task 4).
const RECENT_NATIVE_BUNDLES_CAP: usize = 3;

/// Wrap a raw RLP EVM transaction in a DirectRequest forward envelope (marker + body).
fn encode_forwarded_evm(raw_rlp: &[u8]) -> Vec<u8> {
    let mut envelope = Vec::with_capacity(1 + raw_rlp.len());
    envelope.push(FORWARD_EVM_MARKER);
    envelope.extend_from_slice(raw_rlp);
    envelope
}

/// Strip the EVM forward marker, returning the raw RLP body. Returns `None` unless the payload
/// starts with `FORWARD_EVM_MARKER` and carries a non-empty body — so a native / pre-proposal
/// envelope (different marker) or a marker-only frame never routes to `add_evm_tx`.
fn parse_forwarded_evm_tx(payload: &[u8]) -> Option<&[u8]> {
    match payload.split_first() {
        Some((&FORWARD_EVM_MARKER, rest)) if !rest.is_empty() => Some(rest),
        _ => None,
    }
}

/// Push `item` into a bounded ring, evicting the oldest when at `cap`.
fn push_bounded(ring: &mut VecDeque<Vec<u8>>, item: Vec<u8>, cap: usize) {
    if ring.len() >= cap {
        ring.pop_front();
    }
    ring.push_back(item);
}

/// Max concurrent in-flight native-action push `send_request`s (#4 Task 3). Bounds the
/// push loop so N validators × bursty pre-proposal batches can't exhaust quinn's raised
/// per-connection bidi-stream window (`NATIVE_DA_STREAM_LIMIT`=512, Task 2) — the
/// `max sub-streams reached` storm that helped wedge bs=1000. Kept well below the window
/// so consensus sends (sharing `/torus/direct`) keep stream headroom.
const PUSH_MAX_INFLIGHT: usize = 32;

/// Max native-action push envelopes queued behind the in-flight cap (#4 Task 3). Bounds
/// memory if a link saturates; overflow drops the OLDEST (also retained in
/// `recent_native_bundles` for re-push on reconnect, so a drop is recoverable).
const PUSH_QUEUE_CAP: usize = 512;

/// Bounded-in-flight scheduler for native-action pushes (#4 Task 3). Caps concurrent
/// direct `send_request`s to `max_inflight` and queues the overflow (bounded by
/// `queue_cap`, drop-oldest), dispatching the next queued push only as an in-flight one
/// completes (Direct `Response`/`OutboundFailure`). Generic over the request-id type so
/// the policy is unit-testable without a live swarm.
pub struct PushScheduler<Id: std::hash::Hash + Eq + Copy> {
    inflight: HashSet<Id>,
    queue: VecDeque<(PeerId, Vec<u8>)>,
    max_inflight: usize,
    queue_cap: usize,
}

impl PushScheduler<request_response::OutboundRequestId> {
    /// Construct the production push scheduler with the configured caps (#4 Task 3).
    pub(crate) fn for_pushes() -> Self {
        Self::new(PUSH_MAX_INFLIGHT, PUSH_QUEUE_CAP)
    }
}

impl<Id: std::hash::Hash + Eq + Copy> PushScheduler<Id> {
    fn new(max_inflight: usize, queue_cap: usize) -> Self {
        Self {
            inflight: HashSet::new(),
            queue: VecDeque::new(),
            max_inflight,
            queue_cap,
        }
    }

    /// May another push go out now without exceeding the in-flight cap?
    fn has_capacity(&self) -> bool {
        self.inflight.len() < self.max_inflight
    }

    /// Record a freshly dispatched push as in-flight (keyed by its request id).
    fn record(&mut self, id: Id) {
        self.inflight.insert(id);
    }

    /// Queue a push for later dispatch (the cap was reached). Bounded: when full, drops
    /// the OLDEST queued push and returns `true` so the caller can log the drop.
    fn enqueue(&mut self, pid: PeerId, envelope: Vec<u8>) -> bool {
        let dropped = self.queue.len() >= self.queue_cap && self.queue.pop_front().is_some();
        self.queue.push_back((pid, envelope));
        dropped
    }

    /// A tracked Direct send completed (`Response`/`OutboundFailure`). If `id` was one of
    /// our in-flight pushes, free its slot and return the next queued push to dispatch (if
    /// any); the caller sends it and calls [`Self::record`] with the new id. Returns `None`
    /// for an untracked id (a consensus/forward send on the shared protocol) or an empty
    /// queue.
    fn complete(&mut self, id: Id) -> Option<(PeerId, Vec<u8>)> {
        if self.inflight.remove(&id) {
            self.queue.pop_front()
        } else {
            None
        }
    }

    #[cfg(test)]
    fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    #[cfg(test)]
    fn queued_len(&self) -> usize {
        self.queue.len()
    }
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
    /// Inbound forwarded EVM transactions (raw RLP) received on the leader from a peer's
    /// direct-to-leader forward (Option B). Consumed by the node's ingest task → `add_evm_tx`.
    pub evm_tx_inbound: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
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
    /// Bounded-in-flight scheduler for native-action pushes (#4 Task 3): caps concurrent
    /// `direct.send_request` pushes to `PUSH_MAX_INFLIGHT` so the push loop can't exhaust
    /// quinn's bidi-stream window (the `max sub-streams reached` storm). Overflow is queued
    /// (bounded) and dispatched as in-flight pushes complete (Direct `Response` /
    /// `OutboundFailure`).
    pub push_scheduler: Mutex<PushScheduler<request_response::OutboundRequestId>>,
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
/// Serialized overhead of an empty native batch: marker (1) + action count (4).
const NATIVE_BATCH_HEADER_BYTES: usize = 5;
/// Per-action serialized overhead inside a batch: the u32 length prefix.
const NATIVE_BATCH_ENTRY_OVERHEAD: usize = 4;

/// Whether appending an action of `action_len` bytes to a batch currently measuring
/// `batch_bytes` on the wire would exceed `max_msg_size` — the receivers' hard-reject
/// cap (`config.max_tx_message_size`). Receivers reject strictly-greater sizes AND
/// penalize the forwarding peer, so the publisher must flush before crossing it
/// (s339: count-only batching shipped 160–595KB messages that banned friend2).
fn batch_would_exceed(batch_bytes: usize, action_len: usize, max_msg_size: usize) -> bool {
    batch_bytes + NATIVE_BATCH_ENTRY_OVERHEAD + action_len > max_msg_size
}

/// Whether `peer` is a member of the CURRENT consensus validator set.
///
/// Validator-set peers are exempt from ban-driven refusal on consensus-critical
/// paths: severing a validator's votes/proposals/pacemaker messages (or refusing
/// it DA/block-data service) turns a tx-layer penalty into a cluster liveness
/// fault — s339: a 1h ban of one validator for relaying oversized batches stalled
/// commits cluster-wide until expiry. Abuse from a validator stays bounded by the
/// per-author rate limiters, which apply regardless of ban state.
fn is_validator_peer(shared: &SharedState, peer: &PeerId) -> bool {
    let peer_map = shared.peer_map.read().unwrap();
    match peer_map.get_vk(peer) {
        Some(vk) => shared.validators.read().unwrap().contains(&vk.to_bytes()),
        None => false,
    }
}

/// Ban gate for gossip processing: drop only if the peer is banned AND not a
/// current validator (see [`is_validator_peer`]).
fn should_drop_banned_gossip(
    scoring: &mut PeerScoring,
    shared: &SharedState,
    peer: &PeerId,
) -> bool {
    scoring.is_banned(peer) && !is_validator_peer(shared, peer)
}

/// Penalize the cryptographic AUTHOR of an invalid gossip message — never the
/// last-hop forwarder. Gossip meshes relay other nodes' messages, so the
/// `propagation_source` is routinely an honest peer (s339: forwarder penalties
/// banned a validator for relaying another node's oversized batches). With no
/// author in the envelope, nobody is penalized. Returns the penalized peer.
fn penalize_gossip_author(
    scoring: &mut PeerScoring,
    author: Option<PeerId>,
    amount: i64,
    reason: &str,
) -> Option<PeerId> {
    if let Some(ref a) = author {
        scoring.penalize(a, amount, reason);
    }
    author
}

/// Publish the pending native-action batch to gossipsub and reset the accumulator.
/// `trigger` names which bound flushed it (timer / byte budget / count cap) for logs.
fn publish_native_batch(
    swarm: &mut libp2p::Swarm<TorusBehaviour>,
    shared: &Arc<SharedState>,
    topic: &gossipsub::IdentTopic,
    batch: &mut Vec<Vec<u8>>,
    batch_bytes: &mut usize,
    trigger: &str,
    zstd: bool,
) {
    if batch.is_empty() {
        return;
    }
    let payload = serialize_native_batch(batch);
    // Sprint 5: behind --gossip-zstd the batch ships zstd-framed on the v2
    // topic. On a compress failure keep the batch for the next flush — never
    // ship raw bytes on the v2 topic, v2 receivers inflate unconditionally.
    let payload = if zstd {
        match zstd::bulk::compress(&payload, crate::codec::ZSTD_WIRE_LEVEL) {
            Ok(c) => {
                crate::codec::record_wire_compression(
                    crate::codec::WirePath::GossipNative,
                    payload.len(),
                    c.len(),
                );
                c
            }
            Err(e) => {
                warn!(%e, "zstd gossip compress failed; batch kept for retry");
                return;
            }
        }
    } else {
        payload
    };
    let count = batch.len();
    batch.clear();
    *batch_bytes = NATIVE_BATCH_HEADER_BYTES;
    match swarm.behaviour_mut().gossipsub.publish(topic.clone(), payload) {
        Ok(_) => {
            debug!(count, trigger, "published native action batch to gossipsub");
            if let Some(ref m) = shared.metrics {
                m.gossip_messages_sent.inc();
                m.native_gossip_published_actions.inc_by(count as u64);
            }
        }
        Err(e) => {
            warn!(count, trigger, "failed to publish native batch: {e:?}");
        }
    }
}

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
    let native_action_topic_v1 = gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC);
    let native_action_topic_v2 = gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC_V2);
    // Sprint 5: receive BOTH topic versions unconditionally; PUBLISH v2 (zstd)
    // only behind --gossip-zstd — gossipsub cannot negotiate per-peer the way
    // request_response does, so the flip waits for all validators on 2.0.
    let gossip_zstd = config.gossip_zstd;
    let native_action_topic = if gossip_zstd {
        native_action_topic_v2.clone()
    } else {
        native_action_topic_v1.clone()
    };

    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&consensus_topic) {
        warn!("Failed to subscribe to consensus topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&tx_topic) {
        warn!("Failed to subscribe to tx topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&native_action_topic_v1) {
        warn!("Failed to subscribe to native action topic: {e:?}");
    }
    if let Err(e) = swarm.behaviour_mut().gossipsub.subscribe(&native_action_topic_v2) {
        warn!("Failed to subscribe to native action v2 topic: {e:?}");
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
    // Running on-wire size of `native_batch` (header + length-prefixed entries),
    // kept in lockstep so the byte-budget flush never crosses `max_tx_msg_size`.
    let mut native_batch_bytes: usize = NATIVE_BATCH_HEADER_BYTES;
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
                publish_native_batch(
                    &mut swarm,
                    &shared,
                    &native_action_topic,
                    &mut native_batch,
                    &mut native_batch_bytes,
                    "timer",
                    gossip_zstd,
                );
            }
            SwarmAction::NativeAction(Some(action_bytes)) => {
                let entry_bytes = NATIVE_BATCH_ENTRY_OVERHEAD + action_bytes.len();
                if NATIVE_BATCH_HEADER_BYTES + entry_bytes > max_tx_msg_size {
                    // Can never be delivered: receivers hard-reject > max_tx_msg_size
                    // and penalize the forwarder (s339: relayed oversized batches got a
                    // validator banned). The pre-proposal push / DA pull path still
                    // carries the action to inclusion when its holder leads.
                    warn!(
                        size = NATIVE_BATCH_HEADER_BYTES + entry_bytes,
                        cap = max_tx_msg_size,
                        "native action exceeds gossip cap — dropped from pre-spread"
                    );
                    if let Some(ref m) = shared.metrics {
                        m.native_gossip_dropped_oversized.inc();
                    }
                } else {
                    if batch_would_exceed(native_batch_bytes, action_bytes.len(), max_tx_msg_size) {
                        publish_native_batch(
                            &mut swarm,
                            &shared,
                            &native_action_topic,
                            &mut native_batch,
                            &mut native_batch_bytes,
                            "byte budget",
                            gossip_zstd,
                        );
                    }
                    native_batch_bytes += entry_bytes;
                    native_batch.push(action_bytes);
                    if native_batch.len() >= NATIVE_BATCH_MAX_SIZE {
                        publish_native_batch(
                            &mut swarm,
                            &shared,
                            &native_action_topic,
                            &mut native_batch,
                            &mut native_batch_bytes,
                            "count cap",
                            gossip_zstd,
                        );
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
            // Ban gate by last-hop forwarder — validator-set peers exempt (s350
            // FIX B): a banned validator's relayed traffic still carries the
            // cluster's votes/proposals, and dropping it stalls commits.
            if should_drop_banned_gossip(&mut *peer_scoring, shared, &propagation_source) {
                return;
            }

            // Increment gossip received counter
            if let Some(ref m) = shared.metrics {
                m.gossip_messages_received.inc();
            }

            let consensus_hash = gossipsub::IdentTopic::new(CONSENSUS_TOPIC).hash();
            if message.topic == consensus_hash {
                // Message size validation (Phase 3: 3.1.7). Penalty goes to the
                // AUTHOR — the forwarder merely relayed it (s350 FIX B).
                if message.data.len() > max_consensus_msg_size {
                    warn!(
                        peer = %propagation_source,
                        author = ?message.source,
                        size = message.data.len(),
                        "oversized consensus message rejected"
                    );
                    penalize_gossip_author(
                        peer_scoring,
                        message.source,
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
                // Validator-set authors keep consensus service even while banned
                // (s350 FIX B) — quorum needs their votes; rate limits still apply.
                if should_drop_banned_gossip(&mut *peer_scoring, shared, &author) {
                    return;
                }

                // Rate-limit by cryptographic author, not forwarder — prevents
                // a banned node from consuming honest relayers' rate-limit tokens.
                if !consensus_rate_limiter.check_and_increment(&author) {
                    debug!(%author, "consensus rate limit exceeded, dropping message");
                    return;
                }
                handle_consensus_gossip(&message.data, shared, peer_scoring, &author);
            } else if message.topic == gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC).hash()
                || message.topic == gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC_V2).hash()
            {
                if message.data.len() > max_tx_msg_size {
                    // Penalty goes to the AUTHOR, never the relayer — forwarder
                    // penalties on this exact path banned friend2 (validator) for
                    // an hour and stalled commits cluster-wide (s339 root cause).
                    warn!(
                        peer = %propagation_source,
                        author = ?message.source,
                        size = message.data.len(),
                        "oversized native action message rejected"
                    );
                    penalize_gossip_author(
                        peer_scoring,
                        message.source,
                        PENALTY_INVALID_TX,
                        "oversized native action message",
                    );
                    return;
                }
                // Sprint 5: the v2 topic carries a zstd frame of the identical
                // batch bytes; inflate bounded by the same receiver cap before
                // parsing. Undecodable frames penalize the AUTHOR (never the
                // forwarder — the s339/s350 ban-the-relayer lesson).
                let v2_buf;
                let payload: &[u8] = if message.topic
                    == gossipsub::IdentTopic::new(NATIVE_ACTION_TOPIC_V2).hash()
                {
                    match zstd::bulk::decompress(&message.data, max_tx_msg_size) {
                        Ok(d) => {
                            v2_buf = d;
                            &v2_buf
                        }
                        Err(e) => {
                            warn!(
                                peer = %propagation_source,
                                author = ?message.source,
                                %e,
                                "undecodable zstd native batch rejected"
                            );
                            penalize_gossip_author(
                                peer_scoring,
                                message.source,
                                PENALTY_INVALID_TX,
                                "undecodable zstd native batch",
                            );
                            return;
                        }
                    }
                } else {
                    &message.data
                };
                if let Some(actions) = deserialize_native_batch(payload) {
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
                    if let Some(ref m) = shared.metrics {
                        m.native_gossip_received_actions.inc_by(ok as u64);
                    }
                } else {
                    match bincode::deserialize::<(torus_types::Address, torus_types::SignedNativeAction)>(payload) {
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
            // Ban gate — validator-set peers exempt (s350 FIX B): /torus/direct
            // carries consensus unicasts; refusing a validator stalls quorum.
            if should_drop_banned_gossip(&mut *peer_scoring, shared, &peer) {
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
            } else if request.payload.first() == Some(&PRE_PROPOSAL_HASHES_MARKER) {
                // Phase 2.3 (#5): the proposer pushed only the HASHES (the body set was too
                // big to disseminate within the view). PRE-WARM the bodies by pulling them
                // by-hash from the proposer (this verified `peer` mirrored them in
                // produce_block) BEFORE its CompactBlock arrives, so the hot validate path
                // finds them already absorbed instead of wedging on dissemination.
                match plan_prewarm_requests(&request.payload[1..]) {
                    Some(chunks) => {
                        let count: usize = chunks.iter().map(|c| c.len()).sum();
                        for hashes in chunks {
                            swarm
                                .behaviour_mut()
                                .native_da
                                .send_request(&peer, NativeDaNetRequest { hashes });
                        }
                        tracing::info!(count, %peer, "pre-proposal HASH manifest -> pre-warm pull from proposer");
                    }
                    None => warn!(%peer, "pre-proposal hash manifest decode failed -- no pre-warm"),
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
            } else if let Some(raw_rlp) = parse_forwarded_evm_tx(&request.payload) {
                // Option B: a peer forwarded a raw EVM tx to us as (current) leader. Hand it to
                // the ingest task → add_evm_tx, which full-validates (sig/nonce/balance/gas).
                if let Some(ref tx) = shared.evm_tx_inbound {
                    let _ = tx.send(raw_rlp.to_vec());
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
            // #4 Task 3: if this acked a native push, free its in-flight slot and
            // dispatch the next queued push (no-op for consensus/forward sends).
            dispatch_next_push(swarm, shared, local_key, request_id);
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
                if let Some(ref m) = shared.metrics {
                    m.direct_send_failures_untracked.inc();
                }
            }
            // #4 Task 3: a FAILED native push still frees its in-flight slot — dispatch
            // the next queued push so a saturated link drains instead of stalling (no-op
            // for consensus sends, already handled above). The failed body is recoverable
            // via T4 re-push + the hot-path pull (#4 Task 1).
            dispatch_next_push(swarm, shared, local_key, request_id);
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
            // Validator-set peers exempt (s350 FIX B): identify keeps the peer
            // map fresh, which the validator exemption itself depends on.
            if should_drop_banned_gossip(&mut *peer_scoring, shared, &peer_id) {
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
            // Validator-set peers exempt (s350 FIX B): refusing block-data serve
            // to a validator starves its sync and breaks cluster liveness.
            if should_drop_banned_gossip(&mut *peer_scoring, shared, &peer) {
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
            // Validator-set peers exempt (s350 FIX B): empty DA responses to a
            // validator make its pull-fallback fail and veto valid proposals
            // (s339: val1's 16 unrecovered pulls each broke a view's quorum).
            let bodies = if should_drop_banned_gossip(&mut *peer_scoring, shared, &peer) {
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
            if let Some(ref m) = shared.metrics {
                m.native_da_pull_failures.inc();
            }
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
            // Validator-set peers exempt (s350 FIX B): never refuse a quorum
            // member's connection — a banned validator that reconnects mid-ban
            // would otherwise be severed entirely until expiry.
            if should_drop_banned_gossip(&mut *peer_scoring, shared, &peer_id) {
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
        NetworkCommand::ForwardEvmTx { target, payload } => {
            let peer_id = shared.peer_map.read().unwrap().get_peer_id(&target).copied();
            if let Some(pid) = peer_id {
                let req = DirectRequest {
                    sender_key: local_key.to_bytes(),
                    payload: encode_forwarded_evm(&payload),
                };
                swarm.behaviour_mut().direct.send_request(&pid, req);
            } else {
                warn!("ForwardEvmTx: leader not in peer map");
            }
        }
        NetworkCommand::BroadcastNativeActions { payload } => {
            // Full-body push (the fast common case for small batches): wrap the bodies in
            // the batch marker and fan them out. The receiver mirrors bodies to its durable
            // DA store, keeping the hot-path pull rare.
            let mut envelope = vec![PRE_PROPOSAL_BATCH_MARKER];
            envelope.extend_from_slice(&payload);
            fan_native_push(swarm, shared, local_key, envelope, true);
        }
        NetworkCommand::BroadcastNativeActionHashes { hashes } => {
            // Phase 2.3 (#5): the body set was too big to disseminate within the view, so
            // push only a tiny HASH manifest — validators PULL the bodies (pre-warm) the
            // moment they see it, off the view's critical path. Same PushScheduler-bounded
            // fan + re-push ring as the body push (the envelope is just far smaller, so it
            // never approaches the /torus/direct cap and always lands within the view).
            match bincode::serialize(&hashes) {
                Ok(body) => {
                    let mut envelope = vec![PRE_PROPOSAL_HASHES_MARKER];
                    envelope.extend_from_slice(&body);
                    tracing::info!(
                        count = hashes.len(),
                        "pre-proposal HASH-ONLY manifest push (bodies pulled by peers)"
                    );
                    fan_native_push(swarm, shared, local_key, envelope, false);
                }
                Err(e) => tracing::warn!(%e, "hash manifest serialize failed -- skipping push"),
            }
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

/// Defensive cap on a pre-warm manifest's hash count (review F6): a legitimate block holds
/// far fewer native actions than this, so a larger manifest from a (mapped) peer is rejected
/// rather than fanned into thousands of NativeDa sub-stream opens. The manifest body is
/// already bounded by the 4 MB `/torus/direct` codec; this bounds the pre-warm FAN itself.
const MAX_PREWARM_HASHES: usize = 100_000;

/// Decode a `PRE_PROPOSAL_HASHES_MARKER` manifest body (`bincode(Vec<[u8;32]>)`) into
/// per-request hash chunks (≤ `NATIVE_DA_FETCH_CHUNK`) for the pre-warm pull (Phase 2.3 #5).
/// `None` on a malformed, empty, or over-cap manifest — nothing is pulled, so the node falls
/// back to the hot-path pull when the CompactBlock arrives.
fn plan_prewarm_requests(body: &[u8]) -> Option<Vec<Vec<[u8; 32]>>> {
    let hashes: Vec<[u8; 32]> = bincode::deserialize(body).ok()?;
    if hashes.is_empty() || hashes.len() > MAX_PREWARM_HASHES {
        return None;
    }
    Some(
        hashes
            .chunks(crate::bridge::NATIVE_DA_FETCH_CHUNK)
            .map(|c| c.to_vec())
            .collect(),
    )
}

/// Fan a pre-proposal native-DA push `envelope` (already marker-prefixed) out to every
/// connected mapped peer, bounded by the PushScheduler (#4 Task 3) and queued for a
/// disconnected validator (T7), then retain it in `recent_native_bundles` for re-push on
/// (re)connect (T4). Shared by the full-body push (`BroadcastNativeActions`) and the
/// hash-only manifest push (`BroadcastNativeActionHashes`, Phase 2.3 #5) — only the
/// envelope contents differ, so the bounded-fan + re-push machinery is identical.
fn fan_native_push(
    swarm: &mut Swarm<TorusBehaviour>,
    shared: &SharedState,
    local_key: &VerifyingKey,
    envelope: Vec<u8>,
    retain_for_repush: bool,
) {
    let bytes = envelope.len();
    // Resolve every mapped peer (validators + RPC nodes that also reconstruct bodies),
    // tagging validator membership and skipping self.
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
    // T7: send to every connected peer now; for a DISCONNECTED validator, QUEUE the push so
    // it is delivered on (re)connect instead of dropped under load (and nudge a dial).
    //
    // #4 Task 3: bound concurrent IN-FLIGHT pushes via the PushScheduler so this loop can't
    // exhaust quinn's per-connection bidi-stream window (the `max sub-streams reached`
    // storm). Beyond the cap, queue the push (bounded); it is dispatched as in-flight pushes
    // complete (see `dispatch_next_push`). Disconnected-validator T7 queuing is unchanged.
    let mut sent = 0usize;
    let mut queued = 0usize;
    let mut backpressured = 0usize;
    let mut dropped = 0usize;
    for (vk, pid, is_validator) in targets {
        if swarm.is_connected(&pid) {
            let mut sched = shared.push_scheduler.lock().unwrap();
            if sched.has_capacity() {
                let req = DirectRequest {
                    sender_key: local_key.to_bytes(),
                    payload: envelope.clone(),
                };
                let id = swarm.behaviour_mut().direct.send_request(&pid, req);
                sched.record(id);
                sent += 1;
            } else {
                if sched.enqueue(pid, envelope.clone()) {
                    dropped += 1;
                }
                backpressured += 1;
            }
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
    // T4: retain FULL-BODY bundles (bounded ring) for a broad re-push to any validator that
    // (re)connects after this push — a re-pushed body seeds the reconnecting node's DA store
    // directly. A HASH manifest is a pre-warm tied to a SPECIFIC upcoming proposal: re-pushing
    // it after reconnect only fires spurious pulls for bodies the node will get via normal
    // block processing / sync, so manifests are NOT retained (review F1).
    if retain_for_repush {
        push_bounded(
            &mut shared.recent_native_bundles.lock().unwrap(),
            envelope,
            RECENT_NATIVE_BUNDLES_CAP,
        );
    }
    tracing::info!(
        sent,
        queued,
        backpressured,
        dropped,
        bytes,
        "broadcast pre-proposal actions to validators"
    );
    if dropped > 0 {
        // Never silent: a saturated push link dropped the oldest queued pushes. Recoverable
        // — the bundle is retained in `recent_native_bundles` and re-pushed when the
        // validator (re)connects, and the hot-path pull (#4 Task 1) recovers a still-missing
        // body.
        tracing::warn!(dropped, "native push queue saturated -- oldest pushes dropped (recoverable via re-push + hot pull)");
    }
}

/// On a completed Direct send (`Response`/`OutboundFailure`), if it was a tracked native
/// push, free its in-flight slot and dispatch the next queued push (#4 Task 3 — bounded
/// backpressure). No-op for consensus/forward sends, which the scheduler does not track.
///
/// The queue drains monotonically: every completion (ack OR failure) pops at most one
/// queued push, so even a saturated/failing link empties the queue rather than growing it.
/// A redispatch to a now-disconnected peer simply fails and drains the next; its body is
/// recoverable via the T4 re-push and the hot-path pull (#4 Task 1).
fn dispatch_next_push(
    swarm: &mut Swarm<TorusBehaviour>,
    shared: &SharedState,
    local_key: &VerifyingKey,
    completed: request_response::OutboundRequestId,
) {
    let next = shared.push_scheduler.lock().unwrap().complete(completed);
    if let Some((pid, payload)) = next {
        let req = DirectRequest {
            sender_key: local_key.to_bytes(),
            payload,
        };
        let id = swarm.behaviour_mut().direct.send_request(&pid, req);
        shared.push_scheduler.lock().unwrap().record(id);
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
    use crate::peer_scoring::INITIAL_SCORE;

    /// Phase 2.3 (#5, RED first): a hash-only manifest body (`bincode(Vec<[u8;32]>)`)
    /// decodes into per-request hash chunks (≤ `NATIVE_DA_FETCH_CHUNK`) for the pre-warm
    /// pull, covering every hash once in order; a malformed or empty manifest yields `None`
    /// (no pull fired). MUST fail before Task 3 (no `plan_prewarm_requests`).
    #[test]
    fn prewarm_requests_chunk_the_manifest() {
        let chunk = crate::bridge::NATIVE_DA_FETCH_CHUNK;
        let hashes: Vec<[u8; 32]> = (0..40u8).map(|i| [i; 32]).collect();
        let body = bincode::serialize(&hashes).unwrap();

        let chunks = plan_prewarm_requests(&body).expect("a valid manifest yields chunks");
        assert_eq!(chunks.len(), hashes.len().div_ceil(chunk), "ceil(40/16) = 3 chunks");
        assert!(
            chunks.iter().all(|c| (1..=chunk).contains(&c.len())),
            "each chunk carries 1..=NATIVE_DA_FETCH_CHUNK hashes"
        );
        assert_eq!(chunks.concat(), hashes, "every hash covered exactly once, in order");

        assert!(plan_prewarm_requests(b"\x00\x01not-bincode").is_none(), "garbage -> no pull");
        let empty = bincode::serialize::<Vec<[u8; 32]>>(&vec![]).unwrap();
        assert!(plan_prewarm_requests(&empty).is_none(), "empty manifest -> no pull");

        // Review F6: a manifest exceeding the defensive cap is rejected (no pre-warm fan).
        let over_cap = bincode::serialize(&vec![[0u8; 32]; MAX_PREWARM_HASHES + 1]).unwrap();
        assert!(plan_prewarm_requests(&over_cap).is_none(), "over-cap manifest -> no pull");
    }

    /// EVM direct-to-leader forward (Option B), RED first: the EVM forward marker must not
    /// collide with any other DirectRequest marker, or a forwarded EVM tx would be misrouted
    /// into the native-action / pre-proposal receive branches (and vice-versa). MUST fail
    /// before the const exists.
    #[test]
    fn forward_evm_marker_is_distinct() {
        let others = [
            FORWARD_ACTION_MARKER,
            PRE_PROPOSAL_BATCH_MARKER,
            PRE_PROPOSAL_HASHES_MARKER,
        ];
        assert!(
            !others.contains(&FORWARD_EVM_MARKER),
            "FORWARD_EVM_MARKER {:#x} collides with an existing DirectRequest marker",
            FORWARD_EVM_MARKER
        );
    }

    /// The EVM forward wire format round-trips: `encode` prepends the marker, `parse` strips
    /// it and returns the raw RLP. A marker-only or empty envelope yields `None` (no tx to
    /// admit — guards an off-by-one on the slice).
    #[test]
    fn forwarded_evm_tx_roundtrips() {
        let rlp = vec![0x02u8, 0xf8, 0x6c, 0x01, 0x02, 0x03]; // arbitrary RLP-shaped bytes
        let env = encode_forwarded_evm(&rlp);
        assert_eq!(env.first(), Some(&FORWARD_EVM_MARKER), "marker prefixed");
        assert_eq!(parse_forwarded_evm_tx(&env), Some(rlp.as_slice()), "round-trips to raw RLP");

        assert_eq!(parse_forwarded_evm_tx(&[FORWARD_EVM_MARKER]), None, "marker-only -> None");
        assert_eq!(parse_forwarded_evm_tx(&[]), None, "empty -> None");
    }

    /// Cross-routing guard: a NATIVE forward envelope (marker 0xFE) must NOT parse as an EVM
    /// tx — otherwise native-action bodies would be fed to `add_evm_tx`.
    #[test]
    fn parse_forwarded_evm_rejects_native_marker() {
        let mut native_env = vec![FORWARD_ACTION_MARKER];
        native_env.extend_from_slice(b"\x00\x01\x02 native body");
        assert_eq!(parse_forwarded_evm_tx(&native_env), None);
    }

    /// s350 FIX B (RED first): a banned peer that is a CURRENT VALIDATOR-SET member must
    /// keep consensus-critical service — severing a validator's gossip turns a tx-layer
    /// penalty into a cluster liveness fault (s339: our 1h ban of friend2 for *relaying*
    /// oversized batches stalled commits until expiry). Banned non-validators stay
    /// dropped. MUST fail before FIX B lands (no `should_drop_banned_gossip`).
    #[test]
    fn banned_validator_keeps_consensus_service() {
        let vk = test_vk(7);
        let peer = test_peer(7);
        let shared = test_shared();
        shared.peer_map.write().unwrap().insert(vk, peer);
        shared.validators.write().unwrap().insert(vk.to_bytes());
        let mut scoring = PeerScoring::new(None);
        for _ in 0..30 {
            scoring.penalize(&peer, PENALTY_INVALID_CONSENSUS_MSG, "test");
        }
        assert!(scoring.is_banned(&peer), "precondition: the validator IS banned");
        assert!(
            !should_drop_banned_gossip(&mut scoring, &shared, &peer),
            "validator-set member must keep consensus service while banned"
        );

        let outsider = test_peer(8);
        for _ in 0..30 {
            scoring.penalize(&outsider, PENALTY_INVALID_CONSENSUS_MSG, "test");
        }
        assert!(
            should_drop_banned_gossip(&mut scoring, &shared, &outsider),
            "banned non-validator stays dropped"
        );
    }

    /// s350 FIX B: invalid-gossip penalties must hit the cryptographic AUTHOR
    /// (`message.source`), never the last-hop forwarder — meshes relay other nodes'
    /// messages, and penalizing relayers is how an honest validator got banned (s339).
    /// No author in the envelope → nobody is penalized.
    #[test]
    fn oversized_penalty_targets_author_not_forwarder() {
        let author = test_peer(9);
        let forwarder = test_peer(10);
        let mut scoring = PeerScoring::new(None);

        let hit = penalize_gossip_author(
            &mut scoring,
            Some(author),
            PENALTY_INVALID_TX,
            "oversized native action message",
        );
        assert_eq!(hit, Some(author));
        assert_eq!(scoring.score(&author), INITIAL_SCORE - PENALTY_INVALID_TX);
        assert_eq!(scoring.score(&forwarder), INITIAL_SCORE, "forwarder untouched");

        let none = penalize_gossip_author(&mut scoring, None, PENALTY_INVALID_TX, "no author");
        assert_eq!(none, None, "no author -> no penalty");
    }

    /// s350 FIX A (RED first): the publish batcher must flush on a BYTE budget, not just
    /// count — bs50 actions (~10KB+) batched by count produced 160–595KB gossip messages
    /// that every receiver rejects at `max_tx_message_size` (128KB) and penalizes the
    /// forwarder for (root cause of the friend2 validator ban, s339 wedge). MUST fail
    /// before FIX A lands (no `batch_would_exceed` / byte constants).
    #[test]
    fn native_batch_flushes_before_byte_budget() {
        let cap = 128 * 1024;
        // Empty batch: an action that fits exactly at the cap is allowed through...
        let exact_fit = cap - NATIVE_BATCH_HEADER_BYTES - NATIVE_BATCH_ENTRY_OVERHEAD;
        assert!(
            !batch_would_exceed(NATIVE_BATCH_HEADER_BYTES, exact_fit, cap),
            "receivers reject only len > cap (strict), so == cap must pass"
        );
        // ...one byte more can never be delivered: the drop decision boundary.
        assert!(
            batch_would_exceed(NATIVE_BATCH_HEADER_BYTES, exact_fit + 1, cap),
            "an action that cannot fit even alone must be dropped from pre-spread"
        );
        // A 10KB action lands in a batch already holding ~120KB → flush first.
        assert!(
            batch_would_exceed(120 * 1024, 10 * 1024, cap),
            "pushing past the cap must flush the pending batch first"
        );
        assert!(
            !batch_would_exceed(60 * 1024, 10 * 1024, cap),
            "well under budget keeps accumulating"
        );
    }

    /// s350 FIX A: the incremental byte accounting used by the publish loop must match
    /// the real serialized size, or the budget drifts from what receivers measure.
    #[test]
    fn native_batch_size_accounting_matches_serializer() {
        let actions: Vec<Vec<u8>> = vec![vec![0xAB; 10_240], vec![0xCD; 3], vec![0xEF; 75_000]];
        let accounted = NATIVE_BATCH_HEADER_BYTES
            + actions
                .iter()
                .map(|a| NATIVE_BATCH_ENTRY_OVERHEAD + a.len())
                .sum::<usize>();
        assert_eq!(
            serialize_native_batch(&actions).len(),
            accounted,
            "running budget must equal the on-wire message size"
        );
        assert_eq!(
            serialize_native_batch(&[]).len(),
            NATIVE_BATCH_HEADER_BYTES,
            "empty batch is exactly the header"
        );
    }

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
            evm_tx_inbound: None,
            pending_sends: Mutex::new(PendingSendQueue::new(256)),
            outbound_direct: Mutex::new(HashMap::new()),
            recent_native_bundles: Mutex::new(VecDeque::new()),
            native_da: RwLock::new(None),
            native_da_inbound: Mutex::new(VecDeque::new()),
            pending_native_pushes: Mutex::new(PendingSendQueue::new(8)),
            push_scheduler: Mutex::new(PushScheduler::for_pushes()),
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

    /// #4 Task 3 (RED first): the native-action push loop must cap concurrent in-flight
    /// `direct.send_request`s to `PUSH_MAX_INFLIGHT` and QUEUE the overflow, dispatching
    /// the next queued push only as an in-flight one completes (Direct Response /
    /// OutboundFailure) — so N validators × bursty pre-proposal batches can't exhaust
    /// quinn's bidi-stream window (the `max sub-streams reached` storm). `PushScheduler` is
    /// generic over the request-id type so it is testable without a live swarm. MUST fail
    /// before Task 3 (the loop is unbounded; no PushScheduler / PUSH_MAX_INFLIGHT yet).
    #[test]
    fn push_scheduler_caps_inflight_and_drains_on_completion() {
        let mut sched: PushScheduler<u64> = PushScheduler::new(2, 16);

        // Fire 5 pushes; only the cap (2) goes in-flight, the other 3 queue.
        let mut id = 0u64;
        let mut dispatched = 0;
        for _ in 0..5 {
            if sched.has_capacity() {
                id += 1;
                sched.record(id);
                dispatched += 1;
            } else {
                sched.enqueue(test_peer(1), vec![id as u8]);
            }
        }
        assert_eq!(dispatched, 2, "only PUSH_MAX_INFLIGHT sends go in-flight at once");
        assert_eq!(sched.inflight_len(), 2);
        assert_eq!(sched.queued_len(), 3, "overflow is queued, not dropped");

        // An untracked completion (a consensus send shares the Direct protocol) is ignored.
        assert!(sched.complete(999).is_none(), "a non-push id dispatches nothing");
        assert_eq!(sched.inflight_len(), 2);

        // Completing each in-flight push frees a slot and yields one queued push; the
        // caller records the redispatch, so in-flight never exceeds the cap.
        for completed in [1u64, 2, 3] {
            let _ = sched.complete(completed).expect("completion dispatches the next queued push");
            id += 1;
            sched.record(id);
            assert!(sched.inflight_len() <= 2, "in-flight never exceeds the cap");
        }
        assert_eq!(sched.queued_len(), 0, "all queued pushes dispatched");

        // With an empty queue, a completion frees the slot but dispatches nothing.
        assert!(sched.complete(5).is_none());
        assert_eq!(sched.inflight_len(), 1, "freed a slot, nothing left to re-dispatch");
    }

    /// #4 Task 3: the push queue is BOUNDED at `PUSH_QUEUE_CAP` — overflow drops the
    /// OLDEST (the bundle is also retained in `recent_native_bundles` for re-push on
    /// reconnect) and reports the drop, so a saturated link can't grow the queue without
    /// bound and the cap is never silent.
    #[test]
    fn push_scheduler_queue_bounded_drops_oldest() {
        let mut sched: PushScheduler<u64> = PushScheduler::new(1, 2);
        sched.record(1); // fill the single in-flight slot
        assert!(!sched.has_capacity());

        assert!(!sched.enqueue(test_peer(1), vec![1]), "1st fits under the cap");
        assert!(!sched.enqueue(test_peer(2), vec![2]), "2nd fits under the cap");
        assert!(sched.enqueue(test_peer(3), vec![3]), "3rd overflows -> drops oldest");
        assert_eq!(sched.queued_len(), 2, "queue stays bounded at PUSH_QUEUE_CAP");

        // Oldest ([1]) was dropped; FIFO order preserved for the rest ([2] then [3]).
        assert_eq!(sched.complete(1).unwrap().1, vec![2u8]);
    }

    /// #4 Task 3: the in-flight cap must stay well under the raised QUIC stream window
    /// (NATIVE_DA_STREAM_LIMIT=512, Task 2) so the push loop never approaches the ceiling.
    #[test]
    fn push_max_inflight_is_bounded() {
        assert!(
            PUSH_MAX_INFLIGHT > 0 && PUSH_MAX_INFLIGHT <= 64,
            "PUSH_MAX_INFLIGHT={PUSH_MAX_INFLIGHT} must bound the loop well below the 512 window",
        );
    }

    /// Task 4 (headline E2E): a >4 MB native-action body-set reconstructs with ZERO
    /// missing via the REAL pull path — chunked client fetch (`NATIVE_DA_FETCH_CHUNK`)
    /// → `NativeDaCodec` request round-trip → server `serve_native_da_bodies` →
    /// `NativeDaCodec` response round-trip → client absorb by RECOMPUTED hash.
    ///
    /// This is the exact case that wedged the chain at bs=1000 (~4.17 MB blocks): the
    /// old single 4 MB codec cap, shared across protocols, silently dropped the one
    /// oversized native-DA response, so a compact block's bodies never landed and the
    /// block could not be reconstructed (liveness wedge). With per-protocol caps
    /// (Task 1) + client chunking (Task 2) the SAME set travels as several bounded
    /// responses and fully reconstructs. This is the test that would have caught it.
    #[test]
    fn native_da_bigbody_reconstructs_over_4mb() {
        use crate::bridge::NATIVE_DA_FETCH_CHUNK;
        use crate::codec::NativeDaCodec;
        use futures::io::Cursor;
        use libp2p::request_response::Codec as _;
        use libp2p::StreamProtocol;
        use torus_state::db::StateDb;
        use torus_types::{
            compute_action_hash, ActionSignature, FixedPoint, NativeAction, OrderType,
            PlaceOrderParams, Signature, SignedNativeAction, TimeInForce,
        };

        // One `PlaceOrderBatch` body of `n_orders` orders; distinct per `nonce`
        // (`compute_action_hash` mixes in the nonce), so 100 bodies get 100 hashes.
        fn big_body(nonce: u64, n_orders: usize) -> SignedNativeAction {
            let order = PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: FixedPoint::from_raw(6_000_000_000_000),
                quantity: FixedPoint::from_raw(FixedPoint::SCALE),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            };
            SignedNativeAction {
                action: NativeAction::PlaceOrderBatch(vec![order; n_orders]),
                nonce,
                signature: ActionSignature::Eip712(Signature { v: 27, r: [0u8; 32], s: [0u8; 32] }),
            }
        }

        // Seed a DA store with 100 distinct bodies summing > 4 MB (the old shared cap).
        const N_BODIES: usize = 100;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = NativeDaStore::new(StateDb::open(dir.path()).expect("open db"));
        let mut all_hashes: Vec<[u8; 32]> = Vec::with_capacity(N_BODIES);
        let mut total_stored = 0usize;
        for nonce in 0..N_BODIES as u64 {
            let action = big_body(nonce, 1000);
            store.put(&action).expect("seed body");
            let h = compute_action_hash(&action).0;
            total_stored += store.get_raw(&h).unwrap().expect("body stored").len();
            all_hashes.push(h);
        }
        assert!(
            total_stored > 4 * 1024 * 1024,
            "fixture must exceed the old 4 MB cap to be meaningful (got {total_stored} bytes)"
        );

        // Drive the full client→codec→server→codec→client path, chunk by chunk, exactly
        // as the chunked pull-fallback does on the wire.
        let proto = StreamProtocol::new("/torus/native-da/1.0");
        let mut reconstructed: HashMap<[u8; 32], Vec<u8>> = HashMap::new();
        let mut max_response_bytes = 0usize;
        let mut chunk_count = 0usize;
        futures::executor::block_on(async {
            let mut codec = NativeDaCodec;
            for chunk in all_hashes.chunks(NATIVE_DA_FETCH_CHUNK) {
                chunk_count += 1;

                // Client → server: the request round-trips through the codec.
                let mut wbuf = Cursor::new(Vec::new());
                codec
                    .write_request(&proto, &mut wbuf, NativeDaNetRequest { hashes: chunk.to_vec() })
                    .await
                    .expect("write request");
                let mut rbuf = Cursor::new(wbuf.into_inner());
                let served_req =
                    codec.read_request(&proto, &mut rbuf).await.expect("read request");

                // Server serves the bodies for those hashes from its DA store.
                let bodies = serve_native_da_bodies(Some(&store), &served_req.hashes);

                // Server → client: the response round-trips through the codec. Each
                // chunked response is bounded; together they carry the full >4 MB set.
                let mut wbuf = Cursor::new(Vec::new());
                codec
                    .write_response(&proto, &mut wbuf, NativeDaNetResponse { bodies })
                    .await
                    .expect("write response");
                let response_bytes = wbuf.into_inner();
                max_response_bytes = max_response_bytes.max(response_bytes.len());
                let mut rbuf = Cursor::new(response_bytes);
                let served =
                    codec.read_response(&proto, &mut rbuf).await.expect("read response");

                // Client absorbs by RECOMPUTED hash (a peer cannot place a body under a
                // hash it does not own) — mirrors `app.rs::absorb_fetched_bodies`.
                for body in served.bodies {
                    if body.is_empty() {
                        continue;
                    }
                    let action: SignedNativeAction =
                        bincode::deserialize(&body).expect("deserialize served body");
                    reconstructed.insert(compute_action_hash(&action).0, body);
                }
            }
        });

        // Chunking really happened (7 bounded responses, not 1 oversized one)...
        assert_eq!(
            chunk_count,
            N_BODIES.div_ceil(NATIVE_DA_FETCH_CHUNK),
            "100 bodies fetched as ceil(100/16)=7 chunked requests"
        );
        assert!(
            max_response_bytes < total_stored,
            "no single response carries the whole >4 MB set ({max_response_bytes} < {total_stored})"
        );
        // ...and every body reconstructs by its own hash: ZERO missing (the un-wedge).
        let missing: Vec<[u8; 32]> = all_hashes
            .iter()
            .copied()
            .filter(|h| !reconstructed.contains_key(h))
            .collect();
        assert!(
            missing.is_empty(),
            "{} of {N_BODIES} bodies missing after reconstruct",
            missing.len()
        );
        assert_eq!(reconstructed.len(), N_BODIES, "all 100 distinct bodies recovered");
    }
}
