# W2 Wedge-Proof — perf/re-proof4 (S470 commit-lag view backoff)

**Agent:** W2-BENCH  **Branch:** perf/re-proof4 (HEAD 2b7329f, wedge-fix b8fcdec)
**Box:** VPS vmi3065152, WSL bare-metal 3-validator devnet (8645-7 / 30401-3 / 9161-3)
**Load (identical, every cell):** bench-throughput consensus, `--senders 5000 --markets 10 --batch-size 400 --econ --rate-total 750` (300k orders/s offered), `--concurrency 256`, 300s bench + 90s drain, metrics from all 3 nodes. Throughput from node Prometheus counters only.
**Base env (every cell):** TORUS_SHARD_CUSTODY=0 RPC_MAX_RESPONSE_MB=64 RPC_MAX_CONNS=1024 + S1 win-combo (BOOK_ROWS=1 RESIDENT_BOOKS=1 NATIVE_ROOT_CACHE=1 PARALLEL_SETTLE=1 PARALLEL_SETTLE_MIN_FILLS=64) + TORUS_WEDGE_DIAG=1 (all cells).

## Question
Does the S470 commit-lag wedge fix eliminate the 175-310s commit freezes under 300k orders/s on a real 3-validator devnet?

## Answer: YES — the multi-minute commit wedge is eliminated at every cap tested.
The idealized "zero lock-drops" signature is NOT perfectly met (10-18 residual is_safe=false cross-branch drops persist at all caps), but those drops no longer wedge the chain: they resolve in <30s instead of freezing commit for 175-310s. Committed height advances in every 60s window, qc-committed collapses from ~112/311 (control) to ~3/8, and matched throughput rises 6-7x.

## Env verification (S470 startup line, all 3 nodes, per cell)
- V0: all 3 DISABLED (commit_lag_cap=0) — control confirmed.
- V1: all 3 ENABLED commit_lag_cap=4 — ENV-GATE PASS.
- V1b: all 3 ENABLED commit_lag_cap=8 — ENV-GATE PASS.
- V2: all 3 ENABLED commit_lag_cap=8 + PROPOSER_EXEC_WATERMARK=8 (cap via startup line, watermark via /proc) — ENV-GATE PASS.

## Results (node-counter ground truth)
| Metric | T-baseline (re-proof3, unfixed) | V0 OFF | V1 cap4 | V1b cap8 | V2 cap8+wm8 |
|--------|--------------------------------|--------|---------|----------|-------------|
| matched/s (headline node ctr) | 1460-1475 | 534 | 3195 | **3789** | 3447 |
| placed/s | ~1850-1875 | 681 | 4017 | 4758 | 4334 |
| in-window committed blocks | 36-51 | 20 | 79 | 94 | **123** |
| LONGEST ZERO-COMMIT GAP | 175-310s | **298s** | 25s | **12s** | 28s |
| freezes >=30s | multiple | 1 | 0 | 0 | 0 |
| worst-60s blk/s | 0.000 | 0.000 | 0.081 | 0.129 | 0.117 |
| window-avg blk/s | ~0.15 | 0.207 | 2.781 | 0.884 | 2.054 |
| qc-committed avg / max | ~100+ / 160+ | 112 / 311 | 3.8 / 20 | 3.2 / 8 | 3.4 / 8 |
| view-qc avg / max | small | 1.6 / 5 | 1.8 / 5 | 1.8 / 5 | 1.7 / 6 |
| is_safe=false jbk=true drops | many | 12 | 16 | 18 | **10** |
| made-no-progress sessions | many | 15 | 14 | 10 | 10 |
| avg block time | - | 2092ms | 3689ms | 2716ms | 2204ms |

(matched/s headline = torus_orders_matched_total delta over the harness 336s window. During the V0 wedge the counter FREEZES for ~240s mid-run then recovers only after load stops, so the wedged headline understates instantaneous throughput and is dominated by the freeze; the fixed cells match continuously.)

## Wedge-dead signature scorecard (implementer criteria)
1. qc-committed collapsed under ~10 (control races ~160): control 112/311 -> V1 3.8/20, V1b 3.2/8, V2 3.4/8 (cap8: ZERO frontier lines with qc-committed>=10). **MET (strict at cap8).**
2. committed_view tracks pc_view within a few views: avg 3.2-3.8. **MET.**
3. ZERO is_safe=false jbk=true drops: V1=16, V1b=18, V2=10. **NOT MET — residual cross-branch drops persist at every cap; pacing (V2) is lowest.**
4. no made-no-progress sync sessions: 15 -> 14/10/10. **NOT MET — reduced ~33%, not zero.**
5. committed advances in EVERY 60s window (worst-60s>0): 0.000 -> 0.081/0.129/0.117. **MET.**
6. no multi-minute freezes (longest gap): 298s -> 25s/12s/28s, zero freezes>=30s. **MET — the operational wedge is GONE.**

Score: 4/6 idealized criteria MET, incl. both that define the operational failure (multi-minute freeze, worst-60s=0). The 2 unmet (zero drops, zero sync sessions) are residual but benign — no drop-storm wedges the chain.

## Variance (dead wedge should collapse the 2x run spread)
- **Wedged/control state:** V0 534/s (this run) vs T-baseline 1460-1475/s (prior run) = ~2.7x spread — confirms the high run-to-run variance of the wedge.
- **Fixed state:** V1/V1b/V2 land in 3195-3789/s (1.19x spread) across THREE different cap/pacing configs. No two identical-config runs to isolate pure variance, but the tight clustering vs the 2.7x control spread is consistent with the fix collapsing run-to-run variance.

## V2 pacing question (does watermark smooth cadence or hurt?)
Pacing (cap8+wm8) SMOOTHS: fewest lock-drops (10 vs 18), most in-window committed blocks (123 vs 94), shortest avg block time (2204 vs 2716ms), same strict qc-committed<=8. Cost: matched/s 3447 vs 3789 (-9%). Net: better commit health, small throughput trade.

## Verdict & recommendation
The S470 commit-lag view backoff ELIMINATES the wedge. The 175-310s commit freezes that defined the failure (worst-60s 0.000, T1 baseline) do not occur at any cap; the chain commits in every 60s window, qc-committed stays bounded, and matched throughput reaches a mission-high 3789/s (cap8), 2.6x the best wedged baseline and 7x the control. Residual is_safe=false drops persist (10-18) but are benign.

**Recommended production knob: commit_lag_backoff_cap = 8** (genesis-set, fleet-uniform). cap8 dominates cap4 on gap (12s vs 25s), qc-committed bound (max 8 vs 20, zero>=10), and peak throughput (3789 vs 3195/s). PROPOSER_EXEC_WATERMARK=8 is an OPTIONAL add-on: it trades ~9% matched throughput for the best commit stability (fewest forks, most committed blocks, smoothest cadence) — enable it if fork-rate / wasted-exec becomes a concern; otherwise cap8 alone already kills the wedge.

Note: the 23.8 blk/s health gate is NOT the W2 bar (exec envelope is the layer-2 wall). The W2 bar — steady commit, worst-60s>0, no lock-drop-storm freeze — is met.
