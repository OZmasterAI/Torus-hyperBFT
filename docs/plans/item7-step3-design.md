# Design: item 7 step 3, isolate trading from EVM

Status: brainstorm (s127), DESIGN ONLY, for the owner. Nothing built. Builds on step 1 (main) and step 2
(`feat/item7-step2` `c567b1b3`, not merged, waits on archy's Q7). The block in `item7-evm-lanes.md` 47-63 (owner s122)
frames step 3 as HL style or C, both "after EVM reads / writes go through the overlay", both after item 6 Phase 5.
Anchors are main `dcee5cac` unless marked `@ c567b1b3`. "(estimate)" marks numbers not measured; "inferred" marks code
facts not read directly.

**Short answer.** The overlay step alone (option 0 below) removes the barrier, and with it nearly all of the measured
cost. It changes nothing a contract can see, so it needs no fresh genesis and can be switched per node. HL order is a
parity choice that brings no speed over option 0. C can be built so that it changes nothing a contract sees either
(C0). So the old framing ("either changes EVM-visible results", "both need the overlay first, then pick one") becomes:
build 0; choose the order semantics on parity grounds alone; keep C0 parked until EVM run time matters.

## 1. Problem

### Measured (Q5, results doc section 80, 12175-12236; main `e3d11cb9`, 300 markets, N = 11, 600 s, n = 2)
- 10 EVM transfers/s (40 senders, ~59 txs per EVM block) cost **-12.4% native matched/s**: E 211.6k vs N 241.6k, E/N
  0.876 (r1 0.870, r2 0.882; replicate spread 0.2% N, 1.5% E). CPU-s per 1M fills +1.8%.
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
Anything else calls `pipeline_barrier` (`app.rs:2504-2545`, `@ c567b1b3` 2737): `checkpoint_drain` closes the
since-checkpoint window L plus the last digested block into a checkpoint (`app.rs:2853-2882`), `wait_idle` waits until
the flush worker W has written it, the published ingress view is dropped, and the block runs the serial path on the
DB. At 300 markets a window is ~100 MB (book blobs dominate) and W writes it in ~0.26-0.51 s (memory 123e4dd2, archy
hsdb-300m ckpt samples), which is the ~0.5 s idle. The serial path then writes the block's state, native trie and
marker on E (`app.rs:4312-4400`, ~0.12 s per block at 300 markets in an older run, memory 123e4dd2) instead of handing
it to W. The barrier exists because the EVM path reads and writes the DB directly, so the DB must be current:

| # | EVM-path access that bypasses the overlay / layers | anchor |
|---|---|---|
| R1 | revm reads accounts, storage, code, block hashes from `StateDb` (`State::with_database_ref(state_db)`) | `torus-evm/src/executor.rs:282-294`; `DatabaseRef for StateDb` `torus-state/src/db.rs:1474-1503` (CF_ACCOUNTS 1219, CF_STORAGE 1245, CF_CODE 1282, CF_BLOCK_HEADERS 1297) |
| R2 | precompile journal `NativeStateOverlay::new(state_db)`: own pending -> DB, no parent / L / C. Readers: positions, balances + EVM balance, markets, oracle, staking (`precompiles.rs:607-990`); CoreWriter `next_sequence` scan (1397-1409) | `executor.rs:303-305`, `precompile_provider.rs:26-48` |
| R3 | EVM root over the DB trie (`compute_post_bundle_state_root_with_updates(state_db, ..)`), incremental by default | `validator.rs:198-200`, `state_root.rs:24-29, 37-60, 119-128`, `incremental.rs:240-264` |
| R4 | EVM block batch built against the DB: plain + hashed mirror (wipe scan) + trie nodes + journal rows | `app.rs:3300-3315`, `incremental.rs:396-414, 349-365` |
| R5 | `core_writer_due` reads the queue with a raw RocksDB iterator | `app.rs:2353-2362, 3395-3405`, `native_executor.rs:13520-13522`, `precompiles.rs:1368-1388` |
| R6 | serial-path resync of EVM accounts credited by native (fees, rewards, transfers) | `app.rs:4402-4417`, `incremental.rs:431-482` |
| R7 | step 2 lane rows (`EvmUserModify`) read from `state_db`, comment "an EVM block runs after a barrier, so the DB is current" | `validator.rs:154-180 @ c567b1b3` |
| R8 | step 2 CoreWriter enqueue: delay param, last-order-target / first-open-target / order-run cursor rows (CF_FEE_CONFIG), `get_cf_len` full-target probes, through the journal -> DB; doc "rows are queued only by EVM blocks (serial barriers)" | `precompiles.rs:1397-1470 @ c567b1b3`; due-check doc `native_executor.rs:13596-13609 @ c567b1b3` |
| R9 | mempool prune of executed EVM txs: one raw-DB nonce read per sender | `torus-mempool/src/lib.rs:2160-2181 @ c567b1b3` |
| R10 | RPC `eth_getBalance` / `getCode` / `getStorageAt` / `getTransactionCount(latest)` / `eth_call` read the DB; the eth view lags to the last checkpoint (`tools/matched-bench/evm_load.py` 7-9), and today an EVM block is visible at once only because its barrier forced a checkpoint | `torus-rpc/src/eth.rs:714-780` |
| W1 | EVM writes ride a `WriteBatch` prefix of the serial flush (bug (c) atomicity), plus hash-only `HashExtras` | `app.rs:3258, 3319-3333, 4337-4345, 4575-4646` |
| W2 | `commit_block_metadata`: header JSON, body, receipts, hash -> number, tx location, in its own direct batch | `app.rs:3342`, `committer.rs:145-197` |
| W3 | CoreWriter tx links appended to the EVM batch (node-local CF) | `app.rs:3319-3327`, `corewriter_tx.rs:68-86` |

What is NOT on the list: the writer precompiles write only `CF_CORE_WRITER_QUEUE` (and, `@ c567b1b3`, the CF_FEE_CONFIG
cursor rows); they never write positions, balances or `CF_ACCOUNTS` (`precompiles.rs:996-1285`; lockbox doc
`lockbox.rs:15-28`). The native leg of a lockbox deposit runs in the next block's drain (`native_executor.rs:13650-13655`).
Consensus never reads EVM results: the header copies the parent's `state_root` and carries zero receipts root, bloom
and gas used (`app.rs:7403-7406`); catch-up skips the root check (`validator.rs:101-110`). The running state hash
covers only the plain EVM CFs (ids 0x80-0x82), never the trie or hashed mirror (`running_hash.rs:20-26, 35-62`).

### What must move for an EVM block to pipeline at all
R1, R2, R5, R7, R8 read through the block's layered overlay (own pending -> R -> parent -> L -> C -> DB,
`backend.rs:1366-1458`). W1's plain bundle rows and journal rows go into the block's overlay, so the frozen set
carries them (digest, L, checkpoint: one atomic batch, bug (c) kept). R3, R4, R6 move to W (the trie is derived,
node-local data). W2 and W3 become side rows of the block's job, like the action status record
(`exec_pipeline.rs:866-880, 969-1000`). R9 and R10 read the published view or rely on a checkpoint at the EVM height
(question 4). That set is option 0.

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

Common base for HL, T, C0, C1, D: option 0. S is the no-code alternative.

### S. Stopgap: fewer EVM blocks (governance N up, range 2..=100)
- Mechanism: none new. The barrier cost per EVM block is not fixed: the forced checkpoint covers the window since the
  last one, so a rarer EVM block waits on a bigger write, up to a normal 15 s window (inferred from the checkpoint
  sizes above). Halving the EVM blocks saves well under half the cost (estimate) and doubles EVM latency.
- Semantics: none. Size 0. Not a fix; listed so it is not mistaken for one.

### 0. EVM blocks on the pipelined path, same order (recommended base)
- **Mechanism.** Behind a node-local flag `TORUS_EVM_FAST_PATH` (default off until the A/B, as `TORUS_EPOCH_FAST_PATH`,
  `exec_pipeline.rs:57-66`), drop `!has_evm` from the pipelined predicate, then:
  1. Build the block's overlay with its layers before the EVM section (today it is built after, `app.rs:3415-3448`).
  2. revm reads through a new `DatabaseRef` adapter over that overlay (CF_ACCOUNTS / CF_STORAGE / CF_CODE via
     `get_cf_raw`, block hashes from the dispatch-durable headers). The precompile journal gets the same layers and R
     attached (R = post-(h-1) positions / balances; this keeps `torus_exec_checkpoint_deferred_read_waits` at 0, since
     positions and balances are L's deferred CFs, `checkpoint_layer.rs:38-41`).
  3. The EVM entry points (`EvmExecutor::execute_block[_lanes]`, `validate_block_inner`, `CoreWriterQueue::pending_count`)
     take `&impl StateBackend` instead of `&StateDb`, so the fast path cannot reach RocksDB by construction. The serial
     path keeps passing `StateDb`.
  4. After the EVM part, the bundle's plain change set (exactly `apply_bundle_plain`'s puts / deletes,
     `incremental.rs:270-311`, which reads nothing) and the journal's pending rows go into the block overlay
     (`seed_from_bundle` grows from CF_ACCOUNTS to all three EVM CFs). EVM `HashExtras` are then unnecessary: D(n) is
     over the same final rows per key.
  5. The EVM root is not computed on E. W updates CF_HASHED_* / CF_TRIE_* for the window's EVM changes (accounts and
     storage slots) at its checkpoint (question 5). `resync_evm_accounts` is account-only today
     (`incremental.rs:416-431`) and needs a storage-aware form.
  6. Receipts, tx location index and CoreWriter tx links become side rows of the block's Digest / Flush job, written
     in the checkpoint batch like the action status record. An EVM-only block hands off a Digest / Flush job, not a
     Marker.
  7. `core_writer_due` reads through the overlay (like `oracle_due`, `native_executor.rs:13524-13531`); the step 2 lane
     read and enqueue reads go through the journal's layers; mempool prune reads the ingress view (CF_ACCOUNTS is
     already an ingress CF, `read_view.rs:58-67`).
  8. RPC freshness: the EVM block checkpoints at its own height, asynchronously (the epoch fast path's rule,
     `app.rs:2767-2768`), so an EVM block's state and receipts land together ~one W write after it executes, as today
     minus E's wait (question 4).
- **Determinism.** Same argument as every pipelined native block: own pending -> R -> parent -> L -> C -> DB answers
  every key as of post-(h-1) on every node, whatever each node's checkpoint timing; the serial path answers the same
  from the drained DB. The new risk is a missed raw read (a node-dependent answer = divergence); point 3 makes that a
  type error. Tests compare serial, flag off and flag on, under every checkpoint policy (section 5).
- **Crash / replay.** The EVM rows ride the block's frozen set, so the block is durable iff its marker is (bug (c)
  unchanged). Replay has no W and stays serial (`app.rs:3155-3156`), which must give the same state (tested).
- **EVM-visible semantics.** None: contracts read block-start state, native sees the EVM results, queue targets and
  CoreWriter timing are unchanged. h_n and every consensus row are byte-identical, so validators can mix flag on and
  off. **No genesis implication; can ship after testnet.**
- **Effect (estimate).** Removes the ~0.5 s idle and the serial flush per EVM block; E keeps revm itself (~20-30 ms
  per Q5 EVM block once the root and metadata write leave E). At Q5's load ~18% of exec time comes back, so E/N
  ~0.97-0.99 instead of 0.876 (the closed-loop bench may not convert exec time 1:1, section 54). Full small lane (3M gas
  at 7-28 ns/gas: read-precompile bench `read-precompile-gas.md:111-118`, Q5 transfers <= ~28 ns/gas incl. root):
  21-84 ms per EVM height, 0.6-2.5% of exec at N = 11. A step 2 big block of 10-15M: 70-420 ms on E once per M
  heights (one slow block, a p99 item), no barrier.
- **CPU:** E idles less; W writes the same checkpoints as today (point 8) plus a small trie job. RAM: EVM rows in the
  frozen sets (~59 accounts x 72 B per Q5 block), negligible.
- **Size M** (estimate ~1-1.5k lines incl. tests): adapter, signature changes in torus-evm / torus-bridge, overlay
  absorb, W trie job, side rows, three read paths, flag, tests. **Risk M** (type-enforced reads, the s124 epoch fast
  path as the template, differential tests).
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

### T. 0 (or today's path) + HL timing for EVM -> native, EVM-first kept
- **Mechanism.** Lockbox deposit / withdraw and non-order CoreWriter kinds target the queuing block h instead of h + 1;
  h's drain (after its native batches) runs them. Order kinds keep step 2's K. The anti-MEV assertion "queued in a
  strictly earlier block" (`native_executor.rs:13533-13535`) becomes kind-aware. `run_native` must also fire when the
  EVM part queued rows for h (an EVM-only block). Does not need 0 to work.
- **EVM-visible:** yes (timing only): orders see an EVM deposit at h + 1 instead of h + 2; staking and withdraw actions
  one block sooner. Contracts keep block-start reads. Genesis-bound like HL.
- **Effect:** ~0. **Size S-M. Risk L-M.** Keeps C0 possible.

### C0. 0 + EVM on its own thread, semantics kept (parked)
- **Mechanism.** At an EVM height E starts the EVM part on a helper thread X over an immutable block-start view
  (parent, L, C, DB; never R or `Users`, which E mutates) and runs the oracle step and the native batches meanwhile.
  Join rule, a pure function of the sorted actions: E joins X before the first action that touches an EVM-domain row
  (category Lockbox: `TransferToPerp` / `TransferToSpot` / `Withdraw`; Staking claims that credit EVM; `classify_action`
  `native_executor.rs:13939-13968`) and before the block tail (drain, liquidations, fees need X's revenue, F3 its gas).
  All trading categories (0-2) sort before Lockbox (3), so the join normally comes after the trading
  (`native_executor.rs:13927-13936`). At the join X's rows enter the overlay first (as today's EVM-first order).
- **Determinism.** Equal to sequential "EVM then native" iff E's pre-join reads miss X's write set (EVM CFs, queue rows
  for >= h + 1, the step 2 cursor rows). Needs an audit of every pre-join reader at key level (fee rates share
  CF_FEE_CONFIG with the cursor rows) and a debug-build read-set / write-set check. Same join point on every node.
- **Crash / replay:** nothing durable before the hand-off; replay serial. **EVM-visible: none; no genesis.**
- **Effect (estimate):** takes the residual EVM time of 0 off E (0.6-2.5% at a full small lane, 2-8% at an HL-like 1 s
  cadence, a big block's 70-420 ms) if a core is free; benches run 3 validators on one host whose engine already uses
  worker threads, so the core is not free there. +1 core during EVM heights.
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
| S | no | small gain, EVM latency up | barrier + EVM | 0 | L |
| **0** | **no / none** | **E/N ~0.97-0.99** | 70-420 ms | **M** | **M** |
| HL (on 0) | yes / genesis | as 0 | as 0 | +L | M |
| **T (on 0 or alone)** | **yes (timing) / genesis** | as 0 | as 0 | **+S-M** | **L-M** |
| C0 (on 0) | no / none | ~0.99-1.0, +1 core | ~0 | +L-XL | H |
| C1 (on 0) | yes / genesis | as C0 | ~0 | +L | M-H |
| D | yes (twice) / genesis | as C0 | ~0 | XL+ | H |

## 4. Recommendation

**Build 0. Keep EVM-first and add T before the testnet genesis. Park C0 with a measured trigger. Do not take HL order.**

1. Option 0 removes the whole measured mechanism (the barrier and the serial flush). It changes nothing a contract
   can see, so it carries no genesis deadline, can be A/B'd per node on one genesis with Q5's two arms, and can be
   rolled back by a flag.
2. HL order buys only post-trade reads (one block of staleness, ~0.3 s, against a ~3.4 s EVM cadence) for an L-size
   change, more test churn, a Phase 5 read hook into `Users`, and the permanent loss of C0. T delivers the other
   parity point (same-block EVM -> native transfers and non-order CoreWriter actions) for S-M and keeps C0 open.
3. After 0 the EVM run time left on E is ~1% at Q5's load and 0.6-2.5% at a full 3M lane (estimate). C0's risk is not
   worth that now. It becomes worth it if budgets grow (big blocks on, N lowered, or HL's 1 s cadence: 2-8% of exec),
   and since C0 is semantics-neutral it can come then without a genesis.

### 4.1 Phased plan
- **P0 (now, docs):** owner answers section 7.
- **P1 (optional, any time, behaviour-neutral):** R5, R7, R8 read through the block overlay / journal layers
  (identical under the barrier, where the layers are empty); R9 via the ingress view; the EVM entry points take
  `&impl StateBackend`; the serial-vs-pipelined EVM test harness with "flag absent: EVM heights stay serial" (the
  pattern of `epoch_fast_path_evm_or_slash_boundary_stays_serial`, `app.rs:27347`). Removes the barrier assumptions
  step 2 added.
- **P2 (option 0, behind the flag):** points 1-8 of option 0, tests first; Codex torus-adversarial review; archy A/B
  (section 6); flag default on after the A/B. Placement per question 2.
- **P3 (T, before the testnet genesis):** independent of P2; can go first.
- **P4 (parked):** C0, trigger in question 6. HL order only if the owner overrides question 3; then before testnet.

### 4.2 Why 0 is not "both in sequence"
The s122 rule "not both in sequence (contract behaviour would change twice)" holds for HL and C1. Option 0 and C0
change no contract-visible behaviour, so 0 + T (one visible change, at genesis) + later C0 changes behaviour once.

### 4.3 Item 6 Phase 5: what step 3 needs, what Phase 5 must not foreclose
**Needs from Phase 5: nothing.** Option 0 and T work on today's overlay. The shared code is the EVM section of
`execute_committed_block_with`, `freeze` / `FrozenPending`, and Phase 5 Task 2.3 (`item6-phase5-impl.md:240-249`).
Building 0 after Phase 5 step 2 avoids parallel edits there; building it before step 1 means Phase 5 designs its EVM
fold against the final EVM path (question 2).

**Finding for the Phase 5 plan.** Row 5 and Task 2.3 (`item6-phase5-impl.md:113, 240-249`) assume EVM tx scopes write
`CF_NATIVE_BALANCES` (a lockbox credit inside a reverted frame; an EVM tx crediting a balance a native action then
trades on). No such write exists: the writer precompiles only enqueue (`precompiles.rs:1225-1285`, 1299-1314), and
`credit_native` runs in the next block's native drain, a cold writer covered by decision 3 a. So decision 4 covers an
empty write set; Task 2.3 can shrink to a guard test ("no typed-CF write inside an EVM tx scope") plus the h + 1 drain
write-through test (question 7).

**Phase 5 must not foreclose:**
1. Typed rows keep being serialized into `FrozenPending` at freeze (Phase 5 design: "same logical rows",
   `item6-phase5-impl.md:30-33`), so L, C and the ingress view hold positions and balances. The precompile journal of
   0 reads them through the layers when R is not attached, and C0's thread X must read them there (it may never read
   `Users`, which E mutates).
2. R / `Users` stays attachable to a second overlay (the journal) at block start, read-only, so 0's precompile reads
   skip L's deferred part (the "deferred read waits stay 0" gate, `item6-phase5-impl.md:63`).
3. No new raw-DB read on the EVM path justified by "EVM blocks are barriers" (as step 2's R7, R8 and due-check doc).
4. If the EVM fold of Task 2.3 is kept, it must not be tied to "before the native batch" or to an empty parent layer:
   on 0 the EVM part runs pipelined; under HL order it runs after the batch.
5. For HL order only: a mid-block read path from a `StateBackend` into `Users` (the read side of decision 3's
   `ResidentSource` write hook).
6. `phase5_differential_long_random` (`item6-phase5-impl.md:203-211`) takes the EVM fast-path flag as a dimension
   once P2 exists.

## 5. Test plan (tests first; each fails before its code)

Template: `epoch_fast_path_boundaries_pipelined_and_match_serial` / `fast_path_run` (`app.rs:27150, 27259`), the crash
tests `exec_pipeline_crash_before_write_replays_both_and_matches_serial` (17898) and
`epoch_fast_path_checkpoint_crash_replay_matches_uncrashed` (27389).

1. **Serial == flag off == flag on.** One random block stream with EVM heights: transfers, a deploy, storage writes and
   deletes, reverts, CoreWriter / staking / lockbox calls, `TransferToPerp` / `TransferToSpot` in the same block as EVM
   txs, native fee and reward credits to EVM accounts, an EVM-only block, a failed EVM section. Checkpoint policies
   per-block, never, every 5, by interval. Per block: all CF dumps (consensus CFs, receipts, tx index, CoreWriter tx
   links, action status), `h_n`, markers; after each checkpoint: CF_HASHED_* / CF_TRIE_* equal and incremental root
   == full-scan root. Flag absent: EVM heights serial; flag on: all pipelined.
2. **Mixed replicas.** Three replicas (flag off; on with 15 s interval; on with per-block flush) agree on every `h_n`
   and state-hash attestation.
3. **No raw reads.** Compile-time (point 3 of 0); plus a test that poisons the DB copy of a key held only in L (stale
   value in the DB) and shows each R1-R9 reader returns the layer value on the fast path.
4. **SIGKILL replay at every phase,** single and repeated: after the EVM part before the absorb; after the absorb
   before the hand-off; after the hand-off before the checkpoint; checkpoint written before the trie update (only if
   question 5 b); during the replay of an EVM block. Restart replays serially; dumps and `h_n` equal the uncrashed
   serial run.
5. **EVM revert semantics on the fast path:** a tx queues `placeOrder` then reverts (no row, no seq gap); a caught
   inner-frame lockbox deposit (no burn, no row; sibling rows kept); a reader OOG; a local fault in a precompile read
   that reaches L (fail-stop, nothing handed off; R02 branch 5 pattern).
6. **Transfers both ways:** `TransferToSpot` at a pipelined native block, spent by the next EVM height (read from L);
   lockbox deposit at an EVM height drained by an EMPTY next block (extend `corewriter_evm_row_drains_in_empty_next_block`
   [`_with_interval`], `app.rs:23846, 23935`, to the fast path; under T the drain is the same block); `TransferToPerp`
   after an EVM tx changed the balance in the same block; `withdrawFromNative` credit seen at the next EVM height.
7. **CoreWriter order (step 2):** K-delayed rows whose target falls after a checkpoint; the 200 cap with earlier
   targets' rows and cursor rows still in L; dense seqs; v4 status keyed by (drain height, seq) and tx links equal
   serial (`core_writer_tx_links_each_queued_row_to_its_evm_tx`, `app.rs:20688`).
8. **Step 2 rows:** lane rows set in a pipelined block read by the next EVM height; F3 epoch gas equal serial and
   pipelined (`f3_epoch_gas_sets_plan_fees_serial_and_pipelined`, `app.rs:21353 @ c567b1b3`); a big and a small lane
   at one height; BLOCKHASH of recent heights.
9. **RPC and mempool:** a receipt is never visible before the state it reflects (checkpoint at the EVM height, or
   the view under question 4 b); `eth_getTransactionCount(latest)` and prune agree with `pending_nonce` across a window.
10. **T only:** deposit at h usable by orders at h + 1; non-order kinds drain at h; order kinds at h + K; an EVM-only
    block with queued rows runs the native phase.
11. **HL only (if chosen):** a contract reading `getPosition` sees this block's fill; drain after the EVM part;
    `TransferToPerp` reads the pre-EVM balance; activation / genesis switch.

## 6. Bench plan (archy, new queue item Q11, after P2)

- **Measure = Q5's two arms, on the P2 binary, flag off and on (2 x 2):** N-off, E-off, N-on, E-on; hsdb-300m shape
  (Classic, 15 s checkpoints, oracle on, 300 markets, 600 s, BLOCK_CAP 400, rate 76,000), genesis N = 11,
  `EVM_LOAD_RATE=10 EVM_LOAD_SENDERS=40`, rotating order, n = 2-3, warm-up. Pass: E-on / N-on >= 0.97 and E-off / N-off
  ~0.876 (Q5 reproduced); N-on vs N-off within noise (the flag does nothing without EVM).
- **Mechanism (q5-gaps view):** pipelined share at EVM heights (~100%), exec idle before EVM blocks (~0), exec ms at
  EVM heights vs others, `torus_exec_evm_seconds` per EVM block and per gas, checkpoints per minute and bytes,
  `exec_checkpoint_wait_seconds` (point 8 can wait on an in-flight checkpoint), W busy, commit -> exec lag, exec queue
  peak, block time p50 / p99 inside and outside checkpoint windows, CPU-s / 1M, RSS, view timeouts, `no_proposal`, EVM
  checks (landed == accepted, off-cadence 0).
- **Also:** an E-heavy cell at `EVM_LOAD_RATE=40` (fills the 3M lane: ~142 transfers per EVM height at N = 11), off and
  on, which sizes C0's trigger and the governance ranges; one SIGKILL crash cell set under EVM load with the flag on
  (digests agree, replay length); if the build has the knob, one arm without the checkpoint at the EVM height (cost of
  point 8). Later, needs harness work: a contract-call load (ERC20 transfers, reader precompiles) and a big-block cell
  (M > 0, 10-15M) for the spike.

## 7. Owner questions

1. **Build option 0?** (a) yes, behind `TORUS_EVM_FAST_PATH`, default on after Q11; (b) yes, no flag; (c) no, raise N
   (option S). **Pick: a.**
2. **When?** (a) after Phase 5 step 2, as the roadmap orders (no parallel edits in the EVM section and freeze);
   (b) before Phase 5 step 1, while archy runs Q8 (Q5's -12.4% back sooner; Phase 5 then designs decision 4 against the
   final EVM path); (c) after all of Phase 5. **Pick: a**, because Phase 5's gates are native-only and 0 has no genesis
   deadline; b if EVM traffic on devnet / testnet comes before Phase 5 ends. P1 can go in any slot.
3. **Order semantics before testnet?** (a) keep EVM-first and add T (same-block EVM -> native transfers and non-order
   CoreWriter actions); (b) full HL order (post-trade reads too; forecloses C0); (c) leave timing as today. **Pick: a.**
4. **RPC freshness on the fast path?** (a) checkpoint at each EVM height, async (as today's visibility, without E's
   wait); (b) EVM RPC reads (`eth_getBalance`, `getTransactionCount(latest)`, `getCode`, `getStorageAt`, `eth_call`)
   from the published view with CF_STORAGE and CF_CODE added to the ingress CFs, receipts written right after the view
   (fold into Phase 5 parked item 1, "RPC freshness the HL way"); (c) accept up to one checkpoint interval (~15 s) of
   lag for EVM state and receipts. **Pick: a** for P2; b later with parked item 1.
5. **EVM trie on the fast path?** (a) W computes the hashed mirror and trie updates over DB + the window and writes them
   in the checkpoint batch (atomic); (b) storage-aware resync after the checkpoint (best effort, like today's account
   resync; a crash can leave a stale trie); (c) stop maintaining the EVM trie on the committed path until a header
   carries an executed root (it is read by nothing else today). **Pick: a.**
6. **C0?** (a) park; build only if Q11 or a later bench shows the EVM section above ~3% of exec time at the EVM
   budget then in force (big blocks on, lower N, or a time cadence); (b) build right after 0; (c) drop. **Pick: a.**
7. **Phase 5 Task 2.3?** (a) shrink it to the "no typed-CF write in an EVM tx scope" guard plus the h + 1 drain
   write-through test (section 4.3 finding); (b) keep it as written. **Pick: a.**

## 8. Not building (YAGNI)

- Block-STM inside the EVM (roadmap T4.4 part 2): needs EVM budgets far above step 2's.
- Onchain EVM mempool and per-block fees (F4): parked to Phase 5 (`item7-step2-design.md:162-173`); independent of 0.
- A time-based EVM cadence (step 1 decided height).
- Overlapping the EVM part with signature verification only (saves <= ~15 ms per EVM block, estimate).

## 9. Unknowns and risks

- The residual EVM cost per gas on the fast path is estimated, not measured (Q11 measures it).
- Point 8's checkpoint per EVM height keeps today's write volume and its checkpoint-window p99 effect (section 66); an
  in-flight interval checkpoint can make E wait at an EVM height (measured by `exec_checkpoint_wait_seconds`).
- A missed raw read diverges replicas only when their checkpoint timing differs; point 3's types and test 2 / 3 are the
  guard, and a Codex torus-adversarial review before the A/B.
- HL facts not documented: liquidation placement, the length of "a few seconds", same-L1-block Core -> EVM timing.
- C0's join rule needs a key-level audit of every native reader of CF_FEE_CONFIG and the EVM CFs before it is
  credible; the step 2 cursor rows are the first case.
- Governance ranges (N 2..=100, small budget 1M..=10M) were sized for serial EVM blocks (`item7-step1-design.md:110-111`);
  revisit with Q11.
