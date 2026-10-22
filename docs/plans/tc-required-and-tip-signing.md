# TC required (option B) and tip signing: design

Status: owner decision s134: option B. Pre-genesis. Base: main 28ba931e. Phase A built (feat/tc-required-phase-a);
phase B built (feat/tc-required-phase-b: V1 and V3 always on, high-QC verification, §2 amendments below); phase C
built (feat/tc-required-phase-c: F1-F5 and domain tags, §3 "As built").
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
  - Cost: n signature checks per check; a QC equal to the replica's own highest PC (already verified, the common
    case) is not re-verified.
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
  stragglers into v (PM:529-537). It must not count as v's own AdvanceView (`last_advance_view`): otherwise the
  AdvanceView carrying TC(v) is skipped and, in an epoch-change view (extends on timeout), every replica stays in v
  holding TC(v) (found in phase B; fixed there).

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

**As fixed (tc-liveness, s136).** Batch 9 (A+B+C on main) stalled the torus-consensus p00 cluster tests under
`cargo test` (5-6 of 9 failed in one process; main 9/9). Not lost votes: CPU. A node-tagged trace of the CPU-busy
crossing test (debug build, ubuntoz with 24 busy loops) showed one ed25519 check at ~36 ms and the same signatures
checked again on every message (per header 7.5 checks, per timeout vote 6.2; main 3.0 / 2.7): the TC each timeout vote
carries (its signatures, high QC, high tip), the tip and QC every honest vote reports, the header a leader re-sends
every ~270 ms, each AdvanceView copy. Each replica's algorithm thread fell 4-5 s behind its queue (one replica
processed its own TV(7) 4.6 s after sending it), so timeout votes for v-1 reached leader(v) after it had left v, QCs
formed only in 8-16 s backed-off views and never in two consecutive views, and nothing committed.
- **Fix.** All consensus checks go through `crypto_primitives::verify_signature`, which keeps the last 1024 checks that
  passed on its thread, keyed by the exact `key ‖ signature ‖ message` bytes, and skips a check it has seen pass. A
  failed or new check always runs, so every verdict is the one a fresh check gives: what a replica accepts (V1-V4,
  F1-F5, the high-QC checks, the one-fresh-proposer rule) and the §4 argument are unchanged, and a leader still
  proposes in v only with QC(v-1) or TC(v-1). The CPU-busy crossing test then passes in 9 s (main: 37-59 s), with at
  most 1.4 checks per message.
- **Test.** `tests/slow_signature_check_liveness_test.rs` adds 40 ms to every check (test hook): 4 validators all up
  and 3 of 4 must commit height 6 in 90 s (56923063: no commit / height 2 in 60 s; main with the same hook: 15 s /
  27 s; fixed: 9 s / 15 s). Unit: `signature_check_tests`, `timeout_votes_carrying_a_checked_tc_cost_one_signature_check_each`.
- **Not changed (owner, pre-existing on main).** After a backed-off view, the next view entered at multiplier 1 has its
  scheduled deadline in the past (the per-epoch schedule is absolute and only too-far-future deadlines are rebased,
  PM 2b), so it times out at once (`deadline_in_ms=0` in the trace, on main and batch 9 alike). Main recovers its
  consecutive QCs by chance (the leader enters that view by its own timeout and proposes in the same loop pass while
  the others sit in a backed-off view); a slow cluster can lose every such view. Rebasing a past deadline too would
  close it; it changes the view schedule for every chain, so it is left for a separate decision.
- **Rejected.** Re-broadcasting own timeout votes on every timeout (adds messages to queues already seconds behind);
  collecting votes for views further back (leader(v) has left v by the time a late TC(v-1) forms); certificate-driven
  view advance (the votes were not lost, they were late).

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

### As built (phase C)

Owner answers s134: option (3); domain tags on every consensus signature now; drop nudge tips. Open question 4
follows F2 (a tip signed under a set older than the previous one is dropped); 5: reputation stays off, and since the
review (P2) it is refused: `check_leader_selection_mode` makes `ReplicaSpec::start` panic and the node refuse to start
when `reputation_leader_selection` is on, because F2 would pick a tip's leader from today's reputation, not the tip
view's.

- **Signed bytes** (`types/signing.rs`): `DOMAIN_* || borsh(fields)`, tags `torus-hotstuff/v1/{proposal-header,
  phase-vote, timeout-vote, no-endorsement}` (prefix-free). Timeout vote: `(chain_id, view, Option<(tip_hash,
  tip_view)>, Option<(qc_view, qc_block)>)`; the presence bytes keep tip-only and QC-only votes apart. NE now binds
  `chain_id` too (`valid_nec`, `NECollector::new` take it). Block-sync `AdvertiseBlock` stays untagged (not a
  consensus signature; no tagged layout can equal its 16 bytes).
- **F1.** `Proposal::signed`, `ProposalHeader.leader_signature`. A header or proposal not signed by its origin (the
  proposer) is dropped before equivocation detection, so a forged header cannot frame a leader; a full proposal must
  also be for the current view. `ProposalResponse` carries the block only (the recovering leader re-signs).
- **F2.** `TipInfo::is_correct`: `Block::hash` recompute, then the signature of the view's one fresh proposer
  (`fresh_proposer`, §4 "Validator-set update window"): leader(view) of the previous set if `view <= update_view`,
  else of the committed set, decided or not. (First built as "committed or previous set", which let one view have two
  valid tips; review fix s134. The s134 rule also switched to the committed set once the update was decided; s136
  dropped that, see §4.)
- **F3.** `summaries_are_signed`: the high tip is a signed `(hash, view)` at the highest signed tip view, and no
  signed tip is newer than the TC's view. `is_correct` runs F2 on the high tip. `voter_metadata` must cover every
  validator position (the unsigned-base fallback is gone).
- **F4.** The pacemaker drops a timeout vote whose tip fails F2 or is newer than the vote, before echo or collection
  (replaces the CONS-FIND-26 view check).
- **F5.** `cast_header_vote` sets the tip as the full path does: re-proposal (TC high tip wins, no NEC) copies the
  verified `tc.high_tip`; a fresh Generic vote takes the header. The nudge write is gone. VSU (Prepare) votes set no
  tip, as before on the full path.
- **Cost** (18c release, load ~30, `tip_signing_cost_report`): header sign 36 us per proposal (leader), header
  verify 54 us per header or proposal received, `Block::hash` 0.3 us; F2 one verify per tip in a timeout vote (the
  pacemaker and the TC check reuse the validator-set state they already read; a lone F2 with its own state read is
  ~180 us); `TimeoutCertificate::is_correct` with 3 signers 312 us without a tip, 353 us with a high tip. Wire: +64
  bytes per header and per tip, +9 bytes per signer in `voter_metadata` (tip view + option byte).

If this lands after genesis: the P0.0 feature bits (feat/p00-activation-gating) are per-height bits on the app block
header, not a consensus wire version. Borsh messages would need new variants plus a height-gated switch. Not
recommended.

**Stored data (batch 9 merge).** Phase C changes what the consensus DB holds: `LOCAL_TIP` gains the leader signature
(an old record no longer decodes), and every stored PC / TC / NEC was signed over the untagged layout (each fails
`is_correct` under the tagged one). Without a check an old data dir would boot and stall: every proposal on an old
justify and every TC over an old high QC fails verification, and the algorithm thread only logs it. So the block
tree records `CONSENSUS_DB_FORMAT` (variable 25, `CONSENSUS_DB_FORMAT_VERSION` = 1; 2 since dead-leader skip,
docs/plans/dead-leader-skip.md §3 P0.0) at `initialize`, and torus-node's
`open_node_dbs` refuses an initialized consensus DB of any other format (absent = pre-phase-C) before writing
anything: start from a fresh data dir (docs/node-operator-guide.md, "Consensus DB format"). There is no migration;
only local devnets exist.

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
- **Validator-set update window (review fix s134).** While an update is committed but not decided, `is_proposer`
  accepts leader(v) of the committed set (CVS) and of the previous set (PVS), for nudges and re-proposals. If F2 also
  accepted both (as first built, even after the decision), view v could have two valid tips: B from one leader with
  2f+1 votes and X from the other (signed by a byzantine leader, reported by a byzantine voter). The collector keeps the
  first tip at the highest view and `summaries_are_signed` accepts any signed tip there, so TC(v) could name X; X's
  re-proposal fails, an NEC then lets a fresh block drop B, and no rollback fires (equivocation is keyed by
  (view, leader)). Committed blocks stay safe (the lock).
  - *Rule: one fresh proposer per view* (`ValidatorSetState::fresh_proposer_set`, `pacemaker::types::fresh_proposer`).
    The PVS leader for `v <= update_view`, the CVS leader for `v > update_view` (or with no `update_view`: no update
    committed yet). Whether the update is decided does not matter (owner s136). `update_view` (block-tree variable
    24) is recorded when the updating block U is committed: the view of the Commit or Decide PC on U the commit rests
    on (a commit through a descendant uses U's child's justify, U's Decide PC). Honest fresh proposals before it were
    made under the PVS; between U's Commit PC and Decide PC honest leaders only nudge; every block after U justifies
    on U's Decide PC or later.
  - *Why decided-ness dropped out (s136, [claude-review] P3).* The s134 rule switched every view to the CVS leader
    once the update was decided. VSU proposals and nudges set no tip, so every honest tip in the window is a
    pre-update one, signed by the PVS leader at a view `<= update_view`. A replica that had applied the Decide PC
    judged it against the CVS leader: it dropped undecided replicas' timeout votes, rejected every TC whose high tip
    was such a tip (also as the losing tip of a case-3 TC) and every proposal carrying one. A case-3 leader became
    decided by applying the TC's high QC (the Decide PC) and then rejected its own header. Cost: about 1-3 lost views
    per update, more once dead-leader exclusion makes updates frequent. The rule now depends only on the view and
    the committed `update_view`, so it is still exactly one leader per view, and replicas that differ only in
    whether they saw the Decide PC agree on it.
  - *When the PVS is dropped.* The rule reads the PVS for views `<= update_view` after the decision too. Nothing
    changes in storage: the decision only sets `VALIDATOR_SET_UPDATE_DECIDED`; `PREVIOUS_VALIDATOR_SET` and
    `update_view` stay until the next updating block U' is committed, which overwrites the committed set, the
    previous set (with the old committed set), `update_view` and the decided flag in one write batch. From then on
    a tip at a view `<=` the old `update_view` is judged against U''s previous set (the old CVS) and fails unless
    that leader matches: open question 4 (drop, not archive). Such a tip can no longer matter: an honest replica
    holding one omits it from its timeout votes (`own_timeout_summaries`), and the TC argument above needs only the
    tips of blocks still uncertified, which are newer than U''s commit.
  - Applied everywhere `fresh_proposer` is used: F2 (one signature check; the pacemaker's timeout-vote check F4 and
    `TimeoutCertificate::is_correct`); a vote that sets the voter's tip (fresh Generic proposal, NEC proposals
    included, both paths) only for that proposer, judged under the state after the justify (a Decide PC on an update
    this replica has not committed yet is read as if committed; that rebuilt state has no `update_view`, which is
    right because the proposal's view exceeds the justify's, which is at least `update_view`); a leader proposes a
    fresh block (or an NEC block) only if it is that proposer; the timeout blame. Nudges, VSU re-proposals and TC
    re-proposals (which copy the TC's high tip) keep the two-leader `is_proposer` rule.
  - Options rejected: "PVS only while undecided" alone still has two leaders per view in the window. Ranking tied tips
    by signer count fails (X can have up to 2f reporters, B only f+1). Treating both leaders' proposals in one view as
    equivocation would make the undo detectable but TC(v) could still drop B. A single leader per view keeps the
    argument above unconditional, and with dead-leader exclusion (the CVS drops a dead leader) it is the CVS leader
    that proposes once the update takes over.
  - Also from the review: the NE reply goes to the requesting proposer (either leader while undecided, as
    `is_proposer`), not the CVS leader; the timeout blame (reputation bookkeeping) names the view's fresh proposer. The
    justify-block fetch target (`request_missing_parent`) still asks the CVS leader of the justify's view first: a
    hint, with rotation over the committed validators and height-range sync as fallbacks.
  - Residual: a replica that applied U's Decide PC without committing U, and then a Generic PC on U's child before
    the body arrives, judges later proposers under the old set until it commits; this is a liveness lag of that
    replica, and its fresh votes for the old set's leader need 2f+1 such replicas to matter.
- **Speculative-commit undo closed.** A speculatively committed block can then only be undone by leader
  equivocation, the existing rollback trigger (HS:1270-1285). `speculative_commits` has no RPC or exec consumer yet.
- **Commit safety unchanged.** It still comes from the HotStuff lock (`safe_block` / `safe_pc`).

## 5. Performance (derived, not measured; phase C sign/verify measured in §3 "As built")

- **Happy path: no added delay.** leader(v) aggregates the v-1 votes itself (HS:1201-1220), so L1 applies. The header
  grows by 64 bytes and costs one sign and one verify (measured: 36 us and 54 us on a loaded 18c).
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
multi-node tail-fork case checks that the fresh block without TC is rejected, and from phase C (F5) that B is
re-proposed and voted in a later view.

Phase C built the F1-F5 and domain-tag unit cases (`tc_validation_test.rs` "Phase C", `prev_view_collector_test.rs`
F4, `types/signing.rs`) and, in `tests/tc_required_byzantine_test.rs`, a byzantine voter whose forged tips (fields not
hashing to the signed block) must not make any honest leader run a recovery. The VSU case forces the V3-amendment
re-proposal by dropping the first Prepare nudge and asserts it happened.

The review fix (s134) adds `hotstuff/window_tip_leader_test.rs` (F2 per update view and decision, the B-vs-X tie at
TC(v) resolving to B, fresh votes only for the view's fresh proposer incl. a Decide-PC justify on an uncommitted update,
`update_view` recorded at commit, NE reply to the requesting proposer, the full path's drop of a proposal for another
view), two window cases in `prev_view_collector_test.rs` (F4 drops the vote reporting X; timeout blame), the
reputation-mode refusal, and `torus-consensus` slashing: a real phase-vote signature verifies through
`build_vote_message`, which now builds hotstuff_rs's tagged phase-vote bytes.

The s136 window rule (decided-ness dropped) adds, RED on c300a015: in `window_tip_leader_test.rs` F2 per view the same
decided or not (and the committed set without an update view), a TC naming a pre-update tip correct and its
re-proposal voted on both paths by decided and undecided replicas, a case-3 leader decided by the TC's high QC (the
Decide PC) voting for its own header; in `prev_view_collector_test.rs` a decided replica collecting timeout votes
with a pre-update tip and blaming the view's fresh proposer; in `tests/tc_required_byzantine_test.rs`
`validator_set_update_window_timeout_loses_no_further_view` (4 validators, power update, the first fresh proposal
after the update and one validator's timeout votes lost: view w+1 must still get a QC).

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
| C | F1-F5, domain tags, tests. Built. | 2-2.5 d |
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
