# Design / Brainstorm: Erasure-Coded Body Dissemination (Sprint 5 — Item 2)

**Created:** 2026-06-14 · **Status:** BRAINSTORM / DESIGN — build-ready, parked.
Greenlight gated on (a) friends on a current binary (every item is a wire change) and
(b) validator-set growth (the headline win scales with n — see arithmetic). Realizes
item 2 of `sprint5-wire-efficiency.md`. Extends the native-DA dissemination layer
(`native-action-da.md`, `native-da-hash-only-push.md`).

**TL;DR.** Today a node missing a block body pulls the WHOLE body by hash from ONE peer.
Erasure coding splits each body into `n` shards so any `k` reconstruct; a node fetches
`k` shards from `k` DIFFERENT peers ⇒ per-link bytes ≈ `body/k` and the single-source
hotspot dies. zstd (item 1, landed `6cbda39`) cut body *bytes* ~9.4×; erasure coding
changes the *topology*. The two compose (shards are zstd-framed). The payoff scales with
the validator count — modest at today's n=3, large as the set grows toward the 21 cap.

## Problem — single-source body serving

A node that reaches `validate_block` without a referenced native-action body pulls the
**whole body by hash** from one peer over `/torus/native-da/{1.0,2.0}` (`NativeDaCodec`
request/response, chunked `NATIVE_DA_FETCH_CHUNK=16`). The proposer is the natural
source — it mirrored the bodies to its durable DA store (`mirror_native_to_da`,
`app.rs:1204`; committed bodies in `CF_BLOCK_BODIES` keyed by height) — so it becomes a
per-block pull **hotspot**. Under sustained load this melts a single link:

- **s338 storm (mem `55d8e647`, `sprint5-wire-efficiency.md`):** the us↔smallserver link
  SATURATED under native load — **155 pull timeouts + 238 substream exhaustions to ONE
  peer**. Whole-body-from-one-source concentrates all dissemination bytes on the slowest
  link.
- zstd compresses but does **not** change the topology: still one source serving the
  whole (compressed) body. Erasure coding spreads the serve across many peers, each
  carrying a small distinct shard.

This is a **WAN/bandwidth** ceiling, not CPU (CPU is a separate track —
`ingress-cpu-supply.md`). It bounds the 200k–400k orders/s band against the per-block
body budget `NATIVE_BLOCK_BYTES_CAP = 6 MB` (`rate_limit.rs:78`).

## Prior art

- **Polkadot availability** — the closest fit (same BFT, same "data available across the
  validator set" goal): candidate data is systematically RS-encoded into `n = |validators|`
  chunks; validator *i* custodies chunk *i*; any `f+1` chunks reconstruct; an **erasure-root**
  in the candidate receipt + per-chunk Merkle proofs make chunks self-verifying.
- **Ethereum danksharding / DAS** — blob data RS-extended and sampled, KZG commitments for
  integrity (sampling is a future light-client concern, not this doc).

We adopt the Polkadot-style **validator-custody** model (`n` = validator count), adapted to
a small set.

## Key arithmetic — be honest about small-n

RS(`k`,`n`): any `k` of `n` shards reconstruct; each shard ≈ `body/k`. A reconstructing
node fetches `k` shards from `k` DISTINCT peers ⇒ per-**link** bytes ≈ `body/k`, and serve
load spreads across `k` sources (single-source hotspot eliminated).

Availability needs `k = f+1` (reconstruct from any `f+1` honest), with `n−f ≥ 2f+1` honest
shards always present ⇒ always reconstructable:

| set | f | k=f+1 | shard size | per-link | note |
|----|---|------|-----------|---------|------|
| **n=3 (today)** | 1 | 2 | body/2 | ~0.5× | 2 sources, not 1 — mostly hotspot removal |
| n=10 | 3 | 4 | body/4 | ~0.25× | |
| n=21 (cap) | 6 | 7 | body/7 | ~0.14× | 7× per-link reduction |

⇒ **The headline 5–15× is a large-set property.** At the current 3-validator set the gain
is hotspot elimination + ~2× per-link. This is the core reason to make it **build-ready
now** but **deploy as the set grows** — and it composes with zstd (shards stay zstd-framed).

## Where it plugs in (verified architecture)

- Proposals already content-address bodies by hash (`CompactBlock.native_action_hashes`;
  COMPACT_PROPOSALS on, mem `e8ef5ae0`).
- Bodies served from the durable DA store (`mirror_native_to_da` `app.rs:1204`;
  `CF_BLOCK_BODIES`).
- Recovery pull = `/torus/native-da/{1.0,2.0}` (`NativeDaCodec`, `behaviour.rs:100-107`).
- **Wire-versioning precedent:** `/torus/*/{1.0,2.0}`, 2.0-first multistream + per-peer
  fallback (the zstd work). A shard protocol follows the SAME pattern → mixed-binary safe.

Erasure coding replaces (or augments) the recovery pull — and optionally the ingress
dissemination — with shard fetch / dispersal.

## Options

### Option A — Erasure on the RECOVERY path only (conservative)
Keep gossip full-body pre-spread + hash-only push (common case untouched). Replace the
single-source by-hash whole-body PULL with a multi-source **shard fetch**: a node missing a
body requests shard *i* from each of `k` peers, reconstructs, verifies against the
erasure-root.
- **New:** shard encode/decode, erasure-root (committed alongside the body hash),
  `/torus/native-da-shards/1.0` req/resp + per-peer fallback, reconstruct path, integrity
  (Merkle-proof-per-shard, body-hash backstop), fallback to whole-body pull on `<k` shards.
- **Pros:** smallest blast radius (only recovery changes); common fast path (gossip
  pre-spread) untouched → no bs100 regression risk; directly removes the s338 single-source
  hotspot.
- **Cons:** gossip pre-spread still ships full bodies, so under a SATURATED mesh (the s338
  condition) the dominant byte path is unchanged — erasure only helps the recovery tail.
  Smaller total win.
- **Effort:** Medium · **Risk:** Medium.

### Option B — Erasure at INGRESS dispersal (aggressive — the headline)
Erasure-code bodies at ingress and disperse shard *i* to validator *i* (each peer receives
`body/k`, not the full body) INSTEAD of gossiping full bodies. At proposal time each
validator already custodies its shard; reconstruct from the quorum's shards.
- **New:** everything in A, PLUS replace/augment native-action gossip pre-spread with shard
  dispersal; a custody store (`CF_NATIVE_SHARDS`: body_hash → owned shard + proof); a
  dispersal scheduler.
- **Pros:** cuts the DOMINANT path (gossip) by ~`k` per link — directly attacks the mesh
  saturation that wedged s338; the real 200k–400k enabler at scale.
- **Cons:** largest change; touches the consensus-critical pre-spread and the healthy bs100
  path (explicit guardrail, `native-da-hash-only-push.md`); reconstruction now on the common
  path (latency vs the ~500 ms view timeout); dispersal/custody bookkeeping. Higher
  regression risk.
- **Effort:** Large · **Risk:** High.

### Option C — Hybrid / phased (RECOMMENDED)
Ship **A** first (recovery-path erasure: low risk, kills the hotspot, build-ready).
Measure. Promote to **B** (ingress dispersal) only once (i) the validator set is large
enough for the `1/k` win to dominate and (ii) a mesh probe shows gossip pre-spread bytes are
the binding ceiling. Both phases reuse the same shard codec + erasure-root.
- **Pros:** incremental, each step independently shippable + measurable; matches the
  project's prove-then-promote rhythm; defers the consensus-critical pre-spread change until
  data justifies it.
- **Cons:** two-phase wire work (mitigated by shared primitives).
- **Effort:** A now (Medium), B later (Large) · **Risk:** Low → ramped.

## Recommendation — Option C
Build the shard primitives + recovery-path erasure (A) so it is ready when friends deploy;
defer ingress dispersal (B) until set-growth + a mesh probe justify the bigger change.
Honest framing: at n=3 this is hotspot removal + ~2× per-link; its strategic value is
forward-looking as `n` grows.

## Library (decide in writing-plans via a small encode/decode + ratio bench)
- `reed-solomon-erasure` — mature, arbitrary `k/n`, SIMD. Default pick.
- `reed-solomon-simd` — FFT-based, fast at many shards. Revisit if large `n`.
- `reed-solomon-novelpoly` — Polkadot's (systematic, GF(2¹⁶)), built for exactly this.
  Strongest prior-art fit; evaluate.

## Integrity (decision needed)
- **Erasure-root + per-shard Merkle proof (preferred):** the proposer builds a Merkle tree
  over the `n` shards, commits the root (bound to the body hash / in the compact proposal);
  each shard ships with its proof; a node verifies shard *i* **before** using it ⇒ a
  Byzantine peer cannot poison reconstruction.
- **Body-hash backstop (always):** the reconstructed body must hash to the proposal's
  referenced hash — the ultimate integrity check regardless of shard-level proofs.
- Reconstruct-then-verify-hash *without* proofs is rejected for adversarial fetch: one bad
  shard silently corrupts reconstruction; detection is post-hoc ⇒ combinatorial retry.

## Mixed-version / safety (hard prereqs — same class as zstd)
- **Friends on a current binary:** every item is a wire change. New protocol
  `/torus/native-da-shards/1.0` (or fold into `native-da/3.0`) MUST negotiate per-peer with
  fallback to the existing whole-body pull (the zstd 2.0-first precedent, proven
  mixed-version safe in the s356 shake-out). A node that can't gather `k` shards falls back
  to whole-body pull — **never wedges.**
- **Determinism:** erasure-root and shard layout are deterministic functions of the body
  bytes + RS params (`k`,`n`), with `k`,`n` bound to the (epoch-stable) validator-set size,
  so every node computes identical shards/root.
- **No new Byzantine trust:** shard integrity rides the erasure-root + body-hash backstop;
  any proof-fail or `<k` shards falls back to full verify + whole-body pull.

## Non-goals
- Replacing block-data / sync transfers (those ride `/torus/block-data/2.0`; revisit only if
  they show up in the wire counters).
- DAS / random sampling (we reconstruct fully; sampling is a future light-client concern).
- Any change to consensus identity — the erasure-root binds to the existing body hash; no
  chain-id / state-root change ⇒ no fork (unless Q2 puts the root in the header, which then
  must batch with a genesis-era relaunch).

## Open questions
1. **`k`,`n` policy** vs validator-set size and epoch changes — re-shard on set change?
2. **Erasure-root placement** — in `CompactBlock` alongside `native_action_hashes`, or a new
   header field? (A header field is a consensus-format touch ⇒ batch with a coordinated
   genesis-era relaunch.)
3. **Shard granularity** — one erasure set per block-body (simpler) vs per-action-batch
   (finer recovery).
4. **Dispersal source in B** — proposer fans shards vs each ingress node disperses its own
   admitted actions.
5. **Does A alone move the s338 metric**, or is the gossip path (⇒ B) the real binding cap?
   A mesh probe decides promotion.

## Verification (when built)
- **Unit:** encode→decode roundtrip; reconstruct from every `k`-subset incl. arbitrary
  missing shards; corrupt-shard rejection via proof; `<k` shards ⇒ clean fallback to
  whole-body pull.
- **Mixed-version devnet** (mirror the s356 zstd shake-out): a pre-shard peer pulls
  whole-body via fallback while shard-capable peers reconstruct; zero protocol errors; no
  wedge; dup-factor ≤ 1.05.
- **Mesh probe:** per-link body bytes under a bs-sweep, before/after; confirm the s338
  signature (pull timeouts + substream exhaustion to one peer) is gone.

## Status / next
BRAINSTORM. Next LOOP step = **writing-plans** (TDD task breakdown →
`sprint5-erasure-coding-impl.md`) once greenlit. Greenlight gated on friends deploying the
current binary (wire-change prereq) and the Q2 erasure-root-placement decision (batched with
a genesis-era relaunch if it touches the header).
