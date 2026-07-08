# Finish Phase A — Incremental State Root (`/implement`) prompt

Copy everything below the `---` into a fresh (clear-context) session to FINISH Phase A.
- **Plan:** `docs/plans/incremental-state-root-impl.md`  (Stage A1 ✅ done; Stage A2 outline + success criteria + rollback + open questions live here)
- **PRP:** `PRPs/incremental-state-root.tasks.json`  (task 6 = A1.6, task 200 = A2, task 210 = cross-cutting)
- **Design:** `docs/plans/incremental-state-root.md`  (Option A chosen; native SMT-vs-Merkle is Open Q3)

Stage A1 (EVM incremental root) is **DONE + devnet-validated** on branch `phase-a-incremental-root`
(6 commits `039457b`..`76e3b12`, flag-gated OFF). This prompt finishes the rest of Phase A:
**A1.6 scaling proof → Stage A2 (native incremental root) → cross-cutting CI gate → enable + devnet bake.**

---

/implement docs/plans/incremental-state-root-impl.md  — FINISH Phase A: A1.6 (scaling proof), then Stage A2 (native incremental root — write its own -impl.md first), the cross-cutting determinism CI gate + sync/lag verification, and finally enable + devnet-bake the flag.

GOAL: Complete Phase A — incremental state root — so the composite root `keccak(evm_root ‖ native_root)` is O(changed)/block on BOTH halves, holding block time sub-100ms as chain state grows. Stage A1 already made the EVM half incremental (reth `StateRoot`, behind `TORUS_INCREMENTAL_STATE_ROOT`, default off, devnet-validated). The NATIVE half is still the wall: `compute_native_state_root` is a flat keccak over 6 native CFs recomputed 2×/block on the consensus thread (`proposer.rs:235`, `validator.rs:414`). CONSENSUS-CRITICAL: determinism over speed — incremental MUST be byte-identical to the full scan, every block, or consensus splits.

STATE / WHAT'S ALREADY DONE (branch `phase-a-incremental-root`, head `76e3b12`, NOT pushed):
- A1.1 `039457b` — trie-node CFs + RocksDB `TrieCursorFactory` (crates/torus-state/src/trie_cursor.rs).
- A1.2 `59cbb2d` — hashed-state cursors + `build_trie_to_cf` migration (root == full-scan).
- A1.3 `6ddb6a9` — `incremental_evm_root` via reth `StateRoot` overlay + EVM determinism gate (explicit + 40-round fuzz).
- A1.4 `85ea552` — crash-consistent atomic commit (`commit_evm_bundle_incremental`; survives drop+reopen).
- A1.5 `76173cb` — wired into consensus, **flag-gated** (`TORUS_INCREMENTAL_STATE_ROOT` default OFF) + runtime oracle (`TORUS_INCREMENTAL_ORACLE`).
- A1.5 fix `76e3b12` — `resync_evm_accounts` after native post-commit (see CRITICAL LESSON).
- EVM half: `torus-state` 29+4+32 green, `four_node_consensus` green, loaded devnet (flag+oracle, 195→895 blocks) 0 divergence.

BRANCH: continue on `phase-a-incremental-root` (head `76e3b12`). Do NOT branch again; Phase A converges here.

READ FIRST (in this order):
1. Plan:   docs/plans/incremental-state-root-impl.md   (Stage A2 outline at the bottom + success criteria + rollback + open Qs)
2. PRP:    PRPs/incremental-state-root.tasks.json       (task 6 / 200 / 210)
3. Design: docs/plans/incremental-state-root.md         (native structure = Open Q3: SMT vs Merkle-of-sorted-leaves)
4. Memory — run: run_tool("memory","search_knowledge",{"query":"Phase A native incremental root compute_native_state_root CF_NATIVE_TRIE overlay dirty set flush determinism oracle devnet smoke"})
   then get_memory each (use these FULL 16-char ids — 8-char prefixes fail):
   - e4bbaf3d82125bf0  Phase A A1 COMPLETE handoff (what's done / what's left)
   - da3067e54910f2aa  A1.5 native-post-commit FIX verified (the CRITICAL LESSON below)
   - 9bfba03ce066fe63  A1.5 done (flag-gated wiring summary)
   - 87ed0dc4d17ce16e  A1 engine decision (reth StateRoot + DB cursor factories)
   - 6e50b0600c118b59  write_trie_updates MUST delete-before-put (RocksDB WriteBatch order)
   - 09f928594521f246  lifecycle: root pre-vote on consensus thread; native = N-1 lag
   - 333ffef64cec393f  incremental-root LIVE PATH map + overlay dirty set
   - 0efcef9de231cc5a  linear fast-path in validate_block is LOAD-BEARING (do NOT remove)
   - 8f6220b5a7a76074  4 monadbft_b3 reputation tests are PRE-EXISTING failures (not a regression)
   - aa0b20d4dbb11d1f  reth-trie StateRoot/cursor API (if mirroring reth for native)

CRITICAL LESSON — this is WHY the loaded devnet smoke is non-negotiable (do not skip it):
A1.5's devnet smoke caught a determinism bug that EVERY unit test missed. Native post-commit (fee
distribution to treasury/dev_pool, validator rewards) credited EVM account balances straight to
CF_ACCOUNTS via `NativeStateOverlay::flush`, bypassing trie maintenance → the incremental root
drifted from the full scan on every fee-bearing block. The runtime oracle caught it (rejected the
blocks; no consensus split) and the off-by-default flag kept production safe. Fix: `resync_evm_accounts`
re-syncs the trie for the accounts the overlay dirtied (`dirty_evm_accounts`).
**The SYMMETRIC risk dominates A2**: EVERY writer of a native CF — `NativeExecutor` on the overlay,
`distribute_fees`, `process_governance`, `process_epoch_boundary`, `drain_core_writer`, the nonce
writes, epoch rotation — must flow into `CF_NATIVE_TRIE`. Therefore: (a) keep the runtime oracle
(incremental == full, fail loud) wired for native too, and (b) a LOADED devnet smoke
(native-order-flood + tx-loop, flag+oracle ON) is REQUIRED before enabling. Unit tests alone are
INSUFFICIENT — they missed the EVM bug. Generalize the dirty-key/resync pattern, don't hand-enumerate writers.

KEY LANDMARKS (verify in-tree before editing — line numbers drift):
- Native root TODAY: `compute_native_state_root` (crates/torus-bridge/src/state_root.rs:89) — flat
  keccak over **6 CFs**: CF_NATIVE_BALANCES, CF_NATIVE_ORDER_BOOKS, CF_NATIVE_POSITIONS,
  CF_NATIVE_ORACLE, CF_STAKING_DELEGATIONS, CF_STAKING_VALIDATORS. No merkleization → needs a keyed structure.
- 6-CF vs 5-CF INCONSISTENCY (reconcile in A2.3): `trie.rs:90` `compute_native_state_root_from_db`
  uses only **5 CFs** (MISSING CF_NATIVE_ORDER_BOOKS). The state_root.rs 6-CF version is the
  consensus-authoritative one used by proposer/validator — the trie.rs 5-CF helper is divergent.
- `CF_NATIVE_TRIE` does NOT exist yet (cf.rs declares only CF_TRIE_*/CF_HASHED_*). A2.1 adds it (cf.rs + ALL_CF_NAMES).
- Native callsites: proposer.rs:235-236 + validator.rs:414-416 (`compute_native_state_root` →
  `compute_full_composite_root`). NOTE: `compute_full_composite_root` already flag-routes the EVM half
  via `flagged_evm_root`; A2 should flag-route the NATIVE half the same way (inside
  `compute_native_state_root`, or a parallel `flagged_native_root`).
- Dirty set: `NativeStateOverlay.PendingState.{writes,deletes}` ((cf,key) at flush, backend.rs).
  `dirty_evm_accounts()` is the reusable accessor pattern — generalize to native (cf,key) dirty keys.
- Maintenance hook: app.rs `execute_committed_block` native block (`overlay.flush(...)` ~L385, right
  where `resync_evm_accounts` is already called). Native trie maintenance + resync go here, atomically.
- MIRROR the EVM code: crates/torus-state/src/incremental.rs (`build_trie_to_cf`, `incremental_evm_root`,
  `commit_evm_bundle_incremental`, `resync_evm_accounts`) + trie_cursor.rs (cursor factories).
- Flag/oracle pattern: `incremental_state_root_enabled()` + `evm_root_routed` in state_root.rs;
  reuse `TORUS_INCREMENTAL_STATE_ROOT` (one flag for both halves) + `TORUS_INCREMENTAL_ORACLE`.

TASKS (TDD — write the FAILING test first; run the `validate`; paste output; only proceed on green; commit per task):
- A1.6 — Scaling proof: synthetic state 1k / 100k / 1M accounts; measure per-block root-compute time
  — assert incremental is FLAT vs full-scan's linear growth. Devnet leg: block time stays sub-100ms
  under load with a pre-grown seeded genesis. validate: bench output shows flat incremental, growing full-scan.
- A2.0 — Write `docs/plans/incremental-state-root-native-impl.md` (brainstorm → writing-plans): pick
  the native structure — sparse Merkle tree (gives proofs) vs Merkle-of-sorted-leaves (simpler; pick
  if no external native proofs needed, Open Q3). Keyed by (cf,key) → keccak(value).
- A2.1 — Native trie structure + `CF_NATIVE_TRIE`; persisted native root == `compute_native_state_root`
  (full, 6-CF) over a corpus; one-time migration; deterministic ordering; idempotent.
  validate: cargo test -p torus-state native_trie_root_matches_full_scan
- A2.2 — Incremental native root from `NativeStateOverlay` dirty (cf,key) set at flush (backend.rs);
  honor the N-1 native lag (header native root = persisted N-1, updated when N's overlay flushes in
  execute_committed_block); persist in the SAME atomic batch; survives crash/reopen.
  validate: cargo test -p torus-state incremental_native_root_equals_full_scan (+ crash test)
- A2.3 — Swap `compute_native_state_root` callsites to flag-gated incremental + native determinism
  oracle (incremental==full, fail loud); RECONCILE the 6-CF vs 5-CF (state_root.rs:89 vs trie.rs:90).
  validate: cargo test -p torus-bridge -p torus-consensus incl four_node_consensus
- Cross-cutting — Determinism CI gate: wire the incremental==full corpus (EVM A1.3 + native A2.1/A2.2)
  into the workspace test run as a HARD CI failure on divergence; verify `validate_block_for_sync` /
  catchup still compute/compare roots with the N/N-1 lag semantics unchanged.
  validate: workspace determinism target green; sync/catchup path verified.
- ENABLE + BAKE (final, REQUIRED before default-on): LOADED devnet smoke — fresh 4-node docker devnet,
  `TORUS_INCREMENTAL_STATE_ROOT=1` + `TORUS_INCREMENTAL_ORACLE=1` (docker-compose.override.yml), drive
  native-order-flood.py + tx-loop.sh together; assert 0 divergence / 0 "EVM execution failed" over
  sustained EVM+native load (this is what caught the A1.5 bug). Then flip the flag default ON.

NON-NEGOTIABLE CONSTRAINTS:
- DETERMINISM IS #1: incremental == full-scan, byte-identical, every block/corpus. Keep the full-scan
  functions in-tree as the oracle until the gate is green across LOADED devnet.
- The LOADED devnet smoke (EVM + native flood, flag+oracle ON) is MANDATORY before enabling — unit
  tests missed the A1.5 determinism bug. Re-use the docker devnet + override-env pattern.
- EVERY native-CF writer must update CF_NATIVE_TRIE (symmetric to the A1.5 lesson). Prefer a generalized
  dirty-key → resync pass over hand-enumerating writers.
- Do NOT remove/weaken the LOAD-BEARING linear fast-path in validate_block (mem 0efcef9d).
- NO consensus-format change: roots byte-identical by construction. Native N-1 lag unchanged. No coordinated
  relaunch needed to flip the flag, but all nodes must run a binary that MAINTAINS the tries.
- Keep four_node_consensus + the full existing suites green. Crash-consistency: native trie + native CF
  writes in ONE atomic WriteBatch.
- Real, surgical code matching surrounding style; minimal blast radius on consensus/commit hot paths; no placeholders.

GUARDRAILS:
- Do NOT touch any live testnet seed node or testnet/data; LOCAL docker devnet only (host ports 8645+,
  docker-compose.override.yml for the flags, `docker compose down -v` + remove the override after).
- REBUILD RELEASE (`cargo build --release -p torus-node`) before any devnet — the image copies
  target/release/torus-node; a stale binary silently runs old code.
- Do NOT commit the pre-existing unrelated changes (devnet/docker-compose.yml, devnet/scripts/__pycache__,
  testnet/data.pre-phaseb-c7426e0, prompts/*.md).
- The 4 monadbft_b3 reputation test failures are PRE-EXISTING (mem 8f6220b5) — not a regression.
- Commit per task, message ending in the Co-Authored-By footer. Do NOT push; ask first.

Start by confirming a green baseline (cargo build --release -p torus-node; cargo test -p torus-state -p torus-bridge; four_node_consensus), reading the docs + listed memories, verifying the landmarks in-tree, then begin A1.6 → A2.0 (write the native -impl.md) → A2.1.
