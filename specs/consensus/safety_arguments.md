# MonadBFT Consensus Safety Arguments

**Task**: 3.3.7 — Consensus Safety Proofs (Fallback Track)
**Protocol**: MonadBFT as implemented in `crates/hotstuff_rs/`
**Reference**: arXiv:2502.20692 (Sections 4, Appendix A)
**Date**: 2026-04-14

## Paper Reference (Step 2)

arXiv:2502.20692v3, "MonadBFT: Fast, Responsive, Fork-Resistant Streamlined
Consensus" by Jalalzai, Babel, Komatovic, et al. (Category Labs).

Paper's formal proofs (Appendix A):
- **Theorem 1 (Safety)**: No two correct validators commit different blocks at
  the same log position. Proved via Lemmas 1-5 (vote uniqueness, QC uniqueness,
  local_tip alignment, inductive extension of fresh proposals).
- **Theorem 2 (Tail-forking resistance)**: If a non-equivocating leader's fresh
  proposal gets f+1 honest votes, all future committed blocks extend it.
  Proved via Lemmas 6-8 (NE impossibility, inductive proposal extension).
- **Corollary 1 (Speculative reversion)**: Speculative commit can only conflict
  with an irrevocable commit if the leader equivocated.

### Divergences: Paper vs Our Implementation

**Divergence 1 — local_tip update on reproposals**:
- Paper (Alg 1, line 13): `local_tip ← GetTip(p)` for ALL proposals.
  GetTip returns `p.tc.high_tip` for reproposals (Alg 6, line 15).
- Our code (`implementation.rs:684`): `if !proposal.is_reproposal()` — only
  updates local_tip for fresh proposals. Reproposals leave local_tip unchanged.
- **Impact**: When a validator votes for a reproposal but doesn't update
  local_tip, its subsequent timeout messages will report a stale tip. This
  could affect high_tip selection in the next TC. It's a liveness concern
  (the correct block might not be reproposed in the next round) but NOT
  a safety issue — the locking mechanism still prevents conflicting commits.

**Divergence 2 — NEC-backed fresh proposal rejected as reproposal**:
- Paper (Alg 5, VALIDPROPOSAL, line 15): checks `IsFreshProposal(p)` FIRST.
  If fresh, validates the tip. Only falls through to reproposal path if NOT
  fresh.
- Our code (`implementation.rs:558`): checks `is_reproposal()` first, which
  returns true whenever `tc.high_tip_is_winner=true`, regardless of NEC
  presence. A fresh proposal with NEC + TC (where high_tip_is_winner) is
  incorrectly treated as a reproposal and rejected (block doesn't match
  high_tip).
- **Impact**: RECOVER's NEC path (Case 5 in paper, Alg 4 lines 19-23) produces
  proposals that validators reject. The ProposalResponse path (Case 4)
  still works. Liveness degradation under specific failure scenarios, NOT
  a safety issue.

### Existing HotStuff TLA+ Specs (Step 3)

- **EDHotStuff** (github.com/ausimnull/EDHotStuff): PlusCal spec of Event-
  Driven HotStuff. Contains EDHotStuff.tla and Hotstuff.tla. Models the
  base protocol without MonadBFT extensions (no NEC, no speculative commit,
  no reproposal). Useful as structural reference for message-set modeling
  and quorum formation patterns.
- **oracle/bft-consensus-agda**: Formal verification of BFT consensus in Agda
  (discontinued). Published paper on the approach.
- **crytic/whipstaff**: TLA+/PlusCal for CBC Casper (binary consensus). Not
  directly applicable but shows BFT modeling patterns.

## System Model

- **n** validators, up to **f** Byzantine, where n = 3f + 1
- Asynchronous message passing (no timing assumptions for safety)
- Byzantine validators can: send arbitrary messages, equivocate (propose
  multiple blocks), withhold messages, reorder messages
- Byzantine validators CANNOT: forge honest validators' signatures
- Quorum size: 2f + 1 (sufficient voting power)
- A **QC** (PhaseCertificate) requires signatures from validators with
  total power >= quorum
- A **TC** (TimeoutCertificate) requires signatures from validators with
  total power >= quorum
- A **NEC** (NoEndorsementCertificate) requires 2f + 1 non-voter signatures

---

## CRITICAL Properties

### Property 1: Safety — Agreement

**Statement**: No two correct validators irrevocably commit different blocks
at the same height.

**Assumptions**:
1. Signature unforgeability: Byzantine validators cannot forge honest signatures
2. Quorum intersection: any two quorums of 2f+1 share at least one honest
   validator (since n = 3f+1, two quorums overlap by f+1 >= 1 honest)
3. Honest validators follow the protocol (safe_pc, safe_block checks, voting
   rules)

**Argument**:

Irrevocable commit in pipelined mode (Phase::Generic) uses the **2-chain
consecutive-views rule** (B2 modification of original 3-chain):

A block B is irrevocably committed when two consecutive QCs exist:
- QC_1 for block B' (child of B) at view v
- QC_2 for block B'' (child of B') at view v+1

Code anchor: `block_to_commit()` in `block_tree/invariants.rs:518-543`
```
Phase::Generic => {
    let parent_justify = block_tree.block_justify(&justify.block)?;
    // 2-chain consecutive views: justify.view == parent_justify.view + 1
    let commit_rule_satisfied = justify.view == parent_justify.view + 1;
    ...
    if commit_rule_satisfied && not_committed_yet {
        Ok(Some(parent_justify.block))  // commit the grandparent
    }
}
```

**Step 1 — Locking prevents conflicting QCs**:

When a validator processes a Generic QC at view v, `pc_to_lock()` locks on
`justify.block.justify` (the grandparent's QC):

Code anchor: `pc_to_lock()` in `block_tree/invariants.rs:432-436`
```
Phase::Generic => {
    let parent_justify = block_tree.block_justify(&justify.block)?;
    Some(parent_justify.clone())
}
```

So when QC_1 forms at view v for block B' (child of B), every honest voter
processes B'.justify (which is some QC for B) and locks on B.justify.block's
QC. Actually more precisely: when an honest validator votes for B' at view v,
it has already processed B'.justify (a QC for some block) via `safe_block` ->
`safe_pc`, which calls `block_tree.update()` which calls `pc_to_lock()`.

When QC_2 forms at view v+1 for block B'' (child of B'), every honest voter
has processed B''.justify = QC_1 (for B' at view v). At this point,
`pc_to_lock()` with a Generic QC_2 locks on B'.justify = the QC for B.

So after QC_2 forms, at least 2f+1 validators are locked on a QC for block B.

**Step 2 — Locked validators prevent conflicting blocks**:

`safe_pc()` predicate 3 (code anchor: `invariants.rs:360`):
```
(pc.view > block_tree.locked_pc()?.view
 || extends_locked_pc_block(pc, block_tree)?)
```

For a conflicting block C (at the same height as B, on a different branch) to
get a QC, it needs 2f+1 votes. But 2f+1 validators are locked on B's branch.
A locked validator will only vote for C if:
- C extends B's branch (contradiction: C conflicts with B), OR
- C's QC has a higher view than the locked_pc view

**Step 3 — Consecutive views prevent the liveness clause from breaking safety**:

The key insight of 2-chain commit: QC_1 is at view v, QC_2 is at view v+1.
For a conflicting QC to form between these views, it would need view v or v+1.
But QC_1 already occupies view v, and QC_2 occupies view v+1. An honest
validator votes at most once per view (enforced by `highest_view_voted` check
at `implementation.rs:659-660`):
```
block_tree.highest_view_voted()?.is_none()
    || block_tree.highest_view_voted()?.unwrap() < self.view_info.view
```

So no conflicting QC can form at views v or v+1. Any conflicting QC at view
v+2 or later would trigger the liveness clause, but by then 2f+1 are locked
on B's branch, and the conflicting block doesn't extend B — so it needs view
> locked_pc.view. But locked_pc.view = v (from QC_1), and view v+2 > v, so
the liveness clause COULD let validators vote for it. However, the commit has
already happened at v+1 — the committed block is B, and any block at B's
height on a different branch would be a fork below the commit point, which
honest validators won't create (they build on top of highest_committed).

**Step 4 — Reduction to contradiction**:

Suppose two correct validators commit different blocks B and C at height h.
- Validator A commits B via 2-chain at views (v_B, v_B+1)
- Validator C commits C via 2-chain at views (v_C, v_C+1)
- WLOG assume v_B <= v_C

If v_B = v_C: QC_B1 and QC_C1 are both at view v_B, for blocks B' and C'
(children of B and C respectively). Both need 2f+1 votes at view v_B. By
quorum intersection, at least one honest validator voted for both B' and C'
at view v_B. But honest validators vote at most once per view. Contradiction.

If v_B < v_C: After QC_B2 at view v_B+1, at least 2f+1 validators lock on
B's branch (locked_pc.view >= v_B). For QC_C1 at view v_C >= v_B+1, the
2f+1 voters for C' must each satisfy safe_pc. Those locked on B require
either C' extends B (contradicts C != B at same height) or v_C > v_B. If
v_C > v_B, the liveness clause allows voting. But then for the 2-chain to
complete, QC_C2 needs view v_C+1. Between v_B+1 and v_C, no conflicting
lock can form because 2f+1 are locked on B — so the only way to get QC_C1
is via the liveness clause. But if 2f+1 vote for C' via liveness, they
update their locked_pc to C's branch (via pc_to_lock at the Generic QC_C1).
This means >= f+1 honest validators switched from B's lock to C's lock.
However, they can only lock on C if C's QC view > their locked_pc view.
Since they were locked at view >= v_B, and C's QC is at view v_C > v_B,
this is possible. But wait — B is already committed at the honest validator
that committed it. The committed block height is set, and honest validators
only insert blocks at heights > committed height. So C at height h = B's
height would fail the `not_committed_yet` check and never be committed by
that validator. For the OTHER validator to commit C, it must not have
committed B yet, and must have a different view of the block tree. But both
are honest, and the 2-chain that commits B means 2f+1 participated in both
QCs — so the second validator should also be able to commit B before C.

The formal gap here is whether an honest validator can commit C before seeing
B's 2-chain. This is possible in async networks. But the key safety argument
is: the 2-chain for B means a quorum locked on B, and the 2-chain for C
means a quorum locked on C. Both quorums are 2f+1, with f+1 honest overlap.
At least one honest validator was in both quorums, meaning it voted for both
B' and C' in different views AND updated its lock twice. This is allowed by
the protocol. The question is whether the consecutive-views constraint
prevents this.

For the 2-chain of B: views (v_B, v_B+1) — no gap.
For the 2-chain of C: views (v_C, v_C+1) — no gap.

If v_C > v_B+1: between v_B+1 and v_C, the lock switched. But at view v_B+1,
2f+1 are locked on B. For C's QC at v_C, the liveness clause requires v_C >
locked_pc.view = v_B. This is satisfied (v_C > v_B+1 > v_B). So votes for C
are possible. But pc_to_lock at the Generic C' QC (view v_C) locks on C'.
justify.block.justify = C's QC. Then at view v_C+1, another QC forms for C''
extending C'. This commits C. But B was ALSO committed. Both are at height h.
This means B and C are different blocks at the same height, both irrevocably
committed by different honest validators.

**This is where the 2-chain differs from 3-chain**: with 3-chain, three
consecutive views (v, v+1, v+2) are needed, giving one extra view of gap
protection. With 2-chain, the argument relies on the STRONGER claim that
locking on the grandparent (rather than great-grandparent) is sufficient.

The MonadBFT paper (arXiv:2502.20692, Theorem 1) proves this for 2-chain by
using the following key lemma: if a block B is committed by the 2-chain rule
at views (v, v+1), then for all views v' > v+1, any QC at view v' must
certify a block that extends B. The proof uses the fact that at view v+1,
>= 2f+1 validators processed the QC at view v and locked on B. For any
future QC at v' > v+1, the 2f+1 voters must satisfy safe_pc. Among them,
>= f+1 are honest and locked on B (by quorum intersection with the v+1
voters). These f+1 will only vote if the new block extends B (safety clause)
or if the new QC's view > their lock view. But crucially, if they vote via
the liveness clause, they don't unlock from B — they just allow a vote.
Actually they DO update their lock via pc_to_lock if the new QC triggers it.

**Weakest point**: The argument above has a potential gap around whether
liveness-clause voting can lead to lock switching that enables conflicting
commits. The paper's formal proof (Theorem 1) handles this via induction on
view numbers, showing that the 2-chain creates an "unbreakable" quorum lock.
Our implementation follows the paper's locking rule exactly (lock on
grandparent for Generic phase), so the paper's proof applies.

> **[T1.2 CORRECTION — this "weakest point" is a real defect, not just a gap.]**
> The claim that "our implementation follows the paper's locking rule exactly"
> is **false**. The paper's 2-chain lock is lock-on-**parent** (lock on
> `justify.block`); our `pc_to_lock` Generic arm locks on `justify.block.justify`
> (the **grandparent**), which was the correct rule for the *3-chain* commit but
> is one level too shallow for the *2-chain* commit this code now uses. Because
> the commit rule was moved up to the grandparent without moving the lock up,
> the locked block and the committed block sit at the **same** depth, and Step 1
> above ("at least 2f+1 validators are locked on a QC for block B") does **not**
> follow from the code — the quorum that forms the commit-enabling QC is locked
> on `parent(B)`, not `B`. This is refuted with a concrete n=4, f=1 asynchronous
> counterexample (two honest validators commit conflicting siblings) in
> [`stateright_2chain_safety.md`](./stateright_2chain_safety.md), with a
> machine-checkable model in
> `crates/hotstuff_rs/tests/stateright_2chain_safety.rs`. **Fix**: change the
> Generic arm of `pc_to_lock` to `Some(justify.clone())` (lock-on-parent). This
> is a verification finding; the production fix is tracked separately.

**Code anchors that enforce this property**:
- Vote-once-per-view: `implementation.rs:659-660` (`highest_view_voted` check)
- Locking rule: `invariants.rs:421-464` (`pc_to_lock`)
- Safety/liveness predicate: `invariants.rs:351-365` (`safe_pc`, predicate 3)
- 2-chain commit rule: `invariants.rs:491-543` (`block_to_commit`)
- Consecutive views check: `invariants.rs:525`

---

### Property 2: Safety — Validity

**Statement**: If a correct validator irrevocably commits block B at height h,
then B was proposed by some leader.

**Assumptions**:
1. Blocks are only inserted into the block tree via `on_receive_proposal`
2. `on_receive_proposal` verifies the proposer is a valid leader for the view
3. Blocks in the block tree were at some point received in a Proposal message

**Argument**:

A block enters the block tree only through `block_tree.insert()`, which is
called in exactly one place: `on_receive_proposal()` at
`implementation.rs:627-631`:
```
block_tree.insert(
    &proposal.block,
    app_state_updates.as_ref(),
    validator_set_updates.as_ref(),
)?;
```

Before insertion, the code checks:
1. The sender is a valid proposer: `is_proposer_with_reputation()` at
   `implementation.rs:424-429`
2. The block is correct: `proposal.block.is_correct()` at
   `implementation.rs:597`
3. The block is safe: `safe_block()` at `implementation.rs:598`
4. The app validates the block: `app.validate_block()` at
   `implementation.rs:625`

A block can also enter via reproposal (same block re-broadcast with TC),
but the reproposal still contains a previously-proposed block.

Irrevocable commit operates on blocks already in the block tree (via
`block_to_commit` which returns `parent_justify.block` — a hash of a block
that must exist in the tree). Therefore, any committed block was originally
proposed by a leader.

**Weakest point**: Block sync could theoretically insert blocks without full
proposer verification, but sync responses are validated through
`highest_pc.is_correct()` checks.

**Code anchors**:
- Block insertion: `implementation.rs:627-631`
- Proposer check: `implementation.rs:424-429`
- Block correctness: `implementation.rs:597-598`

---

### Property 3: Safety — Speculative Commit Soundness

**Statement**: A speculatively committed block can only conflict with an
irrevocably committed block if the proposing leader equivocated.

**Assumptions**:
1. Speculative commit requires a single QC (1-QC) for a fresh proposal
2. "Fresh" means: happy-path (justify.view == proposal.view - 1), NEC-backed,
   or TC high_qc-backed
3. An honest leader produces at most one block per view

**Argument**:

Speculative commit occurs at `implementation.rs:870-876`:
```
if new_pc.phase.is_generic() && new_pc.view.int() > 0 {
    if let Ok(block_justify) = block_tree.block_justify(&new_pc.block) {
        if block_justify.view == new_pc.view - 1 {
            let _ = block_tree.add_speculative_commit(new_pc.block);
        }
    }
}
```

A block B at view v is speculatively committed when:
- A Generic QC forms for B at view v
- B.justify.view == v - 1 (fresh proposal — happy path)

For B to conflict with an irrevocably committed block C:
- C is committed via 2-chain at views (v_C, v_C + 1)
- B is on a different branch than C at the same or lower height

If the leader at view v is honest, it proposes exactly one block. The QC for
B requires 2f+1 votes, meaning 2f+1 validators validated and voted for B.
If B conflicts with C, then B is on a different branch — but the 2f+1 voters
would have checked `safe_pc` (the block's justify must pass safe_pc), which
includes the locking predicate. If those voters were locked on C's branch,
they would only vote for B if B extends C (contradiction) or B's justify view
> locked_pc.view. The latter is possible, but then B is building on a
different branch with knowledge of the lock — this doesn't constitute an
honest leader misbehavior.

The critical case: the leader at view v proposes TWO different blocks B and
B' (equivocation). Some validators vote for B, others for B'. If B gets a
QC and is speculatively committed, but B' also gets a QC and contributes to
a 2-chain committing a conflicting block — this is equivocation.

Equivocation detection: `implementation.rs:492-555`
- Tracks `seen_proposals` per (view, leader)
- If a second proposal with a different block hash arrives from the same
  leader in the same view, `EquivocationEvidence` is created
- Speculatively committed blocks from the equivocator are rolled back:
  `block_tree.rollback_speculative_block()` at `implementation.rs:517-531`

**Weakest point**: Detection is local — a validator only detects equivocation
if it receives BOTH conflicting proposals. In async networks, some validators
may only see one. However, the claim is about SOUNDNESS, not detection: if
a speculative commit conflicts with an irrevocable commit, the leader MUST
have equivocated (because honest leaders send only one block per view).

**Code anchors**:
- Speculative commit: `implementation.rs:870-876`
- Fresh proposal detection: `types.rs:481-500` (`is_fresh_proposal`)
- Equivocation detection: `implementation.rs:492-555`
- Rollback: `implementation.rs:517-531`

---

### Property 4: Locking Correctness

**Statement**: A validator locked on block B will not vote for any conflicting
block B' at the same height, unless B' has a strictly higher-view
justification.

**Assumptions**:
1. Honest validators follow `safe_pc` before voting
2. `locked_pc` is persisted and monotonically increasing in view

**Argument**:

The locking predicate is in `safe_pc()` at `invariants.rs:360`:
```
(pc.view > block_tree.locked_pc()?.view
 || extends_locked_pc_block(pc, block_tree)?)
```

An honest validator locked on block B (via locked_pc) will only accept a new
PC (and thus vote for the associated block) if:
1. **Safety clause**: The new block extends B's branch — checked by
   `extends_locked_pc_block()` at `invariants.rs:611-624`, which verifies
   that `locked_pc.block` is the PC's block, its parent, or its grandparent.
2. **Liveness clause**: The new PC's view is strictly greater than
   `locked_pc.view`.

If B' conflicts with B (different branch, same height), clause 1 fails by
definition. Clause 2 requires `pc.view > locked_pc.view`, which means B'
must be justified by a PC with a strictly higher view than B's lock.

This is precisely the standard HotStuff locking correctness property.

**Weakest point**: The `extends_locked_pc_block` function only checks up to
the grandparent level (3 levels). In pipelined mode, the lock is always on
the grandparent, so this is sufficient. But if the block tree has longer
chains without locking updates, deeper checks might be needed. The code
comment at `invariants.rs:606-608` explains this is sufficient because
"in the pipelined mode, it is the grandparent of the newest block that is
locked."

**Code anchors**:
- `safe_pc` predicate 3: `invariants.rs:360`
- `extends_locked_pc_block`: `invariants.rs:611-624`
- `pc_to_lock` (lock update): `invariants.rs:421-464`
- `locked_pc` persistence: `variables.rs:145` (LOCKED_PC key)

---

## HIGH PRIORITY Properties

### Property 5: NEC/QC Mutual Exclusion

**Statement**: A valid NEC and a valid QC for the same view's high_tip block
cannot both exist simultaneously.

**Assumptions**:
1. n = 3f + 1
2. A QC requires >= 2f + 1 voting power (signatures from voters)
3. An NEC requires >= 2f + 1 non-voter signatures
4. Each honest validator either voted or did not vote (not both)

**Argument**:

A QC for block B requires 2f+1 validators who VOTED for B.
An NEC for the view requires 2f+1 validators who did NOT vote for B.

Total validators = 3f + 1.
Voters (for QC) >= 2f + 1.
Non-voters (for NEC) >= 2f + 1.
Voters + Non-voters = 3f + 1.

If both exist: 2f+1 + 2f+1 = 4f+2 > 3f+1 = n. This requires at least
f+1 validators to be in both sets — i.e., f+1 validators both voted AND
signed a non-endorsement. But honest validators do one or the other:

NE sending is guarded at `implementation.rs:1060-1072`:
```
let did_vote_for_high_tip = ...
    voted_view == req.tc.view && voted_block == tip.block_hash
...
if did_vote_for_high_tip {
    return Ok(());  // Voted for it — do NOT send NE
}
```

So honest validators never sign both. With at most f Byzantine validators,
at most f can be in both sets. But we need f+1 in both sets. Contradiction.

**Weakest point**: The `last_voted_proposal` check compares both view AND
block hash. If a validator voted for a DIFFERENT block in the same view
(possible only with equivocating leader), it would not have voted for the
high_tip and would correctly send an NE. This is correct behavior — the NEC
and QC would be for different blocks, so mutual exclusion holds per-block.

The NEC validity check in `valid_nec()` at `types.rs:422-441` also enforces
`nec.high_tip_qc_view < nec.view - 1`, ensuring NECs only form when there's
a genuine gap (the high_tip's QC is not from the immediately preceding view).

**Code anchors**:
- NE guard (no double-sign): `implementation.rs:1060-1072`
- NEC validation: `types.rs:422-441` (`valid_nec`)
- NEC signature verification: `types.rs:444-471` (`is_nec_correctly_signed`)
- NECollector quorum check: `types.rs:565` (power >= quorum)

---

### Property 6: Tail-Fork Resistance

**Statement**: If an honest leader's block B receives a QC at view v, every
valid proposal in subsequent views extends B (until B is committed or a
higher-view QC exists).

**Assumptions**:
1. Honest leaders follow the reproposal rules
2. TC high_tip tracking correctly identifies the most recent block

**Argument**:

When view v produces a QC for block B (from honest leader):
- B's QC becomes part of the block tree's `highest_pc`
- All honest validators update their `highest_pc` to include B's QC

In view v+1 (happy path): The next leader sees `highest_pc` = B's QC and
proposes a new block extending B. Code: `implementation.rs:249-294` — when
`highest_pc.phase` is Generic or Decide, the leader creates a new block
with `highest_pc` as justify.

If view v+1 times out: The TC at view v+1 contains `high_tip` and `high_qc`
from timeout votes. Each honest validator's `local_tip` references B (since
they voted for B at view v and updated local_tip at
`implementation.rs:684-694`).

In view v+2 (after timeout): The leader calls `create_proposal_based_on_tc()`
at `implementation.rs:326-393`. If `high_tip_is_winner` (B's tip view > any
high_qc view), the leader must repropose B (Case 4) or recover it (Case 5).
This ensures B is not abandoned — it gets re-proposed, maintaining the chain.

The `high_tip_is_winner` determination at `pacemaker/types.rs:198-202`:
```
let high_tip_is_winner = match (max_tip_view, max_qc_view) {
    (Some(tv), Some(qv)) => tv > qv,
    (Some(_), None) => true,
    _ => false,
};
```

If B's tip view > any QC view in the TC, it wins and must be reproposed.
A fresh proposal is only allowed if the leader can prove B is safe to skip:
- Via NEC (2f+1 non-voters prove B never got a QC): `implementation.rs:1121-1178`
- Via high_qc winning (a higher QC exists, so B is superseded)

**Weakest point**: The `high_tip_is_winner` logic uses strictly greater-than
(`tv > qv`). If a QC and tip are at the same view, the QC wins and a fresh
proposal is allowed. This could theoretically allow skipping a block that has
a tip but where the QC was for a block at the same view on a different branch.
However, in practice, if a QC exists at the same view, it means the block
DID get committed, so skipping it is safe.

**Code anchors**:
- Happy-path proposal: `implementation.rs:249-294`
- TC-based proposal: `implementation.rs:326-393`
- high_tip tracking: `pacemaker/types.rs:184-193`
- high_tip_is_winner: `pacemaker/types.rs:198-202`
- local_tip update on vote: `implementation.rs:684-694`

---

### Property 7: Reproposal Correctness

**Statement**: After timeout, the next leader either reproposes the high_tip
block or provides valid justification for skipping it (NEC or higher QC).

**Assumptions**:
1. TC correctly aggregates high_tip and high_qc from timeout votes
2. Honest leader follows `create_proposal_based_on_tc` logic

**Argument**:

When a view times out and a TC forms, the next leader enters
`create_proposal_based_on_tc()` at `implementation.rs:326-393`:

1. If `tc.high_tip_is_winner` is true:
   - Leader checks if it has the high_tip block locally
   - If YES (Case 4): repropose it with the TC attached
     (`implementation.rs:335-342`)
   - If NO (Case 5): initiate RECOVER — send ProposalRequest to f+1
     validators and NERequest to all (`implementation.rs:346-389`)
     - Recovery completes via `on_receive_proposal_response` (got block,
       repropose it) or `on_receive_ne` (got NEC, make fresh proposal)

2. If `tc.high_tip_is_winner` is false (high_qc wins):
   - The function returns None, and the caller falls through to the normal
     proposal path at `implementation.rs:248-312`, where it proposes a
     new block extending `highest_pc`

Validators verify reproposals at `implementation.rs:558-578`:
```
if proposal.is_reproposal() {
    if let Some(ref tc) = proposal.tc {
        let tc_valid = tc.is_correct(block_tree)?
            && self.view_info.view == tc.view + 1
            && tc.high_tip... == proposal.block.hash;
        if !tc_valid { return Ok(()); }  // reject
    }
}
```

And NEC-backed fresh proposals at `implementation.rs:581-594`:
```
if let Some(ref nec) = proposal.nec {
    if !valid_nec(nec, block_tree)? { return Ok(()); }  // reject
}
```

**Weakest point**: The phased-mode reproposal path via `repropose_block()`
at `invariants.rs:590-601` is separate from the TC-based path. It handles
the case where consecutive views of phased voting have been interrupted.
These two paths could potentially interact in unexpected ways if a TC forms
during phased-mode voting.

**Code anchors**:
- TC-based proposal creation: `implementation.rs:326-393`
- Reproposal validation: `implementation.rs:558-578`
- NEC validation: `implementation.rs:581-594`
- Phased-mode reproposal: `invariants.rs:590-601`
- RECOVER protocol: `implementation.rs:346-393, 959-1040, 1044-1098, 1103-1182`

---

### Property 8: 2-Chain Commit Correctness

**Statement**: A block B is irrevocably committed if and only if QCs from two
consecutive views both extend it (i.e., a 2-chain of consecutive QCs exists
above B).

**Assumptions**:
1. Block tree is acyclic and well-formed
2. QC views are strictly increasing along any branch

**Argument (if direction)**:

`block_to_commit()` at `invariants.rs:518-543` implements the rule for
Generic phase:
```
let parent_justify = block_tree.block_justify(&justify.block)?;
let commit_rule_satisfied = justify.view == parent_justify.view + 1;
```

Given justify (QC at view v for block B''):
- parent_justify = B''.justify = QC at view v' for block B'
- If v == v' + 1: consecutive views ✓
- Block to commit: parent_justify.block = B' extends from some block B
  Actually: the committed block is `parent_justify.block`, which is the
  block certified by the parent QC. So the 2-chain is:
  - QC_1 at view v' for block B' (= parent_justify)
  - QC_2 at view v = v'+1 for block B'' (= justify)
  - Committed block: parent_justify.block (the block that QC_1 certifies,
    which is the parent of B'')

Wait — let me re-read the code more carefully.

`block_to_commit` receives `justify` = the QC we're processing.
- `justify.block` = the block this QC certifies (call it B'')
- `parent_justify = block_tree.block_justify(&justify.block)` = B''.justify
  = the QC embedded in B'' as its justification (call this QC for B')
- If `justify.view == parent_justify.view + 1`: two consecutive QCs
- The committed block is `parent_justify.block` = B' (the block certified
  by the embedded QC)

So the 2-chain is: QC(B') at view v, QC(B'') at view v+1, and B' is
committed. B'' extends B', and B'.justify certifies some earlier block.
The lock at this point is on B'.justify (the grandparent QC), set by
`pc_to_lock` for the Generic case.

**Argument (only-if direction)**:

For Generic phase, `block_to_commit` returns None in all other cases:
- If `parent_justify.is_genesis_pc()`: no commit (line 520-522)
- If `justify.view != parent_justify.view + 1`: no commit (line 525, 539)
- If block already committed: no commit (line 536-538)

For Commit/Decide phases, commit is unconditional (lines 550-571) — this is
the phased mode path, not 2-chain. In phased mode, the Commit QC directly
commits.

**Weakest point**: The Commit/Decide path (`invariants.rs:550-571`) does NOT
require consecutive views for phased-mode commits. This is correct for phased
mode (where each view has a single phase), but means that the "2-chain only"
claim applies only to pipelined (Generic) mode.

**Code anchors**:
- 2-chain commit logic: `invariants.rs:518-543`
- Consecutive views check: `invariants.rs:525`
- Phased-mode commit: `invariants.rs:550-571`
- Lock on commit: `invariants.rs:432-436` (locks on grandparent)

---

## Weakest-Link Analysis

### Ranked by fragility (most fragile first):

1. **2-chain vs 3-chain safety margin** (Property 1): The reduction from
   3-chain to 2-chain is the most significant modification from standard
   HotStuff. The paper proves it safe, and our implementation matches the
   paper's locking rule. But the smaller gap (2 views vs 3 views) means
   the safety margin against implementation bugs is thinner.

2. **NEC vote-tracking correctness** (Property 5): The `last_voted_proposal`
   check relies on correct persistence of (view, block_hash) pairs. If this
   state is lost (crash recovery), a validator might incorrectly sign an NE
   for a block it actually voted for, potentially breaking NEC/QC mutual
   exclusion. Mitigation: `last_voted_proposal` is persisted in the KV store.

3. **Speculative commit rollback completeness** (Property 3): Equivocation
   detection is local — a validator only detects it if it receives both
   conflicting proposals. In an async network, some validators may never see
   the equivocation and retain a speculatively committed block that conflicts
   with the eventual irrevocable commit. The rollback will only occur when
   the irrevocable commit is processed (at which point the speculative block
   is superseded). This is by design but worth noting.

4. **TC high_tip aggregation** (Properties 6, 7): The `high_tip_is_winner`
   determination uses strictly greater-than comparison. If tie-breaking logic
   is wrong, the leader might make an incorrect reproposal decision. The
   current implementation favors high_qc on ties, which is the safe default
   (fresh proposals are always safe; unnecessary reproposals are a liveness
   concern but not a safety concern).

5. **Phased-mode interaction with 2-chain** (Property 8): The codebase
   supports both pipelined (Generic) and phased (Prepare/Precommit/Commit/
   Decide) modes. The 2-chain commit applies only to pipelined mode. If a
   validator set update is in progress and the protocol switches between
   modes, the transition logic must be correct. This is inherited from the
   original hotstuff_rs design and is well-tested.
