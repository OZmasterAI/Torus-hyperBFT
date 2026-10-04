# Matched-bench campaigns (A/B of node builds)

A campaign runs the same matched-bench cell (`../run-cell.sh`: 3-validator devnet
on one host, fixed load, matched/s from node counters) for two or more node builds
("arms"), interleaved, several times each, and scores them against each other. The
scripts here drive it end to end; the per-cell runner and its `summary.json` are
documented in `../README.md`.

> **WARNING: test keys only.** `devnet/wsl/env.sh` (validator consensus keys
> `01..`, `02..`, `03..`) and `devnet/wsl/bench-validator-keys.json` (validator EVM
> keys, hardhat/anvil mnemonic `test test ... junk`, indices 100-102) are publicly
> known test keys. They exist so the bench devnet can sign. Never use them, or a
> genesis built from them, on a real network.

## Files

| File | Role |
| --- | --- |
| `ab-driver.sh CAMPAIGN_DIR` | Runs the cells in `arms.conf`'s ORDER one after another: global lock, quiet-host checks, host sampler, `run_cell.py`, `progress.tsv` row, then the pruners. Skips labels that already have a results dir, so a relaunch resumes. `DRY_RUN=1` prints each cell's command and exits. |
| `run_cell.py` | One cell: waits for an idle host, checks ports and free disk, writes `<label>.manifest.json` (args, harness commit, binary sha256s), runs `$HARNESS_WT/tools/matched-bench/run-cell.sh` with a clean env. |
| `start-warmup.sh CAMPAIGN_DIR` | Detached launch of the `r0` items of ORDER (unscored warm-up), quiet checks relaxed for r0 only. Done marker `warmup.done`. |
| `start-rounds.sh CAMPAIGN_DIR` | Detached launch of the measured rounds (ORDER minus r0), every quiet check on. Done marker `campaign.done`. |
| `score.py [CAMPAIGN_DIR] [LABEL...]` | Per cell, per arm (mean, sd) and paired per-round deltas vs the `main*` arm: matched/s, node CPU-s per 1M matched, open-limit rejects, latency. |
| `bench-guard.sh` | Sourced by the driver: one global lock per host (`$RESULTS_ROOT/.bench-global.lock`), and `bench_wait_quiet` (no torus-node, bench-throughput, cargo or rustc process). |
| `quiet-check.sh LABEL` | Stricter check before each scored cell: load1 < 1.0, no bench/cargo/rustc process, no process > 40% CPU over a 5 s `pidstat` sample. Logs to `quiet-checks.log`. |
| `hostsampler.sh OUT` | Per-cell host sampler: per-thread and per-process `pidstat`, vmstat, iostat, UDP counters, socket drops, meminfo, 10 Hz state/wchan of each `hotstuff-algo` thread. |
| `prune-cell.sh LABEL` | Deletes the cell's devnet RocksDB (`<campaign>/<label>/data`) once `summary.json`, `val*.log.gz` and raw `run/` logs exist and agreement is AGREE (non-AGREE DBs are kept as evidence). Writes `<label>.retention.json`. |
| `prune-rawlogs.sh LABEL` | Deletes `<campaign>/<label>/run/val*.log` only where `zcat val*.log.gz \| cmp` proves the retained copy is byte-identical; records it in `<label>.retention.json`. |
| `arms.conf.example` | Column format of `arms.conf` and the s87 config. |

The driver calls the pruners after every cell. Run them by hand only for cells the
driver did not finish (`CAMPAIGN_DIR=<dir> ./prune-cell.sh <label>`).

## Environment

Defaults reproduce the original bench host's layout under `$HOME`.

| Variable | Used by | Default | Meaning |
| --- | --- | --- | --- |
| `RESULTS_ROOT` | all | `$HOME/bench-results-matched` | Results go to `$RESULTS_ROOT/<label>/`. Keep campaign dirs under it too. |
| `HARNESS_WT` | ab-driver.sh, run_cell.py | `$HOME/projects/wt/harness`: the one permanent harness worktree, detached at the main commit the campaign uses; move it forward only between campaigns | Clean worktree whose `tools/matched-bench/` and `devnet/` run every cell. |
| `CAMPAIGN_DIR` | run_cell.py, pruners, score.py | required (run_cell.py, pruners); cwd (score.py) | The campaign dir. The driver sets it. |
| `PREFIX` | ab-driver.sh, pruners | campaign dir name minus `-YYYYMMDD` | Label prefix: labels are `<PREFIX>-<arm>-<round>`. |
| `BENCH_GLOBAL_LOCK` | bench-guard.sh | `$RESULTS_ROOT/.bench-global.lock` | Must be the same file for every driver on the host. |
| `DRY_RUN=1` | ab-driver.sh | off | Print each cell's resolved command and exit (no lock, no writes). |
| `ORDER_OVERRIDE` | ab-driver.sh, start-*.sh | arms.conf ORDER | Run this ORDER instead. |
| `RELAX_QUIET_R0=1` | ab-driver.sh | off | r0 cells only: skip `quiet-check.sh` and run_cell.py's idle wait. `start-warmup.sh` sets it. |
| `DONE_FILE` | ab-driver.sh | `campaign.done` | Done marker name. |
| `TARGET_DIR` | ../run-cell.sh | `$HOME/.cargo-target-matched` | Where run-cell.sh takes `release/torus-node` and `release/bench-throughput`. run_cell.py sets it to the arm's artifacts dir. |

## Walkthrough

Example names: campaign `s88-example-20261004`, arms `main` (baseline) and `cand`.

### 0. Host prerequisites

Linux, `python3`, `jq`, `flock` (util-linux), `pidstat` / `iostat` (sysstat),
`vmstat`, `ss` (iproute2), `pgrep`. Free ports 8645-8647 and 9161-9163. Free disk:
run_cell.py refuses to start a cell with less than `max(14, 6 + 0.2 x duration)`
GiB free on the campaign dir's filesystem (66 GiB for a 300 s cell). No live
validator, other devnet, other bench or build on the host during the campaign.

```
export RESULTS_ROOT=$HOME/bench-results-matched
C=$RESULTS_ROOT/s88-example-20261004
mkdir -p "$C/artifacts"
```

### 1. Harness worktree

Every cell runs `run-cell.sh`, `devnet/wsl/*` and `summarize.py` from
`HARNESS_WT`. Use a dedicated, clean worktree at the commit whose harness you want,
and run the campaign scripts from the same checkout:

```
git worktree add --detach ~/projects/wt/s88-harness <harness-commit>
export HARNESS_WT=~/projects/wt/s88-harness
T=$HARNESS_WT/tools/matched-bench/campaign
```

Do not build, commit, pull or switch branches in it until the campaign is done: bash
reads a running script from disk as it goes, so changing `ab-driver.sh` under a
running driver corrupts it, and a changed harness changes later cells.

### 2. Build the arm binaries

One worktree and one `CARGO_TARGET_DIR` per branch, set explicitly on the command
line (a `target-dir` in `~/.cargo/config.toml` would otherwise send every build to
one shared dir). Build `torus-node` and `bench-throughput` in separate cargo
invocations (a combined build unifies features and changes the node):

```
cd ~/projects/wt/cand          # worktree at the candidate commit, clean
CARGO_TARGET_DIR=$HOME/.cargo-target-cand cargo build --locked --release -p torus-node
CARGO_TARGET_DIR=$HOME/.cargo-target-cand cargo build --locked --release -p bench-throughput
```

Copy each arm's binaries into the campaign, so later builds cannot change them, and
record where they came from:

```
mkdir -p "$C/artifacts/cand/release"
cp -p $HOME/.cargo-target-cand/release/torus-node "$C/artifacts/cand/release/"
cp -p <the shared bench-throughput> "$C/artifacts/cand/release/"
# optional: $C/artifacts/cand/manifest.json with commit, branch, sha256 of both
# binaries, build notes; run_cell.py copies it to <label>.artifact-provenance.json
```

Give every arm the **same** `bench-throughput` (copy one build into each artifacts
dir) unless the load generator itself is what you test. run_cell.py records the
sha256 of both binaries in each cell's manifest.

### 3. arms.conf

```
cp $T/arms.conf.example "$C/arms.conf"   # then edit: absolute artifact paths, arms, ORDER
```

Columns are TAB-separated; the header of `arms.conf.example` documents them. ORDER
rules:
- `r0:<arm>` = the unscored warm-up (a throwaway first cell); use the candidate's
  binaries for it so it doubles as a smoke test.
- Interleave ABBA (`r1:main r1:cand r2:cand r2:main r3:main r3:cand ...`) so drift
  over the night hits both arms equally.
- At least 4 scored cells per arm: cell-to-cell variation is about 4%, so a 5%
  effect needs 4+ cells per arm.
- Name the baseline arm `main*`; score.py pairs every other arm with it per round.

Check the resolved commands without running anything:

```
DRY_RUN=1 $T/ab-driver.sh "$C"
```

### 4. Warm-up

```
$T/start-warmup.sh "$C"
```

Detached (`setsid nohup`); returns at once. Logs: `$C/<label>.log` (run-cell.sh),
`$C/<label>.driver.log` (run_cell.py), `$C/campaign.log`, `$C/warmup.nohup`. Done
when `$C/warmup.done` exists: `CAMPAIGN DONE <time>` from the driver plus `exit=N`.
Check the warm-up's `summary.json` (agreement AGREE, liveness PASS) before going on.

### 5. Measured rounds

```
$T/start-rounds.sh "$C"
```

Refuses if `warmup.done` is missing or `campaign.done` exists. For a one-off smoke cell without a warm-up (ORDER has no `r0` item), run `NO_WARMUP=1 start-rounds.sh "$C"`: same launcher, quiet checks and done marker, only the warm-up check is skipped. Do not use it for a scored A/B campaign. A 300 s cell takes
about 12.6 min plus quiet-host waits. Watch `$C/progress.tsv` (one row per cell:
exit, accepted, matched_s_avg, agreement, liveness), `$C/campaign.log`,
`$C/quiet-checks.log`. Done when `$C/campaign.done` exists (`CAMPAIGN DONE` or
`CAMPAIGN STOPPED before <label>`, plus `exit=N`).

To stop cleanly, create `$C/STOP`: empty = before the next cell; `N` = finish round
N first. A running cell is never interrupted. A relaunch resumes (finished labels
are skipped). If a cell fails to start, the driver stops with `exit=1` and the
reason in `<label>.driver.log`.

### 6. Score

```
$T/score.py "$C"
```

Report these two per arm:
- **matched/s**: `headline.matched_s_avg` (node Prometheus counters, mean of the 3
  validators).
- **node CPU-s per 1M matched**: CPU of the whole node processes over the load
  window, from the pidstat PROCESS rows (TID `-`) of `$C/<label>-host/threads.log`,
  per node. Not the per-thread rows: those are breakdowns of the same CPU, and
  summing them double-counts.

Read the paired deltas (each arm vs the `main*` arm of the same round) rather than
differences of means; check every scored cell is AGREE/PASS first.

### 7. Re-summarize

After a `summarize.py` fix, re-score existing cells in place from their own
provenance, then score again:

```
for d in $RESULTS_ROOT/s88-example-*-r[0-9]*; do $HARNESS_WT/tools/matched-bench/resummarize.sh "$d"; done
$T/score.py "$C"
```

## What a cell leaves behind

| Where | What | Size (300 s, 300 markets) |
| --- | --- | --- |
| `$RESULTS_ROOT/<label>/` | summary.json, CSVs, metrics, digests, `val*.log.gz`, `rocksdb-log/` | ~40 MB, kept |
| `$C/<label>-host/` | host sampler output (score.py reads `threads.log`) | 15-25 MB, kept |
| `$C/<label>.{log,driver.log,manifest.json,retention.json}` | runner logs and provenance | small, kept |
| `$C/<label>/data/` | devnet RocksDB | up to ~37 GB, deleted by prune-cell.sh (AGREE only) |
| `$C/<label>/run/val*.log` | raw node logs | 0.2-1 GB, deleted by prune-rawlogs.sh after the `.gz` check |

## Oracle feed (ORACLE_FEED=1)

Off by default: cells without it are unchanged. With `ORACLE_FEED=1` in an arm's
runner_env column, run-cell.sh starts `bench-throughput oracle-feed` after the idle
probe: the 3 validators sign one price (`ORACLE_PRICE`, default 30000) for every
market every `ORACLE_INTERVAL_MS` (default 2000, must be < 10000), with the keys in
`devnet/wsl/bench-validator-keys.json`. The load starts once every market has a
fresh mark (90 s, else the cell fails); the feed is paused for the drain and the
state digest, then stopped. Results: `oracle-feed.log`, `oracle-feed-stats.json`,
`oracle-marks-*.json`, `summary.json .oracle_feed`. Details: `../run-cell.sh` header.

Requirements:
- The arm's `bench-throughput` must have the `oracle-feed` subcommand. main does
  not have it yet: build bench-throughput from a branch that does (the crab stack,
  e.g. `bench/s87-oracle-feed`, which needs `tools/price-feeder`) until it merges.
  run-cell.sh fails the cell at pre-flight if the subcommand is missing.
- `HARNESS_WT` must contain `devnet/wsl/bench-validator-keys.json` and the
  matching `gen-3val-genesis.sh` (this commit or later).

Since that genesis change, the 3 bench validators use the addresses from
`bench-validator-keys.json` instead of `0x1000..01 / 0x2000..02 / 0x3000..03`, in
every cell (feed on or off). Consensus keys, stakes and all other genesis bytes are
unchanged, but the genesis md5 differs from older cells: one more reason to compare
only within a campaign.

## Host notes

- **Quiet host, three layers**, all before a cell starts and none during it:
  `bench_wait_quiet` (no torus-node / bench-throughput / cargo / rustc),
  `quiet-check.sh` (load1 < 1.0, no process > 40% CPU), run_cell.py (load1 < 1.5,
  no `fstrim`). Plus run-cell.sh's own pre-flight. Nothing stops a build that
  starts mid-cell, so do not build or run heavy jobs on the host while a campaign
  runs. `RELAX_QUIET_R0=1` relaxes only the second and third layers, only for r0.
- **CPU pinning**: `NODE_CPUS=0-5/6-10/11-15 BENCH_CPUS=16-17` in runner_env pins
  each node and the load generator. Only a harness from branch
  `perf/s75-cpu-pinning` reads them; main's run-cell.sh ignores them without an
  error (the same holds for `RATE_SCHEDULE` and `DEPTH_OBSERVER`). Pinned and
  unpinned cells are not comparable.
- **Disk**: see the table above. Keep about 70 GiB free for a 300 s cell; after
  the pruners each cell keeps roughly 60 MB.
- **One driver at a time**: a second driver queues on the global lock and reads
  its arms.conf only once it holds the lock.

## Known traps

- **Shared CARGO_TARGET_DIR serves stale binaries.** Worktrees building into one
  target dir reuse each other's artifacts as "fresh"; the arm then benches the
  wrong build. One target dir per branch, copy binaries into
  `artifacts/<arm>/release/`, and compare the sha256s in `<label>.manifest.json`
  with the build.
- **run-cell.sh aborts if any process command line contains `cargo build`**
  (`pgrep -f "cargo build"`), including an editor, a `tail` on a build log, or a
  shell or agent whose arguments mention it. The cell then fails at pre-flight.
- **pgrep quirks**: `pgrep -x` matches only the first 15 characters
  (`bench-throughpu`); `pgrep -f <string>` from a shell whose argv contains the
  string matches that shell (use `ps -eo args | grep '[a]b-driver'`).
- **The quiet checks run only at cell start.** A build or job started during a
  cell is not detected and skews it. Never bench while builds run.
- **Only compare same-campaign, interleaved cells.** The same binary has swung
  from 61k to 94k matched/s within 2 hours on one host. Never compare against
  cells from another campaign, another harness commit, or another time.
- **Do not touch the tooling or harness checkout during a run** (see step 1).
- **Labels are unique**: run_cell.py refuses a label whose results dir or devnet
  dir exists; ab-driver.sh skips it. Use a new campaign dir (new prefix) to re-run.

## Host-specific defaults

`$HOME/bench-results-matched`, `$HOME/projects/wt/harness` and
`$HOME/.cargo-target-matched` are the original bench host's defaults. On another
host set `RESULTS_ROOT` and `HARNESS_WT` (and `TARGET_DIR` when calling
run-cell.sh directly).
