/*
    s75 rejoin leader sync (fix A) — per-replica proposal- and vote-safety
    model. Extends the s74 round-skip model (stateright_rejoin_view_sync.rs,
    kept as the B-only baseline) with honest leaders, NewView messages, the
    NewView-driven leader skip and the boot enter_view.

    WHAT IT ENCODES

    Three replicas with their own views, n = 3, f = 0, leader(v) = v mod 3.

    enter_view(v) (timeout, round-skip B, leader skip, boot), production
    order (`HotStuff::enter_view`):
      1. send NewView{prev} to leader(prev + 1), prev = the view being left
         (for a timeout that is NewView{v-1} to leader(v)). It is a direct,
         queued send: it may be delivered at any later time, repeatedly, also
         after the receiver restarted (per-peer queue flushed on reconnect);
      2. take view v; no proposal seen yet;
      3. persist `highest_view_entered = v` — a SEPARATE step that may lag
         (the write lost in a crash);
      4. if leader(v): propose — a separate step, allowed only once step 3
         is done (it is also how a deferred proposal is retried: the loop
         re-runs enter_view in the same view). The mutation
         `propose_before_persist` allows it before step 3.
    Persisted means durable here. Production's RocksDB write is not fsync'd
    (kv_store.rs): SIGKILL keeps it, power loss may not — the same class as
    the vote state (`set_vote_state_atomic`). A lost entered view is exactly
    what `propose_before_persist` exercises: a crash between the proposal and
    the persist.

    Boot: a restart comes back at init = entered + 1 (`Algorithm::new`).
    With fix A, HotStuff starts at init - 1 and the first loop pass runs
    enter_view(init) (NewView{init-1}, and a proposal if it leads init).
    Without fix A it sits in init without entering it: no NewView, no
    proposal there.

    Leader skip (fix A, `on_receive_new_view` step 4): replica i at local
    view l receives NewView{w-1}, leader(w) == i, l < w, w - l <= MAX_SKIP,
    and (w > l + 1 or no proposal seen in l): enter_view(w). One NewView is
    power > f at n = 3.

    Votes and round-skip B are as in the s74 model: decide / persist / send
    are separate steps (mutation `send_first`); a future header with the
    voted view below it skips to its view. A Byzantine copy of leader(v)'s key
    may add one extra block to a view (`Equivocate`), so "one vote per view"
    is a real constraint; it is an environment action, not an honest send.

    PROPERTIES
      * always: every replica's SENT votes have strictly increasing views
        (an identical resend aside);
      * always: every replica's honest PROPOSALS have strictly increasing
        views, across crash/restart — never two different proposals in one
        view, never one below an earlier one;
      * sometimes (coverage): a vote decided right after a round-skip is sent;
        a restarted replica proposes after a leader skip; a restarted
        replica proposes in its init view at boot.

    ASSERTIONS
      * both safety properties hold with fix A + round-skip on, and with both
        off (TORUS_ROUND_SKIP=0);
      * `propose_before_persist` breaks proposal safety (5 steps: propose
        in v, crash, restart in v, boot enter_view re-proposes) and
        `send_first` breaks vote safety (the model has teeth). The mutation
        is caught without fix A too (restart below v, time out into v): the
        persist order guarded proposals before fix A; the boot entry only
        makes the re-proposal immediate;
      * without fix A no restarted replica ever proposes after a leader skip
        or at boot;
      * the s74 pattern-A scenarios: the rejoiner leads the survivors' view
        v and restarted at v - 2 (leader skip) or at v (boot): with fix A it
        proposes in v without a timeout, without fix A it does not.
    Regular suite to FAST_DEPTH; exhaustive in the `--ignored` test.

    NOT MODELLED: block contents / highest_pc / QCs (a proposal is a block
    id), the TC and re-propose paths, reputation-weighted leaders, the n >= 4
    tally (f = 0 here: one NewView is enough).
*/

use stateright::{Checker, HasDiscoveries, Model, Property};

const N: usize = 3;
/// Exploration bound on views.
const MAX_VIEW: u8 = 3;
/// Crash budget per run.
const CRASHES: u8 = 1;
/// Mirrors `MAX_ROUND_SKIP_VIEWS` (never binding at this view bound).
const MAX_SKIP: u8 = 16;

/// How a replica entered the view it leads (coverage only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Entry {
    Boot,
    LeaderSkip,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Replica {
    alive: bool,
    /// Restarted at least once (a "rejoiner").
    rebooted: bool,
    /// In-memory current view.
    view: u8,
    /// Persisted `highest_view_entered` (may lag `view`).
    entered: u8,
    /// Leads `view` and has not proposed there yet (how it entered).
    lead: Option<Entry>,
    /// A proposal for `view` was seen (`ProposalStatus` != WaitingForProposal).
    seen_proposal: bool,
    /// Last proposal this replica ever sent (view, block).
    last_proposed: Option<(u8, u8)>,
    proposal_violated: bool,
    /// Persisted voted view (`highest_view_phase_voted`).
    voted: Option<u8>,
    /// Decided, not yet sent (lost in a crash).
    pending: Option<(u8, u8)>,
    /// `send_first` only: sent, not yet persisted (lost in a crash).
    unpersisted: Option<u8>,
    /// Last vote this replica ever sent.
    last_sent: Option<(u8, u8)>,
    vote_violated: bool,
}

impl Replica {
    /// Entered `view` (persisted) through enter_view.
    fn at(i: usize, view: u8) -> Self {
        Replica {
            alive: true,
            rebooted: false,
            view,
            entered: view,
            lead: (leader(view) == i).then_some(Entry::Other),
            seen_proposal: false,
            last_proposed: None,
            proposal_violated: false,
            voted: None,
            pending: None,
            unpersisted: None,
            last_sent: None,
            vote_violated: false,
        }
    }

    /// Crashed with `entered` persisted.
    fn down(i: usize, entered: u8) -> Self {
        Replica {
            alive: false,
            lead: None,
            ..Replica::at(i, entered)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Cluster {
    replicas: [Replica; N],
    /// Proposed headers (view, block), sorted.
    headers: Vec<(u8, u8)>,
    /// Bit v: NewView{v} sent to leader(v + 1) by another replica.
    new_views: u16,
    crashes_left: u8,
    /// Coverage: a vote decided right after a skip was sent.
    skip_vote_sent: bool,
    /// (replica, view, block) of pending votes decided after a skip.
    skip_pending: Vec<(usize, u8, u8)>,
    /// Coverage: a restarted replica proposed after a leader skip.
    rejoiner_skip_proposed: bool,
    /// Coverage: a restarted replica proposed in its init view at boot.
    rejoiner_boot_proposed: bool,
}

impl Cluster {
    fn new(replicas: [Replica; N], crashes_left: u8) -> Self {
        Cluster {
            replicas,
            headers: vec![],
            new_views: 0,
            crashes_left,
            skip_vote_sent: false,
            skip_pending: vec![],
            rejoiner_skip_proposed: false,
            rejoiner_boot_proposed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Step {
    Timeout(usize),
    PersistEntered(usize),
    Propose(usize),
    /// A Byzantine copy of leader(view)'s key adds a block to the view.
    Equivocate(u8),
    Deliver(usize, u8, u8),
    /// NewView{view} reaches leader(view + 1).
    DeliverNewView(u8),
    Send(usize),
    PersistVote(usize),
    Crash(usize),
    Restart(usize),
}

fn leader(view: u8) -> usize {
    view as usize % N
}

struct LeaderSyncModel {
    /// Exploration bound on views (scenarios lift it).
    max_view: u8,
    round_skip: bool,
    fix_a: bool,
    /// Mutation: send the vote before persisting it.
    send_first: bool,
    /// Mutation: propose before persisting the entered view.
    propose_before_persist: bool,
}

impl LeaderSyncModel {
    fn new(on: bool) -> Self {
        LeaderSyncModel {
            max_view: MAX_VIEW,
            round_skip: on,
            fix_a: on,
            send_first: false,
            propose_before_persist: false,
        }
    }

    /// `HotStuff::enter_view(view)` up to (not including) the persist.
    fn enter(&self, state: &mut Cluster, i: usize, view: u8, entry: Entry) {
        let r = &mut state.replicas[i];
        let prev = r.view;
        if leader(prev + 1) != i {
            state.new_views |= 1 << prev;
        }
        r.view = view;
        r.seen_proposal = false;
        r.lead = (leader(view) == i).then_some(entry);
    }

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

    /// Production: `set_highest_view_entered(view)` precedes the propose path.
    fn can_propose(&self, r: &Replica) -> bool {
        r.alive && r.lead.is_some() && (self.propose_before_persist || r.entered == r.view)
    }
}

impl Model for LeaderSyncModel {
    type State = Cluster;
    type Action = Step;

    fn init_states(&self) -> Vec<Self::State> {
        vec![Cluster::new(
            [Replica::at(0, 1), Replica::at(1, 1), Replica::at(2, 1)],
            CRASHES,
        )]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        for (i, r) in state.replicas.iter().enumerate() {
            if !r.alive {
                if r.entered < self.max_view {
                    actions.push(Step::Restart(i));
                }
                continue;
            }
            if r.view < self.max_view {
                actions.push(Step::Timeout(i));
            }
            if r.entered < r.view {
                actions.push(Step::PersistEntered(i));
            }
            if self.can_propose(r) {
                actions.push(Step::Propose(i));
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
        for view in 0..16 {
            if state.new_views & (1 << view) != 0 {
                actions.push(Step::DeliverNewView(view));
            }
        }
        for view in 1..=self.max_view {
            let in_view = state.headers.iter().filter(|h| h.0 == view).count();
            if in_view == 1 {
                actions.push(Step::Equivocate(view));
            }
        }
    }

    fn next_state(&self, state: &Self::State, action: Self::Action) -> Option<Self::State> {
        let mut next = state.clone();
        match action {
            Step::Timeout(i) => {
                let view = next.replicas[i].view + 1;
                self.enter(&mut next, i, view, Entry::Other);
            }
            Step::PersistEntered(i) => next.replicas[i].entered = next.replicas[i].view,
            Step::Propose(i) => {
                let r = &mut next.replicas[i];
                let view = r.view;
                let block = (0..)
                    .find(|b| !state.headers.contains(&(view, *b)))
                    .unwrap();
                if let Some((last_view, last_block)) = r.last_proposed {
                    r.proposal_violated |=
                        view < last_view || (view == last_view && block != last_block);
                }
                r.last_proposed = Some((view, block));
                r.seen_proposal = true;
                if r.rebooted {
                    match r.lead {
                        Some(Entry::Boot) => next.rejoiner_boot_proposed = true,
                        Some(Entry::LeaderSkip) => next.rejoiner_skip_proposed = true,
                        _ => {}
                    }
                }
                r.lead = None;
                next.headers.push((view, block));
                next.headers.sort();
                // The leader votes on its own proposal (inline header path).
                self.decide(&mut next, i, view, block, false);
            }
            Step::Equivocate(view) => {
                let block = (0..)
                    .find(|b| !state.headers.contains(&(view, *b)))
                    .unwrap();
                next.headers.push((view, block));
                next.headers.sort();
            }
            Step::Deliver(i, view, block) => {
                let r = &mut next.replicas[i];
                let skipped = view > r.view
                    && self.round_skip
                    && view - r.view <= MAX_SKIP
                    && r.voted.is_none_or(|voted| voted < view);
                if skipped {
                    self.enter(&mut next, i, view, Entry::Other);
                }
                let r = &mut next.replicas[i];
                if view == r.view {
                    r.seen_proposal = true;
                    self.decide(&mut next, i, view, block, skipped);
                }
            }
            Step::DeliverNewView(nv) => {
                // Without fix A a future NewView is only buffered (its
                // highest_pc is not modelled): no effect.
                let (i, w) = (leader(nv + 1), nv + 1);
                let r = &next.replicas[i];
                let skip = self.fix_a
                    && r.alive
                    && w > r.view
                    && w - r.view <= MAX_SKIP
                    && (w > r.view + 1 || !r.seen_proposal);
                if skip {
                    self.enter(&mut next, i, w, Entry::LeaderSkip);
                }
            }
            Step::Send(i) => {
                let r = &mut next.replicas[i];
                let (view, block) = r.pending.take().unwrap();
                if let Some((last_view, last_block)) = r.last_sent {
                    r.vote_violated |=
                        view < last_view || (view == last_view && block != last_block);
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
                r.lead = None;
                r.seen_proposal = false;
                r.pending = None;
                r.unpersisted = None;
                next.skip_pending.retain(|p| p.0 != i);
                next.crashes_left -= 1;
            }
            Step::Restart(i) => {
                let r = &mut next.replicas[i];
                r.alive = true;
                r.rebooted = true;
                let init = r.entered + 1;
                if self.fix_a {
                    // HotStuff starts at init - 1; the first loop pass (before
                    // any message is polled) runs enter_view(init).
                    r.view = r.entered;
                    self.enter(&mut next, i, init, Entry::Boot);
                } else {
                    r.view = init;
                }
            }
        }
        (next != *state).then_some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always("sent vote views strictly increase", |_, state| {
                state.replicas.iter().all(|r| !r.vote_violated)
            }),
            Property::<Self>::always("proposal views strictly increase", |_, state| {
                state.replicas.iter().all(|r| !r.proposal_violated)
            }),
            Property::<Self>::sometimes("a vote decided after a round-skip is sent", |_, state| {
                state.skip_vote_sent
            }),
            Property::<Self>::sometimes("a rejoiner proposes after a leader skip", |_, state| {
                state.rejoiner_skip_proposed
            }),
            Property::<Self>::sometimes(
                "a rejoiner proposes in its init view at boot",
                |_, state| state.rejoiner_boot_proposed,
            ),
        ]
    }
}

/// Exploration depth of the regular (fast) checks.
const FAST_DEPTH: usize = 12;

/// Explore `model` to `depth` (None: exhaustively). With `until`, stop at the
/// first counterexample to that property (mutants).
fn check(
    model: LeaderSyncModel,
    depth: Option<usize>,
    until: Option<&'static str>,
) -> impl Checker<LeaderSyncModel> {
    let label = format!(
        "round_skip={} fix_a={} send_first={} propose_before_persist={}",
        model.round_skip, model.fix_a, model.send_first, model.propose_before_persist
    );
    let mut builder = model.checker();
    if let Some(property) = until {
        builder = builder.finish_when(HasDiscoveries::AnyOf([property].into()));
    }
    let checker = match depth {
        Some(depth) => builder.target_max_depth(depth).spawn_bfs().join(),
        None => builder.spawn_bfs().join(),
    };
    eprintln!(
        "{label}: {} states, {} unique, max depth {}",
        checker.state_count(),
        checker.unique_state_count(),
        checker.max_depth()
    );
    checker
}

const VOTE_SAFETY: &str = "sent vote views strictly increase";
const PROPOSAL_SAFETY: &str = "proposal views strictly increase";

fn assert_safety(depth: Option<usize>) {
    // Fix A + round-skip on: safe, and every coverage path is reached.
    let on = check(LeaderSyncModel::new(true), depth, None);
    on.assert_properties();

    // TORUS_ROUND_SKIP=0 (no fix A either): safe, no leader skip, no boot proposal.
    let off = check(LeaderSyncModel::new(false), depth, None);
    off.assert_no_discovery(VOTE_SAFETY);
    off.assert_no_discovery(PROPOSAL_SAFETY);
    off.assert_no_discovery("a rejoiner proposes after a leader skip");
    off.assert_no_discovery("a rejoiner proposes in its init view at boot");

    let mutant = LeaderSyncModel {
        propose_before_persist: true,
        ..LeaderSyncModel::new(true)
    };
    assert!(
        check(mutant, depth, Some(PROPOSAL_SAFETY))
            .discovery(PROPOSAL_SAFETY)
            .is_some(),
        "proposing before persisting the entered view must let a restarted leader re-propose"
    );
    // The persist order was already the guard before fix A: a restart below
    // v that times out into v re-proposes too. Fix A's boot enter_view only
    // makes it immediate (restart view == v).
    let mutant = LeaderSyncModel {
        fix_a: false,
        propose_before_persist: true,
        ..LeaderSyncModel::new(true)
    };
    assert!(
        check(mutant, depth, Some(PROPOSAL_SAFETY))
            .discovery(PROPOSAL_SAFETY)
            .is_some(),
        "without fix A the mutation must still be caught (timeout re-entry)"
    );
    let mutant = LeaderSyncModel {
        send_first: true,
        ..LeaderSyncModel::new(true)
    };
    assert!(
        check(mutant, depth, Some(VOTE_SAFETY))
            .discovery(VOTE_SAFETY)
            .is_some(),
        "sending before persisting must let a crash-restarted replica vote twice"
    );
}

#[test]
fn rejoin_leader_sync_preserves_proposal_and_vote_safety() {
    assert_safety(Some(FAST_DEPTH));
}

/// Exhaustive at MAX_VIEW = 3, CRASHES = 1 (s75: passed, ~131 s, 14.3 M
/// unique states with fix A on, 0.76 GB RSS).
#[test]
#[ignore = "exhaustive, ~2.5 min; run with --ignored"]
fn rejoin_leader_sync_preserves_proposal_and_vote_safety_exhaustive() {
    assert_safety(None);
}

/// Apply `steps` in order; a step the model does not enable (not in
/// `actions`, or a no-op) leaves the state unchanged.
fn run(model: &LeaderSyncModel, mut state: Cluster, steps: &[Step]) -> Cluster {
    for step in steps {
        let mut enabled = vec![];
        model.actions(&state, &mut enabled);
        if !enabled.contains(step) {
            continue;
        }
        if let Some(next) = model.next_state(&state, step.clone()) {
            state = next;
        }
    }
    state
}

/// Survivors 0 and 1 entered v = 5 by timeout (leader(5) = 2) and queued
/// NewView{4} to the rejoiner 2, which is down with entered = `entered`.
fn pattern_a(entered: u8) -> Cluster {
    let mut state = Cluster::new(
        [
            Replica::at(0, 5),
            Replica::at(1, 5),
            Replica::down(2, entered),
        ],
        0,
    );
    state.new_views = 1 << 4;
    state
}

/// s74 bb-serial-r1 v423 / rv-skip-r3 v570: the rejoiner leads the
/// survivors' view v and restarted at v - 2. With fix A the queued NewView
/// makes it skip to v and propose there, no timeout; without, it stays.
#[test]
fn rejoiner_restarted_below_its_view_proposes_after_leader_skip() {
    for fix_a in [true, false] {
        let model = LeaderSyncModel {
            max_view: 8,
            ..LeaderSyncModel::new(fix_a)
        };
        let state = run(
            &model,
            pattern_a(2),
            &[
                Step::Restart(2),
                Step::DeliverNewView(4),
                Step::PersistEntered(2),
                Step::Propose(2),
            ],
        );
        let r = &state.replicas[2];
        if fix_a {
            assert_eq!(r.view, 5);
            assert_eq!(r.last_proposed, Some((5, 0)));
            assert_eq!(state.headers, vec![(5, 0)]);
            assert!(state.rejoiner_skip_proposed);
        } else {
            assert_eq!(r.view, 3, "restarted at v - 2 and stays there");
            assert_eq!(r.last_proposed, None);
            assert!(state.headers.is_empty());
        }
    }
}

/// Restarted exactly at v (entered = v - 1): with fix A the boot
/// enter_view proposes in v; without, the replica sits in v silently.
#[test]
fn rejoiner_restarted_in_its_view_proposes_at_boot() {
    for fix_a in [true, false] {
        let model = LeaderSyncModel {
            max_view: 8,
            ..LeaderSyncModel::new(fix_a)
        };
        let state = run(
            &model,
            pattern_a(4),
            &[Step::Restart(2), Step::PersistEntered(2), Step::Propose(2)],
        );
        let r = &state.replicas[2];
        assert_eq!(r.view, 5);
        if fix_a {
            assert_eq!(r.last_proposed, Some((5, 0)));
            assert!(state.rejoiner_boot_proposed);
        } else {
            assert_eq!(r.last_proposed, None);
            assert!(state.headers.is_empty());
        }
    }
}

/// The mutation's trace, spelled out: the leader of view 2 proposes, crashes
/// before its entered view is persisted, restarts in view 2 and, through the
/// boot enter_view, proposes a second block there. Production order refuses
/// the first proposal until the entered view is persisted.
#[test]
fn propose_before_persist_reproposes_after_restart() {
    let steps = [
        Step::Timeout(2),
        Step::Propose(2),
        Step::Crash(2),
        Step::Restart(2),
        Step::Propose(2),
    ];
    let mutant = LeaderSyncModel {
        propose_before_persist: true,
        ..LeaderSyncModel::new(true)
    };
    let state = run(&mutant, mutant.init_states().remove(0), &steps);
    assert!(state.replicas[2].proposal_violated, "{state:?}");

    let model = LeaderSyncModel::new(true);
    let first = run(&model, model.init_states().remove(0), &steps[..2]);
    assert_eq!(
        first.replicas[2].last_proposed, None,
        "no proposal before the persist"
    );
}
