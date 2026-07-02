//! View-phase timing recorder: translates hotstuff replica lifecycle events
//! into per-phase histograms so each node self-reports where its view time
//! goes (leader build, QC collection, proposal arrival, persist, vote).
//!
//! Event timestamps are taken from the events themselves (emission time on
//! the algorithm thread), not from handler execution time, so event-bus
//! queuing delay does not pollute the measurements. Negative deltas from
//! wall-clock adjustments are clamped to zero.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::Metrics;

/// Seconds from `earlier` to `later`, clamped to zero on clock regression.
fn secs_between(earlier: SystemTime, later: SystemTime) -> f64 {
    later
        .duration_since(earlier)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Default)]
struct ViewState {
    /// When the current view started (StartView event).
    view_started_at: Option<SystemTime>,
    /// When we broadcast our proposal this view (leader only).
    proposed_at: Option<SystemTime>,
    /// When the leader's proposal arrived this view (follower only).
    proposal_received_at: Option<SystemTime>,
    /// Guards `view_insert_persist_seconds` against multi-insert double counts.
    insert_observed: bool,
    /// Previous CommitBlock event time, for the commit interval gap.
    last_commit_at: Option<SystemTime>,
}

/// Translates replica lifecycle callbacks into view-phase histograms.
///
/// Register one method per hotstuff event on the `ReplicaSpec` builder,
/// passing each event's own `timestamp`.
pub struct ViewMetricsRecorder {
    metrics: Arc<Metrics>,
    state: Mutex<ViewState>,
}

impl ViewMetricsRecorder {
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self {
            metrics,
            state: Mutex::new(ViewState::default()),
        }
    }

    /// StartView: closes the previous view (observing its full duration),
    /// resets per-view state and updates the `torus_consensus_view` gauge.
    pub fn start_view(&self, ts: SystemTime, view: u64) {
        let mut s = self.state.lock().unwrap();
        if let Some(prev) = s.view_started_at {
            self.metrics
                .view_duration_seconds
                .observe(secs_between(prev, ts));
        }
        s.view_started_at = Some(ts);
        s.proposed_at = None;
        s.proposal_received_at = None;
        s.insert_observed = false;
        self.metrics.consensus_view.set(view as i64);
    }

    /// Propose (leader): time from view start to proposal broadcast.
    pub fn propose(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if let Some(started) = s.view_started_at {
            self.metrics
                .view_propose_delay_seconds
                .observe(secs_between(started, ts));
        }
        s.proposed_at = Some(ts);
    }

    /// CollectPC (leader): vote round-trip from our proposal broadcast to
    /// the phase certificate assembling (includes remote validate+persist).
    pub fn collect_pc(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if let Some(proposed) = s.proposed_at.take() {
            self.metrics
                .view_qc_collect_seconds
                .observe(secs_between(proposed, ts));
        }
    }

    /// ReceiveProposal (follower): how late the leader's proposal arrives
    /// relative to our view start.
    pub fn receive_proposal(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if let Some(started) = s.view_started_at {
            self.metrics
                .view_proposal_arrival_seconds
                .observe(secs_between(started, ts));
        }
        s.proposal_received_at = Some(ts);
        s.insert_observed = false;
    }

    /// InsertBlock: proposal arrival to persisted insert (validate + block
    /// tree write + fsync). Only the first insert after a received proposal
    /// counts; block-sync inserts (no proposal) are ignored.
    pub fn insert_block(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if s.insert_observed {
            return;
        }
        if let Some(received) = s.proposal_received_at {
            self.metrics
                .view_insert_persist_seconds
                .observe(secs_between(received, ts));
            s.insert_observed = true;
        }
    }

    /// PhaseVote (follower): proposal arrival to our vote leaving.
    pub fn phase_vote(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if let Some(received) = s.proposal_received_at.take() {
            self.metrics
                .view_vote_delay_seconds
                .observe(secs_between(received, ts));
        }
    }

    /// CommitBlock: gap between consecutive local commits (chain cadence as
    /// this node observes it).
    pub fn commit_block(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if let Some(prev) = s.last_commit_at {
            self.metrics
                .commit_interval_seconds
                .observe(secs_between(prev, ts));
        }
        s.last_commit_at = Some(ts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t(ms: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(ms)
    }

    fn rec() -> (Arc<Metrics>, ViewMetricsRecorder) {
        let m = Arc::new(Metrics::new());
        let r = ViewMetricsRecorder::new(m.clone());
        (m, r)
    }

    /// Extract the value of a plain `name value` sample line from the
    /// OpenMetrics encoding.
    fn sample(text: &str, name: &str) -> f64 {
        text.lines()
            .find(|l| l.starts_with(name) && l.as_bytes().get(name.len()) == Some(&b' '))
            .unwrap_or_else(|| panic!("sample {name} not found:\n{text}"))
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn view_metrics_register() {
        let (m, _r) = rec();
        let text = m.encode();
        for name in [
            "torus_view_duration_seconds",
            "torus_view_propose_delay_seconds",
            "torus_view_qc_collect_seconds",
            "torus_view_proposal_arrival_seconds",
            "torus_view_insert_persist_seconds",
            "torus_view_vote_delay_seconds",
            "torus_commit_interval_seconds",
        ] {
            assert!(text.contains(name), "{name} not registered:\n{text}");
        }
    }

    #[test]
    fn view_duration_and_gauge_across_views() {
        let (m, r) = rec();
        r.start_view(t(0), 7);
        r.start_view(t(300), 8);
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_duration_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_duration_seconds_sum") - 0.3).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_consensus_view"), 8.0);
    }

    #[test]
    fn leader_view_records_propose_and_qc() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.propose(t(50));
        r.collect_pc(t(150));
        // A second PC without a new proposal must not double count.
        r.collect_pc(t(160));
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_delay_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_propose_delay_seconds_sum") - 0.05).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_qc_collect_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_qc_collect_seconds_sum") - 0.1).abs() < 1e-9);
    }

    #[test]
    fn follower_view_records_arrival_insert_vote() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.receive_proposal(t(30));
        r.insert_block(t(70));
        // Duplicate insert for the same proposal must not double count.
        r.insert_block(t(75));
        r.phase_vote(t(80));
        let text = m.encode();
        assert_eq!(
            sample(&text, "torus_view_proposal_arrival_seconds_count"),
            1.0
        );
        assert!((sample(&text, "torus_view_proposal_arrival_seconds_sum") - 0.03).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_insert_persist_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_insert_persist_seconds_sum") - 0.04).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_vote_delay_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_vote_delay_seconds_sum") - 0.05).abs() < 1e-9);
    }

    #[test]
    fn sync_insert_without_proposal_not_recorded() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.insert_block(t(40));
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_insert_persist_seconds_count"), 0.0);
    }

    #[test]
    fn commit_interval_counts_gaps() {
        let (m, r) = rec();
        r.commit_block(t(0));
        r.commit_block(t(400));
        let text = m.encode();
        assert_eq!(sample(&text, "torus_commit_interval_seconds_count"), 1.0);
        assert!((sample(&text, "torus_commit_interval_seconds_sum") - 0.4).abs() < 1e-9);
    }

    #[test]
    fn clock_regression_clamps_to_zero() {
        let (m, r) = rec();
        r.start_view(t(100), 1);
        r.propose(t(50)); // timestamp before view start
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_delay_seconds_count"), 1.0);
        assert_eq!(sample(&text, "torus_view_propose_delay_seconds_sum"), 0.0);
    }
}
