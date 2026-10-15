# Bench queue (archy)

Benches run on archy only. 18c keeps this list current; archy reads it on pull.
Last archy bench: `ozarchy-hsdb`, 71.8 option A A/B (section 74 of `ozarchy-antispam-item6-pf1-2026-10-04.md`), 2026-10-15.

## Ready

1. **Epoch-boundary fast path** (`TORUS_EPOCH_FAST_PATH`, main `efe9aa47`, default off).
   - One binary, two arms: J (flag absent, list it in `absent_env`) vs F (`"env": {"TORUS_EPOCH_FAST_PATH": "1"}`).
     Same setup as `hsdb-300m`: 300 markets, 600 s, 3 replicates, rotating order, native load only (a boundary block
     with an EVM tx stays serial).
   - Arm F took effect: `node-env-all.txt` shows the flag on all 3 nodes; `zgrep -c 'epoch fast path ON'
     val*.log.gz` gives 1 per node; `executing finalized block height=..00 ... pipelined=true` about 18-19 times per
     node per 600 s (J: 0).
   - Metrics, per node on heights `h % 100 == 0`: the gap from 'block done' at h-1 to 'executing finalized block' at
     h (J ~500-530 ms, F ~1 ms); the boundary block's run time (J 364-479 ms); matched/s (expected +1.2-2%). Also
     `exec_checkpoint_upkeep_seconds`, `flush_worker_checkpoint_seconds`, commit-interval p99.
2. **`TORUS_PARALLEL_ENGINE`** A/B (setting only, no code). Already in archy's paused queue.
3. Archy's own s124 candidates (for example the push-manifest threshold): tracked by archy.

## Waiting on 18c code

4. **The four s123 experiments**, one flag each, default off: `exp/s123-parallel-cancel-all-book-pass`
   (`TORUS_PARALLEL_CANCEL_ALL`), `exp/s123-propose-defer-on-exec-queue` (`TORUS_PROPOSE_EXEC_WATERMARK`),
   `exp/s123-exec-action-keys-parallel` (`TORUS_EXEC_PARALLEL_KEYS`), `exp/s123-defer-book-blobs-in-layer-merge`
   (`TORUS_CKPT_DEFER_BOOKS`). Not built yet; 18c rebases, finishes and tests them, then merges them with the flags
   off. Bench plan: one binary, arms all-off and each flag alone, then the winners together; propose-defer in its own
   campaign, since it changes consensus timing (watch view timeouts and exec lag).
5. **Native vs native + EVM transfers** at N = 11 (`EVM_LOAD_RATE`, off by default), after branch
   `chore/evm-n11-bench-evm-load` merges. Shows what EVM blocks cost native matched/s.
6. **71.8 checkpoint slicing** vs option A, after the slicing prototype (section 74.6 next cells): 300 markets,
   600 s, n = 3 ABBA (4-5 per arm resolves a ~200 ms step). Success: in-window block p99 within ~100 ms of the
   out-of-window p99, and `on_committed_block` in-window = out. Add a timer around `persist_commit_manifest` first.
7. **Item 7 step 2**, native load only, main vs step 2 (after the step 2 build): the best bid/ask rows add up to one
   write per dirty market per block.

## Later

- Big-block sizing with mixed load (native + swaps), only when big blocks are switched on (item 7 step 2 decision 1).
