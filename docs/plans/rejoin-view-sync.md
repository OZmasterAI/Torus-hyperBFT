# Design: Rejoin view sync (round-skip on an authenticated future-view header)

s74, item 4 Part B. Branch `perf/s74-rejoin-view-sync` (off local main 1ecfc14).

## Problem

A validator that is SIGKILLed and restarted comes back at
`max(highest_view_entered, highest_pc, highest_tc) + 1`. That is below the
survivors' view. With n=3 every vote is needed, so no QC or TC can form while
it lags.

The survivors sit in a backed-off view (up to ~32 s). The rejoiner receives and
validates their current-view proposal header but does not vote on it, because
`header_is_current` is false (`hotstuff/implementation.rs:2125`). The chain
resumes only when the survivors' timer expires.

s74 WAL-cap campaign, after Part A:
- The DB opens in 7.9 s.
- The freeze is still 16–35 s (mean 28 s).
- The remainder is this wait.

## Context (memory + code map, verified)

**Most of the path already exists.** A header for view w > local view is
already processed ahead of time:
- the leader check is against leader(w) (`:967-1001`);
- the integrity check runs;
- the safety check runs: `safe_pc`, or the lock clause alone when the justify
  block is pending;
- `update_locks_only` runs;
- the body fetch runs.

The header is also kept in the `ProgressMessageBuffer`. When the replica
reaches w, the buffered copy is replayed and voted on exactly once. Test
`future_header_recovers_early_and_votes_once_on_buffered_same_view_delivery`,
`:3486`, covers this.

**What's missing is a way for a checked future header to move the replica to w.**

**View ownership.** The pacemaker owns the view. HotStuff's `view_info` is a
copy, synced each loop by `is_view_outdated` (`!=`, `algorithm.rs:185`). A
skip must therefore go through the pacemaker first. `Pacemaker::update_view`
is private and rejects `next <= cur`.

**Vote safety.** Voting is protected by two mechanisms:
- the header path votes only when `header.view == view_info.view` and
  `highest_view_voted < view_info.view`;
- `set_vote_state_atomic` is persisted before the send (s65).

**Gaps the skip must not widen.** The header path never validates `header.tc`
or `header.nec` (the full path does). Epoch-change views
(`view % 100_000 == 0`) exit only with a certificate.

**CONS-FIND-26** (`pacemaker/implementation.rs:344-349`) blocks fallback-TC
catch-up from peers whose tip is ahead. That is a separate gap: no TC exists
at n=3 with one node down.

## Options

### Option A: Header-driven round-skip through the pacemaker (recommended)

**How it works.**
1. In `on_receive_proposal_header`, the existing checks run first: leader(w),
   integrity, safe justify, `update_locks_only`.
2. HotStuff then records a skip request `Some(w)` and keeps the header, but
   only when all of these hold:
   - `header.view > view_info.view`;
   - the replica is a phase voter for the justify;
   - `highest_view_voted < w`;
   - `header.tc` and `header.nec` are both `None`;
   - the skip stays in the same epoch (`epoch(cur) == epoch(w)`). This is
     checked in the pacemaker, which owns `epoch_length`. Landing on an
     epoch-change view is fine, since the timeout path does that too. Leaving
     one needs a certificate.
3. After `on_receive_msg`, `Algorithm` takes the request. It calls a new
   `Pacemaker::skip_to_view(w)`, a thin wrapper over the private `update_view`
   with the same arguments as the AdvanceView path, which keeps the S395
   rebase and the Task A deadline.
4. It then calls `hotstuff.enter_view(pacemaker.query())` and re-dispatches
   the kept header.
5. The header now takes the ordinary current-view path and gets one vote at w.
   Later copies are refused by `highest_view_voted`.

**Files affected.**
- `hotstuff/implementation.rs`: skip request, kept header,
  `take_view_skip`.
- `pacemaker/implementation.rs`: `skip_to_view`.
- `algorithm.rs`: take the request, skip, `enter_view`, re-dispatch.
- Tests.

**Trade-offs.**
- Upside: it reuses every existing safety check and the existing
  once-per-view vote rule. The vote itself goes through unchanged code.
- Upside: it helps exactly the observed case, where the header arrives
  ~32 s before the survivors' timeout.
- Downside: a leader of any future view can pull a replica forward. That is
  liveness, not safety: safety still rests on the lock and once-per-view
  voting. At n=3 (f=0) it is moot today; at n≥4 it is worth bounding.

**Effort:** Medium. **Risk:** Medium (consensus path; guarded; kill switch).

### Option B: f+1 TimeoutVote view sync (plus removing the CONS-FIND-26 guard)

**How it works.** When TimeoutVotes with power > f arrive for a view above
the local one, the replica jumps there (the LibraBFT/Bracha-style rule).

**Trade-offs.** It does not fix the observed freeze:
- the survivors send no TimeoutVote until their 32 s timer expires, which is
  exactly the wait we want to cut;
- earlier votes were lost while the node was down.

It would also change pacemaker behaviour for every lagging replica.

**Effort:** Medium. **Risk:** Medium.

### Option C: Re-send the latest TimeoutVote/TC on peer reconnect

**How it works.** Survivors replay their latest pacemaker certificate to a
peer that reconnects.

**Trade-offs.** At n=3 with one node down there is no TC. A single
TimeoutVote does not move a view without Option B. It also needs a new
connection hook in `torus-network`, which is more code outside `hotstuff_rs`.

**Effort:** Medium. **Risk:** Low/Medium.

## Recommendation

**Option A.** It targets the measured gap (the header arrives and is
validated, and the vote is withheld). It needs no new messages. It keeps the
vote on the existing once-per-view path.

**Kill switch.** Ship it behind a kill switch (default ON, `=0` off,
matching the repo's `TORUS_EXEC_PIPELINE` convention) so the crash A/B can
compare it on one binary.

**Validation, tests first:**
1. HotStuff unit tests (`sync_recovery_tests`). A replica at v-4 gets
   leader(v)'s header and requests a skip to v with no vote yet. There is no
   skip request in these cases:
   - a header carrying a TC or NEC;
   - a replica that has already voted at ≥ v.
2. Pacemaker unit test: `skip_to_view(w)` sets view w with a bounded
   deadline. It refuses (returns false, view unchanged) for `w <= cur` or
   across an epoch boundary.
3. Algorithm-level test (on the `body_before_expiry_tests` fixture, with
   pacemaker and HotStuff at the same view). A replica at v-4 plus leader(v)'s
   header sends exactly one PhaseVote, at view v. After that, none of these
   produce a vote:
   - a replay of the same header;
   - a header for v-1;
   - a second header at v for a different block.
4. Stateright model: 3 replicas with per-replica view, persisted
   `highest_view_voted` and `highest_view_entered`; actions are
   timeout-advance, crash-restart and header-driven skip. Properties:
   - no replica votes twice in a view;
   - vote views strictly increase across restarts;
   - after a restart, all replicas reach a common view without a timeout
     when a current header is delivered.
5. Crash A/B `--crash-at 60`, n=3 per arm, same binary, skip on vs off.
   Score: freeze length and time from `state database opened` to val1's
   first vote. Agreement and the crash gate must pass.

## Not Building (YAGNI)

- Option B and Option C: they don't address the measured gap.
- A skip-distance bound or f+1 evidence rule. At n=3 (f=0) it adds nothing.
  Revisit for n≥4. See Open Questions.
- Validating `header.tc`/`header.nec` on the header path. Instead, the skip
  refuses such headers, and the existing behaviour stays unchanged.
- Changing CONS-FIND-26: a separate issue with no effect at n=3.

## Open Questions

1. **Skip bound for n≥4.** Require the header's justify to be at least our
   highest PC, or cap `w - local`? Not needed for the 3-validator devnet.
2. **Leader(w+1) is the rejoiner** in ~1/3 of views. The skip helps only if
   survivors' votes for w are in its buffer. The A/B will show how often this
   bites (kill node and view vary per cell).
3. **Kill-switch plumbing.** Is it a `hotstuff_rs` `Configuration` field set
   by `torus-node` from env, or read directly? Follow how
   `TORUS_BODY_SERVE_THREAD` and `TORUS_DEFER_PARENT_FEED` are wired.

## Review follow-up (s74, commit after 1da934e)

The read-only review of 1da934e found no safety issue. Changes made:

- **M1 (liveness at n≥4).** `Pacemaker::skip_to_view` now refuses skips of
  more than `MAX_ROUND_SKIP_VIEWS = 16` views. The rejoin gap was 4 views in
  the crash cells. This bounds how far the leader of a far-future view (which
  may be Byzantine at n≥4) can pull an honest replica.
- **M2 (weak model).** The stateright model now has these features:
  - vote decide, persist and send are separate steps, with crashes allowed
    between them;
  - `highest_view_entered` persistence can lag, so a restart can come back at
    or below a voted view;
  - delivery is block-aware;
  - the mutation is send-before-persist, which is caught.

  The regular suite explores to depth 12. An `--ignored` exhaustive check
  (MAX_VIEW 3, 1 crash) passed in about 390 s.
- **L1.** A repeat header from the same leader no longer moves
  `proposal_status` from `OneLeaderProposed` to `AllLeadersProposed`, at both
  header-path sites.
- **L2.** If `enter_view` fails after HotStuff already took view w, the
  header is still re-dispatched.
- **Nits.**
  - `take_sync_needed` is now handled after the re-dispatch.
  - The ineffective `view_skip` condition is removed.
  - The test comments are corrected.
- **Not changed.**
  - L3: a timeout vote and then a phase vote at w. This is not a safety issue
    under the lock-based `safe_pc`, and it already happens within a view.
  - The duplicate body re-request on a repeated header is a pre-existing,
    performance-only issue.

**Note (review):** the skip also fires in the healthy case. When leader(v+1)'s
header arrives before AdvanceView(QC(v)), its justify is QC(v), so the skip
is harmless and saves a round trip.
