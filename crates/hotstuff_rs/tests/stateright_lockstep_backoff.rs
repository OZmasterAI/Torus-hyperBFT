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
