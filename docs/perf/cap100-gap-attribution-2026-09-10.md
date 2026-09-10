# The cap-100 gap: why full-block interval is 1.5–2× the exec chain — 2026-09-10 (s55)

Follow-up to `consensus-thread-attribution-2026-08-21.md` and `block-latency-campaign-2026-08-20.md`
§6–7 (backlog item 1: "consensus-timeout-attribution-under-pipeline"). Branch `perf/matched-200k`
@ `1e41bea` (binary md5 `f4f57db1`), bench worktree `/home/18c/projects/wt/matched-bench`.
All cells: 10 markets, 120 s, RATE=76000, batch 400 orders/action, 3-val devnet on the shared
18-core box. Results under `~/bench-results-matched/s55-gap-*`. Analysis script:
session scratchpad `gap_attr.py` (steady-window slicing of `sampler.csv` + the s46 view timers).

## 1. The question

The block-latency sweep (`db02d93`) showed the full-block interval running 1.6–1.9× the exec chain
at every cap while exec was only 53–64 % busy: cap 25 = 217 ms interval / 115 ms chain, cap 100 =
513 ms / 319 ms. The s46 view timers explained cap 25 but had never been run at cap 100. This
session ran them there.

## 2. Two window corrections first

- **The ramp contaminates the load window.** The first ~3 s after `t_bench0` commit 90–134 idle
  blocks (empty, 20–60 ms each) before the pool fills. Every "per committed block" number over
  `[t_bench0, t_bench1]` — including `sched_by_node`'s per-block on-CPU — is diluted by them. All
  numbers below use a **steady window** `[t_bench0 + 10 s, t_bench1]` from `sampler.csv`
  (`torus_consensus_view`, `torus_blocks_committed_total`, `torus_consensus_timeout_total_total`,
  `torus_orders_matched_total`), and schedstat totals are divided by steady-window commits.
- **The per-stage view timers (`consensus_by_node`) are whole-run averages** (idle + load + drain),
  so they understate loaded cost by 2–10× at cap 100 where ~900 idle views dilute ~200 loaded
  ones. They are used qualitatively only.

## 3. Steady-window results (val0; val1/val2 agree within 3 %)

| cell | cap | ms/view | views per block | timeouts (% of views) | ms per full block | matched/s | HotStuff thread per block: on-CPU + runqueue wait |
|---|---|---|---|---|---|---|---|
| `s50-item2-25-sep-r1` | 25 | 223 | 1.01 | 1.2 % | 225 | 26.9k | 39 + 23 ms |
| `s50-item2-25-sep-r2` | 25 | 220 | 1.01 | 1.0 % | 222 | 27.5k | 40 + 22 ms |
| `s55-gap-25-r1` | 25 | 262 | 1.02 | 2.2 % | 266 | 23.8k | 47 + 27 ms |
| `bl-sweep-100-8` (db02d93) | 100 | 458 | 1.23 | 18.6 % | 566 | 46.5k | (no schedstat on that harness) |
| `bl-sweep-100-8-r2` (db02d93) | 100 | 456 | 1.22 | 19.8 % | 558 | 47.8k | — |
| `s55-gap-100-r2` | 100 | 527 | 1.45 | 20.1 % | 766 | 36.6k | 147 + 74 ms |
| `s55-gap-100-oldenv-r1` (backoff 8, no rocksdb stats, pipelined write off) | 100 | 656 | 1.64 | 22.1 % | 1076 | 26.7k | 200 + 88 ms |
| `s55-gap-100-r1` | 100 | 967 | 1.76 | 35.8 % | 1706 | 13.5k | 281 + 131 ms |
| `s55-gap-100-db02d93-newenv-r1` | 100 | PENDING | | | | | |

Every cell AGREE, 0 panics, drained, `rebuilds [1,1,1]`. Box load1 avg 22–25 in all cells (the
cell's own three validators + bench). The pool never starved in any cap-100 cell: it grew
monotonically to 10–19k actions and 50–69 % of submitted actions expired in the 60 s nonce window
unincluded. Every committed block under load was full (87–89 actions).

## 4. Decomposition: interval = views/block × ms/view, and both terms grow with cap

**(a) ms/view tracks the HotStuff thread's per-block cost by a constant ~2.4–3.5×.** The thread is
only ~18 % CPU-busy over a cell; inside a view it is mostly waiting for the *other* nodes' thread
work, because leader and follower per-block work serialize across the chain (s46 §2: the QC waits
on the next leader's `insert_persist`). Thread cost per full block is ~linear in actions on the
representative cell (62 ms at 25 actions → 221 ms at 89, r2), i.e. ~1.7 ms per action ≈ 4 µs per
order on the consensus thread, one-third of it runqueue wait. The whole-run stage timers all grow
together at cap 100 (block_build, propose_finalize, DA reconstruct, attest, commit_persist,
on_committed) — the per-order work on that thread is: selection + encode at produce, action-hash
recompute over ≤10 pending proposals, DA multi_get + bincode deserialize of every body at validate,
attest re-encode + sha256, JSON body encode + write at commit, mempool prune.

**(b) Views per block is the timeout cliff.** `timeout_base_ms = 500` (genesis, `main.rs:314`) sets
absolute per-view deadlines 500 ms apart from epoch start (`pacemaker/implementation.rs:868`), rebased
to `now + 500 ms` whenever a replica gets more than 2 slots ahead (`:612`, the S395 liveness fix). So
each view's budget oscillates between 500 ms and 1 s. At cap 25 the mean view is 220 ms and 1–2 % of
views time out. At cap 100 the mean view (457–967 ms) sits **on** the budget: 57 % of loaded views
exceed 512 ms (histogram), 19–36 % time out. A timed-out view is a dead view (its proposal is
abandoned, its actions stay in the in-flight exclusion set until that height commits, `app.rs:2195`),
so views/block goes 1.01 → 1.22–1.76: **+22 % to +76 % of the interval is dead views.**

**(c) The regime is unstable.** Three same-binary same-env reps at cap 100 span 527–967 ms/view and
13.5k–36.6k matched/s (the old head's two reps were 456/458). Longer views → more timeouts →
abandoned proposals → longer effective views. n=1 at cap 100 is meaningless; n=2 is barely readable.

**(d) Not the env, not exec.** Cell A (old env: backoff cap 8, no RocksDB stats, pipelined write
off) lands inside the new-env spread. Exec busy is 0.33–0.64 at cap 100; the flush worker and exec
thread are not on the critical path.

## 5. What this says about the "cap-100 matched/s at cap-25 blockspeed" question

The interval at cap 100 is not exec-coupled; it is (HotStuff-thread per-block cost) × ~2.5 ×
(1 + dead-view fraction). Two levers, in order:

1. **The timeout cliff is the cheap one.** Raising `timeout_base_ms` (genesis) or making the base
   scale with the recent view mean removes the 22–76 % dead-view term at cap 100 for zero
   per-block cost. It costs recovery latency on real leader failure; the Task A stall multiplier
   already handles the lockstep-stall case. A genesis-only A/B (`timeout_base_ms` 500 vs 1500 at
   cap 100, n≥3) is the next cell.
2. **Per-order work on the HotStuff thread is the structural one.** Items 1 (write-group) and 3
   (off-thread validate) from the next-steps list, plus the three unlisted items (skip DA re-mirror
   on produce, cache action hashes in `pending_proposals`, drop the duplicate body write), all
   attack the ~4 µs/order. Off-thread validate alone removes the DA reconstruct + attest share.
   Measure each at cap 100 with n≥3, never at cap 25 where the per-order term is small.

Cap 25 with batch 1000 (same orders/block as cap ~62) is still worth one cell: it isolates whether
the thread cost is per action or per order, which decides whether bigger batches are free.

## 6. Harness notes

- `gap_attr.py`: steady-window slice, histogram tail (`torus_view_duration_seconds` buckets >512 ms,
  >1.024 s, >2.048 s), idle-stripped stage estimates. Worth folding into `summarize.py`
  (`steady_window` sibling of `load`, plus per-block schedstat over the steady window).
- `db02d93`'s harness predates the view timers and schedstat, so cell B carries only the sampler
  counters.
- Driver scripts must be launched with argv free of `run-cell.sh` / `cargo build` / their own
  name (pgrep guard trap; cost two dead commands this session).
