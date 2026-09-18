# Cap-200 load-window repeats — 2026-09-18

## Result

Follow-up: [acceptance fix and height-584 diagnosis](stall-584-diagnosis-2026-09-18.md).
The final view was 659; views continued advancing during the height plateau.

Completed the three repeats requested by the s58 handoff. The unchanged node
produced **25,959.6**, **22,787.3**, and **48,622.9 matched orders/s**. The second
repeat stopped advancing with pending work and is **not a healthy baseline**,
despite the harness reporting `AGREE` and `drained=true`.

Together with the original run, the three advancing runs support proposal
construction as a substantial cost. They do **not** establish a stable throughput
baseline, a causal explanation of all variance, or an optimization win. No chain
code, runtime flag, or compiled default was changed during this experiment.

## Reproduction and provenance

- Worktree: `/home/18c/projects/wt/matched-200k-next`, branch
  `perf/matched-200k-next`, starting commit `23539a5`.
- Runtime sources and Cargo inputs are unchanged from `4076355`; intervening
  commits only change documentation and benchmark tooling.
- Frozen node SHA256:
  `7b8375e1de4f3cec4c814cfec9b554f39b3e3eed591fd0f0511513f7f24bcfa8`.
- Frozen load-generator SHA256:
  `ce06befbab9e87f9e98d0f45a7c18e506d7764f1709be532550f5b342c064528`.
- Node MD5 `2c13de0b77cd0e2cfa318023469843d2`, load-generator MD5
  `62d2d3ed97912d8d851b1a01012d7ec0`, and generated genesis MD5
  `478d698b5b331837fd7d0c378ece9f46` match the original r1 in all repeats.
- Three validators on this machine; 10 markets; cap 200; 5,000 senders;
  batch 400; nominal duration 120 s; rate 76,000; unchanged record environment.
  Async validation and the execution flush pipeline remain disabled.
- Each repeat started with 1-minute host load below 1.5, ran sequentially, and
  used a separate fresh data directory. No builds, profiler, or CPU pinning.
  The higher 5-/15-minute load averages were not start gates.
- `test_summarize.py` passed; `test_harness.py` passed all 52 tests before launch.

Experiment directory:
`/home/18c/bench-results-matched/s58-loadwin-repeats-20260918/`.
It retains `manifest.json`, `run_repeats.py`, frozen `artifacts/release/` binaries,
per-run logs and databases, `analyze.py`, and `comparison.json`.
Each cell's normal result directory is
`/home/18c/bench-results-matched/s58-cap200-loadwin-rN/`.
The original r1 is retained separately and was not rerun or overwritten.

`analyze.py` independently recomputes histogram means from `sampler.csv`, checks
them against every node's stored load-window means, verifies binary/genesis/cell
identity, and calculates a second window starting at bench+10 seconds.

## Throughput and liveness

Rates below are val0 node-counter averages over the actual load window, excluding
drain. Nominal 120-second runs have actual sampled spans of 124–127 seconds.

| Cell | Matched/s | Actual load span | Agreement | Observation |
| --- | ---: | ---: | --- | --- |
| r1, original | 41,871.8 | 127 s | AGREE | Advanced through drain; final mempools empty |
| r2, new | 25,959.6 | 126 s | AGREE | Advanced through drain; final mempools empty |
| r3, new | 22,787.3 | 124 s | AGREE | **Commit height stalled at 584 with pending actions; final view 659** |
| r4, new | 48,622.9 | 126 s | AGREE | Advanced through drain; final mempools 1/0/0 |

All four summaries report `dissemination_clean=false`: their counted body-fetch
exhaustion/sync-fallback totals are respectively 23, 30, 48, and 23. Agreement
therefore does not make these clean dissemination acceptance cells. Retain every
result; do not average away the stalled run or describe the survivors as passing
all benchmark gates. The advancing runs alone still span 1.87x throughput.

## Proposal construction repeats, but the first 10 seconds still distort means

Val0 means, milliseconds per observation of the named timer. These are nested or
overlapping stages with different counts; do not add them into a view budget.

| Cell | Window | Block build | Proposal build | Follower arrival | View duration |
| --- | --- | ---: | ---: | ---: | ---: |
| r1 | Full load | 231.3 | 320.9 | 443.8 | 745.3 |
| r2 | Full load | 163.0 | 220.9 | 286.7 | 545.7 |
| r3, stalled | Full load | 160.5 | 189.0 | 273.4 | 937.8 |
| r4 | Full load | 153.4 | 215.7 | 324.4 | 517.2 |
| r1 | Bench+10 to end | 260.2 | 367.9 | 511.5 | 848.3 |
| r2 | Bench+10 to end | 345.2 | 467.5 | 606.1 | 1,103.1 |
| r3, stalled | Bench+10 to end | 293.2 | 334.4 | 462.1 | 1,773.3 |
| r4 | Bench+10 to end | 210.1 | 294.8 | 456.2 | 713.2 |

The window correction matters: r2 looks cheaper than r1 in the full-load table,
but becomes more expensive after removing the early empty-block ramp. Even the
full *load* window is not a reliable steady-work stage comparison by itself.

Across all three nodes in the advancing runs (r1/r2/r4), steady block build spans
200.3–384.5 ms, proposal build 274.5–553.4 ms, and follower arrival 417.4–606.8 ms.
The faster r4 has lower build/arrival times than r2 on every node. This supports
continued investigation of the build path. It does not measure the individual
cost of action hashing or DA mirroring, prove that either change will improve
throughput, or explain the stalled r3.

## A stalled chain passes the current drain/agreement checks

In r3, the final samples show all validators at committed height 584, consensus
view 659, and matched counter 2,825,622. The final height plateau lasts 84/87/87 s
through the last recorded sample (83/86/86 s within the explicit drain boundary).
Execution queues are zero, but native mempools contain 15,550/15,428/15,810 actions.

The runner declares `drained=1 after 19s` because order/action counters are quiet.
It then observes equal block hashes and equal state digests at height 584 and
reports `AGREE`. This confirms agreement at a stopped state; it does not establish
liveness or completion of pending work.

The logs contain 2,756/1,849/24 `Send Queue full` warnings and zero matches for the
checked panic/fail-stop patterns. These warnings accompany the failure; their
causal relationship and ordering relative to the stall have not been established.
The observation does not prove a permanent wedge or recovery behavior after restart.

Raw logs total about 1.6 GB. The existing collector materializes entire logs in
Bash variables and repeatedly scans them; post-stop dissemination extraction ran
from 03:10:29 to 03:22:27 local time. This delay occurs after measurement and must
not be attributed to node execution latency.

Saved Torus issue: `395ac6a4-fa2a-4baf-b857-694b1dd075a5`.

## Health-screen implications and next action

The old `views/committed <= 1.35` and timeout fraction `<= 18%` thresholds cannot
be transferred from whole-run data to load or steady windows. For val0:

| Cell | Full-load views/commit | Full-load timeout/view | Steady views/commit | Steady timeout/view |
| --- | ---: | ---: | ---: | ---: |
| r1 | 1.667 | 22.2% | 1.767 | 23.7% |
| r2 | 1.283 | 11.7% | 1.794 | 23.0% |
| r3, stalled | 1.402 | 23.1% | 1.786 | 38.7% |
| r4 | 1.506 | 14.3% | 1.842 | 19.4% |

Applying the old thresholds to the full-load values accepts the slower advancing
r2 and rejects both r1 and r4. Merely raising the views/commit threshold does not
separate the stalled run from the advancing runs in the steady window. Four cells
are insufficient to fit and independently validate replacement cutoffs.

Next, fix the benchmark acceptance check to detect sustained lack of commit
progress with pending work, preserving `AGREE` as a separate state-consistency
verdict. Investigate the retained r3 stall before treating a throughput A/B as an
acceptance test. Stream log processing to avoid the observed collector delay.

Cached proposal action hashes and avoiding duplicate DA mirroring remain concrete
build-path candidates. The repeated timing supports measuring them, but this
experiment has **not** met the clean, stable-baseline gate for claiming a win.
No new numerical health threshold or chain optimization is adopted here.
