//! s123 rank 2 (propose-defer-on-exec-queue): a leader whose app reports
//! [`App::proposal_backpressure`] defers its FRESH proposal — no
//! `produce_block`, nothing broadcast, `has_deferred_proposal()` set — and
//! the algorithm loop's re-entry (same view) sends it as soon as the hint
//! clears. Within [`APP_BACKPRESSURE_ESCAPE`] of the view deadline, or once
//! the max hold has passed since view entry, it proposes anyway. TC-based
//! proposals are never gated, nor is any fresh proposal in the view right
//! after a TC, and an app without the override proposes at view entry exactly
//! as before.
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
use crate::hotstuff::implementation::{
    app_backpressure_max_hold, HotStuff, APP_BACKPRESSURE_DEFAULT_MAX_HOLD, APP_BACKPRESSURE_ESCAPE,
};
use crate::hotstuff::types::PhaseCertificate;
use crate::hotstuff::vote_persist_order_test::{
    base_tree, kv_reads, replica, OrderKV, OrderLog, OrderNetwork, BROADCAST_HEADER, CHAIN_ID,
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
    assert_eq!(
        app.produced, 0,
        "a deferred leader must not call produce_block"
    );
    assert_eq!(
        headers(&lead.log),
        0,
        "nothing may be broadcast while deferred"
    );
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
    assert_eq!(
        app.produced, 1,
        "exactly one block once backpressure clears"
    );
    assert_eq!(headers(&lead.log), 1, "exactly one proposal header");
    assert!(!lead.leader.has_deferred_proposal());
}

/// Case 3: within the escape of the view deadline the leader proposes at
/// once, backpressure or not.
#[test]
fn near_deadline_proposes_despite_backpressure() {
    let near = Duration::from_millis(100);
    assert!(
        near < APP_BACKPRESSURE_ESCAPE,
        "the case must sit inside the escape"
    );
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
    assert!(
        !App::<OrderKV>::proposal_backpressure(&app),
        "trait default is false"
    );
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

/// Review fix (a): the hold is capped by the time since this node ENTERED the
/// view, not only by its own deadline. Deadlines differ across nodes (a fast
/// run banks up to 2x `max_view_time` of surplus), entry times do not, so a
/// leader can no longer outwait its followers' timeouts.
#[test]
fn hold_is_capped_by_time_since_view_entry() {
    let mut lead = leader_of_view_3();
    lead.leader
        .set_app_backpressure_max_hold(Duration::from_millis(40));
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    // The deadline is far: only the entry-time cap can release the proposal.
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 0, "inside the hold: still deferred");
    assert!(lead.leader.has_deferred_proposal());

    std::thread::sleep(Duration::from_millis(60));
    enter(&mut lead, &info, &mut app);
    assert_eq!(
        app.produced, 1,
        "past entry + max hold the leader must propose"
    );
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}

/// The hold is half of `max_view_time` (the node wires it from its pacemaker
/// config); the default before wiring matches the node's 1.2 s view time.
#[test]
fn max_hold_is_half_the_view_time() {
    assert_eq!(
        app_backpressure_max_hold(Duration::from_millis(1200)),
        Duration::from_millis(600)
    );
    assert_eq!(
        APP_BACKPRESSURE_DEFAULT_MAX_HOLD,
        Duration::from_millis(600)
    );
}

fn losing_tc(view: ViewNumber) -> TimeoutCertificate {
    TimeoutCertificate {
        chain_id: CHAIN_ID,
        view,
        signatures: SignatureSet::genesis(),
        high_tip: None,
        high_qc: None,
        high_tip_is_winner: false,
        voter_metadata: vec![],
    }
}

/// Review fix (b): in the view right after a timeout (`highest_tc.view + 1 ==
/// view`), a fresh proposal (the TC path returned `None`) is never deferred,
/// so one timeout cannot cascade into the next view.
#[test]
fn fresh_proposal_right_after_tc_is_not_deferred() {
    let mut lead = leader_of_view_3();
    let tc = losing_tc(ViewNumber::new(lead.view.int() - 1));
    lead.block_tree.set_highest_tc(&tc).expect("set highest_tc");
    lead.log.lock().unwrap().clear();

    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1, "the view after a TC must propose at once");
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}

/// An older TC (not the previous view) does not lift the deferral.
#[test]
fn older_tc_does_not_lift_deferral() {
    let mut lead = leader_of_view_3();
    let tc = losing_tc(ViewNumber::new(lead.view.int() - 2));
    lead.block_tree.set_highest_tc(&tc).expect("set highest_tc");
    lead.log.lock().unwrap().clear();

    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 0);
    assert_eq!(headers(&lead.log), 0);
    assert!(lead.leader.has_deferred_proposal());
}

/// Codex s123 review P2: while the app hint holds, the algorithm loop's
/// ~10 ms retries keep the proposal deferred WITHOUT rebuilding the proposal
/// context: no `highest_pc`, validator-set, reputation or `highest_tc` read
/// per retry (was 1 TC + 8 validator-set reads per retry on the consensus
/// thread). The view's first deferral pays them once; the release still
/// builds the proposal from fresh reads.
#[test]
fn deferring_leader_retries_read_nothing_from_the_store() {
    let mut lead = leader_of_view_3();
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert!(lead.leader.has_deferred_proposal());

    let before = kv_reads();
    for _ in 0..9 {
        enter(&mut lead, &info, &mut app);
        assert!(lead.leader.has_deferred_proposal());
    }
    assert_eq!(
        kv_reads() - before,
        0,
        "a deferring leader's retries must not read the store"
    );
    assert_eq!(app.produced, 0);
    assert_eq!(headers(&lead.log), 0);

    app.backpressure = false;
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1, "the release proposes once");
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}

/// The deadline escape still fires on a retry (the cheap retry path checks
/// the same pacing as the full one): entered 500 ms before the deadline
/// (deferred: more than the 300 ms escape), a retry 250 ms later (inside the
/// escape, before the 600 ms hold) proposes.
#[test]
fn retry_inside_the_escape_proposes() {
    let mut lead = leader_of_view_3();
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + Duration::from_millis(500));
    enter(&mut lead, &info, &mut app);
    assert!(
        lead.leader.has_deferred_proposal(),
        "outside the escape: deferred"
    );
    std::thread::sleep(Duration::from_millis(250));
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1, "inside the escape the retry must propose");
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}

/// The cheap retry path is invalidated by a pacemaker message, the only
/// writer of `highest_tc` mid-view: a TC for `view - 1` stored while the
/// leader defers (a late TC after a QC entry) lifts the deferral at the next
/// retry once the loop reports the message, as the full checks do (review
/// fix (b)). Without the message the retry stays on the cheap path.
#[test]
fn tc_stored_mid_deferral_lifts_it_after_the_pacemaker_message() {
    let mut lead = leader_of_view_3();
    let mut app = PacedApp {
        backpressure: true,
        produced: 0,
    };
    let info = ViewInfo::new(lead.view, Instant::now() + FAR);
    enter(&mut lead, &info, &mut app);
    assert!(lead.leader.has_deferred_proposal());

    let tc = losing_tc(ViewNumber::new(lead.view.int() - 1));
    lead.block_tree.set_highest_tc(&tc).expect("set highest_tc");
    lead.log.lock().unwrap().clear();
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 0, "no pacemaker message yet: still deferred");
    assert!(lead.leader.has_deferred_proposal());

    lead.leader.invalidate_app_deferral();
    enter(&mut lead, &info, &mut app);
    assert_eq!(app.produced, 1, "the view after a TC must propose at once");
    assert_eq!(headers(&lead.log), 1);
    assert!(!lead.leader.has_deferred_proposal());
}
