//! s84 wedge theory: a CERTIFIED block that the other validators find INVALID.
//!
//! Replicas phase-vote on a proposal HEADER before the body arrives and before
//! `App::validate_block` runs on it (header fast path, vote-before-validate). So a
//! block can get a QC and only afterwards be rejected by `validate_block` on the
//! validators that voted for it. Its proposer self-inserted it without validating
//! and is the only replica holding it.
//!
//! The QC is then the highest PC everywhere, so every later proposal must extend
//! the block, and the rejecting replicas can neither insert it (by-hash justify
//! fetch returns a body that fails validation, "justify fetch exhausted") nor
//! sync it (block sync serves committed blocks only, "made no progress"). The s83
//! drill wedge at height 5630 carried exactly these log lines.
//!
//! The test runs 4 validators (quorum 3); the first block at `POISON_HEIGHT` that
//! any replica validates is rejected by every replica that validates it.
//!
//! # Status: RED (ignored until a protocol decision)
//!
//! On main d623d3c and on fix/liveness-3val the chain wedges: the proposer
//! commits up to height 3 and keeps extending (highest_pc 13), the three
//! rejecting replicas stay at committed 1 with an unknown highest-PC block, for
//! 120 s. They also keep VOTING for descendants of the block their app
//! rejected: the rejected body is parked in `deferred_bodies`
//! (`on_receive_block_data_response`) while its header stays in
//! `pending_headers`, which the header path treats as "previously validated".
//! Run: `cargo test -p hotstuff_rs --test certified_invalid_block_test -- --ignored`.

use std::time::Duration;

use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction, PoisonedBlock},
    poll::wait_until,
    signature_log::{self, signature},
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_VIEW_TIME: Duration = Duration::from_millis(2000);
const POISON_HEIGHT: u64 = 4;

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
#[ignore = "s84 RED: certified block the voters cannot insert wedges the chain; needs a protocol decision (vote timing)"]
fn certified_block_rejected_by_its_voters_does_not_wedge_the_chain() {
    signature_log::install();

    let keypairs: Vec<SigningKey> = (1..=4u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1));
    }
    let init_as = NumberApp::initial_app_state();
    let poison = PoisonedBlock::at_height(POISON_HEIGHT);

    let stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));
    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_poison(
                kp,
                net,
                init_as.clone(),
                vs_updates.clone(),
                MAX_VIEW_TIME,
                poison.clone(),
            )
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    wait_until(
        Duration::from_secs(60),
        POLL_INTERVAL,
        "a block at the poisoned height to be rejected",
        || poison.lock().unwrap().rejections > 0,
        || describe(&nodes),
    );

    // Liveness: the chain must commit well past the poisoned height. The budget
    // exceeds the 60 s block-sync trigger timeout, so sync gets its chance too.
    let target = POISON_HEIGHT + 3;
    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        &format!("every replica to commit height >= {target} past the rejected block"),
        || {
            nodes
                .iter()
                .all(|n| n.committed_height().unwrap_or(0) >= target)
        },
        || {
            let p = poison.lock().unwrap();
            format!(
                "{} | poisoned={:?} rejections={} | {}",
                describe(&nodes),
                p.hash.map(|h| h.bytes()[..4].to_vec()),
                p.rejections,
                signature(),
            )
        },
    );
}
