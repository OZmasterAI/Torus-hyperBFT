# Producer block-build attribution

Branch `perf/build-stage-timers` starts at recovery commit `9af0eea`.
These measurements must be present on both sides of a performance comparison.
They are instrumentation, not an optimization or evidence of a throughput gain.

`torus_block_build_seconds` retains its existing boundaries: after parent-header
lookup/decode, through epoch validator-set update calculation. Observations for
the new stages occur after the existing total has been recorded. Added clock
reads still have a small instrumentation cost inside the measured work.

Every normal `produce_block` call records each stage once, including empty
payloads and disabled optional work. Execution fail-stop returns before all
these timers, as it already did for the total. Sum/count deltas should therefore
have equal counts within a common scrape window. A concurrent scrape can catch
part of the observation group; compare counts before adding stage means.

| Histogram suffix after `torus_block_build_` | Consecutive wall-time interval |
| --- | --- |
| `parent_decode_seconds` | Parent block lookup and full/compact header decode. Outside the existing build total. |
| `selection_seconds` | Timestamp/gas setup, pending-proposal exclusion hashes, native selection and EVM drain. Selection includes buffered DA flush, expiry and clone-out. |
| `mirror_seconds` | Clone selected bodies, durable whole-body mirror, and nested shard custody. |
| `attestation_seconds` | Clone selected bodies into the proposal and compute/sign the attestation. |
| `assemble_seconds` | Parent canonical-header hash, proposal structure construction and pre-proposal push enqueue. Network delivery happens asynchronously outside this interval. |
| `encode_seconds` | Compute own action hashes and encode the full/compact datum. Compact encoding currently computes native hashes again. |
| `bookkeeping_seconds` | Update/retain proposal cache, note in-flight hashes, and hash the encoded datum. |
| `epoch_seconds` | Compute validator-set updates at an epoch boundary. |

Except for parent decode, these stages partition the original build interval.
Their sum can be compared with the total using equal-count, same-window raw
sum deltas; the small residual is the end-of-interval measurement/observation
overhead. Do not add parent decode to that partition. These are wall times,
including descheduling, locking and storage waits—not CPU time.

The existing `torus_validate_block_custody_seconds` includes both producer and
validator calls. Producer custody is nested inside the new mirror stage, so
adding the old custody metric to that stage double-counts work. If mirror or
selection dominates, split that stage in a subsequent controlled experiment
(e.g. DA serialization versus RocksDB write, or exclusion hashing versus pool
selection), rather than introducing a per-action timing loop now.

`view_propose_build_seconds` measures StartView through leader self-insertion.
It also includes HotStuff preparation and block-tree insertion, and its sample
population differs from calls to `produce_block`; subtraction of unrelated
means cannot precisely attribute the difference.

The harness records every new histogram's sum/count in `sampler.csv` and emits
its mean/count in the existing whole-run and load-window consensus JSON
sections. The sampler permits additional steady-window analysis. Historical
absent series remain unmeasured. Use load and steady windows together to expose
empty-block ramp dilution.

Source hypotheses for the next experiment remain repeated pending/action
hashing, redundant durable body writes, shard encoding/write cost, repeated
serialization for attestation and compact encoding, and native pool selection.
The new timings identify which deserves the next isolated comparison.
