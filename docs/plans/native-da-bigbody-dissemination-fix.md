# Design: Native-DA Big-Body Dissemination Fix (un-wedge >4 MB blocks)

**Created:** 2026-06-08 · **Status:** DESIGN (awaiting option pick) · **Branch:** TBD (off `phase-a-incremental-root`, NOT on the live testnet)

## Problem
Blocks whose serialized native-action body exceeds the **single shared 4 MB codec cap**
(`MAX_DIRECT_MSG_SIZE`, `crates/torus-network/src/codec.rs:23`, shared by `/torus/direct`,
`/torus/block-data`, `/torus/native-da`) cannot be disseminated by **either** body path:

- **PUSH** (`BroadcastNativeActions`, `swarm.rs:994`): the 4.17 MB payload is over the 4 MB codec; and
  it opens one new QUIC stream per validator in a tight loop with no backpressure, exhausting quinn's
  default `max_concurrent_bidi_streams=100` → `max sub-streams reached`.
- **PULL** (`/torus/native-da/1.0`): the ~4.4 MB response is silently dropped by the same 4 MB cap
  (`codec.rs:86`); and the consensus pull budget is only **80 ms** (`PULL_RETRIES=4 × PULL_DELAY=20ms`,
  `app.rs:859`) — far too short under load.

Result: validators can't reconstruct the body → can't vote → chain wedges (committed height frozen,
`block_sync` loops forever returning unexecutable blocks). Observed at bs=1000 (4.17 MB blocks);
bs=500 (~2 MB) is fine. **LIVENESS bug only** — a body-less block never reaches 2f+1 votes, so it is
never committed (committed state stays clean; no rollback needed).

## Context (memory + investigation)
- Phase C (CompactBlock + native-DA) moved the wall 256 KB → 4 MB, but the **pull-DA fallback shares
  the same cap**, so it cannot rescue exactly the blocks that exceed it.
- The proposer **does** mirror its body to the durable DA store at propose time (`app.rs:1086`), so the
  body *is* available to serve — the failure is purely transport (codec cap) + budget, not a missing body.
- Caps today: `NATIVE_TOTAL_BLOCK_CAP=100` actions, `NATIVE_ORDERS_PER_BATCH_CAP=1024`,
  `NATIVE_ORDERS_PER_BLOCK_CAP=50_000` (`crates/torus-mempool/src/rate_limit.rs`).

## Options

### Option A — Bigger caps only (quick mitigation)
Per-protocol codec caps (native-DA / block-data sized to the real max body) + widen pull budget. No chunking.
- **Files:** `codec.rs`, `app.rs`
- **Pros:** smallest change; unblocks blocks up to the new cap fast.
- **Cons:** just moves the ceiling; single huge message = memory spike + all-or-nothing; re-wedges past the new cap.
- **Effort:** Small · **Risk:** Low–Med

### Option B — Chunked pull-DA (robust, no ceiling)
Native-DA client requests missing bodies in size-bounded **chunks** (split the hash list so each response
stays under a safe size), reassemble; widen pull budget. Codec cap stays modest.
- **Files:** `bridge.rs` (chunked fetch), `swarm.rs` (server already serves partial hash lists), `app.rs`
- **Pros:** no hard ceiling; bounded memory; partial progress; scales with block size.
- **Cons:** more logic (chunk loop + reassembly + missing-chunk handling); a few more round-trips.
- **Effort:** Medium · **Risk:** Medium

### Option C — Chunking + per-protocol cap headroom + budget (recommended)
Option B's chunking as the mechanism, **plus** a generous per-protocol native-DA codec cap as headroom
(so a chunk + framing always fits), **plus** widened pull budget. The robust end-state: pull-DA reliably
reconstructs an any-size body in bounded pieces.
- **Files:** `codec.rs`, `bridge.rs`, `swarm.rs`, `app.rs`
- **Pros:** removes the ceiling (not just raises it); makes pull-DA the reliable big-body path (the stated
  goal); each piece small and testable; fixes the wedge even with PUSH (#4) untouched.
- **Cons:** most code of the three (still contained).
- **Effort:** Medium · **Risk:** Medium

## Recommendation
**Option C.** It delivers the "pull-DA is the reliable big-body path" end-state and removes the hard
ceiling rather than relocating it. Because a reliable pull means validators always reconstruct the body
regardless of push, this fixes the liveness wedge with the PUSH sub-stream fix (#4) deferred. Pick
**Option A** only if a same-day mitigation is wanted before the full fix.

## Open Questions
1. **Chunk sizing:** fixed (≤2 MB/response) vs computed from `max_action_size`? → recommend computed, with margin.
2. **Codec cap value:** fixed (16 MB) vs computed from `NATIVE_ORDERS_PER_BLOCK_CAP × max_action_size`? → recommend computed.
3. **Hot path vs sync path:** the steady-state hot path (`implementation.rs:844`) relies on the pre-proposal
   PUSH having delivered the body; only the **sync** path pulls. Scope #1 to the sync-path pull (fixes the
   wedge + recovery); add a hot-path pull-fallback as part of #4? → recommend yes, defer hot-path pull to #4.
4. **Pull budget values:** retries / delay / backoff (target ~1 s total, off the hot path).
