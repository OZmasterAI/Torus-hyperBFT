# Target design: crab features at near-main speed (target spec + scorecard)

Status: TARGET SPEC, documentation only. Nothing here is implemented. Written s87
(2026-10-04). Use it twice: as input to the item 6 design update, and as the scorecard
the finished work is measured against (after fixes 3/2a/1, B, item 6 and D).

Sources (read-only):
* crab stack `rebase/s85-oracle-feeder` @ `c93c579`; `file:line` references are for
  that commit unless marked otherwise. Design docs on that commit:
  `docs/plans/account-level-margin-f1.md` (F1), `docs/plans/liquidation.md`,
  `docs/parity-audit-fixes-s515.md`.
* perf fixes 3/2a/1: `perf/s87-crab-fixes` (`d0d722b`, `6cbd812`, `473a037`, `4d81f4a`),
  plan `wt/s87-ubench/docs/plans/crab-perf-fixes-s87.md`.
* option B design (s87, owner decisions at its end; session scratchpad, not committed).
* item 6: `docs/plans/market-scaling-in-memory-design.md` (this branch; D1-D16). It is
  not edited here. Section 3 lists what its update has to take from this document.

## 1. Summary (plain language)

The crab stack adds four things to main: account-level cross margin (F1, Hyperliquid
style), oracle mark prices, liquidation, and the s515 parity fixes (reduce-only,
market-order caps, match-time margin checks, withdrawal checks). They work, but the
engine is ~10x slower per fill than main. On the devnet the stack does 1.9k matched/s
against main's 47.7k.

The slowdown is in how the code gets the data, not in the margin rules. Every margin
or liquidation question ("how much free margin does this trader have?") is answered
by reading ALL of the trader's positions from storage and re-adding them, again and
again: once per maker per market per batch, once per sender per batch, and for 2048
traders at the end of every block. Bench traders hold ~250 positions each.

"Same effect, near-main speed" means:
* Same rules: every order, fill, cancel, withdrawal and liquidation result is the same
  as the crab stack's (except where the owner already chose to change a rule: B, D).
* Each trader's margin numbers are kept in memory as a small running summary. A fill
  or an order updates it in O(1). Mark-dependent parts are re-valued only for traders
  that are actually touched, at most once per block.
* Liquidation looks only at traders that can have become unhealthy. Every other trader
  is skipped by a proof that it is still healthy, with the same writes.
* Target cost: the crab features add 10-15% per fill over main (30% is the fail line),
  and devnet matched/s stays within 10% of main with the oracle on.

## 2. Target architecture per feature

### 2.1 Where the time goes today (measured, s87)

| Path | Today (c93c579) | Cost driver | Status after fixes 3/2a/1 |
|---|---|---|---|
| Maker check at match (`maker_fill_fits`, `order_book.rs:485`) | `AccountMargins::load` (`order_book.rs:436`) -> `AccountReader::maker_account` (`native_executor.rs:802`): balance + ALL positions + one oracle read per position, once per (maker, market, batch) | positions per maker x markets per maker | fix 1: once per maker per batch (`BatchMakerAccounts`) |
| Phase-2 sender view (`prepare_one`, `:4524`) | `AccountReader::view` (`:769`) per sender per batch, plus a `position_px` point read per (sender, market) | positions per sender | unchanged: ~60 prefix scans + ~25-35k point reads per block |
| Phase-3 policing (`reduce_only_positions_for`, `:6058`) | a position point read per (policed trader, market) | senders x markets | unchanged |
| Liquidation (`liquidation_pass`, `liquidation_step.rs:56`) | `traders_after` seeks + `liq_view` (`:149`) = all positions + balance for 2048 traders, every block | 2048 x positions | fix 2a: skipped while no listed market has a mark; full cost with marks (~24-25 ms/1k) |
| Overlay prefix scan (`iterate_cf`, `backend.rs:1677`) | linear filter over all pending + parent writes of the CF | writes per block | fix 3: range lookup |
| D2 one-market pool (`:4268-4348`) | a sender's free margin goes only to the market of its first checked taker; elsewhere taker-only | design rule | unchanged: ~17.5k takers per ubench run cancelled, fills/block 6.2k vs 11.3k |

### 2.2 Per-trader margin summary in memory

**Today.** `AccountView` (`margin.rs:117-196`) is never stored. Each reader rebuilds it
with `AccountView::build` from a prefix scan of the trader's positions.

**Target.** One `TraderSummary` per trader with a position or a balance row, node
memory only, derived from consensus state:

| Field | Meaning | Update |
|---|---|---|
| `available`, `order_margin` | mirror of the balance row (collateral = their sum) | O(1) on every balance write: fill PnL, reservation, release, fee, transfer |
| `terms[m]` per position | `(upnl_m, im_m, mm_m, n_m)` of the position in market m at the price it was valued at, plus that price | recomputed O(1) when the position in m changes |
| `upnl`, `position_im`, `maintenance`, `notional` | Σ of `terms` (= `AccountView`'s four sums) | subtract the old term, add the new one: O(1) per fill |
| `marked`, `cross_only` | how many positions sit in marked markets; no Isolated position | O(1) per position change |
| `valued_at` | mark version the terms were computed with | set on re-value |

The summary keeps each position's four values computed at that position's own
position-size tier (`order_initial_margin(tiers(m), n_m)`, `maintenance_margin`), so
tiers are exact by construction. "Σ IM by tier" is then simply `position_im`.
Order margin is the balance row's `order_margin`: placing or cancelling an order moves
value between `available` and `order_margin` (O(1)) and leaves equity unchanged.

**Exactness rule (mandatory).** The summary must return exactly what
`AccountView::build` returns for the same state, to the raw unit, including `Err` on
overflow. Two consequences:
* No algebraic shortcut. An "entry-weighted size" `K = Σ s_m · entry_m` (so that
  `UPnL = Σ s_m · mark_m − K`) does NOT reproduce `build`: `FixedPoint` multiplication
  truncates, so `(mark − entry) · size` and `mark · size − entry · size` can differ by
  1 raw per position, and the marks still differ per market, so it saves nothing. The
  per-position term is kept instead. Changing the formula to the aggregated form would
  be a consensus change (fresh genesis, golden re-pin); not proposed.
* One formula. The per-position term is factored out of `AccountView::build` into one
  function (`position_terms(pos, mark, tiers)`). `build` (cold path, tests) and the
  summary (hot path) both call it. `build` iterates in market order and checks every
  partial sum; a delta-updated sum could avoid an intermediate overflow that `build`
  hits. Rule: a trader with any term above a magnitude guard (e.g. `2^100` raw) is
  routed to `build` on every read. Fixed by test, not by argument.

**Mark-dependent parts, lazily.** Marks change only in `begin_block_oracle`
(`native_executor.rs:7690`), before any action, so they are constant within a block.
A global mark version increments when the block's aggregate changes any usable mark
(or a mark appears or goes stale). On the first read of a trader in a block where
`valued_at` is older, the trader's terms are recomputed: O(positions) in memory,
~30 ns per position. Only touched traders pay it, once per block. Bench shape:
600 senders x ~250 positions = ~4.5 ms per block, ~0.4 ms per 1k fills.

**Consumers (all O(1) after the per-block re-value):**
* Phase-2 placement gate: `free = available + upnl − position_im` (`AccountView::free`).
* Maker start-of-batch snapshot (F1 D8): the summary's `free` replaces
  `AccountMargins::load`'s storage read. Each book still runs its own copy (D8 stays).
* Phase-2 `position_px` and Phase-3 `reduce_only_positions_for`: the position is read
  from the in-memory record (O(log positions)), not from storage.
* Withdrawals (`withdrawal_allowed`, `margin.rs:195`): `transfer_required` from the sums.
* Liquidation: `equity()` and `maintenance` (2.3).

**Freshness within a block.** The summary describes the state at the end of the
previous block (it is updated with the resident rows, 3.1). A trader with writes in
the current block's pending layer (e.g. the post-EVM `execute_batch` call, liquidation
fills, triggered stops) is valued from the overlay (pending -> resident rows), O(its
positions), never from RocksDB. In the normal case (one batch per block) every trader
is clean at batch start.

### 2.3 Event-driven liquidation

**Today.** Rule (C1, D5): each block walks up to 2048 distinct position holders from a
round-robin cursor (`traders_after`, `liquidation.rs:418`), values each with
`liq_view`, classifies (`classify`: Healthy / Stage 1 / Backstop / ADL), acts on up to
64, and writes cooldown, pending, previous-mark and cursor rows only when they change.
Healthy -> `clear_cooldown` + `set_pending(false)`; no marked position -> `set_pending(false)`.

Three levels, each exact or explicitly a rule change:

**L1: same walk, valued from memory (exact, no proof needed).** `traders_after` is
served from the sorted in-memory trader set; `liq_view` from the summary. Same window,
same order, same writes. Cost: O(1) per candidate whose summary is current;
O(positions) for one re-valued after a mark change. Worst case with marks moving every
block: 2048 x 250 x ~30 ns = ~15 ms per block (~1.3 ms/1k fills).

**L2: same walk, provably-healthy candidates skipped (exact if the bound is sound).**
Each valuation that classifies Healthy stores a certificate in node memory:

* `AV0` = equity, `n_m` = position notionals at the marks used, `G = Σ n_m` over marked
  positions, `W = Σ n_m / (2 · lev_min(m))` with `lev_min(m)` = the lowest leverage in
  market m's tier table (production: one flat tier, so `lev_min` = the market's
  leverage), `k` = number of positions, and the mark version.
* After a relative move `ρ` (largest `|mark_now / mark_0 − 1|` over the trader's marked
  markets): `AV ≥ AV0 − ρ·G − 2k` raw (truncation, per position) and
  `MM ≤ (1+ρ)·W + 2k` raw (each position's MM is at most `n / (2·lev_min)` at any tier the
  new notional reaches). So the trader is still Healthy if
  `ρ ≤ ρ* = (AV0 − W − 4k) / (G + W)`, rounded down. `ρ* ≤ 0`: no certificate.
  (The `2k` raw slack per side covers the truncation of `size · mark`, of the UPnL
  product and of the two IM/MM divisions; the exact constant is fixed by the proof in
  P4, rounding always toward "re-check".)
* Invalidated by: any position or balance write of the trader (including writes made
  earlier in the same liquidation pass: stage-1 fills against makers, ADL
  counterparties, the vault), a mark appearing, disappearing or going stale in one of
  its markets (entry-price fallback changes), a margin-config change (listing).
* `ρ` without reading the trader's positions: a per-block table of the largest relative
  move over all listed markets, `D(b)`, and the conservative cumulative bound
  `F(b) = Π (1 + D(b'))` (exact integers, rounded up). Then `ρ ≤ F(now)/F(b0) − 1`.
  Tighter per-market bounds are optional (O(positions)).
* A skipped candidate gets exactly the Healthy writes (`clear_cooldown`,
  `set_pending(false)`); both rows are rare and only deleted if present.
* Same window, cursor, `scanned` / `cut` / `last`, so the same liquidations in the same
  order. Cost: O(1) per candidate, ~0.1-0.5 ms per block.
* The certificate only ever says "Healthy". If it is sound, the outcome cannot differ.
  If it is unsound, a warm node skips what a cold node liquidates: a fork. Hence the
  proof obligations in section 4.

**L3: Hyperliquid-style rule "every account, every block" (consensus change, owner
decision).** Today's window finds an unhealthy account only when the cursor reaches
it: up to `accounts / 2048` blocks late (100k accounts: ~49 blocks; 1M: ~490). HL's
docs do not say how often or which accounts are checked; from outside it shows no such
delay (inferred, not documented). L3 defines the rule as SCAN = ∞: every account
is classified every block, the unhealthy ones are acted on in address order from the
cursor, up to 64, carry-over as today. The implementation never values all accounts:
it keeps a node-local index of certificates keyed by trigger level `F(b0)·(1+ρ*)` in a
`BTreeMap`. Each block pops the triggered entries plus the dirty traders, values only
those, and uses the cooldown and pending rows (few) for the Healthy writes. The index
is not consensus state, so C1 ("no stored index") still holds. Because the rule is
"all accounts", the reference test is a brute-force valuation of every account. It
changes timing and order of liquidations, so it needs a fresh genesis and a golden B
re-pin. Recommended to bundle with item 6 D12 (nonce window), which needs a fresh
genesis anyway.

**Recommendation.** Phase 1 builds L1. L2 is added in Phase 1 only if the oracle-on
liquidation tail misses its target (1.0 ms/1k, section 5). L3 is a separate owner
decision.

### 2.4 Cross-market margin in parallel matching

**Today (F1 D2, D8).** Phase 3 runs one worker per market. A sender's free margin is an
exclusive pool of the market of its first checked taker. In its other markets a
checked taker's budget is its own reservation (`insert_taker_only`). A non-pool GTC
sell that meets a better bid needs more IM than it reserved at its limit, so it gets
0 fills and is Cancelled (~19% of orders in ubench_econ). Makers use a start-of-batch
snapshot per book.

**B (now, owner decided s87): B-N + taker +1-raw allowance.** Batch path only,
match-checked sells in a non-pool market reserve at `max(base, B0)`, where `B0` is
that market's best bid at the start of Phase 2. Hold price, resting row and releases
stay at the limit (A5 telescoping unchanged). A taker-only budget gets the makers' +1
raw rounding allowance. Pool market, single path, buys, PostOnly, reduce-only, stops
and liquidation orders are unchanged. Speed cost: one `best_bid()` per market per batch
and one map lookup per order. Expected `rejected_cancelled`: ~19% -> <= 1% (upper bound
~0.2%; first measure how many exhausted takers hit bids placed earlier in the same
batch). Golden A re-pinned in B's commit, golden B unchanged.

**D (later, after item 6 Phase 4/5): sequential second pass.** After the parallel
pass, the takers cut by margin exhaustion in a non-pool market are collected in flat
batch order. Once the parallel fills have updated the summaries, they are re-run one by
one against the account-level free margin (the summary's `free`, O(1)), on the books
before they are saved. Deterministic (serial, canonical order) and HL-like (account
margin). The cost is proportional to the cut takers (<= 1% after B). It changes
outcomes: golden A re-pin. It needs O(1) account free margin and in-memory books and
users, hence after Phase 4/5. Residuals it covers: bids placed earlier in the same
batch above `B0`, market buys filling above the mark in non-pool markets.

### 2.5 Mark aggregation (already cheap)

`aggregate_oracle_prices` (`:7725`) runs once per block: per listed market, the
reporter rows, 3xMAD cut and stake-weighted median, O(markets x reporters). Target:
no change, except that its output is the per-block mark table (a `Vec` indexed by
market) that all readers use. That replaces fix 1's `BatchMarks` (lazy `OnceLock`
per market) and the per-position oracle reads in `AccountReader::mark`. It also
supplies the mark version and `D(b)` for 2.3. Target cost < 0.5 ms per block at
300 markets.

### 2.6 Parity checks (cheap)

Reduce-only policing, market-order cap, closing-is-free, position-tier IM at match,
maker `marginCanceled`, ModifyOrder gate and withdrawal checks are O(1) or O(log n)
arithmetic per order or fill once positions and the summary are in memory. The only
storage cost today is the position point reads in `reduce_only_positions_for` and
Phase 2. Both go to the in-memory record. No rule changes.

### 2.7 Consensus state vs node memory

| Data | Where | Notes |
|---|---|---|
| positions, balances (`available`, `order_margin`), books, oracle rows, market rows / margin configs, `CF_NATIVE_LIQUIDATION` (cooldown, previous mark, cursor, pending) | consensus state (hashed) | unchanged; no new CF, no new row type |
| resident position and balance rows (item 6 Phase 1 R), sorted trader set | node memory, complete copy | value-neutral cache of the state |
| `TraderSummary` (2.2), mark table and version, `D(b)` / `F(b)` | node memory, derived | rebuilt from the state on any guard miss or restart |
| liquidation certificates (L2) and trigger index (L3) | node memory, derived | used only to skip provably-healthy valuations |
| B's `B0` table, D's cut-taker list | per batch, stack | computed from consensus state at batch start |

Fork risks:
* A derived structure that is not value-neutral (summary != `build`, unsound
  certificate, a missed write path) makes a warm node decide differently from a cold
  one. The running state hash and attestations catch it after the fact (AGREE fails);
  they do not prevent it. Mitigation: section 4, and every write to positions or
  balances goes through the one place that updates R and the summary (positions are
  written only via `PositionManager::put_position` / `delete_position`, liquidation F12).
* `HashMap` iteration must never reach a decision or a write: the walk uses the sorted
  trader set, L3 pops a `BTreeMap`, ties by address.
* Storing certificates in state (a tag `0x07` row) would remove the warm/cold risk but
  adds a hashed write per checked account per block and reverses C1. Not recommended.

## 3. Mapping onto item 6 phases, and what gets deleted

Build order per item 6 D1: 1 -> 3 -> 4 -> 2 -> 5 (owner numbering), each phase a branch
in the linear stack (D16), benched against main and the previous phase.

| Step | Crab work | Deletes (no duplicate implementation may remain) |
|---|---|---|
| now: perf fixes 3/2a/1 (done) | overlay range lookup, no-mark liquidation skip, one maker snapshot per batch | none |
| now: B (+ taker +1 raw) | 2.4 | none |
| interim 2b | only if the oracle-on devnet misses the 90% bar before Phase 1 lands | (would be replaced by Phase 1) |
| item 6 Phase 1 | R for positions + balances, built so it can ALSO serve ordered prefix iteration (`positions_for_trader`, `traders_after`); `TraderSummary` side map updated when R is updated (O(changed rows)); per-block mark table; summary consumers (2.2); liquidation L1 (+ L2 if needed) | `AccountReader::view` / `maker_account` / `maker_free` / `maker_position_px` storage reads; fix 1 `BatchMakerAccounts` and `BatchMarks`; `AccountMargins::load`'s source read (O(1) from the summary); `liq_view`'s reads and fix 2a's `liq_view_if_marked` (subsumed); `traders_after` RocksDB seeks; storage reads in `reduce_only_positions_for` and Phase-2 `position_px`; withdrawal view reads |
| item 6 Phase 3 | none; the summary has no persistent form, so crash replay from a checkpoint rebuilds R and the summary cold | none |
| item 6 Phase 4 | `UserState` holds the summary fields and per-position terms next to the positions | the side map; R's raw position rows on the hot path |
| item 6 Phase 2 | persistent worker pool also used by match workers (no crab change) | none |
| item 6 Phase 5 | executor mutates `UserState` directly; summary updated inline per fill | overlay reads on the margin/liquidation path |
| D | sequential second pass (2.4) | D2 pool logic stays (D builds on it) unless the owner later replaces D2 by D entirely |
| L3 (if chosen) | trigger index, rule SCAN = ∞ | the round-robin window code (cursor stays for ACT carry-over) |

Stays: `AccountView::build` as the cold path and test reference, sharing
`position_terms` with the summary; fix 3 (generic read path, now off the hot path);
the ADL counterparty scan (`ADL_MAX_SCAN_ROWS`, rare, not hot).

**Gaps the item 6 update must cover:**
1. Item 6 Phase 1 says R is point-lookup only and "iteration (`positions_for_trader`,
   RPC only) keeps going to the DB". On the crab stack, iteration is on the hot path
   (F1 views, liquidation walk). R must serve ordered prefix and seek iteration (a
   `BTreeMap` layer, or a per-trader index next to the hash map), with the overlay's
   pending/parent layers merged on top (fix 3's range merge).
2. Phase 1 adds the summary and liquidation L1/L2. Phase 4's gain line ("O(positions of
   one user) cross-margin and liquidation checks") becomes "summary moves into the
   record". The main crab gain lands in Phase 1.
3. Phase 1's differential tests extend to summary == `build` and L2 == full scan.
4. Item 6's bench recipe has no oracle. Crab cells need the price feeder (marks on),
   otherwise fix 2a hides the liquidation cost.
5. D and L3 are scheduled after Phase 4/5. L3 and D12 share a fresh genesis.

## 4. Determinism and proof obligations

Every derived structure is tested against its from-scratch definition. Comparisons go
through test-only constructors, not runtime flags (D16).

| # | Obligation | Test |
|---|---|---|
| P1 | summary == `AccountView::build` | after every block of long seeded sequences (open, increase, partial close, flip, full close; balance-only writes; marks fresh / stale / absent / reappearing; flat and multi-tier configs; a listing mid-run; negative `available`; Isolated and overflow-sized positions): for every trader all four sums, `equity`, `free`, `transfer_required`, and the `Err` cases identical |
| P2 | consumers unchanged | golden digests A and B (perf branch `perf_equivalence_golden.rs`) unchanged by the summary commits, serial and engine-forced; all F1 / parity suites (`account_margin_tests`, `maker_margin_release_tests`, `market_order_margin_tests`, `reduce_only_tests`, `engine_parallel_tests`) green |
| P3 | L1/L2 == full scan | the same block sequences run with the storage-valued walk (reference) and with L1, then L2: identical `CF_NATIVE_LIQUIDATION` rows, positions, balances, results, metrics and `h_n` per block, including cursor cuts, cooldown, stage-1 chunks, backstop, ADL of a trader and of the vault, mark-on/off transitions |
| P4 | certificate soundness | property test: random accounts and random mark paths (including tier edges, `lev_min` < `lev`, many tiny positions for the `k` term): whenever the certificate allows a skip, the full valuation is Healthy. Plus a targeted case: a stage-1 fill earlier in the pass pushes a maker later in the window under MM (its certificate must be invalidated) |
| P5 | warm == cold | 3 replicas, one restarted every K blocks (summary and certificates rebuilt cold), another with summary invalidated every block: identical state and `h_n` |
| P6 | crash replay | kill at random points (mid-block, between R/summary update and flush, mid-checkpoint after Phase 3): restarted node identical, state and every `h_n`; `liquidation_determinism_serial_pipelined_and_replay_are_identical` and chaos `liquidation_step_keeps_incremental_root_equal_to_full_scan` green |
| P7 | guard misses | height mismatch, skipped height, fatal block: R and summary dropped and rebuilt, never used stale |
| P8 | B, D | option-B tests 1-17 (B design section 7); D: serial == engine at threads 1/2/4/8, crash replay, golden A re-pinned in D's commit with counters before/after |
| P9 | L3 (if chosen) | reference = brute-force valuation of every account every block; identical liquidations and rows |

## 5. Scorecard

Fill in the empty cells at the end. "Main" = `d995f68` unless stated. ubench =
`ubench_econ` (600 senders, `UB_MARKETS` markets, median of 3, ms per 1k fills;
interleaved A/B on the shared box; main numbers from the same generator applied on
main). Devnet = same harness and `bench-throughput` as cell `s87-crab-main-r0`.

**Why +10-15% per fill, with +30% as the fail line.** The crab work that remains per
fill is arithmetic that HL also does: the taker's match-time margin search, the maker
check, reduce-only policing and one summary term update per fill (estimated +0.5-1.0
ms/1k on margin, match and settle each). Then once per touched trader per block, the
mark re-value (~0.4 ms/1k in the bench shape), and the liquidation tail (<= 1.0 ms/1k
with L1, ~0.05 with L2). Sum: ~+2-4 ms/1k on 12.8, i.e. +15-30%. The owner's devnet bar
(within ~10% of main matched/s, oracle on) is tighter. The engine is ~75% of E's block
wall (item 6 table 1.5), so a per-fill engine overhead `x` costs ~`0.75·x` matched/s:
<= 10% needs `x` <= ~15%. Hence target <= +15%, fail > +30%.

### 5.1 ubench_econ, ms per 1k fills

| Row | 30 mkts | 100 mkts | 300 mkts | 300 mkts, marks |
|---|---|---|---|---|
| main d995f68 | (measure) | (measure) | 12.8 | n/a (no oracle on main; compare to 12.8) |
| crab today c93c579 | ~3.9x main | ~6.1x main | 135-161 | 153-155 |
| after fixes 3/2a/1 (prelim, noisy) | | | 40.9 | 76.4 |
| TARGET final | <= 1.15x main | <= 1.15x main | <= 14.7 | <= 14.7 |
| FAIL above | 1.3x | 1.3x | 16.6 | 16.6 |
| measured final | | | | |

### 5.2 Per phase, 300 markets, ms per 1k fills

| Row | margin | match | settle | liquidation tail | fills/block |
|---|---|---|---|---|---|
| main d995f68 | 0.8 | 1.0 | 8.4 | ~0 | 11.3k |
| crab today c93c579 | 16 | 75 | ~18 (remainder of 160.7) | 52 | (measure) |
| after fixes 3/2a/1, no marks (prelim) | 9.9 | 13.6 | 11.1 | 1.2 | 6.2k |
| after fixes 3/2a/1, marks (prelim) | 11.3 | 18.0 | 13.1 | 25.7 | (measure) |
| TARGET final (marks) | <= 1.5 | <= 2.0 | <= 9.5 | <= 1.0 (L1), <= 0.1 (L2) | >= 10.7k (95% of main) |
| measured final | | | | | |

Notes: per-1k-fill numbers after fixes are inflated ~1.8x by the lower fills per block
(per block: crab ~255 ms vs main ~145 ms). Settle rises with fills per block once B
restores them; per-1k it should stay near main.

### 5.3 Margin outcome and devnet

| Metric | main | today c93c579 | after 3/2a/1 (prelim) | after B (target) | final (target) | measured |
|---|---|---|---|---|---|---|
| rejected_cancelled share | (measure) | ~19% | ~19% (17.5k takers/run) | <= 1% | ~0% beyond real exhaustion (after D) | |
| devnet matched/s, oracle on | 47.7k (oracle off; rerun with feeder) | 1.9k | (measure) | >= 0.9x main | >= item 6 targets (below) | |
| devnet drain time | 85 s | never drained | (measure) | <= 1.1x main | <= 1.1x main-at-same-phase | |
| node CPU-s per 1M matched | (measure) | (not measured) | (measure) | <= 1.15x main | <= 1.15x main-at-same-phase | |
| devnet engine split (val0) | match 35 s, tail < 12 s | match 454 s, tail 496 s | (measure) | | | |
| AGREE / PASS | yes | yes | | yes | yes | |

### 5.4 Item 6 targets (its own doc, s84 4-arm recipe, 300 markets uniform)

| Phase | item 6 target vs previous | crab-specific target |
|---|---|---|
| 1 | +15-30% matched/s, -8..-12% CPU-s/1M | margin + liquidation tail at target (5.2) with marks; P1-P7 green |
| 3 | ~0 throughput, ~-10% node CPU | warm == cold after checkpoint replay |
| 4 | +0-10% | summary in `UserState`; side map deleted |
| 2 | +7-15% | none |
| 5 | up to +60-90% vs main | crab overhead still <= +15% vs the same phase without crab |
| D | | rejected_cancelled ~0%; per-1k cost unchanged within noise |

"main-at-same-phase" = the item 6 phase branch without the crab commits, built from the
same stack, in the same campaign (D15: same-campaign cells only).

## 6. Open questions for the owner

1. **Liquidation rule.** Keep today's window and make it fast (L1, plus L2 if needed;
   no consensus change), or move to L3 ("every account, every block", HL-like, no
   stored index, fresh genesis, golden B re-pin)? L3 fixes detection latency growing
   with account count (1M accounts: ~490 blocks per sweep). If L3, bundle it with the
   D12 genesis?
2. **L2 at all?** Build L1 first and add L2 only if the oracle-on tail exceeds 1.0 ms
   per 1k fills? (Recommended: yes, measure first.)
3. **Item 6 Phase 1 scope.** Accept that R must also serve ordered prefix iteration,
   and that the summary and liquidation L1 land in Phase 1 (not Phase 4)?
4. **Targets.** Confirm +15% per fill as the target (derived from the 90% devnet bar)
   and +30% as the fail line, and that comparisons after item 6 are against the same
   phase without crab, not against today's main.
5. **Exactness.** Confirm the summary keeps per-position rounding (bit-exact with
   `AccountView::build`) rather than switching to an aggregated formula at a fresh
   genesis.
6. **D vs D2.** After B is measured: if the in-batch share is small, is D still wanted,
   and should D eventually replace the D2 pool (one account budget, serial fallback)
   or only complement it?
7. **Interim 2b.** If the oracle-on devnet misses the 90% bar before Phase 1, build a
   stopgap (L2 on the storage path) or wait for Phase 1?
8. **Missing baselines.** Measure main at 30 and 100 markets, main's
   `rejected_cancelled`, and CPU-s per 1M for both arms in the next campaign?

### 6.1 Owner decisions (s88, 2026-10-04)

| Q | Decision |
|---|---|
| 1 | L1: keep today's window (2048 valued / 64 acted per block) and serve it from memory; no consensus change in Phase 1. L3 stays a separate later decision, bundled with the D12 fresh genesis, before position holders approach 2048 (below that the window already covers every account every block). |
| 2 | Build L1 first; add L2 only if the oracle-on liquidation tail exceeds 1.0 ms per 1k fills. |
| 3 | Yes: R also serves ordered prefix and seek iteration, and the summary plus liquidation L1 land in Phase 1, not Phase 4. |
| 5 | Bit-exact: the summary keeps per-position rounding; `AccountView::build` is the P1 reference. |
| 4, 6, 7, 8 | Open; decided after measurements (Gate 1 review, B measured, oracle-on devnet). |

## Stage gates (owner, s87, PROPOSED)

All against main d995f68 on the devnet, oracle on for the stack. Each gate also needs
AGREE/PASS, a normal drain, and the differential tests passing.

| Gate | When | Proposed target |
|---|---|---|
| 1 (merge) | after fixes 3/2a/1 + B | >= 0.6x main matched/s; ubench_econ <= 25 ms per 1k fills |
| 2 | after item 6 Phase 1 + margin summary + event-driven liquidation | >= 0.9x main (crab cost <= +15% per fill, fail above +30%) |
| 3 (final) | after all item 6 phases | >= 1.4x today's main at 300 markets |

These are guidance, not automatic rules. After the Gate 1 measurements the owner reviews
the results and decides whether to merge, and whether Gates 2 and 3 should change.
