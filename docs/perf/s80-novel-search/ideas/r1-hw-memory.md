# novel-s80 round 1, lens hw-memory (exec thread + exec pool micro-architecture and memory)

Read-only on main `eb993e3f`. Nothing built or run. Every number below is from existing artefacts (paths given) or
from code reading; estimates are marked **est.** with their arithmetic.

New evidence found for this lens (not in measured.md):

- **THP collapse under load.** `~/bench-results-matched/ozarchy-9j2-std-r2-CL21/mem-samples.txt` (val0) shows
  AnonHugePages / Anonymous at 96% before load, then 43% (+40 s), 30%, 25%, 23%, **22% steady** (2,501 MB anon, 560 MB
  THP). Deep `ozarchy-9j2-deep-r4-CL21`: 98% after the 1M preload, **23%** steady (4,054 MB / 942 MB). Host sysfs:
  THP `enabled=[always]`, `defrag=[madvise]`, so non-madvised refaults never compact and come back as 4K pages.
- **TLB / DRAM rates of the exec group** (`~/bench-results-matched/ozarchy-ipc-main/ipc-w2.json`, main `92a02ed`,
  2026-10-05, older binary): 48.8 G cycles, 55.9 G instructions, 217.6 M L1-dTLB misses (3.9/kinstr); W1 DRAM demand
  fills 1.14/kinstr (`ozarchy-ipc-analysis.md`); cross-CCX fills only 3.39 M (0.06/kinstr).
- **CPU**: AMD Ryzen 9 5950X (Zen 3: L1 dTLB 64 entries, L2 TLB 2,048 entries = 8 MB reach at 4K, 4 GB at 2M;
  2 CCDs x 32 MB L3). `/proc/cpuinfo` has `cat_l3 cdp_l3 mba cqm_llc cqm_mbm_total rdt_a`; `resctrl` is in
  `/proc/filesystems` (not mounted).
- **Exec page faults by phase** (`ozarchy-8u-run/analyst/8u-w2report.txt`, deep, pre-L3.1): 60.1 faults per 1k fills
  = save_books 28.5 (via `do_rallocx`, gone with L3.1/L3.3) + engine settle 19.8 + engine other 11.0; engine leaf
  memmove/memcpy 0.235, page fault 0.082, jemalloc 0.019, alloc wrappers 0.017 ms per 1k fills. 0.153 ms / 60 faults
  = 2.55 us per fault, which is the cost of a 4K fault (a 2M fault would cost ~50-100 us).
- **Refuted while checking (not proposed):** the checkpoint-close dropper flood (`app.rs:235-240`, `Dropper::QUEUE=2`
  `app.rs:486`). `drop_inline_total` 7,326-7,595 per cell (~99 per checkpoint) looked like ~100 ingress slices freed
  on E, but memory `091a069d` (s124 adversarial check, perf) shows those are Arc decrements; the old view is freed on
  `torus-ckpt-drop` or rpc-worker except at epoch barriers. A cross-CCD placement idea was also dropped: cross-CCX
  fills are ~2% of exec cycles.

---

## 1. thp-preserving-heap: keep the node heap huge-page backed (jemalloc HPA, or thp:always with defrag defer)

**Mechanism.** jemalloc purges dirty memory in 4K extents (10 s decay; the >= 8 MiB huge arena purges at once). Each
purge inside a transparent huge page splits it, and the refault comes back as 4K because `defrag=madvise` never
compacts for regions that were not madvised. Under load the heap therefore drops from ~97% to ~22% THP. The fix
keeps purges hugepage-aligned and the heap madvised:
- arm (a): `MALLOC_CONF=thp:always,metadata_thp:always`, with host `defrag=defer` and a faster khugepaged;
- arm (b): jemalloc's hugepage-aware allocator (`hpa:true`, 5.3.x);
- arm (c), optional: bind `torus-end-resident`, which builds `Users`, to its own arena with decay -1. Long-lived
  resident state then never shares hugepages with per-block churn.

Production: bake the winning config with `JEMALLOC_SYS_WITH_MALLOC_CONF` (allocator at
`crates/torus-node/src/main.rs:37-42`), add a THP startup log and a `torus_anon_huge_bytes` gauge.

**Target metric:** exec on-CPU ms per 1k fills, then matched/s; deep RSS as a guard.

**Expected gain (est.).** Exec group, per the numbers above:
- **Page walks.** 63.7 M DRAM fills (1.14/kinstr x 55.9 G) are, on a 4K multi-GB heap, roughly the L2-TLB misses. At
  35-60 cycles per walk that is 2.2-3.8 G cycles = 4.5-7.8% of 48.8 G.
- **L2-TLB hits.** (217.6 - 63.7) M x ~7 cycles = 1.1 G cycles = 2.2%.
- **TLB total: 6.7-10% of exec cycles** at 4K. With 2M pages the L2 TLB covers 4 GB, so most of it goes.
- **Faults.** Engine ~31 faults per 1k fills x 2.55 us = 0.08 ms/1k, i.e. ~0.9 ms/blk at 11.5k fills, mostly per-fault
  overhead: about -0.6 ms/blk.
- **Total.** -2.2..-3.6 ms of 31.8 ms on-CPU per block = **-7..-11% E CPU per fill**. Matched/s at the E ceiling
  **+5..+9%** (265k -> ~278-289k). Deep (4.4 GB heap) should gain at least as much.

**Evidence.** mem-samples THP fractions above; sysfs THP modes; ipc-w2 TLB/DRAM rates; 8u fault table. The
decay A/Bs (`MALLOC_CONF dirty_decay 1000 / 0`: -7.1% / -30.7% matched/s, OZS s62/s73) show that page-level churn is
first-order for this workload.

**Nearest prior.** Grepped the ledger for `thp|huge.?page|hugetlb|MADV_HUGE|transparent|tlb|page.?fault|tcache|
narenas|percpu|oversize|MALLOC_CONF`, and memory for "THP AnonHugePages hpa TLB". No hit. Closest entries:
- jemalloc-decay-tuning / jemalloc-decay (rejected) and jemalloc-decay-background-thread (inconclusive): they change
  WHEN dirty pages are purged. This idea changes the page size and keeps purges from shattering hugepages;
- jemalloc-global-allocator (default-on), glibc-vs-jemalloc-preload, phase3-checkpoint-layer-shrink.

**Risks.**
- RSS bloat of +0.3-1 GB against the 8 GB class (deep max 5.7 GB).
- Direct-compaction fault stalls on E (p99) if madvised regions compact on fault. Use `defrag=defer`.
- HPA is experimental in 5.3. Anything that raises the refault rate can be strongly negative (the decay=0 lesson).
- Host knobs need an ops note.

**Prototype (<= 3 h).**
1. ubench_econ (`crates/torus-bridge/tests/ubench_econ.rs`; test binaries have no global allocator) under
   `LD_PRELOAD=/usr/lib/libjemalloc.so`.
2. MALLOC_CONF arms, 3 reps each: `thp:never` (emulates the 4K steady state), default, `thp:always,metadata_thp:always`,
   `hpa:true` (confirm `opt.hpa` via `stats_print:true`).
3. Measure with `perf stat -e cycles,instructions,ls_l1_d_tlb_miss.all,ls_l1_d_tlb_miss.tlb_reload_4k_l2_miss,
   ls_l1_d_tlb_miss.tlb_reload_2m_l2_miss,ls_tablewalker.dside,page-faults`, plus AnonHugePages from smaps_rollup.
4. Gate: always vs never >= 5% fewer cycles per 1k fills.
5. If it passes, one 90 s 300m cell with the best arm in the node env. mem-samples THP must stay >= 70% and exec
   on-CPU per 1k fills (schedstat) must drop >= 4%.

**Effort:** 0.5 d prototype; 2-3 d production (baked config, gauge, ops doc, n=4 std+deep A/B).

---

## 2. l3-cat-exec-partition: reserve L3 ways and DRAM bandwidth for the exec path with resctrl CAT/MBA

**Mechanism.** The 5950X supports L3 CAT, MBA and CQM/MBM. Create a resctrl group holding `torus-execution`,
`torus-exec-pool-*` and `torus-end-resident`, with an exclusive L3 capacity mask on each CCD (e.g. `L3:0=ff00;1=ff00`).
Put the ingest pools (`torus-gossip-verify-*`, `torus-ingress-verify-*`, rpc-worker, tokio), the flush worker and the
RocksDB bg threads in a second group (`00ff`) with an MBA cap.

E's per-block hot set should then survive the WAL, compaction, checkpoint-batch and ReadView traffic around it. That
hot set is the 63 senders' 31 KB position arrays, the touched book tops, the caches and the overlay.

Step 1 is monitoring only: llc_occupancy and mbm_total_bytes per group. An optional `TORUS_RESCTRL_EXEC_GROUP` hook
writes TIDs into `<group>/tasks` at thread spawn (`app.rs:8335`, `exec_pool.rs:20-37`). No CPU affinity is involved.

**Target metric:** exec on-CPU ms per 1k fills and exec DRAM fills/kinstr; matched/s; exec time p99.

**Expected gain (est.).**
- E runs ~115 M instructions per block (31.8 ms x 3.3 GHz x IPC 1.1). At 1.14 DRAM fills/kinstr that is ~131k DRAM
  fills per block.
- If CAT turns 20-30% of them into L3 hits, saving ~70 ns each at MLP ~2: **-0.9..-1.4 ms/blk (3-4.5% of E)**, so
  matched/s **+2..+4%**, plus lower exec-time variance.
- The CQM step bounds it: if E holds < 4 MB of L3 under load the upside is larger; if it already holds > 16 MB, ~0.

**Evidence.**
- CPU flags and resctrl support listed above.
- Exec L1d MPKI 14.7, DRAM 1.14/kinstr, IPC 1.03-1.23 (ozarchy-ipc, measured.md s3.1).
- The ingest side is 48.9% of node user cycles, on the same cores (q8re).
- WAL 37-55 MB/s; checkpoint batches 87-181 MB; host load1 35-45.
- Cross-CCX fills are only 0.06/kinstr: the problem is capacity, not placement.

**Nearest prior.** Grepped `pin|affinity|taskset|ccd|numa|cpuset|smt|sibling|llc|cache.?miss`. Closest entries:
- cpu-pinning-cpuset / per-node-cpu-pinning (rejected, -5.7%), hotstuff-cpus-pin (rejected);
- dedicated-cores-per-node (decided; co-location is on the owner list);
- shrink-main-tokio-runtime, verify-pools-parking-executor.

Nothing on CAT, MBA or resctrl. The difference: threads stay unpinned; only cache ways and DRAM bandwidth are
partitioned, which pinning cannot do.

**Risks.**
- On the shared bench host the 3 nodes' exec threads share the exec mask and still evict each other.
- Ingest pools on fewer ways or under MBA could become binding (work conservation says they have slack).
- Needs root/CAP_SYS_ADMIN on validators (ops change). The result may not transfer to one node per host.

**Prototype (<= 3 h; needs owner OK for root).**
1. `mount -t resctrl resctrl /sys/fs/resctrl`.
2. In a 90 s 300m cell, put each node's exec TIDs (from `/proc/<pid>/task/*/comm`) into mon groups and sample
   llc_occupancy / mbm_total_bytes at 1 Hz.
3. Arms: no CAT / 50-50 / 75-25 masks, n=2 each.
4. Measure schedstat exec on-CPU per 1k fills, `perf stat -e ls_dmnd_fills_from_sys.mem_io_local` on the exec TIDs,
   and matched/s.
5. Gate: >= 3% lower E CPU per fill.

**Effort:** 0.5 d prototype; 2 d production (optional hook, docs, A/B).

---

## 3. exec-block-arena: per-call bump arena (pre-faulted, hugepage) for the engine's transient maps and vectors

**Mechanism.** `execute_batch_phases` (NE 8281-9115) builds ~15 maps and Vecs from empty on every call, twice per
block. The exec map, section 1.4, lists them:
- flatten Vec (NE 8304-8363);
- `SenderFold` (NE 2805-2860), `BalanceCache` (NE 211-425), `PositionCache` (POS 781-790);
- `market_batches`, `basis` (NE 10919), `phase2_markets` (NE 11018), `pools`, `open_at_start`, claimed, listed;
- `deferred` (NE 10043), plans.

Each grows by doubling (rehash + memcpy) and is freed at the end of the call. Change: allocate the E-built ones from a
`bumpalo::Bump` owned by `NativeExecContext` and `reset()` it after each call:
- `hashbrown::HashMap<K,V,FoldHashState,&Bump>` through allocator-api2 (hashbrown 0.15/0.16 and bumpalo 3.20 are
  already in Cargo.lock);
- `bumpalo::collections::Vec`, presized from the batch counts.

The arena region is allocated once, madvised for hugepages and pre-touched, so there is no malloc/free, no fault and
no realloc copy. Pool-built pass-A plans stay on the global allocator.

**Target metric:** exec on-CPU ms per 1k fills; engine faults per 1k fills -> ~0.

**Expected gain (est.).**
- Engine memory overhead per 1k fills (8u) ~0.18 ms: faults 0.082 + jemalloc/wrappers 0.036 + ~half of kernel-other
  0.063 + ~30% of memmove 0.235.
- That is ~2.1 ms/blk at 11.5k fills. Removing 60-80% gives **-1.3..-1.7 ms/blk (4-5% of E)**, matched/s +3..+4%.
- About 0.6 ms of this overlaps idea 1 (faults).

**Evidence.**
- 8u-w2report.txt engine faults 30.7 per 1k fills (settle 19.8, other_engine 11.0) and memmove 0.235 ms/1k.
- q8re EXEC inclusive malloc/free 6.3%.
- Exec map section 1.4 (two calls per block, maps built from empty).

**Nearest prior.** Grepped `bump|bumpalo|arena|capacity|presiz|reuse|recycl|scratch`. Closest entries:
- s61-scratch-reuse-bundle (merged: hash/serialization scratch and presized vectors);
- executor-presize-flatten (parked, never benched), traderorders-presize (no gain), cloid-claimed-set-presize;
- staged-kv-byte-arena / consensus-kv-byte-arena / packed-trade-history-arena: storage-side arenas.

The difference: an allocator-level per-call region that covers the engine's HashMaps (rehash), not single
serialization buffers, and never returns pages to jemalloc.

**Risks.**
- Invasive lifetime and type changes (std HashMap -> hashbrown with an allocator) across the phase functions.
- Anything that escapes the call must stay on the global allocator; lifetimes enforce this.
- Adversarial block sizes need arena growth (bumpalo chunks).
- Gain is unattributed until faults and memmove are traced to call sites.

**Prototype (<= 3 h).**
1. `perf record -e page-faults -g` and a memmove callgraph on the exec TID in ubench_econ, to attribute the 30.7
   faults/1k and the memmove to allocation sites.
2. Hack-convert the 3 largest E-built structures (market_batches, SenderFold, BalanceCache) to a reset-per-call Bump.
3. ubench_econ A/B. Gate: >= 2% E wall per 1k fills and engine faults ~0.

**Effort:** 4-6 d.

---

## 4. resting-order-compaction: 176 B `Order` -> 64-80 B hot record, cold fields in a side table

**Mechanism.** `Order` (OB 107-121) is 176 B, almost three cache lines:
- `OrderType` is 48 B: an enum with two i128 stop payloads, though resting orders are almost always Limit
  (`lib.rs:1530-1540`);
- `Option<Cloid>` is 32 B;
- `price` (16 B) duplicates the BTreeMap level key;
- `original_qty` and padding make up the rest.

Store each level as `VecDeque<RestingOrder{id u128, remaining i128, original i128, trader [u8;20], ts u64,
flags u8}>`, 80 B. Rare stop payloads and cloids go to a per-book `HashMap<OrderId, Cold>`; `Order` is rebuilt on
demand for RPC and tests. `encode_order` (`classic_encode.rs:73-`) still writes the identical 97-byte record from
hot + cold + level price, pinned by the existing `classic_encode_tests` proptest.

**Target metric:** deep RSS; cancel-all pool wait and window-close time on E; matched/s on deep.

**Expected gain (est.).**
- **Deep RSS.** 1,135,555 resting x 96 B = 109 MB/node live, 140-220 MB with VecDeque slack.
- **Cancel-all and match.** Survivor compaction (`cancel_batch.rs:104-136`) and `Vec<Option<Order>>` moves shrink
  2.2x. Of the cancel-all pool wait (0.145 ms/1k in 8u), ~-0.07 ms/1k = **~-0.8 ms/blk on E**.
- **L3.1 close encode.** It reads 2.2x fewer bytes: est. -15..-30% of the deep close (61-70 ms mean, p99 126-160
  every ~100 blocks).
- **Total.** ~-1 ms/blk deep (~2.5% of E). That closes about half of the deep-vs-std +5.6% exec CPU per fill.

**Evidence.**
- Struct layout read from code.
- Deep resting count from the 9j2 deep metric.
- deep-queue-concentration: 2 queues per side hold ~99% of orders, so compactions shift long runs.
- Memory notes the save as DRAM-bound (mem eae79029).

**Nearest prior.** Grepped `order.?struct|order size|176|compact order|slim|shrink|hot.?cold|resting order|
seq-inline`. Closest entries:
- seq-inline-level-queues (L2.3, proposed): it adds a seq per order; this removes ~96 B per order;
- dense-trader-slots (L2.4, rejected);
- cancel-all-in-place-compaction (inconclusive), lazy-delete-level-queues (proposed);
- compact-order-encoding (wire format), exact-size-book-encoder-l33 (blob encode).

None of these changes the in-memory resting record.

**Risks.**
- `Order` is a pub type used across crates (RPC snapshots, tests, mode 1-3 paths).
- FOK, modify and stop paths read cold fields.
- The Classic blob must stay byte-identical (consensus-visible if it drifts; the proptest guards it).

**Prototype (<= 3 h).**
1. Scratch microbench next to `l3_savebooks_ubench.rs`: synthetic 7,000-deep levels, 1M orders.
2. Time `remove_targets`-style compaction and the `encode_order` loop on `Order` vs a compact `RestingOrder` copy.
3. Gate: >= 25% on both, and `size_of` 176 -> <= 80.

**Effort:** 5-8 d.

---

## 5. users-position-key-index: sender-hoisted, SoA-keyed position lookup in `Users`

**Mechanism.** Each `AccountReader::get_position` (NE 2554-2575) pays:
- a drain-dirty HashSet probe (NE 2521-2531);
- `Users::get`, which does an opaque probe and the `map` probe (`users.rs:705-710`): 3 foldhash passes over a 20-byte
  address in all;
- `find`, a `binary_search_by_key` over `Vec<Position>` (`users.rs:1537-1539`) with a 112-B element stride.

Active bench traders hold ~280 positions, so a search does ~8 probes across a 31 KB, 8-page array. Change:
- `UserState` keeps `keys: Box<[MarketId]>` parallel to `positions`: 2.2 KB, one page, the last probes in one line;
- Phase 2 caches the sender's clean record slice in `SenderState` (`fold.state`, NE 2805-2860), so
  `st.proj.get_or_try_insert` (NE 9267) does only the key search per market;
- `end_resident` apply keeps both arrays in step.

**Target metric:** E margin/prepare ms/blk; pool match/pass-A wall.

**Expected gain (est.).**
- Prepare calls `get_position` once per (sender, market): 63 actions x ~260 markets ~= 16k calls/blk on E.
- get_position is 5.0% of E (q8re), i.e. ~1.6 ms/blk.
- Hoisting removes 3 hash probes per call: 16k x 3 x 15-20 ns = 0.7-1.0 ms. The key array halves the remaining search
  misses.
- Total **-0.9..-1.2 ms/blk (3-4% of E)**, plus pool maker lookups (AccountReader::get_position 1.43% + Users::position
  1.28% of node cycles, 74% of them on the pool). Matched/s +2..+3%.

**Evidence.**
- Resident users gauge 1,605,060 rows / 214 MB (std) and 2,505,060 / 262 MB (deep), i.e. ~1.4M positions over ~5k
  active senders.
- q8re EXEC inclusive get_position 5.0%, Users::row 1.2%.
- Crab-era positions_for_trader ran at IPC 0.65 with L1d 30 MPKI (ozarchy-ipc): these per-trader walks are miss-bound.

**Nearest prior.** Grepped `binary.?search|eytzinger|position.?index|userstate|get_position|typedrows|hoist|prefetch|
hasher`. Closest entries:
- adl-holder-record-slice (proposed; ADL only);
- users-record-per-trader (Phase 4, merged: the current layout), users-btreemap-shape (rejected);
- resident-caches-hoist-prefetch (Aug, before Users existed);
- cheaper-address-hasher (proposed), dense-trader-slots L2.4 (rejected).

The difference: this changes the record layout and the per-sender hoist, not the hasher, and does no per-job
interning.

**Risks.**
- Gain is small if clone and compute dominate get_position.
- Phase 5 (excluded) may rework Users mutation.
- The hoist must respect the dirty fallback.

**Prototype (<= 3 h).**
1. Microbench beside `ubench_users_rss.rs`: 5k traders x 280 positions plus 100k light traders.
2. Run 16k sender-grouped lookups and 11.5k random maker lookups: current vs SoA keys vs hoisted, with perf stat L1d
   misses.
3. Gate: >= 40% per-lookup cut, then ubench_econ.

**Effort:** 2-3 d.
