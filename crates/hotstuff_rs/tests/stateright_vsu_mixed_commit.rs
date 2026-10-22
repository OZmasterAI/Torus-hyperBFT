/*
    s138 (Codex P1 on 93948e2b): Stateright model of the PHASED validator-set-update (VSU) window,
    with mixed commits (a Commit / Decide PC that reaches only some honest replicas), re-proposals
    and the three lock rules for a proposal of the locked block.

    The 2-chain model (stateright_2chain_safety.rs) covers Generic blocks only. In pipelined mode
    the locked block is always above the committed one, so a COMMITTED locked block (the case the
    Codex P1 is about) only exists in the phased window, modeled here.

    WHAT IT ENCODES (crates/hotstuff_rs/src/block_tree/invariants.rs)
      * A (agreed, QC(A) at view 1 = `qa`) <- U, and optionally a conflicting sibling U' <- A (a
        Byzantine leader's update block). Both justify on `qa`; their hashes differ.
      * Phase votes Prepare (on a proposal) -> Precommit -> Commit -> Decide (on nudges).
      * update(pc) (`BlockTreeSingleton::update`): highest PC by view; pc_to_lock (Precommit locks;
        Commit / Decide lock when the lock is on another block; strictly greater view);
        block_to_commit (Commit / Decide commit pc.block); Decide sets the decided flag.
      * Proposal lock rule (`safe_proposal_lock`) on a proposal of B with justify qa:
        qa satisfies `safe_pc_lock_clause` (qa.view > lock.view, or qa.block = A extends the locked
        block, i.e. the lock is still on A), or the reflexive arm, per `LockRule`.
      * safe_nudge: safe_pc (pc.view > lock.view or pc.block extends the locked block), the phase is
        Prepare / Precommit / Commit, and it is a Commit PC or from the view right before.
      * vote once per view (highest_view_voted).
      * n = 4, f = 1: honest replicas 0, 1, 2; validator 3 is Byzantine (votes for anything in the
        adversarial prefix, withholds in the synchronous suffix).

    ACTIONS (adversarial prefix, views 2..=max_view): a PC forms with any quorum of voters (the
    Byzantine one included or not) whose honest members may vote; any honest replica processes any
    formed PC at any time (nudges, TCs, sync, or never: lost messages).

    PROPERTIES
      * `agreement` (always): no two honest replicas commit different blocks.
      * `sync_suffix_decides` (always): from the state, a synchronous suffix decides the update at
        every honest replica within SUFFIX_VIEWS views, starting right after the last used view or
        one view later (that view's nudge lost). In the suffix the Byzantine validator is silent and
        never leads (dead-leader skip), every honest leader acts on the highest PC it learns from the
        TC (max by view over the honest replicas; a TC moves only highest PCs, as
        `advance_highest_pc_from_remote`), and a PC formed by the three honest votes reaches every
        honest replica (next view's nudge / child proposal).
      * `committed_reflexive_vote` (sometimes): a replica voted for its COMMITTED locked block.

    RESULTS (tests below)
      * LockRule::Original (justify-only lock, pre-93948e2b): `sync_suffix_decides` fails (the
        Precommit-stage livelock that 93948e2b fixed).
      * LockRule::ReflexiveUncommitted (93948e2b): fails with the Codex P1 sequence (committed
        replica refuses U's re-proposal).
      * LockRule::Reflexive (the fix): holds; `agreement` holds with the sibling U' too.
*/

use std::collections::BTreeSet;

use stateright::{Checker, Model, Property};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum BlockId {
    U,
    /// U': a conflicting sibling of U (same parent A, same justify qa, another hash).
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Phase {
    Prepare,
    Precommit,
    Commit,
    Decide,
}

impl Phase {
    fn next(self) -> Option<Phase> {
        match self {
            Phase::Prepare => Some(Phase::Precommit),
            Phase::Precommit => Some(Phase::Commit),
            Phase::Commit => Some(Phase::Decide),
            Phase::Decide => None,
        }
    }
}

/// A phase certificate on an updating block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Pc {
    block: BlockId,
    phase: Phase,
    view: u8,
}

/// The lock rule for a proposal of the locked block itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LockRule {
    /// The lock clause on the justify only (before 93948e2b).
    Original,
    /// 93948e2b: or the block is the locked block and it is not committed.
    ReflexiveUncommitted,
    /// The fix: or the block is the locked block.
    Reflexive,
}

const HONEST: usize = 3;
/// View of qa = QC(A), the justify of U and U'.
const QA_VIEW: u8 = 1;
/// Synchronous-suffix length (honest-led views); the fix decides within 5.
const SUFFIX_VIEWS: u8 = 12;

/// One honest replica's consensus state. `highest: None` / `locked: None` = qa (view 1, on A).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Replica {
    highest: Option<Pc>,
    locked: Option<Pc>,
    committed: Option<BlockId>,
    decided: bool,
    /// highest_view_voted (0 = none).
    hvv: u8,
}

impl Replica {
    fn highest_view(&self) -> u8 {
        self.highest.map_or(QA_VIEW, |pc| pc.view)
    }
    fn lock_view(&self) -> u8 {
        self.locked.map_or(QA_VIEW, |pc| pc.view)
    }

    /// `update(pc)` (Highest PC, pc_to_lock, block_to_commit, decided flag).
    fn update(&mut self, pc: Pc) {
        if pc.view > self.highest_view() {
            self.highest = Some(pc);
        }
        let lock = match pc.phase {
            Phase::Prepare => None,
            Phase::Precommit => Some(pc),
            Phase::Commit | Phase::Decide => (self.locked.map(|l| l.block) != Some(pc.block)).then_some(pc),
        };
        if let Some(lock) = lock {
            if lock.view > self.lock_view() {
                self.locked = Some(lock);
            }
        }
        if matches!(pc.phase, Phase::Commit | Phase::Decide) && self.committed.is_none() {
            self.committed = Some(pc.block);
        }
        if pc.phase == Phase::Decide {
            self.decided = true;
        }
    }

    /// `advance_highest_pc_from_remote(pc)` (a TC's high PC).
    fn advance_highest(&mut self, pc: Pc) {
        if pc.view > self.highest_view() {
            self.highest = Some(pc);
        }
        if pc.phase == Phase::Decide {
            self.decided = true;
        }
    }

    /// May vote Prepare for a proposal of `block` (justify qa) in `view`. Returns
    /// `Some(committed_reflexive)` if so: whether only the reflexive arm on a committed block
    /// admitted it.
    fn proposal_vote(&self, block: BlockId, view: u8, rule: LockRule) -> Option<bool> {
        if self.hvv >= view {
            return None;
        }
        // safe_pc_lock_clause(qa): qa.view > lock.view, or A (qa.block) extends the locked block.
        if QA_VIEW > self.lock_view() || self.locked.is_none() {
            return Some(false);
        }
        let is_locked = self.locked.is_some_and(|l| l.block == block);
        let committed = self.committed == Some(block);
        match rule {
            LockRule::Original => None,
            LockRule::ReflexiveUncommitted => (is_locked && !committed).then_some(false),
            LockRule::Reflexive => is_locked.then_some(committed),
        }
    }

    /// May vote on a nudge of `justify` in `view` (safe_nudge).
    fn nudge_vote(&self, justify: Pc, view: u8) -> bool {
        self.hvv < view
            && justify.phase != Phase::Decide
            && (justify.phase == Phase::Commit || justify.view + 1 == view)
            && (justify.view > self.lock_view() || self.locked.is_none_or(|l| l.block == justify.block))
    }
}

#[derive(Clone)]
struct VsuModel {
    rule: LockRule,
    sibling: bool,
    max_view: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    pool: BTreeSet<Pc>,
    replicas: [Replica; HONEST],
    committed_reflexive_vote: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Action {
    /// A PC forms in `view` with the voters of `mask` (bits 0..=2 honest, bit 3 Byzantine). A
    /// Prepare PC is the votes on a proposal of `block`; any other phase the votes on a nudge of the
    /// formed PC of the previous phase (`justify_view`).
    FormPc { block: BlockId, phase: Phase, view: u8, justify_view: u8, mask: u8 },
    /// Honest replica `r` processes the formed `pc`.
    Deliver { r: usize, pc: Pc },
}

/// Quorum masks: the three honest replicas, or two of them with the Byzantine one. (All four has
/// the honest voters of the first.)
const MASKS: [u8; 4] = [0b0111, 0b1011, 0b1101, 0b1110];

impl VsuModel {
    fn blocks(&self) -> &'static [BlockId] {
        if self.sibling {
            &[BlockId::U, BlockId::Up]
        } else {
            &[BlockId::U]
        }
    }

    /// The synchronous suffix from `state`, starting `offset` views after the last used view.
    fn sync_suffix_decides(&self, state: &State, offset: u8) -> bool {
        let mut replicas = state.replicas.clone();
        let last = state
            .pool
            .iter()
            .map(|pc| pc.view)
            .chain(replicas.iter().map(|r| r.hvv))
            .max()
            .unwrap_or(QA_VIEW);
        for view in (last + offset..).take(SUFFIX_VIEWS as usize) {
            // The TC (or the nudge) of the previous view: every honest replica learns the highest PC.
            let high = replicas.iter().filter_map(|r| r.highest).max_by_key(|pc| pc.view);
            if let Some(high) = high {
                replicas.iter_mut().for_each(|r| r.advance_highest(high));
            }
            if replicas.iter().all(|r| r.decided) {
                return true;
            }
            // The honest leader acts on `high` (`repropose_block`, nudges).
            let formed = match high {
                Some(pc) if pc.phase == Phase::Decide => {
                    // A child proposal justified by the Decide PC: every replica applies it.
                    replicas.iter_mut().for_each(|r| r.update(pc));
                    None
                }
                Some(pc) if pc.phase == Phase::Commit || (pc.phase != Phase::Decide && view == pc.view + 1) => {
                    let voters: Vec<usize> = (0..HONEST).filter(|&r| replicas[r].nudge_vote(pc, view)).collect();
                    for &r in &voters {
                        replicas[r].update(pc);
                        replicas[r].hvv = view;
                    }
                    (voters.len() == HONEST).then(|| Pc { block: pc.block, phase: pc.phase.next().unwrap(), view })
                }
                // Re-propose the highest PC's block, or propose U fresh.
                high => {
                    let block = high.map_or(BlockId::U, |pc| pc.block);
                    let voters: Vec<usize> =
                        (0..HONEST).filter(|&r| replicas[r].proposal_vote(block, view, self.rule).is_some()).collect();
                    for &r in &voters {
                        replicas[r].hvv = view;
                    }
                    (voters.len() == HONEST).then_some(Pc { block, phase: Phase::Prepare, view })
                }
            };
            if let Some(pc) = formed {
                replicas.iter_mut().for_each(|r| r.update(pc));
            }
        }
        replicas.iter().all(|r| r.decided)
    }
}

impl Model for VsuModel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let replica = Replica { highest: None, locked: None, committed: None, decided: false, hvv: 0 };
        vec![State {
            pool: BTreeSet::new(),
            replicas: [replica.clone(), replica.clone(), replica],
            committed_reflexive_vote: false,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for &block in self.blocks() {
            for view in 2..=self.max_view {
                for mask in MASKS {
                    actions.push(Action::FormPc { block, phase: Phase::Prepare, view, justify_view: QA_VIEW, mask });
                }
            }
        }
        for justify in &state.pool {
            let Some(phase) = justify.phase.next() else { continue };
            for view in (justify.view + 1)..=self.max_view {
                for mask in MASKS {
                    actions.push(Action::FormPc { block: justify.block, phase, view, justify_view: justify.view, mask });
                }
            }
        }
        for r in 0..HONEST {
            for pc in &state.pool {
                actions.push(Action::Deliver { r, pc: *pc });
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        match action {
            Action::FormPc { block, phase, view, justify_view, mask } => {
                let pc = Pc { block, phase, view };
                if last.pool.contains(&pc) {
                    return None;
                }
                let mut st = last.clone();
                let justify = (phase != Phase::Prepare).then(|| {
                    let prev = [Phase::Prepare, Phase::Precommit, Phase::Commit][phase as usize - 1];
                    Pc { block, phase: prev, view: justify_view }
                });
                for r in 0..HONEST {
                    if mask & (1 << r) == 0 {
                        continue;
                    }
                    let replica = &mut st.replicas[r];
                    match justify {
                        None => {
                            let committed_reflexive = replica.proposal_vote(block, view, self.rule)?;
                            st.committed_reflexive_vote |= committed_reflexive;
                        }
                        Some(justify) => {
                            if !replica.nudge_vote(justify, view) {
                                return None;
                            }
                            replica.update(justify);
                        }
                    }
                    st.replicas[r].hvv = view;
                }
                st.pool.insert(pc);
                Some(st)
            }
            Action::Deliver { r, pc } => {
                let mut st = last.clone();
                st.replicas[r].update(pc);
                (st != *last).then_some(st)
            }
        }
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::<Self>::always("agreement", |_, state: &State| {
                let committed: BTreeSet<BlockId> = state.replicas.iter().filter_map(|r| r.committed).collect();
                committed.len() <= 1
            }),
            Property::<Self>::always("sync_suffix_decides", |model: &VsuModel, state: &State| {
                (1..=2).all(|offset| model.sync_suffix_decides(state, offset))
            }),
            Property::<Self>::sometimes("committed_reflexive_vote", |_, state: &State| state.committed_reflexive_vote),
        ]
    }
}

fn check(rule: LockRule, sibling: bool, max_view: u8) -> impl Checker<VsuModel> {
    VsuModel { rule, sibling, max_view }.checker().spawn_bfs().join()
}

/// 93948e2b's rule (committed locked blocks excluded) fails the synchronous suffix: the Codex P1
/// sequence (U's Commit PC at replica 0 only, Prepare(U) re-certified by replicas 1, 2 and the
/// Byzantine one, the next nudge lost) is a counterexample.
#[test]
fn reflexive_uncommitted_rule_strands_a_mixed_commit() {
    let checker = check(LockRule::ReflexiveUncommitted, false, 5);
    let path = checker.discovery("sync_suffix_decides").expect("the mixed commit strands the update");
    let last = path.last_state();
    assert!(
        last.replicas.iter().any(|r| r.committed.is_some()) && last.replicas.iter().any(|r| r.committed.is_none()),
        "the counterexample is a mixed commit: {:?}",
        path.into_actions()
    );
    assert!(checker.discovery("agreement").is_none());
}

/// The justify-only lock (before 93948e2b) fails too (the Precommit-stage livelock).
#[test]
fn original_rule_strands_the_update() {
    let checker = check(LockRule::Original, false, 5);
    assert!(checker.discovery("sync_suffix_decides").is_some());
}

/// The fix: every reachable state (mixed commits, lost nudges, re-certifications in views
/// 2..=5) decides in the synchronous suffix, and the committed-locked-block vote is exercised.
#[test]
fn reflexive_rule_decides_after_a_mixed_commit() {
    let checker = check(LockRule::Reflexive, false, 5);
    assert!(
        checker.discovery("sync_suffix_decides").is_none(),
        "the update is stranded under the reflexive rule: {:?}",
        checker.discovery("sync_suffix_decides").map(|p| p.into_actions())
    );
    assert!(checker.discovery("agreement").is_none());
    assert!(checker.discovery("committed_reflexive_vote").is_some(), "no replica voted for its committed locked block");
}

/// The fix keeps Agreement with a conflicting sibling U' (a Byzantine leader's update block on
/// the same parent): a vote for a committed locked block certifies only that block.
#[test]
fn reflexive_rule_keeps_agreement_with_a_conflicting_sibling() {
    let checker = check(LockRule::Reflexive, true, 5);
    assert!(
        checker.discovery("agreement").is_none(),
        "Agreement violated: {:?}",
        checker.discovery("agreement").map(|p| p.into_actions())
    );
    assert!(checker.discovery("committed_reflexive_vote").is_some());
}
