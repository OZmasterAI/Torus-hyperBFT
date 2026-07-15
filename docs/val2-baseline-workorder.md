# Val2 baseline work order + context (2026-07-15)

Two sections: (1) the exact message to copy-paste to val2, (2) why we're doing this, so we
can pick up cleanly after a break.

---

## SECTION 1 — COPY-PASTE FOR VAL2

> **[Torus — baseline re-measurement + orderbook-execution fix · 3-VAL Setup-1 · branch off 5a90c2f]**
>
> **Coordinated relaunch — get val2 onto the baseline**
> We're switching off the WAL1 chain back to the Setup-1 baseline to measure what actually matters. Our side is already staged (seed + 18c: down, binary→`5a90c2f`, genesis→`5042dbdb` shipped+verified, WAL env removed).
>
> **Your val2 prep:**
> - **Commit `5a90c2f`** (baseline, pre-erasure).
> - **Genesis: regenerate → `5042dbdb`** (sha256 `5042dbdb1429ae80…`, 3 validators, 100,060 funded, chain_id 7778). **Reuse — no new genesis.** Byte-identical across all 3 nodes.
> - **`TORUS_SYNC_WAL_ON_COMMIT` unset** (baseline predates the toggle).
> - **3-val: seed + val2 + 18c. val1 stays DOWN.** Full mesh = **peers=2 each**, not 3.
> - Wipe val2 data-dir. val2 CPU as in original Setup-1 (~64c nice).
> - Ping when down + wiped + on 5042dbdb/5a90c2f — seed comes up first, then 18c, then val2. Gate climb-from-0 + peers=2.
>
> **Why (ground truth from the WAL1 chain's own counters):**
> ```
> torus_native_actions_processed_total   28,699   (actions actually executed)
> torus_orders_matched_total             60,881   (orders actually matched)
> resting now (getOrderBook orderCount)   3,822
> ```
> - **Matched was undercounted ~15.6×** — `getTradeHistory` caps at 1000/market. Use `getTradeHistoryRange` (paginate, 5000/call) or `orders_matched_total`.
> - **`×400` orders/action is a load-gen offered convention, not executed book orders.** Ground truth = **~2.1 matched orders per executed action** (60,881÷28,699) ≈ **0.53%** of the ×400. These fully executed, so the collapse is at the **placement layer**, not body-starvation.
> - So the baseline's "**~250k orders/s**" is a **DA/inclusion rate, not matched-and-placed.** Extrapolated real number: **~1,280 matched/s at peak (s500)** — but baseline was wiped, so unmeasured. That's why Phase 1 exists.
>
> **Phase 1 — REDO THE MEASUREMENT PROPERLY (prerequisite, before any fix)**
> Run the instrumented baseline and give us the true per-cell funnel:
> `submitted/s → included/s → executed/s → placed/s → matched/s → rejected/s`
> - Drive it off **node counters** (`torus_native_actions_processed_total`, `torus_orders_matched_total`, resting via `getOrderBook` orderCount) — **not** the ×400.
> - **Capture reject reasons** (untracked today) so we see where orders die.
> - Confirm or correct our **~1,280 matched/s** estimate.
>
> **Phase 2 — Investigate the collapse**
> Why does a 400-order batch yield only ~2 book orders/action on a chain with **no** body-starvation? Rejects? self-match/cancel? batch semantics? silent place failures? Pin the mechanism.
>
> **Phase 3 — Propose, coordinate, implement**
> - **Propose** the fix first (likely: fix whatever drops 400→2 at placement, and/or correct the metric so "orders/s" = executed book orders).
> - **Coordinate before implementing** — send proposal, we agree, then build.
> - **Push as a new branch off `5a90c2f`.**
>
> Bottom line: measure the real submit→include→execute→match funnel on an instrumented 3-val baseline first. The metric that matters is orders that place and execute via the matching engine, not hash-inclusion.

---

## SECTION 2 — WHY (context to pick up cleanly)

### The journey
We ran a 5-rung commit ladder (5a90c2f → 9f3d1e3 → 9a0806a → add1249 WAL0 → add1249 WAL1)
on two setups: Setup-1 (3-val) and Setup-2b (4-val weighted). Full table:
`devnet/ladder-10run-setup1-setup2b.md`. WAL merge verdict = GREEN (gated default-OFF, zero
default cost) — but that's paused; the bigger finding took over.

### The finding that redirected us
The benchmark's headline metric — "orders/s included" (= incl-act/s × 400) — does **not**
measure real orderbook throughput. It counts **hash-inclusion at the DA layer** using the
load-gen's ×400 *offered* convention. We proved this from the live WAL1 chain's own Prometheus
counters:
- `torus_native_actions_processed_total` = 28,699 (executed)
- `torus_orders_matched_total` = 60,881 (matched)
- → **~2.1 matched orders per executed action, ≈0.53% of the ×400 number**

So "our record, 250k orders/s on the baseline" is a DA-inclusion rate, **not** matched-and-placed.
The real matched/placed number is ~2 orders of magnitude lower. We estimate ~1,280 matched/s at
the baseline's s500 peak, but the baseline chain is **wiped** — unmeasured and unrecoverable.

Two layers of loss stack: (1) submit→include drop 80–99% (DA/body-starvation, post-erasure);
(2) include(×400)→executed book orders (~2/action, placement layer, present even pre-erasure).

### Why the baseline specifically
5a90c2f is pre-erasure — no hash/body split, no body-starvation — so it isolates the
placement-layer collapse cleanly (layer 2 only). It's also the "record" run people cite, so
getting its *real* funnel is the priority.

### Current state (where we are)
- **Both our nodes DOWN and staged** for baseline 5a90c2f 3-val: run1 binary + genesis 5042dbdb
  (verified both) + WAL env removed. Live WAL1 chain is stopped.
- **Pending:** user wipes both data-dirs; friend preps val2 (regen→5042dbdb, 5a90c2f, wipe);
  val1 stays down (3-val, peers=2); then coordinated seed-first launch.
- Runbook: `testnet/baseline-5a90c2f-3val-relaunch.md` (launch cmd, gotchas, instrumentation).
- Genesis 5042dbdb: 3 validators, 100,060 funded accounts, chain_id 7778. Backup at
  `torus-seed-live/genesis.5042dbdb-setup1.bak.json`.

### Next actions
1. User wipes data-dirs (seed `testnet/data`, 18c `/home/18c/torus-hyperbft/data`).
2. Send val2 Section 1.
3. Coordinated launch (seed-first), verify climb-from-0 + peers=2.
4. Friend runs instrumented Phase-1 measurement → real funnel numbers.
5. Phase 2 investigate → Phase 3 propose/coordinate/implement → push new branch off 5a90c2f.
