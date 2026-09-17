# Blown-cell diagnosis — 2026-09-17 (s58)

## Scope and method

Analysis of retained cells in `~/bench-results-matched` (258 cell
directories) plus two cells run at branch head in s58 (section 6). The population is every cell with a
`summary.json` and a `sampler.csv`: 136 at cap 200, 15 at cap 100.

The prior plan was to read the single failed baseline cell
(`base-off-cap100-v2-r1`) and explain its QUIC `Send Queue full` warnings. That
was replaced with a paired-population approach: **10 blown/clean pairs at cap 200
where both reps share a commit, a binary and a day.** A blown cell is one whose
matched/s is under two-thirds of its own twin's. Pairing removes code, build and
day-to-day drift as explanations by construction.

All per-block figures below are val0 deltas between the first and last
`sampler.csv` rows, divided by `torus_exec_block_seconds_count`.

## 1. Storage and gossip are excluded

Across every cap-200 cell measured — blown and clean alike:

| counter | value, all cells |
| --- | --- |
| `torus_rocksdb_stall_micros` (delta) | 0 |
| `torus_rocksdb_write_stall_count` (delta) | 0 |
| `torus_rocksdb_write_stopped` | 0 |
| `torus_rocksdb_delayed_write_rate` (max) | 0 |
| `torus_rocksdb_l0_files_max` (max) | 4–8 |
| `torus_rocksdb_pending_compaction_bytes_all` (max) | 0.00–0.35 GB |
| `torus_native_gossip_dropped_full_total` (delta) | 0 |

No RocksDB write stall, no compaction backlog, no gossip queue drop occurs in any
cell. Storage-side and gossip-drop hypotheses are dead. This also rules out the
memtable/L0 pressure that the earlier `769d7b3` memtable-cap work targeted as a
possible cause of cell variance.

## 2. The catastrophic tail is an amplification chain with a threshold

Ten pairs, consistent without exception:

| metric | clean twin | blown | direction |
| --- | --- | --- | --- |
| exec ms per block | 171–352 | 361–698 | ~2x |
| **ms per view** | 215–432 | 583–1094 | **~3x** |
| consensus timeout rate | 6–16 % | 19–29 % | up |
| views per block | 1.16–1.33 | 1.40–1.66 | up |
| blocks/s | 1.10–4.02 | 0.06–1.34 | down |
| peak exec queue depth | 14–66 | 3–10 | **down** |

Two observations carry the diagnosis.

**View duration inflates faster than execution does.** For
`r5-workers-combo`, exec goes 184 -> 504 ms (2.7x) while ms/view goes 236 -> 877
(3.7x). For `bl2-flush-1deep-...`, 171 -> 379 (2.2x) against 235 -> 839 (3.6x).
This is the s55 invariant (`ms/blk ~= 3.2x` HotStuff-thread cost) acting as an
amplifier: leader and follower work serialize across nodes, so a given increase
in per-block cost buys a larger increase in view duration.

**The exec queue is shallower in blown cells, not deeper.** Peak depth falls from
14–66 to 3–10. A node that were overloaded would show a queue backing up. These
nodes are starved.

The resulting chain:

```
exec ms/block ~2x
  -> ms/view ~3x (serialized leader+follower work)
    -> mean view approaches the 500 ms timeout budget
      -> timeout rate 6-16% jumps to 19-29%
        -> dead views; views/block 1.16 -> 1.6
          -> block rate halves to quarters
            -> bench backlog ages past NONCE_WINDOW_MS=60s
              -> tens of thousands of actions silently evicted
                -> matched/s collapses
```

The input (per-block cost) varies continuously; the 500 ms timeout is a cliff.
A continuous input crossing a threshold produces a **bimodal output**. That is
why the cell population looked like it contained a discrete fault. There is no
discrete fault.

The nonce eviction is the last link, not the first. It is a *consequence* of slow
block production, and quoting it as a cause inverts the chain.

## 3. A validity screen falls out

`views_per_block <= 1.35 AND timeout_rate <= 18 %`, computed from val0 sampler
deltas, with no knowledge of the throughput result:

| population | n | median matched/s | spread (max/min) |
| --- | --- | --- | --- |
| cap 200, screen PASS | 99 | 45,258 | 3.22x |
| cap 200, screen FAIL | 37 | 26,406 | **110.16x** |
| cap 100, screen PASS | 14 | 32,562 | 3.24x |
| cap 100, screen FAIL | 1 | 16,290 | — |

The screen is **necessary, not sufficient**. It removes the catastrophic tail;
it does not produce a tight population. Both thresholds are empirical fits to
this box and this harness, and should be re-derived if either changes.

## 4. Reversal: the cap-100 instability is starvation, not overload

Section 3's screen does not explain the instability that actually blocks the
async-validation work. `s55-gap-100-r1` and `-r2` share binary `f4f57db1`, ran the
same day, and **both pass the screen** — yet differ 2.54x.

| metric | slow (14,493/s) | fast (36,801/s) |
| --- | --- | --- |
| exec ms per block | **88.4** | 129.5 |
| actions per block | **19.9** | 24.8 |
| blocks/s | 3.50 | 4.62 |
| views per block | 1.16 | 1.12 |
| bench submitted actions | 42,623 | 40,906 |
| chain absorbed | **31.5 %** | 50.5 % |
| nonce evictions | 29,495 | 20,468 |
| block interval (bench log) | 745 ms | 489 ms |

The slow cell does **less** work per block, in **emptier** blocks, with **no**
excess dead views, under the **same** offered load. It is starved, not overloaded.

Execution inside the block fell while the block interval rose. Therefore the
non-execution portion of the view went **359 ms -> 657 ms**. That is where the
throughput went.

**The limiter in the unstable cap-100 regime is the non-execution part of the
view — HotStuff-thread work plus dissemination round-trip — not execution, not
storage, not dead views.**

This retroactively explains why cap-100 exec-side attribution kept returning
null: the campaign was optimizing a component that was not binding. It is also
consistent with the s55 timeout A/B being null — raising `timeout_base_ms`
removes dead views but does not touch the non-exec cost that sets view duration.

## 5. The non-exec window is HotStuff-thread *blocked* time

`schedstat.json` carries `[on_cpu_ns, runqueue_wait_ns, timeslices]` per named
thread at `before` / `bench_end` / `after`. For the bench window, the val0
`hotstuff-algo` thread gives a three-way split of the block interval: on-CPU,
waiting for a CPU, and everything else — blocked on I/O, a socket, a channel or
a lock.

**Coverage limit, now closed (see section 6).** No *historical* cap-200 cell has
schedstat: snapshots landed in the runner in `d951236` on 2026-08-21, and every
one of the 136 cap-200 cells ran 2026-08-18 to 08-20. Of the historical cells
that carry `schedstat.json`, 36 are cap 25 and 8 are cap 100. Two cells run in
s58 at branch head close the gap.

Seven pairs have schedstat, spanning cap 25 and cap 100:

| cell | cap | matched/s | wall/blk | onCPU | rq wait | **blocked** | blocked % |
| --- | --- | --- | --- | --- | --- | --- | --- |
| s48-items126-25-r2 | 25 | 20,990 | 244.7 | 31.1 | 28.8 | 184.8 | 76 % |
| s48-items126-25-r1 | 25 | 1,372 | 1347.8 | 33.1 | 22.4 | 1292.3 | 96 % |
| s50-control-…-udpbuf8m-r0 | 25 | 23,858 | 236.6 | 41.8 | 21.3 | 173.5 | 73 % |
| s50-control-…-udpbuf8m-r2 | 25 | 4,160 | 1007.9 | 32.1 | 17.1 | 958.7 | 95 % |
| s50-items21-25-sep-r2 | 25 | 21,440 | 254.9 | 45.6 | 18.3 | 190.9 | 75 % |
| s50-items21-25-sep-r1 | 25 | 3,160 | 573.4 | 37.7 | 14.2 | 521.5 | 91 % |
| s55-gap-100-r2 | 100 | 36,801 | 452.3 | 79.8 | 40.4 | 332.1 | 73 % |
| s55-gap-100-r1 | 100 | 14,493 | 636.4 | 96.6 | 45.1 | 494.6 | 78 % |

Two results, without exception across all seven pairs:

**Blocked time is 71–96 % of the block interval in every cell**, fast or slow.
On-CPU time is comparatively stable (31–50 ms at cap 25, 76–137 ms at cap 100)
and runqueue wait is small (14–116 ms).

**Within every pair, the slow cell's extra wall time is almost entirely extra
blocked time.** For `s55-gap-100`, +184.1 ms wall against +162.5 ms blocked —
88 %, with on-CPU contributing only +16.8 and runqueue wait +4.7. For
`s50-items21`, +318.5 ms wall against +330.6 ms blocked. For `s48-items126`,
+1103 ms against +1108 ms.

A high blocked fraction is not by itself pathological: a BFT consensus thread is
*expected* to spend most of a view waiting for a quorum of votes. The finding is
about the **variance**. Cell-to-cell throughput variance is variance in blocked
time, and almost nothing else.

This refines s55, which attributed cell variance to box contention visible "in
on-CPU time not just rq-wait". On this data on-CPU barely moves between a cell
and its 2.5x-slower twin; the movement is in blocked time.

The consequence for the optimization backlog is direct. Execution, engine,
`save_books` and root hashing are on-CPU work; on-CPU work is a minority of the
block interval and is not the source of the variance. Attributing that variance
requires knowing *what the thread blocks on* — socket read, exec-pipeline
handoff, or a mempool lock — which schedstat cannot distinguish.

## 6. Cap 200 confirmed, and the off-CPU route is a dead end

Two cells at branch head `4076355` (cap 200, 10 markets, 120 s, rate 76000,
`TORUS_ASYNC_VALIDATE` off), run 2026-09-17/18 on an idle box with loadavg
gated below 1.5. Node binary verified byte-identical to the frozen s57
candidate (`7b8375e1...`) and distinct from its baseline (`121a77c0...`).

| | profiled (`s58-cap200-offcpu-r2`) | control (`s58-cap200-ctl-r1`) |
| --- | --- | --- |
| matched/s | 2,624 | **36,726** |
| agreement | DIGEST_UNVERIFIED | AGREE |
| wall ms/block | 2082.0 | 962.1 |
| actions absorbed | 2,043 / 54,258 | 22,362 / 46,300 |
| hotstuff on-CPU ms/blk | 138.6 | 190.6 |
| hotstuff rq-wait ms/blk | 55.0 | 70.9 |
| **hotstuff blocked ms/blk** | 1888.4 (**91 %**) | 700.6 (**73 %**) |

**Section 5 generalises to cap 200.** The clean cell sits at 73 % blocked,
inside the 73-78 % band the clean cap-25 and cap-100 cells occupy; the degraded
one sits at 91 %, alongside the 91-96 % blown cells. The result is not an
artefact of the smaller caps.

**`offcputime-bpfcc` cannot be used on this workload.** The only difference
between the two cells is the profiler, and it cost **14x throughput**. The probe
fires on every context switch and this workload switches constantly, so the
instrument destroys what it measures. Two further notes for whoever tries again:

- `-t <TID>` silently matches nothing in this bcc build (bcc 0.29, kernel
  6.8.0-106). It returned zero stacks for a plain `sleep` target as well.
  Whole-system and `-p <PID>` modes work and do resolve Rust frames — the
  binary is not stripped (110k symbols). Filter by the thread name, which
  folded output carries as the first frame.
- The section-3 screen does **not** catch this failure. Both cells pass it
  (VPB 1.19 and 1.30, timeout 13.4 % and 12.1 %) despite a 14x throughput gap.
  The screen detects the timeout-cliff mode of section 2 and nothing else. A
  cell can be clean by every consensus-health measure and still be worthless.

Naming the blocking call therefore needs a method whose overhead is bounded:
in-process instrumentation around the suspected waits, or a sampling profiler,
not exhaustive context-switch tracing. That work is not done.

## 7. What is not explained

- Residual spread among screen-passing 10-market cap-200 cells is **2.61x**
  (n=87, median 45,634, min 21,362, max 55,718).
- `corr(matched/s, exec ms per action)` is **-0.44** once market count is
  controlled. An earlier uncontrolled figure of -0.83 was a 300-market vs
  10-market confound and should not be quoted.
- **What the HotStuff thread blocks on.** Section 5 localises the variance to
  blocked time but cannot name the blocking call. schedstat gives no
  distinction between a socket read, an exec-pipeline handoff and a lock. This
  needs either an off-CPU profile (`offcputime`/eBPF) on a live cell, or
  in-process instrumentation around the suspected waits.
- Nothing here establishes a cause for the QUIC `Send Queue full` lines in
  `base-off-cap100-v2-r1`. That cell is consistent with the starvation picture
  but has not been screened against this population.

## 8. Consequences for the open work

1. **Do not pursue exec-side levers for throughput variance.** Sections 4 and 5
   show execution is not the binding constraint and on-CPU work is not where the
   variance lives. This affects backlog items 2 and 4 of the s55 next-steps
   list, and it applies at cap 25 and cap 100 alike.
2. **Apply the section-3 screen before any A/B verdict**, and report it with the
   cell. It would have rejected the worst historical cells outright.
3. **The screen does not rescue cap-100 pairing.** Both s55 cells passed it and
   still differ 2.54x. Same-day pairing at n>=3 remains mandatory, and cap 200
   remains the better-behaved operating point for A/Bs.
4. **Re-read historical verdicts.** Any conclusion drawn from a single cell in
   the screen-FAIL group is unsafe; that group's spread is 110x.
5. **The next cut is an off-CPU profile, not another A/B.** Until the blocking
   call is named, any throughput A/B on this rig is measuring a 71-96 % term
   nobody can attribute. Two prior sessions touched adjacent ground and should
   be read first: s353 (dedicated 4-thread ingress pool, "starvation FIXED") and
   S391 (`fix/hotstuff-idle-cpu-spin`).
