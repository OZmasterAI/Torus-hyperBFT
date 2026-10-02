//! s84 wedge theory: a CERTIFIED block that the other validators find INVALID.
//!
//! Replicas vote for a proposal once they hold its body and the body passes
//! `App::check_block_data` (s84 option A), but before `App::validate_block`
//! runs on it. If `validate_block` then rejects the block for something inside
//! it (one bad transaction), the block is certified and only its proposer
//! (which self-inserts without validating) holds it in its tree: every later
//! proposal must extend it, and the rejecting replicas can neither insert it
//! ("justify fetch exhausted") nor sync it. The s83 drill wedge at height 5630
//! carried exactly these log lines.
//!
//! s84 decision 1 (a): a transaction that fails its validity check never makes
//! the block invalid. The app executes the block, skips that transaction
//! deterministically (no state change) and records it as skipped.
//!
//! The test runs 4 validators (quorum 3). Every node submits one `Increment`
//! and one `Invalid` transaction. RED while `NumberApp::validate_block` rejects
//! a block holding an `Invalid` transaction (the chain wedges); GREEN once the
//! app skips and records it.

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
    signature_log::{self, signature},
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_VIEW_TIME: Duration = Duration::from_millis(2000);
const TARGET_HEIGHT: u64 = 7;

fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{i}{{committed={:?}, highest_pc={:?}, view={}, number={}, skipped={:?}}}",
                n.committed_height(),
                n.highest_pc_height(),
                n.highest_view_entered(),
                n.number(),
                n.skipped_transactions(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn certified_block_with_an_invalid_transaction_does_not_wedge_the_chain() {
    signature_log::install();

    let keypairs: Vec<SigningKey> = (1..=4u8)
        .map(|i| SigningKey::from_bytes(&[i; 32]))
        .collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1));
    }
    let init_as = NumberApp::initial_app_state();

    let stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));
    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_max_view_time(
                kp,
                net,
                init_as.clone(),
                vs_updates.clone(),
                MAX_VIEW_TIME,
            )
        })
        .collect();
    let mut invalid_ids = Vec::new();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
        invalid_ids.push(node.submit_transaction(NumberAppTransaction::Invalid));
    }

    // Liveness: every replica commits past the blocks carrying the invalid
    // transactions, and every valid transaction executes exactly once.
    // The budget exceeds the 60 s block-sync trigger timeout, so sync gets its
    // chance too.
    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        &format!("every replica to commit height >= {TARGET_HEIGHT} and apply all 4 increments"),
        || {
            nodes
                .iter()
                .all(|n| n.committed_height().unwrap_or(0) >= TARGET_HEIGHT && n.number() == 4)
        },
        || format!("{} | {}", describe(&nodes), signature()),
    );

    // Every replica recorded at least one invalid transaction as skipped, and
    // only invalid transactions (a skipped valid one would change `number`).
    for (i, node) in nodes.iter().enumerate() {
        let skipped = node.skipped_transactions();
        assert!(
            !skipped.is_empty(),
            "n{i}: no skipped transaction recorded | {}",
            describe(&nodes)
        );
        assert!(
            skipped.iter().all(|id| invalid_ids.contains(id)),
            "n{i}: recorded a valid transaction as skipped: {skipped:?} (invalid: {invalid_ids:?})"
        );
    }
}
