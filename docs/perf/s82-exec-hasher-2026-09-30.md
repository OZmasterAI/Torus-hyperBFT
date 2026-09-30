# s82: execution-path hasher (SipHash → ahash) — no effect, not merged — 2026-09-30

Branch `perf/s82-exec-hasher` `3dabdb4` (main `9c80e29` + 1, local only,
parked). Campaign `~/bench-results-matched/s82-hash-20260930`.

## Summary

- The s77 profile put std `RandomState` SipHash at ~9% of the execution
  thread (`s77-s80-trade-history-and-streams-2026-09-30.md` §1).
- Replacing it with seeded ahash on every execution-path map gave **no
  measurable change**: exec-thread CPU-s/1M −0.9% ± 3.7% (95%, adjusted for
  cell throughput). The ~7% saving predicted from the profile is excluded.
- Nothing changes for traders or API users. Not merged.

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

## Conclusion

- The s77 SipHash share does not translate into a saving on current main.
  That profile ran on `07a34c2` (before hash dedup and packed rows), and its
  dwarf capture lost 17% of events.
- Re-profile current main before sizing the other CPU targets (duplicate
  `compute_action_hash`, compaction ZSTD).
- Stats: `stats.txt` / `stats.py` in the campaign dir.
