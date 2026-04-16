# Technical Requirements: Trading App RPC Gaps

**Date:** 2026-04-16
**Status:** v1.2 — trimmed to reflect shipped state
**Parent:** [PRD-trading-app.md](./PRD-trading-app.md) §5.2, §5.3, §8
**Shipped in:** commit `0b2f87b` (2026-04-16)

---

## Current State

| Endpoint | Status | Impl reference | Response type |
|---|---|---|---|
| `torus_getOpenOrders` | ✅ Shipped | `crates/torus-rpc/src/torus.rs:910` | `RpcOpenOrder` (`types.rs:331-343`) |
| `torus_getOpenInterest` | ✅ Shipped | `crates/torus-rpc/src/torus.rs:973` | `RpcOpenInterest` (`types.rs:347`) |
| `torus_getMarkPrice` | ✅ Shipped | `crates/torus-rpc/src/torus.rs:1020` | `RpcMarkPrice` (`types.rs:355`) |
| `torus_getUserTrades` | ✅ Shipped | `crates/torus-rpc/src/torus.rs:1050` | `RpcUserTrade` (`types.rs:365`) |

Read the response types directly in `crates/torus-rpc/src/types.rs` — the doc will go stale, the code won't.

**Not in scope:** funding rate, vaults, sub-accounts, TWAP (Phase 3 protocol work).

---

## Architectural note: enriched data from day 1

`torus_getOpenOrders` returns all enriched fields (`order_type`, `time_in_force`, `original_qty`, `reduce_only`, `client_order_id`, `timestamp`) from launch. This was achieved without a schema migration by reading the in-memory `OrderBookSnapshot` from `CF_NATIVE_ORDER_BOOKS` via a new `OrderBook::orders_for_trader()` helper, rather than per-order entries in `CF_NATIVE_ORDERS`.

**Consequence:** the previously-planned `StoredOrderV2` migration is **retired — do not implement**. The schema on disk is unchanged; all enrichment comes from reading the live snapshot.

If a future refactor drops `CF_NATIVE_ORDER_BOOKS` (e.g., for storage-size reasons), reconsider — but not before.

---

## Test coverage

All tests inline in `crates/torus-rpc/src/lib.rs`.

### Shipped (7)

- `torus_get_open_orders_empty`
- `torus_get_open_orders_single_market`
- `torus_get_open_orders_filter_by_market`
- `torus_get_open_interest_basic`
- `torus_get_mark_price_from_order_book`
- `torus_get_user_trades_basic`
- `torus_get_user_trades_filter_market`

### Pending (7) — defer until trading-app webapp exposes real cases

| Test | Covers |
|---|---|
| `get_open_orders_multi_market` | Unfiltered query returns orders from all markets |
| `get_open_orders_after_cancel` | Cancelled order removed |
| `get_open_orders_after_fill` | Filled order removed |
| `get_open_orders_limit_500` | 500-row cap enforced |
| `get_open_interest_no_positions` | Empty market → zero OI |
| `get_mark_price_from_oracle` | Oracle-sourced price path (vs. order-book-sourced) |
| `get_user_trades_newest_first` | Descending-block ordering verified |

**Rationale for deferral:** webapp usage will identify which edge cases actually occur. Pre-speculating tests risks covering wrong scenarios. A 30-minute smoke-test sweep pre-webapp ("each endpoint returns valid shape on valid input") is the only pre-webapp testing worth doing.
