# r2 lens lit-exchange-2: production matching-engine mechanisms, mapped to 29bfc705

Read-only. Nothing was built, run or benched. Anchors are at `29bfc705` (`origin/perf/h2h-combined`), extracted with
`git show` into `/home/oz/projects/wt/novel-s80/scratch-lx2/`:
- NE = `native_executor.rs`;
- OB = `order_book.rs` (identical at 29bfc705 and main 2d2f3ed9);
- APP = `app.rs` at 29bfc705.

New numbers used below:
- **Bench funnel**, std CL21 r2 val0 over 7,037 blocks (`~/bench-results-matched/ozarchy-9j2-std-r2-CL21/summary.json` `.funnel_by_node.val0`):
  - 15.39k placed/blk, 11.39k fills/blk, 9.5k resting/blk.
  - 7.6k full sell top-ups/blk (53.5M), 0 partial or no top-ups.
  - 0 margin rejects, 0 maker margin cancels, about 2.9 sell margin cuts/blk.
  - Cell: `--cross-fraction 0.5 --band 5`, flat 20x (no margin configs).
- **q8re thread profile** (`ozarchy-q8re-run/analysis/ozarchy-q8re-300m-r1-ONP-w1.txt`):
  - CPU per native block: exec pool 197.7 ms, EXEC 75.2 ms (2.6x).
  - Pool inclusive shares: match_market 43.9%, of which match_at_level 21.9%, account_fill 13.0%, insert_order 7.9%,
    maker_account 7.1%. compute_market_settle_plan 26.6%, cancel_all 10.7%.
  - EXEC inclusive shares: prepare_one 16.6%, Projections::get_or_try_insert 6.54%, get_position 4.96%.

## Lens mapping: the textbook mechanisms mostly land in the pool, not on the exec thread (E)

Memory 2484d5d6 (S395) already killed "optimize matching": match is about 4% of engine time. The matching-engine data
structures run inside the pool jobs. E only waits on them:
- match + plan: about 3.6 ms/blk (2.68 + 0.93, std CL21);
- cancel-all: about 1.7 ms/blk (8u: 0.145 ms per 1k fills off-CPU).

The pool's effective parallelism is about 16-20 (60-80 ms of pool CPU drains in about 3.6 ms of wall time). A
mechanism that halves book CPU (match_at_level + insert_order, about 30% of pool) therefore buys E about 0.5 ms/blk,
or about +0.4% matched/s at elasticity 0.3.

| mechanism | where it maps here | verdict |
|---|---|---|
| Price-level aggregates (Σqty, count) | FOK pre-check, depth RPC, B-blind floor (best bid only, NE 12727) | covered: r1 #5, `pf1-per-level-ask-depth` |
| Reserve-at-place, release-on-fill | already the A5 design (NE 12002-12095). Maker re-check at fill is HL parity (F1 D8). The bench never binds: 0 maker cancels, 0 margin rejects | **idea 1** (optimistic risk) |
| Slab / intrusive lists, O(1) cancel | pool (OB 2087-2147 `VecDeque::remove`, `queue_position`) | covered: `lazy-delete-level-queues`, `level-queue-16byte-slab`, L2.3, cancel compaction |
| Bitmap / radix / tick-ladder price index | pool. Band 5 leaves ~10 load levels per side, so the BTreeMap is depth 2: at most ~1 ms pool CPU/blk | not in the ledger, but ≈0 on E: not proposed |
| One-sweep batch matching per market | per-taker overhead on the pool (MW 445 params clone, PlaceResult with 5 Vecs) | ≤0.3 ms on E |
| In-block netting of crossing flow | changes price-time outcomes | semantic; `frequent-batch-auctions` (proposed) |
| Mass-quote / cancel-replace | P3 product surface | owner-excluded |
| Snapshot off the matching thread (LMAX, Redis CoW) | L6.1 %100 / L3.1 window-close encode on E | **idea 2** |
| Optimistic pre-trade risk (credit headroom; CME/ICE/Eurex pre-trade credit controls) | Phase 2 `prepare_one` | **idea 1** |
| No garbage on the business-logic thread (LMAX) | pool-allocated results freed on E | **idea 3** |

---

## 1. p2-headroom-certificate: skip the exact F1 placement math for senders whose headroom provably covers the run

**Mechanism.** Production venues gate order entry on an aggregate credit headroom and recompute exact portfolio risk
only near the limit. Here a sender's PlaceOrderBatch orders form one contiguous run in flat order (sort key: category,
sender, hash).

At the run's start, E does one cheap pre-pass over the params (the serial loop at NE 9448-9480):
- `R = Σ required_i` (order-tier IM; the same `try_reserve_for_qty_cfg`, NE 11986);
- `T = Σ` of the B-blind top-up extras at the markets' bid floors (NE 12727-12800);
- `X_ub = Σ excess_ub_i`:
  - flat tiers: 1 raw unit per order (IM floor dust);
  - tiered: `(PosSums.notional + Σ q_i·max(mark, res_price)) · (1/lev_min − 1/lev_max)`;
- `C_ub` = the worst-case charge against the sender's D2 pool market: its checked takers and its own resting orders
  there, found through `trader_orders`.

**Certificate.** `F0 = available + pos_net ≥ R + T + X_ub + C_ub`. When it holds:
- every `account_check` passes;
- every top-up is granted in full;
- no pool-market check binds.

`prepare_one` (NE 9959-10224) then skips `Projections::get_or_try_insert`, `get_position` (NE 10093-10108, 2850),
`placement_need`, and the excess, released and projection-advance work.

It emits `pre_pos = None`. `match_setup` already re-reads `None` senders on the pool (NE 13041-13080,
`reduce_only_positions_for` NE 13120, `d2_taker_px`). Item 6 M1 moved this read into Phase 2 while match setup still
ran on E. Since M (parallel match setup, default on) it belongs on the pool for certified senders.

Top-ups are granted inline in the same pass (no second pass over 7.6k candidates). Runs with reduce-only orders, stops,
IOC/FOK/market orders or an unmarked market take the exact path.

**Target metric.** Exec on-CPU per fill (margin phase), matched/s, native blk/s.

**Expected gain.**

| item | E ms/blk |
|---|---|
| skippable share of `prepare_one`: Projections + get_position (6.54% EXEC) + placement_need and FixedPoint math (~2-3%) ≈ 50-55%, out of a 10.29 ms margin phase | -3..-4 |
| position reads moved to the pool (+1.5-2.5 ms pool CPU) | +0.1 (wait) |
| fused top-ups | -0.4..-0.8 |
| **net** | **-2.5..-4.5** (-6.5..-11.7% of 38.35) |

At elasticity 0.3 that is matched/s +2..+3.5% and blk/s +6..+11%. If r1 #3 lands first, the net is -1.5..-3 ms/blk.

**Hit rate.** With 0 margin rejects and 100% full top-ups, `F0 − R − excess − T ≥ 0` held for every sender at the end
of its fold. The certificate needs only the extra slack for `X_ub + C_ub`:
- flat 20x: `X_ub` is about 400 raw units per batch, negligible;
- `C_ub` is about 0 because the mark-loss charge is 0 inside ±5 ticks (`tol_open` ≫ loss).

**Byte-identical (proof obligations).**
- IM is monotone, so a closing order needs no more than a non-closing one.
- The overflow is bounded.
- `excess_unchecked` only feeds top-up grants and pools, and both are certified non-binding.
- The px/RO values `match_setup` reads equal Phase 2's (frozen reader, same `position_px`).

Ship with a debug shadow mode that runs the exact path and asserts equal PrepOutcomes (`pre_pos` excepted), grants
and post-match books.

**Nearest prior.**

Grepped: `certif|headroom|optimistic|credit limit|skip.*check|fast.path|account_check|placement_need|projection|prepare`.
Memory checked: 57cb9090, 9aa50928, 2484d5d6, the s517 F1 series.

| prior | difference |
|---|---|
| `liq-l2-health-certificates`, `liquidation-health-certificate` | certificates for liquidation only, not placement |
| `margin-f1-account-check-cuts` | s11-era micro-cuts of the same per-order work |
| `fast-sell-top-ups-t` (merged) | a stitch list; no fusion |
| `parallel-engine-pe` | excluded; parallelises the same work instead of skipping it |
| r1 #3 position lookup | makes the reads cheaper; idea 1 removes them for certified runs |
| r1 #15 lookahead | state-free constants only |
| r1 #18 reciprocals | cheaper division only |

**Consensus-visible.** No: byte-identical by proof.

**Risks.**
- An unsound bound would accept an order the spec rejects. All nodes running the same binary agree, so this is a spec
  bug. A mixed fleet forks only if the bound is unsound.
- Tiered markets give conservative bounds, so the hit rate is lower for leveraged accounts.
- The M1 shadow test (NE 13083) treats `pre_pos = None` as a mismatch and must change.
- Ship behind a flag, default off, until the differential fuzz is green.
- **Optional stage 2:** a per-(trader, book) certificate in `match_setup` that also skips per-fill `account_fill`
  (13% of pool). Pool-only gain.

**Effort.** 5-7 person-days.

**Prototype (≤ 3 h).**
1. In `proto/` on the h2h base, add `TORUS_S80_P2_CERT` (pre-pass, certificate, skip path, `pre_pos = None`).
2. Run `crates/torus-bridge/tests/ubench_econ.rs` with `UB_SENDERS=5000`, 300 markets, ABAB n=3.
3. Record: margin ms/blk, match ms/blk, `engine_ms_per_1k_fills`, the certificate hit rate, and the block digests over
   50 blocks with the shadow assert on.
4. Add a tiered-config arm (`default_margin_tiers`) for the hit rate.
5. Kill if any of these holds:
   - any digest or outcome mismatch;
   - flat hit rate < 80%;
   - margin phase saves < 25%;
   - match wait rises by more than 30% of the E saving.

---

## 2. book-cow-snapshot-encode: the L6.1 %100 and window-close Classic encodes leave E (copy-on-write level snapshots)

**Mechanism.** Today, on an attest height, `save_order_books` (NE 7978-8070, `full`) encodes all ~300 Classic books on
E via `encode_classic_book` (NE 788) and puts ~110 MB (deep) into the overlay. The L3.1 window close encodes the owed
books on E too (APP 4088-4120 `coalesce_unsaved_books`). The close merge then clones those blobs (APP 3886-3889; r1 #6).

The change:
- Levels become `Arc<VecDeque<Order>>` (OB 1191-1193).
- Mutation sites call `Arc::make_mut`: match (OB 2705-3064), `insert_order` (3111), `cancel_order` (2087), the
  cancel_batch compaction.
- E takes an O(levels) snapshot: Arc clones of the levels, header fields and a stops clone. It stores a blob handle in
  the %100 write set.
- W's digest/checkpoint job (exec_pipeline.rs `worker_loop`, ~984) runs `OrderBook::encode_classic`
  (classic_encode.rs:177) over the snapshot, then hashes and writes.
- The bytes are the same, from the same logical state.
- Only levels mutated while W still holds the snapshot are copied, once each.

**Expected gain.**
- **Deep:** save_books 1.22 → ~0.6 ms/blk (std is 0.57), and the close-merge blob clone -0.3..-0.45 ms/blk (r1 #6:
  ~80% of the deep close premium). That is **E -0.9..-1.1 ms/blk** (-2.5% of 40.35), so deep matched/s +0.7..+0.8%.
- **The %100-height E spike** shrinks by ~100 ms (60 ms encode + ~40 ms clone).
- **Deep RSS:** -110..-220 MB transient.
- **Standard:** -0.05..-0.1 ms/blk.
- **Scaling:** E's attest cost moves from ~55 ns per resting order (10M resting ≈ 0.6 s encode plus a 1.1 GB clone)
  to ~1-2 ms per ~63k levels.
- **Costs:** W +0.6 ms/blk on deep (busy 0.205 → ~0.22). Pool +~1 ms CPU/blk for the Arc indirection (≈ +0.05 ms E
  wait).

**Nearest prior.**

Grepped: `cow|copy-on-write|persistent|snapshot|structural|off.*exec.*encode|on W|book save`. Memory checked: 3059a0d8,
d2e41de7, 4a70fa90.

| prior | difference |
|---|---|
| `book-save-pass1-offthread` (rejected) | rejected because live levels are mutated by engine(N+1); CoW removes exactly that obstacle |
| `parallel-classic-book-save` (parked / no gain) | E waits for the pool; +3.5% CPU |
| Phase 5 step 3 (d2e41de7) and r1 #12 rope | per-level byte cache on E, depends on parked Phase 5; leaves 7-12 ms on E |
| r1 #6 close stack | removes the merge, not the encode; composes with this idea |
| `evm-trie-on-w-window` | precedent for deriving data on W |

**Consensus-visible.** No.

**Risks.**
- The level-type refactor touches the mode 2/3 code paths.
- Every reader of the %100 set must resolve the handle: ReadView, the resident-holder rebuild after a drain,
  `checkpoint_drain`.
- W's digest latency at %100.
- A proptest is needed: snapshot encode == direct encode.
- The snapshot pins levels until W finishes.

**Effort.** 7-10 person-days.

**Prototype (≤ 3 h).** A torus-core `#[ignore]` microbench on the deep shape:
- 300 books;
- 100 cold levels per side, ≥1,000 ticks out, ~3.3k orders per book;
- 10 hot levels per side, 50 orders each.

Time:
- (a) `encode_classic` of all books;
- (b) the snapshot;
- (c) place/fill/cancel ops on hot levels with `Arc` + `make_mut` against plain `VecDeque`;
- (d) the post-snapshot copy.

Kill if (b) > 5 ms, (c) > +3% per op, or the bytes differ.

---

## 3. exec-garbage-off-e: pool-allocated per-market results are freed off the exec thread

**Mechanism.** LMAX keeps garbage off the business-logic thread. Under L2.1, E receives each market's
`MarketBatchResult` and plan from the pool jobs and frees them:
- ~15.4k `PlaceResult`s, with the fill Vecs of ~7.7k takers;
- 300 match-result, plan, `pos_cache` and volume tables;
- the AccountMargins/RO maps (cleared on E in `absorb_matched`, NE 10437-10442).

Most of this drops when `settle_streamed_rest` returns (NE 10835-10912). The `pos_runs` drop after the flush.

The change: hand them, as one item per call, to the existing `Dropper` (APP 514-559, QUEUE 2, inline fallback). Or
return them to the pool for the next block's jobs, which keeps frees in the allocating threads' tcaches. The result is
byte-identical.

**Expected gain.**
- The 8u E alloc/free leaf is 0.048 ms per 1k fills (≈0.55 ms/blk).
- q8re saw `_rjem_je_arena_ptr_array_flush` at 1.38% of EXEC self.
- Unattributed E time is 1.26 ms/blk in settle and 1.54 ms/blk untimed in the engine (std CL21).
- Estimate: **E -0.3..-0.8 ms/blk**, matched/s +0.2..+0.6%. Low confidence.

**Nearest prior.**

Grepped: `drop|dealloc|jemalloc|tcache|remote free|cross-thread|arena|allocat`.

| prior | difference |
|---|---|
| `ckpt-drop-thread` | checkpoint layers only |
| r1 #17 exec-block-arena | E-built transient containers; Bump is !Sync, so it cannot take pool outputs |
| jemalloc decay rows | allocator tuning, not ownership |

**Risks.**
- The Dropper queue is 2 deep, so it falls back to inline drops.
- Memory is held about one block longer.
- The gain may be under 0.3 ms.

**Effort.** 1-2 person-days.

**Prototype.** Time the end-of-settle drop and run `ubench_econ` ABAB (inline vs dropper vs pool-return). Kill if E
saves < 0.25 ms/blk.
