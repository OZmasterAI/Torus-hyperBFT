# Bench queue (archy)

Benches run on archy only. 18c keeps this list current; archy reads it on pull. IDs (Q1, Q2, ...) stay fixed when an
item moves between sections.
Last archy bench: `ozarchy-hsdb`, 71.8 option A A/B (section 74 of `ozarchy-antispam-item6-pf1-2026-10-04.md`), 2026-10-15.

## Order (owner s125)

Every campaign uses the N = 11 bench genesis (rebuilt once with `FORCE=1`, step 1). Arms that compare against main
share one binary and one all-off baseline inside a campaign.

**Now, on current main** (code = origin/main `bb9eb20f`; the Q4 experiments are still being finished on 18c):

1. **Setup:** build binary M from current main, rebuild the bench genesis with `FORCE=1` (N = 11), run the Q5 smoke
   cell and its checks (Q5 below). Gates every later campaign.
2. **Q8. Item 6 Phase 5 step 0 profile** (campaign P, profile only, no A/B). Sent to archy directly (s125); it decides
   whether and how much of Phase 5 gets built.
3. **Q5. Native vs native + EVM transfers** (campaign C): 2 arms on M.

**After 18c merges the Q4 experiments** (flags off; rebuild M' = main + Q4):

4. **Campaign A:** one shared all-off baseline + Q1 (`TORUS_EPOCH_FAST_PATH=1`) + Q2 (`TORUS_PARALLEL_ENGINE=1`) +
   Q4 `TORUS_PARALLEL_CANCEL_ALL=1`, `TORUS_EXEC_PARALLEL_KEYS=1`, `TORUS_CKPT_DEFER_BOOKS=1`: 6 arms, 300 markets,
   600 s, 3 replicates, rotating order, native load only.
5. **Q4 winners together:** baseline vs the flags that won in step 4, combined.
6. **Campaign B, Q4 propose-defer** (`TORUS_PROPOSE_EXEC_WATERMARK`): off vs on, its own campaign (consensus timing:
   watch view timeouts and exec lag).

**When 18c delivers the code:**

7. **Q6. 71.8 checkpoint slicing** vs M. Run after step 4 has settled the Q1 / Q4 defaults (both touch checkpoint
   code, so they move slicing's baseline).
8. **Q7. Item 7 step 2** vs M.

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
- **Q4. The four s123 experiments**, one flag each, default off: `exp/s123-parallel-cancel-all-book-pass`
  (`TORUS_PARALLEL_CANCEL_ALL`), `exp/s123-propose-defer-on-exec-queue` (`TORUS_PROPOSE_EXEC_WATERMARK`),
  `exp/s123-exec-action-keys-parallel` (`TORUS_EXEC_PARALLEL_KEYS`), `exp/s123-defer-book-blobs-in-layer-merge`
  (`TORUS_CKPT_DEFER_BOOKS`). Waiting on 18c: rebase, finish, test, merge with the flags off (in progress s125).
  Three run in campaign A, the winners together in step 5, propose-defer in campaign B.
- **Q5. Native vs native + EVM transfers** at N = 11 (main `639ab185` and later; `EVM_LOAD_RATE` and
  `EVM_LOAD_SENDERS`, off by default). Shows what EVM blocks cost native matched/s.
  - Rebuild the bench genesis with `FORCE=1`: an existing `testnet/genesis-weighted-full.json` keeps N = 5.
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
- **Q7. Item 7 step 2**, native load only, main vs step 2 (after the step 2 build): the best bid/ask rows add up to
  one write per dirty market per block; two book shapes: mode 3 (harness default) and Classic with 15 s checkpoints
  (the shipped shape). Waiting on 18c code.
- **Q8. Item 6 Phase 5 step 0 profile:** see the message sent to archy (s125) and
  `docs/plans/item6-phase5-impl.md` "Step 0" on `origin/docs/item6-phase5-plan`.

## Later

- Big-block sizing with mixed load (native + swaps), only when big blocks are switched on (item 7 step 2 decision 1).
