# s80 prototype exec-block-arena: recycle the engine's per-market scratch maps (TORUS_S80_EXEC_ARENA)

Branch `s80/exec-block-arena` (worktree `/home/oz/projects/wt/novel-s80/proto/exec-block-arena`), base 29bfc705,
commit ea886f5a. Target dir `/home/oz/.cargo-target-s80-exec-block-arena`. Nothing pushed.

## Verdict

**Built, flag works, gain inside the noise: not cell-worthy.**

- Tests: green with the flag off and on (golden 5/5 in all four flag x L2.1 combinations, exec_arena_tests 5/5 off
  and on, torus-core 421/421 off and on, torus-bridge full suite: see "Tests").
- Engine proxy (ubench_econ, ABBA, 3+ rounds per arm): about -1% engine ms/1k fills with the flag on, under both
  allocators. The run-to-run spread is 3-5%, and the gate is >= 2% beyond the spread.
- The premise ceiling explains the small gain. Recycling the per-batch maps removes their rehash (~1.5% of engine
  cycles on the pool workers). The page faults the write-up counted on stay: under jemalloc, the node's allocator,
  they come from the exec thread, which these maps do not touch.

## Premise check on 29bfc705

The write-up claims ~15 maps/Vecs in `execute_batch_phases` built from empty twice per block: 4-5% of exec
(-1.3..-1.7 ms/blk), of which ~0.4-0.6 ms is page faults ("60 faults per 1k fills", measured.md O10, 8u deep
profile, exec thread including save_books). The skeptic correction is 0.6-2.5% of exec.

Measured in the engine proxy (`ubench_econ`, 300 markets, UB_MARKS=1, TORUS_SETTLE_STREAMED=1, 6 measured blocks x 5
runs, ~11.3-11.9k fills/blk). The timed window is oracle + `execute_batch` + liquidations, with perf enabled only for
that window through a `--control fifo` hook added to the ubench:

1. **The allocator matters.** The ubench test binary runs on glibc malloc, while the node has jemalloc compiled in.
   With `LD_PRELOAD=/usr/lib/libjemalloc.so` (Arch 5.3.1, the node's bundled version), the base engine reads
   2.53 ms/1k fills against 2.85 on glibc (-11%; `ub-exec-block-arena-pre-{je,glibc}.log`). Allocation A/Bs are
   therefore run under both allocators, and the jemalloc arm is the node-faithful one.
2. **Faults are small.** `perf stat -e page-faults:u` over the window, jemalloc, 5 runs x 6 blocks: 6143 faults,
   ~205/blk, ~17.8 per 1k fills (`results/eba/exec-block-arena-stat-pre.out`). That is ~0.2 ms/blk at most, <1% of
   the 29.5 ms engine block. On glibc at steady state (runs 1-4), the window takes 3.9 faults per 1k fills process-wide
   and 0.5 on the exec thread. The cell's "60 per 1k" includes the exec thread's non-engine work (save_books etc.),
   which the arena would not touch.
3. **Where rehash/alloc time is** (window-only flat cycles:u profile, jemalloc, `results/eba/rep-children.txt`,
   `win-pre-flat.txt`; the whole-run profile `rep-children.txt` agrees): pool threads take 62% of engine cycles and
   the exec thread 38%. `reserve_rehash` of the per-batch pool maps is 1.52% of engine cycles:
   - `Address->TraderMargins` (`AccountMargins`) 0.46%;
   - `Address->FixedPoint` (`ReduceOnlyPositions` / `MAKER_FREE`) 0.45%;
   - `(Address,MarketId)->Option<Position>` (the plan `PositionCache` map) 0.40%;
   - its dirty set `(Address,MarketId)` 0.21%.

   Pool-thread jemalloc malloc+free adds 1.14%. Exec-thread `reserve_rehash` is 0.26% (largest:
   `u64->Projection` 0.16%). jemalloc is 1.78% in total
   and libc memmove 5.1% (mostly not map growth). Frame-pointer-less release code gave no user call chains
   (`--call-graph dwarf` produced none), so the attribution is by leaf symbol and map type.

**Premise verdict.** The cost exists but is smaller than the write-up says. The removable part is the pool-side map
growth, ~1.5% of engine cycles (plus some of the 1.78% jemalloc). That is a ceiling of about 1.5-2% engine, at the
bottom of the skeptic's range. The exec-thread arena (bump allocator, lifetime changes) is not justified by 0.26%
rehash. Built the cheapest decisive version: per-market pool maps cleared and reused, not dropped.

## Design (as built)

Flag `TORUS_S80_EXEC_ARENA` (`parse_exec_arena`: `1|true|on`; read once per process via `OnceLock`, INFO line
`TORUS_S80_EXEC_ARENA on=<bool>` at the first engine call; `pin_exec_arena` is a process-wide test pin, because the
pool workers read it too). Default off = 29bfc705 behaviour byte for byte (every changed site is `if flag {new} else
{old}`).

New module `crates/torus-core/src/recycle.rs`: `Recycler<T>`, a bounded process-wide LIFO free list (`Mutex<Vec<T>>`,
1024 values; hit/miss counters; `torus_core::recycle::stats()` sums them). A map is cleared when it is given back, so
every value handed out is empty. A map whose capacity exceeds `MAX_KEPT_CAPACITY` = 65,536 entries is dropped, not
kept. The free lists are process-wide because the maps are built on the match/plan pool workers and released on the
exec thread.

Sites (29bfc705 line numbers in `crates/torus-bridge/src/native_executor.rs`):

| map | built (base line) | released | recycled by |
|---|---|---|---|
| pass A's plan `PositionCache` (map + dirty set) per market | `compute_market_settle_plan` `PositionCache::new()` (11711) | after the end-of-call position flush, from the L2.2 `pos_runs` (9886-9896) | `PositionCache::recycled()` / `recycle()` (`torus-core/src/position.rs`) |
| `ReduceOnlyPositions` per book per batch | `match_setup` (13044) | `absorb_matched` `clear_reduce_only_positions` (10436) | `ReduceOnlyPositions::recycled()` / `OrderBook::recycle_reduce_only_positions` |
| `AccountMargins.traders` per book per batch | `match_setup` (13065) | `absorb_matched` `clear_account_margins` (10442) | `AccountMargins::new_recycled` / `OrderBook::recycle_account_margins` |
| pool worker thread-local `MAKER_FREE` | `*c = (b.id, HashMap::default())` on a new batch id (3068) | same | `clear()` in place instead of a new map |

Byte-identity argument: a recycled value differs from a fresh one only in capacity and (for the per-instance-seeded
maps) the hasher seed it drew when first built. None of these maps reaches an output in iteration order: they are
point-read/written, or their keys are sorted before writes (`flush_with_runs` sorts). A fresh map's iteration order is
already random per instance, so recycling adds no new order dependence. `recycle()` resets every `PositionCache` field
(map, dirty, key-market range) and `new_recycled` takes all other `AccountMargins` fields from `new()`.

Not covered (by design, after the premise check): the exec-thread structures the write-up listed (flatten Vec,
`SenderFold`, `BalanceCache`, `market_batches`, ...). The window profile put exec-thread `reserve_rehash` at 0.26% of
engine cycles, too small to pay for the lifetime changes a bump arena would need there.

## Diff summary (29bfc705..ea886f5a, 7 files, +1122 / -6)

- `crates/torus-core/src/recycle.rs` (new, 110): `Recycler`, `MAX_KEPT_CAPACITY`, `stats()`, a unit test.
- `crates/torus-core/src/position.rs` (+83): `POSITION_CACHES`, `PositionCache::recycled/recycle`, unit test.
- `crates/torus-core/src/order_book.rs` (+115): `REDUCE_ONLY`, `MARGIN_TRADERS` lists, `recycled` constructors,
  `OrderBook::recycle_*`, unit test.
- `crates/torus-core/src/lib.rs` (+1): `pub mod recycle`.
- `crates/torus-bridge/src/native_executor.rs` (+79/-6): flag, pin, the four sites.
- `crates/torus-bridge/tests/exec_arena_tests.rs` (new, 679): the equivalence harness (below).
- `crates/torus-bridge/tests/ubench_econ.rs` (+61): per-run minor faults in the timed window (process and exec
  thread, `/proc/{self,thread-self}/stat` field 10), a `perf --control fifo` hook (`UB_PERF_CTL`/`UB_PERF_ACK`) that
  enables perf only for the timed window, and a `UB recycle(hits,misses)` line.

## Tests

Written first (session 1), then implementation. Re-run in session 2 on ea886f5a (target dir above, outputs in
`/home/oz/projects/wt/novel-s80/proto/results/eba/verify/`):

| suite | flag off | flag on |
|---|---|---|
| `perf_equivalence_golden` (binary filter), TORUS_SETTLE_STREAMED=0 | 5/5 | 5/5 |
| `perf_equivalence_golden` (binary filter), TORUS_SETTLE_STREAMED=1 | 5/5 | 5/5 |
| `exec_arena_tests` (5) | 5/5 | 5/5 |
| torus-core (`cargo nextest run -p torus-core`) | 421/421 (14 skipped) | 421/421 (14 skipped) |
| torus-bridge (`cargo nextest run -p torus-bridge`) | 1094/1094 (17 skipped) | 1094/1094 (17 skipped) |

Note: `-E 'test(perf_equivalence_golden)'` matches **0** tests (the golden test names do not contain the binary
name; nextest exits 4, "no tests to run"). Use `-E 'binary(perf_equivalence_golden)'`.

`exec_arena_tests.rs` (THE contract: the flag changes capacities only): a recording, fault-injecting backend over
the block overlay (the `settle_no_merge_tests.rs` harness). Each block is the block's two `execute_batch` calls on one
overlay, flushed to the DB afterwards. For every combination of pass-A cap 1/2/8, L2.1 streamed off/on and L2.2
no-merge on/off, the flag-on run must equal the flag-off run in:
- the backend write sequence per call;
- every result field, gas, fills and extras;
- the pending native set and the fatal error;
- the DB and the native state root.

Each flag-on arm runs twice in a row, so the second run starts from maps the first left on the free lists. The on
arm must have recycled (hits > 0 when the parallel settle ran). Streams:
- clean, 24 markets x 8 blocks;
- a second seed, 6 wide markets x 6 blocks;
- a one-shot position read failure in the last block's pass A (plan fallback, then fail-stop);
- a position write failure mid-flush in the last block (fatal, runs recycled after a failed flush).

`parse_exec_arena_is_off_unless_set`. Unit tests in torus-core:
- `recycle::tests` (LIFO, cap);
- `position::recycle_tests` (a recycled cache is empty in every field and keeps its capacity; an oversize cache is
  not kept);
- `order_book::recycle_tests` (the book after `recycle_*` equals `clear_*`; handed-out maps are empty).

Activation proof in the ubench (new binary, `UB recycle(hits,misses)` line): flag off (0, 0); flag on
**(206100, 900)**, i.e. 99.6% of the per-market map constructions reused a kept map (900 misses = 3 lists x 300
markets on the first batch).

## Microbench (ubench_econ, ABBA off,on,on,off,off,on; ub.sh defaults, 5 runs per invocation; MEDIAN engine ms/1k fills)

| set | binary | off medians | on medians | mean off | mean on | delta |
|---|---|---|---|---|---|---|
| glibc (07:14-07:26) | 07:10 build | 2.74, 2.77, 2.80 | 2.73, 2.72, 2.76 | 2.770 | 2.737 | -1.2% |
| jemalloc preload je (07:31-08:00) | 07:10 build | 2.56, 2.56, 2.61 | 2.53, 2.55, 2.58 | 2.577 | 2.553 | -0.9% |
| jemalloc preload je2 (09:00-10:08) | ea886f5a build | 2.58, 2.58, 2.53 | 2.58, 2.57, 2.56 | 2.563 | 2.570 | +0.3% |
| jemalloc pooled (je + je2) | | 6 medians | 6 medians | 2.570 | 2.562 | -0.3% |

Pre (base behaviour, same binary): glibc 2.85, jemalloc 2.53. Run-to-run spread is 3-5%; every delta is inside it.

Per-phase means over all runs (ms/1k fills): the phase the change targets moves as expected, and the rest does not.
`match` (match_setup, where AccountMargins/ReduceOnly are built) drops: glibc 0.188 -> 0.145, je 0.182 -> 0.167,
je2 0.177 -> 0.152. `settle` (plan PositionCache) is unchanged: 0.794/0.786, 0.700/0.707, 0.695/0.708. `margin` is
unchanged as well.

Minor faults in the timed window per 1k fills, steady state (runs 1-4, n=12 per arm), process / exec thread:

| set | off | on |
|---|---|---|
| glibc | 3.9 / 0.5 | 3.1 / 0.1 |
| je | 17.9 / 12.9 | 16.6 / 13.1 |
| je2 | 16.9 / 11.6 | 14.1 / 9.1 |

Under jemalloc most faults are on the exec thread, where these maps do not live. Recycling the pool maps cuts the
pool-side faults by ~1-3 per 1k fills, too few to show in engine time.

Activation: flag-on logs `UB recycle(hits,misses)=(206100, 900)`, flag-off logs (0, 0) (je2 set).

Raw logs: `/home/oz/projects/wt/novel-s80/proto/results/ub-exec-block-arena-{pre,glibc-N,je-N,je2-N}-*.log`.
Summaries: `results/eba/abba-{glibc,je,je2}.txt`. A per-IP page-fault attribution (`results/eba/scripts/faults.sh`,
perf record -e page-faults:u -c 1, window only, jemalloc) was still queued on the build lock when this stage ended,
so its output (`results/eba/flt-{off,on}-top.txt`) is not part of this report.

No cell was run: the gain is below the 2%-beyond-spread gate, and the target metric (engine ms, faults) shows up in
the proxy.

## Risks / follow-up

- The remaining jemalloc faults are exec-thread ones, so they belong to allocator policy (thp-heap / decay) or to
  exec-thread buffer reuse, not to this change.

## Risks / production notes

- Retained memory: a free list keeps up to 1024 maps of up to 65,536 entries each. Each kept map keeps the largest
  capacity it ever served (LIFO reuse ratchets capacities up), so an adversarial block shape could pin a large heap
  footprint (for `PositionCache`, ~160 B per entry). A production version needs a byte budget (or `shrink_to` on
  give) rather than an entry cap.
- The free lists are global `Mutex`es: one lock per take/give, ~600 per batch at 300 markets, uncontended in
  practice (the pool takes and the exec thread gives at different phases).
- The flag reads the env once. The INFO line appears at the first engine call, not at process start.
