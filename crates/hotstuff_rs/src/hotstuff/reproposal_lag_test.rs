//! Review M2 (owner s138: before the testnet genesis): the app's vote-time FRESHNESS rules (Torus:
//! no vote for a proposal whose timestamp is more than 30 s behind the local clock) apply to fresh
//! blocks only, never to a block consensus may force a leader to propose again unchanged.
//!
//! A re-proposed block is fixed by its hash, so its timestamp cannot be refreshed. If the bound
//! applied to it, a stall longer than the bound would make it unvotable forever:
//! - the validator-set-update (VSU) re-proposal of a block that already holds a Prepare /
//!   Precommit PC (`repropose_block`), including a re-proposal of the LOCKED block (the
//!   reflexive lock, s138 `fix/vsu-lock-reflexive`): the VSU window would livelock;
//! - a timeout certificate's winning high tip (case 4, case 5 after a ProposalResponse), which
//!   VALIDPROPOSAL V2 forces the next leader to propose again.
//!
//! So a replica asks [`App::check_reproposal_data`] (which may skip freshness rules) instead of
//! [`App::check_block_data`] when the block
//! - is certified: this replica's highest PC or locked PC, or the attached TC's high QC, is on it;
//! - is a TC re-proposal (correct TC(v-1), high tip wins, no NEC) which the TC's signed tips vouch
//!   for with more than the faulty power (at least one honest replica voted for it while it was
//!   fresh), counted under a set that authenticates the TC; or
//! - outside a TC re-proposal, is the block this replica last voted for (a validator-set update's
//!   Prepare vote, which sets no tip).
//!
//! A TC re-proposal is judged by its TC alone, never by the replica's own tip or vote (Codex P1-a,
//! s138): every honest replica then decides alike, so a stale tip voted by a minority cannot split
//! them into re-voters (who withhold No-Endorsements) and refusers forever.
//!
//! A TC re-proposal nobody honest vouches for is still judged fresh: a byzantine leader that
//! proposes ts = parent.ts after a stall (the M2 hole) and reports that block as its own tip in
//! its timeout vote cannot get it certified by the next honest leader. That leader, instead of
//! re-proposing a tip it would itself refuse as stale, asks for No-Endorsements (RECOVER without
//! the block request) and proposes a fresh block once an NEC forms, so the stale tip cannot wedge
//! the chain either (claude-review round 2, s131: the forged-tip wedge).
//!
//! RED-first: before the fix every case below that expects a vote or an NE request failed (the
//! data check refused every block), the controls passed.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{
    App, BlockDataCheck, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::app::AppBlockTreeView;
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, proposer_for, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
use crate::hotstuff::messages::{
    BlockDataResponse, HotStuffMessage, NEMessage, Proposal, ProposalHeader, ProposalResponse,
};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::networking::messages::{Message, ProgressMessage};
use crate::networking::network::{Network, ValidatorSetUpdateHandle};
use crate::networking::sending::SenderHandle;
use crate::pacemaker::implementation::ViewInfo;
use crate::pacemaker::messages::PacemakerMessage;
use crate::pacemaker::select_leader;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::{key_of, signed_by, unsigned_header};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, SignatureSet, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::signing::{no_endorsement_bytes, phase_vote_bytes, sign_proposal_header};
use crate::types::update_sets::ValidatorSetUpdates;
use crate::types::validator_set::{ValidatorSet, ValidatorSetState};

const CHAIN_ID: ChainID = ChainID::new(0);
/// The view under test; the TC is TC(VIEW - 1).
const VIEW: u64 = 5;

/// Every block is "stale": the fresh data check refuses it, the re-proposal check holds it
/// (`fresh` / `reproposal` override that, e.g. `Missing` for out-of-band data not local yet).
/// Counts both, and produces an empty block (an NEC proposal).
struct StaleApp {
    fresh: BlockDataCheck,
    reproposal: BlockDataCheck,
    fresh_checks: usize,
    reproposal_checks: usize,
}

impl Default for StaleApp {
    fn default() -> Self {
        StaleApp { fresh: BlockDataCheck::Invalid, reproposal: BlockDataCheck::Held, fresh_checks: 0, reproposal_checks: 0 }
    }
}

impl App<MemKV> for StaleApp {
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
        unreachable!("no block sync here")
    }
    fn check_block_data(&mut self, _: &Block, _: &AppBlockTreeView<'_, MemKV>) -> BlockDataCheck {
        self.fresh_checks += 1;
        self.fresh
    }
    fn check_reproposal_data(&mut self, _: &Block, _: &AppBlockTreeView<'_, MemKV>) -> BlockDataCheck {
        self.reproposal_checks += 1;
        self.reproposal
    }
}

/// Records every broadcast and direct send.
#[derive(Clone, Default)]
struct RecordingNetwork {
    sent: Arc<Mutex<Vec<Message>>>,
}

impl Network for RecordingNetwork {
    fn init_validator_set(&mut self, _: ValidatorSet) {}
    fn update_validator_set(&mut self, _: ValidatorSetUpdates) {}
    fn broadcast(&mut self, message: Message) {
        self.sent.lock().unwrap().push(message);
    }
    fn send(&mut self, _: VerifyingKey, message: Message) {
        self.sent.lock().unwrap().push(message);
    }
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

impl RecordingNetwork {
    fn hotstuff_msgs(&self) -> Vec<HotStuffMessage> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|m| match m {
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(msg)) => Some(msg.clone()),
                _ => None,
            })
            .collect()
    }
    fn ne_requests(&self) -> usize {
        self.hotstuff_msgs().iter().filter(|m| matches!(m, HotStuffMessage::NERequest(_))).count()
    }
    fn proposal_requests(&self) -> usize {
        self.hotstuff_msgs().iter().filter(|m| matches!(m, HotStuffMessage::ProposalRequest(_))).count()
    }
}

fn keys() -> Vec<SigningKey> {
    signing_keys(&[1, 2, 3, 4])
}

fn set() -> ValidatorSet {
    validator_set(&keys())
}

/// A quorum-signed PC of `phase` on `block` at `view` (validators 1..3).
fn pc(view: u64, block: &Block, phase: Phase) -> PhaseCertificate {
    let set = set();
    let message = phase_vote_bytes(CHAIN_ID, ViewNumber::new(view), &block.hash, phase);
    let mut signatures = SignatureSet::new(set.len());
    for key in &keys()[..3] {
        let pos = set.position(&key.verifying_key()).unwrap();
        signatures.set(pos, Some(Keypair::new(key.clone()).sign(&message)));
    }
    PhaseCertificate { chain_id: CHAIN_ID, view: ViewNumber::new(view), block: block.hash, phase, signatures }
}

/// `a`: height 0, genesis justify.
fn a() -> Block {
    Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([40; 32]), Data::new(vec![]))
}

/// `b`: height 1 on QC(2) of `a`: the stale block under test (its timestamp lives in its data).
fn b() -> Block {
    Block::new(BlockHeight::new(1), generic_pc(ViewNumber::new(2), a().hash, &keys()[..3], &set()), CryptoHash::new([41; 32]), Data::new(vec![]))
}

/// `block` as proposed at `view` by leader(`view`): a voter's signed tip (F2).
fn tip(block: &Block, view: u64) -> TipInfo {
    let view = ViewNumber::new(view);
    let leader = key_of(&select_leader(view, &set()));
    TipInfo {
        block_hash: block.hash,
        block_height: block.height,
        block_justify: block.justify.clone(),
        block_data_hash: block.data_hash,
        view,
        leader_signature: sign_proposal_header(&leader, CHAIN_ID, view, &block.hash),
    }
}

/// TC(`view`) as the collector forms it from validators 1..3's timeout votes, signer `i` reporting
/// `tips[i]` and `qc`.
fn tc(view: u64, tips: [Option<TipInfo>; 3], qc: Option<PhaseCertificate>) -> TimeoutCertificate {
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set());
    let mut formed = None;
    for (key, tip) in keys()[..3].iter().zip(tips) {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key.clone()),
            CHAIN_ID,
            ViewNumber::new(view),
            None,
            tip,
            qc.clone(),
        ) else {
            unreachable!()
        };
        formed = collector.collect(&key.verifying_key(), vote).or(formed);
    }
    formed.expect("three of four form a quorum")
}

/// TC(VIEW - 1) whose winning high tip is `b` proposed at VIEW - 1, reported by the first
/// `reporters` signers (the others report no tip).
fn tc_with_tip_reporters(reporters: usize) -> TimeoutCertificate {
    let t = || Some(tip(&b(), VIEW - 1));
    let tips = [t(), if reporters > 1 { t() } else { None }, if reporters > 2 { t() } else { None }];
    let tc = tc(VIEW - 1, tips, None);
    assert!(tc.high_tip_is_winner && tc.high_tip.as_ref().is_some_and(|tip| tip.block_hash == b().hash));
    tc
}

fn leader_of(view: u64) -> VerifyingKey {
    select_leader(ViewNumber::new(view), &set())
}

fn hotstuff_for(local: SigningKey, vss: ValidatorSetState) -> (HotStuff<RecordingNetwork>, RecordingNetwork, Receiver<Event>) {
    let net = RecordingNetwork::default();
    let (tx, events) = std::sync::mpsc::channel();
    let hotstuff = HotStuff::new(
        HotStuffConfiguration { chain_id: CHAIN_ID, keypair: Keypair::new(local) },
        ViewInfo::new(ViewNumber::new(VIEW), Instant::now() + Duration::from_secs(3600)),
        SenderHandle::new(net.clone()),
        ValidatorSetUpdateHandle::new(net.clone()),
        vss,
        Some(tx),
    );
    (hotstuff, net, events)
}

// ---------------------------------------------------------------------------
// Voter side.
// ---------------------------------------------------------------------------

/// A replica (not leader(VIEW)) at VIEW whose tree holds `a` (and `b`'s parent link).
struct Replica {
    hotstuff: HotStuff<RecordingNetwork>,
    tree: BlockTreeSingleton<MemKV>,
    events: Receiver<Event>,
    app: StaleApp,
}

fn replica() -> Replica {
    let (mut tree, vss) = steady_block_tree(&set());
    tree.insert(&a(), None, None).unwrap();
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&b().justify).unwrap();
    tree.write(wb);
    let leader = proposer_for(ViewNumber::new(VIEW), &keys(), &vss, &tree);
    let local = keys().into_iter().find(|k| k.verifying_key() != leader).unwrap();
    let (hotstuff, _net, events) = hotstuff_for(local, vss);
    Replica { hotstuff, tree, events, app: StaleApp::default() }
}

impl Replica {
    fn voted(&self) -> bool {
        self.events.try_iter().any(|event| matches!(event, Event::PhaseVote(_)))
    }

    /// `block` at VIEW from leader(VIEW), header-first (header, then body).
    fn header_path(&mut self, block: &Block, tc: Option<TimeoutCertificate>, vsu: bool) -> bool {
        let leader = leader_of(VIEW);
        let header = ProposalHeader {
            tc,
            has_validator_set_updates: vsu,
            ..unsigned_header(CHAIN_ID, block, ViewNumber::new(VIEW))
        };
        let header = signed_by(header, &leader);
        self.hotstuff.on_receive_msg(HotStuffMessage::ProposalHeader(header), &leader, &mut self.tree, &mut self.app).unwrap();
        let body = BlockDataResponse { view: ViewNumber::new(VIEW), block: block.clone() };
        self.hotstuff.on_receive_msg(HotStuffMessage::BlockDataResponse(body), &leader, &mut self.tree, &mut self.app).unwrap();
        self.voted()
    }

    /// `block` at VIEW from leader(VIEW) as a full proposal.
    fn full_path(&mut self, block: &Block, tc: Option<TimeoutCertificate>) -> bool {
        let leader = leader_of(VIEW);
        let proposal = Proposal::signed(&key_of(&leader), CHAIN_ID, ViewNumber::new(VIEW), block.clone(), tc, None);
        self.hotstuff.on_receive_msg(HotStuffMessage::Proposal(proposal), &leader, &mut self.tree, &mut self.app).unwrap();
        self.voted()
    }
}

/// Both paths, each on its own fresh replica prepared by `setup`.
fn votes(setup: impl Fn(&mut Replica), block: &Block, tc: Option<TimeoutCertificate>) -> (bool, bool) {
    let mut header = replica();
    setup(&mut header);
    let mut full = replica();
    setup(&mut full);
    (header.header_path(block, tc.clone(), false), full.full_path(block, tc))
}

/// A fresh block at VIEW: height 1 on QC(VIEW - 1) of `a` (VALIDPROPOSAL V1 without a TC).
fn fresh() -> Block {
    Block::new(BlockHeight::new(1), generic_pc(ViewNumber::new(VIEW - 1), a().hash, &keys()[..3], &set()), CryptoHash::new([42; 32]), Data::new(vec![]))
}

/// Control: a FRESH proposal of a stale block gets no vote (the M2 rule itself), and is judged by
/// the fresh check.
#[test]
fn fresh_proposal_of_a_stale_block_is_not_voted() {
    assert_eq!(votes(|_| {}, &fresh(), None), (false, false));
    let mut r = replica();
    r.header_path(&fresh(), None, false);
    assert_eq!((r.app.fresh_checks, r.app.reproposal_checks), (1, 0), "judged as fresh");
}

/// A TC re-proposal whose tip more than the faulty power of the TC's signers signed (2 of 4,
/// f = 1): an honest replica voted for it while it was fresh, so its re-proposal is voted however
/// old it is now.
#[test]
fn tc_reproposal_vouched_by_more_than_f_signed_tips_is_voted() {
    assert_eq!(votes(|_| {}, &b(), Some(tc_with_tip_reporters(2))), (true, true));
    assert_eq!(votes(|_| {}, &b(), Some(tc_with_tip_reporters(3))), (true, true));
}

/// Codex P1-a (s138 torus-adversarial): a TC re-proposal of this replica's own local tip is
/// judged by the TC alone, like every other replica's. Exempting it locally split the honest
/// replicas: the voters of a stale minority tip re-voted it in every view they or a byzantine
/// leader re-proposed it (and so withheld their No-Endorsements), the others refused it, and
/// neither a QC nor an NEC could form.
#[test]
fn tc_reproposal_of_the_own_tip_is_judged_by_the_tc() {
    let own_tip = |r: &mut Replica| r.tree.set_local_tip(&tip(&b(), VIEW - 1)).unwrap();
    assert_eq!(votes(own_tip, &b(), Some(tc_with_tip_reporters(1))), (false, false));
    assert_eq!(votes(own_tip, &b(), Some(tc_with_tip_reporters(2))), (true, true), "control: vouched");
}

/// As above for the block this replica last voted for: a TC re-proposal is judged by the TC.
#[test]
fn tc_reproposal_of_the_last_voted_block_is_judged_by_the_tc() {
    let voted = |r: &mut Replica| r.tree.set_vote_state_atomic(ViewNumber::new(VIEW - 1), b().hash).unwrap();
    assert_eq!(votes(voted, &b(), Some(tc_with_tip_reporters(1))), (false, false));
}

/// The block this replica last voted for stays exempt outside a TC re-proposal: a validator-set
/// update's Prepare vote sets no tip (no TC can vouch for it, and no NE is withheld for it), and
/// `repropose_block` proposes it again unchanged.
#[test]
fn vsu_reproposal_of_the_last_voted_block_is_voted() {
    let mut r = vsu_replica();
    r.tree.set_vote_state_atomic(ViewNumber::new(VIEW - 1), b().hash).unwrap();
    assert!(r.header_path(&b(), Some(tc(VIEW - 1, [None, None, None], None)), true));
}

/// Control (the M2 hole through the TC): a TC re-proposal only ONE signer vouches for (f = 1:
/// possibly the byzantine leader that proposed it stale and reports it as its own tip) is judged
/// fresh, and refused.
#[test]
fn tc_reproposal_vouched_by_at_most_f_is_judged_fresh() {
    assert_eq!(votes(|_| {}, &b(), Some(tc_with_tip_reporters(1))), (false, false));
}

/// A replica whose highest PC is the Prepare PC of the validator-set-updating block `u` votes
/// Prepare for `u`'s re-proposal (`repropose_block`, under a TC with no tip) however old `u` is:
/// a quorum voted for it fresh.
#[test]
fn vsu_reproposal_of_the_highest_pc_block_is_voted() {
    let mut r = vsu_replica();
    r.set_highest_pc(&pc(3, &b(), Phase::Prepare));
    assert!(r.header_path(&b(), Some(tc(VIEW - 1, [None, None, None], None)), true));
}

/// As above for a replica that did not see `u`'s PC: the re-proposal's TC carries it as its high
/// QC (V3's "B is q's block" arm).
#[test]
fn vsu_reproposal_certified_by_the_tc_high_qc_is_voted() {
    let mut r = vsu_replica();
    let prepare = pc(3, &b(), Phase::Prepare);
    let q = Some(prepare);
    let the_tc = tc(VIEW - 1, [None, None, None], q);
    assert!(!the_tc.high_tip_is_winner);
    assert!(r.header_path(&b(), Some(the_tc), true));
}

/// The lock clause aside (the reflexive lock is `fix/vsu-lock-reflexive`), a block this replica
/// is LOCKED on is certified: the freshness rules never apply to it.
#[test]
fn locked_block_is_freshness_exempt() {
    let mut r = replica();
    r.tree.insert(&b(), None, None).unwrap();
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_locked_pc(&pc(3, &b(), Phase::Precommit)).unwrap();
    r.tree.write(wb);
    assert!(HotStuff::<RecordingNetwork>::freshness_exempt(&b().hash, None, false, &r.tree).unwrap());
    assert!(!HotStuff::<RecordingNetwork>::freshness_exempt(&fresh().hash, None, false, &r.tree).unwrap(), "control: not certified, not voted");
}

/// A replica over a tree in which `b` is a validator-set-updating block (`a` <- `b`).
fn vsu_replica() -> Replica {
    let mut r = replica();
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keys()[1].verifying_key(), Power::new(2));
    r.tree.insert(&b(), None, Some(&updates)).unwrap();
    r
}

impl Replica {
    fn set_highest_pc(&mut self, pc: &PhaseCertificate) {
        let mut wb = BlockTreeWriteBatch::new();
        wb.set_highest_pc(pc).unwrap();
        self.tree.write(wb);
    }
}

// ---------------------------------------------------------------------------
// Leader side: a stale tip nobody honest vouches for is dropped through an NEC, not re-proposed.
// ---------------------------------------------------------------------------

struct Leader {
    hotstuff: HotStuff<RecordingNetwork>,
    net: RecordingNetwork,
    tree: BlockTreeSingleton<MemKV>,
    events: Receiver<Event>,
    app: StaleApp,
}

/// leader(VIEW) holding TC(VIEW - 1) = `the_tc`, with `b` in its tree or not.
fn leader(the_tc: TimeoutCertificate, holds_b: bool) -> Leader {
    let (mut tree, vss) = steady_block_tree(&set());
    tree.insert(&a(), None, None).unwrap();
    if holds_b {
        tree.insert(&b(), None, None).unwrap();
    }
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&b().justify).unwrap();
    tree.write(wb);
    tree.set_highest_tc(&the_tc).unwrap();
    let local = key_of(&proposer_for(ViewNumber::new(VIEW), &keys(), &vss, &tree));
    let (hotstuff, net, events) = hotstuff_for(local, vss);
    Leader { hotstuff, net, tree, events, app: StaleApp::default() }
}

impl Leader {
    fn enter(&mut self) {
        self.hotstuff
            .enter_view(
                ViewInfo::new(ViewNumber::new(VIEW), Instant::now() + Duration::from_secs(3600)),
                &mut self.tree,
                &mut self.app,
            )
            .unwrap();
    }

    fn proposals(&self) -> Vec<Proposal> {
        self.events
            .try_iter()
            .filter_map(|event| match event {
                Event::Propose(e) => Some(e.proposal),
                _ => None,
            })
            .collect()
    }

    fn receive(&mut self, msg: HotStuffMessage, origin: &VerifyingKey) {
        self.hotstuff.on_receive_msg(msg, origin, &mut self.tree, &mut self.app).unwrap();
    }

    /// NE messages for VIEW over `b`'s justify view from validators 1..3.
    fn receive_nes(&mut self) {
        let high_tip_qc_view = b().justify.view;
        for key in &keys()[..3] {
            let bytes = no_endorsement_bytes(CHAIN_ID, ViewNumber::new(VIEW), high_tip_qc_view);
            let ne = NEMessage {
                chain_id: CHAIN_ID,
                view: ViewNumber::new(VIEW),
                high_tip_qc_view,
                signature: Keypair::new(key.clone()).sign(&bytes),
            };
            self.receive(HotStuffMessage::NE(ne), &key.verifying_key());
        }
    }
}

/// Case 4 with a stale tip only one signer vouches for: the leader would refuse it itself, so it
/// does not re-propose it (no honest replica would vote it: a wedge while it keeps winning the
/// TCs). It asks for No-Endorsements instead, and proposes a fresh block on the NEC.
#[test]
fn leader_seeks_an_nec_instead_of_reproposing_a_stale_unvouched_tip() {
    let mut l = leader(tc_with_tip_reporters(1), true);
    l.enter();
    assert!(l.proposals().is_empty(), "the stale, unvouched tip is not re-proposed");
    assert_eq!(l.net.ne_requests(), 1, "the leader asks for No-Endorsements");
    assert_eq!(l.net.proposal_requests(), 0, "it holds the block: no ProposalRequest");
    l.receive_nes();
    let p = l.proposals();
    assert_eq!(p.len(), 1, "an NEC lets the leader propose a fresh block");
    assert!(p[0].nec.is_some() && p[0].block.hash != b().hash);
    assert!(p[0].block.justify == b().justify, "the fresh block extends the tip's justify");
}

/// Control: a vouched tip is re-proposed as before. Codex P1-a: the leader's OWN stale tip that
/// the TC does not vouch for is not (honest replicas judge it by the TC, and refuse it); the
/// leader seeks an NEC instead.
#[test]
fn leader_reproposes_a_vouched_stale_tip_but_not_its_own_unvouched_one() {
    let mut l = leader(tc_with_tip_reporters(2), true);
    l.enter();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert!(p[0].block.hash == b().hash && p[0].nec.is_none());
    assert_eq!(l.net.ne_requests(), 0);

    let mut l = leader(tc_with_tip_reporters(1), true);
    l.tree.set_local_tip(&tip(&b(), VIEW - 1)).unwrap();
    l.tree.set_vote_state_atomic(ViewNumber::new(VIEW - 1), b().hash).unwrap();
    l.enter();
    assert!(l.proposals().is_empty(), "its own stale, unvouched tip is not re-proposed");
    assert_eq!(l.net.ne_requests(), 1);
}

/// Case 5: the leader lacks the tip and recovers it through a ProposalResponse; a stale,
/// unvouched tip is not re-proposed then either, and the NEC still completes the view.
#[test]
fn recovered_stale_unvouched_tip_is_not_reproposed() {
    let mut l = leader(tc_with_tip_reporters(1), false);
    l.enter();
    assert!(l.proposals().is_empty());
    assert_eq!(l.net.ne_requests(), 1);
    let responder = keys()[0].verifying_key();
    let response = ProposalResponse { chain_id: CHAIN_ID, view: ViewNumber::new(VIEW), block: b() };
    l.receive(HotStuffMessage::ProposalResponse(response), &responder);
    assert!(l.proposals().is_empty(), "the recovered stale tip is not re-proposed");
    l.receive_nes();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert!(p[0].nec.is_some() && p[0].block.hash != b().hash);
}

// ---------------------------------------------------------------------------
// Codex torus-adversarial review of the M2 branch (s138), P1-a and P1-b.
// ---------------------------------------------------------------------------

/// TC(VIEW - 1) signed by `signers`, only `reporter` reporting `b` as its tip.
fn tc_signed_by(signers: [VerifyingKey; 3], reporter: VerifyingKey) -> TimeoutCertificate {
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(VIEW - 1), set());
    let mut formed = None;
    for signer in signers {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key_of(&signer)),
            CHAIN_ID,
            ViewNumber::new(VIEW - 1),
            None,
            (signer == reporter).then(|| tip(&b(), VIEW - 1)),
            None,
        ) else {
            unreachable!()
        };
        formed = collector.collect(&signer, vote).or(formed);
    }
    formed.expect("three of four form a quorum")
}

/// Codex P1-b: a stale tip only the byzantine leader of VIEW - 1 reports (tip power 1 = f),
/// whose out-of-band data this leader lacks: the fresh check refuses it, the re-proposal check
/// says `Missing`. Before the fix the leader re-proposed it (only `Held` counted as stale), and
/// self-voted its own loopback header from `pending_bodies` with no data check at all: an honest
/// signed tip that, with the byzantine one, vouched (> f) for the stale block in the next TC, so
/// every honest replica would then certify it. The leader seeks an NEC instead, and casts no vote.
#[test]
fn missing_data_does_not_bootstrap_a_stale_tip() {
    let byz = leader_of(VIEW - 1);
    let me = leader_of(VIEW);
    let the_tc = tc_signed_by([byz, me, leader_of(VIEW + 1)], byz);
    let mut l = leader(the_tc.clone(), true);
    assert!(!HotStuff::<RecordingNetwork>::freshness_exempt(&b().hash, Some(&the_tc), true, &l.tree).unwrap());
    l.app.reproposal = BlockDataCheck::Missing;
    l.enter();
    let proposals = l.proposals();
    // The leader's loopback copy of whatever it proposed.
    for p in &proposals {
        l.receive(HotStuffMessage::ProposalHeader(ProposalHeader::from_proposal(p, false)), &me);
    }
    assert!(l.tree.last_voted_proposal().unwrap().is_none(), "no vote for the stale tip");
    assert!(l.tree.local_tip().unwrap().is_none(), "no honest tip for the stale block");
    assert!(proposals.is_empty(), "the stale tip is not re-proposed");
    assert_eq!(l.net.ne_requests(), 1, "the leader asks for No-Endorsements");
    l.receive_nes();
    let p = l.proposals();
    assert_eq!(p.len(), 1, "an NEC lets the leader propose a fresh block");
    assert!(p[0].nec.is_some() && p[0].block.hash != b().hash);
}

/// Codex P1-b, the self-vote: a leader votes for its own TC re-proposal only once the applicable
/// data check holds (here the re-proposal check of a vouched tip), as every other voter does;
/// the `pending_bodies` shortcut is for its freshly produced blocks only.
#[test]
fn leader_self_vote_on_a_reproposal_waits_for_the_data_check() {
    let me = leader_of(VIEW);
    let mut l = leader(tc_with_tip_reporters(2), true);
    l.app.reproposal = BlockDataCheck::Missing;
    l.enter();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert!(p[0].block.hash == b().hash && p[0].nec.is_none(), "a vouched tip is re-proposed");
    l.receive(HotStuffMessage::ProposalHeader(ProposalHeader::from_proposal(&p[0], false)), &me);
    assert!(l.tree.last_voted_proposal().unwrap().is_none(), "no vote while the data is missing");
    l.app.reproposal = BlockDataCheck::Held;
    std::thread::sleep(Duration::from_millis(30));
    l.hotstuff.tick_body_vote(&mut l.tree, &mut l.app).unwrap();
    assert_eq!(l.tree.last_voted_proposal().unwrap(), Some((ViewNumber::new(VIEW), b().hash)), "voted once it holds");
}

/// Codex P1-a (the probe `review_rotating_leaders_keep_minority_tip_stuck`): `b` was proposed by
/// the byzantine leader(4) and voted by two honest replicas, leader(5) and leader(6), then a
/// partition made it stale; leader(7), honest, never voted for it. The byzantine replica forms
/// every TC from its own vote (no tip), ONE endorser's tip and the non-voter's, so no TC vouches
/// for `b` (tip power 1 = f). Before the fix the endorsers exempted `b` locally (own tip, last
/// vote): as leaders, and under the byzantine leader, they re-proposed and re-voted it in every
/// view (no QC: the non-voter refused it) and so withheld their No-Endorsements, while the
/// non-voter's NEC got 1 of 3: no progress, ever. Now every honest replica judges `b` by the TC:
/// nobody re-votes it, and once the endorsers' last vote is older than the TC's view they sign
/// NEs, so an NEC replaces `b` in the second view.
#[test]
fn rotating_leaders_recover_a_minority_stale_tip() {
    struct Node {
        vk: VerifyingKey,
        hs: HotStuff<RecordingNetwork>,
        net: RecordingNetwork,
        tree: BlockTreeSingleton<MemKV>,
        events: Receiver<Event>,
        app: StaleApp,
    }
    let byz = leader_of(4);
    let endorsers = [leader_of(5), leader_of(6)];
    let nonvoter = leader_of(7);
    assert!(endorsers.iter().all(|v| *v != byz && *v != nonvoter));
    let mut nodes: Vec<Node> = keys()
        .iter()
        .filter(|k| k.verifying_key() != byz)
        .map(|k| {
            let (mut tree, vss) = steady_block_tree(&set());
            tree.insert(&a(), None, None).unwrap();
            tree.insert(&b(), None, None).unwrap();
            let mut wb = BlockTreeWriteBatch::new();
            wb.set_highest_pc(&b().justify).unwrap();
            tree.write(wb);
            if endorsers.contains(&k.verifying_key()) {
                tree.set_vote_state_atomic(ViewNumber::new(4), b().hash).unwrap();
                tree.set_local_tip(&tip(&b(), 4)).unwrap();
            }
            let (hs, net, events) = hotstuff_for(k.clone(), vss);
            Node { vk: k.verifying_key(), hs, net, tree, events, app: StaleApp::default() }
        })
        .collect();
    let mut stale_votes = 0;
    let mut recovered_in = None;
    for view in 5..13 {
        // TC(view - 1): byzantine (no tip), the first endorser (tip b), the non-voter (no tip).
        let mut c = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view - 1), set());
        let mut formed = None;
        for signer in [byz, endorsers[0], nonvoter] {
            let reported = (signer == endorsers[0]).then(|| tip(&b(), 4));
            let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
                &Keypair::new(key_of(&signer)),
                CHAIN_ID,
                ViewNumber::new(view - 1),
                None,
                reported,
                Some(b().justify.clone()),
            ) else {
                unreachable!()
            };
            formed = c.collect(&signer, vote).or(formed);
        }
        let the_tc = formed.unwrap();
        assert!(the_tc.high_tip_is_winner && !the_tc.tip_vouched(&nodes[0].tree).unwrap());
        let leader = leader_of(view);
        for n in &mut nodes {
            n.net.sent.lock().unwrap().clear();
            n.tree.set_highest_tc(&the_tc).unwrap();
            let info = ViewInfo::new(ViewNumber::new(view), Instant::now() + Duration::from_secs(3600));
            n.hs.enter_view(info, &mut n.tree, &mut n.app).unwrap();
        }
        let mut proposals: Vec<Proposal> = nodes
            .iter()
            .flat_map(|n| n.events.try_iter())
            .filter_map(|e| if let Event::Propose(p) = e { Some(p.proposal) } else { None })
            .collect();
        if leader == byz {
            // The byzantine leader re-proposes b under the TC (and withholds its own vote).
            proposals.push(Proposal::signed(&key_of(&leader), CHAIN_ID, ViewNumber::new(view), b(), Some(the_tc.clone()), None));
        }
        for proposal in proposals {
            let h = ProposalHeader::from_proposal(&proposal, false);
            for n in &mut nodes {
                n.hs.on_receive_msg(HotStuffMessage::ProposalHeader(h.clone()), &leader, &mut n.tree, &mut n.app).unwrap();
            }
        }
        let requests: Vec<_> = nodes
            .iter()
            .flat_map(|n| n.net.hotstuff_msgs())
            .filter_map(|m| if let HotStuffMessage::NERequest(r) = m { Some(r) } else { None })
            .collect();
        for request in requests {
            for n in &mut nodes {
                n.hs.on_receive_msg(HotStuffMessage::NERequest(request.clone()), &leader, &mut n.tree, &mut n.app).unwrap();
            }
        }
        let responses: Vec<_> = nodes
            .iter()
            .flat_map(|n| {
                n.net.hotstuff_msgs().into_iter().filter_map(|m| if let HotStuffMessage::NE(ne) = m { Some((n.vk, ne)) } else { None })
            })
            .collect();
        if let Some(n) = nodes.iter_mut().find(|n| n.vk == leader) {
            for (signer, response) in responses {
                n.hs.on_receive_msg(HotStuffMessage::NE(response), &signer, &mut n.tree, &mut n.app).unwrap();
            }
        }
        for n in &nodes {
            for event in n.events.try_iter() {
                match event {
                    Event::PhaseVote(v) if v.vote.block == b().hash => stale_votes += 1,
                    Event::Propose(p) if p.proposal.nec.is_some() => {
                        assert_ne!(p.proposal.block.hash, b().hash);
                        recovered_in.get_or_insert(view);
                    }
                    _ => {}
                }
            }
        }
        if recovered_in.is_some() {
            break;
        }
    }
    assert_eq!(stale_votes, 0, "no honest replica re-votes the stale, unvouched tip");
    assert_eq!(recovered_in, Some(6), "the NEC forms once the endorsers' vote is older than the TC");
}
