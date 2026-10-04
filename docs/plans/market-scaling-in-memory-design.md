# Design: market scaling and Hyperliquid-style in-memory execution (item 6)

Status: DESIGN, read-only research. No code changed, nothing built or benched.
All 16 owner decisions are made (s84, 2026-10-03): see section 9.
Base: `main` @ 199bdc9. All `file:line` references are for that commit, except
section 1.6, section 3.6 and the parts marked "crab", which are for
`perf/s87-crab-fixes` @ `3d2dcd8`.
Owner requirements: memories item6 part 1/3, 2/3, 3/3 (s84). Hyperliquid
facts: memory 8d03fdac (s82/s83 research), marked by source below.

Update s89 (2026-10-04), crab-aware: Phase 1 is now built on top of the crab
stack (account-level cross margin F1, oracle marks, liquidation, s515 parity
fixes, perf fixes 3/2a/1, option B), not on plain main. The crab target spec
and scorecard is `docs/plans/crab-speed-target-design.md` ("crab doc" below);
its owner decisions (s88) are in section 9. Changed in this update: section 1.6
(crab hot paths), Phase 1 (R also serves ordered iteration; the margin summary
and liquidation L1 land in Phase 1), crab lines in Phases 3-5, section 3.6
(B, D, fix 2b, L3), section 4 (summary memory), section 5 (rules 8-9), D1/D4
re-check. Phases are numbered in build order everywhere: 1 -> 2 -> 3 -> 4 -> 5.

## Summary (plain language)

- Today every block reads trader positions and balances from RocksDB and
  writes every changed row back to RocksDB. At 300 markets with uniform load
  the engine spends about twice as much time per fill as at 10 markets
  (~11.8 vs ~6 µs per fill). That extra cost grows with the number of
  (trader, market) pairs a block touches, not with the number of fills.
- The biggest single item is the settle step reading positions from
  RocksDB (settle pass A, 214-252 ms per native block at 300 markets; in the
  s84 profile RocksDB code is 32% of all execution-thread CPU).
- Build order (phases are numbered in this order): **(1)** keep positions
  and balances in memory across blocks; **(2)** remove the remaining
  per-market and per-pair costs on the execution thread; **(3)** stop
  writing state every block: write it in coalesced checkpoints and replay
  committed blocks after a crash; **(4)** one record per trader, in memory
  only; **(5)** full in-memory execution. One record per trader comes fourth
  because on its own it removes no reads that Phase 1 does not already
  remove, and as a disk-format change it would rewrite large records more
  often until writes are coalesced (Phase 3).
- Crab stack (s89 update): the crab features are ~10x slower per fill than
  main because every margin and liquidation question re-reads all of a
  trader's positions from storage. Phase 1 therefore also keeps a per-trader
  margin summary in memory and serves the liquidation walk from memory (L1).
  Most of the crab speed-up lands in Phase 1; Gate 2 (>= 0.9x main matched/s,
  oracle on) is measured after it.
- Phases 1, 2 and 3 change no consensus rule, no state format and no
  running state hash. Each is its own branch in a linear stack and is A/B
  benched against main and the previous phase (no runtime flags, D16).
- Expected gains (estimates, to be confirmed by benches): Phase 1 +15-30%
  matched/s at 300 markets uniform, ~0 at 10 markets; Phase 2 another
  +7-15%; Phase 3 about -10% node CPU and much less disk write, no throughput
  change while the execution thread is the bottleneck; Phase 5 aims at the
  10-market cost per fill at any market count.
- Memory: ~2.4 KB per light user, ~21 KB per active user, ~400 B per resting
  order. 1M mixed users is ~8 GB of state. 64 GB per validator is enough; the
  real limit is the total number of resting orders, not users.
- 16 owner decisions are in the last section, each with a recommendation.

## 1. Current state, mapped

### 1.1 Per-block flow (native block, pipelined path)

Threads: consensus thread (C), execution thread E, flush worker W
(`exec_pipeline.rs:1-33`, rendezvous depth 1), trade writer, RPC.

| step | thread | reads | writes | ref |
|---|---|---|---|---|
| commit: header + body + manifest delete, one batch | C | – | `cf_block_headers`, `cf_block_bodies`, `cf_commit_manifest` | `app.rs:929-1023` |
| skip check, parent link | E | `cf_consensus_meta` marker, `cf_block_headers` | – | `app.rs:1517-1562` |
| buffered slashes (none produced since bug a) | E | staking | staking via own overlay + `commit_tx` | `app.rs:1595-1628` |
| EVM section (EVM blocks only, always serial) | E | `cf_accounts/storage/code`, hashed/trie CFs, native CFs via precompiles | batch BUILT, not written (bug c) | `app.rs:1640-1725` |
| signature verify | E (+verify pool) | `cf_sessions` via overlay | – | `app.rs:1776-1799` |
| nonce replay guard | E | `cf_native_nonces` per action | – | `app.rs:1855-1884` |
| context build | E | marker, `cf_native_markets` (2 point reads); books only on resident miss | – | `native_executor.rs:1742-1830` |
| phase 1 (cancels, transfers, ...) | E | balances direct per action | overlay | `native_executor.rs:3560-3621`, `:4925-5427` |
| phase 2 margin reserve | E (+shards) | `cf_native_balances` (BalanceCache miss) | – | `native_executor.rs:3627-3770` |
| phase 3 match | workers | resident books | books in RAM | `native_executor.rs:3843-3875` |
| phase 4 settle pass A | workers | `cf_native_positions` (PositionCache miss -> RocksDB) | plan-local | `native_executor.rs:4339-4449`, `:4657`, `position.rs:557-570` |
| settle pass B | E | `cf_native_balances` (miss) | BalanceCache | `native_executor.rs:4481-4593` |
| cache flush | E | – | positions + balances into overlay, sorted | `native_executor.rs:3937-3952`, `position.rs:601-616`, `:177-196` |
| tail: core writer, governance, fees, epoch boundary | E | staking/governance/fee/treasury CFs | overlay | `app.rs:2039-2042` |
| save books (pass 1 on E, pass 2 deferred to W) | E/W | `cf_native_order_books` prefix per dirty market (stops) | `cf_native_order_books`, `cf_book_order_rows`, `cf_native_markets` | `app.rs:2085-2103`, `native_executor.rs:2983-3027` |
| nonce puts, marker put, freeze, hand-off | E | – | `cf_native_nonces`, marker into overlay | `app.rs:2140-2179` |
| flush: running-hash digest, batch build, ONE RocksDB write with the marker and the hash | W | `cf_consensus_meta` (h(n-1)) | every pending row + marker + `h_n` (+ checkpoint every 100) | `backend.rs:1247-1490`, `running_hash.rs:333-358` |
| trade history rows | trade writer | – | `cf_native_trades`, `cf_native_user_trades` (node-local) | `app.rs:2309-2351` |
| state-hash monitor / attest | E | `cf_state_hash_votes`, META | – | `app.rs:2526-2532` |

The serial path (EVM blocks, slashes, epoch boundaries, replay) does the same
flush on E (`app.rs:2189-2221`); an EVM block's batch is the prefix of that one
write (`app.rs:2214-2220`). Empty blocks write only the marker
(`app.rs:2420-2477`).

### 1.2 What is resident today

- **Books**: `ResidentBooks` / `ResidentInner { books, next_global_order_id,
  height }` (`native_executor.rs:1160-1237`), `TORUS_RESIDENT_BOOKS=1`
  (`:1131-1139`). Guard: reused only if `holder.height + 1 == block` and the
  DB marker (read through the overlay, so the parent layer counts) equals
  `holder.height` (`:1742-1779`); any miss rebuilds from the DB. Not
  consensus-visible.
- **Positions and balances**: NOT resident. `PositionCache` and
  `BalanceCache` live for one `execute_batch` call
  (`native_executor.rs:3637`, `:3641`); every first touch per call goes
  overlay -> parent layer -> RocksDB (`backend.rs:1536-1557`).
- Node-local caches: level-hash sponge cache (256 MB default,
  `native_executor.rs:593`), native trie caches (trie maintenance is off in
  the bench, `TORUS_NATIVE_TRIE_MAINTENANCE=0`).

### 1.3 Restart and replay

- Stored per block: header+body at commit (`app.rs:929-1023`, before
  execution), then the full state change set + `META_NATIVE_APPLIED_HEIGHT` +
  `h_n` in one batch (`backend.rs:1399-1432`). The block log already exists:
  `cf_block_headers` + `cf_block_bodies`.
- Boot: `ensure_native_trie_built` (`app.rs:3557`), then
  `replay_committed` (`app.rs:3927-3984`) replays `(applied, committed]`
  serially before networking via `replay_gap` (`app.rs:1256`); a missing
  body parks a hole (FIX 1b). W is attached after replay (`app.rs:3590`).
  Books rebuild on the first block (s74: `load_books` 4.3-4.7 s at ~1.0M
  resting orders).
- Measured: s75 restart -> ready 11-55 s (DB open ~7-8 s with the 512 MiB WAL
  cap, then replay of 2-6 blocks before networking).
- Snapshots: `create_snapshot` (RocksDB checkpoint, hardlinks,
  `snapshot.rs:78-108`) and `SnapshotManager` (`snapshot.rs:224-292`) exist
  but no node code calls them; only `--restore-from-snapshot` is wired
  (`torus-node/src/main.rs:514-516`).

### 1.4 Readers outside execution that read the DB

- RPC (`torus-rpc/src/torus.rs`): `getPosition` (`:778`), `getBalances`
  (`:837`, also a prefix scan of the trader's positions), `getOpenOrders`
  (`:1518`, book rows or a scan of all books), `getOrderBook` (`:747`),
  `getOpenInterest` (`:1607`, full scan of `cf_native_positions`),
  `getMarkPrice`, trade history (`:944`, `:1463`, `:1695`, node-local CFs),
  `validate_known_markets` (`:263`), ingress session lookup (`:314`).
  `eth_call` executes against the DB, including native precompile readers
  (`eth.rs:818-918`, `precompiles.rs:425`, `:519`). `getUserLimits` exists
  only on `feat/per-user-open-order-limit` (streamed RocksDB iterators).
- Consensus thread: block validation looks up sessions in the DB
  (`app.rs:4774`); epoch-boundary validator-set plans need the durable
  applied height >= H-L (bug b design). Note: the session lookup reads
  whatever height execution has reached on that node, so its verdict depends
  on execution progress. Already fixed on `fix/vote-after-body` (b6f6aca,
  not yet on main): validate no longer checks signatures or sessions; they
  are resolved at execution (`app.rs` ~1850-1872 there), and an invalid
  action is skipped and recorded instead of rejecting the block.
- `--rpc-only` nodes run the same `TorusApp` with execution
  (`main.rs:159-162`, `:601-612`); they differ only in not voting.
- Pruner: deletes `cf_block_bodies` and `cf_receipts` below
  `current - retention` every 30 s (`pruner.rs:138-161`, `main.rs:1110-1135`);
  no guard against the applied height or a snapshot height. Trade history and
  nonce rows are never pruned.

### 1.5 Rough per-block costs (measured)

s84 4-arm, main, 300 markets uniform, cap 400, 300 s, root skip on, running
hash on (`~/bench-results-matched/s84-prof-20261002/analysis/phase_ms.txt`),
per native block (~1.9 blocks per native block, 45-65k fills, 59-84k orders,
240-400 actions):

| part | ms | notes |
|---|---|---|
| block wall (E) | 882-985 | E is the bottleneck since root skip |
| engine | 664-758 | |
| - phase 1 actions | 105-140 | includes the per-cancel scan of all books |
| - margin | 31-43 | |
| - match | 84-112 | |
| - settle | 422-481 | pass A 214-252, pass B 124-163, cache flush 65-81 |
| save books | 80-115 | |
| flush on W (overlapped) | 351-394 | |
| load books | 0.2-0.4 | resident reuse |

10-market reference (s83 scan): engine ~550 ms at 81-104k fills, pass A
93 ms, cache flush 6 ms; at 300 markets pass A 396 ms, cache flush 112 ms
for similar fills. With trader locality (10 markets per sender) 300 markets
cost only ~5% vs 10 markets (s83 MPS=10 cells); uniform fan-out is the worst
case (-46% at 300 markets).

CPU: s82 (10 markets) execution 34%, signature verify 23%, storage 25% of
node CPU. s84 flat profile (300 markets): execution threads 47% of node CPU;
RocksDB code 21% of node CPU and **32% of execution-thread samples** (E
does no RocksDB writes on the pipelined path, so this is reads); flush worker
6.9%, rocksdb:high 3.7%, rocksdb:low 3.2%, trade writer 2.3%. The dwarf
profile (biased by sample loss, direction only) puts ~87% of settle pass A in
`get_position -> StateDb::get_cf_raw`.

### 1.6 Crab stack: storage reads on the margin and liquidation paths

References for `perf/s87-crab-fixes` @ `3d2dcd8` (crates/...). Measured costs
and targets are in the crab doc, sections 2.1 and 5.

On main, iteration over positions is RPC only. On the crab stack it is on the
block path: every margin or liquidation question rebuilds the trader's
`AccountView` (`torus-core/src/margin.rs:143-169`, Cross positions only,
valued at the mark or else the entry price, `:89`) from a prefix scan of all
the trader's positions.

| path | storage access per call | where |
|---|---|---|
| `positions_for_trader` | overlay `iterate_cf` over the 20-byte trader prefix; the DB part is fully materialised into a Vec, then pending/parent layers are merged in a BTreeMap (fix 3 skips the merge when no layer touches the prefix) | `position.rs:288-299`, `backend.rs:1705-1735` |
| `AccountReader::view` / `pos_net` | `positions_for_trader` + `build` | `native_executor.rs:790-796` |
| `AccountReader::position_px` | `get_position` point read + mark | `:802-815` |
| `maker_free` / `maker_account` | balance point read + `view`; `position_px` per (maker, market) | `:821-844` |
| fix 1 `BatchMakerAccounts` | memoises `free` per maker for Phase 3 of one `execute_batch` call | `:854-880`, created `:4456` |
| fix 1 `BatchMarks` | one `OnceLock` mark per market per `execute_batch` call; not used by the single path or liquidation | `:750`, built `:4240-4246` |
| Phase-2 `prepare_one` | `pos_net` memoised per sender (prefix scan), `position_px` per (sender, market) (point read) | `:4618-4800`, `:4684-4708` |
| Phase-3 `reduce_only_positions_for` | one `signed_position` point read per (policed trader, market) | `:6311-6325` |
| withdrawals `check_withdrawal_margin` | balance read + `view` | `:7829-7856` |
| liquidation `traders_after` | one overlay seek (`iterate_cf_from`, k-way merge over pending, parent and a RocksDB iterator) per trader, scan + 2 traders per block | `liquidation.rs:418-438`, `backend.rs:1743-1806` |
| liquidation `liq_view` | fix 2a guard (no usable mark in any listed market -> skip), else `positions_for_trader` + balance + `build` | `liquidation_step.rs:149-169` |
| liquidation `backstop`, `settle_flat_deficit`, stage 1 / ADL | more `positions_for_trader` calls per acted trader | `liquidation_step.rs:120-313`, `liquidation.rs:296`, `:390` |
| ADL `adl_candidates` | pages `iterate_cf_from` over the WHOLE positions CF (up to `ADL_MAX_SCAN_ROWS = 65_536`), filters by market; `view` per candidate | `liquidation.rs:340-380` |

Per block (`app.rs:1623`, `execute_committed_block_with`):
`begin_block_oracle` (`:2218`, marks fixed for the rest of the block) ->
`execute_batch(pre_evm)` (`:2219`) -> `execute_batch(post_evm)` (`:2220`) ->
`drain_core_writer` (`:2254`) -> `run_liquidations` (`:2258`, window
`LIQ_SCAN_PER_BLOCK = 2_048` valued, `LIQ_ACT_PER_BLOCK = 64` acted,
`liquidation.rs:37-39`) -> governance. Liquidation therefore runs after both
batches: every trader the block touched has rows in the pending layer.

`PositionCache` / `BalanceCache` still live for one `execute_batch_phases` call
(`native_executor.rs:4201-4206`, flushed sorted at `:4576-4588`); the resident
holder (`ResidentInner`, `:1677-1683`) holds books only.

## 2. Hyperliquid comparison

Sources: **[O]** official (HL docs, hl-node README, Jeff Yan's tweet),
**[RE]** reverse-engineered (lastdotnet rust docs, OtterSec blog),
**[?]** unknown / not found.

- [O] consensus does not block on execution; ~200k orders/s, execution is
  the bottleneck; median end-to-end latency 0.2 s (p99 0.9 s co-located, per
  the owner's notes); `periodic_abci_states` snapshot every 10,000 blocks
  (`.rmp`, msgpack); transaction log `replica_cmds`; optional fills / trades
  / order-status files; ~100 GB of logs per day, archived or deleted by hand;
  validator 32 vCPU / 128 GB / 1 TB, non-validator 16 vCPU / 128 GB / 500 GB.
- [RE] whole state is one in-memory `Exchange` struct serialized with rmp;
  `clearinghouse.user_states: BTreeMap<addr, UserState { positions:
  BTreeMap<asset, ..> }>`; app hash = LtHash16 accumulators over execution
  responses, `VoteAppHash` every 2000 blocks; RocksDB seen for the EVM.
- [?] determinism rules for in-memory structures, EVM state placement (no
  official statement), behaviour on app-hash mismatch, how a new node gets a
  snapshot, snapshot verification, crash recovery time.

### 2.1 Gap table (owner's four rows)

| # | Hyperliquid | Torus today | this design |
|---|---|---|---|
| 1 | state in memory; disk = command log + snapshots every 10k blocks [O] | every block writes its changed state to RocksDB with the running hash (`backend.rs:1247-1490`); storage ~25% of node CPU | Phase 3: block log already exists; state written in coalesced checkpoints; replay after crash; Phase 5 keeps all native state typed in memory |
| 2 | books and execution in memory [O/RE] | books resident; positions/balances read from RocksDB per batch (pass A cold reads) | Phase 1: positions + balances resident across blocks |
| 3 | one record per user holding all positions [RE] | one row per (trader, market) (`position.rs:202-207`) | Phase 4: per-trader record in memory; logical rows and hash unchanged (decision D4) |
| 4 | work in proportion to fills | work in proportion to (trader, market) pairs touched; per-fill engine cost doubles from 10 to 300 markets | Phases 1, 2 and 5 remove per-pair reads, per-market scans and per-pair serialization |

### 2.2 The 15 points

| # | point | answer | phase |
|---|---|---|---|
| 1 | positions/balances resident | complete in-memory row layer for both CFs under the overlay, same staleness guard as books | 1 |
| 2 | one record per trader | in-memory `UserState`; on-disk/hashed rows stay per (trader, market) unless the owner picks the format change | 4 |
| 3 | work ∝ fills | remove cold reads (1), per-cancel scan of all books, per-block thread spawns, double serialization (2), overlay on the hot path (5) | 1, 2, 5 |
| 4 | log + periodic snapshots | the log is `cf_block_headers`+`cf_block_bodies` (written at commit, `app.rs:929-1023`); state goes to RocksDB in coalesced checkpoints with the marker; replay = existing `replay_committed` | 3 |
| 5 | full in-memory execution | typed state for all native CFs, overlay off the hot path | 5 |
| 6 | recovery time vs interval | replay time ≈ execution time since the last checkpoint; checkpoint by elapsed execution time (~30 s), not by block count (D6) | 3 |
| 7 | bootstrap from snapshot | RocksDB checkpoint taken right after a state checkpoint + existing `--restore-from-snapshot`, then block sync; full-state hash for trust (D8) | 3 |
| 8 | RPC reads | Phases 1-2: DB, unchanged; Phase 3+: a block-consistent in-memory view (section 6) | 3 |
| 9 | running state hash | already defined over the logical change set, not over DB writes (`running-state-hash-impl.md` "Storage-independent definition"); Phase 3 moves the digest and `h(n-1)` from the flush to memory | 3 |
| 10 | memory budget / hardware | section 4: 64 GB is enough for 1M users; cap resting orders | all |
| 11 | determinism | section 5 | all |
| 12 | EVM state | stays in RocksDB; EVM blocks force a state checkpoint in Phase 3 (section 7) | 3 |
| 13 | fills as append-only files | keep the packed rows (s78: history cost 28.5 -> 4.1 CPU-s/1M); add retention pruning (D13) | 3 |
| 14 | log pruning | pruner clamps its cutoff to the oldest kept snapshot; snapshot rotation; trade-history and nonce retention | 3 |
| 15 | acceptance | relative gates per phase + HL targets as the end goal (D15) | all |

## 3. Phased plan

Phases are numbered in build order. Why this order: after Phase 1, E (not W)
is still the bottleneck (E 880-985 ms vs W 351-394 ms per native block), so
the next throughput lever is E's own per-pair and per-market work (Phase 2).
Phase 3 mostly saves CPU and disk and only adds throughput once E is faster
than W. One record per trader (Phase 4) gains little alone: bench senders
touch ~200+ markets each, so a per-trader disk record (~20 KB) rewritten per
touch writes as many bytes as today's rows until Phase 3 coalesces writes.
On main the margin check reads only the balance (`native_executor.rs:3758-3770`)
and liquidation has no production caller (`run_liquidation_checks`, `:5957`).
On the crab stack both read all of a trader's positions (section 1.6); that
cost is removed in Phase 1 by the margin summary, so Phase 4 still gains
little on its own (s89 re-check, D1).

Common bench recipe (s84 4-arm): 300 markets uniform, cap 400, rate 76000,
300 s, `TORUS_NATIVE_TRIE_MAINTENANCE=0`, `STATE_HASH_ACTIVATION=1`,
`OPEN_ORDER_BUDGET=900` if the order limit is merged; interleaved arms
(Williams/ABBA), >= 4 cells per arm, same bench-throughput binary; score
matched/s and node-total CPU-s/1M (not per-thread rows, see s82); plus 10-market
cells (no regression) and the s84 low-load latency cells (rate 100,
SENDERS=50, 120 s, 2 per arm). Every phase also runs the s75 multi-crash
harness when it touches restart.

Crab cells (every phase from Phase 1 on, since the stack carries the crab
features): the oracle price feeder must be ON (`ORACLE_FEED`, `ORACLE_PRICE`,
`ORACLE_INTERVAL_MS` from `bench/s87-oracle-feed`, added to the campaign
`run_cell.py` allowlist). Without marks, fix 2a skips liquidation valuation
and hides its cost. Also record `rejected_cancelled` and the engine split
(margin, match, settle, liquidation tail) per crab doc 5.2-5.3.

### Phase 1: positions and balances resident across blocks, margin summary, liquidation L1 (size L)

Base (s89): on top of the crab branch `perf/s87-crab-fixes` (after the option B
review), not plain main. After Gate 2 the rest of the crab stack and Phase 1
are merged into main together (normal merge, crab commit ids kept, crab commits
not rebased). Phases 2-5 then stack on main as before. Size was M before the
crab update; the summary and L1 make it L.

What changes

**1a. Resident row layer R.**
- Complete resident row layer `R` for `cf_native_positions` and
  `cf_native_balances` (raw key -> value bytes; this also covers the
  `cum_volume` keys kept in the balances CF, crab `position.rs:342`, and the
  `cvlm` rows of the order-limit branch). "Complete" = built by one full scan
  of both CFs, so a miss in R means "absent", never a RocksDB read.
- **Ordered** (crab gap 1). On the crab stack iteration is on the block path
  (section 1.6), so R serves three kinds of read, all from memory:
  - point lookup: `get_cf_raw` order pending -> parent -> **R** (crab
    `backend.rs:1641-1661`);
  - prefix iteration: `iterate_cf` (`:1705-1735`, used by
    `positions_for_trader`): R's prefix range replaces the materialised DB
    Vec; the pending and parent layers are merged on top with the existing
    `overlay_into` (tombstones removed, then writes);
  - seek iteration: `iterate_cf_from` (`:1743-1806`, used by `traders_after`
    and `adl_candidates`): R's range from the start key replaces the RocksDB
    iterator in the existing k-way merge.
- Form: one sorted map per CF (`BTreeMap` over raw keys). A point lookup is
  then O(log n) (~20 key compares at 1M rows) instead of a hash probe. If that
  shows in settle pass A, add a hash index over the same entries; measure
  first.
- R travels with the resident holder (crab `ResidentInner`,
  `native_executor.rs:1677-1683`) and uses the same guard (take, marker,
  `height + 1`, `:2252-2290`); it is dropped with the books on a fatal block
  (`stash_resident`, `:2437-2450`) and on a non-successor block
  (`advance_untouched`, `:1714-1739`). Any mismatch rebuilds R.
- R is read-only while a block runs (pass-A workers read it without locks).
  E applies the block's frozen pending set to R after the freeze and before
  the next block's overlay is built: O(changed rows).
- Writers (Task 0 audit, crab @ `3d2dcd8`): every block-path write to the two
  CFs goes through the overlay: `PositionManager` (`put_position`,
  `delete_position`, `put_native_balance`, `put_cum_volume`, `apply_fill`),
  the per-call caches' sorted flush, the single path (`place_order_inner`,
  `release_order_margin`, cancel, modify), lockbox `atomic_write` (into
  pending), liquidation (`transfer`, `move_collateral`, `backstop`,
  `adl_close`, `settle_flat_deficit`). EVM precompiles write no position or
  balance rows. Outside the overlay: genesis (`torus-genesis/src/lib.rs:481`,
  before boot; R is built after) and the bench-throughput seeding context
  (offline). Task 0 repeats this audit on the actual Phase 1 base.

**1b. Margin summary** (crab doc 2.2; owner Q3, Q5).
- `position_terms(pos, mark, tiers)` is factored out of `AccountView::build`
  (inline today, crab `margin.rs:158-167`). `build` (cold path, tests) and the
  summary both call it. Own commit, no behaviour change, goldens unchanged.
- `TraderSummary` side map per trader with a position or balance row:
  `available`, `order_margin`, per Cross position its terms `(upnl, im, mm,
  notional)` and the price used, the four sums, counts, `valued_at`. Updated
  from the same changed rows that update R (O(changed rows)), so it describes
  the end of the previous block. `cum_volume` keys are ignored.
- Per-block mark table: the output of `begin_block_oracle` (crab
  `native_executor.rs:7944-7954`, once per block before any batch) is kept as a
  Vec indexed by market, plus a mark version that increments when any usable
  mark changes, appears or goes stale. It replaces fix 1's `BatchMarks` and
  the oracle reads in `AccountReader::mark`. A trader whose `valued_at` is
  older is re-valued on first read in a block: O(positions), in memory.
- Bit-exact (Q5): per-position rounding as in `build`; a trader with a term
  above a magnitude guard is routed to `build` on every read; `Err` cases
  identical.
- Freshness within a block: a trader with rows in the current pending layer
  ("dirty"; the overlay already answers whether pending touches a prefix,
  `backend.rs:405-408`) is valued with `build` over the overlay and R:
  O(its positions), in memory, never RocksDB. Clean traders are O(1).
  Liquidation runs after both batches (section 1.6), so every trader the block
  touched is dirty when the walk reaches it (bench shape ~600 senders x ~250
  positions x ~30 ns = ~4.5 ms per block). If that shows in the liquidation
  tail, apply each `execute_batch` call's sorted cache flush to a block-local
  summary layer; measure first.
- Consumers: Phase-2 `prepare_one` (`pos_net`, `position_px`), the maker
  snapshot (`AccountMargins::load` via `MakerAccountSource`: `free` from the
  summary, position from R), `reduce_only_positions_for` (R point reads),
  withdrawals (`check_withdrawal_margin`), liquidation `equity` and
  `maintenance`.

**1c. Liquidation L1** (crab doc 2.3; owner Q1, Q2).
- Same rule as today: window of 2048 valued / 64 acted, same cursor, same
  order, same `CF_NATIVE_LIQUIDATION` writes. No consensus change.
- `traders_after` reads R's ordered keys (in memory); `liq_view` reads the
  summary (clean) or `build` over overlay and R (dirty). Stage 1, backstop and
  ADL are unchanged and now read R; the ADL whole-CF page scan
  (`adl_candidates`) reads R too.
- L2 (certificates that skip provably healthy traders) is added in Phase 1
  only if the oracle-on liquidation tail exceeds 1.0 ms per 1k fills.

Deleted in Phase 1 (no second implementation may remain): fix 1
`BatchMakerAccounts` and `BatchMarks`; the storage reads in
`AccountReader::view` / `maker_account` / `maker_free` / `position_px`;
`AccountMargins::load`'s source read; the RocksDB part of `traders_after`;
the storage reads in `liq_view`; fix 2a's no-mark guard in `liq_view` if P3
stays green without it (valuation is O(1) then). Stays: `AccountView::build`
(cold path and P1 reference), fix 3 (generic read path), the ADL scan rule.

Build order inside the branch: R (with ordered iteration) -> `position_terms`
-> mark table -> summary + consumers -> L1 -> deletes; each commit keeps the
goldens and the F1/parity suites green.

Expected gain (estimate)
- Non-crab part as before: removes the RocksDB point reads from pass A,
  pass B, margin and phase-1 balance reads. Pass A at 300 markets from
  214-252 ms to ~60-90 ms; engine -150..-230 ms per native block ->
  **+15-30% matched/s at 300 markets uniform**, ~0 at 10 markets, roughly
  -8..-12% node CPU-s/1M.
- Crab part (crab doc 5.2, ms per 1k fills at 300 markets with marks, today
  after fixes 3/2a/1 -> target): margin 11.3 -> <= 1.5, match 18.0 -> <= 2.0,
  liquidation tail 25.7 -> <= 1.0. **Gate 2: >= 0.9x main matched/s on the
  devnet, oracle on**, paired alternating cells.

Risks
- Coherence: a writer that bypasses the overlay leaves R and the summary
  stale (Task 0 audit above).
- Determinism: R is iterated on the block path, so it must be ordered; no
  `HashMap` iteration may reach a decision or a write (section 5 rule 1).
- A summary that differs from `build` makes a warm node decide differently
  from a cold one: a fork, caught by the running hash only after the fact
  (section 5 rule 8; tests P1, P5).
- Crash safety: unchanged; R and the summary are caches of DB post-state,
  rebuilt cold. A failed serial flush (logged, not latched) trips the marker
  guard next block, exactly like books.
- Running hash: unchanged (the flush still digests the pending set).
- Memory: section 4; rebuild scan at the first block after boot (~1 s per 1M
  rows, estimate).

Tests (crab doc section 4, P1-P7, plus the s84 list)
- R: differential, same block sequence with and without R (a test-only
  constructor, not a runtime flag), all four BookModes, serial vs pipelined:
  identical CF dumps and `h_n` per block; R == DB scan after every flush;
  property test of point, prefix and seek reads (pending/parent/R) vs the DB.
- P1 summary == `build` after every block of long seeded sequences (open,
  increase, partial close, flip, full close; balance-only writes; marks
  fresh / stale / absent / reappearing; flat and multi-tier configs; a listing
  mid-run; negative `available`; Isolated and overflow-sized positions).
- P2 goldens A and B (`torus-bridge/tests/perf_equivalence_golden.rs`)
  unchanged, serial and engine-forced; `account_margin_tests`,
  `maker_margin_release_tests`, `market_order_margin_tests`,
  `reduce_only_tests`, `engine_parallel_tests` green.
- P3 L1 == storage-valued walk: identical liquidation rows, positions,
  balances, results, metrics and `h_n` per block (cursor cuts, cooldown,
  stage-1 chunks, backstop, ADL of a trader and of the vault, marks on/off).
- P5 warm == cold: 3 replicas, one restarted every K blocks, one with the
  summary invalidated every block: identical state and `h_n`.
- P6 crash/replay: kill between the R/summary update and W's write, and
  between W's write and the next block; restarted node identical (state +
  `h_n`); `liquidation_determinism_serial_pipelined_and_replay_are_identical`
  (`app.rs:16388`) and chaos `liquidation_step_keeps_incremental_root_equal_to_full_scan`
  (`chaos.rs:447`) green.
- P7 guard: marker mismatch, skipped height, fatal block -> R and summary
  rebuilt, never used stale.
- EVM lockbox deposit (0x0820) in block N, order using it in N+1.
- `ubench_econ` (`torus-bridge/tests/ubench_econ.rs`) with marks after each
  commit, for the per-phase split.

Bench: recipe above with crab cells (oracle on). Phase 1 branch vs main and
vs its crab base branch. Mechanism metrics: `exec_settle_pass_a_seconds`,
engine ms per native block split (margin, match, settle, liquidation tail),
`rejected_cancelled`, RSS, R rebuild count.

### Phase 2: per-block work in proportion to fills on E (size M)

What changes (each item independent, byte-identical state, node-local)
1. OrderId -> MarketId index in the resident holder for cancel and modify;
   today each cancel scans all books (`native_executor.rs:5169-5206`,
   modify `:5360`); the markets lens estimated ~18 µs per cancel at 300
   markets.
2. Persistent worker pool for match, settle, book drain (and the order-limit
   count if merged) instead of a `std::thread::scope` per block
   (`market_workers.rs:135`, `native_executor.rs:4417`, `:924`). s82 counted
   ~1,300 thread ids per node per 60 s; s84 found the order-limit count's
   fan-out costing +40 ms wall per block on a loaded host.
3. Cache flush: serialize each dirty row once (owned buffers, presized maps)
   and feed the same bytes to the overlay and to R
   (`position.rs:601-616`, `native_executor.rs:177-196`); 65-81 ms today.
4. Stops dirty flag: skip the per-dirty-market prefix scan in
   `diff_stop_rows` when a book's stops did not change
   (`native_executor.rs:2983-3027`).

Expected gain (estimate): -60..-120 ms per native block at 300 markets
(phase 1 105-140 ms, cache flush 65-81 ms, part of save books) ->
**+7-15% matched/s**. Small at 10 markets.

Crab: no rule change. The pool also runs the crab Phase-2 sender shards
(`phase2_parallel_prepare`, crab `native_executor.rs:4860`) and the
per-market settle plans.

Risks: error strings and result order of cancel/modify must not change
(they are action results); worker-panic containment (T1.5) must be kept with
a pool; pool threads must not outlive a fatal block.

Tests: differential vs the previous phase's code path in tests (CF dumps +
`h_n`), cancel of unknown /
foreign / already-filled ids, cancel-all runs, worker panic -> fail-stop as
today, 300-market fixture.

Bench: recipe above, the phase-2 branch binary vs main and vs the phase-1
binary. If one item's effect must be isolated, bench an intermediate commit
of the branch.

### Phase 3: block log + coalesced state checkpoints instead of per-block state writes (size L)

What changes
- E stops handing a per-block Flush job to W. Each block's frozen pending
  set is merged into one "since-checkpoint" layer (last value per key) that
  sits between the block overlay and the DB for every reader. A checkpoint
  writes that layer + marker + `h_S` + attest checkpoints in ONE atomic
  batch (`backend.rs:1247-1490` reused), then clears the layer.
- Checkpoint triggers (node-local, no consensus effect): elapsed execution
  time (default ~30 s, D6), layer size cap, shutdown, and forced before or at
  every EVM block (EVM reads the DB directly, `app.rs:1657`), every epoch
  boundary (consensus thread reads plans with `applied >= H-L`, bug b
  design), every slash block. These are the blocks that are serial barriers
  today (`app.rs:1580-1585`).
- Running hash: the per-block digest runs at freeze over the same layers
  (`block_digest`, `backend.rs:1205-1218`); `h(n-1)` comes from memory instead
  of META (`chain_step`, `running_hash.rs:333-358`); `h_S` and the 100-block
  attest checkpoints are persisted with each state checkpoint. Replay from S
  recomputes `h_{S+1..}` deterministically. Without this move the node goes
  hash-unverified at the first skipped write.
- Crash recovery: marker = last checkpoint S; existing `replay_committed`
  replays `(S, committed]` (`app.rs:3927-3984`). Needs the bodies of that
  range: pruner cutoff clamped to the oldest kept snapshot and to S
  (`pruner.rs:138-161`).
- Readers move to a block-consistent memory view (section 6): RPC, `eth_call`,
  ingress and consensus-thread session lookups (`torus.rs:314`,
  `app.rs:4774`), state-hash monitor (`app.rs:2526-2532`).
- Snapshot for bootstrap: optional RocksDB checkpoint (`snapshot.rs:78-108`)
  right after a state checkpoint, rotation via `SnapshotManager`
  (`snapshot.rs:224-292`), metadata adds `h_S` and block hash; restore =
  existing `--restore-from-snapshot` + block sync.
- Automated pruning: snapshots rotated; bodies kept from the oldest kept
  snapshot; trade-history retention (D13); nonce rows (D12).
- Crab: no crab-specific code. The margin summary, mark table and (if built)
  L2 certificates have no persistent form; after a crash, R and the summary
  are rebuilt cold from checkpoint S and replay recomputes them. The
  `CF_NATIVE_LIQUIDATION` rows are ordinary state in the since-checkpoint
  layer.

Expected gain (estimate)
- CPU: flush worker 6.9% + most of rocksdb:high 3.7% + part of
  rocksdb:low 3.2% -> about **-10% node CPU** at 300 markets. Disk writes
  fall by the coalescing factor (bench traders rewrite the same rows every
  block, so roughly ×blocks-per-checkpoint).
- Throughput: ~0 while E is the bottleneck; removes the W ceiling
  (351-394 ms per native block) for later phases.
- Restart: replay adds up to one checkpoint interval of execution time
  (~30 s at the default); DB open and book load as today.

Risks
- Restart -> ready grows (s75 work); bounded by the time-based trigger.
- A reader left on the raw DB sees state up to one interval old; for
  consensus-thread readers that is a liveness/correctness bug. Mitigation:
  route every reader through one view type; test with an "infinite" interval.
- One large atomic batch per checkpoint (tens of MB at 300 markets): write
  stall risk; must stay one batch (a split batch with the marker last is not
  idempotent on replay).
- Memory: the layer holds at most one copy of every row changed since S.

Tests
- Crash at random points between checkpoints (incl. mid-checkpoint write):
  restarted node identical to an uncrashed node, state and every `h_n`.
- 3 replicas with different checkpoint intervals: identical state and hash.
- Forced checkpoints: EVM block, epoch boundary with exec lag (bug b tests
  extended), slash block.
- Reader audit: the RPC and consensus test suites against a node whose
  interval never fires vs interval 1: identical answers.
- Pruner never deletes a body above the oldest kept snapshot; snapshot
  restore + replay -> identical `h_n`.
- Crab: warm == cold after checkpoint replay (P5/P6 rerun with checkpoint
  intervals > 1 block), including liquidation blocks.

Bench: recipe above (interval 30 s vs 1 block = today), plus s75 multi-crash
cells (restart -> ready vs interval), iostat write bytes, RSS.

### Phase 4: one record per trader (size M in memory; L as a format change)

What changes: typed `UserState { balance, cum_volume, open_order_count,
positions: sorted Vec<(MarketId, PositionCore)> }` keyed by address replaces
R's two raw maps; `PositionCore` drops the trader and market fields that are
already the key (`Position` 96 B -> 80 B, `position.rs:51-62`). Settle keeps
its plan/apply split: pass-A workers read `UserState` immutably, produce
per-market deltas, pass B applies them in market order. The per-(trader,
market) logical rows, the running-hash input and RPC formats stay as they are
(D4 option a); rows are produced from `UserState` at freeze.
Crab: `UserState` also holds the Phase 1 summary fields and each position's
terms next to the position, and the sorted trader set is the `UserState` map
itself (ordered by address). The Phase 1 `TraderSummary` side map and R's raw
position rows on the hot path are deleted.

Expected gain (estimate): small at today's workload (+0-10%). The
O(positions of one user) cross-margin and liquidation checks already land in
Phase 1 (summary); Phase 4 moves the summary into the record (one lookup per
trader instead of two maps) and gives O(1) per-user open-order counts (s84
order-limit profile). It is also the base for D (section 3.6).

Risks: two representations of one state (typed + rows) must agree;
determinism of iteration over a user's positions (sorted by market id).
Tests: differential vs Phase 3 (identical rows and `h_n`), random
open/close/flip sequences, P1 (summary in `UserState` == `build`) and P3
(liquidation) rerun.
Bench: recipe above, Phase 3 vs Phase 4.

### Phase 5: full in-memory execution (size L)

What changes: all native consensus state typed in memory (books done, users
from Phase 4, windowed nonce set, sessions, oracle, markets, staking and
governance maps); the executor mutates typed state directly; the
`NativeStateOverlay` leaves the hot path; the per-block change set (running
hash input and checkpoint rows) is produced from typed dirty sets, serializing
each changed record once; EVM precompiles and RPC read through a
`StateBackend` adapter over the typed state. RocksDB keeps: block log, EVM
state, trade history, DA, hotstuff metadata, checkpoints.

Expected gain (estimate): removes the overlay lock/alloc/Borsh path, the
cache flush and the freeze copy; target = the 10-market engine cost per fill
(~6 µs) at any market count, i.e. up to **+60-90% matched/s at 300 markets
uniform** vs main if E stays the bottleneck.

Crab: the executor updates the summary inline per fill (one term per fill);
the margin and liquidation paths no longer read the overlay. Target: crab
overhead <= +15% per fill vs the same phase without crab, and **Gate 3:
>= 1.4x today's main matched/s at 300 markets** after all phases.

Risks: largest change; every native code path; memory DoS via resting
orders (section 4). Tests: full differential vs the Phase 3 binary on long
randomized block sequences (state, `h_n`, action results), crash/replay,
replicas with different lag and intervals. Bench: recipe above plus a
1M-account / 1M-resting-order genesis cell for RSS and restart.

### 3.6 Crab work around the phases: F1 D2, option B, option D, fix 2b, L3

Details and proofs: crab doc sections 2.3-2.4 and 4. References are crab
(`3d2dcd8`).

**F1 D2 (today's rule).** Phase 3 of the engine matches each market on its
own worker. A sender's free margin is given whole to ONE market: the market
of its first checked taker (`d2_pool_takers`, `native_executor.rs:6242-6257`;
pools built `:4370-4377`). In its other markets a checked taker may only use
its own reservation (`insert_taker_only`, `order_book.rs:438-441`). Item 6
keeps this parallel per-market matching; it changes where state lives, not
how margin is split between markets.

**Option B (built, on the Phase 1 base).** Match-checked sells in a non-pool
market reserve at `max(limit, B0)`, `B0` = that market's best bid at the
start of Phase 2 (`phase2_bid_floors`, `:6139-6153`); `3d2dcd8` adds the
same-batch bound (`same_batch_bid_top_ups`, `:6177-6236`). Cuts the takers
cancelled for margin from ~19% to ~1.8%. Not part of any item 6 phase.

**Fix 2b (liquidation scan cost with marks) folds into Phase 1.** It was
planned as a separate health-buffer index; instead the liquidation walk is
served from R and the summary (L1, Phase 1). L2 certificates are added in
Phase 1 only if the oracle-on tail exceeds 1.0 ms per 1k fills. An interim 2b
before Phase 1 is built only if the oracle-on devnet misses the 90% bar
(owner Q7, open).

**Option D: after Phase 4 and 5, not inside item 6.** After the parallel pass,
the takers cut by margin exhaustion in a non-pool market are collected in
flat batch order and re-run one by one against the account-level free
margin (the summary's `free`, O(1)), on the books before they are saved.
Serial and in canonical order, so deterministic; HL-like (one account
budget). Cost is proportional to the cut takers (~1.8% after B). It changes
outcomes: golden A re-pinned in D's commit (test P8). It needs O(1) account
free margin (Phase 1) and users and books in memory (Phase 4/5), which makes
the second pass cheap; that is why it comes after them. Whether D later
replaces the D2 pool or only complements it is owner Q6 (open, after B is
measured).

**L3 ("every account, every block").** A separate consensus change (fresh
genesis, golden B re-pin), bundled with D12 (nonce window), before the number
of position holders approaches the 2048 window (below that the window
already covers every account every block). Owner Q1.

**Liquidation cooldown parity bug (owner, later).** During the 30 s cooldown
HL liquidates the whole position; we skip stage 1 (`liquidation_step.rs:193`).
Not part of item 6. If it is fixed before Phase 1, P3's reference walk is
built on the fixed rule; if after, P3 is rerun with the fix.

## 4. Memory budget

Struct sizes on x86_64 (i128 is 16-byte aligned; computed from the
definitions, to be pinned with `size_of` tests):

| item | layout | size |
|---|---|---|
| `FixedPoint` | `i128` | 16 B |
| `Position` (`position.rs:51-62`) | 4 FP + u64 + Address + 2 bytes | 96 B |
| `NativeBalance` (`position.rs:146-150`) | 2 FP | 32 B |
| `Order` (`order_book.rs:36-48`) | 4 FP-sized + `OrderType` (48 B, two-FP variant) + u64 + `Option<u64>` + Address + 3 bytes | 160 B |

Per entry in memory, including hashbrown buckets at ~0.65 average load,
allocator rounding and Vec/VecDeque slack (estimates):

| item | estimate | composition |
|---|---|---|
| position, Phase 1 raw row | ~250 B | 28 B key + Vec header + 95 B value + bucket |
| balance, Phase 1 raw row | ~120 B | 20 B key + 33 B value |
| (Phase 1 R is a sorted map: B-tree node overhead per entry is similar to a hash bucket at the same load, so the two rows above stand) | | |
| trader, Phase 1 `TraderSummary` (crab) | ~200 B | 2 FP balance mirror + 4 FP sums + counts + version + map entry |
| position term, Phase 1 summary (crab) | ~100 B | 4 FP terms + 1 FP price + Vec slack |
| account, Phase 4 typed `UserState` | ~175 B | 112 B bucket (key padded) / load |
| position, Phase 4 typed | ~125 B | 96 B `(MarketId, PositionCore)` + Vec slack |
| resting order | ~400 B | `Order` in VecDeque ~220 B + `order_index` ~75 + `order_seq` ~50 + `row_exists` ~25 + `trader_orders` ~20 |
| book price level | ~300 B | BTreeMap entry, `level_exists`, `level_epoch`, chunk aggregates (level-hash cache capped separately at 256 MB) |
| live nonce (windowed set) | ~50 B | only nonces within ±60 s (`eip712.rs:27`) |

Per user (typed, Phase 4/5):

| profile | positions | orders | per user | 100k users | 1M users |
|---|---|---|---|---|---|
| light | 2 | 5 | ~2.4 KB | 0.24 GB | 2.4 GB |
| active | 10 | 50 | ~21 KB | 2.1 GB | 21 GB |
| market maker | 300 | 900 | ~400 KB | – | – |
| mix 90% light / 9% active / 1% MM | | | ~8 KB | 0.8 GB | 8 GB |
| every user at the 1,000-order base limit | 10 | 1000 | ~400 KB | 40 GB | 400 GB |

Fixed per node: RocksDB write buffers up to 1 GB (`db.rs:912-914`), block
cache 256 MiB (`db.rs:305`), WAL cap 512 MiB (`db.rs:132`), level-hash cache
256 MB, plus mempool, DA store and network buffers. Phase 3 adds at most one
copy of the rows changed since the last checkpoint. A Phase 5 own-format
snapshot (D7 b) would need a consistent copy while it serializes (up to +1x
state); a RocksDB checkpoint needs none.

Comparison
- Owner's 64 GB per validator (HL requires 128 GB [O]): 1M mixed users ~8 GB
  of state + ~2-4 GB fixed + 2x headroom for growth and fragmentation is
  ~20-25 GB. 64 GB fits; 128 GB is not needed for the user counts discussed.
- The binding limit is total resting orders: 10M orders ~4 GB, 100M ~40 GB.
  The per-user limit (order-limit branch: 1000-5000) does not bound the total;
  margin reservation is the only economic bound (D14).
- Bench peak ~15 GB for 3 nodes + load gen (300 markets, ~100k accounts),
  i.e. <= ~5 GB per node, mostly RocksDB buffers and network/DA state.
  Phase 1 at that scale adds roughly 100k balance rows (~12 MB) plus the
  traded positions (~250 B each, e.g. 100k positions ~25 MB): well under
  1 GB per node.
- Crab summary (Phase 1): ~200 B per trader + ~100 B per position. Bench
  shape (~100k accounts, 600 senders x ~250 positions): ~35 MB. The 1M mixed-user profile
  above (~5.7 positions per user on average) adds ~0.8 GB. In Phase 4 the
  terms move into `UserState` (~+80 B per typed position).

## 5. Determinism rules for in-memory state

1. Never let map iteration order reach state, action results, trade order,
   hashes or snapshots. std `HashMap` is randomly seeded per process, so any
   such use diverges between replicas (good: tests catch it). Iterate a
   `BTreeMap` or sort first (existing pattern: `PositionCache::flush_all`
   sorts, `position.rs:601-616`; settle sorts markets,
   `native_executor.rs:3884`).
2. Parallel phases: workers compute pure per-shard results over immutable
   inputs; one thread applies them in canonical order (pass A / pass B,
   sender shards, `merge_disjoint` only over provably disjoint keys).
3. No floating point in consensus code (`FixedPoint` = i128,
   `torus-types/src/lib.rs:45-52`); no wall clock, only the block timestamp;
   no randomness; no pointer, capacity or allocation-dependent behaviour.
4. Caches must be value-neutral: a hit returns exactly what the DB would
   (Phase 1 R is complete; deletes are applied as removals). Eviction is only
   allowed for derived data (the level-hash cache pattern).
5. Every write path goes through one place that produces the block's change
   set (overlay today, typed dirty sets in Phase 5), so the running hash and
   checkpoints see every change.
6. A panic or fatal error in one replica fail-stops that node (existing
   `exec_failed` latch); partial in-memory state is never kept (the resident
   holder is invalidated on a fatal block, `native_executor.rs:1919-1922`).
7. Snapshot bytes (own format, D7 b) are written in sorted key order so
   every node produces identical bytes for the same height.
8. (crab) Every derived structure equals its from-scratch definition, to the
   raw unit and including `Err`: the margin summary == `AccountView::build`
   (bit-exact, per-position rounding, owner Q5); the mark table == the
   oracle rows; L2 certificates (if built) may only ever say "Healthy" and
   are invalidated by any write to the trader and any mark appearing,
   disappearing or going stale in its markets. A wrong derived value makes a
   warm node decide differently from a cold one; the running hash detects
   that only after the fact, so each structure has a differential test
   against its definition (crab doc P1-P7).
9. (crab) Ordered reads on the block path (`positions_for_trader`,
   `traders_after`, `adl_candidates`) come from a sorted structure (R's
   sorted map, later the `UserState` map); ties by address.

Running state hash when state is no longer written per block: `h_n` is
defined over the block's logical change set (consensus key -> new value or
delete, sorted by `(cf_id, key)`, `running-state-hash-impl.md` "Design"), not
over a database write. Phases 1-2 change nothing (the flush still digests).
Phase 3 computes the same digest at freeze over the same layers, keeps
`h(n-1)` in memory and persists `h_S` with each checkpoint; replay from S
recomputes identical digests because execution is deterministic. Phase 4
(option a) and Phase 5 keep the same logical rows, so the hash is unchanged
(pinned by the existing golden vector). A per-trader hashed format (D4 b)
would be a new hash definition at a fresh genesis.

## 6. RPC impact

| reader | Phases 1-2 | Phase 3 | Phase 4-5 |
|---|---|---|---|
| `getBalances`, `getPosition` (`torus.rs:778-880`) | DB, unchanged (DB current after W) | memory view | typed view |
| `getOpenOrders`, `getOrderBook` (`:747`, `:1518`) | DB | memory view (books are already in memory) | typed view |
| `getOpenInterest` (full scan, `:1607`) | DB | view; better: per-market OI counters | counters |
| `getUserLimits` (order-limit branch) | DB iterators | per-user counts from memory | O(1) from `UserState` |
| trade history (`:944`, `:1463`, `:1695`) | node-local trade CFs, unchanged | unchanged | unchanged |
| `getStateHash` | META | memory + checkpoints | same |
| `eth_call` with native readers (`eth.rs:818-918`) | DB | view for native CFs; EVM CFs from DB (current after forced checkpoints) | adapter |
| ingress session lookup (`torus.rs:314`) | DB | view | view |
| `--rpc-only` nodes | same as validators (they execute) | same; may use a longer checkpoint interval | same |

The view: E publishes, after each block, a block-consistent read handle
(since-checkpoint layer + DB, swapped atomically per block); RPC threads read
it without blocking E. RPC results become consistent per block (today reads
can straddle W's write).

## 7. EVM state

Recommendation: **stay in RocksDB.**
- EVM state is unbounded (contract storage) and is read and written through
  revm over `StateDb` with the incremental EVM trie (`app.rs:1657-1703`).
- EVM blocks are rare in our load (EVM time 0.0 ms in the s84 cells) and are
  already serial barrier blocks (`app.rs:1580-1585`).
- HL reportedly keeps the EVM in RocksDB [RE]; no official statement [?].
- Bug (c) put the EVM batch into the block's one flush write; Phase 3 keeps
  that by forcing a checkpoint at EVM blocks, so the EVM batch, all
  accumulated native rows and the marker land in one write.
- Native state reached from the EVM (readers 0x0800-0x0803, lockbox 0x0820
  writes balances, `precompiles.rs:4-36`) must see memory: guaranteed by the
  forced checkpoint in Phase 3 and by an adapter in Phase 5.

## 8. Decisions for the owner

D1. Build order.
- a) The order of the s84 requirement list (one record per trader second).
- b) The order of section 3 (phases are numbered in this order: resident
  rows, per-block work, checkpoints, one record per trader, full in-memory).
- c) Phase 1 only, then re-profile before choosing.
- Recommendation: b, with a re-profile after Phase 1 (c's check is built in).
- s89 re-check (crab): still b. The crab cost (all positions re-read per
  margin or liquidation question) is removed by Phase 1's summary, so one
  record per trader still gains little on its own and stays fourth. The
  re-profile after Phase 1 is done with the oracle on.

D2. Phase 1 form.
- a) Raw-bytes resident layer under the overlay: every writer covered
  automatically, generic, decode cost stays (~50 ns per row).
- b) Typed caches inside the executor: saves decode, but every writer must
  be routed through them.
- Recommendation: a.

D3. Phase 1 residency.
- a) Complete: full scan at build, a miss never touches RocksDB, RAM = whole
  CFs.
- b) Read-through LRU: bounded RAM, misses (new pairs) still hit RocksDB.
- Recommendation: a (1M positions ~250 MB).

D4. One record per trader.
- a) In memory only; logical rows, hash and RPC unchanged; no consensus change.
- b) Per-trader rows on disk and in the hash at the next fresh genesis.
- c) Skip until cross-margin / liquidation is merged.
- Recommendation: a, timed with `feat/liquidation`.
- s89 re-check (crab): still a. The timing condition is met: liquidation and
  cross margin reach main together with Phase 1 (merge after Gate 2). The
  record now also carries the margin summary (Phase 4), with per-position
  rounding (owner Q5). Option b would need a fresh genesis; if it is ever
  wanted, bundle it with the D12 / L3 genesis.

D5. Per-block state writes.
- a) Keep them (stop after Phase 2).
- b) Coalesced checkpoints into RocksDB + replay by re-execution.
- c) Per-block change-set log file (the running-hash input bytes) +
  checkpoints; recovery applies logged change sets (fast restart, new file
  format).
- Recommendation: b now; c only if restart time with b is not acceptable.

D6. Checkpoint interval.
- a) Fixed 100 blocks (aligned with attestations).
- b) ~30 s of execution time, plus forced at EVM, epoch-boundary and slash
  blocks.
- c) HL-like 10,000 blocks (replay of hours at our block times).
- Recommendation: b; it bounds replay time directly.

D7. Snapshot format.
- a) RocksDB checkpoint (hardlinks, existing code, no extra RAM).
- b) Own canonical file (borsh/msgpack, sorted): portable, hashable bytes.
- c) a now, b with Phase 5.
- Recommendation: c.

D8. Snapshot trust.
- a) Trust the source (operator copy), as today.
- b) Add a full-state LtHash (cheap once old values are in memory) to
  `AttestStateHash`; coordinated upgrade.
- c) Verify by replaying from genesis.
- Recommendation: a for devnet/testnet, b before public bootstrap.

D9. Snapshot distribution.
- a) Out of band (rsync / object store).
- b) Node serves snapshots over P2P or HTTP.
- Recommendation: a first.

D10. RPC reads from Phase 3 on.
- a) Block-consistent memory view.
- b) DB, accepting lag up to one interval.
- c) RPC-only nodes keep per-block writes, validators checkpoint.
- Recommendation: a (b breaks users seeing their own fills).

D11. EVM state.
- a) RocksDB; EVM blocks force a checkpoint.
- b) RocksDB; EVM reads native state through the layered view (no forced
  checkpoint).
- c) In memory.
- Recommendation: a; b only if EVM blocks become frequent.

D12. Nonce rows (one row per action forever, `app.rs:2140-2147`, `cf.rs:93-98`).
- a) Keep all rows.
- b) Windowed set: drop nonces older than block time - 60 s at execution;
  consensus rule + hash change, fresh genesis (HL-like).
- Recommendation: b, before Phase 5.

D13. Trade history.
- a) Keep packed rows (4.1 CPU-s/1M) + retention pruning.
- b) HL-style hourly append-only fill files + index.
- c) Both.
- Recommendation: a.

D14. RAM target and resting-order cap.
- a) 64 GB per validator, no global cap.
- b) 64 GB + a global or per-market resting-order cap.
- c) 128 GB like HL.
- Recommendation: b; size the cap from ~400 B per order.

D15. Acceptance criteria.
- a) HL parity as gates now (median 0.2 s, p99 0.9 s, 200k orders/s).
- b) Relative gates per phase, HL parity as the end goal: matched/s at 300
  markets uniform up by the phase's estimate (lower bound, >= 4+4 interleaved
  cells), no regression at 10 markets, CPU-s/1M not worse, all cells AGREE
  with identical `h_n`, low-load order age exec p50 <= 0.25 s and p99 <=
  1.0 s (today 213 ms / 2462 ms, s77), restart -> ready <= 60 s.
- Recommendation: b.

D16. Rollout.
- a) Each phase behind an env flag, default off, flipped after cells (like
  `TORUS_RESIDENT_BOOKS`, `TORUS_EXEC_PIPELINE`).
- b) Direct switch.
- Recommendation: a.

## 9. Owner decisions (s84, 2026-10-03)

| # | Decision |
|---|---|
| D1 | b: build in the order of section 3 (Phase 1 -> 2 -> 3 -> 4 -> 5 as numbered there), re-profile after Phase 1 |
| D2 | a: raw-bytes resident layer for Phase 1. HL's typed in-memory structure is the end state (Phase 5), not the next step |
| D3 | a: complete residency, no LRU (as HL: whole state in RAM) |
| D4 | a: one record per trader in memory only, timed with `feat/liquidation` |
| D5 | b: coalesced checkpoints + replay by re-execution |
| D6 | b: ~30 s of execution time, forced at EVM, epoch-boundary and slash blocks; revisit if needed |
| D7 | c: RocksDB checkpoint now, own canonical format with Phase 5 |
| D8 | a on devnet/testnet; b (attested full-state hash) before public bootstrap |
| D9 | a: out-of-band distribution first |
| D10 | a: RPC reads from a block-consistent memory view |
| D11 | a: EVM state stays in RocksDB; EVM blocks force a checkpoint |
| D12 | b: drop nonces older than block time - 60 s (consensus rule + hash change; fresh genesis), before Phase 5 |
| D13 | a: keep packed trade-history rows + retention pruning. Hourly files would need a separate indexer, and validators run `TORUS_TRADE_HISTORY=0`, so neither option touches chain throughput |
| D14 | b: 64 GB per testnet validator + a resting-order cap sized at ~400 B/order. The devnet needs no separate target (one 94 GB host for 3 nodes, ~30 GB each; ~5 GB/node used today) |
| D15 | b: relative gates per phase, HL parity as end goal. "No regression at 10 markets" means extra 10-market cells in the SAME campaign (phase vs main), next to the primary 300-market cells, never against old benches |
| D16 | No switches by default: the linear stack provides the per-phase A/B (each branch tip is its own binary); rollback = redeploy the previous binary. Overrides the recommendation in D16 above |

Branching: a linear stack of branches (each phase on top of the previous one);
each phase is benched against main and against the previous phase; the merge
decision comes at the end (the whole stack or a prefix of it). The stack is
rebased onto main whenever main moves.
s89 exception (crab): the Phase 1 branch sits on `perf/s87-crab-fixes`. The
crab commits are not rebased; after Gate 2 the rest of the crab stack and
Phase 1 are merged into main with a normal merge (shared commit ids).
Phases 2-5 then stack on main as above.

### 9.1 Owner decisions for the crab update (s88, 2026-10-04)

From the crab doc, section 6.1:

| Q | Decision |
|---|---|
| Q1 | L1: keep today's liquidation window (2048 valued / 64 acted per block), served from memory; no consensus change in Phase 1. L3 later, bundled with the D12 fresh genesis, before position holders approach 2048 |
| Q2 | Build L1 first; add L2 only if the oracle-on liquidation tail exceeds 1.0 ms per 1k fills |
| Q3 | R also serves ordered prefix and seek iteration; the margin summary and liquidation L1 land in Phase 1, not Phase 4 |
| Q5 | Bit-exact: the summary keeps per-position rounding; `AccountView::build` is the reference |
| Q4, Q6, Q7, Q8 | Open until measured (targets +15% / +30% fail line, D vs D2, interim 2b, missing main baselines) |

Stage gates (proposed, owner reviews each): Gate 1 merge >= 0.6x main (after
fixes 3/2a/1 + B); Gate 2 >= 0.9x main after Phase 1; Gate 3 >= 1.4x today's
main at 300 markets after all phases. Devnet, oracle on, paired alternating
cells, AGREE/PASS, normal drain.

## Not building (YAGNI)

- Async native trie maintenance: trie maintenance is off and has no
  production consumer (`async-native-trie-maintenance.md`).
- HL-style hash over execution responses: our change-set hash already costs
  O(changed rows) and covers state (s83 A/B: no measurable cost).
- Moving the EVM into memory.

## Open questions

- How large is a block body at bench load (estimate: a few MB per native
  block, i.e. ~100-200 GB/day per node at bench rates)? Measure
  `torus_db_size_bytes` growth to size retention.
- How HL bootstraps a new node and how fast it replays [?].
- ~~Whether the consensus-thread session lookup (`app.rs:4774`) depending on
  execution progress needs its own fix (outside item 6).~~ Resolved: removed
  by `fix/vote-after-body` (b6f6aca); arrives on main with that merge.
