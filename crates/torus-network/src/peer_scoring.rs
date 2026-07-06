//! Peer scoring and banning for DoS resilience (Phase 3: 3.1.7).
//!
//! Tracks peer behavior and bans peers that fall below score thresholds.
//! Ban list is persisted as a JSON file in the data directory.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use libp2p::PeerId;
use serde::{Deserialize, Serialize};

/// Starting score for new peers.
pub const INITIAL_SCORE: i64 = 100;
/// Temporary ban threshold (score <= 0).
pub const TEMP_BAN_THRESHOLD: i64 = 0;
/// Permanent ban threshold (score <= -50).
pub const PERM_BAN_THRESHOLD: i64 = -50;
/// Duration of a temporary ban.
pub const TEMP_BAN_DURATION: Duration = Duration::from_secs(3600); // 1 hour

/// Score penalties for various offenses.
pub const PENALTY_INVALID_CONSENSUS_MSG: i64 = 20;
pub const PENALTY_INVALID_TX: i64 = 5;
pub const PENALTY_EXCESSIVE_RATE: i64 = 10;
/// Score reward for positive behavior.
pub const REWARD_BLOCK_RELAY: i64 = 1;

/// Persistent ban entry stored in JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BanEntry {
    pub peer_id: String,
    pub permanent: bool,
    pub banned_at: u64,
    pub expires_at: Option<u64>,
    pub reason: String,
}

/// Per-peer score tracking.
struct PeerScore {
    score: i64,
    last_update: Instant,
}

/// Token bucket state for a single peer.
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

/// Consensus message rate limiter per peer using token bucket algorithm
/// (Batch EK: CONS-FIND-19). Prevents 2x burst that tumbling windows allow.
pub struct ConsensusRateLimiter {
    buckets: HashMap<PeerId, TokenBucket>,
    max_tokens: f64,
    refill_rate: f64, // tokens per second
}

/// Stale bucket entries older than this are cleaned up.
const RATE_LIMITER_CLEANUP_AGE: Duration = Duration::from_secs(300);

impl ConsensusRateLimiter {
    pub fn new(limit_per_sec: u32) -> Self {
        Self {
            buckets: HashMap::new(),
            max_tokens: limit_per_sec as f64,
            refill_rate: limit_per_sec as f64,
        }
    }

    /// Check if a consensus message from this peer should be accepted.
    /// Returns false if rate exceeded.
    pub fn check_and_increment(&mut self, peer: &PeerId) -> bool {
        let now = Instant::now();
        let max = self.max_tokens;
        let rate = self.refill_rate;
        let bucket = self.buckets.entry(*peer).or_insert(TokenBucket {
            tokens: max,
            last_refill: now,
        });
        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(max);
        bucket.last_refill = now;
        // Try to consume one token
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Remove stale bucket entries for peers not seen recently (Batch EK: CONS-FIND-21-24).
    pub fn cleanup_stale(&mut self) {
        let now = Instant::now();
        self.buckets
            .retain(|_, b| now.duration_since(b.last_refill) < RATE_LIMITER_CLEANUP_AGE);
    }
}

/// Peer scoring and ban management.
pub struct PeerScoring {
    scores: HashMap<PeerId, PeerScore>,
    temp_bans: HashMap<PeerId, Instant>,
    perm_bans: HashMap<PeerId, String>, // peer_id -> reason
    ban_file: Option<PathBuf>,
}

impl PeerScoring {
    pub fn new(ban_file: Option<PathBuf>) -> Self {
        let mut scoring = Self {
            scores: HashMap::new(),
            temp_bans: HashMap::new(),
            perm_bans: HashMap::new(),
            ban_file,
        };
        scoring.load_bans();
        scoring
    }

    /// Check if a peer is currently banned.
    pub fn is_banned(&mut self, peer: &PeerId) -> bool {
        // Check permanent ban
        if self.perm_bans.contains_key(peer) {
            return true;
        }
        // Check temporary ban (with expiry)
        if let Some(until) = self.temp_bans.get(peer) {
            if Instant::now() < *until {
                return true;
            }
            // Expired — remove
            self.temp_bans.remove(peer);
        }
        false
    }

    /// Get the current score for a peer.
    pub fn score(&self, peer: &PeerId) -> i64 {
        self.scores
            .get(peer)
            .map(|s| s.score)
            .unwrap_or(INITIAL_SCORE)
    }

    /// Apply a penalty to a peer. Triggers banning if thresholds are crossed.
    pub fn penalize(&mut self, peer: &PeerId, amount: i64, reason: &str) {
        let entry = self.scores.entry(*peer).or_insert(PeerScore {
            score: INITIAL_SCORE,
            last_update: Instant::now(),
        });
        entry.score -= amount;
        entry.last_update = Instant::now();

        tracing::debug!(
            %peer, penalty = amount, new_score = entry.score, %reason,
            "peer score decreased"
        );

        if entry.score <= PERM_BAN_THRESHOLD {
            self.permanent_ban(peer, reason);
        } else if entry.score <= TEMP_BAN_THRESHOLD {
            self.temporary_ban(peer, reason);
        }
    }

    /// Reward a peer for positive behavior.
    pub fn reward(&mut self, peer: &PeerId, amount: i64) {
        let entry = self.scores.entry(*peer).or_insert(PeerScore {
            score: INITIAL_SCORE,
            last_update: Instant::now(),
        });
        // Cap at initial score — don't let rewards accumulate unbounded
        entry.score = (entry.score + amount).min(INITIAL_SCORE + 20);
        entry.last_update = Instant::now();
    }

    /// Apply a temporary ban (1 hour).
    /// Note: does NOT call save_bans — temp bans are in-memory only (Instant
    /// is not serializable). Only permanent bans are persisted.
    fn temporary_ban(&mut self, peer: &PeerId, reason: &str) {
        let until = Instant::now() + TEMP_BAN_DURATION;
        self.temp_bans.insert(*peer, until);
        tracing::warn!(%peer, %reason, "peer temporarily banned (1 hour)");
    }

    /// Apply a permanent ban.
    fn permanent_ban(&mut self, peer: &PeerId, reason: &str) {
        self.perm_bans.insert(*peer, reason.to_string());
        self.temp_bans.remove(peer);
        tracing::warn!(%peer, %reason, "peer permanently banned");
        self.save_bans();
    }

    /// Get the list of permanently banned peer IDs for the block list.
    pub fn permanently_banned_peers(&self) -> Vec<PeerId> {
        self.perm_bans.keys().cloned().collect()
    }

    /// Load ban list from JSON file.
    fn load_bans(&mut self) {
        let path = match &self.ban_file {
            Some(p) => p.clone(),
            None => return,
        };
        if !path.exists() {
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(json) => {
                if let Ok(entries) = serde_json::from_str::<Vec<BanEntry>>(&json) {
                    for entry in entries {
                        if let Ok(peer_id) = entry.peer_id.parse::<PeerId>() {
                            if entry.permanent {
                                self.perm_bans.insert(peer_id, entry.reason);
                            }
                            // Temporary bans from disk are not restored (Instant is not serializable)
                        }
                    }
                    tracing::info!(
                        permanent = self.perm_bans.len(),
                        "loaded ban list from {}",
                        path.display()
                    );
                }
            }
            Err(e) => {
                tracing::warn!(%e, "failed to load ban list");
            }
        }
    }

    /// Remove stale score entries and expired temp bans (Batch EK: CONS-FIND-21-24).
    /// Should be called periodically from the event loop.
    pub fn cleanup_stale(&mut self, stale_age: Duration) {
        let now = Instant::now();
        // Remove score entries for peers not seen in a long time and not banned
        self.scores.retain(|peer, entry| {
            now.duration_since(entry.last_update) < stale_age || self.perm_bans.contains_key(peer)
        });
        // Purge expired temp bans
        self.temp_bans.retain(|_, until| now < *until);
    }

    /// Save ban list to JSON file.
    ///
    /// FIX CONS-FIND-29: File I/O is offloaded to a background OS thread via
    /// `std::thread::spawn` so it does not block the async event loop.
    fn save_bans(&self) {
        let path = match &self.ban_file {
            Some(p) => p.clone(),
            None => return,
        };
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entries: Vec<BanEntry> = self
            .perm_bans
            .iter()
            .map(|(peer, reason)| BanEntry {
                peer_id: peer.to_string(),
                permanent: true,
                banned_at: now_secs,
                expires_at: None,
                reason: reason.clone(),
            })
            .collect();

        std::thread::spawn(move || {
            if let Ok(json) = serde_json::to_string_pretty(&entries) {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!(%e, "failed to save ban list");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer(id: u8) -> PeerId {
        // Generate deterministic PeerIds for testing
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        let key = libp2p::identity::ed25519::SecretKey::try_from_bytes(bytes).unwrap();
        let keypair =
            libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(key));
        keypair.public().to_peer_id()
    }

    #[test]
    fn new_peer_has_initial_score() {
        let scoring = PeerScoring::new(None);
        let peer = test_peer(1);
        assert_eq!(scoring.score(&peer), INITIAL_SCORE);
    }

    #[test]
    fn penalty_decreases_score() {
        let mut scoring = PeerScoring::new(None);
        let peer = test_peer(2);
        scoring.penalize(&peer, PENALTY_INVALID_CONSENSUS_MSG, "test");
        assert_eq!(
            scoring.score(&peer),
            INITIAL_SCORE - PENALTY_INVALID_CONSENSUS_MSG as i64
        );
    }

    #[test]
    fn peer_temp_banned_at_zero() {
        let mut scoring = PeerScoring::new(None);
        let peer = test_peer(3);
        // Penalize enough to reach 0
        for _ in 0..5 {
            scoring.penalize(&peer, PENALTY_INVALID_CONSENSUS_MSG, "invalid msg");
        }
        assert!(scoring.is_banned(&peer));
    }

    #[test]
    fn peer_perm_banned_at_negative_50() {
        let mut scoring = PeerScoring::new(None);
        let peer = test_peer(4);
        // Penalize heavily
        for _ in 0..8 {
            scoring.penalize(&peer, PENALTY_INVALID_CONSENSUS_MSG, "invalid msg");
        }
        assert!(scoring.is_banned(&peer));
        assert!(scoring.permanently_banned_peers().contains(&peer));
    }

    #[test]
    fn reward_increases_score() {
        let mut scoring = PeerScoring::new(None);
        let peer = test_peer(5);
        scoring.penalize(&peer, 10, "minor");
        assert_eq!(scoring.score(&peer), 90);
        scoring.reward(&peer, REWARD_BLOCK_RELAY);
        assert_eq!(scoring.score(&peer), 91);
    }

    #[test]
    fn honest_peer_stays_healthy() {
        let mut scoring = PeerScoring::new(None);
        let peer = test_peer(6);
        // Simulate normal operation: many rewards, few penalties
        for _ in 0..100 {
            scoring.reward(&peer, REWARD_BLOCK_RELAY);
        }
        scoring.penalize(&peer, PENALTY_INVALID_TX, "one bad tx");
        assert!(scoring.score(&peer) > TEMP_BAN_THRESHOLD);
        assert!(!scoring.is_banned(&peer));
    }

    #[test]
    fn consensus_rate_limiter_allows_under_limit() {
        let mut limiter = ConsensusRateLimiter::new(50);
        let peer = test_peer(7);
        for _ in 0..50 {
            assert!(limiter.check_and_increment(&peer));
        }
        // 51st should be rejected (token bucket starts with 50 tokens)
        assert!(!limiter.check_and_increment(&peer));
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut limiter = ConsensusRateLimiter::new(10);
        let peer = test_peer(10);
        // Exhaust all tokens
        for _ in 0..10 {
            assert!(limiter.check_and_increment(&peer));
        }
        assert!(!limiter.check_and_increment(&peer));
        // Manually advance the last_refill to simulate time passing
        if let Some(bucket) = limiter.buckets.get_mut(&peer) {
            bucket.last_refill -= Duration::from_millis(500);
        }
        // Should have ~5 tokens refilled (10/sec * 0.5s)
        for _ in 0..5 {
            assert!(limiter.check_and_increment(&peer));
        }
        // Next should fail
        assert!(!limiter.check_and_increment(&peer));
    }

    #[test]
    fn token_bucket_no_2x_burst() {
        // The old tumbling window allowed 2x burst at window boundaries.
        // Token bucket should not: after exhausting tokens, even a small
        // time advance only refills proportionally.
        let mut limiter = ConsensusRateLimiter::new(100);
        let peer = test_peer(11);
        // Exhaust all tokens
        for _ in 0..100 {
            assert!(limiter.check_and_increment(&peer));
        }
        // Simulate 10ms passing — should refill ~1 token, not 100
        if let Some(bucket) = limiter.buckets.get_mut(&peer) {
            bucket.last_refill -= Duration::from_millis(10);
        }
        assert!(limiter.check_and_increment(&peer)); // ~1 token
        assert!(!limiter.check_and_increment(&peer)); // depleted again
    }

    #[test]
    fn rate_limiter_cleanup_stale() {
        let mut limiter = ConsensusRateLimiter::new(10);
        let active = test_peer(12);
        let stale = test_peer(13);
        limiter.check_and_increment(&active);
        limiter.check_and_increment(&stale);
        assert_eq!(limiter.buckets.len(), 2);
        // Mark stale peer's bucket as old
        if let Some(bucket) = limiter.buckets.get_mut(&stale) {
            bucket.last_refill -= RATE_LIMITER_CLEANUP_AGE + Duration::from_secs(1);
        }
        limiter.cleanup_stale();
        assert_eq!(limiter.buckets.len(), 1);
        assert!(limiter.buckets.contains_key(&active));
    }

    #[test]
    fn ban_list_persistence() {
        let dir = tempfile::TempDir::new().unwrap();
        let ban_file = dir.path().join("bans.json");

        {
            let mut scoring = PeerScoring::new(Some(ban_file.clone()));
            let peer = test_peer(8);
            // Force permanent ban
            for _ in 0..10 {
                scoring.penalize(&peer, PENALTY_INVALID_CONSENSUS_MSG, "attack");
            }
            assert!(scoring.permanently_banned_peers().contains(&peer));
        }

        // Wait for background thread to flush ban file to disk.
        for _ in 0..100 {
            if ban_file.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Reload and check persistence
        let mut scoring2 = PeerScoring::new(Some(ban_file));
        let peer = test_peer(8);
        assert!(scoring2.is_banned(&peer));
    }
}
