# Implementation Plan: Native Incremental State Root (Phase A, Stage A2)

**Branch:** `phase-a-incremental-root` · **Created:** 2026-06-07 · **Status:** PLAN (structure decided)
**Parent plan:** `docs/plans/incremental-state-root-impl.md` (Stage A1 = EVM half, DONE + devnet-validated)
**Design:** `docs/plans/incremental-state-root.md` (Option A; native structure = Open Q3, resolved here)

## Problem

The composite block root is `keccak256(evm_root ‖ native_root)`. Stage A1 made the **EVM** half
incremental (reth `StateRoot`, O(changed)/block — measured flat: 3.19ms at 1M accounts vs 6.98s
full-scan, see A1.6). The **native** half is still the wall:

- `compute_native_state_root` (`crates/torus-bridge/src/state_root.rs:89`) is a **flat keccak over
  all entries of 6 native CFs**, recomputed **2×/block on the consensus thread** (`proposer.rs:235`,
  `validator.rs:414`). Cost is **O(total native state)**.
- A flat keccak is **fundamentally non-incremental**: keccak streams the whole byte string, so a
  one-entry change forces a full re-hash. There is no way to update it in O(changed). The native
  root *must* become a **keyed Merkle structure** (design doc, Open Q3).

As native exchange state (balances, order books, positions, oracle, staking) grows, the per-block
native-root tax inflates block time regardless of how many orders are in the block — breaking the
sub-100ms guarantee at production scale.

## Decision (structure): Bucketed Merkle tree

**User-confirmed 2026-06-07.** A **bucketed Merkle tree** (a Merkle-of-sorted-leaves partitioned
into a fixed bucket set so updates stay local), keyed by `(cf, key)`.

Rejected alternatives:
- **Sparse Merkle Tree (SMT):** gives succinct per-key proofs and is strictly state-size-independent,
  but ~256-deep paths → high RocksDB write-amplification (~256 nodes/changed key) and more new code
  (higher determinism risk). **Hyperliquid — the perf reference — ships *no* native state proofs**
  (deterministic replay + 10k-block ABCI snapshots + validator-direct read precompiles; no per-block
  Merkle state proofs for HyperCore). So proofs aren't needed for parity; the SMT's main advantage is
  unused.
- **Plain Merkle-of-sorted-leaves:** simplest, but *not* incremental — an insert/delete shifts every
  later leaf position → O(total) recompute. Bucketing is exactly what fixes that.

Why bucketed wins here: it **reuses the proven flat-keccak framing** (each bucket hash is the
existing length-framed keccak, just scoped to a bucket) → lowest determinism risk; **low
write-amplification** (only changed buckets + their ~16-node tree paths); simple, fixed-shape tree;
and it already gives a **per-block verifiable native root** — more than Hyperliquid exposes.

### Block-speed / throughput (estimates, anchored to the measured EVM A1.6 figures)

| native entries | flat-keccak (today, ×2/block) | bucketed (×2/block) |
|---|---|---|
| 10k  | ~2ms | <0.2ms |
| 100k | ~20ms | <1ms |
| 1M   | **~200ms** (breaks 100ms) | ~1–6ms (flat) |
| 10M  | **~2s** | ~10–30ms (flat) |

The native-root cost is a per-block tax that depends on **native state size, not block size**, so
flat-keccak makes orders/sec *decay over the chain's life*; bucketed holds it flat. Bucketed vs SMT
are within single-digit ms — **not** a throughput differentiator.

## Structure spec (the consensus-critical definition)

Authoritative native CFs (the 6 the consensus root covers), each with a **stable 1-byte `cf_tag`**:

| cf_tag | CF |
|---|---|
| 0 | `CF_NATIVE_BALANCES` |
| 1 | `CF_NATIVE_ORDER_BOOKS` |
| 2 | `CF_NATIVE_POSITIONS` |
| 3 | `CF_NATIVE_ORACLE` |
| 4 | `CF_STAKING_DELEGATIONS` |
| 5 | `CF_STAKING_VALIDATORS` |

- **Bucket assignment:** `bucket_id(cf_tag, key) = u16::from_be_bytes(keccak256(cf_tag ‖ key)[0..2])`
  → `0..65536` (16-bit prefix; uniform regardless of key layout, so no hot buckets from clustered keys).
- **Leaf entry framing** (reuses `compute_native_state_root`'s framing, + `cf_tag`):
  `cf_tag(1) ‖ (key.len() as u32 LE) ‖ key ‖ (value.len() as u32 LE) ‖ value`.
- **Bucket hash:** concatenate the framing of every entry in the bucket, **sorted by `(cf_tag, key)`**,
  then `keccak256(...)`. Empty bucket → `EMPTY_LEAF = keccak256(b"torus.native.bucket.empty")`
  (domain-separated sentinel).
- **Tree:** a complete binary tree of **65536 leaves (depth 16)**. `node[16][i] = bucket_hash(i)`;
  `node[L][i] = keccak256(node[L+1][2i] ‖ node[L+1][2i+1])`; **root = `node[0][0]`**.
- **Default nodes:** `default[16] = EMPTY_LEAF`, `default[L] = keccak256(default[L+1] ‖ default[L+1])`.
  Any all-empty subtree = `default[L]`; only **non-default** nodes are persisted.

### Persistence: `CF_NATIVE_TRIE` (new) + `CF_NATIVE_HASHED` (new mirror)

- `CF_NATIVE_HASHED` — **bucket-ordered mirror** (direct analog of EVM `CF_HASHED_*`): key
  `bucket_id(2 BE) ‖ cf_tag(1) ‖ native_key` → `native_value`. A prefix-scan on a 2-byte `bucket_id`
  yields exactly that bucket's members, already in `(cf_tag, key)` order. This is what makes a
  changed bucket re-hashable in **O(bucket_size)** instead of O(total). (~2× native-state storage,
  same trade the EVM half already makes.)
- `CF_NATIVE_TRIE` — the tree: bucket-hash leaves keyed `0x00 ‖ bucket_id(2 BE)`; internal nodes keyed
  `0x01 ‖ level(1) ‖ index(2 BE)`; persisted root marker keyed `0x02`. Only **non-default** nodes
  stored; nodes that revert to default are **deleted** (see determinism rule 4).

## EVM-symmetric API (`crates/torus-state/src/native_trie.rs`, new)

Mirrors `incremental.rs` exactly:

- `native_root_full(db) -> B256` — pure **O(total)** recompute from the 6 CFs (group entries by
  bucket in memory, hash, build tree). Touches no persisted trie. The **determinism oracle** and the
  **flag-off path**. [analog of `full_post_bundle_evm_root`]
- `build_native_trie_to_cf(db) -> B256` — one-time migration: populate `CF_NATIVE_HASHED` + the tree,
  return root (== `native_root_full`). Idempotent. [analog of `build_trie_to_cf`]
- `ensure_native_trie_built(db) -> bool` — boot helper (build once if mirror empty).
- `commit_native_trie_incremental(db, dirty: &[(cf_tag,key)], new_values) -> (B256, batch ops)` — at
  flush: for each changed `(cf,key)` update the mirror, rehash the affected buckets from the mirror,
  update the ≤16-node tree path per changed bucket, **delete-to-default** as needed; persist mirror +
  tree + root **in the same atomic `WriteBatch` as the native-CF writes**. [analog of
  `incremental_evm_root` + `commit_evm_bundle_incremental`]

## Tasks (TDD — failing test first; run `validate`; paste output; commit per task)

### A2.1 — Structure + `CF_NATIVE_TRIE`/`CF_NATIVE_HASHED` + full-scan build
- Add both CFs to `cf.rs` + `ALL_CF_NAMES`.
- Implement `native_trie.rs`: framing, bucket/tree/default-node logic, `native_root_full`,
  `build_native_trie_to_cf`, `ensure_native_trie_built`.
- **Test first:** `native_trie_root_matches_full_scan` — on a seeded corpus across all 6 CFs,
  `build_native_trie_to_cf(db) == native_root_full(db)`; idempotent (re-run == same root + node set);
  empty state == `EMPTY_ROOT_HASH`; persisted nodes are non-empty.
- **validate:** `cargo test -p torus-state native_trie_root_matches_full_scan`

### A2.2 — Incremental from `NativeStateOverlay` dirty `(cf,key)` set
- `NativeStateOverlay::dirty_native_keys()` (analog of `dirty_evm_accounts`) → the `(cf,key)` writes
  + deletes hitting the 6 root CFs.
- `commit_native_trie_incremental` wired into `execute_committed_block` (app.rs, native block right by
  the existing `overlay.flush(...)` / `resync_evm_accounts(...)`), folding native-CF writes + mirror +
  trie into **one atomic batch**. Honors the **N−1 lag** (header native root = persisted N−1 root,
  advanced when N's overlay flushes).
- **Tests first:** `incremental_native_root_equals_full_scan` (single-step gate, varied bundles incl.
  insert/update/delete and bucket-emptying); `sequential_native_commits_match_full_scan` (the
  multi-block accumulation gate that caught the EVM bug the single-step test missed);
  `native_trie_survives_crash_reopen` (atomic batch → drop+reopen → root unchanged).
- **validate:** `cargo test -p torus-state incremental_native_root_equals_full_scan native_trie_survives_crash_reopen sequential_native_commits_match_full_scan`

### A2.3 — Flag-gated callsites + native oracle + reconcile 6-CF/5-CF
- `flagged_native_root(db)` in `state_root.rs` (mirror `flagged_evm_root`): flag-off →
  `native_root_full`; flag-on → persisted root + oracle cross-check (`incremental == full`, fail loud)
  under `cfg!(debug_assertions) || TORUS_INCREMENTAL_ORACLE`. Reuse `TORUS_INCREMENTAL_STATE_ROOT`.
- Swap `compute_native_state_root` callsites (`proposer.rs:235`, `validator.rs:414`) to
  `flagged_native_root`.
- **Reconcile:** delete/fix the divergent 5-CF `compute_native_state_root_from_db` (`trie.rs:90`,
  missing `CF_NATIVE_ORDER_BOOKS`) — check callers first; the 6-CF bucketed root is authoritative.
- **validate:** `cargo test -p torus-bridge -p torus-consensus` incl. `four_node_consensus`.

## Determinism rules (non-negotiable — this is consensus-critical)

1. **`cf_tag` mapping is frozen.** Never reorder/renumber. Set must equal the 6 authoritative CFs.
2. **Canonical member order:** within a bucket, `(cf_tag, key)`; the mirror key layout enforces it.
   Full-scan and incremental must produce byte-identical bucket inputs.
3. **Fixed constants:** `EMPTY_LEAF` and `default[L]` are domain-separated and never change.
4. **Delete-to-default discipline** (analog of mem `6e50b060` — RocksDB delete-before-put; non-canonical
   node sets caused the EVM single-step-pass-but-later-diverge bug): when a bucket empties, delete its
   leaf node; when any node reverts to `default[L]`, **delete it** from `CF_NATIVE_TRIE`. A node set
   that is root-correct but non-canonical will diverge on a later block.
5. **Symmetric A1.5 lesson:** the incremental update is driven by the **generalized overlay dirty
   `(cf,key)` set**, not a hand-enumerated writer list. *Every* writer of the 6 root CFs must flow
   through `NativeStateOverlay` (so `dirty_native_keys` captures it). **Verify in A2.2/A2.3** that
   `NativeExecutor`, staking/epoch rotation, oracle updates, and fee distribution all write the 6 CFs
   via the overlay; if any bypasses it, route it through or add a resync. The runtime oracle
   (incremental==full, fail loud) + the LOADED devnet smoke are the safety nets — unit tests alone
   missed the A1.5 bug.

## Flag / rollout semantics

- One flag `TORUS_INCREMENTAL_STATE_ROOT` (both halves) + `TORUS_INCREMENTAL_ORACLE`.
- **Value-neutral flag:** flag-off (`native_root_full`) and flag-on (persisted incremental) compute
  the *same* bucketed root → flipping the flag needs no coordination (EVM model). The **binary upgrade**
  (flat-keccak → bucketed) is the consensus change; adopted on a **fresh local devnet** per guardrails.
- No wire/header/lag change: composite root stays `keccak(evm ‖ native)`, native N−1 lag unchanged.
- **Rollback:** `native_root_full` stays in-tree as the oracle; revert = flip callsites back. New CFs
  become dead but harmless.

## Open questions (resolve during implementation)
1. **Atomic batch shape (A2.2):** fold the native-trie/mirror updates into the overlay-flush batch vs a
   second batch immediately after. Target: one batch (crash-consistent). Confirm against the existing
   `flush` + `resync_evm_accounts` ordering in `execute_committed_block`.
2. **Writer coverage (A2.2/A2.3):** confirm all 6-CF writers go through the overlay (rule 5).
3. **5-CF helper fate (A2.3):** fix to 6 CFs vs delete — depends on its callers (sync/catchup?).
