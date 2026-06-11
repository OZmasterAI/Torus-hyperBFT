# Design: Sprint 5 Item 1 — zstd on Body-Carrying Paths

## Problem
Order-JSON native-action bodies are highly repetitive; the roadmap
(sprint5-wire-efficiency.md) projects a conservative 3–5x byte cut from
compression. Today every body-carrying path ships raw length-prefixed borsh.
At the 6MB body cap and ~2 blk/s the per-link ceiling is ~12MB/s; zstd alone
makes that an effective 36–60MB/s without touching Sprint 4 hardware. zstd is
the one Sprint 5 item exempt from the hardware prerequisite.

## Context (exploration s355)
Body-carrying surfaces (all length-prefixed borsh via
`read_length_prefixed_borsh`, codec.rs:90):
- `/torus/direct/1.0` — pre-proposal push, 4MB cap (codec.rs:30,
  behaviour.rs:62). Push-primary: this is THE hot body path post-hardening.
- `/torus/native-da/1.0` — pull-rare body fetch, 8MB cap (codec.rs:33,
  behaviour.rs:81, wire format codec.rs:199–318).
- `/torus/block-data/1.0` — sync transfers, 16MB cap (codec.rs:35,
  behaviour.rs:71).
- `/torus/sync/1.0` — sync protocol (behaviour.rs:90).
- Gossip topic `/torus/native-actions/1.0` (`NATIVE_ACTION_TOPIC`,
  behaviour.rs:14) — pre-spread batch publishes.

Hard constraint (s338 wedge, quorum 3-of-3): val1 + friend2 still run
92cb466. Any wire change must be mixed-binary safe or it can wedge the chain.

## Options

### Option A: Versioned protocols via multistream negotiation + dual-subscribe gossip (recommended)
Request/response paths get sibling `2.0` protocols (`/torus/direct/2.0`,
`/torus/native-da/2.0`, `/torus/block-data/2.0`, `/torus/sync/2.0`): same
borsh payload, zstd-framed. libp2p request_response registers both versions;
multistream-select negotiates the best mutual protocol PER PEER — a 2.0
binary talks 2.0 to 2.0 peers and falls back to 1.0 with old peers
automatically. Zero coordination, deployable to our seed immediately.
Gossip cannot negotiate per-peer: new binary subscribes BOTH topics,
decompresses v2 receives, but publishes v1 until `--gossip-zstd` flips
(config-only change once all three validators are upgraded).
Decompression-bomb guard: streaming decode bounded by the existing per-path
caps.
- Files: torus-network codec.rs / behaviour.rs / bridge.rs / swarm.rs,
  torus-node main.rs (flag), Cargo.toml (zstd dep), telemetry counters.
- Pros: mixed-binary safe by construction; per-peer fallback free; staged
  rollout (seed now, full savings after friend deploy); bomb-guarded.
- Cons: protocol surface doubles temporarily; gossip flip is a manual
  config moment; dual-subscribe costs a topic until 1.0 retires.
- Effort: Medium. Risk: Low-Medium.

### Option B: In-band format byte on the existing 1.0 protocols
Prefix frames with 0x00 (raw) / 0x01 (zstd) and sniff on read.
- Pros: no new protocols.
- Cons: NOT mixed-binary safe — old binaries misparse the new frame; whole
  net must upgrade in lockstep first (exactly the s338 wedge shape). Rejected
  while mixed versions exist.

### Option C: Transport-level compression (muxer wrapper)
- Cons: no standard libp2p compression transport for this stack; custom
  muxer = deep, untestable risk for the same byte win. Rejected.

## Recommendation
Option A. It is the only one deployable before the friend upgrade, and the
negotiation machinery is exactly the "versioned envelopes" prerequisite the
roadmap names — building it here also unblocks erasure coding (item 2) later.

## Rollout
1. Land + test (mixed-version devnet proves 2.0↔1.0 fallback).
2. Deploy seed: pull/sync/push savings activate per-peer as others upgrade.
3. Friend deploy (the standing prerequisite) → all req/resp paths at 3–5x.
4. Flip `--gossip-zstd` on all three → gossip savings; retire v1 topic in a
   later release.

## Open Questions
- zstd level: start 3 (fast, ~3-4x on JSON); benchmark 1 vs 3 vs 6 in T1.
- Dictionary training (shared zstd dict for small frames): defer — batches
  are large enough that plain frames should hit target; revisit if gossip
  singles dominate.
- Retire `/torus/*/1.0` + v1 topic: after all validators on 2.0 for a full
  era (ops decision).
