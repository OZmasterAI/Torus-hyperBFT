# Reusable action-hash preimages

Candidate based on record runtime `080c4fa` plus cancellation commits `96063cc`
and `6a51413`. It preserves the canonical action encoder and exposes an append
operation for caller-owned buffers. `canonical_bytes` keeps its allocating API.
`compute_action_hash_with_scratch` clears the supplied buffer, appends the same
action, nonce and domain-separated signature bytes, then makes one Keccak call.
The ordinary `compute_action_hash` remains an allocating wrapper.

`NativeDaStore::put_batch`, `put_shards_batch` and `CompactBlock::from_block`
reuse one local preimage buffer across their action loops. Once capacity is
sufficient, subsequent hashes need no new preimage allocation. Capacity lasts
only for that call, so a large action does not establish a process-wide retained
buffer. No tiny incremental Keccak updates are introduced. Body serialization,
atomic durable writes, arrival notifications, shard encoding/custody calls and
their order are unchanged. There is no trusted-hash input or signature-verification
bypass; `verified_cache_key` keeps its distinct format and session exclusion.

Correctness fixtures retain independent copies of the prior action, order and
signed-hash encoders. They cover all 26 action tags, all nested proposal actions,
order types/time-in-force, optional fields, numeric limits, Unicode and empty
variable fields, both signature variants and nonce extremes. Append-prefix and
small→large→small reuse tests check exact bytes, hashes, capacity and backing
pointer. Compact-block bytes are compared against old-encoder hash ordering.
A real-StateDb test compares mixed-size batched bodies/shards with independent
single-action writes, then reopens the database and checks stored bytes and shard
proofs. The root scheduler records current qualification in its session receipt.

This is an allocation-lifetime change, with a source-level argument and
correctness checks. No benchmark or throughput result applies, and no performance
gain is claimed. Existing canonical length casts, action semantics and serde
wire encoding are unchanged.
