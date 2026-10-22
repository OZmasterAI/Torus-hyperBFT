//! tc-liveness (s136): liveness when every ed25519 check is slow.
//!
//! A debug build on a loaded host takes tens of milliseconds per signature check (36 ms measured
//! on the CPU-busy torus-consensus p00 crossing test). Each replica checks on its one algorithm
//! thread, so the checks a message costs set how far that thread's queue falls behind. From
//! TC-required phases B and C a replica checked the same signatures over and over: every timeout
//! vote carries the sender's highest TC (its signatures, high QC and high tip), the honest votes
//! of a view report the same tip and QC, a leader re-sends its header (leader signature, justify
//! QC, TC) every few hundred milliseconds, and every validator re-broadcasts each AdvanceView.
//! The queues fell seconds behind, timeout votes for v-1 reached leader(v) after it had left v,
//! QCs formed only in long backed-off views, never two in a row, and nothing committed.
//!
//! Four equal validators on a healthy mock network, with [`CHECK_DELAY`] added to every check (a
//! test hook, so the outcome does not depend on the host's load). The cluster must keep
//! committing, with all four up and with one down (its leader slots are timeout views, which
//! need TC(v-1)). The hook is process wide: every test in this file uses the same delay.
//!
//! RED-first: on 56923063 (no repeat-check memo) neither case commits a block in 90 s (ubuntoz).
//! Main c3808eac, with the same delay on its check sites, passes both (15 s, 27 s with a 60 s
//! bound). With the memo both reach the target height in 9-15 s.

use std::time::{Duration, Instant};

use hotstuff_rs::types::{
    crypto_primitives::{set_signature_check_delay_for_tests, SigningKey},
    data_types::Power,
    update_sets::ValidatorSetUpdates,
};

mod common;

use common::{
    network::mock_network,
    node::Node,
    number_app::{NumberApp, NumberAppTransaction},
    poll::wait_until,
};

/// Added to every ed25519 check: about a debug-build check on a loaded host.
const CHECK_DELAY: Duration = Duration::from_millis(40);
const MAX_VIEW_TIME: Duration = Duration::from_millis(1500);
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Committed height every live validator must reach.
const TARGET_HEIGHT: u64 = 6;
const LIVENESS_BOUND: Duration = Duration::from_secs(90);

fn describe(nodes: &[Option<Node>]) -> String {
    nodes
        .iter()
        .enumerate()
        .map(|(i, n)| match n {
            Some(n) => format!(
                "n{i}{{committed={:?}, highest_pc_view={}, view={}}}",
                n.committed_height(),
                n.highest_pc_view(),
                n.highest_view_entered(),
            ),
            None => format!("n{i}{{down}}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Run four equal validators (keys `[1; 32]` .. `[4; 32]`) except `down`, with every signature
/// check slowed by [`CHECK_DELAY`], until every live one commits [`TARGET_HEIGHT`].
fn run_with_slow_checks(down: Option<usize>) {
    set_signature_check_delay_for_tests(CHECK_DELAY);

    let keypairs: Vec<SigningKey> = (1..=4u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let mut vs_updates = ValidatorSetUpdates::new();
    for kp in &keypairs {
        vs_updates.insert(kp.verifying_key(), Power::new(1));
    }
    let init_as = NumberApp::initial_app_state();
    let stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));

    let mut nodes: Vec<Option<Node>> = keypairs
        .into_iter()
        .zip(stubs)
        .enumerate()
        .map(|(i, (kp, net))| {
            (Some(i) != down).then(|| {
                Node::new_with_max_view_time(kp, net, init_as.clone(), vs_updates.clone(), MAX_VIEW_TIME)
            })
        })
        .collect();
    for node in nodes.iter_mut().flatten() {
        node.submit_transaction(NumberAppTransaction::Increment);
    }

    let started = Instant::now();
    wait_until(
        LIVENESS_BOUND,
        POLL_INTERVAL,
        &format!("every live validator to commit height >= {TARGET_HEIGHT} with {CHECK_DELAY:?} per signature check"),
        || nodes.iter().flatten().all(|n| n.committed_height().unwrap_or(0) >= TARGET_HEIGHT),
        || describe(&nodes),
    );
    eprintln!(
        "slow signature checks, down {down:?}: height {TARGET_HEIGHT} in {:?}: {}",
        started.elapsed(),
        describe(&nodes)
    );
}

#[test]
fn four_validators_keep_committing_when_every_signature_check_is_slow() {
    run_with_slow_checks(None);
}

#[test]
fn three_of_four_keep_committing_through_timeout_views_when_every_signature_check_is_slow() {
    run_with_slow_checks(Some(3));
}
