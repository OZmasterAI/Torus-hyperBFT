//! docs/plans/tc-required-and-tip-signing.md: with VALIDPROPOSAL V1 enforced
//! (a test-only switch in phase A, always on from phase B), the chain stays
//! live with one validator down.
//!
//! RED-first: before phase A a leader entering view v after a timeout rarely
//! held TC(v-1) (the pacemaker dropped timeout votes for the view it had just
//! left) and proposed without it, so strict validators rejected every
//! timeout-view proposal and the s84 one-down cluster never committed (the
//! `exp/tc-require-tc-probe` wedge: committed=None after 120 s).
//!
//! Two shapes:
//! - 2:2:1 stakes, the power-1 validator down (the `one_down_liveness_test`
//!   cluster); it leads every third view.
//! - 4 validators of equal power, one down: the live three are the exact
//!   quorum, and the dead one leads every fourth view. Phase B (§6 "4 nodes
//!   with one down, at each leader slot"): each of the four keys is the down
//!   one in turn.

use std::time::Duration;

use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);
const TARGET_HEIGHT: u64 = 4;

fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{i}{{committed={:?}, highest_pc={:?}, view={}, tc_less_proposals={}}}",
                n.committed_height(),
                n.highest_pc_height(),
                n.highest_view_entered(),
                n.tc_less_proposals(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Start `powers.len()` strict validators, of which those in `down` never
/// run, and wait for every live one to commit `TARGET_HEIGHT`.
fn strict_cluster_commits_with_some_down(powers: &[u64], down: &[usize]) {
    let keypairs: Vec<SigningKey> =
        (1..=powers.len() as u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for (kp, p) in keypairs.iter().zip(powers) {
        vs_updates.insert(kp.verifying_key(), Power::new(*p));
    }
    let init_as = NumberApp::initial_app_state();

    // The down validators' stubs are dropped (never started): every message
    // sent to them is lost, exactly a stopped node.
    let stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));

    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .enumerate()
        .filter(|(i, _)| !down.contains(i))
        .map(|(_, (kp, net))| {
            Node::new_with_tc_required(kp, net, init_as.clone(), vs_updates.clone(), MAX_VIEW_TIME)
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        &format!("every live strict validator to commit height >= {TARGET_HEIGHT}"),
        || {
            nodes
                .iter()
                .all(|n| n.committed_height().unwrap_or(0) >= TARGET_HEIGHT)
        },
        || describe(&nodes),
    );
    assert!(
        nodes.iter().all(|n| n.tc_less_proposals() == 0),
        "a leader proposed without QC(v-1) or TC(v-1): {}",
        describe(&nodes)
    );
}

#[test]
fn strict_v1_two_of_three_at_2_2_1_stakes_keep_committing_with_the_minority_down() {
    strict_cluster_commits_with_some_down(&[2_000_000, 2_000_000, 1_000_000], &[2]);
}

/// docs/plans/tc-required-and-tip-signing.md §6: 4 nodes with one down, at
/// each leader slot (the down validator is the 1st, 2nd, 3rd, 4th key, so it
/// precedes a different live leader each time).
#[test]
fn strict_v1_three_of_four_equal_validators_keep_committing_with_one_down() {
    strict_cluster_commits_with_some_down(&[1, 1, 1, 1], &[3]);
}

#[test]
fn strict_v1_four_equal_validators_first_slot_down() {
    strict_cluster_commits_with_some_down(&[1, 1, 1, 1], &[0]);
}

#[test]
fn strict_v1_four_equal_validators_second_slot_down() {
    strict_cluster_commits_with_some_down(&[1, 1, 1, 1], &[1]);
}

#[test]
fn strict_v1_four_equal_validators_third_slot_down() {
    strict_cluster_commits_with_some_down(&[1, 1, 1, 1], &[2]);
}
