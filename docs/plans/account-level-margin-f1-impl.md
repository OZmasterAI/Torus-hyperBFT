# Implementation Plan: Account-level margin (F1) — Option B, HL cross margin

Design: `docs/plans/account-level-margin-f1.md` (decisions s517 are binding).
Branch `fix/parity-audit-bugs` @ `d88d581`. Consensus-visible (lockstep upgrade,
fresh genesis is already required by this branch).

Tasks are TDD: write the failing test, see it RED, implement, run `validate`.
Only the lead runs cargo (serialized). Line numbers verified at `d88d581`.

## Verified anchors

| What | Where |
|------|-------|
| `order_initial_margin` (THE IM formula, raw-integer division) | `torus-core/src/margin.rs:65` |
| `MarginEngine::cross_margin_equity` double count (`available - order_margin`) | `margin.rs:201-221` (unit test pinning it `:444-463`) |
| `total_maintenance_margin` (skips positions without oracle price) | `margin.rs:224-251` |
| `LiquidationEngine::check_cross_liquidation` uses both | `liquidation.rs:115-147` |
| `Position`, `NativeBalance`, `positions_for_trader` (prefix scan) | `position.rs:52,147,260` |
| `NativeStateOverlay::iterate_cf` merges DB + parent + pending (so the scan sees in-block writes) | `torus-state/src/backend.rs:1166-1189` |
| `Lockbox::withdraw_from_native_to` (`available >= amount` only) | `lockbox.rs:146-194` |
| `TakerMarginLimit` / `need` / `affordable` | `order_book.rs:157-219` |
| `MatchMargin` | `order_book.rs:222-228` |
| `ReduceOnlyPositions`, `reduce_only_allowance` | `order_book.rs:241-284` |
| `PlaceResult` (+ `rejected()`) | `order_book.rs:65-105` |
| `OrderBook` transient fields (`reduce_only_positions`) / `new()` / deserializer | `:480`, `:565`, `:3087` |
| `place_order_with_margin` (FOK pre-check `:762-775`, match `:778-786`) | `order_book.rs:613-883` |
| `set_/clear_reduce_only_positions` | `order_book.rs:959-965` |
| `execute_match` / `match_at_level` (taker check `:1554-1576`, RO cut `:1521-1543`) | `order_book.rs:1365-1621` |
| `can_fill_completely` (FOK) | `order_book.rs:1720-1772` |
| book margin unit tests (`mod taker_margin_limit_tests`) | `order_book.rs:5261-5481` |
| `BalanceCache` (write-back) | `native_executor.rs:114-220` |
| `PreparedOrder` / `PrepOutcome` | NE:222-231 / NE:418-427 |
| `execute` dispatch (TransferToSpot NE:3274, Withdraw NE:3278) | NE:3162 |
| `execute_batch_phases`: Phase1 NE:3541, Phase2 NE:3608, basis NE:3651, sharded stitch NE:3699-3739, serial loop NE:3741-3847, Phase3 NE:3856-3932, Phase4 clear NE:3952-3957, cache flush NE:4019-4024 | NE:3451 |
| `phase2_parallel_prepare` | NE:4059-4173 |
| `maker_margin_releases_cfg` | NE:4869-4929 |
| `mark_price` | NE:4969-4974 |
| `match_margin_checked` / `never_rests` / `margin_tiers` / `taker_margin_limit` / `is_stop` | NE:4986 / 5003 / 5015 / 5023 / 5040 |
| `phase2_reservation_basis` | NE:5071-5140 |
| `place_order_inner` (reservation NE:5330-5417, book setup NE:5419-5458) | NE:5298-5540 |
| `run_triggered_stops` (re-enters `place_order_inner`) | NE:5268 |
| `exec_modify_order` (extra reservation NE:5875-5892) | NE:5784-5911 |
| `exec_withdraw_from_native` / `exec_withdraw_to` | NE:6338 / NE:6356 |
| CoreWriter `LockboxWithdraw` -> `NativeAction::TransferToSpot` | NE:6852 |
| `MarketWorkerPool::match_parallel` / `_capped` / `match_market` | `market_workers.rs:75 / 85 / 306` |

## The model (one set of formulas, `torus-core/src/margin.rs`)

```
px(pos)        = mark(market) else pos.entry_price             (decision 2)
equity         = available + order_margin + Σ upnl(pos, px)    (order_margin counted ONCE)
position_im    = Σ order_initial_margin(tiers(market), |size| × px)   (position-size tier)
pos_net        = Σ upnl − position_im
free           = equity − (position_im + order_margin) = available + pos_net
withdrawal ok  ⇔ amount <= available                                          (cash bound, kept)
                 ∧ equity − order_margin − amount >= max(position_im, 10% × Σ |size| × px)
                 (s517 user decision, SAFE variant: reservations of resting orders are NOT
                  collateral for the positions — i.e. available + upnl − amount >= required)
placement_need = max( IM(pos after a full fill at res_price) − IM(pos),          (closing part releases IM)
                      can_rest ? IM(pos + opening × res_price) − IM(pos) : 0 )   (the resting opening part)
                 → accepted iff need <= 0 (only reduces) or need <= free.  This is the ONLY placement
                   gate (s517 user decision, STRICT HL): the old `available >= reservation` gate is
                   removed on all three placement paths and on ModifyOrder. The reservation is still
                   debited from `available`, which may therefore go NEGATIVE (UPnL funds it).
                   Stops (pending) are checked too, as resting orders at their reservation price.
match need     = IM(|pos0|×px − closed×px + charged + hold × opening_rest) − IM(|pos0|×px)
                 → a fill fits iff it only CLOSES (q <= closing capacity) OR need <= budget
                   (a net-IM-reducing flip must still fit its opening part: need <= budget)
budget         = own reservation + the sender's running free margin in this book
maker fill     = IM delta of the fill (closing part free) − the fill's share of the maker's reservation
                 <= the maker's running free (snapshot) — else the maker is cancelled (HL marginCanceled)
```

Flat account, no UPnL: `free = available`, `placement_need = reservation`, `budget = available`
— identical to today (need <= free ⇔ reservation <= available, same "insufficient margin: need X,"
prefix), which keeps every existing flat-taker test unchanged.

### Negative `available` (D1 = strict HL): every consumer, and why it stays correct

`available` is the cash part of collateral; `available + order_margin` is total collateral and
is what every conservation argument uses. `FixedPoint` is `i128`; Borsh writes the raw i128
big-endian (`position.rs:19-27`, `:166-194`), so negative values round-trip and hash exactly
like today's negative-after-realized-loss balances.

| Consumer | Where | Negative `available` |
|----------|-------|----------------------|
| Reservation / release telescoping (A5) | reserve NE:3815, 4127, 5398, 5887; releases NE:4236, 4322, 4573, 4633, 5243, 5608, 5670, 5745 | OK. Amounts are the formula `reserve_for_qty_cfg`, independent of the balance; Σ released = Σ reserved still holds. Every release moves `min(amount, order_margin)` (NE:5241 and the batch equivalents) — clamped on `order_margin`, never on `available`. |
| Placement early-outs `available < reservation` | NE:3797 (serial), NE:4110 (sharded), NE:5381 (single), NE:5881 (modify) | REMOVED (T5/T7/T10); replaced by the account check. |
| Match budget (`TakerMarginLimit.budget`, pools) | T4/T5/T7 | OK. Budget = own reservation (>= 0) + running free; the pool `available_end + pos_net` is signed by design (D2) and may be negative → the taker can only close. Nothing assumes `available >= 0`. |
| Maker snapshot | T9 `maker_account` | OK: `free = available + pos_net`, signed. |
| Realized PnL credit | `position.rs:363`, NE:4591, NE:4741 | OK: signed add (already produced negatives before F1). |
| Withdrawals / lockbox | `lockbox.rs:158` `available < amount` | OK and KEPT: a negative-available account cannot withdraw anything (tested, T5). Deposits `lockbox.rs:84,122` use `checked_credit` (signed). |
| Liquidation equity | `margin.rs` (T2) | OK after T2: equity = available + order_margin + UPnL. |
| **Liquidation settlement deficit** | **`liquidation.rs:233-236`** | **NOT tolerant**: `available < 0` is taken as a loss deficit and zeroed while `order_margin` still holds collateral that is later released — mints the reservation back. Fixed in T2 (deficit on `available + order_margin`). Not wired in production. |
| Socialized-loss eligibility | `liquidation.rs:397` | Tolerant (conservative): a negative-cash account is excluded as ineligible. Not wired. |
| **Precompile 0x0801 `getBalances`** | **`precompiles.rs:568,572`** (`encode_fp_as_u128` = `raw as u128`, `precompiles.rs:203-205`) | **NOT tolerant**: a negative i128 wraps to ~3.4e38 `uint128` — a contract reading it sees an enormous balance. Fixed in T14 (clamp at 0; ABI unchanged). Order margin is never negative. |
| RPC `torus_getBalances` | `torus-rpc/src/torus.rs:867-873` via `hex_fp` (`types.rs:249-256`) | Tolerant: `hex_fp` prints a signed `-0x…`. Clients may not expect a sign on `available_balance` — documented in T12; `native_balance` (= available + order_margin) stays >= 0 unless the account is truly under water. |
| Genesis | `torus-genesis/src/lib.rs:462` | Input only; unaffected. |
| Fees | NE:1380, NE:6537 | No NativeBalance is debited for fees (`total_native_fees` is a separate u64 counter, never read from a balance). |
| `check_initial_margin` isolated branch | `margin.rs:145` | No production caller; unchanged. |
| u64/u128 conversions of `available` | only `precompiles.rs:568,572` (above); `fp_to_wei` in lockbox converts the withdrawn AMOUNT (> 0), not the balance | — |

## Success criteria (each is a named test below)

1. **40x repro closed** — 100 USDC, 20x, two market sells of 20 @100: second rejected in a
   later block; in one batch the two together open at most 20. Single / batch-serial /
   batch-sharded (`forty_x_*::{single,batch,parallel}`, T5/T7).
2. **Position-size tier** — tiers `<=1,000 → 20x, above → 5x`, 200 funded, GTC buys of 5 @100:
   the 3rd (position 1,500 → IM 300, +250 > free 150) is rejected, per block and in one batch,
   all paths (`position_tier_*`, T5/T7). Book level: `f1_increase_is_charged_at_the_position_tier` (T4).
3. **UPnL counts** — long 10 @100 (IM 50): mark 110 → a 100-IM order is accepted; mark 100 → rejected;
   mark 95 → even a 5-IM order is rejected (`upnl_counts`, T5/T7). **D1 strict HL:** profit funds a
   reservation beyond cash (`available` → −200, accepted), no mark → rejected
   (`upnl_funds_a_reservation_beyond_cash`); a negative-cash account withdraws nothing and closing
   the position restores `available >= 0` with collateral conserved
   (`negative_cash_cannot_withdraw_and_closing_restores_it`); ModifyOrder likewise (T10).
4. **No-mark fallback** — entry price, UPnL 0, even after a trade at another price (`no_mark_values_at_entry`).
5. **Cross-market same-batch overlap closed** — two checked market sells of one sender in two
   markets in one batch open at most what 100 of collateral supports (`cross_market_*`, T5/T7).
6. **Maker marginCanceled** — an under-water maker's bid is cancelled, its reservation released,
   the taker fills the next bid; a funded maker fills (`maker_margin_cancel_*`, T9; book unit tests T8,
   incl. FOK pre-check mirroring the cancel).
7. **Withdrawal rule incl. 10% floor, SAFE variant** (equity − order_margin − amount >= required;
   amount <= available) on `TransferToSpot`, `Withdraw{to}` (T3) and CoreWriter `LockboxWithdraw`
   drained next block (`lockbox_queue_tests`, T3); the 5x example (IM 100, reservation 100,
   available 100 → withdraw 100 rejected) at core and bridge level.
7b. **Negative-`available` consumers** — liquidation deficit counts order margin (T2); the 0x0801
   reader never wraps a negative balance into a huge `uint128` (T14).
8. **Liquidation equity** counts `order_margin` once; maintenance at entry price without a mark (T2).
9. **Determinism** — `engine_parallel_tests` + `parallel_settle_tests` byte-identical, plus a new
   F1-shaped thread-matrix test (T11).
10. **Closing / reduce-only fills stay free** — existing section (i) of `market_order_margin_tests`
    passes (one fixture updated, justified in T5), plus `f1_closing_fills_fit_an_under_margined_account` (T4).
11. **Existing margin tests** keep passing; the only intended edits are listed per task with justification.

## Where determinism could break, and how this plan keeps it

| Hazard | Guard |
|--------|-------|
| Serial vs sharded Phase 2 computing different outcomes | T6 extracts ONE `prepare_one` used by both paths; T7 adds F1 only there. Views / projections are per-sender state inside the fold (sender shards are disjoint). |
| Pool assignment order | Pools are computed after Phase 2 from `PreparedOrder.index` sorted ascending (flat order), never from `HashMap` iteration. |
| Phase-3 maker snapshots read state concurrently | Workers only READ the backend (`positions_for_trader`, balances, oracle). Balances/positions are write-back cached (`BalanceCache`, `PositionCache`) until NE:4019-4024, so the backend is the frozen post-Phase-1 state for the whole of Phase 3 on every node. Workers never read `bal_cache`. |
| Per-market running state | `AccountMargins` lives inside the market's `OrderBook` (one worker), `BTreeMap`-keyed; cleared at NE:3953 with the reduce-only map. Not serialized, not hashed. |
| Maker cancels in two settle modes | They ride in `PlaceResult.margin_cancels` and are released by the pure `maker_margin_releases_cfg` (used by sequential, C3 parallel and single paths). |
| FOK pre-check vs matching disagree on maker cancels | Both call the same `maker_fill_fits`; the pre-check runs on clones. |
| Overflow panics | Every new product uses `checked_*`; overflow = reject / not affordable / maker cancelled (deterministic). |
| Resident-book handoff carrying stale per-batch state | `clear_account_margins()` next to every `clear_reduce_only_positions()` (NE:3953, NE:5458). |

---

## Tasks

### T1 — core: account-margin formulas (`margin.rs`)

**Test first** — append to `crates/torus-core/tests/margin_tests.rs` (extend the `use` line with
`order_initial_margin, placement_need, AccountView, MarginTier`):

```rust
fn cross(market_id: u64, is_long: bool, size: i64, entry: i64) -> Position {
    Position {
        trader: addr(1),
        market_id,
        is_long,
        size: fp(size),
        entry_price: fp(entry),
        realized_pnl: FixedPoint::ZERO,
        isolated_margin: FixedPoint::ZERO,
        margin_type: MarginType::Cross,
    }
}

fn tiers_20_then_5() -> Vec<MarginTier> {
    vec![
        MarginTier { max_notional: fp(1_000), max_leverage: 20 },
        MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
    ]
}

/// F1: equity = available + order_margin + UPnL (order margin ONCE); UPnL,
/// notional and IM at the mark, at the entry price without one (s517 #2).
#[test]
fn account_view_values_positions_at_mark_else_entry() {
    let bal = NativeBalance { available: fp(1_000), order_margin: fp(300) };
    let ps = [cross(1, true, 10, 100), cross(2, false, 5, 200)];
    // m1 mark 110: UPnL +100, notional 1,100, IM 55. m2 no mark: 0 / 1,000 / 50.
    let v = AccountView::build(&bal, &ps, |m| (m == 1).then(|| fp(110)), |_| None).unwrap();
    assert_eq!(v.upnl, fp(100));
    assert_eq!(v.notional, fp(2_100));
    assert_eq!(v.position_im, fp(105));
    assert_eq!(v.equity(), fp(1_400));
    assert_eq!(v.pos_net(), -fp(5));
    assert_eq!(v.free(), fp(995));
}

/// F1: a position's IM uses the POSITION's notional tier (1,500 → 5x).
#[test]
fn account_view_charges_the_position_size_tier() {
    let t = tiers_20_then_5();
    let bal = NativeBalance { available: fp(1_000), order_margin: FixedPoint::ZERO };
    let v = AccountView::build(&bal, &[cross(1, true, 15, 100)], |_| None, |_| Some(t.as_slice())).unwrap();
    assert_eq!(v.position_im, fp(300));
}

/// F1 (s517 #5, SAFE variant): amount <= available AND
/// equity − order_margin − amount >= max(Σ IM, 10% × Σ notional).
#[test]
fn account_view_withdrawal_rule() {
    let bal = NativeBalance { available: fp(150), order_margin: FixedPoint::ZERO };
    let v = AccountView::build(&bal, &[cross(1, true, 10, 100)], |_| None, |_| None).unwrap();
    assert_eq!(v.transfer_required(), fp(100)); // max(IM 50, 10% x 1,000)
    assert!(v.withdrawal_allowed(fp(50)));
    assert!(!v.withdrawal_allowed(fp(51)));
    assert!(!v.withdrawal_allowed(fp(151)));
    let flat = AccountView::build(&bal, &[], |_| None, |_| None).unwrap();
    assert!(flat.withdrawal_allowed(fp(150)));
}

/// F1 (s517 D3 = SAFE): a resting order's reservation is not collateral for
/// the positions. 5x: long 5 @100 (IM 100, 10% floor 50), 100 reserved by a
/// resting order, 100 available: withdrawing 100 would leave the positions
/// backed only by the reservation → REJECTED (equity − amount = 100 would
/// have passed). Nothing is withdrawable (100 + 0 − 1 < 100).
#[test]
fn account_view_reservations_do_not_back_withdrawals() {
    let t = vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 }];
    let bal = NativeBalance { available: fp(100), order_margin: fp(100) };
    let v = AccountView::build(&bal, &[cross(1, true, 5, 100)], |_| None, |_| Some(t.as_slice())).unwrap();
    assert_eq!(v.transfer_required(), fp(100));
    assert!(!v.withdrawal_allowed(fp(100)));
    assert!(!v.withdrawal_allowed(fp(1)));
}

/// F1 (D1 = strict HL): a negative cash balance withdraws nothing.
#[test]
fn account_view_negative_available_withdraws_nothing() {
    let bal = NativeBalance { available: -fp(200), order_margin: fp(300) };
    let v = AccountView::build(&bal, &[cross(1, true, 10, 100)], |_| Some(fp(150)), |_| None).unwrap();
    assert!(v.free() > FixedPoint::ZERO); // UPnL 500 − IM 75 − 200
    assert!(!v.withdrawal_allowed(fp(1)));
}

/// F1: placement need — same side at the position tier, closing <= 0, a
/// flip releases the old IM, a resting order's opening part counts.
#[test]
fn account_placement_need() {
    let t = tiers_20_then_5();
    let t = Some(t.as_slice());
    let z = FixedPoint::ZERO;
    // long 10 valued at 100 (IM 50): buy 5 @100 → 1,500 (IM 300): +250
    assert_eq!(placement_need(t, fp(10), fp(100), true, fp(5), fp(100), false), Some(fp(250)));
    // closing sell 5: IM 50 → 25
    assert_eq!(placement_need(t, fp(10), fp(100), false, fp(5), fp(100), false), Some(-fp(25)));
    // flip: sell 15 → short 5 (IM 25): −25
    assert_eq!(placement_need(t, fp(10), fp(100), false, fp(15), fp(100), false), Some(-fp(25)));
    // the same sell as a resting GTC: its opening 5 on top of the long: IM(1,500) − 50
    assert_eq!(placement_need(t, fp(10), fp(100), false, fp(15), fp(100), true), Some(fp(250)));
    // a resting closing sell needs nothing
    assert_eq!(placement_need(t, fp(10), fp(100), false, fp(10), fp(100), true), Some(z));
    // flat: plain order IM
    assert_eq!(placement_need(t, z, z, true, fp(5), fp(100), true), Some(fp(25)));
    assert_eq!(order_initial_margin(t, fp(500)), fp(25));
}
```

**Implementation** — `crates/torus-core/src/margin.rs`, new section after `order_initial_margin`
(:75); import `use crate::position::NativeBalance;` next to :10.

```rust
// ============================================================================
// Account-level margin (F1, s517) — Hyperliquid cross margin, computed
// ============================================================================

/// F1: the price a position is valued at — the market's mark, else its entry
/// price (s517 decision 2: UPnL 0 and IM at entry notional without a mark).
pub fn position_price(pos: &Position, mark: Option<FixedPoint>) -> FixedPoint {
    mark.unwrap_or(pos.entry_price)
}

/// F1: IM change when one market's (position [+ resting]) notional goes from
/// `before` to `after`, both at their own POSITION-size tier. < 0 = released.
pub fn im_delta(tiers: Option<&[MarginTier]>, before: FixedPoint, after: FixedPoint) -> FixedPoint {
    order_initial_margin(tiers, after) - order_initial_margin(tiers, before)
}

/// F1: margin need of placing `qty` on side `is_buy` against signed position
/// `signed` (valued at `px`), priced at `price`: the larger of the IM delta of
/// a complete fill (the closing part releases IM) and — for an order that can
/// rest — of resting its opening part. `<= 0`: it only reduces. `None` on
/// overflow. The book's match-time need is the same notional arithmetic.
pub fn placement_need(
    tiers: Option<&[MarginTier]>,
    signed: FixedPoint,
    px: FixedPoint,
    is_buy: bool,
    qty: FixedPoint,
    price: FixedPoint,
    can_rest: bool,
) -> Option<FixedPoint> {
    let size = if signed < FixedPoint::ZERO { -signed } else { signed };
    let closing = qty.min(crate::order_book::reduce_only_allowance(signed, is_buy));
    let opening_notional = price.checked_mul(qty - closing).ok()?;
    let before = size.checked_mul(px).ok()?;
    let left = (size - closing).checked_mul(px).ok()?;
    let filled = im_delta(tiers, before, left.checked_add(opening_notional).ok()?);
    if !can_rest {
        return Some(filled);
    }
    let rested = im_delta(tiers, before, before.checked_add(opening_notional).ok()?);
    Some(filled.max(rested))
}

/// F1: one trader's cross-margin account (never stored — decision 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountView {
    pub available: FixedPoint,
    pub order_margin: FixedPoint,
    /// Σ unrealized PnL at [`position_price`].
    pub upnl: FixedPoint,
    /// Σ position IM at [`position_price`], each at its market's position-size tier.
    pub position_im: FixedPoint,
    /// Σ |size| × [`position_price`].
    pub notional: FixedPoint,
}

impl AccountView {
    /// Cross positions only (every production position is Cross).
    pub fn build<'t>(
        bal: &NativeBalance,
        positions: &[Position],
        mark: impl Fn(MarketId) -> Option<FixedPoint>,
        tiers: impl Fn(MarketId) -> Option<&'t [MarginTier]>,
    ) -> Result<Self, CoreError> {
        let of = |_| CoreError::Overflow("account margin overflows i128".into());
        let mut v = Self {
            available: bal.available,
            order_margin: bal.order_margin,
            upnl: FixedPoint::ZERO,
            position_im: FixedPoint::ZERO,
            notional: FixedPoint::ZERO,
        };
        for pos in positions.iter().filter(|p| p.margin_type == MarginType::Cross) {
            let px = position_price(pos, mark(pos.market_id));
            let n = pos.size.checked_mul(px).map_err(of)?;
            let diff = if pos.is_long { px - pos.entry_price } else { pos.entry_price - px };
            v.upnl = v.upnl.checked_add(diff.checked_mul(pos.size).map_err(of)?).map_err(of)?;
            v.notional = v.notional.checked_add(n).map_err(of)?;
            v.position_im = v
                .position_im
                .checked_add(order_initial_margin(tiers(pos.market_id), n))
                .map_err(of)?;
        }
        Ok(v)
    }

    /// Collateral + UPnL.
    pub fn equity(&self) -> FixedPoint {
        self.available + self.order_margin + self.upnl
    }

    /// What the positions add to (or take from) free margin: UPnL − IM.
    pub fn pos_net(&self) -> FixedPoint {
        self.upnl - self.position_im
    }

    /// equity − (position IM + order margin) = available + [`Self::pos_net`].
    pub fn free(&self) -> FixedPoint {
        self.available + self.pos_net()
    }

    /// HL `transfer_margin_required`: max(Σ IM, 10% × Σ notional).
    pub fn transfer_required(&self) -> FixedPoint {
        self.position_im.max(FixedPoint::from_raw(self.notional.raw() / 10))
    }

    /// s517 decision 5, SAFE variant (user, s517): the cash bound
    /// `amount <= available` AND equity WITHOUT the resting orders'
    /// reservations (`available + upnl`) after the withdrawal covers
    /// [`Self::transfer_required`]. A negative `available` withdraws nothing.
    pub fn withdrawal_allowed(&self, amount: FixedPoint) -> bool {
        amount <= self.available
            && self.equity() - self.order_margin - amount >= self.transfer_required()
    }
}
```

(Check `FixedPoint::checked_add/checked_mul` return `Result` — they do, see `order_book.rs:181`;
`Ord::max` is used at NE:5227.)

**Validate:** `cargo test -p torus-core --test margin_tests account_` · depends_on: []

---

### T2 — core: align `MarginEngine` (fix the `order_margin` double count) + liquidation deficit (D1)

**Test first**
1. `margin.rs:444-463` — the existing unit test pins the bug. Replace it (justification: `available`
   is already net of `order_margin`, `NativeBalance` doc + every reserve site NE:3815-3816):
   ```rust
   // F1: equity counts collateral once — available is already net of order_margin.
   #[test]
   fn cross_margin_equity_counts_order_margin_once() {
       // ... same setup (available 10,000, order_margin 3,000, no oracle) ...
       assert_eq!(equity, fp(13_000));
   }
   ```
2. `crates/torus-core/tests/liquidation_tests.rs`, append:
   ```rust
   /// F1: collateral held as order margin is equity. Available 0 + 600 reserved,
   /// long 1 @50,000 at mark 50,000: maintenance 500 <= 600 → NOT liquidatable.
   /// The old equity (available − order_margin = −600) flagged it.
   #[test]
   fn reserved_order_margin_counts_as_equity_once() {
       let (_dir, pm) = setup();
       let trader = addr(1);
       pm.put_native_balance(&trader, &NativeBalance { available: FixedPoint::ZERO, order_margin: fp(600) }).unwrap();
       make_cross_long(&pm, &trader, 1, fp(1), fp(50_000));
       let config = MarketMarginConfig::new(1, 50);
       let liqs = LiquidationEngine::check_liquidations(&pm, &[trader], &config, &[(1, fp(50_000))]).unwrap();
       assert!(liqs.is_empty(), "{liqs:?}");
   }
   ```
3. `crates/torus-core/tests/margin_tests.rs`, append:
   ```rust
   /// F1 (s517 #2): without an oracle price, maintenance is taken at the entry
   /// price (was: the position was skipped → 0).
   #[test]
   fn maintenance_uses_entry_price_without_a_mark() {
       let (_dir, pm) = setup();
       let trader = addr(1);
       pm.put_position(&Position { trader, ..cross(1, true, 1, 50_000) }).unwrap();
       let config = MarketMarginConfig::new(1, 50);
       let m = MarginEngine::total_maintenance_margin(&pm, &trader, &config, &[]).unwrap();
       assert_eq!(m, fp(500)); // IM 50,000/50 = 1,000 × 50%
   }
   ```

**Implementation** — `margin.rs:201-251`:

```rust
    /// F1: [`AccountView::equity`] — collateral (available + order margin) + UPnL
    /// at the mark, entry price without one.
    pub fn cross_margin_equity(
        positions: &PositionManager<impl StateBackend>,
        trader: &Address,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let bal = positions.get_native_balance(trader)?;
        let all_pos = positions.positions_for_trader(trader)?;
        let view = AccountView::build(&bal, &all_pos, |m| oracle_price_for(oracle_prices, m), |_| None)?;
        Ok(view.equity())
    }

    /// F1: Σ maintenance of cross positions — IM ([`order_initial_margin`],
    /// position-size tier) at [`position_price`] × maintenance_factor_bps / 10,000.
    pub fn total_maintenance_margin(
        positions: &PositionManager<impl StateBackend>,
        trader: &Address,
        config: &MarketMarginConfig,
        oracle_prices: &[(MarketId, FixedPoint)],
    ) -> Result<FixedPoint, CoreError> {
        let maint_num = FixedPoint::from_raw(config.maintenance_factor_bps as i128 * FixedPoint::SCALE);
        let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
        let mut total = FixedPoint::ZERO;
        for pos in positions.positions_for_trader(trader)? {
            if pos.margin_type != MarginType::Cross {
                continue;
            }
            let px = position_price(&pos, oracle_price_for(oracle_prices, pos.market_id));
            let initial = order_initial_margin(Some(&config.tiers), pos.notional(px));
            total += initial * maint_num / bps_denom;
        }
        Ok(total)
    }
```

`check_initial_margin` (no production caller) is left as is; its Cross branch now sees the
corrected equity (noted in Risks R9).

**D1 consumer fix — liquidation deficit** (`liquidation.rs:233-240`, the one core consumer that
reads a negative `available` as a loss). Test first, `liquidation_tests.rs`:
```rust
/// F1/D1: with UPnL-funded reservations, `available` < 0 is not a loss —
/// collateral is `available + order_margin`. Cash −100, 300 reserved, long
/// 1 @50,000 liquidated at 50,000 (PnL 0, penalty 2.5% = 1,250): collateral
/// 200 − 1,250 → deficit 1,050 (was: 100 + 1,250 = 1,350, and the 300
/// reservation was later released on top — minting it back).
#[test]
fn liquidation_deficit_counts_order_margin_as_collateral() {
    let (_dir, pm) = setup();
    let trader = addr(1);
    pm.put_native_balance(&trader, &NativeBalance { available: -fp(100), order_margin: fp(300) }).unwrap();
    make_cross_long(&pm, &trader, 1, fp(1), fp(50_000));
    let pos = pm.get_position(&trader, 1).unwrap().unwrap();
    let liq = torus_core::liquidation::Liquidation { trader, market_id: 1, position: pos, shortfall: FixedPoint::ZERO, margin_type: MarginType::Cross };
    let r = LiquidationEngine::execute_liquidation(&pm, &liq, fp(50_000)).unwrap();
    assert_eq!(r.remaining_deficit, fp(1_050));
    let bal = pm.get_native_balance(&trader).unwrap();
    assert_eq!((bal.available, bal.order_margin), (-fp(300), fp(300)));
}
```
(Check the `Liquidation` / result field names at `liquidation.rs:20-60` when writing it.)
Implementation:
```rust
// F1/D1: collateral is available + order_margin (available alone may be
// negative while resting orders hold UPnL-funded reservations).
let collateral = bal.available + bal.order_margin;
let remaining_deficit = if collateral < FixedPoint::ZERO {
    bal.available = -bal.order_margin; // collateral 0; reservations release later
    -collateral
} else {
    FixedPoint::ZERO
};
```

**Validate:** `cargo test -p torus-core --lib margin::tests && cargo test -p torus-core --test margin_tests && cargo test -p torus-core --test liquidation_tests` · depends_on: [1]

---

### T3 — bridge: `AccountReader` + withdrawal rule (3 entry points)

**Test first**
1. New file `crates/torus-bridge/tests/account_margin_tests.rs`. Helpers copied verbatim from
   `market_order_margin_tests.rs:31-210` (`open_test_db, addr, fp, fp_cents, make_ctx, fund_native,
   limit, market, place, bal, assert_bal, FUNDING, filler, Path, PATHS, run, fresh`) and `set_mark`
   (:467-480), plus:
   ```rust
   use alloy_primitives::U256;
   use torus_core::margin::{MarginTier, MarketMarginConfig};

   fn pos_in(ctx: &NativeExecContext, t: &Address, m: MarketId) -> FixedPoint {
       match ctx.positions.get_position(t, m).unwrap() {
           Some(p) if p.is_long => p.size,
           Some(p) => -p.size,
           None => FixedPoint::ZERO,
       }
   }
   fn resting_in(ctx: &NativeExecContext, t: &Address, m: MarketId) -> Vec<FixedPoint> {
       ctx.order_books.get(&m).map(|b| b.orders_for_trader(t).iter().map(|o| o.remaining_qty).collect()).unwrap_or_default()
   }
   fn abs(x: FixedPoint) -> FixedPoint { if x < FixedPoint::ZERO { -x } else { x } }
   fn raw(x: FixedPoint) -> U256 { U256::from(x.raw() as u128) }
   fn to_spot(t: Address, a: FixedPoint) -> (Address, NativeAction) {
       (t, NativeAction::TransferToSpot { amount: raw(a) })
   }
   fn withdraw_to(t: Address, a: FixedPoint) -> (Address, NativeAction) {
       (t, NativeAction::Withdraw { amount: raw(a), to: addr(77) })
   }
   fn tiered(ctx: &mut NativeExecContext, m: MarketId, tiers: Vec<MarginTier>) {
       let mut c = MarketMarginConfig::new(m, 999);
       c.tiers = tiers;
       ctx.margin_configs.insert(m, c);
   }
   /// `t` long 10 @100 in market 1 (20x: IM 50, notional 1,000) with `avail`
   /// available and no order margin; counterparty addr(3).
   fn long_10(ctx: &mut NativeExecContext, path: Path, t: Address, avail: FixedPoint) {
       fund_native(ctx, &addr(3), fp(FUNDING));
       let r = run(ctx, path, &[place(addr(3), limit(1, false, 100, 10))]);
       assert!(r[0].success, "{path:?}: {:?}", r[0].error);
       fund_native(ctx, &t, avail);
       let r = run(ctx, path, &[place(t, limit(1, true, 100, 10))]);
       assert!(r[0].success, "{path:?}: {:?}", r[0].error);
       assert_eq!(pos_in(ctx, &t, 1), fp(10), "{path:?}");
       assert_bal(ctx, &t, avail, FixedPoint::ZERO, "after open");
   }
   /// One `#[test]` per PlaceOrder path: `<case>::single|batch|parallel`.
   macro_rules! per_path {
       ($case:ident) => {
           mod $case {
               use super::*;
               #[test] fn single() { super::$case(Path::Single) }
               #[test] fn batch() { super::$case(Path::Batch) }
               #[test] fn parallel() { super::$case(Path::Parallel) }
           }
       };
   }
   ```
   Withdrawal tests:
   ```rust
   /// Long 10 @100 on 150: equity 150, required max(IM 50, 10% × 1,000) = 100.
   /// 50 leaves exactly 100 (ok); 51 is rejected (was: allowed, only
   /// `available >= amount` was checked). TransferToSpot and Withdraw{to}.
   fn withdraw_keeps_the_transfer_margin(path: Path) {
       for (amount, ok) in [(50, true), (51, false)] {
           for to_evm in [false, true] {
               let t = addr(2);
               let (_d, mut ctx) = fresh(path, &[]);
               long_10(&mut ctx, path, t, fp(150));
               let a = if to_evm { withdraw_to(t, fp(amount)) } else { to_spot(t, fp(amount)) };
               let r = run(&mut ctx, path, &[a]);
               let what = format!("{path:?} to_evm={to_evm} amount={amount}");
               assert_eq!(r[0].success, ok, "{what}: {:?}", r[0].error);
               if !ok {
                   assert!(r[0].error.as_deref().unwrap_or("").contains("under-margined"), "{what}");
               }
               assert_bal(&ctx, &t, fp(if ok { 150 - amount } else { 150 }), FixedPoint::ZERO, &what);
           }
       }
   }
   per_path!(withdraw_keeps_the_transfer_margin);

   /// At 5x the position IM (200) dominates the 10% floor (100): 250 → 50 ok, 51 not.
   fn withdraw_position_im_dominates_at_low_leverage(path: Path) {
       for (amount, ok) in [(50, true), (51, false)] {
           let t = addr(2);
           let (_d, mut ctx) = fresh(path, &[]);
           tiered(&mut ctx, 1, vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 }]);
           long_10(&mut ctx, path, t, fp(250));
           let r = run(&mut ctx, path, &[to_spot(t, fp(amount))]);
           assert_eq!(r[0].success, ok, "{path:?} {amount}: {:?}", r[0].error);
       }
   }
   per_path!(withdraw_position_im_dominates_at_low_leverage);

   /// UPnL counts toward equity but never makes more than `available`
   /// withdrawable. Mark 120: equity 150 + 200, required max(60, 120).
   fn withdraw_counts_upnl_but_not_beyond_available(path: Path) {
       for (amount, ok) in [(150, true), (151, false)] {
           let t = addr(2);
           let (_d, mut ctx) = fresh(path, &[]);
           long_10(&mut ctx, path, t, fp(150));
           set_mark(&ctx, 1, fp(120));
           let r = run(&mut ctx, path, &[to_spot(t, fp(amount))]);
           assert_eq!(r[0].success, ok, "{path:?} {amount}: {:?}", r[0].error);
       }
   }
   per_path!(withdraw_counts_upnl_but_not_beyond_available);

   /// Regression: flat accounts withdraw everything.
   fn withdraw_flat_account_everything(path: Path) {
       let t = addr(2);
       let (_d, mut ctx) = fresh(path, &[t]);
       let r = run(&mut ctx, path, &[to_spot(t, fp(FUNDING))]);
       assert!(r[0].success, "{path:?}: {:?}", r[0].error);
       assert_bal(&ctx, &t, FixedPoint::ZERO, FixedPoint::ZERO, "flat");
   }
   per_path!(withdraw_flat_account_everything);

   /// s517 D3 (SAFE variant): 5x everywhere. Long 5 @100 in m1 (IM 100,
   /// 10% floor 50), a resting bid 5 @100 in m2 (reservation 100), 100
   /// available: withdrawing 100 would leave only the reservation behind the
   /// position → REJECTED (the unsafe variant, equity − amount = 100 >= 100,
   /// allowed it; if the bid then filled, IM 200 would sit on 100).
   fn withdraw_cannot_use_resting_reservations_as_collateral(path: Path) {
       let t = addr(2);
       let (_d, mut ctx) = fresh(path, &[addr(3)]);
       let five_x = vec![MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 }];
       tiered(&mut ctx, 1, five_x.clone());
       tiered(&mut ctx, 2, five_x);
       fund_native(&ctx, &t, fp(200));
       run(&mut ctx, path, &[place(addr(3), limit(1, false, 100, 5))]);
       assert!(run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))])[0].success, "{path:?}");
       assert!(run(&mut ctx, path, &[place(t, limit(2, true, 100, 5))])[0].success, "{path:?}");
       assert_bal(&ctx, &t, fp(100), fp(100), "position + resting bid");
       for amount in [100, 1] {
           let r = run(&mut ctx, path, &[to_spot(t, fp(amount))]);
           assert!(!r[0].success, "{path:?} amount={amount}: must be rejected");
           assert!(r[0].error.as_deref().unwrap_or("").contains("under-margined"), "{path:?}");
       }
       assert_bal(&ctx, &t, fp(100), fp(100), "unchanged");
   }
   per_path!(withdraw_cannot_use_resting_reservations_as_collateral);
   ```
2. `crates/torus-bridge/tests/lockbox_queue_tests.rs` (import `MarginType, Position` from
   `torus_core::position`):
   ```rust
   /// F1 (s517 #5): a queued `withdrawFromNative` (drains as TransferToSpot)
   /// obeys the transfer margin: long 10 @100 (10% floor 100) on 150 native —
   /// 51 fails at the drain, 50 passes.
   #[test]
   fn queued_withdraw_respects_account_transfer_margin() {
       for (amount, ok) in [(51u64, false), (50, true)] {
           let (_dir, db) = open_test_db();
           fund_evm(&db, &ALICE, wei(10));
           seed_native(&db, &ALICE, fp(150));
           PositionManager::new(db.clone())
               .put_position(&Position {
                   trader: ALICE,
                   market_id: 1,
                   is_long: true,
                   size: fp(10),
                   entry_price: fp(100),
                   realized_pnl: FixedPoint::ZERO,
                   isolated_margin: FixedPoint::ZERO,
                   margin_type: MarginType::Cross,
               })
               .unwrap();
           assert_eq!(evm_block(&db, 1, vec![withdraw(ALICE, wei(amount), 0)]), vec![true]);
           let results = drain(&db, 2);
           assert_eq!(results.len(), 1);
           assert_eq!(results[0].0, ok, "amount {amount}: {:?}", results[0].1);
           assert_eq!(native(&db, &ALICE), fp(if ok { 150 - amount as i64 } else { 150 }));
       }
   }
   ```

**Implementation** — `native_executor.rs`
1. Imports (:16-19): add `placement_need, AccountView` to the `torus_core::margin` use;
   `torus_core::order_book::{AccountMargins, MakerAccount, MakerAccountSource}` come in T4/T8.
2. After `PrepOutcome` (NE:427) add the ONE reader every F1 path uses:
   ```rust
   /// F1 (s517): read-only inputs of the account-level margin formulas —
   /// shared by placement (single + Phase 2), the Phase-3 maker check, modify
   /// and withdrawals, so every path computes the same numbers. Only READS
   /// state; in Phase 3 the backend is frozen (write-back caches, NE:4019).
   struct AccountReader<'a, T: StateBackend> {
       positions: &'a PositionManager<T>,
       oracle: &'a OracleManager<T>,
       height: u64,
       margin_configs: &'a HashMap<MarketId, MarketMarginConfig>,
   }

   impl<'a, T: StateBackend> AccountReader<'a, T> {
       fn of(ctx: &'a NativeExecContext<T>) -> Self {
           Self {
               positions: &ctx.positions,
               oracle: &ctx.oracle,
               height: ctx.block_height,
               margin_configs: &ctx.margin_configs,
           }
       }

       /// s515 review 4 mark: the aggregated oracle price, `None` when absent,
       /// stale, non-positive or unreadable (moved from `mark_price`).
       fn mark(&self, market_id: MarketId) -> Option<FixedPoint> {
           match self.oracle.get_price(market_id, self.height) {
               Ok(p) if !p.stale && p.price > FixedPoint::ZERO => Some(p.price),
               _ => None,
           }
       }

       fn tiers(&self, market_id: MarketId) -> Option<&'a [MarginTier]> {
           self.margin_configs.get(&market_id).map(|c| c.tiers.as_slice())
       }

       fn view(&self, trader: &Address, bal: &NativeBalance) -> Result<AccountView, CoreError> {
           let ps = self.positions.positions_for_trader(trader)?;
           AccountView::build(bal, &ps, |m| self.mark(m), |m| self.tiers(m))
       }

       /// UPnL − position IM of `trader` (balance-independent part of `free`).
       fn pos_net(&self, trader: &Address) -> Result<FixedPoint, CoreError> {
           Ok(self.view(trader, &NativeBalance::default())?.pos_net())
       }

       /// Signed position in `market_id` and the price it is valued at
       /// (mark, else entry; ZERO when flat without a mark).
       fn position_px(&self, trader: &Address, market_id: MarketId) -> Result<(FixedPoint, FixedPoint), CoreError> {
           let mark = self.mark(market_id);
           Ok(match self.positions.get_position(trader, market_id)? {
               Some(p) => (if p.is_long { p.size } else { -p.size }, position_price(&p, mark)),
               None => (FixedPoint::ZERO, mark.unwrap_or(FixedPoint::ZERO)),
           })
       }
   }
   ```
   `mark_price` (NE:4969-4974) becomes `AccountReader::of(ctx).mark(market_id)` (one formula).
3. Next to `exec_withdraw_to` (after NE:6370):
   ```rust
   /// F1 (s517 decision 5, Hyperliquid `transfer_margin_required`): a native
   /// withdrawal — TransferToSpot, Withdraw, and CoreWriter LockboxWithdraw
   /// (drains as TransferToSpot, NE:6852) — must leave equity minus order
   /// margin >= max(Σ position IM, 10% × Σ position notional) (SAFE variant).
   /// `amount > available` (incl. any amount while `available < 0`) is left
   /// to the Lockbox's own check `lockbox.rs:158` (error text unchanged).
   fn check_withdrawal_margin<T: StateBackend>(
       ctx: &NativeExecContext<T>,
       sender: &Address,
       amount: FixedPoint,
   ) -> Result<(), String> {
       let bal = ctx.positions.get_native_balance(sender).map_err(|e| e.to_string())?;
       if amount <= FixedPoint::ZERO || amount > bal.available {
           return Ok(());
       }
       let view = AccountReader::of(ctx).view(sender, &bal).map_err(|e| e.to_string())?;
       if view.withdrawal_allowed(amount) {
           return Ok(());
       }
       Err(format!(
           "withdrawal of {amount} would leave the account under-margined: equity after (excl. order margin) {}, required {}",
           view.equity() - view.order_margin - amount,
           view.transfer_required()
       ))
   }
   ```
   In `exec_withdraw_from_native` (after the `fp_amount` match, NE:6348) and `exec_withdraw_to`
   (after NE:6365):
   ```rust
   if let Err(msg) = Self::check_withdrawal_margin(ctx, sender, fp_amount) {
       return NativeActionResult::err("withdraw_from_native", msg); // "withdraw_to" in the other
   }
   ```

**Validate:** `cargo test -p torus-bridge --test account_margin_tests withdraw && cargo test -p torus-bridge --test lockbox_queue_tests` · depends_on: [1]

---

### T4 — core book: taker need at the position tier + running account free

**Test first** — `order_book.rs`, inside `mod taker_margin_limit_tests` (after :5480). The 13
existing tests in the module must pass unchanged (no `AccountMargins` installed ⇒ old semantics).

```rust
    fn fp_c(v: i64) -> FixedPoint {
        FixedPoint::from_raw(v as i128 * (FixedPoint::SCALE / 100))
    }

    fn tiers_20_then_5() -> std::sync::Arc<[crate::margin::MarginTier]> {
        std::sync::Arc::from(vec![
            crate::margin::MarginTier { max_notional: fp(1_000), max_leverage: 20 },
            crate::margin::MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
        ])
    }

    fn with_account(b: &mut OrderBook, t: Address, free: FixedPoint, px: FixedPoint,
                    tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>) {
        let mut am = AccountMargins::new(tiers);
        am.insert(t, free, px);
        b.set_account_margins(am);
    }

    fn market_buy(qty: FixedPoint) -> PlaceOrderParams {
        PlaceOrderParams { order_type: OrderType::Market, time_in_force: TimeInForce::IOC, ..order(true, fp(1_000), qty) }
    }

    /// F1: an increase is charged at the POSITION's tier. Long 10 valued at
    /// 100 (IM 50 at 20x), asks 100 x 5, market buy 5 with reservation 25:
    /// unit k makes the position 1,000 + 100k at 5x → +170 … +250. Free 225
    /// (budget 250) fits all 5; free 224 fits 4 (+230). Per-order margin
    /// (IM(100k) <= 25) filled 5 in both.
    #[test]
    fn f1_increase_is_charged_at_the_position_tier() {
        for (free, fills) in [(fp(225), 5), (fp(224), 4)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            b.place_order(order(false, fp(100), fp(5)), addr(1), 1);
            let mut pos = ReduceOnlyPositions::new();
            pos.insert(addr(2), fp(10));
            b.set_reduce_only_positions(pos);
            with_account(&mut b, addr(2), free, fp(100), Some(tiers_20_then_5()));
            let lim = TakerMarginLimit { budget: fp(25), tiers: Some(tiers_20_then_5()), hold_price: None };
            let r = b.place_order_with_margin(market_buy(fp(5)), addr(2), 2, Some(&lim));
            assert_eq!(filled(&r), fp(fills), "free {free}");
        }
    }

    /// F1: closing releases the position's IM for the flip. Long 20 valued at
    /// 100 (IM 100); budget = 0.5 reservation + free −50.5 = −50: sell 30 —
    /// 20 close, 10 open short (IM 50): 50 − 100 = −50 fits → 30. Free
    /// −51.5: 29 (45 − 100). Per-order margin (budget 0.5) filled only 20.
    #[test]
    fn f1_closing_releases_position_im_for_the_flip() {
        for (free, fills) in [(fp_c(-5050), 30), (fp_c(-5150), 29)] {
            let mut b = book_with_taker_pos(fp(20));
            with_account(&mut b, addr(2), free, fp(100), None);
            let r = b.place_order_with_margin(market_sell(fp(30)), addr(2), 2, Some(&limit(fp_c(50), None)));
            assert_eq!(filled(&r), fp(fills), "free {free}");
        }
    }

    /// F1: one sender's takers in one book share a RUNNING free margin (the
    /// batch's exclusive pool). Free 98: sell A (reservation 1) fills 19
    /// (IM 95 <= 99) and leaves 4; sell B (reservation 1) is short 19 at the
    /// book's last price 100 and fills 1 more (5 <= 1 + 4). Free ends at 0.
    #[test]
    fn f1_running_free_is_shared_by_one_senders_takers() {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(40)), addr(1), 1);
        let mut pos = ReduceOnlyPositions::new();
        pos.insert(addr(2), FixedPoint::ZERO);
        b.set_reduce_only_positions(pos);
        with_account(&mut b, addr(2), fp(98), FixedPoint::ZERO, None);
        let r1 = b.place_order_with_margin(market_sell(fp(20)), addr(2), 2, Some(&limit(fp(1), None)));
        let r2 = b.place_order_with_margin(market_sell(fp(20)), addr(2), 3, Some(&limit(fp(1), None)));
        assert_eq!((filled(&r1), filled(&r2)), (fp(19), fp(1)));
        assert_eq!(b.account_margins().get(&addr(2)).unwrap().free, FixedPoint::ZERO);
    }

    /// F1: a purely closing fill always fits, even with the account under
    /// water (HL: reducing needs no margin).
    #[test]
    fn f1_closing_fills_fit_an_under_margined_account() {
        let mut b = book_with_taker_pos(fp(20));
        with_account(&mut b, addr(2), -fp(500), fp(100), None);
        let r = b.place_order_with_margin(market_sell(fp(20)), addr(2), 2, Some(&limit(FixedPoint::ZERO, None)));
        assert_eq!(filled(&r), fp(20));
    }

    /// F1 FOK: the complete fill is judged by the same need (closing releases
    /// IM): free −50.5 → the flip fits (−50 <= −50); free −51.5 → rejected
    /// whole even though the fill lowers the need (it opens 10 short).
    #[test]
    fn f1_fok_uses_the_account_need() {
        let mut p = market_sell(fp(30));
        p.order_type = OrderType::Limit;
        p.price = fp(100);
        p.time_in_force = TimeInForce::FOK;
        for (free, fills) in [(fp_c(-5050), 30), (fp_c(-5150), 0)] {
            let mut b = book_with_taker_pos(fp(20));
            with_account(&mut b, addr(2), free, fp(100), None);
            let r = b.place_order_with_margin(p.clone(), addr(2), 2, Some(&limit(fp_c(50), None)));
            assert_eq!(filled(&r), fp(fills), "free {free}");
        }
    }
```

**Implementation** — `crates/torus-core/src/order_book.rs`
1. New public type after `ReduceOnlyPositions` (:271):
   ```rust
   /// F1 (s517): account-level margin state of ONE book for the current
   /// placement / batch — the running free margin (and position valuation
   /// price) of each trader whose fills the book must check. Installed by the
   /// executor, advanced by the book, cleared with the reduce-only map. A
   /// trader ABSENT from it is checked as before F1 (book-only callers).
   #[derive(Clone, Debug, Default)]
   pub struct AccountMargins {
       tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>,
       accounts: BTreeMap<Address, AccountMargin>,
   }

   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub struct AccountMargin {
       /// Running free margin (may be negative).
       pub free: FixedPoint,
       /// Price the trader's position here is valued at (mark, else entry;
       /// ZERO = use the book's last trade price).
       pub px: FixedPoint,
   }

   impl AccountMargins {
       pub fn new(tiers: Option<std::sync::Arc<[crate::margin::MarginTier]>>) -> Self {
           Self { tiers, accounts: BTreeMap::new() }
       }
       pub fn insert(&mut self, trader: Address, free: FixedPoint, px: FixedPoint) {
           self.accounts.insert(trader, AccountMargin { free, px });
       }
       pub fn get(&self, trader: &Address) -> Option<AccountMargin> {
           self.accounts.get(trader).copied()
       }
       fn set_free(&mut self, trader: &Address, free: FixedPoint) {
           if let Some(a) = self.accounts.get_mut(trader) {
               a.free = free;
           }
       }
   }
   ```
2. `OrderBook` field after `reduce_only_positions` (:480): `account_margins: AccountMargins,`
   (doc: in-RAM only, never serialized); initialize `AccountMargins::default()` in `new()` (:565)
   and the deserializer (:3087). Methods next to :959-965:
   ```rust
   pub fn set_account_margins(&mut self, a: AccountMargins) { self.account_margins = a; }
   pub fn clear_account_margins(&mut self) { self.account_margins = AccountMargins::default(); }
   pub fn account_margins(&self) -> &AccountMargins { &self.account_margins }
   ```
3. Replace `MatchMargin` (:222-228) and `TakerMarginLimit::{need, affordable}` (:164-218); keep
   `im` (used nowhere else after this — delete it if unused). Update the `TakerMarginLimit` doc:
   `budget` is now "the order's own reservation; the book adds the sender's running free margin
   from [`AccountMargins`] (absent = 0, i.e. the pre-F1 budget)".
   ```rust
   /// Running state of a [`TakerMarginLimit`] during one taker's matching.
   struct MatchMargin<'a> {
       limit: &'a TakerMarginLimit,
       lot: FixedPoint,
       /// F1: |position| at the taker's start, the price it is valued at, and
       /// its closing capacity then (opposite-side size; 0 if same side).
       size0: FixedPoint,
       px: FixedPoint,
       allowance0: FixedPoint,
       /// F1: `limit.budget` + the sender's running free margin.
       budget: FixedPoint,
       /// Notional of the charged (non-closing) part of the fills so far.
       charged: FixedPoint,
       exhausted: bool,
   }

   impl TakerMarginLimit {
       /// F1: IM delta of this taker's market (position-size tier) after
       /// filling `q` at `price` on top of the fills so far: the position
       /// valued at `px` shrinks by what the fills closed, the opening
       /// fills add their notional, and — for an order that can rest — the
       /// part of the remainder that would OPEN adds `hold_price` × it
       /// (resting closing quantity needs no margin, HL). `free` is the
       /// live closing capacity (the book's position map). `None` on overflow.
       fn need(&self, m: &MatchMargin<'_>, price: FixedPoint, q: FixedPoint, free: FixedPoint, left_before: FixedPoint) -> Option<FixedPoint> {
           let closing = q.min(free);
           let closed = m.allowance0 - free + closing;
           let before = m.size0.checked_mul(m.px).ok()?;
           let mut after = (m.size0 - closed)
               .checked_mul(m.px).ok()?
               .checked_add(m.charged).ok()?
               .checked_add(price.checked_mul(q - closing).ok()?).ok()?;
           if let Some(hp) = self.hold_price {
               let open_rest = ((left_before - q) - (free - closing)).max(FixedPoint::ZERO);
               after = after.checked_add(hp.checked_mul(open_rest).ok()?).ok()?;
           }
           Some(crate::margin::im_delta(self.tiers.as_deref(), before, after))
       }

       /// Largest quantity `<= q` (all of `q`, or a multiple of the lot) that
       /// fits: a purely closing quantity (`<= free`) always does (HL:
       /// reducing needs no margin, even under water); beyond it the need
       /// must be `<= budget`. Beyond the closing part the need is
       /// non-decreasing (checked takers fill at prices >= their hold), so the
       /// quantities that fit are one interval from zero; `lo` = 0 always fits.
       fn affordable(&self, m: &MatchMargin<'_>, price: FixedPoint, q: FixedPoint, free: FixedPoint, left_before: FixedPoint) -> FixedPoint {
           let fits = |q: FixedPoint| {
               q <= free
                   || self.need(m, price, q, free, left_before).is_some_and(|n| n <= m.budget)
           };
           if fits(q) {
               return q;
           }
           let step = if m.lot > FixedPoint::ZERO { m.lot.raw() } else { 1 };
           let (mut lo, mut hi) = (0i128, q.raw() / step + 1);
           while hi - lo > 1 {
               let mid = lo + (hi - lo) / 2;
               let cand = FixedPoint::from_raw(mid * step);
               if cand < q && fits(cand) { lo = mid; } else { hi = mid; }
           }
           FixedPoint::from_raw(lo * step)
       }
   }
   ```
4. `place_order_with_margin` (:762-786): build the `MatchMargin` BEFORE the FOK pre-check:
   ```rust
   // F1: the taker's account (absent ⇒ pre-F1 check: no position valuation,
   // budget = the limit's own). Position from the policed / tracked map.
   let mut match_margin = margin.map(|limit| {
       let signed = self.reduce_only_positions.get(&trader).unwrap_or(FixedPoint::ZERO);
       let allowance0 = reduce_only_allowance(signed, params.is_buy);
       let acct = self.account_margins.get(&trader);
       let px = match acct {
           Some(a) if a.px > FixedPoint::ZERO => a.px,
           Some(_) => self.last_trade_price.unwrap_or(FixedPoint::ZERO),
           None => FixedPoint::ZERO,
       };
       MatchMargin {
           limit,
           lot: self.lot_size,
           size0: if signed < FixedPoint::ZERO { -signed } else { signed },
           px,
           allowance0,
           budget: limit.budget + acct.map_or(FixedPoint::ZERO, |a| a.free),
           charged: FixedPoint::ZERO,
           exhausted: false,
       }
   });
   ```
   FOK branch: replace `margin.is_none_or(|m| notional.is_some_and(|n| m.im(n) <= m.budget))` with
   ```rust
   Some(notional) => match_margin.as_ref().is_none_or(|m| {
       // A purely closing FOK always fits; otherwise the complete fill's
       // need (closing releases IM) must fit the budget.
       quantity <= free
           || notional.is_some_and(|n| {
               let closing = quantity.min(free);
               let before = m.size0.checked_mul(m.px).ok();
               let after = (m.size0 - closing).checked_mul(m.px).ok().and_then(|x| x.checked_add(n).ok());
               matches!((before, after), (Some(b), Some(a))
                   if crate::margin::im_delta(m.limit.tiers.as_deref(), b, a) <= m.budget)
           })
   }),
   ```
   After `execute_match` and the status block (:830-865), write back the running free when the
   taker has an account:
   ```rust
   // F1: what this taker committed comes off the sender's running free margin.
   if let Some(m) = match_margin.as_ref() {
       if let Some(a) = self.account_margins.get(&trader) {
           let free_now = self.reduce_only_positions.get(&trader)
               .map_or(FixedPoint::ZERO, |p| reduce_only_allowance(p, params.is_buy));
           let left = if rested_qty > FixedPoint::ZERO { rested_qty } else { FixedPoint::ZERO };
           // Every fill's need was computed checked; on overflow count the
           // whole budget as spent.
           let need = m.limit.need(m, FixedPoint::ZERO, FixedPoint::ZERO, free_now, left).unwrap_or(m.budget);
           self.account_margins.set_free(&trader, a.free + m.limit.budget - need);
       }
   }
   ```
   (`budget_eff − need` = `a.free + limit.budget − need`.) Replace the existing
   `let margin_exhausted = match_margin.is_some_and(|m| m.exhausted);` with `.as_ref().is_some_and`.
5. `match_at_level` taker block (:1554-1576):
   ```rust
   if let Some(m) = margin.as_deref_mut() {
       let free = ro_positions.get(&taker.trader).map_or(FixedPoint::ZERO, |p| {
           reduce_only_allowance(p, taker.side == Side::Buy)
       });
       let lim = m.limit;
       let fits = lim.affordable(m, price, fill_qty, free, taker.remaining_qty);
       if fits < fill_qty {
           m.exhausted = true;
           fill_qty = fits;
       }
       if fill_qty <= FixedPoint::ZERO {
           break;
       }
       // Cannot overflow: `affordable` returned a purely closing quantity
       // (charges nothing) or one whose notional it computed checked.
       m.charged += price * (fill_qty - fill_qty.min(free));
   }
   ```

**Validate:** `cargo test -p torus-core --lib taker_margin_limit_tests` · depends_on: [1]

Correction s517 (T4): the module had 10 existing tests, not 13; all 10 pass unchanged. The
`AccountMargins.tiers` field is only read from T8 on — `#[allow(dead_code)]` until then.

---

### T5 — bridge single path: account check as the ONLY placement gate (D1) + running free at match

**Test first** — `account_margin_tests.rs` (cases; `per_path!` each; T5 validates `single`):

```rust
/// F1 repro (design doc): 100 at 20x, market sell 20 @100 (IM 100) twice in
/// successive blocks. Per-order margin gave the second the same 100 again
/// (~40x). Account-level: the second is rejected; still short 20.
fn forty_x_sequential(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    fund_native(&ctx, &t, fp(100));
    run(&mut ctx, path, &[place(maker, limit(1, true, 100, 40))]);
    let sell = market(1, false, fp(1), 20);
    let r = run(&mut ctx, path, &[place(t, sell.clone())]);
    assert!(r[0].success, "{path:?}: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &t, 1), -fp(20), "{path:?}: first opens 20x");
    let r = run(&mut ctx, path, &[place(t, sell)]);
    assert!(!r[0].success, "{path:?}: second must be rejected");
    assert!(r[0].error.as_deref().unwrap_or("").starts_with("insufficient margin"), "{path:?}: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &t, 1), -fp(20), "{path:?}");
    assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, "no reservation left");
}
per_path!(forty_x_sequential);

/// The same two sells in ONE run (single path: two blocks; batch: one batch —
/// sell A takes 19 on the exclusive pool, sell B the last 1). Never past 20.
fn forty_x_one_batch(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    fund_native(&ctx, &t, fp(100));
    run(&mut ctx, path, &[place(maker, limit(1, true, 100, 40))]);
    let sell = market(1, false, fp(1), 20);
    run(&mut ctx, path, &[place(t, sell.clone()), place(t, sell)]);
    assert_eq!(pos_in(&ctx, &t, 1), -fp(20), "{path:?}: at most 20x");
    assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, "released");
}
per_path!(forty_x_one_batch);

/// Two checked market sells of one sender in two markets in one run cannot
/// both spend the same 100 (per-order margin opened 39-40 on 100).
fn cross_market_cannot_double_spend(path: Path) {
    let (m1, m2, t) = (addr(1), addr(3), addr(2));
    let (_d, mut ctx) = fresh(path, &[m1, m2]);
    fund_native(&ctx, &t, fp(100));
    run(&mut ctx, path, &[place(m1, limit(1, true, 100, 20)), place(m2, limit(2, true, 100, 20))]);
    run(&mut ctx, path, &[place(t, market(1, false, fp(1), 20)), place(t, market(2, false, fp(1), 20))]);
    let open = abs(pos_in(&ctx, &t, 1)) + abs(pos_in(&ctx, &t, 2));
    assert!(open <= fp(20), "{path:?}: opened {open} on 100 at 20x");
    assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, "released");
}
per_path!(cross_market_cannot_double_spend);

/// Tiers <= 1,000 at 20x, above 5x; 200 funded; GTC buys of 5 @100 (IM 25
/// each at its own tier). The 3rd makes the POSITION 1,500 (IM 300): +250 >
/// free 150 → rejected. One block per order.
fn position_tier_per_block(path: Path) {
    let (maker, t) = (addr(1), addr(2));
    let (_d, mut ctx) = fresh(path, &[maker]);
    tiered(&mut ctx, 1, vec![
        MarginTier { max_notional: fp(1_000), max_leverage: 20 },
        MarginTier { max_notional: FixedPoint::MAX, max_leverage: 5 },
    ]);
    fund_native(&ctx, &t, fp(200));
    run(&mut ctx, path, &[place(maker, limit(1, false, 100, 20))]);
    let mut ok = Vec::new();
    for _ in 0..3 {
        ok.push(run(&mut ctx, path, &[place(t, limit(1, true, 100, 5))])[0].success);
    }
    assert_eq!(ok, vec![true, true, false], "{path:?}");
    assert_eq!(pos_in(&ctx, &t, 1), fp(10), "{path:?}");
}
per_path!(position_tier_per_block);

/// Same three buys in ONE run (batch: the Phase-2 projection charges the 3rd
/// at the projected position's tier).
fn position_tier_one_run(path: Path) {
    // ... same setup ...
    let buy = limit(1, true, 100, 5);
    let r = run(&mut ctx, path, &[place(t, buy.clone()), place(t, buy.clone()), place(t, buy)]);
    let ok: Vec<bool> = r.iter().map(|x| x.success).collect();
    assert_eq!(ok, vec![true, true, false], "{path:?}: {r:?}");
    assert_eq!(pos_in(&ctx, &t, 1), fp(10), "{path:?}");
}
per_path!(position_tier_one_run);

/// Funded 100, long 10 @100 in m1 (IM 50). GTC buy `qty` @100 in m2.
/// mark 110: UPnL +100, IM 55 → free 145 ≥ 100 (qty 20) ✓;
/// mark 100: free 50 < 100 ✗, qty 10 (50) ✓; mark 95: free 2.5 < 5 (qty 1) ✗.
fn upnl_counts(path: Path) {
    for (mark, qty, ok) in [(110, 20, true), (100, 20, false), (100, 10, true), (95, 1, false)] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[addr(4)]);
        long_10(&mut ctx, path, t, fp(100));
        set_mark(&ctx, 1, fp(mark));
        run(&mut ctx, path, &[place(addr(4), limit(2, false, 100, qty))]);
        let r = run(&mut ctx, path, &[place(t, limit(2, true, 100, qty))]);
        assert_eq!(r[0].success, ok, "{path:?} mark={mark} qty={qty}: {:?}", r[0].error);
    }
}
per_path!(upnl_counts);

/// No oracle: the position is valued at ENTRY (UPnL 0), not at the last
/// trade: long 10 @100 on 100 → free 50 even after others trade at 120.
fn no_mark_values_at_entry(path: Path) {
    for (qty, ok) in [(10, true), (11, false)] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[addr(4), addr(5), addr(6)]);
        long_10(&mut ctx, path, t, fp(100));
        run(&mut ctx, path, &[place(addr(5), limit(1, false, 120, 1))]);
        run(&mut ctx, path, &[place(addr(6), limit(1, true, 120, 1))]);
        run(&mut ctx, path, &[place(addr(4), limit(2, false, 100, qty))]);
        let r = run(&mut ctx, path, &[place(t, limit(2, true, 100, qty))]);
        assert_eq!(r[0].success, ok, "{path:?} qty={qty}: {:?}", r[0].error);
    }
}
per_path!(no_mark_values_at_entry);

/// s517 D1 (STRICT HL): unrealized profit funds a reservation beyond cash.
/// Funded 100, long 10 @100 in m1; mark 150: UPnL +500, IM 75 → free 525.
/// A resting GTC bid 60 @100 in m2 reserves 300 > available 100: ACCEPTED,
/// available goes to −200 (the old `available >= reservation` gate rejected it).
/// Without a mark (entry fallback, UPnL 0): free 50 < 300 → rejected.
fn upnl_funds_a_reservation_beyond_cash(path: Path) {
    for with_mark in [true, false] {
        let t = addr(2);
        let (_d, mut ctx) = fresh(path, &[]);
        long_10(&mut ctx, path, t, fp(100));
        if with_mark {
            set_mark(&ctx, 1, fp(150));
        }
        let r = run(&mut ctx, path, &[place(t, limit(2, true, 100, 60))]);
        let what = format!("{path:?} mark={with_mark}");
        assert_eq!(r[0].success, with_mark, "{what}: {:?}", r[0].error);
        if with_mark {
            assert_bal(&ctx, &t, -fp(200), fp(300), &what);
            assert_eq!(resting_in(&ctx, &t, 2), vec![fp(60)], "{what}");
        } else {
            assert!(r[0].error.as_deref().unwrap_or("").starts_with("insufficient margin"), "{what}");
            assert_bal(&ctx, &t, fp(100), FixedPoint::ZERO, &what);
        }
    }
}
per_path!(upnl_funds_a_reservation_beyond_cash);

/// A negative-cash account withdraws nothing (cash bound `amount <=
/// available`, lockbox.rs:158) on either withdrawal action; closing the
/// profitable position realizes the UPnL and brings `available` back >= 0
/// with collateral conserved: −200 + 500 (PnL 10 × (150 − 100)) = 300,
/// order margin still 300 (the m2 bid), total 600 = 100 funded + 500.
fn negative_cash_cannot_withdraw_and_closing_restores_it(path: Path) {
    let (t, bidder) = (addr(2), addr(5));
    let (_d, mut ctx) = fresh(path, &[bidder]);
    long_10(&mut ctx, path, t, fp(100));
    set_mark(&ctx, 1, fp(150));
    assert!(run(&mut ctx, path, &[place(t, limit(2, true, 100, 60))])[0].success, "{path:?}");
    for a in [to_spot(t, fp(1)), withdraw_to(t, fp(1))] {
        let r = run(&mut ctx, path, &[a]);
        assert!(!r[0].success, "{path:?}: negative cash must not withdraw");
    }
    assert_bal(&ctx, &t, -fp(200), fp(300), "unchanged");
    run(&mut ctx, path, &[place(bidder, limit(1, true, 150, 10))]);
    let r = run(&mut ctx, path, &[place(t, limit(1, false, 150, 10))]);
    assert!(r[0].success, "{path:?}: closing is always allowed: {:?}", r[0].error);
    assert_eq!(pos_in(&ctx, &t, 1), FixedPoint::ZERO, "{path:?}");
    assert_bal(&ctx, &t, fp(300), fp(300), "PnL realized, bid still reserved");
}
per_path!(negative_cash_cannot_withdraw_and_closing_restores_it);
```

Fixture update (justified): `market_order_margin_tests.rs:873` `open_then_lock` short branch lock
`limit(1, true, 10, 190)` → `limit(1, true, 95, 20)` (still reserves exactly 95, still never
interacts: asks in those tests are at 100). Reason: the old lock (buy 190 @10 on short 20) would
open a 170 long if filled — F1 correctly charges its opening part at placement
(IM(2,000+1,700) − IM(2,000) = 85 > free 0) and rejects it; the fixture's intent is only
"95 locked, never matched". Update the doc comment at :852-854 accordingly.

**Implementation** — `native_executor.rs`
1. `taker_margin_limit` (NE:5023-5035): rename param `budget` → `reserved`, doc: "the order's own
   reservation; the book adds the sender's running free margin (F1, `AccountMargins`)".
2. New helper next to it:
   ```rust
   /// F1 (s517): THE placement gate (strict HL, D1 — there is no
   /// `available >= reservation` gate any more): the order's need at the
   /// POSITION-size tier (closing part free) must be `<= 0` (only reduces)
   /// or `<= free` (available + UPnL − position IM, before this reservation;
   /// `available` may be negative). Reduce-only orders are clamped to the
   /// position, so they only reduce and are skipped. A pending stop is
   /// checked as a resting order at its reservation price (it is re-checked
   /// when it triggers); without this, stops would have no gate at all.
   fn account_check(
       tiers: Option<&[MarginTier]>,
       signed: FixedPoint,
       px: FixedPoint,
       params: &PlaceOrderParams,
       res_price: FixedPoint,
       free: FixedPoint,
   ) -> Result<(), String> {
       if params.reduce_only {
           return Ok(());
       }
       // A flat (or unmarked) position is valued at the order's own price.
       let px = if px > FixedPoint::ZERO { px } else { res_price };
       let can_rest = !Self::never_rests(params); // stops: true
       match placement_need(tiers, signed, px, params.is_buy, params.quantity, res_price, can_rest) {
           None => Err(format!("order notional overflows: price {res_price} x quantity {}", params.quantity)),
           Some(need) if need > FixedPoint::ZERO && need > free => {
               Err(format!("insufficient margin: need {need}, have {free} (account)"))
           }
           Some(_) => Ok(()),
       }
   }
   ```
3. `place_order_inner`:
   * Before NE:5374 build the reader from FIELDS (the book below borrows `ctx.order_books`
     mutably, so no `AccountReader::of(ctx)` here):
     ```rust
     let reader = AccountReader { positions: &ctx.positions, oracle: &ctx.oracle, height: ctx.block_height, margin_configs: &ctx.margin_configs };
     let needs_account = !params.reduce_only;
     ```
     and change the balance-load condition NE:5378 to `order_margin_required > 0 || checked || needs_account`.
   * Inside `Ok(mut bal)`: DELETE the `bal.available < order_margin_required` rejection
     (NE:5381-5393, D1 strict HL — the debit below may take `available` negative) and put the
     account check in its place, before the debit:
     ```rust
     // F1 (s517): account-level check (see `account_check`).
     let mut account = None;
     if needs_account {
         let acct = reader.position_px(sender, market_id).and_then(|(s, px)| Ok((s, px, reader.pos_net(sender)?)));
         let (signed, px, pos_net) = match acct {
             Ok(a) => a,
             Err(e) => { /* orders_rejected_other */ return NativeActionResult::err("place_order", e.to_string()); }
         };
         let free = bal.available + pos_net;
         if let Err(msg) = Self::account_check(reader.tiers(market_id), signed, px, params,
                                               Self::reservation_price(params, mark), free) {
             if let Some(ref m) = ctx.metrics { m.orders_rejected_margin.inc(); }
             return NativeActionResult::err("place_order", msg);
         }
         account = Some((free, px));
     }
     ```
     and `margin_budget = Some(bal.available)` (NE:5395) → `margin_budget = Some(order_margin_required)`.
   * Book setup (after NE:5444, before NE:5449):
     ```rust
     // F1: the sender's free margin after this reservation, for the match.
     let mut am = AccountMargins::new(Self::margin_tiers(ctx.margin_configs.get(&market_id)));
     if let (true, Some((free, px))) = (checked, account) {
         am.insert(*sender, free - order_margin_required, px);
     }
     book.set_account_margins(am);
     ```
     and after NE:5458 `book.clear_account_margins();`.
4. `match_margin_checked` doc (NE:4976-4985): budget wording → "reservation + the sender's
   running free margin (F1)".

   The reservation price passed is `Self::reservation_price(params, mark)` — for a pending stop
   that is its cap / limit (`reserve_price`), exactly what it reserves.
5. Existing tests keep their assertions: for a flat account without UPnL `need <= free` ⇔
   `reservation <= available`, and the message keeps the `insufficient margin: need X,` prefix
   `assert_margin_reject` (market_order_margin_tests:482) matches. Any test that relied on the
   removed gate for a trader WITH a profitable position is expected to change (report it).

**Validate:** `cargo test -p torus-bridge --test account_margin_tests single && cargo test -p torus-bridge --test market_order_margin_tests && cargo test -p torus-bridge --test reduce_only_tests` · depends_on: [1, 3, 4]

Correction s517 (T5): doc comment of `negative_cash_cannot_withdraw_and_closing_restores_it` —
the closing GTC sell also reserves 75 (available −275) and releases it after the fill, so
available = −200 − 75 + 500 + 75 = 300 (the assertion was already right). No code change.

---

### T6 — bridge refactor: ONE Phase-2 step for serial and sharded (no behaviour change)

**Test first** — none new (pure refactor); the guard is the existing byte-identity suite
(`engine_parallel_tests`: serial vs threads 2/4/8 incl. error strings) plus
`market_order_margin_tests` / `reduce_only_tests` on all three paths. Run them BEFORE the edit
to record the baseline counts.

**Implementation** — `native_executor.rs`
1. New per-fold state (after `PrepOutcome`, NE:427):
   ```rust
   /// L3-ENG / F1: one Phase-2 fold — the balance cache plus (F1) each
   /// sender's position valuation. Keys are per sender, so a shard's fold
   /// equals the serial fold restricted to its senders.
   struct SenderFold {
       cache: BalanceCache,
   }
   ```
2. Extract the body of the serial loop NE:3748-3829 (identical to the worker body NE:4083-4144)
   into
   ```rust
   /// L3-ENG: Phase 2 of ONE PlaceOrder — shared by the serial loop and the
   /// sharded workers so both run literally the same code.
   fn prepare_one<T: StateBackend>(
       reader: &AccountReader<'_, T>,
       basis: &HashMap<usize, (FixedPoint, FixedPoint)>,
       fold: &mut SenderFold,
       i: usize,
       sender: &Address,
       params: &PlaceOrderParams,
   ) -> PrepOutcome
   ```
   (body = the worker body, using `reader.positions` and `reader.margin_configs`).
3. Extract the stitch NE:3708-3738 into
   `fn stitch_outcome(next_id: &mut u128, metrics: &Option<Arc<torus_telemetry::Metrics>>, market_batches: &mut HashMap<MarketId, Vec<PreparedOrder<'a>>>, results: &mut [NativeActionResult], i, sender, params, outcome)`
   (field-level borrows so it coexists with `reader`).
4. Serial branch NE:3740-3847 becomes:
   ```rust
   let mut fold = SenderFold { cache: std::mem::replace(&mut bal_cache, BalanceCache::new()) };
   for &i in &place_order_indices {
       let (sender, params) = match &flat[i] { (s, FlatAction::Place(p)) => (s, *p), _ => unreachable!() };
       let outcome = Self::prepare_one(&reader, &basis, &mut fold, i, sender, params);
       Self::stitch_outcome(&mut ctx.next_global_order_id, &ctx.metrics, &mut market_batches, &mut results, i, sender, params, outcome);
   }
   bal_cache = fold.cache;
   ```
   with `let reader = AccountReader { positions: &ctx.positions, oracle: &ctx.oracle, height: ctx.block_height, margin_configs: &ctx.margin_configs };` built once before NE:3653.
5. `phase2_parallel_prepare(reader: &AccountReader<'_, T>, basis, groups, threads, n)` —
   replaces the `positions, margin_configs` params; the worker body becomes
   `out.push((i, Self::prepare_one(reader, basis, &mut fold, i, sender, params)))` over a
   worker-local `SenderFold`; returns `fold.cache` for `merge_disjoint`.
   (`AccountReader` is `Sync`: it only holds shared refs to `Send + Sync` state, `StateBackend: Send + Sync`,
   `torus-state/src/backend.rs:24`.)

**Validate:** `cargo test -p torus-bridge --test engine_parallel_tests && cargo test -p torus-bridge --test market_order_margin_tests && cargo test -p torus-bridge --test reduce_only_tests` · depends_on: [3]

---

### T7 — bridge batch: F1 in Phase 2 (cash gate removed, D1), exclusive pools, Phase-3 accounts

**Test first** — the T5 cases' `batch` and `parallel` variants (already written) must go RED
before this task (they pass `single` after T5).

**Implementation** — `native_executor.rs`
1. `SenderFold` gains
   ```rust
   /// F1: each sender's UPnL − position IM (pre-batch, read once).
   pos_nets: HashMap<Address, FixedPoint>,
   /// F1: (sender, market) → (projected signed position, valuation price):
   /// the pre-batch position advanced by the sender's earlier ACCEPTED
   /// non-reduce-only orders of this batch (as if filled — conservative), so
   /// a later order is charged at the projected position's tier.
   proj: HashMap<(Address, MarketId), (FixedPoint, FixedPoint)>,
   ```
2. In `prepare_one`: load the balance when `required > 0 || checked || needs_account`
   (`needs_account = !params.reduce_only`, as in T5). DELETE the `bal.available < required`
   rejection (the body extracted from NE:3797-3810 / NE:4110-4122 in T6 — D1 strict HL, both
   Phase-2 paths at once since T6 made them one function); in its place, before the debit:
   ```rust
   let mut pos_net = FixedPoint::ZERO;
   if needs_account {
       let pn = match fold.pos_nets.get(sender) {
           Some(v) => *v,
           None => match reader.pos_net(sender) {
               Ok(v) => *fold.pos_nets.entry(*sender).or_insert(v),
               Err(e) => return PrepOutcome::Reject { margin: false, msg: e.to_string() },
           },
       };
       let key = (*sender, params.market_id);
       let (signed, px) = match fold.proj.get(&key) {
           Some(v) => *v,
           None => match reader.position_px(sender, params.market_id) {
               Ok(v) => *fold.proj.entry(key).or_insert(v),
               Err(e) => return PrepOutcome::Reject { margin: false, msg: e.to_string() },
           },
       };
       if let Err(msg) = Self::account_check(reader.tiers(params.market_id), signed, px, params, res_price, bal.available + pn) {
           return PrepOutcome::Reject { margin: true, msg };
       }
       pos_net = pn;
   }
   // ... existing debit ...
   if needs_account && !Self::is_stop(params) {
       let e = fold.proj.get_mut(&(*sender, params.market_id)).expect("loaded above");
       e.0 = if params.is_buy { e.0 + params.quantity } else { e.0 - params.quantity };
       if e.1 == FixedPoint::ZERO { e.1 = res_price; } // value an in-batch position at its first order's price
   }
   PrepOutcome::Pass(required, checked.then_some(pos_net))
   ```
   Rename `PreparedOrder.margin_budget` → `checked_pos_net: Option<FixedPoint>` (NE:228-230) and
   the `PrepOutcome::Pass` doc (NE:419-422): "`Some(UPnL − position IM)` of a checked taker's sender".
3. Pools — after the Phase-2 branches (after NE:3847, before the timer at NE:3849):
   ```rust
   // F1 (D2): a sender's free margin after ALL its Phase-2 reservations is
   // an EXCLUSIVE budget of the market of its FIRST checked taker (flat
   // order); in that book its takers share it as a running budget; its
   // other markets start at 0 — no two market workers spend the same free.
   let mut checked: Vec<(usize, Address, MarketId, FixedPoint)> = market_batches
       .values()
       .flatten()
       .filter_map(|p| p.checked_pos_net.map(|n| (p.index, p.sender, p.params.market_id, n)))
       .collect();
   checked.sort_unstable_by_key(|c| c.0);
   let mut pools: HashMap<(Address, MarketId), FixedPoint> = HashMap::new();
   let mut pooled: HashSet<Address> = HashSet::new();
   for (_, sender, market_id, pos_net) in checked {
       if pooled.insert(sender) {
           let available = bal_cache.load(&ctx.positions, &sender).map_or(FixedPoint::ZERO, |b| b.available);
           pools.insert((sender, market_id), available + pos_net);
       }
   }
   ```
4. Phase 3 per market (NE:3876-3900):
   * Track EVERY sender of the batch in this market (maker checks, T9, need exact in-batch
     positions; checked takers need theirs): drop the `tracked` filter —
     `Self::reduce_only_positions_for(&ctx.positions, &book, market_id, prepared.iter().map(|p| p.sender))`,
     installed whenever `prepared` is non-empty.
   * Accounts:
     ```rust
     let mut am = AccountMargins::new(tiers.clone());
     for p in prepared.iter().filter(|p| p.checked_pos_net.is_some()) {
         if am.get(&p.sender).is_none() {
             let px = reader.position_px(&p.sender, market_id).map_or(FixedPoint::ZERO, |(_, px)| px);
             am.insert(p.sender, pools.get(&(p.sender, market_id)).copied().unwrap_or(FixedPoint::ZERO), px);
         }
     }
     book.set_account_margins(am);
     ```
     (`tiers` is currently computed at NE:3890 — move it above.) MatchRequest margin:
     `p.checked_pos_net.map(|_| Self::taker_margin_limit(&tiers, p.params, p.margin_reserved))`.
   * NE:3953: add `mbr.book.clear_account_margins();`.

**Validate:** `cargo test -p torus-bridge --test account_margin_tests && cargo test -p torus-bridge --test market_order_margin_tests && cargo test -p torus-bridge --test engine_parallel_tests && cargo test -p torus-bridge --test reduce_only_tests` · depends_on: [4, 5, 6]

**Decision s517 (user, T7):** Phase 2 credits the position IM that the sender's EARLIER accepted
orders of this batch are projected to RELEASE (their closing parts, valued at the projection's
price; `SenderFold.released`) to `free` ONLY when checking a match-checked order
(`match_margin_checked`: market, IOC/FOK limit, limit sells) — its fills are re-checked against the
real position at match time. Unchecked orders (GTC/PostOnly limit buys, stops) keep the strict
`free = available + pre-batch pos_net`. The credit is NOT added to the Phase-3 pool (the book
credits the real release on the actual closing fill). One code path (`prepare_one`) for serial and
sharded. Why: implemented as first written, `second_closing_order_in_a_batch_sees_the_position_after_the_first`
failed (need on the projected flat position, free on the pre-batch −100). Tests:
`unchecked_order_gets_no_projected_release_credit`, `checked_order_credit_is_bounded_at_match`
(per path).

Correction s517 (T4 write-back, found by `checked_order_credit_is_bounded_at_match`): the running
free after a taker was `free + reservation − need(incl. hold)`, which handed the reservation a
RESTING remainder keeps (`reserve(hold, rested)`, still in `order_margin`) back to the pool — a
closing GTC sell resting in front of a market buy gave the buy 110 of phantom budget. Now
`free + reservation − kept − ΔIM(fills only)` (book unit test
`f1_resting_remainder_keeps_its_reservation_out_of_running_free`).

---

### T8 — core book: maker check at fill (HL `marginCanceled`)

**Test first** — `mod taker_margin_limit_tests`:

```rust
    struct Src(Vec<(Address, MakerAccount)>);
    impl MakerAccountSource for Src {
        fn maker_account(&self, maker: &Address, _market_id: MarketId) -> MakerAccount {
            self.0.iter().find(|(a, _)| a == maker).map(|(_, m)| *m).unwrap_or(MakerAccount {
                free: fp(1_000_000),
                signed_pos: FixedPoint::ZERO,
                px: FixedPoint::ZERO,
            })
        }
    }
    fn acct(free: FixedPoint, signed_pos: FixedPoint, px: FixedPoint) -> MakerAccount {
        MakerAccount { free, signed_pos, px }
    }

    /// F1 (s517 #4): bid 10 @100 of an under-water maker (free −95) cannot
    /// take the fill (IM 50 − its share 50 = 0 > −95): it is cancelled whole
    /// (`margin_cancels`, so its reservation is released) and the taker fills
    /// the next bid. With free 0 it fills.
    #[test]
    fn f1_under_margined_maker_is_cancelled_and_the_taker_moves_on() {
        for (free, cancelled) in [(-fp(95), true), (FixedPoint::ZERO, false)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            let id = b.place_order(order(true, fp(100), fp(10)), addr(1), 1).order_id;
            b.place_order(order(true, fp(99), fp(10)), addr(3), 1);
            let src = Src(vec![(addr(1), acct(free, FixedPoint::ZERO, FixedPoint::ZERO))]);
            let r = b.place_order_with_accounts(market_sell(fp(10)), addr(2), 2, None, Some(&src));
            assert_eq!(filled(&r), fp(10));
            let maker = if cancelled { addr(3) } else { addr(1) };
            assert!(r.fills.iter().all(|f| f.maker == maker), "free {free}");
            if cancelled {
                assert_eq!(r.margin_cancels, vec![ReduceOnlyCut { order_id: id, trader: addr(1), price: fp(100), qty: fp(10) }]);
                assert!(b.orders_for_trader(&addr(1)).is_empty());
            } else {
                assert!(r.margin_cancels.is_empty());
            }
        }
    }

    /// A maker fill that only CLOSES its position always fits.
    #[test]
    fn f1_closing_maker_fill_is_never_cancelled() {
        let mut b = OrderBook::new(1, fp(1), fp(1));
        b.place_order(order(true, fp(100), fp(10)), addr(1), 1);
        let src = Src(vec![(addr(1), acct(-fp(1_000), -fp(10), fp(100)))]);
        let r = b.place_order_with_accounts(market_sell(fp(10)), addr(2), 2, None, Some(&src));
        assert_eq!(filled(&r), fp(10));
        assert!(r.margin_cancels.is_empty());
    }

    /// FOK: the pre-check skips the maker matching would cancel. FOK sell 10
    /// @99 fills 10 from addr 3 (addr 1 cancelled); FOK sell 20 @99 is
    /// rejected and changes nothing (addr 1 still rests).
    #[test]
    fn f1_fok_precheck_mirrors_maker_cancels() {
        for (qty, fills) in [(10, 10), (20, 0)] {
            let mut b = OrderBook::new(1, fp(1), fp(1));
            b.place_order(order(true, fp(100), fp(10)), addr(1), 1);
            b.place_order(order(true, fp(99), fp(10)), addr(3), 1);
            let src = Src(vec![(addr(1), acct(-fp(95), FixedPoint::ZERO, FixedPoint::ZERO))]);
            let mut p = order(false, fp(99), fp(qty));
            p.time_in_force = TimeInForce::FOK;
            let r = b.place_order_with_accounts(p, addr(2), 2, None, Some(&src));
            assert_eq!(filled(&r), fp(fills), "qty {qty}");
            assert_eq!(b.orders_for_trader(&addr(1)).is_empty(), qty == 10, "qty {qty}");
        }
    }
```

**Implementation** — `order_book.rs`
1. Public trait + snapshot next to `AccountMargins`:
   ```rust
   /// F1 (s517 #4): where the book gets a maker's account the first time it
   /// fills in this placement / batch. MUST be deterministic and read-only
   /// (the executor reads the frozen pre-batch state); `Sync` for workers.
   pub trait MakerAccountSource: Sync {
       fn maker_account(&self, maker: &Address, market_id: MarketId) -> MakerAccount;
   }

   #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   pub struct MakerAccount {
       pub free: FixedPoint,
       pub signed_pos: FixedPoint,
       pub px: FixedPoint,
   }
   ```
   `AccountMargins::load`:
   ```rust
   fn load(&mut self, trader: Address, market_id: MarketId, src: &dyn MakerAccountSource,
           ro: &mut ReduceOnlyPositions) -> AccountMargin {
       if let Some(a) = self.accounts.get(&trader) {
           return *a;
       }
       let m = src.maker_account(&trader, market_id);
       if ro.get(&trader).is_none() {
           ro.insert(trader, m.signed_pos);
       }
       let a = AccountMargin { free: m.free, px: m.px };
       self.accounts.insert(trader, a);
       a
   }
   ```
   and the shared check (free function; used by matching AND the FOK pre-check):
   ```rust
   /// F1: can `maker` take a fill of `q` at `price` on its order resting
   /// `rem`? Its IM delta (position tier, closing part free and releasing IM)
   /// minus this fill's share of the order's reservation (the A5 telescoping
   /// piece) must fit its running free margin; a purely closing fill always
   /// fits. Commits the running free on success.
   #[allow(clippy::too_many_arguments)]
   fn maker_fill_fits(accounts: &mut AccountMargins, ro: &mut ReduceOnlyPositions,
                      src: &dyn MakerAccountSource, market_id: MarketId, maker: Address,
                      maker_is_buy: bool, price: FixedPoint, q: FixedPoint, rem: FixedPoint) -> bool {
       let a = accounts.load(maker, market_id, src, ro);
       let s = ro.get(&maker).unwrap_or(FixedPoint::ZERO);
       let size = if s < FixedPoint::ZERO { -s } else { s };
       let closing = q.min(reduce_only_allowance(s, maker_is_buy));
       let px = if a.px > FixedPoint::ZERO { a.px } else { price };
       let tiers = accounts.tiers.clone();
       let t = tiers.as_deref();
       let delta = (|| {
           let before = size.checked_mul(px).ok()?;
           let after = (size - closing).checked_mul(px).ok()?
               .checked_add(price.checked_mul(q - closing).ok()?).ok()?;
           let share = crate::margin::order_initial_margin(t, price.checked_mul(rem).ok()?)
               - crate::margin::order_initial_margin(t, price.checked_mul(rem - q).ok()?);
           Some(crate::margin::im_delta(t, before, after) - share)
       })();
       match delta {
           Some(d) if closing == q || d <= a.free => {
               accounts.set_free(&maker, a.free - d);
               true
           }
           _ => false,
       }
   }
   ```
2. `PlaceResult` (:65-91): add
   ```rust
   /// F1 (s517 #4, HL `marginCanceled`): resting makers cancelled WHOLE at
   /// match time because their account could not afford the fill. Released
   /// by the executor exactly like reduce-only cuts (A5 telescoping).
   pub margin_cancels: Vec<ReduceOnlyCut>,
   ```
   (`rejected()` :94-104 initializes `vec![]`; the final `PlaceResult { .. }` at :874 passes it.)
3. `place_order_with_margin` becomes a wrapper:
   `self.place_order_with_accounts(params, trader, timestamp, margin, None)`; the body moves to
   `pub fn place_order_with_accounts(&mut self, params, trader, timestamp, margin: Option<&TakerMarginLimit>, makers: Option<&dyn MakerAccountSource>) -> PlaceResult`.
   `place_order` keeps calling `place_order_with_margin` (all existing callers unchanged).
4. `execute_match` / `match_at_level`: extra args `accounts: &mut AccountMargins`,
   `makers: Option<&dyn MakerAccountSource>`, `market_id: MarketId`, `margin_cancels: &mut Vec<ReduceOnlyCut>`
   (pass `&mut self.account_margins`, `self.market_id`). In `match_at_level`, after the taker's
   `fits` is known and BEFORE committing the taker state (restructure T4's block so `fill_qty`
   is the candidate `q = fits.min(fill_qty)`; `exhausted` and `m.charged` are set only after
   the maker passes, so a cancelled maker leaves the taker's state untouched):
   ```rust
   // F1 (s517 #4): the maker is checked on the actual fill; one that
   // cannot afford it is cancelled whole and the taker moves on.
   if let Some(src) = makers {
       if !maker_fill_fits(accounts, ro_positions, src, market_id, maker_addr,
                           maker_side == Side::Buy, price, q, maker_remaining) {
           let cut = queue.pop_front().unwrap();
           order_index.remove(&cut.id);
           if let Some(ids) = trader_orders.get_mut(&cut.trader) { ids.retain(|&id| id != cut.id); }
           let seq = order_seq.remove(&cut.id);
           Self::mark_chunk_dirty(chunked_on, dirty_chunks, tag, raw_price, seq);
           row_journal.insert(cut.id);
           margin_cancels.push(ReduceOnlyCut { order_id: cut.id, trader: cut.trader, price: cut.price, qty: cut.remaining_qty });
           continue;
       }
   }
   ```
   (`maker_remaining` captured with the other maker fields at :1546-1548.) Back in
   `place_order_with_accounts`, remove cancelled makers from `reduce_only_index` like `reduce_only_cuts` (:798-800).
5. FOK pre-check (:762-775): when `makers` is `Some`, `can_fill_completely` gets
   `Option<(&dyn MakerAccountSource, &mut AccountMargins, &mut ReduceOnlyPositions)>` built from
   CLONES of `self.account_margins` / `self.reduce_only_positions`; in its maker loop, after
   `take` is computed and before counting it: `if !maker_fill_fits(acc, ro, src, self.market_id, order.trader, order.side == Side::Buy, order.price, take, order.remaining_qty) { continue; }`
   and on success `ro.apply_fill(&order.trader, order.side == Side::Buy, take)`. The clones are
   dropped (the pre-check never mutates the book; loads are pure snapshots so matching reloads the
   same values).

**Validate:** `cargo test -p torus-core --lib taker_margin_limit_tests && cargo test -p torus-core --lib order_book` · depends_on: [4]

Correction s517 (T8): `order_book/matching_entry_tests.rs` (frozen legacy traversal) calls
`match_at_level` / `execute_match` directly — it now passes inert F1 args
(`AccountMargins::default()`, `None`, `market_id`, a scratch `Vec`) and ignores the 4th tuple
element. `execute_match` returns `(fills, stp, ro_cuts, margin_cancels)`.

---

### T9 — bridge: wire the maker source (single path + Phase 3) and release cancels

**Test first** — `account_margin_tests.rs`:

```rust
/// F1 (s517 #4, HL marginCanceled): M (100) rests bid 10 @100 in m1 (50
/// reserved), then opens long 10 @100 in m2 (IM 50). Mark m2 at 90: UPnL
/// −100, IM 45 → free −95. T's market sell 10 in m1 cancels M's bid (its
/// reservation comes back) and fills M2's bid @99. At mark 100 (free 0) M's
/// bid fills (IM 50 == its share).
fn maker_margin_cancel(path: Path) {
    for (mark, cancelled) in [(90, true), (100, false)] {
        let (m, cp, m2, t) = (addr(1), addr(3), addr(4), addr(2));
        let (_d, mut ctx) = fresh(path, &[cp, m2, t]);
        fund_native(&ctx, &m, fp(100));
        assert!(run(&mut ctx, path, &[place(m, limit(1, true, 100, 10))])[0].success);
        run(&mut ctx, path, &[place(cp, limit(2, false, 100, 10))]);
        assert!(run(&mut ctx, path, &[place(m, limit(2, true, 100, 10))])[0].success);
        assert_eq!(pos_in(&ctx, &m, 2), fp(10));
        set_mark(&ctx, 2, fp(mark));
        run(&mut ctx, path, &[place(m2, limit(1, true, 99, 10))]);
        let r = run(&mut ctx, path, &[place(t, market(1, false, fp(1), 10))]);
        let what = format!("{path:?} mark={mark}");
        assert!(r[0].success, "{what}: {:?}", r[0].error);
        assert_eq!(pos_in(&ctx, &t, 1), -fp(10), "{what}");
        assert!(resting_in(&ctx, &m, 1).is_empty(), "{what}: M's bid gone either way");
        assert_eq!(pos_in(&ctx, &m, 1), if cancelled { FixedPoint::ZERO } else { fp(10) }, "{what}");
        assert_eq!(pos_in(&ctx, &m2, 1), if cancelled { fp(10) } else { FixedPoint::ZERO }, "{what}");
        assert_bal(&ctx, &m, fp(100), FixedPoint::ZERO, &format!("{what}: reservation released"));
    }
}
per_path!(maker_margin_cancel);
```

**Implementation**
1. `native_executor.rs`: `impl<T: StateBackend> MakerAccountSource for AccountReader<'_, T>`:
   ```rust
   /// F1: a maker's account as the book first sees it — balance and positions
   /// from the backend (Phase 3: the frozen post-Phase-1 state; single path:
   /// current state). A read error snapshots as free 0 / flat (deterministic).
   fn maker_account(&self, maker: &Address, market_id: MarketId) -> MakerAccount {
       let free = self.positions.get_native_balance(maker)
           .and_then(|b| self.view(maker, &b))
           .map_or(FixedPoint::ZERO, |v| v.free());
       let (signed_pos, px) = self.position_px(maker, market_id).unwrap_or((FixedPoint::ZERO, FixedPoint::ZERO));
       MakerAccount { free, signed_pos, px }
   }
   ```
2. Single path NE:5456-5457: `book.place_order_with_accounts(params.clone(), *sender, ctx.timestamp, margin_limit.as_ref(), Some(&reader))`.
3. `market_workers.rs`: `match_parallel_with(batches, timestamp, makers: Option<&(dyn MakerAccountSource)>)`
   and `match_parallel_capped_with(.., max_workers, makers)`; the existing `match_parallel` /
   `match_parallel_capped` delegate with `None` (tests keep compiling). `match_one` captures
   `makers` (a `Copy` shared ref of a `Sync` trait object — fine in `thread::scope`);
   `match_market(market_id, book, requests, timestamp, makers)` calls `place_order_with_accounts`.
4. NE:3906: `MarketWorkerPool::match_parallel_with(worker_batches, ctx.timestamp, Some(&reader))`
   (`reader` built from fields, it coexists with `ctx.order_books` having been drained).
5. `maker_margin_releases_cfg` (NE:4893-4898): fold `r.margin_cancels` exactly like
   `r.reduce_only_cuts` (same loop body). Both settle modes and the single path go through it.
6. Doc: "Makers" row of `docs/parity-audit-fixes-s515.md` in T12.

**Validate:** `cargo test -p torus-bridge --test account_margin_tests maker && cargo test -p torus-bridge --test maker_margin_release_tests && cargo test -p torus-bridge --test parallel_matching_tests` · depends_on: [7, 8]

---

### T10 — bridge: modify is account-checked (cash gate removed, D1)

**Test first** — `crates/torus-bridge/tests/modify_order_tests.rs`:

```rust
/// F1: a modify's extra reservation must fit the account's free margin, not
/// just `available`. Long 10 @100 in m2 (IM 50) on 100; bid 1 @90 in m1
/// (4.5) → free 95.5 − 50 = 45.5. qty 11 (+45) ok; qty 12 (+49.5) rejected
/// (was accepted: 49.5 <= available 95.5).
#[test]
fn modify_extra_reservation_needs_account_free_margin() {
    for (qty, ok) in [(11, true), (12, false)] {
        let (_d, db) = open_test_db();
        let mut ctx = make_ctx(db);
        let (t, cp) = (addr(2), addr(3));
        fund_native(&ctx, &cp, fp(1_000));
        fund_native(&ctx, &t, fp(100));
        NativeExecutor::execute(&mut ctx, &cp, &NativeAction::PlaceOrder(limit(2, false, 100, 10)));
        assert!(NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(limit(2, true, 100, 10))).success);
        assert!(NativeExecutor::execute(&mut ctx, &t, &NativeAction::PlaceOrder(limit(1, true, 90, 1))).success);
        let id = ctx.order_books[&1].orders_for_trader(&t)[0].id;
        let (s, a) = modify(t, id, None, Some(fp(qty)));
        let r = NativeExecutor::execute(&mut ctx, &s, &a);
        assert_eq!(r.success, ok, "qty {qty}: {:?}", r.error);
    }
}
```

Second test (D1 strict HL; copy `set_mark` from `market_order_margin_tests.rs:467-480`):

```rust
/// D1: UPnL funds a modify beyond cash. Same setup, mark m2 at 150: UPnL
/// +500, IM 75 → free 95.5 + 425 = 520.5. Modify the bid to 100 @90
/// (reservation 450, extra 445.5 > available 95.5): ACCEPTED, available
/// −350 (the removed `available < extra` gate rejected it).
#[test]
fn modify_extra_reservation_can_be_funded_by_upnl() {
    // ... setup as above (t long 10 @100 in m2, bid 1 @90 in m1) ...
    set_mark(&ctx, 2, fp(150));
    let (s, a) = modify(t, id, None, Some(fp(100)));
    let r = NativeExecutor::execute(&mut ctx, &s, &a);
    assert!(r.success, "{:?}", r.error);
    assert_bal(&ctx, &t, -fp(350), fp(450), "UPnL-funded");
}
```

**Implementation** — `exec_modify_order` NE:5876-5892, inside `if extra > 0`: DELETE the
`bal.available < extra` rejection (NE:5881-5886, D1 strict HL) and put the account check in its
place (the debit NE:5887-5888 may then take `available` negative):
```rust
// F1: THE modify gate — the extra must fit the account's free margin
// (UPnL counts), unless the modified order only reduces the position.
let reader = AccountReader::of(ctx);
let acct = reader.position_px(sender, market_id).and_then(|(s, px)| Ok((s, px, reader.pos_net(sender)?)));
let (signed, px, pos_net) = match acct { Ok(a) => a, Err(e) => return err(e.to_string()) };
let px = if px > FixedPoint::ZERO { px } else { price };
let reduces = placement_need(cfg.map(|c| c.tiers.as_slice()), signed, px, is_buy, qty, price, true)
    .is_some_and(|n| n <= FixedPoint::ZERO);
let free = bal.available + pos_net;
if !reduces && extra > free {
    return err(format!("insufficient margin for modify: need {extra}, have {free} (account)"));
}
```

**Validate:** `cargo test -p torus-bridge --test modify_order_tests` · depends_on: [3, 5]

---

### T11 — determinism: F1 shapes byte-identical across thread counts

**Test first** — `crates/torus-bridge/tests/engine_parallel_tests.rs` (copy `set_mark` from
`market_order_margin_tests.rs:467-480`; build market orders inline):

```rust
fn mkt(market_id: MarketId, is_buy: bool, cap: i64, qty: i64) -> PlaceOrderParams {
    PlaceOrderParams { order_type: OrderType::Market, time_in_force: TimeInForce::IOC, ..gtc(market_id, is_buy, cap, qty) }
}

/// F1: account-level shapes — a sender with positions sending checked
/// takers into three markets in one batch (exclusive pool, running budget,
/// flip), an under-water maker cancelled mid-batch, a withdrawal against
/// positions — byte-identical for threads {off, 2, 4, 8}.
#[test]
fn f1_account_margin_shapes_identical() {
    let (t, mk) = (addr(40), addr(41));
    let mut b1 = Vec::new();
    for m in 1..=4u64 {
        if m < 4 {
            b1.push(place(addr(2), gtc(m, false, 100, 40))); // no asks in m4
        }
        b1.push(place(addr(3), gtc(m, true, 99, 40)));
    }
    b1.push(place(t, gtc(1, true, 100, 10)));     // t long 10 in m1
    b1.push(place(mk, gtc(4, true, 100, 10)));    // mk's bid rests at the top of m4
    b1.push(place(mk, gtc(2, true, 100, 10)));    // mk long 10 in m2 (mark 80 later: under water)
    let b2 = vec![
        place(t, mkt(1, false, 1, 30)),           // flip: 10 close, 20 open
        place(t, mkt(2, false, 1, 20)),
        place(t, mkt(3, false, 1, 20)),
        place(addr(5), mkt(4, false, 1, 10)),     // hits mk's bid first
        place(t, gtc(5, true, 100, 60)),          // D1: UPnL-funded (mark m1 150), cash → negative
        (t, NativeAction::TransferToSpot { amount: alloy_primitives::U256::from(fp(1).raw() as u128) }),
    ];
    let run = |threads: usize| -> RunFingerprint {
        let (_dir, db) = open_test_db();
        let mut ctx = make_ctx(db);
        for a in [addr(2), addr(3), addr(5)] { fund_native(&ctx, &a, fp(1_000_000)); }
        fund_native(&ctx, &t, fp(200));
        fund_native(&ctx, &mk, fp(100));
        let mut results = Vec::new();
        let mut total_gas = Vec::new();
        for (k, batch) in [b1.clone(), b2.clone()].iter().enumerate() {
            if k == 1 {
                set_mark(&ctx, 2, fp(80));  // mk under water before batch 2
                set_mark(&ctx, 1, fp(150)); // t in profit: funds the m5 bid beyond cash
            }
            let r = NativeExecutor::execute_batch_engine_mode(&mut ctx, batch, threads);
            assert!(ctx.fatal_error.is_none());
            results.push(r.results.iter().map(|a| (a.success, a.error.clone())).collect());
            total_gas.push(r.total_gas);
        }
        ctx.save_order_books();
        RunFingerprint {
            cf_dump: state_dump(&ctx), results, total_gas,
            trade_index: ctx.trade_index, next_global_order_id: ctx.next_global_order_id,
            state_root: compute_native_state_root(&ctx.state).expect("state root"),
        }
    };
    let golden = run(0);
    for threads in [2usize, 4, 8] {
        for _ in 0..5 {
            assert_eq!(golden, run(threads), "threads={threads}");
        }
    }
}
```
(The scenario only needs identity; the implementer may add a sanity assert that the golden run
cancelled mk's m4 bid: `resting` in m4 for `mk` empty.)

**Implementation** — none expected. If it fails, the diff of `cf_dump` names the diverging CF;
fix the shared code, never the test.

**Validate:** `cargo test -p torus-bridge --test engine_parallel_tests && cargo test -p torus-bridge --test parallel_settle_tests` · depends_on: [7, 9]

---

### T12 — docs

* `docs/parity-audit-fixes-s515.md`: rewrite the "match-time margin" row (budget = reservation +
  the sender's running free margin; takers AND makers checked; `marginCanceled`), the "closing
  needs no margin" row (closing releases position IM; resting closing quantity is not charged at
  match), add a "Withdrawals" row (decision 5, SAFE variant), a "Placement gate" row (D1 strict
  HL: only the account check; UPnL funds reservations; `available` may be negative — RPC
  `available_balance` can be `-0x…`, 0x0801 reports 0 for it) and a "Liquidation formulas" line; move F1 from
  *Known deferred items* to fixed, keeping D1-D8 below as known deviations; delete the
  "Batch mode: the match-time budget uses the Phase-2 balance" deferred item (superseded by pools).
* `docs/plans/account-level-margin-f1.md`: add "Status: implemented (see -impl.md), user
  decisions D1 = strict HL, D3 = safe withdrawal; design corrections D2, D4-D8".

**Validate:** `git diff --stat -- docs/` · depends_on: [2, 3, 7, 9, 10, 14]

### T14 — core: 0x0801 `getBalances` never wraps a negative balance (D1 consumer fix)

**Test first** — `crates/torus-core/tests/precompile_tests.rs`, after `balance_reader_get_balances` (:287):
```rust
/// F1/D1: `available` may be negative (UPnL-funded reservations). The
/// uint128 ABI fields report it as 0 — `raw as u128` used to wrap −200 to
/// ~3.4e38. Order margin is reported as is.
#[test]
fn balance_reader_clamps_negative_available_to_zero() {
    let (_dir, db) = setup();
    let trader = addr(1);
    PositionManager::new(db.clone())
        .put_native_balance(&trader, &NativeBalance { available: -fp(200), order_margin: fp(300) })
        .unwrap();
    let address = precompile_address(ADDR_BALANCE_READER);
    let input = build_input("getBalances(address)", &[encode_addr(&trader)]);
    let out = execute_precompile(&address, &input, &addr(0), &db, 100).unwrap();
    let word = |i: usize| u128::from_be_bytes(out[32 * i + 16..32 * i + 32].try_into().unwrap());
    assert_eq!(word(0), 0, "native_balance");
    assert_eq!(word(2), fp(300).raw() as u128, "total_margin_used");
    assert_eq!(word(3), 0, "available");
}
```

**Implementation** — `crates/torus-core/src/precompiles.rs`: add next to `encode_fp_as_u128`
(:203-205)
```rust
/// F1/D1: a balance that may be negative, as a `uint128`: negative → 0
/// (the ABI stays `uint128`; a raw cast would wrap it to ~2^128).
pub fn encode_balance_as_u128(fp: FixedPoint) -> [u8; 32] {
    encode_u128(fp.raw().max(0) as u128)
}
```
(inside `mod abi`) and use `abi::encode_balance_as_u128` for both `native_bal.available` words
(:568, :572). The reader is deterministic
either way; this changes what EVM contracts see (consensus-visible through EVM results — same
lockstep deploy as the rest of the branch).

**Validate:** `cargo test -p torus-core --test precompile_tests balance_reader` · depends_on: [1]

### T13 — end-to-end verification (lead) — see below · depends_on: [11, 12, 14]

---

## Review fixes s517 (independent review of the F1 diff)

1. **Phase 2 under-charged the position-tier need (HIGH).** `prepare_one` admitted an order on its
   position-tier need but debited only the order-tier reservation while the D6 projection advanced
   the position as if filled; unchecked GTC buys (D7) are never re-checked, so a batch could exceed
   max leverage. Fix: `account_check` returns the need; `SenderFold.committed[sender] +=
   max(need − reservation, 0)` is subtracted from `free` in the sender's LATER Phase-2 checks
   (every order). The D2 pool subtracts the excess of UNCHECKED orders only
   (`PreparedOrder.excess_im`): a checked taker's excess is charged by the book at match, so taking
   it off the pool too would double-count it (a single tier-crossing market buy would be cut on
   batch paths but fill on the single path). Test `batch_commits_the_position_tier_need` (per path).
2. **Maker checks used the taker pool (MEDIUM-HIGH).** `AccountMargins::load` returned the D2 pool
   entry (0 outside the first checked taker's market) as a sender's maker free margin → spurious
   marginCanceled. Fix: separate `makers` map (D8 snapshot + own running updates); taker pools stay
   in `accounts`. Test `maker_uses_its_snapshot_not_the_taker_pool` (per path). Rounding: floored IM
   differences make a no-tier-change maker delta +1 raw unit (floor(B+x) − floor(B) = floor(x) + 1,
   share floor(x)) — confirmed; a +1 raw delta is now treated as 0. Book test
   `f1_maker_fill_rounding_does_not_cancel_at_free_zero`.
3. **Modify gated on the reservation delta (MEDIUM).** Now gated on
   `placement_need(new) − placement_need(old)` (same helper as placement) vs `available + pos_net`,
   for every non-reduce-only modify (also when the reservation shrinks but the opening part grows).
   Test `modify_is_gated_on_the_position_tier_need`.
4. **Maker in its own pool market used the pre-batch snapshot (MEDIUM, regression of fix 2).** In
   the book holding a sender's D2 pool, its resting makers were checked against the snapshot,
   ignoring what its takers had already spent from the pool this batch. Fix: an `accounts` entry
   IS the sender's account in that book (single path; pool market) and is shared by its takers
   AND makers (a maker fill updates it); markets where the sender only has a 0 taker budget use
   `AccountMargins::insert_taker_only`, and there its makers keep the snapshot (D8). Test
   `maker_in_its_pool_market_shares_the_pool` (per path; the single path cancels too).
5. **Modify rejected cancel-and-replace-equivalent reprices (LOW-MEDIUM, fix 3).** An old order with
   a closing part reserves more than its need (D4); the gate now credits
   `max(placement_need(old), old reservation)` (the reservation also when the old need overflows),
   i.e. `need(new) − max(need(old), reserved(old)) <= free`. Test `modify_credits_the_old_reservation`.

## Verification (end-to-end)

1. `cargo test -p torus-core` — expect all green except the 2 known pre-existing lib failures
   (`cancel_all_many_falls_back…`, `id_lookup_agrees…`, order_book fixture bugs).
   Plus `cargo test -p torus-core --test precompile_tests` (T14).
2. `cargo test -p torus-bridge` (all test files, incl. `account_margin_tests`, `market_order_margin_tests`,
   `maker_margin_release_tests`, `reduce_only_tests`, `modify_order_tests`, `lockbox_queue_tests`,
   `parallel_settle_tests`, `engine_parallel_tests`, `parallel_matching_tests`).
3. The 11-crate run of the s515 notes (1534 pass / 2 known fails at d88d581; the exact crate
   list was not recorded — use):
   `cargo test -p torus-types -p torus-state -p torus-evm -p torus-core -p torus-consensus -p torus-bridge -p torus-rpc -p torus-mempool -p torus-economics -p torus-genesis -p torus-telemetry`
   Pass count must be 1534 + new tests, fails = the same 2.
4. `cargo check --workspace --all-targets` (torus-rpc uses `torus_core::order_book`; `PlaceResult`
   gained a field).
5. Perf (Risk R10): `cargo test -p torus-bridge --release --test engine_parallel_bench -- --ignored --nocapture`
   before (d88d581) and after; report the Phase-2 (`margin_ns`) and match deltas.

## Rollback

Every task is a separate commit on `fix/parity-audit-bugs` (nothing pushed). Roll back with
`git revert <sha>` of the task commits in reverse order (T12 → T1); T6 is a pure refactor and can
stay. No schema change and no stored field is added (Option B), so no data migration either way;
the change is consensus-visible, so a rollback is again a lockstep fleet upgrade.

## Risks / design corrections (flagged, not silently changed)

* **D1 — DECIDED (user, s517): STRICT HL.** The `available >= reservation` gate is removed on
  the single path (NE:5381), both Phase-2 paths (NE:3797, NE:4110 — one `prepare_one` after T6)
  and ModifyOrder (NE:5881); the account check (need <= free, UPnL included) is the only gate.
  `available` may go negative; every consumer is listed in *Negative `available`* above. Two were
  NOT tolerant and get fixes: `liquidation.rs:233-236` (deficit, T2) and `precompiles.rs:568,572`
  (u128 wrap, T14). Residual gaps:
  - Orders whose need is <= 0 (reduce-only, closing GTC / stops) pass with no gate yet still
    debit their FULL reservation (A5 keeps `reserve(price, remaining)` for resting rows), so
    `available` can go negative without UPnL behind it. Bounded (200 orders/trader/market, each
    <= its position's notional / leverage); no collateral leaves (withdrawals are cash-bound,
    `free` drops so openings and withdrawals are blocked; releases restore it). This also removes
    the s515 deferred limitation that closing GTCs needed free cash.
  - Pending stops were only gated by cash before; they are now checked as resting orders at
    their reservation price (T5), re-checked at trigger.
  - RPC `available_balance` can be negative (signed hex); external clients must accept it (T12).
  - Liquidation is still unwired; when it is wired it must cancel the account's resting orders
    (releasing `order_margin`) before settling.
* **D2 — pool semantics refined.** "First checked taker takes what is left, later ones get what
  remains after it" is implemented as: the sender's free AFTER ALL its Phase-2 reservations is
  exclusive to the market of its first checked taker (flat order), and inside that book it is a
  RUNNING budget shared by the sender's takers and credited by their closing fills. A per-taker
  split handed out during the fold would double-spend (the pool would include `available` a later
  order then reserves, NE:3811-3816), and a static split cannot pass the existing
  `second_closing_order_in_a_batch_sees_the_position_after_the_first` (the first sell's closing
  releases IM 100 that the second needs). Other markets of that sender start at 0 (their takers
  fill within their own reservation) — conservative.
* **D3 — DECIDED (user, s517): SAFE variant.** Withdrawal allowed iff `amount <= available` AND
  `equity − order_margin − amount >= max(Σ position IM, 10% × Σ position notional)`: resting
  orders' reservations are not collateral for positions. Pinned by the 5x example (IM 100,
  reservation 100, available 100 → withdraw 100 rejected) in T1 and T3. Stricter than HL's
  `transfer_margin_required` wording where HL would count open-order margin in account value.
* **D4 — resting closing quantity is free at match, but still reserves at placement.** HL charges
  nothing to reduce; the book's need counts only the part of a resting remainder that would OPEN.
  The GTC-closing full reservation (s515 deferred item) remains, but under D1 it no longer needs
  free cash (see D1 residual gaps).
* **D5 — opening fills valued at fill price, positions at mark/entry.** Keeps every s515 number
  for flat accounts; the fill-vs-mark UPnL of in-batch fills (and realized PnL, credited in
  Phase 4) is not seen until the next batch — the design's "other markets" caveat applies to the
  same market too.
* **D2 / D6 / D7 re-checked under D1.** D2: the pool `available_end + pos_net` was already
  signed; with the cash gate gone each Phase-2 order is admitted only if its need fits the
  running `available_k + pos_net`, which already has every earlier reservation of the sender
  subtracted, so reservations + pool still never exceed the sender's pre-batch free (up to the
  order-tier vs position-tier gap) — exclusivity holds. D6 unchanged (projection is on positions,
  not cash). D7 unchanged: GTC buys stay unchecked at match; their placement need at the
  position tier is now their only gate, which was already the binding one for accounts with
  positions; the residual leak is still the tier gap.
* **D6 — batch projection.** Phase 2 projects the sender's earlier accepted orders as if filled
  (resting ones too — conservative) and values them at mark / pre-batch entry / the first order's
  price; the single path does not project resting orders. Outcomes can differ between paths when
  earlier orders rest (each path is deterministic).
* **D7 — GTC/PostOnly limit BUYS stay unchecked at match.** The book's interval search needs
  checked fills at prices >= the hold; buys fill below theirs. Their account cost is enforced at
  placement (full fill at the limit is their worst case) and by the maker check once they rest.
  Residual cross-market same-batch leak = the tier gap between their order-tier reservation and
  their position-tier IM.
* **D8 — makers.** A maker whose running free is negative is cancelled even when the fill costs
  exactly its reservation share (HL `marginCanceled`). Batch snapshots are pre-Phase-2
  (a maker that is also a sender in another market can overshoot by at most its snapshot — as
  designed). Phase 3 now tracks every batch sender's position in each book (one point read per
  sender per market) so maker checks see in-batch fills.
* **R9 — liquidation.** `check_liquidations` now values un-marked positions at entry (it skipped
  them); nothing calls it in production, but a future caller must supply marks. `check_initial_margin`
  (no production caller) is not rewritten; its Cross branch sees the corrected equity.
* **R10 — hot-path cost.** One `positions_for_trader` prefix scan (overlay merge,
  `backend.rs:1166-1189`) + one oracle read per position market, per unique non-reduce-only sender
  per batch, plus per maker per market touched in Phase 3. Bench (Verification 5); if it regresses,
  cache the view per sender for the block (valid only until the Phase-4 flush) — not planned now.
* **R11 — existing tests likely to move.** Only one fixture edit is planned (T5). Any other
  failure in `reduce_only_tests` / `maker_margin_release_tests` / `modify_order_tests` must be
  analysed: if the new expectation is the HL-correct one (a trader with positions opening beyond
  its account free margin), update the test with a one-line justification in its doc comment;
  otherwise it is a bug in the plan's code.
