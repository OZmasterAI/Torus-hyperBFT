# Design: Sprint 5 Task 1 — Ingress Verify Cost (binary format + friends)

**Status:** BRAINSTORM (s351) — option pending user pick
**Parent:** docs/plans/sprint5-wire-efficiency.md item 3 (promoted by s351 sweep data)

## Problem

`rpc_submit_verify_seconds` dominates submit latency and caps included
throughput at ~31k orders/s (s351 fresh-era sweep: bs200 peak 31.0k, bs300
19.2k, bs500 1.9k with 97% drops). Cumulative blocking verify: 1,689s over
1,475 batches at the bs500 snapshot (~115ms per 500-order action).

## Context (memory + exploration)

- Hot path `verify_one_action` (torus-rpc/src/torus.rs:214-233):
  hex decode → `serde_json::from_slice` → `validate_with_sessions`
  (EIP-712 struct hash + ecrecover, eip712.rs) → `serde_json::to_vec`
  (canonical bytes) → keccak256 (action hash/identity).
- Batch endpoint (torus.rs:703+): whole batch = ONE `spawn_blocking`, actions
  verified **serially** inside it; one semaphore permit per batch.
- **Identity constraint:** action hash = keccak256(serde_json canonical
  bytes); same bytes feed leader-forward + gossip. Ingress format change must
  not alter identity (determinism test required — sprint5 doc).
- **Measurement gap (discovered in this brainstorm):** the histogram times
  the `spawn_blocking` await → includes blocking-pool queue wait + core
  contention. Est. pure CPU per 500-order action ≈ 1–2ms vs 115ms observed →
  the sum is likely contention-dominated, not parse-dominated.
- **Pay-then-drop:** at bs300+ most actions complete full verify, then die at
  mempool admission (drops 59–97%). Verify cost is spent on doomed actions.
- Box reality: seed co-hosts consensus + exec; CPU pressure on this box has
  caused leader-slot famine before (s350/s351). Any "use more cores" answer
  risks consensus starvation.

## Options

### Option A: Instrument-first (split queue vs CPU) + micro-bench
Split `rpc_submit_verify_seconds` into `_queue_seconds` (spawn_blocking
dispatch wait) and `_cpu_seconds` (measured inside the closure). Add a
micro-bench (test or criterion) for `verify_one_action` at bs{1,100,500}
payloads to apportion hex/parse/eip712/ecrecover/re-serialize/keccak.
- Files: torus-rpc/src/torus.rs, torus-rpc/src/metrics.rs (or equivalent),
  new bench/test in torus-rpc.
- Trade-offs: zero throughput gain by itself; converts every later decision
  from guess to data. Hard to argue against.
- Effort: Small. Risk: Low.

### Option B: Binary ingress endpoint (bincode/borsh over hex), canonical JSON kept
New `submitNativeActionsBin` accepting hex/base64-bincode payloads of
`SignedNativeAction` (serde-derived, so bincode works without new derives).
Server: bincode decode (5–20x cheaper than JSON parse) → validate → ONE
`serde_json::to_vec` for canonical bytes/hash (identity unchanged) → keccak.
Bench client gets `--format bin`. Old JSON endpoint stays (mixed clients OK —
ingress-only change, no consensus/wire impact).
- Files: torus-rpc/src/torus.rs, bench-throughput sender, determinism test
  (bincode→struct→canonical-JSON→hash equals JSON-path hash).
- Trade-offs: kills parse cost + 2–3x client bytes; does NOT touch
  re-serialize, eip712, ecrecover, or contention. Win bounded by A's verdict
  on where time actually goes.
- Effort: Medium. Risk: Medium (serde-bincode edge cases: untagged enums /
  flatten attrs in SignedNativeAction need the determinism test to catch).

### Option C: Cheap-first admission (kill pay-then-drop)
Before paying signature verify, run the free checks: mempool capacity /
per-sender caps / rate limits / batch-size. Under saturation, reject in
microseconds instead of after ~ms of crypto. Directly recovers the bs300+
regime where 59–97% of fully-verified actions were dropped at admit.
- Files: torus-rpc/src/torus.rs (reorder), torus-mempool (expose capacity
  pre-check), tests for early-reject paths.
- Trade-offs: helps only the saturated regime (but that IS the failing
  regime); zero gain below saturation. Small, self-contained.
- Effort: Small. Risk: Low (must keep reject reasons/error semantics
  identical for clients).

### Option D: Parallel in-batch verify (rayon), gated on A
Replace the serial `.map()` with a bounded parallel iterator (small dedicated
pool, 2–4 threads). Multiplies through ALL per-action costs.
- Trade-offs: only helps if cores are actually idle — A must show
  CPU-vs-queue split first. On this box, consensus starvation risk is real
  (famine history). Bound the pool, never default to num_cpus.
- Effort: Small. Risk: Medium (contention with consensus/exec).

## Recommendation

A → C → B as one task sequence (D only if A shows idle cores + queue-light
CPU dominance). A is hours and decides everything; C is the cheapest real
win in the regime that actually fails; B is the Sprint-5 headline and
parallelizes with C. All three preserve hash identity and need no
coordinated validator upgrade.

## Open Questions

1. Where do the 115ms/action actually go? (A answers: queue vs eip712 vs
   parse vs ecrecover vs serialize.)
2. Submit semaphore size — how many concurrent batches contend today?
3. Does `SignedNativeAction` serde shape round-trip bincode cleanly
   (untagged/flatten)? Determinism test decides borsh-vs-bincode.
4. bench client: keep hex envelope for binary payload (JSON-RPC string) or
   add base64 to halve envelope overhead?
