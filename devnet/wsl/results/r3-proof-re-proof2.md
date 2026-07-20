# Round-3 proof @ re-proof2 (cc8e39e+root-cost 5a443ab merge)

Cells S0-S2 (S3 skipped: S2 !> 1.25x S1). Std shape: 5000 senders/10 mkts/bs400/econ/rate 750 (300k/s offered), ~330s, fresh chain per cell, counters only. Node env recorded per cell (nodeenv-S*.txt).

| cell | flags | placed/s | matched/s | worst60 | gate | blk avg |
|---|---|---|---|---|---|---|
| S0 | all off, cap100 | (see log) | ~3.2k band | 0.000 | FAIL | 749ms->2973ms (depth ramp 3.97x) |
| S1 | rows+resident+rootcache+psettle | 3403 | 2714 | 0.3 | FAIL | 2123ms |
| S2 | S1 + parhash6 + membercache256 | 3347 | 2649 | 0.2 | FAIL | 5241ms |

## S2 mechanism evidence (val0, early->late deltas, 32 blocks/210s)
- member cache: 92.6% hit rate late; bucket scans 825/blk vs ~11k dirty buckets/blk (13x scan elimination). Cache works as designed, 0 evictions @256MB.
- resident books: load_books 0.0025s TOTAL over the window (dead as a cost).
- BUT root still 1.18s/blk (~107us/bucket in vivo vs 17us microbench full-stack = 6x gap — suspect CPU contention: 6 hash workers + parallel settle + rocksdb on 8 cores) + state_write 0.80 + body_persist 0.49 + engine 0.72 (settle .39/match .21/margin .06) + verify 0.42 + evm 0.17 ~= 4.3s accounted of ~6.6s late wall; ~2.3s/blk sits OUTSIDE exec counters (consensus/dissemination/queue — Package B territory, never in any proof branch).

## Verdict
Storage mechanism: PROVEN (slope flat, scans gone, reload gone). Throughput: UNMOVED (2.4-3.2k matched/s band across every config this mission). Gate: FAIL everywhere. The budget is now diversified — no single >50% wall remains; engine+verify+body alone (~1.6s/blk) bound blocks well above the 42ms needed for the 23.8 blk/s gate at this load shape.
