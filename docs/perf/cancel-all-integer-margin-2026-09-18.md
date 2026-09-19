# Cancel-all integer-leverage margin candidate

Ported on 2026-09-19 to `perf/s61-cancel-path`, based on frozen record runtime
`080c4fa` plus two-ended cancellation candidate `96063cc`. Only commit
`f2a7164`'s bridge arithmetic and tests were reused; its older `f890e06` queue
implementation is not imported. New issue `cf04c5e5-d4df-49c6-a6cd-64cb97875ce2`,
attempt `8a121b2b-3a51-4c37-96df-4c37d9bfe646` tracks this port.

The previous lineage had correctness qualification, but its receipts do not
qualify this new combination. Current qualification is recorded by the root
scheduler in the session receipt. No throughput run or microbenchmark was
performed for the port, and no measured gain or bottleneck resolution is claimed.

The two `exec_cancel_all` branches previously performed a borrowed configuration
hash lookup and general fixed-point division for each returned order. The
candidate hoists `Option<&MarketMarginConfig>` once per nonempty cancelled market
and replaces only
the division by integer leverage. It does not clone configuration or change
queue cancellation. Tier lookup still occurs for each order using its own
notional, the first matching `<=` threshold, default20 for absent configuration,
and fallback1 for empty/unmatched tiers. The configuration reference is local to
this action; mutation of order books and dirty-book tracking uses disjoint fields.

## Arithmetic equivalence and error boundary

`FixedPoint` uses signed i128 raw values with `SCALE = 100_000_000`. Let N be the
raw notional after the existing checked fixed-point multiplication, and L the
u32 selected leverage. For L>0, the old expression is:

```
trunc_toward_zero((i256(N) * SCALE) / i256(i128(L) * SCALE))
```

This is exactly `N / i128(L)`, including negative N. The scaled numerator fits
i256 even at both i128 extremes; the scaled u32 denominator fits i128; and a
positive integer divisor cannot overflow the resulting i128 quotient. There is
no saturation. A u32 cannot produce the signed-divide overflow case MIN/-1.

The context-free helper uses `checked_div`, maps its only possible failure to
`ArithmeticError::DivisionByZero`, and retains `expect("FixedPoint division error")`.
Thus zero still panics with the old payload. This call stays after book removal,
dirty-book marking, that order's notional multiplication and its tier lookup.
There is no prevalidation that would move the failure before cancellation.
Per-order rounding and checked accumulation order remain unchanged, as do their
overflow panics and the eventual clamp to the sender's current `order_margin`.
The helper is used only by cancel-all. In particular, the existing general
`reserve_for_qty_cfg` is not substituted: its nonpositive-input clamp would
change cancel-all behavior on such inputs.

The source simplification removes the second scaled multiplication/division
representation and narrowing checks. It does not remove price-times-quantity
fixed-point multiplication. Ethnum's native implementation already has a
small-operand division fast path, and optimization can further change generated
code. This document makes no instruction-count, latency, bottleneck-share or
throughput claim.

## Prepared verification

Two unit tests compare the actual private helper against the old expression:

- `cancel_all_integer_margin_matches_general_division_extremes`: signed limits,
  fractional raw values, leverage1 through u32::MAX, and512 deterministic broad
  raw/leverage samples.
- `cancel_all_integer_margin_zero_preserves_panic`: zero leverage at negative,
  zero and positive notional extremes, comparing complete panic text.

The real-StateDb maker-margin harness retains a frozen executor oracle shared
by `f890e06` and `080c4fa` (the relevant source is identical), preserving the
original arithmetic and side-effect order:

- `cancel_all_integer_margin_real_state_matches_legacy`: Some(market)/None,
  absent market, tier boundaries, absent/empty configs, fractional quantities,
  partial maker fill, clamp/no-clamp and repeated empty cancellation. Compare
  action results, exact balance CF bytes for every trader, dirty books,
  remaining order/stop rows, row/level journals and Mode3 level commitment bytes.
- `cancel_all_integer_margin_zero_preserves_mutation_boundary`: introduce a
  zero-leverage tier after resting orders exist; compare panic text, cancelled
  book state, dirty marks and unchanged balance bytes with the old oracle.

The port also compares the two executor implementations on a restored
2,048-deep queue with 80 dispersed targets, which engages current core batching
and compaction. It checks exact released margin, all balances, remaining rows,
journals and chunked commitments. The observation helper now drains row journals
before `full_row_ops` clears them, and compares `next_seq` as well as order IDs.

Requested current checks include the private helper, maker-margin,
parallel-matching, order-book persistence and level-row suites:

```sh
cargo test --release -p torus-bridge --lib cancel_all_integer_margin -- --test-threads=1
cargo test --release -p torus-bridge --test maker_margin_release_tests -- --test-threads=1
```

The historical node build and artifacts belonged to the old lineage and do not
identify a binary for this port. Qualification here is correctness-only; future
performance acceptance requires an exact-candidate comparison under the normal
acceptance gates. Book cancellation was previously observed to dominate total
cancel time, so simplifying margin arithmetic alone is not evidence of resolving
that bottleneck.
