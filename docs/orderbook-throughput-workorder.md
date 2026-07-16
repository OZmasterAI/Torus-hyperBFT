# Work Order: Native-Orders Throughput → 250k–400k Sustained Matched, Chain Healthy

> Hand this to an instance to dispatch the investigation. It is self-contained — the
> instance will NOT have the originating conversation, so everything needed is inline.

---

## Mission

Get the Torus-hyperBFT chain to **sustain 250,000–400,000 orders/sec actually
matched-or-placed on the order books** while the **chain stays healthy** — block
production must not collapse, stall, or wedge under sustained load. You are to
**diagnose, decide the best base, design, AND implement** the fix on a new branch.
Not a proposal-only task.

## The #1 trap — read this twice

The headline "240k / 250k orders/s" figures in prior runs are the load-generator's
`included_actions/s × batch` convention — i.e. **DA/hash-inclusion (offered load)**,
NOT executed/matched book orders. Ground truth from a prior WAL run:
`native_actions_processed=28699`, `orders_matched=60881` → **~2.1 matched per executed
action ≈ 0.53% of the ×400 number**. **Never report throughput from ×400.** Every
throughput claim must come from **node counters / RPC**, or it doesn't count.

## What's already known (don't start cold)

The friend's baseline (GitHub PR #3 data + PR #2 analysis, node `9a0806a`, genesis
`5042dbdb`, 3-val fleet):

- Peak **~74k committed orders/s** (m10·b400·s20); **block-rate crashes 21.5 → 1.15–6.9/s**
  under *every* loaded cell.
- Cause: **`gossipsub: Send Queue full — could not send Publish`** — config-independent
  **gossip send-queue saturation**; read-RPC errors out under flood.
- Small batch (400) digests better than 1000. Chain is healthy idle and recovers the
  instant load stops.
- Two open suspects: **(a)** erasure shard-gossip (`/torus/native-da-shards/1.0`) now
  fleet-wide, **(b)** 3-val topology (quorum = all 3, one saturated node stalls all).

## Required reading (before touching code)

- GitHub PR #3 `bench: native-orders throughput grid — raw results` →
  `testnet/results/native-orders-grid-20260713/`
- GitHub PR #2 comment — the gossip send-queue analysis table (the root-cause writeup)
- Repo `github.com/OZmasterAI/torus-hyperbft-data` — node-logs baselines
  (`node-logs/val2-baseline-prefix-*`)
- `docs/val2-baseline-workorder.md`, `docs/ladder-10run-setup1-setup2b.md`
- Memory: `search_knowledge "orderbook funnel x400 DA inclusion matched node counters"`
  and `"gossip send queue saturation native-da-shards"`

## Candidate bases

- **`5a90c2f`** — pre-erasure; lacks the S459 wedge fix and pacemaker-backoff hardening;
  historically showed the inflated ×400 "240k".
- **`a225844`** (current HEAD of `feat/commit-fsync-durability`) — post-erasure; **has**
  the S459 body-durability wedge fix (`9d56144`) and pacemaker backoff (anti-stall); the
  modern line.

## Phase 0 — Empirical bake-off on LOCAL DEVNET (decide the base)

Do this on the **local devnet** (`devnet/` — do NOT touch the shared testnet fleet;
that's cost-gated and coordinated separately). Build both `5a90c2f` and `a225844`, stand
each up on an identical local multi-node devnet, and drive the **same** native-orders
load grid against both. For each base measure, off node counters:

1. **Sustained matched/placed orders/s** (`torus_orders_matched_total` rate,
   `getOrderBook` resting count) — the real number.
2. **Health under load** — block-rate vs idle, frequency of `gossipsub: Send Queue full`,
   any stall/wedge, recovery behavior.

**Decide the base by evidence**, weighing the tradeoff: `5a90c2f` (pre-erasure →
possibly less gossip pressure, but no wedge/stall hardening) vs `a225844` (erasure +
hardening → healthier, but erasure shard-gossip may be the saturation source). **Write
up the verdict with counter output**, then **continue from the winner**. If they tie on
throughput, prefer the one that stays healthy under sustained load.

## Phase 1 — Localize the ceiling

On the chosen base: reproduce the grid, then run a **pre- vs post-erasure A/B**
(`9f3d1e3` pre-erasure vs `9a0806a`/HEAD post-erasure) to prove whether the
`Send Queue full` saturation is **erasure shard-gossip** or **base gossip / 3-val
topology**. Instrument gossip **publish-rate** and **outbound-queue-depth** under load.
Suspect code: `crates/torus-network/src/{swarm.rs,behaviour.rs,bridge.rs}`
(`broadcast_native_actions`, `broadcast_native_hashes`), plus
`crates/torus-mempool/src/native_pool.rs` and the RPC submit path.

## Phase 2 — True matched funnel

Instrument and report, per second, off counters only:
`submitted → admitted → included(DA) → executed → placed(resting) → matched → rejected`,
**with reject reasons**. Sources: `torus_native_actions_processed_total`,
`torus_orders_matched_total`, `getOrderBook` orderCount, `getTradeHistoryRange`
(caps 1000/mkt → undercounts matched ~15×; correct for it). Explain the
~2.1-matched-per-action conversion and whether it's rejects / self-match / batch
semantics / silent drops. **Both** the gossip ceiling and the match-conversion must
clear the target.

## Phase 3 — Implement + prove

Design the fix (expected orders/s gain + risk), **coordinate the implement-gate with the
user**, then build it on a **new branch off the chosen base**. Follow the loop:
failing test/bench first → implement → prove.

**Success = 250k–400k sustained matched/placed orders/s AND block-rate holding (no
collapse, stall, or wedge) across a multi-minute sustained local-devnet run, proven by
node counters.**

## Hard constraints

- **Local devnet only** for all iteration; **no relaunchable testnet build/run without
  explicit cost sign-off**.
- **No merge/push/deploy without explicit user ask.** New branch off the chosen base only.
- **Prove every claim with counter output. Never assert throughput from the ×400
  convention.**
- Query memory first; save findings/decisions/failed-approaches to memory as you go.

---

## Appendix: orchestrator model choice (Fable 5 vs Opus 4.8)

This is a long-horizon + multi-agent fan-out + deep systems-debugging task — the shape
Fable 5 is built for (sustains ongoing comms with long-running sub-agents/peer agents;
flags intermittent flakes rather than declaring "fixed" after one clean run). Tradeoffs:

- **Fable 5** (`claude-fable-5`): highest ceiling for this work, but **2× the price**
  ($10/$50 vs $5/$25 per MTok), minutes-long turns (plan async), and a cyber-classifier
  caveat (low risk here — perf/consensus, not exploitation).
- **Opus 4.8** (`claude-opus-4-8`): SOTA long-horizon at half the cost.

**Recommended for this task:** because the bottleneck is *already diagnosed* (verify +
fix, not open-ended discovery) and cost is governed, orchestrate with **Opus 4.8** and
run parallel sub-agents on **Opus 4.8 / Sonnet 5** (cheap mechanical stages on Sonnet).
Escalate the orchestrator to **Fable 5** if the first pass stalls or the intermittent-flake
reasoning needs the top tier and 2× cost is acceptable.
