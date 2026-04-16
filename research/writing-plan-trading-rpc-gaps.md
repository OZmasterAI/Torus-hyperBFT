# Writing Plan: Trading App RPC Gaps

**Spec:** [tech-req-trading-rpc-gaps.md](./tech-req-trading-rpc-gaps.md)
**Date:** 2026-04-16 — trimmed; most steps shipped

---

## Status

4 of 5 original steps shipped in commit `0b2f87b`. One step retired. Only test coverage remains.

| Step | Status |
|---|---|
| 1. `torus_getOpenOrders` | ✅ Shipped |
| 2. `torus_getOpenInterest` + `torus_getMarkPrice` | ✅ Shipped |
| 3. `StoredOrderV2` schema migration | ⚠️ **Retired — do not implement** (see below) |
| 4. `torus_getUserTrades` + `CF_NATIVE_USER_TRADES` index | ✅ Shipped |
| 5. Tests | ⚠️ Partial — 7 of 12 shipped, 7 pending |

For endpoint impl details and response shapes, see the spec + the code directly (`crates/torus-rpc/src/torus.rs`, `crates/torus-rpc/src/types.rs`).

---

## Why Step 3 is retired

The original plan proposed a two-PR approach: ship slim `getOpenOrders` (Step 1), then migrate `StoredOrder` → `StoredOrderV2` (Step 3) to expose enriched fields.

The implementation shipped enriched fields in one commit by a different route: reading `OrderBookSnapshot` from `CF_NATIVE_ORDER_BOOKS` (in-memory order book, which already contains `Order` structs with all fields) via a new `OrderBook::orders_for_trader()` helper. No schema migration required.

Reintroducing `StoredOrderV2` would be wasted work — it adds a migration with no new capability. Only revisit if `CF_NATIVE_ORDER_BOOKS` itself is dropped for storage-size reasons.

---

## Remaining work: 7 pending tests

Location: `crates/torus-rpc/src/lib.rs` (match the 7 shipped tests' pattern).

| Test | Covers | Priority |
|---|---|---|
| `get_open_orders_multi_market` | Unfiltered query returns orders from multiple markets | Medium |
| `get_open_orders_after_cancel` | Cancelled order disappears from results | High |
| `get_open_orders_after_fill` | Filled order disappears from results | High |
| `get_open_orders_limit_500` | 500-row cap enforced on oversized query | Low |
| `get_open_interest_no_positions` | Empty market → zero OI | Medium |
| `get_mark_price_from_oracle` | Oracle-sourced price path (distinct from order-book-sourced) | Medium |
| `get_user_trades_newest_first` | Descending-block ordering verified | High |

### Recommended order

1. **Before webapp starts** — 30-minute smoke sweep: one assertion per endpoint confirming "returns valid shape, doesn't panic." ~40 lines total. Prevents day-1 webapp-vs-broken-RPC embarrassment.
2. **After webapp MVP ships** — write the 7 pending tests above, driven by actual edge cases the webapp hits. Speculating tests in isolation risks covering wrong scenarios.

If a future session wants comprehensive coverage now, the 7 tests can be written in ~3-4 hours by mirroring the 7 shipped tests' patterns. But the default answer is: defer.
