# Writing Plan: Trading App RPC Gaps

**Spec:** [tech-req-trading-rpc-gaps.md](./tech-req-trading-rpc-gaps.md)
**Date:** 2026-04-16
**Estimated scope:** ~250 lines new code + ~200 lines tests
**Priority:** Step 1 is a Phase 1 blocker for the trading app frontend

---

## Step 1: torus_getOpenOrders (BLOCKER)

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

Slim V1 — matches current `StoredOrder` fields. Enriched fields added in Step 3.

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

**1d. Add import** at top of torus.rs:

```rust
use torus_state::cf::CF_NATIVE_ORDERS;   // add to existing CF imports (line 26)
```

Also import `StoredOrder` from `torus_core::precompiles` (or define locally
if visibility is an issue).

**Verify:** `cargo check -p torus-rpc && cargo test -p torus-rpc`

---

## Step 2: torus_getOpenInterest + torus_getMarkPrice

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

## Step 3: Enrich StoredOrder to V2 (fast follow)

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

**3b. Update save path** in `crates/torus-bridge/src/native_executor.rs`:

In `save_order_books()` (~line 185), the code iterates `self.order_books`
and writes each order to `CF_NATIVE_ORDERS`. Find where individual orders
are written (may be in the same function or a sub-call). Change the
serialization from `StoredOrder` to `StoredOrderV2`, mapping fields from
the in-memory `Order` struct:

```rust
let stored = StoredOrderV2 {
    order_id: order.id,
    price: order.price.raw(),
    remaining_qty: order.remaining_qty.raw(),
    original_qty: order.original_qty.raw(),
    side: if order.side == Side::Buy { 0 } else { 1 },
    order_type: match order.order_type {
        OrderType::Limit => 0,
        OrderType::Market => 1,
        OrderType::StopMarket { .. } => 2,
        OrderType::StopLimit { .. } => 3,
    },
    time_in_force: match order.time_in_force {
        TimeInForce::GTC => 0,
        TimeInForce::IOC => 1,
        TimeInForce::FOK => 2,
        TimeInForce::PostOnly => 3,
    },
    reduce_only: order.reduce_only,
    client_order_id: order.client_order_id,
    timestamp: order.timestamp,
};
```

**3c. Update RPC reader** in `torus-rpc/src/torus.rs`:

Update `get_open_orders` to try V2 deserialize first, fall back to V1:

```rust
let stored_v2 = StoredOrderV2::try_from_slice(&value);
let stored_v1 = StoredOrder::try_from_slice(&value);

match (stored_v2, stored_v1) {
    (Ok(v2), _) => /* full fields */,
    (_, Ok(v1)) => /* slim fields, defaults for missing */,
    _ => /* skip malformed entry, log warning */,
}
```

**3d. Update RpcOpenOrder** to include enriched fields:

```rust
pub struct RpcOpenOrder {
    // ... existing fields ...
    #[serde(rename = "originalQty")]
    pub original_qty: Option<String>,
    #[serde(rename = "orderType")]
    pub order_type: Option<String>,
    #[serde(rename = "timeInForce")]
    pub time_in_force: Option<String>,
    #[serde(rename = "reduceOnly")]
    pub reduce_only: Option<bool>,
    #[serde(rename = "clientOrderId")]
    pub client_order_id: Option<String>,
    pub timestamp: Option<u64>,
}
```

All new fields are `Option` — `None` for V1 data, `Some` for V2.

**Verify:** `cargo test -p torus-core && cargo test -p torus-rpc`

---

## Step 4: torus_getUserTrades

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

## Step 5: Tests

**File:** `crates/torus-rpc/tests/trading_rpc_tests.rs` (new) or add to
existing RPC test file.

**Tests for getOpenOrders:**
- `get_open_orders_empty` — no orders, returns `[]`
- `get_open_orders_single_market` — place 3 limit orders, query, verify 3 returned with correct fields
- `get_open_orders_multi_market` — orders on market 1 and 2, query all, verify both markets present
- `get_open_orders_filter_by_market` — query with market_id, only that market's orders
- `get_open_orders_after_cancel` — place + cancel, verify gone
- `get_open_orders_after_fill` — place + match, verify fully filled order removed
- `get_open_orders_limit_500` — place 501 orders, verify only 500 returned

**Tests for getOpenInterest:**
- `get_open_interest_no_positions` — returns zero
- `get_open_interest_basic` — open positions, verify OI

**Tests for getMarkPrice:**
- `get_mark_price_from_oracle` — submit oracle prices, query

**Tests for getUserTrades:**
- `get_user_trades_basic` — execute trade, both maker and taker see it
- `get_user_trades_newest_first` — verify ordering
- `get_user_trades_filter_market` — filter works

**Tests for StoredOrderV2:**
- `stored_order_v2_roundtrip` — serialize + deserialize
- `stored_order_v1_fallback` — V1 data → graceful fallback

**Verify:** `cargo test -p torus-rpc`

---

## Dependency Chain

```
Step 1 (getOpenOrders slim) ──── SHIPS IMMEDIATELY (unblocks frontend)
         │
Step 2 (getOpenInterest + getMarkPrice) ── independent, can parallel
         │
Step 3 (StoredOrderV2 enrichment) ── depends on Step 1 shipping first
         │
Step 4 (getUserTrades + secondary index) ── independent of 1-3
         │
Step 5 (tests) ── after all above
```

Step 1 is the critical path. It can ship in isolation within 1-2 days.
Steps 2, 3, 4 are independent and can be done in parallel after Step 1.
