//! TC phase C review (docs/plans/tc-required-and-tip-signing.md §4): the validator-set update
//! window.
//!
//! While an update is committed but not decided, `is_proposer` accepts the leader of a view in the
//! committed set (CVS) and in the previous set (PVS). F2 used to accept a tip signed by either, even
//! after the update was decided, so one view could have two valid tips: B from the CVS leader with
//! 2f+1 votes and X from the PVS leader (a byzantine voter reports X). The TC collector keeps the
//! first tip at the highest view and `summaries_are_signed` accepts any signed tip there, so TC(v)
//! could name X and drop B.
//!
//! The fix names one fresh proposer per view (`ValidatorSetState::fresh_proposer_set`): the PVS
//! leader for views up to the update view (recorded when the updating block is committed) and the
//! CVS leader after it, whether or not the update is decided (s136; the s134 rule switched to the
//! CVS leader once decided). F2, the vote on a fresh proposal, and the leader's own fresh proposals
//! all use it.
//!
//! Also here: the three other committed-set-only leader checks (NE reply, timeout blame; the justify
//! fetch target is left as a hint), and the full path's drop of a proposal for another view.
//!
//! RED-first: on the review skeleton (92105bb2) every test below except the controls and
//! `full_path_drops_a_proposal_for_another_view` fails.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{signing_keys, validator_set, MemKV};
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::{BlockDataResponse, HotStuffMessage, NERequest, Proposal, ProposalHeader};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::networking::messages::{Message, ProgressMessage};
use crate::networking::network::{Network, ValidatorSetUpdateHandle};
use crate::networking::sending::SenderHandle;
use crate::pacemaker::implementation::ViewInfo;
use crate::pacemaker::messages::PacemakerMessage;
use crate::pacemaker::select_leader;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::{key_of, signed_by, tip_signed_by, unsigned_header};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, SignatureSet, ViewNumber};
use crate::types::signed_messages::{Certificate, Collector};
use crate::types::signing::phase_vote_bytes;
use crate::types::update_sets::{AppStateUpdates, ValidatorSetUpdates};
use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

const CHAIN_ID: ChainID = ChainID::new(0);

struct ValidApp;

impl App<MemKV> for ValidApp {
    fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("replica-side test")
    }
    fn validate_block(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
    fn validate_block_for_sync(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("replica-side test")
    }
}

/// Records every direct send.
#[derive(Clone, Default)]
struct RecordingNetwork {
    sends: Arc<Mutex<Vec<(VerifyingKey, Message)>>>,
}

impl Network for RecordingNetwork {
    fn init_validator_set(&mut self, _: ValidatorSet) {}
    fn update_validator_set(&mut self, _: ValidatorSetUpdates) {}
    fn broadcast(&mut self, _: Message) {}
    fn send(&mut self, peer: VerifyingKey, message: Message) {
        self.sends.lock().unwrap().push((peer, message));
    }
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

impl RecordingNetwork {
    /// The peers an NE message was sent to.
    fn ne_recipients(&self) -> Vec<VerifyingKey> {
        self.sends
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m)| matches!(m, Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::NE(_)))))
            .map(|(peer, _)| *peer)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Fixtures: the previous set is the four seed validators 1..4; the update
// replaces 4 by 5. Validators 1, 2 and 3 are in both sets.
// ---------------------------------------------------------------------------

fn old_keys() -> Vec<SigningKey> {
    signing_keys(&[1, 2, 3, 4])
}

fn new_keys() -> Vec<SigningKey> {
    signing_keys(&[1, 2, 3, 5])
}

fn old_set() -> ValidatorSet {
    validator_set(&old_keys())
}

fn new_set() -> ValidatorSet {
    validator_set(&new_keys())
}

fn old_leader(view: u64) -> VerifyingKey {
    select_leader(ViewNumber::new(view), &old_set())
}

fn new_leader(view: u64) -> VerifyingKey {
    select_leader(ViewNumber::new(view), &new_set())
}

/// The first view from `from` whose two leaders differ.
fn split_view(from: u64) -> u64 {
    (from..from + 64).find(|v| old_leader(*v) != new_leader(*v)).expect("the two sets' leaders differ somewhere")
}

/// The update to the old set is committed (at height 0) and took over fresh proposals after
/// `update_view`; decided or not.
fn window_state(update_view: u64, decided: bool) -> ValidatorSetState {
    ValidatorSetState::new(new_set(), old_set(), Some(BlockHeight::new(0)), decided)
        .with_update_view(Some(ViewNumber::new(update_view)))
}

fn tree_of(state: &ValidatorSetState) -> BlockTreeSingleton<MemKV> {
    let mut tree = BlockTreeSingleton::new(MemKV::default());
    tree.initialize(&AppStateUpdates::new(), state).unwrap();
    tree
}

fn window_tree(update_view: u64, decided: bool) -> BlockTreeSingleton<MemKV> {
    tree_of(&window_state(update_view, decided))
}

/// A genesis-justified block distinguished by `tag`.
fn block(tag: u8) -> Block {
    Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([tag; 32]), Data::new(vec![]))
}

fn header_of(block: &Block, view: u64) -> ProposalHeader {
    unsigned_header(CHAIN_ID, block, ViewNumber::new(view))
}

/// `block` proposed at `view` with `leader`'s signature, as a tip.
fn tip_by(block: &Block, view: u64, leader: &VerifyingKey) -> TipInfo {
    tip_signed_by(header_of(block, view), leader)
}

/// TC(`view`) as the collector forms it from timeout votes of the old set's validators 1..3,
/// signer `i` reporting `tips[i]`.
fn collected_tc(view: u64, tips: &[Option<TipInfo>]) -> TimeoutCertificate {
    collected_tc_in(old_set(), view, tips, None)
}

/// TC(`view`) collected in `set` from timeout votes of validators 1..3 (in both sets), signer `i`
/// reporting `tips[i]` and `high_qc`.
fn collected_tc_in(set: ValidatorSet, view: u64, tips: &[Option<TipInfo>], high_qc: Option<PhaseCertificate>) -> TimeoutCertificate {
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set);
    let mut tc = None;
    for (key, tip) in old_keys()[..3].iter().zip(tips) {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key.clone()),
            CHAIN_ID,
            ViewNumber::new(view),
            None,
            tip.clone(),
            high_qc.clone(),
        ) else {
            unreachable!()
        };
        tc = collector.collect(&key.verifying_key(), vote).or(tc);
    }
    tc.expect("three of four form a quorum")
}

/// A replica at `view` over `state`: validator 1, 2 or 3 (in both sets), not a leader of `view`.
struct Replica {
    hotstuff: HotStuff<RecordingNetwork>,
    tree: BlockTreeSingleton<MemKV>,
    events: Receiver<Event>,
    net: RecordingNetwork,
}

fn replica(view: u64, state: ValidatorSetState, tree: BlockTreeSingleton<MemKV>) -> Replica {
    let local = signing_keys(&[1, 2, 3])
        .into_iter()
        .find(|key| key.verifying_key() != old_leader(view) && key.verifying_key() != new_leader(view))
        .expect("a validator in both sets that leads neither");
    let net = RecordingNetwork::default();
    let (tx, events) = std::sync::mpsc::channel();
    let hotstuff = HotStuff::new(
        HotStuffConfiguration { chain_id: CHAIN_ID, keypair: Keypair::new(local) },
        ViewInfo::new(ViewNumber::new(view), Instant::now() + Duration::from_secs(3600)),
        SenderHandle::new(net.clone()),
        ValidatorSetUpdateHandle::new(net.clone()),
        state,
        Some(tx),
    );
    Replica { hotstuff, tree, events, net }
}

impl Replica {
    fn voted(&self) -> bool {
        self.events.try_iter().any(|event| matches!(event, Event::PhaseVote(_)))
    }

    /// Deliver `block` at the replica's view from `origin` header-first (header, then body).
    fn header_path(&mut self, view: u64, block: &Block, origin: &VerifyingKey, tc: Option<TimeoutCertificate>) -> bool {
        let header = signed_by(ProposalHeader { tc, ..header_of(block, view) }, origin);
        self.hotstuff.on_receive_msg(HotStuffMessage::ProposalHeader(header), origin, &mut self.tree, &mut ValidApp).unwrap();
        let body = BlockDataResponse { view: ViewNumber::new(view), block: block.clone() };
        self.hotstuff.on_receive_msg(HotStuffMessage::BlockDataResponse(body), origin, &mut self.tree, &mut ValidApp).unwrap();
        self.voted()
    }

    /// Deliver `block` at `view` from `origin` as a full proposal.
    fn full_path(&mut self, view: u64, block: &Block, origin: &VerifyingKey, tc: Option<TimeoutCertificate>) -> bool {
        let proposal = Proposal::signed(&key_of(origin), CHAIN_ID, ViewNumber::new(view), block.clone(), tc, None);
        self.hotstuff.on_receive_msg(HotStuffMessage::Proposal(proposal), origin, &mut self.tree, &mut ValidApp).unwrap();
        self.voted()
    }
}

/// Whether a window replica (update view `update_view`, undecided) at `view` votes for `block`
/// from `origin` on the header path and on the full path.
fn window_votes(view: u64, update_view: u64, block: &Block, origin: &VerifyingKey, tc: Option<TimeoutCertificate>) -> (bool, bool) {
    window_votes_as(false, view, update_view, block, origin, tc)
}

/// [`window_votes`] for a replica that has (`decided`) or has not applied the update's Decide PC.
fn window_votes_as(
    decided: bool,
    view: u64,
    update_view: u64,
    block: &Block,
    origin: &VerifyingKey,
    tc: Option<TimeoutCertificate>,
) -> (bool, bool) {
    let mut header = replica(view, window_state(update_view, decided), window_tree(update_view, decided));
    let mut full = replica(view, window_state(update_view, decided), window_tree(update_view, decided));
    (header.header_path(view, block, origin, tc.clone()), full.full_path(view, block, origin, tc))
}

// ---------------------------------------------------------------------------
// F2: one tip leader per view.
// ---------------------------------------------------------------------------

#[test]
fn window_tip_up_to_the_update_view_is_the_previous_sets_leaders() {
    let view = split_view(1);
    let tree = window_tree(view, false);
    let b = block(80);
    assert!(tip_by(&b, view, &old_leader(view)).is_correct(CHAIN_ID, &tree).unwrap(), "previous-set leader, view == update view");
    assert!(
        !tip_by(&b, view, &new_leader(view)).is_correct(CHAIN_ID, &tree).unwrap(),
        "committed-set leader for a view before the update took over"
    );
}

#[test]
fn window_tip_after_the_update_view_is_the_committed_sets_leaders() {
    let update_view = 1;
    let view = split_view(update_view + 1);
    let tree = window_tree(update_view, false);
    let b = block(81);
    assert!(tip_by(&b, view, &new_leader(view)).is_correct(CHAIN_ID, &tree).unwrap(), "committed-set leader after the update view");
    assert!(
        !tip_by(&b, view, &old_leader(view)).is_correct(CHAIN_ID, &tree).unwrap(),
        "previous-set leader after the update view (the second tip X)"
    );
}

/// Whether the update is decided does not change a view's fresh proposer (s136): up to the update
/// view it stays the previous set's leader, after it the committed set's, so replicas that already
/// applied the Decide PC and replicas that have not judge every tip alike.
#[test]
fn decided_update_keeps_one_fresh_proposer_per_view() {
    let early = split_view(1);
    let late = split_view(early + 1);
    let b = block(82);
    for decided in [false, true] {
        let tree = window_tree(early, decided);
        assert!(tip_by(&b, early, &old_leader(early)).is_correct(CHAIN_ID, &tree).unwrap(), "decided={decided}: previous-set leader up to the update view");
        assert!(
            !tip_by(&b, early, &new_leader(early)).is_correct(CHAIN_ID, &tree).unwrap(),
            "decided={decided}: committed-set leader up to the update view"
        );
        assert!(tip_by(&b, late, &new_leader(late)).is_correct(CHAIN_ID, &tree).unwrap(), "decided={decided}: committed-set leader after the update view");
        assert!(
            !tip_by(&b, late, &old_leader(late)).is_correct(CHAIN_ID, &tree).unwrap(),
            "decided={decided}: previous-set leader after the update view"
        );
    }
}

/// No update ever committed (no update view): every view's fresh proposer is the committed set's
/// leader, decided or not.
#[test]
fn without_an_update_view_the_committed_sets_leader_proposes() {
    let view = split_view(1);
    let b = block(95);
    for decided in [false, true] {
        let tree = tree_of(&ValidatorSetState::new(new_set(), old_set(), None, decided));
        assert!(tip_by(&b, view, &new_leader(view)).is_correct(CHAIN_ID, &tree).unwrap(), "decided={decided}: committed-set leader");
        assert!(!tip_by(&b, view, &old_leader(view)).is_correct(CHAIN_ID, &tree).unwrap(), "decided={decided}: previous-set leader");
    }
}

// ---------------------------------------------------------------------------
// The B-vs-X tie at the TC's view.
// ---------------------------------------------------------------------------

/// B (from the committed set's leader of v, voted by 2f+1) and X (signed by the previous set's
/// leader of v, reported by one voter) tie at TC(v)'s view. The collector keeps X (first at the
/// highest view); such a TC is not correct, so the only TC(v) a replica accepts names B, and only
/// B's re-proposal is voted.
#[test]
fn window_tie_at_the_tc_view_resolves_to_the_voted_block() {
    let update_view = 1;
    let v = split_view(update_view + 1);
    let tree = window_tree(update_view, false);
    let b = block(83);
    let x = block(84);
    let tip_b = tip_by(&b, v, &new_leader(v));
    let tip_x = tip_by(&x, v, &old_leader(v));

    let names_x = collected_tc(v, &[Some(tip_x.clone()), Some(tip_b.clone()), Some(tip_b.clone())]);
    assert!(names_x.high_tip.as_ref() == Some(&tip_x), "fixture: the collector keeps the first tip at view v");
    assert!(!names_x.is_correct(&tree).unwrap(), "a TC naming X is not correct");

    let names_b = collected_tc(v, &[Some(tip_b.clone()), Some(tip_b.clone()), Some(tip_b)]);
    assert!(names_b.is_correct(&tree).unwrap(), "control: the TC naming B is correct");

    let next = v + 1;
    for origin in [new_leader(next), old_leader(next)] {
        assert_eq!(window_votes(next, update_view, &x, &origin, Some(names_x.clone())), (false, false), "re-proposal of X");
        assert_eq!(window_votes(next, update_view, &b, &origin, Some(names_b.clone())), (true, true), "re-proposal of B");
    }
}

// ---------------------------------------------------------------------------
// Voting on fresh proposals: only from the view's fresh proposer.
// ---------------------------------------------------------------------------

#[test]
fn window_fresh_proposal_after_the_update_view_is_voted_only_from_the_committed_sets_leader() {
    let update_view = 1;
    let view = split_view(update_view + 1);
    let b = block(85);
    assert_eq!(window_votes(view, update_view, &b, &new_leader(view), None), (true, true), "committed-set leader");
    assert_eq!(window_votes(view, update_view, &b, &old_leader(view), None), (false, false), "previous-set leader");
}

#[test]
fn window_fresh_proposal_up_to_the_update_view_is_voted_only_from_the_previous_sets_leader() {
    let view = split_view(1);
    let b = block(86);
    assert_eq!(window_votes(view, view, &b, &old_leader(view), None), (true, true), "previous-set leader");
    assert_eq!(window_votes(view, view, &b, &new_leader(view), None), (false, false), "committed-set leader");
}

/// The first fresh block after an update justifies on the updating block's Decide PC. A replica
/// that has not committed that block yet (its state still names the old set) must judge the
/// proposer under the set the Decide PC brings in: it does not vote for the old set's leader.
#[test]
fn fresh_proposal_on_a_decide_pc_is_judged_under_the_update_it_decides() {
    let view = split_view(3);
    let old = old_set();
    let steady = ValidatorSetState::new(old.clone(), old.clone(), None, true);
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(new_keys()[3].verifying_key(), Power::new(1));
    updates.delete(old_keys()[3].verifying_key());
    let updating = block(87);
    let decide = {
        let pc_view = ViewNumber::new(view - 1);
        let message = phase_vote_bytes(CHAIN_ID, pc_view, &updating.hash, Phase::Decide);
        let mut signatures = SignatureSet::new(old.len());
        for key in &old_keys()[..3] {
            signatures.set(old.position(&key.verifying_key()).unwrap(), Some(Keypair::new(key.clone()).sign(&message)));
        }
        PhaseCertificate { chain_id: CHAIN_ID, view: pc_view, block: updating.hash, phase: Phase::Decide, signatures }
    };
    let fresh = Block::new(BlockHeight::new(1), decide, CryptoHash::new([88; 32]), Data::new(vec![]));
    let stale_replica = || {
        let mut tree = tree_of(&steady);
        tree.insert(&updating, None, Some(&updates)).unwrap();
        replica(view, steady.clone(), tree)
    };
    assert!(!stale_replica().header_path(view, &fresh, &old_leader(view), None), "header path, old set's leader");
    assert!(!stale_replica().full_path(view, &fresh, &old_leader(view), None), "full path, old set's leader");
}

// ---------------------------------------------------------------------------
// The update view is recorded when the updating block is committed.
// ---------------------------------------------------------------------------

fn pc(view: u64, block: &Block, phase: Phase) -> PhaseCertificate {
    PhaseCertificate {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block: block.hash,
        phase,
        signatures: SignatureSet::new(old_set().len()),
    }
}

/// A steady old-set tree holding the updating block U (height 0) and `children` on top of it.
fn tree_with_update(children: &[&Block]) -> (BlockTreeSingleton<MemKV>, Block) {
    let old = old_set();
    let mut tree = tree_of(&ValidatorSetState::new(old.clone(), old, None, true));
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(new_keys()[3].verifying_key(), Power::new(1));
    let updating = block(90);
    tree.insert(&updating, None, Some(&updates)).unwrap();
    for child in children {
        tree.insert(child, None, None).unwrap();
    }
    (tree, updating)
}

#[test]
fn update_view_is_the_commit_pc_view_when_a_commit_pc_commits_the_update() {
    let (mut tree, updating) = tree_with_update(&[]);
    tree.update(&pc(7, &updating, Phase::Commit), &None).unwrap();
    let state = tree.validator_set_state().unwrap();
    assert!(!state.update_decided(), "fixture: committed, not decided");
    assert_eq!(state.update_view(), Some(ViewNumber::new(7)));
}

#[test]
fn update_view_is_the_decide_pc_view_when_a_descendant_commits_the_update() {
    let (_, updating) = tree_with_update(&[]);
    let child = Block::new(BlockHeight::new(1), pc(8, &updating, Phase::Decide), CryptoHash::new([91; 32]), Data::new(vec![]));
    let grandchild = Block::new(BlockHeight::new(2), pc(9, &child, Phase::Generic), CryptoHash::new([92; 32]), Data::new(vec![]));

    // A Generic PC on the child commits U (2-chain: views 8, 9).
    let (mut tree, _) = tree_with_update(&[&child]);
    tree.update(&pc(9, &child, Phase::Generic), &None).unwrap();
    assert_eq!(tree.validator_set_state().unwrap().update_view(), Some(ViewNumber::new(8)), "U is the top committed block");

    // A Generic PC on the grandchild commits the child and U below it.
    let (mut tree, _) = tree_with_update(&[&child, &grandchild]);
    tree.update(&pc(10, &grandchild, Phase::Generic), &None).unwrap();
    assert_eq!(tree.validator_set_state().unwrap().update_view(), Some(ViewNumber::new(8)), "U below the top committed block");
}

// ---------------------------------------------------------------------------
// Coordinator scope (s134): other committed-set-only leader checks.
// ---------------------------------------------------------------------------

/// NE reply: the NE goes to the leader recovering the view (any proposer of it, as
/// `is_proposer`), not to the committed set's leader.
#[test]
fn ne_reply_goes_to_the_recovering_proposer() {
    let update_view = 1;
    let view = split_view(update_view + 1);
    let tc = collected_tc(view - 1, &[Some(tip_by(&block(93), view - 1, &new_leader(view - 1))), None, None]);
    let request = NERequest { chain_id: CHAIN_ID, view: ViewNumber::new(view), tc };
    for origin in [old_leader(view), new_leader(view)] {
        let mut r = replica(view, window_state(update_view, false), window_tree(update_view, false));
        r.hotstuff.on_receive_msg(HotStuffMessage::NERequest(request.clone()), &origin, &mut r.tree, &mut ValidApp).unwrap();
        assert_eq!(r.net.ne_recipients(), vec![origin], "NE to the requesting proposer");
    }
    let outsider = signing_keys(&[1, 2, 3, 4, 5])
        .into_iter()
        .map(|key| key.verifying_key())
        .find(|vk| *vk != old_leader(view) && *vk != new_leader(view))
        .unwrap();
    let mut r = replica(view, window_state(update_view, false), window_tree(update_view, false));
    r.hotstuff.on_receive_msg(HotStuffMessage::NERequest(request), &outsider, &mut r.tree, &mut ValidApp).unwrap();
    assert!(r.net.ne_recipients().is_empty(), "no NE for a requester that does not lead the view");
}

// ---------------------------------------------------------------------------
// Full path: a proposal for another view is dropped (implementation.rs F1 check).
// ---------------------------------------------------------------------------

/// leader(v) sends a correctly signed full proposal for v + 1 while the replica is at v. It must
/// not be voted on in v (votes are cast for the replica's view).
#[test]
fn full_path_drops_a_proposal_for_another_view() {
    let old = old_set();
    let steady = ValidatorSetState::new(old.clone(), old, None, true);
    let view = 5;
    let leader = old_leader(view);
    let b = block(94);
    let mut r = replica(view, steady.clone(), tree_of(&steady));
    let proposal = Proposal::signed(&key_of(&leader), CHAIN_ID, ViewNumber::new(view + 1), b.clone(), None, None);
    r.hotstuff.on_receive_msg(HotStuffMessage::Proposal(proposal), &leader, &mut r.tree, &mut ValidApp).unwrap();
    assert!(!r.voted(), "proposal for view {} at view {view}", view + 1);
    assert!(r.tree.highest_view_voted().unwrap().is_none(), "no vote recorded");
    assert!(r.full_path(view, &b, &leader, None), "control: the same block proposed for view {view} is voted");
}

// ---------------------------------------------------------------------------
// s136 ([claude-review] P3): a replica that already applied the update's Decide PC and one that
// has not must judge a pre-update tip alike. VSU proposals and nudges do not set the tip, so every
// honest tip in the window was signed by the previous set's leader at a view up to the update
// view. Under the decided-ness rule a decided replica checked it against the committed set's
// leader: it rejected every TC naming that tip and every proposal carrying such a TC, and a case-3
// leader, decided by applying the TC's high QC (the Decide PC), dropped its own header.
//
// RED-first: on c300a015 each test in this section fails.
// ---------------------------------------------------------------------------

/// A quorum PC of `phase` on `block` at `view`, signed in `set` by validators 1..3.
fn signed_pc(view: u64, block: &Block, phase: Phase, set: &ValidatorSet) -> PhaseCertificate {
    let pc_view = ViewNumber::new(view);
    let message = phase_vote_bytes(CHAIN_ID, pc_view, &block.hash, phase);
    let mut signatures = SignatureSet::new(set.len());
    for key in &old_keys()[..3] {
        signatures.set(set.position(&key.verifying_key()).unwrap(), Some(Keypair::new(key.clone()).sign(&message)));
    }
    PhaseCertificate { chain_id: CHAIN_ID, view: pc_view, block: block.hash, phase, signatures }
}

/// TC(v) (v after the update view) whose high tip T was signed by the previous set's leader at the
/// update view: correct for a decided and an undecided replica alike, and T's re-proposal in v + 1
/// is voted by both on both paths. Control: the same TC without the tip.
#[test]
fn decided_and_undecided_replicas_accept_a_tc_naming_a_pre_update_tip() {
    let u = split_view(1);
    let v = u + 1;
    let next = v + 1;
    let t = block(98);
    let tip = tip_by(&t, u, &old_leader(u));
    let names_t = collected_tc_in(new_set(), v, &[Some(tip.clone()), Some(tip.clone()), Some(tip)], None);
    let no_tip = collected_tc_in(new_set(), v, &[None, None, None], None);
    for decided in [false, true] {
        let tree = window_tree(u, decided);
        assert!(no_tip.is_correct(&tree).unwrap(), "control, decided={decided}: the TC without a tip is correct");
        assert!(names_t.is_correct(&tree).unwrap(), "decided={decided}: the TC naming the pre-update tip is correct");
        assert_eq!(
            window_votes_as(decided, next, u, &t, &new_leader(next), Some(names_t.clone())),
            (true, true),
            "decided={decided}: re-proposal of the pre-update tip (header path, full path)"
        );
    }
}

// The pacemaker of a replica that has applied the Decide PC (timeout votes, blame):
// `pacemaker/prev_view_collector_test.rs` (`window_decided_*`).

/// Produces empty blocks (the leader below); validates everything.
struct LeaderApp;

impl App<MemKV> for LeaderApp {
    fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        ProduceBlockResponse {
            data: Data::new(vec![]),
            data_hash: CryptoHash::new([99; 32]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
    fn validate_block_for_sync(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("leader-side test")
    }
}

/// Case 3 across the decision: the update U is committed by its Commit PC (the update view u) but
/// not decided; TC(next - 1)'s high QC is U's Decide PC and its high tip the pre-update tip T. The
/// committed set's leader of `next` applies the Decide PC (now decided), proposes on it with the TC
/// and must vote for its own header.
#[test]
fn case3_leader_decided_by_the_tc_high_qc_votes_for_its_own_proposal() {
    let t_view = split_view(1);
    let u = t_view + 1;
    let d = u + 1;
    let next = split_view(d + 2);

    let updating = block(96);
    let committed_update_tree = || {
        let old = old_set();
        let mut tree = tree_of(&ValidatorSetState::new(old.clone(), old, None, true));
        let mut updates = ValidatorSetUpdates::new();
        updates.insert(new_keys()[3].verifying_key(), Power::new(1));
        updates.delete(old_keys()[3].verifying_key());
        tree.insert(&updating, None, Some(&updates)).unwrap();
        tree.update(&pc(u, &updating, Phase::Commit), &None).unwrap();
        tree
    };
    let mut tree = committed_update_tree();
    let state = tree.validator_set_state().unwrap();
    assert!(!state.update_decided(), "fixture: committed, not decided");
    assert_eq!(state.update_view(), Some(ViewNumber::new(u)), "fixture: update view");
    assert!(state.committed_validator_set() == &new_set(), "fixture: the committed set is the new set");

    let decide = signed_pc(d, &updating, Phase::Decide, &new_set());
    assert!(decide.is_correct(&tree).unwrap(), "fixture: the Decide PC is correct");
    let tip = tip_by(&block(97), t_view, &old_leader(t_view));
    let tc = collected_tc_in(new_set(), next - 1, &[Some(tip.clone()), Some(tip.clone()), Some(tip)], Some(decide.clone()));
    assert!(!tc.high_tip_is_winner, "fixture: case 3 (the high QC wins)");
    tree.set_highest_tc(&tc).unwrap();

    let leader = new_leader(next);
    let (tx, events) = std::sync::mpsc::channel();
    let net = RecordingNetwork::default();
    let mut hotstuff = HotStuff::new(
        HotStuffConfiguration { chain_id: CHAIN_ID, keypair: Keypair::new(key_of(&leader)) },
        ViewInfo::new(ViewNumber::new(next), Instant::now() + Duration::from_secs(3600)),
        SenderHandle::new(net.clone()),
        ValidatorSetUpdateHandle::new(net),
        state,
        Some(tx),
    );
    hotstuff
        .enter_view(ViewInfo::new(ViewNumber::new(next), Instant::now() + Duration::from_secs(3600)), &mut tree, &mut LeaderApp)
        .unwrap();
    let proposal = events
        .try_iter()
        .find_map(|event| match event {
            Event::Propose(e) => Some(e.proposal),
            _ => None,
        })
        .expect("the leader proposes in case 3");
    assert!(proposal.block.justify == decide, "case 3 extends the Decide PC");
    assert!(proposal.tc.as_ref().is_some_and(|attached| attached.view == tc.view), "case 3 attaches the TC");
    assert!(tree.validator_set_state().unwrap().update_decided(), "fixture: the leader applied the Decide PC");

    let header = ProposalHeader::from_proposal(&proposal, false);
    hotstuff.on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut tree, &mut LeaderApp).unwrap();
    let body = BlockDataResponse { view: ViewNumber::new(next), block: proposal.block.clone() };
    hotstuff.on_receive_msg(HotStuffMessage::BlockDataResponse(body), &leader, &mut tree, &mut LeaderApp).unwrap();
    assert!(
        events.try_iter().any(|event| matches!(event, Event::PhaseVote(_))),
        "the case-3 leader votes for its own proposal"
    );

    // Its peers judge that proposal alike, decided or not.
    for decided in [false, true] {
        let mut peer_tree = committed_update_tree();
        if decided {
            peer_tree.advance_highest_pc_from_remote(&decide, &None).unwrap();
        }
        let peer_state = peer_tree.validator_set_state().unwrap();
        assert_eq!(peer_state.update_decided(), decided, "fixture: peer decided={decided}");
        let mut r = replica(next, peer_state, peer_tree);
        let voted = r.header_path(next, &proposal.block, &leader, proposal.tc.clone());
        assert!(voted, "decided={decided}: a peer votes for the case-3 proposal on the header path");
    }
}
