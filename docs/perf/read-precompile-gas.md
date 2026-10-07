# Read precompile gas per unit (sizing the 50-gas placeholder)

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
