# C1 pending-parent crash qualification: proposed design

Status: design only, 2026-09-18. No runtime hook is implemented by this document.
`TORUS_EXEC_PIPELINE` remains **default OFF**. A timing-only crash cell with a
positive replay gap can qualify replay recovery, but cannot establish C1.

The target condition is: W has received native batch N but has not written it;
the durable native marker is N−1; E has computed native block N+1 with pending(N)
as its immediate overlay parent; then the process is killed and serial boot
replay reconstructs both blocks. The proposed pause is at the end of N+1 compute,
before its handoff. This exercises C1's pending-parent state loss, without
claiming a crash literally inside a market-worker instruction.

## Existing mechanisms and precise insertion points

Sources: [pipeline design §3.2/§5](design-exec-pipeline-2026-08-20.md),
[exec_pipeline.rs](../../crates/torus-consensus/src/exec_pipeline.rs),
[app.rs](../../crates/torus-consensus/src/app.rs), and
[overlay backend](../../crates/torus-state/src/backend.rs).

| Point | Proposed qualification action |
|---|---|
| `ExecutionContext::attach_flush_worker`, called after `replay_committed` | Create an optional shared qualification controller only for an explicitly armed validator/run. Unset configuration follows the existing path. Never attach the hook during replay. |
| `worker_loop`, after `rx.recv()` and before `run_job`/trie-cache locks | For the selected **Flush** job N, verify/read the durable marker, publish `W_PARKED`, then hold W before any state/root/marker write. Reuse the existing gate's placement, not its unbounded received-height vector or failure injection as production controls. |
| `execute_committed_block_with`, immediately around `NativeStateOverlay::with_parent` | Capture the actual `parent_height`, child height/hash and fast-path eligibility. Require parent N and child N+1, with both headers/bodies persisted and no serial barrier. Read the overlay marker before child writes obscure it. |
| Native execution reads | Capture a qualification-only witness of an actual pending-parent read; see below. |
| After engine success, `save_order_books`, and resident-holder release; before child marker insertion/`freeze`/`pipeline_handoff` | Require W still parked on N and the direct DB marker still N−1. Publish `E_COMPUTED`/`READY`. The rendezvous prevents the subsequent handoff of N+1 from completing while W remains parked. |

Both N and N+1 must be native fast-path blocks. An empty successor, epoch
boundary, EVM work, slashes, missing durable rows or resident rebuild barrier
does not qualify; it must time out without producing READY. Selecting an
explicit height in a controlled fixture is simpler than silently retrying an
unbounded number of candidate windows.
Select N≥2 and require a successfully decoded persisted marker N−1; absent,
malformed or failed marker reads are invalid evidence, never an invented zero.

## What the evidence proves

A layering witness consists of `parent_height=N`, overlay marker N, direct DB
marker N−1, and successful N+1 computation while W remains parked. It proves
which parent was attached and visible. It does **not**, alone, prove that an
economic or nonce decision depended on bytes available only in that parent.

For the stronger claim, require a controlled parent-only nonce/balance dependency
and an actual read witness. For example, a qualification-only observer at the
parent-hit branch of `NativeStateOverlay::get_cf_raw` can record a selected CF/key
hash and value hash, together with a direct-DB comparison showing absent or
different bytes. Exclude the applied-height marker and diagnostic probe reads;
tie the witness to the real nonce guard or engine lookup during N+1. Keep the
observer optional, bounded to one selected key, and outside consensus state.
The workload must make that read occur; merely probing a known pending value
after compute is insufficient. Nonces prove replay-guard dependency; balance
reads additionally exercise economic-state dependency. Report which was tested.

`flush_worker_depth` cannot substitute for these events. `submit` increments
`outstanding` **before** the rendezvous send, so the gauge can be 2 with N on W
and N+1 blocked waiting for reception. It is not a count of received batches.

The existing `exec_pipeline_crash_before_write_replays_both_and_matches_serial`
test parks a **Marker** job, infers child progress after a sleep, injects a write
failure and creates a fresh context. That covers valuable replay/fail-stop
behavior, but supplies neither a real SIGKILL nor the native-parent witness above.

## Bounded control and crash/restart protocol

Use a fresh run identifier, boot identifier, PID plus process-start identity,
validator identity, selected N, and canonical N/N+1 block hashes on every event.
Write small complete evidence records outside RocksDB; reject pre-existing run
artifacts. A shared one-shot state machine is the authority, not log ordering.
It must be impossible to emit READY for a different job, parent or process.

Configure only the killed validator, while recording the deliberate hook-only
configuration difference. Survivors retain the normal pipeline path. The hook
must never change transaction selection, block contents, writes or replay order.
Do not hold resident-book/trie mutexes while publishing evidence or waiting.

Bound arming/parking with a qualification deadline. On expiry or malformed state,
emit INVALID and latch fail-stop/terminate W **without writing N**. Never silently
release W and then let stale READY evidence authorize a later kill. Publication
failure must also invalidate qualification. Integrate the hook with worker panic
handling and shutdown so teardown cannot hang indefinitely.

The harness waits for the matching READY record and parent-read witness, checks
the existing devnet PID guard, then sends SIGKILL to that exact process. It records
the kill identity/time and restarts the same data directory with the hook
explicitly disabled. Reusing the environment unchanged risks parking on restart.

Acceptance requires all matching pre-kill evidence, no timeout/release event,
boot replay beginning at applied=N−1 with committed height at least N+1, replay
completion through N+1 before worker attachment, and final drained state/digest
agreement with survivors at a valid comparison point. The final committed tip
may exceed N+1 because consensus can continue while E is blocked. Trade-history
rows may already exist; deterministic replay must overwrite them consistently.
Missing evidence is UNVERIFIED, never inferred success. This induced-stall run
is correctness qualification, not a throughput promotion result.

## Remaining gate

Before implementing: review the one-shot controller and selected read witness.
Then test default-off equivalence, wrong-height/marker/parent rejection, actual
read versus diagnostic-read discrimination, timeout without write, teardown,
stale-event rejection and disabled-on-restart behavior. Finally run a bounded
subprocess SIGKILL/restart fixture and the three-validator qualification cell.
Compare replayed state with a serial reference as well as surviving validators.
The forthcoming timing-only positive-gap cell does not replace this gate or
justify changing the pipeline default.
