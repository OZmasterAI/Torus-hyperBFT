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

    SIGNED TIPS AND V1-V4 (`tips`, s138, Codex torus-adversarial P1 on 8af2b949)
      With `tips` the model adds the TC-required rules (crates/hotstuff_rs/src/hotstuff/types.rs
      `valid_proposal_certificates`) and signed tips (pacemaker/types.rs `TimeoutCertificate`):
      * a proposal of U or U' (justify qa, view 1) in view v > 2 needs TC(v-1) (V1); if the TC's
        high tip wins (tip view > high QC view) the block is that tip (V2), else it is the high QC's
        block (V3; any block when the high QC is qa);
      * TC(w) forms from three timeout votes; an honest one reports its tip and its highest PC
        (`highest_qc_for_timeout`: none when its tip is newer), and leaves view w; the Byzantine
        one reports any PC formed so far and any tip it can sign: a header of its own view
        (`byz_views`, U or U') or any header a leader signed (`proposed`);
      * an honest Prepare vote on a TC re-proposal (high tip wins) copies the TC's high tip when
        `copy_tips` (the code before the fix); a proposal vote without a PC (the Byzantine one
        withholds) is an action of its own;
      * the synchronous suffix forms TC(v-1) from the honest replicas whenever view v-1 formed no
        PC; its leader re-proposes the winning tip (case 4), else acts on max(its highest PC, the
        TC's high QC), which every honest replica also learns;
      * LockRule::Unlock (the fix): a proposal of B also satisfies the lock when a PC on B newer
        than the lock is known: the TC's high QC or the replica's highest PC (any formed PC on B
        without `tips`, the strongest form for Agreement).
      RESULTS: Reflexive strands a certified sibling (the Codex trace: Prepare(U', 4) highest,
      replica locked on Precommit(U, 3)); Unlock with `copy_tips` strands it when the Byzantine one
      leads view 4 (a copied tip U' at view 4 beats every honest QC, the locked replica refuses
      U'); Unlock without VSU tip copies decides; Agreement holds under Unlock in every mode.
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

/// A signed proposal header of `block` in `view` (`TipInfo`). Ordered by view first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Tip {
    view: u8,
    block: BlockId,
}

/// A timeout certificate's summary: its view, high QC (`None`: qa or lower) and high tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Tc {
    view: u8,
    high_qc: Option<Pc>,
    high_tip: Option<Tip>,
}

impl Tc {
    fn qc_view(&self) -> u8 {
        self.high_qc.map_or(QA_VIEW, |pc| pc.view)
    }

    /// `high_tip_is_winner`: the tip is newer than the high QC.
    fn tip_wins(&self) -> bool {
        self.high_tip.is_some_and(|tip| tip.view > self.qc_view())
    }

    /// V2 / V3 for a proposal of `block` justified by qa (view 1).
    fn admits(&self, block: BlockId) -> bool {
        match (self.tip_wins(), self.high_qc) {
            (true, _) => self.high_tip.is_some_and(|tip| tip.block == block),
            // V3: qa.view >= q.view, or the block is q's.
            (false, None) => true,
            (false, Some(q)) => q.block == block,
        }
    }

    /// The TC that the timeout votes `reports` (tip, highest QC) form for `view`.
    fn form(view: u8, reports: impl IntoIterator<Item = (Option<Tip>, Option<Pc>)>) -> Tc {
        let (mut high_tip, mut high_qc) = (None::<Tip>, None::<Pc>);
        for (tip, qc) in reports {
            high_tip = high_tip.max(tip);
            if qc.is_some_and(|qc| high_qc.is_none_or(|h| qc.view > h.view)) {
                high_qc = qc;
            }
        }
        Tc { view, high_qc, high_tip }
    }
}

/// The lock rule for a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LockRule {
    /// The lock clause on the justify only (before 93948e2b).
    Original,
    /// 93948e2b: or the block is the locked block and it is not committed.
    ReflexiveUncommitted,
    /// 9ddec7fb: or the block is the locked block.
    Reflexive,
    /// s138 sibling fix: Reflexive, or a PC on the block newer than the lock is known.
    Unlock,
}

/// Why a replica may vote for a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProposalVote {
    /// The justify satisfies the lock clause, or the uncommitted locked block.
    Plain,
    /// Only as the replica's COMMITTED locked block.
    CommittedReflexive,
    /// Only through a PC on the block newer than the lock (LockRule::Unlock).
    Witness,
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
    /// highest_view_voted (0 = none); with `tips` also the highest view this replica timed out.
    hvv: u8,
    /// The local tip (`tips` only): none before the window (older than every view modeled).
    tip: Option<Tip>,
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

    /// What this replica's timeout vote reports (`local_tip`, `highest_qc_for_timeout`).
    fn timeout_report(&self) -> (Option<Tip>, Option<Pc>) {
        let qc = if self.tip.is_some_and(|tip| tip.view > self.highest_view()) { None } else { self.highest };
        (self.tip, qc)
    }

    /// May vote Prepare for a proposal of `block` (justify qa) in `view`; `known` are the PCs the
    /// Unlock rule may use (the TC's high QC; any formed PC without `tips`), besides the
    /// replica's own highest PC.
    fn proposal_vote(&self, block: BlockId, view: u8, rule: LockRule, known: &[Pc]) -> Option<ProposalVote> {
        if self.hvv >= view {
            return None;
        }
        // safe_pc_lock_clause(qa): qa.view > lock.view, or A (qa.block) extends the locked block.
        if QA_VIEW > self.lock_view() || self.locked.is_none() {
            return Some(ProposalVote::Plain);
        }
        let is_locked = self.locked.is_some_and(|l| l.block == block);
        let committed = self.committed == Some(block);
        let reflexive = if committed { ProposalVote::CommittedReflexive } else { ProposalVote::Plain };
        let witness = || {
            known
                .iter()
                .chain(self.highest.as_ref())
                .any(|pc| pc.block == block && pc.view > self.lock_view())
        };
        match rule {
            LockRule::Original => None,
            LockRule::ReflexiveUncommitted => (is_locked && !committed).then_some(ProposalVote::Plain),
            LockRule::Reflexive => is_locked.then_some(reflexive),
            LockRule::Unlock if is_locked => Some(reflexive),
            LockRule::Unlock => witness().then_some(ProposalVote::Witness),
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
    /// Signed tips, TCs and V1-V4.
    tips: bool,
    /// A Prepare vote on a TC re-proposal copies the TC's high tip (the code before the fix).
    copy_tips: bool,
    /// The views the Byzantine validator leads (signs fresh headers for).
    byz_views: &'static [u8],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    pool: BTreeSet<Pc>,
    replicas: [Replica; HONEST],
    committed_reflexive_vote: bool,
    witness_vote: bool,
    /// `tips`: the headers leaders signed so far (proposals made).
    proposed: BTreeSet<Tip>,
}

/// `tips`: how the TC(v-1) a proposal in view v carries forms: from the timeout votes of `mask`
/// (bits 0..=2 honest, bit 3 Byzantine), the Byzantine one reporting `byz_tip` and `byz_qc`. It
/// forms when the proposal is made, from the replicas' state then (the honest signers leave
/// view v-1); a TC formed earlier is the same TC with deliveries delayed past it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TcChoice {
    mask: u8,
    byz_tip: Option<Tip>,
    byz_qc: Option<Pc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Action {
    /// A PC forms in `view` with the voters of `mask` (bits 0..=2 honest, bit 3 Byzantine). A
    /// Prepare PC is the votes on a proposal of `block` (carrying `tc`); any other phase the votes
    /// on a nudge of the formed PC of the previous phase (`justify_view`).
    FormPc { block: BlockId, phase: Phase, view: u8, justify_view: u8, mask: u8, tc: Option<TcChoice> },
    /// `tips`, `copy_tips`: honest replica `r` votes Prepare for a TC re-proposal of `block` in
    /// `view` (the TC's high tip wins), and no PC forms (the Byzantine one withholds). Its effect
    /// is the copied tip. (Without a winning tip a lone vote only raises highest_view_voted.)
    VoteProposal { r: usize, block: BlockId, view: u8, tc: TcChoice },
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

    /// The TC(`view`) that `choice` forms in `state`, or `None` if an honest signer has already
    /// left `view`.
    fn form_tc(&self, state: &State, view: u8, choice: TcChoice) -> Option<Tc> {
        let mut reports = Vec::new();
        for (r, replica) in state.replicas.iter().enumerate() {
            if choice.mask & (1 << r) != 0 {
                if replica.hvv > view {
                    return None;
                }
                reports.push(replica.timeout_report());
            }
        }
        if choice.mask & 0b1000 != 0 {
            reports.push((choice.byz_tip, choice.byz_qc));
        }
        Some(Tc::form(view, reports))
    }

    /// The TC choices for a proposal in `view` (of a block justified by qa): none in view 2 (qa
    /// is QC(v-1)) or without `tips`; else a TC(v-1) (V1) admitting `block` (V2, V3).
    fn proposal_tcs(&self, state: &State, block: BlockId, view: u8) -> Vec<Option<TcChoice>> {
        if !self.tips || view == QA_VIEW + 1 {
            return vec![None];
        }
        let mut choices = Vec::new();
        for mask in MASKS {
            if mask & 0b1000 == 0 {
                choices.push(TcChoice { mask, byz_tip: None, byz_qc: None });
                continue;
            }
            for byz_tip in std::iter::once(None).chain(self.byz_tips(state, view - 1).into_iter().map(Some)) {
                for byz_qc in std::iter::once(None).chain(state.pool.iter().copied().map(Some)) {
                    choices.push(TcChoice { mask, byz_tip, byz_qc });
                }
            }
        }
        let mut seen = BTreeSet::new();
        choices
            .into_iter()
            .filter(|&choice| {
                self.form_tc(state, view - 1, choice)
                    .is_some_and(|tc| tc.admits(block) && seen.insert((choice.mask, tc)))
            })
            .map(Some)
            .collect()
    }

    /// The PCs the Unlock rule may use for a proposal carrying `tc`: its high QC; without `tips`
    /// any formed PC (a leader may attach any PC in some certificate: the strongest form).
    fn known_pcs(&self, state: &State, tc: Option<Tc>) -> Vec<Pc> {
        if self.tips {
            tc.and_then(|tc| tc.high_qc).into_iter().collect()
        } else {
            state.pool.iter().copied().collect()
        }
    }

    /// The honest TC signers of `choice` leave view `view - 1`; the TC their reports form.
    fn apply_tc(&self, st: &mut State, view: u8, choice: Option<TcChoice>) -> Option<Option<Tc>> {
        let Some(choice) = choice else { return Some(None) };
        let tc = self.form_tc(st, view - 1, choice)?;
        for r in 0..HONEST {
            if choice.mask & (1 << r) != 0 {
                st.replicas[r].hvv = st.replicas[r].hvv.max(view - 1);
            }
        }
        Some(Some(tc))
    }

    /// Honest replica `replica`'s Prepare vote for `block` in `view` (carrying `tc`), or `None`.
    fn cast_proposal_vote(
        &self,
        st: &mut State,
        known: &[Pc],
        r: usize,
        block: BlockId,
        view: u8,
        tc: Option<Tc>,
    ) -> Option<()> {
        let vote = st.replicas[r].proposal_vote(block, view, self.rule, known)?;
        st.committed_reflexive_vote |= vote == ProposalVote::CommittedReflexive;
        st.witness_vote |= vote == ProposalVote::Witness;
        let replica = &mut st.replicas[r];
        replica.hvv = view;
        if self.copy_tips {
            if let Some(tc) = tc.filter(Tc::tip_wins) {
                replica.tip = tc.high_tip;
            }
        }
        Some(())
    }

    /// The tips the Byzantine validator can report in a timeout vote for `view`: its own headers
    /// and any header a leader signed, no newer than `view` (TC `summaries_are_signed`).
    fn byz_tips(&self, state: &State, view: u8) -> BTreeSet<Tip> {
        let own = self
            .byz_views
            .iter()
            .flat_map(|&v| self.blocks().iter().map(move |&block| Tip { view: v, block }));
        own.chain(state.proposed.iter().copied()).filter(|tip| tip.view <= view).collect()
    }

    /// The synchronous suffix from `state`, starting `offset` views after the last used view.
    fn sync_suffix_decides(&self, state: &State, offset: u8) -> bool {
        if self.tips {
            return self.sync_suffix_decides_with_tips(state, offset);
        }
        let mut replicas = state.replicas.clone();
        let last = self.last_view(state);
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
                    let known: Vec<Pc> = state.pool.iter().copied().collect();
                    let voters: Vec<usize> = (0..HONEST)
                        .filter(|&r| replicas[r].proposal_vote(block, view, self.rule, &known).is_some())
                        .collect();
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

    fn last_view(&self, state: &State) -> u8 {
        state
            .pool
            .iter()
            .map(|pc| pc.view)
            .chain(state.replicas.iter().map(|r| r.hvv))
            .max()
            .unwrap_or(QA_VIEW)
    }

    /// [`Self::sync_suffix_decides`] with signed tips and V1-V4: a view whose predecessor formed
    /// no PC starts from the honest replicas' TC; the Byzantine validator is silent.
    fn sync_suffix_decides_with_tips(&self, state: &State, offset: u8) -> bool {
        let mut replicas = state.replicas.clone();
        let mut previous: Option<Pc> = None;
        for view in (self.last_view(state) + offset..).take(SUFFIX_VIEWS as usize) {
            if replicas.iter().all(|r| r.decided) {
                return true;
            }
            let tc = match previous {
                Some(pc) if pc.view + 1 == view => None,
                _ => Some(Tc::form(view - 1, replicas.iter().map(Replica::timeout_report))),
            };
            // Everyone learns the TC's high QC; the leader also has its own highest PC.
            if let Some(q) = tc.and_then(|tc| tc.high_qc) {
                replicas.iter_mut().for_each(|r| r.advance_highest(q));
            }
            let leader = &replicas[view as usize % HONEST];
            let high = leader.highest;
            let proposal = match (tc.filter(Tc::tip_wins), high) {
                // Case 4: re-propose the winning tip.
                (Some(tc), _) => Some(tc.high_tip.unwrap().block),
                (None, Some(pc)) if pc.phase == Phase::Decide => {
                    replicas.iter_mut().for_each(|r| r.update(pc));
                    previous = None;
                    continue;
                }
                (None, Some(pc)) if pc.phase == Phase::Commit || view == pc.view + 1 => None,
                // Re-propose the highest PC's block, or propose U fresh.
                (None, high) => Some(high.map_or(BlockId::U, |pc| pc.block)),
            };
            let formed = match proposal {
                Some(block) => {
                    let known: Vec<Pc> = tc.and_then(|tc| tc.high_qc).into_iter().collect();
                    let mut voters = 0;
                    for replica in replicas.iter_mut() {
                        if replica.proposal_vote(block, view, self.rule, &known).is_some() {
                            replica.hvv = view;
                            if self.copy_tips && tc.is_some_and(|tc| tc.tip_wins()) {
                                replica.tip = tc.unwrap().high_tip;
                            }
                            voters += 1;
                        }
                    }
                    (voters == HONEST).then_some(Pc { block, phase: Phase::Prepare, view })
                }
                None => {
                    let pc = high.unwrap();
                    let mut voters = 0;
                    for replica in replicas.iter_mut() {
                        if replica.nudge_vote(pc, view) {
                            replica.update(pc);
                            replica.hvv = view;
                            voters += 1;
                        }
                    }
                    (voters == HONEST).then(|| Pc { block: pc.block, phase: pc.phase.next().unwrap(), view })
                }
            };
            if let Some(pc) = formed {
                replicas.iter_mut().for_each(|r| r.update(pc));
            }
            previous = formed;
        }
        replicas.iter().all(|r| r.decided)
    }
}

impl Model for VsuModel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let replica = Replica { highest: None, locked: None, committed: None, decided: false, hvv: 0, tip: None };
        vec![State {
            pool: BTreeSet::new(),
            replicas: [replica.clone(), replica.clone(), replica],
            committed_reflexive_vote: false,
            witness_vote: false,
            proposed: BTreeSet::new(),
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for &block in self.blocks() {
            for view in 2..=self.max_view {
                for tc in self.proposal_tcs(state, block, view) {
                    for mask in MASKS {
                        actions.push(Action::FormPc { block, phase: Phase::Prepare, view, justify_view: QA_VIEW, mask, tc });
                    }
                    let Some(tc) = tc else { continue };
                    if self.copy_tips && self.form_tc(state, view - 1, tc).is_some_and(|tc| tc.tip_wins()) {
                        for r in 0..HONEST {
                            actions.push(Action::VoteProposal { r, block, view, tc });
                        }
                    }
                }
            }
        }
        for justify in &state.pool {
            let Some(phase) = justify.phase.next() else { continue };
            for view in (justify.view + 1)..=self.max_view {
                for mask in MASKS {
                    actions.push(Action::FormPc { block: justify.block, phase, view, justify_view: justify.view, mask, tc: None });
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
            Action::FormPc { block, phase, view, justify_view, mask, tc } => {
                let pc = Pc { block, phase, view };
                if last.pool.contains(&pc) {
                    return None;
                }
                let mut st = last.clone();
                let justify = (phase != Phase::Prepare).then(|| {
                    let prev = [Phase::Prepare, Phase::Precommit, Phase::Commit][phase as usize - 1];
                    Pc { block, phase: prev, view: justify_view }
                });
                let tc = self.apply_tc(&mut st, view, tc)?;
                let known = self.known_pcs(last, tc);
                for r in 0..HONEST {
                    if mask & (1 << r) == 0 {
                        continue;
                    }
                    match justify {
                        None => self.cast_proposal_vote(&mut st, &known, r, block, view, tc)?,
                        Some(justify) => {
                            let replica = &mut st.replicas[r];
                            if !replica.nudge_vote(justify, view) {
                                return None;
                            }
                            replica.update(justify);
                            replica.hvv = view;
                        }
                    }
                }
                st.pool.insert(pc);
                if self.tips && phase == Phase::Prepare {
                    st.proposed.insert(Tip { view, block });
                }
                Some(st)
            }
            Action::VoteProposal { r, block, view, tc } => {
                let mut st = last.clone();
                let tc = self.apply_tc(&mut st, view, Some(tc))?;
                let known = self.known_pcs(last, tc);
                self.cast_proposal_vote(&mut st, &known, r, block, view, tc)?;
                st.proposed.insert(Tip { view, block });
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
            Property::<Self>::sometimes("witness_vote", |_, state: &State| state.witness_vote),
        ]
    }
}

fn check(rule: LockRule, sibling: bool, max_view: u8) -> impl Checker<VsuModel> {
    VsuModel { rule, sibling, max_view, tips: false, copy_tips: false, byz_views: &[] }.checker().spawn_bfs().join()
}

fn check_tips(rule: LockRule, copy_tips: bool, byz_views: &'static [u8], max_view: u8) -> impl Checker<VsuModel> {
    VsuModel { rule, sibling: true, max_view, tips: true, copy_tips, byz_views }.checker().spawn_bfs().join()
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
    for rule in [LockRule::Reflexive, LockRule::Unlock] {
        let checker = check(rule, false, 5);
        assert!(
            checker.discovery("sync_suffix_decides").is_none(),
            "the update is stranded under {rule:?}: {:?}",
            checker.discovery("sync_suffix_decides").map(|p| p.into_actions())
        );
        assert!(checker.discovery("agreement").is_none());
        assert!(checker.discovery("committed_reflexive_vote").is_some(), "no replica voted for its committed locked block");
    }
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

/// s138 sibling fix, safety without V1-V4: a replica locked on U votes for U' whenever ANY formed
/// PC on U' is newer than its lock (the strongest form of the Unlock rule; the code only uses the
/// TC's high QC and its own highest PC), and still no two honest replicas commit different blocks.
#[test]
fn unlock_rule_keeps_agreement_with_a_conflicting_sibling() {
    let checker = check(LockRule::Unlock, true, 5);
    assert!(
        checker.discovery("agreement").is_none(),
        "Agreement violated: {:?}",
        checker.discovery("agreement").map(|p| p.into_actions())
    );
    assert!(checker.discovery("witness_vote").is_some(), "the Unlock arm is never used");
    assert!(checker.discovery("committed_reflexive_vote").is_some());
}

/// Codex torus-adversarial P1 on 8af2b949, with signed tips and V1-V4: under the reflexive rule a
/// replica locked on U refuses U' certified above its lock (a Byzantine tip of U' won TC(3), the
/// unlocked replicas and the Byzantine one certified Prepare(U', 4)), and every later TC selects
/// that PC, so leaders re-propose U' forever. No tip copy is needed for it.
#[test]
fn reflexive_rule_strands_a_certified_sibling() {
    let checker = check_tips(LockRule::Reflexive, false, &[3], 5);
    let path = checker.discovery("sync_suffix_decides").expect("the certified sibling strands the update");
    let last = path.last_state();
    assert!(
        last.replicas.iter().any(|r| r.locked.is_some_and(|l| r.highest.is_some_and(|h| h.block != l.block && h.view > l.view))),
        "the counterexample has a replica locked on one block whose highest PC certifies the other above the lock: {:?}",
        path.into_actions()
    );
    assert!(checker.discovery("agreement").is_none());
}

/// The fix (the certified arm, and no tip copied by a vote on a validator-set-updating block):
/// from every reachable state (Byzantine tips of U or U' at view 3 or 4, sibling certificates,
/// lost nudges and PCs, TCs of any three timeout votes, views 2..=5) the synchronous suffix
/// decides, Agreement holds, and the certified arm is exercised.
#[test]
fn unlock_rule_decides_with_a_certified_sibling() {
    const VIEW_3: &[u8] = &[3];
    const VIEW_4: &[u8] = &[4];
    for byz_views in [VIEW_3, VIEW_4] {
        let checker = check_tips(LockRule::Unlock, false, byz_views, 5);
        assert!(
            checker.discovery("sync_suffix_decides").is_none(),
            "the update is stranded (Byzantine views {byz_views:?}): {:?}",
            checker.discovery("sync_suffix_decides").map(|p| p.into_actions())
        );
        assert!(checker.discovery("agreement").is_none());
        if byz_views == VIEW_3 {
            assert!(checker.discovery("witness_vote").is_some(), "the certified arm is never used");
        }
    }
}

/// Found by this model, independent of the lock rule: when a Prepare vote on a TC re-proposal
/// copies the TC's high tip (the code before the fix), a Byzantine tip of one block at view 4
/// (the Byzantine validator leads view 4) becomes an honest replica's tip; it beats every honest
/// QC (at most a Precommit PC at view 3) in the honest TCs, so leaders re-propose that block (V2),
/// which replicas locked on the other block refuse forever (no PC certifies it). Bounded search:
/// the counterexample is four actions deep.
#[test]
fn copied_validator_set_update_tips_strand_the_update() {
    let checker = VsuModel { rule: LockRule::Unlock, sibling: true, max_view: 5, tips: true, copy_tips: true, byz_views: &[4] }
        .checker()
        .target_max_depth(6)
        .spawn_bfs()
        .join();
    let path = checker.discovery("sync_suffix_decides").expect("a copied Byzantine tip strands the update");
    let last = path.last_state();
    assert!(
        last.replicas.iter().any(|r| r.tip.is_some())
            && last.replicas.iter().any(|r| r.locked.is_some_and(|l| last.replicas.iter().any(|o| o.tip.is_some_and(|t| t.block != l.block)))),
        "the counterexample has an honest copied tip of a block other replicas are locked against: {:?}",
        path.into_actions()
    );
}
