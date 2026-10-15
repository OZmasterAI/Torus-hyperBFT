# s123 rank 2: propose-defer-on-exec-queue

Branch `exp/s123-propose-defer-on-exec-queue`, rebased onto main `2bc88bcf`. Built and unit-tested
(hotstuff_rs and torus-consensus suites, workspace nextest), not benched.

## Review fixes (after the static review of the first commit)

- (a) The hold is also capped by the time since view ENTRY: the leader proposes at
  `min(deadline - 300 ms, entry + max_view_time / 2)`. Entry times are roughly in step across nodes, deadlines are
  not (a fast run banks up to 2x `max_view_time` of surplus before the S395 rebase), so a leader can no longer
  outwait a follower's timeout on its own deadline alone. `HotStuff::view_entered_at` is set on every view change
  (not on a same-view retry); `app_backpressure_max_hold` is wired by `Algorithm::new` from the pacemaker's
  `max_view_time` (default 600 ms before wiring = half of the node's 1.2 s view time).
- (b) No deferral in the view right after a timeout (`highest_tc.view + 1 == view`): there a fresh proposal means
  the TC path found nothing to re-propose (or started RECOVER), and holding it could cascade one timeout into the
  next. A `highest_tc` read error also proposes at once (main's behaviour).
- Tests (written first; RED shown: compile failure for (a), and with the TC guard disabled the (b) test fails):
  `hold_is_capped_by_time_since_view_entry`, `max_hold_is_half_the_view_time`,
  `fresh_proposal_right_after_tc_is_not_deferred`, `older_tc_does_not_lift_deferral`.

## What changed

Two default-off env knobs, both node-local. With both unset the binary behaves as main does today. The
only extra is one startup log line that prints how the two knobs resolved.

1. `crates/hotstuff_rs/src/app.rs:314`: new trait method `App::proposal_backpressure(&self) -> bool`,
   default `false`. The doc states the contract:
   - It is a proposer-local hint and gates only a fresh proposal (the Generic/Decide path).
   - It is retried on the ~10 ms poll.
   - HotStuff proposes anyway near the view deadline.
   - It must be cheap and non-blocking, and must not touch consensus state.
2. `crates/hotstuff_rs/src/hotstuff/implementation.rs`:
   - `:3661` `APP_BACKPRESSURE_ESCAPE = 300 ms` (a quarter of the 1.2 s `timeout_base_ms`, `torus-node/src/main.rs:398`).
   - `:516` `app_defers_proposal`: true when the app reports backpressure and the view deadline is more than
     300 ms away. It writes a debug line once per view, tracked by the new field
     `app_backpressure_logged_view` (`:174`, set to `None` in `new`).
   - `:631` (the retry path) and `:851` (the fresh Generic/Decide path): the condition is now
     `should_yield_proposal_for_exec_backpressure() || app_defers_proposal(app)`. The consequence did not change:
     `proposal_deferred = true; return Ok(())`.
   - The re-propose path, the TC path (`create_proposal_based_on_tc`), nudges and votes are not gated. A comment
     at `:851` says so.
3. `crates/torus-consensus/src/app.rs`:
   - `:819` `parse_propose_exec_watermark` (trims; keeps only values > 0, otherwise `None`), and `:826` a
     read-once `propose_exec_watermark()` over `TORUS_PROPOSE_EXEC_WATERMARK`.
   - `:6122` `TorusApp::proposal_backpressure_at(watermark)`: `!exec_failed && exec_queue_len >= W`. It counts
     channel blocks only. `exec_queue_len` goes up before the send (`dispatch_to_exec`) and down after execution
     (`execution_loop`). `deferred_exec` is deliberately not counted; the reason is under Risks.
   - `:7741` trait override `proposal_backpressure()` calls `proposal_backpressure_at(propose_exec_watermark())`.
   - Control arm: `:839` `parse_exec_queue_depth` over `TORUS_EXEC_QUEUE_DEPTH`. N in 1..=64 gives N; anything
     else gives `EXEC_QUEUE_DEPTH` (64). It is used at the `sync_channel` (`:5895`), and the comment above it is
     updated. `torus-mempool/src/rate_limit.rs` is unchanged: a smaller channel only over-sizes the trust-cache
     floor, and values above 64 are refused.
   - One `tracing::info!` at the channel site logs both resolved knobs, so a cell's arm can be read from the node log.
4. Tests, written first, not run:
   - `crates/hotstuff_rs/src/hotstuff/proposal_backpressure_test.rs` (declared in `hotstuff/mod.rs`). It uses the
     g <- p, QC(p)@view 2 fixture from `leader_feed_order_test`, with an App whose backpressure flag the test
     flips, and counts `produce_block` calls and header broadcasts. Cases:
     1. Backpressure on, deadline +3600 s: deferred, no produce, no broadcast.
     2. A retry while still on stays deferred; once the flag is off, the retry gives exactly one produce and one header.
     3. Backpressure on, deadline +100 ms: proposes at once.
     4. An App without the override proposes at once (trait default is false).
     5. A TC for view 2 whose high tip `p` wins: re-proposes `p` while backpressure is on, with no produce.
   - `torus-consensus/src/app.rs:847` `propose_backpressure_tests` (`TorusApp::stub()`):
     - parse tables for both knobs
     - watermark `None`: false at any depth, and the trait hook is false while the env is unset
     - true iff the queue is >= W (0/15/16/17/66 against W = 16)
     - false once `exec_failed` is set
     - false when only `deferred_exec` holds 32 entries

## Why (evidence)

- The exec channel is `sync_channel(64)`, and the send is blocking unless `TORUS_EXEC_NONBLOCKING_DISPATCH` is set
  (main `app.rs` blocking `tx.send`, `else if tx.send(msg).is_err()` branch). At 300 markets it stays full:
  `torus_exec_queue_depth` is 64-66 in the shock window and in the baseline (perf doc sec 72.2).
- Shock to first liquidation takes ~65 heights, and that is the exec lag (sec 72.2: "the liquidation runs when the
  executor reaches the shock block"). Exec runs ~216-263 ms per block (sec 72.2), so 64 blocks are about 14-16 s
  of commit-to-exec.
- In tail intervals the fleet's HotStuff threads are blocked on that send for 441-652 ms on average (sec 66.4,
  table "blocked on exec send").
- Thinning blocks on the backlog (rank 1, `TORUS_EXEC_THROTTLE_WATERMARKS`) cost -25% matched/s and the queue
  still reached 66 (memory 272b71ca, s77 cell; doc `s77-s80-trade-history-and-streams-2026-09-30.md` sec 4). That
  memory also says "bound lag by delaying/pacing proposals (keep blocks full)", which is what this does.
- S470's `TORUS_PROPOSER_EXEC_WATERMARK` looks at `pending_headers + deferred_bodies` (implementation.rs
  `exec_backlog`), not at the exec queue (same s77 doc). This change reuses its deferral (re-polled every ~10 ms,
  algorithm.rs:208, 323-337) with an app signal.

How it should work (inferred, not measured): consensus is already exec-bound today, because every commit waits on
the full channel. With W < 64 the leader waits before it produces, so the same wait moves from "HotStuff thread
parked on send, no votes or messages handled" to "leader polls every 10 ms, everything else handled". Block rate
stays the exec rate and blocks stay full (the pool keeps filling while the leader waits), but the queue holds about
W blocks instead of 64.

## Expected gain (inferred; nothing was run)

- Exec lag (`torus_exec_queue_depth`): 64-66 -> about W to W+2 (W = 16: -75%).
- Commit-to-exec: ~14-16 s -> ~4-5 s at W = 16.
- Shock to first liquidation: ~65 -> ~W+3 heights.
- RSS: about -0.25 to -0.35 GB per node, from fewer decoded blocks in the queue (inferred from sec 48.2's 381 MB of
  queued blocks; not re-derived here).
- matched/s: about ±2% (same block rate, full blocks). Commit p99 and view timeouts: uncertain. The ~1.2 s floor
  outside checkpoint windows is timeouts (sec 72.5), and followers vote before the commit feed.

## How to measure (later, as extra arms in archy's job 4 at 300 m)

Arms on one binary:
- (a) main (both knobs unset)
- (b) `TORUS_EXEC_QUEUE_DEPTH=16`, the control: the same short queue, but still a blocking send
- (c) `TORUS_PROPOSE_EXEC_WATERMARK=16`

Use W >= 16: at 0.22-0.26 s per block that is ~4 s of buffer, against the largest commit gaps of 2.4-2.55 s (sec 72.2/72.3).

Compare:
- `torus_exec_queue_depth` (mean and max)
- shock-to-first-liquidation heights (`liq_stress.py`)
- consensus/view timeouts per cell (`ViewTimeout` events or the node log)
- commit-interval p50/p99, inside and outside checkpoint windows (`ckpt-gaps.py`)
- matched/s
- RSS
- nonce-expired evictions and mempool size: actions now wait before the proposal, and `NONCE_WINDOW_MS` is 60 s
  (`torus-types/src/eip712.rs:49`)
- the startup line "exec channel depth ... resolved at startup", to confirm the arm

Grep `s123 app backpressure` at debug level to count deferring views. Adopt (c) over (b) only if it wins on p99 or
timeouts.

## Risks

- **Late proposals near the escape.** Bounded by review fix (a): the hold ends at most `max_view_time / 2` after
  view entry (or 300 ms before this node's deadline, whichever is first), so followers keep at least about half a
  view to vote. Watch view timeouts in arm (c).
- **A fresh proposal after a TC** is never deferred (review fix (b)). In a backed-off view (`stall_multiplier`)
  the hold is still `max_view_time / 2` from entry, not scaled.
- **A slower follower can still fill its channel.** Each leader paces on its own queue only. A follower whose exec
  is slower than the others' can still reach 64 and block on its send, exactly as today. Its own leader views drain
  it back to W.
- **`deferred_exec` is not counted.** With blocking dispatch, holes drain only on commits, because
  `on_reconcile_tick` returns early unless dispatch is nonblocking (`app.rs`, `on_reconcile_tick`). Counting them
  would hold the leader, and with it the commits that drain them, until the escape in every view. Do not pair this
  knob with `TORUS_EXEC_NONBLOCKING_DISPATCH`: there, parks are the backlog and are not counted.
- **Extra work while deferring.** Each ~10 ms retry repeats the existing retry-path reads (`highest_pc`,
  `validator_set_state`, `leader_reputation`), the same cost as today's S470 or missing-parent deferrals. This is a
  small amount of extra KV reads on the leader.
- **With `TORUS_DEFER_PARENT_FEED=1`,** a body insert during a deferral marks the feed pending. The loop runs it
  right after `enter_view` returns (`run_pending_commit_feed`), so it is not held by the deferral.
- **Determinism:** none of this is consensus-visible. Deferral only changes when a leader proposes, never what it
  proposes or how anyone validates or votes. Mixed settings across nodes cannot fork.

## Untested / open

- Built and tested after the rebase onto `2bc88bcf` (9 hotstuff tests, 6 torus-consensus tests); no bench yet.
- The debug line's once-per-view behaviour has no test (it would need a log capture).
- How the TC test fixture's `SignatureSet::genesis()` behaves with `highest_tc` round-trip (borsh) is inferred
  from `implementation.rs` test `view_skip_refused_for_tc_nec_voted_current_or_disabled`, which builds the same
  TC shape.
- Coordination: item 7 step 1 touches implementation.rs only at ~:2224 (an event field), and this patch's hunks
  are at :168-177, :257-263, :495-539, :625-635, :841-855 and :3653-3665 (new-file lines). In torus-consensus `app.rs`, this patch's
  hunks are at :806-933 (new section after `nonblocking_exec_dispatch`), :5877-5895, :6108-6125 and :7731-7743.
  Phase 5 and hs-da-own-db (comment at :1582) edit other hunks. Expect textual offsets only, but check on rebase.

## Compile status

Builds and passes its tests on `2bc88bcf` (see the commit message for counts).
