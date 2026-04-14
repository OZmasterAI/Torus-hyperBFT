# Trace 2: Timeout + Reproposal

**Setup**: n=4, f=1. Validators: V1 (honest), V2 (Byzantine, silent),
V3 (honest), V4 (honest). V2 is leader of view 2 but goes offline.

## Initial State

Continuing from a state where block A was proposed at view 1 and received
a QC (QC_A at view 1). Block A is speculatively committed.

- `highest_pc` = QC_A (view=1, block=A.hash, phase=Generic)
- `locked_pc` = genesis_pc (view=0)
- Block A at height 0 in tree
- `local_tip` at all honest validators = TipInfo { block_hash=A.hash, view=1 }

## View 2: V2 (Byzantine) goes silent

### Expected behavior

V2 is the leader of view 2 and should propose a block. Instead, V2 sends
nothing.

### View 2 timeout (pacemaker/implementation.rs:96-161)

After `max_view_time` elapses:
1. `Instant::now() > self.view_info.deadline` -> true
2. Not an epoch-change view -> go to else branch (line 136)
3. Each honest validator (V1, V3, V4) broadcasts a TimeoutVote:
   ```
   TimeoutVote {
       chain_id, view=2,
       signature: sign(chain_id, view=2),
       highest_tc: None,
       local_tip: Some(TipInfo { block_hash=A.hash, view=1, ... }),
       highest_qc: Some(QC_A),
   }
   ```
4. Bracha amplification (pacemaker/implementation.rs:253-281):
   - On receiving f+1 = 2 timeout votes for view 2, each honest validator
     that hasn't already sent its own timeout vote broadcasts one
   - This ensures all honest validators participate even if they haven't
     timed out yet

### TC formation (pacemaker/implementation.rs:290-349)

When any validator collects 3 timeout votes (quorum):
1. `TimeoutVoteCollector.collect()` at `pacemaker/types.rs:171-217`:
   - Tracks high_tip: TipInfo with highest view among all votes
     All votes have local_tip.view=1 -> high_tip = A's TipInfo
   - Tracks high_qc: QC with highest view among all votes
     All votes have highest_qc = QC_A (view=1) -> high_qc = QC_A
   - `high_tip_is_winner`: tip.view=1 vs qc.view=1 -> NOT strictly greater
     -> high_tip_is_winner = **false** (high_qc wins on tie)

Wait — this means the TC says high_qc wins, which means the next leader
should make a FRESH proposal, not repropose. Let me reconsider.

Actually, for the reproposal to trigger, we need high_tip.view > high_qc.view.
In this case both are at view 1, so high_qc wins. The next leader would just
propose a fresh block extending QC_A.

Let me adjust the scenario to make high_tip win. This happens when a block
was proposed and voted on but the QC wasn't formed (e.g., votes were sent
to the wrong recipient or the QC collector crashed).

### Adjusted scenario: Block B proposed at view 2, voted on, but no QC

V2 proposes block B at view 2. Validators V1, V3, V4 receive it, validate
it, and vote. But V2 goes offline AFTER proposing, before collecting votes.
The votes go to V3 (leader of view 3) but view 2 times out before V3 can
use them (V3 isn't the collector for view 2 votes).

Actually, let me look at how vote recipients work. PhaseVotes are sent to
`phase_vote_recipient()` which sends to the next view's leader. So view 2
votes go to V3 (leader of view 3). V3 collects them and might form QC_B.
If QC_B forms, this becomes the happy path.

For the timeout+reproposal case, we need view 2 to genuinely fail. Let me
use the case where V2 is silent (never proposes), so no votes are cast.

In that case:
- local_tip at all honest validators remains at view 1 (A.hash)
- highest_qc remains QC_A (view 1)
- TC forms with high_tip.view=1 and high_qc.view=1
- high_tip_is_winner = false (tie goes to high_qc)

So this is NOT a reproposal case — it's a "fresh proposal extending QC_A"
case. The block is effectively just skipped.

### Correct reproposal scenario

For a true reproposal, we need a situation where a validator voted for a
new block but the QC didn't form, making high_tip.view > high_qc.view.

**Revised setup**: View 1 produces QC_A (view 1). View 2: V2 proposes block
B. V1, V3 vote for B (updating local_tip to B at view 2). V4 doesn't vote
(offline briefly). QC_B doesn't form (only 2 votes, need 3). View 2 times
out.

Timeout votes from V1, V3, V4:
- V1: local_tip = B (view 2), highest_qc = QC_A (view 1)
- V3: local_tip = B (view 2), highest_qc = QC_A (view 1)
- V4: local_tip = A (view 1), highest_qc = QC_A (view 1) [didn't see B]

TC at view 2:
- high_tip: max(B.view=2, B.view=2, A.view=1) = B at view 2
- high_qc: max(QC_A.view=1, QC_A.view=1, QC_A.view=1) = QC_A at view 1
- high_tip_is_winner: 2 > 1 -> **true** (high_tip wins!)

## View 3: V3 reproposals block B

### V3 enters view 3 with TC for view 2

1. `enter_view(view=3)`: V3 sees highest_tc = TC(view=2)
2. `is_proposer(V3, view=3)` -> true
3. `repropose_block(view=3)`: highest_pc=QC_A, phase=Generic -> None
4. Checks TC: `tc.view + 1 = 3 = self.view_info.view` -> true

### create_proposal_based_on_tc (implementation.rs:326-393)

1. `tc.high_tip_is_winner` = true
2. `tc.high_tip` = Some(TipInfo { block_hash=B.hash, ... })
3. Case 4 check: `block_tree.block(&B.hash)?`
   - V3 has block B (received it in view 2) -> Some(block_B)
4. Returns reproposal:
   ```
   Proposal {
       chain_id, view=3,
       block: B,  // same block from view 2
       tc: Some(TC(view=2)),
       nec: None,
   }
   ```
5. `proposal.is_reproposal()` -> tc.high_tip_is_winner=true -> true
6. Broadcasts the reproposal

### Validators receive reproposal of B

1. `is_proposer_with_reputation(V3, view=3)` -> true
2. Equivocation check: no prior proposal in view 3 -> OK
3. **Reproposal validation** (implementation.rs:558-578):
   ```
   proposal.is_reproposal() -> true
   tc_valid = tc.is_correct(block_tree)?       // quorum signatures valid
       && self.view_info.view == tc.view + 1    // 3 == 2+1 ✓
       && tc.high_tip.block_hash == B.hash      // matches reproposed block ✓
   ```
   -> tc_valid = true
4. `safe_block(&B, block_tree, chain_id)`:
   - B.justify = QC_A; `safe_pc(&QC_A)`:
     - Predicate 3: QC_A.view=1 > locked_pc.view=0 -> true
   -> true
5. If B already inserted (from view 2), `block_tree.insert` is idempotent
   or the block is already there. The code proceeds to `update`.
6. `block_tree.update(&QC_A)`:
   - highest_pc already >= QC_A -> no change
   - pc_to_lock: same as before -> no change
   - block_to_commit: genesis parent -> None
7. Vote for B at view 3:
   ```
   PhaseVote(view=3, block=B.hash, phase=Generic)
   ```
8. `highest_view_voted` = 3
9. local_tip NOT updated (reproposal, not fresh):
   `!proposal.is_reproposal()` = false -> skip local_tip update

### QC_B forms at view 3

V4 (leader of view 4) collects 3 votes -> QC_B at view 3.

## View 4: Continuation

With QC_B at view 3 and QC_A at view 1, these are NOT consecutive (3 != 1+1).
So no 2-chain commit yet.

V4 proposes block C with justify=QC_B.

When QC_C forms at view 4:
- `block_to_commit(QC_C)`: parent_justify = block_justify(&C's block) = QC_B
  QC_C.view=4, QC_B.view=3 -> 4 == 3+1 -> **consecutive!**
  commit block = QC_B.block = B.hash
  **IRREVOCABLE COMMIT: Block B committed!**

But wait — block A at height 0 must be committed before B at height 1.
The `commit()` function commits a block AND all its uncommitted ancestors.
So when B is committed, A (B's ancestor via QC_A) is also committed.

## Key Observations

1. **Reproposal preserved safety**: Block B was reproposed with a valid TC
   proving the timeout. The TC's `high_tip_is_winner` flag ensured the
   leader couldn't skip B without justification.

2. **2-chain broke**: Views 1 and 3 both had QCs but were NOT consecutive
   (gap at view 2). So no 2-chain commit after view 3.

3. **2-chain restored**: Views 3 and 4 are consecutive, enabling the commit
   of B (and transitively A).

4. **Tail-fork prevented**: Even though view 2 failed, B was reproposed
   rather than abandoned. A new leader couldn't propose a conflicting block
   C at height 1 without either reproposing B or providing an NEC.

## Adversarial Variant

What if V2 (Byzantine) tries to prevent B from being committed by proposing
a conflicting block B' in view 2?

- If V2 proposes B' to some validators and B to others: equivocation
  detection triggers (implementation.rs:492-555). Validators that see both
  refuse to process the second proposal.
- If V2 proposes only B': some validators vote for B', others for B (if they
  received B first from a different path). Neither gets a QC (split votes).
  The timeout produces a TC with mixed high_tips, and the next leader
  reproposals whichever tip had the higher view.
