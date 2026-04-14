# Task 3.4.5: Fix Audit Findings from 3.4.2 / 3.4.3 / 3.4.4

## Scope
Fix all confirmed findings from three audits:
- **3.4.2** Consensus Safety (`research/consensus-safety-audit-3.4.2.md`) — 7 Crit, 12 High, 15 Med, 12 Low/Info
- **3.4.3** EVM Correctness (`research/audit-3.4.3-evm-correctness.md`) — 6 Crit, 15 High, 14 Med, 8 Low/Info
- **3.4.4** Economic Model (`research/ECON-AUDIT-3.4.4.md`) — 2 Crit, 13 High, 18 Med, 14 Low/Info

## Execution Rules
1. **Sequential by severity**: All Criticals first, then all Highs, then Mediums, then Lows
2. **Cross-audit duplicates**: Fix once, mark resolved in all audits
3. **For each fix**: Read affected code → implement fix → write/update test → `cargo check` → mark done
4. **Never assume** file paths or line numbers from audit reports are still accurate — always `Read` the file first
5. **Commit after each severity tier** (one commit for all Criticals, one for all Highs, etc.)

---

## Cross-Audit Deduplication Map

These findings appear in multiple audits. Fix once, credit to all:

| Canonical ID | Duplicates | Highest Severity | Root Issue |
|---|---|---|---|
| **DIVERGE-01** | CONS-FIND-03, EVM-FIND-01 | Critical | Proposer/validator pipeline executes different steps before state root |
| **SORTKEY-01** | ECON-FIND-01, CONS-PF-03, EVM-FIND-13 | Critical | `action_sort_key` uses `Debug` format — non-deterministic across compilers |
| **SERDE-HASH-01** | EVM-FIND-04, EVM-FIND-11 | High | `serde_json`/`Debug` format used for consensus-critical hashing |
| **RECEIPTS-01** | CONS-PF-13 (3.4.2), CONS-PF-13 (3.4.3) | High | `receipts_root` and `logs_bloom` never verified during validation |
| **BASEFEE-01** | CONS-PF-14 (3.4.2), CONS-PF-14 (3.4.3) | High | `base_fee` trusted from header without EIP-1559 recalculation |
| **LOCKBOX-01** | ECON-PF-04 (3.4.3), ECON-PF-04 (3.4.4) | High | Lockbox deposit/withdraw non-atomic |
| **HASHSET-01** | CONS-FIND-17, ECON-PF-14 | High | `HashSet::difference` non-deterministic in epoch rotation |
| **EPOCH-01** | CONS-PF-04 (3.4.2), CONS-PF-04 (3.4.3) | Medium | `epoch_length=0` and `max_validators=0` hardcoded |
| **FP-OVERFLOW** | ECON-PF-01, ECON-PF-02, EVM-PF-06, EVM-FIND-07, ECON-PF-08 | High | FixedPoint i128 add/sub/div unchecked — wraps or panics |

---

## TIER 1: CRITICAL FIXES (14 unique)

### CRIT-01 | DIVERGE-01 | Proposer/Validator Pipeline Divergence
- **Source**: CONS-FIND-03 + EVM-FIND-01
- **Files**: `torus-bridge/src/proposer.rs:159-197` vs `torus-bridge/src/validator.rs:157-208`
- **Problem**: Proposer runs 4 steps (pre_evm, EVM, post_evm, state_root). Validator runs 8 steps (same 4 + drain_core_writer, process_governance, distribute_fees, process_epoch_boundary, then state_root). State roots always diverge.
- **Fix**: Add steps 5-8 to the proposer pipeline in the same order as validator, before state root computation.
- **Verification**: Build a block via proposer path, validate via validator path — state roots must match.

### CRIT-02 | SORTKEY-01 | action_sort_key Uses Debug Format
- **Source**: ECON-FIND-01 + CONS-PF-03 + EVM-FIND-13
- **File**: `torus-bridge/src/native_executor.rs:872`
- **Problem**: `keccak256(format!("{:?}", action))` — Debug output varies across compiler/crate versions. Two validators with different rustc produce different sort orders → different state roots → consensus failure.
- **Fix**: Replace with deterministic encoding: Borsh serialization or EIP-712 struct hash of the action.
- **Verification**: Ensure identical sort output for same input across debug/release builds.

### CRIT-03 | CONS-PF-07 | NEC valid_nec Missing Non-Voting Verification
- **Source**: 3.4.2
- **File**: `hotstuff_rs/` (NEC validation logic)
- **Problem**: `valid_nec` does not verify that the NEC signer is a non-voting validator, breaking MonadBFT tail-fork resistance. Any validator can forge a NEC.
- **Fix**: Add check that NEC signer is NOT in the current view's voting set. Verify NEC signer is a valid validator.

### CRIT-04 | CONS-FIND-01 | Slash Address Derivation Mismatch
- **Source**: 3.4.2
- **File**: `torus-consensus/src/app.rs:338-345`
- **Problem**: `leader_addr = SHA-256(ed25519_pubkey)[12..32]` but `StakingManager` indexes by Ethereum address. `staking.slash(leader_addr)` always returns `ValidatorNotFound`. No equivocating validator is ever slashed.
- **Fix**: Maintain a consensus_pubkey → eth_address mapping. Use the validator's registered Ethereum address for slashing, not a derived one.

### CRIT-05 | CONS-FIND-02 | Epoch Validator Set Double-Call Halts Proposer
- **Source**: 3.4.2
- **File**: `torus-consensus/src/app.rs:288-291` + `hotstuff_rs/src/hotstuff/implementation.rs:273-274`
- **Problem**: `epoch_validator_set_updates` called during `produce_block`, mutating `last_validator_set`. Second call during `validate_block` sees already-mutated state → returns `None`. Proposer stores `None` while validators store `Some(updates)` → proposer halts permanently at epoch boundaries.
- **Fix**: Make `epoch_validator_set_updates` idempotent (cache result for current height), or split into compute + apply phases.

### CRIT-06 | CONS-PF-01 | produce_empty_block Sets state_root = B256::ZERO
- **Source**: 3.4.2
- **File**: `torus-consensus/` (empty block production)
- **Problem**: Empty blocks set `state_root: B256::ZERO` which validators accept without recalculation.
- **Fix**: Compute actual state root even for empty blocks (the state root should reflect current state, not zero).

### CRIT-07 | CONS-PF-02 | Native Actions Never Validated During Consensus
- **Source**: 3.4.2
- **File**: `torus-consensus/` (validation path)
- **Problem**: Native actions (staking, governance, etc.) in proposed blocks are never validated. Arbitrary staking/governance operations bypass consensus checks.
- **Fix**: Add native action validation in the block validation path — verify signatures, authorization, and parameter validity for each native action.

### CRIT-08 | CONS-PF-08 | on_speculative_rollback Applies Irreversible Slash
- **Source**: 3.4.2
- **File**: `torus-consensus/` (speculative rollback handler)
- **Problem**: `on_speculative_rollback` writes slashing to RocksDB directly. If the block that triggered slashing is rolled back, the slash persists.
- **Fix**: Buffer slashing operations and only commit them when the block is finalized, not during speculative execution.

### CRIT-09 | EVM-PF-08 | BLOCKHASH Opcode Returns JSON Bytes
- **Source**: 3.4.3
- **File**: `torus-state/src/db.rs:155-162`
- **Problem**: `get_block_hash` returns `serde_json::to_vec(&header)` instead of the actual block hash. BLOCKHASH opcode returns wrong value → smart contracts using BLOCKHASH get garbage.
- **Fix**: Return the actual `keccak256(rlp(header))` block hash (or stored block hash from the DB).

### CRIT-10 | EVM-PF-12 | Native Root Has No Length Framing
- **Source**: 3.4.3
- **File**: `torus-state/src/state_root.rs:66-73`
- **Problem**: Native state root concatenates key-value pairs without length prefixes. `("ab", "cd")` and `("a", "bcd")` produce the same hash → collision.
- **Fix**: Add length framing: `hash(len(key) || key || len(value) || value)` for each entry.

### CRIT-11 | EVM-PF-10 | Write Precompiles Have Zero Gas + Not Registered
- **Source**: 3.4.3
- **File**: `torus-core/src/precompiles.rs:219-246`
- **Problem**: Custom write precompiles (staking, governance, trading via CoreWriter) charge 0 gas and are not registered with revm's precompile set. State-mutating operations are free and may not execute.
- **Fix**: Register all write precompiles with revm. Assign appropriate gas costs (at minimum cold SLOAD-equivalent for reads, SSTORE-equivalent for writes).

### CRIT-12 | EVM-FIND-02 | Mempool memory_used Never Decremented
- **Source**: 3.4.3
- **File**: `torus-mempool/src/lib.rs:159-161`
- **Problem**: `fetch_add` on insertion, no `fetch_sub` anywhere. After enough insert/drain cycles, `add_evm_tx` permanently fails with `PoolFull` even when pool is empty.
- **Fix**: Decrement `memory_used` by `tx_size` on every removal path (drain, eviction, replacement).

### CRIT-13 | EVM-FIND-04 | serde_json Block Hash Not Deterministic
- **Source**: 3.4.3
- **File**: `torus-bridge/src/committer.rs:32-34`
- **Problem**: Block hash = `keccak256(serde_json::to_vec(&header))`. Field ordering varies across serde versions → different nodes compute different block hashes for the same block.
- **Fix**: Use canonical serialization: RLP encoding or a manually-defined field order with explicit byte layout.

### CRIT-14 | ECON-FIND-02 | Order Book Not Persisted — In-Memory Only
- **Source**: 3.4.4
- **File**: `torus-bridge/src/native_executor.rs:81`, `torus-core/src/order_book.rs:108-126`
- **Problem**: `OrderBook` is a `HashMap<MarketId, OrderBook>` in memory. No serialization, no DB, no state root commitment. Node restart = all resting orders vanish.
- **Fix**: Implement order book persistence to RocksDB with state-root commitment. Add Borsh serialization for `OrderBook`. Load from DB on startup.

---

## TIER 2: HIGH FIXES (28 unique after dedup)

### Consensus Highs (3.4.2)

| ID | File | Fix Summary |
|----|------|-------------|
| CONS-PF-09 | hotstuff_rs pacemaker | Count voting **power**, not message count, for Bracha timeout |
| CONS-PF-10 | hotstuff_rs leader selection | Use reputation-weighted leader selection for `new_view_recipients` |
| CONS-PF-11 | hotstuff_rs | Validate block in `ProposalResponse` before reproposal |
| CONS-PF-12 | torus-network gossip | Cross-check `sender_key` against authenticated peer identity |
| CONS-FIND-04 | torus-network/src/swarm.rs:227-228 | Same as PF-12 for unicast: authenticate `sender_key` vs peer ID |
| CONS-FIND-05 | hotstuff_rs/src/block_sync/server.rs:122 | Change `max()` to `min()` to cap sync response size |
| CONS-FIND-06 | hotstuff_rs/src/block_sync/client.rs:300-449 | Add max iteration count + total session deadline to sync loop |
| CONS-FIND-07 | hotstuff_rs/src/pacemaker/messages.rs:134-143 | Include `local_tip` and `highest_qc` in `message_bytes()` for signing |
| CONS-FIND-08 | torus-consensus/src/slashing.rs:138-177 | Verify vote signature before storing in `DoubleSignDetector` |
| CONS-FIND-09 | torus-consensus/src/app.rs:99-100, staking.rs:896 | Don't delete key rotations from DB during `produce_block`; defer to commit |
| CONS-FIND-10 | torus-network/src/swarm.rs:41,228,277,343 | Add capacity bound + eviction policy to inbound message `VecDeque` |

### EVM Highs (3.4.3, deduplicated)

| ID | File | Fix Summary |
|----|------|-------------|
| RECEIPTS-01 | torus-bridge validator.rs:43-89 | Recompute `receipts_root` and `logs_bloom` during validation and compare |
| BASEFEE-01 | torus-bridge validator.rs:52-58 | Recalculate base fee via EIP-1559 formula and reject mismatch |
| EVM-PF-01 | torus-types eip1559.rs:16,24-26 | Guard `calc_next_block_base_fee` against `gas_limit < 2` (div-by-zero) |
| EVM-PF-09 | torus-bridge validator.rs:191-201 | Call oracle aggregation + liquidation checks in validator path |
| EVM-PF-11 | torus-state/src/snapshot.rs:133-136 | Compare same root types in snapshot verification |
| EVM-PF-06 | torus-core/src/precompiles.rs:700-702 | Use `checked_cast` or `try_from` for u128->i128 in precompile inputs |
| EVM-FIND-03 | torus-bridge/src/committer.rs:29-70 | Wrap all `put_cf` calls in a single `WriteBatch` |
| EVM-FIND-05 | torus-mempool/src/lib.rs:192-196 | Call full `validate()` (not just `recover_sender`) in `add_native_action` |
| EVM-FIND-06 | torus-state/src/snapshot.rs:177-183 | Copy to temp dir first, then atomic rename |
| EVM-FIND-07 | torus-core/src/lockbox.rs:53 | Use `checked_add` for FixedPoint i128 addition |
| EVM-FIND-08 | torus-mempool/src/validate.rs:70-77 | Add `max_future_nonce_gap` check (reject nonce > 64 ahead) |
| EVM-FIND-09 | torus-core/src/precompiles.rs:164-166 | Use `encode_fp_as_i128` for potentially-negative values |
| LOCKBOX-01 | torus-core/src/lockbox.rs:49-54 | Make lockbox deposit/withdraw atomic (WriteBatch or transactional) |

### Economic Highs (3.4.4, deduplicated)

| ID | File | Fix Summary |
|----|------|-------------|
| FP-OVERFLOW | torus-types/src/lib.rs:72-75+ | Add `checked_add`, `checked_sub`, `checked_div` to FixedPoint; replace bare ops |
| ECON-PF-06 | torus-core/src/liquidation.rs | Guard against zero oracle price in liquidation path |
| ECON-PF-10 | torus-core/src/margin.rs | Fix maintenance margin BPS calculation (off by ~10^8) |
| ECON-PF-11 | torus-core/src/margin.rs | Use `checked_sub` for `free_margin = equity - maint` to prevent wrapping |
| ECON-PF-12 | torus-economics/src/governance.rs | Restrict `ParameterChange` to allowlisted params with validation |
| ECON-PF-15 | torus-core order cancel/modify | Add ownership check: `order.owner == sender` |
| HASHSET-01 | torus-economics/src/staking.rs | Replace `HashSet::difference` with `BTreeSet` or sorted `Vec` for determinism |
| ECON-FIND-03 | eip712.rs, mempool, proposer, native_executor | Call `validate()` in execution path; implement persistent nonce tracking CF |
| ECON-FIND-04 | torus-core/src/liquidation.rs:185-225 | Add liquidation penalty (1-5% notional) credited to insurance fund |
| ECON-FIND-05 | torus-core/src/position.rs:129-160, native_executor.rs:262-287 | Implement order margin reservation in `exec_place_order`; release on cancel/fill |
| ECON-FIND-06 | torus-economics/src/governance.rs:665-707 | Add `timelock_blocks` between passing and execution |
| ECON-FIND-07 | torus-economics/src/governance.rs:969-971 | Validate `permanent_weight_multiplier_den > 0` at deserialization |
| ECON-FIND-08 | torus-core/src/margin.rs:185-197 | Fix equity: `bal.available - bal.order_margin + sum(unrealized_pnl)` |

---

## TIER 3: MEDIUM FIXES (33 unique after dedup)

### Consensus Mediums

| ID | Fix |
|----|-----|
| EPOCH-01 (CONS-PF-04) | Make `epoch_length` and `max_validators` configurable via genesis/config |
| CONS-PF-05 | Don't update `last_header` until block is confirmed |
| CONS-PF-15 | Add size bound + eviction to `seen_proposals` HashMap |
| CONS-PF-16 | Add size bound + eviction to `bracha_timeout_counts` BTreeMap |
| CONS-FIND-11 | Use reputation-weighted leader selection for NE messages |
| CONS-FIND-12 | Use reputation-weighted `is_proposer` in `enter_view` |
| CONS-FIND-13 | Use reputation-weighted leader in `phase_vote_recipient` |
| CONS-FIND-14 | Fix CVS/PVS vote double-counting in `ActiveCollectorPair` |
| CONS-FIND-15 | Fix `valid_nec` dead branch during validator set transition |
| CONS-FIND-16 | Make `highest_view_voted` / `last_voted_proposal` writes atomic |
| CONS-FIND-18 | Accept TC in `AdvanceView` for non-epoch views |
| CONS-FIND-19 | Fix rate limiter tumbling window to prevent 2x burst |
| CONS-FIND-20 | Guard against `epoch_length = 0` division |

### EVM Mediums

| ID | Fix |
|----|-----|
| EVM-PF-02 | Use `u128::try_from` for effective gas price (no silent truncation) |
| EVM-PF-03 | Replace `all_accounts()` with streaming iterator for state root |
| EVM-PF-07 | Propagate RocksDB iterator errors instead of ignoring |
| EVM-PF-14 | Implement block tag support in `eth_call` |
| EVM-PF-15 | Add block range limit to `eth_getLogs` (e.g., max 10,000 blocks) |
| EVM-PF-16 | Replace O(n) `next_sequence` with stored counter |
| EVM-FIND-10 | Return error from `eth_estimateGas` on revert, not gas used |
| EVM-FIND-11 | Use EIP-712 struct hash for native pool dedup (not Debug format) |
| EVM-FIND-12 | Propagate CoreWriter drain errors instead of returning empty vec |
| EVM-FIND-14 | Replace O(N) `find_latest_height` with reverse iterator |
| EVM-FIND-15 | Add signature verification to `submit_native_action` |
| EVM-FIND-16 | Replace `.expect()` with proper error handling in proposer receipts serialization |
| EVM-FIND-17 | Add `accessList` field support for EIP-2930/1559 RPC responses |
| EVM-FIND-19 | Count rate-limited transactions at submission, not just commit |

### Economic Mediums

| ID | Fix |
|----|-----|
| ECON-PF-03 | Map `Abstain` to distinct vote variant, not `No` |
| ECON-PF-05 | Require explicit tick/lot size at market creation |
| ECON-PF-16 | Fix `BLOCKS_PER_YEAR` to match actual block time |
| ECON-PF-17 | Use `to` address in withdraw path (don't discard) |
| ECON-PF-18 | Handle single-reporter case in oracle outlier rejection |
| ECON-FIND-09 | Make order IDs globally unique (prefix with market_id) |
| ECON-FIND-10 | Reject limit orders with `price <= 0` |
| ECON-FIND-11 | Enforce tick size validation in `place_order` |
| ECON-FIND-12 | Validate stop order trigger direction vs current price |
| ECON-FIND-13 | Exclude near-liquidation accounts from socialized loss |
| ECON-FIND-14 | Fix slashing dust rounding: include zero-slash delegations in total |
| ECON-FIND-15 | Add `top_up_self_stake` operation for slashed validators |
| ECON-FIND-16 | Snapshot voting power at proposal start block |
| ECON-FIND-17 | Cap orders per trader per market |
| ECON-FIND-18 | Prune oracle submissions older than `max_oracle_age` |
| ECON-FIND-19 | Verify sender is active non-jailed validator for oracle submissions |

---

## TIER 4: LOW + INFORMATIONAL FIXES (28 unique)

### Low Priority (fix if time permits, no consensus/safety impact)

| ID | Fix |
|----|-----|
| EVM-PF-17 | Compute actual `transactions_root` instead of `B256::ZERO` |
| EVM-PF-18 | Use account nonce (not 0) for `eth_call` default |
| EVM-FIND-18 | Use EIP-155 chain_id encoding for legacy tx `v` value |
| EVM-FIND-20 | Handle orphaned storage on same-block create+destroy |
| ECON-FIND-20 | Apply staleness check in `get_last_valid_price` |
| ECON-FIND-21 | Make whitelist consumption mandatory (fail registration if consume fails) |
| ECON-FIND-22 | Wire FeeSplitter pipeline or remove dead call |
| ECON-FIND-23 | Handle `apply_fill` errors instead of `let _ =` |
| ECON-FIND-24 | Recompute voter stakes at jail vote tally time |
| ECON-FIND-25 | Cap unbonding entries per delegation; enforce minimum amount |
| ECON-FIND-26 | Use `u256_to_fp` in lockbox precompile (not raw cast) |
| ECON-FIND-27 | Validate CoreWriter discriminant values at queue time |
| ECON-FIND-28 | Set `original_qty = new_qty` on modify increase |
| ECON-FIND-29 | Enforce `KEY_ROTATION_COOLDOWN_EPOCHS` or remove dead code |
| ECON-FIND-30 | Add secondary index for `delegations_for_validator` lookups |
| ECON-FIND-31 | Add version byte prefix to Position/NativeBalance Borsh serialization |
| CONS-FIND-21-24 | Add bounds to unbounded data structures; fix ban race + sync limits |
| CONS-FIND-25-32 | Fix precedence bug, TipInfo validation, stale local_tip, deterministic ProposalRequest, sync I/O blocking, cancel_order auth, access_list, ViewNumber overflow |

### Informational (no code fix needed, or tracking only)

| ID | Note |
|----|------|
| EVM-FIND-21 | CF_LOGS and CF_LOGS_BLOOM never populated — wire up or document as TODO |
| EVM-FIND-22 | No unit tests for executor.rs — add test coverage |
| ECON-PF-07 | Admin actions are no-op stubs — document as intentional or implement |
| ECON-PF-09 | Unbonding vec len u32 truncation — add bounds check |

---

## Verification Protocol

After each severity tier:
1. `cargo check --all-targets` — must compile
2. `cargo test` — all existing tests pass
3. `cargo clippy` — no new warnings
4. Create a tracking comment in the commit listing which findings were addressed

## Commit Strategy
- **Commit 1**: "fix(critical): resolve 14 critical audit findings from 3.4.2/3.4.3/3.4.4"
- **Commit 2**: "fix(high): resolve 28 high-severity audit findings"
- **Commit 3**: "fix(medium): resolve 33 medium-severity audit findings"
- **Commit 4**: "fix(low): resolve low-severity and informational findings"

## Finding Count Summary (Deduplicated)

| Severity | Unique Findings |
|----------|----------------|
| Critical | 14 |
| High | 28 |
| Medium | 33 |
| Low/Info | 28 |
| **Total** | **103** |

*Original total across 3 audits: ~117 findings. 14 duplicates consolidated.*
