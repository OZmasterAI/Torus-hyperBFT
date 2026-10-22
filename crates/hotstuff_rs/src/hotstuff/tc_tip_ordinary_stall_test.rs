//! s138 (Codex review of fix/vsu-sibling-unlock 138a9219, P1): a Byzantine tip of an ORDINARY
//! (Generic, no validator-set update) sibling must not win every honest TC forever.
//!
//! 4 equal validators, one Byzantine (leader of view 4). A <- U, and V, a sibling of U on the same
//! parent A and justify QC(A, 1). Every honest replica votes for U in view 2 (tip (U, 2)); only
//! replica 0 learns QC(U, 2) and locks on it. The Byzantine leader(4) signs a header of V for its
//! view 4, never sends it, and reports it as its tip in TC(4); its tip (V, 4) beats QC(U, 2) by
//! view. Honest leader(5) re-proposes V (V2); replicas 1 and 2 vote for it and copy the TC's tip
//! (V, 4) (re-proposal votes take the TC's high tip, F5), replica 0 refuses it (its justify QC(A, 1)
//! is older than its lock and V does not extend U), the Byzantine one withholds and goes silent.
//!
//! From then on every honest TC carries (V, 4) (replicas 1 and 2) and QC(U, 2) (replica 0): the tip
//! wins by view, leaders re-propose V, replica 0 refuses it, two honest votes never make a quorum,
//! and nothing above A is ever committed.
//!
//! The fix (pacemaker/types.rs, the TC's winner rule): a high tip wins only if it also extends the
//! TC's high QC by rank (`tip.block_justify.view >= high_qc.view`); a tip whose block justifies
//! below the high QC is a sibling of a certified block, which the replicas locked on that QC can
//! never vote for, and the high QC wins (V3: a fresh block on it). Timeout votes always report
//! their highest PC (`highest_qc_for_timeout`), so an honest TC's high QC bounds every signer's
//! lock.

use std::time::{Duration, Instant};

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::messages::{HotStuffMessage, NewView, PhaseVote, ProposalHeader};
use crate::hotstuff::types::{PhaseCertificate, PhaseVoteCollector};
use crate::pacemaker::implementation::ViewInfo;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, SignatureBytes, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);

/// Produces an empty ordinary block per view; every block is valid.
struct OrdinaryApp;

impl App<MemKV> for OrdinaryApp {
    fn produce_block(&mut self, request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        let view = request.cur_view().int();
        let mut data_hash = [0x80; 32];
        data_hash[..8].copy_from_slice(&view.to_le_bytes());
        ProduceBlockResponse {
            data_hash: CryptoHash::new(data_hash),
            data: Data::new(vec![]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
}

/// The (unsigned) header announcing the ordinary `block` in `view`, carrying `tc`.
fn header(block: &Block, view: u64, tc: Option<TimeoutCertificate>) -> ProposalHeader {
    ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(view),
        block_hash: block.hash,
        height: block.height,
        data_hash: block.data_hash,
        justify: block.justify.clone(),
        tc,
        nec: None,
        has_validator_set_updates: false,
        leader_signature: SignatureBytes::new([0; 64]),
    }
}

/// `key`'s replica processes `msg` from `origin` in `view`; the phase votes it cast.
fn run(
    tree: &mut BlockTreeSingleton<MemKV>,
    key: &SigningKey,
    view: u64,
    msg: HotStuffMessage,
    origin: &VerifyingKey,
) -> Vec<PhaseVote> {
    let (mut hotstuff, events) =
        hotstuff_at_with_events(ViewNumber::new(view), key.clone(), tree.validator_set_state().unwrap());
    hotstuff.on_receive_msg(msg, origin, tree, &mut OrdinaryApp).expect("processing must not error");
    phase_votes(&events)
}

fn phase_votes(events: &std::sync::mpsc::Receiver<Event>) -> Vec<PhaseVote> {
    events
        .try_iter()
        .filter_map(|event| match event {
            Event::PhaseVote(event) => Some(event.vote),
            _ => None,
        })
        .collect()
}

/// The PC `votes` form in `view`, if they reach a quorum.
fn certify(view: u64, votes: Vec<(SigningKey, PhaseVote)>, set: &ValidatorSet) -> Option<PhaseCertificate> {
    let mut collector = PhaseVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    votes.into_iter().filter_map(|(key, vote)| collector.collect(&key.verifying_key(), vote)).next()
}

/// TC(`view`) from timeout votes, each signer reporting its tip and highest QC.
fn timeout_certificate(
    view: u64,
    entries: Vec<(SigningKey, Option<TipInfo>, Option<PhaseCertificate>)>,
    set: &ValidatorSet,
) -> TimeoutCertificate {
    use crate::pacemaker::messages::PacemakerMessage;
    let mut collector = TimeoutVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    let mut tc = None;
    for (key, tip, qc) in entries {
        let PacemakerMessage::TimeoutVote(vote) =
            PacemakerMessage::timeout_vote(&Keypair::new(key.clone()), CHAIN_ID, ViewNumber::new(view), None, tip, qc)
        else {
            unreachable!()
        };
        tc = tc.or(collector.collect(&key.verifying_key(), vote));
    }
    tc.expect("the timeout votes form a quorum")
}

/// Which timeout votes form the TCs of the synchronous suffix.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TcSigners {
    /// The three honest replicas (the Byzantine validator is silent).
    Honest,
    /// The Byzantine vote (its tip (V, 4), its QC) arrives first, then honest replicas 0 and 1.
    ByzantineWithLocked,
}

/// Four equal validators, one Byzantine (leader of view 4), the honest replicas' block trees, all
/// holding A, QC(A, 1) and the ordinary siblings U and V (justify QC(A, 1)).
struct Cluster {
    keys: Vec<SigningKey>,
    set: ValidatorSet,
    byzantine: SigningKey,
    honest: Vec<SigningKey>,
    trees: Vec<BlockTreeSingleton<MemKV>>,
    u: Block,
    v: Block,
}

impl Cluster {
    fn new() -> Cluster {
        let keys = signing_keys(&[1, 2, 3, 4]);
        let set = validator_set(&keys);
        let (template, vss) = steady_block_tree(&set);
        let byzantine_vk = proposer_for(ViewNumber::new(4), &keys, &vss, &template);
        let byzantine = keys.iter().find(|k| k.verifying_key() == byzantine_vk).unwrap().clone();
        let honest: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != byzantine_vk).cloned().collect();
        let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
        let qc_a = generic_pc(ViewNumber::new(1), a.hash, &honest, &set);
        let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
        let v = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([12; 32]), Data::new(vec![]));
        let trees = honest
            .iter()
            .map(|_| {
                let (mut tree, _) = steady_block_tree(&set);
                tree.insert(&a, None, None).unwrap();
                tree.update(&qc_a, &None).unwrap();
                tree.insert(&u, None, None).unwrap();
                tree.insert(&v, None, None).unwrap();
                tree
            })
            .collect();
        Cluster { keys, set, byzantine, honest, trees, u, v }
    }

    fn leader(&self, view: u64) -> VerifyingKey {
        let vss = self.trees[0].validator_set_state().unwrap();
        proposer_for(ViewNumber::new(view), &self.keys, &vss, &self.trees[0])
    }

    fn honest_index(&self, key: &VerifyingKey) -> Option<usize> {
        self.honest.iter().position(|k| k.verifying_key() == *key)
    }

    /// Every honest replica learns `pc` (a NewView carrying it).
    fn share(&mut self, view: u64, pc: &PhaseCertificate, origin: &VerifyingKey) {
        let msg = HotStuffMessage::NewView(NewView { chain_id: CHAIN_ID, view: ViewNumber::new(view), highest_pc: pc.clone() });
        for (i, tree) in self.trees.iter_mut().enumerate() {
            run(tree, &self.honest[i], view, msg.clone(), origin);
        }
    }

    /// An honest replica's own timeout-vote summaries (its local tip and the QC it reports).
    fn honest_entry(&self, i: usize) -> (SigningKey, Option<TipInfo>, Option<PhaseCertificate>) {
        let tree = &self.trees[i];
        (self.honest[i].clone(), tree.local_tip().unwrap(), tree.highest_qc_for_timeout().unwrap())
    }

    /// TC(`view`) of the synchronous suffix.
    fn suffix_tc(&self, view: u64, signers: TcSigners, byzantine: &(TipInfo, PhaseCertificate)) -> TimeoutCertificate {
        let entries = match signers {
            TcSigners::Honest => (0..self.honest.len()).map(|i| self.honest_entry(i)).collect(),
            TcSigners::ByzantineWithLocked => vec![
                (self.byzantine.clone(), Some(byzantine.0.clone()), Some(byzantine.1.clone())),
                self.honest_entry(0),
                self.honest_entry(1),
            ],
        };
        timeout_certificate(view, entries, &self.set)
    }

    /// Honest leader(`view`) enters its view holding `tc` (TC(view - 1)) or not and runs its own
    /// leader rules; then every honest replica processes what it sent (the header if the replica
    /// holds the block, else the full proposal, which validates and inserts it). Returns a log
    /// line and the honest votes (the leader's inline vote included).
    fn leader_round(&mut self, view: u64, tc: Option<&TimeoutCertificate>) -> (String, Vec<(SigningKey, PhaseVote)>) {
        let leader = self.leader(view);
        let li = self.honest_index(&leader).expect("an honest leader");
        if let Some(tc) = tc {
            self.trees[li].set_highest_tc(tc).unwrap();
        }
        let (mut hotstuff, events) = hotstuff_at_with_events(
            ViewNumber::new(view - 1),
            self.honest[li].clone(),
            self.trees[li].validator_set_state().unwrap(),
        );
        hotstuff
            .enter_view(ViewInfo::new(ViewNumber::new(view), Instant::now() + Duration::from_secs(3600)), &mut self.trees[li], &mut OrdinaryApp)
            .expect("the leader enters its view");
        let events: Vec<Event> = events.try_iter().collect();
        let mut votes: Vec<(SigningKey, PhaseVote)> = events
            .iter()
            .filter_map(|e| match e {
                Event::PhaseVote(e) => Some((self.honest[li].clone(), e.vote.clone())),
                _ => None,
            })
            .collect();
        let Some(proposal) = events.iter().find_map(|e| match e {
            Event::Propose(e) => Some(e.proposal.clone()),
            _ => None,
        }) else {
            return (format!("view {view}: leader sent nothing"), votes);
        };
        let what = format!(
            "view {view}: {} {} (height {}, justify view {})",
            if proposal.is_reproposal() { "re-proposal of" } else { "proposal of" },
            self.name(&proposal.block.hash),
            proposal.block.height.int(),
            proposal.block.justify.view.int(),
        );
        let header = ProposalHeader::from_proposal(&proposal, false);
        for i in 0..self.honest.len() {
            let msg = if self.trees[i].contains(&proposal.block.hash) {
                HotStuffMessage::ProposalHeader(header.clone())
            } else {
                HotStuffMessage::Proposal(proposal.clone())
            };
            let cast = run(&mut self.trees[i], &self.honest[i].clone(), view, msg, &leader);
            votes.extend(cast.into_iter().map(|vote| (self.honest[i].clone(), vote)));
        }
        (what, votes)
    }

    /// The synchronous suffix from `first`, holding TC(first - 1): the Byzantine validator casts no
    /// vote and sends nothing in the views it leads; its timeout vote is in every TC only with
    /// `TcSigners::ByzantineWithLocked`. Runs until every honest replica committed a block above A
    /// or 30 views passed; returns a log line per view.
    fn synchronous_suffix(
        &mut self,
        first: u64,
        first_tc: TimeoutCertificate,
        signers: TcSigners,
        byzantine: &(TipInfo, PhaseCertificate),
    ) -> Vec<String> {
        let byzantine_vk = self.byzantine.verifying_key();
        let mut log = Vec::new();
        let mut tc = Some(first_tc);
        for view in first..first + 30 {
            if self.all_committed_above_a() {
                break;
            }
            if self.leader(view) == byzantine_vk {
                log.push(format!("view {view}: Byzantine leader silent"));
                tc = Some(self.suffix_tc(view, signers, byzantine));
                continue;
            }
            let (what, votes) = self.leader_round(view, tc.as_ref());
            log.push(format!("{what}, {} honest votes", votes.len()));
            match certify(view, votes, &self.set) {
                Some(pc) => {
                    let leader = self.leader(view);
                    self.share(view, &pc, &leader);
                    tc = None;
                }
                None => tc = Some(self.suffix_tc(view, signers, byzantine)),
            }
        }
        log
    }

    fn all_committed_above_a(&self) -> bool {
        self.trees.iter().all(|tree| {
            tree.highest_committed_block()
                .unwrap()
                .and_then(|b| tree.block_height(&b).unwrap())
                .is_some_and(|h| h.int() >= 1)
        })
    }

    fn name(&self, hash: &CryptoHash) -> &'static str {
        if *hash == self.u.hash {
            "U"
        } else if *hash == self.v.hash {
            "V"
        } else {
            "a fresh block"
        }
    }
}

/// The prefix up to TC(4): everyone votes U in view 2, only replica 0 learns QC(U, 2) and locks on
/// it; TC(4) holds replicas 1 and 2 (tip (U, 2)) and the Byzantine vote with its own signed tip
/// (V, 4) and QC(U, 2). Returns the cluster, TC(4) and the Byzantine timeout summaries.
fn byzantine_ordinary_tip_in_view_4() -> (Cluster, TimeoutCertificate, (TipInfo, PhaseCertificate)) {
    let mut c = Cluster::new();
    let (u, v) = (c.u.clone(), c.v.clone());
    let byzantine_vk = c.byzantine.verifying_key();

    let leader2 = c.leader(2);
    let msg = HotStuffMessage::ProposalHeader(signed_by(header(&u, 2, None), &leader2));
    let mut votes = Vec::new();
    for i in 0..c.honest.len() {
        let cast = run(&mut c.trees[i], &c.honest[i].clone(), 2, msg.clone(), &leader2);
        votes.extend(cast.into_iter().map(|vote| (c.honest[i].clone(), vote)));
    }
    assert_eq!(votes.len(), 3, "every honest replica votes for U in view 2");
    let qc_u = certify(2, votes, &c.set).expect("QC(U, 2)");

    run(&mut c.trees[0], &c.honest[0].clone(), 4, HotStuffMessage::NewView(NewView {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(3),
        highest_pc: qc_u.clone(),
    }), &byzantine_vk);
    assert!(c.trees[0].locked_pc().unwrap() == qc_u, "replica 0 is locked on QC(U, 2)");

    let tip = TipInfo::from_header(&signed_by(header(&v, 4, None), &byzantine_vk));
    assert!(tip.is_correct(CHAIN_ID, &c.trees[0]).unwrap(), "a correctly signed tip of leader(4)");
    let byzantine = (tip, qc_u);
    let tc4 = timeout_certificate(
        4,
        vec![
            c.honest_entry(1),
            c.honest_entry(2),
            (c.byzantine.clone(), Some(byzantine.0.clone()), Some(byzantine.1.clone())),
        ],
        &c.set,
    );
    (c, tc4, byzantine)
}

/// Codex's probe (`ordinary_blocks_stall`): the Byzantine validator goes silent after TC(4); every
/// TC of the synchronous suffix is honest. Every honest replica must commit a block above A, and
/// no honest replica may end up holding the Byzantine tip (V, 4).
///
/// RED before the fix: leader(5) re-proposes V, replicas 1 and 2 copy (V, 4), and from then on
/// every honest TC has that tip winning over QC(U, 2); replica 0 refuses every re-proposal of V and
/// nothing is committed (`committed_height1=0/3`).
#[test]
fn a_silent_byzantine_ordinary_tip_does_not_stall_the_chain() {
    let (mut c, tc4, byzantine) = byzantine_ordinary_tip_in_view_4();
    let log = c.synchronous_suffix(5, tc4, TcSigners::Honest, &byzantine);
    let committed: Vec<_> = c.trees.iter().map(|t| t.highest_committed_block().unwrap()).collect();
    assert!(c.all_committed_above_a(), "every honest replica must commit above A; committed {committed:?}; {log:?}");
}

/// The same prefix, but the Byzantine validator stays active: its timeout vote, carrying its tip
/// (V, 4) and QC(U, 2), arrives first and is in every TC, with honest replicas 0 (locked on U) and
/// 1. It casts no vote. Every honest replica must commit a block above A.
///
/// RED before the fix: (V, 4) wins every TC by view, every honest leader re-proposes V, replica 0
/// refuses it and the Byzantine one withholds: no QC, in any view.
#[test]
fn an_active_byzantine_ordinary_tip_does_not_stall_the_chain() {
    let (mut c, tc4, byzantine) = byzantine_ordinary_tip_in_view_4();
    let log = c.synchronous_suffix(5, tc4, TcSigners::ByzantineWithLocked, &byzantine);
    assert!(c.all_committed_above_a(), "every honest replica must commit above A; {log:?}");
}
