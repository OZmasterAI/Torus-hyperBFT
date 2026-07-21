# Mission Handoff — Layers 1+2 CLOSED, Layer 3 (compute) next (updated 2026-07-21)

Continuation doc for a clean session. Supersedes the *phase state* of
`mission-handoff-wedge-layer2.md` (keep for history). Memory manifest
`mission-recovery-manifest` mirrors this and carries the fine detail.

## Where we are, in one paragraph

Layer 1 (HotStuff wedge) and Layer 2 (storage scaling) are CLOSED. The 3c preimage
round (level-rows-as-authority + hash-only mirror, `TORUS_BOOK_ROWS=2`) is proven in
vivo: flush 1.4s→0.23s/blk, dirty buckets O(touched), **mission records 13,659
matched/s window-avg / 23,160 best-60s** on 18c (uncapped). The health gate
(ratified: 0.8 × box idle ⇒ ~21.0 worst-60s blk/s on 18c) still FAILS in every
loaded config — and we now know exactly why: **view time under load is
WORK-CONSERVED**. The body-push A/B (2a0d207) improved every pipeline phase it
touched (arrival 58.7→21.3ms, insert_persist 35.2→16.2, build 22.2→7.6) yet total
view time stayed ~114ms — qc_collect absorbed all savings because replicas are
CPU-saturated (exec_queue pegged): votes arrive when CPUs free, not when messages
deliver. Pipeline-latency levers are EXHAUSTED. Layer 3 = cut replica per-view WORK
113ms → ≤48ms, or deepen exec/consensus overlap.

## Layer-3 work items (the compute round)

1. **Engine parallelism across markets** — matching engine is ~16ms/blk at cap-400
   and grows with block size; markets are independent; 18 cores sit mostly idle
   during exec. TORUS_PARALLEL_SETTLE parallelizes settlement pass-A only; the
   match loop itself is serial. Likely node-local (bridge/executor) if event
   ordering is preserved deterministically — determinism proof required.
2. **Parallel signature verify** — verify was ~0.29-0.42s/blk uncapped; per-order
   ~µs-scale at cap-400 but on the critical path. Batch/parallel ed25519+eip712.
   Node-local.
3. **Exec/consensus overlap** — replicas serialize views behind exec. Investigate
   whether vote-path work can shrink (what must a replica do before voting vs what
   it currently does) and whether exec of block N can overlap consensus of N+1
   more deeply without touching the commit-waits-on-exec safety contract
   (wedge fix S470 depends on qc−commit lag being visible; any overlap change is
   consensus-visible = Fable + stateright).
4. **Then**: re-proof at cap-400 vs the 21.0 gate; if passed, re-open uncapped
   throughput with the compute headroom (engine share dominates uncapped too).

### Layer-3 round-1 outcomes (2026-07-21, this session — items 1-3 RESOLVED, scope pivoted)

- **Item 3 (overlap) CANCELLED as consensus work**: `docs/l3-work-budget.md` @ c1f3d1b
  proved votes already wait only on header-DA + safety + lock advance (~1.6ms) — never
  on exec or body. Exec is async post-commit, one worker, ≤64 blocks deep; view wall =
  exec-worker per-block wall via channel backpressure. Nothing consensus-visible to do;
  the S470 negative constraint (keep qc−committed lag visible + 64-cap) stands.
- **Item 1 (engine parallelism) NEGATIVE RESULT**: `perf/l3-engine-par` @ 21b3073
  (design + verdict in docs/design-parallel-engine.md). KEY CORRECTION: cross-market
  matching was ALREADY parallel (always-on MarketWorkerPool::match_parallel); the
  serial residue is Phase-2 prepare + inherently-ordered settle pass-B, which
  determinism forbids parallelizing. µbench: serial wins at cap-400 AND 5k; +9-13% at
  25k only. TORUS_PARALLEL_ENGINE (sender-sharded Phase-2, determinism matrix proven
  20×) stays in-tree default-OFF; not the lever. Also corrected: margin IS reserved
  (default 20× leverage fallback) — "margin_configs empty ⇒ zero margin" is wrong.
- **Item 2 (parallel verify) ALREADY BANKED**: exec-path verify was already
  global-rayon-parallel + ed25519-batched (serial would be 81ms at cap-400; today
  ~18ms cold). `perf/l3-verify-par` @ a2bcb88 adds TORUS_PARALLEL_VERIFY control,
  mode-pinned test entrypoint, differential/cache tests, µbench. NOTE: cold-cache
  verify ~18ms ≫ the 2-6ms trust-cache estimate — in-vivo hit rate unknown.
- **Flush stream (new)**: `perf/l3-flush-pipe` — only body_persist is safely
  deferrable (~5-10ms; authoritative durable body already written at dispatch,
  app.rs:4128); state_write + evm_resync must stay sync (read-your-writes for
  engine/verify(N+1)). Plus TORUS_BUCKET_HASH_MIN_BUCKETS adaptive root threshold
  (helps uncapped only). In test-debug at handoff time.
- **PIVOTED SCOPE**: measured/addressable new savings sum to ~10-15ms of the 113→48
  gap. The ~40-50ms UNTIMED exec-loop residual (persist_committed_block_durably,
  dispatch prep, deschedule tax — never directly measured) is now the whole game.
  NEXT: (a) pegged cap-400 cell scraping ALL exec_* + exec_queue_depth (needs devnet
  go) — THE decisive measurement; (b) then either attack the revealed untimed
  consumers or build Option 2 (two-stage exec pipeline, overlap flush(N) with
  engine(N+1) behind snapshot reads, ≈16ms, node-local, design in l3-work-budget §4).
- **New known-flaky test**: exec_hole_budget_exhaustion_latches_fail_stop
  (torus-consensus lib) — 3/3 standalone green on branch + 2/2 on base, fails only
  under parallel in-binary test load. justify_block_livelock_test failed 1× standalone
  on quiet 18c (130s) — base comparison pending at handoff time.
- **Ops lessons**: `echo EXIT=$?` inside nested bash -c quoting expands at LAUNCH
  (always 0) — detached runs must use uploaded script files with single-quoted flock
  bodies; never trust old-pattern markers, grep FAILED. Never pipe triage runs
  through tail (destroys evidence). Env-var-toggled features need mode-pinned test
  entrypoints (process-global set_var contaminates parallel in-binary tests).
5. **Parked (Layer-3b, after gate)**: RPC/ingress ceiling (~17k/s accepted at all
   offered rates), real-WAN 3-machine testnet (user may provide 3rd box; Package B
   never WAN-tested), production-debt list below.

## Current state (all pushed to vpsbuild + b18c bare repos; NOTHING on GitHub)

- **Integration tip: `perf/re-proof5` @ d80b422** — everything: C+D+rank8+rootcache
  +3b+B+wedge-fix+3c(level-rows+mirror)+L3 fixes (member-scan bound, compaction
  knobs, body-push v2, dedup-scope corrections) + all results docs.
- Bench-standard env (all proven): `TORUS_BOOK_ROWS=2 TORUS_RESIDENT_BOOKS=1
  TORUS_NATIVE_ROOT_CACHE=1 TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=1
  TORUS_BUCKET_MEMBER_CACHE_MB=256 TORUS_CHURNY_CF_WRITE_BUFFER_MB=16
  TORUS_BODY_PUSH_MAX_BYTES=65536` + genesis/env `TORUS_COMMIT_LAG_BACKOFF_CAP=8`
  (fleet-uniform) + `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=400` for gate cells.
- Boxes: **18c** (ssh `18c`, 18 cores) = default build/test box AND bench box
  (user policy `build-test-box-policy`): clone `~/torus-bench`,
  `CARGO_TARGET_DIR=$HOME/torus-bench/target`, flock `~/.torus-build.lock`,
  coordination file `~/TORUS-BENCH-COORDINATION.md` (shared with user's parallel
  session — honor it; our devnet ports 8645-7/30401-3/9161-3). **VPS** (ssh `vps`)
  = overflow only. All campaign scripts + raw cells in `~/bench-results-18c/` on 18c.
- Results docs (all in `devnet/wsl/results/` on perf/re-proof5): baseline-18c-95cd399,
  reproof5-18c-4f832e5, capsweep-18c-4f832e5, l3verify-18c-06de732,
  bodypush-ab-18c-2a0d207 + docs/l3-diagnosis.md, docs/l3-persist-attribution.md
  (§6 = v1 post-mortem), docs/l3-arrival-attribution.md,
  docs/design-level-rows-authority.md.
- Known-flaky tests (rerun standalone before believing a failure; CPU-load-
  sensitive, pass on quiet 18c): justify_block_livelock_test,
  progress_and_validator_set_update_test, bg_writer::queued_batches_returns_to_zero.

## Production debt (must-fix before prod; grows, never shrinks silently)

margin_configs empty ⇒ zero margin reserved; **vote-safety state is non-durable
(nothing on the vote path fsyncs — needs one grouped ~7ms sync/view + stateright
`vote_state_durable_before_send`)**; Rank-3 lock-fork recovery net; RPC/precompile
book readers row-blind (modes 1/2); state-sync transport for `cf_book_order_rows`;
chunked-level-hash decision pre-preimage-freeze (tag 0x04+ reserved); mode → genesis
file; adaptive bucket-hash threshold + promote proven knobs to defaults at final
integration; flaky-test poll bounds; consensus-visible flag composition needs ONE
fresh genesis at final integration.

## Operational rules (unchanged, restate in every agent brief)

1. Every cargo behind `flock ~/.torus-build.lock`; devnet-guard
   (`ss -ltn | grep -qE ":(864[5-7]|3040[1-3]|916[1-3])\b" && exit 1`) in the same
   shell; ONE devnet at a time; node-Prometheus counters only, never load-gen
   headline (broken at this lineage — reports 0).
2. Detached + `EXIT=$?` marker + ONE run_in_background watcher for all long remote
   work; never foreground-babysit; never relaunch a step whose marker = 0;
   non-interactive ssh needs `PATH=$HOME/.cargo/bin:$PATH`.
3. Agent hygiene: fresh agent + distilled brief per scope; retire before context
   balloons; Fable for consensus-visible/preimage work, Opus for mechanical/bench;
   subagent background watchers die when the subagent idles — orchestrator owns
   all watchers.
4. Boundaries: devnet launches, merges, GitHub pushes need explicit user go;
   perf/* pushes to vpsbuild + b18c pre-authorized; exact-today defaults for every
   change; consensus-visible = fleet-uniform + fresh genesis + stateright.
5. A/B method: env-gated features A/B on ONE binary; un-gated changes need a saved
   control binary. Archive val0.log per cell (CLEAN=1 wipes it).
