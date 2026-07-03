# Sprint S395 Report — blockspeed, native orders/s, height-drag

Branch: `sprint/blockspeed-orders-s395` (3 commits atop f800bce, deployed to seed+val1).
Commits: `15bc833` (pacemaker fix + cap knobs), `b674376` (per-view floor shaves),
`69f0a99` (order-book persistence + bench `--markets`).

## Goals and outcomes

| Goal | Outcome |
|------|---------|
| 1. Blockspeed ~100ms | **Local floor hit: 102ms/block empty cadence** (9.8 blk/s, 4-val devnet, improved binary). Four wire-safe hot-path shaves shipped. Chain-wide sub-100ms on the geo testnet remains RTT-bound (227–502ms legs), as pre-established. |
| 2. Native orders/s ≥150k | **Not demonstrated — and shown to be currently gated by transport/feed, not execution.** Exec never saturated in any bench leg. Structural fixes shipped (order-book dirty-save, cap knobs); precise 150k roadmap produced with measured per-phase costs. |
| 3. Height-correlated slowdown | **Fix (block-tree pruner f99d07a) deployed; live validation blocked on quorum** — and the sprint discovered the real reason the chain halts at all (below), which supersedes the original validation plan. |

## What shipped (all node-local, wire-safe, tested)

1. **Pacemaker: rebase stale absolute view schedule on forward jumps** (`15bc833`).
   The per-epoch schedule assigns absolute deadlines (`epoch_start + gap × max_view_time`).
   A replica entering a view ahead of schedule (restart TC-jump, fast QC run) inherited a
   far-future deadline and parked — observed live as a ~39k-view intra-epoch jump landing
   hours out. `update_view` now rebases the remaining schedule when the entered view's
   deadline is >2× view time away. Catch-up-from-behind and on-schedule behavior unchanged.
   3 new regression tests (jump, fast-run bound, on-schedule preservation); 24/24 lib tests.

2. **Per-view floor shaves** (`b674376`):
   - Leader DA mirror: one atomic WriteBatch + one condvar wake per block instead of up to
     100 individual RocksDB writes + wakes on the produce_block critical path.
   - `SPECULATIVE_COMMITS`: write-through cache (leader-reputation pattern); was a full
     read-deserialize-rewrite of the whole list **twice per view**; no-op promotes no longer write.
   - Dropped the redundant full signature re-verify of locally-collected QCs (the collector
     verified every vote on entry; kept as `debug_assert`). n wasted ed25519 verifies/QC.
   - Reconstruct/pull waits are now wall-clock-deadline-bounded — scheduler overshoot used to
     stack 13×20ms nominal into 500ms+ under load, eating the whole view timeout on a body miss.

3. **Order-book persistence** (`69f0a99`):
   - `save_order_books` wrote EVERY loaded book every block — O(all resting orders) of Borsh
     per block (~100B/order; 60k resting ≈ 6–9MB/block). Now dirty-markets-only (all six
     mutation sites tracked). Byte-identical final state ⇒ state-root-safe even mixed.
   - `next_global_order_id` now persists durably (24-byte key in `CF_NATIVE_MARKETS`, off the
     consensus root, invisible to all existing readers which guard on 8-byte keys). Previously
     draining all books reset the counter on restart ⇒ order-id reuse. `max(scan, persisted)`
     keeps old DBs loading. Lazy book loading was deliberately NOT done: four all-books
     iteration sites make partial maps consensus-unsafe (Phase 2 needs per-order keys).
   - 3 new persistence tests; full torus-bridge suite (79) green.

4. **Throughput cap env knobs** (`15bc833`): `TORUS_NATIVE_TOTAL_BLOCK_CAP`,
   `TORUS_NATIVE_BLOCK_BYTES_CAP`, `TORUS_NATIVE_PER_BLOCK_CAP` — proposer-local
   (validate_block doesn't reject on count), defaults unchanged; enables per-node A/B without
   rebuilds (S387 cap-probe proved cap=1000 no longer wedges).

5. **Bench tool `--markets N`** (`69f0a99`): spreads orders across market ids per order;
   the old single-market shape skipped `MarketWorkerPool::match_parallel` entirely.

## Major discoveries (each root-caused with evidence)

1. **Why the chain actually halts: `epoch_length = 100` + all-3 quorum.** Testnet genesis
   sets 100-view epochs; every 100th view is an epoch-change view that only advances on a
   TimeoutVote **quorum**. With three equal 2M stakes, quorum = floor(2·6M/3)+1 = 4,000,001 —
   **one power unit more than any two validators**. So 2-of-3 can never cross a boundary: the
   chain mechanically halts ≤100 views (~50s of churn) after losing any validator. The
   long-standing "quorum 2-of-3" belief was off by one. This supersedes prior halt diagnoses
   and makes the "4th validator for f=1" backlog item the structural fix.

2. **Live freeze at view 642500 (both nodes)**: the pacemaker jump-park bug (fixed above)
   plus the epoch barrier. After the fix, seed+val1 churned 642500→642600 in ~50s and now
   sit at each boundary as designed. The chain **crawls one boundary per val3 connect window**
   (642703 at wrap) — proving val3's votes still count: his consensus key is valid even though
   his libp2p identity regenerated (dials to his registered peer id fail "Unexpected peer ID";
   his inbound flickers, cause=None, likely his old build's side).

3. **Devnet↔testnet mesh cross-contamination.** Devnet validator-0 had no `--p2p-peers`, so
   it fell back to the **compiled-in** `TESTNET_BOOTSTRAP_PEERS` (the live seed), pulled live
   kademlia records into the devnet and vice-versa; both meshes dial-stormed each other
   (including devnet containers dialing the host's live node) and the live seed's DB absorbed
   ~308MB of junk. All early devnet bench numbers were invalidated by this. Fixed locally in
   the compose (explicit internal peer); flagged for a proper repo fix.

4. **bs400 inclusion is transport-gated, not exec-gated.** With clean mesh: proposers built
   full 100-action blocks ("selected actions=100"), but 2.8MB bodies exceeded what the mesh
   could disseminate: at the default 512KB threshold bodies go manifest-only and views fail
   while gossip spreads ~62MB; with the live testnet's 6MB full-body push, inclusion got
   *worse* — the full-body pre-proposal bundle appears to exceed a message cap
   (`max_consensus_message_size` 256KB / gossip 2MiB / direct 4MB need reconciling; the live
   68.9k record's blocks self-limited to ~74 actions ≈ 2MB). Exec phases stayed near-idle in
   every leg.

5. **Exec ceiling decomposition kills the "optimize matching" lever.** Matching-engine
   micro-bench (20k orders, 1 market): match = **98.6ms**, margin = 726.7ms, settle = 1699.1ms.
   Production phase tables agree (match ~5%, margin ~13µs/order top exec cost). Single exec
   thread ≈ 36k orders/s on this contended box. The 150k path: per-block sender balance/position
   caching in margin, core pinning (~2x, prior evidence), moving non-root trade-history CF
   writes off the critical path, and batched session verification (session actions verify at
   2–12ms each — never trust-cached — which is also why session-mode load collapses today).

## Live testnet state at wrap

- seed + val1 on final sprint binary (`69f0a99`), healthy, in standby boundary-crawl.
- Full resume (and goal-3 pruner drain validation, DB baseline 528.9MB→837MB incl.
  contamination junk) unblocks automatically when val3 stabilizes/upgrades.
- Nothing pushed to origin (held for friend-upgrade flow, as before).

## Honest misses / caveats

- 150k orders/s was not reached: on one 8-core box carrying 4 validators + bench client the
  feed and transport gates dominate long before exec. The transport cap reconciliation
  (finding 4) is the prerequisite for any bs400+ throughput work, on any topology.
- Goal-3 live drain not yet observed (needs commits; see above).
- 2 pre-existing `dynamic_validators` integration test failures documented (fail identically
  on the pre-sprint tree; governance proposal execution timing).
- One test-suite flake root-caused and fixed for real (deadline-bounded waits), verified by
  stash-A/B.
