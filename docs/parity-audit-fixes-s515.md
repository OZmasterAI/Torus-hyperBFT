# Hyperliquid-parity audit fixes (s515) — client & deploy notes

Branch `fix/parity-audit-bugs`. Consensus-visible: every change below alters
executor output, so all validators must run the same build.

## Client-visible behaviour / ABI

| Bug | Change |
|-----|--------|
| 1 — market orders | `Market` / `StopMarket` `price` is a **required** worst-acceptable-price cap (`<= 0` rejected); the book never matches past it and the unfilled remainder is cancelled. **Margin (Hyperliquid-style, review 4):** a market order (buy or sell; a triggered stop-market is placed as one at trigger time) reserves `qty × mark price / tiered max leverage` — the limit-order formula at the **mark** (the oracle's aggregated stake-weighted price for the market). With no usable oracle price (none, stale, `<= 0`) it reserves at its **cap** — today always the case in production (nothing in block execution aggregates oracle prices, see *Known deferred items*). **Review 5:** only the quantity beyond what closes the sender's opposite-side position is reserved (see *closing needs no margin*). Nothing else in the block or batch moves that reservation. Its whole reservation is released after matching. A pending stop-market still holds `reserve(cap, qty)` until it fires. A price × qty that overflows is rejected (no panic). |
| match-time margin (review 4) | Margin is checked **again as the order matches** — modelled on Hyperliquid ("when orders are placed and again when they match"), but **per order**, not account-level like Hyperliquid (see F1 under *Known deferred items*). This applies to every taker whose fills can cost more than it reserved: market buys / sells (fills anywhere up to the cap), **limit sells** (they fill at bids `>=` their price) and (review 5) IOC / FOK limit buys. Before each fill: initial margin of the order's charged fill notional so far incl. this fill's charged part, **plus** for a GTC limit the notional its unfilled rest would keep resting at its limit — one notional at its own leverage tier (review 5, F4) — `<=` its reservation + the available balance right after it. The fill that does not fit is cut to the largest lot multiple that does, then **filling stops and the rest is cancelled** (it never rests). A FOK order whose complete fill does not fit is rejected whole. **Reduce-only orders are exempt** (reducing needs no margin); makers and GTC / PostOnly limit buys are unaffected. |
| closing needs no margin (review 5, F2) | Hyperliquid never charges margin to reduce a position. **Match time:** the part of a checked taker's fills that reduces the sender's opposite-side position is free; only quantity beyond it (the flip / the new position) is charged. The position is the one reduce-only policing uses — read at placement (single path) or when the batch's matching starts, then advanced through every fill of the block in that market — so all three paths free the same quantity (e.g. long 20, two market sells of 20 in one batch: the first closes free, the second is charged in full). **Placement:** an order that cannot rest (market, IOC / FOK limit — reduce-only or not) reserves only for its quantity beyond the closing allowance of the current position (single path) / the pre-batch position (batch; if the batch's earlier fills shrank the position, the extra opening part is charged against the order's budget at match time). Its whole reservation is released after matching, so this never unbalances the margin books. A GTC / PostOnly order still reserves its **full** quantity at placement: a resting row's reservation is `price × remaining` for every later release, so it cannot hold less. Its closing fills are still free at match time, and the rest keeps its full reservation. Example: long 20 @100, 95 of 100 locked in resting orders, plain market sell 20 → all 20 fill (was: 1 filled, 19 cancelled; with a mark of 100, rejected at placement). |
| leverage tiers at match time (review 5, F4) | Fills and the GTC hold are one notional charged at that notional's tier (they were charged apart, each at its own lower tier, which undercharged across a tier boundary). The need is monotone in the fill size, so the "largest lot multiple that fits" search is exact. |
| 2 — `reduce_only` | Enforced. Placement from a flat position or on the increasing side is rejected (stops included, on every path); an oversize order is clamped to the position size and reserves margin only for the clamped size; resting reduce-only orders are shrunk / cancelled when the position shrinks, closes or flips (margin released). A reduce-only maker never fills past its owner's position. |
| stops | Triggered stops are now placed through the normal path after the block's matching settles: they reserve margin, move positions, respect the StopMarket cap and re-check `reduce_only` at trigger time. |
| ModifyOrder | Only the order's **owner** may modify it (was: any account, margin charged to the owner). Validated like placement before anything changes: price `> 0`, a *new* price on the tick (a quantity-only modify of an order resting off the current tick is accepted), quantity `> 0` and `>=` lot, at least one field set; a new price at / through the opposite best is **rejected** (a modify never matches — cancel and place instead). A reduce-only order is clamped to the position (rejected when there is nothing to reduce); as in placement the lot applies to the requested quantity, so the clamp may rest below the lot. Margin uses the placement formula (tiered leverage): the difference to the new reservation is reserved first (insufficient margin ⇒ rejected, order and balances unchanged) or released exactly; overflowing price × qty is rejected. A quantity-only decrease keeps time priority. |
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
* F1 (review 3) — (superseded) the marker-staging fail-stop, the marker-ahead
  check for every block and the fallback-commit trailer (flags byte + bundle
  account addresses) went with the marker. Main's one-flush-batch EVM commit
  (93d4fff) latches the fail-stop instead when an EVM block's batch cannot be
  built or its flush fails.
* Review 4 — more fail-stops (the node halts; operator restores / resyncs):
  * B1: once the fail-stop is latched, no further block is executed or
    flushed. The boot replay stops at the failed height (it used to run the
    later heights on top of the missing state and move the native marker
    past it);
  * B2-B4 (superseded with the marker): B3 (malformed marker) and B4 (failed
    `commit_pending_bundle` fallback) concerned the marker's separate EVM
    commit. On main (93d4fff) the EVM batch, incremental or the plain
    `pending_bundle_batch` fallback, is the prefix of the block's one flush
    batch, and a batch that cannot be built latches the fail-stop. B2 (fail-stop
    when a fallback bundle writes contract storage while the incremental root
    is active) is not on this branch: the plain fallback still writes no
    `CF_HASHED_*` / `CF_TRIE_*` rows, so after a fallback the incremental trie
    can lag the full scan (open item).

## Known deferred items (not fixed on this branch)

* **F1 — margin is per ORDER, not per account (pre-existing on `main`).**
  `NativeBalance` has only `available` + `order_margin`, and fills lock no
  margin (`apply_fill_cached` credits realized PnL only). So the match-time
  budget bounds the leverage of each order on its own, and repeated orders —
  in one batch or across blocks — can exceed the max leverage. Example: 100
  USDC at 20x, two market sells each sized to the full 100 → a ~40x short.
  Leverage tiers are looked up per order (its fill notional), not at the
  resulting position size, so splitting an order keeps the top tier. On
  `main` limit buys always allowed this. The real fix is an account-level
  check — the position's initial margin (at the position-size tier) against
  collateral + unrealized PnL — at placement and at match time, as
  Hyperliquid does. Out of scope here; nothing on this branch that says it
  "matches Hyperliquid" makes margin account-level.
* **Mark price in production (follow-up: wire oracle aggregation into block
  execution).** The oracle stores validator submissions, but nothing in
  block execution runs the aggregation (`aggregate_oracle_prices` has no
  caller). So no market has a mark, and every market order reserves at its
  cap: buys conservatively, sells at ~0. The only protection for a market
  sell is the match-time check.
* **F3 — B2 fail-stop scope.** B2 (the fallback commit would write contract
  storage while the incremental root is active — it is ON by default) is
  node-local in practice: a deterministic trie error fails block validation
  (`validator.rs` ~198, root computation) before anything commits, on every
  node alike. Residual risk: the multi-block catch-up path, which commits
  with `evm_root_updates: None` (`validator.rs` ~322) and so always takes the
  fallback; there, a transient I/O error on a storage-writing block now
  halts that node (operator restores / resyncs). Better long-term: a
  fallback resync that covers storage too.
* GTC / PostOnly orders that close a position still reserve their full
  notional at placement (see *closing needs no margin*), so a trader with
  little free balance must close with a market / IOC order. Needs a
  per-order stored reservation to fix.

* A pending stop-market holds margin at its cap, not the mark. Its book row
  stores no other price, and the release at trigger time must be exact.
* Batch mode: the match-time budget uses the Phase-2 balance. For a sender
  with several orders in one batch, that balance is already reduced by its
  earlier orders' reservations and does not include realized PnL from their
  fills. The single path sees both. Identical when a sender has one order
  per block.
* After an incremental-batch failure the plain fallback leaves the
  incremental trie stale (accounts and storage). With the incremental root on
  (the default) later EVM roots are computed on that stale trie; the
  validator `StateRootMismatch` check is the safety net (see B2 above).

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
