//! s138 (9k2 deep r4-U halt): a leader whose app refuses to build a block
//! ([`App::refuses_proposal`]) proposes nothing in that view.
//!
//! TorusApp refuses an epoch-boundary block while its execution has not
//! applied the previous boundary, with a datum-less placeholder. The leader
//! used to self-insert the placeholder, broadcast it and vote for it without a
//! data check. The placeholder is the same block for every leader on the same
//! parent (`Block::hash(height, justify, data_hash)`), and a replica that
//! proposed a block votes for an identical one from a later leader through
//! `pending_bodies`, so two refusing leaders with 2 + 2 of 5 power certified
//! it. No honest replica can insert it or a child of it: the chain halted for
//! good at height 9498.
//!
//! # RED-first
//!
//! Before the fix `refuses_proposal` was never consulted: the leader of view 3
//! broadcast the placeholder's header, inserted it and sent its own vote.

use std::time::{Duration, Instant};

use crate::app::{
    App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
};
use crate::block_tree::accessors::internal::{BlockTreeSingleton, BlockTreeWriteBatch};
use crate::hotstuff::header_fast_path_regression_test::{generic_pc, proposer_for};
use crate::hotstuff::types::PhaseCertificate;
use crate::hotstuff::vote_persist_order_test::{
    base_tree, replica, OrderKV, OrderLog, BROADCAST_HEADER, SEND,
};
use crate::pacemaker::implementation::ViewInfo;
use crate::types::block::Block;
use crate::types::data_types::{BlockHeight, CryptoHash, Data, ViewNumber};

/// Produces a datum-less placeholder while `refuse` is set, else a one-datum
/// block; declares the placeholder a refusal.
struct RefusingApp {
    refuse: bool,
    produced: u32,
}

const PLACEHOLDER_HASH: [u8; 32] = [7u8; 32];
const BUILT_HASH: [u8; 32] = [8u8; 32];

impl App<OrderKV> for RefusingApp {
    fn produce_block(&mut self, _request: ProduceBlockRequest<OrderKV>) -> ProduceBlockResponse {
        self.produced += 1;
        let (data, data_hash) = if self.refuse {
            (Data::new(vec![]), PLACEHOLDER_HASH)
        } else {
            (
                Data::new(vec![crate::types::data_types::Datum::new(vec![1, 2, 3])]),
                BUILT_HASH,
            )
        };
        ProduceBlockResponse {
            data,
            data_hash: CryptoHash::new(data_hash),
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
    fn refuses_proposal(&self, response: &ProduceBlockResponse) -> bool {
        response.data.vec().is_empty()
    }
}

/// The leader of view 3 over g <- p (highest PC: a QC on p in view 2) right
/// after `enter_view`, and the block it would propose with `data_hash`.
struct Led {
    events: Vec<&'static str>,
    block_tree: BlockTreeSingleton<OrderKV>,
    app: RefusingApp,
    leader: crate::hotstuff::implementation::HotStuff<
        crate::hotstuff::vote_persist_order_test::OrderNetwork,
    >,
    qc_p: PhaseCertificate,
    log: OrderLog,
}

fn lead_view_3(refuse: bool) -> Led {
    let (keys, set, vss, mut block_tree, log) = base_tree();
    let g = Block::new(
        BlockHeight::new(0),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([60u8; 32]),
        Data::new(vec![]),
    );
    block_tree.insert(&g, None, None).expect("insert g");
    let p = Block::new(
        BlockHeight::new(1),
        generic_pc(ViewNumber::new(1), g.hash, &keys[..3], &set),
        CryptoHash::new([61u8; 32]),
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
    // Vote inline on the own block (the production setting), so a vote
    // would show up in the log right away.
    leader.set_defer_commit_feed(true);
    let mut app = RefusingApp { refuse, produced: 0 };
    leader
        .enter_view(
            ViewInfo::new(view, Instant::now() + Duration::from_secs(3600)),
            &mut block_tree,
            &mut app,
        )
        .expect("enter_view as leader must not error");
    let events = log.lock().unwrap().clone();
    Led { events, block_tree, app, leader, qc_p, log }
}

fn own_block(qc_p: &PhaseCertificate, data_hash: [u8; 32], data: Data) -> Block {
    Block::new(BlockHeight::new(2), qc_p.clone(), CryptoHash::new(data_hash), data)
}

#[test]
fn refused_proposal_is_not_inserted_broadcast_or_voted() {
    let led = lead_view_3(true);
    assert_eq!(led.app.produced, 1, "the leader asked the app for a block");
    let placeholder = own_block(&led.qc_p, PLACEHOLDER_HASH, Data::new(vec![]));
    assert!(
        !led.events.contains(&BROADCAST_HEADER),
        "a refusal must not be broadcast; got {:?}",
        led.events
    );
    assert!(
        !led.events.contains(&SEND),
        "the leader must not vote for its refusal; got {:?}",
        led.events
    );
    assert!(
        !led.block_tree.contains(&placeholder.hash),
        "a refusal must not enter the leader's tree"
    );
    assert!(
        led.leader.pending_body(&placeholder.hash).is_none(),
        "a refusal must not count as a held body (a later identical proposal would get our vote)"
    );
    assert_ne!(
        led.block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(ViewNumber::new(3)),
        "no vote in the refused view"
    );
    assert!(
        !led.leader.has_deferred_proposal(),
        "the refused view times out; no retry loop on produce_block"
    );
    drop(led.log);
}

#[test]
fn control_a_built_block_is_proposed_and_self_voted() {
    let led = lead_view_3(false);
    let built = own_block(
        &led.qc_p,
        BUILT_HASH,
        Data::new(vec![crate::types::data_types::Datum::new(vec![1, 2, 3])]),
    );
    assert!(led.events.contains(&BROADCAST_HEADER), "got {:?}", led.events);
    assert!(led.events.contains(&SEND), "got {:?}", led.events);
    assert!(led.block_tree.contains(&built.hash), "the leader self-inserts its block");
    assert_eq!(
        led.block_tree.highest_view_voted().expect("highest_view_voted"),
        Some(ViewNumber::new(3)),
    );
}
