# Remote test gates (cachyoz, ubuntoz)

18c has one shared build lock and is often loaded (~100 on 18 cores). Two Tailscale hosts
run cargo test gates for it, so builders on 18c can offload full and targeted runs.

## Where gates may run

| gate | where |
|---|---|
| `full`, `final` | **cachyoz** (preferred) or 18c |
| `targeted` | **ubuntoz** or 18c |
| any gate | **never on archy** (ozarchy runs benches only), never on crab |

## Runners

| | cachyoz | ubuntoz |
|---|---|---|
| user / shell | `oz` / fish (wrapper is fish-safe) | `ubuntoz` / bash |
| cores / RAM | 16 / 47 GB | 8 / 11.9 GB |
| gate repo | `~/projects/Torus-hyperBFT-gate` (separate clone; `~/projects/Torus-hyperBFT` holds the owner's local work, do not use it) | `~/projects/Torus-hyperBFT` |
| worktrees | `~/projects/gate-wt/<ref>` | same |
| target dir | `~/.cargo-target-gate` | same |
| build lock | `~/projects/.cargo-gate.lock` (one cargo command at a time, `flock -w 3600`) | same |
| jobs / test threads | 16 / default | 6 / 6 (RAM) |
| toolchain | rustup 1.97.0 + clippy, rustfmt (same as 18c's `stable`) | same |
| linker | system clang 23 + mold 3.0 | user-local clang 18.1.3 + mold 2.30.0 (same versions as 18c), in `~/.local/opt`, linked from `~/.local/bin` |
| sccache | system 0.18, gate-only `RUSTC_WRAPPER` | 0.16 in `~/.local/bin`, `rustc-wrapper` in `~/.cargo/config.toml` |
| cargo-nextest | 0.9.140 (prebuilt) in `~/.local/bin` | same |

Host settings live in `~/.config/remote-gate.env` on each host (repo path, toolchain pin, jobs,
test threads, `LIBCLANG_PATH`, `SCCACHE_CACHE_SIZE=30G`). When 18c moves to a new stable
toolchain, install it on both hosts (`rustup toolchain install <ver> --profile minimal -c clippy,rustfmt`)
and bump `RUSTUP_TOOLCHAIN` there.

## Usage from 18c

```bash
torus-remote-gate <host> <branch-or-sha> full|final
torus-remote-gate <host> <branch-or-sha> targeted '<nextest filter expr>' [<package flags>...]
torus-remote-gate <host> <branch-or-sha> warm     # compile-only cache warm-up
```

- The ref must be **pushed to origin**; the runner fetches it (branch name or sha).
- The wrapper prints one line per step, copies `summary.txt` back to
  `/tmp/remote-gates/<host>-<ref>-<mode>-<timestamp>.summary.txt`, prints it, and exits
  with the gate's exit code (0 only if every step passed). Full logs stay on the host in
  `~/gate-logs/<ref>-<mode>-<timestamp>/`.
- Full/final gates take many minutes: start them with `run_in_background: true`,
  output to a file, and read only the summary.

Examples:

```bash
torus-remote-gate cachyoz feat/my-branch full > /tmp/gate.out 2>&1
torus-remote-gate ubuntoz feat/my-branch targeted 'rdeps(torus-state)'
torus-remote-gate ubuntoz 1a2b3c4d targeted 'test(parse_on_by_default)' -p torus-bridge
```

## Modes (`~/bin/remote-gate` on each host)

| mode | steps |
|---|---|
| `full` | `cargo nextest run --workspace` + `cargo test --doc` + `cargo clippy --workspace --all-targets` (warnings counted, no `-D`) + `cargo build --release -p torus-node` |
| `final` | `full` + `cargo test --workspace --no-fail-fast` (the one-process run TESTING.md requires before merging to main) |
| `targeted` | `cargo nextest run -E '<expr>' [<package flags>]` |
| `warm` | nextest `--no-run`, clippy, release torus-node (doc tests cannot be compiled with `--no-run`) |

Nextest runs use the TESTING.md flags plus `--no-fail-fast` and `--final-status-level flaky`.
`summary.txt` lists per step: rc, duration, the nextest `Summary` line or aggregated
`test result` counts, failed and flaky test names, clippy warning/error counts, first
compile errors. Known host flakes are **flagged, not hidden** (`[KNOWN HOST FLAKE]`, the
step still fails): `allocator::tests::stats_are_readable_and_published`,
`epoch_boundary_rejoin_test`, `checkpoint_retention_bounded_over_long_run`. A `flaky`
count above 0 gives `RESULT: PASS with N flaky`, which is reported, not a clean pass.

## Measured at setup (main, s131)

| | cachyoz | ubuntoz |
|---|---|---|
| warm: nextest `--no-run` (cold) | 4m26s | 8m11s |
| warm: clippy / release torus-node | 48s / 4m15s | 11m29s / 18m28s |
| `full` gate, warm cache | 7m35s (nextest 7m30s, 4275 passed) | not measured |
| `targeted 'test(parse_on_by_default)'` | 2s (4 passed) | 29s incl. rebuild after a main merge |

## Maintenance

- Each ref gets its own worktree, and all worktrees share one target dir. Cargo gives
  workspace crates the same artifact names in every worktree and decides freshness by
  mtime, so without care a gate can test another worktree's code (seen at setup: main
  failed to compile against `torus-state` built from `fix/pretestnet-p0-p1`). The script
  therefore records the last worktree built in `~/.cargo-target-gate/.gate-last-worktree`
  and, under the build lock, touches every tracked file when the worktree changed. Cost:
  workspace crates rebuild when consecutive steps come from different refs (dependencies
  stay cached); summaries say `workspace sources restamped` when that happened. Remove old worktrees with
  `git -C <gate repo> worktree remove ~/projects/gate-wt/<ref>`.
- Update a host's script with write-to-temp + `mv` (atomic rename), never by overwriting
  `~/bin/remote-gate` in place: a running gate's bash reads the file as it goes.
- Watch disk on ubuntoz (82 GB free at setup; target dir 25 GB after warm-up).
- Locks: the script uses `flock -o` and starts sccache before taking any lock, so the
  long-lived sccache server never inherits a lock fd (it once held the worktree lock forever).
