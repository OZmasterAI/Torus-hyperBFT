# perf/re-proof5 — branch orientation & mission state (2026-07-21)

Cold-start orientation for anyone (human or agent) opening this branch. Everything
below is traceable to repo docs — no new facts. Primary sources: `docs/mission-handoff-layer3.md`
(esp. its "Layer-3 round-1 outcomes" section), `docs/l3-work-budget.md`,
`docs/l3-savebooks-attribution.md`, the design docs (`design-level-rows-authority`,
`design-parallel-engine`, `design-flush-pipeline`, `design-levelhash-cache`), and the
devnet results under `devnet/wsl/results/` (esp. `reproof5-18c-4f832e5`,
`capsweep-18c-4f832e5`, `l3scrape-18c-b1aba10`, `cacheab-18c-13bd833`).

## 1. What this branch is

`perf/re-proof5` is the **throughput-mission integration branch** (rooted off
`a225844`). It accumulates the whole mission lineage: packages C+D, rank8 resident
books, root cache, package-B dissemination, the S470 wedge fix, the 3c preimage round
(level-rows-as-authority + hash-only mirror), and the Layer-3 compute work.

Two invariants hold across the entire branch:

- **Every feature is env-gated and default-OFF = exact-today.** With no `TORUS_*`
  perf flags set, the merged tree is byte-identical in behavior to the pre-mission
  code. (`mission-handoff-layer3.md` operational rule 4; each design doc restates this.)
- **NOTHING is merged to `main` or pushed to GitHub.** Builds and benches run only on
  the **18c** box, distributed via the **b18c** bare repo (`ssh://18c/.../torus-b.git`);
  the VPS bare repo (`vpsbuild`) is overflow and currently off-limits.
  (`mission-handoff-layer3.md` §"Current state", operational rule 4.)

## 2. Mission arc

**Layer 1 — HotStuff commit wedge — CLOSED.** The S470 wedge fix ships as
`commit_lag_backoff_cap = 8` (fleet-uniform genesis field): the pacemaker lengthens
view deadlines as a function of `highest_qc_view − committed_view` so exec can drain
without the pacemaker timing views out into a cascade. This qc−committed lag signal
**must stay visible** — any change that hides it re-arms the wedge.
(`l3-work-budget.md` §2.)

**Layer 2 — storage scaling — CLOSED.** The 3c preimage round put order books into
**level-rows-as-authority mode 2** (`TORUS_BOOK_ROWS=2`, `BookMode::LevelAuthority`)
with a hash-only mirror and in-book journals. Proven in vivo (`reproof5-18c-4f832e5`):
flush fell ~1.4s → ~0.23s/blk, dirty buckets went O(touched) (~10× down), and the
mission set **new records: 13,659 matched/s window-avg and 23,160 matched/s best-60s**
on 18c (uncapped). The cap sweep (`capsweep-18c-4f832e5`) confirmed throughput scales
~linearly with the block cap (2.4k → 13k matched/s, cap 400 → uncapped) while cadence
degrades (8.6 → 0.7 blk/s) — no single cap gives both the gate and record throughput.

**Layer 3 — compute — in progress (this session).** The gate wall moved from storage
to per-view CPU work. Findings:

- **Vote/overlap already optimal (item 3 cancelled).** `l3-work-budget.md` proved the
  vote waits only on header integrity + justify safety + a lock advance (~1.6ms) —
  never on exec or body. Exec is async post-commit on one worker, ≤64 blocks deep;
  the view wall is the exec-worker per-block wall via channel backpressure. Nothing
  consensus-visible to do.
- **Engine parallelism — honest negative result.** Cross-market matching was already
  parallel; the serial residue is determinism-bound (Phase-2 prepare + ordered settle
  pass-B). µbench: serial wins at cap-400 and 5k; +9–13% only at 25k.
  `TORUS_PARALLEL_ENGINE` stays in-tree, default-off, not the lever.
  (`design-parallel-engine.md`; round-1 outcomes item 1.)
- **Parallel verify — already banked.** Exec-path verify was already rayon-parallel +
  ed25519-batched (serial 81ms → 18ms cold at cap-400). `TORUS_PARALLEL_VERIFY` adds
  a control/test entrypoint. (round-1 outcomes item 2.)
- **Flush pipeline — accepted with caveat.** Only `body_persist` is safely deferrable
  (durable body already written at dispatch); state_write + evm_resync must stay sync.
  Async body-persist proved NEUTRAL in vivo (`l3scrape`), stays default-off.
  (`design-flush-pipeline.md`; round-1 outcomes.)
- **The untimed giant = `save_books`.** `l3scrape-18c-b1aba10` found `save_books` at
  **~107ms/loaded block** (design intent said 1–3ms). Attribution
  (`l3-savebooks-attribution.md`): `level_row_data` keccak-rehashes the ENTIRE FIFO of
  every touched price level, O(orders-at-level), driven by ~195k resting orders
  (offered 300k/s ≫ matched 2.4k/s ⇒ books never drain). It is genuine uncontended CPU,
  not deschedule inflation. The `level_hash` is the consensus-frozen state-root
  preimage, so an O(√depth) chunked/Merkle fix is consensus-visible (production debt).
- **Level-hash sponge cache — built and proven in vivo.** `TORUS_LEVEL_HASH_CACHE`
  (`design-levelhash-cache.md`, now **default-ON at 256 MB** — the A/B's own budget;
  `=0` opts out) keeps each append-only level's un-finalized keccak
  sponge state and absorbs only new tail frames → byte-identical digest in O(delta);
  any non-append op bumps a per-level epoch → full-rehash fallback; staged seeding
  (Probe→Promote→Hit) avoids re-seeding churning levels. A/B (`cacheab-18c-13bd833`):
  **+45% blk/s, +81% matched/s, worst-60s 6.7× (0.67 → 4.50), save_books 83 → 37ms.**

## 3. Feature / flag inventory

Bench-standard env block (verbatim, `mission-handoff-layer3.md` §"Current state"):

```
TORUS_BOOK_ROWS=2 TORUS_RESIDENT_BOOKS=1 TORUS_NATIVE_ROOT_CACHE=1
TORUS_PARALLEL_SETTLE=1 TORUS_PARALLEL_BUCKET_HASH=1 TORUS_BUCKET_MEMBER_CACHE_MB=256
TORUS_CHURNY_CF_WRITE_BUFFER_MB=16 TORUS_BODY_PUSH_MAX_BYTES=65536
```
plus genesis/env `TORUS_COMMIT_LAG_BACKOFF_CAP=8` (fleet-uniform) and
`TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=400` for gate cells.

| flag | default | scope | note |
|---|---|---|---|
| `TORUS_BOOK_ROWS` | 0 (off) | **consensus-visible** (fresh genesis) | =2 selects level-rows-as-authority mode 2; state-root preimage |
| `TORUS_COMMIT_LAG_BACKOFF_CAP` | fleet field | **consensus-visible** (fresh genesis) | =8 is the S470 wedge fix; qc−committed backoff |
| `TORUS_LEVEL_HASH_CACHE` | **unset = ON @ 256 MB**; `0` = OFF | node-local | level-hash sponge cache (MB budget); byte-identical; only engages under `TORUS_BOOK_ROWS=2` |
| `TORUS_PARALLEL_ENGINE` | OFF | node-local | sender-sharded Phase-2; determinism-proven; not the lever |
| `TORUS_PARALLEL_VERIFY` | (verify already parallel) | node-local | control/test entrypoint over the banked win |
| `TORUS_ASYNC_POST_FLUSH` | OFF | node-local | defers body_persist only; NEUTRAL in vivo |
| `TORUS_BUCKET_HASH_MIN_BUCKETS` | off | node-local | adaptive state-root threshold; helps uncapped only |
| `TORUS_PROPOSER_EXEC_WATERMARK` | 0/unset = never yields | node-local | leader yields on local exec backlog; always safe |
| `TORUS_EXEC_NONBLOCKING_DISPATCH` | off | node-local | deeper channel; ~0 mean gain (variance only) |
| `TORUS_BODY_PUSH_MAX_BYTES` | base 0 = exact-today | network | proactive body push for small blocks |
| `TORUS_RESIDENT_BOOKS` / `TORUS_NATIVE_ROOT_CACHE` / `TORUS_PARALLEL_SETTLE` / `TORUS_PARALLEL_BUCKET_HASH` / `TORUS_BUCKET_MEMBER_CACHE_MB` / `TORUS_CHURNY_CF_WRITE_BUFFER_MB` | off / tuning | node-local | Layer-1/2 perf knobs (rank8, root cache, settle, bucket hash, caches) |
| `TORUS_NATIVE_ORDERS_PER_BLOCK_CAP` | uncapped | node-local policy | =400 for gate cells |

## 4. Gate state

- **Ratified health gate:** `0.8 × box idle` ⇒ **~21.0 worst-60s blk/s on 18c**
  (idle 26.26 blk/s; cross-build 18c reference 18.7). (`reproof5-18c-4f832e5`.)
- **Current status: NOT passed, but the wall moved.** Best result is
  **4.50 worst-60s blk/s** (cache-on, cap-400, `cacheab-18c-13bd833`) — the depth-decay
  is **damped 6.7×, not cured**. Residual per-loaded-block chain ≈ 37 (save) + 42
  (engine) + 38 (flush) + verify/evm ≈ ~125ms.
- **Ranked next-round candidates** (`cacheab-18c-13bd833` §Next):
  1. **Match-touched level residual** — matches pop the FIFO *front*, breaking the
     sponge prefix; only a commitment change (chunked/Merkle, consensus-visible, tag
     0x04+, production debt) fixes that class. Node-local cache is near its ceiling;
     export hit/miss/seed gauges to Prometheus next campaign (debt this run).
  2. **Flush at depth** — state_write ~23.8ms, grows with the dirty set.
  3. **Engine contention** — 41.6ms in vivo vs 16ms µbench; re-measure now that
     save_books freed CPU; consider settle-engagement tuning.
  4. **Re-run the cap sweep** — with depth-decay damped, a higher
     `ORDERS_PER_BLOCK_CAP` may now dominate cap-400 on matched/s.

## 5. Known flaky tests & production debt

**Known-flaky tests** (rerun standalone before believing a failure; CPU-load-sensitive):

1. `justify_block_livelock_test` — fails ~1/3, 130–190s, poll-bound.
2. `exec_hole_budget_exhaustion_latches_fail_stop` (torus-consensus lib) — in-binary
   parallel-load flake; green standalone.
3. `parent_body_starvation_test` — poll-bound ~94–100s (`poll.rs:35`), new this round.
4. `bg_writer::queued_batches_returns_to_zero`.

**Production debt** (must-fix before prod; `mission-handoff-layer3.md` §"Production debt"):
vote-safety state is non-durable (nothing on the vote path fsyncs — needs one grouped
~7ms sync/view + a stateright `vote_state_durable_before_send`); Rank-3 lock-fork
recovery net; RPC/precompile book readers row-blind (modes 1/2); state-sync transport
for `cf_book_order_rows`; chunked-level-hash decision pre-preimage-freeze (tag 0x04+
reserved); mode → genesis file; promote proven knobs to defaults + adaptive bucket-hash
threshold at final integration; flaky-test poll bounds; consensus-visible flag
composition needs ONE fresh genesis at final integration.

> **CORRECTION** (round-1 outcomes, supersedes the debt paragraph's first clause):
> margin **IS** reserved — the executor falls back to a **default 20× leverage**. The
> old "`margin_configs` empty ⇒ zero margin reserved" line is **wrong**.

## 6. Operational conventions

- **Box & coordination:** 18c (18 cores) is the default build/test AND bench box.
  Clone `~/torus-bench`, `CARGO_TARGET_DIR=$HOME/torus-bench/target`, every cargo
  behind `flock ~/.torus-build.lock`. Honor `~/TORUS-BENCH-COORDINATION.md` (shared
  with the user's parallel session). VPS is overflow, currently off-limits.
- **One devnet at a time:** guard `ss -ltn | grep -qE ":(864[5-7]|3040[1-3]|916[1-3])\b" && exit 1`
  in the same shell before any launch; our ports are 8645-7 / 30401-3 / 9161-3.
- **Counters, not headline:** trust node-Prometheus counters only; the load-gen
  headline is broken at this lineage (reports 0).
- **A/B on one binary:** env-gated features A/B on ONE binary (no rebuild between arms);
  un-gated changes need a saved control binary. Detached runs use uploaded script files
  with single-quoted `flock` bodies + a runtime `EXIT=$?` marker (nested `bash -c`
  quoting expands markers at launch = always 0); never pipe triage runs through `tail`.
