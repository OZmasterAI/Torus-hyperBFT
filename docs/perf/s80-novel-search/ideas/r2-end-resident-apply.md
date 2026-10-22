# r2 lens end-resident-apply: ideas (novel-s80)

Base: repo HEAD `2d2f3ed9` (main `eb993e3f` + batch10 merge), read-only. Nothing built or run. Line refs are HEAD.
Round 1 already holds #8 end-resident-gate ((0) hand off before upkeep, (A) borrowed frozen delta, (B) parallel
read-only re-value + serial install) and the killed streamed-end-resident (16 shards published in order by one
serial worker). The ideas below avoid both.

## Key finding: the wait is a big-block tail, not a mean-cost problem

Source: `~/bench-results-matched/ozarchy-9j2-std-r4-CL21/metrics-after-val0.txt` (6,939 native blocks; r2 and deep r4
agree), plus `proto/results/s80-base-b10r2` (h2h base, 2,781 blocks).

| histogram (per native block) | <= 1 ms | > 4 ms | > 8 ms | > 16 ms | > 32 ms |
|---|---|---|---|---|---|
| exec chain | 47.9% are <= 4 ms | | | | 18.5% are > 64 ms |
| end_resident worker | 63.4% | 25.2% | 16.8% | 8.1% | 2.5% |
| users apply | 68.0% | 21.1% | 10.8% | 4.3% | 0.4% |
| end_resident_wait (E) | 76.0% | 13.6% | 6.2% | 1.2% | 0.1% |

- Wait buckets: <=0.5 ms 5,005; (0.5,1] 268; (1,2] 316; (2,4] 403; (4,8] 521; (8,16] 342; (16,32] 74; (32,64] 10.
  Bucket-midpoint sum: the 13.6% of blocks that wait > 4 ms carry ~78% of the 10.63 s; > 2 ms (19.5%) carry ~88%.
  Deep r4: 10.7% of blocks > 4 ms carry ~80%.
- Exec blocks are bimodal (p50 3.9 ms, p95 250 ms in b10r2). The worker time follows the block size. A wait of 8-64 ms
  is only possible when the predecessor's apply took 10-66 ms, i.e. a big block N-1 followed by a small block N whose
  prefix (ticket, verify, replay, sort: ~1-2 ms when small) hides little of it.
- Model: wait(N) ~ max(0, T(N-1) - P(N)). Any idea that cuts the *mean* worker time by a fixed amount barely touches
  the tail. What matters is how T scales for big blocks (parallel fraction) and how much of block N runs before the join.
- Worker composition (q8re r1 w1 `torus-end-resid` profile, `ozarchy-q8re-run/analysis/...-r1-ONP-w1.txt:577-636`):
  `Users::apply` 61.9% incl., `follow` 57.3% incl. / **38.2% self**, `resident_delta` copy 23.6%, carry closure 12.3%
  (build_sums 8.1%, position_terms 6.8%), decode 5.5%, drop NativeBatchResult 3.3%, malloc/free 8.9%.
- Shape: Users 1.605M rows / 214 MB std (2.5M / 262 MB deep), ~5k active traders x ~300 positions = ~33.6 KB
  `Vec<Position>` per trader.

Conversion used: 9j2 std CL21 exec block 38.35 ms; elasticity 0.3 for matched/s; blk/s at the s67.4 ratio
(block_ms -10% -> blk/s +9.5%).

---

## 1. users-sharded-parallel-apply: address-range shards of `Users`, applied in parallel on the idle exec pool

**Mechanism.**
- Split `Users` (users.rs:166-200) into S = 16 `UsersShard`s keyed by the top 4 bits of the trader address. Each shard
  owns its `map`, `opaque`, E2 `traders` BTreeSet, C2 `holders` and row/byte counters. Only `other` (rows under no
  trader, normally empty) stays global.
- Every trader's apply touches only its own shard: `follow` / `reload` (users.rs:539-673) touch one record, its
  holder entries, its trader-set entry and the counters. So the per-shard apply is today's code on a subset, and the
  result is bit-identical.
- `EndResidentJob::run` (native_executor.rs:5381-5440) becomes:
  1. a serial prepass: route the key-sorted delta into shard ranges (`partition_point` on the positions slice; one
     pass over balances, where the 20-byte keys and the `cvlm` run are both address-sorted). Keys under no trader
     go serially to `other`. Split the carry memo by shard (`into_carry`, :2463).
  2. `exec_pool::run_jobs` (exec_pool.rs:48) over 16 jobs, each holding `&mut UsersShard`, its delta ranges and its
     `SumsCarry` (shared `&BlockMarksState` / `DenseTiers`). Each job runs `follow`, `trader` and `finish`.
  3. Sum the counters, gauges and `set_source`.
- The exec pool is idle in exactly this window: block N-1's pool phases are over, and block N's start after the join.
- Readers add one shard index: `get`, `sums`, `TypedRows`. `traders_after` and `position_range` (:748, :1293) chain
  the shards in index order, which is still ascending. `holders_with` unions the 16 sets before its existing sort.
- Optional (b): the worker thread claims shards itself. E at `settle_rows` (:4684) claims the remaining shards
  instead of blocking, so progress never depends on preempted pool threads.

**Expected gain.**
- T' = T x (0.1 serial + 0.9/k).
- Applying the wait histogram with P ~ 2 ms for small successors:
  - k_eff = 4 (16 jobs on a loaded host): wait 1.53 -> ~0.25 ms/blk;
  - k_eff = 2 (worker + E-assist only): ~0.6 ms/blk.
- So E -0.95..-1.3 ms/blk, which is -2.5..-3.4% of the block.
  - matched/s +0.75..+1.0%;
  - native blk/s +2.4..+3.2%;
  - tiny blocks after big ones lose their 8-64 ms stall, which helps the exec-age tail.
- The same model for r1 #8(B) (parallel fraction ~0.4) gives ~0.86 ms/blk. This idea adds ~0.6 ms/blk beyond it.
- Side benefit: the Users rebuild and map growth (rehash) run per shard: 1/16 of the size, and parallel.

**Risks.**
- A broad refactor of users.rs (1,628 lines) and its readers.
- The Phase 5 groundwork (`UsersMut`, `Dirty`, :838-1160) must follow the split. Phase 5 itself is untouched.
- The last shard can be preempted (s92: 15-38% of pool waits). Use 16 shards per 4-8 workers, plus E-assist.
- Correctness gates:
  - `users_equal_decoded_rows_after_random_deltas`;
  - `traders_after_equals_the_walk_over_r_and_pending`;
  - the holder-list tests;
  - R == DB dumps;
  - `end_resident_worker_tests`;
  - `perf_equivalence_golden`.

**Effort.** 5-8 person-days.

**Prototype (<= 3 h).** Add an `#[ignore]` arm next to `ubench_trader_positions_apply` (users_tests.rs:1127).
1. Bench shape: `UB_TP_TRADERS=5000 UB_TP_MARKETS=300 UB_TP_DENSITY_PCT=95 UB_TP_TOGGLE_PCT=3`, with
   `UB_TP_WRITES` set to 12000 (mean block) and to 60000 (big block).
2. Untimed setup:
   - build 16 `Users` from address-range-filtered rows;
   - split each block's writes into 16 overlays, so each shard gets its own `own_pending_delta`.
3. Time `exec_pool::run_jobs(16 x Users::apply)` against one serial apply of the whole delta, ABAB n=4. Run once
   on an idle host and once under `stress-ng --cpu 24`.
4. Assert that the concatenated shard `encode_rows` equals the serial dump after every block.
5. Kill if any of these holds:
   - speedup under 2.5x at 60k writes under stress;
   - speedup under 1.5x at 12k writes.

## 2. users-apply-sorted-merge: one pass, no hashing, no per-write full binary search

**Mechanism.** These are exact edits inside `Users::apply` / `follow` (users.rs:487-640):
- (a) **E2 trader set on transitions only.** :621-628 run `traders.insert(t)` / `remove(&t)` on every keyed trader in
  every block, as a full B-tree descent over ~5k addresses. Instead, record `had = !positions.is_empty()` before the
  writes and touch the set only when `had != has`. For a regular trader, set membership equals non-empty positions.
  This is the same appear/disappear rule the s29 fix gave the C2 holders.
- (b) **One map probe.** :578-579 do `map.get(&t)` for the footprint and then `map.entry(t)`. Take the footprint from
  the entry instead (an `or_default` record has footprint (0,0)).
- (c) **Galloping search inside a group.** The group's keys and `positions` are both market-ascending. Search from
  the previous index (gallop, adjusted after `insert` / `remove`) instead of a fresh 9-probe `binary_search_by_key`
  over the 33.6 KB Vec per write (:595, :605).
- (d) **Merge-join balances.** The `balances` HashMap (:490-498 build, :520 remove, :531 leftover loop) becomes a
  three-cursor merge of the positions groups, the 20-byte balance keys and the `cvlm` run, all address-sorted.

**Expected gain.**
- follow self is 38% of the worker. (a)+(b)+(c)+(d) take an estimated 20-40% off users apply (-0.6..-1.2 ms per
  mean block), so T' = 0.71-0.85 T.
- On the wait histogram: 1.53 -> 0.91-1.21 ms/blk, i.e. **-0.3..-0.6 ms/blk** on E.
  - matched/s +0.25..+0.5%;
  - blk/s +0.8..+1.5%.
- It composes with #1, because each shard gets cheaper.

**Risks.**
- The gallop cursor must track `insert` / `remove` shifts.
- The (a) invariant must hold after `reload` (opaque <-> regular). Gate on the existing tests and a debug check that
  the set equals the traders with non-empty records.
- Overlaps r1 #3 (SoA keys), which would also shorten (c).

**Effort.** 1.5-2.5 person-days.

**Prototype (<= 3 h).** In an s80 worktree, put the four edits behind one flag.
1. Run `ubench_trader_positions_apply` with the bench-shape env above, at 12k and 60k writes, ABAB n=4. Run each
   edit alone, n=2, for attribution.
2. Run `cargo nextest run -p torus-bridge -E 'test(users_)|test(holder)|test(traders_after)|test(sums_cache)'`
   with the flag on and with it off.
3. Kill if users apply ms/blk improves less than 15% at 12k writes.

## 3. resident-lazy-users-join: hand back R, marks and sums_version first; join Users at the first position/balance read

**Mechanism.**
- Today `begin_resident` (app.rs:5804, native_executor.rs:5041) joins the whole job through `settle_rows` before
  `new_env`.
- Split the slot:
  - The worker first sends part 1: R's raw rows applied (0.06-0.08 ms), `marks` (moved unchanged), `sums_version`
    (= `carry.version`, known at job start) and the height.
  - It sends `Users` after the apply, into a once-cell.
- R's source for slots 0-1 and `BlockSums.records` hold a `LazyUsers` handle. It blocks on first use, then costs one
  atomic load per access.
- Then everything before the first positions/balances read overlaps the apply:
  - `new_env` (0.14 ms);
  - `begin_block_oracle` (app.rs:5982; 0.68-0.83 ms; it reads oracle, governance and staking rows only,
    native_executor.rs:15843-15930);
  - the flatten / listed / cloid passes;
  - the whole cancel-all book pass. `exec_cancel_all_run` (:14030-14140) visits the books first and only then calls
    `release_order_margin` (:13193), which is the first balance read.
- `end_resident_take` takes Users back out of the cell, as today.

**Expected gain.**
- Extra overlap: ~1.0-1.2 ms for small successors (the tail case), ~2.4 ms for blocks that start with a cancel-all run.
- E[min(wait, X)] over the histogram gives **-0.25..-0.5 ms/blk**: matched/s +0.2..+0.4%, blk/s +0.6..+1.2%.
- It shrinks to ~0.1-0.2 ms on top of #1. It is exact: only the wait moves.

**Risks.**
- A hidden early reader of the two CFs would make the gain 0, never negative.
- The type change touches R's source, `AccountReader` and typed loads.
- The panic and failed-worker paths must leave the lazy cell failed, so the block rebuilds or fail-stops as today.
- The L3.1 close `try_lock` (app.rs:4320) is unaffected.

**Effort.** 3-4 person-days.

**Prototype (<= 3 h).** Instrumentation only, in a 120 s local cell on `best` (`proto/cell.sh`).
1. Set a thread-local `Instant` at `begin_resident`.
2. Record the first `Users::get`, `balance`, `position`, `ResidentSource::get` or `range` per block, along with that
   block's `end_resident_wait`.
3. Kill if the mean span for blocks that waited over 1 ms is under 0.8 ms.

---

## Considered and refuted

- **Double-buffered Users (left-right).** Block N needs state N-1, so a second replica removes no dependency. It pays
  only if the writes are applied before the freeze, which needs a mid-block freeze of the R-CF pending (an overlay
  read-path change). It also doubles apply CPU and adds 214/262 MB RSS.
- **Lagged Users + parent-layer read-through for small blocks.** The 2,048-account liquidation window runs every
  block, and 58% of it is dirty (liq-tail probe). Those traders lose their carried sums, which costs ~5 ms more on E
  per block.
- **Demand-driven materialization** (a pending table with a OnceLock per dirty trader). It adds a probe to every
  Users read, +0.6-1.5 ms on E at 20-30k reads per block, unless the table is folded in, which again needs exclusive
  access. Net gain is at most #1's.
- **Persistent (HAMT) Users.** It costs 2-3 more cache misses per E read, and E is the bottleneck.
- **Deferred carry via a pre-seeded next-block memo.** The carry is only ~12% of T, it needs a concurrent sums table,
  and #1 parallelizes it anyway.
- **Typed delta from the cache flush.** Decode is ~5.5% of the worker, and the change touches `flush_all` (the
  Phase 5 area).

Ledger greps used: resident, Users::apply, sums carry, double buffer, persistent map/HAMT/cow, get_mut, BlockSums,
into_delta/FrozenPending, trader index/holders, shard, help/steal/assist, parallel apply, prefetch, lagged,
big/small block, btreeset, gallop, merge join, transition, traders set, lazy/on-demand, first access/join point/gate.
Memory: 0de901ec (s104: residual apply cost blamed on per-open/close BTreeSet ops; led to the holders HashSet fix,
not the E2 trader set), 28ea31fd (c2-set-holder review), f07f0025 (step 2 worker).
