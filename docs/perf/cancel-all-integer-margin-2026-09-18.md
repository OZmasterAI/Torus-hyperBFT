# Cancel-all integer-leverage margin candidate

Qualified candidate on `perf/cancel-margin-integer`, based on queue-compaction
revision `f890e06`. Issue `a711d834-f4bb-40aa-a0f8-085c75c2260b`, attempt
`84401b87-fa3d-41b5-a14e-2d16201bd38f`. Root verification passed 47 tests and a
separate node build (receipt `3a69134a-c9dc-41a5-a58b-fbf7e3873bab`). Independent
review found no blocker. No microbenchmark or live result applies yet; the
baseline's accepted run is not comparative evidence for this change.

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

The existing real-StateDb maker-margin test harness gains a frozen `f890e06`
cancel-all oracle, retaining its original arithmetic and side-effect order:

- `cancel_all_integer_margin_real_state_matches_legacy`: Some(market)/None,
  absent market, tier boundaries, absent/empty configs, fractional quantities,
  partial maker fill, clamp/no-clamp and repeated empty cancellation. Compare
  action results, exact balance CF bytes for every trader, dirty books,
  remaining order/stop rows, row/level journals and Mode3 level commitment bytes.
- `cancel_all_integer_margin_zero_preserves_mutation_boundary`: introduce a
  zero-leverage tier after resting orders exist; compare panic text, cancelled
  book state, dirty marks and unchanged balance bytes with the old oracle.

Existing maker-margin, parallel-matching and persistence suites remain relevant.
Root ran these focused checks plus the parallel-matching, order-book persistence
and level-row integration suites:

```sh
cargo test --release -p torus-bridge --lib cancel_all_integer_margin -- --test-threads=1
cargo test --release -p torus-bridge --test maker_margin_release_tests -- --test-threads=1
```

The separate node was positively identified by the new integer-margin helper
and the expected grouped-removal helper symbols, with unrelated pass-B and
body-receive-policy diagnostic signatures absent. It is frozen in
`artifacts/cancel-margin-integer`.

The correctness pass does not establish a speedup. Attribution or an isolated
same-workload performance comparison is required before performance acceptance.
