# L3-verify campaign on 18c @ 06de732 vs control 4f832e5 (2026-07-20)

Verification of the L3 fix round (member-scan bound, compaction knobs, bytes_per_sync)
plus the first consensus RTT decomposition (torus_view_* histograms, idle cell RIv).
All loaded cells: mode-2 combo + bhash=1 + ORDERS_PER_BLOCK_CAP=400 @ 300k/s offered.
Control cell ran the saved pre-fix binary (sha256 in env-verify.txt per cell).
Raw: ~/bench-results-18c/cells-l3verify/ on 18c (campaign EXIT=0; val0.log archived per cell).

## Knob matrix (cap-400)

| Cell | binary | knobs | blk/s avg | worst-60s | matched/s |
|---|---|---|--:|--:|--:|
| CTRL400 | pre-fix | — | 8.86 | 3.80 | 2,595 |
| F400 | fixed | default | 4.12 | 2.37 | 2,209 |
| F400-jobs | fixed | bg_jobs=8+sub=4 | 9.12 | 3.97 | 2,385 |
| **F400-buf** | fixed | churny_buf=16 | **10.07** | **4.25** | 2,881 |
| F400-bps4 | fixed | bytes_per_sync=4 | 9.36 | 3.90 | 2,624 |
| F400-comb | fixed | all | 8.71 | 3.15 | 2,435 |

## Findings

1. **bhash=1 was the real Layer-3 item-#1 lever, confirmed in vivo:** control root fell
   26 → 7.7ms/blk purely from disabling parallel bucket hashing (the morning sweep's
   "bhash=4 best" verdict was an artifact of compaction-contended conditions). The
   diagnosis's µbench estimate (2.25ms) understated the in-vivo win ~8×.
2. **The fix round is mildly positive, not transformative:** fixed-binary cells
   (excluding the F400 dud — this shape has ±2× run-to-run noise, cf. C800) run
   8.7–10.1 blk/s vs 8.9 control; churny write-buffer is the best single knob
   (+13% avg, best worst-60s of the day 4.25). Knobs don't stack (comb ≈ control).
   Exec telemetry proves the fixed binary is never slower (root 7.5 vs 7.7ms,
   flush 27.6 vs 30.5ms); member misses are only ~4/blk at 98.5% hit rate, so the
   450× scan fix is pathology insurance, not a mean-mover at this shape.
3. **The block-time budget is now consensus-dominated.** At 9–10 blk/s, block time is
   ~100–110ms: flush ~28ms, engine ~16ms ⇒ ~60–70ms is consensus/pipeline. The RIv
   decomposition (idle, per view): **qc_collect 40.8ms** (leader waiting for votes),
   containing replica **insert_persist 18.4ms** (persisting an EMPTY block!) and
   **proposal_arrival 16.5ms** (loopback delivery latency — should be sub-ms);
   propose_build 7.4ms; vote_delay 1.6ms; view_duration 53.9ms.
4. **Gate arithmetic after this round:** best worst-60s 4.25 vs target 21.0. Closing
   requires the consensus terms: empty-block insert_persist (18ms — what exactly is
   fsync'd on the replica insert path? safety-critical durability, needs Fable-grade
   care), proposal_arrival (16.5ms on loopback — network stack batching/tick, likely
   node-local), and qc_collect residual. Storage-side knobs are exhausted as
   mean-movers; variance smoothing (churny-buf) is worth keeping.

## Recommendations

- Bench-standard env going forward: bhash=1 + TORUS_CHURNY_CF_WRITE_BUFFER_MB=16
  (keep member cache 256, root cache, resident books, mode 2, cap8).
- Next round = consensus RTT: (a) attribute insert_persist's 18ms (what's written +
  sync semantics; consensus-visible risk class — Fable if the durability contract
  changes); (b) proposal_arrival 16.5ms on loopback (libp2p/QUIC delivery path,
  likely node-local); (c) rerun RIv-style decomposition under load to see how the
  three terms scale with real blocks.
- Adaptive-knob chore (production debt): parallel bucket-hash should self-select on
  dirty-set size; proven knobs must become defaults or auto-tuned at final integration.
