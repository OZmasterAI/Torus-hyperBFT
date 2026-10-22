# Trading fees (maker / taker) behind P0.0 feature `trading_fees` (s138)

Owner-approved s138 (testnet MUST). Branch `feat/trading-fees`.

Audit (s138): nothing charges a trading fee today. `NativeExecContext::treasury_address`
feeds only the end-of-block native-token (gas) fee split
(`NativeExecutor::distribute_fees` -> `RewardDistributor::distribute_block_fees`,
burn / validator / treasury / dev pool, `FEE_START_*` / `FEE_END_*`). That split is in
native TORUS (U256 account balances); trading collateral is the quote asset in the
native ledger (`NativeBalance.available`, `FixedPoint`, 8 decimals). The two never mix.

## Feature bit

`Feature::TradingFees = 4`, name `trading_fees` (ids 2 and 3 are taken by item 7 T
`SameBlockCoreWriter` and `FundingEngine` on their branches). In `KNOWN_FEATURES`, **not** in the
built-in table (`builtin_activations` stays empty) and not in
`DEFAULT_GENESIS_ACTIVATIONS`: no chain charges a fee until a release schedules it (a
genesis `consensus.activations.trading_fees = H` on a new chain, or a built-in row for
a running chain). Execution reads the bit from the executed block's header
(`ctx.feature_active(Feature::TradingFees)`), so replay charges exactly the heights the
chain agreed on.

Bit off: no fee is computed, no row is read or written, no balance load is added: the
state (and the running state hash) is byte-identical to today. Pinned by
`perf_equivalence_golden` (unchanged goldens) and `trading_fees_tests::bit_off_*`.

## Fee model

- **Global** taker and maker rates (one pair for the chain), in **ppm of notional**
  (1 ppm = 0.0001 % = 0.01 bp). bps is too coarse: HL tier 0 is 4.5 bp / 1.5 bp.
  Per-market rates are a later, additive option (`trading_taker_fee_ppm:<market>`
  keys); nothing in v1 needs them.
- **Recommended defaults (built in): taker 450 ppm (0.045 %), maker 150 ppm
  (0.015 %)** = HL perps tier 0, the rate every new HL account pays, so testnet traders
  and bots see HL numbers. No volume tiers, no staking discounts in v1 (cum_volume is
  already stored, so tiers can come later without a new row).
- **Maker rebate allowed but off by default.** The maker rate may be negative (a
  rebate) down to -1000 ppm; at read time it is clamped to `>= -taker`, so every fill
  nets `taker_fee + maker_fee >= 0` and the treasury never pays out more than it took on
  that fill. Recommendation: keep maker >= 0 for testnet (HL pays rebates only above a
  maker-share threshold, which we do not model).

## Where and how fees are charged

- On every **book fill**, at settlement, per fill side: taker pays
  `fee(notional, taker_ppm)`, maker pays `fee(notional, maker_ppm)`, with
  `notional = price * qty` (the same `FixedPoint` product `cum_volume` uses).
- Debited from the side's `NativeBalance.available` (quote asset), in the same balance
  update as the fill's realized PnL: one event per fill side,
  `available += closed_pnl.unwrap_or(0) - fee`, emitted when the fill has a close part
  **or** a non-zero fee (bit off: exactly today's PnL events).
- **Rounding** (deterministic, exact i128, no overflow: `n / 1e6 * ppm` plus the
  remainder term): a charge rounds **up**, a rebate rounds **down** (toward zero), so
  rounding always favours the protocol. **No dust**: the treasury is credited the exact
  sum of what every side was charged (rebates subtracted), so
  `sum(trader debits) == treasury credit` per batch; the rounding remainder is inside
  that sum, never lost.
- **Destination: the treasury's native-ledger balance** (`treasury_address`, the
  genesis `economics.treasury_address` = governance `treasury_address`), credited once
  per `execute_batch` call (sum over the call) and once per order on the single-action
  path. Not the native-token split: that split is in TORUS units and a quote-asset fee
  cannot enter it without a swap. Owner options for later: route part to the
  insurance fund / liquidator vault, or apply the burn/validator/treasury/dev ratios in
  the quote asset (credit validator / dev pool native balances).
- **Per-account totals**: a `cfee ‖ trader` row (24 bytes, `CF_NATIVE_BALANCES`, raw
  signed `FixedPoint` 16 bytes BE, same shape as `cvlm`) accumulates the net fees a
  trader paid (rebates negative). Written once per trader per batch call, only when the
  batch's sum for that trader is non-zero (bit off: never). Item 6 `Users` keeps it as
  a raw row (not `cvlm`, not 20 bytes).

## Taker (or maker) without margin for the fee

The fee is charged anyway. `available` may go below zero, exactly as a realized loss
already can; account health (and liquidation / the vault backstop) handles it. No fee
is reserved at placement in v1 (the A5 telescoping reservation stays as is) and the F1
match-time margin check does not include the fee: at <= 1 % of notional against an IM
of >= 2 % this is a small overshoot. Owner option: add the taker fee to the placement
reservation later (Phase 2 change, all three settle paths unaffected).

## Liquidation, ADL, delist, backstop

- **Stage-1 liquidation orders** go through the book (`place_order_inner`,
  `liquidation = true`) and **pay the normal fees** on both sides (HL parity: a
  liquidation fill is a market order into the book). No extra liquidation fee
  (unchanged: none by design).
- **Vault backstop takeover, ADL, delist force-close**: no book fill, **no fee**.

## Paths (all must agree byte for byte)

- Batch path, Phase 4: the sequential settle loop (canonical), the C3 parallel settle
  (pass A plans the per-side delta; pass B applies it in the sequential order),
  `TORUS_PARALLEL_ENGINE` (same settle code) and the L2.1 streamed settle (same pass B;
  its undo already rolls back the balance and volume caches, which carry the fees).
  Per-trader fee sums ride in the volume cache next to `cum_volume` (a side is counted
  for both or neither), so streamed undo / stop points need no new code.
- Rates are read once per `execute_batch` call (two `CF_FEE_CONFIG` point reads, only
  with the bit on) and handed to pass A and the streamed jobs.
- Single-action path (`place_order_inner`: triggered stops, stage-1 liquidations,
  `NativeExecutor::execute`): same per-side fee, written directly, treasury credited
  after the order's fills.
- **CoreWriter / EVM orders** enter the native batch as `PlaceOrder` actions: same
  fees, no special case.

## Parameters

Governance keys (`CF_FEE_CONFIG`, decimal strings, `ParameterChange`), validated at
proposal and at execution, read by the executor and RPC through
`torus_types::trading_fees`:

| key | default | valid range |
|---|---|---|
| `trading_taker_fee_ppm` | 450 | 0 ..= 10 000 (0 - 1 %) |
| `trading_maker_fee_ppm` | 150 | -1 000 ..= 10 000 (rebate <= 0.1 %) |

Absent or invalid stored value = the default (every validator holds the same bytes).
A failed read is a local fault (fail-stop, R02), never the default.
Genesis override: not in v1 (the genesis digest binds the file; the defaults plus a
governance vote cover testnet). If wanted, an optional `economics.trading_fees` block
seeding the two rows is additive.

## RPC

- `torus_getUserFees(trader)`: `{takerFeePpm, makerFeePpm, cumFees}` (rates in force,
  lifetime net fees from the `cfee` row).
- `userFills` stream rows get `fee` (the user's fee on that fill, signed decimal).
- Trade-history rows (`CF_NATIVE_TRADES` / `CF_NATIVE_USER_TRADES`, node-local) do not
  carry the fee in v1 (a row-version bump; follow-up).

## Tests

`crates/torus-types` unit: rounding (up / toward zero), no overflow, clamp, param parse.
`crates/torus-bridge/tests/trading_fees_tests.rs`: charge on a fill (taker, maker,
treasury, `cfee`), conservation, rebate, close + fee in one event, bit off identical to
no rate rows, activation boundary at H (blocks < H charge nothing), state identical
across serial / engine / C3 parallel / streamed, triggered-stop (single path) fee.
Governance: the two keys validate and stage to their own rows.

## Expected conflicts

`feat/funding-engine` (settlement; its `Feature::FundingEngine = 3`, item 7 T's
`SameBlockCoreWriter = 2`: fees take 4, so the three enum variants and `Feature::ALL`
merge as a textual conflict only) and `fix/reject-orders-before-first-mark` (orders on never-marked markets;
the new tests list placeholder markets like the existing differential tests and will
need marks once that lands).
