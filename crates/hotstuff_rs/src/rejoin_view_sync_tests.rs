//! s74 rejoin view sync (round-skip), end to end through the algorithm loop: a
//! replica that restarted behind the survivors receives leader(w)'s header for
//! a future view w, enters w through the pacemaker and votes exactly once.
use super::body_before_expiry_tests::{fixture, new_algorithm, Fixture};
use super::*;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree, validator_set,
    NullNetwork,
};
use crate::hotstuff::messages::{NewView, PhaseVote, ProposalHeader};
use crate::hotstuff::types::PhaseCertificate;
use crate::types::block::Block;
use crate::types::crypto_primitives::Keypair;
use crate::types::data_types::{BlockHeight, CryptoHash, Data, EpochLength};
use std::sync::mpsc::Receiver;

const LOCAL_VIEW: u64 = 3;

struct Rejoin {
    f: Fixture,
    origin: VerifyingKey,
    header: ProposalHeader,
    events: Receiver<Event>,
}

fn header_for(body: &Block, view: ViewNumber) -> ProposalHeader {
    ProposalHeader {
        chain_id: ChainID::new(0),
        view,
        block_hash: body.hash,
        height: body.height,
        data_hash: body.data_hash,
        justify: body.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
    }
}

fn body(seed: u8) -> Block {
    Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([seed; 32]),
        Data::new(vec![]),
    )
}

fn leader(view: ViewNumber, f: &Fixture) -> VerifyingKey {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let (_, vss) = steady_block_tree(&validator_set(&keys));
    proposer_for(view, &keys, &vss, &f.algorithm.block_tree)
}

/// The fixture replica (keys[0]) with its pacemaker AND HotStuff at
/// `LOCAL_VIEW`, and a safe header (genesis justify) from leader(w) for the
/// first view w >= LOCAL_VIEW + 2 that keys[0] does not lead.
fn rejoin(round_skip: bool) -> Rejoin {
    rejoin_at(LOCAL_VIEW, round_skip)
}

fn rejoin_at(local_view: u64, round_skip: bool) -> Rejoin {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let (_, vss) = steady_block_tree(&validator_set(&keys));
    let mut f = fixture();
    let me = keys[0].verifying_key();
    let future = (local_view + 2..)
        .map(ViewNumber::new)
        .find(|view| leader(*view, &f) != me)
        .unwrap();
    let local = ViewNumber::new(local_view);
    f.algorithm.pacemaker = Pacemaker::new(
        PacemakerConfiguration {
            chain_id: ChainID::new(0),
            keypair: Keypair::new(keys[0].clone()),
            epoch_length: EpochLength::new(100),
            max_view_time: Duration::from_secs(1),
            backoff_factor: 1,
            backoff_cap: 0,
            commit_lag_cap: 0,
        },
        SenderHandle::new(NullNetwork),
        local,
        &vss,
        None,
    )
    .unwrap();
    let (mut hotstuff, events) = hotstuff_at_with_events(local, keys[0].clone(), vss);
    hotstuff.set_round_skip(round_skip);
    f.algorithm.hotstuff = hotstuff;
    let origin = leader(future, &f);
    Rejoin { f, origin, header: header_for(&body(95), future), events }
}

impl Rejoin {
    fn view(&self) -> ViewNumber {
        self.f.algorithm.pacemaker.query().view
    }

    fn poll(&mut self) {
        let view_info = ViewInfo::new(self.view(), Instant::now() + Duration::from_millis(50));
        self.f.algorithm.poll_progress_and_retry(view_info, false);
    }

    fn deliver(&mut self, origin: VerifyingKey, header: ProposalHeader) {
        self.f
            .messages
            .send((origin, ProgressMessage::HotStuffMessage(header.into())))
            .unwrap();
        self.poll();
    }

    /// s84: a replica votes once it holds the body; deliver it (as a fetch
    /// response or push) from `origin`.
    fn deliver_body(&mut self, origin: VerifyingKey, block: Block, view: ViewNumber) {
        let resp = crate::hotstuff::messages::BlockDataResponse { view, block };
        self.f
            .messages
            .send((origin, ProgressMessage::HotStuffMessage(resp.into())))
            .unwrap();
        self.poll();
    }

    fn votes(&self) -> Vec<PhaseVote> {
        self.events
            .try_iter()
            .filter_map(|event| match event {
                Event::PhaseVote(event) => Some(event.vote),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn rejoining_replica_skips_to_leader_header_and_votes_once() {
    let mut r = rejoin(true);
    let (origin, header) = (r.origin, r.header.clone());
    let w = header.view;

    r.deliver(origin, header.clone());
    assert_eq!(r.view(), w, "the pacemaker entered the header's view");
    assert!(
        !r.f.algorithm.hotstuff.is_view_outdated(r.f.algorithm.pacemaker.query()),
        "HotStuff entered it too"
    );
    assert!(r.votes().is_empty(), "s84: no vote before the body is held");
    // The first poll drains the receive buffer's cached copy of the header.
    r.poll();
    r.deliver_body(origin, body(95), w);
    let votes = r.votes();
    assert_eq!(votes.len(), 1, "exactly one vote");
    assert_eq!((votes[0].view, votes[0].block), (w, header.block_hash));
    assert_eq!(r.f.algorithm.block_tree.highest_view_voted().unwrap(), Some(w));

    // Never again <= w. The retransmission is refused by the persisted
    // voted-view guard (same block, so equivocation detection does not fire);
    // the conflicting header at w by equivocation detection; the w-1 header
    // by being stale. The first poll drains the receive buffer's cached copy
    // of the header, if any (same guard as the retransmission).
    r.poll();
    r.deliver(origin, header.clone());
    let stale = ViewNumber::new(w.int() - 1);
    let stale_origin = leader(stale, &r.f);
    r.deliver(stale_origin, header_for(&body(96), stale));
    r.deliver_body(stale_origin, body(96), stale);
    r.deliver(origin, header_for(&body(97), w));
    r.deliver_body(origin, body(97), w);
    r.poll();
    assert!(r.votes().is_empty(), "no second vote at or below w");
    assert_eq!(r.view(), w);
    assert_eq!(
        r.f.algorithm.block_tree.last_voted_proposal().unwrap(),
        Some((w, header.block_hash))
    );
}

#[test]
fn round_skip_off_keeps_view_and_withholds_vote() {
    let mut r = rejoin(false);
    let (origin, header) = (r.origin, r.header.clone());
    let w = header.view;
    r.deliver(origin, header);
    r.deliver_body(origin, body(95), w);
    assert_eq!(r.view(), ViewNumber::new(LOCAL_VIEW));
    assert!(r.votes().is_empty());
    assert_eq!(r.f.algorithm.block_tree.highest_view_voted().unwrap(), None);
}

/// s74 fix C (review finding 1): once the justify block of a parked header has
/// arrived through a path other than the receive (sync, block-data channel,
/// retry tick), the receive must not wait out the view deadline before the
/// loop re-checks the header: that wait is the freeze fix C removes.
#[test]
fn parked_header_keeps_the_receive_wait_short() {
    use crate::hotstuff::header_fast_path_regression_test::generic_pc;
    let keys = signing_keys(&[1, 2, 3, 4]);
    let set = validator_set(&keys);
    let me = keys[0].verifying_key();
    let probe = fixture();
    let view = (LOCAL_VIEW..)
        .map(ViewNumber::new)
        .find(|view| leader(*view, &probe) != me)
        .unwrap();
    let mut r = rejoin_at(view.int(), false);
    let origin = leader(view, &r.f);
    let justify_block = body(94);
    let block = Block::new(
        BlockHeight::new(1),
        generic_pc(ViewNumber::new(view.int() - 1), justify_block.hash, &keys, &set),
        CryptoHash::new([95; 32]),
        Data::new(vec![]),
    );
    r.deliver(origin, header_for(&block, view));
    assert!(r.votes().is_empty(), "parked: justify block unknown");
    r.f.algorithm.block_tree.insert(&justify_block, None, None).unwrap();

    // The justify block arrived through sync, which has ended.
    r.f.algorithm.block_sync_client.finish_pending_sync();
    let started = Instant::now();
    let view_info = ViewInfo::new(view, Instant::now() + Duration::from_secs(2));
    r.f.algorithm.poll_progress_and_retry(view_info, false);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "receive waited {:?} with a header parked",
        started.elapsed()
    );
}

/// s75 fix A: the rejoiner leads the survivors' view w (they entered it by
/// timeout, so no header exists). Their NewViews for w - 1 reach it after
/// reconnect; it skips to w and proposes there once.
#[test]
fn rejoining_leader_skips_to_its_view_on_new_views_and_proposes() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let me = keys[0].verifying_key();
    let probe = fixture();
    let w = (LOCAL_VIEW + 2..)
        .map(ViewNumber::new)
        .find(|view| leader(*view, &probe) == me)
        .unwrap();
    let mut r = rejoin(true);
    r.f.algorithm.app.produce = true;
    for key in &keys[1..3] {
        let new_view = NewView {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(w.int() - 1),
            highest_pc: PhaseCertificate::genesis_pc(),
        };
        r.f.messages
            .send((key.verifying_key(), ProgressMessage::HotStuffMessage(new_view.into())))
            .unwrap();
        r.poll();
    }
    assert_eq!(r.view(), w, "the pacemaker entered the view this replica leads");
    assert!(!r.f.algorithm.hotstuff.is_view_outdated(r.f.algorithm.pacemaker.query()));
    assert_eq!(r.f.algorithm.block_tree.highest_view_entered().unwrap(), w);
    let proposals = r.events.try_iter()
        .filter(|event| matches!(event, Event::Propose(p) if p.proposal.view == w))
        .count();
    assert_eq!(proposals, 1, "proposed once in w");
}

#[test]
fn leader_skip_off_keeps_view() {
    let keys = signing_keys(&[1, 2, 3, 4]);
    let me = keys[0].verifying_key();
    let probe = fixture();
    let w = (LOCAL_VIEW + 2..)
        .map(ViewNumber::new)
        .find(|view| leader(*view, &probe) == me)
        .unwrap();
    let mut r = rejoin(false);
    for key in &keys[1..4] {
        let new_view = NewView {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(w.int() - 1),
            highest_pc: PhaseCertificate::genesis_pc(),
        };
        r.f.messages
            .send((key.verifying_key(), ProgressMessage::HotStuffMessage(new_view.into())))
            .unwrap();
        r.poll();
    }
    assert_eq!(r.view(), ViewNumber::new(LOCAL_VIEW));
}

/// s75 fix A (boot): a restarted replica runs the ordinary enter_view for
/// its init view on the first loop pass (so it proposes there if it leads
/// it, and sends the NewView for init - 1). A fresh chain is unchanged.
#[test]
fn restarted_replica_enters_its_init_view_on_the_first_pass() {
    let set = validator_set(&signing_keys(&[1, 2, 3, 4]));
    let (mut tree, _) = steady_block_tree(&set);
    tree.set_highest_view_entered(ViewNumber::new(5)).unwrap();
    let (algorithm, _, _) = new_algorithm(tree);
    assert_eq!(algorithm.pacemaker.query().view, ViewNumber::new(6));
    assert!(
        algorithm.hotstuff.is_view_outdated(algorithm.pacemaker.query()),
        "the first loop pass runs enter_view(6)"
    );

    let (tree, _) = steady_block_tree(&set);
    let (algorithm, _, _) = new_algorithm(tree);
    assert!(
        !algorithm.hotstuff.is_view_outdated(algorithm.pacemaker.query()),
        "fresh chain (init view 0) unchanged"
    );
}

/// s75 fix D: a leader in a long (backed-off) view must wake for its header
/// re-send instead of sleeping in the receive until the view deadline.
#[test]
fn due_header_resend_keeps_the_receive_wait_short() {
    let mut r = rejoin(false);
    let view = r.view();
    r.f.algorithm.block_sync_client.finish_pending_sync();
    r.f.algorithm
        .hotstuff
        .arm_header_resend(header_for(&body(98), view), Instant::now() + Duration::from_millis(100));
    let started = Instant::now();
    r.f.algorithm
        .poll_progress_and_retry(ViewInfo::new(view, Instant::now() + Duration::from_secs(2)), false);
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "receive waited {:?} past a due header re-send",
        started.elapsed()
    );
}
