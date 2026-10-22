# r1 lens: exec-stage-pipeline (novel-s80)

Base: main `eb993e3f`, read-only. The per-block numbers come from the 9j2 r2 CL21 cells, metrics-after val0-2
(`~/bench-results-matched/ozarchy-9j2-{std,deep}-r2-CL21/metrics-after-val*.txt`), computed as sum / count of
`torus_exec_*_seconds`. Exec chain is 35.8-36.8 ms/blk.

| timer (ms/blk) | std val0 / val1 / val2 | deep val0 / val1 / val2 |
|---|---|---|
| exec chain | 36.50 / 36.75 / 36.57 | 35.99 / 35.87 / 35.77 |
| liquidation step | 3.49 / 3.44 / 3.43 | 3.33 / 3.32 / 3.29 |
| end_resident (worker) / users apply | 3.84 / 2.42 | 3.55 / 2.23 |
| end_resident_wait (on E) | 1.43 / 1.59 / 1.40 | 1.38 / 1.36 / 1.34 |
| match (E waits on pool) + pass A | 2.53 + 0.86 | 2.58 + 0.89 |
| cache flush / pass B / untimed | 5.06 / 3.51 / 1.48 | 4.67 / 3.32 / 1.45 |
| liquidation traders scanned | 1,599 | 1,491 |

Other facts used below:
- The liquidation step has a heavy tail: 1.1% of blocks take 25.6-102 ms (std val0 histogram).
- `torus_liquidation_acted_total` is 0, so no liquidation fired in any cell.
- `end_resident_wait`: 73% of blocks wait under 0.5 ms and 18% wait 2-32 ms.

Ledger greps run, all over `ledger/*.md`:
- `pipelin|bl2|design-exec|speculat|hybrid`
- `liquidat|liq_scan|L1.3|prevalue|certificate`
- `end.?resident|begin.?resident|rows.?slot|resident.?wait`
- `fuse|super-block|multi-block|amorti|block cap|cadence|consecutive|bigger blocks`
- `two calls|pre.?list|per-call|fixed cost`
- `drop|alloc|prefault|arena`
- `ticket|persist.?wait`

I also checked `docs/perf/design-exec-pipeline-2026-08-20.md` sections 3.6 and 6, `docs/plans/exec-ceiling.md`
option C, and memories 966f49e6 and 85a53ffe.

---

## 1. liq-window-prevalue-r-snapshot: value the liquidation window from the frozen R(N-1) during engine(N)

**Mechanism.**
- **Inputs are fixed at block start.** At the start of block N:
  - `begin_resident` (app.rs:5610) has joined R(N-1).
  - `begin_block_oracle` (app.rs:5788) has fixed the block-start marks. `AccountReader::mark`
    (native_executor.rs:2431-2447) says oracle aggregation is the only writer, and it runs before any action.
  - R and `Users` are immutable until `EndResidentJob::run` takes them back with `Arc::get_mut`
    (native_executor.rs:5072-5120).
- **Pre-valuation.** Take Arc clones of R/Users and the cursor (R's liquidation CF). Pre-compute, for the scan+slack
  traders after the cursor:
  - the `liq_view` / `classify` result;
  - whether the cooldown row exists;
  - whether the pending row exists.

  These are the only inputs that `liquidation_pass` (liquidation_step.rs:402-528, liq_view 786-826; the per-trader
  gets in torus-core liquidation.rs:573-582 and 643-649) reads for a healthy trader.
- **Where it runs.** Run the pre-valuation as filler work in E's pool-scope waits: `run_jobs` (exec_pool.rs:43-58)
  becomes `in_place_scope` plus chunked filler that polls a "jobs left" counter, during match (native_executor.rs
  8819-8937) and pass A. E idles about 3.4 ms/blk there today. A single helper thread is the fallback variant.
- **At the tail** (app.rs:5876-5889):
  - E rebuilds the window exactly as today.
  - It range-scans the block's own pending writes (positions `t‖m`, balances, liquidation tags) over the window's
    address range. All of N's writes are there after the in-call cache flushes. This gives the window's touched
    traders.
  - It uses the precomputed result for every untouched trader and values touched traders inline, as today.
  - On the first act (a non-healthy trader), it falls back to serial valuation for the rest of the window.
- **Cleanup.** The clones are dropped before `end_resident_on_worker` (app.rs:6434).
- **Result.** State is byte-identical: an untouched trader reads pending (no entry), then R, which equals R(N-1).
  The marks are the same.

**Expected gain.**
- **Saving on E.** The step costs 3.29-3.49 ms/blk, which is 9.2-9.6% of the chain, at about 2.2 µs per scanned
  trader.
  - Touched share of the window: at most about 5% on std (at most 5,000 active senders over 100k accounts) and at
    most 0.5% on deep (1M accounts).
  - Left on E: window rebuild, the 300-market mark rows, vault views and touched traders, about 0.4-0.7 ms.
  - E chain saving: about 2.7-3.0 ms/blk, -7.5% to -8.3%.
- **Throughput.** The s67.4 elasticity (+3.0% matched/s per -10.2% block_ms) gives +2.3-2.5% matched/s. A strictly
  E-bound chain gives up to +8-9%. blk/s moves the same way.
- **Tail.** The 1.1% of blocks where the step takes 25-100 ms leave E's path, which helps exec block p95/p99.
- **CPU.** The filler variant uses E time that is idle today, so it adds about 0 node CPU on an oversubscribed host
  (load1 35-45 on 32 threads).

**Nearest prior.**
- `liq-scan-parallel-prevalue` (rejected s123, mem 966f49e6):
  - It was a fork-join of `liq_view` on the exec pool at the tail, still on the critical path.
  - It took the memo Mutex and the overlay RwLock about 4x per trader.
  - It was judged at 206-239 ms blocks, where the step was about 2% of a block.
  - This idea overlaps the step with engine(N), reads the immutable snapshot without the block memo or the pending
    lock, and the step is now 9.5% of a 36 ms block.
- `liquidation-health-certificate` (proposed) and `liquidation-sweep-moving-marks` (parked) carry results across
  blocks until the marks move. The marks move every 2 s, and a trader is revisited only every ~60 blocks, so
  certificates rarely survive. This idea needs only within-block untouchedness under block N's own marks.
- `L1.3`, `liq-touched-accounts-first`: these change what is scanned. This idea does not.

**Risks.**
- The touched check must cover every writer. The pending range-scan does that by construction. Any gov, delist or
  listing action, or CoreWriter entries, also forces the fallback.
- An Arc clone that outlives the block forces an R rebuild. Add an assert.
- Filler chunks can delay the pool join. Keep chunks around 50 µs and stop when the pool finishes.
- The L1 (sums) path and the walk path must agree. A shadow test exists at liquidation_step.rs:806-818.

**Effort.** 3-5 person-days.

**Prototype (3 h or less).** Use `crates/torus-bridge/tests/ubench_econ.rs` with `UB_MARKS=1 UB_MARK_WALK=10` at
300 markets.
- Arm A: today's `run_liquidations`.
- Arm B: pre-value on a scoped thread from Arc clones of R/Users before `execute_batch`, then a tail variant that
  consumes the precomputed results.
- Measure `tail_ms/blk`, and compare every block's frozen pending set byte for byte over 500 blocks.
- Run a forced-act run (mark shock so stage 1 fires) to prove the fallback gives equal results.
- **Go:** the step drops by at least 70% on E with 0 diffs.

---

## 2. exec-group-fusion: execute 2-4 committed blocks as one engine unit under backlog (consensus-visible)

**Mechanism.**
- **Header bit.** Add a `join_next` bit to `TorusBlockHeader` (torus-types lib.rs:520+), activated through the P0.0
  feature bitmap (torus-types features.rs).
- **Who sets it.** The proposer sets it when its own committed-but-unexecuted backlog is at least G, for example 4
  (`exec_queue_len`, app.rs ~6866).
- **Group formation.** `execution_loop` (app.rs:6814-6870) takes N. If `N.join_next`, it also takes N+1, up to
  `k_max` = 4. A group never crosses a `h%100` checkpoint height or an epoch boundary (app.rs:4083-4099).
- **One run of each step per group:**
  - `begin_resident` (5610) and `begin_block_oracle` (5788);
  - one pre and one post `execute_batch` over the merged, re-sorted lists (5803);
  - the tail (5840-5935), with the liquidation budget times k;
  - `save_order_books` (6056/6069), `end_resident` (6434), and the checkpoint upkeep (3989-4300).
- **Per-height records stay.** Headers, action status, mempool exec release and order-age stay per member height.
- **What does not change.** Consensus cadence, commit latency and DA are untouched. Only the executor's
  granularity changes.

**Expected gain.**
- **Fixed per-block cost.** Estimated at 9.3 ms by regression on the early vs late 60 s windows (53.6 ms at 23.3k
  placed vs 35.9 ms at 14.0k; measured.md §3.1). The 8u engine fit gives about 16 ms fixed + 2.47 ms/1k fills.
  Directly visible per block: end_resident_wait 1.43, untimed 1.48, save_books 0.56, load_books + margin_configs +
  action_status 0.32, upkeep 0.72, plus per-call and per-market setup.
- **Cache flush overlap.** The cache flush costs 5.06 ms/blk. The ~1,971 traders holding all resting orders
  (`cancel_all.index_traders_end`) are dirty in both blocks of a group, so they are flushed once.
- **At k=2:** about 4-6 ms saved per native block, -11% to -16% chain. That is +3.5-5% matched/s at the s67.4
  elasticity, and up to +12-19% if E-bound.
- **Latency.** Groups form only when the backlog is at least G, so waiting for N+1's commit (mean interval 45 ms)
  hides behind at least 4 x 36 ms of queued work.

**Counter-evidence.** In s124, deep R ran 82 actions/blk against CL21's 63. On-CPU per fill was 8.4% lower, but
wall per fill was equal (3.56 vs 3.55 ms/1k) because match wait per fill rose from 0.25 to 0.38.

**Nearest prior.**
- `propose-defer-on-exec-queue` (parked): it slows consensus and the commit.
- `exec-backlog-watermark-pacing-pkgd` (merged, NO-GO): scales proposer caps by exec queue depth.
- `exec-throttle-watermarks` and `proposer-exec-watermark` (proposed): thin blocks or yield proposal slots.
- `multi-block-views` (proposed, consensus side), the block-cap sweeps, and `L1.3`.
- None of these coalesces blocks that are already committed, at the executor, by a deterministic header bit.

**Risks.**
- Semantics: inside a group, "pre before post" now spans the blocks, so a CancelAll in N+1 runs before N's GTC
  places. This is the same rule as inside one block.
- Group timestamp and caps need definitions.
- Exec p99 rises and the bistable cadence may shift.
- A byzantine proposer can force k_max groups (bounded at +3 blocks).
- RPC executed-height jumps by k.
- Replay must re-form groups from the headers.

**Effort.** 8-12 person-days.

**Prototype (3 h or less).** Use ubench_econ with a fixed seed and `UB_MARKS=1` at 300 markets, so no node change
is needed.
- Arm k=1 is today. Arms k=2 and k=4 concatenate consecutive blocks' actions into one pre/post `execute_batch` pair,
  one `run_liquidations` with scan x k, and one begin/end resident.
- Report engine ms per 1k fills, `r_end/blk`, flush ms and wall per unit, and fit F from k = 1, 2, 4.
- **Go:** wall per fill at k=2 at least 8% lower.

---

## 3. streamed-end-resident: trader-granular readiness of R(N-1) instead of a whole-slot join

**Mechanism.**
- **Today.** `begin_resident(N)` joins the whole `end_resident(N-1)` job through `holder.settle_rows()`
  (native_executor.rs:4755). That job (`EndResidentJob::run`, 5072-5120) runs in this order:
  1. `drop(retired)`, which frees the block's per-order results, rejected orders' messages included (app.rs:6020
     `drop_later`);
  2. the sums carry, then `r.apply`, then `u.apply` (2.2-2.4 ms);
  3. `carry.finish`, then `drop(delta)`;
  4. finally the slot is sent.
- **(a) Send first, then free.** Send the slot first, then drop `retired` and `delta`. This needs no API change.
- **(b) Stream by shard.**
  - Split `Users.map` (users.rs:166-200, one `HashMap<Address, UserState>`) into 16 address-prefix shards, each its
    own Arc.
  - The worker applies the delta in trader-ascending order, which is the delta's key order, and publishes shard by
    shard. R's raw rows (0.06 ms) go first.
  - Block N's typed reads wait only on their own trader's shard.
  - The global `traders` and `holders` indexes are finished last. They are read only by liquidation and ADL at the
    tail.
  - Phase 1 and prepare visit senders in ascending order (sort key `(sender, hash)`, native_executor.rs:14937-14965),
    so E trails the worker.

**Expected gain.** E waits 1.34-1.59 ms/blk (3.7-4.3% of the chain). The tail is the target: the 18% of blocks that
wait 2-32 ms. With a residual wait of about 0.15-0.4 ms, E saves about 1.0-1.3 ms/blk (-3% to -3.5%). That is
+1-3.5% matched/s, and a better exec block p95.

**Nearest prior.**
- `end-resident-on-worker` (merged) uses a whole-slot join.
- `end-resident-reuse-decoded` (merged) made the apply cheaper.
- `exec-prep-stage` (rejected) overlapped verify with end_resident and recovered only ~0.4 ms.
- None changes the dependency granularity or the order inside the job.

**Risks.**
- Every typed read gains a shard-ready check (an atomic load).
- `Arc::get_mut` per shard, and the `set_source` wiring.
- `end_resident_worker_tests` must cover partial availability.
- The gain is capped at about 1.4 ms/blk.
- Part of the wait tail may be preemption that streaming only partly hides.

**Effort.** 4-6 person-days. Part (a) alone is under 1 day.

**Prototype (3 h or less).**
1. Correlate the `end_resident_wait` tail with schedstat runqueue in 9j2 (`schedstat.json`, `sampler.csv`).
2. Run ubench_econ with `UB_R_WORKER=1` and a 2.5 ms busy gap that mimics the pre-native work. Measure `wait/blk`
   for today vs part (a).
3. Instrument the first Users read after `begin_resident` and the per-phase access order, and compute the streamed
   wait from the per-shard apply times.

---

## Considered and refuted (not proposed)

- **Sender-disjoint prepare-ahead** (prepare(N+1) during settle(N) for senders that N did not touch). The ~1,971
  traders in `cancel_all.index_traders_end` hold all of the ~104k resting orders, and there are 11.5k fills per
  block, so nearly every active trader is touched in every block. The eligible share is near 0 on this shape. The
  phase-1(N+1) cancel and bid-floor dependencies add more.
- **Cross-block window-close encode fence.** It is adjacent to `parallel-classic-book-save(-b)` (measured no gain),
  L3.1/L6.1 (batch 10) and A1/Q6 (excluded).
- **Commit-persist ticket wait moved to the hand-off.** E waits 0.54 ms/blk on average, but 26 waits of 51-820 ms
  dominate. Those are state-DB write-queue stalls behind the checkpoint batch, which is the T5.3 attribution area
  (owner-excluded).
- **Verify or replay of N+1 in E's pool bubbles.** This is `exec-3stage-pipeline` (rejected) and `exec-prep-stage`
  (rejected). It was rejected because it was ingress-bound at the time; it is in the ledger, so it is not novel.
