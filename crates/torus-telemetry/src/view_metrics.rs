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
    /// When the leader's proposal (or proposal header) first arrived this view.
    proposal_received_at: Option<SystemTime>,
    /// Guards `view_insert_persist_seconds` against multi-insert double counts.
    insert_observed: bool,
    /// Guards `view_vote_delay_seconds` against duplicate-vote double counts.
    vote_observed: bool,
    /// Previous CommitBlock event time, for the commit interval gap.
    last_commit_at: Option<SystemTime>,
    /// Our outstanding proposal awaiting certification: broadcast time and
    /// block hash. Survives view transitions — under leader rotation the PC
    /// for our block is assembled by the next leader, and we only learn of it
    /// when a later justify certifies our block (UpdateHighestPC). Cleared on
    /// match or replaced by our next proposal.
    last_propose: Option<(SystemTime, [u8; 32])>,
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
    /// `last_propose` deliberately survives — our block's certification
    /// arrives after the view transition under leader rotation.
    pub fn start_view(&self, ts: SystemTime, view: u64) {
        let mut s = self.state.lock().unwrap();
        if let Some(prev) = s.view_started_at {
            self.metrics
                .view_duration_seconds
                .observe(secs_between(prev, ts));
        }
        s.view_started_at = Some(ts);
        s.proposal_received_at = None;
        s.insert_observed = false;
        s.vote_observed = false;
        self.metrics.consensus_view.set(view as i64);
    }

    /// Propose (leader): time from view start to proposal broadcast. Also
    /// arms the QC round-trip clock for `block_hash`.
    pub fn propose(&self, ts: SystemTime, block_hash: [u8; 32]) {
        let mut s = self.state.lock().unwrap();
        if let Some(started) = s.view_started_at {
            self.metrics
                .view_propose_delay_seconds
                .observe(secs_between(started, ts));
        }
        s.last_propose = Some((ts, block_hash));
    }

    /// UpdateHighestPC: when the newly stored PC certifies OUR outstanding
    /// proposal, observe the full vote round-trip — from our broadcast to the
    /// certificate reaching us (via the next leader's justify). PCs for other
    /// replicas' blocks are ignored.
    pub fn update_highest_pc(&self, ts: SystemTime, pc_block: [u8; 32]) {
        let mut s = self.state.lock().unwrap();
        if let Some((proposed, ours)) = s.last_propose {
            if ours == pc_block {
                self.metrics
                    .view_qc_collect_seconds
                    .observe(secs_between(proposed, ts));
                s.last_propose = None;
            }
        }
    }

    /// ReceiveProposal / ReceiveProposalHeader (follower): how late the
    /// leader's proposal first arrives relative to our view start. Duplicate
    /// deliveries within the view keep the first arrival as the baseline.
    pub fn receive_proposal(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if s.proposal_received_at.is_some() {
            return;
        }
        if let Some(started) = s.view_started_at {
            self.metrics
                .view_proposal_arrival_seconds
                .observe(secs_between(started, ts));
        }
        s.proposal_received_at = Some(ts);
        s.insert_observed = false;
        s.vote_observed = false;
    }

    /// InsertBlock: proposal arrival to persisted insert (validate + block
    /// tree write + fsync). Only the first insert after a received proposal
    /// counts; block-sync inserts (no proposal) are ignored. In the hash-only
    /// pipeline the body insert happens after our vote, so this must not
    /// depend on vote order.
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

    /// PhaseVote (follower): proposal arrival to our vote leaving. First vote
    /// per received proposal only; leaves the arrival timestamp in place for
    /// the body insert that may still be in flight (hash-only pipeline).
    pub fn phase_vote(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if s.vote_observed {
            return;
        }
        if let Some(received) = s.proposal_received_at {
            self.metrics
                .view_vote_delay_seconds
                .observe(secs_between(received, ts));
            s.vote_observed = true;
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
    fn leader_qc_roundtrip_across_view_boundary() {
        let (m, r) = rec();
        let ours = [7u8; 32];
        r.start_view(t(0), 1);
        r.propose(t(50), ours);
        // Under leader rotation the PC for our block is assembled by the NEXT
        // leader; we only learn of it when a later justify certifies our block,
        // after our view has already ended. The round-trip must survive the
        // view transition.
        r.start_view(t(300), 2);
        // A foreign block's PC must not observe.
        r.update_highest_pc(t(320), [9u8; 32]);
        r.update_highest_pc(t(450), ours);
        // A repeat certification of the same block must not double count.
        r.update_highest_pc(t(460), ours);
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_delay_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_propose_delay_seconds_sum") - 0.05).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_qc_collect_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_qc_collect_seconds_sum") - 0.4).abs() < 1e-9);
    }

    #[test]
    fn follower_classic_order_insert_then_vote() {
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
    fn follower_header_order_vote_then_insert() {
        // Hash-only pipeline: the header arrives, we vote immediately, and the
        // body is fetched and inserted afterwards. All three follower metrics
        // must still observe.
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.receive_proposal(t(30));
        r.phase_vote(t(80));
        // Duplicate vote must not double count.
        r.phase_vote(t(85));
        r.insert_block(t(120));
        let text = m.encode();
        assert_eq!(
            sample(&text, "torus_view_proposal_arrival_seconds_count"),
            1.0
        );
        assert_eq!(sample(&text, "torus_view_vote_delay_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_vote_delay_seconds_sum") - 0.05).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_insert_persist_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_insert_persist_seconds_sum") - 0.09).abs() < 1e-9);
    }

    #[test]
    fn duplicate_proposal_keeps_first_arrival() {
        // Gossip can deliver the same header more than once; the first arrival
        // counts and later duplicates must not shift the baselines.
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.receive_proposal(t(30));
        r.receive_proposal(t(90));
        r.insert_block(t(130));
        let text = m.encode();
        assert_eq!(
            sample(&text, "torus_view_proposal_arrival_seconds_count"),
            1.0
        );
        assert!((sample(&text, "torus_view_proposal_arrival_seconds_sum") - 0.03).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_insert_persist_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_insert_persist_seconds_sum") - 0.1).abs() < 1e-9);
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
        r.propose(t(50), [0u8; 32]); // timestamp before view start
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_delay_seconds_count"), 1.0);
        assert_eq!(sample(&text, "torus_view_propose_delay_seconds_sum"), 0.0);
    }
}
