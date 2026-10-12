# CoreWriter results in `torus_getBlockBody`

A contract (or EOA) that calls a CoreWriter precompile (0x0810 orders, 0x0811
staking) or the lockbox (0x0820) only queues an action. The call returns
"queued", not "executed". The queue for block `N` runs (drains) at the end
of block `N`'s native phase. An action queued by block `N - 1`'s EVM runs in
block `N`.

Every drained queue row has a result in block `N`'s action status record
(`cf_block_action_status`, record v4). `torus_getBlockBody(N)` returns these
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
   "txHash": "0x77…02", "callIndex": 0}
]
```

| Field | Meaning |
|---|---|
| `seq` | The row's position in block `N`'s queue. The row key is `N ‖ seq`, so `(N, seq)` identifies one queued action. Within one block, seqs follow the order of the successful precompile calls in block `N - 1` (tx order, then call order). |
| `kind` | `place_order`, `cancel_order`, `cancel_all`, `delegate`, `undelegate`, `claim_rewards`, `lock_permanent`, `claim_unbonded`, `lockbox_deposit`, `lockbox_withdraw`; `undecodable` for a row that did not decode; `unknown` for a code this node does not know. |
| `trader` | The precompile caller (`msg.sender`, so a contract's address when a contract called it). `null` for `undecodable`. |
| `queuedBlock` | The block whose EVM queued it (hex), normally `N - 1`. `null` for `undecodable`. |
| `status` | `executed`, `failed` or `rejected`. `rejected` is a CoreWriter order refused with a Hyperliquid `*Rejected` status (see `rejectStatus`), as for a native placement. |
| `reason` | Stable lowercase reason (`margin`, `tick`, `ioc_cancel`, `other`, ...), as in `nativeActionFailures`. Absent for `executed`. |
| `rejectStatus` | The HL `*Rejected` name. Present only for `rejected`. |
| `message` | The executor's message (at most 96 bytes). Empty for `executed`. |
| `txHash`, `callIndex` | The EVM tx that queued the row, see below. |

Failed rows include: the one-block delay guard (`action queued in block X
cannot execute in block N`), an order type the queue cannot run (stop
types), a lockbox withdraw without enough native balance, a cancel of an
order that does not exist, and an undecodable row.

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
keyed like the queue row (`N ‖ seq`), in the same atomic batch as block
`N - 1`'s EVM state and queue rows. It is written only for rows a successful
tx queued, so a block that queues nothing writes nothing there. It is not in
the state hash, and it is pruned with block `N`'s body.
