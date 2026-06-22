//! Native action pool with per-sender tracking, dedup, and size limits.
//!
//! Task 3.1.4: Hardens the native pool that previously had no per-sender
//! limits and no dedup. Adds pool size cap with priority-based eviction.

use std::collections::{HashMap, HashSet};

use alloy_primitives::{Address, B256};
use torus_types::{compute_action_hash, NativeAction, SignedNativeAction};

use crate::error::MempoolError;

/// Entry in the native action pool with pre-recovered sender and dedup hash.
pub(crate) struct NativePoolEntry {
    pub sender: Address,
    pub action: SignedNativeAction,
    pub action_hash: B256,
    pub is_cancel: bool,
    /// bincode-encoded size of `action` — the bytes this entry contributes to a
    /// block body (bodies are bincode, app.rs `block_bytes`). Computed once at
    /// insert so byte-capped selection is O(1) per entry.
    pub encoded_len: usize,
    /// True iff THIS node verified the signature and resolved `sender` from it
    /// (RPC ingress or gossip-RECOVER) — the only entries eligible to seed the
    /// exec trust-cache. False for gossip-TRUSTED admits (sender claimed by a
    /// peer, not re-derived here), which must never be short-circuited at exec.
    /// See `docs/plans/double-verify-trust-cache-impl.md`.
    pub verified_locally: bool,
}

/// Native action pool with per-sender tracking, dedup, and size limits.
pub(crate) struct NativePool {
    entries: Vec<NativePoolEntry>,
    sender_counts: HashMap<Address, usize>,
    seen: HashSet<(Address, B256)>,
    hash_index: HashMap<B256, usize>,
    max_size: usize,
    max_per_sender: usize,
    max_per_block: usize,
}

impl NativePool {
    pub fn new(max_size: usize, max_per_sender: usize, max_per_block: usize) -> Self {
        Self {
            entries: Vec::new(),
            sender_counts: HashMap::new(),
            seen: HashSet::new(),
            hash_index: HashMap::new(),
            max_size,
            max_per_sender,
            max_per_block,
        }
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// True when the pool is at capacity — non-cancel inserts are guaranteed
    /// to fail (cancels may still evict their way in).
    pub fn is_full(&self) -> bool {
        self.entries.len() >= self.max_size
    }

    pub fn get_by_hash(&self, hash: &B256) -> Option<SignedNativeAction> {
        self.hash_index
            .get(hash)
            .and_then(|&idx| self.entries.get(idx))
            .map(|e| e.action.clone())
    }

    /// For each committed `hash` still present in the pool that THIS node locally
    /// verified, return its `(verified_cache_key, sender)` so the caller can
    /// refresh-stash the verified sender into the exec trust-cache BEFORE the entry
    /// is pruned — bridging the lag before the (slower) exec thread reads it.
    /// Skips entries not locally verified or whose signature kind isn't cacheable
    /// (`verified_cache_key` returns `None`).
    pub fn verified_restash_keys(&self, hashes: &[B256]) -> Vec<(B256, Address)> {
        let mut out = Vec::new();
        for h in hashes {
            if let Some(&idx) = self.hash_index.get(h) {
                if let Some(entry) = self.entries.get(idx) {
                    if entry.verified_locally {
                        if let Some(key) = torus_types::verified_cache_key(&entry.action) {
                            out.push((key, entry.sender));
                        }
                    }
                }
            }
        }
        out
    }

    /// Insert a native action with a known sender.
    ///
    /// Checks: dedup by (sender, action_hash), per-sender pool cap, total pool cap.
    /// When pool is full and a cancel arrives, evicts a lowest-priority non-cancel.
    /// Entries inserted via this 2-arg form are NOT marked locally-verified (the
    /// conservative default); the verified ingress/recover paths call
    /// [`NativePool::insert_verified`] so they can seed the exec trust-cache.
    pub fn insert(
        &mut self,
        sender: Address,
        action: SignedNativeAction,
    ) -> Result<B256, MempoolError> {
        self.insert_verified(sender, action, false)
    }

    /// Insert with explicit provenance. `verified_locally` records whether THIS
    /// node verified the signature and resolved `sender` from it (so the entry is
    /// eligible for the exec trust-cache). Returns the action hash on success.
    pub fn insert_verified(
        &mut self,
        sender: Address,
        action: SignedNativeAction,
        verified_locally: bool,
    ) -> Result<B256, MempoolError> {
        let action_hash = compute_action_hash(&action);
        let is_cancel = is_cancel(&action.action);
        // Serialization of a serde struct cannot realistically fail; if it ever
        // does, a half-cap sentinel keeps the entry out of byte-capped blocks
        // without overflowing the selection sum.
        let encoded_len = bincode::serialized_size(&action)
            .map(|n| n as usize)
            .unwrap_or(usize::MAX / 2);

        // Dedup: reject identical (sender, action_hash).
        if self.seen.contains(&(sender, action_hash)) {
            return Err(MempoolError::DuplicateNativeAction);
        }

        // Per-sender pool cap.
        let count = self.sender_counts.get(&sender).copied().unwrap_or(0);
        if count >= self.max_per_sender {
            return Err(MempoolError::NativeSenderQueueFull { sender });
        }

        // Pool size cap with priority-based eviction.
        if self.entries.len() >= self.max_size {
            if is_cancel {
                // High-priority: evict a non-cancel action.
                if let Some(idx) = self.entries.iter().rposition(|e| !e.is_cancel) {
                    let evicted = self.entries.swap_remove(idx);
                    self.dec_sender_count(&evicted.sender);
                    self.seen.remove(&(evicted.sender, evicted.action_hash));
                    self.hash_index.remove(&evicted.action_hash);
                    if idx < self.entries.len() {
                        self.hash_index.insert(self.entries[idx].action_hash, idx);
                    }
                } else {
                    return Err(MempoolError::NativePoolFull);
                }
            } else {
                return Err(MempoolError::NativePoolFull);
            }
        }

        *self.sender_counts.entry(sender).or_insert(0) += 1;
        self.seen.insert((sender, action_hash));
        let idx = self.entries.len();
        self.entries.push(NativePoolEntry {
            sender,
            action,
            action_hash,
            is_cancel,
            encoded_len,
            verified_locally,
        });
        self.hash_index.insert(action_hash, idx);

        Ok(action_hash)
    }

    /// Drain up to `limit` actions in priority order with per-sender-per-block caps.
    ///
    /// Cancellations first (highest priority), then remaining actions.
    /// Within each priority group, ordered by (sender, nonce) for determinism.
    /// Excess actions from rate-limited senders stay in pool for next block.
    pub fn drain(&mut self, limit: usize) -> Vec<SignedNativeAction> {
        // Sort: cancels first, then by (sender, nonce) for determinism.
        self.entries.sort_by(|a, b| {
            let a_pri = if a.is_cancel { 0u8 } else { 1 };
            let b_pri = if b.is_cancel { 0u8 } else { 1 };
            a_pri
                .cmp(&b_pri)
                .then_with(|| a.sender.cmp(&b.sender))
                .then_with(|| a.action.nonce.cmp(&b.action.nonce))
        });

        // Take ownership of all entries, then partition into taken/remaining.
        let all_entries = std::mem::take(&mut self.entries);
        self.hash_index.clear();
        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut taken = Vec::new();

        for entry in all_entries {
            if taken.len() < limit {
                let count = block_counts.get(&entry.sender).copied().unwrap_or(0);
                if count < self.max_per_block {
                    *block_counts.entry(entry.sender).or_insert(0) += 1;
                    self.dec_sender_count(&entry.sender);
                    self.seen.remove(&(entry.sender, entry.action_hash));
                    taken.push(entry.action);
                    continue;
                }
            }
            // Not taken — put back in pool.
            let idx = self.entries.len();
            self.hash_index.insert(entry.action_hash, idx);
            self.entries.push(entry);
        }

        taken
    }

    /// Select up to `limit` actions for a block WITHOUT removing them from the pool.
    /// Actions stay in the pool until `remove_committed` is called after commit.
    /// Uses the same priority order as `drain`.
    pub fn select_for_block(&mut self, limit: usize) -> Vec<SignedNativeAction> {
        self.entries.sort_by(|a, b| {
            let a_pri = if a.is_cancel { 0u8 } else { 1 };
            let b_pri = if b.is_cancel { 0u8 } else { 1 };
            a_pri
                .cmp(&b_pri)
                .then_with(|| a.sender.cmp(&b.sender))
                .then_with(|| a.action.nonce.cmp(&b.action.nonce))
        });
        // Rebuild hash_index after sort
        self.hash_index.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            self.hash_index.insert(entry.action_hash, i);
        }

        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut selected = Vec::new();

        for entry in &self.entries {
            if selected.len() >= limit {
                break;
            }
            let count = block_counts.get(&entry.sender).copied().unwrap_or(0);
            if count < self.max_per_block {
                *block_counts.entry(entry.sender).or_insert(0) += 1;
                selected.push(entry.action.clone());
            }
        }

        selected
    }

    pub fn select_for_block_with_senders(&mut self, limit: usize) -> Vec<(Address, SignedNativeAction)> {
        self.select_for_block_with_senders_excluding(limit, &HashSet::new(), usize::MAX)
    }

    /// Like `select_for_block_with_senders`, but skips any action whose hash is in
    /// `exclude`.
    ///
    /// Pipeline-aware selection: the proposer passes the action hashes of its
    /// proposed-but-uncommitted blocks (the in-flight 3-chain window) so the same
    /// action is not re-selected for blocks N+1/N+2 before N commits and calls
    /// `remove_committed`. Root-cause fix for duplicate native inclusion (memory
    /// 282f9818); recovers the ~2/3 of cap-100 block space that dups wasted. Excluded
    /// actions stay in the pool and become selectable again once their in-flight block
    /// commits (removing them) or is evicted from the proposer window.
    /// `bytes_cap` bounds the summed bincode-encoded size of selected actions —
    /// the WAN dissemination budget per block body. Selection stops at the first
    /// entry that would exceed it (deterministic prefix), so a flood of huge
    /// batch actions degrades to more, smaller blocks instead of an
    /// undisseminatable mega-block (s334 bs1000 wedge).
    pub fn select_for_block_with_senders_excluding(
        &mut self,
        limit: usize,
        exclude: &HashSet<B256>,
        bytes_cap: usize,
    ) -> Vec<(Address, SignedNativeAction)> {
        self.entries.sort_by(|a, b| {
            let a_pri = if a.is_cancel { 0u8 } else { 1 };
            let b_pri = if b.is_cancel { 0u8 } else { 1 };
            a_pri
                .cmp(&b_pri)
                .then_with(|| a.sender.cmp(&b.sender))
                .then_with(|| a.action.nonce.cmp(&b.action.nonce))
        });
        self.hash_index.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            self.hash_index.insert(entry.action_hash, i);
        }

        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut selected = Vec::new();
        let mut bytes_used: usize = 0;

        for entry in &self.entries {
            if selected.len() >= limit {
                break;
            }
            if bytes_used.saturating_add(entry.encoded_len) > bytes_cap {
                // Deterministic prefix: stop at the first entry that would blow
                // the body-byte budget rather than skipping past it.
                break;
            }
            if exclude.contains(&entry.action_hash) {
                continue;
            }
            let count = block_counts.get(&entry.sender).copied().unwrap_or(0);
            if count < self.max_per_block {
                *block_counts.entry(entry.sender).or_insert(0) += 1;
                bytes_used = bytes_used.saturating_add(entry.encoded_len);
                selected.push((entry.sender, entry.action.clone()));
            }
        }

        selected
    }

    /// Evict entries whose nonce has aged out of the protocol validity window.
    ///
    /// An action with `nonce + NONCE_WINDOW_MS < now_ms` can never pass
    /// admission/validation again, so keeping it selectable only lets leaders
    /// propose blocks that cannot validate — the s334 bs1000 wedge had no
    /// self-heal precisely because nothing ever removed these. Called lazily
    /// from the selection/drain wrappers. Returns the number evicted.
    pub fn evict_expired(&mut self, now_ms: u64) -> usize {
        use torus_types::eip712::NONCE_WINDOW_MS;
        let mut removed = Vec::new();
        self.entries.retain(|entry| {
            if entry.action.nonce.saturating_add(NONCE_WINDOW_MS) < now_ms {
                removed.push((entry.sender, entry.action_hash));
                false
            } else {
                true
            }
        });
        for (sender, hash) in &removed {
            self.seen.remove(&(*sender, *hash));
            self.dec_sender_count(sender);
        }
        self.hash_index.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            self.hash_index.insert(entry.action_hash, i);
        }
        removed.len()
    }

    /// Remove actions that were included in a committed block.
    pub fn remove_committed(&mut self, hashes: &[B256]) {
        let to_remove: HashSet<B256> = hashes.iter().copied().collect();
        let mut removed_entries = Vec::new();

        self.entries.retain(|entry| {
            if to_remove.contains(&entry.action_hash) {
                removed_entries.push((entry.sender, entry.action_hash));
                false
            } else {
                true
            }
        });

        for (sender, hash) in &removed_entries {
            self.seen.remove(&(*sender, *hash));
            self.dec_sender_count(sender);
        }

        self.hash_index.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            self.hash_index.insert(entry.action_hash, i);
        }
    }

    /// Re-insert previously drained actions (e.g., after block reorg).
    /// Recovers senders from signatures; silently drops invalid.
    pub fn reinsert(&mut self, actions: Vec<SignedNativeAction>) {
        for action in actions {
            if let Ok(sender) = action.recover_sender() {
                let _ = self.insert(sender, action);
            }
        }
    }

    fn dec_sender_count(&mut self, sender: &Address) {
        if let Some(count) = self.sender_counts.get_mut(sender) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.sender_counts.remove(sender);
            }
        }
    }
}

/// Cancels get pool-eviction priority; exported so ingress can route them to
/// full verification even when the pool is full (pre-verify shedding).
pub fn is_cancel(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::CancelOrder { .. } | NativeAction::CancelAllOrders { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use torus_types::{ActionSignature, NativeAction, Signature, SignedNativeAction};

    fn sig() -> ActionSignature {
        ActionSignature::Eip712(Signature {
            v: 27,
            r: [0u8; 32],
            s: [0u8; 32],
        })
    }

    fn make_action(nonce: u64, action: NativeAction) -> SignedNativeAction {
        SignedNativeAction {
            action,
            nonce,
            signature: sig(),
        }
    }

    #[test]
    fn insert_and_drain() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);

        pool.insert(sender, make_action(1, NativeAction::ClaimRewards))
            .unwrap();
        pool.insert(
            sender,
            make_action(2, NativeAction::CancelOrder { order_id: 42 }),
        )
        .unwrap();
        assert_eq!(pool.size(), 2);

        let drained = pool.drain(10);
        assert_eq!(drained.len(), 2);
        // Cancel should come first.
        assert!(matches!(
            drained[0].action,
            NativeAction::CancelOrder { .. }
        ));
        assert_eq!(pool.size(), 0);
    }

    #[test]
    fn byte_cap_bounds_selection() {
        let mut pool = NativePool::new(100, 64, 16);
        // Distinct senders so per-sender caps don't interfere; identical action
        // shape so every entry has the same encoded size.
        let mut per_action: usize = 0;
        for i in 0..10u8 {
            let sender = Address::repeat_byte(i + 1);
            let action = make_action(1_000_000 + i as u64, NativeAction::ClaimRewards);
            per_action = bincode::serialized_size(&action).unwrap() as usize;
            pool.insert(sender, action).unwrap();
        }
        // Budget for exactly 3.5 actions -> 3 selected.
        let cap = per_action * 7 / 2;
        let selected =
            pool.select_for_block_with_senders_excluding(100, &HashSet::new(), cap);
        assert_eq!(selected.len(), 3);
        // usize::MAX preserves uncapped behavior.
        let all =
            pool.select_for_block_with_senders_excluding(100, &HashSet::new(), usize::MAX);
        assert_eq!(all.len(), 10);
    }

    #[test]
    fn evict_expired_purges_stale_entries() {
        use torus_types::eip712::NONCE_WINDOW_MS;
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        let now: u64 = 10 * NONCE_WINDOW_MS;
        let stale = make_action(now - 2 * NONCE_WINDOW_MS, NativeAction::ClaimRewards);
        let fresh = make_action(now, NativeAction::CancelOrder { order_id: 7 });
        let stale_hash = compute_action_hash(&stale);
        pool.insert(sender, stale.clone()).unwrap();
        pool.insert(sender, fresh).unwrap();
        assert_eq!(pool.size(), 2);

        let evicted = pool.evict_expired(now);
        assert_eq!(evicted, 1);
        assert_eq!(pool.size(), 1);
        assert!(pool.get_by_hash(&stale_hash).is_none());
        // seen/sender_counts cleaned: the same (sender, action) is insertable again.
        pool.insert(sender, stale).unwrap();
        assert_eq!(pool.size(), 2);
    }

    #[test]
    fn dedup_rejects_duplicate() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        let action = make_action(1, NativeAction::ClaimRewards);

        pool.insert(sender, action.clone()).unwrap();
        let err = pool.insert(sender, action).unwrap_err();
        assert!(matches!(err, MempoolError::DuplicateNativeAction));
    }

    #[test]
    fn per_sender_cap() {
        let mut pool = NativePool::new(100, 2, 16);
        let sender = Address::repeat_byte(1);

        pool.insert(sender, make_action(1, NativeAction::ClaimRewards))
            .unwrap();
        pool.insert(
            sender,
            make_action(2, NativeAction::CancelOrder { order_id: 1 }),
        )
        .unwrap();
        let err = pool
            .insert(
                sender,
                make_action(3, NativeAction::CancelOrder { order_id: 2 }),
            )
            .unwrap_err();
        assert!(matches!(err, MempoolError::NativeSenderQueueFull { .. }));
    }

    #[test]
    fn is_full_tracks_cap() {
        let mut pool = NativePool::new(2, 64, 16);
        assert!(!pool.is_full());
        pool.insert(
            Address::repeat_byte(1),
            make_action(1, NativeAction::ClaimRewards),
        )
        .unwrap();
        assert!(!pool.is_full());
        pool.insert(
            Address::repeat_byte(2),
            make_action(2, NativeAction::ClaimRewards),
        )
        .unwrap();
        assert!(pool.is_full());
    }

    #[test]
    fn pool_size_cap_rejects_non_cancel() {
        let mut pool = NativePool::new(2, 64, 16);
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let c = Address::repeat_byte(3);

        pool.insert(a, make_action(1, NativeAction::ClaimRewards))
            .unwrap();
        pool.insert(b, make_action(2, NativeAction::ClaimRewards))
            .unwrap();
        let err = pool
            .insert(c, make_action(3, NativeAction::ClaimRewards))
            .unwrap_err();
        assert!(matches!(err, MempoolError::NativePoolFull));
    }

    #[test]
    fn pool_size_cap_cancel_evicts_non_cancel() {
        let mut pool = NativePool::new(2, 64, 16);
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let c = Address::repeat_byte(3);

        pool.insert(a, make_action(1, NativeAction::ClaimRewards))
            .unwrap();
        pool.insert(b, make_action(2, NativeAction::ClaimRewards))
            .unwrap();
        pool.insert(
            c,
            make_action(3, NativeAction::CancelOrder { order_id: 1 }),
        )
        .unwrap();
        assert_eq!(pool.size(), 2);
    }

    #[test]
    fn per_block_cap_defers_excess() {
        let mut pool = NativePool::new(100, 64, 2);
        let sender = Address::repeat_byte(1);

        for i in 1..=5 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards))
                .unwrap();
        }
        assert_eq!(pool.size(), 5);

        let drained = pool.drain(10);
        assert_eq!(drained.len(), 2);
        assert_eq!(pool.size(), 3);
    }

    #[test]
    fn get_by_hash_returns_action() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        let action = make_action(1, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&action);

        pool.insert(sender, action.clone()).unwrap();
        let found = pool.get_by_hash(&hash);
        assert!(found.is_some());
        assert_eq!(found.unwrap().nonce, 1);

        assert!(pool.get_by_hash(&B256::ZERO).is_none());
    }

    #[test]
    fn select_for_block_does_not_drain() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        for i in 1..=5u64 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards)).unwrap();
        }
        assert_eq!(pool.size(), 5);

        let selected = pool.select_for_block(3);
        assert_eq!(selected.len(), 3);
        assert_eq!(pool.size(), 5, "select_for_block must not remove entries");

        let hash1 = compute_action_hash(&selected[0]);
        assert!(pool.get_by_hash(&hash1).is_some(), "selected action still in pool");
    }

    #[test]
    fn remove_committed_cleans_pool() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        for i in 1..=5u64 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards)).unwrap();
        }
        let selected = pool.select_for_block(3);
        let hashes: Vec<B256> = selected.iter().map(|a| compute_action_hash(a)).collect();

        pool.remove_committed(&hashes);
        assert_eq!(pool.size(), 2, "3 committed actions removed, 2 remain");

        for h in &hashes {
            assert!(pool.get_by_hash(h).is_none(), "committed action removed from hash_index");
        }
    }

    #[test]
    fn hash_index_survives_drain() {
        let mut pool = NativePool::new(100, 64, 2);
        let sender = Address::repeat_byte(1);
        for i in 1..=4u64 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards)).unwrap();
        }
        let action4_hash = compute_action_hash(&make_action(4, NativeAction::ClaimRewards));

        let drained = pool.drain(10);
        assert_eq!(drained.len(), 2);
        assert_eq!(pool.size(), 2);

        let remaining = pool.get_by_hash(&action4_hash);
        assert!(remaining.is_some());
    }

    #[test]
    fn hash_index_updated_on_eviction() {
        let mut pool = NativePool::new(2, 64, 16);
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let c = Address::repeat_byte(3);

        let action_a = make_action(1, NativeAction::ClaimRewards);
        let action_b = make_action(2, NativeAction::ClaimRewards);
        let action_c = make_action(3, NativeAction::CancelOrder { order_id: 1 });
        let hash_c = compute_action_hash(&action_c);

        pool.insert(a, action_a).unwrap();
        pool.insert(b, action_b).unwrap();
        pool.insert(c, action_c).unwrap();
        assert_eq!(pool.size(), 2);

        let found = pool.get_by_hash(&hash_c);
        assert!(found.is_some());
    }

    #[test]
    fn multiple_senders_per_block_cap() {
        let mut pool = NativePool::new(100, 64, 2);
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);

        for i in 1..=3 {
            pool.insert(a, make_action(i, NativeAction::ClaimRewards))
                .unwrap();
            pool.insert(
                b,
                make_action(i + 100, NativeAction::CancelOrder { order_id: i as u128 }),
            )
            .unwrap();
        }
        assert_eq!(pool.size(), 6);

        // Each sender gets at most 2: 2 from B (cancels first) + 2 from A = 4.
        let drained = pool.drain(10);
        assert_eq!(drained.len(), 4);
        assert_eq!(pool.size(), 2);
    }

    #[test]
    fn select_excluding_skips_in_flight_actions() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        for i in 1..=5u64 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards))
                .unwrap();
        }

        // Treat a first proposal's actions as in-flight; they must not be re-selected
        // while still in the proposer window (duplicate-inclusion root cause).
        let first = pool.select_for_block_with_senders(5);
        assert_eq!(first.len(), 5);
        let exclude: HashSet<B256> = first.iter().map(|(_, a)| compute_action_hash(a)).collect();

        let second = pool.select_for_block_with_senders_excluding(5, &exclude, usize::MAX);
        assert!(second.is_empty(), "in-flight actions must not be re-selected");
        assert_eq!(pool.size(), 5, "selection stays non-destructive");

        // A fresh (non-excluded) action is still selectable past the exclusion set.
        pool.insert(sender, make_action(99, NativeAction::ClaimRewards))
            .unwrap();
        let third = pool.select_for_block_with_senders_excluding(5, &exclude, usize::MAX);
        assert_eq!(third.len(), 1, "only the fresh action is selected");
        assert_eq!(third[0].1.nonce, 99);
    }
}
