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

/// Ordering key for the gas-price-sorted index.
/// Reversed so BTreeSet iteration yields highest gas price first.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TxPriority {
    max_fee_per_gas: u128,
    max_priority_fee: u128,
    hash: B256,
}

impl Ord for TxPriority {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .max_fee_per_gas
            .cmp(&self.max_fee_per_gas)
            .then_with(|| other.max_priority_fee.cmp(&self.max_priority_fee))
            .then_with(|| self.hash.cmp(&other.hash))
    }
}

impl PartialOrd for TxPriority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Heap entry for the drain k-way merge (max-heap by gas price).
///
/// Task 3.1.5 (anti-MEV): `shuffle_key` is a deterministic tiebreaker derived
/// from the parent block hash. Within the same gas price tier, this randomizes
/// ordering so an attacker cannot guarantee placement relative to a victim's tx.
#[derive(Eq, PartialEq)]
struct DrainEntry {
    max_fee_per_gas: u128,
    max_priority_fee: u128,
    sender: Address,
    nonce: u64,
    gas_limit: u64,
    shuffle_key: u64,
}

impl Ord for DrainEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.max_fee_per_gas
            .cmp(&other.max_fee_per_gas)
            .then_with(|| self.max_priority_fee.cmp(&other.max_priority_fee))
            .then_with(|| self.shuffle_key.cmp(&other.shuffle_key))
    }
}

impl PartialOrd for DrainEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compute a deterministic shuffle key for anti-MEV ordering.
/// Derived from parent_hash + sender + nonce so all nodes produce identical order.
fn compute_shuffle_key(parent_hash: &B256, sender: &Address, nonce: u64) -> u64 {
    let mut data = [0u8; 60]; // 32 (hash) + 20 (address) + 8 (nonce)
    data[..32].copy_from_slice(parent_hash.as_slice());
    data[32..52].copy_from_slice(sender.as_slice());
    data[52..60].copy_from_slice(&nonce.to_be_bytes());
    let hash = keccak256(&data);
    u64::from_be_bytes(hash.as_slice()[..8].try_into().unwrap())
}

/// Inner EVM transaction pool (not thread-safe — wrapped by Mempool).
pub(crate) struct EvmPool {
    /// Per-sender nonce-ordered transaction queues.
    by_sender: HashMap<Address, BTreeMap<u64, EvmPoolEntry>>,
    /// Hash -> (sender, nonce) for O(1) lookup.
    by_hash: HashMap<B256, (Address, u64)>,
    /// Gas-price-ordered index (highest first via reversed Ord).
    by_price: BTreeSet<TxPriority>,
    /// Current pool size.
    size: usize,
}

impl EvmPool {
    pub fn new() -> Self {
        Self {
            by_sender: HashMap::new(),
            by_hash: HashMap::new(),
            by_price: BTreeSet::new(),
            size: 0,
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn contains(&self, hash: &B256) -> bool {
        self.by_hash.contains_key(hash)
    }

    /// Insert a validated transaction.
    /// Returns the hash of any replaced transaction (same sender+nonce).
    pub fn insert(
        &mut self,
        entry: EvmPoolEntry,
        max_pool_size: usize,
        max_per_sender: usize,
        replacement_bump_pct: u64,
    ) -> Result<Option<B256>, MempoolError> {
        let hash = entry.hash;
        let sender = entry.sender;
        let nonce = entry.nonce;

        if self.by_hash.contains_key(&hash) {
            return Err(MempoolError::DuplicateTx(hash));
        }

        // Check replacement (same sender + nonce)
        let replaced = if let Some(sender_txs) = self.by_sender.get(&sender) {
            if let Some(existing) = sender_txs.get(&nonce) {
                let min_price = existing
                    .max_fee_per_gas
                    .saturating_mul(100 + replacement_bump_pct as u128)
                    / 100;
                if entry.max_fee_per_gas < min_price {
                    return Err(MempoolError::ReplacementUnderpriced {
                        need_min: min_price,
                        got: entry.max_fee_per_gas,
                    });
                }
                let old_hash = existing.hash;
                self.by_price.remove(&TxPriority {
                    max_fee_per_gas: existing.max_fee_per_gas,
                    max_priority_fee: existing.max_priority_fee,
                    hash: old_hash,
                });
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

        // Pool-wide size limit: evict lowest gas price tx
        if replaced.is_none() && self.size >= max_pool_size {
            if let Some(lowest) = self.by_price.iter().next_back().cloned() {
                if entry.max_fee_per_gas <= lowest.max_fee_per_gas {
                    return Err(MempoolError::PoolFull);
                }
                self.remove_by_hash(&lowest.hash);
            } else {
                return Err(MempoolError::PoolFull);
            }
        }

        // Insert into all indices
        self.by_price.insert(TxPriority {
            max_fee_per_gas: entry.max_fee_per_gas,
            max_priority_fee: entry.max_priority_fee,
            hash,
        });
        self.by_hash.insert(hash, (sender, nonce));
        self.by_sender
            .entry(sender)
            .or_default()
            .insert(nonce, entry);
        self.size += 1;

        Ok(replaced)
    }

    /// Remove a transaction by hash.
    pub fn remove_by_hash(&mut self, hash: &B256) -> Option<EvmPoolEntry> {
        let (sender, nonce) = self.by_hash.remove(hash)?;
        let sender_txs = self.by_sender.get_mut(&sender)?;
        let entry = sender_txs.remove(&nonce)?;
        self.by_price.remove(&TxPriority {
            max_fee_per_gas: entry.max_fee_per_gas,
            max_priority_fee: entry.max_priority_fee,
            hash: entry.hash,
        });
        if sender_txs.is_empty() {
            self.by_sender.remove(&sender);
        }
        self.size -= 1;
        Some(entry)
    }

    /// Drain transactions for a block proposal using k-way merge by gas price.
    ///
    /// For each sender, starts with their lowest-nonce tx. Picks highest gas price
    /// across all senders, advances that sender's pointer, repeats until gas budget
    /// is exhausted. Returns raw RLP bytes for inclusion in `TorusBlock`.
    ///
    /// Task 3.1.4: `per_sender_limit` caps txs per address in a single block.
    /// Task 3.1.5: `parent_hash` seeds deterministic same-price shuffling (anti-MEV).
    pub fn drain(
        &mut self,
        gas_budget: u64,
        per_sender_limit: usize,
        parent_hash: &B256,
    ) -> Vec<Vec<u8>> {
        let mut heap = BinaryHeap::new();
        for (sender, txs) in &self.by_sender {
            if let Some((&nonce, entry)) = txs.iter().next() {
                heap.push(DrainEntry {
                    max_fee_per_gas: entry.max_fee_per_gas,
                    max_priority_fee: entry.max_priority_fee,
                    sender: *sender,
                    nonce,
                    gas_limit: entry.gas_limit,
                    shuffle_key: compute_shuffle_key(parent_hash, sender, nonce),
                });
            }
        }

        let mut result = Vec::new();
        let mut gas_used: u64 = 0;
        let mut to_remove = Vec::new();
        let mut sender_counts: HashMap<Address, usize> = HashMap::new();

        while let Some(top) = heap.pop() {
            // Per-sender-per-block limit (Task 3.1.4): leave excess in pool.
            let count = sender_counts.get(&top.sender).copied().unwrap_or(0);
            if count >= per_sender_limit {
                continue;
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
            *sender_counts.entry(top.sender).or_insert(0) += 1;

            // Advance: push next nonce for this sender.
            let next_nonce = top.nonce + 1;
            if let Some(next) = self
                .by_sender
                .get(&top.sender)
                .and_then(|m| m.get(&next_nonce))
            {
                heap.push(DrainEntry {
                    max_fee_per_gas: next.max_fee_per_gas,
                    max_priority_fee: next.max_priority_fee,
                    sender: top.sender,
                    nonce: next_nonce,
                    gas_limit: next.gas_limit,
                    shuffle_key: compute_shuffle_key(parent_hash, &top.sender, next_nonce),
                });
            }
        }

        for hash in &to_remove {
            self.remove_by_hash(hash);
        }

        result
    }
}
