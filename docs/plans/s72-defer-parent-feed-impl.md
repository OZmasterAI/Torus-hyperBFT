# Implementation Plan: defer the commit feed on the next leader's parent-body insert (s72 fix D)

## Design Decision

Option A of `docs/plans/s72-defer-parent-feed.md`: `TORUS_DEFER_PARENT_FEED=1`
(default OFF). `try_insert_body` skips the app feed when the deferred proposal
is waiting on the inserted block; the proposal retry runs C's post-broadcast
order; the algo loop flushes any feed still pending right after step 4.

## Success Criteria

1. Flag ON, next leader missing its parent body: after the body insert no
   `on_committed_block` has run; after the proposal retry the header broadcast
   precedes every `on_committed_block`, and all committed heights are fed.
2. Flag ON, pending feed with no proposal retry: `run_pending_commit_feed`
   delivers it, and a second call is a no-op.
3. Flag ON, follower body insert (no deferred proposal): feed runs inline, as
   today.
4. Flag OFF: byte-identical order to today (feed during the body insert).
5. Existing `hotstuff_rs` and `torus-consensus` suites stay green (timing tests
   run sequentially).

## Tasks

### Task 1: failing tests

- **Test first**: `crates/hotstuff_rs/src/hotstuff/parent_feed_order_test.rs`
  (registered in `hotstuff/mod.rs` next to `leader_feed_order_test`). Chain
  g(h0) <- p(h1, QC(g)@v1) <- q(h2, QC(p)@v2); g, p in the tree, q not;
  `highest_pc` = QC(q)@v3. Leader of view 4 enters view 4 (defers, requests
  q), then receives `BlockDataResponse { view: 3, block: q }`, then retries
  `enter_view(4)` and calls `run_pending_commit_feed`. Tests for criteria 1-4.
- **Verify**: `cargo test -p hotstuff_rs parent_feed_order` fails to compile
  (`set_defer_parent_feed`, `run_pending_commit_feed` missing).

### Task 2: flag, pending bit, deferral in `try_insert_body`

- **Implementation** (`hotstuff/implementation.rs`): fields
  `defer_parent_feed: bool` (from `TORUS_DEFER_PARENT_FEED`, `1` enables, via
  `parse_defer_commit_feed`) and `commit_feed_pending: bool`; test setter
  `set_defer_parent_feed`. In `try_insert_body`, when `defer_parent_feed &&
  proposal_deferred && highest_pc.block == block.hash` and the update committed
  something: set `commit_feed_pending` and call `process_update_result` with an
  empty commit list (validator-set updates still forwarded); the `highest_pc`
  read fails open. New `run_pending_commit_feed` and `has_pending_commit_feed`.
  `finish_leader_proposal` uses the deferred order when `defer_commit_feed ||
  commit_feed_pending` (the retry only).
- **Verify**: `cargo test -p hotstuff_rs parent_feed_order leader_feed_order`.

### Task 3: flush from the algo loop

- **Implementation** (`algorithm.rs`): after step 4, call
  `self.hotstuff.run_pending_commit_feed(&mut self.block_tree, &mut self.app)`;
  skip the 1 s live reconcile while a feed is pending (it would log a false
  "LIVE reconcile found" error and feed before the retry).
- **Verify**: `cargo test -p hotstuff_rs` (sequential) and
  `cargo test -p torus-consensus`.

## Verification (end-to-end)

A/B on one binary: `TORUS_DEFER_PARENT_FEED=0` vs `=1`, ABBA, s70 settings.
Compare `propose_build`, the s72 receive->produce_block join, matched/s,
timeouts; all cells must be ACCEPT/AGREE/PASS.

## Rollback

Flag default OFF; unset the env var. Revert the branch if the OFF path differs.
