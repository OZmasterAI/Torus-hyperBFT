//! Epoch plan hand-off (`TORUS_EPOCH_PLAN_HANDOFF`, node-local, default off;
//! the s80 prototype `epoch-plan-logical-ready`, 62373d5d): execution's
//! LOGICAL applied height and the boundary plan rows it wrote, handed to
//! consensus in memory.
//!
//! Consensus bug (b) makes consensus read the plan rows for boundary `H`
//! (written by the execution of `H - L`) only once the DURABLE applied marker
//! reaches `H - L`. With the exec pipeline and checkpoints, the previous
//! boundary's checkpoint can still be on the flush worker when consensus
//! reaches `H`: every refusal in the 9j2 deep cells (212 of 212) had
//! execution logically past `H - L` and only the durable marker behind, and
//! each refusal cost a view timeout. With the flag, execution publishes the
//! rows it wrote (the same bytes the flush or checkpoint writes), then its
//! logical height, and consensus answers from them.
//!
//! The rows are the member plan (`epoch_vset:plan:<H>`) and the dead-leader
//! exclusion plan (`epoch_vset:lx_plan:<H>`, absent = no exclusion delta):
//! consensus builds boundary `H`'s validator-set update from both, so both
//! come from one source (memory, or the DB once durable), never a mix.
//!
//! Safety rule (bug-b2 class): a missing plan is never read as "no change".
//! The DB is read only once the durable marker reaches `H - L` (today's
//! rule); a logical-only readiness without the rows in memory is NOT ready.
//! After a restart the map is empty and the logical height starts at the
//! durable marker: exactly the durable-only rule.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use torus_state::cf::CF_CONSENSUS_META;
use torus_state::{StateBackend, StateError};

/// The environment variable (read once at construction).
pub const PLAN_HANDOFF_ENV: &str = "TORUS_EPOCH_PLAN_HANDOFF";

/// `TORUS_EPOCH_PLAN_HANDOFF` (read once at construction).
pub fn plan_handoff_enabled() -> bool {
    parse_plan_handoff(std::env::var(PLAN_HANDOFF_ENV).ok())
}

/// Pure parse: `1` / `true` = on; unset, empty, `0` or `false` = off;
/// anything else = off with a warning (a typo never enables a
/// consensus-timing change).
pub fn parse_plan_handoff(raw: Option<String>) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => true,
        "" | "0" | "false" => false,
        other => {
            tracing::warn!(
                value = other,
                "TORUS_EPOCH_PLAN_HANDOFF: not 0/1/true/false — epoch plan hand-off OFF"
            );
            false
        }
    }
}

/// The rows the execution of boundary `boundary - L` wrote for `boundary`,
/// as raw bytes (exactly the rows its flush or checkpoint writes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryRows {
    /// `epoch_vset:plan:<boundary>` (an entry exists only with it).
    pub plan: Vec<u8>,
    /// `epoch_vset:lx_plan:<boundary>`; `None`: the row is absent (no
    /// exclusion delta at `boundary`).
    pub lx_plan: Option<Vec<u8>>,
}

/// Read boundary `boundary`'s rows from `state` (the executing boundary's
/// own overlay on E). `Ok(None)`: no member plan row, nothing to publish.
pub fn read_boundary_rows<T: StateBackend>(
    state: &T,
    boundary: u64,
) -> Result<Option<BoundaryRows>, StateError> {
    let Some(plan) = state.get_cf_raw(
        CF_CONSENSUS_META,
        &torus_economics::epoch_plan_key(boundary),
    )?
    else {
        return Ok(None);
    };
    let lx_plan = state.get_cf_raw(
        CF_CONSENSUS_META,
        &torus_economics::leader_stats::leader_exclusion_plan_key(boundary),
    )?;
    Ok(Some(BoundaryRows { plan, lx_plan }))
}

/// Where consensus reads the rows for a boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanSource {
    /// The rows execution published (identical to the rows it wrote).
    Memory(Arc<BoundaryRows>),
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
    /// The highest height execution handed to the flush worker (seeded with
    /// the durable marker when the worker attaches).
    logical: AtomicU64,
    /// Rows by boundary, written by the execution of `boundary - L`.
    plans: Mutex<BTreeMap<u64, Arc<BoundaryRows>>>,
}

impl PlanHandoff {
    /// Execution of boundary `executed` wrote `rows` for `boundary`
    /// (= `executed + L`). Called BEFORE the logical height of `executed` is
    /// published, after the block's fatal-error checks. As the DB rows of
    /// `executed - L` are deleted by this execution, entries at or below
    /// `executed - L` are dropped; the entry for `executed` itself is kept
    /// (consensus may re-read it until `executed + L` executes).
    pub fn publish_plan(
        &self,
        executed: u64,
        epoch_length: u64,
        boundary: u64,
        rows: BoundaryRows,
    ) {
        let floor = executed.saturating_sub(epoch_length);
        let mut plans = self.plans.lock().unwrap();
        plans.retain(|&b, _| b > floor);
        plans.insert(boundary, Arc::new(rows));
    }

    /// Undo [`Self::publish_plan`] when the block's hand-off failed (the
    /// node is fail-stopping; the entry must not outlive its block).
    pub fn retract_plan(&self, boundary: u64) {
        self.plans.lock().unwrap().remove(&boundary);
    }

    /// Execution handed `height` to the flush worker (after any rows it
    /// wrote were published).
    pub fn set_logical(&self, height: u64) {
        self.logical.store(height, Ordering::SeqCst);
    }

    pub fn logical(&self) -> u64 {
        self.logical.load(Ordering::SeqCst)
    }

    /// The published rows for `boundary`, if any.
    pub fn plan(&self, boundary: u64) -> Option<Arc<BoundaryRows>> {
        self.plans.lock().unwrap().get(&boundary).cloned()
    }

    /// The boundaries currently held (tests, logs).
    pub fn boundaries(&self) -> Vec<u64> {
        self.plans.lock().unwrap().keys().copied().collect()
    }

    /// Where to read the rows for `boundary`, which needs execution at
    /// `needed` (= `boundary - L`), given the durable marker `durable`.
    /// Ready iff `max(durable, logical) >= needed`; then the map first, the
    /// DB only if `durable >= needed`, otherwise NOT ready (never "no
    /// change").
    pub fn source(&self, boundary: u64, needed: u64, durable: u64) -> PlanSource {
        // Logical first: rows are published before their height (SeqCst
        // store after the insert), so a logical height at or past `needed`
        // sees its entry below.
        let logical = self.logical();
        if durable.max(logical) < needed {
            return PlanSource::NotReady { logical };
        }
        if let Some(rows) = self.plan(boundary) {
            return PlanSource::Memory(rows);
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

    fn rows(p: u8) -> BoundaryRows {
        BoundaryRows {
            plan: vec![p],
            lx_plan: None,
        }
    }

    #[test]
    fn parse_default_off() {
        assert!(!parse_plan_handoff(None), "unset: off");
        assert!(!parse_plan_handoff(Some(String::new())), "empty: off");
        assert!(!parse_plan_handoff(Some("0".into())), "0: off");
        assert!(!parse_plan_handoff(Some("false".into())), "false: off");
        assert!(!parse_plan_handoff(Some("yes".into())), "typo: off");
        assert!(parse_plan_handoff(Some("1".into())), "1: on");
        assert!(parse_plan_handoff(Some(" TRUE ".into())), "true: on");
    }

    /// (a) durable lags, execution logically applied the previous boundary
    /// and published its rows: Memory, with the published rows.
    #[test]
    fn durable_lags_map_hit_is_memory() {
        let h = PlanHandoff::default();
        let r = BoundaryRows {
            plan: vec![1, 2, 3],
            lx_plan: Some(vec![7]),
        };
        h.publish_plan(1200, 100, 1300, r.clone());
        h.set_logical(1242);
        assert_eq!(h.source(1300, 1200, 1100), PlanSource::Memory(Arc::new(r)));
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
        h.publish_plan(1200, 100, 1300, rows(9));
        h.set_logical(1199);
        assert_eq!(
            h.source(1300, 1200, 1100),
            PlanSource::NotReady { logical: 1199 },
            "the block that published it is not handed off yet"
        );
    }

    /// The bug-b2 guard: logically ready but no entry (a serial boundary
    /// before its flush, a restart) and the durable marker behind is NOT
    /// ready — never Db (whose missing row would read as "no change").
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

    /// Entries live as their DB rows: the execution of `X` drops the rows
    /// for `X - L` and keeps the rows for `X`.
    #[test]
    fn publish_prunes_like_the_db() {
        let h = PlanHandoff::default();
        h.publish_plan(100, 100, 200, rows(2));
        h.publish_plan(200, 100, 300, rows(3));
        assert_eq!(h.boundaries(), vec![200, 300]);
        h.publish_plan(300, 100, 400, rows(4));
        assert_eq!(
            h.boundaries(),
            vec![300, 400],
            "plan(200) dropped at exec of 300"
        );
        h.retract_plan(400);
        assert_eq!(h.boundaries(), vec![300]);
    }

    /// The rows read back from a state are byte for byte its two rows; no
    /// member plan = nothing to publish (the exclusion row alone never
    /// makes an entry).
    #[test]
    fn read_boundary_rows_reads_both_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = torus_state::StateDb::open(dir.path()).unwrap();
        let lx_key = torus_economics::leader_stats::leader_exclusion_plan_key(8);
        db.put_cf_raw(CF_CONSENSUS_META, &lx_key, &[5, 6]).unwrap();
        assert_eq!(read_boundary_rows(&db, 8).unwrap(), None, "no member plan");
        db.put_cf_raw(
            CF_CONSENSUS_META,
            &torus_economics::epoch_plan_key(8),
            &[1, 2],
        )
        .unwrap();
        assert_eq!(
            read_boundary_rows(&db, 8).unwrap(),
            Some(BoundaryRows {
                plan: vec![1, 2],
                lx_plan: Some(vec![5, 6]),
            })
        );
        db.delete_cf_raw(CF_CONSENSUS_META, &lx_key).unwrap();
        assert_eq!(
            read_boundary_rows(&db, 8).unwrap(),
            Some(BoundaryRows {
                plan: vec![1, 2],
                lx_plan: None,
            })
        );
    }
}
