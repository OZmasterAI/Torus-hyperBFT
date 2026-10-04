# Pass 18 — consensus validation and crash recovery

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. This is the second of five
additional focused passes requested after pass 16. It follows proposal checks,
block insertion, committed-block intake, strict-order dispatch and boot replay.
The audit documents are committed separately from the source branch.

**Result: no new finding is promoted in this pass.** The inspected path keeps
proposal, insertion and execution ancestry checks aligned and parks unrecoverable
execution gaps instead of advancing the applied marker. This is a static review;
it does not establish runtime behavior under storage faults or adversarial
HotStuff schedules.

## Review notes

- [`check_proposal_data`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L5068)
  checks the one-datum/hash binding, decodes full or compact form, checks the
  declared native-action count, links the header to the HotStuff parent, applies
  the vote-time future-timestamp bound, and requires compact native bodies to be
  present in durable storage before a vote. [`validate_block`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L5328)
  repeats the parent-link check at insertion; sync may fetch compact bodies
  before entering that validation path. The asynchronous validation hit still
  binds to the recomputed datum hash and re-runs ancestry on the algo thread.
- [`check_parent_link`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L1452)
  uses the justified parent header, requiring both the next height and canonical
  header hash. Execution repeats the parent hash check against the durable,
  already-applied height-minus-one header. The timestamp monotonicity condition
  is consistent across vote, insertion and execution; the local-clock drift limit
  applies only to voting, so an already certified block is not rejected later
  solely due to clock skew.
- [`on_committed_block`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L5419)
  takes the committed header from the consensus datum. Its compact fast path
  accepts cached bodies only when the hashes match the committed reference;
  otherwise execution reconstructs the body from DA. It persists the commit
  manifest before queueing, advances consensus height independently of execution
  readiness, and feeds the strict-order queue. The report did not find a new
  case where a same-height stale cache can replace the committed body.
- [`replay_gap`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L1277)
  visits the applied-to-committed interval in height order. Missing or corrupt
  non-empty data stops replay at the hole; the boot path parks that height for
  bounded live healing, and later heights remain queued behind it. The in-process
  head-of-line watchdog consults commit manifests to recover a committed height
  that did not re-arrive after restart.
- Earlier F13 (phase-mismatched certificate can stop automatic progress) and
  F14 (block-sync server conflict can cause fatal shutdown) remain separate open
  candidates from earlier reports. This pass did not reproduce either scenario
  or establish a new certificate-state-machine defect, so neither is counted as
  a new result here.

The detailed earlier consensus and recovery reports retain their finding
numbers, revisions and reproduction limits. This pass leaves their issue count
unchanged.

## Limits

No Rust tests, node restart, storage fault injection, network sync, or adversarial
certificate schedule was run. Source and existing regression assertions were
inspected only. No source changes or Torus issue writes were made.
