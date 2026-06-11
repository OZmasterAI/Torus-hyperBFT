# Design: Ingress Verify Fix (s351 follow-up)

## Problem
The s351 probe (docs/plans/exec-ceiling-verdict.md) named RPC ingress
verify as the throughput ceiling: per 10-action bs500 batch, verify wall
1.47s (CPU 0.70s + 0.77s core contention), admit 0.42s ⇒ ~1.9s ack RTT.
Ten serial senders ⇒ 39 actions/s submitted, 65.5% bench drop, mempool
starved, exec pipeline idle (queue depth 0 throughout).

## Context (code, verified)
- `run_submit_pipeline` (torus-rpc/src/torus.rs:294): cap → permit
  (`SUBMIT_PERMITS=64`, never contended) → pool-full prescreen →
  **ONE spawn_blocking that verifies the batch SEQUENTIALLY** (:372-391,
  `.into_iter().map(verify_one_action_with)`) → admit loop on the async
  thread (:401-462).
- `verify_one_action_with` (:238): hex parse → decode → EIP-712/session
  verify → **canonical serde_json::to_vec + keccak** (:253-257). Canonical
  JSON is hash-identity-load-bearing (sprint5-B gate, commit 376144f) —
  the wire bytes can NOT be hashed directly.
- 70ms CPU per bs500 action. Exec-side comparison (rayon batch verify ≈
  45ms CPU/action for eip712+ecrecover) ⇒ ~45ms crypto/hash + ~25ms
  decode/encode extras.
- Hard ceiling at current per-action cost: 8 cores ÷ 70ms ≈ **114
  actions/s ≈ 57k orders/s** no matter how work is arranged.
- rayon: in torus-types (eip712.rs:881 par_iter), NOT yet in torus-rpc.

## Options

### Option A: Parallelize within-batch verify (rayon in the closure)
Swap the sequential `.map(verify_one_action_with)` for `par_iter` inside
the existing spawn_blocking (mirror eip712.rs:887; order preserved by
collect). Batch verify wall 0.7s → ~0.1-0.2s; ack RTT ~1.9s → ~0.6s.
Serial senders submit ~3x faster ⇒ offered load ~110/s, saturating the
CPU ceiling instead of idling beneath it.
- Files: torus-rpc/src/torus.rs (closure), torus-rpc/Cargo.toml (rayon).
- Trade-offs: + small/safe, semantics identical, immediate 3x;
  − does not raise the 114/s CPU ceiling; under many concurrent batches
  total CPU unchanged (fair-share latency win only).
- Effort: S. Risk: Low.

### Option B: Cut per-action CPU (raise the ceiling)
Profile then optimize the 70ms: (1) sub-timers splitting decode /
eip712-verify / canonical-encode+keccak; (2) optimize PlaceOrderBatch
hashStruct implementation — per-order buffer reuse, no intermediate
Vecs, single keccak pass per order (output bytes MUST stay identical:
eip712_vectors.rs + the sprint5-B hash-identity gate are the proof);
(3) arena/buffer reuse for the canonical JSON encode.
- Files: torus-types/src/eip712.rs, torus-rpc/src/torus.rs (timers),
  torus-telemetry (3 sub-phase histograms).
- Trade-offs: + raises the real ceiling (1.5-3x plausible ⇒ 170-340
  actions/s); − gains unproven until profiled, touches
  signature-critical hashing (vector-gated), more invasive.
- Effort: M. Risk: Medium (byte-identity must hold).

### Option C: Decouple ack from verify (enqueue-then-ack)
Submit returns immediately after decode-only screening (hash assigned);
verify+admit run on a server-side bounded queue; clients poll status or
watch inclusion. Sender pacing no longer RTT-bound — offered load hits
line rate and the server verifies at the CPU ceiling continuously.
- Files: torus-rpc (new status endpoint + queue), bench-throughput,
  wallet/client docs.
- Trade-offs: + biggest pipeline-fullness win, natural batching server-
  side; − API semantics change (per-item errors become async), needs
  back-pressure + status-tracking design, bench/client updates.
- Effort: L. Risk: Medium.

### Bundled with any option: admit sub-timers (XS)
0.42s/batch (42ms/action) spent in the admit loop ON the async executor
(mempool insert + leader-forward enqueue + per-action alloc). Add
decode/insert/forward sub-timers to name the next target precisely.

## Recommendation
**A + admit sub-timers now** — one small PR, ~3x submit throughput,
saturates the existing ceiling and instruments the next one. Then **B
behind the new sub-timers** once A measurably pins all 8 cores
(probe rerun decides). C only if we want fire-hose ingestion semantics;
revisit after B. The exec-side double-verify trust-cache stays DEFERRED:
exec queue depth was 0 — optimizing the idle thread buys nothing today.

## Results (s352 + s353 probes)

Three identical bs500 solo runs (10 senders, sb10, 30s):

| metric                      | s351 serial | s352 shared-pool | s353 dedicated pool |
|-----------------------------|------------:|-----------------:|--------------------:|
| submitted (acked)           | 39/s        | 15/s             | 14/s                |
| unique actions executed     | 448 (~15/s) | 169              | 469 (~15.6/s)       |
| inclusion efficiency        | 34%         | 34%              | **~100%**           |
| block time under load       | 529ms       | 4034ms           | 1229ms              |
| exec verify phase /nat-blk  | 429ms       | 1224ms (90.7%)   | 291ms               |
| exec queue depth            | 0           | climbed to 26    | one blip of 1       |
| verify in-closure /batch    | 0.70s (cpu) | 5.9s             | 5.4s                |
| admit /batch (insert)       | 0.42s       | 0.14s            | 0.24s               |

- **s352 (naive par_iter)**: REGRESSION — global-rayon-pool sharing
  queue-starved consensus/exec batch verify. Reverted by 1659352.
- **s353 (dedicated 4-thread pool)**: starvation fixed (cadence and exec
  phases recovered, queue empty). Effective unique throughput equals the
  serial baseline (~15 actions/s ≈ 7.5k orders/s) but with ~100%
  inclusion of acked submissions (s351 dropped 65% client-side).
- Bench "included 1013 > submitted 460" is duplicate block-body counting
  across the 3-chain pipeline; exec-side dedup executed exactly 469.
- The wall is now nakedly the **70ms CPU per bs500 action**: 10
  concurrent batches demand ~7s CPU/wave against a 4-thread budget on a
  shared 8-core box. Arrangement is solved; cost is not.

**Verdict: proceed to Option B (cut the per-action CPU)** — sub-profile
decode vs eip712 vs canonical-encode+keccak, optimize the dominant term
byte-identically. Pool size (4) is a secondary tunable; raising it taxes
consensus (s352 showed the failure mode at the extreme).

## Open Questions
- Where exactly does the 70ms split (decode vs eip712 vs canonical
  encode+keccak)? B's sub-timers answer this; could land WITH A.
- Is admit's 42ms/action mempool-lock contention or forward payload
  copies? Sub-timers answer it.
- Does the 0.77s contention shrink proportionally under A, or do 10
  concurrent rayon'd batches thrash? (Single shared rayon pool ⇒ should
  fair-share; verify on the probe rerun.)
