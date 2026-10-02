# Consensus bug (a): equivocation slashes come from local observation

Status: fix implemented on `fix/consensus-determinism` (stop applying locally
observed slashes). On-chain evidence path: OWNER DECISION (see "Decisions").
Line numbers are for `main` @ d623d3c.

## Problem

A leader equivocation (two proposals for the same view) is seen only by the
replicas that happen to receive both proposals. On `main` each such replica
buffers a 5% slash + tombstone for the leader in node-local memory and writes it
to the state DB when it executes the NEXT block it dispatches. Replicas that did
not see both proposals never write it. Same committed chain, different state:
stake, `cf_slash_records`, the leader's validator row (tombstoned or not), and
from the next epoch on the validator set itself.

## Evidence (main @ d623d3c)

- `crates/hotstuff_rs/src/hotstuff/implementation.rs:1209-1263` (proposal) and
  `:2150-2181` (header): equivocation is detected from the proposals this
  replica received; `app.on_speculative_rollback` is called only if the first
  block was speculatively committed on this replica.
- `crates/hotstuff_rs/src/hotstuff/types.rs:614-623`: `EquivocationEvidence` is
  `{view, leader, block_a, block_b}` - two hashes, no leader signatures. It cannot
  be verified by another node (proposals are not leader-signed messages;
  `messages.rs:176-190`), so it is not transferable evidence.
- `crates/torus-consensus/src/app.rs:5225-5266` (`on_speculative_rollback`):
  pushes `PendingSlash { 500 bps, DoubleSign, tombstone }` into
  `self.pending_slashes` (node-local memory, lost on restart).
- `app.rs:5214-5221` (`on_committed_block`): drains `pending_slashes` into
  whichever block this replica commits next.
- `app.rs:1579-1615` (`execute_committed_block_with`): applies them at the start
  of that block's execution through a separate overlay + `commit_tx` - its own
  DB write, outside the block's flush batch.

## Options

1. **Stop applying locally observed slashes** (no protocol change). Keep the
   detection, the rollback, the persisted evidence (`store_equivocation_evidence`)
   and the logs; never mutate consensus state from them.
   + Deterministic immediately; no state-format or wire change; no activation
     height needed (blocks never carried these slashes, so replay from genesis
     is unaffected).
   - An equivocating leader is not punished until an on-chain path exists.
2. **On-chain evidence action** (`NativeAction::SubmitEquivocationEvidence`,
   appended last like `AttestStateHash`): anyone may submit; execution verifies
   the evidence against the offender's consensus key from chain state, records
   it once (dedup by `(offender, view)`), slashes + tombstones inside the
   block's flush batch.
   + Deterministic, accountable, Cosmos/CometBFT-style.
   - Needs evidence that a third party can VERIFY. Today only `PhaseVote`s are
     signed (borsh `(chain_id, view, block, phase)`, `messages.rs:263-290`);
     proposals are not. Proposal equivocation therefore needs a hotstuff_rs
     protocol change (leader signature over `(chain_id, view, block_hash)` in
     `Proposal` / `ProposalHeader`), or the slashing rule has to be "two
     conflicting signed votes" (needs a vote observer hook in hotstuff_rs; the
     `DoubleSignDetector` in `slashing.rs` exists but is not wired, and its
     message encoding `chain_id||view||hash||phase` big-endian does not match
     hotstuff's borsh encoding, so it could not verify real votes as is).
   - New action variant = wire format + EIP-712 type + client fixtures + an
     activation height (old binaries cannot decode it).
3. **Slash through a quorum**: each validator votes "I saw equivocation by X in
   view v" as an action; slash at > 2/3 stake. Deterministic but trusts
   observation rather than cryptographic proof; weaker than 2.

## Recommendation

Do 1 now (implemented). Do 2 as a separate feature after the owner decides the
evidence type. It cannot be finished safely here: the evidence producer lives in
hotstuff_rs (another agent's liveness work is in flight there), and a verifier
without a producer would be dead code.

## Decisions needed from the owner

- D-a1: evidence type for on-chain slashing: signed proposals (hotstuff_rs
  wire change) vs conflicting signed votes (vote observer hook) vs both.
- D-a2: penalty (today's 5% + tombstone, `slash_fraction_bps` 500) and whether
  the submitter is rewarded.
- D-a3: activation height for the new action (chain-wide genesis/config field,
  like `state_hash_activation_height`).

## Upgrade / activation implications

Option 1: none. No new field, no state-format change. A mixed fleet (old binary
slashes on observation, new binary does not) diverges exactly as two old
binaries already can; upgrade all validators together, as for every release.

Option 2 (later): new `NativeAction` discriminant + canonical tag + EIP-712
type, rejected before a chain-wide activation height; evidence-record CF or
`cf_slash_records` key so the record is hashed by the running state hash.

## Tests

- RED on main / GREEN after:
  `app::crash_recovery_tests::equivocation_observed_by_one_node_does_not_change_its_state`
  - two nodes execute the same committed chain; one of them observed an
  equivocation (`on_speculative_rollback`) before committing; their consensus
  state (validator rows, slash records, running state hash) must be identical.
- Regression: existing running-state-hash fixture still covers a slash written
  through the exec path (`running_hash_covers_out_of_batch_consensus_writes`),
  since the plumbing stays for the future on-chain path.
