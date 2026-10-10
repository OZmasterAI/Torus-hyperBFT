# Design: Item 6 Phase 3, coalesced state checkpoints + replay

Status: brainstorm (s109, 2026-10-10). Input to `item6-phase3-impl.md` (writing-plans), after the owner picks an option.
Parent design: `market-scaling-in-memory-design.md` section "Phase 3" and D5 b, D6 b, D7 c, D10 a, D11 a, D13 a.
Baseline: tw = Classic, `TORUS_TRADE_HISTORY=0` on validators, WAL budget 2048 MiB, WAL compression off
(acc3, results doc section 40; 24.93 node CPU-s/1M, 203,992 matched/s). Main `fd9e5dfa`.

## Problem

Today every native block writes its full state change set to RocksDB in one atomic batch on the flush
worker W. Bench traders rewrite the same rows every block, so most of those bytes and the memtable /
compaction work they cause are overwritten within seconds. Phase 3 writes state every ~30 s of execution
(last value per key) and recovers from a crash by re-executing the blocks since the last checkpoint.

## Context

### What step 0 says Phase 3 can win (results doc 37, plan row 42)

- On Classic, the most Phase 3 can remove is flush worker + state-row compaction + state-row memtable
  flush = 1.76 CPU-s/1M (6.4% of 27.48). Against tw (24.93) that is ~7%. The phase gate was lowered to
  about -6% for this reason; trade history off is a setting, not Phase 3 credit.
- Disk: at most ~54% of bytes are coalescable (step 0, mode 3). The rest are block bodies, DA, hotstuff
  and gossip, which Phase 3 does not touch. With tw, trade rows are already gone.
- Throughput: ~0 while E is the bottleneck; Phase 3 removes the W ceiling for later phases.

### Code today (main `fd9e5dfa`, explore map s109)

- E hands `Job::Flush { height, pending: Arc<FrozenPending>, .. }` to W over a rendezvous channel
  (`exec_pipeline.rs:58-87`, `:199-212`); W builds one batch in `flush_pending_after_batch`
  (`backend.rs:1582-1800`): pending state, sidecar (mode 2/3 books + action status), trie stale mark,
  applied-height marker, running-hash rows, one `write`. Classic books are written into the overlay on E.
- Not in the batch: block bodies / headers (consensus thread at commit, `app.rs:1020`), trade rows
  (separate `trade_writer`, `app.rs:3157`), DA (`CF_NATIVE_PENDING`).
- Readers on E: `NativeStateOverlay::get_cf_raw` (`backend.rs:2008`): own pending -> R (5 resident CFs)
  -> parent `FrozenPending` (exactly one block deep) -> DB.
- Running hash: `chain_step` (`running_hash.rs:341`) reads h(n-1) from META in the DB on W. That works
  only because the pipeline is one block deep. Attest checkpoint every 100 blocks, 64 kept.
- Recovery: `replay_committed` (`app.rs:4889`) replays `(applied, committed]` from `CF_BLOCK_BODIES`,
  always serial, before W is attached.
- Serial barriers (`app.rs:1973`): EVM txs, pending slashes, epoch boundary, header/body not durable.
- Pruner (`pruner.rs:139`): cutoff `current - retention`, not clamped to the applied marker; opt-in
  (`--retention-blocks`, unset = no pruning). Trades and DA never pruned.
- Snapshots: `SnapshotManager` exists, the node never creates one; only restore is wired.

### Readers outside E (reader audit s109)

All go through the raw `StateDb` (no view, no RocksDB snapshot pinning); today they are at most one block
behind and accept it. Under Phase 3 they would be up to one interval (~30 s) behind:
- RPC (~30 call sites): `torus.rs` balances, positions, books, open orders, user limits, staking,
  governance, mark price; `eth.rs` getBalance / code / storage / nonce, eth_call, estimateGas. Several use
  raw `.inner()` iterators; revm uses `DatabaseRef` on `StateDb`. Both bypass `StateBackend`.
- Ingress / mempool: market tick/lot, price band, session lookups (RPC and gossip), min-collateral
  balance check, validator duty exemption, oracle-signer index, EVM nonce / balance, `prune_committed_txs`.
- Consensus thread: `epoch_validator_set_updates` checks the durable applied marker against `H - L`.
- E-side, after a barrier: state-hash monitor (reads marker, running hash, checkpoints from DB), EVM
  execution and precompiles (`NativeStateOverlay::new(state_db)`, DB only).

Closest single-view type: `NativeStateOverlay` (pending -> `Arc<FrozenPending>` -> R -> DB) behind the
`StateBackend` trait, which `PositionManager`, `StakingManager`, `OracleManager` and `book_reader` are
already generic over.

## Options

The core (since-checkpoint layer, checkpoint triggers, in-memory h(n-1), replay from S, pruner clamp)
is the same in all three. They differ in how readers are handled, which is most of the risk and effort.

### Option A: full design as decided (D10 a): every reader on one block-consistent view

- E publishes, after each block, an `Arc` read view = since-checkpoint layer + R + DB at height n.
  Every reader above (RPC, ingress, consensus thread, state-hash monitor) reads through it; the raw
  `.inner()` iterators get merged-iterator versions; revm gets a `DatabaseRef` over the view.
- One node behaviour for validators and RPC nodes; RPC answers are fresher than today (no W lag).
- Files: `backend.rs`, `exec_pipeline.rs`, `running_hash.rs`, `app.rs`, `pruner.rs`, `torus-rpc`
  (`torus.rs`, `eth.rs`, `lib.rs`), `torus-mempool` (`lib.rs`, `funded.rs`, `validate.rs`),
  `torus-evm/executor.rs`, `state_hash.rs`.
- Trade-offs: correct everywhere; but ~40 reader call sites plus merged prefix iterators is the
  biggest and riskiest part of Phase 3, and none of it moves the CPU gate.
- Effort: L+. Risk: High (a reader left on the DB is a silent staleness bug).

### Option B: validators checkpoint, RPC nodes keep per-block writes (D10 c)

- The checkpoint interval is a node-local knob: unset / 1 = exactly today (every block durable).
  Validators run ~30 s; the RPC / explorer node (needed anyway: testnet runs one non-validator node with
  trade history on) runs 1 and keeps today's reader behaviour.
- On validators only the readers that matter for consensus and liveness move to the view: ingress
  (sessions, min-collateral, EVM nonce, markets, band, duty exemption), the epoch-plan check (in-memory
  applied height), the state-hash monitor. RPC on a validator is allowed to lag by one interval
  (documented; validators are not the public RPC).
- Files: as A minus most of `torus-rpc`.
- Trade-offs: much smaller reader surface; RPC on validators lags (user-visible only if someone
  points a wallet at a validator). Reverses owner decision D10 a.
- Effort: L. Risk: Medium.

### Option C: staged, B now, A later only if needed (recommended)

- Step 1: core behind the interval knob, default 1 = byte-identical today; all existing suites green
  at interval 1, new crash / replay suites at interval > 1.
- Step 2: validator readers (B's list) onto the view; "never fires" interval vs interval 1 differential
  for ingress / consensus / state-hash suites.
- Step 3: gate campaign tw vs tw + interval 30 s (CPU, disk writes, restart -> ready).
- Step 4 (only if the owner wants validator RPC fresh): A's RPC migration, separately reviewed.
- Trade-offs: the gate is measured after step 3, before the costly reader work; A's work is only
  done if it is wanted. Same reversal of D10 a as B, but kept open.
- Effort: L (A's extra L+ deferred). Risk: Medium.

## Recommendation

**Option C.** CPU gain is capped at ~7%, so the reader migration should not be built before the gate
says Phase 3 pays. The core and the validator readers are needed in every option; RPC freshness on
validators is the only part that waits.

Design points carried by every option:
1. Since-checkpoint layer sits where the parent `FrozenPending` sits today, but spans S+1..n (merge of
   frozen block sets, last value per key). Two layers during a checkpoint write: the one being written
   stays readable until its batch is durable, so E never stalls on W.
2. Checkpoint = one atomic batch (layer + marker S + `h_S` + attest checkpoints in range), on W.
   Triggers: interval of execution time (setting, default 15 s; D6), layer size cap, shutdown, and before EVM / slash / epoch-boundary
   blocks (today's barriers; D11 a).
3. Running hash: h(n-1) in memory on E; `chain_step` no longer reads META. `h_n` must be identical to
   today on the same blocks at any interval (existing `running_hash_*` tests extended).
4. Replay: `replay_committed` from S; bodies come from `CF_BLOCK_BODIES` (already written at commit).
5. Pruner cutoff clamped to min(S, oldest kept snapshot) (also fixes today's unclamped cutoff).
6. Attest checkpoints: the state-hash monitor reads `h` from memory (the view), so `AttestStateHash`
   submission is not delayed by the interval.

## Not Building (YAGNI)

- Per-block change-set log file (D5 c): only if restart with re-execution is too slow.
- Own snapshot format (D7 b), snapshot trust LtHash (D8 b), P2P snapshot serving (D9 b).
- Automatic snapshot creation: `SnapshotManager` exists but is unwired; wire it only with the
  bootstrap work, not for the gate.
- Nonce windowing (D12 b): consensus change, before Phase 5.
- In-memory EVM state (D11 c): EVM blocks force a checkpoint.

## Owner decisions (s109, 2026-10-11)

- D13 trade-history retention pruning: stays in Phase 3 as a small separate step (matters only on the
  RPC node; validators run trade history off).
- Checkpoint interval is a node-local setting, starting default 15 s of execution (D6 b with a shorter
  start; owner may raise it to 30 s later). Worst-case restart ~16.5 s today + up to 15 s replay =
  ~31 s, under the 60 s gate. archy's sizing (coalescing factor at 5 / 15 / 30 s, layer size, restart
  budget) decides whether 30 s is worth it.

- Option C chosen (D10 a -> c for now): validators checkpoint, validator RPC may lag by up to one
  interval; RPC nodes keep interval 1. A's RPC migration (step 4) only if validators must serve public
  RPC or RPC nodes need the savings.
