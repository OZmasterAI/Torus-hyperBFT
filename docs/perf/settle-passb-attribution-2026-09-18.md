# Parallel settlement pass-B attribution

Qualified diagnostic on `diag/settle-passb-attribution`, based on `72f7013`.
Scoped measurement-gap issue: `bab3a2da-0465-4699-bd2a-aa9568fadd1d`.
Root verification passed 21 tests and a separate node build (receipt
`e56bcce8-cd68-4aa5-87f5-edf760ded3d8`). Independent review found no blocker.
Live attribution remains pending; no throughput improvement is claimed.

Set the exact value `TORUS_SETTLE_PASSB_DIAG=1` to enable. Unset, `0`, whitespace,
and other values leave it off. The setting is cached on the first actual
parallel pass B. OFF performs no sender inventory, new clock reads, or record
emission; optional-diagnostic branches remain in the instrumented source.

One payload-free `parallel settlement pass-B attribution` INFO record is emitted
per completed parallel pass-B invocation, after its existing timer stops.
`schema=1` and `block_height` identify the record format and block. A block may
contain multiple settlement invocations; do not assume one record per block.
Sequential settlement and a pass-A fallback emit no such record. A panic before
completion also emits none. This is not a completion audit or new metric API.

| Field | Meaning |
|---|---|
| `pass_b_ns` | The same elapsed interval added to the existing pass-B accumulator. |
| `position_merge_ns` | Sum around each market's `PositionCache::merge_disjoint`. |
| `balance_apply_ns` | Taker release plus ordered PnL application per visited order, and the market's maker/STP releases. Includes attempted reads, defensive clamps, and local counter overhead. Closed before any failed-order `continue`. |
| `trade_route_ns` | Successful-order trade-index stamping, routing calls, and index increments. Includes both immediate and deferred routing modes and local counting overhead. |
| `residual_ns` | `pass_b_ns` minus the three disjoint spans. Book reinsertion, IDs/results/metrics, iteration/destruction and other instrumentation overhead remain here. |
| `timing_valid` | False if the span sum exceeds the whole interval; the residual then saturates to zero and must not be treated as valid attribution. |
| `inventory_ns` | Read-only sender inventory after fallback was ruled out, before pass B starts. Excluded from all pass-B spans, but still costs enclosing settlement/engine time. |
| `markets_merged`, `orders_visited`, `failed_orders` | Actual visited market/order work; failed orders include the early balance-error path. |
| `balance_attempts`, `balance_read_errors` | Actual balance-cache load attempts and errors, including silent release-read failures and zero-PnL events. Later PnL events skipped after a failure are not attempts. These are not backend-read counts: cache hits count too. |
| `trade_route_calls` | Fill-level routing invocations. This does not certify storage success or durability; existing routing error behavior is unchanged. |
| `planned_balance_events` | Planned release/PnL events over the later pass-B visitation shape. Positive pre-clamp releases count even when live balance clamps make the update zero; zero-PnL events count. |
| `planned_unique_senders`, `planned_top1_events`, `planned_top4_events` | Unique planned balance keys and the largest one/four per-sender planned event totals. No addresses or per-sender payloads are emitted. |

The inventory is one read-only pass over plans and prepared-order references.
It does not read live balances or predict read failures. Its temporary map is
consumed before pass B; no diagnostic address hashing occurs inside the balance
timed loops. Planned concentration is neither successful work nor CPU share.
Several markets can touch the same sender; concentration counts those together
without changing their execution order.

Canonical market/order order, position merges, release clamping, zero-PnL row
materialization, first-error selection, trade IDs, gas, and result assignment
are unchanged. No pass-B work is parallelized. Existing pass-A/pass-B timer
boundaries remain; the new inventory and post-timer log intentionally lie
outside pass B. Detailed clocks are per order and per market, so even this
aggregate diagnostic can perturb cache behavior and timing. Compare declared
instrumented controls; do not treat an enabled run as a clean performance A/B.

Focused unit fixtures use thread-local test-only controls instead of global
environment mutation. They compare complete backend rows, mode-3 book saves,
all result fields (including `action_type`), gas, IDs, and trade bytes with the
diagnostic OFF/ON separately for immediate routing and deferred routing. They
do not reconstruct deferred rows to establish cross-route equivalence; the
routing implementation is unchanged. Shared senders, positive releases clamped to zero, zero-PnL materialization,
injected early balance-read failure, skipped later events, and position-failure
fallback are included. Pure checks cover exact flag parsing, top-four counts,
and valid/invalid disjoint duration totals.

Root verification passed these five focused and sixteen integration fixtures:

```sh
cargo test -p torus-bridge --release --lib settle_passb_diagnostic_tests
cargo test -p torus-bridge --release --test parallel_settle_tests --test engine_parallel_tests
```

The separately built node was positively identified by the diagnostic log and
flag signatures, with the unrelated body-receive-policy signature absent.
It is frozen as `artifacts/settle-passb-attribution` for live qualification.
An eventual diagnostic cell should retain identical book mode, worker caps,
workload and strict acceptance criteria, and record this flag in `EXTRA_ENV`
provenance. Decide whether balance application, position merging, or trade
routing merits a next experiment from these spans and end-to-end timers.
Neither sender concentration nor any one span establishes a parallel speedup
or a hardware ceiling.
