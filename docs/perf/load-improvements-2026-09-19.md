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

The original session prohibited new benchmarks and ended before its September
19 deadline. On September 23, the user authorized completion of the three
interrupted patches, correctness checks, integration, a fresh standalone node
build, and this report. No benchmarks or load runs were authorized or launched.
**All future performance/load runs must use cap 200, never a lower cap.**
Small and zero caps in correctness fixtures remain necessary boundary tests;
they are not performance/load runs. The resumed work used sequential builds
and checks in the designated worktrees.

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

## Correctness evidence

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
This historical checkpoint predates the completed implementation below.
A later benchmark session requires authorization and must use cap 200.

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

## Usage-limit stop and later committed changes

Active work reached the usage limit around 08:12 Berlin (06:12 UTC), before the
12:45 Berlin deadline. The final already-launched trie check finished at
06:12:49 UTC. The status exchange resumed after the deadline; no further
implementation or checks were started. No build, test, node or load generator
was running when the final handoff was saved.

At the September 19 stop, committed source was `105ba3a`, containing 28
implementation commits. These six changes postdated the frozen `f8f692e` node:

| Area | Change | Commit |
| --- | --- | --- |
| State ownership | Add compatible owned-value puts; overlay moves values while preserving undo/freeze/error behavior | `bdc335d` |
| Book persistence | Move consumed Classic and order-row serialized buffers into overlay | `ee79f89` |
| Matching | Reuse occupied best-price entry through matching and remove only afterward | `2386826` |
| Balance cache | Track first-dirty senders with entry flags and a retained vector; preserve complete retry set on failure | `9cf45f7` |
| Typed persistence | Serialize positions/balances into 95/33-byte-capacity buffers and transfer ownership | `43be807` |
| Trust-cache hashing | Reuse batch-local buffers while preserving distinct EIP-712 keys and session exclusion | `105ba3a` |

Evidence: owned API 32 backend tests (`38288137-1153-4595-aca0-2c407c0f765d`);
book save 12 tests (`a1c3400a-4bca-4208-834e-d33ca8a6d623`); occupied matching
142 tests, three ignored (`81c39e98-253a-40e3-9a2a-39c3544d70cb`); balance cache
four tests (`5ba339c1-5f06-41fa-aeec-5da4318063af`). Combined root qualification
passed 183 tests: 64 types, 85 mempool, seven position-cache, eight engine,
11 maker-margin, eight settlement; one timing probe ignored. Receipt
`7e2c8946-830c-49f1-ae9f-98a4c1d5e9ba`. No performance measurements.

Ownership retains producer capacity. Order rows start at 120 bytes; Classic
book blobs may retain Borsh growth slack. Small balance/position encoders avoid
Borsh's default 1 KiB buffer. Balance flags add entry padding and an extra lookup
per dirty sender after a successful flush; net workload benefit is unmeasured.

Historical unfinished state preserved at the September 19 stop:

- Root `native_pool.rs`: one sender-count lookup per selection and zero-cap
  test. Uncommitted and untested. Issue `eb9253db-fa94-4ba3-996c-10bcbab6e216`.
- `s61-cancel-path`: trie parent-vector reuse and oracle tests. Uncommitted;
  26 tests passed, one ignored, allocation witness explicitly skipped under
  receipt `2303eab8-f46a-4b6e-8a27-7e8fa058d8cc`. Initial compile failed due to
  stale types artifacts; targeted types/state cleanup fixed the build. Torus
  resolution was rejected because its attempt already had a failure outcome;
  issue `2b3b58ab-58bb-4c39-bfeb-20b57f8e6795` remains unresolved in the tracker.
- `s61-block-path`: consensus packed write-batch candidate and tests. Interrupted
  by quota; uncommitted, unreviewed final patch, uncompiled and untested.
  Issue `34778fd5-1441-4862-bc6a-a44bc20ebd5e`.

Patch backups, including untracked test files, are in
`/home/18c/bench-results-matched/s61-implementation-20260919/stop-handoff/`.
The September 23 completion below supersedes those pending statuses. The
original backups and frozen binaries remain intact.


## September 23 completion

The integration worktree and branch were verified at the expected `105ba3a`
before edits. Both auxiliary worktrees matched the handoff. Review covered all
pending production changes and both untracked test files.

- Mempool selection: `7ef271e`. One sender-count entry lookup replaces the
  repeated lookup in each selection loop; a zero sender cap returns empty.
  Existing exclusion, byte/order budget, prefix and cancellation ordering are
  preserved. The zero-cap test now checks expiry, dedup, hash and sender indexes
  before and after re-enabling selection. All 86 mempool tests passed under
  receipt `a4a897b7-e369-4ace-8ce6-8c662294ba1d`.
- Trie parent reuse: `3a9e53a` (source-worktree commit `43ce385`). Sorted child
  indices remain sorted after division by two, so adjacent dedup produces the
  same parent sequence as the old BTreeSet. Independent legacy and full-tree
  oracles check roots, changed nodes, sibling reads and every injected read
  failure boundary. The fresh check passed 29 tests, one ignored, under receipt
  `2d2f940b-7cd5-4d02-b2c0-8cdda15499ca`; this includes three backend flush tests
  beyond the former 26-test scope. The allocation witness was explicitly
  skipped. Fresh attempt `a682081a-1a85-4f2e-b471-d3b7e70a7948` resolved
  successfully as event `4c7d467f-af05-43c7-b110-f11fb1bc6916`; the earlier
  failed attempt remains historical.

- Consensus KV writes: `d73320e` (source-worktree commit `7826d36`). Keys and
  values share an owned byte arena; ordered offsets preserve duplicate-key and
  delete/set semantics, deferred CF lookup, and one RocksDB write per batch.
  All 159 consensus tests passed, one ignored, under receipt
  `68d290ef-f131-4e12-a5c8-3468bcb1bc28`. All four new tests executed, covering
  legacy raw-CF byte equivalence, input ownership, snapshots, clones, reopen,
  abandoned batches, missing CF and actual read-only write failures. Tracker
  resolution saved as event `9998a503-2075-444c-881d-2f5ac7d21b6e`.

Affected project crates were cleaned before each worktree's check, and expected
new test names were checked. The KV receipt truncated the compilation header;
its fresh dependency file contains the new test module but uses relative paths,
so it does not independently establish the absolute compilation directory.
The integrated check below retains full logs to preserve compilation evidence.

The parent vector retains its largest level capacity until propagation returns.
The consensus byte arena can retain growth slack and copies existing bytes when
it grows; both the arena and RocksDB's batch coexist until the write completes.
These allocation changes have no measured throughput or latency benefit yet.
The new KV tests cover logical storage and injected read-only errors, not
physical power-loss or disk-full fault injection. Multi-validator performance, long-running load,
and end-to-end state parity under load remain unqualified for this candidate.


### Final integrated verification and standalone artifact

All three patches are integrated at
`d73320e445a752e7ee99bd77fdf2c458157e0f4f`, bringing the implementation count to
31 implementation commits beyond the record base, plus harness and
documentation commits.
After cleaning all 13 node project packages, the combined source passed:

| Check | Passed | Ignored |
| --- | ---: | ---: |
| `cargo test --release -p torus-mempool --lib` | 86 | 0 |
| `cargo test --release -p torus-state --lib native_trie -- --skip alloc_witness_native_trie_block` | 29 | 1 |
| `cargo test --release -p torus-consensus --lib` | 159 | 1 |

This is 274 passing tests in the final combined check; earlier checks overlap.
The allocation witness was explicitly skipped and timing probes stayed ignored.
The following standalone build then succeeded:

```sh
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo build --release -p torus-node
```

Receipt `d8210b30-b218-4151-8230-167c12f8321e`, run
`6c275922-9a56-4a0a-8624-c4865d68b3b2`, covers these checks and the build with
unchanged source. Full logs confirm all 13 node project crates compiled from
this integration worktree and every new test executed. The build emitted an
unused `state` parameter warning in RPC and a dependency future-compatibility
warning for `proc-macro-error2`; neither failed the build.

The fresh frozen artifact is:
`/home/18c/bench-results-matched/s61-implementation-20260919/candidate-d73320e/`.

- Binary: `torus-node`, 47,991,352 bytes.
- Source revision: `d73320e445a752e7ee99bd77fdf2c458157e0f4f`.
- SHA256: `fdc2fb563573b9ae413da7c26cc6a1dc1f88921e6d57be3f68e8f22b163116ec`.
- Provenance: `manifest.json`, `SHA256SUMS`, `verification.json`, full test/build
  logs, Cargo/Rust versions, and the report-only diff present during the build.

The source code exactly matches the recorded revision. The only dirty file
during verification/build was this report; the final report commit changes no
compiled source. The copied binary's checksum was checked against the build
output and `SHA256SUMS`. No generator was built, and the node was not launched.
All five requested implementation tasks are complete. Performance improvement
and multi-validator load parity remain unmeasured; any authorized future run
must use **cap 200**.
