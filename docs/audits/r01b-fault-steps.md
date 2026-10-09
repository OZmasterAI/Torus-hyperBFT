# R01b manual fault test on ozarchy: light-load full disk must exit 70 (2026-10-09)

Goal: the case that hung in 19 of 19 R01 runs (`finding-consensus-write-panic-2026-10-09.md`:
a full disk at 0-2% execution load, `hotstuff-algo` panics at `kv_store.rs` `RocksDB write
failed`, process stays up = `ZOMBIE`) now ends in **exit 70**. Harness: the R01 one,
`tools/r01-fault/` (ozarchy paths, `/home/oz/r01-fault/`), unchanged except the node binary
and the `WANT` pattern.

## What R01b changes (what to look for in v3's log)

- `FATAL: <thread> thread panicked (...) — terminating node (fail-stop, exit 70)`: the panic
  hook (`torus-node` `install_fail_stop_panic_hook`), for a panic on `hotstuff-algo`,
  `hotstuff-syncsv`, `hotstuff-bodyserve`, `torus-execution`, `torus-flush-worker` or
  `torus-async-validate`. Expected first line in the light-load case, after the usual
  `panicked at crates/torus-consensus/src/kv_store.rs ... RocksDB write failed` (the default
  panic message still prints just before it).
- `FATAL: native DA store writes keep failing — latching fail-stop`: 3 consecutive DA store
  write failures (mempool mirror paths); the `exec_failed` poller then logs `FATAL: execution
  pipeline has died` and exits 70 within ~250 ms.
- `FATAL: execution channel closed without a node shutdown`: backstop when the consensus
  thread's `TorusApp` is dropped by an unwind (seen only if the hook did not exit first).

## Steps (as `oz`; nothing else on /mnt/r01, no `/mnt/r01/fill`)

1. Build the branch in its own target dir:
   `git -C <wt> checkout fix/r01b-thread-fail-stop` and
   `CARGO_TARGET_DIR=/home/oz/.cargo-target-r01b-fault cargo build --release -p torus-node`.
   Point `NODE_BIN` in `/home/oz/r01-fault/lib.sh` at that binary (`run-case.sh` logs its md5
   and the worktree commit; check both).
2. Free space on /mnt/r01 if needed (old case dirs; delete them yourself, by literal path).
3. Empty blocks, pipeline off, the case that hung 15 of 15 times:
   ```
   CASE=r01b-c2-pipeoff-empty PIPE=0 LOAD=none MAX_ATTEMPTS=5 \
   WANT='FATAL: .* thread panicked|FATAL: native DA store writes keep failing|FATAL: execution|FATAL: native overlay flush|FATAL: applied-height marker flush' \
     /home/oz/r01-fault/run-case.sh
   ```
4. Light native load, pipeline off (hung 4 of 4):
   same with `CASE=r01b-c1-pipeoff-native LOAD=native`.
5. Optional: pipeline on (`PIPE=1`), both loads.

## Pass criteria (per case, `results/<case>/attempts.txt`, `v3.log`)

- Every attempt is `HIT` with exit code **70**; no `ZOMBIE` and no other exit code.
- v3 exits within the 20 s classification window after the fill (the hook exits on the
  panicking thread at once; the DA path within ~250 ms of the third failure).
- The first FATAL line names the thread (`hotstuff-algo` expected) or the DA latch.
- Restart after deleting the fill: v3 replays, rejoins, and `check-hashes.sh` shows the
  running state hash equal on v0-v3 at every retained checkpoint (as in R01).
- Not a regression: no exit 70 during the warm-up or after a restart with free disk.

Record the results in `docs/audits/r01-fault-results/` like the R01 cases.
