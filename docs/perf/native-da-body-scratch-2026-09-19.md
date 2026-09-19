# Batch-local DA body serialization

Follow-up to action-hash scratch commit `bb32ac5`. `put_batch` and
`put_shards_batch` now clear and reuse one local body `Vec` per call, using
`bincode::serialize_into`. Single-body `put` remains unchanged. Hashing still
precedes body serialization, actions and shards retain their order, and the
database receives the same one atomic batch only after every action succeeds.
Serialization/erasure errors return before that write. Body arrival notification
still occurs only after a successful body-batch write; shard writes add no wake.

The installed bincode 1.3.3 source uses fixed-int encoding for both `serialize`
and `serialize_into`. Its unlimited `serialize_into` path omits the size walk
that `serialize` performs before allocating a new buffer. The new buffer grows
as needed and retains capacity only for this call. This is a source-level work
and allocation argument, not measured latency or throughput evidence.

Ownership was checked through the installed RocksDB 0.24.0 wrapper and bundled
10.4.2 C++ implementation: `put_cf` calls `rocksdb_writebatch_put_cf`, then
`WriteBatch::Put` / `WriteBatchInternal::Put`; `PutLengthPrefixedSlice` appends
the bytes into the batch's owned `rep_`. No caller buffer is retained. The local
erasure encoder copies body slices into owned `Vec<Vec<u8>>` shards and returns
an `EncodedBody` with no borrowed lifetime. Reusing the body buffer therefore
cannot rewrite a queued value or an earlier shard.

The mixed-size DA test now compares shard output against a frozen pre-change
batch implementation that still uses allocating `bincode::serialize`, avoiding
a shared serializer path on both sides. It compares entire body/shard column
families for small→large→small actions with both signature variants and a
duplicate overwrite, reopens the database and checks raw bodies and shard proofs.
Root-scheduled qualification is recorded in the session receipt. No benchmarks,
durability-policy changes, trusted hashes or verification bypasses are introduced.
