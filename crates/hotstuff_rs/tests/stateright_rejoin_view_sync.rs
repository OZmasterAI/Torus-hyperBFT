/*
    s74 rejoin view sync (round-skip) — per-replica vote-safety model.

    WHAT IT ENCODES

    Three replicas with their own views (not lock-step), n = 3, leader(v) =
    v mod 3. Leaders may equivocate (up to two blocks per view), so "one vote
    per view" is a real constraint.

    Persistence is modelled separately from memory, so safety must come from
    the right guard rather than hold by construction:
      * the entered view is persisted by a separate step that may lag (an
        unsynced write lost in a crash), so a restart can come back at a view
        at or below one it already voted in: `init_view = entered + 1`
        (`Algorithm::new`);
      * a vote is DECIDED, PERSISTED (`set_vote_state_atomic`) and SENT as
        distinct steps, with a crash possible between any two. Production
        persists before it sends (s65); the mutation `send_first` sends
        before it persists.

    A header (view, block) delivered to a replica:
      * current view: decide a vote if the persisted voted view is below it
        (`on_receive_proposal_header` vote gate);
      * FUTURE view w, round-skip on, voted view below w, w - view within
        MAX_SKIP: enter w (pacemaker `skip_to_view` + `enter_view`), then
        the same current-view gate;
      * past view: nothing.

    PROPERTIES
      * always: every replica's SENT votes have strictly increasing views,
        except an identical (view, block) resend — so never two different
        blocks in one view and never a vote below an earlier one;
      * sometimes: a replica sends a vote it decided right after a skip
        (coverage: the skip path is really explored).

    ASSERTIONS
      * safety holds with round-skip on and off (persist-before-send), and
        the checker finds a violation for the send-before-persist mutation
        (the model has teeth): to depth FAST_DEPTH in the regular suite,
        exhaustively in the `--ignored` test;
      * the motivating s64 scenario votes in the survivors' view at once
        only with round-skip on.
*/

use stateright::{Checker, Model, Property};

const N: usize = 3;
/// Exploration bound on views.
const MAX_VIEW: u8 = 3;
/// Blocks a leader may propose in one view (2 = equivocation allowed).
const BLOCKS: u8 = 2;
/// Crash budget per run.
const CRASHES: u8 = 1;
/// Mirrors `MAX_ROUND_SKIP_VIEWS` (never binding at this view bound).
const MAX_SKIP: u8 = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Replica {
    alive: bool,
    /// In-memory current view.
    view: u8,
    /// Persisted `highest_view_entered` (may lag `view`).
    entered: u8,
    /// Persisted voted view (`highest_view_phase_voted`).
    voted: Option<u8>,
    /// Decided, not yet sent (lost in a crash).
    pending: Option<(u8, u8)>,
    /// `send_first` only: sent, not yet persisted (lost in a crash).
    unpersisted: Option<u8>,
    /// Last vote this replica ever sent.
    last_sent: Option<(u8, u8)>,
    violated: bool,
}

impl Replica {
    fn at(view: u8) -> Self {
        Replica {
            alive: true,
            view,
            entered: view,
            voted: None,
            pending: None,
            unpersisted: None,
            last_sent: None,
            violated: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Cluster {
    replicas: [Replica; N],
    /// Proposed headers (view, block), sorted.
    headers: Vec<(u8, u8)>,
    crashes_left: u8,
    /// Coverage: a vote decided right after a skip was sent.
    skip_vote_sent: bool,
    /// (replica, view, block) of pending votes decided after a skip.
    skip_pending: Vec<(usize, u8, u8)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Step {
    Timeout(usize),
    PersistEntered(usize),
    Propose(u8, u8),
    Deliver(usize, u8, u8),
    Send(usize),
    PersistVote(usize),
    Crash(usize),
    Restart(usize),
}

fn leader(view: u8) -> usize {
    view as usize % N
}

struct RejoinModel {
    round_skip: bool,
    /// Mutation: send the vote before persisting it.
    send_first: bool,
}

impl RejoinModel {
    /// Decide a vote for (view, block) if the persisted guard allows it.
    fn decide(&self, state: &mut Cluster, i: usize, view: u8, block: u8, skipped: bool) {
        let r = &mut state.replicas[i];
        if r.pending.is_some() || !r.voted.is_none_or(|voted| voted < view) {
            return;
        }
        if !self.send_first {
            r.voted = Some(view); // persist, then send (s65 order)
        }
        r.pending = Some((view, block));
        if skipped {
            state.skip_pending.push((i, view, block));
        }
    }
}

impl Model for RejoinModel {
    type State = Cluster;
    type Action = Step;

    fn init_states(&self) -> Vec<Self::State> {
        vec![Cluster {
            replicas: [Replica::at(1), Replica::at(1), Replica::at(1)],
            headers: vec![],
            crashes_left: CRASHES,
            skip_vote_sent: false,
            skip_pending: vec![],
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        for (i, r) in state.replicas.iter().enumerate() {
            if !r.alive {
                if r.entered < MAX_VIEW {
                    actions.push(Step::Restart(i));
                }
                continue;
            }
            if r.view < MAX_VIEW {
                actions.push(Step::Timeout(i));
            }
            if r.entered < r.view {
                actions.push(Step::PersistEntered(i));
            }
            if r.pending.is_some() {
                actions.push(Step::Send(i));
            }
            if r.unpersisted.is_some() {
                actions.push(Step::PersistVote(i));
            }
            if state.crashes_left > 0 {
                actions.push(Step::Crash(i));
            }
            for &(view, block) in &state.headers {
                if leader(view) != i {
                    actions.push(Step::Deliver(i, view, block));
                }
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
            Step::PersistEntered(i) => next.replicas[i].entered = next.replicas[i].view,
            Step::Propose(view, block) => {
                next.headers.push((view, block));
                next.headers.sort();
                // The leader votes on its own proposal (inline header path).
                self.decide(&mut next, leader(view), view, block, false);
            }
            Step::Deliver(i, view, block) => {
                let r = &mut next.replicas[i];
                let skipped = view > r.view
                    && self.round_skip
                    && view - r.view <= MAX_SKIP
                    && r.voted.is_none_or(|voted| voted < view);
                if skipped {
                    r.view = view;
                }
                if view == r.view {
                    self.decide(&mut next, i, view, block, skipped);
                }
            }
            Step::Send(i) => {
                let r = &mut next.replicas[i];
                let (view, block) = r.pending.take().unwrap();
                if let Some((last_view, last_block)) = r.last_sent {
                    r.violated |= view < last_view || (view == last_view && block != last_block);
                }
                r.last_sent = Some((view, block));
                if self.send_first {
                    r.unpersisted = Some(view);
                }
                if next.skip_pending.contains(&(i, view, block)) {
                    next.skip_vote_sent = true;
                }
            }
            Step::PersistVote(i) => {
                let r = &mut next.replicas[i];
                r.voted = r.unpersisted.take();
            }
            Step::Crash(i) => {
                let r = &mut next.replicas[i];
                r.alive = false;
                r.pending = None;
                r.unpersisted = None;
                next.skip_pending.retain(|p| p.0 != i);
                next.crashes_left -= 1;
            }
            Step::Restart(i) => {
                let r = &mut next.replicas[i];
                r.alive = true;
                r.view = r.entered + 1;
                r.entered = r.view;
            }
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always("sent vote views strictly increase", |_, state| {
                state.replicas.iter().all(|r| !r.violated)
            }),
            Property::<Self>::sometimes("a vote decided after a round-skip is sent", |_, state| {
                state.skip_vote_sent
            }),
        ]
    }
}

/// Exploration depth of the regular (fast) checks. The shortest skip-then-send
/// path is ~5 steps and the shortest send-before-persist violation ~8.
const FAST_DEPTH: usize = 12;

fn check(round_skip: bool, send_first: bool, depth: Option<usize>) -> impl Checker<RejoinModel> {
    let builder = RejoinModel { round_skip, send_first }.checker();
    match depth {
        Some(depth) => builder.target_max_depth(depth).spawn_bfs().join(),
        None => builder.spawn_bfs().join(),
    }
}

fn assert_safety(depth: Option<usize>) {
    check(true, false, depth).assert_properties(); // safe, and the skip-vote path is reached
    check(false, false, depth).assert_no_discovery("sent vote views strictly increase");
    assert!(
        check(true, true, depth).discovery("sent vote views strictly increase").is_some(),
        "sending before persisting must let a crash-restarted replica vote twice"
    );
}

#[test]
fn rejoin_round_skip_preserves_vote_safety() {
    assert_safety(Some(FAST_DEPTH));
}

/// Exhaustive at MAX_VIEW = 3, CRASHES = 1 (s74: passed, ~390 s, ~2.8 GB).
#[test]
#[ignore = "exhaustive, ~6.5 min; run with --ignored"]
fn rejoin_round_skip_preserves_vote_safety_exhaustive() {
    assert_safety(None);
}

/// The s64 crash scenario: survivors 0 and 1 sit in view 4 (leader(4) = 1),
/// the restarted replica 2 re-entered at view 2. The leader's header reaches
/// it: with round-skip it votes in view 4 at once; without, it stays behind.
#[test]
fn rejoiner_votes_in_survivors_view_without_timing_out() {
    for round_skip in [true, false] {
        let model = RejoinModel { round_skip, send_first: false };
        let state = Cluster {
            replicas: [Replica::at(4), Replica::at(4), Replica::at(2)],
            headers: vec![],
            crashes_left: 0,
            skip_vote_sent: false,
            skip_pending: vec![],
        };
        let state = model.next_state(&state, Step::Propose(4, 0)).unwrap();
        let state = model.next_state(&state, Step::Deliver(2, 4, 0)).unwrap();
        assert_eq!(state.replicas[2].pending, round_skip.then_some((4, 0)));
        assert_eq!(state.replicas[2].view, if round_skip { 4 } else { 2 });
    }
}
