# Implementation Plan: item 7 step 3 option 0, the EVM fast path

Status (s128): PLAN, nothing built. Design: `docs/plans/item7-step3-design.md` revision 2 (on main; section 7 picks
1-11 accepted by the owner s127). This plan covers option 0 only: design phases P1 and P2 (section 4.1). T (P3) gets
its own plan after P2 merges; C0 stays parked (design question 6). Format: `item7-step2-impl.md`,
`item6-phase5-impl.md`.

**Base: main `47c5f6fb`.** Every `file:line` below is on that commit (crate paths under `crates/`), unless marked
"design" (the design's anchors, written on `dcee5cac` / `c567b1b3`). Section 2 re-checks every design anchor.
Since the design:
- item 7 step 2 is merged (`cfa35ed1`), so the design's `@ c567b1b3` code is on main;
- the design's P1 (a), the revm read-error fail-stop (question 11), is merged (`27f49ad3`), with two sibling fixes:
  a state-root compute read error in catch-up fail-stops (`dab088b3`), and so does a failed metadata write
  (`17456488`);
- item 6 Phase 5 is PARKED after step 1 (owner s128, results section 91): option 0 builds on main as it is, not on
  Phase 5 step 2. Phase 5 step 1's differential harness (`phase5_differential_*`, `app.rs:28245-29080`) is on main and
  is the base of this plan's equivalence harness;
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
| flag on = today | the same golden, in every checkpoint policy, with EVM heights pipelined | test 3.6 |
| coverage | every path in section 5's table counted >= 1 in every configuration that can reach it; each planted bypass (section 5.4) makes the harness fail | test 3.6, 3.7 |
| no raw read | the tripwire (task 1.6) gets 0 hits over the whole stream with the flag on | test 3.6 |
| local faults | every new read path's error fail-stops (never a revert, never a skipped EVM part); restart replays to the uncrashed state | tests 1.1, 3.4 |
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
  these become local faults; the code does not do it. Task 1.1 fixes it (decision 3).
- **F2. An EVM block that executed a tx always runs the native phase** (`evm_gas != (0, 0)`, `app.rs:3786`). The only
  EVM blocks without one executed nothing: a failed EVM section (generic arm) or all txs skipped. Those absorb no rows.
  So on the fast path "absorbed rows and no native phase" cannot occur. The no-native case needs only its
  executed / skipped status row on the Marker job (task 3.3), not a frozen overlay. A debug assert pins it.
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
            else         { evm_batch = hashed mirror + trie + links (today's prefix minus the plain / journal rows);
                           commit_block_metadata(...) }        // serial: unchanged
        }
        Err(LocalFault | State) => fail-stop, Err(other) => log (deterministic), as today
    }
}
core_writer_due(&overlay, h)                                   // step 1: through the overlay, after the absorb
... native phase unchanged (seed_from_bundle is gone: the absorb did it) ...
pipelined: freeze -> Flush / Digest { .., evm: evm_job }       // W: EVM trie step + side rows in the same batch
no native phase: Marker { .., action_status }                  // F2: nothing absorbed
```

W side (step 2): a Flush job with `evm` writes the block's EVM mirror and trie ops and its side rows in the block's
batch. A Digest job folds `evm` into the window. The checkpoint writes the window's EVM ops and side rows in its batch,
before the marker. The post-checkpoint `resync_evm` stays as a safety net; it finds every mirror already equal.

## 4. Steps and tasks

Test names are the RED tests each task adds. Each fails before its code (compile error or wrong value), except
those marked "guard", which pass before and must stay green. "Golden" = the step 0 record. Per-commit gate: TESTING.md
(workspace nextest + doc tests, shown in the commit's notes).

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
  - `TransferToSpot` in a native block, spent by the next EVM height; `TransferToPerp` in the same block as EVM txs.
  - Native credits (fees, rewards) to EVM accounts the window's EVM bundles also touch.
  - Big lane: the fixture header carries `evm_big_block_interval` M = 10 (big heights `h % 10 == 0`, with
    `TEST_EVM_BLOCK_INTERVAL` 5 at `app.rs:262`); an `EvmUserModify` in a native block makes a sender big; a big
    height carries both lanes.
  - Forced EVM heights at: an epoch boundary (`h % 50`), an attest height (`h % 100`), a slash height, the fatal
    block's successor, and an EVM block whose section fails (`ReplayFault::EvmSectionError`, `app.rs:3546`).
  - Over 200 queued rows to one target (the cap spill), and a K-delayed target that falls after a checkpoint.
- Verify: `cargo nextest run -p torus-consensus $F -E 'test(o0_stream_reaches_every_input)'`.

**Task 0.2: the golden record of today (main `47c5f6fb`)**
- Test first: `o0_golden_today` runs the stream on today's code in three configurations and compares with
  `crates/torus-consensus/tests/fixtures/item7_opt0_golden.json`. The configurations: serial (no W); pipelined
  interval 0; pipelined Every5 (EVM heights barrier today). In each it records, per height: the `h_n` write set hash
  (`capture_take`, as `p5_compare`), the action-status row, the receipts / tx-location / hash -> number /
  CoreWriter-link rows of that height. At the end: a hash per CF of the full dump (all `ALL_CF_NAMES`, CF_HASHED_* and
  CF_TRIE_* included), the running hash, the EVM root (incremental == full scan) and the RPC answers of 3.6's list.
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
    per reader class.
  The `ExecutionContext` gets `#[cfg(test)]` path flags in the style of `test_nonce_prune` (pipelined EVM height,
  serial EVM height by reason, absorb rows by kind, Marker with status, W EVM step per job kind, ...). Section 5.2
  lists them.
- Test first: `o0_coverage_report_counts_a_known_stream`: a 12-block hand stream gives exact counts.
- Verify: `cargo nextest run -p torus-consensus -p torus-state $F -E 'test(o0_coverage)'`.

Size: ~700-900 lines, almost all tests (estimate).

### Step 1 (design P1): behaviour-neutral preparation, no flag

Everything in step 1 runs on today's paths. The golden must stay byte-equal after every task.

**Task 1.1: precompile local faults are never reverts (F1)**
- Test first (`torus-evm/tests/r02_precompile_fault_tests.rs`, whose `faulty_db` already opens a DB without one CF;
  the `27f49ad3` seam in `evm_db_fault_tests.rs` is the same idea):
  - `balance_reader_read_fault_is_a_local_fault`: a DB without CF_NATIVE_BALANCES; a contract calls the balance
    reader. Today the receipt is a revert (status false). Expected: `EvmError::LocalFault`.
  - `enqueue_read_fault_is_a_local_fault`: a DB without CF_FEE_CONFIG; `placeOrder`. Expected: `LocalFault`.
  - `deterministic_precompile_errors_stay_reverts` (guard): bad calldata, non-payable value, OOG, a writer in
    `eth_call` keep their revert.
  - `torus-consensus`: `precompile_read_fault_fail_stops_block` (as `evm_db_read_fault_fail_stops_block`,
    `app.rs:27485`).
- Code: in `TorusPrecompiles::run` (`precompile_provider.rs:208`), record any `Err(e)` with `e.is_local_fault()` in
  the cell (not only `PrecompileLocalFault`), then abort as for `PrecompileLocalFault`. Audit every
  `State` / `Borsh` / `MissingCf` / `BookCorrupt` source reachable from `execute_precompile_metered`
  (`precompiles.rs:528-607`). Each must be a local storage or row fault, never a function of the calldata. Where one
  is input-dependent, map it to `InvalidPrecompileInput` first, with a test.
- Verify: `cargo nextest run -E 'rdeps(torus-evm)' $F`; golden unchanged (no fault in the stream).
- Decision 3: ship 1.1 as its own fix branch on main ahead of option 0 (like the s127 Q11a fix).

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

**Task 1.4: the lane read and the enqueue reads go through a backend (R7, R8)**
- Test first: `lane_row_in_parent_layer_sets_big_fee` (`torus-bridge`): `validate_block_inner` over a layered backend
  sees an `EvmUserModify` row that is only in the parent layer. Enqueue: `enqueue_reads_cursor_rows_from_layers`
  (`torus-core`): cursor rows and full-target probes held only in L give the same target and seq as with them in the
  DB.
- Code: `validate_block_inner` (`validator.rs:118`) reads the lane row through the `EvmReads` argument of task 1.5
  (`154-178`; the comment "runs after a barrier" goes). Enqueue already takes `&impl StateBackend`: fix the doc
  (`precompiles.rs:1407`).
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`; golden green.

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

**Task 1.6: the raw-DB tripwire**
- Test first: `tripwire_fires_on_raw_read_in_armed_scope` and `tripwire_allows_overlay_bottom_reads` (`torus-state`).
- Code: under `cfg(any(debug_assertions, feature = "test-probes"))`, a thread-local guard. While armed, every `StateDb`
  read entry point panics: `get_cf_raw`, `get_cf_len`, the iterators, `prefix_exists`, `DatabaseRef`, `get_account`,
  `get_block_hash`, `inner()`, `cf_handle`. The overlay's DB fall-through reads (`backend.rs:2499`, `2615`, `2629`,
  `2671`, `2690`, `2723`) hold an exemption token around the call. Release builds compile it out. Step 3 arms it for
  the fast-path EVM section.
- Verify: `cargo nextest run -p torus-state $F` + `cargo check --workspace --tests -q`.

**Task 1.7: docs of the barrier assumptions** (`native_executor.rs:13619-13628`, `precompiles.rs:1407`,
`validator.rs:154-157`, `app.rs:3418-3426`): they now say "read through the block overlay".

Step 1 size: ~600-850 lines, about half tests (estimate). Golden green after every task.

### Step 2 (design P2, W side): the EVM trie and the side rows on W (dormant until step 3)

**Task 2.1: EVM side rows on the job**
- Test first: `evm_side_rows_equal_commit_block_metadata` (`torus-consensus`, `exec_pipeline` tests): W's encoding of
  `EvmSideRows` gives byte-equal CF_RECEIPTS, CF_BLOCK_HASH_TO_NUMBER, CF_TX_HASH_TO_LOCATION and CoreWriter-link rows
  to `commit_block_metadata` (`committer.rs:145-197`) + `append_links` (`corewriter_tx.rs:68`), minus header and body.
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
- Verify: `cargo nextest run -E 'rdeps(torus-bridge)' $F`.

**Task 2.4: job plumbing**
- Code: `Job::Flush` and `Job::Digest` gain `evm: Option<EvmJobRows { bundle, side }>`. `Job::Marker` gains
  `action_status` (F2), carried into `Digest` by `pipeline_handoff` (`app.rs:2858`). The `Window` gains
  `evm: EvmWindowDelta`. A barrier's `checkpoint_drain` closes it like any window. The serial path never sets `evm`.
- Test first: `marker_job_carries_action_status_to_checkpoint`.

Step 2 size: ~550-750 lines (estimate).

### Step 3 (design P2, E side): the flag and the fast-path EVM section

**Task 3.1: the flag**
- Test first: `parse_evm_fast_path` (unset / `0` / `false` = off; `1` / `true` = on; else off + warning, as
  `parse_epoch_fast_path`, `exec_pipeline.rs:68-87`) and `evm_fast_path_flag_absent_evm_heights_stay_serial`
  (the `epoch_fast_path_evm_or_slash_boundary_stays_serial` pattern, `app.rs:30685`).
- Code: `exec_pipeline::evm_fast_path_enabled()`, an `ExecutionContext.evm_fast_path` field read once in `try_new`
  (next to `epoch_fast_path`, `app.rs:6172`), a startup log line as at `6266-6285` (no effect without W). The
  predicate term `(!has_evm || self.evm_fast_path)` (`3436`).

**Task 3.2: the fast-path EVM section**
- Code:
  - With `pipelined && has_evm`: `EvmBlockState::new(&overlay)`, arm the tripwire, and `validate_block_for_catchup(..,
    RootMode::Skip)`. Skip computes no root and no full-scan oracle (F6), and gives `evm_root_updates: None`.
  - `overlay.absorb_evm(..)` (task 1.2). The journal is consumed, and `ValidatedBlock.native_writes` drops with it
    before the native phase. Otherwise L, its deferred part or C stay shared at `take_layer` (`app.rs:220-254`).
  - `evm_job = EvmJobRows { bundle, side }`. No `evm_batch`, no `commit_block_metadata`, no `append_links`.
  - The assert at `4573` becomes "a pipelined block never built an `evm_batch`".
  - Errors keep today's arms: `LocalFault` and `BridgeError::State` fail-stop, the generic arm logs. An absorb put
    error fail-stops with `fail_stop_on_fault`.
- Test first: tests 3.6-3.8 below; `fast_path_evm_block_hands_off_evm_rows` (unit); `journal_dropped_before_native`
  (the layer-shared counter is 0 after a reader-precompile block).

**Task 3.3: hand-off and the no-native EVM block**
- Code: the Flush / Digest jobs get `evm: evm_job`. A no-native EVM block on the fast path (F2: failed section or all
  skipped) hands off `Marker { action_status: evm_skipped }`, with `debug_assert!(evm_job.is_none())`.
  `advance_untouched` runs as today.
- Test first: `fast_path_failed_evm_section_writes_status_with_marker`.

**Task 3.4: local faults on the fast path**
- Test first: `fast_path_adapter_read_fault_fail_stops`, `fast_path_deferred_merge_failure_under_journal_fail_stops`
  (the merge thread's failure seam), `fast_path_absorb_put_fault_fail_stops`. Each: nothing is handed off, the marker
  stays below h, and the restart replays to the uncrashed dump.

**Task 3.5: W crash and replay**
- Test first: SIGKILL-style crashes (drop the context with W gated, the `WorkerGate` seams): after the EVM part
  before the absorb (`test_crash_after_evm_section`, `app.rs:3688`); after the absorb before the hand-off; after the
  hand-off before the checkpoint; during W's EVM trie step (inject); during the replay of an EVM block. The restart
  replays serially, and the dump (EVM trie included) and `h_n` equal the uncrashed serial run. Single and repeated
  crashes. Template: `epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` (`app.rs:30727`).

**Task 3.6: the equivalence harness with the flag** (section 5)
- Test first: `o0_equivalence_*` (section 5.3's matrix) against the golden, plus `o0_coverage_floor_per_config` and
  the planted bypasses of 5.4.
- Also update `epoch_fast_path_evm_or_slash_boundary_stays_serial`: with the EVM flag on, an EVM boundary block is
  pipelined; a slash or a non-durable header still barriers.

**Task 3.7: existing EVM tests on the fast path**
- `corewriter_evm_row_drains_in_empty_next_block` (+ `_with_interval`, `app.rs:25834`, `25923`),
  `core_writer_tx_links_each_queued_row_to_its_evm_tx` (`21585`) and
  `f3_epoch_gas_sets_plan_fees_serial_and_pipelined` (`21496`) gain a flag-on run. So does the R02 branch 5 fault
  family (`evm_db_read_fault_fail_stops_block` `27485`, `ReplayFault::EvmLocalFault`).

**Task 3.8: mixed replicas**
- Test first: `o0_mixed_replicas_agree`: three nodes on one stream (flag off; flag on with Every5; flag on with
  interval 0) agree on every `h_n` and every state-hash attestation row (`h % 100`).

Step 3 size: ~600-850 lines, mostly tests (estimate).

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
- Per block, as `p5_compare` (`app.rs:28820`): the `h_n` write sets hashed since the last block, the durable markers
  and the action-status rows of newly durable heights. O is compared against B and against S, and every height's
  `h_n` against the golden.
- After each durable advance: full CF dumps. O's CF_HASHED_* and CF_TRIE_* are compared after each checkpoint (S has
  them per block), and the incremental root against the full scan.
- At the end: the running hash, the golden's per-CF dump hashes, and the RPC answers (eth_getBalance / getCode /
  getStorageAt / getTransactionCount(latest) / eth_call on a reader contract / getTransactionReceipt / getLogs /
  getBlockByNumber) for every touched address and tx.

### 5.2 Paths that must be counted (>= 1 per configuration that can reach them)

| # | path | how it is reached |
|---|---|---|
| C1 | EVM height pipelined (flag on) / barrier (flag off) / serial (S) | stream |
| C2 | flag-on EVM height still serial: slash, epoch boundary with the epoch flag off, non-durable header, replay after restart | forced heights; `epoch_fast_path` off config; fatal block |
| C3 | revm reads answered by parent, L main, C and DB, for each of CF_ACCOUNTS, CF_STORAGE, CF_CODE | stream + Every5 / Never; C needs a held checkpoint (`WorkerGate`) |
| C4 | journal reads answered by parent, L main, L deferred, C, DB for positions, balances, CF_ACCOUNTS, oracle, markets, staking, CF_FEE_CONFIG cursor rows, CF_CORE_WRITER_QUEUE (`get_cf_len`, iterate) | reader and writer precompile calls; deferred hits need a held merge (`CkptState.test_gate`) or `defer_books` |
| C5 | lane read answered by a layer | `EvmUserModify` in the block before a big height |
| C6 | due-check true from a layer row; false over a layer tombstone with the row still in the DB | lockbox deposit then an empty block; poison 5.4 |
| C7 | absorb rows of every kind: account put, account delete, slot put, zero-slot delete, code put, queue row, cursor row | stream |
| C8 | EIP-161 wipe and re-create in one window; destroyed account | forced |
| C9 | native credit to an account the window's EVM bundle also changed | stream |
| C10 | EVM height at `h % 100` (checkpoint at an EVM height); EVM epoch boundary pipelined | forced |
| C11 | failed EVM section on the fast path (Marker with status) | `ReplayFault::EvmSectionError` |
| C12 | local faults on the fast path (adapter, journal deferred, absorb put) | test switches (task 3.4) |
| C13 | W EVM step in a Flush (interval 0), in a Digest -> Checkpoint, and in a barrier's `checkpoint_drain` | configs + a slash after an EVM block |
| C14 | big and small lane at one height | forced |
| C15 | K-delayed order drained at a target after a checkpoint; the 200 cap spill | stream |
| C16 | revert dropping journal rows; caught inner frame; reader OOG | stream |
| C17 | BLOCKHASH answered from the DB through the overlay | stream |
| C18 | reader precompile deferred-read wait counted (<= 1 per EVM height) | held merge |
| C19 | crash points of task 3.5 | test switches |

`o0_coverage_floor_per_config` asserts every row for every configuration where it is reachable. The table of
reachable (config, row) pairs lives next to the test, so a new config cannot silently drop a row.

### 5.3 Matrix (300 blocks per run in the default suite; 3,000 in an ignored long variant)
- Pipelined, Classic and OrderRows: interval 0, Every5, Never; each with flag off and on.
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
(where the DB lags), or that the tripwire fires (both expected). Poison tests (design test 3b, 3c): a stale DB value
for a key held only in the parent, only in L (main and deferred) or only in C. Each reader returns the layer value,
and the planted bypass returns the stale one.

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
2. **F1 left open** would make a deferred-merge failure a revert on one node. Task 1.1 closes it before any flag-on
   code.
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
  migration). Task 1.1 (F1) should stay even if option 0 is dropped: it fixes today's path.
- Data: flag off -> on -> off needs nothing. The EVM trie is either current (the step ran in the checkpoint batch)
  or marked stale and rebuilt at boot (2.3).

## 10. Decisions for the owner

1. **Reference for "byte-identical to today".** (a) A golden record written once on `47c5f6fb` (task 0.2) plus the
   live S / B / O differential; (b) keep today's EVM batch path as a `cfg(test)` reference path (Phase 5 style);
   (c) live differential only. **Recommend a:** b keeps two serial paths alive in the code, and c compares the new
   code with itself after step 1 has changed the serial path.
2. **R9 (mempool prune on the view): flag-gated or always?** (a) Only with the flag on; today's raw-DB prune
   otherwise. (b) Always. **Recommend a:** flag off then stays exactly today, proposer behaviour included; it becomes
   unconditional with the default flip.
3. **F1 (precompile local faults) timing.** (a) Its own fix on main now, ahead of option 0 (like Q11a in s127).
   (b) Inside step 1. **Recommend a:** it is a divergence gap on today's path and does not depend on option 0.
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

## 11. Commit groups and size

| group | tasks | content | size (lines, incl. tests; estimate) | builder hours (estimate) |
|---|---|---|---|---|
| G0 | 0.1-0.3 | stream, golden, probes | 700-900 | 8-11 |
| G1a | 1.1 | precompile local faults (own branch, decision 3) | 150-250 | 3-4 |
| G1b | 1.2-1.7 | absorb, overlay first, typed entry points, tripwire, docs | 450-650 | 9-12 |
| G2 | 2.1-2.4 | W: side rows, EVM trie step, stale marker, jobs | 550-750 | 10-14 |
| G3 | 3.1-3.8 | flag, fast-path section, faults, crashes, harness matrix | 600-850 | 11-15 |
| G4 | 4.1-4.4 | mempool view prune, metrics, docs, review fixes | 150-300 | 4-6 + review |
| | | **total** | **~2.6-3.7k** | **~45-62 h** |

The design said ~1.5-2.2k. The difference is the reference and the coverage proof the s128 lessons ask for
(G0, ~0.8k), the F1 audit, the stale-trie handling (F4) and the per-config coverage floor. G1a can go in parallel
with G0; the rest is sequential. Q11 is ~2 h 20 of bench time plus the extra cells (bench agent). Each group passes
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
