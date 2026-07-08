# Phase A — Incremental State Root (`/implement`) prompt

## Refresh notes (S426) — READ THIS FIRST

**Status changed materially since this prompt was written (pre-O2-split, pre-S420-rustfmt).**
Verified against the working tree at commit `6e03294` (origin tip / fleet pin; the only uncommitted
deltas are the BS-4a/4b DA-recovery work in `crates/torus-consensus/src/app.rs` +
`crates/torus-telemetry/src/lib.rs`, unrelated to state root).

**Phase A is essentially ALREADY IMPLEMENTED and merged.** This prompt originally described building
A1.1–A1.6 and A2 from scratch; that work has since landed. Do NOT re-implement it. What remains is
verification/validation and CI-gating (see "Remaining work"). Concretely, in the current tree:

- **A1.1 (trie node storage)** — DONE, realized differently than the prompt assumed. There is NO
  `reth-trie-sparse` `SparseStateTrie` / `TrieNodeProvider`; reth's `StateRoot` engine drives
  DB-backed cursor factories `RocksTrieCursorFactory` / `RocksHashedCursorFactory` + helpers
  `account_trie_key` / `storage_trie_key` / `write_trie_updates`, all in
  `crates/torus-state/src/trie_cursor.rs`. Nodes persist to `CF_TRIE_ACCOUNTS` / `CF_TRIE_STORAGE`;
  a keccak-ordered hashed mirror lives in `CF_HASHED_ACCOUNTS` / `CF_HASHED_STORAGE` (cf.rs:91–104).
- **A1.2 migration** — DONE: `build_trie_to_cf` / `ensure_trie_built` / `is_trie_built`
  (`crates/torus-state/src/incremental.rs`); oracle is `compute_state_root_from_db` (`trie.rs:50`).
  Test: `trie_migration_root_matches_full_scan`.
- **A1.3 incremental EVM root** — DONE: `incremental_evm_root(db,bundle)->(B256,TrieUpdates)`
  (`incremental.rs:219`) via `StateRoot` + a `HashedPostStateCursorFactory` overlay (NOT
  `SparseStateTrie`). Oracle `full_post_bundle_evm_root` (`incremental.rs:148`). Tests:
  `incremental_evm_root_equals_full_scan` + `_randomized`.
- **A1.4 atomic persist** — DONE: `commit_evm_bundle_incremental` (`incremental.rs:355`), wired into
  the commit path at `app.rs:273` with a plain-commit fallback. Test: `trie_survives_crash_reopen`.
- **A1.5 callsite swap + oracle + flag** — DONE: routing lives in `state_root.rs`
  (`evm_root_routed`/`flagged_evm_root`, env `TORUS_INCREMENTAL_STATE_ROOT` **default ON**, plus a
  runtime divergence oracle under `debug_assertions` or `TORUS_INCREMENTAL_ORACLE`). Callsites
  already route through it (proposer.rs:126/255, validator.rs:183/279/433); the `StateRootMismatch`
  net is intact (validator.rs:186/282/436). NOT in the original plan but present:
  `resync_evm_accounts` (`incremental.rs:377`, called at `app.rs:470`) repairs trie drift after
  native post-commit credits EVM balances straight to `CF_ACCOUNTS`.
- **A1.6 scaling proof** — a code proof exists as `#[ignore]`d test
  `incremental_evm_root_scales_flat_vs_full_scan` (`incremental.rs`) + a `bench-throughput
  state-root` mode. The large-state DEVNET proof (flat block time under load) is the piece still
  worth running/recording.
- **A2 native incremental root** — DONE (own doc `docs/plans/incremental-state-root-native-impl.md`).
  Bucketed Merkle tree over `CF_NATIVE_TRIE` (+`CF_NATIVE_HASHED`) in
  `crates/torus-state/src/native_trie.rs`: `native_root_full` (oracle), `persisted_native_root`,
  `build/ensure_native_trie_built`, `commit_native_trie_incremental`, `apply_native_dirty_to_batch`.
  Wired via `flagged_native_root` (proposer.rs:254, validator.rs:431) and
  `overlay.flush_with_native_trie` (app.rs:459); boot build at app.rs:1049. Test:
  `incremental_native_root_equals_full_scan`. A2.3's "reconcile 6-CF vs 5-CF" is DONE — the
  divergent 5-CF `compute_native_state_root_from_db` was REMOVED (trie.rs:86–89); the native root is
  now the 6-CF bucketed-Merkle root, single source of truth.

**Remaining work (honest scope of any new /implement session):**
1. THE OPEN ITEM — large-state DEVNET bake: flat block time (A1.6 runtime proof) + sustained
   no-`StateRootMismatch` under native-flood + tx-loop with the oracle ON, on a large pre-grown
   state. This is the only genuinely-unfinished piece.
2. (Optional top-up) The determinism CI gate already EXISTS —
   `chaos.rs:327 native_incremental_root_matches_full_scan_under_real_execution` runs incremental
   vs full under real execution every block, plus the torus-state unit gates. Only gap worth
   closing: an EVM-under-real-execution CI gate mirroring the native one (the EVM side is covered by
   unit gates + the runtime oracle, not a real-execution integration gate).
3. Decide production default / whether to keep the runtime oracle on in release; fix a stale code
   comment — `state_root.rs:18` calls the flag "default ON / devnet-baked" while the `app.rs` boot
   comments (1042/1048) still say "full-scan root stays primary until enabled". The flag default
   (ON) is the source of truth.

**Base branch:** create a NEW branch off `6e03294` (e.g. `perf/o6-incremental-state-root`); the old
`phase-c-native-da` / `cap100-3val-perf` bases are gone/merged. The working tree currently carries
uncommitted BS-4a/4b changes in `app.rs` — branch from clean `6e03294`. Those changes touch the
`DaRecoveryWorker` region (~app.rs:818+), NOT the state-root commit path (~app.rs:245–473), so there
is no code overlap, but do not carry them into a state-root branch.

**Unverifiable / flagged:** the "4 monadbft_b3 pre-existing failures" claim and the
seed-node/guardrail paths below (PID file, testnet/data.pre-phaseb-c7426e0, docker-compose deltas)
are old and were NOT re-checked against the current fleet — verify before relying on them.

---

Copy-paste everything below the `---` into a fresh (clear-context) session to implement Phase A
(incremental state root). Plan: `docs/plans/incremental-state-root-impl.md` · PRP:
`PRPs/incremental-state-root.tasks.json` · Design: `docs/plans/incremental-state-root.md`.
NOTE: those three plan docs are themselves stale (they predate the merge above and still name the
`cap100-3val-perf` base); treat this Refresh-notes block as authoritative over them.

Phase C native-action DA is DONE and long since merged into `6e03294`. Phase A (this doc) has ALSO
landed — the remaining track is validation + CI-gating, not fresh implementation.

---

/implement docs/plans/incremental-state-root-impl.md  — Phase A. NOTE (S426): Stage A1 (A1.1–A1.6) and Stage A2 + cross-cutting are ALREADY implemented and merged into `6e03294`. This is now a VALIDATE-AND-FINISH pass: confirm every `validate` is green on a fresh branch, then run the one open item (the A1.6 large-state devnet bake). Do NOT re-implement.

GOAL (REVISED — see Refresh notes): Phase A incremental state root is ALREADY implemented (Option A: a persistent reth `StateRoot`-driven EVM trie via DB-backed cursor factories — NOT `reth-trie-sparse` — plus a bucketed-Merkle native root). The remaining job is to VALIDATE + CI-GATE it, not build it. It killed the O(total-state) state-root recompute that ran 2x/block on the consensus thread — the callsites are now routed through `state_root.rs::flagged_evm_root` / `flagged_native_root` (proposer.rs:126/255, validator.rs:183/279/433), so block time stays sub-100ms as chain state grows. CONSENSUS-CRITICAL: determinism over speed — the incremental root MUST be byte-identical to the full scan, every block, or consensus splits (enforced by the runtime oracle + unit gates).

WHY NOW / RELATIONSHIP: orders/sec = orders/block × blocks/sec. Phase C native-DA (out-of-band bodies + compact proposals) removed the orders/block wall and is long since merged into `6e03294`. Phase A removed the blocks/sec wall (incremental root) and has ALSO landed. Both walls are down in-tree; what is left is proving the block-speed win at large state and hard-gating determinism in CI.

BRANCH: create a NEW branch off `6e03294` (the origin tip / fleet pin), e.g. `perf/o6-incremental-state-root`. The old `phase-c-native-da` / `cap100-3val-perf` bases named by the plan docs are gone/merged — do NOT base on them. Branch from a CLEAN `6e03294`; the working tree's uncommitted BS-4a/4b changes (app.rs `DaRecoveryWorker`, ~app.rs:818+) are unrelated and must not be carried along.

READ FIRST (in this order):
1. Plan:    docs/plans/incremental-state-root-impl.md   (Stage A1 tasks A1.1–A1.6, Stage A2 outline, success criteria, rollback, open questions)
2. PRP:     PRPs/incremental-state-root.tasks.json       (per-task files + `validate` commands + deps)
3. Design:  docs/plans/incremental-state-root.md         (Options A/B/C; A chosen EVM-first; cited landmarks)
4. Memory — run: run_tool("memory","search_knowledge",{"query":"incremental state root reth-trie-sparse EVM trie native SMT determinism StateRootMismatch dirty set BundleState overlay"})
   then get_memory each (use these FULL ids — 8-char prefixes fail):
   - fc16d7c27f657f3b  Phase A PLAN (Option A persistent incremental trie)
   - 6b1865989c0beebc  Phase A DESIGN (kill O(total-state) root, runs 2x/block on consensus thread)
   - f3d3f8581843a376  prior StateRootMismatch ROOT CAUSE — determinism is the #1 risk
   - 0efcef9de231cc5a  CRITICAL: linear fast-path in validate_block is LOAD-BEARING — do NOT remove (you WILL edit validator/commit paths)
   - e1c8efd0bfe4b4cd  perf roadmap (state-root + double secp256k1 verify are the block-speed costs)
   - 41ca452897ba30bd  bs=500 collapse (the OTHER wall — orders/block — already fixed by Phase C)
   - 8f6220b5a7a76074  4 monadbft_b3 reputation tests are PRE-EXISTING failures — NOT a regression

KEY LANDMARKS (S426-verified line numbers; all IMPLEMENTED — re-verify before touching, minor drift expected):
- Trie/hashed CFs — DECLARED AND POPULATED (Phase A activated them): CF_TRIE_NODES=cf.rs:91, CF_TRIE_ACCOUNTS=cf.rs:94, CF_TRIE_STORAGE=cf.rs:97; hashed mirror CF_HASHED_ACCOUNTS=cf.rs:102, CF_HASHED_STORAGE=cf.rs:104; native CF_NATIVE_TRIE=cf.rs:110, CF_NATIVE_HASHED=cf.rs:115. All in ALL_CF_NAMES.
- The EVM engine is reth `StateRoot` (reth-trie), NOT `reth-trie-sparse`/`SparseStateTrie`. Persistence bridge = DB-backed `RocksTrieCursorFactory` + `RocksHashedCursorFactory` + `write_trie_updates` in trie_cursor.rs (RocksTrieCursorFactory=trie_cursor.rs:187, RocksHashedCursorFactory=trie_cursor.rs:362, write_trie_updates=trie_cursor.rs:135). Per-block overlay = `HashedPostStateCursorFactory` over `HashedPostState::from_bundle_state`.
- Dirty sets are exact + free: EVM = BundleState.state (changed accounts/slots); native = the overlay dirty-key set, applied at flush via `overlay.flush_with_native_trie` (app.rs:459) → `apply_native_dirty_to_batch` (native_trie.rs:491).
- Full-scan ORACLES (kept in-tree, the determinism reference): EVM `compute_post_bundle_evm_root` (state_root.rs:172) and `full_post_bundle_evm_root` (incremental.rs:148); native `native_root_full` (native_trie.rs:213). The old flat-keccak `compute_native_state_root` (state_root.rs:125) still exists but is superseded by the bucketed-Merkle native root; the divergent 5-CF variant was REMOVED (trie.rs:86–89).
- Routed callsites (already swapped in A1.5/A2): proposer.rs:126 (build_block → compute_post_bundle_state_root), proposer.rs:254/255 (build_block_with_native → flagged_native_root + compute_full_composite_root); validator.rs:183 (validate_block_inner), validator.rs:279 (validate_block_for_sync), validator.rs:431/433 (catchup native root). StateRootMismatch at validator.rs:186/282/436.
- Lifecycle (CTE): root computed PRE-vote in proposer/validator; execute_committed_block runs on the background `torus-execution` thread (spawned at app.rs:1058) and does the atomic commit (commit_evm_bundle_incremental=app.rs:273; native flush+trie=app.rs:459; resync_evm_accounts=app.rs:470). Boot trie build: ensure_trie_built=app.rs:1043, ensure_native_trie_built=app.rs:1049. Lag: block N header = EVM(N over N-1) + native(N-1) — unchanged.
- Determinism oracle: (a) runtime cross-check in evm_root_routed/native_root_routed (state_root.rs:44–54 / 73–81); (b) unit gates incremental_evm_root_equals_full_scan + _randomized + sequential_incremental_commits_match_full_scan (incremental.rs), incremental_native_root_equals_full_scan (native_trie.rs:658). The old two-DB native determinism test is native_bridge_tests.rs:369–374 (782-line file), still on the flat-keccak `compute_native_state_root` — extend from the incremental gates, not this one.

TASKS — STATUS (A1.1–A1.6, A2, and the cross-cutting gate are all DONE in-tree; the ONE genuine open item is the devnet bake, A1.6-runtime. Re-run each `validate` to confirm green on your new branch before touching anything):
- A1.1 — DONE. Trie-node CF layout + DB-backed cursor factories over CF_TRIE_ACCOUNTS/STORAGE (storage keyed hashed_address++path, no cross-account collision). Landed as trie_cursor.rs (RocksTrieCursorFactory/RocksHashedCursorFactory/write_trie_updates), NOT a `TrieNodeProvider` trait. validate: cargo test -p torus-state (trie_cursor tests) — confirm green.
- A1.2 — DONE. build_trie_to_cf; persisted root == compute_state_root_from_db (full-scan oracle); idempotent; deterministic. validate: cargo test -p torus-state trie_migration_root_matches_full_scan.
- A1.3 — DONE. incremental_evm_root(db,bundle) -> (B256, TrieUpdates) via reth `StateRoot` + HashedPostState overlay (NOT SparseStateTrie); EVM DETERMINISM GATE green over explicit + randomized corpus. validate: cargo test -p torus-state incremental_evm_root_equals_full_scan.
- A1.4 — DONE. TrieUpdates persisted in the SAME atomic WriteBatch as the EVM state write (commit_evm_bundle_incremental); survives drop+reopen. validate: cargo test -p torus-state trie_survives_crash_reopen.
- A1.5 — DONE. Callsites routed through state_root.rs (flagged_evm_root/flagged_native_root); full-scan kept as a runtime ORACLE under debug/TORUS_INCREMENTAL_ORACLE; env TORUS_INCREMENTAL_STATE_ROOT (default ON — the flag flip already happened); validator StateRootMismatch is the live net. Also added: resync_evm_accounts for native-post-commit drift. validate: cargo test -p torus-bridge -p torus-consensus (incl four_node_consensus).
- A1.6 — PARTIAL / THE OPEN ITEM. Code proof exists as #[ignore]d test incremental_evm_root_scales_flat_vs_full_scan + `bench-throughput state-root`. REMAINING = the devnet bake: rebuild release, 4-node docker with flag+oracle ON, native-flood + tx-loop, prove 0 divergence and flat sub-100ms block time under a large pre-grown state. validate: devnet run shows flat incremental time + zero StateRootMismatch.
- A2 — DONE (own doc docs/plans/incremental-state-root-native-impl.md). Bucketed-Merkle native root over CF_NATIVE_TRIE/CF_NATIVE_HASHED (native_trie.rs), updated from the overlay dirty (cf,key) set at flush; 6-CF vs 5-CF inconsistency RESOLVED (5-CF variant removed, trie.rs:86–89). validate: cargo test -p torus-state incremental_native_root_equals_full_scan.
- Cross-cutting — DONE. Determinism CI gate present: chaos.rs:327 native_incremental_root_matches_full_scan_under_real_execution (REAL NativeExecutor→overlay→flush_with_native_trie, asserts persisted==full every block, runs in CI) + the torus-state unit gates. Sync verified: validate_block_for_sync→validate_block→flagged_native_root (validator.rs:431); validate_block_for_catchup is EVM-only; N-1 native lag preserved. If you add an EVM-under-real-execution CI gate to mirror the native one, that's the only worthwhile top-up.

NON-NEGOTIABLE CONSTRAINTS:
- DETERMINISM IS #1: incremental_root == full-scan root, byte-identical, for every block/corpus (prior mismatch f3d3f858). Keep the full-scan functions in-tree as the oracle until the gate is green across devnet load; do NOT delete them.
- Do NOT remove/weaken the linear fast-path in validate_block (mem 0efcef9d) — you will be editing validator.rs/the commit path; keep it intact.
- NO consensus-format change: roots are byte-identical by construction (that's the gate), so unlike Phase C's compact flag this needs NO coordinated relaunch.
- Keep four_node_consensus + the full existing suites green. Crash-consistency: trie nodes + state commit in ONE atomic WriteBatch.
- Real, surgical code matching surrounding style; minimal blast radius on the consensus/commit hot paths; no placeholders. (Both EVM and native halves already exist — any new code is validation harness / CI gate, not root logic.)

GUARDRAILS:
- Do NOT touch a running seed/testnet node or its data dir; local devnet only for scaling runs. (Old prompt named PID /tmp/torus-seednode.pid + testnet/data — VERIFY the current fleet's paths before assuming.)
- Branch from a CLEAN `6e03294`. Do NOT carry the working tree's uncommitted BS-4a/4b DA-recovery changes (app.rs + torus-telemetry/src/lib.rs) into a state-root branch, and do NOT commit unrelated stray files.
- The "4 monadbft_b3 pre-existing failures" claim (mem 8f6220b5) is OLD and unverified against the current tree — re-check whether it still holds before dismissing any red test.
- Commit per task, message ending in the Co-Authored-By footer. Do NOT push; ask first.

Start by reading this Refresh-notes block + the docs + the listed memories, confirm a green baseline (cargo build --release -p torus-node; cargo test -p torus-state -p torus-bridge; the chaos.rs determinism gate), and REVERIFY each task's `validate` is already green. Then move straight to the one open item: the A1.6 large-state devnet bake (flat sub-100ms block time, zero StateRootMismatch with the oracle ON). Do NOT re-implement A1.x/A2.
