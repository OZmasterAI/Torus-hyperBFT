# Implementation Plan: item 6 Phase 5 (full in-memory execution)

Status: DRAFT, numbers pending Gate 3 (2026-10-14, s119). Nothing built. Section 7 decided (owner s122: 1 a, 2 a, 3 a, 4 a, 5 b, 6 a, 7 a, 8 a, 9 build for the gain).
Parent design: `market-scaling-in-memory-design.md` Phase 5, section 4 (memory budget), section 5 rules 1, 2, 4, 5, 6,
8, 9, D7 c, D10, D11 a, D12 b, D16. Short form: `item6-phases-2-5-plans.md` Phase 5. Format and base:
`item6-phase4-impl.md` (and `item6-phase3-impl.md`).

**Base: Phase 4, built first as Phase 5's base (owner s119, not folded in).** Phase 4 steps 1-4 go ahead; step 5 (the
open-order counter) runs last and stays only on its own net A/B, otherwise it is dropped or moved here (optional step 4
below). **Phase 4 is not built yet:** its step 1 has RED tests only (`feat/item6-phase4-step1` `da59b395`). Wherever
this plan names `Users`, `UserState`, `ResidentSource` or typed loads, it means the shape in `item6-phase4-impl.md`
section 3 and steps 1-4, not code. Those anchors are re-checked when Phase 4 merges.

**Code references are `file:line` on main `21d238e0`** (= origin/main). Crate paths are under `crates/`.

Every number below is from the results doc (`docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md`, "section N"), from
the code or from the Phase 4 plan, unless marked **(estimate)**. Placeholders that wait for Gate 3 are marked
**TBD (Gate 3)**.

## Design decision

Proposed (decision 1): **hot state typed and mutated in place; cold state stays raw rows.**
- Typed in memory, mutated in place by the executor, one dirty set per structure:
  - `Users` (Phase 4's record: balance, `cum_volume`, positions, sums; plus the open-order count if optional step 4 is
    kept);
  - books (already typed in `ResidentInner`, `native_executor.rs:3170`). Their rows come from a dirty set instead of
    `save_order_books`' diff.
- The per-block change set is the same logical rows as today. At freeze, the typed dirty sets serialize each changed
  row once, and the result is merged with the overlay's pending rows of the cold CFs into the same `FrozenPending`
  (`backend.rs:951`). The digest, `L`, the published view and the checkpoint keep reading `FrozenPending` unchanged.
  No consensus change, no state format change, no `h_n` change (D4 a, section 5 "Running state hash").
- Cold CFs stay on the overlay and R, raw, as today: staking, governance, sessions, oracle, markets, liquidation,
  nonces (decision 2), EVM accounts.
- Cold writers to the typed CFs keep their code. An overlay write to positions or balances goes through a write-through
  path: decode, apply to `Users`, mark dirty (decision 3). So rule 5 holds: every write still reaches the one change
  set.
- EVM blocks (serial barriers) keep raw, journalled writes and fold them into `Users` once the journal can no longer
  revert them (decision 4).
- RPC needs no adapter: validator RPC reads the durable DB (`torus-rpc/src/lib.rs:248`, Phase 3 D10 c), and ingress
  reads the `ReadView` (`torus-state/src/read_view.rs:88`). Both are fed from `FrozenPending`.

No runtime switch (D16). Each step keeps a test-only reference path (today's overlay writes, i.e. the Phase 4 path)
behind `cfg(test)` / the `test-reference-paths` feature (`torus-core/Cargo.toml:23`) for the differential.

## 1. Goals and gates

Goal: native execution touches typed state only. No per-write overlay path (lock, allocation, Borsh) on the hot path.
No cache flush and no `save_order_books` diff. No `end_resident` apply for the typed CFs. Each changed row is
serialized once per block.

The phase gate is the final item 6 gate. It is called **"final Gate 3 (1.4x)"** here because the crab ubench gate in
the Phase 1 / 2 plans is also named Gate 3 (`item6-phase1-impl.md:349`, `item6-phase2-impl.md:42`).

| gate | target | measured by |
|---|---|---|
| final Gate 3 (1.4x) | matched/s >= 1.4x main `199bdc97` at 300 markets, oracle off primary, as shipped (oracle on) secondary, the Gate 3 recipe (ozarchy-g3s0) | step 7 |
| phase gate vs Phase 4 | matched/s >= **TBD (Gate 3)** vs the Phase 4 head, derived as `1.4 / (r x (1 + p4)) - 1`. `r` = `d3cba5e2` / `199bdc97` from Gate 3; `p4` = the Phase 4 head vs `d3cba5e2` from Phase 4 step 6. Lower bound of the paired CI; native blocks/s no regression; no 10-market regression (same campaign, D15) | step 7 |
| mechanism | exec `block_ms` down by >= **TBD (Gate 3)** ms per native block (the ms the phase gate needs, at +0.4-0.55% matched/s per ms, `item6-phase2-impl.md:55`); `torus_exec_cache_flush_seconds` and `torus_exec_save_books_seconds` ~0 for typed state; `end_resident` rows ms down | step 7 |
| CPU | node CPU-s/1M no regression beyond noise | step 7 |
| crab overhead | engine ms per 1k fills oracle on <= +15% vs oracle off, same binary and campaign (decision 6; the base ratio on main is **TBD (Gate 3)**, from its two arms) | step 7 |
| correctness | full differential vs the Phase 4 path on long randomized sequences: identical rows, CF dumps, every `h_n` and every action result; serial and pipelined; interval 0 / 15 s / never; all BookModes; EVM blocks; goldens A and B unchanged; P1, P3, P5 (warm == cold), P7 green; `torus_exec_checkpoint_deferred_read_waits` stays 0; SIGKILL multi-crash replay identical | tests, steps 1-6; step 7 crash cells |
| RAM per user | within section 4 (light ~2.4 KB, active ~21 KB incl. orders); Phase 4's record sizes unchanged or smaller | step 7 ubench |
| genesis cell | 1M accounts / 1M resting orders: RSS per node **TBD** (section 4: ~8 GB of state for 1M mixed users **(estimate)**; the 1M-order cell has no number yet), restart -> ready <= 60 s | step 7 |
| block time p99 | no regression vs the Phase 4 head beyond noise. Target and cause wait on the commit-interval p99 decomposition (section 9) | step 7 |
| EVM / RPC | EVM, precompile and RPC suites green, unchanged | tests |

**Why a TBD gate.** Gate 3 (campaign ozarchy-g3s0: `199bdc97` vs `d3cba5e2`, oracle off primary, as shipped
secondary, an LD_PRELOAD-jemalloc arm) is running. It sets how far today's main is from 1.4x, and that distance sets
how much Phase 5 must deliver.

**How much Phase 5 can remove (estimates from older builds; step 0 re-measures).** Exec wall is 244.4 ms per native
block at ~49k fills on main `d3cba5e2` (section 64.2), ~217k matched/s, ~4.2 native blocks/s (section 64.1). Phase 4
removes R reads, 2.43% of exec wall (~5.9 ms), and possibly `open_order_counts`, 1.45% (3.5 ms). These are gross
figures (section 64.4). What Phase 4 leaves to Phase 5 (`item6-phase4-impl.md` "Why only +0-10%"):

| item | last measured | build / section |
|---|---|---|
| cache flush (`PositionCache` / `BalanceCache` `flush_all` -> overlay), nested in settle | 11.6-14.7 ms per native block, 0.43-0.55 ms per 1k fills; inside `flush_all`, the per-row `BTreeMap` insert was 7.2 of 17.6 ms | Phase 2 builds, sections 25, 33. At ~49k fills: ~21-27 ms **(estimate)** |
| other overlay writes on the hot path (cum volume, liquidation / ADL writes) | not split out | step 0 |
| `save_books` (`save_order_books` + `diff_stop_rows`) | 13.2 ms per native block | main `ef812b02`, section 59.3 |
| `end_resident` (worker) / exec wait | rows 13.3 + positions 10.0 ms on the worker / wait 2.1 ms | `d3cba5e2`, section 64.2 |
| checkpoint settle wait / upkeep | 3.1-3.5 / 0.8-1.2 ms | sections 54.2, 61.1 |
| residual untimed | 13.3 ms | section 59.3 |

Sum of plausibly removable items: ~25-45 ms per native block **(estimate)**, i.e. +10-25% matched/s by the rule of
thumb **(estimate)**. **Caveat (section 54):** exec time saved showed up as +11.3% native blocks/s with matched/s a
tie in the closed-loop bench. The matched/s gate can lag the mechanism.

### Scope by the Gate 3 gap (decision 9)

Needed Phase 5 gain `g = 1.4 / (r x (1 + p4)) - 1`; ms per native block = `g / (0.4-0.55%)`. With `p4` = +3%
**(estimate)**:

| Gate 3 `r` | Phase 5 must add | ms per native block | proposed scope |
|---|---|---|---|
| >= 1.36 | ~0% | ~0 | Phase 5 is not needed for the gate. Build steps 1-2 only if the owner wants the typed base for option D; otherwise stop |
| ~1.25 | ~+9% | ~16-22 | small gap: steps 1, 2, 3 (+ 5 for the crab gate) |
| ~1.15 | ~+18% | ~33-46 | medium: steps 1-6, all of them |
| <= ~1.05 | >= ~+29% | >= ~54-74 | large: above what Phase 5 removes **(estimate)**. Owner picks: add exec-thread work outside item 6 (signature verify 11.5 ms per native block, section 59.3, off the exec thread; per-fill margin / settle cost), restate the target, or judge on blocks/s too |

The `r` rows are examples, not predictions. The real row is chosen when Gate 3 reports.

## 2. What changes vs the design doc (code check on main `21d238e0`)

| # | design says | code on `21d238e0` (and the Phase 4 plan) | consequence |
|---|---|---|---|
| 1 | all native consensus state typed in memory | 49 CFs in `ALL_CF_NAMES` (`torus-state/src/cf.rs:266`). Exec-thread write cost sits in positions / balances (cache flush) and book rows; staking, governance, sessions, oracle, markets are written rarely | type the hot state only; decision 1 |
| 2 | overlay off the hot path | after Phase 4 the reads of the two CFs come from `Users`. Writes still go `put_cf_raw` / `_owned`: RwLock write lock + `key.to_vec()` (+ `value.to_vec()`) + `BTreeMap` insert per row (`backend.rs:2419-2443`), Borsh on the caller (`put_position`, `torus-core/src/position.rs:292-298`), via `PositionCache::flush_all` (`position.rs:757`) and `BalanceCache::flush_all` (`native_executor.rs:396`), timed at `native_executor.rs:7490-7513` | steps 1-2 |
| 3 | change set produced from typed dirty sets; freeze copy removed | `freeze` is already a move (`mem::replace` under the write lock, `backend.rs:1476`). The digest (`block_digest` :1857 over `for_each_consensus_write` :762; `digest_on_worker` :1213), `L` (`checkpoint_layer.rs:106`), R's delta (`resident_delta_of` :960) and the view all read `FrozenPending` | Phase 5 fills `FrozenPending`'s typed CFs from dirty sets; its consumers are untouched |
| 4 | RPC through a `StateBackend` adapter over typed state | RPC reads `StateDb` (`torus-rpc/src/lib.rs:248`; `get_position` `torus.rs:1075`, `get_balances` :1155, `get_user_limits` :1234). Ingress reads `ReadView` (`read_view.rs:88`, `publish_ingress_view` `app.rs:2565`). Both are fed from frozen rows | no RPC adapter. `getUserLimits` O(1) from `UserState` is a separate RPC task (not building) |
| 5 | EVM precompiles through an adapter | precompiles take `&impl StateBackend` over `TorusPrecompiles.journal: NativeStateOverlay` (`torus-evm/src/precompile_provider.rs:34`). Lockbox writes `CF_NATIVE_BALANCES` (`credit_native`, `lockbox.rs:113-125`). EVM reverts use the overlay journal (`begin_tx` / `revert_tx` / `revert_to_checkpoint`, `backend.rs:1542-1633`; `torus-evm/src/executor.rs:327-510`). EVM, slash and epoch blocks run serial (`app.rs:2834-2840`) | keep raw journalled writes at EVM blocks; fold into `Users` after (decision 4) |
| 6 | windowed nonce set (needs D12 b, "before Phase 5") | D12 is not built. One nonce row per action: check `app.rs:3303-3307`, write `app.rs:3841-3846` | nonces stay raw unless D12 lands; decision 2 |
| 7 | books done | books are typed (`ResidentInner.books`, `native_executor.rs:3171`). Rows come from a diff: `save_order_books` :5840, `diff_stop_rows` :6281; the deferred modes use `save_order_books_deferred` :6120 | step 3 for Classic. Modes 2/3 keep the diff (Phase 3 refuses an interval with them) |
| 8 | crab: summary updated inline per fill | `SumsCache` (:1148, moving into `UserState` in Phase 4 step 3), `BlockSums` memo (:1169), `sums_with_changes` (:1033). The mark version moves most blocks with the oracle on (Phase 4 row 5) | inline update is valid within one mark version (step 5). The gate's "same phase without crab" has no binary; decision 6 |
| 9 | target = 10-market engine cost per fill (~6 µs) at any market count; up to +60-90% vs main | 300-market engine cost is already 3.42-3.47 ms per 1k fills (section 64.1); the estimate predates Phases 1-3 | superseded by the Gate 3 budget (section 1) |
| 10 | `open_order_count` in `UserState` | counted from the books per call (`order_book.rs:54`, wrapper `native_executor.rs:8997`, `open_at_start` :7019 / :9762); Phase 4 step 5 is conditional (s119) | optional step 4 |
| 11 | (not in design) | a native batch stages writes in the caches and flushes them at the batch end (`native_executor.rs:7490-7513`); a flush error sets `fatal_error`. Whether any action path writes before its last failure check is not audited | task 2.1 audits it and pins "a rejected action leaves `Users` untouched" |
| 12 | (not in design) | `torus-state` cannot name `Position` / `NativeBalance` (Phase 4 row 12) | the write-through goes through Phase 4's byte-level `ResidentSource`, extended with writes |

## 3. Shape of the core

```
Phase 4 (planned)                                   Phase 5
  writers -> caches -> overlay pending (raw)          hot writers -> UsersMut (in place) + dirty set
  R slots 0-1 = Users (ResidentSource), follows       Users = the state; no follow for its CFs
    the frozen delta at end_resident                  cold raw writes to its CFs -> write-through -> Users
  books -> save_order_books diff -> pending           books -> book dirty set -> rows at freeze (Classic)
  freeze: pending -> FrozenPending                    freeze: dirty sets serialized once + cold pending
                                                        -> FrozenPending (same rows, same order)
```

- **Dirty sets.** `Users` keeps a per-block set of changed `(trader, market)` positions, balance and `cvlm` keys, and
  removals. At freeze, each key is encoded from the record once (Phase 4's re-encode rule keeps the bytes equal) and
  written into the frozen set. Opaque traders (irregular rows, Phase 4 section 3) stay raw on the overlay path.
- **Reads.** Typed CFs: own pending (cold raw writes not yet folded, EVM blocks only) -> `Users`. They never fall past
  `Users` (Phase 4 row 9 holds; deferred read waits stay 0).
- **Pipelining.** Block n+1 runs while n's frozen set is merged into `L`. Because `Users` already holds n's writes and
  n's rows were serialized at its freeze, nothing has to be shared with the merge thread.
- **Fatal block.** Same as today: `exec_failed` latches, and `Users` is invalidated and rebuilt cold (rule 6).
- **Order.** Dirty keys are emitted in `(cf_id, key)` order (rule 1). The dirty sets are `BTreeSet`s, or hash sets
  sorted at freeze; step 1 measures which.

## 4. Steps and tasks

Each task: test first (fails before), then code, then verify with
`F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"`.
Before every commit: `cargo nextest run --workspace $F` and the doc-test command from `TESTING.md`.

| step | what | size | who |
|---|---|---|---|
| 0 | re-profile the Phase 4 head; stop rule against the Gate 3 gap | S | archy |
| 1 | typed write API + dirty sets + rows at freeze, shadow only; long differential harness | M | builder |
| 2 | hot writers mutate `Users` in place; caches and their flush deleted; write-through; EVM fold | L | builder, then Codex torus-adversarial |
| 3 | book rows from a dirty set (Classic) | M | builder, then Codex torus-adversarial |
| 4 | (optional) open-order count in `UserState` from the book dirty sites | M | builder |
| 5 | crab: summary updated inline per fill; summary slimmed for light users (owner s121) | M | builder |
| 6 | (conditional) remaining hot overlay reads (oracle, markets) to typed tables | S | builder |
| 7 | RAM ubench, 1M genesis cell, gate campaign incl. final Gate 3 (1.4x) | M | builder (ubench), archy (campaigns) |

### Step 0: re-profile (archy, measurement only, no code)

Precondition (owner s119): D12 is on main (built after Phase 4 step 6), so this baseline includes its cost.

On the Phase 4 head + D12, 300 markets uniform, oracle on and off, interval 15 s, perf on val1 (section 64's sidecar). Measure
the exec-thread share and ms per native block of:
- overlay writes to the two typed CFs: `put_cf_raw` / `put_cf_raw_owned` / `delete_cf_raw` incl. the lock, the key and
  value copies and the `BTreeMap` insert, plus the callers' Borsh encode;
- `torus_exec_cache_flush_seconds`, settle pass B `merge_disjoint` (`position.rs:729`, `native_executor.rs:388`);
- `torus_exec_save_books_seconds` and `diff_stop_rows`;
- `end_resident` rows / users ms and the exec wait; checkpoint settle wait and upkeep; residual untimed;
- overlay reads of the cold R slots (oracle, markets, liquidation) and sessions on the exec thread (step 6's trigger:
  >= ~1% of exec wall **(estimate)**);
- the per-fill sums work (`BlockSums::base` :1337, `sums_with_changes`), oracle on vs off (step 5, decision 6);
- `open_order_counts` if Phase 4 step 5 was dropped (optional step 4);
- 10-market cells for engine ms per 1k fills.

Add the **commit-interval p99 decomposition** (section 9), queued for archy after Gate 3.

**Stop rule:** step 0's removable ms per native block x 0.4% must reach the ms the phase gate needs (**TBD (Gate 3)**,
section 1 table). If it does not, the owner re-scopes (decision 9) before any code.

**Measured:** TBD (step 0, after Phase 4 step 6 and Gate 3).

### Step 1: typed writes and rows at freeze, shadow only

The overlay still takes every write. The new path runs next to it in tests and is checked row for row.

**Task 1.1: `UsersMut` and the dirty sets**
- Test first (`torus-bridge`, `users_tests.rs` from Phase 4 step 1):
  - `dirty_rows_equal_overlay_rows_every_block`: random blocks over every writer of the two CFs (fills,
    liquidation, ADL, lockbox credit, `margin.rs` balance writes, `cvlm`, opens / closes / flips, irregular rows, deletes). After each block, the
    rows serialized from the dirty sets equal the overlay's pending rows for positions and balances, byte for byte,
    deletes included.
  - `dirty_rows_sorted_and_once`: each changed key appears once, in `(cf_id, key)` order, whatever the order of
    mutation.
  - `opaque_trader_stays_raw`: a trader with an irregular row is never serialized from the record.
- Code: `UsersMut` (`set_position`, `remove_position`, `set_balance`, `add_cum_volume`) marks keys dirty;
  `Users::drain_dirty_into(&mut FrozenPending)` (test-only call site in this step). The record shape is Phase 4 section 3.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`.

**Task 1.2: long differential harness (reused by every later step)**
- Test first: `phase5_differential_long_random` (`torus-consensus`, next to `warm_equals_cold_every_block_serial`
  `app.rs:24168`). Two nodes on the same random block stream, one on the reference path (overlay writes,
  `test-reference-paths`), one on the typed path. Per block: CF dumps, `h_n`, action results. Covers serial and
  pipelined, interval 0 / every 5 blocks / never, all BookModes, EVM / slash / epoch blocks, a fatal block.
  ~10k blocks in an ignored long variant, ~300 in the default run **(estimate)**.
- Code: the harness only. In this step both nodes run the reference path (the test must pass trivially and catch a
  planted divergence: `harness_detects_one_flipped_row`).
- Verify: `cargo nextest run -p torus-consensus $F`.

### Step 2: hot writers mutate `Users` in place

Consensus-adjacent write path. Codex torus-adversarial review before step 3.

**Task 2.1: settle, balances and volume on `UsersMut`**
- Test first:
  - `rejected_action_leaves_users_untouched`: every rejection path of `execute_batch` (`native_executor.rs:6682`;
    every rejection reason the batch can return) leaves `Users` and its dirty set equal to before. This also
    audits row 11 of section 2.
  - `phase5_differential_long_random` with the typed path on.
  - P1 / P3 on the in-place record: `sums_cache_equals_reference_on_seeded_sequences`, the liquidation and ADL walk
    tests from Phase 4 step 1.
  - Goldens A and B (`tests/perf_equivalence_golden.rs:847-889`) unchanged, serial and engine-forced.
- Code: settle pass B applies its per-market deltas to `UsersMut` in market order (rule 2); pass A keeps reading
  `Users` immutably. `PositionCache` / `BalanceCache` and their `flush_all` / `merge_disjoint` leave the production
  path (kept for the reference path). `add_cum_volume` goes to `UsersMut`. `freeze` fills the typed CFs from the dirty
  sets. `EndResidentJob::run` (:4095) no longer applies the two CFs.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`, then `cargo nextest run --workspace $F`.

**Task 2.2: write-through for cold writers (decision 3)**
- Test first: `cold_raw_write_reaches_users_and_change_set`. A raw `put_cf_raw` / `delete_cf_raw` to positions or
  balances from a writer outside the settle path (lockbox `credit_native`, `lockbox.rs:113`; liquidation / ADL, `torus-core/src/liquidation.rs`, `liquidation_step.rs`; the balance writes in `margin.rs:635-701`; test writers) is visible to the
  next typed read, lands in the dirty set and is emitted once at freeze.
- Code: the overlay's write to a typed CF calls the `ResidentSource` write hook (decode, apply, mark dirty) instead of
  the pending map. Opaque rows stay raw.
- Verify: `cargo nextest run -p torus-state $F`, `cargo nextest run -E 'rdeps(torus-state)' $F`.

**Task 2.3: EVM blocks (decision 4)**
- Test first:
  - `evm_revert_drops_typed_writes`: a lockbox credit inside a reverted frame or tx (`revert_to_checkpoint`,
    `revert_tx`) never reaches `Users`.
  - `evm_then_native_same_key`: an EVM tx credits a balance and a native action in the same block trades on it. Rows
    and `h_n` equal the reference path.
  - EVM suites (`torus-evm`, precompile reader tests) green.
- Code: while an EVM tx scope is open, typed-CF writes stay raw in the journalled pending map. They are folded into
  `Users` (apply + dirty, then removed from pending) after the block's EVM part, before the native batch, and at freeze.
- Verify: `cargo nextest run -p torus-evm $F`, `cargo nextest run -E 'rdeps(torus-state)' $F`.

**Task 2.4: guards and warm == cold**
- Test first: `warm_equals_cold_every_block_{serial,pipelined,...}` (`app.rs:24168-24183`); the R == DB dump test
  `resident_rows_equal_db_scan_after_every_block_serial_and_pipelined` (`app.rs:21853`), with the two CFs read from
  `Users`; Phase 4's guard tests (marker mismatch, skipped height, fatal block); `users_reads_never_reach_the_deferred_part`.
- Code: fatal block invalidates `Users`; a rebuild is cold from DB + layers.
- Verify: `cargo nextest run --workspace $F`.

### Step 3: book rows from a dirty set (Classic)

Codex torus-adversarial review before step 4.

- Test first: `book_dirty_rows_equal_save_order_books`. Random places, cancels, cancel-alls, fills, stops, modifies,
  over every block: the rows from the book dirty set equal `save_order_books` + `diff_stop_rows` output, serial and
  with match workers. Plus `phase5_differential_long_random` in Classic, and modes 2/3 unchanged.
- Code: the place / cancel / fill / stop sites mark (market, order or level) dirty. Per-market workers return their
  dirty sets; E merges them in market order. In Classic, freeze serializes them; `save_order_books` stays as the
  reference path and for modes 2/3.
- Verify: `cargo nextest run -p torus-core $F`, `cargo nextest run -E 'rdeps(torus-bridge)' $F`.

### Step 4 (optional): open-order count in `UserState`

Only if Phase 4 step 5 was dropped or moved (owner s119) and step 0 still shows `open_order_counts` at a size worth it.
Same keep rule as s119: kept only on its own net A/B (counter upkeep netted). Decision 5.
**Moved here (owner s120):** Phase 4 step 5 (`6288ebe8`) had no net gain (section 68: walk saved -2.76 ms per 1k fills,
counter read ~3.7 ms per native block); revisit after step 0, needs a cheaper counter read; drop if still no net gain.

- Test first: `open_order_count_in_user_equals_book_scan` (Phase 4 step 5's test), serial and with match workers.
- Code: count deltas from the book dirty sites of step 3 (shared upkeep); `open_at_start` (:7019, :9762) reads the
  record; a cold rebuild recounts from the books.

### Step 5: crab summary updated inline per fill

- Test first: `inline_sums_equal_account_view_every_fill`. After every fill, at the block's mark version, the
  trader's summary equals `AccountView::build` (`torus-core/src/margin.rs:283`), bit-exact incl. `Err` (rule 8, owner
  Q5). Random marks, moving every block and still. P1 / P3 green.
- Code: the fill applied to a `UserState` updates its versioned summary (one term per fill) when the entry's version
  equals the block's; otherwise the entry stays stale and is recomputed on read, as Phase 4 step 3 does.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`.
- RAM (owner s121): Phase 4 step 6 measured light users (2 positions) at 726 / 819 B per user (100k / 1M, jemalloc,
  with sums) vs the ~0.6 KB target, accepted for Phase 4. The excess is the summary entry, one
  `Box<(u64, SumsResult)>` per user (176 B, class 192) where section 1 budgets ~80 B per position, plus record-table
  slack after a doubling (~100 B per user at 1M). Here, with the summary reworked anyway: keep it only for users that
  traded recently (drop it for idle users, recompute on their next order), and/or store it inline in `UserState`
  instead of boxed. Test first: a dropped summary recomputes bit-exact to `AccountView::build`. Re-measure with
  `ubench_users_rss` (step 7); pass = light <= ~0.6 KB, active and market-maker shapes no worse.

### Step 6 (conditional): typed oracle and market tables

Only if step 0 shows the cold R slots' reads at >= ~1% of exec wall **(estimate)**. Marks and market parameters as typed
tables following their rows (rows stay raw writes; the table follows like Phase 4's `Users`).
- Test first: `typed_marks_equal_oracle_rows_every_block`; P1 with moving marks.

### Step 7: RAM ubench, genesis cell and gate campaign

- Ubench `ubench_users_rss` (Phase 4 step 6), re-run on the in-place record and dirty sets; pass = section 1.
- Genesis cell (archy): 1M accounts / 1M resting orders. RSS per node, restart -> ready (SIGTERM and SIGKILL), replay
  correctness.
- Campaign (archy): Phase 5 head vs Phase 4 head, 300 markets, oracle off and on, interval 15 s, Classic, n = 4 ABBA,
  120 s cells, plus 10-market cells, one 10 min soak pair, SIGKILL multi-crash cells. Allocator: jemalloc compiled in
  (as shipped).
- Final Gate 3 (1.4x): Phase 5 head vs `199bdc97`, the ozarchy-g3s0 recipe (oracle off primary, as shipped secondary,
  LD_PRELOAD-jemalloc arm for `199bdc97`).
- Report: matched/s, native blocks/s, `block_ms`, exec busy, engine ms per 1k fills and its phases, cache flush and
  save_books ms, `end_resident` ms, RSS mean / peak / slope and jemalloc allocated, CPU-s/1M, block time p50 / p99.

## 5. Not building

- Typed nonces / windowed nonce set (D12 b, a consensus change at a fresh genesis), unless decision 2 b.
- Typed staking, governance, sessions, liquidation, lockbox / EVM accounts (cold; decision 1).
- RPC on typed state or the view (validator RPC stays on the DB, Phase 3 D10 c); `getUserLimits` O(1) from
  `UserState` (separate RPC task).
- Own canonical snapshot format (D7 b), unless decision 7 b; LtHash snapshot trust (D8 b).
- Per-trader rows on disk or in the hash (D4 b); L3 (owner Q1, D12 genesis).
- Option D (design section 3.6) and its golden A re-pin: after Phase 5, separate plan.
- Shrinking `L` or the view, unless decision 8 b.
- Book rows from dirty sets in modes 2/3 (Classic on testnet; mode 3 revisit before mainnet).
- The remaining cloid exec-thread sites (0.364 ms per 1k fills at 100% share, section 63.8): not item 6.

## 6. Rollback

No runtime switch (D16). Rollback = redeploy the Phase 4 binary. Nothing is persisted in a new form: rows, `h_n` and
checkpoints are byte-identical, and `Users` and the dirty sets are node-local, rebuilt cold from DB + layers. Nodes can
roll back one at a time; no data migration.

## 7. Decisions (open; owner)

1. **Scope of typed state.**
   (a) Hot only: `Users` + book rows; everything else raw (this plan).
   (b) All native state typed (design text: nonces, sessions, oracle, markets, staking, governance).
   (c) `Users` only, books keep the diff.
   Recommendation: a. (b) adds cold structures that no profile shows on the exec thread; (c) leaves `save_books`
   (13.2 ms per native block in section 59.3) in place.
   **Decided: a** (owner s122).
2. **Nonces (D12 b said "before Phase 5"; not built).**
   (a) Nonce rows unchanged (raw, check reads the overlay); no consensus change.
   (b) D12 b at a fresh genesis before Phase 5 (bundle with L3), then a typed live-nonce set.
   (c) Keep all nonces but hold the consumed set in memory (RAM grows with every action ever; section 4 rejects it).
   Recommendation: a. `replay_guard` was 2.8 ms per native block (section 59.3); D12 stays a genesis item.
   **Decided: a** (owner s119): Phase 5 keeps nonce rows raw, no typed nonce set. D12 b itself is built before
   this phase, after Phase 4 step 6 (see step 0), so the rows Phase 5 sees are already windowed. Rationale for not
   typing them: since s94 execution already drops nonces older than block time - 60 s
   (`check_nonce_window`, `torus-types/src/eip712.rs`), so older nonce rows only grow state; that is a state-size
   item for a long-lived chain, not a Phase 5 throughput item.
3. **Cold writers to the typed CFs.**
   (a) Write-through in the overlay (decode, apply, dirty) for every raw write; only the hot writers move to `UsersMut`.
   (b) Move every writer (lockbox, liquidation, ADL, `margin.rs`'s balance writes) to `UsersMut`.
   Recommendation: a. One hook covers every writer, so rule 5 holds by construction; per-writer moves follow only
   where step 0 shows cost.
   **Decided: a** (owner s122).
4. **EVM blocks.**
   (a) Raw journalled writes during EVM txs, folded into `Users` after the EVM part and at freeze (task 2.3).
   (b) Write-through during EVM txs with a typed undo log mirroring revm frame checkpoints.
   Recommendation: a. EVM blocks are serial and rare (EVM 0.0 ms in the s84 cells); the overlay journal already
   handles reverts.
   **Decided: a** (owner s122).
5. **Open-order counter (if Phase 4 step 5 is not kept).**
   (a) Drop it; keep counting from the books.
   (b) Optional step 4, sharing step 3's dirty sites, kept only on its own net A/B.
   Recommendation: b if Phase 4 step 5 failed only on upkeep cost (step 3 removes part of it); a if step 0 shows
   `open_order_counts` well under 1.45%.
   **Decided: b** (owner s122).
6. **Crab overhead gate ("<= +15% per fill vs the same phase without crab").**
   (a) Proxy: engine ms per 1k fills oracle on vs oracle off, same binary and campaign (Gate 3 already runs both).
   (b) Build a crab-free binary of the Phase 5 head (no longer a clean cut: crab is merged into main).
   (c) Drop the gate; final Gate 3 (1.4x) with the as-shipped arm covers it.
   Recommendation: a.
   **Decided: a** (owner s122).
7. **Own snapshot format (D7 c: "own canonical format with Phase 5").**
   (a) Not in Phase 5; the RocksDB checkpoint stays.
   (b) In Phase 5 as an extra step (sorted canonical file, +1x state RAM while it serializes, section 4).
   Recommendation: a. No gate needs it; it pairs with D8 b and D9 b before public bootstrap.
   **Decided: a** (owner s122).
8. **RAM follow-up (Phase 4 decision 5 b).**
   (a) Not in Phase 5.
   (b) A persistent `Users` map pinned by the view, so the view drops its positions / balances slices.
   Recommendation: a. Only ingress reads the view (RPC reads the DB); judge after the Phase 3 RAM decision and step 7's
   soak.
   **Decided: a** (owner s122).
9. **Scope by the Gate 3 gap (after Gate 3).** Pick the row of section 1's scope table.
   Gate 3 (section 65): `d3cba5e2` / `199bdc97` = 1.931x [1.877, 1.985] oracle off, 1.835x as shipped, so the
   >= 1.36 row applies: Phase 5 is not needed for the gate.
   **Decided (owner s119): build Phase 5 for the gain, not the gate** (~+10-25% matched/s **(estimate)**, the typed
   base for option D). Step 0 sizes what is built: only the items it measures as worth it. Block-time p99 (section
   66) is worked on in parallel by archy and is not a Phase 5 gate beyond "no regression".

## 8. Risks

- Two representations during steps 1-2 (overlay and typed); the long differential is the guard.
- A writer missed by the write-through and the typed API: it would reach the pending map of a typed CF. Guard: in
  tests and debug builds, a pending write to a typed CF outside an EVM scope fails; in release, the fold at freeze
  still emits it.
- Freeze cost moves to the exec thread (serialize once per changed row). Step 1 measures it; a worker over an immutable
  copy is the fallback.
- Matched/s may not follow exec savings in the closed-loop bench (section 54).

## 9. Open: block time (commit interval) p99

Section 61: block time p99 +17.7% [+6.7, +28.7] SAT and +23.1% soak on Phase 3, p50 -32.7% / -13.6%. Exec p99 per
native block does not explain it (590 / 570 ms on s2, section 61; 581-596 / 585 ms on `d3cba5e2`, section 64.3; exec
tail = start burst, epoch-boundary blocks, checkpoint upkeep spikes, no RocksDB stalls). A decomposition is queued for
archy after Gate 3.

**Result (section 66, no new run):** the rise sits in the checkpoint state-write windows. The flush worker writes
~0.9 s (205-255 MB per node); in SAT all 3 validators start within the same second. Excluding window intervals,
Phase 3 is level (SAT) or lower (soak). Inside a window tail about half the time all three HotStuff threads are
silent and view timeouts are 3.5x. Ruled out: exec back-pressure, RocksDB stall counters, CPU. The stalling call is
not found. Hypothesis (unproven): one shared host and one 256 MB `vm.dirty_bytes`, so kernel writeback throttling
stalls any writing thread, consensus included (a single-host devnet artifact in part). Second lead: on `c88d3073`
~34 mempool admission inserts take > 0.5 s (0 on `de1a97c1`); Phase 3 moved that check onto `ReadView`.

**Plan (owner s119).** Phase 5 does not target this; its block time p99 gate stays "no regression" unless the
cause turns out to be on E.
- Archy, when back (archy is paused until Phase 4 is built), on the Phase 4 head, after Phase 4 step 6's campaign:
  (1) off-CPU profile of the HotStuff thread across one checkpoint window; (2) one run with a larger
  `vm.dirty_bytes` (tests the shared-host hypothesis). Then, once the cause is known: (4) spread checkpoint times
  across validators. Skipped: the 15 s vs 60 s interval A/B (it changes the frequency, not the cause).
- Builder: the slow mempool admission inserts on `ReadView` (code-side, not a host artifact): read-only
  investigation first, slotted between Phase 4 steps so it does not run beside a Phase 4 build.
