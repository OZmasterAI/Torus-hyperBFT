# Post-Erasure Backlog (queued S466)

Two consensus-hardening tasks surfaced by val2's independent forensic and
**verified against `think-dev` HEAD (9d56144)** this session. **Queued to tackle
AFTER the erasure-recovery-wiring Phase A (T1–T10) lands.** Both are separate
from the erasure work (which is dissemination-scale, not liveness/durability).

Prereq for both: follow THE LOOP (memory → brainstorm → writing-plans → RED
tests → implement → prove → review → commit). Each is consensus-adjacent —
gets a written safety argument, not just a passing test.

---

## Task A — Adaptive (multiplicative) pacemaker timeout backoff  [CONSENSUS-LIVENESS]

**Problem (VERIFIED).** The pacemaker gives every view a FLAT `max_view_time`
with no backoff on consecutive timeouts; epoch-extend is additive, not
multiplicative:
- `crates/hotstuff_rs/src/pacemaker/implementation.rs:664-668` (initial),
  `:704-709` (update) — deadline = `start + max_view_time * (view - start + 1)`,
  a constant per-view increment.
- `:723-724` (extend_epoch_change_view_timeout) — adds exactly one more flat
  `max_view_time`.
- `crates/hotstuff_rs/src/pacemaker/mod.rs:98-124` — post-GST progress rests on
  a doc-level "park in the same epoch-change view until quorum reconverges"
  argument, enforced by NO code.

Corroborated by prior memory: "view timeout is NOT exponential backoff"
(e2529a8ef114b884) and "S395 LIVE FREEZE ROOT CAUSE = pacemaker epoch schedule
= ABSOLUTE deadline" (a0a6940c337429bc).

**Why.** With flat timeouts, honest nodes can re-time-out in lock-step without
the overlap window ever growing past message delay — the classic flat-timeout
liveness wedge. val2's thesis (worth taking seriously): the recurring stall-scars
**S395 (5.5h park @ view 642500), S426, S432** are case-by-case patches of THIS
one missing mechanism. A single adaptive-backoff change likely retires the class.
Directly serves the standing "don't let liveness/blockspeed degrade under load"
goal.

**Plan (sketch).** Multiplicative backoff on *consecutive* timeouts:
`view_time = base_max_view_time * factor^min(consecutive_timeouts, cap)` (e.g.
factor 1.5–2, capped), reset to base on a successful view/commit. So after GST,
view duration provably exceeds net message delay within a bounded number of
timeouts. Keep the base equal to today's `max_view_time` (no change to the happy
path). LOCKSTEP-sensitive: all validators must share the schedule (it's a
timing policy, not a wire format, but divergent schedules waste views) — ship
fleet-wide.

**Acceptance.** RED-first: a model/sim (fold into the T1.2 Stateright bounded-
progress model) where flat timeouts fail to converge post-partition-heal and
backoff converges. Devnet: induce a lock-step timeout (kill+restore quorum) and
show views reconverge without a multi-view park. Happy-path block cadence
unchanged (base == old max_view_time).

---

## Task B — `set_sync` on commit-frontier + body-manifest writes  [DURABILITY]

**Problem (VERIFIED).** Durability today is kill-durable only, not crash-durable.
Every consensus/DA write uses default (async) `WriteOptions` — WAL lands in the
OS page cache, survives process SIGKILL / `docker compose kill`, but NOT host
power-loss / hard VM-stop in the fsync window:
- `crates/torus-consensus/src/kv_store.rs:100-113` — `db.write(batch)`, default
  `WriteOptions`, no `set_sync`.
- `crates/torus-state/src/native_da.rs:85-97` — same.
- Repo-wide grep for `set_sync|WriteOptions|fsync|sync_wal|flush_wal` → zero
  write-path hits in `crates/`.

The 9d56144 body-durability invariant's proof only covers process-kill. Under a
*correlated* host crash (power-loss on ≥f+1 holders in the window), the recovery
guarantee (f+1 durable copies) can still lose the body — a narrow reopening of
the wedge.

**Plan (sketch).** `set_sync(true)` (or a periodic explicit WAL sync) on ONLY the
two safety-critical writes — the commit-frontier update and the body/commit
manifest — NOT the hot per-action path (leave ingress async; the ingress-CPU
lessons say don't fsync the hot path). Measure the commit-path latency delta;
if material, batch-then-sync or use a bounded WAL-flush cadence instead of
per-write sync.

**Acceptance.** RED-first test proving the two writes are fsync'd (inject a crash
between write and fsync in a harness; value survives with `set_sync`, is lost
without). Commit cadence A/B shows no material regression, or documents the
accepted cost. Gate: only enable if the deployment threat model includes
hard-power-loss (document the decision).

---

## Context / provenance
- Verified this session (S466) via read-only forensic agents against 9d56144;
  full quoted code + verdicts in memory (search "val2 forensic verified S466",
  "pacemaker flat timeout", "set_sync commit manifest").
- Related but SEPARATE and already-owned: T1.2 lock-depth is CLOSED in code
  (`invariants.rs:479-492`, lock-on-parent, "T1.2 SAFETY FIX") — only the
  Stateright model is unrun; recovery-from-permanent-loss is closed by 9d56144's
  f+1-durable-copy guarantee once deployed fleet-wide (live is still 9f3d1e3 →
  URGENT fleet upgrade, tracked separately).
