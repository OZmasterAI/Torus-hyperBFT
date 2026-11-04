//! Item 3 (owner idea, s140; built s142): `TORUS_EMPTY_PROPOSAL_WAIT_MS`. A leader whose app has
//! nothing to propose ([`App::nothing_to_propose`]) holds its fresh block for up to the wait,
//! instead of proposing an empty block at once, and proposes as soon as the app has something.
//! The wait is a deferred proposal (retried by the algorithm loop like the exec-backpressure
//! yield), never longer than the wait, so a quiet chain still proposes (empty) and keeps going.
//!
//! - Off (the default): an empty app proposes at once, as before.
//! - On: an empty app defers; the retry after the first action proposes it; with nothing
//!   arriving, the retry after the wait proposes the empty block.
//! - On, with something pending: proposes at once.
//!
//! Setup as `leader_feed_order_test`: `g` (height 0) and `p` (height 1) in the tree, highest PC a
//! QC on `p` in view 2, and the leader of view 3 enters it.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, proposer_for};
use crate::hotstuff::implementation::{parse_empty_proposal_wait_ms, HotStuff};
use crate::hotstuff::types::PhaseCertificate;
use crate::hotstuff::vote_persist_order_test::{base_tree, replica, OrderKV, OrderNetwork};
use crate::pacemaker::implementation::ViewInfo;
use crate::types::block::Block;
use crate::types::data_types::{BlockHeight, CryptoHash, Data, ViewNumber};

/// Counts the blocks it produces; `empty` is what `nothing_to_propose` answers.
struct WaitApp {
    produced: u32,
    empty: Cell<bool>,
}

impl WaitApp {
    fn new(empty: bool) -> Self {
        Self { produced: 0, empty: Cell::new(empty) }
    }
}

impl App<OrderKV> for WaitApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<OrderKV>) -> ProduceBlockResponse {
        self.produced += 1;
        ProduceBlockResponse {
            data: Data::new(vec![]),
            data_hash: CryptoHash::new([9u8; 32]),
            app_state_updates: None,
            validator_set_updates: None,
        }
    }
    fn validate_block(&mut self, _request: ValidateBlockRequest<OrderKV>) -> ValidateBlockResponse {
        ValidateBlockResponse::Valid { app_state_updates: None, validator_set_updates: None }
    }
    fn validate_block_for_sync(&mut self, _request: ValidateBlockRequest<OrderKV>) -> ValidateBlockResponse {
        unreachable!("no block sync in this test")
    }
    fn nothing_to_propose(&self) -> bool {
        self.empty.get()
    }
}

/// The leader of view 3 over the g <- p chain, with the given wait, before it enters the view.
struct Leader {
    block_tree: BlockTreeSingleton<OrderKV>,
    leader: HotStuff<OrderNetwork>,
    view: ViewInfo,
}

fn leader_of_view_3(wait: Option<Duration>) -> Leader {
    let (keys, set, vss, mut block_tree, log) = base_tree();
    let g = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(), CryptoHash::new([50u8; 32]), Data::new(vec![]));
    block_tree.insert(&g, None, None).expect("insert g");
    let p = Block::new(
        BlockHeight::new(1),
        generic_pc(ViewNumber::new(1), g.hash, &keys[..3], &set),
        CryptoHash::new([51u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&p, None, None).expect("insert p");
    let mut wb = BlockTreeWriteBatch::new();
    wb.set_highest_pc(&generic_pc(ViewNumber::new(2), p.hash, &keys[..3], &set)).expect("set highest_pc");
    block_tree.write(wb);

    let view = ViewNumber::new(3);
    let leader_vk = proposer_for(view, &keys, &vss, &block_tree);
    let leader_key = keys.iter().find(|k| k.verifying_key() == leader_vk).expect("leader key among fixtures").clone();
    let mut leader = replica(&leader_key, &vss, &log, view);
    leader.set_empty_proposal_wait(wait);
    Leader { block_tree, leader, view: ViewInfo::new(view, Instant::now() + Duration::from_secs(3600)) }
}

impl Leader {
    /// Enter view 3, or (once entered) the algorithm loop's retry of a deferred proposal.
    fn enter(&mut self, app: &mut WaitApp) {
        self.leader.enter_view(self.view.clone(), &mut self.block_tree, app).expect("enter_view must not error");
    }
}

#[test]
fn off_by_default_an_empty_app_proposes_at_once() {
    let mut l = leader_of_view_3(None);
    let mut app = WaitApp::new(true);
    l.enter(&mut app);
    assert_eq!(app.produced, 1, "without the wait an empty app's leader proposes at once");
    assert!(!l.leader.has_deferred_proposal());
}

#[test]
fn an_empty_app_defers_until_the_first_action() {
    let mut l = leader_of_view_3(Some(Duration::from_secs(60)));
    let mut app = WaitApp::new(true);
    l.enter(&mut app);
    assert_eq!(app.produced, 0, "nothing to propose: the leader waits");
    assert!(l.leader.has_deferred_proposal(), "the wait is a deferred proposal, retried by the loop");

    l.enter(&mut app);
    assert_eq!(app.produced, 0, "still nothing, still within the wait");

    app.empty.set(false);
    l.enter(&mut app);
    assert_eq!(app.produced, 1, "the retry after the first action proposes");
    assert!(!l.leader.has_deferred_proposal());
}

#[test]
fn the_wait_ends_with_an_empty_proposal() {
    let mut l = leader_of_view_3(Some(Duration::from_millis(20)));
    let mut app = WaitApp::new(true);
    l.enter(&mut app);
    assert_eq!(app.produced, 0, "nothing to propose: the leader waits");

    std::thread::sleep(Duration::from_millis(25));
    l.enter(&mut app);
    assert_eq!(app.produced, 1, "past the wait the leader proposes the empty block: the chain keeps going");
    assert!(!l.leader.has_deferred_proposal());
}

#[test]
fn with_something_pending_the_wait_does_not_apply() {
    let mut l = leader_of_view_3(Some(Duration::from_secs(60)));
    let mut app = WaitApp::new(false);
    l.enter(&mut app);
    assert_eq!(app.produced, 1, "something to propose: the leader proposes at once");
    assert!(!l.leader.has_deferred_proposal());
}

#[test]
fn the_env_knob_is_off_unless_a_positive_number_and_is_capped() {
    assert_eq!(parse_empty_proposal_wait_ms(None), None);
    assert_eq!(parse_empty_proposal_wait_ms(Some(String::new())), None);
    assert_eq!(parse_empty_proposal_wait_ms(Some("0".into())), None);
    assert_eq!(parse_empty_proposal_wait_ms(Some("ten".into())), None);
    assert_eq!(parse_empty_proposal_wait_ms(Some(" 15 ".into())), Some(Duration::from_millis(15)));
    assert_eq!(
        parse_empty_proposal_wait_ms(Some("5000".into())),
        Some(Duration::from_millis(100)),
        "capped at 100 ms, far below any view timeout"
    );
}
