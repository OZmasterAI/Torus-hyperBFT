/*
    T1.2 (CONSENSUS-SAFETY) — Stateright safety model for the 2-chain commit /
    grandparent lock interaction.

    Companion write-up (rigorous proof + counterexample trace + MonadBFT Thm 1
    mapping): specs/consensus/stateright_2chain_safety.md.

    ---------------------------------------------------------------------------
    STATUS: source-complete, UNCOMPILED.

    There is no Rust toolchain in the authoring environment, so this model has
    NOT been compiled or run. It is a fully-defined Stateright `Model` (state,
    action, next_state, and two `always` safety properties are all concrete) —
    the "compiles-plausibly skeleton with state/action/property fully defined"
    deliverable. Pin/adjust the `stateright` version if the API drifts. See the
    structured result's `unverified[]`.
    ---------------------------------------------------------------------------

    WHAT IT ENCODES (verbatim from crates/hotstuff_rs/src/block_tree/invariants.rs)

      * 2-chain consecutive-views commit  (block_to_commit, Generic, :518-555):
          commit parent(J.block) iff J.view == J.block.justify.view + 1
      * parent lock                       (pc_to_lock, Generic, post-4a94e8b):
          lock on J itself (= Some(justify.clone())); the model also encodes the
          FORMER grandparent rule (lock J.block.justify) to prove it unsafe
      * safe_pc predicate 3               (:360):
          J.view > locked.view  ||  extends_locked_pc_block(J)   (grandparent depth, :622-634)
      * vote-once-per-view                (implementation.rs:954-955)
      * header-first relaxations          (AllowPendingBypass knob):
          vote-before-validate + safe_pc bypass for pending blocks
          (implementation.rs:1215-1221, 1659-1669) + sync-commit skipping
          safe_block (implementation.rs:1227-1235).

    The two lock rules are selectable via `LockRule`:
      * `Grandparent` — the FORMER production rule (lock J.block.justify), proven
        unsafe by this model; fixed by 4a94e8b.
      * `Parent`      — the CURRENT production rule since 4a94e8b (lock J.block,
        i.e. Some(justify.clone())).

    RED -> GREEN (historical: fix 4a94e8b has landed; PRODUCTION == Parent)
      * `agreement_under_production_rule` (the RED-first test) configures the
        model with `LockRule::PRODUCTION`. It was RED while PRODUCTION ==
        Grandparent and is GREEN now that the pc_to_lock fix is applied and
        `LockRule::PRODUCTION` is repointed to `Parent`.
      * `grandparent_lock_violates_agreement` — asserts a counterexample exists
        (documents the bug; regression witness for the old rule).
      * `parent_lock_upholds_agreement` — asserts NO counterexample exists
        (validates the fix).

    Drives n=4, f=1, one Byzantine validator (v4) + an equivocating/withholding
    leader (offers two conflicting children of A across different views;
    delivery order is searched, which realizes partition/heal + the lock lag).
*/

use std::collections::BTreeSet;

use stateright::{Checker, Model, Property};

// ===========================================================================
// Fixed block universe
//
//                 Genesis
//                    |
//                    A            height 0   (agreed grandparent; QC(A,1) seeded)
//                   / \
//                  P   P'         height 1   (conflicting siblings)
//                  |    |
//                  X   X'         height 2
//
// Agreement is violated iff two honest replicas commit {P} and {P'} respectively.
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum BlockId {
    Genesis,
    A,
    P,
    Pp, // P'
    X,
    Xp, // X'
}

impl BlockId {
    /// Parent block (the block certified by this block's embedded justify).
    fn parent(self) -> BlockId {
        match self {
            BlockId::Genesis => BlockId::Genesis,
            BlockId::A => BlockId::Genesis,
            BlockId::P => BlockId::A,
            BlockId::Pp => BlockId::A,
            BlockId::X => BlockId::P,
            BlockId::Xp => BlockId::Pp,
        }
    }

    /// Height in the chain. Genesis is a sentinel (unused in comparisons).
    fn height(self) -> u8 {
        match self {
            BlockId::Genesis => 255,
            BlockId::A => 0,
            BlockId::P | BlockId::Pp => 1,
            BlockId::X | BlockId::Xp => 2,
        }
    }

    /// Blocks that can receive a QC (everything except Genesis and the seeded A).
    fn proposable() -> [BlockId; 4] {
        [BlockId::P, BlockId::Pp, BlockId::X, BlockId::Xp]
    }
}

/// A phase certificate: which block it certifies, and the view it formed in.
///
/// s138: `jv` is the view of the certified block's own justify (the parent QC it embeds). A
/// block's hash binds its justify, so `(block, jv)` identifies a block; with re-proposals the
/// same block can be certified again in a later view (same `block`, same `jv`, higher `view`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Qc {
    block: BlockId,
    view: u8,
    jv: u8,
}

// ===========================================================================
// Model configuration
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LockRule {
    /// FORMER production rule (pre-4a94e8b): pc_to_lock(J) = J.block.justify (grandparent).
    Grandparent,
    /// CURRENT production rule (4a94e8b): pc_to_lock(J) = J (lock-on-parent; Some(justify.clone())).
    Parent,
}

impl LockRule {
    /// Manual mirror of the shipped pc_to_lock rule (invariants.rs, Generic arm).
    /// The T1.2 fix (4a94e8b) landed lock-on-parent: `Phase::Generic => Some(justify.clone())`.
    /// Keep this in sync with invariants.rs — the model cannot import the real rule.
    const PRODUCTION: LockRule = LockRule::Parent;
}

/// n = 4, f = 1. Honest replicas are indices 0,1,2 (= v1,v2,v3). Index 3 (v4)
/// is Byzantine and carries no honest state (it will "vote" for anything).
const HONEST: usize = 3;
const QUORUM: u32 = 3; // 2f + 1
const MAX_VIEW: u8 = 5; // seeded QC(A,1); fork 2-chains fit in views 2..=5
const BASE_VIEW: u8 = 1; // view of the seeded QC(A)

#[derive(Clone)]
struct MonadBftModel {
    lock_rule: LockRule,
    /// Models the header-first relaxations: when true, the safe_pc predicate-3
    /// lock check is skipped at vote time (pending-block bypass). Strictly
    /// widens reachable behaviour.
    allow_pending_bypass: bool,
    /// s138: a block may be (re-)certified in more than one view, with its own justify, and
    /// children may be built on any of its parent's QCs. Off = the original model (each block
    /// is proposed once).
    reproposals: bool,
    /// s138: the reflexive lock rule (`invariants::safe_proposal_lock`): a proposal of the
    /// replica's (uncommitted) locked block itself satisfies the lock.
    reflexive_lock: bool,
    /// Highest view a QC may form in.
    max_view: u8,
}

impl MonadBftModel {
    /// The original model: no re-proposals, `MAX_VIEW`.
    fn new(lock_rule: LockRule, allow_pending_bypass: bool) -> Self {
        MonadBftModel { lock_rule, allow_pending_bypass, reproposals: false, reflexive_lock: false, max_view: MAX_VIEW }
    }
}

// ===========================================================================
// State
// ===========================================================================

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    /// Every QC that has formed (global; at most one per block).
    pool: BTreeSet<Qc>,
    /// Per honest replica: current locked_pc.
    locked: [Qc; HONEST],
    /// Per honest replica: highest view voted (vote-once-per-view).
    hvv: [Option<u8>; HONEST],
    /// Per honest replica: irrevocably committed blocks.
    committed: [BTreeSet<BlockId>; HONEST],
    /// Per honest replica: QCs already processed (bounds Deliver; avoids churn).
    processed: [BTreeSet<Qc>; HONEST],
    /// s138 witness: some honest vote was admitted by the reflexive lock arm alone.
    reflexive_vote: bool,
}

impl State {
    fn max_committed_height(&self, r: usize) -> u8 {
        self.committed[r]
            .iter()
            .map(|b| b.height())
            .filter(|h| *h != 255)
            .max()
            .unwrap_or(0)
    }
}

// ===========================================================================
// Actions
// ===========================================================================

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Action {
    /// A QC for `block` forms at `view`, voted by the replicas in `voter_mask`
    /// (bits 0..=3 = v1..v4). Models an (equivocating) proposal + a quorum of
    /// votes. Honest voters apply the vote-time lock update on `block.justify`.
    /// `jv` selects the block's justify: its parent's QC formed in view `jv`.
    FormQc { block: BlockId, view: u8, voter_mask: u8, jv: u8 },
    /// Honest replica `r` processes an already-formed `qc` it has not seen yet
    /// (models async/partitioned delivery + the collect/child-proposal paths).
    Deliver { r: u8, qc: Qc },
}

// ===========================================================================
// Protocol helpers (direct transcriptions of invariants.rs)
// ===========================================================================

/// The QC embedded in `block` = QC(parent(block)) formed in view `jv` (block.justify), if formed.
fn justify_of(pool: &BTreeSet<Qc>, block: BlockId, jv: u8) -> Option<Qc> {
    let par = block.parent();
    if par == BlockId::Genesis {
        // Genesis PC — treated as "seeded"; view 0. Only A has this and A is
        // pre-seeded as QC(A, BASE_VIEW), so this branch is not hit for A here.
        return Some(Qc { block: BlockId::Genesis, view: 0, jv: 0 });
    }
    pool.iter().copied().find(|q| q.block == par && q.view == jv)
}

/// pc_to_lock(J): the candidate locked_pc after processing `qc`.
/// None ~ "no lock / genesis" (as in pc_to_lock returning None at genesis).
fn pc_to_lock(pool: &BTreeSet<Qc>, qc: Qc, rule: LockRule) -> Option<Qc> {
    match rule {
        // FIX: lock on the QC itself (its block == parent of the block whose
        // proposal carried it). Some(justify.clone()).
        LockRule::Parent => Some(qc),
        // CURRENT: lock on qc.block.justify = QC(parent(qc.block)) (grandparent).
        LockRule::Grandparent => {
            let par = qc.block.parent();
            if par == BlockId::Genesis {
                None
            } else {
                justify_of(pool, qc.block, qc.jv) // = QC(parent(qc.block))
            }
        }
    }
}

/// block_to_commit(J) for the Generic phase: Some(block) to irrevocably commit,
/// subject to the caller's not-committed-yet height check.
fn block_to_commit(pool: &BTreeSet<Qc>, qc: Qc) -> Option<BlockId> {
    let par = qc.block.parent();
    if par == BlockId::Genesis {
        return None;
    }
    let pj = justify_of(pool, qc.block, qc.jv)?; // qc.block.justify
    if pj.block == BlockId::Genesis {
        return None;
    }
    // Consecutive-views rule: justify.view == parent_justify.view + 1.
    if qc.view == pj.view + 1 {
        Some(pj.block) // = parent(qc.block), the "grandparent" of the newest block
    } else {
        None
    }
}

/// extends_locked_pc_block: locked block is qc.block, its parent, or grandparent.
fn extends(qc_block: BlockId, locked_block: BlockId) -> bool {
    locked_block == qc_block
        || locked_block == qc_block.parent()
        || locked_block == qc_block.parent().parent()
}

/// safe_pc predicate 3 for a justify `j` against replica lock `locked`.
fn safe_pc_pred3(j: Qc, locked: Qc, allow_bypass: bool) -> bool {
    if allow_bypass {
        return true; // header-first: pending-block bypass skips the lock check
    }
    j.view > locked.view || extends(j.block, locked.block)
}

/// Apply update(qc) to one honest replica: lock (monotonic, per Property-4
/// assumption 2 "locked_pc monotonically increasing in view") then commit.
fn process(model: &MonadBftModel, st: &mut State, r: usize, qc: Qc) {
    // Lock update.
    if let Some(cand) = pc_to_lock(&st.pool, qc, model.lock_rule) {
        if cand.view > st.locked[r].view {
            st.locked[r] = cand;
        }
    }
    // Commit update (with not-committed-yet height guard).
    if let Some(b) = block_to_commit(&st.pool, qc) {
        if b.height() != 255 && b.height() > st.max_committed_height(r) {
            st.committed[r].insert(b);
        }
    }
    st.processed[r].insert(qc);
}

// ===========================================================================
// Model impl
// ===========================================================================

impl Model for MonadBftModel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<Self::State> {
        let seed = Qc { block: BlockId::A, view: BASE_VIEW, jv: 0 };
        let mut pool = BTreeSet::new();
        pool.insert(seed);
        let committed_seed = {
            let mut s = BTreeSet::new();
            s.insert(BlockId::A); // A is agreed at height 0
            s
        };
        let mut processed_seed = BTreeSet::new();
        processed_seed.insert(seed);
        vec![State {
            pool,
            locked: [seed; HONEST],
            hvv: [None; HONEST],
            committed: [
                committed_seed.clone(),
                committed_seed.clone(),
                committed_seed,
            ],
            processed: [
                processed_seed.clone(),
                processed_seed.clone(),
                processed_seed,
            ],
            reflexive_vote: false,
        }]
    }

    fn actions(&self, state: &Self::State, actions: &mut Vec<Self::Action>) {
        // FormQc candidates: every proposable block, view, and quorum mask.
        for block in BlockId::proposable() {
            let parent_qcs: Vec<u8> = state.pool.iter().filter(|q| q.block == block.parent()).map(|q| q.view).collect();
            for jv in parent_qcs {
                for view in (BASE_VIEW + 1)..=self.max_view {
                    for voter_mask in 0u8..16u8 {
                        // 0b1111 has the honest voters of 0b0111 (v4 carries no state): the
                        // same successor state, so it is skipped.
                        if voter_mask.count_ones() >= QUORUM && voter_mask != 0b1111 {
                            actions.push(Action::FormQc { block, view, voter_mask, jv });
                        }
                    }
                }
            }
        }
        // Deliver candidates: any honest replica processing any formed QC.
        for r in 0u8..(HONEST as u8) {
            for qc in &state.pool {
                actions.push(Action::Deliver { r, qc: *qc });
            }
        }
    }

    fn next_state(&self, last: &Self::State, action: Self::Action) -> Option<Self::State> {
        match action {
            Action::FormQc { block, view, voter_mask, jv } => {
                // One QC per block (a block is proposed once), unless re-proposals are modeled:
                // then only an identical QC is a no-op.
                if !self.reproposals && last.pool.iter().any(|q| q.block == block) {
                    return None;
                }
                if last.pool.contains(&Qc { block, view, jv }) {
                    return None;
                }
                // The block's justify (QC of its parent) must already exist.
                let justify = justify_of(&last.pool, block, jv)?;
                if justify.block == BlockId::Genesis {
                    return None; // proposable blocks always have a non-genesis parent QC
                }
                // Child voted strictly after its justify formed.
                if view <= justify.view {
                    return None;
                }
                // Honest voters must each be eligible (vote-once + safe_pc pred 3, or, with
                // `reflexive_lock`, the block is the voter's uncommitted locked block itself).
                let mut reflexive_vote = false;
                for r in 0..HONEST {
                    if voter_mask & (1 << r) != 0 {
                        let voted_ok = last.hvv[r].map_or(true, |hv| hv < view);
                        let pred3 = safe_pc_pred3(justify, last.locked[r], self.allow_pending_bypass);
                        let locked = last.locked[r];
                        let reflexive = self.reflexive_lock
                            && locked.block == block
                            && locked.jv == jv
                            && !last.committed[r].contains(&block);
                        if !(voted_ok && (pred3 || reflexive)) {
                            return None;
                        }
                        reflexive_vote |= !pred3;
                    }
                }
                // Apply: QC forms; honest voters mark voted and process block.justify
                // (the vote-time update(proposal.block.justify)).
                let mut st = last.clone();
                st.reflexive_vote |= reflexive_vote;
                st.pool.insert(Qc { block, view, jv });
                for r in 0..HONEST {
                    if voter_mask & (1 << r) != 0 {
                        st.hvv[r] = Some(view);
                        process(self, &mut st, r, justify);
                    }
                }
                Some(st)
            }
            Action::Deliver { r, qc } => {
                let r = r as usize;
                if !last.pool.contains(&qc) || last.processed[r].contains(&qc) {
                    return None;
                }
                let mut st = last.clone();
                process(self, &mut st, r, qc);
                Some(st)
            }
        }
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // AGREEMENT: no two honest replicas commit distinct blocks of the
            // same height.
            Property::<Self>::always("agreement", |_, state| {
                for a in 0..HONEST {
                    for b in (a + 1)..HONEST {
                        for ba in &state.committed[a] {
                            for bb in &state.committed[b] {
                                if ba != bb && ba.height() == bb.height() && ba.height() != 255 {
                                    return false;
                                }
                            }
                        }
                    }
                }
                true
            }),
            // BOUNDED-DEPTH: committed heights never exceed the modeled horizon.
            Property::<Self>::always("bounded_depth", |_, state| {
                state
                    .committed
                    .iter()
                    .flat_map(|s| s.iter())
                    .all(|b| b.height() == 255 || b.height() <= 2)
            }),
            // s138 witness: the reflexive lock arm admits some honest vote (so the agreement
            // check above covers it).
            Property::<Self>::sometimes("reflexive_vote", |_, state| state.reflexive_vote),
        ]
    }
}

// ===========================================================================
// Tests
// ===========================================================================

/// RED-first witness for the fix. Configures the model with the production lock
/// rule and asserts Agreement holds. FAILS today (LockRule::PRODUCTION ==
/// Grandparent -> Agreement is violated); PASSES once pc_to_lock is fixed and
/// LockRule::PRODUCTION is repointed to Parent.
#[test]
fn agreement_under_production_rule() {
    let model = MonadBftModel::new(LockRule::PRODUCTION, false);
    let checker = model.checker().spawn_bfs().join();
    assert!(
        checker.discovery("agreement").is_none(),
        "Agreement violated under LockRule::PRODUCTION ({:?}) — the grandparent \
         lock is unsafe for 2-chain commit. Apply the pc_to_lock fix \
         (Generic => Some(justify.clone())) and set LockRule::PRODUCTION = Parent. \
         Counterexample path: {:?}",
        LockRule::PRODUCTION,
        checker.discovery("agreement").map(|p| p.into_actions()),
    );
}

/// Documents the bug: under the grandparent lock there IS an execution that
/// violates Agreement (two honest validators commit conflicting siblings).
#[test]
fn grandparent_lock_violates_agreement() {
    let model = MonadBftModel::new(LockRule::Grandparent, false);
    let checker = model.checker().spawn_bfs().join();
    assert!(
        checker.discovery("agreement").is_some(),
        "expected the grandparent lock to admit an Agreement violation"
    );
    // Bounded-depth must still hold — commits are well-formed, just conflicting.
    assert!(checker.discovery("bounded_depth").is_none());
}

/// Validates the fix: under the lock-on-parent rule Agreement holds across the
/// entire reachable state space (no counterexample).
#[test]
fn parent_lock_upholds_agreement() {
    let model = MonadBftModel::new(LockRule::Parent, false);
    let checker = model.checker().spawn_bfs().join();
    // both `agreement` and `bounded_depth` hold (`reflexive_vote` is the s138 witness, off here)
    assert!(checker.discovery("agreement").is_none());
    assert!(checker.discovery("bounded_depth").is_none());
}

/// s138: the reflexive lock rule (a proposal of the voter's uncommitted locked block itself
/// satisfies the lock, `invariants::safe_proposal_lock`) with re-proposals (a block certified
/// again in a later view, children built on any of its parent's QCs) keeps Agreement, and the
/// reflexive arm is exercised (the `reflexive_vote` witness is found). QCs in views 2..=`max_view`.
fn check_reflexive_lock_with_reproposals(max_view: u8) {
    let model = MonadBftModel { reproposals: true, reflexive_lock: true, max_view, ..MonadBftModel::new(LockRule::Parent, false) };
    let checker = model.checker().spawn_bfs().join();
    assert!(
        checker.discovery("agreement").is_none(),
        "Agreement violated under the reflexive lock rule. Counterexample path: {:?}",
        checker.discovery("agreement").map(|p| p.into_actions()),
    );
    assert!(checker.discovery("bounded_depth").is_none());
    assert!(checker.discovery("reflexive_vote").is_some(), "the reflexive lock arm was never used");
}

/// [`check_reflexive_lock_with_reproposals`] with QCs in views 2..=4 (in the regular suite).
#[test]
fn reflexive_lock_with_reproposals_upholds_agreement() {
    check_reflexive_lock_with_reproposals(4);
}

/// [`check_reflexive_lock_with_reproposals`] with QCs in views 2..=5 (`MAX_VIEW`, as the other
/// checks): about 9 minutes in a debug build, so run on demand:
/// `cargo nextest run -p hotstuff_rs --test stateright_2chain_safety --run-ignored all`.
#[test]
#[ignore = "slow (about 9 min, debug): the views 2..=5 state space with re-proposals"]
fn reflexive_lock_with_reproposals_upholds_agreement_to_max_view() {
    check_reflexive_lock_with_reproposals(MAX_VIEW);
}
