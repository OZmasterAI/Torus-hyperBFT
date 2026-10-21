//! P0.0 phase 6 (docs/plans/p00-activation-height-gating.md, "Build phases"):
//! the feature-schedule gauges an operator watches before and at an
//! activation height.
//!
//! - [`FeatureMonitor`] (execution thread): which features the last applied
//!   block's header carries (`torus_feature_active`), the schedule's heights
//!   (`torus_feature_activation_height`), the next activation above the
//!   applied height and the blocks until it.
//! - [`report_upgrade_required`] (vote, insertion, proposal and execution
//!   sites): a header carries a feature this binary does not know
//!   (`torus_upgrade_required` = 1, the feature id and height, a refusal
//!   counter by site).
//!
//! Operator text: `docs/node-operator-guide.md`, "Feature activations and
//! upgrades".

use torus_telemetry::Gauge;
use torus_types::{ActivationSchedule, Feature, TorusBlockHeader};

/// The `feature` / `id` labels of a feature's series.
pub(crate) fn feature_labels(feature: Feature) -> Vec<(String, String)> {
    vec![
        ("feature".to_string(), feature.name().to_string()),
        ("id".to_string(), feature.id().to_string()),
    ]
}

/// The features of `known` (a bitmap; the node passes `KNOWN_FEATURES`), in
/// id order.
pub(crate) fn known_feature_list(known: u128) -> impl Iterator<Item = Feature> {
    Feature::ALL
        .iter()
        .copied()
        .filter(move |f| known & f.bit() != 0)
}

struct Gauges {
    /// `torus_feature_active` of each known feature, resolved once.
    active: Vec<(u128, Gauge)>,
    next: Gauge,
    until: Gauge,
    schedule: ActivationSchedule,
}

/// The execution thread's feature gauges (`Default`: no metrics, reports
/// nothing).
#[derive(Default)]
pub(crate) struct FeatureMonitor {
    gauges: Option<Gauges>,
}

impl FeatureMonitor {
    /// Resolve the gauges and report the schedule (one
    /// `torus_feature_activation_height` series per scheduled feature, every
    /// known feature inactive until a block is reported).
    pub(crate) fn new(
        metrics: Option<&torus_telemetry::Metrics>,
        schedule: &ActivationSchedule,
        known: u128,
    ) -> Self {
        let _ = (metrics, schedule, known);
        Self { gauges: None }
    }

    /// After block `header` applied (and at boot with the last applied
    /// header): the bits it carries (execution runs by them, not by this
    /// node's schedule), and the next activation of this node's schedule
    /// above its height.
    pub(crate) fn after_block(&self, header: &TorusBlockHeader) {
        let _ = header;
    }
}

/// A header carries a feature outside `known` (`UPGRADE REQUIRED` at `site`:
/// `vote`, `insert`, `propose` or `execute`): latch `torus_upgrade_required`,
/// keep the lowest height met with its lowest unknown feature id (the
/// activation height the operator looks for; later blocks above it do not move
/// it), count the refusal. Nothing when every bit is known.
pub(crate) fn report_upgrade_required(
    metrics: Option<&torus_telemetry::Metrics>,
    site: &'static str,
    features: u128,
    known: u128,
    height: u64,
) {
    let _ = (metrics, site, features, known, height);
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSH: Feature = Feature::RunningStateHash;

    fn value(m: &torus_telemetry::Metrics, series: &str) -> Option<String> {
        m.encode().lines().find_map(|l| {
            l.strip_prefix(series)
                .and_then(|rest| rest.strip_prefix(' '))
                .map(str::to_string)
        })
    }

    fn active_series(f: Feature) -> String {
        format!(
            "torus_feature_active{{feature=\"{}\",id=\"{}\"}}",
            f.name(),
            f.id()
        )
    }

    fn height_series(f: Feature) -> String {
        format!(
            "torus_feature_activation_height{{feature=\"{}\",id=\"{}\"}}",
            f.name(),
            f.id()
        )
    }

    fn header(height: u64, features: u128) -> TorusBlockHeader {
        let mut h = torus_bridge::genesis_parent_header(torus_types::DEFAULT_EVM_GAS_LIMIT, 1);
        h.height = height;
        h.features = features;
        h
    }

    fn schedule_at(height: u64) -> ActivationSchedule {
        ActivationSchedule::merge(&[(RSH, height)].into_iter().collect(), &[]).unwrap()
    }

    /// The gauges follow the applied header's bits and the node's schedule.
    /// RED before phase 6: nothing reported.
    #[test]
    fn gauges_report_the_schedule_and_the_applied_header_bits() {
        let m = torus_telemetry::Metrics::new();
        let monitor = FeatureMonitor::new(Some(&m), &schedule_at(10), torus_types::KNOWN_FEATURES);
        assert_eq!(value(&m, &height_series(RSH)).as_deref(), Some("10"));
        assert_eq!(value(&m, &active_series(RSH)).as_deref(), Some("0"));

        monitor.after_block(&header(0, 0));
        assert_eq!(
            value(&m, "torus_feature_next_activation_height").as_deref(),
            Some("10")
        );
        assert_eq!(
            value(&m, "torus_feature_blocks_until_activation").as_deref(),
            Some("10")
        );

        monitor.after_block(&header(4, 0));
        assert_eq!(value(&m, &active_series(RSH)).as_deref(), Some("0"));
        assert_eq!(
            value(&m, "torus_feature_next_activation_height").as_deref(),
            Some("10")
        );
        assert_eq!(
            value(&m, "torus_feature_blocks_until_activation").as_deref(),
            Some("6")
        );

        monitor.after_block(&header(9, 0));
        assert_eq!(
            value(&m, "torus_feature_blocks_until_activation").as_deref(),
            Some("1")
        );

        monitor.after_block(&header(10, RSH.bit()));
        assert_eq!(value(&m, &active_series(RSH)).as_deref(), Some("1"));
        assert_eq!(
            value(&m, "torus_feature_next_activation_height").as_deref(),
            Some("0")
        );
        assert_eq!(
            value(&m, "torus_feature_blocks_until_activation").as_deref(),
            Some("-1")
        );

        // Execution runs by the header: bits that differ from the schedule
        // (another binary moved the height) are what the gauge shows.
        monitor.after_block(&header(11, 0));
        assert_eq!(value(&m, &active_series(RSH)).as_deref(), Some("0"));
    }

    /// Nothing scheduled: no height series, no next activation.
    #[test]
    fn an_empty_schedule_reports_known_features_inactive_and_no_next_activation() {
        let m = torus_telemetry::Metrics::new();
        let monitor = FeatureMonitor::new(
            Some(&m),
            &ActivationSchedule::default(),
            torus_types::KNOWN_FEATURES,
        );
        monitor.after_block(&header(7, 0));
        assert_eq!(value(&m, &height_series(RSH)), None);
        assert_eq!(value(&m, &active_series(RSH)).as_deref(), Some("0"));
        assert_eq!(
            value(&m, "torus_feature_next_activation_height").as_deref(),
            Some("0")
        );
        assert_eq!(
            value(&m, "torus_feature_blocks_until_activation").as_deref(),
            Some("-1")
        );
        // Without metrics nothing is reported (and nothing panics).
        FeatureMonitor::new(None, &schedule_at(3), 1).after_block(&header(1, 1));
        FeatureMonitor::default().after_block(&header(1, 1));
    }

    /// `UPGRADE REQUIRED` latches the gauge with the lowest unknown id and the
    /// height, and counts the refusal by site; known bits report nothing.
    /// RED before phase 6: nothing reported.
    #[test]
    fn upgrade_required_latches_the_gauge_and_counts_the_site() {
        let m = torus_telemetry::Metrics::new();
        report_upgrade_required(Some(&m), "vote", 1, 1, 5);
        assert_eq!(
            value(&m, "torus_upgrade_required").as_deref(),
            Some("0"),
            "known bits"
        );

        report_upgrade_required(Some(&m), "vote", 1 | (1 << 9) | (1 << 30), 1, 12);
        report_upgrade_required(Some(&m), "insert", 1 << 9, 1, 12);
        assert_eq!(value(&m, "torus_upgrade_required").as_deref(), Some("1"));
        assert_eq!(
            value(&m, "torus_upgrade_required_feature").as_deref(),
            Some("9")
        );
        assert_eq!(
            value(&m, "torus_upgrade_required_height").as_deref(),
            Some("12")
        );
        assert_eq!(
            value(&m, "torus_upgrade_required_refusals_total{site=\"vote\"}").as_deref(),
            Some("1")
        );
        assert_eq!(
            value(&m, "torus_upgrade_required_refusals_total{site=\"insert\"}").as_deref(),
            Some("1")
        );
        // The lowest height met is kept (the activation height an operator
        // looks for), with its feature.
        report_upgrade_required(Some(&m), "vote", 1 << 40, 1, 15);
        assert_eq!(
            value(&m, "torus_upgrade_required_height").as_deref(),
            Some("12")
        );
        assert_eq!(
            value(&m, "torus_upgrade_required_feature").as_deref(),
            Some("9")
        );
        report_upgrade_required(Some(&m), "execute", 1 << 33, 1, 11);
        assert_eq!(
            value(&m, "torus_upgrade_required_height").as_deref(),
            Some("11")
        );
        assert_eq!(
            value(&m, "torus_upgrade_required_feature").as_deref(),
            Some("33")
        );
        assert_eq!(
            value(&m, "torus_upgrade_required_refusals_total{site=\"vote\"}").as_deref(),
            Some("2")
        );
        report_upgrade_required(None, "execute", 1 << 9, 1, 12);
    }
}
