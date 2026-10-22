//! s138 (9k2 deep r4-U, all-up, stakes 2/2/1): the chain halted for good at
//! height 9498.
//!
//! TorusApp refuses an epoch-boundary block while its execution has not
//! applied the previous boundary yet, returning a datum-less placeholder that
//! every honest `validate_block` rejects. The leader self-inserted the
//! placeholder, broadcast it and voted for it without a data check. The
//! placeholder is the same block for every leader on the same parent, and a
//! replica that proposed a block votes for an identical one from a later
//! leader through `pending_bodies`: val1 (view 9507) and val0 (view 9508) both
//! refused, both voted, 2 + 2 of 5 power certified it. Every later proposal
//! extends the certified placeholder and fails the data check; val2 can
//! neither fetch nor sync it. No commit after 17:29:07.
//!
//! The test runs 3 validators with powers 2/2/1 (quorum 4). Every leader
//! refuses height 3 for a few view timeouts, then builds it. RED while
//! hotstuff proposes a refusal (the chain never commits past height 2), GREEN
//! once a refusing leader proposes nothing (`App::refuses_proposal`).

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network,
    node::{Node, WedgeOptions},
    number_app::{NumberApp, NumberAppTransaction, Refusal},
    poll::wait_until,
    signature_log::{self, signature},
};

const POLL_INTERVAL: Duration = Duration::from_millis(200);
const MAX_VIEW_TIME: Duration = Duration::from_millis(1000);
const REFUSED_HEIGHT: u64 = 3;
const TARGET_HEIGHT: u64 = 8;

fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{i}{{committed={:?}, highest_pc={:?}, view={}, number={}}}",
                n.committed_height(),
                n.highest_pc_height(),
                n.highest_view_entered(),
                n.number(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn refusing_leaders_do_not_certify_their_placeholder() {
    signature_log::install();

    let keypairs: Vec<SigningKey> = (1..=3u8)
        .map(|i| SigningKey::from_bytes(&[i; 32]))
        .collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for (kp, power) in keypairs.iter().zip([2u64, 2, 1]) {
        vs_updates.insert(kp.verifying_key(), Power::new(power));
    }
    let init_as = NumberApp::initial_app_state();
    let refusal = Refusal {
        height: REFUSED_HEIGHT,
        active: Arc::new(AtomicBool::new(true)),
    };
    let options = WedgeOptions {
        produce_delay: Duration::from_millis(20),
        validate_delay: Duration::from_millis(20),
        commit_lag_cap: 0,
    };

    let stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));
    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_refusal(
                kp,
                net,
                init_as.clone(),
                vs_updates.clone(),
                MAX_VIEW_TIME,
                options,
                refusal.clone(),
            )
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    // The chain reaches the refused height: block 2 is certified everywhere.
    wait_until(
        Duration::from_secs(30),
        POLL_INTERVAL,
        "every replica to certify height 2",
        || {
            nodes
                .iter()
                .all(|n| n.highest_pc_height().unwrap_or(0) >= REFUSED_HEIGHT - 1)
        },
        || format!("{} | {}", describe(&nodes), signature()),
    );

    // Every leader refuses height 3 for six view timeouts: each validator,
    // both heavy ones included, leads a view on block 2 and refuses.
    thread::sleep(MAX_VIEW_TIME * 20);
    refusal.active.store(false, Ordering::SeqCst);

    // Liveness: once the leaders build height 3 again the chain moves on.
    wait_until(
        Duration::from_secs(45),
        POLL_INTERVAL,
        &format!("every replica to commit height >= {TARGET_HEIGHT} after the refusals"),
        || {
            nodes
                .iter()
                .all(|n| n.committed_height().unwrap_or(0) >= TARGET_HEIGHT)
        },
        || format!("{} | {}", describe(&nodes), signature()),
    );
}
