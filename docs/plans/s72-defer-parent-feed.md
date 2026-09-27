# Design: defer the commit feed on the next leader's parent-body insert (s72 fix D)

## Problem

The next leader N of view v+1 cannot build until the parent body B_v is in its
block tree. s72 split of the s70-ladv logs (4 cells, body-late views, i.e. the
body arrived after N entered v+1):

| piece | p50 ms | mean ms |
|---|---|---|
| N StartView(v+1) -> body received | 7-51 | 100-190 |
| body received -> first `on_committed_block` dispatch | 94-114 | 115-146 |
| further committed blocks | 0 | 34-47 |
| last dispatch -> `produce_block` | 36-43 | 54-82 |
| body received -> `produce_block` | 158-210 | 203-274 |

Delivery hops are small at the median (serve queue ~1.5, body transfer ~10).
The time is on N's algo thread after the body arrives: `validate_block`
(~57 ms, DA reconstruct ~48), the tree write, and the commit feed.

Why the feed is there: N's PC for B_v cannot commit B_{v-1} while B_v's body is
missing, so the commit happens in `try_insert_body` -> `block_tree.update` ->
`process_update_result` -> `feed_committed_blocks_to_app` ->
`on_committed_block` (materialize, durable persist, mempool prune, exec
dispatch), before N's deferred proposal is retried.

## Context

- s65 item C (`TORUS_DEFER_COMMIT_FEED=1`, default OFF) defers the feed only in
  `finish_leader_proposal` (the leader's own insert): header broadcast, inline
  self-vote, then feed. s66 A/B: no gain at n=3. It never covered the
  parent-body insert, which this split shows is the one on the critical path.
- The feed is replayable (S444): it walks `APP_FED_BLOCK_HEIGHT + 1 ..=
  highest_committed`, so a later call delivers everything a skipped call would
  have. The 1 s live reconcile logs an error if heights stay unfed, so the
  deferral must be flushed promptly, not left to the watchdog.
- `block_tree.update` (locks, highest PC, durable commit write) is unchanged;
  only the app callback moves.
- Committed-but-unfed actions stay in the app's in-flight window until
  `on_committed_block`, so `produce_block` still excludes them (same property
  C relied on; C cells were AGREE).

## Options

### Option A: defer the feed in `try_insert_body` when the deferred proposal waits on this block (recommended)

`try_insert_body` checks `defer_parent_feed && proposal_deferred &&
block.hash == highest_pc.block`. If true, it forwards validator-set updates as
today but skips the feed and sets `commit_feed_pending`. The next loop pass
retries the proposal (step 4), and `finish_leader_proposal` then uses C's order
(header, inline self-vote, feed) for that retry only, because a feed is
pending; every other proposal keeps its order.
Right after step 4 the loop calls `run_pending_commit_feed`, which feeds if
anything is still pending (proposal not retried: view moved, backpressure
yield, parent chain still incomplete, or the own insert committed nothing).

- Files: `hotstuff_rs/src/hotstuff/implementation.rs`, `hotstuff_rs/src/algorithm.rs`,
  new test `hotstuff_rs/src/hotstuff/parent_feed_order_test.rs`.
- Trade-offs: small and local; flag-gated for A/B. Removes the feed
  (~80-130 ms median) from N's propose path; validate (~57 ms) stays.
  Exec gets the committed block a few ms later (after the header broadcast).
- Effort: Small. Risk: Low (app callback timing only, replay-safe).

### Option B: move `on_committed_block`'s heavy work to the exec thread

Keep the feed where it is but make it cheap: hand the block to the exec
thread and do materialize + durable persist there.

- Trade-offs: helps every feed site, but the durable body write on commit is
  the t12 crash-recovery fix (committed-but-bodiless height); moving it needs a
  new crash-safety argument. Larger.
- Effort: Medium-Large. Risk: Medium.

### Option C: cut validate cost on the next leader

Skip or overlap the DA reconstruct for bodies the next leader needs.

- Separate target (~57 ms), orthogonal to A. Not this change.

## Recommendation

Option A behind `TORUS_DEFER_PARENT_FEED=1` (default OFF), A/B on one binary:
off vs on, ABBA, n>=2 pairs, same settings as s70 (cap 200, 300 s, timeout
base 1200, `TORUS_BODY_FETCH_TRACE=1`). Success: `propose_build` and the
body-late receive->produce_block drop, and matched/s does not fall.

## Not Building (YAGNI)

- No separate storage for the pending `UpdateResult`: the feed is replayable,
  so one bool is enough.
- No new metric: `view_propose_build_seconds` and the s72 log join
  (receive -> produce_block) measure the effect. One info line per skip
  (`parent_feed_deferred: height= committed=`) shows which views took the path.
- No change to C's flag semantics: `TORUS_DEFER_COMMIT_FEED=1` alone behaves
  as before.

## Open Questions

- Should D also defer the feed for a body that arrives before N enters v+1
  (it then runs during the vote gather on N's algo thread)? Not in scope; the
  A/B shows whether the late case is enough.
