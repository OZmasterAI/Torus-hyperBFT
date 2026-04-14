# Trace 3: NEC Recovery — Leader Lacks Block, NEC Forms, Fresh Proposal

**Setup**: n=4, f=1. Validators V1-V4, all honest.
V3 is leader of view 3 but missed view 2's proposal.

## Scenario Setup

- View 1: V1 proposes block A, QC_A forms (view 1)
- View 2: V2 proposes block B (extending A via QC_A)
  - V1 and V2 receive B and vote for it
  - V4 receives B and votes for it
  - V3 does NOT receive B (network partition, message drop)
  - QC_B does NOT form: only V1, V2, V4 voted (3 votes = quorum), but
    votes go to V3 (next leader) who hasn't seen B. V3 cannot validate
    the votes without the block. View 2 times out.

Actually, let me reconsider the vote routing. Votes go to `phase_vote_recipient`
which is the next view's leader. View 2 votes go to V3 (leader of view 3).
V3 receives the votes but cannot process them because it doesn't have block B
in its block tree.

Wait — `on_receive_phase_vote` at `implementation.rs:831-909` collects votes
via the `PhaseVoteCollector` which doesn't require the block to be in the
tree. The collector just accumulates signatures. If a QC forms, THEN
`safe_pc` and `is_correct` check against the block tree.

So V3 would collect QC_B even without having block B! But then:
- `QC_B.is_correct(block_tree)` at line 854: checks if B.hash is in the
  block tree. It's not (V3 doesn't have B). The check at `types.rs:56`:
  `block_height = block_tree.block_height(&self.block)?` returns None.
  With `(None, _)` in the match at line 64, it validates against committed
  validator set. The signatures ARE valid. So `is_correct` returns true.
- `safe_pc(&QC_B, ...)` at line 855:
  - Predicate 2: `block_tree.contains(&QC_B.block) || QC_B.is_genesis_pc()`
    B is NOT in the tree and not genesis -> **false**
  -> safe_pc returns false -> QC_B is rejected by V3.

So V3 cannot use QC_B. View 2 times out for everyone.

### Revised scenario: nobody forms QC_B

V2 proposes B. V1, V4 vote (2 votes). V3 doesn't receive B (0 votes from
V3). V2 doesn't vote for own proposal (leaders typically don't in our impl
— actually, let me check).

Looking at `on_receive_proposal` at `implementation.rs:477`: the leader
processes its OWN proposal via the regular message path? No — the leader
calls `broadcast` which sends to others but not self (typically). The leader
would need to call `on_receive_msg` on its own proposal to vote.

Actually, looking at the code, the leader broadcasts the proposal and does
NOT self-process it. So V2 doesn't vote for B. Only V1 and V4 vote. That's
2 votes, which is less than quorum (3). QC_B doesn't form.

View 2 timeout votes:
- V1: local_tip = B (view 2), highest_qc = QC_A (view 1)
- V2: local_tip = B (view 2, proposed it), highest_qc = QC_A (view 1)
  (wait, does the leader set local_tip for its own proposal? Let me check)
  Looking at `enter_view` proposal path, the leader doesn't explicitly set
  local_tip. Only voters set it at `implementation.rs:684-694`. So V2's
  local_tip might still be A from view 1.
- V3: local_tip = A (view 1), highest_qc = QC_A (view 1)
- V4: local_tip = B (view 2), highest_qc = QC_A (view 1)

TC at view 2:
- high_tip: max of B(view 2), A(view 1), A(view 1) = B at view 2
  (V2 might not have B as local_tip, but V1 and V4 do)
  Actually V2's local_tip: if V2 didn't vote, its local_tip is still from
  view 1. But V1 and V4 have B at view 2. The collector picks the highest:
  high_tip = B's TipInfo (view 2, from V1 or V4's vote)
- high_qc: QC_A (view 1) from everyone
- high_tip_is_winner: 2 > 1 -> **true**

## View 3: V3 is leader, lacks block B, initiates RECOVER

### V3 enters view 3

1. `enter_view(view=3)`: V3 is leader
2. Checks TC: `tc.view=2, tc.view+1=3 == self.view_info.view=3` -> true
3. Calls `create_proposal_based_on_tc(&tc, ...)`

### create_proposal_based_on_tc (implementation.rs:326-393)

1. `tc.high_tip_is_winner` = true
2. `tc.high_tip` = Some(TipInfo { block_hash=B.hash, block_justify=QC_A, ... })
3. Case 4 check: `block_tree.block(&B.hash)?` -> **None** (V3 doesn't have B!)
4. Fall through to Case 5: RECOVER (implementation.rs:346-389)

### RECOVER initiation

1. Compute `kappa = f+1 = 2` (at least one honest responder guaranteed)
2. **Send ProposalRequest** to 2 validators (e.g., V1, V2):
   ```
   ProposalRequest { chain_id, view=3, tc: TC(view=2) }
   ```
3. **Broadcast NERequest** to all validators:
   ```
   NERequest { chain_id, view=3, tc: TC(view=2) }
   ```
4. Store recovery state:
   ```
   RecoveryState::Recovering {
       tc: TC(view=2),
       ne_collector: NECollector::new(view=3, high_tip_qc_view=QC_A.view=1, validator_set)
   }
   ```
5. Return None (no proposal yet — recovery is async)

### Path A: ProposalResponse arrives first

If V1 has block B and responds (implementation.rs:959-986):
1. V1 receives ProposalRequest for view 3
2. `req.view=3 == self.view_info.view=3` -> true (V1 also in view 3)
3. `tc.high_tip.block_hash = B.hash` -> `block_tree.block(&B.hash)?` -> Some(B)!
4. V1 sends ProposalResponse containing B back to V3

V3 receives ProposalResponse (implementation.rs:990-1040):
1. `resp.view=3 == self.view_info.view=3` -> true
2. `is_recovering` = true
3. Verify block matches: `resp.proposal.block.hash == B.hash` -> true
4. Cancel recovery: `recovery_state = RecoveryState::None`
5. Create reproposal:
   ```
   Proposal { view=3, block=B, tc=Some(TC(view=2)), nec=None }
   ```
6. Broadcast the reproposal -> all validators process it as in Trace 2

### Path B: NEC forms (enough NE messages arrive)

If ProposalResponse doesn't arrive in time, NE messages accumulate.

#### NE message processing at each validator

Each validator receives NERequest (implementation.rs:1044-1098):
1. `req.view=3 == self.view_info.view=3` -> true
2. `ne_sent_views.contains(&3)` -> false (first NE request in this view)
3. Check `did_vote_for_high_tip`:
   - V1: `last_voted_proposal` = (view=2, B.hash) BUT req.tc.view=2 and
     tip.block_hash=B.hash -> `voted_view=2 == req.tc.view=2 && voted_block=B.hash`
     -> **true** -> V1 voted for B -> does NOT send NE
   - V3: `last_voted_proposal` = (view=1, A.hash) or None for view 2
     -> `voted_view=1 != 2` -> **false** -> V3 did NOT vote for B -> sends NE
   - V4: `last_voted_proposal` = (view=2, B.hash)
     -> `voted_view=2 == 2 && voted_block=B.hash` -> **true** -> does NOT send NE
   - V2: V2 didn't vote for B (it was the leader, not a voter)
     `last_voted_proposal` view != 2 -> **false** -> V2 sends NE

Wait, V2 proposed B but didn't vote for it. So V2's `last_voted_proposal`
for view 2 is not set. V2 can send an NE.

NE messages that arrive at V3:
- V3: signs NE (view=3, high_tip_qc_view=1)
- V2: signs NE (view=3, high_tip_qc_view=1)

That's 2 NE messages. Quorum for NEC = 2f+1 = 3. We only have 2.
We need a third. V1 and V4 both voted for B, so they won't send NEs.

This means with f=1, if 2 validators (V1, V4) voted for B and 2 (V2, V3)
didn't, we can't get 3 NE signatures. The NEC CANNOT form — which is
correct! Because B actually received 2 votes (close to getting a QC), it's
not safe to abandon it.

For NEC to form, we need at least 2f+1 = 3 non-voters. With only 2 non-
voters, NEC fails. In this case, the leader must wait for the ProposalResponse
(Path A) or the view times out and the next leader tries.

#### Adjusted scenario: NEC CAN form

If only 1 validator voted for B (say only V1), and V2, V3, V4 did not:
- V2 sends NE, V3 sends NE, V4 sends NE -> 3 NE messages -> NEC forms!

V3 receives 3 NE messages (implementation.rs:1103-1182):
1. Each `ne_collector.collect(origin, view=3, high_tip_qc_view=1, sig)`
2. After 3rd NE: total_power >= quorum -> returns NEC:
   ```
   NoEndorsementCertificate { view=3, high_tip_qc_view=1, signatures=... }
   ```
3. Cancel recovery: `recovery_state = RecoveryState::None`
4. NEC proves B never got a QC -> safe to propose fresh block

### Fresh proposal with NEC (implementation.rs:1130-1178)

1. `high_tip_qc = tc.high_tip.block_justify = QC_A`
2. Parent block = QC_A.block = A.hash
3. `child_height = block_height(A.hash) + 1 = 1`
4. `app.produce_block(view=3, parent=A.hash)` -> new data
5. `Block::new(height=1, justify=QC_A, data_hash, data)` = block C
   (C is a DIFFERENT block than B, at the same height 1)
6. Proposal:
   ```
   Proposal { view=3, block=C, tc=Some(TC(view=2)), nec=Some(NEC) }
   ```
7. Broadcast to all validators

### Validators receive fresh proposal with NEC

1. Not a reproposal: `is_reproposal()` -> false (high_tip_is_winner in TC
   but block is different from high_tip) — actually `is_reproposal` checks
   `tc.high_tip_is_winner`, so this would return true. But the block doesn't
   match high_tip, so TC validation at `implementation.rs:562` would fail.
   
   Wait — let me re-examine. `is_reproposal()` checks `tc.high_tip_is_winner`.
   The TC has `high_tip_is_winner=true`, so `is_reproposal()` returns true.
   Then TC validation checks `tc.high_tip.block_hash == proposal.block.hash`.
   Block C != B, so this check FAILS.

   But wait — the proposal also has an NEC. The code at line 558-578 rejects
   the reproposal due to TC mismatch. But then line 581-594 checks NEC.
   However, the code structure is:

   ```
   if proposal.is_reproposal() {
       if tc_valid { ... } else { return Ok(()); }  // REJECTED
   }
   if let Some(ref nec) = proposal.nec {
       if !valid_nec(nec)? { return Ok(()); }  // rejected
   }
   ```

   So if `is_reproposal()` returns true (because TC has `high_tip_is_winner`),
   the code enters the reproposal validation path. Since the block doesn't
   match the high_tip, it's rejected. The NEC check is never reached.

   **This is a potential issue**: a fresh proposal with NEC but where the TC
   has `high_tip_is_winner=true` would be rejected as an invalid reproposal.

   Let me re-read `is_reproposal()` at `messages.rs:161-163`:
   ```
   pub fn is_reproposal(&self) -> bool {
       self.tc.as_ref().map_or(false, |tc| tc.high_tip_is_winner)
   }
   ```

   So if the proposal has a TC with `high_tip_is_winner=true`, it's considered
   a reproposal, regardless of whether the block matches or an NEC is present.

   But in the `on_receive_ne` handler at `implementation.rs:1166-1172`, the
   fresh proposal IS constructed with `tc=Some(tc)` where `tc.high_tip_is_winner`
   is true. This means `is_reproposal()` would return true, and the receiving
   validators would reject it because the block doesn't match the high_tip.

   **This appears to be a bug or at least a design tension**: the sending code
   constructs a valid NEC-backed fresh proposal, but the receiving code
   treats it as a reproposal and rejects it because the block doesn't match.

   **Resolution**: Looking more carefully at the receiver code
   (implementation.rs:558-594), when `is_reproposal()` is true and TC
   validation fails, the code sets `proposal_status` to record that the
   leader has proposed, then returns. The NEC validation at line 581-594
   is only reached if the reproposal check passes OR if `is_reproposal()`
   is false.

   This means NEC-backed fresh proposals from RECOVER would be rejected by
   validators if the TC has `high_tip_is_winner=true`. This is likely
   a bug — the `is_reproposal` check should also consider whether an NEC
   is present (an NEC overrides the reproposal requirement).

   **For this trace, I'll note this as a finding and continue with the
   ALTERNATIVE PATH**: the leader receives the ProposalResponse first
   and reproposals block B directly (Path A above), which works correctly.

## NEC/QC Mutual Exclusion Verification

In the adjusted scenario where only 1 validator voted for B:
- QC for B requires >= 3 voters. Only 1 voted. QC_B cannot form.
- NEC has 3 non-voter signatures. NEC is valid.
- Both cannot coexist: 1 voter + 3 non-voters = 4 = n. No overlap needed.
- If 3 had voted: 3 voters + at most 1 non-voter. NEC needs 3 non-voters.
  Impossible. Mutual exclusion holds.

## Finding: NEC Fresh Proposal Handling

**Severity**: Medium (affects NEC recovery path when high_tip_is_winner=true)

The `is_reproposal()` method at `messages.rs:161-163` checks only
`tc.high_tip_is_winner` without considering whether an NEC is present. When
a leader completes RECOVER with an NEC and creates a fresh proposal, it
includes the TC (which has `high_tip_is_winner=true`). Receiving validators
call `is_reproposal()` which returns true, then validate the TC against the
block — which fails because the fresh block doesn't match the high_tip.

**Impact**: NEC-backed fresh proposals are rejected. The recovery falls
through to the ProposalResponse path (reproposal of the original block)
or the view times out.

**Mitigation**: The ProposalResponse path (Path A) provides a working
alternative. The NEC path is a fallback that currently doesn't complete
successfully due to this issue. This is a liveness concern, not a safety
concern — no invalid blocks are accepted.
