# Hyperliquid-parity audit fixes (s515) — client & deploy notes

Branch `fix/parity-audit-bugs`. Consensus-visible: every change below alters
executor output, so all validators must run the same build.

## Client-visible behaviour / ABI

| Bug | Change |
|-----|--------|
| 1 — market orders | `Market` / `StopMarket` `price` is a **required** worst-acceptable-price cap (`<= 0` rejected); the book never matches past it. Margin is reserved at the cap (market *sells* at `max(cap, best bid)`, see F5 — in a batch, also at least the highest Limit-buy price placed earlier in the same batch in that market); the unused part is released when the order stops resting. A price × qty that overflows is rejected (no panic). |
| 2 — `reduce_only` | Enforced. Placement from a flat position or on the increasing side is rejected (stops included, on every path); an oversize order is clamped to the position size and reserves margin only for the clamped size; resting reduce-only orders are shrunk / cancelled when the position shrinks, closes or flips (margin released). A reduce-only maker never fills past its owner's position. |
| stops | Triggered stops are now placed through the normal path after the block's matching settles: they reserve margin, move positions, respect the StopMarket cap and re-check `reduce_only` at trigger time. |
| ModifyOrder | Only the order's **owner** may modify it (was: any account, margin charged to the owner). Validated like placement before anything changes: price `> 0` on the tick, quantity `> 0` and `>=` lot, at least one field set; a new price at / through the opposite best is **rejected** (a modify never matches — cancel and place instead). A reduce-only order is clamped to the position (rejected when there is nothing to reduce). Margin uses the placement formula (tiered leverage): the difference to the new reservation is reserved first (insufficient margin ⇒ rejected, order and balances unchanged) or released exactly; overflowing price × qty is rejected. A quantity-only decrease keeps time priority. |
| 3 — unbonding | New `ClaimUnbonded` native action (canonical action tag **26**; EIP-712 `ClaimUnbonded(uint64 nonce)`, fund-moving, not session-signable) and CoreWriterStaking `claimUnbonded()` (queued kind tag **7**). Releases every matured unbonding entry of the sender, all-or-nothing. |
| 4 — listing | `ListMarket` / `DelistMarket` / `UpdateMarketParams` native actions now error ("governance-only"). Listings go through governance; the market id is `max(existing) + 1`, assigned at proposal **execution**. |
| 5/6 — lockbox 0x0820 | Native amounts are 8-dec, EVM is 18-dec wei: native→EVM ×10^10; EVM→native floors ÷10^10 and burns the dust. `depositToNative(uint128)` is **payable** and requires `msg.value == arg`; the value is burned in-frame and the native credit is queued for the **next block**. `withdrawFromNative(uint128)` is non-payable, takes a multiple of 10^10 wei, and is also applied next block. Queue rows now commit atomically with the block's EVM bundle (F1). This branch first added a node-local EVM-applied marker (`cf_consensus_meta` / `evm_applied_block`) so crash replay could skip a committed block's EVM txs; that marker was superseded by main's one-flush-batch EVM commit (93d4fff): the bundle, its queue rows, the native phase and the applied-height marker land in ONE write, so after a crash the whole block replays from its parent state and a tx skipped the first time (e.g. nonce too high) can never execute on replay. |
| writer precompiles | DELEGATECALL / CALLCODE / STATICCALL to any writer precompile (CoreWriter 0x0810, CoreWriterStaking 0x0811, Lockbox 0x0820) reverts — they act for `msg.sender`, so only a plain CALL is accepted. Readers 0x0800–0x0803 stay callable any way. |

## Deployment requirements

* **Fresh genesis.** The lockbox unit change (8 ↔ 18 decimals) reinterprets
  every existing EVM↔native balance; there is no migration.
* **Simultaneous upgrade of every validator** (lockstep): a mixed set forks.
* F3 — legacy stop rows: `StopLimit` / `StopMarket` rows written before this
  branch carry no StopMarket cap and were reserved under the old formula; on
  trigger they would release the wrong amount of margin. Fresh genesis means
  none exist.
* F7 — old nodes cannot decode `ClaimUnbonded` (tag 27 / queued tag 7), and
  the CoreWriter drain deletes queue rows it cannot decode (new lockbox kinds
  0x20 / 0x21), silently dropping them on an old node. Another reason the
  upgrade must be lockstep.

* F1 — (superseded) this branch wrote a node-local EVM-applied marker and
  fail-stopped on a marker AHEAD of the block being executed. Main's
  one-flush-batch EVM commit (93d4fff) replaced it: an EVM block is durable
  only together with its native phase and applied-height marker, so there is
  no marker and no deployment step for it.

## Known deferred items (not fixed on this branch)

* A `ModifyOrder` whose new price would cross the book is rejected; Hyperliquid
  (cancel + new order) would match it.

* The executor accepts orders for unknown market ids (a book is created on demand).
* A governance proposal whose execution fails blocks later proposals.
* `cancel_all` does not release the margin of the trader's pending stops.
* Stops cannot be cancelled by order id.
* Unbonding stake is not slashable.
* `eth_call` / `eth_estimateGas` reject calls reaching 0x0820 (writer
  precompiles are denied in simulation), so wallets cannot estimate deposits.
* Wallet staking commands send amounts in wei.
* Governance `Delist` / `UpdateParams` proposals are text-only (no executor effect).
