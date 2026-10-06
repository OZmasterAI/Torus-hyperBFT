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

| case | block | transfers | rows closed | units | B ms | step ms (3 runs) | ns/unit |
|---|---|---|---|---|---|---|---|
| HL: 3 accounts × 100 markets | B | 300 | 300 | 500,800 | 1.0 | 532 / 537 / 540 | 1,059-1,075 (median 1,068) |
| S=750-like: 100 × 270 | B₁ (64 accounts) | 17,280 | 7,745 | 630,126 | 66-68 | 12,303 / 12,305 / 12,344 | ~19,450 |
| | B₂ (36 accounts) | 9,720 | 7,808 | 630,862 | 36-37 | 4,193 / 4,266 / 4,352 | ~6,700 |
| | drain 3 | 0 | 5,184 | 630,616 | — | 502 / 537 / 538 | ~830 |
| | drain 4 | 0 | 4,464 | 634,178 | — | 480 / 505 / 507 | ~780 |
| | drain 5 | 0 | 1,799 | 253,698 | — | 178 / 179 / 180 | ~700 |

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
* Open (owner): hoist the trader set out of `adl_candidates_of` (once per drain; a trader gone
  flat returns no position), and/or charge B's transfers into W, or a lower per-block act limit
  for ADL accounts; a per-block AV cache for the ranking. Not implemented.

Commands (worktree root, `CARGO_TARGET_DIR=~/.cargo-target-adl-budget`,
`RUSTFLAGS="-C link-arg=-fuse-ld=mold -C force-frame-pointers=yes"`,
`CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`; `UB_ADL_WORK` overrides W):

    UB_ADL_HL=1 UB_ADL_TRADERS=5000 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture
    UB_ADL_TRADERS=5000 UB_ADL_BANKRUPT=100 UB_ADL_POSITIONS=270 cargo test -p torus-bridge --release --test ubench_adl -- --ignored --nocapture
