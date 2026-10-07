# Read precompile gas per unit (sizing the 50-gas placeholder)

> Superseded by the s99 owner decisions: see the last section, "s99 owner decisions (a)-(c)".

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

## s99 owner decisions (a)-(c): implemented and measured (ozarchy, 2026-10-07)

Owner decisions (18c s99, item6 plan 9.16, consensus change, fresh genesis, all "revisit later"):
(a) single reads at Hyperliquid level, ~16,500 gas per position read; (b) scan reads
(getOrderBook, getOpenOrders) kept for now at 500 gas per scanned row + 20 per returned word or
32 B blob chunk, at most 64 orders per call; (c) are the deletion markers getOpenOrders steps over
node-local RocksDB tombstones? No charge and no result-changing cap; a node-local compaction
(s89 fix B pattern) is allowed.

Branch `bench/read-precompile-gas`. Bench: the same `ubench_read_precompile_gas.rs` source
(c137f816; only rustfmt since) built twice, `before` = origin/main pricing (2,600 + 50 per unit), `after` = this
change; release, frame pointers, mold, line-tables-only; 3 reps each started at 1-min load < 1.5
(`bench-read-gas-before` / `bench-read-gas-after` units). Results, per-rep logs and `summary.txt`
(median of 3): `~/bench-results-matched/read-precompile-gas-ac/{before,after}/`. Spread between reps
under 5 % except where noted. "Block" = a 30M-gas block of one reader; "e2e cold" = a contract
STATICCALLs the reader in a loop through `EvmExecutor::execute_tx`, each call on a different
existing target (2,000 positions / balances / stakers / prices, 700 traders or markets with 64
orders), first tx after a flush + compact + reopen (block cache cold, page cache warm);
"part 1 cold" = `execute_precompile_metered` on the first call of each of 16-64 targets after the
reopen (also pays the first loads of index / filter blocks), scaled to 30M gas.

### Pricing as implemented (`crates/torus-core/src/precompiles.rs`)

| constant | value |
|---|---|
| `GAS_PRECOMPILE_READ` (base of every reader call 0x0800-0x0803) | 16,400 |
| `GAS_PRECOMPILE_READ_PER_ROW` (hashed row a prefix scan returned) | 500 |
| `GAS_PRECOMPILE_READ_PER_WORD` (32 B word returned, or 32 B of a classic blob) | 20 |
| `READER_MAX_ORDERS` (getOpenOrders / getOrderBook rows per call) | 64 |

`ReadMeter` now counts gas above the base (`reader_gas(used) = base + used`,
`reader_budget(gas_limit) = gas_limit - base`); point reads stay in the base; deletion markers are
never charged. One base for all readers, scans included: with it both scan targets are met (below),
so no separate scan base is needed.

### (a) Exact base

Target: worst cold single-read 30M-gas block ~35 ms = 1.167 ns per gas. getPosition returns 5 words
(5 x 20 = 100 gas), so base = G - 100 where G = t_cold x 30M / 35 ms.

| cold getPosition measure (before build, median of 3) | t per call | G for 35 ms | base |
|---|---|---|---|
| part 1 cold, first call per position (the 18-20 us of the s26 doc) | 20.17 us | 17,290 | 17,190 |
| part 1 cold, mean of getPosition 20.17 / getBalances 18.44 us | 19.31 us | 16,550 | 16,450 |
| e2e cold block, 2,000 distinct positions (34.98 ms / 2,000) | 17.49 us | 14,990 | 14,890 |

Chosen: **base 16,400, getPosition 16,500** (the owner's figure; the middle row rounded). Measured
after: e2e cold getPosition block **33.7 ms**; part 1 first-call cold 20.38 us x 30M / 16,500 =
37.1 ms. An exact 35 ms would need 17,190 (part 1 measure) or 14,890 (e2e block measure).

Every single reader at the new base (gas = 16,400 + 20 x answer words, + 500 per scanned delegation):

| reader | gas | block e2e cold, before -> after | block e2e warm, after | part 1 cold, after |
|---|---|---|---|---|
| getPosition | 16,500 | 170.7 -> **33.7 ms** | 9.1 ms | 37.1 ms |
| getBalances | 16,480 | 79.6 -> **15.2 ms** | 9.1 ms | 33.7 ms |
| getPrice | 16,460 | 27.6 -> **5.1 ms** | 5.1 ms | 3.8 ms |
| getStakingInfo (1 delegation) | 16,980 | 150.0 -> **28.0 ms** | 15.6 ms | 27.0 ms |
| getStakingInfo (0 delegations) | 16,480 | - | 13.3 ms (one staker) | 47.8 ms (first calls after the reopen) |
| SLOAD loops (reference) | 2,100 / 100 | 12.8 / 18.5 -> 12.0 / 13.0 ms | | |

### (b) Scans: split pricing and the 64-order cap

Cap semantics (deterministic, no revert, the same for metered and unmetered callers):

* **getOpenOrders**: the trader's first 64 rows of the market in key (order id) order.
* **getOrderBook, mode 2** (level rows `market ‖ 0x03 ‖ side ‖ price`): the best 32 levels per side,
  best-first (two bounded scans: bids, then asks; 64 rows at most).
* **getOrderBook, mode 1** (order rows `market ‖ 0x01 ‖ order_id`, read when the market has no level
  row): the market's first 64 resting orders in order-id order (the oldest), aggregated per price.
  The root CF keeps no price index, so past 64 resting orders this is a partial book, not the top.
* **getOrderBook, classic** (default): unchanged and uncapped; the whole blob is sized, charged
  20 per 32 B, read and decoded (a production blob still reverts, pinned by
  `classic_precompile_behaviour_is_unchanged`).
* Meta and stop rows are no longer read by the EVM reader (`book_reader::depth_from_rows`), so a
  mode-2 market with many stop orders still shows its levels. The EVM reader therefore no longer
  raises the corrupt-layout errors (rows without a meta row, mixed order + level rows); the RPC
  readers (`read_book_depth`) still do.
* Other scans (getMarkets, getAllPrices, getValidators, getStakingInfo's delegations) are not
  capped (bounded by governance / the validator set); they pay 500 per row + 20 per word.

| case (worst pattern per reader) | gas after | block before | block after: e2e cold / warm | part 1 cold after |
|---|---|---|---|---|
| row-heavy: getOrderBook mode 1, 64 orders at one price (1-level answer) | 48,600 | 161.5 ms cold (64) / 233 ms warm (1,024) | **23.4 / 21.1 ms** | 25.9 ms |
| word-heavy: getOpenOrders, 64 orders (4 words per row) | 53,680 | 71.1 ms cold (64) / 48.5 ms warm (1,024) | **23.6 / 15.7 ms** | 28.6 ms |
| getOrderBook mode 2, 32 bid + 32 ask levels | 51,120 | 78.0 ms cold (64 bids) / 80.0 warm (1,024) | **20.2 / 18.8 ms** | 21.9 ms |
| getStakingInfo, 1,024 delegations (uncapped) | 528,480 | 216 ms warm | - / 22.3 ms | 30.8 ms |
| getMarkets / getAllPrices / getValidators, 1,024 rows | 569,440 / 589,960 / 589,960 | 90.6 / 62.5 / 75.8 ms (part 1 cold) | | 24.8 / 22.3 / 26.0 ms |
| empty scans (N = 0): getOpenOrders / getOrderBook (row modes, 3 empty lookups) | 16,560 | 147.9 / 33.8 ms (part 1 cold) | | 27.2 / 14.8 ms |

Both scan targets (~30 ms) hold with the single base: row-heavy and word-heavy worst blocks are
20-26 ms ozarchy (cold), 21 / 16 ms warm. Outliers above 30 ms are only part-1 first calls right
after a reopen: getStakingInfo N = 0 / 64 (47.8 / 41.8 ms) and getMarkets N = 1 (80.8 ms, a single
sample: the first call on a freshly opened DB); a block cannot repeat a cold first call, and the
e2e cold blocks over distinct targets stay below 34 ms.

### (c) Deletion markers: answer, fix and what remains

**Answer.** The markers a scan steps over are of two kinds, and neither is (or may be) in a block
result:

1. **RocksDB tombstones** from our own deletes: node-dependent count. Delete path of an order row:
   `NativeExecContext::save_order_books` (`crates/torus-bridge/src/native_executor.rs`: mode 1
   order rows ~4981, `book.take_row_ops()` `None` -> `delete_cf_raw(CF_NATIVE_ORDER_BOOKS,
   book_order_key(..))`; mode 2 level rows ~5272, stop rows ~5339 / ~5430) -> the block's
   `NativeStateOverlay` pending `deletes` (`backend.rs` `CfPending`) -> block flush
   (`flush_with_native_trie_stats`) -> `WriteBatch` delete -> a point tombstone in the memtable ->
   an L0 SST -> dropped only when a compaction carries it to the bottommost level or merges it with
   the put below. How many are still there depends on each node's flush / compaction history
   (and a state-synced node has none). The scan's iterator (`scan_prefix_metered` ->
   `NativeStateOverlay::iterate_cf_prefix_from` -> RocksDB iterator with
   `prefix_read_options(prefix)`) skips them inside RocksDB (`internal_delete_skipped_count`):
   125-140 ns each, measured.
2. **Overlay deletes** of the current block (and the parent block's frozen set on the pipelined
   path): keys deleted in a layer while RocksDB still holds them live; `merge_from` reads them from
   the DB iterator and drops them in Rust: 260-277 ns each. Same keys on every node, but whether a
   parent delete shows up here or as a RocksDB tombstone depends on flush timing, so this cost is
   not chargeable either. Bounded by the deletes of one or two blocks.

Upper bound: yes, the scan already has one. `StateDb` and `NativeStateOverlay` iterate with an
iterate upper bound at the prefix successor (`backend.rs` `prefix_read_options`, the same rule as
s89 fix A `db.rs` `prefix_read_opts` / `prefix_iter`), so only markers INSIDE the scanned prefix cost.

Correction to the premise: **`cf_native_orders` (what getOpenOrders scans) has no production
writer or deleter.** The only writer is `precompiles::write_stored_order`, test scaffolding. On a
live chain getOpenOrders always answers empty arrays and its range has no tombstones; the open
orders live in the books (classic blob, mode 1 order rows, mode 2 node-local `cf_book_order_rows`).
The production tombstone exposure is getOrderBook in modes 1 / 2. Owner decision below.

Gas and answer do not depend on markers: a reader charges only the hashed rows it returned
(`tombstones_change_neither_gas_nor_answer`: same logical state with 500 / 200 tombstones in the
range vs none, identical gas and bytes, for getOpenOrders and for getOrderBook mode 1 after real
place + cancel churn).

**Fix (node-local, result-invariant, s89 fix B pattern).** `StateDb::compact_range_in_background`
(`crates/torus-state/src/db.rs`) generalizes the s89 job: one compaction in flight per DB, requests
while it runs coalesce into the next run (one range per CF, widened), `CompactOptions` with
`BottommostLevelCompaction::Force` and non-exclusive manual compaction, `Weak<DB>`, failures logged
+ counted, the last owner joins the worker. The block flush (`backend.rs`, after the durable
`write`) requests it for every CF of `cf::READER_SCANNED_ORDER_CFS` = [`cf_native_order_books`,
`cf_native_orders`] in which the batch (main set + deferred-book sidecar) deletes rows, over
`[first deleted key, last deleted key]`. The s89 oracle prune now calls the same job. A manual
CompactRange flushes the overlapping memtable first, so memtable tombstones go too. Tests:
`torus-state/tests/scanned_range_compaction.rs` (red with the trigger disabled, green with it),
`a_block_flush_that_deletes_order_rows_compacts_them_away` (torus-bridge).

Single scans over N markers (one live row, warm, part 1; `skipped` = RocksDB markers walked):

| N, state | getOpenOrders before -> after | getOrderBook mode 1 before -> after | block after |
|---|---|---|---|
| 1,024, flushed by a block flush, then the background compaction (after: 29-72 ms per run) | 136 us, 1,024 skipped -> **4 us, 0 skipped** | 143 us -> **9 us, 0** | 1,256 -> **7.1 ms** / 1,336 -> **15.3 ms** |
| 16,384, same | 2.07 ms -> **4 us, 0** | 2.14 ms -> **9 us, 0** | 19.1 s -> **7.1 ms** / 20.1 s -> **15.3 ms** |
| 1,024, tombstones not yet compacted (memtable / L0 SST) | 134-146 us | 139-144 us | 234-247 ms after (1,250-1,350 before: the base cut the calls per block 5.5x) |
| 16,384, not yet compacted | 2.05-2.24 ms | 2.06-2.20 ms | 3.6-3.7 s after |
| 1,024, overlay deletes (current / parent block) | 266 us | 268-275 us | 465-483 ms after |
| 16,384, overlay deletes | 4.5 ms | 4.5 ms | 7.9-8.0 s after |
| e2e block over 1,024 L0 tombstones, before -> after | 1,290 -> 242 ms | 1,296 -> 248 ms | |

**Worst case asked by 18c: one trader churning every block.** The trader keeps 1,000 orders open
(`OPEN_ORDER_BASE_LIMIT`) and every block cancels all of them and places 1,000 new ones; blocks are
flushed every 100 ms like the node; after each flush one reader call on that trader + market.
getOpenOrders variant: rows put / deleted straight in `cf_native_orders` (as asked; production never
writes them); getOrderBook-rows1 variant: the real executor (CancelAllOrders + PlaceOrder, mode 1
order rows). 150 blocks, median of 3 reps:

| block | getOpenOrders before: skipped, 30M block | after | getOrderBook mode 1 before | after |
|---|---|---|---|---|
| 0 | 0, 40.7 ms | 0, 13.7 ms | 0, 184 ms | 0, 16.9 ms |
| 1 | 1,000, 50.1 ms | 1,000, 70.7 ms | 1,000, 227 ms | 1,000, 146.5 ms |
| 10 | 10,000, 134 ms | 1,000, 89.2 ms | 10,000, 596 ms | 1,000, 102.9 ms |
| 50 | 50,000, 506 ms | 1,000, 87.3 ms | 50,000, 2,202 ms | 1,000, 107.6 ms |
| 100 | 100,000, 975 ms | 1,000, 89.4 ms | 100,000, 4,251 ms | 1,000, 101.7 ms |
| 149 | **149,000, 1,431 ms** (still growing) | **1,000, 90.0 ms** (flat) | **149,000, 6,232 ms** | **1,000, 99.8 ms** |

Before, the markers grow by one block's deletes per block and nothing (in 150 blocks; the 128 MiB
memtable never filled) drops them. After, the peak is one block's deletes (1,000): the
measurement runs right after the flush, before that block's compaction finishes; 149 runs for 150
blocks, none failed, so the compaction keeps up at 100 ms blocks. (Before, the getOpenOrders answer
is all 1,000 orders, 253,250 gas, so its per-block figure is lower than mode 1's 58,200-gas call.)

**What remains (node-local time only; results never change):** the markers of the last block or
two: the overlay deletes of the current / parent block, plus the last flushed block's tombstones
until its compaction run ends (29-72 ms per run here). Per block, deletes in one scanned range are
bounded only by block content: one trader at the open-order limit 1,000 (up to 5,000 with volume),
many traders up to the proposer-local order cap (`NATIVE_ORDERS_PER_BLOCK_CAP` = 200,000, not
validated). Cost is linear: ~90-100 ms per 30M-gas block per 1,000 markers (RocksDB, 125-140 ns
each) and ~2x that for overlay deletes (260-277 ns each); 16,384 markers = 3.6 s (RocksDB) /
7.9 s (overlay) per 30M-gas block. Each compaction run also rewrites the deleted span's files
bottommost (Zstd); with many markets churning, the span covers most of `cf_native_order_books`
(not measured at that scale).

### Re-pinned EVM-visible fixtures (gas changed)

* `crates/torus-evm/tests/evm_tests.rs`
  * `precompile_charges_correct_gas`: getBalances lower bound 21,000 + 2,600 -> 21,000 + 16,480 (base).
  * `reader_precompile_gas_scales_with_returned_words`: 200 markets cost 600 x 50 -> 200 x 500 +
    400 x 20 more than none (split pricing).
  * `tight_stipend_reader_calls_run_out_of_gas`: stipends 2,600 + 50 x 20 / 700 -> 16,400 +
    500 x 20 / 16,400 + 200 x 500 + 404 x 20 + 1,000 (base and split pricing).
  * `reader_stipend_exact_boundary`: 2,600 + 604 x 50 = 32,800 -> 16,400 + 200 x 500 + 404 x 20 =
    124,480.
* `crates/torus-bridge/tests/precompile_work_bound_tests.rs`: budgets and charges in gas instead of
  units (meter change); `get_order_book_work_is_bounded_by_the_budget` now expects the best 32 bid
  levels of a 150-level book (cap); `mode1_charges_one_row_per_order` charges the 30 order rows only
  (the meta row is no longer read); new tests pin the base (16,400 / getPosition 16,500), the split,
  the cap semantics, the tombstone invariance and the compaction.

### Open for the owner

1. **getOpenOrders reads a CF nothing writes** (`cf_native_orders`): on a live chain it always
   answers empty. Remove it (the owner leans to removing scan reads after item 7), or re-point it to
   the books (classic: whole blob; mode 1: no per-trader index in the root CF; mode 2: the orders
   are node-local, `cf_book_order_rows`, unhashed, so a scan of them cannot be charged by rows).
2. **The per-block marker bound is large** (above): the compaction removes the accumulation, not
   the last block's deletes, which one block can make arbitrarily many (proposer-local cap 200,000
   orders). Options: accept until scan reads are removed; a consensus cap on deletes per market per
   block; or serve getOrderBook from the executor's in-memory book (no markers; plumbing into the
   EVM provider).
3. **Base 16,400** gives 33.7 ms (e2e cold block) / 37.1 ms (part 1 first-call) for getPosition;
   exactly 35 ms would be 14,890 / 17,190.
4. getOrderBook mode 1 under the cap answers the oldest 64 orders, not the best prices.
