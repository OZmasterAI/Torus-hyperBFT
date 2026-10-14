# Implementation Plan: item 6 Phase 4 (one record per trader in memory)

Status: PLAN, nothing built (2026-10-13). Section 6 decided by the owner (s118): 1 a, 2 b, 3 a, 4 a, 5 a.
Parent design: `market-scaling-in-memory-design.md` Phase 4,
section 4 (memory budget), section 5 rules 1, 4, 8, 9, and D4 a. Short form: `item6-phases-2-5-plans.md`
Phase 4. Format and base: `item6-phase3-impl.md`.

**Code references are `file:line` on `feat/item6-phase3-step2` @ `bd522027`** (Phase 3 step 2, not on main).
`perf/p3-merge-wait` (`06611175`, results doc section 54) is being merged into step 2. It changes `app.rs`,
`backend.rs` and `checkpoint_layer.rs`, so anchors in those three files will move after that merge.
`native_executor.rs`, `trader_positions.rs`, `resident_rows.rs` and `position.rs` are not touched by it.
Crate paths are under `crates/`.

Every number below is from the results doc (`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`) or
from the code, unless marked **(estimate)**.

## Design decision

D4 a: one typed record per trader, in memory only. The logical rows per (trader, market), the
running-hash input, the checkpoint rows and the RPC formats stay as they are. No consensus change, no
state format change, no `h_n` change.

Phase 4 merges three structures that already exist on step 2 into one record per trader:
- R's raw rows of `CF_NATIVE_POSITIONS` and `CF_NATIVE_BALANCES`;
- the decoded positions, sorted trader set and per-market holders (`TraderPositions`, C7 / E2 / adl C2);
- the margin sums cache (`SumsCache`, C3).

Writers stay as they are: they put raw rows into the block's pending set, and the record follows the
block's frozen delta at `end_resident`, as `TraderPositions` does today. Rows "produced at freeze" from
typed state need typed writes, which is Phase 5 (section 2, row 2; decision 1).

No runtime switch (D16). Each step keeps a test-only reference path (today's raw R) for the
differential, behind `cfg(test)` / the existing `test-reference-paths` feature.

## 1. Goals and gates

Goal: one lookup per trader for its balance, volume, positions and margin sums. R no longer holds the
raw position and balance rows next to their decoded copies.

| gate | target | measured by |
|---|---|---|
| Phase gate, throughput | matched/s +0-10% vs the Phase 3 head (step 2 + merge-wait), 300 markets, interval 15 s; lower bound 0 = no regression beyond noise; native blocks/s no regression; no 10-market regression (same campaign) | step 6 |
| mechanism | exec `block_ms` and engine ms per 1k fills down; `end_resident` ms per block down; R + record bytes down | step 6 |
| CPU | node CPU-s/1M no regression beyond noise | step 6 |
| correctness | identical rows, full CF dumps and every `h_n` vs Phase 3 (serial, pipelined, interval 0 / 15 s / never, all BookModes); goldens A and B unchanged; P1, P3, P5 (warm == cold), P7 (guards) green; `torus_exec_checkpoint_deferred_read_waits` stays 0 | tests, steps 1-4 |
| RAM per user | resident bytes per user (orders excluded) within section 4: light (2 positions) <= ~0.6 KB, active (10 positions) <= ~2.3 KB (175 B per account + ~125 B per position + ~80 B of summary per position) | step 6 ubench |
| RAM per node | the 10 min soak levels off, under the allocator chosen in Phase 3 | step 6 soak |

**Why only +0-10%.** Exec is the ceiling: exec thread busy 0.96 on both arms of section 54. Phase 4 removes
per-read work on R's two CFs (a `BTreeMap<Vec<u8>, Vec<u8>>` lookup, a `Vec` clone and a Borsh decode per
position or balance read of a dirty trader and per `PositionCache` / `BalanceCache` load), two of three
per-trader maps, and the raw apply of those CFs at `end_resident`. It leaves the per-write overlay path
(lock, allocation, Borsh), the cache flush, match, save books and the checkpoint upkeep. Those are Phase 5.
Section 54 also showed exec time saved turning into more, smaller native blocks (+11.3% blocks/s) with
matched/s a tie in the closed-loop bench. So a real exec saving may show up as blocks/s, not matched/s.

**What it sets up for Phase 5.** Phase 5 mutates this record in place per fill (the summary updated inline
per fill) and builds the change set from typed dirty sets, serializing each changed record once. The
record also gives O(1) account free margin for option D (design section 3.6).

## 2. What changes vs the design doc (code check on step 2 `bd522027`)

| # | summary / design says | code on step 2 | consequence |
|---|---|---|---|
| 1 | `UserState` "replaces R's two raw maps" | R holds 5 CFs: positions, balances, liquidation, oracle, markets (`torus-state/src/resident_rows.rs:33-36`) | only the first two move; R keeps the other three raw |
| 2 | rows produced from `UserState` at freeze | writers put raw rows into the pending set: `put_position` (`torus-core/src/position.rs:292`), `put_native_balance` (:335), `put_cum_volume` (:363), lockbox (`torus-core/src/lockbox.rs:125`), the cache flushes; `freeze` moves that set out (`torus-state/src/backend.rs:1416`); digest, L, view and R's delta all read it | rows from typed state need typed writes (Phase 5). Phase 4 keeps writers; decision 1 |
| 3 | new per-trader record | decoded per-trader positions already exist: `TraderPositions { map, opaque, traders, holders }` (`torus-bridge/src/trader_positions.rs:53-70`), built with R (:90), following each delta (:153, :217) | Phase 4 extends and merges an existing structure; it adds no new position read path |
| 4 | the address-ordered `UserState` map serves `traders_after` / `adl_candidates` | `traders_after` reads E2's sorted set of position holders (`trader_positions.rs:317`, called at `liquidation_step.rs:548-560`); ADL ranks a per-market holder list (`holders_with` :291, used at `liquidation_step.rs:645`; `adl_candidates` takes that list, `torus-core/src/liquidation.rs:441`); no whole-map scan is left | an ordered map of all accounts includes balance-only accounts that the walk must step over; decision 3 |
| 5 | the crab sums cache and each position's terms move into `UserState` | `SumsCache { version, map: HashMap<Address, SumsResult> }` (`native_executor.rs:1049-1057`) in `RowsSlot` (:3026-3038), one `PosSums` per trader (:905-922); no per-position terms are stored (changed rows are re-valued on the fly, `sums_with_changes` :934); the cache is cleared when the mark version moves, which is most blocks with the oracle on (:1253-1262), so the per-block memo (`BlockSums.memo` :1071) does most of the work | Phase 4 moves the per-trader sums only; decision 4 for the memo |
| 6 | `Position` 96 B -> `PositionCore` 80 B | Position v2 adds `cost_basis` (`position.rs:54-72`, 111-byte row :102): 112 B in memory, ~96 B without trader and market | keep `Position` whole (section 3); 112 B + no slack fits section 4's ~125 B per position |
| 7 | `open_order_count` in `UserState` | no stored count: counts come from the books per `execute_batch` call (`native_executor.rs:6560`, :8450; `torus-core/src/order_book.rs:45`); books live in `ResidentInner` (:3040) and match on per-market workers | a counter needs per-market deltas from every rest, cancel, fill and stop site; decision 2 |
| 8 | `cum_volume` field | a `cvlm` row in the balances CF (`position.rs:230-238`), read once per sender per call (`native_executor.rs:8407`), written per fill (:8472) | a typed field, as planned |
| 9 | (not in design) | merge-wait defers L's positions and balances merge; no exec read may reach them (`DEFERRED_CFS`, `checkpoint_layer.rs` on `perf/p3-merge-wait`; `deferred_read_waits` 0 in section 54). That holds because R answers both CFs completely (`backend.rs:2323-2327`) | the record must stay complete for both CFs, irregular rows included; a read that falls through to L would wait |
| 10 | (not in design) | clean traders already skip raw: `AccountReader::get_position` reads the record (`native_executor.rs:1630-1660`). Dirty traders and every `PositionCache::load` (`position.rs:644`, :671) and `BalanceCache::load` (`native_executor.rs:337`) go overlay `get_cf_raw` (`backend.rs:2311`) -> R lookup + clone -> Borsh decode (`position.rs:278-289`, :324-333) | step 4 targets these loads |
| 11 | (owner s114) layer / view shrink reconsidered at Phase 4 | R's raw rows of both CFs duplicate the decoded records. At bench shape (~150k positions x ~250 B, ~100k balances x ~120 B) that is ~50 MB per node **(estimate)**. The Phase 3 RSS delta is +1.10 GB per node under jemalloc (levels off) and +2.62 GB still growing under glibc (section 53d); layer ~153 MB logical per checkpoint at ~2.1x, view ~0.68 GB (sections 48, 54). L and the view hold the frozen raw rows that the checkpoint writes and the digest hashes | Phase 4 does not shrink L or the view by itself; decision 5 |
| 12 | (not in design) | `torus-state` cannot name `Position` or `NativeBalance` (`torus-core` depends on `torus-state`) | the overlay reads the record through a byte-level trait in `torus-state`, implemented in `torus-bridge` (step 2) |

## 3. Shape of the core

```
RowsSlot today (native_executor.rs:3026)      RowsSlot in Phase 4
  rows: R { positions, balances,                rows: R { liquidation, oracle, markets }   raw, as today
            liquidation, oracle, markets }      users: Users {
  positions: TraderPositions {                    map:     HashMap<Address, UserState>     (decision 3)
    map, opaque, traders, holders }               traders: BTreeSet<Address>   E2: holders of a 28-byte key
  sums: SumsCache { version, map }                holders: HashMap<MarketId, HashSet<Address>>   adl C2
                                                  opaque:  traders with an irregular row, kept raw
                                                  other:   raw rows under no trader (keys < 20 bytes) }

UserState {
  balance:    Option<NativeBalance>        None = no balance row (absent != zero: a different row set)
  cum_volume: Option<FixedPoint>           None = no cvlm row
  positions:  Vec<Position>                market order (= key order), shrink_to_fit
  sums:       Option<Box<(u64, SumsResult)>>   version tag; boxed so light users pay 8 B
}
```

- **Regular rows only.** A trader is regular when every row of it in both CFs decodes AND re-encodes to
  the same bytes (28-byte position key matching trader and market; 20-byte balance; 24-byte `cvlm` with a
  16-byte value). The re-encode check is new: today raw reads are served from raw bytes, so a lenient
  decode (for example `is_long` byte 2, `margin_type` byte 7) did not matter. Once raw reads are served
  by encoding the record, it does. Any other trader is opaque: its rows stay raw in the record set and are
  served as bytes.
- **Reads.** Overlay order is unchanged: own pending -> R (now: the record set for the two CFs) -> parent ->
  L -> C -> DB, and R's CFs never go past R. Point, prefix, seek and `prefix_exists` reads of the two CFs
  come from `Users` through a `torus-state` trait (row 12). Typed readers in `torus-bridge` read the
  record directly for clean keys (step 4).
- **Ordered iteration.** The positions CF in key order = `traders` in address order, each user's
  positions in market order, merged with opaque raw rows. The balances CF in key order is needed only by
  the proof-only `liquidation_value_sum` (`liquidation_step.rs:194`): served by a collect-and-sort slow
  path, counted in tests.
- **Upkeep.** `end_resident` applies the frozen delta to `Users` (positions as `TraderPositions::apply`
  today, plus balance and `cvlm` rows). The raw apply of the two CFs (`resident_rows.rs:114-129`) goes away.
  Same guards, same worker (`end_resident_on_worker`, `native_executor.rs:3629`), same `Arc::get_mut`
  check, extended to `Users`.
- **Sums.** An entry is read only when its tag equals the block's mark version. A moving version no
  longer clears a map: stale entries are ignored and overwritten (no O(traders) clear).

## 4. Steps and tasks

Each task: test first (fails before), then code, then verify with
`F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"`.
Before every commit: `cargo nextest run --workspace $F` and the doc-test command from `TESTING.md`.

| step | what | size | who |
|---|---|---|---|
| 0 | re-profile the Phase 3 head; stop rule | S | archy |
| 1 | `Users` record set next to raw R (replaces `TraderPositions`) | M | builder |
| 2 | overlay reads the two CFs from `Users`; R's two raw maps deleted | M | builder, then Codex torus-adversarial |
| 3 | sums cache into `UserState`; `SumsCache` deleted | S | builder |
| 4 | typed loads for `PositionCache`, `BalanceCache`, `cum_volume` | S-M | builder |
| 5 | open-order count in `UserState` (decision 2 b) | M | builder |
| 6 | RSS per user ubench + gate campaign | S | builder (ubench), archy (campaign) |

### Step 0: re-profile (archy, measurement only, no code)

On the Phase 3 head (step 2 + merge-wait), 300 markets uniform, oracle on, interval 15 s:
- exec-thread share of R reads of the two CFs: `get_cf_raw` / `iterate_cf` on them, the clone and the
  Borsh decode of `Position` / `NativeBalance`;
- `PositionCache::load` / `BalanceCache::load` misses per native block;
- `BlockSums` memo lock and `Arc<OnceLock>` allocation (decision 4);
- `open_order_counts` ms per native block (decision 2);
- `exec_end_resident_rows_seconds`, `exec_end_resident_positions_seconds`, `exec_resident_rows_bytes`.

**Stop rule:** if the items Phase 4 removes are below ~2% of exec-thread time, the throughput gate is
about 0. The owner then decides: build Phase 4 now as Phase 5's base, or fold it into Phase 5.

**Measured (campaign ozarchy-p4s0, results doc section 64, plan row 72; main `d3cba5e2`, 2026-10-14).** % of exec-thread
wall, mean of 2 perf cells: R reads of the two CFs 2.43 (exec thread 1.21 + settle-pool `PositionCache` misses the exec
thread waits on 1.21), memo lock + `Arc<OnceLock>` 0.18, `open_order_counts` 1.45 (3.5 ms per native block); sum 4.06,
2.61 without `open_order_counts`, gross (the typed replacements' cost is not netted). Load misses: no counter in the build;
`BalanceCache::load` miss path 0.29%. `end_resident` rows 13.3 / positions 10.0 ms per native block on the worker, exec
wait 2.1 ms; `exec_resident_rows_bytes` 214 MB. **Above ~2%: Phase 4 goes ahead; the margin is thin** (2.43% for R reads
alone, since the memo stays under decision 4 a). Decision 2's number for the owner: 1.45% / 3.5 ms per native block.

### Step 1: `Users` record set next to raw R

Raw R still serves every read. The new record set only replaces `TraderPositions` and is checked against R.

**Task 1.1: record type, build and follow**
- Test first (`torus-bridge`, new `users_tests.rs`, from `trader_positions_tests.rs`):
  - `users_equal_decoded_rows_after_random_deltas` (from :219): random blocks over positions, balances,
    `cvlm` rows and irregular rows; after every block the records equal R decoded.
  - `users_encode_byte_identical_to_rows`: for every regular user, encoding each field gives R's bytes;
    a row whose re-encoding differs (`is_long` byte 2, `margin_type` byte 7, trailing byte) makes the
    trader opaque.
  - `users_size_of_pins`: `size_of::<UserState>()` and `size_of::<Position>()` pinned (target **(estimate)**
    UserState <= 112 B, Position 112 B).
  - Existing E2 / C2 tests moved over unchanged: `traders_after_equals_the_walk_over_r_and_pending` (:286),
    `holder_lists_with_the_dirty_traders_cover_the_walk` (:370), `holder_lists_follow_scripted_blocks_bit_identically`
    (:456), `holders_with_ascending_whatever_the_insertion_order` (:759).
- Code: `trader_positions.rs` becomes `users.rs` (`Users`, `UserState`); `build` (:90) and `apply`
  (:153, :217) also follow balance and `cvlm` rows; `get` (:303), `traders_after` (:317),
  `holders_with` (:291) unchanged in behaviour. `RowsSlot.positions` -> `users`
  (`native_executor.rs:3036`), `BlockSums.records` (:1074), `begin_resident` (:3555), `end_resident`
  (:3794-3805).
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`.
- Metric rename (owner s119): the `end_resident` sub timer now also covers balance and `cvlm` rows, so
  `exec_end_resident_positions_seconds` became `exec_end_resident_users_seconds`
  (`torus_exec_end_resident_users_seconds`); its values are not comparable with older builds.
  `tools/matched-bench` samples and summarizes both names (the one a binary does not emit reads 0.0), so
  archived cells still summarize. The subs shift between Phase 4 steps (step 1 moved balance / `cvlm` work
  into users; step 2 removes the raw rows apply), so cross-step comparisons use the `end_resident` total and
  `end_resident_wait`, not the subs.

### Step 2: the overlay reads the two CFs from `Users`

Consensus-adjacent read path. Codex torus-adversarial review before step 3.

**Task 2.1: byte-level resident source in `torus-state`**
- Test first (`torus-state`, next to `prefix_exists_matches_iterate_cf_over_every_layering`
  `backend.rs:2682` and `iterate_cf_from_matches_iterate_cf_over_every_layering` :2732):
  `resident_source_reads_match_raw_rows`. For every layering (own pending, parent, L, C, DB) and random
  keys, an overlay whose positions and balances slots come from a test source answers `get_cf_raw`,
  `iterate_cf`, `iterate_cf_from`, `iterate_cf_prefix_from`, `prefix_exists`, `layer_touches` and
  `layer_keys` exactly as the same rows held raw in R.
- Code: a `ResidentSource` trait in `resident_rows.rs` (point get as bytes, ordered range from a key,
  len / bytes); `ResidentRows` holds raw maps for slots 2-4 and the source for slots 0-1; `build` and
  `apply` (:101-129) skip slots 0-1. Overlay reads: `get_cf_raw` (`backend.rs:2311`, R branch :2323-2327),
  `iterate_cf` (:2381), `iterate_cf_from` (:2434), `iterate_cf_prefix_from` (:2493), `prefix_exists` (:2538).
  `resident_delta_of` (:960) and `FrozenPending::resident_delta` (:1060) are unchanged (the delta is still
  raw rows).
- Verify: `cargo nextest run -p torus-state $F` and `cargo check --workspace --tests -q`; then
  `cargo nextest run -E 'rdeps(torus-state)' $F` (core crate, read path).

**Task 2.2: `Users` as the source; raw maps deleted**
- Test first:
  - R == DB dumps after every flush, now with the two CFs read through `Users` (`app.rs:20479-20502`), at
    interval 0, 15 s-like (every 5 blocks) and never.
  - Guards (P7): `resident_rows_tests.rs:73-287` (marker mismatch, skipped height, fatal block, live clone,
    pipelined parent) green with `Users` in the slot; a live clone of `Users` falls back to a rebuild.
  - Worker: `end_resident_worker_tests.rs:135-389` green.
  - `users_reads_never_reach_the_deferred_part` (after the merge-wait merge): over 130 blocks at interval
    "never", `deferred_read_waits` stays 0, including blocks with irregular rows.
  - Warm == cold: `warm_equals_cold_every_block_{serial,pipelined,...}` (`app.rs:22684-22699`).
- Code: `Users` implements `ResidentSource` (`torus-bridge`); `begin_resident` attaches it with R
  (`native_executor.rs:3494-3590`, `app.rs:3201`); `EndResidentJob::run` applies the delta to `Users`
  only for the two CFs (`native_executor.rs:3782-3805`); gauges split into rows and users bytes.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`, then `cargo nextest run --workspace $F`.
- **Built (s120, branch `feat/item6-phase4-step2`).** `ResidentSource` (point get, length, ordered range "from a key
  while under a prefix") in `resident_rows.rs`; `ResidentRows::build_with_source` holds slots 2-4 raw and reads
  slots 0-1 from the source; `apply` skips them; today's raw R stays as the reference (`ResidentRows::build` /
  `Default`, `cfg(test)` / torus-state feature `test-reference-paths`). The overlay reads all R CFs through one
  view (`Resident`), read order unchanged. `Users` implements the trait: point reads encode the record (opaque /
  other rows as bytes); positions in key order walk the trader set merged with opaque and other rows; balances
  ranges under a prefix naming one trader (20+ bytes, not a `cvlm` head under 24) read that trader's row and the
  other rows, every other balances range is the collect-and-sort slow path (thread-local test counter). R keeps
  its source (an `Arc<Users>`, the same one `BlockSums.records` and `ResidentBlock.users` hold) in the slot
  between blocks; `end_resident_take` takes the source off and drops the sums' handle, then requires the record
  set unshared like R (a live clone: `rows_shared_fallbacks` + 1, slot empty, next block rebuilds).
  Deviations from the text above: no separate "len / bytes" on the trait (the bridge sets the gauges from typed
  `Users`, which keeps a row count and bytes per change); `SumsCarry::drop_written` deleted (the record set always
  comes back with R now, so the "context kept the block's state" rebuild path is gone); `trader_positions_match_rows`
  checks `Users` against a cold build of its own rows (R has no raw rows to compare), the rows themselves are checked
  against the DB through `ResidentRows::dump` (p2 runs now check R == DB after every block whose writes are all
  durable, at interval 0 / every 5 / never).
  Metrics: `torus_exec_resident_rows` / `_bytes` now count R's raw rows (liquidation, oracle, markets) only; new
  `torus_exec_resident_users` / `_bytes` count the record set's rows of positions and balances (key + value
  bytes as stored); rows + users == the older meaning. `exec_end_resident_rows_seconds` likewise covers the raw
  CFs only. `tools/matched-bench/campaign-template.sh` samples both bytes gauges on the G lines (an older binary
  omits the users one: reads 0); summarize.py reads neither and keeps the sub names.

### Step 3: sums cache into `UserState`

- Test first:
  - P1 on the new home: `sums_cache_equals_reference_on_seeded_sequences` (`sums_cache_tests.rs:435`),
    `carried_sums_equal_reference_on_calm_marks` (:469), `end_resident_carries_dirtied_traders_and_rebuild_starts_empty`
    (:521), `end_resident_on_worker_equals_inline_and_reference` (:492).
  - `stale_version_entry_never_read`: the version moves; an entry with the old tag is never returned, and
    no step clears all users.
- Code: `UserState.sums`; delete `SumsCache` (`native_executor.rs:1049-1057`) and `RowsSlot.sums`;
  `BlockSums::base` (:1238) and `cached_sums` (:1728) read the user's entry; `SumsCarry::trader`
  (:1096-1148) and `drop_written` (:1150) write it.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`.
- **Built (s120, branch `feat/item6-phase4-step3`, on step 2).** `UserState.sums: Option<Box<(u64, SumsResult)>>`
  (tag = mark version); `Users::sums(trader, version)` returns an entry only when its tag equals the version.
  `SumsCache` and `RowsSlot.sums` deleted; `BlockSums::base` and `cached_sums` read the user's entry through
  `BlockSums.records`. At `end_resident`, `Users::apply` hands each trader written in the positions CF to
  `SumsCarry::trader` together with its entry: the entry is moved on (base = the block's memo, else the entry
  tagged with the version) or cleared; a re-decoded record starts with no sums. After apply, `SumsCarry::finish`
  joins the memo into the users' entries (tagged with the memo version) for traders the block did not write.
  A moving version clears nothing: stale entries stay and are ignored or overwritten (`stale_version_entry_never_read`).
  `UserState` is still 112 B (step 1 already counted the field).
  Deviations from the text above: `drop_written` was already gone (step 2). `RowsSlot` keeps one `sums_version: u64`
  (the version entries were last joined at). The node-local "carry only while the version held" rule needs it;
  reads never use it. A trader with no record (opaque, or no rows left after the block) keeps no sums, so a
  trader whose last row was deleted loses its entry. Before, the cache kept such an entry; no result changes.
  `SumsCarry::trader` also runs when no sums came back with the block (a context that never handed back its
  block state). Written traders' entries are then cleared, the rest stay valid, because before this change the
  whole cache was dropped in that case. `drop_sums_cache` (tests only) clears every user's entry. Metrics and bench
  tools are unchanged: the memo join moved after the users apply, outside the users sub-timer, as before.

### Step 4: typed loads

- Test first: `typed_loads_equal_overlay_reads`. Random blocks with pending writes in earlier
  `execute_batch` calls; for clean and dirty keys the typed load of a position, a balance and a
  `cum_volume` equals `PositionManager` over the overlay; a test counter shows the typed path for clean
  keys. Goldens A and B (`tests/perf_equivalence_golden.rs`) unchanged, serial and engine-forced.
- Code: own pending first (as raw bytes, as today), else the record. `PositionCache::load` / `update_in_place`
  (`position.rs:644`, :671) and `BalanceCache::load` (`native_executor.rs:337`) take an optional typed
  source; `get_cum_volume` at `native_executor.rs:8407` / :8477 likewise. Needs an own-pending-only lookup
  on `StateBackend` (default: none).
- Verify: `cargo nextest run -p torus-core $F`, `cargo check --workspace --tests -q`, then
  `cargo nextest run -E 'rdeps(torus-bridge)' $F`.
- **Built (s120, branch `feat/item6-phase4-step4`, on step 3).** `StateBackend::own_pending_row(cf, key)` ->
  `OwnPendingRow::{Unknown, Pending(Option<bytes>), Resident}` (torus-state; default `Unknown`; the overlay answers
  only with R attached for the CF: own pending write / tombstone, else `Resident`). torus-core: trait `TypedRows`
  (position / balance / `cum_volume` of a trader, `None` = decline), implemented by `Users` in torus-bridge (an
  opaque trader declines; no record = `Some(None)`), so the dependency direction holds (row 12).
  `PositionManager::{get_position, get_native_balance, get_cum_volume}_typed(typed, ..)`: own pending bytes decoded
  as before, else the record when `Resident`, else (`Unknown`, no source, a decline) `get_cf_raw`; same errors.
  `PositionCache::load_typed`, `apply_fill_cached_effect_typed` (the private `update_in_place` takes the source);
  the untyped `load` / `apply_fill_cached_effect` remain as `None` wrappers. torus-bridge passes the block's
  record set (`BlockSums.records`; `NativeExecContext::typed_rows`, `AccountReader::typed`) to `BalanceCache::load`,
  the sequential and parallel settle (`apply_fill_via_caches`, `compute_market_settle_plan`), the `cum_volume`
  reads (`take_open_slot`, `add_cum_volume`), and `d2_pools` / `sell_top_ups`. The check is per key (own pending
  row), finer than `AccountReader`'s per-trader dirty check. Tests: `own_pending_row_matches_get_cf_raw`
  (torus-state), `typed_rows_tests.rs` (torus-core, fake backend and source: routing and errors),
  `typed_loads_equal_overlay_reads` (torus-bridge: 4 seeds x 24 blocks x 3 calls, engine forced / sequential /
  parallel settle, own pending tombstones, an opaque trader; `Users` test counters).
  Deviations from the text above: also `SenderState::load_balance` (Phase 2's per-sender balance load, which is
  `BalanceCache::load` by another name), `d2_pools` and `sell_top_ups` (their `BalanceCache::load` calls) and the
  single-action path's `add_cum_volume` / `take_open_slot`. Not changed: `AccountReader::maker_free`'s balance read
  and the single-action path's `get_native_balance` / `apply_fill` (not named by the plan).

### Step 5: open-order count (decision 2 b)

Step 0's `open_order_counts` number goes to the owner before this step starts (decision 2).
**Owner s119:** step 0 measured 1.45% (3.5 ms per native block), below decision 2's ~5% bar. Step 5 runs last and
stays only if its own A/B (counter upkeep netted) shows a gain; otherwise it is dropped or moved to Phase 5.

- Test first: `open_order_count_in_user_equals_book_scan`. After every block of random places, cancels,
  fills, stops and cancel-alls, each user's count equals `open_order_counts` over the books
  (`order_book.rs:45`), serial and with match workers.
- Code: per-market workers return per-trader count deltas; E applies them in market order (rule 2);
  `open_at_start` (`native_executor.rs:6560`) reads the record. Books rebuilt from the DB rebuild the counts.

### Step 6: RSS per user and gate campaign

- Ubench `ubench_users_rss` (`torus-bridge/tests`, ignored): build 100k and 1M synthetic users (light: 2
  positions; active: 10) and report resident bytes per user from allocator stats. Pass: light <= ~0.6 KB,
  active <= ~2.3 KB (section 1).
- Campaign (archy): Phase 4 head vs Phase 3 head (step 2 + merge-wait), interval 15 s, Classic, standard
  300-market shape, oracle on, n = 4 ABBA, 120 s cells; plus 10-market cells and one 10 min soak pair.
  Allocator: the one Phase 3 picks (glibc and jemalloc pairs if still open).
- Report: matched/s, native blocks/s, `block_ms`, exec busy, engine ms per 1k fills, `end_resident` ms,
  rows and users bytes, RSS mean / peak / slope, CPU-s/1M, block time p99.
- **Built (s120, branch `feat/item6-phase4-step6`, on step 4; ubench only).** `crates/torus-bridge/tests/ubench_users_rss.rs`.
  Users: hashed addresses, light 2 / active 10 positions in distinct markets of 300 (1 in 10 Isolated), each with a
  balance row and a `cvlm` row, built cold from raw rows the way R's rebuild does (`Users::build_maps`, through the
  `#[doc(hidden)]` seam `native_executor::users_bench::UsersProbe`). Then every record gets a sums entry (step 3's
  steady state: entries are never cleared). Allocator stats: the test binary has no jemalloc (only `torus-node` sets
  it as `#[global_allocator]`), so a counting global allocator over `System` counts live bytes three ways: requested,
  glibc (`malloc_usable_size` + 8 B header), jemalloc 5 size class (small classes <= 14 KiB; above that, the request
  rounded up to whole pages; slab fragmentation not counted). Pass is judged on the jemalloc column with sums. The
  ubench asserts only loose bounds (at least the positions' bytes, at most 4x the target, drop frees all). Non-ignored
  checks in the same file: synthetic users (also market makers, 100 and 300 positions) are regular and round-trip
  byte for byte, the size-class table, the counter
  sees a build and its drop.
  Run: `cargo test -q --release -p torus-bridge --test ubench_users_rss -- --ignored --nocapture` (default sizes
  `100000,1000000`; `UB_USERS=100000` for one size; ~330 s on 18c). Measured on 18c (B per user, jemalloc column,
  no sums / with sums):

  | shape | 100k | 1M | target |
  |---|---|---|---|
  | light (2 positions) | 534 / 726 | 627 / 819 | ~600: **over with sums** |
  | active (10 positions) | 1,786 / 1,978 | 2,044 / 2,236 | ~2,300: ok |

  | shape (s121) | 1k | 10k | target |
  |---|---|---|---|
  | mm100 (100 positions) | 16,348 / 16,540 | 15,278 / 15,470 | 20,675: ok |
  | mm300 (300 positions, every market) | 50,754 / 50,946 | 47,595 / 47,787 | 61,675: ok |

  Market makers (s121): target = section 1's 175 B + ~125 B + ~80 B sums per position. They run at env
  `UB_MM_USERS` sizes (default `1000,10000`), not `UB_USERS`; env `UB_SHAPES` (e.g. `mm100,mm300`, default all) picks
  shapes. The positions `Vec` has no growth slack (shrunk at build); the gap to jemalloc is size-class / page rounding
  of that one `Vec` (mm300: 33,600 B -> 36,864 B). The other ~35 B per position is adl C2's per-market
  `HashSet<Address>` (load 0.61 at 10k holders). The sums entry costs ~190 B per user, not per position.

  Why light is over: the sums entry is one `Box<(u64, SumsResult)>` per user, 176 B (jemalloc class 192). The
  section 1 budget is ~80 B per position, so 160 B for 2 positions. The record table (`HashMap<Address, UserState>`,
  144 B per slot) runs at load 0.87 at 100k and 0.54 at 1M (just after a doubling), which adds ~100 B per user at 1M.
  Logical row bytes: 371 (light), 1,483 (active). glibc column: light within ~15 B of jemalloc; active ~160 B lower
  (a 10-position `Vec`, 1,120 B, is class 1,280 in jemalloc). Full table in the test output (`USERS_RSS` lines).
  Owner s121: light-user overshoot accepted; margin-sums slimming (keep the note only for recently active users, or
  inline in UserState) moved to Phase 5 as an optional item.
- **Campaign: ready for archy.** Phase 4 head = `feat/item6-phase4-step4` @ `2a245321` (steps 1-4; the step 6
  branch adds only the ubench and its unused seam) vs Phase 3 head = main @ `21d238e0` (last main before the step 1
  merge `7487b764`; includes merge-wait `06611175`). Interval 15 s, Classic, standard 300-market shape, oracle on,
  n = 4 ABBA, 120 s cells; plus 10-market cells and one 10 min soak pair; allocator jemalloc (the `torus-node`
  default feature). Report as listed above. Step 5 (`feat/item6-phase4-step5` @ `6288ebe8`, on step 4, incl. the Codex r1 fix) is on its own
  branch: its A/B (step 5 vs step 4, counter upkeep netted) is a separate campaign, not part of this one.

## 5. Not building

- Per-trader rows on disk or in the hash (D4 b; fresh genesis, with D12 / L3 if ever).
- Typed writes and rows produced from typed state (Phase 5), unless decision 1 c.
- Stored per-position terms (Phase 1 optional step O1; never needed by a gate).
- `PositionCore` without trader and market (16 B per position; signature churn in `margin.rs` and
  `fill_transition` for ~2 MB at bench shape **(estimate)**).
- An ordered index of the balances CF (only a proof-only reader iterates it).
- Shrinking L or the view (decision 5).

## 6. Decisions (owner s118)

1. **Scope of the swap.**
   (a) Read side: `Users` replaces R's two raw maps, writers unchanged (this plan, M).
   (b) Merge records and sums only, keep R's raw maps (S; no RAM change; saves one lookup per trader).
   (c) Also typed writes, rows produced at freeze (L; overlaps Phase 5's typed dirty sets).
   Recommendation: a. It is the summary's Phase 4 without Phase 5's write path.
   **Decided: a.** HL keeps all state typed in one in-memory struct [RE] (c plus Phase 5); Phase 5 closes
   that gap.
2. **Open-order count.**
   (a) Keep counting from the books per call (today).
   (b) A counter in `UserState`, step 5.
   Recommendation: a, unless step 0 shows `open_order_counts` at >= ~5% of exec time.
   **Decided: b (step 5); s119: step 0 gave 1.45%, so step 5 is kept only on its own net A/B (see Step 5).** The count serves the HL-style per-user cap
   already on main (min(1000 + cumVolume / 5M, 5000) over all markets, merge `2567e56`; HL docs [O]).
   (a) walks every book for every sender per batch (`order_book.rs:47` on `c88d3073`), so it grows with
   markets x senders. Section 11 of the results doc (older build, 300 markets): 0.26-0.36 ms per 1k
   fills on the exec thread, ~6-8% of today's ~4.4 ms per 1k fills (section 60: +0.71 = +16%)
   **(estimate, not measured on step 2, which splits the count across threads)**. The node is
   exec-bound, so exec time saved is blocks/s and matched/s. (b) adds a hash update per rest, cancel,
   fill and stop (the kind of upkeep that cost cloid +0.36 ms per 1k fills), so the net saving is
   smaller than the 6-8%. HL checks the cap on every order, which needs a stored count (inference; HL's
   code is not public).
3. **Map shape for the liquidation walk and ADL.**
   (a) `HashMap<Address, UserState>` plus E2's sorted holder set and C2's per-market holders (today's
   measured shape).
   (b) `BTreeMap<Address, UserState>` serving `traders_after` directly (design text): the walk steps over
   balance-only accounts, ~100k at bench shape, every block **(estimate)**; point reads O(log n).
   (c) A `BTreeMap` of position holders plus a hash map of balance-only users.
   Recommendation: a. It keeps the E2 and C2 results and hash point reads.
   **Decided: a** (today's `TraderPositions` shape, `trader_positions.rs:53`). HL's shape is (b)
   (`BTreeMap<addr, UserState>` [RE]). (b) costs a tree descent per point read (every prepare and fill)
   and a walk over balance-only accounts per block; C2's `BTreeSet` -> `HashSet` swap cut `apply` per
   fill by 36% (`item6-phase2-impl.md` row 40), and (b) would undo that kind of gain.
4. **Per-block sums memo.**
   (a) Keep `BlockSums.memo`; move only the persistent per-trader sums (step 3).
   (b) A per-user memo cell in `UserState` (drops the per-block `Mutex<HashMap>` and one `Arc` per trader;
   needs a versioned cell).
   Recommendation: a; b only if step 0 shows the memo.
   **Decided: a** (b only if step 0 shows the memo). HL: not public.
5. **The s114 RAM follow-up (layer and view shrink).**
   (a) Not in Phase 4. Pick the allocator first (sections 53b, 53d: jemalloc levels off at ~+1.1 GB, glibc
   grows). Shrink as its own task after Phase 4 if the RAM rule still fails.
   (b) A persistent (structurally shared) `Users` map that the published view pins, so the view drops its
   positions and balances slices (L-size, Phase 5-adjacent).
   (c) Compact L's storage (~2.1x logical today) as an extra Phase 4 step.
   Recommendation: a. Phase 4 saves ~50 MB per node **(estimate)**; the Phase 3 delta is layer, view and
   allocator retention.
   **Decided: a.** HL: 128 GB RAM per node [O]; allocator and layout not public.

## 7. Rollback

No runtime switch (D16). Rollback = redeploy the Phase 3 binary. Nothing is persisted: `Users` is
node-local and rebuilt cold from DB + layers at the first block, like R today. No data migration.
