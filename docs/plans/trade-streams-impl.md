# Implementation Plan: websocket trade and userFills streams (s80)

Branch `feat/s80-trade-streams` (worktree `~/projects/wt/s80-streams`, off
main d930405). Design: `docs/plans/trade-streams.md`. Build with a
per-worktree target: `export CARGO_TARGET_DIR=~/.cargo-target-s80-streams`.

## Design Decision

Option A (owner-approved, s80), with these decisions:

- The streams are fed from execution: the exec thread wraps the block's fills
  once in an `Arc<BlockFills>`, calls an optional sink, and hands the same Arc
  to the trade writer. There is no DB read at commit.
- Messages: one per block per subscription (an array). The names stay
  `newTrades {marketId?}`, and `userFills {user}` is added. Both use the
  JSON-RPC `torus_subscribe` envelope.
- Encoding: every `torus_*` FixedPoint output (prices, sizes, PnL, balances)
  becomes a decimal string (`"123.45000000"`). `eth_*` stays hex.
- Fill content:
  - `newTrades`: tradeId, marketId, price, quantity, side (taker), maker,
    taker, blockNumber, timestamp.
  - `userFills` adds orderId, role, side from the user's view, startPosition,
    closedPnl and dir (HL's content).
- A lagging subscriber gets an explicit error and is closed.
- Streams work with `TORUS_TRADE_HISTORY=0`.
- History rows are **unchanged**. Adding the PnL fields to the rows is a later
  step, gated on a 4+4 cell A/B.
- Not building: trading fees, liquidation fills, closed PnL in rows, snapshot
  on subscribe, a routing hub, HL protocol compatibility.

## Success Criteria

1. A `newTrades` subscriber receives every fill of every executed block, in
   height order, with correct fields. That includes blocks under an exec
   backlog, so the commit-time gap is gone.
2. A `userFills {user}` subscriber receives exactly the fills where `user` is
   maker or taker. `startPosition`, `closedPnl` and `dir` must match the
   position math (`fill_transition`).
3. Both settle paths (sequential and parallel) and the single-action path
   produce identical `TradeFill` records, including the new fields.
4. With `TORUS_TRADE_HISTORY=0`, the streams still deliver, no trade rows are
   written, and state and the native root are byte-identical to history on.
5. With no sink installed, every CF and the native root are byte-identical to
   main.
6. A lagging subscriber receives an error and its subscription ends. Unknown
   kinds and bad params are rejected before accept.
7. All `torus_*` FixedPoint outputs are decimal strings. The explorer candles
   and all scripts/tests that parsed hex are updated.
8. The workspace tests pass (the known pre-existing torus-core debug_assert
   failures excepted) and clippy is clean on the touched files.
9. One full-load cell shows no large regression against the s78 cell
   (99.1k matched/s, 46.0 CPU-s/1M).

## Tasks

### Task 1: decimal strings for torus_* FixedPoint outputs

- **Test first**
  - `crates/torus-types/src/lib.rs`: tests for `FixedPoint::from_str`:
    `"123.45"` → raw 12345000000, `"-0.5"` → -50000000, `".5"`, `"100"`; and
    rejects `""`, `"1.123456789"`, `"1.2.3"` and `"0x10"`.
  - A round trip: `x.to_string().parse::<FixedPoint>() == x` for raw
    -150000000, 0 and 12345000000.
  - `crates/torus-rpc/src/lib.rs`, test `scan_trades_for_block_lists_every_market_in_order`
    (2495-2500): change the literal to `"price": "0.00000009", "quantity": "0.00000010"`.
    It must fail on the current code.
- **Implementation**
  1. torus-types:
     - Move `parse_decimal_to_fixed_point` (`tools/wallet/src/parse.rs:53`)
       into `impl std::str::FromStr for FixedPoint` (with `type Err = String`).
     - The wallet calls `s.parse::<FixedPoint>()`, keeping its existing tests.
  2. `crates/torus-rpc/src/types.rs:249`:
     - Replace `hex_fp` with `pub fn dec_fp(v: FixedPoint) -> String { v.to_string() }`.
     - Swap all call sites in `torus.rs`: `rpc_trade` 70-71, order book
       686-702/716-717, getPosition 819-825, getBalances 870-873, getMarkets
       924-925, getOpenInterest 1566-1600, getMarkPrice 1638-1640,
       getUserTrades 1704-1705, `order_to_rpc` 1861-1863.
     - Delete `hex_fp`.
  3. `crates/torus-rpc/src/lib.rs:536-542` (`scan_trades_for_block`): use
     `FixedPoint::from_raw(t.price_raw).to_string()` for price and the same for
     quantity. This also fixes the two's-complement bug on negatives.
  4. Explorer:
     - Add `torus-types = { workspace = true }` to `crates/torus-explorer/Cargo.toml`.
     - In `indexer.rs:123-124`, parse with
       `s.parse::<FixedPoint>().map(|f| f.raw()).unwrap_or(0)`.
     - Replace `parse_hex_i128`, keeping it only if other fields still use it.
  5. Tests pinned to hex:
     - The `hex_fp(x)` comparisons become `dec_fp(x)`.
     - The literals in `torus.rs` 2128-2155, `lib.rs` 2383-3451 and
       `crates/torus-rpc/tests/book_read_modes_rpc_tests.rs` 148-220.
     - `e2e_trading.rs` 327-329 and 349-350: parse with `str::parse::<FixedPoint>`.
  6. `getUserTrades` side (owner-approved s80, matches HL `userFills`): at
     `torus.rs` ~1703, `side` becomes the **user's own side**. For a taker
     entry (role 1) that is the taker side; for a maker entry (role 0) it is
     the opposite. For example: `let user_bought = (trade.taker_side == 0) == (trade.role == 1);`.
     - Test first: a maker's row reports the maker's side (`"buy"` when the
       taker sold) and a taker's row is unchanged.
     - Public feeds (`getBlockTrades`, `getTradeHistory*`, `newTrades`) keep the
       taker side.
  7. Scripts:
     - `devnet/scripts/native-transfer-probe.py:187`: parse each field;
       `evmBalance` stays hex, the others become `Decimal(v)`.
     - `testnet/bench-native-orders-grid.sh:71,168` and
       `testnet/bench-ofat-ramp-s447.sh:72,108`: match `"nativeBalance":"[0-9.-]+"`
       and check for `0.00000000`.
- **Verify**
  - `cargo test -p torus-types -p torus-rpc -p torus-explorer && cargo test -p torus-integration-tests --test e2e_trading && (cd tools/wallet && cargo test)`
  - `grep -rn "hex_fp" crates/ tools/` must return nothing.
- **Depends on:** none

### Task 2: position effect of a fill (start position, closed PnL)

- **Test first:** in `crates/torus-core/tests/position_cache_tests.rs`, add
  `apply_fill_cached_effect_reports_start_and_pnl`. One trader, market 1:
  - Buy 5 @100 → `start_size == 0`, `closed_pnl == None`.
  - Buy 3 @110 → start 5, None.
  - Sell 2 @120 → start 8, `closed_pnl == Some(32.5)`. The entry after the
    increase is (500+330)/8 = 103.75, so the PnL is (120-103.75)*2 = 32.5.
  - Sell 10 @90 → start 6, a flip: `Some(pnl)` for closing 6.
  - Buy 4 @90 → start -4, closing a short fully: `Some`.
  - Also: `apply_fill` (uncached) returns the same effect for the same sequence.
- **Implementation:** `crates/torus-core/src/position.rs`:
  ```rust
  /// What one fill did to one trader's position (s80 userFills stream).
  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
  pub struct FillEffect {
      /// Signed position size before the fill (long > 0, short < 0, none = 0).
      pub start_size: FixedPoint,
      /// Realized PnL of the fill's close component (None = no close part).
      pub closed_pnl: Option<FixedPoint>,
  }

  fn signed_size(p: &Option<Position>) -> FixedPoint {
      match p {
          Some(p) if p.is_long => p.size,
          Some(p) => FixedPoint::ZERO - p.size,
          None => FixedPoint::ZERO,
      }
  }
  ```
  - `apply_fill_cached_effect(...) -> Result<FillEffect, CoreError>` is today's
    body of `apply_fill_cached` (340), plus `let start_size = signed_size(&existing);`
    before `fill_transition`.
  - `apply_fill_cached` becomes `self.apply_fill_cached_effect(..).map(|e| e.closed_pnl)`,
    so its 7 test callers stay unchanged.
  - `apply_fill` (310) returns `Result<FillEffect, CoreError>`. Its callers use
    `?`, `.unwrap()` or `if let Err`, so they still compile. Check with
    `cargo check --tests`.
- **Verify:** `cargo test -p torus-core --test position_cache_tests`
- **Depends on:** none

### Task 3: carry order ids and per-party effects on every recorded fill

- **Test first**
  - `crates/torus-bridge/tests/trade_defer_tests.rs`: add
    `fills_carry_order_ids_and_position_effects`. Run `crossing_actions()` in
    both settle modes (`execute_batch_settle_mode(.., parallel)`), take the
    fills, then assert:
    - `maker_order_id`/`taker_order_id` are non-zero and equal to the book's
      order ids.
    - The first fill's `taker_start == 0` and `taker_pnl == 0`.
    - The sequential and parallel `Vec<TradeFill>` are **equal** (derive
      `PartialEq`, already present).
  - Add a second fixture where one trader opens and then partly closes, and
    assert `maker_pnl`/`taker_pnl` equal the `FillEffect` computed by a
    separate `PositionManager` replay.
  - `crates/torus-bridge/tests/parallel_settle_tests.rs`: extend
    `run_batches_workers` so the fingerprint includes
    `ctx.take_pending_trade_fills()` per batch. The 50x fuzz and worker-cap
    tests then compare the fills too.
  - Single-action path: extend `single_action_execute_writes_rows_inline` to
    assert the new fields.
- **Implementation**
  1. `crates/torus-state/src/trade_rows.rs:44`, `TradeFill`: add
     `maker_order_id: u128, taker_order_id: u128` and
     `maker_start_raw, taker_start_raw, maker_pnl_raw, taker_pnl_raw: i128`
     (PnL 0 when the fill has no close part). `encode_block` ignores them
     (rows unchanged), so existing row tests still pass.
  2. `native_executor.rs:5001`: change the signature to
     `persist_trade(ctx, market_id, fill, taker: FillEffect, maker: FillEffect)`
     and fill the new fields.
  3. Sequential (4134-4183):
     - `apply_fill_via_caches` (4633) returns `Result<FillEffect, CoreError>`
       via `apply_fill_cached_effect`, still crediting PnL.
     - Push the per-fill `[taker, maker]` pairs into a reusable
       `ctx.fill_effects_scratch: Vec<[FillEffect; 2]>` (cleared per order, so
       no allocation per order).
     - The persist loop zips `result.fills` with the scratch.
  4. Parallel:
     - `OrderSettlePlan` (230): add `fill_effects: Vec<[FillEffect; 2]>`.
     - Pass A (4577-4600) uses `apply_fill_cached_effect`, pushes the
       `pnl_events` exactly as today (from `closed_pnl`) and pushes each fill's
       pair.
     - Pass B (4500) zips `result.fills` with `oplan.fill_effects`.
     - The order is unchanged, and fills of failed orders are still skipped.
  5. Single-action `exec_place_order` (4952-4989): collect the effects from the
     two `apply_fill` calls, then persist with them.
- **Verify**
  - `cargo test -p torus-bridge --test trade_defer_tests --test parallel_settle_tests --test position_cache_exec_tests && cargo test -p torus-state trade_rows`
- **Depends on:** Task 2

### Task 4: record fills when trade history is off, if a stream wants them

- **Test first:** in `trade_defer_tests.rs`, extend
  `trade_index_counts_fills_in_both_settle_paths_with_history_on_and_off`:
  - Add a `record_fills` axis. With `history=false, record_fills=true`, it
    returns 3 fills.
  - With `history=false, record_fills=false`, it returns 0 fills (today).
  - Inline mode (`defer_trades=false`) with `history=false, record_fills=true`
    writes **no** rows to either trade CF.
- **Implementation:** `native_executor.rs`:
  - Add `pub record_fills: bool` (default false, doc: "s80: record fills for
    the stream sink even with trade history off").
  - In `persist_trade`, the condition becomes
    `if ctx.trade_history || ctx.record_fills`.
  - `write_trades_inline` (3161) returns early when `!ctx.trade_history`.
- **Verify:** `cargo test -p torus-bridge --test trade_defer_tests`
- **Depends on:** Task 3

### Task 5: BlockFills sink on the execution path

- **Test first:** in `crates/torus-consensus/src/app.rs` tests, next to
  `trade_history_off_writes_no_trade_rows_and_leaves_state_identical` (12257):
  - `fill_sink_receives_every_executed_block_in_order`: install a sink that
    pushes `(height, fills.len())` into an `Arc<Mutex<Vec<_>>>`, then run
    `pipeline_fixture_blocks()`. Expect `[(3, n3), (6, n6), (11, n11)]`,
    matching the fixture's fills at heights 3, 6 and 11. Run it with the
    pipeline both off and on (`pipeline_ctx(.., true/false, None)`).
  - `fill_sink_works_with_trade_history_off`: with `trade_history=false` plus a
    sink, the same blocks are delivered and both trade CFs stay empty.
  - `no_fill_sink_leaves_state_identical`: a run without a sink vs a run with
    one gives the same `dump_all_cfs` and native root.
- **Implementation**
  1. `trade_rows.rs`:
     ```rust
     /// One executed block's fills, shared by the trade writer and the stream sink.
     #[derive(Debug)]
     pub struct BlockFills { pub height: u64, pub timestamp: u64, pub fills: Vec<TradeFill> }
     /// Called on the execution thread once per executed block with fills; must not block.
     pub type FillSink = std::sync::Arc<dyn Fn(std::sync::Arc<BlockFills>) + Send + Sync>;
     ```
  2. `app.rs`, `ExecutionContext` (536):
     - Add `fill_sink: Arc<std::sync::OnceLock<FillSink>>`, created in `new`
       (3300) and cloned into `TorusApp`.
     - `pub fn set_fill_sink(&self, sink: FillSink)` sets it. Main calls it
       before consensus starts, so the exec thread gets no blocks before that.
       Boot replay inside `new()` has no sink, as intended.
     - Test constructors (`make_exec_ctx`, the literal at 9883) get
       `Default::default()`.
  3. At 1887, add `ctx.record_fills = self.fill_sink.get().is_some();`.
  4. At 2037-2038 keep the take. At 2177 replace `if !fills.is_empty()` with:
     ```rust
     if !fills.is_empty() {
         let block = Arc::new(torus_state::trade_rows::BlockFills {
             height: fills_block, timestamp: fills_ts, fills,
         });
         if let Some(sink) = self.fill_sink.get() { sink(block.clone()); }
         if self.trade_history { /* existing encode + send, closure moves `block`
                                    and calls encode_block(block.height, block.timestamp, &block.fills, ..) */ }
     }
     ```
     This sits after the flush or pipeline hand-off, so a failed hand-off at
     shutdown emits nothing.
- **Verify:** `cargo test -p torus-consensus fill_sink && cargo test -p torus-consensus trade_history && cargo test -p torus-consensus exec_pipeline`
- **Depends on:** Task 4

### Task 6: RPC notifier and the two subscriptions

- **Test first**, in `crates/torus-rpc`:
  - Pure unit tests (`lib.rs` or a new `streams.rs` module):
    - `fill_dir` in all 6 cases: open long from 0, open short from 0, increase
      long, close long partly, close long exactly, flip long → short ("Long >
      Short"), and the short mirror cases.
    - `trades_for_market(&BlockFills, Some(m))` keeps only market m, in
      trade_index order, with maker/taker set. `None` keeps all.
    - `fills_for_user(&BlockFills, user)`:
      - A fill where the user is taker has role "taker", side from the user's
        view, the taker's orderId/start/pnl.
      - A maker fill has the mirror values.
      - A self-trade (maker == taker) yields two entries, maker then taker.
      - A user absent from the block yields an empty array.
  - WS tests, in the style of the existing server tests that start
    `RpcServer`; check `lib.rs` tests for a helper, or else add one with
    `jsonrpsee::ws_client`:
    - `new_trades_stream_delivers_one_message_per_block`
    - `user_fills_stream_filters_by_address`
    - `unknown_subscription_kind_is_rejected`
    - `user_fills_requires_valid_address`
    - `lagging_subscriber_gets_error_and_is_closed`: use a notifier with a
      capacity of 2, send 5 blocks before reading, and assert that the client
      sees an error and the subscription ends.
- **Implementation**
  1. `lib.rs:113`:
     - `new_trades: broadcast::Sender<Arc<BlockFills>>` with capacity 256
       blocks.
     - `pub fn notify_fills(&self, b: Arc<BlockFills>) { let _ = self.new_trades.send(b); }`
       replaces `notify_new_trades`.
     - `BlockNotifier::with_trade_capacity(n)` exists for the lag test only
       (`#[cfg(test)]`).
  2. `types.rs`:
     - `RpcStreamTrade { #[serde(flatten)] trade: RpcTrade, maker: String, taker: String }`.
     - `RpcUserFill { trade_id, market_id, side, price, quantity, role, order_id, start_position, closed_pnl, dir, block_number, timestamp }`,
       camelCase, decimal FixedPoint via `dec_fp`, and ids and heights via the
       existing `hex_*` helpers.
  3. `torus.rs` `subscribe` (1721):
     - Parse the kind and params **before** `pending.accept()`. For an unknown
       kind, a bad `marketId` or a missing or bad `user`, call
       `pending.reject(..)` and return.
     - Then run the loop inline. jsonrpsee drives each subscription future on
       its own task.
     - `Ok(block)`: build the filtered array; if it is non-empty, send one
       `SubscriptionMessage`. On a send error, return `Ok(())`.
     - `Err(Lagged(n))`: return `Err(format!("subscriber lagged: {n} blocks dropped; resubscribe and backfill with torus_getTradeHistoryRange / torus_getUserTrades").into())`.
       jsonrpsee 0.26 sends this as the subscription's close error. **The lag
       test pins what the client sees.** If 0.26 does not send it, fall back to
       a final `{"error": ...}` notification before returning.
     - `Err(Closed)`: return `Ok(())`.
     - The `active_subscriptions` decrement moves into a small drop guard, so
       every exit path decrements.
- **Verify:** `cargo test -p torus-rpc streams && cargo test -p torus-rpc subscribe`
- **Depends on:** Tasks 1 (`dec_fp`), 5 (`BlockFills`)

### Task 7: wire the node, remove the commit-time scan

- **Test first:** `cargo build -p torus-node` fails until main.rs uses the new
  notifier API (a compile-level check). The behaviour is covered by Tasks 5 and
  6, plus the devnet check in Verification.
- **Implementation:** `crates/torus-node/src/main.rs`:
  - After `TorusApp::new` (563), and after the notifier is created (move
    `BlockNotifier::new()` above it if needed), call:
    ```rust
    let n = notifier.clone();
    app.set_fill_sink(Arc::new(move |b| n.notify_fills(b)));
    ```
  - Delete 914-915 (`scan_trades_for_block` + `notify_new_trades`) and the
    now-unused import at 27. `scan_trades_for_block` stays, because
    `torus_getBlockTrades` uses it.
- **Verify:** `cargo build -p torus-node && cargo clippy -p torus-node -p torus-rpc -p torus-consensus -p torus-bridge -p torus-core -p torus-state -- -D warnings`
- **Depends on:** Tasks 5, 6

### Task 8: API doc

- **Test first:** n/a. Doc-only; review against Tasks 6-7.
- **Implementation:** write `docs/api/streams.md` covering:
  - subscribe and unsubscribe calls, params, example messages for both kinds;
  - the delivery rules: height order; at most once; dedupe by
    `blockNumber`+`tradeId`; emitted at execution, so a fill can appear before
    `getUserTrades` returns it;
  - the lag error and how to backfill;
  - that `orderId`/`startPosition`/`closedPnl`/`dir` are stream-only for now.

  Also note the decimal switch for all `torus_*` fields, and update
  `docs/plans/trade-streams.md` with the decisions.
- **Verify:** `test -s docs/api/streams.md`
- **Depends on:** Task 7

### Task 9: full-load perf check

- **Test first:** n/a (measurement).
- **Implementation:**
  - Build a release artifact of this branch and run **one** full-load cell with
    the s78 recipe (cap 400, campaign template `run_cell.py`, s60-campaign):
    history on, no WS subscriber.
  - Score it with `score.py`: matched/s and node CPU-s/1M.
  - Compare with s78-packed-packed-r1 (99.1k, 46.0). One cell detects a
    regression above ~5% only.
  - If the rig allows, run a second cell with one all-markets `newTrades`
    subscriber attached to val0. Since s80 fix 2 val0 needs
    `TORUS_ALL_MARKET_TRADES=1` for that (validators reject all-markets
    `newTrades` by default).
- **Verify:** AGREE PASS; matched/s ≥ ~94k and CPU-s/1M ≤ ~48. Otherwise stop
  and profile before merging.
- **Depends on:** Task 7

## Verification (end-to-end)

1. The full workspace: `cargo test --workspace`. Expected: the only failures
   are the known ones: `cancel_all_many_falls_back_on_stale_or_shared_indexes`,
   `id_lookup_agrees_with_linear_scan_under_mutation`, and the flaky
   `progress_and_validator_set_update_test`.
2. Devnet:
   - Start the local devnet with this binary, attach `websocat` (or a small
     Python `websockets` client) with `torus_subscribe("newTrades")` and
     `torus_subscribe("userFills", {user})`, then drive crossing orders.
     Since s80 fix 2 a validator rejects all-markets `newTrades` unless it
     runs with `TORUS_ALL_MARKET_TRADES=1`; otherwise pass a `marketId` or
     use an `--rpc-only` node.
   - Check that every fill arrives once, in height order, and matches
     `torus_getBlockTrades` / `torus_getUserTrades` for the same heights.
3. Repeat step 2 with `TORUS_TRADE_HISTORY=0`: the streams deliver and the
   history RPCs return empty.
4. Task 9 cell within bounds.

## Rollback

- Every task is a separate commit.
- Nothing touches consensus state, and trade rows are unchanged, so a revert
  needs no DB wipe.
- Reverting Task 1 restores hex (the explorer and scripts revert with it).
- Reverting Tasks 5-7 restores the old commit-time `newTrades`.

## Owner decisions (s80)

- Fix 2 (after the Task 9 cell with one all-markets subscriber on val0:
  22-33 MB of JSON per block, up to 15 s behind execution, ~+0.5 core):
  - a block's `newTrades` array is serialized once per filter (all markets
    or one `marketId`) and shared by its subscribers (`StreamBlock` in
    `streams.rs`); `userFills` stays per subscriber;
  - all-markets `newTrades` is rejected on validators and allowed on
    `--rpc-only` nodes by default; `TORUS_ALL_MARKET_TRADES=1|0` overrides.

- `torus_getUserTrades` switches from the taker's side for both roles to the
  user's own side (Task 1, step 6), matching HL `userFills` and the new stream.
