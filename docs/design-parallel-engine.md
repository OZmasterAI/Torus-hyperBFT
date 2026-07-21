# Design — TORUS_PARALLEL_ENGINE (Layer-3 engine parallelism)

Branch `perf/l3-engine-par` (from `perf/re-proof5` @ bd80ba8). Node-local only:
all changes live in `crates/torus-bridge/src/native_executor.rs`. Nothing
consensus-visible — the parallel path is required (and tested) to be
byte-identical to today's serial semantics at every thread count.

## 1. Map of the current exec path

Live path (`torus-consensus/src/app.rs:1334-1358`): the engine timer wraps
`NativeExecutor::execute_batch(pre_evm)` + `execute_batch(post_evm)` +
`drain_core_writer` + `process_governance` + `distribute_fees` +
`process_epoch_boundary` ⇒ the ~16 ms/blk "engine" share at cap-400.

`execute_batch_inner` (`native_executor.rs`) is strictly **phase-sequenced**:

| Phase | What | Parallel today? |
|---|---|---|
| flatten | expand `PlaceOrderBatch` → flat `(sender, PlaceOrder)` list, deterministic order | serial (cheap) |
| 1 | all non-PlaceOrder actions (cancels, transfers, staking, oracle, gov) execute inline, mutating balances/books directly | serial — inherently order-dependent, cross-market shared state |
| 2 | per PlaceOrder: margin reserve (`reserve_for_qty`, balance check + debit via `BalanceCache`), global order-id assignment, partition into per-market batches | **serial** |
| 3 | matching: `MarketWorkerPool::match_parallel` — one scoped thread per market owns that market's `OrderBook`; in-market matching is order-sequential (must be) | **already parallel per market, always on** (not env-gated) |
| 4 | settlement: taker margin release, `apply_fill_cached` position transitions, realized-PnL credits, trade-row persistence (`trade_index` stamping), A5 maker/STP releases | serial by default; `TORUS_PARALLEL_SETTLE=1` enables the pass-A/pass-B parallel path **but its auto work-gate is `TORUS_PARALLEL_SETTLE_MIN_FILLS` = 1024 fills — never met at cap-400 (≤ ~400 fills), so in every gate cell settlement runs on one thread** |
| flush | `PositionCache`/`BalanceCache` flush to overlay, sorted keys | serial (cheap, deterministic bytes) |

Correction to the mission brief: the *match loop across markets* is **not**
serial — Phase 3 has been per-market threads since Task 2.5. What is serial on
the cap-400 critical path is Phase 2 (prepare) and, in practice, Phase 4
(settle, because the 1024-fill auto gate keeps the proven parallel path
disengaged at cap-400 block shapes).

Per-order work inventory: reserve arithmetic + one balance RMW (Phase 2), book
insert/match (Phase 3, parallel), and per fill: 2× position RMW + PnL credit +
3 trade-history KV builds + telescoped margin releases (Phase 4).

## 2. What TORUS_PARALLEL_ENGINE adds

`TORUS_PARALLEL_ENGINE=N` (unset/`0`/`1`/garbage = OFF = exact-today;
`N ≥ 2` = on, N is the Phase-2 worker count, capped at 32):

1. **Phase-2 parallel prepare, sender-sharded.** Group the flat PlaceOrder
   indices by sender (first-appearance order); shard the sender list across
   `min(N, senders)` scoped threads. Each worker replays *its senders'* orders
   in flat order against a worker-local `BalanceCache`: reserve computation,
   balance check/debit, pass/fail outcome (with the exact serial error
   strings). After join: merge the (sender-disjoint) worker caches into the
   batch `BalanceCache`, then a serial **stitch** walks the flat order once,
   assigning `next_global_order_id` to passing orders and building the
   per-market `PreparedOrder` batches — identical ids, identical order.
   Auto-engages at ≥ 2 senders and ≥ `TORUS_PARALLEL_ENGINE_MIN_ORDERS`
   (default 64) placed orders.
2. **Phase-4 engagement at cap-400 shapes.** The existing (proven,
   differential-tested) parallel settle engages when `N ≥ 2`, ≥ 2 markets have
   work, and total fills ≥ `TORUS_PARALLEL_ENGINE_MIN_FILLS` (default 64) —
   OR-composed with the existing `TORUS_PARALLEL_SETTLE` auto condition, whose
   default stays untouched. No change to pass-A/pass-B logic.
3. **Phase 3 untouched** (already parallel; one thread per market is fine for
   the 10-market bench genesis on 18 cores).
4. Phases 1, flush, governance/fees/epoch stay serial: they are ordered
   mutations of cross-market shared state with no per-market decomposition,
   and they are cheap.

Test entry points (mirroring the C3 idiom, immune to per-process env races):
`execute_batch_engine_mode(ctx, actions, threads)` — `threads ≤ 1` pins the
canonical serial path (serial Phase 2 + sequential settle); `threads ≥ 2` pins
sharded Phase 2 with that worker count + forced parallel settle (no gates).
`execute_batch_settle_mode` keeps its exact pre-engine behavior (engine
pinned off).

## 3. Determinism analysis (the load-bearing part)

**Requirement:** identical state root and identical event/effect byte-stream
(action results incl. error strings, gas, trade-history KV sequence,
`trade_index`, `next_global_order_id`, overlay write set) at any thread count.

**The phase-sequencing law.** Under today's serial semantics, `execute_batch`
already completes ALL of Phase 2 before ANY matching, and ALL matching before
ANY settlement effect. Therefore the match decision for order N in market A
**cannot** observe balance changes from any fill in market B — fills only take
effect in Phase 4, after every match decision in the batch is final. The book
itself has no balance access by construction (`OrderBook::place_order(params,
sender, timestamp)` — no state handle in the signature), so matching is pure
in (book state, per-market order sequence). Cross-market coupling through the
shared per-trader `NativeBalance` row exists in exactly two places: the
Phase-2 reserve sequencing and the Phase-4 credit/clamp sequencing. That is
why per-market matching was already legal, and it is the entire surface the
new work must pin down.

**Phase 2 (sender-sharding argument).** Phase 2's only mutable shared state is
each sender's `NativeBalance` (margin config is read-only; at present
`margin_configs` is never populated ⇒ uniform default 20× leverage — note the
brief's "zero margin reserved" is inaccurate: `reserve_for_qty_cfg` falls back
to 20×, so order margin IS reserved and balance-gating IS live in Phase 2).
An order's pass/fail outcome and error string are a pure function of (margin
config, params, that sender's balance trajectory), and the trajectory is a
fold over *that sender's own orders in flat order* — no other Phase-2 step
touches it. Sender-sharding preserves each fold exactly and shards are
disjoint, so outcomes are bit-identical for any shard assignment (thread
count changes only which worker computes a fold, never the fold). The stitch
then replays the global flat order serially, so order-id assignment — which
DOES depend on other senders' pass/fail outcomes (ids go only to passing
orders) — is computed exactly as today, from identical outcomes. The merged
`BalanceCache` holds the same (addr → balance, dirty) pairs; its flush is
sorted by address, so overlay bytes are order-independent anyway.
Crucially this argument does NOT rely on `margin_configs` being empty: even
with per-market margin tiers, Phase-2 cross-market coupling flows only
through the sender's own balance row, which sharding serializes per sender.
If a future change ever made *match* decisions balance-dependent (margin
check inside the book), the phase-sequencing law itself would break for the
existing always-on Phase-3 parallelism before it broke this design; the
book's state-free signature is the structural guard.

**Phase 4 (existing pass-A/pass-B idiom, unchanged).** Pass A computes
market-local plans on worker threads: position transitions land in per-market
`PositionCache`s whose key sets `(trader, market_id)` are provably disjoint
across markets; PnL amounts and margin-release amounts are pure in (config,
params, match results, post-match book) and balance-independent. Pass B
applies every cross-market mutation single-threaded in canonical order —
markets ascending, orders in prepared order, PnL credits in fill order — so
every `.min(order_margin)` clamp sees byte-identical balance state and
`trade_index` stamps the identical trade-key sequence. This is already
enforced by `parallel_settle_tests.rs` (50× rerun differential); the engine
flag only changes *when* this path engages, never what it does.

**Failure semantics.** Any Phase-2 worker panic → discard worker output
(workers mutate only worker-local state; the batch cache is merged only on
full success) and rerun the canonical serial loop. Same doctrine as the C3
settle fallback. A balance-row read error inside a worker is captured as the
per-order failure outcome exactly like serial.

## 4. Env gate

- `TORUS_PARALLEL_ENGINE` — unset/`0`/`1`/garbage ⇒ OFF, byte-exact today's
  path (serial Phase 2; settle per `TORUS_PARALLEL_SETTLE` alone). `N ≥ 2` ⇒
  on with N Phase-2 workers (capped 32). Read once per process (`OnceLock`),
  pure-parse function unit-tested like every other toggle in this file.
- `TORUS_PARALLEL_ENGINE_MIN_ORDERS` (default 64) / `TORUS_PARALLEL_ENGINE_MIN_FILLS`
  (default 64) — break-even work gates, perf-only (determinism holds at any
  size; forced test modes bypass them).
- Node-local, safe to mix across a fleet (outputs byte-identical). Composes
  with the bench-standard env; `TORUS_PARALLEL_SETTLE=1` remains
  independently meaningful (its 1024-fill auto gate for uncapped blocks).

## 5. Consensus visibility

None. No wire, genesis, voting, or state-encoding change. All observable
outputs (state root over the 6 native root CFs, node-local trade CFs, action
results, counters) are required identical; the differential matrix is the
enforcement.

## 6. Test plan (acceptance mapping)

- `engine_parallel_tests.rs`: thread-count differential matrix — same block
  sequence at threads ∈ {off, 2, 4, 8}, full-world fingerprint (CF dump incl.
  trade CFs + `compute_native_state_root` + results/gas/counters) equal,
  ≥ 20 reruns per parallel cell for schedule nondeterminism.
- Named adversarial cases: `mixed_market_*`, `single_market_degenerate_*`,
  `cross_market_same_trader_*` (incl. a sender whose balance exhausts
  MID-BATCH across markets — pass/fail sequence and order-id numbering must
  be identical), plus a full-combo cell (BookMode::LevelAuthority + resident
  books) since book mode/resident are the only bench-combo flags that touch
  this code path (root-cache/bucket-hash/member-cache act at flush time,
  after `execute_batch` returns). Round-1 finding: the combo cell's first
  failure was a test-strength bug, not a determinism gap — `trade_index` is
  per-block (fresh context per height), so the scenario-strength assertion
  must sum trades across blocks; every differential comparison passed.
- µbench: `#[ignore]`d timed test, cap-400 shape (400 orders, 10 markets,
  40 senders, seeded resting ladders, crossing flow), serial vs threads
  {2,4,8}, printed ms — run on 18c.
