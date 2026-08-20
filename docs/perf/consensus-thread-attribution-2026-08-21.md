# Consensus-thread attribution — 2026-08-21 (s46)

Follow-up to `block-latency-campaign-2026-08-20.md` §6–7, item 1 of its backlog. Branch
`perf/matched-200k`, commits `dbd5e99..a3a476f`. All cells: 10 markets, 120 s, RATE=76000,
cap 25, `TORUS_EXEC_PIPELINE=1`, 3-val devnet on the shared 18-core box, `TORUS_ROCKSDB_STATS=2`.
Results under `~/bench-results-matched/s46-*`.

## 1. Correction: the campaign's headline cadence numbers were a window artifact

`summarize.py` averaged `wall_ms_per_committed_block`, `native_blk_s`, `empty_blk_s` and the
commit-interval percentiles over `[t_bench0, t_drain]`. That window includes 44–61 s of post-bench
drain in which the chain free-runs at 20–30 **empty** blk/s. Re-sliced to the load window
`[t_bench0, t_bench1]` on the same data (`bl-sweep-25-2`):

| | reported (incl. drain) | load window |
|---|---|---|
| wall per committed block | 151 ms | **239 ms** (`bench.log`: 239 ms avg) |
| full blocks/s | 4.6 | 4.07 |
| empty blocks/s | 2.0 | **0.12** |
| commit p50 / p95 | 75 / 508 ms | **206 / 736 ms** |

Consequences for §6/§7 of the campaign report:

- The "~2 empty blocks/s pinned regardless of cap" is drain-tail ÷ window length, cap-invariant by
  arithmetic. Under load, empties occur only in the first 5–10 s of mempool ramp; there is no
  full/empty alternation; the proposer is always 1–3 heights ahead of its executed state and still
  fills the cap (the producer never waits on exec).
- Consensus itself is healthy: 98 % view efficiency, ~10 NewViews per run, timeouts 1.9 % of
  commits and coincident with only 43 % of the >600 ms gaps. The commit-lag backoff is confirmed
  inert (`COMMIT_LAG_GRACE=4` > healthy skew).
- `summarize.py` now reports the load window at top level, `drain` and `incl_drain` as siblings
  (`incl_drain` reproduces the old numbers exactly). `consensus_by_node` surfaces the
  `torus_view_*` histograms that were always in `metrics-after-val*.txt` and never summarized;
  `sched_by_node` reads `/proc/<pid>/task/<tid>/schedstat` per named thread.

## 2. Where a full block's time goes (cap 25, per view, all three nodes agree within a few ms)

Baseline = `s46-attr-25-unpinned` r1/r2 (binary `a25ee4a1`, same code as `db02d93` plus timers).

| stage | ms | who |
|---|---|---|
| view duration | 132.6 / 132.9 | all |
| `propose_delay` (view start → header out) | 46–66 | leader |
| ↳ `block_build` (produce_block proper) | 18–20 | leader |
| ↳ remainder: block-tree writes + thread busy | 20–25 | leader |
| `qc_collect` (own proposal → own QC) | 61–73 | leader |
| `proposal_arrival` | 52–62 | follower |
| `vote_delay` (header → vote) | 8–9 | follower |
| `insert_persist` (header → body validated + inserted) | 46–57 | follower |
| ↳ `validate_block` | 16–19 | follower |
| ↳↳ `da_reconstruct` (per compact block, all hits) | 22–31 | follower |
| ↳↳ `attest` / `decode` / `custody` | 1 / 0 / 0 | follower |
| `on_committed_block` | 13.5 | all |
| ↳ mempool `remove_committed` | 9.3 | all |
| `commit_persist` | 3–4 | all |

Followers vote 9 ms after the header; the QC still takes 60–73 ms to come back because the **next
leader's single HotStuff thread is busy with `insert_persist`** when the votes land. Across caps,
`block_build ≈ 13 + 0.21·actions` and `insert_persist ≈ 48 + 0.22·actions` ms — a ~68 ms
size-independent constant on one thread. That constant, not exec (53 % busy) and not the pacemaker,
is why thinner blocks stopped helping.

## 3. Rig vs code (schedstat on `hotstuff-algo`, per committed block, val0)

| cell | on-CPU | runqueue wait | wall/committed |
|---|---|---|---|
| unpinned r1 | 36.9 | 19.4 | 210 |
| unpinned r2 | 36.8 | 22.1 | 213 |
| pinned to one core (`HOTSTUFF_CPUS=15/16/17`) | 38.9 | **31.9–40.6** | **227–234** |

~20 ms/block of the constant is the thread being runnable without a core (load1 24–26 on 18 cores;
val0's submit-path signature verification alone burns ~2.9 cores). **Pinning is a negative on this
box**: a thread locked to one core cannot migrate to a free one; without exclusive cpusets/isolcpus
affinity makes it worse. RocksDB `db.write` avg 7.2 ms / p99 27 ms with zero stalls: the consensus
thread's 6–9 tiny per-view writes queue behind exec's flush batches in the shared write group.

## 4. Two levers measured

| | control (pipelined-write r1/r2) | `PIPELINED_WRITE=1` vs unpinned | DA batch read (`a3a476f`) r1 / r2 |
|---|---|---|---|
| view ms | 132.6–132.9 → 119.2 / 121.5 | −9 % (n=2, scatter <1 %) | 117.5 / 117.6 (−2.5 %) |
| `da_reconstruct` | 22–24 | — | **13.9–16.0 / 14.7–14.8** (−37 %) |
| `validate_block` | 14.9–15.9 | — | **9.2–10.6 / 9.7–9.9** |
| HotStuff on-CPU /blk | 37–39 | — | **33–35 / 35–36** |
| db.write avg | 7.2 → 5.8 | | 5.8 |
| matched/s | 23.5–24.8k → 26.5–26.6k | +7–13 % | 27.7k / 27.0k |
| agreement | AGREE | AGREE | AGREE / AGREE |

- `TORUS_ROCKSDB_PIPELINED_WRITE=1` (`6f61f05`): now in the bench `RECORD_ENV`; node default
  unchanged pending a cap-100 / 300 s confirm.
- DA batch read (`a3a476f`): one `flush_da_mirrors` + one RocksDB `MultiGet` per compact block
  instead of 25 + 25. Recovers ~8 ms of on-CPU per block; only a third of it shows in view time
  because the follower's validate is not always on the view's critical path.

## 5. What is left on the consensus thread, ranked

1. Block-tree writes on the leader (`propose_build − block_build` ≈ 20–25 ms, size-independent):
   batch the 6–9 per-view writes, or give the non-critical ones `set_low_pri`.
2. mempool `remove_committed` 9.3 ms: two O(pool) scans under the write lock per commit → hash
   index.
3. Remaining `insert_persist` base (~38–42 ms after the DA fix): tree insert + update/commit +
   `on_committed_block`. Structural option: validate bodies off the HotStuff thread on arrival.
4. Runqueue wait ~20 ms: only dedicated/quieter hardware removes it.

## 6. Harness notes

- `HOTSTUFF_CPUS` pinning must run after the health wait — the thread is spawned after DB open and
  the first attempt silently pinned nothing (`f3eaf2f`).
- No view-timeout / pacemaker / leader log lines exist in production (`log_events(false)`,
  `wedge_diag` gated); attribution is metrics-only.
- `torus_mempool_native_size` was registered and never set; it is now set after each commit prune.
- `validate_block` had no timer at all; the new `torus_validate_block_*` family closes that gap.
