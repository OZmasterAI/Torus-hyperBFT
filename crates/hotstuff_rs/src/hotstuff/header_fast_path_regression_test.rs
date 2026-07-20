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
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::block_tree::invariants::safe_pc_lock_clause;
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::events::Event;
use crate::hotstuff::implementation::{
    parse_body_push_max_bytes, should_push_body, HotStuff, HotStuffConfiguration,
    BODY_PUSH_HARD_CAP_BYTES, PUSHED_BODY_BUFFER_CAP,
};
use crate::hotstuff::messages::{
    BlockDataRequest, BlockDataResponse, HotStuffMessage, Proposal, ProposalHeader,
};
use crate::hotstuff::roles::is_proposer_with_reputation;
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::networking::messages::{Message, ProgressMessage};
use crate::networking::network::{Network, ValidatorSetUpdateHandle};
use crate::networking::sending::SenderHandle;
use crate::pacemaker::implementation::ViewInfo;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{
    BlockHeight, ChainID, CryptoHash, Data, Datum, Power, SignatureSet, ViewNumber,
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

/// Like [`hotstuff_at`], but wires an event publisher so a test can observe the
/// ORDER in which block-tree updates and phase-votes are emitted.
fn hotstuff_at_with_events(
    view: ViewNumber,
    local: SigningKey,
    vss: ValidatorSetState,
) -> (HotStuff<NullNetwork>, Receiver<Event>) {
    let config = HotStuffConfiguration {
        chain_id: CHAIN_ID,
        keypair: Keypair::new(local),
    };
    let view_info = ViewInfo::new(view, Instant::now() + Duration::from_secs(3600));
    let (tx, rx) = std::sync::mpsc::channel();
    let hotstuff = HotStuff::new(
        config,
        view_info,
        SenderHandle::new(NullNetwork),
        ValidatorSetUpdateHandle::new(NullNetwork),
        vss,
        Some(tx),
    );
    (hotstuff, rx)
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

/// PART 1 (PRIMARY): the header fast-path must advance `locked_pc` from the
/// header's justify BEFORE emitting its phase-vote — mirroring the full-block
/// path, which calls `block_tree.update(&justify)` (lock-on-parent via
/// `pc_to_lock`) *before* voting. The broken fast-path voted WITHOUT locking,
/// deferring the lock to body arrival and opening a conflicting-vote window.
///
/// RED on pre-fix code: `on_receive_proposal_header` never touched `locked_pc`,
/// so after processing a safe header `locked_pc` stayed at the genesis PC and no
/// `UpdateLockedPC` event was published.
#[test]
fn header_vote_locks_before_vote() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);

    // Parent b1 (genesis-justified), inserted directly so the child header's
    // Generic justify passes the FULL safe_pc path (block-in-tree + non-VS).
    let b1 = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([42u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&b1, None, None).expect("insert parent b1");

    // Header for child b2 whose justify is a correct Generic QC on b1 at view 1.
    let justify = generic_pc(ViewNumber::new(1), b1.hash, &keys[..3], &set);
    let b2 = Block::new(
        BlockHeight::new(1),
        justify.clone(),
        CryptoHash::new([43u8; 32]),
        Data::new(vec![]),
    );
    let view2 = ViewNumber::new(2);
    let h2 = header_for(&b2, view2);

    let (mut hotstuff, events) = hotstuff_at_with_events(view2, keys[0].clone(), vss.clone());
    let mut app = NullApp;
    let p2 = proposer_for(view2, &keys, &vss, &block_tree);
    hotstuff
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(h2),
            &p2,
            &mut block_tree,
            &mut app,
        )
        .expect("processing a safe header must not error");

    // The replica voted...
    assert_eq!(
        block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(view2),
        "control: the safe header must be phase-voted"
    );
    // ...and locked on the header's justify (the parent). PhaseCertificate has
    // no `Debug`, so compare the identifying fields.
    let locked = block_tree.locked_pc().expect("locked_pc");
    assert!(
        locked.view == justify.view
            && locked.block == justify.block
            && locked.phase == justify.phase,
        "the header fast-path must advance locked_pc to the header's justify at vote time \
         (got view={}, expected view={})",
        locked.view.int(),
        justify.view.int()
    );

    // Event ORDER proves lock-before-vote: UpdateLockedPC precedes PhaseVote.
    let mut locked_idx = None;
    let mut vote_idx = None;
    for (i, ev) in events.try_iter().enumerate() {
        match ev {
            Event::UpdateLockedPC(_) if locked_idx.is_none() => locked_idx = Some(i),
            Event::PhaseVote(_) if vote_idx.is_none() => vote_idx = Some(i),
            _ => {}
        }
    }
    let locked_idx = locked_idx.expect("an UpdateLockedPC event must be published on the fast path");
    let vote_idx = vote_idx.expect("a PhaseVote event must be published on the fast path");
    assert!(
        locked_idx < vote_idx,
        "the lock must be updated BEFORE the vote is emitted (got locked@{locked_idx}, vote@{vote_idx})"
    );
}

/// PART 1 regression — the double-vote window. A validator header-votes block C
/// (justify QC(B)); with the fix it is now LOCKED on B. A conflicting sibling B'
/// (child of A, justify QC(A)) then arrives in a LATER view. The vote-view gate
/// is satisfied (later view), so ONLY the lock clause can refuse it — and it
/// must: QC(A) neither out-views nor extends the locked block B.
///
/// RED on pre-fix code: header-voting C did NOT advance `locked_pc` (it stayed
/// at genesis), so QC(A).view > locked.view and the conflicting sibling B' was
/// (unsafely) voted in the later view.
#[test]
fn header_double_vote_window_refused() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);

    // Chain A <- B, both inserted so headers built on their QCs pass safe_pc.
    let a = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([10u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&a, None, None).expect("insert A");
    let qc_a = generic_pc(ViewNumber::new(1), a.hash, &keys[..3], &set);
    let b = Block::new(
        BlockHeight::new(1),
        qc_a.clone(),
        CryptoHash::new([11u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&b, None, None).expect("insert B");
    let qc_b = generic_pc(ViewNumber::new(2), b.hash, &keys[..3], &set);

    // C extends B. Header-vote C at view 3: this must lock on B (C's parent).
    let c = Block::new(
        BlockHeight::new(2),
        qc_b.clone(),
        CryptoHash::new([12u8; 32]),
        Data::new(vec![]),
    );
    let view_c = ViewNumber::new(3);
    let hc = header_for(&c, view_c);
    let mut hs1 = hotstuff_at(view_c, keys[0].clone(), vss.clone());
    let mut app = NullApp;
    let prop_c = proposer_for(view_c, &keys, &vss, &block_tree);
    hs1.on_receive_msg(
        HotStuffMessage::ProposalHeader(hc),
        &prop_c,
        &mut block_tree,
        &mut app,
    )
    .expect("processing C header must not error");
    assert_eq!(
        block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(view_c),
        "precondition: C must be header-voted"
    );
    let locked_after_c = block_tree.locked_pc().expect("locked_pc");
    assert!(
        locked_after_c.view == qc_b.view
            && locked_after_c.block == qc_b.block
            && locked_after_c.phase == qc_b.phase,
        "precondition: header-voting C must lock on B (its parent); \
         got locked view={}, expected view={}",
        locked_after_c.view.int(),
        qc_b.view.int()
    );

    // Conflicting sibling B' of B (also a child of A, justify QC(A)) arrives in
    // a LATER view 4. A fresh participant at view 4 over the SAME block tree
    // (lock + highest_view_voted persist there) makes the vote-view gate pass,
    // so ONLY the lock clause can refuse B'.
    let b_prime = Block::new(
        BlockHeight::new(1),
        qc_a.clone(),
        CryptoHash::new([99u8; 32]),
        Data::new(vec![]),
    );
    let view_bp = ViewNumber::new(4);
    let hbp = header_for(&b_prime, view_bp);
    let mut hs2 = hotstuff_at(view_bp, keys[0].clone(), vss.clone());
    let prop_bp = proposer_for(view_bp, &keys, &vss, &block_tree);
    hs2.on_receive_msg(
        HotStuffMessage::ProposalHeader(hbp),
        &prop_bp,
        &mut block_tree,
        &mut app,
    )
    .expect("processing B' header must not error");

    assert_eq!(
        block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(view_c),
        "the lock (advanced to B by header-voting C) must REFUSE the conflicting \
         sibling B' even though B' arrives in a later, unvoted view"
    );
}

// ---------------------------------------------------------------------------
// Recording-network fixtures (kept from the reverted inline-body change —
// reusable for wire-level leader/follower assertions).
// ---------------------------------------------------------------------------

/// Network stub that records every outbound message (broadcast + direct, the
/// latter with its target) and every dedicated-protocol body request, so tests
/// can assert exactly what a leader put on the wire and whether a follower had
/// to fetch a body.
#[derive(Clone, Default)]
struct RecordingNetwork {
    sent: Arc<Mutex<Vec<Message>>>,
    direct: Arc<Mutex<Vec<(VerifyingKey, Message)>>>,
    body_requests: Arc<Mutex<Vec<BlockDataRequest>>>,
}

impl Network for RecordingNetwork {
    fn init_validator_set(&mut self, _validator_set: ValidatorSet) {}
    fn update_validator_set(&mut self, _updates: ValidatorSetUpdates) {}
    fn broadcast(&mut self, message: Message) {
        self.sent.lock().unwrap().push(message);
    }
    fn send(&mut self, peer: VerifyingKey, message: Message) {
        self.sent.lock().unwrap().push(message.clone());
        self.direct.lock().unwrap().push((peer, message));
    }
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
    fn request_block_data(&mut self, _peer: VerifyingKey, request: BlockDataRequest) {
        self.body_requests.lock().unwrap().push(request);
    }
}

impl RecordingNetwork {
    /// Full `Proposal`s broadcast/sent so far.
    fn full_proposals(&self) -> Vec<Proposal> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                    HotStuffMessage::Proposal(p),
                )) => Some(p.clone()),
                _ => None,
            })
            .collect()
    }

    /// `ProposalHeader`s broadcast/sent so far.
    fn headers(&self) -> Vec<ProposalHeader> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                    HotStuffMessage::ProposalHeader(h),
                )) => Some(h.clone()),
                _ => None,
            })
            .collect()
    }

    /// Unsolicited pushed bodies direct-sent so far: `(target, response)`
    /// pairs for every `BlockDataResponse` this node sent point-to-point.
    fn pushed_body_responses(&self) -> Vec<(VerifyingKey, BlockDataResponse)> {
        self.direct
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(peer, m)| match m {
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                    HotStuffMessage::BlockDataResponse(r),
                )) => Some((*peer, r.clone())),
                _ => None,
            })
            .collect()
    }

    /// Body fetches issued so far — via the dedicated protocol AND via the
    /// `HotStuffMessage::BlockDataRequest` direct-message fallback.
    fn body_fetch_count(&self) -> usize {
        self.body_requests.lock().unwrap().len()
            + self
                .sent
                .lock()
                .unwrap()
                .iter()
                .filter(|m| {
                    matches!(
                        m,
                        Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                            HotStuffMessage::BlockDataRequest(_)
                        ))
                    )
                })
                .count()
    }
}

/// Like [`hotstuff_at`], but over a [`RecordingNetwork`] whose handle is
/// returned for outbound-message assertions.
fn hotstuff_recording_at(
    view: ViewNumber,
    local: SigningKey,
    vss: ValidatorSetState,
) -> (HotStuff<RecordingNetwork>, RecordingNetwork) {
    let net = RecordingNetwork::default();
    let config = HotStuffConfiguration {
        chain_id: CHAIN_ID,
        keypair: Keypair::new(local),
    };
    let view_info = ViewInfo::new(view, Instant::now() + Duration::from_secs(3600));
    let hotstuff = HotStuff::new(
        config,
        view_info,
        SenderHandle::new(net.clone()),
        ValidatorSetUpdateHandle::new(net.clone()),
        vss,
        None,
    );
    (hotstuff, net)
}

/// App that produces a block whose body is exactly `datums`, and validates
/// every block as app-valid (used on both the leader and follower sides of the
/// body-push tests).
struct FixedBodyApp {
    datums: Vec<Vec<u8>>,
}

impl App<MemKV> for FixedBodyApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        ProduceBlockResponse {
            data: Data::new(self.datums.iter().cloned().map(Datum::new).collect()),
            data_hash: CryptoHash::new([9u8; 32]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<MemKV>,
    ) -> ValidateBlockResponse {
        unreachable!("sync validation is not reached in the body-push tests")
    }
}

/// Drive `local` (which must be the proposer for `view`) through `enter_view`
/// so it produces and broadcasts a proposal, and return its network handle.
fn propose_as_leader(
    view: ViewNumber,
    keys: &[SigningKey],
    vss: &ValidatorSetState,
    block_tree: &mut BlockTreeSingleton<MemKV>,
    datums: Vec<Vec<u8>>,
    push_threshold: Option<u64>,
) -> RecordingNetwork {
    let leader_vk = proposer_for(view, keys, vss, block_tree);
    let leader_key = keys
        .iter()
        .find(|k| k.verifying_key() == leader_vk)
        .expect("proposer key must be among the fixture keys")
        .clone();
    let (mut leader, net) = hotstuff_recording_at(view, leader_key, vss.clone());
    if let Some(threshold) = push_threshold {
        leader.set_body_push_max_bytes(threshold);
    }
    let mut app = FixedBodyApp { datums };
    leader
        .enter_view(
            ViewInfo::new(view, Instant::now() + Duration::from_secs(3600)),
            block_tree,
            &mut app,
        )
        .expect("enter_view as the proposer must not error");
    net
}

// ---------------------------------------------------------------------------
// L3 v2: proactive body push (TORUS_BODY_PUSH_MAX_BYTES).
//
// RED on pre-v2 code: `HotStuff::set_body_push_max_bytes`,
// `parse_body_push_max_bytes`, `should_push_body`, `BODY_PUSH_HARD_CAP_BYTES`
// and `PUSHED_BODY_BUFFER_CAP` did not exist (compile failure);
// `broadcast_proposal_as_header` never direct-sent a body, and an unsolicited
// `BlockDataResponse` for an untracked hash was dropped outright, so a pushed
// body could never spare the fetch. GREEN with v2: the header broadcast is
// byte-identical to today, the body additionally travels as an unsolicited
// `BlockDataResponse` (view-exempt, existing wire variant), and followers
// insert it through the existing pending-header machinery with zero
// `BlockDataRequest`s — while every default/fallback path stays exact-today.
// ---------------------------------------------------------------------------

/// L3 v2 (primary): with the push threshold ON and the body under it, the
/// leader broadcasts the header UNCHANGED and pushes the body to each other
/// validator; a follower that receives the push (even BEFORE the header —
/// the worst-case ordering) inserts it via the pending-header path when the
/// header arrives, votes as today, and never issues a `BlockDataRequest`.
#[test]
fn body_push_small_block_delivers_body_without_request() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut leader_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let leader_net = propose_as_leader(
        view1,
        &keys,
        &vss,
        &mut leader_tree,
        vec![], // empty body — the idle-devnet case the attribution measured
        Some(BODY_PUSH_HARD_CAP_BYTES),
    );

    // Leader wire shape: exactly one ProposalHeader broadcast (unchanged),
    // plus one unsolicited BlockDataResponse per OTHER validator — and never
    // a full-Proposal broadcast (the reverted v1 liveness hazard).
    let headers = leader_net.headers();
    assert_eq!(
        headers.len(),
        1,
        "the header broadcast must be unconditional and unchanged"
    );
    assert!(
        leader_net.full_proposals().is_empty(),
        "the v1 full-Proposal broadcast must stay dead — the push rides BlockDataResponse"
    );
    let leader_vk = proposer_for(view1, &keys, &vss, &leader_tree);
    let pushes = leader_net.pushed_body_responses();
    assert_eq!(
        pushes.len(),
        3,
        "the body must be pushed to each other committed validator"
    );
    assert!(
        pushes.iter().all(|(target, _)| *target != leader_vk),
        "the leader must not push to itself"
    );
    let header = headers.into_iter().next().unwrap();
    assert!(
        pushes
            .iter()
            .all(|(_, resp)| resp.block.hash == header.block_hash && resp.view == header.view),
        "every push must carry the proposed block for the header's view"
    );

    // Follower side, worst-case ordering: the push arrives BEFORE the header.
    // It must be parked — not inserted, not voted (receiving a body never
    // implies anything) — until the verified header claims it.
    let (mut follower_tree, _) = steady_block_tree(&set);
    let follower_key = keys
        .iter()
        .find(|k| k.verifying_key() != leader_vk)
        .expect("a non-leader key must exist")
        .clone();
    let (mut follower, follower_net) = hotstuff_recording_at(view1, follower_key, vss.clone());
    let mut follower_app = FixedBodyApp { datums: vec![] };

    let (_, pushed) = leader_net
        .pushed_body_responses()
        .into_iter()
        .next()
        .unwrap();
    follower
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(pushed),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("an unsolicited pushed body must not error");
    assert!(
        !follower_tree.contains(&header.block_hash),
        "a pushed body alone (header not yet seen) must not be inserted"
    );
    assert_eq!(
        follower.pushed_body_buffer_len(),
        1,
        "the early pushed body must be parked awaiting its header"
    );

    // The header arrives: vote as today (vote-before-DA unchanged), then the
    // parked body is consumed — inserted with ZERO fetch round trips.
    follower
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(header.clone()),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("processing the header must not error");
    assert!(
        follower_tree.contains(&header.block_hash),
        "the header must consume the parked pushed body and insert the block"
    );
    assert_eq!(
        follower_tree
            .highest_view_voted()
            .expect("highest_view_voted"),
        Some(view1),
        "the follower must still phase-vote on the header exactly as today"
    );
    assert_eq!(
        follower_net.body_fetch_count(),
        0,
        "the body was pushed — no BlockDataRequest may be issued"
    );
    assert_eq!(
        follower.pushed_body_buffer_len(),
        0,
        "the parked body must be consumed, not retained"
    );

    // Idempotency: a duplicate push after insertion is benign — not re-parked
    // (the block is in the tree), no fetch, no error.
    let (_, dup) = leader_net
        .pushed_body_responses()
        .into_iter()
        .next()
        .unwrap();
    follower
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(dup),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("a duplicate push must not error");
    assert_eq!(follower.pushed_body_buffer_len(), 0);
    assert_eq!(follower_net.body_fetch_count(), 0);
}

/// L3 v2 default: threshold unset/0 keeps BOTH sides byte-identical to today —
/// the leader broadcasts a header and pushes nothing; the follower issues
/// exactly one solicited `BlockDataRequest` at header processing and inserts
/// on the solicited response. Also pins the pure parse: unset/junk mean 0=OFF.
#[test]
fn body_push_default_zero_is_byte_identical_header_first() {
    // Pure parse semantics: default OFF.
    assert_eq!(parse_body_push_max_bytes(None), 0);
    assert_eq!(parse_body_push_max_bytes(Some("".to_string())), 0);
    assert_eq!(parse_body_push_max_bytes(Some("junk".to_string())), 0);
    assert_eq!(parse_body_push_max_bytes(Some(" 4096 ".to_string())), 4096);
    assert!(
        !should_push_body(0, 0),
        "0 means OFF even for an empty body"
    );

    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut leader_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    // No threshold override: the constructor default (env unset => 0) applies.
    let leader_net = propose_as_leader(view1, &keys, &vss, &mut leader_tree, vec![], None);

    assert_eq!(
        leader_net.headers().len(),
        1,
        "default 0 = exact-today: the proposal must go out header-first"
    );
    assert!(
        leader_net.full_proposals().is_empty(),
        "default 0 = exact-today: no full Proposal may be broadcast"
    );
    assert!(
        leader_net.pushed_body_responses().is_empty(),
        "default 0 = exact-today: nothing may be pushed"
    );

    // Follower: today's pipeline — vote on the header, exactly one solicited
    // body request, insert on the solicited response.
    let header = leader_net.headers().into_iter().next().unwrap();
    let leader_vk = proposer_for(view1, &keys, &vss, &leader_tree);
    let (mut follower_tree, _) = steady_block_tree(&set);
    let follower_key = keys
        .iter()
        .find(|k| k.verifying_key() != leader_vk)
        .expect("a non-leader key must exist")
        .clone();
    let (mut follower, follower_net) = hotstuff_recording_at(view1, follower_key, vss.clone());
    let mut follower_app = FixedBodyApp { datums: vec![] };
    follower
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(header.clone()),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("processing the header must not error");
    assert_eq!(
        follower_net.body_fetch_count(),
        1,
        "exact-today: one solicited BlockDataRequest at header processing"
    );
    assert!(!follower_tree.contains(&header.block_hash));

    // Reconstruct the proposed block (FixedBodyApp is deterministic) and
    // deliver it as the solicited response.
    let block = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([9u8; 32]),
        Data::new(vec![]),
    );
    assert_eq!(
        block.hash, header.block_hash,
        "fixture: reconstructed block must be the proposed block"
    );
    follower
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: header.view,
                block,
            }),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("processing the solicited response must not error");
    assert!(
        follower_tree.contains(&header.block_hash),
        "exact-today: the solicited response must insert the block"
    );
    assert_eq!(
        follower_net.body_fetch_count(),
        1,
        "exact-today: no additional requests"
    );
}

/// L3 v2 bound: a body over the threshold is NOT pushed even with the knob ON
/// — the follower falls back to today's request/response fetch. Also pins the
/// boundary semantics of the pure decision.
#[test]
fn body_push_over_threshold_falls_back_to_fetch() {
    // Boundary semantics of the pure decision.
    assert!(should_push_body(8, 8), "at-threshold must push");
    assert!(!should_push_body(9, 8), "over-threshold must not push");

    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut leader_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let leader_net = propose_as_leader(
        view1,
        &keys,
        &vss,
        &mut leader_tree,
        vec![vec![0u8; 100]], // 100-byte body > 8-byte threshold
        Some(8),
    );

    assert_eq!(
        leader_net.headers().len(),
        1,
        "an over-threshold body must keep the header-first pipeline"
    );
    assert!(
        leader_net.pushed_body_responses().is_empty(),
        "an over-threshold body must not be pushed"
    );
    assert!(leader_net.full_proposals().is_empty());

    // Follower: with no push in flight, the pipeline is today's — one
    // solicited request at header processing, insert on the response.
    let header = leader_net.headers().into_iter().next().unwrap();
    let leader_vk = proposer_for(view1, &keys, &vss, &leader_tree);
    let (mut follower_tree, _) = steady_block_tree(&set);
    let follower_key = keys
        .iter()
        .find(|k| k.verifying_key() != leader_vk)
        .expect("a non-leader key must exist")
        .clone();
    let (mut follower, follower_net) = hotstuff_recording_at(view1, follower_key, vss.clone());
    let mut follower_app = FixedBodyApp {
        datums: vec![vec![0u8; 100]],
    };
    follower
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(header.clone()),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("processing the header must not error");
    assert_eq!(
        follower_net.body_fetch_count(),
        1,
        "over-threshold: the follower must fetch exactly as today"
    );
    assert_eq!(follower.pushed_body_buffer_len(), 0);

    let block = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([9u8; 32]),
        Data::new(vec![Datum::new(vec![0u8; 100])]),
    );
    assert_eq!(block.hash, header.block_hash);
    follower
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: header.view,
                block,
            }),
            &leader_vk,
            &mut follower_tree,
            &mut follower_app,
        )
        .expect("processing the solicited response must not error");
    assert!(
        follower_tree.contains(&header.block_hash),
        "the solicited response must insert the block, exactly as today"
    );
}

/// L3 v2 transport safety: the effective threshold is hard-capped at 64 KiB
/// regardless of the env value — at the parse AND at the decision point — so
/// a legacy full-TorusBlock body can never be pushed over the network's
/// message-size gates.
#[test]
fn body_push_hard_cap_clamps_env() {
    // Parse-level clamp: 10 MiB in the env means 64 KiB effective.
    assert_eq!(
        parse_body_push_max_bytes(Some("10485760".to_string())),
        BODY_PUSH_HARD_CAP_BYTES
    );
    assert_eq!(parse_body_push_max_bytes(Some("65535".to_string())), 65535);
    assert_eq!(
        parse_body_push_max_bytes(Some("65536".to_string())),
        BODY_PUSH_HARD_CAP_BYTES
    );
    // Decision-level clamp: even a raw u64::MAX threshold cannot push a body
    // over the cap.
    assert!(should_push_body(BODY_PUSH_HARD_CAP_BYTES, u64::MAX));
    assert!(
        !should_push_body(BODY_PUSH_HARD_CAP_BYTES + 1, u64::MAX),
        "the hard cap must bind at the decision point"
    );

    // End-to-end: a leader configured with 10 MiB pushes a 64 KiB body...
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let view1 = ViewNumber::new(1);

    let (mut tree_a, vss_a) = steady_block_tree(&set);
    let net_a = propose_as_leader(
        view1,
        &keys,
        &vss_a,
        &mut tree_a,
        vec![vec![7u8; BODY_PUSH_HARD_CAP_BYTES as usize]],
        Some(10 * 1024 * 1024),
    );
    assert_eq!(net_a.headers().len(), 1);
    assert_eq!(
        net_a.pushed_body_responses().len(),
        3,
        "an at-cap body must be pushed"
    );

    // ...but NOT a body one byte over the cap.
    let (mut tree_b, vss_b) = steady_block_tree(&set);
    let net_b = propose_as_leader(
        view1,
        &keys,
        &vss_b,
        &mut tree_b,
        vec![vec![7u8; BODY_PUSH_HARD_CAP_BYTES as usize + 1]],
        Some(10 * 1024 * 1024),
    );
    assert_eq!(net_b.headers().len(), 1);
    assert!(
        net_b.pushed_body_responses().is_empty(),
        "a body over the hard cap must never be pushed, whatever the env says"
    );
}

/// L3 v2 DoS/robustness: an unsolicited `BlockDataResponse` for a hash no
/// header ever announced is benign — parked at most (bounded FIFO, structural
/// hash gate, size gate), never inserted, never voted, never fetched-for, and
/// a flood cannot grow state beyond `PUSHED_BODY_BUFFER_CAP` entries.
#[test]
fn body_push_unsolicited_unknown_header_is_benign() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let (mut replica, net) = hotstuff_recording_at(view1, keys[0].clone(), vss.clone());
    let mut app = FixedBodyApp { datums: vec![] };
    let sender = keys[1].verifying_key();

    // 1. Structurally valid unknown body: parked, nothing else happens.
    let junk = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([77u8; 32]),
        Data::new(vec![Datum::new(vec![1, 2, 3])]),
    );
    replica
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: view1,
                block: junk.clone(),
            }),
            &sender,
            &mut tree,
            &mut app,
        )
        .expect("an unknown-header response must not error");
    assert!(
        !tree.contains(&junk.hash),
        "an unclaimed body must never be inserted"
    );
    assert_eq!(
        tree.highest_view_voted().expect("highest_view_voted"),
        None,
        "receiving a body must never imply a vote"
    );
    assert_eq!(net.body_fetch_count(), 0);
    assert_eq!(replica.pushed_body_buffer_len(), 1);

    // Redelivery of the same body: first copy wins, no growth.
    replica
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: view1,
                block: junk.clone(),
            }),
            &sender,
            &mut tree,
            &mut app,
        )
        .expect("a duplicate unknown-header response must not error");
    assert_eq!(replica.pushed_body_buffer_len(), 1);

    // 2. Structural forgery (claimed hash does not recompute from the block's
    //    own tuple): not even parked.
    let mut forged = junk.clone();
    forged.hash = CryptoHash::new([66u8; 32]);
    replica
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: view1,
                block: forged,
            }),
            &sender,
            &mut tree,
            &mut app,
        )
        .expect("a structurally forged response must not error");
    assert_eq!(
        replica.pushed_body_buffer_len(),
        1,
        "a block whose hash does not recompute must not be parked"
    );

    // 3. Oversized body (> hard cap): not parked, whatever the sender claims.
    let oversized = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([55u8; 32]),
        Data::new(vec![Datum::new(vec![
            0u8;
            BODY_PUSH_HARD_CAP_BYTES as usize + 1
        ])]),
    );
    replica
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: view1,
                block: oversized,
            }),
            &sender,
            &mut tree,
            &mut app,
        )
        .expect("an oversized unsolicited response must not error");
    assert_eq!(
        replica.pushed_body_buffer_len(),
        1,
        "a body over the hard cap must not be parked"
    );

    // 4. Flood of distinct unknown bodies: FIFO-bounded, no other state moves.
    for i in 0..(PUSHED_BODY_BUFFER_CAP + 10) {
        let flood = Block::new(
            BlockHeight::new(0),
            PhaseCertificate::genesis_pc(),
            CryptoHash::new([100 + i as u8; 32]),
            Data::new(vec![Datum::new(vec![i as u8])]),
        );
        replica
            .on_receive_msg(
                HotStuffMessage::BlockDataResponse(BlockDataResponse {
                    view: view1,
                    block: flood,
                }),
                &sender,
                &mut tree,
                &mut app,
            )
            .expect("a flood of unknown-header responses must not error");
    }
    assert_eq!(
        replica.pushed_body_buffer_len(),
        PUSHED_BODY_BUFFER_CAP,
        "the park buffer must be FIFO-bounded under flood"
    );
    assert_eq!(
        net.body_fetch_count(),
        0,
        "a flood must not trigger fetches"
    );
    assert_eq!(
        tree.highest_view_voted().expect("highest_view_voted"),
        None,
        "a flood must not move any consensus state"
    );
}
