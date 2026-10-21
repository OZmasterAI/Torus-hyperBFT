//! S430 regression test: discard wrong-set locally-collected PCs during
//! validator-set (VS) transitions.
//!
//! Locks in the semantics restored by commit `771b063`
//! ("fix(hotstuff): discard wrong-set locally-collected PCs during VS
//! transitions (S430)") in
//! [`crate::hotstuff::implementation`]'s `on_receive_phase_vote` handler.
//!
//! # Background
//!
//! The [`ActiveCollectorPair`](crate::types::signed_messages::ActiveCollectorPair)
//! is phase-blind. During an in-flight VS transition (i.e.,
//! `validator_set_state.update_decided() == false`) it holds *two* collectors:
//! one for the Committed Validator Set (CVS, the "new" set) and one for the
//! Previous Validator Set (PVS, the "old" set). Because votes do not carry the
//! validator set they belong to, the PVS collector can assemble a **Decide** PC
//! carrying an *old-set* quorum — but the protocol requires Decide PCs to be
//! validated against the *new* set
//! (see [`PhaseCertificate::is_correct`](crate::hotstuff::types::PhaseCertificate)
//! and `roles::is_phase_voter`).
//!
//! The S395 floor shave (`b674376`) had removed the unconditional `is_correct`
//! filter on locally-collected PCs as "redundant by construction". That filter
//! was actually load-bearing during transitions: without it the bogus old-set
//! Decide PC was acted on, prematurely flipping `update_decided` and producing a
//! `highest_pc` that peers reject (debug builds SIGABRT'd on the tripwire
//! `debug_assert!` 8/8 under load). `771b063` restored the discard for the
//! transition window only:
//!
//! ```ignore
//! if block_tree.validator_set_state()?.update_decided() {
//!     debug_assert!(new_pc.is_correct(block_tree)?, "...");   // steady state: shave kept
//! } else if !new_pc.is_correct(block_tree)? {
//!     return Ok(());                                          // transition: DISCARD
//! }
//! ```
//!
//! # What these tests assert
//!
//! * [`old_set_decide_pc_is_rejected_by_is_correct`] pins the exact predicate the
//!   fix relies on: an old-set-signed Decide PC fails `is_correct` during a
//!   transition, while a properly new-set-signed Decide PC passes (positive
//!   control, so the predicate is not vacuously false).
//! * [`transition_discards_wrong_set_locally_collected_decide_pc`] drives the
//!   real handler end-to-end: it feeds a quorum of old-set Decide votes so the
//!   PVS collector emits a bogus old-set Decide PC, and asserts the handler
//!   silently discards it (`Ok(())`, `highest_pc` unchanged). If `771b063`'s
//!   discard is reverted, the restored `debug_assert!` fires on the bogus PC and
//!   this test panics (RED) in debug builds — which is exactly the regression
//!   being guarded.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::block_tree::pluggables::{KVGet, KVStore, WriteBatch};
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::{HotStuffMessage, PhaseVote};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::networking::messages::Message;
use crate::networking::network::{Network, ValidatorSetUpdateHandle};
use crate::networking::sending::SenderHandle;
use crate::pacemaker::implementation::ViewInfo;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Power, SignatureSet, ViewNumber};
use crate::types::signed_messages::Certificate;
use crate::types::update_sets::{AppStateUpdates, ValidatorSetUpdates};
use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

// ---------------------------------------------------------------------------
// Minimal in-memory KVStore (mirrors the pattern used by the block-tree unit
// tests in `block_tree/accessors/internal.rs`).
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
// Inert pluggables. The discard path returns before touching the network or the
// app, so both mocks are deliberately no-ops / `unreachable!`.
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
        unreachable!("produce_block is not reached on the PC-discard path")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("validate_block is not reached on the PC-discard path")
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<MemKV>,
    ) -> ValidateBlockResponse {
        unreachable!("validate_block_for_sync is not reached on the PC-discard path")
    }
}

// ---------------------------------------------------------------------------
// Test fixtures.
// ---------------------------------------------------------------------------

const CHAIN_ID: ChainID = ChainID::new(0);

/// Deterministic signing keys from a fixed seed byte (mirrors the block-tree
/// unit tests). Distinct seeds guarantee distinct verifying keys.
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

/// Manually assemble a Decide `PhaseCertificate` for `block`/`view`, signed by
/// `signers`, with signatures placed at each signer's position inside `set`
/// (exactly as the [`PhaseVoteCollector`] would).
fn decide_pc(
    view: ViewNumber,
    block: CryptoHash,
    signers: &[SigningKey],
    set: &ValidatorSet,
) -> PhaseCertificate {
    let message = (CHAIN_ID, view, block, Phase::Decide).try_to_vec().unwrap();
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
        phase: Phase::Decide,
        signatures,
    }
}

/// A block tree that is initialized directly into an *in-flight VS transition*:
/// committed set = new set, previous set = old set, `update_decided == false`.
fn transition_block_tree(
    committed: &ValidatorSet,
    previous: &ValidatorSet,
) -> (BlockTreeSingleton<MemKV>, ValidatorSetState) {
    let vss = ValidatorSetState::new(
        committed.clone(),
        previous.clone(),
        Some(BlockHeight::new(1)),
        false, // update NOT decided => PVS collector is active => transition window
    );
    let mut block_tree = BlockTreeSingleton::new(MemKV::default());
    block_tree
        .initialize(&AppStateUpdates::new(), &vss)
        .expect("block tree initialization");
    (block_tree, vss)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// Predicate the fix depends on: during a VS transition, a Decide PC carrying an
/// OLD-set (previous-validator-set) quorum must fail `is_correct`, because a
/// Decide PC on a block not (yet) in the tree is validated against the committed
/// (NEW) set. A properly NEW-set-signed Decide PC is the positive control.
#[test]
fn old_set_decide_pc_is_rejected_by_is_correct() {
    let old_keys = signing_keys(&[1, 2, 3, 4]); // previous validator set
    let new_keys = signing_keys(&[101, 102, 103, 104]); // committed validator set (disjoint)
    let old_set = validator_set(&old_keys);
    let new_set = validator_set(&new_keys);

    let (block_tree, _vss) = transition_block_tree(&new_set, &old_set);

    let view = ViewNumber::new(1);
    let block = CryptoHash::new([9u8; 32]); // NOT in the block tree

    // A Decide PC assembled by the phase-blind PVS collector from an OLD-set
    // quorum: the protocol requires the NEW set, so it must be rejected.
    let old_set_decide_pc = decide_pc(view, block, &old_keys, &old_set);
    assert!(
        !old_set_decide_pc
            .is_correct(&block_tree)
            .expect("is_correct must not error"),
        "an old-set Decide PC must be rejected during an in-flight VS transition"
    );

    // Positive control: the correctly new-set-signed Decide PC passes, proving
    // the predicate is not vacuously false.
    let new_set_decide_pc = decide_pc(view, block, &new_keys, &new_set);
    assert!(
        new_set_decide_pc
            .is_correct(&block_tree)
            .expect("is_correct must not error"),
        "a correctly new-set-signed Decide PC must be accepted"
    );
}

/// End-to-end guard for `771b063`. Feeds a quorum of OLD-set Decide votes into
/// the real `on_receive_msg` handler during a VS transition. The phase-blind PVS
/// collector assembles a bogus old-set Decide PC; the handler must **silently
/// discard** it (return `Ok(())`, leave `highest_pc` untouched).
///
/// Revert guard: with `771b063` reverted, the restored
/// `debug_assert!(new_pc.is_correct(...))` fires on the bogus PC and this test
/// panics (RED) in debug builds.
#[test]
fn transition_discards_wrong_set_locally_collected_decide_pc() {
    let old_keys = signing_keys(&[1, 2, 3, 4]); // previous validator set
    let new_keys = signing_keys(&[101, 102, 103, 104]); // committed validator set (disjoint)
    let old_set = validator_set(&old_keys);
    let new_set = validator_set(&new_keys);

    let (mut block_tree, vss) = transition_block_tree(&new_set, &old_set);

    let highest_pc_before = block_tree
        .highest_pc()
        .expect("highest_pc read")
        .is_genesis_pc();
    assert!(
        highest_pc_before,
        "precondition: highest_pc starts at the genesis PC"
    );

    let view = ViewNumber::new(1);
    let local_keypair = Keypair::new(SigningKey::from_bytes(&[200u8; 32]));
    let config = HotStuffConfiguration {
        chain_id: CHAIN_ID,
        keypair: local_keypair,
    };
    let view_info = ViewInfo::new(view, Instant::now() + Duration::from_secs(3600));

    let mut hotstuff: HotStuff<NullNetwork> = HotStuff::new(
        config,
        view_info,
        SenderHandle::new(NullNetwork),
        ValidatorSetUpdateHandle::new(NullNetwork),
        vss,
        None,
    );

    let mut app = NullApp;
    let block = CryptoHash::new([9u8; 32]); // NOT in the block tree

    // OLD-set quorum: total power 4 => quorum = (4*2/3)+1 = 3. Feed 3 old-set
    // Decide votes. Only the PVS collector (old set) can collect these; on the
    // third the collector emits the bogus old-set Decide PC, driving the handler
    // straight into the S430 discard branch.
    for sk in old_keys.iter().take(3) {
        let signer = sk.verifying_key();
        let vote = PhaseVote::new(
            &Keypair::new(sk.clone()),
            CHAIN_ID,
            view,
            block,
            Phase::Decide,
        );
        let result = hotstuff.on_receive_msg(
            HotStuffMessage::PhaseVote(vote),
            &signer,
            &mut block_tree,
            &mut app,
        );
        assert!(
            result.is_ok(),
            "on_receive_msg must return Ok while discarding the wrong-set PC"
        );
    }

    // The bogus old-set Decide PC must have been discarded: highest_pc is still
    // the genesis PC (never advanced), and update_decided was never flipped.
    assert!(
        block_tree
            .highest_pc()
            .expect("highest_pc read")
            .is_genesis_pc(),
        "highest_pc must be unchanged after a wrong-set Decide PC is discarded"
    );
    assert!(
        !block_tree
            .validator_set_state()
            .expect("validator_set_state read")
            .update_decided(),
        "update_decided must not be flipped by a discarded wrong-set Decide PC"
    );
}
