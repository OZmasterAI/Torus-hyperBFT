# Consensus bug (c): EVM bundle commits in its own batch

Status: fix implemented on `fix/consensus-determinism` (one atomic batch).
Line numbers are for `main` @ d623d3c.

## Problem

A committed block with EVM transactions becomes durable in several separate
RocksDB writes:

1. every successful tx's writer-precompile side effects (CoreWriter / lockbox
   queue rows for height+1) - one `commit_tx` write PER TX, during execution;
2. the EVM bundle (accounts, storage, code, hashed mirror, incremental trie) -
   its own batch;
3. the native phase (native actions, CoreWriter drain, fee distribution, epoch
   rewards) together with the applied-height marker and the running state hash.

A crash between 2 and 3 restarts with the marker at height-1 and the bundle
already durable. Replay re-executes the block's EVM txs on top of their own
result. The catch-up path skips invalid txs one by one, so this is not
idempotent: txs that ran are now nonce-invalid and skipped (fee revenue becomes
0, so `distribute_fees` credits nothing), and a tx that was skipped the first
time (nonce too high) can become valid and run (burn + a second lockbox
credit). A crash between two writes of step 1 replays a tx whose queue row is
already durable (double credit with a fresh sequence number). An EVM block whose
execution errors part-way keeps the rows of the txs before the error.

## Evidence (main @ d623d3c)

- `crates/torus-evm/src/executor.rs:320-326` (`execute_block`) and `:463-469`
  (`execute_block_with_overlay`): `journal.commit_tx(state_db)` per successful tx.
- `crates/torus-state/src/backend.rs:879-898` (`commit_tx`): its own
  `WriteBatch` + `target.write`.
- `crates/torus-consensus/src/app.rs:1655-1673`: `commit_evm_bundle_incremental`
  (fallback `commit_pending_bundle`) - own batch (`incremental.rs:378-393`).
- `app.rs:2138-2175` (serial native flush with the marker, a later batch) and
  `app.rs:2365-2381` (`flush_marker_only` for EVM-only blocks, also later).
- `app.rs:1546-1574`: EVM blocks are never pipelined (always serial), so all of
  the above run on the exec thread in sequence.

## Options

1. **One atomic batch** (chosen). The writer journal becomes block-scoped
   (nothing touches the DB during EVM execution); the bundle's batch (plain +
   hashed mirror + incremental trie) and the writer rows are BUILT, not
   written, and become the prefix of the block's flush batch, ahead of the
   native writes and the applied-height marker. One `db.write`.
   + Crash anywhere before that write: nothing of the block is durable, replay
     re-executes it from the same base: identical result. After it: everything
     is durable, replay skips it.
   + The final key/value set of the block is the same as today (EVM writes
     first, native writes on top - a later op on the same key wins inside a
     `WriteBatch`), so the state root and the running state hash do not change.
   - Touches torus-evm / torus-state / torus-bridge / app.rs.
2. **Idempotent replay marker** (`META_EVM_APPLIED` in the bundle's batch, skip
   EVM on replay; the approach on the unmerged `fix/parity-audit-bugs`
   3fa99b6..b63d401). Keeps two batches.
   - Replay that skips EVM loses the block's EVM write set for the running state
     hash (the bundle is not reconstructable from the DB), so the replayed node
     computes a different `h(n)` and is hash-mismatched / fail-stopped. Would need
     the bundle's hash digest persisted too.
   - Needs a separate fail-stop path for "marker ahead", an incremental-trie
     resync on the fallback path, and storage-trie caveats (see that branch's
     review notes). More moving parts than 1.

## Recommendation

Option 1. Port the block-scoped writer journal from `fix/parity-audit-bugs`
b5ef142 (same API: `begin_tx` / `keep_tx` / `revert_tx` /
`append_pending_to_batch`, `BlockExecResult::native_writes`,
`ValidatedBlock::native_writes`), then fold the EVM batch into the flush batch
instead of writing it.

## Decisions needed from the owner

- D-c1: when `fix/parity-audit-bugs` (and the branches built on it:
  `feat/liquidation`, `feat/oracle-aggregation`, `feat/oracle-feeder`) merges,
  drop its `META_EVM_APPLIED` marker commits (3fa99b6, 99f8bdb, e9233d8,
  61e8a69, 6afb4e6, b63d401): with one batch there is no "EVM durable, native
  not" state to recover from. b5ef142 is ported here with the same API, so it
  should merge cleanly.

## Upgrade / activation implications

None. No state-format, wire or protocol change: the bytes written per block are
unchanged, only their grouping into one write. Root and running state hash are
unchanged (pinned by the existing running-hash tests). A node that crashed in
the window on an OLD binary may already hold a divergent state; this fix does
not repair it (resync from a snapshot or a peer).

## Tests

- RED on main / GREEN after (torus-consensus `crash_recovery_tests`, crash
  injected right after the EVM section via a `#[cfg(test)]` hook, restart =
  fresh exec context replaying from the durable marker, compared with a node
  that never crashed):
  - `evm_crash_between_evm_and_native_flush_replays_identically` - fee
    revenue (treasury / dev-pool credits) and every account/storage row equal;
  - `evm_crash_after_evm_section_skipped_tx_not_revived` - a block
    `[nonce 1, nonce 0]` (first skipped as nonce-too-high) must not run the
    nonce-1 tx on replay.
- Ported from b5ef142: `block_journal_tx_scopes_keep_and_revert` (torus-state),
  evm block path writes nothing before the commit (torus-evm).
- Existing running-hash tests (`running_hash_captures_evm_writer_precompile_side_effects_end_to_end`,
  `running_hash_covers_out_of_batch_consensus_writes`) must stay green
  unchanged.
