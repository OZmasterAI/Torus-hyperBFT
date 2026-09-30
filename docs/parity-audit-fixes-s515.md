# Hyperliquid-parity audit fixes (s515) — client & deploy notes

Branch `fix/parity-audit-bugs`. Consensus-visible: every change below alters
executor output, so all validators must run the same build. The oracle, epoch
and block-timestamp rows are on `feat/oracle-aggregation` (stacked on it, s517).

## Client-visible behaviour / ABI

| Bug | Change |
|-----|--------|
| 1 — market orders | `Market` / `StopMarket` `price` is a **required** worst-acceptable-price cap (`<= 0` rejected); the book never matches past it and the unfilled remainder is cancelled. **Margin (Hyperliquid-style, review 4):** a market order (buy or sell; a triggered stop-market is placed as one at trigger time) reserves `qty × mark price / tiered max leverage` — the limit-order formula at the **mark** (the oracle's aggregated stake-weighted price for the market). With no usable oracle price (none, stale, `<= 0`) it reserves at its **cap** — today always the case in production (block execution aggregates every block, but no price feeder submits prices, see *Known deferred items*). **Review 5:** only the quantity beyond what closes the sender's opposite-side position is reserved (see *closing needs no margin*). Nothing else in the block or batch moves that reservation. Its whole reservation is released after matching. A pending stop-market still holds `reserve(cap, qty)` until it fires. A price × qty that overflows is rejected (no panic). |
| match-time margin (review 4, F1 s517) | Margin is checked **again as the order matches** (Hyperliquid: "when orders are placed and again when they match"), **account-level** (F1). **Takers** whose fills can cost more than they reserved — market buys / sells, **limit sells**, IOC / FOK limit buys — are checked before each fill: the **increase of the position's initial margin at the position-size tier** (the position valued at the mark, entry price without one; the closing part releases its IM; plus, for a GTC limit, the part of its unfilled rest that would OPEN, at its limit) must fit the order's reservation + the sender's **running free margin** in that book. Batch: a sender's free margin after all its Phase-2 reservations (minus what its unchecked orders committed beyond their reservations, see *Placement gate*) is an **exclusive** pool of the market of its first checked taker (flat order) — its other markets start at 0 — so no two market workers spend the same free margin; in that book its takers share the pool as a running budget, credited by their closing fills. A fill that does not fit is cut to the largest lot multiple that does, then filling stops and the rest is cancelled (never rests); a FOK order whose complete fill does not fit is rejected whole. **Makers** are checked too (HL `marginCanceled`, see *Makers*). Reduce-only orders are exempt; GTC / PostOnly limit **buys** are not re-checked at match (they fill at or below their limit; their account cost is enforced at placement). |
| closing needs no margin (review 5, F2; F1 s517) | Hyperliquid never charges margin to reduce a position. **Match time:** the part of a checked taker's fills that reduces the sender's opposite-side position is free and **releases that position's initial margin** (F1), so a flip is charged only the net IM change; a purely closing fill always fits, even for an under-margined account. The **resting** part of a GTC order that would only close is not charged at match either. The position is the one reduce-only policing uses — read at placement (single path) or when the batch's matching starts, then advanced through every fill of the block in that market — so all three paths free the same quantity (e.g. long 20, two market sells of 20 in one batch: the first closes free, the second opens 1 unit on the IM its predecessor released). **Placement:** an order that cannot rest (market, IOC / FOK limit — reduce-only or not) reserves only for its quantity beyond the closing allowance of the current position (single path) / the pre-batch position (batch), running per sender / market / side across the batch's orders so one allowance never frees two orders. Its whole reservation is released after matching. A GTC / PostOnly order still **reserves** its full quantity at placement (a resting row's reservation is `price × remaining` for every later release), but its account check (see *Placement gate*) charges only what it would open, and under strict HL that reservation no longer needs free cash. Example: long 20 @100, 95 of 100 locked in resting orders, plain market sell 20 → all 20 fill. |
| leverage tiers at match time (review 5, F4) | Fills and the GTC hold are one notional charged at that notional's tier (they were charged apart, each at its own lower tier, which undercharged across a tier boundary). The need is monotone in the fill size, so the "largest lot multiple that fits" search is exact. |
| Placement gate (F1 s517, D1 strict HL) | Account-level, Hyperliquid cross margin, computed (nothing new is stored): `equity = available + order_margin + Σ UPnL`, `free = available + Σ UPnL − Σ position IM` (positions valued at the mark, at their **entry price** without one; each position's IM at its **position-size tier**). An order is accepted iff the increase of its market's IM (a complete fill; for an order that can rest, also resting its opening part) is `<= 0` (it only reduces) or `<= free`. This is the **only** placement gate on every path (single, batch serial, batch sharded) and for `ModifyOrder`: the old `available >= reservation` check is gone, so **unrealized profit funds reservations** and `available` may go **negative** (RPC `torus_getBalances` `available_balance` can then be a signed `-0x…`; the 0x0801 `getBalances` reader reports a negative balance as 0 — ABI unchanged). Error text keeps the `insufficient margin: need X, have Y` prefix (plus ` (account)`). Pending stops are checked as resting orders at their reservation price and re-checked when they trigger. Batch (Phase 2, identical serial / sharded): each sender's earlier accepted orders are projected as if filled, so a later order is charged at the projected position's tier; the part of an order's need beyond its order-tier reservation stays **committed** and comes off `free` for the sender's later orders (review fix: two GTC buys crossing a tier in one batch could otherwise exceed max leverage) and, for unchecked orders, off the match-time pool. **T7 decision:** the IM the sender's earlier orders are projected to RELEASE (their closing parts) is credited to `free` **only** when checking a match-checked order (market, IOC / FOK limit, limit sell — its fills are re-checked against the real position); GTC / PostOnly buys and stops get no such credit (the closing order may rest unfilled), and the credit never enters the match-time pool. |
| Makers (F1 s517, HL `marginCanceled`) | Every maker fill is checked: the fill's IM increase (position tier, closing part free) minus the fill's share of the maker's reservation must fit the maker's free margin — a snapshot of its account taken when the book first sees it in the placement / batch (Phase 3 reads the frozen pre-batch state), advanced by its own fills. A maker that cannot afford the fill is **cancelled whole** (its reservation released, like a reduce-only cut) and the taker continues with the next maker; the FOK pre-check skips such makers the same way. A purely closing maker fill always fits. Review fixes: in the book holding the sender's batch pool (and on the single path) its makers share that running pool with its takers; in its other markets they use the snapshot (they were checked against the 0 taker budget there); and a +1 raw-unit cost produced only by floor rounding of two IM differences is not charged. A maker filling in several markets of one batch can overshoot by at most its snapshot. |
| Withdrawals (F1 s517, D3 SAFE) | `TransferToSpot`, `Withdraw{to}` and CoreWriter `LockboxWithdraw` (drained as TransferToSpot) are allowed iff `amount <= available` **and** `available + Σ UPnL − amount >= max(Σ position IM, 10% × Σ position notional)` (HL `transfer_margin_required`; SAFE variant: resting orders' reservations are not collateral for positions). New error: `withdrawal of X would leave the account under-margined: …`; `amount > available` (any amount while `available < 0`) fails with the lockbox's existing error. Flat accounts withdraw everything, as before. |
| Liquidation formulas (F1 s517) | `MarginEngine::cross_margin_equity` = available + order margin + UPnL (the old `available − order_margin` counted reserved collateral as a loss); maintenance is taken at the entry price when a market has no mark (it was skipped); the liquidation settlement deficit uses `available + order_margin` as collateral (a negative `available` is not a loss by itself). Not wired into block execution (see *Known deferred items*). |
| 2 — `reduce_only` | Enforced. Placement from a flat position or on the increasing side is rejected (stops included, on every path); an oversize order is clamped to the position size and reserves margin only for the clamped size; resting reduce-only orders are shrunk / cancelled when the position shrinks, closes or flips (margin released). A reduce-only maker never fills past its owner's position. |
| stops | Triggered stops are now placed through the normal path after the block's matching settles: they reserve margin, move positions, respect the StopMarket cap and re-check `reduce_only` at trigger time. |
| ModifyOrder | Only the order's **owner** may modify it (was: any account, margin charged to the owner). Validated like placement before anything changes: price `> 0`, a *new* price on the tick (a quantity-only modify of an order resting off the current tick is accepted), quantity `> 0` and `>=` lot, at least one field set; a new price at / through the opposite best is **rejected** (a modify never matches — cancel and place instead). A reduce-only order is clamped to the position (rejected when there is nothing to reduce); as in placement the lot applies to the requested quantity, so the clamp may rest below the lot. Margin (F1 s517, review fix): the modify is gated like cancel + place — the new order's position-tier need (closing free) minus what the old order gives back (the larger of its need and its reservation) must fit the account's free margin (UPnL counts; `insufficient margin for modify: need X, have Y (account)`, order and balances unchanged); there is no `available >= extra` check any more. The reservation difference (placement formula, tiered leverage) is then reserved (it may take `available` negative) or released exactly; overflowing price × qty is rejected. A quantity-only decrease keeps time priority. |
| 3 — unbonding | New `ClaimUnbonded` native action (canonical action tag **26**; EIP-712 `ClaimUnbonded(uint64 nonce)`, fund-moving, not session-signable) and CoreWriterStaking `claimUnbonded()` (queued kind tag **7**). Releases every matured unbonding entry of the sender, all-or-nothing. |
| 4 — listing | `ListMarket` / `DelistMarket` / `UpdateMarketParams` native actions now error ("governance-only"). Listings go through governance; the market id is `max(existing) + 1`, assigned at proposal **execution**. |
| 5/6 — lockbox 0x0820 | Native amounts are 8-dec, EVM is 18-dec wei: native→EVM ×10^10; EVM→native floors ÷10^10 and burns the dust. `depositToNative(uint128)` is **payable** and requires `msg.value == arg`; the value is burned in-frame and the native credit is queued for the **next block**. `withdrawFromNative(uint128)` is non-payable, takes a multiple of 10^10 wei, and is also applied next block. Queue rows now commit atomically with the block's EVM bundle (F1), together with a node-local EVM-applied marker (`cf_consensus_meta` / `evm_applied_block` = height ‖ fee revenue, not in any root): after a crash between the EVM commit and the native flush, replay skips the block's EVM txs and runs the native phase with the stored fee revenue, so a tx skipped the first time (e.g. nonce too high) can never execute on replay. |
| writer precompiles | DELEGATECALL / CALLCODE / STATICCALL to any writer precompile (CoreWriter 0x0810, CoreWriterStaking 0x0811, Lockbox 0x0820) reverts — they act for `msg.sender`, so only a plain CALL is accepted. Readers 0x0800–0x0803 stay callable any way. |
| Oracle submissions (s517 T3/T4) | `SubmitOraclePrices` is validated as a whole before anything is written (all-or-nothing): sender an Active validator (as before), 1..=256 entries (`MAX_ORACLE_PRICES_PER_SUBMISSION`), no repeated market, market listed (governance), `0 < price <= 10^12` units. One row per (market, validator): a new submission overwrites the validator's previous one. |
| Oracle aggregation / mark (s517 T2, T5–T7) | Runs at the **start of every block**, before any action (`begin_block_oracle`): prune, then aggregate every listed market (ascending); the whole block reads one mark, and a submission of block h counts from block h+1. Time-based on the block header timestamp: a validator's latest submission counts while `<= 10 s` old; **min 3 reporters** (Active validators, whole-token stake weight), 3×MAD outlier cut (exact integers), **stake-weighted median**. Fewer than 3 ⇒ the last aggregate is kept and ages. **Usable** (one rule for every reader) iff it exists, `price > 0` and it is `<= 60 s` older than the block's timestamp. Readers: `AccountReader::mark` / market-order reservation, precompiles 0x0802 (stale flag) and 0x0800 (position UPnL), RPC `torus_getMarkPrice` (stale ⇒ `markPrice = indexPrice = 0`, `timestamp 0`) and `torus_getPosition` (stale ⇒ UPnL at entry price) — "now" is the latest committed header's timestamp. ABIs unchanged. Per-market errors never abort the block; a storage fault fail-stops. The native phase also runs while submission rows exist, so an idle chain still aggregates. |
| Epoch processing (s517 T0) | Runs on **every** epoch-boundary block. It lived in the native phase, which an empty block skipped, so an empty boundary block ran no epoch (no rewards / inflation, no validator status changes). Empty non-boundary blocks are unchanged. |
| Block timestamps (s517 T0b/T1) | Body validation (`validate_block` → `finish_validate`) rejects a proposal whose timestamp is below its parent's or more than **5 s** ahead of the local clock (`MAX_BLOCK_TIMESTAMP_DRIFT_SECS`); the proposer uses `max(now, parent.ts)`. Not applied on block sync, to blocks at or below the committed height, in execution or in replay. Every execution path reads the committed header timestamp. **Validators need NTP-synced clocks** (a node more than 5 s behind rejects valid proposals). Replicas vote on the header **before** body validation (pre-existing), so a bad timestamp is never executed but can stall, and committed history can still hold one; oracle ages clamp at 0. |

## Deployment requirements

* **Fresh genesis.** The lockbox unit change (8 ↔ 18 decimals) reinterprets
  every existing EVM↔native balance; there is no migration.
* **Simultaneous upgrade of every validator** (lockstep): a mixed set forks.
* F3 — legacy stop rows: `StopLimit` / `StopMarket` rows written before this
  branch carry no StopMarket cap and were reserved under the old formula; on
  trigger they would release the wrong amount of margin. Fresh genesis means
  none exist.
* F7 — old nodes cannot decode `ClaimUnbonded` (tag 26 / queued tag 7), and
  the CoreWriter drain deletes queue rows it cannot decode (new lockbox kinds
  0x20 / 0x21), silently dropping them on an old node. Another reason the
  upgrade must be lockstep.

* F1 — the EVM-applied marker is written from this build on; a node restarted
  on data from an older build has none and replays EVM as before (fresh
  genesis: never the case). A marker AHEAD of the block being executed means
  a LATER block's EVM is already durable while this block's native phase is
  not — the node fail-stops rather than executing it on top of that state.
  The check runs for every block, with or without EVM txs (it cannot fire in
  normal operation: EVM blocks are pipeline barriers and replay starts above
  the durable native marker).
* F1 (review 3) — operational fail-stops / marker format:
  * failing to stage the marker into the block's EVM batch fail-stops the
    node (never commits a bundle without it);
  * the marker is `height ‖ fee revenue` (24 bytes); when the incremental EVM
    commit failed and the plain `commit_pending_bundle` fallback was used, it
    also carries a flags byte (`0x01`) and the bundle's account addresses, so
    an EVM-skipped crash replay re-syncs `CF_HASHED_*` / `CF_TRIE_*` for them
    (the bare 24-byte record still decodes). Node-local, not in any root.
* Review 4 — more fail-stops (the node halts; operator restores / resyncs):
  * B1: once the fail-stop is latched, no further block is executed or
    flushed. The boot replay stops at the failed height (it used to run the
    later heights on top of the missing state and move the native marker
    past it). The marker-ahead check runs before the block's buffered slashes
    are written, so that fail-stop leaves no partial writes;
  * B2: when the incremental EVM commit fails and the fallback bundle writes
    contract storage or selfdestructs, the node fail-stops before committing.
    This applies only while the incremental root is active, and it is by
    default: `TORUS_INCREMENTAL_STATE_ROOT` is on unless set to `0` / `false`,
    and the node builds the trie at boot. The fallback's resync covers
    accounts only. Account-only fallbacks commit and resync as before;
  * B3: a present but malformed EVM-applied marker (short, unknown flags
    byte, partial address) or an unreadable one fail-stops. Only an absent
    row means "no marker" (treating a malformed one as absent re-executed the
    block's EVM on top of its committed bundle);
  * B4: a failed `commit_pending_bundle` fallback fail-stops (it used to run
    the native phase on top of the missing bundle).
* Oracle (s517, `feat/oracle-aggregation`) — **lockstep**: aggregation at every
  block start writes the native root, and the epoch, timestamp and submission
  rules change which blocks and actions are valid; a mixed set forks.
  **Fresh genesis semantics** for the changed `CF_NATIVE_ORACLE` row layouts:
  submissions are keyed `"sub"‖market‖validator` (31 bytes, was 39 with the
  block number) and the aggregate row is 36 bytes (a timestamp appended to the
  28-byte row). Old rows are not migrated: old submission rows are never
  overwritten, and a 28-byte aggregate does not decode as usable.

## Known deferred items (not fixed on this branch)

* **F1 — margin is account-level now (fixed on this branch, s517).** Formerly
  per order (100 USDC at 20x, two market sells each sized to the full 100 →
  ~40x). See *Placement gate*, *match-time margin*, *Makers*, *Withdrawals*.
  Known deviations from Hyperliquid, kept deliberately (details:
  `docs/plans/account-level-margin-f1-impl.md`, *Risks / design corrections*):
  * D1 — orders that only reduce (need `<= 0`) pass with no gate but still
    debit their full reservation, so `available` can go negative without UPnL
    behind it (bounded; nothing is withdrawable while it is).
  * D2 — batch pools: a sender's free margin is exclusive to the market of its
    first checked taker; its checked takers in other markets of the same batch
    fill only within their own reservation (conservative).
  * D3 — withdrawals do not count resting orders' reservations as collateral
    (stricter than HL).
  * D4 — a resting closing quantity is free at match but still reserved at
    placement.
  * D5 — opening fills are valued at the fill price, positions at mark / entry;
    UPnL of fills in the same batch is seen only from the next batch.
  * D6 — Phase 2 projects a sender's earlier orders as if filled (resting ones
    too); the single path does not, so outcomes can differ between paths when
    earlier orders rest.
  * D7 — GTC / PostOnly limit buys are not re-checked at match; their residual
    cross-market leak within one batch is the gap between order-tier and
    position-tier IM.
  * D8 — maker snapshots are pre-batch; a maker filling in several markets of
    one batch can overshoot by at most its snapshot.
* **Liquidation is NOT wired into block execution.** `run_liquidation_checks`
  / `LiquidationEngine` have no production caller (user decision s517:
  HL-style liquidation is its own item, right after oracle aggregation).
  Until then nothing closes an under-water account, so realized losses can
  exceed collateral: `available < 0` with no UPnL behind it is **bad debt**.
  `ctx.margin_configs` is also never populated in production (every market
  uses the flat 20x default).
* **Mark price in production — aggregation fixed on `feat/oracle-aggregation`
  (s517).** Time-based (a submission counts 10 s, the aggregate is stale 60 s
  after the last fresh one), stake-weighted median, min 3 reporters,
  aggregation at every block start, submission hardening (see *Oracle
  aggregation / mark*, *Oracle submissions*). **Still deferred:**
  * no price feeder exists (item B, `docs/plans/oracle-aggregation.md`):
    nothing submits `SubmitOraclePrices` in production, so no market has a
    mark yet — market orders reserve at their cap (sells at ~0 — the
    match-time check is their real bound) and F1 values positions at their
    **entry price** (UPnL 0). On a 3-validator net all three must submit
    for the price to stay fresh;
  * mark = oracle aggregate, not the Hyperliquid mark formula (item C).
* **Validate before voting (CometBFT-style), consensus item.** Replicas vote
  on a proposal's header before its body is validated (incl. the timestamp
  rule, see *Block timestamps*). A bad block is never executed, but it can
  gather votes and stall the round, and committed history can hold an
  out-of-range timestamp. Move validity checks before the vote.
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
  notional at placement (see *closing needs no margin*). Under F1 strict HL
  this no longer needs free cash (the account check charges nothing to
  close; `available` may go negative), but the reservation is still held.
  Needs a per-order stored reservation to fix.

* A pending stop-market holds margin at its cap, not the mark. Its book row
  stores no other price, and the release at trigger time must be exact.
* B2 with the incremental root OFF: no fail-stop. A trie left stale by a
  storage-writing fallback would diverge if the flag were later turned on.

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
