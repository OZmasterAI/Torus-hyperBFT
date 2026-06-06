# Design: Incremental State Root (Phase A)

**Branch:** `cap100-3val-perf` · **Created:** 2026-06-06 · **Status:** BRAINSTORM (pick an option → /writing-plans)

## Problem

The composite state root is recomputed by **scanning the entire chain state every block**, on the **consensus thread**, *twice* per block (proposer + each validator):
- `compute_post_bundle_evm_root` (`state_root.rs:98-184`) iterates **all** `CF_ACCOUNTS` + re-reads **all** storage.
- `compute_native_state_root` (`state_root.rs:51-91`) re-hashes **all** entries of 6 native CFs into one flat keccak.

Cost is **O(total chain state) per block on the critical path**. As state grows, block time grows unboundedly regardless of how many orders are in the block — this is the wall that breaks the **sub-100ms** guarantee at production scale. (On a fresh devnet it's cheap, so the Phase B bench balloon was block *size*, not this — but at real exchange state this dominates.)

## Context (from memory + exploration, all cited)

- **Lifecycle:** CTE (consensus-then-execute). `produce_block` carries `parent.state_root` (`app.rs:885`); the *real* root is computed pre-vote in `proposer.rs:235` (`build_block_with_native`) and `validator.rs:413` (`validate_block_with_native_inner`). `execute_committed_block` runs on a background `torus-execution` thread (`app.rs:400`) and does **not** compute the root.
- **Lag:** block N header root = EVM(N changes over N-1 DB) + native(N-1 DB).
- **Dirty sets are available:** EVM = `BundleState.state` (exact changed accounts+slots); native = `NativeStateOverlay.PendingState.writes` (exact `(cf,key)` set at flush, `backend.rs`).
- **Gifts in-tree:** `CF_TRIE_NODES / CF_TRIE_ACCOUNTS / CF_TRIE_STORAGE` exist (`cf.rs:81-83`) but are **never populated** — the activation point. `reth-trie-sparse v2.0.0` provides `SparseStateTrie` (`reveal_multiproof` → `update_account` → `root_with_updates` returning `(root, TrieUpdates)`) with a `TrieNodeProvider` trait for DB-backed lazy node loading (no built-in persistence — we write that bridge).
- **Native root has NO merkleization** — a flat keccak over all entries. Not an incremental drop-in; needs a keyed structure (sparse Merkle tree / accumulator).
- **Determinism is the #1 risk** (prior mismatch `f3d3f858`). Existing two-DB root-equality test pattern (`native_bridge_tests.rs:293-341`) extends into an "incremental == full-scan" CI gate.
- **Correctness gate is free:** `validator.rs` already compares computed root to header root → any incremental≠full divergence surfaces immediately as `StateRootMismatch` on devnet.

## Options

### Option A — Persistent incremental trie (reth-trie-sparse for EVM + keyed SMT for native)
**How:** Activate `CF_TRIE_NODES`: persist the full MPT node set in RocksDB. Implement `TrieNodeProvider` reading those CFs. Per block, the sparse trie lazily reveals only the paths for the dirty accounts (`BundleState.state`), `update_account` each, `root_with_updates` → new root + `TrieUpdates`, written back in the same `WriteBatch`. For native: introduce a persistent sparse Merkle tree (new `CF_NATIVE_TRIE`) keyed by `(cf,key)`, updating only the `PendingState.writes` leaves. **No O(state) cold start** — the trie *is* the persisted node set, loaded by path on demand (one-time migration builds it once).
**Files:** `state_root.rs` (rewrite both fns), `trie.rs` (sparse provider + native SMT), `backend.rs` (expose dirty keys at flush), `cf.rs` (+`CF_NATIVE_TRIE`), `proposer.rs`/`validator.rs` (callsites), one-time migration + determinism CI gate.
**Trade-offs:** **+** True O(changed)/block → sub-100ms at any state size; Ethereum-compatible EVM root (enables `eth_getProof`); no cold-start scan. **−** Largest surface; crash-consistency of persisted trie nodes; two subsystems (EVM trie + native SMT); determinism-critical.
**Effort:** Large · **Risk:** High

### Option B — In-memory cached trie (rebuild on startup, incremental in RAM)
**How:** Hold the full state trie in memory (EVM + native). Build once on boot from the DB (O(state) cold start). Per block apply only the dirty set and recompute root from changed paths. Nothing persisted; rebuilt on restart.
**Files:** new in-memory trie module (`torus-state`), `state_root.rs`, `backend.rs`, boot wiring, `proposer.rs`/`validator.rs`.
**Trade-offs:** **+** Simpler — no trie-node persistence or crash-consistency; O(changed)/block when warm; no multiproof plumbing. **−** **O(state) cold-start rebuild on every restart** (slow rejoin at scale); full trie in RAM (grows with state); the in-memory structure must deterministically match the canonical root.
**Effort:** Medium · **Risk:** Medium

### Option C — Move the scan off the consensus thread + parallelize (relocate, don't cure)
**How:** Keep full-scan computation but compute the root for height N−k on the background execution thread and embed the already-known lagged root, so the consensus thread never scans. Rayon-parallelize the scan.
**Files:** `app.rs` (exec thread computes+caches root), `proposer.rs`/`validator.rs` (use cached lagged root), `state_root.rs` (rayon).
**Trade-offs:** **+** Smallest change; removes the scan from the consensus critical path immediately; low determinism risk (same computation). **−** Still **O(state)/block** — just relocated, so the exec thread becomes the throughput ceiling as state grows (backpressure); larger lag weakens finality; doesn't cure the fundamental cost. A band-aid.
**Effort:** Small–Medium · **Risk:** Medium (lag semantics caused past mismatch bugs)

## Recommendation

**Option A, sequenced EVM-first then native** — it's the only option that actually *cures* O(state)/block and holds sub-100ms at production scale, and the enabling pieces (`CF_TRIE_*` stubs, `BundleState` dirty set, `reth-trie-sparse`) are already present. Ship it behind a **determinism CI gate** (incremental == full-scan over a corpus) and keep the full-scan as a runtime fallback/oracle until the gate is green across devnet load. EVM-first because `BundleState` + `CF_TRIE_*` + reth-sparse are ready today; the native SMT is the fast-follow.

Option C is worth folding in *opportunistically* (computing A's incremental root on the background thread also removes it from the consensus critical path), but C alone is not a cure.

## Open Questions
1. **Dirty-set capture timing:** the root is computed *before* the overlay flush; the incremental path must snapshot `PendingState.writes` at the right point. Confirm ordering in `execute_committed_block` vs proposer/validator.
2. **One-time migration:** building `CF_TRIE_NODES` from current state at genesis/upgrade — do it lazily or as a boot step? Determinism of the migration.
3. **Native SMT shape:** sparse Merkle tree vs a simpler per-CF Merkle-of-sorted-leaves. SMT gives proofs; the simpler form may suffice if no native proofs are needed externally.
4. **Lag interaction:** does making the EVM root incremental change the existing N/N-1 lag semantics, and does that ripple into the sync/catchup path (`validate_block_for_sync`)?
