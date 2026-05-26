use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::PeerId;

/// Tracks seen transaction hashes for deduplication and per-peer rate limiting.
pub struct TxGossipState {
    seen: HashMap<[u8; 32], Instant>,
    dedup_window: Duration,
    peer_rates: HashMap<PeerId, PeerRateState>,
    rate_limit: u32,
}

struct PeerRateState {
    count: u32,
    window_start: Instant,
}

impl TxGossipState {
    pub fn new(dedup_window_secs: u64, rate_limit_per_peer: u32) -> Self {
        Self {
            seen: HashMap::new(),
            dedup_window: Duration::from_secs(dedup_window_secs),
            peer_rates: HashMap::new(),
            rate_limit: rate_limit_per_peer,
        }
    }

    /// Returns true if the transaction is new (not a duplicate) and within rate limits.
    pub fn should_accept(&mut self, tx_hash: [u8; 32], peer: PeerId) -> bool {
        let now = Instant::now();
        self.cleanup_expired(now);

        // Dedup check
        if self.seen.contains_key(&tx_hash) {
            return false;
        }

        // Rate limit check
        let rate = self.peer_rates.entry(peer).or_insert(PeerRateState {
            count: 0,
            window_start: now,
        });
        if now.duration_since(rate.window_start) >= Duration::from_secs(1) {
            rate.count = 0;
            rate.window_start = now;
        }
        if rate.count >= self.rate_limit {
            return false;
        }
        rate.count += 1;

        self.seen.insert(tx_hash, now);
        true
    }

    fn cleanup_expired(&mut self, now: Instant) {
        self.seen
            .retain(|_, ts| now.duration_since(*ts) < self.dedup_window);
        // Also clean up stale per-peer rate entries (Batch EK: CONS-FIND-21-24).
        self.peer_rates
            .retain(|_, rate| now.duration_since(rate.window_start) < self.dedup_window);
    }
}

/// Handle for submitting transactions to the gossip network.
#[derive(Clone)]
pub struct TxGossipHandle {
    pub(crate) tx_sender: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl TxGossipHandle {
    /// Submit a transaction for gossip. The bytes should be RLP (EVM) or borsh (native) encoded.
    pub fn submit_tx(&self, tx_bytes: Vec<u8>) -> Result<(), &'static str> {
        self.tx_sender
            .send(tx_bytes)
            .map_err(|_| "network task shut down")
    }
}

/// Handle for gossiping native actions to the validator mesh.
///
/// Uses a bounded channel (capacity 8192) with backpressure: if the swarm
/// can't keep up, actions are silently dropped rather than unbounded growth.
#[derive(Clone)]
pub struct NativeGossipHandle {
    pub(crate) sender: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl NativeGossipHandle {
    pub fn submit(&self, action_bytes: Vec<u8>) -> Result<(), &'static str> {
        self.sender
            .try_send(action_bytes)
            .map_err(|_| "native gossip channel full or closed")
    }

    pub fn into_sender(self) -> tokio::sync::mpsc::Sender<Vec<u8>> {
        self.sender
    }
}
