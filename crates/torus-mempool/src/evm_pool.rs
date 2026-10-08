use alloy_primitives::{keccak256, Address, B256, U256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

use crate::error::MempoolError;

/// Decoded metadata for a pooled EVM transaction.
#[derive(Clone, Debug)]
pub struct EvmPoolEntry {
    pub hash: B256,
    pub sender: Address,
    pub nonce: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee: u128,
    pub gas_limit: u64,
    pub value: U256,
    pub raw_rlp: Vec<u8>,
}

/// Per-gas amount the proposer earns from a tx at `base_fee`:
/// min(max_priority_fee, max_fee - base_fee), as revm charges it (EIP-1559;
/// legacy/2930 txs carry tip == gas price, so theirs is gas_price - base_fee).
/// Negative when max_fee < base_fee: such a tx is below the D4 floor and can
/// not be included, so it ranks below every payable tx.
fn effective_tip(max_fee: u128, max_priority_fee: u128, base_fee: u128) -> i128 {
    let cap = |v: u128| i128::try_from(v).unwrap_or(i128::MAX);
    cap(max_priority_fee).min(cap(max_fee).saturating_sub(cap(base_fee)))
}

/// Ordering key for the eviction index: effective tip at the pool's base fee.
/// Reversed so BTreeSet iteration yields the highest tip first and
/// `next_back()` the eviction candidate (lowest tip, then lowest max fee).
#[derive(Clone, Debug, Eq, PartialEq)]
struct TxPriority {
    tip: i128,
    max_fee_per_gas: u128,
    hash: B256,
}

impl TxPriority {
    fn new(entry: &EvmPoolEntry, base_fee: u128) -> Self {
        Self {
            tip: effective_tip(entry.max_fee_per_gas, entry.max_priority_fee, base_fee),
            max_fee_per_gas: entry.max_fee_per_gas,
            hash: entry.hash,
        }
    }
}

impl Ord for TxPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .tip
            .cmp(&self.tip)
            .then_with(|| other.max_fee_per_gas.cmp(&self.max_fee_per_gas))
            .then_with(|| self.hash.cmp(&other.hash))
    }
}

impl PartialOrd for TxPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Heap entry for the drain k-way merge (max-heap by effective tip at the
/// drain's base fee, i.e. by what the proposer earns per gas).
///
/// Task 3.1.5 (anti-MEV): `shuffle_key` is a deterministic tiebreaker derived
/// from the parent block hash. Within the same tip, this randomizes
/// ordering so an attacker cannot guarantee placement relative to a victim's tx.
#[derive(Eq, PartialEq)]
struct DrainEntry {
    tip: i128,
    sender: Address,
    nonce: u64,
    gas_limit: u64,
    shuffle_key: u64,
}

impl DrainEntry {
    fn new(entry: &EvmPoolEntry, base_fee: u128, parent_hash: &B256) -> Self {
        Self {
            tip: effective_tip(entry.max_fee_per_gas, entry.max_priority_fee, base_fee),
            sender: entry.sender,
            nonce: entry.nonce,
            gas_limit: entry.gas_limit,
            shuffle_key: compute_shuffle_key(parent_hash, &entry.sender, entry.nonce),
        }
    }
}

impl Ord for DrainEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tip
            .cmp(&other.tip)
            .then_with(|| self.shuffle_key.cmp(&other.shuffle_key))
    }
}

impl PartialOrd for DrainEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Minimum value a replacement must offer for a fee field: `old` raised by
/// `bump_pct`, and strictly above `old` (geth `txpool/legacypool` list.Add).
fn bumped(old: u128, bump_pct: u64) -> u128 {
    (old.saturating_mul(100 + bump_pct as u128) / 100).max(old.saturating_add(1))
}

/// Compute a deterministic shuffle key for anti-MEV ordering.
/// Derived from parent_hash + sender + nonce so all nodes produce identical order.
fn compute_shuffle_key(parent_hash: &B256, sender: &Address, nonce: u64) -> u64 {
    let mut data = [0u8; 60]; // 32 (hash) + 20 (address) + 8 (nonce)
    data[..32].copy_from_slice(parent_hash.as_slice());
    data[32..52].copy_from_slice(sender.as_slice());
    data[52..60].copy_from_slice(&nonce.to_be_bytes());
    let hash = keccak256(data);
    u64::from_be_bytes(hash.as_slice()[..8].try_into().unwrap())
}

/// Inner EVM transaction pool (not thread-safe — wrapped by Mempool).
pub(crate) struct EvmPool {
    /// Per-sender nonce-ordered transaction queues.
    by_sender: HashMap<Address, BTreeMap<u64, EvmPoolEntry>>,
    /// Hash -> (sender, nonce) for O(1) lookup.
    by_hash: HashMap<B256, (Address, u64)>,
    /// Eviction index: effective tip at `base_fee` (highest first via
    /// reversed Ord).
    ///
    /// The base fee moves every block, which reorders effective tips, so the
    /// keys are rebuilt in `set_base_fee` whenever the value changes: one
    /// O(n log n) pass per committed block at most (base fee is frozen today,
    /// so usually none), and inserts stay O(log n) however hard the pool is
    /// spammed. geth does the same (pricedList.Reheap on each new head).
    by_price: BTreeSet<TxPriority>,
    /// Current pool size.
    size: usize,
    /// Base fee `by_price` is keyed at (latest committed header).
    base_fee: u128,
}

impl EvmPool {
    pub fn new(base_fee: u64) -> Self {
        Self {
            by_sender: HashMap::new(),
            by_hash: HashMap::new(),
            by_price: BTreeSet::new(),
            size: 0,
            base_fee: base_fee as u128,
        }
    }

    /// Re-key the eviction index at a new base fee (no-op if unchanged).
    pub fn set_base_fee(&mut self, base_fee: u64) {
        let base_fee = base_fee as u128;
        if base_fee == self.base_fee {
            return;
        }
        self.base_fee = base_fee;
        self.by_price = self
            .by_sender
            .values()
            .flat_map(|txs| txs.values())
            .map(|e| TxPriority::new(e, base_fee))
            .collect();
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn contains(&self, hash: &B256) -> bool {
        self.by_hash.contains_key(hash)
    }

    pub fn all_senders(&self) -> Vec<Address> {
        self.by_sender.keys().cloned().collect()
    }

    /// Insert a validated transaction.
    /// Returns (replaced_hash, freed_bytes) — freed_bytes is the total raw RLP size
    /// of any evicted or replaced transactions (FIX EVM-FIND-02).
    pub fn insert(
        &mut self,
        entry: EvmPoolEntry,
        max_pool_size: usize,
        max_per_sender: usize,
        replacement_bump_pct: u64,
    ) -> Result<(Option<B256>, usize), MempoolError> {
        let hash = entry.hash;
        let sender = entry.sender;
        let nonce = entry.nonce;
        let mut freed_bytes = 0usize;

        if self.by_hash.contains_key(&hash) {
            return Err(MempoolError::DuplicateTx(hash));
        }

        // Check replacement (same sender + nonce). geth rule: the max fee AND
        // the tip must both rise by the bump, so a replacement can't keep the
        // slot while lowering what the proposer earns.
        let replaced = if let Some(sender_txs) = self.by_sender.get(&sender) {
            if let Some(existing) = sender_txs.get(&nonce) {
                let need_max_fee = bumped(existing.max_fee_per_gas, replacement_bump_pct);
                let need_priority_fee = bumped(existing.max_priority_fee, replacement_bump_pct);
                if entry.max_fee_per_gas < need_max_fee
                    || entry.max_priority_fee < need_priority_fee
                {
                    return Err(MempoolError::ReplacementUnderpriced {
                        need_max_fee,
                        got_max_fee: entry.max_fee_per_gas,
                        need_priority_fee,
                        got_priority_fee: entry.max_priority_fee,
                    });
                }
                let old_hash = existing.hash;
                freed_bytes = existing.raw_rlp.len();
                self.by_price
                    .remove(&TxPriority::new(existing, self.base_fee));
                self.by_hash.remove(&old_hash);
                self.size -= 1;
                Some(old_hash)
            } else {
                None
            }
        } else {
            None
        };

        // Per-sender limit (skip check for replacements)
        if replaced.is_none() {
            if let Some(sender_txs) = self.by_sender.get(&sender) {
                if sender_txs.len() >= max_per_sender {
                    return Err(MempoolError::PoolFull);
                }
            }
        }

        // Pool-wide size limit: evict the lowest effective tip at the
        // current base fee (what the proposer would earn least from).
        let key = TxPriority::new(&entry, self.base_fee);
        if replaced.is_none() && self.size >= max_pool_size {
            if let Some(lowest) = self.by_price.iter().next_back().cloned() {
                if key.tip <= lowest.tip {
                    return Err(MempoolError::PoolFull);
                }
                if let Some(evicted) = self.remove_by_hash(&lowest.hash) {
                    freed_bytes += evicted.raw_rlp.len();
                }
            } else {
                return Err(MempoolError::PoolFull);
            }
        }

        // Insert into all indices
        self.by_price.insert(key);
        self.by_hash.insert(hash, (sender, nonce));
        self.by_sender
            .entry(sender)
            .or_default()
            .insert(nonce, entry);
        self.size += 1;

        Ok((replaced, freed_bytes))
    }

    /// Remove a transaction by hash.
    pub fn remove_by_hash(&mut self, hash: &B256) -> Option<EvmPoolEntry> {
        let (sender, nonce) = self.by_hash.remove(hash)?;
        let sender_txs = self.by_sender.get_mut(&sender)?;
        let entry = sender_txs.remove(&nonce)?;
        self.by_price
            .remove(&TxPriority::new(&entry, self.base_fee));
        if sender_txs.is_empty() {
            self.by_sender.remove(&sender);
        }
        self.size -= 1;
        Some(entry)
    }

    /// Return the next nonce this sender should use, accounting for pool contents.
    ///
    /// Walks consecutive nonces starting from `state_nonce`. If the pool holds
    /// nonces 5, 6, 7 and state_nonce is 5, returns 8. If there's a gap (5, 7),
    /// returns 6.
    pub fn pending_nonce(&self, sender: &Address, state_nonce: u64) -> u64 {
        let txs = match self.by_sender.get(sender) {
            Some(m) => m,
            None => return state_nonce,
        };
        let mut nonce = state_nonce;
        while txs.contains_key(&nonce) {
            nonce += 1;
        }
        nonce
    }

    /// Remove transactions with nonce below `confirmed_nonce` for the given sender.
    /// Returns (count_removed, freed_bytes).
    pub fn prune_confirmed(&mut self, sender: &Address, confirmed_nonce: u64) -> (usize, usize) {
        let stale: Vec<u64> = match self.by_sender.get(sender) {
            Some(txs) => txs.range(..confirmed_nonce).map(|(&n, _)| n).collect(),
            None => return (0, 0),
        };
        let mut freed_bytes = 0usize;
        for nonce in &stale {
            if let Some(entry) = self.by_sender.get_mut(sender).and_then(|m| m.remove(nonce)) {
                freed_bytes += entry.raw_rlp.len();
                self.by_hash.remove(&entry.hash);
                self.by_price
                    .remove(&TxPriority::new(&entry, self.base_fee));
                self.size -= 1;
            }
        }
        if self.by_sender.get(sender).is_none_or(|m| m.is_empty()) {
            self.by_sender.remove(sender);
        }
        (stale.len(), freed_bytes)
    }

    /// Drain transactions for a block proposal using k-way merge by effective tip.
    ///
    /// For each sender, starts with their lowest-nonce tx. Picks the highest
    /// effective tip at `min_base_fee` across all senders, advances that
    /// sender's pointer, repeats until gas budget is exhausted. Returns raw RLP bytes for inclusion in `TorusBlock`.
    ///
    /// D1 (S392): selection is bounded by `gas_budget` alone (count caps
    /// deleted); `sender_gas_cap` optionally caps one sender's share of it.
    /// Task 3.1.5: `parent_hash` seeds deterministic same-tip shuffling (anti-MEV).
    /// D4 (S392): `min_base_fee` re-checks the fee floor at selection time.
    pub fn drain(
        &mut self,
        gas_budget: u64,
        sender_gas_cap: Option<u64>,
        parent_hash: &B256,
        min_base_fee: u128,
    ) -> Vec<Vec<u8>> {
        let mut heap = BinaryHeap::new();
        for txs in self.by_sender.values() {
            if let Some(entry) = txs.values().next() {
                heap.push(DrainEntry::new(entry, min_base_fee, parent_hash));
            }
        }

        let mut result = Vec::new();
        let mut gas_used: u64 = 0;
        let mut to_remove = Vec::new();
        let mut sender_gas: HashMap<Address, u64> = HashMap::new();

        while let Some(top) = heap.pop() {
            // D4 (S392) drain re-check: a negative tip means max fee < base
            // fee. The heap pops the highest tip first and only pushes after a
            // selection, so once the top is below the floor everything left is
            // too — stop selecting. Below-floor txs stay pooled (the floor is
            // dynamic).
            if top.tip < 0 {
                break;
            }

            // D1 per-sender gas share (testnet spam guard): after a sender's
            // FIRST selected tx, further txs must fit inside their share of
            // the budget. The first tx is always eligible (bounded only by
            // the remaining block budget) so a single large tx — e.g. a
            // contract deploy bigger than the share — can still land; the
            // cap throttles sustained multi-tx flooding. Excess stays pooled.
            if let Some(cap) = sender_gas_cap {
                let used = sender_gas.get(&top.sender).copied().unwrap_or(0);
                if used != 0 && used.saturating_add(top.gas_limit) > cap {
                    continue;
                }
            }

            if gas_used.saturating_add(top.gas_limit) > gas_budget {
                continue;
            }

            let raw = match self
                .by_sender
                .get(&top.sender)
                .and_then(|m| m.get(&top.nonce))
            {
                Some(e) => e.raw_rlp.clone(),
                None => continue,
            };

            gas_used += top.gas_limit;
            to_remove.push(
                self.by_sender
                    .get(&top.sender)
                    .and_then(|m| m.get(&top.nonce))
                    .map(|e| e.hash)
                    .unwrap(),
            );
            result.push(raw);
            *sender_gas.entry(top.sender).or_insert(0) += top.gas_limit;

            // Advance: push next nonce for this sender.
            let next_nonce = top.nonce + 1;
            if let Some(next) = self
                .by_sender
                .get(&top.sender)
                .and_then(|m| m.get(&next_nonce))
            {
                heap.push(DrainEntry::new(next, min_base_fee, parent_hash));
            }
        }

        for hash in &to_remove {
            self.remove_by_hash(hash);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUMP: u64 = 10;

    fn entry(sender: u8, nonce: u64, max_fee: u128, tip: u128) -> EvmPoolEntry {
        EvmPoolEntry {
            hash: keccak256([sender, nonce as u8, max_fee as u8, tip as u8]),
            sender: Address::repeat_byte(sender),
            nonce,
            max_fee_per_gas: max_fee,
            max_priority_fee: tip,
            gas_limit: 21_000,
            value: U256::ZERO,
            raw_rlp: vec![sender; 10 + nonce as usize],
        }
    }

    fn pool_with(base_fee: u64, entries: &[EvmPoolEntry], max_pool_size: usize) -> EvmPool {
        let mut pool = EvmPool::new(base_fee);
        for e in entries {
            pool.insert(e.clone(), max_pool_size, 16, BUMP).unwrap();
        }
        pool
    }

    fn senders_of(drained: &[Vec<u8>]) -> Vec<u8> {
        drained.iter().map(|raw| raw[0]).collect()
    }

    /// Fix (a): at base fee 10, A (max 100, tip 1) pays the proposer 1 and
    /// B (max 20, tip 9) pays 9, so B drains first although A's max fee is higher.
    #[test]
    fn drain_orders_by_effective_tip_not_max_fee() {
        let mut pool = pool_with(10, &[entry(0xA, 0, 100, 1), entry(0xB, 0, 20, 9)], 100);
        let first = pool.drain(21_000, None, &B256::ZERO, 10);
        assert_eq!(senders_of(&first), vec![0xB]);
        let rest = pool.drain(21_000, None, &B256::ZERO, 10);
        assert_eq!(senders_of(&rest), vec![0xA]);
    }

    /// Legacy/2930 txs carry tip == max fee, so their tip is max_fee - base_fee.
    #[test]
    fn drain_caps_tip_at_max_fee_minus_base_fee() {
        // A: legacy-style (gas price 15, tip 15) -> effective 5 at base 10.
        // B: tip 6 under max 50 -> effective 6.
        let mut pool = pool_with(10, &[entry(0xA, 0, 15, 15), entry(0xB, 0, 50, 6)], 100);
        let drained = pool.drain(42_000, None, &B256::ZERO, 10);
        assert_eq!(senders_of(&drained), vec![0xB, 0xA]);
    }

    /// Same effective tip: order follows shuffle_key (anti-MEV, Task 3.1.5),
    /// not max fee. Both orders must occur across parent hashes.
    #[test]
    fn same_tip_ties_follow_shuffle_key() {
        let a = entry(0xA, 0, 100, 5);
        let b = entry(0xB, 0, 50, 5);
        let mut seen_a_first = false;
        let mut seen_b_first = false;
        for seed in 0u8..32 {
            let parent = B256::repeat_byte(seed);
            let ka = compute_shuffle_key(&parent, &a.sender, 0);
            let kb = compute_shuffle_key(&parent, &b.sender, 0);
            let want = if ka > kb {
                vec![0xA, 0xB]
            } else {
                vec![0xB, 0xA]
            };
            let mut pool = pool_with(10, &[a.clone(), b.clone()], 100);
            assert_eq!(senders_of(&pool.drain(42_000, None, &parent, 10)), want);
            seen_a_first |= ka > kb;
            seen_b_first |= kb > ka;
        }
        assert!(
            seen_a_first && seen_b_first,
            "seeds must exercise both orders"
        );
    }

    /// D4: a tx with max fee below the current base fee is never selected and
    /// stays pooled, even when it would have the largest tip.
    #[test]
    fn below_floor_tx_not_drained_and_stays_pooled() {
        let mut pool = pool_with(5, &[entry(0xA, 0, 9, 9), entry(0xB, 0, 11, 1)], 100);
        let drained = pool.drain(1_000_000, None, &B256::ZERO, 10);
        assert_eq!(senders_of(&drained), vec![0xB]);
        assert_eq!(pool.size(), 1);
        assert!(pool.contains(&entry(0xA, 0, 9, 9).hash));
    }

    /// Fix (a) replacement: geth rule, both the max fee and the tip must rise
    /// by the bump.
    #[test]
    fn replacement_needs_both_fee_and_tip_bump() {
        let mut pool = pool_with(10, &[entry(0xA, 0, 100, 10)], 100);

        let err = pool
            .insert(entry(0xA, 0, 200, 10), 100, 16, BUMP)
            .unwrap_err();
        match err {
            MempoolError::ReplacementUnderpriced {
                need_max_fee,
                got_max_fee,
                need_priority_fee,
                got_priority_fee,
            } => {
                assert_eq!((need_max_fee, got_max_fee), (110, 200));
                assert_eq!((need_priority_fee, got_priority_fee), (11, 10));
            }
            other => panic!("expected ReplacementUnderpriced, got {other}"),
        }
        // Tip bumped, max fee not.
        assert!(matches!(
            pool.insert(entry(0xA, 0, 109, 11), 100, 16, BUMP),
            Err(MempoolError::ReplacementUnderpriced { .. })
        ));

        let (replaced, freed) = pool.insert(entry(0xA, 0, 110, 11), 100, 16, BUMP).unwrap();
        assert_eq!(replaced, Some(entry(0xA, 0, 100, 10).hash));
        assert_eq!(freed, 10, "old raw RLP bytes freed");
        assert_eq!(pool.size(), 1);
    }

    /// geth also requires a strict rise: a zero tip must become at least 1.
    #[test]
    fn replacement_of_zero_tip_needs_nonzero_tip() {
        let mut pool = pool_with(10, &[entry(0xA, 0, 100, 0)], 100);
        assert!(matches!(
            pool.insert(entry(0xA, 0, 110, 0), 100, 16, BUMP),
            Err(MempoolError::ReplacementUnderpriced {
                need_priority_fee: 1,
                ..
            })
        ));
        pool.insert(entry(0xA, 0, 110, 1), 100, 16, BUMP).unwrap();
    }

    /// Fix (a) eviction: a full pool drops the lowest effective tip, not the
    /// lowest max fee.
    #[test]
    fn eviction_drops_lowest_effective_tip() {
        // Base 10: A tip 20 (max 100), B tip 55 (max 65). Lowest max fee is B,
        // lowest effective tip is A.
        let a = entry(0xA, 0, 100, 20);
        let b = entry(0xB, 0, 65, 65);
        let mut pool = pool_with(10, &[a.clone(), b.clone()], 2);

        let (_, freed) = pool.insert(entry(0xC, 0, 100, 25), 2, 16, BUMP).unwrap();
        assert!(!pool.contains(&a.hash), "A (lowest tip) evicted");
        assert!(pool.contains(&b.hash));
        assert_eq!(freed, a.raw_rlp.len());
        assert_eq!(pool.size(), 2);

        // A newcomer that does not beat the lowest tip (C, 25) is refused.
        assert!(matches!(
            pool.insert(entry(0xD, 0, 1_000, 25), 2, 16, BUMP),
            Err(MempoolError::PoolFull)
        ));
    }

    /// The base fee moves every block, so the eviction order can flip between
    /// inserts: at base 60, B's tip falls to 5 and B becomes the one dropped.
    #[test]
    fn eviction_follows_base_fee_change() {
        let a = entry(0xA, 0, 100, 20);
        let b = entry(0xB, 0, 65, 65);
        let mut pool = pool_with(10, &[a.clone(), b.clone()], 2);

        pool.set_base_fee(60);
        // C: tip 10 at base 60 beats B (5) but not A (20).
        let (_, freed) = pool.insert(entry(0xC, 0, 100, 10), 2, 16, BUMP).unwrap();
        assert!(!pool.contains(&b.hash), "B (lowest tip at base 60) evicted");
        assert!(pool.contains(&a.hash));
        assert_eq!(freed, b.raw_rlp.len());

        // Back to base 10: C (tip 10) is now the lowest; a tip-15 newcomer evicts it.
        pool.set_base_fee(10);
        let c_hash = entry(0xC, 0, 100, 10).hash;
        pool.insert(entry(0xD, 0, 100, 15), 2, 16, BUMP).unwrap();
        assert!(!pool.contains(&c_hash));
        assert!(pool.contains(&a.hash));
    }

    /// Below-floor txs (max fee under the current base fee) rank below every
    /// payable tx, so they are evicted first.
    #[test]
    fn eviction_prefers_below_floor_tx() {
        let a = entry(0xA, 0, 50, 1);
        let b = entry(0xB, 0, 1_000, 1_000);
        let mut pool = pool_with(10, &[a.clone(), b.clone()], 2);
        // At base 500, A (max 50) is below the floor; B still pays tip 500.
        pool.set_base_fee(500);
        pool.insert(entry(0xC, 0, 600, 1), 2, 16, BUMP).unwrap();
        assert!(!pool.contains(&a.hash), "below-floor A evicted");
        assert!(pool.contains(&b.hash));
    }

    /// Every removal path must find the entry in the eviction index after a
    /// base fee change (the index is re-keyed, never left stale).
    #[test]
    fn index_stays_consistent_across_base_fee_changes() {
        let entries = [
            entry(0xA, 0, 100, 20),
            entry(0xA, 1, 90, 30),
            entry(0xB, 0, 65, 65),
            entry(0xC, 0, 40, 2),
        ];
        let mut pool = pool_with(10, &entries, 100);
        pool.set_base_fee(30);
        pool.remove_by_hash(&entries[2].hash).unwrap();
        pool.set_base_fee(35);
        assert_eq!(
            pool.prune_confirmed(&entries[0].sender, 1),
            (1, entries[0].raw_rlp.len())
        );
        pool.set_base_fee(10);
        pool.drain(1_000_000, None, &B256::ZERO, 10);
        assert_eq!(pool.size(), 0);
        assert!(pool.by_price.is_empty(), "no stale keys left in by_price");
        assert!(pool.by_hash.is_empty());
    }
}
