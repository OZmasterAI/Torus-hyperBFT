# Blown-cell diagnosis — 2026-09-17 (s58)

## Scope and method

Log-only analysis of retained cells in `~/bench-results-matched` (258 cell
directories). No benchmark cells were run. The population is every cell with a
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

## 5. What is not explained

- Residual spread among screen-passing 10-market cap-200 cells is **2.61x**
  (n=87, median 45,634, min 21,362, max 55,718).
- `corr(matched/s, exec ms per action)` is **-0.44** once market count is
  controlled. An earlier uncontrolled figure of -0.83 was a 300-market vs
  10-market confound and should not be quoted.
- The non-exec window has not been split into HotStuff-thread on-CPU, runqueue
  wait, and network wait. That split decides between box contention and
  dissemination and is the next cut; `schedstat.json` in these cells is the
  intended source.
- Nothing here establishes a cause for the QUIC `Send Queue full` lines in
  `base-off-cap100-v2-r1`. That cell is consistent with the starvation picture
  but has not been screened against this population.

## 6. Consequences for the open work

1. **Do not pursue exec-side levers for cap-100 throughput.** Section 4 shows
   execution is not the binding constraint there. This affects backlog items 2
   and 4 of the s55 next-steps list.
2. **Apply the section-3 screen before any A/B verdict**, and report it with the
   cell. It would have rejected the worst historical cells outright.
3. **The screen does not rescue cap-100 pairing.** Both s55 cells passed it and
   still differ 2.54x. Same-day pairing at n>=3 remains mandatory, and cap 200
   remains the better-behaved operating point for A/Bs.
4. **Re-read historical verdicts.** Any conclusion drawn from a single cell in
   the screen-FAIL group is unsafe; that group's spread is 110x.
