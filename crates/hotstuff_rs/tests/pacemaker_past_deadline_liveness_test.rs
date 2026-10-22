//! Past deadline after a backed-off view (s137, docs/plans/tc-required-and-tip-signing.md, "As
//! fixed (pacemaker past deadline)").
//!
//! The pacemaker schedules every view of an epoch on an absolute grid (`base + k * max_view_time`)
//! and Task A stretches a stalled view in place (`entry + max_view_time * multiplier`) without
//! moving the views after it. After a run of backed-off views the grid lies seconds in the past.
//! When the backed-off view finally gets its QC, every replica enters the next view at
//! multiplier 1 with that view's scheduled deadline already gone and times it out on the next
//! tick, so that view never gets its QC; the view after it is backed off again (x2), gets a QC,
//! and the pattern repeats until the grid has caught up with real time. QCs in two consecutive
//! views, which the 2-chain commit rule needs, only return once that deficit is used up: about one
//! lost view per 0.75 s of deficit here.
//!
//! The cluster: four equal validators on a healthy mock network, with every HotStuff message
//! (headers, votes, new-views, block data) of views [`STALL`] dropped, so those views time out with
//! a growing backoff (x1, x2, x4, x8) while timeout votes and TCs still flow. The first view after
//! the stall gets a QC; the cluster must then form QCs in two consecutive views within
//! [`CONSECUTIVE_BOUND`] views of it.
//!
//! RED-first: on 0937f178 the QCs after the stall fall on every other view (q0, q0 + 2, ...) well
//! past the bound.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::RangeInclusive,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use hotstuff_rs::{
    hotstuff::messages::HotStuffMessage,
    networking::messages::{Message, ProgressMessage},
    types::{crypto_primitives::SigningKey, data_types::Power, update_sets::ValidatorSetUpdates},
};

mod common;

use common::{
    network::mock_network_with_filter,
    node::{Node, WedgeOptions},
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

const MAX_VIEW_TIME: Duration = Duration::from_millis(500);
/// Views whose HotStuff traffic is dropped: four timeout views, x1 .. x8.
const STALL: RangeInclusive<u64> = 8..=11;
/// After the first QC above the stall (view q0), a QC in views w and w + 1 with
/// w + 1 <= q0 + CONSECUTIVE_BOUND. Fixed: q0 and q0 + 1.
const CONSECUTIVE_BOUND: u64 = 4;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const BOUND: Duration = Duration::from_secs(90);
/// Quorum of four equal validators.
const QUORUM: usize = 3;

/// Voters per view, from the phase votes the network delivered.
type Votes = Arc<Mutex<BTreeMap<u64, BTreeSet<[u8; 32]>>>>;

fn qc_views(votes: &Votes) -> BTreeSet<u64> {
    votes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, voters)| voters.len() >= QUORUM)
        .map(|(view, _)| *view)
        .collect()
}

fn describe(nodes: &[Node], votes: &Votes) -> String {
    let nodes = nodes
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
        .join(", ");
    format!("{nodes}; QC views {:?}", qc_views(votes))
}

#[test]
fn consecutive_qcs_return_right_after_a_backed_off_stall() {
    let keypairs: Vec<SigningKey> = (1..=4u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1));
    }
    let init_as = NumberApp::initial_app_state();
    let (stubs, filter) = mock_network_with_filter(keypairs.iter().map(|kp| kp.verifying_key()));

    let votes: Votes = Arc::default();
    let votes_in = votes.clone();
    filter.rewrite(Arc::new(move |sender, _recipient, message| {
        if let Message::ProgressMessage(ProgressMessage::HotStuffMessage(hs)) = &message {
            if STALL.contains(&hs.view().int()) {
                return None;
            }
            if let HotStuffMessage::PhaseVote(vote) = hs {
                votes_in.lock().unwrap().entry(vote.view.int()).or_default().insert(sender.to_bytes());
            }
        }
        Some(message)
    }));

    // Short produce/validate so a QC round fits well inside one view time.
    let options = WedgeOptions {
        produce_delay: Duration::from_millis(50),
        validate_delay: Duration::from_millis(50),
        commit_lag_cap: 0,
    };
    let mut nodes: Vec<Node> = keypairs
        .into_iter()
        .zip(stubs)
        .map(|(kp, net)| {
            Node::new_with_wedge_options(kp, net, init_as.clone(), vs_updates.clone(), MAX_VIEW_TIME, options)
        })
        .collect();
    for node in nodes.iter_mut() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    let stall_end = *STALL.end();
    let started = Instant::now();
    let first_qc_after_stall = || qc_views(&votes).into_iter().find(|v| *v > stall_end);
    wait_until(
        BOUND,
        POLL_INTERVAL,
        &format!("a QC above the stalled views {STALL:?}"),
        || first_qc_after_stall().is_some(),
        || describe(&nodes, &votes),
    );
    let q0 = first_qc_after_stall().unwrap();
    let limit = q0 + CONSECUTIVE_BOUND;
    let consecutive = || {
        let qcs = qc_views(&votes);
        qcs.iter().copied().find(|w| *w >= q0 && *w < limit && qcs.contains(&(w + 1)))
    };
    // Decided once a consecutive pair exists or the QCs have moved past the bound.
    wait_until(
        BOUND.saturating_sub(started.elapsed()),
        POLL_INTERVAL,
        &format!("QCs in two consecutive views by view {limit}, or QCs past it"),
        || consecutive().is_some() || qc_views(&votes).last().is_some_and(|v| *v > limit),
        || describe(&nodes, &votes),
    );
    let qcs_after: Vec<u64> = qc_views(&votes).into_iter().filter(|v| *v >= q0).collect();
    eprintln!(
        "past-deadline: first QC after the stall {q0}, QC views from it {qcs_after:?}, {:?}: {}",
        started.elapsed(),
        describe(&nodes, &votes),
    );
    assert!(
        consecutive().is_some(),
        "after the backed-off stall {STALL:?} (first QC at view {q0}) no two consecutive views \
         had QCs by view {limit}: QC views {qcs_after:?}. A view entered on the QC of a \
         backed-off view must get a full view time, not a past deadline"
    );
}
