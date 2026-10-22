# Perp funding engine (P0.0 feature `funding_engine`)

Status (s138): design + implementation on `feat/funding-engine` (from main `e60c326c`). Owner-approved as a
testnet MUST (s138 product-scope audit: "no funding engine yet", `native_executor.rs:15502`; `max_funding_rate_bps`
has no state slot). Paths are relative to the repo root.

## 0. Constraints
- Consensus change, so it sits behind a new P0.0 feature: `Feature::FundingEngine` (id 3, genesis name
  `funding_engine`), not scheduled anywhere (no built-in row, not in `DEFAULT_GENESIS_ACTIVATIONS`). With the bit
  off, execution, every row and the state hash are byte-identical to main (tests pin this).
- No O(positions) work per block. 1M accounts / positions and the deep-book bench numbers rule out an eager
  per-interval walk over positions. Per-block work is O(listed markets) point reads, plus one bounded book walk
  per market per premium sample.
- Same result under serial, `TORUS_PARALLEL_ENGINE` and L2.1 streamed settle, and with or without the resident
  rows / sums cache (item 6).
- Mark = oracle aggregate (`native_executor.rs:12255`; >= 3 reporters and > 2/3 of the stake, `oracle.rs`).

## 1. Model (HL-like)
**Premium sample.** HL takes a sample every 5 s. Here the sample is triggered by **block timestamp** (header seconds,
consensus-agreed), not by height. In each block, for each listed market, a sample is due when
`now / sample_secs > last_sample_ts / sample_secs` (or no sample has been taken yet). A sample:
- `oracle` = the usable mark (`usable_mark`: fresh aggregate, > 0). **No fresh mark → no sample** (the slot is
  consumed and nothing is added).
- `impact_bid` = average fill price of a market sell of `impact_notional` into the bids (walked best-first); the
  same for `impact_ask` on the asks. A side that cannot fill the notional within `IMPACT_WALK_MAX_ORDERS` (4096
  resting orders) has no impact price, and its term is 0.
- `premium = (max(impact_bid − oracle, 0) − max(oracle − impact_ask, 0)) / oracle`, clamped to ±100%.
- The book read is the in-memory `OrderBook` at block start, before any action (the previous block's
  post-state), so every node and every settle mode sees the same book.

**Funding rate (per interval).** Default interval: hourly. Settlement happens in the first block with
`now >= interval_start + interval_secs`; `interval_start` is aligned to multiples of `interval_secs`, so funding
lands on the hour. The 8-hour formula, as on HL:
`F8 = avg_premium + clamp(interest − avg_premium, −clamp, +clamp)` with interest 0.01% / 8 h and clamp 0.05%.
Per interval: `rate = F8 × interval_secs / 28800`, capped at `±max_rate_bps` (default 400 = 4% per interval, HL's
hourly cap). No samples in the interval (oracle stale the whole time) gives rate 0. An interval spanning a chain
halt is paid once.

**Cumulative index per market** (`i128`, quote per 1 unit of base, scaled 1e18 for precision on low-priced assets):
`cum_index += rate × price`. `price` is the mark at the settlement block, or the last sampled oracle price if the
mark is stale. One rounding per settlement, truncated toward zero.

**Per-position entry index, lazy settlement on touch.** Each position stores the index it last settled at
(`entry_index`). Its funding owed is `signed_size × (cum_index − entry_index)` (positive = this position pays).
The amount is rounded **up**, against the holder: a payer pays at least the exact amount and a receiver gets at
most the exact amount, so funding never creates value (the dust is lost). Eager per-interval settlement would be
O(positions) per interval and is rejected. With lazy settlement, per-block cost does not depend on the position
count.
- **On touch** (any fill of the position: trade, liquidation transfer, ADL, delist close, backstop), the owed
  amount is paid **in cash** (credited to or debited from `available`, as HL does hourly). It goes through the
  existing realized-PnL credit, so every settle path applies it the same way.
  `FillEffect::cash() = closed_pnl − funding`. `closed_pnl` (userFills `closedPnl`) stays trade-only. After the
  touch, `entry_index = cum_index` and the position's `funding_paid` (cumulative since open, for RPC) grows by the
  owed amount.
- **Not touched:** the owed amount is **pending**. It counts in unrealized PnL wherever an account is valued (margin
  checks, withdrawals, liquidation and ADL ranking, the sums cache, the getPosition precompile, RPC):
  `upnl_funded = upnl − owed`. When there is no mark, UPnL is 0 as before (s517 decision 2), and pending funding
  is 0 too. Settling moves the owed amount from UPnL to cash, so equity does not change at the touch (tested).

**Effect on margin, liquidation and UPnL.** Pending funding reduces (or increases) equity, so a position that is
never touched still drifts toward liquidation as it pays, with no per-block cost. IM and MM use notional only,
so they are unchanged. Liquidation sees the same equity through `AccountView`. The resulting transfer is a fill,
so it settles the funding in cash before the position moves.

## 2. State
- `CF_NATIVE_ORACLE` row `"fnd" ‖ market(8 BE)` (hashed, in the native root): version byte, `cum_index` i128,
  `interval_start` u64, `premium_sum` i128 (e18), `samples` u32, `last_sample_ts` u64, `last_oracle` i128 raw,
  `last_rate` i128 (e18), `last_settle_ts` u64. The row is created the first time a listed market is stepped with
  the bit on, with `cum_index = 0`. An index of 0 is also the implicit entry of every position opened before the
  row existed.
- Position row **v3** = v2 + `entry_index` i128 + `funding_paid` FixedPoint (143 bytes). It is written only when
  the market has a funding row. With the bit off, every row stays v2 (111 bytes, byte-identical). v2 and v3 rows
  both decode, and v3 rows are regular rows for the resident records (item 6 C7).
- No new CF and no per-position rows.

## 3. Execution
- Block-start step `begin_block_funding`, inside `begin_block_oracle`: after aggregation and before the mark table
  is filled. If the bit is off it returns at once with no reads. Otherwise it reads the params, then for each
  listed market reads its row, settles if due, samples if due, writes the row if changed, and builds the block's
  index table (`market → cum_index`). It runs before any action, so the whole block reads one index.
- The table is set on `ctx.positions` (`PositionManager::set_funding`), the manager every fill path uses: the
  sequential loop, pass A plans, streamed pass B, the engine, the single-action path, liquidation, ADL and delist.
  `fill_transition` takes the market's index (`None` when the bit is off) and settles the owed amount before the
  fill.
- The mark-table version (item 6 C2, which keys the sums cache) also changes when the index table changes, so a
  cached sum is never reused across a funding settlement.

## 4. Parameters (governance keys, `CF_FEE_CONFIG`, decimal; absent or invalid = default)
| key | default | range |
|---|---|---|
| `funding_interval_secs` | 3600 | 60..=86400 |
| `funding_sample_secs` | 5 | 1..=3600 |
| `funding_impact_notional` | 10000 (quote units) | 1..=10^12 |
| `funding_interest_ppm_8h` | 100 (0.01% / 8 h) | 0..=10000 |
| `funding_premium_clamp_ppm_8h` | 500 (0.05%) | 0..=100000 |
| `funding_max_rate_bps` | 400 (4% / interval) | 1..=10000 |

Governance accepts these keys only while the bit is active (checked in `exec_submit_proposal`), so with the bit
off, proposal outcomes are byte-identical too.

## 5. RPC
- `torus_getFundingRate(market)`: `active`, `lastRate`, `predictedRate` (the current interval's samples so far run
  through the rate formula), `cumIndex`, `intervalSecs`, `nextFundingTime`, `lastFundingTime`, `samples`.
- `torus_getPosition` gains `fundingSinceOpen` (`funding_paid` + pending; positive = paid). The field is omitted
  when the position has no funding state.

## 6. Owner decisions needed
1. **Cash on touch vs. folding into cost basis.** Recommended: cash (HL-like; entry price stays clean). Funding
   received becomes withdrawable at the next touch of the position, not hourly.
2. **Per-market cap.** `ListMarket` / `UpdateMarketParams.max_funding_rate_bps` still has no slot. Storing it means
   a proposal payload format change. Recommended for testnet: the chain-wide `funding_max_rate_bps` key; add a
   per-market slot later behind its own P0.0 bit.
3. **Defaults.** HL's interest, clamp and cap, a 5 s sample and an impact notional of 10000 (HL uses 20000 for
   BTC/ETH and 6000 otherwise).
4. **First interval after activation** is partial (fewer samples, full-interval rate). Alternative: pro-rate it.
5. **Feature id 3.** Id 2 is left for `TradingFees` (`feat/trading-fees`), if it takes a bit. Renumber at merge
   if not.

## 7. Tests (RED first)
- Unit (`torus-core/src/funding.rs`): rate formula, clamp and cap, premium sides, impact walk and budget, index
  math, rounding against the holder, row codec, sample and settle timing (timestamp buckets, catch-up,
  backwards time).
- Position: v2/v3 codec, settlement in `fill_transition` (open, increase, partial, full, flip), a touch is
  equity-neutral.
- Executor (`torus-bridge/tests/funding_tests.rs`): settlement on touch through every path, pending funding in
  margin and liquidation, determinism across serial / forced parallel (caps 1, 2, 8) / engine / streamed on and
  off / resident rows (byte-identical DB and native root), activation boundary at H (pre-H identical to a bit-off
  run), crash and restart mid-interval, the governance key gate, and a cost check (the step's reads and writes
  do not depend on the number of positions; it never reads `CF_NATIVE_POSITIONS`).
