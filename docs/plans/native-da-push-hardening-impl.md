# Implementation Plan: Native-Action PUSH Hardening (#4)

**Branch:** TBD (off `fix/native-da-bigbody` or `main`) · **Created:** 2026-06-08
**Design:** `docs/plans/native-da-push-hardening.md`

Phased per the design recommendation. **Phase 1 alone un-wedges bs≈1000** (liveness);
Phases 2–3 restore/scale throughput. TDD: write the failing test first, commit per task,
message ends with the Co-Authored-By footer, do NOT push, do NOT touch the live testnet.

## Success criteria
- A compact block whose bodies are absent from the local DA store is reconstructed on
  the **hot** `validate_block` path via a bounded/async pull — without blocking past the
  view timeout — so a push miss no longer wedges live consensus.
- The `BroadcastNativeActions` push loop cannot exhaust quinn's stream window
  (bounded in-flight) and the limit is raised for headroom.
- Re-run bs-sweep: **bs=1000 keeps committed height advancing** (no permanent wedge).
- `four_node_consensus` + bigbody suites stay green.

---

## Phase 1 — Hot-path pull-fallback (Option A; the un-wedge)

### Task 1.1: Hot-path pull seam (test first)
- **Test first** (`app.rs` tests): drive `validate_block` on a `CompactBlock` whose
  bodies are missing locally, with a `NativeDaFetcher` double that delivers within a
  **tight** bound. Assert: (a) the block validates once bodies arrive, and (b) when the
  fetcher never delivers, `validate_block` returns within the hot-path budget (it must
  **fail the view, not hang past the view timeout**). Mirrors the bigbody
  `pull_recovers_body_delivered_after_old_budget` test but on the hot path with a
  short budget. MUST fail today (hot path never pulls).
- **Implementation** (`crates/torus-consensus/src/app.rs:1157` `validate_block`):
  add a hot-path pull seam that, on a reconstruction miss, fires the chunked fetch and
  does a **short bounded** drain-and-absorb (e.g. `HOT_PULL_RETRIES`×`HOT_PULL_DELAY`
  budgeted **≪ 500 ms view timeout** — e.g. ~150–200 ms), then re-checks. Reuse
  `pull_missing_bodies` infra but with hot-path constants (NOT the ~1 s sync budget).
  Keep the existing local retry as the fast first check.
- **Verify:** `cargo test -p torus-consensus validate_block`
- **Depends on:** bigbody Tasks 2–3 (already landed).

### Task 1.2 (design-gated): async-vote variant if bounded-block is too tight
- If a ~150 ms bounded block proves too short to recover under real RTT (decided from
  Task 1.1 + a devnet check), switch to **async**: fire the fetch, fail the current
  view, and vote in a later view once the body is present (the body arrives between
  views). **Test first:** body delivered after the view fails is voted in the next view.
- **Verify:** `cargo test -p torus-consensus` + devnet bs=1000 keeps advancing.

---

## Phase 2 — Push hardening (Options B + C)

### Task 2.1: Raise quinn stream window (config)
- **Test first:** an assertion/unit that the configured quinn transport sets
  `max_concurrent_bidi_streams` ≥ a chosen headroom (e.g. 512) — or, if not unit-testable,
  a devnet check that `max sub-streams reached` no longer fires at bs=500.
- **Implementation** (`crates/torus-network/src/bridge.rs:125`): replace `.with_quic()`
  with `.with_quic_config(|cfg| …)` raising the per-connection bidi-stream limit and (if
  warranted) send/receive windows.
- **Verify:** `cargo test -p torus-network` + devnet bs=500 shows 0 `max sub-streams`.

### Task 2.2: Bounded in-flight backpressure on the push loop
- **Test first** (`swarm.rs` tests): a unit over the push dispatch that caps concurrent
  in-flight `send_request`s to `PUSH_MAX_INFLIGHT` (drain/await before opening more), so
  N validators × bursts can't exceed the stream window. MUST fail today (unbounded loop).
- **Implementation** (`crates/torus-network/src/swarm.rs:994` `BroadcastNativeActions`):
  bound concurrent direct sends (semaphore / in-flight counter keyed by request id, reuse
  the `outbound_direct` tracking), queuing the remainder. Preserve T7 disconnect-queue
  behaviour (`pending_native_pushes`).
- **Verify:** `cargo test -p torus-network`

### Task 2.3 (optional): hash-only push for oversized batches
- If a batch still exceeds `MAX_DIRECT_MSG_SIZE` (4 MB) after backpressure, **push the
  hashes only** (compact) and let the now-reliable hot-path pull (Phase 1) fetch bodies,
  instead of pushing the full >4 MB envelope through the 4 MB `/torus/direct` codec.
- **Verify:** devnet bs=1000.

---

## Phase 3 — Gossipsub dissemination (Option D; can be its own follow-up #5)
Replace per-validator unicast push with a gossipsub publish of the batch (hashes for
large batches + pull). Sketch only here; align with
`docs/plans/mempool-gossip-hash-proposals-impl.md`. Defer unless Phases 1–2 don't scale.

---

## Cross-cutting: regression gate
- `cargo test -p torus-network -p torus-consensus -p torus-mempool` and
  `cargo test -p torus-consensus four_node_consensus` all green.
- E2E: rebuild release, local-devnet (or coordinated live) bs-sweep — **bs=1000 keeps
  committed height advancing and recovers after load stops** (the #4 acceptance proof).

## Rollback
All changes are additive/parameterizing (hot-path budget consts, quinn config, a push
in-flight cap). Revert the branch; no consensus-format or state-root change (bodies stay
content-addressed by hash; this is transport/timing only).
