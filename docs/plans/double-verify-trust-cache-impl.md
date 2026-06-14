# Implementation Plan: Double-Verify Trust-Cache

## Design Decision
Option A from the trust-cache brainstorm (`mem a644ca0a` trace, `mem 36eef1e7` plan):
a `verified_locally`-gated, FIFO-bounded `verified_senders` cache living **inside
`Mempool`** (rides the existing `Arc<Mempool>` shared by rpc/gossip/consensus),
populated at the two local-recover sites **and** refresh-stashed by
`remove_committed_native` at prune time, and read by `batch_verify_native_actions`
via a lookup **closure** (mirrors the existing session-lookup closure). On hit →
reuse sender, skip secp256k1 recovery; on **miss → full recover + slashing path**.

**Problem:** the exec thread re-recovers signatures already verified at ingress
(`torus.rs:268`) or gossip-admit (`mempool/lib.rs:309`) — the `exec_verify_seconds`
phase, ~291 ms/blk (429 ms at s351). The redundancy is real because Sprint-3 gossip
pre-spread means a block's actions are already in this node's mempool by proposal
time. The exec re-verify can't simply be deleted: it backs the proposer-slashing
guarantee (`app.rs:303-320`).

**Why a cache (not 1b reuse-mempool):** `remove_committed_native` (`app.rs:1649`,
consensus thread) prunes committed actions from the pool **before** the block crosses
the 64-block channel to the exec thread (`app.rs:291`), so a live pool lookup at exec
misses. A dedicated cache with independent lifetime (refresh-stashed at prune) bridges
that ordering. See `mem a644ca0a`.

## ⚠️ GATE (do not start before this)
This is a **secondary** ceiling — exec is idle ~90%; the primary wall is ingress CPU
**starvation** (s351/s353), and the s353 reframe says most of the 291 ms is
starvation-inflation, not crypto. **Parked behind the B1 clean-box probe (Q4):** only
`/implement` this if a pinned/quiet-box probe shows verify still dominates. Building it
before B1 optimizes an idle phase. Ship behind `--exec-trust-cache` (default off) so it
can be A/B-measured.

## Success Criteria
- Cache **hit** → reuse sender, **no** recovery; **miss** → full recover + slash
  (`app.rs:303-320`) intact.
- Cross-node **determinism**: hit and miss yield identical resolved sender + state.
- Cold cache (restart / crash-replay `app.rs:808`) = all misses = today's behavior.
- A `verified_locally == false` (gossip-trusted-claim) action is **never**
  short-circuited at exec.
- Existing `torus-mempool` / `torus-consensus` / `torus-types` suites stay green.
- New metrics: `verified_sender_cache_{hits,misses,evictions}`.

## Tasks

### Task 1: Cache structure + provenance field
- **Test first** (`torus-mempool`): `verified_sender_cache_fifo_evicts` — insert past
  cap → oldest evicted, newest present; `verified_sender(hash)` read does **not** mutate
  ordering (read-lock only, no LRU recency bump).
- **Implementation**: add `verified_locally: bool` to `NativePoolEntry`
  (`native_pool.rs:14`); add `verified_senders: RwLock<FifoCache<B256, Address>>` to
  `Mempool` (`lib.rs:90`, init `lib.rs:125`); methods `cache_verified_sender(hash, sender)`
  / `verified_sender(hash) -> Option<Address>`; const `VERIFIED_SENDER_CACHE_CAP` in
  `rate_limit.rs` = `EXEC_QUEUE_DEPTH (64) * NATIVE_TOTAL_BLOCK_CAP (100) * margin`
  (hard floor 6400; ship ~16384).
- **Verify**: `cargo test -p torus-mempool --lib verified_sender`
- **Depends on**: —

### Task 2: Populate at the local-recover sites
- **Test first**: after `add_native_action_presigned` and `add_native_action_from_gossip`,
  `verified_sender(hash) == Some(sender)`; after `add_native_action_from_gossip_trusted`,
  **not** cached.
- **Implementation**: in `add_native_action_presigned` (`lib.rs:272`) and
  `add_native_action_from_gossip` (`lib.rs:309`, post-recover) → set
  `verified_locally = true` and `cache_verified_sender(action_hash, sender)`; in
  `add_native_action_from_gossip_trusted` (`lib.rs:341`) → `verified_locally = false`,
  do **not** cache.
- **Verify**: `cargo test -p torus-mempool --lib`
- **Depends on**: 1

### Task 3: Refresh-stash at prune
- **Test first**: insert verified action; churn cache past cap; `remove_committed_native([hash])`;
  `verified_sender(hash)` still `Some` (re-stashed to fresh end before removal).
- **Implementation**: in `remove_committed_native` (`lib.rs:516`), for each hash look up
  the still-present entry and, if `verified_locally`, `cache_verified_sender(hash, entry.sender)`
  **before** removing it — refreshes the entry to the fresh FIFO end so it survives the
  ≤64-block exec lag.
- **Verify**: `cargo test -p torus-mempool --lib remove_committed_stash`
- **Depends on**: 1, 2

### Task 4: Read path — closure into `batch_verify_native_actions`
- **Test first** (`torus-types`): closure returning `Some(sender)` → recovery **not**
  called (panic-on-recover probe), sender reused; `None` → recovers; invalid action →
  `None` (slashing input).
- **Implementation**: add `verified_lookup: impl Fn(&B256) -> Option<Address>` param to
  `batch_verify_native_actions` (`crates/torus-types/src/eip712.rs`); per action compute
  hash and short-circuit on hit. At `app.rs:291` pass
  `|h| self.mempool.as_ref().and_then(|m| m.verified_sender(h))` (mirrors the session
  closure at `app.rs:294`). No new `torus-types` → `torus-mempool` dependency.
- **Verify**: `cargo test -p torus-types --lib batch_verify && cargo test -p torus-consensus --lib`
- **Depends on**: 1 (2/3 for live hits)

### Task 5: Determinism + slashing + provenance integration tests
- **Test first** (`torus-consensus`): (a) block executed with vs without cache →
  identical `resolved_senders` + state; (b) invalid sig (uncached) → `invalid_count > 0`
  → slash path fires (mirror existing slashing assertion); (c) `verified_locally == false`
  action → exec re-verifies it (not short-circuited).
- **Implementation**: integration test mirroring `crash_recovery_tests` (`app.rs:1775`).
- **Verify**: `cargo test -p torus-consensus --lib trust_cache`
- **Depends on**: 4

### Task 6: Metrics + gate flag
- **Test first**: counter-registration test (mirror exec-phase metric tests) for
  `verified_sender_cache_{hits,misses,evictions}`; flag OFF → `batch_verify` always
  recovers (today's behavior).
- **Implementation**: counters in `torus-telemetry`; node flag `--exec-trust-cache`
  (default **off**, zstd-style) plumbed through `config.rs` + `main.rs`; increment
  hit/miss/evict in the cache methods.
- **Verify**: `cargo test -p torus-telemetry --lib` + `cargo build`
- **Depends on**: 1, 4

## Verification (end-to-end — post-B1 only)
Clean/pinned-box probe with `--exec-trust-cache` on vs off: confirm
`exec_verify_seconds` drops, cache hit-rate is high (gossip pre-spread), block cadence +
slashing behavior unchanged, dup-factor ≤ 1.05. Keep `four_node_consensus`, the bigbody
suites, and `torus-mempool`/`torus-consensus`/`torus-types` green.

## Rollback
`--exec-trust-cache` off (or revert) → `batch_verify` always recovers = current
behavior. The cache is additive; no state/format migration; cold-start safe. No
consensus identity / state-root change → no coordinated-relaunch fork risk.
