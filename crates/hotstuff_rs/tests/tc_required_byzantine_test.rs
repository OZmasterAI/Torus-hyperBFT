//! Phase B of docs/plans/tc-required-and-tip-signing.md (§2 V1/V3, §6
//! multi-node): byzantine leaders and voters, a leader restart while it waits
//! for TC(v-1), and a validator-set update with timeout views.
//!
//! Four validators of equal power (quorum 3, f = 1) on a mock network whose
//! shared filter rewrites messages to play the byzantine part (proposal
//! headers carry no leader signature yet, phase C, so a rewritten header is
//! exactly what a byzantine leader could send).
//!
//! RED-first: on phase A (3e5317ff, validators lax) honest validators vote for
//! the TC-stripped headers, so both byzantine-leader cases fail.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use ed25519_dalek::Signer;

use hotstuff_rs::{
    hotstuff::{
        messages::HotStuffMessage,
        types::{Phase, PhaseCertificate},
    },
    networking::messages::{Message, ProgressMessage},
    pacemaker::{
        messages::{AdvanceView, PacemakerMessage, ProgressCertificate},
        select_leader,
        types::TipInfo,
    },
    types::{
        crypto_primitives::{SigningKey, VerifyingKey},
        data_types::{CryptoHash, Power, SignatureBytes, SignatureSet, ViewNumber},
        update_sets::{AppStateUpdates, ValidatorSetUpdates},
        signing::sign_proposal_header,
        validator_set::ValidatorSet,
    },
};

mod common;

use common::{
    network::{mock_network_with_filter, FilterHandle, NetworkStub},
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);
const EPOCH_LENGTH: u32 = 50;
/// Blocks every validator must commit while the byzantine node misbehaves.
const TARGET_HEIGHT: u64 = 5;
/// The byzantine-leader cases lose two views per rotation of four (about one
/// commit per rotation, ~25 s idle with the backoff); they stop at this height.
const BYZANTINE_LEADER_TARGET: u64 = 3;
const LIVENESS_BOUND: Duration = Duration::from_secs(240);

/// `n` equal validators, keys `[1; 32]` .. `[n; 32]`.
struct Cluster {
    keypairs: Vec<SigningKey>,
    vs_updates: ValidatorSetUpdates,
    init_as: AppStateUpdates,
    filter: FilterHandle,
    stubs: Vec<NetworkStub>,
    nodes: Vec<Option<Node>>,
}

impl Cluster {
    fn new() -> Cluster {
        Cluster::with_validators(4)
    }

    fn with_validators(n: u8) -> Cluster {
        let keypairs: Vec<SigningKey> = (1..=n).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut vs_updates = ValidatorSetUpdates::new();
        for kp in &keypairs {
            vs_updates.insert(kp.verifying_key(), Power::new(1));
        }
        let (stubs, filter) = mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));
        Cluster {
            keypairs,
            vs_updates,
            init_as: NumberApp::initial_app_state(),
            filter,
            stubs,
            nodes: Vec::new(),
        }
    }

    fn validator_set(&self) -> ValidatorSet {
        let mut vs = ValidatorSet::new();
        vs.apply_updates(&self.vs_updates);
        vs
    }

    fn key(&self, i: usize) -> VerifyingKey {
        self.keypairs[i].verifying_key()
    }

    /// The index of `leader(view)`.
    fn leader(&self, view: u64) -> usize {
        let leader = select_leader(ViewNumber::new(view), &self.validator_set());
        self.keypairs.iter().position(|kp| kp.verifying_key() == leader).unwrap()
    }

    fn node(&self, i: usize, kv_store: Option<common::mem_db::MemDB>) -> Node {
        Node::new_with_epoch_length(
            self.keypairs[i].clone(),
            self.stubs[i].clone(),
            self.init_as.clone(),
            self.vs_updates.clone(),
            MAX_VIEW_TIME,
            EPOCH_LENGTH,
            kv_store,
        )
    }

    /// Start every validator not in `down` (arm the filter first).
    fn start(&mut self, down: &[usize]) {
        self.nodes = (0..self.keypairs.len())
            .map(|i| (!down.contains(&i)).then(|| self.node(i, None)))
            .collect();
        for node in self.nodes.iter_mut().flatten() {
            node.submit_transaction(NumberAppTransaction::Increment);
        }
    }

    fn live(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().flatten()
    }

    fn max_committed(&self) -> u64 {
        self.live().map(|n| n.committed_height().unwrap_or(0)).max().unwrap_or(0)
    }

    fn describe(&self) -> String {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| match n {
                Some(n) => format!(
                    "n{i}{{committed={:?}, highest_pc_view={}, view={}}}",
                    n.committed_height(),
                    n.highest_pc_view(),
                    n.highest_view_entered(),
                ),
                None => format!("n{i}{{down}}"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Wait for every live validator to commit `height`.
    fn wait_for_commits(&self, height: u64, context: &str) {
        wait_until(
            LIVENESS_BOUND,
            POLL_INTERVAL,
            &format!("every live validator to commit height >= {height} ({context})"),
            || self.live().all(|n| n.committed_height().unwrap_or(0) >= height),
            || self.describe(),
        );
    }
}

fn header_of(message: &Message) -> Option<&hotstuff_rs::hotstuff::messages::ProposalHeader> {
    match message {
        Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalHeader(h))) => Some(h),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Byzantine leader that drops the TC.
// ---------------------------------------------------------------------------

/// How the view before the byzantine leader's fails, so it must back its
/// proposal with a TC.
#[derive(Clone, Copy, PartialEq)]
enum Interrupt {
    /// The honest leader's header is lost (asynchrony): nobody votes, the view
    /// times out.
    HonestHeaderLost,
    /// Tail-fork attempt: every honest validator votes for the honest leader's
    /// block B (2f+1 votes), but the votes are withheld (from the byzantine
    /// next leader and, as lost messages, from the backup vote recipient), so
    /// QC(B) never forms and the view times out. The byzantine leader then
    /// proposes a fresh block (not extending B) without the TC.
    VotesWithheldByByzantineLeader,
}

#[derive(Default)]
struct ByzantineLeaderLog {
    /// (view, block) of every header the byzantine leader sent whose TC was
    /// stripped although V1 needed it (its block does not justify on view - 1).
    stripped: HashSet<(u64, CryptoHash)>,
    /// (voter, view, block) of every phase vote an honest validator sent.
    honest_votes: HashSet<([u8; 32], u64, CryptoHash)>,
    /// (view, voter) of the honest votes for the honest leader's views that were withheld.
    withheld: HashSet<(u64, [u8; 32])>,
    /// (view, block, voter) of the same withheld votes.
    withheld_blocks: HashSet<(u64, CryptoHash, [u8; 32])>,
}

/// The byzantine validator `byz` strips the TC from every proposal header it
/// sends; the view before each of its slots fails as `interrupt` says. The
/// honest validators must reject every stripped header (VALIDPROPOSAL V1),
/// lose that view and keep committing.
fn byzantine_leader_drops_the_tc(interrupt: Interrupt) {
    let mut cluster = Cluster::new();
    // byz leads view 5; `honest` leads the view before it.
    let byz_index = cluster.leader(5);
    let honest_index = cluster.leader(4);
    assert_ne!(byz_index, honest_index);
    let byz = cluster.key(byz_index);
    let honest = cluster.key(honest_index);
    let log = Arc::new(Mutex::new(ByzantineLeaderLog::default()));
    let vs = cluster.validator_set();

    let log_in = log.clone();
    cluster.filter.rewrite(Arc::new(move |sender, _recipient, message| {
        let leader_of = |view: ViewNumber| select_leader(view, &vs);
        if let Some(header) = header_of(&message) {
            if *sender == honest && interrupt == Interrupt::HonestHeaderLost {
                return None;
            }
            if *sender == byz && header.tc.is_some() {
                let mut header = header.clone();
                header.tc = None;
                // Only a header that needs its TC (V1) must be rejected without
                // it: case 3 can apply a high QC of view v-1 and justify on it.
                let needs_tc = header.justify.view.int() + 1 != header.view.int() && !header.justify.is_genesis_pc();
                if needs_tc {
                    log_in.lock().unwrap().stripped.insert((header.view.int(), header.block_hash));
                }
                return Some(HotStuffMessage::ProposalHeader(header).into());
            }
        }
        if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::PhaseVote(vote))) =
            &message
        {
            if *sender != byz {
                let mut log = log_in.lock().unwrap();
                log.honest_votes.insert((sender.to_bytes(), vote.view.int(), vote.block));
                if interrupt == Interrupt::VotesWithheldByByzantineLeader && leader_of(vote.view) == honest {
                    log.withheld.insert((vote.view.int(), sender.to_bytes()));
                    log.withheld_blocks.insert((vote.view.int(), vote.block, sender.to_bytes()));
                    return None;
                }
            }
        }
        Some(message)
    }));
    cluster.start(&[]);
    cluster.wait_for_commits(BYZANTINE_LEADER_TARGET, "a byzantine leader strips its TCs");

    // Take the log out first: a failed assertion must not poison the mutex
    // the replicas' network threads still use while the cluster shuts down.
    let log = std::mem::take(&mut *log.lock().unwrap());
    assert!(!log.stripped.is_empty(), "scenario: the byzantine leader never had a TC to strip");
    let voted_for_stripped: Vec<_> = log
        .honest_votes
        .iter()
        .filter(|(_, view, block)| log.stripped.contains(&(*view, *block)))
        .collect();
    assert!(
        voted_for_stripped.is_empty(),
        "honest validators voted for TC-stripped headers (V1 not enforced): {} votes, stripped {:?}",
        voted_for_stripped.len(),
        log.stripped.iter().map(|(v, _)| v).collect::<Vec<_>>()
    );
    if interrupt == Interrupt::VotesWithheldByByzantineLeader {
        // The tail-fork setting: some honest leader's block got votes from all
        // three honest validators (2f+1), withheld from the byzantine leader.
        let mut per_view = std::collections::HashMap::<u64, usize>::new();
        for (view, _) in &log.withheld {
            *per_view.entry(*view).or_default() += 1;
        }
        assert!(
            per_view.values().any(|&voters| voters >= 3),
            "scenario: no block of the honest leader got 2f+1 withheld votes: {per_view:?}"
        );
        // Phase C (F3, F5): such a block B is every honest voter's signed tip,
        // so TC(w) carries B as its winning high tip and an honest leader
        // re-proposes B, which the honest validators vote for in a later view.
        let mut voters_of = std::collections::HashMap::<(u64, CryptoHash), usize>::new();
        for (view, block, _) in &log.withheld_blocks {
            *voters_of.entry((*view, *block)).or_default() += 1;
        }
        let tail_fork_blocks: Vec<_> = voters_of.iter().filter(|(_, &n)| n >= 3).map(|(key, _)| *key).collect();
        let reproposed = tail_fork_blocks.iter().any(|(w, b)| {
            log.honest_votes.iter().any(|(_, view, block)| block == b && view > w)
        });
        assert!(
            reproposed,
            "no block with 2f+1 withheld votes was re-proposed and voted in a later view: {:?}",
            tail_fork_blocks.iter().map(|(w, _)| w).collect::<Vec<_>>()
        );
    }
}

/// §6: a byzantine leader that drops the TC loses its view; the next one commits.
#[test]
fn byzantine_leader_dropping_the_tc_loses_only_its_view() {
    byzantine_leader_drops_the_tc(Interrupt::HonestHeaderLost);
}

/// §6 tail-fork attempt: B gets 2f+1 votes, its QC is withheld, a
/// fresh-block proposal without the TC is rejected, and B is re-proposed (and
/// voted) in a later view (phase C, F5: the header path sets the tip).
#[test]
fn tail_fork_attempt_fresh_block_without_tc_is_rejected() {
    byzantine_leader_drops_the_tc(Interrupt::VotesWithheldByByzantineLeader);
}

// ---------------------------------------------------------------------------
// Byzantine voter with a forged high QC (phase-A open question 2).
// ---------------------------------------------------------------------------

/// The bytes a TimeoutVote signs.
fn timeout_vote_bytes(vote: &hotstuff_rs::pacemaker::messages::TimeoutVote) -> Vec<u8> {
    let (tip, qc) = vote.signed_summaries();
    hotstuff_rs::types::signing::timeout_vote_bytes(vote.chain_id, vote.view, tip, qc)
}

/// The byzantine validator `byz` stays silent as leader (so the view after
/// each of its slots needs a TC) and sends every timeout vote with a forged
/// highest QC: a QC for the vote's own view over a block nobody has, with no
/// signatures, re-signed so the vote itself is correct. Without the
/// pacemaker's check the forged QC becomes the TC's high QC whenever the
/// vote is among the first quorum, and under V3 no honest proposal can
/// extend it: the chain stalls behind every silent slot.
#[test]
fn byzantine_voter_with_a_forged_high_qc_cannot_block_case_3() {
    let mut cluster = Cluster::new();
    let byz_index = cluster.leader(5);
    let byz_key = cluster.keypairs[byz_index].clone();
    let byz = byz_key.verifying_key();
    let n = cluster.keypairs.len();
    let forged = Arc::new(Mutex::new(0u64));

    let forged_in = forged.clone();
    cluster.filter.rewrite(Arc::new(move |sender, _recipient, message| {
        if *sender != byz {
            return Some(message);
        }
        match message {
            Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalHeader(_))) => None,
            Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::TimeoutVote(mut vote))) => {
                vote.highest_qc = Some(PhaseCertificate {
                    chain_id: vote.chain_id,
                    view: vote.view,
                    block: CryptoHash::new([0xEE; 32]),
                    phase: Phase::Generic,
                    signatures: SignatureSet::new(n),
                });
                vote.signature = SignatureBytes::new(byz_key.sign(&timeout_vote_bytes(&vote)).to_bytes());
                *forged_in.lock().unwrap() += 1;
                Some(PacemakerMessage::TimeoutVote(vote).into())
            }
            other => Some(other),
        }
    }));
    cluster.start(&[]);
    cluster.wait_for_commits(TARGET_HEIGHT, "a byzantine voter forges its high QC");
    assert!(*forged.lock().unwrap() > 0, "scenario: no forged timeout vote was sent");
}

/// Phase C (§6 "byzantine voter with a forged tip: the TC still forms"): the
/// byzantine validator `byz` stays silent as leader and sends every timeout
/// vote with a forged tip for the vote's own view: a block hash its header
/// fields do not hash to (F2 recompute), signed by byz (the view's leader in
/// the views it leads, which are the ones that time out; a non-leader in the
/// others), re-signed so the vote itself is correct. Its view is above every
/// honest high QC. Without F4 the forged tip wins every TC it is in
/// and the next leader must run a recovery (ProposalRequest / NERequest) for
/// a block nobody has; with F4 honest replicas drop the vote, so no TC carries
/// the forged tip and nobody recovers it.
#[test]
fn byzantine_voter_with_a_forged_tip_cannot_block_the_tc() {
    let mut cluster = Cluster::new();
    let byz_index = cluster.leader(5);
    let byz_key = cluster.keypairs[byz_index].clone();
    let byz = byz_key.verifying_key();
    let forged = Arc::new(Mutex::new(0u64));
    let recoveries = Arc::new(Mutex::new(0u64));
    let justify = PhaseCertificate::genesis_pc();
    let data_hash = CryptoHash::new([0xDD; 32]);
    let height = hotstuff_rs::types::data_types::BlockHeight::new(0);
    // Not `Block::hash(height, &justify, &data_hash)`: the fields do not hash to it.
    let forged_block = CryptoHash::new([0xDE; 32]);

    let forged_in = forged.clone();
    let recoveries_in = recoveries.clone();
    cluster.filter.rewrite(Arc::new(move |sender, _recipient, message| {
        if *sender != byz {
            let tc = match &message {
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::NERequest(r))) => Some(&r.tc),
                Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalRequest(r))) => {
                    Some(&r.tc)
                }
                _ => None,
            };
            if tc.and_then(|tc| tc.high_tip.as_ref()).is_some_and(|tip| tip.block_hash == forged_block) {
                *recoveries_in.lock().unwrap() += 1;
            }
            return Some(message);
        }
        match message {
            Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalHeader(_))) => None,
            Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::TimeoutVote(mut vote))) => {
                let tip_view = vote.view;
                vote.local_tip = Some(TipInfo {
                    block_hash: forged_block,
                    block_height: height,
                    block_justify: justify.clone(),
                    block_data_hash: data_hash,
                    view: tip_view,
                    leader_signature: sign_proposal_header(&byz_key, vote.chain_id, tip_view, &forged_block),
                });
                vote.highest_qc = None;
                vote.signature = SignatureBytes::new(byz_key.sign(&timeout_vote_bytes(&vote)).to_bytes());
                *forged_in.lock().unwrap() += 1;
                Some(PacemakerMessage::TimeoutVote(vote).into())
            }
            other => Some(other),
        }
    }));
    cluster.start(&[]);
    cluster.wait_for_commits(TARGET_HEIGHT, "a byzantine voter forges its tip");
    assert!(*forged.lock().unwrap() > 0, "scenario: no forged timeout vote was sent");
    let recoveries = *recoveries.lock().unwrap();
    assert_eq!(recoveries, 0, "honest leaders ran {recoveries} recovery requests for the forged tip (its TC formed)");
}

// ---------------------------------------------------------------------------
// Leader restart while it waits for TC(v-1).
// ---------------------------------------------------------------------------

/// One validator `down` never runs, so every view it leads times out and
/// leader(w) (w the next view) needs TC(w-1). TC(w-1) is kept from L =
/// leader(w) (its incoming timeout votes and AdvanceViews for w-1 are
/// dropped), so L enters w and waits (`awaiting_tc`). L is restarted on its
/// own store mid-wait; the cluster must keep committing.
#[test]
fn leader_restarted_while_awaiting_the_tc_recovers() {
    let mut cluster = Cluster::new();
    // The first view >= 10 whose previous view's leader is the down validator.
    let down = cluster.leader(9);
    let w = (10..).find(|w| cluster.leader(w - 1) == down).unwrap();
    let l_index = cluster.leader(w);
    let l = cluster.key(l_index);
    let proposed_in_w = Arc::new(Mutex::new(false));

    let proposed = proposed_in_w.clone();
    cluster.filter.rewrite(Arc::new(move |sender, recipient, message| {
        if *sender == l && header_of(&message).is_some_and(|h| h.view.int() == w) {
            *proposed.lock().unwrap() = true;
        }
        if *recipient == l {
            if let Message::ProgressMessage(ProgressMessage::PacemakerMessage(pm)) = &message {
                let for_prev = match pm {
                    PacemakerMessage::TimeoutVote(tv) => tv.view.int() + 1 == w,
                    PacemakerMessage::AdvanceView(av) => av.progress_certificate.view().int() + 1 == w,
                };
                if for_prev {
                    return None;
                }
            }
        }
        Some(message)
    }));
    cluster.start(&[down]);

    wait_until(
        LIVENESS_BOUND,
        Duration::from_millis(20),
        &format!("leader n{l_index} to enter view {w}"),
        || cluster.nodes[l_index].as_ref().unwrap().highest_view_entered().int() >= w,
        || cluster.describe(),
    );
    std::thread::sleep(Duration::from_millis(300));
    let entered = cluster.nodes[l_index].as_ref().unwrap().highest_view_entered().int();
    assert_eq!(entered, w, "scenario: n{l_index} left view {w} before the restart: {}", cluster.describe());
    assert!(!*proposed_in_w.lock().unwrap(), "n{l_index} proposed in view {w} without TC({})", w - 1);

    let committed_before = cluster.max_committed();
    let store = cluster.nodes[l_index].take().unwrap().kv_store();
    cluster.filter.heal();
    cluster.nodes[l_index] = Some(cluster.node(l_index, Some(store)));
    cluster.wait_for_commits(committed_before + 3, &format!("n{l_index} restarted in view {w}"));
}

// ---------------------------------------------------------------------------
// Validator-set update with timeout views (V3 amendment).
// ---------------------------------------------------------------------------

/// Five equal validators, one down (the live four are the exact quorum). The
/// phased voting on a validator-set-updating block (Prepare, Precommit,
/// Commit in consecutive views, then Decide) is interrupted at the down
/// validator's slots, and the next leader re-proposes that block under
/// TC(v-1), whose high QC certifies the block itself: the block justifies
/// below that QC's view, which V3 allows only through its amendment ("or B is
/// q's block"). Five, not four: with one of four down only three consecutive
/// views separate its slots, and the update then commits only when the
/// re-proposal lands exactly on the first of them (timing-dependent, it
/// stalled for 180 s in some phase-A runs as well).
#[test]
fn validator_set_update_commits_through_timeout_views() {
    let mut cluster = Cluster::with_validators(5);
    let down = 4;
    // Observe: VSU re-proposals under a TC whose high QC certifies the block
    // itself while the block justifies below it (the V3 amendment's case).
    let amended = Arc::new(Mutex::new(HashSet::<u64>::new()));
    let amended_in = amended.clone();
    // Force that case once: drop the first Prepare nudge (the leader of the
    // view after the update block's carries its Prepare QC). That view times
    // out with the Prepare QC only in the nudging leader's hands; with one of
    // five down every live vote is in the TC (quorum 4), so the TC's high QC
    // is that Prepare QC and the next leader re-proposes the update block
    // under it, justifying below it.
    let dropped_nudge = Arc::new(Mutex::new(None::<u64>));
    cluster.filter.rewrite(Arc::new(move |_sender, _recipient, message| {
        if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::Nudge(nudge))) = &message {
            if nudge.justify.phase == Phase::Prepare {
                let mut dropped = dropped_nudge.lock().unwrap();
                if dropped.is_none_or(|view| view == nudge.view.int()) {
                    *dropped = Some(nudge.view.int());
                    return None;
                }
            }
        }
        if let Some(h) = header_of(&message) {
            let q = h.tc.as_ref().and_then(|tc| tc.high_qc.as_ref());
            if h.has_validator_set_updates && q.is_some_and(|q| q.block == h.block_hash && h.justify.view < q.view) {
                amended_in.lock().unwrap().insert(h.view.int());
            }
        }
        Some(message)
    }));
    cluster.start(&[down]);
    cluster.wait_for_commits(2, "before the update");
    let target = cluster.key(0);
    cluster.nodes[0]
        .as_mut()
        .unwrap()
        .submit_transaction(NumberAppTransaction::SetValidator(target.to_bytes(), Power::new(2)));
    wait_until(
        LIVENESS_BOUND,
        POLL_INTERVAL,
        "every live validator to commit the power update of n0",
        || cluster.live().all(|n| n.committed_validator_set().power(&target).map(|p| p.int()) == Some(2)),
        || cluster.describe(),
    );
    let after = cluster.max_committed();
    cluster.wait_for_commits(after + 2, "after the update");
    let amended = std::mem::take(&mut *amended.lock().unwrap());
    assert!(
        !amended.is_empty(),
        "scenario: no VSU re-proposal under a TC whose high QC certifies the block itself (the V3 amendment case)"
    );
}

// ---------------------------------------------------------------------------
// s136 ([claude-review] P3): the validator-set update window.
// ---------------------------------------------------------------------------

/// One proposal header the cluster sent: its view, its justify's view, and, if it carries a TC,
/// the TC's view, high-QC phase and high-tip view.
#[derive(Clone, Copy, Debug, PartialEq)]
struct HeaderViews {
    view: u64,
    justify_view: u64,
    tc: Option<(u64, Option<Phase>, Option<u64>)>,
}

#[derive(Default)]
struct WindowLog {
    /// The update block whose header was let through (the first one proposed right after a view
    /// whose leader differs between the two sets).
    update_block: Option<CryptoHash>,
    /// Views of update-block proposals dropped to get there.
    skipped: HashSet<u64>,
    /// The view of the first fresh header justified by the update's Decide PC (dropped).
    dropped: Option<u64>,
    /// The validator whose timeout votes for `dropped` are lost.
    silent: Option<VerifyingKey>,
    headers: Vec<HeaderViews>,
    /// Timeout votes for `dropped` and the view after it as sent: (view, sender index, tip view,
    /// high-QC view and phase), for the failure messages.
    timeout_votes: Vec<(u64, usize, Option<u64>, Option<(u64, Phase)>)>,
}

/// No view is lost after the timeout of the first view after an update (s136).
///
/// Four validators; a power update of n0 (1 -> 2: total 5, quorum 4). Votes on the update block
/// and its nudges set no tip, so every honest tip is a pre-update one, signed by the previous
/// set's leader: the update block is only let through when proposed right after a view whose
/// leader differs between the two sets (n0's earlier update proposals are dropped), so that tip
/// fails under the committed set. The Decide votes reach only the committed set's leader of the
/// next view (w), and its AdvanceView with the Decide PC is lost. Its fresh proposal (justified by the Decide PC, so only it has applied that PC)
/// is lost, and so are one
/// power-1 validator's timeout votes for w, so TC(w) carries the leader of w's vote: its high QC
/// is the Decide PC (case 3) and its high tip the pre-update tip. leader(w + 1) applies the Decide
/// PC and proposes with that TC. Under the decided-ness rule that leader and leader(w) (both
/// decided) refused the TC, the two others could not reach the quorum and view w + 1 was lost
/// too. With one fresh proposer per view regardless of the decision, QC(w + 1) forms.
#[test]
fn validator_set_update_window_timeout_loses_no_further_view() {
    let mut cluster = Cluster::new();
    let keys: Vec<VerifyingKey> = (0..4).map(|i| cluster.key(i)).collect();
    let target = keys[0];
    let old_set = cluster.validator_set();
    let mut new_set = old_set.clone();
    let mut power_update = ValidatorSetUpdates::new();
    power_update.insert(target, Power::new(2));
    new_set.apply_updates(&power_update);
    let split = {
        let (old_set, new_set) = (old_set.clone(), new_set.clone());
        move |view: u64| select_leader(ViewNumber::new(view), &old_set) != select_leader(ViewNumber::new(view), &new_set)
    };

    let log = Arc::new(Mutex::new(WindowLog::default()));
    let log_in = log.clone();
    let split_in = split.clone();
    let (keys_in, new_set_in) = (keys.clone(), new_set.clone());
    cluster.filter.rewrite(Arc::new(move |sender, recipient, message| {
        let mut w = log_in.lock().unwrap();
        // Only the committed set's leader of the next view collects the Decide votes, so it alone
        // holds the Decide PC until it proposes on it (otherwise the votes also reach the previous
        // set's leader and the backup aggregators, and every validator is decided by then).
        if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::PhaseVote(vote))) = &message {
            if vote.phase == Phase::Decide && *recipient != select_leader(ViewNumber::new(vote.view.int() + 1), &new_set_in) {
                return None;
            }
        }
        // Its AdvanceView carrying the Decide PC is lost too (asynchrony): the others stay undecided.
        if let Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::AdvanceView(AdvanceView {
            progress_certificate: ProgressCertificate::PhaseCertificate(pc),
        }))) = &message
        {
            if pc.phase == Phase::Decide && recipient != sender {
                return None;
            }
        }
        if let Some(h) = header_of(&message) {
            let view = h.view.int();
            if h.has_validator_set_updates && w.update_block.is_none() && !w.skipped.contains(&view) {
                if split_in(view - 1) {
                    w.update_block = Some(h.block_hash);
                } else {
                    w.skipped.insert(view);
                }
            }
            if w.update_block.is_some() && w.dropped.is_none() && h.justify.phase == Phase::Decide && !h.has_validator_set_updates {
                let next_leader = select_leader(ViewNumber::new(view + 1), &new_set_in);
                w.dropped = Some(view);
                w.silent = keys_in[1..].iter().copied().find(|k| k != sender && *k != next_leader);
            }
        }
        if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(
            m @ (HotStuffMessage::Proposal(_) | HotStuffMessage::ProposalHeader(_) | HotStuffMessage::BlockDataResponse(_)),
        )) = &message
        {
            let view = m.view().int();
            if w.skipped.contains(&view) || w.dropped == Some(view) {
                return None;
            }
        }
        if let Message::ProgressMessage(ProgressMessage::PacemakerMessage(PacemakerMessage::TimeoutVote(tv))) = &message {
            if w.dropped.is_some_and(|view| tv.view.int() == view || tv.view.int() == view + 1) {
                let entry = (
                    tv.view.int(),
                    keys_in.iter().position(|k| k == sender).unwrap(),
                    tv.local_tip.as_ref().map(|tip| tip.view.int()),
                    tv.highest_qc.as_ref().map(|qc| (qc.view.int(), qc.phase)),
                );
                if !w.timeout_votes.contains(&entry) {
                    w.timeout_votes.push(entry);
                }
            }
            if Some(tv.view.int()) == w.dropped && w.silent.as_ref() == Some(sender) {
                return None;
            }
        }
        if let Some(h) = header_of(&message) {
            let entry = HeaderViews {
                view: h.view.int(),
                justify_view: h.justify.view.int(),
                tc: h.tc.as_ref().map(|tc| {
                    (tc.view.int(), tc.high_qc.as_ref().map(|qc| qc.phase), tc.high_tip.as_ref().map(|tip| tip.view.int()))
                }),
            };
            if w.headers.last() != Some(&entry) {
                w.headers.push(entry);
            }
        }
        Some(message)
    }));
    cluster.start(&[]);
    cluster.wait_for_commits(2, "before the update");
    cluster.nodes[0]
        .as_mut()
        .unwrap()
        .submit_transaction(NumberAppTransaction::SetValidator(target.to_bytes(), Power::new(2)));
    wait_until(
        LIVENESS_BOUND,
        POLL_INTERVAL,
        "every validator to commit the power update of n0",
        || cluster.live().all(|n| n.committed_validator_set().power(&target).map(|p| p.int()) == Some(2)),
        || cluster.describe(),
    );
    let after = cluster.max_committed();
    cluster.wait_for_commits(after + 3, "after the update");

    let log = log.lock().unwrap();
    let w = log.dropped.expect("scenario: the first fresh proposal after the update was seen and dropped");
    let trace = format!(
        "w = {w}, skipped {:?}, silent n{:?}, leaders(w, w + 1) = n{} n{}; timeout votes (view, sender, tip view, high QC): {:?}; headers: {:?}",
        log.skipped,
        log.silent.and_then(|s| keys.iter().position(|k| *k == s)),
        keys.iter().position(|k| *k == select_leader(ViewNumber::new(w), &new_set)).unwrap(),
        keys.iter().position(|k| *k == select_leader(ViewNumber::new(w + 1), &new_set)).unwrap(),
        log.timeout_votes,
        log.headers
    );
    let recovery = log
        .headers
        .iter()
        .find(|h| h.view == w + 1 && h.tc.is_some_and(|(tc_view, _, _)| tc_view == w))
        .unwrap_or_else(|| panic!("scenario: leader(w + 1) proposed under TC(w); {trace}"));
    let (_, high_qc_phase, high_tip_view) = recovery.tc.unwrap();
    assert_eq!(high_qc_phase, Some(Phase::Decide), "scenario: case 3 on the Decide PC; {trace}");
    let tip_view = high_tip_view.unwrap_or_else(|| panic!("scenario: TC(w) has a high tip; {trace}"));
    assert!(split(tip_view), "scenario: the high tip's view {tip_view} has different leaders in the two sets; {trace}");
    assert!(log.headers.iter().any(|h| h.justify_view == w + 1), "view w + 1 was lost: no header justifies on QC(w + 1); {trace}");
}
