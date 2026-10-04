# Implementation Plan: item 6 Phase 1 (resident rows, margin sums, liquidation L1)

Status: PLAN, nothing built. Written s89 (2026-10-04).
Design: `market-scaling-in-memory-design.md` Phase 1 + section 3.6; targets and proof
obligations: `crab-speed-target-design.md` sections 2.2, 2.3, 4, 5 ("crab doc").
Base: `perf/s87-crab-fixes` @ `9c4be2c` (s89: option B review fix `ef5eab7`, oracle-feed
command, tombstone fixes A/B + RPC staleness `3578199`/`a4c17e6`/`ac65f48`, then main merged
in at sync point 1). The `file:line` references below were taken at `3d2dcd8`; Step 0
re-checks them on the base (the s89 commits moved some lines in `native_executor.rs`,
`backend.rs`, `db.rs`). Integration with main (owner, s89): method A, merge main INTO the
Phase 1 branch at sync points (after a gated commit at most, and before Gate 2); no rebase.

## How this plan is organised (and why)

The crab stack defined the rules first and measured cost at the end; it came out ~10x
slower per fill and nothing stopped it on the way. This plan is organised around a
**cost budget** instead:

1. The budget (section 1) is part of the spec. Every path has a number today and a target.
2. Step 0 builds the guardrails before any engine change: the reference path, the
   differential harness, a storage-read counter that must reach zero, and the per-path
   cost split in the ubench.
3. Every step after that is: change + correctness test (tests first) + **cost gate**
   (expected, measured, go / miss).
4. Optional steps (section 5) start only when a measured gate says so. A missed cost
   gate goes straight to the optional step its gate names (owner, s89), and the miss is
   written into the review log (section 8) for the owner's review at the end. Two
   exceptions stop the work instead: a failing correctness test (differential, golden,
   P1-P7), and a missed gate that names no optional step.

## 1. Budget (ubench_econ, 300 markets, marks on, ms per 1k fills)

| path | today after fixes 3/2a/1 (prelim) | Phase 1 target | fail above | measured by |
|---|---|---|---|---|
| margin (Phase 2) | 11.3 | <= 1.5 | 3.0 | `margin_ms` split |
| match (Phase 3, maker checks) | 18.0 | <= 2.0 | 4.0 | `match_ms` split |
| settle | 13.1 | <= 9.5 | 12.0 | `settle_ms` split |
| liquidation tail | 25.7 | <= 1.0 | 2.0 | `tail_ms` |
| total | ~76 (prelim) | <= 14.7 | 16.6 | median of 3 |
| storage reads on margin + liquidation paths | thousands per block | **0** | > 0 | counting backend (test) |
| empty block with live feed (liquidation walk, 2048 traders) | ~500 ms (s89 probe) | <= 20 ms | 75 ms (= 13 blocks/s) | `ubench_epoch` `UB_DRAIN=fresh` |
| devnet matched/s, oracle on (Gate 2) | (measure) | >= 0.9x main | < 0.9x | paired alternating cells |

The first step re-measures the "today" column on the base commit (same box, same
window); the numbers above are prelim and noisy.

## 2. Design decisions in this plan

Two simplifications of the design doc. Both keep the owner's decisions (Q3: summary in
Phase 1; Q5: bit-exact) and are smaller to build and to prove. **Decided s89 (owner):
start with S1 + S2; O1 is the switch if its gate triggers.** Switching does not discard
S1: the cache, the dirty check, the per-block memo and every consumer stay; only how a
stale entry is recomputed changes, and P1 checks both forms.

- **S1. The summary caches sums, not per-position terms.** For each trader it caches the
  four position sums plus two counts, computed by `AccountView::build` itself over the
  trader's rows in R (in memory). It is bit-exact by construction because it is the same
  function on the same rows. There is no second formula, no magnitude guard and no
  partial-sum proof. Cost difference vs stored per-position terms: only when marks move,
  ~80 ns (decode + arithmetic) vs ~30 ns per re-valued position. Stored terms are the
  optional step O1, built only if the gate shows that difference.
- **S2. No balance mirror.** Balances are point reads from R (in memory). The cache holds
  only what depends on positions and marks.

Facts that shaped the design (verified at `3d2dcd8`):
- The overlay is built at `app.rs:1931` before verify; the book holder is taken in
  `new_with_mode` (`native_executor.rs:2252-2290`), stashed at `app.rs:2369`, then the
  nonce and marker puts, then `freeze` (`app.rs:2403`, pipelined) or the serial flush.
- Overlay read methods: `get_cf_raw` (`backend.rs:1641`), `iterate_cf` (`:1705`),
  `iterate_cf_from` (`:1743`), `prefix_exists` (`:1813`). Writes into pending only.
- `margin_configs` are loaded once per context (`native_executor.rs:2344`), never mutated
  mid-block. Marks: only `begin_block_oracle` writes the aggregate, before any action;
  `usable()` is time-based (stale after 60 s of block time).
- Liquidation values with `Marks` = listed markets with a usable mark
  (`liquidation_step.rs:65-68`); `AccountReader::mark` = any market's usable mark. They
  differ for a delisted market whose last aggregate is still fresh (up to 60 s).
- The golden test (`perf_equivalence_golden.rs:122`) and `ubench_econ.rs:200` drive the
  pipelined block loop by hand (overlay with parent, freeze, flush), not through app.rs.
- `BalanceCache` / `PositionCache` live for one `execute_batch_phases` call
  (`:4201-4206`); during Phase 2-4 nothing writes the backend (caches).

### 2.1 R: resident rows (torus-state)

- New `torus-state/src/resident_rows.rs`: `ResidentRows` = one `BTreeMap<Vec<u8>, Vec<u8>>`
  each for `CF_NATIVE_POSITIONS` and `CF_NATIVE_BALANCES` (all keys, including `cvlm`
  rows and any non-28-byte key). `build(&overlay)` = `iterate_cf(cf, None)` of both CFs
  through an overlay WITHOUT R (so DB + parent layer = post-state of the previous block).
  `apply(&ResidentDelta)` applies puts and deletes. `ResidentDelta` = the block's own
  pending writes and tombstones of the two CFs, sorted.
- `NativeStateOverlay` gets `resident: Option<Arc<ResidentRows>>`, set by
  `attach_resident` before the overlay is cloned. For R's two CFs every read uses R in
  place of the DB: `get_cf_raw` (pending -> parent -> R; a miss is "absent"),
  `iterate_cf` (R's prefix range replaces the materialised DB Vec; the existing
  `overlay_into` merge stays), `iterate_cf_from` (R's range replaces the RocksDB iterator
  in the k-way merge), `prefix_exists`. Other CFs: unchanged.
- `own_pending_delta()` (for the end of the block) and `layer_touches(cf, prefix)` (own
  pending only; the parent is already in R). `layer_touches` is a `StateBackend` method
  with default `true` ("assume dirty"), overridden by the overlay.

### 2.2 Lifecycle: one function used by app.rs, the golden and the ubench

`ResidentBooks` gets a second slot `rows: Option<RowsSlot { rows: Arc<ResidentRows>,
sums: SumsCache, marks: BlockMarksState, height }>` with the books' guard semantics
(take; reuse iff `height + 1 == block` and the applied marker == `height`; `invalidate`
drops it; `advance_untouched` advances it). Not gated by `TORUS_RESIDENT_BOOKS` (D16).

Two functions in `native_executor.rs`, called by app.rs, `perf_equivalence_golden.rs`
and `ubench_econ.rs` (three call sites, so the harnesses measure the real path):
- `begin_resident(holder, &mut overlay, height) -> ResidentBlock`: take the slot or
  build it (metric `exec_resident_rows_rebuilds`), attach R to the overlay.
- `end_resident(holder, block: ResidentBlock, delta, ok: bool)`: after the hand-off
  (pipelined) or a successful flush (serial) and after the overlay is dropped:
  `Arc::get_mut` on R (if another clone is alive: warn, metric, leave the holder empty =
  rebuild next block; a test asserts this never happens in the normal sequence), apply
  the delta, merge the block's sums memo, drop the sums of every trader in the delta's
  position keys, stash with `height`. A fatal block returns early (`app.rs:2226-2238`,
  `:2261-2271`) without calling it: the slot was taken, so the next block rebuilds.

app.rs: `let mut overlay` at `:1931`; `begin_resident` at the top of `if run_native`
(only sessions, nonces and the oracle / liquidation due-checks read the overlay before
that; none of them reads R's CFs); `own_pending_delta()` right before `freeze` / the
serial flush; `end_resident` after the hand-off / flush. Non-native blocks: the existing
`advance_untouched` call (`:2729-2732`) advances the slot too. Step 0 checks whether an
EVM batch, the slash overlay (`:1743`) or the hash extras can write R's CFs; if any can,
those blocks invalidate the slot.

### 2.3 Block mark table

`BlockMarks { marks: HashMap<MarketId, Option<FixedPoint>>, version: u64 }` on the
context, filled at the end of `begin_block_oracle` for every market in `margin_configs`
plus the listed ones (`get_price(m, now).usable()`, the same call as today); a market
outside the table reads the oracle directly, as `BatchMarks` does today. `version`
increments when the table or `margin_configs` differ from the previous block's (kept in
the slot). Replaces `BatchMarks` (`:750`, built `:4240-4246`). Liquidation's `Marks` =
the table filtered to listed markets; `delisted_marked` = markets with a mark that are
not listed (normally empty).

### 2.4 Sums cache

```rust
/// Position-dependent part of `AccountView` (S1): exactly `build`'s sums.
#[derive(Clone, Copy)]
struct PosSums { upnl: FixedPoint, position_im: FixedPoint, notional: FixedPoint,
                 maintenance: FixedPoint, any_isolated: bool, marked: u32 }
```

- Persistent (in the slot, read-only during a block): `HashMap<Address, (u64 /*mark
  version*/, Result<PosSums, ()>)>`. Per block: a memo `Mutex<HashMap<Address,
  Arc<OnceLock<Result<PosSums, ()>>>>>` (fix 1's pattern, safe under parallel Phase 2/3).
- `AccountReader::pos_sums(trader)`:
  1. no cache attached (tests, reference path) -> `build` over `positions_for_trader`;
  2. `layer_touches(POSITIONS, trader)` (dirty this block) -> `build` over the overlay
     (pending + R), as today but in memory;
  3. persistent entry at the block's mark version -> it;
  4. else memo `get_or_init(build over R rows)`.
  `Err(())` reproduces `CoreError::Overflow("account margin overflows i128")`.
- Consumers: `view` / `pos_net` (Phase-2 `prepare_one` `:4684-4695`, maker `maker_free`
  `:821-826`, withdrawals `check_withdrawal_margin` `:7829-7856`). `position_px` and
  `reduce_only_positions_for` stay point reads (now in memory through R).
- `BatchMakerAccounts` (`:854-880`) is deleted: `free` = balance (frozen backend during
  Phase 3) + memoised sums is already identical in every market.

### 2.5 Liquidation L1

- `traders_after`, stage 1, backstop, ADL and `adl_candidates`: no code change; they read
  R through the overlay (in memory).
- `liq_view` (`liquidation_step.rs:149-169`): with a cache attached and
  `delisted_marked` empty, use `pos_sums`: `None` if `any_isolated`, `marked == 0` or
  `Err`; else `AccountView` from the R balance and the sums. Otherwise: today's code.
  The fix 2a guard stays (one line; it still saves the dirty builds when no mark exists).

## 3. Steps

Each step: tests first (they fail before the change), then the change, then the gate.
Commands: full suite `cargo test --workspace` (base: 2571 pass / 0 fail / 38 ignored);
golden `cargo test -p torus-bridge --test perf_equivalence_golden`; ubench
`UB_MARKS=1 UB_MARKETS=300 cargo test -p torus-bridge --release --test ubench_econ -- --ignored --nocapture`
(and `UB_MARKS=0`). Own worktree `wt/item6-phase1`, own `CARGO_TARGET_DIR`
(`~/.cargo-target-item6-p1`). Never bench while a build or another agent runs.

### Step 0: guardrails (no engine change)

- 0.1 Branch `perf/item6-phase1` from the base; full suite, golden, ubench (both mark
  settings, 3 runs each) on the base: fill the "today" column of section 1.
- 0.2 Storage-read counter test (`torus-bridge/tests/`, using `common/counting_backend.rs`):
  run a fed 20-block sequence and count DB reads of `CF_NATIVE_POSITIONS` /
  `CF_NATIVE_BALANCES` per path (margin, match, liquidation). Today: > 0. The test
  asserts `== 0` and is `#[ignore]`d until step 3; step 3 un-ignores it.
- 0.3 Audit: every writer of R's two CFs outside the overlay (expected: genesis before
  boot, the bench-throughput seeding context); EVM batch / slash overlay / hash extras
  touching R's CFs; any reader of the two CFs that bypasses the overlay on the block path.
  Result goes into the commit message of step 1.
- 0.4 Reference switch for tests only: `#[cfg(test)]` app field
  `test_no_resident_rows` (like `test_book_mode`) and `begin_resident(None, ..)` in the
  harnesses = today's path. No runtime flag (D16).
- 0.5 Moving marks. Both bench mark sources submit ONE fixed price per market
  (`ubench_econ` `UB_MARKS=1`: the mid every block; devnet `oracle-feed`: `--price`,
  default 30000, `oracle_feed.rs:76-80`). With fixed marks the mark version never
  changes, so the re-value cost (O1 / L2 triggers) is never measured. Add a small
  deterministic mean-reverting walk: `UB_MARK_WALK=<bp per block>` in the ubench and
  `--walk-bp` / `ORACLE_WALK_BP` in the feeder (default 0 = today). Size it so that
  liquidations stay rare (e.g. 10 bp per block around the mid). Gates 3 and 4 are
  measured with walk 0 AND with the walk; the walk numbers decide O1 / O2 / L2.
- Gate 0: base numbers recorded (walk 0 and walk on); counter test fails as expected.

### Step 1: R (commit C1)

- Tests first:
  - `resident_rows`: `build` == `iterate_cf` of both CFs; `apply` puts/deletes/re-puts;
    `cvlm` and odd-length keys kept.
  - Overlay property test: random pending / parent / R / DB states: `get_cf_raw`,
    `iterate_cf` (prefix and `None`), `iterate_cf_from`, `prefix_exists` == the same reads
    on a DB where everything was flushed. R authoritative: a DB row absent from R is
    invisible.
  - App: R == DB scan after every block (serial and pipelined); same block sequence with
    and without R: identical CF dumps and `h_n`, all four BookModes; guard P7 (marker
    mismatch, skipped height, fatal block -> rebuild, never stale); `Arc::get_mut`
    fallback count 0; EVM lockbox deposit (0x0820) in N, order using it in N+1; crash
    between hand-off and W's write -> restarted node identical (P6).
- Change: 2.1, 2.2, metrics (`exec_resident_rows`, `_bytes`, `_rebuilds`,
  `_build_seconds`).
- Gate 1: full suite and golden green; ubench `settle` down, total down (expected:
  pass-A point reads in memory); rebuild cost at bench size measured (< 1 s per 1M rows
  expected). If settle or margin is up from R point lookups: O3. If total is up for
  another reason: stop (no optional step).

### Step 2: block mark table (commit C2)

- Tests first: table == `get_price(m, now).usable()` for every market, including a mark
  going stale by time, absent, non-positive, and a delisted market with a fresh
  aggregate; version bumps exactly when table or configs change; goldens unchanged.
- Change: 2.3; delete `BatchMarks`.
- Gate 2a: golden green; ubench oracle reads per block = markets (counter), margin and
  match not worse.

### Step 3: sums cache + consumers (commit C3)

- Tests first:
  - P1: seeded sequences (open, increase, partial close, flip, full close; balance-only
    writes; marks fresh / stale / absent / reappearing; flat and multi-tier configs; a
    listing mid-run; negative `available`; Isolated and overflow-sized positions): after
    every block and at every consumer call, cache path == reference path (`view`,
    `pos_net`, `free`, `transfer_required`, `Err`).
  - P2: golden A and B unchanged with the cache attached (harnesses call
    `begin_resident`); `account_margin_tests`, `maker_margin_release_tests`,
    `market_order_margin_tests`, `reduce_only_tests`, `engine_parallel_tests` green.
  - Un-ignore the step-0 counter test for margin and match: 0 storage reads.
- Change: 2.4; delete `BatchMakerAccounts`.
- Gate 3: margin <= 1.5 and match <= 2.0 ms/1k with marks (fail lines 3.0 / 4.0).
  If marks move every block in the fed ubench and margin misses: measure the re-value
  share, then O1.

### Step 4: liquidation L1 (commit C4)

- Tests first:
  - P3: same block sequences with the reference walk and with L1: identical
    `CF_NATIVE_LIQUIDATION` rows, positions, balances, results, metrics and `h_n` per
    block (cursor cuts, cooldown, stage-1 chunks, backstop, ADL of a trader and of the
    vault, marks on/off, a delisted market with a fresh mark).
  - `liquidation_determinism_serial_pipelined_and_replay_are_identical` (`app.rs:16388`),
    chaos `liquidation_step_keeps_incremental_root_equal_to_full_scan` (`chaos.rs:447`).
  - Counter test for liquidation: 0 storage reads.
- Change: 2.5.
- Gate 4: tail <= 1.0 ms/1k with marks (fail 2.0). If missed: split the tail into dirty
  builds (traders touched this block) vs re-values vs walk, then O2 or L2 (owner Q2).

### Step 5: warm == cold (commit C5, tests only)

- P5: 3 replicas over 200+ fed blocks, one restarted every K blocks (slot rebuilt cold),
  one with the sums cache dropped every block: identical state and every `h_n`.
- Gate 5: green.

### Step 6: Gate 2 measurement (owner starts the campaign)

- Devnet crab cells, oracle on (needs item 2's `run_cell.py` allowlist), paired
  alternating cells vs main and vs the base branch, warm-up cell, AGREE/PASS, drain.
  Record matched/s, CPU-s/1M, engine split, `rejected_cancelled`, RSS.
- Live-feed idle check (s89): one extra cell with the oracle feed kept running through
  the drain (the harness pauses it today, so marks go stale and the liquidation walk takes
  its cheap path). s89 probe (`ubench_epoch.rs`): with the feed live, every empty block
  runs the native phase and the walk costs ~0.5 s per block in the bench shape (5k
  traders x ~249 positions), so execution cannot keep up with an idle chain (~13 empty
  blocks/s). Pass = the node drains with the feed live; record empty-block exec ms.
- Gate 2 (crab doc): >= 0.9x main. Owner reviews; merge of crab + Phase 1 into main
  follows (normal merge).

## 4. Commit plan

C1 R + lifecycle + guards | C2 mark table, `BatchMarks` deleted | C3 sums cache,
`BatchMakerAccounts` deleted | C4 L1 | C5 warm == cold tests. Every commit: full suite
and golden green, ubench numbers in the commit message.

## 5. Optional steps (only when a gate says so)

| id | trigger | step |
|---|---|---|
| O1 | margin or tail misses AND the re-value share is the cause | store per-position terms (crab doc 2.2, `position_terms` shared with `build`, magnitude guard, P1 extended) |
| O2 | tail misses AND dirty builds (traders touched this block) are the cause | partial re-value: sums − terms of the trader's touched markets + terms of its pending rows (needs O1) |
| O3 | settle or margin shows R point lookups | hash index over R's entries |
| L2 | tail > 1.0 ms/1k after O1/O2 (owner Q2) | healthy certificates (crab doc 2.3, P4) |

## 6. Rollback

No consensus rule, state format or hash change: the previous binary runs on the same
data. Each commit can be dropped from the top of the branch; the branch stack (D16) is
the A/B.

## 7. Owner decisions (s89)

1. S1 + S2 (section 2): start with cached `build` sums over R and no balance mirror; O1
   (stored per-position terms) is the switch if its gate triggers.
2. Missed cost gate: go straight to the optional step it names, log it (section 8),
   owner reviews the log when Phase 1 is done. Correctness failures and gates without an
   optional step stop the work.

## 8. Review log (filled during the build, reviewed by the owner at the end)

One entry per missed gate or judgement call. Nothing here is decided until the owner
reviews it.

| # | step / gate | measured vs target | question for the owner | what was done (proposal) | commit |
|---|---|---|---|---|---|
| | | | | | |
