# Baseline-18c — perf/re-proof4 @ 95cd399 (first campaign on the 18-core box)

**Agent:** BENCH-18c  **Branch:** perf/re-proof4 (HEAD 95cd399 — C+D+rank8+rootcache+3b+B+wedge-fix, all flags default-off = exact-today)
**Box:** 18c (13.140.140.138), 18 cores. Bare-metal 3-validator devnet, ports 8645-7 / 30401-3 / 9161-3. localhost only, no docker.
**torus-node sha256:** aa141fe0409506e776a562e1d4e5c98de0e14772afd40bd3d479f3c0431c2b84
**Load (every loaded cell):** `bench-throughput consensus --rpc-urls http://127.0.0.1:8645 --senders 5000 --sender-offset 60 --duration 300 --concurrency 256 --batch-size 400 --markets 10 --econ --rate-total 750 --sign-mode eip712 --metrics-urls <9161,9162,9163>` (300k orders/s offered, 400-order PlaceOrderBatch).
**Base env (every cell):** TORUS_SHARD_CUSTODY=0 TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000 TORUS_NATIVE_TOTAL_BLOCK_CAP=100 (harness defaults, present in all cells incl. flags-off).
**Ground truth:** node Prometheus counters ONLY, scraped 1 Hz from val0:9161 into per-cell `funnel.csv` / `phase.csv`, analysed with `win60.awk` / `phase60.awk`. Per-cell `counters-before/after.txt` are direct start/end curl snapshots. **The bench-throughput HEADLINE was IGNORED — see Anomaly 2.**

## Run provenance (IMPORTANT)
- **Run 1 INVALID, discarded.** The orchestrator pointed the 1 Hz scrapers/awk at `~/bench-results-18c/` but `scrape.sh`/`scrape-phase.sh`/`*.awk` live in `~/torus-bench/devnet/wsl/results/`, so no CSV was ever written. Combined with the broken bench headline (Anomaly 2), run 1 showed "0 matched" everywhere — a pure measurement artifact, not a chain failure. Archived at `~/bench-results-18c/cells-run1-broken/`.
- **Run 2 VALID** — corrected scraper paths + direct counter snapshots. All numbers below are run 2 (`~/bench-results-18c/cells/`, `campaign2.log`, CAMPAIGN COMPLETE 2026-07-20T14:02:02Z, EXIT=0).

## Env verification (per cell, from /proc/<pid>/environ + node startup line)
All 3 validators fleet-uniform each cell (evidence in `cells/<C>/env-verify.txt`):
- **I, F:** S470 commit-lag view backoff **DISABLED** (commit_lag_cap=0, genesis_value=0); no win-combo vars.
- **W:** commit_lag_cap=**8** (env override, genesis_value=0) + TORUS_BOOK_ROWS=1 RESIDENT_BOOKS=1 NATIVE_ROOT_CACHE=1 PARALLEL_SETTLE=1.
- **S2/S4/S8/S12:** as W + TORUS_PARALLEL_BUCKET_HASH=2/4/8/12 + TORUS_BUCKET_MEMBER_CACHE_MB=256.
Fresh genesis + wiped data dirs (CLEAN=1) before every cell.

## Results — node-counter ground truth (run 2)

| Cell | Config | matched/s (win) | best-60s matched/s | placed/s (win) | blk/s (win) | worst-60s blk/s | committed (win) | peak execq | rej_book | gate 23.8 |
|------|--------|----------------:|-------------------:|---------------:|------------:|----------------:|----------------:|-----------:|---------:|:---------:|
| **I** idle | flags off, no load | — | — | — | **23.4** (idle) | — | — | — | — | PASS |
| **F** | flags off (cap0) | 6446 | 10307 | 8124 | 0.418 | 0.131 | 145 | 52 | 11 | FAIL |
| **W** | win-combo + cap8 | 9425 | 15618 | 11886 | 0.566 | 0.300 | 197 | 14 | 26 | FAIL |
| **S2** | W + bhash2 + mc256 | 9261 | 12688 | 11685 | 0.796 | 0.492 | 305 | 64 | 0 | FAIL |
| **S4** | W + bhash4 | **9804** | 14013 | 12359 | 0.724 | 0.383 | 254 | 60 | 0 | FAIL |
| **S8** | W + bhash8 | 9499 | 13554 | 11997 | 0.754 | 0.426 | 264 | 66 | 0 | FAIL |
| **S12** | W + bhash12 | 9176 | 15285 | 11586 | 0.739 | 0.426 | 260 | 65 | 0 | FAIL |

Idle Cell I: 23.4 blk/s mean across all 3 nodes (val0 23.41 / val1 23.44 / val2 23.49), 0 ERROR/panic lines, mesh formed. 18c idle reference — never compare cross-machine (VPS ref was 29.78).
All loaded cells FAIL the 23.8 blk/s health gate (blk/s 0.42–0.80). Per W2, the 23.8 gate is NOT the bar under 300k orders/s — the **exec envelope is the wall**; idle passes, load is the exec-wall regime (expected, not a regression). Zero rej_margin, zero send-queue-full, zero panics in every cell.

## Exec phase timers (ms/block) + per-bucket root cost

| Cell | root E→L | state_write L | flush L | dirty_buckets/obs L | **per-bucket root µs (L)** | root ×(L/E) |
|------|---------:|--------------:|--------:|--------------------:|---------------------------:|------------:|
| F (no root cache) | 164.8→**2013.7** | 1675.0 | 3689.9 | 776.7 | (full-scan, n/a) | **12.2×** |
| W (root cache, serial hash) | 320.2→649.2 | 214.8 | 881.3 | 10973.8 | **59.2** | 2.0× |
| S2 (2 threads) | 185.6→300.4 | 139.6 | 537.3 | 9291.9 | **32.3** | 1.6× |
| S4 (4 threads) | 269.7→303.2 | 171.9 | 601.3 | 9162.2 | **33.1** | 1.1× |
| S8 (8 threads) | 255.9→328.2 | 212.4 | 669.6 | 10344.0 | **31.7** | 1.3× |
| S12 (12 threads) | 195.6→288.7 | 197.2 | 637.3 | 9831.2 | **29.4** | 1.5× |

Per-bucket root µs = (root ms/block, LATE window) / (dirty_buckets/obs, LATE) × 1000. Microbench ref = 17µs; VPS in-vivo was 107µs (6.3×). NOTE: F uses a different state layout (BOOK_ROWS=0 → ~800 buckets, full-scan root) so its per-bucket cost is not comparable to the win-combo family (BOOK_ROWS=1 → ~9–11k buckets); compare per-bucket µs only W vs S-sweep.

## Verdicts

**(a) Flags-off vs win-combo delta.** Win-combo+cap8 (W) beats flags-off (F) by **+46% matched/s** (6446→9425 window; +52% on best-60s, 10307→15618), +46% placed/s, and 2.3× worst-60s blk/s (0.131→0.300) with 3.7× less exec backpressure (peak execq 52→14). The mechanism is exec-bounding: **F's root time degrades 12.2× early→late** (no native root cache → O(state) full scan = the exec wall), while **W's root cache holds it to 2.0×**. Flags-off is also wedge-prone — run-1 Cell F fully wedged (froze at height #789, 0 matched); run-2 F limped through at 6446/s. cap8+win-combo was stable in both runs. This reproduces W2's variance-collapse finding.

**(b) Contention sweep + best PARALLEL_BUCKET_HASH.** Parallel bucket-hash **halves per-bucket root cost**: serial (W) 59.2µs → 2 threads 32.3µs, then flat (S4 33.1 / S8 31.7 / S12 29.4µs). The knee is at **2 threads** — 4→12 add nothing to per-bucket µs. End-to-end matched/s is essentially flat across the sweep (~9.2–9.8k window), because at this scale the bottleneck is flush + state_write + consensus cadence, not bucket hashing. **Recommended knob: PARALLEL_BUCKET_HASH=4** — best window-avg matched/s (9804/s) and lowest root degradation (1.1× L/E), while 2 already captures the per-bucket win. Member-cache MB=256 added no measurable regression.

**(c) Did the 6× per-bucket gap close on 18 cores? YES — largely.** VPS: in-vivo 107µs vs microbench 17µs = 6.3×. On 18c even **serial** per-bucket dropped to 59µs = 3.5× microbench, and **parallel** bucket-hash brings it to ~30µs = **~1.8× microbench**. The gap was substantially CPU-oversubscription on the 8-core VPS; with 18 cores + parallel bucket hash the in-vivo per-bucket root cost lands within ~1.8× of the microbench floor.

## Anomalies
1. **Run 1 discarded** (scraper path bug) — see provenance. No chain fault.
2. **bench-throughput HEADLINE is BROKEN at 95cd399** — its `--metrics-urls` scraper reports 0 placed / 0 matched while the node counters (direct /metrics curl) show millions (e.g. W: 4,136,374 placed / 3,279,892 matched). Diagnosed with an isolated 45s probe: raw counters climbed to 1.8M/1.45M while the headline said 0. The load-gen-side headline must not be trusted at this commit; the 1 Hz CSV scrape is authoritative. **Suggest fixing the bench headline metrics parser separately.**
3. **"skipping duplicate/replayed native action on live commit path" WARNs** (thousands per cell) are **benign** re-proposal dedup, NOT order loss — proven by counters reaching millions with zero rej_other.
4. Idle→load block cadence collapses (23.4→~0.5 blk/s) — expected exec-wall regime under 300k orders/s, consistent with W2.
