# Finish: Native-DA Big-Body Dissemination Fix (`/implement`) prompt

Copy everything below the `---` into a fresh (clear-context) session to FINISH the fix.
- **Design:** `docs/plans/native-da-bigbody-dissemination-fix.md` (Option C chosen)
- **Plan:** `docs/plans/native-da-bigbody-dissemination-fix-impl.md` (5 TDD tasks)
- **PRP:** `PRPs/native-da-bigbody.tasks.json`
- **Branch:** `fix/native-da-bigbody` (off `phase-a-incremental-root`, HEAD `e6cc192`). **Task 1 DONE + green; Tasks 2–5 remain.**

---

/implement docs/plans/native-da-bigbody-dissemination-fix-impl.md — FINISH the native-DA big-body dissemination fix (Option C): Task 2 (chunk native-DA client fetch), Task 3 (widen pull budget), Task 4 (e2e >4 MB reconstruct test), Task 5 (regression gate). TDD: failing test first, per task. Do NOT touch the live testnet.

GOAL: Make pull-DA the reliable big-body path so blocks >4 MB disseminate without wedging the chain. Phase C moved the wall 256KB→4MB, but the pull-DA fallback shares a single 4MB codec cap, so a block just over 4MB (bs=1000 ≈ 4.17MB) breaks BOTH push and pull and wedges consensus (committed height freezes; block_sync loops forever). LIVENESS bug only — a body-less block never gets 2f+1 votes, so committed state stays clean (no rollback).

ROOT CAUSE (verified, mem fa9dcaf36b0cf2e7): shared `MAX_DIRECT_MSG_SIZE=4MB` (`crates/torus-network/src/codec.rs`) used by `/torus/direct` + block-data + native-da. PUSH (`swarm.rs` `BroadcastNativeActions`: one QUIC stream per validator, no backpressure → quinn `max_concurrent_bidi_streams=100` → "max sub-streams reached"; also 4.17MB > 4MB). PULL native-da response ~4.4MB > 4MB silently dropped; pull poll budget only 80ms (`app.rs` `PULL_RETRIES=4 × 20ms`).

STATE / DONE (branch `fix/native-da-bigbody`, uncommitted):
- **Task 1 DONE + GREEN — per-protocol codec caps** (`crates/torus-network/src/codec.rs`): `read_length_prefixed_borsh` now takes `max_size`; each codec passes `Self::MAX_MSG_SIZE` (an inherent const): direct=4MB (UNCHANGED), native-da=8MB, block-data=16MB. Red→green test `native_da_response_over_4mb_roundtrips` added. `cargo test -p torus-network codec` = 2 passed, 0 failed.

TASKS REMAINING (TDD — write the FAILING test first; commit per task; message ends with the Co-Authored-By footer; do NOT push):
- **Task 2 — Chunk native-DA client fetch.** `bridge.rs` `fetch_native_actions_from_validators` sends ALL hashes in one request per validator. Add `pub const NATIVE_DA_FETCH_CHUNK: usize = 16;` and split: `for target { for chunk in hashes.chunks(NATIVE_DA_FETCH_CHUNK) { send(chunk.to_vec()) } }`. Server `serve_native_da_bodies` (`swarm.rs`) + `absorb_fetched_bodies` (`app.rs`, stores by RECOMPUTED hash) already handle partial/unordered → **client-only change**. Test first: 100 hashes × 2 validators ⇒ `ceil(100/16)=7` commands/validator (14 total), each ≤16 hashes. Verify: `cargo test -p torus-network`.
- **Task 3 — Widen pull budget.** `app.rs` `pull_missing_bodies`: `PULL_RETRIES=4`, `PULL_DELAY=20ms` (80ms) → `PULL_RETRIES=20`, `PULL_DELAY=50ms` (~1s). Sync path; blocking ≤1s is safe (NOT the consensus hot path). Test first: assert effective budget ≥1s, or a body delivered ~500ms is still recovered. Verify: `cargo test -p torus-consensus`.
- **Task 4 — E2E >4MB reconstruction proof (headline test).** Seed a DA store with 100 bodies summing >4MB; via codec + chunked fetch + serve, reconstruct all by-hash; assert 0 missing. This is the test that would have caught the wedge. Verify: `cargo test -p torus-network native_da_bigbody`.
- **Task 5 — Regression gate:** `cargo test -p torus-network -p torus-consensus -p torus-mempool` + `cargo test -p torus-consensus four_node_consensus` all green.

KEY LANDMARKS (verify line numbers — they drift after Task 1's edits):
- `codec.rs`: `MAX_DIRECT/NATIVE_DA/BLOCK_DATA_MSG_SIZE` consts + per-codec inherent `MAX_MSG_SIZE` + `read_length_prefixed_borsh(io, max_size)`.
- `bridge.rs` `fetch_native_actions_from_validators` (Task 2), `drain_native_da_inbound`.
- `swarm.rs` `serve_native_da_bodies` (server, already partial-safe); `BroadcastNativeActions` (PUSH — OUT OF SCOPE; that's #4).
- `app.rs` `pull_missing_bodies` (Task 3 budget) + `absorb_fetched_bodies` (by-hash).
- `rate_limit.rs`: `NATIVE_TOTAL_BLOCK_CAP=100`, `NATIVE_ORDERS_PER_BATCH_CAP=1024`, `NATIVE_ORDERS_PER_BLOCK_CAP=50_000` (sizing math).

CONSTRAINTS:
- TRANSPORT-ONLY: bodies are content-addressed by hash; caps/chunking change transport only. NO consensus-format or state-root change.
- Codec round-trip tests use `futures::executor::block_on` + `futures::io::Cursor` (mirror the Task 1 test).
- Keep `four_node_consensus` + existing suites green.
- The PUSH sub-stream exhaustion fix (raise quinn stream window / backpressure / gossipsub) is a SEPARATE follow-up (#4) — NOT in this fix.

GUARDRAILS:
- Do NOT touch the live testnet seed or `testnet/data`. The testnet is currently STOPPED and was WEDGED at committed height **235819** (clean; orphaned 235820+ never finalized). Recovery = build the fixed binary, then a COORDINATED restart of all 3 validators (us + 2 friends) — the same op redeploys the fix and un-wedges. Do NOT do this without the user + friends coordinating.
- Do NOT commit the pre-existing unrelated changes: `devnet/docker-compose.yml`, `devnet/scripts/__pycache__`, `testnet/data.pre-phaseb-c7426e0`, `prompts/*.md`. Commit ONLY `codec.rs`/`bridge.rs`/`app.rs` + `docs/plans/native-da-bigbody*.md` + `PRPs/native-da-bigbody.tasks.json` + new tests.
- REBUILD RELEASE (`cargo build --release -p torus-node`) before any devnet test — the docker image copies `target/release/torus-node`.
- LOCAL docker devnet only for load testing (host ports 8645+). Do NOT push bs=1000 at the live testnet — that caused the wedge. Stay ≤ bs=500 there, or use the devnet.

MEMORY (read for full context):
- `fa9dcaf36b0cf2e7` — wedge root cause (4MB shared cap)
- `9ad2026a856d9b42` — the 5-task plan
- `9e099d5f5823d0eb` — Option C design decision
- `cefd7119a7242244` — bs-sweep result (Phase C killed 256KB; this is the NEXT wall)

Start by confirming the green baseline (`cargo test -p torus-network codec`), reading the plan + this state, then begin Task 2 (chunk the native-DA fetch).
