# Deferred trade keys: qualified default-off candidate

Branch `perf/deferred-trade-fixed-keys` starts at `f2a7164`. This candidate removes
the three key-vector allocations made for each deferred fill. It retains the
existing `[u8;20]` primary key and two `[u8;32]` user keys until RocksDB copies their
borrowed slices into its write batch. Value vectors are moved, not cloned. It
does not change matching, balance settlement, counters, trade IDs or persistence
policy. No speed improvement or new durability guarantee has been established.

## Evidence and experiment

Implementation issue `d5946f27-0982-4939-a8a7-02abc4b5aa79` links MAIN finding
`744580a4`. Root's accepted pass-B diagnostic run reported 271 invocations per
node: trade stamping/routing occupied 48.9–50.5% of pass B, and balance application
15.4–18.5%. Those are pass-B shares, not total-engine shares. Attribution was
measured on diagnostic `c43909f`; this candidate's `f2a7164` base also contains the
separately qualified cancellation change. Use this candidate's same binary OFF/ON
for a causal comparison, not a comparison of the two source bases.

Only exact `TORUS_DEFERRED_TRADE_FIXED_KEYS=1` enables fixed keys. Unset, `0`, and
every other value select the existing raw representation. The process caches the
flag, and each context fixes its buffer representation at construction. The flag
has no effect on the inline trade route. Root should record its effective value
with the cell environment and keep every other workload/runtime setting fixed.

The source operation reduction is three key `Vec` allocations per deferred fill;
the three ordered row pushes and all value allocations remain. Row-vector growth,
enum layout, RocksDB copying and writer scheduling can offset savings. This is not
merely moving those key allocations onto the writer: its typed path never converts
them to vectors. No allocation or timing measurement has yet been performed on the
candidate. Judge total engine/block time, accepted fills, writer queue, flush
latency and final drain as well as any routing attribution.

## Representation and compatibility

`DeferredTradeRow` is either `Primary { key:[u8;20], value:Vec<u8> }` or
`User { key:[u8;32], value:Vec<u8> }`. Its variant supplies the original non-root CF
name. `DeferredTradeBatch` owns either the old raw-row vector or a typed-row vector.
One context has one variant; it cannot collect raw and typed rows in separate
buffers and accidentally concatenate them in the wrong order.

Both drain methods empty the same buffer and retain its variant for the next
batch. Existing `take_pending_trades() -> Vec<RawCfKv>` remains a compatibility
conversion for callers that require raw rows. Production uses
`take_pending_trade_batch()` and avoids conversion. Existing public `defer_trades`
still selects deferred versus inline writes, independently of key representation.
Toggling that field does not change the pending buffer's mode or order.

`BackgroundCfWriter::send(Vec<RawCfKv>)` retains its signature, successful raw
path and returned-vector failure contract. New `send_batch` accepts either variant.
The writer channel remains one bounded FIFO of batches; the application still uses
capacity 256. No second worker, extra queue or wire format is introduced. Other
raw-CF users retain their existing write loop and API.

## Ordering, errors and durability boundaries

* Definitive index stamping remains in canonical market/order/fill order, before
  routing. Primary, maker and taker rows are appended in that order, followed by
  the original `trade_index` increment and matched-counter update.
* The inline route and all balance clamps, zero-PnL materialization, failed-order
  continuation and pass-A fallback decisions are unchanged. No counter guards or
  counter aggregation are included.
* Writer chunk size remains a **row** count. Default 2,048 can split a fill's
  three rows, and `0` still writes the whole batch. Low-priority options are
  unchanged. A failed chunk stops that batch; earlier groups remain durable, and
  the writer retains its existing log/decrement/continue-next-batch behavior.
* Disconnection returns the same owned batch. The application falls back to
  synchronous borrowed row writes in the same order, retaining its existing
  per-row error handling. Queue accounting and close/drain/join remain unchanged.
* Application dispatch remains after the serial flush **attempt** (which already
  logs failures and continues), or after successful pipeline handoff. No new flush
  success gate or commit barrier has been added. Typed rows are not flushed early.
* Trade history is node-local and outside consensus roots. Hard crashes can lose
  queued rows or a chunked tail; replay of an unapplied block rewrites deterministic
  keys idempotently. An already applied block need not be replayed to repair a
  cosmetic history gap. The pipeline's broader crash qualification remains separate.

Removing allocations can change where allocation failure occurs. This candidate
adds no panic containment/recovery or permission to commit a partially executed
block. It preserves ordinary error/control-flow boundaries but does not claim the
same transient in-memory row prefix under an allocation panic. Ordered three-row
pushes remain; no whole-fill or whole-market atomicity is introduced.

## Authored checks, not executed by the implementation agent

Five bridge fixtures use explicit per-context modes without global-env mutation:
exact flag parsing; full serial/parallel, inline/deferred row-byte/results/gas/ID
parity including cold zero-PnL materialization; injected balance-read failure and
no failed-order trades; position-error fallback; and ordered accumulation across
compatibility/typed drains with retained mode. Bad-balance-read comparisons are
OFF/ON within each settlement algorithm, since the existing algorithms already
differ in position application at that error boundary.

Five state fixtures cover raw/typed complete rows and actual RocksDB write-group
counts for chunks 1/2/7/2,048/0, duplicate-key overwrite order, disconnected batch
ownership and synchronous fallback, mixed raw/typed FIFO batches with shutdown
drain and reopen, stop-at-first-error chunk-driver behavior, and a read-only DB
write failure. The injected chunk-driver failure tests control flow; it is not a
power-loss or mid-write durability test. All DB fixtures create fresh temporary
directories. No existing campaign DB is modified.

Root controls execution and receipts. Suggested focused commands:

```sh
cargo test --release -p torus-bridge --lib deferred_trade_fixed_keys_tests
cargo test --release -p torus-state --lib fixed_trade_key_tests
cargo test --release -p torus-state --lib bg_writer
cargo test --release -p torus-bridge --test trade_defer_tests --test parallel_settle_tests --test engine_parallel_tests
TORUS_DEFERRED_TRADE_FIXED_KEYS=1 cargo test --release -p torus-bridge --test trade_defer_tests --test parallel_settle_tests --test engine_parallel_tests
TORUS_DEFERRED_TRADE_FIXED_KEYS=1 cargo test --release -p torus-consensus --lib deferred_trades_reach_db_via_background_writer
cargo build --release -p torus-node
```

Run OFF commands with the flag absent or explicitly `0`. Existing integration
coverage plus the typed writer drain/reopen fixtures do not prove abrupt-crash
behavior for this representation. No process-kill commands or fixtures were run
or added; any additional crash/replay check is a separate root qualification gate.
No default change, merge, promotion or live performance claim is implied.

## Root qualification

Receipt `fc92801b-387d-4fee-8b68-e98d679b0a59` passed on unchanged source after
cleaning seven shared-target packages, including `torus-state`. All five focused
bridge tests, all 12 background-writer tests (including five new fixtures),
19 bridge integration tests with the flag OFF and the same 19 ON, and the
consensus deferred-writer integration ON passed: 56 executions, 37 unique tests.
The separate node-only release build also passed. Independent source review
found no blocker. No abrupt-crash or live throughput qualification is implied.

Binary identity checks find the exact flag, typed writer helper and integer-margin
base helper; unrelated body-send, body-before-expiry and pass-B diagnostics are
absent. The production drain helper has no standalone symbol in this optimized
binary, so it is not used as an identity requirement. Same-binary OFF/ON live
measurements remain pending.
