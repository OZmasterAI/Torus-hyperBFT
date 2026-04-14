# Torus-hyperBFT Consensus Safety Audit (3.4.2)

**Instance**: A — Consensus Safety (including MonadBFT extensions)
**Date**: 2026-04-14
**Scope**: hotstuff_rs, torus-consensus, torus-network, torus-bridge (partial)
**Lines Audited**: ~19,000 Rust LOC across 5 crates, 70+ files

---

## 1. Executive Summary

The Torus-hyperBFT consensus layer contains **7 critical**, **12 high**, **15 medium**, and **8 low** severity findings across the five crates in scope. The most severe issues cluster around three themes: **(1) MonadBFT NEC safety** — the `valid_nec` function does not verify that NEC signers abstained from voting for the high-tip block, breaking the core tail-fork resistance guarantee; **(2) Validator-Proposer execution asymmetry** — the proposer and validator execute structurally different native-action pipelines, and the `epoch_validator_set_updates` double-call produces inconsistent validator set diffs on the proposer's node, which can permanently halt that node at epoch boundaries; **(3) Missing validation** — native actions, receipts_root, logs_bloom, and base_fee_per_gas are never verified during consensus validation, allowing a Byzantine proposer to falsify these fields without detection. The slashing mechanism has a critical address-derivation mismatch that causes all slashes to silently fail, and the networking layer allows full sender-identity spoofing via unauthenticated payload bytes. These findings collectively undermine safety, liveness, and accountability guarantees. No code changes were made — this is an analysis-only audit.

---

## 2. Verified Pre-Findings

### CONS-PF-07 — NEC `valid_nec` Does Not Verify Non-Voting

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Critical** |
| **File** | `hotstuff_rs/src/hotstuff/types.rs:422-441` |

**Evidence**: `valid_nec()` performs exactly two checks: (1) `nec.high_tip_qc_view >= nec.view - 1` (line 427); (2) 2f+1 valid signatures over `(nec.view, nec.high_tip_qc_view)` (lines 432-440). The signed payload (`message_bytes` at line 450) is only `(view, high_tip_qc_view)` — it carries no commitment to non-voting.

**Root Cause**: The NEC safety property ("2f+1 validators who did NOT vote for high_tip") is enforced only at NE *issuance* (implementation.rs:1076-1088, which checks `last_voted_proposal`), never at *verification*. A Byzantine validator can vote for high_tip (contributing to a QC) AND sign an NE message, because the verifier cannot distinguish voters from non-voters.

**Additional defect**: Lines 433-437 contain a dead branch — both `if update_decided()` and `else` select `committed_validator_set()`, so PVS-only validators are never accepted as NEC signers during transitions.

**Recommended Fix**: Include the signer's `last_voted_proposal` commitment in the NE signed payload, or cross-reference the NEC signer set against the PhaseVote signer set for the same view at verification time.

---

### CONS-PF-08 — `on_speculative_rollback` Applies Irreversible Slashing

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Critical** |
| **File** | `torus-consensus/src/app.rs:319-367` |

**Evidence**: Lines 347-367 call `staking.slash(leader_addr, 500, SlashReason::DoubleSign, 0)` and `staking.tombstone_validator(&leader_addr)`, both of which write directly to RocksDB via `StakingManager`. These writes are permanent and non-reversible.

**Root Cause**: The function is named "speculative rollback" but performs a permanent, non-speculative slash. The slash bypasses the EVM state overlay and writes directly to the staking column family. There is no multi-node agreement before the slash is committed.

**Compounding issue**: See CONS-FIND-01 below — the address derivation is also broken, so the slash silently fails on a non-existent validator address.

**Recommended Fix**: Buffer the slash in a speculative overlay and commit only when the evidence block is finalized; or submit slashing evidence as a transaction in a future block, requiring consensus before application.

---

### CONS-PF-01 — `produce_empty_block` Sets `state_root: B256::ZERO`

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Critical** |
| **File** | `torus-consensus/src/app.rs:377-417` |

**Evidence**: Line 390: `state_root: B256::ZERO`. The fallback path is reached when `build_block` returns `Err` (line 279-281). Since the empty block has `evm_tx_count: 0` and `native_action_count: 0`, validators take the else branch at `do_validate:246-253` which returns `Valid` without checking state_root.

**Root Cause**: No state root verification exists for blocks without EVM transactions. The zero sentinel is accepted unconditionally.

**Recommended Fix**: Always compute and verify the actual state root, even for empty blocks.

---

### CONS-PF-02 — Native Actions Never Validated During Consensus

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED and BROADENED** |
| **Severity** | **Critical** |
| **File** | `torus-consensus/src/app.rs:230-253` |

**Evidence**: `do_validate` at line 231: `if !torus_block.evm_transactions.is_empty()` gates ALL validation on the presence of EVM transactions. The else branch at lines 246-253 accepts any block with no EVM transactions as valid, regardless of `native_actions`, `core_writer_actions`, `header.state_root`, `header.receipts_root`, `header.logs_bloom`, or `header.native_action_count`.

**Broadened Scope**: A Byzantine proposer can include arbitrary native actions (staking changes, delegations, governance proposals, order placements) with no EVM transactions and the block will pass validation with zero semantic checking.

**Recommended Fix**: Validate native actions independently of EVM transaction presence; verify state root for all block types.

---

### CONS-PF-09 — Bracha Timeout Amplification Uses Message Count, Not Voting Power

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **High** |
| **File** | `hotstuff_rs/src/pacemaker/implementation.rs:252-281` |

**Evidence**: Line 257: `*count += 1` increments by 1 per message. Line 258-260: threshold `f+1` is derived from `total_power`. The counter is cardinality-based while the threshold is power-based.

**Attack**: f+1 low-power validators (total stake well below f) can trigger timeout amplification at any honest replica.

**Recommended Fix**: Accumulate sender power instead of count.

---

### CONS-PF-10 — `new_view_recipients` Uses Non-Reputation Leader Selection

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **High** |
| **File** | `hotstuff_rs/src/hotstuff/roles.rs:256-274` |

**Evidence**: Lines 261 and 268 call `select_leader` (plain IWRR). Meanwhile `is_proposer_with_reputation` (line 89) uses `select_leader_with_reputation`. Part of a systematic 4-function mismatch.

**Recommended Fix**: Ensure all leader-selection call sites use the reputation-aware variant when reputation is enabled.

---

### CONS-PF-11 — ProposalResponse Block Not Validated Before Reproposal

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **High** |
| **File** | `hotstuff_rs/src/hotstuff/implementation.rs:1004-1055` |

**Evidence**: Checks only `resp.proposal.block.hash == tip.block_hash` (line 1025). Parameters `_block_tree` and `_app` suppressed with underscores (lines 1010-1011). Block broadcast at line 1048 without any `safe_block` or `validate_block` call.

**Recommended Fix**: Call `block.is_correct(block_tree)` and `safe_block()` before broadcasting.

---

### CONS-PF-12 — Gossip Sender Identity from Payload Bytes

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **High** |
| **File** | `torus-network/src/swarm.rs:320-351` |

**Evidence**: Line 331: sender parsed from `data[..32]`. Line 343: unverified `sender_vk` pushed to inbound queue. The authenticated `propagation_source` is used only for rate limiting.

**Recommended Fix**: Cross-check `sender_vk` against transport-authenticated identity.

---

### CONS-PF-13 — `receipts_root` and `logs_bloom` Never Verified

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **High** |
| **File** | `torus-bridge/src/validator.rs:43-89` |

**Evidence**: `validate_block` checks `gas_used` and `state_root` but never computes or compares `receipts_root` or `logs_bloom`. `ValidatedBlock` struct does not include these fields. Also: `receipts_root` is `keccak256(JSON)`, not Merkle trie (proposer.rs:240-246).

**Recommended Fix**: Recompute both fields during validation and compare against the header.

---

### CONS-PF-14 — `base_fee` Accepted Without EIP-1559 Recalculation

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **High** |
| **File** | `torus-bridge/src/validator.rs:52-58` |

**Evidence**: Line 57: `base_fee: block.header.base_fee_per_gas` — validator uses proposer-supplied value directly. A malicious proposer can set `base_fee_per_gas = 0` and all validators reproduce the same execution.

**Recommended Fix**: Recalculate `base_fee_per_gas` from the parent header during validation.

---

### CONS-PF-03 — `action_sort_key` Uses Debug Format

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Medium** |
| **File** | `torus-bridge/src/native_executor.rs:872` |

**Evidence**: `keccak256(format!("{:?}", action).as_bytes())`. `Debug` output not stable across Rust versions or crate updates. Can cause consensus divergence on toolchain upgrade.

---

### CONS-PF-04 — `epoch_length: 0` and `max_validators: 0` Hardcoded

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Medium** |
| **File** | `torus-bridge/src/proposer.rs:167-168`, `validator.rs:149-150` |

---

### CONS-PF-05 — `last_header` Updated Before Consensus Confirmation

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Medium** |
| **File** | `torus-consensus/src/app.rs:288` |

---

### CONS-PF-15 — `seen_proposals` HashMap Unbounded Growth

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/hotstuff/implementation.rs:86-88` |

---

### CONS-PF-16 — `bracha_timeout_counts` BTreeMap Unbounded Growth

| Field | Value |
|-------|-------|
| **Status** | **CONFIRMED** |
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/pacemaker/implementation.rs:562-564` |

---

## 3. New Findings

### CONS-FIND-01 — Slashing Address Derivation Mismatch: All Slashes Silently Fail

| Field | Value |
|-------|-------|
| **Severity** | **Critical** |
| **File** | `torus-consensus/src/app.rs:338-345` |

`on_speculative_rollback` derives `leader_addr` via `SHA-256(ed25519_pubkey)[12..32]`. However, `StakingManager::register_validator` takes the account's Ethereum address and a separate ed25519 consensus public key as independent parameters with no enforced relationship. `staking.slash(leader_addr, ...)` returns `Err(ValidatorNotFound)` at line 360. `tombstone_validator` also fails. **No equivocating validator is ever actually slashed.**

---

### CONS-FIND-02 — `epoch_validator_set_updates` Double-Call Causes Proposer Halt

| Field | Value |
|-------|-------|
| **Severity** | **Critical** |
| **File** | `torus-consensus/src/app.rs:288-291` + `hotstuff_rs/src/hotstuff/implementation.rs:273-274` |

The proposer calls `epoch_validator_set_updates` during `produce_block` (line 291), mutating `self.last_validator_set`. `implementation.rs:274` discards the result: `validator_set_updates: _`. When `validate_block` runs, the second call sees already-mutated state and returns `None`. The proposer's block tree stores `validator_set_updates = None` while all other validators store `Some(real_updates)`. The proposer **permanently halts** at epoch boundaries due to `safe_pc` check failure at `invariants.rs:361-364`.

---

### CONS-FIND-03 — Proposer-Validator Native Execution Structural Divergence

| Field | Value |
|-------|-------|
| **Severity** | **Critical** (latent) |
| **File** | `torus-bridge/src/proposer.rs:174-197` vs `validator.rs:187-207` |

The proposer executes: pre-EVM -> EVM -> post-EVM -> state root. The validator additionally runs **drain_core_writer** (line 191), **process_governance** (line 196), **distribute_fees** (line 199), **process_epoch_boundary** (line 202) before computing state root. When any of these phases has state effects, `StateRootMismatch` rejects every block. Comment at proposer.rs:120-121 acknowledges the asymmetry.

---

### CONS-FIND-04 — Direct Message `sender_key` Unauthenticated

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `torus-network/src/swarm.rs:227-228`, `codec.rs:10-13` |

`DirectRequest.sender_key` is a self-reported field. The authenticated `peer` identity is never cross-checked. Same class of vulnerability as CONS-PF-12 for the unicast path.

---

### CONS-FIND-05 — Block Sync Server Uses `max()` Instead of `min()`

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `hotstuff_rs/src/block_sync/server.rs:122` |

`max(limit, self.config.request_limit)` — a client requesting `limit = u32::MAX` forces the server to fetch billions of blocks. The intended function is `min()`.

---

### CONS-FIND-06 — Unbounded Sync Loop Blocks Algorithm Thread

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `hotstuff_rs/src/block_sync/client.rs:300-449` |

No maximum iteration count or total session deadline. A malicious server sending valid non-empty batches indefinitely holds the entire algorithm thread, preventing all consensus message processing.

---

### CONS-FIND-07 — TimeoutVote `local_tip` and `highest_qc` Are Unsigned

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `hotstuff_rs/src/pacemaker/messages.rs:134-143` |

`message_bytes()` signs only `(chain_id, view)`. The `local_tip` and `highest_qc` fields can be substituted by a MITM without breaking signature verification. These fields influence TC construction and locking decisions.

---

### CONS-FIND-08 — `DoubleSignDetector::record_vote` Accepts Forged Votes

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `torus-consensus/src/slashing.rs:138-177` |

Stores votes without signature verification (line 170). A forged vote poisons the detector slot. When the real vote arrives, evidence is generated with a forged signature that fails `verify()`. The actual double-sign escapes detection.

---

### CONS-FIND-09 — `apply_pending_rotations` Permanently Deletes During `produce_block`

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `torus-consensus/src/app.rs:99-100` -> `torus-economics/src/staking.rs:896` |

Key rotations for the epoch are deleted from RocksDB during `produce_block` (line 291), before the block is confirmed. If the block is never committed, the rotations are permanently lost.

---

### CONS-FIND-10 — Inbound Message Queue Unbounded `VecDeque`

| Field | Value |
|-------|-------|
| **Severity** | **High** |
| **File** | `torus-network/src/swarm.rs:41, 228, 277, 343` |

No capacity bound, max-size check, or eviction policy. With 100 peers at 50 msgs/sec, worst-case growth is 1.28 GB/sec.

---

### CONS-FIND-11 — NE Messages Routed to Wrong Leader

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/hotstuff/implementation.rs:1104-1110` |

Uses `select_leader` (plain IWRR) instead of reputation-weighted variant. Part of the systematic IWRR/reputation mismatch (PF-10, FIND-12, FIND-13).

---

### CONS-FIND-12 — `enter_view` Uses Plain `is_proposer`

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/hotstuff/implementation.rs:199-202` |

---

### CONS-FIND-13 — `phase_vote_recipient` Uses Plain Leader Selection

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/hotstuff/implementation.rs:682`, `roles.rs:219-240` |

---

### CONS-FIND-14 — CVS/PVS Vote Double-Counting in `ActiveCollectorPair`

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/types/signed_messages.rs:257-271` |

A validator present in both sets can have their vote counted in both collectors across separate `collect` invocations. Both collectors can independently reach quorum for different blocks.

---

### CONS-FIND-15 — `valid_nec` Dead Branch During Validator Set Transition

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/hotstuff/types.rs:433-437` |

Both `if update_decided()` and `else` arms select `committed_validator_set()`. PVS-only NE signers are never accepted during transitions.

---

### CONS-FIND-16 — Non-Atomic Write of `highest_view_voted` / `last_voted_proposal`

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/hotstuff/implementation.rs:686-688` |

Two separate KVStore writes. Crash between them can cause a validator to send NE for a block it actually voted for.

---

### CONS-FIND-17 — `HashSet::difference` Non-Deterministic in Epoch Rotation Cap

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `torus-economics/src/epoch.rs:163-185` |

Rust's randomized SipHash causes `HashSet::difference` iteration order to vary between nodes. `.skip(allowed_swaps)` selects different validators on different machines, causing consensus divergence.

---

### CONS-FIND-18 — TC in AdvanceView Rejected for Non-Epoch Views

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/pacemaker/implementation.rs:399-405` |

Liveness gap: lagging replicas cannot catch up via TC-based AdvanceView for non-epoch views.

---

### CONS-FIND-19 — Rate Limiter Tumbling Window Allows 2x Burst

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `torus-network/src/peer_scoring.rs:61-73` |

Hard-reset tumbling window. A peer achieves 2x burst at window boundary (100 msgs in ~1ms with default config).

---

### CONS-FIND-20 — Division by Zero if `epoch_length = 0`

| Field | Value |
|-------|-------|
| **Severity** | **Medium** |
| **File** | `hotstuff_rs/src/pacemaker/implementation.rs:804-806` |

`view.int() % (epoch_length as u64)` panics unconditionally if `epoch_length = 0`. No input validation exists.

---

### CONS-FIND-21 through CONS-FIND-24 — Unbounded Data Structures and Missing Limits

| ID | Severity | File | Description |
|----|----------|------|-------------|
| CONS-FIND-21 | Medium | block_sync/client.rs:480 | `available_sync_servers` HashMap unbounded |
| CONS-FIND-22 | Medium | implementation.rs:85,1111 | `ne_sent_views` HashSet unbounded |
| CONS-FIND-23 | Medium | torus-network/src/sync.rs:8 | Sync protocol `count: u64` unvalidated |
| CONS-FIND-24 | Medium | swarm.rs:253-261 | Ban race window at ConnectionEstablished |

---

### CONS-FIND-25 through CONS-FIND-32 — Low/Informational

| ID | Severity | File | Description |
|----|----------|------|-------------|
| CONS-FIND-25 | Low | pacemaker/implementation.rs:327-332 | Operator precedence bug bypasses `is_validator` in AdvanceView |
| CONS-FIND-26 | Low | pacemaker/types.rs:184-186 | TipInfo in TimeoutVote unvalidated |
| CONS-FIND-27 | Low | implementation.rs:692-710 | `local_tip` not updated for Nudge votes |
| CONS-FIND-28 | Low | implementation.rs:354-370 | ProposalRequest targets same k validators deterministically |
| CONS-FIND-29 | Info | peer_scoring.rs:208-238 | Synchronous ban file I/O blocks event loop |
| CONS-FIND-30 | Info | native_executor.rs:289-295 | `cancel_order` has no sender ownership check |
| CONS-FIND-31 | Info | decode.rs:63-90 | `access_list` not propagated to TxEnv |
| CONS-FIND-32 | Info | data_types.rs:408-421 | ViewNumber arithmetic has no overflow protection |

---

## 4. MonadBFT Deep Analysis

### NEC Safety Property

**Does the implementation satisfy it?** **No.**

The NEC safety property requires: "If an NEC exists for view v, then the high_tip block could NOT have been committed, because 2f+1 non-voting validators signed the NEC, and any QC requires 2f+1 signatures, so both sets cannot coexist (they would require >n total signatures)."

The implementation breaks this in three ways:

1. **Verification gap (CONS-PF-07)**: `valid_nec` does not verify non-voting. A Byzantine validator can sign both a PhaseVote AND an NE message. If f Byzantine validators do this, plus f+1 honest non-voters sign the NE, the NEC has 2f+1 signatures. Simultaneously, f Byzantine voters plus f+1 honest voters form a QC. Both coexist, breaking the exclusivity invariant (Property 9).

2. **Crash recovery gap (CONS-FIND-16)**: Non-atomic writes of `highest_view_voted` and `last_voted_proposal` mean a crash can cause a validator to send an NE for a block it actually voted for.

3. **Dead branch (CONS-FIND-15)**: During validator set transitions, NE signers from the PVS are silently rejected, potentially preventing NEC formation when most needed.

### Speculative Finality

**Can a speculatively committed block be rolled back safely?** **No.**

- `on_speculative_rollback` applies permanent slashing to RocksDB (CONS-PF-08)
- Address derivation is incorrect — slash silently fails (CONS-FIND-01)
- No EVM state rollback implemented (comment at app.rs:332-334 acknowledges this)
- `last_header` not reset on rollback (CONS-PF-05)

### Tail-Fork Resistance

**Is the implementation actually resistant?** **Partially, with significant gaps.**

- Verification gap (CONS-PF-07) allows Byzantine NEC to coexist with valid QC
- ProposalResponse block not validated (CONS-PF-11) — malformed block wastes recovery view
- ProposalRequest targets deterministic set (CONS-FIND-28) — predictable withholding target

### Leader Reputation

**Can it be gamed?** **Yes.**

1. **Routing mismatch**: 4 of 6 leader-selection call sites use plain IWRR while 2 use reputation. When reputation is active, the system is effectively non-functional.
2. **Minimum power floor**: `max(1, ...)` prevents full exclusion of failing validators.
3. **Timeout manipulation**: Adversary forcing view timeouts degrades honest leader reputation via `record_leader_timeout`.

---

## 5. Quorum & Vote Counting Verification

### Quorum Formula

Formula: `quorum = floor(total_power * 2 / 3) + 1` (validator_set.rs:157-170, signed_messages.rs:142-153). Uses `checked_mul(2)` for overflow safety.

| n | Powers | total_power | quorum | Fraction | Correct? |
|---|--------|-------------|--------|----------|----------|
| 4 | [1,1,1,1] | 4 | 3 | 75.0% | Yes |
| 7 | [1,1,1,1,1,1,1] | 7 | 5 | 71.4% | Yes |
| 13 | [1..1] | 13 | 9 | 69.2% | Yes |
| 100 | [1..1] | 100 | 67 | 67.0% | Yes |
| 3 | [1,1,1] | 3 | 3 | 100% | Over-conservative (safe) |
| 4 | [1,2,3,4] | 10 | 7 | 70.0% | Yes |

**No off-by-one errors. Overflow-safe. Formula is slightly conservative for n divisible by 3.**

### CVS->PVS Transition (CONS-FIND-14)

A vote can be counted in both CVS and PVS collectors when a validator appears in both sets. Both collectors can independently reach quorum for different blocks.

---

## 6. Liveness Analysis

### Can a Minority of Byzantine Validators Halt Progress?

**Yes, through multiple vectors:**

1. **Bracha amplification (CONS-PF-09)**: f+1 low-power validators trigger timeout amplification with stake well below threshold.
2. **NEC formation deadlock (CONS-FIND-11, CONS-FIND-15)**: NE messages routed to wrong leader; PVS-only validators rejected.
3. **Reputation mismatch (PF-10 + FIND-11-13)**: Complete liveness failure when reputation is enabled — no votes, NewViews, or NE messages reach the correct leader.
4. **Epoch boundary halt (CONS-FIND-02)**: Proposer permanently halts at epoch boundaries.

### Timeout Escalation

Static `max_view_time` with no exponential backoff. Bracha amplification at f+1 messages accelerates propagation. Count-vs-power mismatch causes premature amplification but does not prevent eventual convergence.

### View Synchronization Failures

1. **TC AdvanceView epoch gating (CONS-FIND-18)**: Lagging replicas cannot catch up via TC for non-epoch views.
2. **Sync stall (CONS-FIND-06)**: Unbounded sync loop holds algorithm thread indefinitely.
3. **Epoch halt (CONS-FIND-02)**: Proposer stuck at epoch boundary while others advance.

---

## 7. Severity Classification

### Critical (7)

| ID | Summary |
|----|---------|
| CONS-PF-07 | NEC `valid_nec` doesn't verify non-voting — breaks tail-fork resistance |
| CONS-FIND-01 | Slash address derivation mismatch — all slashes silently fail |
| CONS-FIND-02 | Epoch double-call — proposer halts at epoch boundaries |
| CONS-PF-01 | `produce_empty_block` state_root=ZERO accepted by validators |
| CONS-PF-02 | Native actions never validated — arbitrary staking/governance bypasses consensus |
| CONS-PF-08 | `on_speculative_rollback` applies irreversible slash to RocksDB |
| CONS-FIND-03 | Proposer-Validator native execution divergence — latent state root mismatch |

### High (12)

| ID | Summary |
|----|---------|
| CONS-PF-09 | Bracha timeout counts messages not power |
| CONS-PF-10 | NewView routed to wrong leader (reputation mismatch) |
| CONS-PF-11 | ProposalResponse block not validated before broadcast |
| CONS-PF-12 | Gossip sender identity spoofable from payload bytes |
| CONS-PF-13 | receipts_root and logs_bloom never verified |
| CONS-PF-14 | base_fee accepted without EIP-1559 recalculation |
| CONS-FIND-04 | Direct message sender_key unauthenticated |
| CONS-FIND-05 | Block sync server max() vs min() — attacker controls response size |
| CONS-FIND-06 | Unbounded sync loop blocks algorithm thread |
| CONS-FIND-07 | TimeoutVote local_tip/highest_qc unsigned — modifiable post-signing |
| CONS-FIND-08 | DoubleSignDetector accepts forged votes — slot poisoning |
| CONS-FIND-09 | apply_pending_rotations permanently deletes during produce_block |

### Medium (15)

| ID | Summary |
|----|---------|
| CONS-PF-03 | Debug format for consensus-critical sort key |
| CONS-PF-04 | epoch_length=0, max_validators=0 hardcoded |
| CONS-PF-05 | last_header updated before confirmation |
| CONS-PF-15 | seen_proposals unbounded |
| CONS-PF-16 | bracha_timeout_counts unbounded |
| CONS-FIND-11 | NE messages routed to wrong leader |
| CONS-FIND-12 | enter_view uses plain is_proposer |
| CONS-FIND-13 | phase_vote_recipient uses plain leader |
| CONS-FIND-14 | CVS/PVS double-counting |
| CONS-FIND-15 | valid_nec dead branch |
| CONS-FIND-16 | Non-atomic KV writes (crash-recovery NEC gap) |
| CONS-FIND-17 | HashSet::difference non-deterministic |
| CONS-FIND-18 | TC AdvanceView epoch-gating liveness gap |
| CONS-FIND-19 | Rate limiter 2x burst |
| CONS-FIND-20 | Division by zero if epoch_length=0 |

### Low (8) / Informational (4)

| ID | Summary |
|----|---------|
| CONS-FIND-21-24 | Unbounded data structures, ban race, sync limits |
| CONS-FIND-25-28 | Precedence bug, TipInfo unvalidated, stale local_tip, deterministic ProposalRequest |
| CONS-FIND-29-32 | Sync I/O blocking, cancel_order auth, access_list, ViewNumber overflow |

---

## Appendix A: Checklist Results Summary

| ID | Result | Evidence |
|----|--------|----------|
| CHK-01 | PASS | types.rs:149-187 — signature alignment correct |
| CHK-02 | PASS | types.rs:340-393 — conflicting votes in separate buckets (by design) |
| CHK-03 | PASS | validator_set.rs:157-170 — quorum formula correct, overflow-safe |
| CHK-04 | FINDING | signed_messages.rs:257-271 — CVS/PVS double-counting |
| CHK-05 | PASS | pacemaker/implementation.rs:737-770 — power=0 handled |
| CHK-06 | OBSERVATION | implementation.rs:780-801 — floor at 1 prevents exclusion |
| CHK-07 | FINDING | roles.rs:89-114 — phase_vote_recipient no reputation |
| CHK-08 | FAIL | roles.rs:256-274 — confirmed PF-10 |
| CHK-09 | FAIL | implementation.rs:252-260 — count vs power |
| CHK-10 | PASS/CONCERN | implementation.rs:399-405 — epoch OK, non-epoch liveness gap |
| CHK-11 | PARTIAL PASS | implementation.rs:463-503 — guard exists |
| CHK-12 | FAIL | implementation.rs:562-564 — no pruning |
| CHK-13 | PASS | invariants.rs:518-543 — 2-chain commit correct |
| CHK-14 | PASS | invariants.rs:550-570 — safety via safe_nudge |
| CHK-15 | PASS | invariants.rs:351-365 — genesis edge case handled |
| CHK-16 | PASS | invariants.rs:611-623 — depth 0/1/2 correct |
| CHK-17 | FAIL | types.rs:422-441 — NEC safety not enforced |
| CHK-18 | PARTIAL PASS | types.rs:553-556 — bound at collection |
| CHK-19 | PASS (gap) | implementation.rs:686-688 — persisted, non-atomic |
| CHK-20 | CONFIRMED | implementation.rs:354-370 — deterministic |
| CHK-21 | FAIL | implementation.rs:1004-1056 — no validation |
| CHK-22 | PASS | types.rs:481-500 — NEC satisfies is_fresh_proposal |
| CHK-23 | WARNING | app.rs:191 — default no-op rollback |
| CHK-24 | FAIL | implementation.rs:86-88 — never pruned |
| CHK-25 | FAIL | implementation.rs:85,1111 — never pruned |
| CHK-26 | PASS | receiving.rs:154 — chain_id filter |
| CHK-27 | PASS | receiving.rs:300 — highest-view evicted |
| CHK-28 | PASS | implementation.rs:563 — view slot burned |
| CHK-29 | FAIL | app.rs:347-367 — irreversible slash |
| CHK-30 | FAIL | app.rs:291,238 — double-call divergence |
| CHK-31 | FAIL | slashing.rs:138-177 — no sig verification |
| CHK-32 | PASS | slashing.rs:180-197 — min_view guard |
| CHK-33 | RESULT | app.rs:124 — old set size, edge at n<=2 |
| CHK-34 | FAIL | app.rs:91-197 — not deterministic |
| CHK-35 | FAIL | swarm.rs:331 — payload bytes |
| CHK-36 | FAIL | swarm.rs:227 — self-reported sender_key |
| CHK-37 | COND PASS | peer_scoring.rs:127 — no practical underflow |
| CHK-38 | FAIL | peer_scoring.rs:64 — tumbling window |
| CHK-39 | PARTIAL PASS | behaviour.rs:42 — 256KB match, TX mismatch |
| CHK-40 | FAIL | swarm.rs:41 — unbounded VecDeque |
| CHK-41 | FAIL (design) | swarm.rs:276-281 — self-delivery bypasses checks |
| CHK-42 | FAIL | validator.rs:43-89 — receipts_root not verified |
| CHK-43 | FAIL | proposer.rs:240-246 — keccak256(JSON) |
| CHK-44 | FAIL | proposer.rs:167, validator.rs:149 — hardcoded 0 |
| CHK-45 | FAIL | validator.rs:57 — base_fee verbatim |
| CHK-46 | FAIL | validator.rs:60-88 — logs_bloom not verified |

---

## Appendix B: Property Invariant Assessment

| # | Property | Status | Key Evidence |
|---|----------|--------|--------------|
| 1 | Safety: identical commits at height h | **AT RISK** | NEC/QC coexistence (PF-07), non-deterministic epoch cap (FIND-17) |
| 2 | Liveness: blocks eventually committed | **AT RISK** | Reputation mismatch (PF-10), epoch halt (FIND-02), sync stall (FIND-06) |
| 3 | Quorum overlap: >=1 honest in intersection | **HOLDS** | Quorum formula correct (CHK-03) |
| 4 | Monotonic locking: locked QC never decreases | **HOLDS** | update_view guard (CHK-11) |
| 5 | Epoch continuity: quorum overlap maintained | **AT RISK** | Non-deterministic cap (FIND-17), double-call (FIND-02) |
| 6 | Deterministic leader: same inputs -> same leader | **HOLDS** plain; **FAILS** with reputation | Routing mismatch (PF-10) |
| 7 | Evidence validity: verify() <-> valid sigs | **AT RISK** | Slot poisoning (FIND-08), address mismatch (FIND-01) |
| 8 | Slashing finality: tombstoned never re-enter | **NOT ENFORCED** | All slashes fail (FIND-01) |
| 9 | NEC exclusivity: signers in at most one group | **BROKEN** | valid_nec doesn't verify non-voting (PF-07) |
| 10 | Speculative rollback atomicity | **BROKEN** | Irreversible writes (PF-08), no EVM rollback, stale header (PF-05) |

---

*End of Consensus Safety Audit (3.4.2) — Instance A*
