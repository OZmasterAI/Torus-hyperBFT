# Record-derived load improvements (implementation session)

Status: unmeasured candidates. No throughput benchmark, microbenchmark, or devnet
load run was performed in this session. Correctness checks do not establish a
matched/s or block-time improvement.

The integration branch is `perf/s61-load-improvements`, in
`/home/18c/projects/wt/s61-load-improvements`, based on `080c4fa`.
That runtime produced the accepted 41,723.7 fills/s over 306 seconds in the prior
campaign. Its frozen record binary remains intact. The separate 51,608.1 fills/s
record used `388f7cd` and a 127-second window; those durations are not comparable.
The previous campaign report lives on `perf/matched-200k-next` at `c7c8542`.

The user prohibited new benchmarks and set a hard stop of September 19, 2026,
12:45 Berlin (10:45 UTC), superseding the initial approximate deadline.
Only root schedules builds/checks; agents edit isolated worktrees. No node or
load generator is running. Account remaining percentage is not exposed.

## Implemented changes

| Area | Change | Integration commit |
| --- | --- | --- |
| Raw network frames | Read directly into the final payload Vec and write the original payload behind a stack header; retain v1 bytes, caps, error kinds and consumption boundaries | `b3f1c7e` |
| Proposal hashes | Immutable pending bodies lazily retain ordered signed-action hashes for compact encoding, selection and warm commit matching | `984e6ef` |
| Native fanout | Share one immutable Vec allocation across peers, reconnect retention, bounded scheduler queues and retries; inbound/small direct payloads remain owned | `7865952` |
| Compact encoding | Borrow header/hash/EVM/core-writer fields during serialization instead of cloning them | `1e26d30` |
| Cancellation | Stable two-ended compaction or directional removals for eligible concentrated deep-book batches; preserve return order and all bookkeeping | `f794cad` |
| Mempool expiry | Maintain an exact nonce-ordered index and visit only expired entries instead of scanning the entire live pool under its write lock | `9e9800e` |
| Proposal bodies | Reuse the body vector already prepared for durable DA mirroring instead of cloning selected bodies a second time | `c250a5d` |
| Push decision | Use exact serialized size before choosing hash-only transport; avoid building a body buffer that will be discarded | `0bfef34` |
| Cancel margin | Port the previously reviewed exact integer-leverage arithmetic and per-market configuration lookup, with current deep-book accounting coverage | `3a01d08` |
| Push envelopes | Serialize directly into final marker-prefixed buffers; remove full-body copying on the swarm command path | `cdd9871` |
| Ban persistence | Replace overlapping detached writes with a lazy serial worker, one replaceable pending snapshot, atomic file replacement and shutdown drain | `d3f667f` |
| Attestation | Reuse a call-local serialization buffer across signed actions while preserving the original digest and signature bytes | `8b5d37f` |
| Canonical hashes | Reuse canonical signed-preimage buffers in DA batches and compact blocks; retain allocating APIs and exact bytes | `a488a00` |
| Proposal hash loops | Extend scratch reuse to lazy proposal hashes, cold commit comparisons, commit pruning and hash-only network pushes | `1364d74` |
| DA reads | Decode native batches from RocksDB pinned values, avoiding temporary full-body Vec copies and preserving input order | `fe83745` |
| DA writes | Reuse batch-local bincode buffers for body and shard custody writes, omitting per-action allocations and size walks | `c1e32b7` |
| DA serving | Fetch raw response bodies in bounded 16-hash batches, retaining input slots and independent-read fallback on storage errors | `778f059` |
| RPC acknowledgements | Reuse task-local JSON scratch and retain only parsed actions and acknowledgement hashes after verification | `8a34358` |
| Executor allocation | Pre-size flattened actions and place-order indices from valid batch lengths; preserve execution and skip order | `90e04d2` |
| Trade writer | Accept packed byte-arena batches alongside the old raw-row API, preserving queue and write-chunk behavior | `e4cf9a4` |
| Trade history | Build fixed-size rows and use packed production handoff instead of six small key/value allocations per fill | `5582366` |
| Reserve arithmetic | Extend exact raw integer-leverage division to per-order reservation/release and single cancellation, retaining old rounding and panic order | `796a70e` |

The cancellation implementation and eligibility proof are in
[cancel-batch-2026-09-19.md](cancel-batch-2026-09-19.md). The integer arithmetic
proof and mutation-boundary qualification are in
[cancel-all-integer-margin-2026-09-18.md](cancel-all-integer-margin-2026-09-18.md).
The latter is a port of `f2a7164`, not a new arithmetic discovery.

## Correctness evidence so far

Counts below describe separate checks and include overlap; do not add them into
one unique-test total. Ignored timing probes remained ignored.

- Consensus hash-cache qualification: 152 passed, one ignored. Receipt
  `d47798ae-1839-41f3-b927-a445a3e52a24` in the block-agent scope.
- Borrowed compact encoding: eight targeted tests passed, including the existing
  stale-smaller-body test. Receipt `f78e48ec-a756-420d-8994-741e9353c6f5`.
- Core cancellation: 118 library, five determinism, three fuzz and 12 level-row
  tests passed; three ignored. Receipt `f522dbbf-2146-4fdc-a7a3-7dee17dbe654`.
  After adding the conservative admission guard, all ten cancellation tests
  passed (`81c5a6d9-60ca-4217-a38b-341b2cedec8d`).
- Cancellation plus integer margin: 48 arithmetic/accounting/parallel-matching/
  persistence/level-row tests passed, including a 2,048-deep book with 80 canceled
  targets. Receipt `51cf3ffb-da39-4bc6-95eb-a50c4eb88332`.
- Mempool: 85 tests passed, including the old full-scan expiry oracle, saturated
  nonce boundaries and 2,000 deterministic mixed mutation steps. Receipt
  `fe586dd7-c8d4-4385-b4a3-f44695e9ab09`.
- Proposal body reuse: four targeted tests passed, including an actual read-only
  DA-store failure proving empty proposal outputs and retained retry work.
  Receipt `1af26c7f-41b8-4cd0-89fb-bae1deb691a0`.
- Latest integrated network and attestation check: 137 network tests and six
  attestation tests passed. Only the compression-ratio probe was skipped.
  Receipt `eeeec573-6f30-4070-bf96-f932f222f8e1` in the integration scope.
- Canonical hash scratch: 62 type tests and nine DA tests passed, one timing
  probe ignored. Independent frozen encoders cover all 26 tags, both signature
  forms and nonce extremes. Receipt `0dbd3d68-e0c6-45fc-ae25-96d509015b27`.
- Integrated proposal scratch loops: 155 consensus tests passed, one ignored,
  and all 12 network bridge tests passed. Receipt
  `aacd81ae-63ad-4b2d-9cd8-72778dced126`.
- Pinned DA reads: ten DA tests and the mempool buffered-read test passed,
  including unsorted duplicates, missing bodies, large batches, memtable/SST
  reads, compaction, reopen, corruption and deletion. Receipt
  `df59b192-42e6-42d7-a830-f6c5a1cf1cea`.
- DA serialization scratch: nine tests passed against an independent legacy
  shard serializer and unchanged single-body writer. Receipt
  `3851ba4b-e302-4aa1-9d8a-6d955a466d66` in the cancellation-agent scope.
- DA serving: five network DA checks, including a >4 MiB codec/reconstruction
  fixture, and ten integrated state DA tests passed. Receipt
  `5137a30a-4421-4bf2-a793-0234e8574026`. Aggregate-error fallback was reviewed
  in source; no storage I/O failure was injected.
- RPC: all 62 correctness tests passed, including old JSON hash/error parity,
  both signature forms and actual JSON/binary endpoints. The existing unignored
  `verify_breakdown_by_batch_size` timing probe was explicitly skipped. Receipt
  `7f4b1d4c-9fed-4c66-bae1-33fd90c86330`.
- Executor pre-sizing: all 19 matching tests passed, including batch-cap and
  hand-flattened equivalence cases. Receipt `b15f3400-3d24-4cd7-9a34-58a0efa033ab`.
- Packed history: 12 writer tests passed (`9b8bf7d2-62d6-4f0c-8925-970b5e62f006`);
  the encoder oracle, eight settlement, three deferred-trade and one consensus
  writer test passed (`b20a69ca-d657-49f3-9378-b029ee6e09ac`). Details and memory
  tradeoffs: [packed-trade-history-2026-09-19.md](packed-trade-history-2026-09-19.md).
- Reserve arithmetic extension: four arithmetic differential tests and 38
  maker-accounting/matching/settlement tests passed. Receipt
  `86ddf6b9-2147-45e7-a3dc-587d5dabfb70`. The original i256 formula remains
  the test oracle; this is exact SCALE cancellation for a u32 leverage divisor,
  with nonpositive quantity/price gates and zero-divisor panic behavior retained.

The initial broad network run exposed the existing ban-file race: 128 tests
passed and `ban_list_persistence` failed. That issue is now fixed and the latest
integrated suite includes it. Three body-reuse fixtures initially used dummy
signatures and one attestation fixture omitted a StopLimit field; those test
fixtures were corrected before successful qualification.

A shared-target stale artifact was positively detected: receipt
`789cffc7-816b-4f72-8228-22962ff03c85` reported 11 old peer-scoring tests and omitted
all four new tests. **That receipt is invalid evidence and was not used to resolve
the issue.** After cleaning only `torus-network`, compilation from the correct
worktree was observed and all 15 expected tests passed under valid receipt
`64fa5479-3325-4f8c-8a59-26c29e9ae4b2`. The integrated 137-test run then compiled the
network crate again and passed. Check names/counts and actual compilation when
switching worktrees; a passing source fingerprint alone cannot detect stale
external build artifacts.

## Limits and remaining qualification

Pending hashes add 32 bytes per cached action for the existing proposal lifetime.
The expiry index adds storage and logarithmic insert/remove work; its net speed
benefit is unmeasured. Cancellation admission is deliberately conservative:
32–200 trader targets, a deep first target level and concentration in a bounded
sample. Recovered above-cap books, scattered levels and shallow prefixes retain
the original path. Movement bounds are not CPU measurements, and actual workload
activation share is unknown. The release checks do not execute the debug-only
epoch-overflow panic branch.

Raw reads add a separate header read stage. Shared outbound buffers add small Arc
metadata allocations. Ban writer shutdown now waits for its final write; normal
penalties do not wait for file I/O. Atomic replacement provides complete-file
visibility, not an fsync/power-loss guarantee.

Scratch buffers retain the largest action's capacity only for the current call.
Pinned DA reads hold RocksDB value pins until each body is decoded, then release
them; the net effect on cache pressure and latency is unmeasured.

Existing experimental pipeline/async-validation defaults, genesis rules,
validation predicates, consensus header authority, DA writes and matching
semantics remain as in the record lineage. No hardware ceiling is inferred.

The integrated standalone node built successfully at `6099a68` after cleaning
the affected project packages. Compilation from the integration worktree was
observed for every affected package. Receipt
`1ed3190a-4abc-4fb9-94c4-2708809c258b`; build duration was 76.5 seconds, not a
performance measurement. No generator was built and the node was not launched.

Frozen checkpoint artifact:
`/home/18c/bench-results-matched/s61-implementation-20260919/candidate-6099a68/`.
It contains `torus-node`, `SHA256SUMS` and `manifest.json`; binary SHA256 is
`47a8d64d31c6c1186a5da9fac66eab1cf83365f7b90bd2d5790f625c54a12d0b`.
Further implementation continues; later changes need their own qualification.
A later benchmark session requires authorization; this session must not measure it.

Second integrated checkpoint: `f8f692e`, including all 22 implementation commits
listed above, built successfully after another affected-package cleanup. Receipt
`25eb4dc6-6c69-4510-9be5-aac6c9699113` also includes four integer-margin and eight
parallel-settlement tests on this combined source. Compilation from the root
worktree was observed; build duration was 73.05 seconds. The standalone node
was not launched and no generator or benchmark was built.

Frozen artifact directory:
`/home/18c/bench-results-matched/s61-implementation-20260919/candidate-f8f692e/`.
The binary is 47,999,536 bytes; SHA256 is
`bade8c32bd46103851eadb30b32429b9941f98422af8299ef019d9f99793bda4`.
This remains an unmeasured candidate, not a new record.
