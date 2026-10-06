# Implementation Plan: item 6 Phase 1 (resident rows, margin sums, liquidation L1)

Status: IN PROGRESS. Written s89 (2026-10-04). s91: C1 `81a9567`, C2 `ccdb59b`, sync point 2
`d52a33f` (main 92a02ed with native anti-spam + oracle lane + oracle-signer exemption, built on
ozarchy as `merge/item6-sync2`); C3 building. s91 decisions: Gate 2 on two load shapes
(section 1.1, step 6), a per-fill track next to C3/C4 (section 5.1), 18c builds and ozarchy
(bare metal) measures (section 3). s91 profile: the 10-market gap is one function,
`same_batch_bid_top_ups` (section 5.1, step PF1, built on ozarchy in parallel with C3).
C3 `424d030` (pushed): correct, Gate 3 missed on 18c's sanity numbers (review log 18,
owner review). C4 `62af701`: correct, Gate 4 met at 10 markets, missed at 300 (review log
23). PF1 merged: `perf/item6-phase1` @ `0a25560` = C3 + C4 + PF1 (suite 2701 / 0 / 39).
Liquidation cooldown parity fix `2d03111` (whole position during the 30 s cooldown = X,
confirmed by HL's public liquidation fills; same-block rule A -> B follow-up, review log
28); tip `14236fa`, pushed. Ozarchy 300-market
profile (C3 + PF1): 0.555x main; the cost is per-account position reads (section 1.1).
s92 (2026-10-05): built and pushed on `perf/item6-phase1` (suite 2760 / 0 / 39 at `4a26653`):
C7 `82bd1a4`; P1-P4 matching per-fill fixes `b809d43` `1606853` `205f996`; fix A off-tick /
dust rejected before the book `d4ece00` + P4(b) `14a0d20`; ozarchy: trie maintenance off by
default `db6c9de`, RPC tick / lot check `44b7473`; M1 margin-phase cuts + rows 40-42
`7c365d4` `e81aa2e` `49df3eb` `b9959e2`; ozarchy C per-action results `9195c32` + typed
reasons `4a26653`; tip `c58775f`. Ozarchy 300 markets, trie off (results doc sections
10-14): 0.508x (`14236fa`) -> 0.638x (C6 + C7) -> 0.648x (`239ff69`) -> **0.760x** (M1
`90a752c`, 76.5k vs main 100.6k; unprofiled warm cell 0.91x). Decisions with the options not
taken: section 9.
s92 later: step 1 `end_resident` timers `b2bcfaa` + sums carry reuses C7's decoded positions
`1242d80` (18c ubench `end_resident` 21.1 -> 14.2 ms/block, steady marks); step 2
`end_resident` on a worker, `begin_resident` moved to just before `new_env` `2333ba4` (ozarchy
section 15: hides ~76-95 of ~105 ms/block at 300 markets) + harness columns `e65411d`; tip
`4acdc59` (suite 2770 / 0 / 39). **Gate 2 at 10 markets** (sections 15-16, interleaved main,
trie off both arms): 0.893x (`c58775f`, perf on crab) and **0.866x** without perf
(`5524646`): missed. Engine per block ~ main; the rest is outside the engine (consensus views
395 vs 335 ms, rpc / gossip-verify / ingress CPU per fill, ~12 ms/block untimed); ozarchy
section 17 breaks it down.
s94: **Gate 2 met** on B-blind `31cea69` vs main `92a02ed` (ozarchy section 19, merged into
`perf/item6-phase1`): 300 markets **1.097x**, 10 markets **0.997x**. Residual zero-fill sell cuts
0.063% of placed, so option A is deferred behind a counter trigger (section 9.10).
s94 later (`perf/item6-phase1` @ `9ab36ee`, pushed): C5 warm == cold `6571a76` (3 replicas, 210
blocks, every `h_n`); main synced (docs only since `92a02ed`, Gate 2 holds); row 43 listing check
`8d449f5`; governance `Failed` status `e439982` (row 73). Ozarchy: step 6 live-feed idle check
**passed** (section 20, `5584880`: oracle-only block max 5.92 ms, drained in 38 s; harness
`ORACLE_FEED_DRAIN=1`); moving prices (walk 10, 2 rounds, section 21 pending): **1.088x main**,
0.961x walk 0, no liquidation fired (row 76); cargo test 2806 / 0 on `5584880`.
**Remaining: pre-merge `cargo test --workspace` on `9ab36ee` (ozarchy) -> owner review -> merge
into main.** Phase 2 prep on ozarchy: generator fix `bench/max-in-flight` (open-limit rejects
39-43% of orders on both arms, section 20.3), then the step 0 profile.
**Remaining order (s92, later): E2-E4 (building; re-measure the empty block first, E1 never
built) | ozarchy: section 17 (10-market gap outside the engine), then 300 markets on `4acdc59`
(step 2) -> 10-market cuts from section 17 -> C5 (warm == cold, last) -> sync main -> Gate 2
(300 and 10 markets).** Ubench gates are ratios to the base on the same machine (section 1.2).
Design: `market-scaling-in-memory-design.md` Phase 1 + section 3.6; targets and proof
obligations: `crab-speed-target-design.md` sections 2.2, 2.3, 4, 5 ("crab doc").
Base: `perf/s87-crab-fixes` @ `9c4be2c` (s89: option B review fix `ef5eab7`, oracle-feed
command, tombstone fixes A/B + RPC staleness `3578199`/`a4c17e6`/`ac65f48`, then main merged
in at sync point 1). The `file:line` references below were taken at `3d2dcd8`; Step 0
re-checked them on `9c4be2c` and they are updated below (the s89 commits moved some lines in `native_executor.rs`,
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
| margin (Phase 2) | 7.5 | <= 1.5 | 3.0 | `margin_ms` split |
| match (Phase 3, maker checks) | 9.4 | <= 2.0 | 4.0 | `match_ms` split |
| settle | 9.2 | <= 9.5 | 12.0 | `settle_ms` split |
| liquidation tail | 16.1 | <= 1.0 | 2.0 | `tail_ms` |
| total | 46.7 | <= 14.7 | 16.6 | median of 3 |
| storage reads on margin + liquidation paths | thousands per block | **0** | > 0 | counting backend (test) |
| empty block with live feed (liquidation walk, 2048 traders) | 548 ms (walk 486) | <= 20 ms | 75 ms (= 13 blocks/s) | `ubench_epoch` `UB_DRAIN=fresh` |
| devnet matched/s, oracle on (Gate 2) | (measure) | >= 0.9x main | < 0.9x | paired alternating cells |

"Today" = Step 0 measurement on `9c4be2c` (s89): ubench_econ, marks on, walk 0, median of
3; run-to-run noise about +-10-15% (marks off: total 27.7, margin 7.1, match 8.0, settle
9.1, tail 0.7; marks on + walk 10: 48.0, within noise). The earlier prelim column
(76 total) was too high. Empty block: `ubench_epoch` `UB_DRAIN=fresh`, 4937 holders.

### 1.1 Two load shapes (s91, owner)

Goal: 300+ markets; 300 is the minimum for testnet. The budget above is the 300-market
shape (many positions per market maker: the account cost Phase 1 targets). The 10-market
shape (2-3 positions per trader: the per-fill cost) is measured as well, because every
fill pays the per-fill cost at any market count, and once C3/C4 remove the account cost
the per-fill cost dominates at 300 markets too.

| measurement | 300 markets | 10 markets |
|---|---|---|
| ubench_econ on 18c, `d52a33f` (C2), marks on, R, ms per 1k fills | 34.1 (margin 5.45, match 7.21, settle 7.54, other exec 4.50, tail 9.38) | 8.9 (margin 2.64, match 1.44, settle 2.67, other exec 1.49, tail 0.59) |
| same, without R (`UB_NO_R=1`) | see C1 (54.3 on the C1 binary) | 10.3 (tail 1.60) |
| full node on ozarchy, crab `81a9567` (C1) vs main, oracle on for crab, cap 400 | (measure) | 64.2k vs ~174-185k matched/s (~0.37x); engine 16.6 vs ~6 ms per 1k fills; exec thread 96-99% busy |
| full node, C1 vs pre-C1 `9c4be2c` | (measure) | 64.2k vs 62.1k (+3.3%, within ~5% resolution): C1's ubench gain does not show at 10 markets |
| full node on ozarchy, crab C3 + PF1 `d9ef4f7` vs main | 49.6k vs 89.4k matched/s = **0.555x**; exec CPU 30.1 vs 13.45 ms per 1k: maker_fill_fits 5.08 / 0, prepare_one 3.94 / 0, liquidation 3.81 / 0, matching 3.39 / 1.72, settle 2.43 / 3.41, glue 7.48 / 5.88; hot: overlay `get_cf_raw`, BTreeMap range, `Vec<(Vec,Vec)>` collect, `positions_for_trader` | PF1: 127.5k vs 175.9k = 0.72x (with resends), 0.66x without; exec CPU 12.5 vs 10.05 |
| exec-path CPU profile on ozarchy, crab `d52a33f` vs main `92a02ed`, ms per 1k fills | (after PF1) | crab 22.91 vs main 9.95: `same_batch_bid_top_ups` 11.21 vs 0, other margin 1.57 vs 0.10, maker checks 0.89 vs 0.04, matching 3.57 vs 2.97, settle 1.52 vs 1.41, liquidation 0.17 |

At 10 markets the ubench engine (8.9) explains only about half of the full node's 16.6 ms
per 1k fills (review log 16). Phase 1's targets (margin, match, tail) are ~4.7 of the 8.9,
so C3/C4 cannot close the 10-market gap on their own: that is the per-fill track (5.1).
The profile found the rest: one function, `same_batch_bid_top_ups`, ~86% of the gap and
~49% of crab's exec-path CPU; account-level margin itself costs ~2.3 ms per 1k fills. The
ubench missed it because its price levels are shallow (the cells' best ask levels hold
~50k orders). Units: the harness "engine ms/1k fills" is wall time of the timer at
`app.rs:2245-2309`, which on crab also covers `begin_block_oracle`, `drain_core_writer`,
`run_liquidations` and governance; main's covers only the two `execute_batch` calls.
Compare crab vs main by profile CPU or matched/s, not by that timer.

RPC (ozarchy, s91): crab's 1.8x RPC CPU per fill at 10 markets is not a crab cost. RPC CPU
per request is equal (0.95-0.97x); crab simply gets ~1.85x more submit requests per fill,
almost all fast busy refusals (closed-loop generator + slower exec = fuller backlog). It is
a symptom of the exec gap: Gate 2 is decided by the exec path. Report RPC CPU per request
and per admitted action, not only per fill.

### 1.2 Ubench gates as ratios (s91)

The absolute ms targets in section 1 were measured on 18c (KVM VPS). On ozarchy (bare
metal) the same ubench runs 2-4x faster (300 mk marks: margin 2.15 vs 5.2 on 18c), so one
build can pass on one machine and fail on the other. From s91 every ubench gate is a
ratio to the base `9c4be2c` measured on the same machine in the same session (alternating
runs): margin <= 0.20x base (1.5 / 7.5), match <= 0.21x (2.0 / 9.4), settle <= 1.0x,
tail <= 0.06x (1.0 / 16.1), total <= 0.31x (14.7 / 46.7); fail lines scale the same way.
Verdicts come from ozarchy. Gate 2 (full node vs main) stays the deciding gate.

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
  `new_with_mode` (`native_executor.rs:2260-2292`), stashed at `app.rs:2369`, then the
  nonce and marker puts, then `freeze` (`app.rs:2403`, pipelined) or the serial flush.
- Overlay read methods: `get_cf_raw` (`backend.rs:1657`), `iterate_cf` (`:1721`),
  `iterate_cf_from` (`:1759`), `prefix_exists` (`:1829`). Writes into pending only.
- `margin_configs` are loaded once per context (`native_executor.rs:2347`), never mutated
  mid-block. Marks: only `begin_block_oracle` writes the aggregate, before any action;
  `usable()` is time-based (stale after 60 s of block time).
- Liquidation values with `Marks` = listed markets with a usable mark
  (`liquidation_step.rs:65-68`); `AccountReader::mark` = any market's usable mark. They
  differ for a delisted market whose last aggregate is still fresh (up to 60 s).
- The golden test (`perf_equivalence_golden.rs:122`) and `ubench_econ.rs:86` (generator now in `tests/common/econ_load.rs`) drive the
  pipelined block loop by hand (overlay with parent, freeze, flush), not through app.rs.
- `BalanceCache` / `PositionCache` live for one `execute_batch_phases` call
  (`:4204-4208`); during Phase 2-4 nothing writes the backend (caches).

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
  position keys, stash with `height`. A fatal block returns early (`app.rs:2224-2238`,
  `:2259-2271`) without calling it: the slot was taken, so the next block rebuilds.

app.rs: change `let overlay` to `let mut overlay` at `:1931`; `begin_resident` at the top of `if run_native`
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
the slot). Replaces `BatchMarks` (`:750`, built `:4243-4249`). Liquidation's `Marks` =
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
  `:821-826`, withdrawals `check_withdrawal_margin` `:7881-7904`). `position_px` and
  `reduce_only_positions_for` stay point reads (now in memory through R).
- `BatchMakerAccounts` (`:854-881`) is deleted: `free` = balance (frozen backend during
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
Who measures (s91, owner): 18c builds (code, tests, a sanity ubench); ozarchy (Ryzen 9
5950X, 32 threads, bare metal; 18c is a KVM VPS with +-10-15% ubench noise) gives the gate
verdicts, ubench and full node. The ubench baselines are re-measured on ozarchy once
(9c4be2c, C1, C2; 300 and 10 markets); 18c numbers are not compared with ozarchy numbers.
Commands: full suite `cargo test --workspace` (base `9c4be2c`: 2601 pass / 0 fail / 39 ignored; after
Step 0: 2607 / 0 / 40);
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
  asserts `== 0` and is `#[ignore]`d until C1 (review log 1: R answers every read of both
CFs, so DB reads reach 0 at C1; C3/C4 then add per-path asserts on overlay prefix scans).
Built in Step 0: `tests/storage_reads_tests.rs` (`8814fc7`); today 20 fed blocks read
margin 1308 / 264, match 2829 / 117, liquidation 1840 / 96 (positions / balances).
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
- Gate 1: full suite and golden green; ubench `settle` not up (it already meets its target; review log 2), total down (expected:
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

s91: built LAST (after C7 and E1-E4), so it also covers C7's per-trader records and any
slot state E adds.

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
- Gate 2 (crab doc, s91 update): >= 0.9x main on BOTH shapes: 300 markets uniform (main
  gate) and 10 markets, oracle on for crab, paired alternating cells on ozarchy. Owner
  reviews; merge of crab + Phase 1 into main follows (normal merge).

## 4. Commit plan

C1 R + lifecycle + guards | C2 mark table, `BatchMarks` deleted | C3 sums cache,
`BatchMakerAccounts` deleted | C4 L1 | PF1 (ozarchy) | cooldown fix | C6 B0 + A-lite + D |
C7 per-trader positions in memory | E1-E4 empty block | C5 warm == cold tests (last). Every commit: full suite
and golden green, ubench numbers in the commit message.

## 5. Optional steps (only when a gate says so)

| id | trigger | step |
|---|---|---|
| O1 | margin or tail misses AND the re-value share is the cause | store per-position terms (crab doc 2.2, `position_terms` shared with `build`, magnitude guard, P1 extended) |
| O2 | tail misses AND dirty builds (traders touched this block) are the cause | partial re-value: sums − terms of the trader's touched markets + terms of its pending rows (needs O1) |
| O3 | settle or margin shows R point lookups | hash index over R's entries |
| L2 | tail > 1.0 ms/1k after O1/O2 (owner Q2) | healthy certificates (crab doc 2.3, P4) |

### 5.1 Per-fill track (s91, owner; not optional)

Closes the 10-market gap (section 1.1) for Gate 2's second shape. Scope comes from the
ozarchy exec-thread profile at 10 markets, crab `d52a33f` vs main `92a02ed`
(`~/bench-results-matched/ozarchy-prof10-*`): which buckets make up crab's extra ~10 ms
per 1k fills, and what of the node path the ubench does not cover (~8 ms per 1k fills).
Profile result (s91, `~/bench-results-matched/ozarchy-prof10-{crab,main}/` on ozarchy): the
gap is `NativeExecutor::same_batch_bid_top_ups` (`native_executor.rs:6484` at `d52a33f`,
from the same-batch bid bound `3d2dcd8` / `ef5eab7`). Owner s91: ozarchy builds the fix in
parallel with C3, on its own branch `perf/item6-pf1` from `d52a33f`, touching only
`same_batch_bid_top_ups` plus tests (C3 changes other functions of the same file); 18c
merges it into `perf/item6-phase1` after C3 (method A). Ozarchy measures Gate PF1.

**Step PF1: per-level ask depth in `same_batch_bid_top_ups` (commit PF1)**
- Cause: for every GTC bid at or above the best ask that may rest, the function sums
  `remaining_qty` over every resting ask ORDER up to the bid price
  (`ask_queues().take_while(..).flat_map(q.iter()).fold(..)`): O(crossing bids x resting
  asks). It is also a quadratic cost an attacker can drive (many tiny asks at one level,
  crossing bids), so it is fixed before testnet regardless of Gate 2.
- Change: the book depth per ask level, summed once per market per batch (prefix sums
  over the levels up to the batch's highest crossing bid, computed lazily on the first
  crossing bid), then a range lookup per bid. Exact: every `remaining_qty` is
  non-negative, so the saturating sum is the same in any grouping (min(true sum, MAX)).
  Same decisions, same results; no consensus or state change.
- Tests first: differential test, old walk vs prefix sums, on random books and batches
  (deep levels, many levels, empty book, bids below / at / above the levels, saturation)
  -> identical `rests` decisions and identical top-ups; golden A/B unchanged; the s87/s89
  same-batch tests green. A deep-level ubench shape (e.g. `UB_DEEP_LEVELS`: resting asks
  concentrated on a few levels) so the ubench sees this cost from now on.
- Gate PF1 (ozarchy): 10 markets full node, `top_ups` < 0.5 ms per 1k fills in the profile;
  crab exec-path CPU down by ~11 ms per 1k fills (expected ~22.9 -> ~11.7, about 2x
  matched/s, ~0.85x main). If the RPC `from_hex` load (resent shed batches) remains,
  that is its own item.
- **Result (ozarchy, `0ebfd71`):** top_ups 11.21 -> 0.02 ms/1k; exec CPU 22.91 -> 12.49
  (main 10.05 = 1.24x); matched/s 65.9k -> 127.5k (1.93x), 0.72x main. PASS (top_ups,
  exec); the rest of the 10-market gap is exec 1.24x plus its RPC symptom (section 1.1).

### 5.2 Per-account position cost at 300 markets: C6, C7 (s91, owner)

Source: ozarchy profile (section 1.1) + a read-only analysis at `0a25560` (s91). Every
active account is built once per block (per-block memo), and every account written this
block is built again in liquidation. C3's persistent cache carries nothing between blocks
(active accounts always trade). Two findings drive the order:
- **B0 (redundant parent layer).** With R attached, the overlay still consults the parent
  layer (the previous block's frozen writes) for R's two CFs, although R already holds that
  state (`own_pending_delta`: "the parent layer is already in R"). `iterate_cf`
  (`backend.rs` ~1848) takes the BTreeMap merge + double collect path whenever the parent
  touched the prefix = nearly every active trader. Skipping the parent for R's CFs in
  `get_cf_raw` / `iterate_cf` / `iterate_cf_from` / `prefix_exists` is a few lines.
- **Cliff the bench does not hit.** The dirty branch of `pos_sums` has no memo and C3
  deleted `BatchMakerAccounts`: a maker filled by an IOC / market order in the pre-EVM
  batch is rebuilt from scratch for every market it fills in the post-EVM batch. The
  bench sends GTC only, so it never shows.

Estimates (exec ms per 1k fills at 300 markets, crab ~29 after C4, main 13.45; inferred;
throughput ~ 1/exec, bounds 13.45/exec and 0.555 x 30.1/exec):

| step | what | effort | exec after | vs main |
|---|---|---|---|---|
| C6 = B0 | skip the parent layer for R's CFs | S | ~26 | 0.52-0.64x |
| C6 + A-lite | liquidation rebuild of a written trader = memo sums - terms of its R rows + terms of its pending rows (`position_terms` factored out of `build`, magnitude guard falling back to `build`; no stored terms); also removes the cliff | S-M | ~24 | 0.56-0.70x |
| C6 + D | per-batch memo for dirty traders, frozen dirty flag instead of a lock per (maker, book), per-worker maker cache before the global Mutex | S | ~23.3 | 0.58-0.72x |
| C7 | decoded positions per trader in R's slot, updated by each block's delta; valuation / maker checks read decoded structs (no format change, ~+100 MB per 1M positions) | M-L | ~19 | 0.71-0.88x |
| O1 + O2 full | stored per-position terms, cache updated at block end instead of dropped | M | ~18.5 | 0.73-0.90x |

O1 + O2 full only if marks are mostly stable (measure how many markets' marks move per
block first). What remains after C7 (~5 ms per 1k over main): matching +1.7 (F1's
book-side account checks), glue ~+1, books drain/save/load +0.8, per-order margin work
~1-1.5, liquidation ~0.5. Tests: B0 = randomised layered-overlay test with and without
parent, golden unchanged; A-lite = P1 shadow check vs `build` (already compares every
answer); C7 = records == decoded R range (P1), warm == cold (C5), guards (P7). No
consensus or state-format change in C6 / C7. Gates: ubench ratios (1.2) on ozarchy plus
the 300-market full-node cell after C6 and after C7.

### 5.3 Empty block: E1-E4 (s91)

Read-only analysis at `0a25560` (ubench_epoch fresh, ~85 ms per block). The bench adds
the exec thread E (~40 ms: ctx ~7, oracle ~19, liquidation ~13) and the writer thread W
(~44 ms flush, uncached trie, ~300 `agg` rows rewritten every block because each stores
the block time) serially; on the node they overlap. Fixes on E, no consensus change:
- E1 oracle: one scan of the `sub` range per block for prune + aggregation; the mark
  table built from the aggregation's own results (19 -> ~8 ms). Built on ozarchy
  (`perf/item6-e1` from `14236fa`, oracle code only), merged by 18c.
- E2 liquidation: cooldown / pending rows read once per block as a set instead of ~4k
  point reads; sorted trader list in R's slot so `traders_after` is a slice (13 -> ~3 ms).
- E3 oracle `sub` rows resident (decoded, delta-maintained) (~8 -> ~3 ms).
- E4 ctx: split timer first, then cache `margin_configs` in the slot (-1 to -6 ms).
Expected E ~8-12 ms. W: `TORUS_NATIVE_TRIE_MAINTENANCE=0` (node-local; the analysis found
no non-test consumer of the native root: VERIFY before deciding) or rewrite `agg` only
when its inputs change (consensus change: row bytes and staleness timing). Measure E vs W
on the node first (ozarchy idle-with-feed cell).

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
| 1 | Step 0 / counter test | DB reads reach 0 at C1 already (R serves both CFs) | un-ignore the counter at C1 instead of C3/C4? | proposed: un-ignore at C1 (harness calls `begin_resident`; `CountingBackend` forwards new trait methods); C3/C4 add per-path asserts on overlay prefix scans | `8814fc7` |
| 2 | Step 0 / budget | today 46.7 total vs prelim 76; settle 9.2 already <= 9.5 | keep absolute targets or rescale? | kept absolute targets; Gate 1 now "settle not up" instead of "settle down" | plan |
| 3 | Step 0 / noise | +-10-15% per run | gates near a fail line need more runs | proposed: 5 runs or alternating A/B runs when a result is within 15% of a gate line | plan |
| 4 | Step 0 / walk | the walk moves marks only; balances are so large that nobody is liquidated | is Gate 4 with walk representative? | noted: Gate 4 with walk measures valuation cost, not liquidation actions; a liquidation-heavy case stays in P3 tests | `3924b76` |
| 5 | Step 0 / empty block | live-feed empty block 548 ms vs <= 20 ms target (bench shape 4.9k holders x ~250 positions) | none yet: the target stands; C1-C4 are expected to close it | tracked at Gate 4 | - |
| 6 | C1 / R upkeep | `own_pending_delta` + `end_resident` 11.4 ms per block (1.1 ms/1k fills) on the exec thread, outside the ubench timed window | count it in the Gate 3/4 totals? | reported; total still well down with it added | `81a9567` |
| 7 | C1 / rebuild cost | 2.07 s per 1M rows at 1.17M rows (ubench_epoch size, built through the overlay), expected < 1 s; 0.78-0.96 s per 1M at 165k rows | ~2.4 s once per process start or guard trip acceptable, or a faster single-pass build? | not optimised; no optional step covers it | `81a9567` |
| 8 | C1 / harness scope | plan names 3 call sites | OK to wire R into a 4th harness (`ubench_epoch`), which now also emulates the node's marker-only job on non-native blocks? | done, test-only, `UB_NO_R=1` for A/B | `81a9567` |
| 9 | C1 / apply variant | `apply` moving the delta's buffers instead of copying doubled the upkeep (25-29 ms per block) and made the tail ~2.5x slower | none | reverted to copying, re-run confirmed, doc comment says why | `81a9567` |
| 10 | C1 / lockbox test | plan: deposit in N, order using it in N+1 | none | the credit lands in N+1's CoreWriter drain, after N+1's batch, so the N+1 order is rejected in both paths; the order using it is in N+2. Test asserts both | `81a9567` |
| 11 | C2 / table keys | listed + margin-config markets alone miss a delisted market with a fresh aggregate (delisting removes the market row) | OK to add one prefix scan of the aggregate rows per block? | added (`aggregated_market_ids`); such a market is in the table and `delisted_marked(listed)` sees it (needed by C4) | `ccdb59b` |
| 12 | C2 / Gate 2a marks off | first build: margin +25%, match +28% with marks off (no listed markets -> empty table -> every position read the oracle) | none | book markets added to the table as the old memo did; test failed first, then passed; whole A/B re-run on the fixed build | `ccdb59b` |
| 13 | C2 / version | plan says "increments" | process-wide counter OK? | one counter: always up, never reused (also across a slot rebuild), may skip numbers | `ccdb59b` |
| 14 | C2 / plumbing | not in the plan | accept `ctx.attach_resident_block` / `detach_resident_block` at each call site? | wired at 5 sites (app.rs, golden, ubench_econ, ubench_epoch, storage_reads); C3 reuses them for the sums cache | `ccdb59b` |
| 15 | C2 / no oracle step | contexts that never run `begin_block_oracle` (criterion benches, many unit tests) have no table | none | they read the oracle per mark (same results, slower than the old per-batch memo); the node always runs it (`app.rs:2250`) | `ccdb59b` |
| 16 | s91 / ubench vs full node | 10 markets: ubench engine 8.9 vs full node 16.6 ms per 1k fills (ozarchy); C1 +35% in the ubench, ~0 on the full node | Gate 2 on which shapes? | owner: both shapes, 300 markets main gate, both >= 0.9x; per-fill track 5.1 from the ozarchy profile; ozarchy measures | docs |
| 17 | s91 / profile | 10-market gap = `same_batch_bid_top_ups` 11.2 ms per 1k fills (O(crossing bids x resting asks)); ubench shallow levels hid it | none (owner decided) | step PF1 (5.1), built on ozarchy in parallel with C3 (`perf/item6-pf1`), merged after C3; ubench gets a deep-level shape | docs |
| 18 | C3 / Gate 3 (18c sanity, 3 pairs x 3 runs) | 300 mk marks: margin 5.27 -> 5.18 (target 1.5, fail 3.0), match 6.63 -> 5.62 (target 2.0, fail 4.0), total 33.6 -> 31.3; walk 10 the same; 10 mk 8.37 -> 8.32. Miss in every pair | No optional step covers the cause (rebuilds of traders that trade; O1 needs a re-value share, measured ~0). Profile 300-market margin / match on the full node (ozarchy, after PF1) before any O2-like step? Are the absolute targets 1.5 / 2.0 right for crab's model, given Gate 2 is the real goal? | committed as instructed (correct); verdict to be confirmed on ozarchy; owner proposal: C4 next, 300-market profile after PF1 | `424d030` |
| 19 | C3 / cache scope | the persistent cache carries 0 entries between blocks in `ubench_econ` and the fed storage test: every trader valued in a block also trades in it, so its sums are dropped at block end; only the per-block memo works (fix 1 already gave most of that). Valuation is ~0.72 of margin's 5.3 ms/1k (~14%); match values ~82k positions per block (299 makers) | keep the cache as-is? It pays off for traders valued without trading: the liquidation walk and empty blocks (C4) | kept | `424d030` |
| 20 | C3 / outside-table marks | not in the plan | OK that a result using a mark for a market outside the block's mark table is never cached (computed each time)? | guard added | `424d030` |
| 21 | C3 / storage-read test | the fixed-marks assert holds, but its traders trade every block, so it never shows a carried entry | add a non-trading-maker workload? | carried entries proven in P1 and the end_resident test | `424d030` |
| 22 | C3 / cache shape | plan: per-entry version map | one version tag per cache, cleared when the version moves, OK? | done (same behaviour, smaller); memo stores read errors / uncacheable results as "none" so each trader is computed once per block; fix 1's one-scan test moved onto the node path | `424d030` |
| 23 | C4 / Gate 4 (18c sanity, 3 pairs x 3 runs) | tail 9.83 -> 6.93 (300 mk), 10.05 -> 9.92 (300 mk walk 10) vs <= 1.0, fail 2.0: missed; 0.56 -> 0.45 at 10 mk: met. Split per block (300 mk, 585 traders scanned): traders written this block 51.8 ms (331 x ~157 us), cached 7.1, `traders_after` 3.4, other 2.7; walk 10 adds re-values 32.3 ms | build O2 (needs O1) for written traders and O1 for re-values? Proposal: decide after ozarchy's 300-market profile (is liquidation the biggest cost there?) | committed; O1 / O2 / L2 not built | `62af701` |
| 24 | C4 / empty block | ~294 -> ~90 ms (ubench_epoch fresh, 2 pairs) vs <= 20, fail 75: missed; the walk itself 232 -> 15 ms | the rest is flush ~45 ms + oracle ~19 ms, not liquidation: separate work items? | recorded | `62af701` |
| 25 | C4 / inherited diff | the first C4 builder was stopped by accident; its diff was complete (tests first verified, planted bugs caught) | none | a second builder reviewed it, measured, committed | `62af701` |
| 26 | s91 / row 23 answered | ozarchy profile 6.2 (C3 + PF1, 300 mk): liquidation 3.81 < maker_fill_fits 5.08 + prepare_one 3.94; all three are per-account position reads | none | next steps target the margin path first: C6 (B0 + A-lite + D), C7 (section 5.2); O1 + O2 full only if marks are stable | docs |
| 27 | s91 / RPC 1.8x | ozarchy: equal RPC CPU per request; more refused requests per fill because exec is slower | none | ruled out as a crab cost; gate metrics per request and per admitted action | docs |
| 28 | cooldown fix / HL reading | HL docs: whole position DURING the cooldown (X, built); wiki / article: pause, then whole AFTER (Y). Same block: A (one chunk per block, built), B (each position by its own rule), C (whole right after the first chunk). **Evidence (s91, HL public API, 44 liquidated accounts, 270 orders, `orderStatus` origSz):** X: within 30 s after a 20% chunk the next order is the whole remainder (23 / 23, 2.8-22.5 s); after 30 s 20% again (17 / 17). B: every position > 100k gets its own 20% order in the same block, <= 100k whole (up to 7 positions in one block, same hash); in the cooldown all positions whole in one block. Liquidation stops once back above MM (many single-chunk episodes); backstop rare (thin-book cascades after partly filled whole-remainder IOC orders) | none (owner: B) | X kept; follow-up commit A -> B (18c, before C6), tests first, P3 L1 vs walk under B; raw data in the s91 scratchpad `hl-liq/` | `2d03111` + follow-up |
| 29 | s91 / ubench gates | absolute ms targets are machine-dependent (ozarchy 2-4x faster than 18c) | OK to use ratios to the base on the same machine (section 1.2)? | changed | docs |
| 30 | C7 / test observability | the fed storage tests used range scans as the stand-in for "valued once per block"; C7 removes the scans | OK that per-trader valuation counts are now checked only by the in-crate counters (`sums_cache_tests`)? | the four tests assert no scan of a clean trader; a planted "records unused" bug fails all four | `82bd1a4` |
| 31 | C7 / block-end upkeep | +3-4 ms per block at 300 mk (~+0.3 ms/1k), outside the ubench timed window | count it in the gate totals? move the record update off the exec thread (8.2 "`end_resident` off the critical path")? | reported only | `82bd1a4` |
| 32 | C7 / scope | plan named valuation and maker checks | OK that reduce-only position reads also use the records? liquidation actions and other single-path reads stay on the overlay | reduce-only reads only | `82bd1a4` |
| 33 | P1 / HashMap | `AccountMargins` and `ReduceOnlyPositions` on std `HashMap` (random per-process hashing) | keep, or BTreeMap / fixed hasher as a guard against a future loop over them? safe only while never iterated (verified) | kept, doc comments on both maps | `1606853` |
| 34 | P4(b) | first build: tick `%` in `can_rest_shape` not redundant (Phase 2 had no tick check); ozarchy section 11: the `%` costs nothing anyway | none | superseded by row 39 | `205f996` |
| 35 | P4(a) / tests first | P4(a)'s equivalence check (sums shadow vs fresh `position_px`) written with the change, not before | none | a planted "no mark" bug is caught | `205f996` |
| 36 | fix A (s92, owner) | off-tick and dust orders reported ok and held a slot, margin, an order id and the D2 pool for the batch | none (owner approved; consensus change, lockstep deploy) | rejected before the book on all paths with the book's rule and tick / lot (ONE / ONE without a book); `orders_rejected_other` | `d4ece00` |
| 37 | fix A / side effect | a rejected order no longer creates a missing book (before: an off-tick / dust order to a market without a book saved an empty 1 / 1 book) | review | built | `d4ece00` |
| 38 | fix A / triggered stops | via `place_order_inner`: still a rejection, but an off-tick stop-limit no longer marks its book dirty or re-syncs its `next_order_id` (persisted book can differ; deterministic) | review | built | `d4ece00` |
| 39 | P4(b) after A | after A every order reaching `same_batch_bid_top_ups` passed the same tick / lot check | none | tick `%` and lot clauses removed, kept as `debug_assert!` | `14a0d20` |
| 40 | fix A / stop-limit limit | a StopLimit with an off-tick limit is accepted at placement, holds a slot and margin while pending, rejected only on trigger | owner s92: YES, check at placement | M1 | - |
| 41 | fix A / Limit price <= 0 | still reaches the book, holds a slot and margin, reports ok (`validate_order_price` checks only Market / Stop) | owner s92: YES, reject before the book | M1 | - |
| 42 | ozarchy B / tick source | RPC (`fix/rpc-tick-check` `44b7473`) reads tick / lot from the market row; the executor creates every book with ONE / ONE (`native_executor.rs` ~5313, ~7561) and never reads the row; all markets today 1 / 1 | none (s92: fix in M1) | M1: create books from the market row; align RPC and executor messages | - |
| 43 | M1 / listing validation | governance market listings do not check tick > 0 and lot > 0; since row 42 a lot of 0 makes a book that accepts zero-quantity orders and a tick of 0 turns the tick check off | validate at proposal time? (consensus change) | s94 owner: yes. Built: refused at proposal creation and at execution (no market row), genesis refuses; native `ListMarket` / `UpdateMarketParams` already governance-only; no 0 market in any repo genesis / fixture (128 checked) | `8d449f5` |
| 44 | M1 / undecodable market row | executor uses 1 / 1, RPC only checks the market exists (test placeholders only) | should the RPC also apply 1 / 1? | not built (recommendation: yes) | - |
| 45 | M1 / book guard | the book itself does not tick-check a StopLimit's limit; every executor path checks it before the book | add it to the book as a second guard? | not built (recommendation: yes) | - |
| 46 | M1 / RPC price <= 0 | RPC does not reject a Limit with price <= 0 at intake; the executor rejects it before the book | add it at intake? (node-local) | not built (recommendation: yes) | - |
| 47 | M1 cut 4 / end-of-block cost | 18c estimate ~8.6 ms/block; **ozarchy section 14: +29.7 ms/block** (`end_resident` 67 -> 97, untimed; `BlockSums::into_cache` ~18) | count in the gates or move off the execution thread? | step 1 (reuse C7's decoded positions + timer), then step 2 if the overlap check pays (section 9.7) | `b9959e2` |
| 48 | M1 / hasher | a faster per-process-seeded hasher (foldhash / ahash) would be a new direct dependency; not measured; s82 A/B found no gain from ahash on the exec maps | add? | not added (recommendation: no) | - |
| 49 | M1 / client-visible | RPC messages now start with "order rejected: "; off-tick StopLimit limits rejected at intake | none (heads-up for clients) | built | `7c365d4` |
| 50 | C / zero-fill IOC, crossing PostOnly | still report executed (success); recording them needs the settle loop to return a result per order | failed with new codes, or a separate "canceled" status like HL? | unchanged | `4a26653` |
| 51 | C / typed reasons | three reasons differ from C's text parser: modify price <= 0 `other` -> `price`, modify qty <= 0 `other` -> `lot`, withdrawal refused by margin `other` -> `margin` | confirm, or revert those three to `other`? | built | `4a26653` |
| 52 | C / reduce-only rejects | stored as `other` | dedicated code (next free value 8)? | not built | - |

## 9. s92 decisions: options considered, chosen, not chosen, and why

Measurements: ozarchy results doc sections 10-14 (`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`).

### 9.1 Shared-code inflation (crab vs main, 300 markets, s92 read-only analysis)
- Finding: `insert_order`, `drain_book`, `sort_native_actions`, `cancel_all_many` are byte-identical in
  both builds, so their 1.4-1.6x instructions per fill are workload; `match_at_level` /
  `match_market` inflation is F1 (every sender and maker tracked, `maker_fill_fits` on every fill:
  ~7-9 `BTreeMap<Address>` lookups and ~10 i256 `checked_mul` per fill vs ~3 on main).
- Chosen: P1 (one `HashMap` entry per trader), P2 (exact i128 fast path in `checked_mul` /
  `checked_div`), P3 (loop invariants), P4(a) (no second position read). Measured: `checked_mul`
  1.12 -> 0.26 ms/1k (section 12).
- Not chosen: a faster hasher (new dependency; attacker-chosen addresses need a seeded hasher;
  s82 found no gain), row 48. P4(b) first skipped (the tick `%` was not redundant), removed after
  fix A made it redundant (row 39); ozarchy section 11: it cost nothing anyway.

### 9.2 Book rejects reported as success (off-tick bug)
Every order the book rejects reported ok, held a slot / margin / order id / the D2 pool for the
batch, and `app.rs` discarded execution results, so users saw "executed" for every reject.
| option | gain | risk / cost | decision |
|---|---|---|---|
| A executor pre-book reject | removes the in-batch side effects; ~0 on honest load, +3-5% under off-tick spam | consensus change (lockstep deploy); serial / sharded must read the same tick | built `d4ece00` |
| B RPC intake check | immediate error for the user, bad orders never enter a block | node-local; tick source could differ from the book's (fixed by row 42) | built by ozarchy `44b7473` |
| C real per-action results | fixes "executed" for every reject type, incl. margin | write cost: storing every action ~13-26 GB/day, failures only ~KB/block | built by ozarchy `9195c32` (failures only, flush worker; measured no cost), typed reasons `4a26653` |
Not chosen: B alone (other nodes bypass it, side effects stay); storing every action's status (disk).

### 9.3 Native trie maintenance: off by default
- Options: flip the default off; keep on and set `=0` in configs; decide at Gate 2. Chosen: off
  (`db6c9de`).
- Why: nothing reads the trie in production (re-verified on `82bd1a4`: the only readers are
  `build_block_with_native` / `validate_block_with_native*`, called only from tests). History
  (git): the node used a native root by FULL SCAN in block build / validate until `02aa50c`
  (2026-05-22) removed those calls; the incremental trie was built 2026-06-07 into the
  already-orphaned path, so it was never used. Cost when on: ~160 ms/block on the flush worker,
  main -12.5% throughput.
- Reversible: `TORUS_NATIVE_TRIE_MAINTENANCE=1` plus a one-time boot rebuild (tested
  byte-identical); not consensus. Side effect: Gate 2 ratio harder (0.553x -> 0.508x at
  `14236fa`). Open: whether dropping the native root from blocks in `02aa50c` was intended.

### 9.4 Tick / lot source (row 42)
RPC read the market row, the executor created books with 1 / 1 and never read the row. Chosen:
the market row is the single source for new books (`e81aa2e`); existing books keep their stored
meta. Not chosen: leave until a non-1 market is listed (silent disagreement risk).

### 9.5 Rows 40 / 41 (owner: yes)
Off-tick StopLimit limit and Limit price <= 0 rejected before the book, same pattern as A
(`7c365d4`).

### 9.6 Measuring each step separately
Ozarchy benched P1-P4 + A (`239ff69`) and M1 (`90a752c`, without C) as separate rounds, because
C's cost was measured ~0 on its own (section 13) and M1 needed clean attribution (it exposed the
end-of-block cost, row 47). Not chosen: one combined round after M1 (faster, no attribution).

### 9.7 After M1: order of the next steps
1. Step 1: `into_cache` reuses C7's decoded positions, timer around `end_resident` (S, exact,
   ~-18 ms/block; the timer also feeds step 2).
2. Step 2: move `end_resident` off the execution thread, ONLY if it pays. Block N+1's native
   execution needs the updated resident rows, so only N+1's pre-native work (decode, signature
   checks, nonces, oracle) can overlap with it; the saving is min(`end_resident`, that window).
   A small window means an M-size concurrent change for a small gain, so skip and go to step 3.
   Ozarchy measures the window from the M1 profiles first.
3. E2-E4, C5 (last), sync main, Gate 2 at 300 and 10 markets.

### 9.8 Work split (s92)
18c builds in the item 6 files (`native_executor.rs`, `order_book.rs`, `torus-types`); ozarchy
builds isolated pieces on branches from the item 6 tip (RPC check, trie default, C) and measures
on bare metal; 18c merges with method A and runs the full suite on the merged tree.

### 9.9 Bench load note
Section 13: ~52% of actions on the 300-market bench load fail, mostly batches hitting the
open-order limit at execution. matched/s is unaffected (it counts fills), but a large share of
each block is rejected work; revisit the load generator before Gate 2's final cells.

### 8.1 Review log, s92-s94 (rows 53-79; the table above ends at 52, section 9 sits between)

| # | step / gate | measured vs target | question for the owner | what was done (proposal) | commit |
|---|---|---|---|---|---|
| 53 | step 1 / timers | `end_resident` total + 2 parts (rows, positions incl. sums carry) | none | no separate sums timer: the carry runs inside the positions pass | `b2bcfaa` |
| 54 | step 1 / irregular row | trader with an opaque row or irregular write | none | its sums entry is dropped (before: carried from R's rows); exact, costs one rebuild | `1242d80` |
| 55 | step 1 / irregular tombstone | tombstone of an irregular key under a regular trader | keep carrying, or drop as before (conservative)? | carried (exact: a regular trader has no irregular row) | `1242d80` |
| 56 | step 1 / rebuild vs adjust | positions after <= 2 x changes | none | rebuild from the decoded list (same sums, fewer terms) | `1242d80` |
| 57 | step 1 / no decoded positions | never happens in the node sequence | none | written traders dropped (exact) | `1242d80` |
| 58 | step 1 / dropped ideas | in-place copy in `ResidentRows::apply`; skip the change list when the carry is off | none | not shipped (no measurable gain) | - |
| 59 | step 2 / worker shape | one thread spawn per native block (~tens of us) | OK, or a persistent worker + channel? | spawn per block (no lifecycle / shutdown code) | `2333ba4` |
| 60 | step 2 / failure mode | a panic in `end_resident` used to panic the exec thread (poisoned mutex, halt) | OK that it now drops R and the next block rebuilds (R is a cache, the DB is authoritative)? | built | `2333ba4` |
| 61 | step 2 / spawn failure | `Builder::spawn` error drops the job | none | slot dropped, next block rebuilds (no inline fallback) | `2333ba4` |
| 62 | step 2 / worker CPU | 18c: worker-side `end_resident` +15-25% CPU vs inline (cold caches) | measure on ozarchy against verify's cores | reported | `2333ba4` |
| 63 | step 2 / harnesses | golden runs inline / worker / off; ubench_econ inline unless `UB_R_WORKER=1`; ubench_epoch, storage_reads, liquidation_l1 inline | none | as described | `2333ba4` |
| 64 | step 2 / accounting | a join on a block without a native phase counts in that block's wall | none (rare) | noted in `summarize.py` | `2333ba4` |
| 65 | step 1 / harness gap | `run-cell.sh` never sampled the `end_resident` timers (summary.json showed 0; ozarchy section 16) | none | columns added, pinned by a test | `e65411d` |
| 66 | E2 / cooldown + pending rows | plan: read once per block as a set; built: the liquidation CF moved into R | OK? (reuses R's tested guards, no per-trader reasoning) | built | `b2641cf` |
| 67 | E3 / E4 / R scope | R now holds five CFs (positions, balances, liquidation, oracle, market rows) | none (rule: any future writer of these CFs outside the block overlay must invalidate R) | metrics count all five | `8b0673d` `ecf4aec` |
| 68 | E4 / margin-config cache | ~0.4 ms per block left with real market rows | none | not built (needs the context constructor changed at five call sites) | - |
| 69 | bench fixture | `b"listed"` market rows add ~6 ms of fake ctx per block in `ubench_epoch` / `ubench_econ` (failed borsh decode allocates 1 MiB); ozarchy section 7.3's ctx was mostly this | default to real rows (breaks comparison with past runs) or a length guard in `market_margin_config`? (s92 suggestion: real rows by default, note the break) | `UB_REAL_MARKETS=1` added | `ecf4aec` |
| 70 | E1 | oracle step now 1.8-2.0 ms per empty block | none | dropped | - |
| 71 | liquidation skip (section 17 cut 3) | no exact whole-pass skip: the pass writes the hashed cursor and clears cooldown / pending rows; per-trader "healthy at mark version V" certificate is exact but saves ~1 ms per empty block after E2 | none (s92: dropped) | not built | - |
| 72 | C5 coverage | cooldown / pending rows appear in 2 of 210 blocks (one seeded account, 1,100 @100 over the 100k chunk threshold) | none | covered thinly; a dedicated cooldown sequence can follow | `6571a76` |
| 73 | governance / failed execution (pre-existing) | a passed proposal failing at execution stayed `Passed`, retried every block, and its `?` aborted the loop, so every later due proposal was skipped forever (reachable via `MarketIdInUse`) | s94 owner: fix before the merge | `ProposalStatus::Failed` (borsh 6), loop continues, per-proposal results, RPC `"Failed"`; payload errors -> Failed, storage / decode errors still stop the step; every Failed case raised before the first write (8 cases byte-checked) | `e439982` |
| 74 | governance / storage errors (pre-existing) | `app.rs` ~2315 discards `process_governance`'s results and governance never sets `fatal_error`: a storage error skips governance for that block, the node does not halt (liquidation does) | halt the node on a governance storage error? (consensus) | open | - |
| 75 | governance / partial writes (pre-existing) | TreasurySpend and PermanentUnlock write more than once; a storage error after the first write keeps it and leaves the proposal `Passed`, so a retry could pay twice | make both all-or-nothing? (recommendation: yes, before mainnet) | open | - |
| 76 | liquidation at full-node load | no cell ever fired a liquidation (walk 10 stays within +-80 bp; 0 on every node, drain included); correctness covered by `liquidation_tests` + C5 (7) | none | stress cell before testnet (larger walk or accounts seeded near maintenance) | - |
| 77 | drain after load (walk pair) | one 20-49 ms oracle-only block per node right after the load ends (both W10 runs, W0 r1); steady p50 ~4 ms | none | investigate in the Phase 2 step 0 profile | - |
| 78 | empty block with feed live | 2.5-2.8 ms vs 0.03-0.06 ms with the feed paused; likely M1's mark-carry pass over positions (timings only, not confirmed in code) | none | Phase 2 step 0 profile | - |
| 79 | trading app (separate repo) | RPC proposal status can now be `"Failed"` | none | add `"Failed"` to the app's status type | - |

### 9.10 Zero-fill sell cuts at 300 markets (s92, owner decisions)
- Finding (ozarchy section 18 + 18c read-only checks): at 300 markets ~8% of placed orders are GTC sell
  takers in a non-pool market cancelled with zero fills by match-time taker margin exhaustion
  (`orders_rejected_cancelled`, ~1.7M per cell; main 0), although accounts have ~100M free. Cause:
  the s89 same-batch bound counts earlier same-batch sells as ask depth, so it almost never fires;
  the sell is cut at its first unaffordable bid. Knock-on: unhit bids rest, senders hit the
  1000 open-order limit, crab fills 0.77x main per native block (matched/s 0.832x while
  crab's per-block execution is faster than main).
- Options compared: A second pass (design 3.6 "option D"), B better sell reservation, C bench-only
  `OPEN_ORDER_BUDGET`, D accept. Hyperliquid: one sequential engine, whole-account checks.
- Decided: **B-blind** next (after the Phase-2 fold, top up each non-pool sell to
  `reserve(B0 x (1 + 10 bps))` from leftover free margin, PARTIAL top-up, never blocks a placement;
  no input from other traders, so no griefing; replaces the s89 bound and deletes PF1's depth sums;
  counter bucketing cut sells by hit price minus reservation price). **A** only if the residual after
  B exceeds 0.5% of placed (ozarchy), first scope zero-fill sells, exact settled-state budget, retries
  lose time priority, Phase-3 stops before retry stops. Keep the D2 pool (owner Q6: complement).
  Golden A re-pinned per commit; golden B must not change.
- Not chosen: C (fixes nothing for users), D (well-funded orders fail where HL fills them), B-bid
  (counts same-batch bids; reopens griefing, breaks the s89 tests' intent).
- Result (ozarchy section 19, `31cea69` vs main `92a02ed`): Gate 2 met, 300 markets 1.097x, 10 markets
  0.997x. Residual non-pool zero-fill sell cuts 0.063% of placed at 300 markets (buckets t1_2 / t3_5 /
  t6_10 only), 0.0038% at 10; pool and partial cuts 0; all top-ups full; maker margin cancels and
  reduce-only cuts 0.
- s94 owner decision: **A deferred**, not scheduled and not dropped. Below its 0.5% threshold, and
  it is consensus-affecting (golden re-pin, a second settle + flush every block). Trigger: non-pool
  zero-fill sell cuts > 0.5% of placed on testnet or real flow (bench flow is synthetic; volatility
  past the 10 bps reservation is what produces cuts). If it fires, first try a larger delta (the
  bucket counters size it; measure the extra free margin the top-up holds), then A with the scope
  and budget above.

### 9.11 Open owner question: maker over-commit across markets in one batch (s92)
- Crab matches markets in parallel. A maker's fills are checked against a snapshot of its free margin
  taken at batch start (D8), tracked per book only; each market's book loads its own copy. A maker
  resting in many markets can be filled in several markets in one batch, each assuming the full free
  margin, so the account can end the batch using more margin than it has; liquidation catches it in
  the next block. Hyperliquid (sequential, live account) would margin-cancel the later fills.
- Not a Gate 2 issue (bench makers are well funded). A risk / bad-debt question before mainnet:
  a small account spread over many markets can over-commit for one block.
- Options (each with a cost): split each maker's free margin across the markets it rests in (safe,
  stricter than HL); reconcile after the parallel pass (fills cannot be undone: flag or cancel
  remaining orders only); serialise makers resting in several markets (closest to HL, costs parallel
  speed).
- Not in item 6 (item 6 keeps parallel matching by design). s92: tracked as its own backlog item,
  decide with a risk view before mainnet.
