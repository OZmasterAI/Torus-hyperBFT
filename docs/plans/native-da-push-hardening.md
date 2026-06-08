# Design: Native-Action PUSH Hardening (#4 — un-wedge bs≈1000 / >4 MB)

**Created:** 2026-06-08 · **Status:** DESIGN (awaiting option pick) · **Branch:** TBD (off `fix/native-da-bigbody` or `main`, NOT on the live testnet)

This is follow-up **#4** explicitly deferred by the native-DA big-body fix
(`docs/plans/native-da-bigbody-dissemination-fix.md`, Open Q#3). That fix made the
**pull** path reliable for >4 MB bodies (per-protocol codec caps + chunked fetch +
~1 s sync-path budget). #4 hardens the **push** path and adds a **hot-path** pull so
the chain survives high-`batch_size` load without wedging.

## Problem
Under sustained native-action load the proposer's **PUSH** of pre-proposal action
batches collapses, and the **hot consensus path cannot recover** because it never
pulls — only the block-sync path does. Net effect: liveness wedge at high load.

## Evidence (live testnet bs-sweep, 2026-06-08, on the fixed pull binary)
| batch_size | result | note |
|---|---|---|
| 100 | ✅ no wedge | ~14.5k orders/s included, 80 blocks |
| 250 | ✅ no wedge | throughput collapsing (98.8% drop) |
| 500 | ✅ no wedge | a 50,000-order (~2–4 MB) block committed |
| 1000 | ❌ **WEDGED** | committed height frozen, views kept climbing |

- `max sub-streams reached` fired **2,537×** at bs=500 (worse at bs=1000) — quinn's
  `max_concurrent_bidi_streams` (default 100) exhausted by the push loop.
- **Zero** native-DA pull events at bs=1000 — the hot path doesn't pull, so a push
  miss is unrecoverable live (only sync pulls, and the wedged tip never enters sync).
- **No `message too large`** at any level — the bigbody codec/pull fix is sound; the
  bottleneck is purely the push path + the missing hot-path fallback.

## Root cause (verified in code)
- **`crates/torus-network/src/swarm.rs:994` `BroadcastNativeActions`:** builds one
  envelope (the full batched payload — multi-MB at high `batch_size`) and calls
  `direct.send_request(&pid, ...)` **once per connected peer in a tight loop with no
  backpressure**. Each request opens a QUIC bidi stream; many in flight → quinn
  `max_concurrent_bidi_streams=100` exhausted → `max sub-streams reached`. The payload
  also rides the **`/torus/direct` codec whose cap is still 4 MB** (`MAX_DIRECT_MSG_SIZE`,
  intentionally unchanged by the bigbody fix), so a >4 MB batch is rejected outright.
- **`crates/torus-network/src/bridge.rs:125` `.with_quic()`:** default quinn transport
  config — no raised stream limit, no send window tuning.
- **Hot vs sync pull asymmetry (`crates/torus-consensus/src/app.rs`):** the **sync**
  path `validate_block_for_sync` (L1340) calls `pull_compact_bodies_if_missing` (L833)
  → chunked pull. The **hot** path `validate_block` (L1157) keeps only a bounded local
  retry and **never triggers a network pull** (mem 8ee99db3) — by design, to keep the
  ~500 ms view timeout. So when push fails live, the hot path can't recover the body.

## Goal & non-goals
- **Goal:** the chain stays live (committed height advances) under bs≈1000 / >4 MB
  pre-proposal batches — no permanent wedge, graceful throughput degradation at worst.
- **Non-goal:** maximizing throughput/orders-per-sec (that's a separate perf track).
  #4 is a **liveness** fix, like the bigbody fix.

## Options

### Option A — Hot-path pull-fallback (the safety net; reuses bigbody pull)
Give the hot `validate_block` path a **tightly-bounded, non-view-blocking** native-DA
pull when a body is missing, instead of only the local retry. Reuses the chunked
fetch + by-hash absorb already built (bigbody Tasks 2–3). The key: it must NOT block
past the view timeout — either (a) a short bounded poll (≪500 ms) then fail the view
(re-proposed next view, by which time the body has likely arrived), or (b) fire the
fetch async and vote in a later view once present.
- **Files:** `app.rs` (hot-path pull seam), `bridge.rs`/`swarm.rs` (none — reused).
- **Pros:** directly closes the gap the bs-sweep exposed (0 hot-path pull → wedge);
  reuses proven pull infra; makes push reliability non-critical for liveness.
- **Cons:** must respect the view timeout (async/bounded design needed); pull is
  fallback-only so common case must still be push.
- **Effort:** Small–Med · **Risk:** Med (hot-path timing).

### Option B — Push backpressure + bounded in-flight streams
Cap concurrent in-flight `direct.send_request`s (e.g. a semaphore / bounded queue),
await/drain before opening more, so the push loop can't exhaust the 100-stream window.
- **Files:** `swarm.rs` (`BroadcastNativeActions` loop), maybe a small in-flight tracker.
- **Pros:** removes the `max sub-streams reached` root cause; small, contained.
- **Cons:** slows push under burst (throughput trade-off); doesn't help the >4 MB
  direct-cap rejection (needs chunking or the bigger native-DA codec).
- **Effort:** Small · **Risk:** Low–Med.

### Option C — Raise quinn `max_concurrent_bidi_streams` (quick mitigation)
`bridge.rs:125` → `.with_quic_config(|cfg| …)` to raise the per-connection stream limit
(and tune send windows). Pure config.
- **Pros:** trivial; buys headroom immediately.
- **Cons:** just moves the ceiling; unbounded loop still bursts; memory pressure from
  many concurrent big streams; doesn't fix the >4 MB direct-cap rejection.
- **Effort:** XS · **Risk:** Low (but incomplete).

### Option D — Gossipsub for native-action dissemination (robust end-state)
Replace per-validator unicast push with a **gossipsub publish** of the action batch
(one publish, mesh fans out) — no per-validator stream loop at all. Aligns with
`docs/plans/mempool-gossip-hash-proposals-impl.md`.
- **Pros:** eliminates the stream-exhaustion class entirely; scales to N validators;
  dedup/relay handled by the mesh.
- **Cons:** largest change; gossipsub message-size limits still apply (chunk or
  hash-then-pull large batches); re-tests dissemination semantics.
- **Effort:** Large · **Risk:** Med–High.

## Recommendation — phased
1. **Option A (hot-path pull-fallback)** first — it is the liveness fix: even with the
   push path untouched, a missing body is recoverable live, so bs≈1000 stops wedging.
   Smallest path to un-wedge, reusing the bigbody pull.
2. **Option B + C** next — bound the push loop and raise the stream window so push stops
   self-exhausting and the pull stays rare (push is the fast common case).
3. **Option D (gossipsub)** as the durable end-state if unicast push still limits scale
   after 1–2; can be its own follow-up (#5).

Ship 1 alone to restore bs≈1000 liveness; 2 to restore throughput; 3 for scale.

## Open questions
1. **Hot-path pull timing:** bounded-block (how many ms before failing the view?) vs
   async-fetch-then-vote-later. → lean async / very short bound to protect the view timeout.
2. **Push payload >4 MB:** chunk the push envelope too, or route oversized batches
   straight to "hash-only push + pull"? → likely hash-only-push for big batches once A lands.
3. **Stream-limit value (C):** fixed (e.g. 512) vs computed from validator-set size × headroom.
4. **Gossipsub size cap (D):** publish hashes (compact) + pull bodies, mirroring CompactBlock.

## Verification
Re-run the live (or local-devnet) bs-sweep: bs=1000 must keep committed height
advancing (no permanent wedge), recovering after load stops. Keep `four_node_consensus`
and the bigbody suites green.
