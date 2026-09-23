# s63 campaign tooling

Host-specific copies of the scripts that drove the s63 campaigns. The live copies
run from `~/bench-results-matched/`; paths inside are absolute for this host.

| Script | Role |
| --- | --- |
| `ab-driver.sh CAMPAIGN_DIR` | Sequential A/B driver. Reads `CAMPAIGN_DIR/arms.conf` (tab-separated `arm artifacts_dir extra_env` lines plus one `ORDER r1:armA r1:armB …` line). Keeps every cell, runs `hostsampler.sh` per cell and prunes each cell's devnet DB once its evidence is retained. Uses a per-campaign copy of `run_cell.py`, which puts the DB next to itself. Harness worktree: `WT=` near the top (now `wt/s63-body-fetch`). |
| `bench-guard.sh` | Sourced by the driver. One global lock (`~/bench-results-matched/.bench-global.lock`) so only one driver runs at a time; `bench_wait_quiet` waits until no node, generator, cargo or rustc process exists before each cell. |
| `hostsampler.sh OUT` | Per-second host sampler for one cell: UDP counters, per-socket drops, per-thread pidstat, host pidstat, vmstat, iostat, meminfo. |
| `prune-cell.sh LABEL…` | Deletes a cell's disposable devnet RocksDB (`$CAMPAIGN_DIR/<label>/data`) only if the summary, `val*.log.gz` and raw `run/` logs exist and agreement is AGREE. Writes `<label>.retention.json`. |
| `compare.py` | Per-cell and per-build table (gates, matched/s incl. steady window, block timing, RSS). Edit `BUILDS`, `COMMIT` and the glob per campaign. |

Traps these encode: `pgrep -x` sees only 15 characters (`bench-throughpu`);
`pgrep -f <string>` from a shell whose argv contains the string matches that
shell; `run_cell.py` needs `6 + 0.2 × duration` GiB free before each cell.
