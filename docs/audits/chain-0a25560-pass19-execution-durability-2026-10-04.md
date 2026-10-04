# Pass 19 — execution durability and flush pipeline

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. This is the third of five
additional focused passes requested after pass 16. It examines commit-time
block persistence, native/EVM execution flushes, the asynchronous flush worker,
applied-height markers, and replay after interrupted execution.

**Result: no new finding is promoted in this pass.** The checked flush paths
keep native state and its applied marker in one RocksDB batch; EVM-only state
and its marker share a batch, and worker failure latches execution fail-stop.
This static pass does not prove crash consistency for every RocksDB failure
mode or process termination point.

## Review notes

- [`persist_committed_block_durably`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L939)
  writes the committed header/body at dispatch preparation, before execution is
  handed to the pipeline. Commit manifests are recorded earlier and retained
  for parked holes; once the full body is durable the manifest can be pruned.
  Re-delivered blocks below the execution frontier are skipped before repeating
  those durable side effects.
- Native execution folds the applied-height marker into the same state/trie
  flush batch through [`flush_with_sidecar_native_trie_stats`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-state/src/backend.rs#L900).
  EVM-only blocks carry their EVM batch and marker in a single flush. When a
  native batch fails but an EVM section was already executed, the code latches
  fail-stop rather than continuing on a base that lacks those EVM writes.
- [`FlushWorker`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/exec_pipeline.rs#L1)
  uses a zero-capacity rendezvous channel, so jobs are received in order and
  only one pending overlay can sit ahead of durable state. On a worker write
  error or panic, the shared failure latch prevents the next block from being
  written on that incomplete base. Pipeline barriers cover serial paths that
  need settled direct DB visibility.
- [`replay_gap`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L1277)
  re-executes every committed height after the durable marker, and parks on a
  missing non-empty body. The pipeline is attached only after boot replay and
  seeds its logical and durable frontiers from the resulting marker. The
  pass-18 report separately covers validation and strict-order queue behavior.
- The flush worker's post-native EVM mirror resync logs a failure and proceeds;
  this is intended as a derived incremental-index repair path and was not shown
  here to alter canonical state. This pass did not run a forced resync failure
  against queries or restart recovery, so it makes no claim about operational
  impact beyond the inspected code path.

Earlier execution and crash-recovery reports retain their finding numbers and
limits. This pass did not confirm a distinct write-order defect and does not
change their issue count.

## Limits

No Rust tests, forced write failure, process kill, database reopen, or
multi-validator agreement run was performed. Existing regression tests and
source were inspected. No source changes or Torus issue writes were made.
