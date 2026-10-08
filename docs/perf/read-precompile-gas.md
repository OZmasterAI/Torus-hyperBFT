# Read precompile gas per unit (sizing the 50-gas placeholder)

> Superseded by the s99 owner decisions (section "s99 owner decisions: implemented and measured")
> and the s100 decisions (last section, "Decisions (owner s100)").

ozarchy, 2026-10-07. Bench `crates/torus-bridge/tests/ubench_read_precompile_gas.rs`
(branch `bench/read-precompile-gas`, base `98f035ee`). Release, `-C force-frame-pointers=yes`,
mold, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`. 3 repetitions, each started at 1-min load
< 1.5 (1.00 / 1.00 / 1.07), unit `bench-read-gas.service`. RocksDB on btrfs (`TMPDIR` under
the results dir; `/tmp` is tmpfs). Results and per-rep logs:
`~/bench-results-matched/read-precompile-gas/` (`rep{1,2,3}.log`, `summary.txt`). All numbers
are medians of the 3 runs; the spread between runs is under 10 % except the single-sample
`cold` whole-CF scans.

## Units and current pricing (precompiles.rs, as merged in 882b0620)

A reader call (0x0800-0x0803) pays `GAS_PRECOMPILE_READ` = 2,600 plus
`GAS_PRECOMPILE_READ_PER_UNIT` = 50 per unit (`reader_gas`). Its work is capped at
`(gas_limit - 2,600) / 50` units (`reader_budget`, set by `TorusPrecompiles::run`). One unit is:

* one hashed row that a prefix scan returned (`scan_prefix_metered`: getMarkets, getOpenOrders,
  getAllPrices, getStakingInfo's delegations, getValidators, getOrderBook in row layouts);
* 32 bytes of a classic whole-book blob (sized with `get_cf_len` and charged before it is read);
* one 32-byte word of the answer (charged after the answer is built).

Not charged beyond the base: point reads (`get_cf_raw`: getPosition, getBalances, getPrice,
the staking permanent / rewards rows, the classic blob read itself), RocksDB deletion markers
and node-local rows a scan steps over, and decoding.

## Method

Part 1: `execute_precompile_metered` over a `NativeStateOverlay` of a `StateDb` (the backend
and meter the provider passes), budget `reader_budget(30M)`. Sizes N = 0, 1, 4, 16, 64, 256,
1024; 16 targets (distinct keys) per size; 300,000 random filler rows each in orders,
delegations, positions and balances. States: `mem` (rows in the memtable), then every CF
flushed + compacted and the DB reopened: `cold` (the first call of each target: block cache
cold, OS page cache warm) and `warm` (repeated calls). Fit ns = fixed + slope x N;
ns per unit = slope / units per N.

Part 2 (end to end): a contract STATICCALLs one reader in a loop with a fixed stipend until a
30M-gas tx is spent, through `EvmExecutor::execute_tx` (the real `TorusPrecompiles`), and SLOAD
loops (a new slot each time, cold 2,100 gas; one slot, warm 100 gas) for comparison.

## ns per unit (median of 3)

| unit (case) | words per row | mem ns | warm ns | cold ns |
|---|---|---|---|---|
| row only (getStakingInfo: N delegations, 4-word answer) | 0 | 324 | 383 | **513** |
| row only (getOrderBook, mode 1, N orders at one price) | 0 | 360 | **417** | 469 |
| row + 2 words (getOrderBook, mode 2, N levels) | 2 | 351 / 3 = 117 | 407 / 3 = 136 | 439 / 3 = 146 |
| row + 2 words (getMarkets) | 2 | 313 / 3 = 104 | 374 / 3 = 125 | 421 / 3 = 140 |
| row + 3 words (getAllPrices / getValidators) | 3 | 81 / 92 | 95 / 110 | 103 / 124 |
| row + 4 words (getOpenOrders) | 4 | 350 / 5 = 70 | 413 / 5 = 83 | 491 / 5 = 98 |
| 32 B classic blob (getOrderBook, classic) | - | 2.2 | 0.8 | 8.2 |

A row costs 310-520 ns whether the answer has 0, 2 or 4 words per row, so a word is at most
~10-20 ns and a blob unit ~1-8 ns: **the row is the expensive unit**, and the worst pattern is
a scan whose answer does not grow with the rows (getStakingInfo; getOrderBook in mode 1).

Fixed cost per call (N = 0 / point reads): mem 0.9-2.4 us, warm 1.5-6.5 us, cold 2-20 us.

## Worst pattern, 30M-gas block at 50 gas per unit

| pattern | ns/gas | 30M-gas block, ozarchy | rig (x 1.9-2) |
|---|---|---|---|
| e2e getOrderBook mode 1, 1,024 orders at one price (warm) | 7.81 | **234 ms** | 445-470 ms |
| e2e getStakingInfo, 1,024 delegations (warm) | 7.25 | 218 ms | 415-435 ms |
| part 1 getStakingInfo, 1,024 delegations (cold) | 10.08 | 302 ms | 575-605 ms |
| e2e SLOAD cold (2,100) / warm (100) | 0.46 / 0.47 | 14 ms | 26-28 ms |

The default book mode is classic (blob units are cheap), so on a default node the worst
priced pattern is getStakingInfo; mode 1 applies with `TORUS_BOOK_ROWS=1`.

## Budget and recommendation

Budget: the ~250 ms block target applied as rig-equivalent ms = ozarchy ms x 1.9-2
(`docs/plans/adl-budget.md` section 11, with the section 9 factor), so **125 ms ozarchy** for the
30M-gas block (4.17 ns per gas).

Row-only reads at g gas per unit tend to ns-per-row / g per gas, so g >= ns-per-row / 4.17:

| worst row cost used | g needed (125 ms ozarchy) | g needed (if 250 ms is read as ozarchy ms) |
|---|---|---|
| cold, 513 ns (getStakingInfo) | 123 | 62 |
| warm, 417 ns (mode 1 book) | 100 | 50 |
| e2e warm, 390 ns (mode 1 book) | 94 | 47 |

**Recommendation: `GAS_PRECOMPILE_READ_PER_UNIT` = 125.** The worst measured case then runs at
4.15 ns/gas (getStakingInfo, 1,024 delegations, cold: 544 us for 131,100 gas), a 30M-gas block
in ~125 ms ozarchy (~240-250 ms rig); warm row-only reads take ~94-100 ms ozarchy. 50 gas only
fits if the 250 ms is read as ozarchy ms and the reads are warm.

Option (consensus change, owner decision): price the unit types apart, ~125 gas per row and a
few gas (EVM copy price, 3 gas) per returned word / 32 blob bytes. A single price of 125
overcharges word-heavy answers 3-5x (getOpenOrders: 83 ns per unit warm).

Sanity point: at 125 gas a scanned row (~400 ns) still costs ~3-4 ns per gas, against ~0.46 ns
per gas for the EVM's own SLOAD loops on this node (an SLOAD of a new slot costs ~1 us for 2,100 gas
here, its block already cached).
Matching SLOAD's ns per gas would need ~1,000 gas per row; the block budget, not SLOAD parity,
sets the 125.

## Not fixed by the per-unit price

* **Deletion markers** (getOpenOrders over a trader + market whose N orders were written and
  deleted): ~80-100 ns per marker, uncharged; the answer stays 8 units (3,000 gas at 50). 1,024
  markers: 27.3 ns/gas end to end, a 30M-gas block in **~820 ms ozarchy** at any per-unit price.
  The markers survived this bench's flush + `compact_range` (likely a trivial move), so they can last until
  a real compaction rewrites the file. Every filled or cancelled order leaves one in
  `cf_native_orders`. Bounding this needs a skip limit on the iterator
  (`ReadOptions::set_max_skippable_internal_keys`, an error past it) or a scan cost that counts
  the keys the iterator visits.
* **Cold point reads** (block cache miss): getPosition / getBalances 18-20 us per call
  (~7 ns/gas, ~216 ms per 30M) and N = 0 scans ~12-14 us. That is the base (2,600) and the
  cache size, not the per-unit price. An SLOAD that misses the block cache would pay the same
  miss (not measured).

## s99 owner decisions: implemented and measured (ozarchy, 2026-10-07)

### Decisions (owner s99, final)

* **Base 16,400** gas for every reader call (0x0800-0x0803): single reads at Hyperliquid level,
  getPosition = 16,400 + 5 words x 20 = 16,500.
* **Scans: 500 gas per scanned row + 20 per returned word or 32 B blob chunk.**
* **getOpenOrders removed** from 0x0800 (it read `cf_native_orders`, which no production code
  writes, so it always answered empty). Its selector now reverts like any unknown selector
  (16,400 gas through the EVM). `CF_NATIVE_ORDERS` and its running-hash entry stay (state layout).
* **getOrderBook: the 64 best levels per side in modes 2 / 3** (level rows), best-first, no revert
  past the cap; **mode 1 removed** (order rows by id have no price index: a market with order rows
  reverts as an unsupported layout, deterministically, 16,900 gas); **classic unchanged**.
* **The previous block's uncharged deletes are accepted while scan reads exist** (~90-100 ms flat
  per 30M-gas block for one trader at the open-order limit, measured below).
* **RocksDB tombstones are never charged** (their count is node-local); a node-local, per-market
  background compaction with threshold T = 64 uncompacted deletes removes them.

Plan section 9.16 is updated by 18c at merge.

### Setup

Branch `bench/read-precompile-gas`. Bench `crates/torus-bridge/tests/ubench_read_precompile_gas.rs`,
release, frame pointers, mold, line-tables-only; 3 reps each started at 1-min load < 1.5; results
are medians of 3, spread under 5 % unless noted. Results, per-rep logs, `summary.txt`:
`~/bench-results-matched/read-precompile-gas-ac/`:

| run | code | parts |
|---|---|---|
| `before` | origin/main pricing (2,600 + 50 per unit), old bench (c137f816) | all |
| `after` | 76eb081c (first a-c commit), old bench | all |
| `review-before` | 76eb081c, review bench (one compaction span per CF, Force) | 3-5 (mode-1 rows) |
| `review-after` | final code (per-market + T = 64, ForceOptimized, 64 levels / side) | all |
| `review-nocompact` | final code with the delete trigger disabled (RocksDB's own compaction only) | 4-5 |

"Block" = a 30M-gas block of one reader. "e2e cold" = a contract STATICCALLs the reader in a loop
through `EvmExecutor::execute_tx`, each call on a different existing target (2,000 positions /
balances / stakers / prices; 700 markets of 64 bid + 64 ask levels), first tx after a flush +
compact + reopen (block cache cold, page cache warm). "Part 1 cold" = `execute_precompile_metered`
on the first call of each of 16-64 targets after the reopen (also pays the first loads of index /
filter blocks), scaled to 30M gas.

### Pricing as implemented (`crates/torus-core/src/precompiles.rs`)

| constant | value |
|---|---|
| `GAS_PRECOMPILE_READ` (base of every reader call) | 16,400 |
| `GAS_PRECOMPILE_READ_PER_ROW` (hashed row a prefix scan returned) | 500 |
| `GAS_PRECOMPILE_READ_PER_WORD` (32 B word returned, or 32 B of a classic blob) | 20 |
| `READER_MAX_LEVELS_PER_SIDE` (getOrderBook, modes 2 / 3) | 64 |

`ReadMeter` counts gas above the base (`reader_gas(used) = base + used`,
`reader_budget(gas_limit) = gas_limit - base`); point reads are in the base; deletion markers are
never charged. One base for all readers, scans included: both scan targets are met with it.

### (a) Exact base

Target: worst cold single-read 30M-gas block ~35 ms = 1.167 ns per gas; base = G - 100 with
G = t_cold x 30M / 35 ms.

| cold getPosition measure (`before` build, median of 3) | t per call | G for 35 ms | base |
|---|---|---|---|
| part 1 cold, first call per position | 20.17 us | 17,290 | 17,190 |
| part 1 cold, mean of getPosition 20.17 / getBalances 18.44 us | 19.31 us | 16,550 | 16,450 |
| e2e cold block, 2,000 distinct positions (34.98 ms / 2,000) | 17.49 us | 14,990 | 14,890 |

Chosen and kept: **base 16,400**, getPosition 16,500.

| reader | gas | block e2e cold, before -> final | block e2e warm, final | part 1 cold, after |
|---|---|---|---|---|
| getPosition | 16,500 | 170.7 -> **34.2 ms** | 9.3 ms | 37.1 ms |
| getBalances | 16,480 | 79.6 -> **15.2 ms** | 9.1 ms | 33.7 ms |
| getPrice | 16,460 | 27.6 -> **4.9 ms** | 4.9 ms | 3.8 ms |
| getStakingInfo (1 delegation; +500 per delegation) | 16,980 | 150.0 -> **28.3 ms** | 15.7 ms | 27.0 ms |
| SLOAD loops (reference, cold / warm) | 2,100 / 100 | 12.8 / 18.5 -> 13.7 / 13.6 ms | | |

### (b) Scans and getOrderBook

getOrderBook (`book_levels`):

* **Modes 2 / 3**: two bounded scans, `market ‖ 0x03 ‖ bid` then `market ‖ 0x03 ‖ ask`, 64 rows
  each at most, keys in best-first order. Gas = 16,400 + 500 x levels + 20 x (8 + 2 x levels);
  the full cap (64 + 64) = **85,680**. A key of another length under those prefixes reverts
  (corrupt row store).
* **Mode 1**: when the market has no level row, one order row (`market ‖ 0x01`) is probed and
  charged; if there is one, the call reverts (`BookLayout`, "does not serve the order-row layout"):
  16,400 + 500 = **16,900** gas, the same on every node. A mode-1 market whose orders are all gone
  answers the empty book (16,560).
* **Classic**: unchanged and uncapped; the blob is sized and charged 20 per 32 B before it is read
  (a production blob still reverts).
* Meta and stop rows are not read. Not detected by the EVM reader (an extra read per call): a
  market holding both level and order rows, or rows without a meta row; the RPC readers
  (`book_reader::read_book_depth`) still reject both.

Other scans (getMarkets, getAllPrices, getValidators, getStakingInfo's delegations) are not capped
(bounded by governance / the validator set) and pay 500 per row + 20 per word.

| case | gas | block before | block final: e2e cold / warm | part 1 final (cold / warm) |
|---|---|---|---|---|
| getOrderBook mode 2, 64 bid + 64 ask levels (worst row-heavy, capped) | 85,680 | 78.0 ms cold (64 bids) / 80.0 warm (1,024 bids) | **20.6 / 18.7 ms** | 21.4-24.5 / 18.2-18.6 ms |
| getOrderBook mode 1 (reverts) | 16,900 | 161.5 ms cold (64 orders) / 233 warm (1,024) | (reverts: part 1 only) | 23.2-30.1 / 17.6-17.8 ms |
| getStakingInfo, 1,024 delegations (uncapped) | 528,480 | 216 ms warm | - / 22.0 ms | 30.8 ms (after) |
| getMarkets / getAllPrices / getValidators, 1,024 rows | 569,440 / 589,960 / 589,960 | 90.6 / 62.5 / 75.8 ms (part 1 cold) | | 24.8 / 22.3 / 26.0 ms (after) |

The 64 + 64-level block stays at ~20 ms ozarchy (target ~20-30 ms). Outliers above 30 ms are only
part-1 first calls right after a reopen (getStakingInfo N = 0: 47.8 ms; getMarkets N = 1: 80.8 ms,
one sample); a block cannot repeat a cold first call, and every e2e cold block stays at or below
34.2 ms.

### (c) Deletion markers

**What they are.** Two kinds; neither may be in a block result:

1. **RocksDB tombstones** from our own deletes, node-dependent count. Delete path of a book row:
   `NativeExecContext::save_order_books` (`crates/torus-bridge/src/native_executor.rs`: mode 2
   level rows ~5272, stop rows ~5339 / ~5430; mode 1 order rows ~4981) -> the block's
   `NativeStateOverlay` pending `deletes` -> block flush (`flush_with_native_trie_stats`) ->
   `WriteBatch` delete -> a point tombstone in the memtable -> an SST -> dropped only when a
   compaction merges it with the put below, which for a put in the bottommost level means
   rewriting that bottommost file. The scan's iterator (`scan_prefix_metered` ->
   `NativeStateOverlay::iterate_cf_prefix_from` -> RocksDB iterator with `prefix_read_options`,
   iterate upper bound at the prefix successor, the s89 fix A rule) skips them inside RocksDB:
   125-140 ns each.
2. **Overlay deletes** of the current block (and of the parent block's frozen set on the pipelined
   path): keys deleted in a layer while RocksDB still holds them; `merge_from` drops them in Rust:
   255-280 ns each. Same keys on every node, but whether a parent delete shows up here or as a
   tombstone depends on flush timing. Bounded by the deletes of one or two blocks.

Gas and answer never depend on them (`tombstones_change_neither_gas_nor_answer`: the same book
with 200 deleted better levels in an SST vs none: identical gas and bytes).

**Node-local fix** (`StateDb::note_scanned_deletes`, `compact_range_in_background`, `db.rs`; trigger
in `backend.rs` after the durable batch write):

* Every block flush groups its deletes in `cf_native_order_books` (main set + deferred-book
  sidecar) by market (8-byte prefix, `cf::READER_SCANNED_CFS`) and adds them to a per-market count
  kept across flushes (in memory, node-local). A market whose count reaches
  **T = 64** (`cf::SCANNED_DELETES_COMPACTION_THRESHOLD`) gets the span of its uncompacted deletes
  compacted and its count reset. One range per market, never a span across markets.
* `CompactOptions`: `BottommostLevelCompaction::ForceOptimized` (was `Force`): it still rewrites a
  bottommost file that got there by a trivial move (the s89 test
  `failed_compaction_does_not_affect_execution` fails with the default setting, 50 tombstones left,
  and passes with ForceOptimized), and skips files the same compaction just wrote.
* One long-lived worker thread per DB, parked on a condvar (was: a new thread per request); one
  batch of ranges at a time; requests during a run join the next batch. The stop flag is checked
  between ranges; the last owner's drop calls `cancel_all_background_work(false)` if a run is in
  progress and nothing else holds the DB (s100: not while the consensus kv store shares it), then
  joins the worker. A run fails (logged, counted) on a panic or when RocksDB's
  `rocksdb.background-errors` count grows during it (s100; `compact_range` returns no status).
  The s89 oracle prune uses the same job (range `sub..`).
* Not added: `add_compact_on_deletion_collector_factory`. It only marks SST files for RocksDB's own
  compaction; the churned tombstones live in the 128 MiB memtable, and in the no-compaction
  baseline below RocksDB ran no compaction at all, so it would not have changed these numbers.

**Why T = 64.** Up to T - 1 tombstones linger in a market. Measured on the cheapest scan a market
with markers can get (an empty-ish getOrderBook, 17,100 gas): 64 RocksDB markers -> **25.8-27.7
ms** per 30M-gas block (e2e 27.7 ms; clean: 10.9 ms); 256 markers -> 67.9-69.2 ms. T = 64 keeps
the lingering worst case at the ~30 ms scan target; 256 would not.

Single scans over N markers (one live bid level, final build, warm, part 1):

| N, state | getOrderBook | 30M-gas block |
|---|---|---|
| 64 / 256 / 1,024, deleted by a block flush, then the background compaction (6-24 ms per run) | 6.2 us, 0 skipped | 10.9 ms |
| 64 / 256 / 1,024, not yet compacted (memtable or L0) | 14.7-15.1 / 38.7-39.5 / 138.5-140.5 us | 26-27 / 68-69 / 243-247 ms |
| 64 / 256 / 1,024, overlay deletes (current / parent block) | 21.6 / 71.8 / 261.8 us | 38 / 126 / 459 ms |

**Single-market churn** (one trader at the open-order limit: every block cancels its 1,000 bids
and places 1,000 new, worse ones, real executor, mode 2; blocks flushed every 100 ms; one
getOrderBook after each flush; 150 blocks):

| block | no background compaction (`review-nocompact`) | final (`review-after`) |
|---|---|---|
| 1 | 1,000 markers, 58 ms | 1,000, 137 ms (first block, cold) |
| 10 | 10,000, 481 ms | 1,000, 90 ms |
| 100 | 100,000, 4,596 ms | 1,000, 90 ms |
| 149 | **149,000, 6,590 ms** (growing) | **1,000, 92 ms** (flat; 149 runs for 150 blocks) |

The final build keeps exactly the previous block's deletes (the measurement runs right after the
flush, before that block's compaction finishes): the accepted ~90-100 ms flat. The earlier
76eb081c build gave the same flat line on mode-1 rows (101 ms median).

**Multi-market churn** (50 markets spread among 4,000 filler markets, book CF 383 MB on disk,
incompressible filler pushed to the bottommost level; each churn market deletes and re-writes 100,
then 10, bid levels per block, written through the block overlay; 100 blocks of 100 ms; compaction
work = RocksDB compaction tickers per block, automatic + background):

| per market per block | build | compaction per block (read / write / CPU) | runs | markers in a churn market (peak / at block 100) | getOrderBook block (median / max) |
|---|---|---|---|---|---|
| 100 | no background compaction | 0 / 0 / 0 | 0 | 10,000 / 10,000 (growing) | 382 / 734 ms |
| 100 | 76eb081c: one span per CF, Force | 15.5 / 15.5 MB / 97 ms | 3-4 | 7,400 / 5,000 | 293 / 452 ms |
| 100 | final: per market, T = 64, ForceOptimized | 15.4 / 15.3 MB / 97 ms | 5 | 5,000 / 5,000 | 197 / 368 ms |
| 10 | no background compaction | 0 / 0 / 0 | 0 | 1,000 / 1,000 (growing) | 99 / 183 ms |
| 10 | 76eb081c | 15.7 / 15.7 MB / 98 ms | 4 | 730 / 510 | 85 / 184 ms |
| 10 | final | 14.6 / 14.6 MB / 92 ms | 5 | 500 / 440 | 61 / 146 ms |

The per-market change does **not** reduce the compaction work here. Removing a tombstone whose put
is in the bottommost level means rewriting that bottommost file, and 50 markets spread over a
383 MB CF touch every bottommost file, so each run rewrites about the whole CF (~380 MB, 2-2.5 s of
one core; the last run's tail wrote 382 MB). The one job in flight keeps it at one background core
and the runs back to back. What the final build changes: no span covers markets without deletes,
a market below 64 deletes is never compacted, and the markers stay lower (5,000 vs 7,400 peak).
With few churning markets the per-market ranges rewrite only their files.

**What remains (node-local time only; results never change):** the overlay deletes of the
current / parent block (255-280 ns each), the last flushed block's deletes until its run ends, up
to 63 per market below the threshold (~27 ms per block worst), and, when many markets churn
heavily, the deletes made during one run (~5,000 per market in the 50 x 100 case, ~200-370 ms per
30M-gas block). Per block, deletes in one market are bounded only by block content (one trader:
open-order limit 1,000, up to 5,000 with volume; many traders: the proposer-local order cap,
`NATIVE_ORDERS_PER_BLOCK_CAP` = 200,000, not validated).

### Re-pinned EVM-visible fixtures

* `crates/torus-evm/tests/evm_tests.rs` (gas changed): `precompile_charges_correct_gas` (lower bound
  21,000 + 16,480), `reader_precompile_gas_scales_with_returned_words` (200 x 500 + 400 x 20),
  `tight_stipend_reader_calls_run_out_of_gas` (stipends), `reader_stipend_exact_boundary`
  (32,800 -> 124,480).
* `crates/torus-bridge/tests/book_read_modes_tests.rs`: `precompile_get_order_book_serves_row_modes`
  now covers modes 2 and 3; new `precompile_get_order_book_reverts_on_mode1` (mode 1 removed).
* `crates/torus-core/tests/precompile_tests.rs`: `order_book_reader_get_open_orders` replaced by
  `order_book_reader_get_open_orders_is_an_unknown_selector`.
* `crates/torus-bridge/tests/precompile_work_bound_tests.rs`: gas-denominated budgets; pins the
  base, the split, the 64 / 65 levels-per-side boundary with its exact out-of-gas boundary
  (85,680), the mode-1 revert (16,900, out of gas at 499), wrong-length level keys, tombstone
  invariance and the compaction.

### Open for the owner

Both decided in s100: see "Decisions (owner s100)" at the end.

1. **Heavy multi-market churn costs one background core** (above): the per-market threshold cannot
   avoid rewriting the bottommost files that hold the tombstones. Options: accept until scan reads
   are removed; cap the job's duty cycle (less CPU, more lingering markers); a smaller SST target
   for `cf_native_order_books` (cheaper per-market runs when few markets churn); or serve
   getOrderBook from the executor's in-memory book (no markers at all).
2. **Corrupt books not detected by the EVM reader** (mixed level + order rows, rows without a meta
   row): detecting them costs one extra read per call. The RPC readers still reject them.

## Write stalls and the book CF's SST target (ozarchy, 2026-10-07, `bench/read-gas-stall`)

Questions: does the per-market tombstone compaction cause write stalls (18c review finding 2: each
per-market `compact_range` forces a memtable flush of the book CF), and which `target_file_size_base`
should `cf_native_order_books` get (owner s100: accept one background core plus a smaller SST target
now; getOrderBook from memory later).

### Method

* Bench: parts 4-5 of `ubench_read_precompile_gas` (`UB_RG_ONLY=churn`). Part 5 = 50 churn markets
  spread among 4,000 filler markets, book CF 383 MB on disk (incompressible filler, bottommost level),
  each churn market deletes and re-writes 100 bid levels per block through the block overlay and its
  flush, **200 blocks** of 100 ms. New in part 5: every churn market also holds 64 static ask levels,
  so the measured getOrderBook answers the s99 cap on both sides (64 + 64 levels, 85,680 gas); the
  first call after each flush ("first": new SST files not in the block cache, OS page cache warm;
  a page-cache-cold read was not feasible without root) and the median of the next 5 ("warm").
  Part 4 = single-market churn (1,000 cancels + 1,000 places per block, real executor, 150 blocks).
* New per-block counters (parts 4 and 5; `TORUS_ROCKSDB_STATS=2`): `STALL_MICROS` ticker delta,
  write-stall histogram count, `rocksdb.is-write-stopped`, `rocksdb.actual-delayed-write-rate`,
  flushes (flush histogram count), the book CF's `num-files-at-level0` and total SST files, the
  block's own overlay flush (write) time; at the end the book CF's `rocksdb.cfstats` stall lines
  ("Write Stall (count)" per cause, "Cumulative stall") and the level table.
* SST target: new node-local knob `TORUS_BOOK_CF_TARGET_FILE_MB` (`db.rs`
  `book_cf_target_file_bytes`), read at open, applied to `cf_native_order_books` only. Unset meant
  RocksDB's 64 MiB at bench time; 4 MiB is the default since s100, so set
  `TORUS_BOOK_CF_TARGET_FILE_MB=64` to reproduce the 64 MiB baseline rows below (tables A and B; test
  `book_cf_target_file_size_is_4mib_by_default_and_book_cf_only` reads the OPTIONS file).
  `target_file_size_multiplier` stays 1 (all levels), `level_compaction_dynamic_level_bytes` on.
* Variants: `nocompact` (the trigger `note_scanned_deletes` disabled in `backend.rs`, temporary patch,
  RocksDB's own compaction only), `compact` = current per-market compaction (T = 64,
  ForceOptimized, one worker) with SST target 64 (current) / 16 / 8 / 4 / 2 / 1 MiB.
  3 reps each, variants interleaved per rep, each run started at 1-min load < 1.5.
  v1 (64/16/8/4 MiB + nocompact) and v2 (64/4/2/1 MiB + nocompact, part-4 counters added): the
  64 and 4 MiB cells agree between v1 and v2 within 3 %. Medians of 3 reps below; rep spread was
  under 5 % except the max columns.
* Results: `~/bench-results-matched/read-gas-stall/` (v2 at the top, v1 in `v1/`, `summary-v2.txt`,
  `v1/summary-v1.txt`; binaries and the nocompact patch in `compact/`, `nocompact/`, `bin-v1/`).

### A. Write stalls: none

| cell | variant | stall time | write-stall events | blocks with write stopped / delayed | flushes (max per block) | book CF L0 files (max) | block flush (write) median / max |
|---|---|---|---|---|---|---|---|
| 50 markets x 100, 200 blocks | nocompact | 0 us | 0 | 0 / 0 | 1 (1) | 1 | 5.0 / 6.0 ms |
| 50 markets x 100, 200 blocks | compact, 64 MiB (current) | 0 us | 0 | 0 / 0 | 9 (1) | 0 | 4.6 / 6.0 ms |
| 50 markets x 100, 200 blocks | compact, 4 MiB | 0 us | 0 | 0 / 0 | 13 (1) | 1 | 4.5 / 7.9 ms |
| 50 markets x 100, 200 blocks | compact, 1 MiB | 0 us | 0 | 0 / 0 | 35 (1) | 1 | 4.4 / 7.6 ms |
| 1 market x 1,000, 150 blocks | nocompact | 0 us | 0 | 0 / 0 | 0 | 0 | 1.9 / 2.3 ms |
| 1 market x 1,000, 150 blocks | compact, 64 / 4 / 1 MiB | 0 us | 0 | 0 / 0 | 148 (1) | 0 | 1.6 / 2.0-2.1 ms |

The book CF's `rocksdb.cfstats` agrees in every run: "Write Stall (count)" 0 for every cause
(L0 file count, memtable limit, pending compaction bytes, write buffer manager), "Cumulative stall:
00:00:0.000".

Why: the forced flushes are real (part 4: 148 flushes in 150 blocks, 18c's "149 L0 files") but they
do not pile up. Each forced flush writes one small L0 file, and the `compact_range` that forced it
compacts that market's span out of L0 into the bottommost level, so the book CF never held more
than 1 L0 file when sampled after a block (RocksDB slows writes at 20, stops at 36). At most one
flush per block, and no stall cause (L0 count, memtable count, pending compaction bytes, write
buffer manager) ever fired. The block's own write (overlay flush) time does not move: 4.4-5.0 ms
median in part 5 and 1.6-1.9 ms in part 4, with or without the compaction. Stalls are **not a
problem** at this load; what the compaction costs is the background CPU below.

Not measured: the same churn on a node also writing full blocks (the 128 MiB memtables and L0
triggers shared with heavy EVM / native writes); a forced flush there flushes whatever the book CF
memtable holds at that moment, which is still one CF and one file.

### B. SST target for `cf_native_order_books`

50-market churn cell (100 levels per market per block, 200 blocks of 100 ms, 383 MB book CF).
Compaction = all RocksDB compaction in the interval (automatic + background), per block; one core =
100 ms CPU per 100 ms block. Markers = deletes one getOrderBook on a churn market walks (peak, and
the median over blocks 101-200 = steady). getOrderBook = one 30M-gas block of that call (64 + 64
levels).

| SST target | files (end) | compaction per block read / write / CPU | background runs (cfstats Comp(cnt), whole run) | markers peak / steady | getOrderBook block, warm median / max | first call after the flush, median / max | tail after the last block |
|---|---|---|---|---|---|---|---|
| no compaction (64 MiB) | 7 | 0 / 0 / 0 | 0 (7) | 20,000 / 15,050 (growing) | 449 / 776 ms | 457 / 827 ms | 0 |
| 64 MiB (current) | 6 | 15.5 / 15.4 MB / 97 ms | 10 (26) | 5,000 / 3,750 | 131 / 234 ms | 146 / 300 ms | 4.7 s, 725 MB |
| 16 MiB (v1) | 23 | 15.1 / 15.1 MB / 97 ms | 9 (24) | 5,100 / 3,800 | 142 / 211 ms | 159 / 293 ms | 2.9 s, 435 MB |
| 8 MiB (v1) | 46 | 14.7 / 14.7 MB / 96 ms | 9 (44) | 4,100 / 2,800 | 117 / 220 ms | 134 / 281 ms | 3.2 s, 480 MB |
| **4 MiB** | 92 | 13.1 / 13.1 MB / 88 ms | 14 (612) | 1,800 / 1,000 | **62 / 115 ms** | 78 / 171 ms | 2.5 s, 343 MB |
| 2 MiB | 183 | 11.3 / 11.3 MB / 80 ms | 23 (1,158-1,210) | 1,100 / 600 | 47 / 111 ms | 63 / 135 ms | 1.2 s, 143 MB |
| 1 MiB | 364 | 9.2 / 9.2 MB / 69 ms | 36 (1,894) | 700 / 400 | 40 / 86 ms | 54 / 149 ms | 0.8 s, 75 MB |

Single-market churn (part 4, 1,000 cancels + places per block) at every target: flat 1,000 markers
(exactly the previous block's deletes), last-20-block median 89-92 ms per 30M-gas block (64 MiB:
92.0, 4 MiB: 89.2, 2 MiB: 89.5, 1 MiB: 91.0; no compaction: 149,000 markers, 6.3 s). No regression.

Reading the table:

* The worker is CPU-bound (one core, ~97 % at 64 MiB): the bytes it rewrites per block barely
  change with the target, because it always runs back to back. What a smaller target changes is how
  much each market's run must rewrite (the bottommost file(s) holding that market's 100-level span:
  one 64 MiB file vs one 4 MiB file), so a full pass over the 50 markets finishes sooner and the
  markers it leaves behind drop: steady 3,750 -> 1,000 (4 MiB) -> 400 (1 MiB).
* 16 MiB does nothing here (markers and CPU as 64 MiB); 8 MiB little. The step is at 4 MiB:
  read time per 30M-gas block halves (131 -> 62 ms median, 234 -> 115 ms max), CPU -9 %.
  2 and 1 MiB add less per halving (62 -> 47 -> 40 ms) and start to free CPU (80 / 69 ms of 100),
  for 2x and 4x the files of 4 MiB.
* Stalls: none at any target (table A).

### Recommendation: `target_file_size_base` = 4 MiB for `cf_native_order_books`

* 4 MiB takes most of the measured gain: steady markers -73 % (3,750 -> 1,000), the 30M-gas
  getOrderBook block 131 -> 62 ms median and 234 -> 115 ms worst, compaction CPU 97 -> 88 ms per
  100 ms block, single-market churn unchanged, no stalls. What remains in this cell is ~1,000
  markers per churn market (about 10 blocks of its deletes).
* Why not 1-2 MiB, although they measure better: the file count. The DB opens with
  `max_open_files = -1` (every SST file stays open; the s470 status list has fd exhaustion as a
  known validator-death mode, finding 14, and the code sets no fd limit). At 4 MiB the book CF
  needs ~250 open files per GB of book data (92 for the 383 MB CF), at 1 MiB ~950 per GB (364):
  a 1 GB book plus the other CFs then passes a 1,024 `nofile` soft limit (systemd's default for
  services). Move to 2 MiB only together with a raised `LimitNOFILE` (or a bounded
  `max_open_files`); 2 MiB then gives 47 ms / 600 markers / 80 ms CPU.
* Companion options: none needed. L0 never exceeded 1 file in the book CF, so the L0 slowdown / stop
  triggers (RocksDB 20 / 36) are not involved; `max_bytes_for_level_base` sizes levels, not files,
  and with `level_compaction_dynamic_level_bytes` the bottommost level holds the data either way;
  `target_file_size_multiplier` stays 1 (the same 4 MiB at every level). The memtable (128 MiB) is
  unchanged: the forced flushes already make it small and often while markets churn.
* How to ship: **done** (s100, `fix/read-gas-followup`): the knob's default is 4 MiB for
  `cf_native_order_books` only. Node-local, no format or consensus impact, results unchanged.

**A node with a large, quiet book CF** (many resting orders, little churn): no deletes, so the
compaction worker never runs and costs nothing at either target. What changes is the file layout:
16x as many, 16x smaller files in the book CF (~250 per GB instead of ~16), each kept open
(`max_open_files = -1`) with its index and filter blocks in the shared 256 MiB block cache (about
the same total bytes, split across more files) plus per-file table-reader metadata (not measured).
A read binary-searches more files of the level (a few more steps, log2 of the file count), not
more I/O; not measured on a quiet CF here. Existing 64 MiB files are
not rewritten on the switch: the new target applies to files that compactions write after it, so a
quiet CF moves over gradually and a churning one within its first passes. When only a few markets
churn in a large CF, each of their runs rewrites ~4 MiB instead of ~64 MiB (16x less compaction I/O
and CPU per run).

## Decisions (owner s100, 2026-10-07)

Built on `fix/read-gas-followup` (ozarchy).

* **Tombstone bound re-accepted for testnet.** The previous block's deletes stay uncharged; they
  are bounded per block only by block content (one trader: the open-order limit; many traders:
  `NATIVE_ORDERS_PER_BLOCK_CAP` = 200,000, not validated), so a Sybil cancel wave can make the next
  block's getOrderBook walk that many markers until the background compaction removes them.
* **Before mainnet: getOrderBook answers from the in-memory book.** Requirements: byte-identical
  to today's answer (and the same on every validator), the top 64 levels per side in modes 2 / 3,
  including the block's own changes up to the call, rebuilt after a restart and after crash
  replay, and no fallback to the row scan. That removes the deletion markers from the read path
  entirely. Fallback if it slips: a per-market cap on level deletes per block, as a new block
  validity rule.
* **Book CF SST target 4 MiB by default** (`target_file_size_base` of `cf_native_order_books` only;
  `db.rs` `book_cf_target_file_bytes`, test
  `book_cf_target_file_size_is_4mib_by_default_and_book_cf_only`). `TORUS_BOOK_CF_TARGET_FILE_MB`
  still overrides. 1-2 MiB only together with a raised `LimitNOFILE` or a bounded
  `max_open_files` (the DB keeps every SST open, `max_open_files` = -1). Node-local, no format or
  consensus impact.
* **No integrity check in the EVM reader; the layout is enforced at write time.** Already the case,
  no new check: each process runs one mode (`TORUS_BOOK_ROWS`, read once), the mode-1 saver
  writes only order rows (its level journal is discarded), the mode-2 / 3 savers write level rows
  to the root CF and order rows to the node-local `cf_book_order_rows`, every save writes the
  meta row of each book it writes (when missing or moved) and no path deletes a meta row, and a
  DB whose content or `__book_mode__` marker belongs to another mode fail-stops at load before
  any block executes. Pinned by `book_read_modes_tests::row_mode_writers_keep_one_row_kind_and_a_meta_row_per_market`
  (modes 1-3, serial and deferred save, reload, a market emptied by a fill). The RPC readers
  still reject both corruptions.
