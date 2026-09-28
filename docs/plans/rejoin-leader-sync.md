# Design: Rejoin leader sync (fix A — the rejoiner leads the survivors' view)

s75. Branch `perf/s74-rejoin-view-sync` (round-skip B + parked-header C).

## Problem

A validator that is SIGKILLed and restarted comes back at
`max(highest_view_entered, highest_pc, highest_tc) + 1`, which is below the
survivors' view v. When it is **leader(v)**, nobody proposes in v. With n=3
every vote is needed, so the survivors wait out v's backed-off timeout:
- s74 bb-serial-r1 view 423;
- s74 rv-skip-r3 view 570;
- each froze for 19–38 s.

Round-skip (B) cannot help: it jumps on leader(w)'s header, and here the
missing leader is the rejoiner itself.

## Context (code map at 1a8af75, verified)

- **How the survivors reach v.** Each survivor advances by its own local
  timeout (`pacemaker/implementation.rs` `tick`, 103-180). No QC or TC forms
  with one node down. On entering v, each survivor sends
  `NewView{view: v-1, highest_pc}` to leader(v) (`enter_view` step 1, HS
  559-582). This is a direct send, and it happens exactly once.
- **NewView reaches the rejoiner after reconnect.** Direct sends to a
  disconnected peer are queued per peer (256 entries, oldest dropped;
  `swarm.rs` 2749-2771) and flushed on `ConnectionEstablished` (2007-2105).
- **The rejoiner never acts on that NewView.** It is only buffered.
  `filter_message` (`receiving.rs` 268-302) delivers future-view HotStuff
  messages early only for `ProposalHeader` and block data. A future NewView
  waits in `ProgressMessageBuffer` until the local view reaches it. By then,
  the survivors have already timed out.
- **`on_receive_new_view` does very little** (HS 1686-1723). It only runs
  `block_tree.update(highest_pc)` when `safe_pc` holds. It never counts
  NewViews, never changes the view and never proposes.
- **A leader proposes immediately in `enter_view`** (HS 645-728), using its
  own highest_pc, with no NewView quorum. The TC path is taken only when
  `highest_tc.view + 1 == view`, which never happens in pattern A.
- **Boot gap.** `HotStuff::new` starts in the init view without calling
  `enter_view` (ALG 74-100). A rebooted replica therefore never proposes (or
  sends a NewView) in its init view, even when it leads it. This matters when
  init view == v.
- **Survivor acceptance.** A proposal's justify must pass the lock clause
  (`invariants.rs` 382-412). If the rejoiner first applies the survivors'
  NewView `highest_pc` (it needs that block), its proposal carries their
  highest QC, which is safe for them. The header path needs no TC when
  `view > justify.view + 1`.
- **Existing machinery to reuse.** `Pacemaker::skip_to_view` (cap 16 views,
  same epoch, `AdvanceView` deadline rules), and the algorithm-loop hand-off
  that B uses (`take_view_skip` → `skip_to_header_view`).

## Options

### Option 1: NewView-driven leader skip (+ enter the init view at boot) — recommended

- **Receive.** Deliver a future-view `NewView` early, the same way as a
  `ProposalHeader` (`filter_message` exception). Keep it in the buffer too, so
  the existing path is unchanged.
- **HotStuff.** When `round_skip` is on and a NewView arrives for
  `w = nv.view + 1 > local view`, with me == leader(w) and origin a validator,
  do three things:
  1. apply its `highest_pc` exactly as today (`update` if `safe_pc`);
  2. when the QC block is unknown, request it by hash (same path as C);
  3. record a skip request for w once NewViews for w from validators holding
     **power > f** have arrived (n=3: one; n≥4: f+1 distinct).
- **Algorithm loop.** Reuse `skip_to_view(w)`, then `enter_view(w)`, which
  proposes. There is no header to re-dispatch.
- **Boot.** Call `enter_view` for the init view on the first loop pass, so a
  rejoiner that leads its own init view proposes. This covers the
  s == v case, which the skip rule (w > local) does not.

Trade-offs:
- **Pro:** no new message type and no network code. The NewView already
  arrives on reconnect.
- **Pro:** it reuses B's pacemaker path, with its cap and kill switch.
- **Con:** it relies on the queued NewView surviving the 256-entry per-peer
  queue during the downtime. That is unverified.
- **Con:** if the survivors' QC block is missing, the proposal waits for the
  by-hash fetch. That costs milliseconds, as in C.

Files: `networking/receiving.rs`, `hotstuff/implementation.rs`,
`algorithm.rs`, tests (`rejoin_view_sync_tests.rs`, sync_recovery_tests),
stateright model.

Effort: Medium. Risk: Medium (consensus path; forward-only skip; kill switch
`TORUS_ROUND_SKIP`).

### Option 2: Option 1 plus survivors re-send their NewView while waiting

While `WaitingForProposal`, a replica re-sends its last NewView to leader(v)
every ~2 s. This is insurance for when the queued copy was dropped or sent
before the rejoiner's connection was up.
- **Pro:** robust without a reconnect hook.
- **Con:** one extra direct message per replica every 2 s during stalls. It
  also changes behaviour on every replica, not only on the rejoiner.

Effort: Small on top of Option 1. Risk: Low.

### Option 3: Reconnect hook — re-send the latest NewView on `ConnectionEstablished`

This needs a new `Network` method to surface connection events to the
algorithm thread (torus-network `swarm.rs` 2039, `bridge.rs`). It is precise,
but it adds cross-crate plumbing that the existing queue flush already
mostly provides.

Effort: Medium. Risk: Low/Medium.

## Recommendation

**Option 1.** It targets the measured gap: the NewView arrives and is ignored.
It adds no messages or network code, and it reuses B's guarded skip.

Add Option 2 **only if** the multi-crash A/B shows pattern-A freezes with no
`leader-skip` log line. That would mean the queued NewView never arrived.
The new code logs every leader-skip, so the next run answers this directly.

## Not Building (YAGNI)

- **Option 3.** The queue flush already re-delivers direct messages on
  reconnect.
- **NewView signatures.** NewView is unsigned, but the origin is
  authenticated by the transport. The skip moves only forward, is capped at
  16 views and stays within an epoch. The power > f rule bounds a single
  Byzantine node at n ≥ 4.
- **Counting NewView quorums for proposing.** A leader proposes on its own
  highest_pc today. That is unchanged.

## Open Questions

1. **Queue survival.** Does the survivors' NewView survive the per-peer
   256-message queue during a 20–40 s downtime? The survivors also direct-send
   votes and block data to val1. Measure in the A/B (leader-skip log line).
2. **Boot `enter_view`.** It also sends `NewView{init-1}` and may re-run
   leader recovery. Check that the `highest_view_proposed` guard covers a
   crash between proposing and persisting the entered view. The stateright
   model should include this. **Answered (Model, below):** there is no such
   window. `enter_view` persists the entered view before it proposes.
3. **Review finding 4 (B+C gap).** A future header with an unknown justify
   block neither parks nor skips, so a rejoiner behind on both view and blocks
   still waits for the next header. Extend C's parking to future headers, or
   leave it? Decide after the A/B shows whether it occurs.

## Model

`crates/hotstuff_rs/tests/stateright_rejoin_leader_sync.rs` extends the s74
round-skip model (`stateright_rejoin_view_sync.rs`, kept as the B-only
baseline). It uses n=3, f=0 and leader(v) = v mod 3, and each replica keeps
its own view.
- **enter_view** runs in production order: send NewView{prev} to
  leader(prev+1), then take the view, then persist the entered view, then
  propose if leader. The persist and the proposal are separate steps, so a
  crash can fall between them.
- **Delivery** is queued: a NewView can arrive late, repeatedly, or after the
  receiver restarts.
- **Boot:** the replica restarts at entered+1. With fix A it runs
  enter_view(init); without fix A it stays silent in init.
- **Leader skip:** the rule from step 4 of `on_receive_new_view`.
- **Kept from the s74 model:** round-skip B, vote decide/persist/send as
  separate steps, and a Byzantine extra block per view.

Results:
- **Safety holds.** Proposal views strictly increase per replica, across
  restarts, and so do sent vote views. This holds with fix A and round-skip
  on, and with both off. The regular suite runs to depth 12 in ~1 s. The
  `--ignored` exhaustive run (views ≤ 3, 1 crash) takes ~131 s and
  explores 14.3 M unique states.
- **Coverage is reached.** A restarted replica proposes after a leader skip,
  and a restarted replica proposes at boot. Without fix A, neither ever
  happens.
- **The mutation is caught.** `propose_before_persist` fails the proposal
  property in 5 steps: propose in v, crash, restart in v, then the boot
  enter_view proposes again. Without fix A it is also caught (restart below
  v, then time out into v). So the persist order already guarded proposals;
  fix A only makes the second proposal immediate.
- **Scenarios** (s74 v423/v570): the rejoiner restarted at v−2 skips on the
  queued NewView and proposes in v, and the rejoiner restarted at v proposes
  at boot. Neither needs a timeout, and neither happens without fix A.

Residual risk (same class as votes): the entered-view write is not fsync'd.
SIGKILL keeps it, but power loss can lose it. After a power loss, the leader
can propose a second block in v. Other replicas then record equivocation
evidence against an honest node. Fsyncing that write before proposing would
close this.
