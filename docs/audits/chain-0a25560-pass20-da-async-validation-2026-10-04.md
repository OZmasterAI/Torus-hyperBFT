# Pass 20 — DA body custody and asynchronous validation

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. This is the fourth of five
additional focused passes requested after pass 16. It traces full and compact
proposal body handling, recovery fetches, durable custody, and speculative
validation on the network worker.

**Result: no new finding is promoted in this pass.** Cache hits remain tied to
exact datum bytes and consume the same synchronous ancestry and bookkeeping
checks as cache misses. Body misses fall through to bounded recovery or
`MissingData`; I found no new way for a missing body to be voted through.

## Review notes

- [`decode_proposal_and_ensure_durable_core`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L4820)
  mirrors inline full-block native bodies into the durable DA store and fails
  closed if that write fails. Compact blocks note their hashes as in-flight
  before reconstruction, then require the referenced native actions to be
  available. This avoids losing a body reference merely because validation
  returned `MissingData` before a pending proposal was installed.
- [`reconstruct_compact_from_da`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L4301)
  makes a batched lookup and returns the exact missing hashes. Sync recovery
  fetches missing bodies off the voting path and rechecks validation afterward.
  Fetched bytes are decoded and stored under hashes recomputed from the action,
  so a peer cannot assign a body to a different requested key. Failed DA writes
  are rechecked as misses and do not produce a valid verdict.
- The async worker has a bounded queue and bounded verdict cache. Queue
  saturation drops speculative work and uses the synchronous fallback. The
  worker does not cache negative results, so a transient miss is retried by the
  normal validation path. On a hit, [`validate_datum_after_fail_stop`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L4985)
  first checks the datum hash, consumes the exact-byte verdict once, restores
  its in-flight notes, and enters shared pending bookkeeping. The outer
  `validate_block` still checks fail-stop and parent linkage.
- Validator-set size is snapshotted for best-effort shard custody and can be
  one boundary block stale, as documented in the source. The review found no
  vote validity dependency on that shard-custody count. Shard custody is
  explicitly best-effort after a body is durably mirrored.
- This pass did not independently reproduce earlier F16's DA disk-growth
  concern or F15's pre-validation queue bounds issue; those remain distinct
  earlier candidates and are not counted again here.

Earlier DA and availability reports retain their issue numbers, revisions and
limits. No distinct additional defect is promoted by this pass.

## Limits

No Rust tests, hostile DA peer, disk-full simulation, multivalidator recovery,
or throughput measurements were run. Source and existing test assertions were
inspected. No source changes or Torus issue writes were made.
