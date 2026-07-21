# Level-hash cache in-vivo A/B — 18c @ 13bd833 (2026-07-21)

Campaign `run-cacheab.sh`, cells in `~/bench-results-18c/cells-l3scrape/CACHE_{OFF,ON}/`.
Branch perf/l3-levelhash-cache (= merged re-proof5 b1aba10 ⊕ savebooks attribution ⊕
sponge cache + staged seeding). Cap-400 bench-standard env, 300s @ rate 750, one
binary, env A/B (`TORUS_LEVEL_HASH_CACHE` unset vs 256).

## Headline

| metric | CACHE_OFF | CACHE_ON | delta |
|---|---:|---:|---|
| blk/s window-avg | 6.89 | **9.96** | **+45%** |
| matched/s window-avg | 1,623 | **2,932** | **+81%** |
| **worst-60s blk/s (gate)** | 0.672 | **4.500** | **6.7×** |
| best-60s blk/s | 24.7 | 24.3 | = (idle-ish ceiling unchanged) |
| view_duration mean | 134.1ms | **97.1ms** | −28% |
| exec_block mean | 75.7ms | 59.2ms | −22% |
| save_books /loaded blk | 83.2ms | **37.1ms** | −55% |
| engine /loaded blk | 52.1ms | 41.6ms | −20% (deschedule relief) |
| flush /loaded blk | 50.5ms | 38.3ms | −24% (ditto) |

Cache verdict: **works in vivo as designed** — save_books halves, everything else
speeds up from freed CPU, worst-60s (the decay endgame) improves 6.7×. The −55%
(vs µbench's ~9×) is the expected in-vivo mix: match-invalidated levels full-rehash,
staged seeding needs one clean interval before hitting, books keep growing, and
~2.5ms of rows/stops/meta rides in the span.

## Gate status: NOT passed — but the wall moved

Worst-60s 4.50 vs gate 21.0. The depth-decay is damped, not cured: at cell end
(~1M+ resting accumulated) the residual per-loaded-block chain is still
~37 (save residual) + 42 (engine) + 38 (flush) + verify/evm ≈ ~125ms. Remaining
depth-coupled or contention-coupled consumers, in order: engine 41.6ms (µbench 16
serial — ~2.6× contention inflation), flush 38.3 (state_write dominant), save
residual 37.1 (invalidated-level rehashes = still O(depth) on match-touched levels).

## Next-round candidates (post-merge)

1. **Match-touched level residual**: matches invalidate level FRONTS — a front-pop
   is a suffix-preserving op on the FIFO... but keccak absorbs front→back, so
   front-pops break the prefix. Only a commitment change fixes that class
   (chunked/Merkle — consensus-visible, tag 0x04+, production debt). Node-local
   option: bound invalidated-level rehash via level segmentation is the same thing.
   So the node-local cache is likely near its ceiling; measure hit/miss/seed stats
   next campaign (gauges exist but weren't exported to Prometheus this run — debt).
2. **Flush at depth** (state_write 23.8ms baseline, grows with dirty set).
3. **Engine contention** (41.6 in vivo vs 16 µbench): fewer competing pools now that
   save_books freed CPU; re-measure after merge; consider settle-engagement tuning.
4. Re-run the cap sweep: with depth-decay damped, a higher ORDERS_PER_BLOCK_CAP may
   now dominate cap-400 on matched/s (drain-rate equilibrium shifts).

## Cell hygiene note

CACHE_OFF ran right after the 65GB debug-artifact wipe + fresh release build (cold
caches) — its 6.89 avg is below the morning's healthy 8.43 control. The A/B holds
regardless (ON ran second on an equally-warm box and beat the WARM historical
control by +18% blk/s / +22% matched), but treat OFF's absolute as conservative.
