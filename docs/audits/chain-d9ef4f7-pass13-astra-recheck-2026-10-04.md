# Pass 13 — independent findings and coverage recheck

Reviewed 2026-10-04 against `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`, using the source checkout rather
than the older source in the separate audit-document worktree.

**F46 survives as a bounded P3 monitoring candidate. No additional numbered
finding or new C3/PF1 execution defect is established.** Existing findings are
not closed. The [audit convention](README.md) requires failing production-code
regressions; this report supplies source and test-coverage evidence only.

I read the prior finding index, initial fee finding, pass-6 correctness review,
pass-12 consolidated/recheck reports and all current pass-13 companions, then
independently inspected the producer/consumer and execution paths below. No
applicable `AGENTS.md` was found. Cargo, rustc and promtool were absent from
PATH. All cited tests were **read, not run**. No source/Git mutation, Torus
write, installation, service, key operation or live-chain request occurred.
The interrupted pass-5 certificate, malformed-input and adversarial-network
assignments were not resumed. Only this document was written.

## F46: attempted counterexamples do not repair the contract

The [shipped node][attach] attaches its shared Metrics to the live mempool.
The [registered EVM gauge][gauge] is a default Gauge described as pending EVM
transactions. Unrestricted searches across `crates` and `tools` find its field
name only in declaration, construction, registration and Metrics assembly;
there is no repository producer. Inspection of [successful admission][admit],
reinsertion and drain confirms that they change the actual pool and retained
byte count without updating this gauge. Registry encoding does not sample
`evm_pool_size()`.

An ordinarily funded, valid transaction left pooled before proposal selection
therefore separates actual occupancy from exported occupancy. No nonce gap,
abandoned proposal or failed execution is needed. The existing [insertion
test][insert-test] asserts actual pool size equals one, but attaches no Metrics.
Conversely, the [native tests][native-test] attach real Metrics and assert one
after attachment to a populated pool, then zero after drain. Native occupancy
has a real [locked publisher][publisher]; it refutes any blanket claim that
all mempool telemetry is unwired, but does not publish EVM occupancy.

The [actual alert][alert] requires:

```promql
(torus_mempool_evm_size + torus_mempool_native_size) > 0
and increase(torus_mempool_evm_size[10m]) > 0
```

Under the shipped exporter, with the rule loaded and these series scraped,
the unchanged zero EVM series cannot satisfy the second comparison. Waiting
through `for: 10m` cannot repair a false condition. A real native backlog can
grow while this warning remains inactive. Even after adding an EVM publisher,
native-only growth still fails that conjunct. The [dashboard][panel] also
selects the EVM gauge directly.

This is source-derived monitoring failure, not observed Prometheus behavior,
a missing notification witnessed in production, or transaction loss. Custom
exporters/rules and other alerts are outside the claim. F43's distinct timeout
sample-name problem remains separate. I agree with grouping EVM production and
the native-only predicate limitation under F46's advertised backlog contract.

The useful regression attaches real Metrics to a real EVM pool, verifies the
encoded sample against occupancy after initial attachment, admission,
replacement, rejection, drain and reinsertion, then evaluates the actual rule
on EVM growth, native-only growth, stable occupancy and empty/draining pools.
Changing the producer alone cannot close both aspects.

## C3 and PF1: safeguards and exact test boundaries

The [C3 reader][reader] caches position sums and combines them with the supplied
current balance. Dirty trader prefixes bypass both caches. The overlay's
[touch check][touch] covers own pending puts/deletes with resident rows and
conservatively returns dirty otherwise. [Block exit][end] removes affected
traders using delta keys, including tombstones. [Mark/config comparison][marks]
changes the version when usable marks or margin configurations change; an
out-of-table mark is not cached. [Holder reconstruction][begin] creates an
empty sums cache. Balance-only changes therefore need not evict position sums.
These mechanisms defeat the obvious stale-balance, deletion, stale-mark and
cold-holder counterexamples on the inspected ordinary path.

The [six-seed, forty-block test][sums-test] asserts equality of per-block
result success/errors and position/balance rows, plus more than 100 persistent,
memo, computed and dirty-path uses and nonzero overflow coverage. Its
[harness][sums-harness] directly writes positions/config/oracle fixtures,
manually carries books, compares overlay rows, and flushes the previous frozen
block. The final frozen block remains unflushed. This is meaningful differential
coverage, not a complete signed execution or durable final-state round trip.
The [application matrix][app-test] adds all-CF/write-set/hash comparisons across
book modes and context recreation, but its helper retains the same open
StateDb; resident comparisons drain the worker after each block. The lifecycle
companion correctly distinguishes the additional deferred-worker barrier tests.

[PF1][depth] preserves ascending-level and queue order for saturating addition;
saved prefixes and `partition_point` also handle later lower-price queries.
Its [production caller][depth-caller] uses an immutable resting book per market
batch and separately folds earlier same-batch asks. The [tests][depth-tests]
compare arbitrary query orders to the old walk and assert saturation. Whole
helper equality includes top-up count, each prepared reservation, cached
balances and dirty addresses. The 50,000-ask fixture independently checks
depth, prefix count and boundary decisions. No changed result was established;
these assertions do not establish combined full-node throughput. That verdict
is explicitly [still open][perf].

## Older accounting and cross-subsystem evidence remains material

**F02/F17 are not disproved by separate component tests.** The current
[application][fees-caller] computes full receipt fee revenue, then
[seeds the native overlay from the EVM bundle][seed] and
[distributes that revenue][fees-distribute]. The initial F02 report's pinned
revm beneficiary analysis remains historical dependency evidence; I did not
download or rerun it. The [fee conservation test][fee-test] injects a synthetic
fee amount, asserts bucket totals, and checks burn/reward/treasury credits.
It never debits an executed transaction. The [bridge tip test][tip-test] does
execute a typed envelope but commits via BlockCommitter and asserts
`gas_used * (max_fee - base_fee)` beneficiary credit; it omits the application's
later native distribution. These do not reconcile the combined ledger. Keep
the [earlier regression requirement](chain-cea1254-pass6-correctness-review-2026-10-04.md):
actual committed sender/beneficiary/recipient/reward-liability and burn deltas,
with a priority cap below maximum fee headroom, including success and revert.

**F42's request/response distinction still matters.** [CallRequest][call]
contains no access-list field. Its [compliance test][access-test] manually
builds a response transaction and asserts a one-element serialized array.
That assertion cannot refute a lost call-input access list. No new ID follows.

**F32 must remain caller-specific.** The [wallet formatter][wallet] reads
integer power and `commission_bps`; the [explorer][explorer] reads hex power
and `commissionBps`. The latter is a concrete counterexample to extending the
wallet finding to every client. **F45 remains bounded:** ordinary
[undelegation][undelegate] preserves the zero-active row with queued principal,
and [inflation distribution][rewards] can assign its last-row residual there.
The earlier recipient-allocation qualification is appropriate; no extra total
emission or new reproduction is claimed.

No contradiction requiring a change to the consolidated pass-13 conclusions
was found. Next verification should combine signed execution, actual worker
overlap, cache-path non-vacuity, full drain and real DB close/reopen, alongside
the unresolved economic regressions and F46's producer/rule tests.

[attach]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-node/src/main.rs#L656
[gauge]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-telemetry/src/lib.rs#L833
[admit]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L395
[insert-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L2433
[native-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L1569
[publisher]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-mempool/src/lib.rs#L375
[alert]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/monitoring/alerts/consensus.yml#L57
[panel]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/monitoring/dashboards/execution.json#L204
[reader]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L1043
[touch]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-state/src/backend.rs#L1980
[end]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2234
[marks]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L8601
[begin]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2158
[sums-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/sums_cache_tests.rs#L320
[sums-harness]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/sums_cache_tests.rs#L208
[app-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L16853
[depth]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L339
[depth-caller]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L4925
[depth-tests]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/same_batch_depth_tests.rs#L300
[perf]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md#L172
[fees-caller]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L1825
[seed]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2142
[fees-distribute]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-consensus/src/app.rs#L2306
[fee-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-integration-tests/tests/fee_flow.rs#L41
[tip-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/tests/bridge_tests.rs#L621
[call]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/src/types.rs#L222
[access-test]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-rpc/tests/eth_compliance_tests.rs#L238
[wallet]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/tools/wallet/src/commands/query.rs#L63
[explorer]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-explorer/src/indexer.rs#L209
[undelegate]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/staking.rs#L143
[rewards]: https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-economics/src/rewards.rs#L231
