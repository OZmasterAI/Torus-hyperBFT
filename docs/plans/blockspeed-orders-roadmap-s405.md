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
- [ ] **O3 trade-history CF off critical path** (small, ~5–10% + tail
  smoothing). CF_NATIVE_TRADES / CF_USER_TRADES are node-local (not in root
  CFs) → background writer. Quick-fix loop, no plan; care point = shutdown
  flush ordering. Same bench harness as O1.

## Phase 2 — capacity unlocks (ordered by dependency)

- [ ] **O5 feed/dissemination gates** — reconcile max_consensus_message_size
  256KB vs gossip 2MiB vs direct 4MB so bs400+ blocks disseminate (today
  512KB manifest vs 2.8MB bodies self-limits inclusion). NEEDS WRITING-PLANS:
  consensus-facing size caps = mixed-fleet compat + rollout ordering (same
  care class as explicit peers).
- [ ] **O2 PlaceOrderBatch** — one signed action carrying Vec of orders: one
  ecrecover + one manifest entry per batch. Collapses the ~1700-orders/block
  wire cap and per-order sig cost; prerequisite for 20k-order blocks (200k/s
  target, mem 9af8b543816e63f7). NEEDS WRITING-PLANS: EIP-712 schema, executor
  path, atomic-vs-partial batch failure semantics, RPC surface. Depends on O5
  to be benchable end-to-end.

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
