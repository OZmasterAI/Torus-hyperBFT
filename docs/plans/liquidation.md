# Design: Hyperliquid-style liquidation (item 3)

**Status (s517):** **implemented** on `feat/liquidation` (d16f9ec..6d5645d + this docs commit, plan
`docs/plans/liquidation-impl.md` with its "Correction s517" notes), stacked on
`feat/oracle-aggregation` (the mark this design consumes). Not merged / pushed.
User answers (s517): D1-D11, C1 (no separate index) and C4 (CancelAll cancels stops) decided
as proposed; C3 / C5 recorded as known limitations.
Consensus-visible: lockstep upgrade + fresh genesis (new native-root CF, margin configs,
liquidation step, CancelAll also cancels stops).

**Rev. 2 (user, s517):** C1 — no separate account index: candidates = round-robin walk of
`CF_NATIVE_POSITIONS` from a cursor in `CF_NATIVE_LIQUIDATION`; C4 — user `CancelAll` also
cancels pending stops and releases their margin (own commit); D1-D11 decided as proposed; C3 /
C5 recorded as known limitations.

## Binding decisions (user, s517)

1. **Option B: full HL in ONE release.**
2. **Trigger:** account value (collateral + UPnL at mark) < maintenance margin. MM = half
   of the IM at max leverage, position-size tier, per market; computed with the F1
   `AccountView` (`margin.rs:117-196`).
3. **Stage 1 (book):** cancel the account's orders, then reduce-only MARKET orders into the
   book through the existing order path (`place_order_inner`, like `run_triggered_stops`).
   Positions above a notional threshold go in 20% chunks with a cooldown in BLOCK-TIMESTAMP
   seconds (HL: 100k USDC, 30 s). Remaining collateral stays with the trader once MM is met.
4. **Stage 2 (backstop):** account value < 2/3 MM ⇒ positions + remaining collateral move to
   the liquidator vault at the mark.
5. **ADL:** an account (or the vault) with negative value: its positions are closed against
   opposite-side counterparties ranked by (mark/entry) × (notional/account value), at the
   PREVIOUS mark. Every transfer keeps Σ long size == Σ short size (no position is ever
   deleted without a counterparty).
6. **No liquidation penalty:** the 2.5% penalty, the insurance fund and `socialize_loss` go
   (HL: no clearance fee; the backstop's retained buffer compensates the vault).
7. **Vault** = a fixed protocol account address now; designed so HLP-style deposits can be
   added later (separate branch).
8. **Candidates** (rev. 2, C1): walk `CF_NATIVE_POSITIONS` (keys `trader ‖ market`, already
   sorted by trader) from a round-robin cursor stored in `CF_NATIVE_LIQUIDATION`; 2048 account
   checks / 64 liquidation actions per block, carry-over. No separate index.
9. **Deterministic everywhere:** no HashMap iteration, exact integer math, a stale / absent
   mark ⇒ SKIP the account (never value at entry for liquidation).
10. **Placement:** after `drain_core_writer`, before `process_governance` (app.rs:2263-2264),
    on the block-start mark (`begin_block_oracle`, app.rs:2227). EVM runs before the native
    phase, so the EVM sees a block's liquidations in the next block.

## Review fixes (user decisions, s517 review)

* **H2 — no dust shield.** Replaces decision 9's "skip the account": a position whose market
  has no usable mark is valued at its ENTRY price (UPnL 0; its IM / MM still count) when the
  account is classified, and is never acted on — stage 1 / backstop / ADL use only the marked
  positions (the backstop still moves all collateral). An account with no marked position is
  not liquidated. Orders in unmarked markets are not blocked (devnet has no feeder).

* **H1 — ADL at the bankruptcy price.** The ADL close price is the previous mark (or the mark)
  CLAMPED to the account's bankruptcy price (`entry ∓ (collateral + other UPnL) / size`, other
  positions marked at the mark, unmarked at entry) on the side unfavourable to the bankrupt
  account; exact integer math, rounded against the trader (`ceil(rest × SCALE / size)` off the
  entry), so a close never leaves it positive. A non-vault account without marked positions then
  hands its remaining collateral (rounding dust, or the deficit when the previous mark was worse)
  to the vault: it ends at exactly 0; the counterparties are paid the difference. A previous-mark
  row is deleted whenever a step sees the market without a usable mark, so it is always the
  immediately preceding usable mark.

* **H3 — bounded scans.** `StateBackend::iterate_cf_from(cf, start, limit)` (StateDb: RocksDB
  seek; overlay: merge of DB / parent / pending honouring tombstones). The candidate walk seeks
  once per trader (`trader ‖ ff×8 ‖ 00` skips its rows). ADL counterparties: the 65,536-row
  window (`ADL_MAX_SCAN_ROWS`) is superseded by adl-budget Q1 (`adl-budget.md`): every trader
  of the positions CF (the slot's sorted set, else the walk) gets one point read of its position
  in the ADL market, so the HL ranking always covers every opposite-side holder.

* **M1 — no activation height.** Liquidation, the CancelAll change and the margin configs apply
  from block 1: a node with this binary needs a fresh genesis and cannot replay / sync a chain
  produced by an older build (it would diverge from the recorded roots).
* **M2 — pending row.** `0x06 ‖ trader` (value `[1]`) is written when an acted account (or the
  vault) is still under MM after its action (thin book, cooldown, bounded ADL) and deleted when it
  is healthy, flat or has no marked position; `liquidation_due` also checks it, so the step keeps
  running without other activity until the account is done.

## Hyperliquid reference (docs, as summarized in research memory a054f368)

* MM = half the initial margin at max leverage (per asset tier table).
* Liquidation when account value < MM: market orders into the book first. Positions >
  100k USDC: 20% of the position per order; after a block with such a partial liquidation a
  30 s cooldown during which every market liquidation order of the user is for the entire
  position (corrected s88; this line used to say "only the backstop can act"). In one block
  every position gets its own order (s91, from HL's public API: one block with seven 20%
  orders of one account).
* Backstop when account value < 2/3 MM: positions + collateral transferred to the liquidator
  vault (HLP) at the mark. No clearance fee.
* ADL when an account's value is negative: counterparties ranked by
  (mark/entry) × (notional/account value) (longs; entry/mark for shorts), closed at the
  previous mark price.

## What exists today (verified at `d4fe69c`)

| # | Finding | Where |
|---|---------|-------|
| F1 | `run_liquidation_checks` has **no caller**; it collects `ctx.margin_configs` (a `HashMap`) and iterates it — non-deterministic order | NE:6901-6966 (collect :6908-6913) |
| F2 | `ctx.margin_configs` is **always empty** in production (`HashMap::new()`), so even if called the loop is a no-op; every placement uses `DEFAULT_ORDER_MAX_LEVERAGE` = 20 | NE:1962; `margin.rs:56` |
| F3 | `execute_liquidation` **deletes** the position (`delete_position`) — no counterparty, Σ long ≠ Σ short afterwards | `liquidation.rs:230` |
| F4 | Cross liquidation flags ALL the account's positions, the caller executes each at the LOOPED market's price; with N configs the account is liquidated N times | `liquidation.rs:116-139`; NE:6931-6944 |
| F5 | ADL result discarded (`let _ =`); ranking = UPnL descending (not HL); ADL only shrinks winners — nothing is transferred to the liquidated side | NE:6951-6957; `liquidation.rs:257-337` (sort :271-272) |
| F6 | `total_maintenance_margin` applies ONE market's tiers to every position (vs `AccountView`'s per-market `tiers(m)`) | `margin.rs:336-353` (:352) |
| F7 | 2.5% penalty to `"insurance_fund"`, a 14-byte key in `CF_NATIVE_BALANCES` next to 20-byte trader keys | `liquidation.rs:57-60, 207-215, 433-450` |
| F8 | Governance `maintenance_margin_bps` / `max_leverage` are validated but never read; a listing's `maintenance_margin_bps` is dropped (the market row stores only `initial_margin` = 100 / max_leverage, percent) | `governance.rs:557`; NE:6596-6613; market row `governance.rs:1107-1119`, genesis `torus-genesis/src/lib.rs:424-446` |
| F9 | **New:** `OrderBook::cancel_all` returns early when the trader has no resting order — its pending STOP orders survive; when it does remove them (`retain`), their margin reservation is NOT released (stops are not in the returned `Vec<Order>`). A user `CancelAll` leaks stop reservations into `order_margin` | `order_book.rs:1337-1346, 1388-1389`; NE:5914-5970 |
| F10 | `AccountView` values a position without a mark at ENTRY (F1 decision 2) — correct for placement, wrong for liquidation (decision 9) | `margin.rs:83-86, 135-166` |
| F11 | The fatal-error check runs BEFORE `drain_core_writer`; anything after it cannot fail-stop today | app.rs:2235-2251 vs :2263 |
| F12 | Positions are written ONLY through `PositionManager::put_position` / `delete_position` (`apply_fill` :309-329 and `PositionCache::flush_all` :551-566 both call them; single serial flush NE:4100) | `position.rs:243-257` |
| F13 | Every production fill passes `MarginType::Cross` (no `Isolated` in bridge / consensus sources) | NE:5788-5810, settle paths |
| F14 | No trading fees on native fills (fees = per-action gas `total_native_fees`) | NE:1509, :6974 |
| F15 | Test fixtures list markets with an undecodable row (`b"listed"`) | `oracle_block_tests.rs:59-61`; app.rs:14381; chaos.rs |

## Design

### Definitions (all exact integer, `FixedPoint` raw i128, checked)

* `mark(m)` = `AccountReader::mark` (NE:466): the block-start aggregate while usable (time
  rule, `OraclePrice::usable`), else `None`. Only `begin_block_oracle` writes it, before any
  action, so the liquidation step at the end of the block reads the block-start mark.
* `tiers(m)` = `ctx.margin_configs[m].tiers` (now populated, see *Margin configs*), `None` ⇒
  `DEFAULT_ORDER_MAX_LEVERAGE` (20).
* `IM_pos = order_initial_margin(tiers(m), |size| × mark)` (`margin.rs:65`, position-size tier).
* `MM_pos = maintenance_margin(tiers(m), notional) = IM_pos.raw() / 2` (truncating).
  `AccountView` gains `maintenance = Σ MM_pos`.
* `AV` = `AccountView::equity()` = available + order_margin + Σ UPnL at mark.
* **Liquidation valuation** of an account = `AccountView::build` with `mark` and `tiers`,
  **only if every position's market has `Some(mark)`**; otherwise the account is SKIPPED this
  block (decision 9). An `Isolated` position or an arithmetic overflow also skips (with an
  error result) — never a panic.

### Classification (pure, `torus-core::liquidation::classify`)

| Condition (in this order) | Class |
|---|---|
| `AV >= MM` | Healthy (clears a cooldown row) |
| `AV < 0` | **ADL** |
| `3·AV < 2·MM` | **Backstop** |
| otherwise (`2/3·MM <= AV < MM`) | **Stage 1** (whole-position orders while in cooldown, s88) |

### Block step `NativeExecutor::run_liquidations(ctx)` (after `drain_core_writer`)

1. **Marks** for listed markets (`governance.listed_market_ids()`, ascending) into a `BTreeMap`;
   **previous marks** from the liquidation CF.
2. **Candidates**: distinct traders of `CF_NATIVE_POSITIONS` (ascending; consecutive keys of
   one trader deduplicated), strictly after the stored cursor, the vault excluded. Up to
   `SCAN` = 2048 accounts valued, up to `ACT` = 64 acted on; when a budget stops the pass, the
   cursor row = last scanned (carry-over); at the end of the CF it is deleted (next block
   starts from the first trader).
3. Per liquidatable account, in that order:
   1. **Cancel all its orders** — resting AND pending stops, every market ascending, releasing
      their reservations (the helper of the C4 CancelAll fix). AV is unchanged (order margin is
      collateral either way).
   2. **ADL / Backstop / Stage 1** per the class.
   3. **Flat deficit:** if the account is now flat and `available + order_margin < 0`, the
      deficit moves to the vault (`vault.available += c; acct.available -= c`) — value is
      conserved, nothing is written off.
4. **Vault:** if the vault holds positions and its AV (same valuation) < 0 ⇒ ADL the vault.
5. Write the previous-mark rows (only changed ones) and the cursor.

### Stage 1

* Positions ordered by `MM_pos` descending, ties market ascending (fewest orders to restore).
* Per position: `qty = size` if notional at mark ≤ 100,000; else `qty = size.raw() / 5`
  (whole size if that is below the book's lot). Every position of the account goes by its own
  rule in the same block (rule B, owner decision s91): a chunk does not end the account's stage
  1 for the block; the next position is ordered unless `AV >= MM`. If the block placed a chunk,
  the account's cooldown row is set to `now` (block timestamp) once, after the loop. Was rule
  A (2d03111): "after a chunk no further stage-1 order for this account this block". Evidence
  (s91, HL public API, 44 liquidated accounts, 270 orders, `orderStatus` `origSz`): in one block
  (same hash) account 0xb0fb had seven 20% orders (HYPE 1.25M, NEAR 285k, MNT 211k, ETH 206k,
  XPL 188k, LINK 155k, MON 109k notional); in its next episode MON at 87.6k went whole with six
  others at 20%; inside the cooldown all positions went whole in one block.
* Order: `PlaceOrderParams { market, is_buy: !is_long, price: cap, quantity: qty,
  order_type: Market, time_in_force: IOC, reduce_only: true, client_order_id: None }` through
  `place_order_inner(ctx, trader, &p, None, &mut queue)`, then `run_triggered_stops(ctx, queue)`
  (exactly `exec_place_order`, NE:5496-5505). Reduce-only ⇒ no margin gate (NE:5198-5201,
  :5636); fills are ordinary book fills (maker checks, STP, trade rows, stops).
* After each order re-value; stop as soon as `AV >= MM` (the rest of the collateral and the
  remaining positions stay with the trader).
* Cooldown (per account): while `now − last_chunk_ts < 30`, every stage-1 order of the account
  is for the ENTIRE position (no chunk; it does not write the cooldown row, so only a chunk
  starts / restarts a cooldown); backstop and ADL still apply. HL parity fix s88 — this line
  used to say "stage 1 is skipped" (a misreading of HL: "During this cooldown period, all
  market liquidation orders for that user will be for the entire position").

### Stage 2 — backstop

For each position (ascending market): `apply_fill(trader, m, !is_long, size, mark)` and
`apply_fill(VAULT, m, is_long, size, mark)` — a fill between the account and the vault at the
mark (the vault nets with any position it already has; `fill_transition` handles
open / increase / reduce / flip). Then `c = available + order_margin` moves to the vault.
The account ends flat with `available + order_margin == 0`.

### ADL

For each position of the underwater account `U` (ascending market):

* price `p` = previous-mark row of `m`, else the current mark (first step ever).
* counterparties = every opposite-side position in `m` (scan of `CF_NATIVE_POSITIONS`,
  key order), `U` excluded; ranked by `k = (px_num × notional) / (px_den × AV)` descending
  (`px_num/px_den` = mark/entry for a long counterparty, entry/mark for a short; notional at
  the mark; AV by `AccountReader::view`, entry fallback allowed for RANKING only); exact
  comparison by U512 cross-multiplication; counterparties with `AV <= 0` rank last; ties by
  address ascending.
* close `q = min(remaining, cp.size)` pairwise: `apply_fill(U, m, !U.is_long, q, p)`,
  `apply_fill(cp, m, U.is_long, q, p)`. Σ opposite size ≥ `U.size` always (OI symmetry), so the
  position is fully closed.
* then the flat-deficit rule.

### Invariants (tested)

* **OI symmetry:** for every market, Σ long size == Σ short size after every step.
* **Value conservation:** Σ over all accounts (available + order_margin + UPnL at mark) is
  unchanged by the step (any fill at any price conserves it; collateral moves are
  transfers). Tests use exactly representable prices / sizes.
* Liquidation never runs on an account with a stale / absent mark in any of its markets.

### State: new native-root CF `CF_NATIVE_LIQUIDATION` (tag 6)

| Key | Value | Meaning |
|---|---|---|
| `0x01` | — | unused / reserved (no account index, C1) |
| `0x02 ‖ trader(20)` | u64 BE | last stage-1 chunk timestamp (cooldown) |
| `0x03 ‖ market(8)` | i128 BE raw | previous mark (ADL price) |
| `0x04` | trader(20) | scan cursor (only while a pass was cut) |
| `0x05 ‖ …` | — | reserved: vault deposits / shares (later branch) |
| `0x06 ‖ trader(20)` | `[1]` | still under MM after its last action — keeps the step due (review M2) |

Appended as tag 6 to `NATIVE_ROOT_CFS` (tags 0-5 frozen and unchanged) and to
`compute_native_state_root`. An empty CF contributes nothing to either root; rows exist only
once the step has run (ordinary trading writes none).
The insurance-fund key in `CF_NATIVE_BALANCES` is removed (F7).

### Vault

`LIQUIDATOR_VAULT: Address = Address::new(*b"torus-liquidator-vlt")` (20 ASCII bytes, no
known key ⇒ no signed action can come from it). It is an ordinary account (balance row +
position rows), so RPC / precompile readers see it unchanged. Exempt from stage 1
and backstop; ADL when its AV < 0. Deposits later: share rows under `0x05` plus deposit /
withdraw actions; nothing in this design depends on the vault having no depositors.

Monitoring (s17, `fix/liq-oracle-metrics`): a vault with negative cash and no positions is
never acted on (ADL needs positions), so its deficit is exported. Gauge
`torus_liquidator_vault_deficit` (negative cash in tokens, 0 otherwise; set after each
liquidation pass, 0 after a restart until the next pass) and RPC `torus_getLiquidatorVault`
(`address`, signed `availableBalance`, `deficit`, `openPositions`; reads committed state, see
`docs/api/liquidator-vault.md`).

Step telemetry (`feat/liq-telemetry`, node-local: no state writes, metrics only when attached,
block results identical): histogram `torus_liquidation_step_seconds` (one sample per step);
counters `torus_liquidations_{stage1,backstop,adl}_total` (accounts acted on per class; ADL
includes the vault's own), `torus_liquidation_scanned_total`, `torus_liquidation_acted_total`;
gauges `torus_liquidation_pending` = pending rows ∪ the scan-window candidates the act budget
left unclassified (an upper bound; counting exactly would mean classifying up to ~2,000 more
accounts on every budget-cut block) and `torus_liquidation_deferred` (that second part alone).
The pending rows are re-counted only on a step that changed one (or the first step after a
start); `adl` counts ADL runs (an ADL'd account, or the vault, also when nothing closes).
Logs: one info line per step that acted (height, scanned, acted, per-class counts, deferred,
pending, ms), debug otherwise; one info line per ADL'd (account, market) (`liquidation: ADL`:
counterparty count, total size, price), each counterparty close at debug. Metric rows:
`docs/monitoring-setup.md`.

### Margin configs (F2, F8)

`NativeExecContext` constructors load `margin_configs` from `CF_NATIVE_MARKETS` rows
(borsh: base, quote, lot, tick, initial_margin percent): `max_leverage = max(1,
floor(100 × SCALE / initial_margin.raw))`, `tiers = [MarginTier { max_notional: MAX,
max_leverage }]` (flat — identical to today's IM for every 20x market), `maintenance_factor_bps`
5000. Undecodable rows (test fixtures) and `initial_margin <= 0` ⇒ no config (default 20x).
Read errors ⇒ `fatal_error`. A listing in block h applies from block h+1.

### Native phase gate

`run_native` (app.rs:1983) gains `liquidation_due(&overlay)`: a cooldown (`0x02`) or cursor
(`0x04`) row exists. Without it a pending chunk (30 s) or a cut pass would wait for unrelated
activity. Marks only change in native-phase blocks (oracle C1) and positions / balances only
change through native actions or CoreWriter, so no other trigger is needed.

## Defaults — decided (user, s517)

| # | Item | Decided (user, s517) | Why |
|---|------|------------------|-----|
| D1 | Slippage cap of liquidation orders | `mark ∓ mark / (2·lev)` — the position's MM rate (2.5% at 20x), per market | Bounds a stage-1 fill's loss beyond the mark to one MM_pos: from `AV ≥ 2/3 MM` the worst case is `AV ≥ −1/3 MM` (then ADL). A fixed 5% doubles that tail for every market; a thin book simply leaves the rest for the next block / the backstop. |
| D2 | Chunk basis | notional at the MARK > 100,000 (USDC units) ⇒ 20% of the SIZE (`raw / 5`) | HL; mark = the same price the trigger used. |
| D3 | Cooldown storage | per account, `0x02 ‖ trader` → block timestamp (s); 30 s | HL is per user; seconds of header time like the oracle. |
| D4 | Cancel all resting orders + pending stops before stage 1 | **yes** (all markets) | Releases order margin, prevents STP against the liquidation order and stale stops re-opening risk; HL undocumented. |
| D5 | Budgets + carry-over | `SCAN = 2048` accounts valued, `ACT = 64` accounts acted on per block; round-robin cursor | Bounds per-block work at ~65 ms blocks; 100k accounts ⇒ a full sweep in ~49 blocks (~3 s), well inside the 10 s / 30 s windows. |
| D6 | Isolated margin | not supported: account with an Isolated position is skipped (error result) | F13: no production path creates one. |
| D7 | Stops triggered by liquidation fills | run them (`run_triggered_stops`, FIFO) right after each liquidation order | Same as any fill; bounded by the pending set. |
| D8 | Vault's own margin | exempt from stage 1 / backstop; ADL when its AV < 0 | It is the backstop; nothing else can take its positions. |
| D9 | Residual deficit of a flat account (after stage 1 / ADL) | moves to the vault balance | Conserves value; no write-off, no socialization (decision 6). |
| D10 | Previous mark | the mark the previous liquidation step stored per market; current mark if none | Deterministic, no oracle row-layout change. |
| D11 | Margin configs | flat single tier at the listing's max leverage; MM fixed at half of IM | Zero IM change for current 20x markets; HL's "half". |

## Decisions (user, s517), known limitations, flags

* **C1 — decided: no separate index.** `CF_NATIVE_POSITIONS` (key `trader(20) ‖ market(8)`) is
  already sorted by trader; the step walks it from the cursor. No extra write per position, no
  index/positions consistency invariant. Cost: one positions-CF walk per native block (R1 in
  the plan).
* **C4 — decided: fix user `CancelAll`** (F9): it also cancels pending stops and releases their
  reservation — its own commit, failing test first; the liquidation step reuses the helper.
  Client-visible.
* **C3 — known limitation:** a listing's `maintenance_margin_bps` is ignored; MM = ½ IM at max
  leverage (HL). The market row has no slot for it (F8).
* **C5 — known limitation:** the vault cannot unwind (no orders, no strategy) until the
  deposits / HLP branch; it only shrinks through ADL. Its balance can go negative (D9); when its
  AV < 0 it is ADL'd.
* **C6 — `CF_NATIVE_MARKETS` is off-root** but now drives margin (oracle R6, same exposure as
  governance ids).
* **C7 — ranking AV uses entry fallback** for counterparties' unmarked markets (ranking only;
  the execution price is the previous mark of the ADL market).
* **C8 — no liquidation flag in trade rows** (fills look like normal trades in history).
* **C9 — governance `maintenance_margin_bps` / `max_leverage` params** stay validated-but-unread
  (`UpdateMarketParams` is text-only); out of scope.

## Out of scope

HLP deposits / vault strategy; mark = HL formula (item C); funding (when added: the ADL escrow
positions are excluded, `docs/plans/adl-budget.md` §8); isolated margin; liquidation fees; RPC
liquidation endpoints.
