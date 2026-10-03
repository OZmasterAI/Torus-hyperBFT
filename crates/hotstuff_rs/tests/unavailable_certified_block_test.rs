//! s84 wedge theory, data-availability variant: a CERTIFIED block whose body no
//! other validator can obtain.
//!
//! Replicas used to phase-vote on a proposal HEADER and fetch the body
//! afterwards, so a QC could form on a block only its proposer holds. If the
//! proposer never serves that body (withheld by a faulty leader, or lost in a
//! crash before it was served), every later proposal had to extend the QC, and
//! the other validators could neither fetch the block by hash ("justify fetch
//! exhausted" after the retry budget) nor sync it (block sync serves committed
//! blocks only, "made no progress"): the s83 drill wedge signature.
//!
//! 4 validators (quorum 3); once the cluster commits, every body sent by node 0
//! is dropped. Three correct validators hold a quorum, so the chain must stay
//! live.
//!
//! # Status: GREEN since s84 "vote after body"
//!
//! A replica now phase-votes only once it holds the body (`App::check_block_data`),
//! so node 0's blocks get no QC (only node 0 holds them) and its views time out,
//! while nodes 1-3 keep certifying and committing their own blocks.
//!
//! Before (main abac292): nodes 1-3 stopped at committed 5 with an unknown
//! highest-PC block for 120 s, logging the s83 lines (about 320 "justify fetch
//! exhausted", 290 "made no progress", 320 header drops with
//! `justify_block_known=false`), while node 0 committed 7: blocks no other
//! validator held were committed.

use std::time::Duration;

use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network_with_filter,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
    signature_log::{self, signature},
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_VIEW_TIME: Duration = Duration::from_millis(2000);

fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{i}{{committed={:?}, highest_pc={:?}, view={}}}",
                n.committed_height(),
                n.highest_pc_height(),
                n.highest_view_entered(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lowest committed height among nodes 1..=3 (the validators that still serve bodies).
fn others_min_committed(nodes: &[Node]) -> u64 {
    nodes[1..]
        .iter()
        .map(|n| n.committed_height().unwrap_or(0))
        .min()
        .unwrap_or(0)
}

#[test]
fn certified_block_with_unavailable_body_does_not_wedge_the_chain() {
    signature_log::install();

    let keypairs: Vec<SigningKey> = (1..=4u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1));
    }
    let init_as = NumberApp::initial_app_state();
    let withholder = keypairs[0].verifying_key();

    let (stubs, filter) = mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));
    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_max_view_time(kp, net, init_as.clone(), vs_updates.clone(), MAX_VIEW_TIME)
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        "baseline: nodes 1-3 commit height >= 2",
        || others_min_committed(&nodes) >= 2,
        || describe(&nodes),
    );
    filter.withhold_bodies_from(vec![withholder]);
    let base = others_min_committed(&nodes);
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    // Liveness: the three validators that serve bodies are a quorum. The budget
    // exceeds the 60 s block-sync trigger timeout, so sync gets its chance too.
    let target = base + 4;
    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        &format!("nodes 1-3 to commit height >= {target} while node 0's bodies are unavailable"),
        || others_min_committed(&nodes) >= target,
        || format!("{} | {}", describe(&nodes), signature()),
    );
    println!("live past the unavailable bodies: {} | {}", describe(&nodes), signature());
}
