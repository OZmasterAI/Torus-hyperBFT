/*
    Task A (adaptive pacemaker backoff, Option C) — bounded-progress liveness
    model. Companion to the T1.2 safety model in `stateright_2chain_safety.rs`.

    WHAT IT ENCODES

    The post-GST lock-step-timeout failure mode that motivates Task A: after a
    partition heals, message delay Δ exceeds the flat `max_view_time`, and all
    honest replicas — running the identical epoch-grid schedule — re-time-out
    in unison, so the window in which they overlap inside a view never grows
    past Δ and no QC can ever form (livelock).

    Because Option C's multiplier is a *pure function* of
    `(view, highest_qc_view, factor, cap)` (pinned against the production
    implementation by `backoff_multiplier_table` in
    `pacemaker/implementation.rs`), a lock-step fleet stays lock-step: every
    replica derives the identical schedule from the identical consensus state.
    That is exactly what licenses modelling the whole fleet as ONE state
    (view, qc frontier) instead of N per-replica clocks — divergence is
    impossible by construction, which is the reason Option C was chosen over
    the local-counter Options A/B.

    Progress rule (the Lewis-Pye / HotStuff view-synchronization argument):
    a QC forms in a view iff the honest replicas overlap in it for at least Δ.
    In lock-step that overlap is the full view duration
    `base * factor^min(stall_depth, cap)`, with
    `stall_depth = view − (highest_qc_view + 1)` — depth 0 is a healthy entry,
    mirroring `PacemakerConfiguration::stall_multiplier`.

    ASSERTIONS
      * factor=1 (backoff off / flat schedule): the checker finds a terminal
        no-progress path — the livelock witness. This is the Option D
        validation folded into Task A: the gap is real under flat timeouts.
      * factor=2: `eventually commit` holds within the exploration bound.
      * Convergence is bounded: commit within ceil(log2(Δ/base)) + 1 views.
*/

use stateright::{Checker, Model, Property};

/// Base view time in abstract ticks (= `max_view_time`).
const BASE: u64 = 1;
/// Post-partition-heal message delay Δ, in the same ticks. Δ > BASE is the
/// premise of the failure mode (flat views are too short for a QC round-trip).
const DELTA: u64 = 12;
/// Fleet-wide backoff exponent cap (mirrors the production default).
const CAP: u32 = 8;
/// Exploration bound: enough views for flat schedules to prove they never
/// converge and for backed-off schedules to converge with room to spare.
const MAX_VIEW: u64 = 40;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Fleet {
    /// The view every honest replica is in (lock-step — see header).
    view: u64,
    /// The QC frontier every honest replica sees. Frozen during a stall:
    /// exactly the property that makes the Option C multiplier lockstep-exact.
    highest_qc_view: u64,
    /// Whether a QC has formed (progress; a commit follows by 2-chain).
    committed: bool,
}

/// The fleet's per-view duration under the Option C schedule. Mirrors
/// `stall_multiplier`: depth = view − (highest_qc_view + 1), saturating,
/// exponent capped, multiplier floored at 1.
fn view_duration(factor: u64, view: u64, highest_qc_view: u64) -> u64 {
    let depth = view.saturating_sub(highest_qc_view + 1).min(CAP as u64) as u32;
    BASE * factor.saturating_pow(depth).max(1)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Step {
    /// The current view ends: with a QC if the lock-step overlap covered Δ,
    /// with a unison timeout otherwise.
    ViewEnds,
}

struct LockstepBackoff {
    /// Fleet-wide backoff factor. 1 models today's flat schedule (backoff
    /// off — also the `backoff_cap = 0` kill switch); 2 the Task A default.
    factor: u64,
}

impl Model for LockstepBackoff {
    type State = Fleet;
    type Action = Step;

    fn init_states(&self) -> Vec<Self::State> {
        // Partition just healed: the fleet enters the first view past the
        // frontier in lock-step, and message delay is now Δ.
        vec![Fleet {
            view: 1,
            highest_qc_view: 0,
            committed: false,
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        if !state.committed && state.view <= MAX_VIEW {
            actions.push(Step::ViewEnds);
        }
    }

    fn next_state(&self, state: &Self::State, _action: Self::Action) -> Option<Self::State> {
        if view_duration(self.factor, state.view, state.highest_qc_view) >= DELTA {
            // Honest replicas overlapped for >= Δ: a QC forms and the
            // frontier advances (which collapses the stall depth — the
            // self-resetting half of the liveness argument).
            Some(Fleet {
                view: state.view + 1,
                highest_qc_view: state.view,
                committed: true,
            })
        } else {
            // Unison timeout: the view is abandoned with no new QC, the
            // frontier stays frozen, the stall deepens by one.
            Some(Fleet {
                view: state.view + 1,
                highest_qc_view: state.highest_qc_view,
                committed: false,
            })
        }
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![Property::<Self>::eventually("commit", |_, state| {
            state.committed
        })]
    }
}

/// Flat schedule (factor 1, today's behavior): every view lasts BASE < Δ, the
/// fleet re-times-out in unison forever — the checker must find the
/// no-progress witness. This validates that the failure mode Task A targets
/// is real and survives the S395 rebase clamp (which never lengthens views).
#[test]
fn lockstep_backoff_flat_never_converges() {
    let checker = LockstepBackoff { factor: 1 }.checker().spawn_bfs().join();
    assert!(
        checker.discovery("commit").is_some(),
        "flat timeouts must livelock after partition heal (Δ > max_view_time)"
    );
}

/// Backed-off schedule (factor 2): view duration grows geometrically until it
/// exceeds Δ, a QC forms, and `eventually commit` holds.
#[test]
fn lockstep_backoff_geometric_converges() {
    let checker = LockstepBackoff { factor: 2 }.checker().spawn_bfs().join();
    checker.assert_properties();
}

/// The convergence bound from the design's liveness argument: with factor 2
/// the fleet commits within ceil(log2(Δ/BASE)) + 1 = 5 views of the heal.
#[test]
fn lockstep_backoff_convergence_is_bounded() {
    let model = LockstepBackoff { factor: 2 };
    let mut fleet = model.init_states().remove(0);
    let mut views_used = 0u64;
    while !fleet.committed {
        fleet = model.next_state(&fleet, Step::ViewEnds).unwrap();
        views_used += 1;
        assert!(
            views_used <= 5,
            "backoff must converge within O(log2(Δ / base)) views"
        );
    }
}

// ===========================================================================
// S470 — commit-lag branch (the header-pipeline COMMIT WEDGE)
// ===========================================================================
//
// WHAT IT ENCODES
//
// The S470 wedge is the Task A blind spot: body execution E exceeds the view
// time, so no leader ever proposes within the view its justify QC entitles it
// to — QCs keep forming (header votes need no execution, so the frontier
// CRAWLS, advancing every view in this worst-case model), but every QC pair
// is non-consecutive and the 2-chain commit rule never fires. Because a QC
// forms every view, the stall depth `view − (highest_qc_view + 1)` is pinned
// at 0 — the pre-S470 multiplier is constantly 1 and the flat schedule churns
// forever: commit livelock with a live-looking QC frontier.
//
// The S470 term is keyed on `highest_qc_view − committed_view`, which grows
// by one with every committed-less view: the deadline stretches geometrically
// until a view is long enough (>= E) for the leader to execute the tip AND
// propose inside the justified view — consecutive QCs form and the commit
// frontier advances.
//
// The multiplier mirrors production `PacemakerConfiguration::stall_multiplier`
// (pinned there by `commit_lag_exponent_table` and
// `commit_lag_cap_zero_is_exact_today`): exponent = max(stall_exp,
// min((qc − committed).saturating_sub(GRACE), lag_cap)), GRACE = 4
// (production `COMMIT_LAG_GRACE`).
//
// LOCKSTEP ARGUMENT (why one fleet state suffices, again): the new term is a
// pure function of (view, highest_qc_view, committed_view, fleet config) —
// all consensus-visible. Local execution progress, which genuinely differs
// across replicas, is NOT an input; `wedge_duration` below cannot even
// observe the `local_exec_progress` field carried in the two-replica purity
// test at the bottom. Identical consensus state → identical deadlines →
// no replica times out early → no premature Bracha amplification.

/// Production `COMMIT_LAG_GRACE` mirror (pinned by `commit_lag_exponent_table`
/// in `pacemaker/implementation.rs`).
const S470_GRACE: u64 = 4;
/// Body-execution time in abstract ticks — the wedge premise is E > BASE.
const S470_EXEC: u64 = 12;
/// Commit-lag exponent cap under test (the recommended devnet setting).
const S470_LAG_CAP: u32 = 8;
/// Exploration bound for the wedge model.
const S470_MAX_VIEW: u64 = 60;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WedgeFleet {
    /// The view every honest replica is in (lock-step — see header).
    view: u64,
    /// The QC frontier: advances EVERY view (header-speed votes) — the wedge's
    /// signature, and the reason the stall term stays 0.
    highest_qc_view: u64,
    /// The commit frontier, in view units (justify-view of the highest
    /// committed block). Frozen until a consecutive-view QC pair forms.
    committed_view: u64,
    /// Whether a 2-chain commit has happened (progress).
    committed: bool,
}

/// The fleet's per-view duration under the S470 schedule: mirrors
/// `stall_multiplier` with both terms. In this model the stall exponent is
/// always 0 (a QC forms every view), which is exactly the blind spot: with
/// `lag_cap = 0` this collapses to the flat pre-S470 schedule.
fn wedge_duration(
    factor: u64,
    view: u64,
    highest_qc_view: u64,
    committed_view: u64,
    lag_cap: u32,
) -> u64 {
    let stall_exp = view.saturating_sub(highest_qc_view + 1).min(CAP as u64) as u32;
    let lag_exp = highest_qc_view
        .saturating_sub(committed_view)
        .saturating_sub(S470_GRACE)
        .min(lag_cap as u64) as u32;
    BASE * factor.saturating_pow(stall_exp.max(lag_exp)).max(1)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum WedgeStep {
    ViewEnds,
}

struct CommitLagWedge {
    /// S470 commit-lag exponent cap: 0 models today (knob off / kill switch),
    /// S470_LAG_CAP the fix.
    lag_cap: u32,
}

impl Model for CommitLagWedge {
    type State = WedgeFleet;
    type Action = WedgeStep;

    fn init_states(&self) -> Vec<Self::State> {
        // Wedge onset: load arrived, bodies now take E > BASE to execute; the
        // chain enters view 1 with both frontiers caught up.
        vec![WedgeFleet {
            view: 1,
            highest_qc_view: 0,
            committed_view: 0,
            committed: false,
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        if !state.committed && state.view <= S470_MAX_VIEW {
            actions.push(WedgeStep::ViewEnds);
        }
    }

    fn next_state(&self, state: &Self::State, _action: Self::Action) -> Option<Self::State> {
        let duration = wedge_duration(
            2,
            state.view,
            state.highest_qc_view,
            state.committed_view,
            self.lag_cap,
        );
        if duration >= S470_EXEC {
            // The view was long enough for the leader to execute the certified
            // tip and propose INSIDE the justified view: the new QC is
            // consecutive with its parent's justify — 2-chain commit fires.
            Some(WedgeFleet {
                view: state.view + 1,
                highest_qc_view: state.view,
                committed_view: state.highest_qc_view,
                committed: true,
            })
        } else {
            // Too short: the proposal lands views late. Header votes still
            // certify it — the QC frontier advances (keeping the stall term
            // at 0) — but the QC pair is non-consecutive: no commit.
            Some(WedgeFleet {
                view: state.view + 1,
                highest_qc_view: state.view,
                committed_view: state.committed_view,
                committed: false,
            })
        }
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![Property::<Self>::eventually("commit", |_, state| {
            state.committed
        })]
    }
}

/// Knob off (`commit_lag_cap = 0`, today's shipped schedule): the QC frontier
/// advances every view so the stall term never engages, views stay flat BASE
/// < E, and the checker must find the no-commit witness — the commit wedge is
/// a genuine livelock under the pre-S470 schedule.
#[test]
fn commit_lag_wedge_flat_never_commits() {
    let checker = CommitLagWedge { lag_cap: 0 }.checker().spawn_bfs().join();
    assert!(
        checker.discovery("commit").is_some(),
        "the commit wedge must livelock under the knob-off schedule (E > max_view_time, \
         QCs crawling): if this fails the model no longer captures the Task A blind spot"
    );
}

/// Knob on: the commit-lag term grows with every committed-less view until the
/// deadline covers E — `eventually commit` holds within the exploration bound.
#[test]
fn commit_lag_wedge_backoff_commits() {
    let checker = CommitLagWedge {
        lag_cap: S470_LAG_CAP,
    }
    .checker()
    .spawn_bfs()
    .join();
    checker.assert_properties();
}

/// Convergence is bounded: with factor 2 the wedge resolves within
/// GRACE + ceil(log2(E / BASE)) + 1 views of onset.
#[test]
fn commit_lag_wedge_convergence_is_bounded() {
    let model = CommitLagWedge {
        lag_cap: S470_LAG_CAP,
    };
    let mut fleet = model.init_states().remove(0);
    let mut views_used = 0u64;
    let bound = S470_GRACE + (S470_EXEC as f64 / BASE as f64).log2().ceil() as u64 + 1;
    while !fleet.committed {
        fleet = model.next_state(&fleet, WedgeStep::ViewEnds).unwrap();
        views_used += 1;
        assert!(
            views_used <= bound,
            "commit-lag backoff must converge within GRACE + O(log2(E / base)) views \
             (bound {bound}, used {views_used})"
        );
    }
}

/// LOCKSTEP-SAFETY: honest replicas derive IDENTICAL deadlines from the new
/// term. Two replicas run the whole wedge trace with WILDLY different local
/// execution progress (one executes instantly, one is far behind);
/// `wedge_duration` receives only the shared consensus state — the local
/// field is not an input, so the schedules are equal at every step BY
/// CONSTRUCTION, and no replica can time out ahead of the fleet (no premature
/// Bracha amplification). This is the property that forbids ever feeding
/// exec-queue depth or other node-local state into the deadline formula.
#[test]
fn commit_lag_deadlines_are_lockstep_identical() {
    #[derive(Clone)]
    struct Replica {
        /// Node-local, consensus-INVISIBLE state: differs across replicas.
        /// Deliberately unused by the deadline computation.
        #[allow(dead_code)]
        local_exec_progress: u64,
    }

    impl Replica {
        /// A replica's deadline multiplier: forwards ONLY consensus-visible
        /// inputs. (If someone later threads local state in here, this test's
        /// premise — and the fleet's lockstep — is broken.)
        fn deadline(&self, view: u64, qc: u64, committed: u64, lag_cap: u32) -> u64 {
            wedge_duration(2, view, qc, committed, lag_cap)
        }
    }

    let fast = Replica {
        local_exec_progress: u64::MAX,
    };
    let slow = Replica {
        local_exec_progress: 0,
    };

    let model = CommitLagWedge {
        lag_cap: S470_LAG_CAP,
    };
    let mut fleet = model.init_states().remove(0);
    for _ in 0..S470_MAX_VIEW {
        for lag_cap in [0, 1, 4, S470_LAG_CAP] {
            assert_eq!(
                fast.deadline(
                    fleet.view,
                    fleet.highest_qc_view,
                    fleet.committed_view,
                    lag_cap
                ),
                slow.deadline(
                    fleet.view,
                    fleet.highest_qc_view,
                    fleet.committed_view,
                    lag_cap
                ),
                "replicas with divergent local exec progress derived divergent deadlines \
                 at view {} — the commit-lag term must be a pure function of consensus state",
                fleet.view,
            );
        }
        fleet = model.next_state(&fleet, WedgeStep::ViewEnds).unwrap();
        if fleet.committed {
            // Restart the wedge to keep exercising fresh states.
            fleet.committed = false;
        }
    }
}
