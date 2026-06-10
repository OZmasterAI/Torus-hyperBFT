# Design: Sprint 3 — Native-Action Gossip Pre-Spread (enable + harden)

**Session:** 334 · **Branch:** `fix/native-da-push-hardening` · **Status:** DESIGN

## Problem
Post-Sprint-2 the chain is byte-budget-bound: 34.5k orders/s ≈ NATIVE_BLOCK_BYTES_CAP
(2MB) × ~1.8 blk/s, because ALL body bytes still cross the wire at proposal time
(s297 locked gossip OFF; bodies move proposer→peers via push/manifest-pull only).
Raising the cap without pre-spreading bodies recreates the bs1000 wedge.

## Discovery (exploration)
The gossip pipeline ALREADY EXISTS end-to-end, switched off:
- `NATIVE_ACTION_TOPIC` subscribed (swarm.rs:333); outbound batching publish loop
  (swarm.rs:447-491, `NATIVE_BATCH_MAX_SIZE`, bounded 8192 channel).
- Mempool publishes `(sender, action)` bincode post-admission
  (`gossip_native_action`, lib.rs:319) — gated on `native_gossip_enabled`
  (AtomicBool, default false, **zero callers** of the setter).
- Inbound: gossipsub → `native_action_inbound` channel → node task →
  `add_native_action_from_gossip_trusted` (main.rs:427) which **DA-mirrors
  FIRST** (livelock lesson pre-applied, lib.rs:300) then nonce-gates + admits.
- Native-DA pull-fallback already fans out every chunk to EVERY validator
  (bridge.rs:267).

Remaining gaps:
1. Nothing enables the flag (the s297 mesh-flooding concern predates compact
   proposals + bounded/batched publishing; calculus has changed).
2. Inbound is signature-TRUSTED: a peer could gossip forged `(sender, action)`
   pairs into pools (testnet-tolerable, but cheap to fix: 1 ecrecover at ingest
   rate is trivial vs the 20k/s scenario "trusted" was built for).
3. hotstuff body fetch (`tick_pending_body_retries`, implementation.rs:1775)
   only ever re-asks `origin` (the proposer) — 3×300ms then sync fallback. A
   non-serving proposer (s334 executor livelock, friend2 zombie) defeats it
   even when other validators hold everything.

## Decision
1. **T1 Enable**: `--native-gossip` CLI flag on torus-node (default ON),
   wired to `set_native_gossip_enabled`. Validator-local; mixed-version safe
   (friend2 without it just doesn't gossip).
2. **T2 Verify-at-ingest**: new `Mempool::add_native_action_from_gossip`
   — DA-mirror first (unchanged), then `recover == claimed sender` check,
   then nonce gate + pool admission. Forged sender ⇒ pool-rejected (body may
   stay in DA: availability ≠ validity; reconstruction needs bytes regardless).
3. **T3 Rotate body-fetch targets**: after `MAX_BODY_RETRIES` from origin,
   retry remaining attempts against the OTHER committed validators
   (one per tick, rotating) before declaring sync_needed.
4. **T4 Raise cap**: `NATIVE_BLOCK_BYTES_CAP` 2MB → 6MB once T1-T3 are in
   (bodies pre-spread at ingress; proposal-time transfer ≈ gap-pulls only).
5. **T5 Prove**: deploy both nodes; dual-box sweep bs500 + bs1000.
   Expectation: >50k orders/s at bs500-1000; bs1000 no longer collapses
   (bodies already local when manifests arrive).

## Rollback
Flag off (`--native-gossip=false`) restores today's behavior; cap revert is
one const; T2/T3 are strict hardenings.
