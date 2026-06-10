# Implementation Plan: Sprint 3 — Gossip Pre-Spread (enable + harden)

**Design:** docs/plans/sprint3-gossip-prespread.md · **Status:** implemented s334

## Tasks (as executed, TDD)

### T1: Enable switch
- `torus-node` CLI: `--native-gossip` (bool, default **true**) →
  `mempool.set_native_gossip_enabled(cli.native_gossip)` right after
  `set_native_gossip_tx` (main.rs). Log line records the state at boot.
- Existing pipeline (already in tree, previously dormant): mempool
  `gossip_native_action` post-admission → bounded channel → swarm batched
  publish on `NATIVE_ACTION_TOPIC` → peer inbound → node ingest task.

### T2: Verified gossip ingest
- Test: `gossip_ingest_verifies_claimed_sender` (torus-mempool) — forged
  `(sender, action)` rejected from pool but still DA-mirrored; genuine sender
  admitted.
- `Mempool::add_native_action_from_gossip`: DA-mirror first (availability ≠
  validity), then Eip712 `recover_sender()==claimed` / Session
  `verify_session_signature()` + `state.get_session().owner==claimed`, then the
  pre-existing nonce gates + admission. Node ingest task switched to it.

### T3: Rotated body-fetch targets (hotstuff_rs)
- Tests: `origin_first_then_rotate_through_others`, `no_others_means_origin_only`.
- `rotated_body_fetch_target(attempt, max_origin_retries, origin, others)` —
  proposer for the first 3 attempts, then round-robin over the other committed
  validators. `tick_pending_body_retries` now takes `&BlockTreeSingleton<K>`
  (caller passes `self.block_tree`), `MAX_BODY_RETRIES_TOTAL = 9` before the
  sync fallback.

### T4: Byte cap raise
- `NATIVE_BLOCK_BYTES_CAP` 2MB → 6MB (rate_limit.rs) — proposal-time transfer
  is now ~hashes + gap-pulls; 2MB was the push-only budget that pinned s334
  throughput at 34.5k orders/s.

### T5: Prove
- All touched crates green; release build; deploy ours + friend1; dual-box
  sweep bs500 + bs1000 (smallserver senders 10–19 → friend1 localhost; ours
  0–9 → localhost; submit-batch 10). Pass: >50k orders/s, bs1000 no collapse,
  no stall, gossip visible in logs (`published native action batch`).

## Rollback
`--native-gossip=false` + revert the cap const; T2/T3 are strict hardenings.
