//! s80 epoch-plan-logical-ready (`TORUS_S80_PLAN_LOGICAL`, node-local,
//! default off): execution's LOGICAL applied height and the epoch plans it
//! wrote, handed to consensus in memory.
//!
//! Consensus bug (b) makes consensus read the plan for boundary `H` (written
//! by the execution of `H - L`) only once the DURABLE applied marker reaches
//! `H - L`. With the exec pipeline and checkpoints, the boundary's own
//! checkpoint can still be on the flush worker when consensus reaches `H`:
//! every refusal in the 9j2 deep cells had execution logically past `H - L`
//! and only the durable marker behind. With the flag, execution publishes
//! the plan bytes it wrote (exactly the row the checkpoint will write) and
//! then its logical height, and consensus answers from them.
//!
//! Safety rule (bug-b2 class): a missing plan is never read as "no change".
//! The DB is read only once the durable marker reaches `H - L` (today's
//! rule); a logical-only readiness without the plan in memory is NOT ready.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// `TORUS_S80_PLAN_LOGICAL` (read once at construction).
pub fn plan_logical_enabled() -> bool {
    parse_plan_logical(std::env::var("TORUS_S80_PLAN_LOGICAL").ok())
}

/// Pure parse: `1` / `true` = on; unset or anything else = off.
pub fn parse_plan_logical(raw: Option<String>) -> bool {
    matches!(
        raw.as_deref()
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true")
    )
}

/// Where consensus reads the plan for a boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanSource {
    /// The bytes execution published (identical to the row it wrote).
    Memory(Vec<u8>),
    /// The durable marker is at or past `H - L`: today's DB read.
    Db,
    /// Not readable yet: refuse and retry, never guess.
    NotReady {
        /// Execution's logical height, for the refusal log.
        logical: u64,
    },
}

/// Shared by the execution context (writer) and the app (reader).
#[derive(Debug, Default)]
pub struct PlanHandoff {
    /// The highest height execution handed to the flush worker (or 0).
    logical: AtomicU64,
    /// Plan bytes by boundary, written by the execution of `boundary - L`.
    plans: Mutex<BTreeMap<u64, Vec<u8>>>,
}

impl PlanHandoff {
    /// Execution of boundary `executed` wrote `bytes` as the plan for
    /// `boundary` (= `executed + L`). Called BEFORE the logical height of
    /// `executed` is published, after the block's fatal-error checks. As the
    /// DB row of `executed - L` is deleted by this execution, entries at or
    /// below `executed - L` are dropped; the plan for `executed` itself is
    /// kept (consensus may re-read it until `executed + L` executes).
    pub fn publish_plan(&self, executed: u64, epoch_length: u64, boundary: u64, bytes: Vec<u8>) {
        let floor = executed.saturating_sub(epoch_length);
        let mut plans = self.plans.lock().unwrap();
        plans.retain(|&b, _| b > floor);
        plans.insert(boundary, bytes);
    }

    /// Undo [`Self::publish_plan`] when the block's hand-off failed (the
    /// node is fail-stopping; the entry must not outlive its block).
    pub fn retract_plan(&self, boundary: u64) {
        self.plans.lock().unwrap().remove(&boundary);
    }

    /// Execution handed `height` to the flush worker (after any plan it
    /// wrote was published).
    pub fn set_logical(&self, height: u64) {
        self.logical.store(height, Ordering::SeqCst);
    }

    pub fn logical(&self) -> u64 {
        self.logical.load(Ordering::SeqCst)
    }

    /// The published plan for `boundary`, if any.
    pub fn plan(&self, boundary: u64) -> Option<Vec<u8>> {
        self.plans.lock().unwrap().get(&boundary).cloned()
    }

    /// The boundaries currently held (tests, logs).
    pub fn boundaries(&self) -> Vec<u64> {
        self.plans.lock().unwrap().keys().copied().collect()
    }

    /// Where to read the plan for `boundary`, which needs execution at
    /// `needed` (= `boundary - L`), given the durable marker `durable`.
    /// Ready iff `max(durable, logical) >= needed`; then the map first, the
    /// DB only if `durable >= needed`, otherwise NOT ready (never "no
    /// change").
    pub fn source(&self, boundary: u64, needed: u64, durable: u64) -> PlanSource {
        // Logical first: a plan is published before its height, so a
        // logical height at or past `needed` sees its entry below.
        let logical = self.logical();
        if durable.max(logical) < needed {
            return PlanSource::NotReady { logical };
        }
        if let Some(bytes) = self.plan(boundary) {
            return PlanSource::Memory(bytes);
        }
        if durable >= needed {
            return PlanSource::Db;
        }
        PlanSource::NotReady { logical }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_default_off() {
        assert!(!parse_plan_logical(None), "unset: off");
        assert!(!parse_plan_logical(Some(String::new())), "empty: off");
        assert!(!parse_plan_logical(Some("0".into())), "0: off");
        assert!(!parse_plan_logical(Some("yes".into())), "typo: off");
        assert!(parse_plan_logical(Some("1".into())), "1: on");
        assert!(parse_plan_logical(Some(" TRUE ".into())), "true: on");
    }

    /// (a) durable lags, execution logically applied the previous boundary
    /// and published its plan: Memory, with the published bytes.
    #[test]
    fn durable_lags_map_hit_is_memory() {
        let h = PlanHandoff::default();
        h.publish_plan(1200, 100, 1300, vec![1, 2, 3]);
        h.set_logical(1242);
        assert_eq!(
            h.source(1300, 1200, 1100),
            PlanSource::Memory(vec![1, 2, 3])
        );
    }

    /// (b) neither the durable marker nor the logical height reached
    /// `needed`: not ready, even with a (pre-published) entry.
    #[test]
    fn neither_ready_is_not_ready() {
        let h = PlanHandoff::default();
        assert_eq!(
            h.source(1300, 1200, 1100),
            PlanSource::NotReady { logical: 0 }
        );
        h.publish_plan(1200, 100, 1300, vec![9]);
        h.set_logical(1199);
        assert_eq!(
            h.source(1300, 1200, 1100),
            PlanSource::NotReady { logical: 1199 },
            "the block that published it is not handed off yet"
        );
    }

    /// The bug-b2 guard: logically ready but no entry (a serial boundary,
    /// a restart) and the durable marker behind is NOT ready — never Db
    /// (whose missing row would read as "no change").
    #[test]
    fn logical_ready_map_miss_durable_behind_is_not_ready() {
        let h = PlanHandoff::default();
        h.set_logical(1250);
        assert_eq!(
            h.source(1300, 1200, 1100),
            PlanSource::NotReady { logical: 1250 }
        );
    }

    /// (c) a restart: empty map, logical seeded at the durable marker: the
    /// DB path exactly as without the flag.
    #[test]
    fn restart_empty_map_reads_db() {
        let h = PlanHandoff::default();
        h.set_logical(1200);
        assert_eq!(h.source(1300, 1200, 1200), PlanSource::Db);
        assert_eq!(h.source(1300, 1200, 1250), PlanSource::Db);
        assert_eq!(
            h.source(1300, 1200, 1199),
            PlanSource::NotReady { logical: 1200 }
        );
    }

    /// Entries live as their DB rows: the execution of `X` drops the plan
    /// for `X - L` and keeps the plan for `X`.
    #[test]
    fn publish_prunes_like_the_db() {
        let h = PlanHandoff::default();
        h.publish_plan(100, 100, 200, vec![2]);
        h.publish_plan(200, 100, 300, vec![3]);
        assert_eq!(h.boundaries(), vec![200, 300]);
        h.publish_plan(300, 100, 400, vec![4]);
        assert_eq!(
            h.boundaries(),
            vec![300, 400],
            "plan(200) dropped at exec of 300"
        );
        h.retract_plan(400);
        assert_eq!(h.boundaries(), vec![300]);
    }
}
