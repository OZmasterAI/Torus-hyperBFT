# s80 adversarial-worst-block: FOK pre-check, RO sweep and stop-miss attack sizes, plus a bounded FOK pre-check

- Branch `s80/adversarial-worst-block` (worktree `/home/oz/projects/wt/novel-s80/proto/adversarial-worst-block`, base 29bfc705), local only.
- Flag `TORUS_S80_FOK_BOUND` (node-local, read once, default OFF = exact 29bfc705 behaviour; `1`/`true` = on). INFO line
  `TORUS_S80_FOK_BOUND on=<bool>` at the first block (`NativeExecutor::new_with_mode`) or first FOK.
- Status: **built** (Step 1 measured for all three arms, Step 2 fix for arm A built, tested and measured).
- cell_worthy: **false** (the bench sends no FOK; the deliverable is the attack size and the fix size).

## 1. Premise check on 29bfc705

Code references (re-located on 29bfc705; they match main's numbering):

- `crates/torus-core/src/order_book.rs:1746-1797`: the FOK pre-check in `place_order_inner`. With a maker source
  (the batch path always passes one) it clones the whole `AccountMargins` and `ReduceOnlyPositions` per FOK (`:1753-1754`),
  then `can_fill_completely` (`:3224`) walks every opposite order up to the limit, running `maker_fill_fits` (IM delta,
  tiers, mark loss) per order and `continue`-ing past the taker's own orders (`:3246`). A rejected FOK (`:1797`) changes
  nothing, so it can be repeated.
- `crates/torus-mempool/src/rate_limit.rs`: 64 native actions per sender per block (`:50`), 400 in total (`:97`),
  200,000 orders per block (`:250`); `crates/torus-types/src/lib.rs:745`: 1,024 orders per batch. So one block can carry
  ~195 batches x 1,024 FOKs, all in one market (one serial pool job).
- `order_book.rs:2576` `pending_stop`: linear `find` over the book's stops; `:2589` `cancel_stop` likewise.
- `order_book.rs:1967` `sweep_reduce_only`: every reduce-only placement re-scans the trader's kept RO orders.

Microbench: new `#[ignore]` test `crates/torus-core/tests/adversarial_match_ubench.rs` (public `OrderBook` API only;
jemalloc global allocator, as the node). Run:
`cargo test --release -p torus-core --test adversarial_match_ubench -- --ignored --nocapture --test-threads=1`
(under the heavy lock; env `ADV_ARMS`, `ADV_D`, `ADV_S`, `ADV_K`, `ADV_R`, `ADV_N`, `ADV_RUNS`, `ADV_FOKS`).
Values are the median of `ADV_RUNS` (5; 3 for D=20k) timings of one action.

### Arm A: FOK pre-check (1,024 rejected FOK buys per action, one attacker)

Book: D one-lot asks over D/20 levels from 50 makers. Maps as installed by the batch path: `AccountMargins` with S
checked senders, a mark and pre-batch snapshots; `ReduceOnlyPositions` with the S senders, the 50 makers and K extra
holders. Cases: `nocross` (limit below the best ask: walk visits nothing), `thin` (one lot at the best ask, margin
fails: walk visits one order), `over` (qty = depth + 1 lot: walk visits all D), `margin` (qty = depth - 1 lot, the
attacker's margin fails the complete fill: walk visits ~D). `Some` = batch path (maker source), `None` = no maker
source (no clone, no maker checks: the lower bound). `clone` = the two map clones alone.

Flag OFF = 29bfc705 behaviour (final binary, md5 5910ca93; the pure-29bfc705 binary gave the same numbers, see raw
data), ms per action:

| D | S | K | clone | nocross Some | thin Some | thin None | over Some | over None | margin Some | margin None |
|---|---|---|---|---|---|---|---|---|---|---|
| 2,000 | 80 | 0 | 0.66 | 0.82 | 1.16 | 0.24 | 504.6 | 263.7 | 507.3 | 258.4 |
| 2,000 | 80 | 10k | 20.50 | 20.04 | 21.84 | 0.24 | 557.1 | 259.3 | 552.0 | 258.9 |
| 2,000 | 209 | 0 | 1.65 | 1.70 | 2.25 | 0.24 | 521.1 | 258.6 | 519.6 | 258.2 |
| 2,000 | 209 | 10k | 21.01 | 21.63 | 22.27 | 0.24 | 548.9 | 259.5 | 563.5 | 263.5 |
| 2,000 | 2,000 | 0 | 20.21 | 21.09 | 21.69 | 0.24 | 555.3 | 258.9 | 555.2 | 264.7 |
| 2,000 | 2,000 | 10k | 33.80 | 31.21 | 33.02 | 0.24 | 555.8 | 259.3 | 572.4 | 264.9 |
| 20,000 | 209 | 0 | 1.63 | 1.72 | 2.08 | 0.24 | 4,977.9 | 2,542.9 | 5,326.6 | 2,559.0 |
| 20,000 | 209 | 10k | 21.92 | 20.11 | 21.48 | 0.24 | 5,304.9 | 2,535.1 | 5,066.7 | 2,566.7 |

`nocross None` is 0.12 ms everywhere. The pure-29bfc705 binary (`step1.log`, no `nocross`/`thin` case yet) read
over Some 498-552 ms / margin Some 501-567 ms at D=2k and 5,110-5,220 ms at D=20k: the same as flag off.

Kill criteria (spec): A dies if < 20 ms/action at S=209/D=2k AND < 50 ms at K=10k. Measured 519-521 ms (over/margin) at
S=209/D=2k/K=0 and 549-564 ms at K=10k: **arm A is real, ~25x over the kill line**.

Premise correction: the write-up said 0.6-1.0 s of the ~1-1.4 s worst block is the per-FOK clone. Measured, the clone
is 1.6 us per FOK at S=209/K=0 (1.7 ms per action, 0.33 s per 200k FOKs), 21 us at K=10k (21 ms/action, 4.2 s per
200k FOKs) and 33 us at S=2k/K=10k. It dominates only when the walk is short (`nocross`, `thin`). The **walk** is the big
cost: ~250 ns per visited order with maker checks (~128 ns without), i.e. 0.5 s per action at D=2k and 5.1 s per action
at D=20k, independent of S and K.

### Arm B: reduce-only sweep (1,024 RO IOC sells per action from one trader)

| R kept RO asks | layout | nofill ms/action | 1-lot fill ms/action |
|---|---|---|---|
| 0 | - | 0.118 | 0.368 |
| 100 | one level | 6.51 | 6.93 |
| 100 | spread | 4.99 | 5.38 |
| 1,000 | one level | 112.7 | 118.0 |
| 1,000 | spread | 64.8 | 67.7 |

Kill: < 20 ms/action at R=1k. Measured 65-118 ms: **arm B is real** (~110 ns per kept order per placement).

### Arm C: stop miss (300 books, `pending_stop` of an absent id in every book)

| stops/book | total stops | linear (today) us/miss | binary search us/miss |
|---|---|---|---|
| 1,000 | 300,000 | 583 | 3.9 |
| 3,000 | 900,000 | 2,648 | 4.9 |

Kill: < 0.5 ms/miss at 300k stops. Measured 0.58 ms: **arm C is real but marginal at 300k** (5.3x at 900k). The stops
are id-ascending (pushes use fresh ids, restore enforces ascending ids, retain/remove keep order), so a binary search is
a byte-identical 150-540x fix (not built here; see section 5).

## 2. Design (arm A, the worst arm), `TORUS_S80_FOK_BOUND`

Both parts are node-local cost changes with byte-identical results (the pre-check never touches live state).

- **(a) Exact quantity early reject** (`OrderBook::fok_depth_orders`). Sum the remaining quantity of the other traders'
  orders on the levels the walk would cross (same `take_while` on the limit, own orders skipped), stopping at the first
  order that reaches the FOK quantity. Each walked order gives at most its remaining quantity, so if the sum is below the
  quantity the walk can only return `None`: reject with `FokCancel` at once (same `PlaceResult` as the walk's reject; the
  pre-check sits after every earlier step, nothing else differs). An overflowing sum counts as reaching (walk as before).
  Cost ~2.6 ns per counted order (a sequential scan, no maker checks).
- **(b) Copy-on-first-touch scratch maps** (`AccountMargins::fok_scratch`, `OrderBook::seed_fok_scratch`). Instead of
  cloning both maps, the walk runs on empty maps (settings, mark, tiers, counter copied) and copies a trader's live
  entries in before its first read: the taker at the start, each maker before its maker check. Every walk write to a
  trader is preceded by that read, so an absent scratch entry was never written and copying the live one reproduces the
  clone exactly. v1 always used the scratch; measured, the per-visit seeding lookups (~40 ns per checked order) made the
  full-depth `margin` walk 5-16% slower than the clone. v2 picks the scratch only when the maps hold more than
  `FOK_SCRATCH_RATIO` = 16 entries per order (a) counted (the walk checks at least that many orders; the clone costs ~2
  ns per entry, the scratch ~40 ns per checked order, break-even ~20). Cost heuristic only: both give the same result.
- **Not fixed (by design, needs consensus):** a FOK that passes (a) but cannot fill still walks its depth: `margin`
  (the taker's complete-fill margin fails) and phantom depth (margin- or RO-skipped makers). Only a deterministic work
  budget bounds it (section 5).

Diff (`git diff 29bfc705 --stat`):

```
 crates/torus-bridge/src/native_executor.rs         |   3 +   (read/log the flag at the first block)
 crates/torus-core/src/order_book.rs                | 261 ++++-  (flag, (a), (b), ratio, test hooks)
 .../torus-core/src/order_book/fok_bound_tests.rs   | 353 +++++  (new, 4 tests)
 .../torus-core/tests/adversarial_match_ubench.rs   | 353 +++++  (new, #[ignore] arms A-C)
 4 files changed, 968 insertions(+), 2 deletions(-)
```

Hot-path impact with the flag off: one `OnceLock` read per FOK placement and per block; nothing on GTC / IOC / Market
paths.

## 3. Tests

- RED first: the new tests did not compile against the prior state (v1 test RED: `FokBoundStats`/`FokBoundOverride`
  missing, `results/adv/red-check.log`; v2 test RED: `set_ratio` / `clone_walks` missing, 7 errors,
  `results/adv/red-check2.log`).
- `crates/torus-core/src/order_book/fok_bound_tests.rs` (4 tests):
  - `fok_bound_is_byte_identical_to_the_clone_precheck`: 400 random multi-placement scenarios (both book sides, every
    trader with own resting orders, RO makers, checked takers with/without mark charge, unchecked takers with pre-batch
    snapshots and D2 taker-only budgets, maps re-installed every 7 placements, GTC/IOC/FOK/Market). The transcript
    (every `PlaceResult`, the live RO and account maps sorted, charged-fill counter, BBO, order count after each
    placement, then every order row) is compared line by line flag off vs flag on with the scratch forced on every walk
    (ratio 0) and with the default ratio. Asserts every new path is reached (early rejects, scratch walks, clone walks,
    walk rejects, FOK fills > 50, FOK cancels > 100) and flag off takes none of them.
  - `fok_bound_quantity_bound_is_exact_at_the_edge`: own orders and levels beyond the limit are not counted; qty one
    above the countable depth rejects without a walk, qty equal to it walks and fills (flag off and on).
  - `fok_bound_scratch_copies_only_touched_traders`: with 5,000-entry maps, a 2-maker FOK copies <= 3 entries.
  - `fok_bound_picks_scratch_for_thin_walks_over_large_maps`: thin FOK over 2,001 entries -> scratch; 30-order walk
    over 21 entries -> clone; both equal to flag off.
  - Mutation check (v1, previous session): three deliberate bugs in the implementation (`results/adv/mut1-3.log`) each
    fail the differential or edge test.
- torus-core full suite: `cargo nextest run -p torus-core` v2: flag off 422/422 passed (17 skipped = ignored ubenches), flag on
  (`TORUS_S80_FOK_BOUND=1`, every FOK of every test on the new path) 422/422 passed. (v1: 421/421 off and on.)
- torus-bridge `perf_equivalence_golden` + every test matching `fok` (case-insensitive): v1 build (scratch on every walk + (a), i.e. the superset of v2's paths):
  `cargo nextest run -p torus-bridge -E 'test(perf_equivalence_golden) | test(/(?i)fok/)'` 4/4 passed flag off and
  flag on (the golden places no FOK: it proves the flag-off/on binary is neutral on the bench path). Every bridge test
  binary that places FOKs, run directly flag off and flag on (`bridge-fok-bins.log`): account_margin_tests 140/140,
  action_reason_tests 6/6, engine_parallel_tests 28/28, parallel_match_setup_tests 3/3, price_band_tests 6/6,
  reject_reason_metrics_tests 6/6, settle_no_merge_tests 7/7, lib `market_workers` 18/18 and `sell_top_up_stitch` 8/8,
  all passed both ways.
  v2 build (commit 6b381e04), golden + `fok` + those 7 binaries + lib `market_workers|sell_top_up_stitch` through
  nextest: flag off 223/223 passed, flag on 223/223 passed (`v2-bridge-off.log`, `v2-bridge-on.log`).
- Disk incident: `/home` reached 100% (68 MB free) at ~11:07 from other builds; the first v2 bridge attempt failed
  writing an rmeta (`No space left on device`), not on a test. It was re-queued after space came back (40 GB).

## 4. Measurements after the fix

Same binary (md5 5910ca93), flag ON, same grid. ms per action, off -> on (factor):

| D | S | K | nocross Some | thin Some | over Some | margin Some | margin None |
|---|---|---|---|---|---|---|---|
| 2,000 | 80 | 0 | 0.82 -> 0.08 (10x) | 1.16 -> 0.62 (1.9x) | 504.6 -> 5.86 (86x) | 507.3 -> 511.3 (1.0x) | 258.4 -> 262.2 |
| 2,000 | 80 | 10k | 20.04 -> 0.08 (251x) | 21.84 -> 0.62 (35x) | 557.1 -> 5.11 (109x) | 552.0 -> 565.3 (1.0x) | 258.9 -> 267.4 |
| 2,000 | 209 | 0 | 1.70 -> 0.08 (21x) | 2.25 -> 0.63 (3.6x) | 521.1 -> 5.12 (102x) | 519.6 -> 525.7 (1.0x) | 258.2 -> 262.7 |
| 2,000 | 209 | 10k | 21.63 -> 0.08 (270x) | 22.27 -> 0.62 (36x) | 548.9 -> 5.02 (109x) | 563.5 -> 564.1 (1.0x) | 263.5 -> 261.3 |
| 2,000 | 2,000 | 0 | 21.09 -> 0.08 (264x) | 21.69 -> 0.62 (35x) | 555.3 -> 5.34 (104x) | 555.2 -> 558.9 (1.0x) | 264.7 -> 262.8 |
| 2,000 | 2,000 | 10k | 31.21 -> 0.08 (395x) | 33.02 -> 0.62 (53x) | 555.8 -> 5.33 (104x) | 572.4 -> 573.4 (1.0x) | 264.9 -> 262.4 |
| 20,000 | 209 | 0 | 1.72 -> 0.08 (21x) | 2.08 -> 0.61 (3.4x) | 4,977.9 -> 69.9 (71x) | 5,326.6 -> 5,116.7 (1.0x) | 2,559.0 -> 2,631.9 |
| 20,000 | 209 | 10k | 20.11 -> 0.08 (245x) | 21.48 -> 0.61 (35x) | 5,304.9 -> 69.3 (77x) | 5,066.7 -> 5,130.5 (1.0x) | 2,566.7 -> 2,627.3 |

- `nocross` and `over`: (a) rejects without a walk or a clone; the residual is (a)'s own scan, ~2.5-2.9 ns per
  counted order at D=2k (5.0-5.9 ms/action) and ~3.4 ns at D=20k (69-70 ms/action: it no longer fits in cache).
- `thin`: (b), scratch maps (2-4 copied entries instead of cloning 210-14,050): 0.61-0.63 ms/action flat in S and K, vs the
  0.24-0.25 ms no-maker-source lower bound.
- `margin`: the walk is unchanged (it is the cost); with the v2 ratio the deep walk keeps the clone, so flag on equals
  flag off within noise (-3.9% .. +2.4%; the (a) scan adds ~1%). The v1 binary (scratch on every walk, `grid-on.log` vs `grid-off.log`) read
  margin Some 586-600 ms vs 515-561 off at D=2k (+5..+16%) and 5,821-5,972 vs 5,113-5,170 at D=20k (+14..+16%):
  the reason for the ratio.

ABBA (flag off, on, on, off, off, on; D=2k, S=209, ADV_RUNS=5 each), ms/action:

| K | case | makers | off medians (r1, r4, r5) | on medians (r2, r3, r6) | mean off | mean on | delta of means |
|---|---|---|---|---|---|---|---|
| 0 | clone | - | 1.628, 1.621, 1.674 | 1.615, 1.668, 1.608 | 1.641 | 1.630 | -0.7% (1.0x) |
| 0 | nocross | None | 0.119, 0.120, 0.118 | 0.079, 0.080, 0.080 | 0.119 | 0.080 | -33.1% (1.5x) |
| 0 | nocross | Some | 1.690, 1.821, 1.680 | 0.079, 0.084, 0.080 | 1.730 | 0.081 | -95.3% (21.4x) |
| 0 | thin | None | 0.241, 0.243, 0.243 | 0.248, 0.250, 0.249 | 0.242 | 0.249 | +2.8% (1.0x) |
| 0 | thin | Some | 2.040, 2.051, 2.045 | 0.616, 0.612, 0.627 | 2.045 | 0.618 | -69.8% (3.3x) |
| 0 | over | None | 255.753, 256.388, 256.895 | 6.302, 6.023, 5.598 | 256.345 | 5.974 | -97.7% (42.9x) |
| 0 | over | Some | 515.132, 515.275, 518.779 | 6.243, 5.205, 5.605 | 516.395 | 5.684 | -98.9% (90.8x) |
| 0 | margin | None | 257.262, 255.790, 258.164 | 265.653, 262.201, 265.929 | 257.072 | 264.594 | +2.9% (1.0x) |
| 0 | margin | Some | 510.819, 520.369, 517.203 | 522.388, 522.978, 530.411 | 516.130 | 525.259 | +1.8% (1.0x) |
| 10000 | clone | - | 21.264, 21.277, 20.967 | 21.116, 21.058, 21.027 | 21.169 | 21.067 | -0.5% (1.0x) |
| 10000 | nocross | None | 0.118, 0.119, 0.119 | 0.080, 0.082, 0.080 | 0.119 | 0.081 | -32.0% (1.5x) |
| 10000 | nocross | Some | 22.000, 20.730, 21.954 | 0.079, 0.081, 0.080 | 21.561 | 0.080 | -99.6% (269.5x) |
| 10000 | thin | None | 0.241, 0.247, 0.242 | 0.249, 0.246, 0.248 | 0.243 | 0.248 | +1.8% (1.0x) |
| 10000 | thin | Some | 22.703, 21.606, 22.005 | 0.616, 0.612, 0.622 | 22.105 | 0.617 | -97.2% (35.8x) |
| 10000 | over | None | 257.605, 255.779, 256.877 | 5.576, 5.354, 5.196 | 256.754 | 5.375 | -97.9% (47.8x) |
| 10000 | over | Some | 545.100, 554.066, 541.926 | 5.558, 5.503, 5.099 | 547.031 | 5.387 | -99.0% (101.6x) |
| 10000 | margin | None | 256.966, 256.200, 257.782 | 263.291, 268.374, 266.356 | 256.983 | 266.007 | +3.5% (1.0x) |
| 10000 | margin | Some | 549.727, 556.188, 559.138 | 567.880, 555.734, 555.236 | 555.018 | 559.617 | +0.8% (1.0x) |

The rows a fix targets move 21-270x (nocross), 3.3-36x (thin), 91-102x (over). The deep `margin` walk (no early
reject possible) pays (a)'s scan on top: +0.8-1.8% with the maker source, +2.9-3.5% without (~5-7 us per FOK for
~2,000 counted orders); `clone` (not a placement) is unchanged.

Engine proxy (ubench_econ, UB_MARKS=1, UB_RUNS=5, UB_MARKETS=300, TORUS_SETTLE_STREAMED=1; neither ubench_econ nor
the bench sends a FOK, so the flag is never consulted on this path). v1 binary via `ub.sh`, ABBA order, MEDIAN
engine_ms/1k_fills:

| round | arm | MEDIAN engine_ms/1k_fills | per-run values |
|---|---|---|---|
| 1 | off | 2.78 | 2.90, 2.87, 2.78, 2.74, 2.70 |
| 2 | on | 2.77 | 2.83, 2.88, 2.77, 2.71, 2.71 |
| 3 | on | 3.35 | 3.35, 2.94, 4.55, 3.72, 3.35 (host disturbed: load average 225-240 at 09:26) |
| 4 | off | 2.78 | 2.85, 2.84, 2.78, 2.75, 2.77 |
| 5 | off | 2.98 | 2.98, 2.85, 5.02, 3.17, 2.92 (one 5.02 outlier run) |
| 6 | on | 2.84 | 2.85, 2.84, 2.77, 2.85, 2.73 |

Mean of medians: off 2.847, on 2.987 (+4.9%), driven by the disturbed round 3; without rounds 3 and 5 (the two with
outlier runs): off 2.78, on 2.805 (+0.9%), inside the 3-5% spread. The flag is not consulted on this path (no FOK
order is ever placed; one `OnceLock` read per block), so the A/B is a layout/noise check: no measurable effect.

v2 binary (commit 6b381e04, ubench_econ md5 a8186418), 4 rounds back to back under one lock hold, ABBA:

| round | arm | MEDIAN engine_ms/1k_fills | per-run values |
|---|---|---|---|
| 1 | off | 2.78 | 2.88, 2.78, 2.80, 2.77, 2.74 |
| 2 | on | 2.73 | 3.18, 2.87, 2.73, 2.73, 2.69 |
| 3 | on | 2.75 | 2.80, 2.79, 2.75, 2.71, 2.73 |
| 4 | off | 2.78 | 2.82, 2.83, 2.78, 2.68, 2.72 |

Mean of medians: off 2.780, on 2.740 (-1.4%), inside the spread (calibration 2.79-2.93). Engine delta: 0 (no FOK).

## 5. Per-block worst-case budget note (for the owner)

One block may carry 200,000 native orders (~195 batches x 1,024), all aimed at one market, and one market's work is
one serial pool job that E waits on. Rejected FOKs release their reservation and pay no fee, so the attacker's capital
is reusable every block. Worst one-market block cost = measured ms/action x 195 actions (seconds per block on that
market's worker; the std exec block is ~38 ms):

| attack (D=2k unless noted) | S / K | flag off (29bfc705) | flag on |
|---|---|---|---|
| nocross (no crossing) | 209 / 0 | 0.33 s | 0.016 s |
| nocross | 209 / 10k (RO map inflated by resting RO orders' owners) | 4.2 s | 0.016 s |
| nocross | 2k / 10k | 6.1 s | 0.016 s |
| thin (1 lot, margin fails) | 209 / 10k | 4.3 s | 0.12 s |
| thin | 2k / 10k | 6.4 s | 0.12 s |
| over (qty > depth) | 209 / 0-10k | 102-107 s | 1.0 s |
| over, D=20k | 209 / 0-10k | 971-1,034 s | 13.5-13.6 s |
| margin / phantom depth | any | 99-112 s | unchanged (99-112 s) |
| margin / phantom depth, D=20k | 209 | 988-1,039 s | unchanged |
| RO sweep, R=1k one level (arm B) | - | 22-23 s | (not built) |

Reachability notes (code reading, not run through the executor): the `margin` variant needs a complete fill whose need
exceeds the taker's budget, which is its own reservation plus its running free margin
(`native_executor.rs:12302-12312`, `taker_margin_limit`); with a mark (s94 option 1) a buy above the mark carries a
mark loss the reservation does not cover, so a sender with free margin just above the order IM fails after the walk.
Phantom depth needs resting orders from makers that fail `maker_fill_fits` (margin-short makers) or kept RO orders.
The block-bytes cap (12 MB, `rate_limit.rs:309`) and the 64-actions-per-sender cap (4 senders for 195 actions) do not
bind first.

Recommendations:

1. **Take (a)+(b)** (`TORUS_S80_FOK_BOUND`): byte-identical, no consensus change; removes the over-depth and the clone
   attacks (100x and 25-400x per action). Residual of (a) itself: O(D) at ~2.6-3.5 ns per counted order (a D=20k
   book still costs ~70 ms per over-depth action). Per-level quantity totals maintained in the book would make (a)
   O(levels) (est. <0.5% match CPU upkeep; not built).
2. **A consensus work budget is still required** for the margin / phantom-depth walk (0.5 s per action at D=2k, ~100 s
   per worst block): e.g. R16 option B, W visited orders per action (16,384 -> <= ~4 ms per action) plus a per-market
   per-block budget (e.g. 200k visits -> <= ~50 ms per block), FOKs over budget reject `FokCancel`. Or the owner rule
   "batch items are GTC/ALO only" (takers as single actions: <= 400 per block -> 0.2 s worst at D=2k). Both are
   consensus-visible (P0.0 activation).
3. **RO sweep (arm B)**: 65-118 ms per 1,024-order action at R=1k (~23 s per worst block). An O(1) "nothing to cut"
   aggregate (r1 ro-sweep-aggregate) is byte-identical; not built.
4. **Stop miss (arm C)**: a binary search in `pending_stop` / `cancel_stop` is a byte-identical ~5-line change
   (0.58 -> 0.004 ms per miss at 300k stops); not built.

## 6. Verdict

- **Premise:** the attack is real and bigger than the write-up said, but its main cost is the pre-check **walk**
  (~250 ns per visited order), not the per-FOK map clone (1.6 us per FOK at S=209/K=0; 21-33 us only when the RO map is
  inflated to 10k or S is 2k). Arms A, B and C all pass their kill lines (A ~25x, B 3-6x, C 1.2x at 300k stops).
- **Fix built (arm A):** `TORUS_S80_FOK_BOUND`, byte-identical (400-scenario differential transcript, flag off and on
  over the whole torus-core suite, bridge golden off and on). Per 1,024-FOK action at D=2k: over-depth 505-557 ->
  5.0-5.9 ms (86-109x), no-cross 0.8-31 -> 0.08 ms (10-395x), thin margin-failing 1.2-33 -> 0.62 ms (1.9-53x);
  margin / phantom-depth walk unchanged (by design: needs a consensus budget). Worst one-market block for the fixed
  shapes: ~105 s -> ~1 s (D=2k), ~6 s -> 0.12 s (clone attacks).
- **Still open (owner):** the margin / phantom-depth walk (~100 s per worst block at D=2k) needs a consensus work
  budget or a "batch items are GTC/ALO only" rule; (a) itself is O(D) (~70 ms/action at D=20k), per-level totals would
  make it O(levels); arm B (RO sweep, ~23 s per worst block) and arm C (binary search, 5 lines) are byte-identical
  follow-ups not built here.
- **Bench:** zero effect expected and measured within noise (no FOK in the bench); cell_worthy = false.

## Raw data

All under `/home/oz/projects/wt/novel-s80/proto/results/adv/`: `step1.log` (pure 29bfc705 binary `ubench-base-29bfc705`,
arms A-C), `grid-off.log` / `grid-on.log` (v1 binary), `v2-grid-off.log` / `v2-grid-on.log`, `v2-abba-*.log`,
`econ-abba.log` and `../ub-adversarial-worst-block-{off,on}-N.log` (v1), `../ub-adversarial-worst-block-v2-*.log` (v2),
test logs `core-*.log`, `v2-core-*.log`, `bridge-*.log`, `bridge-fok-bins.log`, `v2-bridge-*.log`, RED checks
`red-check.log`, `red-check2.log`, mutation logs `mut1-3.log`; scripts `step1.sh`, `pipe*.sh`, `abba-batch.sh`,
`econ-batch.sh`. Not run: clippy, the full workspace suite (spec scope: touched crates + golden), cells (no FOK in the
bench shape).
