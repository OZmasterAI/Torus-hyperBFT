# Blockspeed + Orders/s Roadmap — open items as of S405 (2026-07-05)

Phase 0 (mesh watchdog + propose_delay) COMPLETE — S405 delivered the
watchdog/explicit-peers stack (devnet-proven: base wedges 8/10 unhealed, fixed
0/10) and went past diagnosis on propose_delay to the root-cause fix:
`select_leader` was O(view % total_power) — the height drag. Closed-form
O(n log n) replacement committed with an exhaustive equivalence proof
(consensus-identical, deployable per-node). All pushed:
`sprint/blockspeed-orders-s395` @ 5828ec2.

Testnet state: STOPPED 2026-07-05 20:43Z at height 1,114,439 / view 1,198,460
(seed clean SIGTERM exit, val1 systemd inactive, val3 idling without quorum).

---

## Phase 0 — carry-over items (not code)

- [ ] **Relaunch testnet + live leader-fix verification.** Decide build:
  cherry-pick WITHOUT explicit-peers (576d3be) if val3 stays old, or full
  branch head on all three if val3 upgrades in the same window (preferred —
  lights up explicit peering fleet-wide). Then ONE 150s histogram window:
  prediction is seed propose_delay ~240ms → tens of ms at view ~1.2M. If it
  doesn't collapse, revert is one commit per node.
- [ ] **val3 operator upgrade** — point him at `sprint/blockspeed-orders-s395`
  head (5828ec2); refresh docs/val3-upgrade-instructions-s392.md accordingly.
- [ ] *(Optional, deferred)* **Memtable-fill sawtooth**: bounded ±tens-of-ms
  oscillation from 128MiB-buffer CFs (headers/bodies) filling between flushes
  — RocksDB InlineSkipList read cost. Only revisit if post-deploy live legs
  show it matters. Same fix pattern as the committed cf_consensus_meta 8MiB
  change (7e67bd1).
- [ ] **SIGTERM-hang** repro (wedge-frozen node ignores SIGTERM) — S405 data:
  healthy seed exited <5s, supporting wedge-specific hypothesis. Repro on
  devnet via wedge-repro-s405.sh base build if needed.

## Phase 1 — exec-side orders/s wins (devnet-only; testnet-down does NOT block)

- [ ] **O1 margin caching** (~1.6× orders/s expected). Per-block
  sender-balance/position cache in Phase-2 margin (prod cost ~13µs/order, top
  exec cost). NEEDS SHORT PLAN first — correctness subtlety: balances/positions
  mutate as orders settle within the block, so it must be a write-through
  overlay, not a read cache. Bench: `exec_phase_margin_seconds` phase table
  under native-order-flood, before/after on devnet. Baseline mem: 2484d5d60814db2b
  (engine ~28µs/order, ceiling ~36k/s contended).
- [x] **O3 trade-history CF off critical path** — DONE S414 (commit 1a51d1e).
  `ctx.defer_trades` buffers fill KVs; `torus_state::BackgroundCfWriter`
  (ExecutionContext-owned, drain-on-drop = shutdown flush ordering) writes
  them off the exec thread; sync fallback if the writer dies. Micro-bench
  (exec_batch_trades.rs, 200-fill block): 2.15ms → 1.30ms median (−39%),
  non-overlapping CIs. Live A/B (exec_flush_seconds tail +
  torus_trade_writer_queued_batches gauge) joins the O1 proof at relaunch.
  Known window: hard crash loses queued batches (cosmetic RPC history gap,
  replay-idempotent, never consensus).

## Phase 2 — capacity unlocks (ordered by dependency)

- [x] **O5 feed/dissemination gates** — DONE S415 (code 97b931b; plan+outcome
  docs/plans/o5-feed-gates-impl.md). Accept-first ladder: caps.rs single
  source of truth + test-enforced ordering; consensus accept 256KB→1MB
  (closes the EVM-inline livelock trap — a LEGAL 319.8KB compact proposal
  exceeded the old gate → drop+penalize loop; red→green proven); direct read
  4→8MB with LEGACY_FLEET_DIRECT_MSG_FLOOR=4MB pinning send policy; env
  threshold CLAMPED to floor+WARN (S388's 6MB env > 4MB codec was a live
  silent violation); gossip transmit config-wired (S391 drift class).
  Devnet bs400 sweep ×2 (order-reversed, 0 wedges): 512KB manifest+pull
  11.4k orders/s STABLE vs 3MB full-push 0.5-4.5k with 6-14× more fallback
  pulls — "512KB too eager" FALSIFIED on loopback (off-loop serving made
  pull the fast path). Compiled default stays 512KB. → RELAUNCH follow-ups:
  seed-only WAN A/B 512KB vs clamped-4MB; likely DROP the 6MB env from
  seed+val1 (it forces 2.8MB WAN pushes and now clamps anyway).
- [x] **O2 PlaceOrderBatch** — CORRECTION: feature SHIPPED 2026-06-06 as Phase B
  B1–B5 (b58f858) — one ecrecover + one manifest entry per batch; the old entry
  ("NEEDS WRITING-PLANS") was stale (premise fix: mem 27a66377be573bc0). S416
  close-out (design docs/plans/o2-placeorderbatch-design.md, impl
  docs/plans/o2-placeorderbatch-impl.md): deterministic exec-side batch cap
  (skip-wholesale at the execute_batch flatten, G1) + gossip/DA-admit checks;
  order-aware selection budget (order_count → NATIVE_ORDERS_PER_BLOCK_CAP, G2);
  SessionScope::Trading += PlaceOrderBatch (G3, LOCKSTEP: whole fleet before
  clients sign Trading-scoped batches); partial-per-order contract pinned by
  test (G4); PlaceOrderBatch golden vectors multi+single (G5 — TS parity in
  torus-trading-app is an external follow-up); exec_place_batch criterion
  bench + BS={1,100,400,1024} sweep script (G6,
  devnet/sweep-o2-batchsize-s416.sh).
  **S419 SWEEP VERDICT (devnet 1-box, 512KB threshold, 4-market genesis, all
  legs wedged=0):** sequential 4-leg orders/s|block_ms_fit|native/blk|pulls:
  bs1 293|468|89|70 · bs100 11,213|443|84|26 · bs400 7,405|492|43|55 ·
  bs1024 2,223|406|16|7. Single-first-leg trials at bs400 (clean unit,
  n=4 interleaved incl. semantic-noop control): 10,967–13,312 — batching
  lifts throughput ~38x (293 → 11.2k) to the 1-box exec ceiling; beats the
  S415 baseline (11,388). Strict bs1→400 monotonicity fails only as a
  same-ceiling technicality in the sequential run (leg-position drag);
  single-leg runs are the trustworthy protocol for devnet perf claims.
  Two harness bugs found+fixed en route: compose-fallback
  TORUS_PUSH_THRESHOLD=6MB (sweep now exports 524288) and genesis seeding
  only market 1 (now 1–4; pre-O2 runs rode phantom auto-created books).
  WATCH-ITEM for testnet relaunch: ~2/7 O2-head devnet runs hit a
  stochastic body-miss death spiral (0–1.5k, MissingData view churn,
  nonce mass-eviction); never reproduced in clean single-leg trials,
  pre-O2 immunity unproven (n=3). bs1024 degradation = order-budget trim
  (48/blk) + 4x oversubmission, by design.

## Phase 3 — load-tail levers (measurable only with Phase-2 traffic)

- [ ] **BS-4a native-DA reconstruct off-thread** — cache-miss stall (local
  retry + 8×20ms pull budget, ≤~160ms) runs on the single consensus thread
  (app.rs reconstruct_native_actions_hot). Short plan when reached; p99 lever
  under heavy body traffic; invisible on empty chain.
- [ ] **BS-4b body-retry event-driven** — interval already 100ms×2+rotation
  (mostly landed pre-S405); remaining tail trim is small. Piggyback commit
  whenever in that file.

## Phase 4 — the big rock

- [ ] **O6 incremental state root** — O(total state) per block on the hot
  path is the LAST unbounded-growth term in the system now that select_leader
  is fixed; becomes dominant as real order flow grows state. Plan exists:
  prompts/incremental-state-root-phase-a-prompt.md +
  prompts/finish-phase-a-prompt.md. Own branch, multi-session, merge only when
  Phase A determinism proven.

## Standing constraints / reminders

- Explicit peers (576d3be) is RECIPROCAL-ONLY: never long-lived on a mixed
  fleet — val3 must carry it before/with seed+val1.
- Restart protocol: stop → wait 20s → start; check views/block before trusting
  any perf number; record mesh mode with every rate measurement.
- Devnet policy: up only while actively testing, down after.
- Quorum is 3-of-3 — any single node stop halts the chain; 4th validator
  remains the structural fix (also stops qc_collect tracking the slowest).
- Slope measurements: fit regressions over the series, never endpoint-to-
  endpoint (S405 lesson — minute-to-minute noise is ±10ms).
