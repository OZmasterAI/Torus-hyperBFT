//! s131: metrics for the collector's `qc_to_advance` leg and the commit path
//! inside it (see `hotstuff_rs::qc_advance_timers` for the step definitions).
//!
//! Per advanced collector view (one observation each):
//! `torus_qc_advance_<step>_seconds` for every step in [`QC_ADVANCE_STEPS`]
//! (they sum to `torus_qc_advance_total_seconds`),
//! `torus_qc_advance_own_loopback_seconds` (AdvanceView broadcast -> our own
//! copy back on the consensus thread), and counters for committed blocks,
//! messages handled while waiting and what advanced the view.
//!
//! Per committed block (torus-consensus `on_committed_block`, consensus
//! thread): `torus_commit_feed_{decode,evm,manifest,enqueue}_seconds`, and per
//! exec dispatch `torus_exec_dispatch_send_seconds` (time in the exec channel
//! send, i.e. blocked on a full bounded queue). Per AdvanceView broadcast:
//! `torus_qc_advance_swarm_cmd_seconds` (network command queue wait until the
//! swarm loop self-delivers it). Per `on_commit_block` event:
//! `torus_commit_event_handler_seconds` (event-bus thread).

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

/// Step names, in the order of `hotstuff_rs::qc_advance_timers::STEP_NAMES`.
pub const QC_ADVANCE_STEPS: [&str; 15] = [
    "pc_check",
    "highest_pc",
    "tree_update",
    "commit_feed_tree",
    "commit_feed_app",
    "post_commit",
    "advance_send",
    "wait_busy",
    "wait_recv",
    "wait_msg",
    "advance_rx",
    "loop_to_enter",
    "enter_new_view",
    "enter_view_write",
    "enter_start",
];

const STEP_HELP: [&str; 15] = [
    "qc_to_advance: collected PC correctness/safety checks",
    "qc_to_advance: advance_highest_pc_from_remote",
    "qc_to_advance: block_tree.update (lock + durable commit write)",
    "qc_to_advance: committed-block feed minus the app callbacks (tree reads, fed-frontier write)",
    "qc_to_advance: app on_committed_block calls in the feed",
    "qc_to_advance: speculative commit + collector validator-set update",
    "qc_to_advance: AdvanceView broadcast hand-off",
    "qc_to_advance: algorithm-loop work (not receive, not message handling) while waiting for the AdvanceView",
    "qc_to_advance: blocked in the progress-message receive while waiting for the AdvanceView",
    "qc_to_advance: handling other messages while waiting for the AdvanceView",
    "qc_to_advance: handling the AdvanceView that moved the pacemaker",
    "qc_to_advance: rest of the loop pass + pacemaker tick until enter_view",
    "qc_to_advance: enter_view NewView build + send",
    "qc_to_advance: enter_view set_highest_view_entered write",
    "qc_to_advance: enter_view leader selection up to StartView",
];

/// What moved the pacemaker into the next view (mirrors
/// `hotstuff_rs::qc_advance_timers::AdvancedBy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QcAdvanceBy {
    OwnAdvanceView,
    PeerAdvanceView,
    Other,
}

fn fine_buckets() -> impl Iterator<Item = f64> {
    // 10 µs .. ~5.2 s
    exponential_buckets(0.000_01, 2.0, 20)
}

pub struct QcAdvanceMetrics {
    /// One per [`QC_ADVANCE_STEPS`] entry, same order.
    pub step_seconds: Vec<Histogram>,
    pub total_seconds: Histogram,
    pub own_loopback_seconds: Histogram,
    pub swarm_cmd_seconds: Histogram,
    pub committed_blocks: Counter,
    pub wait_msgs: Counter,
    pub by_own_advance_view: Counter,
    pub by_peer_advance_view: Counter,
    pub by_other: Counter,
    pub commit_feed_decode_seconds: Histogram,
    pub commit_feed_evm_seconds: Histogram,
    pub commit_feed_manifest_seconds: Histogram,
    pub commit_feed_enqueue_seconds: Histogram,
    pub exec_dispatch_send_seconds: Histogram,
    pub commit_event_handler_seconds: Histogram,
}

impl QcAdvanceMetrics {
    pub(crate) fn register(registry: &mut Registry) -> Self {
        let step_seconds = QC_ADVANCE_STEPS
            .iter()
            .zip(STEP_HELP)
            .map(|(name, help)| {
                let h = Histogram::new(fine_buckets());
                registry.register(format!("torus_qc_advance_{name}_seconds"), help, h.clone());
                h
            })
            .collect();
        let hist = |registry: &mut Registry, name: &str, help: &str| {
            let h = Histogram::new(fine_buckets());
            registry.register(name, help, h.clone());
            h
        };
        let counter = |registry: &mut Registry, name: &str, help: &str| {
            let c = Counter::default();
            registry.register(name, help, c.clone());
            c
        };
        Self {
            step_seconds,
            total_seconds: hist(
                registry,
                "torus_qc_advance_total_seconds",
                "qc_to_advance (s131, Instant clock): collected PC to next StartView; the sum of the step histograms",
            ),
            own_loopback_seconds: hist(
                registry,
                "torus_qc_advance_own_loopback_seconds",
                "qc_to_advance: AdvanceView broadcast to our own copy received on the consensus thread",
            ),
            swarm_cmd_seconds: hist(
                registry,
                "torus_qc_advance_swarm_cmd_seconds",
                "AdvanceView broadcast command: queued by consensus to self-delivered by the swarm loop",
            ),
            committed_blocks: counter(
                registry,
                "torus_qc_advance_committed_blocks",
                "qc_to_advance: blocks fed to the app in the collector handler",
            ),
            wait_msgs: counter(
                registry,
                "torus_qc_advance_wait_msgs",
                "qc_to_advance: messages handled while waiting for the AdvanceView",
            ),
            by_own_advance_view: counter(
                registry,
                "torus_qc_advance_by_own_advance_view",
                "qc_to_advance views advanced by our own AdvanceView (self-delivery)",
            ),
            by_peer_advance_view: counter(
                registry,
                "torus_qc_advance_by_peer_advance_view",
                "qc_to_advance views advanced by a peer's AdvanceView",
            ),
            by_other: counter(
                registry,
                "torus_qc_advance_by_other",
                "qc_to_advance views advanced otherwise (timeout, round skip)",
            ),
            commit_feed_decode_seconds: hist(
                registry,
                "torus_commit_feed_decode_seconds",
                "on_committed_block: committed datum decode, per block",
            ),
            commit_feed_evm_seconds: hist(
                registry,
                "torus_commit_feed_evm_seconds",
                "on_committed_block: EVM pool work (remove committed, reinsert lost, resend due), per block",
            ),
            commit_feed_manifest_seconds: hist(
                registry,
                "torus_commit_feed_manifest_seconds",
                "on_committed_block: persist_commit_manifest, per block",
            ),
            commit_feed_enqueue_seconds: hist(
                registry,
                "torus_commit_feed_enqueue_seconds",
                "on_committed_block: enqueue_for_execution (materialize, dispatch prep, exec send), per block",
            ),
            exec_dispatch_send_seconds: hist(
                registry,
                "torus_exec_dispatch_send_seconds",
                "dispatch_to_exec: time in the exec channel send (blocked on a full queue), per dispatch",
            ),
            commit_event_handler_seconds: hist(
                registry,
                "torus_commit_event_handler_seconds",
                "on_commit_block event handler (event-bus thread: mempool prune, header read, notify), per event",
            ),
        }
    }

    /// One advanced collector view: `steps` in [`QC_ADVANCE_STEPS`] order
    /// (extra entries ignored, missing ones not observed).
    pub fn observe_view(
        &self,
        steps: &[f64],
        total: f64,
        own_loopback: Option<f64>,
        committed_blocks: u64,
        wait_msgs: u64,
        by: QcAdvanceBy,
    ) {
        for (h, secs) in self.step_seconds.iter().zip(steps) {
            h.observe(*secs);
        }
        self.total_seconds.observe(total);
        if let Some(l) = own_loopback {
            self.own_loopback_seconds.observe(l);
        }
        self.committed_blocks.inc_by(committed_blocks);
        self.wait_msgs.inc_by(wait_msgs);
        match by {
            QcAdvanceBy::OwnAdvanceView => self.by_own_advance_view.inc(),
            QcAdvanceBy::PeerAdvanceView => self.by_peer_advance_view.inc(),
            QcAdvanceBy::Other => self.by_other.inc(),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Metrics;

    fn sample(text: &str, name: &str) -> f64 {
        text.lines()
            .find(|l| l.starts_with(name) && l.as_bytes().get(name.len()) == Some(&b' '))
            .unwrap_or_else(|| panic!("sample {name} not found"))
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn qc_advance_metrics_register_and_record() {
        let m = Metrics::new();
        let text = m.encode();
        for step in QC_ADVANCE_STEPS {
            let name = format!("torus_qc_advance_{step}_seconds");
            assert!(text.contains(&name), "{name} not registered");
        }
        for name in [
            "torus_qc_advance_total_seconds",
            "torus_qc_advance_own_loopback_seconds",
            "torus_qc_advance_swarm_cmd_seconds",
            "torus_qc_advance_committed_blocks_total",
            "torus_qc_advance_wait_msgs_total",
            "torus_qc_advance_by_own_advance_view_total",
            "torus_qc_advance_by_peer_advance_view_total",
            "torus_qc_advance_by_other_total",
            "torus_commit_feed_decode_seconds",
            "torus_commit_feed_evm_seconds",
            "torus_commit_feed_manifest_seconds",
            "torus_commit_feed_enqueue_seconds",
            "torus_exec_dispatch_send_seconds",
            "torus_commit_event_handler_seconds",
        ] {
            assert!(text.contains(name), "{name} not registered");
        }

        let mut steps = [0.0; 15];
        steps[2] = 0.004; // tree_update
        steps[8] = 0.020; // wait_recv
        m.qc_advance
            .observe_view(&steps, 0.024, Some(0.021), 3, 5, QcAdvanceBy::OwnAdvanceView);
        m.qc_advance
            .observe_view(&steps, 0.024, None, 1, 0, QcAdvanceBy::Other);
        m.qc_advance.exec_dispatch_send_seconds.observe(0.002);
        let text = m.encode();
        assert_eq!(sample(&text, "torus_qc_advance_tree_update_seconds_count"), 2.0);
        assert!((sample(&text, "torus_qc_advance_wait_recv_seconds_sum") - 0.040).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_qc_advance_pc_check_seconds_count"), 2.0);
        assert!((sample(&text, "torus_qc_advance_total_seconds_sum") - 0.048).abs() < 1e-9);
        assert_eq!(sample(&text, "torus_qc_advance_own_loopback_seconds_count"), 1.0);
        assert_eq!(sample(&text, "torus_qc_advance_committed_blocks_total"), 4.0);
        assert_eq!(sample(&text, "torus_qc_advance_wait_msgs_total"), 5.0);
        assert_eq!(sample(&text, "torus_qc_advance_by_own_advance_view_total"), 1.0);
        assert_eq!(sample(&text, "torus_qc_advance_by_other_total"), 1.0);
        assert_eq!(sample(&text, "torus_qc_advance_by_peer_advance_view_total"), 0.0);
        assert_eq!(sample(&text, "torus_exec_dispatch_send_seconds_count"), 1.0);
    }
}
