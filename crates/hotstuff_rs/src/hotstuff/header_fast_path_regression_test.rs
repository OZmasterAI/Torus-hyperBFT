//! T1.3 regression tests: the three header-first fast-path relaxations must be
//! OBSERVABLE and BOUNDED.
//!
//! Locks in the tightening of `on_receive_proposal_header` /
//! [`try_insert_body`](crate::hotstuff::implementation::HotStuff) in
//! [`crate::hotstuff::implementation`]:
//!
//! 1. **Metric** — a phase-vote is sent on a `ProposalHeader` *before*
//!    `app.validate_block` runs (validation is deferred until the body
//!    arrives). When the body later fails app validation, the
//!    `header_vote_invalid_count` metric must be incremented — exactly once per
//!    block, no matter how many times the invalid body is (re)delivered.
//! 2. **Retained lock check** — when the header's justify points at a block
//!    that is tracked in `pending_headers`/`pending_bodies` ("previously
//!    validated"), `safe_pc` used to be bypassed *entirely*. The bypass may
//!    only skip the block-in-tree predicate (clause 2 of `safe_pc`); the LOCK
//!    clause (clause 3: `pc.view > locked_pc.view` OR `pc.block` extends
//!    `locked_pc.block`) must still be enforced.
//!
//! # RED-first
//!
//! On pre-T1.3 code:
//! * [`pending_justify_bypass_enforces_lock_clause`] compiles against the old
//!   API and FAILS at its final assertions: the bypass treated the
//!   lock-violating header as safe, so `take_sync_needed()` stayed `false`.
//! * [`header_vote_on_app_invalid_body_increments_metric`] and
//!   [`safe_pc_lock_clause_semantics`] fail to compile: neither
//!   `HotStuff::header_vote_invalid_count` nor
//!   `invariants::safe_pc_lock_clause` existed.
//!
//! With T1.3 applied, all three pass.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::block_tree::invariants::safe_pc_lock_clause;
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::{BlockDataResponse, HotStuffMessage, ProposalHeader};
use crate::hotstuff::roles::is_proposer_with_reputation;
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::networking::messages::Message;
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
// Minimal in-memory KVStore (mirrors `pc_discard_regression_test`).
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct MemKV {
    map: HashMap<Vec<u8>, Vec<u8>>,
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
        self.map.get(key).cloned()
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
    fn snapshot<'b>(&'b self) -> MemSnap {
        MemSnap(self.map.clone())
    }
}

// ---------------------------------------------------------------------------
// Inert network; apps for each scenario.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct NullNetwork;

impl Network for NullNetwork {
    fn init_validator_set(&mut self, _validator_set: ValidatorSet) {}
    fn update_validator_set(&mut self, _updates: ValidatorSetUpdates) {}
    fn broadcast(&mut self, _message: Message) {}
    fn send(&mut self, _peer: VerifyingKey, _message: Message) {}
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

/// The header handlers never call the app; only body insertion does.
struct NullApp;

impl App<MemKV> for NullApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("produce_block is not reached on the header paths under test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("validate_block is not reached on the header paths under test")
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<MemKV>,
    ) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the header paths under test")
    }
}

/// Rejects every body: models an app that finds a header-voted block invalid.
struct RejectingApp {
    validate_calls: u32,
}

impl App<MemKV> for RejectingApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("produce_block is not reached on the body-insertion path under test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        self.validate_calls += 1;
        ValidateBlockResponse::Invalid
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<MemKV>,
    ) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the body-insertion path under test")
    }
}

// ---------------------------------------------------------------------------
// Test fixtures.
// ---------------------------------------------------------------------------

const CHAIN_ID: ChainID = ChainID::new(0);

/// Deterministic signing keys from fixed seed bytes.
fn signing_keys(seeds: &[u8]) -> Vec<SigningKey> {
    seeds
        .iter()
        .map(|s| SigningKey::from_bytes(&[*s; 32]))
        .collect()
}

/// Build a `ValidatorSet` from signing keys, each with power 1.
fn validator_set(keys: &[SigningKey]) -> ValidatorSet {
    let mut vs = ValidatorSet::new();
    for k in keys {
        vs.put(&k.verifying_key(), Power::new(1));
    }
    vs
}

/// A steady-state (no in-flight VS transition) block tree over `set`.
fn steady_block_tree(set: &ValidatorSet) -> (BlockTreeSingleton<MemKV>, ValidatorSetState) {
    let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
    let mut block_tree = BlockTreeSingleton::new(MemKV::default());
    block_tree
        .initialize(&AppStateUpdates::new(), &vss)
        .expect("block tree initialization");
    (block_tree, vss)
}

/// Manually assemble a quorum-signed Generic `PhaseCertificate` for
/// `block`/`view` (exactly as the `PhaseVoteCollector` would).
fn generic_pc(
    view: ViewNumber,
    block: CryptoHash,
    signers: &[SigningKey],
    set: &ValidatorSet,
) -> PhaseCertificate {
    let message = (CHAIN_ID, view, block, Phase::Generic)
        .try_to_vec()
        .unwrap();
    let mut signatures = SignatureSet::new(set.len());
    for sk in signers {
        let vk = sk.verifying_key();
        let pos = set
            .position(&vk)
            .expect("signer must belong to the validator set");
        signatures.set(pos, Some(Keypair::new(sk.clone()).sign(&message)));
    }
    PhaseCertificate {
        chain_id: CHAIN_ID,
        view,
        block,
        phase: Phase::Generic,
        signatures,
    }
}

/// Build the `ProposalHeader` that announces `block` in `view`.
fn header_for(block: &Block, view: ViewNumber) -> ProposalHeader {
    ProposalHeader {
        chain_id: CHAIN_ID,
        view,
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
    }
}

/// Find the proposer for `view`, computed exactly as `on_receive_msg`'s
/// entry gate computes it.
fn proposer_for<K: KVStore>(
    view: ViewNumber,
    keys: &[SigningKey],
    vss: &ValidatorSetState,
    block_tree: &BlockTreeSingleton<K>,
) -> VerifyingKey {
    let reputation = block_tree.leader_reputation().ok();
    keys.iter()
        .map(|k| k.verifying_key())
        .find(|vk| is_proposer_with_reputation(vk, view, vss, reputation.as_ref()))
        .expect("one of the validators must be the proposer for the view")
}

/// A `HotStuff` participant at `view` whose keypair is `local` (must be in the
/// committed validator set for it to phase-vote).
fn hotstuff_at(
    view: ViewNumber,
    local: SigningKey,
    vss: ValidatorSetState,
) -> HotStuff<NullNetwork> {
    let config = HotStuffConfiguration {
        chain_id: CHAIN_ID,
        keypair: Keypair::new(local),
    };
    let view_info = ViewInfo::new(view, Instant::now() + Duration::from_secs(3600));
    HotStuff::new(
        config,
        view_info,
        SenderHandle::new(NullNetwork),
        ValidatorSetUpdateHandle::new(NullNetwork),
        vss,
        None,
    )
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// T1.3 (2): the pending-justify `safe_pc` bypass may skip only the
/// block-in-tree predicate — the LOCK clause must still be enforced.
///
/// A first header (genesis-justified block `b1`) is voted on and parked in
/// `pending_headers` (body in flight). A second header then arrives whose
/// justify is a *cryptographically correct* quorum PC on `b1`, but with
/// `pc.view == 0`: not greater than `locked_pc.view` (genesis, also 0) and not
/// extending `locked_pc.block` — a lock-clause violation.
///
/// RED on pre-T1.3 code: because `b1` is tracked in `pending_headers`, the
/// bypass skipped `safe_pc` entirely and treated the header as safe, so no
/// sync was flagged. GREEN with T1.3: the retained lock clause rejects the
/// header; the unsafe-header branch flags sync (the justify block is not in
/// the tree) and casts no vote.
#[test]
fn pending_justify_bypass_enforces_lock_clause() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let mut hotstuff = hotstuff_at(view1, keys[0].clone(), vss.clone());
    let mut app = NullApp;

    // Header 1: genesis-justified block. Passes the full safe_pc path, is
    // phase-voted, and is parked in pending_headers awaiting its body.
    let b1 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([42u8; 32]),
        Data::new(vec![]),
    );
    let h1 = header_for(&b1, view1);
    let p1 = proposer_for(view1, &keys, &vss, &block_tree);
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(h1), &p1, &mut block_tree, &mut app)
        .expect("processing a safe header must not error");
    assert_eq!(
        block_tree
            .highest_view_voted()
            .expect("highest_view_voted read"),
        Some(view1),
        "control: the replica must phase-vote on the safe genesis-justified header"
    );
    assert!(
        !hotstuff.take_sync_needed(),
        "control: a safe header must not flag sync"
    );

    // Header 2: justify is a correct (quorum-signed) Generic PC on b1 — which
    // is tracked in pending_headers, so the bypass fires — but pc.view (0) is
    // NOT greater than locked_pc.view (genesis, 0) and b1 does not extend the
    // locked block: the LOCK clause is violated.
    let stale_justify = generic_pc(ViewNumber::new(0), b1.hash, &keys[..3], &set);
    let b2 = Block::new(
        BlockHeight::new(1),
        stale_justify,
        CryptoHash::new([43u8; 32]),
        Data::new(vec![]),
    );
    let view2 = ViewNumber::new(2);
    let h2 = header_for(&b2, view2);
    let p2 = proposer_for(view2, &keys, &vss, &block_tree);
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(h2), &p2, &mut block_tree, &mut app)
        .expect("processing an unsafe header must not error");

    assert!(
        hotstuff.take_sync_needed(),
        "a pending-justify header that violates the lock clause must be rejected \
         via the unsafe-header branch (which flags sync for the unknown justify block)"
    );
    assert_eq!(
        block_tree
            .highest_view_voted()
            .expect("highest_view_voted read"),
        Some(view1),
        "no vote may be recorded for a lock-violating header"
    );
}

/// T1.3 (1): voting on a header whose body later fails `app.validate_block`
/// must increment the `header_vote_invalid_count` metric — exactly once per
/// block, even if the invalid body is redelivered.
///
/// RED on pre-T1.3 code: `HotStuff::header_vote_invalid_count` did not exist
/// (compile failure); the vote-then-found-invalid sequence was silent.
#[test]
fn header_vote_on_app_invalid_body_increments_metric() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let mut hotstuff = hotstuff_at(view1, keys[0].clone(), vss.clone());
    let mut app = RejectingApp { validate_calls: 0 };

    // Vote on the header first (body still in flight).
    let b1 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([42u8; 32]),
        Data::new(vec![]),
    );
    let h1 = header_for(&b1, view1);
    let p1 = proposer_for(view1, &keys, &vss, &block_tree);
    hotstuff
        .on_receive_msg(HotStuffMessage::ProposalHeader(h1), &p1, &mut block_tree, &mut app)
        .expect("processing a safe header must not error");
    assert_eq!(
        block_tree
            .highest_view_voted()
            .expect("highest_view_voted read"),
        Some(view1),
        "precondition: the vote must have been cast before the body arrived"
    );
    assert_eq!(
        hotstuff.header_vote_invalid_count(),
        0,
        "the metric must not move before the body's validation outcome is known"
    );

    // The body arrives and the app finds it invalid: the already-sent vote is
    // now known to have endorsed an app-invalid block. The metric must record it.
    let resp = BlockDataResponse {
        view: view1,
        block: b1.clone(),
    };
    hotstuff
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(resp),
            &p1,
            &mut block_tree,
            &mut app,
        )
        .expect("processing an invalid body must not error");
    assert_eq!(app.validate_calls, 1, "precondition: the app validated the body");
    assert_eq!(
        hotstuff.header_vote_invalid_count(),
        1,
        "a header-vote on a block later found app-invalid must be counted"
    );

    // Redelivering the same invalid body must not double-count.
    let resp_again = BlockDataResponse {
        view: view1,
        block: b1.clone(),
    };
    hotstuff
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(resp_again),
            &p1,
            &mut block_tree,
            &mut app,
        )
        .expect("reprocessing an invalid body must not error");
    assert_eq!(
        hotstuff.header_vote_invalid_count(),
        1,
        "the metric counts each header-voted invalid block at most once (bounded)"
    );
}

/// T1.3 (2)/(3): pins the semantics of the extracted lock clause
/// (`invariants::safe_pc_lock_clause`) that both the header-path bypass and
/// the sync-path observability check rely on.
///
/// RED on pre-T1.3 code: `safe_pc_lock_clause` did not exist (compile failure).
#[test]
fn safe_pc_lock_clause_semantics() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, _vss) = steady_block_tree(&set);

    let block_a = CryptoHash::new([7u8; 32]); // NOT in the block tree
    let block_b = CryptoHash::new([8u8; 32]); // NOT in the block tree

    // Fresh tree: locked_pc is the genesis PC (view 0), so any view >= 1
    // passes on the view arm.
    let pc_v1 = generic_pc(ViewNumber::new(1), block_a, &keys[..3], &set);
    assert!(
        safe_pc_lock_clause(&pc_v1, &block_tree).expect("lock clause must not error"),
        "a PC with view greater than the locked view must satisfy the lock clause"
    );

    // Lock on a PC at view 10 over block_a.
    let lock_pc = generic_pc(ViewNumber::new(10), block_a, &keys[..3], &set);
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_locked_pc(&lock_pc).expect("set_locked_pc");
    block_tree.write(wb);

    // View arm: 11 > 10.
    let pc_v11 = generic_pc(ViewNumber::new(11), block_b, &keys[..3], &set);
    assert!(
        safe_pc_lock_clause(&pc_v11, &block_tree).expect("lock clause must not error"),
        "a PC with view greater than the locked view must satisfy the lock clause"
    );

    // Extends arm: view 9 <= 10, but pc.block IS the locked block (this is the
    // arm that keeps the clause checkable for blocks whose bodies are still in
    // flight — no tree membership needed).
    let pc_on_locked = generic_pc(ViewNumber::new(9), block_a, &keys[..3], &set);
    assert!(
        safe_pc_lock_clause(&pc_on_locked, &block_tree).expect("lock clause must not error"),
        "a stale-view PC on the locked block itself must satisfy the lock clause"
    );

    // Violation: view 9 <= 10 on an unrelated, not-in-tree block.
    let pc_conflicting = generic_pc(ViewNumber::new(9), block_b, &keys[..3], &set);
    assert!(
        !safe_pc_lock_clause(&pc_conflicting, &block_tree).expect("lock clause must not error"),
        "a stale-view PC that does not extend the locked block must violate the lock clause"
    );
}
