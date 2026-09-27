# Design: block_build phase timers (s68)

## Problem
Leader `block_build` is ~171-199 ms of a ~600 ms loaded view (s67-votedb-ctl-r2,
LOAD window, cap 200) and is the largest on-critical-path stage left. It is one
timer; nothing says which part of `produce_block` costs what.

## Context
- The s55 "cache action hashes in pending_proposals" item is already on main
  (984e6ef, 1364d74, c250a5d); s67 measured with it in place.
- Proposer DA mirror must stay for durability unless ingest-side flushes can
  prove per-body success (Codex issue d7129204).
- Custody is ~0 ms in bench cells (validate_block_custody_ms = 0.0).
- `ProduceBlockRequest::new` is `pub(crate)` in hotstuff_rs, so
  `TorusApp::produce_block` is not unit-testable directly.

## Change
Five histograms, same shape as `torus_validate_block_*`:

| Metric | Covers |
| --- | --- |
| `torus_block_build_parent_seconds` | parent block read + datum decode (runs before the existing block_build timer) |
| `torus_block_build_select_seconds` | `select_block_payload` (in-flight set, mempool select, EVM drain) |
| `torus_block_build_mirror_seconds` | `mirror_or_drop_native` (body clone, DA put_batch, shard custody) |
| `torus_block_build_attest_seconds` | `generate_sig_attestation` (bincode + sha256 over all bodies, sign) |
| `torus_block_build_encode_seconds` | block construction, pre-proposal push, `PendingProposal` encode, ledger note, datum hash |

`produce_block` keeps parent-header resolution and delegates the rest to a new
`build_proposal(parent_header)` so tests can drive it. Harness: add the five
series to `VIEW_HISTS` (summarize.py) and `WIDE_COLS` (run-cell.sh) so the LOAD
window slices them.

## Tests (first)
1. telemetry: the five names are registered.
2. consensus: `build_proposal` with one pooled action observes each of
   select/mirror/attest/encode once and block_build once; phase sums <= total.
3. harness: every `VIEW_HISTS` entry has `_sum` and `_count` in `WIDE_COLS`.

## Measurement
1 warm-up + 2 scored cap-200 cells from this branch's binary, same runner as
s67 (`TORUS_BODY_FETCH_TRACE=1`, 76000). Output: per-phase LOAD-window means.

## Not building
Per-phase timers inside the mempool or DA store; a separate proposer custody
histogram (custody is ~0 ms); any fix before the numbers are in.
