//! S470 — deterministic in-proc reproduction of the header-pipeline COMMIT WEDGE, and proof
//! that the commit-lag view-deadline backoff (`commit_lag_cap`) resolves it.
//!
//! # The mechanism being reproduced
//!
//! Production shape (300k orders/s offered, ~6MB blocks, ~2.7s body execution, 500ms views):
//! proposals are broadcast header-first and replicas vote on headers WITHOUT executing bodies,
//! so `highest_pc` advances at header speed while COMMIT waits on body dissemination +
//! execution (`try_insert_body` → `validate_block` → 2-chain commit). The 2-chain commit rule
//! requires CONSECUTIVE-VIEW QCs (`justify.view == parent_justify.view + 1`), but with the
//! block-insertable latency far above the view timer no leader ever has the QC frontier's
//! block inserted in time to propose in `justify.view + 1` — every proposal lands views late,
//! every QC pair is non-consecutive, the commit frontier FREEZES while the QC frontier crawls.
//!
//! The pre-S470 Task A backoff is keyed on `view − highest_qc_view`, which RESETS every time a
//! (non-committing) QC forms — i.e. every few views — so it never stretches deadlines enough
//! for a leader to obtain the tip AND propose within the tip's justified view. That is the
//! diagnosed blind spot; the S470 term is keyed on `highest_qc_view − committed_view`, the gap
//! that grows without bound during this wedge.
//!
//! # Scale-down used here (and why it is a NETWORK body delay, not a validate sleep)
//!
//! 3 validators, `max_view_time` 500ms, and a mock network that delivers every block BODY
//! (`BlockDataResponse` / full `Proposal`) 2.5s late while headers, votes and pacemaker
//! traffic stay instant (`mock_network_with_body_delay`). The 2.5s lumps together what
//! production splits between dissemination and execution: the time from "header certified"
//! to "block insertable on a non-proposer".
//!
//! The first attempt — a 2s `validate_block` sleep (the delay-injection route suggested by the
//! harness) — does NOT reproduce the wedge, and the reason is instructive: `validate_block`
//! runs ON the algorithm thread, so a sleeping replica's pacemaker stops ticking and its view
//! clock FREEZES for the duration. All replicas then wake from validation still standing in
//! `justify.view + 1`, propose "in time" in view-number space, and the consecutive-views rule
//! happily commits. The wedge's precondition is precisely that the view clock keeps running
//! while the tip is not yet insertable — in production the exec pipeline is off-thread, and in
//! this harness the delay must therefore live in the network, not in the app. (This is the
//! "single-threaded NumberApp delivers bodies too promptly" caveat from the mission brief; the
//! test hook used is the harness-side body-delay network — no product-side hook was needed.)
//!
//! What this harness does NOT reproduce (documented per the mission brief): the cross-branch
//! lock rejection (`is_safe=false, justify_block_known=true` drops). The NumberApp propose
//! path only ever builds on `highest_pc.block` (deferring while it is missing), so no forked
//! branches form in-proc — the same limitation documented in `justify_block_livelock_test.rs`.
//! The fork variant remains observable only at EVM scale (the W2 bench, `TORUS_WEDGE_DIAG=1`).
//! What IS reproduced is the wedge's load-bearing core: the commit frontier frozen for tens of
//! views while the QC frontier crawls and views churn.
//!
//! # RED / GREEN
//!
//! * [`commit_lag_wedge_red_knob_off_commit_freezes`]: `commit_lag_cap = 0` (today's shipped
//!   schedule) — the commit frontier stays FROZEN for ≥ 30 views while `highest_pc.view`
//!   climbs. This test passes before and after the fix: it pins the wedge's existence under
//!   the knob-off schedule (if it ever fails, the harness no longer reproduces the wedge and
//!   the GREEN test proves nothing).
//! * [`commit_lag_wedge_green_knob_on_commits_advance`]: `commit_lag_cap = 8` — once the QC
//!   frontier outruns the commit frontier past the grace window the commit-lag term stretches
//!   deadlines, a leader obtains the certified tip and proposes within the justified view,
//!   consecutive QCs form, and the commit frontier advances on every node.

use std::time::Duration;

use rand_core::OsRng;

use hotstuff_rs::types::{
    crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    logging::log_with_context,
    network::mock_network_with_body_delay,
    node::{Node, WedgeOptions},
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

/// Interval between polls of cluster state.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The view timer — deliberately far below the body-insertable latency
/// (production: 500ms views vs ~2.7s dissemination+exec).
const MAX_VIEW_TIME: Duration = Duration::from_millis(500);

/// Body-insertable latency stand-in: every block body is delivered this late,
/// ≫ `MAX_VIEW_TIME` (and comfortably above one view even with the S395
/// cumulative-schedule slack and TC-formation latency, so a body can never
/// arrive while the cluster still stands in `justify.view + 1` under the
/// knob-off 500ms schedule).
const BODY_DELAY: Duration = Duration::from_millis(2500);

/// `NumberApp` delays: kept small — the wedge driver is `BODY_DELAY`, and an
/// in-thread validate sleep must stay well under `MAX_VIEW_TIME` or it would
/// freeze the view clock (see module docs).
const PRODUCE_DELAY: Duration = Duration::from_millis(50);
const VALIDATE_DELAY: Duration = Duration::from_millis(250);

/// RED: how many views the cluster must churn through while the commit
/// frontier stays frozen. (Production froze for 175–310s per 330s window
/// while views raced ~160 ahead; one in-proc wedge cycle is ~3 views.)
const RED_FROZEN_VIEWS: u64 = 30;

/// RED: how far the QC frontier must crawl during the frozen window
/// (distinguishes the wedge — QCs keep forming, the stall term keeps
/// resetting — from a total stall, which Task A already handles).
const RED_MIN_PC_ADVANCE: u64 = 3;

/// GREEN: every node must commit at least this many blocks.
const GREEN_TARGET_COMMITS: u64 = 3;

fn build_cluster(commit_lag_cap: u32) -> Vec<Node> {
    let mut csprg = OsRng {};
    let keypairs: Vec<SigningKey> = (0..3).map(|_| SigningKey::generate(&mut csprg)).collect();
    let network_stubs =
        mock_network_with_body_delay(keypairs.iter().map(|kp| kp.verifying_key()), BODY_DELAY);

    let init_as = NumberApp::initial_app_state();
    let init_vs_updates = {
        let mut vs_updates = ValidatorSetUpdates::new();
        for kp in &keypairs {
            vs_updates.insert(kp.verifying_key(), Power::new(1));
        }
        vs_updates
    };

    keypairs
        .into_iter()
        .zip(network_stubs)
        .map(|(keypair, network)| {
            Node::new_with_wedge_options(
                keypair,
                network,
                init_as.clone(),
                init_vs_updates.clone(),
                MAX_VIEW_TIME,
                WedgeOptions {
                    produce_delay: PRODUCE_DELAY,
                    validate_delay: VALIDATE_DELAY,
                    commit_lag_cap,
                },
            )
        })
        .collect()
}

/// Render every node's `(committed_height, highest_pc_view, highest_view)` for diagnostics.
fn describe_cluster(nodes: &[Node]) -> String {
    let rows: Vec<String> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            format!(
                "n{}{{committed={:?}, highest_pc_view={}, view={}}}",
                i,
                n.committed_height(),
                n.highest_pc_view(),
                n.highest_view_entered(),
            )
        })
        .collect();
    rows.join(", ")
}

/// Committed BLOCK COUNT (height + 1; 0 = nothing committed) — max across nodes.
fn max_committed(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| n.committed_height().map(|h| h + 1).unwrap_or(0))
        .max()
        .unwrap_or(0)
}

/// Committed BLOCK COUNT — min across nodes.
fn min_committed(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| n.committed_height().map(|h| h + 1).unwrap_or(0))
        .min()
        .unwrap_or(0)
}

fn max_view(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| n.highest_view_entered().int())
        .max()
        .unwrap_or(0)
}

fn max_pc_view(nodes: &[Node]) -> u64 {
    nodes.iter().map(|n| n.highest_pc_view()).max().unwrap_or(0)
}

/// RED — knob off (`commit_lag_cap = 0`, today's shipped schedule): with body delivery ≫ view
/// time, the commit frontier FREEZES while views churn and the QC frontier crawls. This is the
/// deterministic in-proc reproduction of the production wedge signature.
#[test]
fn commit_lag_wedge_red_knob_off_commit_freezes() {
    let mut nodes = build_cluster(0);
    for node in nodes.iter_mut() {
        for _ in 0..3 {
            node.submit_transaction(NumberAppTransaction::Increment);
        }
    }

    // Warm-up: let the cluster start churning views (proof the pacemaker and proposers run).
    wait_until(
        Duration::from_secs(120),
        POLL_INTERVAL,
        "cluster to start churning views (warm-up)",
        || max_view(&nodes) >= 10,
        || describe_cluster(&nodes),
    );

    // Snapshot the frontiers, then let the cluster churn RED_FROZEN_VIEWS more views.
    let committed_0 = max_committed(&nodes);
    let pc_view_0 = max_pc_view(&nodes);
    let view_0 = max_view(&nodes);
    log_with_context(
        None,
        &format!(
            "RED window opens: committed={committed_0}, pc_view={pc_view_0}, view={view_0}. {}",
            describe_cluster(&nodes)
        ),
    );

    wait_until(
        Duration::from_secs(300),
        POLL_INTERVAL,
        &format!("the cluster to churn through {RED_FROZEN_VIEWS} further views"),
        || max_view(&nodes) >= view_0 + RED_FROZEN_VIEWS,
        || describe_cluster(&nodes),
    );

    let committed_1 = max_committed(&nodes);
    let pc_view_1 = max_pc_view(&nodes);
    log_with_context(
        None,
        &format!(
            "RED window closes: committed={committed_1}, pc_view={pc_view_1}. {}",
            describe_cluster(&nodes)
        ),
    );

    // The wedge signature. (1) The commit frontier froze across the whole window...
    assert_eq!(
        committed_1, committed_0,
        "knob-off cluster must exhibit the commit wedge: committed height frozen while ≥ {} \
         views churned — if this fails the harness no longer reproduces the wedge and the \
         GREEN test proves nothing. Cluster: {}",
        RED_FROZEN_VIEWS,
        describe_cluster(&nodes),
    );
    // ...(2) while the QC frontier kept crawling — QCs form, so the pre-S470 stall backoff
    // (keyed on view − highest_qc_view) keeps resetting and never engages far enough. This is
    // the diagnosed blind spot, distinguished from a total stall.
    assert!(
        pc_view_1 >= pc_view_0 + RED_MIN_PC_ADVANCE,
        "the QC frontier must CRAWL during the wedge (pc_view {pc_view_0} -> {pc_view_1}, \
         expected +{RED_MIN_PC_ADVANCE}): if QCs stop forming entirely this is a different \
         failure mode (a plain stall, which Task A already handles). Cluster: {}",
        describe_cluster(&nodes),
    );
}

/// GREEN — knob on (`commit_lag_cap = 8`): identical cluster, identical delays. Once the QC
/// frontier outruns the commit frontier past the grace window, the S470 commit-lag term
/// stretches view deadlines until a leader can obtain the certified tip and propose within
/// the justified view; consecutive-view QCs form and the commit frontier advances on EVERY
/// node.
#[test]
fn commit_lag_wedge_green_knob_on_commits_advance() {
    let mut nodes = build_cluster(8);
    for node in nodes.iter_mut() {
        for _ in 0..3 {
            node.submit_transaction(NumberAppTransaction::Increment);
        }
    }

    // First commit: proves the consecutive-views commit rule fired at all under the identical
    // delay regime that freezes the RED cluster.
    wait_until(
        Duration::from_secs(240),
        POLL_INTERVAL,
        "the first block to commit anywhere (knob-on cluster)",
        || max_committed(&nodes) >= 1,
        || describe_cluster(&nodes),
    );
    log_with_context(
        None,
        &format!("GREEN first commit. {}", describe_cluster(&nodes)),
    );

    // Steady advance: every node's commit frontier reaches GREEN_TARGET_COMMITS. The knob-on
    // system oscillates (commit bursts collapse the lag, churn regrows it), so the budget
    // spans several burst cycles.
    wait_until(
        Duration::from_secs(360),
        POLL_INTERVAL,
        &format!(
            "every node to commit at least {GREEN_TARGET_COMMITS} blocks (steady commit advance)"
        ),
        || min_committed(&nodes) >= GREEN_TARGET_COMMITS,
        || describe_cluster(&nodes),
    );
    log_with_context(
        None,
        &format!("GREEN steady advance reached. {}", describe_cluster(&nodes)),
    );
}
