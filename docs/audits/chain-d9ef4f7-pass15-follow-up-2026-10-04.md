# Pass 15 — C3/PF1 integration and critical-path follow-up

Reviewed 2026-10-04 against `merge/item6-c3-pf1` at
`d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd`. This report is committed on
`audit/chain-findings-2026-10-04`, whose preceding audit commit is `c2fed8e`.
The source and report worktrees remain separate. This is a focused static
follow-up to passes 13–14, not a line-by-line re-audit or a test run.

**Result: no new finding is promoted.** The follow-up traces the changed C3
margin-sums cache and PF1 ask-depth accumulator through their callers, then
rechecks representative high-impact execution, session-expiry, fee and vote
write paths. Existing findings remain open under their original numbers and
qualifications. A lack of a newly confirmed defect in these paths is not a
clean bill of health for the project.

No applicable `AGENTS.md` was found. Source links below pin `d9ef4f7`, because
the audit worktree contains documentation commits based on older production
code. Rust tests were not run. No production source, live chain, RPC endpoint,
database, service or remote branch was changed.

## C3 margin-sums cache

The [cache lookup](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L1038)
uses a cached sum only when the current block has the matching mark-table
version and the overlay reports no position write or tombstone for that
trader. A dirty trader is recomputed through `positions_for_trader`, which
reads the block's overlay. The default backend implementation of
`layer_touches` is conservatively dirty; the resident overlay checks its own
pending keys, while parent-layer writes are already represented in the
resident rows built at block start.

At [block completion](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L2258),
the cache merges only completed memo entries and removes sums for traders
whose position keys appear in the block delta, including deletions. A changed
mark or margin configuration receives a new valuation version; a failed mark
scan leaves the table absent and uses direct reads. The resident rows, marks
and cache are discarded together when reuse fails. These paths are coherent
on static inspection and do not establish process-crash or full-node
equivalence; see the [pass-13 lifecycle review](chain-d9ef4f7-pass13-astra-lifecycle-2026-10-04.md)
for those remaining verification limits.

## PF1 ask-depth accumulator

The [new accumulator](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L339)
consumes ask levels in ascending book order and orders within each queue, using
the same saturating addition as the former per-order fold. `upto(price)` keeps
the through-level prefix, so queries at later or lower prices select the same
prefix as the prior `take_while(price <= limit)` walk. The only production
caller, [same-batch top-ups](https://github.com/OZmasterAI/Torus-hyperBFT/blob/d9ef4f7a7ed8f449867dbf56fda961dd9d3d37cd/crates/torus-bridge/src/native_executor.rs#L6757),
uses one accumulator per book while preserving batch-ask accumulation and the
later flat-index ordering. The checked-in differential test compares the new
path with the prior implementation over generated books and batches, but this
review did not execute it. No source-supported output difference was found.

## Representative prior findings

- **F02, EVM fee accounting:** the proposer still derives native fee revenue
  from `gas_used * effective_gas_price`; the prior consensus path still adds
  the EVM bundle and distributes the full computed revenue. This follow-up
  does not run a committed-transaction conservation regression; F02 remains
  open with its earlier evidence and scope.
- **F03, session expiry:** session creation retains millisecond expiry values,
  while the committed batch verifier receives the block timestamp in seconds
  and compares the values directly. This targeted recheck found no unit
  conversion at the verifier seam. F03 remains open.
- **F04, execution after state errors:** the committed-block EVM execution
  error branch logs the failure and continues into the native phase. A later
  EVM batch-construction failure fail-stops, but does not guard this earlier
  execution-error branch. F04 remains open under its prior qualification.
- **F05, vote durability:** the vote-state write still calls ordinary
  `RocksDB::write` without requesting synchronous durability. This is a
  durability-property concern; this review performs no crash or power-loss
  experiment and makes no new persistence claim.

The sources for these findings and their boundaries are documented in the
[initial audit](chain-cea1254-2026-10-04.md),
[pass 3](chain-cea1254-pass3-project-wide-2026-10-04.md), and
[pass 13](chain-d9ef4f7-pass13-project-wide-2026-10-04.md). This pass does not
mark any issue fixed, verified, or newly reproduced.

## Limits

This follow-up was source inspection only. It did not run unit, integration,
property, model, or performance checks; reproduce cache behavior in a running
node; inspect a live chain; or exercise crash durability. Existing audit
reports retain the evidence and unresolved work for areas outside this focused
pass, including consensus adversarial input, economic correctness, restart
behavior, transaction ownership and client recovery. No new issue was written
to Torus memory because no new source-supported finding was promoted.
