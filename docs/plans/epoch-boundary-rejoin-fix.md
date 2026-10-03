# Fix: epoch-boundary rejoin wedge (restart inside an epoch-change view)

s85. Branch `fix/epoch-boundary-rejoin` (off local main ded6e6c). Crate `hotstuff_rs`.

## Problem

Devnet: 3 validators with equal stake, so every QC and TC needs all 3.
epoch_length is 100. A view v with `v % epoch_length == 0` is an epoch-change
view (`pacemaker/implementation.rs:1280`). A replica can leave one only with a
QC or a TC for it. On timeout it broadcasts a TimeoutVote and extends the view
(`:128-151`, `extend_view` `:698`).

Drill (s85, view 12400): val2 entered 12400 and was SIGTERMed before its vote
left. On restart, `Algorithm::new` (`algorithm.rs:74-81`) booted it at
`highest_view_with_progress + 1` = 12401, in the next epoch, with no
certificate for 12400. val0 and val1 stay in 12400:
- a QC needs val2's phase vote, but val2 only votes on current-view headers;
- a TC needs val2's TimeoutVote(12400), but val2 drops pacemaker messages
  below its view (`networking/receiving.rs:301-303`);
- round-skip refuses to cross an epoch (`skip_to_view`, `:668-681`).

A full restart of all 3 recovers, because all 3 then skip 12400.

## Evidence: do stuck replicas re-broadcast TimeoutVotes? Yes

`tick` (`:128-151`): when an epoch-change view times out, a validator
broadcasts a fresh TimeoutVote and `extend_view` re-arms the deadline at
`now + max_view_time * stall_multiplier` (`:937-950`). So val0 and val1 re-send
TimeoutVote(12400) every 0.5 s in the drill (multiplier 1 while the QC
frontier is at 12399). The s74 note ("survivors emit no TimeoutVote during
their 32 s wait") was about a backed-off NORMAL view, not an epoch-change view.

Why the wedge does not heal on its own: val2 runs through 12401..12499 alone
by local timeouts (the backoff grows to 256 x 0.5 s per view). Its
TimeoutVote(w) for a normal view w does get Bracha-amplified by val0 and val1
(`:275-336`). But val2 has already left w when their votes arrive, so no TC
forms. Only at the next epoch-change view (12500) does val2 stay put, and then
TC(12500) forms. That is hours away at epoch_length 100, and about 100k views
away with the production default.

## Chosen fix: boot back into an epoch-change view that has no certificate

`Algorithm::new` boots at `pacemaker::boot_view(...)`:
- `progress = max(highest_view_entered, highest_pc.view, highest_tc.view)`, as today;
- if `progress` is an epoch-change view that the replica ENTERED, and it holds
  no QC or TC for it (`highest_pc.view < progress` and `highest_tc.view < progress`),
  boot INTO `progress`;
- otherwise, and for a fresh chain (0), keep today's `progress + 1`.

When it re-enters, HotStuff starts IN that view instead of one view behind
(s75 fix A). So the first loop pass does not run `enter_view`, and the
replica does not propose there a second time.

After the restart, the restarted replica is in E with the others, and the
ordinary epoch-change path completes:
- the survivors' re-broadcast TimeoutVote(E) reaches it, and Bracha
  amplification (or its own timeout) makes it send its TimeoutVote(E);
- some replica collects TC(E) and broadcasts AdvanceView(TC);
- everyone enters E+1, and the restarted replica moves forward, never back.

If the others already left E (with QC(E) or TC(E)), the replica catches up as
any slow replica in E does:
- AdvanceView;
- the `highest_tc` fallback carried in their TimeoutVotes;
- a future header whose justify (>= E) raises highest_pc, after which its own
  tick sends an AdvanceView.

### Safety argument

Re-entering a view the replica already entered is safe if each action it can
take there is safe to repeat:
1. Phase vote: guarded by the persisted `highest_view_voted` (written before
   the vote is sent, s65, `hotstuff/implementation.rs:1630-1633`). A replica
   that voted in E cannot vote again in E. The boot rule was never the vote
   guard: `tests/stateright_rejoin_view_sync.rs` already models a restart at
   or below a voted view.
2. Proposal: not repeated. HotStuff starts in E without running `enter_view`.
   No other path proposes in E: `proposal_deferred` is set only by
   `enter_view`, and leader-skip needs a future view in the same epoch. So a
   leader of E never equivocates on restart. If it had not proposed before the
   crash, E ends by TC, which is what an epoch-change view with a silent
   leader does anyway.
3. TimeoutVote(E): the replica signs its current `highest_tc`, `local_tip` and
   `highest_qc`, as on every extension of E. Re-signing is the existing
   behavior.
4. No persisted state moves backwards. `highest_view_entered` stays E, and
   the pacemaker view only rises (TC(E) -> E+1).
5. Liveness is not weakened elsewhere. The rule only fires when the replica
   holds no certificate for E, and without one it may not be in E+1 at all
   (the invariant the pacemaker relies on). Boot is the only path that ever
   broke that invariant.

## Rejected: the replica past E answers TimeoutVote(E) (the preferred direction)

- The restarted replica acts in E+1 before any TimeoutVote(E) reaches it. If
  it leads E+1, the boot `enter_view` proposes at once and it votes for its own
  block (s75 fix A). Its `local_tip` can then point to a block from a view > E.
  The stuck replicas drop a TimeoutVote whose tip is above their view before
  collecting it (`pacemaker/implementation.rs:345-349`, CONS-FIND-26), so in
  that case no TC forms (at n=3 the restarted replica leads 1 view in 3).
  Leaving the tip out instead would under-report a MonadBFT tip
  (`TimeoutCertificate::high_tip`) for E.
- It needs a new past-view exception in the receive filter and in the
  pacemaker collectors, which only collect the current view (`types.rs:199`).
  The boot fix needs neither.

Also rejected: having the replica fetch or derive TC(E). The TC does not exist
yet, because it needs this replica's own vote.

## Tests (written first, RED on the current code)

- `pacemaker::implementation::boot_view_*` (unit): re-enters an uncertified E;
  does not re-enter when a QC or TC >= E is held, when the entered view is not
  an epoch-change view, or when the replica is already past E; handles a fresh
  chain and `epoch_length = 0`.
- `rejoin_view_sync_tests::restarted_replica_reenters_uncertified_epoch_change_view`:
  a tree with `highest_view_entered = 100` boots at 100 with HotStuff not
  outdated (no second `enter_view`). The survivors' TimeoutVotes(100) form
  TC(100), and AdvanceView(TC) moves it to 101.
- `tests/epoch_boundary_rejoin_test.rs` (3 equal-stake nodes, epoch_length 10):
  1. silence val2 for views >= 20 (filter), so it enters 20 but nothing it
     sends from 20 on leaves;
  2. wait until all 3 are in 20, then stop val2 and restart it on the same
     MemDB with the queued inbox;
  3. commits must resume within 60 s. On the current code val2 boots at 21
     and the cluster stays in 20 for about 19 min (backoff).
