//! Leader propose-path write batching — flush-before-send ordering regression
//! tests (perf/matched-200k).
//!
//! SAFETY INVARIANT under test: every block-tree variable that backs vote/lock
//! safety (LOCKED_PC, HIGHEST_PC, and the proposed block itself) must be
//! DURABLY persisted before the corresponding `ProposalHeader` leaves this
//! replica. The batched propose path flushes ONE atomic write batch after
//! `update` and strictly before `broadcast_proposal_as_header` — these tests
//! pin that ordering with a KV store and a network that log into a single
//! shared event log, for BOTH `enter_view` propose call sites (the main
//! `am_proposer` path and the deferred-proposal retry path).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::block_tree::variables;
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::HotStuffMessage;
use crate::hotstuff::roles::is_proposer_with_reputation;
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::networking::messages::{Message, ProgressMessage};
use crate::networking::network::{Network, ValidatorSetUpdateHandle};
use crate::networking::sending::SenderHandle;
use crate::pacemaker::implementation::ViewInfo;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{
    BlockHeight, ChainID, CryptoHash, Data, Power, SignatureSet, ViewNumber,
};
use crate::types::update_sets::{AppStateUpdates, ValidatorSetUpdates};
use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

// ---------------------------------------------------------------------------
// A single shared event log recording both KV flushes and network broadcasts,
// so relative durability/send order is directly assertable.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    /// One `KVStore::write` call; carries the keys set in that atomic batch.
    KvWrite(Vec<Vec<u8>>),
    /// A `ProposalHeader` left the replica via `Network::broadcast`.
    BroadcastProposalHeader,
}

type Log = Arc<Mutex<Vec<Op>>>;

#[derive(Clone)]
struct LogKV {
    map: HashMap<Vec<u8>, Vec<u8>>,
    log: Log,
}

impl LogKV {
    fn new(log: Log) -> Self {
        Self {
            map: HashMap::new(),
            log,
        }
    }
}

struct LogWb {
    sets: Vec<(Vec<u8>, Vec<u8>)>,
    deletes: Vec<Vec<u8>>,
}

#[derive(Clone)]
struct LogSnap(HashMap<Vec<u8>, Vec<u8>>);

impl WriteBatch for LogWb {
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

impl KVGet for LogKV {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.map.get(key).cloned()
    }
}

impl KVGet for LogSnap {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
}

impl KVStore for LogKV {
    type WriteBatch = LogWb;
    type Snapshot<'a> = LogSnap;
    fn write(&mut self, wb: LogWb) {
        self.log
            .lock()
            .unwrap()
            .push(Op::KvWrite(wb.sets.iter().map(|(k, _)| k.clone()).collect()));
        for (k, v) in wb.sets {
            self.map.insert(k, v);
        }
        for k in wb.deletes {
            self.map.remove(&k);
        }
    }
    fn clear(&mut self) {
        self.map.clear();
    }
    fn snapshot<'b>(&'b self) -> LogSnap {
        LogSnap(self.map.clone())
    }
}

#[derive(Clone)]
struct LogNetwork {
    log: Log,
}

impl Network for LogNetwork {
    fn init_validator_set(&mut self, _validator_set: ValidatorSet) {}
    fn update_validator_set(&mut self, _updates: ValidatorSetUpdates) {}
    fn broadcast(&mut self, message: Message) {
        if matches!(
            message,
            Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                HotStuffMessage::ProposalHeader(_)
            ))
        ) {
            self.log.lock().unwrap().push(Op::BroadcastProposalHeader);
        }
    }
    fn send(&mut self, _peer: VerifyingKey, _message: Message) {}
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

/// App producing an empty block — enough to drive the leader propose path.
struct EmptyBlockApp;

impl App<LogKV> for EmptyBlockApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<LogKV>) -> ProduceBlockResponse {
        ProduceBlockResponse {
            data_hash: CryptoHash::new([42u8; 32]),
            data: Data::new(vec![]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<LogKV>) -> ValidateBlockResponse {
        unreachable!("validate_block is not reached on the propose path")
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<LogKV>,
    ) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the propose path")
    }
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

const CHAIN_ID: ChainID = ChainID::new(0);

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

fn generic_pc(view: u64, block: CryptoHash) -> PhaseCertificate {
    PhaseCertificate {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block,
        phase: Phase::Generic,
        signatures: SignatureSet::new(0),
    }
}

fn set_highest_pc(block_tree: &mut BlockTreeSingleton<LogKV>, pc: &PhaseCertificate) {
    let mut wb: BlockTreeWriteBatch<LogWb> = BlockTreeWriteBatch::new();
    wb.set_highest_pc(pc).expect("set_highest_pc");
    block_tree.write(wb);
}

fn proposer_for(
    view: ViewNumber,
    keys: &[SigningKey],
    vss: &ValidatorSetState,
    block_tree: &BlockTreeSingleton<LogKV>,
) -> SigningKey {
    let reputation = block_tree.leader_reputation().ok();
    keys.iter()
        .find(|k| is_proposer_with_reputation(&k.verifying_key(), view, vss, reputation.as_ref()))
        .expect("one of the validators must be the proposer for the view")
        .clone()
}

fn hotstuff_at(view: ViewNumber, local: SigningKey, vss: ValidatorSetState, log: Log) -> HotStuff<LogNetwork> {
    let config = HotStuffConfiguration {
        chain_id: CHAIN_ID,
        keypair: Keypair::new(local),
    };
    let view_info = ViewInfo::new(view, Instant::now() + Duration::from_secs(3600));
    let network = LogNetwork { log };
    HotStuff::new(
        config,
        view_info,
        SenderHandle::new(network.clone()),
        ValidatorSetUpdateHandle::new(network),
        vss,
        None,
    )
}

fn view_info_at(view: ViewNumber) -> ViewInfo {
    ViewInfo::new(view, Instant::now() + Duration::from_secs(3600))
}

/// In `ops`, the FIRST `KvWrite` whose batch contains `key` — the durability
/// point of that variable.
fn first_write_containing(ops: &[Op], key: &[u8]) -> Option<usize> {
    ops.iter().position(
        |op| matches!(op, Op::KvWrite(keys) if keys.iter().any(|k| k == key)),
    )
}

fn broadcast_position(ops: &[Op]) -> Option<usize> {
    ops.iter()
        .position(|op| matches!(op, Op::BroadcastProposalHeader))
}

/// Assert the batched-propose ordering contract on a recorded op sequence:
/// LOCKED_PC (the vote-safety variable) and the proposed block are flushed in
/// the SAME atomic batch, and that batch strictly precedes the
/// `ProposalHeader` broadcast.
fn assert_flush_before_send(ops: &[Op]) {
    let lock_idx = first_write_containing(ops, &variables::LOCKED_PC)
        .expect("a batch containing LOCKED_PC must be flushed on the propose path");
    let bcast_idx = broadcast_position(ops).expect("the proposal header must be broadcast");
    assert!(
        lock_idx < bcast_idx,
        "the safety batch (LOCKED_PC) must be durable BEFORE the header broadcast \
         (lock at op {lock_idx}, broadcast at op {bcast_idx})"
    );
    // The proposed block travels in the same atomic batch as the lock.
    if let Op::KvWrite(keys) = &ops[lock_idx] {
        assert!(
            keys.iter().any(|k| k.starts_with(&variables::BLOCKS)),
            "the proposed block's insert must be in the SAME atomic batch as LOCKED_PC"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// Main `am_proposer` call site: entering a view as leader flushes one batch
/// (insert + LOCKED_PC) before broadcasting the proposal header, and
/// HIGHEST_VIEW_ENTERED is durable before the broadcast too.
#[test]
fn leader_flushes_propose_batch_before_broadcasting_header() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    let mut block_tree = BlockTreeSingleton::new(LogKV::new(log.clone()));
    block_tree
        .initialize(&AppStateUpdates::new(), &vss)
        .unwrap();

    // Parent block b0 certified by the highest PC (view 1).
    let b0 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([1u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&b0, None, None).unwrap();
    set_highest_pc(&mut block_tree, &generic_pc(1, b0.hash));

    let view2 = ViewNumber::new(2);
    let local = proposer_for(view2, &keys, &vss, &block_tree);
    let mut hotstuff = hotstuff_at(ViewNumber::new(1), local, vss, log.clone());

    log.lock().unwrap().clear();
    hotstuff
        .enter_view(view_info_at(view2), &mut block_tree, &mut EmptyBlockApp)
        .expect("enter_view as leader");

    let ops = log.lock().unwrap().clone();
    assert_flush_before_send(&ops);

    // The view-entry write is also durable before the header leaves.
    let hve_idx = first_write_containing(&ops, &variables::HIGHEST_VIEW_ENTERED)
        .expect("HIGHEST_VIEW_ENTERED must be written on view entry");
    let bcast_idx = broadcast_position(&ops).unwrap();
    assert!(
        hve_idx < bcast_idx,
        "HIGHEST_VIEW_ENTERED must be durable before the header broadcast"
    );
}

/// Deferred-proposal retry call site: the parent body was missing on view
/// entry (proposal deferred); once it arrives, the retry flushes the same
/// single safety batch before broadcasting.
#[test]
fn deferred_retry_flushes_propose_batch_before_broadcasting_header() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    let mut block_tree = BlockTreeSingleton::new(LogKV::new(log.clone()));
    block_tree
        .initialize(&AppStateUpdates::new(), &vss)
        .unwrap();

    // Highest PC certifies a block that is NOT yet in the tree (body in
    // flight) -> the leader must defer its proposal.
    let b0 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([1u8; 32]),
        Data::new(vec![]),
    );
    set_highest_pc(&mut block_tree, &generic_pc(1, b0.hash));

    let view2 = ViewNumber::new(2);
    let local = proposer_for(view2, &keys, &vss, &block_tree);
    let mut hotstuff = hotstuff_at(ViewNumber::new(1), local, vss, log.clone());

    hotstuff
        .enter_view(view_info_at(view2), &mut block_tree, &mut EmptyBlockApp)
        .expect("enter_view with missing parent");
    assert!(
        hotstuff.has_deferred_proposal(),
        "missing parent body must defer the proposal"
    );
    assert!(
        broadcast_position(&log.lock().unwrap()).is_none(),
        "nothing must be broadcast while the proposal is deferred"
    );

    // The parent body arrives; the retry (same view) must propose with the
    // same flush-before-send ordering.
    block_tree.insert(&b0, None, None).unwrap();
    log.lock().unwrap().clear();
    hotstuff
        .enter_view(view_info_at(view2), &mut block_tree, &mut EmptyBlockApp)
        .expect("deferred-proposal retry");

    let ops = log.lock().unwrap().clone();
    assert_flush_before_send(&ops);
}
