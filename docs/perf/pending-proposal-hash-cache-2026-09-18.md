# Pending proposal action hash cache

Candidate based on `3cc958b`, branch `perf/cached-proposal-hashes`.

`PendingProposal` owns a block and an ordered vector of content addresses derived
lazily on first use from its signed native actions. Proposal encoding, the in-flight ledger,
selection exclusions, and the compact commit cache check reuse those addresses.
Previously selection rehashed every pending body, own proposal encoding repeated
the ledger's hashing, and compact commit cache matching hashed the body again.

The block has no mutable accessor. Replacing or removing a pending entry also
replaces or removes its hash vector. Compact matching compares the complete
ordered vector; it must reject reordered, substituted, partially cached, or
differently signed bodies. The committed compact's header, EVM transactions and
core-writer actions remain authoritative. The independent in-flight hash ledger
still covers missing compact bodies and unions same-height proposals.

DA mirroring is unchanged. In particular, selection's `flush_da_mirrors()` returns
no success result, and a concurrent ingress flush can hold a batch not yet written.
Successful selection therefore does not prove durability. Removing the explicit
proposer mirror would remove its fail-closed guarantee.

The three new `pending_proposal_` tests cover byte-for-byte full/compact encoding
against the existing encoder, ordered and signature-bound cache matching, and
real selection exclusions from both validated bodies and the missing-body ledger.
The existing `committed_block_ignores_stale_smaller_pending_proposal`,
`produce_block_bodies_durable`, and `in_flight_ledger_` tests remain relevant.

At handoff, Rust parsing through `rustfmt --emit stdout` and `git diff --check`
pass. Cargo tests and benchmark cells have not run; builds are coordinated by the
parent agent. No speedup or healthy baseline is claimed. The retained cap-200
cells still fail dissemination acceptance, so acceptance requires the liveness
prerequisite and healthy same-shape baseline/candidate repeats.

This first change does not carry cached hashes through deferred execution;
dispatch's mempool-prune hash pass remains. A OnceLock avoids adding eager hashing
to every successful validation; the first selection, compact encoding or commit
cache comparison initializes the hashes. Measure validation alongside build time.
