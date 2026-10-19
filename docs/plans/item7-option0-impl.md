# Implementation Plan: item 7 step 3 option 0, the EVM fast path

Status (s128): PLAN revision 2, nothing built. Design: `docs/plans/item7-step3-design.md` revision 2 (on main;
section 7 picks 1-11 accepted by the owner s127). This plan covers option 0 only: design phases P1 and P2 (section 4.1). T (P3) gets
its own plan after P2 merges; C0 stays parked (design question 6). Format: `item7-step2-impl.md`,
`item6-phase5-impl.md`.

**Revision 1 (s128), after the Codex adversarial review of `18d18d6a` (needs-attention, three findings):**
- R1, all-skipped EVM blocks: a successful EVM section whose txs are all skipped still writes CF_BLOCK_HASH_TO_NUMBER
  today. F2, section 3, tasks 2.1, 2.4, 3.2, 3.3 and the matrix now carry the side rows on every job kind, the Marker
  included (decision 11).
- R2, the harness: `p5_compare` cannot compare nodes with different checkpoint policies, and `P5Node` drains W after
  every block, which hides in-flight reads. New task 0.4 (per-height ledgers, reference snapshots by durable height,
  W-gated asynchronous runs); section 5 and tasks 0.2, 0.3, 3.5, 3.6, 3.8 rebuilt on it (decision 12).
- R3, F1: confirmed on main and fixed on its own branch `fix/precompile-read-error-fail-stop`. Gate D1 (section 4):
  no flag-on execution before that fix is in the branch's base. Task 1.1 keeps the classification audit.

**Revision 2 (s128), after the Codex round 2 review of `a719620a` (needs-attention, two harness gaps):**
- R4, crashes: dropping a gated context is not a crash. It releases the gate, W drains every held job, and a healthy
  context writes a final checkpoint (F10). Tasks 3.4, 3.5 and C19 now crash through a latched fail-stop (the
  `ckpt_crash_replay_with` mechanism) and assert that the marker stays behind, the side rows stay absent and the lost
  heights replay. A guard test pins the drain (decision 13).
- R5, snapshots: a snapshot keyed by the DB marker can show a height whose EVM mirror resync has not run yet (F11).
  Task 0.4 now takes its snapshots in a completion hook after the resync, tagged with the completed height; tasks
  0.2, 3.4, 3.5 and section 5.1 compare by completed height.

**Base: main `47c5f6fb`.** Every `file:line` below is on that commit (crate paths under `crates/`), unless marked
"design" (the design's anchors, written on `dcee5cac` / `c567b1b3`). Section 2 re-checks every design anchor.
Since the design:
- item 7 step 2 is merged (`cfa35ed1`), so the design's `@ c567b1b3` code is on main;
- the design's P1 (a), the revm read-error fail-stop (question 11), is merged (`27f49ad3`), with two sibling fixes:
  a state-root compute read error in catch-up fail-stops (`dab088b3`), and so does a failed metadata write
  (`17456488`);
- item 6 Phase 5 is PARKED after step 1 (owner s128, results section 91): option 0 builds on main as it is, not on
  Phase 5 step 2. Phase 5 step 1's differential harness (`phase5_differential_*`, `app.rs:28245-29080`) is on main and
  is the base of this plan's equivalence harness (its node and stream; not its comparator or its drain, see F5);
- F1 (2.2) is being fixed on `fix/precompile-read-error-fail-stop` (worktree `/home/18c/projects/wt/f1-precompile`; no
  commit on it yet at this revision). This plan depends on it (gate D1);
- code defaults since `3429855d`: `TORUS_PARALLEL_CANCEL_ALL` 8, `TORUS_EPOCH_FAST_PATH` on, header re-send 250 ms,
  push threshold 512 KB; shard custody off.

"(estimate)" marks a number not measured. Nothing in this plan changes consensus rules, the genesis or anything a
contract can see.

```bash
git -C /home/18c/projects/Torus-hyperBFT worktree add /home/18c/projects/wt/item7-opt0 -b feat/item7-option0 main
cd /home/18c/projects/wt/item7-opt0
export CARGO_TARGET_DIR=/home/18c/.cargo-target-item7opt0   # never share a target dir with another worktree
L="flock -w 60 -o /home/18c/projects/wt/.cargo-heavy.lock"   # exit 1 after the wait = lock busy, report it
F="--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar"
P="-p torus-types -p torus-genesis -p torus-bridge -p torus-consensus -p torus-economics -p torus-mempool \
   -p torus-rpc -p torus-core -p torus-evm -p hotstuff_rs -p torus-node -p torus-state -p torus-integration-tests \
   -p torus-wallet"
# one -p set for every cargo call (other subsets re-unify features and rebuild librocksdb-sys)
```

## Design decision (short)

Behind the node-local flag `TORUS_EVM_FAST_PATH` (default off until Q11 passes, then on), a block with EVM txs takes
the pipelined path like any native block. Its EVM part reads through the block's layered overlay (own pending ->
parent -> L -> C -> DB), never the raw DB. Its EVM rows (bundle plain rows, writer-precompile journal rows) enter the
block overlay, so they ride the frozen set, the digest, L and the checkpoint. Its EVM trie work moves to the flush
worker W. Its receipts, hash index, tx index and CoreWriter tx links become side rows of its W job. The EVM order in
the block, the queue targets and every consensus row stay as they are: `h_n`, state, receipts and roots are
byte-identical with the flag off and on. Only timing changes, so validators may run mixed flags.

Two rules from s128 shape the plan:
1. **Equivalence is proven against today, not against itself.** Step 0 captures a golden record of today's outputs
   (main `47c5f6fb`, serial and pipelined-barrier) on a stream that reaches every EVM path. Every later step must
   reproduce it with the flag off and on.
2. **The harness proves it reached every path** (Phase 5 step 2's divergence hid in a path its harness never ran).
   Path counters are asserted per configuration, test switches force the paths the stream cannot reach, and a
   planted raw-DB read of each reader class must make the harness fail.

## 1. Success criteria

| gate | target | measured by |
|---|---|---|
| flag off = today | every height's `h_n` write set, every CF dump (consensus, receipts, tx index, hash -> number, CoreWriter tx links, action status, CF_HASHED_*, CF_TRIE_*), the running hash and the RPC answers equal the step 0 golden of main `47c5f6fb` | tests 0.2, 1.x, 3.6 |
| flag on = today | the same golden, in every checkpoint policy, with EVM heights pipelined, in drained (D) and W-gated asynchronous (G) runs, compared by height and by completed height (task 0.4's hook snapshots, never a marker-keyed read) | test 3.6 |
| coverage | every path in section 5's table counted >= 1 in every (configuration, run mode) that can reach it, layer rows counted only for reads served by IN-FLIGHT layers; each planted bypass and each in-flight-only planted fault (section 5.4) makes the harness fail | test 0.4, 3.6, 3.7 |
| F1 first | the F1 fix (`fix/precompile-read-error-fail-stop`) is in the base before any flag-on run | gate D1 |
| no raw read | the tripwire (task 1.6) gets 0 hits over the whole stream with the flag on | test 3.6 |
| local faults | every new read path's error fail-stops (never a revert, never a skipped EVM part); the held jobs are refused, not written (marker stays at the pre-fault height, no side row above it); restart replays exactly the lost heights to the uncrashed state | tests 1.1, 3.4, 3.5 |
| R and L unshared | `rows_shared_fallbacks` 0, `torus_exec_checkpoint_layer_shared` 0 over the stream with the flag on | test 3.6 |
| Q11 | the exec-path merge rule (bench-queue.md) on E-on / E-off, plus section 6's passes | Q11 |
| mixed flags | three replicas (off; on 15 s; on interval 0) and a 3-node devnet cell agree on every `h_n` and attestation | test 3.8, Q11 extra cell |

## 2. Anchor re-check on `47c5f6fb`

### 2.1 Drift table (design anchor -> main)

Most anchors only moved (step 2 and the three fail-stop fixes added code above them). Rows marked **changed** moved
and changed meaning.

| what | design | main `47c5f6fb` | note |
|---|---|---|---|
| `execute_committed_block_with` | `app.rs:3003` | `app.rs:3273` | |
| pipelined predicate (`!has_evm`) | `3163-3173` (`@c567` 3391-3397) | `3433-3439` | same terms |
| `pipeline_barrier` | `2509-2550` | `2779-2814` | |
| `checkpoint_drain` | `2856-2882` | `3126-3152` | |
| `has_evm` | `3110` | `3380` | |
| replay is serial (comment) | `3155-3156` | `3425-3426` | |
| EVM section | `3243-3365` | `3513-3671` | **changed:** `BridgeError::State` arm fail-stops (`3658-3666`, `dab088b3`); metadata write error fail-stops (`3621-3633`, `17456488`) |
| EVM batch build, tx links | `3300-3333`, `3319-3327` | `3573-3614`, `3589-3600` | |
| `commit_block_metadata` wrapper | `2377-2387` | `2647-2657` (call `3621`) | |
| `core_writer_due` wrapper | `2353-2362` | `2623-2632` (call `3702-3712`) | |
| `LocalFault` arm / generic arm | `3352-3360` / `3361-3363` | `3642-3650` / `3667-3669` | the comment now names revm `EVMError::Database` too |
| overlay built after the EVM part | `3415-3448` | `3713-3755` | |
| `checkpoint_read_layers` | `2643` | `2913-2943` | |
| `run_native` on fee revenue | `3476-3480` | `3784-3790` | **changed:** also `evm_gas != (0, 0)` (step 2 F3, `3786`). Every EVM block that executed a tx runs the native phase. See 2.2 F2 |
| `begin_resident` | `3747` | `4055` | `seed_from_bundle` call at `4013` |
| native order: oracle, batches, drain, liquidations, fees, epoch | `3870-3990`, `3954-3956` | `4205`, `4219`, `4268`, `4289-4304`, `4324-4325` | |
| serial flush with the EVM prefix | `4312-4400`, `4337-4345` | `4627-4735`, `4672-4679` | |
| fast-path hand-off; assert "EVM blocks are never pipelined" | - | `4572-4626`; `4573` | |
| resync on E | `4402-4417` | `4736-4748` | |
| no-native EVM block: status + marker-only | `4575-4646` | `4900-4975` | |
| checkpoint triggers | `2767-2773` | `3037-3043` | |
| `take_layer` | `220-252` | `220-254` | |
| header copies parent root, zero receipts root | `7403-7406` | `7794-7797` | |
| header / body written at dispatch | `1582-1640` | `1633-1660` (record), `1740-1833` (`persist_committed_block_durably`) | |
| `pipeline_handoff` (Flush / Marker -> Digest) | - | `2820-2870` | |
| tests: `fast_path_run`, `epoch_fast_path_boundaries_pipelined_and_match_serial`, `epoch_fast_path_evm_or_slash_boundary_stays_serial`, `epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` | `27259`, `27150`, `27347`, `27389` | `30488`, `30597`, `30685`, `30727` | |
| tests: `exec_pipeline_crash_before_write_replays_both_and_matches_serial`, `corewriter_evm_row_drains_in_empty_next_block` (`_with_interval`), `core_writer_tx_links_each_queued_row_to_its_evm_tx`, `f3_epoch_gas_sets_plan_fees_serial_and_pipelined` | `17898`, `23846` / `23935`, `20688`, `21353 @c567` | `18687`, `25834` / `25923`, `21585`, `21496` | |
| Phase 5 harness: `p5_configure`, `p5_blocks`, `P5Node`, `p5_compare`, `p5_differential`, `p5_differential_matrix` | - | `28289`, `28416`, `28680`, `28820`, `28924`, `28966` | new on main (Phase 5 step 1) |
| flag template | `exec_pipeline.rs:57-66` | `57-94` | **changed:** the epoch flag is default ON since s128 (`parse_epoch_fast_path`) |
| `Job` enum | - | `exec_pipeline.rs:288-349` | |
| action status as side rows | `866-880` | Digest `877-889`, Flush `997-1006` | |
| `resync_evm` after a checkpoint | `948` | `957` (Flush `1054`, fn `1078`) | |
| revm reads the DB (`with_database_ref`) | `torus-evm/src/executor.rs:282-294` | `326-337` (`execute_block_lanes`) | |
| journal `NativeStateOverlay::new(state_db)` | `303-305` | `355-357` | |
| `map_evm_err` | `673-680` (`@c567` 733) | `759-772` | **changed:** `EVMError::Database` -> `LocalFault` (`27f49ad3`): design P1 (a) is done |
| `local_fault_or` | `662-671` | `738-748` | |
| provider fault cell | `precompile_provider.rs:26-48, 83` | `25-48, 83`; recorded at `208-216` | only `CoreError::PrecompileLocalFault` is recorded: see 2.2 F1 |
| precompile readers | `precompiles.rs:607-990` | dispatch `594-607`, readers `617-990` | |
| CoreWriter enqueue (R8) | `1397-1530 @c567` | `1397-1521` | doc `1407` still says "serial barriers" |
| `next_sequence` | `1397-1409` (design, main) | `1597-1612` | |
| `pending_count(&StateDb)` (R5) | `1571-1590 @c567` | `1572-1590` | raw RocksDB prefix iterator |
| spill walk / `candidate >= next_block` | `1423-1470` / `1436-1440 @c567` | `1442-1466` / `1438-1441` | |
| writer precompiles | `996-1285` | `core_writer` `1049`, `core_writer_staking` `1163`, `lockbox_precompile` `1278` | |
| `GAS_PRECOMPILE_WRITE`, `is_order` | `57`, `1726-1734 @c567` | `57`, `1726` | |
| EVM root on E (R3) | `validator.rs:198-200` | `233-236` | |
| lane read on the DB (R7) | `validator.rs:154-180 @c567` | `154-178` | comment "runs after a barrier, so the DB is current" |
| catch-up skips the root check | `101-110` | `107-116` | |
| `core_writer_due` / doc | `native_executor.rs:13520-13522` / `13596-13609 @c567` | `13629-13631` / `13619-13628` | doc "EVM blocks are serial barriers" |
| `oracle_due` | `13524-13531` | `13638-13640` | |
| drain assertion `queued < drain height` | `13535-13536` (`@c567` 13624) | doc `13647`, check `13713` | |
| `credit_native` in the drain | `13650-13655`, `13743` | `13766` | |
| `end_resident` / R fallback counter | `4568-4590` / `4167-4171` | `4582-4614` / `4169-4171` (increments `4747`, `4765`) | |
| `classify_action` | `13939-13968` | `14052-14064` | |
| `commit_block_metadata` | `committer.rs:144-197` | `145-197` | |
| `DatabaseRef for StateDb` | `torus-state/src/db.rs:1474-1503` | same | block hash `1499-1500` via `get_block_hash` `1297` (CF_BLOCK_HEADERS) |
| layered overlay reads | `backend.rs:1366-1458` | `1393-1498` (`with_split_layers` `1422`, `lookup_lower` `1488`) | |
| deferred-merge read error | `1430-1440` | `1470-1478` | `StateError::Io` |
| `StateDb: StateBackend` / `db()` | `222` / `1724` | `222` / `1763` | |
| overlay DB fall-through reads | `2460, 2576, 2632, 2684` | `2499` (get), `2615`, `2671`, `2723` (iterators), `get_cf_len` `2629`, `prefix_exists` `2690` | |
| `seed_from_bundle` (CF_ACCOUNTS only) | `1702-1721` | `1741-1760` | |
| `HashExtras::add_evm_bundle` | - | `890-923` | |
| `apply_bundle_plain` / `apply_bundle_hashed` | `incremental.rs:270-311` / `322-367` | `270-310` / `319-379` | |
| `incremental_evm_root` / `evm_block_batch_incremental` | `240-264` / `322-414` | `236-264` / `396-414` | |
| `resync_evm_accounts` / `ensure_trie_built` | `416-490` / `124-137` | `431-479` / `131-137` | |
| root routing (incremental + debug full-scan oracle) | `state_root.rs:24-29, 37-60, 119-128` | `24-29, 37-60, 119-130` | the debug oracle (`evm_root_routed` `50-58`) reads the DB too: see 2.2 F6 |
| `INGRESS_CFS` / freshness contract | `read_view.rs:58-67` / `20-26` | same; `ReadView::height()` `161` | |
| `DEFERRED_CFS` | `checkpoint_layer.rs:26-41` | `26-40` | |
| hashed CFs (EVM ids 0x80-0x82) | `running_hash.rs:20-26, 35-62` | `21-27`, `35-64` | |
| `append_links(&StateDb)` | `corewriter_tx.rs:68-86` | `68` | |
| pruner cutoff <= marker | `pruner.rs:337` | `203-206` | |
| mempool prune (R9) | `torus-mempool/src/lib.rs:2120-2195 @c567`, `2152-2158` | `2120-2200`, `2154-2159` | **changed:** the caller is `torus-node/src/main.rs:1120-1130` (raw-DB marker); `notify_block_committed` has only a test caller |
| `pending_nonce` on the view | `lib.rs:2101-2114 @c567` | `2101-2113` | |
| eth reads / eth head | `torus-rpc/src/eth.rs:714-780` / `173-181` | `785-860` (`eth_call` `1140`) / `179-184` | |
| `CORE_WRITER_DRAIN_CAP` | `torus-types/src/lib.rs:781 @c567` | `781` | |

### 2.2 Findings from the re-check (not in the design)

- **F1 (gap on today's path too). A local read error inside a precompile is a revert.** The provider records only
  `CoreError::PrecompileLocalFault` (`precompile_provider.rs:208-216`). A reader or the enqueue path that fails a
  read with `?` returns `CoreError::State` (for example `read_balances`, `precompiles.rs:712-720`; every enqueue read,
  `1424-1466`). That error is a revert: the tx's receipt has status false and its journal rows are dropped, on this
  node only. `CoreError::is_local_fault` (`torus-core/src/error.rs:165-192`) already classes `State`, `Borsh`,
  `MissingCf`, `BookCorrupt` as local. Today this needs a RocksDB error. On the fast path it gets a second source: a
  journal read that reaches L's deferred part after its merge failed (`backend.rs:1470-1478`). Design point 3 assumes
  these become local faults; the code does not do it. Confirmed by the Codex review (probe: a DB without
  CF_NATIVE_BALANCES gives `Ok`, one receipt, status false; the `Err` reaches the `Revert` arm at
  `precompile_provider.rs:244-248`; reader `torus-core/src/precompiles.rs:712`). The fix is on its own branch
  (`fix/precompile-read-error-fail-stop`, decision 3 settled); gate D1 makes this plan wait for it, and task 1.1
  keeps the classification audit.
- **F2. An EVM block that executed a tx always runs the native phase** (`evm_gas != (0, 0)`, `app.rs:3786`). The only
  EVM blocks without one executed nothing. There are two such cases, and they differ (R1):
  - **failed section** (generic arm `app.rs:3667-3669`, e.g. `ReplayFault::EvmSectionError` `3546`, a base-fee
    mismatch): no `commit_block_metadata`, so no side row at all; only the status row;
  - **all txs skipped, section `Ok`**: undecodable txs are dropped by the lossy decode (`validator.rs:144-145`) and a
    revm-invalid tx (wrong nonce, ...) by `skip_invalid` (`executor.rs:394-397`). `validated.receipts` is empty, gas
    `(0, 0)`, fee revenue 0, bundle and journal empty, but the `Ok` arm still calls `commit_block_metadata`
    (`app.rs:3621`). Its receipts and tx-location loops are empty (`committer.rs:175-182`, `188-193`), but it writes
    the header (`167`), the body (`173`) and **CF_BLOCK_HASH_TO_NUMBER unconditionally (`committer.rs:184`)**. Codex
    probe on main: `all_skipped: receipts=0 gas=0 bundle_accounts=0 journal_rows=0`. Header and body are dispatch rows
    on the fast path; the hash -> number row is a side row the fast path must still write.
  Both absorb no rows, so "absorbed rows and no native phase" cannot occur, and the no-native case needs no frozen
  overlay. But it is not side-row free: the Marker job carries the status row AND the block's `evm` job (empty bundle,
  side rows of the hash -> number row only) whenever the section returned `Ok` (tasks 2.4, 3.3). An all-skipped block
  can also run the native phase (a native action, a due CoreWriter row, an epoch boundary, an oracle or liquidation
  step: `app.rs:3784-3793`); its `evm` job then rides the Flush / Digest like any other. A debug assert pins "a
  Marker's `evm` has an empty bundle and no receipts".
- **F3. R9's production caller is `torus-node/src/main.rs:1120-1130`.** It reads the raw-DB marker, then
  `prune_committed_txs` reads raw-DB nonces. `notify_block_committed` has only a test caller.
- **F4. A stale EVM trie hits today's fail-stop.** Since `dab088b3`, a root-compute `StateError` in catch-up
  fail-stops. If W's EVM trie step fails and leaves the trie stale (design point 6), the next serial EVM block
  computes its root over a stale trie. Release builds then produce a wrong but unused root. Debug builds and
  `TORUS_INCREMENTAL_ORACLE` produce an oracle mismatch, which now fail-stops the node. The stale marker must
  therefore route `evm_root_routed` to the full scan (task 2.3), and boot must rebuild. `ensure_trie_built` today only
  builds into an empty CF_HASHED_ACCOUNTS (`incremental.rs:131-137`).
- **F5. The Phase 5 differential is the harness base.** It already has EVM blocks (lockbox deposits, creates),
  slashes, epoch boundaries, a fatal block with restart, serial and pipelined nodes in three checkpoint policies, and
  per-height `h_n` + action status + full CF dumps (`p5_compare`, `app.rs:28820`). It lacks reverts, CoreWriter
  orders (K, cap), lane rows and the big lane, reader precompiles, BLOCKHASH, EIP-161 wipes, EVM heights at
  boundaries, attest heights and slashes on purpose, and any coverage counters. Step 0 extends it rather than writing
  a second harness. Its `P5Path` switch becomes a per-node config (design 4.3 "must not foreclose" item 6).
  Two of its assumptions do not carry over (R2), so step 0 replaces its comparator and adds a second run mode
  (task 0.4):
  - `p5_compare` compares two nodes with the SAME checkpoint policy in lockstep. It requires equal durable markers
    after every block (`app.rs:28872-28874`), equal sets of heights hashed since the last call (`28836-28846`), and
    then dumps both current DBs (`28896-28913`). S (marker = h after every block) against an Every5 or Never node
    (marker at the last checkpoint) fails on legitimate checkpoint lag, not on a bug.
  - `P5Node::execute` drains W after every block (`wait_idle`, `app.rs:28778-28780`; `exec_pipeline.rs:635-641`).
    At interval 0 the parent's Flush is then durable before the next block runs, so the parent layer
    (`last_job`, `app.rs:3722-3726`, still held after its write) answers with bytes the DB already has; and C is
    discarded once durable (`checkpoint_read_layers`, `2929-2937`), so no read is ever served by a checkpoint in
    flight. Layer-hit counts then pass on already-durable data, and a reader that skips the parent layer or C reads
    the same bytes from the DB: the drained harness cannot see that bug.
- **F6. The debug full-scan oracle reads the DB.** `evm_root_routed` cross-checks against a full scan in debug
  builds (`state_root.rs:50-58`). On the fast path E computes no EVM root, so neither the root nor the oracle may run
  there (task 3.2).
- **F7. BLOCKHASH is safe from the DB.** `block_hash_ref` reads CF_BLOCK_HEADERS (`db.rs:1297-1304`). Every header
  below h is in the DB before h runs: dispatch writes it for pipelined blocks, and a non-durable header forces the
  serial path. The pruner never deletes headers (`pruner.rs` table). The adapter still reads it through the overlay,
  so the tripwire needs no exemption for it.
- **F8. No native reader iterates the CFs the journal writes**, apart from the drain (prefix h) and `next_sequence`
  (prefix target). Task 1.2's absorb makes h's journal rows (queue rows for targets >= h + 1, the CF_FEE_CONFIG
  cursor rows) visible to h's native phase. That equals today only if no native reader observes those keys in block
  h. Grep finds no other production iteration of CF_CORE_WRITER_QUEUE or CF_FEE_CONFIG. Task 1.2 pins it with a probe
  test, and the golden backs it.
- **F9. `validate_block_with_overlay` / `execute_block_with_overlay`** (`validator.rs:285`, `executor.rs:497`) are an
  older deferred-commit path with no production caller. Their precompiles read the raw DB (the very bug), so they are
  not a template. They stay as they are.
- **F10 (R4). Dropping a context with W gated lands its held jobs.** `Drop for FlushWorker`
  (`exec_pipeline.rs:677-687`) closes the channel, releases the gate and joins W, and `worker_loop`
  (`723-810`) still receives and writes every job already sent. Before that, `Drop for ExecutionContext`
  (`app.rs:2595-2603`) runs `pipeline_barrier`, a final checkpoint, whenever `exec_failed` is not latched. A bare drop
  is a clean shutdown, not a crash (Codex probe: `held_before_drop: marker=0`, `after_drop: marker=1`). W refuses to
  write only once `exec_failed` is latched (`exec_pipeline.rs:742-744`) or after an injected write failure
  (`gate.fail_next()`, `wait_open` `459-473`; the latch `787-798`, `latch_failstop` `818-825`). The Phase 3 crash
  template does exactly that before its drop: `fail_next`, `release`, then asserts `!wait_idle()` and
  `exec_failed` (`app.rs:29887-29897`), and only then drops and checks `durable < last` (`29899-29904`). The E-side
  seam `test_crash_after_evm_section` (`app.rs:3688-3690`) only returns. It latches nothing, so with W held a drop
  after it lands the parent's jobs too.
- **F11 (R5). The durable marker lands before the EVM mirror is resynced.** On W a Flush writes native state and the
  marker in one batch (`flush_on_worker`, `exec_pipeline.rs:1032-1039`; the result `1041-1052`), then
  `resync_evm` (`1054`, fn `1078-1089`) rewrites CF_HASHED_* / CF_TRIE_* for the native-credited EVM accounts in a
  separate write. A checkpoint does the same (`flush_checkpoint` `932`, `resync_evm` `957`). E's serial flush too:
  the batch with the marker at `app.rs:4674`, the resync at `4743-4753`. A marker-keyed snapshot can therefore see
  height h's native state next to h-1's EVM mirror (Codex probe: `same_marker=true same_hashed_account=false`). A
  RocksDB snapshot does not help: the interval lies between two separate writes. A height is complete only after
  `run_job` returns `Ok`, or `TrieStale` for a Checkpoint (a TrieStale Flush skips the resync, F4b)
  (`worker_loop` `766-786`, before `durable_height.store` and
  `finish_job`), or on E after the resync (`app.rs:4759`) or the marker-only flush (`4970`). W runs its jobs in
  order on one thread, so no later job writes before that point.
- **F4b (Codex P2 on `543980f0`, step 0). A TrieStale Flush on W skips the EVM mirror resync, for good.** When a
  Flush's batch lands but its native trie maintenance fails (only with `TORUS_NATIVE_TRIE_MAINTENANCE=1`; off by
  default), `run_job`'s Flush arm returns `TrieStale` before `resync_evm`, and nothing makes it up: the block's
  native-credited EVM accounts keep a pre-resync CF_HASHED_* / CF_TRIE_* mirror until a later block resyncs them, so
  the incremental EVM root drifts from the full scan (the F4 failure mode, from the native side). The three sites
  disagree today: a Checkpoint resyncs even on `TrieStale`, W's Flush skips it and goes on, and E's serial flush
  fail-stops on the same error (its flush `Err` arm). Step 0 changes no production code: the completion hook does not
  fire for a TrieStale Flush (its height is not complete), the next completed height records it, and
  `o0_trie_stale_flush_is_not_a_completion` pins the gap (a manual `resync_evm_accounts` fixes the row). Task 2.3
  closes it: resync on `TrieStale` as the Checkpoint does, or set F4's stale marker so the root routes to the full
  scan and boot rebuilds.

## 3. Shape of the fast-path EVM section

```
let pipelined = W && durable.header && durable.body
    && (!has_evm || self.evm_fast_path)                       // step 3
    && pending_slashes.is_empty()
    && (self.epoch_fast_path || !is_epoch_boundary(h));
if !pipelined && !barrier() { return }
slashes ...
let overlay = with_split_layers(db, parent, L, deferred, C);   // step 1: built BEFORE the EVM part (serial: no layers)
if has_evm {
    let reads = EvmBlockState::new(&overlay);                  // adapter (revm) + journal over the SAME layers, no R
    let _trip = raw_db_tripwire::arm();                        // test / debug builds only
    match validator.validate_block_for_catchup(block, &reads, exec, root = if pipelined { Skip } else { OnE }) {
        Ok(v) => {
            overlay.absorb_evm(&v.bundle, v.native_writes)?;   // step 1, both paths; journal consumed by value
            if pipelined { evm_job = Some(EvmJobRows { bundle: v.bundle, side: EvmSideRows::of(block, receipts, links) }) }
                                                               // on EVERY Ok, also 0 receipts (F2: hash -> number row)
            else         { evm_batch = hashed mirror + trie + links (today's prefix minus the plain / journal rows);
                           commit_block_metadata(...) }        // serial: unchanged
        }
        Err(LocalFault | State) => fail-stop, Err(other) => log (deterministic), as today
    }
}
core_writer_due(&overlay, h)                                   // step 1: through the overlay, after the absorb
... native phase unchanged (seed_from_bundle is gone: the absorb did it) ...
pipelined: freeze -> Flush / Digest { .., evm: evm_job }       // W: EVM trie step + side rows in the same batch
no native phase: Marker { .., action_status, evm: evm_job }     // F2: nothing absorbed; side rows if the section was Ok
```

W side (step 2): a Flush job with `evm` writes the block's EVM mirror and trie ops and its side rows in the block's
batch; a Marker job with `evm` writes its side rows in the marker batch (its bundle is empty). A Digest job (also the
one `pipeline_handoff` makes from a Marker with an interval) folds `evm` into the window. The checkpoint writes the window's EVM ops and side rows in its batch,
before the marker. The post-checkpoint `resync_evm` stays as a safety net; it finds every mirror already equal.

## 4. Steps and tasks

Test names are the RED tests each task adds. Each fails before its code (compile error or wrong value), except
those marked "guard", which pass before and must stay green. "Golden" = the step 0 record. Per-commit gate: TESTING.md
(workspace nextest + doc tests, shown in the commit's notes).

**Gate D1 (F1 dependency, R3).** No flag-on execution of any kind (task 3.1 onward: unit tests, the matrix, Q11)
before the F1 fix from `fix/precompile-read-error-fail-stop` is on main AND in this branch's base. Preferred order:
the fix merges first and `feat/item7-option0` is cut from that main, so the golden is captured on it. If step 0-2
work starts earlier on `47c5f6fb`, the branch rebases onto the fix's main before task 3.1, and the golden is
re-captured there by task 0.2's rebase rule (expected byte-equal: the stream has no storage fault; any difference is a
finding, not a re-bless). Task 3.1's commit names the fix commit in its message, and its guard test
`precompile_read_fault_fail_stops_block` (task 1.1) must already be green on the base.

### Step 0: the reference: golden record and harness extension (tests only, no behaviour change)

**Task 0.1: the option 0 stream (`o0_blocks`), extending `p5_blocks`**
- Test first: `o0_stream_reaches_every_input` (guard on the generator): the fed counters (`P5Fed` + new ones) are all
  >= their floor over 300 blocks.
- Code (`torus-consensus/src/app.rs` test module, next to `p5_blocks` `28416`). Add to the stream:
  - EVM content: plain transfers; a contract that writes and deletes slots; reverts (top-level revert after a
    `placeOrder`; a caught inner-frame lockbox deposit; reader OOG); CoreWriter `placeOrder` / cancel (K-delayed
    targets), staking writer calls; lockbox deposits; BLOCKHASH of h-1 and h-256; reader precompile calls
    (`getPosition`, balances, oracle, staking, bbo).
  - An account destroyed or EIP-161-emptied, then re-created with new storage inside one checkpoint window.
    - Destroy is same-block only: the EVM spec is CANCUN (`torus-evm/src/executor.rs:75`), so SELFDESTRUCT
      (EIP-6780) deletes only an account created in the same tx; a pre-existing account's storage can never be wiped.
      The stream's `destroy_recreate` (create + destroy in one tx, re-create by the next tx of the same block) reads
      no lower layer.
    - The cross-block deletion is EIP-161, and it is reachable (s131, `o0_eip161_native_drained_account_is_deleted_by_touch`):
      `Withdraw { to: Z }` writes Z's CF_ACCOUNTS row (nonce 0, KECCAK_EMPTY: `torus-core/src/lockbox.rs:146-194`,
      new row `236-247`); Z's `TransferToPerp` of its whole EVM balance rewrites the row with balance 0 and keeps it
      (`lockbox.rs:62-103`, `build_evm_balance_update` preserves nonce and code hash and never deletes); a zero-value
      EVM call to Z in a later block then deletes the row (revm state clear; `apply_bundle_plain` deletes
      CF_ACCOUNTS, `torus-state/src/incremental.rs:291-294`). A native credit re-creates it, and Z sends again
      from nonce 0. The stream runs this cycle for 5 addresses (`O0_EMPTIED`, shape `O0Recreate`, alternating):
      fund at e-3, drain at e-2, touch at the EVM height e (`e % 5 == 0`, so drain and touch share one Every5
      window), then the re-creation in one of two shapes, then Z's create (new storage) at the next EVM height:
      - **next-block** (3 cycles, fed `eip161_cross_block`, floor 3): Z's `TransferToSpot` at e+1, the first height
        of the next window. Seed `O0_SEED`: touches 20, 60, 90; reuse 25, 65, 95.
      - **same-block** (2 cycles, fed `eip161_same_block`, floor 2): a trader's `Withdraw { to: Z }` in block e's
        native phase, which runs after the EVM section: deletion and re-creation in one block, one window. Touches
        30, 70; reuse 40, 80.
      `o0_stream_eip161_cycles_delete_and_recreate` checks each row on the serial run (written, empty, then gone
      and re-created at nonce 0 / re-created in the touch block, then nonce 1).
      `o0_eip161_recreation_reads_by_layer` (serial, G interval 0, G Every5 with parked checkpoints) pins where
      the re-creation reads Z, with a key watch on the probe (`Probe::watch` / `set_height`, test-probes only: the
      per-(reader, layer, CF) counters cannot attribute a read to one key or height):
      - same-block: every native-phase read of Z in block e is answered by the block's own pending layer, the
        first with the tombstone `seed_from_bundle` put there for the EVM deletion (all three configurations).
      - next-block: **today the deletion is never read from an in-flight layer**. e is an EVM height, so it
        barriers (the barrier closes the window into a checkpoint and drains W, `pipeline_barrier`) and its
        serial flush writes the deletion to the DB with the marker. At e+1 the overlay has no parent, L or C, and
        the re-fund's first read of Z is a DB miss. The test pins that (no frozen-layer read of Z at e+1).
        With the flag on, e is pipelined and its deletion sits in the parent / C at e+1: task 3.6 asserts that
        read in flight (the stream already has the input).
      **Why no EVM re-creation inside the deletion's Every5 window.** The deletion is an EVM touch at an EVM
      height e, which with `TEST_EVM_BLOCK_INTERVAL` 5 is on the Every5 grid (e % 5 == 0; flag on, the
      checkpoint triggers at `height - base >= 5`, base a checkpoint height, attest and epoch heights are
      multiples of 5 too): e is the last height of its window. An EVM re-creation (a value transfer to Z or Z's
      own tx) needs a later EVM height, at least e+5, the last height of the next window. A re-creation by a later
      tx of block e itself collapses into the block's bundle (Z stays a put, no deletion reaches any layer), and
      today every EVM height ends its window anyway (barrier). The re-creation inside the window is therefore
      native: the same-block shape. The golden was re-written with the 5 cycles (serial, interval 0 and Every5
      agree).
  - `TransferToSpot` in a native block, spent by the next EVM height; `TransferToPerp` in the same block as EVM txs.
  - Native credits (fees, rewards) to EVM accounts the window's EVM bundles also touch.
  - Big lane: the fixture header carries `evm_big_block_interval` M = 10 (big heights `h % 10 == 0`, with
    `TEST_EVM_BLOCK_INTERVAL` 5 at `app.rs:262`); an `EvmUserModify` in a native block makes a sender big; a big
    height carries both lanes.
  - Forced EVM heights at: an epoch boundary (`h % 50`), an attest height (`h % 100`), a slash height, the fatal
    block's successor, and an EVM block whose section fails (`ReplayFault::EvmSectionError`, `app.rs:3546`).
  - All-skipped EVM blocks (F2, R1), two kinds, each forced (a) without a native phase (Marker), (b) with a native
    action in the same block (Flush / Digest), (c) at a checkpoint height of Every5 (the Marker -> Digest conversion,
    `app.rs:2863-2870`): **wrong-nonce-only** (every tx's nonce one above the sender's: skipped by `skip_invalid`,
    `executor.rs:394-397`) and **undecodable-only** (garbage bytes: dropped by `decode_txs_lossy`,
    `validator.rs:144-145`). Fed counters `evm_all_skipped_{nonce,undecodable}_{marker,native,ckpt}`.
  - Over 200 queued rows to one target (the cap spill), and a K-delayed target that falls after a checkpoint.
- Verify: `cargo nextest run -p torus-consensus $F -E 'test(o0_stream_reaches_every_input)'`.

**Task 0.2: the golden record of today (main `47c5f6fb`)**
- Test first: `o0_golden_today` runs the stream on today's code in three configurations and compares with
  `crates/torus-consensus/tests/fixtures/item7_opt0_golden.json`. The configurations: serial (no W); pipelined
  interval 0; pipelined Every5 (EVM heights barrier today). Each run fills a task 0.4 ledger, keyed by height, never
  by "after block h": the `h_n` write set hash of every height (`capture_take` returns `(height, entries)`,
  `running_hash.rs:623`), the action-status row, the receipts / tx-location / hash -> number / CoreWriter-link rows of
  that height, and per-CF dump hashes at each completed height the run reaches (every height for serial; the
  checkpoint heights for Every5), taken by task 0.4's completion hook, never by reading the marker (F11). The golden
  stores the per-height entries once (the three runs must agree) and the dump hashes at the completed heights all
  three runs share (multiples of 5 and the last height). At the end: a hash per CF of the
  full dump (all `ALL_CF_NAMES`, CF_HASHED_* and CF_TRIE_* included), the running hash, the EVM root (incremental ==
  full scan) and the RPC answers of 3.6's list.
  `TORUS_O0_GOLDEN_WRITE=1` writes the file; it is written once, on the step 0 commit, whose parent is `47c5f6fb`.
- Code: the recorder, plus `torus-rpc` as a dev-dependency of `torus-consensus` (no cycle: torus-rpc does not depend on
  torus-consensus). The RPC answers come from the eth handlers over the node's DB, after a final drain.
- Verify: write once, then `cargo nextest run -p torus-consensus $F -E 'test(o0_golden_today)'` green twice in a row
  (determinism). The three configurations must agree with each other already (they are today's serial and barrier
  paths).
- If the branch rebases onto a main that changes execution, re-run the step 0 commit on the new base, rewrite the
  golden there, and only then re-apply steps 1-4 (commit message names the base).

**Task 0.3: path counters and the coverage report**
- Code: a `test-probes` feature on `torus-state` (enabled by `torus-consensus`'s dev-dependency, like
  `torus-bridge`'s `test-reference-paths`, `torus-consensus/Cargo.toml:39`) with thread-local counters:
  - overlay reads by layer that answered (own pending, R, parent, L main, L deferred, C, DB) x CF, in `get_cf_raw` /
    `lookup_lower` / the iterators / `get_cf_len` / `prefix_exists` (`backend.rs:1488`, `2476-2730`);
  - a scope label set by the caller (revm adapter, journal, lane read, due-check, native phase), so a hit is counted
    per reader class;
  - **in-flight hits (R2)**, a second counter beside each layer hit. A hit is in flight when, at the moment of the
    read, the DB does not yet give the layer's answer: the probe re-reads the same key from the DB (through the
    tripwire exemption token, task 1.6) and counts `inflight` iff the DB value differs (absent, other bytes, or a
    row the layer tombstoned). Only these hits prove a raw-DB reader would have diverged at that read. A parent hit
    after a drain, or a C hit whose checkpoint already landed, counts as a hit and not as in flight. The probe costs
    a DB read per layer hit, test builds only.
  The `ExecutionContext` gets `#[cfg(test)]` path flags in the style of `test_nonce_prune` (pipelined EVM height,
  serial EVM height by reason, absorb rows by kind, Marker with status, Marker with side rows, W EVM step per job
  kind, ...). Section 5.2 lists them.
- Test first: `o0_coverage_report_counts_a_known_stream`: a 12-block hand stream gives exact counts.
  `o0_inflight_counter_ignores_durable_layer_hits`: a parent-layer hit at interval 0 counts in flight with W held
  and not after `wait_idle`.
- Verify: `cargo nextest run -p torus-consensus -p torus-state $F -E 'test(o0_coverage) | test(o0_inflight)'`.

**Task 0.4: the ledger comparator and the W-gated asynchronous runner (R2; replaces `p5_compare` for option 0)**
- Code (`app.rs` test module, next to `p5_compare` `28820`, which stays for Phase 5's own tests):
  - `O0Ledger` per node: `hn: BTreeMap<height, hash>`; `status: BTreeMap<height, Option<hash>>` and the per-height
    side rows of task 0.2, filled when that height completes; `dumps: BTreeMap<completed height, Vec<(cf, hash)>>`.
    E appends to `hn` after each of its blocks without waiting for W (it drains `capture_take` by height). Status,
    side rows and dumps come only from the completion hook below, never from a read of the marker.
  - **Completion hook (R5, F11).** A marker-keyed snapshot can sit between a batch and its EVM mirror resync, and a
    RocksDB snapshot cannot close that gap (two separate writes). The ledger therefore snapshots where a height is
    known complete: a `#[cfg(test)] test_on_complete: Option<Arc<dyn Fn(u64, &StateDb) + Send + Sync>>` on the
    worker env and on `ExecutionContext`, called with the completed height:
    - on W in `worker_loop` (`exec_pipeline.rs:766-786`), for a Flush, Marker or Checkpoint whose `run_job` returned
      `Ok`, or `TrieStale` for a Checkpoint (a TrieStale Flush returns before its resync and is not complete, F4b;
      the next completed height covers it). That is after `resync_evm` (`957`, `1054`) and before `durable_height.store` /
      `finish_job`, and so before W can `recv` the next job. Never for a Digest (nothing durable) or a failed job;
    - on E after the serial flush's resync (`app.rs:4743-4753`, at the end of the serial-flush block `4759`) and after
      a successful `flush_marker_only` (`4970`). S takes all its heights here; a pipelined node takes its barrier
      heights here, written by E after `wait_idle`, so W is idle.
    The hook takes a `rocksdb::Snapshot` (a test helper `dump_all_cfs_at`; `StateDb` has no snapshot API on
    `47c5f6fb`, `dump_all_cfs` is `app.rs:18156`). It records, for every height in (previous completed, h], the
    status and side rows, and at a dump point the per-CF dump hashes. It asserts the marker in the snapshot equals h
    (a check on the hook site). Hashing may run on a ledger thread after the snapshot is handed over (the snapshot
    pins the bytes). Neither E nor the gate waits on the hook: W pauses only for the snapshot call, and E is never
    drained between blocks. Two writers besides W can run while W is in the hook, and their rows are filtered out: the
    dispatcher's header and body rows above h (it writes them ahead, `persist_committed_block_durably`), and E's
    trade-history rows (`CF_NATIVE_TRADES`, `CF_NATIVE_USER_TRADES`) of blocks at or above h, written outside the
    block batch (found in step 0, s131; the plan first named the dispatcher only). The filter covers those rows and
    nothing else (`o0_snapshot_filter_is_dispatch_rows_only`).
  - `o0_compare(a: &O0Ledger, b: &O0Ledger)`: compares each key present in both (height for `hn`, status and side
    rows; completed height for dumps) as soon as both have it, and never compares markers. A divergence names the
    height and the CF. At the end both nodes drain once, and completeness is asserted: every height in `hn` and
    `status` on both, and every dump key of the coarser node present in the finer one (S dumps at every height, so S's
    ledger holds the reference snapshot for any completed height a pipelined node reports). On a dump mismatch the
    harness re-runs S to that height and prints the first differing row (S is deterministic), so S keeps hashes only.
  - Two run modes per pipelined node:
    - **D (drained lockstep):** today's `P5Node::execute` (W drained after each block). Deterministic markers; it is
      the mode the golden is recorded in.
    - **G (W-gated asynchronous):** no `wait_idle`, no forced checkpoint, barrier or drain between blocks. E runs in
      a scoped thread; a controller thread holds and releases W from a seeded schedule so that reads happen with
      writes in flight: at interval 0 it holds W (`WorkerGate::hold`, `exec_pipeline.rs:387`) before block h-1's
      Flush and releases it only after block h's pre-hand-off hook fired, so h's whole EVM section and native phase
      read with h-1 in flight (the hand-off is a rendezvous, `exec_pipeline.rs:19`, so E would block there and the
      controller must release from its own thread). The hook is new: a `#[cfg(test)] test_pre_handoff: Option<Arc<dyn
      Fn(u64) + Send + Sync>>` on `ExecutionContext`, called just before every `pipeline_handoff` (Flush and Marker).
      `test_block_probe` does not fit: it runs mid native phase, native blocks only (`app.rs:1315-1319`, `4306`).
      With an interval it parks checkpoint jobs (`hold_checkpoints`, `403`) across 1-4 blocks, EVM heights included, so reads are served by C; and
      `CkptState.test_gate` (`DeferredGate`, `app.rs:126`, `145-150`) holds a merge's deferred part across an EVM
      height. Every hold has a release bound (k <= 4 blocks at interval 0 by construction; well under
      `DIGEST_QUEUE` 128 with an interval) and a 30 s watchdog that fails the test, not a hang.
- Test first (on today's code, so the harness is proven before anything changes):
  - `o0_ledger_accepts_checkpoint_lag` (guard): S against today's pipelined Every5 and Never in G mode compares
    clean, and `o0_p5_compare_rejects_checkpoint_lag` pins why: `p5_compare` on the same pair reports a divergence
    after the first block that is not a checkpoint (hashed heights `28837-28846`, else the marker `28873-28874`),
    with no bug.
  - `o0_gated_run_reaches_inflight_layers`: today's native-only stream in G mode at interval 0 and Every5 counts
    in-flight parent, L main, L deferred and C hits > 0 for the native phase; the same stream in D mode counts 0
    in-flight parent and C hits at interval 0 (the masking, pinned).
  - `o0_planted_skip_parent_caught_only_in_g`: a `test-probes` switch makes the overlay skip the parent layer
    (`with_split_layers`). D mode at interval 0 does not see it (asserted, the reason G exists); G mode reports a
    divergence. Same for `o0_planted_skip_inflight_checkpoint` (skip C) with an interval; on today's native-only
    stream it is caught only with the resident rows R off: with R the native phase reads too little through C (s131).
  - `o0_golden_today_g`: today's code in G mode reproduces the golden (determinism under timing).
  - `o0_marker_keyed_snapshot_sees_pre_resync_mirror` (guard, pins F11): a pipelined interval-0 block that credits an
    EVM account (a fee to an EVM address, so `evm_addrs` is non-empty), with W parked between its batch and
    `resync_evm` by a new `#[cfg(test)]` `WorkerGate::hold_before_resync` seam (as `hold_latch`, `exec_pipeline.rs:
    374-379`). A marker-keyed snapshot taken there has S's marker and a different CF_HASHED_ACCOUNTS row (the Codex
    probe). The hook snapshot of the same height equals S's (`o0_completion_snapshot_equals_serial`).
  - `o0_completion_hook_fires_after_resync_only`: per job kind (Flush, Marker, Checkpoint, a barrier's E flush), one
    hook call per completed height range. None for a Digest or a failed job. The marker in each snapshot equals its
    tag.
- Verify: `cargo nextest run -p torus-consensus $F -E 'test(o0_ledger) | test(o0_gated) | test(o0_planted_skip) |
  test(o0_golden_today) | test(o0_marker_keyed) | test(o0_completion)'`, twice (G mode is timing-driven; the gate
  makes its coverage deterministic).

Size: ~1,120-1,450 lines, almost all tests (estimate; task 0.4 adds ~420-550, of which ~70-100 for the completion
hook, the resync seam and their guards, R5).

### Step 1 (design P1): behaviour-neutral preparation, no flag

Everything in step 1 runs on today's paths. The golden must stay byte-equal after every task.

**Task 1.1: precompile local faults are never reverts (F1): dependency check and classification audit**
- Scope (R3): the fix itself (the provider change and its tests below) is built on `fix/precompile-read-error-fail-stop`
  and merged to main on its own (decision 3, settled); its lines are not in this plan's size. Here, once that fix is
  in the base (gate D1): (1) check that the tests below exist and are green, and add any that the fix branch did not
  ship; (2) the **classification audit**, kept whether or not the fix branch did one: the list of every
  `State` / `Borsh` / `MissingCf` / `BookCorrupt` source reachable from `execute_precompile_metered`
  (`torus-core/src/precompiles.rs:528-607`), each marked "local storage / row fault" or "input-dependent", written
  as a table in the commit message and pinned by a test per input-dependent source; (3) the option 0 extension: the
  same classification for the new error source of the fast path, a journal read that hits L's deferred part after a
  failed merge (`StateError::Io`, `backend.rs:1470-1478`), tested in task 3.4.
- Tests (from the fix branch, or added here) (`torus-evm/tests/r02_precompile_fault_tests.rs`, whose `faulty_db`
  already opens a DB without one CF; the `27f49ad3` seam in `evm_db_fault_tests.rs` is the same idea):
  - `balance_reader_read_fault_is_a_local_fault`: a DB without CF_NATIVE_BALANCES; a contract calls the balance
    reader. Today the receipt is a revert (status false). Expected: `EvmError::LocalFault`.
  - `enqueue_read_fault_is_a_local_fault`: a DB without CF_FEE_CONFIG; `placeOrder`. Expected: `LocalFault`.
  - `deterministic_precompile_errors_stay_reverts` (guard): bad calldata, non-payable value, OOG, a writer in
    `eth_call` keep their revert.
  - `torus-consensus`: `precompile_read_fault_fail_stops_block` (as `evm_db_read_fault_fail_stops_block`,
    `app.rs:27485`).
- Code (on the fix branch, as planned in revision 0): in `TorusPrecompiles::run` (`precompile_provider.rs:208-216`,
  which records only `PrecompileLocalFault`), record any `Err(e)` with `e.is_local_fault()` in the cell, then abort
  as for `PrecompileLocalFault`, so no local fault reaches the `Revert` arm (`244-248`). Each audited source must be a
  local storage or row fault, never a function of the calldata. Where one is input-dependent, map it to
  `InvalidPrecompileInput` first, with a test.
- Verify: `cargo nextest run -E 'rdeps(torus-evm)' $F`; golden unchanged (no fault in the stream).
- Decision 3 (settled, R3): the fix ships on its own branch ahead of option 0; gate D1 enforces the order.
- **Done (s131, on base `fd94818d`, F1 fix `86cd520f` + scan fix `a361e764`).** (1) The tests exist under other
  names, all green: `balance_reader_read_fault_is_a_local_fault` and `enqueue_read_fault_is_a_local_fault` are
  cases of `f1_precompile_read_fault_is_a_local_fault` (`0x0801 getBalances balance read`, CF_NATIVE_BALANCES;
  `0x0810 placeOrder delay read`, CF_FEE_CONFIG; 8 cases, block / overlay / tx paths) plus
  `f1_reader_read_fault_in_eth_call_is_an_error`; the guard is `f1_deterministic_precompile_errors_stay_reverts`;
  `precompile_read_fault_fail_stops_block` (+ `precompile_scan_fault_fail_stops_block`) in torus-consensus. Added
  here: `f1_undecodable_stored_row_is_a_local_fault` (guard, the row-decoding sources). (2) The audit (table in the
  commit message): every `State` / `Borsh` source reachable from `execute_precompile_metered` is a local storage
  fault (19: 18 reads, every CF a constant, and the journal's puts) or a local row fault (3: `Position` and `NativeBalance` decodes, the bbo
  row length; the calldata picks the key, but every key it can address is written only by that row's codec), plus
  one unreachable `Borsh` (encoding a `QueuedAction` into a `Vec`). No `MissingCf` / `BookCorrupt` source is
  reachable. **No input-dependent source**: nothing maps to `InvalidPrecompileInput`, no behaviour change, golden
  byte-equal. (3) **Option 0 extension, no code now:** on the fast path the journal (and the revm adapter) reads
  through the block overlay, so a read that reaches L's deferred part after its merge failed returns
  `StateError::Io` (`backend.rs` `layer_for`, "since-checkpoint layer merge failed"). From a precompile it is
  `CoreError::State`, a local fault: the provider records it, the block aborts with `EvmError::LocalFault` and E
  fail-stops, like a missing CF today. Tested in task 3.4 (`fast_path_deferred_merge_failure_under_journal_fail_stops`).

**Task 1.2: absorb the EVM rows into the block overlay on the serial path**
- Test first:
  - `absorb_evm_writes_exactly_the_plain_batch_rows` (`torus-state`): for random bundles (puts, deletes, zeroed
    slots, destroyed accounts, new code) and journal pending sets, the overlay rows after `absorb_evm` equal, key and
    value, what `apply_bundle_plain` + `append_pending_to_batch` put in a `WriteBatch` (decoded back).
  - `absorb_keeps_h_n_and_flush_bytes` (`torus-consensus`): an EVM block through today's code and through the absorb
    gives the same `h_n` and the same CF dump.
  - `native_phase_never_reads_this_blocks_journal_rows` (F8): a counting backend over the native phase of an EVM
    block flags any read of a queue key whose target is not h, or of the three cursor keys
    (`precompiles.rs:1350-1365`).
  - golden (`o0_golden_today`) stays green.
- Code:
  - `NativeStateOverlay::absorb_evm(&self, bundle, journal: NativeStateOverlay) -> Result<(), StateError>`
    (`backend.rs`, next to `seed_from_bundle` `1741`): the plain change set exactly as `apply_bundle_plain`
    (`incremental.rs:270-310`: CF_ACCOUNTS put / delete, CF_STORAGE put / delete per slot, CF_CODE put), then the
    journal's own pending puts and deletes in its order. It takes the journal by value and drops it before returning.
  - `seed_from_bundle` is removed: the absorb covers its CF_ACCOUNTS rows, and `dirty_evm_accounts` still sees them.
  - `app.rs` EVM section (`3567-3614`): `evm_batch` keeps only `apply_bundle_hashed` + `write_trie_updates` +
    `append_links` (a new `evm_block_trie_batch`, split out of `evm_block_batch_incremental`, `incremental.rs:396`).
    `bundle_extras` gets nothing from the bundle or the journal (the rows are in pending; `HashExtras` rank below
    pending, `backend.rs:881-923`). Slash extras are untouched.
  - The absorb runs right after the EVM part, so the overlay must exist then: task 1.3 moves its construction.
  - The plain fallback on an incremental error (`3580-3587`) becomes "trie ops dropped + stale marker" (task 2.3);
    until step 2 it keeps the plain rows in the overlay and logs as today.
- Verify: `cargo nextest run -E 'rdeps(torus-state)' $F` (big core change); golden green.
- **Done (s131, G1b, `619d1f83`, after 1.3).** As planned; `absorb_evm` moves a journal CF the overlay has not
  touched without a copy. Deviations: `absorb_keeps_h_n_and_flush_bytes` is a torus-state test (today's path rebuilt
  in-test from its parts: prefix batch, `HashExtras`, the seeded CF_ACCOUNTS rows; 40 random blocks with native
  writes overlapping the EVM rows), not torus-consensus; the golden is the end-to-end check. F8's test uses the
  probe's key watch, extended to scans (`Probe::watch_scans`), instead of a counting backend (the native phase
  runs on the concrete overlay). New path flag `evm.absorb` (the coverage test's exact map gained it).

**Task 1.3: the overlay before the EVM part; due-check through it after the absorb (R5)**
- Test first:
  - `core_writer_due_reads_layers` (`torus-bridge`): rows only in a parent layer -> due; a row drained (tombstoned)
    in L but still in the DB -> not due (design test 3c).
  - `pending_count_matches_prefix_exists` (`torus-core`, guard on the new body).
  - golden green.
- Code:
  - `NativeExecutor::core_writer_due<T: StateBackend>(state: &T, h)` = `state.prefix_exists(CF_CORE_WRITER_QUEUE,
    &h.to_be_bytes())` (same answer as `pending_count > 0`). `CoreWriterQueue::pending_count` moves to `&impl
    StateBackend` (iterate prefix), or goes if it has no other caller. Fix the doc at `13619-13628`.
  - `app.rs`: move the block of `3713-3755` (parent, `checkpoint_read_layers`, `with_split_layers`) above the EVM
    section (`3513`). The due-check (`3702`) moves after the absorb and reads `&overlay`. On the serial path the overlay
    has no layers, so every read is the DB read it was. On the pipelined path `checkpoint_read_layers` now runs before
    the EVM part; for native blocks that only moves time.
  - The `ExecutionContext::core_writer_due` test seam (`2623`) keeps its `ReplayFault::CoreWriterDueRead` switch.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`; golden green.
- **Done (s131, G1b, `cf6b9b2b`, first G1b commit).** As planned; the due-check runs under the `DueCheck` probe
  scope. `pending_count` stays (test callers), over any backend.

**Task 1.4: the lane read and the enqueue reads go through a backend (R7, R8)**
- Test first: `lane_row_in_parent_layer_sets_big_fee` (`torus-bridge`): `validate_block_inner` over a layered backend
  sees an `EvmUserModify` row that is only in the parent layer. Enqueue: `enqueue_reads_cursor_rows_from_layers`
  (`torus-core`): cursor rows and full-target probes held only in L give the same target and seq as with them in the
  DB.
- Code: `validate_block_inner` (`validator.rs:118`) reads the lane row through the `EvmReads` argument of task 1.5
  (`154-178`; the comment "runs after a barrier" goes). Enqueue already takes `&impl StateBackend`: fix the doc
  (`precompiles.rs:1407`).
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`; golden green.
- **Done (s131, G1b, `10eabfad`, after 1.5: the lane read uses 1.5's `EvmReads`).** `EvmReads::get_cf_raw` (Db:
  the DB; Layered: `EvmBlockState::lane_read`, probe scope `Lane`). `enqueue_reads_cursor_rows_from_layers` is a
  guard (enqueue already read through its backend): rows only in the parent, L or C give the same seqs and rows.

**Task 1.5: EVM entry points take `EvmReads`, not `&StateDb`**
- Test first: `execute_block_lanes_over_layers_equals_db` (`torus-evm`): the same block over the DB and over an
  `EvmBlockState` whose layers hold the same rows gives the same receipts, bundle and journal rows.
- Code:
  - `torus-state`: `LayeredEvmDb<'a>` implements `revm::DatabaseRef<Error = StateError>` over `&NativeStateOverlay`.
    Account (CF_ACCOUNTS), storage (`storage_key`, CF_STORAGE), code (CF_CODE) and block hash (CF_BLOCK_HEADERS, first
    32 bytes) are decoded exactly as `DatabaseRef for StateDb` (`db.rs:1474-1503`; share the decoders).
  - `EvmBlockState<'a> { db: LayeredEvmDb<'a>, journal: NativeStateOverlay }`. The journal is `with_split_layers(db,
    parent, L, deferred, C)` cloned from the block overlay's layers, without R (design point 2, H3).
  - `torus-evm`: `execute_block_lanes` becomes generic over `D: DatabaseRef<Error = StateError>` and takes the
    journal (today it builds `NativeStateOverlay::new(state_db)`, `executor.rs:355`). `execute_block` and the RPC
    paths keep a `&StateDb` wrapper (journal over the DB, as today).
  - `torus-bridge`: `validate_block_for_catchup(block, reads: EvmReads<'_>, executor, root: RootMode)`, where
    `EvmReads` is `Db(&StateDb)` or `Layered(&EvmBlockState)`. Serial: `Layered` over the layer-less overlay (the same
    reads) or `Db`. Both are covered by the golden.
  - `append_links(&StateDb)` stays on the serial path; the fast path drops it (task 2.2).
- Verify: `cargo nextest run -E 'rdeps(torus-evm)' $F`; golden green.
- **Done (s131, G1b, `1ee11778`).** Deviations: `EvmReads::Layered` holds the `EvmBlockState` by value (not
  `&EvmBlockState`): the journal moves into the executor and comes back as `native_writes`, which the absorb consumes,
  so no layer handle outlives the EVM part (3.2's "journal dropped before native"). `RootMode { OnE, Skip }` is in
  (Skip: zero root, no `TrieUpdates`, no oracle; unused until 3.2). The serial path passes `EvmReads::Db`: the same
  reads and journal as before, without the overlay's per-read lookups (perf: the flag-off path is unchanged).
  The shared decoders live in `db.rs` (`decode_code`, `decode_storage_value`, `decode_block_hash`).
  Open for step 3: the journal's precompile reads carry no `Journal` probe scope yet (torus-evm has no
  `test-probes` feature); the revm adapter's reads carry `Revm`.

**Task 1.6: the raw-DB tripwire**
- Test first: `tripwire_fires_on_raw_read_in_armed_scope` and `tripwire_allows_overlay_bottom_reads` (`torus-state`).
- Code: under `cfg(any(debug_assertions, feature = "test-probes"))`, a thread-local guard. While armed, every `StateDb`
  read entry point panics: `get_cf_raw`, `get_cf_len`, the iterators, `prefix_exists`, `DatabaseRef`, `get_account`,
  `get_block_hash`, `inner()`, `cf_handle`. The overlay's DB fall-through reads (`backend.rs:2499`, `2615`, `2629`,
  `2671`, `2690`, `2723`) hold an exemption token around the call. Release builds compile it out. Step 3 arms it for
  the fast-path EVM section.
- Verify: `cargo nextest run -p torus-state $F` + `cargo check --workspace --tests -q`.
- **Done (s131, G1b, `4a7665a7`).** Also trips `multi_get_cf_raw`, `exists_cf_raw`, `get_storage`, `get_code`,
  `all_accounts`, `account_storage`, `get_session`, `count_sessions_for_owner`. The overlay exempts each read method
  for its whole call (its DB fall-through and the probes' re-reads run inside). `cargo check` ran on the plan's -p
  set, not `--workspace` (feature re-unification); the rest of the workspace ran its nextest after 1.7.

**Task 1.7: docs of the barrier assumptions** (`native_executor.rs:13619-13628`, `precompiles.rs:1407`,
`validator.rs:154-157`, `app.rs:3418-3426`): they now say "read through the block overlay".
- **Done (s131, G1b).** `native_executor.rs` (`core_writer_due`) in 1.3, `precompiles.rs` (`enqueue`) and
  `validator.rs` (lane read) in 1.4; this task: the pipelined-predicate comment in `app.rs` (the EVM part's reads go
  through the block overlay; an EVM block still barriers for its writes until step 3). "EVM blocks are never
  pipelined" (the hand-off assert, the no-native comment) stays true until 3.2.

Step 1 size: ~500-800 lines, about half tests (estimate; task 1.1 is now ~50-150, the fix's own lines are on its
branch). Golden green after every task.

### Step 2 (design P2, W side): the EVM trie and the side rows on W (dormant until step 3)

**Task 2.1: EVM side rows on the job**
- Test first: `evm_side_rows_equal_commit_block_metadata` (`torus-consensus`, `exec_pipeline` tests): W's encoding of
  `EvmSideRows` gives byte-equal CF_RECEIPTS, CF_BLOCK_HASH_TO_NUMBER, CF_TX_HASH_TO_LOCATION and CoreWriter-link rows
  to `commit_block_metadata` (`committer.rs:145-197`) + `append_links` (`corewriter_tx.rs:68`), minus header and body.
  Cases include zero receipts (all skipped, F2): exactly one row, CF_BLOCK_HASH_TO_NUMBER (`committer.rs:184`), and
  no CF_RECEIPTS / CF_TX_HASH_TO_LOCATION row.
- Code: `EvmSideRows { height, block_hash, receipts, links }` built on E from the validated block. W encodes it into
  the job's sidecar (Flush: `exec_pipeline.rs:980-1009`) or the window's side layer (Digest: `877-901`), like the
  action status. Header and body are not side rows: the dispatcher wrote them (`durable.header && durable.body`), and
  a second writer would break Q6 A1's key disjointness (design point 7). A shared encoder in `torus-bridge` serves
  both `commit_block_metadata` and W.
- Verify: `cargo nextest run -E 'rdeps(torus-consensus)' $F`.

**Task 2.2: the EVM trie step on W**
- Test first (`torus-state`, `incremental.rs` tests):
  - `window_trie_step_equals_per_block_apply`: random windows of 1-8 EVM bundles (wipes, EIP-161 empties, re-creates,
    storage churn) plus native-credit accounts. From the DB at S, W's step (mirror ops + `TrieUpdates` over DB
    cursors + the folded delta) gives CF_HASHED_* and CF_TRIE_* byte-equal to applying each block in order with
    `evm_block_batch_incremental` then `resync_evm_accounts`. The root equals the full scan.
  - `per_block_flush_trie_step_equals_serial` (interval 0: a window of one).
- Code:
  - `EvmWindowDelta` (`torus-state/src/incremental.rs`): fold(bundle) in height order. Per touched account: its
    final info and `apply_bundle_hashed`'s keep flag (`incremental.rs:327-339`: kept iff `Some(info)` and not
    EIP-161 empty). A not-kept account marks a wipe and clears its earlier window slots. Changed slots of kept
    accounts are kept as last-value-wins.
  - At the step: the native-credit accounts from the window's final CF_ACCOUNTS rows (account-only, as
    `resync_evm_accounts`). Then the mirror ops: the wipe scans against the DB at S first, then the puts and deletes.
    The root: `StateRoot` over the DB cursors plus the sorted delta, with the same EIP-161 override as
    `incremental_evm_root` (`236-264`).
  - W hashes keys (keccak) on W, not E. The job carries the `BundleState` moved out of the validated block.
  - `flush_on_worker` and `CheckpointLayer::flush_checkpoint` take an optional prefix `WriteBatch`, which W fills
    with the step's ops. CF_HASHED_* / CF_TRIE_* are never in a pending set, so the order is free. Q6 A1's tail
    routing (design 4.4 item 3) treats these as trie-CF rows.
- Verify: `cargo nextest run -E 'rdeps(torus-state)' $F`.
- Decision 4: if the window step cannot match per-block CF_TRIE_* bytes (reth's update sets differ by batching) while
  the roots and the mirror agree, fall back to per-block steps on W in height order over reth's in-memory
  trie / hashed cursor overlays.

**Task 2.3: stale trie: marker, full-scan fallback, boot rebuild (F4)**
- Test first: `evm_trie_step_failure_marks_stale_and_boot_rebuilds` and `stale_trie_routes_root_to_full_scan`.
- Code: a node-local key outside `META_CONSENSUS_PREFIXES` (not hashed) set in the same batch when the step fails (the
  checkpoint lands without the EVM ops). `evm_root_routed` (`state_root.rs:37`) uses the full scan while it is set.
  `ensure_trie_built` (`incremental.rs:131`) wipes and rebuilds when it is set, then clears it.
- F4b: a TrieStale Flush returns before `resync_evm`. Make it resync (as the Checkpoint) or set the stale marker;
  then flip `o0_trie_stale_flush_is_not_a_completion`'s pinned pre-resync row and let the hook fire for it.
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`.

**Task 2.4: job plumbing**
- Code: `Job::Flush`, `Job::Digest` and `Job::Marker` gain `evm: Option<EvmJobRows { bundle, side }>`; `Job::Marker`
  also gains `action_status` (F2). `pipeline_handoff`'s Marker -> Digest arm (`app.rs:2863-2870`) today builds the
  Digest with `action_status: None` and no EVM field; it carries both. W writes a Marker's side rows in the marker
  batch (interval 0) or folds them into the window (via the Digest). The `Window` gains `evm: EvmWindowDelta`. A
  barrier's `checkpoint_drain` closes it like any window. The serial path never sets `evm`.
- Test first: `marker_job_carries_action_status_to_checkpoint` and
  `marker_job_side_rows_land_at_interval_0_and_at_checkpoint` (an all-skipped block's hash -> number row is in the DB after its Marker at interval 0, and after the checkpoint
  with Every5, byte-equal to `commit_block_metadata`'s).

Step 2 size: ~580-800 lines (estimate; +30-50 for the Marker's `evm` field and its tests).

### Step 3 (design P2, E side): the flag and the fast-path EVM section

**Task 3.1: the flag**
- Test first: `parse_evm_fast_path` (unset / `0` / `false` = off; `1` / `true` = on; else off + warning, as
  `parse_epoch_fast_path`, `exec_pipeline.rs:68-87`) and `evm_fast_path_flag_absent_evm_heights_stay_serial`
  (the `epoch_fast_path_evm_or_slash_boundary_stays_serial` pattern, `app.rs:30685`).
- Code: `exec_pipeline::evm_fast_path_enabled()`, an `ExecutionContext.evm_fast_path` field read once in `try_new`
  (next to `epoch_fast_path`, `app.rs:6172`), a startup log line as at `6266-6285` (no effect without W). The
  predicate term `(!has_evm || self.evm_fast_path)` (`3436`).
- Gate D1: this task starts only on a base that holds the F1 fix (section 4); its commit message names the fix commit.

**Task 3.2: the fast-path EVM section**
- Code:
  - With `pipelined && has_evm`: `EvmBlockState::new(&overlay)`, arm the tripwire, and `validate_block_for_catchup(..,
    RootMode::Skip)`. Skip computes no root and no full-scan oracle (F6), and gives `evm_root_updates: None`.
  - `overlay.absorb_evm(..)` (task 1.2). The journal is consumed, and `ValidatedBlock.native_writes` drops with it
    before the native phase. Otherwise L, its deferred part or C stay shared at `take_layer` (`app.rs:220-254`).
  - `evm_job = Some(EvmJobRows { bundle, side })` on EVERY `Ok`, including zero receipts (all skipped, F2): the
    side rows then hold just the hash -> number row that `commit_block_metadata` writes today (`committer.rs:184`).
    `evm_job` is `None` only when the section returned `Err`. No `evm_batch`, no `commit_block_metadata`, no
    `append_links`.
  - The assert at `4573` becomes "a pipelined block never built an `evm_batch`".
  - Errors keep today's arms: `LocalFault` and `BridgeError::State` fail-stop, the generic arm logs. An absorb put
    error fail-stops with `fail_stop_on_fault`.
- Test first: tests 3.6-3.8 below; `fast_path_evm_block_hands_off_evm_rows` (unit); `journal_dropped_before_native`
  (the layer-shared counter is 0 after a reader-precompile block).

**Task 3.3: hand-off and the no-native EVM block**
- Code: the Flush / Digest jobs get `evm: evm_job`. A no-native EVM block on the fast path (F2) hands off
  `Marker { action_status: evm_skipped, evm: evm_job }`: `evm_job` is `None` for a failed section and `Some` (empty
  bundle, hash -> number side row) for an all-skipped `Ok`. The debug assert is
  `evm_job.as_ref().is_none_or(|j| j.bundle.is_empty() && j.side.receipts.is_empty() && j.side.links.is_empty())`
  ("a Marker never carries absorbed rows"); revision 0's `evm_job.is_none()` was wrong for the all-skipped case.
  An all-skipped block that runs the native phase (`app.rs:3784-3793`) hands its `evm_job` to the Flush / Digest.
  `advance_untouched` runs as today.
- Test first (each against the same block on today's serial path: status row, hash -> number row, no receipt,
  same `h_n`; at interval 0 and Every5):
  - `fast_path_failed_evm_section_writes_status_with_marker` (no hash -> number row, as today);
  - `fast_path_wrong_nonce_only_block_writes_hash_index_with_marker`;
  - `fast_path_undecodable_only_block_writes_hash_index_with_marker`;
  - `fast_path_all_skipped_block_with_native_phase_writes_hash_index` (a native action in the same block: Flush /
    Digest path).

**Task 3.4: local faults on the fast path**
- Test first: `fast_path_adapter_read_fault_fail_stops`, `fast_path_deferred_merge_failure_under_journal_fail_stops`
  (the merge thread's failure seam; task 1.1's option 0 extension: a precompile journal read that reaches L's
  deferred part after a failed merge gets `StateError::Io`, so `CoreError::State`, a local fault: never a revert),
  `fast_path_absorb_put_fault_fail_stops`. The faults are injected in G mode with
  the parent's job (interval 0) or the checkpoint (interval) still held (R2). Each is a fail-stop on E, which latches
  `exec_failed`, so W must refuse the held jobs (`exec_pipeline.rs:742-744`) and not land them. The checks follow
  task 3.5's crash protocol (R4): d = the marker before the fault; before the drop, `exec_failed` is set and
  `wait_idle()` returns false; after it, the marker is still d (not just below h); no status or side row exists for
  any height in (d, h]; the restart replays exactly d+1..=h. The result equals S's hook snapshot at h, by completed
  height (R5).

**Task 3.5: W crash and replay**
- Test first: crashes at these points: after the EVM part before the absorb (`test_crash_after_evm_section`,
  `app.rs:3688`); after the absorb before the hand-off; after the hand-off before the checkpoint; during W's EVM trie
  step (inject); during the replay of an EVM block. The restart replays serially, and the dump (EVM trie included)
  and `h_n` equal the uncrashed serial run. Single and repeated crashes. Template:
  `epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` (`app.rs:30727`) through `ckpt_crash_replay_with`
  (`31191`) and its `CkptCrash` arm (`29843-29922`).
- R2: every crash point is taken in task 0.4's G mode, with the jobs before it still held at the gate (never after a
  `wait_idle`); the comparison is the ledger one (crashed node vs uncrashed S, by height and completed height), not
  `p5_compare`. One case crashes with an all-skipped block's Marker held (its hash -> number row must come back from
  the replay).
- **R4, the crash protocol (`o0_crash`, one helper for every point; never a bare drop).** A drop releases the gate,
  W writes every held job, and a healthy context also writes a final checkpoint (F10). A crash by drop alone would
  land the "lost" jobs and the comparison would pass without any replay. So, as the template does:
  1. Record d = the durable marker and the last handed-off height `last` while W is held.
  2. Latch before releasing. E-side points (after the EVM part, after the absorb): the seam latches `exec_failed`,
     then returns. `test_crash_after_evm_section` gains the latch, since today it only returns (`3688-3690`); the
     new after-absorb seam is built the same way. W-side points (after the hand-off, the EVM trie step):
     `gate.fail_next()` before `gate.release()` fails the oldest held job, and W latches (`exec_pipeline.rs:787-798`)
     and refuses the rest (`742-744`); the trie step's inject is a panic seam inside the step, before its batch
     (caught at `800-806`). Replay-time crash: the same latch on the replay context.
  3. Before the drop: `!wait_idle()` (or `exec_failed` for the E-side points with W idle) and `exec_failed` set.
  4. After the drop: the marker is still d, below every handed-off height. For every height in (d, last], no
     CF_BLOCK_ACTION_STATUS, CF_RECEIPTS, CF_BLOCK_HASH_TO_NUMBER, CF_TX_HASH_TO_LOCATION or CoreWriter-link row exists.
     CF_HASHED_* / CF_TRIE_* equal the hook snapshot at d.
  5. The restart: `replay_committed` reaches `last`; the replay's `capture_take` lists exactly d+1..=last (each
     re-hashed, `dedup_captured` as in `ckpt_crash_replay_with`, `app.rs:31209-31214`), with >= 1 EVM height and,
     where the point involves one, the all-skipped Marker height among them. Only then the ledger comparison with S.
  - Guard `o0_bare_drop_lands_held_jobs` (pins F10, the Codex probe): interval 0, W held after a native block and
    after a flag-on EVM block, a drop without the latch. The marker goes from d to `last` and the side rows of
    (d, last] are present. This is why every crash uses `o0_crash`.
  - Guard `o0_crash_protocol_rejects_unlatched_crash`: `o0_crash` with step 2 switched off fails at step 4 (the
    harness catches a crash that is not one).
  - A real process kill (the node as a child process, SIGKILL) is not used here. It needs a node binary harness and
    adds no path the latch does not reach: W writes nothing after the latch, and the atomic batch makes a kill land
    all of a job or none of it. Q11's SIGKILL cell (section 6) covers the process kill (decision 13).

**Task 3.6: the equivalence harness with the flag** (section 5)
- Test first: `o0_equivalence_*` (section 5.3's matrix, D and G modes) against the golden, plus
  `o0_coverage_floor_per_config` (in-flight hits, 5.2) and the planted bypasses of 5.4.
- Also update `epoch_fast_path_evm_or_slash_boundary_stays_serial`: with the EVM flag on, an EVM boundary block is
  pipelined; a slash or a non-durable header still barriers.
- And flip the next-block half of `o0_eip161_recreation_reads_by_layer` (task 0.1): with the flag on, the re-fund at
  e+1 reads Z's EIP-161 deletion from an in-flight layer (parent at interval 0, C or L with Every5 in G mode) at
  least once per configuration; the same-block half (the block's own pending layer) stays.

**Task 3.7: existing EVM tests on the fast path**
- `corewriter_evm_row_drains_in_empty_next_block` (+ `_with_interval`, `app.rs:25834`, `25923`),
  `core_writer_tx_links_each_queued_row_to_its_evm_tx` (`21585`) and
  `f3_epoch_gas_sets_plan_fees_serial_and_pipelined` (`21496`) gain a flag-on run. So does the R02 branch 5 fault
  family (`evm_db_read_fault_fail_stops_block` `27485`, `ReplayFault::EvmLocalFault`). That family already drains
  before the faulty block and asserts the latch before its drop (`assert_storage_cf_fault_fail_stops_then_replays`,
  `27515`). Its flag-on run keeps that and adds task 3.5's step 4: no side row of the faulted height, and the replay
  re-hashes it. It must not be rewritten to drop with W held without the latch (F10).

**Task 3.8: mixed replicas**
- Test first: `o0_mixed_replicas_agree`: three nodes on one stream (flag off; flag on with Every5; flag on with
  interval 0) agree on every `h_n` and every state-hash attestation row (`h % 100`). Their checkpoint policies differ,
  so the comparison is task 0.4's ledger (by height), never `p5_compare` (it would fail on the marker lag). Run in D
  and in G mode (the two flag-on nodes gated on independent schedules).

Step 3 size: ~750-1,070 lines, mostly tests (estimate; +100-150 for the all-skipped cases, the G-mode wiring of the
matrix, the in-flight planted faults; +50-70 in revision 2 for the `o0_crash` protocol, the seam latches and their
two guards, R4).

### Step 4: node-local consumers, docs, review

**Task 4.1: mempool prune on the view (R9, F3)**
- Test first: the decision 7 prune tests re-run on a published view
  (`prune_on_view_uses_view_height_and_view_nonces`); `prune_without_view_reads_db_marker` (guard).
- Code: `Mempool::prune_committed_txs_on_view()`. It takes one `ingress_state()` handle, reads `executed_height =
  view.height()` from it first (DB marker if `db_only`), then the nonces through the same handle. `torus-node`
  (`main.rs:1120-1130`) calls it when the flag is on and keeps today's call otherwise (decision 2).

**Task 4.2: metrics and logs**
- `torus_exec_evm_fast_path_blocks` (counter); `torus_flush_worker_evm_trie_seconds` (W's step); the existing
  `torus_exec_evm_seconds` keeps its meaning (E's EVM section). Startup line as task 3.1. The `executing finalized
  block` line already logs `pipelined`.

**Task 4.3: docs**
- `docs/configuration-reference.md` (the flag, the RPC freshness rule of design question 4 a), the design's status
  line, `docs/perf/bench-queue.md` Q11 (section 6 here), `docs/api` note: eth head, receipts and EVM state move
  together at the checkpoint.

**Task 4.4: reviews**
- Codex torus-adversarial review of the branch (`codex_worktree.py`, scope branch). Its fixes come with tests.
  Then Q11.

Step 4 size: ~150-300 lines (estimate).

### Step 5: Q11 and the default

Q11 (section 6). If it passes, a one-line commit flips the default to on (as `TORUS_EPOCH_FAST_PATH` in s128),
together with the bench-queue defaults note ("an arm that leaves it unset gets it ON"). Then T's plan starts from P2.

## 5. The equivalence harness and its path-coverage proof

### 5.1 Nodes and comparisons
- Reference: the golden (task 0.2) = main `47c5f6fb`'s serial and barrier runs.
- Live nodes (the `P5Node` machinery, with the path switch replaced by a per-node config `{pipelined, ckpt, mode,
  evm_fast_path, epoch_fast_path, defer_books}`):
  - S: serial (no W).
  - B: pipelined, flag off (EVM heights barrier).
  - O: pipelined, flag on.
- Each node fills a task 0.4 ledger; comparisons are by key, never "after block h" (R2: `p5_compare` requires equal
  markers, `app.rs:28872-28874`, and fails S vs a checkpointing node on legitimate lag):
  - per height: `h_n` write-set hash, action-status row, side rows (receipts, tx index, hash -> number, CoreWriter
    links), compared as soon as both nodes have that height;
  - per completed height: per-CF dump hashes (CF_HASHED_* and CF_TRIE_* included) from task 0.4's completion-hook
    snapshot, taken after the EVM mirror resync (F11), never keyed by a marker read. A pipelined node's dump at
    completed height d is compared with S's reference snapshot at d (S has one for every height), and the incremental
    root against the full scan;
  - O against B, O against S, B against S, and every height and shared completed height against the golden.
- Run modes (task 0.4): D (drained lockstep, today's `P5Node`) and G (W-gated asynchronous: no drain, no forced
  checkpoint or barrier between blocks; the controller holds the parent's Flush, a checkpoint job or a deferred merge
  across EVM heights). D gives deterministic markers; only G reaches reads served by in-flight layers at interval 0
  and by C.
- At the end: the running hash, the golden's per-CF dump hashes, and the RPC answers (eth_getBalance / getCode /
  getStorageAt / getTransactionCount(latest) / eth_call on a reader contract / getTransactionReceipt / getLogs /
  getBlockByNumber) for every touched address and tx.

### 5.2 Paths that must be counted (>= 1 per configuration that can reach them)

"In flight" (C3-C6, C18, C20) means task 0.3's in-flight counter: the layer answered AND the DB, read at that
moment, gave a different answer. A plain layer hit does not count toward these rows (R2: after a drain the parent
layer still answers, with bytes the DB already has).

| # | path | how it is reached |
|---|---|---|
| C1 | EVM height pipelined (flag on) / barrier (flag off) / serial (S) | stream |
| C2 | flag-on EVM height still serial: slash, epoch boundary with the epoch flag off, non-durable header, replay after restart | forced heights; `epoch_fast_path` off config; fatal block |
| C3 | revm reads answered in flight by parent, L main and C, and reads answered by the DB, for each of CF_ACCOUNTS, CF_STORAGE, CF_CODE | stream + Every5 / Never; parent at interval 0 and C only in G mode (held Flush / held checkpoint) |
| C4 | journal reads answered in flight by parent, L main, L deferred, C, and by the DB, for positions, balances, CF_ACCOUNTS, oracle, markets, staking, CF_FEE_CONFIG cursor rows, CF_CORE_WRITER_QUEUE (`get_cf_len`, iterate) | reader and writer precompile calls; G mode; deferred hits need a held merge (`CkptState.test_gate`) or `defer_books` |
| C5 | lane read answered in flight by a layer | `EvmUserModify` in the block before a big height; G mode at interval 0 |
| C6 | due-check true from an in-flight layer row; false over an in-flight layer tombstone with the row still in the DB | lockbox deposit then an empty block; poison 5.4 |
| C7 | absorb rows of every kind: account put, account delete, slot put, zero-slot delete, code put, queue row, cursor row | stream |
| C8 | EIP-161 wipe and re-create in one window; destroyed account | forced |
| C9 | native credit to an account the window's EVM bundle also changed | stream |
| C10 | EVM height at `h % 100` (checkpoint at an EVM height); EVM epoch boundary pipelined | forced |
| C11a | failed EVM section on the fast path (Marker with status, `evm: None`) | `ReplayFault::EvmSectionError` |
| C11b | all-skipped `Ok` without a native phase: Marker with status and the hash -> number side row, wrong-nonce-only and undecodable-only each | forced (task 0.1) |
| C11c | all-skipped `Ok` with a native phase (Flush / Digest with an empty-bundle `evm`) | forced |
| C11d | a Marker with side rows converted to a Digest (`app.rs:2863-2870`) and written at the checkpoint | forced, Every5 / Never |
| C12 | local faults on the fast path (adapter, journal deferred, absorb put) | test switches (task 3.4) |
| C13 | W EVM step in a Flush (interval 0), in a Digest -> Checkpoint, and in a barrier's `checkpoint_drain` | configs + a slash after an EVM block |
| C14 | big and small lane at one height | forced |
| C15 | K-delayed order drained at a target after a checkpoint; the 200 cap spill | stream |
| C16 | revert dropping journal rows; caught inner frame; reader OOG | stream |
| C17 | BLOCKHASH answered from the DB through the overlay | stream |
| C18 | reader precompile deferred-read wait counted (<= 1 per EVM height) | held merge |
| C19 | crash points of task 3.5, each with jobs held in flight, crashed through the latch (`o0_crash`, never a bare drop: F10), the marker left at d and every height in (d, last] replayed | test switches, G mode |
| C20 | an EVM height executed while its parent's Flush (interval 0) or a checkpoint job (interval) is held, with >= 1 in-flight read from that held layer | G mode |

`o0_coverage_floor_per_config` asserts every row for every (configuration, run mode) where it is reachable. The
table of reachable pairs lives next to the test, so a new config cannot silently drop a row. It also asserts the
pairs marked unreachable: D mode at interval 0 must report 0 in-flight parent and C hits (if it ever reports more,
the table is stale or the counter is wrong).

### 5.3 Matrix (300 blocks per run in the default suite; 3,000 in an ignored long variant)
- Pipelined, Classic and OrderRows: interval 0, Every5, Never; each with flag off and on; each in D and G mode
  (task 0.4), compared by ledger with S and the golden.
- Equivalence cases forced into the stream and asserted by name (R1): wrong-nonce-only and undecodable-only EVM
  blocks, each without a native phase, with one, and at a checkpoint height; a failed section (C11a-d). For each,
  the hash -> number row, the status row, no receipt row, and `h_n` equal the golden in every configuration.
- Epoch flag {0, 1} on Classic Every5. `TORUS_CKPT_DEFER_BOOKS` {0, 1} on Classic Every5.
- Serial S in every BookMode (golden and reference).
- Deferred modes 2 / 3: per block only (they refuse an interval, as the P5 matrix).
- Split across tests to stay under nextest's 10 min cap, as `phase5_differential_long_random_*` (`app.rs:29045-29080`).

### 5.4 Proof that the harness catches the bug class (planted bypasses)
Under `test-probes`, one switch per reader class makes the flag-on path read the raw DB instead of the overlay:
- revm adapter -> `StateDb`;
- journal without layers (`NativeStateOverlay::new(db)`);
- due-check on the DB;
- lane read on the DB;
- enqueue cursor and probe reads on the DB.

For each, `o0_planted_bypass_<class>_is_caught` asserts that the harness reports a divergence, in Every5 or Never
(where the DB lags) and in G mode at interval 0, or that the tripwire fires (both expected). Poison tests (design
test 3b, 3c): a stale DB value for a key held only in the parent, only in L (main and deferred) or only in C. Each
reader returns the layer value, and the planted bypass returns the stale one.

Planted faults that only an in-flight read exposes (R2). They read through the overlay (the tripwire cannot fire)
but skip one layer, so their answer differs only while that layer is not yet durable:
- `o0_planted_skip_parent_layer` (the EVM section's overlay and journal built without the parent): caught in G mode at
  interval 0; in D mode at interval 0 it passes (asserted: the drain hides it, the reason G exists);
- `o0_planted_skip_inflight_checkpoint` (without C): caught only in G mode with a held checkpoint (on the native
  phase only with R off, task 0.4; the EVM section reads no R, so step 3 must show it caught with R on);
- `o0_planted_skip_deferred_part` (without L's deferred part): caught with a held merge.
Task 0.4 proves the same switches on today's native phase before any option 0 code exists.

### 5.5 Mapping to the design's test list
Design 5.1 -> 3.6; 5.2 -> 3.8; 5.3 -> 1.6, 5.4; 5.4 -> 1.1, 3.4; 5.5 -> 3.2, 3.6 (`rows_shared_fallbacks` 0,
`rows_builds` 1, `layer_shared` 0); 5.6 -> 3.5; 5.7-5.10 -> 0.1 stream + 3.6 + 3.7; 5.11 -> 4.1 + 3.6 RPC answers;
5.12 (T) and 5.13 (HL) are out of scope.

## 6. Bench plan Q11 (archy; replaces the design's 6.1 on current main)

- **Binary:** one build of `feat/item7-option0` after the Codex review (built after `3429855d`: PC and F on by
  default, 512 KB push threshold, header re-send 250 ms, custody off). The off and on arms run the same binary.
- **Arms (2 x 2):** N-off, E-off, N-on, E-on. Every arm sets the flag explicitly (`TORUS_EVM_FAST_PATH=0` or `=1`),
  never "absent", so the arms keep their meaning when the default flips (the s128 defaults lesson). E arms:
  `EVM_LOAD_RATE=10 EVM_LOAD_SENDERS=40` (Q5's load).
- **Shape:** the standard load (bench-queue.md: hsdb-300m, 300 markets, rate 76,000, Classic, 15 s checkpoints,
  oracle on, `BLOCK_CAP=400 RETRY_BUSY=1 MAX_IN_FLIGHT=4 OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 IN_FLIGHT_TAIL=commit
  IN_FLIGHT_TAIL_DEPTH=1 CLOID_SHARE=0`), N = 11 bench genesis, 600 s cells, 3 validators.
- **Order:** smoke per arm (60 s), warm-up w1 (E-on, 300 s), then r1 N-off E-off E-on N-on, r2 N-on E-on E-off N-off,
  r3 as r1 (n = 3; ~2 h 20 with setup, estimate). Extend to n = 4 if a merge-rule CI straddles 1 by under 1 point.
- **Merge rule (bench-queue.md, owner s128), on E-on / E-off:** besides matched/s, exec ms per 1k fills and CPU-s
  per 1M fills, it judges exec-thread on-CPU ms per 1k fills (`sched_by_node` `torus-execution`, as section 91),
  native blocks/s, block p50 / p99 and timeouts per 1,000 blocks. The flag does not ship on if exec-thread CPU per
  fill has its CI above 1, or blocks/s, p50 or p99 has its CI on the wrong side of 1. N-on / N-off: the same rows
  within noise (the flag must be inert without EVM).
- **Option 0 passes (design 6.1, kept):**
  1. E-on / N-on >= 0.97 matched/s; E-off / N-off reproduces section 85's 0.913 within ~2 points.
  2. Pipelined share at EVM heights >= 99% in E-on; exec idle before an EVM block p50 < 5 ms.
  3. `torus_exec_checkpoint_deferred_read_waits` delta 0 (the transfer load calls no precompile).
  4. `torus_exec_resident_rows_rebuilds` delta 0 and `torus_exec_checkpoint_layer_shared` 0, equal across arms.
  5. `torus_exec_checkpoints` per minute and layer bytes per minute: E-on within noise of N-on (no forced
     checkpoints); E-off shows the forced ones.
  6. EVM checks: landed = accepted, `off_cadence_blocks` 0, eth head reaches the load's last height within one
     interval (`evm_load.py` `wait_head` allows 60 s), no fail-stop or divergence line, attestations agree.
- **Expected (estimate):** E-off native blocks/s ~3.3 (section 85: 12.99 -> 3.33 under EVM load), E-on ~12-13 with
  block p50 back near 14-18 ms; E-on / N-on matched/s 0.97-0.99; exec-thread CPU per fill at or below E-off (the EVM
  root, metadata write and serial flush leave E; the absorb and the layered reads are small).
- **Report:** the q5-gaps view (exec ms at EVM heights vs others, idle before EVM blocks), `torus_exec_evm_seconds`
  per EVM block, `torus_flush_worker_evm_trie_seconds`, W busy, commit -> exec lag, exec queue peak, CPU-s / 1M, RSS,
  `no_proposal`.
- **Extra cells (n = 1 each, after the matrix):** E-heavy `EVM_LOAD_RATE=40` off and on (fills the 3M lane; sizes
  C0's trigger, design question 6); one SIGKILL crash cell under EVM load with the flag on (digests agree, replay
  length); one mixed-flag cell (val0 on, val1 off, val2 on with `TORUS_CHECKPOINT_INTERVAL_MS=0`): attestations
  agree for the whole cell.
- **Doc target:** the next results-doc section; 18c copies the verdict into the design's status line.

**Keep N = 11? Yes** (decision 7). It is the genesis default and the testnet value. It is Q5's (section 80) and Q12
r1's E11 (section 85) cadence, so E-off reproduces a measured baseline. With 3 bench validators, 11 is not a multiple
of 3, so EVM heights rotate over the leaders, unlike Q12 r1's N = 33 / 66 (see section 7). The owner fixed N = 11 in
s128 (bench-queue item 7). Option 0 makes the per-EVM-block cost nearly independent of N (no barrier), so N becomes a
latency / product parameter and stops being a performance knob. A second N adds nothing to Q11.

## 7. Item 7 "to check before genesis": who proposes the EVM blocks

**Answer from the code: yes, with equal stake one validator proposes every EVM block.** Leaders come from
`select_leader(view, validator_set)` (`hotstuff_rs/src/pacemaker/implementation.rs:1066`): interleaved weighted
round robin over the IWRR array, index `view % total_power`. With equal powers the array is the validator list
repeated, so the leader is `view mod n`. On the happy path the view advances by one per height
(`next_evm_targets`, `app.rs:1074-1137`, maps a target height to `first_view + (h - tip - 1)`). An EVM height
`h = k x N` therefore gets the leader `(c + k x N) mod n`. With N = 11 and n = 11 that is the same validator every
time. Each timeout shifts `c` by one, so the role moves only on a timeout (0.1-1.4 per 1,000 blocks in recent cells),
and to the next validator. With unequal stake the EVM heights sample `total_power / gcd(N, total_power)` IWRR slots,
skewed but not one validator. With 3 validators the same happens for every N that is a multiple of 3 (Q12 r1's N = 33
/ 66). The reputation-weighted variant (`implementation.rs:1166-1207`) rescales powers after a 20-view warm-up, which
can break the pattern, but not by design.

Consequences: one validator orders and includes every small-lane EVM block and collects its priority fees
(`beneficiary: block.header.proposer`, `validator.rs:184`). The EVM routing sends every tx to that leader. A slow or
censoring leader stalls all EVM inclusion until a timeout moves the role.

**Option 0 does not change it.** Leader choice is a function of the view and the set; EVM cadence a function of the
height. Option 0 changes only when and where a block executes. Indirectly it removes the EVM-height exec stalls. If
those stalls cost timeouts, fewer timeouts make the offset drift less, so the same proposer keeps the EVM heights
longer.

**Where it belongs:** item 7, before the testnet genesis, as its own small design note. It touches consensus or the
EVM cadence rule, so it is genesis-bound or needs P0.0 activation, and it is independent of option 0 and T. Candidate
fixes for that note: an extra rotation per EVM round in the leader schedule (e.g. `select_leader(view + view / N)`,
consensus-wide); a per-height hashed permutation; or the EVM cadence on a stride kept coprime with the set size
(fragile as the set changes). Pick in decision 9. A test belongs with it:
`evm_heights_rotate_proposers` (n in {3, 4, 7, 11, 21} x N in {11, 22, 33}, equal and unequal stake). Roadmap entry:
item 7 pre-genesis list, next to T.

## 8. Risks

1. **A missed raw read** answers from the DB at S on one node and from a layer on another: a divergence, visible only
   when checkpoint timing differs. Guards: the typed entry points (1.5), the tripwire (1.6), the poison tests and
   planted bypasses (5.4), the coverage floor (5.2), mixed replicas (3.8), Codex adversarial review. Residual: a read
   outside the armed scope (the native phase is already layered; the tripwire covers only the EVM section).
2. **F1 left open** would make a deferred-merge failure a revert on one node. The fix branch closes it; gate D1
   forbids flag-on execution until it is in the base; task 1.1's audit and task 3.4 cover the fast path's new
   source.
2b. **A harness blind to in-flight reads** (R2): drained runs pass a reader that skips the parent layer or C. G mode,
   the in-flight counter and the skip-layer planted faults (5.4) close it. G mode is timing-driven; its coverage is
   forced by the gate, its results must be timing-free, and a watchdog turns a gate deadlock into a failure. Residual:
   a schedule the seeded gate never produces; the ignored 3,000-block variant runs more seeds.
2c. **A harness that tests a crash that is not one, or compares half-written heights** (R4, R5): a bare drop lands
   the held jobs (F10), and a marker-keyed snapshot can precede the EVM resync (F11). Closed by `o0_crash`'s latch and
   its step-4 assertions with the guards `o0_bare_drop_lands_held_jobs` / `o0_crash_protocol_rejects_unlatched_crash`,
   and by the completion hook with `o0_marker_keyed_snapshot_sees_pre_resync_mirror`. Residual: the hook sites are
   test-only code on W and E. A new durable write after the hook (a later refactor) would reopen F11. The hook's
   assert "marker in the snapshot == tag" catches a misplaced hook, but not a write added after it.
3. **Window trie != per-block trie bytes** (CF_TRIE_* is node-local and unhashed, but the golden compares it).
   Decision 4 has the fallback.
4. **The journal keeps L / C shared** -> deep copy at `take_layer`, or R rebuilt. Test 3.6 asserts 0; Q11 pass 4.
5. **Deferred-part waits** on reader precompiles (<= 1 per EVM height, ~20 ms, estimate). Not measured under contract
   load; Q11's later contract-call cell.
6. **W load:** the EVM trie step and the receipts encoding move to W. That is small at Q5's load (~240 accounts per
   15 s window, estimate) but grows with full lanes. The E-heavy cell measures it; the checkpoint wait metric shows
   whether E ever waits on W.
7. **RPC freshness:** with the flag on, EVM state and receipts lag up to one checkpoint interval (accepted, design
   question 4 a). Wallets that poll receipts see `null` for up to ~15 s. `pending` nonces stay fresh (the view).
8. **Golden brittleness:** a rebase onto a main that changes execution needs a re-capture (task 0.2 rule). A golden
   written on the wrong base would bless a bug. The commit that writes it must have `47c5f6fb` (or the named new
   base) as parent, checked in review.
9. **Q6 A1** (parked): if it comes back, its sink must class the EVM CF rows and trie ops (design 4.4 item 3).
10. **Leader concentration** (section 7) is not a risk of option 0, but Q11's results do not cover it (3 validators,
    N = 11).

## 9. Rollback

- Before the default flip: unset or `0` = today's path on that node. Mixed flags are safe (3.8, Q11 mixed cell).
- After the flip: `TORUS_EVM_FAST_PATH=0` per node, no restart of the others, no genesis change.
- Step 1 has no flag: a revert of its commits restores today's code (its outputs are golden-equal, so no data
  migration). The F1 fix is on main independently of option 0 (its own branch) and stays if option 0 is dropped.
- Data: flag off -> on -> off needs nothing. The EVM trie is either current (the step ran in the checkpoint batch)
  or marked stale and rebuilt at boot (2.3).

## 10. Decisions for the owner

**Decided (owner s128): every recommendation accepted** (1a, 2a, 4a, 5a, 6a, 7a, 8a, 9a, 10a, 11a, 12a, 13a; 3 settled
earlier). The options NOT chosen stay listed below with their trade-offs, for a later revisit: 1b / 1c, 2b, 4b, 5b,
6b, 7b, 8b, 9b / 9c, 10b, 11b, 12b, 13b. Revisit triggers worth noting: 6b if a release-only raw read is ever
suspected; 7b if Q11 shows N matters with the fast path on; 10b if Q11 passes but T changes the EVM path; 13b if a
crash bug shows up that the in-process latch cannot reach.


1. **Reference for "byte-identical to today".** (a) A golden record written once on `47c5f6fb` (task 0.2) plus the
   live S / B / O differential; (b) keep today's EVM batch path as a `cfg(test)` reference path (Phase 5 style);
   (c) live differential only. **Recommend a:** b keeps two serial paths alive in the code, and c compares the new
   code with itself after step 1 has changed the serial path.
2. **R9 (mempool prune on the view): flag-gated or always?** (a) Only with the flag on; today's raw-DB prune
   otherwise. (b) Always. **Recommend a:** flag off then stays exactly today, proposer behaviour included; it becomes
   unconditional with the default flip.
3. **F1 (precompile local faults) timing. SETTLED (a) in revision 1:** its own fix on
   `fix/precompile-read-error-fail-stop`, ahead of option 0. The plan now depends on it (gate D1); task 1.1 keeps
   only the audit. No choice left for the owner.
4. **CF_TRIE_* byte-equality as a gate.** (a) A hard gate; if the window step fails it, build the per-block fallback
   (task 2.2). (b) Accept root + mirror equality and log trie-byte differences. **Recommend a:** the golden then
   proves the whole state with no exception list; the fallback costs ~150 lines (estimate).
5. **Absorb on the serial path in step 1** (design P1 b) or only on the fast path. (a) Both paths. (b) Fast path
   only, the serial prefix unchanged. **Recommend a:** one code path for EVM rows, and T needs it on both paths
   anyway (design 3 T). The golden guards the serial change.
6. **The tripwire in release builds.** (a) Debug and tests only. (b) Also release, as a counter instead of a panic.
   **Recommend a:** the poison tests and the coverage floor give the proof; a release counter would cost a
   thread-local load per read on every node.
7. **N for Q11.** (a) N = 11 only. (b) Add an N = 33 E-on cell. **Recommend a** (section 6).
8. **Q11 size.** (a) 4 arms, n = 3 (~2 h 20). (b) Drop N-on (n = 3 of three arms). **Recommend a:** N-on / N-off is
   the only proof the flag is inert without EVM under the merge rule.
9. **Leader concentration at EVM heights (section 7).** (a) A separate item 7 pre-genesis design note with the test
   above, fix picked there. (b) Accept it for testnet and measure. (c) Fold it into option 0. **Recommend a:** it is
   genesis-bound and consensus-visible, and option 0 is neither.
10. **Default flip.** (a) On after Q11 passes (design question 1 a, kept). (b) On only after T. **Recommend a.**
11. **(new, R1) Side rows of an all-skipped EVM block without a native phase.** (a) The Marker job carries `evm`
    (empty bundle, side rows), like Flush / Digest. (b) Such a block hands off a Flush with a marker-only pending set
    instead of a Marker. **Recommend a:** one field on the job W already handles, and `pipeline_handoff`'s Marker ->
    Digest arm must carry the status row anyway; b adds a second shape for "nothing executed".
12. **(new, R2) G mode in the default suite.** (a) G mode in the default suite for every pipelined config (more
    wall time, ~1.3-1.6x the D-only matrix, estimate) with free-running seeds in the ignored long variant. (b) G only
    in the ignored long variant. **Recommend a:** the in-flight floor and the skip-layer planted faults are only
    reachable in G; leaving them out of the per-commit gate repeats Phase 5 step 2's unreached-path lesson.

13. **(new, R4) How the crash tests crash.** (a) In process: latch the fail-stop (seam or `gate.fail_next()`), then
    drop, as `ckpt_crash_replay_with` does; a bare drop is forbidden by the guard test. (b) The node as a child
    process, SIGKILLed with W held. **Recommend a:** after the latch W writes nothing, and the atomic batch makes a
    kill land all of a job or none of it, so (b) reaches no extra path at the cost of a process harness in a unit test.
    The real kill stays in Q11's SIGKILL cell.

Recommendations unchanged by revisions 1 and 2: 1, 2, 4-10, 12. Decision 1 (a) keeps the golden, now stored by height
and by completed height (task 0.2). Decision 3 is no longer open.

## 11. Commit groups and size

| group | tasks | content | size (lines, incl. tests; estimate) | builder hours (estimate) |
|---|---|---|---|---|
| G0 | 0.1-0.4 | stream, golden, probes, ledger comparator, completion hook, G-mode runner | 1,120-1,450 | 14-18 |
| G1a | 1.1 | F1 dependency check + classification audit (the fix itself is on `fix/precompile-read-error-fail-stop`, not counted) | 50-150 | 1-2 |
| G1b | 1.2-1.7 | absorb, overlay first, typed entry points, tripwire, docs | 450-650 | 9-12 |
| G2 | 2.1-2.4 | W: side rows (Marker included), EVM trie step, stale marker, jobs | 580-800 | 11-15 |
| G3 | 3.1-3.8 | flag, fast-path section, all-skipped cases, faults, latched crashes, harness matrix (D + G) | 750-1,070 | 14-19 |
| G4 | 4.1-4.4 | mempool view prune, metrics, docs, review fixes | 150-300 | 4-6 + review |
| | | **total** | **~3.1-4.5k** | **~53-72 h** |

Revision 1 changes the total from ~2.6-3.7k / 45-62 h: +350-450 for task 0.4 (R2), +30-50 in G2 and +100-150 in G3
for the all-skipped side rows and the G-mode matrix (R1, R2), -100 in G1a (the F1 fix moved to its own branch, R3).
Revision 2 adds +70-100 in G0 (completion hook, resync seam, guards, R5) and +50-70 in G3 (crash protocol, R4):
~3.0-4.3k / 51-70 h -> ~3.1-4.5k / 53-72 h.
The design said ~1.5-2.2k. The difference is the reference and the coverage proof the s128 lessons ask for
(G0, ~1.2k), the F1 audit, the stale-trie handling (F4) and the per-config coverage floor. G1a waits for the F1 fix
(gate D1) and can go in parallel with G0; the rest is sequential, and G3 cannot start before D1 holds. Q11 is ~2 h 20 of bench time plus the extra cells (bench agent). Each group passes
the per-commit gate; G3 merges only with Codex torus-adversarial approval. Before merging to main: TESTING.md's full
`cargo test`, `RUSTFLAGS="-D warnings"` build, fmt and clippy counts not above main's (as the step 2 plan's
verification block).

## 12. Not building

- T, HL order, C0, C1, D (design section 3; T gets its own plan after P2).
- A checkpoint at EVM heights or any knob for it (design question 4 a obsolete).
- Fresher eth reads through the view (design question 4 b; with Phase 5 parked item 1, before testnet if wallets need
  it).
- Re-checking the EVM root on E with the flag on (nothing in consensus reads it; the root check runs on W against the
  full scan in tests).
- Any change to `validate_block_with_overlay` / `execute_block_with_overlay` (F9).
