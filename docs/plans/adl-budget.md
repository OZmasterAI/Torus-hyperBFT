# ADL per-block budget (P0 before testnet)

**Status (s18, ozarchy):** design **decided** (owner 18c s96). **s99 (owner):** W = 100,000,
holder units, B charged into W, cheap `has_key`: §12 (as built, s26). Q1-Q4 are the recommendations in
section 7. **Q5 (freeze) is dropped and Q6 = P2 (terms fixed at B, ADL escrow)**, plus the new
previous-mark rule **H**; see section 8, which supersedes sections 4 and 5 where they differ.
Implementation plan: `docs/plans/adl-budget-impl.md`. Branch
`perf/adl-budget` off main aae6b9b (profiling test b60bdff). It changes who gets ADL'd, so it
needs a fresh genesis (fine pre-testnet). Liquidation design: `docs/plans/liquidation.md`.

## 1. Problem (measured)

Liquidation-stress cell S=750 (ozarchy, bench/liq-stress af8529e, N=4 b900, 300 markets, 100
thin accounts with positions in ~270 markets each, `docs/perf/` section 23):

* all 100 thin losers classify **ADL** (AV ≈ 1M − 0.075 × 20M ≈ −500k < 0) in heights 788-790;
* 26,778 ADL steps (one per account-market), 27,410 counterparty closes (K ≈ 1 per step);
* liquidation step **332 s / 123 s / 241 s** per block, median 23-27 ms per step, rising
  ~1.4× within a block; consensus height frozen 787-789 for **~11.6 min** (liveness FAIL);
  state AGREE, vault deficit 26,516,805.13 on every node.

S=400 (same cell, AV/MM ≈ 0.40 after the shock): all 100 BACKSTOP (0 stage 1, 0 ADL), step
98 / 46 / 115 ms, vault +19.98M, pass. 18c's hypothesis is confirmed: stage 1 is the
`[2/3 MM, MM)` band; a 400 bp shock on ~20M notional against 1M collateral moves AV/MM from
~2.0 to ~0.40, past the band (a shock of 251-333 bp would land in it). Per-account AV/MM is
derived (the logs print counts only); the mean (199,850 AV) is measured from the vault.

## 2. Root cause (profiled)

`adl_account` (`liquidation_step.rs:488`) runs, per marked position of the ADL'd account,
`liq::adl_candidates` (`liquidation.rs:344`): a paged walk of `CF_NATIVE_POSITIONS` **from the
empty key** until `ADL_MAX_SCAN_ROWS` = 65,536 rows, then a filter on `key[20..28] == m`. The
CF is keyed `trader ‖ market`, so every step copies, merges and drops the same first 65,536 rows
whatever the market.

Microbench `crates/torus-bridge/tests/ubench_adl.rs` (ignored; real `run_liquidations` through
`NativeStateOverlay` with R attached; quiet machine):

| traders N (×300 positions) | rows | window | bankrupt K | ms / step (median) |
|---|---|---|---|---|
| 100 | 30,540 | whole CF | 1 | 5.4 |
| 250 | 75,540 | capped | 1 | 11.7 |
| 1000 | 300,540 | capped | 1 | 13.8 |
| 5000 | 1,500,540 | capped | 1 | 12.0 |
| 5000 | 1,502,970 | capped | 10 | 16.4 (13.5 → 17.3 first → last account) |
| 5000, no R | 1,500,540 | capped | 1 | 37.9 |

perf (N=5000, K=10): `adl_candidates` = **97.7 %** of `run_liquidations`
(`iterate_cf_from`/`merge_from` ~35 % self, `memcmp` ~28 % from pending/parent map lookups per
row, malloc/free ~25 % from two `Vec`s per row, `memcpy` ~6 %). Ranking AV (`pos_sums`) 0.4 %,
`adl_rest` 0.04 ms, `adl_close`/`apply_fill` ≤ 0.1 %. The within-block rise is the pending map
growing with every ADL transfer (each row's merge lookup gets slower). The rig's ~26 ms/step
(~0.40 µs/row) is the same mechanism on a host running 3 nodes.

Two further defects in the same code:

* **Fairness (correctness).** The window always starts at the lowest key, so with > 65,536 rows
  the counterparties come only from the lowest-address traders (~200 at 300 positions each),
  not "every opposite-side position in m" as `liquidation.md` *ADL* specifies (H3 accepted the
  window as a limitation; at bench scale it is the common case, not the rare one).
* **No bound on ADL work.** `LIQ_ACT_PER_BLOCK` = 64 counts **accounts**; one ADL'd account is
  one step per marked position (~270 here) and nothing bounds work inside it. Even at 0.1 ms
  per step, 64 accounts × 300 markets = 1.9 s in one block.

So the fix has two independent parts: **(A) make a step cheap and correct** (candidate source)
and **(B) bound ADL work per block** (budget + carry-over queue). Either alone is not enough:
(A) alone still lets one block do unbounded work; (B) alone at ~15-26 ms per step allows only
~1-2 steps per block (26,778 steps ⇒ hours of ADL).

## 3. Part A — candidate source (Q1)

Target semantics (restores `liquidation.md`): candidates of `m` = **every** opposite-side
position in `m` (the ADL'd account excluded), ranked as today (`adl_rank`, exact, ties by
address). `ADL_MAX_SCAN_ROWS` goes. OI symmetry then guarantees one step fully closes the
position (Σ opposite size ≥ size), so there is no partial-close retry.

| Option | How | Cost per ranking of m | Format / consensus | Notes |
|---|---|---|---|---|
| **C1 — point reads over the trader set (recommended now)** | for each trader of the E2 sorted set (`TraderPositions::traders_after(None, ∞)`, block-written traders merged), `get_position(t, m)` via the records (clean traders) / overlay (dirty) | O(A) lookups (~0.1 µs with records) + AV for holders | none | Smallest change; reuses item 6 E2/C7 with the existing shadow check. Fallback without records (tests / tools): the same loop over `liq::traders_after` + overlay reads (slow, correct). Scales with A, not with holders of m. |
| C2 — node-local market → traders index | kept in `TraderPositions` next to `traders`, updated from each block's `ResidentDelta` at `end_resident`; merged with the block's dirty traders | O(holders of m) | none (node-local, derived) | Same as C1 when every trader holds every market (the bench); wins for thin markets at 100k accounts. More code (another maintained set + tests). |
| C3 — consensus index rows `market ‖ trader` | written on every position open / close / flip | O(holders of m), also on cold nodes | new root-hashed rows (fresh genesis; fine pre-testnet), extra write on the order hot path | Reverses C1-decision (s517 "no separate index"); needs a throughput A/B. Only if node-local is ever not enough. |

Estimate at bench scale (A = 5000, ~2,500 opposite holders per market): C1 ≈ 0.5 ms of lookups
+ ~2,500 AV evaluations (~0.5 µs each from the microbench's ranking share) + one sort ≈ **2 ms
per ranking**, i.e. ~10× cheaper than today, and correct. 27k rankings would still be ~54 s
of work in total, hence Q2 (rank once per block and market) and the budget (Part B).

### Q2 — ranking per step or per (block, market)?

> Final (section 8): once per **(block, market, side)**, from a per-block cache with a per-key
> position.

* **Per step** (today's semantics): every ADL'd account-market re-ranks m. Exact "current
  state" ranking; cost = rankings × holders.
* **Per (block, market) (recommended):** the first ADL step in m in a block ranks m once; later
  steps in m in the same block walk the same ranked list from where the previous one stopped,
  re-reading each candidate's current position (`adl_close` already skips a vanished / flipped
  candidate and caps at the current size). Deterministic; a counterparty's AV used for ranking
  is the one at the block's first ADL in m. S=750 cost: ~270 rankings per block instead of
  ~12,600 (≈ 0.5 s → bounded by the budget anyway).

## 4. Part B — budget and carry-over queue

> **Superseded in part by section 8 (final, s96).** The queue holds obligation rows
> (`0x07 ‖ height ‖ market ‖ side ‖ trader`), not accounts. The account is flat at B, so there
> is no freeze and no re-classification. W is sized so an HL-sized event closes in one block.
> The queue gauge counts rows. This section is kept for the reasoning behind the options.

### Budget (Q3)

`ADL_WORK_PER_BLOCK` = W work units, deterministic counts only:

* **(a) units = candidates examined + closes (recommended):** a ranking of m costs the traders
  it looked at (C1: A; C2: holders of m), a cached step costs its closes. Tracks CPU; a step
  starts only while `used < W` and is atomic, so a block overshoots by at most one step (one
  ranking of one market).
* (b) units = account-market steps (18c's first suggestion): simplest to explain; with Q2 a
  step's cost differs ~1000× between a ranking step and a cached one, so W must be set for the
  worst case.
* (c) units = closes: does not see the ranking cost (the dominant term). Not recommended.

W is set from the prototype's measured ns per unit so that ADL adds ≤ ~20 ms to a block on the
rig (normal exec ~18 ms/block); the number goes into this doc with the measurement.

### Queue (Q4) — in `CF_NATIVE_LIQUIDATION` (root tag 6, already hashed; no new CF)

* `0x07 ‖ height(8, BE) ‖ trader(20)` → `[1]`: account under ADL, **FIFO by the height it was
  classified ADL, then address**. `0x08 ‖ trader` → height (membership / delete).
  (Alternative: `0x07 ‖ trader` only = address order; simpler, but a high address can wait
  behind later bankruptcies.)
* Per block, the liquidation step:
  1. the regular pass as today (scan 2048 / act 64, cursor); an account classified ADL gets its
     orders and stops cancelled (D4) and is **enqueued**, not processed in the pass;
  2. drains the queue in key order under W: per queued account, **re-classify first** — if
     AV ≥ 0 now (marks moved) it leaves the queue (the regular pass handles it next time as
     backstop / stage 1 / healthy); else its marked positions are closed in ascending market
     (closed positions vanish, so resuming needs no stored market) until W runs out;
  3. an account with no marked position left: flat-deficit / remaining collateral to the vault
     (D9, unchanged) and dequeue;
  4. the vault (D8): enqueued like any account when its AV < 0 (today it is ADL'd at the end of
     every pass without bound).
* `liquidation_due` also checks the `0x07` prefix (the queue keeps the step due, like the M2
  pending row).

### What a queued account may do meanwhile (Q5)

* **(a) frozen (recommended):** every signed action of a queued account is rejected
  (orders incl. reduce-only, cancels, withdrawals, transfers, leverage changes) with a
  `liquidating` result; deposits / incoming transfers are credited (they only reduce the
  deficit). Cost: one membership read per action only in blocks whose queue is non-empty
  (`prefix_exists(0x07)` once per block → flag), so zero cost normally.
* (b) not frozen, rely on margin gates: AV < 0 should fail opening orders and withdrawals (to
  verify per action before choosing this), but **reduce-only orders have no margin gate**
  (`liquidation.md` stage 1), so the account could
  close positions into the book at worse prices between ADL steps; harmless for conservation
  (the loss still lands in the vault) but it races the queue and is harder to reason about.

### Vault deficit in between (Q6)

* Queued positions stay with the bankrupt account and are marked every block; its negative AV is
  the **unrealized** deficit. The vault's balance (the realized deficit) changes only when the
  account goes flat (D9), at the ADL prices of each step (previous mark clamped to the
  bankruptcy price at that step, H1/D10 unchanged — no price snapshot at enqueue).
* Counterparties keep their positions (and PnL) until their market is reached; who is closed
  depends on the ranking at that block, not at the bankruptcy block.
* Exposure grows with drain time = total work / W. New node-local gauges:
  `torus_liquidation_adl_queue` (accounts) and `torus_liquidation_adl_queue_deficit`
  (Σ −AV of queued accounts, computed only with metrics).
* Alternative considered, **not recommended:** at ADL classification move every marked position
  + collateral to the vault at the mark (backstop-style, cheap) and then ADL the vault under the
  budget. Resolves the account at once (no freeze), but ADL then prices against the vault's
  bankruptcy price and closes the vault's healthy backstop inventory too — a larger semantic
  change from HL (HL ADLs the user's positions).

## 5. Tests first (before code)

Unit / integration (torus-core `liquidation_tests.rs`, bridge `liquidation_l1_tests.rs` style):

1. Candidates = every opposite-side holder: with > 65,536 position rows, the top-ranked
   counterparty at a **high** address is closed first (fails today).
2. C1 shadow: records path == overlay walk for candidate lists (incl. traders the block wrote).
3. Budget: K ADL'd accounts whose work > W → the step stops at W (± one step), the queue holds
   the rest in FIFO order, the next block resumes exactly there; two runs → identical state root.
4. Re-classification: a queued account whose AV recovers ≥ 0 leaves the queue and is not ADL'd.
5. Freeze (if Q5 a): queued account's order / cancel / withdraw rejected with `liquidating`;
   deposit credited; no check cost when the queue is empty.
6. Invariants over a multi-block drain: OI symmetry per market after every block, value
   conservation, vault ends with the summed deficit, ADL'd accounts end at exactly 0.
7. Vault in the queue: vault AV < 0 → enqueued, drained under W.
8. `liquidation_due` true while the queue is non-empty.
9. Backstop / stage-1 paths byte-identical (regression: existing liquidation tests unchanged).
10. `ubench_adl`: per-step and per-block ADL time below the W target at N = 5000.

## 6. Proof

* `ubench_adl` before / after (ms per step, ms per block at W).
* Liquidation-stress cell **S=750**: liveness PASS (no commit gap above the normal cadence
  bound), liquidation step ≤ the W target every block, queue drains to 0, AGREE, vault deficit
  identical on all nodes, number of blocks to drain reported.
* **S=400 unchanged:** 100 BACKSTOP, vault +19.98M (19,984,975.69), step times in the same range.
* Harness fixes from s17 first (liq_stress.py `_count` KeyError, stale test genesis in the
  worktree, reflink-seeded stale binaries).

Added by the owner (s96): the S=750 **drain time in blocks** until the queue is fully empty, and
**conservation of total balances** (Σ available + order margin + UPnL at the mark over all
accounts incl. the vault) across the whole drain.

## 7. Decisions (owner 18c s96: all = the recommendation)

| # | Question | Decision |
|---|---|---|
| Q1 | Candidate source | C1 now (no format change); C2 if a 100k-account sweep shows ranking dominates; C3 only as a later consensus item |
| Q2 | Ranking refresh | once per (block, market, **side**), candidates re-read at close (per-block cache with a per-key position) |
| Q3 | Budget unit | rows visited + candidates examined + candidates read at close (+ edge rows); one constant W, sized so an **HL-sized event closes the escrows in its own block** (supersedes the earlier "≤ ~20 ms") |
| Q4 | Queue order | FIFO obligation rows `0x07 ‖ height ‖ market ‖ side ‖ trader` → (size, price) in `CF_NATIVE_LIQUIDATION` (section 8) |
| Q5 | Queued account actions | **dropped** (s96 final): the account is flat at B, nothing to freeze |
| Q6 | Deficit in between | **P2** (s96 final): terms fixed at B, positions to two ADL escrows, deficit to the vault at B (section 8) |
| H | Previous mark (D10 change) | the last mark **different** from the current one, per market (section 8) |
| 5 | Funding | escrow positions excluded (section 8; doc-only, no funding on main) |

## 8. Final design (owner 18c s96): P2 escrow, rule H, funding

**P2: terms fixed at B.** In the block B where an account classifies ADL (or the vault's
AV < 0), the pass does the following:
1. D4: cancel the account's orders and stops.
2. For each marked position, in ascending market: compute the ADL price exactly as H1 does today
   (the previous mark, rule H below, clamped one-sided to the account's bankruptcy price:
   `liq::adl_price`, `bankruptcy_price`, `adl_rest`, unchanged), then transfer the position to
   an **ADL escrow** at that price.
3. D9: the account's remaining collateral moves to the vault. Under the one-sided clamp it is
   **never positive**: each clamped close keeps AV at the mark ≤ 0, inductively; the ceil
   rounding goes against the trader; and the `b ≤ 0` mark fallback would need AV ≥ mark × size
   > 0, which is impossible for an ADL-classified account. So the vault only ever receives a
   deficit (≤ 0). The account ends flat at exactly 0 and leaves liquidation in B.

The escrows:
* There are two protocol accounts with no key: `ADL_ESCROW_LONG` takes bankrupt longs and
  `ADL_ESCROW_SHORT` takes bankrupt shorts, so obligations of opposite sides at different
  prices never net.
* Each holds one aggregated position per (market, side).
* They are never classified, have no margin checks, and are never ADL candidates. The vault
  stays a candidate.

The queue:
* Each obligation is a row `0x07 ‖ height(8) ‖ market(8) ‖ side(1) ‖ trader(20)` →
  (size, price). The trader stays in the key because the clamp makes prices per account.
* The drain runs in key order under W, with one ranking per (block, market, side). The escrow
  closes against the ranked opposite holders at the obligation's stored price.
* When the real holders are exhausted (both escrows in one market), the two escrows close
  against each other, each at its own stored price, and the vault pays the difference.
* A flat escrow's remaining balance (dust from averaged entries) is swept to the vault and
  reported separately.
* Node-local gauges (metrics only):
  * `torus_liquidation_adl_queue`: obligation **rows**, not accounts;
  * `torus_liquidation_adl_queue_deficit`: the escrows' balance + UPnL at the mark;
  * escrow notional, swept dust, and edge-pairing amounts.

Sections 4-5 above describe the earlier account queue (Q4-Q6 before P2). Where they differ from
this section, this section holds: there is no account queue, no freeze and no re-classification,
and the queue gauge counts rows.

W is one constant (630,000, measured in §9; **100,000 since owner s99, §12**), high enough that an HL-sized event (a few hundred
account-markets) closes the escrows in its own block. Only S=750-type storms spill over several blocks. HL's Oct 10 2025 ADL
(raw node_fills) closed every (account, coin) fully in one block.

**H: the previous mark.** Row `0x03 ‖ market` holds `last(16) ‖ [prev(16)]`. When the step's mark
differs from `last`, the row becomes (`last` = mark, `prev` = old `last`). An unchanged mark
leaves the row as it is, and a market without a usable mark deletes it (as today). The
pre-clamp ADL price is `last` when the mark changed this step, else `prev`; with neither, it is
the mark (D10's fallback). So every account of a market gets the same pre-clamp price
regardless of which block of one mark interval the scan reaches it in. That holds within one
mark interval only: at 100k accounts the 2,048-per-block scan spans several mark changes
(follow-up: C2 market index).

**S=750 deficit (26,516,805.13) explained.** The shock lands at 788.
* Block 788's batch (47 accounts) used a previous mark from before the shock. The clamp to the
  bankruptcy price then bounds each loss, so their deficits are ~0.
* Blocks 789 and 790 (53 accounts) used a previous mark that was already after the shock (D10
  stored 788's mark), so the clamp did not bind. That gives 26,516,805 (the model gives
  26,488,513; the 28k residual is walk PnL).
* The vault gauge went 0 → 9.99M → 26.5M.

Under H, all 100 accounts get the pre-shock base (no mark change between 788 and 790), so the
deficit is the clamped ~0 for all of them.

**Funding (owner decision 5).** Main has no funding mechanism yet (only `max_funding_rate_bps`
params). The future funding item **must exclude the positions of `ADL_ESCROW_LONG` /
`ADL_ESCROW_SHORT`**: they neither pay nor receive, otherwise an escrow could not end at 0.
Counterparties keep paying and receiving funding until their obligation is drained. This is a
small difference from HL that only shows during a storm (HL parity backlog item).

## 9. Budget measurement (A8, measured)

`crates/torus-bridge/tests/ubench_adl.rs` (ignored; real `run_liquidations_with` through
`NativeStateOverlay` with R attached, scan 2,048 / act 64 / `UB_ADL_WORK`). ozarchy, quiet
(load < 1.5), release with `-C force-frame-pointers=yes`, 3 runs each, at W = 630,000. Setup:
N = 5,000 traders × 300 markets (1.5M position rows), bankrupt accounts long size 1 (one side).
*B ms* = step start → the last `liquidation: ADL to escrow` event (classification + transfers);
*step ms* = the `liquidation step` line's `ms`; ns/unit = (step − B − the healthy-scan baseline)
/ `adl_work`.

*Before* = d0dfed7 (a trader set and AV per ranking); *after* = the drain caches (one trader
set per drain, the ranking AV memoized per trader; bit-identical, same units per block).

| case | block | transfers | rows closed | units | B ms | step ms before (3 runs) | ns/unit before | step ms after (3 runs) | ns/unit after |
|---|---|---|---|---|---|---|---|---|---|
| HL: 3 accounts × 100 markets | B | 300 | 300 | 500,800 | 1.0 | 532 / 537 / 540 | 1,059-1,075 (median 1,068) | 396 / 399 / 405 | 787-804 (median 792) |
| S=750-like: 100 × 270 | B₁ (64 accounts) | 17,280 | 7,745 | 630,126 | 65-68 | 12,303 / 12,305 / 12,344 | ~19,450 | 715 / 722 / 727 | ~1,040 |
| | B₂ (36 accounts) | 9,720 | 7,808 | 630,862 | 35-37 | 4,193 / 4,266 / 4,352 | ~6,700 | 571 / 571 / 575 | ~850 |
| | drain 3 | 0 | 5,184 | 630,616 | — | 502 / 537 / 538 | ~830 | 399 / 400 / 402 | ~635 |
| | drain 4 | 0 | 4,464 | 634,178 | — | 480 / 505 / 507 | ~780 | 386 / 387 / 390 | ~610 |
| | drain 5 | 0 | 1,799 | 253,698 | — | 178 / 179 / 180 | ~700 | 138 / 139 / 141 | ~550 |

* **W = 630,000** = max(1.25 × U_hl, 100 × (5,000 + 3) + 300 × 2) rounded up to 10,000, with
  U_hl = 500,800 (100 rankings × 5,002 traders + 300 rows × 2). The HL event closes the escrows
  in block B (asserted by the HL mode); S=750-like drains in 5 blocks (B spans 2 at act 64).
  Before (b60bdff, §2): 27,000 steps × 12-16 ms ≈ 6-7 min of step time.
* **Owner condition (block B outside W, ~250 ms): block B's own work holds** (1 ms HL, 67 ms
  for 64 accounts × 270 markets). **The W-block does not:** an HL event's block costs ~540 ms
  here (~1 s at the rig factor 1.9-2), and a block with B's transfers *and* a drain costs up to
  12.3 s for the same units (19 µs/unit vs 0.8).
* Why (perf): in the HL block ~70 % of the step is the ranking AV (`reader.view` / `pos_sums`
  per candidate, ~2,500 per (market, side), 100 rankings). In B₁, 83 % is `liq_traders_after`
  run once per ranking: `layer_keys(CF_NATIVE_POSITIONS)` (the block's ~17k pending position
  keys) plus a `has_key` overlay seek per dirty trader, × 270 rankings. Units count traders
  examined, not the block's pending keys, so W does not bound that cost.
* **After the caches** (`DrainCache`, `liquidation_step.rs`; proof: the lib test
  `adl_drain_caches_are_bit_identical`, the seeded L1 test, the goldens unchanged): the HL block
  is ~400 ms, still above ~250 ms; a B + drain block is 0.57-0.72 s (was 4.3-12.3 s); a full-W
  drain block ~0.4 s. Remaining profile of the HL drain (perf, inclusive, partly nested):
  ~73 % under `get_position` (each ranking point-reads all 5,002 traders' position in its
  market: the overlay's dirty-range check, the records hash lookup, a binary search), ~29 %
  `pos_sums` (AV builds: first sight of each candidate and after each close), ~20 %
  `adl_rank`'s sort. W unchanged; the ~250 ms question is open (owner).
* Still open (owner): charge B's transfers into W, or a lower per-block act limit for ADL
  accounts; a per-market holder list (C2) instead of reading every trader per ranking.

Commands (worktree root, `CARGO_TARGET_DIR=~/.cargo-target-adl-budget`,
`RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"`,
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`; `UB_ADL_WORK` overrides W):

    UB_ADL_HL=1 UB_ADL_TRADERS=5000 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture
    UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture

## 10. C2 and the s96 fix list (as built, s24 ozarchy, after 18c s96 / s99)

**C2: node-local per-market holder list (decided s96, replaces C1's walk per ranking).**
* `TraderPositions` (item 6 C7 records, `trader_positions.rs`) keeps `holders`: per market `m`,
  every trader with the 28-byte key `t ‖ m` in R (regular or opaque), ascending. It has the
  records' lifecycle: built with R at load (`build`), followed per key from each block's
  `ResidentDelta` at `end_resident` (`apply`: a write adds, a tombstone removes; O(log n) per
  changed key). Never consensus-visible, no format change.
* Block-dirty merge: `dirty_by_market(state)` groups the block's own pending 28-byte keys
  (`layer_keys`, writes and tombstones) by market; `holders_with(m, dirty[m])` merges them with
  R's holders of `m`. The drain takes the dirty map once, at its first ranking (with the trader
  set, `DrainCache.dirty`): the drain gives no trader a new key (the same argument that keeps
  the cached trader set exact), so the list stays a superset of `m`'s holders for the whole
  drain. A listed trader without a position in `m` (a deleted key) is skipped by the position
  read, exactly as C1 skips a trader of the whole set that does not hold `m`.
* `adl_candidates_of` ranks the holder list when the records are attached; without records
  (no R: tests, tools) it is C1. (**Superseded by §12, owner s99:** a ranking now charges the
  holders of its market.) **Units are unchanged:** a ranking still charges the whole
  trader set's size (C1's units), so the drain's per-block progress, its state and every golden
  are the same; only the work behind a unit drops to O(holders of `m`). (Charging holders
  instead would be a consensus change of the drain's progress; not done.)
* Shadow (as E2): in tests with `shadow` on, every C2 ranking also runs C1 over the whole set
  with the same reads and valuation and records any difference (candidates, order). Proof:
  `trader_positions_tests::holder_lists_with_the_dirty_traders_cover_the_walk` (random blocks,
  irregular rows, new / vanishing traders: the merged list is ascending, unique, and holds
  exactly the walk's traders holding `m` plus only deleted dirty keys; the warm `holders` equal
  a cold build after every block), `liquidation_l1_tests::adl_c2_holder_lists_are_bit_identical_to_c1`
  (HL-like and S=750-like shapes, caches on and off: rows, units, results block by block; holder
  lists used and shorter than the set), the seeded L1 test (every node-path ranking from a
  holder list, shadow-clean), the goldens unchanged (R modes Inline / Worker use C2, Off uses C1).
* Where it pays: markets held by a fraction of the accounts (thin markets at 100k accounts).
  In `ubench_adl`'s HL shape every trader holds every market, so the ranking reads the same
  5,002 positions as C1 there; memory ~1 BTreeSet entry per position row (~30-50 bytes).

**Fix list (18c review of 6a25e20, s96; s99 additions).**
* (b) `adl_queue_telemetry` no longer counts the whole queue every metrics block: a running
  count on the Metrics instance (`liquidation_adl_queue_rows_cache`) = last count + rows the
  step wrote (B) − rows it deleted (drain, pairing); an empty queue is one seek and resets it to
  0; a count is taken only on a start, after a read error / failed step, or when the running
  count says 0 while rows exist. Test `telemetry_counts_the_adl_queue_without_rescanning_it`.
  Project review LOW 1: a drain that ends at the queue's end has seen every remaining row and
  stores their exact number (the rows that waited for a mark), so an overcount corrects itself
  with no extra read. Test `telemetry_adl_queue_count_self_corrects_at_the_queues_end`.
* (c) The proof-only value sum values every position at one common price per market (0:
  UPnL = −signed size × entry); while OI is symmetric that equals Σ UPnL at the marks. It no
  longer jumps when a market loses or regains its mark (s750vs: −1,740.69 in the step the feed's
  marks went stale). Test `value_sum_does_not_jump_when_a_market_loses_its_mark`. 18c s99: the
  value-sum tests (that one and `telemetry_reports_the_adl_queue_escrow_and_value_sum`) also
  assert net signed size 0 per market over every holder (traders, both escrows, the vault) at
  each checked block (`net_size_per_market`), so the price-0 sum cannot hide an unbalanced
  market.
* (d) `liquidation_e2e_adl_drain_survives_a_restart` closes RocksDB (drops the context and the
  StateDb), opens the directory again, configures the running-hash activation as boot does, and
  compares `liquidation_adl_work_total` per block (plus dumps and roots from block 9).
* (e) `adl_drain_caches_are_bit_identical` also compares the run with R (E2 set, C2 holder
  lists) against the run without R (walk, C1), block by block.
* (f) `an_hl_shaped_event_costs_exactly_the_sizing_formula` (default suite): the HL shape at
  N = 20 (every trader short in all 100 markets, 3 bankrupt longs, 300 rows) costs exactly
  U(N) = 100 × (N + 2) + 300 × 2 units; W = U(N) closes in B, W = U(N) − 2 leaves the last row.
* (g) Delisted markets rank at the stored price of the first row of their (market, side) the
  block visits (once per block and key; each row still closes at its own price): documented at
  the drain. Mainnet risk below.
* (h) The two "escrow dust" lines of s750 (h803: long +0.00132290, short −0.00216415) and
  s750vs (h620: +0.00147665 / −0.00221684) are the sweep itself, not dust left behind:
  `adl_drain` logs `liquidation: ADL escrow dust to the vault` after `move_collateral` has moved
  the flat escrow's whole balance to the vault. End state on all nodes: `adl_queue` 0, escrow
  notional 0, `adl_queue_deficit` 0.0 (both escrows' balance + UPnL), vault deficit = −(sum of
  the two lines) exactly (0.00084125 / 0.00074019), i.e. the vault holds exactly the dust and
  D9 summed to 0. The dust itself is `apply_fill`'s truncated weighted-average entry on the
  aggregated escrow positions (exact arithmetic gives 0; within the *Dust bound*, far below the
  1-token alarm). No behaviour change; pinned by `p2_a_two_sided_storm_sweeps_both_escrows_to_zero`
  (B over three blocks with act 2, L1 + S1 at block 2, L2 + S2 at 3, L3 at 4, interleaved with
  the drain at W = 1 from block 3, both escrows dusty: each swept once, 0
  positions and (0, 0) at the end, the vault = the dust, the dust gauge = the vault).
* Value-sum checks (18c s99): judge by the exact i128 sum in the `liquidation: value sum` log
  line, not the f64 gauge. The sum drifts ~−0.2 units of 1e-8 per trade fill (s750vs; likely the
  truncating `total_cost / new_size` in `position.rs` `fill_transition`): on 18c's backlog (a test
  showing it, then exact conservation). Until then a check across fills allows ~fills × 1 unit;
  checks with no fills in between (ADL, escrows, stale marks) stay exact.

**Mainnet risk (g): a historical OI imbalance halts every node.** The drain treats a row still
open after the ranked holders and the escrow pairing as a broken invariant (escrow size = Σ rows,
OI symmetric) and sets `fatal_error` (review M1, "ADL obligation left open ..."). That is
deterministic, so every node stops at the same block: a chain halt, not a fork. Any state whose
open interest is not symmetric in some market — a past bug, a migration, a manual state edit,
a genesis with one-sided positions — turns the first ADL storm in that market into a halt.
Before mainnet: an OI-symmetry check per market at genesis / upgrade (and ideally a node-local
invariant gauge), and a decision whether an unpairable remainder should go to the vault with an
error line instead of halting.

## 11. Budget measurement on C2 (ozarchy s24, measured; W unchanged until the owner picks option 1 or 2)

`crates/torus-bridge/tests/ubench_adl.rs` on perf/adl-budget @ **3139c36f** (= 497121e5 C2 + the bench
knob `UB_ADL_HOLDERS_PCT`; tests-only change, no consensus-visible change, W unchanged). Method as
§9: release, `CARGO_TARGET_DIR=~/.cargo-target-adl-budget`, `RUSTFLAGS="-C link-arg=-fuse-ld=mold
-C force-frame-pointers=yes"`, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`. Every case started
at 1-min load < 1.5 (the 5/15-min averages were still 2-6, decaying after a nextest run and the
build). 3 runs each, run as systemd units `bench-adl-c2{,b,c}`. B ms, step ms and ns/unit are as in §9.
The 250 ms target is applied to **rig-equivalent ms = ozarchy ms × 1.9-2** (§9 factor). The
ozarchy-ms reading is given beside it.

**Thin markets (`UB_ADL_HOLDERS_PCT=p`, default 100 = unchanged).** Traders come in pairs (2i, 2i+1).
A pair holds market m iff splitmix64(i, m) mod 100 < p, so each market has ~p % of the traders as
holders, with net size 0 (one long, one short). Bankrupt accounts and the sink are as in HL mode. At
p = 10 every trader still holds ~30 markets, so the **trader set (and the units) are the same as at
p = 100**. Only C2's work per ranking drops, to ~10 % of the set.

### 11.1 At W = 630,000 (today)

| case | block | transfers | rows closed | units | B ms | step ms (3 runs) | ns/unit | §9 "after" step ms |
|---|---|---|---|---|---|---|---|---|
| 1 HL, N = 5,000, all hold all (1.5M rows) | B | 300 | 300 | 500,800 | 1.0 | 408.5 / 407.2 / 410.8 | 809-816 | 396 / 399 / 405 |
| 2 HL, N = 5,000, 10 % holders (150k rows) | B | 300 | 300 | 500,800 | 0.9-1.0 | 58.8 / 60.9 / 59.3 | 113-118 | — |
| 3 HL, N = 100,000, 10 % holders (3.0M rows) | B | 300 | 19 | 700,052 | 1.0 | 143.5 / 139.6 / 144.3 | 196-203 | — |
| | drains 2-16 | 0 | 18 each | 700,050 | — | 107.9-122.1 | 152-170 | — |
| | drain 17 | 0 | 11 | 400,030 | — | 64.0 / 69.0 / 64.7 | 156-169 | — |
| 4 S=750-like: 100 × 270 | B₁ (64 accounts) | 17,280 | 7,745 | 630,126 | 65.6-67.4 | 739.2 / 738.2 / 786.4 | 1,065-1,144 | 715 / 722 / 727 |
| | B₂ (36 accounts) | 9,720 | 7,808 | 630,862 | 35.8-36.7 | 583.1 / 592.9 / 588.3 | 865-880 | 571 / 571 / 575 |
| | drain 3 | 0 | 5,184 | 630,616 | — | 412.3 / 412.5 / 411.4 | 650-652 | 399 / 400 / 402 |
| | drain 4 | 0 | 4,464 | 634,178 | — | 398.0 / 399.7 / 393.4 | 619-629 | 386 / 387 / 390 |
| | drain 5 | 0 | 1,799 | 253,698 | — | 144.3 / 143.9 / 144.8 | 562-566 | 138 / 139 / 141 |

* Case 3: 17 blocks, 11,600,832 units in total (U = 10,000,800). At N = 100k one ranking charges
  100,002 units, so a block overshoots W by up to one ranking (7 rankings = 700,050). A market whose
  3 rows span a block boundary is ranked again in the next block.
* **C2 on the all-hold shapes (cases 1, 4): no saving.** It is +1-3 % against §9 "after" (the
  `holders_with` merge, ~2 % in the profile, or day-to-day noise). Units and rows per block are
  identical to §9 (630,126 / 630,862 / 630,616 / 634,178 / 253,698).
* **C2 at 10 % holders: ~7× less time per unit** (114 vs 811 ns/unit at N = 5,000), because a ranking
  now reads ~500 holders, not 5,002. At N = 100k a ranking reads ~10,000 holders: 150-200 ns/unit.

### 11.2 Option (1): full, HL-sized W (W = max(1.25 × U_hl, 100 × (N + 3) + 600), rounded up to 10,000)

| shape | U_hl (units) | HL-sized W | blocks | HL block, ozarchy ms (3 runs) | rig-equivalent | ≤ 250 ms? |
|---|---|---|---|---|---|---|
| N = 5,000, all hold all | 500,800 | 630,000 | 1 | 408.5 / 407.2 / 410.8 | ~775-820 | no |
| N = 5,000, 10 % holders | 500,800 | 630,000 | 1 | 58.8 / 60.9 / 59.3 | ~112-122 | **yes** |
| N = 100,000, 10 % holders | 10,000,800 | 12,510,000 | 1 | 1,540.7 / 1,545.6 / 1,549.3 | ~2.9-3.1 s | no |
| same, W = U exactly | 10,000,800 | 10,000,800 | 1 | 1,530.5 / 1,510.1 / 1,572.6 | ~2.9-3.1 s | no |

U_hl = 100 rankings × trader-set size + 300 rows × 2. C2 does not change units, so the HL-sized W
grows with the account count N. C2's work grows only with the holders per market, which is still
10,000 at 100k accounts and 10 %, twice the 5,002 of case 1.

### 11.3 Option (2): lower W (all measured with `UB_ADL_WORK`, 3 runs each)

Worst block / (later full-W blocks; the S=750-like column also gives B₂ and the drain range), ozarchy ms. The worst block is block B (the block with B's own writes) except at N = 100k, W = 100k, where a later block reached 29.8 ms against B's 28.4.

| W | HL all-hold N = 5k: blocks / worst (later) | HL 10 % N = 5k | HL 10 % N = 100k | S=750-like: blocks / worst B₁ (B₂; drains) |
|---|---|---|---|---|
| 630,000 | 1 / 407-411 | 1 / 59-61 | 17 / 140-144 (108-122) | 5 / 738-786 (583-593; 144-412) |
| 400,000 | 2 / 336-340 (61-62) | 2 / 49-51 (11) | — | — |
| 300,000 | 2 / 259-264 (123-124) | 2 / 38-42 (18-20) | 50 / 66-68 (32-54) | 10 / 456-469 (304-319; 40-194) |
| 200,000 | 3 / 190-195 (121-124) | 3 / 27-28 (18) | 100 / 47-48 (17-43) | 14 / 373-385 (230-236; 108-127) |
| 150,000 | 4 / 152-154 (89-92) | — | — | — |
| 100,000 | 6 / 115-117 (57-61) | 6 / 18-19 (9-11) | 300 / 28-30 (14-30) | 29 / 290-293 (152-156; 24-61) |
| 50,000 | — | — | — | 60 / 245-254 (112-118; 27-34) |

* Units of the whole HL event grow as W falls, because rankings repeat across blocks: N = 5k
  500,800 → 525,810 at W = 100k. N = 100k: 10.0M → 11.6M (630k), 14.9M (300k), 19.9M (200k),
  30.0M (100k). At N = 100k and W ≤ ~200k, one ranking (100,002 units) is a whole block's budget:
  W = 100k closes **one row per block** (300 blocks); W = 200k closes one market per block (100 blocks).
* Block B costs ~2× a later block at the same W, even with only 300 transfers (all-hold, W = 100k:
  116 vs 58 ms; N = 100k: 28 vs 19 ms).
* **S=750-like B₁ is not bounded by W**: it is 245-254 ms even at W = 50,000. From the profile
  (B₁ at W = 200,000, 367 ms of samples) roughly: B's transfers `adl_to_escrow` ~15 % (~55 ms; B ms
  66), the drain's trader set `liq_traders_after` → `TraderPositions::traders_after` 26 % (~96 ms;
  almost all `has_key`, an overlay `iterate_cf_from` seek per dirty trader, here the 64 accounts whose
  270 keys the block just deleted; < 0.1 % in a plain drain block), and the first-sight AV `pos_sums`
  10 % (~38 ms). This is §9's open item (charge B's work into W, or a lower act limit for ADL
  accounts). The `has_key` seek cost is node-local and could be cut without a consensus change.

### 11.4 Profiles (perf, 4,999 Hz, `--call-graph fp`, samples under `adl_drain`, inclusive, partly nested)

* **Case 1, HL all-hold** (1,884 samples ≈ 377 ms): `adl_candidates_of` 81 %; `get_position` 73 %
  (`resident_positions` 39 %: the records' `binary_search_by_key` 31 %; the overlay dirty-range check
  `layer_touches` 32 %); `adl_rank` 19 % (its sort 10 %); `holders_with` 2.1 %; `pos_sums` 1.1 %.
  This is §9's profile: C2 reads the same 5,002 positions per ranking.
* **Case 2, HL 10 %** (263 samples ≈ 53 ms): `adl_candidates_of` 59 %; `get_position` 43 %
  (`resident_positions` 26 %, `layer_touches` 24 %, binary search 12 %); `adl_rank` 37 % (sort 33 %);
  `pos_sums` 8.7 % (`dirty_sums` 4.2 %); `holders_with` 1.9 %. The sort is now a third of the block.
* **Case 4, B₁ at W = 200k** (1,835 samples ≈ 367 ms): `adl_drain` 82 %; `adl_candidates_of` 72 %
  (`get_position` 38 %, `traders_after` 26 %, `pos_sums` 10 %); `adl_to_escrow` 15 %; `adl_rank` 7 %.
  A plain drain block (h = 6, 116 ms) looks like case 1: `get_position` 62 %, `adl_rank` 20 %.

### 11.5 Reading (for the owner; 18c s96 rule)

* **Rule: is the HL-sized block ≤ ~250 ms on C2 at the realistic ~10 % holders shape?**
  * **N = 5,000: yes.** 59-61 ms on ozarchy, ~112-122 ms rig-equivalent. W = 630,000 stays.
  * **N = 100,000: no.** The HL-sized W is 12,510,000 and its block takes 1.51-1.57 s on ozarchy
    (~3 s on the rig). C2's 7× gain per unit is eaten by the 20× unit count, because units still
    charge the whole trader set.
  * The all-hold shape (case 1) and the S=750-like B₁ fail at any N (408 / 738-786 ms on ozarchy).
* **Option (2), the W that keeps the worst measured block ≤ ~250 ms rig-equivalent (≤ ~125-130 ms
  on ozarchy): W = 100,000.** Worst blocks: all-hold 115-117 ms (~220-235 rig), 10 % N = 5k 18-19 ms,
  10 % N = 100k 28-30 ms. The HL event then takes 6 blocks at N = 5,000 (either shape) and 300 blocks at
  N = 100,000, one row per block. If the 250 ms is read as ozarchy ms, W = 200,000 suffices for every
  HL shape (worst 190-195 ms; 3 blocks at N = 5k, 100 blocks at N = 100k).
* **No W keeps the S=750-like B₁ ≤ 250 ms** (245-254 ms on ozarchy even at W = 50,000, ~470-510 rig).
  That needs B's work counted or capped, or the `traders_after` `has_key` seeks made cheap.
* Measured C2 ns/unit by shape: 114 (10 %, 5k), ~155-200 (10 %, 100k), ~570-810 (all hold, 5k),
  ~550-650 (S=750-like drains), 1,065-1,144 (S=750-like B₁). Units track the trader set, not the
  work, so a single W fits one shape at a time.

Commands (worktree root; the cargo form, the runs used the built binary
`~/.cargo-target-adl-budget/release/deps/ubench_adl-5626de9bfbcc1f30 --ignored --nocapture` with the
same env):

    UB_ADL_HL=1 UB_ADL_TRADERS=5000 [UB_ADL_WORK=W] cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture
    UB_ADL_HL=1 UB_ADL_TRADERS=5000 UB_ADL_HOLDERS_PCT=10 [UB_ADL_WORK=W] cargo test ... (case 2)
    UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10 UB_ADL_WORK=W cargo test ... (case 3)
    UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270 [UB_ADL_WORK=W] cargo test ... (case 4)
    perf record -F 4999 --call-graph fp -o perf-<case>.data -- <binary> --ignored --nocapture

Raw logs: `~/bench-results-matched/ubench-adl-c2/` (`<case>-w<W>.r<n>.log`, `campaign{,2,3}.log`
with load and step ms per run, `campaign{,2,3}.sh`, `perf-c{1,2,4}.data`).
Setup: case 1 1,500,400 rows (6.2 s), case 2 150,126 (0.6 s), case 3 3,003,738 (13 s), case 4
1,527,270 (6.3 s). RAM was not a limit (62 GB host, ~53 GB available).

## 12. s99 owner decisions: W = 100,000, holder units, B charged into W, cheap `has_key` (as built, s26 ozarchy)

Commits: `6134998c` (decision 4, `has_key`) and `f7b191fd` (decisions 1-3, the units). Proof:
`cargo nextest run --workspace` 3,001 / 3,001 passed (0 flaky), doc tests 1 / 0.

Owner 18c s99, after the §11 numbers. There is no live chain and this merge needs a fresh devnet
genesis, so consensus changes are allowed. The decisions:

1. **W = 100,000** (option 2, §11.5).
2. **A ranking charges the holders of the ranked market**, not the trader set. The count is derived
   from consensus state, so every validator charges the same. A test checks it against a state
   walk at every ranking, and the C1 shadow also compares the count.
3. **Block B's own work is charged into W**: its escrow transfers, T units per position moved.
   The first-sight valuations are drain units (12.1), not B's. B stays atomic: when its work alone reaches W, the drain gets
   nothing in that block and continues in the next. **The per-block act limit is not lowered**
   (`LIQ_ACT_PER_BLOCK` = 64 for every class).
4. **The `traders_after` → `has_key` seeks are made cheap** (node-local, the same trader set).

This supersedes §10's "units are unchanged" (C2 still lists the same candidates; only the
charge changed) and §8's / §9's W = 630,000.

### 12.1 Units (consensus)

A block's ADL units, in order (`liquidation_step.rs`):

| work | units | code |
|---|---|---|
| B: each position the pass moves to an ADL escrow (an ADL'd account's, or the vault's under D8) | T = `ADL_TRANSFER_UNITS` = 6 | `liquidation_pass` (`b_work`) |
| drain: each obligation row visited (a waiting row too) | 1 | `adl_drain` |
| drain: the ranking of a (market, side) key, once per block at the key's first row | H(m) | `adl_candidates_of` |
| drain: each candidate valued for the first time in the block's drain | 1 | `DrainCache.seen` |
| drain: each candidate `adl_close` reads | 1 | `adl_close` |
| drain: each row the escrow-pairing scan reads | 1 | `adl_cross` |

**H(m), the holders of m:** the number of traders t, other than `ADL_ESCROW_LONG` /
`ADL_ESCROW_SHORT`, with a live position row at key `t ‖ m` in the block's overlay at ranking time,
on either side. "Ranking time" is after B's transfers and after the drain's earlier closes in the
block. The details:
* The two escrows are out, because `adl_candidates` never reads them (they are never candidates).
* The vault (`LIQUIDATOR_VAULT`, which is also the liquidator) is in, because it is an ordinary
  candidate.
* An account ADL'd in this block is flat in m (its escrow took the position), so it is out.
* An account waiting for B behind the act limit still holds m, so it is in.

How H(m) is counted: the ranking reads `get_position(t, m)` for each listed trader, and H is the
number of reads that return a row. Both lists hold every holder exactly once and read the same
overlay:
* C1 (no R: tests, tools; or C2 off in tests) lists every trader of the set.
* C2 lists R's holders of m merged with the block's dirty traders of m. That is a superset of the
  holders: a dirty key the block deleted reads no row and is not counted.

So both give the exact live count, the same on every node, with or without R. The trader set no
longer enters the units. With a holder list the drain does not take the set at all; only the test
shadow does.

**What is checked only in tests (18c s99 review):** C1 == C2 (same candidates, same count) and
"count == a state walk" are `#[cfg(test)]` assertions. A release node runs C2 alone and does not
re-check it, so it relies on R being correct (R's own equality with the state is also shadow-checked
in tests only). A wrong R gives a wrong candidate list and a wrong count on that node.

**The C1 path charges H(m) but reads every trader.** Without R the ranking reads `get_position`
for all N traders of the set and charges only the H(m) that return a row. At N = 100,000 with 10 %
holders that is ~10× the charged work. This is kept on purpose: the units are consensus, so a node
without R must charge what a node with R charges; charging N there would split the units by node.
Production always has R attached; C1 runs only in tests and tools.

**Outside W (not charged):**
* The cold `build_sums` (R's build when the records are first attached or rebuilt).
* `dirty_by_market`, taken once at the drain's first ranking (one pass over the block's pending
  position keys).
* Stale-listed reads: C2's dirty entries that read no row (keys the block deleted), and on the C1
  path every non-holder read.
* `liq_view`: B's classification of each scanned trader and the vault's view (bounded by the
  scan / act limits; T's calibration includes it only for accounts moved to an escrow).

**Valuations:** `seen` is the set of traders the block's drain valued for a ranking. It is never
reduced within the block and is kept with the caches on and off, so the units do not depend on the
caches. A re-valuation after a close (the AV cache drops the trader) costs no unit, because the
close's read unit covers it.

**Budget flow:**
1. `used` starts at B's units.
2. If `used >= W`, nothing drains this block. The rows wait, `liquidation_due` keeps the step due,
   and `liquidation_adl_work_total` shows B's units.
3. Else the drain runs rows while `used < W`. A row is atomic, so it overshoots by at most one row.
4. B is never cut. A block's units are at most max(B, W − 1 + the cost of one row).

**T = 6, calibrated from the measurements.** B costs 3.3-3.8 µs per transfer (§9 / §11: the HL
block 1.0 ms for 300 transfers, the S=750-like B₁ ~66 ms for 17,280, classification included). A
drain unit costs 0.55-0.8 µs (§11.1 drains, all-hold shapes). So a transfer is 4.1-6.9 units,
rounded up to 6. A first-sight valuation (`pos_sums` of a clean trader, ~0.8 µs in §11.4 case 1)
is 1 unit.

### 12.2 Formulas

**HL event:** 3 accounts × 100 markets, one side, 300 rows of size 1, each row closed by one
candidate read. U = 300 T + Σ_{m = 1..100} H(m) + V + 300 × 2, where V is the number of distinct
opposite-side candidates over the 100 markets (each valued once in B). The event closes in block B
iff W ≥ U − 1: the last row starts at U − 2 and costs 2.

| shape | H(m) | V | U |
|---|---|---|---|
| bridge test fixture (`an_hl_shaped_event_costs_exactly_the_sizing_formula`: N traders short in every market, a long sink) | N + 1 | N | 300 T + 100 (N + 1) + N + 600 (N = 20: 4,520) |
| `ubench_adl` HL, all hold (traders alternate sides, a short sink) | N + 1 | N + 1 | 1,800 + 100 (N + 1) + (N + 1) + 600 (N = 5,000: 507,501) |
| `ubench_adl` HL, holder fraction p | ≈ pN + 1 | the distinct shorts over the 100 markets, + 1 | computed exactly by the bench (it prints `U`) |

With holders charged, a ranking costs ≈ pN + 1 instead of N + 3 (§11), so the units follow C2's
work at any holder fraction (§11.5's "units track the trader set, not the work" is gone). The W
floor test (`adl_work_per_block_is_option_2_and_covers_a_thin_hl_event`, core) pins W = 100,000 and
W ≥ 300 T + 100 × (500 + 3) + (5,000 + 3) + 600 = 57,703. That is the HL event at N = 5,000 with
10 % holders, which closes in B.

### 12.3 Expected units for the re-measure (W = 100,000)

Computed with a small model of these rules: each row closes against the top-ranked short sink,
which holds k per market. The model reproduces U(20) of the ubench shape exactly (1,800 + 2,100 +
21 + 600 = 4,521). The bench itself prints `adl_work` per block, and
in HL mode it asserts "closes in block B" iff W ≥ U, with U computed from the setup.

| case | blocks with ADL work | block B | later blocks | total units |
|---|---|---|---|---|
| HL, N = 5,000, all hold | 6 | 101,930 (1,800 transfers + 55 rows) | 100,128 × 4 (54 rows each), last 55,069 (29 rows) | 557,511 |
| HL, N = 5,000, 10 % holders | 1 | 57,283 (all 300 rows) | — | 57,283 |
| HL, N = 100,000, 10 % holders | 17 | 102,763 (19 rows) | 100,064-113,593 (18-21 rows), last 44,933 (8 rows) | 1,672,032 |
| S=750-like (5,000 × 300, 100 × 270, act 64) | 32 | B₁ 103,680 (17,280 transfers: B alone > W, no drain); B₂ 104,227 (58,320 for 9,720 transfers + 449 rows) | 101,316-102,324 (648-1,152 rows), last 60,802 (395 rows) | 3,221,601 |

Compared with §11.3 at W = 100,000 (trader-set units): HL at N = 100k, 10 % takes 17 blocks, not
300. HL at N = 5k, 10 % closes in B, not 6 blocks. HL at N = 5k all-hold still takes 6 blocks (there
every trader holds every market, so the units barely change). S=750-like takes 32 blocks, not 29.
B₁ is now B's transfers only: no drain, no trader set (C2), and bounded `has_key` seeks.

### 12.4 `has_key` (decision 4, node-local, bit-identical)

`trader_positions::has_key` used to seek unbounded from `t ‖ 00×8` with limit 1. For a trader the
block emptied (B's ADL'd accounts, every key a tombstone over R), the overlay merge stepped over
its tombstones and then over every following emptied trader's, until it found another trader's
live key. B₁ empties 64 adjacent accounts × 270 keys, so this was quadratic: ~96 ms of B₁ (§11.3).
Each seek is now bounded to the trader's prefix (`iterate_cf_prefix_from`, which existed already)
and reads only the trader's own keys and tombstones. The answer is the same, since a key past the
prefix already meant `false`.

Proof:
* `trader_positions_tests::traders_after_equals_the_walk_over_r_and_pending`: random blocks,
  traders appearing and vanishing in the block, every cursor and limit; unchanged and passing.
* The E2 shadow in `liq_traders_after`, run in every lib test with shadow on.
* The new `has_key_seeks_only_under_the_traders_prefix`: 8 adjacent emptied traders × 20
  markets, a live trader after them, and traders with longer keys. Every seek is bounded to the
  trader's prefix, never returns another trader's key, and the answers equal the walk's.
  It was red before the change: the first seek was unbounded.

The drain also skips the trader set with C2 (see 12.1), so in production the B₁ drain no longer
calls `traders_after` at all.

### 12.5 Tests (written first; red against 8f1150b9 + the constant stub, then green)

New tests:
* `block_b_work_is_charged_into_w_before_the_drain` (bridge): p2 fixture, B = 8 × 6 = 48.
  * At W = 47 (B alone exceeds W) and W = 48: no drain in B; 8 rows; u1 and u2 flat at 0; the
    escrow equals Σ rows; the step stays due; units 48. Block 3 drains all 8 rows for 44 units.
  * At W = 49: one row drains (overshoot), 60 units.
* `a_ranking_charges_the_holders_of_its_market_not_the_trader_set` (bridge): 33 traders, of which
  1 holds the ranked market. The block costs T + 4: 1 visit, 1 holder, 1 valuation, 1 read. With
  the old units it was 35.
* `adl_charged_holders_equal_a_state_walk_in_every_r_mode` (bridge lib): the HL-like and storm
  shapes, run in every combination of:
  * R off, inline or worker (`end_resident_on_worker`, as the goldens);
  * caches on or off;
  * C2 on or off.

  At every ranking, a `#[cfg(test)]` assert in `adl_candidates_of` checks that the charged H(m)
  equals a full walk of `CF_NATIVE_POSITIONS` through the overlay. With C2 and the shadow, the C1
  shadow compares candidates, order **and the holder count**. Every run checks the same number of
  rankings and gives the same rows, units and results block by block.
* `has_key_seeks_only_under_the_traders_prefix` (12.4).

Existing tests re-derived for the new units. Each was red first, with its new expectation:

| test | change |
|---|---|
| `adl_work_per_block_is_option_2_and_covers_a_thin_hl_event` (core; was `..._covers_an_hl_sized_event`) | the new floor and W = 100,000 |
| `an_hl_shaped_event_costs_exactly_the_sizing_formula` | U(N) = 300 T + 100 (N + 1) + N + 600; W = U closes in B, W = U − 2 leaves the last row |
| `p2_drain_stops_at_w_and_resumes_in_fifo_order` | W = 63; per-block units 70 / 64 / 11 |
| `p2_drain_overshoots_by_at_most_one_step` | W = 60 / 61 |
| `p2_escrow_pairing_units_are_linear_in_the_rows` | 3K − 1: the market's only holders are the escrows, so H = 0 |
| `telemetry_reports_the_adl_queue_escrow_and_value_sum` | B at W = 0 shows 48 units; block 3 = 48 + 22 |
| `liquidation_e2e_adl_obligations_drain_over_empty_blocks`, `liquidation_e2e_adl_drain_survives_a_restart` (app.rs) | at W = 2, B's 14 transfers spend the block, so the drain runs in blocks 3..=16 (one more empty block); the restart is at 8 rows left |

The in-code walk assert also turned `adl_drain_caches_are_bit_identical`,
`adl_c2_holder_lists_are_bit_identical_to_c1` and `p2_ranks_each_market_side_once_per_block` red
until the units changed. The storm shape's W is now `STORM_W` = 160, so the drain still interleaves
with B (B's 3 accounts × 6-7 positions = 108-114 units per B block). `ubench_adl.rs` was changed:
* its HL assert uses U from 12.2 (one block iff W ≥ U; more than one iff W ≤ U − 2), and it prints
  `U`;
* its per-block bound is max(B's units, W + one row).

### 12.6 Goldens: no re-pin

Every pinned digest is unchanged in every R mode, serial and engine: `PRE_ROW50_A`,
`ROW50_OUT_A`, `GOLDEN_A`, `GOLDEN_B`, `REDUCED_A` and `REDUCED_B`
(`perf_equivalence_golden.rs`). Why:
* **Scenario A has no ADL.** `GOLDEN_PRINT=1` shows `adl=0` (cumulative `liquidations_adl`) in all
  12 blocks of every R mode. So the A pins cannot depend on the ADL units, and they did not move.
* **Scenario B ADLs** in blocks 8-12 (cumulative 2 → 6 accounts, the vault included). Each event
  is a few rows whose units are far below both the old and the new W, so the whole queue drains in
  its own block under both. The per-block state and outputs are therefore identical, which the
  unchanged per-block `GOLDEN_B` / `REDUCED_B` digests (full DB + outputs) prove directly.
  `liquidation_adl_work_total` is not part of any digest.
* No other test pins a digest over liquidation state (the other 64-hex constants are in genesis,
  RPC and type encodings).

The units are visible only in `liquidation_adl_work_total` and in drain progress, so every test
that pins either is re-derived in 12.5.

## 13. Re-measure at W = 100,000 with holder units (s99)

`crates/torus-bridge/tests/ubench_adl.rs` on perf/adl-budget @ **f6a54382** (local, not pushed). This is §12 as built:
* W = `ADL_WORK_PER_BLOCK` = 100,000 (`UB_ADL_WORK` not set);
* a ranking charges H(m);
* block B is charged into W (T = `ADL_TRANSFER_UNITS` = 6 per escrow transfer, 1 per first-sight valuation);
* `has_key` is prefix-bounded.

No code or bench knob was changed. The method is as in §9 / §11:
* release build, `CARGO_TARGET_DIR=~/.cargo-target-adl-budget`, `RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"`, `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`;
* binary `ubench_adl-5626de9bfbcc1f30`, sha256 `9b04a13c…`;
* 3 runs per case, run as the systemd unit `bench-adl-s99`, plus one perf unit `bench-adl-s99-perfc3`;
* every case started at 1-min load < 1.5. The 5/15-min averages were still 7-9 / 7, decaying after a nextest.

B ms, step ms and ns/unit are as in §9. "Gross ns/unit" = step ms / `adl_work`. "Rig" = ozarchy ms × 1.9-2 (§9 factor).

### 13.1 Units: measured = §12.3 model, exactly

| case | load at start (1/5/15 min) | blocks | block B units (rows) | later blocks units (rows) | last block | total | vs §12.3 |
|---|---|---|---|---|---|---|---|
| 1 HL N = 5,000, all hold | 1.10 / 9.12 / 7.62 | 6 | 101,930 (55) | 100,128 × 4 (54 each) | 55,069 (29) | 557,511 | identical |
| 2 HL N = 5,000, 10 % | 1.06 / 8.47 / 7.44 | 1 | 57,283 (300) | — | — | 57,283 | identical |
| 3 HL N = 100,000, 10 % | 1.06 / 8.34 / 7.41 | 17 | 102,763 (19) | 100,064-113,593 (18-21; 21 rows = 113,593 in block 5) | 44,933 (8) | 1,672,032 | identical |
| 4 S=750-like (5,000 × 300; 100 × 270) | 1.05 / 7.02 / 7.01 | 32 | B₁ 103,680 (0 rows: 17,280 transfers × 6, no drain); B₂ 104,227 (449 rows; 58,320 for 9,720 transfers) | 102,324 × 14 (1,152), 101,932 (956), 101,316 × 14 (648) | 60,802 (395) | 3,221,601 | identical |

Units, rows and transfers are bit-identical across the 3 runs of every case, and no block differs from the model. The bench's single-block U (`U=` line) is 507,501 for case 1 and 1,101,695 for case 3. The multi-block totals are higher (557,511 / 1,672,032) because a market whose rows span a block boundary is ranked again (§11.3).

### 13.2 Time per block (ozarchy ms, 3 runs; rig = × 1.9-2)

| case | block | transfers | rows | units | B ms | step ms (r1 / r2 / r3) | rig ms | gross ns/unit |
|---|---|---|---|---|---|---|---|---|
| 1 HL 5k all-hold | B (h=4) | 300 | 55 | 101,930 | 1.0 | 113.4 / 110.7 / 109.1 | 207-227 | 1,070-1,113 |
| | 2-5 | 0 | 54 | 100,128 | — | 54.6-59.8 | 104-120 | 545-597 |
| | 6 | 0 | 29 | 55,069 | — | 28.4 / 28.5 / 28.1 | 53-57 | 510-518 |
| 2 HL 5k 10 % | B (only) | 300 | 300 | 57,283 | 0.9 | 57.2 / 57.0 / 61.1 | 108-122 | 995-1,067 |
| 3 HL 100k 10 % | B (h=50) | 300 | 19 | 102,763 | 1.0 | 138.0 / 137.0 / 134.6 | **256-276** | 1,310-1,343 |
| | 2-16 | 0 | 18-21 | 100,064-113,593 | — | 101.8-123.3 | 193-247 | 1,008-1,112 |
| | worst later: 5 | 0 | 21 | 113,593 | — | 123.3 / 118.1 / 118.1 | 224-247 | 1,040-1,086 |
| | 17 | 0 | 8 | 44,933 | — | 45.6 / 44.2 / 44.4 | 84-91 | 984-1,015 |
| 4 S=750-like | B₁ (h=4) | 17,280 | 0 | 103,680 | 65.7-65.9 | 66.0 / 66.2 / 66.1 | 125-132 | 637-639 |
| | B₂ (h=5) | 9,720 | 449 | 104,227 | 35.1-35.5 | 107.1 / 106.6 / 107.3 | 203-215 | 1,023-1,030 |
| | 3 | 0 | 1,152 | 102,324 | — | 62.3 / 63.0 / 62.7 | 118-126 | 609-616 |
| | 4-31 | 0 | 648-1,152 | 101,316-102,324 | — | 54.8-59.9 | 104-120 | 537-590 |
| | 32 | 0 | 395 | 60,802 | — | 33.8 / 33.4 / 33.2 | 63-68 | 546-556 |

Sum of step ms per event: case 1 366-367, case 2 57-61, case 3 1,738-1,791, case 4 1,844-1,847.

**ns/unit by kind of work:**

| work | ns/unit | per transfer |
|---|---|---|
| B's transfers (B ms / 6 T) | 500-556 (HL), 602-609 (B₂), 634-636 (B₁) | 3.0-3.8 µs, so T = 6 holds |
| plain drain blocks, N = 5k all-hold and S=750-like | 535-605 | — |
| HL 10 % holders (N = 5k and 100k) | ~1,000-1,090 | — |
| drain inside block B | 1,070-1,110 (case 1), 1,310-1,345 (case 3), 1,536-1,547 (case 4 B₂, 45,907 drain units) | — |

The spread is now ~2-3×. In §11 it was 114-1,144 (10×) under trader-set units.

### 13.3 Against §11 at W = 100,000 (trader-set units, `UB_ADL_WORK=100000`)

| shape | §11: blocks / worst (later) ms | §13: blocks / worst (later) ms | change |
|---|---|---|---|
| HL 5k all-hold | 6 / 115-117 (57-61) | 6 / 109-113 (55-60) | same blocks; units barely change (H = N + 1 ≈ N + 3) |
| HL 5k 10 % | 6 / 18-19 (9-11) | **1** / 57-61 | closes in B, as at W = 630k (§11.1: 59-61 ms) |
| HL 100k 10 % | 300 / 28-30 (14-30) | **17** / 135-138 (102-123) | 18× fewer blocks; the block is ~4.6× heavier; event step time 1.74-1.79 s |
| S=750-like | 29 / B₁ 290-293 (B₂ 152-156; drains 24-61) | 32 / B₁ **66** (B₂ 107; drains 55-63) | B₁ −77 %; B₂ −31 %; 3 more blocks |

### 13.4 Answers to 18c / owner

**(a) Is every HL block ≤ ~250 ms rig-equivalent?**

Every block but one is. The exception is case 3's block B.

| HL shape | worst block | ozarchy ms | rig ms | ≤ 250 rig? |
|---|---|---|---|---|
| 5k all-hold | B | 109-113 | 207-227 | yes |
| 5k all-hold | later | ≤ 59.8 | ≤ 120 | yes |
| 5k 10 % | B (only block) | 57-61 | 108-122 | yes |
| 100k 10 % | B | 134.6-138.0 (142 under perf) | **256-276** | **no**, 2-10 % over |
| 100k 10 % | block 5 (21 rows, 113,593 units) | 118-123 | 224-247 | yes, at the edge |
| 100k 10 % | other later blocks | 102-112 | 193-224 | yes |

Read as ozarchy ms, every HL block is ≤ 138 ms. The §11.5 ozarchy line was ≤ ~125-130 ms, and case 3's B is over it by 5-8 ms.

**(b) S=750-like B₁ and B₂.**
* **B₁ is now transfers only: 66.0-66.2 ms on ozarchy, 125-132 rig.**
  * At W = 100k it was 290-293 ms, and 245-254 ms even at W = 50k.
  * B ms = step ms, so there is no drain work: 103,680 units = 17,280 × 6, and `adl_drain` has 0 samples.
  * `traders_after` / `has_key` do not appear in the profile at all (§13.5).
* **B₂ (9,720 transfers + 449 drain rows + 1 ranking path): 106.6-107.3 ms, 203-215 rig.** It is case 4's worst block.
  * Split: B 35.1-35.5 ms (3.6 µs per transfer); `liq_view` ~30 ms (the cold healthy scan, §13.5); the drain ~42 ms.

**(c) Blocks per event:**

| case | blocks |
|---|---|
| HL 5k all-hold | 6 |
| HL 5k 10 % | 1 |
| HL 100k 10 % | 17 |
| S=750-like | 32: B₁ has no drain, B₂ starts it, 30 drain-only blocks follow |

### 13.5 Profiles (perf 4,999 Hz, `--call-graph fp`, one perf run per case)

Each block was isolated as a burst of samples under `run_liquidations_with`, with a gap of more than 3 ms between bursts. Burst k = block h=k, and the span matches the block's step ms. Percentages are inclusive (partly nested) shares of the block's samples.

**Case 4 B₂ (h=5, worst block of case 4; 547 samples ≈ 109 ms; this run's step 110.7 ms).** The callees of `liquidation_pass`:

| callee | share | ms |
|---|---|---|
| `adl_drain` | 38.8 % | ~42 |
| `liq_view` | 27.8 % | ~30 |
| `adl_to_escrow` | 27.6 % | ~30 |
| `mark_pending` | 2.9 % | |
| `settle_flat_deficit` | 1.1 % | |

* `adl_drain`: `get_position` ← `adl_candidates_of` 25 %, `adl_rank` 4.8 % (sort 2.9 %).
* `liq_view`: all of it is `pos_sums` → `cached_sums` → `build_sums` (26 %), the healthy scan's classification of 2,048 traders × 270-300 positions running on a cold sums cache.
* `adl_to_escrow`: `transfer` 17.9 %, of which `apply_fill` 13.3 %, and `get_position` 8.2 %.
* `traders_after` / `has_key`: 0 samples (§12.4: the drain skips the trader set under C2).
* Leaf (self) frames: `find_key_index` 13 % (B-tree search), `select_unpredictable` 9 % (binary search in the records), `__divti3` 7 %, `build_with` 7 %.

**Case 4 B₁ (h=4; 349 samples ≈ 70 ms).** The callees of `liquidation_pass`:

| callee | share |
|---|---|
| `adl_to_escrow` | 81.7 % |
| `mark_pending` | 9.5 % |
| `settle_flat_deficit` | 3.2 % |
| `positions_for_trader` | 2.9 % |
| `liq_view` | 1.4 % (only 64 traders scanned) |

* Inside `adl_to_escrow`: `transfer` 49.9 %, of which `apply_fill` 39.3 %; `get_position` 24.6 % (B-tree `search_tree`/`find_key_index` 24 %); `put_obligation` 12 %; `delete_position` 8.3 %; `put_position` 6.3 %.
* `adl_drain`, `traders_after`, `has_key`: 0 samples.
* So B₁ is pure escrow-transfer work: 3.8 µs per transfer, linear in the 17,280 transfers.

**Case 1 block B (h=4; 574 samples ≈ 115 ms; this run's step 117.8 ms).**

| frame | share | ms |
|---|---|---|
| `adl_drain` | 71.8 % | ~82 |
| `liq_view` | 27.4 % | ~31 |
| `adl_to_escrow` | 0.5 % | |

* `liq_view` is all cold `pos_sums` → `build_sums` (26 %), as in B₂.
* `adl_drain`: `get_position` ← `adl_candidates_of` 50.9 % (`resident_positions` binary search 24.7 %, overlay `layer_touches` 19.7 %, `dirty` 19.5 %), `adl_rank` 11.8 % (sort 5.9 %), `holders_with` 1 %.

Block 2 for comparison (h=5; 282 samples ≈ 56 ms):
* `adl_drain` 97.9 %, `liq_view` 0.7 %;
* `get_position` 59.9 % (`layer_touches` 15.2 %);
* `adl_rank` 21.3 % (sort 12.1 %).

So block B's ~2× over a later block at the same units is:
1. ~30 ms of cold healthy-scan classification;
2. ~25 ms more in the drain's `get_position`, because each read's overlay dirty check (`layer_touches` → B-tree `find_leaf_edges_spanning_range` 10 %) runs over a current layer that holds B's writes.

**Case 3 block B (h=50; 689 samples ≈ 138 ms; this run's step 142.2 ms; extra perf unit).**
* `adl_drain` 95.5 %, `liq_view` only 3.5 % (holders hold ~30 markets, so the cold scan is cheap).
* `get_position` ← `adl_candidates_of` 43.4 % (`layer_touches` 15.5 %, `dirty` 11 %).
* `adl_rank` 28.3 % (sort 25.3 %).
* `pos_sums` ← `adl_candidates_of` 7 %.
* `get_native_balance` 9 %.

Block 2 (h=51; 565 samples ≈ 113 ms):

| frame | block B | block 2 |
|---|---|---|
| `adl_drain` | 95.5 % | 98.9 % |
| `get_position` | 43.4 % | 41.1 % |
| `adl_rank` | 28.3 % | 33.5 % (sort 29.7 %) |
| `get_native_balance` | 9 % | 12 % |
| `layer_touches` | 15.5 % | **6 %** |
| `dirty` | 11 % | **4.6 %** |

Case 3's block B excess (~25 ms over a later block) is the same overlay dirty check as case 1's, not the scan. Why case 3 costs ~1,000-1,100 ns/unit against case 1's ~540:
* the sort over ~10,000 holders per ranking (n log n, ~30 % of the block);
* `get_native_balance` for the candidates;
* a 3M-row R.

### 13.6 Reading

* The units are exact: every block of every case equals the §12.3 model. B's charge works: B₁ alone exceeds W, so it does not drain. Holder units give 1 block for 5k 10 % and 17 blocks for 100k 10 % (§11: 6 and 300).
* T = 6 is confirmed: B costs 3.0-3.8 µs per transfer = 500-636 ns per B-unit, the same band as a plain drain unit (535-605 ns).
* The only block over ~250 ms rig is **HL 100k 10 % block B (135-138 ms ozarchy, 256-276 rig)**. It is over because block B's drain pays ~25 ms more per unit than later blocks (the overlay dirty check against B's writes). The 10 % shape also costs ~2× per unit vs all-hold (sort over 10k holders, balances). Units do not see either.
* In block B of the bench, ~30 ms is the healthy scan running against a cold sums cache. This happens when the scanned traders hold ~300 positions: case 1 and case 4 B₂, not case 3. The bench's scan cursor revisits traders 0-2,047 for the first time since the mark moved at h=2, so this part is a bench-shape effect and not ADL work. How it compares with production depends on how often production's mark-table version changes; that was not measured here.
* S=750-like B₁ is fixed: 66 ms (−77 %), pure transfer work, and `has_key` / `traders_after` are absent. The worst S=750-like block is now B₂ at 107 ms (203-215 rig).

### 13.7 Commands and files

Build (worktree root `/home/oz/projects/wt/adl-budget`):

    CARGO_TARGET_DIR=~/.cargo-target-adl-budget RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes" \
      CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo test -p torus-bridge --release --test ubench_adl --no-run

Runs (the same env; the campaign called the binary
`~/.cargo-target-adl-budget/release/deps/ubench_adl-5626de9bfbcc1f30 --ignored --nocapture` directly):

    UB_ADL_HL=1 UB_ADL_TRADERS=5000 <bin> --ignored --nocapture                                  # case 1
    UB_ADL_HL=1 UB_ADL_TRADERS=5000 UB_ADL_HOLDERS_PCT=10 <bin> --ignored --nocapture            # case 2
    UB_ADL_HL=1 UB_ADL_TRADERS=100000 UB_ADL_HOLDERS_PCT=10 <bin> --ignored --nocapture          # case 3
    UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270 <bin> --ignored --nocapture     # case 4
    perf record -F 4999 --call-graph fp -o perf-<case>.data -- <bin> --ignored --nocapture       # same env per case
    perf script -i perf-<case>.data -F tid,time,ip,sym > <case>.txt
    python3 -I analysis/prof.py <burst,...> < <case>.txt                                         # burst k = block h=k

Launch: `tools/matched-bench/campaign/detach.sh adl-s99 ~/bench-results-matched/ubench-adl-s99/campaign.log bash -c '…/campaign.sh; echo "exit=$?" > …/campaign.done'`.
The case 3 perf run used `adl-s99-perfc3` with `perf-c3.sh` in the same way.

Raw files in `~/bench-results-matched/ubench-adl-s99/`:
* per-run logs `c{1,2,3,4}.r{1,2,3}.log` and the smoke run `smoke-c2.log`;
* the campaign `campaign.{sh,log,done}`;
* the perf data and logs `perf-c{1,3,4}.{data,log}`, `perf-c3.{sh,campaign.log,done}`;
* `analysis/`: `per-block.txt`, `ns-per-unit.txt`, `prof-c4-B1-B2-blk3.txt`, `prof-c1-B-blk2.txt`, `prof-c3-B-blk2.txt`, and the scripts `split.py`, `prof.py`, `ns.py`, `tab.py`.

Setup times: case 1 1,500,400 rows (6.0 s), case 2 150,126 (0.6 s), case 3 3,003,738 (12.8 s), case 4 1,527,270 (6.3 s).
