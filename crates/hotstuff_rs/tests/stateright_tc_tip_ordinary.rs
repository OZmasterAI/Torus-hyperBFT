/*
    s138 (Codex review of fix/vsu-sibling-unlock 138a9219, P1): Stateright model of ORDINARY
    (Generic, pipelined, 2-chain) blocks under TC-required (V1-V4), signed tips and the HotStuff
    lock, with a Byzantine validator that signs a tip of a sibling block.

    WHAT IT ENCODES (crates/hotstuff_rs/src/{block_tree/invariants.rs, hotstuff/types.rs,
    pacemaker/types.rs, hotstuff/implementation.rs})
      * Blocks: A (agreed; QC(A, 1) = `qa`, every replica's lock and highest PC at the start),
        the siblings U and V on A (justify qa), and fresh blocks F(j, p) an honest leader of view p
        proposes on the QC of view j (`propose_from_highest_pc`). A QC is unique per view.
      * update(qc) (`BlockTreeSingleton::update`): highest PC by view; lock on qc if newer
        (`pc_to_lock`, Generic: the justify itself, strictly greater view); 2-chain commit of the
        certified block's parent when qc.view == its justify's view + 1 (`block_to_commit`).
      * Proposal lock rule (`safe_proposal_lock`): the block is the locked block (reflexive), or
        its justify is newer than the lock, or the justify's block, parent or grandparent is the
        locked block (`extends_locked_pc_block`); with `unlock` (fix/vsu-sibling-unlock) also a PC
        on the block newer than the lock (the TC's high QC or the replica's highest PC).
      * V1-V4 (`valid_proposal_certificates`): a proposal in view v justifies on view v-1, or
        carries TC(v-1); with the TC's tip winning the block is that tip (V2), else it justifies on
        a view >= the high QC's (V3). NECs are not modeled (every leader holds U and V).
      * TC winner (`TimeoutVoteCollector`, `summaries_are_signed`): `Winner::ByView` (main before
        the fix: tip.view > high_qc.view) or `Winner::Compatible` (the fix: also the tip's block
        justifies on a view >= the high QC's).
      * Timeout summaries (`highest_qc_for_timeout`): `Report::OmitIfTipNewer` (before the fix) or
        `Report::Always` (the fix: the highest PC is always reported).
      * Tips: a fresh vote takes the proposal's header (block, view); a TC re-proposal vote copies
        the TC's high tip (`copy_tips`, production: F5).
      * One vote per view (`highest_view_voted`); a replica votes only in its current view and
        leaves a view by voting in a later one or by timing out (its timeout summary for the view
        is recorded then).
      * n = 4, f = 1: honest replicas 0, 1, 2; validator 3 is Byzantine. It leads the views
        congruent to `byz_view` mod 4, signs headers of U and V in the views it leads (tips it
        never has to send), proposes U or V there with any valid certificate, votes for anything,
        and reports any tip a leader signed and any formed QC (or none) in its timeout votes.

    ACTIONS (adversarial prefix, views 2..=max_view): timeouts, honest leader proposals (the leader
    rules L1/L2 over its own highest PC and any TC(v-1) formable from three timeout summaries),
    Byzantine proposals and signed headers, honest votes, QC formation (the Byzantine vote counts),
    delivery of any formed QC to any honest replica at any time (or never).

    PROPERTIES
      * `agreement` (always): every two honest replicas' committed blocks are on one chain.
      * `suffix_honest` (always): a synchronous suffix starting one or two views after the last
        used view (the Byzantine validator silent: no votes, no proposals, no timeout votes; every
        TC from the three honest timeout summaries) commits a block above A at every honest replica
        within SUFFIX_VIEWS views. Honest leaders follow L1/L2; every honest replica receives every
        proposal; a QC of three honest votes reaches every honest replica.
      * `suffix_byzantine_with_top_lock` (always): the same, but the Byzantine timeout vote (one
        fixed choice of tip and QC, over every choice) is in every TC, with two honest replicas
        that include one holding the highest lock.
      * `suffix_byzantine_any` (always): as above with any two honest replicas (the TC need not
        carry the highest lock).
*/

use std::collections::{BTreeMap, BTreeSet};

use stateright::{Checker, Model, Property};

const HONEST: usize = 3;
const QUORUM: usize = 3;
const SUFFIX_VIEWS: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum BlockId {
    A,
    U,
    V,
    /// A fresh block an honest leader proposed in view `.1` on the QC of view `.0`.
    F(u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Qc {
    view: u8,
    block: BlockId,
}

const QA: Qc = Qc { view: 1, block: BlockId::A };

/// A signed proposal header: `block` proposed in `view`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Tip {
    view: u8,
    block: BlockId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Tc {
    view: u8,
    tip: Option<Tip>,
    qc: Option<Qc>,
    tip_wins: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Cert {
    /// The block's justify is QC(v-1).
    Justify,
    Tc(Tc),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Proposal {
    view: u8,
    block: BlockId,
    cert: Cert,
    byzantine: bool,
}

/// A timeout vote's summaries.
type Summary = (Option<Tip>, Option<Qc>);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Replica {
    view: u8,
    voted: u8,
    lock: Qc,
    highest: Qc,
    tip: Option<Tip>,
    committed: Option<BlockId>,
    reports: BTreeMap<u8, Summary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    replicas: [Replica; HONEST],
    qcs: BTreeMap<u8, BlockId>,
    proposals: BTreeSet<Proposal>,
    /// Headers signed by leaders: every proposal, and the Byzantine leader's unsent headers.
    signed: BTreeSet<Tip>,
    /// (view, block) -> mask of honest voters.
    votes: BTreeMap<(u8, BlockId), u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Action {
    /// A proposal, and the honest signers of its TC (0 for `Cert::Justify`).
    Propose(Proposal, u8),
    Sign(Tip),
    Vote { r: usize, proposal: Proposal },
    Deliver { r: usize, view: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Winner {
    ByView,
    Compatible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    OmitIfTipNewer,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SuffixTcs {
    Honest,
    ByzantineWithTopLock,
    ByzantineAny,
}

#[derive(Clone)]
struct TipModel {
    winner: Winner,
    report: Report,
    copy_tips: bool,
    unlock: bool,
    max_view: u8,
    byz_view: u8,
    suffixes: &'static [SuffixTcs],
}

/// Block ancestry over the formed QCs.
fn parent(block: BlockId, qcs: &BTreeMap<u8, BlockId>) -> Option<BlockId> {
    match block {
        BlockId::A => None,
        BlockId::U | BlockId::V => Some(BlockId::A),
        BlockId::F(j, _) => qcs.get(&j).copied(),
    }
}

fn justify_view(block: BlockId) -> u8 {
    match block {
        BlockId::A => 0,
        BlockId::U | BlockId::V => 1,
        BlockId::F(j, _) => j,
    }
}

fn justify(block: BlockId, qcs: &BTreeMap<u8, BlockId>) -> Option<Qc> {
    match block {
        BlockId::A => None,
        BlockId::U | BlockId::V => Some(QA),
        BlockId::F(j, _) => qcs.get(&j).map(|&b| Qc { view: j, block: b }),
    }
}

fn height(block: BlockId, qcs: &BTreeMap<u8, BlockId>) -> u8 {
    let mut h = 0;
    let mut b = block;
    while let Some(p) = parent(b, qcs) {
        h += 1;
        b = p;
    }
    h
}

fn extends(block: BlockId, ancestor: BlockId, qcs: &BTreeMap<u8, BlockId>) -> bool {
    let mut b = Some(block);
    while let Some(x) = b {
        if x == ancestor {
            return true;
        }
        b = parent(x, qcs);
    }
    false
}

impl Replica {
    fn new() -> Replica {
        Replica { view: 2, voted: 1, lock: QA, highest: QA, tip: None, committed: None, reports: BTreeMap::new() }
    }

    fn update(&mut self, qc: Qc, qcs: &BTreeMap<u8, BlockId>) {
        if qc.view > self.highest.view {
            self.highest = qc;
        }
        if qc.view > self.lock.view {
            self.lock = qc;
        }
        if let Some(pj) = justify(qc.block, qcs) {
            if qc.view == pj.view + 1 {
                let commit = pj.block;
                if self.committed.is_none_or(|c| height(commit, qcs) > height(c, qcs)) {
                    self.committed = Some(commit);
                }
            }
        }
    }

    fn summary(&self, report: Report) -> Summary {
        let qc = match (report, self.tip) {
            (Report::OmitIfTipNewer, Some(tip)) if tip.view > self.highest.view => None,
            _ => Some(self.highest),
        };
        (self.tip, qc)
    }

    /// `safe_proposal_lock` for `block` (justify `j`), with `tc_qc` the attached TC's high QC.
    fn safe(&self, block: BlockId, j: Qc, tc_qc: Option<Qc>, unlock: bool, qcs: &BTreeMap<u8, BlockId>) -> bool {
        let lock = self.lock;
        let jp = parent(j.block, qcs);
        let jgp = jp.and_then(|p| parent(p, qcs));
        let extends_lock = j.block == lock.block || jp == Some(lock.block) || jgp == Some(lock.block);
        let reflexive = lock != QA && block == lock.block;
        let certified_above = |qc: Qc| qc.block == block && qc.view > lock.view;
        reflexive
            || j.view > lock.view
            || extends_lock
            || (unlock && (certified_above(self.highest) || tc_qc.is_some_and(certified_above)))
    }
}

impl TipModel {
    fn byzantine_leads(&self, view: u8) -> bool {
        view % 4 == self.byz_view % 4
    }

    /// The honest replica leading `view`, if honest.
    fn honest_leader(&self, view: u8) -> Option<usize> {
        if self.byzantine_leads(view) {
            return None;
        }
        // The three honest leaders rotate over the other residues.
        let residues: Vec<u8> = (0..4u8).filter(|r| *r != self.byz_view % 4).collect();
        residues.iter().position(|r| *r == view % 4)
    }

    fn tip_wins(&self, tip: Option<Tip>, qc: Option<Qc>) -> bool {
        match (tip, qc) {
            (Some(t), Some(q)) => t.view > q.view && (self.winner == Winner::ByView || justify_view(t.block) >= q.view),
            (Some(_), None) => true,
            _ => false,
        }
    }

    fn make_tc(&self, view: u8, summaries: &[Summary]) -> Tc {
        let tip = summaries.iter().filter_map(|s| s.0).max_by_key(|t| t.view);
        let qc = summaries.iter().filter_map(|s| s.1).max_by_key(|q| q.view);
        // The collector keeps the first tip at the highest view; ties between two blocks at one
        // view need an equivocating leader, which the Byzantine leader of that view may be.
        Tc { view, tip, qc, tip_wins: self.tip_wins(tip, qc) }
    }

    /// The Byzantine timeout summaries it may send for `view`.
    fn byzantine_summaries(&self, state: &State, view: u8) -> Vec<Summary> {
        let mut tips: Vec<Option<Tip>> = vec![None];
        tips.extend(state.signed.iter().filter(|t| t.view <= view).map(|t| Some(*t)));
        let mut qcs: Vec<Option<Qc>> = vec![None];
        qcs.extend(state.qcs.iter().filter(|(v, _)| **v <= view).map(|(v, b)| Some(Qc { view: *v, block: *b })));
        let mut out = Vec::new();
        for t in &tips {
            for q in &qcs {
                out.push((*t, *q));
            }
        }
        out
    }

    /// Honest replica `r`'s timeout summary for `view`, if it can still time out of `view` (it has
    /// not voted in a later view): its summary now. An over-approximation (a replica's earlier
    /// timeout vote for `view` may have carried an older summary, and a replica may vote in a view
    /// after a TC of a later view counted it): every reachable state is covered, which is what
    /// Agreement needs; a liveness counterexample is checked against the code by hand.
    fn honest_summary(&self, r: &Replica, view: u8) -> Option<Summary> {
        (r.voted <= view).then(|| r.summary(self.report))
    }

    /// Every TC(view) formable now from three timeout summaries, with the mask of its honest
    /// signers (whose timeouts forming it records).
    fn formable_tcs(&self, state: &State, view: u8) -> BTreeSet<(Tc, u8)> {
        let honest: Vec<Option<Summary>> = state.replicas.iter().map(|r| self.honest_summary(r, view)).collect();
        let mut out = BTreeSet::new();
        if honest.iter().all(|s| s.is_some()) {
            let all: Vec<Summary> = honest.iter().map(|s| s.unwrap()).collect();
            out.insert((self.make_tc(view, &all), 0b111));
        }
        for i in 0..HONEST {
            for j in (i + 1)..HONEST {
                let (Some(a), Some(b)) = (honest[i], honest[j]) else { continue };
                for z in self.byzantine_summaries(state, view) {
                    out.insert((self.make_tc(view, &[z, a, b]), (1 << i) | (1 << j)));
                }
            }
        }
        out
    }

    /// V1-V4 for `block` proposed in `view` with `cert`.
    fn valid(&self, view: u8, block: BlockId, cert: Cert, qcs: &BTreeMap<u8, BlockId>) -> bool {
        let Some(j) = justify(block, qcs) else { return false };
        match cert {
            Cert::Justify => j.view + 1 == view,
            Cert::Tc(tc) => {
                if tc.view + 1 != view {
                    return false;
                }
                if tc.tip_wins {
                    tc.tip.is_some_and(|t| t.block == block)
                } else {
                    tc.qc.is_none_or(|q| j.view >= q.view || block == q.block)
                }
            }
        }
    }

    /// The honest leader rules L1/L2 for `leader` in `view`, holding `cert`: the block it proposes.
    fn leader_block(&self, leader: &Replica, view: u8, cert: Cert) -> BlockId {
        match cert {
            Cert::Justify => BlockId::F(view - 1, view),
            Cert::Tc(tc) if tc.tip_wins => tc.tip.unwrap().block,
            Cert::Tc(tc) => {
                let base = match tc.qc {
                    Some(q) if q.view > leader.highest.view => q,
                    _ => leader.highest,
                };
                BlockId::F(base.view, view)
            }
        }
    }

    /// Honest replica `r` votes for `p` if it may; `None` if it may not.
    fn vote(&self, st: &mut State, r: usize, p: Proposal) -> Option<()> {
        let qcs = st.qcs.clone();
        let replica = &mut st.replicas[r];
        if replica.view > p.view || replica.voted >= p.view {
            return None;
        }
        let j = justify(p.block, &qcs)?;
        let tc_qc = match p.cert {
            Cert::Tc(tc) => tc.qc,
            Cert::Justify => None,
        };
        if !replica.safe(p.block, j, tc_qc, self.unlock, &qcs) {
            return None;
        }
        // The certificates carried move the highest PC (a TC's high QC as an AdvanceView does).
        if let Some(q) = tc_qc {
            if q.view > replica.highest.view {
                replica.highest = q;
            }
        }
        replica.update(j, &qcs);
        replica.view = p.view;
        replica.voted = p.view;
        match p.cert {
            Cert::Tc(tc) if tc.tip_wins => {
                if self.copy_tips {
                    replica.tip = tc.tip;
                }
            }
            _ => replica.tip = Some(Tip { view: p.view, block: p.block }),
        }
        let mask = st.votes.entry((p.view, p.block)).or_insert(0);
        *mask |= 1 << r;
        Some(())
    }

    fn last_view(&self, state: &State) -> u8 {
        let mut v = 1;
        for r in &state.replicas {
            v = v.max(r.view).max(r.voted);
        }
        v = v.max(state.qcs.keys().copied().max().unwrap_or(0));
        v = v.max(state.proposals.iter().map(|p| p.view).max().unwrap_or(0));
        v
    }

    /// The synchronous suffix from `first` (TC(first - 1) formed per `tcs`, `byz` the Byzantine
    /// timeout summaries, `pair` the two honest replicas with it). Whether every honest replica
    /// commits a block above A.
    fn suffix_commits(&self, state: &State, first: u8, tcs: SuffixTcs, byz: Summary, pair: (usize, usize)) -> bool {
        let mut st = state.clone();
        let form_tc = |st: &State, view: u8| -> Tc {
            let s: Vec<Summary> = st.replicas.iter().map(|r| r.summary(self.report)).collect();
            match tcs {
                SuffixTcs::Honest => self.make_tc(view, &s),
                _ => self.make_tc(view, &[byz, s[pair.0], s[pair.1]]),
            }
        };
        let mut cert = Cert::Tc(form_tc(&st, first - 1));
        for view in first..first.saturating_add(SUFFIX_VIEWS) {
            if st.replicas.iter().all(|r| r.committed.is_some_and(|c| c != BlockId::A)) {
                return true;
            }
            let Some(li) = self.honest_leader(view) else {
                cert = Cert::Tc(form_tc(&st, view));
                continue;
            };
            let block = self.leader_block(&st.replicas[li], view, cert);
            let p = Proposal { view, block, cert, byzantine: false };
            let mut voters = 0;
            if self.valid(view, block, cert, &st.qcs) {
                for r in 0..HONEST {
                    if self.vote(&mut st, r, p).is_some() {
                        voters += 1;
                    }
                }
            }
            if voters == HONEST {
                st.qcs.insert(view, block);
                let qc = Qc { view, block };
                let qcs = st.qcs.clone();
                st.replicas.iter_mut().for_each(|r| r.update(qc, &qcs));
                cert = Cert::Justify;
            } else {
                cert = Cert::Tc(form_tc(&st, view));
            }
        }
        st.replicas.iter().all(|r| r.committed.is_some_and(|c| c != BlockId::A))
    }

    fn suffix_holds(&self, state: &State, tcs: SuffixTcs) -> bool {
        let last = self.last_view(state);
        let byz: Vec<Summary> = match tcs {
            SuffixTcs::Honest => vec![(None, None)],
            _ => self.byzantine_summaries(state, u8::MAX),
        };
        let pairs: Vec<(usize, usize)> = match tcs {
            SuffixTcs::Honest => vec![(0, 1)],
            SuffixTcs::ByzantineAny => vec![(0, 1), (0, 2), (1, 2)],
            SuffixTcs::ByzantineWithTopLock => {
                let top = state.replicas.iter().map(|r| r.lock.view).max().unwrap();
                [(0, 1), (0, 2), (1, 2)]
                    .into_iter()
                    .filter(|(a, b)| state.replicas[*a].lock.view == top || state.replicas[*b].lock.view == top)
                    .collect()
            }
        };
        (1..=2).all(|offset| {
            byz.iter().all(|b| pairs.iter().all(|pair| self.suffix_commits(state, last + offset, tcs, *b, *pair)))
        })
    }
}

impl Model for TipModel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            replicas: [Replica::new(), Replica::new(), Replica::new()],
            qcs: BTreeMap::from([(1, BlockId::A)]),
            proposals: BTreeSet::new(),
            signed: BTreeSet::new(),
            votes: BTreeMap::new(),
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for r in 0..HONEST {
            for view in state.qcs.keys() {
                if *view > 1 {
                    actions.push(Action::Deliver { r, view: *view });
                }
            }
        }
        for view in 2..=self.max_view {
            let mut certs = vec![(Cert::Justify, 0u8)];
            certs.extend(self.formable_tcs(state, view - 1).into_iter().map(|(tc, mask)| (Cert::Tc(tc), mask)));
            match self.honest_leader(view) {
                Some(li) => {
                    if state.proposals.iter().any(|p| p.view == view) {
                        continue;
                    }
                    // L1 exactly when the leader's highest PC is QC(v-1), else L2 on a TC(v-1).
                    let l1 = state.replicas[li].highest.view + 1 == view;
                    for (cert, mask) in certs {
                        if (cert == Cert::Justify) != l1 {
                            continue;
                        }
                        let block = self.leader_block(&state.replicas[li], view, cert);
                        if self.valid(view, block, cert, &state.qcs) {
                            actions.push(Action::Propose(Proposal { view, block, cert, byzantine: false }, mask));
                        }
                    }
                }
                None => {
                    for block in [BlockId::U, BlockId::V] {
                        actions.push(Action::Sign(Tip { view, block }));
                        for (cert, mask) in &certs {
                            if self.valid(view, block, *cert, &state.qcs) {
                                actions.push(Action::Propose(Proposal { view, block, cert: *cert, byzantine: true }, *mask));
                            }
                        }
                    }
                }
            }
        }
        for p in &state.proposals {
            for r in 0..HONEST {
                actions.push(Action::Vote { r, proposal: *p });
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut st = last.clone();
        match action {
            Action::Propose(p, mask) => {
                if !st.proposals.insert(p) {
                    return None;
                }
                let _ = mask;
                st.signed.insert(Tip { view: p.view, block: p.block });
            }
            Action::Sign(tip) => {
                if !st.signed.insert(tip) {
                    return None;
                }
            }
            Action::Vote { r, proposal } => {
                self.vote(&mut st, r, proposal)?;
                // The QC forms (at its collector) once two honest votes and the Byzantine one
                // exist; whether it reaches anyone is `Deliver`. Honest leaders use it only once
                // delivered to them (L1 reads the leader's own highest PC).
                let mask = st.votes[&(proposal.view, proposal.block)];
                if mask.count_ones() as usize + 1 >= QUORUM {
                    st.qcs.entry(proposal.view).or_insert(proposal.block);
                }
            }
            Action::Deliver { r, view } => {
                let qc = Qc { view, block: st.qcs[&view] };
                let qcs = st.qcs.clone();
                st.replicas[r].update(qc, &qcs);
                if st.replicas[r] == last.replicas[r] {
                    return None;
                }
            }
        }
        Some(st)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut props = vec![Property::<Self>::always("agreement", |_, state: &State| {
            let committed: Vec<BlockId> = state.replicas.iter().filter_map(|r| r.committed).collect();
            committed.iter().all(|a| committed.iter().all(|b| extends(*a, *b, &state.qcs) || extends(*b, *a, &state.qcs)))
        })];
        if self.suffixes.contains(&SuffixTcs::Honest) {
            props.push(Property::<Self>::always("suffix_honest", |m: &TipModel, s: &State| m.suffix_holds(s, SuffixTcs::Honest)));
        }
        if self.suffixes.contains(&SuffixTcs::ByzantineWithTopLock) {
            props.push(Property::<Self>::always("suffix_byzantine_with_top_lock", |m: &TipModel, s: &State| {
                m.suffix_holds(s, SuffixTcs::ByzantineWithTopLock)
            }));
        }
        if self.suffixes.contains(&SuffixTcs::ByzantineAny) {
            props.push(Property::<Self>::always("suffix_byzantine_any", |m: &TipModel, s: &State| {
                m.suffix_holds(s, SuffixTcs::ByzantineAny)
            }));
        }
        props.push(Property::<Self>::sometimes("sibling_certified", |_, s: &State| {
            s.qcs.values().any(|b| *b == BlockId::U) && s.qcs.values().any(|b| *b == BlockId::V)
        }));
        props
    }
}

fn model(winner: Winner, report: Report, max_view: u8, suffixes: &'static [SuffixTcs]) -> TipModel {
    TipModel { winner, report, copy_tips: true, unlock: true, max_view, byz_view: 4, suffixes }
}

/// Prefixes of at most this many events (proposals, signed headers, votes, QC deliveries); the
/// Codex trace is nine events deep. 11 by default (each fix check runs in about a minute in a debug
/// build). Raise it with `TORUS_TC_TIP_MODEL_DEPTH`; for s138 the fix checks were run in a release
/// build at 13 (the active-Byzantine one at 12) and hold.
fn depth() -> usize {
    std::env::var("TORUS_TC_TIP_MODEL_DEPTH").ok().and_then(|d| d.parse().ok()).unwrap_or(11)
}

fn check(m: TipModel, depth: usize) -> impl Checker<TipModel> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(8));
    m.checker().threads(threads).target_max_depth(depth).spawn_bfs().join()
}

const HONEST_ONLY: &[SuffixTcs] = &[SuffixTcs::Honest];

/// Before the fix (tip wins by view, the QC omitted when the tip is newer): a replica locked on a
/// QC whose block another replica's tip does not extend (the tip's block justifies below the
/// lock), and every honest TC selects that tip, which the locked replica refuses forever. Codex's
/// trace (a Byzantine tip of V copied by two replicas) is one such state
/// (`tc_tip_ordinary_stall_test`); the shortest one the search finds needs no Byzantine tip: a
/// replica counted in TC(2) by its Bracha echo still votes in view 2 (the echo does not leave the
/// view), and the fresh sibling of view 3 then outranks the QC of view 2 by view.
#[test]
fn tip_by_view_stalls_on_an_incompatible_tip() {
    let checker = check(model(Winner::ByView, Report::OmitIfTipNewer, 5, HONEST_ONLY), 10);
    let path = checker.discovery("suffix_honest").expect("an incompatible tip stalls the honest suffix");
    let last = path.last_state().clone();
    let incompatible = last.replicas.iter().any(|locked| {
        locked.lock != QA
            && last.replicas.iter().any(|r| {
                r.tip.is_some_and(|t| !extends(t.block, locked.lock.block, &last.qcs) && justify_view(t.block) < locked.lock.view)
            })
    });
    assert!(incompatible, "the counterexample has a tip that does not extend another replica's lock: {:?}", path.into_actions());
    assert!(checker.discovery("agreement").is_none());
}

/// The fix: from every reachable state (views 2..=5, Byzantine tips of U and V in view 4, any TC of
/// three timeout summaries, lost QCs, copied tips) the honest synchronous suffix commits, and
/// Agreement holds.
#[test]
fn compatible_tip_rule_commits_after_an_ordinary_sibling_tip() {
    let checker = check(model(Winner::Compatible, Report::Always, 5, HONEST_ONLY), depth());
    assert!(
        checker.discovery("suffix_honest").is_none(),
        "the honest suffix stalls: {:?}",
        checker.discovery("suffix_honest").map(|p| p.into_actions())
    );
    assert!(
        checker.discovery("agreement").is_none(),
        "Agreement violated: {:?}",
        checker.discovery("agreement").map(|p| p.into_actions())
    );
}

/// The fix, with the Byzantine timeout vote (any fixed tip and QC) in every TC together with a
/// replica holding the highest lock: the suffix commits. One event shallower than the honest check
/// (every Byzantine choice is simulated per state).
#[test]
fn compatible_tip_rule_commits_with_an_active_byzantine_tip() {
    const TOP: &[SuffixTcs] = &[SuffixTcs::ByzantineWithTopLock];
    let checker = check(model(Winner::Compatible, Report::Always, 5, TOP), depth() - 1);
    assert!(
        checker.discovery("suffix_byzantine_with_top_lock").is_none(),
        "the suffix stalls: {:?}",
        checker.discovery("suffix_byzantine_with_top_lock").map(|p| p.into_actions())
    );
}

/// RESIDUAL (not fixed, needs the TC lock escape, docs): an active Byzantine validator whose
/// timeout vote is in every TC together with the two replicas NOT holding the highest lock (and
/// that reports a low QC) keeps its own tip of V compatible with the TC's high QC; honest leaders
/// re-propose V, the replica locked on U refuses it, and the Byzantine one withholds.
#[test]
fn compatible_tip_rule_residual_when_the_tc_omits_the_top_lock() {
    const ANY: &[SuffixTcs] = &[SuffixTcs::ByzantineAny];
    let checker = check(model(Winner::Compatible, Report::Always, 5, ANY), 10);
    assert!(checker.discovery("suffix_byzantine_any").is_some(), "the residual stall is gone; update the docs");
}


/// Both halves of the fix are needed. The winner rule alone (the QC still omitted when the tip is
/// newer): a replica locked on QC(W, 3) whose tip is a newer sibling U hides its QC, U wins as
/// compatible with the lower high QC, and the locked replica refuses it. Reporting the QC alone
/// (tip still winning by view): the fresh sibling of view 3 outranks QC(U, 2) by view.
#[test]
fn each_half_of_the_fix_alone_stalls() {
    for (winner, report) in [(Winner::Compatible, Report::OmitIfTipNewer), (Winner::ByView, Report::Always)] {
        let checker = check(model(winner, report, 5, HONEST_ONLY), 10);
        assert!(checker.discovery("suffix_honest").is_some(), "{winner:?} with {report:?} alone does not stall");
        assert!(checker.discovery("agreement").is_none());
    }
}
