# Pass 13 — whole-project review at the C3/PF1 integration head

Reviewed 2026-10-04. Source: `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`. Documentation destination:
`audit/chain-findings-2026-10-04`, starting at `e8d9181`.
These are separate branches and worktrees. This audit changes documents only;
it does not merge the integration branch into the audit branch.

**Result: existing high-priority chain findings remain open; one additional
P3 monitoring candidate, F46, is documented below.** No new C3/PF1 execution
defect was established in this pass. That is a bounded review result, not a
clean bill of health for the chain. As in the [audit policy](README.md),
source-supported findings still need failing production-code regressions.

The review covers the project beyond the nine files changed since `d52a33f`:
execution and economic accounting, resident state and persistence, ordinary
consensus application scheduling, mempool ownership, staking/governance,
EVM/bridge/RPC, wallet/explorer/feeder, configuration and monitoring. The
previously interrupted pass-5 certificate/malformed-input/adversarial-network
assignments were not resumed. Historical findings in those areas are preserved
with provenance, not represented as newly completed audits.

## Review evidence and coverage

| Reviewer | Scope | Detailed report |
| --- | --- | --- |
| GPT-6.1-sol | Execution, margin, matching, liquidation, staking/governance, C3/PF1 | [Execution](chain-d9ef4f7-pass13-sol-execution-2026-10-04.md) |
| GPT-6.1-sol | EVM bridge, RPC, clients, explorer, feeder, operations | [Interfaces](chain-d9ef4f7-pass13-sol-interfaces-2026-10-04.md) |
| GPT-6-astra | Execution scheduling, resident state, replay, storage and pool ownership | [Lifecycle](chain-d9ef4f7-pass13-astra-lifecycle-2026-10-04.md) |
| GPT-6-astra | Independent challenge of findings and coverage | [Independent recheck](chain-d9ef4f7-pass13-astra-recheck-2026-10-04.md) |

The coordinator additionally checked the C3/PF1 implementation and test
assertions, session timestamp units, EVM error handling, fee-accounting callers,
vote-store write semantics and monitoring producer/consumer wiring. Source
links in this pass pin `d9ef4f7`: relative source links would incorrectly open
the audit branch's older implementation. Prior reports retain their original
revisions and qualifications; their numbering is not a count of newly found
defects.

Cargo and rustc are unavailable in this environment. **Rust tests were read,
not run.** No live chain, Prometheus deployment, database restart, power-loss
experiment or performance benchmark was exercised. Historical Python models
and dependency checks are provenance, not fresh execution of this head. The
code index returned unverified/stale-looking locations; current checkout reads
and Git comparisons supplied the evidence.

## F46 — P3: backlog telemetry leaves the shipped warning silent

The shipped [MempoolBacklog rule](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/monitoring/alerts/consensus.yml#L57)
requires both a positive combined pool size and positive ten-minute increase
in `torus_mempool_evm_size`. The
[EVM gauge](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-telemetry/src/lib.rs#L833)
is registered and exposed, but repository-wide producer searches find no
production update of it. In contrast, the
[native publisher](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L375)
updates the native gauge at attachment and pool transitions.

With the shipped node exporter and alert loaded, a sustained ordinary native
backlog can make the sum positive while the EVM series stays zero. The second
conjunct remains false, so waiting longer does not make this warning fire.
EVM dashboards also report an unchanged zero while that pool contains entries.
This is a monitoring omission, not evidence that transactions were lost or
that consensus stopped. Deployments without this alert are outside its alert
impact, and other logs, metrics or custom alerts may expose the backlog.

Publish the EVM gauge on relevant pool mutations and initial metrics attachment.
Define the backlog rule using the intended pool(s) and gauge growth/stall
semantics; merely wiring EVM updates still leaves a native-only growing queue
dependent on EVM growth. Add a production-pool metrics regression and an alert
rule fixture covering native-only growth, EVM growth, stable/empty pools and
draining. The [interfaces report](chain-d9ef4f7-pass13-sol-interfaces-2026-10-04.md)
contains the complete caller and test evidence. F46 is separate from F43's
timeout metric-name mismatch and predates this integration's source changes.

## Existing issues that still determine chain readiness

These are repeated findings, not new numbers. The detailed reports distinguish
current caller inspection from retained historical/dependency evidence.

| Existing issue | Current disposition and practical implication |
| --- | --- |
| F01: execution gas/base-fee constraints | Retained from the [initial audit](chain-cea1254-2026-10-04.md). The consensus application, validator and EVM executor are unchanged from `d52a33f`; this pass does not rerun adversarial proposal experiments or the pinned dependency inspection. C3/PF1 does not address header policy. |
| F02: duplicate EVM priority-fee credit | Current Torus callers still compute full receipt fees, seed native state from the EVM bundle, and distribute full fees. The initial report's standard-revm beneficiary accounting evidence remains historical. Add an actual committed-transaction supply-conservation regression before considering this closed. |
| F03: session expiry unit mismatch | Coordinator rechecked the complete unit seam: session creation stores milliseconds; committed execution passes header seconds to the verifier's direct expiry comparison. Mempool expiry checks do not correct this execution contract. |
| F04: execution after state errors | Coordinator rechecked the committed EVM validator's blanket error branch: it logs and continues. Later batch-build failures do fail-stop, but that protection does not cover the earlier validation error. Session lookup also retains error-to-absence conversion. |
| F05: vote persistence durability | The atomic vote record still reaches ordinary `db.write`. Atomic grouping and persist-before-send ordering do not establish power-loss durability. Prior dependency/durability analysis is retained; no crash experiment was run. |
| F06/F12: liquidation and insolvency | Execution report retains bounded ADL coverage and the off-mark/flat-account solvency issue. Cached equality with the existing margin formula is not an independent solvency proof. |
| F07/F09/F11/F29/F45: epochs, governance and staking | Existing header epoch, governance snapshot/timing, small-set rotation and reward-residue qualifications remain. Empty epoch-boundary processing and ClaimUnbonded are present; old contrary assumptions are not revived. F45 is bounded wrong-recipient dust, not extra total emission. |
| F18/F19/F21/F30/F31/F38: durable state and restart | Lifecycle report rechecks bytecode persistence, restart config, snapshot verification coverage, state/mirror ordering, pruning of unapplied inputs and archive pruning-frontier reload. Clean successful-path cache handoff does not close these seams. |
| R2/F39/F44: transaction ownership | Future-nonce EVM selection/destructive draining lacks a production reinsertion caller; byte-budget admission and the wallet's latest-nonce choice remain separate issues. A wallet pending-nonce fix alone does not provide in-flight ownership. |
| F22–F28/F32–F37/F40–F43: interfaces and operations | Execution/interfaces reports retain the applicable bridge IDs/order types, market handling, explorer/wallet schemas/history, configuration, read-precompile, RPC quantity/access-list and monitoring issues with their earlier caller qualifications. See the historical index for findings not independently retraced here. |
| F08/F10/F13–F16 and other unlisted historical candidates | Not closed or renumbered. This pass does not claim a fresh complete audit of certificate admission, validator-key adversarial scenarios or ingress/DA abuse. Earlier reports remain the evidence, subject to their later corrections. |

Current coordinator anchors: [seconds passed into batch verification](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1998),
[millisecond session creation](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8024),
[expiry comparison](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-types/src/eip712.rs#L1226),
[EVM error continuation](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1880),
[full fee calculation](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/proposer.rs#L395),
[bundle seed](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2142),
[fee distribution](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2306),
and [vote-store write](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/kv_store.rs#L170).

## C3/PF1 conclusions and limits

C3 caches position-dependent sums, not the full live balance. The
[reader](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L1043)
bypasses the cache for a trader with pending position writes; missing marks
outside the block table are not cached. Mark/config changes invalidate old
versions, and block completion evicts traders touched by position puts or
tombstones. The cache is RAM state owned by the resident slot and starts empty
on reconstruction. Deferred order-book writes use different column families.
The lifecycle report traces the real handoff and worker barrier.

PF1's [ask-depth prefix accumulator](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L339)
preserves ascending-level and queue order for saturating accumulation. Stored
prefixes and binary lookup handle queries that decrease as well as increase.
No changed production result was established from these optimizations.

The new seeded cache tests contain useful non-vacuity counters and reference
comparisons, but use direct fixture writes and a manually managed resident
sequence. They do not establish a signed full-node sequence or a final durable
database reopen. Existing context-recreation and deferred-worker tests provide
additional coverage, with the limits described in the lifecycle report.
Differential equality may preserve a shared economic defect.

The merged-head performance verdict also remains open. The checked-in
[performance report](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md#L172)
explicitly lists combined C3/PF1 full-node cells and Gate 3 benchmarks as pending.
Earlier PF1-only throughput numbers and historical workspace test results must
not be reported as validation of `d9ef4f7`.

## Recommended next verification

1. Run the workspace and targeted cache/depth tests on this exact source head
   in a provisioned Rust environment; preserve failures rather than substituting
   results from the pre-merge branches.
2. Prioritize actual committed-path regressions for fee conservation, session
   expiry and storage-error handling, then the existing persistence and
   liquidation findings. No production fixes are included in this commit.
3. Exercise signed multi-block activity with real pipeline overlap, mark/config
   transitions and dirty traders; compare exact state and outputs against the
   reference, drain pending work, close all DB owners, reopen and continue.
4. Test the EVM gauge and backlog alert together. Keep monitoring severity
   separate from the unresolved execution/accounting priorities.

The audit documents preserve both actionable findings and unsuccessful leads;
additional report volume is not evidence that every subsystem is correct.
