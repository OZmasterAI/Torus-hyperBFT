# r1 lens `tail-sched`: scheduling mechanisms against exec / pool / consensus tails

Base: main `eb993e3f` (read-only). Numbers: `map/measured.md` plus my own reads of
`~/bench-results-matched/ozarchy-9j2-std-r2-CL21` (val0; load window 1792621450-1792621753).
Every idea is node-local (no consensus effect) and needs no determinism work.

## What I measured or verified before proposing (read-only)

- Exec thread, std CL21 r2 val0 schedstat: on-CPU 211,395 ms, runqueue wait 19,844 ms, 116,782 slices over
  6,692 committed blocks, so **31.59 ms on-CPU + 2.97 ms rq wait per block** (17.5 slices per block, 0.17 ms
  wait per slice). The on-CPU part is 82% of the 38.35 ms exec block. hotstuff-algo: 11.57 on-CPU / 1.69 rq ms/blk.
- Per-second view (sampler.csv): exec busy mean 0.874 = **38.2 idle s of 303 s**. The 95 seconds with exec
  queue 0-4 have busy 0.61-0.74 and 170-212k matched/s, while mempool reaches 2,000-2,558 actions. In these
  phases consensus is the binding stage. With queue >= 20 busy is 1.00-1.04 and matched/s is 298-389k.
- exec_chain histogram (load window, val0): 42.8% of blocks < 4 ms, 9.8% 64-128 ms, 7.2% 128-256 ms, 2.4%
  256-512 ms. So the chain-ms tail is block-size variance, while scheduling adds a near-uniform tax on top.
- end_resident: 28.0 s over 7,290 jobs (3.84 ms each, `users` apply 2.42 ms), exec waits 10.44 s (1.43 ms/blk).
- Host: load1 avg 35.6 / max 52.4, node pcpu 4.6/4.6/5.0 + bench 1.4 (summary `.cpu`). SMT on, siblings (0,16).
  Kernel 7.2.5 BORE has CONFIG_SCHED_CORE=y, CONFIG_SCHED_CLASS_EXT=y and CONFIG_UCLAMP_TASK=y (cpufreq is
  amd-pstate-epp active, so uclamp has no effect).
- Privilege probes on archy:
  - `prctl(PR_SCHED_CORE, CREATE)` on a same-uid pid works **unprivileged**.
  - `sched_setattr` with a custom fair slice (sched_runtime 0.3 ms) works **unprivileged**.
  - SCHED_FIFO gives EPERM (RLIMIT_RTPRIO 0, sudo needs a password).
  - `kernel.sched_rt_runtime_us` = 1,000,000 = the period, so **RT throttling is off**.
- Prior results that shape this lens (memory):
  - 578faa1f: pinning raised hotstuff rq wait 19-22 to 32-41 ms/blk because a pinned thread cannot migrate.
  - 12d0cc8f: bulk nice was refuted because BORE's burst penalty does not drive exec waits. Its corrected gain
    was ~+1%. Exec has 131 preemptions/s, preempt waits are ~78% of run_delay, and pool preemptors are
    pool-to-pool 70% / verify 10%.
  - 0baab823: pool phases are start-skewed. Thread first-sample is p50 3.4-4.0 / p90 7.8-8.6 ms after phase start.
  - 9d5da33a: the s124 conversion is about +0.3% matched/s per ms/blk of exec wall.

Ledger greps (all `ledger/*.md`):
- `realtime|real-time|rtprio|SCHED_RR|SCHED_FIFO|sched_setattr|sched_deadline|chrt`: 0 hits.
- `SMT|hyperthread|sibling|core sched|PR_SCHED|CCD|IPC|frequency`: 0 scheduling hits.
- `scx|sched_ext|lavd|eevdf|kernel sched`: 0 hits.
- `pin|nice|SCHED_|cgroup|affinity|taskset`: cpu-pinning-cpuset / per-node-cpu-pinning (-5.7%),
  hotstuff-cpus-pinning (rejected), bulk-thread-nice / renice-sched-batch (rejected), bounded-pools-pinning /
  core-budget (-67.7%), dedicated-cores-per-node (owner, co-location).
- `rayon|spin|steal|worker cap|LPT|fine-grain|in_place`: exec-pool-fine-grain-jobs (measured-no-gain / parked),
  exec-worker-count-knobs / match-settle-worker-caps / joint-worker-budget-sweep (proposed), caller
  participation (s124 proposed), verify-pools-parking-executor (proposed), pool-wait-cuts (proposed).
- `budget|burst|smooth|liquidat|idle`: L1.3 cadence-invariant liquidation (proposed), ADL budget (merged),
  exec-backlog-deschedule-tax (proposed, a problem statement with no mechanism).

Considered and dropped:
- Parallel encode of the L6.1 %100 full-book save. Deep only (~60 ms per 100 blocks, about 0.5 ms/blk), it is
  close to parallel-classic-book-save B, and s110 found the close stall does not set block p99.
- Parallel end_resident apply. 1.43 ms/blk at most, and it borders on Phase 5 / reuse-decoded.
- Phase-gated verify pause. Verify is only 10% of pool preemptors, so it is bulk-nice-sized.
- Adaptive pool width. That is worker caps under another name.
- Block-size control against the 10-15 s wave. That is consensus cadence (T5.1/T5.2 and throttle watermarks).

---

## 1. `exec-smt-core-cookie`: the serial threads never share a physical core (Linux core scheduling)

**Mechanism.**
- At the start of the `torus-execution` closure (`crates/torus-consensus/src/app.rs:8333-8336`) and of
  `hotstuff-algo` (`crates/hotstuff_rs/src/algorithm.rs:152-155`), call `prctl(PR_SCHED_CORE=62, CREATE, 0,
  SCOPE_THREAD, 0)` behind a node-local knob.
- Core scheduling then lets only same-cookie tasks run on the SMT sibling, so the sibling is forced idle while
  exec is on-CPU.
- Exec keeps full migration freedom. Pinning lost because the pinned thread could not move; this changes only
  the sibling's tenant.
- Exec is 82% on-CPU per block, with IPC 1.03-1.23. Its sibling usually runs Keccak/k256 verify, rayon spin or
  RocksDB compaction, which share L1/L2, the decoder and the ports.
- Unprivileged for same-uid threads (verified).

**Target metric.** Exec on-CPU ms/blk, then matched/s, blk/s and exec chain p95. The hotstuff arm also targets
the commit interval in exec-starved phases.

**Expected gain.**
- Exec: on-CPU 31.6 ms/blk. Sibling busy fraction f is about 0.55 on average (about 19.8 of 32 logical CPUs
  busy) and up to about 0.9 in bursts (load1 35-45). Zen 3 co-run inflation s is assumed to be 10-30%; the
  microbench measures it.
- Solo = 31.6 / (1 + f·s), so -1.6..-4.8 ms/blk at f = 0.55, and up to -6.7 at f = 0.9, s = 0.3. That is 4-17%
  of the 38.35 ms exec block.
- hotstuff-algo: -0.6..-2.4 ms/blk of 11.6 on-CPU. This matters in the 95 s with exec queue 0-4, when
  consensus binds.
- Cost: forced idle of about 0.70 x f, roughly 0.4 logical CPU per node (+0.15 for hotstuff), about 1.6 of 32
  host-wide.
- Result: matched/s +1..+5% (the s124 conversion gives the low end), blk/s +4..+15%, exec chain p95 -4..-15%.

**Nearest prior.** cpu-pinning-cpuset / per-node-cpu-pinning (-5.7%), hotstuff-cpus-pinning (rq 19-22 to
32-41), dedicated-cores-per-node (owner co-location), bulk-thread-nice / renice-sched-batch (share, not
sibling). None restricts the sibling.

**Risks.**
- The first cookie switches the host to core-wide rq locking (a static key). That costs something per context
  switch at 158k cs/s.
- Forced idle raises rq waits of the verify and pool threads.
- The gain is ~0 if exec is DRAM-bound rather than core-bound.
- No-op without SMT (ENODEV).
- Partly bench-shaped: siblings are busy because 3 nodes share the host.
- BORE interplay is unmeasured.

**Effort.** 0.5-1 day: a helper, an env parse plus test, a log on failure and a gauge.

**Prototype (≤3 h, unprivileged).**
- (a) Build `ubench_econ` (infra recipe, own worktree, no code change). Start 32 `chrt -i 0` spinners: they
  are SCHED_IDLE, so they take only SMT siblings and idle CPUs, never CPU share.
- Arms, ABC x3 with UB_MARKETS=300 and UB_RUNS=3:
  - C: quiet;
  - A: spinners;
  - B: spinners plus a wrapper `prctl(62,1,0,1,0)` before `execv` (whole thread group cookie, inherited by
    the exec pool).
- Record the MEDIAN engine_ms/1k fills, the phase ms and `perf stat` cycles/instructions.
- Go if A/C >= 1.08 and B recovers >= 60% of the A-C gap.
- (b) Cell: in the proto copy of `run-cell.sh`, after `:1625-1644`, a hook applies the prctl by TID to each
  node's `torus-execution` (arm 2: plus `hotstuff-algo`) via `find_tid` (`:754`). Run ABBA 120 s std cells,
  n=2.
- Compare matched_s_first120, blk/s, exec schedstat on-CPU ms/blk, the exec_block phases, host CPU and
  CPU-s/1M.

## 2. `rt-serial-chain`: SCHED_FIFO for exec, hotstuff-algo, end-resident; FIFO run-to-completion pool chunks

**Mechanism.**
- Set SCHED_FIFO at thread start:
  - `torus-execution` prio 20 (`app.rs:8333-8336`);
  - `hotstuff-algo` 19 (`algorithm.rs:152-155`);
  - `torus-end-resident` 18 (`native_executor.rs:3960-3968`);
  - arm 2: the exec pool at prio 10, through rayon `ThreadPoolBuilder::start_handler` (`exec_pool.rs:31-35`).
- A FIFO thread preempts any fair (BORE) thread when it wakes and is never time-sliced by one, yet still
  migrates (which pinning lacked).
- This removes:
  - exec's runqueue wait;
  - the post-latch wake delay after every `run_jobs` scope (`exec_pool.rs:43-59`);
  - end-resident preemption behind begin_resident's join;
  - most pool start skew. Pool chunks also run to completion with no tick preemption mid `match_market`.
- On EPERM the thread keeps today's policy.
- Unprivileged comparison arm: EEVDF custom slice (sched_runtime 0.1-0.3 ms, verified accepted).

**Target metric.** Exec rq ms/blk, pool phase wall (match, pass A), end_resident_wait, matched/s, blk/s, exec
chain p95 and commit interval p99.

**Expected gain.**
- Exec rq wait 2.97 to ~0.
- Pool waits (match 2.68 + pass A 0.93 = 3.61 ms/blk) to an estimated 1.6-2.6. Start skew p50 3.4-4.0 ms
  becomes about 0.05 ms, and mid-job preemption goes.
- end_resident wait 1.43 to an estimated 0.8-1.1.
- Total -3.5..-5.5 ms/blk, 9-14% of the exec block. hotstuff rq 1.69 goes to ~0 in consensus-bound phases.
- Result: matched/s +1..+4% (0.3%/ms conversion at the low end; the 9-14% exec-capacity ceiling is the high
  bound), blk/s +5..+12%.
- Tails: pool-phase p90 and the 15-38% "last worker preempted" share shrink, and exec chain p95 drops.

**Nearest prior.** bulk-thread-nice / renice-sched-batch (weight on others; BORE premise refuted; +1%),
pinning (-5.7%; no migration), exec-pool-fine-grain-jobs (granularity, measured-no-gain), caller
participation (proposed), exec-backlog-deschedule-tax (problem only).

**Risks.**
- Needs CAP_SYS_NICE / RLIMIT_RTPRIO. In production that is systemd `LimitRTPRIO=`.
- RT throttling is off on archy, so a spinning RT bug owns a CPU. Set 950000 for tests.
- The pool arm can black out fair threads host-wide for a phase. Keep it below exec/hotstuff and behind its
  own flag.
- Priority inversion on locks is bounded: std and jemalloc spin briefly, then futex.
- 70% pool-to-pool preemption is a 3-nodes-per-host effect, so the production gain is smaller.

**Effort.** 1-2 days.

**Prototype (≤3 h).**
- (a) Unprivileged upper bound: ubench_econ with 48 SCHED_OTHER spinners vs the same spinners under
  `chrt -i 0`, which mimics strict priority for exec+pool. Third arm: a script applies sched_runtime 0.2 ms to
  all ubench TIDs under SCHED_OTHER spinners.
- (b) Owner one-time setup: rtprio 20 for oz in `limits.d`, re-login, and `sched_rt_runtime_us=950000`.
- Then a hook in the proto `run-cell.sh` copies the `:1625-1644` pattern with `chrt -f -p` per TID
  (`torus-execution`, `hotstuff-algo`, `torus-end-resid`; arm 2 adds all `torus-exec-pool`).
- Run ABBA 120 s cells, n=2. Measure schedstat rq, phase_match / pass A / end_resident_wait, matched/s,
  blk/s, commit p99, timeouts and agreement.

## 3. `scx-forkjoin-scheduler`: a sched_ext policy that knows the node's fork-join

**Mechanism.**
- archy's kernel has CONFIG_SCHED_CLASS_EXT=y (state: disabled).
- Step 1: A/B an off-the-shelf BPF scheduler (scx_bpfland, scx_lavd) against BORE, with no node change.
- Step 2: a `scx_rustland_core` policy (Rust) keyed on comm/tgid:
  - `torus-execution` and `hotstuff-algo` are dispatched first and placed on cores with an idle sibling;
  - `torus-exec-pool` threads woken while their node's exec waits in `run_jobs` (`exec_pool.rs:43-59`) are
    gang-dispatched ahead of other fair work, with a slice longer than a chunk;
  - threads yielding from rayon's idle loop go to the lowest queue.
- This combines ideas 1-2 with fork-join awareness and needs no CAP_SYS_NICE in the node.
- The sched_ext watchdog reverts to the fair class on a stall, which is the safety net FIFO lacks while
  throttling is off.

**Target metric.** Pool phase wall, exec rq + on-CPU ms/blk, matched/s, blk/s, commit interval p99.

**Expected gain.**
- Upper bound = ideas 1 + 2: -5..-11 ms/blk, 13-29% of the exec block.
- It also covers the 70% pool-to-pool preemption, which neither FIFO-at-equal-prio nor cookies fix, and it
  deprioritises rayon yield-spin (crossbeam epoch/steal is 3.3% of node user cycles, about 0.18 cores/node).
- Result: matched/s +2..+8% if it captures ideas 1-2. Off-the-shelf heuristics may land anywhere from -3% to
  +5%.

**Nearest prior.** The same scheduling entries as 1-2. No sched_ext or kernel-policy entry exists. The bench
host runs BORE, and only its burst penalty was ever examined (12d0cc8f).

**Risks.**
- Root is needed to load it; production needs a host scheduler daemon.
- It replaces BORE host-wide, including the archy desktop.
- Generic heuristics may boost tokio/verify instead.
- BPF policy complexity.
- blk/s and p99 need n >= 4.

**Effort.** 0.5 day for the off-the-shelf A/B; 5-8 days for the custom policy.

**Prototype (≤3 h, owner root).**
- `pacman -S scx-scheds` (nothing is installed today).
- ABBA 120 s std cells: default vs `scx_bpfland`, then `scx_lavd`. Same metrics as idea 2.
- Read the schedstat rq and phase timers first to confirm the mechanism before trusting matched/s.
