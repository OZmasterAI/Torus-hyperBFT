# Design: Native-Action Data-Availability (DA) Layer

**Branch:** suggest `phase-c-native-da` off `cap100-3val-perf` · **Created:** 2026-06-07 · **Status:** BRAINSTORM
**Goal:** Reliable out-of-band delivery of native-action bodies so the chain sustains **400k+ orders/sec** (beat Hyperliquid) without wedging. The first increment also un-wedges the current testnet livelock (height 151859).

## Problem
Native orders are the throughput product. At 400k orders/s and sub-100ms blocks, a block references ~20–40k orders ≈ **1–2 MB of bodies — 4–8× over the 256 KB consensus message limit** (`max_consensus_message_size`, config.rs:79). So bodies cannot ride in the consensus proposal/sync message; the block carries only action *hashes* (`CompactBlock`, torus-types/lib.rs:429) and bodies disseminate out-of-band. The current out-of-band path is unreliable and is livelocking the chain:

- Bodies live only in an **ephemeral, RAM-only** mempool (`native_pool.rs`, 65 536 cap, no durability, lost on restart). No persistent CF exists.
- The only delivery is a **fire-and-forget unicast "pre-proposal push"** (bounded 4-slot channel, drops under load; swarm.rs `BroadcastNativeActions`); gossip is disabled (it drowned consensus).
- A **60 s nonce-staleness gate** (`NONCE_WINDOW_MS`, eip712.rs:27) blocks bodies from (re)entering the store on *every* insert path — so a missing body can **never** be recovered once stale.
- On a missing body, sync **REJECTS the block and BLACKLISTS the peer** (block_sync/client.rs:333) → peer-set exhaustion; commit **early-returns before advancing `last_header`** (app.rs:1135) → app/RPC height freezes. Net: consensus livelock (root cause verified, mem `28e1a821`).

## Context — dissemination history (don't repeat it)
Three prior implementations (mem `a6cf33a9`):
1. **Header-only + on-demand body fetch** (~May17): ABANDONED — per-block fetch round-trip caused a **46→211 ms latency regression**. → **Lesson: pull must NOT be per-block.**
2. **Full-block proposals** (06620a1 / 66da812): current stopgap; reliable but **caps at 256 KB → dead-end for 400k**.
3. **CompactBlock + unicast push** (865d4e1): shrinks proposals (needed for 400k) but hits missing-action stalls = the current livelock.

Session 297 **LOCKED Option D: harden CompactBlock delivery, gossip off** (mem `c19a35aa`) — reliable push *primary*, fetch *rare* (not per-block); T2–T5 were built but it was reverted to full-block to stop the bleeding, and durability + fallback-fetch were **deferred**. The livelock proves those deferrals are the gap.

**Reusable infra already built:** `/torus/block-data/1.0` request_response (codec.rs) for on-demand block-body fetch; header-first proposals; `/torus/direct/1.0` unicast. Adding a `/torus/native-da/1.0` request_response is a clean parallel — **no hotstuff_rs changes** required.

## Options

### Option A — Harden push only (finish s297 Option D as scoped)
Make the unicast pre-proposal push reliable: stop fire-and-forget, queue+retry when a validator isn't yet in the peer map, fix proposal-publish drops, keep the 100 ms retry safety net. No durable store, no pull-fetch.
- **Files:** torus-network/swarm.rs (push queue/retry, drop fixes), torus-consensus/app.rs (retry).
- **Pros:** smallest delta; partly built (T2–T5); no new protocol.
- **Cons:** push stays best-effort — a lost/late push under 400k + WAN + all-3 quorum still stalls; no durability (restart loses bodies); doesn't fix the blacklist/freeze footguns or recover stale bodies. History shows push-only is fragile.
- **Effort:** Small · **Risk:** Medium (may not hold at 400k).

### Option B — Durable DA store + hardened push-primary + RARE pull-fallback ★
The complete realization of Option D, with the durability + fallback the livelock proved necessary.
- **Durable body store** keyed by action-hash (new `CF_NATIVE_PENDING` or dedicated DA store), **decoupled from the 60 s nonce gate**, written when a body is seen (RPC/push). Survives restart; a block-referenced body is always recoverable.
- **Push primary (hardened):** proposer proactively pushes bodies (low-latency common case) with reliable queue/retry + reconnect re-push.
- **Pull fallback (RARE, by-hash):** on a miss at validate/commit/sync, fetch *only* the missing bodies via a new `/torus/native-da/1.0` request_response (template: block-data protocol). Push covers the common case → pull is rare → **avoids the May17 per-block round-trip regression**.
- **Remove the footguns:** never blacklist a peer for a body-less block (fetch instead); never freeze `last_header` on a committed block (fetch-or-fail-loud).
- **Files:** torus-state/cf.rs (+CF), torus-mempool (durable mirror + decouple nonce gate for DA inserts), torus-network (new behaviour + codec + commands, swarm.rs), torus-consensus/app.rs (fetch-on-miss in validate/commit; drop redundant double-send), hotstuff_rs/block_sync/client.rs (no blacklist on missing body).
- **Pros:** reliable (push + pull + durable) → kills the livelock; compact proposals → scales to 400k; faithful to s297 D + adds what was missing; rare-pull dodges the latency trap.
- **Cons:** most work; new protocol + CF; hot-path durability write (mitigate with async mirror).
- **Effort:** Large · **Risk:** Medium (well-understood; incremental + testable).

### Option C — Full DA layer (erasure-coded / sampled; Celestia/Avalanche-style)
Redundant, sampled data-availability with erasure coding.
- **Pros:** most scalable/robust; future-proof beyond 400k.
- **Cons:** large, complex, premature for a 3-validator set; over-engineered now.
- **Effort:** XL · **Risk:** High · **Defer.**

## Recommendation
**Option B.** It is the only path that is both *reliable* (kills the livelock) and *compatible with 400k* (compact proposals stay tiny while ~MB of bodies move out-of-band). It completes the s297-locked Option D and adds the durability + rare pull-fallback the livelock proved necessary, while explicitly avoiding the known per-block round-trip regression (pull is fallback-only). Its **first increment — durable store + fetch-on-miss + remove blacklist/freeze — un-wedges the current testnet**, so nothing is throwaway.

## Open Questions (resolve in writing-plans)
1. **Durability shape:** new RocksDB CF (survives restart, hot-path write) vs larger in-memory + async durable mirror? (Lean: in-mem primary + async CF mirror.)
2. **Nonce-gate decoupling:** let a block-referenced body bypass the 60 s gate without opening a spam vector — gate RPC *admission*, not block-referenced DA inserts.
3. **Pull transport:** new `/torus/native-da/1.0` behaviour (clean) vs reuse `/torus/direct` with a marker byte. (Lean: new behaviour.)
4. **Body lifecycle/eviction** in the durable store (evict after commit + N blocks; size cap).
5. **Proposer re-emits CompactBlock** (needed for 400k): version discipline (all validators on the fixed binary) + historic full-block compatibility.
6. **Sequencing vs Phase A** (incremental state-root — the *other* 400k wall, block speed): run in parallel.

## Relationship to prior plan
Supersedes/extends `docs/plans/step3-dissemination-hardening.md` (s297 Option D) — same push-primary/fetch-rare principle, now with durability + fallback + the livelock footgun fixes, scoped to the 400k goal.
