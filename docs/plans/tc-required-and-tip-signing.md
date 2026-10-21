# TC required (option B) and tip signing: design

Status: owner decision s134: option B. Pre-genesis. Base: main 28ba931e. Phase A built (feat/tc-required-phase-a);
phase B built (feat/tc-required-phase-b: V1 and V3 always on, high-QC verification, §2 amendments below).
Paths (under `crates/hotstuff_rs/src/`): **PM** = pacemaker/implementation.rs, **PT** = pacemaker/types.rs,
**PMsg** = pacemaker/messages.rs, **HS** = hotstuff/implementation.rs, **HT** = hotstuff/types.rs,
**HM** = hotstuff/messages.rs, **RX** = networking/receiving.rs.

## 1. Current code: what blocks the strict rule

- **No TC on a local timeout.** On timeout a replica broadcasts TimeoutVote(v) and calls `update_view(v+1)` with no
  TC (PM:153-180). `update_view` replaces the timeout collector with one for v+1 (PM:647-652). `filter_message` drops
  pacemaker messages below the current view (RX:301-303), and buffered messages below the view are discarded
  (RX:428-436). So leader(v+1) rarely holds TC(v) at `enter_view` (HS:767-782).
- **A missing TC is accepted.** `valid_proposal_certificates` returns true when `tc` is None (HT:473-475). Both the
  full path (HS:1315) and the header path (HS:2200) use it.
- **Leaders never attach a TC in case 3** (high_qc wins). `create_proposal_based_on_tc` returns None (HS:1091) and the
  fresh proposal hardcodes `tc: None` (HS:914-920). Validators never check that the block extends `tc.high_qc`.
- **Case 5 (RECOVER) has two problems.** It sets the recovery state, then falls through to a fresh `tc: None`
  proposal (HS:784-860). It can then re-propose in the same view (HS:1938-1957), which `seen_proposals` flags as
  equivocation (HS:2151-2183). The validator-set-update (VSU) reproposal also sends `tc: None` (HS:743-765).
- **Unsigned tip view.** A timeout vote signs only the tip hash (PMsg:140-157). `TipInfo.view` (PT:27-35) is
  unsigned, yet the collector ranks tips by it (PT:265-270); PT:74-76 says so. Height, justify and data_hash are
  bound by `block_hash` through `Block::hash` (types/block.rs:73-81), but nothing recomputes that hash.
- **Where local_tip is set:**
  - only on the full-proposal path (HS:1468-1488) and by nudges (HS:1573-1585);
  - `cast_header_vote` never sets it (HS:2740-2783), and that is the production path: proposals go out header-first
    (HS:975-1004); only the NEC proposal is sent as a full Proposal (HS:2112);
  - the nudge tip stores the nudge's QC and view, so it is not the block's header;
  - a reproposal copies `tc.high_tip` into local_tip (HS:1481-1487), so a forged view spreads into honest votes.
- **Earlier probe.** `exp/tc-require-tc-probe` added the strict validator rule on its own and wedged
  `one_down_liveness_test` (no commit in 120 s).

## 2. Change 1: TC required (option B)

### Validator rule (VALIDPROPOSAL)

All of this lives in `valid_proposal_certificates`, which gains a `justify` argument. Phase B enforces it on every
replica (the phase-A test-only switch `tc_required` is gone). A proposal or header for view v with block B is valid
only if:

- **V1.** `B.justify.view == v-1`, or B.justify is the genesis PC, or a TC with `tc.view == v-1` is attached
  (existing check HT:476).
- **V2.** TC attached, no NEC, `high_tip_is_winner` => B is `tc.high_tip` (existing check HT:479-484).
- **V3 (new).** TC attached, no NEC, high_qc wins with `tc.high_qc = q` => `B.justify.view >= q.view` **or B is
  q's block** (`extends_tc_high_qc`). Without V3, a valid TC still lets a leader drop the high_qc block.
  - *Amendment (phase B).* A validator-set-update re-proposal (invariants.rs:686-697) re-proposes the block that
    `highest_pc` certifies, and that block justifies below `q.view` when `q` is that very PC. The rule as first
    written rejected it, so the update could never finish through a timeout view.
- **High QC verified (phase B).** A timeout vote signs only its highest QC's `(view, block)` summary (PMsg:140-157),
  so one byzantine voter could make a forged high-view QC the TC's `high_qc`, and under V3 no honest leader could
  then propose. Two checks, both consistent with `summaries_are_signed` (which requires `high_qc` to be the highest
  signed summary):
  - the pacemaker drops a timeout vote whose `highest_qc` is not a correct QC (`is_correct` under the committed or
    previous set, as for any PC) before echoing or collecting it, so an honest collector forms the TC from honest
    votes only;
  - `TimeoutCertificate::is_correct` requires `high_qc` to be a correct QC, so a TC built by anyone else around a
    forged vote is rejected everywhere (proposals, AdvanceView, a vote's `highest_tc`).
  - Rejected alternative: the TC builder taking the highest *verified* QC. A verifier only sees the signed summaries,
    so it cannot tell a skipped forged QC from a skipped real one; allowing a non-highest `high_qc` would let a
    byzantine builder pick a lower QC and evade V3.
- **V4.** NEC rules are unchanged (HT:465-472).

Nudges are exempt: they carry no new block, and `safe_nudge` already requires `justify.view == v-1` unless the phase
is Commit (block_tree/invariants.rs:439).

### Leader rule (on entering v as leader)

- **L1.** If `highest_pc.view == v-1`, propose as today. Happy path, no wait. The genesis PC also counts, but only
  without TC(v-1): a held TC goes through L2 so its winning tip or high QC is kept (phase-A review P2).
- **L2.** Else, if `highest_tc.view == v-1`, propose via `create_proposal_based_on_tc` and attach the TC in every case
  (`finish_leader_proposal` takes `tc: Option`). In case 3, first apply `tc.high_qc` (as PM:487-489 does); if its
  block is missing, defer and fetch it (HS:806-815). A re-proposed tip (case 4, or case 5 after a ProposalResponse,
  which the leader validates and inserts first) keeps its own `has_validator_set_updates` flag, so validators vote
  Prepare on a validator-set update (phase-A review P1).
- **L3.** Otherwise enter a new `awaiting_tc` state, a sibling of `proposal_deferred`, retried every loop pass via
  `has_deferred_proposal` (algorithm.rs:208-211), which re-checks L1 and L2.
- **L4.** Case 5 never falls through. It waits for a ProposalResponse (reproposal with TC, HS:1938-1957) or an NEC
  (fresh block with TC + NEC, HS:2101-2112). VSU reproposals (invariants.rs:686-697) go through L2/L3.
- **Wait limit: view v's own deadline, no extra timer.** A proposal without a certificate would be rejected, so
  proposing early gains nothing. If neither QC(v-1) nor TC(v-1) arrives in time, view v times out and leader(v+1)
  needs TC(v).

### Collecting timeout votes for v-1 after moving on

- **C1.** `update_view` keeps the outgoing collector as `prev_collector` and drops it on the next `update_view`. It
  is an ActiveCollectorPair, kept current by `update_validator_sets` (PM:185-190).
- **C2.** `filter_message` passes TimeoutVotes for `cur-1` (RX:301-303). `remove_expired_msgs` keeps buffered
  TimeoutVotes for `cur-1` (RX:428-436), so a replica that skips to w replays TV(w-1). The leader's own vote arrives
  by loopback, which HS:931-932 already relies on for headers.
- **C3.** `on_receive_timeout_vote` routes `view == cur-1` to `prev_collector`. Bracha echo stays restricted to
  `view >= cur` (PM:275).
- **C4.** A TC formed this way uses the existing highest_tc and AdvanceView path (PM:381-416), which also pulls
  stragglers into v (PM:529-537).

### Liveness

TC(v-1) forms at leader(v) once quorum power has timed out of v-1. Honest replicas time out when no QC forms
(PM:153-173), and Bracha echo (PM:274-336) sends each replica's vote once f+1 power has voted, so honest votes arrive
within about two one-way delays of the first f+1.

- **One validator down.** The live quorum (3 of 4, or the 2/2 stake in the 2/2/1 test) all time out, so the TC
  forms. If the down node is leader(v), view v is lost as today and leader(v+1) proposes with TC(v).
- **f byzantine and silent.** 2f+1 honest replicas suffice. A byzantine leader that drops the TC is rejected by V1
  and view v times out: one lost view, the same as a silent leader today.
- **Partitions.** With less than a quorum connected there is no QC and no TC, as today. After healing, TC fallback
  (PM:339-342, PM:420-442) and AdvanceView sync views. Entering v via a TC means already holding TC(v-1), so L2
  applies.
- **Epoch change.** Epoch-change views extend instead of advancing (PM:129-151), so leader(E+1) always holds TC(E).
  TC verification accepts the committed or previous validator set (PT:113-126).
- **Restart.** Collectors are in memory (PM:873-889), so a restarted leader's partial votes are lost and its view
  times out. highest_tc is persisted, so L2 still applies when it boots into `highest_tc.view + 1`.
- **Leader-skip** (HS:1825-1858). If the NewView senders entered w via QC(w-1), their highest_pc is applied
  (HS:1787-1806) and L1 holds. If they entered via timeout, C2 keeps their buffered TV(w-1) and L2 holds.
- **Round-skip** (HS:2361-2376) only follows TC-free headers. Under V1 those have `justify.view == w-1`, so it is
  unchanged.
- **Headroom.** The wait fits inside view v's timer: 1200 ms base (torus-types/src/lib.rs:1755-1757), stretched x2
  per stalled view up to cap 8 (PM:625-635, lib.rs:1767-1773).

## 3. Change 2: tip signing

### Options compared

- **(1) Voters sign the tip view.** Proves what the voter claims, not that the block exists. One byzantine voter can
  claim a view-(v-1) tip for a block nobody holds, win high_tip, and force a RECOVER/NE round on every timeout view.
  The copy at HS:1481-1487 then launders that tip into honest votes.
- **(2) Leaders sign headers; the tip is header plus signature.** The view becomes provable, and a second header for
  the same view is transferable equivocation evidence. But `voter_metadata` holds only tip hashes (PT:54-59), so a
  verifier cannot check that `high_tip` is the highest tip among signers. A byzantine leader building the TC can pick
  a lower genuine tip, or let high_qc win, and drop the block 2f+1 voted for. The tail-fork stays open.
- **(3) Both. Recommended.** As read from MonadBFT, this is its shape: a signed tip in the timeout message, and a TC
  carrying per-signer tip views plus the high tip (check against the paper in phase C). Cost: 8 bytes per voter,
  64 bytes per header and per tip, one ed25519 sign per proposal and one verify per header or tip.

### Format changes (pre-genesis, genesis-bound)

- **F1.** ProposalHeader and Proposal gain `leader_signature` over `DOMAIN_HDR || chain_id || view || block_hash`;
  `block_hash` already commits height, justify and data_hash.
  - The domain tag is required. A TimeoutVote with a tip and no QC signs `chain_id || view || tip_hash`
    (PMsg:146-150; ChainID and ViewNumber are both u64, types/data_types.rs:22 and :388). Same 48-byte layout, so an
    untagged header signature would double as the leader's timeout vote.
  - Tag the TimeoutVote, PhaseVote (HM:273) and NE (HT:495) messages too. Today the NE bytes `(view, qc_view)` and a
    bare TimeoutVote `(chain_id, view)` are both u64 pairs.
- **F2.** TipInfo becomes the signed header: `{view, block_hash, height, data_hash, block_justify,
  leader_signature}`. Valid iff `Block::hash(height, block_justify, data_hash) == block_hash` and the signature
  verifies under leader(view) in the committed or previous set.
- **F3.** The timeout-vote signed bytes add `tip.view`, and voter_metadata stores `(hash, view)`.
  `summaries_are_signed` also checks that `high_tip.view` equals the highest signed tip view, that `high_tip`'s hash
  was signed at that view, and that `high_tip` passes F2.
- **F4.** The collector checks F2 before collecting (replacing PM:344-349). A vote with an invalid tip is dropped.
- **F5.** `cast_header_vote` sets local_tip from the header; reproposals copy the verified `tc.high_tip`. Remove the
  nudge write (HS:1573-1585).

If this lands after genesis: the P0.0 feature bits (feat/p00-activation-gating) are per-height bits on the app block
header, not a consensus wire version. Borsh messages would need new variants plus a height-gated switch. Not
recommended.

## 4. Safety

- **Tail-fork closed.** Suppose block B of view v got votes from 2f+1 power, which a QC(v) or a speculative commit
  requires (HS:1729-1740).
  - A valid v+1 proposal either extends QC(v), which keeps B, or carries TC(v) (V1).
  - TC(v)'s signers overlap B's voters in at least f+1, so at least one is honest. That voter's signed tip is B at
    view v (F3, F5), and its `highest_qc` is absent or at most v-1 (block_tree/accessors/internal.rs:1660-1672).
  - So high_tip wins, and V2 forces reproposing B. The alternative, an NEC showing 2f+1 did not vote for B, is
    impossible (HT:401-406).
  - Unsigned views let the TC builder rank B below high_qc: F3 closes that. Optional TCs let the builder skip the TC
    entirely: V1 closes that. V3 closes the QC-winner variant.
  - The argument carries across consecutive timeout views, because a reproposal keeps B as the tip.
- **Speculative-commit undo closed.** A speculatively committed block can then only be undone by leader
  equivocation, the existing rollback trigger (HS:1270-1285). `speculative_commits` has no RPC or exec consumer yet.
- **Commit safety unchanged.** It still comes from the HotStuff lock (`safe_block` / `safe_pc`).

## 5. Performance (derived, not measured)

- **Happy path: no added delay.** leader(v) aggregates the v-1 votes itself (HS:1201-1220), so L1 applies. The header
  grows by 64 bytes and costs one sign and one verify (tens of us).
- **Timeout view: about +0.5 to 1 RTT** (~13 ms LAN, 227-502 ms WAN). Part of it overlaps, because followers already
  wait for their own timeouts before voting.
- **s134 estimates (one of 4 down):** LAN 1.51 -> 1.50 blocks/s; WAN -8% to -13%. A dead-leader view already costs
  1.2-2.4 s today.
- **WAN headroom is thin.** At 502 ms RTT, TC + header + vote is about 1 s of a 1.2 s view; the backoff covers a miss.

## 6. Test plan

**Unit** (`tc_validation_test.rs`, `header_fast_path_regression_test.rs`):
- V1 on both paths: no TC with `justify.view < v-1` rejected; genesis justify accepted; a TC for v-2 rejected.
- V3: a case-3 block below `tc.high_qc` rejected.
- F2/F3 rejections: forged tip view; `block_justify` whose hash does not match; tip signed by a non-leader; header
  signature replayed as a TimeoutVote; TC whose high_tip is not the highest signed view.
- Pacemaker: TV(v-1) after a local timeout forms a TC; buffered TV(w-1) survive a skip to w.
- Leader: case 3 attaches the TC; case 5 waits (no fall-through, no second proposal in the view); `awaiting_tc`
  proposes as soon as a QC or TC arrives.

**Multi-node** (`tests/`):
- `one_down_liveness_test` passes with the strict rule.
- 4 nodes with one down, at each leader slot.
- Byzantine leader that drops the TC: one view lost, then the next view commits.
- Byzantine voter with a forged tip: the TC still forms.
- Tail-fork attempt: B gets 2f+1 votes, the QC is withheld, a fresh-block proposal is rejected, B is re-proposed.
- Leader restarts mid-wait.
- `multiple_validator_set_updates_test` and `epoch_boundary_rejoin_test` with timeouts.
- Update the stateright models (`stateright_lockstep_backoff.rs`, `stateright_rejoin_leader_sync.rs`).

Phase B built the V1/V3 unit cases, the forged-high-QC cases (TC and collector), and in
`tests/tc_required_byzantine_test.rs`: the byzantine leader that drops the TC, the tail-fork attempt, a byzantine
voter with a forged high QC, a leader restart while awaiting TC(v-1), and a validator-set update through timeout views
(5 validators, one down); `tests/tc_required_strict_test.rs` runs 4 nodes with each key down in turn. The
multi-node tail-fork case checks that the fresh block without TC is rejected; that B is then re-proposed needs the TC
to carry B as its high tip, which the header path does not set before F5 (phase C), so phase B checks it on the
validator paths only (`tail_fork_attempt_fresh_block_rejected_tip_reproposed`).

**Mutation checks** (each must fail at least one test): revert V1 to `Ok(true)`; drop V3; unsign `tip.view`; skip
the F2 hash recompute; skip the leader-signature check; drop the domain tag; drop `prev_collector` (expect the
one-down wedge); restore the case-5 fall-through.

## 7. Bench plan

matched-bench (`tools/matched-bench`), main vs branch, same genesis, 3 runs each.

- **Cells:** all-up, and one validator down (stake 2/2/1, kill the stake-1 node, no restart).
- **Harness gap:** `CRASH_KILL_AT_S` always restarts the node (tools/matched-bench/README.md:503-508), so the down
  cell needs a no-restart knob.
- **Report:** blocks/s, matched/s, timeout views per minute, TC wait (enter_view -> propose, a new event field),
  p50/p99 commit latency.
- **Pass:** all-up within noise; one-down LAN blocks/s within -5%.
- **Optional:** WAN cell with netem at 227 and 502 ms.

## 8. Phases and estimates

| Phase | Work | Estimate |
|---|---|---|
| A | `prev_collector`, filter and buffer changes (C1-C4); `awaiting_tc`; attach TC in every case; no case-5 fall-through; apply high_qc. Validator rule stays lax; assert every timeout-view proposal carries a TC. | 2-3 d |
| B | Turn on V1 and V3 (V3 amended, high QC verified); liveness and byzantine tests; mutation set. Built. | 1-1.5 d |
| C | F1-F5, domain tags, tests. | 2-2.5 d |
| D | Bench cells and report. | 1 d |

A Codex torus review runs after each phase. Total about 6-8 working days.

## 9. Open questions for the owner

1. Tip signing: (3) or (2) alone? Recommended: (3).
2. Domain tags on all consensus signatures now? Recommended: yes; pre-genesis is the cheap time.
3. Nudge tips: drop them (recommended) or sign nudges as well?
4. A tip signed under a validator set older than the previous one: drop the vote, or check it against an archived
   set?
5. Reputation leader selection is off by default (PM:1140-1147), but tip checks need every replica to agree on
   leader(view). Keep it off, or tie reputation to committed state?
6. Should a header's valid TC(w-1) also advance a replica that is behind?
7. On WAN at >= 400 ms RTT, raise `timeout_base_ms` above 1200?
8. Bench: add a no-restart kill knob, or use a 4-node equal-stake devnet?
