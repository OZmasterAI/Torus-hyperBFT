//! Iteration-2 P0 consensus safety regression tests.
//!
//! Two residual holes closed after iteration 1 (lock-before-vote on the header
//! fast-path):
//!
//! * **FIX A** — the QC-collection path (`on_receive_phase_vote`) skipped
//!   `safe_pc` *entirely* when the certified block was still pending (header/body
//!   in flight), so the LOCK clause was not enforced and the unguarded full
//!   `update()` (lock advance + commit) ran. An out-of-order / conflicting-branch
//!   QC could therefore move the lock (even backwards) and commit.
//!   [`pending_qc_violating_lock_clause_is_not_applied`] pins the fix: a pending
//!   QC that violates the lock clause must NOT advance `locked_pc` nor
//!   `highest_pc`; [`pending_qc_satisfying_lock_clause_still_applies`] proves the
//!   fix keeps liveness (a lock-satisfying pending QC is still applied).
//!
//! * **FIX B** — [`pc_to_lock`](crate::block_tree::invariants::pc_to_lock) had no
//!   view-monotonicity guard, so a lower-view (or equal-view sibling) Generic QC
//!   could regress the lock backwards onto a conflicting branch, un-protecting
//!   the node. [`pc_to_lock_is_view_monotonic`] and
//!   [`update_locks_only_never_regresses_lock`] pin the fix at the single choke
//!   point.
//!
//! # RED-first (on bbfc99d, pre-fix)
//!
//! * FIX A negative test FAILS its assertions: the pending bypass treats the
//!   lock-violating QC as safe, `update()` runs, `locked_pc` regresses to the
//!   stale QC's view (and `highest_pc` advances).
//! * FIX B tests FAIL: `pc_to_lock` returns `Some(stale_qc)` for a lower/equal
//!   view, regressing the lock.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::block_tree::invariants::pc_to_lock;
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::{HotStuffMessage, PhaseVote, ProposalHeader};
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
// Minimal in-memory KVStore (mirrors the other `hotstuff/*_test.rs` files).
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
// Inert network + app; the paths under test never call the app.
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

struct NullApp;

impl App<MemKV> for NullApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("produce_block is not reached on the paths under test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("validate_block is not reached on the paths under test")
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<MemKV>,
    ) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the paths under test")
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

/// A steady-state (no in-flight VS transition) block tree over `set`.
fn steady_block_tree(set: &ValidatorSet) -> (BlockTreeSingleton<MemKV>, ValidatorSetState) {
    let vss = ValidatorSetState::new(set.clone(), set.clone(), None, true);
    let mut block_tree = BlockTreeSingleton::new(MemKV::default());
    block_tree
        .initialize(&AppStateUpdates::new(), &vss)
        .expect("block tree initialization");
    (block_tree, vss)
}

/// Manually assemble a quorum-signed Generic `PhaseCertificate`.
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

fn set_locked_pc(block_tree: &mut BlockTreeSingleton<MemKV>, pc: &PhaseCertificate) {
    let mut wb: BlockTreeWriteBatch<MemWb> = BlockTreeWriteBatch::new();
    wb.set_locked_pc(pc).expect("set_locked_pc");
    block_tree.write(wb);
}

/// Park `block` in the replica's `pending_headers` (body in flight) by feeding a
/// genesis-justified header for it — the realistic way a certified-but-not-yet
/// -in-tree block arises. Returns after the header has been processed.
fn park_pending_header(
    hotstuff: &mut HotStuff<NullNetwork>,
    block: &Block,
    view: ViewNumber,
    keys: &[SigningKey],
    vss: &ValidatorSetState,
    block_tree: &mut BlockTreeSingleton<MemKV>,
) {
    let header = header_for(block, view);
    let proposer = proposer_for(view, keys, vss, block_tree);
    let mut app = NullApp;
    hotstuff
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(header),
            &proposer,
            block_tree,
            &mut app,
        )
        .expect("processing a genesis-justified header must not error");
}

/// Feed a quorum (3 of 4) of Generic phase-votes for `block`/`view` so the local
/// collector emits a Generic PC and drives `on_receive_phase_vote`.
fn drive_generic_qc(
    hotstuff: &mut HotStuff<NullNetwork>,
    block: CryptoHash,
    view: ViewNumber,
    keys: &[SigningKey],
    block_tree: &mut BlockTreeSingleton<MemKV>,
) {
    let mut app = NullApp;
    for sk in keys.iter().take(3) {
        let signer = sk.verifying_key();
        let vote = PhaseVote::new(&Keypair::new(sk.clone()), CHAIN_ID, view, block, Phase::Generic);
        hotstuff
            .on_receive_msg(
                HotStuffMessage::PhaseVote(vote),
                &signer,
                block_tree,
                &mut app,
            )
            .expect("collecting a phase-vote must not error");
    }
}

// ---------------------------------------------------------------------------
// FIX A — QC-collection path must enforce the lock clause for pending blocks.
// ---------------------------------------------------------------------------

/// A collected QC whose certified block is PENDING (body in flight) and whose
/// view is STALER than our lock, on a block that does not extend the locked
/// block, VIOLATES the lock clause. The QC-collection path must NOT apply it:
/// `locked_pc` must not regress and `highest_pc` must not advance.
///
/// RED on bbfc99d: because the block is pending, `pc_safe` was set `true`
/// unconditionally and the full `update()` ran — `pc_to_lock(Generic)` returned
/// the stale QC, regressing `locked_pc` to view 1, and `highest_pc` advanced.
#[test]
fn pending_qc_violating_lock_clause_is_not_applied() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let mut hotstuff = hotstuff_at(view1, keys[0].clone(), vss.clone());

    // Park block B (genesis-justified, height 0) in pending_headers.
    let b = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([43u8; 32]),
        Data::new(vec![]),
    );
    park_pending_header(&mut hotstuff, &b, view1, &keys, &vss, &mut block_tree);

    // Now advance the lock to view 10 on an UNRELATED block X (not in tree, not
    // an ancestor of B): B neither out-views nor extends the locked block.
    let x = CryptoHash::new([77u8; 32]);
    let lock_pc = generic_pc(ViewNumber::new(10), x, &keys[..3], &set);
    set_locked_pc(&mut block_tree, &lock_pc);
    assert_eq!(
        block_tree.locked_pc().expect("locked_pc").view,
        ViewNumber::new(10),
        "precondition: lock advanced to view 10"
    );
    assert!(
        block_tree.highest_pc().expect("highest_pc").is_genesis_pc(),
        "precondition: highest_pc is still the genesis PC"
    );

    // Collect a Generic QC on the PENDING block B at the STALE view 1.
    drive_generic_qc(&mut hotstuff, b.hash, view1, &keys, &mut block_tree);

    // The lock must NOT regress and highest_pc must NOT advance: the stale,
    // non-extending QC on a pending block violates the lock clause.
    assert_eq!(
        block_tree.locked_pc().expect("locked_pc").view,
        ViewNumber::new(10),
        "a pending QC that violates the lock clause must NOT regress locked_pc"
    );
    assert!(
        block_tree.highest_pc().expect("highest_pc").is_genesis_pc(),
        "a pending QC that violates the lock clause must NOT advance highest_pc"
    );
}

/// Liveness control: a collected QC whose certified block is PENDING but which
/// SATISFIES the lock clause (its view out-runs the lock) is still applied —
/// FIX A must not reject legitimate pending-block QCs.
#[test]
fn pending_qc_satisfying_lock_clause_still_applies() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, vss) = steady_block_tree(&set);

    let view1 = ViewNumber::new(1);
    let mut hotstuff = hotstuff_at(view1, keys[0].clone(), vss.clone());

    // Park block B; lock stays at the genesis PC (view 0).
    let b = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([44u8; 32]),
        Data::new(vec![]),
    );
    park_pending_header(&mut hotstuff, &b, view1, &keys, &vss, &mut block_tree);
    assert!(
        block_tree.highest_pc().expect("highest_pc").is_genesis_pc(),
        "precondition: highest_pc starts at the genesis PC"
    );

    // Collect a Generic QC on the PENDING block B at view 1 (> locked view 0):
    // the lock clause is satisfied, so the QC is applied and highest_pc advances.
    drive_generic_qc(&mut hotstuff, b.hash, view1, &keys, &mut block_tree);

    assert_eq!(
        block_tree.highest_pc().expect("highest_pc").view,
        view1,
        "a pending QC that satisfies the lock clause must still advance highest_pc (liveness)"
    );
}

// ---------------------------------------------------------------------------
// FIX B — locking must be monotonic in view (defense-in-depth in pc_to_lock).
// ---------------------------------------------------------------------------

/// `pc_to_lock` must never return a candidate whose view is LOWER than (or equal
/// to, for a different block) the current locked view — locking is monotonic.
///
/// RED on bbfc99d: `pc_to_lock(Generic)` returned `Some(stale_qc)` whenever the
/// candidate differed from the locked PC, regardless of view, regressing the
/// lock onto a sibling branch.
#[test]
fn pc_to_lock_is_view_monotonic() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, _vss) = steady_block_tree(&set);

    // Lock on a Generic PC at view 10 over block X.
    let x = CryptoHash::new([10u8; 32]);
    let lock_pc = generic_pc(ViewNumber::new(10), x, &keys[..3], &set);
    set_locked_pc(&mut block_tree, &lock_pc);

    // Lower-view Generic QC on a different block Y: must NOT replace the lock.
    let y = CryptoHash::new([20u8; 32]);
    let stale = generic_pc(ViewNumber::new(5), y, &keys[..3], &set);
    assert!(
        pc_to_lock(&stale, &block_tree)
            .expect("pc_to_lock must not error")
            .is_none(),
        "a lower-view Generic QC must not regress the lock"
    );

    // Equal-view Generic QC on a different block (an equivocation sibling): must
    // NOT replace the lock (strictly-greater rule).
    let equal_view_sibling = generic_pc(ViewNumber::new(10), y, &keys[..3], &set);
    assert!(
        pc_to_lock(&equal_view_sibling, &block_tree)
            .expect("pc_to_lock must not error")
            .is_none(),
        "an equal-view sibling QC must not replace the lock"
    );

    // Higher-view Generic QC: legitimately advances the lock (liveness).
    let z = CryptoHash::new([30u8; 32]);
    let advancing = generic_pc(ViewNumber::new(11), z, &keys[..3], &set);
    let result = pc_to_lock(&advancing, &block_tree).expect("pc_to_lock must not error");
    assert!(
        result.as_ref().is_some_and(|pc| pc.view == ViewNumber::new(11) && pc.block == z),
        "a higher-view Generic QC must advance the lock"
    );
}

/// The monotonicity guard sits at the single choke point (`pc_to_lock`), so both
/// the full `update()` path and the header fast-path `update_locks_only()` path
/// inherit it. This pins the `update_locks_only` path specifically.
///
/// RED on bbfc99d: `update_locks_only` applied the lower-view QC, regressing
/// `locked_pc` from view 10 to view 5.
#[test]
fn update_locks_only_never_regresses_lock() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (mut block_tree, _vss) = steady_block_tree(&set);

    let x = CryptoHash::new([10u8; 32]);
    let lock_pc = generic_pc(ViewNumber::new(10), x, &keys[..3], &set);
    set_locked_pc(&mut block_tree, &lock_pc);

    let y = CryptoHash::new([20u8; 32]);
    let stale = generic_pc(ViewNumber::new(5), y, &keys[..3], &set);
    block_tree
        .update_locks_only(&stale, &None)
        .expect("update_locks_only must not error");

    assert_eq!(
        block_tree.locked_pc().expect("locked_pc").view,
        ViewNumber::new(10),
        "update_locks_only must not regress the lock to a lower-view QC"
    );
}
