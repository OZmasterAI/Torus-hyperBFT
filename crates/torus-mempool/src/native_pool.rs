//! Native action pool with per-sender tracking, dedup, and size limits.
//!
//! Task 3.1.4: Hardens the native pool that previously had no per-sender
//! limits and no dedup. Adds pool size cap with priority-based eviction.

use std::collections::{HashMap, HashSet};

use alloy_primitives::{keccak256, Address, B256};
use torus_types::{NativeAction, SignedNativeAction};

use crate::error::MempoolError;

/// Entry in the native action pool with pre-recovered sender and dedup hash.
pub(crate) struct NativePoolEntry {
    pub sender: Address,
    pub action: SignedNativeAction,
    pub action_hash: B256,
    pub is_cancel: bool,
}

/// Native action pool with per-sender tracking, dedup, and size limits.
pub(crate) struct NativePool {
    entries: Vec<NativePoolEntry>,
    sender_counts: HashMap<Address, usize>,
    seen: HashSet<(Address, B256)>,
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
            max_size,
            max_per_sender,
            max_per_block,
        }
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// Insert a native action with a known sender.
    ///
    /// Checks: dedup by (sender, action_hash), per-sender pool cap, total pool cap.
    /// When pool is full and a cancel arrives, evicts a lowest-priority non-cancel.
    pub fn insert(
        &mut self,
        sender: Address,
        action: SignedNativeAction,
    ) -> Result<(), MempoolError> {
        let action_hash = compute_action_hash(&action);
        let is_cancel = is_cancel(&action.action);

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
                } else {
                    return Err(MempoolError::NativePoolFull);
                }
            } else {
                return Err(MempoolError::NativePoolFull);
            }
        }

        *self.sender_counts.entry(sender).or_insert(0) += 1;
        self.seen.insert((sender, action_hash));
        self.entries.push(NativePoolEntry {
            sender,
            action,
            action_hash,
            is_cancel,
        });

        Ok(())
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
            self.entries.push(entry);
        }

        taken
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

fn is_cancel(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::CancelOrder { .. } | NativeAction::CancelAllOrders { .. }
    )
}

/// Compute a deterministic hash for dedup purposes.
///
/// Uses `NativeAction::canonical_bytes()` instead of `Debug` formatting to ensure
/// the hash is identical across compiler versions and crate updates.
///
/// AUDIT: EVM-FIND-11 requested EIP-712 struct hash for dedup.
/// canonical_bytes() provides equivalent collision resistance with simpler implementation.
/// EIP-712 would add complexity without material security benefit for internal pool dedup.
fn compute_action_hash(action: &SignedNativeAction) -> B256 {
    let mut data = action.action.canonical_bytes();
    data.extend_from_slice(&action.nonce.to_be_bytes());
    keccak256(&data)
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
}
