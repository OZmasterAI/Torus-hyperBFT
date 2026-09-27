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
