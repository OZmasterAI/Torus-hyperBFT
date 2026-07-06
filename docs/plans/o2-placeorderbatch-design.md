# Design: O2 PlaceOrderBatch — close-out (hardening + proof), S416

## Problem

Roadmap O2 ("PlaceOrderBatch — one signed action carrying Vec of orders … NEEDS
WRITING-PLANS") is **stale**: the feature shipped 2026-06-06 as Phase B B1–B5
(commit b58f858) and is live on `sprint/blockspeed-orders-s395`. The S415 O5
sweep already ran it at `--batch-size 400` (11.4k orders/s stable). What O2
actually needs is to close the verified gaps and prove the batch win with the
standard methodology, then check the roadmap off honestly.

Premise correction saved to memory: `27a66377be573bc0`. Lesson: code-verify
roadmap items before planning greenfield work.

## Context (what exists — all verified on branch, 4-agent recon S416)

- Variant `PlaceOrderBatch(Vec<PlaceOrderParams>)` — `torus-types/src/lib.rs:466`,
  canonical tag 25 (`lib.rs:609`), shared per-order encoder with single
  `PlaceOrder` (`lib.rs:566`).
- EIP-712: `PlaceOrderItem(...)` item hash (`eip712.rs:283`) folded into
  `PlaceOrderBatch(bytes32 ordersHash,uint64 count,uint64 nonce)`
  (`eip712.rs:301`). **One ecrecover and one `(sender, nonce)` per batch**
  (replay test `app.rs:2632`).
- Executor flattens batches into per-order entries preserving batch order
  (`native_executor.rs:514`); serial margin phase (~13µs/order, O1 cache),
  per-market parallel matching, serial settle. B4 test: batch ≡ same orders
  individually, byte-identical state (`parallel_matching_tests.rs:433`).
- Caps: `NATIVE_ORDERS_PER_BATCH_CAP=1024` (`rate_limit.rs:117`); batch counts
  as **1 action** toward `NATIVE_TOTAL_BLOCK_CAP=100`; bytes gated by
  `NATIVE_BLOCK_BYTES_CAP=6MB`; ~51–59 B/order marginal (500-order batch
  ≈ 26–30 KB; ~19 batches ≈ 9.5k orders fit under the 512KB full-push rung).
- Tooling: `tools/bench-throughput --batch-size N` signs real batches
  (`main.rs:301`); registers `SessionScope::Full` sessions (`main.rs:603`).

## Verified gaps (the real O2 work)

G1. **Batch-size cap is RPC-only.** `validate_batch_size` callers: `torus.rs:247,967`
    — nothing at gossip admit, DA ingest, block validation, or exec. A peer
    (direct push, 4–8MB rungs) or malicious proposer can deliver a ~100k-order
    batch; exec flattens and executes all of it → view-budget blowout
    (liveness DoS, not a fork). The `rate_limit.rs:111` doc comment claiming
    "RPC ingress + block validation" is aspirational.
G2. **Order-aware block budget unwired (Phase C debt).** `order_count()`
    (`rate_limit.rs:173`) has no production callers; selection counts actions,
    so an honest proposer can legally build 100×1024 = 102,400-order blocks vs
    the documented 50k ceiling (`NATIVE_ORDERS_PER_BLOCK_CAP`).
G3. **Least-privilege market makers can't batch.** `SessionScope::Trading`
    excludes `PlaceOrderBatch` (`lib.rs:208–214`); only over-broad `Full` scope
    or a hot master EIP-712 key can sign batches. Scope is enforced on the
    consensus path too (`eip712.rs:985`) → extending it is a lockstep change:
    old validators treat a Trading-scoped batch as a violation → **skip + slash
    the proposer**. Must be fleet-wide before clients use it.
G4. **Partial-failure semantics shipped but never decided/tested.** Per-order
    isolation is the de-facto contract (`native_executor.rs:382,509`); a mixed
    pass/fail batch has zero test coverage; the single-action wrapper reports
    `"k/N orders placed"` (`native_executor.rs:400`). Also: book-level rejects
    still yield `ok` results and `PlaceResult` carries no reject reason —
    results are then dropped wholesale (`app.rs:414`, no receipts exist).
G5. **Cross-language signing contract gap.** `eip712_vectors.rs all_vectors()`
    has vectors for ~30 variants but none for `PlaceOrderBatch`; the TS client
    (`torus-trading-app/tests/eip712.spec.ts`) is unbound on the one action
    market makers will actually sign. Python flood script also has no batch
    typehash (bench-throughput covers devnet needs).
G6. **No batch-vs-single evidence.** No criterion bench isolates the amortized
    verify/dedup/manifest win; no devnet sweep across batch sizes.

Adjacent pre-existing wart (flagged, separate fix): unknown `market_id`
silently creates a phantom tick=1/lot=1 book (`native_executor.rs:656,854`) —
a typo'd batch "succeeds" into a phantom market.

## Options

### Option A — Codify current semantics + harden caps + prove (recommended)
Keep partial-per-order as THE batch contract and write it down + test it.
Enforce the batch cap deterministically at exec (oversize batch → whole batch
skipped, warn; defensive checks at gossip/DA admit), wire `order_count()` into
proposer selection, extend `Trading` scope to include `PlaceOrderBatch`,
add the golden vectors, bench + devnet sweep, fix the two stale docs.
- Files: `rate_limit.rs`, `native_pool.rs` (selection), `mempool/lib.rs`
  (admit), `app.rs` or `native_executor.rs` (exec cap), `lib.rs` (scope),
  `eip712_vectors.rs`, new `benches/exec_place_batch.rs`, sweep script.
- Trade-offs: no new product surface; accepts fee-less batches bounded by caps
  only; receipt fidelity stays poor (G4 tail) — deferred.
- Effort: Small-Medium. Risk: Low (one lockstep-deploy item: scope).

### Option B — A + opt-in atomic batch (`PlaceOrderBatchAtomic`, tag 26)
All-or-nothing **pre-match reserve**: dry-run all margin reserves in a
speculative BalanceCache submap; any failure rejects the whole batch before ID
assignment/matching. Post-match atomicity is impossible (fills hit third
parties) — "atomic" can only ever mean the reserve stage, which limits its
honesty as a product promise.
- Adds: new action variant (append-only bincode tag + canonical tag 26 + new
  typehash), executor pre-pass, scope/table updates, vectors, tests.
- Trade-offs: consensus change + schema surface for a mode with no
  demonstrated MM demand; "atomic" that can still partially fail post-match
  invites misunderstanding.
- Effort: Medium. Risk: Medium.

### Option C — A + per-order receipt channel
Persist per-order results with real reject reasons (extend `PlaceResult`,
stop dropping `NativeActionResult`s, new CF + RPC/WS surface). Real product
value for MMs but an independent feature (storage growth, RPC design) that
should not gate the throughput close-out.
- Effort: Large. Risk: Medium.

## Recommendation

**Option A.** Partial-per-order matches the shipped consensus behavior, the
B4 equivalence proof, and the market-maker use case (independent quotes; one
thin-margin order must not kill 499 good ones). Atomicity can't be honestly
promised past the reserve stage anyway. B and C become roadmap follow-ups
(C first — receipts have real MM value).

Batch classification stays as-is (whole batch sorts as one GtcOrder unit,
`native_executor.rs:1941`): document that inner IOC/market orders execute in
the GTC phase; MM batches are GTC quotes. Splitting by category is deferred.

## Work items (input to writing-plans)

- T1 tests-first: mixed pass/fail batch exec test (margin-fail among valid;
  per-order isolation; failed orders consume no order-id); mempool batch
  accounting test (admitted once, `order_count` orders); oversize-batch
  reject tests at each new enforcement point; Trading-scope batch test.
- T2: G1 enforcement — exec-path deterministic cap + gossip/DA admit checks.
- T3: G2 — order-aware selection budget (`order_count()` into
  `select_native_for_block…`, stop at `NATIVE_ORDERS_PER_BLOCK_CAP`).
- T4: G3 — `SessionScope::Trading` += `PlaceOrderBatch` (+ CancelOrderBatch
  does not exist; no change). Lockstep-deploy note in val3 doc.
- T5: G5 — PlaceOrderBatch golden vectors (multi + batch-of-one) + regenerate
  fixture; TS parity note.
- T6: G6 — criterion `exec_place_batch` bench (1 batch×N vs N singles).
- T7: devnet sweep BS={1,100,400,1024} via S415 methodology (slope-fit
  block_ms, `Sustained:`, pull deltas, wedge check); laptop builds binaries.
- T8: docs — roadmap O2 entry rewritten to reality; handoff state updated.

## Open Questions

1. Native actions pay zero fees (`total_native_fees` never incremented) —
   fine to ship O2 with cap-only control? (Fee design is its own roadmap item.)
2. Phantom-market creation on unknown market_id: fix now at RPC ingress
   (non-consensus, cheap) or defer whole fix (incl. exec-side, consensus) to
   its own item? Recommend: RPC-ingress guard now, exec-side later.
3. Receipt channel (Option C) priority vs O6 incremental state root.
