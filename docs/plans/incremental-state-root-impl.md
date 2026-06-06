# Implementation Plan: Incremental State Root (Phase A, Option A)

**Branch:** suggest `phase-a-incremental-root` off `cap100-3val-perf` · **Created:** 2026-06-06 · **Status:** PLAN

## Design Decision (from brainstorm)
Option A — **persistent incremental trie**: `reth-trie-sparse` for the EVM root (activate `CF_TRIE_NODES`/`CF_TRIE_STORAGE`), a keyed sparse-Merkle structure for the native root (`CF_NATIVE_TRIE`). Trie nodes live on disk and are loaded by path on demand (no O(state) cold start). **EVM-first**, native fast-follow. Ship behind a **determinism gate** (incremental == full-scan), keeping the full scan as a runtime oracle until green.

Cures the core problem: the root scan that today runs **O(total state), 2×/block on the consensus thread** (`proposer.rs:235`, `validator.rs:413`) becomes **O(changed)/block**, holding sub-100ms as state grows.

## Success Criteria
- **Determinism (non-negotiable):** for any block/corpus, `incremental_root == compute_state_root_from_db` (full scan). Enforced by a CI gate test AND a runtime debug-assert oracle on devnet. No `StateRootMismatch` under devnet load.
- **Persistence:** trie nodes persist to `CF_TRIE_*`; root survives crash/reopen (extend `chaos.rs` pattern).
- **Performance:** state-root computation time is independent of total state size — measured flat block time as a synthetic state grows from 1k → 1M accounts (the proof Phase B's small-state devnet couldn't show).
- **No regressions:** `four_node_consensus` + all existing suites green; sync/catchup (`validate_block_for_sync`) still validates roots correctly.

## Stage A1 — EVM incremental trie (the ready, highest-value half)

### Task A1.1 — Trie-node CF layout + DB-backed `TrieNodeProvider`
- **Test first** (`torus-state` tests): write a few trie nodes via the new store, read them back by `Nibbles` path; absent path → `Ok(None)`; storage nodes keyed by `(hashed_address, path)` don't collide across accounts.
- **Implementation:** in `trie.rs`, define encode/decode for trie nodes; implement `TrieNodeProvider`/`TrieNodeProviderFactory` (from `reth-trie-sparse`) reading `CF_TRIE_ACCOUNTS` (account-trie nodes by path) and `CF_TRIE_STORAGE` (by `hashed_address ++ path`). `cf.rs:81-83` already declares the CFs.
- **Verify:** `cargo test -p torus-state trie_node_provider`
- **Depends on:** —

### Task A1.2 — One-time migration: build persistent trie from current state
- **Test first:** on a DB seeded with N accounts (+storage), `build_trie_to_cf(db)` then read root == `compute_state_root_from_db(db)` (full-scan oracle). Idempotent: running twice yields the same root and node set.
- **Implementation:** `build_trie_to_cf` walks `CF_ACCOUNTS` + storage once, constructs the MPT, persists **all** nodes to `CF_TRIE_*`, stores the root marker. Runs at boot iff `CF_TRIE_*` empty (or behind an explicit migrate step). Deterministic ordering.
- **Verify:** `cargo test -p torus-state trie_migration_root_matches_full_scan`
- **Depends on:** A1.1

### Task A1.3 — `incremental_evm_root(db, bundle) -> (B256, TrieUpdates)`
- **Test first** (the **EVM determinism gate**): start from a migrated trie; apply a `BundleState` (varied: new account, balance change, storage insert/delete, account deletion/EIP-161); assert incremental root == full-scan root of the post-bundle state, across a randomized corpus.
- **Implementation:** build `SparseStateTrie` (blind, seeded with stored root) over the A1.1 provider; for each `addr` in `bundle.state` call `update_account` (lazily reveals path, applies dirty storage slots, recomputes storage root); `root_with_updates()` → `(root, TrieUpdates)`. Mirror EIP-161 clearing already in `state_root.rs:137`.
- **Verify:** `cargo test -p torus-state incremental_evm_root_equals_full_scan`
- **Depends on:** A1.2

### Task A1.4 — Persist `TrieUpdates` in the commit WriteBatch
- **Test first:** execute a block, persist updates, drop & reopen the DB, recompute root from `CF_TRIE_*` → equals the pre-crash root (extend `chaos.rs:162` crash/reopen pattern).
- **Implementation:** in the commit path (`BlockCommitter`/`execute_committed_block`, `app.rs`), fold `TrieUpdates` (changed + removed nodes) into the same atomic `WriteBatch` as the EVM state write, so trie and state commit together (crash-consistent).
- **Verify:** `cargo test -p torus-state trie_survives_crash_reopen`
- **Depends on:** A1.3

### Task A1.5 — Swap EVM callsites to incremental, keep full-scan as oracle
- **Test first:** a debug-only assertion in `compute_post_bundle_state_root`/`compute_full_composite_root` that, when `cfg!(debug_assertions)` or an env flag is set, computes BOTH incremental and full-scan and asserts equal. Unit test confirms the oracle fires on an injected divergence.
- **Implementation:** point `proposer.rs:235`, `validator.rs:169/268/413`, and `state_root.rs` EVM path at `incremental_evm_root`. Gate the full-scan oracle behind a flag (default on for devnet, off in release). The existing `validator.rs` `StateRootMismatch` check is the live safety net.
- **Verify:** `cargo test -p torus-bridge -p torus-consensus` incl. `four_node_consensus`; devnet smoke (no mismatch).
- **Depends on:** A1.4

### Task A1.6 — Scaling proof: flat root time vs state size
- **Test first / bench:** synthetic state at 1k / 100k / 1M accounts; measure root-computation time per block — assert it does **not** grow with state size (vs the full scan, which does). Devnet: block time stays sub-100ms under load with a pre-grown state snapshot.
- **Implementation:** a bench harness (extend `bench-throughput` matching-engine mode or a dedicated state-root bench) + a devnet run with a large seeded genesis.
- **Verify:** bench output shows flat incremental time; full-scan grows.
- **Depends on:** A1.5

## Stage A2 — Native incremental root (fast-follow)

> Native root is today a flat keccak over all entries of 6 CFs (`state_root.rs:51-91`) — no merkleization, so this stage introduces a keyed structure. Its own `-impl.md` once A1 lands; outline:

- **A2.1** Native trie structure + `CF_NATIVE_TRIE`: keyed by `(cf,key)` → `keccak(value)`. Decide **sparse Merkle tree** (gives proofs) vs **Merkle-of-sorted-leaves** (simpler; pick if no external native proofs needed — Open Q3). Test: persisted-trie native root == `compute_native_state_root` (full) on a corpus + migration.
- **A2.2** Incremental update from `NativeStateOverlay.PendingState.writes` at **flush time** (`backend.rs`) — the lag means the header's native root is the persisted N−1 root, updated when N's overlay flushes in `execute_committed_block`. Capture the dirty `(cf,key)` set there. Test: incremental native root == full after a flush; crash/reopen survives.
- **A2.3** Swap `compute_native_state_root` callsites; native determinism oracle; reconcile the 6-CF vs 5-CF inconsistency between `state_root.rs:51` and `trie.rs:90`.

## Cross-cutting
- **Determinism CI gate:** a dedicated test target running the incremental-vs-full corpus (EVM in A1.3, native in A2.1) wired into the workspace test run; treat any divergence as a hard CI failure.
- **Lag/sync (Open Q4):** verify `validate_block_for_sync` and catchup still compute/compare roots correctly once incremental; the lag semantics (N over N−1) must be unchanged.
- **Rollback:** the full-scan functions stay in the tree as the oracle; reverting = flip the callsites back. `CF_TRIE_*` become dead but harmless. No consensus-format change (roots are byte-identical by construction — that's the gate).

## Verification (end-to-end)
1. `cargo test --workspace` green incl. determinism gate + `four_node_consensus`.
2. Fresh devnet: no `StateRootMismatch`; batched flood (Phase B bench `--batch-size`) holds sub-100ms with the full-scan oracle ON.
3. Large-state devnet (seeded genesis): block time flat vs the same load on the full-scan build — the production-scale proof.

## Open Questions (carried from design; resolve during A1)
1. Dirty-set capture timing (native, vs flush) — A2.2.
2. One-time `CF_TRIE` migration: boot-lazy vs explicit step — A1.2.
3. Native SMT vs Merkle-of-sorted-leaves — A2.1.
4. Lag/sync interaction once EVM root is incremental — cross-cutting.
