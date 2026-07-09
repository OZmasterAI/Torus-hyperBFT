//! S430 "header-first body starvation" livelock — deterministic proposer-path repro.
//!
//! # The bug (S430, log-proven on devnet A1.6 bake)
//!
//! In the hash-only header-first pipeline a validator votes on proposal *headers* while the
//! corresponding *bodies* are still in flight. When such a validator collects (as leader) or observes
//! a quorum certificate over a block whose body it does not yet hold, it advances its `highest_pc` to
//! that block via `advance_highest_pc_from_remote`
//! (`hotstuff/implementation.rs:1208`, gated by the `pc_block_pending` relaxation at `:1196`) — even
//! though the block is only a *pending header*, absent from its block tree.
//!
//! Later, when that same validator becomes leader, `enter_view`'s proposer path looks up
//! `block_tree.block_height(&highest_pc.block)`, finds `None`, sets `proposal_deferred = true` and
//! returns. **Before the fix nothing actively fetches the missing parent** — the only healing route is
//! height-range block sync, which triggers only at a multi-view lag. So leadership rotating onto
//! body-less validators burns full view timeouts, and the cluster's commit frontier crawls (under
//! 8-way load `progress_and_validator_set_update_test` fails ~84% with an rc=101 poll timeout).
//!
//! # What this test does
//!
//! 1. Runs a 4-node cluster (quorum = 3) with height-range block sync **disabled**
//!    ([`Node::new_with_max_view_time_sync_disabled`]) so the *only* fast recovery path for a missing
//!    parent body is the by-hash parent fetch under test (`enter_view` → `request_justify_block`).
//! 2. Baselines the cluster to a healthy commit frontier `H`.
//! 3. Starves node 3 of **block bodies only** (headers still flow) so it keeps voting on headers and
//!    advances its `highest_pc` onto blocks whose bodies it never receives — the exact
//!    `highest_pc_height() == None while highest_view_entered() advances` precondition of the bug.
//! 4. Heals the network, then asserts the commit frontier advances **across all four nodes** — i.e.
//!    node 3 (now periodically a body-less leader, with block sync disabled) recovers the missing
//!    parent by hash and resumes proposing, instead of deferring forever.
//!
//! Without the fix, the starved leader defers within its view and — block sync being disabled — only
//! recovers on the slower header-follower path, so the bounded commit assertion is starved. With the
//! fix, `enter_view` proactively by-hash fetches the missing parent and the frontier advances promptly.

use std::time::Duration;

use rand_core::OsRng;

use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    logging::log_with_context,
    network::mock_network_with_filter,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

/// Interval between polls of cluster state.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A short max view time so a body-less-leader stall costs ~1s, not tens of seconds — enough time for
/// `NumberApp`'s 250ms produce/validate but short enough that many views rotate inside the window.
const MAX_VIEW_TIME: Duration = Duration::from_millis(4000);

/// Committed height the baseline must reach before we induce starvation.
const BASELINE_TARGET_HEIGHT: u64 = 2;

/// How far past `H` the quorum {0,1,2} must advance while node 3 is starved, so node 3 collects QCs
/// over several blocks whose bodies it never obtains (building the body-less `highest_pc`).
const STARVE_ADVANCE: u64 = 3;

/// Render every node's `(committed, highest_pc, view)` for panic diagnostics.
fn describe_cluster(nodes: &[Node]) -> String {
    let rows: Vec<String> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{}{{committed={:?}, highest_pc={:?}, view={}}}",
                i,
                n.committed_height(),
                n.highest_pc_height(),
                n.highest_view_entered(),
            )
        })
        .collect();
    rows.join(", ")
}

/// Minimum committed height across all nodes (a node that has committed nothing counts as 0).
fn min_committed(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| n.committed_height().unwrap_or(0))
        .min()
        .unwrap_or(0)
}

/// Minimum committed height across the quorum {0,1,2}.
fn quorum_min_committed(nodes: &[Node]) -> u64 {
    [
        nodes[0].committed_height().unwrap_or(0),
        nodes[1].committed_height().unwrap_or(0),
        nodes[2].committed_height().unwrap_or(0),
    ]
    .into_iter()
    .min()
    .unwrap()
}

#[test]
fn parent_body_starvation_test() {
    // 1. Initialize a 4-node cluster (quorum = 3) with a filterable mock network and block sync
    //    DISABLED, so the by-hash parent fetch is the only fast recovery path.
    let mut csprg = OsRng {};
    let keypairs: Vec<SigningKey> = (0..4).map(|_| SigningKey::generate(&mut csprg)).collect();

    let (network_stubs, filter) =
        mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));

    // Node 3 (target `T`) will be starved of block bodies during the window.
    let target_key = keypairs[3].verifying_key();

    let init_as = NumberApp::initial_app_state();
    let init_vs_updates = {
        let mut vs_updates = ValidatorSetUpdates::new();
        for kp in &keypairs {
            vs_updates.insert(kp.verifying_key(), Power::new(1));
        }
        vs_updates
    };

    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(network_stubs)
        .map(|(keypair, network)| {
            Node::new_with_max_view_time_sync_disabled(
                keypair.clone(),
                network,
                init_as.clone(),
                init_vs_updates.clone(),
                MAX_VIEW_TIME,
            )
        })
        .collect();

    // 2. Baseline: on a healthy network every node commits at least BASELINE_TARGET_HEIGHT blocks.
    log_with_context(None, "Baseline: submitting Increments to all 4 nodes.");
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }
    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        &format!(
            "all 4 nodes to commit at least {BASELINE_TARGET_HEIGHT} blocks (baseline liveness)"
        ),
        || min_committed(&nodes) >= BASELINE_TARGET_HEIGHT,
        || describe_cluster(&nodes),
    );

    let baseline_h = min_committed(&nodes);
    log_with_context(
        None,
        &format!(
            "Baseline committed. H = {baseline_h}. {}",
            describe_cluster(&nodes)
        ),
    );

    // 3. Starve node 3 of BLOCK BODIES only (headers still flow). It keeps voting on headers and
    //    advances its highest_pc onto blocks whose bodies it never obtains.
    log_with_context(
        None,
        "Enabling body starvation on node 3 (headers still delivered; nodes 0/1/2 keep quorum).",
    );
    filter.starve(vec![target_key], /* drop_headers */ false);

    // Keep the quorum {0,1,2} producing fresh blocks so their frontier advances past H.
    for _ in 0..8 {
        nodes[0].submit_transaction(NumberAppTransaction::Increment);
        nodes[1].submit_transaction(NumberAppTransaction::Increment);
        nodes[2].submit_transaction(NumberAppTransaction::Increment);
    }

    // Confirm the body-less precondition actually formed: the quorum commits past H while node 3's
    // highest_pc certifies a block whose body it lacks (highest_pc_height() == None) yet it keeps
    // entering views (highest_view_entered advances) — i.e. it is on the deferring-proposer path.
    let starve_target = baseline_h + STARVE_ADVANCE;
    wait_until(
        Duration::from_secs(90),
        POLL_INTERVAL,
        &format!(
            "quorum {{0,1,2}} to commit to >= {starve_target} while node 3 is a body-less validator \
             (highest_pc_height == None)"
        ),
        || {
            quorum_min_committed(&nodes) >= starve_target
                && nodes[3].highest_pc_height().is_none()
                && nodes[3].highest_view_entered().int() > baseline_h
        },
        || describe_cluster(&nodes),
    );
    let frontier_at_starve = quorum_min_committed(&nodes);
    log_with_context(
        None,
        &format!(
            "Body-less precondition formed on node 3. quorum frontier = {frontier_at_starve}. {}",
            describe_cluster(&nodes)
        ),
    );

    // 4. Heal the network. Bodies now flow again, so a by-hash parent fetch issued by node 3 can be
    //    served. Block sync remains DISABLED, so the by-hash fetch is the only fast recovery path.
    log_with_context(
        None,
        "Lifting the filter — bodies flow again (block sync stays disabled).",
    );
    filter.heal();

    // 5. Liveness assertion: the commit frontier must advance ACROSS ALL FOUR NODES within a bounded
    //    window. This requires node 3 — now periodically a body-less leader — to recover its missing
    //    parent by hash and resume proposing.
    //
    //    Without the fix and with block sync disabled, node 3 defers whenever it leads and only heals
    //    on the slower header-follower path, so this bounded frontier advance is starved. With the
    //    fix, `enter_view` by-hash fetches the missing parent and node 3 rejoins promptly.
    let recovery_target = frontier_at_starve + 1;
    wait_until(
        Duration::from_secs(30),
        POLL_INTERVAL,
        &format!(
            "commit frontier to advance to >= {recovery_target} across ALL FOUR nodes on a healed \
             network with block sync disabled (node 3 must by-hash fetch the missing parent)"
        ),
        || min_committed(&nodes) >= recovery_target,
        || describe_cluster(&nodes),
    );
    log_with_context(
        None,
        &format!(
            "Frontier advanced across all nodes. {}",
            describe_cluster(&nodes)
        ),
    );
}
