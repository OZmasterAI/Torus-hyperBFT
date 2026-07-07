//! S426 consensus livelock — fork-precondition characterization test.
//!
//! # STATUS: this test PASSES (green). It is *not* a RED reproduction.
//!
//! The goal was a DELIBERATELY RED test reproducing the S426 livelock: a node that missed a QC'd
//! block never recovers it, even on a healed network, pinning the commit frontier forever. The
//! production log signature (devnet A1.6 bake) was:
//!
//! ```text
//! WARN ... dropping proposal header: ... is_safe=false, justify_block_known=false
//! block_sync: batch at height H made no commit progress
//! ```
//!
//! This test *deterministically induces the documented precondition* — a single validator is
//! starved of proposal headers and block bodies while the other three (a quorum) keep committing,
//! so the starved node lands on the `justify_block_known=false` path — and then heals the network.
//! But on this branch's `hotstuff_rs`, running the `NumberApp` harness, **the cluster recovers**:
//! `min(committed height)` advances past `H`, so the final `wait_until` returns instead of panicking.
//!
//! # Why it does not reproduce RED here (empirical, two experiments + code trace)
//!
//! The root cause (memory `7462e1ed5ccbc63a`) requires block sync to be *unable to serve* the missing
//! block — the serve walk `Err`ing on a missing speculative body
//! (`block_tree/accessors/public.rs:59`/`:93`) so the server sends nothing (`block_sync/server.rs:145`).
//! That requires a **server-side speculative gap**: a node whose `newest_block → committed` chain
//! contains a block it lacks the body of. On this branch that gap cannot form via message dropping:
//!
//! - `set_newest_block` is only ever called from `insert` (`block_tree/accessors/internal.rs`), and
//!   `try_insert_body` (`hotstuff/implementation.rs:1787`) refuses to insert a block whose parent
//!   state is missing (it defers the body). So `newest_block` only ever advances along a *fully
//!   bodied* chain — no server ever has an unserveable speculative gap.
//! - `block_to_commit` returns `Ok(None)` while a body is pending (session-212 fix, memory
//!   `693d2e1d…`), so the committed chain gets no holes either.
//! - A block's proposer self-inserts its body before broadcasting the header, so there is **always a
//!   clean server** for any block. `block_sync_test` confirms block sync heals a lagging node.
//!
//! Two experiments both went green: (a) drop `BlockDataResponse` bodies to two nodes; (b) drop
//! `ProposalHeader`s+bodies to one node (this test). In both, the split is induced (verified below)
//! but the healed network recovers via `body_fetch_tracker` re-fetch and/or working block sync.
//!
//! The reliable production repro is `devnet/bake-a16-s426.sh` — the **real EVM app** at scale
//! (~17-19k accounts, 512 KB blocks, committed ~1311 while executed ~1244), where large-body
//! dissemination timing opens the header-first race *and* the EVM/DA execution pipeline produces the
//! server gap. That integration-scale condition is not expressible in the `NumberApp` harness (tiny,
//! instantly-delivered bodies, no execution pipeline) without a product-side test hook to force a
//! node to persist `highest_pc`/`newest` onto a body-less block. See the agent report for details.
//!
//! This test is retained as: (1) a regression guard that the harness self-heals this fork
//! precondition, and (2) ready-made scaffolding (see [`common::network::FilterHandle`]) for a future
//! RED test once such a hook exists.

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

/// A short max view time so a stall becomes observable in seconds rather than tens of seconds.
const MAX_VIEW_TIME: Duration = Duration::from_millis(2000);

/// The committed height the baseline must reach before we induce the fork.
const BASELINE_TARGET_HEIGHT: u64 = 2;

/// Render every node's `(committed_height, highest_pc_height, highest_view)` for panic diagnostics.
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

#[test]
fn justify_block_livelock_test() {
    // 1. Initialize a 4-node cluster (quorum = 3) wired through a filterable mock network.
    let mut csprg = OsRng {};
    let keypairs: Vec<SigningKey> = (0..4).map(|_| SigningKey::generate(&mut csprg)).collect();

    let (network_stubs, filter) =
        mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));

    // Node 3 is the target `T`: it will be starved of proposal headers and block bodies during the
    // window. The other three nodes are a full quorum and keep committing without it.
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
            Node::new_with_max_view_time(
                keypair.clone(),
                network,
                init_as.clone(),
                init_vs_updates.clone(),
                MAX_VIEW_TIME,
            )
        })
        .collect();

    // 2. Baseline sanity: on a fully healthy network, every node must commit at least
    //    BASELINE_TARGET_HEIGHT blocks. Submit an Increment to every node so whichever node leads has
    //    something to propose and blocks are produced steadily.
    log_with_context(None, "Baseline: submitting Increments to all 4 nodes.");
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }
    wait_until(
        Duration::from_secs(180),
        POLL_INTERVAL,
        &format!("all 4 nodes to commit at least {BASELINE_TARGET_HEIGHT} blocks (baseline liveness)"),
        || min_committed(&nodes) >= BASELINE_TARGET_HEIGHT,
        || describe_cluster(&nodes),
    );

    // Reference committed height H = the frontier the cluster must advance past to be "live".
    let baseline_h = min_committed(&nodes);
    log_with_context(
        None,
        &format!("Baseline committed. H = {baseline_h}. {}", describe_cluster(&nodes)),
    );

    // 3. Starve node 3 of proposal headers and block bodies while the other three (a quorum) keep
    //    committing. Node 3 thereby misses the blocks that become the justify targets of subsequent
    //    proposals — the seed of the `justify_block_known=false` header drop.
    log_with_context(
        None,
        "Enabling header+body starvation on node 3 (nodes 0/1/2 keep quorum).",
    );
    filter.starve(vec![target_key], true);

    // Keep the quorum {0,1,2} producing fresh blocks so their commit frontier advances past H while
    // node 3 is cut off.
    for _ in 0..6 {
        nodes[0].submit_transaction(NumberAppTransaction::Increment);
        nodes[1].submit_transaction(NumberAppTransaction::Increment);
        nodes[2].submit_transaction(NumberAppTransaction::Increment);
    }

    // Confirm the split was actually induced before we heal the network: the quorum {0,1,2} commits
    // past H while node 3 is left behind at H. Node 3 is now missing QC'd blocks it never saw the
    // headers for.
    wait_until(
        Duration::from_secs(90),
        POLL_INTERVAL,
        "the quorum {0,1,2} to commit past H while node 3 is left behind at H",
        || {
            let quorum_min = [
                nodes[0].committed_height().unwrap_or(0),
                nodes[1].committed_height().unwrap_or(0),
                nodes[2].committed_height().unwrap_or(0),
            ]
            .into_iter()
            .min()
            .unwrap();
            let target = nodes[3].committed_height().unwrap_or(0);
            quorum_min > baseline_h && target <= baseline_h
        },
        || describe_cluster(&nodes),
    );
    log_with_context(
        None,
        &format!("Split induced (node 3 left behind). {}", describe_cluster(&nodes)),
    );

    // 4. Heal the network completely. From here on nothing is dropped. The S426 hypothesis was that
    //    node 3 could never recover the QC'd blocks it missed; the assertion below tests that.
    log_with_context(None, "Lifting the filter — network is now fully healthy.");
    filter.heal();

    // 5. Recovery assertion.
    //
    // Hypothesis (S426): this PANICS because node 3 can never recover the QC'd blocks whose headers it
    // missed. Empirically on this branch it does NOT panic — the healed network recovers node 3 (via
    // `body_fetch_tracker` re-fetch and/or working block sync) and the commit frontier advances past
    // H. See the module docs for why a true RED repro needs the EVM-scale bake or a product hook.
    //
    // The 90s budget deliberately exceeds the block-sync trigger timeout (60s): if the harness were
    // ever to livelock this precondition, this would become a genuine (not impatient) failure.
    wait_until(
        Duration::from_secs(90),
        POLL_INTERVAL,
        &format!(
            "commit frontier to advance past H={baseline_h} on a HEALED network \
             (min committed height across all nodes > {baseline_h})"
        ),
        || min_committed(&nodes) > baseline_h,
        || describe_cluster(&nodes),
    );
}
