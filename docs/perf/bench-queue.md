# Bench queue (archy)

Benches run on archy only. 18c keeps this list current; archy reads it on pull. IDs (Q1, Q2, ...) stay fixed when an
item moves between sections.
Last archy benches: `ozarchy-q8p2` (Q8, section 81) and `ozarchy-q5c` (Q5, section 80) of
`ozarchy-antispam-item6-pf1-2026-10-04.md`, 2026-10-17; before them `ozarchy-s58b600`, `ozarchy-s58dmt`, `ozarchy-s58safe` (sections 78-79), 2026-10-16.

## Order (owner s125, updated s127)

Every campaign uses the N = 11 bench genesis (rebuilt once with `FORCE=1` in setup). Arms that compare against main
share one binary and one all-off baseline inside a campaign.

**Main as benched (s126):** main carries the three merged s123 experiments (KP, DB, PC; flags off) and archy's s58
bundle (merge 4156a83b: D `TORUS_SETTLE_DEFER_RESULTS`, M `TORUS_PARALLEL_MATCH_SETUP`, T `TORUS_FAST_SELL_TOP_UPS`,
all ON by default; 18c Codex torus review no findings, torus-adversarial approve; main gate 3923/3923). Build M from
main at or after 50d2316c.
- D, M and T ship on and are already measured off vs on (sections 78, 79): no re-bench, no DMT/DMTE arms, no =0 arm.
  Every arm runs main as it ships: D, M, T unset (= on).
- `TORUS_HEADER_RESEND_MS=${...:-250}` in `devnet/wsl/env.sh` and `devnet/docker-compose.yml` gives every cell 250 ms
  (code default 3 s; revisited pre-testnet, roadmap). Comparisons inside a campaign stay valid; comparisons with
  results from before the resend change do not (fewer lost-proposal timeouts move block rate and matched/s).
- Block rate from cells shorter than about 5 min is indicative only: under sustained load it falls to exec pace
  (5.9 -> 3.0 blocks/s by minute 5) while matched/s stays flat (section 79). matched/s stays the primary metric.
- Watch `no_proposal` in every replicate (1 in the s58dmt check cell, 1 in the s58safe soak).
- Soaks longer than about 20 min need account-margin top-ups or a lower load (the s58safe load ran out of margin).
- Not on main: B (`TORUS_PARALLEL_BOOK_SAVE`) and G (`TORUS_EXEC_POOL_GRAIN`) are parked (78.6); archy's E duplicate
  (bb62bab1) is not merged (E is main's `TORUS_EPOCH_FAST_PATH`).

**Driver generator (main d280a8fd):** a campaign check sees only its own checks file's variables and the driver's
globals, never a driver function's local (`$dur`, `$rc`, `$label`, `$t`, ...). Before the next campaign with a checks
file (e.g. `s58dmt-checks.sh`): freeze the new tools, regenerate the driver, run `DRIVER dry` and `DRIVER run smoke`; a
check that read a driver local now fails (use `$1` label, `$2` arm, or globals `R`, `AENV`, `AXE`, ...). `_DRV_` is
reserved. Warm-up gate on by default (`"gate": false` restores the old behaviour); 1 Hz soak sampler on every cell.

**Now (owner s127 order):**

1. **Setup:** build binary M from main, rebuild the bench genesis with `FORCE=1` (N = 11), run the Q5 smoke cell and
   its checks (Q5 below). Gates every later campaign. **Done s64 (setup PASS, M = e3d11cb9).**
2. **Q8. Item 6 Phase 5 step 0 profile** (campaign P, profile only, no A/B, ~1.5 h, `perf_event_paranoid=1` for this
   run only). Sent to archy directly (s125); it decides whether and how much of Phase 5 gets built.
   **Done s66 (ozarchy-q8p2, 9 / 9 valid; section 81): exec ~131 / ~149 ms per native block ON / OFF; flush_all 11%,
   Keccak 10-11%, save_order_books 6-7%, put_cf_raw 5%, borsh 5%, merge_disjoint 4%; exec off-CPU 20-26% (pool waits,
   one ~1 s pipeline_barrier per 15 s); hotstuff-algo blocked on the full exec channel 9-35%.**
3. **Q7. Item 7 step 2: READY (s126), ~2.5 h.** Arm A = main at the merge base **db84a1ad**; arm B =
   `feat/item7-step2` head **c567b1b3** (origin). C1-C10 built and Codex-clean, 4-validator devnet check done, main
   with the s58 bundle merged in, EVM double-inclusion fix and its follow-ups Codex-clean; gates nextest --workspace
   4021/4021, full cargo test 4022. Spec under Items, Q7. (Main after db84a1ad adds only docs, harness fixes and the
   behaviour-neutral exec-failed latch order fix, so arm A stays db84a1ad.) Both arms are fixed SHAs: campaign A
   does not move it. Moved ahead of campaign A (s127): Q7 gates the step 2 merge, the last item before the item 6
   Phase 5 build (roadmap order s121; item 7 step 3 comes after Phase 5, `item7-evm-lanes.md`).
4. **Campaign A** (~4 h 40): 7 arms on M, 300 markets, 600 s, 3 replicates, rotating order, native load only.
   **Shape: the `hsdb-300m` shape** (Classic, 15 s checkpoints, oracle on), NOT the harness default
   `TORUS_BOOK_ROWS=3`: mode 3 turns checkpoints off, and then Q1 and DB do nothing.

   | arm | env |
   |---|---|
   | base | all flags absent (list them in `absent_env`); D, M, T unset = on, as shipped (no =0) |
   | F (Q1) | `TORUS_EPOCH_FAST_PATH=1` |
   | PE (Q2) | `TORUS_PARALLEL_ENGINE=4` (N >= 2 workers; 0, 1 or anything else = off) |
   | PC (Q4) | `TORUS_PARALLEL_CANCEL_ALL=8` (unset, 0, 1 or garbage = the serial loop) |
   | KP (Q4) | `TORUS_EXEC_PARALLEL_KEYS=1` (only "1" turns it on) |
   | DB (Q4) | `TORUS_CKPT_DEFER_BOOKS=1`; check its `defer_books=true` info line in each node's log (it logs `false` if gated off) |
   | C (Q10) | `TORUS_SHARD_CUSTODY=1` (overrides `env.sh`'s pinned 0); fresh data dirs per cell and a free-disk check first (Items, Q10) |

5. **Q4 winners together:** baseline vs the flags that won in step 4, combined.
6. **Q10 10-market cell** (conditional): only if campaign A's arm C shows no cost at 300 markets (Items, Q10).
7. **Q9. Data-dir growth: main vs main + RH4** (60 min soak per arm plus the idle phase, ~2.5-3 h; Items, Q9).
   **NOT READY: waits on Q10 and the owner's custody decision** (Items, Q9, "Arm b depends on Q10"). Moved here in
   s127: nothing in steps 2-6 needs RH4 (its GC only runs with `--retention-blocks`); only pre-testnet calls (prune by
   default, validator retention N) and the RH4 merge wait on it.
8. **Q6. 71.8 checkpoint slicing** vs M, when 18c delivers the code (not built yet; 18c adds the
   `persist_commit_manifest` timer first). Run after steps 4-5 have settled the Q1 / Q4 defaults (both touch
   checkpoint code, so they move slicing's baseline).

Done / dropped: **Q5** native vs native + EVM (campaign C, done s64: E/N 0.876, -12.4% matched/s; EVM heights never
pipelined, ~0.5 s exec idle before each, section 80). **Campaign B** (propose-defer) dropped (owner s126; branch
parked); revisit rule and arms: `docs/plans/item6-phase5-impl.md`, "Parked for a decision at Phase 5", item 3.

If Q1 or `TORUS_CKPT_DEFER_BOOKS` becomes a default, re-check the checkpoint and save-books rows of the Q8 profile
(a short re-run, not a full profile).

## Items

- **Q1. Epoch-boundary fast path** (`TORUS_EPOCH_FAST_PATH`, in main since `efe9aa47`, default off). Runs in campaign A.
  - Arms: J (flag absent, list it in `absent_env`) vs F (`"env": {"TORUS_EPOCH_FAST_PATH": "1"}`). Native load only
    (a boundary block with an EVM tx stays serial).
  - Arm F took effect: `node-env-all.txt` shows the flag on all 3 nodes; `zgrep -c 'epoch fast path ON'
    val*.log.gz` gives 1 per node; `executing finalized block height=..00 ... pipelined=true` about 18-19 times per
    node per 600 s (J: 0).
  - Metrics, per node on heights `h % 100 == 0`: the gap from 'block done' at h-1 to 'executing finalized block' at
    h (J ~500-530 ms, F ~1 ms); the boundary block's run time (J 364-479 ms); matched/s (expected +1.2-2%). Also
    `exec_checkpoint_upkeep_seconds`, `flush_worker_checkpoint_seconds`, commit-interval p99.
- **Q2. `TORUS_PARALLEL_ENGINE`** A/B (setting only, no code). Runs in campaign A.
- **Q3.** Archy's own s124 candidates (for example the push-manifest threshold): tracked by archy, slotted by archy.
- **Q4. The s123 experiments on main** (flags off): `TORUS_PARALLEL_CANCEL_ALL` (merge 7f9bb3a9; flag-off allocations
  = main), `TORUS_EXEC_PARALLEL_KEYS`, `TORUS_CKPT_DEFER_BOOKS`. They run in campaign A, the winners together in step
  5. Branch notes: `/home/18c/projects/wt/exp-s123/<name>.md` (18c). The fourth, propose-defer, is parked (step 6).
- **Q5. Native vs native + EVM transfers** at N = 11 (`EVM_LOAD_RATE` and `EVM_LOAD_SENDERS`, off by default). Shows
  what EVM blocks cost native matched/s.
  - Uses the setup genesis (an old `testnet/genesis-weighted-full.json` without the `FORCE=1` rebuild keeps N = 5).
  - Build `torus-node`, `bench-throughput` and `torus-tx-flood` (release) into `$T`, then a smoke cell:
    `FORCE=1 EVM_LOAD_RATE=10 TARGET_DIR=$T tools/matched-bench/campaign/detach.sh evm-smoke
    ~/bench-results-matched/evm-smoke.log tools/matched-bench/run-cell.sh "$PWD" evm-smoke-10m 10 120 76000`.
  - Checks: `summary.json` `.evm_load` has `ok` true, `interval` 11, `off_cadence_blocks` 0, `landed` about equal to
    `accepted`, `rc` 0; `jq '[.evm_block_txs[][0] % 11] | unique' evm-load-scan.json` gives `[0]`; the cell still
    passes `health.py accept`. A cell that asked for EVM load and did not get it is REJECT in `summary.json`.
  - Then the A/B (campaign C): arms without and with `EVM_LOAD_RATE`, 300 markets, 600 s; compare matched/s, block
    time and the blocks at EVM heights. At 10 tx/s from 40 senders an EVM block holds ~34 transfers (cap 142 at 3M
    gas).
- **Q6. 71.8 checkpoint slicing** vs option A (= main), after the slicing build (section 74.6 next cells): 300
  markets, 600 s, n = 3 ABBA (4-5 per arm resolves a ~200 ms step). Success: in-window block p99 within ~100 ms of the
  out-of-window p99, and `on_committed_block` in-window = out. 18c adds a timer around `persist_commit_manifest` first.
  Waiting on 18c code.
- **Q7. Item 7 step 2**, native load only, main vs step 2 (after the step 2 build and the 18c devnet check): the best
  bid/ask rows add up to one write per dirty market per block. Full spec: `docs/plans/item7-step2-impl.md`,
  "Archy native-only A/B". Two binaries (commit vs commit, no flag): arm A = main at the step 2 merge base, arm B =
  the step 2 head (18c gives both SHAs).
  - Genesis: the 300-market weighted genesis; arm B with the new step 2 keys absent (M = 0, big blocks off). EVM
    load off.
  - Build each binary once and use one genesis for both shapes.
  - Shape 1, mode 3 (harness default, `TORUS_BOOK_ROWS=3`, `BLOCK_CAP=200`): warm-up cell, n = 4 ABBA rounds of
    120 s cells at 300 markets. No 10-market secondary (owner s126: step 2 adds one bbo write per dirty market per
    block, so 300 markets is the worst case).
  - Shape 2, Classic (the shipped shape): both arms `EXTRA_ENV='TORUS_BOOK_ROWS='`, 15 s checkpoints
    (`KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000`, as `tools/matched-bench/campaign-template.sh`; its `envok` Classic
    check), 300 markets, n = 4 ABBA. Mode 3 turns checkpoints off, so it alone would miss the bbo writes in
    `save_order_books`.
  - Report per shape: matched/s (primary), native blocks/s (indicative only, see above), `torus_exec_save_books_seconds`
    per native block, exec `block_ms`, block time p50/p99, CPU-s/1M.
  - Pass, both shapes: paired B/A matched/s 95% CI includes 1.0 or lies above it, and save_books rises by at most
    0.3 ms per native block. A fail blocks the step 2 merge (follow-up: B16's `saved_bbo` hit rate).
  - Doc target: `docs/perf/item7-step2-native-ab.md`.
- **Q8. Item 6 Phase 5 step 0 profile:** see the message sent to archy (s125) and `docs/plans/item6-phase5-impl.md`
  "Step 0" (on main).
- **Q9. Data-dir growth: main vs main + RH4** (s126, from s58safe, section 79): in archy's 60 min soak each node's data
  dir grew about 0.19-0.2 GB/min; open files 351 -> 667. 18c's code read (not measured): committed action bodies are
  kept twice and never deleted: DA bodies in `<data_dir>/consensus-db` (`cf_native_pending`, no production delete) and
  full block bodies in the state DB (pruned only with `--retention-blocks`, which the harness doesn't pass). The
  "no-load" phase still committed blocks full of margin-rejected orders (rejects are counted inside execution), so it
  was not idle. Open files follow the SST count (`max_open_files` unset). 18c ports the RH4 DA body GC to
  `perf/rh4-da-body-gc` (GC only on pruning nodes; window `max(N, 1000) + 128` blocks) with the harness env var
  `TORUS_RETENTION_BLOCKS`; 18c says when the branch is ready (SHA then).
  - Arm b depends on Q10 (s127). The branch is pushed as `origin/perf/rh4-da-body-gc` 4514573f, a backup, NOT the
    Q9 SHA. Custody OFF by default after Q10: arm (b) = that head with `main` merged in, custody off as specified.
    Custody stays ON: 18c first builds the RH4 shard-count fix (option d; today a validator-set shrink leaves
    n_old - n_new shards per body of the ~3 boundary blocks), and arm (b) also runs `TORUS_SHARD_CUSTODY=1` so the
    shard GC is measured too. Either way 18c merges `main` into the branch once, runs the gates and gives the SHA.
  - Arms (owner s126, two binaries): (a) main; (b) main + RH4 with `TORUS_RETENTION_BLOCKS=1000` (adds
    `--retention-blocks=1000`). RH4's GC is OFF on archive nodes (a short window would strand a node that was down,
    without state sync), so (b) needs the retention to do anything; 1000 is a test value (the GC starts within
    minutes; the devnet's 100,000 is about 5-9 h and would delete nothing in 60 min). FRESH data dirs for every cell
    (bodies written before the RH4 binary are never GC'd). 60 min each, 300 markets, hsdb-300m shape (Classic, 15 s
    checkpoints), margin top-ups or a lower load; then a TRULY idle phase of at least 10 min (load generator stopped).
  - Sample per node every 60 s: `du -sb` of `<data_dir>/consensus-db` and of the state DB separately, SST count per DB,
    per-CF sizes from each RocksDB `LOG`'s Compaction Stats (`cf_native_pending`, block bodies),
    `torus_rocksdb_pending_compaction_bytes`, L0 file counts, open fds (`ls /proc/PID/fd | wc -l`) and deleted-but-open
    fds (`ls -l /proc/PID/fd | grep -c deleted`, expect 0), `torus_da_bodies_gc_deleted` and `torus_da_gc_seconds`
    (b). The harness summary needs the 40 consensus-DB columns (archy's `fix/harness-upstream-s64`, merged before Q9).
  - Also report per arm: matched/s, block rate, commit latency p50/p99 (the GC runs on the commit path).
  - Pass: (b) holds both `consensus-db` and the state DB flat once past the window (about 1,128 blocks), with
    matched/s and block rate within noise of (a). Then validators prune by default (owner s126: only if Q9 proves it);
    production retention N is decided after Q9.

- **Q10. Shard custody on vs off** (s127, setting only, no code). `TORUS_SHARD_CUSTODY=0` is pinned in
  `devnet/wsl/env.sh` since perf A1 (8fa6ccdb, Jul 16), so every bench so far ran custody off; the node binary
  ships it ON (only "0" disables) and custody-on was never measured. Custody runs synchronously on hotstuff-algo
  twice per view (leader `block_build_mirror`, every follower's `validate_block` decode, before the vote). s124's
  model (n = 3, not measured): ~95-145 ms per block per node, 0 to -25% matched/s at 300 markets, worse at 10. At
  N = 11 (k = 4) the per-shard proof work grows about n^2/k, so expect more.
  - 300 markets: arm C of campaign A (same binary, genesis and base arm). 10 markets: one ABBA pair base vs C
    (n = 3, 600 s) after the Q4 winners round (step 6), only if arm C is within noise at 300.
  - DISK: main never deletes shards (`cf_native_shards` in `<data_dir>/consensus-db`). Each body writes n shards of
    ~body/k plus proofs, about 2.75x the body bytes at N = 11: rough estimate 20+ GB per node per 600 s cell (x 11 if
    all nodes share a host). Fresh data dirs per cell, delete them after, check free disk before each C cell.
  - Report per arm: matched/s (primary), native blocks/s, block time p50/p99, `torus_validate_block_custody_seconds`
    (expect ~0 on base), `torus_block_build_mirror_seconds` (leader), hotstuff-algo CPU, CPU-s/1M, consensus-DB write
    bytes and `cf_native_shards` size, `torus_native_da_shard_recovered`.
  - Decision it feeds (owner): a real cost -> ship custody OFF by default until the validator set is large (roadmap
    T3.1), and the RH4 shard-count fix waits for T3.1 (likely lazy serve-time shards: nothing stored, nothing to GC).
    Within noise at 300 and 10 -> keep it on, 18c builds the RH4 shard-count fix (option d: max of the old and new
    set's n for 64 blocks after a set change, kept in the GC's own batch), benches switch to custody on, and Q9's
    arm (b) runs custody on (Items, Q9).

## Later

- HL-style storage for write-once, delete-by-age data (s127 design item, not built): HL keeps one file per height
  and prunes by deleting files older than 48 h (cron, `hyperliquid-dex/node` `pruner/`), so nothing is compacted and
  nothing counts what a file holds. Ours: full block bodies (`cf_block_bodies`, per height already: segment files,
  prune by unlink; first stop storing bodies twice, the s124 compact body record) and DA bodies (`cf_native_pending`,
  keyed by hash before a height exists: an append-only log plus a hash index, or RocksDB blob files as a
  config-only try). Gain is CPU and disk (s124: bodies ~15 s and DA ~113 s compaction CPU per 600 s cell), not
  matched/s.
- Big-block sizing with mixed load (native + swaps), only when big blocks are switched on (item 7 step 2 decision 1).
- Q5 rerun on the item 7 step 2 build (native vs native + EVM load): step 2 adds a lane-row read per EVM tx and the
  per-epoch fee rows. Only if Q7 shows a cost.
