# Design: BS-4a — native-DA reconstruct off the consensus thread

## Problem

`reconstruct_native_actions_hot` (app.rs:1124) runs inside `validate_block` on
the single hotstuff consensus thread. On a body miss it blocks that thread for
up to **~260ms**: a 100ms local retry (`RECONSTRUCT_RETRIES 5 × 20ms`) plus a
160ms network pull (`HOT_PULL_RETRIES 8 × 20ms`), against a 500ms view
timeout. hotstuff_rs's own timing contract
(`4*EWNL + produce_block_duration + validate_block_duration < max_view_time`,
hotstuff_rs/src/app.rs:90) means every ms spent here stretches the view
directly, and while the thread waits it processes **no other protocol
messages**. Under heavy body traffic with repeated misses the stalls compound
— this is implicated in the S419 O2 collapse dynamics (MissingData churn →
views stretch → nonce mass-eviction; mem 7d140b09, c8308bdb). Invisible on an
empty chain; a p99 lever under load.

BS-4b (body-retry event-driven) piggybacks: the retry cadence is already
wake-on-arrival (S391) inside these loops; whatever recovery machinery
survives this change should stay event-driven, not fixed-sleep.

## Context (memory + exploration)

- `fetcher.fetch(hashes)` is **already non-blocking** — it sends
  `NetworkCommand::FetchNativeActions` down a channel
  (bridge.rs:374/390, chunked per validator). The blocking parts are only the
  `NativeDaStore::wait_for_arrival` loops in app.rs (local retry
  app.rs:1148-1188, pull wait pull_missing_bodies_bounded app.rs:1076-1100).
- Voting `MissingData` is the designed miss outcome (NOT Invalid, no peer
  blacklisting — mem 28e1a821): the view fails fast and the block is
  re-proposed next view. Changing *when* we give up is per-node liveness
  policy, **not** a consensus-validity change — deployable per-node, no
  lockstep.
- Pre-warm already exists (Phase 2.3): a hash-only manifest makes the receiver
  pull bodies out-of-band; `absorb_fetched_bodies` mirrors them into the
  durable DA store. The common case never enters the slow path.
- Background-worker precedent: O3's `torus_state::BackgroundCfWriter`
  (ExecutionContext-owned, drain-on-drop) — same shape of fix.
- Sync path (`pull_missing_bodies`, ~1s budget) is off the voting path and
  explicitly safe to block — untouched by this design.
- Existing tests pinning current behavior: `hot_path_pulls_missing_body_within_budget`
  (app.rs:2906), `hot_path_fails_view_fast_when_body_never_arrives` (:2986),
  `prewarmed_bodies_absorbed_without_redundant_hot_fetch` (:2948), budget
  assertion (:3007).

## Options

### Option A: Fail-fast — fire the pull, vote MissingData immediately

**How it works.** On a local DA-store miss, keep at most ONE short
wake-on-arrival slice (≤20ms, catches the racing pre-proposal push), then fire
the (already non-blocking) validator-set fetch and return
`Err(missing)` → `MissingData` right away. Responses land in the fetcher
inbound as they arrive and get absorbed on the next validate/local-retry pass;
the re-proposed block next view finds the bodies locally.

- **Files:** app.rs only (reconstruct_native_actions_hot,
  pull_missing_bodies_bounded callers, consts, tests).
- **Trade-offs:** simplest possible change; consensus-thread worst case drops
  ~260ms → ~20ms. BUT recovery becomes passive — nothing owns re-requesting if
  the first fan-out fetch is lost (fetch is fire-and-forget UDP-ish over
  request/response; a dropped response = wait for next view's fetch). Misses
  that today recover in-window (pull succeeds at 60-160ms, view survives) now
  always cost a full failed view (~500ms for that block).
- **Effort:** Small. **Risk:** Medium — under a lossy WAN the passive recovery
  could *increase* MissingData rounds per block (churn is exactly the S419
  death-spiral signature).

### Option B: Background recovery worker (fail-fast + owned off-thread recovery)

**How it works.** Same fail-fast vote as Option A (one ≤20ms local slice, then
`MissingData`), but the missing hashes are handed to a dedicated
`DaRecoveryWorker` (std thread, O3 BackgroundCfWriter pattern: owned by
TorusApp, channel-fed, drain-on-drop). The worker runs the existing
wake-on-arrival pull loop with the **sync-tier ~1s budget** (far more
effective than the hot path's 160ms) fully off-thread: fetch → wait-for-arrival
→ absorb into the durable DA store → re-fetch with peer rotation until
recovered or a generous deadline (e.g. 2s). By re-proposal time the bodies are
locally present with high confidence. Dedup by hash so repeated views don't
stack redundant work. BS-4b lands here for free: the worker's loop is
event-driven off the arrival notifier (already built, S391), no fixed sleeps.

- **Files:** app.rs (worker struct + handoff in reconstruct_native_actions_hot,
  consts, tests); no signature changes to the App trait; torus-node wiring
  unchanged (worker reuses the existing `da_fetcher`/`mempool` handles, both
  Send + Sync).
- **Trade-offs:** consensus thread worst case ~20ms AND recovery is owned +
  stronger than today (1-2s of event-driven retries vs 160ms). Per-miss block
  latency: a genuinely pushed-missed block costs one failed view (~500ms)
  instead of sometimes recovering at 60-260ms — but under load that trade is
  the whole point (thread freed to process votes/proposals; churn dampened by
  near-certain recovery before the re-propose). Slightly more code: a thread +
  channel + shutdown ordering.
- **Effort:** Medium. **Risk:** Low-Medium — thread lifecycle is
  well-precedented (O3); behavior change is per-node liveness policy only.

### Option C: Push the pre-warm earlier; keep the hot budget as-is

**How it works.** Attack the miss *rate* instead of the stall: trigger the
by-hash prefetch at CompactBlock gossip arrival (network thread), before
hotstuff ever calls validate_block, so the hot path almost never waits. Keep
the current 260ms in-line budget as the rare last resort.

- **Files:** swarm.rs/bridge.rs (gossip handler prefetch), app.rs untouched.
- **Trade-offs:** no consensus-thread improvement in the worst case — the
  260ms stall stays and still fires exactly in the bad regime (loss/late
  bodies), i.e. it does not fix the S419-implicated mechanism. Mostly
  duplicates the existing Phase 2.3 manifest pre-warm.
- **Effort:** Small-Medium. **Risk:** Low, but low reward — does not meet the
  BS-4a goal.

## Recommendation

**Option B.** It is the only option that both removes the consensus-thread
stall (the roadmap goal, ~260ms → ~20ms worst case) and *strengthens* recovery
(1-2s event-driven off-thread vs 160ms in-line), which is what the S419
churn-spiral regime needs. Option A is a cheaper first cut but its passive
recovery risks more MissingData rounds exactly under load; Option C doesn't
fix the mechanism. The O3 BackgroundCfWriter gives a proven in-repo pattern
for the thread lifecycle.

Deployment: per-node safe (liveness policy only, no consensus-validity or wire
change) — does NOT require the lockstep treatment O2 does, and does not need
to be in the relaunch binary (fleet stays pinned to 6e03294; this lands on its
own branch and joins a later rollout with devnet A/B evidence).

## Open Questions

1. Keep one 20ms local slice or two (40ms) before failing the view? The racing
   pre-proposal push is the common case today; measure the race-window
   distribution on devnet before final consts.
2. Worker deadline: 1s (sync parity) vs 2s (cover a slow re-propose cycle)?
   Cheap to tune; start at sync parity.
3. Should the worker also serve the *sync* path's pull (unify), or leave sync
   blocking in place (it is explicitly allowed to block)? Leaning leave-alone —
   minimal blast radius.
4. Metrics: add `native_da_recovery_handoffs` + worker recovery-latency
   histogram so the relaunch A/B can see the lever move (p99 view time under
   O2 bs400 load is the acceptance signal).
