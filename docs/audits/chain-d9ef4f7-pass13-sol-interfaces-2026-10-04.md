# Pass 13 — ordinary EVM, RPC, client and operations interfaces

Reviewed 2026-10-04 against source `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`, read from
`/home/oz/projects/Torus-hyperBFT`. This report is written in the separate
`audit/chain-findings-2026-10-04` documentation worktree. All source links are
pinned to the reviewed revision; relative report links retain earlier provenance.

**Result: one additional P3 source-supported candidate, F46, concerning the
EVM mempool gauge and advertised backlog warning.** Significant existing
interface roots remain present. No earlier finding is closed. Under the
[audit convention](README.md), this is static evidence awaiting a failing
production-code regression, not a runtime-confirmed incident or verified fix.

The audit README, September Astra rounds and progress, October main reports
through pass 12, and the detailed RPC/client, compatibility, history, operations
and feeder reports were checked for deduplication. F01–F45 and their established
qualifications are not counted again. The production diff from `d52a33f` was
inspected for orientation, but the review traversed interfaces across the project
rather than restricting its scope to that diff. No applicable `AGENTS.md` was
found in either repository or the containing directories checked.

Neither `cargo`, `rustc` nor `promtool` was available on PATH. Tests below were
**read, not run**. No source edits, Git mutations, installs, service starts,
exchange requests, live-chain calls, key actions or Torus writes occurred.
The incomplete pass-5 certificate, malformed-input and adversarial work was
not resumed. Only this report was written by this reviewer.

## F46 — P3: the EVM pool gauge stays zero and suppresses the backlog warning

**Ordinary preconditions.** A shipped node has initialized metrics and its
mempool, and an ordinarily funded sender submits a valid supported EVM
transaction with the correct chain ID, acceptable fee and nonce, and sufficient
gas. Inspect the gauge while the admitted transaction remains pooled, before
proposal drain. No invalid transaction, nonce gap, abandoned proposal,
concurrency race, storage error or execution failure is needed. The monitoring
consequence additionally assumes that Prometheus scrapes this node and loads
the shipped rule, without a custom producer or replacement recording rule.

**Producer trace.** The exporter [constructs and registers a default
Gauge](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-telemetry/src/lib.rs#L833) as
`torus_mempool_evm_size`, described as “Number of pending EVM transactions.”
The [node attaches the shared Metrics handle](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L656)
to its live pool. However, [set_metrics](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L376)
initializes only native occupancy, and its
[publisher](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L388) updates only
`mempool_native_size`. The successful
[EVM insertion path](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L424) updates retained
bytes but never publishes EVM occupancy. Neither
[drain](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L468) nor
[reinsertion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L451) updates that gauge.
The [actual pool-size accessor](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L503) reads
the pool directly and can correctly return one while the registered gauge
remains zero. [Encoding](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-telemetry/src/lib.rs#L2271) serializes
the registry; it does not sample the pool.

A whole `crates`/`tools` Rust-source search for `mempool_evm_size` found only
its field declaration, local default construction, registration and final
Metrics struct assembly, all in telemetry. No production or test updater was
found. This is a bounded repository caller search, supported by the inspected
live mutation paths, not a claim about external applications that hold this
public gauge. Native occupancy has a real writer and is a counterexample to
a blanket claim that both pool gauges are unwired.

**Source-derived counterexample and consequence.** Attach real metrics, admit
one valid funded EVM envelope, and leave it pooled. Actual EVM size becomes one;
`torus_mempool_evm_size` still encodes zero. The shipped
[Mempool Sizes Over Time panel](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/monitoring/dashboards/execution.json#L204)
selects that exact gauge, so it displays an empty EVM pool despite queued work.
The [MempoolBacklog expression](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/monitoring/alerts/consensus.yml#L57) is:

```promql
(torus_mempool_evm_size + torus_mempool_native_size) > 0
and increase(torus_mempool_evm_size[10m]) > 0
```

With the default EVM gauge permanently zero, the right-hand comparison cannot
be true, regardless of real EVM/native growth. The warning cannot accumulate
its `for: 10m` condition. Even if the producer is repaired, an ordinary native
backlog growing while EVM occupancy stays zero still fails this conjunct. The
[alert summary](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/monitoring/alerts/README.md#L132) advertises “mempool
growing 10+ min,” and the rule description speaks about the combined mempool,
without an EVM-only qualification. This is a concrete exporter-to-consumer
contract failure, beyond merely preferring a different panel.

The impact is incorrect EVM occupancy display and loss of this specific backlog
warning. Pool admission, selection, byte accounting and native occupancy still
operate; other stall alerts and logs can expose trouble. No actual sustained
backlog, Prometheus rule evaluation, missing notification or exhaustion was
observed. P3 reflects operational visibility, not fund loss or consensus safety.
This is distinct from [F43's timeout sample-name mismatch](chain-d52a33f-pass10-sol-operations-2026-10-04.md):
F46's gauge sample exists under the selected name but has no live producer,
and the backlog predicate separately excludes native-only growth. Both aspects
are grouped here as one advertised backlog-monitoring contract candidate.

**Actual tests and focused regression.** The
[existing EVM insertion test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L2433) funds a
sender, inserts an envelope and asserts `evm_pool_size`; it does not attach
metrics or compare the exported gauge. The
[native initialization test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L1569) does
attach metrics after admission and asserts native gauge values one then zero
after drain. Its [lifecycle companion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L1585)
checks native admission/drain/reinsert/commit occupancy; these useful assertions
cannot establish EVM occupancy. The
[telemetry encoding assertion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-telemetry/src/lib.rs#L2503)
manually sets native size to three and checks the encoded sample. No EVM-size
producer regression or MempoolBacklog rule fixture was located.

Use the real EVM pool fixture with attached real Metrics. Require admission to
change both actual size and the exact encoded EVM sample from zero to one,
replacement to retain one, insertion of a second valid nonce to yield two,
and drain/reinsertion to yield zero/two. Also attach metrics after existing
admission and require correct initial occupancy; a rejected transaction must
leave the gauge unchanged. Publish size while holding the same EVM lock as
mutation, following the native publisher's ordering safeguard, so stale captured
values cannot overwrite newer values.

Then evaluate the actual rule with exported-series fixtures: ordinary EVM
growth, native-only growth with EVM zero, no backlog and stable occupancy.
Specify whether growth or sustained nonempty occupancy is the intended warning
policy and use a gauge-appropriate expression. Require the intended warning
only after its hold duration, preserving per-instance labels. Repairing only
the EVM producer leaves the native-only predicate limitation. No test or repair
was implemented here.

## Significant prior interface roots revalidated at the pinned source

| Existing root | Current evidence, ordinary fixture and limit |
| --- | --- |
| F37: EVM open orders use an unpopulated index | The [reader](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/precompiles.rs#L545) still scans `CF_NATIVE_ORDERS`; [Classic persistence](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L3747) writes the actual book CF, and [row persistence](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L4020) uses book/order-row CFs. Whole-repository `write_stored_order`/CF searches found no production population of the reader index. A settled valid noncrossing GTC order, aligned with effective tick/lot settings, can appear in [RPC open orders](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/torus.rs#L1738) while the EVM selector returns four empty arrays. The [precompile test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/tests/precompile_tests.rs#L252) manually seeds this separate index and checks response length, not production order identity. Retain [pass 8's exact selector/layout regression](chain-d52a33f-pass8-astra-compatibility-2026-10-04.md). |
| F41: U256 Quantity padding | [hex_u256](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/types.rs#L21) drops whole zero bytes, preserving a leading zero nibble for ordinary values such as one (`0x01`) and fifteen (`0x0f`). Zero's explicit `0x0` branch and fixed-width DATA/address/hash encodings are counterexamples to a blanket hex-encoding defect. [Balance](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L618) reaches the helper for real account values. Preserve [pass 10's exact-string Quantity assertions](chain-d52a33f-pass10-sol-rpc-2026-10-04.md). |
| F42: simulation loses a valid access list | [CallRequest](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/types.rs#L222) still has no access-list field; [environment construction](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L255) cannot preserve a supplied valid nonempty list. Actual decoded [type-2 envelopes](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/decode.rs#L96) do copy it, so response/envelope handling does not refute the request-path defect. The [compliance assertion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/tests/eth_compliance_tests.rs#L238) serializes a manually built RpcTransaction and asserts an array of length one; it neither deserializes nor executes a call request. The 21,001 lower-bound result and ignored request gas ceiling were already documented; they receive no new ID. |
| F17/F18: transaction and contract-code fidelity | The [type-2 TxEnv arm](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/decode.rs#L96) still ends in default fields without explicitly preserving the envelope type. [Code persistence](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/committer.rs#L83) still writes `bytecode.bytes()`. These retain the earlier source-backed roots and dependency qualifications; this review does not rerun dependency experiments or claim the RPC projection alone repairs actual execution/accounting. See [pass 6's code and fee test boundaries](chain-cea1254-pass6-correctness-review-2026-10-04.md). |
| F32/F44: first-party wallet contracts | [Validator formatting](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/commands/query.rs#L63) still expects integer power and snake-case commission, while [RPC projection](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/torus.rs#L1284) emits hex power and camelCase commission. [Send](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/commands/transfer.rs#L35) still uses the client's [latest-only nonce request](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/rpc.rs#L72), so a second invocation while the first remains pooled can reuse its nonce. JSON output preserves the original validator object; completed-first-send and explicitly pending nonce callers limit these claims. Retain [pass 11's radix and sequential-send regressions](chain-d52a33f-pass11-sol-client-2026-10-04.md). |
| F26 and historical explorer envelope/candle projection | [Same-hash skip](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L71) still precedes descendant recovery. [Body fetch](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L93) can be swallowed after insertion, and [cursor advancement](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L157) can make missing children durable in the explorer. [Native action parsing](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L362) still selects an outer envelope key instead of the inner action tag. Historical native action count and asynchronous candle persistence qualifications remain [prior surface/history findings](chain-d52a33f-pass9-sol-history-2026-10-04.md). The source does reconcile clean gaps on later heads, so do not state that all missed heights lack reconciliation. |

## Scope matrix, established safeguards and rejected leads

| Surface inspected | Conclusion and remaining boundary |
| --- | --- |
| Ethereum executed projection | [eth_head](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L176) caps the applied marker by committed height. [Block construction](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L447) follows receipt-bearing transactions; [committer indexing](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/committer.rs#L127) follows original receipt body positions. The dense Ethereum versus body index distinction is intentional. The existing [real bridge/RPC view fixture](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/lib.rs#L3200) covers success/skipped/revert and applied visibility but manually installs application metadata, so it is not a full live admission-to-application proof. |
| Simulation and state selectors | [Call/estimate](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L965) reject resolved heights different from applied head; the [executor](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/src/executor.rs#L188) disables base-fee and nonce checks for calls. [Bare-call test](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/tests/eth_compliance_tests.rs#L85) seeds a nonzero-base-fee applied header. Historical account selectors still resolve then read live state, and multi-probe simulation lacks a pinned snapshot; these remain September/prior qualifications. A lower configured genesis gas limit did not establish an ordinary lower applied-header limit, as already explained in pass 10. |
| Ordinary reader ABI and units | Position sign extension, short signed size, eight-decimal native values versus EVM wei, aggregate age and the explicitly preserved block-number timestamp field retain [pass 8's mappings](chain-d52a33f-pass8-astra-compatibility-2026-10-04.md). [getOrderBook](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/precompiles.rs#L422) documents its deliberate Classic legacy-decoder behavior and row-layout readers; it is not counted again as a new silent migration defect. No novel ordinary tuple-layout or denomination mismatch was established. |
| Native/EVM delayed writes | [Lockbox contract](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-core/src/precompiles.rs#L1058) describes scaled units, deposit dust and next-block queue acknowledgement. [Provider restrictions](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/src/precompile_provider.rs#L126) deny writers in simulation and nonplain/static calls. This intentional denial is not promoted as an estimate compatibility defect. [Block journal](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-evm/src/executor.rs#L267) buffers writer effects for atomic commit with the EVM bundle. Queue success is not native business success; F22's handle mismatch, F23's stop-code behavior and historical drain-result handling retain earlier provenance. No blocked adversarial rollback analysis was resumed. |
| Subscriptions | [Logs subscription](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/eth.rs#L1251) consumes a public notifier channel, but repository caller search still found no shipped production log publisher. F33 remains. The commit-time head / execution-time head quiet-tail explorer race and native stream lag/recovery limits remain prior reports. No live delivery was tested and no external embedding application's notifier policy is inferred. |
| Explorer validators | [snapshot_validators](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L209) correctly consumes hex power and camelCase commission from the current RPC; this is a counterexample to extending F32's wallet formatting defect to the explorer. Backfill labels those current validator responses with the historical indexing height, because [the client](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/rpc_client.rs#L48) has no historical selector. Keep this as a provenance limitation when interpreting snapshot heights, not a new P2: shipped public queries return latest cached validator snapshots, and no historical-validator API guarantee or distinct material execution consequence was established. |
| Feeder integration | [Node paging/parsing](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/price-feeder/src/node.rs#L47), signer/startup checks, per-cycle listing refresh, bounded submissions and monotonic nonce generation retain [pass 12's actual coverage](chain-d52a33f-pass12-sol-feeder-2026-10-04.md). Partial-admission health and more-than-four-chunk coverage remain conditional qualifications. The real-RPC feeder fixture tests admission, not final oracle quorum; recorded parser fixtures do not establish current exchange API compatibility. No venue request was made. |
| Configuration and observability | F35 CLI-default precedence, F36 overwrite behavior, F38 archive pruning metadata and F43 timeout-series mismatch remain [prior operations/recheck findings](chain-d52a33f-pass10-astra-recheck-2026-10-04.md). Telemetry bind-result, separate Compose-network, printed devnet port and static liveness/net-peer limits retain their prior qualifications. This review adds F46 only; it does not certify every metric or launcher. |

This bounded audit is complete at the stated revision. F46 needs a real
producer/encoding regression and a meaningful Prometheus rule fixture. Prior
interface regressions remain necessary; no Rust or rule tests passed here,
no production defect was fixed, and no exhaustive correctness certification
is claimed.
