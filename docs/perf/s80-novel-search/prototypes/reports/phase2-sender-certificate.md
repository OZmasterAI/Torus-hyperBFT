# s80 prototype: phase2-sender-certificate (arm H only)

Branch `s80/phase2-sender-certificate` (local only, not pushed), worktree
`/home/oz/projects/wt/novel-s80/proto/phase2-sender-certificate`, base 29bfc705. First build a2dac3ea; FIX ROUND
commit e35bb1a4 (section 0) on top of it.
Target dir `/home/oz/.cargo-target-s80-phase2-sender-certificate` (ubench_econ binary
`ubench_econ-1a6f456edd610a23`; bench-throughput fa8fd4bd unchanged; release/torus-node md5
3cb731239a677b305c6ce0c3126d9301 at a2dac3ea, contains the flag string).
Flag `TORUS_S80_P2_CERT` (read once per process at the first `execute_batch`, INFO line
`TORUS_S80_P2_CERT on=<bool> mode=<mode>`):
`0`/unset = exact 29bfc705 path; `1` = certificate + skip (serial loop and, since the fix round, the sharded
prepare); `shadow` = exact path + check the certificate's claims (since the fix round: a violation is counted and
logged at ERROR, never a panic); `probe` = exact path + certificate statistics (pre-pass cost). The `ub` arm (UNSOUND
upper bound, bench only) existed in a2dac3ea and was REMOVED in the fix round (`ub` now parses as off). Arm B
(market-major pool evaluation) was not built, per the ranking.

## 0. Fix round (adversarial review of a2dac3ea)

| # | review issue | fix (e35bb1a4) |
|---|---|---|
| 1 | P1 margin bypass: an order the exact path rejects for its price (limit <= 0) or a negative quantity entered the sums with a negative notional, so `need` went negative and the sender was certified (100 units available, a 15000-IM buy accepted, flag on and off disagree) | pass 1 of `p2_cert_one` returns NOT_ELIGIBLE unless `validate_order_price(p).is_ok() && p.quantity > 0`; every price and quantity summed is then > 0 |
| 2 | P1 panic: `base - r` / `r - base` (FixedPoint `Sub` -> `expect`) overflowed on a sell at raw `i128::MIN + 1` in the anchor market, with the flag on, probe or shadow | both adverse terms (own orders and resting orders) use `i128::checked_sub`, mapped to OVERFLOW (not certified); with fix 1 the own-order case is unreachable anyway |
| 3 | P2 sharded path (`TORUS_PARALLEL_ENGINE >= 2`, >= 64 orders) never certified | `phase2_parallel_prepare` takes `cert: Option<(mode, &books)>`; each worker certifies its sender group (`p2_cert_group`: the sender's PlaceOrders of the call in flat order, the same input `p2_certify` gathers) and passes `certified` to `prepare_one`; shadow checks there too; stat 11 counts sharded certifications |
| 4 | P3 `tup` fell back to 0 when `sell_top_up_price(bid)` overflows | falls back to the bid itself (no top-up is granted then, so `max(limit, bid)` is exactly the highest reservation price) |
| 5 | P3 no test at the bound / with invalid orders | 5 new tests in `tests/p2_cert_tests.rs` (below), incl. the two review probes with shadow and probe arms and a bisected slack-0 sender |
| 6 | P3 cfg(test) sticky global `P2_CERT_SKIPPED` weakened the M1 `pre_pos` oracle for the whole test process | removed; `PrepPass` / `PreparedOrder` carry a cfg(test) `cert_skipped` per order, and `match_setup`'s M1 shadow accepts `pre_pos = None` only for those orders |
| 7 | P3 `ub` unsound; `shadow` used a release `assert!` | `ub` removed (enum, parse, loop); shadow is `p2_shadow_check`: counts (stat 10) + `tracing::error!`, never panics |

New tests (`crates/torus-bridge/tests/p2_cert_tests.rs`, two-block fixture straight on the DB, every arm compared on
results + fills + fatal + next id + post-block DB digest, shadow mismatch count asserted 0):
- `p2_cert_invalid_price_or_quantity_voids_the_certificate`: the review's repro (100 units, a 10 x mid buy = 15000 IM)
  alone (control: bound fails, margin reject), then with each of: buy at -1e9 units, buy at 0, sell at raw
  i128::MIN+1, buy / sell of quantity -1e6 units, quantity 0 (market 2). Off == On == Shadow == Probe, the big buy is
  a margin reject in every arm, the sender is NOT_ELIGIBLE (never certified, also when rich).
- `p2_cert_extreme_prices_do_not_panic`: the review's panic repro (sell at mark + sell at i128::MIN+1) plus buy / sell
  at i128::MAX and an i128::MAX x i128::MAX sell: no panic, all arms identical.
- `p2_cert_shape_and_band_rejects_in_a_certified_batch`: a rich sender mixing a below-lot, an off-tick, two
  out-of-band orders with valid ones (a crossing checked sell, a second-market sell): still certified (rejected orders
  only add to the bound), all arms identical, the valid orders skipped.
- `p2_cert_at_the_bound` (serial and sharded, threads 0 and 4): the sender holds a losing long (bought at 130% of the
  mid, mark now 95%), its checked sell crosses a bid at 120% (the pool pays the IM above its reservation), its
  second-market sell takes the bid floor + B-blind top-up, and it rests a buy and a PostOnly buy. Bisection over
  `available` (raw units) finds the smallest certified balance (24380.40000035 units, i.e. slack 0) and the smallest
  balance where the exact path has no margin reject (14775 units: the certificate is 9605 units / 65% conservative
  on this shape). At the bound, +1 raw and -1 raw: Off == On == Shadow == Probe; at the bound the exact path has no
  margin reject and the same fill count as with 1M units (the pool never binds); -1 raw is not certified.
- `p2_cert_sharded_prepare_certifies_and_is_byte_identical`: the 30-block near-margin fixture through
  `execute_batch_engine_mode(4)`: Off == On == Shadow; On stats `[248, 137, 1264, 573, 573, 55, 0, 56, 0, 0, 0, 137]`
  (137 senders certified, all in the sharded workers; 573 orders skipped; 56 bound failures; 0 shadow mismatches).

RED: the review's probe tests (`proto/review-phase2-sender-certificate/crates/torus-bridge/tests/p2_cert_review_tests.rs`)
failed 2/2 on a2dac3ea (margin bypass; 'FixedPoint subtraction underflow' panic); the same inputs are now in the
first two tests above and pass. Fix-round results: section 4b; microbench: section 5b.

## 1. Premise check on 29bfc705

Code re-located on 29bfc705 (main's numbers differ): serial Phase-2 loop `native_executor.rs:9448-9480`
(pre-change), `prepare_one` `:9959`, account block `:10075-10200` (position read in
`st.proj.get_or_try_insert` `:10093`, `account_check` `:12321` -> `torus_core::margin::placement_need`), D2 pools
`d2_pools` `:12597`, B-blind top-ups `sell_top_ups(_fast)` `:12653/:12727`, `match_setup` `:13020` (M1 `pre_pos`
reuse `:13041-13090`, test oracle `:17041`).

Profile (perf record -F 4000, ubench_econ UB_MARKS=1 UB_RUNS=2 300 markets, 29bfc705 binary, flat self %, whole
process; exec thread E = 48.6% of samples, exec pool 51.4%). On E: `get_position` 0.78, `prepare_one` 0.83,
`Projections::get_or_try_insert` + rehash 0.29, `account_check` + `placement_need` 0.24, `__divti3` 0.74 (part),
`stitch_outcome` 0.54, `take_open_slot` 0.19. ubench breakdown (29bfc705, 600 senders): margin/1k 0.55-0.60 of
engine 2.74-2.87 (about 20%), 15.2k accepted orders/blk, so about 0.4 us of margin phase per order.
Pass-rate premise: ubench senders and the cell's genesis bulk senders both hold 100M (testnet/gen-weighted-genesis.sh
NATIVE_AVAIL default 100000000.0), 0 margin rejects, all top-ups full. The cost and the headroom are there: not
killed.

## 2. Design (arm H)

Before the serial loop (only when the flag is `1`/`shadow`/`probe`), `p2_certify` (`native_executor.rs:10393`)
groups the call's PlaceOrders by sender (contiguous runs; a sender with several runs is gathered over all of them in
flat order, so the certificate is per sender per whole call) and `p2_cert_one` (`:10483`) checks

    F0 = available + pos_net  >=  floor(Σ notional_i / L_min) + 1        (R + T)
                                 + 2 raw per order                       (X_ub)
                                 + C_ub(anchor market)

- Bail to the exact path unless every order of the sender is a plain GTC / PostOnly limit, not reduce-only
  (no stop / IOC / FOK / market), with a valid price (`validate_order_price`: > 0) and a quantity > 0 (fix round),
  without a `basis` override, in a market with a usable mark equal to its band
  reference, a band, and flat tiers (`p2_flat_cap`: one leverage L > 0 in every tier; the 1x fallback above the
  largest tier bounds the range). The open-order limit cannot bind (open + orders <= 1000), so the anchor below is
  the fold's D2 pool market.
- R + T: notional_i at `max(limit, top-up price of the best bid)` for bid-floor sells, else the limit: covers the
  reservation, the option-B floor and the B-blind top-up grant (one checked i128 mul per order; one division per
  sender: `Σ floor(n_i / L_i) <= Σ n_i / L_min`).
- X_ub: flat tiers give `placement_need <= reservation + 1 raw` (floor(a+b) - floor(a) <= floor(b) + 1, IM
  monotone; unit test over 20k random positions/orders), plus <= 1 raw committed excess per earlier order.
- C_ub, only in the anchor market (the first checked order passing the pure pre-book checks: price validation,
  shape, band): per own order the mark-loss bound `max(0, ±(limit - reference)) x qty` (taker or maker fills; the
  charge is <= the loss), per own checked sell the IM above its reservation at the highest fill price
  `max(best bid, band top)` + 2 raw, per own resting order there the mark-loss bound on its remainder, and 4 raw of
  rounding per possible fill in that market (2 x batch orders + resting orders).
- Overflow / range guards: PosSums `abs[2]` (Σ |position notional|) <= i128::MAX >> 20, no isolated positions,
  Σ qty x the highest price any check values an order at + resting notional <= the flat cap; any checked overflow =
  not certified (no panicking FixedPoint op is reached on the skip path).
- Per-market constants (top-up price, or the bid itself when the top-up price overflows; `max(best bid, band top)`,
  leverage, cap) are computed once per call in `phase2_markets` (`P2CertMarket`, only with the flag on).
- Sharded prepare (fix round): each worker runs `p2_cert_one` on its sender group (the sender's PlaceOrders of the
  call in flat order, i.e. exactly `p2_certify`'s gathered input) before that sender's `prepare_one` calls.

Then every account check of the sender passes, every top-up is granted in full, and the D2 pool cannot bind, with the
exact excess or with the skip path's 0 (they differ by <= 1 raw per order, and the pool value is never stored). So
`prepare_one(certified = true)` skips the position read, Projections, `placement_need`, the excess and the projection
advance; it still runs the slot, validation, shape, band, reservation, `pos_net` and the balance debit, sets
`pre_pos = None` (Phase 3's `match_setup` reads the position itself, as for reduce-only senders) and keeps `st.pool`
(so floors and the top-up candidates are unchanged).

## 3. Diff summary (a2dac3ea vs 29bfc705)

- `crates/torus-bridge/src/native_executor.rs` (+~540): `P2CertMode` / `parse_p2_cert` / `with_p2_cert` (test pin) /
  `p2_cert_mode` (env once + INFO line) / probe counters `p2_cert_stats_take`; `prepare_one(.., certified)`;
  `p2_certify`, `p2_cert_one`, `p2_cert_market`, `p2_flat_cap`, `p2_band_hi`, `p2_cert_eligible`;
  `Phase2Market.cert`; the shadow assertion in the serial loop; the M1 shadow in `match_setup` (cfg(test)) accepts
  `pre_pos = None` once a certified skip ran; `p2_cert_unit_tests`.
- `crates/torus-bridge/tests/p2_cert_tests.rs` (new): near-margin multi-block differential.
- `crates/torus-bridge/tests/ubench_econ.rs`: prints a `UB CERT` line (pass rate, bails, min slack).
- `sell_top_up_stitch_tests.rs`, `r02_margin_release_unit_tests.rs`: new argument / field.

Fix round (e35bb1a4 vs a2dac3ea): `native_executor.rs` (pass-1 validity bail, checked adverse terms, `tup` fallback,
`p2_cert_group` + certificates in `phase2_parallel_prepare`, `p2_shadow_check` (no panic, stat 10), `ub` removed,
stats 10 -> 12 slots, per-order cfg(test) `cert_skipped` replacing the global `P2_CERT_SKIPPED`);
`tests/p2_cert_tests.rs` (+5 tests, `run_engine`, shadow-mismatch asserts, parse `ub` -> off);
`sell_top_up_stitch_tests.rs`, `r02_margin_release_unit_tests.rs`, `r02_action_rejects_unit_tests.rs`
(`cert_skipped: false`, `None` cert argument).

## 4. Tests

- `tests/p2_cert_tests.rs` (28 senders funded 4.5k..126k units, 4 marked markets with a 15 bp mark walk, prices up to
  ±40% from the mid inside the ±50% default band so checked sells fill far above their limits, a third of the senders
  mixing reduce-only / IOC / FOK / market / stop-limit / PostOnly, cancel-alls, 30 blocks, resident rows attached):
  per block results + liquidation results + fills + fatal + next id + keccak of the post-flush DB (hashed CFs, native
  state root, running hash). `p2_cert_on_is_byte_identical_near_margin` (seeds 0-2, off vs `1`): identical, with
  449 / 573 / 583 orders skipped, 56 / 56 / 55 senders failing the bound and 66 / 55 / 62 bailing on special orders
  (the fixture straddles the bound). `p2_cert_shadow_and_probe_are_byte_identical` (seed 7): shadow and probe identical
  to off, shadow asserted every certified order. `parse_p2_cert_values`. 3/3 pass (release).
- lib `p2_cert_unit_tests` (flat cap incl. the 1x fallback and 0x tier, band top == the band rule's boundary,
  eligibility table, `placement_need <= reservation + 1 raw` over 20k random cases) + the touched modules
  `option_b_fold_pool_tests`, `sell_top_up_stitch*`, `r02_margin_release*`: 17/17 pass with the flag unset and with
  `TORUS_S80_P2_CERT=1`.
- `perf_equivalence_golden`: 5/5 with `TORUS_S80_P2_CERT=0`, `1` and `shadow` (its scenarios are mostly unmarked, so
  the certificate mostly bails there; the differential above is the real coverage).
- Full torus-bridge nextest (release), flag off and on: see section 6.

## 4b. Tests at the fix round (e35bb1a4)

- Targeted (release, flag unset): `p2_cert*` (8 tests: the 3 old + parse + the 5 new) + `perf_equivalence_golden` +
  `sell_top_up_stitch*` + `r02_margin_release*` + `r02_action_rejects*` + `option_b_fold_pool*`: 29/29 passed.
- Full `cargo nextest run --release -p torus-bridge --no-fail-fast` (golden included), one lock hold, 12:19-12:20:
  flag unset 1101/1101 passed, 17 skipped (12.5 s); `TORUS_S80_P2_CERT=1` 1101/1101 (12.5 s);
  `TORUS_S80_P2_CERT=shadow` 1101/1101 (12.5 s). flaky 0. (The p2_cert tests pin their mode; the env arms run the
  rest of the crate, golden included, through the certificate.)
- The new tests' printed numbers: bound test (threads 0 and 4) certified from 2438040000035 raw (24380.4 units),
  exact path without margin reject from 1477500000000 raw (14775 units); sharded differential On stats
  `[248, 137, 1264, 573, 573, 55, 0, 56, 0, 0, 0, 137]`.
- Not run: clippy, the workspace gate, cells (batch rule), a RED build of the new tests on a2dac3ea (the review's
  probe tests are the RED evidence: 2/2 FAIL on a2dac3ea).

## 5. Microbench of a2dac3ea (ubench_econ, 300 markets, UB_MARKS=1, UB_RUNS=5, TORUS_SETTLE_STREAMED=1, one lock hold per set)

Final code (v2 = a2dac3ea minus a doc line), ABBA order off,on,on,off,off,on (+ probe, ub):

| set | arm | MEDIAN engine ms/1k per invocation | mean | margin/1k | match/1k | engine ms/blk |
|---|---|---|---|---|---|---|
| UB_SENDERS=5000 | off | 2.32, 2.31, 2.33 | 2.320 | 0.479 | 0.116 | 37.19 |
| UB_SENDERS=5000 | on | 2.19, 2.21, 2.21 | 2.203 (-5.0%) | 0.361 (-24.6%) | 0.120 | 35.33 (-1.86) |
| UB_SENDERS=5000 | probe | 2.36 | | 0.514 (+0.035 pre-pass) | 0.114 | 37.60 |
| UB_SENDERS=600 | off | 2.79, 2.78, 2.79 | 2.787 | 0.570 | 0.159 | 32.45 |
| UB_SENDERS=600 | on | 2.64, 2.62, 2.60 | 2.620 (-6.0%) | 0.378 (-33.7%) | 0.178 | 30.49 (-1.96) |
| UB_SENDERS=600 | probe | 2.83 | | 0.602 (+0.032) | 0.184 | 32.96 |
| UB_SENDERS=600 | ub (unsound bound) | 2.54 | | 0.340 | 0.182 | 29.86 |

(margin/match/blk columns: mean over the 15 / 5 per-run values of the set.) Certificate: 5000 senders 37791/37791
senders and 15.17M/15.17M orders certified (100%; 135 multi-run senders gathered), 600 senders 28959/28962 (99.99%)
senders, 99.97% orders; 0 bound failures; min slack 98.76% of F0. fills/blk (16284/16072/15830/16024/16145 at 5000,
11294/11379/11644/11853/11816 at 600) and the s92 line (top_ups full=487104 / 375838 ..., partial 0, none 0,
sell cuts) identical in every arm and run.

First version (v1: per-run certificate with no multi-run gathering, so ~5% of senders bailed at 600; per-order band /
top-up divisions), same method:
- 5000: off 2.29, 2.29, 2.29 (2.290); on 2.21, 2.22, 2.22 (2.217, -3.2%); margin 0.46 -> 0.39; pass 99.3%.
- 600: off 2.74, 2.77, 2.75 (2.753); on 2.62, 2.64, 2.66 (2.640, -4.1%); ub 2.60, 2.58, 2.56 (2.580); probe 2.82;
  margin 0.56 -> 0.41 (ub 0.33); pass 95.3%.

Within-arm spread 0-1.3%; the arms do not overlap. Raw logs:
`/home/oz/projects/wt/novel-s80/proto/results/ub-phase2-sender-certificate-{s600,s5k,v2s600,v2s5k}-<k>-<arm>.log`.

Against the spec's kill rules: pass rate 100% (>= 85% senders / 90% orders), no shadow mismatch, margin saving 24.6%
(5000) / 33.7% (600) (>= 20%), engine 5.0% / 6.0% (>= 2%). Not killed. The sound arm reaches 0.04 ms/1k of the
unsound upper bound's margin; the pre-pass costs ~0.03 ms/1k; the position reads moved to the pool are not visible in
match/1k at 5000 senders (+0.004) and +0.02 at 600.

## 5b. Microbench at the fix round (e35bb1a4 code; ubench_econ binary md5 15b2ec6a..., built 12:14)

Same method as section 5 (ub.sh's env: UB_MARKETS=300 UB_RUNS=5 UB_MARKS=1 TORUS_SETTLE_STREAMED=1), ABBA order
off,on,on,off,off,on per set, both sets in one hold of the heavy lock (12:20:07-12:22:58), arm = `TORUS_S80_P2_CERT`
empty vs `1`. Values: the MEDIAN engine_ms/1k_fills of each invocation (5 runs each).

| set | arm | MEDIAN per invocation (ABBA order) | mean | delta of means | margin/1k | match/1k | engine ms/blk |
|---|---|---|---|---|---|---|---|
| UB_SENDERS=600 | off | 2.73, 2.76, 2.76 | 2.750 | | 0.558 | 0.158 | 32.27 |
| UB_SENDERS=600 | on | 2.61, 2.60, 2.61 | 2.607 | -5.2% | 0.379 (-32.1%) | 0.168 | 30.46 (-1.81) |
| UB_SENDERS=5000 | off | 2.29, 2.31, 2.30 | 2.300 | | 0.473 | 0.113 | 36.93 |
| UB_SENDERS=5000 | on | 2.21, 2.21, 2.21 | 2.210 | -3.9% | 0.365 (-22.8%) | 0.126 | 35.45 (-1.48) |

(margin/match/blk: mean of the 15 per-run values.) Certificate (on arms): 600 senders 28959/28962 senders, 11883600 /
11887200 orders (99.97%), 0 bound failures, min slack 98.76% of F0; 5000 senders 37791/37791 senders, 15174000 /
15174000 orders (100%), 0 bound failures, min slack 98.79%. fills/blk per run and the s92 top-up / sell-cut lines are
identical between arms (md5 of the sorted lines: 600 9203e3fa both arms, 5000 0056dc2c both arms). The validity bail
and the checked subtractions cost nothing visible: the gain matches a2dac3ea's (-6.0% / -5.0%) and the review's
independent run (-6.5% / -4.3%) within the round-to-round spread. Logs:
`/home/oz/projects/wt/novel-s80/proto/results/ub-phase2-sender-certificate-fix-{s600,s5k}-<k>-<off|on>.log`.

## 6. Crate tests and torus-node

`cargo nextest run --release -p torus-bridge --no-fail-fast` at a2dac3ea (release: the target dir only holds release
artifacts):
- flag unset: 1096 tests run, 1096 passed, 17 skipped (12.8 s);
- `TORUS_S80_P2_CERT=1`: 1096 passed, 17 skipped (12.3 s);
- `TORUS_S80_P2_CERT=shadow`: 1096 passed, 17 skipped (12.5 s).
(A first flag-off attempt at 11:10 failed to build: /home was 100% full, "No space left on device"; space came back
and the rerun above is clean. flaky = 0.)

`cargo build --release --locked -p torus-node` at a2dac3ea:
`/home/oz/.cargo-target-s80-phase2-sender-certificate/release/torus-node` md5 3cb731239a677b305c6ce0c3126d9301,
contains `TORUS_S80_P2_CERT`. Cell ON arm: `TORUS_S80_P2_CERT=1`; the INFO line `TORUS_S80_P2_CERT on=true mode=On`
proves it was active. Not run: cells (batch rule), clippy, the workspace gate.

Fix round: `cargo build -q --release --locked -p torus-node` at e35bb1a4 (12:23):
`/home/oz/.cargo-target-s80-phase2-sender-certificate/release/torus-node` md5 3b121c9672e872ab00d68673e3104622
(contains `TORUS_S80_P2_CERT` and the shadow-mismatch text; supersedes the a2dac3ea binary above).

## 7. Soundness notes and open items

Closed at the fix round: the invalid-order hole (review P1), the overflow panic (P1), the sharded path (P2), the `tup`
fallback (P3), the global test flag (P3), the `ub` arm and the panicking shadow (P3). Still open (below):

- The proof sketch is in section 2; tests check it empirically (differential + shadow), not formally. Open items a
  production version must close: (a) the overflow guard assumes PosSums' notional terms value a position at the same
  mark `placement_need` uses (true when the block's mark table holds the market; the 2^20 headroom covers other
  ratios but is not a proof); (b) pending stops of the sender triggering in the same block's matching would draw the
  pool (stops are not in a certified run, but older pending stops are not scanned); (c) the pool-cost bound is
  per-fill dust x a fill-count bound and per-order loss bounds, it assumes the book's charge <= loss and fill prices
  of the sender's sells <= max(best bid, band top) (bids above the band can only be older resting orders, covered
  by the best bid); (d) liquidation / ADL orders run after `execute_batch` and are not affected.
- Bench-shape dependence: the pass rate is ~100% because every bench sender holds 100M; lean real accounts would
  certify less often: in the near-margin fixture (balances 4.5k-126k against ~1.5k IM per order) 55-56 senders per
  seed fail the bound, and with the default ±50% band C_ub charges each checked sell in the anchor market the IM up
  to the band top (about half its reservation again). Not measured: the pass rate on lean real-world accounts.
- If users-position-key-index (r1 #3, -4.4% engine, margin -10%) lands first, part of this gain overlaps (both cut
  the Phase-2 position read cost).
- Not run: cells (session rule), clippy, the workspace gate.

## 8. Verdict

Fix round (e35bb1a4): all seven review issues fixed, five new tests (both review repros, invalid shapes, a slack-0
sender in the serial and sharded paths, the sharded differential), torus-bridge 1101/1101 with the flag unset, `1` and
`shadow`. Microbench unchanged in substance: engine -5.2% (600 senders) / -3.9% (5000), margin -32% / -23%,
identical fills and top-ups. Status stays built; cell_worthy left false per the batch rule (cell env
`TORUS_S80_P2_CERT=1`, binary in section 6). First build's verdict:

Built, tests green (section 4/6), microbench engine -5.0% (5000 senders) / -6.0% (600 senders) beyond the spread,
margin phase -25% / -34%, identical fills and top-ups. Cell-worthy on the numbers; per the batch rule cell_worthy is
left false for the main session to schedule (cell env: `TORUS_S80_P2_CERT=1`, binary
`/home/oz/.cargo-target-s80-phase2-sender-certificate/release/torus-node`).
