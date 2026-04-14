# Code Anchor Map: Safety Properties → Implementation

This document maps each safety property to the exact code locations in
`crates/hotstuff_rs/src/` that enforce it. A human reviewer can inspect
these locations to verify the safety arguments.

## Property → Guard Map

### P1: Agreement (no conflicting commits)

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| Vote-once-per-view | hotstuff/implementation.rs | 659-660 | on_receive_proposal | Checks `highest_view_voted < current_view` before voting |
| Vote-once-per-view | hotstuff/implementation.rs | 783-784 | on_receive_nudge | Same check for nudge voting |
| Locking predicate | block_tree/invariants.rs | 351-365 | safe_pc | Predicate 3: view > locked_pc.view OR extends locked branch |
| Lock update | block_tree/invariants.rs | 421-464 | pc_to_lock | Updates locked_pc based on justify phase |
| Lock extends check | block_tree/invariants.rs | 611-624 | extends_locked_pc_block | Verifies block/parent/grandparent matches locked_pc |
| 2-chain commit | block_tree/invariants.rs | 518-543 | block_to_commit | Commits only on consecutive-view QC pair |
| Consecutive views | block_tree/invariants.rs | 525 | block_to_commit | `justify.view == parent_justify.view + 1` |
| Persist voted view | hotstuff/implementation.rs | 679 | on_receive_proposal | `set_highest_view_phase_voted(view)` |
| Persist lock | block_tree/accessors/internal.rs | 306 | update | `wb.set_locked_pc(&new_locked_pc)` |

### P2: Validity (committed blocks were proposed)

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| Proposer check | hotstuff/implementation.rs | 424-429 | on_receive_msg | `is_proposer_with_reputation(origin, view, ...)` |
| Block correctness | hotstuff/implementation.rs | 597 | on_receive_proposal | `proposal.block.is_correct(block_tree)?` |
| Block safety | hotstuff/implementation.rs | 598 | on_receive_proposal | `safe_block(&proposal.block, block_tree, chain_id)?` |
| App validation | hotstuff/implementation.rs | 625 | on_receive_proposal | `app.validate_block(validate_block_request)` |
| Single insert path | hotstuff/implementation.rs | 627-631 | on_receive_proposal | Only place blocks enter the tree |

### P3: Speculative commit soundness

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| Speculative trigger | hotstuff/implementation.rs | 870-876 | on_receive_phase_vote | 1-QC + fresh proposal = speculative commit |
| Fresh check | hotstuff/implementation.rs | 872 | on_receive_phase_vote | `block_justify.view == new_pc.view - 1` |
| Equivocation detect | hotstuff/implementation.rs | 492-555 | on_receive_proposal | Tracks `seen_proposals` per (view, leader) |
| Rollback on equivoc | hotstuff/implementation.rs | 517-531 | on_receive_proposal | `rollback_speculative_block` if equivocation detected |
| Evidence persist | hotstuff/implementation.rs | 514 | on_receive_proposal | `store_equivocation_evidence` (survives rollback) |
| Promote to irrevoc | block_tree/accessors/internal.rs | 322-326 | update | `promote_speculative_to_irrevocable` on commit |

### P4: Locking correctness

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| Safety clause | block_tree/invariants.rs | 360 | safe_pc | `extends_locked_pc_block(pc, block_tree)` |
| Liveness clause | block_tree/invariants.rs | 360 | safe_pc | `pc.view > locked_pc.view` |
| Lock monotonicity | block_tree/invariants.rs | 459-463 | pc_to_lock | Only updates if new lock != current lock |
| Persist lock | variables.rs | 145 | — | LOCKED_PC stored in KV store |

### P5: NEC/QC mutual exclusion

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| NE vote guard | hotstuff/implementation.rs | 1060-1072 | on_receive_ne_request | Checks `last_voted_proposal` — won't NE if voted for high_tip |
| NE dedup guard | hotstuff/implementation.rs | 1055-1057 | on_receive_ne_request | `ne_sent_views.contains(&view)` prevents double NE |
| NEC quorum | hotstuff/types.rs | 565 | NECollector::collect | `signature_set_power >= quorum` (2f+1) |
| NEC validity | hotstuff/types.rs | 422-441 | valid_nec | View gap check + signature verification |
| QC quorum | hotstuff/types.rs | 374 | PhaseVoteCollector::collect | `signature_set_power >= quorum` (2f+1) |
| Voted-block tracking | hotstuff/implementation.rs | 681 | on_receive_proposal | `set_last_voted_proposal(view, block.hash)` |

### P6: Tail-fork resistance

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| local_tip update | hotstuff/implementation.rs | 684-694 | on_receive_proposal | Sets local_tip for fresh Generic votes |
| Tip in timeout | pacemaker/messages.rs | 130-131 | TimeoutVote | `local_tip: Option<TipInfo>` field |
| TC aggregates tip | pacemaker/types.rs | 183-193 | TimeoutVoteCollector::collect | Tracks highest tip view |
| high_tip_is_winner | pacemaker/types.rs | 196-202 | TimeoutVoteCollector::collect | tip.view > qc.view → must repropose |
| Reproposal path | hotstuff/implementation.rs | 332-342 | create_proposal_based_on_tc | Case 4: repropose if block available |
| Recovery path | hotstuff/implementation.rs | 346-389 | create_proposal_based_on_tc | Case 5: RECOVER if block unavailable |

### P7: Reproposal correctness

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| Reproposal detect | hotstuff/messages.rs | 161-163 | Proposal::is_reproposal | `tc.high_tip_is_winner` |
| TC validation | hotstuff/implementation.rs | 558-578 | on_receive_proposal | Verifies TC correct, view matches, block matches high_tip |
| NEC validation | hotstuff/implementation.rs | 581-594 | on_receive_proposal | `valid_nec(nec, block_tree)?` |
| Phased reproposal | block_tree/invariants.rs | 590-601 | repropose_block | Re-propose on interrupted phased chain |

### P8: 2-chain commit correctness

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| 2-chain rule | block_tree/invariants.rs | 525 | block_to_commit | `justify.view == parent_justify.view + 1` |
| Commit target | block_tree/invariants.rs | 539-541 | block_to_commit | Commits `parent_justify.block` (grandparent) |
| No premature commit | block_tree/invariants.rs | 536-538 | block_to_commit | `not_committed_yet` check |
| Genesis guard | block_tree/invariants.rs | 496-498, 520-522 | block_to_commit | No commit on genesis PC |
| Phased commit | block_tree/invariants.rs | 550-571 | block_to_commit | Commit/Decide phase: commit justify.block |

## Supplementary Guards (cross-cutting)

| Guard | File | Lines | Function | What it does |
|-------|------|-------|----------|--------------|
| Block correctness | types/block.rs | — | Block::is_correct | Hash verification, structure checks |
| PC correctness | hotstuff/types.rs | 48-188 | PhaseCertificate::is_correct | Signature + quorum verification |
| TC correctness | pacemaker/types.rs | 55-116 | TimeoutCertificate::is_correct | Signature + quorum verification |
| View monotonicity | pacemaker/implementation.rs | 472-478 | update_view | `next_view > cur_view` enforced |
| Proposal dedup | hotstuff/implementation.rs | 433-437 | on_receive_msg | ProposalStatus prevents double proposals |
| Bracha amplify | pacemaker/implementation.rs | 253-281 | on_receive_timeout_vote | f+1 timeout votes → broadcast own |
| Backup QC broadcast | hotstuff/implementation.rs | 891-904 | on_receive_phase_vote | Leader broadcasts QC as AdvanceView |
| Recovery cleanup | hotstuff/implementation.rs | 182 | enter_view | `recovery_state = None` on view change |
| Rep success tracking | block_tree/accessors/internal.rs | 324-348 | update | Records leader success on commit |
| Rep timeout tracking | pacemaker/implementation.rs | 302-307 | on_receive_timeout_vote | Records leader timeout on TC |
