# r2 lens `e-verify-replay`: exec-thread verify (2.62 ms/blk) and replay guard (0.55 ms/blk)

Read-only. Repo HEAD `2d2f3ed9` (batch10 merge on top of `eb993e3f`; line numbers below are HEAD). Numbers from
`~/bench-results-matched/ozarchy-9j2-std-r2-CL21` (val1 `metrics-after-val1.txt`, `metrics-1hz.txt.gz`), map/measured.md,
and memories 5ab71d96 (8e ubench), b6d2198b (prep-stage overlap), a48de6c0 (libsecp parity), 5a8a9cc5 / cb43ab8d
(carried senders). Nothing built or run.

## 0. What the two timers actually contain (read first)

- **verify** (`app.rs:5565-5615` -> `eip712.rs:1443-1542`): for every action, `append_canonical_bytes` (C, ~10.8 KB mean,
  17.6 KB for a 400-order place) plus one Keccak absorb that forks into the anti-MEV sort hash `keccak(C)` and the
  trust-cache key `keccak(C||nonce||v||r||s)` (`torus-types lib.rs:1482-1508`), then one `RwLock<FifoCache>` lookup
  (`mempool lib.rs:599-617`). Phase 1 recovery is skipped on all-hit (8g). Measured: 18.28 s / 7,290 calls /
  441,274 actions = **41.4 us per action, 60.5 actions per call**. Per-second distribution of us/action (val1, 300 s):
  p5 28, p50 40, p95 74, max 131. The 8e ubench (100 x 239-order actions, 18c) gave 52 us/action, and keccak-asm runs at
  206-248 MB/s (`Cargo.toml` comment). So **~80-84% of verify is Keccak throughput**, 16-20% is encoding, and the lookup
  is ~2%.
- **replay guard** (`app.rs:5653-5761`): 3.82 s / 441,274 = **8.65 us per action**. It includes the nonce read through
  pending -> parent -> L -> C -> RocksDB (`backend.rs:3057-3105`, `db.rs:1387`). It also includes
  **`signed.action.clone()` (`app.rs:5741`)**, which deep-copies a 400 x 128 B `Vec<PlaceOrderParams>` (~51 KB) per
  place action, ~3.2 MB per block. The clone is likely half or more of the guard. Removing it is
  `drop-exec-deep-clone-bodies` (tried-inconclusive, July), so it is not proposed here, but it is the first thing to
  re-measure before anyone touches nonce lookups.
- **Shadow.** The whole prologue (ticket wait, verify, replay, sort) runs while the previous block's `end_resident`
  worker runs (`app.rs:5793-5798`; `begin_resident` joins it). `end_resident` averages 4.03 ms per block
  (29.40 s / 7,290) and the E wait 1.59 ms (11.56 s). The wait is under 0.5 ms in 72% of blocks (5,218 / 7,290) and
  over 8 ms in 6.5%. So a prologue cut turns into E wall time in most blocks, and into more waiting in the large blocks.
  `exec-prep-stage` was rejected on exactly this point (mem b6d2198b: then `end_resident` was 15.55 ms against a
  0.36 ms wait). Today the two sides are about equal, so cuts convert again, but only partly.

## 1. keyed-local-trust-key: cheap exact trust key, consensus sort hash memoized in the cache value

**Mechanism.**
- `verified_cache_key` is node-local. It is never on the wire or in state; its only users are the mempool seed and
  restash paths and E (`lib.rs:1433-1457`; callers `mempool lib.rs:1355,1495`, `native_pool.rs:398`,
  `eip712.rs:1469-1480`). Yet it is paid at keccak-asm speed (8.8 c/b) on E for every action.
- Change the key function to a fast collision-resistant one, picked once per process:
  - (a) **SHA-256 via SHA-NI.** `sha2 0.10.9` is already in `Cargo.lock` with runtime SHA-NI detection; ozarchy (Ryzen
    5950X) has `sha_ni`. About 4-4.5x keccak-asm per byte.
  - (b) **Keyed POLYVAL.** `polyval 0.6.2` (CLMUL) is already in the lock. Use a per-process random key, fed field by
    field from `PlaceOrderParams` in fixed-width 16-byte blocks (an injective layout plus a length block), with no
    canonical-byte encode. That is about 10x+ keccak per byte, and it removes the ~20% encode share.
- Store the consensus sort hash `keccak(C)` **in the cache value**: `(sender, sort_hash)` instead of `sender`.
  - The seeding paths already absorb C with Keccak for the pool content hash `keccak(C||nonce||0x00||sig)`
    (`native_pool.rs:414`, `lib.rs:1388-1412`). Forking that sponge after C (as 8e does at exec) yields `keccak(C)` for
    one extra finalize.
  - The pool entry keeps it next to `restash_key` (`native_pool.rs:357-369`), so `remove_committed_native` restashes
    `(key, sender, sort_hash)` (`lib.rs:2208-2213`).
- On a hit, E gets both the sender and the exact sort hash with **zero Keccak**, ties included, and no consensus change.
- A miss is today's path: E computes `keccak(C)` itself and recovers.
- Safety argument, unchanged in kind: a hit means this node verified the byte-identical (C, nonce, sig), under
  SHA-256 collision resistance or keyed-POLYVAL eps-AXU (about L/2^128 with a secret key that never leaves the
  process). E still derives the key from the exact bytes it executes. The session exclusion and the distinctness from
  the content hash stay as `mempool lib.rs:263-275` requires.

**Target.** E verify (and E on-CPU per fill). It also removes one full Keccak per received action per node at
ingress and gossip.

**Expected gain** (from the 41.4 us/action composition above):
- **SHA-NI arm:** 41.4 x (0.18 encode + 0.82/4.4 hash + 0.02 lookup) = about 16 us/action, which saves 25 us x 60.5 =
  **-1.5 ms/blk**.
- **POLYVAL field-wise arm:** 41.4 x (0.08 + 0.82/12 + 0.02) = about 7 us, which saves 34 us x 60.5 =
  **-2.1 ms/blk**.
- **E on-CPU per 1k fills:** today 2.633. This is -0.13..-0.18, i.e. **-5..-7%**, independent of the shadow.
- **E wall:** with 50-75% conversion (the shadow), **-0.8..-1.6 ms/blk** (-2..-4% of 38.35 ms). matched/s
  **+0.6..+1.2%** at elasticity 0.3; native blk/s +2..+4%.
- **CPU-s/1M:** -1.3..-1.5%. That is E 0.03-0.05 cores plus ingest about 0.046 cores per node
  (33 us x 1,386 actions/s), out of 6.14.
- **Tail:** a 400-action block's verify (16-64 ms today; 4.7% of blocks are over 16 ms) shrinks 3-6x.
- **Deep shape:** verify 2.60 / 3.45 ms (CL21 / R) scales the same way.

**Nearest prior.** Grepped: trust.cache, verified.sender, exec-carried, exec-action-keys, double-verify, par_recover,
keccak-x4, blake, sha, polyval, xxh, k12, faster hash, zero-rehash, single-pass, K1, k3, L6.3, R31, sort-key-ties.
- `k1-trust-key-dispatch-hash`, proposed: re-key the cache by the content hash carried from dispatch.
- `exec-carried-verified-senders` / `carry-senders-from-dispatch`: proposed, "folded into KP / 8g".
- `zero-rehash-exec-frontend`: carries the key and sort digest in `CommittedBlockMsg`; partly merged.
- `exec-hash-once` 8e/8g: shared absorb, merged.
- `exec-action-keys-parallel` (KP): measured no gain, +1.03%, CPU +2.1%.
- `l6-3-preexec-lookahead-verify`: proposed.
- `eip712-batch-item-keccak-x4`: ingress items only.

The difference: every prior moves or shares the Keccak work. None changes the hash function of the node-local key,
and none memoizes the consensus sort hash in the cache value. This idea needs no cross-thread plumbing, keeps "verify
what you execute" on E, and works the same for leader, follower and replay.

**Risks.**
- Without SHA-NI, SHA-256 soft is slower than keccak-asm. Select at startup and fall back to keccak.
- POLYVAL needs a carefully injective field layout and key hygiene. Keep SHA-NI as the default and POLYVAL as an
  experiment.
- Pool entry +32 B and cache value +32 B (about 2 MB each at 64k entries).
- Tests that pin `verified_cache_key` values need updating.
- The conversion is partial while `end_resident` is the long pole. It stacks with r1 #8.

**Effort.** 2 person-days for SHA-NI plus the memo and tests. +1.5 d for the POLYVAL arm.

**Prototype (<= 3 h).**
1. Extend `crates/torus-bridge/tests/exec_hash_once_ubench.rs` with `UB_TRUST_KEY={keccak,sha256,polyval}`. Use a
   bench-shaped block: 60 actions = 37 PlaceOrderBatch x 400 + 23 CancelAll, from a sender pool with a tie-rate knob
   (0 / 20 / 50% same-sender places).
2. Record verify+sort ms per block (median of 200 reps). Assert that the sorted lists and the senders are identical to
   today's path.
3. Kill if the SHA-NI arm saves under 1.0 ms/blk at 60 actions, or POLYVAL under 1.5 ms.
4. Then a 2+2 ABBA cell (`proto/cell.sh trust-key {off,on}`, `TORUS_S80_TRUST_KEY`). Read:
   - `torus_exec_verify_seconds` sum/count;
   - schedstat exec on-CPU per 1k fills;
   - `torus_exec_end_resident_wait_seconds` sum (the conversion check);
   - `matched_s_first120`, `blk_s_avg`.

## 2. rule-versioned-trust-key: the trust cache must not carry a sender across a P0.0 activation

**Mechanism.**
- The cache contract is "hit => the sender a fresh recover at the executed height would produce". It is seeded at
  ingress or gossip time, which uses "the expected bits for tip+1, best effort, not consensus" (P0.0 plan,
  `docs/plans/p00-activation-height-gating.md:33`). It is read by E at the executed header's bits
  (`app.rs:5597-5603`). It is never cleared (`mempool lib.rs:585-597`), and the key has no rule identity
  (`lib.rs:1443-1457`).
- Any live activation that changes sender resolution would let a warm-cache node execute `S_old` while a restarted or
  DA-only node recovers `S_new` (or fails). That is a deterministic state split at the first such height, and the
  running hash fail-stops the minority. Example rule changes:
  - the known EIP-712 stop trigger/limit gap fix (typehash change, crypto-runtime map section 5);
  - a v2 PlaceOrderBatch signing format;
  - a domain or chain-id change;
  - a low-S policy for a libsecp backend.
- Fix: add a `sig_rule_id` (u8, derived from the active verify-affecting feature bits; 0 today) to the key preimage.
  Ingress seeds under the rule it verified with, and E looks up under `rule_id(torus_block.header.features)`.
  - Across the boundary the result is a miss and a full recover under the executed rule, so the result is correct.
  - `eip712_domain_separator`'s `OnceLock` (`eip712.rs:215-229`) needs the same rule input if the domain ever becomes
    a feature.
  - Cost: one byte per key.

**Target.** Robustness and security: it removes a latent fork class before P0.0 phase 7 (governance-scheduled
activation).

**Expected gain.** 0 perf. It turns "fork at the activation height" into "a few hundred extra ecrecovers on E at that
height": about 60 actions x the exec lag (~2 s = ~40 blocks) x 330 us, about 0.8 s of CPU, once per activation.

**Nearest prior.** Grepped activation / feature bit / p0.0 x cache / verify / sign / typehash.
- `eip712-per-variant-typehash`: merged, fresh genesis.
- Mem a48de6c0 (libsecp parity): cache seeding must equal exec semantics, at the backend level.
- The P0.0 plan treats admission as best effort and does not mention the trust cache.

None covers a rule change at an activation height reaching E through the cache.

**Risks.**
- None at runtime.
- A future feature author must mark verify-affecting features. Add a `Feature::affects_sender_resolution()` list with
  a test.

**Effort.** 0.5-1 person-day.

**Prototype (<= 3 h).** Red then green test on the existing poisoned-cache templates (`app.rs:19190-19270`).
1. Under the `test-features` build, let `TestStateRule` (id 127) switch a test-only sender rule, for example a
   different chain id in the domain for recovery.
2. Seed a warm cache before H, execute a block at H on a warm and a cold context, and assert identical post-state.
3. The assertion fails today and passes with the rule id in the key.

## Considered, not proposed (ledger or dominated)

- **Moving verify off E** (to dispatch, a lookahead thread, pool bubbles, or the T5.3 persist writer):
  `exec-carried-verified-senders`, K1, L6.3, KP, `exec-prep-stage` (rejected), `exec-3stage-pipeline` (rejected). The
  persist-writer placement is only an L6.3 placement variant.
- **Replay nonce read:** `nonce-hwm-db-skip` (killed r1), `batch-nonce-replay-guard` (proposed),
  `typed-or-in-memory-nonce-set` (owner decision 2a). The read is only about 2-4 us of the 8.65 us; the clone is the
  larger part (above).
- **Exact equality cache** (signature-indexed, retained `Arc` bodies compared structurally): exact, but it holds up to
  ~1.3 GB worst case and is dominated by arm (b) at about the same 5-7 us/action.
- **Per-block uncached-verify budget against session floods:** about 0.4 ms of serial E per 400-order session action,
  so a 400-action block costs about 160 ms. It is dominated by `session-action-parallel-verify` and L5.3 (both
  proposed).
- **Trust-cache pinning against eviction storms:** the restash plus a cap of 64 x 400 x 2.5 already bounds churn to
  about 23 s of residency against a 1-3 s exec lag; inserts happen only after a successful pool insert
  (`lib.rs:1572-1576`).
