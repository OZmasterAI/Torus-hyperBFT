# s82: execution-path hasher (SipHash → ahash) — ~3% node CPU, below A/B resolution, parked — 2026-09-30

Branch `perf/s82-exec-hasher` `3dabdb4` (main `9c80e29` + 1, local only,
parked). Campaigns `~/bench-results-matched/s82-hash-20260930` (A/B) and
`~/bench-results-matched/s82-prof-20260930` (profiles of both binaries).

## Summary

- The s77 profile put std `RandomState` SipHash at ~9% of the execution
  thread (`s77-s80-trade-history-and-streams-2026-09-30.md` §1).
- The 4+4 A/B of seeded ahash on the execution-path maps showed no
  measurable change: node CPU-s/1M +0.9% ± 2.3%, matched/s +0.4% (p=0.91).
- Profiles of both binaries (§ Profiles) show the change works in code:
  SipHash 2.58 → 0.19 CPU-s/1M/node, all hash-map code 3.06 → 1.88. The net
  saving, ~3% of node CPU (~2.5% on the A/B's whole-cell scale), is at or
  below what a 4+4 A/B can resolve. No throughput gain.
- The A/B's primary metric (exec-thread CPU-s/1M from `pidstat -t`) could not
  see the saving: 83% of the SipHash ran in short-lived per-block worker
  threads that pidstat's per-thread lines miss (§ Profiles).
- Nothing changes for traders or API users. Parked, not merged.

## Change

`torus_core::fast_hash::{FastMap, FastSet}` = std `HashMap`/`HashSet` with
`ahash::RandomState` (`runtime-rng`, seeded from the OS RNG). Used by the
order book maps, `PositionCache`, `BalanceCache`, the executor's
book/margin maps, the per-batch maps and the market-worker batches.
Startup-only maps stay on std. Workspace tests: 1977 pass, 2 known torus-core
failures, 30 ignored.

Why ahash: traders choose Address and price keys, so the hasher must be
seeded. In this workspace alloy's `AddressHashMap`/`Fb*` maps are
**unseeded** FxHash (alloy's `rand` / `map-fxhash` features are off), and
foldhash is seeded only from ASLR + time. The std SipHash being replaced is
already seeded and is the stronger keyed hash, so the change has no security
benefit.

### Where SipHash time went (s77 dwarf stacks, `hash_one::<K>` frames)

| key | share of SipHash samples on `torus-execution` |
|---|---|
| `u128` order id | 43% |
| `(Address, MarketId)` position | 32% |
| `Address` | 19% |
| `(u8, i128)` price level | ~0% |

Hash-only microbenchmark on this host (ns per key, no `target-cpu`):

| key | SipHash | foldhash | Fx | ahash |
|---|---|---|---|---|
| `u128` | 17.8 | 1.2 | 1.1 | 2.0 |
| `(Address, u64)` | 29.3 | 4.6 | 2.8 | 3.6 |
| `Address` | 22.7 | 4.0 | 2.2 | 2.8 |

## A/B

main `9c80e29` (torus-node sha256 `7b9013e0`, byte-identical to the s80ab2
branch binary) vs branch `3dabdb4` (sha256 `4a3025d9`). s80ab2 recipe: rate
76000, 300 s, cap 400, 10 markets, same bench-throughput binary (s75,
`51e512e5`). ABBA order r1..r4 plus a warm-up on main (excluded). All 9
cells accepted, AGREE. No cell LOADGEN-SUSPECT (median submit 421
actions/s, threshold 337/s).

| metric | main r1 / r2 / r3 / r4 | branch r1 / r2 / r3 / r4 | branch − main | Welch p |
|---|---|---|---|---|
| matched/s (k) | 92.0 / 96.8 / 100.0 / 89.3 | 99.5 / 91.8 / 98.1 / 90.3 | +0.4% | 0.91 |
| total matched (M) | 37.3 / 38.8 / 39.6 / 36.2 | 38.9 / 36.0 / 39.4 / 36.0 | −1.1% | 0.73 |
| node CPU-s/1M | 48.5 / 44.9 / 45.4 / 49.5 | 45.6 / 49.4 / 45.5 / 48.9 | +0.6% | 0.87 |
| exec-thread CPU-s/1M | 5.14 / 4.71 / 4.80 / 5.17 | 4.73 / 5.21 / 4.63 / 5.01 | −1.2% | 0.75 |
| engine ms / 1k fills | 6.63 / 6.28 / 6.30 / 6.73 | 6.15 / 6.79 / 6.19 / 6.70 | −0.4% | 0.90 |
| best 60 s (k) | 138.6 / 155.3 / 141.8 / 133.2 | 136.4 / 128.8 / 132.2 / 123.2 | −8.5% | 0.08 |
| exec age p50 (s, val0) | 65.2 / 62.5 / 56.4 / 67.5 | 56.5 / 52.5 / 56.1 / 54.9 | −12.5% | 0.04 |
| gossipsub Send Queue full | 0 / 0 / 0 / 0 | 2 / 80 / 20 / 0 | | 0.27 |

Adjusted for cell throughput (least squares on matched/s plus an arm term):
exec-thread CPU-s/1M −0.9% ± 3.7%, node CPU-s/1M +0.9% ± 2.3%, engine
ms/1k fills −0.1% ± 2.3%. No thread's CPU-s/1M moved by more than ~4%.

- Exec age p50 is the only metric with p < 0.05, out of ~17 compared. It
  has no CPU-side mechanism (exec CPU and engine time are flat, the queue is
  still pinned at 64 blocks), and it does not track fills per block (r =
  0.04) or blocks/s (r = 0.25). Treated as chance.
- The branch binary does contain the change (73 `ahash::` symbols vs 11).

## Profiles (`s82-prof-20260930`)

One cell per binary, s77-prof recipe (`perf-watch.sh`: `perf record -a -F 99`
for 60 s from 90 s into the bench, then 15 s `--call-graph dwarf` on the node
pids, here with `-m 2048`: no lost events, s77 lost 17%). main
`s82prof-main-r1` (88.9k matched/s, AGREE; `perf/`), branch
`s82prof-branch-r1` (98.9k, AGREE; `perf-branch/`). Symbols: `sudo perf
script -f`, then `c++filt` (binutils 2.42 demangles Rust v0).

### Current main, where CPU goes (profile window, 92.6k matched/s/node)

| area | s77 `07a34c2` | s82 main | share now |
|---|---|---|---|
| execution (long-lived thread + per-block workers) | 15.0 | 13.4 (3.4 + 10.0) | 34% |
| signature verify (gossip + ingress) | 13.5 | 9.0 | 23% |
| storage (rocksdb low + high, flush, trade writer) | 23.2 | 9.7 | 25% |
| tokio, hotstuff, rpc, other | 8.6 | 7.2 | 18% |
| total, CPU-s/1M/node | 60.3 | 39.3 | |

- **Per-block worker threads.** `drain_book`, `match_parallel_capped` and
  `settle_market_results_parallel` run on scoped threads spawned for every
  block (~1,300 distinct thread ids per node sampled in 60 s). They inherit
  the name `torus-execution` and hold three quarters of execution CPU. The
  spawning itself is cheap: page clearing and spin-lock samples are 0.24% and
  0.55% of node samples, mostly RocksDB appends and condvar waits.
- **Measurement gap.** `pidstat -t` (host sampler, 1 s) prints per-thread
  lines only for threads alive at a sample, so these workers are missing
  from the per-thread tables (the "exec-thread" rows here and in s77-s80).
  Their CPU is in the process total: in s82hash the per-thread sum is 34.2
  CPU-s/1M against a node total of 47.1.
- SipHash is 6.6% of node CPU on main, 83% of it in the workers: `order_seq`
  / `order_index` / `row_exists` inserts in `insert_order`, `PositionCache`,
  `trader_orders`.
- Keccak (flat) 15.8% of node CPU, mostly `eip712_struct_hash` on the verify
  threads. The duplicate `compute_action_hash` (pool insert + DA
  `put_batch`) is only ~0.4-0.8%. ZSTD (compaction) ~3.8%.

### main vs branch, hash-map code (CPU-s/1M/node)

| | main | branch |
|---|---|---|
| SipHash symbols | 2.58 | 0.19 |
| all hash-map symbols (SipHash, ahash, hashbrown insert/probe) | 3.06 | 1.88 |

The table work (probe, insert, remove) stays, and ahash is not free
(`HashMap<u128, u64>::insert` 0.47, `HashMap<u128, OrderLocation>` 0.41 on
the branch), so the saving is 1.2 CPU-s/1M, not the whole SipHash share.
Whole-window totals are not comparable between two single profiled cells:
the branch window came out at 50.8 CPU-s/1M against 39.4, driven by gossip
verify, compaction and tokio, which the change does not touch.

## Conclusion

- The change does what it should in code (~3% of node CPU) but gives no
  throughput and is below the 4+4 A/B's resolution. Parked on
  `perf/s82-exec-hasher`.
- A/B CPU comparisons must use the process total (`node total`), not
  per-thread rows, while execution runs on per-block threads.
- The remaining CPU targets from this profile are all small: duplicate
  `compute_action_hash` <1%, per-block thread spawning <1%, ZSTD ~4%.
- Stats: `stats.txt` / `stats.py` in the A/B campaign dir; profiles `agg.txt`,
  `flat.txt`, `dwarf.dm.txt` in the profile campaign dir.
