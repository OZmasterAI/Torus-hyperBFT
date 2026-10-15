//! s123 rank 2 (propose-defer-on-exec-queue): a leader whose app reports
//! [`App::proposal_backpressure`] defers its FRESH proposal — no
//! `produce_block`, nothing broadcast, `has_deferred_proposal()` set — and
//! the algorithm loop's re-entry (same view) sends it as soon as the hint
//! clears. Within [`APP_BACKPRESSURE_ESCAPE`] of the view deadline it
//! proposes anyway. TC-based proposals are never gated, and an app without
//! the override proposes at view entry exactly as before.
//!
//! Setup (the chain of `leader_feed_order_test`): grandparent `g` (height 0)
//! and parent `p` (height 1, justified by a QC on `g` in view 1) are in the
//! tree and `highest_pc` is a QC on `p` in view 2, so the leader of view 3
//! builds a fresh block on `p`.
//!
//! # RED-first
//!
//! Before the hook existed, `App::proposal_backpressure` and
//! `APP_BACKPRESSURE_ESCAPE` did not exist (compile failure), and a leader
//! always produced at view entry whatever its app's state.

use std::time::{Duration, Instant};

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, proposer_for};
use crate::hotstuff::implementation::{HotStuff, APP_BACKPRESSURE_ESCAPE};
use crate::hotstuff::types::PhaseCertificate;
use crate::hotstuff::vote_persist_order_test::{
    base_tree, replica, OrderKV, OrderLog, OrderNetwork, BROADCAST_HEADER, CHAIN_ID,
};
use crate::pacemaker::implementation::ViewInfo;
use crate::pacemaker::types::{TimeoutCertificate, TipInfo};
use crate::types::block::Block;
use crate::types::data_types::{BlockHeight, CryptoHash, Data, SignatureSet, ViewNumber};

/// Far enough that the escape never fires on its own in a test.
const FAR: Duration = Duration::from_secs(3600);

fn empty_block() -> ProduceBlockResponse {
    ProduceBlockResponse {
        data: Data::new(vec![]),
        data_hash: CryptoHash::new([9u8; 32]),
        app_state_updates: None,
        validator_set_updates: None,
    }
}

/// Produces empty blocks (counted), validates everything, and reports
/// backpressure from a flag the test flips between calls (stands in for
/// torus-consensus's exec queue depth).
struct PacedApp {
    backpressure: bool,
    produced: usize,
}

impl App<OrderKV> for PacedApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<OrderKV>) -> ProduceBlockResponse {
        self.produced += 1;
        empty_block()
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
    fn proposal_backpressure(&self) -> bool {
        self.backpressure
    }
}

/// The same app WITHOUT the override: the trait default must never defer.
struct PlainApp {
    produced: usize,
}

impl App<OrderKV> for PlainApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<OrderKV>) -> ProduceBlockResponse {
        self.produced += 1;
        empty_block()
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
}

/// The leader of view 3 over the g <- p chain, before `enter_view`.
struct Lead {
    block_tree: BlockTreeSingleton<OrderKV>,
    leader: HotStuff<OrderNetwork>,
    log: OrderLog,
    view: ViewNumber,
    /// The parent of the leader's fresh block (the TC case re-proposes it).
    p: Block,
}

fn leader_of_view_3() -> Lead {
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
    // Pin the env-read feed-order flags: only the proposal decision is under test.
    leader.set_defer_commit_feed(false);
    leader.set_defer_parent_feed(false);
    Lead {
        block_tree,
        leader,
        log,
        view,
        p,
    }
}

/// `enter_view(info)` as the leader; the algorithm loop makes the same call
/// (same view) on every ~10 ms poll while a proposal is deferred.
fn enter(lead: &mut Lead, info: &ViewInfo, app: &mut impl App<OrderKV>) {
    lead.leader
        .enter_view(info.clone(), &mut lead.block_tree, app)
        .expect("enter_view as leader must not error");
}

fn headers(log: &OrderLog) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|e| **e == BROADCAST_HEADER)
        .count()
}

/// Case 1: backpressure on, deadline far: deferred, nothing produced or sent.
#[test]
fn backpressure_defers_fresh_proposal() {
    let mut lead = leader_of_view_3();
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 0, "a deferred leader must not call produce_block");
    assert_eq!(headers(&lead.log), 0, "nothing may be broadcast while deferred");
    assert!(
        lead.leader.has_deferred_proposal(),
        "the proposal must be deferred (retried by the loop), not abandoned"
    );
}

/// Case 2: the loop's retry keeps it deferred while the hint holds, and
/// sends exactly one proposal once it clears.
#[test]
fn deferred_proposal_goes_out_once_backpressure_clears() {
    let mut lead = leader_of_view_3();
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert!(lead.leader.has_deferred_proposal());

    // A poll while still backpressured: still deferred, still nothing built.
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 0);
    assert_eq!(headers(&lead.log), 0);
    assert!(lead.leader.has_deferred_proposal());

    // The executor caught up: the next poll proposes.
    app.backpressure = false;
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1, "exactly one block once backpressure clears");
    assert_eq!(headers(&lead.log), 1, "exactly one proposal header");
    assert!(!lead.leader.has_deferred_proposal());
}

/// Case 3: within the escape of the view deadline the leader proposes at
/// once, backpressure or not.
#[test]
fn near_deadline_proposes_despite_backpressure() {
    let near = Duration::from_millis(100);
    assert!(near < APP_BACKPRESSURE_ESCAPE, "the case must sit inside the escape");
    let mut lead = leader_of_view_3();
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + near);
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1, "the deadline escape must produce");
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}

/// Case 4: an app without the override proposes at view entry, as before.
#[test]
fn default_app_never_defers() {
    let mut lead = leader_of_view_3();
    let mut app = PlainApp { produced: 0 };
    assert!(!App::<OrderKV>::proposal_backpressure(&app), "trait default is false");
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1);
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}

/// Case 5: a TC for view - 1 whose high tip wins (and is in the tree) makes
/// the leader re-propose that block; backpressure must not gate it.
#[test]
fn tc_based_proposal_is_not_gated() {
    let mut lead = leader_of_view_3();
    let tc = TimeoutCertificate {
        chain_id: CHAIN_ID,
        view: ViewNumber::new(lead.view.int() - 1),
        signatures: SignatureSet::genesis(),
        high_tip: Some(TipInfo {
            block_hash: lead.p.hash,
            block_height: lead.p.height,
            block_justify: lead.p.justify.clone(),
            block_data_hash: lead.p.data_hash,
            view: ViewNumber::new(2),
        }),
        high_qc: None,
        high_tip_is_winner: true,
        voter_metadata: vec![],
    };
    lead.block_tree.set_highest_tc(&tc).expect("set highest_tc");
    lead.log.lock().unwrap().clear();

    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert_eq!(headers(&lead.log), 1, "the TC re-proposal must go out");
    assert_eq!(app.produced, 0, "a TC re-proposal builds no new block");
    assert!(!lead.leader.has_deferred_proposal());
}
