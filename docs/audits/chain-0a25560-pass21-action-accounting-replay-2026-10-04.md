# Pass 21 — cross-path action accounting and replay

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. This is the fifth of five
additional focused passes requested after pass 16. It compares proposal
admission, committed execution and boot replay for native and EVM action counts.

**Finding candidate: an EVM transaction-count mismatch can make crash replay
skip committed EVM transactions.** The voting/insertion checks compare the
native-action body count to the header, but do not compare the EVM transaction
vector length with `header.evm_tx_count`. Live execution decides whether to
execute EVM work from the actual vector; crash replay decides whether a body is
needed from the two header counts. A certified block whose EVM vector is
non-empty while both header counts are zero can therefore execute its EVM
transactions live, then be treated as empty during recovery and have its
applied-height marker advanced without replaying those effects.

## Evidence and impact

- [`check_proposal_data`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L5068)
  enforces `native_actions.len() == header.native_action_count` for full blocks
  and `native_action_hashes.len() == header.native_action_count` for compact
  blocks. The compact EVM transactions are inline, but their length is not
  compared with `header.evm_tx_count`. `validate_block` delegates to the same
  body checks after its ancestry check.
- [`execute_committed_block_with`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L1721)
  sets `has_evm` from `!torus_block.evm_transactions.is_empty()` and passes
  those transactions to catch-up execution. [`validate_block_for_catchup`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/validator.rs#L102)
  decodes and executes that vector; the inspected code does not compare its
  length with the header count.
- [`persist_committed_block_durably_timed`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L947)
  persists the committed header and body together before execution dispatch, so
  the malformed count does not make the EVM body unavailable after a crash.
  However, [`replay_gap`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L1277)
  decides a block is empty solely when both header counts are zero and then
  synthesizes an empty body without loading the persisted body. This skips the
  EVM vector and lets replay proceed past that height.

This creates replica state divergence after a crash/restart or catch-up from a
node whose applied marker is below this committed height. It is consensus
relevant even if the malformed block was produced by a faulty or Byzantine
proposer: deterministic acceptance of an inconsistent header/body pair is
sufficient to expose honest replicas to different state after recovery.

## Recommended regression and correction

Require `evm_transactions.len() == header.evm_tx_count` alongside the native
count check for both full and compact proposals, and retain a committed-path
assertion before execution. During replay, do not classify a block as empty
based only on header counts: load the durable body and check both decoded vector
lengths against the committed counts, or use an explicit authenticated body
presence/empty marker. A regression should commit a block with a non-empty EVM
vector and zero EVM count, then simulate restart before execution; admission
must reject it, and recovery must never advance the applied marker while
silently omitting the transaction.

This is a source-derived candidate, not a runtime-reproduced finding. The
previous five reports did not document this count mismatch, so it is recorded as
new in pass 21.

## Limits

No Rust test or restart simulation was run. The schedule follows the static
callsites and persistence format described above; production reproduction and
patch validation remain outstanding. No source changes were made and no Torus
issue was written.
