# Implementation Plan: Hyperliquid-style liquidation (item 3, option B)

Design: `docs/plans/liquidation.md` (binding decisions 1-10, defaults D1-D11 decided
(user, s517), C1 (no separate index) and C4 (fix user CancelAll) decided (user, s517), known
limitations C3 / C5). Branch `feat/liquidation` @ `d4fe69c`, stacked on
`feat/oracle-aggregation`. Consensus-visible: lockstep upgrade + fresh genesis.

Tasks are TDD: failing test first (a missing API = RED by compile error), implement, run
`validate`. Only the lead runs cargo, serialized, `-j6`, through the wrapper:

```
/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests
```

Every cargo command below is spelled with this wrapper (flock-serialized cargo); never call
`cargo` directly.

Line numbers verified at `d4fe69c` (NE = `crates/torus-bridge/src/native_executor.rs`).

## Verified anchors

| What | Where |
|------|-------|
| `run_liquidation_checks` (no caller; HashMap snapshot; one price for all markets; ADL `let _ =`) | NE:6901-6966 |
| `margin_configs: HashMap::new()` — only constructor body; every constructor funnels into `new_with_mode` | NE:1962; `new` :1715, `new_with_book_rows` :1745, `new_env` :1777, `new_with_modes` :1808, `new_with_mode` :1846 |
| `AccountReader` (`of`, `mark` usable rule, `tiers`, `view`, `position_px`) | NE:441-504 |
| `exec_place_order` → `place_order_inner` + `run_triggered_stops` | NE:5496-5505, 5516-5541, 5546-5838 |
| reduce-only: no match-time budget, no account check | NE:5198-5201 (`match_margin_checked`), :5636 (`needs_account`) |
| `try_reserve_for_qty_cfg`, `margin_tiers`, `release_order_margin` | NE:5041, 5227, 5480 |
| `exec_cancel_all` (HashMap key order for `None`; order-independent sum) | NE:5914-5970 |
| `drain_core_writer`, `oracle_due`, `begin_block_oracle` | NE:6788, 6778, 6848 |
| child-module pattern `#[path = "…"] mod …;` (child sees private items) | NE:209-214 |
| `MarginTier`, `default_margin_tiers`, `effective_max_leverage`, `DEFAULT_ORDER_MAX_LEVERAGE` = 20, `order_initial_margin` | `margin.rs:17-62, 65-75` |
| `AccountView` (build / equity / pos_net / free / transfer_required) | `margin.rs:117-196` |
| `MarketMarginConfig` (`maintenance_factor_bps` 5000) | `margin.rs:203-224` |
| `MarginEngine::total_maintenance_margin` — one config's tiers for all positions | `margin.rs:336-353` (:352); test-only users (`margin_tests.rs:580`, `check_initial_margin` :278) |
| `LiquidationEngine` (check / execute = delete :230 / ADL sort :271 / socialize / insurance key :57-60, :433-450) + 4 module tests | `liquidation.rs:68-450, 452-662` |
| `PositionManager::put_position` / `delete_position` / `positions_for_trader` / `apply_fill` / `PositionCache::flush_all` | `position.rs:243-251, 253-257, 260-271, 309-329, 551-566` |
| the single batch flush of positions | NE:4100 |
| `OrderBook::cancel_all` (early return :1339-1342; stops `retain` :1345, :1389), `pending_stops`, `pending_stop_count`, `lot_size` (pub), dust rule `qty < lot` | `order_book.rs:1337-1392, 664, 1585, 666, 839-842` |
| `StopOrder { price_cap, limit_price, quantity, trader, … }` | `order_book.rs:901-913` |
| `NATIVE_ROOT_CFS` (6, frozen tags), `cf_tag`, `leaves_from_db` | `torus-state/src/native_trie.rs:47-62, 198-215` |
| `compute_native_state_root` CF list | `torus-bridge/src/state_root.rs:171-200` |
| `ALL_CF_NAMES` / `CfId` index | `torus-state/src/cf.rs:166-212`; `backend.rs:226-256` |
| per-tag arrays `[_; 6]` | `backend.rs:974, 978, 1118`; `torus-telemetry/src/lib.rs:276, 1350-1360` |
| market row borsh layout (base, quote, lot, tick, initial_margin percent) | `governance.rs:1107-1119`; `torus-genesis/src/lib.rs:189-206, 424-446`; listing IM = 100 / max_leverage NE:6604-6612 |
| `GovernanceManager::listed_market_ids` (8-byte keys ascending) | `governance.rs:1464-1474` |
| pipeline: gate `run_native` :1983-1998 (oracle due :1987); `begin_block_oracle` :2227; batches; fatal check :2235-2251; `drain_core_writer` :2263; `process_governance` :2264 | `torus-consensus/src/app.rs` |
| app.rs test helpers: `make_test_config_and_db` :7125, `make_block` :7157 (ts = 1000 + h), `make_exec_ctx` :10312, `link_blocks` :12568, `dispatch_and_execute` :12599, `dump_all_cfs` / `assert_dumps_equal` :12608-12620, `persist_committed_block_durably` :968, `read_native_applied_height` :624; oracle helpers `oracle_key/addr` :14346-14354, `oracle_fixture_db` :14375, `px` :14386, `oracle_sub_rows` :14418, `OracleRun` / `run_oracle_fixture` :14536-14598 | |
| chaos incremental-vs-full root tests (manual pipeline) | `torus-integration-tests/tests/chaos.rs:321-378, 380-440` |
| bridge test helpers to copy: `ctx_at` (ts 1000 + h) `oracle_block_tests.rs:31-35`, `list_market` :59-61; `set_mark` `account_margin_tests.rs:165-180`; `limit` / `market` :68-92; stop helper `market_order_margin_tests.rs:105-116` | |

## Semantics (exact)

See the design. In short, per block after `drain_core_writer`:
marks of listed markets (usable only) → distinct traders of `CF_NATIVE_POSITIONS` (keys
`trader ‖ market`, ascending) after the round-robin cursor (vault excluded, budgets `SCAN` /
`ACT`, carry-over) → per account: valuation only if EVERY position has a mark →
`classify`: Healthy / ADL (`AV < 0`) / Backstop (`3·AV < 2·MM`) / Stage 1 → cancel orders +
stops → act → flat deficit to the vault → vault ADL if its AV < 0 → write previous marks and
cursor. MM = `order_initial_margin(tiers, notional).raw() / 2` per position.

## Success criteria (each is a named test below)

1. **MM per market** — half the position-size-tier IM, each market's own tiers (T1).
2. **Root coverage** — `CF_NATIVE_LIQUIDATION` is native-root tag 6 in both the incremental and
   the full root; tags 0-5 unchanged (T2).
3. **User CancelAll** — also cancels pending stops and releases their reservation, with and
   without resting orders, on every placement path, and the removal survives save + reload (T3).
4. **Margin configs** — loaded from market rows; a 20x row is byte-identical to no config (T4).
5. **Penalty gone** — no insurance key, no `socialize_loss`, no deleting liquidation (T5).
6. **Stage 1** — reduce-only market order into the book, trader keeps the rest; stops once
   MM is met; chunks of 20% above 100k with a 30 s block-time cooldown; cancels resting orders
   AND pending stops with their reservations (T7a, T7b).
7. **Backstop** — positions + collateral to the vault at the mark, also during a cooldown (T6, T7b, T7c).
8. **ADL** — HL ranking (exact), at the previous mark; vault ADL; flat deficit to the vault (T5, T6, T7d).
9. **Skip rule** — a stale / absent mark in ANY of the account's markets ⇒ nothing happens (T7a).
10. **Invariants** — Σ long == Σ short per market and Σ value conserved in every scenario (T6, T7*).
11. **Budgets / carry-over / due** — round-robin cursor; a cooldown or cursor row alone makes an
    empty block run the native phase (T7e, T8).
12. **Wiring** — runs at the end of the block on the block-start mark (T8).
13. **Determinism** — serial = pipelined (parked) = crash replay; incremental root = full (T9).
14. **Nothing else moves** — existing suites green except the removed penalty tests (recount T11).

## Where determinism could break, and how this plan keeps it

| Hazard | Guard |
|--------|-------|
| HashMap iteration (`margin_configs`, `order_books`, `PositionCache`) | Liquidation never iterates a HashMap: markets from `listed_market_ids()` (ascending) into a `BTreeMap`; order books visited by SORTED market id; `margin_configs` only point lookups; `flush_all` already sorts. The old `run_liquidation_checks` (HashMap snapshot) is deleted. |
| Candidate order | Walk of `CF_NATIVE_POSITIONS` (root CF, key `trader ‖ market`) via `iterate_cf` (DB + parent layer + pending, BTreeMap merge) ⇒ ascending traders; consecutive keys deduplicated; cursor row in `CF_NATIVE_LIQUIDATION` (state); vault excluded by constant. |
| Within-account order | Stage 1: `(MM desc, market asc)`; backstop / ADL: positions ascending market; ADL counterparties: `CF_NATIVE_POSITIONS` key order then exact rank, ties by address. |
| Mark / time | Block-start aggregate via `AccountReader::mark` (only `begin_block_oracle` writes it); cooldown on the COMMITTED header timestamp, `saturating_sub`. Missing mark ⇒ skip (never entry). |
| Arithmetic | i128 `checked_*` (overflow ⇒ skip the account with an error result, never panic); `3·AV < 2·MM` on raw i128; ADL ranking by U512 cross-multiplication (exact); chunk `raw / 5`; cap `mark ∓ mark / (2·lev)` with `lev >= 1`. `FixedPoint` operators (`+ - * /`, which panic) are not used on unbounded inputs. |
| Partial failure | No per-action rollback (NE:3291): a storage error in the step ⇒ `fatal_error` (fail-stop, new check after the step, F11); a rejected liquidation order is a result, the next block retries. |
| Whether the step runs | Same `run_native` flag as the oracle; `liquidation_due` reads the block's OVERLAY (DB + pipelined parent layer), never `self.state_db` alone; read error ⇒ fail-stop. |
| Stops fired by liquidation fills | FIFO `VecDeque` through `run_triggered_stops` (each stop fires once — bounded). |
| Margin configs from off-root `CF_NATIVE_MARKETS` | Same source and exposure as governance ids / oracle markets (C6); undecodable rows deterministic `None`; read error ⇒ fatal. |
| Crash replay / pipelining | Same function, same header, layered reads (T9). |
| EVM | EVM runs before the native phase; precompile readers see liquidations from the next block. Nothing to guard; documented. |

---

## Tasks

### Commit boundaries

| Commit | Tasks | Content |
|--------|-------|---------|
| 1 | T1 | `maintenance_margin`, `AccountView::maintenance`, per-market `total_maintenance_margin` |
| 2 | T2 | `CF_NATIVE_LIQUIDATION` native-root tag 6 |
| 3 | T3 | user `CancelAll` also cancels pending stops + releases their margin (C4, own commit) |
| 4 | T4 | margin configs from market rows |
| 5 | T5 + T6 | core engine rewrite: penalty / insurance / socialize removed; classify, chunk, cap, ADL rank, transfer primitives |
| 6 | T7a-T7e | bridge step `run_liquidations` (+ `liquidation_due`, `take_pending_stops`) |
| 7 | T8 | consensus wiring + gate + fatal check |
| 8 | T9 | determinism gates |
| 9 | T10 | docs |

### T1 — core: maintenance margin per market (`margin.rs`) · depends_on: []

**Test first** — append to `crates/torus-core/tests/margin_tests.rs` (add `maintenance_margin`
to the `use torus_core::margin::{…}` list):

```rust
/// Item 3: MM = half the IM at max leverage, POSITION-size tier, each market's
/// own tiers (truncating).
#[test]
fn maintenance_is_half_the_position_size_tier_im_per_market() {
    let t = tiers_20_then_5();
    assert_eq!(maintenance_margin(Some(&t), fp(1_000)), fp(25)); // 20x: IM 50
    assert_eq!(maintenance_margin(Some(&t), fp(1_500)), fp(150)); // 5x: IM 300
    assert_eq!(maintenance_margin(None, fp(1_000)), fp(25)); // default 20x
    assert_eq!(maintenance_margin(None, FixedPoint::from_raw(39)).raw(), 0); // IM raw 1 -> 0
    let bal = NativeBalance { available: fp(1_000), order_margin: FixedPoint::ZERO };
    // m1: tiers_20_then_5, 15 @ 100 = 1,500 -> 5x, MM 150; m2: no tiers, 10 @ 100 -> MM 25.
    let ps = [cross(1, true, 15, 100), cross(2, false, 10, 100)];
    let v = AccountView::build(&bal, &ps, |_| None, |m| (m == 1).then_some(t.as_slice())).unwrap();
    assert_eq!(v.maintenance, fp(175));
}

/// F6: total_maintenance_margin uses EACH market's tiers (was: one config's
/// tiers for every position).
#[test]
fn total_maintenance_margin_uses_each_markets_tiers() {
    let (_dir, pm) = setup();
    let trader = addr(1);
    pm.put_position(&Position { trader, ..cross(1, true, 15, 100) }).unwrap();
    pm.put_position(&Position { trader, ..cross(2, true, 15, 100) }).unwrap();
    let t = tiers_20_then_5();
    let m = MarginEngine::total_maintenance_margin(
        &pm,
        &trader,
        |m| (m == 1).then_some(t.as_slice()),
        &[],
    )
    .unwrap();
    // m1: 1,500 at 5x -> MM 150; m2: 1,500 at the default 20x -> MM 37.5
    assert_eq!(m, fp(150) + FixedPoint::from_raw(3_750_000_000));
}
```

and update `maintenance_uses_entry_price_without_a_mark` (:571-582) to the new signature:
`MarginEngine::total_maintenance_margin(&pm, &trader, |_| Some(config.tiers.as_slice()), &[])`
(value unchanged: 500).

**Implementation (`crates/torus-core/src/margin.rs`):**

```rust
/// Item 3 (HL): maintenance margin = half the initial margin at max leverage
/// (position-size tier), truncating — THE formula of liquidation.
pub fn maintenance_margin(tiers: Option<&[MarginTier]>, notional: FixedPoint) -> FixedPoint {
    FixedPoint::from_raw(order_initial_margin(tiers, notional).raw() / 2)
}
```

* `AccountView` gains `pub maintenance: FixedPoint` (Σ `maintenance_margin(tiers(m), n)`,
  `checked_add`, in the same loop as `position_im`, :155-163). No struct literal exists outside
  `build` (verified).
* `total_maintenance_margin<'t>(positions, trader, tiers: impl Fn(MarketId) -> Option<&'t
  [MarginTier]>, oracle_prices)` = `AccountView::build(&bal, &ps, |m| oracle_price_for(…),
  tiers)?.maintenance`. `check_initial_margin` (:278) passes `|_| Some(config.tiers.as_slice())`.
  `maintenance_factor_bps` is no longer read by it (only `margin.rs` / `liquidation.rs` reference
  the field — verified); keep the field (public API, C3).

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --test margin_tests` · depends_on: []

`══ COMMIT 1 ══` `feat(margin): maintenance margin = half IM, per-market tiers (liquidation T1)`

### T2 — state: `CF_NATIVE_LIQUIDATION` = native-root tag 6 · depends_on: []

**Test first:**

(a) `crates/torus-state/src/native_trie.rs`, tests module (uses `tempfile` + `StateDb` like the
module's other tests):

```rust
    /// Item 3: the liquidation CF is native-root tag 6 (0-5 frozen and
    /// unchanged); its rows move the incremental AND the full root, and an
    /// empty CF contributes nothing.
    #[test]
    fn liquidation_cf_is_native_root_tag_6_incremental_equals_full() {
        use crate::cf::CF_NATIVE_LIQUIDATION;
        assert_eq!(cf_tag(CF_NATIVE_LIQUIDATION), Some(6));
        let tags: Vec<u8> = NATIVE_ROOT_CFS.iter().map(|(_, t)| *t).collect();
        assert_eq!(tags, vec![0, 1, 2, 3, 4, 5, 6]);
        let dir = tempfile::tempdir().unwrap();
        let db = StateDb::open(dir.path()).unwrap();
        build_native_trie_to_cf(&db).unwrap();
        let empty = persisted_native_root(&db).unwrap();
        let key = [0x02u8; 21];
        let ov = crate::NativeStateOverlay::new(db.clone());
        crate::StateBackend::put_cf_raw(&ov, CF_NATIVE_LIQUIDATION, &key, &1_001u64.to_be_bytes()).unwrap();
        ov.flush_with_native_trie(&db).unwrap();
        let root = persisted_native_root(&db).unwrap();
        assert_ne!(root, empty, "a liquidation row moves the native root");
        assert_eq!(root, native_root_full(&db).unwrap());
        let ov = crate::NativeStateOverlay::new(db.clone());
        crate::StateBackend::delete_cf_raw(&ov, CF_NATIVE_LIQUIDATION, &key).unwrap();
        ov.flush_with_native_trie(&db).unwrap();
        assert_eq!(persisted_native_root(&db).unwrap(), empty);
        assert_eq!(native_root_full(&db).unwrap(), empty);
    }
```

(b) `crates/torus-bridge/src/state_root.rs` tests (create `#[cfg(test)] mod` if absent):

```rust
    /// Item 3: the consensus full-scan root covers the liquidation CF.
    #[test]
    fn full_native_root_covers_the_liquidation_cf() {
        let dir = tempfile::tempdir().unwrap();
        let db = torus_state::StateDb::open(dir.path()).unwrap();
        let before = compute_native_state_root(&db).unwrap();
        db.put_cf_raw(torus_state::cf::CF_NATIVE_LIQUIDATION, &[0x02; 21], &1_001u64.to_be_bytes())
            .unwrap();
        assert_ne!(compute_native_state_root(&db).unwrap(), before);
    }
```

**Implementation:**
* `cf.rs`: `pub const CF_NATIVE_LIQUIDATION: &str = "cf_native_liquidation";` with the row-layout
  doc (design table); APPEND to `ALL_CF_NAMES` after `CF_BOOK_ORDER_ROWS` (keeps every `CfId`).
* `native_trie.rs:47`: `NATIVE_ROOT_CFS: [(&str, u8); 7]`, append `(CF_NATIVE_LIQUIDATION, 6)`;
  fix "6 native-root CFs" comments (:56, :198, cf.rs:161).
* `state_root.rs:174-190`: import + append to the CF list (order = tag order).
* `backend.rs:974, 1118`: `[_; 7]`; telemetry `lib.rs:276, 1350`: `[Counter; 7]` + suffix
  `"liquidation"`.
* Verify `StateDb::open` creates missing CFs (`create_missing_column_families`); otherwise
  existing data dirs fail to open — fresh genesis anyway.
* Grep for hard-coded native-root values / dirty-entry counts in tests (`state_root_once_tests`,
  `root_cache_tests`, `level_rows_tests.rs:1090`) — only rows of tag 6 can change them; they
  appear only when the liquidation step runs (T7+), never from ordinary trading (no index, C1).

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-state --lib native_trie && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --lib state_root && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh check --workspace --all-targets` · depends_on: []

**Correction s517 (T2):** `native_trie.rs`'s CHAR-PIN test (`charpin_corpus`) builds its
corpus over every `NATIVE_ROOT_CFS` entry and asserts `6 × …` entries plus pinned roots; with
tag 6 appended its shape changed. The corpus is now `NATIVE_ROOT_CFS.iter().take(6)` (tags 0-5)
so the pins still prove that the 0-5 preimages did not move; tag 6 is covered by
`liquidation_cf_is_native_root_tag_6_incremental_equals_full`.

`══ COMMIT 2 ══` `feat(state): native-root CF for liquidation state (tag 6)`

### T3 — book + exec: user `CancelAll` cancels pending stops and releases their margin (C4, own commit) · depends_on: []

Decided (user, s517): fix F9 for users, own commit, failing test first. Today
`OrderBook::cancel_all` (`order_book.rs:1337-1342`) returns early when the trader has no
resting order (stops survive), and when it does drop them (`retain` :1345 / :1389) their
reservation is not released (stops are not in the returned `Vec<Order>`).

**Test first** — append to `crates/torus-bridge/tests/market_order_margin_tests.rs` (its
`fresh`, `run`, `PATHS`, `place`, `limit`, `stop_market`, `assert_bal`, `make_ctx`):

```rust
/// C4 (s517, liquidation plan T3): CancelAll also cancels the sender's PENDING
/// STOPS and releases their reservation — without a resting order (the book
/// returned early: stops survived) and with one (stops dropped, margin leaked).
/// Two consecutive CancelAlls = the batched `exec_cancel_all_run` path. The
/// removal survives save + reload.
#[test]
fn cancel_all_cancels_pending_stops_and_releases_their_margin() {
    for path in PATHS {
        for with_resting in [false, true] {
            for target in [None, Some(1)] {
                let t = addr(1);
                let what = format!("{path:?} resting={with_resting} target={target:?}");
                let (_d, mut ctx) = fresh(path, &[t, addr(2)]);
                // stop buy 1, trigger 110, cap 120 -> reserves 6 (20x); bid 1 @ 90 -> 4.5
                let mut acts = vec![place(t, stop_market(1, true, 110, fp(120), 1))];
                if with_resting {
                    acts.push(place(t, limit(1, true, 90, 1)));
                }
                assert!(run(&mut ctx, path, &acts).iter().all(|r| r.success), "{what}");
                assert!(bal(&ctx, &t).order_margin > FixedPoint::ZERO, "{what}: reserved");
                let r = run(
                    &mut ctx,
                    path,
                    &[
                        (t, NativeAction::CancelAllOrders { market_id: target }),
                        (addr(2), NativeAction::CancelAllOrders { market_id: None }),
                    ],
                );
                assert!(r.iter().all(|r| r.success), "{what}");
                assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "{what}");
                assert_bal(&ctx, &t, fp(FUNDING), FixedPoint::ZERO, &what);
                ctx.save_order_books();
                let reloaded = make_ctx(ctx.state.clone());
                assert_eq!(
                    reloaded.order_books.get(&1).map_or(0, |b| b.pending_stop_count()),
                    0,
                    "{what}: persisted"
                );
            }
        }
    }
}
```

**Implementation:**
* `order_book.rs`: `pub fn take_pending_stops(&mut self, trader: &Address) -> Vec<StopOrder>`
  — the same `pending_stops` mutation as `cancel_all`'s `retain` (order kept, id-ascending
  invariant :2237 holds), returning the removed stops.
* NE: `fn stop_reservation(cfg: Option<&MarketMarginConfig>, s: &StopOrder) -> FixedPoint` =
  `try_reserve_for_qty_cfg(cfg, s.limit_price.unwrap_or(s.price_cap), s.quantity)
  .unwrap_or(ZERO)` — exactly what `run_triggered_stops` releases at trigger (NE:5522-5528).
* `exec_cancel_all` (NE:5914-5970, both branches) and `exec_cancel_all_run` (:5984-6040): per
  market, FIRST `take_pending_stops(sender)`, THEN `cancel_all` / `cancel_all_many`; a market with
  removed stops is dirty; add Σ `stop_reservation` to `total_margin_release` (same
  `min(order_margin)` release). Market iteration stays as today (order-independent exact sum);
  the run path keeps the per-action release order.
* Extract `fn cancel_orders_and_stops(ctx, trader, market: Option<MarketId>) -> FixedPoint`
  (release amount) shared with the liquidation step (T7a calls it with `None`, sorted markets).
* Client-visible: CancelAll now also cancels stops (documented in T10).

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test market_order_margin_tests && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --lib cancel_batch && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --lib order_book` · depends_on: []

**Correction s517 (T3):** `StopOrder` and its fields are private to `order_book.rs`, so
`take_pending_stops` returns each removed stop's reservation inputs `(price, quantity)` (price =
limit, else cap — what `trigger_stops` hands to the placement) instead of `Vec<StopOrder>`;
`stop_reservation(cfg, price, qty)` takes them. `cancel_orders_and_stops(ctx, trader, None)`
always visits the markets SORTED (the user path's sum was order-independent anyway — one code
path for the user and the liquidation step). In the run path a market is dirty when an action
took a stop even if its reservation was 0 (overflowing legacy row), so the removal persists.

`══ COMMIT 3 ══` `fix(exec): CancelAll also cancels pending stops and releases their margin`

### T4 — bridge: margin configs from market rows (F2, F8, D11) · depends_on: [1]

**Test first** — new `crates/torus-bridge/tests/liquidation_tests.rs` (this task creates the
file with the shared helpers; T7 appends):

```rust
//! Item 3: Hyperliquid-style liquidation — `docs/plans/liquidation.md`,
//! `docs/plans/liquidation-impl.md`. Helpers copied from oracle_block_tests.rs,
//! account_margin_tests.rs and market_order_margin_tests.rs.

use std::collections::BTreeMap;

use alloy_primitives::Address;

use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::liquidation::LIQUIDATOR_VAULT;
use torus_core::position::{MarginType, NativeBalance, Position};
use torus_state::cf::{CF_NATIVE_BALANCES, CF_NATIVE_LIQUIDATION, CF_NATIVE_MARKETS, CF_NATIVE_POSITIONS};
use torus_state::{StateBackend, StateDb};
use torus_types::{FixedPoint, MarketId, NativeAction, OrderType, PlaceOrderParams, TimeInForce};

fn open_test_db() -> (tempfile::TempDir, StateDb) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let db = StateDb::open(dir.path()).expect("open db");
    (dir, db)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

/// Block `height`, timestamp `1_000 + height` (one second per block).
fn ctx_at(db: StateDb, height: u64) -> NativeExecContext {
    NativeExecContext::new(db, height, 1_000 + height, 0, 1_000, 10, addr(99), addr(100), addr(101))
}

/// Markets listed with the test-fixture row (undecodable ⇒ default 20x config).
fn liq_db(markets: &[MarketId]) -> (tempfile::TempDir, StateDb) {
    let (dir, db) = open_test_db();
    for m in markets {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), b"listed").unwrap();
    }
    (dir, db)
}

fn market_row(initial_margin: FixedPoint) -> Vec<u8> {
    borsh::to_vec(&("BTC".to_string(), "USDC".to_string(), fp(1).raw(), fp(1).raw(), initial_margin.raw()))
        .unwrap()
}

fn fund(ctx: &NativeExecContext, t: &Address, amount: FixedPoint) {
    ctx.positions
        .put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn bal(ctx: &NativeExecContext, t: &Address) -> NativeBalance {
    ctx.positions.get_native_balance(t).unwrap()
}

/// (available, order_margin) — `NativeBalance` has no `PartialEq`.
fn ab(ctx: &NativeExecContext, t: &Address) -> (FixedPoint, FixedPoint) {
    let b = bal(ctx, t);
    (b.available, b.order_margin)
}

/// `long` buys / `short` sells `qty` at `px` — a matched pair (OI stays symmetric).
fn open_pair(ctx: &NativeExecContext, long: &Address, short: &Address, m: MarketId, qty: i64, px: i64) {
    ctx.positions.apply_fill(long, m, true, fp(qty), fp(px), MarginType::Cross).unwrap();
    ctx.positions.apply_fill(short, m, false, fp(qty), fp(px), MarginType::Cross).unwrap();
}

fn pos(ctx: &NativeExecContext, t: &Address, m: MarketId) -> FixedPoint {
    match ctx.positions.get_position(t, m).unwrap() {
        Some(p) if p.is_long => p.size,
        Some(p) => -p.size,
        None => FixedPoint::ZERO,
    }
}

fn all_positions(ctx: &NativeExecContext) -> Vec<Position> {
    ctx.state
        .iterate_cf(CF_NATIVE_POSITIONS, None)
        .unwrap()
        .into_iter()
        .map(|(_, v)| borsh::from_slice(&v).unwrap())
        .collect()
}

/// (Σ long size, Σ short size) in market `m`.
fn oi(ctx: &NativeExecContext, m: MarketId) -> (FixedPoint, FixedPoint) {
    let (mut l, mut s) = (FixedPoint::ZERO, FixedPoint::ZERO);
    for p in all_positions(ctx).into_iter().filter(|p| p.market_id == m) {
        if p.is_long { l += p.size } else { s += p.size }
    }
    (l, s)
}

/// Σ over every balance row (available + order margin) + Σ UPnL at `marks`.
fn total_value(ctx: &NativeExecContext, marks: &BTreeMap<MarketId, FixedPoint>) -> FixedPoint {
    let mut v = FixedPoint::ZERO;
    for (k, b) in ctx.state.iterate_cf(CF_NATIVE_BALANCES, None).unwrap() {
        if k.len() == 20 {
            let b: NativeBalance = borsh::from_slice(&b).unwrap();
            v += b.available + b.order_margin;
        }
    }
    for p in all_positions(ctx) {
        v += p.unrealized_pnl(marks[&p.market_id]);
    }
    v
}

/// Distinct traders with position rows, ascending — the candidate walk's input.
fn traders(ctx: &NativeExecContext) -> Vec<Address> {
    let mut v: Vec<Address> = all_positions(ctx).iter().map(|p| p.trader).collect();
    v.dedup();
    v
}

fn liq_rows(ctx: &NativeExecContext, tag: u8) -> Vec<(Vec<u8>, Vec<u8>)> {
    ctx.state.iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[tag])).unwrap()
}

fn limit(m: MarketId, is_buy: bool, price: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(price),
        quantity: fp(qty),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn stop_market(m: MarketId, is_buy: bool, trigger: i64, cap: i64, qty: i64, reduce_only: bool) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: m,
        is_buy,
        price: fp(cap),
        quantity: fp(qty),
        order_type: OrderType::StopMarket { trigger: fp(trigger) },
        time_in_force: TimeInForce::IOC,
        reduce_only,
        client_order_id: None,
    }
}

fn place(ctx: &mut NativeExecContext, t: &Address, p: PlaceOrderParams) {
    let r = NativeExecutor::execute(ctx, t, &NativeAction::PlaceOrder(p));
    assert!(r.success, "{:?}", r.error);
}

/// The aggregated mark of `m` at the context's block (3 equal reporters).
fn set_mark(ctx: &NativeExecContext, m: MarketId, price: FixedPoint) {
    let reporters = [addr(150), addr(151), addr(152)];
    for v in &reporters {
        ctx.oracle.submit_price(v, m, price, ctx.block_height, ctx.timestamp).unwrap();
    }
    let stakes: Vec<(Address, FixedPoint)> = reporters.iter().map(|v| (*v, fp(1))).collect();
    assert_eq!(ctx.oracle.aggregate_price(m, ctx.block_height, ctx.timestamp, &stakes).unwrap(), price);
}

fn marks(v: &[(MarketId, i64)]) -> BTreeMap<MarketId, FixedPoint> {
    v.iter().map(|&(m, p)| (m, fp(p))).collect()
}

// ---- T4: margin configs from market rows ----

/// F2/F8: configs come from CF_NATIVE_MARKETS — max leverage = floor(100 /
/// initial_margin %), one flat tier; undecodable / non-positive rows: none.
#[test]
fn margin_configs_load_from_market_rows() {
    let (_d, db) = open_test_db();
    let rows: [(u64, Vec<u8>); 6] = [
        (1, market_row(fp(5))),             // 20x
        (2, market_row(fp(2))),             // 50x
        (3, market_row(fp(3))),             // 33.3 -> 33x
        (4, market_row(fp(200))),           // 0.5 -> clamped to 1x
        (5, market_row(FixedPoint::ZERO)),  // none
        (6, b"listed".to_vec()),            // undecodable: none
    ];
    for (m, row) in &rows {
        db.put_cf_raw(CF_NATIVE_MARKETS, &m.to_be_bytes(), row).unwrap();
    }
    let ctx = ctx_at(db, 1);
    assert!(ctx.fatal_error.is_none());
    let cfg = |m: MarketId| {
        ctx.margin_configs.get(&m).map(|c| {
            assert_eq!(c.tiers.len(), 1, "one flat tier");
            assert_eq!(c.tiers[0].max_notional, FixedPoint::MAX);
            assert_eq!(c.tiers[0].max_leverage, c.max_leverage);
            c.max_leverage
        })
    };
    assert_eq!([cfg(1), cfg(2), cfg(3), cfg(4), cfg(5), cfg(6)], [Some(20), Some(50), Some(33), Some(1), None, None]);
}

/// D11: for a 20x market the loaded config is byte-identical to no config
/// (today's production IM) — single and sharded batch paths.
#[test]
fn twenty_x_market_config_is_byte_identical_to_no_config() {
    let run = |row: &[u8], workers: usize| {
        let (dir, db) = open_test_db();
        db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), row).unwrap();
        let mut ctx = ctx_at(db.clone(), 1);
        for t in [addr(1), addr(2), addr(3), addr(200)] {
            fund(&ctx, &t, fp(1_000_000));
        }
        set_mark(&ctx, 1, fp(1_000));
        let mkt = |is_buy: bool, cap: i64, qty: i64| PlaceOrderParams {
            order_type: OrderType::Market,
            time_in_force: TimeInForce::IOC,
            ..limit(1, is_buy, cap, qty)
        };
        let acts = vec![
            (addr(1), NativeAction::PlaceOrder(limit(1, true, 990, 7))),
            (addr(2), NativeAction::PlaceOrder(limit(1, false, 1_010, 5))),
            (addr(3), NativeAction::PlaceOrder(mkt(false, 900, 4))),
            (addr(3), NativeAction::PlaceOrder(mkt(true, 1_100, 6))),
            (addr(200), NativeAction::PlaceOrder(limit(9, true, 1, 1))), // filler (2 senders)
        ];
        let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, &acts, workers).results;
        assert!(r.iter().all(|x| x.success));
        let dump = |cf| db.iterate_cf(cf, None).unwrap();
        (dump(CF_NATIVE_BALANCES), dump(CF_NATIVE_POSITIONS), dir)
    };
    for workers in [1, 4] {
        let (b1, p1, _d1) = run(&market_row(fp(5)), workers);
        let (b2, p2, _d2) = run(b"listed", workers);
        assert_eq!(b1, b2, "balances, {workers} workers");
        assert_eq!(p1, p2, "positions, {workers} workers");
    }
}
```

**Implementation:**
* `margin.rs`: `pub fn market_margin_config(market_id, row: &[u8]) -> Option<MarketMarginConfig>`
  — borsh-decode `(String, String, i128, i128, i128)` (`try_from_slice`, the whole row);
  `im <= 0` ⇒ `None`; `lev = max(1, (100 * SCALE) / im)` as `u32` (saturating);
  `tiers = vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: lev }]`,
  `maintenance_factor_bps: 5000`.
* NE `new_with_mode` (:1846): `let (margin_configs, cfg_err) = Self::load_margin_configs(&state);`
  — `iterate_cf(CF_NATIVE_MARKETS, None)`, 8-byte keys only, `market_margin_config` per row;
  a read error ⇒ `load_error` (fatal, like the book-mode marker) and an empty map. Replace
  :1962 `margin_configs: HashMap::new()` with it. Tests that assign `ctx.margin_configs`
  afterwards keep working.
* Per block cost: one scan of ≤ M rows.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests margin_config && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test account_margin_tests --test market_order_margin_tests --test modify_order_tests --test maker_margin_release_tests` · depends_on: [1]

`══ COMMIT 4 ══` `feat(exec): margin configs from market listings (flat tier at max leverage)`

### T5 — core: rewrite `liquidation.rs` (pure parts; penalty / insurance / socialize removed) · depends_on: [1]

**Test first** — REPLACE `crates/torus-core/tests/liquidation_tests.rs` (its 13 tests pin the
removed behaviour: deleting liquidation, ADL by UPnL, `socialize_loss`, insurance fund) with:

```rust
//! Item 3: Hyperliquid-style liquidation, core — docs/plans/liquidation.md.

use torus_core::liquidation::{
    adl_close, adl_rank, backstop, classify, settle_flat_deficit, slippage_cap, stage1_qty,
    AdlCandidate, Health, LIQUIDATOR_VAULT,
};
use torus_core::margin::{AccountView, MarginTier};
use torus_core::position::{MarginType, NativeBalance, PositionManager};
use torus_state::cf::CF_NATIVE_POSITIONS;
use torus_state::{StateBackend, StateDb};
use torus_types::{Address, FixedPoint, MarketId};

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn addr(n: u8) -> Address {
    Address::new([n; 20])
}

fn setup() -> (tempfile::TempDir, PositionManager) {
    let dir = tempfile::tempdir().unwrap();
    let db = StateDb::open(dir.path()).unwrap();
    (dir, PositionManager::new(db))
}

fn set_balance(pm: &PositionManager, t: &Address, amount: FixedPoint) {
    pm.put_native_balance(t, &NativeBalance { available: amount, order_margin: FixedPoint::ZERO })
        .unwrap();
}

fn open_pair(pm: &PositionManager, long: &Address, short: &Address, m: MarketId, qty: i64, px: i64) {
    pm.apply_fill(long, m, true, fp(qty), fp(px), MarginType::Cross).unwrap();
    pm.apply_fill(short, m, false, fp(qty), fp(px), MarginType::Cross).unwrap();
}

fn view(av: FixedPoint, mm: FixedPoint) -> AccountView {
    AccountView {
        available: av,
        order_margin: FixedPoint::ZERO,
        upnl: FixedPoint::ZERO,
        position_im: FixedPoint::ZERO,
        notional: FixedPoint::ZERO,
        maintenance: mm,
    }
}

/// Decisions 2, 4, 5: Healthy at AV >= MM; ADL below 0; backstop below 2/3 MM;
/// otherwise stage 1. Exact on raw units; overflow -> None (skip).
#[test]
fn classify_follows_the_hl_thresholds_exactly() {
    let one = FixedPoint::from_raw(1);
    assert_eq!(classify(&view(fp(300), fp(300))), Some(Health::Healthy), "AV == MM");
    assert_eq!(classify(&view(fp(300) - one, fp(300))), Some(Health::Stage1));
    assert_eq!(classify(&view(fp(200), fp(300))), Some(Health::Stage1), "AV == 2/3 MM");
    assert_eq!(classify(&view(fp(200) - one, fp(300))), Some(Health::Backstop));
    assert_eq!(classify(&view(FixedPoint::ZERO, fp(300))), Some(Health::Backstop), "0 is not < 0");
    assert_eq!(classify(&view(-one, fp(300))), Some(Health::Adl));
    assert_eq!(classify(&view(-fp(5), FixedPoint::ZERO)), Some(Health::Adl));
    assert_eq!(classify(&view(FixedPoint::ZERO, FixedPoint::ZERO)), Some(Health::Healthy));
    let huge = FixedPoint::from_raw(i128::MAX / 2);
    assert_eq!(classify(&view(huge, FixedPoint::MAX)), None, "3 x AV overflows");
}

/// D2: above 100,000 notional at the mark -> 20% of the size (raw / 5); at or
/// below, or when 20% is under the lot -> the whole size.
#[test]
fn stage1_qty_chunks_above_100k_notional_at_mark() {
    assert_eq!(stage1_qty(fp(100), fp(1_000), fp(1)), (fp(100), false), "100,000 is not above");
    assert_eq!(stage1_qty(fp(101), fp(1_000), fp(1)), (FixedPoint::from_raw(fp(101).raw() / 5), true));
    assert_eq!(stage1_qty(fp(3), fp(50_000), fp(1)), (fp(3), false), "0.6 < lot 1 -> whole");
}

/// D1: cap = mark ∓ mark / (2 x max leverage of the position's tier).
#[test]
fn slippage_cap_is_the_positions_mm_rate() {
    assert_eq!(slippage_cap(None, fp(1_000), fp(10_000), false), fp(975), "sell, 20x: 2.5%");
    assert_eq!(slippage_cap(None, fp(1_000), fp(10_000), true), fp(1_025), "buy");
    let t = [MarginTier { max_notional: FixedPoint::MAX, max_leverage: 50 }];
    assert_eq!(slippage_cap(Some(&t), fp(1_000), fp(10_000), false), fp(990), "50x: 1%");
}

/// Decision 5: rank = (mark/entry for longs, entry/mark for shorts) x
/// (notional at mark / AV), descending, exact; AV <= 0 last; ties by address.
#[test]
fn adl_rank_is_hl_profit_times_leverage_exact() {
    let short = |n: u8, entry: i64, av: i64| AdlCandidate {
        trader: addr(n),
        is_long: false,
        size: fp(3),
        entry_price: fp(entry),
        account_value: fp(av),
    };
    // mark 980: (1000/980)(2940/10060) = 0.298; (1100/980)(2940/1360) = 2.43
    let ranked = adl_rank(
        fp(980),
        vec![short(1, 1_000, 10_060), short(2, 1_100, 1_360), short(3, 1_000, 0), short(4, 1_000, 10_060), short(0, 1_000, 10_060)],
    );
    let order: Vec<Address> = ranked.iter().map(|c| c.trader).collect();
    assert_eq!(order, vec![addr(2), addr(0), addr(1), addr(4), addr(3)]);
    // longs at mark 1,100, size 1, AV 1,000: entry 900 (1.344) before entry 1,000 (1.21)
    let long = |n: u8, entry: i64| AdlCandidate { is_long: true, size: fp(1), account_value: fp(1_000), ..short(n, entry, 0) };
    let ranked = adl_rank(fp(1_100), vec![long(1, 1_000), long(2, 900)]);
    assert_eq!(ranked.iter().map(|c| c.trader).collect::<Vec<_>>(), vec![addr(2), addr(1)]);
}

/// Extreme magnitudes rank without panicking (U512 cross-multiplication).
#[test]
fn adl_rank_never_overflows() {
    let c = |n: u8, size: i128, av: i128| AdlCandidate {
        trader: addr(n),
        is_long: false,
        size: FixedPoint::from_raw(size),
        entry_price: FixedPoint::from_raw(i128::MAX / 4),
        account_value: FixedPoint::from_raw(av),
    };
    let r = adl_rank(FixedPoint::from_raw(1), vec![c(1, i128::MAX / 4, 1), c(2, 1, i128::MAX / 4)]);
    assert_eq!(r[0].trader, addr(1));
}
```

**Implementation (`crates/torus-core/src/liquidation.rs`, rewritten):**
* Delete `LiquidationEngine`, `Liquidation`, `LiquidationResult`, `LiquidationMethod`,
  `AdlResult`, `INSURANCE_FUND_KEY`, `DEFAULT_LIQUIDATION_PENALTY_BPS`, `get/set_insurance_fund`,
  `socialize_loss` and the 4 module tests (decision 6, F3-F5, F7).
* Constants (proposed defaults, documented as such):

```rust
/// Item 3 (decision 7): the liquidator vault — a fixed protocol account (no
/// known key: no signed action can come from it). Deposits: later branch.
pub const LIQUIDATOR_VAULT: Address = Address::new(*b"torus-liquidator-vlt");
/// HL: positions above this notional (at the mark) are liquidated in chunks.
/// Raw units (`FixedPoint::from_raw` is not `const`, `torus-types/src/lib.rs:62`).
pub const CHUNK_NOTIONAL_THRESHOLD_RAW: i128 = 100_000 * FixedPoint::SCALE;
/// HL: 20% of the position per chunk = size / 5.
pub const CHUNK_DIVISOR: i128 = 5;
/// HL: seconds of block time after a chunk during which only backstop / ADL act.
pub const CHUNK_COOLDOWN_SECS: u64 = 30;
/// D5 (proposed default): accounts valued / acted on per block.
pub const LIQ_SCAN_PER_BLOCK: usize = 2_048;
pub const LIQ_ACT_PER_BLOCK: usize = 64;
/// `CF_NATIVE_LIQUIDATION` tags (0x01 unused / reserved: no account index, C1).
pub const COOLDOWN_TAG: u8 = 0x02;
pub const PREV_MARK_TAG: u8 = 0x03;
pub const CURSOR_KEY: [u8; 1] = [0x04];
```

* `pub enum Health { Healthy, Stage1, Backstop, Adl }`;
  `pub fn classify(v: &AccountView) -> Option<Health>` — `av = v.equity()` via `checked_add`
  of the three parts; `av >= mm` ⇒ Healthy; `av < 0` ⇒ Adl;
  `av.raw().checked_mul(3)? < mm.raw().checked_mul(2)?` ⇒ Backstop; else Stage1.
* `pub fn stage1_qty(size, mark, lot) -> (FixedPoint, bool)` — notional `checked_mul`
  (overflow ⇒ chunk); `> threshold` ⇒ `q = raw / 5`; `q < lot` ⇒ `(size, false)`.
* `pub fn slippage_cap(tiers, mark, notional, is_buy) -> FixedPoint` —
  `lev = tiers.map_or(DEFAULT_ORDER_MAX_LEVERAGE, |t| effective_max_leverage(t, notional)).max(1)`;
  `d = mark.raw() / (2 * lev as i128)`; buy `mark + d`, sell `mark − d` (raw, checked).
* `pub struct AdlCandidate { trader, is_long, size, entry_price, account_value }`;
  `pub fn adl_rank(mark, Vec<AdlCandidate>) -> Vec<AdlCandidate>` — key numerator
  `px_num × notional`, denominator `px_den × AV` in `alloy_primitives::U512` (verify the alias;
  else `ruint::aliases::U512`), notional = `size.raw() × mark.raw()` as U512 (scale cancels in
  the comparison); `sort_by` with `a.num × b.den` vs `b.num × a.den` descending, AV <= 0 last,
  then address ascending. Stable and total.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --test liquidation_tests && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --lib liquidation` · depends_on: [1]

### T6 — core: transfer primitives (backstop, ADL close, flat deficit) · depends_on: [5]

**Test first** — append to `crates/torus-core/tests/liquidation_tests.rs`:

```rust
fn oi(pm: &PositionManager, m: MarketId) -> (FixedPoint, FixedPoint) {
    let (mut l, mut s) = (FixedPoint::ZERO, FixedPoint::ZERO);
    for (k, v) in pm.state().iterate_cf(CF_NATIVE_POSITIONS, None).unwrap() {
        let p: torus_core::position::Position = borsh::from_slice(&v).unwrap();
        if k[20..28] == m.to_be_bytes() {
            if p.is_long { l += p.size } else { s += p.size }
        }
    }
    (l, s)
}

/// Σ (available + order margin + UPnL at `mark`) over `ts` (one market).
fn value(pm: &PositionManager, ts: &[Address], m: MarketId, mark: FixedPoint) -> FixedPoint {
    ts.iter()
        .map(|t| {
            let b = pm.get_native_balance(t).unwrap();
            let u = pm.get_position(t, m).unwrap().map_or(FixedPoint::ZERO, |p| p.unrealized_pnl(mark));
            b.available + b.order_margin + u
        })
        .fold(FixedPoint::ZERO, |a, b| a + b)
}

/// Decision 4: positions + remaining collateral move to the vault at the mark;
/// OI symmetric, value conserved, nothing deleted without a counterparty.
#[test]
fn backstop_moves_positions_and_collateral_to_the_vault_at_the_mark() {
    let (_d, pm) = setup();
    let (t, s) = (addr(1), addr(2));
    set_balance(&pm, &t, fp(300));
    set_balance(&pm, &s, fp(100_000));
    open_pair(&pm, &t, &s, 1, 10, 1_000);
    let mark = fp(975);
    let who = [t, s, LIQUIDATOR_VAULT];
    let before = value(&pm, &who, 1, mark);
    backstop(&pm, &t, &LIQUIDATOR_VAULT, |_| Some(mark)).unwrap();
    assert!(pm.get_position(&t, 1).unwrap().is_none());
    let v = pm.get_position(&LIQUIDATOR_VAULT, 1).unwrap().unwrap();
    assert_eq!((v.is_long, v.size, v.entry_price), (true, fp(10), mark));
    let tb = pm.get_native_balance(&t).unwrap();
    assert_eq!(tb.available + tb.order_margin, FixedPoint::ZERO);
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, fp(50), "AV 300 - 250");
    assert_eq!(oi(&pm, 1), (fp(10), fp(10)));
    assert_eq!(value(&pm, &who, 1, mark), before);
}

/// The vault nets with what it already holds (fill semantics): short 4 @ 990
/// + takes long 10 @ 975 -> closes 4 (+60 realized), long 6 @ 975.
#[test]
fn backstop_nets_against_the_vaults_existing_position() {
    let (_d, pm) = setup();
    let (t, s, l) = (addr(1), addr(2), addr(3));
    set_balance(&pm, &t, fp(300));
    open_pair(&pm, &t, &s, 1, 10, 1_000);
    open_pair(&pm, &l, &LIQUIDATOR_VAULT, 1, 4, 990);
    let mark = fp(975);
    let who = [t, s, l, LIQUIDATOR_VAULT];
    let before = value(&pm, &who, 1, mark);
    backstop(&pm, &t, &LIQUIDATOR_VAULT, |_| Some(mark)).unwrap();
    let v = pm.get_position(&LIQUIDATOR_VAULT, 1).unwrap().unwrap();
    assert_eq!((v.is_long, v.size, v.entry_price), (true, fp(6), mark));
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, fp(60) + fp(50));
    assert_eq!(oi(&pm, 1), (fp(10), fp(10)), "longs L 4 + vault 6; shorts S 10");
    assert_eq!(value(&pm, &who, 1, mark), before);
}

/// Decision 5: U long 4 closes against ranked shorts at the given (previous
/// mark) price: S2 (ranked first) 3, then S1 1. OI stays symmetric.
#[test]
fn adl_close_pairs_against_ranked_counterparties_at_the_price() {
    let (_d, pm) = setup();
    let (u, s1, s2, l) = (addr(1), addr(2), addr(3), addr(4));
    open_pair(&pm, &u, &s1, 1, 4, 1_000);
    open_pair(&pm, &l, &s2, 1, 3, 1_100);
    let ranked = adl_rank(
        fp(900),
        vec![
            AdlCandidate { trader: s1, is_long: false, size: fp(4), entry_price: fp(1_000), account_value: fp(10_400) },
            AdlCandidate { trader: s2, is_long: false, size: fp(3), entry_price: fp(1_100), account_value: fp(1_600) },
        ],
    );
    let closed = adl_close(&pm, &u, 1, fp(990), &ranked).unwrap();
    assert_eq!(closed, fp(4));
    assert!(pm.get_position(&u, 1).unwrap().is_none());
    assert!(pm.get_position(&s2, 1).unwrap().is_none());
    assert_eq!(pm.get_position(&s1, 1).unwrap().unwrap().size, fp(3));
    assert_eq!(pm.get_native_balance(&u).unwrap().available, -fp(40), "4 x (990 - 1,000)");
    assert_eq!(pm.get_native_balance(&s2).unwrap().available, fp(330), "3 x (1,100 - 990)");
    assert_eq!(pm.get_native_balance(&s1).unwrap().available, fp(10));
    assert_eq!(oi(&pm, 1), (fp(3), fp(3)));
}

/// D9: a FLAT account's negative collateral moves to the vault (conserved);
/// an account with positions is untouched.
#[test]
fn flat_deficit_moves_to_the_vault() {
    let (_d, pm) = setup();
    let (t, s) = (addr(1), addr(2));
    pm.put_native_balance(&t, &NativeBalance { available: -fp(100), order_margin: fp(30) }).unwrap();
    assert_eq!(settle_flat_deficit(&pm, &t, &LIQUIDATOR_VAULT).unwrap(), -fp(70));
    let b = pm.get_native_balance(&t).unwrap();
    assert_eq!(b.available + b.order_margin, FixedPoint::ZERO);
    assert_eq!(pm.get_native_balance(&LIQUIDATOR_VAULT).unwrap().available, -fp(70));
    set_balance(&pm, &s, -fp(5));
    open_pair(&pm, &s, &t, 1, 1, 10);
    assert_eq!(settle_flat_deficit(&pm, &s, &LIQUIDATOR_VAULT).unwrap(), FixedPoint::ZERO);
    assert_eq!(pm.get_native_balance(&s).unwrap().available, -fp(5));
}
```

(`borsh` must be a dev-dependency of torus-core — it is a normal dependency, usable from tests.)

**Implementation (`liquidation.rs`):**
* `pub fn transfer(pm, from, to, m, qty, price) -> Result<(), CoreError>` — reads `from`'s
  side; `pm.apply_fill(from, m, !from_long, qty, price, Cross)` then
  `pm.apply_fill(to, m, from_long, qty, price, Cross)` (one fill between two accounts: the
  ONLY way liquidation moves size).
* `pub fn backstop(pm, trader, vault, mark: impl Fn(MarketId) -> Option<FixedPoint>)` — every
  position ascending market (`positions_for_trader` key order): `transfer(trader, vault, m,
  size, mark?)` (a missing mark is a `CoreError` — the caller already checked); then
  `c = available + order_margin` (checked): `trader.available -= c; vault.available += c`.
* `pub fn adl_close(pm, u, m, price, ranked) -> Result<FixedPoint, CoreError>` — `remaining =
  u.size`; for each candidate `q = min(remaining, cand.size)` (re-read the candidate's current
  position; skip if it changed side / vanished); `transfer(u, cand, m, q, price)`; returns the
  closed size.
* `pub fn adl_candidates(pm, m, exclude, want_long, av: impl Fn(&Address) -> Result<FixedPoint,
  CoreError>) -> Result<Vec<AdlCandidate>, CoreError>` — one `iterate_cf(CF_NATIVE_POSITIONS,
  None)` pass, keys with `k[20..28] == m`, side `== want_long`, trader ≠ `exclude`.
* `pub fn settle_flat_deficit(pm, t, vault) -> Result<FixedPoint, CoreError>` — only if
  `positions_for_trader(t)` is empty and `c = available + order_margin < 0`: same move as the
  backstop's collateral step; returns `c` (else ZERO). Never for `t == vault`.
* `pub fn traders_after<T: StateBackend>(state: &T, after: Option<Address>, limit: usize) ->
  Result<Vec<Address>, CoreError>` — C1 candidate walk: `iterate_cf(CF_NATIVE_POSITIONS, None)`,
  28-byte keys only, trader = `k[..20]`, consecutive duplicates dropped (keys are sorted by
  trader), strictly `> after`, at most `limit` (the caller passes `SCAN + 1` to detect a cut).
  Test (append to core `liquidation_tests.rs`, counted under T6):

```rust
/// C1: candidates = distinct traders of CF_NATIVE_POSITIONS, ascending, after
/// the cursor, at most `limit`.
#[test]
fn traders_after_walks_the_positions_cf_from_the_cursor() {
    let (_d, pm) = setup();
    open_pair(&pm, &addr(9), &addr(3), 1, 1, 10);
    open_pair(&pm, &addr(9), &addr(5), 2, 1, 10); // addr(9): two rows, listed once
    let st = pm.state();
    assert_eq!(traders_after(st, None, 10).unwrap(), vec![addr(3), addr(5), addr(9)]);
    assert_eq!(traders_after(st, Some(addr(3)), 10).unwrap(), vec![addr(5), addr(9)]);
    assert_eq!(traders_after(st, None, 2).unwrap(), vec![addr(3), addr(5)]);
    assert!(traders_after(st, Some(addr(9)), 10).unwrap().is_empty());
}
```

  (add `traders_after` to the test file's `use torus_core::liquidation::{…}`.)
* Cooldown / prev-mark / cursor rows: `in_cooldown(state, t, now)` (`now.saturating_sub(ts) <
  30`), `set_cooldown`, `clear_cooldown`, `prev_marks(state, markets) -> BTreeMap`,
  `put_prev_marks(state, marks, prev)` (writes only changed values), `cursor(state)`,
  `put_cursor(state, Option<Address>)` (None ⇒ delete). Fixed widths; a malformed row is a
  `CoreError`.
* Drop the `MarketMarginConfig` / `MarginEngine` imports that become unused.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --test liquidation_tests && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --test margin_tests && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh check --workspace --all-targets` · depends_on: [5]

`══ COMMIT 5 ══` `feat(core)!: HL liquidation primitives; drop penalty, insurance fund, socialized loss`
(the commit also deletes NE `run_liquidation_checks` and its import NE:14 — dead, and it no
longer compiles; liquidation is unwired between commits 5 and 7, as it is today).

### T7a — bridge: `run_liquidations` — candidate walk, skip rule, cancel orders + stops, stage 1 · depends_on: [3, 4, 6]

**Test first** — append to `crates/torus-bridge/tests/liquidation_tests.rs`:

```rust
// ---- T7a: scan, skip, cancel, stage 1 ----

/// Stage 1: AV 200 < MM 247.5 (and >= 2/3 MM): one reduce-only market sell
/// into the book closes the long at 985; the trader keeps 300 - 150 = 150.
#[test]
fn stage1_closes_into_the_book_and_the_trader_keeps_the_rest() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990));
    let before = total_value(&ctx, &marks(&[(1, 990)]));
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.fatal_error.is_none());
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &m, 1), fp(10), "the maker bought it");
    assert_eq!(ab(&ctx, &t), (fp(150), FixedPoint::ZERO));
    assert_eq!(oi(&ctx, 1), (fp(10), fp(10)));
    assert_eq!(total_value(&ctx, &marks(&[(1, 990)])), before);
    assert_eq!(traders(&ctx), vec![s, m], "t has no position rows left");
}

/// "Remaining collateral stays once MM is met": the larger-MM market goes
/// first; afterwards AV 140 >= MM 24.75, so market 2 stays open.
#[test]
fn stage1_stops_once_maintenance_is_met() {
    let (_d, db) = liq_db(&[1, 2]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &t, &s, 2, 1, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    place(&mut ctx, &m, limit(2, true, 985, 1));
    set_mark(&ctx, 1, fp(990));
    set_mark(&ctx, 2, fp(990));
    // AV 300 - 100 - 10 = 190 < MM 272.25; 3 x 190 >= 2 x 272.25 -> stage 1
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &t, 2), fp(1), "MM met after market 1: market 2 untouched");
    assert_eq!(bal(&ctx, &t).available, fp(150));
}

/// Decision 9: a stale / absent mark in ANY of the account's markets skips the
/// whole account — even though market 1 alone is underwater. Nothing moves.
#[test]
fn a_missing_mark_in_any_market_skips_the_account() {
    let (_d, db) = liq_db(&[1, 2]);
    let mut ctx = ctx_at(db.clone(), 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &t, &s, 2, 1, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990)); // market 2: no mark
    let snap = |db: &StateDb| (db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
    let before = snap(&db);
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(snap(&db), before);
    assert_eq!(ctx.order_books[&1].orders_for_trader(&m).len(), 1, "maker's bid untouched");
    // a stale mark is the same: block 62 (ts 1062) sees market 1's aggregate (ts 1001) aged 61
    let mut late = ctx_at(db.clone(), 62);
    set_mark(&late, 2, fp(990));
    NativeExecutor::run_liquidations(&mut late);
    assert_eq!(snap(&db).0, before.0, "market 1 mark stale -> skipped");
}

/// D4 + F9: before stage 1, ALL the account's orders go — resting orders AND
/// pending stops with their reservations (T3's shared helper). order_margin
/// returns to 0.
#[test]
fn cancels_resting_orders_and_pending_stops_with_their_reservations() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    fund(&ctx, &t, fp(300));
    fund(&ctx, &s, fp(1_000_000));
    fund(&ctx, &m, fp(1_000_000));
    place(&mut ctx, &t, limit(1, true, 900, 1)); // reserves 45
    place(&mut ctx, &t, stop_market(1, true, 1_100, 1_200, 1, false)); // reserves 60
    assert_eq!(ab(&ctx, &t), (fp(195), fp(105)));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 10));
    set_mark(&ctx, 1, fp(990)); // AV 195 + 105 - 100 = 200 -> stage 1
    NativeExecutor::run_liquidations(&mut ctx);
    assert!(ctx.order_books[&1].orders_for_trader(&t).is_empty());
    assert_eq!(ctx.order_books[&1].pending_stop_count(), 0, "t's stop removed");
    assert_eq!(ab(&ctx, &t), (fp(150), FixedPoint::ZERO));
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
}

/// Healthy accounts: positions / balances / books unchanged; only the
/// previous-mark row is written (no cooldown, no cursor).
#[test]
fn healthy_accounts_are_untouched() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db.clone(), 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(1_000));
    fund(&ctx, &s, fp(1_000_000));
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    set_mark(&ctx, 1, fp(990)); // AV 900 >= MM 247.5
    let before = (db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap());
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!((db.iterate_cf(CF_NATIVE_POSITIONS, None).unwrap(), db.iterate_cf(CF_NATIVE_BALANCES, None).unwrap()), before);
    assert!(liq_rows(&ctx, 0x02).is_empty() && liq_rows(&ctx, 0x04).is_empty());
    let prev = liq_rows(&ctx, 0x03);
    assert_eq!(prev.len(), 1);
    assert_eq!(prev[0].1, fp(990).raw().to_be_bytes().to_vec());
}
```

**Implementation** — new child module `crates/torus-bridge/src/liquidation_step.rs`, declared
in NE next to :209-214 as `#[path = "liquidation_step.rs"] mod liquidation_step;` (a child
module sees NE's private `place_order_inner`, `run_triggered_stops`, `try_reserve_for_qty_cfg`,
`AccountReader`):

```rust
//! Item 3: the end-of-block liquidation step — docs/plans/liquidation.md.
use super::*;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use torus_core::liquidation::{self as liq, Health, LIQUIDATOR_VAULT};
use torus_core::margin::maintenance_margin;

impl NativeExecutor {
    /// Item 3 block step (after `drain_core_writer`, before governance): stage 1
    /// (book) / backstop (vault) / ADL on the block-start mark. A storage error
    /// is a node fault -> `fatal_error`; everything else is a result.
    pub fn run_liquidations<T: StateBackend>(ctx: &mut NativeExecContext<T>) -> Vec<NativeActionResult> {
        Self::run_liquidations_with(ctx, liq::LIQ_SCAN_PER_BLOCK, liq::LIQ_ACT_PER_BLOCK)
    }

    /// `run_liquidations` with explicit budgets (tests).
    pub fn run_liquidations_with<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
    ) -> Vec<NativeActionResult> {
        match Self::liquidation_pass(ctx, scan, act) {
            Ok(r) => r,
            Err(e) => {
                ctx.fatal_error = Some(format!("liquidation step: {e}"));
                Vec::new()
            }
        }
    }

    fn liquidation_pass<T: StateBackend>(
        ctx: &mut NativeExecContext<T>,
        scan: usize,
        act: usize,
    ) -> Result<Vec<NativeActionResult>, CoreError> {
        let marks: BTreeMap<MarketId, FixedPoint> = {
            let reader = AccountReader::of(ctx);
            ctx.governance
                .listed_market_ids()
                .map_err(|e| CoreError::Internal(e.to_string()))? // use the closest existing variant
                .into_iter()
                .filter_map(|m| reader.mark(m).map(|p| (m, p)))
                .collect()
        };
        let prev = liq::prev_marks(&ctx.state, marks.keys().copied())?;
        // C1 (decided, s517): no separate index — walk CF_NATIVE_POSITIONS
        // (sorted by trader) from the round-robin cursor; SCAN + 1 detects a cut.
        let cursor = liq::cursor(&ctx.state)?;
        let accounts = liq::traders_after(&ctx.state, cursor, scan.saturating_add(1))?;
        let (mut results, mut scanned, mut acted, mut last, mut cut) = (Vec::new(), 0, 0, None, false);
        for &trader in accounts.iter().filter(|a| **a != LIQUIDATOR_VAULT) {
            if scanned == scan || acted == act {
                cut = true;
                break;
            }
            scanned += 1;
            last = Some(trader);
            let class = Self::liq_view(ctx, &marks, &trader)?.map(|v| liq::classify(&v));
            let h = match class {
                Some(Some(h)) => h,
                _ => {
                    results.push(NativeActionResult::err("liquidation", format!("{trader}: skipped (no mark / overflow / isolated)")));
                    continue;
                }
            };
            if h == Health::Healthy {
                liq::clear_cooldown(&ctx.state, &trader)?;
                continue;
            }
            acted += 1;
            if let Some(ref m) = ctx.metrics {
                m.liquidations_triggered.inc();
            }
            Self::cancel_account_orders(ctx, &trader)?;
            match h {
                Health::Adl => Self::adl_account(ctx, &marks, &prev, &trader)?,
                Health::Backstop => liq::backstop(&ctx.positions, &trader, &LIQUIDATOR_VAULT, |m| marks.get(&m).copied())?,
                Health::Stage1 => Self::stage1(ctx, &marks, &trader)?,
                Health::Healthy => unreachable!(),
            }
            liq::settle_flat_deficit(&ctx.positions, &trader, &LIQUIDATOR_VAULT)?;
            if ctx.positions.positions_for_trader(&trader)?.is_empty() {
                liq::clear_cooldown(&ctx.state, &trader)?;
            }
            results.push(NativeActionResult::ok("liquidation", 3000));
        }
        // D8: the vault is exempt from stage 1 / backstop; ADL when its AV < 0.
        if let Some(v) = Self::liq_view(ctx, &marks, &LIQUIDATOR_VAULT)? {
            if v.equity() < FixedPoint::ZERO {
                Self::adl_account(ctx, &marks, &prev, &LIQUIDATOR_VAULT)?;
            }
        }
        liq::put_prev_marks(&ctx.state, &marks, &prev)?;
        liq::put_cursor(&ctx.state, if cut { last } else { None })?;
        Ok(results)
    }

    /// Decision 9: `None` unless EVERY position (all Cross) has a usable mark;
    /// overflow -> `None` (skip).
    fn liq_view<T: StateBackend>(
        ctx: &NativeExecContext<T>,
        marks: &BTreeMap<MarketId, FixedPoint>,
        trader: &Address,
    ) -> Result<Option<AccountView>, CoreError> {
        let ps = ctx.positions.positions_for_trader(trader)?;
        if ps.is_empty()
            || ps.iter().any(|p| p.margin_type != MarginType::Cross || !marks.contains_key(&p.market_id))
        {
            return Ok(None);
        }
        let bal = ctx.positions.get_native_balance(trader)?;
        let reader = AccountReader::of(ctx);
        Ok(AccountView::build(&bal, &ps, |m| marks.get(&m).copied(), |m| reader.tiers(m)).ok())
    }
}
```

* `cancel_account_orders(ctx, trader)` = T3's shared `cancel_orders_and_stops(ctx, trader,
  None)` (stops first, then resting orders, reservations released) — called with the market ids
  SORTED (the liquidation step never iterates a HashMap; the user path keeps today's
  order-independent iteration).
* `stage1(ctx, marks, trader)`: `if liq::in_cooldown(state, trader, ctx.timestamp)? { return }`;
  positions keyed `(Reverse(maintenance_margin(tiers, size × mark)), market)` (computed BEFORE
  any `&mut ctx` use); per position: `lot = ctx.order_books.get(&m).map_or(FixedPoint::ONE,
  |b| b.lot_size)`; `(qty, chunked) = liq::stage1_qty(size, mark, lot)`; `cap =
  liq::slippage_cap(tiers, mark, notional, !is_long)`; params `{ market, is_buy: !is_long,
  price: cap, quantity: qty, order_type: Market, time_in_force: IOC, reduce_only: true,
  client_order_id: None }`; `let mut q = VecDeque::new(); let r = Self::place_order_inner(ctx,
  trader, &p, None, &mut q); Self::run_triggered_stops(ctx, q);` (D7); a failed `r` is a
  result; `if chunked { set_cooldown(now); break }`; re-value — `AV >= MM` ⇒ break.
* `adl_account(ctx, marks, prev, u)`: per position ascending market: `px = prev.get(&m)
  .copied().unwrap_or(mark)`; `cands = liq::adl_candidates(&ctx.positions, m, u, !is_long,
  |t| AccountReader::of(ctx).view(t, &ctx.positions.get_native_balance(t)?).map(|v|
  v.equity()))?` (ranking AV, entry fallback — C7); `liq::adl_close(&ctx.positions, u, m, px,
  &liq::adl_rank(mark, cands))?`.
* Delete NE `run_liquidation_checks` (:6901-6966) if not already gone in commit 5.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --lib order_book` · depends_on: [3, 4, 6]

### T7b — chunks + cooldown (block time), backstop during cooldown · depends_on: [7a]

**Test first:**

```rust
// ---- T7b: chunks, cooldown ----

/// T long 200 @ 1,000 (198k notional at 990 > 100k), collateral 5,500:
/// AV 3,500 < MM 4,950, 3 x 3,500 >= 2 x 4,950 -> stage 1 in 20% chunks.
/// Chunk 1 at ts 1001; nothing until ts 1031 (age 30); chunk 2 = 20% of 160.
#[test]
fn chunks_of_20_percent_with_a_30s_block_time_cooldown() {
    let (_d, db) = liq_db(&[1]);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(5_500));
    fund(&ctx, &s, fp(10_000_000));
    fund(&ctx, &m, fp(10_000_000));
    open_pair(&ctx, &t, &s, 1, 200, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 100));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), fp(160), "chunk 1 = 40");
    assert_eq!(liq_rows(&ctx, 0x02).len(), 1, "cooldown row");
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), fp(160), "same block: cooldown");
    ctx.save_order_books();
    for h in [2u64, 30] {
        let mut c = ctx_at(db.clone(), h); // ts 1002 / 1030: still < 30 s
        NativeExecutor::run_liquidations(&mut c);
        assert_eq!(pos(&c, &t, 1), fp(160), "h {h}");
        c.save_order_books();
    }
    let mut c = ctx_at(db.clone(), 31); // ts 1031: 30 s after 1001, mark age 30 (usable)
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), fp(128), "chunk 2 = 20% of 160");
    assert_eq!(oi(&c, 1), (fp(200), fp(200)));
}

/// HL: during the cooldown only the backstop can act. Mark 975 at ts 1005:
/// AV 4,900 - 4,000 = 900, 3 x 900 < 2 x 3,900 -> backstop; cooldown cleared.
#[test]
fn backstop_acts_during_the_cooldown() {
    let (_d, db) = liq_db(&[1]);
    let (t, s, m) = (addr(1), addr(2), addr(3));
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &t, fp(5_500));
    fund(&ctx, &s, fp(10_000_000));
    fund(&ctx, &m, fp(10_000_000));
    open_pair(&ctx, &t, &s, 1, 200, 1_000);
    place(&mut ctx, &m, limit(1, true, 985, 100));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx); // chunk 1 -> 160 left, available 4,900
    ctx.save_order_books();
    let mut c = ctx_at(db.clone(), 5);
    set_mark(&c, 1, fp(975));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), FixedPoint::ZERO);
    let v = c.positions.get_position(&LIQUIDATOR_VAULT, 1).unwrap().unwrap();
    assert_eq!((v.is_long, v.size, v.entry_price), (true, fp(160), fp(975)));
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, fp(900));
    assert!(liq_rows(&c, 0x02).is_empty(), "flat -> cooldown cleared");
    assert_eq!(oi(&c, 1), (fp(200), fp(200)));
}
```

**Implementation:** cooldown helpers (T6) wired in `stage1` (T7a). Fix only what the tests show.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests chunk && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests cooldown` · depends_on: [7a]

### T7c — backstop through the step · depends_on: [7a]

```rust
// ---- T7c: backstop ----

/// Decision 4 through the block step: collateral 345 (45 of it reserved by a
/// resting bid), mark 975: AV 95 < 2/3 MM (162.5): the vault takes long 10 @ 975
/// and the 95; the trader's order is cancelled; the vault now holds positions;
/// value conserved.
#[test]
fn backstop_through_the_step_moves_everything_to_the_vault() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s) = (addr(1), addr(2));
    fund(&ctx, &t, fp(345));
    fund(&ctx, &s, fp(1_000_000));
    place(&mut ctx, &t, limit(1, true, 900, 1)); // 45 reserved
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    set_mark(&ctx, 1, fp(975));
    let before = total_value(&ctx, &marks(&[(1, 975)]));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &LIQUIDATOR_VAULT, 1), fp(10));
    assert_eq!(ab(&ctx, &t), (FixedPoint::ZERO, FixedPoint::ZERO));
    assert_eq!(bal(&ctx, &LIQUIDATOR_VAULT).available, fp(95));
    assert!(ctx.order_books[&1].orders_for_trader(&t).is_empty());
    assert_eq!(traders(&ctx), { let mut v = vec![s, LIQUIDATOR_VAULT]; v.sort(); v });
    assert_eq!(total_value(&ctx, &marks(&[(1, 975)])), before);
}
```

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests backstop` · depends_on: [7a]

### T7d — ADL at the previous mark, vault ADL, flat deficit · depends_on: [7a]

```rust
// ---- T7d: ADL ----

/// T long 4 @ 1,000 (200); L long 3 @ 1,100; S1 short 4 @ 1,000 (10,000);
/// S2 short 3 @ 1,100 (1,000). Block 1 mark 990: all healthy (T: AV 160 >=
/// MM 99), previous mark 990 stored. Block 2 mark 900: T's AV -200 -> ADL at
/// 990 (previous mark): S2 ranked first (2.06 vs 0.38) closes 3, S1 closes 1.
fn adl_fixture() -> (tempfile::TempDir, StateDb, [Address; 4]) {
    let (d, db) = liq_db(&[1]);
    let ctx = ctx_at(db.clone(), 1);
    let w @ [t, s1, s2, l] = [addr(1), addr(2), addr(3), addr(4)];
    for (who, a) in [(t, 200), (s1, 10_000), (s2, 1_000), (l, 10_000)] {
        fund(&ctx, &who, fp(a));
    }
    open_pair(&ctx, &t, &s1, 1, 4, 1_000);
    open_pair(&ctx, &l, &s2, 1, 3, 1_100);
    (d, db, w)
}

#[test]
fn adl_closes_against_ranked_counterparties_at_the_previous_mark() {
    let (_d, db, [t, s1, s2, l]) = adl_fixture();
    let mut c1 = ctx_at(db.clone(), 1);
    set_mark(&c1, 1, fp(990));
    NativeExecutor::run_liquidations(&mut c1);
    assert_eq!(pos(&c1, &t, 1), fp(4), "AV 160 >= MM 99: healthy");
    let mut c2 = ctx_at(db.clone(), 2);
    set_mark(&c2, 1, fp(900));
    let before = total_value(&c2, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c2);
    assert_eq!(pos(&c2, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c2, &s2, 1), FixedPoint::ZERO, "ranked first: fully closed");
    assert_eq!(pos(&c2, &s1, 1), -fp(3));
    assert_eq!(pos(&c2, &l, 1), fp(3));
    assert_eq!(bal(&c2, &t).available, fp(160), "200 + 4 x (990 - 1,000): no deficit");
    assert_eq!(bal(&c2, &s2).available, fp(1_330));
    assert_eq!(bal(&c2, &s1).available, fp(10_010));
    assert_eq!(oi(&c2, 1), (fp(3), fp(3)));
    assert_eq!(total_value(&c2, &marks(&[(1, 900)])), before);
}

/// D10 + D9: the first step ever has no previous mark -> ADL at the current
/// mark (900); T's -200 then moves to the vault (conserved, not written off).
#[test]
fn adl_without_a_previous_mark_uses_the_mark_and_the_deficit_goes_to_the_vault() {
    let (_d, db, [t, ..]) = adl_fixture();
    let mut c = ctx_at(db.clone(), 2);
    set_mark(&c, 1, fp(900));
    let before = total_value(&c, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c);
    assert_eq!(pos(&c, &t, 1), FixedPoint::ZERO);
    assert_eq!(bal(&c, &t).available, FixedPoint::ZERO);
    assert_eq!(bal(&c, &LIQUIDATOR_VAULT).available, -fp(200));
    assert_eq!(oi(&c, 1), (fp(3), fp(3)));
    assert_eq!(total_value(&c, &marks(&[(1, 900)])), before);
}

/// D8: the vault (exempt from stage 1 / backstop) is ADL'd when its AV < 0, at
/// the previous mark: backstop at 975 (block 1), mark 900 (block 2): vault AV
/// 50 - 750 < 0 -> closes long 10 against S at 975.
#[test]
fn the_vault_is_adld_when_its_value_goes_negative() {
    let (_d, db) = liq_db(&[1]);
    let (t, s) = (addr(1), addr(2));
    let mut c1 = ctx_at(db.clone(), 1);
    fund(&c1, &t, fp(300));
    fund(&c1, &s, fp(1_000_000));
    open_pair(&c1, &t, &s, 1, 10, 1_000);
    set_mark(&c1, 1, fp(975));
    NativeExecutor::run_liquidations(&mut c1);
    assert_eq!(pos(&c1, &LIQUIDATOR_VAULT, 1), fp(10));
    let mut c2 = ctx_at(db.clone(), 2);
    set_mark(&c2, 1, fp(900));
    let before = total_value(&c2, &marks(&[(1, 900)]));
    NativeExecutor::run_liquidations(&mut c2);
    assert_eq!(pos(&c2, &LIQUIDATOR_VAULT, 1), FixedPoint::ZERO);
    assert_eq!(pos(&c2, &s, 1), FixedPoint::ZERO);
    assert_eq!(bal(&c2, &LIQUIDATOR_VAULT).available, fp(50), "closed at its entry 975");
    assert_eq!(bal(&c2, &s).available, fp(1_000_250), "10 x (1,000 - 975)");
    assert_eq!(total_value(&c2, &marks(&[(1, 900)])), before);
}
```

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests adl` · depends_on: [7a]

### T7e — budgets + cursor, `liquidation_due`, stops fired by liquidation fills · depends_on: [7a]

**Test first:**

```rust
// ---- T7e: budgets, carry-over, due, stops ----

/// D5: 5 underwater accounts, an empty book (stage-1 orders do not fill, so
/// they stay liquidatable), budgets scan 2 / act 2: blocks act on a1-a2,
/// a3-a4, a5 (end of the positions CF -> cursor deleted), then a1-a2 again. "Acted" is
/// visible as the account's resting bid being cancelled.
#[test]
fn budgets_carry_over_through_the_cursor_round_robin() {
    let (_d, db) = liq_db(&[1]);
    let s = addr(50);
    let accts: Vec<Address> = (1..=5).map(addr).collect();
    let mut ctx = ctx_at(db.clone(), 1);
    fund(&ctx, &s, fp(1_000_000));
    for a in &accts {
        fund(&ctx, a, fp(345));
        place(&mut ctx, a, limit(1, true, 900, 1)); // the "not yet acted" marker
        open_pair(&ctx, a, &s, 1, 10, 1_000);
    }
    set_mark(&ctx, 1, fp(990)); // each: AV 245 < MM 247.5, 3 x 245 >= 495 -> stage 1; bids @ 900 < cap 965.25: no fill
    let acted = |c: &NativeExecContext| -> Vec<bool> {
        accts.iter().map(|a| c.order_books[&1].orders_for_trader(a).is_empty()).collect()
    };
    let cursor = |c: &NativeExecContext| liq_rows(c, 0x04).first().map(|(_, v)| Address::from_slice(v));
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2);
    assert_eq!(acted(&ctx), [true, true, false, false, false]);
    assert_eq!(cursor(&ctx), Some(accts[1]));
    assert!(NativeExecutor::liquidation_due(&db).unwrap(), "a cut pass is due");
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2);
    assert_eq!(acted(&ctx), [true, true, true, true, false]);
    NativeExecutor::run_liquidations_with(&mut ctx, 2, 2);
    assert_eq!(acted(&ctx), [true; 5]);
    assert_eq!(cursor(&ctx), None, "a5 then s (healthy) reached the end: cursor deleted");
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
}

/// `liquidation_due`: false on an empty CF, true with a cooldown row.
#[test]
fn liquidation_due_reads_cooldown_and_cursor_rows() {
    let (_d, db) = liq_db(&[1]);
    assert!(!NativeExecutor::liquidation_due(&db).unwrap());
    db.put_cf_raw(CF_NATIVE_LIQUIDATION, &[[0x02u8].as_slice(), &[7u8; 20]].concat(), &1_001u64.to_be_bytes())
        .unwrap();
    assert!(NativeExecutor::liquidation_due(&db).unwrap());
}

/// D7: a stop fired by a liquidation fill runs in the same step. X's
/// reduce-only stop sell (trigger 986) fires on T's liquidation trade at 985
/// and sells X's 2 into the next bid (980).
#[test]
fn stops_fired_by_liquidation_fills_run_in_the_step() {
    let (_d, db) = liq_db(&[1]);
    let mut ctx = ctx_at(db, 1);
    let (t, s, m, x) = (addr(1), addr(2), addr(3), addr(4));
    fund(&ctx, &t, fp(300));
    for w in [s, m, x] {
        fund(&ctx, &w, fp(1_000_000));
    }
    open_pair(&ctx, &t, &s, 1, 10, 1_000);
    open_pair(&ctx, &x, &s, 1, 2, 1_000);
    place(&mut ctx, &x, stop_market(1, false, 986, 900, 2, true));
    place(&mut ctx, &m, limit(1, true, 985, 10));
    place(&mut ctx, &m, limit(1, true, 980, 2));
    set_mark(&ctx, 1, fp(990));
    NativeExecutor::run_liquidations(&mut ctx);
    assert_eq!(pos(&ctx, &t, 1), FixedPoint::ZERO);
    assert_eq!(pos(&ctx, &x, 1), FixedPoint::ZERO, "x's stop fired and filled at 980");
    assert_eq!(pos(&ctx, &m, 1), fp(12));
    assert_eq!(oi(&ctx, 1), (fp(12), fp(12)));
}
```

**Implementation:**
* `pub fn liquidation_due<T: StateBackend>(state: &T) -> Result<bool, CoreError>` (NE, next to
  `oracle_due` :6778): a `0x02` row (`iterate_cf(CF_NATIVE_LIQUIDATION, Some(&[0x02]))`
  non-empty) or the `0x04` row exists.
* Cursor semantics as in the test: start strictly after the cursor; stop at a budget (write
  cursor = last scanned) or at the end of `CF_NATIVE_POSITIONS` (delete it); no wrap within a
  block. A cursor trader that has since closed all positions is fine (`> cursor`).

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --test liquidation_tests` · depends_on: [7a]

`══ COMMIT 6 ══` `feat(exec): HL liquidation step — book, backstop vault, ADL (not wired)`

### T8 — consensus: wire the step, gate, fatal check · depends_on: [7e]

**Test first** — append to `mod crash_recovery_tests` (app.rs), after the oracle helpers:

```rust
    // ---- item 3: liquidation helpers ----

    /// oracle_fixture_db + T (seed 71) long `size` @ 1,000 against S (72); T funded
    /// `collateral`, S and maker M (73) funded 10^7. Seeded through PositionManager.
    fn liq_fixture_db(size: i64, collateral: i64) -> (ChainConfig, StateDb) {
        use torus_core::position::{MarginType, NativeBalance, PositionManager};
        let (config, db) = oracle_fixture_db();
        let pm = PositionManager::new(db.clone());
        for (seed, amt) in [(71u8, collateral), (72, 10_000_000), (73, 10_000_000)] {
            pm.put_native_balance(&oracle_addr(seed), &NativeBalance { available: px(amt), order_margin: FixedPoint::ZERO })
                .unwrap();
        }
        pm.apply_fill(&oracle_addr(71), ORACLE_MARKET, true, px(size), px(1_000), MarginType::Cross).unwrap();
        pm.apply_fill(&oracle_addr(72), ORACLE_MARKET, false, px(size), px(1_000), MarginType::Cross).unwrap();
        (config, db)
    }

    fn oracle_sub(seed: u8, h: u64, price: i64) -> SignedNativeAction {
        torus_types::eip712::sign_native_action(
            NativeAction::SubmitOraclePrices(torus_types::OracleSubmission {
                prices: vec![(ORACLE_MARKET, px(price))],
                timestamp: 0,
            }),
            h * 1_000 + seed as u64,
            &oracle_key(seed),
        )
    }

    fn signed_bid(seed: u8, nonce: u64, price: i64, qty: i64) -> SignedNativeAction {
        torus_types::eip712::sign_native_action(
            NativeAction::PlaceOrder(torus_types::PlaceOrderParams {
                market_id: ORACLE_MARKET,
                is_buy: true,
                price: px(price),
                quantity: px(qty),
                order_type: torus_types::OrderType::Limit,
                time_in_force: torus_types::TimeInForce::GTC,
                reduce_only: false,
                client_order_id: None,
            }),
            nonce,
            &oracle_key(seed),
        )
    }

    /// Heights 1..=rounds.len() (ts = 1000 + h), linked.
    fn liq_blocks(rounds: Vec<Vec<SignedNativeAction>>) -> Vec<TorusBlock> {
        let mut blocks: Vec<TorusBlock> =
            rounds.into_iter().enumerate().map(|(i, a)| make_block(i as u64 + 1, a)).collect();
        link_blocks(&mut blocks);
        blocks
    }

    fn signed_pos_of(db: &StateDb, who: &Address) -> FixedPoint {
        match torus_core::position::PositionManager::new(db.clone()).get_position(who, ORACLE_MARKET).unwrap() {
            Some(p) if p.is_long => p.size,
            Some(p) => -p.size,
            None => FixedPoint::ZERO,
        }
    }

    /// Item 3 T8: the step runs at the END of the block (block 2's own bid fills
    /// the chunk) on the block-start mark; a cooldown row alone makes an EMPTY
    /// block (no submission rows left) run the native phase.
    /// T long 200 @ 1,000, collateral 5,500, mark 990: stage 1, 20% chunks.
    #[test]
    fn liquidation_e2e_chunks_at_block_end_and_cooldown_drives_empty_blocks() {
        let (config, db) = liq_fixture_db(200, 5_500);
        let ctx = make_exec_ctx(&config, &db);
        let t = oracle_addr(71);
        let mut rounds = vec![
            vec![oracle_sub(61, 1, 990), oracle_sub(62, 1, 990), oracle_sub(63, 1, 990)], // 1 (ts 1001)
            vec![signed_bid(73, 2_073, 985, 100)],                                         // 2 (mark 990)
        ];
        rounds.extend(std::iter::repeat_n(Vec::new(), 30)); // 3..=32 (ts 1003..=1032)
        let blocks = liq_blocks(rounds);
        ctx.execute_committed_block(&blocks[0], vec![]);
        assert_eq!(signed_pos_of(&db, &t), px(200), "block 1: no mark yet");
        ctx.execute_committed_block(&blocks[1], vec![]);
        assert_eq!(signed_pos_of(&db, &t), px(160), "block 2: chunk 1 filled by block 2's own bid");
        for b in &blocks[2..31] {
            ctx.execute_committed_block(b, vec![]); // 3..=31 (ts <= 1031)
        }
        assert_eq!(signed_pos_of(&db, &t), px(160), "cooldown: next chunk at ts >= 1032");
        assert!(oracle_sub_rows(&db).is_empty(), "rows pruned: only the cooldown makes block 32 due");
        ctx.execute_committed_block(&blocks[31], vec![]); // 32, ts 1032, empty
        assert_eq!(signed_pos_of(&db, &t), px(128), "chunk 2 = 20% of 160");
        assert!(!ctx.exec_failed.load(Ordering::SeqCst));
        assert_eq!(read_native_applied_height(&db), Some(32));
    }
```

(Mark: block-1 rows (ts 1001) re-stamp the aggregate through ts 1011; usable through 1071.
After chunk 1: AV 3,300 < MM 3,960, 3·3,300 ≥ 2·3,960 ⇒ still stage 1.)

**Implementation (`crates/torus-consensus/src/app.rs`):**
* Gate :1987: `match NativeExecutor::oracle_due(&overlay).and_then(|d| Ok(d ||
  NativeExecutor::liquidation_due(&overlay)?))` — same fail-stop arm.
* After :2263 `drain_core_writer`: `let _ = NativeExecutor::run_liquidations(&mut ctx);` then a
  fatal check identical to :2235-2251 (`ctx.fatal_error.take()` ⇒ `exec_failed`, header persist
  when folded, `return`) — F11.
* Comment: EVM ran before this phase; precompiles see liquidations from the next block.
* `app.rs:13176` (book-mode test pipeline) and the chaos manual pipelines are tests; T9 adds
  the step where roots are compared.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib liquidation_e2e && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib oracle_ && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib exec_pipeline && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib crash` · depends_on: [7e]

`══ COMMIT 7 ══` `feat(consensus): run liquidations at the end of each native block`

### T9 — determinism gates · depends_on: [8]

**Test first:**

(a) app.rs — generalize `run_oracle_fixture(mode, blocks)` (:14549) to
`run_fixture(mode, blocks, fixture: fn() -> (ChainConfig, StateDb))`; the two oracle tests pass
`oracle_fixture_db`. Then:

```rust
    /// T1 (71) long 10 @ 100, collateral 40; T2 (74) long 10 @ 100, collateral 50;
    /// S (72) short 20 @ 100; maker M (73). 1: V @100 + M bid 10 @ 95;
    /// 3: V @97 -> block 4: T1 backstop (AV 10 < 16.17), T2 stage 1 (AV 20) into
    /// M's bid; 5: V @80 -> block 6: vault AV 10 - 170 < 0 -> ADL vs S at 97.
    fn liq_det_fixture() -> (ChainConfig, StateDb) {
        use torus_core::position::{MarginType, NativeBalance, PositionManager};
        let (config, db) = oracle_fixture_db();
        let pm = PositionManager::new(db.clone());
        for (seed, amt) in [(71u8, 40), (74, 50), (72, 1_000_000), (73, 1_000_000)] {
            pm.put_native_balance(&oracle_addr(seed), &NativeBalance { available: px(amt), order_margin: FixedPoint::ZERO })
                .unwrap();
        }
        for long in [71u8, 74] {
            pm.apply_fill(&oracle_addr(long), ORACLE_MARKET, true, px(10), px(100), MarginType::Cross).unwrap();
            pm.apply_fill(&oracle_addr(72), ORACLE_MARKET, false, px(10), px(100), MarginType::Cross).unwrap();
        }
        (config, db)
    }

    fn liq_det_blocks() -> Vec<TorusBlock> {
        let subs = |h: u64, p: i64| vec![oracle_sub(61, h, p), oracle_sub(62, h, p), oracle_sub(63, h, p)];
        let mut r1 = subs(1, 100);
        r1.push(signed_bid(73, 1_073, 95, 10));
        let mut rounds = vec![r1, vec![], subs(3, 97), vec![], subs(5, 80)];
        rounds.extend(std::iter::repeat_n(Vec::new(), 9)); // 6..=14
        liq_blocks(rounds)
    }

    #[test]
    fn liquidation_determinism_serial_pipelined_and_replay_are_identical() {
        use torus_core::liquidation::LIQUIDATOR_VAULT;
        let blocks = liq_det_blocks();
        let (serial, root_s, db_s) = run_fixture(OracleRun::Serial, &blocks, liq_det_fixture);
        let (piped, root_p, _) = run_fixture(OracleRun::PipelinedParked, &blocks, liq_det_fixture);
        let (replay, root_r, _) = run_fixture(OracleRun::Replay, &blocks, liq_det_fixture);
        // Non-vacuous: all three mechanisms ran.
        assert_eq!(signed_pos_of(&db_s, &oracle_addr(71)), FixedPoint::ZERO, "T1 backstopped");
        assert_eq!(signed_pos_of(&db_s, &oracle_addr(74)), FixedPoint::ZERO, "T2 sold into the book");
        assert_eq!(signed_pos_of(&db_s, &oracle_addr(73)), px(10), "M bought T2's 10 @ 95");
        assert_eq!(signed_pos_of(&db_s, &LIQUIDATOR_VAULT), FixedPoint::ZERO, "vault ADL'd");
        assert_eq!(signed_pos_of(&db_s, &oracle_addr(72)), -px(10));
        assert_dumps_equal(&serial, &piped, "liquidation: serial vs pipelined (parked)");
        assert_dumps_equal(&serial, &replay, "liquidation: serial vs crash replay");
        assert_eq!(root_s, root_p);
        assert_eq!(root_s, root_r);
    }
```

(Numbers: block 4 mark 97, MM per account = 970 / 40 = 24.25: T1 AV 40 − 30 = 10, 3·10 < 48.5 ⇒
backstop; T2 AV 50 − 30 = 20 ⇒ stage 1, cap 97 − 2.425 = 94.575 ≤ 95 ⇒ fills M's bid,
T2 available 0. Block 6 mark 80 (block 5's rows overwrite block 3's), previous mark 97 (stored
by block 5's step): vault long 10 @ 97 + 10 ⇒ AV −160 ⇒ closes against S at 97 ⇒ vault
available 10, S short 10 left. If an intermediate assertion fails, fix the fixture numbers, not
the rule.)

(b) `torus-integration-tests/tests/chaos.rs` — after
`oracle_block_start_step_keeps_incremental_root_equal_to_full_scan`:

```rust
/// Item 3: the liquidation step (cooldown / prev-mark / cursor rows,
/// vault positions) keeps the incremental native root equal to the full scan.
#[test]
fn liquidation_step_keeps_incremental_root_equal_to_full_scan() {
    use torus_core::liquidation::LIQUIDATOR_VAULT;
    use torus_core::position::{MarginType, PositionManager};
    use torus_economics::{StakingManager, ValidatorState, ValidatorStatus, MIN_SELF_DELEGATION};
    use torus_state::cf::CF_NATIVE_MARKETS;
    use torus_state::native_trie::{build_native_trie_to_cf, native_root_full, persisted_native_root};
    use torus_state::NativeStateOverlay;

    let tmp = tempfile::TempDir::new().unwrap();
    let state_db = StateDb::open(tmp.path()).unwrap();
    let staking = StakingManager::new(state_db.clone());
    for (v, mult) in [(21u8, 1u64), (22, 1), (23, 3)] {
        staking
            .put_validator(&addr(v), &ValidatorState {
                address: addr(v),
                pubkey: [v; 32],
                commission_bps: 0,
                self_stake: MIN_SELF_DELEGATION * U256::from(mult),
                total_delegated: U256::ZERO,
                status: ValidatorStatus::Active,
                jailed_until: None,
                last_commission_change_block: None,
            })
            .unwrap();
    }
    state_db.put_cf_raw(CF_NATIVE_MARKETS, &1u64.to_be_bytes(), b"listed").unwrap();
    let pm = PositionManager::new(state_db.clone());
    for i in 1..=8u8 {
        let mut b = pm.get_native_balance(&addr(i)).unwrap();
        b.available = b.available + fp(40 * i as i64);
        pm.put_native_balance(&addr(i), &b).unwrap();
        pm.apply_fill(&addr(i), 1, true, fp(10), fp(1_000), MarginType::Cross).unwrap();
        pm.apply_fill(&addr(9), 1, false, fp(10), fp(1_000), MarginType::Cross).unwrap();
    }
    let mut b = pm.get_native_balance(&addr(9)).unwrap();
    b.available = b.available + fp(10_000_000);
    pm.put_native_balance(&addr(9), &b).unwrap();
    build_native_trie_to_cf(&state_db).unwrap();

    for block in 1..=30u64 {
        let overlay = NativeStateOverlay::new(state_db.clone());
        let mut ctx = NativeExecContext::new(
            overlay.clone(), block, 1_700_000_000 + block, 0, 1_000, 100,
            Address::ZERO, Address::ZERO, Address::ZERO,
        );
        NativeExecutor::begin_block_oracle(&mut ctx);
        let actions: Vec<_> = if block <= 12 {
            [21u8, 22, 23]
                .iter()
                .map(|&v| (addr(v), NativeAction::SubmitOraclePrices(OracleSubmission {
                    prices: vec![(1, fp(1_000 - 8 * block as i64))],
                    timestamp: 0,
                })))
                .collect()
        } else {
            Vec::new()
        };
        NativeExecutor::execute_batch(&mut ctx, &actions);
        NativeExecutor::run_liquidations(&mut ctx);
        assert!(ctx.fatal_error.is_none(), "block {block}");
        overlay.flush_with_native_trie(&state_db).unwrap();
        assert_eq!(
            persisted_native_root(&state_db).unwrap(),
            native_root_full(&state_db).unwrap(),
            "block {block}: incremental native root != full scan (liquidation step)"
        );
    }
    // Non-vacuous: trader 1 (collateral 40) is backstopped at the first mark
    // (992); the vault may be ADL'd flat later, so assert on the trader.
    assert!(pm.get_position(&addr(1), 1).unwrap().is_none(), "non-vacuous: liquidations ran");
    let _ = LIQUIDATOR_VAULT;
}
```

**Implementation:** none expected; fix what the gates expose.

**validate:** `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib liquidation_determinism && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib oracle_determinism && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-integration-tests --test chaos` · depends_on: [8]

`══ COMMIT 8 ══` `test(liquidation): serial = pipelined = replay; incremental root = full`

### T10 — docs · depends_on: [8, 9]

* `docs/parity-audit-fixes-s515.md`: behaviour-table row "Liquidation" (replaces the F1 row
  :18 text "Not wired"): trigger, stages, ADL, no penalty, vault address, defaults D1-D11; remove
  the deferred item :122-124; *Deployment requirements*: lockstep + fresh genesis (new root CF).
* `docs/plans/liquidation.md`: *Status* → implemented (commit range); record the user's answers
  — D1-D11, C1, C4 decided (user, s517); C3 / C5 known limitations.
* `docs/plans/oracle-aggregation.md` *Status*: "Next: liquidation" → done.

**validate:** `true` · depends_on: [8, 9]

`══ COMMIT 9 ══` `docs: HL liquidation behaviour and deployment notes`

### T11 — end-to-end verification (lead) · depends_on: [9, 10]

See below.

## Verification (end-to-end)

1. `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --test margin_tests --test liquidation_tests && /tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-core --lib`
2. `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-state --lib native_trie`
3. `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge` (all files incl. `liquidation_tests`, `account_margin_tests`,
   `oracle_block_tests`, position-cache / reduce-only / margin suites)
4. `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-consensus --lib`
5. `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-integration-tests --test chaos`
6. 12-crate run:
   `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 --no-fail-fast -p torus-types -p torus-state -p torus-evm -p torus-core -p torus-consensus -p torus-bridge -p torus-rpc -p torus-mempool -p torus-economics -p torus-genesis -p torus-telemetry -p torus-integration-tests`
   Baseline **1684 pass / 2 known torus-core fails** (`order_book::cancel_batch::many_tests::
   cancel_all_many_falls_back_on_stale_or_shared_indexes`,
   `order_book::queue_lookup_tests::id_lookup_agrees_with_linear_scan_under_mutation`).
   Expected: −17 removed (13 old `liquidation_tests.rs` + 4 `liquidation.rs` module tests) +
   34 new (T1 2, T2 2, T3 1, T4 2, T5 5, T6 5, T7a 5, T7b 2, T7c 1, T7d 3, T7e 3, T8 1,
   T9 2) ⇒ **≈ 1701 pass, same 2 fails**. Recount at T11.
7. `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh check --workspace --all-targets` and `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh clippy -p torus-core -p torus-bridge -p torus-consensus --all-targets`.
8. Perf sanity (the step's positions-CF walk + valuations per native block):
   `/tmp/claude-1000/-home-crab-projects-Torus-hyperBFT/3494bc74-14e1-46bc-8bdd-c0275a4443cf/scratchpad/cargo-serial.sh test -j6 -p torus-bridge --release --test engine_parallel_bench -- --ignored --nocapture`
   before/after (settle unchanged — no index; the step adds one positions-CF walk per block).

## Rollback

Commits per *Commit boundaries*. Revert in reverse order (9 → 1). Commits 1 (MM formula) and 4
(margin configs) are independent of the step and may stay; reverting 2 (root CF) requires
reverting 6+ first; commit 3 (CancelAll) is independent. State layout changes (new root CF, liquidation rows, vault rows): fresh genesis
either way; a rollback is again a lockstep upgrade.

## Deployment

Lockstep upgrade of every validator + fresh genesis: new native-root CF (tag 6) changes every
native root once the liquidation step writes a row; margin
configs now come from market rows (identical IM for the current 20x markets, D11); the
liquidation step changes positions / balances / books at the end of native blocks; the native
phase also runs while cooldown / cursor rows exist. Optionally seed the vault through genesis
`native_balances` (no code needed). Client-visible: liquidation fills appear as ordinary trades
(C8); the vault is a visible account; `CancelAll` now also cancels pending stops (T3).

## Decisions, known limitations, risks

Decided (user, s517): **C1** no separate account index — candidates are a round-robin walk of
`CF_NATIVE_POSITIONS` from the cursor in `CF_NATIVE_LIQUIDATION` (2048 checks / 64 actions per
block, carry-over); **C4** user `CancelAll` also cancels pending stops and releases their margin
(T3, own commit); **D1-D11** as proposed (see the design table).

Known limitations (recorded, user s517):
* **C3** — a listing's `maintenance_margin_bps` is ignored: MM = ½ IM at max leverage (HL);
  the market row has no slot for it.
* **C5** — the vault cannot unwind (no orders / strategy) until the deposits branch; it only
  shrinks through ADL; its balance can go negative (D9) and is ADL'd when its AV < 0.

Other flags:
* **C6 — `CF_NATIVE_MARKETS` off-root** now drives margin (same exposure as oracle R6).
* **C7 — ADL ranking AV** uses entry fallback for counterparties' unmarked markets.
* **C8 — no liquidation flag** in trade rows.
* **C9 — governance `maintenance_margin_bps` / `max_leverage`** params stay validated-but-unread.
* **R1 — cost:** per native block one `iterate_cf(CF_NATIVE_POSITIONS)` walk (O(all position
  rows), overlay merge) + ≤ `SCAN` valuations + M mark reads; ADL scans the CF once per ADL'd
  position (rare). If the walk shows up in the perf sanity, a bounded range iterator on the
  backend is the follow-up (not needed for correctness).
* **R2 — stage 1 on thin books:** IOC remainder cancelled; retried next block; the backstop
  catches a falling account (D1 bounds each fill's tail at one MM_pos).
* **R3 — stops cascade** (D7) can move other accounts in the same step; they are valued later in
  the same pass (ascending order) or next block — deterministic either way.
* **R4 — no per-action rollback:** a storage error mid-step fail-stops (new check, F11); a
  rejected liquidation order is a result.
* **R5 — between commits 5 and 7** liquidation is unwired (as today); the old buggy code is
  deleted in 5.
* **R6 — `alloy_primitives::U512`** — verify the alias at T5 (fallback `ruint::aliases::U512`).
  (`FixedPoint::from_raw` is not `const`: the threshold constant is raw i128.)
