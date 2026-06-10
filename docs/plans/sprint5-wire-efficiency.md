# Roadmap: Sprint 5 — Wire Efficiency (200k–400k band)

**Status:** ROADMAP (formalized s338; full brainstorm/design when Sprint 4 lands)
**Position:** after Sprint 4 (hardware). Software-side. Formerly the unnumbered
"erasure-coded chunk dissemination later + zstd 3–5x" clauses inside the
Sprint-4 paragraph of the s334 roadmap ("the plan — four provable sprints").

## Goal
Carry the 200k–400k orders/s band on Sprint-4 hardware by cutting body bytes
per link 5–15x, instead of buying more bandwidth.

## Items

### 1. zstd on body-carrying paths (cheapest, first)
Gossip batch publishes, native-DA pull responses, sync transfers. Order-JSON
is highly repetitive — 3–5x is conservative. At the 6MB cap and ~2 blk/s the
per-link body ceiling is ~12MB/s today; zstd alone makes that an effective
36–60MB/s. Independent of everything else; can even precede Sprint 4 once the
wire-versioning prerequisite (below) is met.

### 2. Erasure-coded chunk dissemination (the headline)
Reed-Solomon shard bodies across validators at ingress; reconstruction needs
any k-of-n shards from ANY peers. Per-link bytes ≈ 1/n; kills the
single-source hotspot that melted the us↔smallserver link in the s338 storm
(155 pull timeouts + 238 substream exhaustions to ONE peer). New chunk store
+ wire protocol + reconstruction path — the real lift of this sprint.

### 3. Binary ingress format (conditional — may move earlier)
bincode/borsh submit payloads replacing hex-JSON: removes the per-action
serde_json parse + full re-serialize on the ack path (torus.rs verify) and
2–3x of client→server bytes. This is Option C of the Sprint-3.5 design: if
the rpc_submit_verify_seconds histogram dominates in the 3.5 re-sweep, pull
this forward; otherwise it lands here. Hash-identity rules need a
determinism test (canonical serialization defines the action hash).

## Hard prerequisites
- **friend2 on a current binary** — every item is a wire change; mixed-version
  safety requires versioned envelopes (NATIVE_ACTION_TOPIC v2 topic,
  /torus/native-da/2.0 protocol name) or a coordinated upgrade. The s338
  wedge proved all three validators are liveness-critical (quorum =
  (2/3 power)+1 with equal stakes = 3-of-3).
- **Sprint 4 hardware** for the band this targets (zstd exempt — helps VPSes too).
- Sprint 3.5 re-sweep verdict (decides whether item 3 moves forward).

## Non-goals
Swarm QoS / backpressure lanes (Sprint-3.5 Option B — only if its telemetry
shows queue buildup after path-dedup); validator-set topology (4th validator /
weighted stakes — genesis/ops track, not sprint code).
