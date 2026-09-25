//! Native action pool with per-sender tracking, dedup, and size limits.
//!
//! Task 3.1.4: Hardens the native pool that previously had no per-sender
//! limits and no dedup. Adds pool size cap with priority-based eviction.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use alloy_primitives::{Address, B256};
use torus_types::{compute_action_hash, NativeAction, SignedNativeAction};

use crate::error::MempoolError;

/// Selection-order key (T2.3): `(cancel-priority, sender, nonce, seq)`.
///
/// A `BTreeMap` over this key IS the pool's selection order, maintained
/// incrementally at insert/remove instead of a full `sort_by` plus
/// `hash_index` rebuild under the write lock on every `produce_block`.
/// Iteration reproduces the previous stable
/// `sort_by(cancel-priority, sender, nonce)` EXACTLY: `seq` is a
/// monotonically increasing insertion counter, so entries with equal
/// `(priority, sender, nonce)` iterate in insertion order — precisely the
/// tie order the stable sort preserved.
///
/// s66: an arrival-order key `(priority, seq, sender, nonce)` (s65 item B,
/// meant to stop high-address senders starving) collapsed throughput under
/// overload (s66-abcfix-abc-r1: 3.4k matched/s, 15 txs/blk, gossip flood) and
/// was reverted; the same binary with this key ran 56k (s66-bisect-noarr-r1).
type SortKey = (u8, Address, u64, u64);

/// Same identity as SortKey, ordered by nonce for expiry-prefix removal.
/// No stale heap entries: every insert/remove maintains this exact live index.
type ExpiryKey = (u64, u8, Address, u64);

fn expiry_key(&(priority, sender, nonce, seq): &SortKey) -> ExpiryKey {
    (nonce, priority, sender, seq)
}

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
    /// The exec trust-cache key (`torus_types::verified_cache_key`) iff THIS node
    /// verified the signature and resolved `sender` from it (RPC ingress or
    /// gossip-RECOVER) AND the signature kind is cacheable (EIP-712) — the only
    /// entries eligible to seed the exec trust-cache. `None` for gossip-TRUSTED
    /// admits (sender claimed by a peer, not re-derived here), which must never
    /// be short-circuited at exec, and for session actions.
    /// See `docs/plans/double-verify-trust-cache-impl.md`.
    ///
    /// s65 item C: stored at insert (ingress already derives it) so the
    /// commit-time restash is a lookup, not a canonical re-encode + keccak of
    /// every committed action on the consensus thread (~30 ms per commit).
    pub restash_key: Option<B256>,
}

/// Native action pool with per-sender tracking, dedup, and size limits.
pub(crate) struct NativePool {
    /// Entries in selection order (see [`SortKey`]) — incrementally sorted.
    entries: BTreeMap<SortKey, NativePoolEntry>,
    expiry_index: BTreeSet<ExpiryKey>,
    sender_counts: HashMap<Address, usize>,
    seen: HashSet<(Address, B256)>,
    /// Multimap: action hash -> ALL live entries with that hash, in insertion
    /// order. `compute_action_hash` omits the claimed sender, so a
    /// gossip-TRUSTED peer can admit byte-identical signed bytes under a
    /// different sender — `remove_committed` must find every such duplicate
    /// without scanning the pool. `Vec` len is 1 except under that adversarial
    /// duplicate admit.
    hash_index: HashMap<B256, Vec<SortKey>>,
    /// Insertion counter feeding [`SortKey`] tie order.
    next_seq: u64,
    max_size: usize,
    max_per_sender: usize,
    max_per_block: usize,
}

impl NativePool {
    pub fn new(max_size: usize, max_per_sender: usize, max_per_block: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            expiry_index: BTreeSet::new(),
            sender_counts: HashMap::new(),
            seen: HashSet::new(),
            hash_index: HashMap::new(),
            next_seq: 0,
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
        // Duplicates (if any) are byte-identical `SignedNativeAction`s — the
        // sender is not part of the hash — so returning the earliest-inserted
        // copy is observationally identical to any other.
        self.hash_index
            .get(hash)
            .and_then(|keys| keys.first())
            .and_then(|key| self.entries.get(key))
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
            if let Some(key) = self.hash_index.get(h).and_then(|keys| keys.first()) {
                if let Some(entry) = self.entries.get(key) {
                    if let Some(restash) = entry.restash_key {
                        out.push((restash, entry.sender));
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
        let restash_key = if verified_locally {
            torus_types::verified_cache_key(&action)
        } else {
            None
        };
        self.insert_with_restash_key(sender, action, restash_key)
    }

    /// [`insert_verified`](Self::insert_verified) with the trust-cache key the
    /// caller already derived (`Some` only for a locally verified, cacheable
    /// action), so it is not recomputed here under the pool write lock.
    pub fn insert_with_restash_key(
        &mut self,
        sender: Address,
        action: SignedNativeAction,
        restash_key: Option<B256>,
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
                // High-priority: evict the lowest-priority non-cancel — the
                // LAST entry in selection order (cancels sort first, so if any
                // non-cancel exists it sits at the back of the map).
                let evict_key = match self.entries.iter().next_back() {
                    Some((key, entry)) if !entry.is_cancel => *key,
                    _ => return Err(MempoolError::NativePoolFull),
                };
                self.remove_entry_by_key(&evict_key);
            } else {
                return Err(MempoolError::NativePoolFull);
            }
        }

        *self.sender_counts.entry(sender).or_insert(0) += 1;
        self.seen.insert((sender, action_hash));
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let key: SortKey = (u8::from(!is_cancel), sender, action.nonce, seq);
        self.hash_index.entry(action_hash).or_default().push(key);
        self.expiry_index.insert(expiry_key(&key));
        self.entries.insert(
            key,
            NativePoolEntry {
                sender,
                action,
                action_hash,
                is_cancel,
                encoded_len,
                restash_key,
            },
        );

        Ok(action_hash)
    }

    /// Remove one entry by its selection-order key, maintaining `seen`,
    /// `sender_counts`, and `hash_index` (this key is dropped from the hash's
    /// `Vec`; the mapping itself only when no same-hash duplicate survives).
    /// The single choke point for index maintenance on every removal path.
    fn remove_entry_by_key(&mut self, key: &SortKey) -> Option<NativePoolEntry> {
        let entry = self.entries.remove(key)?;
        self.expiry_index.remove(&expiry_key(key));
        self.seen.remove(&(entry.sender, entry.action_hash));
        self.dec_sender_count(&entry.sender);
        if let Some(keys) = self.hash_index.get_mut(&entry.action_hash) {
            keys.retain(|k| k != key);
            if keys.is_empty() {
                self.hash_index.remove(&entry.action_hash);
            }
        }
        Some(entry)
    }

    /// Drain up to `limit` actions in priority order with per-sender-per-block caps.
    ///
    /// Cancellations first (highest priority), then remaining actions.
    /// Within each priority group, ordered by (sender, nonce) for determinism
    /// — the incremental [`SortKey`] iteration order (T2.3), no re-sort.
    /// Excess actions from rate-limited senders stay in pool for next block.
    pub fn drain(&mut self, limit: usize) -> Vec<SignedNativeAction> {
        if self.max_per_block == 0 {
            return Vec::new();
        }
        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut take_keys: Vec<SortKey> = Vec::new();
        for (key, entry) in &self.entries {
            if take_keys.len() >= limit {
                break;
            }
            let count = block_counts.entry(entry.sender).or_insert(0);
            if *count < self.max_per_block {
                *count += 1;
                take_keys.push(*key);
            }
        }

        let mut taken = Vec::with_capacity(take_keys.len());
        for key in &take_keys {
            if let Some(entry) = self.remove_entry_by_key(key) {
                taken.push(entry.action);
            }
        }

        taken
    }

    /// Select up to `limit` actions for a block WITHOUT removing them from the pool.
    /// Actions stay in the pool until `remove_committed` is called after commit.
    /// Uses the same priority order as `drain`. Read-only (T2.3): callers select
    /// from a shared snapshot under the read lock — no sort, no index rebuild.
    pub fn select_for_block(&self, limit: usize) -> Vec<SignedNativeAction> {
        if self.max_per_block == 0 {
            return Vec::new();
        }
        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut selected = Vec::new();

        for entry in self.entries.values() {
            if selected.len() >= limit {
                break;
            }
            let count = block_counts.entry(entry.sender).or_insert(0);
            if *count < self.max_per_block {
                *count += 1;
                selected.push(entry.action.clone());
            }
        }

        selected
    }

    pub fn select_for_block_with_senders(
        &self,
        limit: usize,
    ) -> Vec<(Address, SignedNativeAction)> {
        self.select_for_block_with_senders_excluding(limit, &HashSet::new(), usize::MAX, usize::MAX)
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
    /// `orders_cap` bounds the summed `order_count` of selected actions (G2:
    /// without it, 100 actions × 1024-order batches = 102,400 orders vs the
    /// documented 50k ceiling). Same deterministic-prefix rule as `bytes_cap`.
    pub fn select_for_block_with_senders_excluding(
        &self,
        limit: usize,
        exclude: &HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
    ) -> Vec<(Address, SignedNativeAction)> {
        if self.max_per_block == 0 {
            return Vec::new();
        }
        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut selected = Vec::new();
        let mut bytes_used: usize = 0;
        let mut orders_used: usize = 0;

        for entry in self.entries.values() {
            if selected.len() >= limit {
                break;
            }
            // Skip in-flight (already-proposed) entries BEFORE the budget gates:
            // an excluded entry is never selected, so it must neither charge nor
            // trip the byte/order budget — otherwise a large in-flight batch
            // sitting early in the sort would halt selection and under-fill the
            // block with valid later entries.
            if exclude.contains(&entry.action_hash) {
                continue;
            }
            if bytes_used.saturating_add(entry.encoded_len) > bytes_cap {
                // Deterministic prefix: stop at the first entry that would blow
                // the body-byte budget rather than skipping past it.
                break;
            }
            let entry_orders = crate::rate_limit::order_count(&entry.action.action);
            if orders_used.saturating_add(entry_orders) > orders_cap {
                // Deterministic prefix for the ORDER budget too (O2/G2) —
                // bounds worst-case matching/exec time per block. Cancels sort
                // first and count 1 each, so cancels-first is preserved.
                break;
            }
            let count = block_counts.entry(entry.sender).or_insert(0);
            if *count < self.max_per_block {
                *count += 1;
                bytes_used = bytes_used.saturating_add(entry.encoded_len);
                orders_used = orders_used.saturating_add(entry_orders);
                selected.push((entry.sender, entry.action.clone()));
            }
        }

        selected
    }

    /// Cancels-ONLY selection (Package D rank 1, deepest exec-backlog pacing
    /// tier): the same walk, budgets, and per-sender cap as
    /// [`Self::select_for_block_with_senders_excluding`], but the scan stops at
    /// the first non-cancel entry. Cancels sort FIRST in [`SortKey`] (priority
    /// byte 0), so this touches exactly the pooled cancel prefix and never
    /// walks the (possibly huge) non-cancel tail. Non-destructive like every
    /// selection: paced-out non-cancels stay pooled and remain fully
    /// selectable by the normal path — pacing defers, never sheds.
    pub fn select_cancels_for_block_with_senders_excluding(
        &self,
        limit: usize,
        exclude: &HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
    ) -> Vec<(Address, SignedNativeAction)> {
        if self.max_per_block == 0 {
            return Vec::new();
        }
        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut selected = Vec::new();
        let mut bytes_used: usize = 0;
        let mut orders_used: usize = 0;

        for entry in self.entries.values() {
            if !entry.is_cancel {
                // Cancels sort first: the first non-cancel ends the cancel
                // prefix — nothing selectable remains beyond it in this mode.
                break;
            }
            if selected.len() >= limit {
                break;
            }
            // In-flight exclusion before the budget gates, exactly like the
            // normal path: an excluded entry must neither charge nor trip them.
            if exclude.contains(&entry.action_hash) {
                continue;
            }
            if bytes_used.saturating_add(entry.encoded_len) > bytes_cap {
                // Deterministic prefix, same rule as the normal path.
                break;
            }
            let entry_orders = crate::rate_limit::order_count(&entry.action.action);
            if orders_used.saturating_add(entry_orders) > orders_cap {
                break;
            }
            let count = block_counts.entry(entry.sender).or_insert(0);
            if *count < self.max_per_block {
                *count += 1;
                bytes_used = bytes_used.saturating_add(entry.encoded_len);
                orders_used = orders_used.saturating_add(entry_orders);
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
        // nonce.saturating_add(window) < now is equivalent to nonce <
        // now-window when subtraction succeeds; otherwise nothing is expired.
        // In particular, nonce+window==now and saturated u64::MAX stay live.
        let Some(cutoff) = now_ms.checked_sub(NONCE_WINDOW_MS) else {
            return 0;
        };
        let mut removed = 0;
        while let Some(&(nonce, priority, sender, seq)) = self.expiry_index.first() {
            if nonce >= cutoff {
                break;
            }
            self.remove_entry_by_key(&(priority, sender, nonce, seq));
            removed += 1;
        }
        removed
    }

    /// Remove actions that were included in a committed block.
    ///
    /// O(k log n) in committed actions via `hash_index` — no pool scan (the
    /// old O(pool) scan under the commit-path write lock cost ~9.3 ms/commit
    /// at 200k pool, `torus_mempool_remove_committed_seconds`). Taking the
    /// `Vec` out of the index first removes ALL same-hash duplicates, exactly
    /// like the scan did; `remove_entry_by_key`'s index maintenance then sees
    /// the hash already unindexed and no-ops.
    pub fn remove_committed(&mut self, hashes: &[B256]) {
        for hash in hashes {
            if let Some(keys) = self.hash_index.remove(hash) {
                for key in &keys {
                    self.remove_entry_by_key(key);
                }
            }
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

    /// Test-only invariant check: `hash_index` <-> `entries` is an exact
    /// bijection — every indexed key resolves to a live entry with that hash,
    /// no key is indexed twice, no empty `Vec` lingers, and every live entry
    /// is indexed under its hash exactly once.
    #[cfg(test)]
    fn assert_index_consistent(&self) {
        assert_eq!(self.expiry_index.len(), self.entries.len());
        for key in self.entries.keys() {
            assert!(self.expiry_index.contains(&expiry_key(key)), "missing expiry key");
        }
        let mut indexed: HashSet<SortKey> = HashSet::new();
        for (hash, keys) in &self.hash_index {
            assert!(!keys.is_empty(), "empty Vec left in hash_index for {hash}");
            for key in keys {
                let entry = self
                    .entries
                    .get(key)
                    .unwrap_or_else(|| panic!("indexed key {key:?} has no entry"));
                assert_eq!(&entry.action_hash, hash, "entry indexed under wrong hash");
                assert!(indexed.insert(*key), "key {key:?} indexed twice");
            }
        }
        assert_eq!(
            indexed.len(),
            self.entries.len(),
            "every live entry must be indexed exactly once"
        );
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
    fn zero_sender_block_cap_leaves_every_selection_empty_and_pool_intact() {
        let mut pool = NativePool::new(100, 64, 0);
        for i in 1..=4u8 {
            let sender = Address::repeat_byte(i);
            pool.insert(sender, make_action(1, NativeAction::ClaimRewards))
                .unwrap();
            pool.insert(
                sender,
                make_action(2, NativeAction::CancelOrder { order_id: i as u128 }),
            ).unwrap();
        }
        for limit in [0, 1, usize::MAX] {
            assert!(pool.select_for_block(limit).is_empty());
            assert!(pool.select_for_block_with_senders(limit).is_empty());
            assert!(pool.select_cancels_for_block_with_senders_excluding(
                limit, &HashSet::new(), usize::MAX, usize::MAX,
            ).is_empty());
            assert!(pool.drain(limit).is_empty());
            assert_eq!(pool.size(), 8);
            assert_eq!(pool.expiry_index.len(), 8);
            assert_eq!(pool.seen.len(), 8);
            assert_eq!(pool.hash_index.values().map(Vec::len).sum::<usize>(), 8);
            assert_eq!(pool.sender_counts.len(), 4);
            assert!(pool.sender_counts.values().all(|&count| count == 2));
        }
        pool.max_per_block = 1;
        assert_eq!(pool.select_for_block(8).len(), 4);
        assert_eq!(pool.drain(8).len(), 4);
        assert_eq!(pool.size(), 4);
        assert_eq!(pool.expiry_index.len(), 4);
        assert_eq!(pool.seen.len(), 4);
        assert_eq!(pool.hash_index.values().map(Vec::len).sum::<usize>(), 4);
        assert!(pool.sender_counts.values().all(|&count| count == 1));
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
            pool.select_for_block_with_senders_excluding(100, &HashSet::new(), cap, usize::MAX);
        assert_eq!(selected.len(), 3);
        // usize::MAX preserves uncapped behavior.
        let all = pool.select_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(all.len(), 10);
    }

    fn evict_expired_reference(pool: &mut NativePool, now: u64) -> usize {
        let keys: Vec<_> = pool
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry
                    .action
                    .nonce
                    .saturating_add(torus_types::eip712::NONCE_WINDOW_MS)
                    < now
            })
            .map(|(key, _)| *key)
            .collect();
        for key in &keys {
            pool.remove_entry_by_key(key);
        }
        keys.len()
    }

    fn assert_same_pool(left: &NativePool, right: &NativePool) {
        left.assert_index_consistent();
        right.assert_index_consistent();
        assert_eq!(left.sender_counts, right.sender_counts);
        assert_eq!(left.seen, right.seen);
        assert_eq!(left.hash_index, right.hash_index);
        assert_eq!(left.expiry_index, right.expiry_index);
        assert_eq!(left.next_seq, right.next_seq);
        assert_eq!(
            left.entries.keys().collect::<Vec<_>>(),
            right.entries.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            bincode::serialize(&left.select_for_block_with_senders(usize::MAX)).unwrap(),
            bincode::serialize(&right.select_for_block_with_senders(usize::MAX)).unwrap()
        );
    }

    #[test]
    fn expiry_index_matches_saturating_strict_boundary_reference() {
        use torus_types::eip712::NONCE_WINDOW_MS as W;
        for now in [0, 1, W - 1, W, W + 1, 3 * W, u64::MAX - 1, u64::MAX] {
            let mut candidate = NativePool::new(100, 100, 100);
            let mut reference = NativePool::new(100, 100, 100);
            for nonce in [
                0,
                1,
                W,
                2 * W,
                u64::MAX - W - 1,
                u64::MAX - W,
                u64::MAX - 1,
                u64::MAX,
            ] {
                for sender in [Address::repeat_byte(1), Address::repeat_byte(2)] {
                    let action = make_action(nonce, NativeAction::ClaimRewards);
                    candidate.insert(sender, action.clone()).unwrap();
                    reference.insert(sender, action).unwrap();
                }
            }
            assert_eq!(
                candidate.evict_expired(now),
                evict_expired_reference(&mut reference, now)
            );
            assert_same_pool(&candidate, &reference);
            assert_eq!(candidate.evict_expired(now), 0);
        }
    }

    #[test]
    fn expiry_index_remains_exact_through_mixed_pool_mutations() {
        let mut candidate = NativePool::new(40, 8, 3);
        let mut reference = NativePool::new(40, 8, 3);
        let mut rng = 0xcafe_babe_8642_7531u64;
        for step in 0..2000u64 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let sender = Address::repeat_byte((rng % 8) as u8);
            let nonce = (rng >> 9) % 100_000;
            let action = make_action(
                nonce,
                if rng & 1 == 0 {
                    NativeAction::CancelAllOrders { market_id: None }
                } else {
                    NativeAction::ClaimRewards
                },
            );
            let a = candidate.insert_verified(sender, action.clone(), rng & 4 == 0);
            let b = reference.insert_verified(sender, action, rng & 4 == 0);
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
            match step % 5 {
                0 => {
                    let now = (rng >> 5) % 170_000;
                    assert_eq!(
                        candidate.evict_expired(now),
                        evict_expired_reference(&mut reference, now)
                    );
                }
                1 => {
                    let hashes: Vec<_> = reference
                        .entries
                        .values()
                        .take(3)
                        .map(|e| e.action_hash)
                        .collect();
                    candidate.remove_committed(&hashes);
                    reference.remove_committed(&hashes);
                }
                2 => {
                    assert_eq!(
                        bincode::serialize(&candidate.drain(3)).unwrap(),
                        bincode::serialize(&reference.drain(3)).unwrap()
                    );
                }
                _ => {}
            }
            assert_same_pool(&candidate, &reference);
        }
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

    /// Package D rank 3: a block genuinely carries MORE than 100 native actions
    /// when the selection `limit` is raised above the historical 100 cap. The
    /// `limit` argument is the ONLY per-block-count gate in the selection path
    /// (proposer passes `native_total_block_cap()` here); nothing downstream
    /// silently re-clamps to ~100. Uses enough distinct senders that the
    /// per-sender cap (`max_per_block`, 64) never binds before the total limit.
    #[test]
    fn selection_carries_more_than_100_actions_when_limit_raised() {
        // 10 senders * 40 distinct-nonce actions = 400 pooled entries; each
        // sender stays under the 64 per-sender-per-block cap.
        let mut pool = NativePool::new(4096, 512, 64);
        for s in 0..10u8 {
            let sender = Address::repeat_byte(s + 1);
            for n in 0..40u64 {
                let nonce = 1_000_000 + s as u64 * 1000 + n;
                pool.insert(sender, make_action(nonce, NativeAction::ClaimRewards))
                    .unwrap();
            }
        }
        assert_eq!(pool.size(), 400);

        // Historical cap: exactly 100 selected — the old ceiling.
        let capped =
            pool.select_for_block_with_senders_excluding(100, &HashSet::new(), usize::MAX, usize::MAX);
        assert_eq!(capped.len(), 100, "limit=100 reproduces today's ceiling");

        // Raised cap: the block carries all 400 — proof the limit is the sole
        // count gate and 400 is reachable end-to-end in selection.
        let raised =
            pool.select_for_block_with_senders_excluding(400, &HashSet::new(), usize::MAX, usize::MAX);
        assert_eq!(raised.len(), 400, "limit=400 genuinely selects >100 actions");

        // A limit between the two resolves exactly, no hidden clamp near 100.
        let mid =
            pool.select_for_block_with_senders_excluding(250, &HashSet::new(), usize::MAX, usize::MAX);
        assert_eq!(mid.len(), 250);
    }

    /// T2.3 (RED-first: `select_for_block*` previously took `&mut self` for a
    /// full re-sort + `hash_index` rebuild, so this did not compile): the
    /// selection order is maintained incrementally, selection is read-only
    /// and works through a SHARED reference — the `Mempool` wrappers select
    /// under the read lock, off the ingress write path.
    #[test]
    fn selection_is_read_only_through_shared_ref() {
        let mut pool = NativePool::new(100, 64, 16);
        pool.insert(
            Address::repeat_byte(1),
            make_action(1, NativeAction::ClaimRewards),
        )
        .unwrap();
        let shared: &NativePool = &pool;
        assert_eq!(shared.select_for_block(10).len(), 1);
        assert_eq!(shared.select_for_block_with_senders(10).len(), 1);
        assert_eq!(
            shared
                .select_for_block_with_senders_excluding(
                    10,
                    &HashSet::new(),
                    usize::MAX,
                    usize::MAX
                )
                .len(),
            1
        );
    }

    /// T2.3: the incremental BTreeMap order must reproduce the previous STABLE
    /// `sort_by(cancel-priority, sender, nonce)` EXACTLY for identical pool
    /// state — including insertion-order ties (equal priority, sender, nonce)
    /// — since the proposer's selection order feeds the block body.
    #[test]
    fn selection_order_identical_to_reference_stable_sort() {
        let mut pool = NativePool::new(100, 64, 16);
        // Shuffled inserts across senders/nonces with cancels interleaved,
        // plus a deliberate tie: same sender + nonce, two distinct cancels.
        let inserts: Vec<(Address, SignedNativeAction)> = vec![
            (
                Address::repeat_byte(3),
                make_action(7, NativeAction::ClaimRewards),
            ),
            (
                Address::repeat_byte(1),
                make_action(9, NativeAction::ClaimRewards),
            ),
            (
                Address::repeat_byte(2),
                make_action(5, NativeAction::CancelOrder { order_id: 8 }),
            ),
            (
                Address::repeat_byte(1),
                make_action(2, NativeAction::ClaimRewards),
            ),
            // Tie with the order_id-8 cancel above: stable sort keeps the
            // earlier insert first.
            (
                Address::repeat_byte(2),
                make_action(5, NativeAction::CancelOrder { order_id: 3 }),
            ),
            (
                Address::repeat_byte(3),
                make_action(4, NativeAction::CancelOrder { order_id: 1 }),
            ),
            (
                Address::repeat_byte(2),
                make_action(6, NativeAction::ClaimRewards),
            ),
        ];
        for (sender, action) in &inserts {
            pool.insert(*sender, action.clone()).unwrap();
        }

        // Reference = the previous algorithm verbatim: a STABLE sort of the
        // inserted list by (cancel-priority, sender, nonce).
        let mut reference = inserts.clone();
        reference.sort_by(|(sa, aa), (sb, ab)| {
            let a_pri = if is_cancel(&aa.action) { 0u8 } else { 1 };
            let b_pri = if is_cancel(&ab.action) { 0u8 } else { 1 };
            a_pri
                .cmp(&b_pri)
                .then_with(|| sa.cmp(sb))
                .then_with(|| aa.nonce.cmp(&ab.nonce))
        });

        let selected = pool.select_for_block_with_senders(inserts.len());
        let got: Vec<(Address, B256)> = selected
            .iter()
            .map(|(s, a)| (*s, compute_action_hash(a)))
            .collect();
        let want: Vec<(Address, B256)> = reference
            .iter()
            .map(|(s, a)| (*s, compute_action_hash(a)))
            .collect();
        assert_eq!(
            got, want,
            "incremental selection order must equal the reference stable sort"
        );

        // Drain follows the identical order.
        let drained = pool.drain(inserts.len());
        let drained_hashes: Vec<B256> = drained.iter().map(compute_action_hash).collect();
        let want_hashes: Vec<B256> = want.iter().map(|(_, h)| *h).collect();
        assert_eq!(drained_hashes, want_hashes, "drain order matches too");
    }

    /// s65 item C: the commit-time restash returns the key stored at insert,
    /// never a recomputation (a deliberately wrong stored key comes back
    /// verbatim), and nothing for entries stored without one.
    #[test]
    fn restash_returns_stored_key_without_recomputing() {
        let mut pool = NativePool::new(100, 64, 16);
        let stored = B256::repeat_byte(0xab);
        let a = make_action(1, NativeAction::ClaimRewards);
        let b = make_action(2, NativeAction::ClaimRewards);
        let ha = pool
            .insert_with_restash_key(Address::repeat_byte(1), a, Some(stored))
            .unwrap();
        let hb = pool
            .insert_with_restash_key(Address::repeat_byte(2), b, None)
            .unwrap();
        assert_eq!(
            pool.verified_restash_keys(&[ha, hb]),
            vec![(stored, Address::repeat_byte(1))]
        );
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
        pool.insert(c, make_action(3, NativeAction::CancelOrder { order_id: 1 }))
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
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards))
                .unwrap();
        }
        assert_eq!(pool.size(), 5);

        let selected = pool.select_for_block(3);
        assert_eq!(selected.len(), 3);
        assert_eq!(pool.size(), 5, "select_for_block must not remove entries");

        let hash1 = compute_action_hash(&selected[0]);
        assert!(
            pool.get_by_hash(&hash1).is_some(),
            "selected action still in pool"
        );
    }

    #[test]
    fn remove_committed_cleans_pool() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        for i in 1..=5u64 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards))
                .unwrap();
        }
        let selected = pool.select_for_block(3);
        let hashes: Vec<B256> = selected.iter().map(|a| compute_action_hash(a)).collect();

        pool.remove_committed(&hashes);
        assert_eq!(pool.size(), 2, "3 committed actions removed, 2 remain");

        for h in &hashes {
            assert!(
                pool.get_by_hash(h).is_none(),
                "committed action removed from hash_index"
            );
        }
    }

    #[test]
    fn hash_index_survives_drain() {
        let mut pool = NativePool::new(100, 64, 2);
        let sender = Address::repeat_byte(1);
        for i in 1..=4u64 {
            pool.insert(sender, make_action(i, NativeAction::ClaimRewards))
                .unwrap();
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
                make_action(
                    i + 100,
                    NativeAction::CancelOrder {
                        order_id: i as u128,
                    },
                ),
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

        let second =
            pool.select_for_block_with_senders_excluding(5, &exclude, usize::MAX, usize::MAX);
        assert!(
            second.is_empty(),
            "in-flight actions must not be re-selected"
        );
        assert_eq!(pool.size(), 5, "selection stays non-destructive");

        // A fresh (non-excluded) action is still selectable past the exclusion set.
        pool.insert(sender, make_action(99, NativeAction::ClaimRewards))
            .unwrap();
        let third =
            pool.select_for_block_with_senders_excluding(5, &exclude, usize::MAX, usize::MAX);
        assert_eq!(third.len(), 1, "only the fresh action is selected");
        assert_eq!(third[0].1.nonce, 99);
    }

    /// Rank-1 (Package D) exec-backlog pacing, deepest tier: cancels-only
    /// selection must take ONLY cancel actions (which sort first — the scan
    /// stops at the first non-cancel), honor the exclude set and byte budget,
    /// and stay NON-destructive: paced-out non-cancels remain pooled and fully
    /// selectable by the normal path afterwards (pacing, not shedding).
    #[test]
    fn cancels_only_selection_takes_only_cancels_nondestructively() {
        let mut pool = NativePool::new(100, 64, 16);
        // 3 non-cancels + 3 cancels across distinct senders.
        for i in 0..3u8 {
            pool.insert(
                Address::repeat_byte(i + 1),
                make_action(10 + i as u64, NativeAction::ClaimRewards),
            )
            .unwrap();
        }
        let mut cancel_hashes = Vec::new();
        let mut cancel_len = 0usize;
        for i in 0..3u8 {
            let a = make_action(
                20 + i as u64,
                NativeAction::CancelOrder {
                    order_id: i as u128,
                },
            );
            cancel_len = bincode::serialized_size(&a).unwrap() as usize;
            cancel_hashes.push(compute_action_hash(&a));
            pool.insert(Address::repeat_byte(i + 10), a).unwrap();
        }

        let sel = pool.select_cancels_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(sel.len(), 3, "exactly the cancels are selected");
        assert!(
            sel.iter().all(|(_, a)| is_cancel(&a.action)),
            "cancels-only mode must never select a non-cancel"
        );
        assert_eq!(pool.size(), 6, "selection is non-destructive");

        // Exclude one in-flight cancel: only the other two come back.
        let exclude: HashSet<B256> = [cancel_hashes[0]].into_iter().collect();
        let sel2 = pool.select_cancels_for_block_with_senders_excluding(
            100,
            &exclude,
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(sel2.len(), 2, "in-flight cancels are excluded");

        // Byte budget applies to cancels too (deterministic prefix).
        let sel3 = pool.select_cancels_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            cancel_len * 2,
            usize::MAX,
        );
        assert_eq!(sel3.len(), 2, "byte budget bounds the cancel prefix");

        // The paced-out non-cancels are STILL selectable by the normal path —
        // nothing was dropped by the cancels-only tier.
        let normal = pool.select_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(normal.len(), 6, "pacing defers non-cancels, never drops them");
    }

    #[test]
    fn order_budget_bounds_selection_and_preserves_cancels_first() {
        let mut pool = NativePool::new(100, 64, 16);
        let p = torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(100),
            quantity: torus_types::FixedPoint::from_raw(100),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        // 5 batches of 10 orders + 2 cancels, distinct senders.
        for i in 0..5u8 {
            pool.insert(
                Address::repeat_byte(i + 1),
                make_action(
                    1_000 + i as u64,
                    NativeAction::PlaceOrderBatch(vec![p.clone(); 10]),
                ),
            )
            .unwrap();
        }
        pool.insert(
            Address::repeat_byte(10),
            make_action(2_000, NativeAction::CancelOrder { order_id: 1 }),
        )
        .unwrap();
        pool.insert(
            Address::repeat_byte(11),
            make_action(2_001, NativeAction::CancelOrder { order_id: 2 }),
        )
        .unwrap();

        // Order budget 25: cancels first (1+1), then TWO batches (10+10 -> 22);
        // a third batch would reach 32 > 25 -> deterministic-prefix break.
        let sel =
            pool.select_for_block_with_senders_excluding(100, &HashSet::new(), usize::MAX, 25);
        assert_eq!(
            sel.len(),
            4,
            "2 cancels + 2 batches fit the 25-order budget"
        );
        assert!(matches!(sel[0].1.action, NativeAction::CancelOrder { .. }));
        assert!(matches!(sel[1].1.action, NativeAction::CancelOrder { .. }));
        assert!(matches!(sel[2].1.action, NativeAction::PlaceOrderBatch(_)));
        assert!(matches!(sel[3].1.action, NativeAction::PlaceOrderBatch(_)));

        // usize::MAX order budget preserves today's behavior exactly.
        let all = pool.select_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(all.len(), 7);
    }

    /// `compute_action_hash` covers action+nonce+signature but NOT the claimed
    /// sender, so a gossip-TRUSTED peer can admit byte-identical signed bytes
    /// under a different sender: two live entries, one hash. `remove_committed`
    /// must remove ALL of them — a single-slot-index rewrite would leave a
    /// re-selectable duplicate survivor (re-proposal/liveness bug).
    #[test]
    fn remove_committed_removes_all_same_hash_duplicates() {
        let mut pool = NativePool::new(100, 64, 16);
        let action = make_action(1, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&action);
        pool.insert(Address::repeat_byte(1), action.clone()).unwrap();
        pool.insert(Address::repeat_byte(2), action).unwrap();
        assert_eq!(pool.size(), 2, "same-hash duplicates coexist");
        pool.assert_index_consistent();

        pool.remove_committed(&[hash]);
        assert_eq!(pool.size(), 0, "ALL same-hash duplicates removed");
        assert!(pool.get_by_hash(&hash).is_none());
        pool.assert_index_consistent();
    }

    #[test]
    fn index_consistent_across_insert_remove_reinsert() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        let action = make_action(1, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&action);

        pool.insert(sender, action.clone()).unwrap();
        pool.remove_committed(&[hash]);
        assert_eq!(pool.size(), 0);
        pool.assert_index_consistent();

        // seen/sender_counts cleaned: same (sender, action) insertable again.
        pool.insert(sender, action).unwrap();
        assert_eq!(pool.get_by_hash(&hash).unwrap().nonce, 1);
        pool.assert_index_consistent();

        pool.remove_committed(&[hash]);
        assert_eq!(pool.size(), 0, "second commit empties the pool again");
        pool.assert_index_consistent();
    }

    #[test]
    fn remove_committed_unknown_hash_is_noop() {
        let mut pool = NativePool::new(100, 64, 16);
        let sender = Address::repeat_byte(1);
        let action = make_action(1, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&action);
        pool.insert(sender, action).unwrap();

        pool.remove_committed(&[B256::ZERO, B256::repeat_byte(0xAB)]);
        assert_eq!(pool.size(), 1);
        assert!(pool.get_by_hash(&hash).is_some());
        pool.assert_index_consistent();

        // Subsequent inserts unaffected.
        pool.insert(sender, make_action(2, NativeAction::ClaimRewards))
            .unwrap();
        assert_eq!(pool.size(), 2);
        pool.assert_index_consistent();
    }

    /// Cap-2 pool: an incoming cancel evicts the back non-cancel
    /// (`insert_verified` eviction branch). The evicted entry must leave the
    /// index; the survivors must stay removable by hash.
    #[test]
    fn eviction_path_keeps_index_consistent() {
        let mut pool = NativePool::new(2, 64, 16);
        let kept_action = make_action(1, NativeAction::ClaimRewards);
        let kept_hash = compute_action_hash(&kept_action);
        // Larger sender sorts to the back of the non-cancel range -> evicted.
        let evicted_action = make_action(2, NativeAction::ClaimRewards);
        let evicted_hash = compute_action_hash(&evicted_action);
        let cancel = make_action(3, NativeAction::CancelOrder { order_id: 1 });
        let cancel_hash = compute_action_hash(&cancel);

        pool.insert(Address::repeat_byte(1), kept_action).unwrap();
        pool.insert(Address::repeat_byte(2), evicted_action).unwrap();
        pool.insert(Address::repeat_byte(3), cancel).unwrap();
        assert_eq!(pool.size(), 2);
        pool.assert_index_consistent();
        assert!(
            pool.get_by_hash(&evicted_hash).is_none(),
            "evicted entry left the index"
        );

        // remove_committed on the evicted hash is a no-op.
        pool.remove_committed(&[evicted_hash]);
        assert_eq!(pool.size(), 2);
        pool.assert_index_consistent();

        // Remaining entries still removable by hash.
        pool.remove_committed(&[kept_hash, cancel_hash]);
        assert_eq!(pool.size(), 0);
        pool.assert_index_consistent();
    }

    /// After one same-hash duplicate is removed (drain), the survivor must
    /// remain findable and later removable — the guarantee
    /// `repoint_hash_survivors` used to provide under the single-slot index.
    #[test]
    fn duplicate_survivor_findable_after_partial_removal() {
        let mut pool = NativePool::new(100, 64, 16);
        let action = make_action(1, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&action);
        pool.insert(Address::repeat_byte(1), action.clone()).unwrap();
        pool.insert(Address::repeat_byte(2), action).unwrap();

        let drained = pool.drain(1);
        assert_eq!(drained.len(), 1);
        assert_eq!(pool.size(), 1);
        assert!(pool.get_by_hash(&hash).is_some(), "survivor still indexed");
        pool.assert_index_consistent();

        pool.remove_committed(&[hash]);
        assert_eq!(pool.size(), 0);
        assert!(pool.get_by_hash(&hash).is_none());
        pool.assert_index_consistent();
    }

    /// Same-hash duplicates share a nonce, so they expire together; the index
    /// must be empty afterwards and both copies re-insertable.
    #[test]
    fn evict_expired_with_duplicates_keeps_index_consistent() {
        use torus_types::eip712::NONCE_WINDOW_MS;
        let mut pool = NativePool::new(100, 64, 16);
        let now: u64 = 10 * NONCE_WINDOW_MS;
        let stale = make_action(now - 2 * NONCE_WINDOW_MS, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&stale);
        pool.insert(Address::repeat_byte(1), stale.clone()).unwrap();
        pool.insert(Address::repeat_byte(2), stale.clone()).unwrap();

        assert_eq!(pool.evict_expired(now), 2);
        assert_eq!(pool.size(), 0);
        assert!(pool.get_by_hash(&hash).is_none());
        pool.assert_index_consistent();

        pool.insert(Address::repeat_byte(1), stale.clone()).unwrap();
        pool.insert(Address::repeat_byte(2), stale).unwrap();
        assert_eq!(pool.size(), 2);
        pool.assert_index_consistent();
    }
}
