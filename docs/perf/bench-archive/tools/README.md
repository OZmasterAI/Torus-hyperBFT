# Bench tools archive (ozarchy `~/bench-results-matched`, copied 2026-10-08, s29)

Copies of the scripts and patches that lived only in ozarchy's `/home/oz/bench-results-matched` and in no git
repo: campaign, build and launch drivers (top level `*-campaign.sh`, `*-build.sh`, `*-launch.sh`), analysis and
sidecar scripts from the `*-tools` / `ubench-*` / `presuite-*` dirs, `ozarchy-p2s0b-spawn/spawn.c` and
`read-gas-stall/nocompact/nocompact.patch` (the temporary no-compaction patch of the stall campaign). Paths are
the same as under `~/bench-results-matched`, so a doc that cites `ozarchy-p2s0y-tools/p2s0y.py` finds it at
`tools/ozarchy-p2s0y-tools/p2s0y.py`.

Copied as they were run: hard-coded paths (`/home/oz/...`, worktrees, staged binaries) point at ozarchy at run
time and are not maintained. The matched-bench harness itself (`run-cell.sh`, `summarize.py`, `detach.sh`) is in
`tools/matched-bench/` of the repo, not here. Data, perf dumps and logs are not copied (see `../INDEX.md` and
`../DELETABLE.md`).
