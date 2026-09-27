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

/// Unix microseconds (signed, like `BodyFetchTraceStamp`), `-` when absent.
struct Us(Option<SystemTime>);

impl std::fmt::Display for Us {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.map(|t| t.duration_since(SystemTime::UNIX_EPOCH)) {
            None => f.write_str("-"),
            Some(Ok(d)) => write!(f, "{}", d.as_micros()),
            Some(Err(e)) => write!(f, "-{}", e.duration().as_micros()),
        }
    }
}

/// One closed view as this node saw it (s70), returned by `start_view` so the
/// node can log it under `TORUS_BODY_FETCH_TRACE`; the three validators' lines
/// join per view offline (tools/matched-bench/gap_attr.py).
pub struct ViewTrace {
    view: u64,
    start: SystemTime,
    end: SystemTime,
    propose: Option<SystemTime>,
    proposal_rx: Option<SystemTime>,
    vote: Option<SystemTime>,
    timeout: Option<SystemTime>,
    /// PC collected in this view: its view, first vote received for it, PC time.
    pc: Option<(u64, Option<SystemTime>, SystemTime)>,
}

impl std::fmt::Display for ViewTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "view={} start_us={} end_us={} propose_us={} proposal_rx_us={} vote_us={} timeout_us={} ",
            self.view,
            Us(Some(self.start)),
            Us(Some(self.end)),
            Us(self.propose),
            Us(self.proposal_rx),
            Us(self.vote),
            Us(self.timeout)
        )?;
        match self.pc {
            Some((view, first, pc)) => write!(
                f,
                "pc_view={view} first_vote_rx_us={} pc_us={}",
                Us(first),
                Us(Some(pc))
            ),
            None => f.write_str("pc_view=- first_vote_rx_us=- pc_us=-"),
        }
    }
}

#[derive(Default)]
struct ViewState {
    /// The current view number (from StartView), for the closed-view trace.
    view: u64,
    /// When the current view started (StartView event).
    view_started_at: Option<SystemTime>,
    /// When the leader's proposal (or proposal header) first arrived this view.
    proposal_received_at: Option<SystemTime>,
    /// Guards `view_insert_persist_seconds` against multi-insert double counts.
    insert_observed: bool,
    /// Latest insert this view with NO received proposal — the leader's own
    /// block (insert happens right before Propose). Consumed by `propose()`
    /// for the build/finalize split; leadership is only confirmed there, so a
    /// block-sync insert that never leads to a Propose records nothing.
    self_insert_at: Option<SystemTime>,
    /// Our first vote after the received proposal; guards
    /// `view_vote_delay_seconds` against duplicate-vote double counts.
    vote_sent_at: Option<SystemTime>,
    /// When we proposed in the current view (timeout classification, s68).
    propose_at: Option<SystemTime>,
    /// First ViewTimeout of the current view; the view is classified once (the
    /// pacemaker re-emits ViewTimeout on later ticks of the same view).
    timeout_at: Option<SystemTime>,
    /// Collector (s70): first generic vote received, keyed by the vote's view.
    /// Votes can precede our entry into that view, so it survives StartView.
    first_vote: Option<(u64, SystemTime)>,
    /// Collector (s70): the last PC we assembled, until StartView moves past it.
    pc_collected: Option<(u64, SystemTime)>,
    /// The PC assembled in the current view, for the closed-view trace.
    trace_pc: Option<(u64, Option<SystemTime>, SystemTime)>,
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

    /// StartView: closes the previous view (observing its full duration and
    /// returning its trace), resets per-view state and updates the
    /// `torus_consensus_view` gauge. `last_propose` deliberately survives — our
    /// block's certification arrives after the view transition under leader
    /// rotation. Moving past a view we collected the PC for observes
    /// `view_qc_to_advance_seconds`.
    pub fn start_view(&self, ts: SystemTime, view: u64) -> Option<ViewTrace> {
        let mut s = self.state.lock().unwrap();
        if let Some((pc_view, collected)) = s.pc_collected {
            if view > pc_view {
                self.metrics
                    .view_qc_to_advance_seconds
                    .observe(secs_between(collected, ts));
                s.pc_collected = None;
            }
        }
        let closed = s.view_started_at.map(|start| {
            self.metrics
                .view_duration_seconds
                .observe(secs_between(start, ts));
            ViewTrace {
                view: s.view,
                start,
                end: ts,
                propose: s.propose_at,
                proposal_rx: s.proposal_received_at,
                vote: s.vote_sent_at,
                timeout: s.timeout_at,
                pc: s.trace_pc,
            }
        });
        s.view = view;
        s.view_started_at = Some(ts);
        s.proposal_received_at = None;
        s.insert_observed = false;
        s.vote_sent_at = None;
        s.self_insert_at = None;
        s.propose_at = None;
        s.timeout_at = None;
        s.trace_pc = None;
        self.metrics.consensus_view.set(view as i64);
        closed
    }

    /// StartView companion (s68): the entered view's pacemaker deadline minus
    /// the entry time, signed. A view entered at or past its deadline times out
    /// on the next tick; those are counted, positive slack is observed.
    pub fn view_entry_slack(&self, slack_secs: f64) {
        if slack_secs > 0.0 {
            self.metrics.view_entry_slack_seconds.observe(slack_secs);
        } else {
            self.metrics.view_entered_past_deadline.inc();
        }
    }

    /// ViewTimeout (s68): once per view, observe view start -> timeout and
    /// classify by what this node had seen: its own proposal, no proposal, a
    /// proposal it did not vote for, or its vote sent without a next view.
    pub fn view_timeout(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if s.timeout_at.is_some() {
            return;
        }
        s.timeout_at = Some(ts);
        if let Some(started) = s.view_started_at {
            self.metrics
                .view_timeout_after_seconds
                .observe(secs_between(started, ts));
        }
        let class = if s.propose_at.is_some() {
            &self.metrics.view_timeout_leader
        } else if s.proposal_received_at.is_none() {
            &self.metrics.view_timeout_no_proposal
        } else if s.vote_sent_at.is_none() {
            &self.metrics.view_timeout_no_vote
        } else {
            &self.metrics.view_timeout_after_vote
        };
        class.inc();
    }

    /// Propose (leader): time from view start to proposal broadcast. Also
    /// arms the QC round-trip clock for `block_hash`, and — when our own
    /// block's insert was seen this view — splits the delay into build
    /// (StartView -> insert: produce_block + block-tree write) and finalize
    /// (insert -> Propose: update/commit + event emit + broadcast handoff).
    pub fn propose(&self, ts: SystemTime, block_hash: [u8; 32]) {
        let mut s = self.state.lock().unwrap();
        if let Some(started) = s.view_started_at {
            self.metrics
                .view_propose_delay_seconds
                .observe(secs_between(started, ts));
            if let Some(inserted) = s.self_insert_at.take() {
                self.metrics
                    .view_propose_build_seconds
                    .observe(secs_between(started, inserted));
                self.metrics
                    .view_propose_finalize_seconds
                    .observe(secs_between(inserted, ts));
            }
        }
        if s.last_propose.is_some() {
            self.metrics.view_proposals_uncertified.inc();
        }
        s.last_propose = Some((ts, block_hash));
        s.propose_at = Some(ts);
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
        s.vote_sent_at = None;
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
        } else {
            // No received proposal: our own block (leader) or a sync insert.
            // Stash the latest; propose() consumes it only if we actually lead.
            s.self_insert_at = Some(ts);
        }
    }

    /// PhaseVote (follower): proposal arrival to our vote leaving. First vote
    /// per received proposal only; leaves the arrival timestamp in place for
    /// the body insert that may still be in flight (hash-only pipeline).
    pub fn phase_vote(&self, ts: SystemTime) {
        let mut s = self.state.lock().unwrap();
        if s.vote_sent_at.is_some() {
            return;
        }
        if let Some(received) = s.proposal_received_at {
            self.metrics
                .view_vote_delay_seconds
                .observe(secs_between(received, ts));
            s.vote_sent_at = Some(ts);
        }
    }

    /// ReceivePhaseVote (collector, generic phase only): keep the first vote
    /// arrival for the newest view seen; a late vote for an older view never
    /// replaces it.
    pub fn receive_phase_vote(&self, ts: SystemTime, view: u64) {
        let mut s = self.state.lock().unwrap();
        if s.first_vote.map_or(true, |(v, _)| view > v) {
            s.first_vote = Some((view, ts));
        }
    }

    /// CollectPC (collector, generic phase only): first vote for the PC's view
    /// to the PC (`view_vote_gather_seconds`), and arm the PC -> next StartView
    /// clock (`view_qc_to_advance_seconds`).
    pub fn collect_pc(&self, ts: SystemTime, view: u64) {
        let mut s = self.state.lock().unwrap();
        let first = match s.first_vote {
            Some((v, at)) if v == view => {
                s.first_vote = None;
                self.metrics
                    .view_vote_gather_seconds
                    .observe(secs_between(at, ts));
                Some(at)
            }
            _ => None,
        };
        s.pc_collected = Some((view, ts));
        s.trace_pc = Some((view, first, ts));
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
            "torus_view_vote_gather_seconds",
            "torus_view_qc_to_advance_seconds",
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
        assert_eq!(
            sample(&text, "torus_view_insert_persist_seconds_count"),
            1.0
        );
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
        assert_eq!(
            sample(&text, "torus_view_insert_persist_seconds_count"),
            1.0
        );
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
        assert_eq!(
            sample(&text, "torus_view_insert_persist_seconds_count"),
            1.0
        );
        assert!((sample(&text, "torus_view_insert_persist_seconds_sum") - 0.1).abs() < 1e-9);
    }

    #[test]
    fn sync_insert_without_proposal_not_recorded() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.insert_block(t(40));
        let text = m.encode();
        assert_eq!(
            sample(&text, "torus_view_insert_persist_seconds_count"),
            0.0
        );
    }

    /// S405 propose decomposition: on the leader, InsertBlock (own block, no
    /// received proposal) followed by Propose splits propose_delay into
    /// build (StartView -> insert) and finalize (insert -> Propose).
    #[test]
    fn leader_build_finalize_split() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.insert_block(t(80)); // own-block insert: no proposal received
        r.propose(t(130), [7u8; 32]);
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_build_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_propose_build_seconds_sum") - 0.08).abs() < 1e-9);
        assert_eq!(
            sample(&text, "torus_view_propose_finalize_seconds_count"),
            1.0
        );
        assert!((sample(&text, "torus_view_propose_finalize_seconds_sum") - 0.05).abs() < 1e-9);
        // Total still observed as before.
        assert!((sample(&text, "torus_view_propose_delay_seconds_sum") - 0.13).abs() < 1e-9);
        // Follower insert metric untouched by the leader path.
        assert_eq!(
            sample(&text, "torus_view_insert_persist_seconds_count"),
            0.0
        );
    }

    /// A sync insert (no proposal, no subsequent Propose in the view) must not
    /// record leader build/finalize — leadership is only confirmed at Propose.
    #[test]
    fn sync_insert_without_propose_records_no_build() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.insert_block(t(40));
        r.start_view(t(300), 2);
        r.propose(t(360), [7u8; 32]); // next view: leader, but no insert seen yet
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_build_seconds_count"), 0.0);
        assert_eq!(
            sample(&text, "torus_view_propose_finalize_seconds_count"),
            0.0
        );
    }

    /// The follower path (proposal received) must not feed the leader split.
    #[test]
    fn follower_insert_records_no_build() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.receive_proposal(t(30));
        r.insert_block(t(70));
        r.phase_vote(t(80));
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_propose_build_seconds_count"), 0.0);
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

    /// s68 pacemaker diagnosis: the entered view's deadline slack. A view
    /// entered at or past its deadline is counted; positive slack is observed.
    #[test]
    fn view_entry_slack_counts_views_entered_past_deadline() {
        let (m, r) = rec();
        r.view_entry_slack(0.4);
        r.view_entry_slack(-0.12);
        r.view_entry_slack(0.0);
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_entry_slack_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_entry_slack_seconds_sum") - 0.4).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_entered_past_deadline_total"), 2.0);
    }

    /// Each timed-out view is classified once, by what this node had seen when
    /// the deadline fired, and the time from view start to timeout is observed.
    #[test]
    fn view_timeout_classified_once_per_view() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.view_timeout(t(5)); // nothing arrived: instant burn
        r.view_timeout(t(9)); // repeat tick in the same view: ignored
        r.start_view(t(10), 2);
        r.receive_proposal(t(40));
        r.view_timeout(t(510)); // proposal but no vote
        r.start_view(t(520), 3);
        r.receive_proposal(t(550));
        r.phase_vote(t(560));
        r.view_timeout(t(1020)); // voted, no next view
        r.start_view(t(1030), 4);
        r.propose(t(1100), [4u8; 32]);
        r.view_timeout(t(1530)); // our own proposal never certified in time
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_timeout_no_proposal_total"), 1.0);
        assert_eq!(sample(&text, "torus_view_timeout_no_vote_total"), 1.0);
        assert_eq!(sample(&text, "torus_view_timeout_after_vote_total"), 1.0);
        assert_eq!(sample(&text, "torus_view_timeout_leader_total"), 1.0);
        assert_eq!(sample(&text, "torus_view_timeout_after_seconds_count"), 4.0);
        assert!(
            (sample(&text, "torus_view_timeout_after_seconds_sum") - (0.005 + 0.5 + 0.5 + 0.5))
                .abs()
                < 1e-9
        );
    }

    /// A proposal replaced by our next one before any PC certified it is
    /// counted as uncertified (the explorers' orphaned proposal). A certified
    /// proposal is not.
    #[test]
    fn uncertified_proposals_counted_when_replaced() {
        let (m, r) = rec();
        r.start_view(t(0), 1);
        r.propose(t(50), [1u8; 32]);
        r.update_highest_pc(t(300), [1u8; 32]);
        r.start_view(t(600), 4);
        r.propose(t(650), [2u8; 32]);
        r.start_view(t(1200), 7);
        r.propose(t(1250), [3u8; 32]); // [2] was never certified
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_proposals_uncertified_total"), 1.0);
    }

    /// s70 proposal->QC split, collector side: first generic vote received for
    /// view v to the PC for v (vote gather), then that PC to our next StartView
    /// (update/commit feed + AdvanceView broadcast + pacemaker entry).
    #[test]
    fn collector_vote_gather_and_qc_to_advance() {
        let (m, r) = rec();
        r.start_view(t(0), 5);
        r.receive_phase_vote(t(10), 4); // stale vote for an older view: not the baseline
        r.receive_phase_vote(t(20), 5);
        r.receive_phase_vote(t(35), 5); // later votes keep the first arrival
        r.collect_pc(t(40), 5);
        r.start_view(t(100), 6);
        r.start_view(t(400), 7); // no PC collected in view 6: nothing more
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_vote_gather_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_vote_gather_seconds_sum") - 0.02).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_view_qc_to_advance_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_qc_to_advance_seconds_sum") - 0.06).abs() < 1e-9);
    }

    /// A PC for a view with no recorded vote (votes arrived before a restart,
    /// or were for another view) observes no gather, but still arms the
    /// advance clock; a StartView that does not move past the PC's view does
    /// not consume it.
    #[test]
    fn pc_without_vote_baseline_only_times_advance() {
        let (m, r) = rec();
        r.start_view(t(0), 8);
        r.receive_phase_vote(t(5), 7);
        r.collect_pc(t(50), 8);
        r.start_view(t(60), 8); // re-entry of the same view: not an advance
        r.start_view(t(90), 9);
        let text = m.encode();
        assert_eq!(sample(&text, "torus_view_vote_gather_seconds_count"), 0.0);
        assert_eq!(sample(&text, "torus_view_qc_to_advance_seconds_count"), 1.0);
        assert!((sample(&text, "torus_view_qc_to_advance_seconds_sum") - 0.04).abs() < 1e-9);
    }

    /// StartView closes the previous view into one trace record with every
    /// timestamp this node saw in it (unix us; `-` when absent), so the three
    /// validators' lines can be joined per view offline.
    #[test]
    fn start_view_returns_closed_view_trace() {
        let (_m, r) = rec();
        assert!(
            r.start_view(t(1_000), 3).is_none(),
            "no previous view to close"
        );
        r.receive_proposal(t(1_020));
        r.phase_vote(t(1_030));
        r.receive_phase_vote(t(1_031), 3);
        r.collect_pc(t(1_045), 3);
        let tr = r.start_view(t(1_050), 4).expect("view 3 closed");
        assert_eq!(
            tr.to_string(),
            "view=3 start_us=1000000 end_us=1050000 propose_us=- proposal_rx_us=1020000 \
             vote_us=1030000 timeout_us=- pc_view=3 first_vote_rx_us=1031000 pc_us=1045000"
        );
        // Leader view that timed out: propose and timeout, no PC.
        r.propose(t(1_060), [1u8; 32]);
        r.view_timeout(t(2_250));
        let tr = r.start_view(t(2_260), 5).unwrap();
        assert_eq!(
            tr.to_string(),
            "view=4 start_us=1050000 end_us=2260000 propose_us=1060000 proposal_rx_us=- \
             vote_us=- timeout_us=2250000 pc_view=- first_vote_rx_us=- pc_us=-"
        );
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
