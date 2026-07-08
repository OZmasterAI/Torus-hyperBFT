# Rollout note: zstd body compression (Sprint 5 T3.2)

**Created:** 2026-07-08 · **Status:** NOTE ONLY — no code change. Records the
verified deployment state so nobody re-opens a settled item.

## TL;DR — already on, nothing to gate

zstd body compression is **active by default in production** and needs no flag,
no migration, and no further work under Sprint 5. This note exists only because
the sprint backlog listed a "T3.2 zstd" item; the verified reality is that the
work already landed and the one dial it once had was deleted.

## What is actually deployed

- **Request/response bodies are zstd-framed on the `/2.0` protocols** —
  `/torus/{direct,block-data,native-da}/2.0` — negotiated **2.0-first with a
  per-peer fallback to `/1.0`** (`crates/torus-network/src/behaviour.rs`,
  `codec.rs` `WirePath` dispatch). This is ON by default: a node offers `/2.0`
  first and only drops to `/1.0` for a peer that does not speak it. Proven
  mixed-version safe in the s356 shake-out.
- **Gossip is never zstd-framed.** The one-time experimental *gossip*-zstd flag
  was **removed in s364**; there is no `gossip-zstd` toggle to set, and gossip
  bodies ride the plain framing on purpose (compressing tiny, already-diverse
  gossip frames did not pay and complicated the mesh path).

## Consequence for T3.2

There is **nothing to enable and nothing to gate**. The only remaining
body-dissemination lever is **topology, not bytes** — i.e. erasure coding
(T3.1, `sprint5-erasure-coding.md`), which changes *where* bytes flow rather than
how compressed they are. zstd and erasure coding **compose**: shards stay
zstd-framed under a future `/torus/native-da-shards/2.0`, so the shard scaffold
added in T3.1 deliberately reuses the existing `WirePath::NativeDa` framing.

## Do NOT

- Do not add a `gossip-zstd` flag back — it was removed for cause (s364).
- Do not treat "turn on zstd" as an open task — it is the default.
