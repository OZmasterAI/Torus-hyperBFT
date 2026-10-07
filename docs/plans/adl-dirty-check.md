# Design: ADL drain dirty check without `layer_touches` (node-local)

Branch `perf/adl-dirty-check` from main `a746c408`. Follow-up to adl-budget §13.6 (owner 18c s99).

## Problem

The only measured block over ~250 ms rig is **HL 100k 10 % block B: 135-138 ms on ozarchy, 256-276
rig** (adl-budget §13.6; rig = ozarchy × 1.9-2, so the target is ≤ ~125-130 ms on ozarchy). Block B's
drain pays ~25 ms more than a later block at the same units. The profile puts the excess in the
ranking's `get_position` dirty check: `layer_touches` 15.5 % / `dirty` 11 % of block B against 6 % /
4.6 % in block 2 (§13.5 case 3).

Every ranking read goes `AccountReader::get_position` → `resident_positions` → `dirty(t)` →
`NativeStateOverlay::layer_touches`: a `RwLock` read plus two B-tree range searches (pending writes,
pending deletes) over a current layer that holds B's writes. The ranking's valuation (`pos_sums`)
asks `dirty(t)` again for the same trader. Block 2's layer is smaller, so the same search is cheaper.

## Context

* `DrainCache` (liquidation_step.rs) already holds one-per-drain state built from the overlay at the
  first ranking, including `dirty_by_market` (from `layer_keys`, the block's pending position keys).
* Every position write the drain makes goes through `DrainCache::touched(t, m)`: each close's
  counterparty and escrow, and both escrows on a pairing. The `av` cache already relies on this
  invariant and the tests check it (an AV hit must equal a fresh valuation).
* C6c's batch reader already replaces `layer_touches` with a frozen `HashSet<Address>` (`BatchSums.dirty`).
  It does not fit the drain as-is: the drain writes between rankings, and `BatchSums.memo` would then
  serve stale sums.

## Options

### Option A: drain-local dirty-trader set (recommended)
`DrainCache.dirty_traders: Option<HashSet<Address>>` holds the trader prefix of every pending
position key. It is built with the dirty map at the drain's first ranking (R attached, caches on), and
`touched` inserts into it. The ranking's `AccountReader` gets `drain_dirty: Option<&HashSet<Address>>`,
and `dirty()` consults it before `batch` / `layer_touches`. Inside a ranking there are no writes,
so the set is exact for the whole ranking.
* Files: `liquidation_step.rs`, `native_executor.rs` (reader field + `dirty()`), `trader_positions.rs`
  (one helper), tests.
* Pros: small, local, O(1) per check. Exactness is checked at every call in tests (set ==
  `layer_touches`). Caches off keeps the old path as the reference.
* Cons: correctness depends on `touched` covering every drain position write. That invariant
  already exists and the new assert enforces it.
* Effort: small. Risk: low.

### Option B: index dirty prefixes inside `NativeStateOverlay`
Keep a per-CF `HashMap<prefix, count>` updated on every put, delete, journal revert and absorb.
* Pros: every caller gets the speedup.
* Cons: 8+ mutation sites in torus-state (checkpoint revert, absorb, freeze), a cost on every write
  of every block, and the prefix length (20) is a positions-CF detail leaking into the generic overlay.
* Effort: medium. Risk: medium.

### Option C: per-key check (`t ‖ m` pending) instead of per-trader
Use R's record for `t ‖ m` when that key is clean, even if `t` is dirty in another market.
* Pros: more records hits.
* Cons: changes which path serves dirty traders (records vs overlay). It is still identical, but a
  bigger argument, and it does not remove the lookup cost by itself.
* Not needed for the target.

## Recommendation

Option A. It removes the per-read `layer_touches` from the ranking. Expected saving ≈ the block-B
`layer_touches` + `dirty` share (~15-20 ms of 138 ms), bringing HL 100k 10 % block B to roughly
118-123 ms on ozarchy (~225-245 rig). The re-measure decides.

Units and state are unchanged by construction: the read path changes, not what is read. Goldens are
not re-pinned.

## Tests (first)

* `adl_ranking_dirty_checks_use_the_drain_set` (liquidation_l1_tests): HL-like and storm shapes,
  R inline and worker. With caches on, every dirty check inside a ranking goes to the drain set
  (> 0) and none to `layer_touches`. Rows, units and results are bit-identical to the caches-off
  run (the `layer_touches` reference) block by block. Red before: the rankings ask `layer_touches`.
* In code (`#[cfg(test)]`): every set answer equals `layer_touches`. This runs in every lib test that
  drains with R.
* Existing differential tests (`adl_drain_caches_are_bit_identical`, `adl_c2_holder_lists_are_bit_identical_to_c1`,
  `adl_charged_holders_equal_a_state_walk_in_every_r_mode`) and the goldens stay green unchanged.

## Not Building (YAGNI)

* Option B (overlay-wide index): only the drain needs it now.
* Option C (per-key check).
* The `resident_positions` binary search (24.7 % in case 1): a separate item if the target is missed.
* The cold healthy-scan `build_sums` in block B (~30 ms in cases 1 and 4): a bench-shape effect
  (§13.6), outside this item.

## Open Questions

* None blocking. If the re-measure stays over ~125-130 ms on ozarchy, the next candidate is the
  records binary search (hand the holder list's record slice to the ranking).
