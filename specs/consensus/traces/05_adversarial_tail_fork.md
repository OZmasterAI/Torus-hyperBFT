# Trace 5: Adversarial Tail-Fork Attempt

**Setup**: n=4, f=1. V1 (honest), V2 (Byzantine, leader of views 2 and 5),
V3 (honest), V4 (honest). V2 attempts to create a tail-fork: suppress an
honest block and replace it with its own.

## Background: What is a tail-fork?

A tail-fork occurs when a Byzantine leader:
1. Observes an honest leader's block B getting votes (but no QC yet)
2. In the next view, proposes a DIFFERENT block C that does NOT extend B
3. If C gets a QC, B is "forked off" — the honest leader's work is wasted

MonadBFT prevents tail-forks via reproposal requirements and the local_tip
mechanism.

## Setup Phase

- View 1: V1 (honest) proposes block A, QC_A forms. A speculatively committed.
- View 2: V2 (Byzantine, leader) proposes block B. V1, V3, V4 all vote for B.
  B's QC would form at V3 (leader of view 3). B is speculatively committed.
- **V2's attack begins**: V2 wants to suppress B (maybe B contains
  transactions V2 dislikes) and propose a competing block.

## The Attack

### Phase 1: V2 withholds progress

V2 is the leader of view 2. Instead of proposing B normally, V2:
1. Proposes B to V1, V3, V4 (they vote for it)
2. V2 does NOT vote for its own proposal (leaders don't self-vote in this impl)
3. Votes for B go to V3 (leader of view 3)
4. V2 immediately triggers a timeout — sends TimeoutVote for view 2

V2's timeout vote includes:
- local_tip: V2 doesn't have a local_tip for view 2 (didn't vote for B)
  So V2's local_tip is from a previous view

### Phase 2: V2 tries to cause timeout

V2 wants view 2 to time out so that it can try to influence view 3's
leader. But V3 already has 3 votes for B and can form QC_B.

**Defense: QC formation prevents tail-fork**

V3 collects votes from V1, V3, V4 -> QC_B forms!

Once QC_B exists:
- `block_tree.update(&QC_B)`: highest_pc = QC_B (view 2)
- Lock: `pc_to_lock(QC_B)` -> Generic -> lock on B.justify.block.justify
  = lock on QC_A (block A's QC)

QC_B means B is now part of the certified chain. V2 cannot fork it off.

### Alternative attack: V2 prevents QC_B from forming

V2 would need to prevent 3 validators from voting for B. With n=4, f=1,
V2 can only control its own vote. The other 3 (V1, V3, V4) are honest and
will vote for B if it passes `safe_block`. V2 cannot prevent QC_B.

### Phase 3: V2 tries to fork after view 2

Suppose V2 is leader again at view 5 (due to rotation). V2 tries to
propose a block C that conflicts with B (same height, different branch).

**Defense: safe_pc locking predicate**

By view 5, assuming views 3-4 progressed normally:
- View 3: QC_C forms for block C' (child of B via QC_B)
  Lock: pc_to_lock(QC_C) -> lock on QC_B (B's QC)
  Commit: QC_C.view=3 == QC_B.view+1=3 -> 2-chain! **B irrevocably committed**
- View 4: QC_D forms

When V2 proposes block E at view 5 that conflicts with B:
- E must pass `safe_block` -> `safe_pc(&E.justify)`:
  - Predicate 3: E.justify.view must be > locked_pc.view (now at least view 2)
    AND E must extend locked_pc.block (which is on B's branch)
  - If E conflicts with B, it does NOT extend B's branch
  - So it needs E.justify.view > locked_pc.view
  - If locked_pc.view = 2 (QC_B), E.justify.view must be > 2
  - But B is already irrevocably committed at height 1
  - Honest validators won't insert blocks at height <= committed height
  - E at the same height as B would fail the "not_committed_yet" check
    (if B is committed) or the app validation (duplicate height)

**B is already committed — the attack cannot succeed.**

## Alternative: View 2 genuinely fails (V2 doesn't propose)

If V2 doesn't propose at all:
- No block B in view 2
- View 2 times out
- TC forms with local_tip from view 1 (A's tip)
- high_tip.view = 1, high_qc.view = 1 -> high_tip_is_winner = false
- V3 (leader of view 3) makes a fresh proposal extending QC_A

No tail-fork possible because there's no "tail" to fork — view 2 produced
nothing.

## Alternative: V2 proposes to only SOME validators

V2 proposes B to V1 only (not V3, V4). Only V1 votes for B.

### Timeout

- V1: local_tip = B (view 2), highest_qc = QC_A
- V3: local_tip = A (view 1), highest_qc = QC_A
- V4: local_tip = A (view 1), highest_qc = QC_A

TC at view 2:
- high_tip: B (view 2, from V1) vs A (view 1, from V3, V4) -> B wins
- high_qc: QC_A (view 1) from all
- high_tip_is_winner: 2 > 1 -> true

### V3 must repropose or recover B

V3 is leader of view 3. TC says high_tip is B (view 2). V3 must either:
1. Repropose B (if it has it): V3 doesn't have B -> case 5
2. RECOVER: send ProposalRequest, NERequest

**Defense: Reproposal requirement prevents skipping B**

V3 CANNOT simply propose a fresh block C that ignores B. The TC's
`high_tip_is_winner=true` forces the leader to either repropose B or
obtain an NEC proving B is safe to skip.

NEC formation:
- V1 voted for B -> won't send NE
- V2 is Byzantine -> might send NE (if it helps its attack)
- V3 didn't vote for B -> sends NE
- V4 didn't vote for B -> sends NE

NE count: V3, V4, (maybe V2) = 2 or 3 non-voters.
- If V2 sends NE: 3 NEs -> NEC forms -> fresh proposal allowed
- If V2 doesn't: 2 NEs < 3 (quorum) -> NEC fails -> must wait for
  ProposalResponse or timeout

V1 has block B and responds to ProposalRequest -> V3 gets B -> repropose.

**In either case, B is either reproposed or proven safe to skip (NEC).**
The tail-fork attack fails because:
1. If B is reproposed, it maintains its position in the chain
2. If NEC forms, B provably never got a QC, so skipping it is safe
   (no honest validator will be surprised)

## Property Verification

### Tail-fork resistance

The MonadBFT mechanism prevents tail-forks through three layers:

1. **local_tip tracking**: When validators vote for a block, they record
   it as their local_tip. This tip is included in timeout votes and
   aggregated into the TC's high_tip field.

2. **TC high_tip_is_winner flag**: If the highest tip has a view greater
   than the highest QC, the next leader MUST repropose the tipped block
   or prove it's safe to skip (NEC).

3. **NEC mutual exclusion**: If a block actually received enough votes
   for a QC (2f+1), an NEC cannot form (would need 2f+1 non-voters,
   but total n = 3f+1). So the only blocks that can be skipped via NEC
   are those that genuinely failed to get a QC.

**Code anchors for tail-fork resistance**:
- local_tip update: `implementation.rs:684-694`
- TimeoutVote includes local_tip: `pacemaker/messages.rs:130-131`
- TC aggregates high_tip: `pacemaker/types.rs:183-193`
- high_tip_is_winner: `pacemaker/types.rs:196-202`
- Reproposal requirement: `implementation.rs:332-342`
- NEC validation: `types.rs:422-441`

### Adversarial analysis

The Byzantine leader's options are limited:
- **Equivocate**: Detected by honest validators (Trace 4). Results in
  slashing, not a successful tail-fork.
- **Withhold proposal**: View times out. No tail to fork.
- **Selective proposal**: Some validators see the block, others don't.
  The reproposal mechanism ensures the block is eventually either
  committed or provably abandoned.
- **Send invalid TC**: Validators check `tc.is_correct()` — forged TCs
  are rejected.
- **Collude with future leaders**: Even with a Byzantine leader in a
  future view, the locking mechanism prevents committing conflicting
  blocks (Property 1: Agreement).
