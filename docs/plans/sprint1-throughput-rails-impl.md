# Implementation Plan: Sprint 1 — Throughput Rails

**Design:** docs/plans/sprint1-throughput-rails.md (Option B everywhere)
**Branch:** `fix/native-da-push-hardening` · **Session:** 334

## Success Criteria
- A proposer can never build a block whose native bodies exceed `NATIVE_BLOCK_BYTES_CAP`
  (bs1000-class load degrades to more, smaller blocks — no wedge).
- Nonce-expired actions are evicted from the pool lazily; a stuffed mempool self-heals
  within `NONCE_WINDOW_MS` without restarts.
- A burst of >16 concurrent submits queues briefly instead of instantly erroring
  "overloaded"; sustained overload still sheds.
- Sync-path body pulls scale their budget with the number of missing bodies (1s→8s);
  hot-path budgets unchanged.
- `cargo test --workspace` green; live re-sweep: bs1000 no longer wedges, bs500 ≥ 28k o/s.

## Tasks

### Task 1: Byte-capped native selection
- **Test first** (`crates/torus-mempool/src/native_pool.rs` tests): insert 10 non-cancel
  actions of known encoded size; call `select_for_block_with_senders_excluding(100, &empty,
  bytes_cap)` with `bytes_cap` = 3.5 × per-action size → assert exactly 3 selected.
  Assert `usize::MAX` cap preserves old behavior (all 10).
- **Implementation:**
  - `rate_limit.rs`: add `pub const NATIVE_BLOCK_BYTES_CAP: usize = 2_000_000;` (doc
    comment: WAN dissemination budget — what pre-warm push + pull moves in ~1-2 views).
  - `native_pool.rs:14` `NativePoolEntry`: add `encoded_len: usize`; set in `insert()`
    via `bincode::serialized_size(&action)` (add `bincode` workspace dep to torus-mempool
    if absent — block bodies are bincode, app.rs:540).
  - `select_for_block_with_senders_excluding(limit, exclude)` → add `bytes_cap: usize`
    param; running `bytes_used` sum, skip-break when `bytes_used + entry.encoded_len >
    bytes_cap` (break, not continue — keep deterministic prefix semantics).
  - Thread through wrappers: `select_for_block_with_senders` (pass `usize::MAX`),
    `Mempool::select_native_for_block_with_senders_excluding` (lib.rs:429) new param;
    caller `app.rs:1201` passes `torus_mempool::rate_limit::NATIVE_BLOCK_BYTES_CAP`.
- **Verify:** `cargo test -p torus-mempool`
- **Depends on:** —

### Task 2: Lazy TTL eviction from nonce window
- **Test first** (`native_pool.rs` tests): insert entry with `nonce = now - 2×window`
  (bypass admission by calling `NativePool::insert` directly) + one fresh; call
  `evict_expired(now_ms)` → size 1, stale gone from `seen`/`hash_index`/`sender_counts`
  (re-insert of stale hash now succeeds = seen cleaned). Selection after eviction
  returns only fresh.
- **Implementation:**
  - `native_pool.rs`: `pub fn evict_expired(&mut self, now_ms: u64) -> usize` — retain
    `entry.action.nonce.saturating_add(NONCE_WINDOW_MS) >= now_ms`; rebuild
    `sender_counts`/`seen`/`hash_index` for removed (mirror `remove_committed` :243).
    Import `torus_types::eip712::NONCE_WINDOW_MS`.
  - `lib.rs` `Mempool`: call `pool.evict_expired(current_time_ms)` at the top of
    `select_native_for_block*` and `drain_native` (compute now in the wrapper, keep
    `NativePool` clock-free). Log at `info!` when evicted > 0.
- **Verify:** `cargo test -p torus-mempool`
- **Depends on:** T1 (same file churn; land after)

### Task 3: Submit semaphore 64 + bounded queue
- **Test first** (`crates/torus-rpc/src/lib.rs` tests, tokio): helper
  `acquire_submit_permit(&sem)` — with 64 permits free returns Ok fast; with all held
  longer than the timeout returns Err within ~2× timeout.
- **Implementation:**
  - `lib.rs:175`: `Semaphore::new(SUBMIT_PERMITS)`; consts `SUBMIT_PERMITS: usize = 64`,
    `SUBMIT_QUEUE_TIMEOUT: Duration = Duration::from_millis(250)`.
  - `torus.rs:597`: replace `try_acquire()` reject with
    `tokio::time::timeout(SUBMIT_QUEUE_TIMEOUT, self.submit_semaphore.acquire()).await`
    → timeout ⇒ existing "overloaded" error path.
- **Verify:** `cargo test -p torus-rpc`
- **Depends on:** —

### Task 4: Size-aware sync pull budget
- **Test first** (`crates/torus-consensus/src/app.rs` tests): pure fn
  `sync_pull_retries(missing: usize) -> usize`: `0→20`, `40→20` (floor),
  `100→50`, `1000→160` (cap).
- **Implementation:** `app.rs:553` area — keep `PULL_RETRIES=20` as floor const, add
  `MAX_SYNC_PULL_RETRIES: usize = 160`; `fn sync_pull_retries(missing) =
  missing/2 clamped [PULL_RETRIES, MAX_SYNC_PULL_RETRIES]`; use in
  `pull_missing_bodies` sync-path loop (retry count only; `PULL_DELAY` unchanged).
  Hot-path consts (`RECONSTRUCT_*`, `HOT_PULL_*`) untouched.
- **Verify:** `cargo test -p torus-consensus`
- **Depends on:** —

### Task 5: Prove — workspace, deploy, re-sweep
- `cargo test --workspace` green; `cargo build --release`.
- Deploy: restart our node on new binary; rsync binary to friend1
  (`/root/torus-hyperbft/target/release/torus-node`) + `systemctl restart` (same glibc
  verified s334 via bench-throughput). friend2 stays old — all changes validator-local.
- Re-sweep from smallserver: `run-sweep.sh 500 1000`. Pass bar: bs1000 NO wedge (chain
  advances within 30s of load end without restarts), bs500 ≥ 28k o/s, byte-capped blocks
  visible in logs (`selected actions for block` with bounded counts).

## Verification (end-to-end)
Sweep results + post-load health from run-sweep.sh; memory record of numbers.

## Rollback
All four changes are validator-local consts/logic: revert commit, rebuild, restart the
two nodes. Chain state unaffected.
