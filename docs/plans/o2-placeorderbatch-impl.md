# Implementation Plan: O2 PlaceOrderBatch close-out (hardening + proof)

## Design Decision

Option A from [o2-placeorderbatch-design.md](o2-placeorderbatch-design.md): codify partial-per-order
semantics, harden the caps deterministically, extend Trading scope, prove with bench + sweep.

## Success Criteria

- All new tests pass on the laptop; `cargo test --workspace` green; `cargo clippy --workspace
  --all-targets -- -D warnings` green (CI runs `RUSTFLAGS=-D warnings`).
- Criterion bench `exec_place_batch` shows 1 batch of 400 ≥ throughput of 400 singles, with the
  amortized verify win quantified (expected dominant term: 400 ecrecovers vs 1).
- Devnet sweep BS={1,100,400,1024} completes with 0 wedges, producing an orders/s +
  block_ms_fit table (script ready this session; sweep EXECUTION is a follow-up session step).
- Roadmap O2 entry rewritten to reality (stale-entry correction) + handoff state updated.

## Cross-cutting facts (verified this session — govern several tasks)

- **Crate DAG**: `torus-bridge` does NOT depend on `torus-mempool` (both depend on
  `torus-types`). The batch cap constant currently lives in
  `crates/torus-mempool/src/rate_limit.rs:117` — exec-side enforcement in torus-bridge
  therefore requires **moving the constant to `torus-types`** with a re-export shim
  (Task 2). External importer to keep working:
  `crates/torus-integration-tests/tests/o5_feed_gates.rs:11-12`
  (`use torus_mempool::rate_limit::{... NATIVE_ORDERS_PER_BATCH_CAP}`).
- **Exec choke point**: every production native execution path funnels through
  `NativeExecutor::execute_batch` (`crates/torus-bridge/src/native_executor.rs:510`):
  the live commit path calls it at `crates/torus-consensus/src/app.rs:414-415`, and the
  crash-replay single-action wrapper arm (`native_executor.rs:400-411`) delegates into it via
  `from_ref`. `torus-bridge/src/validator.rs` (`validate_block*`) is EVM-only. So the
  **flatten loop (`native_executor.rs:514-537`) is the single deterministic enforcement
  point** — chosen over an app.rs pre-exec check, which would cover only the live path and
  silently miss any other `execute_batch` caller.
- **Admit choke point**: every network pool-admission path funnels through
  `Mempool::admit_gossip` (`crates/torus-mempool/src/lib.rs:427`): gossip topic + pre-proposal
  full-body push (`swarm.rs:961-976`) + direct forward (`swarm.rs:1002-1013`) all land on the
  `native_action_inbound` channel → `add_native_action_from_gossip` (main.rs:473) → admit_gossip;
  `add_native_action_from_gossip_trusted` (lib.rs:414) also routes through it. DA pull-absorbs
  never enter the pool (they only feed block reconstruction), so the exec-side skip covers them.
  The admit check MUST run **after** `mirror_to_da` — availability ≠ validity (lib.rs:433-436,
  mem 28e1a821): a block-referenced oversize body must stay reconstructable so the exec-side
  skip can run deterministically.
- **Stale design-doc claim**: `parallel_matching_tests.rs:497`
  (`place_order_batch_failure_isolated`) ALREADY covers mixed pass/fail isolation — G4's "zero
  test coverage" is overstated. What is actually missing (Task 1): failed-order-consumes-no-id
  and state-equivalence-vs-singles-minus-failed.
- `PlaceOrderBatch` is NOT in `requires_eip712` (`eip712.rs:741-752`) — session keys can sign
  batches (S415 sweep ran `--sign-mode session --batch-size 400`), so the Trading-scope
  extension (Task 5) is meaningful.
- `MAX_ORDERS_PER_TRADER_PER_MARKET = 200` (`torus-core/src/order_book.rs:23`) — resting-order
  tests/benches must spread big batches across markets or expect book-level rejects.
- `CF_NATIVE_MARKETS` is seeded at genesis (`torus-genesis/src/lib.rs:407-429`, key =
  `market_id.to_be_bytes()`, 8 bytes) and written by governance `ListMarket`
  (`torus-economics/src/governance.rs:1011`). The global-order-id counter row in the same CF
  uses a 24-byte key (`native_executor.rs:318`), so an 8-byte point-get can never collide.

## Consensus-behavior changes — deploy constraints (read before shipping)

Two tasks change what validators accept/execute and need fleet-lockstep care:

- **Task 2 (exec-side skip)**: old binaries EXECUTE a >1024-order batch; new binaries SKIP it.
  Divergence requires such a batch in a committed block, which RPC + admit checks prevent for
  honest flows — only a malicious proposer (or crafted direct push) can create the exposure.
  Deploy fleet-wide promptly; risk during a mixed window is attacker-triggered only.
- **Task 5 (Trading scope += PlaceOrderBatch)**: scope is enforced on the consensus verify path
  (`eip712.rs:985` guard → sender `None` → app.rs:331-349 SLASHES + tombstones the proposer).
  Old validators treat a Trading-scoped batch as an invalid signature. **The whole fleet must
  run Task 5 before any client signs Trading-scoped batches.** (`tools/bench-throughput`
  registers `SessionScope::Full` sessions — main.rs:603 — so existing tooling is unaffected.)

---

## Tasks

### Task 1 — Pin the partial-per-order contract (mixed pass/fail batch)

**Spec-pin task**: this test SHOULD PASS against current code. Writing it and watching it pass
IS the deliverable — it turns the shipped de-facto behavior (design doc G4) into the written
contract. Tasks 2–5 tests must FAIL first; this one must not.

**Test first** — append to `crates/torus-bridge/tests/parallel_matching_tests.rs` (after
`place_order_batch_failure_isolated`, which ends at :527; uses the file's own helpers
`open_test_db` :18, `addr` :24, `fp` :28, `make_ctx` :32, `fund_native` :46, `limit_buy` :54):

```rust
// ============================================================================
// Test 11: O2 contract pin — mixed pass/fail batch: per-order isolation,
// failed order consumes NO order-id, state == equivalent singles minus failed
// ============================================================================

#[test]
fn mixed_batch_pass_fail_pins_partial_per_order_contract() {
    // Run A: one batch [ok, margin-fail, ok]. Margin per order = price*qty/20
    // (default max leverage, native_executor.rs:591-599): 100*10/20 = 50.
    // Fund 120: order0 reserves 50 (70 left), order1 needs 100*40/20 = 200
    // -> FAILS, order2 reserves 50 (20 left) — isolation from the failure.
    let (_dir, db) = open_test_db();
    let mut ctx_batch = make_ctx(db.clone());
    let mm = addr(1);
    fund_native(&ctx_batch, &mm, fp(120));

    let batch = NativeAction::PlaceOrderBatch(vec![
        limit_buy(1, 100, 10), // 50 — ok
        limit_buy(2, 100, 40), // 200 — insufficient margin — FAILS
        limit_buy(3, 100, 10), // 50 — ok (must be unaffected by #1's failure)
    ]);
    let id0 = ctx_batch.next_global_order_id;
    let res = NativeExecutor::execute_batch(&mut ctx_batch, &[(mm, batch)]);

    assert_eq!(res.results.len(), 3, "flattened: one result per order");
    assert!(res.results[0].success, "order 0: {:?}", res.results[0].error);
    assert!(!res.results[1].success, "order 1 must fail on margin");
    assert!(res.results[1]
        .error
        .as_ref()
        .unwrap()
        .contains("insufficient margin"));
    assert!(
        res.results[2].success,
        "order 2 must be isolated from order 1's failure: {:?}",
        res.results[2].error
    );
    // THE contract detail G4 left untested: the margin-fail `continue`
    // (native_executor.rs:615) precedes ID assignment (:629), so a failed
    // order consumes NO global order id.
    assert_eq!(
        ctx_batch.next_global_order_id,
        id0 + 2,
        "failed order must not consume an order id"
    );

    // Run B: the two GOOD orders as singles on a fresh ctx — end state must match.
    let (_dir2, db2) = open_test_db();
    let mut ctx_singles = make_ctx(db2.clone());
    fund_native(&ctx_singles, &mm, fp(120));
    let singles: Vec<(Address, NativeAction)> = vec![
        (mm, NativeAction::PlaceOrder(limit_buy(1, 100, 10))),
        (mm, NativeAction::PlaceOrder(limit_buy(3, 100, 10))),
    ];
    let res_singles = NativeExecutor::execute_batch(&mut ctx_singles, &singles);
    assert!(res_singles.results.iter().all(|r| r.success));

    let b = ctx_batch.positions.get_native_balance(&mm).unwrap();
    let s = ctx_singles.positions.get_native_balance(&mm).unwrap();
    assert_eq!(b.available, s.available, "available: batch == singles minus failed");
    assert_eq!(b.order_margin, s.order_margin, "reserved margin: batch == singles minus failed");
    assert_eq!(
        ctx_batch.next_global_order_id, ctx_singles.next_global_order_id,
        "order-id consumption: batch == singles minus failed"
    );
    assert_eq!(ctx_batch.trade_index, ctx_singles.trade_index);
}
```

**Implementation**: none (test-only; codifies shipped semantics).

**Verify**:
```
lap test -p torus-bridge --test parallel_matching_tests mixed_batch_pass_fail_pins_partial_per_order_contract
```
Expected: PASSES immediately (that is the point — spec pin, not red/green).

**Depends on**: nothing.

---

### Task 2 — Exec-path deterministic oversize-batch skip (G1, consensus-critical)

**Test first** — append to `crates/torus-bridge/tests/parallel_matching_tests.rs`:

```rust
// ============================================================================
// Test 12: G1 — oversize batch is skipped WHOLESALE at exec (deterministic);
// the block continues and a valid sibling action still executes
// ============================================================================

#[test]
fn oversize_batch_skipped_deterministically_sibling_executes() {
    use torus_types::NATIVE_ORDERS_PER_BATCH_CAP;
    let (_dir, db) = open_test_db();
    let mut ctx = make_ctx(db.clone());
    let attacker = addr(1);
    let honest = addr(2);
    fund_native(&ctx, &attacker, fp(100_000_000));
    fund_native(&ctx, &honest, fp(100_000));

    // CAP+1 batch crafted DIRECTLY — models a malicious proposer / direct-push
    // body that never saw the RPC or admit checks.
    let oversize =
        NativeAction::PlaceOrderBatch(vec![limit_buy(1, 100, 1); NATIVE_ORDERS_PER_BATCH_CAP + 1]);
    let sibling = NativeAction::PlaceOrder(limit_buy(2, 50, 1));

    let id0 = ctx.next_global_order_id;
    let res = NativeExecutor::execute_batch(&mut ctx, &[(attacker, oversize), (honest, sibling)]);

    // Whole batch skipped at flatten: only the sibling's result exists, only
    // one order id consumed, zero attacker margin reserved.
    assert_eq!(res.results.len(), 1, "oversize batch must not flatten into results");
    assert!(res.results[0].success, "sibling: {:?}", res.results[0].error);
    assert_eq!(ctx.next_global_order_id, id0 + 1, "no ids for the skipped batch");
    assert_eq!(
        ctx.positions.get_native_balance(&attacker).unwrap().order_margin,
        FixedPoint::ZERO,
        "skipped batch must reserve nothing"
    );

    // Boundary: an AT-CAP batch still executes (flattens to CAP results;
    // book-level per-trader caps may reject some — length is the invariant).
    let at_cap =
        NativeAction::PlaceOrderBatch(vec![limit_buy(1, 100, 1); NATIVE_ORDERS_PER_BATCH_CAP]);
    let res2 = NativeExecutor::execute_batch(&mut ctx, &[(attacker, at_cap)]);
    assert_eq!(res2.results.len(), NATIVE_ORDERS_PER_BATCH_CAP);

    // Empty batch: skipped by the same rule (mirrors validate_batch_size).
    let res3 = NativeExecutor::execute_batch(&mut ctx, &[(attacker, NativeAction::PlaceOrderBatch(vec![]))]);
    assert_eq!(res3.results.len(), 0);
}
```

This FAILS on current code at the first assert (`res.results.len()` is `CAP + 2`).

**Implementation** (three files, one commit — compile-coupled):

1. `crates/torus-types/src/lib.rs` — add the canonical constant right above the
   `NativeAction` enum (after the section banner at :452-454):

```rust
/// Max orders a single `PlaceOrderBatch` may carry — the consensus-facing
/// safety ceiling. SINGLE SOURCE OF TRUTH for every enforcement layer:
/// RPC ingress + gossip/DA admission (torus-mempool `rate_limit` re-exports
/// this) and the deterministic exec-side skip (torus-bridge
/// `NativeExecutor::execute_batch` flatten). Lives here because torus-bridge
/// cannot depend on torus-mempool (both depend on torus-types).
pub const NATIVE_ORDERS_PER_BATCH_CAP: usize = 1024;
```

2. `crates/torus-mempool/src/rate_limit.rs:110-117` — replace the `pub const` with a
   re-export and fix the aspirational doc comment (G1: "block validation" never existed):

```rust
/// Max orders a single `PlaceOrderBatch` may carry (Phase B throughput keystone).
///
/// Canonical definition lives in `torus-types` (single source shared with the
/// exec-side deterministic skip in torus-bridge). Enforced at RPC ingress
/// (`validate_batch_size`, torus-rpc torus.rs), at gossip/DA admission
/// (`Mempool::admit_gossip`), and — the consensus-critical layer — at the
/// `execute_batch` flatten, which skips an oversize batch wholesale on every
/// node identically. Clients (market makers) tune their actual batch size up
/// to this bound. Bytes per batch ≈ size × ~70B.
pub use torus_types::NATIVE_ORDERS_PER_BATCH_CAP;
```

   (`o5_feed_gates.rs:11-12` and the rate_limit.rs tests keep compiling unchanged.)

3. `crates/torus-bridge/src/native_executor.rs` — two edits:

   a. Flatten loop, replace the batch arm at :524-529:

```rust
                match action {
                    NativeAction::PlaceOrderBatch(orders) => {
                        // G1 (O2): DETERMINISTIC exec-side cap. The RPC/admit
                        // checks are node-local policy; this is the consensus-
                        // critical bound. An oversize (or empty) batch is
                        // skipped WHOLESALE — same doctrine as the replay-guard
                        // skip (app.rs warn + continue) — the block is never
                        // aborted, and every correct node skips identically.
                        if orders.is_empty()
                            || orders.len() > torus_types::NATIVE_ORDERS_PER_BATCH_CAP
                        {
                            tracing::warn!(
                                batch_len = orders.len(),
                                cap = torus_types::NATIVE_ORDERS_PER_BATCH_CAP,
                                %sender,
                                "skipping oversize/empty PlaceOrderBatch at exec (deterministic cap)"
                            );
                            continue;
                        }
                        for p in orders {
                            out.push((*sender, NativeAction::PlaceOrder(p.clone())));
                        }
                    }
                    other => out.push((*sender, other.clone())),
                }
```

   b. Single-action wrapper arm (:400-411) — without this, a skipped batch on the
      crash-replay path would report `0/0 → success:true`. Guard before delegating:

```rust
            NativeAction::PlaceOrderBatch(orders) => {
                if orders.is_empty() || orders.len() > torus_types::NATIVE_ORDERS_PER_BATCH_CAP {
                    return NativeActionResult::err(
                        "place_order_batch",
                        format!(
                            "batch size {} outside [1, {}] — skipped (deterministic cap)",
                            orders.len(),
                            torus_types::NATIVE_ORDERS_PER_BATCH_CAP
                        ),
                    );
                }
                let pair = (*sender, action.clone());
                let batch = Self::execute_batch(ctx, std::slice::from_ref(&pair));
                let total = batch.results.len();
                let ok = batch.results.iter().filter(|r| r.success).count();
                NativeActionResult {
                    action_type: "place_order_batch",
                    success: ok == total,
                    error: (ok != total).then(|| format!("{ok}/{total} orders placed")),
                    gas_used: batch.total_gas,
                }
            }
```

Note the nonce side-effect (deliberate, deterministic): app.rs consumes the `(sender, nonce)`
BEFORE exec (:384, :431-437), so a skipped batch still burns its nonce on every node —
replaying the same oversize batch is dead on arrival.

**Verify**:
```
lap test -p torus-bridge --test parallel_matching_tests oversize_batch_skipped_deterministically_sibling_executes
lap test -p torus-mempool
lap test -p torus-integration-tests --test o5_feed_gates
```
(second/third commands prove the constant move + re-export broke nothing).

**Depends on**: Task 1 (contract pinned before touching batch exec semantics).

---

### Task 3 — Gossip/DA-admit defensive batch checks (G1, node-local layer)

**Test first** — append to the tests module in `crates/torus-mempool/src/lib.rs` (module starts
:747; model = `gossip_ingest_verifies_claimed_sender` :807, uses `setup()` :800 and `now_ms()`):

```rust
    #[test]
    fn gossip_admit_rejects_oversize_and_empty_batch_but_keeps_da_mirror() {
        use crate::rate_limit::NATIVE_ORDERS_PER_BATCH_CAP;
        let (_dir, state) = setup();
        let pool = Mempool::new(state, MempoolConfig::default());
        let key = k256::ecdsa::SigningKey::from_slice(
            &alloy_primitives::hex::decode(
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            )
            .unwrap(),
        )
        .unwrap();
        let now = now_ms();
        let params = torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(6_500_000_000_000),
            quantity: torus_types::FixedPoint::from_raw(10_000_000),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };

        // Oversize batch: rejected by BOTH gossip paths, pool stays empty,
        // but the body IS DA-mirrored (availability != validity — a block
        // referencing it must stay reconstructable for the exec-side skip).
        let over = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![
                params.clone();
                NATIVE_ORDERS_PER_BATCH_CAP + 1
            ]),
            now,
            &key,
        );
        let sender = over.recover_sender().unwrap();
        let over_hash = torus_types::compute_action_hash(&over);
        assert!(pool.add_native_action_from_gossip(sender, over.clone()).is_err());
        assert!(pool
            .add_native_action_from_gossip_trusted(sender, over)
            .is_err());
        assert_eq!(pool.native_pool_size(), 0, "oversize batch must never enter the pool");
        assert!(pool.get_native_da(&over_hash).is_some(), "body still DA-mirrored");

        // Empty batch: same rejection.
        let empty = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![]),
            now + 1,
            &key,
        );
        assert!(pool.add_native_action_from_gossip(sender, empty).is_err());
        assert_eq!(pool.native_pool_size(), 0);

        // At-cap batch: admitted (boundary).
        let at_cap = torus_types::eip712::sign_native_action(
            torus_types::NativeAction::PlaceOrderBatch(vec![
                params;
                NATIVE_ORDERS_PER_BATCH_CAP
            ]),
            now + 2,
            &key,
        );
        pool.add_native_action_from_gossip(sender, at_cap).unwrap();
        assert_eq!(pool.native_pool_size(), 1);
    }
```

FAILS on current code: both oversize admits return `Ok` and pool size is 2.

**Implementation** — `crates/torus-mempool/src/lib.rs`, in `admit_gossip` (:427-451), insert
after the `self.mirror_to_da(&action);` at :437 (order matters — see cross-cutting facts):

```rust
        // G1 defensive layer (O2): an oversize/empty batch must never enter
        // the POOL (an honest node must never SELECT it into a proposal). It
        // is still DA-mirrored above: a malicious proposer may reference it,
        // and the block must stay reconstructable so the deterministic
        // exec-side skip can run (availability != validity, mem 28e1a821).
        // Covers every network ingest: gossip topic, pre-proposal full-body
        // push, direct forward, and the gossip-trusted path.
        crate::rate_limit::validate_batch_size(&action.action)
            .map_err(MempoolError::NativeValidationFailed)?;
```

No other admit path needs a check: RPC ingress already calls `validate_batch_size`
(torus.rs:247, :967) before `add_native_action_presigned`, and DA pull-absorbs never insert
into the pool.

**Verify**:
```
lap test -p torus-mempool gossip_admit_rejects_oversize_and_empty_batch_but_keeps_da_mirror
```

**Depends on**: Task 2 (uses the re-exported constant; keeps rate_limit.rs edits serialized).

---

### Task 4 — Order-aware selection budget (G2: wire `order_count()` + `NATIVE_ORDERS_PER_BLOCK_CAP`)

**Test first** — append to the tests module in `crates/torus-mempool/src/native_pool.rs`
(module starts :400; uses `make_action` :412; note the extra `usize::MAX` argument in the
existing-call updates below — this test is written against the NEW 4-arg signature, so the
file will not even compile until the implementation lands; that is the red state):

```rust
    #[test]
    fn order_budget_bounds_selection_and_preserves_cancels_first() {
        let mut pool = NativePool::new(100, 64, 16);
        let p = torus_types::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(100),
            quantity: torus_types::FixedPoint::from_raw(100),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        // 5 batches of 10 orders + 2 cancels, distinct senders.
        for i in 0..5u8 {
            pool.insert(
                Address::repeat_byte(i + 1),
                make_action(
                    1_000 + i as u64,
                    NativeAction::PlaceOrderBatch(vec![p.clone(); 10]),
                ),
            )
            .unwrap();
        }
        pool.insert(
            Address::repeat_byte(10),
            make_action(2_000, NativeAction::CancelOrder { order_id: 1 }),
        )
        .unwrap();
        pool.insert(
            Address::repeat_byte(11),
            make_action(2_001, NativeAction::CancelOrder { order_id: 2 }),
        )
        .unwrap();

        // Order budget 25: cancels first (1+1), then TWO batches (10+10 -> 22);
        // a third batch would reach 32 > 25 -> deterministic-prefix break.
        let sel = pool.select_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            25,
        );
        assert_eq!(sel.len(), 4, "2 cancels + 2 batches fit the 25-order budget");
        assert!(matches!(sel[0].1.action, NativeAction::CancelOrder { .. }));
        assert!(matches!(sel[1].1.action, NativeAction::CancelOrder { .. }));
        assert!(matches!(sel[2].1.action, NativeAction::PlaceOrderBatch(_)));
        assert!(matches!(sel[3].1.action, NativeAction::PlaceOrderBatch(_)));

        // usize::MAX order budget preserves today's behavior exactly.
        let all = pool.select_for_block_with_senders_excluding(
            100,
            &HashSet::new(),
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(all.len(), 7);
    }
```

**Implementation** (four files):

1. `crates/torus-mempool/src/rate_limit.rs` — env-override accessor next to the constant
   (:119-126); also update that constant's NOTE paragraph (:123-125) to: *"Enforced in
   `select_for_block_with_senders_excluding` via `order_count` (O2/G2, S416); selection-only —
   `validate_block` does not reject on order count, so mixed values cannot fork."*

```rust
/// Effective per-block ORDER budget: `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP`
/// overrides the compiled default PER NODE (same OnceLock pattern as
/// `TORUS_NATIVE_TOTAL_BLOCK_CAP`). Proposer-local selection policy —
/// validate_block does not reject on order count — mixed values cannot fork.
pub fn native_orders_per_block_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("TORUS_NATIVE_ORDERS_PER_BLOCK_CAP")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(NATIVE_ORDERS_PER_BLOCK_CAP)
    })
}
```

2. `crates/torus-mempool/src/native_pool.rs`:
   - `select_for_block_with_senders_excluding` (:270): add 4th param `orders_cap: usize`, and
     extend the loop body (:293-311). Doc comment gains: *"`orders_cap` bounds the summed
     `order_count` of selected actions (G2: without it, 100 actions × 1024-order batches =
     102,400 orders vs the documented 50k ceiling). Same deterministic-prefix rule as
     `bytes_cap`."*

```rust
        let mut block_counts: HashMap<Address, usize> = HashMap::new();
        let mut selected = Vec::new();
        let mut bytes_used: usize = 0;
        let mut orders_used: usize = 0;

        for entry in &self.entries {
            if selected.len() >= limit {
                break;
            }
            if bytes_used.saturating_add(entry.encoded_len) > bytes_cap {
                // Deterministic prefix: stop at the first entry that would blow
                // the body-byte budget rather than skipping past it.
                break;
            }
            let entry_orders = crate::rate_limit::order_count(&entry.action.action);
            if orders_used.saturating_add(entry_orders) > orders_cap {
                // Deterministic prefix for the ORDER budget too (O2/G2) —
                // bounds worst-case matching/exec time per block. Cancels sort
                // first and count 1 each, so cancels-first is preserved.
                break;
            }
            if exclude.contains(&entry.action_hash) {
                continue;
            }
            let count = block_counts.get(&entry.sender).copied().unwrap_or(0);
            if count < self.max_per_block {
                *block_counts.entry(entry.sender).or_insert(0) += 1;
                bytes_used = bytes_used.saturating_add(entry.encoded_len);
                orders_used = orders_used.saturating_add(entry_orders);
                selected.push((entry.sender, entry.action.clone()));
            }
        }
```

   - Delegator `select_for_block_with_senders` (:251-253) passes `usize::MAX` as `orders_cap`.
   - Existing tests updated with the extra `usize::MAX` arg: :459, :463, :717, :724.

3. `crates/torus-mempool/src/lib.rs` — wrapper `select_native_for_block_with_senders_excluding`
   (:612-624): add `orders_cap: usize` param, pass through at :623. Doc comment gains one line:
   *"`orders_cap` bounds total orders via `order_count` (`NATIVE_ORDERS_PER_BLOCK_CAP`)."*

4. `crates/torus-consensus/src/app.rs:1385-1389` — the only production caller:

```rust
            let native = mempool.select_native_for_block_with_senders_excluding(
                torus_mempool::rate_limit::native_total_block_cap(),
                &in_flight,
                torus_mempool::rate_limit::native_block_bytes_cap(),
                torus_mempool::rate_limit::native_orders_per_block_cap(),
            );
```

This gives `order_count()` (rate_limit.rs:173) its first production caller — delete nothing.

**Verify**:
```
lap test -p torus-mempool order_budget_bounds_selection_and_preserves_cancels_first
lap test -p torus-mempool
lap test -p torus-consensus
```

**Depends on**: nothing (independent of Tasks 2-3; ordered here for rate_limit.rs edit locality).

---

### Task 5 — `SessionScope::Trading` += `PlaceOrderBatch` (G3, LOCKSTEP deploy)

**Test first** — add to the tests module in `crates/torus-types/src/eip712.rs` (next to
`make_session` :1390, which already defaults to `SessionScope::Trading`; helper
`sign_action_with_session` :1381):

```rust
    /// O2/G3: least-privilege market makers must be able to batch under
    /// Trading scope; the narrow TransfersOnly scope must still reject.
    /// Covers BOTH enforcement points: `resolve_sender` (ingress, :823) and
    /// `batch_verify_native_actions` (exec/consensus, :985) — one `allows()`.
    #[test]
    fn trading_scope_allows_place_order_batch_transfers_only_rejects() {
        let ed_key = ed25519_dalek::SigningKey::from_bytes(&[44u8; 32]);
        let pubkey = ed_key.verifying_key().to_bytes();
        let owner = Address::from([0x44; 20]);

        let params = crate::PlaceOrderParams {
            market_id: 1,
            is_buy: true,
            price: FixedPoint::from_raw(6_500_000_000_000),
            quantity: FixedPoint::from_raw(10_000_000),
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let signed = sign_action_with_session(
            NativeAction::PlaceOrderBatch(vec![params.clone(), params]),
            TEST_NONCE,
            &ed_key,
        );

        // Ingress path: Trading scope resolves to the owner.
        let trading = make_session(owner); // scope: Trading
        let got = signed
            .resolve_sender(0, |pk| (pk == &pubkey).then(|| trading.clone()))
            .expect("Trading scope must allow PlaceOrderBatch");
        assert_eq!(got, owner);

        // Exec/consensus path agrees.
        let senders = batch_verify_native_actions(
            std::slice::from_ref(&signed),
            TEST_NONCE,
            |pk| (pk == &pubkey).then(|| trading.clone()),
        );
        assert_eq!(senders[0], Some(owner));

        // TransfersOnly still rejects with the precise scope error.
        let transfers = SessionData {
            owner,
            expiry: u64::MAX,
            scope: SessionScope::TransfersOnly,
            created_at: 0,
        };
        let err = signed
            .resolve_sender(0, |pk| (pk == &pubkey).then(|| transfers.clone()))
            .unwrap_err();
        assert!(matches!(err, Eip712Error::SessionScopeViolation));
    }
```

FAILS on current code: `resolve_sender` returns `SessionScopeViolation` for the Trading leg.

**Implementation** — `crates/torus-types/src/lib.rs`:

1. Enum doc (:196): `/// PlaceOrder, PlaceOrderBatch, CancelOrder, ModifyOrder, CancelAllOrders only.`
2. The `allows` Trading arm (:208-214), with the lockstep warning as a doc comment ON the arm:

```rust
            // LOCKSTEP-DEPLOY WARNING (O2/G3): scope is enforced on the
            // CONSENSUS path too (eip712.rs batch verify guard) — on an old
            // validator a Trading-scoped PlaceOrderBatch resolves to None,
            // which the exec pipeline treats as an invalid signature and
            // SLASHES + TOMBSTONES the block's PROPOSER (app.rs invalid-
            // attestation arm). The ENTIRE fleet must run this change before
            // any client signs batches under a Trading-scoped session.
            SessionScope::Trading => matches!(
                action,
                NativeAction::PlaceOrder(_)
                    | NativeAction::PlaceOrderBatch(_)
                    | NativeAction::CancelOrder { .. }
                    | NativeAction::ModifyOrder { .. }
                    | NativeAction::CancelAllOrders { .. }
            ),
```

(No `CancelOrderBatch` variant exists — no further scope change.)

**Verify**:
```
lap test -p torus-types trading_scope_allows_place_order_batch_transfers_only_rejects
lap test -p torus-types
```

**Depends on**: nothing (code-wise). Deploy-wise: see lockstep section.

---

### Task 6 — RPC ingress unknown-market guard (design Open Question 2 → resolved: add now)

RPC-only, non-consensus. Closes the phantom-book trap: an unknown `market_id` currently
"succeeds" into a conjured tick=1/lot=1 book at exec (`native_executor.rs:656`,
`unwrap_or_else(|| OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE))`). The
exec-side fix is consensus-affecting and stays a separate roadmap item.

**Test first** — append to the tests module in `crates/torus-rpc/src/lib.rs` (module :497;
model = `submit_native_actions_batch_per_item_results` :642, uses `setup()` :567 /
`start_server` :600):

```rust
    #[tokio::test]
    async fn submit_rejects_unknown_market_single_and_batch() {
        let (_dir, state, mempool, executor) = setup();
        // Seed market 1 (existence is what the guard reads; genesis writes
        // borsh StoredMarket bytes under the same 8-byte BE key).
        state
            .put_cf_raw(
                torus_state::cf::CF_NATIVE_MARKETS,
                &1u64.to_be_bytes(),
                b"seeded-market",
            )
            .unwrap();
        let (handle, addr) = start_server(state, mempool.clone(), executor).await;
        use jsonrpsee::core::client::ClientT;
        let client = jsonrpsee::http_client::HttpClientBuilder::default()
            .build(format!("http://{addr}"))
            .unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let key = k256::ecdsa::SigningKey::from_slice(
            &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap(),
        )
        .unwrap();
        let params = |market_id: u64| torus_types::PlaceOrderParams {
            market_id,
            is_buy: true,
            price: torus_types::FixedPoint::from_raw(6_500_000_000_000),
            quantity: torus_types::FixedPoint::from_raw(10_000_000),
            order_type: torus_types::OrderType::Limit,
            time_in_force: torus_types::TimeInForce::GTC,
            reduce_only: false,
            client_order_id: None,
        };
        let mk = |action: torus_types::NativeAction, nonce: u64| {
            let signed = torus_types::eip712::sign_native_action(action, nonce, &key);
            format!("0x{}", hex::encode(serde_json::to_vec(&signed).unwrap()))
        };

        let good_single = mk(torus_types::NativeAction::PlaceOrder(params(1)), now_ms);
        let bad_single = mk(torus_types::NativeAction::PlaceOrder(params(99)), now_ms + 1);
        let bad_batch = mk(
            torus_types::NativeAction::PlaceOrderBatch(vec![params(1), params(99)]),
            now_ms + 2,
        );

        // Batch pipeline: per-item errors, listed-market order admitted.
        let results: Vec<RpcSubmitResult> = client
            .request(
                "torus_submitNativeActions",
                jsonrpsee::rpc_params![vec![good_single, bad_single, bad_batch]],
            )
            .await
            .unwrap();
        assert!(results[0].hash.is_some() && results[0].error.is_none());
        assert!(results[1].error.as_deref().unwrap_or("").contains("unknown market_id 99"));
        assert!(results[2].error.as_deref().unwrap_or("").contains("unknown market_id 99"));
        assert_eq!(mempool.native_pool_size(), 1, "only the listed-market order admitted");

        // Single endpoint: call-level error.
        let err = client
            .request::<String, _>(
                "torus_submitNativeAction",
                jsonrpsee::rpc_params![mk(
                    torus_types::NativeAction::PlaceOrder(params(77)),
                    now_ms + 3
                )],
            )
            .await
            .expect_err("unknown market must be rejected on the single endpoint");
        assert!(err.to_string().contains("unknown market_id 77"));
        handle.stop().unwrap();
    }
```

**Implementation** — `crates/torus-rpc/src/torus.rs`:

1. New guard after `decode_action_bin` (:230-232):

```rust
/// RPC-only ingress guard (non-consensus; O2 design Open Question 2): reject
/// PlaceOrder / PlaceOrderBatch actions referencing a market_id with no row
/// in CF_NATIVE_MARKETS — closing the phantom-book trap where a typo'd id
/// "succeeds" into a tick=1/lot=1 book conjured at exec
/// (native_executor.rs:656; exec-side fix is a separate consensus item).
/// Point-gets on the 8-byte BE market key; the order-id counter row in the
/// same CF has a 24-byte key (NEXT_GLOBAL_ORDER_ID_KEY) so it never collides.
/// Cheap: runs BEFORE signature verify; batches dedup market ids first.
pub(crate) fn validate_known_markets(
    action: &torus_types::NativeAction,
    state_db: &torus_state::StateDb,
) -> Result<(), String> {
    let check = |mid: u64| -> Result<(), String> {
        match state_db.get_cf_raw(CF_NATIVE_MARKETS, &mid.to_be_bytes()) {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(format!("unknown market_id {mid}")),
            Err(e) => Err(format!("market lookup failed: {e}")),
        }
    };
    match action {
        torus_types::NativeAction::PlaceOrder(p) => check(p.market_id),
        torus_types::NativeAction::PlaceOrderBatch(orders) => {
            let mut seen = std::collections::BTreeSet::new();
            for p in orders {
                if seen.insert(p.market_id) {
                    check(p.market_id)?;
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
```

2. Call site 1 — `verify_one_action_with`, directly after :247 (before the ecrecover,
   error type is already the per-item `String`):

```rust
    torus_mempool::rate_limit::validate_batch_size(&action.action)?;
    validate_known_markets(&action.action, state_db)?;
```

3. Call site 2 — `submit_native_action` inline block, directly after :968:

```rust
            validate_known_markets(&action.action, &state_db)
                .map_err(RpcError::InvalidParams)?;
```

Error naming matches the existing per-item `RpcSubmitResult.error` convention (plain lowercase
reason strings like `"invalid hex: …"` / the `validate_batch_size` message; call-level errors
wrap as `RpcError::InvalidParams` exactly like the batch-size check at :967-968).

**Verify**:
```
lap test -p torus-rpc submit_rejects_unknown_market_single_and_batch
lap test -p torus-rpc
```

**Depends on**: nothing.

---### Task 7 — PlaceOrderBatch golden vectors (G5)

**Test first**: adding the vectors makes the existing drift gate
`fixture_file_matches_current_implementation` (`crates/torus-types/tests/eip712_vectors.rs:450`)
FAIL against the stale fixture — that is the natural red. Regenerating the fixture is the green.

**Implementation** — `crates/torus-types/tests/eip712_vectors.rs`, append to the `vec![...]`
in `all_vectors()` (after the `ListMarket` vector, :365-375):

```rust
        // O2/G5: the action market makers actually sign. Multi-order + a
        // batch-of-ONE — the latter pins that PlaceOrderBatch([x]) hashes
        // DIFFERENTLY from PlaceOrder(x) (distinct PlaceOrderItem typehash,
        // item hash carries no nonce); the struct-hash-uniqueness assert in
        // write_eip712_fixture_file enforces the non-collision. Inner order
        // of the batch-of-one deliberately equals PlaceOrder_Limit_Buy_GTC.
        build_vector(
            "PlaceOrderBatch_TwoOrders",
            NativeAction::PlaceOrderBatch(vec![
                PlaceOrderParams {
                    market_id: 1,
                    is_buy: true,
                    price: FixedPoint::from_raw(6_500_000_000_000),
                    quantity: FixedPoint::from_raw(10_000_000),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::GTC,
                    reduce_only: false,
                    client_order_id: Some(42),
                },
                PlaceOrderParams {
                    market_id: 2,
                    is_buy: false,
                    price: FixedPoint::from_raw(3_200_000_000_000),
                    quantity: FixedPoint::from_raw(5_000_000),
                    order_type: OrderType::Limit,
                    time_in_force: TimeInForce::PostOnly,
                    reduce_only: false,
                    client_order_id: None,
                },
            ]),
        ),
        build_vector(
            "PlaceOrderBatch_SingleOrder",
            NativeAction::PlaceOrderBatch(vec![PlaceOrderParams {
                market_id: 1,
                is_buy: true,
                price: FixedPoint::from_raw(6_500_000_000_000),
                quantity: FixedPoint::from_raw(10_000_000),
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::GTC,
                reduce_only: false,
                client_order_id: Some(42),
            }]),
        ),
```

The pinned key/nonce are shared with `eip712::tests::test_key` (`eip712.rs:1030`, `[0u8;31]||0x01`)
and `pinned_key()` (`eip712_vectors.rs:29`) — **do not touch either**; the three sites must stay
identical.

Regenerate the fixture ON THE LAPTOP, then pull it back to this tree (the `#[ignore]` test
writes to the laptop's checkout, and `lap test` rsyncs VPS→laptop only):

```
lap sh 'cd ~/projects/Torus-hyperBFT && PATH=$HOME/.cargo/bin:$PATH cargo test -p torus-types --test eip712_vectors write_eip712_fixture_file -- --ignored'
rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/crates/torus-types/tests/fixtures/eip712_vectors.json crates/torus-types/tests/fixtures/
```

**TS parity (external follow-up, NOT this repo)**: `torus-trading-app/tests/eip712.spec.ts`
must gain a PlaceOrderBatch signer + verify against the two new vectors. Track in that repo;
noted in the roadmap rewrite (Task 9).

**Verify**:
```
lap test -p torus-types --test eip712_vectors
```
(runs the drift gate + v-byte + domain tests against the regenerated fixture).

**Depends on**: nothing (vectors reflect the shipped EIP-712 encoding, untouched by Tasks 2-6).

---

### Task 8 — Criterion bench `exec_place_batch` (G6: N singles vs 1 batch of N, N=400)

**Test-first note**: benches have no red/green; the equivalence assertions inside the bench
body are its correctness gate (same orders both variants, all sigs resolve, N result rows).

**Implementation** — new file `crates/torus-bridge/benches/exec_place_batch.rs` (conventions
copied from `exec_batch_trades.rs`: tempdir RocksDB, senders funded once directly in the DB,
fresh `NativeStateOverlay` + `NativeExecContext` per iteration, `iter_batched` +
`BatchSize::LargeInput`):

```rust
//! O2 micro-bench (G6): the SAME 400 orders as 400 signed single PlaceOrder
//! actions vs ONE signed PlaceOrderBatch. Timed path = the exec pipeline's
//! Phase-3 verify (`batch_verify_native_actions`: 400 ecrecovers vs 1) plus
//! `execute_batch` (flatten + margin + match + settle) over a prod-shaped
//! NativeStateOverlay. Non-crossing resting buys spread over 4 markets
//! (100/market, under MAX_ORDERS_PER_TRADER_PER_MARKET=200) isolate the
//! placement path — no fills, so the delta is pure per-action overhead.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use torus_bridge::native_executor::{NativeExecContext, NativeExecutor};
use torus_core::position::{NativeBalance, PositionManager};
use torus_state::{NativeStateOverlay, StateDb};
use torus_types::eip712::{batch_verify_native_actions, sign_native_action};
use torus_types::{
    Address, FixedPoint, NativeAction, OrderType, PlaceOrderParams, SignedNativeAction,
    TimeInForce,
};

const N: usize = 400;
const MARKETS: u64 = 4;

fn fp(v: i64) -> FixedPoint {
    FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)
}

fn mm_key() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[0x42u8; 32]).unwrap()
}

/// Non-crossing resting buy i (deterministic; identical across variants).
fn order(i: usize) -> PlaceOrderParams {
    PlaceOrderParams {
        market_id: 1 + (i as u64 % MARKETS),
        is_buy: true,
        price: fp(50 + (i as i64 % 40)),
        quantity: fp(1),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    }
}

fn signed_singles() -> Vec<SignedNativeAction> {
    let key = mm_key();
    (0..N)
        .map(|i| {
            sign_native_action(
                NativeAction::PlaceOrder(order(i)),
                1_000_000 + i as u64,
                &key,
            )
        })
        .collect()
}

fn signed_batch() -> Vec<SignedNativeAction> {
    let key = mm_key();
    vec![sign_native_action(
        NativeAction::PlaceOrderBatch((0..N).map(order).collect()),
        2_000_000,
        &key,
    )]
}

fn bench_variant(c: &mut Criterion, name: &str, actions: Vec<SignedNativeAction>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = StateDb::open(dir.path()).expect("open db");
    let sender = actions[0].recover_sender().expect("recover");
    // Fund once, directly in RocksDB (overlay writes never flush).
    PositionManager::new(db.clone())
        .put_native_balance(
            &sender,
            &NativeBalance { available: fp(100_000_000), order_margin: FixedPoint::ZERO },
        )
        .expect("fund");

    // Equivalence gate: both variants carry EXACTLY the same N orders.
    let total_orders: usize = actions
        .iter()
        .map(|a| match &a.action {
            NativeAction::PlaceOrderBatch(o) => o.len(),
            _ => 1,
        })
        .sum();
    assert_eq!(total_orders, N);

    c.bench_function(name, |b| {
        b.iter_batched(
            || {
                let overlay = NativeStateOverlay::new(db.clone());
                NativeExecContext::new(overlay, 1, 1_000_000, 0, 1000, 100, sender, sender, sender)
            },
            |mut ctx| {
                // Phase-3 analogue: one verify pass — N ecrecovers vs 1.
                let senders = batch_verify_native_actions(&actions, 1_000_000, |_| None);
                let pairs: Vec<(Address, NativeAction)> = actions
                    .iter()
                    .zip(&senders)
                    .map(|(a, s)| (s.expect("valid sig"), a.action.clone()))
                    .collect();
                let result = NativeExecutor::execute_batch(&mut ctx, &pairs);
                assert_eq!(result.results.len(), N, "same flattened work both variants");
                ctx
            },
            BatchSize::LargeInput,
        );
    });
}

fn bench_singles(c: &mut Criterion) {
    bench_variant(c, "exec_place_batch/singles_400", signed_singles());
}

fn bench_batch(c: &mut Criterion) {
    bench_variant(c, "exec_place_batch/batch_400", signed_batch());
}

criterion_group!(benches, bench_singles, bench_batch);
criterion_main!(benches);
```

Register in `crates/torus-bridge/Cargo.toml` (after the `exec_batch_trades` entry, :38-40):

```toml
[[bench]]
name = "exec_place_batch"
harness = false
```

(`k256`, `criterion`, `tempfile` are already dev-dependencies — no manifest additions.)

Record the two medians + the ratio in the wrap-up notes; "batch ≥ single throughput" =
`batch_400` median ≤ `singles_400` median.

**Verify**:
```
lap sh 'cd ~/projects/Torus-hyperBFT && PATH=$HOME/.cargo/bin:$PATH cargo bench -p torus-bridge --bench exec_place_batch'
```

**Depends on**: Task 1 (semantics pinned), Task 2 (at-cap batches must not be skipped — N=400 < 1024, unaffected, but run after to bench final code).

---

### Task 9 — Devnet sweep script + docs (stale-entry correction)

Sweep EXECUTION is a later session step (devnet-up policy + binaries rsynced from the laptop);
this task delivers the script + docs.

**Implementation 1** — new file `devnet/sweep-o2-batchsize-s416.sh`, modeled line-for-line on
`devnet/sweep-o5-threshold-s415.sh` (same `height()`, `pulls()`, `fit_block_ms()` helpers
:50-84, same docker compose lifecycle + in-image bench invocation :93-124, same CSV shape :86).
Differences ONLY:

- Header comment: O2 Task 9, batch-size sweep, S416.
- No `TORUS_PUSH_THRESHOLD` export anywhere (compiled 512KB manifest+pull default — the
  S415-proven stable config).
- Legs are `bs:rate` pairs; rate scaled per leg so submitted orders/s
  (`SENDERS × rate × bs`) meets or exceeds the measured exec ceiling for bs ≥ 100
  (chain-bound legs) while bs=1 shows the single-action ceiling (action-cap-bound —
  the O2 baseline the batch win is measured against):

```bash
OUT=${OUT:-sweep-o2-batchsize-s416.csv}
echo "bs,rate,orders_s,block_ms_fit,avg_native_per_block,pull_delta,wedged" > "$OUT"

# bs:rate — submitted orders/s = 20*rate*bs: 2,000 / 20,000 / 24,000 / 40,960.
# bs=1@100 exposes the single-action cap ceiling (~250 actions/s at cap 100);
# bs>=100 legs are exec-bound (S415 measured 11.4k sustained at bs400).
LEGS=${LEGS:-"1:100 100:10 400:3 1024:2"}
for leg in $LEGS; do
    bs=${leg%%:*}; rate=${leg##*:}
    echo "=== leg bs=$bs rate=$rate ==="
    docker compose down -v >/dev/null 2>&1 || true
    docker compose up -d --build
    # ... identical wait/sample/pulls blocks as the S415 script ...
    docker run --rm --network host --entrypoint /bench \
        -v "$(cd .. && pwd)/target/release/bench-throughput:/bench:ro" \
        torus-devnet-node:local \
        consensus --rpc-urls "$RPCS" --batch-size "$bs" --senders "$SENDERS" \
        --duration "$DURATION" --rate "$rate" --pre-sign "$((DURATION * rate))" \
        --sign-mode "$SIGN" --markets "$MARKETS" \
        --format bin 2>&1 | tee "bench-o2-bs$bs.txt"
    # ... identical orders_s/avg_native/block_ms/wedged extraction ...
    echo "$bs,$rate,${orders_s:-0},${block_ms:-0},${avg_native:-0},$((p1 - p0)),$wedged" >> "$OUT"
    docker compose down -v >/dev/null 2>&1 || true
done
```

- Acceptance line in the header: *all four legs `wedged=0`; orders/s strictly increasing
  bs=1 → bs=400; report the orders/s + block_ms_fit table in the roadmap entry.*
- Note in header: bench signs with `--sign-mode session` registering `SessionScope::Full`
  sessions (main.rs:603) — the sweep does NOT depend on Task 5; devnet rebuilds the whole
  fleet from this branch so no lockstep concern inside the sweep.

**Implementation 2** — `docs/plans/blockspeed-orders-roadmap-s405.md:70-75`, replace the stale
O2 entry (feature did NOT need writing-plans — it shipped 2026-06-06):

```markdown
- [ ] **O2 PlaceOrderBatch** — CORRECTION: feature SHIPPED 2026-06-06 as Phase B
  B1–B5 (b58f858) — one ecrecover + one manifest entry per batch; the old entry
  ("NEEDS WRITING-PLANS") was stale (premise fix: mem 27a66377be573bc0). S416
  close-out (design docs/plans/o2-placeorderbatch-design.md, impl
  docs/plans/o2-placeorderbatch-impl.md): deterministic exec-side batch cap
  (skip-wholesale at the execute_batch flatten, G1) + gossip/DA-admit checks;
  order-aware selection budget (order_count → NATIVE_ORDERS_PER_BLOCK_CAP, G2);
  SessionScope::Trading += PlaceOrderBatch (G3, LOCKSTEP: whole fleet before
  clients sign Trading-scoped batches); partial-per-order contract pinned by
  test (G4); PlaceOrderBatch golden vectors multi+single (G5 — TS parity in
  torus-trading-app is an external follow-up); exec_place_batch criterion
  bench + BS={1,100,400,1024} sweep script (G6,
  devnet/sweep-o2-batchsize-s416.sh). REMAINING to check off: run the sweep
  (0 wedges, orders/s + block_ms_fit table) and paste the verdict here.
```

(Checkbox flips to `[x]` only with the sweep verdict — same standard O3/O5 were held to.)

**Implementation 3** — `.claude-state.json` (project handoff): set "What's Next" to
*"run devnet/sweep-o2-batchsize-s416.sh (binaries: lap build + rsync back), paste verdict into
roadmap O2 entry, flip checkbox"*; note the Task 5 lockstep constraint for the next testnet
relaunch. (State file is hook-maintained — update via the wrap-up flow, not by hand-editing
beyond these fields.)

**Verify**:
```
bash -n devnet/sweep-o2-batchsize-s416.sh
```
(syntax gate on the VPS; execution deferred by policy).

**Depends on**: Tasks 2-8 (the docs claim them).

---

## Verification (end-to-end)

On the laptop (via `lap` from this VPS):

```
lap sh 'cd ~/projects/Torus-hyperBFT && PATH=$HOME/.cargo/bin:$PATH cargo test --workspace'
lap sh 'cd ~/projects/Torus-hyperBFT && PATH=$HOME/.cargo/bin:$PATH cargo clippy --workspace --all-targets -- -D warnings'
lap sh 'cd ~/projects/Torus-hyperBFT && PATH=$HOME/.cargo/bin:$PATH cargo bench -p torus-bridge --bench exec_place_batch'
```

Focused per-task suites (fast loop during implementation):

```
lap test -p torus-bridge --test parallel_matching_tests
lap test -p torus-mempool
lap test -p torus-types
lap test -p torus-rpc
lap test -p torus-consensus
lap test -p torus-integration-tests --test o5_feed_gates
lap test -p torus-types --test eip712_vectors
```

Devnet sweep (later session; on this VPS):

```
lap build
rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/target/release/torus-node       target/release/
rsync -az -e "ssh -p 2222" oz@localhost:projects/Torus-hyperBFT/target/release/bench-throughput target/release/
bash devnet/sweep-o2-batchsize-s416.sh
```

Acceptance: 4 CSV rows, `wedged=0` on all, orders/s monotonically increasing bs=1→400 (bs=1024
row is informative — exec-bound plateau or gain both acceptable), block_ms_fit reported per leg.

## Rollback

One commit per task → `git revert <sha>` in reverse order. Specific notes:

- **Task 2 (constant move + exec skip)**: the torus-types/torus-mempool/torus-bridge edits are
  compile-coupled — they land as ONE commit and revert as one. Consensus caveat: reverting on a
  PARTIAL fleet re-opens the mixed-window divergence (old executes >cap batch, new skips);
  revert fleet-wide, and only if no oversize batch has ever been committed (none can exist via
  honest paths — RPC + admit reject them).
- **Task 3 (admit check)**: node-local policy, revert freely per node; no fork risk.
- **Task 4 (order budget)**: proposer-local selection only (`validate_block` never rejects on
  count) — revert freely; `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP` env is the no-rebuild kill switch
  (set huge to neutralize).
- **Task 5 (Trading scope)**: **LOCKSTEP BOTH WAYS.** Must not reach testnet clients before the
  whole validator fleet runs it (old validators slash the proposer of a block containing a
  Trading-scoped batch). Reverting after clients have signed Trading-scoped batches re-creates
  the same slash-the-proposer hazard for in-flight actions — drain/expire the 60s nonce window
  before reverting fleet-wide.
- **Task 6 (market guard)**: RPC-only, revert freely; no consensus surface.
- **Task 7 (vectors)**: revert the vector additions AND the regenerated fixture together, or
  the drift gate fails.
- **Task 8 (bench)**: additive; revert freely.
- **Task 9 (script/docs)**: additive; revert freely.
