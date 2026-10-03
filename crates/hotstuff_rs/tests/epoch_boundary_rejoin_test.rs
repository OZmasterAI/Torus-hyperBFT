//! s85 epoch-boundary rejoin: a validator restarted inside an epoch-change view must not wedge
//! the chain.
//!
//! # Setup
//!
//! Three validators at EQUAL stake: `ValidatorSet::quorum` = `floor(2 * total / 3) + 1` is then all
//! three, so every QC and every TC needs every validator. A view `v` with
//! `v % epoch_length == 0` is an epoch-change view: a replica leaves it only with a QC or a TC for
//! it (on timeout it broadcasts a TimeoutVote and extends the view).
//!
//! # The bug (s85 drill, view 12400)
//!
//! val2 entered the epoch-change view E and was stopped before its vote left. It restarted at
//! `highest_view_with_progress + 1` = E + 1, in the next epoch, with no certificate for E. val0 and
//! val1 stayed in E: a QC(E) needs val2's phase vote, but val2 only votes in its current view; a
//! TC(E) needs val2's TimeoutVote(E), but val2 drops pacemaker messages below its view; and
//! round-skip never crosses an epoch. val2 ran alone through its epoch on local timeouts (with the
//! view-timeout backoff, about 19 min here), so the chain stopped committing.
//!
//! # The test
//!
//! The filter drops everything val2 sends for E or later. So val2 enters E, but nothing it does
//! there leaves the node, as in the drill. Once all three are in E, val2 is stopped and restarted
//! on its own store, with its inbox (what the others sent meanwhile) kept. Commits must resume.

use std::time::Duration;

use hotstuff_rs::types::{
    crypto_primitives::SigningKey,
    data_types::{Power, ViewNumber},
    update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network_with_filter,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);

const EPOCH_LENGTH: u32 = 10;

/// The second epoch-change view, so the chain has committed blocks before it.
const EPOCH_CHANGE_VIEW: u64 = 2 * EPOCH_LENGTH as u64;

/// Index of the validator that is stopped in [`EPOCH_CHANGE_VIEW`] and restarted.
const RESTARTED: usize = 2;

/// Blocks every validator must commit after the restart.
const COMMITS_AFTER_RESTART: u64 = 2;

fn describe(nodes: &[Node]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{i}{{committed={:?}, highest_pc_view={}, view={}}}",
                n.committed_height(),
                n.highest_pc_view(),
                n.highest_view_entered(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn restart_inside_an_epoch_change_view_does_not_wedge_equal_stake_validators() {
    let keypairs: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();

    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1_000_000));
    }
    let init_as = NumberApp::initial_app_state();

    let (stubs, filter) = mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));
    // The restarted process gets the same inbox: messages sent to it while it is down are kept.
    let restart_stub = stubs[RESTARTED].clone();
    let epoch_change_view = ViewNumber::new(EPOCH_CHANGE_VIEW);
    filter.silence_from_view(keypairs[RESTARTED].verifying_key(), epoch_change_view);

    let mut nodes: Vec<Node> = keypairs
        .iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_epoch_length(
                kp.clone(),
                net,
                init_as.clone(),
                vs_updates.clone(),
                MAX_VIEW_TIME,
                EPOCH_LENGTH,
                None,
            )
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    // Nothing val2 sends from the epoch-change view on leaves it, so no QC or TC for that view can
    // form: all three end up in it and stay there.
    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        &format!("all three validators to enter the epoch-change view {EPOCH_CHANGE_VIEW}"),
        || nodes.iter().all(|n| n.highest_view_entered() == epoch_change_view),
        || describe(&nodes),
    );
    let committed_before = nodes
        .iter()
        .map(|n| n.committed_height().unwrap_or(0))
        .max()
        .unwrap();

    // Stop val2 (dropping the node shuts its replica down) and restart it on its own store.
    let store = nodes[RESTARTED].kv_store();
    drop(nodes.remove(RESTARTED));
    filter.heal();
    nodes.insert(
        RESTARTED,
        Node::new_with_epoch_length(
            keypairs[RESTARTED].clone(),
            restart_stub,
            init_as,
            vs_updates,
            MAX_VIEW_TIME,
            EPOCH_LENGTH,
            Some(store),
        ),
    );

    let target = committed_before + COMMITS_AFTER_RESTART;
    wait_until(
        Duration::from_secs(60),
        POLL_INTERVAL,
        &format!(
            "every validator to leave view {EPOCH_CHANGE_VIEW} and commit height >= {target} \
             (committed {committed_before} at the restart)"
        ),
        || {
            nodes.iter().all(|n| {
                n.highest_view_entered() > epoch_change_view
                    && n.committed_height().unwrap_or(0) >= target
            })
        },
        || describe(&nodes),
    );
}
