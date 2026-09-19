//! Peer scoring and banning for DoS resilience (Phase 3: 3.1.7).
//!
//! Tracks peer behavior and bans peers that fall below score thresholds.
//! Ban list is persisted as a JSON file in the data directory.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
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

#[derive(Default)]
struct PendingBans {
    latest: Option<Vec<BanEntry>>,
    stopping: bool,
}

/// At most one active write and one replaceable pending snapshot. The owner
/// starts this worker only on its first permanent-ban save.
struct BanWriter {
    pending: Arc<(Mutex<PendingBans>, Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BanWriter {
    fn spawn(
        mut write: impl FnMut(&[BanEntry]) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let pending = Arc::new((Mutex::new(PendingBans::default()), Condvar::new()));
        let worker_pending = pending.clone();
        let thread = std::thread::Builder::new()
            .name("torus-ban-writer".into())
            .spawn(move || loop {
                let snapshot = {
                    let (lock, wake) = &*worker_pending;
                    let mut pending = lock.lock().unwrap();
                    while pending.latest.is_none() && !pending.stopping {
                        pending = wake.wait(pending).unwrap();
                    }
                    match pending.latest.take() {
                        Some(snapshot) => snapshot,
                        None => break,
                    }
                };
                // No lock is held during serialization or file I/O.
                if let Err(e) = write(&snapshot) {
                    tracing::warn!(%e, "failed to save ban list");
                }
            })?;
        Ok(Self {
            pending,
            thread: Some(thread),
        })
    }

    fn submit(&self, snapshot: Vec<BanEntry>) {
        let (lock, wake) = &*self.pending;
        let superseded = lock.lock().unwrap().latest.replace(snapshot);
        // Drop a superseded allocation outside the worker's critical section.
        drop(superseded);
        wake.notify_one();
    }
}

impl Drop for BanWriter {
    fn drop(&mut self) {
        let (lock, wake) = &*self.pending;
        lock.lock().unwrap().stopping = true;
        wake.notify_one();
        // Only owner shutdown waits for I/O. Drain the newest queued snapshot
        // before returning, rather than leaving a detached writer behind.
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!("ban-list writer panicked");
            }
        }
    }
}

/// Stage in the target directory, then replace the visible file atomically.
/// This guarantees complete-file visibility, not power-loss durability (no fsync).
fn replace_ban_file(
    path: &Path,
    write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let filename = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ban file needs a filename"))?;
    let (temp_path, mut file) = loop {
        let mut name = filename.to_os_string();
        name.push(format!(
            ".tmp-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let temp_path = parent.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => break (temp_path, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = write(&mut file);
    drop(file);
    let result = result.and_then(|()| std::fs::rename(&temp_path, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

fn write_bans(path: &Path, entries: &[BanEntry]) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(entries).map_err(io::Error::other)?;
    replace_ban_file(path, |file| file.write_all(&json))
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
    ban_writer: Option<BanWriter>,
}

impl PeerScoring {
    pub fn new(ban_file: Option<PathBuf>) -> Self {
        let mut scoring = Self {
            scores: HashMap::new(),
            temp_bans: HashMap::new(),
            perm_bans: HashMap::new(),
            ban_file,
            ban_writer: None,
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
    /// One lazy background worker serializes writes and coalesces queued
    /// snapshots, keeping file I/O off the async event loop.
    fn save_bans(&mut self) {
        if self.ban_writer.is_none() {
            let Some(path) = self.ban_file.clone() else {
                return;
            };
            match BanWriter::spawn(move |entries| write_bans(&path, entries)) {
                Ok(writer) => self.ban_writer = Some(writer),
                Err(e) => {
                    tracing::warn!(%e, "failed to start ban-list writer");
                    return;
                }
            }
        }
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

        self.ban_writer.as_ref().unwrap().submit(entries);
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
            scoring.penalize(&test_peer(9), 200, "second peer");
            scoring.penalize(&peer, 20, "latest reason");
        }

        // Owner shutdown drains the latest snapshot; no existence polling or
        // sleeps can mistake a newly-created file for a completed write.
        let mut scoring2 = PeerScoring::new(Some(ban_file));
        let peer = test_peer(8);
        assert!(scoring2.is_banned(&peer));
        assert!(scoring2.is_banned(&test_peer(9)));
        assert_eq!(scoring2.perm_bans[&peer], "latest reason");
        assert!(
            scoring2.ban_writer.is_none(),
            "loading does not start a writer"
        );
    }

    fn ban_snapshot(reason: &str) -> Vec<BanEntry> {
        vec![BanEntry {
            peer_id: test_peer(1).to_string(),
            permanent: true,
            banned_at: 1,
            expires_at: None,
            reason: reason.to_owned(),
        }]
    }

    #[test]
    fn ban_writer_starts_only_for_configured_permanent_bans() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        let mut scoring = PeerScoring::new(Some(path.clone()));
        assert!(scoring.ban_writer.is_none());
        scoring.penalize(&test_peer(1), 100, "temporary");
        assert!(scoring.ban_writer.is_none());
        assert!(!path.exists());
        scoring.penalize(&test_peer(2), 200, "permanent");
        assert!(scoring.ban_writer.is_some());
        drop(scoring);
        let entries: Vec<BanEntry> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].peer_id, test_peer(2).to_string());
        let mut memory_only = PeerScoring::new(None);
        memory_only.penalize(&test_peer(3), 200, "no persistence configured");
        assert!(memory_only.ban_writer.is_none());
        assert!(memory_only.is_banned(&test_peer(3)));
    }

    #[test]
    fn ban_writer_coalesces_latest_and_drains_after_success_or_failure() {
        use std::sync::mpsc::sync_channel;
        for fail_first in [false, true] {
            let (entered_tx, entered_rx) = sync_channel(1);
            let (release_tx, release_rx) = sync_channel(1);
            let calls = Arc::new(Mutex::new(Vec::new()));
            let observed = calls.clone();
            let mut first = true;
            let writer = BanWriter::spawn(move |entries| {
                observed
                    .lock()
                    .unwrap()
                    .push((entries[0].reason.clone(), std::thread::current().id()));
                if first {
                    first = false;
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                    if fail_first {
                        return Err(io::Error::other("injected write failure"));
                    }
                }
                Ok(())
            })
            .unwrap();
            writer.submit(ban_snapshot("active"));
            entered_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            // The writer is held inside its first I/O call. Submissions must
            // stay nonblocking and retain only the newest pending snapshot.
            for i in 0..100 {
                writer.submit(ban_snapshot(&format!("pending-{i}")));
            }
            let pending_reason = writer.pending.0.lock().unwrap().latest.as_ref().unwrap()[0]
                .reason
                .clone();
            release_tx.send(()).unwrap();
            drop(writer); // Includes the pending latest snapshot, not just active I/O.
            assert_eq!(pending_reason, "pending-99");
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, "active");
            assert_eq!(calls[1].0, "pending-99");
            assert_eq!(calls[0].1, calls[1].1);
            assert_ne!(calls[0].1, std::thread::current().id());
        }
    }

    #[test]
    fn ban_file_replacement_preserves_complete_old_file_until_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        write_bans(&path, &ban_snapshot("old")).unwrap();
        let old = std::fs::read(&path).unwrap();
        let error = replace_ban_file(&path, |file| {
            file.write_all(b"partial JSON")?;
            assert_eq!(std::fs::read(&path).unwrap(), old);
            Err(io::Error::other("injected partial write failure"))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(std::fs::read(&path).unwrap(), old);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "failed temporary file removed"
        );
        let next = serde_json::to_vec_pretty(&ban_snapshot("new")).unwrap();
        replace_ban_file(&path, |file| {
            file.write_all(&next)?;
            assert_eq!(std::fs::read(&path).unwrap(), old);
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), next);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn ban_file_failed_rename_cleans_temp_and_writer_can_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        std::fs::create_dir(&path).unwrap();
        assert!(write_bans(&path, &ban_snapshot("blocked")).is_err());
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        std::fs::remove_dir(&path).unwrap();
        write_bans(&path, &ban_snapshot("recovered")).unwrap();
        let entries: Vec<BanEntry> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(entries[0].reason, "recovered");
    }
}
