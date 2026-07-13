# Implementation Plan: Adaptive pacemaker timeout backoff (Task A)

## Design Decision
**Option C — state-derived, lockstep-exact multiplier** (see
`pacemaker-backoff-design.md`). Per-view timeout is scaled by
`factor^min(gap, cap)` where `gap = view − highest_qc_view`. Healthy chain has
`gap ≈ 0 → multiplier = 1 →` schedule byte-identical to today (Lewis-Pye
optimistic responsiveness untouched). Backoff only grows while views outrun QCs
(a genuine stall).

## Why this is safety-trivial (the load-bearing argument)
In HotStuff, view timeouts govern only **when** a view is abandoned — never
**which** block commits. Any timeout schedule is *safe*; only *liveness* needs
timeouts to eventually exceed message delay Δ. So this change **cannot** cause a
safety violation; it can only affect liveness, which it improves. The safety
argument therefore reduces to a **liveness + lockstep** argument (below), not a
consensus-safety re-proof.

### Liveness argument
`multiplier(gap)` is monotonic non-decreasing. After GST, during a lock-step
timeout the gap increases by 1 each failed view, so view duration grows
geometrically until `view_time > Δ`. Once a view lasts longer than Δ, honest
replicas overlap in it long enough to form a QC, which advances
`highest_qc_view`, collapsing the gap → multiplier resets to 1. Convergence is
bounded: `O(log_factor(Δ / base_max_view_time))` timeouts.

### Lockstep argument (the reason Option C over A/B)
`multiplier` is a pure function of `(view, highest_qc_view, factor, cap)`.
`factor`/`cap` are fleet-wide config (identical by genesis). `highest_qc_view` is
a consensus object. Crucially, **backoff is only active during a stall — exactly
when no new QCs are forming — so `highest_qc_view` is FROZEN and identical across
all honest replicas.** Identical inputs → identical schedule → no divergent-schedule
wasted views. (A local counter, as in Options A/B, has no such guarantee.)

## Success Criteria
- Happy path (`gap ≤ 1`) produces the **exact same** deadlines as today — the
  three existing tests (`update_view_on_schedule_keeps_cumulative_deadline:1236`,
  `update_view_fast_run_deadline_bounded:1221`,
  `update_view_jump_rebases_stale_schedule:1197`) stay green.
- Under an induced lock-step timeout, view duration grows geometrically and the
  fleet reconverges without a multi-view park.
- `factor`/`cap` are fleet-wide config; no node-local timing state feeds the schedule.
- A Stateright bounded-progress model: flat schedule fails to converge
  post-partition-heal, backoff converges within the bound.
- Zero new clippy warnings on touched files; `cargo test -p hotstuff_rs` green.

## Grounded anchors (verified in `think-dev` this session)
- `PacemakerConfiguration` — `implementation.rs:614` (add `backoff_factor`, `backoff_cap`).
- `PacemakerState.timeouts` — `:631`; builders `initial_timeouts:653`, `update_timeouts:689` (currently `(view, config)` only — no QC access).
- `update_view:516`; production call sites `:156`, `:412`, `:503` (all have `block_tree`).
- QC frontier: `block_tree.highest_pc()?.view` (already used at `:171`).
- Backoff-undo hazard: the `:554` rebase clamp (`scheduled > now + max_view_time*2`) must be made multiplier-aware.
- `extend_epoch_change_view_timeout:718` (flat add — also gets the multiplier).

## Tasks

### Task 1: Add backoff config knobs (fleet-wide, defaults = active-but-neutral)
- **Test first**: `pacemaker_config_has_backoff_defaults` — construct a
  `PacemakerConfiguration`; assert `backoff_factor == 2` and `backoff_cap == 8`
  (or chosen values). A second assert: `backoff_multiplier(v, v, ..)` (gap 0) `== 1`.
- **Implementation**: add `pub(crate) backoff_factor: u32` and
  `pub(crate) backoff_cap: u32` to `PacemakerConfiguration` (`:614`). Thread from
  `ConsensusConfig`/`ChainConfig` → node config (mirror `max_view_time`'s path).
  Genesis-sourced so it is identical fleet-wide.
- **Verify**: `cargo test -p hotstuff_rs pacemaker_config_has_backoff_defaults`
- **Depends on**: —

### Task 2: Pure `backoff_multiplier` helper (the heart — RED-first)
- **Test first**: `backoff_multiplier_table` — `mult(view=v, qc=v, f=2, cap=8)==1`;
  `mult(v+3, v, 2, 8)==8`; `mult(v+100, v, 2, 8)==2^8` (capped);
  `mult(v, v+5, ..)==1` (qc ahead → saturating gap 0).
- **Implementation**: free fn
  `fn backoff_multiplier(view: ViewNumber, highest_qc_view: ViewNumber, factor: u32, cap: u32) -> u32 { let gap = view.int().saturating_sub(highest_qc_view.int()); factor.saturating_pow(gap.min(cap as u64) as u32) }`.
- **Verify**: `cargo test -p hotstuff_rs backoff_multiplier_table`
- **Depends on**: —

### Task 3: Apply multiplier in the schedule builders
- **Test first**: `update_timeouts_gap0_matches_legacy` — build timeouts with
  `highest_qc_view == epoch_start_view` (gap 0) and assert deadlines equal the
  pre-change formula (`max_view_time*(view−start+1)`). `update_timeouts_gap_scales`
  — gap `k` scales the per-view increment by `factor^min(k,cap)`.
- **Implementation**: add a `highest_qc_view: ViewNumber` param to
  `initial_timeouts:653` and `update_timeouts:689`; multiply
  `time_to_view_deadline` by `backoff_multiplier(view, highest_qc_view, factor, cap)`.
- **Verify**: `cargo test -p hotstuff_rs update_timeouts_gap`
- **Depends on**: 2

### Task 4: Thread `highest_qc_view` through `update_view` + call sites
- **Test first**: existing tests recompiled with new signature; add
  `update_view_threads_qc_frontier` asserting a `gap>0` entry yields a scaled deadline.
- **Implementation**: add `highest_qc_view: ViewNumber` param to `update_view:516`;
  pass it into both `update_timeouts` calls (`:535`, `:555`). At the 3 call sites
  (`:156`, `:412`, `:503`) pass `block_tree.highest_pc()?.view`. In
  `Pacemaker::new`/`initialize`, pass `init_view` as the QC frontier (gap 0 at boot).
- **Verify**: `cargo test -p hotstuff_rs pacemaker`
- **Depends on**: 3

### Task 5: Make the `:554` rebase clamp backoff-aware (backoff-undo hazard)
- **Test first (3 assertions, RED)**:
  (a) `clamp_preserves_legitimate_backoff` — a freshly built backed-off schedule
  (large gap) is **not** rebased away on entry.
  (b) `update_view_jump_rebases_stale_schedule:1197` **stays green** — a stale
  absolute-jump schedule is still rebased.
  (c) `update_view_fast_run_deadline_bounded:1221` **stays green** (bound scales
  with multiplier).
- **Implementation**: scale the clamp threshold by the current multiplier:
  `if scheduled > Instant::now() + self.config.max_view_time * 2 * mult { update_timeouts(...) }`
  where `mult = backoff_multiplier(next_view, highest_qc_view, ..)`. A stale jump
  (old `epoch_start_time`) still exceeds the scaled threshold; a fresh backoff does not.
- **Verify**: `cargo test -p hotstuff_rs update_view`
- **Depends on**: 4

### Task 6: Apply multiplier to the epoch-change extend path
- **Test first**: `extend_scales_with_gap` — `extend_epoch_change_view_timeout`
  under `gap=k` extends by `max_view_time * factor^min(k,cap)`, not flat.
- **Implementation**: pass `highest_qc_view` into
  `extend_epoch_change_view_timeout:718`; multiply the added `max_view_time`.
  Update `extend_view:589` caller to supply `block_tree.highest_pc()?.view`
  (needs `block_tree` — thread it in; `tick` already holds it at the `:133` call).
- **Verify**: `cargo test -p hotstuff_rs extend`
- **Depends on**: 4

### Task 7: Stateright bounded-progress model (validation folded in from Option D)
- **Test first**: extend the T1.2 Stateright harness with a lock-step-timeout
  scenario post-partition-heal. Assert: with `factor=1` (backoff off) the model
  finds a no-progress cycle; with `factor=2` progress occurs within the bound.
- **Implementation**: model view-timeout as `base * factor^gap`, message delay Δ,
  N honest replicas timing out in lock-step; check `always eventually commit`.
- **Verify**: `cargo test -p hotstuff_rs --test <stateright_model> lockstep_backoff`
- **Depends on**: 2

### Task 8: Devnet lock-step-timeout acceptance + written safety argument
- **Test first (harness, not unit)**: on a 4-node devnet, induce a lock-step
  timeout (drop quorum then restore), record per-view durations. Accept iff:
  (i) durations grow geometrically during the stall, (ii) fleet reconverges
  without a multi-view park, (iii) happy-path empty-block cadence (gap≈0) is
  **unchanged** vs baseline binary (same-machine A/B, per S444 rule).
- **Implementation**: no code — this is the acceptance gate. Fold the liveness +
  lockstep argument above into the plan's "Safety Argument" section and have it
  reviewed before merge.
- **Verify**: devnet run script + manual A/B table; review sign-off.
- **Depends on**: 5, 6, 7

## Verification (end-to-end)
`cargo test -p hotstuff_rs` all green (incl. the 3 legacy timeout tests) → Stateright
model shows bounded convergence → devnet A/B shows geometric backoff + unchanged
happy-path cadence → written safety argument reviewed. Ship fleet-wide (config is
consensus-visible; every node must run the same binary + genesis knobs).

## Rollback
Single branch `feat/pacemaker-backoff`. Backoff is neutralized at runtime by
`backoff_cap = 0` (multiplier ≡ 1 → today's exact schedule) — a config-only kill
switch, no redeploy of logic. Full revert = drop the branch; no migration, no
persisted-state change (timeouts are in-memory).
