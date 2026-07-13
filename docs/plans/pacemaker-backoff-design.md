# Design: Adaptive (multiplicative) pacemaker timeout backoff — Task A

> Status: BRAINSTORM (design exploration). Impl plan (`pacemaker-backoff-impl.md`)
> is written only AFTER an option is chosen and Open Question 1 is resolved.
> Consensus-adjacent: ships with a written liveness/safety argument, not just tests.

## Problem (as queued)

`post-erasure-backlog-s466.md` Task A: the pacemaker gives every view a FLAT
`max_view_time` with no backoff on consecutive timeouts; epoch-extend is additive,
not multiplicative. Thesis (val2): the recurring stall-scars **S395 / S426 / S432**
are case-by-case patches of this one missing mechanism, and a single
adaptive-backoff change retires the class.

## Context (verified in `think-dev` this session — source, not the stale doc)

The pacemaker was **rewritten** to a MonadBFT / Lewis-Pye (2021) epoch-grid model.
The doc's cited line numbers (664-668 …) are stale. Verified current facts:

- `PacemakerState.timeouts: BTreeMap<ViewNumber, Instant>` — an **absolute epoch
  grid**. `initial_timeouts` sets view deadline = `epoch_start + max_view_time *
  (view - start + 1)` (`implementation.rs:667`, `:705`). Flat per-view slots.
- `extend_epoch_change_view_timeout` adds exactly **one more flat `max_view_time`**
  (`:724`) — additive, as the doc said.
- **Design tension:** `mod.rs:28-36` states HotStuff v0.4 *deliberately dropped*
  "exponentially increasing view timeouts" because they "completely fail optimistic
  responsiveness," replacing them with the Lewis-Pye epoch-grid. Naive per-view
  multiplicative backoff fights that design.
- **Existing clamp:** `:554` already rebases the remaining schedule when a view's
  deadline is > `2 * max_view_time` ahead of now ("fast-run surplus bounded to 2×").
  Backoff must interact with this clamp deliberately.

### Forensics that complicate the premise (memory, verified)

- **`a0a6940c`** — S395 5.5h park root cause = **absolute-deadline far-future jump**
  (TC jumped view to 642500, ~39k views past committed × `max_view_time` = 5.5h).
  **Already fixed** by the `:554` rebase clamp (commit `15bc833`).
- **`b4ea40d`** (correction) — the *actual* S395 freeze = **quorum topology**: 3
  equal-stake validators ⇒ quorum = floor(2P/3)+1 = ALL 3 (f=0). Losing any one
  validator ⇒ 2-of-3 can never cross an epoch-change boundary ⇒ halt in ~100 views
  (~50s). **Not a timeout-backoff problem.**

⇒ For the *specific* cited incidents, the flat-timeout mechanism was **not** the
proven root cause (one was schedule-jump, already clamped; one was f=0 topology).
Multiplicative backoff addresses a *different*, still-real but **unconfirmed**
failure mode: post-GST **lock-step normal-view timeouts** where honest nodes
re-time-out in unison and the view-overlap window never grows past message delay.

## Options

### Option A — Narrow: multiplicative backoff on the epoch-change extend path only
Change only `extend_epoch_change_view_timeout` from additive (`+ max_view_time`) to
multiplicative on *consecutive* extends of the same epoch-change view; normal-view
grid untouched (Lewis-Pye optimistic responsiveness preserved). Reset when the view
advances.
- **Files:** `implementation.rs` (extend fn + a consecutive-extend counter in
  `PacemakerState`); config threading for `factor`/`cap`.
- **Trade-offs:** + smallest blast radius; targets the epoch-change park directly.
  − only helps the epoch-change view, not lock-step normal-view storms; local
  `Instant::now()`-based extends can diverge between validators (lockstep concern).
- **Effort:** Small · **Risk:** Medium

### Option B — Broad: global consecutive-timeout counter scaling every view
`consecutive_timeouts` in `PacemakerState`, reset on any view-advancing QC/commit;
multiply every view's timeout by `factor^min(consecutive_timeouts, cap)`. Matches
the doc sketch literally.
- **Files:** `implementation.rs` (state field + both schedule builders + the `:554`
  clamp must scale with backoff); config threading.
- **Trade-offs:** + uniform; also covers normal-view lock-step storms. − touches the
  normal-view grid ⇒ risks the optimistic-responsiveness property the module was
  engineered for; largest blast radius; local counter can diverge across validators.
- **Effort:** Medium · **Risk:** High

### Option C — State-derived, lockstep-exact backoff
Derive the multiplier from **consensus-visible state** instead of a local counter:
`multiplier = factor^min(cur_view - highest_qc_view, cap)`. Because every honest
validator sees the same QC state, all derive an *identical* schedule ⇒ lockstep by
construction (directly kills the doc's #1 risk: divergent local schedules waste
views). Naturally resets as QCs catch up.
- **Files:** `implementation.rs` (read highest_qc/committed view at schedule-build
  time; apply multiplier in `initial_timeouts`/`update_timeouts`); config threading.
- **Trade-offs:** + lockstep-exact, no new mutable counter, self-resetting. − "view −
  qc gap" is a proxy for "consecutive timeouts" (needs a proof it tracks stall
  depth); requires QC view available where timeouts are built.
- **Effort:** Medium · **Risk:** Medium

### Option D — Validate-the-failure-mode-first (gate before any of A/B/C)
Before changing production consensus code, first prove flat-timeout lock-step is a
*real current* failure mode: fold a bounded-progress model into the T1.2 Stateright
harness where flat timeouts fail to converge post-partition-heal and backoff
converges; and/or reproduce a lock-step timeout on devnet (kill+restore quorum) with
today's binary. Only implement A/B/C if the model/repro shows the gap survives the
`:554` clamp.
- **Effort:** Small–Medium · **Risk:** Low (spends effort on evidence, not shipped code)

## Recommendation

**Option D first, then Option C** if D confirms the gap. Rationale: the forensics
show the cited incidents had *other* proven causes (schedule-jump already clamped;
f=0 topology), so we should confirm the lock-step-timeout failure mode is real and
survives `15bc833` before touching consensus timing. If confirmed, Option C's
state-derived multiplier is the lockstep-safe way to add backoff without abandoning
the Lewis-Pye responsiveness the module deliberately chose (which Option B risks).

## Open Questions

1. **(Blocking)** Is post-GST lock-step normal-view timeout an observed failure mode
   on the *current* binary, or was every cited stall already covered by `15bc833`
   (schedule-rebase) + validator topology (f≥1 with ≥4 vals)? Resolve via Option D
   or val2 forensics on S426/S432 specifically.
2. `factor` (1.5 vs 2) and `cap` values; interaction with the `:554` 2× clamp.
3. Reset trigger: on commit vs on any view-advancing QC.
4. Lockstep: is a local counter acceptable (A/B) or must the schedule be a pure
   function of block-tree state (C)?
5. Does adding `>4` validators (roadmap T1.4, f≥1) reduce the urgency of Task A by
   removing the f=0 halt path that motivated it?
