# Implementation Plan: crab-stack perf fixes 1-3 (s87)

Branch: new commits on top of `rebase/s85-oracle-feeder` (tip `c93c579`). Worktree
`/home/18c/projects/wt/s87-ubench` (detached at `c93c579`). Status: fixes 3, 2a and 1 IMPLEMENTED
on `perf/s87-crab-fixes` (C0 `d0d722b`, C1 `6cbd812`, C2 `473a037`, C3 `4d81f4a`). Golden digests
unchanged. ubench_econ (ms/1k fills, median of 3): ~135-169 → 105 (Fix 3) → 95, tail 1.3 (Fix 2a)
→ 41 (Fix 1): Fix 1 misses its ≤ 25 target. Remaining cost is per-(sender, market) position point
reads in Phase 2 and the Phase-3 policing (outside this plan; see the commit message of C3).
Fix 1 implements the mark table lazily (one `OnceLock` per market, no eager reads). 1c not done.
Scope: fixes 1-3 only. Fix 4 (sharing free margin across markets) is a separate design
discussion. Nothing here changes it, and the F1 D2/D8 rules (exclusive pool, start-of-batch
maker snapshot) stay byte-identical.

Status (option B, s87, after C4): IMPLEMENTED as C5 (owner decisions: narrow B-N + B2). A
batch sell outside the sender's D2 pool market reserves at max(limit | mark-or-cap, start-of-
Phase-2 best bid); a taker-only budget gets the makers' +1 raw rounding allowance. GOLDEN_A
re-pinned (accepted 1555 → 1686, rejected_cancelled 299 → 188, rejected_margin 2078 → 2079),
GOLDEN_B unchanged. ubench_econ: rejected_cancelled ~17.5k → ~5.6k per run, fills/block 6.2k →
9.9k, ms/1k fills ~37-41 → ~29-30 (UB_MARKS=1 ~59-69 → ~42-46). Every remaining zero-fill
exhaustion hits a bid placed earlier in the same batch above the start best bid (probe; 25-28% of
the pre-B 17.5k). The same-batch bound is NOT implemented; it needs owner approval.

## Design decision

| Fix | Chosen design | Consensus impact |
|---|---|---|
| 3 | Range-bounded layer merge in `PendingState::overlay_into` plus a no-overlay fast path in `NativeStateOverlay::iterate_cf` | none (read path only; identical results) |
| 1 | A per-`execute_batch` memo of each maker's snapshot `free`, shared by all market workers, plus a per-batch mark table in `AccountReader` | none (same values, computed once) |
| 2 | (a) when no listed market has a usable mark, skip the valuation (`liq_view`) of every account. Cursor, pending, cooldown and prev-mark writes stay exactly as they are. (c) is already in place. (b) is deferred to an owner decision. | none (same writes) |

All three are pure refactors of *how much work* is done. None of them changes a state row,
a result or the state root. That is the property every test below checks.

## Success criteria

1. Golden equivalence: per-block state digests and result digests on the seeded scenarios
   (Task 0.2) are identical to `c93c579`, serial and engine-forced, after every commit.
2. Every existing test passes, in particular: `torus-state` backend tests;
   `torus-bridge` tests `account_margin_tests`, `maker_margin_release_tests`,
   `engine_parallel_tests`, `parallel_matching_tests`, `parallel_settle_tests`,
   `market_order_margin_tests`, `reduce_only_tests`, `liquidation_tests`, `oracle_block_tests`;
   `torus-consensus` `liquidation_determinism_serial_pipelined_and_replay_are_identical`;
   `torus-integration-tests` chaos `liquidation_step_keeps_incremental_root_equal_to_full_scan`.
3. The work-count RED tests (Tasks 1.1, 2.1, 3.1) fail on `c93c579` and pass after their fix.
4. `ubench_econ` (median of 3 runs, ms per 1k fills; `c93c579` = 161, main `d995f68` = 12.8)
   meets the per-fix targets in the Verification section.

---

## Verified facts this plan relies on (c93c579)

* **Backend is frozen during Phase 3.** Phase 2 reservations go into the write-back
  `BalanceCache` (`fold.cache`, `native_executor.rs:4123-4134`, `prepare_one` writes only
  `fold.cache.set`). Phase 4 fills go into `pos_cache` / `bal_cache`. Both are flushed only
  after settlement (`:4482-4489`). Market workers get only books, requests and
  `&dyn MakerAccountSource` (`market_workers.rs:84-90`). Nothing writes the backend between
  the end of Phase 2 and the end of Phase 4. Stops fired during matching run afterwards through
  the single path (`run_triggered_stops`, `:4498`).
  => Within one `execute_batch` call, `AccountReader::maker_account(maker, m).free`
  (`:802-813`) returns the **same value for every market `m`**. It reads only the backend
  balance and `positions_for_trader`, and `free` does not depend on `m`. Positions and balances
  do change between `execute_batch` calls (pre-EVM / post-EVM calls of one block) and between
  single-path placements. So the memo is scoped to one call's Phase 3 and is never used on the
  single path (`:6259`, `:6369-6381`).
* **Marks are constant within a block.** The only writer of the aggregated price row is
  `aggregate_oracle_prices` (`:7725`), called only from `begin_block_oracle` (`:7690`), which
  runs before any action (`app.rs:2218`).
* **The F1 snapshot is per market by design** (D8, `account-level-margin-f1.md:163-168`):
  each book loads a maker's start-of-batch snapshot once (`AccountMargins::load`,
  `order_book.rs:436-460`, cached per book in `makers`) and then runs its own copy. Fix 1
  memoizes only the **start value**, never the running value. That keeps behaviour
  byte-identical and leaves Fix 4 out of scope.
* **Liquidation (c) is already done.** `set_pending` (`liquidation.rs:467`) and
  `clear_cooldown` (`:496`) read first and write only when the row changes. What remains is one
  or two point reads per scanned account, not writes.
* **Liquidation results are discarded** (`app.rs:2258`, `let _ =`). Only state writes, metrics
  and `fatal_error` are observable.
* **The running state hash covers the logical change set** (`running_hash.rs`, key -> value or
  deletion, id 23 = `CF_NATIVE_LIQUIDATION`). Write order inside a block is not observable, but
  a tombstone for a key that does not exist would be. Every change below therefore keeps
  "delete only rows that exist".
* **Overlay key order** = `Vec<u8>` `Ord` = RocksDB's default bytewise comparator. No CF sets a
  custom comparator or prefix extractor (`grep set_comparator|prefix_extractor`: none).

---

## Fix 3 — range lookup in the overlay merge

### Root cause
`NativeStateOverlay::iterate_cf` (`backend.rs:1677-1699`) merges DB, parent and pending through
`PendingState::overlay_into` (`backend.rs:383-403`). For a prefix query, that function walks
**every** tombstone and **every** write of the CF in the layer and filters with
`prefix.is_none_or(|p| k.starts_with(p))` (`:393`, `:400`). `positions_for_trader`
(`position.rs:288-298`) is such a query. In the pipelined exec path the parent layer holds the
whole previous block's position writes, and pending holds this block's. Each 28-byte-key
position read therefore pays O(all position writes of two blocks). That is ~45% of each position
read in `ubench_econ`, and probably most of the devnet tail (2048 `liq_view` calls per block
against a parent layer holding a 47k-fills/s block's position writes).
`iterate_cf_from` (`:1708`) and `prefix_exists` (`:1778`) already use `BTreeMap::range`
(`writes_under`, `:1601`). Only `iterate_cf` was left linear.

### Design
1. `overlay_into` walks only `range(prefix..)` and stops at the first key without the prefix
   (`take_while`), on both `writes` (BTreeMap) and `deletes` (BTreeSet). `None` behaves as the
   empty prefix: `range(..)` and `starts_with(&[])` is always true, which is identical to
   today's full walk.
2. Fast path: if neither the parent nor the pending layer holds any key (write or tombstone)
   under the prefix, `iterate_cf` returns the DB rows directly instead of round-tripping them
   through a `BTreeMap`. The DB rows are already sorted, unique and in the same order (bytewise
   comparator).

```rust
// backend.rs, PendingState
/// Keys of this layer (writes and tombstones) under `prefix`, each in key order.
fn under<'a>(&'a self, id: CfId, prefix: &'a [u8])
    -> (impl Iterator<Item = (&'a Vec<u8>, &'a Vec<u8>)>, impl Iterator<Item = &'a Vec<u8>>)
{
    let cfp = self.cf(id);
    let r = (std::ops::Bound::Included(prefix), std::ops::Bound::Unbounded);
    (
        cfp.writes.range::<[u8], _>(r).take_while(move |(k, _)| k.starts_with(prefix)),
        cfp.deletes.range::<[u8], _>(r).take_while(move |k| k.starts_with(prefix)),
    )
}

fn touches(&self, id: CfId, prefix: &[u8]) -> bool {
    let (mut w, mut d) = self.under(id, prefix);
    w.next().is_some() || d.next().is_some()
}

/// Apply this layer's writes/tombstones (under `prefix`) on top of `merged`.
fn overlay_into(&self, id: CfId, prefix: Option<&[u8]>, merged: &mut BTreeMap<Vec<u8>, Vec<u8>>) {
    let (writes, deletes) = self.under(id, prefix.unwrap_or(&[]));
    for key in deletes {
        merged.remove(key);
    }
    for (key, value) in writes {
        merged.insert(key.clone(), value.clone());
    }
}
```

```rust
// backend.rs, NativeStateOverlay::iterate_cf (after the intern_cf early return)
let db_entries = StateBackend::iterate_cf(&self.db, cf, prefix)?;
let p = prefix.unwrap_or(&[]);
let parent = self.parent.as_ref().map(|f| &f.state);
let state = self.pending.read().unwrap();
if !state.touches(id, p) && !parent.is_some_and(|s| s.touches(id, p)) {
    return Ok(db_entries); // no layer has a key under the prefix
}
let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = db_entries.into_iter().collect();
if let Some(parent) = parent {
    parent.overlay_into(id, prefix, &mut merged);
}
state.overlay_into(id, prefix, &mut merged);
drop(state);
Ok(merged.into_iter().collect())
```
(If the `impl Iterator` tuple fights the borrow checker, inline the two `range` calls in
`overlay_into` and `touches`. `writes_under` at `:1601` already covers the writes half and is
reused.)

### Invariants
* Same rows, same order, same tombstone semantics: parent tombstones and writes apply before
  pending ones. Within a layer `writes` and `deletes` are disjoint (`put` removes the tombstone,
  `delete` removes the write), so the remove-then-insert order inside a layer is irrelevant, and
  it is kept anyway.
* `iterate_cf(cf, None)` is unchanged (full range).
* Lock order unchanged: parent (immutable `Arc`), then the `pending` read lock. The read lock is
  now taken before the parent merge. Both are read-only.

### Callers checked
Every production `iterate_cf` caller (≈40: positions, oracle, books, staking, governance, core
writer queue, precompiles) consumes the full sorted `Vec`. None relies on a limit or on
anything beyond "sorted, tombstones applied, keys under prefix". `iterate_cf_from` and
`prefix_exists` are untouched. `StateDb::iterate_cf` is untouched. Test backends
(`RecordingBackend`, `PoisonedRow`) are unaffected.

### Risks
Low. A read path only. The one subtle edge is the empty-prefix and "prefix is a full key"
cases, which the equivalence test enumerates. No state-root, replay or determinism exposure.

### Tests (first)
* **Task 3.1 RED (work count):** a `#[cfg(test)] thread_local! { static VISITED: Cell<usize> }`
  is bumped per key `overlay_into` visits. Test `overlay_prefix_scan_visits_only_the_prefix`:
  pending holds 50,000 writes and 1,000 tombstones under other prefixes, plus 3 under `p`; the
  parent layer holds the same shape. `iterate_cf(cf, Some(p))` must visit <= 6 + 6 keys. Fails on
  `c93c579` (visits ~102k). A `thread_local` keeps parallel test threads apart.
* **Task 3.2 equivalence (guard, green before and after):**
  `overlay_iterate_cf_equals_reference_over_random_layers`. A seeded LCG (no new dev-deps)
  runs 500 rounds. Each round draws keys from a small alphabet with prefixes of length 0..4,
  including keys equal to the prefix, `prefix ‖ ff…`, the successor prefix, and keys shorter
  than the prefix. Each key gets a DB state (absent/present) × parent op (none/put/delete;
  parent via `freeze`) × pending op (none/put/delete). Assert
  `iterate_cf(cf, prefix)` == a reference model (a BTreeMap applying DB, then parent, then
  pending), for `None` and for every prefix. Also assert agreement with
  `iterate_cf_from(cf, prefix, usize::MAX)` filtered to the prefix, and with `prefix_exists`.
  This extends the exhaustive layering tests at `backend.rs:1865` and `:1915`.
* Existing: `overlay_iterate_merges_correctly`, `statedb_iterate_cf_prefix`,
  `interned_overlay_stores_identical_rocksdb_keys`, every `freeze_*` test.

### Expected gain (ubench_econ)
~45% of each position read (s87 profile). Position reads dominate both the F1 excess
(~100 ms/1k) and the liquidation tail (~50 ms/1k). Expect 161 → ~95-110 ms/1k. It also helps
anything else that does prefix scans on a busy overlay.

---

## Fix 1 — one maker account snapshot per batch

### Root cause
`maker_fill_fits` (`order_book.rs:485`) → `AccountMargins::load` (`:436`) → on the first fill
of a maker in a book, `src.maker_account(maker, m)` (`native_executor.rs:802`). That reads the
balance and **all** of the maker's positions (`view` → `positions_for_trader`, `:769`) plus one
oracle read per position (`mark`, `:759`), ~1 ms at ~250 positions. `AccountMargins` is per
book (`:4334`), so a maker that fills in K markets in one batch pays K full views, all of which
return the same `free` (see the verified facts). The cost grows with positions per account
times markets per maker per batch (3.9x at 30 markets, 12.5x at 300).

### Design
1. **`BatchMakerAccounts`** (new, private, `native_executor.rs` next to `AccountReader`):
   wraps `&AccountReader` and memoizes `free` per maker for one `execute_batch` call. It
   implements `MakerAccountSource`. `signed_pos` / `px` stay per market: one `get_position`
   point read plus a mark, cheap, and they genuinely differ by market. A `OnceLock` per maker
   means concurrent workers needing the same maker wait for one computation instead of
   repeating it. The value is a pure function of the frozen backend, so it does not matter
   which worker computes it.

```rust
/// Fix 1 (s87): the makers' start-of-batch free margin (F1 D8 snapshot), computed
/// ONCE per `execute_batch` call and shared by every market worker. Sound because
/// nothing writes the backend between Phase 2 and the end of Phase 4 (reservations
/// and fills sit in the write-back caches), so `free` is the same for every market.
/// Only the START value is shared; each book still runs its own copy (D8).
struct BatchMakerAccounts<'r, 'a, T: StateBackend> {
    reader: &'r AccountReader<'a, T>,
    free: std::sync::Mutex<HashMap<Address, Arc<std::sync::OnceLock<FixedPoint>>>>,
}

impl<'r, 'a, T: StateBackend> BatchMakerAccounts<'r, 'a, T> {
    fn new(reader: &'r AccountReader<'a, T>) -> Self {
        Self { reader, free: std::sync::Mutex::new(HashMap::new()) }
    }
}

impl<T: StateBackend> MakerAccountSource for BatchMakerAccounts<'_, '_, T> {
    fn maker_account(&self, maker: &Address, market_id: MarketId) -> MakerAccount {
        let cell = self
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(*maker)
            .or_default()
            .clone();
        let free = *cell.get_or_init(|| self.reader.maker_free(maker));
        let (signed_pos, px) = self
            .reader
            .position_px(maker, market_id)
            .unwrap_or((FixedPoint::ZERO, FixedPoint::ZERO));
        MakerAccount { free, signed_pos, px }
    }
}
```
   `AccountReader::maker_account` is split so both sources share ONE formula:
```rust
/// F1 (s517 #4): a maker's snapshot free margin. A read error snapshots as 0 (deterministic).
fn maker_free(&self, maker: &Address) -> FixedPoint {
    self.positions
        .get_native_balance(maker)
        .and_then(|b| self.view(maker, &b))
        .map_or(FixedPoint::ZERO, |v| v.free())
}
// maker_account(&self, maker, m) { MakerAccount { free: self.maker_free(maker), .. as today } }
```
   Wiring at `:4365-4369`: `let makers = BatchMakerAccounts::new(&reader);` and pass
   `Some(&makers)` instead of `Some(&reader)`. The single path (`:6380`) keeps `Some(&reader)`.

2. **Per-batch mark table.** `AccountReader` gets
   `marks: Option<&'a HashMap<MarketId, Option<FixedPoint>>>`, and `mark()` consults it first:
```rust
fn mark(&self, market_id: MarketId) -> Option<FixedPoint> {
    if let Some(hit) = self.marks.and_then(|t| t.get(&market_id)) {
        return *hit;
    }
    self.oracle.get_price(market_id, self.now).ok().and_then(|p| p.usable())
}
```
   `AccountReader::of` and the single-path literal (`:6259`) pass `marks: None`. At `:4165` the
   batch builds the table before Phase 2 for the markets of this call's PlaceOrders,
   `ctx.order_books` keys and `ctx.margin_configs` keys, each through the same
   `oracle.get_price(..).usable()` expression. Any other market falls through to the direct
   read, which is correct, just not memoized. The table is filled eagerly and read without
   locks, so Phase-2 shards and Phase-3 workers never contend. It removes ~250 oracle point reads
   per view, in Phase 2's `pos_net` as well as in maker views.

3. **Optional 1c (only if the post-1 profile still shows maker views):** seed the memo from
   Phase 2's `SenderFold.pos_nets` (each sender's `pos_net`, already computed against the same
   frozen positions) as `free = backend_balance.available + pos_net`. That is the identical
   expression (`AccountView::free`, `margin.rs:182`), and a view fails for a given position set
   regardless of balance. It needs the sharded Phase 2 to return `pos_nets` alongside
   `worker_cache`. Skip unless measured (YAGNI).

### Invariants
* `BatchMakerAccounts::maker_account(x, m) == AccountReader::maker_account(x, m)` for every
  maker and market inside a batch (the frozen backend plus constant marks guarantee it).
* No change to `AccountMargins` (insert / insert_taker_only / shared / makers), the D2 pool
  computation (`:4268-4294`), `maker_fill_fits` or the `+1 raw` rounding rule. In the pool market
  `shared()` still short-circuits before the source is asked.
* The memo lives on the stack of one `execute_batch_phases` call. It is dropped before Phase 4
  flushes, so the post-EVM call, triggered stops and the single path never see it. Crash replay
  re-executes the call and rebuilds it.

### Risks
* **Staleness:** only if a future change writes the backend during Phase 3. Today's D8 snapshot
  design already assumes the same thing (`account-level-margin-f1.md:166`). The memo's doc
  comment states the dependency, and the golden test catches a violation.
* **Determinism:** the value does not depend on which thread computes it. HashMap iteration
  order is never observed (lookups only).
* **Mutex poisoning:** a worker panic is already a fail-stop (`:4370-4392`). `into_inner`
  avoids a cascading secondary panic.
* State root: unaffected.

### Tests (first)
* **Task 1.1 RED (work count):** new integration test
  `crates/torus-bridge/tests/maker_snapshot_once_tests.rs` with a `CountingBackend` wrapping
  `StateDb`. It delegates every `StateBackend` method and counts
  `iterate_cf(CF_NATIVE_POSITIONS, Some(prefix))` per prefix, plus `get_cf_raw` on
  `CF_NATIVE_ORACLE`. Maker M has positions in 20 markets and rests a bid in each of 5 markets;
  five takers each sell into one of the 5 markets in ONE `PlaceOrderBatch` (run both
  `execute_batch` and `execute_batch_engine_mode(.., 4)`). Assert: M's position prefix scans
  during the call == 1, and oracle price reads <= number of markets. `c93c579`: 5 scans, and
  oracle reads = 5 × 20 = 100+.
* **Task 1.2 equivalence (unit, in `native_executor.rs` `#[cfg(test)]`):**
  `batch_maker_accounts_equal_reader_on_random_states`. 200 seeded rounds: 1-30 traders,
  positions in 1-40 markets (long/short, random entry), random balances including negative
  `available`, marks fresh for some markets, stale for some, absent for others, and one
  overflow-sized position. For every (trader, market) pair, queried in a shuffled order from 4
  threads, assert `BatchMakerAccounts` == `AccountReader::maker_account`, and that a reader with
  the mark table gives the same `mark` / `view` / `pos_net` / `position_px` as one without.
* **Task 0.2 golden** (scenario A below) pins fills, margin cancels, `orders_rejected_cancelled`
  and the state digest per block, serial and engine-forced.
* Existing: `account_margin_tests` (D2/D8/review fixes 2 and 4), `maker_margin_release_tests`,
  `engine_parallel_tests` (sharded = serial), `parallel_matching_tests`, `order_book.rs` unit
  tests with the mock `MakerAccountSource` (`:6278`).

### Expected gain (ubench_econ)
Maker views per block drop from Σ_markets(distinct makers hit) to distinct makers. In this shape
(80 orders per market per block, 600 makers spread over 300 markets) that is roughly an order of
magnitude, and the mark table removes the per-position oracle reads. After Fix 3, expect the F1
excess (~55 ms/1k left) to fall to <= ~10 ms/1k.

---

## Fix 2 — liquidation scan

### Root cause
`liquidation_pass` (`liquidation_step.rs:55-143`) runs every native block. It walks up to
2048+2 traders from the cursor (`traders_after`, `liquidation.rs:418`: one bounded seek per
trader). For each trader it calls `liq_view` (`:149`), which loads ALL positions
(`positions_for_trader`) and, when one is marked, the balance and tiers. A healthy trader then
costs two more point reads (`clear_cooldown` + `set_pending`). With 0 liquidations this costs
~50 ms per 1k fills in the microbench and ~10 s per block on the devnet. Called from
`app.rs:2258`.

Key observation: when `marks` is empty (no listed market has a usable mark, including no
listed markets at all, as in `ubench_econ`), `liq_view` is **always** `None`: it returns `None`
when no position is in a marked market (`:155-158`). The pass then only does
`set_pending(t, false)`, the cursor and the prev-mark cleanup, yet it still loads every
scanned trader's positions to find that out. The bench tooling submits no oracle prices
(`tools/bench-throughput` has no oracle code), so this is very likely the devnet case too
(open question 1).

### Options evaluated

**(a) Skip valuation when no listed market has a mark. RECOMMENDED.**
Exact by construction: with empty `marks`, `liq_view` = `None` for every trader and for the
vault, so `liq_view` is replaced by `None` without reading. Everything else stays: the cursor
walk (same accounts, same `scanned` / `cut` / `last`, so the round-robin position is
byte-identical), `set_pending(t, false)` per scanned trader (deletes only existing rows),
`clear_cooldown` (never reached on this path, as today), `put_prev_marks` (deletes the rows of
listed markets), `put_cursor`, the vault's `set_pending`, and the same "skipped" results.
```rust
// liquidation_step.rs: one helper, used at :85, :130, :136 (and mark_pending :167)
/// Fix 2a (s87): with no usable mark in any listed market every account is
/// unvaluable (`liq_view` is `None` without a marked position), so skip the reads.
fn liq_view_if_marked<T: StateBackend>(
    ctx: &NativeExecContext<T>,
    marks: &Marks,
    trader: &Address,
) -> Result<Option<AccountView>, CoreError> {
    if marks.is_empty() {
        return Ok(None);
    }
    Self::liq_view(ctx, marks, trader)
}
```
Remaining cost per block: the `traders_after` seeks (one RocksDB iterator per trader, range
merges after Fix 3) and one pending point read per trader. Expected well under 1 ms per 1k fills
in the microbench.
Only observable difference: in a no-mark block a corrupt position row (borsh error) no longer
fail-stops the step. That is identical on every node running this binary (no divergence), and
the row still fail-stops the next reader that needs it (open question 6).
Limitation: helps only while there are no marks. A chain running the feeder pays the full scan.

**(b) Dirty traders + moved-mark markets + per-account health buffer.** Not now.
* As literally stated, "check only changed traders" changes **which** accounts are checked and
  **when**. The full scan finds an unhealthy account when the round-robin window reaches it (up
  to `N/2048` blocks later), whereas a dirty-set scan would find it in the same block. To keep
  the same liquidations in the same order, (b) has to keep the window (same cursor) and only
  *skip valuations that are provably Healthy* inside it, then still apply
  `clear_cooldown` / `set_pending(false)` for the skipped ones.
* Proving "still healthy" without loading positions needs a stored certificate per account:
  slack `S = AV − MM` at check time, gross notional `G`, and worst-case MM `W = Σ n/(2·lev_min)`.
  Tiers are a step function on the whole notional (`margin.rs:65-81`), so MM jumps at tier
  edges, and the bound has to use `lev_min` per market. The rule is: re-check when
  `ρ·G + (1+ρ)·W + n_pos ≥ AV0`, with `ρ` = the largest relative mark move since the anchor.
  Any appearance or disappearance of a mark (entry fallback) invalidates everything. Positions
  and balance changes come from the block's overlay write set (pending CF_NATIVE_POSITIONS /
  CF_NATIVE_BALANCES keys at the end of the block, after fees and epoch).
* Where to store it: **node-local** (RAM) keeps the state root unchanged and lets crash replay
  start cold, which gives identical outputs *if the bound is sound*. A soundness bug, though,
  lets a warm node skip what a cold node liquidates: a fork. **In state**
  (`CF_NATIVE_LIQUIDATION` new tag `0x07 ‖ trader`, hashed as id 23, in the native root)
  keeps every node consistent even with a bug, but adds a write per checked account per block,
  changes the root format, and reverses the owner's C1 decision ("no separate index",
  `liquidation.md:263`).
* A simpler exact variant for later: a node-local **positions mirror** (cache each trader's
  positions and balance, invalidated by the block's write set, cleared on any non-sequential
  height or restart). The valuation becomes arithmetic only, with no bound to prove. It still
  carries the warm/cold divergence risk if invalidation misses a write path.
* Decide after measuring a fed chain (`UB_MARKS=1`, Task 0.1) once 3 + 1 + 2a have landed.

**(c) Avoid no-op writes.** Already implemented (`set_pending` / `clear_cooldown` /
`put_prev_marks` / `put_cursor` write only on change). Nothing to do. A prefetch of the window's
pending rows by range would trade two point reads per trader for one range read: a micro-gain,
only if a profile shows it.

### Interaction with cursor / cooldown / pending (why 2a is exact)
* The cursor depends only on the candidate list (`traders_after`, unchanged), the vault filter,
  `scan` and `acted`. With empty marks `acted` stays 0, exactly as today: no trader classifies.
* Pending: today `set_pending(t, false)` runs for every scanned trader on the `None` branch.
  Kept verbatim.
* Cooldown: on the `None` branch no cooldown row is touched today, and none is with 2a either.
* `liquidation_due` (`:46-52`, `app.rs:1947`) reads the same rows, so the same blocks run the
  native phase.
* Crash replay and pipelined exec: 2a's branch depends only on `marks`, derived from consensus
  state at the block's timestamp, identical in every execution mode.

### Tests (first)
* **Task 2.1 RED (work count):** in `liquidation_tests.rs`, a `CountingBackend` (same wrapper as
  1.1; shared as `tests/common/counting_backend.rs`). 300 traders with positions in 3 listed
  markets without marks, pending rows for 5 of them, a cooldown row for 1, `scan = 100`. Run 4
  steps. Assert: 0 `iterate_cf(CF_NATIVE_POSITIONS, Some(_))` calls during the steps, and the
  cursor after each step == the 100th/200th/300th trader / `None`. `c93c579`: 400+ scans.
* **Task 2.2 equivalence (guard):**
  `unmarked_step_rows_equal_the_full_scan_rules`. Same fixture. After each step, the pending
  rows of exactly the scanned traders are gone, the others' are kept, the cooldown row is kept,
  prev-mark rows of listed markets are deleted, and there is no tombstone for any absent key
  (checked through the overlay's pending set after `freeze`).
* **Task 0.2 golden** scenario B (below) pins per-block `CF_NATIVE_LIQUIDATION` rows, positions
  and balances across mark-on / mark-off transitions, cursor cuts, stage-1 chunks with cooldown,
  backstop and ADL.
* Existing: all of `liquidation_tests.rs` (incl. `budgets_carry_over_through_the_cursor_round_robin`,
  `liquidation_due_reads_cooldown_and_cursor_rows`, `an_account_with_only_unmarked_positions_is_not_liquidated`,
  `the_vault_in_the_window_does_not_end_a_pass_early`), app.rs
  `liquidation_determinism_serial_pipelined_and_replay_are_identical`, chaos
  `liquidation_step_keeps_incremental_root_equal_to_full_scan`.

### Expected gain (ubench_econ)
The microbench has no listed markets, so no marks: tail ~50 ms/1k → ≤ 1-3 ms/1k (cursor walk
plus pending reads). Devnet: removes nearly all of the ~10 s/block tail if the devnet has no
feeder. Otherwise only Fix 3's share applies, and (b) becomes the topic.

---

## Shared test infrastructure (Task 0, before any engine change)

### Task 0.1 — commit `ubench_econ` as an ignored test
* File: `crates/torus-bridge/tests/ubench_econ.rs` (already `#[ignore]`). Add `UB_MARKS=1`:
  list markets 1..=M (`CF_NATIVE_MARKETS` rows) and post three-reporter oracle submissions at
  `TARGET*LEV` with the block's timestamp before `begin_block_oracle`, so the fed-chain
  liquidation cost is measurable. Default stays unmarked (comparable to the s87 numbers).
  `ubench_probe.rs` stays uncommitted unless the owner wants it (Q4).
* Verify: `CARGO_TARGET_DIR=/home/18c/.cargo-target-s87-ubench UB_RUNS=3 cargo test -p torus-bridge --release --test ubench_econ -- --ignored --nocapture`
  on `c93c579`. Record the baseline (expect ~161 ms/1k) in the commit message.

### Task 0.2 — golden equivalence test, pinned on c93c579
* File: `crates/torus-bridge/tests/perf_equivalence_golden.rs`. Not ignored; small (<5 s
  debug).
* Scenario A (Fix 1 / 3): 40 senders × 12 listed markets, econ generator (copied LCG), balances
  sized so that some makers hit `marginCanceled` and some senders are pool / taker-only in
  different markets, 12 blocks, block-by-block through `NativeStateOverlay::with_parent` +
  `freeze` + `flush` exactly like ubench. Run once with `execute_batch` and once with
  `execute_batch_engine_mode(.., 4)`.
* Scenario B (Fix 2 / 3): 10 traders with positions in 3 markets, `run_liquidations_with(ctx, 3, 2)`
  (cursor cuts). Blocks 1-4 without marks, with pending and cooldown rows pre-seeded. Blocks 5-12
  with a falling mark on markets 1-2 (stage 1 with chunks > 100k notional and a cooldown,
  backstop, ADL of a trader and of the vault). Blocks 13-16 with marks stale (timestamp jump
  > 60 s) while pending and cooldown rows exist. Blocks 17-18 with marks back.
* Digest per block = keccak256 over (all `HASHED_CFS` native CFs incl. `CF_NATIVE_LIQUIDATION`,
  sorted dumps) ‖ `format!("{:?}", results)` ‖ metrics (`orders_rejected_cancelled`,
  `orders_placed_accepted`, margin cancels, `liquidations_triggered`) ‖ `ctx.trade_index`.
* Procedure: write the test with `GOLDEN_PRINT=1` printing digests, run it on `c93c579`, paste
  the constants into `const GOLDEN_A_SERIAL/A_ENGINE/B: [&str; N]`, and commit. The test then
  asserts equality. It is green on `c93c579` by construction and must stay green after every
  fix commit. It is the old-path vs new-path oracle, so no dead "old path" code stays in the
  engine.

---

## Order of work and commit plan

Recommended order: **0 → 3 → 2a → 1**.
* **3 first:** it is the smallest and lowest-risk change, and both other fixes read positions
  through `iterate_cf(prefix)`. Measuring 1 and 2 after 3 attributes their gains honestly.
* **2a second:** a few lines, exact by construction, and it removes the whole microbench tail
  and (very likely) the devnet's ~500 s `post_engine_tail`. That gives the earliest devnet signal.
* **1 last:** the most intricate change (shared memo across market worker threads in the hot
  matching path). It gets the most review attention on a clean baseline.
(3 → 1 → 2 is equally valid. The fixes are independent and each commit is measurable on its own.)

| # | Commit (one fix per test + impl pair) | Files |
|---|---|---|
| C0 | `test(bench): ubench_econ as an ignored test; golden perf-equivalence digests pinned on c93c579` | `tests/ubench_econ.rs`, `tests/perf_equivalence_golden.rs`, `tests/common/counting_backend.rs` |
| C1 | `perf(state): overlay prefix iteration walks only the prefix range` (Tasks 3.1, 3.2 and impl) | `crates/torus-state/src/backend.rs` |
| C2 | `perf(liquidation): no valuation reads when no listed market has a mark` (Tasks 2.1, 2.2 and impl) | `crates/torus-bridge/src/liquidation_step.rs`, `tests/liquidation_tests.rs` |
| C3 | `perf(exec): one maker account snapshot and one mark table per batch` (Tasks 1.1, 1.2 and impl) | `crates/torus-bridge/src/native_executor.rs`, `tests/maker_snapshot_once_tests.rs` |
| C4 | `docs(perf): s87 crab perf fixes — ubench before/after` | `docs/perf/…`, this plan's status line |

Within each pair the RED test is written and run first (it fails on the previous commit), then
the implementation. Each commit message records the ubench line before and after. No push.
Each commit ends with the attribution line given in the session's instructions.

---

## Verification

1. Per commit: `cargo test -p torus-state` / `-p torus-bridge` (targeted) plus the golden test.
2. After C3: full workspace, in the background (it takes more than 4 minutes):
   `CARGO_TARGET_DIR=/home/18c/.cargo-target-s87-ubench cargo test --workspace > /home/18c/.cargo-target-s87-ubench/ws-test.log 2>&1`
   plus `cargo clippy --workspace --all-targets -- -D warnings`.
3. ubench_econ, median of 3 runs, quiet box, columns `engine_ms/1k [margin match settle tail]`:

| After | Target ms/1k fills | Fail if | Note |
|---|---|---|---|
| c93c579 (baseline) | 161 | — | main 12.8 |
| C1 (Fix 3) | ≤ 110 | > 125 | match/1k and tail/1k both drop |
| C2 (Fix 2a) | tail/1k ≤ 3 | tail/1k > 5 | total ≈ C1 − ~25-45 |
| C3 (Fix 1) | ≤ 25 (≤ 2× main) | > 35 | stretch ≤ 19 (1.5×) via 1c |
| `UB_MARKS=1` at C3 | report only | — | input for the (b) decision |

   These are estimates from the s87 profile shares, not promises. If a target misses, profile
   (`perf record` on the ubench) before adding anything.
4. Devnet cell (owner launches), same shape as `s87-crab-stack-r0`: compare `phase_match`,
   `post_engine_tail`, `margin` and `settle` against r0 and main.
5. Two-round paired bench against main `d995f68` (owner's standard procedure), reporting
   matched/s and `rejected_cancelled`. The latter is expected to stay ~19%: it is the F1 D2
   design effect and belongs to Fix 4, not to these fixes.

## Rollback
Each fix is one self-contained commit with no state-format change. Reverting the commit restores
`c93c579` behaviour exactly, and no migration or genesis change is involved.

## Open questions for the owner
1. **Does the devnet bench run `price-feeder` (marks present)?** `bench-throughput` submits no
   oracle prices, and s87 saw 0 liquidations, so I assume no. If no, 2a removes the tail. If yes,
   2a does nothing there and (b) is needed.
2. **(b) for fed chains:** keep the full scan (C1 decision), a node-local positions mirror or
   health certificate (state root unchanged, fork risk on a soundness bug), or in-state
   certificates (`CF_NATIVE_LIQUIDATION` tag `0x07`, hashed id 23, root-format change, one write
   per checked account, reverses C1)?
3. OK to pin golden digests captured on `c93c579` as constants (they need re-capturing whenever
   behaviour is changed on purpose, e.g. Fix 4)?
4. Commit `ubench_probe.rs` too, or only `ubench_econ.rs`?
5. Optional 1c (seed the maker memo from Phase-2 `pos_nets`): only if C3 misses its target. Agree?
6. 2a: a corrupt position row no longer fail-stops a no-mark liquidation step (later readers
   still do). Acceptable?
7. Where do the commits go: directly onto `rebase/s85-oracle-feeder`, or onto a new branch
   (e.g. `perf/crab-s87`) from `c93c579`? The worktree is detached.
8. Devnet acceptance threshold (e.g. matched/s ≥ X% of main) for calling the stack done
   before Fix 4?
