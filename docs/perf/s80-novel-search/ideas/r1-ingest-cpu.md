# r1 lens `ingest-cpu`: new ideas (novel-s80)

Repo main `eb993e3f`, read-only.

## 0. Correction to the lens premise (read first)

The lens premise ("rpc-worker 13.8% of node user cycles, futex lock_contended 8.7%, ReadView drop 4.7%") comes from
the q8re perf profile. That profile weights threads by sample count at `-F 499`. Bursty threads get samples with
small periods, so this weighting inflates them. The rayon-idle check (memory 91563c5d) already found that idle samples
carry about 1/20 of the mean period.

The ground truth is /proc per-thread utime+stime. I used `ozarchy-hsdb-300m-r{1,2}-B/io-threads-{start,end}.txt`
(602-604 s windows, s124-era binary, same 300m bench shape and same RPC and gossip code paths). Cores per node:

| thread class | val0 / val1 (u + s) | val2 = bench commit-tail node (u + s) |
|---|---|---|
| torus-exec-pool | 1.13-1.16 | 1.12-1.15 |
| torus-gossip-verify | 0.94-0.96 | 0.97 |
| torus-execution | 0.85 | 0.85 |
| torus-ingress-verify | 0.82-0.84 | 0.76 |
| tokio-rt-worker | 0.44 | 0.44 |
| **rpc-worker** | **0.17** (0.13 + 0.04, 72-80 threads) | **0.80** (0.70 + 0.10) |
| rocksdb:low / hotstuff-algo | 0.27 / 0.19 | 0.26 / 0.19 |

Conclusions:
- **rpc-worker is about 3% of a non-tail node, not 13.8%.** The q8re 13.19% is a sampling artefact. The
  spawn_blocking / LockLatch rendezvous it shows is real but worth only about 0.05-0.09 cores per node, and
  `verify-pools-parking-executor` already covers it. I dropped my rendezvous idea for that reason.
- **The extra rpc-worker CPU on val2 is the bench commit tail.** Every 9j2 cell shows val2 at +9.3% user CPU per
  1k fills, from `torus_getBlockBody` building serde_json `Value` trees. A fix is in the ledger as
  `rpc-block-body-raw-json` (rejected, "no chain gain").
- **Exec runqueue wait is the same on all three nodes.** In 9j2 std r1/r2 and deep r1 it is 2.97-3.33 ms per block.
  So contention is host-wide, and the extra 0.6 cores on val2 do not show up in val2's exec thread.
- **What actually dominates ingest are the two verify pools: about 1.8 cores per node, 35% of node CPU.**
  - Keccak: about 0.69 cores. Nearly all of it is the EIP-712 per-item struct hash. Every lever for it is already in
    the ledger or excluded: v2 format, keccak-x4, the L5.1 memo, and sharded verification.
  - k256 EC: about 0.45 cores. This is the one large lever that is not in the ledger.

## How much the exec thread gains (applies to every idea)
The exec runqueue wait is about 3.0 ms per block, about 170 us per wakeup at 17.5 timeslices per block. Lowering the
nice level of ingest threads gave about +1%. So expect matched/s within +-2% from any ingest cut. The wins are
CPU-s/1M (23.15 today), host headroom on one shared host, and validator sizing in production.

---

## 1. `gossip-pubkey-batch-msm-verify`: ship the signer key in gossip; receivers batch-verify each frame with one MSM

**Mechanism.** Each gossip-received EIP-712 action pays a full k256 `recover_from_prehash`. That is the recovery
lincomb plus a trailing `verify_prehash` lincomb, plus decompress and inversions (`eip712.rs:901-917`, called from
`mempool/src/lib.rs:1356-1373`). The claimed sender already rides in the frame (`lib.rs:1460-1482`,
`swarm.rs:1543-1560`).

The change has three parts:
1. **Carry the key.** The ingress node keeps the recovered `VerifyingKey` (`ecrecover` currently discards it,
   `eip712.rs:914-916`). It ships the uncompressed point (64 B, about 0.5% of a 13 KB action) in a tagged v2 gossip
   entry; receivers dual-decode v1 and v2. The receiver checks that the point is on the curve, then that
   `keccak(Q)[12..] == claimed` (one permutation; this is `pubkey_to_address`, `eip712.rs:893-898`).
2. **One MSM per frame.**
   - Compute each `signing_hash` as today. The struct hash is unchanged.
   - Reject high-S explicitly. k256 verify rejects it; `Signature::from_slice` already rejects r or s of 0 or >= n.
   - Lift R from (r, v-27). v is only 27 or 28 (`eip712.rs:910-912`), so there is no x-overflow case.
   - Batch-invert the s values.
   - Check `sum z_i R_i - (sum z_i u1_i) G - sum (z_i u2_i) Q_i == O` with 128-bit CSPRNG z_i. This is one
     Strauss/Pippenger multi-scalar multiplication per frame (9 actions or more, or frames accumulated for 2 ms or
     less).
   - If the check fails, fall back to today's per-action recover, so the bad action is identified and penalized.
3. **Per-frame dispatch.** The gossip inbound task spawns one rayon job per frame, not one per action
   (`main.rs:1012-1031`). That is 9x fewer HeapJobs, injects and `wake_any_threads` scans.

Optional extension: RPC submit accepts a client-supplied key, so the RPC verify pool batch-verifies too. That also
lets the address limiter run before any crypto.

**Why it is exact.** The check `s R = e G + r Q_claimed` with R lifted from v holds if and only if `recover(e, v, r, s)`
returns `Q_claimed`. The random linear combination accepts an invalid batch with probability 2^-128 or less. The
accept set therefore equals today's `recover == claimed` path, and the trust cache seeded from it stays sound.

**Evidence.**
- Thread costs: gossip-verify is 0.94-0.97 cores per node and ingress-verify 0.76-0.84 (io-threads above; crypto map
  §3).
- EC share of each thread (q8re, sample shares are fair within a busy thread):
  - gossip-verify: `ecrecover` 30.98%, `lincomb` 24.22%, `verify_prehashed` 14.85%
  - ingress-verify: `ecrecover` 17.84%
- Volume (9j2): trust-cache misses 294,104, about equal to gossip received (293,926) per 303 s. That is 970 recovers/s
  per node on gossip plus 487 on RPC.
- Cost per recover: about 2 lincombs of roughly 400 point ops each (k256 4-bit windows, G not tabled), plus about 80
  for decompress and inversions, so about 880 point-op equivalents.
- Cost per signature in a batch:

  | batch | point ops | per signature with R lift | vs recover |
  |---|---|---|---|
  | k=9 (one frame) | 19 points; 1,061 adds + 256 doubles | about 166 | 5.3x cheaper |
  | k=16 | 2,090 / 16 = 131 | about 150 | 5.9x cheaper |

**Expected gain.**
- Gossip arm: 0.95 cores x 31% x 0.82 = **0.24 cores per node, 0.72 on the host, CPU-s/1M about -3.9%.**
- With RPC keys: another 0.80 x 17.8% x 0.82 = 0.12, so **0.36 cores per node in total, about 1.1 on the host,
  CPU-s/1M about -5.9%.** The gossip pool also gets about 9x fewer job wakeups.
- matched/s: 0..+1% (host contention only).
- If libsecp (s124 KEEP) lands first, the baseline EC cost roughly halves, and the extra saving is about half of the
  above.

**Nearest prior** (grepped: ecdsa, secp, msm, multi-scalar, pippenger, strauss, ecrecover, recover, batch verify,
pubkey, verifying key):
- `l5-2-known-key-ecdsa-comb-tables` / `known-key-ecdsa-tables`: per-signer precomputed comb tables, still one
  verify per signature; rated 0.07 for "fork risk via accept-set".
- `ecrecover-libsecp256k1(-backend)` and `k256-recover-noverify`: a faster single recover.
- `sharded-sender-verification`: fewer verifiers, using QC-certified claims (trust).
- `ed25519-batch-verify`: sessions only.
- `t2-1-batch-verify-api`: parallel recover only.

What is new: a randomized multi-signature MSM that shares the doublings and the G term across a frame, with the key
shipped in the frame and bound by keccak. There is no per-signer state and no trust.

**Risks.**
- The accept set must equal recover's, otherwise the trust cache could diverge and fork exec. This needs a
  differential fuzz suite: high-S, s or r of 0 or >= n, v outside 27/28, Q off the curve or at identity, wrong Q,
  wrong e. The z_i must come from a CSPRNG.
- An attacker can poison batches to force fallbacks. Batch per peer, cap the fallback cost at today's cost plus one
  MSM, and penalize the peer.
- The gossip wire bump needs dual decode for one release.
- New MSM code, roughly 250-350 lines on k256 `ProjectivePoint`/`Scalar`, with constant-time not required since
  these are public inputs.

**Effort.** 7-10 person-days.

**Prototype (3 h or less).** Write a microbench crate on k256 0.13.4 with a naive interleaved 4-bit-window Strauss
MSM. Sign 10k prehashes with 5k keys using EIP-712-like digests. Measure us/sig for:
- `recover_from_prehash` (today)
- `verify_prehash` with a known key
- the batch at k = 9, 16, 32

Then run a differential test with 100k random corruptions: the batch with fallback must give the same verdict as
`recover == claimed` every time. Pass if k=9 costs 25% or less of recover per signature on archy (5950X).

---

## 2. `gossipsub-native-anonymous-instance`: a second, unsigned, content-addressed gossipsub only for native frames

**Mechanism.** Today one gossipsub behaviour, configured `MessageAuthenticity::Signed` + `ValidationMode::Strict`
(`behaviour.rs:84-97, 128-135`), carries both consensus and native-action frames. Every native frame of up to
128 KiB is ed25519-signed on the single swarm task (`swarm.rs:820-850`; signing hashes the payload with SHA-512
twice). Every received copy, relays included, is verified inside `GossipsubCodec::decode` before dedup.

The change: add a second behaviour with its own `protocol_id`, `MessageAuthenticity::Anonymous`,
`ValidationMode::Anonymous`, and `message_id_fn = xxh3(data)`, so relays dedup by content. It has the same explicit
validator peers and the mesh watchdog. It also turns on `validate_messages()`, so a frame is forwarded only after it
decodes, and penalties go to the QUIC-authenticated `propagation_source`. Consensus stays on the Signed instance.
Rollout is dual-publish and dual-subscribe for one release. Every action inside a frame is still user-signed and
verified downstream.

**Evidence.** In q8re tokio, `GossipsubCodec::decode` is 20.65% inclusive (ed25519 verify 18.12%), `Keypair::sign`
is 9.18% (inside `publish_native_batch`, 10.84%), and `sha512_compress` self is 23.67%. Signing is about 43% of the
swarm task's busy time. Cycle-weighted figures (crypto map, mem db0cb68d): envelope verify 0.059 plus sign 0.033
cores per node.

**Expected gain.** About 0.09 cores per node (1.5% of node CPU), 0.28 on the host, CPU-s/1M about -1.5%. The swarm
loop also gets about 0.03 cores of serial time back. matched/s about 0. This is modest; I list it because the lens
asked about ed25519 over payloads that users already sign.

**Nearest prior.**
- `gossipsub-validation-none`: rejected because it "breaks consensus source auth and msg-id". Here consensus keeps
  its own Signed instance, and the msg-id is content-addressed.
- `native-prespread-direct-zstd` / `prespread-over-direct-zstd` (proposed, -3.2..-4.4%): leaves gossipsub for
  /torus/direct unicast. Here the gossipsub mesh and relay are kept, which rpc-only nodes need.
- `gossipsub-explicit-peer-relay-dup`: about relay counts, not signatures.

**Risks.**
- Author attribution is lost. The s339 author-only penalty rule is replaced by validated relaying plus penalties on
  the propagation source.
- A second mesh has to be kept healthy.
- Mixed versions need dual publish.
- It overlaps direct-zstd if that lands.

**Effort.** 4-6 person-days.

**Prototype (3 h or less).** Run 2-3 in-process libp2p nodes over QUIC on loopback. Publish 128 KiB frames at 40/s
for 60 s on Signed+Strict and on Anonymous with the xxh3 id. Measure tokio task-clock per frame, swarm-task busy
time, delivery p50/p99, and relay duplicate suppression.

---

## Grepped and dropped (not novel, or premise failed)

- RPC spawn_blocking / LockLatch rendezvous: premise failed. The true cost is 0.17 cores per node (section 0), and
  `verify-pools-parking-executor` covers it.
- f+1 sender claims: `sharded-sender-verification`.
- Mirror after verify, bounding inbound: `da-mirror-after-signature`.
- Single-pass hashing, deep clones: `single-pass-ingest`, `compute-action-hash-twice`.
- Market reads: `cache-known-market-ids`.
- Thread priority, pinning, budgets: `bulk-thread-nice` / `renice-sched-batch`, `cpu-pinning-*`, `core-budget`.
  An exec-fork-join ingest gate is not in the ledger, but the renice result bounds it to about +1%.
- Tail-node JSON: `rpc-block-body-raw-json` (rejected).
- Bodies stored twice: `compact-body-record*`.
- DA blob: `da-cf-blobdb`.
- No mempool-lock contention shows in any profile. The rpc-worker `lock_contended` is rayon's LockLatch and
  sleep-state mutexes (rayon-core 1.13.0 `latch.rs:243-266`, `sleep/mod.rs:275-308`), not the pool `RwLock`.
  A lock-free mempool therefore has no measured target.
