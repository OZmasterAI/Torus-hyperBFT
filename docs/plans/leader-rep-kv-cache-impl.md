# Implementation Plan: Leader-Reputation In-Memory Cache

## Design Decision
From the s375 lever brainstorm (`mem 047fd86a`, `mem 588f3ee4`): the HotStuff
consensus hot path reads `leader_reputation()` ~9×/round and does
`record_leader_success` read-modify-write 2×/commit — every call a raw RocksDB
get/put + borsh (de)serialize, **uncached** (`internal.rs:1548/1567/1586`). Add a
**write-through in-memory cache** of the deserialized `LeaderReputation` inside
`BlockTreeSingleton` (`internal.rs:84`): reads return the cached copy (lazy-load on
first/cold), every write updates cache **and** KV. Transparent to the 9 call sites —
they keep calling `leader_reputation()`, now served from memory.

**Problem:** ~11 KV round-trips/block on consensus meta (9 reputation reads + 2
record read-writes), height-scaling RocksDB pressure; contributes to the ~139 ms
idle block floor at height ~332k and the s370 "~260 ms empty-block latency"
(`588f3ee4`). Confirmed unfixed at s375 (`047fd86a`).

**Why in-struct cache (not coalesce-at-round):** the 9 reads are spread across
distinct functions in `implementation.rs` (212, 287, 336, 613, 895, 1043, 1349,
1580 + `pacemaker:327`); threading a parameter through all of them is invasive. An
accessor-level write-through cache is transparent and localized to `internal.rs`.

**Why write-through (keep the KV writes):** persistence / crash-recovery must be
identical to today — only the redundant **reads** are eliminated. Cold cache
(restart) loads from KV = today's behavior.

## Sequencing
Secondary lever — execution-bound until trust-cache lands (`double-verify-trust-cache`).
Its value (idle floor + post-trust-cache consensus floor) compounds **after** the
exec re-verify is cut. Build trust-cache first; this second.

## Success Criteria
- `leader_reputation()` served from memory after first load; the 9 hot-path reads
  no longer hit KV.
- **Write-through**: every `set_leader_reputation` / `record_leader_success` /
  `record_leader_timeout` updates cache + KV identically.
- **Determinism (fork-safety)**: cached reputation is byte-identical to a fresh KV
  deserialize at all times; `select_leader_reputation_weighted` yields the same
  leader as today for a fixed (view, validator_set).
- **Crash/restart safe**: cold cache → first read loads from KV = current state.
- No consensus identity / leader-selection change → **no coordinated-relaunch fork**.
- `cargo test -p hotstuff_rs --lib` stays green (NB: the `block_sync` integration
  test hangs pre-existing — use `--lib`); new determinism test added.

## Tasks

### Task 1: Write-through in-memory cache field on BlockTreeSingleton
- **Test first** (`hotstuff_rs --lib`): `leader_rep_cache_write_through` — after
  `record_leader_success(&k)`, assert `leader_reputation()` reflects the bump AND a
  fresh borsh-deserialize of `variables::LEADER_REPUTATION` straight from the KV
  equals the cached value (the cache == KV invariant).
- **Implementation**:
  - `internal.rs:84` — `pub struct BlockTreeSingleton<K: KVStore>(K, core::cell::RefCell<Option<crate::hotstuff::types::LeaderReputation>>);` (keeps `self.0` = KV; adds `self.1` = cache). Ensure `LeaderReputation: Clone` (`types.rs:679`; add `#[derive(Clone)]` if missing).
  - Update every `BlockTreeSingleton(kv)` construction site → `BlockTreeSingleton(kv, core::cell::RefCell::new(None))` (a `cargo build` enumerates them).
  - `leader_reputation()` (`:1548`): if `self.1.borrow().is_some()` → return the clone; else run the existing KV get+deser (or `LeaderReputation::new(100)`), store into `self.1`, return.
  - `set_leader_reputation()` (`:1567`): after `self.write(wb)`, add `*self.1.borrow_mut() = Some(reputation.clone());`.
  - `record_leader_success`/`record_leader_timeout` (`:1586`/`:1597`) are unchanged — they call the now-cached read + write-through set.
- **Risk note**: `RefCell` makes `BlockTreeSingleton` `!Sync`. It is owned/borrowed by the single consensus algorithm thread, so this is fine; if a `Sync` bound breaks compilation, fall back to caching only on the write path (set/record) and leave reads lazy. `cargo build` is the check.
- **Verify**: `cargo test -p hotstuff_rs --lib leader_rep_cache_write_through`
- **Depends on**: —

### Task 2: Determinism + crash-recovery tests
- **Test first** (`hotstuff_rs --lib`): `leader_rep_cache_deterministic` —
  drive a fixed sequence of `record_leader_success` / `record_leader_timeout`;
  assert (a) final scores == scores from a **fresh** `BlockTreeSingleton` over the
  same KV (cold cache, simulates restart); (b) `select_leader_reputation_weighted`
  (`pacemaker:885`) returns the same leader for a fixed (view, validator_set) with
  warm vs cold cache; (c) `decay()` fires at the same `window_size` boundary either way.
- **Implementation**: integration test mirroring the existing block_tree tests;
  recreate the accessor from the same KV handle to get a cold cache and compare.
- **Verify**: `cargo test -p hotstuff_rs --lib leader_rep_cache_deterministic`
- **Depends on**: 1

### Task 3 (optional): Coalesce record writes + speculative_commits read-cache + metrics
- **Test first**: `record_success_single_write` — the commit path that calls
  `record_leader_success` at `:344` and `:375` issues ≤1 KV put when coalesced;
  `speculative_commits_cached` returns the cached (small, pruned) Vec without a KV get.
- **Implementation**: (a) coalesce the 2 `record_leader_success` writes/commit into
  one write at the end of `update()`; (b) mirror the write-through cache for
  `speculative_commits` (`:1383`; small/pruned via `promote_speculative_to_irrevocable:1418`,
  so secondary); (c) `leader_rep_cache_{hits,misses}` counters in `torus-telemetry`.
- **Verify**: `cargo test -p hotstuff_rs --lib && cargo build`
- **Depends on**: 1

## Verification (end-to-end)
`four_node_consensus` (or devnet 3-box): leader selection + finality unchanged,
idle block time drops at height, `cargo test -p hotstuff_rs --lib` green.

## Rollback
Revert the struct field + read/write-through edits. Cache is additive; write-through
keeps KV authoritative; cold-start loads from KV. No state / format / identity
change → no fork risk. Optionally gate behind a `--leader-rep-cache` flag
(zstd-style, default on) if A/B measurement is wanted.

## Note (corrects an earlier concern)
`speculative_commits` is **not** an unbounded blob — `promote_speculative_to_irrevocable`
(`:1418`) prunes finalized blocks, so it stays small (in-flight 3-chain depth). The
primary win is the 9×/round `leader_reputation` reads, not `speculative_commits`.
