# Implementation Plan: 200k Native Orders/sec @ Sub-100ms Blocks

**Branch:** `cap100-3val-perf` · **Created:** 2026-06-06 · **Status:** PLAN (not started)

## Design Decision (from this session's brainstorm)

User goals, confirmed:
- **Target metric:** 200,000 native orders/sec **end-to-end committed** through BFT consensus.
- **Block time:** **sub-100ms** (hard constraint).
- **Signing model:** orders arrive **batched from market makers** (few submitters), *not* 20k distinct end-users each signing one order.

The batched-signing answer is decisive: it makes a `PlaceOrderBatch(Vec<PlaceOrderParams>)` action the keystone, collapsing the two cheap walls (message size + signature count) and leaving incremental state-root as the one hard prerequisite.

## The governing equation

```
throughput = orders_per_block × blocks_per_second
200,000/s ÷ 10 blk/s (@100ms) = 20,000 orders/block      (cap today: 100 actions)
200,000/s ÷ 20 blk/s (@ 50ms) = 10,000 orders/block
```

Today: cap 100 actions/block, ~280ms/block under load → **~330 orders/s measured**. The matching engine alone does **~750k/s** in-process (`native_executor.rs:328` `execute_batch`, per-market parallel matching at `:434`). **Consensus + the per-block tax is the funnel, not matching.**

## The three walls (all verified in code)

| # | Wall | Evidence | Phase that kills it |
|---|------|----------|---------------------|
| **W1** | State root is **O(total chain state), recomputed every block on the hot path** | `torus-bridge/src/state_root.rs:98-184` iterates *all* accounts (`:108`) + re-reads *all* storage (`:152`); `:51-91` re-keccaks *all* order-books/balances/positions. Incremental path never built (`trie.rs:48` comment admits it). Root goes in the voted header → can't be hidden by pipelining. | **A** (the sub-100ms unlock) |
| **W2** | **256KB** `max_consensus_message_size` vs block size | `torus-network/src/config.rs:79`. Full self-contained blocks (compact reverted `66da812`). ~150 B/order → 256KB holds **~1,700 orders max**; 20k orders ≈ 3MB (12× over). | **B** (batching) + **C** (size bump) |
| **W3** | **One order = one secp256k1 ecrecover** | `NativeAction::PlaceOrder(PlaceOrderParams)` (`lib.rs:461`), no batch variant. Verify is *already* rayon-parallel + single-recovery (`eip712.rs:831`, done). 20k ecrecover/block ≈ 125ms on 8 cores → over budget. | **B** (batching → ~200 sigs/block) |

**Key insight:** W2 and W3 share one solution — **batch many orders under one signature**. 100 orders/batch turns 20,000 orders into ~200 signed actions: ~200 ecrecovers/block (sub-ms) and per-order signature bytes vanish. W1 has no shortcut and is its own phase.

**Bonus the batch unlocks for free:** the block cap counts **actions, not orders** (`app.rs:858` `select_native_for_block_with_senders_excluding(NATIVE_TOTAL_BLOCK_CAP, …)`), so `orders/block = cap × batch_size` with zero selection-logic change. And RPC ingress — the current submission ceiling (`submit_semaphore = 16` → "server overloaded", `torus.rs:597`; `serde_json` deserialize/reserialize `:606`/`:620`; full sig-verify per call `:615`) — sees **~100× fewer calls/sigs/acquisitions**, dissolving the 241/s submission bound.

---

## Phase roadmap (each phase independently provable)

| Phase | Goal | Order rationale | Risk |
|-------|------|-----------------|------|
| **B — Batch action** | `PlaceOrderBatch(Vec<…>)`, one nonce/sig/batch, configurable per-batch + per-block order caps | Cheapest, additive, biggest immediate orders/sec jump; keeps blocks sub-100ms on small (devnet) state | **Low** |
| **A — Incremental state root** | Replace full-scan EVM root + native re-keccak with dirty-only updates | **Prerequisite for sub-100ms as state grows** — no shortcut; invisible on tiny devnet, fatal at scale | **High** (determinism-critical; gate behind CI root-match — prior mismatch `f3d3f858`) |
| **C — Caps + dissemination** | Raise `NATIVE_TOTAL_BLOCK_CAP`, bump `max_consensus_message_size` to fit 1–2MB blocks, validate full-block propagation across 3 validators | Only matters once B+A let us fill big blocks; this is a *size* concern, not the old compact-block availability bug | **Medium** (confirm no stall at 1–2MB) |
| **D — Off-box bench** | Clean bigger box, client off-box, honest end-to-end numbers per phase | Current 8-core host is **load-38 contended** (5 devnet + live testnet) — can *never* show 200k | None (measurement) |

**Honest expectation:** B + a message-size bump should jump devnet from ~330/s into the **thousands–tens-of-thousands/s** while staying sub-100ms (small state). Full **200k/s end-to-end** requires A + C + D on real hardware — a multi-step program, not one commit. The 750k/s engine proves the headroom; the work is getting orders *through* consensus.

---

## Success Criteria

**Phase B (this plan's executable scope):**
- A single `SignedNativeAction` carrying `PlaceOrderBatch` of N orders verifies with **one** signature recovery and executes all N deterministically (same order IDs on every validator).
- Per-batch order cap and per-block total-order cap are **configurable** (`rate_limit.rs`) and enforced at RPC ingress *and* `validate_block`.
- A duplicate committed batch executes **at most once** (extends the `2e52851` dup-native guard; one batch = one `(sender, nonce)`).
- Devnet flood with batched submitter: **orders/s ≫ 330**, duplication factor **~1.0**, blocks **sub-100ms**, no stall.
- All existing tests stay green: `torus-types`, `torus-bridge`, `torus-mempool`, `torus-consensus` (incl. `four_node_consensus`), `torus-state`.

**Phases A/C/D:** each gets its own detailed `-impl.md` when reached (see outlines below). They are intentionally *not* expanded to code-level tasks here — A's design (persistent trie vs rolling per-CF hash) needs its own brainstorm, and C/D depend on what B+A actually measure.

---

## Phase B — Tasks (TDD)

### Task B1 — Add `PlaceOrderBatch` variant + canonical bytes
- **Test first** (`crates/torus-types/src/lib.rs` tests or `eip712.rs` tests): construct `NativeAction::PlaceOrderBatch(vec![p1, p2])`; assert `canonical_bytes()` is deterministic, length-framed, and differs from two separate `PlaceOrder` actions (no ambiguity). Assert serde round-trip.
- **Implementation:**
  - `lib.rs:459` enum: add `PlaceOrderBatch(Vec<PlaceOrderParams>)`.
  - `lib.rs:562` `canonical_bytes`: new arm — unique 1-byte tag + `u32` count + each order's existing canonical encoding.
  - Update any exhaustive `match` on `NativeAction` the compiler flags (e.g. `requires_eip712`/`is_exempt_action` are non-exhaustive-safe, but check `lib.rs:210`, `native_executor.rs:229`, `eip712.rs:195`).
- **Verify:** `cargo test -p torus-types canonical`
- **Depends on:** —

### Task B2 — EIP-712 batch hash + single-signature recover round-trip
- **Test first** (`eip712.rs` tests, mirror `sign_and_recover_place_order` at `:980`): sign one `PlaceOrderBatch` of 3 orders with a key; assert `recover_sender()` returns the signer, and that tampering with any order in the batch changes the recovered address.
- **Implementation:**
  - `eip712.rs:195` `eip712_struct_hash`: add arm `NativeAction::PlaceOrderBatch(orders) => hash_place_order_batch(orders, nonce)`.
  - New `hash_place_order_batch(orders, nonce)`: EIP-712 array encoding — `keccak256(typehash || keccak256(concat per-order struct hashes) || encode_u64(nonce))`, reusing the per-order field hashing from `hash_place_order` (`:244`). Factor the per-order struct hash into a helper so single + batch share it.
- **Verify:** `cargo test -p torus-types -- eip712 batch`
- **Depends on:** B1

### Task B3 — Configurable batch caps + oversize rejection
- **Test first** (`rate_limit.rs` tests): assert a batch with `len > NATIVE_ORDERS_PER_BATCH_CAP` is rejected by a new `validate_batch_size(action)` helper; at/under cap passes. Assert per-block total-order accounting helper counts orders, not actions.
- **Implementation:**
  - `rate_limit.rs:37` area: add `pub const NATIVE_ORDERS_PER_BATCH_CAP: usize = 256;` and `pub const NATIVE_ORDERS_PER_BLOCK_CAP: usize = 25_600;` (bounds worst-case execution + block bytes; both tunable). Document the block-bytes implication (`256 orders × ~70B ≈ 18KB/batch`; keep `cap_actions × batch ≤ message_size`).
  - Add `pub fn order_count(action: &NativeAction) -> usize` (1 for non-batch; `v.len()` for batch) and `pub fn validate_batch_size(action) -> Result<(), …>`.
- **Verify:** `cargo test -p torus-mempool rate_limit`
- **Depends on:** B1

### Task B4 — Executor: flatten batch into per-market parallel matching
- **Test first** (`crates/torus-bridge` tests near `execute_batch`): build one `(sender, PlaceOrderBatch[N across 2 markets])`; run `execute_batch`; assert all N orders matched, global order IDs assigned in deterministic (batch, then market-partition) order, margin reserved per order, and one failing order does **not** abort the batch.
- **Implementation:**
  - `native_executor.rs:328` `execute_batch`: in the Phase-1 partition loop (`:341`), when the action is `PlaceOrderBatch`, expand its orders into `place_order_indices` / `PreparedOrder` with a stable `(top_index, sub_index)` so the existing Phase-2/3/4 per-market pipeline (`:356`–`:434`) is unchanged.
  - Result vector: emit **one** `NativeActionResult` per top-level action; for a batch, aggregate sub-results (e.g. `"batch: {ok}/{total} placed"`). Keep `results` indexed by top-level action (`:333`).
- **Verify:** `cargo test -p torus-bridge execute_batch`
- **Depends on:** B1

### Task B5 — Replay/nonce + rate-tracker correctness for batches
- **Test first** (`crates/torus-consensus/src/app.rs` tests — extend `duplicate_committed_native_action_executes_once`): a committed `PlaceOrderBatch` consumes exactly one `(sender, nonce)` in `CF_NATIVE_NONCES`; replaying the same committed block re-executes it **zero** times; rate tracker attributes the batch's order count to the sender.
- **Implementation:**
  - `app.rs` `execute_committed_block`: the per-action `(sender, nonce)` guard (`native_nonce_key`, from `2e52851`) already treats a batch as one unit — verify and add the batch to the intra-block seen-set the same way. No new key needed.
  - Rate-tracker feed (`rate_limit.rs:84` `record_block`): decide and document whether `native_senders` repeats per-order or per-batch; prefer per-order (push sender `order_count` times) so `NATIVE_RATE_LIMIT_PER_WINDOW` still bounds order flow.
- **Verify:** `cargo test -p torus-consensus duplicate_committed_native`
- **Depends on:** B4

### Task B6 — Devnet proof: batched flood, orders/s + sub-100ms
- **Test first** (bench harness assertion): add `--batch-size N` to `tools/bench-throughput/src/main.rs` (and/or `tools/tx-flood`) so it emits `PlaceOrderBatch` actions; pass bar: **orders/s ≫ 330**, dup-factor **≤ 1.05**, block time **< 100ms**, 0 stalls over a 30s run against devnet validator RPCs (`:8645/8546/8547/8548` — **never** `:8545` live testnet).
- **Implementation:**
  - Bench tool: build + sign batched actions; report orders/s (not just actions/s).
  - One-line: if a full block of batches would exceed 256KB at the chosen batch size, bump `config.rs:79` `max_consensus_message_size` enough for the devnet proof (full size-vs-dissemination validation is Phase C). Keep batch size modest for the first proof.
- **Verify:** `python3 devnet/scripts/native-order-flood.py --batch …` (or the rust bench) → metrics meet the bar; `cargo test --workspace` stays green.
- **Depends on:** B2, B3, B5

---

## Phase A — Incremental state root (outline; needs own `-impl.md`)
**Success:** block-time independent of total state size; root identical to full-scan recompute (determinism CI gate). 
**Approach options to brainstorm:** (1) persistent reth-trie with cursor factories (the `trie.rs:48` "intended" path) updating only changed accounts/slots; (2) native root → real incremental Merkle / rolling per-key hash so only changed CF entries rehash (the native root at `state_root.rs:51-91` is the easy first win — it's a flat keccak of all entries today). 
**Hard parts:** determinism across validators (gate with a CI check that incremental == full-scan on a corpus; prior mismatch `f3d3f858`); crash-consistency of the persisted trie; EIP-161 clearing (`state_root.rs:137`). 
**Files:** `torus-bridge/src/state_root.rs`, `torus-state/src/trie.rs`, `torus-state/src/db.rs`, callers in `app.rs` produce/validate/commit.

## Phase C — Caps + large-block dissemination (outline)
**Success:** `NATIVE_TOTAL_BLOCK_CAP` + `max_consensus_message_size` raised to carry ~20k orders/block (1–2MB); full-block propagation across 3 validators stays sub-100ms, no stall. 
**Watch:** the old stall was compact-block *availability* (`66da812`), not raw size — but re-measure; consider block compression; confirm `validate_block` TorusBlock path (`app.rs:954`) handles larger blocks within the view budget (`4*EWNL + produce + validate < max_view_time`). 
**Files:** `rate_limit.rs`, `network/src/config.rs`, `network/src/swarm.rs`, `app.rs`.

## Phase D — Off-box benchmark harness (outline)
**Success:** reproducible end-to-end numbers on a quiet, larger box with the client off-box. Pass bar carried from state: sustainable load, <1% rejects, p99 < target, no stalls. 
**Why:** current host load 31–38 on 8 cores confounds every measurement and drags idle block rate. 
**Files:** `tools/bench-throughput`, `devnet/` compose + scripts.

---

## Verification (end-to-end, Phase B)
1. `cargo test --workspace` green (esp. `four_node_consensus`).
2. Fresh devnet (`devnet/start.sh up --build`) — rebuild `torus-node` first (Dockerfile ships host `target/release` binary).
3. Batched flood against `:8645/8546/8547/8548` → orders/s, dup-factor, block-time, stall checks meet the bar.
4. `torus_getBlockBody` spot-check: a batch action appears once, executes once (u64 height, camelCase — memory `ec399809`).

## Rollback
Phase B is additive (new enum variant + new caps + bench flag). Revert = drop the `PlaceOrderBatch` arm and caps; existing single-`PlaceOrder` path is untouched. The message-size bump (B6) is a one-line config revert. No migration: cold-start rate tracker, and `CF_NATIVE_NONCES` keys are unchanged (batch = one nonce).
