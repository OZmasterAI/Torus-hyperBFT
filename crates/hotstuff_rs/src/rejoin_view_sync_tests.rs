//! s74 rejoin view sync (round-skip), end to end through the algorithm loop: a
//! replica that restarted behind the survivors receives leader(w)'s header for
//! a future view w, enters w through the pacemaker and votes exactly once.
use super::body_before_expiry_tests::{fixture, Fixture};
use super::*;
use crate::events::Event;
use crate::hotstuff::header_fast_path_regression_test::{
    hotstuff_at_with_events, proposer_for, signing_keys, steady_block_tree, validator_set,
    NullNetwork,
};
use crate::hotstuff::messages::{PhaseVote, ProposalHeader};
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
    let keys = signing_keys(&[1, 2, 3, 4]);
    let (_, vss) = steady_block_tree(&validator_set(&keys));
    let mut f = fixture();
    let me = keys[0].verifying_key();
    let future = (LOCAL_VIEW + 2..)
        .map(ViewNumber::new)
        .find(|view| leader(*view, &f) != me)
        .unwrap();
    let local = ViewNumber::new(LOCAL_VIEW);
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
    r.deliver(origin, header_for(&body(97), w));
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
    r.deliver(origin, header);
    assert_eq!(r.view(), ViewNumber::new(LOCAL_VIEW));
    assert!(r.votes().is_empty());
    assert_eq!(r.f.algorithm.block_tree.highest_view_voted().unwrap(), None);
}
