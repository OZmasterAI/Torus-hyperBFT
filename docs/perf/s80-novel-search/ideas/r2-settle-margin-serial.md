# r2 lens settle-margin-serial: the serial margin phase (10.3 ms/blk) and settle pass B (3.7 ms/blk) on E

Code refs are repo HEAD `2d2f3ed9` (main `eb993e3f` + batch 10; the Phase 2 / settle code cited is the same family).
NE = `crates/torus-bridge/src/native_executor.rs`, OB = `crates/torus-core/src/order_book.rs`. Numbers come from
`map/measured.md` (std CL21: exec block 38.35 ms, margin 10.29, settle 11.25 = pass A 0.93 / pass B 3.71 / cache
flush 5.35, 11.5k fills/blk) and `~/bench-results-matched/ozarchy-9j2-std-r2-CL21/summary.json` (108.3M orders
placed-accepted over 7,037 blocks = 15.4k placed/blk, ~39 PlaceOrderBatch/blk; orders_rejected_margin,
_open_limit, _book and _other all 0; sell_top_ups_full 53.5M = 49% of orders, partial 0, none 0). Profile shares
are from q8re W1 EXEC (val1, 34.5k placed/blk, 75.2 ms/blk), scaled by 15.4/34.5 = 0.45 where used. Conversion
(RD s67.4): block_ms -10% gives matched/s +3.0% and blk/s +9.5%. This is a ceiling: PE and s124 show about 0.
Nothing was built or run.

Ranked: 1 > 2 > 3. 1 and 2 are independent (Phase 2 vs Phase 4) and stack. 3 is a robustness fix.

---

## 1. phase2-order-free-certificate: a per-sender order-independence certificate, then Phase 2 evaluated market-major in exec-pool jobs

**Mechanism.**
- Today Phase 2 (NE 9297-9554) folds each sender's orders in flat order on E. Each order goes through:
  - `take_open_slot`;
  - the reservation;
  - `account_check` against `free = available + pos_net + credit - committed` (NE 10132);
  - the `Projections` map;
  - the `stitch_outcome` id assignment (NE 10251).
- After the fold come `sell_top_ups_fast` (NE 12745) and `d2_pools` (NE 12615).
- Every running term moves `free` one way only. `required` and `excess` are >= 0 and only lower it. `credit` is >= 0
  and only raises it.
- So define `F_min = av0 + pn - sum(required + excess)` over ALL of the sender's orders. Assume three things hold:
  - every `need_i <= max(0, F_min)`;
  - `open0` plus the slot-taking orders stays within the limit (and the RO/stop base rule);
  - the free margin left after the fold is at least the sum of the top-up extras.
- Then every order passes in any evaluation order. Every output is either a per-order value or a per-sender sum:
  - `PrepPass` fields;
  - the `FoldOut` balance, pools and `excess_unchecked`;
  - full top-up grants.
- The only flat-order facts are trivial:
  - the D2 pool market = the market of the first checked order that passes the pure validations;
  - an order takes the floor iff it comes after that order and is in another market.

**Change.**
1. **E pre-pass:** bucket the place orders by market (CSR), and find each sender's pool order.
2. **One exec-pool round, LPT by market.** For each order, the job computes:
   - the pure validations (price, shape, band);
   - the floor and `res_price`;
   - the reservation;
   - the projection walk (one local variable per sender run, no map);
   - `need` / `excess` / `release`;
   - the top-up extra;
   - plus each book's open-order counts and its `Phase2Market`.
   Each job builds its market's `PreparedOrder` vector directly.
3. **E reduces the per-sender partial sums** (about 39 senders x J jobs). It then:
   - checks the certificate;
   - assigns order ids from a prefix count over a validity byte per flat index (the match job patches them);
   - builds pools and balances from the sums.
4. **Senders that fail the certificate** (or any fault or overflow) rerun today's `prepare_one` serially. Their
   speculative output is discarded, so the result is byte-identical.

**Stage A (no pool):** the same certificate and market-major evaluation run serially on E. This removes the
SenderFold map, the Projections map, the per-order `market_batches` / `phase2_markets` probes, `TopUpCand` and the
per-candidate top-up lookups.

**Evidence.**
- Margin is 10.29 ms/blk (std) and 10.45 (deep). It is the largest engine phase and runs serially (measured.md 3.1).
- In the bench, margin, open-limit and other rejects are 0 and every top-up is full, so the certificate should hold
  for nearly every sender. The prototype must confirm this.
- q8re prepare_one inclusive is 12.54 ms = 363 ns/order:
  - Projections 4.94 ms;
  - try_reserve 1.71 ms;
  - account_check 1.60 ms;
  - take_open_slot 0.98 ms.
- Other q8re items: stitch 1.53 ms, open_order_counts 1.04 ms, SenderFold::state 0.42 ms.
- Today only E does this work; the pool is idle between Phase 1 and Phase 3.
- The PE design doc (`docs/design-parallel-engine.md` section 7) says Phase-2 per-order cost is "too small for
  spawn/coordination to amortize below ~5k orders". That was at 400 orders per block. Today there are 15.4k orders
  per block at about 660 ns each.

**Expected gain.**
- E residual after the change:

  | step | ms/blk |
  |---|---|
  | bucketing, 15.4k x ~20 ns | 0.3 |
  | one pool round trip (s92) | 0.34-0.53 |
  | LPT tail / preemption | about 0.3 |
  | reduce / rank / pools | about 0.1 |
  | **total** | **about 1.0-1.3** |

- E falls by 6.5..9 ms/blk, -17..-23% of the 38.35 ms block.
- Ceilings: matched/s +5..+7% and blk/s +16..+22%.
- Realistic:
  - matched/s 0..+5%;
  - blk/s +15..+20% (PE's comparable E cut gave +15.7% blk/s and -1.4% matched/s);
  - block p99 -5..-10% (PE -7%);
  - exec-age p50 (1,152 ms) falls as the queue drains faster.
- E on-CPU per fill falls by about 20%. Node CPU stays about flat: 4.3-5.5 ms/blk of per-order work moves to the
  pool, and the fold, stitch and map overhead (about 1.5-2 ms) goes away.
- Early-window blocks (23.3k placed, margin 15.2 ms) gain more.
- Stage A alone: -1..-2 ms/blk.

**Nearest prior.**
- Grepped: `certif`, `speculat`, `optimistic`, `order.independ`, `commutat`, `sufficien`, `headroom`, `all.pass`,
  `market.major`, `by market`, `phase2.*pool`, `prepare.*pool`, `parallel.engine`, `L3-ENG`, `prepare.*shard`.
- `parallel-engine-pe` / `parallel-engine-phase2-sharding` (merged default-off, measured-no-gain; owner list).
  - It replays each sender's order-dependent fold on `std::thread::scope` spawns, keeps the serial stitch and the
    serial top-ups, and adds +2.4% CPU per fill.
  - Here, the certificate makes outcomes order-free. That allows market-major evaluation in the existing exec pool,
    with no stitch, no fold maps and no spawns.
- `engine-par-l3-microbench` (no gain at cap-400) and `phase2-parallel-by-sender`: same as PE.
- `margin-f1-account-check-cuts` (proposed): a list of inline micro cuts.
- r1 #3 phase2-position-lookup: superseded on E (lookups move to the pool).
- r1 #15 phase2-order-prep-lookahead: state-free constants only.
- `liquidation-health-certificate`: a different domain.

**Risks.**
- The owner may read the PE exclusion as covering any parallel Phase 2. In that case keep stage A.
- PE precedent: an E cut may convert to blk/s and latency, not matched/s.
- Host contention: the last LPT job is preempted 15-38% of the time.
- Real near-margin traders fail the certificate, and their pool work is wasted. E then costs today's time plus the
  reduce step.
- Exactness subtleties:
  - pool-order validity;
  - floor and `res_price`;
  - `basis` overrides;
  - FixedPoint overflow in the sums (use checked sums and fall back);
  - reader faults (fall back);
  - the `split_margin_label` path exists only on rejects.
- Interplay with M (`setup_in_jobs`) and L2.1.

**Effort.** 8-12 person-days; stage A alone 3-5 days.

**Prototype (<= 3 h, `ubench_econ`, `UB_MARKETS=300 UB_SENDERS=5000`, batch 400).**
1. Test-only hook after the serial fold. For each sender, log `F_min - max need_i`, the slot margin and the top-up
   margin, and report the certificate pass rate. Also run `account_margin_tests` near-margin fixtures to exercise
   fallbacks.
2. Shadow arm. On the same `AccountReader` / `basis` / `markets`, run the per-market evaluation in
   `exec_pool().scope`. Assert equality of every `PreparedOrder` field, `FoldOut`, the top-up counts, the pools and
   the ids for certificate senders.
3. Time three things:
   - shadow wall;
   - pool CPU;
   - E bucketing + reduce + rank.
4. Kill if any of these holds:
   - pass rate < 90% of senders;
   - any mismatch;
   - projected E residual > 40% of the serial margin ms.

---

## 2. passb-cothread-under-position-flush: run the unchanged pass B on one co-thread, overlapped with E's position flush

**Mechanism.**
- Under L2.1, E runs `settle_pass_b_market` (NE 11479-11713) market by market in the streamed region.
- But in the 9b standard cells only 18 of 97-101 streamed markets per block went through pass B before the pool
  finished (RD s114.2; `is_early = !rx.all_done()`, NE 10730). So about 80% of pass B runs on E after matching,
  ahead of the position flush (NE 9891-9914).
- **Change:** the region hands each finished market's `(MatchResult, plan minus pos_cache)` to one scoped co-thread.
  - The co-thread owns the `PassBSink` / `PassBOut` fields (NE 6018-6070): books, trader/cloid indexes, `bal_cache`,
    `vol_cache`, trade log, results, gas, metrics and a local fatal latch.
  - E only pushes each plan's `pos_cache` into `pos_runs` as it arrives.
  - E runs `flush_with_runs` while the co-thread finishes.
  - E joins before `bal_cache.flush_all` (NE 9931).
- Same code, same order, disjoint fields, so the result is byte-identical.
- **Option: completion-order apply.** Releases are >= 0 and clamp only `order_margin`, and PnL only adds to
  `available`. So per trader, `om' = max(om0 - sum R, 0)` and `av' = av0 + sum P + min(sum R, om0)` in any order. The
  co-thread can therefore apply markets as they complete, and never parks behind a hot market.

**Evidence.**
- Pass B is 3.71 ms (measured.md) and 3.66 ms (9j2 val0). The cache flush is 5.26 ms, and the positions part
  dominates it: q8re `PositionCache::flush_all` inclusive is 10.48 ms vs the 10.19-10.77 ms cache-flush timer.
- RD s119.4: pass B is 1.07-1.33x slower beside a busy pool.
- s119.3 Q0 16.8k: pass B 4.38 ms -> 5.79 ms with streaming.
- The flush order is fixed (NE 9891-9947): positions, then balances, then cum-volume, then triggered stops. Nothing
  before the balance flush reads `bal_cache`, the trade log or results.

**Expected gain.**
- About 82% of 3.7 ms, i.e. about 3.0 ms, sits on E after `T_pool`.
- The co-thread's post-pool work, 3.0 x 1.1-1.3 = 3.3-3.9 ms, is less than the ~4.5 ms position flush, so it is
  hidden.
- Net E -2.0..-3.0 ms/blk (-5..-8%), after the per-market hand-off (about 300 x 1 us) and join slack.
- blk/s +5..+7.5%; matched/s 0..+2.4%.
- E on-CPU per fill -6..-8% (the owner's merge metric). Node CPU +0..+1%.
- With a skewed load, completion order adds the hot market's match time. On the uniform bench it adds about 0.

**Nearest prior.**
- Grepped: `pass.b`, `helper`, `overlap`, `concurrent`, `offload`, `flush.*pass`, `completion.order`, `reorder`,
  `head.of.line`, `net.*per.trader`, `clamp`.
- L2.1 `streamed-in-order-settle-l21`: pass B on E, overlapped only with matching.
- `settle-passb-parallel-shard-apply` and `parallel-passb-persistent-shards` (proposed): data-parallel pass B by
  trader shard. They need an ordering design.
- `net-balance-per-trader` (proposed): netting.
- `exec-prep-stage` (rejected): a different stage.
- Offload precedents: end-resident-on-worker and `running-hash-overlap-digest-thread`.
- Concrete difference here: the serial, canonical pass B is unchanged and moved whole onto one co-thread,
  task-overlapped with E's own flush. The clamp-composition lemma is used only to allow reordering.

**Risks.**
- Memory contention (the L2.1 precedent: +2% E CPU per fill).
- On the 3-node host the co-thread can be preempted, so E waits at the join.
- Overlay `RwLock` contention between the co-thread's balance first-reads (`own_pending_row`, `position.rs:424`) and
  E's position puts.
- L2.1 undo and fallback must join the co-thread first.
- The co-thread needs panic containment.

**Effort.** 4-6 person-days.

**Prototype (<= 3 h).**
1. In `ubench_settle_streamed.rs` (the s119 Q harness, 16.8k and 65k, `UBENCH_L22=on`), add a hack flag that sends
   each market to a `std::thread::scope` co-thread running `settle_pass_b_market` in market order. E keeps the
   `pos_runs` pushes and the flush.
2. Measure:
   - exec wall and on-CPU per call;
   - co-thread pass B time;
   - join wait.
3. Run `settle_streamed_tests` and `parallel_settle_tests` with the flag on.
4. Kill if any of these holds:
   - wall improves < 1.5 ms per call at 16.8k;
   - mean join wait > 0.5 ms;
   - any byte difference.

---

## 3. ro-basis-walk-memo: the Phase-2 reduce-only reservation basis walks the sender's whole resting set per RO order

**Mechanism.**
- `phase2_reservation_basis` (NE 12402-12487) runs for every place order (NE 9358), before any slot or margin check
  (`prepare_one`, NE 9473).
- For each reduce-only non-stop order it calls `book.orders_for_trader(&sender)` (NE 12439) and folds the non-RO
  remaining quantity.
  - `orders_for_trader` (OB 2448) builds a `Vec<&Order>` with R `get_order` calls.
  - Each `get_order` is an `order_index` probe, a BTreeMap get, and an `order_seq` binary search (OB 2376-2410).
- So each RO order costs O(R), where R is the sender's resting orders in that market.
- Phase 2 touches no book, so the sum is a pure function of (sender, market) within the call.
- **Change:** memoize the sum in a `HashMap<(Address, MarketId), FixedPoint>` beside `growth`. The first RO order of
  a pair walks once, over the ids, with no Vec. The fold order is the same (`sat_add`), so the result is
  byte-identical.
- **Stage 2 (optional):** keep a non-RO remaining sum per trader in `TraderOrders`. That is O(1), derived and never
  serialized. Fall back to the walk near saturation.

**Evidence.**
- NE 12439 is the only exec-path caller of `orders_for_trader`.
- The RPC applies only the delisted rule to RO orders (`torus.rs:418-440`), so no position is needed.
- The RO open-limit reject runs in `take_open_slot` after the walk. With open >= 1,000, all the RO orders are
  rejected for free.
- Caps that bound the attack:
  - `NATIVE_ORDERS_PER_BATCH_CAP` 1,024 (`torus-types/src/lib.rs:762`);
  - orders per block 200,000 (`rate_limit.rs:250`);
  - open limit 1,000-5,000 (`position.rs:245-256`).
- Not in r2-worst-block-catalogue: its idea 1 is the match-time `ReduceOnlyPositions` install.

**Expected gain.**
- Bench: 0 (it sends no RO orders).
- Attack: one account rests R = 1,000 dust orders in market m (reusable capital) and sends 1,024-order RO batches.
  - Per 10 KB action: 1,024 x 1,000 x ~0.1-0.4 us = 0.1-0.4 s of E.
  - At the block order cap: 20-80 s per block, against 38 ms honest. Since E paces commits, the chain stalls.
- After the memo: one walk per (sender, market) per call, summing to at most the open limit per sender. That is about
  0.1-0.4 ms per attacking sender, a 1,000x cut or more. Stage 2 makes it O(1).

**Nearest prior.**
- Grepped: `basis`, `orders_for_trader`, `reduce.only.*reserv`, `ro.reserv`, `resting non`.
- r2-worst-block-catalogue idea 1 (lazy-ro-policing): `ReduceOnlyPositions` install in `match_setup`, NE 13056-13167.
  A different site.
- r1 #10 ro-sweep-aggregate: the book's RO sweep at match time (OB 1838-1851).
- r1 #5 fok-precheck-bounded; R16 option B budget; resting-order-cap (D14 b).

**Risks.**
- Stage 2 must be maintained at every rest, fill, cancel, modify and restore site and rebuilt on load. Drift between
  nodes is a fork under the hash fail-stop, so add a debug recount.
- The memo itself is low risk.

**Effort.** 0.5-1 day for the memo and tests; +1-2 days for stage 2.

**Prototype (<= 2 h).**
1. Add an `#[ignore]` test in `crates/torus-bridge/tests/` (next to `reduce_only_tests.rs`):
   - one book where sender S rests R in {100, 1,000} non-RO orders, at one level and spread over levels;
   - one `execute_batch` with a 1,024-order RO batch of S in that market.
2. Read `phase_accum.margin_ns` before and after the memo, and assert identical results and state.
3. Kill if R = 1,000 costs < 20 ms per action today.
