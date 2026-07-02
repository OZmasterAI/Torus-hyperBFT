# Design: Block-Tree Pruner + Blockspeed Quick Wins

Session 391 (2026-07-02). Root-cause evidence: mem `d0c25c6c5c77d162` (cf_consensus_meta
never pruned, 244.5MB = ~50% of live DB at h~548k, ~7 permanent keys/block, hot-path reads
every view) and mem `48c2c2463263c8ae` (ranked blockspeed levers).

## Problem

1. Block time degrades ~+45%/50k blocks on every node: hotstuff's block-tree KV
   (`cf_consensus_meta`) grows forever (BLOCKS + BLOCK_AT_HEIGHT + BLOCK_TO_CHILDREN per
   committed block, `variables.rs:138-170`), outgrew the 256MB shared block cache, and is
   read on every view.
2. Three latency cliffs independent of height: (a) native-DA reconstruct sleep-polls
   20ms ticks inline on the consensus thread (`app.rs:656-668, 1041-1109`); (b) body-fetch
   retries wait 300ms × 3 on the proposer before rotating (`implementation.rs:1771-1777`);
   (c) `gossipsub_heartbeat_ms` config is dead — `behaviour.rs:54` hardcodes 500ms.

## Constraints (from memory)

- `validate_block` linear fast-path is LOAD-BEARING (mem `0efcef9de231cc5a`): do NOT move
  validation off the consensus thread or defer execution paths — only change the *waiting*
  mechanism inside the existing budget.
- `ChannelNetwork::request_block_data` is a synchronous test shortcut (mem `84c6ec741eb2ea6a`):
  body-retry changes must stay at the hotstuff tracker level, not the network trait.
- Hot-path budget invariant: local retry + hot pull ≤ 260ms < 500ms view timeout
  (`hot_pull_budget_under_view_timeout` test).

## Task 1: Block-tree pruner

### Option A (chosen): bounded prune-on-commit inside `BlockTreeSingleton::update()`
After the main `self.write(wb)` in `update()` (internal.rs:339), when blocks were committed,
run `prune_old_committed_blocks()` in a **separate** write batch: advance a persisted
`BLOCK_TREE_PRUNED_HEIGHT` pointer (new variable key `[22]`), deleting up to
`PRUNE_BATCH_MAX = 64` heights per commit where `height <= highest_committed - retention`.
Per height: `BLOCK_AT_HEIGHT[h]` -> hash, then reuse existing primitives — `delete_block`
(5 fields + data), `delete_children`, `delete_pending_app_state_updates`,
`delete_block_validator_set_updates` — plus delete the `BLOCK_AT_HEIGHT[h]` key itself.
Retention reaches hotstuff via a crate-level static setter
(`set_block_tree_retention(Option<u64>)`), mirroring the existing
`set_reputation_leader_selection` precedent; torus-node wires it from the same
`--retention-blocks` flag that gates the state pruner (one knob, aligned semantics:
"how far back this node serves history"). Floor: retention is clamped to >= 1000 so the
uncommitted tail / speculative window / sync-trigger lookbacks are never touched.
NEVER touched: LEADER_REPUTATION, SPECULATIVE_COMMITS, EQUIVOCATION_EVIDENCE, all
singletons.

- Trade-offs: deterministic, single-writer (no race with consensus), self-throttling
  (64 heights/commit drains a 448k backlog in ~30-60 min); adds ~10 point-deletes/commit
  to the consensus thread (micro vs the read-amp it removes). Separate batch means a
  crash between commit-write and prune-write only re-prunes later (idempotent).
- Effort: Medium. Risk: Medium (sync-serving behavior below).

### Option B (rejected): background pruner thread in torus-node over RocksKVStore
Off the consensus thread, but violates hotstuff's single-writer assumption, races the
sync server's reads, needs its own view of "highest committed", and duplicates key-layout
knowledge outside hotstuff_rs.

### Sync-serving consequence (both options)
A peer requesting blocks below the pruned horizon gets an empty/failed response and
rotates to another peer (existing blacklist/rotation). Operators keep >=1 archive node
(no `--retention-blocks`) as the genesis-sync source — same operational contract the
state pruner already established. Log once per pruned request at debug.

## Task 2a: gossipsub heartbeat rewire (Trivial)
`TorusBehaviour::new/with_limits` take `heartbeat_ms: u64`; extract
`fn gossipsub_config(heartbeat_ms) -> gossipsub::Config` (unit-testable via
`Config::heartbeat_interval()` accessor). `bridge.rs:194` passes
`config.gossipsub_heartbeat_ms`; `swarm.rs:1798` likewise from its NetworkConfig.
Default stays 100 (config.rs:78) — the intended value; 500ms was the drift.

## Task 2b: reconstruct wake-on-arrival (Small-Medium)
Add a process-wide arrival notifier to `NativeDaStore` (torus-state/native_da.rs):
`static ARRIVALS: OnceLock<Arc<(Mutex<u64>, Condvar)>>`; `put()` bumps the generation +
`notify_all`. New `wait_for_arrival(seen_gen, timeout) -> u64`. In
`reconstruct_native_actions_hot` and the bounded pull loop, replace
`std::thread::sleep(20ms)` ticks with `wait_for_arrival(gen, 20ms)` — identical worst-case
budget (260ms invariant preserved), but the common push-race case wakes in ~0-5ms instead
of a full 20ms tick (and multi-tick waits collapse to actual arrival). The 20ms slice is
kept (not one long wait) because hot-pull responses land in the fetcher inbound and are
only absorbed into the store by this thread's `absorb_fetched_bodies` between slices.
Global static is justified: NativeDaStore instances are constructed independently over
one DB (`native_da.rs:26`), so an instance-local notifier would miss cross-instance puts.

## Task 2c: body-retry faster rotation (Small)
`BODY_RETRY_INTERVAL` 300ms -> 100ms; `MAX_BODY_RETRIES` (proposer-only attempts) 3 -> 2;
`MAX_BODY_RETRIES_TOTAL` stays 9. Worst wait before first alternate target: 900ms -> 200ms.
Bounded extra chatter: at most the same 9 total requests, just denser. Rotation logic
(`rotated_body_fetch_target`) unchanged.

## Recommendation
Option A pruner + all three quick wins, one commit per task, TDD-first.

## Verification
- Unit/integration: new pruner tests (old blocks gone, recent retained, safety vars
  intact, commit works across the pruned horizon, disabled-by-default = archive).
- Existing suites must stay green: hotstuff_rs, consensus (incl.
  `hot_pull_budget_under_view_timeout`), four_node, mempool, network.
- Live: devnet soak with small retention; on testnet expect `cf_consensus_meta` compacted
  size to plateau then shrink, `torus_view_duration_seconds` flattening vs height.

## Open questions
- Whether to also `compact_range` cf_consensus_meta after the catch-up drain (tombstone
  read-amp is transient; RocksDB compaction will get there — revisit if the live drain
  is slow to reflect in view timings).
