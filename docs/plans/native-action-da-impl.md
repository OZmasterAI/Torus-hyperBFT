# Implementation Plan: Native-Action Data-Availability (DA) Layer

**Branch:** suggest `phase-c-native-da` off `cap100-3val-perf` · **Created:** 2026-06-07 · **Status:** PLAN
**Design:** Option B from `docs/plans/native-action-da.md` — durable body store (keyed by action-hash, decoupled from the 60s nonce gate) + hardened push-primary + RARE pull-fallback by-hash (`/torus/native-da/1.0`) + remove blacklist/freeze footguns.

## Design Decision (from brainstorm)
For **400k+ orders/sec**, native-action bodies must travel **out-of-band** (a block referencing ~20–40k orders ≈ 1–2 MB ≫ the 256 KB `max_consensus_message_size`). The block carries only action hashes (`CompactBlock`). Delivery is **push-primary, fetch-rare** (s297 Option D, mem `c19a35aa`) — a per-block pull was tried and abandoned (46→211 ms regression, mem `a6cf33a9`), so pull is a **fallback only**. The livelock (mem `28e1a821`) proved the deferred pieces are mandatory: a **durable** body store, a **fetch-on-miss** fallback, and removal of the **blacklist/freeze footguns**.

## Success Criteria
- **Liveness (non-negotiable):** a `CompactBlock` whose bodies are absent from the nonce-gated mempool but available in the DA store (or fetchable from a peer) **validates, commits, and syncs** without stalling, blacklisting peers, or freezing `last_header`. No `justify_block_known=false` livelock under flood.
- **Durability:** native-action bodies survive the 60s nonce window and a process restart; a body referenced by a proposed/committed block is always reconstructable.
- **Latency:** push covers the common case → pull-fetch fires **rarely** (assert low pull rate under load); no per-block round-trip (no May17-style regression).
- **Throughput:** with compact proposals re-enabled, the bs=500 flood that collapsed before (mem `41ca452`) now holds; orders/sec scales toward 400k (block-speed wall is Phase A, run in parallel).
- **Determinism/compat:** consensus block identity = `data_hash` over the `CompactBlock` datum bytes (app.rs:923) → all validators MUST emit the same encoding (version discipline). No change to the EVM/RPC header hash (`keccak256(canonical_header_bytes)`).
- **No regressions:** `four_node_consensus` + existing suites green.

## Tasks

### Task 1 — Durable DA body store keyed by action-hash, decoupled from the nonce gate
- **Test first** (`torus-state` or `torus-mempool`): `put_body(hash, action)` then `get_body(hash)` returns it; absent → `None`; a body whose nonce is >60s old can still be stored+fetched (proves nonce-gate decoupling); survives a store reopen (CF-backed).
- **Implementation:** add `CF_NATIVE_PENDING` to `crates/torus-state/src/cf.rs` (const + `ALL_CF_NAMES`). New `NativeDaStore` (in `torus-mempool` or `torus-state`) over `CF_NATIVE_PENDING`: key = `compute_action_hash(&action)` (32B), value = bincode(`SignedNativeAction`). API: `put(&SignedNativeAction)`, `get(&B256) -> Option<SignedNativeAction>`, `remove(&[B256])`. Start CF-backed (durable); an in-mem read cache is a later optimization (Open Q1).
- **Verify:** `cargo test -p torus-state native_da_store_roundtrip` (or `-p torus-mempool`)
- **Depends on:** —

### Task 2 — Populate the DA store on every ingest + produce path
- **Test first:** after `add_native_action`, `add_native_action_from_gossip_trusted`, and `produce_block`, each action's body is in the DA store by hash — **including** when the nonce-gate would reject mempool admission.
- **Implementation:** mirror bodies into `NativeDaStore` from the mempool insert paths (`torus-mempool/src/lib.rs:229/245/273`) and from `produce_block` (`torus-consensus/src/app.rs:~880-915`). The gossip/push receive path writes to the DA store even on nonce-stale (decoupled), while still gating *mempool* admission.
- **Verify:** `cargo test -p torus-mempool da_store_populated_on_ingest`
- **Depends on:** 1

### Task 3 — Reconstruct from the DA store in validate/sync/commit (the reproduction → fix)
- **Test first (reproduction):** a `CompactBlock` referencing a body present in the DA store but **absent from the nonce-gated mempool** now reconstructs and validates `Valid` (today → `Invalid`, app.rs:1024) and commits (today → silent `return`, app.rs:1135). Missing-everywhere case returns a typed `NeedFetch(hashes)` rather than `Invalid`.
- **Implementation:** replace `mempool.get_native_by_hash` lookups at `app.rs:977` (validate) and `app.rs:1132` (on_committed_block) with `da_store.get`; make `validate_block_for_sync` (app.rs:1102) reconstruct from the DA store (drop the misleading bare delegate); remove the early-`return`-before-`last_header` (app.rs:1135 → 1164).
- **Verify:** `cargo test -p torus-consensus compactblock_reconstructs_from_da_store`
- **Depends on:** 2

### Task 4 — Remove the footguns (no blacklist for missing body, no last_header freeze)
- **Test first:** during sync, a body-less block does **not** blacklist the serving peer (assert peer still in the available set); `on_committed_block` never advances consensus while silently failing app execution (loud error or fetch, never silent desync).
- **Implementation:** `crates/hotstuff_rs/src/block_sync/client.rs:333` — distinguish `Invalid` (cryptographically bad) from `NeedFetch`/cannot-reconstruct; only blacklist the former. `app.rs on_committed_block` — on unreconstructable committed block, trigger fetch (Task 6) or hard-error, never silent `return`.
- **Verify:** `cargo test -p hotstuff_rs -p torus-consensus missing_body_does_not_blacklist`
- **Depends on:** 3

### Task 5 — `/torus/native-da/1.0` request_response protocol (serve bodies by-hash)
- **Test first** (`torus-network`): codec roundtrip for `NativeDaRequest{hashes}` / `NativeDaResponse{bodies}`; a node serving the protocol returns bodies for known hashes, empty for unknown.
- **Implementation:** add `native_da: request_response::Behaviour<NativeDaCodec>` to `crates/torus-network/src/behaviour.rs:16-29` (template: existing `block_data` at L26); add `NativeDaRequest`/`NativeDaResponse` + codec in `codec.rs` (template: `BlockDataCodec`); `NetworkCommand::FetchNativeActions{target,hashes}` + serve-from-DA-store handler + receive→inbound channel in `swarm.rs` (template: block-data at swarm.rs:620-683). No hotstuff_rs changes.
- **Verify:** `cargo test -p torus-network native_da_protocol_roundtrip`
- **Depends on:** 1

### Task 6 — Wire the RARE pull-fallback into the miss path
- **Test first** (integration): a `CompactBlock` whose bodies are absent locally but present on a peer → node issues `FetchNativeActions`, receives bodies, inserts to DA store, reconstructs, and validates/commits — no stall, no blacklist. Assert it does **not** fire when bodies are already local (rarity).
- **Implementation:** in the `NeedFetch` path (Task 3), send `FetchNativeActions(missing)`, await on the inbound channel with a **bounded** timeout (well under view timeout, cf. the existing 100ms retry budget at app.rs), insert results to the DA store, retry reconstruct once.
- **Verify:** `cargo test -p torus-integration-tests native_da_pull_fallback_recovers`
- **Depends on:** 3, 5

### Task 7 — Harden the push-primary (queue/retry, no silent drops) feeding the DA store
- **Test first:** when a target validator isn't yet in the peer map, the push is **queued and delivered on connect** (not dropped); received bodies land in the DA store.
- **Implementation:** `torus-network/src/swarm.rs` `BroadcastNativeActions` (swarm.rs:870) — replace the bounded(4) fire-and-forget `try_send` with a queue + retry/backpressure; keep the reconnect re-push (swarm.rs:733-749). Receive path → DA store (Task 2).
- **Verify:** `cargo test -p torus-network push_queues_until_peer_connected`
- **Depends on:** 2

### Task 8 — Re-enable CompactBlock proposals behind the reliable DA; drop the redundant double-send
- **Test first:** update `produce_block_datum_is_full_self_contained_block` (app.rs:1337) → `produce_block_datum_is_compact`; assert the datum is a `CompactBlock` and bodies are available via DA (push/pull) for a peer to reconstruct.
- **Implementation:** `app.rs encode_proposal_datum` (app.rs:469) → `CompactBlock::from_block(block)`; bodies already mirrored to DA (Task 2) + pushed (Task 7); remove the now-redundant inline-body duplication. Gate behind a config/version flag for staged rollout.
- **Verify:** `cargo test -p torus-consensus compact_proposal_disseminates`
- **Depends on:** 6, 7

### Task 9 — MILESTONE: scale proof toward 400k + coordinated relaunch
- **Bench/devnet:** `bench-throughput` at rising batch sizes — assert no missing-action stalls, DA **pull rate stays low** (push covers common case), orders/sec climbs toward target; re-run the bs=500 flood that collapsed at 2.56 MB (mem `41ca452`) and confirm it holds. Coordinated fresh testnet relaunch on the fixed binary (all validators).
- **Verify:** bench output: stalls=0, pull-rate low, orders/sec ↑; devnet no `justify_block_known=false`.
- **Depends on:** 8 (block-speed to 400k also needs **Phase A incremental state-root** — parallel track).

## Verification (end-to-end)
1. `cargo test --workspace` green incl. the new DA tests + `four_node_consensus`.
2. Reproduction test (Task 3) is red before the fix, green after.
3. Devnet flood (bs up to 500): no missing-action stalls, no peer blacklisting, height advances; DA pull rate logged and low.
4. Coordinated testnet relaunch: chain produces blocks under native flood without wedging.

## Rollback
- The DA store + protocol are additive (new CF, new behaviour). To revert delivery: flip `encode_proposal_datum` back to full blocks (Task 8 flag) — the durable store + fetch-on-miss + footgun fixes are safe to keep (they only add robustness). `CF_NATIVE_PENDING` becomes dead but harmless.
- No consensus-format change beyond the compact-vs-full datum encoding, which is gated by the Task 8 flag + version discipline.

## Cross-cutting
- **Version discipline:** consensus block identity hashes the datum bytes, so all validators must emit the same encoding. Re-enabling compact (Task 8) requires every validator on the fixed binary; ship behind a coordinated flag.
- **Parallel track — Phase A (incremental state-root):** the *other* 400k wall (block speed). DA (this plan) unblocks orders/block; Phase A unblocks blocks/sec. Both needed for 400k.
