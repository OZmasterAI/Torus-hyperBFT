//! s140 r3 (bench 9p on main 3c5d02ea: ozarchy-s140-9p-r2-aK, r3-aK, r2-aK-rerun, 36-154 s
//! freezes at the leader exclusion). Four equal validators, x down, an update U = exclude x. The
//! Commit votes of U's Commit view vp + 2 go to the old leader of vp + 3, which is x, and to the
//! backup aggregator p = old leader(vp + 4), so only p forms Commit(U, vp + 2) and commits U. The
//! other replicas learn that PC from p's AdvanceView only. The pacemaker applied it with
//! `advance_highest_pc_from_remote` (highest PC, no commit), so they entered vp + 3 with the old
//! committed set. Once x is excluded, vp + 3 is led by q = new leader(vp + 3). When q != p (two
//! thirds of the exclusion views), q held the Commit PC but not U: it saw x as the leader of
//! vp + 3 and did not nudge, p did not lead vp + 3, so nobody nudged Decide (the first fix,
//! e2b44457, only covers q == p). The view timed out; on top, the commit-lag backoff
//! (TORUS_COMMIT_LAG_BACKOFF_CAP=8) counted the update's phase-PC views as commit lag: the
//! replicas without U's commit sat in vp + 3 for 76.8 s (2^6 x 1.2 s, r2-aK) while p, which had
//! committed U, left after 4.8 s and ran ahead alone; with every live vote needed (quorum 3 of 4)
//! nothing formed until the views met again 154 s later.
//!
//! Fixes: (1) an AdvanceView's Commit or Decide PC on an update still pending at the replica is
//! applied by HotStuff (correct and safe, as a NewView's highest PC is), so the update commits
//! before the pacemaker enters the next view; (2) the commit-lag term does not count the views of
//! an update's phase certificates (they certify the same block) as commit lag.

use super::body_before_expiry_tests::{new_algorithm, TestAlgorithm};
use super::*;
use crate::hotstuff::header_fast_path_regression_test::{
    generic_pc, hotstuff_at_with_events, signing_keys, steady_block_tree, validator_set, MemKV,
    NullNetwork,
};
use crate::hotstuff::messages::{HotStuffMessage, Nudge, PhaseVote};
use crate::hotstuff::types::{Phase, PhaseCertificate, PhaseVoteCollector};
use crate::pacemaker::implementation::select_leader;
use crate::pacemaker::messages::{PacemakerMessage, ProgressCertificate};
use crate::types::block::Block;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{BlockHeight, CryptoHash, Data, EpochLength};
use crate::types::signed_messages::{Certificate, Collector};
use crate::types::update_sets::ValidatorSetUpdates;
use crate::types::validator_set::ValidatorSet;
use std::sync::mpsc::Receiver;

const CHAIN: ChainID = ChainID::new(0);
const MAX_VIEW_TIME: Duration = Duration::from_secs(1);

/// The freeze shape for the fixture replica `me` (keys[0]): x = old leader(vp + 3) and the backup
/// aggregator p = old leader(vp + 4) are other validators, and `me` leads vp + 3 once x is
/// excluded. vp >= 4, so the update's phase PCs reach past the commit-lag grace over an empty
/// commit frontier (see `update_phase_pcs_add_no_commit_lag`). Returns (x, vp, p, the update).
fn freeze_shape(keys: &[SigningKey], set: &ValidatorSet) -> (VerifyingKey, u64, VerifyingKey, ValidatorSetUpdates) {
    let me = keys[0].verifying_key();
    let leader = |view: u64, set: &ValidatorSet| select_leader(ViewNumber::new(view), set);
    for vp in 4..64u64 {
        let x = leader(vp + 3, set);
        let p = leader(vp + 4, set);
        let mut updates = ValidatorSetUpdates::new();
        updates.exclude_leader(x);
        let mut new_set = set.clone();
        new_set.apply_updates(&updates);
        if x != me && p != me && p != x && leader(vp + 3, &new_set) == me {
            return (x, vp, p, updates);
        }
    }
    panic!("no view whose new leader is neither the down validator nor the backup aggregator");
}

/// The PC `signers` form for `block` in `view` and `phase` over `set`, as the collector forms it.
fn certify(view: u64, block: CryptoHash, phase: Phase, signers: &[SigningKey], set: &ValidatorSet) -> PhaseCertificate {
    let mut collector = PhaseVoteCollector::new(CHAIN, ViewNumber::new(view), set.clone());
    signers
        .iter()
        .filter_map(|key| {
            let vote = PhaseVote::new(&Keypair::new(key.clone()), CHAIN, ViewNumber::new(view), block, phase);
            collector.collect(&key.verifying_key(), vote)
        })
        .next()
        .expect("the signers form a quorum")
}

/// `pc` with one signature dropped: short of a quorum when `pc` has exactly one.
fn drop_one_signature(pc: &PhaseCertificate) -> PhaseCertificate {
    let mut forged = pc.clone();
    let first_signed = (0..forged.signatures.len()).find(|&i| forged.signatures.get(i).is_some()).unwrap();
    forged.signatures.set(first_signed, None);
    forged
}

/// The update's certificates: Prepare (vp), Precommit (vp + 1), Commit (vp + 2), all signed by
/// the three live validators over the old set.
struct Update {
    keys: Vec<SigningKey>,
    live: Vec<SigningKey>,
    set: ValidatorSet,
    new_set: ValidatorSet,
    x: VerifyingKey,
    p: VerifyingKey,
    vp: u64,
    a: Block,
    qc_a: PhaseCertificate,
    u: Block,
    updates: ValidatorSetUpdates,
    prepare: PhaseCertificate,
    precommit: PhaseCertificate,
    commit: PhaseCertificate,
}

fn update() -> Update {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let (x, vp, p, updates) = freeze_shape(&keys, &set);
    let live: Vec<SigningKey> = keys.iter().filter(|k| k.verifying_key() != x).cloned().collect();
    let mut new_set = set.clone();
    new_set.apply_updates(&updates);
    let a = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([10; 32]), Data::new(vec![]));
    let qc_a = generic_pc(ViewNumber::new(vp - 1), a.hash, &live, &set);
    let u = Block::new(BlockHeight::new(1), qc_a.clone(), CryptoHash::new([11; 32]), Data::new(vec![]));
    let prepare = certify(vp, u.hash, Phase::Prepare, &live, &set);
    let precommit = certify(vp + 1, u.hash, Phase::Precommit, &live, &set);
    let commit = certify(vp + 2, u.hash, Phase::Commit, &live, &set);
    Update { keys, live, set, new_set, x, p, vp, a, qc_a, u, updates, prepare, precommit, commit }
}

impl Update {
    /// A replica's block tree after it voted Commit in vp + 2: A, QC(A), U, and the Prepare and
    /// Precommit PCs (the justifies of the Precommit and Commit nudges it voted on).
    fn tree(&self) -> BlockTreeSingleton<MemKV> {
        let (mut tree, _) = steady_block_tree(&self.set);
        tree.insert(&self.a, None, None).unwrap();
        tree.update(&self.qc_a, &None).unwrap();
        tree.insert(&self.u, None, Some(&self.updates)).unwrap();
        tree.update(&self.prepare, &None).unwrap();
        tree.update(&self.precommit, &None).unwrap();
        tree
    }

    fn committed_u(&self, tree: &BlockTreeSingleton<MemKV>) -> bool {
        tree.validator_set_state().unwrap().committed_validator_set().is_leader_excluded(&self.x)
    }
}

/// The fixture replica (keys[0] = q) with the update's tree, its pacemaker and HotStuff in `view`
/// (the bench's backoff: factor 2, cap 8, commit-lag cap 8).
fn replica(u: &Update, view: u64) -> Replica {
    let tree = u.tree();
    let vss = tree.validator_set_state().unwrap();
    let (mut algorithm, messages, commands) = new_algorithm(tree);
    algorithm.pacemaker = Pacemaker::new(
        PacemakerConfiguration {
            chain_id: CHAIN,
            keypair: Keypair::new(u.keys[0].clone()),
            epoch_length: EpochLength::new(100_000),
            max_view_time: MAX_VIEW_TIME,
            backoff_factor: 2,
            backoff_cap: 8,
            commit_lag_cap: 8,
        },
        SenderHandle::new(NullNetwork),
        ViewNumber::new(view),
        &vss,
        None,
    )
    .unwrap();
    let (hotstuff, events) = hotstuff_at_with_events(ViewNumber::new(view), u.keys[0].clone(), vss);
    algorithm.hotstuff = hotstuff;
    Replica { algorithm, messages, events, _commands: commands }
}

struct Replica {
    algorithm: TestAlgorithm,
    messages: Sender<(VerifyingKey, ProgressMessage)>,
    events: Receiver<Event>,
    /// Kept open: the block-sync client's command channel.
    _commands: Receiver<crate::block_sync::worker::SyncCommand>,
}

/// Deliver `pc` to `algorithm` in an AdvanceView from `origin` through the algorithm loop's receive
/// step.
fn advance_view(replica: &mut Replica, origin: VerifyingKey, pc: &PhaseCertificate) {
    let msg = PacemakerMessage::advance_view(ProgressCertificate::PhaseCertificate(pc.clone()));
    replica.messages.send((origin, ProgressMessage::PacemakerMessage(msg))).unwrap();
    let algorithm = &mut replica.algorithm;
    let view_info = ViewInfo::new(algorithm.pacemaker.query().view, Instant::now() + Duration::from_millis(50));
    algorithm.poll_progress_and_retry(view_info, false);
}

/// The loop's step 4: HotStuff enters the pacemaker's view.
fn enter_pacemaker_view(algorithm: &mut TestAlgorithm) {
    let view_info = algorithm.pacemaker.query().clone();
    algorithm
        .hotstuff
        .enter_view(view_info, &mut algorithm.block_tree, &mut algorithm.app)
        .expect("HotStuff enters the pacemaker's view");
}

fn nudges(events: &Receiver<Event>) -> Vec<Nudge> {
    events
        .try_iter()
        .filter_map(|event| match event {
            Event::Nudge(event) => Some(event.nudge),
            _ => None,
        })
        .collect()
}

/// The bench shape: q gets Commit(U, vp + 2) only by p's AdvanceView. It must commit U from it,
/// enter vp + 3 as the leader U installs, and nudge Decide there with the Commit PC; that nudge
/// then gets a Decide vote from every live replica (p committed U itself; the third replica r, which
/// holds neither, applies the nudge's Commit PC first: fix e2b44457), a Decide quorum in vp + 3.
#[test]
fn commit_pc_from_an_advance_view_commits_the_update_and_its_new_leader_nudges_decide() {
    let u = update();
    let me = u.keys[0].verifying_key();
    let mut r = replica(&u, u.vp + 2);
    assert!(!u.committed_u(&r.algorithm.block_tree), "fixture: q has not committed U");

    advance_view(&mut r, u.p, &u.commit);
    let (q, events) = (&mut r.algorithm, &r.events);
    assert_eq!(q.pacemaker.query().view.int(), u.vp + 3, "the AdvanceView moves q to vp + 3");
    assert!(
        u.committed_u(&q.block_tree),
        "q commits U from the AdvanceView's Commit PC (else it does not know it leads vp + 3: \
         old leader x, new leader q)"
    );
    enter_pacemaker_view(q);
    let sent = nudges(events);
    assert_eq!(
        sent.iter().map(|n| (n.view.int(), n.justify.phase, n.justify.view.int())).collect::<Vec<_>>(),
        vec![(u.vp + 3, Phase::Commit, u.vp + 2)],
        "q, the leader U installs for vp + 3, nudges Decide with Commit(U, vp + 2)"
    );
    let nudge = HotStuffMessage::Nudge(sent[0].clone());

    // Every live replica votes Decide on it: q itself, p (committed U), r (holds neither).
    let mut decide_votes = Vec::new();
    q.hotstuff.on_receive_msg(nudge.clone(), &me, &mut q.block_tree, &mut q.app).unwrap();
    decide_votes.extend(events.try_iter().filter_map(|e| match e {
        Event::PhaseVote(e) => Some((u.keys[0].clone(), e.vote)),
        _ => None,
    }));
    for other in u.live.iter().filter(|k| k.verifying_key() != me) {
        let mut tree = u.tree();
        if other.verifying_key() == u.p {
            tree.update(&u.commit, &None).unwrap();
        }
        let (mut hotstuff, events) =
            hotstuff_at_with_events(ViewNumber::new(u.vp + 3), other.clone(), tree.validator_set_state().unwrap());
        hotstuff.on_receive_msg(nudge.clone(), &me, &mut tree, &mut q.app).unwrap();
        decide_votes.extend(events.try_iter().filter_map(|e| match e {
            Event::PhaseVote(e) => Some((other.clone(), e.vote)),
            _ => None,
        }));
    }
    assert_eq!(
        decide_votes.iter().filter(|(_, v)| v.phase == Phase::Decide && v.block == u.u.hash).count(),
        3,
        "every live replica votes Decide(U) in vp + 3: {:?}",
        decide_votes.iter().map(|(_, v)| v.phase).collect::<Vec<_>>()
    );
    let signers: Vec<SigningKey> = decide_votes.into_iter().map(|(k, _)| k).collect();
    let decide = certify(u.vp + 3, u.u.hash, Phase::Decide, &signers, &u.new_set);
    assert!(decide.is_correct(CHAIN, &q.block_tree).unwrap(), "a Decide quorum forms in vp + 3");
}

/// A Decide PC on an update still pending here (the replica missed the Commit PC) arrives by
/// AdvanceView: it must commit the update, not only mark it decided. The pacemaker's
/// `advance_highest_pc_from_remote` set `update_decided` with the old set still committed: the
/// replica then checked every proposer against the old set alone.
#[test]
fn decide_pc_from_an_advance_view_commits_the_pending_update() {
    let u = update();
    let mut r = replica(&u, u.vp + 3);
    let decide = certify(u.vp + 3, u.u.hash, Phase::Decide, &u.live, &u.new_set);
    advance_view(&mut r, u.p, &decide);
    let q = &r.algorithm;
    let state = q.block_tree.validator_set_state().unwrap();
    assert!(state.update_decided(), "the Decide PC decides U");
    assert!(u.committed_u(&q.block_tree), "and U is committed with it, not left pending under a decided flag");
}

/// The certificate is checked: a Commit PC short of a quorum is not applied (U stays pending and
/// the pacemaker does not move).
#[test]
fn sub_quorum_commit_pc_from_an_advance_view_is_not_applied() {
    let u = update();
    let mut r = replica(&u, u.vp + 2);
    let forged = drop_one_signature(&u.commit);
    assert!(!forged.is_correct(CHAIN, &r.algorithm.block_tree).unwrap(), "fixture: two of four is not a quorum");
    advance_view(&mut r, u.p, &forged);
    let q = &r.algorithm;
    assert!(!u.committed_u(&q.block_tree), "a sub-quorum Commit PC does not commit U");
    assert_eq!(q.pacemaker.query().view.int(), u.vp + 2, "nor move the pacemaker");
}

/// The commit-lag backoff must not count an update's phase-PC views as commit lag: they certify
/// the same block as its Generic QC. Measured from Commit(U, vp + 2) instead of QC(A, vp - 1), a
/// replica that has not committed U counted U's three phase views as lag and backed vp + 3 off
/// (in r2-aK 2^6: 76.8 s, while the replica that had committed U waited 4.8 s).
#[test]
fn update_phase_pcs_add_no_commit_lag() {
    let u = update();
    // q at vp + 2 holds Prepare and Precommit; Commit(U) comes by AdvanceView. Its committed
    // frontier is 0 (nothing committed); QC(A) is at vp - 1, U's phase PCs at vp .. vp + 2.
    let mut r = replica(&u, u.vp + 2);
    let q = &mut r.algorithm;
    let committed = q.block_tree.committed_qc_view().unwrap().int();
    assert_eq!(committed, 0, "fixture: nothing committed");
    let generic_lag = (u.vp - 1).saturating_sub(committed).saturating_sub(crate::pacemaker::implementation::COMMIT_LAG_GRACE);
    // Apply the Commit PC as the pacemaker alone does (no HotStuff, so no commit either way).
    let msg = PacemakerMessage::advance_view(ProgressCertificate::PhaseCertificate(u.commit.clone()));
    let entered = Instant::now();
    q.pacemaker.on_receive_msg(msg, &u.p, &mut q.block_tree).unwrap();
    assert!(!u.committed_u(&q.block_tree), "fixture: the pacemaker alone commits nothing");
    let view_info = q.pacemaker.query().clone();
    assert_eq!(view_info.view.int(), u.vp + 3);
    // Unbacked: the schedule's slot (two view times from the pacemaker's start) or, at most, the
    // multiplier of the Generic QC's lag.
    let allowed = MAX_VIEW_TIME * 2u32.pow(generic_lag as u32).max(2) + Duration::from_millis(500);
    assert!(
        view_info.deadline <= entered + allowed,
        "vp + 3's deadline is {:?} after entry (allowed {:?}): the Prepare / Precommit / Commit views \
         of U counted as commit lag over QC(A, {})",
        view_info.deadline.saturating_duration_since(entered),
        allowed,
        u.vp - 1
    );
}
