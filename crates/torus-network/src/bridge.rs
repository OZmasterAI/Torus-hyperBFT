use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, RwLock};

use borsh::BorshSerialize;
use ed25519_dalek::{SigningKey, VerifyingKey};
use hotstuff_rs::hotstuff::messages::{BlockDataRequest, BlockDataResponse, HotStuffMessage};
use hotstuff_rs::networking::messages::Message;
use hotstuff_rs::networking::network::Network;
use hotstuff_rs::types::block::Block;
use hotstuff_rs::types::data_types::{CryptoHash, ViewNumber};
use hotstuff_rs::types::update_sets::ValidatorSetUpdates;
use hotstuff_rs::types::validator_set::ValidatorSet;
use libp2p::{identity, multiaddr::Protocol, Multiaddr, PeerId, SwarmBuilder};
use tokio::sync::mpsc;
use tracing::{info, warn};
use zeroize::Zeroize;

use crate::behaviour::TorusBehaviour;
use crate::config::NetworkConfig;
use crate::peer::PeerMap;
use crate::pending_send::PendingSendQueue;
use crate::swarm::{run_swarm_with_config, NetworkCommand, PushScheduler, SharedState};
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

/// Per-connection QUIC bidi-stream window for the swarm transport (#4 Task 2).
///
/// The native-action PUSH loop opens one bidi stream per validator per pre-proposal
/// batch, and consensus sends share the same `/torus/direct` protocol — so under high
/// `batch_size` the libp2p default (256) was exhausted, firing `max sub-streams reached`
/// (2,537x at bs=500) and starving consensus, which helped wedge bs=1000. 512 gives
/// headroom over the push burst + concurrent consensus streams; Task 3 additionally
/// BOUNDS the push loop so it cannot burst toward even this raised ceiling.
const NATIVE_DA_STREAM_LIMIT: u32 = 512;

/// Raise the QUIC transport's per-connection bidi-stream window to
/// [`NATIVE_DA_STREAM_LIMIT`] (used via `SwarmBuilder::with_quic_config`). Factored out
/// so the limit is unit-testable without a live swarm (#4 Task 2).
fn tune_quic_config(mut cfg: libp2p::quic::Config) -> libp2p::quic::Config {
    cfg.max_concurrent_stream_limit = NATIVE_DA_STREAM_LIMIT;
    cfg
}

/// libp2p-based Network implementation for hotstuff_rs.
///
/// Bridges async libp2p with the synchronous Network trait using channels
/// and shared state protected by `Arc<Mutex/RwLock>`.
pub struct LibP2PNetwork {
    command_tx: mpsc::UnboundedSender<NetworkCommand>,
    shared: Arc<SharedState>,
    native_inbound_rx:
        Option<mpsc::UnboundedReceiver<(torus_types::Address, torus_types::SignedNativeAction)>>,
    /// Inbound forwarded EVM txs (raw RLP) received on the leader (Option B). Taken once at
    /// startup to drive an ingest task → `add_evm_tx`.
    evm_inbound_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    /// This node's own validator key, used to skip self when fanning a native-DA
    /// pull-fallback out to the validator set (Task 6).
    local_key: VerifyingKey,
}

impl Clone for LibP2PNetwork {
    fn clone(&self) -> Self {
        Self {
            command_tx: self.command_tx.clone(),
            shared: self.shared.clone(),
            native_inbound_rx: None,
            evm_inbound_rx: None,
            local_key: self.local_key,
        }
    }
}

/// Max action-hashes per `/torus/native-da/1.0` fetch request. The pull-fallback
/// splits a missing-body set into chunks of this size per validator so each response
/// stays well under `MAX_NATIVE_DA_MSG_SIZE` (a `PlaceOrderBatch` body is ≤ ~75 KB, so
/// 16 bodies ≤ ~1.2 MB). Keeps a >4 MB total fetch from collapsing into one oversized
/// response the codec drops (native-DA fix Task 2).
pub const NATIVE_DA_FETCH_CHUNK: usize = 16;

/// Pre-proposal pushes whose encoded body set exceeds this go out as a tiny HASH MANIFEST
/// (peers PULL the bodies) instead of the full multi-MB body push that wedges the view at
/// bs≈500 — the live failure mode is a VIEW TIMEOUT on big-body dissemination (mem
/// f58957c6), not the substream cap. Below the threshold the full-body push stays the fast
/// path (no pull RTT), protecting the healthy bs100 baseline (23,430 o/s @ 289 ms). It sits
/// well UNDER `MAX_DIRECT_MSG_SIZE` (4 MB) so the manifest path engages before the codec
/// rejects an oversized batch. Tunable — validated/adjusted by the bs-sweep (Phase 2.3 #5).
pub const HASH_ONLY_PUSH_THRESHOLD: usize = 512 * 1024; // 512 KB

/// Effective manifest threshold: `TORUS_HASH_ONLY_PUSH_THRESHOLD` (bytes) overrides the
/// compiled default PER NODE, read once at first use. TRANSPORT-ONLY and safe to A/B on a
/// single validator without coordination: receivers handle both push forms (body batch
/// 0xFD / hash manifest 0xFC) regardless of what any proposer chose, so the flag can never
/// split consensus. Exists for the S387 follow-up — with serves off-loop and pre-warm
/// pulling only missing bodies, the 512 KB default is likely too eager, but a raise must be
/// measured on WAN (the view-timeout wedge it guards against is bandwidth-bound, mem
/// f58957c6) — this makes that sweep a restart, not a rebuild/redeploy.
///
/// O5: the request is CLAMPED to `caps::LEGACY_FLEET_DIRECT_MSG_FLOOR`. A threshold above
/// the floor orders full-body pushes that the oldest fleet codec must reject at read time —
/// the S388 `=6000000` deployment did exactly this: every body set in (4 MB, 6 MB] was
/// pushed, rejected by the receiver's 4 MB codec, and survived only via pull fallback.
/// Above-floor requests WARN once at first use and run at the floor.
fn hash_only_push_threshold() -> usize {
    static THRESHOLD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        let requested = std::env::var("TORUS_HASH_ONLY_PUSH_THRESHOLD")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(HASH_ONLY_PUSH_THRESHOLD);
        let floor = crate::caps::LEGACY_FLEET_DIRECT_MSG_FLOOR;
        if requested > floor {
            warn!(
                requested,
                floor, "TORUS_HASH_ONLY_PUSH_THRESHOLD above fleet direct-msg floor — clamped"
            );
        }
        effective_push_threshold_at(requested, floor)
    })
}

/// Pure clamp seam for [`hash_only_push_threshold`]: the effective threshold never
/// exceeds the fleet's direct-msg read floor, so a configured full-body push is always
/// one every peer's codec can actually accept.
fn effective_push_threshold_at(requested: usize, floor: usize) -> usize {
    requested.min(floor)
}

/// Whether a pre-proposal push of `encoded_len` bytes (the bincoded action bodies) should
/// ship HASHES only and let validators pull the bodies (Phase 2.3 #5). Boundary is
/// exclusive: exactly at the threshold still uses the full-body push.
pub fn should_push_hashes_only(encoded_len: usize) -> bool {
    should_push_hashes_only_at(encoded_len, hash_only_push_threshold())
}

/// Pure decision seam for [`should_push_hashes_only`], split out so the boundary is
/// unit-testable at any threshold without touching process env.
fn should_push_hashes_only_at(encoded_len: usize, threshold: usize) -> bool {
    encoded_len > threshold
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
        let (evm_inbound_tx, evm_inbound_rx) = mpsc::unbounded_channel();
        let shared = Arc::new(SharedState {
            inbound: Mutex::new(VecDeque::new()),
            peer_map: RwLock::new(PeerMap::default()),
            validators: RwLock::new(HashSet::new()),
            metrics,
            block_store: RwLock::new(HashMap::new()),
            block_data_inbound: Mutex::new(VecDeque::new()),
            native_action_inbound: Some(native_inbound_tx),
            evm_tx_inbound: Some(evm_inbound_tx),
            pending_sends: Mutex::new(PendingSendQueue::new(256)),
            outbound_direct: Mutex::new(HashMap::new()),
            recent_native_bundles: Mutex::new(VecDeque::new()),
            native_da: RwLock::new(None),
            native_da_inbound: Mutex::new(VecDeque::new()),
            native_da_shards_inbound: Mutex::new(VecDeque::new()),
            pending_native_pushes: Mutex::new(PendingSendQueue::new(8)),
            push_scheduler: Mutex::new(PushScheduler::for_pushes()),
            recently_deposed: RwLock::new(HashMap::new()),
            redial_backoff: Mutex::new(HashMap::new()),
            allow_private_addrs: config.allow_private_addrs,
            pending_da_fetches: Mutex::new(PendingSendQueue::new(
                crate::swarm::DA_FETCH_QUEUE_CAP,
            )),
        });

        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (tx_tx, tx_rx) = mpsc::unbounded_channel();
        let (native_tx, native_rx) = mpsc::channel(8192);

        let max_peers = config.max_peers;
        let gossipsub_heartbeat_ms = config.gossipsub_heartbeat_ms;
        // O5: the transport frame cap must dominate every app-level accept
        // gate riding gossip, or a config raise silently makes legal messages
        // unpublishable (ladder ordering asserted in caps::tests).
        let gossip_max_transmit = crate::caps::GOSSIP_MAX_TRANSMIT_SIZE
            .max(config.max_consensus_message_size)
            .max(config.max_tx_message_size);
        // B3: per-peer send-queue length is config-wired (env
        // TORUS_GOSSIP_QUEUE_LEN via NetworkConfig::default; =5000 restores
        // libp2p's shipped default).
        let gossipsub_queue_len = config.gossipsub_queue_len;
        // DNS wraps the dial filter which wraps QUIC, so /dns4 bootstrap
        // entries resolve BEFORE the filter judges the literal IP. Kademlia
        // query dials to stale private records (re-learned from peers that
        // still carry them) die at the filter instead of hitting the wire.
        let enforce_global_dials = !config.allow_private_addrs;
        let exempt_bootstrap: Arc<HashSet<Multiaddr>> = Arc::new(
            config
                .bootstrap_peers
                .iter()
                .map(|(_, a)| crate::transport::strip_p2p(a))
                .collect(),
        );
        let mut swarm = SwarmBuilder::with_existing_identity(libp2p_keypair)
            .with_tokio()
            .with_other_transport(|key| {
                let quic = libp2p::quic::tokio::Transport::new(tune_quic_config(
                    libp2p::quic::Config::new(key),
                ));
                crate::transport::GlobalOnlyTransport::new(
                    quic,
                    enforce_global_dials,
                    exempt_bootstrap,
                )
            })
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
            .with_dns()
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
            .with_behaviour(|key| {
                TorusBehaviour::with_limits_and_heartbeat(
                    key,
                    max_peers,
                    gossipsub_heartbeat_ms,
                    gossip_max_transmit,
                    gossipsub_queue_len,
                )
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
                swarm,
                command_rx,
                tx_rx,
                native_rx,
                shared_clone,
                local_key,
                &config_clone,
            )
            .await
        });

        let network = Self {
            command_tx,
            shared,
            native_inbound_rx: Some(native_inbound_rx),
            evm_inbound_rx: Some(evm_inbound_rx),
            local_key,
        };
        let tx_handle = TxGossipHandle { tx_sender: tx_tx };
        let native_handle = NativeGossipHandle { sender: native_tx };
        Ok((network, tx_handle, native_handle))
    }

    /// Take the inbound native action receiver. Called once at startup to
    /// spawn a task that drains gossip-received actions into the mempool.
    pub fn take_native_action_rx(
        &mut self,
    ) -> Option<mpsc::UnboundedReceiver<(torus_types::Address, torus_types::SignedNativeAction)>>
    {
        self.native_inbound_rx.take()
    }

    /// Take the inbound forwarded-EVM-tx receiver (raw RLP). Called once at startup to spawn
    /// the ingest task that drains direct-to-leader-forwarded EVM txs into the mempool (Option B).
    pub fn take_evm_tx_rx(&mut self) -> Option<mpsc::UnboundedReceiver<Vec<u8>>> {
        self.evm_inbound_rx.take()
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
        let _ = self
            .command_tx
            .send(NetworkCommand::ForwardNativeAction { target, payload });
    }

    /// Forward a raw RLP EVM transaction directly to the leader (Option B — EVM tx
    /// dissemination). Payload is the raw RLP; the leader full-validates via `add_evm_tx`.
    pub fn forward_evm_tx(&self, target: VerifyingKey, payload: Vec<u8>) {
        let _ = self
            .command_tx
            .send(NetworkCommand::ForwardEvmTx { target, payload });
    }

    pub fn broadcast_native_actions(&self, payload: Vec<u8>) {
        let _ = self
            .command_tx
            .send(NetworkCommand::BroadcastNativeActions { payload });
    }

    /// Push only the action HASHES (a tiny manifest) for an oversized pre-proposal batch;
    /// validators PULL the bodies by-hash (Phase 2.3 #5). The body set is too big to
    /// disseminate within the view, so the full-body push would wedge it (bs≈500 VIEW
    /// TIMEOUT on big-body dissemination, mem f58957c6).
    pub fn broadcast_native_action_hashes(&self, hashes: Vec<[u8; 32]>) {
        let _ = self
            .command_tx
            .send(NetworkCommand::BroadcastNativeActionHashes { hashes });
    }

    /// Attach the durable native-action DA store so the swarm can SERVE bodies
    /// by-hash on `/torus/native-da/1.0` (Task 5). Called once at startup with a
    /// cheap clone over the shared StateDb.
    pub fn set_native_da_store(&self, store: torus_state::NativeDaStore) {
        *self.shared.native_da.write().unwrap() = Some(store);
    }

    /// Issue a RARE pull-fallback fetch for missing native-action bodies by-hash
    /// from `target` (Task 6). Non-blocking: the response is delivered to the
    /// inbound queue, drained via [`Self::drain_native_da_inbound`].
    pub fn fetch_native_actions(&self, target: VerifyingKey, hashes: Vec<[u8; 32]>) {
        let _ = self
            .command_tx
            .send(NetworkCommand::FetchNativeActions { target, hashes });
    }

    /// Fan a RARE pull-fallback fetch out to every other validator (Task 6). Used on
    /// a reconstruction miss: at least one peer holds the bodies durably (the proposer
    /// mirrored them in produce_block), so asking the whole set recovers reliably
    /// without the consensus layer needing to resolve peer identity. Non-blocking;
    /// fires rarely (push covers the common case).
    ///
    /// The hash list is split into [`NATIVE_DA_FETCH_CHUNK`]-sized requests per target
    /// so no single `/torus/native-da/1.0` response approaches the codec cap — a >4 MB
    /// body-set is pulled as several bounded responses instead of one oversized one that
    /// the codec drops (the bs=1000 wedge; native-DA fix Task 2).
    pub fn fetch_native_actions_from_validators(&self, hashes: Vec<[u8; 32]>) {
        if hashes.is_empty() {
            return;
        }
        let local = self.local_key.to_bytes();
        let mut targets: Vec<VerifyingKey> = {
            let validators = self.shared.validators.read().unwrap();
            validators
                .iter()
                .filter(|vk_bytes| **vk_bytes != local)
                .filter_map(|vk_bytes| VerifyingKey::from_bytes(vk_bytes).ok())
                .collect()
        };
        // S447 (body starvation): with an EMPTY/uninitialized validator set (an
        // rpc-only node, or a pull racing `init_validator_set`) the committed-set
        // fan resolved to ZERO targets and the pull was a SILENT NO-OP — the node
        // could never source committed-block bodies. Fall back to the mapped
        // peers (the connected mesh): every peer serves `/torus/native-da` reads
        // (S443 FIX 3). The committed validator set stays the primary source, so
        // the healthy validator path is bit-identical.
        if targets.is_empty() {
            let peer_map = self.shared.peer_map.read().unwrap();
            targets = peer_map
                .vks()
                .filter(|vk| vk.to_bytes() != local)
                .cloned()
                .collect();
            if !targets.is_empty() {
                tracing::warn!(
                    fallback_peers = targets.len(),
                    "native-da pull: validator set empty — falling back to mapped peers"
                );
            }
        }
        for target in targets {
            for chunk in hashes.chunks(NATIVE_DA_FETCH_CHUNK) {
                let _ = self.command_tx.send(NetworkCommand::FetchNativeActions {
                    target,
                    hashes: chunk.to_vec(),
                });
            }
        }
    }

    /// Drain native-action bodies received via the pull-fallback response (each =
    /// `bincode(SignedNativeAction)`). Non-blocking — returns whatever has arrived.
    pub fn drain_native_da_inbound(&self) -> Vec<Vec<u8>> {
        let mut q = self.shared.native_da_inbound.lock().unwrap();
        q.drain(..).collect()
    }

    /// Erasure-shard pull fan-out (T8-integration inc 3): the additive analog to
    /// [`Self::fetch_native_actions_from_validators`], but instead of pulling the
    /// WHOLE body from every peer it asks each DISTINCT validator for a DISTINCT
    /// shard index → up to `want` shards from `want` distinct sources (killing the
    /// s338 single-source serve hotspot). Every custodying peer holds all `n`
    /// shards, so asking the j-th target for index `j` yields distinct shards from
    /// distinct sources; `targets = committed validators − self = n − 1`, so every
    /// requested index `j < n` is valid. Non-blocking — a reconstruction miss falls
    /// back to the whole-body pull, so a skipped/disconnected peer never wedges.
    pub fn fetch_shards_from_validators(&self, body_hash: [u8; 32], want: u16) {
        if want == 0 {
            return;
        }
        let local = self.local_key.to_bytes();
        let mut targets: Vec<VerifyingKey> = {
            let validators = self.shared.validators.read().unwrap();
            validators
                .iter()
                .filter(|vk_bytes| **vk_bytes != local)
                .filter_map(|vk_bytes| VerifyingKey::from_bytes(vk_bytes).ok())
                .collect()
        };
        // Same S447 fallback as the whole-body pull: an EMPTY/uninitialized
        // validator set (an rpc-only node, or a fetch racing `init_validator_set`)
        // would otherwise make this a SILENT NO-OP. Fall back to the mapped peers
        // (the connected mesh — every peer serves `/torus/native-da-shards`); the
        // committed validator set stays the primary source so the healthy path is
        // bit-identical.
        if targets.is_empty() {
            let peer_map = self.shared.peer_map.read().unwrap();
            targets = peer_map
                .vks()
                .filter(|vk| vk.to_bytes() != local)
                .cloned()
                .collect();
            if !targets.is_empty() {
                tracing::warn!(
                    fallback_peers = targets.len(),
                    "shard pull: validator set empty — falling back to mapped peers"
                );
            }
        }
        for (j, target) in targets.into_iter().take(want as usize).enumerate() {
            let _ = self.command_tx.send(NetworkCommand::FetchNativeDaShards {
                target,
                body_hash,
                shard_index: j as u16,
            });
        }
    }

    /// Drain erasure shards received via the shard pull-fallback response
    /// (T8-integration inc 3), each tagged with its source peer bytes. `torus-node`
    /// maps these `(source, StoredShard)` tuples into
    /// `torus_consensus::shard_recovery::GatheredShard` (the network crate stays
    /// free of a consensus dependency). Non-blocking.
    pub fn drain_native_da_shards_inbound(&self) -> Vec<(Vec<u8>, torus_state::StoredShard)> {
        let mut q = self.shared.native_da_shards_inbound.lock().unwrap();
        q.drain(..).collect()
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
        let mut deposed = self.shared.recently_deposed.write().unwrap();
        let now = std::time::Instant::now();
        for (vk, _power) in updates.inserts() {
            validators.insert(vk.to_bytes());
            let peer_id = peer_id_from_verifying_key(vk);
            peer_map.insert(*vk, peer_id);
            // Re-seated (or rejoining) — no longer deposed, drop any grace record.
            deposed.remove(&vk.to_bytes());
        }
        for vk in updates.deletes() {
            validators.remove(&vk.to_bytes());
            peer_map.remove_by_vk(vk);
            // FIX 3 (S443): stamp the deposition instant so the swarm loop grants a
            // grace window before disconnecting the peer or penalizing its still-in-
            // flight votes as an "unregistered peer" (t15). Bounded: only the deleted
            // set is stored, and the grace check sweeps expired entries lazily.
            deposed.insert(vk.to_bytes(), now);
        }
        // Opportunistic sweep so the map can't grow across many rotations.
        deposed.retain(|_, t| t.elapsed() < crate::swarm::DEPOSED_GRACE);
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
            Ok(block) => Some((
                vk,
                BlockDataResponse {
                    view: ViewNumber::new(view),
                    block,
                },
            )),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal [`SharedState`] carrying a fixed validator set — enough to drive
    /// `fetch_native_actions_from_validators` without a live swarm. Only `validators`
    /// is exercised here; the rest mirror the swarm-module test helper.
    fn shared_with_validators(vks: &[VerifyingKey]) -> Arc<SharedState> {
        let validators: HashSet<[u8; 32]> = vks.iter().map(|vk| vk.to_bytes()).collect();
        Arc::new(SharedState {
            inbound: Mutex::new(VecDeque::new()),
            peer_map: RwLock::new(PeerMap::default()),
            validators: RwLock::new(validators),
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
            native_da_shards_inbound: Mutex::new(VecDeque::new()),
            pending_native_pushes: Mutex::new(PendingSendQueue::new(8)),
            push_scheduler: Mutex::new(PushScheduler::for_pushes()),
            recently_deposed: RwLock::new(HashMap::new()),
            redial_backoff: Mutex::new(HashMap::new()),
            allow_private_addrs: false,
            pending_da_fetches: Mutex::new(PendingSendQueue::new(
                crate::swarm::DA_FETCH_QUEUE_CAP,
            )),
        })
    }

    fn test_signing_key(seed: u8) -> SigningKey {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        SigningKey::from_bytes(&bytes)
    }

    /// #4 Task 2 (RED first): the QUIC transport must raise its per-connection bidi-stream
    /// window above the libp2p default so the native-action PUSH loop (one stream per
    /// validator per pre-proposal, plus consensus sends sharing the Direct protocol) cannot
    /// exhaust it under bs=500+ load — the `max sub-streams reached` storm (2,537x at
    /// bs=500) that helped wedge bs=1000. `tune_quic_config` must RAISE the limit and land
    /// it at the chosen headroom. MUST fail before Task 2 (no tune_quic_config /
    /// NATIVE_DA_STREAM_LIMIT yet).
    #[test]
    fn quic_stream_window_raised_for_push_headroom() {
        let kp = identity::Keypair::generate_ed25519();
        let default_limit = libp2p::quic::Config::new(&kp).max_concurrent_stream_limit;
        let tuned = tune_quic_config(libp2p::quic::Config::new(&kp));
        assert!(
            tuned.max_concurrent_stream_limit > default_limit,
            "tuning must RAISE the bidi-stream window above the libp2p default ({default_limit})",
        );
        assert!(
            tuned.max_concurrent_stream_limit >= 512,
            "stream window must be >=512 for push-burst + consensus headroom, got {}",
            tuned.max_concurrent_stream_limit,
        );
        assert_eq!(
            tuned.max_concurrent_stream_limit, NATIVE_DA_STREAM_LIMIT,
            "the builder must use NATIVE_DA_STREAM_LIMIT",
        );
    }

    /// Task 2 (RED first): the by-hash native-DA pull-fallback must split its hash
    /// list into `NATIVE_DA_FETCH_CHUNK`-sized requests PER validator, so a >4 MB
    /// body-set is pulled as several codec-sized responses instead of one oversized
    /// response the codec drops (the bs=1000 wedge). Driving the fetch with 100 hashes
    /// against a 3-validator set (self + 2 peers) must enqueue `ceil(100/16)=7`
    /// `FetchNativeActions` commands per NON-self validator (=14), each carrying
    /// ≤16 hashes and together covering every hash once per validator. Today (one
    /// oversized request/validator) this fails the per-request size bound.
    #[test]
    fn fetch_native_actions_chunks_per_validator() {
        let local = test_signing_key(1);
        let peer_a = test_signing_key(2).verifying_key();
        let peer_b = test_signing_key(3).verifying_key();

        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let shared = shared_with_validators(&[local.verifying_key(), peer_a, peer_b]);
        let net = LibP2PNetwork {
            command_tx,
            shared,
            native_inbound_rx: None,
            evm_inbound_rx: None,
            local_key: local.verifying_key(),
        };

        // 100 distinct hashes.
        let hashes: Vec<[u8; 32]> = (0..100u32)
            .map(|i| {
                let mut h = [0u8; 32];
                h[..4].copy_from_slice(&i.to_le_bytes());
                h
            })
            .collect();
        net.fetch_native_actions_from_validators(hashes.clone());

        // Drain every enqueued command; bucket the chunks by target.
        let mut per_target: HashMap<[u8; 32], Vec<[u8; 32]>> = HashMap::new();
        let mut command_count = 0usize;
        while let Ok(cmd) = command_rx.try_recv() {
            let NetworkCommand::FetchNativeActions { target, hashes } = cmd else {
                panic!("fetch must enqueue only FetchNativeActions commands");
            };
            command_count += 1;
            assert!(
                (1..=NATIVE_DA_FETCH_CHUNK).contains(&hashes.len()),
                "each request carries 1..=NATIVE_DA_FETCH_CHUNK hashes, got {}",
                hashes.len()
            );
            per_target
                .entry(target.to_bytes())
                .or_default()
                .extend(hashes);
        }

        let chunks_per_validator = hashes.len().div_ceil(NATIVE_DA_FETCH_CHUNK); // 7
        assert_eq!(
            command_count,
            chunks_per_validator * 2,
            "ceil(100/16)=7 chunks × 2 non-self validators = 14 commands"
        );
        assert_eq!(
            per_target.len(),
            2,
            "exactly the 2 non-self validators are targeted"
        );
        assert!(
            !per_target.contains_key(&local.verifying_key().to_bytes()),
            "self is never a fetch target"
        );
        for got in per_target.values() {
            assert_eq!(
                got, &hashes,
                "every hash delivered exactly once per validator, in order"
            );
        }
    }

    /// S447 val1 body starvation (RED first): with an EMPTY validator set (an
    /// rpc-only node, or a pull racing `init_validator_set`) the committed-set
    /// fan-out resolved to ZERO targets and the pull was a SILENT NO-OP — the
    /// node could never source committed-block bodies and starved. The fix
    /// keeps the committed validator set as the primary selection source
    /// (healthy path bit-identical: see `fetch_native_actions_chunks_per_validator`)
    /// and falls back to the MAPPED peers (the connected mesh — every peer
    /// serves `/torus/native-da` since S443 FIX 3) only when the validator fan
    /// yields no targets. Today this fails: zero commands are enqueued.
    #[test]
    fn fetch_native_actions_falls_back_to_mapped_peers_when_validator_set_empty() {
        let local = test_signing_key(1);
        let peer_a = test_signing_key(2).verifying_key();
        let peer_b = test_signing_key(3).verifying_key();

        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        // EMPTY validator set — the starvation precondition.
        let shared = shared_with_validators(&[]);
        {
            let mut pm = shared.peer_map.write().unwrap();
            pm.insert(peer_a, peer_id_from_verifying_key(&peer_a));
            pm.insert(peer_b, peer_id_from_verifying_key(&peer_b));
            // Self is mapped too and must still be skipped.
            pm.insert(
                local.verifying_key(),
                peer_id_from_verifying_key(&local.verifying_key()),
            );
        }
        let net = LibP2PNetwork {
            command_tx,
            shared,
            native_inbound_rx: None,
            evm_inbound_rx: None,
            local_key: local.verifying_key(),
        };

        let hashes: Vec<[u8; 32]> = (0..20u32)
            .map(|i| {
                let mut h = [0u8; 32];
                h[..4].copy_from_slice(&i.to_le_bytes());
                h
            })
            .collect();
        net.fetch_native_actions_from_validators(hashes.clone());

        let mut per_target: HashMap<[u8; 32], Vec<[u8; 32]>> = HashMap::new();
        while let Ok(cmd) = command_rx.try_recv() {
            let NetworkCommand::FetchNativeActions { target, hashes } = cmd else {
                panic!("fetch must enqueue only FetchNativeActions commands");
            };
            assert!(
                (1..=NATIVE_DA_FETCH_CHUNK).contains(&hashes.len()),
                "fallback requests stay chunk-bounded, got {}",
                hashes.len()
            );
            per_target
                .entry(target.to_bytes())
                .or_default()
                .extend(hashes);
        }

        assert!(
            !per_target.contains_key(&local.verifying_key().to_bytes()),
            "self is never a fetch target"
        );
        assert_eq!(
            per_target.len(),
            2,
            "empty validator set must fall back to the 2 mapped non-self peers \
             instead of silently no-oping"
        );
        for got in per_target.values() {
            assert_eq!(
                got, &hashes,
                "every hash delivered exactly once per fallback peer, in order"
            );
        }
    }

    /// T8-integration inc 3 (RED first): the erasure-shard pull fan-out must ask each
    /// DISTINCT non-self validator for a DISTINCT shard index (0,1,2,…) so `want`
    /// shards arrive from `want` distinct SOURCES (the s338 single-source hotspot
    /// killer). Driving it against a 4-validator set (self + 3 peers) with `want=8`
    /// (capped by the 3 available targets) must enqueue exactly ONE
    /// `FetchNativeDaShards` per non-self validator, indices exactly {0,1,2}, all
    /// carrying the right `body_hash`, and never targeting self. Today this fails
    /// (no method / variant).
    #[test]
    fn fetch_shards_emits_distinct_index_per_peer() {
        let local = test_signing_key(1);
        let peer_a = test_signing_key(2).verifying_key();
        let peer_b = test_signing_key(3).verifying_key();
        let peer_c = test_signing_key(4).verifying_key();

        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let shared =
            shared_with_validators(&[local.verifying_key(), peer_a, peer_b, peer_c]);
        let net = LibP2PNetwork {
            command_tx,
            shared,
            native_inbound_rx: None,
            evm_inbound_rx: None,
            local_key: local.verifying_key(),
        };

        let body_hash = [0x5au8; 32];
        net.fetch_shards_from_validators(body_hash, 8);

        let mut targets: HashSet<[u8; 32]> = HashSet::new();
        let mut indices: Vec<u16> = Vec::new();
        while let Ok(cmd) = command_rx.try_recv() {
            let NetworkCommand::FetchNativeDaShards {
                target,
                body_hash: got_hash,
                shard_index,
            } = cmd
            else {
                panic!("fetch must enqueue only FetchNativeDaShards commands");
            };
            assert_eq!(got_hash, body_hash, "the exact body_hash is forwarded");
            assert!(
                targets.insert(target.to_bytes()),
                "each non-self validator is targeted exactly once (distinct source)"
            );
            indices.push(shard_index);
        }

        assert_eq!(
            targets,
            [peer_a, peer_b, peer_c]
                .iter()
                .map(|vk| vk.to_bytes())
                .collect::<HashSet<_>>(),
            "exactly the 3 non-self validators are targeted"
        );
        assert!(
            !targets.contains(&local.verifying_key().to_bytes()),
            "self is never a shard-fetch target"
        );
        indices.sort_unstable();
        assert_eq!(
            indices,
            vec![0u16, 1, 2],
            "distinct contiguous shard indices 0,1,2 — one per distinct source"
        );
    }

    /// T8-integration inc 3 (RED first): with an EMPTY validator set the shard pull
    /// must fall back to the MAPPED peers (same S447 no-op fix as the whole-body
    /// pull) instead of silently enqueuing nothing — otherwise an rpc-only node (or
    /// a fetch racing `init_validator_set`) could never source shards. Today this
    /// fails: zero commands enqueued.
    #[test]
    fn fetch_shards_falls_back_to_peer_map_when_validators_empty() {
        let local = test_signing_key(1);
        let peer_a = test_signing_key(2).verifying_key();
        let peer_b = test_signing_key(3).verifying_key();

        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let shared = shared_with_validators(&[]);
        {
            let mut pm = shared.peer_map.write().unwrap();
            pm.insert(peer_a, peer_id_from_verifying_key(&peer_a));
            pm.insert(peer_b, peer_id_from_verifying_key(&peer_b));
            // Self is mapped too and must still be skipped.
            pm.insert(
                local.verifying_key(),
                peer_id_from_verifying_key(&local.verifying_key()),
            );
        }
        let net = LibP2PNetwork {
            command_tx,
            shared,
            native_inbound_rx: None,
            evm_inbound_rx: None,
            local_key: local.verifying_key(),
        };

        let body_hash = [0x11u8; 32];
        net.fetch_shards_from_validators(body_hash, 8);

        let mut targets: HashSet<[u8; 32]> = HashSet::new();
        while let Ok(cmd) = command_rx.try_recv() {
            let NetworkCommand::FetchNativeDaShards {
                target,
                body_hash: got_hash,
                ..
            } = cmd
            else {
                panic!("fetch must enqueue only FetchNativeDaShards commands");
            };
            assert_eq!(got_hash, body_hash);
            targets.insert(target.to_bytes());
        }
        assert!(
            !targets.contains(&local.verifying_key().to_bytes()),
            "self is never a shard-fetch target"
        );
        assert_eq!(
            targets,
            [peer_a, peer_b]
                .iter()
                .map(|vk| vk.to_bytes())
                .collect::<HashSet<_>>(),
            "empty validator set falls back to the 2 mapped non-self peers instead of no-oping"
        );
    }

    /// Phase 2.3 (#5, RED first): the hash-only push helper must enqueue a
    /// `BroadcastNativeActionHashes` command carrying exactly the manifest hashes (the
    /// swarm then wraps them in a `PRE_PROPOSAL_HASHES_MARKER` envelope and fans them out
    /// over the same PushScheduler-bounded path). MUST fail before Task 2 (no method/variant).
    #[test]
    fn broadcast_hashes_enqueues_hashes_command() {
        let local = test_signing_key(1);
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let net = LibP2PNetwork {
            command_tx,
            shared: shared_with_validators(&[local.verifying_key()]),
            native_inbound_rx: None,
            evm_inbound_rx: None,
            local_key: local.verifying_key(),
        };
        net.broadcast_native_action_hashes(vec![[7u8; 32], [9u8; 32]]);
        let NetworkCommand::BroadcastNativeActionHashes { hashes } =
            command_rx.try_recv().expect("a command must be enqueued")
        else {
            panic!("expected a BroadcastNativeActionHashes command");
        };
        assert_eq!(
            hashes,
            vec![[7u8; 32], [9u8; 32]],
            "the exact manifest hashes are forwarded"
        );
    }

    /// Phase 2.3 (#5, RED first): pre-proposal pushes above `HASH_ONLY_PUSH_THRESHOLD`
    /// must ship HASHES only (peers pull the bodies) instead of the full-body push that
    /// wedges the view at bs≈500 (VIEW TIMEOUT on big-body dissemination, mem f58957c6).
    /// The gate is by encoded byte size and must engage BELOW the 4 MB `/torus/direct`
    /// cap so the manifest path kicks in before the codec rejects an oversized batch.
    /// MUST fail before Task 1 (no `should_push_hashes_only` / `HASH_ONLY_PUSH_THRESHOLD`).
    #[test]
    fn hash_only_gate_triggers_above_threshold() {
        assert!(
            !should_push_hashes_only(0),
            "empty/small batch keeps the full-body push"
        );
        assert!(
            !should_push_hashes_only(HASH_ONLY_PUSH_THRESHOLD),
            "at the threshold stays full-body (boundary is exclusive)"
        );
        assert!(
            should_push_hashes_only(HASH_ONLY_PUSH_THRESHOLD + 1),
            "above the threshold ships hashes only"
        );
        assert!(
            HASH_ONLY_PUSH_THRESHOLD < 4 * 1024 * 1024,
            "gate must engage BELOW the 4 MB /torus/direct cap"
        );
    }

    /// S387 follow-up: the manifest gate honors a per-node override threshold via the
    /// pure seam (`TORUS_HASH_ONLY_PUSH_THRESHOLD` routes here; OnceLock env state
    /// itself is process-wide, so the boundary is tested threshold-parameterized).
    #[test]
    fn hash_only_gate_boundary_holds_at_any_threshold() {
        for threshold in [0usize, 1, HASH_ONLY_PUSH_THRESHOLD, 2 * 1024 * 1024] {
            assert!(
                !should_push_hashes_only_at(threshold, threshold),
                "at threshold {threshold} stays full-body (exclusive boundary)"
            );
            assert!(
                should_push_hashes_only_at(threshold + 1, threshold),
                "above threshold {threshold} ships hashes only"
            );
        }
    }

    /// O5: an env-requested threshold above the fleet's direct-msg read floor is
    /// clamped (pure seam — OnceLock env state is process-wide, so the clamp rule
    /// is tested parameterized). The S388 live value (6 MB > 4 MB floor) is the
    /// motivating case: it ordered pushes every receiver's codec had to reject.
    #[test]
    fn env_threshold_clamps_to_fleet_floor() {
        use crate::caps::LEGACY_FLEET_DIRECT_MSG_FLOOR;
        // S388 live value: 6 MB requested, 4 MB fleet floor -> clamped.
        assert_eq!(
            effective_push_threshold_at(6_000_000, LEGACY_FLEET_DIRECT_MSG_FLOOR),
            LEGACY_FLEET_DIRECT_MSG_FLOOR
        );
        // At or under the floor: honored verbatim.
        assert_eq!(
            effective_push_threshold_at(512 * 1024, LEGACY_FLEET_DIRECT_MSG_FLOOR),
            512 * 1024
        );
        assert_eq!(
            effective_push_threshold_at(
                LEGACY_FLEET_DIRECT_MSG_FLOOR,
                LEGACY_FLEET_DIRECT_MSG_FLOOR
            ),
            LEGACY_FLEET_DIRECT_MSG_FLOOR
        );
    }
}
