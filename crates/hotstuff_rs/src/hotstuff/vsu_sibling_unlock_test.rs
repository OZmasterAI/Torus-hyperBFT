//! s138 (Codex torus-adversarial P1 on 8af2b949): a conflicting validator-set-update sibling U'
//! must not strand the honest replicas when one of them is locked on U.
//!
//! 4 equal validators, one Byzantine (leader of view 3). Prepare(U, 2) and Precommit(U, 3) form;
//! only the backup aggregator, leader(5), learns Precommit(U, 3) and locks on U. The Byzantine
//! leader signs a header of U' (a sibling of U on the same parent and justify) for its view 3
//! and reports it as its tip, so TC(3) (two unlocked honest replicas reporting Prepare(U, 2),
//! plus the Byzantine one) is valid under V1-V4 and its high tip wins. Honest leader(4)
//! re-proposes U'; the locked replica refuses, the other two and the Byzantine one certify
//! Prepare(U', 4), which reaches everyone. View 5's nudge is lost.
//!
//! From then on every TC's high QC is Prepare(U', 4), so leaders re-propose U' with its stale
//! justify QC(A, 1) (`repropose_block`; V3 admits "the high QC's block"). The replica locked on
//! Precommit(U, 3) refused it forever (the justify is older than its lock and does not extend U,
//! and U' is not its locked block), and with the Byzantine one withholding, two honest votes
//! never make a quorum of three.
//!
//! The fix (standard HotStuff unlock, `safe_proposal_lock`): a proposed block that a PC newer
//! than the lock certifies (here Prepare(U', 4), view 4 > 3, the TC's high QC and this replica's
//! highest PC) satisfies the lock, as a block justified by such a PC does.
//!
//! Second livelock, found by the extended Stateright model (`stateright_vsu_mixed_commit.rs`,
//! `copied_validator_set_update_tips_strand_the_update`), on every rule: the Byzantine validator
//! leads view 4 and signs a header of U' there. A TC(4) with its tip wins over Prepare(U, 2), the
//! honest leader re-proposes U' (V2), and the two unlocked replicas voting for it COPIED the TC's
//! tip (U', 4). Every later honest TC then has that tip beating the locked replica's
//! Precommit(U, 3), so leaders re-propose U', which no PC certifies, and the locked replica
//! refuses it forever. Fix: a vote on a validator-set-updating block never takes a tip (honest
//! votes on such blocks never set one, so the tail-fork argument has nothing to keep).

use std::time::{Duration, Instant};

use crate::app::{App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse};
use crate::block_tree::accessors::internal::BlockTreeSingleton;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree, validator_set, MemKV,
};
use crate::hotstuff::messages::{HotStuffMessage, NewView, Nudge, PhaseVote, ProposalHeader};
use crate::hotstuff::types::{valid_proposal_certificates, Phase, PhaseCertificate, PhaseVoteCollector};
use crate::pacemaker::implementation::ViewInfo;
use crate::pacemaker::types::{TimeoutCertificate, TimeoutVoteCollector, TipInfo};
use crate::test_keys::signed_by;
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Power, SignatureBytes, ViewNumber};
use crate::types::signed_messages::Collector;
use crate::types::update_sets::ValidatorSetUpdates;
use crate::types::validator_set::ValidatorSet;

const CHAIN_ID: ChainID = ChainID::new(0);

/// Every block under test is already in the tree, so no body is validated.
struct TreeApp;

impl App<MemKV> for TreeApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<MemKV>) -> ProduceBlockResponse {
        unreachable!("no replica produces a block in this test")
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::MissingData
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<MemKV>) -> ValidateBlockResponse {
        unreachable!("no block sync in this test")
    }
}

/// The header announcing the validator-set-updating `block` in `view`, carrying `tc`.
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
        has_validator_set_updates: true,
        leader_signature: SignatureBytes::new([0; 64]),
    }
}

/// `key`'s replica (block tree `tree`) processes `msg` from `origin` in `view`; returns the phase
/// votes it cast.
fn run(
    tree: &mut BlockTreeSingleton<MemKV>,
    key: &SigningKey,
    view: u64,
    msg: HotStuffMessage,
    origin: &VerifyingKey,
) -> Vec<PhaseVote> {
    let (mut hotstuff, events) =
        hotstuff_at_with_events(ViewNumber::new(view), key.clone(), tree.validator_set_state().unwrap());
    hotstuff.on_receive_msg(msg, origin, tree, &mut TreeApp).expect("processing must not error");
    events
        .try_iter()
        .filter_map(|event| match event {
            Event::PhaseVote(event) => Some(event.vote),
            _ => None,
        })
        .collect()
}

/// The PC that `votes` form in `view` (as the `PhaseVoteCollector` forms it).
fn certify(view: u64, votes: Vec<(SigningKey, PhaseVote)>, set: &ValidatorSet) -> PhaseCertificate {
    let mut collector = PhaseVoteCollector::new(CHAIN_ID, ViewNumber::new(view), set.clone());
    votes
        .into_iter()
        .filter_map(|(key, vote)| collector.collect(&key.verifying_key(), vote))
        .next()
        .expect("the votes form a quorum")
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
        tc = collector.collect(&key.verifying_key(), vote).or(tc);
    }
    tc.expect("the timeout votes form a quorum")
}

/// Four equal validators, one Byzantine, the honest replicas' block trees, all holding A, QC(A, 1)
/// and both validator-set-updating siblings U and U' (justify QC(A, 1)).
struct Cluster {
    keys: Vec<SigningKey>,
    set: ValidatorSet,
    byzantine: SigningKey,
    honest: Vec<SigningKey>,
    trees: Vec<BlockTreeSingleton<MemKV>>,
    qc_a: PhaseCertificate,
    u: Block,
    u_prime: Block,
}

impl Cluster {
    /// The Byzantine validator is the leader of `byzantine_view`.
    fn new(byzantine_view: u64) -> Cluster {
        let keys = signing_keys(&[1, 2, 3, 4]);
        let set = validator_set(&keys);
        let (template, vss) = steady_block_tree(&set);
        let byzantine_vk = proposer_for(ViewNumber::new(byzantine_view), &keys, &vss, &template);
        let byzantine = keys.iter().find(|k| k.verifying_key() == byzantine_vk).unwrap().clone();
        let honest: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != byzantine_vk).cloned().collect();
        let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
        let qc_a = generic_pc(ViewNumber::new(1), a.hash, &honest, &set);
        let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
        let u_prime = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([12; 32]), Data::new(vec![]));
        let trees = honest
            .iter()
            .map(|_| {
                let (mut tree, _) = steady_block_tree(&set);
                tree.insert(&a, None, None).unwrap();
                tree.update(&qc_a, &None).unwrap();
                let mut updates = ValidatorSetUpdates::new();
                updates.insert(honest[0].verifying_key(), Power::new(2));
                tree.insert(&u, None, Some(&updates)).unwrap();
                tree.insert(&u_prime, None, Some(&updates)).unwrap();
                tree
            })
            .collect();
        Cluster { keys, set, byzantine, honest, trees, qc_a, u, u_prime }
    }

    fn leader(&self, view: u64) -> VerifyingKey {
        let vss = self.trees[0].validator_set_state().unwrap();
        proposer_for(ViewNumber::new(view), &self.keys, &vss, &self.trees[0])
    }

    fn honest_index(&self, key: &VerifyingKey) -> Option<usize> {
        self.honest.iter().position(|k| k.verifying_key() == *key)
    }

    /// Every honest replica processes `msg` from `origin` in `view`; the votes they cast.
    fn deliver(&mut self, view: u64, msg: &HotStuffMessage, origin: &VerifyingKey) -> Vec<(SigningKey, PhaseVote)> {
        let mut votes = Vec::new();
        for (i, tree) in self.trees.iter_mut().enumerate() {
            let cast = run(tree, &self.honest[i], view, msg.clone(), origin);
            votes.extend(cast.into_iter().map(|vote| (self.honest[i].clone(), vote)));
        }
        votes
    }

    /// Every honest replica learns `pc` (a NewView / TC summary carrying it).
    fn share(&mut self, view: u64, pc: &PhaseCertificate, origin: &VerifyingKey) {
        let msg = HotStuffMessage::NewView(NewView { chain_id: CHAIN_ID, view: ViewNumber::new(view), highest_pc: pc.clone() });
        self.deliver(view, &msg, origin);
    }

    /// TC(`view`) from the honest replicas' timeout votes (their local tip and highest QC), or,
    /// with `byzantine`, from the Byzantine timeout vote (that tip and QC) and the first two
    /// honest ones (it arrives first).
    fn timeout_certificate(&self, view: u64, byzantine: Option<&(TipInfo, PhaseCertificate)>) -> TimeoutCertificate {
        let mut entries: Vec<_> = self
            .honest
            .iter()
            .zip(&self.trees)
            .map(|(k, tree)| (k.clone(), tree.local_tip().unwrap(), tree.highest_qc_for_timeout().unwrap()))
            .collect();
        if let Some((tip, qc)) = byzantine {
            entries.truncate(2);
            entries.insert(0, (self.byzantine.clone(), Some(tip.clone()), Some(qc.clone())));
        }
        timeout_certificate(view, entries, &self.set)
    }

    /// What the honest leader of `view` sends, holding `tc` = TC(view - 1) or not: its own
    /// `enter_view` (`propose_as_leader`, leader rules L1-L4) run on its block tree, read from
    /// its Propose / Nudge event.
    fn leader_message(&mut self, view: u64, tc: Option<&TimeoutCertificate>) -> HotStuffMessage {
        let leader = self.leader(view);
        let i = self.honest_index(&leader).expect("an honest leader");
        let tree = &mut self.trees[i];
        if let Some(tc) = tc {
            tree.set_highest_tc(tc).unwrap();
        }
        let (mut hotstuff, events) =
            hotstuff_at_with_events(ViewNumber::new(view - 1), self.honest[i].clone(), tree.validator_set_state().unwrap());
        hotstuff
            .enter_view(ViewInfo::new(ViewNumber::new(view), Instant::now() + Duration::from_secs(3600)), tree, &mut TreeApp)
            .expect("the leader enters its view");
        events
            .try_iter()
            .find_map(|event| match event {
                Event::Propose(event) => {
                    let has_updates = tree.validator_set_updates_status(&event.proposal.block.hash).unwrap().contains_updates();
                    Some(HotStuffMessage::ProposalHeader(ProposalHeader::from_proposal(&event.proposal, has_updates)))
                }
                Event::Nudge(event) => Some(HotStuffMessage::Nudge(event.nudge)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the leader of view {view} proposes or nudges"))
    }

    /// The synchronous suffix from `first` (TC(first - 1) if `previous_failed`): the Byzantine
    /// validator casts no vote and sends nothing in the views it leads; it sends no timeout vote
    /// either, unless `byzantine_timeout` (then its vote, with that tip and QC, is in every TC).
    /// Honest leaders run their own leader rules, every honest replica receives every message,
    /// and every PC the honest votes form reaches every honest replica. Runs until every honest
    /// replica committed a block or 30 views passed; returns a log line per view.
    fn synchronous_suffix(
        &mut self,
        first: u64,
        mut previous_failed: bool,
        byzantine_timeout: Option<&(TipInfo, PhaseCertificate)>,
    ) -> Vec<String> {
        let byzantine_vk = self.byzantine.verifying_key();
        let mut log = Vec::new();
        for view in first..first + 30 {
            if self.trees.iter().all(|tree| tree.highest_committed_block().unwrap().is_some()) {
                break;
            }
            let leader = self.leader(view);
            if leader == byzantine_vk {
                log.push(format!("view {view}: Byzantine leader silent"));
                previous_failed = true;
                continue;
            }
            let tc = previous_failed.then(|| self.timeout_certificate(view - 1, byzantine_timeout));
            let msg = self.leader_message(view, tc.as_ref());
            let what = match &msg {
                HotStuffMessage::ProposalHeader(h) => format!("proposal of {}", self.name(&h.block_hash)),
                HotStuffMessage::Nudge(n) => format!("nudge {:?}({}, {})", n.justify.phase, self.name(&n.justify.block), n.justify.view.int()),
                _ => unreachable!(),
            };
            let votes = self.deliver(view, &msg, &leader);
            log.push(format!("view {view}: {what}, {} honest votes", votes.len()));
            if votes.len() < self.honest.len() {
                previous_failed = true;
                continue;
            }
            let pc = certify(view, votes, &self.set);
            self.share(view, &pc, &leader);
            previous_failed = false;
        }
        log
    }

    fn name(&self, hash: &CryptoHash) -> &'static str {
        if *hash == self.u.hash {
            "U"
        } else if *hash == self.u_prime.hash {
            "U'"
        } else {
            "?"
        }
    }

    /// Every honest replica votes Prepare(`block`, 2) on leader(2)'s header; the PC.
    fn prepare_in_view_2(&mut self, block: &Block) -> PhaseCertificate {
        let leader2 = self.leader(2);
        let msg = HotStuffMessage::ProposalHeader(signed_by(header(block, 2, None), &leader2));
        let votes = self.deliver(2, &msg, &leader2);
        assert_eq!(votes.len(), 3, "every honest replica votes Prepare in view 2");
        certify(2, votes, &self.set)
    }

    /// Every honest replica votes Precommit on leader(3)'s nudge of `prepare`; the votes.
    fn precommit_votes_in_view_3(&mut self, prepare: &PhaseCertificate) -> Vec<(SigningKey, PhaseVote)> {
        let leader3 = self.leader(3);
        let msg = HotStuffMessage::Nudge(Nudge { chain_id: CHAIN_ID, view: ViewNumber::new(3), justify: prepare.clone() });
        let votes = self.deliver(3, &msg, &leader3);
        assert_eq!(votes.len(), 3, "every honest replica votes Precommit in view 3");
        votes
    }
}

/// The Codex P1 trace (every certificate is real: signed votes the replicas cast, a TC that
/// passes V1-V4), then a synchronous suffix in which every honest leader runs its own leader
/// rules. All three honest replicas must commit U'.
///
/// RED before the fix: from view 6 on, every re-proposal of U' gets two honest votes (the
/// replica locked on Precommit(U, 3) refuses), no PC forms, and nothing is ever committed.
#[test]
fn locked_replica_votes_for_a_sibling_certified_above_its_lock() {
    let mut c = Cluster::new(3);
    let (u, u_prime, qc_a) = (c.u.clone(), c.u_prime.clone(), c.qc_a.clone());
    let byzantine_vk = c.byzantine.verifying_key();
    let prepare2 = c.prepare_in_view_2(&u);
    let votes = c.precommit_votes_in_view_3(&prepare2);
    let precommit3 = certify(3, votes.clone(), &c.set);

    // The backup aggregator leader(5) alone collects Precommit(U, 3) and locks on U.
    let backup = c.leader(5);
    let locked = c.honest_index(&backup).expect("leader(5) is honest");
    let (mut aggregator, events) = hotstuff_at_with_events(
        ViewNumber::new(3),
        c.honest[locked].clone(),
        c.trees[locked].validator_set_state().unwrap(),
    );
    for (key, vote) in votes {
        aggregator
            .on_receive_msg(HotStuffMessage::PhaseVote(vote), &key.verifying_key(), &mut c.trees[locked], &mut TreeApp)
            .unwrap();
    }
    assert!(events
        .try_iter()
        .any(|e| matches!(e, Event::CollectPC(e) if e.phase_certificate == precommit3)));
    assert_eq!(c.trees[locked].locked_pc().unwrap().block, u.hash, "leader(5) is locked on U");

    // The Byzantine leader(3) reports its own signed header of U' as its tip(3), which beats
    // Prepare(U, 2) in TC(3).
    let tip = TipInfo::from_header(&signed_by(header(&u_prime, 3, None), &byzantine_vk));
    assert!(tip.is_correct(CHAIN_ID, &c.trees[0]).unwrap(), "a correctly signed tip of leader(3)");
    let mut entries: Vec<_> = c
        .honest
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != locked)
        .map(|(_, k)| (k.clone(), None, Some(prepare2.clone())))
        .collect();
    entries.push((c.byzantine.clone(), Some(tip), Some(prepare2.clone())));
    let tc3 = timeout_certificate(3, entries, &c.set);
    assert!(tc3.high_tip_is_winner);

    // Honest leader(4) re-proposes U' (V2). The locked replica refuses it; the other two and
    // the Byzantine one certify Prepare(U', 4), which every honest replica learns.
    let leader4 = c.leader(4);
    assert_ne!(leader4, backup);
    for tree in &c.trees {
        assert!(valid_proposal_certificates(CHAIN_ID, ViewNumber::new(4), &u_prime.hash, &qc_a, Some(&tc3), None, tree).unwrap());
    }
    let msg = c.leader_message(4, Some(&tc3));
    assert!(matches!(&msg, HotStuffMessage::ProposalHeader(h) if h.block_hash == u_prime.hash), "leader(4) re-proposes U'");
    let mut votes = c.deliver(4, &msg, &leader4);
    assert_eq!(votes.len(), 2, "only the unlocked replicas vote for U' in view 4");
    votes.push((c.byzantine.clone(), PhaseVote::new(&Keypair::new(c.byzantine.clone()), CHAIN_ID, ViewNumber::new(4), u_prime.hash, Phase::Prepare)));
    let prepare4 = certify(4, votes, &c.set);
    c.share(4, &prepare4, &backup);
    for (i, tree) in c.trees.iter().enumerate() {
        assert!(tree.highest_pc().unwrap() == prepare4, "replica {i} learns Prepare(U', 4)");
    }
    assert!(c.trees[locked].locked_pc().unwrap() == precommit3, "the lock on U stays");

    // View 5's nudge is lost. Synchronous suffix from view 6.
    let log = c.synchronous_suffix(6, true, None);
    for (i, tree) in c.trees.iter().enumerate() {
        assert_eq!(
            tree.highest_committed_block().unwrap(),
            Some(u_prime.hash),
            "replica {i} (locked on U: {}) must commit U' in the synchronous suffix; {log:?}",
            i == locked
        );
    }
}

/// The copied-tip livelock (module docs). Replica 0 alone locks on Precommit(U, 3). The
/// Byzantine leader(4) signs a header of U' and wins TC(4) with it over Prepare(U, 2); honest
/// leader(5) re-proposes U' (V2); the two unlocked replicas vote for it, the locked one refuses,
/// the Byzantine one withholds. In the synchronous suffix every honest replica must commit, and
/// no honest replica may have taken U' as its tip.
///
/// RED before the fix: the two voters copied the TC's tip (U', 4); every honest TC from then on
/// has that tip winning over Precommit(U, 3), every leader re-proposes U', the locked replica
/// refuses it (no PC certifies U'), and nothing is ever committed.
#[test]
fn a_copied_byzantine_tip_does_not_strand_the_update() {
    let (mut c, _) = byzantine_tip_of_the_sibling_in_view_4();
    let (u, u_prime) = (c.u.clone(), c.u_prime.clone());
    let tips: Vec<bool> = c
        .trees
        .iter()
        .map(|tree| tree.local_tip().unwrap().is_some_and(|tip| tip.block_hash == u_prime.hash))
        .collect();

    // The Byzantine one withholds its vote: no PC in view 5. Synchronous suffix from view 6.
    let log = c.synchronous_suffix(6, true, None);
    for (i, tree) in c.trees.iter().enumerate() {
        assert_eq!(
            tree.highest_committed_block().unwrap(),
            Some(u.hash),
            "replica {i} must commit U (re-proposed on Precommit(U, 3)) in the synchronous suffix; {log:?}"
        );
    }
    assert_eq!(tips, vec![false; 3], "no replica takes the Byzantine tip of the validator-set-updating U'");
}

/// RESIDUAL, not fixed (docs/plans/disjoint-vset-livelock.md section 9, "Active Byzantine
/// tip"): the same prefix, but the Byzantine validator stays active after it. It casts no vote,
/// yet its timeout vote, carrying its own tip (U', 4), arrives first and is in every TC; the tip
/// beats every honest QC (Precommit(U, 3) at most), so every honest leader re-proposes U' (V2),
/// which no PC certifies, and replica 0, locked on U, refuses it. Needs a decision on the
/// leader / NEC rules for a validator-set-updating tip (see the doc).
#[test]
#[ignore = "residual liveness gap: an active Byzantine VSU tip (disjoint-vset-livelock.md section 9)"]
fn an_active_byzantine_tip_does_not_strand_the_update() {
    let (mut c, byzantine_timeout) = byzantine_tip_of_the_sibling_in_view_4();
    let u = c.u.clone();
    let log = c.synchronous_suffix(6, true, Some(&byzantine_timeout));
    for (i, tree) in c.trees.iter().enumerate() {
        assert_eq!(tree.highest_committed_block().unwrap(), Some(u.hash), "replica {i} must commit U; {log:?}");
    }
}

/// The prefix of the copied-tip livelock, up to the failed view 5; returns the Byzantine
/// timeout vote's tip (U', 4) and QC (Prepare(U, 2)).
fn byzantine_tip_of_the_sibling_in_view_4() -> (Cluster, (TipInfo, PhaseCertificate)) {
    let mut c = Cluster::new(4);
    let (u, u_prime) = (c.u.clone(), c.u_prime.clone());
    let byzantine_vk = c.byzantine.verifying_key();
    let prepare2 = c.prepare_in_view_2(&u);
    let votes = c.precommit_votes_in_view_3(&prepare2);
    let precommit3 = certify(3, votes, &c.set);

    // Only replica 0 learns Precommit(U, 3) (the Byzantine leader(4) collected it).
    run(&mut c.trees[0], &c.honest[0].clone(), 4, HotStuffMessage::NewView(NewView {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(3),
        highest_pc: precommit3.clone(),
    }), &byzantine_vk);
    assert!(c.trees[0].locked_pc().unwrap() == precommit3, "replica 0 is locked on U");

    // TC(4): replicas 1 and 2 (Prepare(U, 2)) and the Byzantine one with its tip (U', 4).
    let tip = TipInfo::from_header(&signed_by(header(&u_prime, 4, None), &byzantine_vk));
    let entries = vec![
        (c.honest[1].clone(), None, Some(prepare2.clone())),
        (c.honest[2].clone(), None, Some(prepare2.clone())),
        (c.byzantine.clone(), Some(tip.clone()), Some(prepare2.clone())),
    ];
    let tc4 = timeout_certificate(4, entries, &c.set);
    assert!(tc4.high_tip_is_winner);

    let leader5 = c.leader(5);
    let msg = c.leader_message(5, Some(&tc4));
    assert!(matches!(&msg, HotStuffMessage::ProposalHeader(h) if h.block_hash == u_prime.hash), "leader(5) re-proposes U' (V2)");
    let votes = c.deliver(5, &msg, &leader5);
    assert_eq!(votes.len(), 2, "the unlocked replicas vote for U', the locked one refuses");
    (c, (tip, prepare2))
}

/// Codex adversarial probe on 8bd323af (fix/tc-embedded-chain-id): the same validator keys sign
/// another chain (chain 7), where a real quorum of honest votes certifies a genesis-justified
/// sibling of the block chain 0 committed at height 0. The Byzantine validator reports that chain-7
/// QC as its highest QC in TC(4) of chain 0; `TimeoutCertificate::is_correct` checked only its
/// signatures, so the TC was valid and its high QC (view 4, above every lock) the unlock witness:
/// all three honest replicas voted for the sibling, which then committed at replica 1 while
/// replica 0 had committed the original block. A TC must not carry another chain's QC.
#[test]
fn a_qc_of_another_chain_in_a_tc_does_not_unlock_a_conflicting_sibling() {
    use crate::hotstuff::header_fast_path_regression_test::NullNetwork;
    use crate::hotstuff::implementation::{HotStuff, HotStuffConfiguration};
    use crate::networking::{network::ValidatorSetUpdateHandle, sending::SenderHandle};

    let mut c = Cluster::new(4);
    c.trees = c.honest.iter().map(|_| steady_block_tree(&c.set).0).collect();
    let committed = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([31; 32]), Data::new(vec![]));
    let sibling = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([32; 32]), Data::new(vec![]));
    for tree in &mut c.trees {
        tree.insert(&committed, None, None).unwrap();
    }
    let ordinary_header = |block: &Block, view, tc| ProposalHeader { has_validator_set_updates: false, ..header(block, view, tc) };

    // Chain 0: QC(committed, 1), its child, QC(child, 2); replica 0 commits `committed`.
    let leader1 = c.leader(1);
    let votes1 = c.deliver(1, &HotStuffMessage::ProposalHeader(signed_by(ordinary_header(&committed, 1, None), &leader1)), &leader1);
    assert_eq!(votes1.len(), 3);
    let qc1 = certify(1, votes1, &c.set);
    let child = Block::new(BlockHeight::new(1), qc1.clone(), CryptoHash::new([33; 32]), Data::new(vec![]));
    for tree in &mut c.trees {
        tree.insert(&child, None, None).unwrap();
    }
    let leader2 = c.leader(2);
    let votes2 = c.deliver(2, &HotStuffMessage::ProposalHeader(signed_by(ordinary_header(&child, 2, None), &leader2)), &leader2);
    assert_eq!(votes2.len(), 3);
    let qc2 = certify(2, votes2, &c.set);
    let key0 = c.honest[0].clone();
    run(&mut c.trees[0], &key0, 3, HotStuffMessage::NewView(NewView { chain_id: CHAIN_ID, view: ViewNumber::new(2), highest_pc: qc2.clone() }), &leader2);
    assert!(c.trees[0].highest_committed_block().unwrap() == Some(committed.hash), "fixture: replica 0 commits `committed`");
    assert!(c.trees[1].highest_committed_block().unwrap().is_none());

    // Chain 7: honest replicas (same keys, fresh trees) vote for the sibling in view 4; no vote
    // equivocates within chain 7.
    let foreign_chain = ChainID::new(7);
    let leader4 = c.leader(4);
    let foreign_header = signed_by(ProposalHeader { chain_id: foreign_chain, ..ordinary_header(&sibling, 4, None) }, &leader4);
    let mut foreign_collector = PhaseVoteCollector::new(foreign_chain, ViewNumber::new(4), c.set.clone());
    let mut foreign_qc = None;
    for key in &c.honest {
        let (mut tree, vss) = steady_block_tree(&c.set);
        tree.insert(&sibling, None, None).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut hs = HotStuff::new(
            HotStuffConfiguration { chain_id: foreign_chain, keypair: Keypair::new(key.clone()) },
            ViewInfo::new(ViewNumber::new(4), Instant::now() + Duration::from_secs(3600)),
            SenderHandle::new(NullNetwork),
            ValidatorSetUpdateHandle::new(NullNetwork),
            vss,
            Some(tx),
        );
        hs.on_receive_msg(HotStuffMessage::ProposalHeader(foreign_header.clone()), &leader4, &mut tree, &mut TreeApp).unwrap();
        let vote = rx
            .try_iter()
            .find_map(|e| if let Event::PhaseVote(e) = e { Some(e.vote) } else { None })
            .expect("an honest chain-7 vote");
        foreign_qc = foreign_collector.collect(&key.verifying_key(), vote).or(foreign_qc);
    }
    let foreign_qc = foreign_qc.expect("a real chain-7 quorum");

    // Chain 0, view 5: leader(5) proposes the sibling with TC(4) whose high QC is chain 7's.
    for tree in &mut c.trees {
        tree.insert(&sibling, None, None).unwrap();
    }
    let tc = timeout_certificate(
        4,
        vec![
            (c.honest[0].clone(), None, Some(qc2)),
            (c.honest[1].clone(), None, Some(qc1)),
            (c.byzantine.clone(), None, Some(foreign_qc)),
        ],
        &c.set,
    );
    let leader5 = c.leader(5);
    let votes5 = c.deliver(5, &HotStuffMessage::ProposalHeader(signed_by(ordinary_header(&sibling, 5, Some(tc)), &leader5)), &leader5);
    let sibling_votes = votes5.len();
    if sibling_votes >= 3 {
        // The probe's continuation: the sibling commits at replica 1.
        let qc5 = certify(5, votes5, &c.set);
        let sibling_child = Block::new(BlockHeight::new(1), qc5, CryptoHash::new([34; 32]), Data::new(vec![]));
        for tree in &mut c.trees {
            tree.insert(&sibling_child, None, None).unwrap();
        }
        let leader6 = c.leader(6);
        let votes6 = c.deliver(6, &HotStuffMessage::ProposalHeader(signed_by(ordinary_header(&sibling_child, 6, None), &leader6)), &leader6);
        assert_eq!(votes6.len(), 3);
        let qc6 = certify(6, votes6, &c.set);
        let key1 = c.honest[1].clone();
        run(&mut c.trees[1], &key1, 7, HotStuffMessage::NewView(NewView { chain_id: CHAIN_ID, view: ViewNumber::new(6), highest_pc: qc6 }), &leader6);
    }
    let committed_at = |i: usize| c.trees[i].highest_committed_block().unwrap();
    assert!(
        committed_at(1).is_none_or(|hash| hash == committed.hash),
        "AGREEMENT VIOLATED: replica 0 committed `committed`, replica 1 the sibling (on {sibling_votes} sibling votes)"
    );
    assert_eq!(sibling_votes, 0, "honest replicas voted for the conflicting sibling on chain 7's QC");
}
