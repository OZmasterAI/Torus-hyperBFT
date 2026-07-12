//! Offline consensus-state surgery for QC'd-poison-ancestry wedges (S459).
//!
//! # Background
//!
//! A node is "wedged" when its `cf_consensus_meta` persists an uncommitted block
//! tree above the committed frontier that contains a **hole** — a block whose
//! `justify.block` (its parent) references a hash that exists nowhere in the
//! universe (the body was destroyed before it could be synced). The commit walk
//! ([`internal.rs`](super::accessors::internal) `commit`) refuses to advance the
//! committed frontier across such a hole forever, and block-sync can never
//! backfill a body that no longer exists. No restart, rebuild, or resync heals
//! this.
//!
//! This module implements the surgery described in `docs/plans/unwedge-tool.md`:
//! enumerate the uncommitted remainder, delete it with the same per-block
//! primitives the commit path already uses, reset the racing singletons, and
//! install a harvested **recovery PC** whose `.block` is the committed frontier
//! so the node can propose/vote again from a clean tip.
//!
//! # Enumeration is walk-based (no KV iteration)
//!
//! The pluggable [`KVGet`](super::pluggables::KVGet) interface has **no** prefix
//! scan / iteration — it is a pure point-lookup `get(key)`. So the uncommitted
//! set is discovered by *walking* the graph structure that the protocol itself
//! persists:
//!
//! * the children lists of every committed block,
//! * the `NEWEST_BLOCK` singleton walked backwards through `block_justify`,
//! * the `SPECULATIVE_COMMITS` list,
//! * and, transitively, the children of any hole discovered along the way.
//!
//! A fixpoint over these sources reaches every uncommitted block that the
//! protocol can possibly *reference* — because every reference edge (parent via
//! `justify`, child via the children list, tip via `NEWEST_BLOCK`, speculative
//! via `SPECULATIVE_COMMITS`) is one of the sources above. Any block NOT reached
//! by this walk is an **unreachable orphan**: nothing in the block tree points at
//! it, so `NEWEST_BLOCK`-rooted sync, the commit walk, and children-list traversal
//! can never reach it either. Its lingering keys are harmless dead weight — they
//! are never read by the running protocol and cannot re-poison a cleaned peer.
//! We therefore intentionally leave such orphans in place (deleting them would
//! require the very prefix scan the pluggable interface forbids).

use std::collections::HashSet;

use super::accessors::internal::{BlockTreeError, BlockTreeSingleton, BlockTreeWriteBatch};
use super::pluggables::KVStore;
use crate::hotstuff::types::PhaseCertificate;
use crate::types::data_types::{BlockHeight, CryptoHash, DataLen};
use crate::types::signed_messages::Certificate;

/// Genesis parent sentinel — the `block` field of the Genesis PC.
const GENESIS_BLOCK: CryptoHash = CryptoHash::new([0u8; 32]);

/// Read-only classification of a wedged block tree.
pub struct WedgeReport {
    /// Height of the committed frontier (the highest committed block).
    pub committed_frontier_height: BlockHeight,
    /// Hash of the committed frontier.
    pub committed_frontier_hash: CryptoHash,
    /// Number of committed blocks retained (present in `BLOCK_AT_HEIGHT`).
    pub committed_retained: u64,
    /// Every uncommitted block hash discovered by the walk.
    pub uncommitted: Vec<CryptoHash>,
    /// Missing parent hashes referenced by discovered justifies (the holes):
    /// a `justify.block` that is neither committed nor present in `BLOCKS`.
    pub holes: Vec<CryptoHash>,
    /// Justifies of discovered uncommitted blocks whose `.block` equals the
    /// committed frontier hash and whose phase is `Generic`/`Decide` — the
    /// harvestable recovery-PC candidates.
    pub pc_candidates: Vec<PhaseCertificate>,
}

/// Summary of a completed surgery.
pub struct SurgeryReport {
    /// Number of uncommitted blocks deleted.
    pub pruned: u64,
    /// Number of hole children-lists cleared.
    pub holes_cleared: u64,
    /// View of the installed recovery PC.
    pub recovery_pc_view: crate::types::data_types::ViewNumber,
    /// Height of the committed frontier the surgery reset to.
    pub frontier_height: BlockHeight,
    /// Hash of the committed frontier the surgery reset to.
    pub frontier_hash: CryptoHash,
}

/// Errors that can occur during inspection or surgery.
#[derive(Debug)]
pub enum RecoveryError {
    /// A read/write against the underlying block tree failed.
    BlockTree(BlockTreeError),
    /// No committed block exists (empty / uninitialized tree).
    NoCommittedBlock,
    /// The supplied recovery PC does not certify the committed frontier.
    PcWrongBlock,
    /// The supplied recovery PC has a phase that cannot serve as a block justify
    /// (must be `Generic` or `Decide`).
    PcWrongPhase,
    /// The supplied recovery PC does not carry a valid signature quorum against
    /// the committed validator set.
    PcBadSignatures,
    /// Post-apply verification failed with the given reason.
    VerifyFailed(String),
}

impl From<BlockTreeError> for RecoveryError {
    fn from(value: BlockTreeError) -> Self {
        RecoveryError::BlockTree(value)
    }
}

/// Read-only: classify a (possibly wedged) block tree.
pub fn inspect<K: KVStore>(kv: K) -> Result<WedgeReport, RecoveryError> {
    // `newest_block` is a `KVGet` method not surfaced on `BlockTreeSingleton`,
    // so read it off the raw store before wrapping it.
    let newest = kv
        .newest_block()
        .map_err(|e| RecoveryError::BlockTree(BlockTreeError::from(e)))?;
    let bt = BlockTreeSingleton::new(kv);
    inspect_inner(&bt, newest)
}

/// Surgery: prune the uncommitted remainder and reset the racing singletons,
/// installing `recovery_pc` as the highest/locked PC. Single atomic write batch,
/// preceded by full validation; idempotent.
pub fn apply<K: KVStore>(
    kv: K,
    recovery_pc: &PhaseCertificate,
) -> Result<SurgeryReport, RecoveryError> {
    // A second handle onto the SAME store (KVStore: Clone; both RocksKVStore and
    // the test MemKV share their backing store across clones). Used for the
    // post-apply verification pass on a FRESH `BlockTreeSingleton`, side-stepping
    // the write-through singleton caches (leader reputation / speculative
    // commits) that would be stale on `bt` after the surgery write.
    let verify_handle = kv.clone();

    let newest = kv
        .newest_block()
        .map_err(|e| RecoveryError::BlockTree(BlockTreeError::from(e)))?;
    let mut bt = BlockTreeSingleton::new(kv);

    let report = inspect_inner(&bt, newest)?;
    let frontier_hash = report.committed_frontier_hash;

    // ---- Validate the recovery PC BEFORE writing anything. ----
    if recovery_pc.block != frontier_hash {
        return Err(RecoveryError::PcWrongBlock);
    }
    if !recovery_pc.is_block_justify() {
        return Err(RecoveryError::PcWrongPhase);
    }
    if !recovery_pc.is_correct(&bt)? {
        return Err(RecoveryError::PcBadSignatures);
    }

    // ---- One atomic write batch. ----
    let mut wb = BlockTreeWriteBatch::new_unsafe();
    for b in &report.uncommitted {
        let data_len = bt.block_data_len(b)?.unwrap_or(DataLen::new(0));
        wb.delete_block(b, data_len);
        wb.delete_children(b);
        wb.delete_pending_app_state_updates(b);
        wb.delete_block_validator_set_updates(b);
    }
    // Clear dangling children lists: the holes (their children were the pruned
    // floating branch roots) and the frontier (its child was a pruned block).
    for h in &report.holes {
        wb.delete_children(h);
    }
    wb.delete_children(&frontier_hash);

    // Install the recovery PC and re-anchor the sync/tip singletons.
    wb.set_highest_pc(recovery_pc)?;
    wb.set_locked_pc(recovery_pc)?;
    wb.set_newest_block(&frontier_hash)?;
    wb.delete_local_tip();
    wb.delete_highest_tc();
    wb.set_speculative_commits(&[])?;

    bt.write(wb);

    // ---- Hard-fail if the surgery did not produce a clean tree. ----
    verify_after(verify_handle)?;

    Ok(SurgeryReport {
        pruned: report.uncommitted.len() as u64,
        holes_cleared: report.holes.len() as u64,
        recovery_pc_view: recovery_pc.view,
        frontier_height: report.committed_frontier_height,
        frontier_hash,
    })
}

/// DEVNET/TEST ONLY: fabricate a QC'd-poison-ancestry hole by deleting one
/// interior uncommitted block from the tree — the storage shape a real body
/// loss leaves behind (children list and descendants stay; the block's own
/// keys vanish). Used by `torus-wedge-inject` to prove the unwedge flow on a
/// live devnet; NEVER run this against a chain you care about.
///
/// Picks (or validates, when `target` is given) a block that is:
/// * uncommitted and present in `BLOCKS`,
/// * NOT justified by the frontier (so the frontier's direct child survives as
///   the harvestable recovery-PC source), and
/// * an interior block (has at least one in-tree child) — guaranteeing a
///   descendant whose commit walk crosses the hole.
///
/// Returns the deleted block's hash so the same hole can be injected on every
/// other node (pass it as `target`); a consistent fleet-wide hole is required,
/// otherwise a node that kept the block heals its peers via block-sync.
pub fn inject_hole_for_testing<K: KVStore>(
    kv: K,
    target: Option<CryptoHash>,
) -> Result<CryptoHash, RecoveryError> {
    let newest = kv
        .newest_block()
        .map_err(|e| RecoveryError::BlockTree(BlockTreeError::from(e)))?;
    let mut bt = BlockTreeSingleton::new(kv);
    let report = inspect_inner(&bt, newest)?;

    let is_injectable = |bt: &BlockTreeSingleton<K>, b: &CryptoHash| -> Result<bool, RecoveryError> {
        let justify = bt.block_justify(b)?;
        let interior = bt.children(b).unwrap_or_default().iter().next().is_some();
        Ok(justify.block != report.committed_frontier_hash && interior)
    };

    let victim = match target {
        Some(t) => {
            if !report.uncommitted.contains(&t) {
                return Err(RecoveryError::VerifyFailed(format!(
                    "target {:x?} is not an uncommitted in-tree block",
                    &t.bytes()[..4]
                )));
            }
            t
        }
        None => {
            // Lowest interior candidate — maximizes QC'd depth above the hole.
            let mut best: Option<(u64, CryptoHash)> = None;
            for b in &report.uncommitted {
                if is_injectable(&bt, b)? {
                    let h = bt
                        .block_height(b)?
                        .map(|h| h.int())
                        .unwrap_or(u64::MAX);
                    if best.map(|(bh, _)| h < bh).unwrap_or(true) {
                        best = Some((h, *b));
                    }
                }
            }
            best.map(|(_, b)| b).ok_or_else(|| {
                RecoveryError::VerifyFailed(
                    "no injectable interior uncommitted block (tail too short — \
                     stop the devnet mid-flight and retry)"
                        .into(),
                )
            })?
        }
    };

    let data_len = bt.block_data_len(&victim)?.unwrap_or(DataLen::new(0));
    let mut wb = BlockTreeWriteBatch::new_unsafe();
    // Keep the children list — a never-synced block leaves one behind (child
    // inserts write it under the parent hash); only the block itself is gone.
    wb.delete_block(&victim, data_len);
    wb.delete_pending_app_state_updates(&victim);
    wb.delete_block_validator_set_updates(&victim);
    bt.write(wb);

    Ok(victim)
}

/// Post-apply verification: re-inspect and assert the tree is clean.
pub fn verify_after<K: KVStore>(kv: K) -> Result<(), RecoveryError> {
    let newest = kv
        .newest_block()
        .map_err(|e| RecoveryError::BlockTree(BlockTreeError::from(e)))?;
    let bt = BlockTreeSingleton::new(kv);
    let report = inspect_inner(&bt, newest)?;

    if !report.uncommitted.is_empty() {
        return Err(RecoveryError::VerifyFailed(format!(
            "{} uncommitted block(s) still present",
            report.uncommitted.len()
        )));
    }
    if !report.holes.is_empty() {
        return Err(RecoveryError::VerifyFailed(format!(
            "{} hole(s) still present",
            report.holes.len()
        )));
    }

    let frontier = report.committed_frontier_hash;

    let highest_pc = bt.highest_pc()?;
    if highest_pc.block != frontier {
        return Err(RecoveryError::VerifyFailed(
            "highest_pc does not certify the committed frontier".into(),
        ));
    }
    if !bt.contains(&frontier) {
        return Err(RecoveryError::VerifyFailed(
            "committed frontier block is missing from BLOCKS".into(),
        ));
    }

    let locked_pc = bt.locked_pc()?;
    if locked_pc.block != frontier {
        return Err(RecoveryError::VerifyFailed(
            "locked_pc does not certify the committed frontier".into(),
        ));
    }

    // Boot-critical singletons must still deserialize / be present.
    bt.committed_validator_set()?;
    if bt.highest_committed_block()?.is_none() {
        return Err(RecoveryError::VerifyFailed(
            "highest committed block is absent".into(),
        ));
    }

    Ok(())
}

/// Core walk-based classification, shared by [`inspect`] and [`verify_after`].
/// `newest` is the `NEWEST_BLOCK` singleton (read via `KVGet` by the caller,
/// since `BlockTreeSingleton` does not surface a getter for it).
fn inspect_inner<K: KVStore>(
    bt: &BlockTreeSingleton<K>,
    newest: Option<CryptoHash>,
) -> Result<WedgeReport, RecoveryError> {
    // ---- 1. Committed set C and frontier. ----
    let frontier_hash = bt
        .highest_committed_block()?
        .ok_or(RecoveryError::NoCommittedBlock)?;
    let frontier_height = bt
        .highest_committed_block_height()?
        .ok_or(RecoveryError::NoCommittedBlock)?;

    let pruned_from = bt.block_tree_pruned_height()?.map(|h| h.int()).unwrap_or(0);

    let mut committed: HashSet<CryptoHash> = HashSet::new();
    for h in pruned_from..=frontier_height.int() {
        if let Some(hash) = bt.block_at_height(BlockHeight::new(h))? {
            committed.insert(hash);
        }
    }
    if committed.is_empty() {
        return Err(RecoveryError::NoCommittedBlock);
    }
    let committed_retained = committed.len() as u64;

    // ---- 2. Fixpoint discovery of the uncommitted set. ----
    let mut processed: HashSet<CryptoHash> = HashSet::new();
    let mut uncommitted: Vec<CryptoHash> = Vec::new();
    let mut worklist: Vec<CryptoHash> = Vec::new();

    // (a) children of every committed block.
    for c in &committed {
        for child in bt.children(c).unwrap_or_default().iter() {
            worklist.push(*child);
        }
    }
    // (b) the newest block (its backward walk is realized via parent enqueue).
    if let Some(n) = newest {
        worklist.push(n);
    }
    // (c) the speculative-commits list.
    for s in bt.speculative_commits()? {
        worklist.push(s);
    }

    while let Some(x) = worklist.pop() {
        if !processed.insert(x) {
            continue; // already examined
        }
        if committed.contains(&x) || x == GENESIS_BLOCK {
            continue; // committed boundary / genesis sentinel
        }
        if !bt.contains(&x) {
            // A hole. Enqueue its children so any branch parented on the hole
            // (source (d)) is discovered; the hole itself is classified below.
            for child in bt.children(&x).unwrap_or_default().iter() {
                worklist.push(*child);
            }
            continue;
        }
        // Present and not committed => uncommitted.
        uncommitted.push(x);
        let justify = bt.block_justify(&x)?;
        if !justify.is_genesis_pc() {
            worklist.push(justify.block); // parent (may be a hole)
        }
        for child in bt.children(&x).unwrap_or_default().iter() {
            worklist.push(*child);
        }
    }

    // ---- 3. Holes and recovery-PC candidates. ----
    let mut holes: Vec<CryptoHash> = Vec::new();
    let mut pc_candidates: Vec<PhaseCertificate> = Vec::new();
    for b in &uncommitted {
        let justify = bt.block_justify(b)?;
        let parent = justify.block;
        if !justify.is_genesis_pc()
            && !committed.contains(&parent)
            && !bt.contains(&parent)
            && !holes.contains(&parent)
        {
            holes.push(parent);
        }
        if justify.block == frontier_hash
            && justify.is_block_justify()
            && !pc_candidates.contains(&justify)
        {
            pc_candidates.push(justify);
        }
    }

    Ok(WedgeReport {
        committed_frontier_height: frontier_height,
        committed_frontier_hash: frontier_hash,
        committed_retained,
        uncommitted,
        holes,
        pc_candidates,
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use borsh::BorshSerialize;

    use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
    use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
    use crate::hotstuff::types::{Phase, PhaseCertificate};
    use crate::pacemaker::types::{TimeoutCertificate, TipInfo};
    use crate::types::block::Block;
    use crate::types::crypto_primitives::Keypair;
    use crate::types::crypto_primitives::{SigningKey, VerifyingKey};
    use crate::types::data_types::{
        BlockHeight, ChainID, CryptoHash, Data, DataLen, Power, SignatureSet, ViewNumber,
    };
    use crate::types::update_sets::AppStateUpdates;
    use crate::types::validator_set::{ValidatorSet, ValidatorSetState, ValidatorSetUpdatesStatus};

    const CHAIN_ID: ChainID = ChainID::new(0);

    // -----------------------------------------------------------------------
    // Shared-store in-memory KVStore. Unlike the plain-HashMap harness in
    // `pc_discard_regression_test.rs`, this one backs its map with
    // `Arc<Mutex<..>>` so a `clone()` shares the SAME store — the fixture
    // builder (a `BlockTreeSingleton`) and the by-value `recovery::*` calls all
    // observe one logical database, exactly as `RocksKVStore` (Arc<DB>) does.
    // -----------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct MemKV {
        map: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>>,
    }

    struct MemWb {
        sets: Vec<(Vec<u8>, Vec<u8>)>,
        deletes: Vec<Vec<u8>>,
    }

    #[derive(Clone)]
    struct MemSnap(HashMap<Vec<u8>, Vec<u8>>);

    impl WriteBatch for MemWb {
        fn new() -> Self {
            Self {
                sets: Vec::new(),
                deletes: Vec::new(),
            }
        }
        fn set(&mut self, key: &[u8], value: &[u8]) {
            self.sets.push((key.to_vec(), value.to_vec()));
        }
        fn delete(&mut self, key: &[u8]) {
            self.deletes.push(key.to_vec());
        }
    }

    impl KVGet for MemKV {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.map.lock().unwrap().get(key).cloned()
        }
    }

    impl KVGet for MemSnap {
        fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
            self.0.get(key).cloned()
        }
    }

    impl KVStore for MemKV {
        type WriteBatch = MemWb;
        type Snapshot<'a> = MemSnap;
        fn write(&mut self, wb: MemWb) {
            let mut map = self.map.lock().unwrap();
            for (k, v) in wb.sets {
                map.insert(k, v);
            }
            for k in wb.deletes {
                map.remove(&k);
            }
        }
        fn clear(&mut self) {
            self.map.lock().unwrap().clear();
        }
        fn snapshot(&self) -> MemSnap {
            MemSnap(self.map.lock().unwrap().clone())
        }
    }

    // -----------------------------------------------------------------------
    // Fixture helpers.
    // -----------------------------------------------------------------------

    fn signing_keys(seeds: &[u8]) -> Vec<SigningKey> {
        seeds
            .iter()
            .map(|s| SigningKey::from_bytes(&[*s; 32]))
            .collect()
    }

    fn validator_set(keys: &[SigningKey]) -> ValidatorSet {
        let mut vs = ValidatorSet::new();
        for k in keys {
            vs.put(&k.verifying_key(), Power::new(1));
        }
        vs
    }

    /// Build a correctly-signed `Generic` PhaseCertificate for `block`/`view`.
    fn generic_pc(
        view: u64,
        block: CryptoHash,
        signers: &[SigningKey],
        set: &ValidatorSet,
    ) -> PhaseCertificate {
        let v = ViewNumber::new(view);
        let message = (CHAIN_ID, v, block, Phase::Generic).try_to_vec().unwrap();
        let mut signatures = SignatureSet::new(set.len());
        for sk in signers {
            let vk: VerifyingKey = sk.verifying_key();
            let pos = set.position(&vk).expect("signer in set");
            signatures.set(pos, Some(Keypair::new(sk.clone()).sign(&message)));
        }
        PhaseCertificate {
            chain_id: CHAIN_ID,
            view: v,
            block,
            phase: Phase::Generic,
            signatures,
        }
    }

    /// A height/justify-parameterized empty block.
    fn blk(height: u64, justify: PhaseCertificate) -> Block {
        Block::new(
            BlockHeight::new(height),
            justify,
            CryptoHash::new([0u8; 32]),
            Data::new(vec![]),
        )
    }

    struct Wedged {
        kv: MemKV,
        keys: Vec<SigningKey>,
        vs: ValidatorSet,
        h1: CryptoHash,
        h2: CryptoHash,
        h3: CryptoHash,
        h4p: CryptoHash,
        h5: CryptoHash,
        h6: CryptoHash,
        h7: CryptoHash,
        missing: CryptoHash,
        recovery_pc: PhaseCertificate,
    }

    /// A block tree wedged exactly like the S458 testnet incident:
    /// committed chain h1..h3, one harvestable uncommitted child h4' of h3, and a
    /// floating branch h5..h7 whose root h5 justifies a block (`missing`) that was
    /// never inserted (the hole), plus racing singletons above the frontier.
    fn wedged_tree() -> Wedged {
        let keys = signing_keys(&[1, 2, 3, 4]);
        let vs = validator_set(&keys);
        let kv = MemKV::default();
        let mut bt = BlockTreeSingleton::new(kv.clone());
        let vss = ValidatorSetState::new(vs.clone(), vs.clone(), None, true);
        bt.initialize(&AppStateUpdates::new(), &vss)
            .expect("initialize");

        // Committed chain h1 (genesis-justified) -> h2 -> h3.
        let h1 = blk(1, PhaseCertificate::genesis_pc());
        let h2 = blk(2, generic_pc(1, h1.hash, &keys, &vs));
        let h3 = blk(3, generic_pc(2, h2.hash, &keys, &vs));
        bt.insert(&h1, None, None).expect("insert h1");
        bt.insert(&h2, None, None).expect("insert h2");
        bt.insert(&h3, None, None).expect("insert h3");

        // The harvestable recovery PC: justifies the committed frontier h3.
        let recovery_pc = generic_pc(3, h3.hash, &keys, &vs);
        let h4p = blk(4, recovery_pc.clone());
        bt.insert(&h4p, None, None).expect("insert h4p");
        bt.add_speculative_commit(h4p.hash).expect("spec commit");

        // Floating branch: h5 justifies `missing`, which is NEVER inserted.
        let missing = blk(5, generic_pc(500, h2.hash, &keys, &vs));
        let h5 = blk(6, generic_pc(600, missing.hash, &keys, &vs));
        let h6 = blk(7, generic_pc(700, h5.hash, &keys, &vs));
        let h7 = blk(8, generic_pc(800, h6.hash, &keys, &vs));
        bt.insert(&h5, None, None).expect("insert h5 (absent parent ok)");
        bt.insert(&h6, None, None).expect("insert h6");
        bt.insert(&h7, None, None).expect("insert h7");

        // Committed-side singletons.
        let mut wb = BlockTreeWriteBatch::new_unsafe();
        wb.set_block_at_height(BlockHeight::new(1), &h1.hash).unwrap();
        wb.set_block_at_height(BlockHeight::new(2), &h2.hash).unwrap();
        wb.set_block_at_height(BlockHeight::new(3), &h3.hash).unwrap();
        wb.set_highest_committed_block(&h3.hash).unwrap();
        bt.write(wb);

        // Racing singletons above the frontier.
        let mut wb2 = BlockTreeWriteBatch::new_unsafe();
        wb2.set_highest_pc(&generic_pc(900, h6.hash, &keys, &vs)).unwrap();
        wb2.set_locked_pc(&generic_pc(899, h5.hash, &keys, &vs)).unwrap();
        bt.write(wb2);
        bt.set_highest_view_entered(ViewNumber::new(900)).unwrap();
        bt.set_highest_view_phase_voted(ViewNumber::new(900)).unwrap();
        bt.set_last_voted_proposal(ViewNumber::new(900), h7.hash).unwrap();
        let tip = TipInfo {
            block_hash: h7.hash,
            block_height: h7.height,
            block_justify: h7.justify.clone(),
            block_data_hash: h7.data_hash,
            view: ViewNumber::new(900),
        };
        bt.set_local_tip(&tip).unwrap();
        let tc = TimeoutCertificate {
            chain_id: CHAIN_ID,
            view: ViewNumber::new(900),
            signatures: SignatureSet::new(vs.len()),
            high_tip: Some(tip.clone()),
            high_qc: None,
            high_tip_is_winner: false,
            voter_metadata: vec![],
        };
        bt.set_highest_tc(&tc).unwrap();

        Wedged {
            kv,
            keys,
            vs,
            h1: h1.hash,
            h2: h2.hash,
            h3: h3.hash,
            h4p: h4p.hash,
            h5: h5.hash,
            h6: h6.hash,
            h7: h7.hash,
            missing: missing.hash,
            recovery_pc,
        }
    }

    fn sorted(mut v: Vec<CryptoHash>) -> Vec<CryptoHash> {
        v.sort_by_key(|h| h.bytes());
        v
    }

    // -----------------------------------------------------------------------

    #[test]
    fn inspect_classifies_wedged_tree() {
        let w = wedged_tree();
        let report = inspect(w.kv.clone()).expect("inspect ok");

        assert_eq!(report.committed_frontier_height, BlockHeight::new(3));
        assert_eq!(report.committed_frontier_hash, w.h3);

        assert_eq!(
            sorted(report.uncommitted.clone()),
            sorted(vec![w.h4p, w.h5, w.h6, w.h7]),
            "uncommitted set"
        );
        assert_eq!(report.holes, vec![w.missing], "holes");
        // PhaseCertificate has no Debug impl, so compare with `==`.
        assert!(
            report.pc_candidates == vec![w.recovery_pc.clone()],
            "pc candidates"
        );
    }

    #[test]
    fn prune_removes_only_uncommitted() {
        let w = wedged_tree();
        apply(w.kv.clone(), &w.recovery_pc).expect("apply ok");

        let bt = BlockTreeSingleton::new(w.kv.clone());

        for h in [w.h4p, w.h5, w.h6, w.h7] {
            assert!(bt.block(&h).unwrap().is_none(), "block gone");
            assert!(bt.children(&h).is_err(), "children gone");
            assert!(
                bt.pending_app_state_updates(&h).unwrap().is_none(),
                "pending app state gone"
            );
            assert!(
                matches!(
                    bt.validator_set_updates_status(&h).unwrap(),
                    ValidatorSetUpdatesStatus::None
                ),
                "vs updates gone"
            );
        }

        // Committed side fully intact.
        assert!(bt.block(&w.h1).unwrap().is_some());
        assert!(bt.block(&w.h2).unwrap().is_some());
        assert!(bt.block(&w.h3).unwrap().is_some());
        assert_eq!(bt.block_justify(&w.h3).unwrap().block, w.h2);

        // Frontier's children list and the hole's children list are cleared.
        assert!(bt.children(&w.h3).is_err(), "frontier children cleared");
        assert!(bt.children(&w.missing).is_err(), "hole children cleared");
    }

    #[test]
    fn singletons_reset_and_safety_vars_kept() {
        let w = wedged_tree();
        apply(w.kv.clone(), &w.recovery_pc).expect("apply ok");

        let bt = BlockTreeSingleton::new(w.kv.clone());

        assert!(bt.highest_pc().unwrap() == w.recovery_pc, "highest pc");
        assert!(bt.locked_pc().unwrap() == w.recovery_pc, "locked pc");
        assert_eq!(w.kv.newest_block().unwrap(), Some(w.h3), "newest block");

        assert!(bt.local_tip().unwrap().is_none(), "local tip cleared");
        assert!(bt.highest_tc().unwrap().is_none(), "highest tc cleared");
        assert!(
            bt.speculative_commits().unwrap().is_empty(),
            "speculative commits cleared"
        );

        // Safety vars kept.
        assert_eq!(bt.highest_view_entered().unwrap(), ViewNumber::new(900));
        assert_eq!(bt.highest_view_voted().unwrap(), Some(ViewNumber::new(900)));
        assert_eq!(
            bt.last_voted_proposal().unwrap(),
            Some((ViewNumber::new(900), w.h7))
        );

        // Orthogonal vars untouched (never set on this fixture).
        assert!(bt.block_tree_pruned_height().unwrap().is_none());
        assert!(bt.app_fed_block_height().unwrap().is_none());
    }

    #[test]
    fn apply_rejects_bad_pc() {
        let w = wedged_tree();

        // PC certifies the wrong block (h2, not the frontier h3).
        let pc_wrong_block = generic_pc(3, w.h2, &w.keys, &w.vs);
        assert!(matches!(
            apply(w.kv.clone(), &pc_wrong_block),
            Err(RecoveryError::PcWrongBlock)
        ));

        // Well-formed PC(h3) but signed by a DISJOINT keypair set.
        let disjoint = signing_keys(&[101, 102, 103, 104]);
        let disjoint_set = validator_set(&disjoint);
        let pc_bad_sig = generic_pc(3, w.h3, &disjoint, &disjoint_set);
        assert!(matches!(
            apply(w.kv.clone(), &pc_bad_sig),
            Err(RecoveryError::PcBadSignatures)
        ));

        // Precommit PC cannot serve as a block justify (Block::new would panic,
        // so construct the literal). Must be rejected on phase.
        let pc_wrong_phase = PhaseCertificate {
            chain_id: CHAIN_ID,
            view: ViewNumber::new(3),
            block: w.h3,
            phase: Phase::Precommit,
            signatures: SignatureSet::new(w.vs.len()),
        };
        assert!(matches!(
            apply(w.kv.clone(), &pc_wrong_phase),
            Err(RecoveryError::PcWrongPhase)
        ));
    }

    #[test]
    fn verify_after_ok() {
        let w = wedged_tree();
        apply(w.kv.clone(), &w.recovery_pc).expect("apply ok");
        verify_after(w.kv.clone()).expect("verify ok after apply");
    }

    #[test]
    fn verify_detects_corruption() {
        let w = wedged_tree();
        apply(w.kv.clone(), &w.recovery_pc).expect("apply ok");

        // Deliberately delete the frontier's BLOCKS entry.
        let mut bt = BlockTreeSingleton::new(w.kv.clone());
        let mut wb = BlockTreeWriteBatch::new_unsafe();
        wb.delete_block(&w.h3, DataLen::new(0));
        bt.write(wb);

        assert!(verify_after(w.kv.clone()).is_err());
    }

    #[test]
    fn apply_is_idempotent() {
        let w = wedged_tree();

        let r1 = apply(w.kv.clone(), &w.recovery_pc).expect("first apply ok");
        assert!(r1.pruned > 0, "first apply prunes");

        let r2 = apply(w.kv.clone(), &w.recovery_pc).expect("second apply ok");
        assert_eq!(r2.pruned, 0, "second apply prunes nothing");

        verify_after(w.kv.clone()).expect("verify ok");
        let bt = BlockTreeSingleton::new(w.kv.clone());
        assert!(bt.highest_pc().unwrap() == w.recovery_pc, "highest pc");
    }

    /// A HEALTHY tree (committed h1..h3 + connected uncommitted chain h4->h5->h6,
    /// no hole). `inject_hole_for_testing` must pick h5 — the only interior
    /// uncommitted block not justified by the frontier (h4 is excluded exactly
    /// because deleting it would destroy the harvestable recovery PC) — and the
    /// resulting tree must round-trip the full unwedge flow.
    #[test]
    fn inject_then_unwedge_roundtrip() {
        let keys = signing_keys(&[1, 2, 3, 4]);
        let vs = validator_set(&keys);
        let kv = MemKV::default();
        let mut bt = BlockTreeSingleton::new(kv.clone());
        let vss = ValidatorSetState::new(vs.clone(), vs.clone(), None, true);
        bt.initialize(&AppStateUpdates::new(), &vss).unwrap();

        let h1 = blk(1, PhaseCertificate::genesis_pc());
        let h2 = blk(2, generic_pc(1, h1.hash, &keys, &vs));
        let h3 = blk(3, generic_pc(2, h2.hash, &keys, &vs));
        let recovery_pc = generic_pc(3, h3.hash, &keys, &vs);
        let h4 = blk(4, recovery_pc.clone());
        let h5 = blk(5, generic_pc(4, h4.hash, &keys, &vs));
        let h6 = blk(6, generic_pc(5, h5.hash, &keys, &vs));
        for b in [&h1, &h2, &h3, &h4, &h5, &h6] {
            bt.insert(b, None, None).unwrap();
        }
        let mut wb = BlockTreeWriteBatch::new_unsafe();
        wb.set_block_at_height(BlockHeight::new(1), &h1.hash).unwrap();
        wb.set_block_at_height(BlockHeight::new(2), &h2.hash).unwrap();
        wb.set_block_at_height(BlockHeight::new(3), &h3.hash).unwrap();
        wb.set_highest_committed_block(&h3.hash).unwrap();
        wb.set_highest_pc(&generic_pc(5, h5.hash, &keys, &vs)).unwrap();
        bt.write(wb);

        // Healthy: no holes yet.
        let before = inspect(kv.clone()).expect("inspect healthy");
        assert!(before.holes.is_empty(), "healthy tree has no holes");

        // Inject: must pick h5 (h4 justifies the frontier; h6 is the tip).
        let victim = inject_hole_for_testing(kv.clone(), None).expect("inject");
        assert_eq!(victim, h5.hash, "picks the interior non-frontier-child");

        // Targeted mode agrees / validates.
        assert!(
            matches!(
                inject_hole_for_testing(kv.clone(), Some(h5.hash)),
                Err(RecoveryError::VerifyFailed(_))
            ),
            "re-targeting the now-missing block is rejected"
        );

        // The wedge signature is real: h6's parent is gone.
        let wedged = inspect(kv.clone()).expect("inspect wedged");
        assert_eq!(wedged.holes, vec![h5.hash], "hole is the victim");
        assert!(
            sorted(wedged.uncommitted.clone()) == sorted(vec![h4.hash, h6.hash]),
            "h4 and h6 remain uncommitted"
        );
        assert_eq!(wedged.pc_candidates.len(), 1, "recovery PC survives via h4");

        // Full unwedge round-trip on the injected wedge.
        let surgery = apply(kv.clone(), &recovery_pc).expect("apply on injected wedge");
        assert_eq!(surgery.pruned, 2, "h4 + h6 pruned");
        verify_after(kv.clone()).expect("clean after surgery");
    }
}
