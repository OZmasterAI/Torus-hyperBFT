//! Phase A of docs/plans/tc-required-and-tip-signing.md: leader rules L1-L4.
//!
//! On entering view v as leader:
//! - L1: `highest_pc.view == v - 1` (or the genesis PC): propose at once, no TC.
//! - L2: else, with `highest_tc.view == v - 1`: propose through the TC and
//!   attach it in every case; in case 3 (high QC wins) first apply
//!   `tc.high_qc`, deferring while its block is missing.
//! - L3: else wait (`awaiting_tc`), re-checked through `has_deferred_proposal`.
//! - L4: case 5 (high tip wins, block missing) waits for a ProposalResponse or
//!   an NEC; it never falls through to a fresh proposal. Validator-set-update
//!   re-proposals go through L2/L3.
//!
//! RED-first: before phase A the leader proposed at once without a TC in the
//! L3, case-3, case-5 and VSU cases (and case 5 proposed twice in a view).

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use borsh::BorshSerialize;

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree,
    validator_set, MemKV, NullNetwork,
};
use crate::hotstuff::implementation::HotStuff;
use crate::hotstuff::messages::{HotStuffMessage, Proposal, ProposalResponse};
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::pacemaker::implementation::ViewInfo;
use crate::pacemaker::messages::PacemakerMessage;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, SignatureSet, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);
const VIEW: u64 = 5;

/// Produces empty blocks; validates everything.
struct LeaderApp;

impl App<MemKV> for LeaderApp {
    fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        ProduceBlockResponse {
            data: Data::new(vec![]),
            data_hash: CryptoHash::new([9; 32]),
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

fn keys() -> Vec<SigningKey> {
    signing_keys(&[1, 2, 3, 4])
}

fn set() -> ValidatorSet {
    validator_set(&keys())
}

/// `g`: height 0, genesis justify.
fn g() -> Block {
    Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([50; 32]), Data::new(vec![]))
}

fn qc(view: u64, block: &Block) -> PhaseCertificate {
    generic_pc(ViewNumber::new(view), block.hash, &keys()[..3], &set())
}

/// A quorum-signed PC of `phase` (as the collector forms it).
fn pc_with_phase(view: u64, block: &Block, phase: Phase) -> PhaseCertificate {
    let set = set();
    let message = (CHAIN_ID, ViewNumber::new(view), block.hash, phase).try_to_vec().unwrap();
    let mut signatures = SignatureSet::new(set.len());
    for key in &keys()[..3] {
        let pos = set.position(&key.verifying_key()).unwrap();
        signatures.set(pos, Some(Keypair::new(key.clone()).sign(&message)));
    }
    PhaseCertificate { chain_id: CHAIN_ID, view: ViewNumber::new(view), block: block.hash, phase, signatures }
}

/// `h`: height 1, justified by QC(2) on `g`.
fn h() -> Block {
    Block::new(BlockHeight::new(1), qc(2, &g()), CryptoHash::new([51; 32]), Data::new(vec![]))
}

fn tip(block: &Block, view: u64) -> TipInfo {
    TipInfo {
        block_hash: block.hash,
        block_height: block.height,
        block_justify: block.justify.clone(),
        block_data_hash: block.data_hash,
        view: ViewNumber::new(view),
    }
}

/// TC for `view` from three signers that all report `tip` and `qc`.
fn tc(view: u64, tip: Option<TipInfo>, qc: Option<PhaseCertificate>) -> TimeoutCertificate {
    let keys = keys();
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set());
    let mut formed = None;
    for key in &keys[..3] {
        let PacemakerMessage::TimeoutVote(vote) = PacemakerMessage::timeout_vote(
            &Keypair::new(key.clone()),
            CHAIN_ID,
            ViewNumber::new(view),
            None,
            tip.clone(),
            qc.clone(),
        ) else {
            unreachable!()
        };
        formed = collector.collect(&key.verifying_key(), vote).or(formed);
    }
    formed.expect("three of four form a quorum")
}

struct Leader {
    hotstuff: HotStuff<NullNetwork>,
    tree: BlockTreeSingleton<MemKV>,
    events: Receiver<Event>,
}

/// leader(VIEW) over a tree holding `blocks`, with `highest_pc` and
/// `highest_tc` as given. Not yet entered VIEW.
fn leader(blocks: &[Block], highest_pc: PhaseCertificate, highest_tc: Option<TimeoutCertificate>) -> Leader {
    let keys = keys();
    let (mut tree, vss) = steady_block_tree(&set());
    for block in blocks {
        tree.insert(block, None, None).unwrap();
    }
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&highest_pc).unwrap();
    tree.write(wb);
    if let Some(tc) = highest_tc {
        tree.set_highest_tc(&tc).unwrap();
    }
    let view = ViewNumber::new(VIEW);
    let leader_vk = proposer_for(view, &keys, &vss, &tree);
    let key = keys.iter().find(|k| k.verifying_key() == leader_vk).unwrap().clone();
    let (hotstuff, events) = hotstuff_at_with_events(view, key, vss);
    Leader { hotstuff, tree, events }
}

impl Leader {
    /// Enter VIEW (the first call), or retry a deferred / awaiting-TC
    /// proposal as the algorithm loop does (later calls).
    fn enter(&mut self) {
        self.hotstuff
            .enter_view(
                ViewInfo::new(ViewNumber::new(VIEW), Instant::now() + Duration::from_secs(3600)),
                &mut self.tree,
                &mut LeaderApp,
            )
            .unwrap();
    }

    /// The loop's retry: only while the leader says it has one pending.
    fn retry(&mut self) {
        if self.hotstuff.has_deferred_proposal() {
            self.enter();
        }
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

    fn set_highest_pc(&mut self, pc: &PhaseCertificate) {
        let mut wb = BlockTreeWriteBatch::new();
        wb.set_highest_pc(pc).unwrap();
        self.tree.write(wb);
    }
}

fn tc_view(p: &Proposal) -> Option<u64> {
    p.tc.as_ref().map(|tc| tc.view.int())
}

// L1 -------------------------------------------------------------------------

#[test]
fn l1_previous_view_qc_proposes_at_once_without_a_tc() {
    let mut l = leader(&[g(), h()], qc(VIEW - 1, &h()), None);
    l.enter();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].block.justify.view.int(), VIEW - 1);
    assert!(p[0].tc.is_none());
    assert!(!l.hotstuff.has_deferred_proposal());
}

// L3 -------------------------------------------------------------------------

#[test]
fn l3_without_a_previous_view_qc_or_tc_waits() {
    let mut l = leader(&[g(), h()], qc(3, &h()), None);
    l.enter();
    assert!(l.proposals().is_empty(), "must not propose without QC(v-1) or TC(v-1)");
    assert!(l.hotstuff.has_deferred_proposal(), "awaiting_tc is retried by the loop");
    l.retry();
    assert!(l.proposals().is_empty(), "still nothing to propose on");
}

#[test]
fn l3_proposes_with_the_tc_as_soon_as_it_arrives() {
    let mut l = leader(&[g(), h()], qc(3, &h()), None);
    l.enter();
    assert!(l.proposals().is_empty());
    l.tree.set_highest_tc(&tc(VIEW - 1, None, None)).unwrap();
    l.retry();
    let p = l.proposals();
    assert_eq!(p.len(), 1, "proposes on the retry that sees TC(v-1)");
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1));
    assert!(p[0].block.justify == qc(3, &h()));
    assert!(!l.hotstuff.has_deferred_proposal());
}

#[test]
fn l3_proposes_without_a_tc_once_the_previous_view_qc_arrives() {
    let mut l = leader(&[g(), h()], qc(3, &h()), None);
    l.enter();
    assert!(l.proposals().is_empty());
    l.set_highest_pc(&qc(VIEW - 1, &h()));
    l.retry();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].block.justify.view.int(), VIEW - 1);
    assert!(p[0].tc.is_none());
}

// L2 case 3 ------------------------------------------------------------------

#[test]
fn case3_applies_the_tc_high_qc_and_attaches_the_tc() {
    // Own highest PC: QC(2) on g. The TC's signers report QC(3) on h, which
    // wins (no tips).
    let the_tc = tc(VIEW - 1, None, Some(qc(3, &h())));
    assert!(!the_tc.high_tip_is_winner);
    let mut l = leader(&[g(), h()], qc(2, &g()), Some(the_tc));
    l.enter();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1), "case 3 must attach the TC");
    assert!(p[0].block.justify == qc(3, &h()), "case 3 extends tc.high_qc");
    assert!(l.tree.highest_pc().unwrap() == qc(3, &h()), "tc.high_qc applied");
}

#[test]
fn case3_with_the_high_qc_block_missing_defers_then_attaches_the_tc() {
    let the_tc = tc(VIEW - 1, None, Some(qc(3, &h())));
    let mut l = leader(&[g()], qc(2, &g()), Some(the_tc));
    l.enter();
    assert!(l.proposals().is_empty(), "h is missing: defer and fetch it");
    assert!(l.hotstuff.has_deferred_proposal());
    l.tree.insert(&h(), None, None).unwrap();
    l.retry();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1), "the retried proposal keeps the TC");
    assert!(p[0].block.justify == qc(3, &h()));
}

/// A TC whose high QC is not a correct certificate (only its view and block
/// are signed by the TC) is not applied; the leader still attaches the TC.
#[test]
fn case3_does_not_apply_an_incorrect_high_qc() {
    let mut forged = qc(3, &h());
    forged.signatures = SignatureSet::new(4);
    let the_tc = tc(VIEW - 1, None, Some(forged));
    let mut l = leader(&[g(), h()], qc(2, &g()), Some(the_tc));
    l.enter();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1));
    assert!(p[0].block.justify == qc(2, &g()), "the forged high QC is ignored");
}

// L2 case 4 ------------------------------------------------------------------

#[test]
fn case4_reproposes_the_tip_with_the_tc() {
    let x = Block::new(BlockHeight::new(2), qc(3, &h()), CryptoHash::new([77; 32]), Data::new(vec![]));
    let the_tc = tc(VIEW - 1, Some(tip(&x, VIEW - 1)), None);
    let mut l = leader(&[g(), h(), x.clone()], qc(3, &h()), Some(the_tc));
    l.enter();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert!(p[0].block.hash == x.hash);
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1));
}

// L4 case 5 ------------------------------------------------------------------

#[test]
fn case5_waits_for_the_tip_instead_of_a_fresh_proposal() {
    // The winning tip X is not in the leader's tree.
    let x = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([78; 32]), Data::new(vec![]));
    let the_tc = tc(VIEW - 1, Some(tip(&x, VIEW - 1)), None);
    assert!(the_tc.high_tip_is_winner);
    let mut l = leader(&[g(), h()], qc(3, &h()), Some(the_tc.clone()));
    l.enter();
    assert!(l.proposals().is_empty(), "case 5 must not fall through to a fresh proposal");
    assert!(!l.hotstuff.has_deferred_proposal(), "case 5 waits on messages, not on the retry loop");

    // A validator answers the ProposalRequest with X: re-propose it with the TC.
    let response = ProposalResponse {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(VIEW),
        proposal: Proposal { chain_id: CHAIN_ID, view: ViewNumber::new(VIEW), block: x.clone(), tc: Some(the_tc), nec: None },
    };
    let origin = keys()[0].verifying_key();
    l.hotstuff
        .on_receive_msg(HotStuffMessage::ProposalResponse(response), &origin, &mut l.tree, &mut LeaderApp)
        .unwrap();
    let p = l.proposals();
    assert_eq!(p.len(), 1, "exactly one proposal in the view");
    assert!(p[0].block.hash == x.hash);
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1));
}

// VSU re-proposal through L2/L3 ----------------------------------------------

#[test]
fn vsu_reproposal_waits_for_the_tc_then_attaches_it() {
    // A Prepare QC (validator-set-update block h) from view 3: leader(5)
    // re-proposes h, which needs TC(4).
    let mut l = leader(&[g(), h()], pc_with_phase(3, &h(), Phase::Prepare), None);
    l.enter();
    assert!(l.proposals().is_empty(), "VSU re-proposal waits for TC(v-1)");
    assert!(l.hotstuff.has_deferred_proposal());
    l.tree.set_highest_tc(&tc(VIEW - 1, None, None)).unwrap();
    l.retry();
    let p = l.proposals();
    assert_eq!(p.len(), 1);
    assert!(p[0].block.hash == h().hash);
    assert_eq!(tc_view(&p[0]), Some(VIEW - 1));
}

// ---------------------------------------------------------------------------
// Codex review of phase A (3e5317ff):
// [P1] a winning-tip re-proposal (case 4, and the case-5 ProposalResponse)
//      sent its header with `has_validator_set_updates = false` even when the
//      tip updates the validator set, so validators voted Generic instead of
//      Prepare and the update could stall.
// [P2] the genesis exemption took L1 before looking at an available TC(v-1),
//      dropping its winning tip for a different height-0 block.
// RED-first: on 3e5317ff every test below fails.
// ---------------------------------------------------------------------------

mod review {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::hotstuff::implementation::HotStuffConfiguration;
    use crate::hotstuff::messages::ProposalHeader;
    use crate::networking::messages::{Message, ProgressMessage};
    use crate::networking::network::{Network, ValidatorSetUpdateHandle};
    use crate::networking::sending::SenderHandle;
    use crate::types::crypto_primitives::VerifyingKey;
    use crate::types::update_sets::ValidatorSetUpdates;
    use crate::types::data_types::Power;

    #[derive(Clone, Default)]
    struct RecordingNetwork {
        headers: Arc<Mutex<Vec<ProposalHeader>>>,
    }

    impl Network for RecordingNetwork {
        fn init_validator_set(&mut self, _: ValidatorSet) {}
        fn update_validator_set(&mut self, _: ValidatorSetUpdates) {}
        fn broadcast(&mut self, message: Message) {
            if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalHeader(h))) =
                message
            {
                self.headers.lock().unwrap().push(h);
            }
        }
        fn send(&mut self, _: VerifyingKey, _: Message) {}
        fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
            None
        }
    }

    /// Validates every block as a validator-set update.
    struct VsuApp;

    impl App<MemKV> for VsuApp {
        fn produce_block(&mut self, _: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
            unreachable!("re-proposal test")
        }
        fn validate_block(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
            ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: Some(updates()) }
        }
        fn validate_block_for_sync(&mut self, _: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
            unreachable!("re-proposal test")
        }
    }

    fn updates() -> ValidatorSetUpdates {
        let mut u = ValidatorSetUpdates::new();
        u.insert(keys()[0].verifying_key(), Power::new(2));
        u
    }

    struct RecordingLeader {
        hotstuff: HotStuff<RecordingNetwork>,
        tree: BlockTreeSingleton<MemKV>,
        net: RecordingNetwork,
        events: Receiver<Event>,
    }

    /// leader(VIEW) over a tree holding `blocks` (`vsu` ones inserted as
    /// validator-set updates), with `highest_pc` and `highest_tc` as given.
    fn recording_leader(
        blocks: &[Block],
        vsu: &[Block],
        highest_pc: PhaseCertificate,
        highest_tc: Option<TimeoutCertificate>,
    ) -> RecordingLeader {
        let keys = keys();
        let (mut tree, vss) = steady_block_tree(&set());
        for block in blocks {
            tree.insert(block, None, None).unwrap();
        }
        for block in vsu {
            tree.insert(block, None, Some(&updates())).unwrap();
        }
        let mut wb = BlockTreeWriteBatch::new();
        wb.set_highest_pc(&highest_pc).unwrap();
        tree.write(wb);
        if let Some(tc) = highest_tc {
            tree.set_highest_tc(&tc).unwrap();
        }
        let view = ViewNumber::new(VIEW);
        let leader_vk = proposer_for(view, &keys, &vss, &tree);
        let key = keys.iter().find(|k| k.verifying_key() == leader_vk).unwrap().clone();
        let net = RecordingNetwork::default();
        let (tx, events) = std::sync::mpsc::channel();
        let hotstuff = HotStuff::new(
            HotStuffConfiguration { chain_id: CHAIN_ID, keypair: Keypair::new(key), tc_required: false },
            ViewInfo::new(view, Instant::now() + Duration::from_secs(3600)),
            SenderHandle::new(net.clone()),
            ValidatorSetUpdateHandle::new(net.clone()),
            vss,
            Some(tx),
        );
        RecordingLeader { hotstuff, tree, net, events }
    }

    impl RecordingLeader {
        fn enter(&mut self, app: &mut impl App<MemKV>) {
            self.hotstuff
                .enter_view(
                    ViewInfo::new(ViewNumber::new(VIEW), Instant::now() + Duration::from_secs(3600)),
                    &mut self.tree,
                    app,
                )
                .unwrap();
        }

        fn headers(&self) -> Vec<ProposalHeader> {
            self.net.headers.lock().unwrap().clone()
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
    }

    /// [P1] case 4: the winning tip X updates the validator set; its
    /// re-proposal header must say so (validators then vote Prepare).
    #[test]
    fn case4_reproposal_of_a_vsu_tip_keeps_the_vsu_flag() {
        let x = Block::new(BlockHeight::new(2), qc(3, &h()), CryptoHash::new([79; 32]), Data::new(vec![]));
        let the_tc = tc(VIEW - 1, Some(tip(&x, VIEW - 1)), None);
        let mut l = recording_leader(&[g(), h()], &[x.clone()], qc(3, &h()), Some(the_tc));
        l.enter(&mut LeaderApp);
        let headers = l.headers();
        assert_eq!(headers.len(), 1);
        assert!(headers[0].block_hash == x.hash && headers[0].tc.is_some());
        assert!(headers[0].has_validator_set_updates, "VSU tip re-proposed with has_validator_set_updates=false");
    }

    /// Control: a tip without updates is re-proposed with the flag off.
    #[test]
    fn case4_reproposal_of_a_plain_tip_has_no_vsu_flag() {
        let x = Block::new(BlockHeight::new(2), qc(3, &h()), CryptoHash::new([80; 32]), Data::new(vec![]));
        let the_tc = tc(VIEW - 1, Some(tip(&x, VIEW - 1)), None);
        let mut l = recording_leader(&[g(), h(), x.clone()], &[], qc(3, &h()), Some(the_tc));
        l.enter(&mut LeaderApp);
        let headers = l.headers();
        assert_eq!(headers.len(), 1);
        assert!(!headers[0].has_validator_set_updates);
    }

    /// [P1] case 5: the leader lacks the VSU tip X and gets it in a
    /// ProposalResponse; the re-proposal header must carry the VSU flag.
    #[test]
    fn case5_recovered_vsu_tip_keeps_the_vsu_flag() {
        let x = Block::new(BlockHeight::new(2), qc(3, &h()), CryptoHash::new([81; 32]), Data::new(vec![]));
        let the_tc = tc(VIEW - 1, Some(tip(&x, VIEW - 1)), None);
        let mut l = recording_leader(&[g(), h()], &[], qc(3, &h()), Some(the_tc.clone()));
        l.enter(&mut VsuApp);
        assert!(l.headers().is_empty(), "case 5 waits");
        let response = ProposalResponse {
            chain_id: CHAIN_ID,
            view: ViewNumber::new(VIEW),
            proposal: Proposal { chain_id: CHAIN_ID, view: ViewNumber::new(VIEW), block: x.clone(), tc: Some(the_tc), nec: None },
        };
        let origin = keys()[0].verifying_key();
        l.hotstuff
            .on_receive_msg(HotStuffMessage::ProposalResponse(response), &origin, &mut l.tree, &mut VsuApp)
            .unwrap();
        let headers = l.headers();
        assert_eq!(headers.len(), 1);
        assert!(headers[0].block_hash == x.hash);
        assert!(headers[0].has_validator_set_updates, "recovered VSU tip re-proposed with has_validator_set_updates=false");
    }

    /// [P2] genesis highest PC, but TC(v-1) is held and its high tip wins:
    /// re-propose the tip with the TC rather than a fresh height-0 block.
    #[test]
    fn genesis_highest_pc_honours_a_winning_tc_tip() {
        let the_tc = tc(VIEW - 1, Some(tip(&g(), VIEW - 1)), None);
        assert!(the_tc.high_tip_is_winner);
        let mut l = recording_leader(&[g()], &[], PhaseCertificate::genesis_pc(), Some(the_tc));
        l.enter(&mut LeaderApp);
        let p = l.proposals();
        assert_eq!(p.len(), 1);
        assert!(p[0].block.hash == g().hash, "the TC's winning tip must be re-proposed");
        assert_eq!(tc_view(&p[0]), Some(VIEW - 1), "the TC must be attached");
    }

    /// Control: genesis highest PC and no TC: the bootstrap proposal goes out.
    #[test]
    fn genesis_highest_pc_without_a_tc_bootstraps() {
        let mut l = recording_leader(&[], &[], PhaseCertificate::genesis_pc(), None);
        l.enter(&mut LeaderApp);
        let p = l.proposals();
        assert_eq!(p.len(), 1);
        assert!(p[0].block.justify.is_genesis_pc() && p[0].tc.is_none());
    }
}
