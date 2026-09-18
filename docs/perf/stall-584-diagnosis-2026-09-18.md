# Height-584 stall: acceptance fix and retained-log diagnosis

The benchmark harness could certify a quiet, equal-state, stopped chain as
`AGREE` and `drained=true`. The harness now separates state agreement from
liveness and overall benchmark acceptance. The retained r3 fails liveness;
its throughput and agreement results remain unchanged. No Rust source, node
binary, runtime setting, or consensus algorithm changed in this work.

## Evidence replay

Original cells are `/home/18c/bench-results-matched/s58-cap200-loadwin-r{1..4}`.
Copies re-scored with this harness are under
`/home/18c/bench-results-matched/s59-stall-diagnosis-20260918/rescored/`.
The original summaries and raw evidence remain intact.

| Cell | Matched orders/s | Agreement | Liveness | Overall acceptance |
| --- | ---: | --- | --- | --- |
| r1 | 41,871.8 | AGREE | PASS | REJECT: dissemination |
| r2 | 25,959.6 | AGREE | PASS | REJECT: dissemination |
| r3 | 22,787.3 | AGREE | FAIL | REJECT: stall, drain, dissemination |
| r4 | 48,622.9 | AGREE | PASS | REJECT: dissemination |

For r3, the new detector finds unchanged-commit intervals with pending work of
81/84/84 seconds on val0/val1/val2, within bench start through recorded drain.
The earlier report's 84/87/87 seconds includes the final sampler tail after
recorded drain. Neither calculation establishes permanent loss of liveness.
The final native mempools contain 15,550/15,428/15,810 actions; execution queues
are zero. `drained_reported=true` retains the old claim; `drained=false` rejects
it as evidence for a healthy performance cell.

The 30-second rejection threshold is an operational rule. Four retained cells
are insufficient to calibrate a throughput-variance filter. Historical samples
lack the new `scrape_valid` marker, so replay cannot undo any missing values
that the old collector silently replaced with zero.

## Observed sequence (UTC, 2026-09-18)

Raw logs: `/home/18c/bench-results-matched/s58-loadwin-repeats-20260918/`
`s58-cap200-loadwin-r3/run/val{0,1,2}.log`.

| Time | Evidence |
| --- | --- |
| 01:08:30.592–01:08:32.244 | Final execution of height 584: val1, val2, then val0. |
| 01:08:31.920 | val1 exhausts body-fetch retries and falls back to sync. |
| 01:08:33.262 / 01:08:34.621 | val2 / val0 also exhaust body-fetch retries. |
| 01:08:37.635 | val0 reports a sync-worker fetch error. |
| 01:08:38.332 | First val0 `Send Queue full` warning. |
| 01:08:40.994 / 01:08:49.709 | First corresponding warnings on val1 / val2. |
| 01:09:34 | Sampled views reach 659 while committed height remains 584. |
| 01:10:08.042 | val2 sync batch at height 584 reports no progress and ends its session. |

During the plateau, producers build height 587 on parent height 586 while local
executed height is 584, with direct full-body pushes roughly 2.3–2.7 MB. These
messages do not prove that the required ancestor bodies and certificates reach
all consumers in time.

**The consensus view did not remain frozen.** Val0 samples show views
654, 655, 656, 657, 658, 659 at approximately 01:08:33, :35, :39, :47,
01:09:04, and :34. Timeouts continue accumulating. Increasing waits are
consistent with the source's exponential pacemaker backoff:
`crates/hotstuff_rs/src/pacemaker/implementation.rs`, `update_view` and
`stall_multiplier`. Node wiring is in `crates/torus-node/src/main.rs`.
This is evidence of continuing pacemaker activity, not proof of a deadlocked
consensus thread or proof that backoff caused the initial missing commits.

Body-fetch/sync failures precede the first queue warnings. Queue warnings may
amplify pressure once progress is lost; their presence alone does not establish
the initiating fault. There are no panic/fail-stop matches in these retained
logs. Exact missing body/QC ancestry, the initial retrieval failure, and whether
the chain would eventually recover remain unresolved.

## Log amplification and collector change

`Send Queue full` warnings print entire RawMessage payloads. There are
2,756/1,849/24 such lines on the three nodes, occupying approximately 1.513 GB
of 1.661 GB of logs (91%). This is a large observable I/O cost; its causal
contribution to the stall has not been measured.

The old collector repeatedly copied/scanned whole logs in Bash and took roughly
12 minutes after node shutdown for this cell. `collect_logs.py` now scans each
log once and retains queue-warning byte counts. Its dissemination counters
match the original r3 exactly, including body pushes 28/33/30, body-fetch
exhaustions 4/5/6, and sync fallbacks 4/14/15. Node logging is unchanged.

## Checks and next step

Regression tests cover grow-stop-resume, terminal and single-validator stalls,
missing/invalid scrapes, incomplete evidence, quiet-but-stopped HTTP endpoints,
advancing empty blocks, counter changes/resets, and success/failure exit gates.
Existing summarizer and harness checks cover agreement, crash evidence, and
phase accounting. Retained-cell replay checks preserve throughput and AGREE,
reject r3, and compare streamed counts against original log-derived totals.
Replay script and outputs live beside the re-scored copies.

The next runtime investigation should trace proposal/ancestor identifiers,
body availability and fetch destinations/results, highest QC, committed/executed
heights, and computed view deadlines around the first lost commits. The retained
sequence narrows the search to body retrieval/sync and subsequent recovery; it
does not justify changing consensus backoff or claiming a throughput gain.
Establish a clean advancing baseline before accepting block-construction A/Bs.
