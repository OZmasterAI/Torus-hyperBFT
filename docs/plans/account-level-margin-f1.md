# Design: Account-level margin (F1)

**Status: implemented (s517, see `account-level-margin-f1-impl.md`).** User decisions
D1 = strict HL (the account check is the only placement gate; `available` may go
negative), D3 = SAFE withdrawal (reservations are not collateral for positions), T7
decision (projected releases credited only to match-checked orders); design
corrections D2, D4-D8 and the review fixes are recorded in the impl plan.

Branch `fix/parity-audit-bugs` (s517). Follow-up to `docs/parity-audit-fixes-s515.md`
("Known deferred items", F1).

## Problem

Margin is per ORDER, not per account. `NativeBalance` is `available` +
`order_margin`; a fill releases the order's reservation and locks nothing for
the position (`apply_fill_cached` only returns realized PnL). So:

* 100 USDC at 20x: one order opens 2,000 notional, its reservation is released
  at fill, and the next order (same batch or a later block) gets the same 100
  again. Repeating this gives unbounded leverage (~40x after two orders).
* Leverage tiers are looked up on the order's fill notional, not the
  position's size, so a large position built from small orders stays at the
  50x/20x tier.
* Withdrawals (`Lockbox::withdraw_from_native_to`, `lockbox.rs:146`) only check
  `available >= amount`, so a trader can withdraw the collateral behind open
  positions.

Hyperliquid (cross margin): account value = collateral + unrealized PnL at
mark; initial margin of a position = size x mark / leverage (leverage from the
position-size tier); margin is checked when an order is placed and again when
it matches; a withdrawal must leave the account's margin requirement covered.

## Context (memory + exploration, s517)

* Block flow: `execute_batch_phases` (NE = `torus-bridge/src/native_executor.rs`:3451)
  * Phase 1 (NE:3541): non-placement actions in order: withdrawals, cancels, modifies.
  * Phase 2 (NE:3608): reservations; serial or sharded BY SENDER
    (`phase2_parallel_prepare`, NE:4059). A per-sender account view fits here.
  * Phase 3 (NE:3856): matching, one worker per market in parallel. Checked
    takers carry a `TakerMarginLimit { budget, tiers, hold_price }`
    (`order_book.rs:157`); `budget` = reservation + `available` after it.
  * Phase 4: settlement in sorted market order (sequential NE:4180, C3
    parallel NE:4382); PnL credited to `available`.
  * Single-order path: `place_order_inner` (NE:5298). Stops re-enter it at trigger.
* **Budget overlap:** a sender's checked takers in different markets match in
  parallel and each can spend the same free `available`. An account-level check
  inside a market worker cannot see the other markets' fills in the same batch.
* `MarginEngine::{check_initial_margin, cross_margin_equity,
  total_maintenance_margin}` (`torus-core/src/margin.rs`) exist but only
  `liquidation.rs` and tests call them. `cross_margin_equity` subtracts
  `order_margin` from `available`, which is already net of it (double count).
* Liquidation (`LiquidationEngine`, `liquidation.rs`) and
  `run_liquidation_checks` (NE:6465) have no production caller.
  `ctx.margin_configs` is never populated, so every market uses
  `DEFAULT_ORDER_MAX_LEVERAGE = 20` (flat, no tiers) in production.
* The mark is `None` in production: `aggregate_oracle_prices` has no caller
  (item 2 of this branch fixes it). `mark_price` (NE:4969) returns `None` when
  the price is stale or absent.
* No funding. Every position is `MarginType::Cross`.
* `positions_for_trader` (`position.rs:260`) prefix-scans one account's positions.
* Tests: `torus-bridge/tests/market_order_margin_tests.rs` (helpers `make_ctx`,
  `fund_native`, `set_mark`, `bal`, `pos`), `maker_margin_release_tests.rs`
  (tiers via `ctx.margin_configs`), `reduce_only_tests.rs` (`run` over single
  and batch paths), determinism: `parallel_settle_tests.rs`,
  `engine_parallel_tests.rs`.

## Options

Common to all options: `position_im(pos, mark) = order_initial_margin(tiers,
size x mark)`, i.e. the tier is picked on the POSITION's notional; one formula
shared by placement, match, withdrawal and liquidation.

### Option A: Stored position margin (lock at fill)

Add `position_margin` to `NativeBalance` (schema v2). At settlement, each fill
moves the position's IM delta (at fill price, position-size tier) from
`available` into `position_margin`; closing releases it pro rata. Everything
else keeps checking `available`, so withdrawals are fixed automatically and
the budget model at match time is unchanged.

* Pros: small diff; no prefix scans; no mark needed; parallel matching stays as is.
* Cons: not Hyperliquid: unrealized profit is never usable and unrealized
  loss is never charged (collateral looks healthy while underwater); the lock
  is priced at entry and does not follow the mark; a fill can push
  `available` negative (settlement cannot refuse a fill after matching). The
  cross-market budget overlap in Phase 3 remains.
* Effort: Medium. Risk: Medium (schema change, drift from HL semantics).

### Option B: Computed account margin (Hyperliquid model)  ← recommended

No new stored fields. Per sender:

```
equity   = available + order_margin + Σ upnl(pos, mark)        (collateral + UPnL)
required = Σ position_im(pos, mark) + order_margin
free     = equity − required
```

* **Placement (Phase 2 / single path):** load the sender's account view once
  per batch (one prefix scan per unique sender; Phase 2 is already sharded by
  sender, so this is deterministic). An order is accepted iff its IM
  increment — `position_im(pos after the order fully fills) − position_im(pos)`,
  with closing quantity free — fits in `free`. The reservation itself stays
  as today (debited from `available`), so the release telescoping is unchanged.
* **Match time (Phase 3):** fix the overlap by giving each checked taker an
  EXCLUSIVE budget: its reservation + a share of the sender's free margin
  assigned in Phase 2 in batch order (first checked taker of the sender takes
  what is left, later ones get what remains after it). Nothing is shared
  across market workers, so parallel matching stays deterministic. The
  book's `need` switches to the position-size tier (it already has the
  sender's position via `ReduceOnlyPositions`).
* **Withdrawal (Phase 1 + CoreWriter):** allowed iff `amount <= min(available,
  free)` (unrealized profit is not withdrawable beyond collateral; HL rule to
  be confirmed during planning).
* **Liquidation:** `cross_margin_equity` / `total_maintenance_margin` rewritten
  on the same formulas (fixes the double count); wiring liquidation into block
  execution stays a separate item (needs the mark and margin configs).
* **No mark (until item 2):** fall back to the position's entry price
  (UPnL = 0, IM at entry notional). Deterministic, and the same fallback
  placement already uses for market orders.
* Pros: matches Hyperliquid; one formula everywhere; no schema change; closes
  the Phase-3 overlap.
* Cons: a prefix scan per unique sender per batch (hot path, needs a bench
  check); UPnL of fills in OTHER markets in the same batch is not seen until
  the next batch (conservative only if prices moved against the trader —
  acceptable, HL also snapshots); maker fills are not re-checked, so a maker
  fill that crosses a tier boundary can leave the account slightly
  under-margined (liquidation's job).
* Effort: Large. Risk: Medium (touches all three placement paths + book).

### Option C: A + B hybrid

Stored lock (A) for settlement/withdrawal plus the computed equity check (B)
at placement only. Two sources of truth for the same number. Rejected: they
drift as soon as the mark moves.

## Recommendation

Option B. It is the only one that makes the statement "margin is
account-level, like Hyperliquid" true, it needs no schema migration, and the
per-sender sharding of Phase 2 already gives a deterministic place to compute
the account view. The exclusive budget split fixes the cross-market overlap
without serializing matching.

## Not Building (YAGNI)

* User-selected leverage per market (every order uses the tier max, as today).
* Isolated margin (no path produces it).
* Funding payments.
* Wiring liquidation into block execution (separate item; formulas aligned here).
* Populating `ctx.margin_configs` from governance (separate item; tests set tiers directly).

## Decisions (user, s517)

1. **Model:** Option B (computed, HL-style).
2. **No mark:** entry-price fallback (UPnL 0, IM at entry notional) until item 2
   wires oracle aggregation.
3. **Liquidation:** align `MarginEngine` / `LiquidationEngine` formulas with the
   shared IM / equity functions (fix the `order_margin` double count); wiring
   liquidation into block execution stays a separate item.
4. **Makers: HL-style.** HL has order status `marginCanceled` ("Canceled because
   insufficient margin to fill"), separate from `perpMarginRejected` at
   placement. The book checks every maker fill against that fill's share of the
   maker's reservation + the maker's start-of-batch free-margin snapshot; a maker
   that would breach is cancelled (reservation released) and the taker continues
   with the next maker. The snapshot is per sender and read-only in Phase 3, so
   parallel matching stays deterministic; a maker filling in several markets in
   one batch can overshoot by at most that snapshot, caught by the next check.
5. **Withdrawals: HL exact.** Allowed iff `amount <= available` AND equity after
   the withdrawal `>= max(Σ position IM, 10% x Σ position notional)`
   (HL `transfer_margin_required`).
   Applies to `TransferToSpot`, `Withdraw`, CoreWriter `LockboxWithdraw`.

Sources: hyperliquid.gitbook.io/hyperliquid-docs/trading/margining (IM formula,
transfer margin), .../for-developers/api/info-endpoint (order statuses
`marginCanceled`, `perpMarginRejected`, `perpMaxPositionRejected`).
