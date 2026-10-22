# s80 prototype thp-heap: huge-page-backed node heap by jemalloc config (measured-only, zero node code)

Branch `s80/thp-heap` (local, base 29bfc705), commits `4d3c16a9` and `61372703` (measurement tools only, no node code). Host ozarchy
(Ryzen 9 5950X, 62 GB). Write-up: `ideas/r1-hw-memory.md` section 1.

## Verdict

**Cell-worthy. Hand the Cell stage one arm:**
`MALLOC_CONF=hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152` (arm E below).

- In the engine proxy at steady state, arm E cuts engine ms/1k fills by **2.2%** against jemalloc defaults (n=3+3,
  ranges do not overlap).
- perf counters show the mechanism: L1-dTLB misses and page-table walks per kinstr fall by 60%, page faults by 46%,
  and user cycles by 2.8%.
- Heap THP share: 88-94% with arm E, 49-56% with the defaults.
- Cost: anonymous memory rises 18% in the proxy.
- The arm the write-up named first, `thp:always,metadata_thp:always`, does not keep the heap huge-page backed on
  this host. Its THP share and speed match the defaults, so it does not need a cell.
- The node runs at 22% THP in a cell, below the proxy default's 49%. Its potential gain is therefore at least the
  proxy's. Expected cell gain: matched/s +1..+3%. Watch the tail latencies (exec p99, commit p99) and RSS.

## Premise check (29bfc705)

- Host THP: `enabled=[always]`, `defrag=[madvise]`, khugepaged `defrag=1`, `max_ptes_none=511`. Because defrag is
  madvise, a fault outside a madvised region never compacts and gets a 4K page once a hugepage has been split.
- Node allocator (re-located on 29bfc705):
  - `crates/torus-node/Cargo.toml:61-69`: default feature `jemalloc`, `tikv-jemallocator` 0.7 unprefixed, so it also
    serves RocksDB.
  - `crates/torus-node/src/main.rs:41-42`: `#[global_allocator]`.
  - `crates/torus-node/src/allocator.rs`: stats gauges only.
  - Cargo.lock: tikv-jemalloc-sys `0.7.1+5.3.1-0-g81034ce1`.
  - Build: `--with-private-namespace=_rjem_ --enable-stats`, no `--with-malloc-conf`, so `config.malloc_conf` is
    "". The binary contains the plain `MALLOC_CONF` string, so the env var configures it.
- Heap THP collapse. 9j2 std r2 CL21 val0 (`~/bench-results-matched/ozarchy-9j2-std-r2-CL21/mem-samples.txt`):
  - before load: 96% AnonHugePages/Anonymous;
  - under load: 88 -> 73 -> 53 -> 43 -> 33 -> 29% within ~45 s;
  - then 27-36% (~3.0 GB anon, 0.8-1.2 GB THP).
  - The write-up's 22% steady value and the deep run's 23% are consistent with this.
  - **Premise holds.**
- No `MALLOC_CONF` in those cells (`node-env-all.txt`, `node-preload.txt`), so the node ran jemalloc defaults:
  - `opt.thp "default"`, `dirty_decay_ms 10000`, `muzzy_decay_ms 0`, `retain true`, `narenas 128`, `hpa false`.

## Option validity on the node binary

Method: run `torus-node --version` (md5 f4f2679f, = 29bfc705 release) with
`MALLOC_CONF=<arm>,stats_print:true`, then check stderr for `<jemalloc>: Invalid conf` and read back the `opt.*`
values from the dump.

| conf | accepted | read back |
| --- | --- | --- |
| `thp:always,metadata_thp:always` | yes | opt.thp "always", opt.metadata_thp "always" |
| `hpa:true` | yes | opt.hpa true, opt.hpa_hugify_style "lazy" (default) |
| `hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152` | yes | opt.hpa true, style "eager", purge_threshold 2097152 |
| `hpa:true,hpa_hugify_style:none,hpa_dirty_mult:-1` | yes | style "none", dirty_mult "-1" |
| `hpa:true,metadata_thp:always,background_thread:true` | yes | all three set |
| `thp:bogus` (negative control) | **no** | `<jemalloc>: Invalid conf value: thp:bogus` on stderr |

This jemalloc build is a 5.3.1 dev snapshot. Its HPA has the `hpa_hugify_style` auto/none/eager/lazy options and a
purge threshold (`src/conf.c:842-956`, `include/jemalloc/internal/hpa_opts.h`).

## Design (no code)

- The cell's node env gets `MALLOC_CONF=<value>`.
- `run-cell.sh` (best tools) exports every `EXTRA_ENV` word before `launch-3val.sh` (`:1448`). It records the
  words in `run-id.json` `extra_env` and on the `cell=` log line. `start-node.sh` starts the node with `nohup`,
  so the node inherits the env.
- `bench-throughput` is glibc (no jemalloc strings, plain libc in `ldd`) and ignores `MALLOC_CONF`, so only the
  nodes change.
- jemalloc prints nothing for a valid conf. The activation proof is therefore the sampler below, which reads
  MALLOC_CONF from `/proc/<pid>/environ` and AnonHugePages from `/proc/<pid>/smaps_rollup`, plus `run-id.json`.
- How arm E works:
  - HPA serves small extents (<= 64 KiB slabs) from hugepage-sized pageslabs.
  - `eager` madvises a pageslab HUGEPAGE as soon as it is mapped.
  - `hpa_purge_threshold=2 MiB` purges only fully empty hugepages, so purges stop splitting THPs.
  - Larger extents still go through PAC with the default decay.

## Proxy measurement (engine path)

The engine microbench `ubench_econ` has no global allocator (glibc). It ran under
`LD_PRELOAD=/usr/lib/libjemalloc.so`. That is Arch jemalloc 1:5.3.1-3, whose version string
`5.3.1-0-g81034ce1f1373e37dc865038e1bc8eeecf559ce8` is identical to the node's bundled jemalloc. One `MALLOC_CONF`
per arm. THP share sampled every 1 s from smaps_rollup of the ubench pid (`tools/s80/thp-sampler.sh`).

Arms:
- D = jemalloc defaults (= node today)
- N = `thp:never` (4K bound)
- A = `thp:always,metadata_thp:always`
- H = `hpa:true`
- E = `hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152`
- G = glibc (no preload; the ub.sh calibration path)

### Run 1, short (ub.sh defaults: UB_WARM=40 UB_MEASURE=6 UB_RUNS=5)

4 rounds in balanced order `D N A H E G | G E H A N D | D N A H E G | G E H A N D`. Each value is the MEDIAN
engine ms/1k fills of 5 runs.

| arm | medians | mean | vs D | THP mean | anon MB |
| --- | --- | --- | --- | --- | --- |
| D | 2.56 2.57 2.54 2.55 | 2.555 | | 55.8% | 414 |
| N | 2.73 2.70 2.74 2.72 | 2.723 | +6.6% | 22.9% | 372 |
| A | 2.57 2.57 2.54 2.58 | 2.565 | +0.4% | 57.4% (r1-3) | 405 |
| H | 2.57 2.54 2.59 2.58 | 2.570 | +0.6% | 71.3% (r1-3) | 454 |
| E | 2.61 2.54 2.49 2.50 | 2.535 | -0.8% | 93.9% (r1-3) | 485 |
| G | 2.86 2.82 2.72 2.78 | 2.795 | +9.4% | 21.6% | 331 |

(The round-4 THP samples of A/H/E caught the concurrent steady run's process, which uses the same binary. They are
excluded; the engine medians are unaffected.)

### Run 2, steady (UB_WARM=200 UB_MEASURE=12 UB_RUNS=3, one lock hold per round)

Rounds `D E N H | H N E D | D E N H`. The measured blocks sit after 200 warm blocks, where the default heap has
fragmented.

| arm | medians | mean | vs D | THP mean (min) | anon MB |
| --- | --- | --- | --- | --- | --- |
| D | 2.55 2.58 2.56 | 2.563 | | 49.3% (34.6) | 599 |
| N | 2.59 2.61 2.62 | 2.607 | +1.7% | 16.0% (11.5) | 559 |
| H | 2.53 2.54 2.56 | 2.543 | -0.8% | 66.6% (55.5) | 657 |
| E | 2.50 2.50 2.52 | 2.507 | **-2.2%** | 87.7% (83.2) | 709 (+18%) |

- Engine time falls in step with THP share: N > D > H > E.
- The E and D ranges do not overlap: E max 2.52 < D min 2.55.
- Per-phase (run 1 of each arm, ms/1k): exec 2.10 / 2.08 / 2.15 (D/E/N) and settle 0.69 / 0.68 / 0.69. The gain is
  spread across phases, with no single phase standing out.

### Run 3, perf stat (user-space counters, perf_event_paranoid=2, same steady config, one lock hold, order D E N E D N)

| label | engine median | G cycles | G instr | IPC | L1-dTLB miss /kinstr | 4K L2-TLB miss /ki | 2M L2-TLB miss /ki | table walks /ki | page faults |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| D1 | 2.60 | 206.51 | 317.32 | 1.54 | 2.082 | 0.073 | 0.006 | 0.233 | 89k |
| D2 | 2.61 | 208.93 | 318.25 | 1.52 | 2.128 | 0.074 | 0.007 | 0.245 | 90k |
| E1 | 2.54 | 201.92 | 316.07 | 1.57 | 0.836 | 0.013 | 0.014 | 0.091 | 47k |
| E2 | 2.56 | 202.02 | 316.33 | 1.57 | 0.865 | 0.014 | 0.012 | 0.095 | 50k |
| N1 | 2.65 | 208.36 | 316.99 | 1.52 | 3.432 | 0.314 | 0.006 | 0.415 | 551k |
| N2 | 2.66 | 211.30 | 317.67 | 1.50 | 3.549 | 0.318 | 0.006 | 0.419 | 562k |

E vs D:
- whole process: user cycles -2.8% (201.97 vs 207.72 G) at equal instructions (-0.5%);
- dTLB misses/ki -60%; walks/ki -61%; page faults -46%;
- engine median -2.1%.

So the mechanism the write-up proposed is real, but its size is about 1/3 to 1/4 of the write-up's 7-11% estimate.
The skeptic's 0..+2% is closer.

### Golden (allocator independence)

`perf_equivalence_golden` (release test binary `perf_equivalence_golden-ff91b379bcea1cbc`, 5 tests, checked-in
digests) passes 5/5 under all four allocators:
- glibc (off);
- LD_PRELOAD jemalloc defaults;
- jemalloc + arm E (on, `opt.hpa: true` confirmed by stats_print);
- jemalloc + arm A.

Logs: `results/thp-golden-{glibc,je,je-E,je-A}.log`. No node code changed, so no crate tests were affected.

## Cell stage instructions

```
# 1. start the THP sampler for the cell's own unit (cgroup-filtered; records MALLOC_CONF, Anonymous, AnonHugePages, Rss)
UNIT=bench-s80-thp-heap-<round> /home/oz/projects/wt/novel-s80/proto/thp-heap/tools/s80/thp-sampler.sh \
  'release/torus-node --genesis' /home/oz/projects/wt/novel-s80/proto/results/thp-cell-<round>.tsv 5 10800 1800 &
# 2. the cell (ON arm); the OFF arm is the same line without MALLOC_CONF (with its own sampler, same UNIT pattern)
TOOLS=best /home/oz/projects/wt/novel-s80/proto/cell.sh thp-heap <round> MALLOC_CONF=hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152
```

- Activation proof:
  - `results/s80-thp-heap-<round>/run-id.json` `.extra_env` contains the MALLOC_CONF;
  - the sampler TSV column 7 shows it for all 3 node pids;
  - THP share (column 5) should hold at >= 70% under load for ON and fall to ~25-35% for OFF.
- The target dir `~/.cargo-target-s80-thp-heap` holds the unchanged 29bfc705 `torus-node` (md5 f4f2679f) and the
  `bench-throughput` the cell needs (md5 fa8fd4bd). Both arms use the same binary.
- Watch:
  - matched/s and blk/s;
  - exec p99 and commit p99. With host `defrag=madvise`, a madvised region compacts directly on fault, so stalls
    would show in the tails;
  - RSS (sampler column 6): the proxy predicts +18% anon, about +0.5 GB per node at ~3 GB.

## Risks and notes

- HPA is marked experimental upstream (Meta runs it in production). If it has a crash bug, the cell would fail
  validity.
- Any production adoption should bake the config at build time with `JEMALLOC_SYS_WITH_MALLOC_CONF` (the build
  script reruns on it), add a THP gauge, and A/B a deep cell for the RSS guard.
- Not tried: `hpa_slab_max_alloc` above 64 KiB (would route more extents through HPA), `background_thread:true`
  (moves HPA purge/hugify work off the hot threads), arena-per-thread binding (write-up arm c), and host
  `defrag=defer` (an ops change, outside a MALLOC_CONF A/B).
- Proxy limits:
  - The ubench heap is ~0.6 GB; the node's is ~3 GB plus RocksDB, and its THP baseline is lower (22% vs 49%).
  - The proxy's jemalloc is the system .so with the same version, not the static build. Same code version; the
    configure options may differ.

## Files

- `tools/s80/thp-sampler.sh`, `thp-ub-ab.sh`, `thp-ub-steady.sh`, `thp-ub-perf.sh`, `thp-ub-summary.py` (branch
  `s80/thp-heap`).
- Raw data in `/home/oz/projects/wt/novel-s80/proto/results/`:
  - `ub-thp-heap-{ab,st,pf}-*.log`: per-run breakdowns and MEDIAN lines;
  - `thp-ub-{ab,st}-*.tsv`: THP samples;
  - `thp-perf-pf-*.csv`: perf stat;
  - `thp-ub-ab.out`, `thp-ub-steady.out`, `thp-ub-perf.out`.
  - `thp-ub-ab.out` ends with a bash syntax error. The driver was edited while it ran and bash read the new tail
    after the loop. It is harmless: all 24 invocations had finished.

## Cell stage (120 s CL21, 300 markets, TOOLS=best, ABBA)

Setup checks:
- The binary is built from the final commit 61372703. Under the lock, `cargo build --release --locked -p torus-node` at
  HEAD 61372703 was a no-op (0.5 s), and md5 stayed f4f2679f. The branch commits touch only `tools/s80/`, so the node
  code is 29bfc705's.
- Every cell's `run.log` shows torus-node src md5 f4f2679f598ce0d9810eb796227561b8, bench-throughput md5
  fa8fd4bd764b8b5aa6c427a3679c4f8b, and genesis md5 329a974c (cache `genesis-cache-0f8eedb3`).
- Order: off-1 07:58-08:04, on-1 08:22-08:25, on-2 08:25-08:31, off-2 08:31-08:37.
  - off-1 came from an earlier start of this stage's driver (`results/thp-cell-ab.sh`). That cell ran to completion
    (summary.json, ACCEPT/AGREE, same binary), but the driver died before it logged rc. Its THP sampler died with it
    after 10 s.
  - So off-1 is reused, and `results/thp-cell-ab2.sh` ran the other three cells. That makes exactly 4 cells. Driver
    log: `results/thp-cell-ab.out`.
- ON env word: `MALLOC_CONF=hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152`. OFF: no MALLOC_CONF.

### Rows

Columns: label, validity, agreement, matched_s_avg, matched_s_first120, blk_s_avg, chain_ms, engine_ms/1k, commit
p50, commit p99, exec p99, user ms/1k fills.

```
s80-thp-heap-off-1  ACCEPT  AGREE  257237.5  259059.7  19.6  36.9   2.62  639.9  6581.0  9748.1  19.632
s80-thp-heap-on-1   ACCEPT  AGREE  265911.1  269854.0  20.7  37.3   2.7   492.9  7363.6  9671.0  19.762
s80-thp-heap-on-2   ACCEPT  AGREE  265164.2  265073.7  18.2  40.72  2.65  841.2  7945.8  9786.8  19.446
s80-thp-heap-off-2  ACCEPT  AGREE  269128.3  269369.9  18.3  41.16  2.63  879.7  7673.7  9842.6  19.504
```

All 4 cells are ACCEPT/AGREE. The ON arm has no correctness failure.

### Activation proof

- The prescribed grep finds nothing:
  `zcat results/s80-thp-heap-on-1/val0.log.gz | grep 'MALLOC_CONF=hpa:true,...'` gives **0 matches** (val1, val2,
  on-2 and the OFF cells also give 0). This is expected, not a sign that the arm was inactive. The prototype has no
  node code, so the node never logs its env, and jemalloc prints nothing for a valid conf.
- These three sources show the arm was active:
  1. `results/s80-thp-heap-on-{1,2}/run.log`: the `cell=` line's `extra_env=` ends in
     `MALLOC_CONF=hpa:true,hpa_hugify_style:eager,hpa_purge_threshold:2097152`. The OFF cells' extra_env has no
     MALLOC_CONF.
  2. THP sampler (`results/thp-cell-on-{1,2}.tsv`):
     - It is cgroup-filtered to the cell's own unit and reads `/proc/<pid>/environ`.
     - In each ON cell, all 102 rows across the 3 node pids carry the full MALLOC_CONF string. The OFF rows show `-`.
  3. The allocator behaves differently. Heap THP share under load is 77% ON vs 34% OFF (table below). The node's own
     jemalloc gauges (`metrics-after-val0.txt`) show the HPA footprint: resident 2.96 / 3.03 GB ON vs 1.62 / 1.57 GB
     OFF; retained 2.95 / 2.73 GB vs 6.05 / 5.99 GB; mapped 1.16 / 1.06 GB vs 1.88 / 1.83 GB. Allocated is equal
     (1.05-1.17 GB).

### THP share and memory (sampler, 5 s, 3 node pids, samples at t >= 60 s after the unit started, i.e. under load)

| cell | THP % mean | THP % min | anon MB mean | anon MB max | RSS MB max |
| --- | --- | --- | --- | --- | --- |
| off-1 | (sampler died at t=10 s: 95 -> 86%, pre-load) | | | | |
| on-1 | 76.9 | 71.6 | 4245 | 5725 | 5757 |
| on-2 | 77.4 | 72.5 | 4302 | 5479 | 5513 |
| off-2 | 33.8 | 22.7 | 3124 | 4315 | 4350 |

This matches the proxy (D 49%, E 88%), and the node's OFF value matches the 9j2 baseline (22-36%). The memory cost
is larger than the proxy's +18%: anon under load is **+37%** (about +1.15 GB per node), and anon max is +30%.

### Means per arm (n=2 each) and the delta

| metric | OFF mean | ON mean | ON vs OFF |
| --- | --- | --- | --- |
| matched_s_avg | 263,183 | 265,538 | +0.89% |
| matched_s_first120 | 264,215 | 267,464 | +1.23% |
| blk_s_avg | 18.95 | 19.45 | +2.6% |
| chain_ms | 39.03 | 39.01 | -0.05% |
| engine ms/1k fills | 2.625 | 2.675 | **+1.9% (worse)** |
| commit p50 ms | 759.8 | 667.1 | -12% |
| commit p99 ms | 7,127 | 7,655 | +7.4% (worse) |
| exec p99 ms | 9,795 | 9,729 | -0.7% |
| user ms/1k fills (val0 proc) | 19.568 | 19.604 | +0.2% |
| sys ms/1k fills (val0 proc) | 1.695 | 1.622 | -4.3% |

Caveats:
- n=2 per arm. The usual noise for matched/s is about 1%, but the OFF arm here spans 4.6% (off-1 257.2k vs off-2
  269.1k). The ON arm spans 0.3%.
- So the matched/s gain of +0.9% (+1.2% on first120) is well inside the OFF arm's own spread and is not a measured
  gain.
- blk/s, commit p50 and both p99s are much noisier (blk/s ranges 18.2-20.7 inside each arm) and say nothing either
  way.
- The two most direct measures of the mechanism do not move in the ON arm's favour:
  - engine ms/1k gets worse: ON 2.65-2.70 vs OFF 2.62-2.63, with ranges that do not overlap.
  - node user CPU per 1k fills is flat (+0.2%).
  - The proxy had predicted engine -2.2% and user cycles -2.8%.

### ON arm vs the bar (9j2 CL21, 120 s: 270.3k matched/s, 18.1 blk/s)

- matched_s_avg: 265.5k is **-1.8%** below the bar. matched_s_first120: 267.5k is -1.0% below.
- blk/s: 19.45 is +7.5% above the bar, inside the blk/s noise.
- The same-session OFF arm (the unchanged 29bfc705) also sits below the bar, at 263.2k (-2.6%).
- Neither ON cell beats the bar on matched/s: 265.9k and 265.2k.

### Cell verdict

**No measurable gain; do not adopt.**
- The arm works as configured: the heap stays 72-78% huge-page backed under load, against 23-34% with the defaults.
- But in the real node that does not turn into engine or CPU savings:
  - engine ms/1k is +1.9% worse, with non-overlapping n=2 ranges;
  - user ms/1k is flat;
  - matched/s +0.9% is inside the 4.6% OFF spread.
- It costs about +37% anon memory (+1.1 GB per node) under load, and the jemalloc resident gauge nearly doubles after
  the run.
- Likely reason the proxy gain did not carry over:
  - the node's hot path shares the cores and the TLB with RocksDB, the consensus threads and 128 arenas;
  - the 2-3% dTLB-side saving the proxy measured on a single-threaded 0.6 GB heap is below cell noise here, or is
    eaten by HPA's own hugify/purge work on the hot threads.
- If anyone revisits it: try `background_thread:true` (moves HPA maintenance off the hot threads), with n >= 3 per
  arm and an RSS guard.

Raw data: `results/s80-thp-heap-{off-1,on-1,on-2,off-2}/` (summary.json, run.log, metrics-after-val*.txt),
`results/thp-cell-{off-1,on-1,on-2,off-2}.tsv`, `results/thp-cell-ab.out`, drivers `results/thp-cell-ab.sh` (off-1)
and `results/thp-cell-ab2.sh` (on-1, on-2, off-2).
