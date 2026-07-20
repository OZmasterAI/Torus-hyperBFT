# SYS-PROOF — SYSTEMIC round on `perf/re-proof3` (re-proof2 + Package B)

Branch `perf/re-proof3` = re-proof2 (C+D+rank8+root-cache+3b parhash/member-cache+phase timers) **+ Package B** (B1 batched d2l ForwardBatcher + leader-hint; B2 consensus-direct fan + gossip-mirror; B3 gossipsub send-queue knob). Build @ HEAD `0296347`, release, RC 0. Std cell shape (comparability sacred, identical to S-cells): fresh chain per cell (CLEAN=1), 3 validators loopback, `--senders 5000 --markets 10 --batch-size 400 --econ --rate-total 750` (300k orders/s offered), `--metrics-urls` all 3, ~330s window, node Prometheus counters only. Base env replicates S1 exactly (BOOK_ROWS=1, RESIDENT_BOOKS=1, NATIVE_ROOT_CACHE=1, NATIVE_TOTAL_BLOCK_CAP=100, PARALLEL_SETTLE=1/MIN_FILLS=64, HASH_ONLY_PUSH_THRESHOLD=6M, SHARD_CUSTODY=0, RPC 64MB/1024). Node env verified per cell (`sys-proof-snaps/nodeenv-*.txt`).

## TL;DR — the plateau holds across Package B

> **NO-GO. Package B moves neither matched/s nor the health gate.** At 300k orders/s offered the 3-validator fleet enters a **HotStuff view-synchronization collapse** — repeated `WARN hotstuff_rs: dropping proposal header ... is_safe=false, justify_block_known=true` storms (views race ahead, no QC forms, no commit) that **freeze the chain for 175-310s of every ~330s run**. B2 consensus isolation cannot help: the wedge is a **safety/pacemaker failure, not a delivery failure** — gossip queues stayed empty (`queued=0 backpressured=0 dropped=0`), B1 batcher armed clean, zero SlowPeer/AllQueuesFull/send-queue drops on loopback. The bottleneck Package B was built to attack (dissemination/queueing) is **not** the bottleneck. Exec budget when the chain runs is unchanged from S1/S2 (~2-2.7s/blk, flush-dominated).

## Results (WINDOW-AVG from node-counter deltas over full window; same method as S1/S2)

| cell | config (on top of S1 base) | placed/s | matched/s | worst-60s blk/s | gate | wedge freeze |
|---|---|---|---|---|---|---|
| **S1** (rp2 ref) | parhash OFF | 3403 | **2714** | 0.3 | FAIL | (partial) |
| **S2** (rp2 ref) | parhash6 + membercache256 | 3347 | **2649** | 0.2 | FAIL | (partial) |
| **T1** run2 (rp3) | **parhash2** + membercache256, B default | 3135 | **2495** | **0.000** | FAIL | ~176s frozen @h146 |
| T1 run1 (rp3) | same (reproducibility) | 1509 | 1192 | 0.000 | FAIL | ~286s frozen @h116 |
| **T2** (rp3) | T1 + **DIRECT_FAN=1 GOSSIP_MIRROR=0** (queue 512=B3 default) | 1604 | **1268** | **0.000** | FAIL | ~310s frozen @h191 |
| T3 | **SKIPPED** — gate: T2 matched !> 1.25x T1 (1268 < 1490) | | | | | |

Reference band this entire mission (R/S/T cells): **~1.2-3.2k matched/s, gate FAIL everywhere.** T-cells landed at the wedge-heavy low end; run-to-run wedge timing dominates the number (T1: 1192 vs 2495 = 2x spread on identical config). Note: rp3 T-cells hit **0.000** worst-60s (total freeze) where rp2 S-cells held 0.2-0.3 — the T-cells wedged *harder*, though confounded (single S runs vs 2 T runs; parhash2 vs parhash-off; stochastic wedge).

## T1 knob question — does parhash2 beat S1(no-3b) and S2(parhash6)?

No. Matched/s (2495 run2 / 1192 run1) sits inside/below the S1-S2 band, dominated by wedge variance, not by the hash-parallelism setting. parhash2 does not oversubscribe CPU the way parhash6 was suspected to, but it also does not raise the ceiling — the ceiling is not exec-CPU-bound at this load, it is consensus-wedge-bound. Member-cache worked (T1 whole-window ~87% hits, T2 ~75%, consistent with S2's 92%); root_bucket_scans track misses; storage mechanism remains proven and irrelevant to the plateau.

## T2 question — does the ~2.3s non-exec term shrink with Package B?

**No.** Non-exec gap = (wall_seconds/block) - (exec_block_seconds/block), val0 counters:

| window | cell | wall/blk | exec_block/blk | **NON-EXEC GAP** |
|---|---|---|---|---|
| whole (wedge-inclusive) | T1 | 6.67s | 2.85s | **3.82 s/blk** |
| whole (wedge-inclusive) | T2 | 13.29s | 1.98s | **11.31 s/blk** |
| healthy recovery (snap4->5, ~ S2-late) | T1 | 4.17s | 4.19s | **~0.00 s/blk** |
| healthy recovery (snap4->5, ~ S2-late) | T2 | 4.77s | 2.72s | **2.04 s/blk** |

Two readings, same verdict: (1) whole-window, the gap **grows** (T2 11.3s) because Package B did not prevent the wedge — it *is* the dead-time. (2) In the healthy recovery burst (the fair comparison to S2's ~2.3s late-window), T2's gap is **2.04s** — statistically unchanged from S2's 2.3s, and T1's healthy gap is ~0 (exec-saturated backlog crunch). The "non-exec term" is not a stable consensus per-block overhead that B could shave; it is the binary consensus wedge smearing into the average. B's consensus isolation leaves it untouched.

## Wedge mechanism (val logs, all three vals, both T1 and T2)

```
WARN hotstuff_rs::hotstuff::implementation: dropping proposal header:
     view=402..506, justify_correct=true, is_safe=false, justify_block_known=true
```
Views advance freely (345->506) while height is pinned — proposals are *known* and *justify-correct* but fail the **safety rule** (`is_safe=false`): replicas are locked on a competing QC and refuse to vote, so no new QC forms and nothing commits. Classic HotStuff liveness-under-contention: at this offered load, leader rotation / view timeouts outpace network round-trips, the pacemaker loses view sync, and the chain stalls until a lucky re-sync. `execq` sat at **0** through every freeze (empty exec queue) — proof the stall is upstream of exec, in consensus.

## Package B counters — machinery healthy, wrong target

- B1: `torus_node::forward_batcher: B1 d2l forward batcher armed (TORUS_D2L_BATCH=1) batch_ms=25 max_bytes=524288 reforward_ms=2000` — armed and clean.
- B2: DIRECT_FAN=1 / GOSSIP_MIRROR=0 verified on all nodes (`nodeenv-T2.txt`); consensus fanned over `/torus/direct`, gossip mirror off (fully-isolated end-state — the only setting that actually tests the isolation thesis; safe here because all 3 validators flip simultaneously on loopback).
- B3: queue-len left at compiled default **512** (B3's recommended perf value; `=5000` would restore pre-B3). Pre-proposal broadcasts logged `queued=0 backpressured=0 dropped=0` throughout — **zero send-queue pressure**, so 512 is ample on loopback and B3 is a no-op at this scale.
- Zero SlowPeer / AllQueuesFull / send-queue-full across all vals — exactly as expected on loopback, and exactly why B cannot move the number: there was no queueing pathology to fix.

## Final ranked block-time budget (T2 healthy recovery, Package B ON, per-block seconds)

| phase | s/blk |
|---|---|
| flush (root 0.75 + state_write 0.48 + trie) | **1.38** |
| engine | 0.45 |
| verify | 0.29 |
| body_persist | 0.27 |
| phase_settle | 0.24 |
| save_books | 0.19 |
| evm | 0.15 |
| phase_match | 0.12 |
| **exec envelope (block)** | **2.72** |
| **+ consensus/non-exec** | **~2.04** |
| **= wall/blk (healthy)** | **~4.77** |

Flush (state-root + state-write) remains the single largest exec term, unchanged from every prior round. But the gate needs ~42ms/block (23.8 blk/s); the exec envelope alone (2.72s) is ~65x over budget even before the consensus wedge — and the wedge, not exec, is what pins worst-60s at 0.000.

## Verdict

The systemic round — first ever to include Package B dissemination plus the parhash2 contention fix — **moves neither matched/s nor the gate**. The mission-long plateau (~1.2-3.2k matched/s, gate FAIL) holds across Package B. The decisive finding: at 300k orders/s offered the wall is **HotStuff view-synchronization collapse in the consensus/pacemaker layer**, not execution and not dissemination. Package B's queue/isolation machinery is healthy and correct but targets a pathology (send-queue backpressure) that does not exist on this fleet — the queues were empty the whole time. Reaching the 250-400k matched/s target requires attacking (a) consensus liveness under load (pacemaker/view-timeout tuning, leader-pipeline depth, or capping offered ingress to keep views in sync) and (b) the ~2.7s exec envelope (flush-dominated) — in that order, because no exec win matters while the chain freezes for minutes at a time.

Artifacts: `prof-T1.csv` (run2), `prof-T1-wedge1.csv` (run1), `prof-T2.csv`; `sys-proof-snaps/` (6 phase snapshots x 3 nodes per cell + nodeenv). Machinery: `~/torus-bench-scratch/{t-driver.sh,prof-cell2.sh,gap.sh}`.
