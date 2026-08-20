# bl2 `flush-1deep-worker-applied-height-fence` — implementation notes (2026-08-20)

Branch `cand/bl2-flush-1deep-worker-applied-height-fence` on `perf/matched-200k` @ `6eab88d`.
Implements design candidate 3 of `design-exec-pipeline-2026-08-20.md`
(`state-write-async-overlay-carry`). The candidate's option (b) — overlap only the root —
is **not buildable** without breaking the fence (design §3.6: the trie/mirror ops are
appended to the SAME atomic batch as the state + marker, `backend.rs`
`flush_pending_with_native_trie_stats`), so the whole `flush_with_native_trie_stats` moves to
the worker and the root overlap comes with it.

## Kill switch

`TORUS_EXEC_PIPELINE=1` (only `"1"`, trimmed) attaches the flush worker. **Default OFF**:
unset ⇒ `ExecutionContext::flush_worker == None` ⇒ the exec path is today's, byte for byte
(the only change on that path is that verify's `get_session` and the nonce guard read through
an empty, parent-less overlay — identical reads).

Bench: `EXTRA_ENV='TORUS_EXEC_PIPELINE=1'` on `run-cell.sh` (the drain already waits for
`flush_worker_depth == 0` on every node, bl1). Expect on val0 `chain_ms` ≈ `block_ms − flush`
(+ `handoff_wait_ms`), `pipelined_ms` ≈ former `flush.ms`, `phase_by_node.val0.phases.flush`
unchanged in meaning (observed by W).

## What runs where

| stage | serial (flag off) | pipelined block |
|---|---|---|
| skip-check | durable marker | `max(durable marker, exec_applied)` (E-owned watermark) |
| parent-link check | DB | DB (header is dispatch-durable — fast-path precondition) |
| verify `get_session`, nonce replay guard | DB | `overlay(N)` = own pending → `pending(N−1)` → DB |
| engine, save_books (both passes), stash resident | E | E, on `overlay(N)` |
| marker put | in flush batch | ALSO into `overlay(N)` (same key/bytes; root-neutral CF) so the resident guard reads N via the layer |
| build + root + marker + atomic write | E | **W** (`FrozenPending::flush_with_native_trie_stats`, same code path) |
| `resync_evm_accounts` | E, after the write | W, after its own write, only if `dirty_evm_accounts()` non-empty |
| trade rows → `trade_writer` | after flush | after hand-off (may land before batch(N); idempotent keys) |
| empty / non-native block marker | direct put on E | `Job::Marker` with a 1-key `FrozenPending::marker_only(N)` layer, written by W in order |

Fast-path eligibility (all): worker present ∧ `durable.header ∧ durable.body` ∧ `!has_evm` ∧
`pending_slashes.is_empty()` ∧ `!is_epoch_boundary(N)`. Else **barrier**: `wait_idle()` (fail-stop
if W failed), `last_job = None`, then the serial path on the DB. Boot replay uses
`DurableRows::default()` ⇒ always serial; W is attached only after `replay_committed` returns.

Depth: `sync_channel(0)` rendezvous — `submit(N)` returns when W has *received* N, which W does
only after finishing N−1. Invariant when E starts engine(N+1): heights < N durable, the only
non-durable set is `pending(N)` = `overlay(N+1).parent`. `flush_worker_depth` counts jobs handed
off and not yet durable (incremented before the blocking send), so `== 0` ⇒ nothing in flight.

## Hazard table (as implemented)

| # | hazard | mechanism | test |
|---|---|---|---|
| F1/F6 | two non-durable sets, one layer | rendezvous; parent always layered (durable or not) | `exec_pipeline_parked_worker_blocks_next_handoff_and_reads_previous_height`, `rendezvous_blocks_first_handoff_and_preserves_order` |
| F2/F8 | epoch inflation / consensus-thread staking writes | boundary blocks serial; dirty EVM accounts resynced on W after write | `exec_pipeline_state_identical_over_sequence` (epoch_length 4 → heights 4/8/12 serial, `CF_ACCOUNTS`/`CF_HASHED_*` compared) |
| F3 | re-delivered height re-executed | skip-check on `max(durable, exec_applied)` | `exec_pipeline_redelivery_is_skipped` |
| F4 | replay through the pipeline | replay serial by construction; W attached after replay | `exec_pipeline_fold_header_runs_serial`, `exec_pipeline_crash_before_write_replays_both_and_matches_serial` |
| F5/F7 | resident guard sees a lagging marker | marker in `overlay(N)`; Marker jobs carry a 1-key layer | `resident_guard_sees_marker_through_parent_layer` (+ control) |
| F9 | `fold_header` block rides W → parent-link check disabled | fast path requires durable rows | `exec_pipeline_fold_header_runs_serial` |
| C1 | crash with batch(N) in flight, engine(N+1) ran | marker N−1 durable; replay re-executes N, N+1 | `…crash_before_write_replays_both…` |
| C3 | W write error after engine(N+1) | W latch + `exec_failed`; E's hand-off of N+1 fails → nothing written | same test + `write_error_latches_failstop_and_refuses_further_jobs` |
| trie error, write Ok | today's semantics (state + marker durable, root stale, caches invalidated) | W distinguishes by re-reading the durable marker | — |
| C5/C6 | marker regression across empty blocks | Marker jobs ordered behind batch(N−1) on W | parked-worker test (marker 1→2→3) |
| C7 | shutdown with a job in flight | `FlushWorker::Drop` drains + joins (dropped with `ExecutionContext`) | `drop_drains_and_joins` |
| determinism | ON vs OFF, all CFs + persisted root | same batch code over the same maps | `exec_pipeline_state_identical_over_sequence` (13 blocks: empties, boundaries, duplicate action across k/k+1, session created in k used in k+1, keys read across empties) |

Costs accepted (design §3.5): one more block of crash rewind; stricter fail-stop on a W write
error; ≤ 2 pending sets in RAM; RPC/snapshot readers lag ≤ 1 block (harness digest drains on
`flush_worker_depth == 0`).

## Not done here

* No devnet cell run (rule of this round). Acceptance per the candidate: two agreeing 10 m × 120 s
  ON reps with `chain_ms` down by ≥ flush ms (≈ 260 ms on the r9 shape, ≥ 60 ms root-only floor),
  `handoff_wait_ms < 5`, matched/s within −5 % of the same-binary OFF arm, AGREE, plus the
  kill −9 crash gate. Flip the default only in a separate commit naming the cells.
* Candidate 1 (`advance_untouched` on the non-native path) must run in BOTH branches of the
  non-native marker write (`pipelined` → Marker job, else direct put) when merged.
* Staking-changing workloads near an epoch boundary remain out of validated scope for the flag
  (design §3.1 row 11).
