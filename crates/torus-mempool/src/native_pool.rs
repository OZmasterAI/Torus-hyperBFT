//! Native action pool with per-sender tracking, dedup, and size limits.
//!
//! Task 3.1.4: Hardens the native pool that previously had no per-sender
//! limits and no dedup. Adds pool size cap with priority-based eviction.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Bound;

use alloy_primitives::{Address, B256};
use torus_types::{compute_action_hash, NativeAction, SignedNativeAction};

use crate::error::MempoolError;

/// Selection-order key (T2.3): `(priority class, sender, nonce, seq)`.
///
/// Priority classes (s517 oracle feeder): [`PRIO_CANCEL`] (0) <
/// [`PRIO_ORACLE`] (1) < [`PRIO_NORMAL`] (2) — cancels first, then oracle
/// submissions, then everything else. Node-local: only the proposer selects.
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
///
/// Anti-spam item C: the key is unchanged, but selection no longer takes the
/// whole cancel range first — cancels get at most `cancel_share_pct` of each
/// selected block ahead of non-cancels (see [`NativePool::select_entries`]).
type SortKey = (u8, Address, u64, u64);

/// Priority class of a cancel ([`is_cancel`]).
pub const PRIO_CANCEL: u8 = 0;
/// Priority class of an oracle submission ([`is_oracle_submission`]).
pub const PRIO_ORACLE: u8 = 1;
/// Priority class of every other action.
pub const PRIO_NORMAL: u8 = 2;

/// The smallest possible non-cancel key: `entries.range(..FIRST_NON_CANCEL)`
/// is exactly the pooled cancels, `range(FIRST_NON_CANCEL..)` the rest.
const FIRST_NON_CANCEL: SortKey = (PRIO_ORACLE, Address::ZERO, 0, 0);

/// The smallest possible normal key: `range(..FIRST_NORMAL)` is the pooled
/// cancels and oracle submissions (the priority-only pacing tier's scope).
const FIRST_NORMAL: SortKey = (PRIO_NORMAL, Address::ZERO, 0, 0);

/// What [`SelectionWalk::offer`] did with one entry.
#[derive(PartialEq)]
enum Offer {
    Taken,
    Skipped,
    /// A byte/order budget would be exceeded: selection ends here
    /// (deterministic prefix).
    Stop,
}

/// Per-block selection state shared by the cancel-share phases.
struct SelectionWalk<'a> {
    limit: usize,
    exclude: &'a HashSet<B256>,
    bytes_cap: usize,
    orders_cap: usize,
    max_per_block: usize,
    block_counts: HashMap<Address, usize>,
    bytes_used: usize,
    orders_used: usize,
    selected: Vec<(&'a SortKey, &'a NativePoolEntry)>,
}

impl<'a> SelectionWalk<'a> {
    fn full(&self) -> bool {
        self.selected.len() >= self.limit
    }

    fn offer(&mut self, key: &'a SortKey, entry: &'a NativePoolEntry) -> Offer {
        // Skip in-flight (already-proposed) entries BEFORE the budget gates:
        // an excluded entry is never selected, so it must neither charge nor
        // trip the byte/order budget — otherwise a large in-flight batch
        // sitting early in the sort would halt selection and under-fill the
        // block with valid later entries.
        if self.exclude.contains(&entry.action_hash) {
            return Offer::Skipped;
        }
        if self.bytes_used.saturating_add(entry.encoded_len) > self.bytes_cap {
            // Deterministic prefix: stop at the first entry that would blow
            // the body-byte budget rather than skipping past it.
            return Offer::Stop;
        }
        let entry_orders = crate::rate_limit::order_count(&entry.action.action);
        if self.orders_used.saturating_add(entry_orders) > self.orders_cap {
            // Deterministic prefix for the ORDER budget too (O2/G2) —
            // bounds worst-case matching/exec time per block.
            return Offer::Stop;
        }
        let count = self.block_counts.entry(entry.sender).or_insert(0);
        if *count >= self.max_per_block {
            return Offer::Skipped;
        }
        *count += 1;
        self.bytes_used = self.bytes_used.saturating_add(entry.encoded_len);
        self.orders_used = self.orders_used.saturating_add(entry_orders);
        self.selected.push((key, entry));
        Offer::Taken
    }
}

/// Same identity as SortKey, ordered by nonce for expiry-prefix removal.
/// No stale heap entries: every insert/remove maintains this exact live index.
type ExpiryKey = (u64, u8, Address, u64);

fn expiry_key(&(priority, sender, nonce, seq): &SortKey) -> ExpiryKey {
    (nonce, priority, sender, seq)
}

/// What [`NativePool::evict_expired`] removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Expired {
    /// Every expired entry.
    pub all: usize,
    /// The expired oracle submissions ([`PRIO_ORACLE`]) among them.
    pub oracle: usize,
}

/// Entry in the native action pool with pre-recovered sender and dedup hash.
pub(crate) struct NativePoolEntry {
    pub sender: Address,
    pub action: SignedNativeAction,
    pub action_hash: B256,
    /// [`priority_class`] of the action (the first [`SortKey`] field).
    pub priority: u8,
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
    /// Pooled cancels and oracle submissions (every non-[`PRIO_NORMAL`]
    /// entry), so admission can measure the normal backlog alone.
    priority_count: usize,
    max_size: usize,
    max_per_sender: usize,
    max_per_block: usize,
    /// Anti-spam item C: max share (percent) of a selected block that cancels
    /// may take ahead of non-cancels. 100 = the old unbounded cancel priority.
    cancel_share_pct: u8,
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
            priority_count: 0,
            max_size,
            max_per_sender,
            max_per_block,
            cancel_share_pct: crate::rate_limit::DEFAULT_CANCEL_BLOCK_SHARE_PCT,
        }
    }

    /// Set the cancel share of each selected block (clamped to 100).
    pub fn set_cancel_share_pct(&mut self, pct: u8) {
        self.cancel_share_pct = pct.min(100);
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// Pooled [`PRIO_NORMAL`] actions: the backlog the admission limit
    /// measures, so pooled cancels (item C) and oracle submissions (bounded
    /// per validator, merge with the crab stack) never make ingress shed
    /// orders.
    pub fn normal_size(&self) -> usize {
        self.entries.len() - self.priority_count
    }

    /// True when the pool is at capacity — every insert but an oracle
    /// submission is guaranteed to fail (item C: cancels no longer evict
    /// their way in; s517: an oracle submission may evict a normal entry).
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
    /// A full pool rejects every action, cancels included (item C: a cancel
    /// used to evict the last pending non-cancel, which let cancel spam push
    /// every order out of the pool).
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
        let priority = priority_class(&action.action);
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

        // Pool size cap. Item C: no eviction — rejecting the newcomer is the
        // same rule for every action kind but oracle submissions. Evicting another cancel instead
        // would let one spammer churn other users' cancels out; evicting an
        // order was the starvation vector. A full pool is transient: the
        // admission limit sheds non-cancels well before it.
        if self.entries.len() >= self.max_size {
            if priority != PRIO_ORACLE {
                return Err(MempoolError::NativePoolFull);
            }
            // s517 x item C: an oracle submission (gated to Active validators
            // and their signers, capped per validator) evicts the lowest-
            // priority normal entry — the LAST entry in selection order — or,
            // with no normal entry left, the LAST pooled cancel, so a pool
            // full of cancel spam cannot keep prices out. Bounded churn: the
            // mempool's per-validator oracle cap limits how many cancels the
            // whole validator set can displace. Oracle submissions never
            // evict each other.
            let last_normal = self
                .entries
                .iter()
                .next_back()
                .filter(|(_, entry)| entry.priority == PRIO_NORMAL);
            let last_cancel = || self.entries.range(..FIRST_NON_CANCEL).next_back();
            let evict_key = match last_normal.or_else(last_cancel) {
                Some((key, _)) => *key,
                None => return Err(MempoolError::NativePoolFull),
            };
            self.remove_entry_by_key(&evict_key);
        }

        *self.sender_counts.entry(sender).or_insert(0) += 1;
        self.seen.insert((sender, action_hash));
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let key: SortKey = (priority, sender, action.nonce, seq);
        self.hash_index.entry(action_hash).or_default().push(key);
        self.expiry_index.insert(expiry_key(&key));
        self.entries.insert(
            key,
            NativePoolEntry {
                sender,
                action,
                action_hash,
                priority,
                encoded_len,
                restash_key,
            },
        );
        self.priority_count += usize::from(priority != PRIO_NORMAL);

        Ok(action_hash)
    }

    /// Remove one entry by its selection-order key, maintaining `seen`,
    /// `sender_counts`, and `hash_index` (this key is dropped from the hash's
    /// `Vec`; the mapping itself only when no same-hash duplicate survives).
    /// The single choke point for index maintenance on every removal path.
    fn remove_entry_by_key(&mut self, key: &SortKey) -> Option<NativePoolEntry> {
        let entry = self.entries.remove(key)?;
        self.priority_count -= usize::from(entry.priority != PRIO_NORMAL);
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

    /// Item C selection walk shared by every block-selection entry point.
    ///
    /// 1. cancels, in [`SortKey`] order, until they hold
    ///    `ceil(limit * cancel_share_pct / 100)` slots;
    /// 2. non-cancels in their unchanged [`SortKey`] order (sender, nonce) —
    ///    s66: an arrival-order walk here collapsed throughput, so this phase
    ///    is exactly the old non-cancel walk. Oracle submissions
    ///    ([`PRIO_ORACLE`]) sort before normal entries, so they lead this
    ///    phase: cancels at the share never crowd them out (merge with the
    ///    crab stack; their count is bounded by the per-validator cap);
    /// 3. work-conserving: if slots remain, the cancels after the last one
    ///    phase 1 examined.
    ///
    /// Each phase is a `BTreeMap::range` over the incrementally sorted pool —
    /// no sort, no extra allocation beyond the result. Budgets, the
    /// per-sender cap and the in-flight exclusion apply across all phases; a
    /// budget stop ends the whole selection (deterministic prefix). At 100 %
    /// phase 1 can take the whole block, which is exactly the old
    /// cancels-first walk.
    fn select_entries<'a>(
        &'a self,
        limit: usize,
        exclude: &'a HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
    ) -> Vec<(&'a SortKey, &'a NativePoolEntry)> {
        self.select_entries_before(limit, exclude, bytes_cap, orders_cap, Bound::Unbounded)
    }

    /// [`Self::select_entries`] with phase 2 ending before `end`
    /// (`Excluded(FIRST_NORMAL)` = the priority-only pacing tier).
    fn select_entries_before<'a>(
        &'a self,
        limit: usize,
        exclude: &'a HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
        end: Bound<SortKey>,
    ) -> Vec<(&'a SortKey, &'a NativePoolEntry)> {
        if self.max_per_block == 0 {
            return Vec::new();
        }
        let cancel_cap = (limit as u128 * u128::from(self.cancel_share_pct)).div_ceil(100);
        let cancel_cap = usize::try_from(cancel_cap).unwrap_or(usize::MAX).min(limit);
        let mut walk = SelectionWalk {
            limit,
            exclude,
            bytes_cap,
            orders_cap,
            max_per_block: self.max_per_block,
            block_counts: HashMap::new(),
            bytes_used: 0,
            orders_used: 0,
            selected: Vec::new(),
        };

        // Phase 1: the bounded cancel prefix.
        let mut resume_after: Option<&SortKey> = None;
        let mut cancels_left = false;
        for (key, entry) in self.entries.range(..FIRST_NON_CANCEL) {
            if walk.selected.len() >= cancel_cap {
                cancels_left = true;
                break;
            }
            if walk.offer(key, entry) == Offer::Stop {
                return walk.selected;
            }
            resume_after = Some(key);
        }
        // Phase 2: non-cancels, unchanged order.
        for (key, entry) in self.entries.range((Bound::Included(FIRST_NON_CANCEL), end)) {
            if walk.full() {
                return walk.selected;
            }
            if walk.offer(key, entry) == Offer::Stop {
                return walk.selected;
            }
        }
        // Phase 3: leftover slots go to the remaining cancels.
        if cancels_left {
            let from = resume_after.map_or(Bound::Unbounded, |k| Bound::Excluded(*k));
            for (key, entry) in self
                .entries
                .range((from, Bound::Excluded(FIRST_NON_CANCEL)))
            {
                if walk.full() || walk.offer(key, entry) == Offer::Stop {
                    break;
                }
            }
        }
        walk.selected
    }

    /// Drain up to `limit` actions in selection order (see
    /// [`Self::select_entries`]) with per-sender-per-block caps.
    /// Excess actions from rate-limited senders stay in pool for next block.
    pub fn drain(&mut self, limit: usize) -> Vec<SignedNativeAction> {
        let none = HashSet::new();
        let take_keys: Vec<SortKey> = self
            .select_entries(limit, &none, usize::MAX, usize::MAX)
            .into_iter()
            .map(|(key, _)| *key)
            .collect();

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
    /// Uses the same order as `drain`. Read-only (T2.3): callers select
    /// from a shared snapshot under the read lock — no sort, no index rebuild.
    pub fn select_for_block(&self, limit: usize) -> Vec<SignedNativeAction> {
        let none = HashSet::new();
        self.select_entries(limit, &none, usize::MAX, usize::MAX)
            .into_iter()
            .map(|(_, entry)| entry.action.clone())
            .collect()
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
    /// Item C: cancels take at most `cancel_share_pct` of the block ahead of
    /// non-cancels ([`Self::select_entries`]).
    pub fn select_for_block_with_senders_excluding(
        &self,
        limit: usize,
        exclude: &HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
    ) -> Vec<(Address, SignedNativeAction)> {
        self.select_entries(limit, exclude, bytes_cap, orders_cap)
            .into_iter()
            .map(|(_, entry)| (entry.sender, entry.action.clone()))
            .collect()
    }

    /// Priority-ONLY selection (Package D rank 1, deepest exec-backlog pacing
    /// tier; s517: cancels and oracle submissions): the item-C walk of
    /// [`Self::select_entries`] (same phases, budgets, per-sender cap and
    /// in-flight exclusion) with phase 2 ending at the first [`PRIO_NORMAL`]
    /// key. Priority classes sort FIRST in [`SortKey`], so this touches
    /// exactly the pooled priority prefix and never walks the (possibly
    /// huge) normal tail. The name is kept from the cancels-only era.
    /// Non-destructive like every selection: paced-out normal entries stay
    /// pooled and remain fully selectable by the normal path — pacing
    /// defers, never sheds.
    ///
    /// Merge with the crab stack: cancels take at most `cancel_share_pct`
    /// ahead of the oracle submissions, then (work-conserving) the rest of
    /// the block. Without oracle submissions pooled this is exactly the old
    /// all-cancels walk; with them, cancel spam cannot crowd prices out of
    /// the deepest pacing tier.
    pub fn select_cancels_for_block_with_senders_excluding(
        &self,
        limit: usize,
        exclude: &HashSet<B256>,
        bytes_cap: usize,
        orders_cap: usize,
    ) -> Vec<(Address, SignedNativeAction)> {
        self.select_entries_before(
            limit,
            exclude,
            bytes_cap,
            orders_cap,
            Bound::Excluded(FIRST_NORMAL),
        )
        .into_iter()
        .map(|(_, entry)| (entry.sender, entry.action.clone()))
        .collect()
    }

    /// Evict entries whose nonce has aged out of the protocol validity window.
    ///
    /// An action with `nonce + NONCE_WINDOW_MS < now_ms` can never pass
    /// admission/validation again, so keeping it selectable only lets leaders
    /// propose blocks that cannot validate — the s334 bs1000 wedge had no
    /// self-heal precisely because nothing ever removed these. Called lazily
    /// from the selection/drain wrappers. Returns the number evicted, and how
    /// many of them are oracle submissions (plan 9.14 C telemetry).
    pub fn evict_expired(&mut self, now_ms: u64) -> Expired {
        use torus_types::eip712::NONCE_WINDOW_MS;
        // nonce.saturating_add(window) < now is equivalent to nonce <
        // now-window when subtraction succeeds; otherwise nothing is expired.
        // In particular, nonce+window==now and saturated u64::MAX stay live.
        let Some(cutoff) = now_ms.checked_sub(NONCE_WINDOW_MS) else {
            return Expired::default();
        };
        let mut removed = Expired::default();
        while let Some(&(nonce, priority, sender, seq)) = self.expiry_index.first() {
            if nonce >= cutoff {
                break;
            }
            self.remove_entry_by_key(&(priority, sender, nonce, seq));
            removed.all += 1;
            removed.oracle += usize::from(priority == PRIO_ORACLE);
        }
        removed
    }

    /// Pooled oracle submissions whose sender is any of `accounts` (a
    /// validator and its hot signer). The per-validator cap (M2) reads this
    /// under the same pool write lock as the insert.
    pub fn oracle_pending(&self, accounts: &[Address]) -> usize {
        accounts
            .iter()
            .map(|a| {
                self.entries
                    .range((PRIO_ORACLE, *a, 0, 0)..=(PRIO_ORACLE, *a, u64::MAX, u64::MAX))
                    .count()
            })
            .sum()
    }

    /// Whether this exact `(sender, action)` is already pooled (the dedup identity).
    pub fn contains(&self, sender: &Address, action: &SignedNativeAction) -> bool {
        self.seen.contains(&(*sender, compute_action_hash(action)))
    }

    /// Review M1(a): evict the OLDEST pooled oracle submission of `accounts`
    /// (lowest nonce, then insertion order) if it is older than `nonce`.
    /// Returns whether one was evicted.
    pub fn evict_oldest_oracle_older_than(&mut self, accounts: &[Address], nonce: u64) -> bool {
        let oldest = accounts
            .iter()
            .flat_map(|a| {
                self.entries
                    .range((PRIO_ORACLE, *a, 0, 0)..=(PRIO_ORACLE, *a, u64::MAX, u64::MAX))
                    .map(|(k, _)| *k)
            })
            .min_by_key(|&(_, _, n, seq)| (n, seq));
        match oldest {
            Some(key) if key.2 < nonce => {
                self.remove_entry_by_key(&key);
                true
            }
            _ => false,
        }
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
        assert_eq!(
            self.priority_count,
            self.entries
                .values()
                .filter(|e| e.priority != PRIO_NORMAL)
                .count()
        );
        for key in self.entries.keys() {
            assert!(
                self.expiry_index.contains(&expiry_key(key)),
                "missing expiry key"
            );
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

/// Cancels sort first in the pool key and may take a bounded share of each
/// block ahead of non-cancels (item C); ingress also uses this to keep cancels
/// past the admission limit.
pub fn is_cancel(action: &NativeAction) -> bool {
    matches!(
        action,
        NativeAction::CancelOrder { .. } | NativeAction::CancelAllOrders { .. }
    )
}

/// A validator's (or its hot signer's) oracle price submission (s517).
pub fn is_oracle_submission(action: &NativeAction) -> bool {
    matches!(action, NativeAction::SubmitOraclePrices(_))
}

/// Cancels and oracle submissions: pool-eviction priority, the RPC
/// backlog / pool-full bypass, and the priority-only pacing tier.
pub fn is_priority(action: &NativeAction) -> bool {
    is_cancel(action) || is_oracle_submission(action)
}

/// The [`SortKey`] priority class of `action`.
pub fn priority_class(action: &NativeAction) -> u8 {
    if is_cancel(action) {
        PRIO_CANCEL
    } else if is_oracle_submission(action) {
        PRIO_ORACLE
    } else {
        PRIO_NORMAL
    }
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
    fn normal_size_tracks_inserts_and_every_removal() {
        let mut pool = NativePool::new(100, 64, 100);
        let cancel = |n| make_action(n, NativeAction::CancelAllOrders { market_id: None });
        let other = |n| make_action(n, NativeAction::ClaimRewards);
        let s = Address::repeat_byte(7);
        let c1 = pool.insert(s, cancel(1)).unwrap();
        pool.insert(s, cancel(2)).unwrap();
        let o1 = pool.insert(s, other(3)).unwrap();
        pool.insert(s, other(4)).unwrap();
        pool.insert(s, other(5)).unwrap();
        assert_eq!((pool.size(), pool.normal_size()), (5, 3));
        // Merge with the crab stack: oracle submissions are not backlog either.
        let or1 = pool.insert(s, oracle(6)).unwrap();
        assert_eq!((pool.size(), pool.normal_size()), (6, 3));
        pool.remove_committed(&[c1, or1]);
        assert_eq!((pool.size(), pool.normal_size()), (4, 3));
        pool.remove_committed(&[o1]);
        assert_eq!((pool.size(), pool.normal_size()), (3, 2));
        pool.drain(100);
        assert_eq!((pool.size(), pool.normal_size()), (0, 0));
        pool.assert_index_consistent();
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
                make_action(
                    2,
                    NativeAction::CancelOrder {
                        order_id: i as u128,
                    },
                ),
            )
            .unwrap();
        }
        for limit in [0, 1, usize::MAX] {
            assert!(pool.select_for_block(limit).is_empty());
            assert!(pool.select_for_block_with_senders(limit).is_empty());
            assert!(pool
                .select_cancels_for_block_with_senders_excluding(
                    limit,
                    &HashSet::new(),
                    usize::MAX,
                    usize::MAX,
                )
                .is_empty());
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
                candidate.evict_expired(now).all,
                evict_expired_reference(&mut reference, now)
            );
            assert_same_pool(&candidate, &reference);
            assert_eq!(candidate.evict_expired(now).all, 0);
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
                        candidate.evict_expired(now).all,
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

        let evicted = pool.evict_expired(now).all;
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
        let capped = pool.select_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(capped.len(), 100, "limit=100 reproduces today's ceiling");

        // Raised cap: the block carries all 400 — proof the limit is the sole
        // count gate and 400 is reachable end-to-end in selection.
        let raised = pool.select_for_block_with_senders_excluding(
            400,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(
            raised.len(),
            400,
            "limit=400 genuinely selects >100 actions"
        );

        // A limit between the two resolves exactly, no hidden clamp near 100.
        let mid = pool.select_for_block_with_senders_excluding(
            250,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
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
        // Item C: 100 % = the old unbounded cancel priority, which this
        // reference pins byte-for-byte (the kill switch must restore it).
        pool.set_cancel_share_pct(100);
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

    /// Item C: a cancel arriving at a full pool is rejected like any other
    /// action — it no longer evicts a pending order (the cancel-spam
    /// starvation vector), and the pool is left untouched.
    #[test]
    fn pool_size_cap_rejects_cancel_without_evicting() {
        let mut pool = NativePool::new(2, 64, 16);
        let a = Address::repeat_byte(1);
        let b = Address::repeat_byte(2);
        let c = Address::repeat_byte(3);
        let action_a = make_action(1, NativeAction::ClaimRewards);
        let action_b = make_action(2, NativeAction::ClaimRewards);
        let ha = compute_action_hash(&action_a);
        let hb = compute_action_hash(&action_b);

        pool.insert(a, action_a).unwrap();
        pool.insert(b, action_b).unwrap();
        let err = pool
            .insert(c, make_action(3, NativeAction::CancelOrder { order_id: 1 }))
            .unwrap_err();
        assert!(matches!(err, MempoolError::NativePoolFull));
        assert_eq!(pool.size(), 2);
        assert!(pool.get_by_hash(&ha).is_some() && pool.get_by_hash(&hb).is_some());
        let err = pool
            .insert(
                c,
                make_action(4, NativeAction::CancelAllOrders { market_id: None }),
            )
            .unwrap_err();
        assert!(matches!(err, MempoolError::NativePoolFull));
        pool.assert_index_consistent();
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
        assert_eq!(
            normal.len(),
            6,
            "pacing defers non-cancels, never drops them"
        );
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
        pool.insert(Address::repeat_byte(1), action.clone())
            .unwrap();
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
    /// After one same-hash duplicate is removed (drain), the survivor must
    /// remain findable and later removable — the guarantee
    /// `repoint_hash_survivors` used to provide under the single-slot index.
    #[test]
    fn duplicate_survivor_findable_after_partial_removal() {
        let mut pool = NativePool::new(100, 64, 16);
        let action = make_action(1, NativeAction::ClaimRewards);
        let hash = compute_action_hash(&action);
        pool.insert(Address::repeat_byte(1), action.clone())
            .unwrap();
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

        assert_eq!(pool.evict_expired(now).all, 2);
        assert_eq!(pool.size(), 0);
        assert!(pool.get_by_hash(&hash).is_none());
        pool.assert_index_consistent();

        pool.insert(Address::repeat_byte(1), stale.clone()).unwrap();
        pool.insert(Address::repeat_byte(2), stale).unwrap();
        assert_eq!(pool.size(), 2);
        pool.assert_index_consistent();
    }

    // ---- s517 oracle feeder M1: classes cancel(0) < oracle(1) < rest(2) ----

    fn oracle(nonce: u64) -> SignedNativeAction {
        make_action(
            nonce,
            NativeAction::SubmitOraclePrices(torus_types::OracleSubmission {
                prices: vec![(1, torus_types::FixedPoint::ONE)],
                timestamp: nonce,
            }),
        )
    }

    /// Plan 9.14 C (s100): `evict_expired` reports how many of the expired
    /// entries are oracle submissions (the `expired` oracle-drop metric).
    #[test]
    fn evict_expired_counts_oracle_submissions() {
        use torus_types::eip712::NONCE_WINDOW_MS;
        let mut pool = NativePool::new(100, 64, 16);
        let now = 10 * NONCE_WINDOW_MS;
        let stale = now - 2 * NONCE_WINDOW_MS;
        pool.insert(Address::repeat_byte(1), oracle(stale)).unwrap();
        pool.insert(Address::repeat_byte(2), oracle(stale + 1))
            .unwrap();
        pool.insert(
            Address::repeat_byte(3),
            make_action(stale, NativeAction::ClaimRewards),
        )
        .unwrap();
        pool.insert(
            Address::repeat_byte(4),
            make_action(stale, NativeAction::CancelOrder { order_id: 1 }),
        )
        .unwrap();
        pool.insert(Address::repeat_byte(5), oracle(now)).unwrap();
        assert_eq!(pool.evict_expired(now), Expired { all: 4, oracle: 2 });
        assert_eq!(pool.size(), 1);
        assert_eq!(pool.evict_expired(now), Expired::default());
        pool.assert_index_consistent();
    }

    #[test]
    fn selection_order_is_cancels_then_oracle_then_rest() {
        let mut pool = NativePool::new(100, 64, 64);
        pool.insert(
            Address::repeat_byte(1),
            make_action(1, NativeAction::ClaimRewards),
        )
        .unwrap();
        pool.insert(
            Address::repeat_byte(5),
            make_action(2, NativeAction::CancelOrder { order_id: 9 }),
        )
        .unwrap();
        pool.insert(Address::repeat_byte(9), oracle(3)).unwrap();
        let sel = pool.select_for_block(10);
        let kinds: Vec<u8> = sel.iter().map(|a| priority_class(&a.action)).collect();
        assert_eq!(kinds, vec![PRIO_CANCEL, PRIO_ORACLE, PRIO_NORMAL]);
        assert!(is_cancel(&sel[0].action));
        assert!(is_oracle_submission(&sel[1].action));
        assert!(matches!(sel[2].action, NativeAction::ClaimRewards));
        assert!(
            is_priority(&sel[0].action)
                && is_priority(&sel[1].action)
                && !is_priority(&sel[2].action)
        );
        pool.assert_index_consistent();
    }

    #[test]
    fn full_pool_oracle_submission_evicts_a_normal_entry() {
        let mut pool = NativePool::new(2, 64, 64);
        pool.insert(
            Address::repeat_byte(1),
            make_action(1, NativeAction::ClaimRewards),
        )
        .unwrap();
        pool.insert(
            Address::repeat_byte(2),
            make_action(2, NativeAction::ClaimRewards),
        )
        .unwrap();
        assert!(matches!(
            pool.insert(
                Address::repeat_byte(3),
                make_action(3, NativeAction::ClaimRewards)
            ),
            Err(MempoolError::NativePoolFull)
        ));
        pool.insert(Address::repeat_byte(4), oracle(4)).unwrap();
        assert_eq!(pool.size(), 2);
        assert_eq!(pool.oracle_pending(&[Address::repeat_byte(4)]), 1);
        pool.assert_index_consistent();
    }

    /// Merge with the crab stack: with no normal entry left, an oracle
    /// submission evicts the LAST pooled cancel (cancel spam cannot keep
    /// prices out). A cancel never evicts anything (item C), and oracle
    /// submissions never evict each other.
    #[test]
    fn full_pool_of_cancels_oracle_evicts_the_last_cancel() {
        let mut pool = NativePool::new(3, 64, 64);
        let cancel = |n| {
            make_action(
                n,
                NativeAction::CancelOrder {
                    order_id: n as u128,
                },
            )
        };
        for i in 1..=3u8 {
            pool.insert(Address::repeat_byte(i), cancel(i as u64))
                .unwrap();
        }
        assert!(matches!(
            pool.insert(Address::repeat_byte(9), cancel(9)),
            Err(MempoolError::NativePoolFull)
        ));
        pool.insert(Address::repeat_byte(0xEE), oracle(4)).unwrap();
        assert_eq!(pool.size(), 3);
        assert_eq!(pool.oracle_pending(&[Address::repeat_byte(0xEE)]), 1);
        let senders: Vec<Address> = pool.entries.values().map(|e| e.sender).collect();
        assert_eq!(
            senders,
            vec![
                Address::repeat_byte(1),
                Address::repeat_byte(2),
                Address::repeat_byte(0xEE)
            ],
            "the last cancel in selection order was evicted"
        );
        // ...a cancel never evicts an oracle submission.
        let mut pool = NativePool::new(1, 64, 64);
        pool.insert(Address::repeat_byte(2), oracle(2)).unwrap();
        assert!(matches!(
            pool.insert(Address::repeat_byte(1), cancel(1)),
            Err(MempoolError::NativePoolFull)
        ));
        // ...and oracle submissions never evict each other.
        assert!(matches!(
            pool.insert(Address::repeat_byte(3), oracle(3)),
            Err(MempoolError::NativePoolFull)
        ));
        pool.assert_index_consistent();
    }

    /// Merge with the crab stack: cancels at the item-C cap do not crowd the
    /// oracle lane out — it sits right after the bounded cancel prefix, on
    /// every selection entry point, the priority-only pacing tier included.
    #[test]
    fn oracle_lane_follows_the_capped_cancel_prefix() {
        let mut pool = mixed_pool(40, 30, 25);
        let v = Address::repeat_byte(0xEE);
        pool.insert(v, oracle(5_000)).unwrap();
        let sel = pool.select_for_block_with_senders_excluding(
            20,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(sel.len(), 20);
        assert!(
            cancel_flags(&sel)[..5].iter().all(|c| *c),
            "ceil(25% of 20) cancels first"
        );
        assert_eq!(sel[5].0, v);
        assert!(is_oracle_submission(&sel[5].1.action));
        assert_eq!(cancel_flags(&sel).iter().filter(|c| **c).count(), 5);
        // The pacing tier: cancels alone would fill all 10 slots.
        let paced = pool.select_cancels_for_block_with_senders_excluding(
            10,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(paced.len(), 10);
        assert!(
            cancel_flags(&paced)[..3].iter().all(|c| *c),
            "ceil(25% of 10)"
        );
        assert_eq!(paced[3].0, v, "the oracle lane follows the cancel share");
        assert!(
            cancel_flags(&paced)[4..].iter().all(|c| *c),
            "work-conserving: cancels fill the rest"
        );
        assert!(
            paced.iter().all(|(_, a)| is_priority(&a.action)),
            "never a normal entry"
        );
        // drain takes the same block.
        let drained = pool.drain(20);
        assert!(is_oracle_submission(&drained[5].action));
        pool.assert_index_consistent();
    }

    #[test]
    fn oracle_pending_counts_across_validator_and_signer() {
        let (v, s, other) = (
            Address::repeat_byte(7),
            Address::repeat_byte(8),
            Address::repeat_byte(9),
        );
        let mut pool = NativePool::new(100, 64, 64);
        let first = oracle(1);
        let first_hash = compute_action_hash(&first);
        pool.insert(v, first).unwrap();
        pool.insert(v, oracle(2)).unwrap();
        pool.insert(s, oracle(3)).unwrap();
        pool.insert(other, oracle(4)).unwrap();
        pool.insert(v, make_action(5, NativeAction::ClaimRewards))
            .unwrap();
        pool.insert(v, make_action(6, NativeAction::CancelOrder { order_id: 1 }))
            .unwrap();
        assert_eq!(pool.oracle_pending(&[v, s]), 3);
        assert_eq!(pool.oracle_pending(&[v]), 2);
        assert_eq!(pool.oracle_pending(&[]), 0);
        pool.remove_committed(&[first_hash]);
        assert_eq!(pool.oracle_pending(&[v, s]), 2);
        pool.assert_index_consistent();
    }

    #[test]
    fn priority_only_selection_takes_cancels_and_oracle() {
        let mut pool = NativePool::new(100, 64, 64);
        pool.insert(
            Address::repeat_byte(1),
            make_action(1, NativeAction::ClaimRewards),
        )
        .unwrap();
        pool.insert(
            Address::repeat_byte(5),
            make_action(2, NativeAction::CancelOrder { order_id: 9 }),
        )
        .unwrap();
        pool.insert(Address::repeat_byte(9), oracle(3)).unwrap();
        let sel = pool.select_cancels_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(sel.len(), 2);
        assert!(is_cancel(&sel[0].1.action));
        assert!(is_oracle_submission(&sel[1].1.action));
        assert_eq!(pool.size(), 3, "non-destructive");
    }

    // ---- Anti-spam item C: bounded cancel prefix ----

    /// `n` cancels then `m` non-cancels, each from its own sender.
    fn mixed_pool(cancels: u8, others: u8, pct: u8) -> NativePool {
        let mut pool = NativePool::new(1_000, 64, 16);
        pool.set_cancel_share_pct(pct);
        for i in 0..others {
            pool.insert(
                Address::repeat_byte(100 + i),
                make_action(1_000 + i as u64, NativeAction::ClaimRewards),
            )
            .unwrap();
        }
        for i in 0..cancels {
            pool.insert(
                Address::repeat_byte(1 + i),
                make_action(
                    2_000 + i as u64,
                    NativeAction::CancelAllOrders { market_id: None },
                ),
            )
            .unwrap();
        }
        pool
    }

    fn cancel_flags(sel: &[(Address, SignedNativeAction)]) -> Vec<bool> {
        sel.iter().map(|(_, a)| is_cancel(&a.action)).collect()
    }

    #[test]
    fn cancel_share_bounds_cancels_when_orders_wait() {
        let pool = mixed_pool(30, 30, 25);
        let sel = pool.select_for_block_with_senders_excluding(
            20,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(sel.len(), 20);
        let flags = cancel_flags(&sel);
        assert_eq!(flags.iter().filter(|c| **c).count(), 5, "ceil(25% of 20)");
        assert!(flags[..5].iter().all(|c| *c), "the cancel share goes first");
        // Non-cancel order is unchanged: the lowest (sender, nonce) first.
        let senders: Vec<Address> = sel[5..].iter().map(|(s, _)| *s).collect();
        let want: Vec<Address> = (0..15u8).map(|i| Address::repeat_byte(100 + i)).collect();
        assert_eq!(senders, want);
        // The other selection entry points apply the same share.
        let plain = pool.select_for_block(20);
        assert_eq!(plain.iter().filter(|a| is_cancel(&a.action)).count(), 5);
        let mut drained = mixed_pool(30, 30, 25);
        let d = drained.drain(20);
        assert_eq!(d.iter().filter(|a| is_cancel(&a.action)).count(), 5);
        assert_eq!(drained.size(), 40);
        drained.assert_index_consistent();
    }

    #[test]
    fn cancel_share_is_work_conserving() {
        // Only 3 orders waiting: cancels fill the rest of the block.
        let pool = mixed_pool(30, 3, 25);
        let sel = pool.select_for_block_with_senders(20);
        assert_eq!(sel.len(), 20);
        let flags = cancel_flags(&sel);
        assert_eq!(flags.iter().filter(|c| **c).count(), 17);
        assert!(flags[..5].iter().all(|c| *c));
        assert!(flags[5..8].iter().all(|c| !*c));
        // Every cancel is distinct (resumed after the share, not repeated).
        let hashes: HashSet<B256> = sel.iter().map(|(_, a)| compute_action_hash(a)).collect();
        assert_eq!(hashes.len(), 20);
        // No orders at all: a block of cancels.
        let only = mixed_pool(30, 0, 25);
        assert_eq!(only.select_for_block_with_senders(20).len(), 20);
    }

    #[test]
    fn cancel_share_100_is_old_priority_and_0_puts_orders_first() {
        let old = mixed_pool(30, 30, 100);
        let sel = old.select_for_block_with_senders(20);
        assert!(
            cancel_flags(&sel).iter().all(|c| *c),
            "100 % = all cancels first"
        );
        let zero = mixed_pool(30, 10, 0);
        let sel = zero.select_for_block_with_senders(20);
        let flags = cancel_flags(&sel);
        assert!(flags[..10].iter().all(|c| !*c) && flags[10..].iter().all(|c| *c));
        assert_eq!(sel.len(), 20);
    }

    #[test]
    fn cancel_share_ignores_excluded_cancels_and_keeps_budgets() {
        let pool = mixed_pool(30, 30, 25);
        // The first 5 cancels (lowest senders) are in flight: the share is
        // still filled, by the next 5.
        let in_flight: HashSet<B256> = pool
            .entries
            .values()
            .filter(|e| e.priority == PRIO_CANCEL)
            .take(5)
            .map(|e| e.action_hash)
            .collect();
        let sel =
            pool.select_for_block_with_senders_excluding(20, &in_flight, usize::MAX, usize::MAX);
        assert_eq!(cancel_flags(&sel).iter().filter(|c| **c).count(), 5);
        assert!(sel
            .iter()
            .all(|(_, a)| !in_flight.contains(&compute_action_hash(a))));
        // Order budget still a deterministic prefix across the phases.
        let sel = pool.select_for_block_with_senders_excluding(20, &HashSet::new(), usize::MAX, 8);
        assert_eq!(sel.len(), 8);
        assert_eq!(cancel_flags(&sel).iter().filter(|c| **c).count(), 5);
    }

    #[test]
    fn parse_cancel_block_share_pct_default_clamp_and_off() {
        use crate::rate_limit::parse_cancel_block_share_pct as p;
        assert_eq!(p(None), 25);
        assert_eq!(p(Some("100".into())), 100);
        assert_eq!(p(Some(" 40 ".into())), 40);
        assert_eq!(p(Some("250".into())), 100, "clamped");
        assert_eq!(p(Some("0".into())), 0);
        assert_eq!(p(Some("junk".into())), 25);
    }
}
