# Technical Requirements: Trading App RPC Gaps

**Date:** 2026-04-16
**Status:** Draft v1.0
**Parent:** [PRD-trading-app.md](./PRD-trading-app.md) Section 5.2, 5.3, 8
**Depends on:** torus-rpc (1.8), torus-core order book, torus-state CFs

---

## Summary

The trading app PRD identified 1 blocker and 4 small RPC gaps that must be
filled before the frontend can ship. This document specifies the protocol/RPC
changes needed.

**Priority order:**
1. `torus_getOpenOrders` — **Phase 1 blocker** (trader can't see their orders)
2. `torus_getOpenInterest` — Phase 1b (market stats)
3. `torus_getMarkPrice` — Phase 1b (position valuation)
4. `torus_getUserTrades` — Phase 1b (trade history per user)
5. `StoredOrder` enrichment — improves #1 quality

**Not in scope:** Funding rate (requires new protocol mechanism), vaults,
sub-accounts, TWAP (all Phase 3 protocol work).

---

## 1. torus_getOpenOrders (Phase 1 Blocker)

### 1.1 Problem

A connected trader has no way to see their own open orders. The existing
`torus_getOrderBook` returns the full book (all traders, aggregated by price
level). The trading UI's "Open Orders" tab is non-functional without this.

### 1.2 Existing Infrastructure

The infrastructure already exists — we just need to expose it via RPC:

- **CF_NATIVE_ORDERS** (`cf.rs:24`): Key = `trader(20) + market_id(8 BE) + order_id(16 BE)`.
  Value = borsh-serialized `StoredOrder`.
- **Precompile** (`precompiles.rs:364-401`): `read_open_orders(state_db, trader, market_id)`
  already does a prefix scan on `trader(20) + market_id(8)`. This is the EVM
  precompile version — the RPC endpoint replicates the same pattern.
- **In-memory** (`order_book.rs:120-122`): `trader_orders: HashMap<Address, Vec<OrderId>>`
  exists but is private and not accessible from the RPC layer. The RocksDB
  path is correct.

### 1.3 Current StoredOrder (slim)

```rust
// precompiles.rs:1258
pub struct StoredOrder {
    pub order_id: OrderId,       // u128
    pub price: FixedPoint,       // i128, 8 decimals
    pub remaining_qty: FixedPoint,
    pub side: u8,                // 0=Buy, 1=Sell
}
```

Missing from StoredOrder: `original_qty`, `order_type`, `time_in_force`,
`reduce_only`, `client_order_id`, `timestamp`. The trading UI needs at
minimum `order_type` and `timestamp` to be useful.

### 1.4 Specification

**Method:** `torus_getOpenOrders`

**Parameters:**
```json
{
  "trader": "0x...",           // required: 20-byte address
  "market_id": "0x1"          // optional: hex u64. If omitted, returns all markets.
}
```

**Response:**
```json
[
  {
    "orderId": "0x...",        // hex u128
    "marketId": "0x1",         // hex u64
    "side": "buy",             // "buy" | "sell"
    "price": "0x5f5e100",     // hex FixedPoint (8 decimals)
    "remainingQty": "0x...",   // hex FixedPoint
    "originalQty": "0x...",    // hex FixedPoint (if available, else same as remaining)
    "orderType": "limit",      // "limit" | "market" | "stop_market" | "stop_limit"
    "timeInForce": "gtc",      // "gtc" | "ioc" | "fok" | "post_only"
    "reduceOnly": false,
    "clientOrderId": null,     // u64 or null
    "timestamp": 1713200000    // unix seconds
  }
]
```

**Limits:** Max 500 orders returned. If a trader has >500 open orders across
all markets (extremely unlikely), return the first 500 by market_id then order_id.

**Behavior:**
- If `market_id` provided: prefix scan with `trader(20) + market_id(8)`
- If `market_id` omitted: prefix scan with `trader(20)` across all markets
- Extract `market_id` from the key bytes (bytes 20..28)
- Returns empty array `[]` if no open orders

### 1.5 Decision: Enrich StoredOrder?

**Option A: Ship with slim StoredOrder now.**
Return only `{orderId, marketId, side, price, remainingQty}`. The frontend
can display orders but won't show order type, TIF, or timestamps. Good
enough for MVP — a trader can see and cancel their orders.

**Option B: Enrich StoredOrder first, then add RPC.**
Add `original_qty`, `order_type`, `time_in_force`, `reduce_only`,
`client_order_id`, `timestamp` to `StoredOrder`. Requires updating the
Borsh schema and the write path in `save_order_books()` /
`native_executor.rs`. Existing stored data can't be deserialized with
the new schema — need a migration or dual-read.

**Recommendation: Option A for immediate unblock, Option B as fast follow.**
Ship the slim version in 1-2 days. Enrich StoredOrder in a follow-up PR
(Step 3 in writing plan). The frontend can conditionally show enriched
fields when available.

---

## 2. torus_getOpenInterest

### 2.1 Problem

The markets page needs to show total open interest per market. No endpoint
exists.

### 2.2 Specification

**Method:** `torus_getOpenInterest`

**Parameters:**
```json
{ "market_id": "0x1" }    // required: hex u64
```

**Response:**
```json
{
  "marketId": "0x1",
  "longOI": "0x...",        // hex FixedPoint — total long position size
  "shortOI": "0x...",       // hex FixedPoint — total short position size
  "totalOI": "0x..."        // hex FixedPoint — longOI + shortOI (or max of the two)
}
```

**Implementation:** Iterate `CF_NATIVE_POSITIONS` with prefix `market_id(8 BE)`,
sum long sizes and short sizes separately. Key format in positions CF needs
verification — if keyed by `trader + market`, a full scan is needed.

**Alternative:** Maintain a running OI counter in `CF_NATIVE_MARKETS` or a
dedicated key, updated on each position change. Avoids full scan.

**Recommendation:** Running counter (update on position open/close/liquidation)
for performance. Full scan as fallback for correctness verification.

---

## 3. torus_getMarkPrice

### 3.1 Problem

The trading UI needs a mark/index price for each market to compute unrealized
PnL, estimated liquidation price, and display on the chart.

### 3.2 Specification

**Method:** `torus_getMarkPrice`

**Parameters:**
```json
{ "market_id": "0x1" }    // required: hex u64
```

**Response:**
```json
{
  "marketId": "0x1",
  "markPrice": "0x...",      // hex FixedPoint — oracle price or EWMA
  "indexPrice": "0x...",     // hex FixedPoint — raw oracle aggregated price
  "lastTradePrice": "0x...",// hex FixedPoint — last trade price
  "timestamp": 1713200000
}
```

**Implementation:** Read from `CF_NATIVE_ORACLE` for the market's oracle
price. `lastTradePrice` from the most recent entry in trade history.

The oracle aggregation already exists (`oracle.rs` — stake-weighted median).
This endpoint just reads the stored result.

---

## 4. torus_getUserTrades

### 4.1 Problem

A trader needs to see their own trade history (fills). The existing
`torus_getTradeHistory` is market-wide — no per-user filter.

### 4.2 Current Trade Storage

Trades are stored in `CF_NATIVE_TRADES`. Need to verify the key format.
If keyed by `market_id + block + trade_id`, there's no efficient way to
query by trader without a full scan or a secondary index.

### 4.3 Specification

**Method:** `torus_getUserTrades`

**Parameters:**
```json
{
  "trader": "0x...",           // required: address
  "market_id": "0x1",         // optional: filter to one market
  "limit": 100                 // optional: max 1000, default 100
}
```

**Response:**
```json
[
  {
    "tradeId": "0x...",
    "marketId": "0x1",
    "side": "buy",
    "price": "0x...",
    "quantity": "0x...",
    "fee": "0x...",
    "role": "maker",           // "maker" | "taker"
    "blockNumber": 12345,
    "timestamp": 1713200000
  }
]
```

### 4.4 Implementation Options

**Option A: Secondary index CF.**
Add `CF_NATIVE_USER_TRADES` keyed by `trader(20) + block(8 BE desc) + trade_id`.
Value = same trade data or a pointer to `CF_NATIVE_TRADES`. Updated at
match time alongside the primary trade write.

**Option B: Bloom filter + scan.**
For each trader, maintain a bloom filter of blocks they traded in. Query
scans only those blocks. Complex and approximate.

**Option C: Explorer/indexer handles this.**
The torus-explorer already indexes blocks. Add a per-trader trade index there
instead of in the node. This keeps the node lean.

**Recommendation: Option A** for correctness and simplicity. The write
overhead is one extra CF put per trade fill (two puts total: one for maker,
one for taker). The node already writes to multiple CFs per fill.

---

## 5. StoredOrder Enrichment (Fast Follow)

### 5.1 Problem

`StoredOrder` has only 4 fields. The trading UI benefits from showing order
type, time-in-force, original quantity, and timestamp.

### 5.2 New StoredOrder Schema

```rust
#[derive(BorshSerialize, BorshDeserialize)]
pub struct StoredOrderV2 {
    pub order_id: u128,
    pub price: FixedPoint,          // i128
    pub remaining_qty: FixedPoint,
    pub original_qty: FixedPoint,   // NEW
    pub side: u8,                   // 0=Buy, 1=Sell
    pub order_type: u8,             // NEW: 0=Limit, 1=Market, 2=StopMarket, 3=StopLimit
    pub time_in_force: u8,          // NEW: 0=GTC, 1=IOC, 2=FOK, 3=PostOnly
    pub reduce_only: bool,          // NEW
    pub client_order_id: Option<u64>, // NEW
    pub timestamp: u64,             // NEW
}
```

### 5.3 Migration Strategy

Borsh is not self-describing — adding fields breaks deserialization of
existing data. Two options:

**Option A: Dual-read with version byte.**
Prefix the value with a version byte (`0x01` for old, `0x02` for new).
Reader checks first byte and deserializes accordingly. Old data stays
readable. New writes use V2.

**Option B: Wipe and rewrite on next restart.**
`save_order_books()` already serializes the full in-memory order book to
`CF_NATIVE_ORDER_BOOKS` every block. The `CF_NATIVE_ORDERS` entries are
also rewritten from the in-memory state. On the next node restart, all
stored orders are rewritten with the new schema.

**Recommendation: Option B.** Since `save_order_books()` writes all orders
every block, the old V1 data is overwritten within one block of deploying
the new code. No migration needed — just deploy and the next block's
`save_order_books()` writes V2 format. The RPC reader attempts V2 deserialize
first, falls back to V1 if it fails (for the brief window between deploy
and first block).

### 5.4 Write Path Changes

The order-to-StoredOrder conversion happens in `native_executor.rs` inside
`save_order_books()`. Currently it serializes the full `OrderBook` to
`CF_NATIVE_ORDER_BOOKS` (for the precompile snapshot) and individual orders
to `CF_NATIVE_ORDERS` (for per-trader queries). The individual order write
path needs to construct `StoredOrderV2` from the in-memory `Order` struct.

All fields of `StoredOrderV2` are available on the `Order` struct
(`order_book.rs:31-43`) except: `order_type` needs mapping from the enum,
`time_in_force` needs mapping from the enum. Both are simple `as u8` casts
or match arms.

---

## 6. Files Summary

| File | Change | Scope | Endpoint |
|---|---|---|---|
| `torus-rpc/src/torus.rs` | Add 4 new RPC methods | Medium | All |
| `torus-core/src/precompiles.rs` | Add `StoredOrderV2`, update `StoredOrder` write | Small | getOpenOrders |
| `torus-bridge/src/native_executor.rs` | Update `save_order_books` to write V2 | Small | getOpenOrders (enriched) |
| `torus-state/src/cf.rs` | Add `CF_NATIVE_USER_TRADES` constant | Small | getUserTrades |
| `torus-bridge/src/native_executor.rs` | Write to `CF_NATIVE_USER_TRADES` at match time | Small | getUserTrades |
| `torus-rpc/src/torus.rs` | Import `CF_NATIVE_ORDERS`, `CF_NATIVE_USER_TRADES` | Small | All |

---

## 7. Testing

| Test | Verifies |
|---|---|
| `get_open_orders_empty` | Returns `[]` for address with no orders |
| `get_open_orders_single_market` | Place 3 orders, query, verify all 3 returned |
| `get_open_orders_multi_market` | Orders across 2 markets, query without filter, verify all |
| `get_open_orders_filter_by_market` | Query with market_id filter, only that market's orders returned |
| `get_open_orders_after_cancel` | Place + cancel, verify cancelled order not in results |
| `get_open_orders_after_fill` | Place + fill (via matching trade), verify filled order removed |
| `get_open_interest_basic` | Open long + short positions, verify OI |
| `get_mark_price_from_oracle` | Submit oracle prices, query mark price |
| `get_user_trades_basic` | Execute trades, query per-user, verify |
| `get_user_trades_filter_market` | Trades on 2 markets, filter by 1 |
| `stored_order_v2_roundtrip` | Serialize V2, deserialize, verify all fields |
| `stored_order_v1_fallback` | Old V1 data deserializes gracefully (missing fields get defaults) |
