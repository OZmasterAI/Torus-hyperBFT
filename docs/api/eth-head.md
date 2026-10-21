# The eth head: when EVM blocks, receipts and state become visible

The `eth_*` namespace serves executed blocks only. Its head is the node's applied height (the
marker written with each executed block's state), capped by the last committed header
(`eth_head`, `crates/torus-rpc/src/eth.rs`). `eth_blockNumber`, the `latest`, `safe` and
`finalized` tags, block and log ranges, and every receipt lookup follow that head. A block above it
reads as not yet existing: `null` for the block, its txs and their receipts.

## Validators with checkpoints on

A validator with `TORUS_CHECKPOINT_INTERVAL_MS` above 0 (the default, 15 s) writes executed state to
disk once per checkpoint, not per block. A checkpoint lands as one atomic batch with the marker, at
the interval, every 100th block, the byte cap, a barrier or shutdown.

With the EVM fast path on (the default since s131; `TORUS_EVM_FAST_PATH=0` turns it off; item 7 option 0), a block with EVM txs is no
exception. Its EVM state (accounts, storage, code), its receipts, its tx index (hash -> location),
its block hash index and its CoreWriter tx links ride the same checkpoint batch as its marker.
So:

- **The eth head, the receipts and the EVM state move together, at the checkpoint.** A receipt is
  never visible before the state it produced, and no state change shows before its block does.
- **They lag execution by about one checkpoint interval while blocks keep executing** (~15 s at the
  default), plus the time the flush worker takes to write the checkpoint. There is no wall-clock
  maximum: the checkpoint triggers (the interval, every 100th block, the byte cap, a barrier,
  shutdown) are checked only when a block executes, and there is no idle checkpoint timer. If
  execution pauses after a block with EVM txs, its receipt stays invisible until the next trigger
  fires. Native state read through `torus_*` methods has lagged the same way since item 6 Phase 3.
  A client polling `eth_getTransactionReceipt` gets `null` until the checkpoint that holds the tx
  lands; then the receipt, the block and the new balances appear together. Choose receipt timeouts
  with this in mind, not a fixed 15 s.
- `eth_getTransactionCount(address, "pending")` reads the mempool's ingress view: the executed
  state, not the disk, plus the sender's pooled and committed-but-not-executed txs. A sender can
  sign its next tx at once. `"latest"` returns the nonce at the eth head and lags with it.
- `eth_call`, `eth_estimateGas`, `eth_getBalance`, `eth_getCode` and `eth_getStorageAt` answer at
  the eth head (an older or newer block is `HistoricalStateUnavailable`).

With the flag off (`TORUS_EVM_FAST_PATH=0`; on by default since s131), every block with EVM txs first drains
everything to disk and then writes its own state and receipts with its marker. The eth head then
reaches each EVM block right after it executes. The native-only blocks between EVM heights still lag
by up to one interval.

Both settings produce the same blocks, receipts and state; only when they become visible differs.
Validators may run different settings.

## Fresher reads

`--rpc-only` nodes run at interval 0: every executed block reaches the disk and the eth head with its
own write.
Point wallets and explorers that need receipts within a block time at such a node. Serving eth reads
from the ingress view on validators is design question 4 b
(`docs/plans/item7-step3-design.md`). It is not built.

See `docs/configuration-reference.md` ("EVM fast path", `TORUS_CHECKPOINT_INTERVAL_MS`) and
`docs/validator-home-network.md` (checkpoint interval).
