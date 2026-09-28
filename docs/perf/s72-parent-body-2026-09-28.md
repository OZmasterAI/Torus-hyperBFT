# s72: the next leader's parent-body wait, and fix D (deferred commit feed)

## 1. Split of the parent-body wait (s70-ladv logs, no new code)

Per view v: L = leader of v, N = leader of v+1. Joined from the
`body_fetch_diag` admission/serve/receive/view_close lines, `validate_block
called`, `on_committed_block: sending to execution pipeline` and `produce_block
called parent_height=` (app height = hotstuff height + 1).

Delivery, p50 ms (4 cells): header L->N ~6, N queue ~2, N proposal_rx -> body
request at L 31-40 (mean 56-75), L serve queue ~1.5, body transfer ~10, N
response queue ~3, receive -> validate start <1.

Body-late views (N receives the body after it entered v+1):

| piece | p50 ms | mean ms |
|---|---|---|
| N StartView(v+1) -> body received | 7-51 | 100-190 |
| received -> first commit dispatch | 94-114 | 115-146 |
| further committed blocks | 0 | 34-47 |
| last dispatch -> produce_block | 36-43 | 54-82 |
| received -> produce_block | 158-210 | 203-274 |

The time is on N's algo thread after the body arrives: validate (~57 ms) and
the app commit feed, which runs inside `try_insert_body` because N's PC on
B_v cannot commit B_{v-1} until B_v's body is in the tree.

## 2. Fix D (678719e, `TORUS_DEFER_PARENT_FEED=1`, default off)

When N's proposal is deferred and the inserted body is `highest_pc.block`,
skip the feed in the insert; the retried proposal broadcasts first (item C's
order for that retry only) and the feed runs right after.
Design: `docs/plans/s72-defer-parent-feed.md`.

## 3. A/B `~/bench-results-matched/s72-dpf-20260927`

One binary (sha256 `2a5439be…`), ABBA, cap 200, 300 s, timeout base 1200,
trace on. `s72-dpf-warm-w0` is an aborted start (stopped during genesis).

| cell | flag | verdict | matched/s | native blk/s | timeouts | sync_fallback | exec q p50/p90 (val0) |
|---|---|---|---|---|---|---|---|
| warm-w1 | on | REJECT | 74.8k | 1.275 | 23 | 19 | - |
| on-r1 | on | REJECT | 71.8k | 1.282 | 35 | 14 | - |
| off-r1 | off | ACCEPT | 63.0k | 1.058 | 29 | 0 | - |
| off-r2 | off | ACCEPT | 72.7k | 1.231 | 24 | 0 | 11/49 |
| on-r2 | on | REJECT | 70.5k | 1.223 | 30 | 18 | 30/65 |
| on-r3 | on | REJECT | 69.3k | 1.187 | 23 | 20 | - |
| off-r3 | off | REJECT | 66.5k | 1.195 | 33 | 1 | - |

Every flag-on cell fails the dissemination gate ("justify fetch exhausted 9
retries ... falling back to sync"); flag off 0/0/1. Throughput is not
comparable across a failed gate.

## 4. Mechanism of the failure

on-r3, view 685, leader val0: proposed at 26.400, then ran the pending feed.
`on_committed_block` dispatches to the exec thread with a blocking
`sync_channel` send (`TORUS_EXEC_NONBLOCKING_DISPATCH` off, bound
`EXEC_QUEUE_DEPTH` 64). The exec queue was full (exec ~65 blocks behind), so
the algo thread stalled until exec freed slots (dispatch of 668 at 27.022
right after exec finished 602; resumed at 27.76 after 603). For ~1.35 s the
leader served no body requests for the block it had just proposed; val1 and
val2 exhausted their justify-fetch retries (27.53 / 27.63), fell back to sync,
and the view timed out.

Flag off has the same blocking send, but before the proposal, where it paces
the leader instead of hiding the only copy of the new body. D also lets
consensus run further ahead of execution: exec queue in the load window,
off p50 11-16 / p90 49-61 / at bound 0-11 % of samples; on p50 25-30 / p90
58-65 / at bound 8-26 %.

## 5. Verdict

Not adopted. Branch `perf/s72-defer-parent-feed` kept local, unmerged.

Options if revisited:
- Defer the feed only when the exec channel has room (needs an App hint to
  hotstuff), so the post-broadcast feed can never block.
- Pair D with `TORUS_EXEC_NONBLOCKING_DISPATCH=1` (full channel parks the
  block instead of blocking the algo thread); changes backpressure globally,
  needs its own A/B.
- Serve block-data requests off the algo thread, so a stalled algo thread
  cannot starve body fetches (helps flag-off tails too).
- Attack the other half of the wait: validate (~57 ms, DA reconstruct ~48).

## 6. Option 3: serve block-data requests off the algorithm thread (f9b013e)

`TORUS_BODY_SERVE_THREAD`: the poller routes `BlockDataRequest`s to a
`BlockDataServer` thread that reads the block from a `BlockTreeCamera`
snapshot and replies itself; misses go to the algorithm thread as before.
Design: `docs/plans/s72-body-serve-thread.md`.

A/B `~/bench-results-matched/s72-bst-20260928`, one binary (sha256
`9cdd817e…`), s70 settings, rounds r1-r3 with three arms (rotated order),
r4-r6 with srv/srvd only (alternating). All 16 cells ACCEPT/AGREE/PASS,
sync_fallback 0 everywhere.

| arm | cells (matched/s) | mean | median |
|---|---|---|---|
| off | 67.0 / 66.4 / 67.3k | 66.9k | 67.0k |
| srv (serve thread) | 65.4 / 70.3 / 71.1 / 69.1 / 65.2 / 65.8k | 67.8k | 67.4k |
| srvd (serve + D) | 74.5 / 74.9 / 52.0 / 66.0 / 68.0 / 59.4k | 65.8k | 67.0k |

Per view (r1-r3, mean ms): leader serve queue off 12-18, srv 3-4, srvd 2-9;
next leader StartView -> produce_block off 74-90, srv 82-90, srvd 49-50;
leader propose off-r3 305, srvd 237-252. With the serve thread, D's saving is
real and no longer shifts onto body delivery, but the node that just ran a
deferred feed votes later on the next header in slow views (mean +65-90 ms,
p90 +200-500 ms in srvd-r1/r2), and the exec queue sits at its bound 22-36 %
of the time (off 0-11 %).

Bad minutes (per-minute mean cycle 900-1250 ms against ~650-800) occur in
every arm and in the s70 binary (off-r1, srv-r2, s70-ladv-on-r1, srvd-r3
twice). Not explained by exec-queue saturation, start load, UDP receive-buffer
drops (75-105/s in every cell, fewer in srvd-r3's bad minutes), timeouts or
Send Queue floods. Open.

### Verdict

- Serve thread: adopted, on by default (`TORUS_BODY_SERVE_THREAD=0` turns it
  off). Throughput neutral within cell noise; bodies keep flowing while the
  algorithm thread is busy.
- D on top: fails the pre-declared rule (needed >= 4/6 pair wins and >= +3k
  mean and median; got 3/6, mean -2.0k, median -0.1k). Stays off. Revisit only
  with the refinement that lets queued headers/votes go ahead of the deferred
  feed.
