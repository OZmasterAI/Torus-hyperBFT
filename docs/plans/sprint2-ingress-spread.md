# Design: Sprint 2 — Ingress Spread (batch-submit RPC + local submitters)

**Session:** 334 · **Branch:** `fix/native-da-push-hardening` · **Status:** DESIGN

## Problem

Post-Sprint-1 ingress is the binding constraint: 35–92 actions/s per endpoint, each
action = one HTTP call → parse → ecrecover → permit. bs500's 29,964 o/s record was
reached at only ~50 act/s submitted. Feeding 100k+ orders/s needs (a) fewer
per-action HTTP/JSON round-trips and (b) submitters on every validator host
(localhost, no WAN/tunnel in the submit path).

## Options

### A. Dedicated batch endpoint `torus_submitNativeActions(Vec<String>)` ✅
One permit + one `spawn_blocking` per call covering parse+verify of all items
(≤ `SUBMIT_BATCH_MAX` = 100); per-item results (`{hash}` / `{error}`) so one bad
action never poisons the batch. Amortizes HTTP, JSON envelope, permit churn, and
blocking-pool scheduling. ~Small effort, low risk, additive (old endpoint stays).

### B. jsonrpsee protocol-level request batching
Zero new endpoint — but each sub-request still burns its own permit +
spawn_blocking, and per-item error semantics depend on client library behavior.
Rejected: keeps the per-action scheduling overhead that A removes.

### C. Streaming/WebSocket ingest
Most throughput long-term, but a new transport + client rewrite. Premature.

## Recommendation
A, plus bench support (`--submit-batch N`, `--sender-offset K`) and a dual-box
proof: one bench instance per validator host submitting to localhost, sender key
ranges split (0-9 / 10-19) to avoid cross-instance nonce/rate-limit collisions.

Measurement note: every instance's post-run body sweep counts ALL included
actions (both instances'), so network throughput = ONE instance's included
number over its window — never the sum.

## Open Questions
- None blocking. SUBMIT_BATCH_MAX=100 ≈ 100 ecrecovers ≈ 5–10ms per blocking
  task — bounded and fine at 64 permits.
