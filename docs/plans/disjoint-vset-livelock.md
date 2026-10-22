# Disjoint validator-set replacement livelock (roadmap item, s136)

**Status: draft, s138.** Static analysis only: no test was run. Code read: main `3410279a` and `d3de0abb`
(merge/s136-batch11, the TC-required set). Line numbers are main's unless prefixed `d3de0abb:`. Roadmap item:
`docs/Torus-hyperBFT-roadmap.md:568`.

Notation: an update U changes `S_old` to `S_new`. `C = S_old ∩ S_new` (continuing members), `J = S_new \ S_old`
(joiners). `P_new(X)` is X's power in `S_new`, `q_new` / `q_old` the quorums (`floor(2P/3) + 1`,
`types/validator_set.rs:153-166`). "Window" = U committed, not decided.

## 1. What is known

- `crates/hotstuff_rs/tests/multiple_validator_set_updates_test.rs:28` is `#[ignore]`d. Step 4 (lines 126-167)
  removes `{n0:1, n1:1, n2:1, n3:3}` and adds `{n4:2, n5:4}`. All six transactions go to n1's queue, so one block U
  carries the whole update (`tests/common/number_app.rs:157-204`). `q_old` = 5 of 6 (n3 plus two others); `q_new` =
  5 of 6, so **both** n4 and n5 must sign. n4 leads 2 views in 6, n5 leads 4 (IWRR, `pacemaker/implementation.rs:1066`).
- The test passes when all six replicas report a committed set of 2. A replica's committed set switches when it
  commits U on U's Commit PC (`block_tree/accessors/internal.rs:796-812`), before the Decide. So the check tests "every
  replica committed U". It does not test "S_new decided U" or "S_new makes progress".
- Evidence of a livelock: one note from 2026-04-15 (memory `bd5953f5`), from the day after the fork (`884f29be`),
  before most of today's consensus code existed. Its explanation, "ActiveCollectorPair can't bridge disjoint sets",
  does not describe today's code (§3.1). The only later run (S426, `f4b52981`, 2026-07-07) died at **step 3** (sizes
  `[3,3,3,3,3,3]`) on the old-set collector revival that s84 fixed (`types/signed_messages.rs:290-300`, test
  `decided_update_never_revives_the_previous_set_collector`). The test has been ignored ever since. **Step 4 has not
  been seen failing on current main or on d3de0abb.**

## 2. How an update runs (both commits unless noted)

| Step | Who signs / acts | Routing | PC checked against | Code |
|---|---|---|---|---|
| U proposed, Prepare votes | S_old | leader(v+1) of the committed set (S_old) | committed set (U pending) | `roles.rs:176-193`, `hotstuff/types.rs:116-124`, d3de0abb:2954-2958 |
| Prepare / Precommit nudges | S_old | same | same | `implementation.rs:862-883`, d3de0abb:909-924 |
| Commit PC(v+2) | S_old, collected by leader(v+3) and the s84 backup leader(v+4) | | | `roles.rs:131-163` |
| Commit U | every replica that runs `block_tree.update` on the Commit PC: CVS := S_new, PVS := S_old, decided := false, update_height := h(U); d3de0abb also update_view := v+2 | carriers: Nudge (`implementation.rs:1559`), NewView to a leader (1793), sync terminal PC (`block_sync/client.rs:590-614`), a PC collected locally | | `internal.rs:796-812`; d3de0abb `internal.rs:362-365, 576-590` |
| | AdvanceView with the Commit PC does **not** commit | | | `pacemaker/implementation.rs:488` → `internal.rs:578-595` |
| Window roles | proposers: CVS and PVS leader; validators (timeout votes, AdvanceView): S_old ∪ S_new; TC: a quorum of either set; Bracha echo counts committed-set power only | | TC: either set | `roles.rs:43-54, 74-82`; `pacemaker/implementation.rs:267, 283-296, 468`; `pacemaker/types.rs:113-126` |
| Decide votes | CVS (S_new) only | leader(w+1) and backup leader(w+2) of S_new | committed set (U committed) | `roles.rs:183-188, 241-243`; `implementation.rs:1201-1222`; `hotstuff/types.rs:84-92` |
| Decide collector | its CVS collector is rebuilt for S_new when it commits U; votes that arrived earlier are dropped (no buffering) | | | `signed_messages.rs:270-283`, `implementation.rs:1565-1570` |
| Decided | Decide PC via `update` or AdvanceView | | | `internal.rs:358-360, 589` |
| First fresh block | leader(w+1) of S_new (main: `is_proposer`; d3de0abb: `fresh_proposer_set`, view > update_view) | | | d3de0abb `types/validator_set.rs:329-335`, d3de0abb `implementation.rs:2918-2935` |

## 3. Root-cause analysis

### 3.1 The happy path completes on both commits

Trace for step 4, static. n1 proposes U in view v. S_old certifies Prepare (v), Precommit (v+1) and Commit (v+2).
L = leader(v+3, S_old) commits U on collecting the Commit PC. It broadcasts AdvanceView(Commit PC)
(`pacemaker/implementation.rs:194-215`) and, as PVS leader, nudges with the Commit PC in v+3 (main: L1 is not needed;
d3de0abb: L1, `highest_pc.view + 1 == v`, d3de0abb:730). Every replica, n4 and n5 included, accepts L as leader(v+3) of
its own committed set S_old (`implementation.rs:1128-1158`). They commit U on the nudge
(`implementation.rs:1536-1566`). n4 and n5 hold U: as non-validators they inserted it from its header and a body
request to the proposer (`implementation.rs:2416-2435`). They also followed the views through other replicas'
AdvanceView messages (`pacemaker/implementation.rs:453-555`). Both vote Decide to leader(v+4, S_new). That replica
committed U on the same nudge, which reached its FIFO channel before the other joiner's vote. It forms the Decide PC
and broadcasts AdvanceView. In v+4 it proposes the first block on the Decide PC.

Nothing on this path needs an old-set member to sign for the new set. Decide routing and collection both use S_new.
So the April explanation does not hold today. Steps 2 and 3 of the same test also need a joiner's Decide vote
(`P_new(C) < q_new`, §4) and pass. A Decide quorum that depends on joiners is therefore not enough to cause the
failure.

### 3.2 D1, main only, every update: a failed window view stalls the window for good

If the first window view yields no Decide PC, main never sends U's Commit PC again:

1. A nudge sets the local tip to (U, current view) (`implementation.rs:1573-1585`, FIX CONS-FIND-27). After the
   view-(v+3) nudge, the tip view v+3 is above highest_pc (Commit PC, v+2). `highest_qc_for_timeout` then returns
   `None` (`internal.rs:1666-1678`).
2. One S_new timeout (power at least 2, above f_new = 1) makes every validator still in the view echo
   (`pacemaker/implementation.rs:275-330`). A TC of either set therefore forms at the replicas that are still in the
   view. Its high tip wins (`pacemaker/types.rs:280-284`).
3. A leader of the next view that holds TC(v-1) takes the TC path before the nudge (`implementation.rs:769-787`).
   Case 4 re-proposes U itself (`implementation.rs:1012-1022`), header-first with `has_validator_set_updates = false`
   (773). The header's justify is U's own justify (view v-1). Receivers are locked on U's Precommit PC (v+1,
   `invariants.rs:498`), so the lock clause rejects it (`invariants.rs:391, 706-718`; header drop
   `implementation.rs:2251-2300`). The leader returns without nudging.
4. No nudge reaches anyone, so no tip changes. Every later TC wins on the same tip, and every window leader
   re-proposes U again.

d3de0abb removes this. Nudges set no tip (d3de0abb:1634-1637). Window tips are pre-update headers, so a window TC's
high QC (the Commit PC) wins. L2 HighQcWins then nudges again (d3de0abb:773-782, 909-924), and each failed window view
is retried in the next view that has a TC.

### 3.3 D2, both commits: a replica that missed U's Commit PC can never commit U

`PhaseCertificate::is_correct` picks the signer set from the replica's own `update_height`
(`hotstuff/types.rs:61-81`). A replica that has not committed U checks U's Decide PC, and every later PC, against its
old committed set. `is_correctly_signed` fails on the signature-set length whenever the member list changed (152-153),
or on the key at a position. With the same members it fails when the Decide signers hold less than `q_old` under the
old powers. The `Pending` arm meant for this case ("received via sync", 94-105) is unreachable: `height ==
update_height` only holds for the last *committed* updating block. Block sync blacklists every server on U's
successor (`block_sync/client.rs:437-442`). AdvanceView never commits (§2). After the Decide nobody sends U's Commit PC
again. Such a replica is stuck for good, and so is any node syncing from genesis across U. That follows from
`docs/pruning-guide.md:45,69`, which says there is no state sync.

Step 4 makes this worst. Old-set replicas have no role after the Decide, so any of them that missed the single
view-(v+3) nudge fails the six-replica check permanently.

### 3.4 Smaller defects on the same path (both commits)

- **D3** `internal.rs:808` (d3de0abb:844) marks `block`, the commit target, Committed instead of `b`, the updating
  block. It only matters when U is committed as an ancestor of another target. That needs a Generic-QC commit across
  an uncommitted U, which D2 blocks for membership changes. No reachable path was found today. It is latent, and the
  fix is one token.
- **D4** `on_receive_nudge` drops a nudge whose block is not in the tree without fetching it
  (`implementation.rs:1536-1555`). The header path fetches by hash (2321).
- **D5** an AdvanceView carrying U's Commit PC only advances highest_pc and never commits
  (`pacemaker/implementation.rs:488`).

### 3.5 Verdict for step 4

- **main:** the most likely livelock is a first window view without a Decide PC, which D1 then makes permanent. The
  size check fails only if some replica also missed the view-(v+3) nudge; otherwise every replica has already committed
  U. Such a replica cannot recover (D2).
- **d3de0abb:** D1 is gone. A failed window view is retried. D2 still strands any replica that missed every window
  nudge.
- **Not pinned:** why view v+3 would fail in step 4 when it succeeds in steps 2 and 3. There are three candidates,
  and §6 T0 decides between them:
  - a joiner skips view v+3 (joiners send no AdvanceView or timeout votes and sync views only through others);
  - a Decide vote reaches the collector before the collector committed U and is dropped (2-of-2 quorum, no slack);
  - the window starts in an epoch-change view (`epoch_length` 50, `tests/common/node.rs:57`), which only a TC or QC
    ends.

## 4. Partial changes

The general conditions:

- **(a) Window completion.** The window needs `q_new` of Decide votes from replicas that committed U in the same view.
  If `P_new(C) >= q_new`, continuing validators finish it alone and joiners may lag. Otherwise it waits on joiners,
  which act only as followers until they commit U. A disjoint update is the extreme case: `P_new(C) = 0`.
- **(b) D1 (main).** Any update whose first window view fails.
- **(c) D2.** After the window, a replica that did not commit U is stranded if U's Decide PC fails against its old
  set. That is the case when the member list changed. It is also the case when the members are the same but the
  Decide signers hold less than `q_old` under the old powers.

The torus epoch rotation cap counts members (`n/3`), not power (`crates/torus-economics/src/epoch.rs:140-215`). A
capped epoch can still produce `P_new(C) < q_new`: one joiner with large power, the step-3 shape.

| Update | Window needs joiners? | D1 (main) | D2 stranding | Verdict |
|---|---|---|---|---|
| One member removed; jailing (removes dead power, at most total - quorum, `jailing-design.md` §4) | no (`C = S_new`) | yes | yes: any live replica that missed the window (the member list shrank) | the window does not need joiners; a lagging member is stranded |
| One equal-power member added to n ≥ 3 equal members | no (`n >= floor(2(n+1)/3)+1`) | yes | yes: the joiner itself if it is still syncing when U commits, and any later node syncing from genesis | the window does not need joiners; the late joiner is stranded |
| Member added to n ≤ 2, or with large power (steps 2 and 3) | yes | yes | yes | needs joiners, like step 4; passes in the test |
| Power change only | no | yes | only if the Decide signers lack `q_old` under the old powers (e.g. 1,1,1,1 → 1,1,1,10) | completes; a laggard is stranded only after a large power shift |
| Exclusion only (dead-leader skip; feat/dead-leader-p2-on-tc keeps members and powers, `types/update_sets.rs` there) | no | yes | no (same keys and powers) | does not hit it; d3de0abb base fine |
| Disjoint (step 4) | yes (`P_new(C) = 0`) | yes | every replica that missed the window; old-set replicas get no later carrier | see §3.5 |

So partial changes never hit the disjoint-specific dependence (no continuing member). They do share D1 on main, and
D2 for every membership change and for large power shifts.

## 5. Fix options

**A. Local fixes.** No message or DB format change and no vote-rule change, so they are not genesis-bound. A mixed
network differs only in which laggards can catch up.

- **A1 (D1):** land d3de0abb.
- **A2 (D2):** in `PhaseCertificate::is_correct` (`hotstuff/types.rs:61-112`), check a Decide PC whose block still
  has Pending updates at this replica against committed ⊕ those updates, whatever the heights. That is what the
  Pending arm intends. The replica then commits U from the Decide PC (`invariants.rs:646-671`), and block sync crosses
  U. The updates come from the replica's own validated block, never from the wire. The trust assumption is today's for
  a replica that committed U: S_new voters vote Decide only after a correct Commit PC.
- **A3 (D3):** `internal.rs:808` and d3de0abb:844: mark `b`, not `block`.
- **A4 (D4, D5):** fetch a nudge's missing block by hash; hand an AdvanceView's Commit PC on a known block to
  HotStuff `update`.

Risk: A2 changes certificate validation, so it needs unit tests (T7) and a Codex torus review. A4 touches the commit
and feed order.

**B. Power-overlap rule for updates.** Every update must keep `P_new(C) >= q_new`, so the window never waits on
joiners.

- Where: the epoch plan (`apply_rotation_cap` by power), and optionally a hotstuff validity check on updating blocks.
- Genesis or P0.0-bound: it changes which update a boundary emits, and the check would change block validity.
- It does not fix D2 (a continuing laggard is still stranded). A disjoint replacement becomes a multi-epoch schedule.

**C. Joint quorum (Raft joint consensus).** U's Prepare, Precommit and Commit PCs need quorums of both sets, so
joiners prove before U commits that they hold U and are live.

- Cost: a new PC shape (two signature sets), joiners voting before they are members, new collectors, a new
  `is_correct` and a `CONSENSUS_DB_FORMAT` bump.
- Genesis-bound, with the highest risk and cost.
- Only worth it if a single-update disjoint replacement is a requirement.

**Recommendation:**

- A before mainnet: A1 lands with batch 11; A2 and A3 as one small branch after the RED tests T3, T5, T7 and T8; A4
  optional.
- B decided with `plans/testnet-genesis-checklist.md`. It is cheap and also covers the step-3 shape.
- C not planned.
- Keep the test ignored until T0 has run on d3de0abb.

## 6. Tests

| Name | Scenario | Prediction |
|---|---|---|
| T0 (run, no code) | the ignored test, 10x each on main and d3de0abb (ubuntoz, repeat-run rule), logging commit/decide/TC per replica; record the failing step and sizes | pins §3.5 |
| T1 `vsu_window_retries_after_a_lost_decide_view` | 4 equal + add 1; the mock-network filter drops every Decide vote of the first window view; expect decided within a few views | RED main (D1), GREEN d3de0abb |
| T2 `disjoint_replacement_completes_and_progresses` | step 4 un-ignored, plus: every replica decided and `number` advances under S_new | after T0 |
| T3 `laggard_crosses_a_membership_change` | 4 equal, add 1; silence a continuing member from before U's Commit PC to after the Decide; then expect it to commit U and catch up by sync | RED both (D2); GREEN with A2 |
| T4 `late_joiner_crosses_its_own_join` | same, with the joiner silenced through the window | RED both (D2) |
| T5 `fresh_node_syncs_across_a_membership_change` | after an add has been decided, start a node from genesis; expect it to reach the tip | RED both (D2) |
| T6 `laggard_crosses_a_power_change` | power only: (a) small change, (b) 1,1,1,1 → 1,1,1,10 | (a) GREEN, (b) RED both |
| T7 `decide_pc_on_pending_update_checked_against_post_update_set` (unit, `hotstuff/types.rs`) | U pending at a height above `update_height`; an S_new-signed Decide PC must pass, an S_old-signed one must fail | RED today |
| T8 `commit_marks_the_updating_block` (unit, `internal.rs`) | commit a target above an uncommitted U; U ends Committed, the target unmarked | RED today |
| T9 `exclusion_only_update_survives_a_laggard` | exclusion-only update with a silenced laggard (dead-leader branch) | GREEN |
| T10 `removal_and_equal_add_complete_with_joiners_silent` | one removal; one equal-power add with every joiner silent through the window; expect decided | GREEN d3de0abb |

## 7. Open questions

1. Why the first window view of step 4 would fail (T0).
2. Is there any non-hotstuff path that lets a node cross a membership change (snapshot import)? If not, D2 blocks
   genesis sync for every node that joins after the first membership change. Run T5 before the testnet adds a 5th
   validator.
3. Option B threshold: `P_new(C) >= q_new` only, or also a bound on joiner power for TC formation?
4. With A2, a replica that commits U from the Decide PC records that PC's view as `update_view` (d3de0abb
   `internal.rs:576-590`), while the others record v+2. That is harmless while the window has no fresh proposals or
   tips, but it should be checked against the F2 tip rule (d3de0abb `types/validator_set.rs:329-335`).

## 8. Reflexive lock rule (s138, fix/vsu-lock-reflexive)

**Bug (D1 at the Precommit stage).** 4 validators, one silent and leading the view after U's Precommit PC. The
backup aggregator forms U's Precommit PC and locks on it; that view times out. A Precommit PC is nudged only in the
next view (`safe_nudge` 4), so later leaders re-propose U (`repropose_block`). U's justify is its parent's PC: older
than the lock and not extending U, so the lock clause (checked on the justify only) made every replica locked on U
refuse U itself. Test: `validator_set_update_survives_a_silent_leader_after_the_precommit_pc` (was 4/4 RED).

**Rule.** For a *proposed block* B with justify J, the lock is satisfied if J satisfies the old clause
(`J.view > locked.view` or J's block extends `locked.block`) **or** B is `locked.block` and B is not committed
(`invariants::safe_proposal_lock`). This is PODC'19 `safeNode` ("node extends lockedQC.node", reflexive). PCs on
their own (nudges, collected PCs, NewView, sync) keep `safe_pc` unchanged: a PC on the locked block already passes
its "extends" arm.

**Enforcement sites (all use the same rule).**
- full proposal path and the recovery re-proposal (`on_receive_proposal`, `on_receive_proposal_response`):
  `safe_block` -> `safe_proposal_pc` (`invariants.rs`);
- header path, non-bypass branch: `safe_proposal_pc`; pending-justify bypass branch: `safe_proposal_lock`
  (`on_receive_proposal_header`);
- vote time on the header path: `cast_header_vote` -> `safe_proposal_lock`;
- block sync: log-only lock warning uses `safe_proposal_lock` (sync never checked the lock; no behavior change).
Unchanged on purpose: `safe_nudge`, collected-PC `safe_pc` / `safe_pc_lock_clause`, NewView `safe_pc`,
`pc_to_lock`, `block_to_commit`.

**Why it is safe.**
1. *A vote counts only for its own block.* A phase vote signs `(chain, view, block hash, phase)`; a vote for B can
   only be aggregated into a PC for B. B is the locked block, so the new votes never certify a block that conflicts
   with the lock. Induction (standard): if a block C is committed and a quorum Q is locked on C (or a descendant) from
   view r on, every PC formed after r certifies a descendant-or-self of C. A PC in view w > r needs an honest voter h
   in Q; h votes for X only if X.justify is newer than h's lock (a PC after r: by induction it extends C, so X does),
   or X.justify extends h's lock, or (new) X **is** h's locked block, which extends C by induction. So the new arm
   keeps the invariant; the commit rules are unchanged.
2. *The hash binds the justify.* `Block::hash(height, justify, data_hash)` is checked before the lock rule (full path
   `is_correct`, header path hash check), so the reflexive arm admits B with its own justify only. "U re-proposed
   with a different justify" (lower or higher view) is a different block U': lower or equal view and not extending
   the lock means it is a sibling and is still refused; a higher-view justify passes the old liveness arm, as before.
3. *Re-processing B's stale justify is a no-op.* After the vote check the replica runs `update` /
   `update_locks_only` on J: highest PC moves only forward, `pc_to_lock` is monotone (strictly greater view), and
   `block_to_commit(J)` only commits ancestors of B (already on the locked branch, and only by valid certificates).
   This is the same processing every replica that is not locked on B already did for re-proposals of B. The
   committed case is excluded: a committed B is never re-proposed by an honest leader (its Commit PC is nudged in any
   view), and re-inserting a committed block or re-applying a Decide justify would touch state for nothing.
4. *Phase votes in a later view.* A Prepare vote for U in view w (after the lock's view) is one vote in w
   (`highest_view_voted`), on U; Precommit / Commit follow through nudges in w+1, w+2 under the unchanged
   consecutive-view rule, so U commits with Prepare(w), Precommit(w+1), Commit(w+2).
5. *TC-required rules (V1-V4).* Unchanged and checked before the lock: the re-proposal still needs QC(w-1) or
   TC(w-1), and with the TC's high QC winning, V3 already allows "the block the high QC certifies".
6. *Signed tips.* Votes on validator-set-updating blocks and on re-proposals set no new tip from the proposal (a TC
   re-proposal keeps the TC's verified high tip); the fresh-proposer gate (`is_fresh_proposer`) still applies to any
   tip-setting Generic vote, including one admitted by the reflexive arm.
7. *Nudges.* Unchanged: `safe_nudge` checks a PC, whose block-is-locked case already passed.
8. *VSU-window fresh-proposer rule.* Unchanged: it decides who may propose fresh; re-proposals and Prepare votes are
   not fresh proposals.
9. *Equivocation / slashing.* Proposal equivocation is keyed by `(view, leader)` with different hashes; a
   re-proposal of B by another leader or in another view is a different key. `DoubleSignEvidence` needs the same
   `(view, phase)` and different hashes; a replica votes once per view, and the reflexive votes are for the same
   hash anyway. Nothing new counts as equivocation.

**Model check.** `tests/stateright_2chain_safety.rs` now has re-proposals (a block certified again in a later view,
children on any parent QC) and the reflexive arm: Agreement holds and the reflexive arm is used
(`reflexive_lock_with_reproposals_upholds_agreement`, views 2..=4 in the suite; views 2..=5 `#[ignore]`d, ~9 min).

No message, signing or DB format change.
