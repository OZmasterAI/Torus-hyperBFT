# Deletable raw files in ozarchy `~/bench-results-matched`

Nothing was deleted when this list was made. Sizes are file sizes (apparent), taken at archive time on ozarchy (btrfs: the space actually freed can differ if files
are reflinked). **Run the commands on ozarchy, after this directory is committed.**

**Updated 2026-10-08 (s29).** By this update every file in the original lists below (88 Tier A + 1,008 Tier B files) had already been deleted on ozarchy. Added for the 64 dirs inventoried in s29 (see `INDEX.md`): 6 campaigns (`p2s0b`, `p2s0r`, `p2s0x`, `p2s0y`, `read-gas-stall`, `rpg-ac`) in the per-campaign table and as the last 6 command blocks, sizes taken 2026-10-08 ~17:45. Tier A now also covers staged node / bench binaries (`*-stage/<arm>/release/torus-node`, `release/bench-throughput`), `ubench_hasher` and `ubench.bin`. **Held:** `ozarchy-p2s0r-stage/b/` is in use by the c2h campaign running at update time (its own block, delete after c2h). Not listed: the `*c2h*` dirs; the `presuite-*` dirs (logs only, nothing deletable, as for `presuite-b8bf3e8a`).

* **Tier A (recommended)**: profiler output (`perf.data`, `perf-*.data`, `perf.folded`, perf script dumps `*script.gz`), staged binaries (`ubench.bin`, `ubench_adl`, `ubench_read_precompile_gas.bin`,
  `bin/` dirs) and generated `genesis-*val.json` (the harness regenerates it; the md5 of each is in the cell's `summary.json`). The numbers read from them are in the docs listed in `INDEX.md`.
  Without `perf.data` and the binary that produced it a profile cannot be re-expanded, so this is the "no more re-analysis" cut.
* **Tier B (optional)**: bulky raw logs and sampler dumps: `val*.log.gz` (raw node logs; the small `val*.log.excerpt` next to each is kept), `buckets.csv`, `tasks.txt`, `sampler.csv`
  and `*.a2l.json` (addr2line expansions of the perf dumps). Largest group by bytes; they are the only copy of the per-block detail behind a cell, so delete them only if no cell will be re-analysed.
* **Kept, not listed**: `summary.json`, `run.log`, `bench.log`, small logs / csv, `*.excerpt`, `campaign.sh`, analysis scripts and notes, and anything a doc cites by path:
  `liq-lines-val*.txt` and `thin-snap.jsonl` (up to 7.4 MiB each, named in section 23.2 / 24 of the ozarchy doc), `liq-stress.json`, `partial-at-stop/`, `drain-samples.jsonl`,
  `value-sum-val*.txt`, and the top-level analysis files. The 4 skipped dirs (`presuite-4164382d`, `presuite-76eb081c`, `presuite-ae767806`, `read-precompile-gas-ac`) are not in this list (s29: now inventoried; only `read-precompile-gas-ac` has deletable files, block `rpg-ac`).
* **Marked with a dagger (†)** below: the file is named in a plan doc's "raw files" sentence (`docs/plans/adl-budget.md` sections 11 and 13.7, `docs/plans/adl-dirty-check.md` Result).
  Deleting it leaves that sentence pointing at a removed file; the measured results in those docs do not depend on it.
* The `*-tools` dirs hold derived perf dumps (`*.exec-script.gz`, `*.exec-time-script.gz`, `*.inl.script.gz`, `*.a2l.json`) next to analysis scripts. Only the dumps are listed; the scripts stay.

**Grand total freed: Tier A 3.88 GiB (4,169,742,007 bytes); Tier B 4.42 GiB (4,741,801,673 bytes); A + B 8.30 GiB (8,911,543,680 bytes)**,
out of 8.69 GiB in the 211 inventoried dirs.

**Added s29: Tier A 12.51 GiB (13,430,049,752 bytes, 70 files, of which 999.5 MiB held for c2h); Tier B 1.15 GiB (1,238,410,434 bytes, 291 files); A + B 13.66 GiB (14,668,460,186 bytes)**, out of 13.82 GiB in the 64 dirs added.

## Per campaign

A campaign here is a group of dirs from the same run (for example all `ozarchy-mif-300m-*` cells). The command blocks further down are in this order.

| campaign | what | dirs | Tier A | Tier B | A + B |
|---|---|---|---|---|---|
| `14236fa` | Baseline `14236fa` (C3 + C4 + PF1 + cooldown fix) at 300 markets, with perf on r1/r2 | 3 | 126.4 MiB | 78.3 MiB | 204.7 MiB |
| `239ff69` | `239ff69` (P1-P4 + fix A) at 300 markets, trie off by default, perf on r1/r2 | 3 | 130.6 MiB | 76.5 MiB | 207.1 MiB |
| `4acdc59` | Step 2 (`4acdc59`: end_resident on a worker) vs main at 300 markets (run 4, valid) | 5 | 147.6 MiB | 130.0 MiB | 277.6 MiB |
| `4acdc59-void` | Voided runs 1-3 of the step 2 (`4acdc59`) 300-market campaign (a process was SIGKILLed in each) | 5 | - | 124.8 MiB | 124.8 MiB |
| `5524646` | Gate 2 at 10 markets without perf: `5524646` vs main `92a02ed` | 5 | - | 124.5 MiB | 124.5 MiB |
| `c7` | C6 + C7 (`82bd1a4`) at 300 markets, trie off, with perf on r1/r2 | 3 | 135.5 MiB | 76.7 MiB | 212.2 MiB |
| `90a752c` | M1 (`90a752c`) at 300 markets, trie off by default, perf on r1/r2 | 3 | 137.3 MiB | 76.2 MiB | 213.5 MiB |
| `action-results` | Per-action execution results (`9195c32`) at 300 markets: warm + r1 | 2 | - | 50.0 MiB | 50.0 MiB |
| `adlcells` | ADL budget proof cells (`perf/adl-budget` @ `6a25e20`): warm, S=400, S=750 with the value sum off | 5 | 112.1 MiB | 166.5 MiB | 278.6 MiB |
| `as-r1` | Anti-spam round 1 (branch antispam): control vs `ANTISPAM=1`, cap 400 and cap 20, no spam | 4 | - | 94.7 MiB | 94.7 MiB |
| `as-r2` | Anti-spam round 2: limits off/on, B throttle, 64 unfunded / funded cancel-spam keys at cap 20, C share 25% vs 100% | 9 | - | 217.1 MiB | 217.1 MiB |
| `as-nr` | Anti-spam no-regression A/B: main `79a3752` vs `feat/native-antispam` (`92a02ed`), cap 400, no spam | 5 | - | 118.8 MiB | 118.8 MiB |
| `as-r3-64` | Anti-spam round 3: 64 funded cancel-spam keys at cap 20, 60 s, C at 25%, `ANTISPAM` off / on | 3 | - | 66.6 MiB | 66.6 MiB |
| `as-r3-256` | Anti-spam round 3: 256 funded cancel-spam keys at cap 20, 120 s (plain, and `ANTISPAM=1` with `TORUS_ADDR_RATE_BUFFER=1000`) | 2 | - | 51.7 MiB | 51.7 MiB |
| `bblind` | Gate 2 with B-blind (`31cea69`) vs main `92a02ed`, 300 and 10 markets | 10 | - | 254.5 MiB | 254.5 MiB |
| `bd` | s94 batch cost: main `35e69b3` vs `92a02ed`, 300 markets, N=4 + budget 900 | 6 | 140.1 MiB | 146.8 MiB | 286.9 MiB |
| `c1ab` | C1 full-node A/B: `9c4be2c` (before C1) vs `81a9567` (C1), 10 markets | 6 | - | 156.4 MiB | 156.4 MiB |
| `c3pf1-10m` | C3 + PF1 (`d9ef4f7`) vs main at 10 markets, with and without `RETRY_BUSY` | 9 | - | 226.2 MiB | 226.2 MiB |
| `c3pf1-300m` | C3 + PF1 (`d9ef4f7`) vs main at 300 markets (`RETRY_BUSY=1`); both r1 cells carry perf data; main-r1 is the profile used in section 11 | 5 | 133.2 MiB | 132.2 MiB | 265.5 MiB |
| `c58775f` | Gate 2 at 10 markets: `c58775f` vs main `92a02ed`, both trie off (crab cells with perf) | 5 | 121.6 MiB | 123.7 MiB | 245.3 MiB |
| `feeddrain` | Live-feed idle check: oracle feed live through the drain (`5584880`), 300 markets | 1 | - | 27.0 MiB | 27.0 MiB |
| `gap-c7` | Crab vs main gap after C6 / C7: perf-script dumps and rust-source line extracts (read-only analysis) | 1 | 12.5 MiB | - | 12.5 MiB |
| `ipc` | IPC / cache-miss profile (perf record, one event at a time) crab vs main at 300 markets, trie off | 3 | 690.6 MiB | 77.7 MiB | 768.3 MiB |
| `liq` | Liquidation stress (`bench/liq-stress` @ `af8529e`): thin accounts + parity-signed shock, S=400 and S=750 | 3 | 84.1 MiB | 109.2 MiB | 193.2 MiB |
| `harness-cap-ab` | First matched-bench cells on ozarchy and the block-cap sweep on main (cap 400 default, 200, 50, 20); 10 markets, 120 s, rate 76,000 | 10 | - | 242.9 MiB | 242.9 MiB |
| `hostcheck` | Host check: main `92a02ed` alone at 300 markets, do validators die on this host? | 1 | - | 26.3 MiB | 26.3 MiB |
| `10m-gap` | Main `92a02ed` 10-market profile cell (+ warm) for the gap-outside-the-engine analysis | 2 | 62.7 MiB | 47.7 MiB | 110.4 MiB |
| `margin` | Margin phase breakdown: addr2line expansions of the c7 / c3pf1-main / p2s0 perf profiles, analysis scripts | 1 | 12.5 MiB | 19.4 MiB | 31.9 MiB |
| `mif` | Bench in-flight cap sweep (`--max-in-flight` N = 1/2/4, `OPEN_ORDER_BUDGET` 900) on main, 300 markets | 14 | - | 358.2 MiB | 358.2 MiB |
| `mif2a` | In-flight cap sweep block A: N = 8 / 16 + budget 900, base on main | 4 | - | 100.0 MiB | 100.0 MiB |
| `mif2b` | Crab (`59fa407`) vs main at N = 2 + budget 900, 300 markets | 5 | - | 125.6 MiB | 125.6 MiB |
| `mif3` | Crab (`59fa407`) plateau at N = 4 / 8 + budget 900 (the standard shape) | 3 | - | 75.1 MiB | 75.1 MiB |
| `p2s0` | Phase 2 step 0 profile at N=4 + budget 900: crab `59fa407` vs main, walk 0 / walk 10, perf on r2 / w10 | 6 | 292.2 MiB | 152.9 MiB | 445.2 MiB |
| `pf1` | PF1 gate at 10 markets: `0ebfd71` (crab + PF1) vs main `92a02ed` | 5 | 128.2 MiB | 119.4 MiB | 247.6 MiB |
| `prof10` | Exec-path CPU profile at 10 markets: crab `d52a33f` vs main `92a02ed` (perf on val0) | 3 | 141.0 MiB | 75.6 MiB | 216.6 MiB |
| `s2` | Item 6 sync point 2: `perf/item6-phase1` (`81a9567`) vs `merge/item6-sync2` (`cea1254`), plus a 256-key spam cell at cap 20 | 6 | - | 157.6 MiB | 157.6 MiB |
| `step2-window` | Step 2 window analysis: perf-script dumps of the 90a752c cells, window scripts | 1 | 7.4 MiB | - | 7.4 MiB |
| `trie0` | Trie maintenance off at 300 markets: crab `51051c9` vs main `92a02ed` | 5 | - | 129.5 MiB | 129.5 MiB |
| `walk` | Moving prices (walk 10 bp vs 0) at 300 markets, crab-side `5584880` vs main | 7 | - | 185.9 MiB | 185.9 MiB |
| `rpg` | Read precompile gas microbench (`ubench_read_precompile_gas`), 3 repetitions | 1 | 426.8 MiB | - | 426.8 MiB |
| `ubench-adl-c2` | ubench_adl at W = 100k-630k on C2 (cases 1-4, 3 runs each, perf on c1/c2/c4) | 1 | 20.6 MiB | - | 20.6 MiB |
| `ubench-adl-dc` | ubench_adl case 3 A/B: drain dirty check without `layer_touches` (A = before, B = after), 3 runs each | 1 | 873.6 MiB | - | 873.6 MiB |
| `ubench-adl-s99` | ubench_adl re-measure at W = 100,000 with holder units (cases 1-4, perf on c1/c3/c4) | 1 | 40.1 MiB | - | 40.1 MiB |
| `p2s0b` | Phase 2 step 0 (Gate 0): step 0 `707f132f` vs base main `d3ba3c0a` at N=4 + budget 900 (s-prof perf, hasher binary, staged binaries) | 11 | 2070.1 MiB | 232.6 MiB | 2302.7 MiB |
| `p2s0r` | Regression check main `d3ba3c0a` vs `35e69b3` (arms A-D, interleaved), staged binaries | 12 | 2998.3 MiB | 273.5 MiB | 3271.8 MiB |
| `p2s0x` | Bisect of the `d3ba3c0a` vs `35e69b3` regression (a, p0-p3, b, mirrored), staged binaries | 14 | 4995.8 MiB | 334.2 MiB | 5330.0 MiB |
| `p2s0y` | Perf A/B p2 `2ebe1a14` vs p3 `9e695364` (prof / xstat cells; first-launch rec1 cells superseded) | 12 | 521.6 MiB | 340.8 MiB | 862.4 MiB |
| `read-gas-stall` | Write stalls and the book CF SST target (`bench/read-gas-stall`), staged `ubench.bin` | 1 | 77.1 MiB | - | 77.1 MiB |
| `rpg-ac` | Read precompile gas after the s99 decisions (before / after / review-* runs), staged `ubench.bin` | 1 | 2144.9 MiB | - | 2144.9 MiB |
| **total (2026-10-07)** | | 183 | **3.88 GiB** | **4.42 GiB** | **8.30 GiB** |
| **total added s29** | | 51 | **12.51 GiB** | **1.15 GiB** | **13.66 GiB** |

## Largest Tier A files

| size | dir | file |
|---|---|---|
| 426.8 MiB | `read-precompile-gas` | `ubench_read_precompile_gas.bin` |
| 422.8 MiB | `ubench-adl-dirty-check` | `bin/B/ubench_adl` |
| 422.8 MiB | `ubench-adl-dirty-check` | `bin/A/ubench_adl` |
| 242.3 MiB | `ozarchy-ipc-main` | `perf-w1.data` |
| 238.1 MiB | `ozarchy-ipc-crab` | `perf-w1.data` |
| 107.8 MiB | `ozarchy-ipc-main` | `perf-w2.data` |
| 102.4 MiB | `ozarchy-ipc-crab` | `perf-w2.data` |
| 50.1 MiB | `ozarchy-4acdc59-300m-main-r2` | `perf.data` |
| 47.6 MiB | `ozarchy-82bd1a4-c7-r2` | `perf.data` |
| 47.5 MiB | `ozarchy-90a752c-r2` | `perf.data` |
| 43.9 MiB | `ozarchy-4acdc59-300m-crab-r2` | `perf.data` |
| 43.5 MiB | `ozarchy-239ff69-r1` | `perf.data` |

Largest Tier A files added in s29: the 20 staged `torus-node` (541.3-542.1 MiB) and `bench-throughput` (457.6 MiB) binaries in `ozarchy-p2s0b-stage`, `ozarchy-p2s0r-stage` and `ozarchy-p2s0x-stage`, and the 5 `ubench.bin` (429 MiB each) in `read-precompile-gas-ac`; the largest perf file is `ozarchy-p2s0y-300m-p3-rec1/perf-w1.data` (82.4 MiB).

Tier A files named in plan docs (†): `ubench-adl-c2/perf-c1.data`, `ubench-adl-c2/perf-c2.data`, `ubench-adl-c2/perf-c4.data`, `ubench-adl-dirty-check/perf-c3.B.data`, `ubench-adl-s99/perf-c1.data`, `ubench-adl-s99/perf-c3.data`, `ubench-adl-s99/perf-c4.data`; s29: `read-gas-stall/compact/ubench.bin`, `read-gas-stall/nocompact/ubench.bin`, `read-gas-stall/bin-v1/compact/ubench.bin`, `read-gas-stall/bin-v1/nocompact/ubench.bin` (named as dirs in `docs/perf/read-precompile-gas.md`, section 'Write stalls and the book CF's SST target').

## Commands, one block per campaign

Literal absolute paths only: no variables, no globs, no brace expansion. Each `rm --` names single files under the campaign's own dirs. Tier A and Tier B are separate commands. Paste one block at a time; the shell stops at the first error only if you add `set -e`, so check the exit code.

### `14236fa` (A 126.4 MiB, B 78.3 MiB; dirs: `ozarchy-14236fa-300m-r1`, `ozarchy-14236fa-300m-r2`, `ozarchy-14236fa-300m-warm`)

Tier A, 126.4 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/perf.folded'
```

Tier B, 78.3 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-14236fa-300m-warm/val2.log.gz'
```

### `239ff69` (A 130.6 MiB, B 76.5 MiB; dirs: `ozarchy-239ff69-r1`, `ozarchy-239ff69-r2`, `ozarchy-239ff69-warm`)

Tier A, 130.6 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/perf.folded'
```

Tier B, 76.5 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-239ff69-warm/val2.log.gz'
```

### `4acdc59` (A 147.6 MiB, B 130.0 MiB; dirs: `ozarchy-4acdc59-300m-crab-r1`, `ozarchy-4acdc59-300m-crab-r2`, `ozarchy-4acdc59-300m-main-r1` ...)

Tier A, 147.6 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/perf.folded'
```

Tier B, 130.0 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm/val2.log.gz'
```

### `4acdc59-void` (A 0 B, B 124.8 MiB; dirs: `ozarchy-4acdc59-300m-crab-r1-VOID-run2`, `ozarchy-4acdc59-300m-crab-r1-VOID-run3`, `ozarchy-4acdc59-300m-warm-VOID-run2` ...)

Tier B, 124.8 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run2/val2-died.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run3/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run3/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run3/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run3/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run3/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-crab-r1-VOID-run3/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run3/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run3/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run3/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run3/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run3/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-run3/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-val1died/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-val1died/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-val1died/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-val1died/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-val1died/val1-died.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-4acdc59-300m-warm-VOID-val1died/val2.log.gz'
```

### `5524646` (A 0 B, B 124.5 MiB; dirs: `ozarchy-5524646-10m-crab-r1`, `ozarchy-5524646-10m-crab-r2`, `ozarchy-5524646-10m-main-r1` ...)

Tier B, 124.5 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-5524646-10m-warm/val2.log.gz'
```

### `c7` (A 135.5 MiB, B 76.7 MiB; dirs: `ozarchy-82bd1a4-c7-r1`, `ozarchy-82bd1a4-c7-r2`, `ozarchy-82bd1a4-c7-warm`)

Tier A, 135.5 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/perf.folded'
```

Tier B, 76.7 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-82bd1a4-c7-warm/val2.log.gz'
```

### `90a752c` (A 137.3 MiB, B 76.2 MiB; dirs: `ozarchy-90a752c-r1`, `ozarchy-90a752c-r2`, `ozarchy-90a752c-warm`)

Tier A, 137.3 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/perf.folded'
```

Tier B, 76.2 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-90a752c-warm/val2.log.gz'
```

### `action-results` (A 0 B, B 50.0 MiB; dirs: `ozarchy-action-results-r1`, `ozarchy-action-results-warm`)

Tier B, 50.0 MiB, 12 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-action-results-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-action-results-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-action-results-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-action-results-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-action-results-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-action-results-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-action-results-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-action-results-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-action-results-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-action-results-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-action-results-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-action-results-warm/val2.log.gz'
```

### `adlcells` (A 112.1 MiB, B 166.5 MiB; dirs: `ozarchy-adlcells-300m-s400`, `ozarchy-adlcells-300m-s400vs`, `ozarchy-adlcells-300m-s750` ...)

Tier A, 112.1 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/genesis-3val.json'
```

Tier B, 166.5 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400vs/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400vs/partial-at-stop/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400vs/partial-at-stop/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400vs/partial-at-stop/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400vs/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s400vs/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-s750vs/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-adlcells-300m-warm/val2.log.gz'
```

### `as-r1` (A 0 B, B 94.7 MiB; dirs: `ozarchy-as-ctl-cap20-r1`, `ozarchy-as-ctl-cap400-r1`, `ozarchy-as-on-cap400-r1` ...)

Tier B, 94.7 MiB, 20 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap20-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap20-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap20-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap20-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap20-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap400-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap400-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap400-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap400-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-ctl-cap400-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-on-cap400-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-on-cap400-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-on-cap400-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-on-cap400-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-on-cap400-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-warm-60s/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-warm-60s/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as-warm-60s/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-warm-60s/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as-warm-60s/val2.log.gz'
```

### `as-r2` (A 0 B, B 217.1 MiB; dirs: `ozarchy-as2-off-cap400`, `ozarchy-as2-on-cap400`, `ozarchy-as2-spam-funded-c100-cap20` ...)

Tier B, 217.1 MiB, 45 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-as2-off-cap400/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-off-cap400/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-off-cap400/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-off-cap400/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-off-cap400/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-on-cap400/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-on-cap400/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-on-cap400/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-on-cap400/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-on-cap400/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20-x/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20-x/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20-x/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20-x/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20-x/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c100-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c25-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c25-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c25-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c25-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-funded-c25-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-unfunded-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-unfunded-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-unfunded-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-unfunded-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-spam-unfunded-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400-x/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400-x/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400-x/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400-x/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400-x/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-throttle-cap400/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-warm-60s/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-warm-60s/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as2-warm-60s/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-warm-60s/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as2-warm-60s/val2.log.gz'
```

### `as-nr` (A 0 B, B 118.8 MiB; dirs: `ozarchy-as3-nr-branch-r1`, `ozarchy-as3-nr-branch-r2`, `ozarchy-as3-nr-main-r1` ...)

Tier B, 118.8 MiB, 25 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-branch-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-nr-warm/val2.log.gz'
```

### `as-r3-64` (A 0 B, B 66.6 MiB; dirs: `ozarchy-as3-spam-funded-c25-cap20`, `ozarchy-as3-spam-funded-on-cap20`, `ozarchy-as3-warm-60s`)

Tier B, 66.6 MiB, 15 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-c25-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-c25-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-c25-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-c25-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-c25-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-on-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-on-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-on-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-on-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded-on-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-warm-60s/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-warm-60s/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-warm-60s/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-warm-60s/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-warm-60s/val2.log.gz'
```

### `as-r3-256` (A 0 B, B 51.7 MiB; dirs: `ozarchy-as3-spam-funded256-B1000-cap20`, `ozarchy-as3-spam-funded256-cap20`)

Tier B, 51.7 MiB, 10 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-B1000-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-B1000-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-B1000-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-B1000-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-B1000-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-as3-spam-funded256-cap20/val2.log.gz'
```

### `bblind` (A 0 B, B 254.5 MiB; dirs: `ozarchy-bblind-31cea69-10m-crab-r1`, `ozarchy-bblind-31cea69-10m-crab-r2`, `ozarchy-bblind-31cea69-10m-main-r1` ...)

Tier B, 254.5 MiB, 60 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-10m-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bblind-31cea69-300m-warm/val2.log.gz'
```

### `bd` (A 140.1 MiB, B 146.8 MiB; dirs: `ozarchy-bd-300m-crab-r1`, `ozarchy-bd-300m-crab-r2`, `ozarchy-bd-300m-crab-warm` ...)

Tier A, 140.1 MiB, 5 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/genesis-3val.json'
```

Tier B, 146.8 MiB, 35 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-crab-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-300m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-smoke-main/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-smoke-main/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-bd-smoke-main/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-smoke-main/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-bd-smoke-main/val2.log.gz'
```

### `c1ab` (A 0 B, B 156.4 MiB; dirs: `ozarchy-c1ab-c1-r1`, `ozarchy-c1ab-c1-r2`, `ozarchy-c1ab-pre-r1` ...)

Tier B, 156.4 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-c1-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-smoke/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-smoke/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-smoke/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-smoke/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-pre-smoke/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c1ab-warm/val2.log.gz'
```

### `c3pf1-10m` (A 0 B, B 226.2 MiB; dirs: `ozarchy-c3pf1-10m-retry0-c3pf1-r1`, `ozarchy-c3pf1-10m-retry0-c3pf1-r2`, `ozarchy-c3pf1-10m-retry0-main-r1` ...)

Tier B, 226.2 MiB, 54 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-c3pf1-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry0-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-c3pf1-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-retry1-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-10m-warm/val2.log.gz'
```

### `c3pf1-300m` (A 133.2 MiB, B 132.2 MiB; dirs: `ozarchy-c3pf1-300m-c3pf1-r1`, `ozarchy-c3pf1-300m-c3pf1-r2`, `ozarchy-c3pf1-300m-main-r1` ...)

Tier A, 133.2 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/perf.folded'
```

Tier B, 132.2 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-c3pf1-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c3pf1-300m-warm/val2.log.gz'
```

### `c58775f` (A 121.6 MiB, B 123.7 MiB; dirs: `ozarchy-c58775f-10m-crab-r1`, `ozarchy-c58775f-10m-crab-r2`, `ozarchy-c58775f-10m-main-r1` ...)

Tier A, 121.6 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/perf.folded'
```

Tier B, 123.7 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-c58775f-10m-warm/val2.log.gz'
```

### `feeddrain` (A 0 B, B 27.0 MiB; dirs: `ozarchy-feeddrain-5584880-300m-r1`)

Tier B, 27.0 MiB, 6 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-feeddrain-5584880-300m-r1/val2.log.gz'
```

### `gap-c7` (A 12.5 MiB, B 0 B; dirs: `ozarchy-gap-after-c7-tools`)

Tier A, 12.5 MiB, 2 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-gap-after-c7-tools/ozarchy-14236fa-300m-r1.exec-script.gz' \
  '/home/oz/bench-results-matched/ozarchy-gap-after-c7-tools/ozarchy-c3pf1-300m-main-r1.exec-script.gz'
```

### `ipc` (A 690.6 MiB, B 77.7 MiB; dirs: `ozarchy-ipc-crab`, `ozarchy-ipc-main`, `ozarchy-ipc-warm`)

Tier A, 690.6 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/perf-w2.data' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/perf-w2.data'
```

Tier B, 77.7 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-crab/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-main/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-ipc-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-ipc-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-ipc-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-ipc-warm/val2.log.gz'
```

### `liq` (A 84.1 MiB, B 109.2 MiB; dirs: `ozarchy-liq-300m-s400`, `ozarchy-liq-300m-s750`, `ozarchy-liq-300m-warm`)

Tier A, 84.1 MiB, 3 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/genesis-3val.json' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/genesis-3val.json'
```

Tier B, 109.2 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s400/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-s750/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-liq-300m-warm/val2.log.gz'
```

### `harness-cap-ab` (A 0 B, B 242.9 MiB; dirs: `ozarchy-main-10m-r1`, `ozarchy-main-10m-r2`, `ozarchy-main-cap20-10m-r1` ...)

Tier B, 242.9 MiB, 50 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-10m-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap20-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap20-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap20-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap20-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap20-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap200-10m-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap50-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap50-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-cap50-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap50-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-cap50-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap20-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap20-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap20-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap20-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap20-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap400-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap400-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap400-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap400-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap400-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap50-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap50-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap50-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap50-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap50-10m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap65-10m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap65-10m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap65-10m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap65-10m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-rb-cap65-10m-r1/val2.log.gz'
```

### `hostcheck` (A 0 B, B 26.3 MiB; dirs: `ozarchy-main-300m-hostcheck-c1`)

Tier B, 26.3 MiB, 6 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-main-300m-hostcheck-c1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-300m-hostcheck-c1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-300m-hostcheck-c1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-main-300m-hostcheck-c1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-300m-hostcheck-c1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-300m-hostcheck-c1/val2.log.gz'
```

### `10m-gap` (A 62.7 MiB, B 47.7 MiB; dirs: `ozarchy-main-prof-10m`, `ozarchy-main-prof-10m-warm`)

Tier A, 62.7 MiB, 2 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/perf.folded'
```

Tier B, 47.7 MiB, 12 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-main-prof-10m/val2.log.gz'
```

### `margin` (A 12.5 MiB, B 19.4 MiB; dirs: `ozarchy-margin-c7-tools`)

Tier A, 12.5 MiB, 2 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-82bd1a4-c7-r1.exec-script.gz' \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-82bd1a4-c7-r2.exec-script.gz'
```

Tier B, 19.4 MiB, 6 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-82bd1a4-c7-r1.exec-script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-82bd1a4-c7-r2.exec-script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-c3pf1-300m-main-r1.exec-script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-p2s0-300m-crab-r2.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-p2s0-300m-crab-w10.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-margin-c7-tools/ozarchy-p2s0-300m-main-r2.inl.script.gz.a2l.json'
```

### `mif` (A 0 B, B 358.2 MiB; dirs: `ozarchy-mif-300m-base`, `ozarchy-mif-300m-base-b900`, `ozarchy-mif-300m-base-r2` ...)

Tier B, 358.2 MiB, 84 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r3/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r3/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r3/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r3/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r3/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base-r3/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-base/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r3/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r3/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r3/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r3/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r3/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900-r3/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-n4/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-tailcost/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-tailcost/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-tailcost/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-tailcost/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-tailcost/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-tailcost/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif-300m-warm/val2.log.gz'
```

### `mif2a` (A 0 B, B 100.0 MiB; dirs: `ozarchy-mif2-300m-base`, `ozarchy-mif2-300m-n16-b900`, `ozarchy-mif2-300m-n8-b900` ...)

Tier B, 100.0 MiB, 24 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-base/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-base/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-base/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-base/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-base/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-base/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n16-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n16-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n16-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n16-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n16-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n16-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n8-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n8-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n8-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n8-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n8-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-n8-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-warm/val2.log.gz'
```

### `mif2b` (A 0 B, B 125.6 MiB; dirs: `ozarchy-mif2-300m-crab-r1`, `ozarchy-mif2-300m-crab-r2`, `ozarchy-mif2-300m-crab-warm` ...)

Tier B, 125.6 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-crab-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif2-300m-main-r2/val2.log.gz'
```

### `mif3` (A 0 B, B 75.1 MiB; dirs: `ozarchy-mif3-300m-crab-n4-b900`, `ozarchy-mif3-300m-crab-n8-b900`, `ozarchy-mif3-300m-crab-warm`)

Tier B, 75.1 MiB, 18 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n4-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n4-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n4-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n4-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n4-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n4-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n8-b900/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n8-b900/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n8-b900/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n8-b900/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n8-b900/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-n8-b900/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-mif3-300m-crab-warm/val2.log.gz'
```

### `p2s0` (A 292.2 MiB, B 152.9 MiB; dirs: `ozarchy-p2s0-300m-crab-r1`, `ozarchy-p2s0-300m-crab-r2`, `ozarchy-p2s0-300m-crab-w10` ...)

Tier A, 292.2 MiB, 16 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/ozarchy-p2s0-300m-crab-r2.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/perf.exec.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/drain/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/ozarchy-p2s0-300m-crab-w10.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/perf.drain.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/perf.drain.exec.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/perf.drain.folded' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/perf.exec.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/ozarchy-p2s0-300m-main-r2.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/perf.exec.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/perf.folded'
```

Tier B, 152.9 MiB, 36 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-w10/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-crab-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0-300m-main-r2/val2.log.gz'
```

### `pf1` (A 128.2 MiB, B 119.4 MiB; dirs: `ozarchy-pf1-r1`, `ozarchy-pf1-r2`, `ozarchy-pf1-warm` ...)

Tier A, 128.2 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/perf.folded'
```

Tier B, 119.4 MiB, 25 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-pf1main-r2/val2.log.gz'
```

### `prof10` (A 141.0 MiB, B 75.6 MiB; dirs: `ozarchy-prof10-crab`, `ozarchy-prof10-main`, `ozarchy-prof10-warm`)

Tier A, 141.0 MiB, 5 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-prof10-warm/perf.data'
```

Tier B, 75.6 MiB, 15 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-crab/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-main/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-prof10-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-prof10-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-prof10-warm/val2.log.gz'
```

### `s2` (A 0 B, B 157.6 MiB; dirs: `ozarchy-s2-item6-r1`, `ozarchy-s2-item6-r2`, `ozarchy-s2-sync2-r1` ...)

Tier B, 157.6 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-item6-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-spam256-cap20/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-spam256-cap20/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-spam256-cap20/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-spam256-cap20/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-sync2-spam256-cap20/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-s2-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-s2-warm/val2.log.gz'
```

### `step2-window` (A 7.4 MiB, B 0 B; dirs: `ozarchy-step2-tools`)

Tier A, 7.4 MiB, 2 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-step2-tools/ozarchy-90a752c-r1.exec-time-script.gz' \
  '/home/oz/bench-results-matched/ozarchy-step2-tools/ozarchy-90a752c-r2.exec-time-script.gz'
```

### `trie0` (A 0 B, B 129.5 MiB; dirs: `ozarchy-trie0-crab-r1`, `ozarchy-trie0-crab-r2`, `ozarchy-trie0-main-r1` ...)

Tier B, 129.5 MiB, 30 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-crab-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-main-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-trie0-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-trie0-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-trie0-warm/val2.log.gz'
```

### `walk` (A 0 B, B 185.9 MiB; dirs: `ozarchy-walk-5584880-300m-m-r1`, `ozarchy-walk-5584880-300m-m-r2`, `ozarchy-walk-5584880-300m-w0-r1` ...)

Tier B, 185.9 MiB, 42 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-m-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w0-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-w10-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-walk-5584880-300m-warm/val2.log.gz'
```

### `rpg` (A 426.8 MiB, B 0 B; dirs: `read-precompile-gas`)

Tier A, 426.8 MiB, 1 files:

```
rm -- \
  '/home/oz/bench-results-matched/read-precompile-gas/ubench_read_precompile_gas.bin'
```

### `ubench-adl-c2` (A 20.6 MiB, B 0 B; dirs: `ubench-adl-c2`)

Tier A, 20.6 MiB, 3 files:

```
rm -- \
  '/home/oz/bench-results-matched/ubench-adl-c2/perf-c1.data' \
  '/home/oz/bench-results-matched/ubench-adl-c2/perf-c2.data' \
  '/home/oz/bench-results-matched/ubench-adl-c2/perf-c4.data'
```

### `ubench-adl-dc` (A 873.6 MiB, B 0 B; dirs: `ubench-adl-dirty-check`)

Tier A, 873.6 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ubench-adl-dirty-check/analysis/c3B.txt.gz' \
  '/home/oz/bench-results-matched/ubench-adl-dirty-check/bin/A/ubench_adl' \
  '/home/oz/bench-results-matched/ubench-adl-dirty-check/bin/B/ubench_adl' \
  '/home/oz/bench-results-matched/ubench-adl-dirty-check/perf-c3.B.data'
```

### `ubench-adl-s99` (A 40.1 MiB, B 0 B; dirs: `ubench-adl-s99`)

Tier A, 40.1 MiB, 3 files:

```
rm -- \
  '/home/oz/bench-results-matched/ubench-adl-s99/perf-c1.data' \
  '/home/oz/bench-results-matched/ubench-adl-s99/perf-c3.data' \
  '/home/oz/bench-results-matched/ubench-adl-s99/perf-c4.data'
```

### `p2s0b` (A 2070.1 MiB, B 232.6 MiB; dirs: `ozarchy-p2s0b-300m-b-r1`, `ozarchy-p2s0b-300m-b-r2`, `ozarchy-p2s0b-300m-s-byid` ...)

`ozarchy-p2s0b-stage/step0/release/torus-node` has a second hard link outside `~/bench-results-matched` (link count 2; not found under `~/.cargo-target-*`), so deleting it frees no space until that link goes too.

Tier A, 2070.1 MiB, 9 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/ozarchy-p2s0b-300m-s-prof.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/perf.exec.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/perf.folded' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-hasher/ubench_hasher' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-stage/base/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-stage/base/release/torus-node' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-stage/step0/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-stage/step0/release/torus-node'
```

Tier B, 232.6 MiB, 53 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-b-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-byid/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-byid/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-byid/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-byid/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-byid/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-byid/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-prof/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-w10/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-w10/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-w10/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-w10/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-w10/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-w10/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-300m-s-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-smoke-10m-byid/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-smoke-10m-byid/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-smoke-10m-byid/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-smoke-10m-byid/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0b-smoke-10m-byid/val2.log.gz'
```

### `p2s0r` (A 2998.3 MiB, B 273.5 MiB; dirs: `ozarchy-p2s0r-300m-a-r1`, `ozarchy-p2s0r-300m-a-r2`, `ozarchy-p2s0r-300m-b-r1` ...)

Tier A, 1998.8 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-stage/a/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-stage/a/release/torus-node' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-stage/d/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-stage/d/release/torus-node'
```

**Held: in use by the c2h campaign running at archive time** (`ozarchy-c2h-campaign.sh` arm `b` = `ozarchy-p2s0r-stage/b`, and `ozarchy-c2h-build.sh` / `ozarchy-c2h2-build.sh` copy its `bench-throughput`). Delete only after the c2h campaign is finished and written up.

Tier A, held, 999.5 MiB, 2 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-stage/b/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-stage/b/release/torus-node'
```

Tier B, 273.5 MiB, 64 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-a-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-b-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-c-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-300m-d-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-a/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-a/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-a/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-a/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-a/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-c/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-c/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-c/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-c/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0r-smoke-10m-c/val2.log.gz'
```

### `p2s0x` (A 4995.8 MiB, B 334.2 MiB; dirs: `ozarchy-p2s0x-300m-a-r1`, `ozarchy-p2s0x-300m-a-r2`, `ozarchy-p2s0x-300m-b-r1` ...)

Tier A, 4995.8 MiB, 10 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/a/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/a/release/torus-node' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p0/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p0/release/torus-node' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p1/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p1/release/torus-node' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p2/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p2/release/torus-node' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p3/release/bench-throughput' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-stage/p3/release/torus-node'
```

Tier B, 334.2 MiB, 78 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-a-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-b-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p0-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p1-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p2-r2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0x-300m-p3-r2/val2.log.gz'
```

### `p2s0y` (A 521.6 MiB, B 340.8 MiB; dirs: `ozarchy-p2s0y-300m-p2-prof1`, `ozarchy-p2s0y-300m-p2-prof2`, `ozarchy-p2s0y-300m-p2-rec1` ...)

The first launch's cells (`ozarchy-p2s0y-300m-p2-rec1`, `ozarchy-p2s0y-300m-p3-rec1`, run with the 5-event inherited `perf record`) are distorted reference cells; their `perf-w1.data` / `perf-w2.data` (and the analysis) are superseded by the prof / xstat cells (section 28.4). The first launch's warm cell `ozarchy-p2s0y-300m-p2-warm` (superseded by `p2-warm2`) holds no perf data; only its Tier B files are listed.

Tier A, 521.6 MiB, 36 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/perf-w2.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.L1-dcache-load-misses.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.instructions.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.L1-dcache-load-misses.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.instructions.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/perf.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/perf-w2.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.L1-dcache-load-misses.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.instructions.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/perf-w1.data' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.L1-dcache-load-misses.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.cycles.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.instructions.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz'
```

Tier B, 340.8 MiB, 96 files:

```
rm -- \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof1/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-prof2/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-rec1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-warm2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.L1-dcache-load-misses.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.instructions.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat1/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.L1-dcache-load-misses.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.instructions.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p2-xstat2/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof1/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-prof2/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-rec1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.L1-dcache-load-misses.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.instructions.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat1/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/buckets.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/sampler.csv' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/tasks.txt' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/val0.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/val1.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/val2.log.gz' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.L1-dcache-load-misses.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.cycles.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.instructions.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.ls_dmnd_fills_from_sys.ext_cache_local.inl.script.gz.a2l.json' \
  '/home/oz/bench-results-matched/ozarchy-p2s0y-300m-p3-xstat2/w1.ls_dmnd_fills_from_sys.mem_io_local.inl.script.gz.a2l.json'
```

### `read-gas-stall` (A 77.1 MiB, B 0 B; dirs: `read-gas-stall`)

The four `ubench.bin` (†) are named in `docs/perf/read-precompile-gas.md` ('binaries and the nocompact patch in `compact/`, `nocompact/`, `bin-v1/`'); `nocompact/nocompact.patch` is kept.

Tier A, 77.1 MiB, 4 files:

```
rm -- \
  '/home/oz/bench-results-matched/read-gas-stall/bin-v1/compact/ubench.bin' \
  '/home/oz/bench-results-matched/read-gas-stall/bin-v1/nocompact/ubench.bin' \
  '/home/oz/bench-results-matched/read-gas-stall/compact/ubench.bin' \
  '/home/oz/bench-results-matched/read-gas-stall/nocompact/ubench.bin'
```

### `rpg-ac` (A 2144.9 MiB, B 0 B; dirs: `read-precompile-gas-ac`)

Tier A, 2144.9 MiB, 5 files:

```
rm -- \
  '/home/oz/bench-results-matched/read-precompile-gas-ac/after/ubench.bin' \
  '/home/oz/bench-results-matched/read-precompile-gas-ac/before/ubench.bin' \
  '/home/oz/bench-results-matched/read-precompile-gas-ac/review-after/ubench.bin' \
  '/home/oz/bench-results-matched/read-precompile-gas-ac/review-before/ubench.bin' \
  '/home/oz/bench-results-matched/read-precompile-gas-ac/review-nocompact/ubench.bin'
```

## Grand total freed

* Tier A: **3.88 GiB** (4,169,742,007 bytes), 88 files
* Tier B: **4.42 GiB** (4,741,801,673 bytes), 1008 files
* Tier A + B: **8.30 GiB** (8,911,543,680 bytes), out of 8.69 GiB in the 211 inventoried dirs
* Added s29: Tier A **12.51 GiB** (13,430,049,752 bytes), 70 files, of which 999.5 MiB (`ozarchy-p2s0r-stage/b/`, 2 files) held until the c2h campaign is done
* Added s29: Tier B **1.15 GiB** (1,238,410,434 bytes), 291 files
* Added s29: Tier A + B **13.66 GiB** (14,668,460,186 bytes), out of 13.82 GiB in the 64 dirs added

## After the deletes

The `bin/A` and `bin/B` dirs of `ubench-adl-dirty-check` are left empty and can stay. Run `du -sh /home/oz/bench-results-matched` to see the real saving.

s29: deleting the staged binaries leaves empty `release/` dirs under the `*-stage` dirs; they can stay. `ozarchy-p2s0b-stage/step0/release/torus-node` has a second hard link elsewhere, so its 541.9 MiB are freed only when that link goes too.
