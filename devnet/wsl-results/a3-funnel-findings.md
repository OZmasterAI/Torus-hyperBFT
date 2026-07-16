# A3 — Instrumented funnel run: why ~398/400 orders die in `execute_batch`

**Date:** 2026-07-16 · **Branch:** `perf/funnel-truth` (8fa6ccd) · **Rig:** WSL Ubuntu, 8 cores/63 GB,
3-validator bare-metal devnet (val0 8645/9161, val1 8646/9162, val2 8647/9163), `TORUS_SHARD_CUSTODY=0`,
`TORUS_NATIVE_TOTAL_BLOCK_CAP=100`, `--native-gossip=true`, weighted-100k genesis (bulk senders offset 60,
1,000,000 TRS native each).

**Cell:** `bench-throughput consensus --senders 20 --sender-offset 60 --batch-size 400 --markets 10
--duration 200 --concurrency 64 --submit-batch 1 --rate 2 --format bin` (paced: 40 actions/s = 16,000
orders/s offered — funnel-shape run, not a max-throughput run). Metrics scraped at 1 Hz from val0
(`funnel-m10-b400-s20.csv`); all counter deltas below are load-window deltas from that CSV.

## Funnel (full 207 s load window)

| stage | total | per second |
|---|---:|---:|
| submitted actions (bench) | 8,020 | 40.0 |
| included actions (bench full block re-sweep, dup ×1.00) | 8,020 | 40.0 |
| executed actions (`torus_native_actions_processed_total`) | 8,020 | 38.7 |
| executed orders (actions × 400) | 3,208,000 | 15,496 |
| **placed_accepted** | **416** | **2.01** |
| matched fills (`torus_orders_matched_total`) | 235 | 1.14 |
| resting | 270 | 1.30 |
| **rejected_margin** | **3,207,584** | **15,496** |
| rejected_book / rejected_cancelled / cancelled_partial_fill / rejected_other | 0 | 0 |
| self_trade_cancels | 20 | 0.10 |

Closure is **exact**: 416 accepted + 3,207,584 margin-rejected = 3,208,000 orders executed.
Acceptance = **0.013 % = 0.052 orders per 400-order action** — the "400 → 2" collapse reproduced,
and every dead order died in exactly one place: **Phase 2 margin pre-reserve** (`execute_batch`,
`native_executor.rs:663`).

Peak 15 s window (t+155 s): 68 actions/s executed, 27,200 orders/s executed, **0 accepted**,
27,200/s margin-rejected — pure steady-state rejection.

Block rates: idle 28–31 blk/s warm (8.4 blk/s on the cold post-genesis health check), loaded 24.2 blk/s.
`Send Queue full` log lines during the run: **0** on all three validators. No node errors/panics.

## Pinned mechanism — bench order economics vs sender funding (bench-side)

- The bench generates orders with price ~U[55,000, 65,000] and quantity ~U[1,100] whole units
  (`tools/bench-throughput/src/main.rs:167-190`): average notional ≈ 60,000 × 50.5 ≈ **3.03 M TRS**.
- `margin_configs` in the executor is **never populated** (initialized empty at `native_executor.rs:268`,
  no inserts anywhere), so every order takes `unwrap_or(20)` default 20× leverage
  → margin ≈ notional / 20 ≈ **151 k TRS per order** on average.
- Bulk genesis senders hold **1,000,000 TRS** each (`testnet/gen-weighted-genesis.sh`, NATIVE_AVAIL).
- A sampled on-chain 400-order batch (block 13000 via `torus_getBlockBody`) needs **62.9 M TRS**
  of margin (avg 157 k/order, min 5.6 k, max 324 k); a fresh 1 M sender affords only the **first ~8 orders**
  of its first batch, sequentially, then every later order rejects with "insufficient margin".
- Orders are **GTC limit and never cancelled** by the bench, so reserved margin stays locked in resting
  orders. After each sender's first batch, its available balance is ~0 and *all* subsequent orders reject.
  The only replenishment is the occasional fill/STP-cancel margin release — hence the observed trickle
  (2 accepted/s across 20 senders) instead of exactly 20 × ~7 accepted then zero.
- Corroboration by book state (`torus_getOrderBook`): after 3.2 M submitted orders the 10 books hold only
  ~4–13 price levels per side (1 order per level), prices within the bench's 55–65 k band, tight spreads —
  consistent with 270 cumulative resting orders, not with a functioning 16 k orders/s flow.

Prior WAL numbers (28,699 actions → 60,881 fills → 3,822 resting) are the same shape: the placement
collapse is **not** a matching-engine or consensus defect — inclusion is 100 %, execution keeps up
(38.7 of 40 actions/s live, remainder drained after the window), rejected_book = 0, rejected_other = 0.
It is the **benchmark's order sizing vs genesis funding**: each 400-order batch demands ~63 M TRS of
margin from a 1 M TRS account, ~400 batches per sender per run demand ~25 B TRS.

## Fix sketch (A4)

Bench-side (primary):
1. Scale order size so a batch fits the balance: e.g. quantity U[0.01, 0.5] (margin ≈ 75–1,600 TRS/order,
   400-order batch ≈ 0.06–0.6 M TRS) — or price-proportional sizing targeting a fixed ~1–2 k TRS margin/order.
2. Stop margin accretion across batches: interleave `CancelOrder`/cancel-all actions, or use IOC for the
   crossing fraction, so resting GTC margin is recycled.
3. Alternatively (or additionally) raise `NATIVE_AVAIL` for bulk senders (e.g. 100 M TRS) in
   `gen-weighted-genesis.sh` — cheap, but without (2) any sustained run still hits the same wall later.

Node-side (optional, correctness-neutral): nothing is broken, but two observations for later work:
- `margin_configs` is dead config — either wire genesis `initial_margin`/tiers into the executor or drop the map.
- A margin-rejected order still costs full Phase-2 work; a per-sender "available exhausted" fast-path in
  `execute_batch` would cut wasted work under this failure shape (3.2 M rejects burned CPU harmlessly here,
  but they also masked as "load" in earlier WAL-based readings).

## Files

- `devnet/wsl-results/funnel-m10-b400-s20.csv` — 1 Hz counter scrape (val0), full run.
- `devnet/wsl-results/bench-m10-b400-s20.log` — bench stdout (submit/include live view + final re-sweep).
- `devnet/wsl-results/a3-funnel-summary.csv` — windowed rates summary.
- WSL `~/torus-hyperbft/devnet/wsl/results/` — same + gzipped validator logs (`val{0,1,2}-a3.log.gz`)
  and the run scripts (`scrape-funnel.sh`, `run-cell.sh`).

*Throughput figures above are node Prometheus counters (and the bench's block-body re-sweep for inclusion),
never submitted×batch arithmetic.*
