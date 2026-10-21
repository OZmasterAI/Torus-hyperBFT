# RH4: what `--retention-blocks` costs before the first deleting pass

Status (s131 follow-up, branch `perf/rh4-gc-observability`): analysis and a microbench, no behaviour change.
Question (roadmap, pre-testnet "Validator history retention", item 2; perf doc section 95): bench 8h's r2-B cell
(`TORUS_RETENTION_BLOCKS=1000`) ran 9.3 native blocks/s against r2-A's 16.4 and was already the slowest cell in
minute 1, before its first deleting GC pass (load +90 s). What does retention do per block from block 1, on which
thread, at what cost, and can that explain the r2-B cell?

"(estimate)" marks a number not measured.

## What `--retention-blocks N` turns on

`crates/torus-node/src/main.rs` does three things with the flag (N = 1000 below):

1. `set_block_tree_retention(Some(max(N, 1000)))`: hotstuff's block-tree pruner, run after every commit.
2. `TorusApp::set_da_body_gc`: the RH4 DA body GC, one pass per dispatched block, window `max(N, 1000) + 128` = 1128.
3. `pruner_config`: the background `StatePruner` (bodies, receipts, action status, CoreWriter links in the state DB).

Nothing else reads the flag. The exec thread runs no new code.

## Per-block steps with retention on

Thread "algo" is the hotstuff algorithm thread: `BlockTreeSingleton::update` (commit, then the tree pruner), then
`feed_committed_blocks_to_app` -> `TorusApp::on_committed_block` -> dispatch, where the GC pass runs after the
durable body persist and the mempool prune and BEFORE `exec_tx.send`. Every row except 8 runs on it, serially with
voting and proposing.

Costs: release microbench `rh4_early_cost_per_block` / `rh4_early_cost_block_tree_pruner` (`app.rs` / `da_gc.rs`
tests, `#[ignore]`; `cargo test --release -p torus-consensus --lib rh4_early_cost -- --ignored --nocapture`).
The stream has 1,500 blocks of 130 order batches (8 orders each). The bodies go into the DA store before each commit,
as ingress does. The tree prune pointer moves as hotstuff's pruner moves it. Values are p50 per block, on the 18c
host, from two runs: [A] the test state DB, load 1-2; [B] the node's layout (DA store and block tree in a consensus
DB opened with `open_consensus`), load ~20 from other builds.

| # | step (code) | thread | runs | p50 per block, Torus 1..1001 [A] / [B] |
|---|---|---|---|---|
| 1 | tree pruner check: `prune_old_committed_blocks_with(1000)` reads the committed tip (2 point reads) and returns while the hotstuff tip is <= 1000 | algo, in `update` | every commit | 3 us (mean 3.5); 28 us once it deletes one block per commit (hotstuff tip > 1000) |
| 2 | body presence `MultiGet` over the block's hashes (`mirror_missing_native_to_da` -> `missing`; the bodies are all there, nothing restored) | algo, dispatch, before the exec send | every block with actions | 413 us / 429 us |
| 3 | tree prune pointer read (`DaBodyGc::keep_from`) | same | every block | 13 us / 12 us |
| 4 | GC state read | same | every block | 7 us / 8 us |
| 5 | record: latest-reference `MultiGet` over the hashes (nearly all misses), then the row (2 + 32 N bytes), N references (39 + 10 bytes) and the GC state into the batch | same | every block with actions | 658 us / 664 us |
| 6 | one `WriteBatch` to the consensus DB (~10.6 KB at N = 130) | same | every block with actions | 422 us / 444 us (p99 0.9 / 6.1 ms) |
| 7 | tip read, tail read and decode, due-row walk, deletes | same | only once a row is due: Torus 1129 on | 0 before. After: tip 15 us, tail 7 us (0 tail blocks in the bench), walk 78 us, write grows to 2.4 / 2.8 ms |
| 8 | `StatePruner::maybe_prune` | tokio blocking pool, 30 s timer | returns at once until Torus 1000; then 4 range deletes per 1,000 blocks | not per block, off algo and exec |
| - | whole GC pass (2-6) | algo | every block | 1.54 ms / 1.69 ms (mean 1.55 / 2.06) |
| - | `on_committed_block`, GC off -> on | algo | every block | 2.33 -> 4.27 ms (+1.93) / 2.49 -> 4.87 ms (+2.37) |

At 147 bodies per block (r2-B's in-window mean) run [A] gives a 1.69 ms pass and +2.43 ms on `on_committed_block`.
Torus 1002..1128 (tree pruning, nothing due yet) costs about the same as 1..1001: 2.03 ms per pass. Once rows are
due (1129 on) the pass costs 4.2 ms (p50 [A]), mostly the larger write batch.

**The microbench is slower than the node.** In the same bench the deleting passes take 4.2-5.1 ms p50. 8h measured
1.5 ms (107 bodies) and 2.1 ms (147 bodies) per pass in the node, so the bench reads 1.7-2.7x high (shared host,
test process). Scaled by that ratio, a record-only pass in the node costs about 0.6-1.0 ms per block (estimate).
Section 95.1 says the passes before the first deletion are "all <= 0.1 ms". That fits passes over empty blocks
(before the load). A pass over a 100-150 body block does two 100-150 key `MultiGet`s and a ~10 KB write, so it
cannot be 0.1 ms. The 8h rerun can check this directly: the per-pass mean is the 1 Hz `torus_da_gc_seconds`
sum / count from load start to the first deleting pass.

Timeline in 8h (derived): the first deleting pass (Torus 1129) came at load +82 / +90 s, at ~8.5-10 blk/s, so the
chain was at about Torus 300-400 at load start (estimate). In minute 1 of the load, retention therefore did steps
1-6 only: the tree pruner's no-op check and the record-only GC pass. The tree pruner started deleting ~13 s before
the first deleting pass. The state pruner's first pass came at the first 30 s tick past Torus 1000.

## Can it explain r2-B (16.4 -> 9.3 blk/s)?

Not by itself.

* **Size.** Before the first deleting pass, retention adds about 0.6-1.0 ms per block in the node (estimate), and
  1.5-2.4 ms in the bench, to the algo thread. Add all of it serially to every block of r2-A (61 ms per block at
  16.4 blk/s) and the rate is 15.8-16.2 blk/s. Reaching 9.3 blk/s takes ~47 ms more per block, 20-80x the cost.
* **Same work, different outcome.** r1-B ran the same binary, N and load. It did the same per-block work and came out
  faster than its A (13.1 vs 12.6 blk/s, same block regime).
* **Minute 1.** B cells ran 9.8 (r1) and 8.5 (r2) blk/s in minute 1; A cells ran 9.5 and 10.9. That spread is
  within the warm-up spread. The r2 gap opened later: A climbed to 17-18 blk/s while B stayed at 7.4-11.1. That is
  the big/small block-regime split, which archive-only cells show too (8h A 12.6-16.4, 8g A 9.7-11.8).
* **Exec.** Exec on-CPU per 1k fills is unchanged (B / A 1.003). Blocks/s x exec block_ms is ~1.0 in both arms, so
  the exec thread is the bottleneck. The GC pass delays only the handoff (`exec_tx.send`), by the pass time. That
  costs throughput only while the exec thread sits idle waiting for the next block.

What the data cannot exclude: if the block-regime choice is self-reinforcing (hypothesis: a later commit makes the
next block bigger and slower), a ~1 ms per-block nudge on the algo thread could tilt which regime a cell settles
in. n = 2 cannot test that. The planned rerun can (two more A B B A rounds, blocks/s per minute from minute 1, the
first deleting pass marked, and the new per-scrape max gauge).

## Work that could be cut (proposals, not done here)

1. **Skip the presence `MultiGet` (step 2) until the GC has deleted something.** The restore only puts back a body
   the GC itself deleted (a re-inclusion validated while the action's earlier row was pending, that row aging out
   before this dispatch, Codex P1 sibling). Before the first deletion it cannot find anything. This saves ~30% of
   the pre-window pass (~0.4 ms per block in the bench). After the window it could check only the hashes whose
   reference lookup (step 5) found no live reference. That needs a review: a body can be missing for reasons the
   reference does not show.
2. **Run the pass after `exec_tx.send`**, on the same thread and in the same order. The exec handoff would then no
   longer wait on it. To check: nothing between the pass and the send reads the GC state, and the crash order
   (durable body row, then GC record) is unchanged, since the body persist still comes first.
3. **Move the pass off the algo thread** to one dedicated thread fed `(height, hashes, shard_count)` in order (the
   doc comment on `gc_da_bodies_after_dispatch` sketches this; a late pass only lengthens the window and a lost one
   is back-filled). This removes steps 2-7 from consensus. It needs a review of the hold rules, which read the tree's
   committed tip and tail at pass time.
4. **The reference `MultiGet` (step 5, ~40% of the pass)** almost always misses, because re-inclusions are rare.
   First check whether `cf_native_pending`'s bloom filters cover the 39-byte reference keys (they share the CF with
   the 32-byte body keys). If they do not, a separate CF or a prefix bloom for references is an option. Measure
   before changing.

## Proposal 3 done: the GC worker (s132, branch `perf/rh4-gc-worker`)

Bench 8o (perf doc section 100) measured retention 1000 at block p50 +8.1% and 51-189 ms passes on the algo
thread. The pass now runs on one worker thread (`crates/torus-consensus/src/da_gc_worker.rs`; design and safety
argument in the `da_gc.rs` module docs, "Where the passes run"). The algo thread moves the block's hashes into a job
and `try_send`s it on a bounded queue (512 heights; full = the height is coalesced and back-filled from the block
tree). `TORUS_DA_GC_INLINE=1` restores the old in-dispatch pass for A/B runs. New metrics:
`torus_da_gc_handoff_seconds`, `torus_da_gc_queue_depth`, `torus_da_gc_lag_heights`,
`torus_da_gc_coalesced_heights_total`, `torus_da_gc_fence_kept_total`, `torus_da_gc_fence_pins`.

`rh4_early_cost_per_block` (release, 18c, other builds running; 1,500 blocks x 130 order batches, three arms
interleaved), `on_committed_block` p50 on the algo thread:

| segment | GC off | inline | worker |
|---|---|---|---|
| Torus 1..1001 (record only) | 2.21 ms | 4.16 ms | 2.25 ms |
| 1002..1128 (tree prunes) | 2.46 ms | 4.73 ms | 2.53 ms |
| 1129.. (deleting) | 2.45 ms | 6.45 ms | 2.53 ms |

Handoff mean 127 us (n = 1,500) against a 2.6 ms inline pass mean. The worker's own pass mean is 3.7 ms (it now
competes with the algo thread for CPU and cache). Not yet measured in a node: the 8o A/B rerun with the worker.

## The slow-pass log and metrics (part 1, same branch)

A pass of at least `TORUS_DA_GC_SLOW_MS` (default 50, `0` = every pass; resolved and logged at
`set_da_body_gc`) logs one WARN line:

```
DA body GC: slow pass height=.. total_ms=.. threshold_ms=50 bodies_recorded=.. rows_deleted=.. bodies_scanned=..
  bodies_deleted=.. shard_keys_deleted=.. tail_pinned=.. backlog=.. restore_ms=.. bookkeeping_read_ms=.. record_ms=..
  backfill_ms=.. heights_backfilled=.. tip_ms=.. tail_ms=.. tail_blocks=.. tail_hashes=.. delete_scan_ms=..
  write_ms=.. committed_tip=.. dispatch_lag=.. held=..
```

`restore_ms` is step 2. `bookkeeping_read_ms` is steps 3 + 4. `record_ms` is step 5. `write_ms` is the batch
write(s): the record, the deletes and the GC state go in one batch. `delete_scan_ms` is the due-row walk with the
tail read excluded. `dispatch_lag` is the tree's committed tip minus the height, `none` when there is no tip.
`held` is the hold reason or `none`. Metrics:
* `torus_da_gc_seconds` buckets now run to 1.64 s (15 buckets; the old top was 0.82 s).
* `torus_da_gc_last_pass_max_seconds` is the longest pass since the previous scrape. Each scrape resets it, so it
  is meant for one 1 Hz scraper.
* `torus_da_gc_slow_passes_total` counts the passes at or over the threshold.

The timers are `Instant`s around each phase (`DaGcPassStats`, kept outside `DaGcOutcome`). A test runs the same
passes timed and untimed and checks equal outcomes and byte-identical stores.
