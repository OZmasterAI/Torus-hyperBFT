# ADL per-block budget (P0 before testnet)

**Status (s18, ozarchy):** design **decided** (owner 18c s96). Q1-Q4 are the recommendations in
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

W is one constant (630,000, measured in §9), high enough that an HL-sized event (a few hundred
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
  (no R: tests, tools) it is C1. **Units are unchanged:** a ranking still charges the whole
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
