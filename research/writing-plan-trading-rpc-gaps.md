# Writing Plan: Trading App RPC Gaps

**Spec:** [tech-req-trading-rpc-gaps.md](./tech-req-trading-rpc-gaps.md)
**Date:** 2026-04-16 — **updated to reflect shipped state**
**Estimated scope (original):** ~250 lines new code + ~200 lines tests
**Priority:** Step 1 was a Phase 1 blocker for the trading app frontend

---

## Current State (2026-04-16)

Most of this plan SHIPPED in commit `0b2f87b` ("feat(rpc): implement trading app RPC gaps — 4 new endpoints + trade persistence").

| Step | Status | Notes |
|---|---|---|
| **Step 1** — `torus_getOpenOrders` | ✅ **SHIPPED** | Shipped with **full enriched fields** from day 1 via different architecture than planned (see below) |
| **Step 2** — `getOpenInterest` + `getMarkPrice` | ✅ **SHIPPED** | Both endpoints live |
| **Step 3** — `StoredOrderV2` enrichment | ⚠️ **SUPERSEDED — SKIP** | Not needed. Implementation reads `OrderBookSnapshot` which already has all fields |
| **Step 4** — `getUserTrades` + `CF_NATIVE_USER_TRADES` index | ✅ **SHIPPED** | Option A (secondary index) chosen |
| **Step 5** — Tests | ⚠️ **PARTIAL** (7/~12) | Core happy-path covered; edge cases pending |

**Architectural note on Step 1 vs Step 3:**
The plan proposed a two-PR approach — ship slim `getOpenOrders` first (Step 1), then enrich `StoredOrder` to V2 (Step 3). The actual implementation shipped enriched in one go by using the in-memory `OrderBook` snapshot (`CF_NATIVE_ORDER_BOOKS`) instead of per-order `CF_NATIVE_ORDERS` entries. A new helper `OrderBook::orders_for_trader()` filters by trader. This bypassed the need for schema migration entirely. `RpcOpenOrder` now has `order_type`, `time_in_force`, `original_qty`, `reduce_only`, `client_order_id`, `timestamp` — all from day one.

**What's actually left:**
- 7 pending tests in Step 5 (listed below)
- Nothing else. Step 3 is retired, not deferred.

**Deferral recommendation:** Do not implement the remaining tests ahead of the trading-app webapp. Webapp usage will surface which edge cases actually matter; pre-speculating tests risks covering wrong scenarios. A 30-min smoke-test sweep pre-webapp would be reasonable; comprehensive tests after.

---

## Step 1: torus_getOpenOrders — ✅ SHIPPED (commit `0b2f87b`)

> Shipped in `crates/torus-rpc/src/torus.rs:910` (trait decl at line 164). Reads full `OrderBookSnapshot` from `CF_NATIVE_ORDER_BOOKS`, calls new `book.orders_for_trader(&trader)` helper in `crates/torus-core/src/order_book.rs`. Returns `Vec<RpcOpenOrder>` with all enriched fields. `RpcOpenOrder` struct at `crates/torus-rpc/src/types.rs:331-343`.

**Original plan (preserved for history):**

**File:** `crates/torus-rpc/src/torus.rs`

**1a. Add to trait** (after `torus_getTradeHistoryRange` method, ~line 65):

```rust
#[method(name = "getOpenOrders")]
async fn get_open_orders(
    &self,
    trader: String,
    market_id: Option<String>,
) -> RpcResult<Vec<RpcOpenOrder>>;
```

**1b. Define response type** (near other Rpc* structs, ~line 30):

```rust
#[derive(Serialize)]
pub struct RpcOpenOrder {
    #[serde(rename = "orderId")]
    pub order_id: String,       // hex u128
    #[serde(rename = "marketId")]
    pub market_id: String,      // hex u64
    pub side: String,           // "buy" | "sell"
    pub price: String,          // hex FixedPoint
    #[serde(rename = "remainingQty")]
    pub remaining_qty: String,  // hex FixedPoint
}
```

Slim V1 — matches current `StoredOrder` fields. Enriched fields added in Step 3. **[DEPRECATED: implementation shipped enriched directly — see note at top.]**

**1c. Implement** (in `impl TorusApiServer for RpcState`):

Follow the exact pattern from `read_open_orders()` in `precompiles.rs:364-401`:

```rust
async fn get_open_orders(&self, trader: String, market_id: Option<String>) -> RpcResult<Vec<RpcOpenOrder>> {
    let trader_addr = parse_address(&trader).map_err(ErrorObjectOwned::from)?;
    let cf = self.state.cf_handle(CF_NATIVE_ORDERS).map_err(|e| ...)?;

    let prefix = if let Some(ref mid) = market_id {
        // trader(20) + market_id(8) = 28 bytes
        let mid = parse_u64(mid).map_err(ErrorObjectOwned::from)?;
        let mut p = [0u8; 28];
        p[..20].copy_from_slice(trader_addr.as_slice());
        p[20..28].copy_from_slice(&mid.to_be_bytes());
        p.to_vec()
    } else {
        // trader(20) only — scan all markets
        trader_addr.as_slice().to_vec()
    };

    let mut orders = Vec::new();
    let iter = self.state.inner().prefix_iterator_cf(cf, &prefix);
    for item in iter {
        let (key, value) = item.map_err(|e| ...)?;
        if !key.starts_with(&prefix) { break; }
        if orders.len() >= 500 { break; }

        let stored = StoredOrder::try_from_slice(&value).map_err(|e| ...)?;
        let market_id_bytes: [u8; 8] = key[20..28].try_into().unwrap();
        let market_id = u64::from_be_bytes(market_id_bytes);

        orders.push(RpcOpenOrder {
            order_id: format!("0x{:x}", stored.order_id),
            market_id: format!("0x{:x}", market_id),
            side: if stored.side == 0 { "buy" } else { "sell" }.to_string(),
            price: format_fixed_point(stored.price),
            remaining_qty: format_fixed_point(stored.remaining_qty),
        });
    }
    Ok(orders)
}
```

**[DEPRECATED: actual implementation reads `OrderBookSnapshot` via `book.orders_for_trader()`, not a prefix scan on `CF_NATIVE_ORDERS`.]**

**1d. Add import** at top of torus.rs:

```rust
use torus_state::cf::CF_NATIVE_ORDERS;   // add to existing CF imports (line 26)
```

Also import `StoredOrder` from `torus_core::precompiles` (or define locally
if visibility is an issue).

**Verify:** `cargo check -p torus-rpc && cargo test -p torus-rpc`

---

## Step 2: torus_getOpenInterest + torus_getMarkPrice — ✅ SHIPPED (commit `0b2f87b`)

> `getOpenInterest` at `crates/torus-rpc/src/torus.rs:973`. `getMarkPrice` at line 1020. Both scan/read existing CFs; running-counter optimization was not adopted (deferred as YAGNI).

**Original plan (preserved for history):**

**File:** `crates/torus-rpc/src/torus.rs`

**2a. torus_getOpenInterest**

Add to trait:
```rust
#[method(name = "getOpenInterest")]
async fn get_open_interest(&self, market_id: String) -> RpcResult<RpcOpenInterest>;
```

Response:
```rust
#[derive(Serialize)]
pub struct RpcOpenInterest {
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(rename = "longOI")]
    pub long_oi: String,
    #[serde(rename = "shortOI")]
    pub short_oi: String,
}
```

Implementation: Iterate `CF_NATIVE_POSITIONS`. Need to check key format
first — likely `trader(20) + market_id(8)` or `market_id(8) + trader(20)`.
If keyed by trader first, a full scan is needed (filter by market_id from
key bytes). If keyed by market first, prefix scan is efficient.

If full scan is too slow, defer to Step 4 (running counter).

**2b. torus_getMarkPrice**

Add to trait:
```rust
#[method(name = "getMarkPrice")]
async fn get_mark_price(&self, market_id: String) -> RpcResult<RpcMarkPrice>;
```

Response:
```rust
#[derive(Serialize)]
pub struct RpcMarkPrice {
    #[serde(rename = "marketId")]
    pub market_id: String,
    #[serde(rename = "markPrice")]
    pub mark_price: String,
    #[serde(rename = "lastTradePrice")]
    pub last_trade_price: String,
}
```

Implementation: Read oracle price from `CF_NATIVE_ORACLE` for the market.
The oracle stores aggregated prices keyed by market_id. Last trade price
from `torus_getTradeHistory(market_id, limit=1)`.

**Verify:** `cargo check -p torus-rpc`

---

## Step 3: Enrich StoredOrder to V2 (fast follow) — ⚠️ SUPERSEDED — SKIP

> **Do not implement.** The shipped architecture in Step 1 bypasses the need for this entirely. Reading from `OrderBookSnapshot` (which already contains `Order` structs with all fields) avoids the schema migration. No V2 CF format was needed.
>
> The section below is preserved only as a historical record of the discarded approach. If a future refactor moves away from reading `OrderBookSnapshot` (e.g., if the snapshot CF is dropped for size reasons), this plan could be revisited.

**Original plan (preserved for history):**

**File:** `crates/torus-core/src/precompiles.rs`

**3a. Add StoredOrderV2** (after `StoredOrder` at ~line 1258):

```rust
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug)]
pub struct StoredOrderV2 {
    pub order_id: u128,
    pub price: i128,             // FixedPoint raw
    pub remaining_qty: i128,
    pub original_qty: i128,      // NEW
    pub side: u8,
    pub order_type: u8,          // NEW: 0=Limit, 1=Market, 2=StopMarket, 3=StopLimit
    pub time_in_force: u8,       // NEW: 0=GTC, 1=IOC, 2=FOK, 3=PostOnly
    pub reduce_only: bool,       // NEW
    pub client_order_id: Option<u64>, // NEW
    pub timestamp: u64,          // NEW
}
```

**3b-3d:** (write path + RPC reader + RpcOpenOrder enrichment — all obsolete)

---

## Step 4: torus_getUserTrades — ✅ SHIPPED (commit `0b2f87b`)

> `CF_NATIVE_USER_TRADES` added to `crates/torus-state/src/cf.rs`. Written at match time in `crates/torus-bridge/src/native_executor.rs`. RPC endpoint at `crates/torus-rpc/src/torus.rs:1050`. Prefix scan with descending-block keys for newest-first ordering.

**Original plan (preserved for history):**

**Files:**
- `crates/torus-state/src/cf.rs` — add CF constant
- `crates/torus-bridge/src/native_executor.rs` — write secondary index
- `crates/torus-rpc/src/torus.rs` — add RPC method

**4a. Add CF constant** in `cf.rs` (after `CF_NATIVE_TRADES`, ~line 23):

```rust
pub const CF_NATIVE_USER_TRADES: &str = "cf_native_user_trades";
```

Add to `ALL_CFS` array too.

**4b. Add to RocksDB column family list** in `db.rs` (wherever CFs are
opened). Verify the CF is created on DB open.

**4c. Write secondary index** in `native_executor.rs`:

In the trade execution path (after a match produces fills), for each fill
write to both:
- `CF_NATIVE_TRADES` (existing, keyed by `market_id + block + trade_id`)
- `CF_NATIVE_USER_TRADES` (new, keyed by `trader(20) + block(8 BE desc) + trade_id(16 BE)`)

The block number is stored in **descending** order (`u64::MAX - block`) so
that a prefix scan returns newest trades first.

Value: same trade data as `CF_NATIVE_TRADES` (or a minimal projection).

Each fill produces TWO entries in `CF_NATIVE_USER_TRADES` — one for maker,
one for taker. Include a `role: u8` field (0=maker, 1=taker).

**4d. Add RPC method** in `torus.rs`:

```rust
#[method(name = "getUserTrades")]
async fn get_user_trades(
    &self,
    trader: String,
    market_id: Option<String>,
    limit: Option<u32>,
) -> RpcResult<Vec<RpcUserTrade>>;
```

Implementation: prefix scan on `CF_NATIVE_USER_TRADES` with `trader(20)`.
If `market_id` provided, filter results. Limit default 100, max 1000.

**Note on state root:** `CF_NATIVE_USER_TRADES` is an index for query
convenience. It does NOT need to be included in `compute_native_state_root()`
because it's derived data — identical to `CF_NATIVE_TRADES` content, just
re-keyed. Adding it to the state root would make it consensus-critical
without benefit.

**Verify:** `cargo check -p torus-state -p torus-bridge -p torus-rpc`

---

## Step 5: Tests — ⚠️ PARTIAL (7 of ~12 shipped)

**Location:** inline in `crates/torus-rpc/src/lib.rs` (not a separate `tests/` file).

### Shipped (7)

| Test | Covers |
|---|---|
| `torus_get_open_orders_empty` | Empty result for address with no orders |
| `torus_get_open_orders_single_market` | Place 3 orders, verify all returned |
| `torus_get_open_orders_filter_by_market` | `market_id` filter works |
| `torus_get_open_interest_basic` | Long + short positions → OI sums |
| `torus_get_mark_price_from_order_book` | Mark/last-trade price when book populated |
| `torus_get_user_trades_basic` | Execute trade → maker + taker both see it |
| `torus_get_user_trades_filter_market` | `market_id` filter on user trades |

### Pending (7)

| Test | Covers | Priority |
|---|---|---|
| `get_open_orders_multi_market` | Orders across 2 markets, unfiltered query returns both | Medium |
| `get_open_orders_after_cancel` | Cancelled order removed from results | High (common UX flow) |
| `get_open_orders_after_fill` | Filled order removed from results | High (common UX flow) |
| `get_open_orders_limit_500` | 501 orders → 500 returned cap | Low (unlikely scale) |
| `get_open_interest_no_positions` | Empty market → zero OI | Medium |
| `get_mark_price_from_oracle` | Oracle-sourced price (vs. order-book-sourced) | Medium |
| `get_user_trades_newest_first` | Descending-block ordering verified | High (explicit guarantee) |

### Retired (N/A)

- `stored_order_v2_roundtrip` — StoredOrderV2 not implemented
- `stored_order_v1_fallback` — same

### Recommendation

**Do not write the 7 pending tests pre-webapp.** Rationale:
1. Webapp usage will surface which edge cases are actually hit; spec-driven tests often cover wrong scenarios.
2. The 3 "High priority" tests above are still speculative — real cancel/fill flows may have timing quirks tests-in-isolation miss.
3. A 30-min smoke-test sweep (1 test per endpoint, "doesn't panic on valid input") is the only pre-webapp testing worth doing. Comprehensive coverage after webapp exposes real bugs.

If you disagree and want comprehensive coverage now, the 7 tests above can be written in ~3-4 hours by mirroring the patterns in the 7 shipped tests.

---

## Dependency Chain (updated)

```
Step 1 (getOpenOrders) ─────── ✅ SHIPPED
         │
Step 2 (OI + mark price) ───── ✅ SHIPPED
         │
Step 3 (StoredOrderV2) ─────── ⚠️ SUPERSEDED (skip)
         │
Step 4 (getUserTrades) ─────── ✅ SHIPPED
         │
Step 5 (tests) ─────────────── ⚠️ 7 of 12 shipped, 7 pending

Only remaining work:
  (a) 30-min smoke tests pre-webapp  — cheap safety net
  (b) 7 comprehensive tests post-webapp — driven by real bugs
```

**Shipped in 1 commit (`0b2f87b`) rather than the planned sequential PRs.** The original plan assumed Step 1 would ship slim first, with Step 3 following; the actual implementation shipped enriched in one commit by routing through `OrderBookSnapshot` instead of `CF_NATIVE_ORDERS`.
