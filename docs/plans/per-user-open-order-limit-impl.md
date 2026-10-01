# Implementation Plan: Per-User Open-Order Limit (Hyperliquid model)

Status: APPROVED by owner (s83, 2026-10-01): copy HL's reduce-only/trigger rule (Task 6 in); ship with the coordinated testnet upgrade. Code facts from a read-only
map of `main` @ 7d55675; line numbers are as of that commit.

## Design Decision

Replace the per-market cap `MAX_ORDERS_PER_TRADER_PER_MARKET = 200`
(`crates/torus-core/src/order_book.rs:28`, checked at `:410`) with one
**per-user cap across all markets**, as Hyperliquid documents it
("default open order limit of 1000 plus one additional order for every 5M USDC
of volume, capped at a total of 5000"):

    limit(user) = min(1000 + floor(cum_volume(user) / 5,000,000), 5000)

- `cum_volume` = the user's **lifetime** traded notional (`price * qty`, quote
  units), counting **both** maker and taker sides. HL does not say lifetime vs
  rolling for this rule; lifetime matches its other volume gates (`cumVlm`).
- **Counted toward the limit:** resting orders (Limit with GTC / PostOnly) and
  pending stop orders (StopMarket / StopLimit waiting for trigger).
- **Never counted, never blocked:** Market, IOC, FOK (they cannot rest), so a
  user at the limit can always reduce or close a position.
- **No per-market limit.**
- Enforced in the Phase-2 per-sender fold (the only deterministic place: a
  sender's orders are processed in flat order there, and markets match in
  parallel later). Count = open orders at block start (after Phase-1 cancels)
  + this block's already-accepted restable orders. Conservative within a
  block: a GTC order that fully fills still uses its slot until the next block.
- The limit uses `cum_volume` as of block start; this block's fills raise it
  for the next block.
- Open-order count is **derived from the books** (no new committed state).
  `cum_volume` is **new committed state**: one row per user in
  `CF_NATIVE_BALANCES` under key `b"cvlm" ‖ address` (24 bytes, value =
  16-byte BE raw `FixedPoint`), following the `INSURANCE_FUND_KEY` precedent
  (`crates/torus-core/src/liquidation.rs:59`, `:433-448`). It is covered by the
  native root automatically and leaves the `NativeBalance` schema (v1) alone.
- Fee tiers (next-list item 6) will need rolling 14-day daily buckets; they get
  their own keys later. Not built here (YAGNI).

### Decisions (owner, s83)

1. **HL's reduce-only/trigger rule**: HL rejects reduce-only and trigger orders
   once the user has >= 1000 other open orders. With a volume-scaled limit
   (up to 5000) this matters. DECIDED: copy it (Task 6).
2. **Activation**: this changes which orders are accepted (consensus rule), so
   all validators must switch at the same height. There is no activation-height
   mechanism today; DECIDED: ship with the coordinated testnet upgrade (together
   with the one-time trade-history wipe).
3. **`torus_getOpenOrders` cap** is 500 (`crates/torus-rpc/src/torus.rs:735`),
   so a user at 5000 cannot list all orders. Plan raises it to 5000 (Task 10).

## Success Criteria

1. A user can hold up to `limit(user)` open orders summed over all markets; the
   next restable order is rejected with a clear message and its own funnel
   counter `orders_rejected_open_limit`.
2. Market/IOC/FOK orders are never rejected by the limit.
3. Pending stops count toward the limit; `CancelAllOrders` frees them.
4. `cum_volume` increases by `price*qty` for maker and taker on every fill;
   limit grows by 1 per 5M, capped at 5000.
5. Serial and sharded Phase-2 prepare, and sequential and parallel settle,
   produce byte-identical state (existing `engine_parallel_tests` pattern).
6. No per-market cap remains; all tests that encoded 200/market are updated.
7. `torus_getUserLimits(address)` returns `{openOrders, openOrderLimit, cumVolume}`.
8. Bench: standard cells run with < 1% open-limit rejects after the generator
   respects the limit; a new 10/100/300 baseline is recorded (old cells are not
   comparable).
9. `cargo test` for torus-core, torus-bridge, torus-rpc, torus-consensus pass
   except the known pre-existing failures (torus-core
   `cancel_all_many_falls_back_on_stale_or_shared_indexes`,
   `id_lookup_agrees_with_linear_scan_under_mutation`; flaky
   `progress_and_validator_set_update_test`).

## Tasks

### Task 0: Audit full scans of `CF_NATIVE_BALANCES`
- **Test first**: none (audit). Output: list of every iterator over
  `CF_NATIVE_BALANCES` that decodes values as `NativeBalance` or assumes
  20-byte keys (state root, digest, genesis export, RPC listings, snapshot).
- **Implementation**: for each, confirm the 24-byte `cvlm` keys are skipped or
  harmless (the 14-byte `insurance_fund` key already lives there, so scans that
  survive it likely survive this). Fix any scan that would mis-decode.
- **Verify**: `rg -n "CF_NATIVE_BALANCES" crates --type rust` reviewed; findings
  recorded in this doc.
- **Depends on**: —

### Task 1: `OrderBook::open_order_count(trader)`
- **Test first** (`crates/torus-core/src/order_book.rs` tests): place 3 GTC
  resting + 2 StopMarket pending for trader A, 1 IOC that cancels → count == 5;
  cancel one resting → 4; trigger a stop that then rests → still 4.
- **Implementation**: `pub fn open_order_count(&self, trader: &Address) -> usize`
  = `self.trader_orders.get(trader).map_or(0, Vec::len)` + pending stops whose
  trader matches (scan of `pending_stops`, `:251`; stops are rare).
- **Verify**: `cargo test -p torus-core open_order_count`
- **Depends on**: —

### Task 2: `CancelAllOrders` also cancels pending stops
- **Test first**: trader with 0 resting orders and 2 pending stops sends
  `cancel_all` → both stops gone, count == 0. (Today `cancel_all` returns early
  when the trader has no resting orders, `:645-648`, so stops survive — with
  stops counted, that would strand slots.)
- **Implementation**: in `cancel_all` (and the batch path `cancel_batch.rs:451`
  if it has the same early return), remove the trader's pending stops before
  the early return; keep return/order semantics for resting orders unchanged.
- **Verify**: `cargo test -p torus-core cancel_all`
- **Depends on**: 1

### Task 3: Remove the per-market cap
- **Test first**: replace `reject_excess_orders_per_trader` (`:3911`) with a
  test that 201+ orders from one trader in one market all rest.
- **Implementation**: delete the check at `:409-418` and the constant (`:28`);
  `cancel_batch.rs:200` eligibility upper bound → `OPEN_ORDER_MAX_LIMIT`
  (5000, exported from torus-core). Update fixtures that assumed 200/market:
  `many_tests.rs:116,220,470-484`, `queue_lookup_tests::trader` (`:4566`),
  `torus-rpc/src/lib.rs:3403-3420` (`get_open_orders_limit_500`), benches
  `exec_place_batch.rs:6`, `load_books_ubench.rs:27-29`,
  `l3_savebooks_ubench.rs:225,291`.
- **Verify**: `cargo test -p torus-core && cargo test -p torus-rpc && cargo bench --no-run -p torus-bridge`
- **Depends on**: 4-5 must land in the same commit series before any release
  build (Task 3 alone would leave no cap at all).

### Task 4: `cum_volume` row + limit function (torus-core)
- **Test first**: `open_order_limit(0) == 1000`, `(4_999_999) == 1000`,
  `(5_000_000) == 1001`, `(20_000_000_000) == 5000`; read/write round-trip of
  the `cvlm` row; missing row reads as zero.
- **Implementation**: constants `OPEN_ORDER_BASE_LIMIT = 1000`,
  `OPEN_ORDER_VOLUME_STEP = 5_000_000` (quote units), `OPEN_ORDER_MAX_LIMIT = 5000`;
  `fn open_order_limit(cum_volume: FixedPoint) -> u32`; key helper
  `cum_volume_key(addr) -> [u8; 24]` (`b"cvlm" ‖ addr`); read/write helpers
  next to the insurance-fund ones (`liquidation.rs:433-448`), raw 16-byte BE.
- **Verify**: `cargo test -p torus-core open_order_limit cum_volume`
- **Depends on**: 0

### Task 5: Enforce the limit in Phase-2 prepare (serial + sharded)
- **Test first** (`crates/torus-bridge/src/engine_parallel_tests.rs`, using
  `make_ctx`/`fund_native`/`gtc`/`place`/`run_batches`):
  a. 1000 GTC resting orders spread over 5 markets accepted; the 1001st (any
     market) rejected with "open order limit"; `orders_rejected_open_limit` == 1.
  b. At the limit, an IOC and a Market order are still accepted.
  c. A sender with 999 open sends a batch of 3 GTC → first passes, next two rejected.
  d. Same blocks through serial and sharded prepare → identical `state_dump`
     and identical results (force both via `EngineMode::Force`).
  e. User with `cum_volume` 5M at block start gets 1001.
- **Implementation** (`crates/torus-bridge/src/native_executor.rs`):
  - Before Phase 2 (after Phase 1, `:3620`), build
    `budget: HashMap<Address, u32>` for every sender with >= 1 restable order:
    `open_order_limit(read cum_volume) - Σ books.open_order_count(sender)`
    (saturating). Senders x markets lookups; measure (Task 9).
  - One shared helper used by both loops (2 call sites):
    `fn take_open_slot(budget: &mut u32, p: &PlaceOrderParams) -> Result<(), String>`
    — returns Ok without decrementing for Market/IOC/FOK; otherwise Err if
    `*budget == 0`, else decrement.
  - Serial loop: call it at the top of the per-order body (`:3751`), before the
    margin reserve, so a rejected order reserves nothing.
  - Sharded: pass `&budget` into `phase2_parallel_prepare` (`:3978`); each
    worker copies its senders' budgets (shards are sender-disjoint) and calls
    the helper at `:4001`.
  - `PrepOutcome::Reject { margin: bool, .. }` (`:413-421`) → a reason enum
    `{ Margin, OpenLimit, Other }`; stitch (`:3731-3741`) and serial loop bump
    `orders_rejected_margin` / new `orders_rejected_open_limit` /
    `orders_rejected_other`. Register the counter in
    `crates/torus-telemetry/src/lib.rs` next to `orders_rejected_book` (`:871`).
- **Verify**: `cargo test -p torus-bridge engine_parallel open_limit`
- **Depends on**: 1, 4

### Task 6: reduce-only / trigger rule (decided: copy HL)
- **Test first**: user with 1000 open orders and limit 1200: a reduce-only GTC
  and a StopMarket are rejected; a plain GTC is accepted.
- **Implementation**: in `take_open_slot`, if `open_now >= 1000` and the order is
  reduce-only or a stop → Err. Needs `open_now` alongside `budget` (store
  `(open_now, limit)` instead of a single budget).
- **Verify**: `cargo test -p torus-bridge open_limit_reduce_only`
- **Depends on**: 5

### Task 7: Accumulate `cum_volume` on every fill (both sides)
- **Test first**: one fill of qty 2 @ 30000 → maker and taker `cum_volume`
  each += 60000; sequential and parallel settle → identical state (including
  `cvlm` rows); a mid-order fill failure stops volume at the same fill as the
  position effects.
- **Implementation**: a per-block `HashMap<Address, FixedPoint>` volume cache
  flushed with the balance/position caches (`:3945-3950`).
  - Sequential settle (`:4081-4274`): add after each side's
    `apply_fill_via_caches` succeeds (`:4178-4223`).
  - Parallel: add volume events to `OrderSettlePlan` like `pnl_events`
    (`:236`), apply in pass B (`:4480-4592`) in the same order and with the
    same stop-on-failure point.
  - Cost note: one extra committed row per trading user per block (per user,
    not per market).
- **Verify**: `cargo test -p torus-bridge cum_volume settle`
- **Depends on**: 4

### Task 8: RPC `torus_getUserLimits`
- **Test first** (`crates/torus-rpc` tests): returns
  `{openOrders, openOrderLimit, cumVolume}` (decimal strings like other s80
  fixed-point outputs) for a user with resting orders and volume.
- **Implementation**: new method next to `torus_getBalances` (`torus.rs:110`,
  `:831-876`); does not change `getBalances` (keeps the bench state digest and
  existing clients unchanged). Document in `docs/api/`.
- **Verify**: `cargo test -p torus-rpc get_user_limits`
- **Depends on**: 1, 4

### Task 9: Bench generator respects the limit
- **Test first** (`tools/bench-throughput` tests): with econ shape, a sender's
  estimated open orders never exceed a threshold (900) — when the next batch
  would cross it, the sender sends `CancelAllOrders` instead; uniform plan with
  threshold disabled stays byte-identical to today (existing frozen test).
- **Implementation**: per-sender estimate = passive orders placed since the
  last cancel-all (upper bound: ignores fills); force a cancel-all when
  `estimate + batch_passive > threshold`. Flag `--open-order-budget N`
  (0 = off) and runner env `OPEN_ORDER_BUDGET` in `run-cell.sh` (pattern of
  `CANCEL_FRACTION`, commit 7d55675).
- **Verify**: `cargo test -p bench-throughput && python3 -m pytest -q tools/matched-bench/test_harness.py`
- **Depends on**: —

### Task 10: `torus_getOpenOrders` cap 500 -> 5000
- **Test first**: a user with 600 resting orders gets all 600.
- **Implementation**: `OPEN_ORDERS_LIMIT` (`torus.rs:735`) = `OPEN_ORDER_MAX_LIMIT`.
- **Verify**: `cargo test -p torus-rpc open_orders`
- **Depends on**: 3

### Task 11: Bench baseline + cost check
- Cells (same recipe as s83: cap 400, 300 s, rate 76000, `OPEN_ORDER_BUDGET=900`):
  10 / 100 / 300 markets uniform, plus 100 / 300 with MPS=10. Check
  `orders_rejected_open_limit` < 1% and the Phase-2 budget build time per
  block (add a timer if Task 5's lookup cost is visible: senders x markets
  lookups at 300 markets is ~1.5M per block).
- **Verify**: summary.json funnel + phase timings; AGREE on all cells.
- **Depends on**: 1-9

## Verification (end-to-end)

1. `CARGO_TARGET_DIR=$HOME/.cargo-target-<branch> cargo test -p torus-core -p torus-bridge -p torus-rpc -p torus-consensus`
   (only the known pre-existing failures may remain).
2. Devnet: a script places 1000 GTC orders across markets from one funded
   account → 1001st rejected via RPC error; IOC still accepted;
   `torus_getUserLimits` shows 1000 / 1000.
3. Task 11 cells all AGREE with < 1% open-limit rejects.

## Rollback

Code-only revert restores the 200/market cap. `cvlm` rows written in the
meantime are part of the native root, so a rollback after activation needs a
coordinated revert on all validators; the rows are then inert (no reader).
