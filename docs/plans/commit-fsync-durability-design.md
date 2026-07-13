# Design: crash-durable commit-frontier + body writes (Task B)

> Status: BRAINSTORM. Consensus/durability-adjacent → ships with a written
> threat-model + durability argument, not just a passing test.

## Problem (as queued)
`post-erasure-backlog-s466.md` Task B: durability is **kill-durable only**, not
**crash-durable**. All consensus/DA writes use default (async) `WriteOptions` — WAL
lands in the OS page cache, survives SIGKILL / `docker kill`, but NOT host power-loss
/ hard VM-stop in the fsync window. Under a *correlated* host crash on ≥f+1 body
holders, the f+1-durable-copy recovery guarantee (9d56144) can still lose the body.

## Context (verified in `think-dev` this session)
- **One** consensus-meta write path: `RocksKVStore::write` at
  `crates/torus-consensus/src/kv_store.rs:112` — `self.db.write(batch)`, default
  `WriteOptions`. The commit-frontier update is **batched here with all other
  `CF_CONSENSUS_META` writes** → not separately targetable without an API change.
- DA body writes: `crates/torus-state/src/native_da.rs:99` and `:209`, default
  `WriteOptions`. (Task 0: classify which is the body-mirror / safety-critical one.)
- Repo-wide `set_sync|WriteOptions::|write_opt` → **zero** hits. All async today.

### Prior art (complementary, NOT duplicate — both still `pending`)
- `crash-safety-post-commit` (`PRPs/…`, `docs/plans/crash-safety-post-commit-impl.md`):
  recovers **native post-commit application** via a replay flag
  (`META_NATIVE_APPLIED_HEIGHT`; replay if `committed_height > applied_height`).
  Logical recovery of *derived* state.
- Task B is **physical** durability of the *committed* frontier+body. Replay can only
  recover what physically survives → Task B is the layer *beneath* the replay plan.
  Decision point: do both, in order (B first), or document the threat model and defer.

## Threat-model gate (decide BEFORE building — like Task A's premise check)
`set_sync` only matters for **host power-loss / hard VM-stop** in the fsync window.
Process-kill / `docker kill` is **already** covered (kill-durable). Live nodes are
managed VPS/systemd units. So: is hard-power-loss in the deployment threat model?
- If **no** → Task B is optional; document the accepted risk and stop.
- If **yes** (mainnet / real-money posture) → proceed; measure the commit-path cost.

## Options

### Option A — Blunt: fsync the whole consensus-meta write + body writes
Set `WriteOptions::set_sync(true)` on `kv_store.rs:112` and the body write(s).
- **+** smallest code; closes the window fully. **−** fsyncs *every* consensus-meta
  write, not just the frontier (the plan warns "don't fsync the hot path"); largest
  commit-latency cost. **Effort:** S · **Risk:** Med (latency).

### Option B — Targeted: `write_sync` variant on the KVStore trait
Add a synced write method; call it ONLY from the commit-frontier update and the
body-manifest write; leave every other write async.
- **+** syncs exactly the 2 safety-critical writes the doc intends; minimal latency.
  **−** trait-surface change; must trace the frontier-update call path down to
  `kv_store.write`. **Effort:** M · **Risk:** Med.

### Option C — Bounded WAL-flush cadence (`flush_wal(true)` / periodic sync)
Leave per-write async; explicitly `flush_wal(sync=true)` once per commit (or every
N ms).
- **+** amortizes fsync; decouples from the hot path. **−** bounded loss window (up
  to the cadence); weaker than per-write sync. **Effort:** M · **Risk:** Med.

## DECISION (user, this session)
**Option C chosen** — per-commit `flush_wal(sync)`. Rationale: `kv_store` and
`native_da` share ONE `Arc<DB>` / WAL (verified: `main.rs:453/505` build
`RocksKVStore::new(state_db.db_arc())`, `native_da.rs:48-51` shares the same
`StateDb`), so a single `flush_wal(true)` at the commit boundary makes the whole
committed prefix (frontier + header + body) durable atomically-by-WAL-order — full
coverage, 1 fsync, and no frontier-durable-but-body-not wedge hazard that Option B's
two separate synced writes would place on the implementer. Impl plan:
`commit-fsync-durability-impl.md`.

## Recommendation
**Resolve the threat-model gate first.** If crash-durability is in scope:
**Option B**, with a commit-latency A/B before defaulting it on (fall back to C's
per-commit `flush_wal` if B's per-write sync is too costly). Pair with
`crash-safety-post-commit` as the replay layer above it. Behind a config/threat-model
toggle either way.

## Open Questions
1. **(Blocking)** Is hard-power-loss in the deployment threat model? (decides go/no-go)
2. Which of `native_da.rs:99` / `:209` is the body-mirror safety-critical write?
3. Per-write `set_sync` (B) vs per-commit `flush_wal` (C) — measure the commit-path
   delta at ~65ms/block, 15 blk/s.
4. Sequence with `crash-safety-post-commit` (replay layer) — both, and in what order?
