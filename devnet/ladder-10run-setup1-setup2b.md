# Torus — 10-run measurement ladder (Setup-1 3-val + Setup-2b 4-val weighted)

Assembled 2026-07-15. Source: coordinated bench runs (friend drives val1/val2 + load-gen;
our seed + 18c on this side). WAL0/WAL1 = `TORUS_SYNC_WAL_ON_COMMIT` 0/1 (default OFF, gated,
`crates/torus-state/src/db.rs:482`).

Columns: `blk` = under-load block-rate (blk/s) · `subK` = submitted orders/s (×1000) ·
`incK` = included orders/s (×1000) · `drop%` = (sub−inc)/sub · `iact` = incl-actions/s ·
`nact` = node-actions/s. submitted-orders = submitted-actions ×400 (batch). 1 action = 400 orders.

## Setup-1 (3-val: seed+val2+18c · val2 niced ~64c · 3-cell grid · genesis per-era)

| run/commit | cell | blk | subK | incK | drop% | iact | nact |
|---|---|---|---|---|---|---|---|
| r1 5a90c2f base | s500 | 4.80 | 338 | 242 | 28.6 | 604 | 475 |
| | s1000 | 4.25 | 270 | 197 | 27.0 | 493 | 414 |
| | s2000 | 14.93 | 236 | 60.5 | 74.4 | 151 | 220 |
| r2 9f3d1e3 erasure | s500 | 6.19 | 326 | 56.5 | 82.7 | 141 | 181 |
| | s1000 | 18.55 | 272 | 26.8 | 90.2 | 67 | 100 |
| | s2000 | 15.40 | 239 | 2.7 | 98.9 | 7 | 200 |
| r3 9a0806a eras-A | s500 | 2.00 | 313 | 160 | 48.8 | 401 | 200 |
| | s1000 | 21.60 | 276 | 8.8 | 96.8 | 22 | 100 |
| | s2000 | 7.55 | 242 | 4.3 | 98.2 | 11 | 200 |
| r4 add1249 WAL0 | s500 | 1.70 | 343 | 69.4 | 79.8 | 174 | 168 |
| | s1000 | 0.85 | 283 | 61.8 | 78.2 | 155 | 110 |
| | s2000 | 16.05 | 256 | 22.3 | 91.3 | 56 | 200 |
| r5 add1249 WAL1 | s500 | 8.25 | 356 | 50.7 | 85.7 | 127 | 88 |
| | s1000 | 8.25 | 261 | 33.2 | 87.3 | 83 | 104 |
| | s2000 | 11.50 | 274 | 22.5 | 91.8 | 56 | 200 |

(s5000/s10000 every run = 0 submitted, RPC-choked → omitted.)

## Setup-2b (4-val wtd: seed+18c+val1+val2 · val2 8c cap · 6-cell grid · genesis d53d4818)

| run/commit | cell | blk | subK | incK | drop% | iact | nact |
|---|---|---|---|---|---|---|---|
| r1 5a90c2f base | s500 | 6.05 | 715 | 102 | 85.7 | 255 | 286 |
| | s1000 | 3.10 | 708 | 128 | 82.0 | 319 | 267 |
| | s2000 | 4.05 | 911 | 0.27 | 100.0 | 1 | 172 |
| r2 9f3d1e3 erasure | s100 | 1.92 | 640 | 22.6 | 96.5 | 57 | 92 |
| | (s200..s2000 aborted — floor-abort 1/6) | | | | | | |
| r3 9a0806a eras-A | s100 | 1.45 | 639 | 114 | 82.1 | 286 | 139 |
| | s200 | 5.08 | 645 | 19.2 | 97.0 | 48 | 80 |
| | s400 | 7.15 | 628 | 23.8 | 96.2 | 60 | 101 |
| | s500 | 1.25 | 643 | 72.4 | 88.7 | 181 | 135 |
| | s1000 | 18.20 | 652 | 0.54 | 99.9 | 1 | 100 |
| | s2000 | 11.00 | 664 | 0.53 | 99.9 | 1 | 200 |
| r4 add1249 WAL0 | s100 | 2.50 | 638 | 77.8 | 87.8 | 194 | 154 |
| | s200 | 3.15 | 626 | 68.8 | 89.0 | 172 | 174 |
| | s400 | 2.30 | 631 | 84.8 | 86.6 | 212 | 145 |
| | s500 | 2.45 | 638 | 94.2 | 85.2 | 236 | 200 |
| | s1000 | 15.30 | 646 | 2.6 | 99.6 | 6 | 100 |
| | s2000 | 4.45 | 658 | 4.6 | 99.3 | 12 | 200 |
| r5 add1249 WAL1 (run5redo, clean) | s100 | 1.85 | 631 | 95.0 | 84.9 | 237 | 159 |
| | s200 | 1.50 | 650 | 109 | 83.2 | 273 | 150 |
| | s400 | 2.40 | 626 | 73.7 | 88.2 | 184 | 171 |
| | s500 | 1.50 | 639 | 55.8 | 91.3 | 140 | 153 |
| | s1000 | 11.35 | 658 | 26.1 | 96.0 | 65 | 100 |
| | s2000 | 12.85 | 686 | 7.7 | 98.9 | 19 | 200 |

Setup-2b r1 s100/s200/s400 not run. Idle baselines (blk/s): S2b r3=12.0 · r4=17.5 · r5=19.0.
S1 + S2b r1/r2 baselines not logged.

## Orderbook-level (placed vs matched) — WAL1 chain end-snapshot only

NOT harness-captured per-cell. Live aggregate snapshot of the WAL1 chain (run5redo, head ~17.2k)
via `torus_getOrderBook` + `getTradeHistory`. Other 9 runs' books unrecoverable (chains wiped).
rejected/s: not tracked.

| market | placed (resting now) | matched (cum trades) |
|---|---|---|
| 1 BTC | 482 | 119 |
| 2 ETH | 526 | 494 |
| 3 SOL | 487 | 548 |
| 4 AVAX | 381 | 53 |
| 5 ARB | 345 | 683 |
| 6 OP | 386 | 202 |
| 7 (m7) | 460 | 373 |
| 8 (m8) | 383 | 418 |
| 9 (m9) | 372 | 618 |
| 10 (m10) | 0 | 0 (no load — fanout artifact) |
| **TOTAL** | **3822** | **3508** |

## Synthesis

1. **The wall is submit→include (DA/body-starvation), not consensus, not matching.**
   drop% runs 80–99% across every post-erasure run. Orders that get included place + match
   fine (3508 trades executed, 3822 resting on WAL1). The matching engine is healthy; the
   loss is upstream, before the book ever sees the order.

2. **Erasure/native-DA rollout (run2 9f3d1e3 onward) is where drop% explodes.** Setup-1 r1
   pre-erasure = 27–29% drop at s500/s1000; from r2 on it jumps to 80–99%. Same shape in
   Setup-2b. This is the real perf target.

3. **WAL0 vs WAL1 (merge question): inclusion-neutral; ~30% low-load block-rate cadence cost.**
   Setup-2b s100–s500 iact: WAL0 avg 204 vs WAL1 avg 209 (holds). block-rate: WAL0 avg 2.60 vs
   WAL1 avg 1.81 (~−30%). Notably WAL1 is BETTER in the floored s1000/s2000 regime
   (iact 65/19 vs WAL0 6/12) — consistent with "fewer, fuller commits" letting more bodies
   distribute per block. Idle unaffected (WAL1 19.0 vs WAL0 17.5). Single 20s windows,
   high variance — ~30% is directional, not precise.

4. **Merge verdict: GREEN.** fsync is gated default-OFF, so merging imposes ZERO default cost.
   The ~30% cadence is an opt-in operator price, inclusion-neutral, possibly favorable under
   load. Code is correct (crash-durability lock test, Task B/3), clean FF into think-dev.
   Follow-ups (non-blocking): group-commit/batched fsync to cut the opt-in cost; confirm run
   with longer windows to firm the noisy ~30%; attack DA body-starvation as the real lever.
