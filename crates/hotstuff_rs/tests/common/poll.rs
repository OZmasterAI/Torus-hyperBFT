//! Bounded polling helper for integration tests.
//!
//! Integration tests poll cluster state in a `while !condition { sleep(500ms) }` loop. Left
//! unbounded, a single cluster stall hangs the loop forever. Under `cargo test` (which, unlike
//! `nextest`, has no per-test timeout) this wedges the whole CI job. [`wait_until`] bounds the
//! loop with a deadline so a stall becomes a fast, diagnosable failure instead of a hang.

use std::{
    thread,
    time::{Duration, Instant},
};

/// Poll `condition` every `poll_interval` until it returns `true`, or panic once `timeout` has
/// elapsed.
///
/// On expiry, panics with `context` (a human-readable description of what was being waited for)
/// and the string returned by `describe` (the last observed cluster state, e.g. heights/view
/// numbers), so that a CI failure is diagnosable straight from the test log.
pub(crate) fn wait_until<C, D>(
    timeout: Duration,
    poll_interval: Duration,
    context: &str,
    mut condition: C,
    mut describe: D,
) where
    C: FnMut() -> bool,
    D: FnMut() -> String,
{
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "wait_until timed out after {:?} waiting for: {}\n  last observed state: {}",
                timeout,
                context,
                describe(),
            );
        }
        thread::sleep(poll_interval);
    }
}

/// One observation of a scenario being set up: see [`wait_for_scenario`].
pub(crate) enum Scenario {
    /// The state the test wants has been reached.
    Reached,
    /// Not there yet; the cluster may still get there.
    Pending,
    /// The run went past the state without reaching it and can never reach it (e.g. a node left
    /// the view whose vote the test meant to catch). The caller may start a fresh run.
    Missed,
}

/// Poll `observe` until it reports [`Scenario::Reached`] (returns `true`) or
/// [`Scenario::Missed`] (returns `false`). Panics if `progress` (a counter that grows while the
/// cluster moves: views entered, certificates, commits) stays the same for `stall`, or if `ceiling`
/// elapses in total.
///
/// Unlike [`wait_until`]'s single deadline, a slow but moving cluster (a loaded host) is not
/// failed early, while a cluster that stops moving still fails within `stall`.
pub(crate) fn wait_for_scenario<O, P, D>(
    ceiling: Duration,
    stall: Duration,
    poll_interval: Duration,
    context: &str,
    mut observe: O,
    mut progress: P,
    mut describe: D,
) -> bool
where
    O: FnMut() -> Scenario,
    P: FnMut() -> u64,
    D: FnMut() -> String,
{
    let start = Instant::now();
    let (mut last, mut last_change) = (progress(), start);
    loop {
        match observe() {
            Scenario::Reached => return true,
            Scenario::Missed => return false,
            Scenario::Pending => {}
        }
        let now = Instant::now();
        let p = progress();
        if p != last {
            (last, last_change) = (p, now);
        }
        if now - last_change >= stall || now - start >= ceiling {
            panic!(
                "wait_for_scenario: {} after {:?} (no progress for {:?}; stall bound {:?}, ceiling \
                 {:?}) waiting for: {}\n  last observed state: {}",
                if now - start >= ceiling { "ceiling hit" } else { "stalled" },
                now - start,
                now - last_change,
                stall,
                ceiling,
                context,
                describe(),
            );
        }
        thread::sleep(poll_interval);
    }
}
