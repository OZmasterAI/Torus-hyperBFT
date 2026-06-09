# Design: Native-Action HASH-ONLY Push (Phase 2.3 — un-wedge bs≈500 live)

**Created:** 2026-06-09 · **Status:** DESIGN (awaiting option pick) · **Branch:** `fix/native-da-push-hardening` (continue) or a fresh `feat/native-da-hash-only-push`

Follow-up **#5** to the PUSH-hardening work (`docs/plans/native-da-push-hardening.md`).
PUSH-hardening #4 (T1 hot-path pull, T2 QUIC 256→512, T3 PushScheduler bounded push)
landed and is **necessary but INSUFFICIENT at bs≥500** on the live testnet. This implements
the deferred, now-empirically-warranted **Task 2.3** ("hash-only push for oversized
batches") with TDD + a receiver-side pull **pre-warm**.

## Problem
Under sustained native-action load the proposer's pre-proposal **PUSH** carries the
**full multi-MB action bodies** (`bincode(Vec<(Address, SignedNativeAction)>)`) to every
validator over `/torus/direct`. That big-body push cannot disseminate within the ~500 ms
view timeout, so validators reach `validate_block` without the bodies → `MissingData` →
the view fails → re-propose → the same big push fails again → **liveness wedge**.

## Evidence (live testnet bs-sweep, 2026-06-09, on the #4-hardened binary — mem f58957c6)
| batch_size | result | note |
|---|---|---|
| 100 | ✅ healthy | 23,430 orders/s, 289 ms blocks |
| 200 | ⚠️ degraded-but-live | 5,783 orders/s, 631 ms blocks, 85% drop |
| 500 | ❌ **WEDGED** | 26.8 s blocks, 97% drop, **25 view-timeouts** |

- Failure mode at bs500 = **VIEW TIMEOUT on oversized-body dissemination**, NOT the
  substream cap: `max sub-streams reached` fired **0×** at bs500 (only 1 block produced),
  so #4 T2/T3 (stream window + bounded push) are not the bottleneck here. The proposer's
  view simply can't wait for the multi-MB body to reach the validators in time.
- #4 RECOVERS the wedge ~1–2 min after load stops (no permanent wedge, mem 7d47035c) —
  this is a throughput/liveness ceiling, not corruption.

## Root cause (verified in code)
- **`crates/torus-node/src/main.rs:445-451`** (pre-proposal glue): drains
  `pre_proposal_rx` and pushes `bincode(bundle.actions)` — the **FULL bodies** — via
  `network.broadcast_native_actions(payload)`.
- **`crates/torus-network/src/swarm.rs:1091`** (`BroadcastNativeActions`): wraps the
  full-body payload as `[PRE_PROPOSAL_BATCH_MARKER]+payload`, PushScheduler-bounded
  unicast to each validator over `/torus/direct` (4 MB cap → a >4 MB batch is rejected
  outright; a ≤4 MB-but-large batch still can't disseminate within the view).
- **`crates/torus-network/src/swarm.rs:661`** (inbound): on `PRE_PROPOSAL_BATCH_MARKER`,
  deserializes the full bodies into `native_action_inbound` → mempool → DA store.
- The proposer already **mirrors** bodies to its own durable DA store
  (`app.rs:1204 mirror_native_to_da`), so it can SERVE them on a by-hash pull regardless.

## Key insight — the body is already content-addressed
Bodies travel out-of-band and the `CompactBlock` proposal already references them **by
hash** (`compact.native_action_hashes`). The by-hash **serve** (#4/Phase-C T5),
**fetch** (T6, chunked `NATIVE_DA_FETCH_CHUNK=16`), and **hot-path pull absorb**
(T1 `reconstruct_native_actions_hot`) infra all exist. So we can stop shipping the big
body in the push and instead ship a **tiny hash manifest**, letting validators PULL the
body **concurrently with consensus** (pre-warming the pull before the proposal arrives)
rather than blocking the proposer's view on dissemination.

## Goal & non-goals
- **Goal:** committed height keeps advancing at bs≈500 / >4 MB pre-proposal batches —
  no permanent wedge, graceful throughput degradation at worst. **Liveness** fix.
- **Non-goal:** maximizing orders/s (a separate perf track). Must NOT regress the healthy
  bs100 path (23,430 o/s @ 289 ms) — that is an explicit guardrail.

## Options

### Option A — Size-gated hash-manifest pre-push + receiver pre-warm pull (RECOMMENDED)
The proposer/glue pushes a tiny `PRE_PROPOSAL_HASHES_MARKER` manifest (`Vec<[u8;32]>`)
**when the encoded bundle exceeds a dissemination threshold** (byte-size gate); small
batches keep the full-body push (no added pull RTT). On the new marker the receiver
enqueues a chunked by-hash `FetchNativeActions` **targeted at the proposer**
(`sender_vk` is already verified at `swarm.rs:649`) to **pre-warm** the DA store before
the `CompactBlock` arrives. The hot path is unchanged — by the time
`reconstruct_native_actions_hot` runs, the body is already present (or in-flight, absorbed
within the short hot budget).
- **Files:** `main.rs` (gate + manifest), `swarm.rs` (new outbound marker; inbound
  marker→pull branch), small payload helper. PushScheduler/T7/re-push, T5 serve, T6
  fetch, T1 absorb all REUSED. No consensus-format change.
- **Pros:** the state-file lever exactly; preserves the healthy small-batch fast path
  (gated); pre-warm hides pull latency so the 500 ms view timeout is protected; un-wedges
  >4 MB (manifest is tiny, never hits the 4 MB cap); content-addressing unchanged → no
  fork risk (transport/timing only).
- **Cons:** two push code paths (gated); a new payload discriminant both sides must agree
  on (transport-only, not consensus identity); pre-warm target selection (proposer vs all).
- **Effort:** Medium · **Risk:** Medium (receiver pre-warm timing, bounded by existing pull).

### Option B — Always hash-only push (no size gate)
Uniformly push the manifest and always pull bodies; drop the full-body push path.
- **Pros:** simplest single path; eliminates the big-body push class entirely.
- **Cons:** adds a pull RTT to **every** block including tiny ones → risks regressing the
  healthy bs100 23,430 o/s baseline (the guardrail); makes the proposer a per-block pull
  hotspot; loses the "push lands before the proposal" common-case fast path.
- **Effort:** Small–Med · **Risk:** Medium (regresses the path we must protect).

### Option C — Drop the pre-proposal push for big batches (no new wire message)
Above the threshold the glue simply **skips** the push; rely on the `CompactBlock`
hashes + the T1 hot-pull. No new protocol, no receiver change.
- **Pros:** smallest diff; zero new wire format; reuses T1 exactly.
- **Cons:** the pull only STARTS when the proposal arrives (no pre-warm), so the full
  RTT + chunked transfer of a multi-MB body must fit the **≪500 ms hot budget** — the
  exact margin the live bs500 wedge shows is tight; the view can still fail (now via
  pull-timeout instead of push-timeout). Leans entirely on T1's budget being enough.
- **Effort:** Small · **Risk:** Medium–High (the marginal hot-pull budget is the risk).

## Recommendation — Option A
It is the state-file lever, preserves the healthy small-batch fast path (no bs100
regression), and **pre-warms** the pull so the body is present before the hot path needs
it — directly protecting the view timeout that wedges today. It reuses every landed
T1/T5/T6 component and changes only transport/timing (no consensus-format/state-root
change, so no coordinated-relaunch fork risk). **Option C** is the minimal fallback if a
devnet check shows pre-warm is unnecessary; **Option B** is rejected because it risks the
bs100 baseline we must protect.

## Open questions
1. **Size-gate threshold (A):** by encoded byte size (push bodies if
   `bincode(actions).len() ≤ T`, else manifest; T ≈ 1 MB, a fraction of the 4 MB direct
   cap) vs action-count vs always-manifest. → lean byte-size, tied to dissemination cost.
2. **Pre-warm pull target (A):** fetch from the **proposer only** (`sender_vk`; 1
   guaranteed source — it mirrored the bodies — no N× amplification) vs reuse
   `fetch_native_actions_from_validators` (fan-to-all; simpler, more robust if the
   proposer is slow). → lean proposer-targeted, fall back to fan-to-all on miss.
3. **Absorb seam (A):** network-side insert into `shared.native_da` on pull response (so
   the consensus local lookup hits with zero hot-pull) vs leave the consensus hot-pull to
   drain/absorb (simpler reuse, one redundant fetch). → decide in writing-plans; prefer
   network-side absorb if cheap.
4. **Threshold vs the live view timeout:** validate the gate against the measured
   bs100/200/500 dissemination times so the manifest path engages exactly where the
   full-body push would miss the view.

## Verification
Re-run the live (or local-devnet) bs-sweep: **bs=500 keeps committed height advancing**
(no permanent wedge) and **bs100 stays at/near 23,430 o/s** (no regression). Keep
`four_node_consensus`, the bigbody suites, and `torus-network`/`torus-consensus` green.
