# CoreWriter results in `torus_getBlockBody`

A contract (or EOA) that calls a CoreWriter precompile (0x0810 orders, 0x0811
staking) or the lockbox (0x0820) only queues an action. The call returns
"queued", not "executed". The queue for block `N` runs (drains) at the end
of block `N`'s native phase.

Which block an action targets (item 7 step 2, owner decision 4): an order
action (`place_order`, `cancel_order`, `cancel_all`, `cancel_by_cloid`)
queued by block `B`'s EVM targets block `B + K`, where K is the governance
key `core_writer_order_delay` (1..=100, default 10), and never a block
before the previous order action's target (so lowering K cannot let a
cancel overtake an earlier order). Every other kind (staking, lockbox)
targets `B + 1`, as before step 2. A target block holds at most 200 rows
(`CORE_WRITER_DRAIN_CAP`): a row whose target is full goes on to the next
block with room, in queue order. So block `N`'s queue holds rows queued by
block `N - 1` (any kind), by block `N - K` (order kinds), and rows that an
earlier full target sent on.

**Item 7 T (P0.0 feature `same_block_core_writer`, id 2; not scheduled on any
network yet).** From the height a network activates it, the targets and seqs
change (blocks below that height keep the rules above):
- Every non-order kind (`lockbox_deposit`, `lockbox_withdraw`, `delegate`,
  `undelegate`, `claim_rewards`, `lock_permanent`, `claim_unbonded`) queued by
  block `B`'s EVM targets `B` itself and drains at the end of `B`'s native
  phase, after all of `B`'s native actions (`queuedBlock` = `N`).
- Each target block has two seq ranges, each capped at 200: seqs `0..=199`
  for those kinds ("same-block" range) and `200..=399` for the order kinds.
  A full range sends its row on to the next block with room in that same
  range, so a backlog of delayed orders never pushes a deposit out of its
  block. Block `N` drains at most 400 rows, in seq order: the same-block
  range first, then the orders.
- Order kinds keep `max(B + K, previous order target)`; their first seq in a
  target is 200, not 0.
- At the activation height `H` itself, rows queued by `H - 1` under the old
  rules (seqs from 0, any kind) drain first, and `H`'s same-block rows follow
  them in the same range.

Every drained queue row has a result in block `N`'s action status record
(`cf_block_action_status`, record v5; v4 before s114). `torus_getBlockBody(N)` returns these
results in `coreWriterResults`, in queue order. This includes blocks that
have no native action and no EVM tx.

```json
{"jsonrpc":"2.0","id":1,"method":"torus_getBlockBody","params":[12]}
```

```json
"coreWriterResults": [
  {"seq": 0, "kind": "lockbox_deposit", "trader": "0x07…07", "queuedBlock": "0xb",
   "status": "executed", "message": "", "txHash": "0x3f…a0", "callIndex": 0},
  {"seq": 1, "kind": "undecodable", "trader": null, "queuedBlock": null,
   "status": "failed", "reason": "other",
   "message": "undecodable queued action (block 12, seq 1): …",
   "txHash": null, "callIndex": null},
  {"seq": 2, "kind": "place_order", "trader": "0x08…08", "queuedBlock": "0xb",
   "status": "rejected", "reason": "ioc_cancel", "rejectStatus": "iocCancelRejected",
   "message": "order rejected: IOC order could not immediately match against any resting order",
   "txHash": "0x5c…e1", "callIndex": 1},
  {"seq": 3, "kind": "lockbox_withdraw", "trader": "0x09…09", "queuedBlock": "0xb",
   "status": "failed", "reason": "margin", "message": "…",
   "txHash": "0x77…02", "callIndex": 0},
  {"seq": 4, "kind": "place_order", "trader": "0x0a…0a", "queuedBlock": "0xb",
   "status": "executed", "message": "", "oid": "0x2a",
   "cloid": "0x0000000000000000000000000000beef",
   "txHash": "0x77…02", "callIndex": 1},
  {"seq": 5, "kind": "cancel_by_cloid", "trader": "0x0a…0a", "queuedBlock": "0xb",
   "status": "executed", "message": "",
   "cloid": "0x0000000000000000000000000000beef",
   "txHash": "0x77…02", "callIndex": 2}
]
```

| Field | Meaning |
|---|---|
| `seq` | The row's position in block `N`'s queue. The row key is `N ‖ seq`, so `(N, seq)` identifies one queued action (at most 200 per block; under item 7 T at most 400, seqs `0..=199` same-block kinds and `200..=399` order kinds, see above). Seqs follow queue order: rows are appended to their target block's queue as the queuing blocks execute, and within one queuing block in the order of its successful precompile calls (tx order, then call order). The rows of block `N` come from `N - 1` (any kind), `N - K` (order kinds) and earlier blocks whose target was full. |
| `kind` | `place_order`, `cancel_order`, `cancel_by_cloid`, `cancel_all`, `delegate`, `undelegate`, `claim_rewards`, `lock_permanent`, `claim_unbonded`, `lockbox_deposit`, `lockbox_withdraw`; `undecodable` for a row that did not decode; `unknown` for a code this node does not know. |
| `trader` | The precompile caller (`msg.sender`, so a contract's address when a contract called it). `null` for `undecodable`. |
| `queuedBlock` | The block whose EVM queued it (hex): `N - 1` for staking and lockbox rows (`N` itself under item 7 T), `N - K` for order rows (K = `core_writer_order_delay`, default 10), earlier when a full block or an earlier order's target pushed the row on. `null` for `undecodable`. |
| `status` | `executed`, `failed` or `rejected`. `rejected` is a CoreWriter order refused with a Hyperliquid `*Rejected` status (see `rejectStatus`), as for a native placement. |
| `reason` | Stable lowercase reason (`margin`, `tick`, `ioc_cancel`, `other`, ...), as in `nativeActionFailures`. Absent for `executed`. |
| `rejectStatus` | The HL `*Rejected` name. Present only for `rejected`. |
| `message` | The executor's message (at most 96 bytes). Empty for `executed`. |
| `oid` | The order id an executed `place_order` row's order got (hex, like every order id), whether it rests or filled. CoreWriter `cancelOrder(bytes32)` takes it. Present only for an executed `place_order`. |
| `cloid` | The client order id the row carried: a `place_order` queued with one (the 7-argument `placeOrder`), or a `cancel_by_cloid`. `0x` + 32 hex digits, whatever the row's status (a placement refused as a duplicate shows the cloid it tried). Absent otherwise. |
| `txHash`, `callIndex` | The EVM tx that queued the row, see below. |

Failed rows include: the delay guard (`action queued in block X
cannot execute in block N`, a row from the current or a later block; under
item 7 T a same-block-range row of a non-order kind from block `N` itself is
allowed, an order kind or an order-range row from `N` is not; never
expected), an order type the queue cannot run (stop
types), a lockbox withdraw without enough native balance, a cancel of an
order that does not exist (`cancel_order`, `cancel_by_cloid`), a placement
whose cloid one of the caller's open orders already uses (reason
`duplicate_cloid`), and an undecodable row.

`coreWriterResults` is `null` while block `N` has not executed on this node
(like `nativeActionStatus`). It is `[]` when the block drained nothing. The
record is node-local derived data. It is not in the state hash, and it is
pruned with the block bodies (`pruning-guide.md`): once block `N`'s body is
pruned, its results are gone too.

## EVM tx link (`txHash`, `callIndex`)

Each entry also carries the EVM tx whose precompile call queued the row:

```json
{"seq": 2, "kind": "place_order", "...": "...",
 "txHash": "0x5c…e1", "callIndex": 1}
```

| Field | Meaning |
|---|---|
| `txHash` | Hash of the EVM tx in block `queuedBlock` that queued the row. `null` when this node has no link: an `undecodable` row, or a row written without one. |
| `callIndex` | The row's position among the rows that tx queued, in call order (0 = its first successful writer-precompile call; calls in reverted frames do not count). `null` with `txHash`. |

The link is not in the queue row (that is consensus state and stays
unchanged). Execution writes it to a node-local index, `cf_core_writer_tx`,
keyed like the queue row (`N ‖ seq`), in the same atomic batch as the
queuing block's EVM state and queue rows. It is written only for rows a successful
tx queued, so a block that queues nothing writes nothing there. It is not in
the state hash, and it is pruned with block `N`'s body.

## Client order ids (cloid)

The order precompile (0x0810) takes an optional client order id, a
`uint128` as in Hyperliquid. The 6-argument `placeOrder` is unchanged and
places an order without one.

```solidity
interface ICoreWriterOrders {
    // marketId: bytes32 (the u64 market id, right-aligned); side 0 = buy,
    // 1 = sell; orderType 0 = limit, 1 = market; price / quantity are
    // 1e8 fixed point; timeInForce 0 = GTC, 1 = IOC, 2 = FOK, 3 = PostOnly.
    function placeOrder(bytes32 marketId, uint8 side, uint8 orderType,
        uint128 price, uint128 quantity, uint8 timeInForce) external returns (bytes32);
    // s114: the same, plus a cloid (0 = none). Returns zero, as above: the
    // order id is known only when the queue drains (`oid` in the result).
    function placeOrder(bytes32 marketId, uint8 side, uint8 orderType,
        uint128 price, uint128 quantity, uint8 timeInForce, uint128 cloid)
        external returns (bytes32);
    function cancelOrder(bytes32 orderId) external returns (bool);
    // s114: cancel the caller's open order (resting, or a pending stop) with
    // this cloid in this market (Hyperliquid cancelByCloid).
    function cancelByCloid(bytes32 marketId, uint128 cloid) external returns (bool);
    function cancelAll(bytes32 marketId) external returns (uint32);
}
```

| Selector | Signature |
|---|---|
| `0xdf0e4c45` | `placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8)` |
| `0x622739a9` | `placeOrder(bytes32,uint8,uint8,uint128,uint128,uint8,uint128)` |
| `0xfdae07eb` | `cancelByCloid(bytes32,uint128)` |

The queued placement runs as a native `PlaceOrder` with that
`client_order_id`, under the native rule: a cloid is unique among ALL of
the caller's open orders (resting orders and pending stops) in every
market. A placement whose cloid is in use fails with reason
`duplicate_cloid`. The cloid is free again once that order is gone
(filled, cancelled, triggered and re-placed under the same cloid, ...).
`cancelByCloid` runs as the native `CancelByCloid { market_id, cloid }`
and releases the order's margin reservation. See `native-cloid.md` for
the native actions and the RPC lookup.

Queue rows (consensus state, fresh-genesis format change in s114): a
`place_order` row ends with `0`, or `1 ‖ cloid (16 BE)`; a
`cancel_by_cloid` row is kind tag 8: `market (8 BE) ‖ cloid (16 BE)`. In
the action status record (v5) a result's ext byte bit 1 says a 16-byte
cloid follows (after the 16-byte oid when bit 0 is set too).
