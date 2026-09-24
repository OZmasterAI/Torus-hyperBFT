//! s65 regression tests: a replica must PERSIST its vote state before the vote
//! leaves the node.
//!
//! `highest_view_voted` is the only thing that stops a restarted replica from
//! voting a second time in the same view. If the vote is sent first and the
//! process dies before `set_vote_state_atomic` lands, the restarted replica
//! sees no vote for that view and may vote again — for a different block — so
//! two conflicting votes from one validator can exist for one view.
//!
//! The KV store and the network below append to one shared log, so each test
//! observes the exact order of "vote state written" and "vote sent".
//!
//! # RED-first
//!
//! On the pre-fix code the header fast path and the full-proposal path called
//! `sender_handle.send(phase_vote)` BEFORE `set_vote_state_atomic`, and the
//! nudge path called it before `set_highest_view_phase_voted`, so all three
//! tests fail on their order assertion.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::block_tree::variables::HIGHEST_VIEW_PHASE_VOTED;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, proposer_for, signing_keys, validator_set,
};
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::{HotStuffMessage, Nudge, Proposal, ProposalHeader};
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

const CHAIN_ID: ChainID = ChainID::new(0);
const PERSIST: &str = "persist_vote_state";
const SEND: &str = "send_vote";

type OrderLog = Arc<Mutex<Vec<&'static str>>>;

// ---------------------------------------------------------------------------
// KV store that logs every write of the vote-state key.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct OrderKV {
    map: HashMap<Vec<u8>, Vec<u8>>,
    log: OrderLog,
}

struct OrderWb {
    sets: Vec<(Vec<u8>, Vec<u8>)>,
    deletes: Vec<Vec<u8>>,
}

struct OrderSnap(HashMap<Vec<u8>, Vec<u8>>);

impl WriteBatch for OrderWb {
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

impl KVGet for OrderKV {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.map.get(key).cloned()
    }
}

impl KVGet for OrderSnap {
    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.0.get(key).cloned()
    }
}

impl KVStore for OrderKV {
    type WriteBatch = OrderWb;
    type Snapshot<'a> = OrderSnap;
    fn write(&mut self, wb: OrderWb) {
        if wb.sets.iter().any(|(k, _)| k[..] == HIGHEST_VIEW_PHASE_VOTED[..]) {
            self.log.lock().unwrap().push(PERSIST);
        }
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
    fn snapshot(&self) -> OrderSnap {
        OrderSnap(self.map.clone())
    }
}

// ---------------------------------------------------------------------------
// Network that logs every phase-vote send.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct OrderNetwork {
    log: OrderLog,
}

impl Network for OrderNetwork {
    fn init_validator_set(&mut self, _validator_set: ValidatorSet) {}
    fn update_validator_set(&mut self, _updates: ValidatorSetUpdates) {}
    fn broadcast(&mut self, _message: Message) {}
    fn send(&mut self, _peer: VerifyingKey, message: Message) {
        if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(
            HotStuffMessage::PhaseVote(_),
        )) = message
        {
            self.log.lock().unwrap().push(SEND);
        }
    }
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

/// Validates every block as app-valid; never asked to produce.
struct ValidApp;

impl App<OrderKV> for ValidApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<OrderKV>) -> ProduceBlockResponse {
        unreachable!("produce_block is not reached on the follower paths under test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<OrderKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<OrderKV>,
    ) -> ValidateBlockResponse {
        unreachable!("sync validation is not reached on the follower paths under test")
    }
}

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

struct Fixture {
    keys: Vec<SigningKey>,
    vss: ValidatorSetState,
    block_tree: BlockTreeSingleton<OrderKV>,
    log: OrderLog,
    /// Child block `b2` (height 1) justified by a correct Generic QC on the
    /// inserted parent `b1`.
    b2: Block,
    view: ViewNumber,
}

/// An initialized 4-validator block tree whose KV store appends to `log`.
fn base_tree() -> (Vec<SigningKey>, ValidatorSet, ValidatorSetState, BlockTreeSingleton<OrderKV>, OrderLog) {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
    let log: OrderLog = Arc::new(Mutex::new(Vec::new()));
    let mut block_tree = BlockTreeSingleton::new(OrderKV {
        map: HashMap::new(),
        log: log.clone(),
    });
    block_tree
        .initialize(&AppStateUpdates::new(), &vss)
        .expect("block tree initialization");
    (keys, set, vss, block_tree, log)
}

/// A quorum-signed `PhaseCertificate` of any `phase` (as `generic_pc`, which
/// is Generic-only).
fn pc(
    view: ViewNumber,
    block: CryptoHash,
    phase: Phase,
    signers: &[SigningKey],
    set: &ValidatorSet,
) -> PhaseCertificate {
    let message = (CHAIN_ID, view, block, phase).try_to_vec().unwrap();
    let mut signatures = SignatureSet::new(set.len());
    for sk in signers {
        let pos = set.position(&sk.verifying_key()).expect("signer in set");
        signatures.set(pos, Some(Keypair::new(sk.clone()).sign(&message)));
    }
    PhaseCertificate {
        chain_id: CHAIN_ID,
        view,
        block,
        phase,
        signatures,
    }
}

/// A 4-validator block tree holding parent `b1`, plus child `b2` to vote on in
/// view 2 — the same shape as `header_vote_locks_before_vote`.
fn fixture() -> Fixture {
    let (keys, set, vss, mut block_tree, log) = base_tree();

    let b1 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([42u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&b1, None, None).expect("insert parent b1");
    let justify = generic_pc(ViewNumber::new(1), b1.hash, &keys[..3], &set);
    let b2 = Block::new(
        BlockHeight::new(1),
        justify,
        CryptoHash::new([43u8; 32]),
        Data::new(vec![]),
    );
    log.lock().unwrap().clear(); // only the vote under test matters
    Fixture {
        keys,
        vss,
        block_tree,
        log,
        b2,
        view: ViewNumber::new(2),
    }
}

/// Replica `key` at `view`, whose network appends to `log`.
fn replica(
    key: &SigningKey,
    vss: &ValidatorSetState,
    log: &OrderLog,
    view: ViewNumber,
) -> HotStuff<OrderNetwork> {
    let net = OrderNetwork { log: log.clone() };
    HotStuff::new(
        HotStuffConfiguration {
            chain_id: CHAIN_ID,
            keypair: Keypair::new(key.clone()),
        },
        ViewInfo::new(view, Instant::now() + Duration::from_secs(3600)),
        SenderHandle::new(net.clone()),
        ValidatorSetUpdateHandle::new(net),
        vss.clone(),
        None,
    )
}

fn assert_persist_before_send(log: &OrderLog, path: &str) {
    let log = log.lock().unwrap().clone();
    let persist = log.iter().position(|e| *e == PERSIST);
    let send = log.iter().position(|e| *e == SEND);
    let send = send.unwrap_or_else(|| panic!("{path}: the replica must vote (log: {log:?})"));
    let persist =
        persist.unwrap_or_else(|| panic!("{path}: the vote state must be persisted (log: {log:?})"));
    assert!(
        persist < send,
        "{path}: vote state must be persisted BEFORE the vote is sent, got {log:?}"
    );
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// Header fast path (`on_receive_proposal_header`).
#[test]
fn header_path_persists_vote_state_before_sending_vote() {
    let mut f = fixture();
    let header = ProposalHeader {
        chain_id: CHAIN_ID,
        view: f.view,
        block_hash: f.b2.hash,
        height: f.b2.height,
        data_hash: f.b2.data_hash,
        justify: f.b2.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
    };
    let proposer = proposer_for(f.view, &f.keys, &f.vss, &f.block_tree);
    let mut hotstuff = replica(&f.keys[0], &f.vss, &f.log, f.view);
    hotstuff
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(header),
            &proposer,
            &mut f.block_tree,
            &mut ValidApp,
        )
        .expect("processing a safe header must not error");
    assert_persist_before_send(&f.log, "header path");
}

/// Full-proposal path (`on_receive_proposal`).
#[test]
fn proposal_path_persists_vote_state_before_sending_vote() {
    let mut f = fixture();
    let proposal = Proposal {
        chain_id: CHAIN_ID,
        view: f.view,
        block: f.b2.clone(),
        tc: None,
        nec: None,
    };
    let proposer = proposer_for(f.view, &f.keys, &f.vss, &f.block_tree);
    let mut hotstuff = replica(&f.keys[0], &f.vss, &f.log, f.view);
    hotstuff
        .on_receive_msg(
            HotStuffMessage::Proposal(proposal),
            &proposer,
            &mut f.block_tree,
            &mut ValidApp,
        )
        .expect("processing a safe proposal must not error");
    assert_persist_before_send(&f.log, "proposal path");
}

/// Nudge path (`on_receive_nudge`): a Prepare-phase QC on a validator-set-updating
/// block, one view back, is nudged and must be voted (Precommit).
#[test]
fn nudge_path_persists_vote_state_before_sending_vote() {
    let (keys, set, vss, mut block_tree, log) = base_tree();
    let b1 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([44u8; 32]),
        Data::new(vec![]),
    );
    let mut vs_updates = ValidatorSetUpdates::new();
    vs_updates.insert(signing_keys(&[5])[0].verifying_key(), Power::new(1));
    block_tree
        .insert(&b1, None, Some(&vs_updates))
        .expect("insert validator-set-updating b1");
    log.lock().unwrap().clear();

    let view = ViewNumber::new(2);
    let nudge = Nudge {
        chain_id: CHAIN_ID,
        view,
        justify: pc(ViewNumber::new(1), b1.hash, Phase::Prepare, &keys[..3], &set),
    };
    let proposer = proposer_for(view, &keys, &vss, &block_tree);
    let mut hotstuff = replica(&keys[0], &vss, &log, view);
    hotstuff
        .on_receive_msg(
            HotStuffMessage::Nudge(nudge),
            &proposer,
            &mut block_tree,
            &mut ValidApp,
        )
        .expect("processing a safe nudge must not error");
    assert_persist_before_send(&log, "nudge path");
}
