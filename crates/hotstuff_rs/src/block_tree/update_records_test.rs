//! s138 option 1 (docs/plans/vset-membership-sync-remainder.md): a replica that commits a
//! validator-set update U on U's Commit PC keeps that PC (with U's updates) under
//! `VALIDATOR_SET_UPDATE_RECORDS ++ height(U)`, and its block-sync server ends a batch at U with the
//! PC as the terminal PC.
//!
//! RED on the skeleton: `update` writes no record and `sync_response` never cuts a batch. The
//! controls (an ordinary commit touches no record key; an update committed without its Commit PC
//! gets no record; a batch without a record is served unchanged) pass on both.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use borsh::BorshSerialize;

use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::block_tree::update_records::validator_set_update_record_key;
use crate::block_tree::variables::{HIGHEST_COMMITTED_BLOCK, VALIDATOR_SET_UPDATE_RECORDS};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::types::block::Block;
use crate::types::crypto_primitives::SigningKey;
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, SignatureSet, ViewNumber};
use crate::types::update_sets::{AppStateUpdates, ValidatorSetUpdates, ValidatorSetUpdatesBytes};
use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

const CHAIN_ID: ChainID = ChainID::new(0);

// ---------------------------------------------------------------------------
// A KV store that logs every write batch and every read key.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Log {
    map: HashMap<Vec<u8>, Vec<u8>>,
    /// The keys each write batch set, one entry per batch, in write order.
    batches: Vec<Vec<Vec<u8>>>,
    /// Every key read through `get`.
    reads: Vec<Vec<u8>>,
}

#[derive(Clone, Default)]
struct RecKV(Arc<Mutex<Log>>);

struct RecWb {
    sets: Vec<(Vec<u8>, Vec<u8>)>,
    deletes: Vec<Vec<u8>>,
}

struct RecSnap(HashMap<Vec<u8>, Vec<u8>>);

impl WriteBatch for RecWb {
    fn new() -> Self {
        Self { sets: Vec::new(), deletes: Vec::new() }
    }
    fn set(&mut self, key: &[u8], value: &[u8]) {
        self.sets.push((key.to_vec(), value.to_vec()));
    }
    fn delete(&mut self, key: &[u8]) {
        self.deletes.push(key.to_vec());
    }
}

impl KVGet for RecKV {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut log = self.0.lock().unwrap();
        log.reads.push(key.to_vec());
        log.map.get(key).cloned()
    }
}

impl KVGet for RecSnap {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
}

impl KVStore for RecKV {
    type WriteBatch = RecWb;
    type Snapshot<'a> = RecSnap;
    fn write(&mut self, wb: RecWb) {
        let mut log = self.0.lock().unwrap();
        log.batches.push(wb.sets.iter().map(|(k, _)| k.clone()).collect());
        for (k, v) in wb.sets {
            log.map.insert(k, v);
        }
        for k in wb.deletes {
            log.map.remove(&k);
        }
    }
    fn clear(&mut self) {
        self.0.lock().unwrap().map.clear();
    }
    fn snapshot(&self) -> RecSnap {
        RecSnap(self.0.lock().unwrap().map.clone())
    }
}

impl RecKV {
    fn reset_log(&self) {
        let mut log = self.0.lock().unwrap();
        log.batches.clear();
        log.reads.clear();
    }

    /// Every key read or written since the last `reset_log` that is a record key.
    fn record_keys_touched(&self) -> Vec<Vec<u8>> {
        let log = self.0.lock().unwrap();
        log.reads
            .iter()
            .chain(log.batches.iter().flatten())
            .filter(|k| k.first() == Some(&VALIDATOR_SET_UPDATE_RECORDS[0]))
            .cloned()
            .collect()
    }

    fn batches(&self) -> Vec<Vec<Vec<u8>>> {
        self.0.lock().unwrap().batches.clone()
    }
}

// ---------------------------------------------------------------------------
// Fixtures: four validators of power 1; U adds a fifth.
// ---------------------------------------------------------------------------

fn keys() -> Vec<SigningKey> {
    (1..=5u8).map(|s| SigningKey::from_bytes(&[s; 32])).collect()
}

fn old_set() -> ValidatorSet {
    let mut vs = ValidatorSet::new();
    for k in &keys()[..4] {
        vs.put(&k.verifying_key(), Power::new(1));
    }
    vs
}

fn updates() -> ValidatorSetUpdates {
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keys()[4].verifying_key(), Power::new(1));
    updates
}

fn pc(view: u64, block: &CryptoHash, phase: Phase) -> PhaseCertificate {
    PhaseCertificate {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block: *block,
        phase,
        signatures: SignatureSet::new(4),
    }
}

fn block(height: u64, justify: PhaseCertificate, tag: u8) -> Block {
    Block::new(BlockHeight::new(height), justify, CryptoHash::new([tag; 32]), Data::new(vec![]))
}

fn new_tree() -> (BlockTreeSingleton<RecKV>, RecKV) {
    let kv = RecKV::default();
    let mut tree = BlockTreeSingleton::new(kv.clone());
    let old = old_set();
    tree.initialize(&AppStateUpdates::new(), &ValidatorSetState::new(old.clone(), old, None, true))
        .unwrap();
    (tree, kv)
}

/// B0 (height 0) <- B1 (height 1) <- U (height 2, updates), Generic PCs at views 1 and 2, U
/// justified by a Generic PC on B1 at view 3. Nothing committed. Returns `[B0, B1, U]`.
fn chain_to_update(tree: &mut BlockTreeSingleton<RecKV>) -> [Block; 3] {
    let b0 = block(0, PhaseCertificate::genesis_pc(), 10);
    let b1 = block(1, pc(1, &b0.hash, Phase::Generic), 11);
    let u = block(2, pc(2, &b1.hash, Phase::Generic), 12);
    tree.insert(&b0, None, None).unwrap();
    tree.insert(&b1, None, None).unwrap();
    tree.insert(&u, None, Some(&updates())).unwrap();
    [b0, b1, u]
}

/// U's Commit PC (view 6, after its Prepare, Precommit PCs at views 4, 5).
fn commit_pc(u: &Block) -> PhaseCertificate {
    pc(6, &u.hash, Phase::Commit)
}

/// Extend `parent` (whose PC is at `parent_view`) by `n` ordinary blocks, each justified by a PC
/// one view after its parent's, and run `update` on each block's justify and then on a PC on the
/// last block, so all but the last block commit (2-chain). Returns the new blocks.
fn extend_and_commit(
    tree: &mut BlockTreeSingleton<RecKV>,
    parent: &Block,
    parent_pc: PhaseCertificate,
    n: u64,
    tag: u8,
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut justify = parent_pc;
    let mut height = parent.height.int();
    for i in 0..n {
        height += 1;
        let b = block(height, justify.clone(), tag + i as u8);
        tree.insert(&b, None, None).unwrap();
        tree.update(&justify, &None).unwrap();
        justify = pc(justify.view.int() + 1, &b.hash, Phase::Generic);
        blocks.push(b);
    }
    tree.update(&justify, &None).unwrap();
    blocks
}

fn updates_bytes(updates: &ValidatorSetUpdates) -> Vec<u8> {
    ValidatorSetUpdatesBytes::from(updates).try_to_vec().unwrap()
}

// ---------------------------------------------------------------------------
// Store.
// ---------------------------------------------------------------------------

/// A Commit PC that commits U stores the record at U's height, in the same write batch that marks
/// U committed (a crash cannot leave U committed without its record).
#[test]
fn a_commit_pc_on_an_update_is_recorded_in_the_commit_batch() {
    let (mut tree, kv) = new_tree();
    let [_, _, u] = chain_to_update(&mut tree);
    kv.reset_log();

    let result = tree.update(&commit_pc(&u), &None).unwrap();
    assert!(result.validator_set_updates.is_some(), "fixture: U committed");

    let record = tree
        .validator_set_update_record(u.height)
        .unwrap()
        .expect("the record of U at its height");
    assert!(record.commit_pc == commit_pc(&u), "the record holds U's Commit PC");
    assert_eq!(record.updates.try_to_vec().unwrap(), updates_bytes(&updates()), "and U's updates");

    let key = validator_set_update_record_key(u.height);
    let commit_batches: Vec<_> = kv
        .batches()
        .into_iter()
        .filter(|batch| batch.iter().any(|k| k.as_slice() == HIGHEST_COMMITTED_BLOCK))
        .collect();
    assert_eq!(commit_batches.len(), 1, "one batch commits U");
    assert!(commit_batches[0].contains(&key), "the record is in U's commit batch");
}

/// Owner s138: no extra work on ordinary blocks. Committing blocks that update nothing reads and
/// writes no record key.
#[test]
fn an_ordinary_commit_neither_reads_nor_writes_records() {
    let (mut tree, kv) = new_tree();
    let b0 = block(0, PhaseCertificate::genesis_pc(), 10);
    tree.insert(&b0, None, None).unwrap();
    kv.reset_log();

    let blocks = extend_and_commit(&mut tree, &b0, pc(1, &b0.hash, Phase::Generic), 6, 20);
    assert!(
        tree.highest_committed_block_height().unwrap() >= Some(blocks[3].height),
        "fixture: ordinary blocks committed"
    );
    assert!(kv.record_keys_touched().is_empty(), "record keys touched: {:?}", kv.record_keys_touched());
}

/// U committed by its Decide PC (a replica that missed the Commit PC) or as an ancestor of a later
/// commit gets no record: there is no Commit PC to store.
#[test]
fn an_update_committed_without_its_commit_pc_has_no_record() {
    // By its Decide PC.
    let (mut tree, _) = new_tree();
    let [_, _, u] = chain_to_update(&mut tree);
    let result = tree.update(&pc(7, &u.hash, Phase::Decide), &None).unwrap();
    assert!(result.validator_set_updates.is_some(), "fixture: U committed");
    assert!(tree.validator_set_update_record(u.height).unwrap().is_none(), "Decide PC: no record");

    // As the grandparent of a Generic PC (2-chain on U's child).
    let (mut tree, _) = new_tree();
    let [_, _, u] = chain_to_update(&mut tree);
    let child = block(3, pc(7, &u.hash, Phase::Decide), 13);
    tree.insert(&child, None, None).unwrap();
    let result = tree.update(&pc(8, &child.hash, Phase::Generic), &None).unwrap();
    assert!(result.validator_set_updates.is_some(), "fixture: U committed");
    assert!(tree.validator_set_update_record(u.height).unwrap().is_none(), "ancestor: no record");
}

/// The block-tree pruner deletes U's block once it leaves the retention window but keeps U's
/// record: a node syncing from genesis needs every record.
#[test]
fn records_survive_block_tree_pruning() {
    let (mut tree, _) = new_tree();
    let [_, _, u] = chain_to_update(&mut tree);
    tree.update(&commit_pc(&u), &None).unwrap();
    extend_and_commit(&mut tree, &u, pc(7, &u.hash, Phase::Decide), 12, 30);

    let pruned = tree.prune_old_committed_blocks_with(4).unwrap();
    assert!(pruned > 0, "fixture: the pruner ran");
    assert!(tree.block(&u.hash).unwrap().is_none(), "fixture: U's block was pruned");
    let record = tree
        .validator_set_update_record(u.height)
        .unwrap()
        .expect("U's record survives pruning");
    assert!(record.commit_pc == commit_pc(&u));
}

// ---------------------------------------------------------------------------
// Serve.
// ---------------------------------------------------------------------------

/// A committed chain B0, B1, U (recorded), C (U's child, Decide PC), and four ordinary blocks.
fn served_tree(record: bool) -> (BlockTreeSingleton<RecKV>, [Block; 3]) {
    let (mut tree, _) = new_tree();
    let chain = chain_to_update(&mut tree);
    let u = &chain[2];
    if record {
        tree.update(&commit_pc(u), &None).unwrap();
    } else {
        tree.update(&pc(7, &u.hash, Phase::Decide), &None).unwrap();
    }
    extend_and_commit(&mut tree, u, pc(7, &u.hash, Phase::Decide), 5, 40);
    (tree, chain)
}

fn hashes(blocks: &[Block]) -> Vec<CryptoHash> {
    blocks.iter().map(|b| b.hash).collect()
}

/// A batch that reaches a recorded update ends at it, with U's Commit PC as the terminal PC.
#[test]
fn a_sync_batch_ends_at_a_recorded_update_with_its_commit_pc() {
    let (tree, [b0, b1, u]) = served_tree(true);
    for start in 0..=2 {
        let (blocks, terminal) = tree.snapshot().sync_response(BlockHeight::new(start), 64).unwrap();
        let expected: Vec<_> = [&b0, &b1, &u][start as usize..].iter().map(|b| b.hash).collect();
        assert_eq!(hashes(&blocks), expected, "start {start}: the batch ends at U");
        assert!(terminal == commit_pc(&u), "start {start}: the terminal PC is U's Commit PC");
    }
    // A limit that stops before U: unchanged.
    let (blocks, terminal) = tree.snapshot().sync_response(BlockHeight::new(0), 2).unwrap();
    assert_eq!(hashes(&blocks), vec![b0.hash, b1.hash]);
    assert!(terminal == tree.highest_pc().unwrap(), "below U: the highest PC");
}

/// A batch from above the update, or over an update without a record, is served as before.
#[test]
fn a_sync_batch_without_a_record_in_range_is_unchanged() {
    for (record, start) in [(true, 3), (false, 0), (false, 2)] {
        let (tree, _) = served_tree(record);
        let snapshot = tree.snapshot();
        let (blocks, terminal) = snapshot.sync_response(BlockHeight::new(start), 64).unwrap();
        let plain = snapshot.blocks_from_height_to_newest(BlockHeight::new(start), 64).unwrap();
        assert_eq!(hashes(&blocks), hashes(&plain), "record {record}, start {start}: all blocks");
        assert!(terminal == tree.highest_pc().unwrap(), "record {record}, start {start}: highest PC");
        assert!(blocks.len() > 3 - start as usize, "fixture: the batch reaches past U");
    }
}
