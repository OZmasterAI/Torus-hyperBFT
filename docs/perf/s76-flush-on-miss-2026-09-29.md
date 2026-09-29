# s76 item 6 step 2: DA batch read flushes only on a miss — 2026-09-29

Branch `perf/s76-flush-on-miss`, `bce54d0`, on main `9910bc2`.

## Change

`Mempool::get_native_da_batch(_timed)` used to flush the whole ingress
DA-mirror buffer (~33 bodies, mostly other blocks') before every read. It now
reads first; only when a body is absent does it flush and re-read the misses.
A present body is as durable as a flush would make it (same DB and write
options, as `NativeDaStore::missing` relies on since s68), so S459 holds: a
body that exists only in the buffer is still a miss and is flushed before the
vote. `torus_validate_block_da_flush_seconds_count` now counts validates that
flushed.

## Cells

Same env, rate, duration and bench binary as the step 1 cells
(`docs/perf/s76-validate-timers-2026-09-29.md`). B cells only; A = the two
step 1 timer cells (code = main `9910bc2`), run about 1.5 h earlier.

| cell | binary | time | matched/s | accepted / agree / liveness |
| --- | --- | --- | --- | --- |
| vt2-timers-r1 (A) | af00202 | 11:18 | 65,854 | yes / AGREE / PASS |
| vt2-timers-r2 (A) | af00202 | 11:29 | 64,760 | yes / AGREE / PASS |
| fom-r1 (B) | bce54d0 | 13:04 | 68,497 | yes / AGREE / PASS |
| fom-r2 (B) | bce54d0 | 13:15 | 57,685 | yes / AGREE / PASS |

Throughput is within cell spread (main alone gave 61.9k and 71.1k in the same
morning); fom-r2 had the worst disk of the six cells (`w_await` 27.6 ms vs
16.7-18.6). No throughput claim either way.

## Result (per node, measured)

| | A | B |
| --- | --- | --- |
| compact reconstructs that flushed | 100% | 0.4-6.5% |
| DA reconstruct mean / p50, ms | 47-54 / 31-37 | 20-23 / 15-16 |
| missing-body waits (all nodes, 2 cells) | 10 | 8 |
| `validate_block_seconds` sum per node, s | 16.6-18.5 | 9.6-11.4 |

Body-late next-leader views (39-44% of views), pooled:

| ms | A r1 / r2 | B r1 / r2 |
| --- | --- | --- |
| validate p50 | 40 / 40 | 25 / 26 |
| validate mean | 52 / 55 | 33 / 35 |
| commit feed p50 | 103 / 93 | 86 / 97 |
| receive -> produce_block p50 | 154 / 157 | 133 / 157 |
| receive -> produce_block mean | 209 / 216 | 174 / 174 |

Validate drops ~15 ms p50 / ~19 ms mean per late view, ~8 ms per view on
average. Proposer block build (incl. mirror and select), commit-persist write
and thread CPU per 1M matched are unchanged.

MissingData: fom-r2 had one on val1 and one on val2. This is the base rate,
not the change: main-r1 had 1 and step 1's vt-timers-r2 had 5 (s75 unpinned
cells: 0).

## Tests

Mempool: `durable_hit_skips_flush_miss_still_flushes`,
`timed_batch_read_reports_its_single_flush` (updated); consensus:
`validate_da_subphase_timers_observe` (updated: no flush for durable bodies, one
flush on a miss). Telemetry 37, state 147, mempool 97+1, consensus 168+2,
node 22, integration 102: all pass.

## Remaining

The commit feed on the parent-body insert (86-103 ms p50 per late view) is
now the largest part of the next leader's path. s72 fix D (defer the feed)
failed the dissemination gate.
