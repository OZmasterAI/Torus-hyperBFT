# Pending proposal action hash cache

Current integration: branch `perf/cached-proposal-hashes-viewbound`, source
`3495652`, based on main `d3e1370`. Commits `70a2818` and `3495652` are exact
cherry-picks of `c951d58` and `388f7cd` from the original
`perf/cached-proposal-hashes` candidate. The integration adds only this cache,
its tests and documentation; it includes no DA-reuse or cancellation candidate.

`PendingProposal` owns an immutable block and a `OnceLock` containing ordered
content addresses derived from its signed native actions. The first selection
exclusion or compact encoding initializes that vector; proposal encoding,
selection exclusions and the independent in-flight ledger reuse it. Successful
validation alone adds no hashing pass, and full-block encoding does not initialize
the cache. Previously selection rehashed pending bodies, and own compact encoding
repeated the ledger's hashing.

Compact commit matching first checks the action count. A warm cache compares its
ordered hash vector with the committed compact hashes. A cold cache streams
`compute_action_hash` comparisons, preserving early mismatch rejection and
leaving the `OnceLock` uninitialized: commit immediately consumes the entry, so
allocating a cache there provides no reuse. This rejects reordered, substituted,
partially cached or differently signed bodies.

The block has no mutable accessor. Replacing or removing a pending entry also
replaces or removes its hash cache. The committed compact's header, EVM
transactions and core-writer actions remain authoritative. The independent
in-flight ledger still covers missing compact bodies and unions same-height
proposals. Deferred execution does not carry cached hashes; dispatch's
mempool-prune hash pass remains.

DA mirroring is unchanged. Selection's `flush_da_mirrors()` returns no success
result, and a concurrent ingress flush can hold a batch not yet written.
Successful selection therefore does not prove durability. The explicit proposer
mirror and its fail-closed behavior remain intact.

The three `pending_proposal_` tests cover byte-for-byte full/compact encoding,
ordered and signature-bound matching including cold/warm cache behavior, and
real selection exclusions from validated bodies and the missing-body ledger.
Existing stale-smaller-proposal, durable-proposer-body and in-flight-ledger tests
remain relevant.

## Current integration verification and artifact

The coordinator verified source `3495652`: 149 consensus tests passed, one was
ignored, and a separate node build passed. Receipt:
`247ed19c-d694-492b-a191-b557cc180422`. Integration issue `a4c27022` was resolved.
These checks validate the integration; they do not establish a performance win.

Frozen node artifact set:
`/home/18c/bench-results-matched/s60-campaign-20260918/artifacts/cached-hashes-viewbound`.
Node SHA256:
`4e28a137ccd640842c415b1e8584b57b1309ac1115444a2c47bb94957bdb2fd5`.
The load generator is unchanged. This documentation-only update does not alter
that verified source or frozen binary.

## Live evidence and remaining acceptance

The older `388f7cd` candidate had one accepted live cell,
`s60-hashes-cap200-r1`: 51,608.1 matched fill events/s over its 127-second load
window, with a 93-second drain. That is evidence for the older source/base only,
not a live result for `3495652` and not a repeated causal speedup measurement.
The campaign also observed larger backlog on that candidate. The unchanged
`torus_orders_matched_total` measures fill events, not unique orders or two legs;
see [metric definitions](matched-fill-units-and-generator-2026-09-18.md).

The current integration `3495652` has no live result yet. It still requires
healthy, matched-duration baseline/candidate comparisons on the current view
recovery base, including construction/validation time, backlog, drain, agreement
and full-load actual matched/s. No promotion, sustained throughput improvement,
or 200k result is claimed.
