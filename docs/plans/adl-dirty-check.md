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
position key. It is built with the dirty map, from the same `layer_keys` call, at the drain's first
ranking (R attached, C2 records on, caches on: the dirty map's condition), and
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

## Result (A/B on ozarchy, s26, measured)

A = main `a746c408` and B = `c60ee4fa`. Both were built with adl-budget §13.7's flags into separate target dirs and run
interleaved A, B × 3 as `ubench_adl` case 3 (HL, N = 100,000, 10 % holders). Each run started at load < 1.5.

| step ms (r1 / r2 / r3, median) | A | B | delta |
|---|---|---|---|
| block B (h = 50) | 149.6 / 148.4 / 150.0, **149.6** | 128.8 / 128.7 / 131.7, **128.8** | **−20.8 ms (−13.9 %)** |
| block 2 (h = 51) | 126.6 / 125.9 / 124.0, **125.9** | 122.2 / 114.5 / 123.2, **122.2** | −3.7 ms |
| blocks 2-16, all runs | median 118.5 | median 120.2 | no change |

* **Units are identical:** every run has 17 blocks with ADL work and 1,672,032 units, the same as §12.3 / §13.
  Per-block `adl_work`, rows, transfers and scanned are equal across all 6 runs.
* **Block B's excess over block 2:** ~24 ms → ~7 ms (ratio 1.19 → 1.05).
* **Profile of B** (one perf run; inclusive shares, s99 §13.5 → B):

| frame | block B | block 2 |
|---|---|---|
| `layer_touches` | 15.5 → 0.6 % | 6 → 0.0 % |
| `dirty` | 11 → 3.1 % | 4.6 → 2.7 % |
| `get_position` | 43.4 → 38.8 % | 41.1 → 39.4 % |
| `resident_positions` | 16.0 → 11.6 % | 11.2 → 9.7 % |
| `adl_rank` (sort) | 28.3 (25.3) → 31.4 (27.7) % | 33.5 → 30.8 % |
| `get_native_balance` | 9 → 12.8 % | 12 → 13.6 % |

What remains of block B's excess is the cold `liq_view` scan (~5 ms) and the extra `pos_sums` (~3 ms).

**Against 250 ms rig:** B's block B is 128.8 ms on ozarchy, so 245-258 rig: on the line. This session ran
9-15 % slower than s99 for the same shape (A's block B 149.6 vs §13's 134.6-138.0 ms; later blocks ~118 vs
~103). §13 measured `f6a54382`, not `a746c408`, and Firefox held ~70 % of a core, so it is not settled
whether the host or the code caused that gap. If it is all host, B's block B is ~118 ms at s99 speed
(224-236 rig).

After the bench, the review nit was applied: `dirty_by_market_and_traders` builds the dirty map and the
set from one `layer_keys` call (one clone + sort of block B's pending keys, not two). It is a
once-per-drain cost, and units and state are unchanged.

Raw files: `~/bench-results-matched/ubench-adl-dirty-check/` (`summary.txt`, `c3.{A,B}.r{1,2,3}.log`,
`perf-c3.B.{data,log}`, `analysis/`).
