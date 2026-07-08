# Implement: Native-Action PUSH Hardening (#4) (`/implement`) prompt

Copy everything below the `---` into a fresh (clear-context) session to IMPLEMENT #4.
- **Design:** `docs/plans/native-da-push-hardening.md` (Options A–D; phased recommendation)
- **Plan:** `docs/plans/native-da-push-hardening-impl.md` (TDD tasks, phased)
- **PRP:** `PRPs/native-da-push-hardening.tasks.json` (4 tasks)
- **Prereq (LANDED):** the native-DA big-body fix (Tasks 1–5) is on `fix/native-da-bigbody`
  (commits `24f0107`,`f9581f5`,`d08fed8`,`392dd95`, pushed). #4 **reuses** its chunked pull.

---

/implement docs/plans/native-da-push-hardening-impl.md — IMPLEMENT native-action PUSH hardening (#4): Task 1 (hot-path pull-fallback — THE un-wedge), Task 2 (raise quinn stream window), Task 3 (bounded push backpressure), Task 4 (regression + bs=1000 un-wedge proof). TDD: failing test first, per task. Branch `fix/native-da-push-hardening` off `fix/native-da-bigbody` (it has the pull infra #4 reuses). Do NOT touch the live testnet.

GOAL: Keep the chain LIVE (committed height advancing) under bs≈1000 / >4 MB pre-proposal batches — no permanent wedge. This is a LIVENESS fix (not throughput), following the bigbody fix.

ROOT CAUSE (VERIFIED in code + live bs-sweep 2026-06-08):
- PUSH `BroadcastNativeActions` (`crates/torus-network/src/swarm.rs:994`) opens ONE QUIC bidi stream per validator in a tight loop with NO backpressure, shipping the FULL batched payload (multi-MB at high batch_size) through the `/torus/direct` codec whose cap is still 4 MB (`MAX_DIRECT_MSG_SIZE`, intentionally unchanged). → quinn default `max_concurrent_bidi_streams=100` exhausted → `max sub-streams reached`; and >4 MB batches rejected by the 4 MB cap.
- quinn uses defaults: `crates/torus-network/src/bridge.rs:125` `.with_quic()` (no stream-limit/window tuning).
- HOT/SYNC pull asymmetry: the SYNC path `validate_block_for_sync` (`crates/torus-consensus/src/app.rs:1340`) calls `pull_compact_bodies_if_missing` (app.rs:833) → chunked pull. The HOT path `validate_block` (app.rs:1157) keeps only a bounded LOCAL retry and NEVER triggers a network pull (mem 8ee99db3). So when push fails live, the hot path cannot recover the body → wedge.

EVIDENCE (live bs-sweep on the fixed pull binary): bs=100/250/500 NO WEDGE (bs=500 committed a 50k-order ~2–4 MB block); bs=1000 WEDGED (committed frozen, views kept climbing). `max sub-streams reached` ×2537 at bs=500; ZERO native-DA pull events at bs=1000 (hot path doesn't pull); NO `message too large` ever (the bigbody codec/pull fix is sound — the bottleneck is push + missing hot-path pull). mem a53d503f.

TASKS (TDD — failing test first; commit per task; message ends with the Co-Authored-By footer; do NOT push):
- **Task 1 — Hot-path pull-fallback (Phase 1; THE un-wedge).** Give `validate_block` (app.rs:1157) a bounded/async native-DA pull on a reconstruction miss, reusing `pull_missing_bodies` infra but with HOT-path constants budgeted **≪ the 500 ms view timeout** (`timeout_base_ms=500`; e.g. ~150–200 ms) — it MUST fail the view rather than block past the timeout. If a bounded block is too tight under real RTT, switch to the async-vote-next-view variant (impl doc Task 1.2). Test first: missing-body block reconstructs when the fetcher delivers within budget AND returns within the hot budget (no hang) when it doesn't. Verify: `cargo test -p torus-consensus validate_block`.
- **Task 2 — Raise quinn stream window.** `bridge.rs:125` `.with_quic()` → `.with_quic_config(|cfg| …)` raising `max_concurrent_bidi_streams` (e.g. 512) + send/recv windows. Verify: `cargo test -p torus-network` (+ devnet: 0 `max sub-streams` at bs=500).
- **Task 3 — Bounded in-flight push backpressure.** `swarm.rs:994` `BroadcastNativeActions`: cap concurrent `direct.send_request` to `PUSH_MAX_INFLIGHT`, queue the rest; preserve the T7 `pending_native_pushes` disconnect-queue. Optional 3b: hash-only push for batches >4 MB (rely on Task 1's pull). Verify: `cargo test -p torus-network`.
- **Task 4 — Regression + bs=1000 un-wedge proof.** `cargo test -p torus-network -p torus-consensus -p torus-mempool` + `cargo test -p torus-consensus four_node_consensus` green; then rebuild release + LOCAL-devnet bs-sweep showing bs=1000 keeps committed height advancing and recovers after load stops.

KEY LANDMARKS (verify line numbers — they drift):
- `swarm.rs:994` `BroadcastNativeActions` (push loop); `swarm.rs:1047` `FetchNativeActions` (pull send).
- `bridge.rs:125` `.with_quic()`; `bridge.rs` `NATIVE_DA_FETCH_CHUNK=16` (pull chunk const, reused).
- `app.rs:1157` `validate_block` (hot — add pull); `app.rs:1340` `validate_block_for_sync` (sync — already pulls); `app.rs:833` `pull_compact_bodies_if_missing`; `app.rs:852` `pull_missing_bodies` + module-level `PULL_RETRIES=20`/`PULL_DELAY=50ms` (sync ~1 s — add SEPARATE tighter hot consts, do NOT reuse these on the hot path).

CONSTRAINTS:
- LIVENESS-ONLY + TRANSPORT/TIMING-ONLY: bodies stay content-addressed by hash; NO consensus-format or state-root change.
- The hot path is the 500 ms view-timeout path — the hot pull MUST NOT block past it (bounded ≪500 ms or async). The sync ~1 s budget is FINE for sync, FATAL for hot.
- Reuse the bigbody chunked pull (`fetch_native_actions_from_validators`, `serve_native_da_bodies`, `absorb_fetched_bodies`) — do not reinvent.
- Keep `four_node_consensus` + bigbody suites green.
- Gossipsub dissemination (design Option D) is OUT OF SCOPE — that's a separate follow-up #5.

GUARDRAILS:
- Do NOT touch the live testnet or `testnet/data`. As of 2026-06-08 the live 3-validator chain is WEDGED at committed height 235822→27197-era (a fresh chain at 27197 stalled by the bs=1000 backlog); recovery = coordinated restart, separate from this work.
- bs-sweep / load tests: LOCAL docker devnet ONLY (host ports 8645+). The push #4 issue is exactly why bs=1000 is unsafe on the live testnet until this lands.
- REBUILD RELEASE (`cargo build --release -p torus-node`) before any devnet test (docker copies `target/release/torus-node`).
- Do NOT commit unrelated working-tree files (devnet/docker-compose.yml, prompts/*.md, testnet/data*). Commit ONLY app.rs/bridge.rs/swarm.rs + the plan docs/PRP + new tests.

MEMORY (read for full context):
- `997e181d6420068c` — this #4 plan (design+impl+PRP, root cause, tasks)
- `a53d503fa12b2da7` — the live bs-sweep results (#4 evidence)
- `5a1fcdca75a528cf` — bigbody fix COMPLETE (the pull infra #4 reuses)
- `9e099d5f5823d0eb` / `9ad2026a856d9b42` — bigbody design/plan (Open Q#3 deferred hot-path pull to #4)

Start by confirming the bigbody pull infra is present (`cargo test -p torus-network native_da_bigbody` green), reading the design + impl plan + this state, then begin Task 1 (the hot-path pull-fallback — the un-wedge).
