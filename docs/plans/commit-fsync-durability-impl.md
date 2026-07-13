# Implementation Plan: crash-durable commit via per-commit WAL flush (Task B)

## Design Decision
**Option C — per-commit `flush_wal(sync)`.** `kv_store` (consensus-meta / frontier)
and `native_da` (bodies) share ONE `Arc<DB>` and therefore one WAL. A single
`flush_wal(true)` at the commit boundary fsyncs the whole committed prefix
(frontier + header + body) atomically-by-WAL-order — full coverage, one fsync, no
ordering hazard. See `commit-fsync-durability-design.md`.

## What this closes (verbatim, from the code)
`persist_committed_block_durably` (`app.rs:322`) writes the committed block's
header+body to durable CFs *at commit time* (FIX1a) — but with default async
`WriteOptions`. Its own comment at the body write (`:337`) admits "crash-recovery
may hole here." Under host power-loss in the fsync window, the committed
header/body can be lost → boot finds a committed non-empty height with no body →
**wedge**. This plan makes that write crash-durable.

## Grounded anchors (verified in `think-dev` this session)
- Commit boundary + body/header writes: `persist_committed_block_durably` —
  `crates/torus-consensus/src/app.rs:322` (header first `:323-324`, body `:337`).
- Shared DB handle: `StateDb { db: Arc<DB> }` — `crates/torus-state/src/db.rs:28-29`;
  `db_arc()` already exposed (used at `main.rs:453/505`).
- Consensus-meta write path (frontier): `RocksKVStore::write` `kv_store.rs:112`
  (same `Arc<DB>` → same WAL).
- RocksDB API: `DB::flush_wal(&self, sync: bool) -> Result<(), rocksdb::Error>`.
- Zero existing `set_sync`/`flush_wal` in `crates/` (all async today).

## Threat-model note (carried from design)
Only matters under host power-loss / hard VM-stop; process-kill is already survived.
Ship behind a per-node toggle, default OFF until the latency A/B (Task 4), then flip
the default if the cost is immaterial. Proposer/replica-local, format-neutral, not
genesis-gated — safe to A/B on one node.

## Success Criteria
- A committed block's header+body are fsync'd before the commit handler returns
  (when the toggle is on); a crash-inject harness loses them without the flush,
  keeps them with it.
- Exactly **one** `flush_wal(true)` per committed block (not per write, not per action).
- Toggle OFF ⇒ behavior byte-identical to today (no flush call).
- Commit-latency A/B measured; no material regression, or documented + defaulted OFF.
- `cargo test --workspace` green; zero new clippy on touched files.

## Tasks

### Task 1: Add `StateDb::sync_wal()` + a per-node toggle
- **Test first**: `sync_wal_calls_flush` — construct a `StateDb`, write a batch,
  call `sync_wal()`, assert `Ok(())` (and, with a temp DB reopened, the record is
  present). Add `sync_wal_on_commit_default_off` asserting the toggle default is off.
- **Implementation**: on `StateDb` (`db.rs:28`) add
  `pub fn sync_wal(&self) -> Result<(), StateError> { self.db.flush_wal(true).map_err(..) }`.
  Add toggle `TORUS_SYNC_WAL_ON_COMMIT` read once (OnceLock, same pattern as
  `evm_block_gas_budget()`), default `false`.
- **Verify**: `cargo test -p torus-state sync_wal`
- **Depends on**: —

### Task 2: Call `sync_wal()` at the commit boundary, gated by the toggle
- **Test first**: `commit_triggers_single_wal_flush` — a test double / counter around
  the commit path asserts `sync_wal` is invoked exactly once per committed block when
  the toggle is on, and zero times when off.
- **Implementation**: at the END of `persist_committed_block_durably` (`app.rs:322`),
  after the header+body writes, call `state_db.sync_wal()` if the toggle is on.
  **Placement note**: it must be the last durability step at commit so the WAL flush
  covers the HotStuff frontier write too; if the frontier write happens *after*
  `persist_committed_block_durably` in the caller, move the flush to just after
  `on_committed_block` returns instead. The crash test (Task 3) pins correctness.
- **Verify**: `cargo test -p torus-consensus commit_triggers_single_wal_flush`
- **Depends on**: 1

### Task 3: Crash-durability RED test (kill-in-fsync-window)
- **Test first**: `commit_survives_simulated_power_loss` — harness: open a temp DB,
  drive a committed block through the commit path, then reopen the DB **without** a
  clean flush. Assert: with the toggle ON the header+body are present after reopen;
  a control with the toggle OFF + suppressed background flush shows the hole. (If a
  true power-loss can't be simulated in-process, assert the invariant behaviorally:
  `sync_wal` is on the path before the handler returns, and add an OS-level
  hard-stop repro to Task 5's acceptance.)
- **Implementation**: none beyond Tasks 1-2; this locks the guarantee.
- **Verify**: `cargo test -p torus-consensus commit_survives_simulated_power_loss`
- **Depends on**: 2

### Task 4: Commit-latency A/B + default decision
- **Test first (harness)**: same-machine A/B (per S444 rule) — toggle OFF vs ON on
  one node, measure empty-block cadence (~65ms baseline) and under-load block time.
  Accept iff the per-commit fsync delta is immaterial (single-digit ms) or documented.
- **Implementation**: if immaterial, flip the toggle default to ON; else leave OFF +
  document the accepted cost in the design doc.
- **Verify**: A/B table + decision recorded.
- **Depends on**: 3

### Task 5: Hard-stop acceptance + threat-model write-up + sequencing
- **Test first (ops)**: on devnet, hard-stop a node (`kill -9` covers process; for
  power-loss, a VM hard-reset or `echo b > /proc/sysrq-trigger` on a throwaway box)
  mid-load, restart, confirm no committed-height hole (no wedge) with the toggle ON.
- **Implementation**: write the durability/threat-model argument into the design doc;
  note that `crash-safety-post-commit` (replay of native application) is the
  complementary logical layer above this physical layer — recommend it next.
- **Verify**: devnet hard-stop clean restart + review sign-off.
- **Depends on**: 4

## Verification (end-to-end)
`cargo test --workspace` green (toggle OFF path unchanged) → crash-inject test proves
the guarantee → A/B shows immaterial commit-latency delta → devnet hard-stop restarts
clean with no hole. Toggle is per-node, format-neutral; enable fleet-wide by config,
no relaunch.

## Rollback
Toggle `TORUS_SYNC_WAL_ON_COMMIT=0` (default) ⇒ today's exact behavior — a runtime
kill switch, no redeploy. Full revert = drop the branch; no data-format or persisted-
state change (only WAL fsync timing).
