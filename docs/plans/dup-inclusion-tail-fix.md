# Design: Duplicate-Inclusion Tail Fix (MissingData exclusion gap)

## Problem
s355 sweep measured residual duplicate native-action inclusion on the live
3-val testnet: factor 1.08–1.27 (8–27% of block slots wasted), with a small
tail of actions included 2–4x. The original dup fix (2e52851: leader-local +
validate-time `pending_proposals` exclusion, exec replay guard) works — gross
3x duplication is gone, execution is at-most-once. What remains is a tracking
gap, not a selection bug.

Correction recorded alongside: the "~3x duplication" inferred earlier in s355
from `native_actions_processed` vs bench "Submitted" was an artifact of the
bench under-counting submissions (it keeps submitting during the drain phase
but only counts the 30s window). Chain-body ground truth via
native-dup-factor.py: bs100 1.269, bs200 1.227, bs250 1.083, bs300 1.248;
all inter-run gap ranges empty.

## Context (from memory + exploration)
- 282f9818 / b206f59c / 1e83195f: original 2.93x diagnosis + two-layer fix.
- app.rs:1251 `produce_block` builds `in_flight` from `pending_proposals`
  (HashMap<height, TorusBlock>) and passes it to
  `select_native_for_block_with_senders_excluding`.
- `pending_proposals` is inserted at produce (app.rs:1321) AND at successful
  validate (app.rs:1463) — cross-leader exclusion exists on ALL validators
  (92cb466 has both inserts).
- THE GAP: with `COMPACT_PROPOSALS = true`, `validate_block` that fails body
  reconstruction returns `MissingData` (app.rs:1400) and inserts NOTHING.
  The compact datum's `native_action_hashes` are known at that point — but
  discarded. Under burst load (push lag) the next leader proposes without
  excluding those actions → 2–4x tail. Same-height re-proposals also
  overwrite (`HashMap::insert`) rather than union.
- Exec-side: replay guard (app.rs:323–358) keeps correctness; duplicate
  copies still burn `exec_verify` time (verify runs before the guard) —
  bounded at the same 8–27%.

## Options

### Option A: Hash ledger noted at validate time (recommended)
New `pending_proposal_hashes: HashMap<u64, HashSet<B256>>` on TorusApp.
`validate_block` notes `native_action_hashes` from the compact datum as soon
as it decodes — BEFORE reconstruction, so MissingData proposals still get
excluded; full-block datums note computed hashes. Same-height entries union
instead of overwrite. `produce_block` exclusion set = existing
`pending_proposals` hashes ∪ ledger. Evicted by the same h+10 window;
cleared at commit beside `pending_proposals.remove`.
- Files: crates/torus-consensus/src/app.rs (+tests)
- Pros: minimal diff; no wire change; mixed-binary safe; covers the measured
  gap exactly; fixes same-height overwrite for free; ≤100 hashes × 10 heights
  of state.
- Cons: second bookkeeping structure beside pending_proposals (could subsume
  it later); rebuilt-from-empty after restart (self-heals in ≤10 blocks).
- Effort: Small. Risk: Low.

### Option B: Ancestor-walk exclusion at propose time
Walk `request.block_tree()` from parent up to highest_committed (≤3 hops),
decode each ancestor datum (compact → hashes inline) and exclude those.
- Pros: exact by construction (excludes precisely the uncommitted ancestor
  chain); immune to windows/restarts; dead forks auto-release actions.
- Cons: leans on hotstuff_rs AppBlockTreeView guarantees at propose time;
  decode on the hot proposer path; more invasive to reason about.
- Effort: Medium. Risk: Medium.
- Verdict: principled follow-up if A leaves factor >1.05.

### Option C: Destructive selection + re-offer on abort
Move actions out of the pool at selection/validate; re-add if the proposal
dies. Rejected: abort detection is fork-sensitive; risk of silently losing
actions (liveness) far outweighs the 8–27% win.

## Recommendation
Option A. It closes the only mechanism the measurements still show, in one
file, mixed-binary safe, with cheap tests. Re-measure with
native-dup-factor.py after deploy; if a tail >1.05 persists, graduate to B.

## Open Questions
- Exec-verify burn on duplicate copies (verify-before-guard): defer — at
  ≤1.27x it is ≤27% of verify; a sender-independent hash pre-filter is the
  candidate if it ever matters. Carryover list.
- Bench "Submitted" accounting (drain-phase submissions uncounted): separate
  bench fix, queued with the drain-gate cosmetics carryover.
