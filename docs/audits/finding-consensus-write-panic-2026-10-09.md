# Finding: a failed consensus write panics `hotstuff-algo` and leaves a live node (2026-10-09)

Found by the R01 fault test on ozarchy (s34, `fix/r01-write-fail-stop` `01d991ab`; evidence and
harness on that branch: `docs/audits/r01-fault-test-2026-10-09.md`, `tools/r01-fault/`). Outside
R01: R01 covers the execution thread's serial state / marker flushes, and those fail-stop as
intended. Logged as its own finding (18c s106). Not fixed.

## What happens

On a disk fault (full disk or read-only fs) while execution is idle or nearly idle, the first write
to fail is usually a consensus write, not an execution flush:

1. `crates/torus-consensus/src/kv_store.rs:170`: `self.db.write(batch).expect("RocksDB write failed")`
   panics on the `hotstuff-algo` thread (`crates/hotstuff_rs/src/algorithm.rs:152`, plain
   `thread::Builder::spawn`). The native DA store batch write (`torus_mempool: native DA store batch
   write failed`) is the other first-failing write; it only logs.
2. The workspace has no `panic = "abort"` profile and the node sets no panic hook, so only that thread
   dies; the process stays up.
3. The execution loop sees its channel close, logs "shutting down" and returns without setting the
   fail-stop latch, so nothing exits with 70.
4. The node keeps answering RPC with a frozen applied height; it no longer votes or proposes. It was
   observed alive > 3.5 min; the harness killed it.

## How often

19 of 19 light-load full-disk runs (15 empty-block, 4 light-native) ended this way. Exit 70 needed a
committed block to be executing when writes started failing (heavy native load or a starved
execution thread). The natural shape of an idle or lightly loaded validator therefore hangs instead
of failing stop.

## Impact

Liveness and operability, not safety: the node stops taking part in consensus and does not apply
blocks past the fault, but it does not exit, so a supervisor (systemd `Restart=`) never restarts it,
and RPC clients read stale state from a node that looks healthy. Restart after clearing the fault
replayed and matched the peers in the R01 runs.

## Candidate fix (not built)

- Turn a `hotstuff-algo` panic into the node-wide fail-stop: a panic hook (or `catch_unwind` around
  `execute`) that sets the same latch the execution fail-stop uses and makes the binary exit 70; or
  return the `kv_store` write error to the consensus loop and stop there.
- Treat an execution-loop channel close that is not a clean shutdown as a fail-stop.
- Decide whether the native DA store write failure should also fail-stop (today: log only).
- Test: the R01 harness light-load full-disk case (`tools/r01-fault/run-case.sh`, empty blocks,
  pipeline off) must end in exit 70 instead of `ZOMBIE`.
