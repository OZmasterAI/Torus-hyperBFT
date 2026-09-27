//! s72 fix D: with `TORUS_DEFER_PARENT_FEED` the next leader, whose proposal
//! is deferred because its parent body is missing, does NOT run the app
//! commit feed when that body is inserted. The retried proposal broadcasts
//! its header first (item C's order) and the feed runs after it; a feed left
//! pending is delivered by `run_pending_commit_feed`. The default keeps
//! today's order (feed inside the body insert).
//!
//! Setup: g (height 0) <- p (height 1, QC(g)@v1) <- q (height 2, QC(p)@v2).
//! g and p are in the tree, q is not, and `highest_pc` is QC(q)@v3. Inserting
//! q runs `update(QC(p))`, which commits g; the leader of view 4 then builds
//! on QC(q)@v3, which commits p.
//!
//! # RED-first
//!
//! Before fix D, `set_defer_parent_feed` and `run_pending_commit_feed` did not
//! exist (compile failure), and the feed always ran inside the body insert.

use std::time::{Duration, Instant};

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, proposer_for};
use crate::hotstuff::implementation::HotStuff;
use crate::hotstuff::messages::{BlockDataResponse, HotStuffMessage, ProposalHeader};
use crate::hotstuff::types::PhaseCertificate;
use crate::hotstuff::vote_persist_order_test::{
    base_tree, replica, OrderKV, OrderLog, OrderNetwork, BROADCAST_HEADER, CHAIN_ID,
};
use crate::pacemaker::implementation::ViewInfo;
use crate::types::block::Block;
use crate::types::crypto_primitives::{SigningKey, VerifyingKey};
use crate::types::data_types::{BlockHeight, CryptoHash, Data, ViewNumber};
use crate::types::validator_set::ValidatorSetState;

const FEED: &str = "commit_feed";

/// Produces empty blocks, validates everything, and logs each commit-feed
/// delivery into the shared order log.
struct FeedApp {
    log: OrderLog,
}

impl App<OrderKV> for FeedApp {
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
        unreachable!("sync validation is not reached on the paths under test")
    }
    fn on_committed_block(&mut self, _block: &Block, _committed_hash: CryptoHash) {
        self.log.lock().unwrap().push(FEED);
    }
}

struct Chain {
    keys: Vec<SigningKey>,
    vss: ValidatorSetState,
    block_tree: BlockTreeSingleton<OrderKV>,
    log: OrderLog,
    /// Height 2, justified by QC(p)@v2; NOT in the tree.
    q: Block,
}

/// g <- p in the tree, q built but not inserted.
fn chain() -> Chain {
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
    let q = Block::new(
        BlockHeight::new(2),
        generic_pc(ViewNumber::new(2), p.hash, &keys[..3], &set),
        CryptoHash::new([52u8; 32]),
        Data::new(vec![]),
    );
    let qc_q = generic_pc(ViewNumber::new(3), q.hash, &keys[..3], &set);
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&qc_q).expect("set highest_pc");
    block_tree.write(wb);
    log.lock().unwrap().clear();
    Chain {
        keys,
        vss,
        block_tree,
        log,
        q,
    }
}

fn view_info(view: u64) -> ViewInfo {
    ViewInfo::new(
        ViewNumber::new(view),
        Instant::now() + Duration::from_secs(3600),
    )
}

/// The leader of view 4, after entering view 4 without q (proposal deferred)
/// and then receiving q's body.
struct Waiting {
    chain: Chain,
    leader: HotStuff<OrderNetwork>,
    app: FeedApp,
    /// Log entries produced by the body insert alone.
    at_insert: Vec<&'static str>,
}

fn leader_receives_parent(defer_parent_feed: bool) -> Waiting {
    let mut chain = chain();
    let leader_vk = proposer_for(ViewNumber::new(4), &chain.keys, &chain.vss, &chain.block_tree);
    let leader_key = key_of(&chain.keys, &leader_vk);
    let mut leader = replica(&leader_key, &chain.vss, &chain.log, ViewNumber::new(4));
    leader.set_defer_parent_feed(defer_parent_feed);
    let mut app = FeedApp {
        log: chain.log.clone(),
    };
    leader
        .enter_view(view_info(4), &mut chain.block_tree, &mut app)
        .expect("enter_view without the parent body must not error");
    assert!(
        leader.has_deferred_proposal(),
        "the leader must defer its proposal while q's body is missing"
    );
    assert!(!chain.block_tree.contains(&chain.q.hash));

    chain.log.lock().unwrap().clear();
    let peer = other_than(&chain.keys, &leader_vk);
    leader
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: ViewNumber::new(3),
                block: chain.q.clone(),
            }),
            &peer,
            &mut chain.block_tree,
            &mut app,
        )
        .expect("body response must not error");
    assert!(
        chain.block_tree.contains(&chain.q.hash),
        "q's body must be inserted"
    );
    let at_insert = chain.log.lock().unwrap().clone();
    Waiting {
        chain,
        leader,
        app,
        at_insert,
    }
}

/// Retry the deferred proposal as the algorithm loop does (step 4), then flush.
fn retry_and_flush(w: &mut Waiting) -> Vec<&'static str> {
    w.leader
        .enter_view(view_info(4), &mut w.chain.block_tree, &mut w.app)
        .expect("proposal retry must not error");
    assert!(!w.leader.has_deferred_proposal(), "the retry must propose");
    w.leader
        .run_pending_commit_feed(&mut w.chain.block_tree, &mut w.app);
    w.chain.log.lock().unwrap().clone()
}

fn key_of(keys: &[SigningKey], vk: &VerifyingKey) -> SigningKey {
    keys.iter()
        .find(|k| k.verifying_key() == *vk)
        .expect("key among fixtures")
        .clone()
}

fn other_than(keys: &[SigningKey], vk: &VerifyingKey) -> VerifyingKey {
    keys.iter()
        .map(|k| k.verifying_key())
        .find(|k| k != vk)
        .expect("another validator")
}

fn count(events: &[&str], what: &str) -> usize {
    events.iter().filter(|e| **e == what).count()
}

fn index_of(events: &[&str], what: &str) -> usize {
    events
        .iter()
        .position(|e| *e == what)
        .unwrap_or_else(|| panic!("missing {what} in {events:?}"))
}

#[test]
fn deferred_parent_feed_runs_after_the_retried_broadcast() {
    let mut w = leader_receives_parent(true);
    assert_eq!(
        count(&w.at_insert, FEED),
        0,
        "flag ON: inserting the awaited parent must not run the feed; got {:?}",
        w.at_insert
    );
    let events = retry_and_flush(&mut w);
    assert!(
        index_of(&events, BROADCAST_HEADER) < index_of(&events, FEED),
        "flag ON: the header must go out before any commit feed; got {events:?}"
    );
    assert_eq!(
        count(&events, FEED),
        2,
        "g and p must each be fed exactly once; got {events:?}"
    );
}

#[test]
fn pending_parent_feed_is_flushed_without_a_retry() {
    let mut w = leader_receives_parent(true);
    assert_eq!(count(&w.at_insert, FEED), 0, "got {:?}", w.at_insert);
    assert!(w.leader.has_pending_commit_feed());
    w.leader
        .run_pending_commit_feed(&mut w.chain.block_tree, &mut w.app);
    assert!(!w.leader.has_pending_commit_feed());
    assert_eq!(
        count(&w.chain.log.lock().unwrap(), FEED),
        1,
        "the flush must deliver g"
    );
    w.leader
        .run_pending_commit_feed(&mut w.chain.block_tree, &mut w.app);
    assert_eq!(
        count(&w.chain.log.lock().unwrap(), FEED),
        1,
        "a second flush must be a no-op"
    );
}

#[test]
fn default_order_feeds_inside_the_parent_insert() {
    let mut w = leader_receives_parent(false);
    assert_eq!(
        count(&w.at_insert, FEED),
        1,
        "flag OFF: the body insert feeds g as today; got {:?}",
        w.at_insert
    );
    let events = retry_and_flush(&mut w);
    let last_feed = events.iter().rposition(|e| *e == FEED).expect("a feed");
    assert!(
        last_feed < index_of(&events, BROADCAST_HEADER),
        "flag OFF: today's order (every feed before the broadcast); got {events:?}"
    );
    assert_eq!(count(&events, FEED), 2, "got {events:?}");
}

#[test]
fn follower_body_insert_still_feeds_inline_with_flag_on() {
    let mut chain = chain();
    let leader3 = proposer_for(ViewNumber::new(3), &chain.keys, &chain.vss, &chain.block_tree);
    let follower_key = chain
        .keys
        .iter()
        .find(|k| k.verifying_key() != leader3)
        .expect("a follower")
        .clone();
    let mut follower = replica(&follower_key, &chain.vss, &chain.log, ViewNumber::new(3));
    follower.set_defer_parent_feed(true);
    let mut app = FeedApp {
        log: chain.log.clone(),
    };
    let q = chain.q.clone();
    follower
        .on_receive_msg(
            HotStuffMessage::ProposalHeader(ProposalHeader {
                chain_id: CHAIN_ID,
                view: ViewNumber::new(3),
                block_hash: q.hash,
                height: q.height,
                data_hash: q.data_hash,
                justify: q.justify.clone(),
                tc: None,
                nec: None,
                has_validator_set_updates: false,
            }),
            &leader3,
            &mut chain.block_tree,
            &mut app,
        )
        .expect("header must not error");
    assert!(!follower.has_deferred_proposal());
    assert!(!follower.has_pending_commit_feed());
    chain.log.lock().unwrap().clear();
    follower
        .on_receive_msg(
            HotStuffMessage::BlockDataResponse(BlockDataResponse {
                view: ViewNumber::new(3),
                block: q.clone(),
            }),
            &leader3,
            &mut chain.block_tree,
            &mut app,
        )
        .expect("body response must not error");
    assert!(chain.block_tree.contains(&q.hash), "q must be inserted");
    assert_eq!(
        count(&chain.log.lock().unwrap(), FEED),
        1,
        "no deferred proposal: the feed runs inside the insert, as today"
    );
}

/// Deferred proposal, but the inserted body is an intermediate ancestor, not
/// `highest_pc.block`: its commit is fed inline; only the awaited block's
/// insert (drained right after) skips the feed.
///
/// g <- p <- q <- r, g and p in the tree, q and r missing, highest_pc QC(r)@v4,
/// leader of view 5. r arrives first and parks (q missing); q then inserts
/// (commits g, not awaited) and drains r (commits p, awaited).
#[test]
fn only_the_awaited_parent_insert_skips_the_feed() {
    let Chain {
        keys,
        vss,
        mut block_tree,
        log,
        q,
    } = chain();
    let set = vss.committed_validator_set().clone();
    let r = Block::new(
        BlockHeight::new(3),
        generic_pc(ViewNumber::new(3), q.hash, &keys[..3], &set),
        CryptoHash::new([53u8; 32]),
        Data::new(vec![]),
    );
    let qc_r = generic_pc(ViewNumber::new(4), r.hash, &keys[..3], &set);
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&qc_r).expect("set highest_pc");
    block_tree.write(wb);

    let leader_vk = proposer_for(ViewNumber::new(5), &keys, &vss, &block_tree);
    let mut leader = replica(&key_of(&keys, &leader_vk), &vss, &log, ViewNumber::new(5));
    leader.set_defer_parent_feed(true);
    let mut app = FeedApp { log: log.clone() };
    leader
        .enter_view(view_info(5), &mut block_tree, &mut app)
        .expect("enter_view");
    assert!(leader.has_deferred_proposal());
    let peer = other_than(&keys, &leader_vk);
    for (view, block) in [(4, r.clone()), (3, q.clone())] {
        leader
            .on_receive_msg(
                HotStuffMessage::BlockDataResponse(BlockDataResponse {
                    view: ViewNumber::new(view),
                    block,
                }),
                &peer,
                &mut block_tree,
                &mut app,
            )
            .expect("body response");
    }
    assert!(block_tree.contains(&q.hash) && block_tree.contains(&r.hash));
    assert_eq!(
        count(&log.lock().unwrap(), FEED),
        1,
        "g (committed by q's insert) is fed inline; p's feed waits"
    );
    assert!(leader.has_pending_commit_feed());
    leader.run_pending_commit_feed(&mut block_tree, &mut app);
    assert_eq!(count(&log.lock().unwrap(), FEED), 2);
}
