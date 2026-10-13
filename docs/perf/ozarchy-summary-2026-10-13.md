# ozarchy campaigns 2026-10-13: absolute per-arm values

Sources: results doc `docs/perf/ozarchy-antispam-item6-pf1-2026-10-04.md` sections 53-58 (branch docs/wedge-fix, worktree
`/home/oz/projects/wt/docs-wedge-fix`), 59 (branch docs/p3-cloid), 60 (branch docs/p3-cloidprof), 61 (branch docs/p3-gate) and 62 (branch docs/p3-kd), `<run>/analyst/{cells.json,arms.txt,paired.txt,soak.txt,series.txt}`
(61 also `arms-soak.txt`, `a-term.txt`; 62 also `restart.txt`)
and, for 53d / 57 only, the cells' `summary.json` (see footnotes). Shape everywhere: tw, Classic, 300 markets, MAX_IN_FLIGHT 4, commit tail, shared NVMe.

| Sec | arm (sha, allocator) | matched/s: whole cell [min 0-2 / 6-10] | native blk/s | blk time p99 ms | node CPU-s/1M | RSS mean / peak GB per node | dev writes MB/s/val | settle ms/native blk | key delta (CI), as in the write-up |
|---|---|---|---|---|---|---|---|---|---|
| 53 soak | gm main 219f0c20, glibc | 168,384 [200,881 / 135,177] | 2.95 | 1,135 | 30.35 | 4.15 / 4.22 | - | n/a | step 2 RSS +1.98 GB/node @10 min (+1.67..+2.27), still +40..+67 MB/min (CIs excl. 0); single cells |
| 53 soak | g2 step 2 7b006739, glibc ¹ | 172,911 [206,925 / 137,008] | 3.03 | 1,140 | 28.47 | 6.00 / 6.22 | - | 21.9 | |
| 53b soak | jgm main 219f0c20, LD_PRELOAD je ¹ | 191,756 [208,224 / 163,200] | 3.15 | 1,083 | 29.93 | 4.27 / 4.60 | - | n/a | Phase 3 delta je +0.68 GB (min 5-10; +0.52..+0.79), +1.01 peak; glibc +1.86; single cells |
| 53b soak | jg2 step 2 7b006739, LD_PRELOAD je | 199,134 [223,954 / 165,169] | 3.48 | 1,048 | 27.78 | 4.95 / 5.60 | - | 18.9 | |
| 53c soak | g2 step 2 7b006739, glibc ¹ | 176,544 [215,840 / 142,051] | 3.03 | 1,198 | 27.78 | 6.04 / 6.26 | - | 21.7 | cs2: 0 expiries vs 16,168/node; RSS vs s53 main +2.76 GB, slopes +129..+178 MB/min (CIs excl. 0) |
| 53c soak | cs2 41a95355 (s2 + cancel fix), glibc | 190,776 [201,516 / 182,401] | 2.60 | 1,496 | 24.46 | 6.97 / 7.43 | - | 31.8 | |
| 53d soak | mf de97682f (main + fix), glibc | 185,054 [201,476 / 176,031] | 2.50 ² | - ³ | 27.17 ² | 4.33 / 4.42 | - | n/a | cs2 - mf +2.62 GB, diff slope +120..+158 MB/min (CIs excl. 0); jcs2 - mf +1.10 GB, slopes CIs span 0 |
| 53d soak | cs2 41a95355, glibc | 190,602 [204,027 / 181,995] | 2.60 ² | - ³ | 24.55 ² | 7.02 / 7.52 | - | 31.5 ² | |
| 53d soak | jcs2 41a95355, LD_PRELOAD je | 214,299 [207,969 / 213,669] | 2.92 ² | - ³ | 24.06 ² | 5.46 / 5.94 | - | 29.2 ² | |
| 54 120 s | g2 step 2 7b006739, glibc, 15 s | 213,819 | 3.51 | 1,321 | 23.29 | 4.64 / 5.67 | 65.1 | 23.4 | mw: settle -20.3 ms [-20.9, -19.6]; blk/s +11.3% [+8.4, +14.1]; matched/s +1.8% [-1.9, +5.4] |
| 54 120 s | mw 3cf8f72d, glibc, 15 s | 217,658 | 3.90 | 1,282 | 23.61 | 4.53 / 5.56 | 66.8 | 3.1 | |
| 55 120 s | g2 7b006739, glibc, 15 s | 211,652 | 3.48 | 1,293 | 23.36 | 4.61 / 5.68 | 64.7 | 23.2 | g5: RSS peak -0.33 GB [-0.65, -0.01]; CPU +1.10% [+0.44, +1.75]; writes +10.8% [+7.0, +14.6] |
| 55 120 s | g5 7b006739, glibc, 5 s | 209,656 | 3.49 | 1,233 | 23.61 | 4.34 / 5.35 | 71.7 | 21.8 | |
| 56 SAT 120 s | jb main deeacdb3, glibc | 215,255 | 3.61 | 1,128 | 24.74 | 3.48 / 4.07 | 138.0 | n/a | ja: matched/s -0.3% [-1.5, +0.9]; blk/s +14.9% [+3.2, +26.6]; CPU +2.66% [+1.83, +3.49] |
| 56 SAT 120 s | ja 88ddc4c2, je compiled in | 214,571 | 4.15 | 1,047 | 25.40 | 3.35 / 4.08 | 144.8 | n/a | |
| 56 FIX 907 120 s | jb deeacdb3, glibc | 162,468 | 3.80 | 1,161 | 29.00 | 3.34 / 3.93 | 209.0 | n/a | ja: CPU +4.26% [+3.26, +5.25]; RSS mean -6.6% [-8.9, -4.2]; matched/s -2.2% [-3.5, -0.9] |
| 56 FIX 907 120 s | ja 88ddc4c2, je compiled in | 158,902 | 4.18 | 1,059 | 30.24 | 3.12 / 3.79 | 218.2 | n/a | |
| 57 soak | jmf 0360c840 (main + fix), je compiled in | 194,684 [199,262 / 191,138] | 2.71 ² | - ³ | 28.22 ² | 4.42 / 4.67 | - | n/a | jcs - jmf +0.94 GB (min 5-10), +1.27 peak; levels off (diff slopes +43 [-4, +90], -80 [-134, -27], +8 [-49, +65]) |
| 57 soak | jcs e7be8460 (s2 + fix), je compiled in | 216,765 [208,457 / 217,531] | 2.97 ² | - ³ | 24.01 ² | 5.39 / 5.97 | - | 28.4 ² | |
| 57 soak | jb deeacdb3, glibc, no fix | 174,556 [214,934 / 139,385] | 2.95 ² | - ³ | 29.28 ² | 4.16 / 4.26 | - | n/a | ja - jb +0.15 GB (min 5-10), +0.39 peak; both lose load from min ~3 (no fix) |
| 57 soak | ja 88ddc4c2, je compiled in, no fix ¹ | 194,248 [213,025 / 170,704] | 3.21 ² | - ³ | 29.69 ² | 4.31 / 4.66 | - | n/a | |
| 58 SAT 120 s | wm main 925fd962, glibc | 200,292 | 3.55 | 1,181 | 25.86 | 3.49 / 4.11 | 156.9 ⁴ | n/a | wf: matched/s -0.86% [-2.42, +0.69]; CPU -0.39% [-1.79, +1.02] |
| 58 SAT 120 s | wf e3bb7cca, glibc | 198,561 | 3.53 | 1,196 | 25.76 | 3.53 / 4.17 | 156.2 ⁴ | n/a | |
| 58 KILL ⁵ | wm 925fd962 (16 SIGKILLs) | longest gap 19.7 mean / 29.6 max s; kill -> resume 20.0 / 29.9 s | - | - | - | - | - | n/a | wf - wm longest gap -1.1 s [-3.7, +1.4]; resume -0.9 s [-3.4, +1.5]; no wedge in either arm |
| 58 KILL ⁵ | wf e3bb7cca (16 SIGKILLs) | longest gap 18.6 mean / 27.2 max s; kill -> resume 19.1 / 27.3 s | - | - | - | - | - | n/a | |
| 59 SAT 120 s | c0 main ef812b02, glibc, CLOID_SHARE=0 | 188,654 | 3.41 | 1,251 | 26.43 | 3.62 / 4.25 | 145.1 | n/a | c100: matched/s -11.7% [-13.8, -9.6]; CPU +8.84% [+7.37, +10.31]; exec p99 +18.9% [+9.4, +28.4] |
| 59 SAT 120 s | c100 main ef812b02, glibc, CLOID_SHARE=100 | 166,551 | 3.17 | 1,402 | 28.77 | 3.86 / 4.57 | 147.7 | n/a | |
| 60 PROF 120 s | c0 main ef812b02, glibc, CLOID_SHARE=0, perf on val1 | 191,565 | 3.48 | 1,231 | 26.35 | 3.59 / 4.25 | 144.0 | n/a | c100: matched/s -12.5% [-21.6, -3.5]; engine/1k fills +13.1% [+2.8, +23.4]; CPU +8.94% [+3.44, +14.44] |
| 60 PROF 120 s | c100 main ef812b02, glibc, CLOID_SHARE=100, perf on val1 | 167,570 | 3.12 | 1,409 | 28.70 | 3.88 / 4.54 | 148.5 | n/a | |
| 61 SAT 120 s | m main de1a97c1, je compiled in | 198,189 | 3.82 | 1,138 | 26.51 | 3.69 / 4.63 | 159.0 | n/a | s2: CPU -9.72% [-10.46, -8.98]; writes -54.8% [-56.9, -52.7]; matched/s +8.8% [+5.2, +12.4]; blk/s +20.1% [+10.3, +30.0]; RSS mean +0.99 GB [+0.67, +1.30] |
| 61 SAT 120 s | s2 step 2 c88d3073, je compiled in, 15 s | 215,590 | 4.58 | 1,340 | 23.94 | 4.68 / 5.91 | 71.9 | 3.5 | |
| 61 soak | m main de1a97c1, je compiled in | 193,398 [201,679 / 190,516] | 2.61 | 1,196 | 27.93 | 4.67 / 4.91 | 231.6 | n/a | s2 - m RSS +1.43 GB/node (min 5-10), +1.80 peak; levels off (s2 slopes -77 / -66 / -10 MB/min; diff slope CIs span 0); CPU -12.4%, writes -64%; single cells |
| 61 soak | s2 step 2 c88d3073, je compiled in, 15 s | 219,139 [210,202 / 218,481] | 2.97 | 1,473 | 24.46 | 6.14 / 6.75 | 82.9 | 9.0 | |
| 61 TERM | m de1a97c1 (1 SIGTERM) | SIGTERM -> exit 21.5 s; kill -> first chain commit 37.3 s; replay 0 | - | - | - | - | - | n/a | s2 restart 35.0 s vs m 37.3 s (gate 60 s); replay 0 in both; one kill per arm |
| 61 TERM | s2 c88d3073 (1 SIGTERM) | SIGTERM -> exit 19.1 s; kill -> first chain commit 35.0 s; replay 0 | - | - | - | - | - | - | |
| 62 KILL | m de1a97c1 (4 SIGKILLs) | kill -> val1 first commit 27.2 mean / 31.95 max s; replay 61-65 blocks (= exec queue) | - | - | - | - | - | n/a | s2 29.9 / 39.5 s vs m 27.2 / 31.95 s (gate 60 s); replay from the marker + 1, contiguous; state hashes agree (121 checkpoints) |
| 62 KILL | s2 c88d3073 (4 SIGKILLs) | kill -> val1 first commit 29.9 mean / 39.5 max s; replay 72-113 blocks (last 100-block checkpoint) | - | - | - | - | - | - | |
| 62 soak | s2def c88d3073, je defaults, 15 s | 217,605 [210,278 / 217,519] | 2.95 | 1,398 | 24.47 | 6.40 / 7.07 | - | 9.2 | dec - def RSS -1.72 GiB/node (min 5-10), -1.71 peak; je allocated unchanged; CPU +4.6%, matched/s -2.5%, blk/s +2.6%; single cells |
| 62 soak | s2dec c88d3073, je + MALLOC_CONF=background_thread:true,dirty_decay_ms:1000, 15 s | 212,075 [207,428 / 210,248] | 3.02 | 1,425 | 25.60 | 4.64 / 5.32 | - | 8.9 | |

Definitions and caveats

- **Soak vs 120 s cells are not comparable on matched/s or RSS.** 120 s cells = minutes 0-2 only (n = 4 ABBA, arm means);
  soaks = one 600 s cell per arm, no CI. Soak RSS = minutes 5-10 mean / peak of VmRSS (mem-samples, 5 s), mean of the 3
  per-node values in the doc's RSS tables. 120 s RSS = cpu.csv whole-cell mean / peak, mean of 3 nodes. Doc deltas for
  53d / 57 come from the difference series, so differences of these absolute means can differ from them by up to ~0.07 GB.
- **Without the cancel fix** (53 gm/g2, 53b, 53c g2, 57 jb/ja) the soak workload collapses from minute ~6 (margin
  rejections), so whole-cell and minute 6-10 matched/s and soak CPU-s/1M are not full-load figures.
- **Allocators:** 53b jgm/jg2 and 53d jcs2 are LD_PRELOAD system jemalloc 5.3.1; 56 ja and 57 jmf/jcs/ja are compiled-in
  tikv-jemallocator (also covers RocksDB). Everything else glibc.
- **59 (cloid cost):** one binary (main `ef812b02`, node and bench built together) in both arms; the arms differ only in the
  bench's cloid share (run-cell.sh `CLOID_SHARE` 0 vs 100). 100% is the worst case; realistic shares are not measured.
- **60 (cloid profile):** 59's binary and arms, n = 3 ABBA, perf recording on val1 in every cell (user space; val1's W1
  engine is ~4% above its whole-cell value in both arms, not a controlled overhead measurement); matched/s is val0's headline (not profiled). Device writes from `paired.txt` (`a-arms.py` has no `wr` row).
- **61 (final Phase 3 gate):** both arms jemalloc compiled in, node and bench built per arm, CLOID_SHARE=0, full workload.
  SAT = n = 4 ABBA 120 s; soak = one 600 s cell per arm (no CI; soak columns from `arms-soak.txt`, RSS = minutes 5-10 mean /
  peak of the 3 per-node values in `series.txt`, matched/s minute windows from `series.txt`); soak settle = `ck_settle` (summary
  phase per native block; per `torus_exec_native_blocks_total` it is 8.8, SAT 3.1). TERM = one 300 s cell per arm, SIGTERM of val1 at bench+180 s; restart = kill -> first chain commit after the restart.
- **62 (SIGKILL restart gate + jemalloc decay):** p3gate's staged binaries (not rebuilt), CLOID_SHARE=0, full workload. KILL =
  2 cells x 2 SIGKILLs of val1 per arm (300 s, state hash on), restart = kill -> val1's own first commit (`restart.txt`; on
  the 61 chain basis s2 29.9 / 40.0 s). Soak = one 600 s cell per setting on s2: RSS = minutes 5-10 mean / peak of the 3
  per-node MB values in `series.txt` / 1000, as the 61 soak rows (the delta text and section 62 use GiB = MB / 1024:
  6.25 -> 4.53); blk/s, CPU, p99 and settle from `cells.json` (p99 = mean of nodes, a-arms.py's definition); device writes
  not computed (no `a-arms.py --io` run). The decay setting is `MALLOC_CONF` in the node environment, not compiled in.
- **Commit tail everywhere** (bench in-flight released on committed bodies); block time p99 = commit interval from val0-2
  logs, mean of 3 nodes. Closed-loop bench: faster commits raise offered load (jemalloc and mw rows).
- **Settle wait** = `checkpoint_settle_wait` summary phase, ms per native block, mean of 3 nodes; n/a = no Phase 3 (main
  arms export none).
- ¹ rc 2 REJECT on agreement only, the known mark-only digest artifact (books, OI, cash identical); used in the write-ups.
- ² Not in the 53d / 57 write-ups or analyst outputs: read from each cell's `summary.json` (native blk/s = headline
  `native_blk_s`; CPU-s/1M and settle = a-cells.py's definition, mean over nodes of whole-run (user+sys)/fills and of the
  phase ms). The same method reproduces every 53 / 53b / 53c `cells.json` value exactly.
- ³ No soak analysis for 53d / 57 computed block-time p99 (`summary.json` has only p95; 53-53c from `cells.json` blocks).
- **Device writes "-" for the 53-57 soaks:** no soak analysis computed io-ticks writes; deriving them from raw io-ticks would be a
  new computation, not an existing output. The 61 soak writes are `arms-soak.txt` row `wr` (a-arms.py `--io`).
- ⁴ From `ozarchy-wedge-sat-run/analyst/paired.txt` row `wr` (not in section 58's table).
- ⁵ Kill cells: 540 s, val1 SIGKILL at 60/180/300/420 s, state hash on; their throughput is not analysed (restarts), so
  only stall timing is given (chain = val0 commit lines).
