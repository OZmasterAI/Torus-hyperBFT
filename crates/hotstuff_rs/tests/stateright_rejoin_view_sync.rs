/*
    s74 rejoin view sync (round-skip) — per-replica vote-safety model.

    WHAT IT ENCODES

    Three replicas with their own views (not lock-step). Each keeps, as the
    production block tree does, a persisted voted view
    (`set_vote_state_atomic`, written before the vote leaves), and its view is
    persisted on entry (`enter_view` → `set_highest_view_entered`), so a
    restart re-enters at `view + 1` (`Algorithm::new`'s `init_view`). A crash
    loses everything else. Crashes are bounded (`CRASHES`) to keep the space
    finite; views are bounded by `MAX_VIEW`.

    Leaders may equivocate (two blocks per view), so "at most one vote per
    view" is a real constraint, not a tautology.

    A header delivered to a replica:
      * for its current view: vote if the voted view is below it
        (`on_receive_proposal_header` vote gate);
      * for a FUTURE view w, with round-skip on and the voted view below w:
        enter w first (pacemaker `skip_to_view` + `enter_view`), then vote
        through the same current-view gate;
      * for a past view: nothing.

    State compression: instead of full vote histories each replica keeps the
    highest view it ever voted in and a `violated` flag, set when it votes in
    a view at or below that. "Vote views strictly increase" (which implies
    "never two votes in one view") is then `!violated`, checked every step.

    ASSERTIONS
      * safety, round-skip on AND off: no replica's vote views ever fail to
        strictly increase, across crash/restart;
      * the model has teeth: dropping the voted-view guard lets the checker
        find a violation;
      * the motivating scenario: a replica restarted below the survivors
        votes in their view on the leader's header with zero timeouts only
        when round-skip is on.
*/

use stateright::{Checker, Model, Property};

const N: usize = 3;
/// Exploration bound on views.
const MAX_VIEW: u8 = 5;
/// Blocks a leader may propose in one view (2 = equivocation allowed).
const BLOCKS: u8 = 2;
/// Crash budget per run.
const CRASHES: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Replica {
    alive: bool,
    /// Current view; persisted on entry, so it survives a crash.
    view: u8,
    /// Persisted voted view (`highest_view_phase_voted`).
    voted: Option<u8>,
    /// Highest view this replica ever sent a vote in.
    max_vote: Option<u8>,
    /// It sent a vote in a view <= an earlier vote's view.
    violated: bool,
}

impl Replica {
    fn at(view: u8) -> Self {
        Replica { alive: true, view, voted: None, max_vote: None, violated: false }
    }

    fn may_vote(&self, view: u8, guard: bool) -> bool {
        !guard || self.voted.is_none_or(|voted| voted < view)
    }

    fn vote(&mut self, view: u8) {
        self.violated |= self.max_vote.is_some_and(|max| max >= view);
        self.max_vote = Some(self.max_vote.map_or(view, |max| max.max(view)));
        self.voted = Some(view);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Cluster {
    replicas: [Replica; N],
    /// Proposed headers (view, block), sorted.
    headers: Vec<(u8, u8)>,
    crashes_left: u8,
    /// Coverage: some replica has voted right after a round-skip.
    skip_voted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Step {
    Timeout(usize),
    Propose(u8, u8),
    Crash(usize),
    Restart(usize),
    Deliver(usize, u8),
}

fn leader(view: u8) -> usize {
    view as usize % N
}

struct RejoinModel {
    round_skip: bool,
    /// Production's voted-view guard; `false` is the mutation that must fail.
    guard: bool,
}

impl Model for RejoinModel {
    type State = Cluster;
    type Action = Step;

    fn init_states(&self) -> Vec<Self::State> {
        vec![Cluster {
            replicas: [Replica::at(1), Replica::at(1), Replica::at(1)],
            headers: vec![],
            crashes_left: CRASHES,
            skip_voted: false,
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        for (i, replica) in state.replicas.iter().enumerate() {
            if replica.alive {
                if replica.view < MAX_VIEW {
                    actions.push(Step::Timeout(i));
                }
                if state.crashes_left > 0 {
                    actions.push(Step::Crash(i));
                }
                // Which block does not matter to a receiver's vote-view
                // safety; any header of the view is the same action.
                let mut views: Vec<u8> = state.headers.iter().map(|h| h.0).collect();
                views.dedup();
                for view in views {
                    if leader(view) != i {
                        actions.push(Step::Deliver(i, view));
                    }
                }
            } else if replica.view < MAX_VIEW {
                actions.push(Step::Restart(i));
            }
        }
        for view in 1..=MAX_VIEW {
            let proposer = &state.replicas[leader(view)];
            if proposer.alive && proposer.view == view {
                for block in 0..BLOCKS {
                    if !state.headers.contains(&(view, block)) {
                        actions.push(Step::Propose(view, block));
                        break;
                    }
                }
            }
        }
    }

    fn next_state(&self, state: &Self::State, action: Self::Action) -> Option<Self::State> {
        let mut next = state.clone();
        match action {
            Step::Timeout(i) => next.replicas[i].view += 1,
            Step::Propose(view, block) => {
                next.headers.push((view, block));
                next.headers.sort();
                // The leader votes on its own proposal (inline header path).
                let r = &mut next.replicas[leader(view)];
                if r.may_vote(view, self.guard) {
                    r.vote(view);
                }
            }
            Step::Crash(i) => {
                next.replicas[i].alive = false;
                next.crashes_left -= 1;
            }
            Step::Restart(i) => {
                let r = &mut next.replicas[i];
                r.alive = true;
                r.view += 1;
            }
            Step::Deliver(i, view) => {
                let r = &mut next.replicas[i];
                let skipped = view > r.view && self.round_skip && r.may_vote(view, self.guard);
                if skipped {
                    r.view = view;
                }
                if view == r.view && r.may_vote(view, self.guard) {
                    r.vote(view);
                    next.skip_voted |= skipped;
                }
            }
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always("vote views strictly increase", |_, state| {
                state.replicas.iter().all(|r| !r.violated)
            }),
            Property::<Self>::sometimes("a replica votes right after a round-skip", |_, state| {
                state.skip_voted
            }),
        ]
    }
}

#[test]
fn rejoin_round_skip_preserves_vote_safety() {
    let on = RejoinModel { round_skip: true, guard: true }.checker().spawn_bfs().join();
    on.assert_properties(); // safety holds and the skip-vote path is reached
    let off = RejoinModel { round_skip: false, guard: true }.checker().spawn_bfs().join();
    off.assert_no_discovery("vote views strictly increase");
}

#[test]
fn rejoin_model_catches_a_missing_voted_view_guard() {
    let checker = RejoinModel { round_skip: true, guard: false }.checker().spawn_bfs().join();
    assert!(
        checker.discovery("vote views strictly increase").is_some(),
        "without the voted-view guard an equivocating leader must get a second vote"
    );
}

/// The s64 crash scenario: survivors 0 and 1 sit in view 4 (leader(4) = 1),
/// the restarted replica 2 re-entered at view 2. The leader's header reaches
/// it: with round-skip it votes in view 4 at once; without, it stays behind.
#[test]
fn rejoiner_votes_in_survivors_view_without_timing_out() {
    for round_skip in [true, false] {
        let model = RejoinModel { round_skip, guard: true };
        let state = Cluster {
            replicas: [Replica::at(4), Replica::at(4), Replica::at(2)],
            headers: vec![],
            crashes_left: 0,
            skip_voted: false,
        };
        let state = model.next_state(&state, Step::Propose(4, 0)).unwrap();
        let state = model.next_state(&state, Step::Deliver(2, 4)).unwrap();
        assert_eq!(state.replicas[2].voted, round_skip.then_some(4));
        assert_eq!(state.replicas[2].view, if round_skip { 4 } else { 2 });
    }
}

