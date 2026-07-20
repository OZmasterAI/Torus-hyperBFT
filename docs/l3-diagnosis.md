# Layer-3 opening diagnosis — cap-400 flush + idle-RTT attribution

Branch `perf/re-proof5` @ e788729 (code = 4f832e5). Diagnosis only; no fixes, no
devnets. Evidence: existing C400 cell data (`~/bench-results-18c/cells-capsweep/C400`
on 18c), a new small-N microbench (`l3_smalln_per_bucket`, run behind flock on 18c),
and code reading. cap-400 shape = **~226 dirty buckets/blk, ~1 entry/bucket**
(dirty entries ≈ dirty buckets: 402471/1776 ≈ 226; order_books-heavy churn).

Phase decode (phase.csv accumulators, valid to block ~1776 before the counter
freeze): root 46.41s/1776 = **26.1 ms/blk**, state-write 46.90/1776 = **26.4 ms/blk**,
flush total 98.24/1776 = **55.3 ms/blk**. (The `db_s` column is the dirty-bucket
accumulator, 401676/1776 = 226.2 — not a write timer; the atomic `db.write` is folded
into flush total, ~3 ms, WAL fsync OFF.)

## Microbench (warm caches = prod config TORUS_NATIVE_ROOT_CACHE=1 + MEMBER_CACHE_MB=256), THIN ~2 mem/bucket
```
N=  50  triecache_member p1=0.97ms(19.4us)  p4=1.93ms(38.7us)
N= 100  triecache_member p1=1.78ms(17.8us)  p4=3.27ms(32.7us)
N= 226  triecache_member p1=4.22ms(18.7us)  p4=6.47ms(28.6us)   <- cap-400 shape
N= 500  triecache_member p1=9.16ms(18.3us)  p4=10.53ms(21.1us)
N=1650  triecache_member p1=31.2ms(18.9us)  p4=34.0ms(20.6us)
uncachedfold N=226 46.4us/bkt  vs  N=1650 28.7us/bkt  (only the UNCACHED fold "explodes")
```

## Ranked fix list

**1. Set `TORUS_PARALLEL_BUCKET_HASH=1` (from 4). [config only — ZERO risk, node-local, value-neutral]**
Mechanism: at ~226 dirty buckets the scoped-thread spawn/join + allocator contention
is pure overhead. Evidence: N=226 p1=4.22 ms vs p4=6.47 ms (**−35%, −2.25 ms/blk**);
parallel LOSES at every N with caches on (never crosses over through N=1650). It only
wins in the cold uncached-fold path (irrelevant — caches are on in prod). Also frees 3
cores for compaction/exec, relieving the contention that inflates root 6→26 ms.
Optional code gate: `run_buckets` (native_trie.rs:879) `if parallel<=1 || work.len()<=1`
→ add `|| work.len() < PARALLEL_MIN_BUCKETS` (~3000). Saved: ≥2.25 ms/blk direct +
indirect.

**CONTRADICTS established context (Puzzle A).** The framing "per-bucket cost EXPLODES
at small dirty sets ⇒ fixed overhead (thread-spawn, tree-fold walk, fsync)" is wrong
for the prod config: with caches warm the per-bucket cost is **flat ~18 µs** across
50–1650 buckets. The explosion (46→28 µs) exists ONLY in the uncached DB-fold path
(depth-16 sibling reads that don't amortise at low density) — already eliminated by the
trie cache. There is no meaningful fixed floor: `default_nodes()` ≈5 µs, WAL fsync OFF,
mirror writes batched.

**2. Reduce member-cache misses [node-local]**
Mechanism: prod root 26 ms ≫ warm-bench 6.47 ms (par4) at identical N=226 — the 4×
gap is (a) member-cache MISSES on churny order_book/new-key buckets forcing cold
`CF_NATIVE_HASHED` prefix-scans over the large tombstone-laden mirror (802k writes /
593k deletes), and (b) worker threads descheduled by background compaction + the
CPU-bound leader. Evidence gap: member_hits/misses ARE exported (app.rs:1428-9) but
were NOT scraped into the cells. Sketch: scrape next run; if miss-heavy, raise
`TORUS_BUCKET_MEMBER_CACHE_MB` and/or pre-warm hot buckets. Expected: closes most of the
26→~7 ms root gap once contention (fix 1) is removed.

**3. Smooth RocksDB compaction (Puzzle B variance) [node-local, perf-only]**
Mechanism: per-window flush swings 39→119 ms/blk (peaks 189 ms), bursting every
~50–120 blocks. Decomposed: bad windows split into ROOT-CPU spikes (root 59–111 ms,
e.g. h=2023 root=111) AND NON-ROOT spikes (state-write+db.write 70–129 ms, e.g. h=1767
nonroot=129, h=1688 nonroot=108). iowait stays <4 %, box 27–62 % idle → NOT disk-bound;
it is compaction-thread vs exec CPU contention (max_background_jobs=4 vs a leader pinned
at 2 cores). Fix (db.rs:38-61): raise `max_background_jobs` 4→8 (18-core box); add
`set_max_subcompactions`; give churny CFs (order_books) a smaller write_buffer so they
flush small/often — the exact treatment already applied to CF_CONSENSUS_META (db.rs:71).
Fix 1 also frees cores for compaction. Risk: node-local.

**4. Re-check `bytes_per_sync=1<<20` (db.rs:41) [node-local]**
1 MiB range-sync during heavy flush can add write-path latency spikes (contributes to
the non-root bursts). A/B raise to 4 MiB or 0. Perf-only.

**5. Idle RTT = 38.1 ms/blk (Puzzle C) — decompose with EXISTING metrics, no new timers**
idle_blk/s=26.28 (RI health.log) ⇒ 38.1 ms/view. NOT fsync (TORUS_SYNC_WAL_ON_COMMIT
unset; kv_store writes non-synced, kv_store.rs:112). NOT the 10 ms poll (steady-state
recv is event-driven, wakes on message arrival; the 10 ms deadline at algorithm.rs:242
only fires during sync/deferred-proposal fallback). NOT max_view_time (500 ms, timeout
only). No fixed proposal-pacing sleep in the happy path (BODY_RETRY_INTERVAL=100 ms is
deferred-proposal only, implementation.rs:2475). So 38 ms = the sum of pipeline phases
across ~2 local-libp2p transport hops (proposal broadcast + vote round-trip) + insert/
persist — most likely transport-hop dominated on loopback (TCP+noise+yamux req/resp),
not consensus logic.

Attribution is possible WITHOUT new instrumentation: torus-telemetry/view_metrics.rs
already emits 9 histograms that were simply not scraped —
`torus_view_duration_seconds`, `torus_view_propose_build/delay/finalize_seconds`,
`torus_view_proposal_arrival_seconds`, `torus_view_vote_delay_seconds`,
`torus_view_qc_collect_seconds`, `torus_view_insert_persist_seconds`,
`torus_commit_interval_seconds`. Action: scrape these on a 120 s idle cell (next round)
and split 38 ms directly. RTT-reduction candidates then: if `proposal_arrival` +
`qc_collect` (transport hops) dominate → TCP_NODELAY / direct req-resp path in
torus-network transport; if `insert_persist` dominates → blocktree write path. This is
consensus-visible territory — measure before touching.

## Risk summary
Fixes 1–4 are node-local, value-neutral, no consensus/format impact (safe to A/B on one
validator). Fix 5 is diagnosis + a scrape recommendation; any RTT change is
consensus-visible and must follow the measurement.
