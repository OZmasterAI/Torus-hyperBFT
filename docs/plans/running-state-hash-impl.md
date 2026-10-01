# Implementation Plan: Running State Hash (on top of root skip)

Status: APPROVED by owner (s83, 2026-10-01). Owner direction:
follow Hyperliquid more closely — cover all consensus state, vote on-chain from
the start, build fail-stop but gated off. Code facts from a read-only map of
local `main` @ e4273ec; line numbers as of that commit.

## Why

Root skip (`TORUS_NATIVE_TRIE_MAINTENANCE=0`, e4273ec) removes the per-block
native trie: +12% matched/s at 100 markets, +31% at 300 (uniform load). What it
gives up is a per-block fingerprint of state. The running hash brings that back
at O(changed rows) cost. Tree maintenance stays a node-local option; the running
hash is computed by every node.

## How Hyperliquid does it (for reference)

Unofficial (reverse-engineered docs, lastdotnet.github.io/hyperliquid-rust-docs):
- Hash input = serialized **execution responses** (order results, fills, ...),
  not state: `rmp_serde -> blake3 XOF (2 KB) -> LtHash16` running accumulators
  (11 L1 + 3 EVM) since genesis; published hash = SHA-256 of the accumulators.
  Commits to history, not to the state set. Order-independent accumulation.
- Validators send a **`VoteAppHash` action every 2000 blocks** (~2.3 min at
  ~70 ms blocks); a 2/3-stake quorum on those votes detects divergence.
- Coverage: every core action type plus the EVM.
- Behaviour on mismatch (halt / stop voting / alert): **not found** in any
  source.
Official: consensus does not block on execution; state snapshots every 10,000
blocks (`periodic_abci_states`).

## Constraints found in our code

1. **No fixed exec lag** (exec queue up to 64 blocks + deferred,
   `rate_limit.rs:316`, `app.rs:5287-5290`); validators don't check state at
   vote time (`app.rs:4536-4704`) => the hash cannot be a header field; it
   travels as an on-chain action, like HL's `VoteAppHash`.
2. **Native write set** exists sorted in `flush_pending_with_native_trie_stats`
   (`torus-state/src/backend.rs:842`, `native_dirty_ref()` `:867`, sidecar
   merged `:871-873`), written in one batch with the applied-height marker
   (`:960-966`, `:972`). Old values are not available (rules out a cheap
   full-state LtHash).
3. **Write-set determinism risk**: `book_mode_marker_present`
   (`native_executor.rs:2882`) re-puts a `CF_NATIVE_MARKETS` row after every
   restart — identical state, different write set. A hash over writes would
   false-alarm on such rows unless they are made canonical.
4. **Crash/replay**: same-batch writes survive crashes atomically; replay
   re-runs `[applied+1, committed]` (`app.rs:3748-3781`); marker-only paths
   (`exec_pipeline.rs:366`, `app.rs:2311`, `app.rs:2316`) must advance the hash.
5. **Validator-only action pattern** exists: `JailVote`
   (`native_executor.rs:5479`, active-validator gate `staking.rs:524-530`,
   stake-weighted >2/3 tally `staking.rs:551-558`). Nothing in the node submits
   such actions automatically today.
6. **Fail-stop machinery** exists: `exec_failed` latch makes a node refuse to
   vote (`app.rs:4932`); `detect_conflicting_commit` (`app.rs:1107, 1476`).
7. `sha2 0.10` already in the workspace (not in torus-state); SHA-NI on host:
   ~1-4 ms per block for 10k-60k rows.
8. **3 validators**: a >2/3-stake quorum with 3 equal validators needs all 3.
   So (a) one diverged validator means no quorum hash is recorded for that
   checkpoint, and (b) one validator that stops voting halts consensus.
   Detection therefore must also compare individual votes, and fail-stop on a
   3-validator net trades a silent fork for a halt.

## Design

### Hash (design A, all consensus state)

    h_0 = 32 zero bytes at the activation height
    h_n = SHA-256( h_{n-1} ‖ n (u64 BE) ‖ D_native(n) ‖ D_evm(n) )
    D_x(n) = SHA-256 over the block's writes to domain x, sorted by
             (cf_id u8, key), each framed as
             cf_id ‖ len(key) u32 BE ‖ key ‖ (0x00 | 0x01 ‖ len(value) u32 BE ‖ value)

- **Covered: every consensus CF** (classified in Task 0). Expected set:
  native positions, balances, order books, markets, oracle, nonces, orders (if
  still live); staking validators, delegations, permanent, rewards; governance
  proposals and votes; fee config, treasury, dev pool; slash records, jail
  votes; sessions; core-writer queue; EVM accounts, storage, code.
- **Excluded** (derived or node-local): block headers/bodies/hash index,
  commit manifest, receipts, logs, bloom, tx index, `CF_BOOK_ORDER_ROWS`,
  trade CFs, DA pending/shards, `CF_CONSENSUS_META`, all trie/hashed CFs.
- **Storage-independent definition:** the hash is defined over each block's
  logical change set (consensus key → new value or deletion), not over a
  database write. Where it is captured (today the per-block flush batch,
  Task 3) and how it survives crashes (Task 4) may change later — e.g. with
  in-memory execution and periodic snapshots — without changing the hash.
- `cf_id` is a new frozen numbering for this hash (the existing native trie
  tags cover only 6 CFs).
- Computed by the flush path in the same batch as the applied-height marker;
  stored in `CF_CONSENSUS_META` (`META_RUNNING_STATE_HASH`) plus a checkpoint
  record every N blocks (keep the last 64).
- Commits to history, not the state set (like HL). A full-state set hash for
  snapshot verification (LtHash, needs old values) stays a later option.

### On-chain attestation (HL `VoteAppHash` model)

- New validator-only native action `AttestStateHash { height, hash }` for
  heights with `height % N == 0`, **N = 100** (~2 min at current block rates,
  same wall-clock order as HL's 2000 x ~70 ms).
- Execution (deterministic; never looks at the local hash): active-validator
  gate and stake-weighted tally like `JailVote`; stores each validator's vote
  per checkpoint height; when > 2/3 of stake agrees on one hash, records the
  **quorum hash** for that height. Votes and quorum records are consensus
  state (pruned to the last 64 checkpoints).
- Each validator node submits its own attestation automatically once its
  checkpoint is durable (new in-node submitter using the validator's account
  key).
- Block space: one action per validator per 100 blocks — negligible.

### Mismatch handling

- **Always on (side effect outside consensus state):** after executing a block
  that adds a vote for checkpoint h, each node compares every recorded vote and
  the quorum hash (if any) with its own checkpoint h. Any difference →
  `torus_state_hash_mismatch_total{validator}` metric and an ERROR log with the
  hashes. A checkpoint whose votes are all in but has no quorum → its own
  metric (`torus_state_hash_no_quorum_total`).
- **Fail-stop, gated OFF by default (`TORUS_STATE_HASH_FAILSTOP=1`):** if a
  quorum hash exists and differs from the local checkpoint, set the
  `exec_failed` latch so the node stops voting. Turn on only after testnet runs
  clean. On a 3-validator net this halts the chain instead of continuing on a
  fork (constraint 8) — document that trade-off when enabling it.

### Activation

`AttestStateHash` is a new action type and the vote state is consensus state
=> ships with the **coordinated testnet upgrade** (same as the per-user
open-order limit and the trade-history wipe).

## Decisions (owner, s83)

1. APPROVED: all consensus CFs, on-chain vote, fail-stop gated off, N = 100.
2. Open, decided when Task 1 runs: if Task 1 finds non-canonical writes that can't be fixed cheaply: exclude
   that CF from the hash (documented) vs switch to a full-state hash.

## Success Criteria

1. Same committed blocks => byte-identical `h_n` on every node: serial vs
   pipelined flush, crash + replay, restart (resident vs reloaded books, every
   BookMode), tree maintenance on vs off.
2. Node-local and derived CFs never change `h_n`.
3. Validators auto-attest every 100 blocks; the quorum hash is recorded
   on-chain; a deliberately diverged node raises the mismatch metric on every
   node within one checkpoint; with `TORUS_STATE_HASH_FAILSTOP=1` the diverged
   node stops voting.
4. Hashing < 5 ms per block at 300 markets uniform load; no throughput change
   beyond cell noise.
5. Existing tests pass (known pre-existing failures excepted).

## Tasks

### Task 0: Classify every CF and its writer paths
- **Output**: table in this doc: CF → consensus / derived / node-local, and
  every code path that writes it during block execution (native overlay dirty
  set, EVM state commit, direct `put_cf` in app.rs such as nonces at
  `app.rs:2038-2044`, staking/governance writers), and whether that write is in
  the same atomic batch as the applied-height marker.
- **Verify**: reviewed list; any consensus write outside the batch is flagged.
- **Depends on**: —

### Task 1: Write-set determinism gate
- **Test first**: replay the same committed blocks (a) straight through and
  (b) with a restart in the middle, for every BookMode, capturing each block's
  sorted writes to the consensus CFs from Task 0; assert identical per block.
- **Implementation**: test-only capture hook. Fix non-canonical writes (e.g.
  `book_mode_marker_present` re-put: write only when the value changes) or
  escalate per open decision 2.
- **Verify**: `cargo test -p torus-consensus running_hash_write_set_determinism`
- **Depends on**: 0

### Task 2: Hash function + frozen `cf_id` table
- **Test first**: golden vector (incl. tombstone, EVM write, and an excluded CF
  write that must not change the result); any byte change changes the hash.
- **Implementation**: `next_running_hash(prev, height, native_writes,
  evm_writes) -> [u8; 32]` in torus-state; `sha2` dependency.
- **Verify**: `cargo test -p torus-state running_hash`
- **Depends on**: 0

### Task 3: Capture all consensus writes and persist the hash in the batch
- **Test first**: flush N blocks → `META_RUNNING_STATE_HASH` matches the
  function block by block; checkpoints at `height % 100 == 0`, pruned to 64;
  tree maintenance on vs off identical; serial vs pipelined flush identical.
- **Implementation**: native writes from `native_dirty_ref()` after the
  sidecar merge; non-overlay consensus writes and EVM writes routed into the
  same batch (per Task 0) and fed to the hash; marker-only paths advance h.
- **Verify**: `cargo test -p torus-state -p torus-consensus running_hash`
- **Depends on**: 1, 2

### Task 4: Activation height + crash/replay
- **Test first**: fresh chain starts at h_0; an existing DB starts at its next
  applied height (recorded in META); kill after flushing N and between exec
  and flush → replayed hashes identical to an uncrashed run.
- **Verify**: `cargo test -p torus-consensus running_hash_activation running_hash_crash_replay`
- **Depends on**: 3

### Task 5: `AttestStateHash` action + tally
- **Test first**: non-validator attestation rejected; non-checkpoint height
  rejected; duplicate vote by one validator handled deterministically (first
  wins); >2/3 stake on one hash records the quorum hash; split votes record
  none; pruning after 64 checkpoints; serial vs parallel engine identical state.
- **Implementation**: new `NativeAction` variant (+ EIP-712 type), executor
  handler following `JailVote` (`native_executor.rs:5479`,
  `staking.rs:524-558`), new consensus CF or keys for votes/quorum (and add it
  to the hashed set from Task 0).
- **Verify**: `cargo test -p torus-types -p torus-bridge attest_state_hash`
- **Depends on**: 3

### Task 6: Auto-submission by validators
- **Test first**: a validator node with a configured account key submits
  exactly one attestation per checkpoint after the flush is durable, and
  re-submits after restart only if its vote is not yet on-chain.
- **Implementation**: small in-node task fed by the flush worker's checkpoint
  event; signs with the validator account key (config path); goes through
  the normal mempool path.
- **Verify**: `cargo test -p torus-consensus state_hash_autosubmit`
- **Depends on**: 5

### Task 7: Mismatch detection + gated fail-stop
- **Test first**: a differing vote → metric + ERROR on every node; quorum hash
  ≠ local checkpoint with `TORUS_STATE_HASH_FAILSTOP=1` → `exec_failed` latch
  set and the node refuses to vote; default (unset) never latches; all votes
  in without quorum → no-quorum metric; detection never changes consensus
  state (state dump identical).
- **Implementation**: post-execution side effect in app.rs reading the vote
  records and the local checkpoint; reuse the `exec_failed` latch
  (`app.rs:4932`); new counters in torus-telemetry.
- **Verify**: `cargo test -p torus-consensus state_hash_mismatch`
- **Depends on**: 5

### Task 8: RPC `torus_getStateHash`
- **Test first**: returns `{height, localHash, votes, quorumHash|null}` for
  retained checkpoints; error for pruned heights.
- **Verify**: `cargo test -p torus-rpc state_hash`
- **Depends on**: 5

### Task 9: Devnet drill + bench cost check
- Devnet (3 validators): corrupt one native row on val2 offline between
  restarts → mismatch metric on all three nodes within one checkpoint and no
  quorum hash; with fail-stop on and the corrupted node in the minority of a
  larger test set (or a forced quorum record in a unit test), the node stops
  voting; clean run → zero mismatches.
- Bench: uniform 100 / 300 markets with root skip, hash on, vs the s83
  root-skip cells; hashing ms/block and matched/s within noise.
- **Depends on**: 1-8

## Rollback

Before activation: code revert, no state effect. After activation: the vote
and quorum records are consensus state, so a revert needs a coordinated
upgrade; the local hash and checkpoints are node-local META and inert without
the code.

## Task 0 output: CF classification and writer paths (s83, base 710dde7)

Line numbers as of 710dde7. "Batch" = the one atomic `WriteBatch` that also
carries `META_NATIVE_APPLIED_HEIGHT` (`flush_pending_with_native_trie_stats`,
`backend.rs:643`; serial call `app.rs:2109`, flush worker `exec_pipeline.rs:372/426`).
Every block-execution write to a consensus CF goes through the per-block
`NativeStateOverlay` (native executor, core managers, staking, governance, nonces
`app.rs:2038-2044`, EVM account seed `app.rs:1801`) and so lands in the batch,
EXCEPT the rows marked **OUT** below.

| CF | class | writers during block execution | in batch? |
|---|---|---|---|
| cf_native_positions, cf_native_balances, cf_native_order_books, cf_native_oracle | consensus | native executor via overlay; deferred book save via sidecar (`exec_pipeline.rs:398`, merged into the same batch) | yes |
| cf_native_markets | consensus, except key `__book_mode__` (node-local wrong-flag marker, `native_executor.rs:2537`) | overlay (`save_order_books` tail `:2683-2703`, `save_order_books_deferred` `:2873-2891`, governance market params) | yes |
| cf_native_orders | consensus (no live writer found) | — | — |
| cf_native_nonces | consensus | overlay `app.rs:2038-2044` | yes |
| cf_staking_validators, cf_staking_delegations, cf_staking_permanent, cf_staking_rewards, cf_slash_records, cf_jail_votes | consensus | overlay (StakingManager over the overlay inside `NativeExecContext`) | yes |
|  |  | **OUT (a)** buffered equivocation slashes: `StakingManager<StateDb>::slash` + `tombstone_validator` straight to the DB at exec start (`app.rs:1532-1567`) | **no** |
|  |  | **OUT (b)** epoch rotation on the CONSENSUS thread: `apply_pending_rotations` (`app.rs:4094`) and `update_validator_statuses` (`app.rs:4138`, `epoch.rs:365`) from `build_proposal` / `finish_validate` (`app.rs:4722, 4845`), straight to the DB, at proposal/validation time of the boundary block, not at its execution | **no** |
| cf_governance_proposals, cf_governance_votes, cf_fee_config, cf_treasury, cf_dev_pool | consensus | overlay (governance, fee split, rewards, dev pool) | yes |
| cf_sessions | consensus | overlay (CreateSession/RevokeSession) | yes |
| cf_core_writer_queue | consensus | overlay (`drain_core_writer`) | yes |
| cf_consensus_meta | node-local (hotstuff block tree, markers, trie sentinel, trade format), EXCEPT key prefixes `pending_rotation:` (`staking.rs:1070`) and `validator_whitelist:` (`governance.rs:1491`) = consensus | consensus prefixes: overlay for submit / whitelist; **OUT (b)** for `apply_pending_rotations` deletes | partly |
| cf_accounts, cf_storage, cf_code (EVM) | consensus | **OUT (c)** EVM bundle: `commit_evm_bundle_incremental` (`app.rs:1599`, own batch, `incremental.rs:378`) or fallback `commit_pending_bundle` (`app.rs:1607`), on the exec thread BEFORE the native flush; accounts also re-put through the overlay (`seed_from_bundle`, `app.rs:1801`) + native fee/reward credits via overlay | accounts: overlay part yes, bundle part **no**; storage/code **no** |
| cf_block_headers, cf_block_bodies, cf_block_hash_to_number, cf_commit_manifest, cf_receipts, cf_logs, cf_logs_bloom, cf_tx_hash_to_location | derived | dispatch (`app.rs:5606`), `commit_block_metadata` (`app.rs:1615`), header fold into overlay (`app.rs:1807`), body put (`app.rs:2275-2289`) | mixed (excluded) |
| cf_native_trades, cf_native_user_trades | node-local | background trade writer / sync fallback (`app.rs:2229`) | no (excluded) |
| cf_book_order_rows | node-local (deterministic materialization) | overlay / sidecar | yes (excluded) |
| cf_native_pending, cf_native_shards | node-local (DA) | mempool / network | no (excluded) |
| cf_trie_nodes, cf_trie_accounts, cf_trie_storage, cf_hashed_accounts, cf_hashed_storage, cf_native_trie, cf_native_hashed | derived | EVM incremental commit, `resync_evm_accounts` (`app.rs:2167`), native trie in the batch | mixed (excluded) |
| cf_state_hash_votes (NEW, Task 5) | consensus | overlay (`AttestStateHash`) | yes |

### Out-of-batch consensus writes (flagged) and how they enter the hash

All three are applied before the block's flush, so the hash routes them into
**the same height's** `D(n)` as hash-only entries carried by the block's pending
set (never written by the flush; the DB write stays where it is). Merge rule:
these entries first, the overlay's own writes override per key (the overlay is
flushed later, so this is the block's final value per key).

- **(a) Slashes** (exec thread, serial path; pipelining is already disabled for
  blocks with slashes): the slash loop runs over a recording `StateBackend`
  that writes through to the DB and records every put/delete. *Pre-existing
  divergence source, not fixed here:* the slash comes from a local observation
  (`on_speculative_rollback`, `app.rs:5124-5160`) and is attached to whichever
  block the node dispatches next, so nodes can disagree on whether and where it
  is applied. The hash will report that as a mismatch, which is correct.
- **(b) Consensus-thread epoch rotation**: timing is not tied to execution, so
  the writes themselves cannot be attributed to a height. Deterministic point:
  executing the epoch-boundary block (already a pipeline barrier), the exec
  thread adds a snapshot of the full `cf_staking_validators` CF and all
  `pending_rotation:` META rows as puts to `D(n)` (O(validators), once per
  epoch). *Pre-existing hazard, not fixed here:* if execution lags by more than
  one epoch, the consensus thread's writes for the next boundary are visible to
  execution early; the hash would flag it.
- **(c) EVM bundle**: deterministic (exec thread, serial path). The bundle's
  plain-state writes (same mapping as `apply_bundle_plain`) are added to the
  EVM domain of `D(n)`. Crash window (pre-existing): the EVM batch can be durable
  while the native batch is not; replay then re-executes the block's EVM part on
  a base that already contains it.

Consensus writes outside block execution: none found (RPC, network, mempool and
node writes outside tests touch only DA / derived CFs). Genesis state is written
before activation and is not part of the hash (identical by genesis file).
