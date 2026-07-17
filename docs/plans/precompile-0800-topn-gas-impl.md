# Implementation Plan: 0x0800 OrderBookReader — hybrid top-N bound + per-level gas

Branch: `feat/precompile-0800-topn-gas` (off `perf/deep-book-storage` @ fd00010).
Seed of the pre-mainnet consensus batch (#8b rides on this branch later).
CONSENSUS-BREAKING: new CF in the root preimage + gas schedule change. Fresh
chain or offline rebuild only — same constraint the parent branch already has.

## Design Decision (user-approved direction: hybrid)

Problem: `getOrderBook` charges flat `GAS_PRECOMPILE_READ = 2_600` but does
O(depth) work. On deep-book rows the full-market row scan measured 48ms at 16k
orders (rows-only leg) — a 2,600-gas DoS amplifier once books deepen.

Fix (three legs, all consensus-relevant):

1. **Price-level aggregate rows** (new CF `CF_NATIVE_BOOK_LEVELS`, in
   `NATIVE_ROOT_CFS`): key `market_id(8 BE) ‖ side(1) ‖ price_enc(16)`,
   value = borsh `FixedPoint` total quantity. `price_enc` = BE bytes of the
   price for asks, **bitwise-NOT** BE bytes for bids — forward lexicographic
   iteration is best-first on both sides. Maintained O(touched-levels) from a
   level journal in `OrderBook`, flushed by `save_book_delta` alongside order
   rows. This is what makes bounded *work* possible at all: order-row keys are
   id-ordered, not price-ordered, so any read through them is O(depth).
2. **Top-N bounded read**: `getOrderBook(bytes32)` returns the best
   `TOP_N_LEVELS_PER_SIDE = 200` levels/side via the level CF (O(N) point
   reads). New selector `getOrderBook(bytes32,uint32 n)` for callers that want
   fewer (n clamped to ≤ TOP_N; n=0 → empty arrays, base gas). Same 4-array
   ABI shape — truncation is semantic, not an ABI break (and no deployed
   consumer exists yet per 2026-07-01 survey).
3. **Per-level surcharge**: `gas = GAS_PRECOMPILE_READ + GAS_PER_BOOK_LEVEL ×
   (bid_levels_returned + ask_levels_returned)`, `GAS_PER_BOOK_LEVEL = 100`.
   Pure function of consensus state + calldata → deterministic across
   validators; eth_estimateGas runs the same function so estimates are exact.
   Worst case `2_600 + 100×400 = 42_600` gas — document as the safe forward
   allowance. Mispriced k is bounded by the O(N) work cap (structural safety).

Other 0x0800 selectors (`getPosition`, `getOpenOrders`) keep flat gas:
positions are O(1); open orders are bounded by trader_cap (200). 0x0801–0x0803
untouched (O(1) reads — flat 2,600 stays honest).

## Implementation deviations (as built — both simplifications)

1. **No new CF.** Level rows live in `CF_NATIVE_ORDER_BOOKS` itself under a
   new tag `0x02` (26-byte keys, length-disjoint from the 8/9/25 layouts) —
   automatically inside the root preimage, zero `cf.rs`/descriptor churn.
   Value = raw i128 BE total quantity (FixedPoint has no borsh impl —
   explicit codec).
2. **Additive gas API.** `execute_precompile`/`execute_precompile_read_only`
   keep returning `Vec<u8>` (no churn across callers); new
   `execute_precompile_with_gas` variants return
   `PrecompileOutput { data, gas_used }` and the EVM provider charges
   `gas_used` post-execution (OOG if over limit; reverts charge the flat
   base). Only the provider ever charged gas, so nothing is lost.

Constants SIGNED OFF 2026-07-17: TOP_N_LEVELS_PER_SIDE = 200,
GAS_PER_BOOK_LEVEL = 100 (max getOrderBook call = 42,600 gas).

## Constants pending USER SIGN-OFF before merge-freeze

- `TOP_N_LEVELS_PER_SIDE = 200` (per side)
- `GAS_PER_BOOK_LEVEL = 100`
- Level CF in root preimage: YES (gas input must be consensus-guarded;
  incremental root makes the ~2× dirty-key cost cheap)

## Success Criteria

- Gas at empty/absent market == 2_600 exactly; at depth == 2_600 + 100×levels
  returned; OOG when gas_limit < actual; estimateGas == execution gas.
- Level CF byte-equals recomputed levels after arbitrary op sequences,
  including save→restart→load (oracle test), and across the block-boundary
  book cache (resume path).
- Precompile output == `to_snapshot()` truncated to N, for both selectors.
- Existing suites green: order_book, order_book_store, book_cache_tests,
  evm_tests, cross_vm_read, chaos, rate_limit_mev.
- Devnet no-regression vs fd00010 control (same compose): cross bs50 r30
  placed/s in 24–26k band; 16k-depth leg exec_block/blk-s parity (~26ms/15.3);
  getOrderBook sha256-identical across 4 validators; reader-spam leg at 16k
  depth shows bounded exec and correct gas.

## Tasks

### Task 1: Level-row layer + journal (test first)
- Test (`order_book_store.rs` tests + new oracle in
  `crates/torus-core/tests/`): after random place/cancel/fill sequences +
  `save_book_delta`, iterating `CF_NATIVE_BOOK_LEVELS` for the market yields
  exactly `to_snapshot()`'s levels, best-first, both sides; empty level rows
  absent; restart (fresh load_book + fresh save) idempotent. FAILS (CF and
  journal don't exist).
- Impl: `cf.rs` add `CF_NATIVE_BOOK_LEVELS` (+ `NATIVE_ROOT_CFS`, descriptors);
  `order_book.rs` add `level_journal: BTreeSet<(bool, FixedPoint)>` populated
  at every point `row_journal` is (add/remove/fill touch a (side, price));
  `order_book_store.rs` add `level_row_key(market, is_bid, price)` with NOT-
  encoding for bids, and in `save_book_delta` drain the journal: level present
  in BTreeMap → upsert summed qty; absent → delete row. Full-persist path
  writes all levels.
- Verify: `cargo test -p torus-core order_book`

### Task 2: Bounded backend iteration (test first)
- Test (`torus-state`): `iterate_cf_bounded(cf, prefix, limit)` == first
  `limit` of `iterate_cf` on rocks backend AND through the overlay with
  pending upserts + tombstones shadowing DB rows. FAILS (method absent).
- Impl: `backend.rs` trait method with default impl (full scan + truncate —
  correct everywhere); rocks override (raw iterator, early stop at limit);
  overlay override (DB fetch of `limit + overlay_prefix_len`, merge, dedup,
  truncate).
- Verify: `cargo test -p torus-state bounded`

### Task 3: Bounded read + n selector (test first)
- Test (`cross_vm_read.rs`): seeded book deeper than N → precompile returns
  exactly N best levels/side matching truncated `to_snapshot()`; explicit-n
  selector returns n; n=0 empty; absent market empty. FAILS.
- Impl: `precompiles.rs` `read_order_book(state, market, n)` reads via two
  `iterate_cf_bounded` calls on the level CF (no full-book load, no borsh
  book decode); add selector `getOrderBook(bytes32,uint32)`; legacy selector
  delegates with n = TOP_N.
- Verify: `cargo test -p torus-integration-tests --test cross_vm_read`

### Task 4: Dynamic gas plumbing (test first)
- Test (`evm_tests.rs`): empty-book call gas == 21_000 + 2_600 + calldata;
  call at 300-level book with default N == +100×400? NO — 300 levels/side →
  +100×(200+200 capped)… seeded exactly: 250 bid / 3 ask levels → surcharge
  100×(200+3). OOG test: gas_limit = base+1 at depth → OutOfGas. estimateGas
  == exec gas. FAILS.
- Impl: `precompiles.rs` `execute_precompile*` return
  `PrecompileOutput { data: Vec<u8>, gas_used: u64 }` (all non-book paths:
  `precompile_gas(id)`); book path computes surcharge from levels returned.
  `precompile_provider.rs run()`: pre-check base only, execute, then
  `record_cost(output.gas_used)`; over-limit → OOG (output discarded).
  Revert keeps base-gas charge (current behavior).
- Verify: `cargo test -p torus-evm`

### Task 5: Full-suite + book-cache/resume proof
- `cargo test --workspace` (fix fallout: gas-assert tests, provider callers).
- Verify level rows correct through `take_book_cache`/`resume` (extend
  `book_cache_tests.rs`: resumed context + ops → save → level CF matches).

### Task 6: Devnet no-regression A/B (like last two rounds)
- Control = fd00010 binary, same compose. Legs: cross bs50 r30 300s
  (placed/s band); rest bs50 r20 16k-depth 600s (exec_block, blk/s,
  save/load metrics parity, 4-validator sha256 book agreement); NEW
  reader-spam leg: EVM contract loop-calling getOrderBook at 16k depth —
  bounded exec_block, gas receipts == formula.
- Artifacts under scratchpad mission dir; verdict table in RESULTS.txt.

## Verification (end-to-end)
Tasks 1–5 gates green locally; Task 6 devnet verdict PASS on all legs; then
present for merge sign-off (constants N/k final call).

## Rollback
Single-branch, additive CF: `git branch -D` + discard worktree devnet volumes.
No deployed chain carries this preimage yet.
