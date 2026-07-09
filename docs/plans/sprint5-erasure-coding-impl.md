# Impl / TDD Breakdown: Erasure-Coded Body Dissemination (Sprint 5 — Item 2, Option C Phase A)

**Created:** 2026-07-09 · **Status:** WRITING-PLANS — TDD task breakdown, build-ready.
Realizes the RECOMMENDED **Option C** of `docs/plans/sprint5-erasure-coding.md` (Status:
BRAINSTORM): ship recovery-path erasure (**Phase A / Option A**) now on shared primitives,
defer ingress dispersal (**Option B**) to a later phase gated on set-growth + a mesh probe.

## What already landed (commit `b29d843` — SHARED PRIMITIVES only)
- `crates/torus-state/src/erasure.rs` — `ErasureParams{k,n}`, `EncodedBody`, `ShardProof`,
  `encode` / `reconstruct` / `verify_shard`, Merkle tree, 7 unit tests. **UNCOMPILED
  SCAFFOLD**, 2 `TODO(toolchain)` markers on the reed-solomon-erasure v6 API.
- `crates/torus-network/src/codec.rs` — `NativeDaShardRequest` / `NativeDaShardResponse`,
  `NativeDaShardsCodec`, `NATIVE_DA_SHARDS_PROTOCOL = "/torus/native-da-shards/1.0"`, borsh
  round-trip test. Cap is a **placeholder** (`MAX_NATIVE_DA_MSG_SIZE`).
- `crates/torus-state/src/cf.rs` — `CF_NATIVE_SHARDS` registered (key
  `body_hash(32) ++ shard_index(2 BE)` -> `bincode(StoredShard)`). **`StoredShard` struct
  does not exist yet** — only the CF name + doc.
- `Cargo` — `reed-solomon-erasure` v6 dependency added to `torus-state`.

## What is unbuilt (this plan)
The ENTIRE integration half: v6 API confirmation, `StoredShard` codec, proposer encode+persist,
`Behaviour` registration + serve path, recovery fetch/reconstruct loop, never-wedge fallback,
`(k,n)` derivation, tighter cap, and the deferred consensus binding. Verification closes it.

## Architecture anchors (verified in-tree)
- Whole-body serve (the hotspot we relieve): `swarm.rs:1269–1322` — off-loop `spawn_blocking`
  serve, response posted back via `SwarmAction::DaServeDone` through the loop `select`.
  Admission-gated by `da_serve.try_admit()`; banned/overloaded answer all-empty inline.
- Whole-body fetch: `NativeDaFetcher` trait (`app.rs:694`, `fetch`+`drain`, non-blocking);
  `DaRecoveryWorker`; inbound bodies queued to `shared.native_da_inbound` (`swarm.rs:1334`).
- Body mirror point (proposer guarantee): `Mempool::mirror_native_to_da` (`mempool/src/lib.rs:630`,
  `da_store.put_batch`), invoked at `produce_block` (`app.rs:~1450`), reinsert, and ingest.
- Behaviour: `TorusBehaviour.native_da: request_response::Behaviour<NativeDaCodec>`
  (`behaviour.rs:35,134,184`); 2.0-first + 1.0 fallback `ProtocolSupport::Full` (mixed-binary).
- `(k,n)` source: `TorusApp.validators: RwLock<ValidatorSet>` (`app.rs:64`), epoch-managed via
  `EpochManager::compute_new_validator_set`; `epoch_length`.
- Cap ladder: `caps.rs` — `assert!(MAX_DIRECT <= MAX_NATIVE_DA <= MAX_BLOCK_DATA)`, ordering
  test-enforced (single source of truth).

## Sequencing rule (consensus safety)
Phase A is **additive wire only** — a new negotiated protocol + a new CF + a local encode. It
changes **no** `data_hash`, so it needs friends on the current binary (wire-change prereq) but
**NOT** a genesis relaunch. The ONLY genesis-touching item is binding `erasure_root` into
`CompactBlock`/header (design Q2, **T11**) — that is DEFERRED and batched with the next
coordinated genesis-era relaunch. Recovery-path integrity holds without it via the **body-hash
backstop** (reconstructed body must hash to the proposal's referenced `native_action_hash`).
Nothing consensus-breaking lands mid-plan. Option B (**T12**) is an explicit later phase.

Order: **T1 → T2 → T3 → T4 → T5 → T6 → T7 → T8 → T9 → T10** (ship Phase A), then **T11**
(deferred, relaunch-gated), then **T12** (Phase B, later).

---

## T1 — Confirm reed-solomon-erasure v6 API; green the 7 erasure unit tests
**Goal.** Resolve the 2 `TODO(toolchain)` markers in `erasure.rs` and make the existing 7
tests compile + pass. This is pure-lib (no wire, no consensus) → smallest possible first step.

**Files touched.** `crates/torus-state/src/erasure.rs` (only if the v6 API differs from the
scaffold's assumptions); `crates/torus-state/Cargo.toml` (confirm the `reed-solomon-erasure`
v6 feature set — `galois_8` is default-on).

**v6 facts to confirm against the compiler (the scaffold assumes these):**
1. `galois_8::ReedSolomon::new(data_shards, parity_shards)` — the scaffold calls
   `new(k, n - k)`. Confirm the two-arg `(data, parity)` shape (v6 kept it).
2. `encode(&mut shards)` where `shards: &mut [Vec<u8>]` — v6 `encode<T: AsRef<[u8]> + AsMut<[u8]>>`;
   `Vec<u8>` satisfies both. Systematic layout: data shards are indices `0..k` (galois_8 is
   systematic) — the scaffold relies on this for `body = concat(shards[0..k])`.
3. `reconstruct(&mut shards)` where `shards: &mut [Option<Vec<u8>>]` — v6 takes
   `T: ReconstructShard`; the blanket impl covers `Option<V>` for
   `V: AsRef<[u8]> + AsMut<[u8]> + FromIterator<u8>` and `Vec<u8>` qualifies, so `None` = missing,
   `Some(bytes)` = present. Confirm `reconstruct` (not `reconstruct_data`) rebuilds the data
   shards in place.

**RED-first test spec.** The 7 tests already exist and are RED on HEAD (the module does not
compile without the dep + API fit). No new tests — this task is "make them build + pass":
`roundtrip_full_set`, `reconstruct_from_k_subset_missing_data_shard`,
`proof_accepts_valid_rejects_corrupt`, `proof_rejects_wrong_index`, `under_k_falls_back`,
`encode_is_deterministic`, `bad_params_rejected`. Add ONE guard test if the v6 error type
surprises us: `encode_rejects_zero_shard_len` is already covered by `max(1)` — skip unless the
backend errors on 1-byte shards.

**Acceptance.** `cargo test -p torus-state erasure::` → 7/7 green; both `TODO(toolchain)`
comments deleted (or narrowed to a one-line "confirmed v6.x" note); no `unwrap` added on the
backend `Result`s (they already map to `ErasureError::Backend`).

---

## T2 — `StoredShard` struct + bincode read/write for `CF_NATIVE_SHARDS`
**Goal.** Define the durable custody record and its key/value codec so the encode path (T5)
and serve path (T7) share one representation. Value = shard bytes + Merkle proof +
`erasure_root` + `(k,n)` + `body_len` (exactly the fields the wire `NativeDaShardResponse`
needs, minus `present`).

**Files touched.** `crates/torus-state/src/cf.rs` (the CF name lives here) OR a new
`crates/torus-state/src/shard_store.rs` re-exported from `lib.rs` — prefer a small module next
to `erasure.rs` to keep `cf.rs` name-only. Add `bincode` to `torus-state` deps if not already
present (it is used elsewhere in the crate for native bodies).

**Struct (mirror the wire response field-for-field so T5/T7 map trivially):**
```rust
#[derive(Debug, Clone, PartialEq, Eq, <bincode-derive-or-serde matching the crate's native store>)]
pub struct StoredShard {
    pub shard_index: u16,
    pub shard_bytes: Vec<u8>,
    pub proof: Vec<[u8; 32]>,   // ShardProof.siblings as raw hashes (no B256 dep on wire)
    pub erasure_root: [u8; 32],
    pub k: u16,
    pub n: u16,
    pub body_len: u64,
}
```
Add free fns `shard_key(body_hash: &[u8;32], shard_index: u16) -> [u8; 34]`
(`body_hash ++ index.to_be_bytes()`) and `encode_stored_shard`/`decode_stored_shard`
(bincode). Confirm which bincode API the crate already uses (`bincode 1.x` `serialize`/
`deserialize` vs `bincode 2.x` `encode_to_vec`/`decode_from_slice`) and match it — the native
body store uses the same crate, reuse its pattern verbatim.

**RED-first test spec.** New `#[cfg(test)]` in the shard-store module:
- `stored_shard_bincode_roundtrip` — build a `StoredShard` with a 2-level proof, encode→decode,
  assert full equality (RED: `StoredShard` does not exist on HEAD).
- `shard_key_is_body_hash_plus_be_index` — assert `shard_key(&h, 258)` == `h ++ [0x01,0x02]`
  and that keys sort by `(body_hash, index)` (BE index ordering) so a prefix-scan on a
  body_hash yields shards `0..n` in order.
- `stored_shard_from_encoded_body` — convert an `EncodedBody` shard `i` (bytes + `proof(i)` +
  root + params + body_len) into a `StoredShard`, decode, and `verify_shard` it against the
  stored `erasure_root` → true (round-trips the exact fields the serve path will ship).

**Acceptance.** `cargo test -p torus-state shard` green; `StoredShard` re-exported from
`torus_state::` so `torus-consensus` and the serve glue can name it; no `unwrap` on decode
(return `Result`/log-and-skip like `get_native_da`).

---

## T3 — Tighter shard `MAX_MSG_SIZE` cap (replace the `MAX_NATIVE_DA_MSG_SIZE` placeholder)
**Goal.** A single shard is at most `body/k + proof + fixed header` — far below the 8 MB
whole-body cap. Give shards their own cap so a bomb/oversize shard frame is rejected at a
tight bound (the placeholder currently lets a shard frame be as large as a whole body).

**Files touched.** `crates/torus-network/src/caps.rs` (add the const + extend the ordering
assert); `crates/torus-network/src/codec.rs` (`NativeDaShardsCodec::MAX_MSG_SIZE` → new const,
delete the placeholder `TODO(T3.1)` comment).

**Cap value.** `NATIVE_BLOCK_BYTES_CAP = 6 MB` is the max body; smallest `k` is `f+1 = 2` at
n=3, so a data shard ≤ ~3 MB, plus a Merkle proof (`≤ ceil(log2(255)) = 8` × 32 B) + fixed
fields. Set `MAX_NATIVE_DA_SHARDS_MSG_SIZE = 4 * 1024 * 1024` (4 MB) — comfortably above the
worst-case single shard at the smallest set, below the whole-body cap. Document the
`body/k + proof` derivation in the const's doc-comment (the file is "single source of truth").

**RED-first test spec.** In `caps.rs` tests (the ordering test already exists):
- extend the ladder assert to `MAX_NATIVE_DA_SHARDS_MSG_SIZE <= MAX_NATIVE_DA_MSG_SIZE`
  (RED: the const does not exist).
- `shard_cap_admits_worst_case_shard` — assert `MAX_NATIVE_DA_SHARDS_MSG_SIZE >=
  NATIVE_BLOCK_BYTES_CAP / 2 + 8 * 32 + 64` (worst single data shard at k=2 + proof + header).
- In `codec.rs`, add `shard_frame_over_cap_rejected` (mirror `zstd_read_rejects_decompressed_over_cap`):
  a shard frame whose length prefix exceeds the shard cap errors `InvalidData` at read.

**Acceptance.** `cargo test -p torus-network caps` + `codec::` green; the cap ladder assertion
compiles with the new rung; `NativeDaShardsCodec::MAX_MSG_SIZE` no longer references
`MAX_NATIVE_DA_MSG_SIZE`.

---

## T4 — Epoch-stable `(k,n) = (f+1, |validators|)` derivation + re-shard-on-set-change policy
**Goal.** Every honest node must derive byte-identical `(k,n)` for a given body, or shards/roots
diverge. Bind `(k,n)` to the epoch-stable validator-set size (design Q1): `n = |validators|`,
`k = f + 1` where `f = (n - 1) / 3` (BFT threshold). Re-shard only at an epoch boundary (set
change), never mid-epoch.

**Files touched.** `crates/torus-state/src/erasure.rs` (add
`ErasureParams::for_validator_set(n_validators: usize) -> ErasureParams`, pure fn) — keep the
policy in the same crate as `ErasureParams`. The caller in T5 reads `n` from the app's
`ValidatorSet` at mirror time.

**Policy.**
```
f = (n - 1) / 3          // classic BFT: n >= 3f + 1
k = f + 1                // reconstruct from any f+1 honest custodians
n = |validators|         // Polkadot-style validator-custody
// n < 2  → degenerate (single node): k = n = 1 (no redundancy, still well-formed)
```
Re-shard policy: `(k,n)` is fixed for the lifetime of a body's erasure set. Because bodies are
mirrored under the CURRENT set's `(k,n)` at proposal time and a body is only recovered within a
bounded window, an epoch flip that changes `n` simply produces new-`(k,n)` shards for
subsequently mirrored bodies; already-stored shards keep their own `(k,n)` (carried in
`StoredShard`), so a fetcher always reconstructs with the params the shards were built under. No
re-shard of historical bodies.

**RED-first test spec.** In `erasure.rs` tests:
- `params_for_set_matches_bft_table` — table test asserting the mapping
  `{3→(2,3), 4→(2,4), 7→(3,7), 10→(4,10), 21→(7,21)}`, which matches the design doc's arithmetic
  table (`docs/plans/sprint5-erasure-coding.md` §Key arithmetic). This test CODIFIES the n=3
  decision (see Open decisions): strict `f=(n-1)/3` gives n=3→f=0→k=1 (no multi-source spread),
  so the fn uses a `k = max(2, f+1)` FLOOR so even n=3 fetches from ≥2 sources (hotspot removal
  is the n=3 win). The test pins that floor.
- `params_are_deterministic_for_same_n` — same `n` twice → identical params.
- `stored_shard_carries_its_own_params` — a shard built at n=3 still reconstructs after the fn
  would return n=4 for a later epoch (params travel with the shard, not re-derived at fetch).

**Acceptance.** The n=3 `k` decision is written down in the fn doc + the test; mapping matches
the design doc table; `cargo test -p torus-state erasure::params` green.

---

## T5 — Proposer-side encode+persist at body-mirror time
**Goal.** When a proposer mirrors bodies to the durable DA store (`mirror_native_to_da`),
ALSO erasure-encode each body under the current `(k,n)` and persist all `n` shards (bytes +
proof + root + params + body_len) into `CF_NATIVE_SHARDS`, so the node can serve any shard it
custodies. Additive: the whole-body mirror is unchanged.

**Files touched.** `crates/torus-mempool/src/lib.rs` (`mirror_native_to_da`, `:630`) OR a new
sibling `encode_and_store_shards` invoked right after `da_store.put_batch` — prefer a separate
fn so the hot `put_batch` stays lean and the encode is a clearly-additive step. The mempool
needs `(k,n)`: pass `ErasureParams` in, derived by the caller in `app.rs` from the live
`ValidatorSet` (keeps the mempool free of validator-set identity, mirroring how peer selection
stays out of the consensus layer). Persist via `StateDb` `put_cf_raw` batch into
`CF_NATIVE_SHARDS` keyed by `shard_key(body_hash, i)`.

**Encode body identity.** The erasure set is over the SAME bytes the whole-body path stores:
`bincode(SignedNativeAction)` keyed by its recomputed action-hash (that is `body_hash`). This
guarantees the body-hash backstop (T8) works: reconstruct → `bincode` → hash → equals the
proposal's `native_action_hash`.

**RED-first test spec.** New test in `torus-consensus` (integration, has a `StateDb`) or
`torus-mempool`:
- `mirror_encodes_and_persists_n_shards` — mirror a batch of native actions under params
  `(2,3)`; assert `CF_NATIVE_SHARDS` contains exactly `n` rows per body, keyed `0..n`; decode
  each `StoredShard` and `verify_shard(root, i, bytes, proof)` → true for all `i` (RED: no
  encode path exists — the CF stays empty on HEAD).
- `stored_shards_reconstruct_original_body` — read the `k` data shards back out, `reconstruct`,
  and assert the bytes equal the original `bincode(SignedNativeAction)` and re-hash to the
  action-hash used as the key.
- `mirror_still_populates_whole_body_store` — the existing `get_native_da(hash)` still returns
  the body (additive guarantee — encode must not disturb the whole-body path).

**Acceptance.** `cargo test -p torus-consensus mirror_encodes` (and the whole-body regression)
green; encode runs OFF the `produce_block` critical timing if a µs bench shows cost (the S395
note says `mirror_native_to_da` is on the leader critical path) — gate the shard encode behind a
`spawn_blocking`/background queue if hot; otherwise inline at n=3. Document the decision.

---

## T6 — Register `Behaviour<NativeDaShardsCodec>` in `behaviour.rs` + swarm wiring
**Goal.** Add the request/response behaviour so the swarm can send/receive shard messages,
mirroring `native_da` (shards are `/1.0` only for now; a future `/2.0` adds zstd like
`native-da/2.0`). No serve logic yet (T7) — this is pure plumbing so the event enum + field
exist.

**Files touched.** `crates/torus-network/src/behaviour.rs` (add field
`pub native_da_shards: request_response::Behaviour<NativeDaShardsCodec>`; construct in `new`
with `[(StreamProtocol::new(NATIVE_DA_SHARDS_PROTOCOL), ProtocolSupport::Full)]` + 10 s
timeout; add to the `Ok(Self { .. })`; import `NativeDaShardsCodec` + `NATIVE_DA_SHARDS_PROTOCOL`
from `codec`). `crates/torus-network/src/swarm.rs` (the derived
`TorusBehaviourEvent::NativeDaShards(_)` variant must be handled — add a
`SwarmEvent::Behaviour(TorusBehaviourEvent::NativeDaShards(_)) => {}` arm so the match stays
exhaustive; import the shard req/resp types).

**Mixed-binary safety.** `request_response::Behaviour` only opens a substream on a protocol the
remote advertised; a pre-shard peer never advertises `/torus/native-da-shards/1.0`, so
`send_request` to it yields `OutboundFailure` (unsupported-protocol) → the fetcher treats that
peer as "no shard" and falls back (T9). SAME per-peer negotiation the zstd 2.0-first work proved
safe (s356).

**RED-first test spec.** In `behaviour.rs` tests (a construction test already exists):
- `behaviour_registers_shard_protocol` — build `TorusBehaviour::new(..)`; assert it constructs
  (RED: the field/import don't exist → won't compile). Compile-gate test; the real coverage is
  the roundtrip already in `codec.rs`.

**Acceptance.** `cargo build -p torus-network` compiles with the new field + exhaustive event
match; the `NetworkBehaviour` derive still succeeds (a `request_response::Behaviour` field is
the identical shape as `native_da`); no serve/fetch behavior yet (arms are `=> {}` / TODO(T7,T8)).

---

## T7 — Serve path: answer `/torus/native-da-shards/1.0` off the consensus loop
**Goal.** On an inbound shard request, look up `StoredShard` for `(body_hash, shard_index)` in
`CF_NATIVE_SHARDS` OFF the consensus event loop (RocksDB point-get, like the whole-body serve at
`swarm.rs:1301`), and reply `present=true` with bytes+proof+root+params, or `present=false` when
not custodied. Reuse the `da_serve` admission gate + `spawn_blocking` + a
`DaShardServeDone`-style response-post.

**Files touched.** `crates/torus-network/src/swarm.rs` (replace the T6 stub arm for
`TorusBehaviourEvent::NativeDaShards(Message::Request{..})` with the off-loop serve; add a
`shared.native_da_shards: RwLock<Option<ShardStore>>` handle OR extend the existing `native_da`
store handle with a `get_shard` method; add a `DaShardServeDone` swarm action + `send_response`
on `native_da_shards`). A free fn `serve_native_da_shard(store, body_hash, index) ->
NativeDaShardResponse` (mirror `serve_native_da_bodies`, `swarm.rs:221`).
`crates/torus-network/src/bridge.rs` (register the shard store handle like `native_da` at `:368`).

**Never block the loop.** The read is a point-get of a ≤ few-MB value; serve it in the same
bounded `spawn_blocking` pool as whole-body (the S387 lesson: inline DA serving starves the
loop). Overloaded/banned → answer `present=false` inline (mirrors the all-empty whole-body
response) so the requester rotates.

**RED-first test spec.**
- `serve_shard_present_for_custodied` — seed `CF_NATIVE_SHARDS` with an `EncodedBody`'s shards;
  `serve_native_da_shard(store, body_hash, 1)` → `present=true`, `shard_bytes`/`proof`/`root`
  match, and `verify_shard(resp.root, 1, resp.shard_bytes, ShardProof{siblings})` → true.
- `serve_shard_absent_returns_present_false` — request an index the node does not custody →
  `present=false`, empty bytes/proof (mirror the whole-body not-found empty entry).
- `serve_shard_out_of_range_index` — `shard_index >= n` → `present=false` (no panic, no OOB).

**Acceptance.** `cargo test -p torus-network serve_shard` green; the serve arm uses
`spawn_blocking` (grep-confirmable it is not inline on the loop); overloaded path answers
`present=false`; whole-body serve untouched.

---

## T8 — Recovery fetch/reconstruct loop (k shards, k DISTINCT peers, verify-then-reconstruct)
**Goal.** When a node reaches reconstruction missing a body, request shard `i` from `k`
DISTINCT peers, `verify_shard` EACH before use, `reconstruct` from any `k` verified shards,
apply the **body-hash backstop**, and absorb the rebuilt body into the whole-body store
(`CF_NATIVE_PENDING`) so normal reconstruction proceeds. This is the payload of Option A.

**Files touched.** `crates/torus-consensus/src/app.rs` (extend the DA-recovery trigger that
today calls `NativeDaFetcher::fetch` for whole bodies to FIRST attempt a shard gather; a new
`ShardFetcher` trait analogous to `NativeDaFetcher` — `fetch_shards(body_hash, indices)` +
`drain_shards()` — or extend `NativeDaFetcher`). `crates/torus-network/src/swarm.rs` (outbound
`native_da_shards.send_request` to distinct peers; inbound `Message::Response` → verify-gate
then queue to a `shared.native_da_shards_inbound` collector keyed by `body_hash`).
`crates/torus-consensus` reconstruct glue (assemble `Vec<Option<Vec<u8>>>` of length `n`,
`reconstruct`, hash-check, absorb via `mempool.mirror_native_to_da`).

**Distinct-peer + verify rules (hard).**
1. Request the `k` (or a few extra for redundancy) shards each from a DIFFERENT connected peer
   — the whole point is to spread the serve; never gather `k` from one peer.
2. On each response: `present==true` AND `verify_shard(root, index, bytes, proof)` → accept;
   else discard that shard (try another peer/index). **Verify BEFORE `reconstruct`** — a
   Byzantine peer must not poison the rebuild (design §Integrity).
3. Once `≥ k` verified shards for one `body_hash` (with consistent `(root,k,n,body_len)`),
   `reconstruct` → **body-hash backstop**: `hash(body) == body_hash`? If not, discard the whole
   set and fall back (T9). If yes, absorb.

**RED-first test spec.** Integration test in `torus-consensus` driving the reconstruct glue
directly (no live swarm — feed it `NativeDaShardResponse`s):
- `reconstruct_from_k_verified_shards_absorbs_body` — encode a body `(2,3)`, hand the glue 2
  verified shards from 2 distinct simulated peers → it reconstructs, the body-hash backstop
  passes, and `get_native_da(body_hash)` now returns the body (RED: no glue on HEAD).
- `corrupt_shard_is_rejected_before_reconstruct` — one of the `k` responses has a flipped byte
  (proof fails) → it is discarded, the glue waits for another shard rather than feeding poison
  to `reconstruct`; with only the corrupt one available it does NOT absorb a wrong body.
- `body_hash_backstop_rejects_mismatched_set` — feed `k` shards whose `root` verifies but whose
  reconstructed body hashes to the WRONG `body_hash` (attacker forged a self-consistent erasure
  set for different bytes) → backstop rejects, no absorb.
- `k_shards_from_one_peer_not_counted_as_spread` — two shards tagged from the same peer id count
  as one source for the distinct-peer requirement.

**Acceptance.** `cargo test -p torus-consensus reconstruct_from_k` green; verify happens before
reconstruct (test proves poison rejection); body-hash backstop is unconditional; a successful
gather results in `get_native_da` returning the body (feeds the existing reconstruct path).

---

## T9 — Never-wedge fallback to whole-body pull + per-peer protocol negotiation
**Goal.** ANY shard-path failure — `< k` verified shards, all peers `present=false`, a
pre-shard peer (unsupported protocol), proof/backstop failures, timeout — falls back to the
EXISTING `/torus/native-da/{1.0,2.0}` whole-body pull. Erasure is strictly additive; never a new
way to wedge (`native-da-hash-only-push.md` guardrail).

**Files touched.** `crates/torus-consensus/src/app.rs` (the recovery trigger: attempt shard
gather with a bounded deadline/attempt budget; on failure, invoke the existing
`NativeDaFetcher::fetch(vec![body_hash])` whole-body path — the current path, unchanged).
`crates/torus-network/src/swarm.rs` (a `NativeDaShards` `OutboundFailure` for an
unsupported-protocol peer increments a metric and signals "no shard from this peer" — the glue
counts it toward the fall-back trigger, does not error).

**Fallback triggers (all → whole-body pull, never wedge):**
- fewer than `k` distinct peers advertise the shard protocol (mixed-version fleet),
- fewer than `k` verified shards gathered within the attempt budget,
- reconstruct error or body-hash-backstop mismatch,
- shard cap exceeded / decode error on a response.

**RED-first test spec.**
- `under_k_shards_falls_back_to_whole_body` — glue given only `k-1` verified shards within the
  budget → it calls the whole-body `fetch` (assert via a mock `NativeDaFetcher` that records the
  requested hash) and does NOT wedge (RED: no fallback wiring on HEAD).
- `pre_shard_peer_never_errors_the_gather` — a simulated `OutboundFailure(UnsupportedProtocol)`
  from a peer is counted as "no shard" and, with too few shard-capable peers, triggers whole-body
  fallback — zero surfaced errors.
- `all_present_false_falls_back` — every peer answers `present=false` (none custody) → whole-body
  fallback fires.
- (reuses `erasure.rs::under_k_falls_back` at the primitive level — this task proves the CALLER
  honors the `InsufficientShards` signal.)

**Acceptance.** `cargo test -p torus-consensus falls_back` green; a mixed-version fleet where
`< k` peers speak the shard protocol reconstructs every body via whole-body fallback with no
wedge; the whole-body pull path is byte-for-byte the pre-existing one (no regression).

---

## T10 — Verification: compile+unit, mixed-version devnet shake-out, mesh probe
**Goal.** Prove Phase A end-to-end and that it removes the s338 single-source signature without
regressing the healthy path — before any friend deploy.

**Files touched.** None (procedural) — plus a devnet runner note under `devnet/` and a `.claude`
handoff. Optionally an in-proc two-app integration test in `torus-consensus` if the harness
supports it (one shard-capable, one pre-shard).

**Steps + acceptance criteria.**
1. **Compile + unit:** `cargo test -p torus-state -p torus-network -p torus-consensus` green,
   including all T1–T9 tests. Zero new clippy warnings on the touched files.
2. **Mixed-version devnet shake-out** (mirror the s356 zstd shake-out): a set where 1 peer is on
   the PRE-shard binary and the rest are shard-capable; drive a bs-sweep that forces recovery
   pulls. Acceptance: the pre-shard peer reconstructs every body via whole-body fallback;
   shard-capable peers reconstruct from `k` distinct sources; **zero protocol errors**; **no
   wedge**; **dup-factor ≤ 1.05** (design §Verification).
3. **Mesh probe:** capture per-link body bytes + the s338 counters (pull timeouts + substream
   exhaustions to one peer) under a saturating native-load bs-sweep, before vs after. Acceptance:
   the single-source signature (155 timeouts + 238 substream exhaustions to ONE peer, mem
   `55d8e647`) is **gone** — serve load spread across `k` sources; per-link recovery bytes ≈
   `body/k`.
4. **Regression:** the healthy bs100 fast path (gossip pre-spread + hash-only push) is UNCHANGED
   — Phase A touches only the recovery pull, so bs100 throughput must match the pre-change
   baseline (guardrail from `native-da-hash-only-push.md`).

**Acceptance.** All four criteria met; results recorded to memory (`remember_this`,
type:decision) + a devnet CSV; only THEN is Phase A greenlit for a friend deploy (the wire-change
prereq — every friend on the current binary).

---

## T11 — DEFERRED (genesis-relaunch-gated): bind `erasure_root` into `CompactBlock`/header
**Goal (design Q2).** Optionally authenticate the erasure set at consensus by committing
`erasure_root` alongside `native_action_hashes` in `CompactBlock` (or a new header field). This
lets a fetcher trust the root WITHOUT relying solely on the body-hash backstop, and is a
prerequisite for a stronger Phase B custody proof.

**Why deferred / sequenced LAST of the wire work.** The consensus block identity is `data_hash`
over the datum bytes (`app.rs:701`, `COMPACT_PROPOSALS`), so ANY new committed field changes
`data_hash` → every validator MUST run the changed binary in a **coordinated genesis-era
relaunch** (same class as flipping `COMPACT_PROPOSALS`). Phase A does NOT need this — the
body-hash backstop (T8) already prevents a forged set from being absorbed. So T11 batches with
the next genesis relaunch, NOT the Phase A friend deploy.

**Files touched (when greenlit).** `CompactBlock` definition (add `erasure_roots: Vec<[u8;32]>`
parallel to `native_action_hashes`), the proposer fill path, the datum encode/`data_hash`, and
the fetcher (prefer the committed root over the response's self-reported root). A genesis era
bump (mirror the S434 genesis + relaunch ops docs).

**RED-first test spec.**
- `compact_block_commits_erasure_roots_roundtrip` — a `CompactBlock` with parallel
  `native_action_hashes`/`erasure_roots` encodes/decodes and its `data_hash` is stable +
  DIFFERENT from a no-roots block (proves the consensus-format touch → relaunch requirement).
- `fetcher_prefers_committed_root_over_response` — when a committed `erasure_root` exists, a
  shard whose response root disagrees is rejected pre-reconstruct.

**Acceptance.** Held until a coordinated relaunch is scheduled; test written RED and parked;
explicitly OUT of the Phase A ship. Ops doc pins the genesis commit like the S434 relaunch notes.

---

## T12 — LATER PHASE (Option B): erasure at INGRESS dispersal
**Goal (design Option B, the headline).** Replace/augment full-body gossip pre-spread with
shard DISPERSAL: erasure-code at ingress and send shard `i` to validator `i`, so each peer
receives `body/k` instead of the whole body; at proposal time each validator already custodies
its shard and reconstructs from the quorum. Attacks the DOMINANT (gossip) byte path, not just
the recovery tail.

**Why a separate later phase.** Largest blast radius — touches the consensus-critical pre-spread
and the healthy bs100 path (explicit guardrail), puts reconstruction on the common path (latency
vs the ~500 ms view timeout), and needs a dispersal scheduler + custody bookkeeping. Per Option
C, promote to B ONLY once (i) the validator set is large enough that the `1/k` win dominates and
(ii) the T10 mesh probe shows gossip pre-spread bytes — not the recovery tail — are the binding
ceiling (design Q5).

**Reuses.** The entire Phase A shard codec, `EncodedBody`, `StoredShard`, `CF_NATIVE_SHARDS`,
`ErasureParams::for_validator_set`, and the shard wire protocol. New: a dispersal scheduler, a
per-validator custody assignment (validator `i` ↔ shard `i`), and the ingress-side encode.

**Sequencing.** Only after T10 data justifies it; batch any consensus-format piece (e.g. a
custody/availability attestation) with a genesis relaunch (same rule as T11). Full TDD breakdown
for B is authored in its OWN impl doc when greenlit — this task is a placeholder to keep the
phase boundary explicit.

---

## Test inventory (Phase A, all RED-first on HEAD)
| Task | New/greened tests |
|------|-------------------|
| T1 | 7 existing `erasure::` tests go green (were RED — module didn't compile) |
| T2 | `stored_shard_bincode_roundtrip`, `shard_key_is_body_hash_plus_be_index`, `stored_shard_from_encoded_body` |
| T3 | cap-ladder assert extended, `shard_cap_admits_worst_case_shard`, `shard_frame_over_cap_rejected` |
| T4 | `params_for_set_matches_bft_table`, `params_are_deterministic_for_same_n`, `stored_shard_carries_its_own_params` |
| T5 | `mirror_encodes_and_persists_n_shards`, `stored_shards_reconstruct_original_body`, `mirror_still_populates_whole_body_store` |
| T6 | `behaviour_registers_shard_protocol` (compile-gate) |
| T7 | `serve_shard_present_for_custodied`, `serve_shard_absent_returns_present_false`, `serve_shard_out_of_range_index` |
| T8 | `reconstruct_from_k_verified_shards_absorbs_body`, `corrupt_shard_is_rejected_before_reconstruct`, `body_hash_backstop_rejects_mismatched_set`, `k_shards_from_one_peer_not_counted_as_spread` |
| T9 | `under_k_shards_falls_back_to_whole_body`, `pre_shard_peer_never_errors_the_gather`, `all_present_false_falls_back` |
| T10 | devnet shake-out + mesh probe (procedural acceptance) |
| T11 | `compact_block_commits_erasure_roots_roundtrip`, `fetcher_prefers_committed_root_over_response` (parked, relaunch-gated) |

## Open decisions this plan forces (resolve in-task, not later)
- **T4:** the `k` value at n=3 — strict `f=(n-1)/3 → k=1` (no spread) vs a `k=max(2,f+1)` floor
  that keeps ≥2 sources even at n=3 (matches the design doc table). The plan recommends the
  floor: hotspot removal is the n=3 win, and the table is the committed target.
- **T5:** inline vs background shard-encode at `mirror_native_to_da` — decided by a µs bench on
  the `produce_block` critical path (S395 note). Default inline at n=3, `spawn_blocking` if hot.
- **T11 vs A:** confirmed — `erasure_root` header binding is NOT required for Phase A (body-hash
  backstop suffices); it is a Phase-B-strength / relaunch item.
