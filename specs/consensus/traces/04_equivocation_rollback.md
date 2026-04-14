# Trace 4: Equivocation + Speculative Rollback

**Setup**: n=4, f=1. V1 (honest), V2 (Byzantine leader of view 2),
V3 (honest), V4 (honest).

## Initial State

- View 1 completed: block A proposed by V1, QC_A at view 1
- Block A speculatively committed
- highest_pc = QC_A (view 1), locked_pc = genesis_pc
- All honest validators in view 2

## View 2: V2 (Byzantine) equivocates

### V2's attack

V2 proposes TWO different blocks in view 2:
- Block B to validators V1, V3 (or some subset)
- Block B' to validator V4 (or a different subset)

Both B and B' have:
- height = 1
- justify = QC_A (same parent)
- Different data/data_hash -> different block hash

### V1 receives block B first

1. `on_receive_proposal(B, origin=V2)` at `implementation.rs:477`
2. Equivocation check at `implementation.rs:493`:
   - `seen_proposals` has no entry for (view=2, V2) -> insert (2, V2) -> B.hash
3. Proposal validates: `safe_block`, `is_correct`, etc.
4. Block B inserted into tree
5. V1 votes for B at view 2
6. `highest_view_voted` = 2
7. `local_tip` = B (view 2)
8. Speculative check will happen when QC_B forms (later)

### V4 receives block B' first

Same flow as V1 but with B'. V4 votes for B'.

### V3 receives BOTH B and B' (adversarial scenario)

V2 sends B to V3 first, then B' to V3 (or messages arrive in that order).

#### Processing B

1. `seen_proposals` empty -> insert (view=2, V2) -> B.hash
2. B validates, inserted, V3 votes for B

#### Processing B' (implementation.rs:492-555)

1. `seen_proposals.get(&(view=2, V2))` -> Some(B.hash)
2. `B.hash != B'.hash` -> **EQUIVOCATION DETECTED!**

3. Create EquivocationEvidence:
   ```
   EquivocationEvidence {
       view: 2,
       leader: V2,
       block_a: B.hash,
       block_b: B'.hash,
   }
   ```

4. Publish `Event::EquivocationDetected`

5. Store evidence persistently:
   `block_tree.store_equivocation_evidence(&evidence)` at line 514
   (persisted at EQUIVOCATION_EVIDENCE key — survives rollback)

6. Check if first block (B) was speculatively committed:
   `block_tree.is_speculatively_committed(&B.hash)?`
   At this point B is NOT yet speculatively committed (QC_B hasn't formed).
   -> false -> no rollback needed yet

7. Check if B' is speculatively committed: also false

8. Return early — B' is NOT processed further (line 550: `return Ok(())`)

### QC formation attempt

Votes cast:
- V1 votes for B
- V3 votes for B (before seeing B')
- V4 votes for B'

Votes go to V3 (leader of view 3). V3's PhaseVoteCollector:
- For (B.hash, Generic): V1's vote, V3's vote = 2 votes
- For (B'.hash, Generic): V4's vote = 1 vote

Neither reaches quorum (3). No QC forms.

### View 2 times out

Timeout votes include:
- V1: local_tip = B (view 2), highest_qc = QC_A
- V3: local_tip = B (view 2), highest_qc = QC_A
- V4: local_tip = B' (view 2), highest_qc = QC_A

TC forms at view 2:
- high_tip: max(B.view=2, B.view=2, B'.view=2) = one of them at view 2
  (they're all view 2, so the first one inserted wins)
- high_qc: QC_A (view 1)
- high_tip_is_winner: 2 > 1 -> true

## View 3: V3 attempts recovery/reproposal

Since V3 detected equivocation, it knows V2 is Byzantine. V3 proposes for
view 3 and must deal with the high_tip from the TC.

V3 has block B locally (received and inserted it before detecting
equivocation). If TC's high_tip references B, V3 can repropose B. If it
references B', V3 needs to recover B' or get an NEC.

In either case, the safety properties hold because:
1. Neither B nor B' got a QC (split votes)
2. Any reproposal will be validated against `safe_block`
3. The equivocation evidence is persisted for future slashing

## Alternative: What if B DOES get a QC?

Suppose V2 is smarter: it sends B to V1, V3, V4 (all of them vote for B)
and B' to nobody initially. QC_B forms at view 2.

### Speculative commit of B

At `implementation.rs:870-876`:
- QC_B.phase = Generic, view = 2 > 0
- block_justify(&B.hash).view = QC_A.view = 1
- QC_B.view - 1 = 1 == 1 -> true
- `add_speculative_commit(B.hash)` -> **B is speculatively committed**

### V2 sends B' later

After B is speculatively committed, V2 sends B' to V1 (or V3/V4).

V1 receives B' in `on_receive_proposal`:
1. `seen_proposals.get(&(view=2, V2))` -> Some(B.hash)
2. `B.hash != B'.hash` -> **EQUIVOCATION DETECTED!**
3. Check if B is speculatively committed:
   `is_speculatively_committed(&B.hash)?` -> **true!**
4. **ROLLBACK** (implementation.rs:517-531):
   ```
   let rolled_back = block_tree.rollback_speculative_block(&B.hash)?;
   if rolled_back {
       Event::RollbackBlock(...).publish(...);
       app.on_speculative_rollback(B.hash, &evidence);
   }
   ```
5. B is removed from the speculative commits set
6. App is notified to revert any state changes from B

### Rollback mechanics (block_tree/accessors/internal.rs:1384-1408)

`rollback_speculative_block(&block_hash)`:
1. Read current SPECULATIVE_COMMITS set
2. If block_hash is in the set: remove it, write back, return true
3. If not: return false

The speculative commit set is separate from the irrevocable committed
blocks. Rollback only affects speculation, not the permanent chain.

### After rollback

- B is still in the block tree (not deleted — it has a valid QC)
- B is no longer marked as speculatively committed
- If B's 2-chain later completes (QC_B at view 2 + QC at view 3 for a
  block extending B), B would be irrevocably committed despite the
  equivocation. This is correct: irrevocable commit means the block IS
  part of the finalized chain, regardless of leader behavior.
- The equivocation evidence is preserved for slashing V2's stake

## Property Verification

### Speculative Commit Soundness

The rollback demonstrates the property in action:
1. B was speculatively committed (1-QC)
2. Equivocation detected (leader signed two conflicting blocks)
3. Speculative commit rolled back
4. No safety violation: B was never irrevocably committed, so the rollback
   is a correct "undo" of a premature optimistic assumption
5. If B is later irrevocably committed (via 2-chain), the irrevocable commit
   is final and cannot be rolled back — but it's also proven safe by the
   agreement property

### Adversarial Analysis

V2's equivocation creates two possible outcomes:
1. **Split votes, no QC**: Neither B nor B' commits. Next honest leader
   reproposals or makes a fresh proposal. Liveness impact: one view lost.
2. **One QC forms** (e.g., QC_B): B is speculatively committed. If
   equivocation detected, rollback occurs. B may or may not be irrevocably
   committed depending on whether the 2-chain completes.

In neither case is safety violated:
- Agreement: only one block per height is irrevocably committed
- The equivocating leader is detectable and punishable
- Honest validators that detect equivocation refuse the second proposal

### Locking behavior during equivocation

When QC_B forms at view 2:
- `pc_to_lock(QC_B)`: Generic -> lock on block_justify(&B.hash) = QC_A
  locked_pc was genesis -> update to QC_A
- This is the SAME lock update as the happy path — equivocation doesn't
  affect the locking mechanism because the lock is on the grandparent,
  which is the same (QC_A) regardless of whether B or B' was voted on.

When QC_B' (hypothetically) forms at view 2:
- This CANNOT happen if QC_B already formed — quorum intersection prevents
  two QCs for different blocks at the same view (2f+1 + 2f+1 > n)
- So at most one QC forms per view per honest execution
