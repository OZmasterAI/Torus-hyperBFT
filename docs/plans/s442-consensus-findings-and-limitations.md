# S442 Consensus Findings & Known Limitations (merge readout)

Scope: what the T1.2 fork hunt actually found, the HotStuff hardening we are keeping,
and the limitations we are knowingly merging with. Written for the engineers reviewing
the merge PR — file:line references are to the tree at merge time.

## 1. The T1.2 finding: the "forks" were an app-layer body-determinism bug, not a consensus bug

The devnet forks were NOT a HotStuff safety failure. HotStuff itself was proven safe by
vote-level instrumentation: every validator voted for and committed **identical** blocks at
each height. The divergence was entirely in the app's block-body handling on the execution
side.

Root cause: on a same-height re-proposal, the leader's `pending_proposals` cache overrode the
committed datum. At execution time the leader executed its **LAST** cached proposal, not the
**COMMITTED** one — so it applied a divergent native-action subset and persisted an eth header
whose `native_action_count` no longer matched the body it executed. Reproduced on devnet
(t12-diag1: a node executed 40 native actions out of a committed 59).

Fix (in `on_committed_block`, `crates/torus-consensus/src/app.rs:2301`):
- **Header taken verbatim from the committed datum.** The committed header is the consensus
  datum; it is never rebuilt from a local cache or from execution results (`app.rs:2335-2357`).
- **Hash-verified cache fast-path.** A cached proposal may supply bodies for a committed compact
  ONLY when its native actions hash-for-hash (order + value) to the committed reference
  (`cached_matches_compact`, `app.rs:214`); any drift falls back to all-or-nothing DA
  reconstruction. A full datum drops any stale same-height cache entry outright (`app.rs:2333`).
- **Fail-stop count guard.** `execute_committed_block` (`app.rs:441-451`) refuses to execute a
  body whose length differs from the committed header's `native_action_count`, latching
  `exec_failed` — a hard latch making the divergence unreachable even if a partial body slips
  through.

Result: full f=1 run (n=4) — **0 forks / 0 breaks over 1358 heights.**

## 2. HotStuff hardening kept from the hunt (correct hygiene, independent of the bug)

These are correctness improvements found while hunting; they stand on their own and stay in.

- **Header fast-path lock-before-vote** (`crates/hotstuff_rs/src/hotstuff/implementation.rs:1784`).
  The header fast-path is the sole vote path in production. It previously voted WITHOUT locking,
  deferring the lock to body arrival; in that window a replica could legally vote for conflicting
  siblings across views, voiding quorum intersection. It now calls `update_locks_only(&header.justify)`
  (advancing the lock via `pc_to_lock`, lock-on-parent) BEFORE voting, unconditionally, mirroring
  the full-block path. Commit stays deferred to the idempotent body-arrival update.
- **View-monotonic `pc_to_lock`** (`crates/hotstuff_rs/src/block_tree/invariants.rs:421`,
  tests in `hotstuff/iter2_lock_safety_test.rs`). `pc_to_lock` had no view-monotonicity guard, so a
  lower-view (or equal-view sibling) Generic QC could regress `locked_pc`. The guard sits at the
  single choke point so both the full-block and header paths inherit it.
- **Pending-QC lock-clause enforcement** (`iter2_lock_safety_test.rs`:
  `pending_qc_violating_lock_clause_is_not_applied`). A pending QC that violates the lock clause must
  NOT advance/regress `highest_pc` / `locked_pc`; a lock-satisfying pending QC still applies (liveness
  preserved).
- **Commit-conflict fail-stops.** `detect_conflicting_commit` (`app.rs:256`, called from
  `execute_committed_block` at `app.rs:461`): a DIFFERENT block hash arriving at an already-applied
  height is a durable agreement violation → latch `exec_failed`. At the sync layer,
  `conflicts_with_committed` → `ConflictingCommittedChain` (`hotstuff_rs/src/block_sync/client.rs:302`)
  refuses the peer's divergent committed chain and calls `app.on_fatal_safety_violation()`
  (`app.rs:2509`), which latches `exec_failed`; the node binary watches that latch and turns it into
  `exit(70)` — so the node stops voting/finalizing over a forked chain rather than merely ending the
  sync session.
- **Lock-on-parent, not grandparent** (commit `4a94e8b`): grandparent-lock is unsafe for a 2-chain
  commit; locking on the parent is the safe rule wired into `pc_to_lock`.

**The dropped contiguity guard, and why.** A *sync-layer* contiguity precondition — refuse to apply a
synced block whose parent (`justify.block`) is neither genesis nor already in the tree — was prototyped
(`sync_block_is_contiguous` / the `NonContiguousSync` error) and DROPPED. It never fired in any run: the
`block_at_height` holes we observed do not come from sync insertion at all — they come from the
deferred-body and restart-window paths (§3 (a)/(b)), where consensus commits past a height whose body is
not yet reconstructable. Worse, the guard actively conflicts with the gap-driven backfill driver, which
deliberately re-fetches heights BELOW the committed frontier to fill exactly those holes; a
"parent-must-already-be-present" gate would reject the backfilled blocks it is trying to land. Gaps are
instead handled by three cooperating mechanisms: fail-loud `replay_gap` on restart (`app.rs:360`), the
strict-order execution queue (which refuses to execute out of height order and surfaces the gap rather
than silently skipping), and the gap-driven backfill trigger in `client.rs` (lowest-hole-first re-fetch).
The sync layer still hard-fails on a genuine *conflict* (`ConflictingCommittedChain`, above) — that guard
is unrelated and stays.

## 3. Known limitations going into the merge

(a) **Deferred-body silent holes.** Consensus can commit past a block whose body is not
reconstructable at exec time (`on_committed_block` logs "execution deferred", advances height, and
returns — `app.rs:2377-2399`). Observed on HEALTHY nodes in the full run (v2: heights 987-989; v3:
911-913). **ADDRESSED (pending devnet re-validation):** the strict-order execution queue now refuses to
skip a missing height (surfacing the hole instead of silently advancing execution), and the gap-driven
backfill driver (`client.rs`, lowest-hole-first re-fetch) re-fetches the deferred body and fills the
hole. Confirmation is a green devnet re-run.

(b) **Restart-window holes.** A killed + restarted validator does not backfill the window it was
down for (96 heights observed). **ADDRESSED (pending devnet re-validation):** same strict-order
execution queue + gap-driven backfill driver as (a) — the restart replay now detects the gap
(`replay_gap`) and the backfill trigger re-fetches the missing window rather than leaving it permanently
absent. Confirmation is a green devnet re-run.

(c) **Slash-atomicity residual.** Invalid-attestation slashing writes directly (unbatched), outside
the atomic native flush, so a narrow crash window remains where a slash could be lost or partially
applied. P2, deferred to post-merge.

(d) **Header identity.** The `parent_hash` fix is pending application. Until it lands, hash equality
does NOT imply ancestry — two headers can hash-equal without being on the same chain. The
conflicting-commit guards compare hashes at a height, which is sufficient for same-height conflict
detection but not for ancestry reasoning.

(e) **`state_root` excluded from the header preimage.** It was a perpetual `0x0` in the canonical
header bytes, so it is currently excluded from the hashed preimage. Re-adding it under
parent-state-root semantics (commit to the PARENT's post-execution root, matching the lagged-root
commit-then-execute model) is future work.

(f) **Fault-window throughput.** With 1-of-4 validators dead, throughput drops >4x. This is a
liveness characteristic of the leader-slot timeout cadence (views waiting out the dead leader's slot),
not a correctness issue — it matches the live val3 symptom.

(g) **1-in-4 SMOKE fault-window stall flake.** The SMOKE harness occasionally stalls in the
fault-window scenario due to view-change timing. Intermittent; a test-timing flake, not an observed
safety violation.

## 4. Evidence pointers

- Fork-detection harness: `devnet/t12-fork-detect-4val.sh`
- Crash-injection harness: `devnet/t15-crash-inject-4val.sh`
- Session S442 memories (vote-level instrumentation results, t12-diag1 divergence, full-run
  0-fork/1358-height readout, deferred-body observed heights).

## 5. Related plan docs (updated for this tree)

- `docs/plans/crash-safety-post-commit-impl.md` — atomic marker fold + gap-loop replay (S442 note).
- `docs/plans/commit-then-execute-impl.md` — native flush now single atomic `WriteBatch` (S442 note).
