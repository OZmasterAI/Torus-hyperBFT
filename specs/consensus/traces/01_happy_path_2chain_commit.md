# Trace 1: Happy Path — 4 Consecutive Blocks Commit via 2-Chain

**Setup**: n=4, f=1. Validators: V1, V2, V3, V4 (V4 is Byzantine but passive).
Leader rotation: V1 -> V2 -> V3 -> V4 -> V1 ...

## Initial State

All validators:
- `locked_pc` = genesis_pc (view 0, block [0;32])
- `highest_pc` = genesis_pc
- `highest_committed_block_height` = None
- `highest_view_entered` = 0
- `highest_view_voted` = None
- Block tree contains only the genesis block at height 0

## View 1: V1 proposes block A

### Leader V1 actions (implementation.rs:144-316)

1. `enter_view(view=1)`: Sends NewView to V2 (next leader)
2. `is_proposer(V1, view=1, ...)` returns true
3. `repropose_block(view=1, ...)` returns None (highest_pc is genesis, no
   interrupted phased chain)
4. No TC at view 0, so skip TC-based proposal
5. `highest_pc.phase` = Generic (genesis) -> produce new block
6. Calls `app.produce_block()` -> gets data for block A
7. `Block::new(height=0, justify=genesis_pc, data_hash, data)` -> block A
8. Broadcasts `Proposal { chain_id, view=1, block=A, tc=None, nec=None }`

### All validators receive Proposal(A) (implementation.rs:477-716)

1. `is_proposer_with_reputation(origin=V1, view=1, ...)` -> true
2. `proposal_status` = WaitingForProposal -> check passes
3. Equivocation check: `seen_proposals` empty -> insert (view=1, V1) -> A.hash
4. Not a reproposal -> skip TC validation
5. No NEC -> skip NEC validation
6. `A.is_correct()` -> true (signatures valid, hash matches)
7. `safe_block(&A, block_tree, chain_id)`:
   - `safe_pc(&A.justify=genesis_pc, ...)`:
     - Predicate 1: genesis_pc -> true
     - Predicate 2: genesis_pc -> true
     - Predicate 3: genesis_pc -> true
     - Predicate 4: genesis_pc is Generic, block A has no VS updates -> true
   - `A.justify.is_block_justify()`: Generic -> true
   - Result: true
8. `app.validate_block(A)` -> Valid
9. `block_tree.insert(A)` -> A is now in the block tree at height 0
10. `block_tree.update(&A.justify=genesis_pc)`:
    - `pc_to_lock(genesis_pc)` -> None (genesis special case)
    - `block_to_commit(genesis_pc)` -> None (genesis special case)
    - No state changes
11. Vote check: `is_phase_voter(V_i, ...)` -> true; `highest_view_voted` = None < 1
12. `vote_phase` = Generic (no VS updates)
13. Creates `PhaseVote(chain_id, view=1, block=A.hash, phase=Generic)`
14. Sends to `phase_vote_recipient` (V2, leader of next view)
15. Sets `highest_view_voted` = 1
16. Sets `last_voted_proposal` = (view=1, A.hash)
17. Updates `local_tip` = TipInfo { block_hash=A.hash, view=1, ... }
18. Sets `proposal_status` = OneLeaderProposed { leader: V1 }

**State after view 1**: All honest validators have block A in tree,
voted for A, locked_pc still = genesis_pc.

## View 2: V2 collects QC for A, proposes block B

### V2 receives phase votes

V2 receives 3 PhaseVotes (from V1, V2, V3) for (view=1, A.hash, Generic).

At the 3rd vote (implementation.rs:831-909):
1. `phase_vote.is_correct(signer)` -> true
2. `phase_vote_collectors.collect(signer, vote)`:
   - After 3rd vote: total_power >= quorum (3 >= 3 for n=4, f=1)
   - Returns `PhaseCertificate { view=1, block=A.hash, phase=Generic, ... }`
   = QC_A
3. `QC_A.is_correct(block_tree)` -> true (3/4 signatures valid, quorum met)
4. `safe_pc(&QC_A, block_tree, chain_id)`:
   - Predicate 1: chain_id matches -> true
   - Predicate 2: A.hash in block_tree -> true
   - Predicate 3: QC_A.view=1 > locked_pc.view=0 -> true
   - Predicate 4: Generic phase, no VS updates -> true
5. `block_tree.update(&QC_A)`:
   - `highest_pc.view=0 < QC_A.view=1` -> set highest_pc = QC_A
   - `pc_to_lock(QC_A)`: phase=Generic -> lock on block_justify(&A.hash)
     = A.justify = genesis_pc. genesis_pc == current locked_pc -> return None
     (no lock change since we'd lock on genesis which is already the lock)
   - `block_to_commit(QC_A)`: phase=Generic ->
     parent_justify = block_justify(&A.hash) = genesis_pc
     genesis_pc -> return None (special case)
   - No commit
6. Speculative commit check: QC_A.phase=Generic, view=1>0,
   block_justify(&A.hash).view = genesis.view=0, but 0 != 1-1=0... wait,
   0 == 0, so `block_justify.view == new_pc.view - 1` = 0 == 0 -> true!
   -> `add_speculative_commit(A.hash)` -- A is speculatively committed

### V2 enters view 2 and proposes

1. `enter_view(view=2)`: `highest_pc` = QC_A (view=1)
2. `is_proposer(V2, view=2)` -> true
3. `repropose_block(view=2)`: highest_pc.phase=Generic -> returns None
4. No TC -> skip
5. `highest_pc.phase` = Generic -> produce new block
6. New block B: `Block::new(height=1, justify=QC_A, data_hash, data)`
7. Broadcasts `Proposal { view=2, block=B, tc=None, nec=None }`

### All validators receive Proposal(B)

1. Checks pass (similar to view 1)
2. `safe_block(&B)`:
   - `safe_pc(&B.justify=QC_A)`:
     - Predicate 3: QC_A.view=1 > locked_pc.view=0 -> true
3. `block_tree.insert(B)` -> B at height 1
4. `block_tree.update(&QC_A)`:
   - highest_pc already = QC_A -> no update
   - `pc_to_lock(QC_A)`: same as above -> None
   - `block_to_commit(QC_A)`: same as above -> None
5. Vote for B: `PhaseVote(view=2, block=B.hash, phase=Generic)`
6. `highest_view_voted` = 2, `local_tip` updated to B

**State after view 2**: Block A speculatively committed. Block B in tree.
locked_pc = genesis_pc. highest_pc = QC_A.

## View 3: V3 collects QC for B, proposes block C

### V3 collects QC_B at view 2

1. QC_B = `PhaseCertificate { view=2, block=B.hash, phase=Generic }`
2. `block_tree.update(&QC_B)`:
   - `highest_pc.view=1 < 2` -> set highest_pc = QC_B
   - `pc_to_lock(QC_B)`: phase=Generic ->
     parent_justify = block_justify(&B.hash) = B.justify = QC_A
     QC_A != current locked_pc (genesis_pc) -> return Some(QC_A)
     **LOCK UPDATE**: locked_pc = QC_A (view=1, block=A.hash)
   - `block_to_commit(QC_B)`: phase=Generic ->
     parent_justify = QC_A (view=1)
     QC_B.view=2 == QC_A.view+1=2 -> **CONSECUTIVE VIEWS** -> commit!
     commit block = parent_justify.block = A.hash
     **IRREVOCABLE COMMIT**: Block A committed!
3. Speculative commit: block_justify(&B.hash).view = QC_A.view = 1,
   QC_B.view - 1 = 1 -> 1 == 1 -> add_speculative_commit(B.hash)
4. Block A promoted from speculative to irrevocable

### V3 proposes block C

1. Block C: `Block::new(height=2, justify=QC_B, data_hash, data)`
2. Broadcasts `Proposal { view=3, block=C }`

### All validators process

- Update locked_pc = QC_A (view=1)
- Commit block A irrevocably
- Vote for C

**State after view 3**: Block A irrevocably committed. Block B speculatively
committed. locked_pc = QC_A (view=1). highest_pc = QC_B (view=2).

## View 4: V4 collects QC for C, proposes block D

### QC_C collected at view 3

1. `block_tree.update(&QC_C)`:
   - highest_pc = QC_C (view=3)
   - `pc_to_lock(QC_C)`: Generic -> parent_justify = QC_B (view=2)
     QC_B != locked_pc (QC_A) -> **LOCK UPDATE**: locked_pc = QC_B
   - `block_to_commit(QC_C)`: Generic ->
     parent_justify = QC_B (view=2), QC_C.view=3 == QC_B.view+1=3 ->
     commit block = QC_B.block = B.hash
     **IRREVOCABLE COMMIT**: Block B committed!
2. Speculative commit: C speculatively committed
3. Block B promoted to irrevocable

### V4 proposes block D (view 4)

1. Block D: `Block::new(height=3, justify=QC_C, data_hash, data)`
2. Even if V4 is Byzantine, it proposes normally here.

### After view 4 (assuming QC_D forms at view 4)

- QC_D (view=4): lock on QC_C (view=3), commit block C
- Block C irrevocably committed
- Block D speculatively committed

## Summary

| View | Leader | Block | QC formed | Lock updated to | Irrevocable commit | Speculative commit |
|------|--------|-------|-----------|-----------------|--------------------|--------------------|
| 1    | V1     | A     | QC_A(v1)  | genesis (no change) | - | A |
| 2    | V2     | B     | QC_B(v2)  | QC_A (v1)       | A (2-chain: v1,v2) | B |
| 3    | V3     | C     | QC_C(v3)  | QC_B (v2)       | B (2-chain: v2,v3) | C |
| 4    | V4     | D     | QC_D(v4)  | QC_C (v3)       | C (2-chain: v3,v4) | D |

Each block is irrevocably committed by the 2-chain rule with 1 view latency
(commit at view v+2 for a block proposed at view v).

## Property Verification

- **Agreement**: All honest validators process the same QCs and reach the
  same commit decisions deterministically.
- **Validity**: All blocks were proposed by valid leaders.
- **Locking**: Lock advances monotonically (genesis -> QC_A -> QC_B -> QC_C).
- **2-chain**: Each commit requires exactly 2 consecutive QC views.
