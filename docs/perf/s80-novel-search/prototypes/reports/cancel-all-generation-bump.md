# cancel-all-generation-bump (s80 #11): premise check, KILLED

Branch `s80/cancel-all-generation-bump` at `89fe9dc7` (base 29bfc705). Worktree
`/home/oz/projects/wt/novel-s80/proto/cancel-all-generation-bump`, target dir
`/home/oz/.cargo-target-s80-cancel-all-generation-bump`. Nothing pushed. No flag was built: the
candidate was killed at Step 1, the "cheapest decisive" step.

## Verdict

**Killed.** At the bench mix, the net E-wall saving per block is **-0.32..+0.59 ms**. The realistic
band is **-0.03..+0.28 ms**, which is below the 0.4 ms kill line. The bound only reaches 0.4 ms if
the maintained-sum upkeep and the match-time stale checks cost nothing. On top of that, the window
that closes the reclaim adds a **+16..42 ms** spike to one block in every 100, against an engine block
of about 28 ms. So block p99 gets worse. The idea saves nothing worth having because the bench
cancels only about 1.3 orders per (sender, book) hit. Making the per-hit work O(1) removes little:
each cancelled order still has to be reclaimed (index, seq, queue). That reclaim costs about
0.30 us of serial CPU per order, against about 1.0 us today. On top of that, every rest and fill now
pays for the upkeep.

## Premise check on 29bfc705 (node path, ubench_econ, `UB_RESIDENT_BOOKS=1`)

I extended `ubench_econ` with `UB_RESIDENT_BOOKS=1`. It runs the node's resident Classic path with
L3.1 coalesce, the P2-1 cancel-all index and the 8-worker parallel book pass. The default ubench
mode visits all 300 books serially, which the node does not do. I also added diagnostic counters to
`native_executor.rs`: the book-pass wall, its CPU summed over pool jobs, and the orders cancelled.
Raw log: `proto/results/ub-cancel-all-generation-bump-res-cpu1.log` (5 runs, seeds 0-4).

| run | phase1/blk ms | book pass wall/blk ms | book pass CPU/blk ms | par | orders cancelled/blk | pool CPU ns/order | CA/blk | visited/CA | hit/CA | engine/blk ms |
|---|---|---|---|---|---|---|---|---|---|---|
| 0 | 1.700 | 1.403 | 7.976 | 5.69 | 2914 | 2737 | 22.0 | 219.4 | 103.0 | 27.71 |
| 1 | 1.839 | 1.550 | 8.169 | 5.27 | 3016 | 2708 | 22.5 | 215.5 | 103.9 | 27.70 |
| 2 | 1.692 | 1.418 | 7.370 | 5.20 | 2847 | 2589 | 21.0 | 212.1 | 103.6 | 27.94 |
| 3 | 1.570 | 1.300 | 7.201 | 5.54 | 2767 | 2602 | 19.8 | 216.2 | 107.0 | 28.08 |
| 4 | 1.620 | 1.337 | 7.445 | 5.57 | 2964 | 2511 | 21.0 | 216.8 | 107.7 | 28.58 |
| mean | 1.684 | **1.402** | **7.632** | 5.45 | 2902 | 2629 | 21.3 | 216 | 105 | 28.0 |

MEDIAN engine_ms/1k fills = 2.42 in resident mode (runs 2.45 / 2.43 / 2.40 / 2.37 / 2.42). ub.sh's
default non-resident mode reads 2.78 (`ub-cancel-all-generation-bump-base-probe.log`): its cancel-all
visits all 300 books serially, 5.5-6.1 ms/blk.

The cost exists, but it is small. The whole cancel-all book pass is **1.40 ms/blk of E wall (5.0% of
engine)**, which is the gross ceiling for any cancel-all rewrite. That is about 2,240 hits/blk and
2,900 orders cancelled/blk, so **1.3 orders per hit**. Pool CPU is 2.6 us per cancelled order, about
2.6x the serial probe's cost per order: the parallel pass is memory-bound.

## Probe (Step 1): `cancel_all_gen_bump_probe_bench_shape`

The probe is `crates/torus-core/src/order_book/cancel_batch/many_tests.rs`, `#[ignore]`, release. Its
shape: 300 books, 600 traders, GTC limits within 5 ticks, 24 distinct cancel-all senders per block,
and the cancelled count re-placed each block (untimed, the same in both arms). The window is 100 blocks.

- **Arm A** is today's code: `plan_cancel_all_many` + `apply_cancel_all_many` per book, plus the
  per-order margin term.
- **Arm B** is the generation bump:
  - per (sender, book), O(1): the `trader_orders` entry is moved to a stale list, plus a gen bump,
    an RO forget and a sum read;
  - per block, a top purge (stale fronts of the best levels popped with their index removal, so
    BBO stays live);
  - at window close, the deferred index/seq removal and journals, then one `retain` per touched level;
  - `PROBE_UPKEEP` maintained-sum updates per block (default 21k = about 11k fills + 10k rests) in a
    separate per-book map.

After every window the probe asserts that both arms' books are **identical**: borsh bytes, order_seq,
trader_orders, order_index, levels and next ids. That passed at every depth. So lazy reclaim before
the window-close encode does give byte-identical book blobs.

Raw logs: `proto/results/probe-cagb-r2.log` and `probe-cagb-r3.log` (repeats within 3%). The earlier
`probe-cagb-r1.log` is from a prior attempt that had no upkeep term.

| mode | resting | upkeep | hits/blk | cancelled/blk | A plan | A apply | A margin | A total | B bump | B purge | B upkeep | reclaim/window (=/blk) | B total | net CPU/blk |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | 50k | 21k | 1590 | 1978 | 1.064 | 0.917 | 0.039 | 2.020 | 0.501 | 0.124 | 1.899 | 60.50 (0.605) | 3.128 | **-1.109** |
| 0 | 104k | 21k | 2773 | 4230 | 2.604 | 2.058 | 0.079 | 4.741 | 0.802 | 0.178 | 2.330 | 130.86 (1.309) | 4.619 | +0.122 |
| 0 | 400k | 21k | 5176 | 16124 | 10.812 | 9.195 | 0.262 | 20.269 | 1.290 | 0.281 | 2.639 | 479.56 (4.796) | 9.006 | +11.263 |
| 0 | 1.1M | 21k | 6373 | 44452 | 31.148 | 30.622 | 0.645 | 62.415 | 1.697 | 0.375 | 2.499 | 1372.26 (13.723) | 18.294 | +44.121 |
| 0 | 50k | 0 | 1590 | 1978 | 1.020 | 0.887 | 0.039 | 1.946 | 0.470 | 0.122 | 0 | 59.88 (0.599) | 1.190 | +0.755 |
| 0 | 104k | 0 | 2773 | 4230 | 2.547 | 2.034 | 0.079 | 4.661 | 0.737 | 0.172 | 0 | 129.76 (1.298) | 2.207 | +2.453 |
| 0 | 50k | 21k (rep) | 1590 | 1978 | 1.068 | 0.916 | 0.039 | 2.024 | 0.504 | 0.121 | 1.878 | 60.16 (0.602) | 3.104 | -1.081 |
| 0 | 104k | 21k (rep) | 2773 | 4230 | 2.620 | 2.053 | 0.078 | 4.752 | 0.788 | 0.178 | 2.332 | 130.45 (1.305) | 4.603 | +0.150 |
| 3 | 50k | 0 | 1590 | 1978 | 1.045 | 1.019 | 0.039 | 2.103 | 0.479 | 0.130 | 0 | 68.37 (0.684) | 1.293 | +0.810 |
| 3 | 104k | 0 | 2773 | 4230 | 2.644 | 2.408 | 0.079 | 5.131 | 0.762 | 0.193 | 0 | 150.02 (1.500) | 2.456 | +2.675 |

All values are ms of serial CPU per block. Mode 0 is Classic, the bench mode; mode 3 is the
chunked digest.

The 400k and 1.1M rows do not describe the bench. In those runs the cancel-all senders own all the
resting depth, so 16k-44k orders are cancelled per block. In the real cells, the deep preload belongs
to other accounts, and E's cancel-all is 2.28 / 2.55 ms/blk (std / deep, measured.md). The ubench mix
(about 50k resting, 1.3 orders/hit) is the relevant shape. It sits between the 50k and 104k probe rows.

## Net E-wall estimate at the bench mix

I scaled the probe's per-unit costs to the ubench mix (2,902 cancelled, 2,240 hits per block):

- A serial is 1.0 us/order, so **2.90 ms**.
- B: the bump is 0.31 us/hit (0.69 ms), the purge 0.12 ms, and the reclaim 0.30 us/order
  (0.88 ms/blk amortized). B therefore costs **1.69 ms, 58% of A**, and the most it can save is
  **42% of A**.
- Upkeep in a separate map is 21k x about 89 ns = 1.88 ms, **65% of A**.
- The match-time stale check (one gen/watermark probe per maker visited, about 11.3k/blk) is not in
  the probe.

E wall saved = 1.402 ms (measured book-pass wall) x (net fraction). This assumes B's work runs on the
pool as efficiently as A does (same 2.6x per-unit inflation, par 5.45):

| assumption | net fraction of A | E wall saved/blk |
|---|---|---|
| upkeep and match checks free (upper bound) | +0.42 | **+0.59 ms** |
| upkeep and checks co-located in the `trader_orders` entry, 20-40 ns marginal x about 32k events = 0.64-1.28 ms (22-44% of A) | -0.02..+0.20 | **-0.03..+0.28 ms** |
| upkeep as probed (separate map), no check cost | -0.23 | **-0.32 ms** |

**Window-close spike:** the reclaim is 88 ms of serial CPU per window at the mix. Run as a pool pass,
that is 88 x 2.63 / 5.45, about **42 ms**, or about 16 ms if it parallelized at serial efficiency.
Either way it lands on one block per window, against an engine block of about 28 ms. The ranked
write-up estimated +13-28 ms. Running the reclaim incrementally would not reduce its total; it would
only spread it out. The books belong to E, so the reclaim cannot run off-E.

Kill line: the net saving must reach >= 0.4 ms/blk. It does not. It gets there only if both the
upkeep and the stale checks cost under about 11 ns per event, i.e. only if every update lands on a
cache line that was already touched. Partial fills (a maker with no `trader_orders` touch today) and
the per-event tiered-margin recompute rule that out.

## What Step 2 would have needed (design, not built)

I record this so the cost is clear if the idea comes back:

- **Stale test.** Use a per-(book, trader) watermark: stale iff `order.id < watermark[trader]`. Global
  order ids are monotonic, and `modify_order` keeps the id, so no new `Order` field is needed and the
  borsh layout does not change. Co-locate the watermark and the reserved-margin sum in the
  `TraderOrders` entry, which a bump must keep rather than drop.
- **Readers that must filter.** Matching fronts (`match_at_level`: STP, RO cut, fill), BBO /
  `best_bid` / `best_ask`, `would_cross`, the band reference, the modify cross, `get_order` / by-id
  cancel / modify (the RO sweep), the cloid duplicate check, `open_order_count(s)` / max-orders,
  pending stops, and the window-close encode (reclaim first). That is about 110 public `OrderBook`
  methods to audit. A miss is a fork on a restarted node, because a reloaded book holds no stale orders.
- **Margin.** The sum is maintained per rest, per partial fill (a tiered, non-linear recompute of the
  order's term) and per full fill. The A5 identity must hold across tier changes, so the sums must be
  recomputed on a margin-config change.
- **Consensus visibility.** None, provided every reader filters. The probe shows that the
  reclaim-before-encode blob is byte-identical.

The ranked estimate of 8-12 person-days stands. In this 2 h box it was not feasible, and the measured
gain does not justify it.

## Side finding (for a different candidate)

In arm A, `plan` costs as much as `apply` (1.02 vs 0.89 ms at 50k; 2.55 vs 2.03 at 104k). Per target,
the plan does a `partition_point` binary search with an `order_seq` HashMap probe at every step. B's
reclaim (index/seq removal plus one retain per level) costs 0.30 us/order against A's 1.0. So an
**eager** batch (one retain per touched level per run, seq read from the location rather than probed)
might get part of the 42% without any generation semantics. Its ceiling is the same 1.40 ms/blk
(5% of engine), so at best about 0.3-0.5 ms/blk. Prior art to check first: `cancel-all-in-place-compaction`
(inconclusive).

## Tests

- `cargo nextest run -p torus-bridge -E 'test(perf_equivalence_golden) | test(cancel_all)'`: 29/29
  passed with the diagnostic counters in (no flag exists; the counters change no behaviour).
- Probe `cancel_all_gen_bump_probe_bench_shape` (release, `--ignored`): passed at 50k / 104k / 400k /
  1.1M in mode 0 and at 50k / 104k in mode 3. A and B books were borsh-identical after every window.
- ubench_econ (release, resident mode): 5/5 runs ok.

## Files

- `crates/torus-core/src/order_book/cancel_batch/many_tests.rs`: the probe.
- `crates/torus-bridge/src/native_executor.rs`: diagnostics (`cancel_all_book_pass_ns`,
  `cancel_all_book_pass_cpu_ns`, `cancel_all_orders`, and two process-wide atomics for the pool jobs).
- `crates/torus-bridge/tests/ubench_econ.rs`: `UB_RESIDENT_BOOKS=1` and the `UB80` breakdown line.
