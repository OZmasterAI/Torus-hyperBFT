//! s65 item C: with `TORUS_DEFER_COMMIT_FEED` a leader broadcasts its header and
//! casts its own vote BEFORE running the app commit feed; the default keeps the
//! pre-s65 order (feed first).
//!
//! Setup: grandparent `g` (height 0) and parent `p` (height 1, justified by a
//! QC on `g` in view 1) are in the tree and `highest_pc` is a QC on `p` in
//! view 2 — so the leader of view 3 builds on it and its
//! `block_tree.update(justify)` commits `g` (2-chain rule, consecutive views),
//! which triggers `on_committed_block(g)` through the commit feed.
//!
//! # RED-first
//!
//! Before `finish_leader_proposal` existed, `set_defer_commit_feed` did not
//! exist (compile failure) and the feed always ran before the broadcast.

use std::time::{Duration, Instant};

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, proposer_for};
use crate::hotstuff::implementation::HotStuff;
use crate::hotstuff::messages::{HotStuffMessage, ProposalHeader};
use crate::hotstuff::types::PhaseCertificate;
use crate::hotstuff::vote_persist_order_test::{
    base_tree, replica, OrderKV, OrderLog, OrderNetwork, BROADCAST_HEADER, CHAIN_ID, SEND,
};
use crate::pacemaker::implementation::ViewInfo;
use crate::types::block::Block;
use crate::types::crypto_primitives::VerifyingKey;
use crate::types::data_types::{BlockHeight, CryptoHash, Data, ViewNumber};

const FEED: &str = "commit_feed";

/// Produces empty blocks, validates everything, and logs each commit-feed
/// delivery into the shared order log.
struct LeaderApp {
    log: OrderLog,
}

impl App<OrderKV> for LeaderApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<OrderKV>) -> ProduceBlockResponse {
        ProduceBlockResponse {
            data: Data::new(vec![]),
            data_hash: CryptoHash::new([9u8; 32]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<OrderKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid {
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block_for_sync(
        &mut self,
        _request: ValidateBlockRequest<OrderKV>,
    ) -> ValidateBlockResponse {
        unreachable!("sync validation is not reached on the leader path under test")
    }
    fn on_committed_block(&mut self, _block: &Block, _committed_hash: CryptoHash) {
        self.log.lock().unwrap().push(FEED);
    }
}

/// The leader of view 3 right after `enter_view`, with what it logged.
struct Led {
    events: Vec<&'static str>,
    block_tree: BlockTreeSingleton<OrderKV>,
    leader: HotStuff<OrderNetwork>,
    leader_vk: VerifyingKey,
    log: OrderLog,
    /// The leader's own proposal, rebuilt deterministically (LeaderApp always
    /// produces an empty body with data hash [9; 32]).
    own_block: Block,
}

/// Enter view 3 as its leader over the g <- p chain.
fn lead_view_3(defer: bool) -> Led {
    let (keys, set, vss, mut block_tree, log) = base_tree();
    let g = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([50u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&g, None, None).expect("insert g");
    let p = Block::new(
        BlockHeight::new(1),
        generic_pc(ViewNumber::new(1), g.hash, &keys[..3], &set),
        CryptoHash::new([51u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&p, None, None).expect("insert p");
    let qc_p = generic_pc(ViewNumber::new(2), p.hash, &keys[..3], &set);
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&qc_p).expect("set highest_pc");
    block_tree.write(wb);
    log.lock().unwrap().clear();

    let view = ViewNumber::new(3);
    let leader_vk = proposer_for(view, &keys, &vss, &block_tree);
    let leader_key = keys
        .iter()
        .find(|k| k.verifying_key() == leader_vk)
        .expect("leader key among fixtures")
        .clone();
    let mut leader = replica(&leader_key, &vss, &log, view);
    leader.set_defer_commit_feed(defer);
    let mut app = LeaderApp { log: log.clone() };
    leader
        .enter_view(
            ViewInfo::new(view, Instant::now() + Duration::from_secs(3600)),
            &mut block_tree,
            &mut app,
        )
        .expect("enter_view as leader must not error");
    let events = log.lock().unwrap().clone();
    let own_block = Block::new(
        BlockHeight::new(2),
        qc_p,
        CryptoHash::new([9u8; 32]),
        Data::new(vec![]),
    );
    Led {
        events,
        block_tree,
        leader,
        leader_vk,
        log,
        own_block,
    }
}

fn index_of(events: &[&str], what: &str) -> usize {
    events
        .iter()
        .position(|e| *e == what)
        .unwrap_or_else(|| panic!("missing {what} in {events:?}"))
}

#[test]
fn deferred_feed_broadcasts_and_self_votes_before_commit_feed() {
    let led = lead_view_3(true);
    let header = index_of(&led.events, BROADCAST_HEADER);
    let vote = index_of(&led.events, SEND);
    let feed = index_of(&led.events, FEED);
    assert!(
        header < vote && vote < feed,
        "expected header, own vote, then feed; got {:?}",
        led.events
    );
    assert_eq!(
        led.block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(ViewNumber::new(3)),
        "the leader must have voted on its own block inline"
    );
}

#[test]
fn default_order_runs_commit_feed_before_broadcast() {
    let led = lead_view_3(false);
    assert!(
        index_of(&led.events, FEED) < index_of(&led.events, BROADCAST_HEADER),
        "flag OFF must keep the pre-s65 order; got {:?}",
        led.events
    );
    assert!(
        !led.events.contains(&SEND),
        "flag OFF: the leader votes later, via the loopback header; got {:?}",
        led.events
    );
}

#[test]
fn loopback_copy_of_inline_header_is_dropped() {
    let mut led = lead_view_3(true);
    assert!(
        led.block_tree.contains(&led.own_block.hash),
        "the rebuilt block must be the leader's own proposal"
    );
    let header = ProposalHeader {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(3),
        block_hash: led.own_block.hash,
        height: led.own_block.height,
        data_hash: led.own_block.data_hash,
        justify: led.own_block.justify.clone(),
        tc: None,
        nec: None,
        has_validator_set_updates: false,
    };
    led.log.lock().unwrap().clear();
    let mut app = LeaderApp {
        log: led.log.clone(),
    };
    led.leader
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(header),
            &led.leader_vk,
            &mut led.block_tree,
            &mut app,
        )
        .expect("loopback header must not error");
    assert!(
        led.log.lock().unwrap().is_empty(),
        "the loopback copy must be a silent no-op; got {:?}",
        led.log.lock().unwrap()
    );
}
