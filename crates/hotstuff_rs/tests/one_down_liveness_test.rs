//! s84 liveness: 3 validators at stake 2:2:1 with the power-1 validator DOWN.
//!
//! # Quorum math
//!
//! `ValidatorSet::quorum` = `floor(2 * total / 3) + 1` (strictly more than 2/3 of the power).
//! At the devnet stakes 2:2:1 (`stake_to_power` = whole TRS: 2_000_000 / 2_000_000 / 1_000_000,
//! total 5_000_000) the quorum is 3_333_334, so the two power-2 validators (4_000_000) form QCs
//! and TCs without the power-1 validator: the chain MUST stay live while it is down. (At 1:1:1 the
//! quorum is 3 of 3, so any one validator down stalls the chain by design.)
//!
//! # The bug (s83 drill)
//!
//! With powers in the millions, `select_leader` is plain round-robin over all three validators for
//! every view below 3_000_000, so the dead validator `D` leads every third view. Phase votes for
//! view `w` go only to `leader(w + 1)`. Votes for the block proposed in the view BEFORE `D`'s view
//! are sent to `D` and lost, so that block is never certified. The 2-chain commit rule needs QCs in
//! two consecutive views; in each 3-view cycle only one block (the one whose next leader is alive)
//! gets a QC, so no two consecutive-view QCs ever exist and NOTHING commits. The chain keeps
//! extending (one certified block per cycle) but the committed height stays at 0.
//!
//! The test runs only the two power-2 validators and asserts that they commit.

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

/// Short views so the dead leader's view times out in about a second.
const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);

/// Committed height both live validators must reach.
const TARGET_HEIGHT: u64 = 4;

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

#[test]
fn two_of_three_at_2_2_1_stakes_keep_committing_with_the_minority_down() {
    let keypairs: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let powers = [2_000_000u64, 2_000_000, 1_000_000];

    let mut vs_updates = ValidatorSetUpdates::new();
    for (kp, p) in keypairs.iter().zip(powers) {
        vs_updates.insert(kp.verifying_key(), Power::new(p));
    }
    let init_as = NumberApp::initial_app_state();

    // The network knows all three keys; the power-1 validator's stub is dropped (never started),
    // so every message sent to it is lost — exactly a stopped node.
    let mut stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));
    stubs.truncate(2);

    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .take(2)
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
        &format!("both live validators (4M of 5M power) to commit height >= {TARGET_HEIGHT}"),
        || {
            nodes
                .iter()
                .all(|n| n.committed_height().unwrap_or(0) >= TARGET_HEIGHT)
        },
        || describe(&nodes),
    );
}
