# Native-Action DA Layer — Tasks 5–9 continuation prompt

Copy-paste this into a fresh (clear-context) session to continue the DA-layer plan.
**Tasks 1–4 (the un-wedge increment) are DONE and committed on branch `phase-c-native-da`.** This prompt covers the remaining scale-to-400k work (Tasks 5–8) + the scale milestone (Task 9).
Plan: `docs/plans/native-action-da-impl.md` · PRP: `PRPs/native-action-da.tasks.json` · Design: `docs/plans/native-action-da.md`
Alternative (orchestrated): replace the first line with `torus-loop.sh native-action-da`.

---

/implement docs/plans/native-action-da-impl.md  — continue with Tasks 5→8, then 9

GOAL: Continue the Native-Action Data-Availability (DA) layer (Option B). Tasks 1–4 (the un-wedge increment) are ALREADY DONE on branch `phase-c-native-da`. Implement Tasks 5–8 (reliable out-of-band delivery so the chain reaches 400k+ orders/sec) then Task 9 (scale milestone). CONSENSUS-CRITICAL: correctness and determinism over speed.

ALREADY DONE — do NOT redo (branch phase-c-native-da, 4 commits off cap100-3val-perf):
- 4ddfe59 T1: `CF_NATIVE_PENDING` (torus-state/src/cf.rs) + `NativeDaStore` (torus-state/src/native_da.rs): durable body store keyed by `compute_action_hash`, value = bincode(SignedNativeAction), API new/put/get/remove, decoupled from the 60s nonce gate. CF auto-creates on open.
- 3a62d97 T2: `Mempool` owns a `NativeDaStore`; bodies mirrored on every ingest (RPC after admission; `add_native_action_from_gossip_trusted` UNCONDITIONALLY before the nonce gate = decoupled), on reinsert, and in `produce_block` (proposer guarantee). New pub `Mempool::get_native_da` / `mirror_native_to_da` / `mirror_to_da`.
- 153fa3e T3: `TorusApp::reconstruct_compact_from_da` (inherent impl) reads the DURABLE store via `mempool.get_native_da`, returns `Err(missing_hashes)` (typed miss). Used in `validate_block` (with the bounded ~100ms poll-retry kept for the proposal-push race), `validate_block_for_sync`, and `on_committed_block`. `on_committed_block` now extracts the header and advances `last_header`+view UNCONDITIONALLY (the freeze is gone).
- e233f4e T4: new `ValidateBlockResponse::MissingData` (hotstuff_rs/src/app.rs); `validate_block` returns `MissingData` (not `Invalid`) when bodies are unreconstructable; `hotstuff_rs/src/block_sync/client.rs::warrants_blacklist` only blacklists a genuinely `Invalid` block (a `MissingData` ends the sync session WITHOUT blacklisting — the hook for Task 6's fetch).

READ FIRST (in this order):
1. Plan:    docs/plans/native-action-da-impl.md   (Tasks 5–9, success criteria, rollback)
2. PRP:     PRPs/native-action-da.tasks.json      (per-task files + `validate` commands + deps)
3. Design:  docs/plans/native-action-da.md        (Option B; A/C rejected; history)
4. Memory — run: run_tool("memory","search_knowledge",{"query":"native action data availability tasks 5 6 7 8 protocol pull-fallback push compact"})
   then get_memory each of:
   - 86488b6f  SESSION 315 wrap-up (what Tasks 1–4 shipped; the current branch state)
   - 360d5336  Task 3 livelock fix detail (reconstruct-from-DA + last_header)
   - a6cf33a9  dissemination history — per-block pull regressed 46→211ms (pull is FALLBACK-ONLY)
   - c19a35aa  s297 Option D (push-primary / fetch-rare)
   - 8ee99db3  CRITICAL: do NOT sleep/retry inside validate_block — it blocks the consensus thread
   - 0efcef9d  CRITICAL: linear fast-path in validate_block execution is LOAD-BEARING — do NOT remove
   - 8f6220b5  4 monadbft_b3 reputation tests are PRE-EXISTING failures — NOT a regression, do not chase

REMAINING TASKS (TDD — failing test first; run the PRP `validate`; paste output; only proceed green):

- T5 — `/torus/native-da/1.0` request_response (serve bodies by-hash). Template: the existing `/torus/block-data/1.0` protocol.
  - codec.rs: add `NativeDaCodec` modeled on `BlockDataCodec` (codec.rs:113-181). `NativeDaNetRequest { hashes: Vec<[u8;32]> }`, `NativeDaNetResponse { bodies: Vec<Vec<u8>> }` (each = bincode(SignedNativeAction); empty/absent entries = not found). Length-prefixed borsh, reuse the existing read/write helpers.
  - behaviour.rs: add `pub native_da: request_response::Behaviour<NativeDaCodec>` (model `block_data` at behaviour.rs:26 + 65-71), protocol `/torus/native-da/1.0`.
  - swarm.rs: add inbound-request + inbound-response handlers (model block-data at swarm.rs:620-695). SERVE bodies by reading a `NativeDaStore` handle on the network `shared` state (ADD one at startup — it's a cheap clone over the same StateDb/Arc<DB>). Receive → an inbound queue (model `block_data_inbound`). Add `NetworkCommand::FetchNativeActions { target, hashes }`.
  - NO hotstuff_rs protocol changes.
  - validate: `cargo test -p torus-network native_da_protocol_roundtrip`
- T6 — wire the RARE pull-fallback into the miss path.
  - On a reconstruction miss (the `Err(missing)` / `MissingData` path in `validate_block_for_sync` and/or `on_committed_block`), send `FetchNativeActions(missing)`, await the inbound channel with a BOUNDED timeout WELL under the 500ms view timeout, insert results into the DA store, retry reconstruct once.
  - HARD CONSTRAINT: do NOT sleep/block the consensus thread (mem 8ee99db3). Prefer fetching off the consensus thread / a non-blocking await; never a busy 500ms sleep in validate_block.
  - Assert it does NOT fire when bodies are already local (rarity). validate: `cargo test -p torus-integration-tests native_da_pull_fallback_recovers`
- T7 — harden the push-primary feeding the DA store.
  - swarm.rs `BroadcastNativeActions` (~870): replace the bounded(4) fire-and-forget `try_send` with a queue + retry/backpressure; keep the reconnect re-push (swarm.rs:733-749). Receive path → DA store.
  - validate: `cargo test -p torus-network push_queues_until_peer_connected`
- T8 — re-enable CompactBlock proposals behind the reliable DA (version-gated).
  - app.rs `encode_proposal_datum` (app.rs:469): emit `CompactBlock::from_block(block)` behind a config/version flag (bodies are already mirrored to DA in T2 + pushed in T7). Remove the now-redundant inline-body double-send.
  - UPDATE the existing test `produce_block_datum_is_full_self_contained_block` → `produce_block_datum_is_compact`.
  - Determinism: consensus block identity = `data_hash` over the datum bytes, so compact ≠ full at the consensus layer → ALL validators MUST emit the same encoding. Ship behind a coordinated flag; do NOT change the EVM/RPC header hash (keccak256(canonical_header_bytes)).
  - validate: `cargo test -p torus-consensus compact_proposal_disseminates`
- T9 — MILESTONE: scale proof toward 400k. Bench at rising batch sizes: stalls=0, DA pull-rate stays LOW (push covers the common case), orders/sec climbs; re-run the bs=500 flood that collapsed at 2.56MB (mem 41ca4528) and confirm it holds. Coordinated fresh testnet relaunch on the fixed binary. validate: bench output stalls=0 + DA pull-rate low + no `justify_block_known=false`.

NON-NEGOTIABLE CONSTRAINTS:
- Pull is a RARE fallback, NEVER per-block (mem a6cf33a9). Push stays primary.
- Do NOT sleep/block the consensus thread for the pull (mem 8ee99db3).
- Do NOT remove the linear fast-path in validate_block execution (mem 0efcef9d). Keep `four_node_consensus` + full `cargo test --workspace` green.
- Reuse `/torus/block-data/1.0` as the template; no hotstuff_rs protocol changes.
- Real, surgical code matching surrounding style; minimal blast radius in consensus paths; no placeholders.

GUARDRAILS:
- Do NOT touch the running testnet seed node (PID /tmp/torus-seednode.pid) or testnet/data.
- Do NOT commit the pre-existing unrelated changes (devnet/docker-compose.yml, devnet/scripts/__pycache__, testnet/data.pre-phaseb-c7426e0).
- The 4 monadbft_b3 reputation test failures are PRE-EXISTING (mem 8f6220b5) — do NOT treat them as a regression.
- Commit per task with a clear message ending in the Co-Authored-By footer. Do NOT push; ask first.

Start by reading the docs + the listed memories, confirm a green baseline (`cargo build --release -p torus-node`), then begin Task 5.
