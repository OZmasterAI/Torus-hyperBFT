# Design: item 7 step 3, isolate trading from EVM

Status: revision 2 (s127). **Option 0 (P1 + P2) BUILT** on `feat/item7-option0` (plan `item7-option0-impl.md`,
steps 0-4, G0-G4, 2026-10-19), behind `TORUS_EVM_FAST_PATH`; Q11 passed (perf doc section 107) and the owner merged it
with the flag **on by default** (s131; `TORUS_EVM_FAST_PATH=0` turns it off per node). RPC freshness as
question 4 a (`docs/api/eth-head.md`). **T (P3) BUILT** on `feat/item7-t-same-block` behind the
P0.0 feature `same_block_core_writer` (id 2), scheduled nowhere (owner s138: after the testnet genesis, activated
at a height); with the bit off every rule is the step 2 one. T's own order-range cursor rows are
`core_writer_t_first_open_order_target` / `core_writer_t_full_order_run` (the step 2 rows speak for the shared range
of the blocks below the activation). HL order and C0 are not built. Revision 1 (`3b79a617`) went through two
adversarial reviews: Codex (`reviews/codex-item7-step3-adv.md`, needs-attention: P1 + P2) and Claude
(`reviews/item7-step3-claude-review.md`, build with changes: 3 high, 5 medium, 2 weak spots, missing tests). Every
finding is verified in code and handled; section 10 maps each one to the change. Owner direction s127 is in: the RPC
freshness pick no longer checkpoints at each EVM height (Q6 conflict), T comes after option 0 with a new size and
explicit capacity semantics, and option S is measured (Q12, section 6.2) instead of dismissed.
Builds on step 1 (main) and step 2 (`feat/item7-step2` `c567b1b3`; step 2 merged to main in `cfa35ed1`, s127). The block in
`item7-evm-lanes.md` 47-63 (owner s122) frames step 3 as HL style or C, both "after EVM reads / writes go through the
overlay", both after item 6 Phase 5. Anchors are main `dcee5cac` unless marked `@ c567b1b3`; the Q6 design is
`q6-checkpoint-slicing.md` at `0f50b54b` (branch `perf/q6-ckpt-slicing`). "(estimate)" marks numbers not measured;
"inferred" marks code facts not read directly.

**Short answer.** The overlay step alone (option 0) removes the barrier and with it nearly all of the measured cost.
It changes nothing a contract can see, so it needs no fresh genesis and can be switched per node. Revision 2 makes it
cheaper and safer: no checkpoint is forced at EVM heights (EVM state and receipts become visible at the next regular
checkpoint, like native state), every new read failure is a local fault (fail-stop), the precompile journal never
shares R, and the EVM trie moves to the flush worker from per-block hashed inputs. T (same-block EVM -> native) comes
after 0, because it needs 0's journal absorb on both execution paths, and gets its own slot range per target so a
backlog of delayed orders cannot push a deposit out of its block. HL order stays a parity-only choice with no speed
over 0; C0 stays parked. Option S (raise N) is measured by Q12 before anything is built; it is expected to be a
stopgap at best.

## 1. Problem

### Measured (Q5, results doc section 80, 12175-12236; main `e3d11cb9`, 300 markets, N = 11, 600 s, n = 2)
- 10 EVM transfers/s (40 senders) cost **-12.4% native matched/s**: E 211.6k vs N 241.6k, E/N 0.876 (r1 0.870, r2
  0.882; replicate spread 0.2% N, 1.5% E). CPU-s per 1M fills +1.8%.
- Every EVM height runs serially (0% pipelined vs 99% for other blocks). Exec idles **~0.5 s before each EVM block**
  (mean 483 / 508 ms, 12.9 / 13.6% of the window; other blocks 0.2 ms p50).
- EVM heights take 479 / 496 ms of exec vs 294 / 293 for other blocks (+185 / +203 ms). Of that, the EVM section
  (`torus_exec_evm_seconds`: revm, EVM root, batch build, metadata write) is only 34-36 ms; EVM-only blocks 8-10 ms.
- 161 EVM blocks in the window = 0.27 per second, x ~0.69 s = ~18% of exec time; partly won back by bigger native
  blocks (+38% fills per native block). Commit -> exec lag E ~24 s vs N ~10-12.5 s; exec queue peak 66 in all cells.
  The commit interval at EVM heights is not worse (p50 264 / 249 ms), so the loss is on the exec thread (E).

### Mechanism (verified in code)
`execute_committed_block_with` (`app.rs:3003`) pipelines a block only if `!has_evm` (and no slashes, dispatch-durable
header and body, not an epoch boundary unless `TORUS_EPOCH_FAST_PATH`; `app.rs:3163-3173`, `@ c567b1b3` 3391-3397).
Anything else calls `pipeline_barrier` (`app.rs:2509-2550`, `@ c567b1b3` 2737): `checkpoint_drain` closes the
since-checkpoint window L plus the last digested block into a checkpoint (`app.rs:2856-2882`), `wait_idle` waits until
the flush worker W has written it, the published ingress view is dropped, and the block runs the serial path on the
DB. At 300 markets a window is ~100 MB (book blobs dominate) and W writes it in ~0.26-0.51 s (memory 123e4dd2, archy
hsdb-300m ckpt samples), which is the ~0.5 s idle. The serial path then writes the block's state, native trie and
marker on E (`app.rs:4312-4400`, ~0.12 s per block at 300 markets in an older run, memory 123e4dd2) instead of handing
it to W.

**Heights without EVM txs do not barrier.** `has_evm = !torus_block.evm_transactions.is_empty()` (`app.rs:3110`;
`@ c567b1b3` 3338): an `h % N == 0` height that carries no EVM tx pipelines like any other block. A native-only run is
therefore the same at every N (Q12 needs one native arm, not one per N).

The barrier exists because the EVM path reads and writes the DB directly, so the DB must be current:

| # | EVM-path access that bypasses the overlay / layers | anchor |
|---|---|---|
| R1 | revm reads accounts, storage, code, block hashes from `StateDb` (`State::with_database_ref(state_db)`) | `torus-evm/src/executor.rs:282-294`; `DatabaseRef for StateDb` `torus-state/src/db.rs:1474-1503` |
| R2 | precompile journal `NativeStateOverlay::new(state_db)`: own pending -> DB, no parent / L / C. Readers: positions, balances + EVM balance, markets, oracle, staking (`precompiles.rs:607-990`); CoreWriter `next_sequence` scan (1397-1409) | `executor.rs:303-305`, `precompile_provider.rs:26-48` |
| R3 | EVM root over the DB trie (`compute_post_bundle_state_root_with_updates(state_db, ..)`), incremental by default | `validator.rs:198-200`, `state_root.rs:24-29, 37-60, 119-128`, `incremental.rs:240-264` |
| R4 | EVM block batch built against the DB: plain + hashed mirror (wipe scan) + trie nodes + journal rows | `app.rs:3300-3333`, `incremental.rs:322-414` |
| R5 | `core_writer_due` / `CoreWriterQueue::pending_count(&StateDb)`: raw RocksDB prefix iterator | `app.rs:2353-2362`, `native_executor.rs:13520-13522`, `precompiles.rs:1571-1590 @ c567b1b3` |
| R6 | serial-path resync of EVM accounts credited by native (fees, rewards, transfers) | `app.rs:4402-4417`, `incremental.rs:416-490` |
| R7 | step 2 lane rows (`EvmUserModify`) read from `state_db`, comment "an EVM block runs after a barrier, so the DB is current" | `validator.rs:154-180 @ c567b1b3` |
| R8 | step 2 CoreWriter enqueue: delay param, last-order-target / first-open-target / order-run cursor rows (CF_FEE_CONFIG), `get_cf_len` full-target probes, through the journal -> DB; doc "rows are queued only by EVM blocks (serial barriers)" | `precompiles.rs:1397-1530 @ c567b1b3`; due-check doc `native_executor.rs:13596-13609 @ c567b1b3` |
| R9 | mempool prune of executed EVM txs (`notify_block_committed`, `prune_committed_txs`): one raw-DB nonce read per sender | `torus-mempool/src/lib.rs:2120-2195 @ c567b1b3` |
| R10 | RPC `eth_getBalance` / `getCode` / `getStorageAt` / `getTransactionCount(latest)` / `eth_call` and receipts read the DB; the eth head is the applied marker (`eth.rs:173-181`), so the eth view moves at checkpoints, and today an EVM block is visible at once only because its barrier forced a checkpoint | `torus-rpc/src/eth.rs:714-780` |
| W1 | EVM writes ride a `WriteBatch` prefix of the serial flush (bug (c) atomicity), plus hash-only `HashExtras` | `app.rs:3258, 3319-3333, 4337-4345, 4575-4646` |
| W2 | `commit_block_metadata`: header JSON, body, receipts, hash -> number, tx location, in its own direct batch | `app.rs:2377-2387`, `committer.rs:144-197` |
| W3 | CoreWriter tx links appended to the EVM batch (node-local CF) | `app.rs:3319-3327`, `corewriter_tx.rs:68-86` |

What is NOT on the list: the writer precompiles write only `CF_CORE_WRITER_QUEUE` (and, `@ c567b1b3`, the CF_FEE_CONFIG
cursor rows); they never write positions, balances or `CF_ACCOUNTS` (`precompiles.rs:996-1285`; lockbox doc
`lockbox.rs:15-28`; Claude re-checked: `credit_native` is called only from the drain). The native leg of a lockbox
deposit runs in the next block's drain (`native_executor.rs:13650-13655`). Consensus never reads EVM results: the
header copies the parent's `state_root` and carries zero receipts root, bloom and gas used (`app.rs:7403-7406`);
catch-up skips the root check (`validator.rs:101-110`). The running state hash covers only the plain EVM CFs (ids
0x80-0x82), never the trie or hashed mirror (`running_hash.rs:20-26, 35-62`).

**Pre-existing gap (Claude H1, applies to today's path too).** revm's `EVMError::Database` maps to
`EvmError::Internal` (`map_evm_err`, `executor.rs:673-680`; `@ c567b1b3` 733); only a precompile that recorded a fault
in the provider's cell becomes `LocalFault` (`local_fault_or`, `executor.rs:662-671`). The app fail-stops on
`LocalFault` (`app.rs:3352-3360`) but the generic arm only logs and runs the block without its EVM writes
(`app.rs:3361-3363`). So today a RocksDB read error inside revm on ONE node skips that node's EVM part while the others
apply it: a divergence. The generic arm must stay for deterministic errors (a Byzantine proposer's over-gas block),
so the fix is to classify the read errors, not to fail-stop the arm (P1, question 11).

### What must move for an EVM block to pipeline at all
R1, R2, R5, R7, R8 read through the block's layered overlay (own pending -> parent -> L -> C -> DB,
`backend.rs:1366-1458`; the journal without R, point 2 below). W1's plain bundle rows and the journal rows go into the
block's overlay, so the frozen set carries them (digest, L, checkpoint: one atomic batch, bug (c) kept). R3, R4, R6
move to W (the trie is derived, node-local data). W2 (minus header and body) and W3 become side rows of the block's
job, like the action status record (`exec_pipeline.rs:866-880`). R9 reads the published view. R10 follows the eth head
at the next regular checkpoint (question 4). That set is option 0.

## 2. How Hyperliquid does it (official docs, fetched s127)

- **Order within an L1 block that produces an EVM block** ([interaction timings](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/interaction-timings)):
  1 L1 block is built, 2 EVM block is built, 3 EVM -> Core transfers are processed, 4 CoreWriter actions are processed.
  "Transfers from HyperEVM to HyperCore happen in the same L1 block as the HyperEVM block, immediately after the
  HyperEVM block is built." "Transfers from HyperCore to HyperEVM are queued on the L1 until the next HyperEVM block."
  A CoreWriter action's account must exist on HyperCore before the EVM block is built.
- **What contracts read:** read precompiles at 0x0800+ "are guaranteed to match the latest HyperCore state at the
  time the EVM block is constructed", i.e. after that L1 block's trading. Gas `2000 + 65 * (input_len + output_len)`
  ([interacting with HyperCore](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/interacting-with-hypercore)).
- **CoreWriter:** system contract 0x3333..33, burns ~25,000 gas, emits a log processed by HyperCore as an action.
  "Order actions and vault transfers sent from CoreWriter are delayed onchain for a few seconds"; they appear twice
  in the L1 explorer (enqueue, execution). Other actions run in step 4 of the same L1 block.
- **Core -> EVM:** a `sendAsset` to the token's system address; credited by a system transaction in the next HyperEVM
  block, costing 200k gas at that block's base fee
  ([transfers](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/hypercore-less-than-greater-than-hyperevm-transfers)).
- **Blocks:** small 1 s / 3M gas, big 1 min / 30M gas; "the first L1 block for each value of `l1_block_time % x`
  produces an EVM block"; the EVM mempool is onchain state, two pools, next 8 nonces per address
  ([dual-block architecture](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/dual-block-architecture)).
  EVM blocks are "built as part of Hyperliquid's execution"
  ([HyperEVM](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm)); with steps 1-4 in order, the
  EVM block runs between the L1 block and its effects, not beside them (inferred). Not documented: whether
  liquidations precede the EVM block, how long "a few seconds" is, and whether the EVM block built in the same L1 block
  counts as "the next HyperEVM block" for a Core -> EVM transfer sent in that L1 block (unverified).

**Torus today vs HL:**

| | HL | Torus (main + step 2) |
|---|---|---|
| order in a block | trading, EVM, EVM -> Core, CoreWriter | EVM first (`app.rs:3243-3365`), then oracle, native batches, CoreWriter drain, liquidations, governance, fees, epoch (`app.rs:3870-3990`) |
| contracts read | post-trade state of this block | block-start state (= end of the previous block); liquidations visible next block (`app.rs:3954-3956`) |
| EVM -> native (lockbox deposit) | same block, after the EVM block | queued for h + 1, drained after h + 1's batches, so orders see it at h + 2 (`precompiles.rs:1300`, `item7-evm-lanes.md:116-118`) |
| CoreWriter non-order actions | same block, after transfers | h + 1 |
| CoreWriter orders | "a few seconds" | step 2: K = 10 blocks, cap 200 per block |
| native -> EVM | queued to the next EVM block, 200k gas | immediate `CF_ACCOUNTS` credit, free; visible at the next EVM height |
| EVM execution | in order inside L1 execution (inferred) | serial, plus the barrier |

Parity therefore needs: (a) post-trade reads, which only a reorder gives; (b) same-block EVM -> native transfers and
non-order CoreWriter actions, which a queue-target change gives without a reorder (option T); (c) delayed orders (step 2
has them). The wall-clock gap of (a) and (b) is one block each (~0.3 s at ~3.2 blocks/s) against an EVM cadence of
~3.4 s at N = 11.

## 3. Options

Common base for HL, T, C0, C1, D: option 0. S is the no-code alternative; N is the narrower-barrier idea.

### S. Fewer EVM blocks (raise N; governance 2..=100, genesis >= 1): measured by Q12

> **Status s136: closed as superseded (owner).** Option 0 (EVM fast path, merged s131, on by default) removed the
> barrier S worked around: section 107 (8l), under 10 EVM tx/s E-on runs at N-on's pace (matched/s 1.079x E-off,
> native blocks/s 3.53x, p50 0.090x, timeouts 22 -> 3). Q12 r1 ran s68 (section 85); owner s128: no r2/r3, N stays 11.
> Revisit S only if the fast path is turned off or a heavier EVM load brings the barrier back.
- **Mechanism:** none new. Each EVM block forces a checkpoint of the window since the last one, so its barrier wait
  grows with the window: at N = 11 a ~3.8 s window is ~100 MB, written in 0.26-0.51 s; a full 15 s window is
  ~200 MB, flushed in 0.84-0.89 s (Q6 section 1.1). The serial run adds ~0.19 s per EVM block whatever N is. So fewer
  EVM blocks save less than in proportion.
- **Estimate (low confidence, Q12 measures it):** cost per EVM block ~0.69 s at N = 11, ~0.75 s at N = 22, ~0.86 s at
  N = 33; EVM blocks 0.27 / ~0.15 / ~0.10 per s; exec share 18% / ~11% / ~9%; with Q5's conversion (12.4% of matched/s
  per 18% of exec) E/N 0.876 / ~0.92 / ~0.94. The EVM cadence goes ~3.8 / ~6.5 / ~9.5 s, so inclusion latency scales
  about with N / 11.
- **Semantics:** none (N is a chain parameter; contracts see a slower cadence). **Size 0** (plus a harness knob for the
  bench). Not a fix: the loss stays and grows with EVM load. Revision 1 called it "not a fix" without numbers (Claude
  weak spot 1); Q12 (section 6.2) replaces that with a measurement and a decision rule (question 10).

### N. A narrower barrier (considered; not simpler, Claude weak spot 2)
Keep the EVM path on the DB and make the barrier cheaper. It does not work out:
- The EVM path does not read only the EVM CFs. Its precompiles read positions, balances, markets, oracle, staking,
  queue and fee rows (R2, R8). "Make the DB current for what EVM reads" is therefore nearly the whole window
  (positions alone are ~92% of a block's state keys, `checkpoint_layer.rs:26-31`).
- Writing only part of L would leave the DB at two heights at once. Boot replay needs the DB at exactly one checkpoint
  height (Q6 section 3), so that would need a marker per CF: a crash-model change bigger than option 0.
- Waiting only for the in-flight checkpoint C does not make the DB current (L stays in memory).
So a narrower barrier is either the same write or a new durability model. Option 0 removes the need for one.

### 0. EVM blocks on the pipelined path, same order (recommended base)
- **Mechanism.** Behind a node-local flag `TORUS_EVM_FAST_PATH` (default off until Q11, as `TORUS_EPOCH_FAST_PATH`; on since s131,
  `exec_pipeline.rs:57-66`), drop `!has_evm` from the pipelined predicate (`app.rs:3163-3173`), then:
  1. **Overlay first.** Build the block's layered overlay (parent, L, deferred part, C: `checkpoint_read_layers`,
     `app.rs:2643`) before the EVM section (today it is built after it, `app.rs:3415-3448`). The EVM section stays the
     first thing in the block, before the due-checks, verify and the native phase.
  2. **Reads, without R** (Claude H3). revm reads through a new `DatabaseRef` adapter over that overlay (CF_ACCOUNTS /
     CF_STORAGE / CF_CODE via `get_cf_raw`; block hashes from CF_BLOCK_HEADERS, dispatch-durable). The precompile
     journal is a second `NativeStateOverlay` over the SAME layers (`with_split_layers(db, parent, L, deferred, C)`):
     own pending -> parent -> L -> C -> DB. It never attaches the resident rows R. Revision 1 attached R, which fails
     twice. First, `end_resident` takes R back with `Arc::get_mut`; a live journal clone (precompiles, inspector,
     `ValidatedBlock.native_writes`) makes that fail, the slot stays empty and the next block rebuilds R
     (`native_executor.rs:4568-4590`, `rows_shared_fallbacks` 4167-4171). Second, R is taken by `begin_resident`
     inside the native phase (`app.rs:3747`). That phase starts only after the EVM part, because `run_native` depends
     on the EVM fee revenue (`app.rs:3476-3480`). Taking R before the EVM part would expose the previous block's
     `end_resident` worker (~100 ms CPU per block at 300 markets, results 15.1; ~92% hidden today, results 18) at every
     EVM height. Without R, a precompile read of positions / balances that misses the parent reaches L's deferred part
     and may wait for block h-2's merge (p3 merge wait, `checkpoint_layer.rs:26-41`). That is at most one wait per EVM
     block, typically about ~20 ms (results 54: the whole merge was 23.4 ms per block before the split; the end-of-block
     join is 0.19 ms). Plain transfers call no precompile, so they never wait. Counted by
     `torus_exec_checkpoint_deferred_read_waits`.
  3. **Read failures are local faults** (Claude H1). The adapter writes every read error into the provider's
     local-fault cell (`TorusPrecompiles::local_fault`, `precompile_provider.rs:83`) before returning it. So revm's
     `EVMError::Database` becomes `EvmError::LocalFault` in `local_fault_or`, and the app fail-stops (`app.rs:3352`)
     instead of running the block without its EVM writes. The same holds for the journal's layer errors (a failed
     deferred merge, `backend.rs:1430-1440`) and for the absorb in point 5 (a frozen-overlay put). P1 adds the same
     wrapper on today's `StateDb` path (section 1).
  4. **No raw reads, enforced at run time** (Claude H2). A signature change alone cannot enforce this. `StateDb`
     implements `StateBackend` (`backend.rs:222`), so `&impl StateBackend` still accepts the DB, and
     `NativeStateOverlay::db()` (`backend.rs:1724`) hands it out. So the design has three guards:
     (a) The fast path builds one `EvmBlockState` (the layered overlay plus the journal). The EVM entry points
     (`EvmExecutor::execute_block[_lanes]`, `validate_block_inner`, `CoreWriterQueue::pending_count` /
     `next_sequence`, `core_writer_due`, the step 2 lane read) take it or `&impl StateBackend`, never `&StateDb`.
     (b) A tripwire in tests and debug builds. While the fast-path EVM section runs, a thread-local guard makes every
     `StateDb` read entry point panic (`get_cf_raw`, `get_cf_len`, the iterators, `prefix_exists`, `DatabaseRef`,
     `inner()`). The only exemption is the overlay's own bottom-layer read: it holds a token around its DB call
     (`backend.rs:2460, 2576, 2632, 2684`).
     (c) The poison tests (test 3) check the behaviour.
     `append_links(&StateDb)` (`corewriter_tx.rs:68`) only uses the DB for a CF handle to write a batch. It goes away
     with the side rows (point 7).
  5. **Absorb on both paths** (Codex P1). After the EVM part, two sets of rows go into the block overlay: the bundle's
     plain change set (exactly `apply_bundle_plain`'s puts / deletes incl. CF_CODE, `incremental.rs:270-311`, which
     reads nothing) and the journal's pending rows (queue rows, cursor rows). `seed_from_bundle` grows from
     CF_ACCOUNTS only (`backend.rs:1702-1721`) to the three EVM CFs plus the journal.
     - The journal is consumed by value and dropped with every clone before the native phase. Otherwise L, its deferred
       part or C would still be shared at `take_layer` (`app.rs:220-252`). A shared L is deep-copied (counted by
       `torus_exec_checkpoint_layer_shared`), and a shared deferred part is copied after a join. This hazard is the same
       class as H3; the reviews did not name it.
     - A block that absorbed rows always freezes (pipelined) or flushes (serial) its overlay, whether or not the native
       phase runs. Today an EVM block without a native phase writes a marker-only set (`app.rs:4575-4640`).
     - The serial path does the same absorb (P1). The rows then ride the overlay's one flush batch instead of its
       `evm_batch` prefix: the same bytes in the same batch, so bug (c) atomicity holds.
     - EVM `HashExtras` are no longer needed (Claude checked: they rank below pending).
     This absorb is what T needs later.
  6. **EVM trie on W** (Claude M6, question 5). E computes no EVM root. The final plain rows of a window are not enough
     to rebuild the hashed mirror. `apply_bundle_hashed` removes an account that ends EIP-161-empty or deleted, and
     wipes ALL of its hashed storage with a DB scan (`incremental.rs:322-367`). Plain CF_STORAGE keeps those slots,
     and the wipe depends on the order of blocks. The design therefore works like this:
     - Each EVM block's Digest / Flush job carries its hashed input: for every touched account its final info and
       `apply_bundle_hashed`'s keep flag, plus the changed slots of kept accounts.
     - W folds the inputs in height order into the window's delta. A wipe drops that account's earlier window slots
       and keeps the wipe.
     - W adds the window's native-credit accounts (`evm_addrs`, final CF_ACCOUNTS value, account-only as in
       `resync_evm_accounts`, `incremental.rs:416-490`).
     - At the checkpoint (or the per-block Flush), W builds the mirror ops (first the wipe scans against the DB at S,
       then the window's puts / deletes) and the `TrieUpdates` over DB cursors plus the sorted delta. All of it goes
       in the checkpoint batch before the marker. CF_HASHED_* / CF_TRIE_* then advance atomically with CF_ACCOUNTS,
       and the post-checkpoint `resync_evm` (`exec_pipeline.rs:948`) has nothing left to do for pipelined windows.
     - If the EVM trie step fails, the checkpoint lands without it and the EVM trie is marked stale (a node-local key),
       to be rebuilt in full at boot (`ensure_trie_built`'s build, `incremental.rs:124-137`). Nothing in consensus
       reads it (section 1).
  7. **Side rows** (Claude M7). Receipts, block hash -> number, tx location and CoreWriter tx links become side rows of
     the block's Digest / Flush job, written in the checkpoint batch like the action status record
     (`exec_pipeline.rs:866-880`). Header and body are NOT side rows, although `commit_block_metadata` also writes them
     (`committer.rs:151-171`). The pipelined path already requires them to be dispatch-durable (`durable.header &&
     durable.body`), and the consensus thread writes them (`app.rs:1582-1640`). A second writer of those keys would
     break Q6 A1's key disjointness. The side-row keys are heights > S (receipts, links) or unique hashes. The pruner
     (cutoff <= marker, `pruner.rs:337`) and the consensus thread never write those keys in the window. An EVM-only
     block hands off a Digest / Flush job, not a Marker.
  8. **Other read paths.**
     - `core_writer_due` reads through the block overlay after the absorb (like `oracle_due`,
       `native_executor.rs:13524-13531`; R5).
     - The step 2 lane read and the enqueue reads (cursor rows, `get_cf_len`, `next_sequence`) go through the journal
       (R7, R8).
     - Mempool prune (both functions, R9) reads nonces from the ingress view (CF_ACCOUNTS is an ingress CF,
       `read_view.rs:58-67`). It passes `executed_height` = that view's height, read from the same view handle before
       the nonces (Claude M8). Step 2 defines `executed_height` as "the applied marker of the DB the nonces are read
       from" (`lib.rs:2152-2158 @ c567b1b3`), and the view answers every ingress key as of >= its height
       (`read_view.rs:20-26`).
  9. **RPC freshness** (owner s127; revision 1's checkpoint at each EVM height is dropped, question 4). No checkpoint is
     forced at an EVM height. The EVM rows, receipts and indices land in the next regular checkpoint (15 s interval,
     h % 100, byte cap; `app.rs:2767-2773`), in the same batch as the marker. The eth head is the applied marker
     (`eth.rs:173-181`), so EVM state and receipts become visible together at that checkpoint. That is up to one
     interval of lag, as for native state since Phase 3 step 2, and a receipt is never visible before its state.
     `eth_getTransactionCount(pending)` and the mempool already read the ingress view (`lib.rs:2101-2114 @ c567b1b3`),
     so senders get fresh nonces. Fresher reads are question 4 b.
- **Determinism.** It is the same argument as for every pipelined native block. Own pending -> parent -> L -> C -> DB
  answers every key as of post-(h-1) on every node, whatever each node's checkpoint timing; the serial path answers the
  same from the drained DB. The new risks:
  - A read that bypasses the layers answers from the DB at S on one node and from a layer on another: divergence.
    Point 4's tripwire and test 3 guard against it.
  - A read error on one node. Point 3 turns it into a fail-stop, never a skipped EVM part.
  Tests compare serial, flag off and flag on under every checkpoint policy (section 5).
- **Crash / replay.** The EVM rows ride the block's frozen set, so the block is durable iff its marker is (bug (c)
  unchanged). The EVM trie and the native-credit resync are in the same batch. Replay has no W and stays serial
  (`app.rs:3155-3156`), which must give the same state (tested).
- **EVM-visible semantics.** None. Contracts read block-start state, native sees the EVM results, and queue targets and
  CoreWriter timing are unchanged. h_n and every consensus row are byte-identical, so validators can mix flag on and
  off. **No genesis implication; can ship after testnet.**
- **Effect (estimate).** It removes the ~0.5 s idle and the serial flush per EVM block. It adds no checkpoint. Revision
  1's point 8 would have added them: Claude M4 put it at ~4x the N arm's checkpoints and ~2x its bytes, 54-131 MB per
  3.4 s vs ~200 MB per 15 s. E keeps revm (~20-30 ms per Q5 EVM block once the root and the metadata write leave E) and
  the absorb (~59 accounts). W adds the EVM trie step (~240 accounts per 15 s window at Q5's load, ms).
  - At Q5's load: E/N ~0.97-0.99 instead of 0.876. The estimate is unchanged, but it now holds without the omitted
    checkpoint cost. The closed-loop bench may not convert exec time 1:1 (section 54).
  - A full small lane (3M gas at 7-28 ns/gas: read-precompile bench `read-precompile-gas.md:111-118`; Q5 transfers
    <= ~28 ns/gas incl. root): 21-84 ms per EVM height, 0.6-2.5% of exec at N = 11, plus at most one deferred-part
    wait (~20 ms) when reader precompiles run.
  - A step 2 big block of 10-15M: 70-420 ms on E once per M heights (one slow block, a p99 item), no barrier.
- **CPU:** E idles less. W writes the same checkpoints as the N arm plus a small trie step. RAM: EVM rows in the frozen
  sets (~59 accounts x 72 B per Q5 block), negligible.
- **Size M-L** (estimate ~1.5-2.2k lines incl. tests; revision 1 said ~1-1.5k). Added since then: the fault wrapper, the
  tripwire, the hashed-input job, the journal lifetime, the view prune and the absorb on both paths. Of that, P1 is
  ~400-700 lines. **Risk M** (run-time-enforced reads, the s124 epoch fast path as the template, differential tests).
- **Phase 5:** not needed (section 4.3).

### HL. 0 + HL order (trading first, then EVM, then EVM -> native and CoreWriter)
- **Mechanism.** In an EVM block: oracle, native batches, CoreWriter drain of earlier rows, liquidations (the "L1
  block"), then the EVM part over the block overlay (post-trade reads), then this block's lockbox deposits and
  non-order CoreWriter rows (targets moved to h, drained after the EVM part), then governance, fees (EVM revenue),
  epoch. Non-EVM blocks unchanged. The journal must read the native overlay's current pending (a chained child of
  it, not a frozen layer) and, after Phase 5, typed state from `Users` mid-block.
- **Determinism / crash:** sequential, one frozen set, as 0.
- **EVM-visible:** yes. Contracts read this block's fills, marks and liquidations; deposits and non-order actions land
  in the same block (orders use a deposit one block sooner); a `TransferToPerp` in an EVM block reads the EVM balance
  before that block's EVM txs (today after). Fresh genesis before testnet, or P0.0 activation after.
- **Effect:** as 0 (same work on E, only reordered). Forecloses C0 for good: EVM over post-trade state cannot run
  beside the trades it reads (only D, below, could overlap it).
- **Size L** on top of 0 (reorder, chained journal, second drain point, many EVM-first tests rewritten, docs and
  explorer). **Risk M.**

### T. Same-block EVM -> native for non-order kinds, EVM-first kept (after 0)
- **Why after 0** (Codex P1, Claude M5; revision 1 said T "does not need 0"). Today:
  - the journal rows go only into the `evm_batch` prefix and the hash extras (`app.rs:3300-3333`);
  - `seed_from_bundle` copies only CF_ACCOUNTS (`backend.rs:1702`);
  - `core_writer_due` reads the raw DB (`app.rs:2353-2362`).
  Retargeting a row to h therefore hides it from h's due-check and drain, and then writes it at target h after h has
  passed. The deposit's EVM value is burned and its credit is stranded. Codex's probe on today's library shows it:
  `journal_rows=1, native_drain_rows=0`, `target7_rows_after_flush=1`. T needs option 0's absorb (point 5) and its
  overlay due-check after the absorb (point 8) on BOTH paths (serial, flag off, replay, barrier blocks). P1 does them
  for the serial path and P2 for the fast path, so T comes after P2.
- **Mechanism.**
  - **Kinds.** The non-order kinds (`LockboxDeposit`, `LockboxWithdraw`, `Delegate`, `Undelegate`, `ClaimRewards`,
    `LockPermanent`, `ClaimUnbonded`; not `is_order`, `precompiles.rs:1726-1734 @ c567b1b3`) target the queuing block
    h. Order kinds keep step 2's `max(h + K, last order target)`.
  - **Capacity** (Codex P2, question 8). Today both classes share one per-target cap of 200
    (`CORE_WRITER_DRAIN_CAP`, `torus-types/src/lib.rs:781 @ c567b1b3`) and one spill walk
    (`precompiles.rs:1423-1470 @ c567b1b3`). Delayed orders that fill h would push a same-block deposit to h + 1 or
    later; with K = 10 and N = 11, one target of spill already lands order rows on the next EVM height. T therefore
    gives each target two seq ranges in the same key layout (target 8 BE || seq 8 BE): seqs 0..199 for the same-block
    class, 200..399 for the order class.
    - Each range has its own cap of 200, its own dense seqs and a range-aware `next_sequence`.
    - The order class keeps step 2's walk, cursor and run rows. The only change is the full test ("full iff seq 399
      taken"). Its invariant `candidate >= next_block` (`precompiles.rs:1438-1440 @ c567b1b3`) still holds, because the class now
      holds orders only.
    - The same-block walk starts at h, probes seq 199 and spills to h + 1, h + 2, ... in its own range only. It needs
      no cursor row. Only EVM block h itself and the spill of a recent EVM block fill these ranges, and gas bounds both:
      at `GAS_PRECOMPILE_WRITE` = 20,000 (`precompiles.rs:57 @ c567b1b3`), one block holds at most ~150 enqueues per 3M
      gas, which never fills the 200-row range, and a 15M big block holds ~750. So a walk probes at most ~4 targets per
      enqueue. That keeps clear of the unmetered O(backlog) probes Codex flagged in step 2's walk (memory
      ad5a8203c6de102f, fixed there by the cursor rows).
    - A target drains in key order: the same-block range first (HL: EVM -> Core transfers before CoreWriter actions),
      then orders. Per-block drain stays <= 400 rows.
  - **Drain point and assertion.** h's drain (after its native batches, as today) runs h's same-block rows. The anti-MEV
    assertion "queued in a strictly earlier block" (`native_executor.rs:13535-13536`; `@ c567b1b3` 13624) becomes per
    range: order range `queued < drain height`, same-block range `queued <= drain height`.
  - **Due.** `core_writer_due` is evaluated after the absorb, so an EVM block whose txs queued rows for h runs the
    native phase even if it earned no fee (a zero base fee). Otherwise `computed_fee_revenue > 0` already forces it,
    `app.rs:3476-3480`.
  - **Tombstones.** A same-block row is put (absorb) and deleted (drain) in one overlay. The flush issues the delete,
    D(h) carries the tombstone for the queue key, and the row never reaches the DB. This is identical on both paths
    and on replay (tested). Its CoreWriter tx link and v4 status are keyed (h, seq), as today's (drain height, seq).
- **The h + 1 anti-frontrunning rule: kept for orders, replaced for the other kinds** (question 9). The rule keeps
  EVM-originated actions from acting inside a block ahead of native order flow. HL's reason for delaying CoreWriter
  orders is that contracts "cannot bypass the L1 mempool" (`item7-evm-lanes.md:62-63`); the parity report says the
  action is "invisible until N+1" (`docs/reports/hl-parity-2026-10-01/findings.md:8289`).
  - Orders keep K >= 1 (step 2: K = 10).
  - A same-block row runs after every native batch of its block, and the contract that queued it read block-start
    state (EVM-first). So it can neither see nor precede any native action of its block. It can only move value
    (deposit, withdraw) or staking state one block sooner.
  - That is HL's own rule for transfers ("same L1 block, immediately after the HyperEVM block").
  So for these kinds the property the rule protects holds without h + 1. Keeping h + 1 for every kind would simply
  mean no T.
- **EVM-visible: yes (timing only).**
  - Native orders see an EVM deposit at h + 1 (today h + 2).
  - A withdraw debits native at h (today h + 1); its EVM credit is visible at the next EVM height either way for
    N >= 2.
  - Staking one block sooner.
  - A deposit is burned and credited in the same block, so no value is in flight across blocks.
  Contracts keep block-start reads. Genesis-bound (fresh genesis before testnet, or P0.0 activation after).
- **Effect:** ~0 on throughput.
- **Size M** (estimate ~600-1,000 lines incl. tests, on top of P1 / P2). Revision 1 said S-M. The work: kind-aware
  targets, the two seq ranges with per-range full test and drain order, the assertion, the due-check after the absorb,
  `docs/api/corewriter-results.md` (seq numbering), and the rewritten h + 1 tests (`torus-evm/tests/evm_tests.rs:1223`,
  `torus-core/tests/precompile_tests.rs:525`, the `corewriter_evm_row_drains_in_empty_next_block` family,
  `app.rs:23846, 23935`). **Risk M** (genesis-bound semantics, cursor invariants). Keeps C0 possible.

### C0. 0 + EVM on its own thread, semantics kept (parked)
- **Mechanism.** At an EVM height E starts the EVM part on a helper thread X. X reads an immutable block-start view
  through the same layers as point 2 (parent, L, C, DB; never R or `Users`, which E mutates). Meanwhile E runs the
  oracle step and the native batches.
  - Join rule, a pure function of the sorted actions: E joins X before the first action that touches an EVM-domain row,
    and before the block tail (drain, liquidations, fees need X's revenue, F3 its gas). EVM-domain rows are category
    Lockbox (`TransferToPerp` / `TransferToSpot` / `Withdraw`) and Staking claims that credit EVM (`classify_action`,
    `native_executor.rs:13939-13968`).
  - All trading categories (0-2) sort before Lockbox (3), so the join normally comes after the trading
    (`native_executor.rs:13927-13936`).
  - At the join X's rows enter the overlay first (as in today's EVM-first order).
- **Determinism.** C0 equals sequential "EVM then native" iff E's pre-join reads miss X's write set. That write set is
  the EVM CFs, the queue rows (>= h + 1, or h's same-block range under T) and the step 2 cursor rows. Proving it needs
  an audit of every pre-join reader at key level (fee rates share CF_FEE_CONFIG with the cursor rows) and a debug-build
  read-set / write-set check. The join point is the same on every node.
- **Crash / replay:** nothing durable before the hand-off; replay serial. **EVM-visible: none; no genesis.**
- **Effect (estimate):** it takes 0's residual EVM time off E (0.6-2.5% at a full small lane, 2-8% at an HL-like 1 s
  cadence, or a big block's 70-420 ms) if a core is free. Benches run 3 validators on one host, whose engine already
  uses worker threads, so the core is not free there. Costs +1 core during EVM heights.
- **Size L-XL** on top of 0. **Risk H** (concurrency inside deterministic execution).

### C1. 0 + EVM thread, native EVM-account effects deferred (rejected)
- As C0 without the join: native writes to EVM accounts in an EVM block take effect next block (the old C text,
  `item7-evm-lanes.md:55-58`). Simpler join, but EVM-visible (genesis-bound) and no faster than C0.

### D. HL order + EVM of block h beside native of h + 1 (rejected)
- Post-trade reads like HL, EVM off E like C0, but EVM -> native effects slip to h + 2 (worse than HL and than today)
  and h's hand-off waits for X. Two visible changes, two parents per overlay. Size XL+, risk H.

### Summary

| option | EVM-visible / genesis | matched/s at Q5 load (est.) | big block 10-15M on E (est.) | size | risk |
|---|---|---|---|---|---|
| today | - | E/N 0.876 (measured) | barrier ~0.5 s + 70-420 ms | - | - |
| S (N 22 / 33) | no (cadence only) | E/N ~0.92 / ~0.94, latency x2 / x3; Q12 measures | barrier + EVM | 0 | L |
| N narrower barrier | no | - (same write, or a new crash model) | - | - | rejected |
| **0** | **no / none** | **E/N ~0.97-0.99**, no extra checkpoints | 70-420 ms | **M-L** | **M** |
| HL (on 0) | yes / genesis | as 0 | as 0 | +L | M |
| **T (after 0)** | **yes (timing) / genesis** | as 0 | as 0 | **+M** | **M** |
| C0 (on 0) | no / none | ~0.99-1.0, +1 core | ~0 | +L-XL | H |
| C1 (on 0) | yes / genesis | as C0 | ~0 | +L | M-H |
| D | yes (twice) / genesis | as C0 | ~0 | XL+ | H |

## 4. Recommendation

**Measure S first (Q12, no node code). Build 0 (P1, then P2). Add T after P2, before the testnet genesis. Park C0
with a measured trigger. Do not take HL order.**

1. Option 0 removes the whole measured mechanism (the barrier and the serial flush). It changes nothing a contract
   can see, so it carries no genesis deadline, can be A/B'd per node on one genesis with Q5's two arms, and can be
   rolled back by a flag. Revision 2 adds no checkpoint traffic, so it does not compete with Q6 for the write queue
   (section 4.4).
2. HL order buys only post-trade reads (one block of staleness, ~0.3 s, against a ~3.4 s EVM cadence) for an L-size
   change, more test churn, a Phase 5 read hook into `Users`, and the permanent loss of C0. T delivers the other
   parity point (same-block EVM -> native transfers and non-order CoreWriter actions) for M and keeps C0 open.
3. T comes after 0 because it needs 0's journal absorb on both paths; doing it first would mean building that absorb
   twice. Its slot-range split makes "same block" hold under any order backlog.
4. After 0 the EVM run time left on E is ~1% at Q5's load and 0.6-2.5% at a full 3M lane (estimate). C0's risk is not
   worth that now. It becomes worth it if budgets grow (big blocks on, N lowered, or HL's 1 s cadence: 2-8% of exec),
   and since C0 is semantics-neutral it can come then without a genesis.
5. Q12 tells whether raising N is a usable no-code stopgap while 0 waits for its slot. It is not expected to replace 0
   (estimate E/N ~0.92-0.94 at 2-3x the latency).

### 4.1 Phased plan
- **P0 (now):** owner answers section 7. Q12 runs on binary M with a ~15-line harness knob (section 6.2).
- **P1 (any time, behaviour-neutral, ~400-700 lines incl. tests):**
  (a) The local-fault wrapper on today's `StateDb` `DatabaseRef`: a read error inside revm fail-stops instead of
  running the block without its EVM writes (question 11).
  (b) The journal absorb into the block overlay on the serial path, and `core_writer_due` through the overlay after
  it. Rows still target >= h + 1, so nothing visible changes (same bytes, same batch).
  (c) R5, R7, R8 read through the overlay / journal (identical under the barrier, where the layers are empty). R9
  reads the ingress view, with the view height as `executed_height`.
  (d) The EVM entry points stop taking `&StateDb`; the tripwire; and the serial-vs-pipelined harness with "flag
  absent: EVM heights stay serial" (the pattern of `epoch_fast_path_evm_or_slash_boundary_stays_serial`,
  `app.rs:27347`).
  P1 removes the barrier assumptions step 2 added.
- **P2 (option 0, behind the flag):** points 1-9 of option 0, tests first; Codex torus-adversarial review; Q11
  (section 6.1); flag default on after Q11. Placement per question 2.
- **P3 (T, after P2, before the testnet genesis):** section 3 T; Codex torus-adversarial review.
- **P4 (parked):** C0, trigger in question 6. HL order only if the owner overrides question 3; it would then replace T
  and come before testnet.

### 4.2 Why 0 is not "both in sequence"
The s122 rule "not both in sequence (contract behaviour would change twice)" holds for HL and C1. Option 0 and C0
change no contract-visible behaviour, so 0 + T (one visible change, at genesis) + later C0 changes behaviour once.

### 4.3 Item 6 Phase 5: what step 3 needs, what Phase 5 must not foreclose
**Needs from Phase 5: nothing.** Option 0 and T work on today's overlay. The shared code is the EVM section of
`execute_committed_block_with`, `freeze` / `FrozenPending`, and Phase 5 Task 2.3 (`item6-phase5-impl.md:240-249` at
`dcee5cac`; on main `b72f9d84` the task carries the s127 note "pending owner", 292-295). Building 0 after Phase 5
step 2 avoids parallel edits there. Building it before step 1 means Phase 5 designs its EVM fold against the final EVM
path (question 2).

**Finding for the Phase 5 plan.** Row 5 and Task 2.3 (`item6-phase5-impl.md:113, 240-249`) assume EVM tx scopes write
`CF_NATIVE_BALANCES`: a lockbox credit inside a reverted frame, or an EVM tx crediting a balance that a native action
then trades on. No such write exists. The writer precompiles only enqueue (`precompiles.rs:1225-1285`, 1299-1314), and
`credit_native` runs in the next block's native drain, a cold writer covered by decision 3 a (Claude re-verified:
`precompiles.rs:1469-1511 @ c567b1b3`, `native_executor.rs:13743`). So decision 4 covers an empty write set. Task 2.3
can shrink to a guard test ("no typed-CF write inside an EVM tx scope") plus the h + 1 drain write-through test
(question 7). Under T the drain write-through runs at h.

**Phase 5 must not foreclose:**
1. Typed rows keep being serialized into `FrozenPending` at freeze (Phase 5 design: "same logical rows",
   `item6-phase5-impl.md:30-33`), so L, C and the ingress view hold positions and balances. Option 0's precompile
   journal and C0's thread X read them there; neither may read `Users`, which E mutates.
2. (Revised.) The journal attaches neither R nor `Users` (point 2), so Phase 5 need NOT keep them attachable to a second
   overlay; revision 1's item 2 is obsolete. Phase 5's gate "deferred read waits stay 0" (`item6-phase5-impl.md:69` on
   main) should count native-phase waits. EVM-height precompile waits should be reported apart: <= 1 per EVM height
   that reads positions / balances, 0 under transfer load.
3. No new raw-DB read on the EVM path justified by "EVM blocks are barriers" (as step 2's R7, R8 and due-check doc).
4. If the EVM fold of Task 2.3 is kept, it must not be tied to "before the native batch" or to an empty parent layer:
   on 0 the EVM part runs pipelined; under HL order it runs after the batch.
5. For HL order only: a mid-block read path from a `StateBackend` into `Users` (the read side of decision 3's
   `ResidentSource` write hook).
6. `phase5_differential_long_random` (`item6-phase5-impl.md:203-211`) takes the EVM fast-path flag as a dimension
   once P2 exists.

### 4.4 Q6 (checkpoint slicing, `0f50b54b`): how option 0 interacts
1. **No extra checkpoints** (point 9). The checkpoint count and bytes per minute stay at the N arm's, so Q6's in-window
   write-queue cost and A1's per-checkpoint fence fsync and redo write are not multiplied by EVM heights. Q11 checks
   this (pass 5).
2. **Key disjointness** (A1 "Key disjointness"). The side rows carry no header or body (point 7). Their keys are
   heights > S or unique hashes, which the consensus thread and the pruner never write in the window.
3. **Trie rows in pipelined windows.** A1.2 routes "any trie-CF row (expected none: pipelined blocks are native-only,
   EVM blocks are serial)" to the tail. Option 0 makes that untrue. The window now holds CF_ACCOUNTS / CF_STORAGE /
   CF_CODE rows (body slices) and the EVM CF_HASHED_* / CF_TRIE_* ops of point 6, which A1's sink must classify as
   trie-CF rows (tail). The tail grows by the window's EVM trie ops (estimate tens of KB at Q5's load).
4. **EVM resync.** For pipelined windows the native-credit resync is inside the batch (point 6), so A1's redo EVM-address
   list and its stale-redo resync are no-ops there. Q6 owner Q12's gap (resync outside sliced checkpoints) closes for
   those windows; serial per-block flushes keep it.
5. **Barrier cost.** A1 adds +0.2-0.5 s to every barrier block (Q6 section 4 A1 expected effect). Option 0 takes EVM
   heights out of the barrier set, so EVM load stops paying it.
6. **Mixed reads.** With slicing on, DB-direct eth reads (question 4 a) can see accounts at T and storage at S for ~1 s
   per window. Q6's owner Q4 accepted that for validator RPC; the eth head (the marker) moves only with Bn.
7. **Build order.** A1 and P2 both touch the checkpoint batch builder (`flush_pending_after_batch`); whichever lands
   second rebases. No design decision is shared.

## 5. Test plan (tests first; each fails before its code)

Template: `epoch_fast_path_boundaries_pipelined_and_match_serial` / `fast_path_run` (`app.rs:27150, 27259`), the crash
tests `exec_pipeline_crash_before_write_replays_both_and_matches_serial` (17898) and
`epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` (27389).

1. **Serial == flag off == flag on.** One random block stream with EVM heights:
   - EVM content: transfers, a deploy, storage writes and deletes, reverts, CoreWriter / staking / lockbox calls,
     `TransferToPerp` / `TransferToSpot` in the same block as EVM txs, an EVM-only block, a failed EVM section.
   - Native credits (fees, rewards) to EVM accounts in the same window.
   - An account destroyed or EIP-161-emptied and re-created in the same window (point 6's keep / wipe order).
   - An EVM height at h % 100, and one at an epoch boundary (both flags on: pipelined; epoch flag off: serial).
   Dimensions: checkpoint policies per-block, never, every 5, by interval; `TORUS_CKPT_DEFER_BOOKS` {0, 1};
   `TORUS_EPOCH_FAST_PATH` {0, 1}. Checked per block: all CF dumps (consensus CFs, receipts, tx index, hash -> number,
   CoreWriter tx links, action status), `h_n`, markers. Checked after each checkpoint: CF_HASHED_* byte-equal to the
   serial run, CF_TRIE_* equal, incremental root == full-scan root. Flag absent: EVM heights serial; flag on: all
   pipelined except the slash / boundary barriers.
2. **Mixed replicas.** Three replicas (flag off; on with 15 s interval; on with per-block flush) agree on every `h_n`
   and state-hash attestation. A 3-node devnet cell under EVM load does the same (Q11).
3. **No raw reads** (Claude H2).
   (a) The tripwire, armed over test 1's whole stream, gets 0 hits.
   (b) Poison: a stale DB value for a key held only in the parent layer, only in L (main and deferred part), and only in
   C. Each R1-R9 reader returns the layer value.
   (c) Queue rows already drained in a layer (tombstoned) but still in the DB: `next_sequence`, the `get_cf_len` full
   probe and `pending_count` / `core_writer_due` see them gone.
4. **Local faults fail-stop** (Claude H1). Faults injected: an adapter read error, a failed deferred merge under the
   journal, a failed absorb put. Each fail-stops before any hand-off, the marker stays below h, and the restart replays
   to the uncrashed state. On today's path (P1), a `StateDb` read error inside revm fail-stops; it skips the EVM part
   today (RED first).
5. **R and L never shared** (Claude H3 and point 5). Over test 1's stream with the flag on: `rows_shared_fallbacks` 0,
   `rows_builds` 1, `torus_exec_checkpoint_layer_shared` 0. A reader-precompile call at an EVM height counts at most
   one deferred read wait and returns the serial value.
6. **SIGKILL replay at every phase,** single and repeated: after the EVM part before the absorb; after the absorb
   before the hand-off; after the hand-off before the checkpoint; during W's EVM trie step; during the replay of an EVM
   block. Restart replays serially; dumps (EVM trie included) and `h_n` equal the uncrashed serial run.
7. **EVM revert semantics on the fast path:**
   - a tx queues `placeOrder`, then reverts (no row, no seq gap);
   - a caught inner-frame lockbox deposit (no burn, no row; sibling rows kept);
   - a reader OOG;
   - a local fault in a precompile read that reaches L (fail-stop, nothing handed off; R02 branch 5 pattern).
8. **Transfers both ways:**
   - `TransferToSpot` at a pipelined native block, spent by the next EVM height (read from L);
   - lockbox deposit at an EVM height, drained by an EMPTY next block (extend
     `corewriter_evm_row_drains_in_empty_next_block` [`_with_interval`], `app.rs:23846, 23935`, to the fast path);
   - `TransferToPerp` after an EVM tx changed the balance in the same block;
   - `withdrawFromNative` credit seen at the next EVM height.
9. **CoreWriter order (step 2):**
   - K-delayed rows whose target falls after a checkpoint;
   - the 200 cap with earlier targets' rows and the cursor rows still in L;
   - dense seqs;
   - v4 status keyed by (drain height, seq) and tx links equal serial
     (`core_writer_tx_links_each_queued_row_to_its_evm_tx`, `app.rs:20688`).
10. **Step 2 rows:** lane rows set in a pipelined block are read by the next EVM height; F3 epoch gas is equal serial
    and pipelined (`f3_epoch_gas_sets_plan_fees_serial_and_pipelined`, `app.rs:21353 @ c567b1b3`); a big and a small
    lane at one height; BLOCKHASH of recent heights.
11. **RPC and mempool** (point 9, Claude M8):
    - eth head, receipts and EVM state move together at the checkpoint; a receipt is never visible before its state;
    - `getTransactionCount(latest)` lags to the head, while `pending` follows the view;
    - prune with the view height drops in-flight entries exactly as step 2's rule does with the DB marker (the
      decision 7 tests re-run on the view).
12. **T only** (Codex P1 / P2):
    - a deposit at h is usable by orders at h + 1; non-order kinds drain at h; order kinds at h + K;
    - target h full of delayed orders (200 in the order range) and a deposit queued at h still drains at h;
    - 201 same-block rows in one block: the 201st drains at h + 1, in order;
    - future targets full;
    - an EVM-only zero-fee block with same-block rows runs the native phase;
    - D(h) carries the drain tombstone on both paths;
    - the drain assertion per range.
    Every case runs with the fast-path flag OFF and ON, and after a SIGKILL restart.
13. **HL only (if chosen):** a contract reading `getPosition` sees this block's fill; drain after the EVM part;
    `TransferToPerp` reads the pre-EVM balance; activation / genesis switch.

## 6. Bench plans

### 6.1 Q11: option 0 A/B (archy, after P2)
- **Arms (2 x 2, one P2 binary):** N-off, E-off, N-on, E-on (`TORUS_EVM_FAST_PATH` absent / `1`). Shape: hsdb-300m
  (Classic, 15 s checkpoints, oracle on, 300 markets, 600 s, BLOCK_CAP 400, rate 76,000). Genesis N = 11,
  `EVM_LOAD_RATE=10 EVM_LOAD_SENDERS=40`, rotating order, n = 2-3, warm-up. E-on is the arm without a checkpoint at
  each EVM height (Claude M4's arm). Under question 4 a it is the design, so no knob for revision 1's point 8 is built.
- **Pass (all):**
  1. E-on / N-on >= 0.97. E-off / N-off ~0.876 (Q5 reproduced within ~1.5 points). N-on vs N-off within noise (the
     flag does nothing without EVM).
  2. Pipelined share at EVM heights >= 99%; exec idle before an EVM block p50 < 5 ms (Q5: 542-588 ms).
  3. `torus_exec_checkpoint_deferred_read_waits` delta 0 in every cell. The transfer load calls no precompile, so any
     count means a read reached L's deferred part: a bug in point 2 or 8.
  4. `torus_exec_resident_rows_rebuilds` delta 0 over the bench window in every cell, equal across arms (R never
     rebuilt), and `torus_exec_checkpoint_layer_shared` 0 (point 5).
  5. `torus_exec_checkpoints` per minute and `torus_exec_checkpoint_layer_bytes` summed per minute: E-on within noise
     of N-on (no checkpoint forced by EVM heights); E-off shows the forced ones (~16 more per minute at N = 11).
     `torus_exec_checkpoint_wait_seconds` and W busy no higher than N-on.
  6. EVM checks: landed = accepted, off_cadence 0. The eth head reaches the load's last height within one interval
     after the load stops (the `evm_load.py` scan). No fail-stop or divergence line; state-hash attestations agree.
- **Report (q5-gaps view):** exec ms at EVM heights vs others, `torus_exec_evm_seconds` per EVM block and per gas, W
  busy, commit -> exec lag, exec queue peak, block time p50 / p99 inside and outside checkpoint windows, CPU-s / 1M,
  RSS, view timeouts, `no_proposal`.
- **Also:**
  - An E-heavy cell at `EVM_LOAD_RATE=40` (fills the 3M lane: ~142 transfers per EVM height at N = 11), off and on. It
    sizes C0's trigger and the governance ranges.
  - One SIGKILL crash cell set under EVM load with the flag on (digests agree, replay length).
  - One 3-node mixed-flag devnet cell under EVM load (val0 on, val1 off, val2 on with
    `TORUS_CHECKPOINT_INTERVAL_MS=0`): attestations agree for the whole cell.
  - Later, after harness work: a contract-call load (ERC20 transfers, reader precompiles; deferred read waits expected
    <= 1 per EVM height) and a big-block cell (M > 0, 10-15M) for the spike.

### 6.2 Q12: option S, larger EVM cadence N (paste into `docs/perf/bench-queue.md` Items)

- **Q12. Option S: does a larger EVM cadence N recover Q5's loss?** (item 7 step 3, `docs/plans/item7-step3-design.md`
  sections 3 S and 6.2; no node code). Q5 (section 80): -12.4% matched/s at N = 11, from the barrier before every EVM
  block. Raising N (genesis `evm.block_interval`, governance 2..=100) is the zero-code alternative to option 0.
  - Binary: M (the Q5 binary `e3d11cb9`, or the campaign's M), unchanged.
  - Genesis change: the per-cell `devnet/wsl/genesis-3val.json` with `.evm.block_interval = N` (`EvmConfig.block_interval`,
    `crates/torus-genesis/src/lib.rs:141-156`; absent = `torus_types::DEFAULT_EVM_BLOCK_INTERVAL` = 11; 0 refused,
    lib.rs:382).
    - Prerequisite (tooling, ~15 lines + a test, 18c): `devnet/wsl/gen-3val-genesis.sh` applies `EVM_BLOCK_INTERVAL`
      with a jq patch like its `TIMEOUT_BASE_MS` step. `tools/matched-bench/run-cell.sh` lifts it from `EXTRA_ENV` like
      `TIMEOUT_BASE_MS` (run-cell.sh:906-908) and logs it. `evm_load.py` already takes N from the genesis
      (run-cell.sh:934).
    - Without the knob: patch `testnet/genesis-weighted-full.json` per cell (`jq '.evm.block_interval = N'`; the
      staleness check ignores `evm`, run-cell.sh:748-760) and restore 11 after.
    - Either way, check `.evm_load.interval` in every `summary.json`.
  - Arms (one binary, one genesis base):
    - N11: native only at N = 11 (no EVM load).
    - E11 / E22 / E33: native + `EVM_LOAD_RATE=10 EVM_LOAD_SENDERS=40` at N = 11 / 22 / 33.
    - No native arm per N: a height without EVM txs never barriers (`has_evm = !evm_transactions.is_empty()`,
      `app.rs:3110`; the predicate needs `!has_evm`, 3166), so native-only is the same at every N.
    - 33 is the top: at 10 tx/s an N = 44 block would carry ~130 txs (nominal), close to the 142-tx cap of a 3M block
      (`app.rs:2261`).
  - Shape: hsdb-300m as Q5 (Classic, 15 s checkpoints, oracle on, 300 markets, 600 s, BLOCK_CAP 400, MAX_IN_FLIGHT 4,
    commit tail, rate 76,000); D, M, T unset; header re-send 250 ms.
  - Cell order: warm-up w1 (N11, 300 s), then r1 N11, E11, E22, E33 and r2 E33, E22, E11, N11 (n = 2, rotating):
    8 cells of ~10 min, ~1 h 45 in all. If the queue is tight, n = 1 is a ~55 min screen. Extend to n = 3 if a rule
    below sits within 1.5 points of its threshold.
  - Checks per E cell (Q5's): `.evm_load.ok` true, `interval` = the cell's N, `off_cadence_blocks` 0, landed =
    accepted, `[.evm_block_txs[][0] % N] | unique` = `[0]`, `health.py accept`.
    - New: max txs per EVM block < 142; a cell at the cap is flagged (the cap, not the load, then sets the EVM rate).
    - Q5 reported 58.8 txs per EVM block, but 6,000 txs / 161 blocks = 37. Check which count the gaps script uses
      before reading the cap margin.
  - Report per arm:
    - matched/s (primary), and E/N = matched/s divided by N11's, paired by replicate;
    - blocks/s and native blocks/s (indicative);
    - EVM blocks per minute, txs per EVM block (mean, max);
    - EVM latency: the EVM height interval in s (p50, p99) and commit -> exec lag p50, giving the inclusion latency
      (interval / 2 + lag; a per-tx latency needs tx-flood send timestamps, not built);
    - exec idle before EVM blocks (mean, p50) and exec ms at EVM heights vs others (q5-gaps view);
    - `torus_exec_checkpoints` per minute and mean `torus_exec_checkpoint_layer_bytes` (does the forced checkpoint
      grow with N?);
    - `torus_exec_evm_seconds` per EVM block, CPU-s / 1M, timeouts, `no_proposal`.
  - Decision. L(N) = 1 - E/N(N); L11 comes from this campaign (Q5: 0.124).
    - **S-INTERIM** if L(22) <= 0.03 or L(33) <= 0.03: raising N recovers the loss at 2-3x the EVM latency. The owner
      may set that N for devnet / testnet until P2 (no code), and P2's slot may wait (question 2).
    - **STOPGAP** if L(33) <= L11 / 2 but above 0.03: record N = 33 (or 22) as the no-code fallback in case EVM traffic
      arrives before P2. Plan unchanged.
    - **REJECT** if L(33) > L11 / 2: S does not halve the loss even at 3x the latency. Dropped with data.
    - Either way, read the mechanism: the per-EVM-block cost (idle + extra exec ms) against the bytes per forced
      checkpoint. A cost flat in N means the barrier is not bound by the window (this doc's inference is wrong), and
      S then scales ~1/N.
  - Doc target: the next results-doc section; 18c copies the outcome into `item7-step3-design.md` section 3 S.

## 7. Owner questions

Revision 2 status of the revision 1 picks: questions 1, 2, 5, 6, 7 keep their pick (5 with a new mechanism). Question
4's pick (a), "checkpoint at each EVM height", is **obsolete** (owner s127). Question 3's pick stands, but its timing
"T is independent of P2 and can go first" (revision 1, 4.1 P3) is **obsolete**. Revision 1's 4.3 item 2 (R / `Users`
attachable to the journal) is **obsolete**. Questions 8-11 are new.

**Decided (owner s127): every revision 2 pick below is accepted (1-11), including 11: the revm read-error gap is fixed now, as its own fix on main, ahead of step 3.**

1. **Build option 0?**
   (a) Yes, behind `TORUS_EVM_FAST_PATH`, default on after Q11.
   (b) Yes, no flag.
   (c) No, raise N (option S): only defensible if Q12 says S-INTERIM and EVM load stays at Q5's level.
   **Pick: a** (kept; c is now decided by Q12 data).
2. **When?**
   (a) After Phase 5 step 2, as the roadmap orders (no parallel edits in the EVM section and freeze).
   (b) Before Phase 5 step 1, while archy runs the Phase 5 benches (Q5's -12.4% back sooner; Phase 5 then designs
   decision 4 against the final EVM path).
   (c) After all of Phase 5.
   **Pick: a** (kept). Phase 5's gates are native-only and 0 has no genesis deadline. Take b instead if EVM traffic on
   devnet / testnet comes before Phase 5 ends and Q12 does not give S-INTERIM. P1 fits in any slot.
3. **Order semantics before testnet?**
   (a) Keep EVM-first and add T after P2 (same-block EVM -> native transfers and non-order CoreWriter actions).
   (b) Full HL order (post-trade reads too; forecloses C0).
   (c) Leave the timing as today.
   **Pick: a.** Revision 1's "T can go first" is obsolete (Codex P1, Claude M5).
4. **RPC freshness on the fast path?** Revision 1's pick (a), checkpoint at each EVM height, is obsolete (Q6
   conflict; ~4x the checkpoints and ~2x the bytes of the N arm, Claude M4). New options:
   (a) EVM state and receipts land at the next regular checkpoint, with the marker (eth head = applied marker). That is
   at most one interval of lag, as for native state since Phase 3 step 2; `pending` nonces and the mempool already use
   the view.
   (b) eth reads through the published view: CF_STORAGE, CF_CODE, CF_RECEIPTS, tx location and hash -> number join the
   ingress CFs. The side rows then become overlay rows (non-hashed, like the header fold), the eth head becomes the
   view height, and `eth_call` runs over point 2's adapter. This is Phase 5 parked item 1, "RPC freshness the HL way".
   (c) Force a checkpoint at an EVM height only when the last one is older than X s: bounded lag, fewer forced
   checkpoints, but still Q6 traffic.
   **Pick: a** for P2; b before testnet if wallets need fresher receipts (with parked item 1).
5. **EVM trie on the fast path?**
   (a) W computes it in the checkpoint batch from per-block hashed inputs with keep / wipe flags, plus the native-credit
   accounts (point 6; revised mechanism, Claude M6).
   (b) A best-effort resync after the checkpoint (a crash can leave a stale trie).
   (c) Stop maintaining the EVM trie on the committed path until a header carries an executed root (nothing else reads
   it today).
   **Pick: a** (kept).
6. **C0?**
   (a) Park it. Build only if Q11 or a later bench shows the EVM section above ~3% of exec time at the EVM budget then
   in force (big blocks on, lower N, or a time cadence).
   (b) Build right after 0.
   (c) Drop it.
   **Pick: a** (kept).
7. **Phase 5 Task 2.3?**
   (a) Shrink it to the "no typed-CF write in an EVM tx scope" guard plus the drain write-through test (section 4.3).
   (b) Keep it as written.
   **Pick: a** (kept; Claude re-verified the premise).
8. **(New) T's capacity under the 200-row cap** (Codex P2).
   (a) Two seq ranges per target (0..199 same-block kinds, 200..399 order kinds), cap 200 each, each spilling within
   its own range; the same-block range drains first.
   (b) One shared cap with spill: same block only if there is room, so a backlog of delayed orders can delay a deposit.
   (c) An uncapped same-block range: always the same block; per-block drain bounded only by EVM gas (~150 rows per 3M,
   ~750 per 15M).
   **Pick: a** (holds under any order backlog; keeps a hard drain bound; step 2's walk unchanged for orders).
9. **(New) T and the h + 1 rule.**
   (a) Keep "queued in a strictly earlier block" for order kinds (K). For the other kinds, replace it with "drained
   after every native batch of the queuing block" (section 3 T).
   (b) Keep h + 1 for every kind (T is dropped).
   **Pick: a.**
10. **(New) Q12 (option S).**
    (a) Run it before P2, after campaign A (~1 h 45, binary M, one harness knob).
    (b) Skip it.
    **Pick: a.**
11. **(New) The pre-existing EVM DB-error gap** (Claude H1, section 1).
    (a) Fix it now in P1 as its own small change: a read error inside revm fail-stops instead of running the block
    without its EVM writes.
    (b) Fix it only with P2.
    **Pick: a.**

## 8. Not building (YAGNI)

- Block-STM inside the EVM (roadmap T4.4 part 2): needs EVM budgets far above step 2's.
- Onchain EVM mempool and per-block fees (F4): parked to Phase 5 (`item7-step2-design.md:162-173`); independent of 0.
- A time-based EVM cadence (step 1 decided height).
- Overlapping the EVM part with signature verification only (saves <= ~15 ms per EVM block, estimate).
- A knob to checkpoint at each EVM height (question 4 a makes it unnecessary; Q11 measures without it).

## 9. Unknowns and risks

- The residual EVM cost per gas on the fast path is estimated, not measured (Q11 measures it). So is S (Q12).
- The size of the deferred-part wait under reader-precompile load (estimate ~20 ms, <= 1 per EVM height): needs the
  contract-call cell of Q11 "later".
- A missed raw read diverges replicas only when their checkpoint timing differs. The guards are point 4's tripwire,
  tests 2 / 3, and a Codex torus-adversarial review before the A/B.
- EVM RPC "latest" lags up to one interval under question 4 a. Is that acceptable for wallets and explorers before
  testnet? (question 4 b otherwise)
- T's seq-range split changes the seq numbers that consumers see (explorer, `docs/api/corewriter-results.md`); they
  must be listed before P3.
- HL facts not documented: liquidation placement, the length of "a few seconds", same-L1-block Core -> EVM timing.
- C0's join rule needs a key-level audit of every native reader of CF_FEE_CONFIG and the EVM CFs before it is
  credible; the step 2 cursor rows are the first case.
- Governance ranges (N 2..=100, small budget 1M..=10M) were sized for serial EVM blocks
  (`item7-step1-design.md:110-111`); revisit with Q11 and Q12.

## 10. Review findings and how the design handles them

Reviews of revision 1 (`3b79a617`): Codex `reviews/codex-item7-step3-adv.md` (C-P1, C-P2) and Claude
`reviews/item7-step3-claude-review.md` (H1-H3, M4-M8, weak spots W1-W2, missing tests). All were verified in code at
`dcee5cac` / `c567b1b3` and all are accepted. Two are accepted with a different fix, with the evidence given.

| finding | verified at | handling | tests / bench |
|---|---|---|---|
| C-P1 (P1): standalone T leaves a target-h row invisible to h's drain, then lands it after h (stranded deposit) | rows only in `evm_batch` + extras `app.rs:3300-3333`; `seed_from_bundle` CF_ACCOUNTS only `backend.rs:1702-1721`; raw due-check `app.rs:2353-2362`, `native_executor.rs:13520`; Codex probe | absorb on both paths (point 5; serial in P1), `core_writer_due` after the absorb (point 8); T sequenced after P2 | 12 (flag off, restart), 1 |
| C-P2 (P2): same-block settlement vs step 2's shared 200 cap and spill walk | `precompiles.rs:1423-1470 @ c567b1b3`, cap `torus-types/src/lib.rs:781 @ c567b1b3` | two seq ranges per target with their own caps; same-block range drained first (section 3 T, question 8) | 12 |
| H1: revm DB error -> `Internal` -> the generic arm runs the block without EVM writes on one node | `executor.rs:673-680` (`@ c567b1b3` 733), `app.rs:3361-3363` | adapter and journal errors recorded as local faults (point 3); the same fix for today's path in P1 (question 11) | 4 |
| H2: "a raw read is a type error" is overstated | `StateDb: StateBackend` `backend.rs:222`; `db()` 1724; `pending_count(&StateDb)` `precompiles.rs:1571 @ c567b1b3`. `append_links` (`corewriter_tx.rs:68`) only writes a batch (no read): handled as a side row | entry points off `&StateDb`, a run-time tripwire with an exemption for the overlay's bottom layer, poison tests (point 4) | 3 |
| H3: R attached to the journal blocks `Arc::get_mut`, so R is rebuilt | `native_executor.rs:4568-4590`, 4167-4171. Also found: L / deferred `Arc::try_unwrap` at `take_layer`, `app.rs:220-252` (deep copy) | **different fix:** the journal never attaches R (point 2). Releasing R before `end_resident` would still need R before the EVM part, but `begin_resident` sits inside the native phase, which depends on the EVM fee revenue (`app.rs:3476-3480`, 3747), and moving it exposes ~100 ms of `end_resident` per EVM height. The journal is consumed before the native phase (point 5). Cost: <= 1 bounded deferred wait per EVM height with reader precompiles | 5; Q11 pass 3, 4 |
| M4: point 8 fights Q6 (~4x checkpoints, ~2x bytes; omitted from the estimate) | Q6 `0f50b54b` 1.1 and A1; triggers `app.rs:2767-2773` | point 8 dropped. RPC follows the regular checkpoint (point 9, question 4 a); Q6 interplay in 4.4 | 11; Q11 pass 5 |
| M5: T needs 0; breaks `candidate >= next_block`; drops the anti-frontrunning reason | `precompiles.rs:1436-1440 @ c567b1b3` | T after P2; the invariant holds in the order-only range; the h + 1 rule kept for orders and justified for the rest (section 3 T, question 9); size S-M -> M | 12 |
| M6: the hashed mirror is not derivable from final plain rows (wipe scan); the resync belongs in the batch | `incremental.rs:322-367`, 416-490; `exec_pipeline.rs:948` | per-block hashed input with keep / wipe flags, folded in height order; native-credit accounts in the same batch (point 6) | 1, 6 |
| M7: W2 side rows rewrite header / body, breaking Q6 A1 key disjointness | `committer.rs:151-171`, `app.rs:1582-1640` | header / body dropped from the side rows (point 7) | 1 |
| M8: prune `executed_height` must be the view's height | `torus-mempool/src/lib.rs:2152-2195 @ c567b1b3`, `read_view.rs:20-26` | the view height, read from the same handle before the nonces (point 8) | 11 |
| W1: option S dismissed without a measurement | - | Q12 bench spec (section 6.2) and estimate (section 3 S); question 10 | Q12 |
| W2: say why a narrower barrier is not simpler | `checkpoint_layer.rs:26-31`; Q6 section 3 | option N (section 3) | - |
| Missing tests (Claude) | - | adapter fault -> 4; tripwire -> 3a; R fallback 0 -> 5; drained-but-in-DB rows -> 3c; poison C and parent -> 3b; h % 100 and epoch boundary with both flags, `TORUS_CKPT_DEFER_BOOKS` -> 1; 3-node mixed-flag cell -> 2, Q11 "also"; Q11 pass criteria -> 6.1 | - |
| Confirmed by Claude (no change) | Task 2.3 premise; hash extras rank below pending; the bbo row is an overlay write on the deferred-book path; reader gas counts hashed rows only; every header is durable at dispatch | kept | - |
| Found while verifying (not in either review) | L / deferred part shared at `take_layer` (`app.rs:220-252`); `run_native` depends on the EVM fee (`app.rs:3476-3480`); a no-native EVM block writes a marker-only set (`app.rs:4575-4640`); Q6 A1 assumes pipelined blocks are native-only (its tail routing) | journal consumed before the native phase (point 5); no R (point 2); the overlay is always frozen / flushed after an absorb (point 5); Q6 note (4.4 item 3) | 5, 1 |
