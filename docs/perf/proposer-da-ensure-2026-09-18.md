# Proposer exact-byte DA ensure candidate

Isolated from `3cc958b` on `perf/proposal-da-ensure`. Experimental flag
`TORUS_PROPOSAL_DA_ENSURE=1`; unset, `0`, or other values keep the existing
unconditional proposer mirror. The flag is resolved once per app and logged at
startup in both states. This supports a same-binary A/B after integration with
the recovery baseline. No improvement or acceptance is claimed.

The existing proposal selection flushes buffered ingress mirrors, but the flush
returns no success result and another flusher may already own the pending batch.
Neither successful selection nor an empty pending buffer proves persistence.
The proposer therefore retains an explicit durability operation.

With the flag on, `NativeDaStore::ensure_present_batch` computes each selected
action's content hash locally, serializes the signed body and compares pinned
stored bytes against those exact bytes. Only an exact match skips the write.
Missing, undecodable, and valid-but-wrong stored bodies are written using the
locally supplied body. A read failure writes every body in the affected chunk;
a write failure requeues the full selected batch and causes an empty-native
proposal, as before. Serialization/CF errors also fail closed. No advertised
hash, mempool presence or signature cache is accepted as persistence evidence.

Reads use RocksDB's existing `batched_multi_get_cf` API in chunks of at most 32
keys and 1 MiB of expected encoding plus one body. Stored values use pinned
handles rather than extra Rust-owned payload vectors. The number of pinned rows
is bounded, but a corrupted oversized stored value can exceed the expected-byte
budget. The accumulated missing-body WriteBatch is bounded by the selected
payload, matching the baseline batch's bound, and all missing/repair writes are
atomic in one batch. An all-hit batch does not write or notify. A successful
nonempty write wakes arrival waiters once; failed writes never notify.

Ingress races are safe under the current monotone store: an incomplete ingress
write produces a miss and our own write, while a completed identical write is
reused. A concurrent writer landing after our miss merely causes an idempotent
duplicate write. Production currently has no DA-delete caller; commit-pruning
only removes in-memory pool entries. Future concurrent DA pruning must coordinate
with the read-before-write operation so positive reads remain valid for pending
proposals. This optimization does not add a stale in-memory durability cache.

Writes retain the existing ordinary RocksDB WAL class. There is no new per-write
fsync or stronger power-loss claim; the existing commit WAL policy still applies.
Full-block validation, ingress, recovery mirroring, shard custody and proposal
wire encoding are unchanged.

Flag-on proposals emit one compact `proposer native DA ensure complete` log with
checked/written body counts, logical compared/written bytes, read-fallback chunk
count and elapsed microseconds. These bytes exclude keys, WAL framing and actual
device IO; failed read chunks are not counted as compared bytes. The existing
block-build histogram remains the overall proposal timing. There are no raw
payload logs. The experiment must record the flag in its environment manifest.

The hypothesis trades duplicate WAL/memtable writes and downstream compaction
for pinned reads and byte comparisons. It still hashes and serializes every
selected action. Miss-heavy or cold-cache workloads may regress; same-shape live
A/Bs, liveness/dissemination acceptance and matching throughput remain required.

Added tests cover exact all-hit/no-write, mixed missing/junk/wrong-body repair,
reopen and explicit removal, bounded read chunks, injected read-error fallback,
write-error propagation and notification ordering, a stale miss racing ingress,
an ingress batch taken but not written, real read-only DB requeue, and proposer
flag-off unconditional writes versus flag-on reuse and failure-driven empty
selection. Existing native DA and proposer durability tests remain relevant.

At handoff, only Rust parsing via `rustfmt --emit stdout` and `git diff --check`
have run. No Cargo build, tests, benchmark or commit was run by this agent. The
parent coordinates these commands after the active cell:

```sh
cargo test -p torus-state --lib ensure_present -- --test-threads=1
cargo test -p torus-state --test native_da_tests
cargo test -p torus-mempool --lib ensure_native_da -- --test-threads=1
cargo test -p torus-consensus --lib proposal_da_ensure -- --test-threads=1
cargo test -p torus-consensus --lib produce_block_bodies_durable -- --test-threads=1
```
