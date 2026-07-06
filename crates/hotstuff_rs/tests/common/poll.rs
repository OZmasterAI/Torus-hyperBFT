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
