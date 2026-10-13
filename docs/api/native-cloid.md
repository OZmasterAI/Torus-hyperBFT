# Client order ids (cloid): native actions and RPC

A client order id (cloid) is a `u128` the trader picks for an order, as in
Hyperliquid. It is optional. It lets a client cancel or look up an order
without knowing the order id the chain assigned. CoreWriter (EVM) orders
take one too: see `corewriter-results.md`, "Client order ids".

## Rule

A cloid is unique among ALL of a trader's open orders, in every market.
Open orders are resting orders and pending stops. A placement whose cloid
is already used by one of the sender's open orders is rejected with reason
`duplicate_cloid` (status `failed`; the message names the order and market
that holds it). The cloid is free again once that order is gone: filled,
cancelled (by id, by cloid, cancel-all), cut (reduce-only, margin),
self-trade cancelled, or liquidated. A stop that triggers places its order
under the same cloid and the same order id; the cloid stays taken while
that order rests. Other traders may use the same cloid.

Within one block execution batch, cancels run before placements, so a
batch can cancel a cloid and place a new order with it. Two placements of
the same cloid by one sender in one batch: the first one claims it (even if
it then fails or fills completely) and the second is rejected.

## Placing: `PlaceOrderParams.client_order_id`

`client_order_id: Option<u128>` on `PlaceOrder` and on every order of a
`PlaceOrderBatch`.

- JSON (`torus_submitNativeAction(s)` JSON payloads, `torus_getBlockBody`):
  `"client_order_id": "0x0123456789abcdeffedcba9876543210"` (`0x` + 32 hex
  digits on output; 1-32 hex digits, or a JSON integer up to 2^64 - 1, on
  input), or `null`.
- bincode (`torus_submitNativeActionsBin`): `Option<u128>` (tag byte, then
  16 bytes little-endian).
- Canonical action bytes (action hash): `0`, or `1 ‖ cloid (16 BE)`.
- EIP-712: the `PlaceOrder` and `PlaceOrderItem` types carry
  `uint128 clientOrderId,bool hasClientOrderId` (`clientOrderId = 0` when
  absent):

```
PlaceOrder(uint64 marketId,bool isBuy,int128 price,int128 quantity,uint8 orderType,uint8 timeInForce,bool reduceOnly,uint128 clientOrderId,bool hasClientOrderId,uint64 nonce)
PlaceOrderItem(uint64 marketId,bool isBuy,int128 price,int128 quantity,uint8 orderType,uint8 timeInForce,bool reduceOnly,uint128 clientOrderId,bool hasClientOrderId)
```

(s114: this was `uint64 clientOrderId`. Signatures over the old types do
not verify.)

## Cancelling: `NativeAction::CancelByCloid { market_id, cloid }`

Cancels the sender's open order with that cloid in `market_id`: a resting
order, or a pending stop. Its margin reservation is released. It fails
(`no open order with cloid ... in market ...`) when the sender has none
there. Session keys may sign it (a trading action). It is a cancel for
block ordering, mempool priority and the ingress shed.

- JSON: `{"CancelByCloid": {"market_id": 1, "cloid": "0x…"}}`.
- bincode: enum tag 29 (appended last).
- Canonical bytes: `29 ‖ market_id (8 BE) ‖ cloid (16 BE)`.
- EIP-712: `CancelByCloid(uint64 marketId,uint128 cloid,uint64 nonce)`.

`CancelOrder { order_id }` also cancels a pending stop by its order id
(s114; before, stops could only be cancelled with `CancelAllOrders`). Only
the stop's owner can cancel it.

## RPC

- `torus_getOpenOrders` shows each order's `clientOrderId` (`0x` + 32 hex
  digits, or `null`).
- `torus_getOrderByCloid(trader, cloid)` (Hyperliquid `orderStatus` by
  cloid): the trader's open order with that cloid, or `null`. `cloid` is
  `0x` + 1-32 hex digits.

```json
{"jsonrpc":"2.0","id":1,"method":"torus_getOrderByCloid",
 "params":["0xaaaa…aaaa","0x0123456789abcdeffedcba9876543210"]}
```

```json
{"status": "open",
 "order": {"orderId": "0x2a", "marketId": "0x1", "side": "buy",
           "price": "100.00000000", "remainingQty": "2.00000000",
           "originalQty": "2.00000000", "orderType": "limit",
           "timeInForce": "gtc", "reduceOnly": false,
           "clientOrderId": "0x0123456789abcdeffedcba9876543210",
           "timestamp": 1001}}
```

`status` is `open` for a resting order and `triggerPending` for a pending
stop. For a stop, `order.price` is the price its triggered order will
carry (the stop-limit's limit, or the stop-market's price cap) and
`triggerPrice` is its trigger. It reads committed state, like
`torus_getOpenOrders`. Filled or cancelled orders are not kept, so they
are `null`.

## Format notes (s114, fresh genesis)

Order and stop rows store the cloid as `0`, or `1 ‖ 16 BE` (was 8 bytes).
These rows are in the native state root. Old encodings are not read.
The `(trader, cloid)` index is not stored: every node rebuilds it from the
books when it loads them.
