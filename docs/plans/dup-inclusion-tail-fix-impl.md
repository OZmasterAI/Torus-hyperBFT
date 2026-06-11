# Implementation Plan: Duplicate-Inclusion Tail Fix (MissingData exclusion gap)

## Design Decision
Option A from docs/plans/dup-inclusion-tail-fix.md — an in-flight hash ledger
on `TorusApp`, noted from the compact datum BEFORE body reconstruction (so
`MissingData` proposals are still excluded), unioned per height, evicted by
the existing h+10 window, cleared at commit.

## Success Criteria
- New unit tests prove: note/union per height, h+10 window eviction,
  commit clear, and that noted hashes flow into the produce-time exclusion
  set used by `select_native_for_block_with_senders_excluding`.
- Existing suite stays green: `cargo test -p torus-consensus --lib`
  (notably `duplicate_committed_native_action_executes_once`,
  `duplicate_committed_native_batch_consumes_nonce_once`,
  `exec_phase_histograms_observe_per_block`) and
  `cargo test -p torus-mempool --lib`.
- Ops follow-up (separate, needs user go): rebuild + bounce seed, rerun a
  bench range, `native-dup-factor.py` factor ≤ ~1.05 (was 1.08–1.27).

## Tasks

### Task 1: `InFlightHashLedger` + unit tests (RED → GREEN)
- **Test first** (app.rs test module):
  - `in_flight_ledger_notes_and_unions_per_height`: note(5,[a,b]); note(5,[c])
    → extend_into yields {a,b,c}; note is idempotent for repeats.
  - `in_flight_ledger_window_evicts_old_heights`: note(1,[a]); note(12,[b])
    → only b remains (1+10 > 12 is false).
  - `in_flight_ledger_commit_clear`: note(5,[a]); clear(5) → empty.
- **Implementation** (crates/torus-consensus/src/app.rs, above `TorusApp`):
  `struct InFlightHashLedger { by_height: HashMap<u64, HashSet<B256>> }`
  with `note(height, hashes)` (entry().or_default().extend + retain h+10>height),
  `clear(height)` (remove), `extend_into(&mut HashSet<B256>)`.
- **Verify**: `nice -n19 cargo test -p torus-consensus --lib in_flight_ledger -j2`
- **Depends on**: —

### Task 2: Wire ledger into produce/validate/commit + seam test
- **Test first**: `noted_hashes_reach_selection_exclusion`: pool with 3
  actions; ledger.note 2 of their `compute_action_hash` values; build the
  exclusion set the way produce_block does (pending_proposals-derived set +
  `ledger.extend_into`); `select_native_for_block_with_senders_excluding`
  returns only the third action.
- **Implementation** (all in crates/torus-consensus/src/app.rs):
  1. Field `in_flight_hashes: InFlightHashLedger` (near `pending_proposals`,
     :499), init in constructor (Default).
  2. `produce_block` (:1246–1250): after building `in_flight` from
     `pending_proposals`, `self.in_flight_hashes.extend_into(&mut in_flight)`.
     After `self.pending_proposals.insert(height, block)` (:1321), note our
     own block's action hashes (`compute_action_hash` over its actions) so a
     same-height overwrite cannot untrack them.
  3. `validate_block` compact arm (:1371, right after `CompactBlock`
     deserializes, BEFORE `reconstruct_native_actions_hot`):
     `self.in_flight_hashes.note(compact.header.height,
     compact.native_action_hashes.iter().copied())` — this is the
     MissingData-path coverage, zero extra hashing (hashes ride the datum).
  4. `on_committed_block`: `self.in_flight_hashes.clear(height)` beside the
     existing `pending_proposals.remove(&height)` sites.
- **Verify**: `nice -n19 cargo test -p torus-consensus --lib -j2`
- **Depends on**: Task 1

### Task 3: Full suite + lint
- **Verify**: `nice -n19 cargo test -p torus-consensus --lib -j2 &&
  nice -n19 cargo test -p torus-mempool --lib -j2 &&
  cargo clippy -p torus-consensus -- -D warnings`
- **Depends on**: Task 2

### Task 4: Commit
- `git add crates/torus-consensus/src/app.rs docs/plans/dup-inclusion-tail-fix*.md PRPs/dup-inclusion-tail-fix.tasks.json`
- `git commit -m "fix(consensus): exclude unreconstructed compact-proposal hashes from selection (dup-inclusion tail)"`
- **Depends on**: Task 3

## Verification (end-to-end)
Code-level: seam test + suite green. Live (deferred, needs user go since it
bounces the seed): rebuild nice -n19 -j2, bounce, 30s bs200 bench,
`native-dup-factor.py` over the new range — expect ≤1.05 (from 1.23).

## Rollback
Single-file change; `git revert` the commit. No state, no wire, no schema
impact. Ledger is in-memory only and self-heals from empty.
