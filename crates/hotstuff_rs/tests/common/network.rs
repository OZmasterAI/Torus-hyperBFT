//! A "mock" (totally local) network for passing around HotStuff-rs messages.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
        Arc, Mutex,
    },
};

use ed25519_dalek::VerifyingKey;
use hotstuff_rs::{
    hotstuff::messages::HotStuffMessage,
    networking::{
        messages::{Message, ProgressMessage},
        network::Network,
    },
    types::{update_sets::ValidatorSetUpdates, validator_set::ValidatorSet},
};

/// A runtime-toggleable message filter shared by every [`NetworkStub`] in a cluster.
///
/// The filter is used by the S426 livelock repro to selectively deny *block bodies* to a chosen
/// subset of nodes (`starved`) while letting lightweight consensus traffic (proposal headers, phase
/// votes, new-views, pacemaker) flow freely. This is exactly the production failure mode: a QC forms
/// on a block whose body reached only some nodes.
///
/// Toggled at runtime via the [`FilterHandle`] returned by [`mock_network_with_filter`]. When
/// disabled the filter is a complete no-op (a fully healthy network) *and takes no lock on the
/// message hot path* — see [`FilterHandle`] for why that matters.
#[derive(Clone, Default)]
pub(crate) struct MessageFilter {
    /// Whether the filter is active. When `false`, no message is ever dropped.
    pub(crate) enabled: bool,
    /// Recipients that must never receive a block body while the filter is enabled.
    pub(crate) starved: Vec<VerifyingKey>,
    /// When `true`, also drop `ProposalHeader`s to `starved` recipients. This drives a starved node
    /// down the "dropping proposal header: ... justify_block_known=false" path
    /// (`hotstuff/implementation.rs:1640`), where the node sets `sync_needed` but does **not** track
    /// the missing justify block for a by-hash re-fetch — so its only route back is (buggy) block
    /// sync. When `false`, only block bodies are dropped (the node still sees headers, votes on them,
    /// and re-fetches bodies via `body_fetch_tracker`).
    pub(crate) drop_headers: bool,
}

impl MessageFilter {
    /// Returns `true` if a message to `recipient` should be dropped.
    ///
    /// Only *body-carrying* HotStuff messages are dropped, and only to starved recipients:
    /// - [`HotStuffMessage::BlockDataResponse`]: the point-to-point body delivered in response to a
    ///   starved node's `BlockDataRequest` (the sole body-delivery path in the header-first pipeline).
    /// - [`HotStuffMessage::Proposal`]: the legacy full proposal (body included), dropped for safety
    ///   in case any code path broadcasts it.
    ///
    /// Everything else — crucially `ProposalHeader` and `PhaseVote` — passes through, so the starved
    /// node still votes on the header (helping a QC form) yet never obtains the block itself.
    fn should_drop(&self, recipient: &VerifyingKey, message: &Message) -> bool {
        if !self.enabled || !self.starved.contains(recipient) {
            return false;
        }
        match message {
            Message::ProgressMessage(ProgressMessage::HotStuffMessage(hs)) => match hs {
                HotStuffMessage::BlockDataResponse(_) | HotStuffMessage::Proposal(_) => true,
                HotStuffMessage::ProposalHeader(_) => self.drop_headers,
                _ => false,
            },
            _ => false,
        }
    }
}

/// A network stub that passes messages to and from nodes using channels.
///
/// ## Limitations
///
/// `NetworkStub`'s implementation of the [`Network`] trait's `init_validator_set` and
/// `update_validator_set` methods are no-ops. As a consequence, the set of peers reachable from a given
/// `NetworkStub` is fixed on construction by [`mock_network`].
///
/// Therefore, tests that dynamically change the validator set must "plan ahead" and create mock network
/// with "extra" `VerifyingKey`s, beyond the ones for the replicas that are started initially.
#[derive(Clone)]
pub(crate) struct NetworkStub {
    my_verifying_key: VerifyingKey,
    all_peers: HashMap<VerifyingKey, Sender<(VerifyingKey, Message)>>,
    inbox: Arc<Mutex<Receiver<(VerifyingKey, Message)>>>,
    /// S470 wedge repro: when set, every BODY-carrying message
    /// (`BlockDataResponse`, full `Proposal`) is delivered this much later
    /// than it was sent, while headers, votes and pacemaker traffic stay
    /// instant. This models the production header-first regime where
    /// `highest_pc` advances at header speed but a block only becomes
    /// INSERTABLE (disseminated + executed) seconds later — with the
    /// algorithm thread and view clock fully live in between, which is what
    /// lets views race ahead of the commit frontier. `None` (every pre-S470
    /// constructor) is a plain instant network.
    body_delay: Option<std::time::Duration>,
    /// Fast-path flag mirroring [`MessageFilter::enabled`], shared across all stubs. Checked with a
    /// single relaxed atomic load on every `send`/`broadcast` so that the *disabled* path (every
    /// existing test, and the healthy phases of the livelock test) never touches the mutex below.
    /// Acquiring the shared mutex on every message serializes the whole mock network across replica
    /// threads and makes the load-sensitive consensus tests flake — this flag avoids that.
    filter_enabled: Arc<AtomicBool>,
    /// Cluster-wide, runtime-toggleable message filter (shared across all stubs). See
    /// [`MessageFilter`]. Only consulted when `filter_enabled` is set.
    filter: Arc<Mutex<MessageFilter>>,
}

impl Network for NetworkStub {
    fn init_validator_set(&mut self, _: ValidatorSet) {}

    fn update_validator_set(&mut self, _: ValidatorSetUpdates) {}

    fn send(&mut self, peer: VerifyingKey, message: Message) {
        if self.filter_enabled.load(Ordering::Relaxed)
            && self.filter.lock().unwrap().should_drop(&peer, &message)
        {
            return;
        }
        if let Some(peer_tx) = self.all_peers.get(&peer) {
            deliver(
                self.my_verifying_key,
                peer_tx.clone(),
                message,
                self.body_delay,
            );
        }
    }

    fn broadcast(&mut self, message: Message) {
        let active = self.filter_enabled.load(Ordering::Relaxed);
        for (recipient, peer_tx) in &self.all_peers {
            if active && self.filter.lock().unwrap().should_drop(recipient, &message) {
                continue;
            }
            deliver(
                self.my_verifying_key,
                peer_tx.clone(),
                message.clone(),
                self.body_delay,
            );
        }
    }

    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        match self.inbox.lock().unwrap().try_recv() {
            Ok(o_m) => Some(o_m),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => panic!(),
        }
    }
}

/// Deliver `message` to `peer_tx`, applying `body_delay` (if any) to
/// body-carrying messages only. Delayed delivery uses one short-lived timer
/// thread per body — bodies flow at most a few per view cycle in these tests,
/// so this stays trivially cheap and preserves per-message ordering where it
/// matters (headers/votes are never delayed, so consensus-critical ordering
/// is untouched; bodies only gate insertion, which is retry-robust).
fn deliver(
    origin: VerifyingKey,
    peer_tx: Sender<(VerifyingKey, Message)>,
    message: Message,
    body_delay: Option<std::time::Duration>,
) {
    let is_body = matches!(
        &message,
        Message::ProgressMessage(ProgressMessage::HotStuffMessage(
            HotStuffMessage::BlockDataResponse(_) | HotStuffMessage::Proposal(_)
        ))
    );
    match body_delay {
        Some(delay) if is_body => {
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                let _ = peer_tx.send((origin, message));
            });
        }
        _ => {
            let _ = peer_tx.send((origin, message));
        }
    }
}

/// Create a vector of `NetworkStub`s, connecting the provided set of `peers`.
///
/// `NetworkStub`s feature in the returned vector in the same order as the provided `peers`, i.e.,
/// the i-th network stub is the network stub for the i-th peer.
pub(crate) fn mock_network(peers: impl Iterator<Item = VerifyingKey>) -> Vec<NetworkStub> {
    // Discard the filter handle: callers of this (unfiltered) constructor never toggle it, so the
    // shared `MessageFilter` stays `enabled == false` forever — a fully healthy network.
    mock_network_with_filter(peers).0
}

/// A cluster-wide handle for toggling the shared [`MessageFilter`] at runtime.
///
/// Cloning the handle is cheap (it just clones two `Arc`s) and every clone — including the copies
/// held by the `NetworkStub`s — refers to the same shared state. The [`filter_enabled`] atomic keeps
/// the *disabled* hot path lock-free; only [`starve`](Self::starve)/[`heal`](Self::heal) take the
/// mutex, and they run once per phase, not once per message.
#[derive(Clone)]
pub(crate) struct FilterHandle {
    enabled: Arc<AtomicBool>,
    filter: Arc<Mutex<MessageFilter>>,
}

impl FilterHandle {
    /// Activate the filter: drop block bodies (and, if `drop_headers`, proposal headers) addressed to
    /// `starved` recipients. Takes effect for the whole cluster immediately.
    pub(crate) fn starve(&self, starved: Vec<VerifyingKey>, drop_headers: bool) {
        *self.filter.lock().unwrap() = MessageFilter {
            enabled: true,
            starved,
            drop_headers,
        };
        self.enabled.store(true, Ordering::Relaxed);
    }

    /// Fully heal the network: nothing is dropped and the message hot path takes no lock again.
    pub(crate) fn heal(&self) {
        self.enabled.store(false, Ordering::Relaxed);
        *self.filter.lock().unwrap() = MessageFilter::default();
    }
}

/// Like [`mock_network`], but also returns a [`FilterHandle`] for toggling the cluster's shared
/// [`MessageFilter`] at runtime. Used by the S426 livelock repro to deny block bodies/headers to a
/// subset of nodes at a precise moment and then heal the network.
pub(crate) fn mock_network_with_filter(
    peers: impl Iterator<Item = VerifyingKey>,
) -> (Vec<NetworkStub>, FilterHandle) {
    let enabled = Arc::new(AtomicBool::new(false));
    let filter = Arc::new(Mutex::new(MessageFilter::default()));

    let mut all_peers = HashMap::new();
    let peer_and_inboxes: Vec<(VerifyingKey, Receiver<(VerifyingKey, Message)>)> = peers
        .map(|peer| {
            let (sender, receiver) = mpsc::channel();
            all_peers.insert(peer, sender);

            (peer, receiver)
        })
        .collect();

    let stubs = peer_and_inboxes
        .into_iter()
        .map(|(my_verifying_key, inbox)| NetworkStub {
            my_verifying_key,
            all_peers: all_peers.clone(),
            inbox: Arc::new(Mutex::new(inbox)),
            body_delay: None,
            filter_enabled: enabled.clone(),
            filter: filter.clone(),
        })
        .collect();

    (stubs, FilterHandle { enabled, filter })
}

/// S470 wedge repro: like [`mock_network`], but every body-carrying message
/// (`BlockDataResponse`, full `Proposal`) to every recipient is delivered
/// `body_delay` after it was sent, while headers/votes/pacemaker traffic stay
/// instant. See the `body_delay` field docs on [`NetworkStub`] for why this is
/// the faithful scale-down of the production wedge (an in-thread
/// `validate_block` sleep would FREEZE the sleeping replica's view clock and
/// mask the wedge; a delayed body keeps the view clock live, which is the
/// wedge's precondition).
#[allow(dead_code)]
pub(crate) fn mock_network_with_body_delay(
    peers: impl Iterator<Item = VerifyingKey>,
    body_delay: std::time::Duration,
) -> Vec<NetworkStub> {
    let (mut stubs, _filter) = mock_network_with_filter(peers);
    for stub in stubs.iter_mut() {
        stub.body_delay = Some(body_delay);
    }
    stubs
}
