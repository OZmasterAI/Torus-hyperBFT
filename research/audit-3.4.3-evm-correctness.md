# Torus-hyperBFT Security Audit: EVM Correctness (Phase 3.4.3, Instance B)

**Auditor**: Claude (automated static analysis)
**Date**: 2026-04-14
**Scope**: EVM execution, state roots, gas accounting, precompiles, mempool, RPC
**Codebase**: ~12,280 lines across 7 crates (torus-evm, torus-bridge, torus-state, torus-mempool, torus-rpc, torus-core/precompiles, torus-core/lockbox)
**Status**: READ-ONLY AUDIT — no code changes made

---

## 1. Executive Summary

The Torus-hyperBFT EVM execution layer contains **6 critical**, **15 high**, **14 medium**, and **8 low/informational** findings. The most severe issues are: (1) a proposer/validator execution pipeline divergence that guarantees `StateRootMismatch` on every block validated via `validate_block_with_native`, rendering full pipeline validation inoperable; (2) the EVM `BLOCKHASH` opcode returning garbage data (first 32 bytes of JSON-serialized headers instead of actual block hashes), breaking all contracts that use `blockhash()`; (3) a mempool memory counter that never decrements, causing permanent pool lockout after sufficient transaction throughput; and (4) non-atomic lockbox transfers that can destroy funds on crash. The EIP-1559 base fee calculation is correct but unguarded against division-by-zero on degenerate gas limits. The precompile system is architecturally incomplete — write precompiles are defined but not registered with revm, making them unreachable from EVM transactions. RPC endpoints have numerous deviations from the Ethereum JSON-RPC specification that will break standard tooling (hardhat, ethers.js, foundry). The state root computation loads all accounts into memory for every block, creating an O(n) DoS surface that will cause OOM as state grows.

---

## 2. Verified Pre-Findings

### EVM-PF-08 — `get_block_hash` Returns JSON Bytes, Not Block Hash
- **Status**: CONFIRMED
- **Severity**: Critical
- **Evidence**: `torus-state/src/db.rs:155-162` reads from `CF_BLOCK_HEADERS` and returns `B256::from_slice(&data[..32])`. The committer (`torus-bridge/src/committer.rs:32-38`) stores `serde_json::to_vec(&block.header)` (variable-length JSON) in `CF_BLOCK_HEADERS`, not the 32-byte hash. The first 32 bytes of JSON (e.g., `{"height":12345,"ti...`) are returned as the "block hash".
- **Root Cause**: `put_block_hash` (db.rs:165-173) stores the correct 32-byte format but is **never called in production** — only in tests (`state_tests.rs:131,203`). The committer uses `put_cf_raw` to store full JSON. Tests pass because they use `put_block_hash`; production uses the committer path.
- **Impact**: `block_hash_ref` (db.rs:286-288) feeds this to revm's `BLOCKHASH` opcode. Every contract calling `blockhash(N)` receives garbage. Breaks commit-reveal schemes, randomness derivation, cross-chain proofs, and any contract relying on block hash values.
- **Recommended Fix**: Either (a) store the actual block hash in `CF_BLOCK_HEADERS` alongside or instead of JSON, or (b) add a dedicated `CF_BLOCK_HASHES` column family and update `get_block_hash` to read from it. Update tests to use the production code path.

---

### EVM-PF-09 — Oracle Aggregation and Liquidation Checks Never Called
- **Status**: CONFIRMED
- **Severity**: High (design incompleteness)
- **Evidence**: `torus-bridge/src/validator.rs:191-202` calls only `process_governance`, `distribute_fees`, and `process_epoch_boundary`. Neither `aggregate_oracle_prices` (defined at `native_executor.rs:652`) nor `run_liquidation_checks` (defined at `native_executor.rs:673`) is called in either the proposer or validator pipeline. The comment at validator.rs:193-195 claims these are "driven by the block-level helpers" but the helpers don't invoke them.
- **Root Cause**: These functions require market/trader lists that the bridge layer does not have access to. The design specification includes steps 7 (oracle) and 9 (liquidation) but the implementation has no call site for them.
- **Impact**: Oracle prices are never aggregated per block, and positions are never automatically liquidated during block execution.
- **Recommended Fix**: Wire `aggregate_oracle_prices` and `run_liquidation_checks` into both proposer and validator pipelines with the necessary market/trader context.

---

### EVM-PF-13 — Block Gas Limit Checked AFTER `transact_commit`
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `torus-evm/src/executor.rs:159-175`: `transact_commit` (line 159) merges state into revm's `State` object. The gas limit check (line 170) fires after commit. The error propagation causes the caller to discard the entire `BlockExecResult`, so damage is contained IF callers always discard on error.
- **Root Cause**: No pre-execution gas guard exists.
- **Impact**: Over-limit transactions consume full revm execution wastefully. If any caller partially reuses the EVM instance after an error, leaked state causes inconsistency.
- **Recommended Fix**: Add pre-execution check: `if cumulative_gas + tx.gas_limit > block_cfg.gas_limit { skip }` before `transact_commit`.

---

### EVM-PF-10 — Write Precompiles Have No Gas Cost
- **Status**: CONFIRMED (with additional finding)
- **Severity**: Critical (architectural)
- **Evidence**: `torus-core/src/precompiles.rs:219-246` — `execute_precompile` has no gas parameter and returns no gas consumed. Additionally, `torus-evm/src/executor.rs` uses vanilla revm and does **not** register Torus precompiles. The precompiles are unreachable from EVM transactions.
- **Impact**: (a) EVM contracts cannot call any Torus precompile — the cross-VM bridge is non-functional via EVM transactions. (b) When registered, zero gas allows unlimited precompile calls per transaction.
- **Recommended Fix**: Register precompiles with revm via `EvmBuilder::with_precompiles()`. Assign appropriate gas costs.

---

### EVM-PF-12 — Native State Root Has No Length Framing
- **Status**: CONFIRMED
- **Severity**: Critical
- **Evidence**: `torus-bridge/src/state_root.rs:66-73` — KV pairs from 5 CFs concatenated with no delimiters, length prefixes, or CF boundary markers.
- **Collision Example**: CF_A key=`[0x01,0x02]` value=`[0x03]` vs CF_B key=`[0x01]` value=`[0x02,0x03]` — identical byte stream `01 02 03`.
- **Impact**: Violates state root uniqueness invariant. Different native states can produce identical composite roots.
- **Recommended Fix**: Use length-prefixed encoding per entry with CF name delimiters.

---

### EVM-PF-11 — Snapshot Verification Compares EVM Root vs Composite Root
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `torus-state/src/snapshot.rs:133-136` — `verify_snapshot` computes pure EVM MPT root but compares against `metadata.state_root` which is the composite root.
- **Impact**: Snapshot verification either always fails or only validates EVM state, leaving native state unverified.
- **Recommended Fix**: Compute and verify the composite root in `verify_snapshot`.

---

### CONS-PF-13 — `receipts_root` and `logs_bloom` Never Verified During Validation
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `torus-bridge/src/validator.rs:43-90` — only `evm_gas_used` (line 69) and `state_root` (line 78) verified. No check for `receipts_root` or `logs_bloom`.
- **Impact**: Byzantine proposer can falsify receipt data without detection.
- **Recommended Fix**: Recompute and verify `receipts_root` and `logs_bloom` from execution results.

---

### CONS-PF-14 — Base Fee Trusted from Proposed Header Without Verification
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `torus-bridge/src/validator.rs:57` — validator uses proposer's `base_fee_per_gas` directly. Function signature does not accept parent header.
- **Impact**: Malicious proposer can set any base fee (0 = free txs, u64::MAX = block all txs).
- **Recommended Fix**: Pass parent header to `validate_block` and verify base fee matches EIP-1559 computation.

---

### EVM-PF-01 — Division by Zero in `calc_next_block_base_fee` When `gas_limit < 2`
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `torus-evm/src/eip1559.rs:16` — `gas_target = gas_limit / 2`. If `gas_limit < 2`, `gas_target == 0`. At lines 24-26, division by zero causes panic.
- **Impact**: Node crash on degenerate gas limit (e.g., via CONS-PF-14 base fee manipulation).
- **Recommended Fix**: Add `if gas_limit < 2 { return base_fee; }` guard.

---

### ECON-PF-04 — Lockbox Deposit/Withdraw Non-Atomic
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `torus-core/src/lockbox.rs:49-54` — debit EVM then credit native in separate `put_cf_raw()` calls. No `WriteBatch`.
- **Impact**: Crash between debit and credit destroys or duplicates funds.
- **Recommended Fix**: Use `WriteBatch` for both writes.

---

### EVM-PF-02 — `calc_effective_gas_price` Silent u128->u64 Truncation
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `torus-evm/src/executor.rs:292,294` — `as u64` casts truncate silently.
- **Impact**: Theoretically wrong receipt `effective_gas_price` for unrealistically high gas prices.
- **Recommended Fix**: Use `u64::try_from()` or saturating cast.

---

### EVM-PF-03 — `all_accounts()` Loads All Accounts Into Memory
- **Status**: CONFIRMED
- **Severity**: Medium (grows to High)
- **Evidence**: `torus-bridge/src/state_root.rs:93` and `db.rs:205-222` — full `CF_ACCOUNTS` scan into `Vec`, then `BTreeMap`.
- **Impact**: O(total accounts) memory per block. OOM as state grows.
- **Recommended Fix**: Implement incremental state root (reth `StateRoot` with cursor factories).

---

### EVM-PF-05 — Lockbox Writes Directly to CF_ACCOUNTS Bypassing StateDb
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `torus-core/src/lockbox.rs:113-124` uses `put_cf_raw(CF_ACCOUNTS, ...)`. The revm `BundleState` also writes to `CF_ACCOUNTS` via `apply_bundle_to_db`.
- **Impact**: Concurrent writes to same account can overwrite each other's changes.
- **Recommended Fix**: Route all account writes through single coordinated API.

---

### EVM-PF-07 — RocksDB Iterator Errors Silently Ignored in Native Root
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `torus-bridge/src/state_root.rs:68` — `if let Ok((key, value)) = item` drops errors.
- **Impact**: Transient I/O error causes silent state root divergence between nodes.
- **Recommended Fix**: Change return type to `Result<B256, StateError>` and propagate errors.

---

### EVM-PF-14 — `eth_call` Ignores Block Tag Parameter
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `torus-rpc/src/eth.rs:744-761` — `_height` computed but discarded; always uses `latest`.
- **Impact**: Breaks historical queries from hardhat, ethers.js, foundry.

---

### EVM-PF-15 — `eth_getLogs` Has No Block Range Limit
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `torus-rpc/src/eth.rs:800-852` — `MAX_LOGS = 10_000` caps results but no block range cap.
- **Impact**: DoS via unbounded sequential RocksDB reads.

---

### EVM-PF-16 — `next_sequence` Is O(n) per Enqueue
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `torus-core/src/precompiles.rs:917-943` — full prefix scan per enqueue. N enqueues = O(N^2).
- **Recommended Fix**: Use `seek_for_prev` for O(log N).

---

### CONS-PF-04 — `epoch_length=0` and `max_validators=0` Hardcoded
- **Status**: CONFIRMED
- **Severity**: Medium
- **Evidence**: `proposer.rs:167-168` and `validator.rs:149-150` — literal `0` for both values.
- **Impact**: Epoch boundaries never trigger; validator rotation non-functional.

---

### EVM-PF-06 — `FixedPoint::from_raw(price as i128)` u128->i128 Overflow
- **Status**: CONFIRMED
- **Severity**: High
- **Evidence**: `precompiles.rs:700-702`, `:758`, `:769`, `:817`, `:822` — values >= 2^127 become negative FixedPoint.
- **Impact**: Negative prices/quantities enter order book; lockbox silently no-ops.
- **Recommended Fix**: Validate `amount_raw <= i128::MAX as u128` before casting.

---

### EVM-PF-17 — `transactions_root` Always `B256::ZERO`
- **Status**: CONFIRMED | **Severity**: Low | **Evidence**: `eth.rs:370`

### EVM-PF-18 — `eth_call` Nonce Defaults to 0
- **Status**: CONFIRMED | **Severity**: Low | **Evidence**: `eth.rs:196-198`

---

## 3. New Findings

### EVM-FIND-01 — Proposer/Validator Pipeline Divergence (State Root Always Mismatches)
- **Severity**: Critical
- **File**: `torus-bridge/src/proposer.rs:159-197` vs `torus-bridge/src/validator.rs:157-208`
- **Description**: The proposer (`build_block_with_native`) executes 4 steps: pre_evm native, EVM, post_evm native, state_root. The validator (`validate_block_with_native`) executes 8 steps: the same 4, then **drain_core_writer, process_governance, distribute_fees, process_epoch_boundary**, then state_root. Steps 5-8 write to native CFs but exist only in the validator.
- **Proof of Concept**: Any block proposed by `build_block_with_native` has `state_root` computed without CoreWriter drain, governance, fee distribution, or epoch processing. The validator computes `state_root` after those steps. `validate_block_with_native` always returns `StateRootMismatch`.
- **Recommended Fix**: Add steps 5-8 to the proposer pipeline before state root computation, in the same order as the validator.

---

### EVM-FIND-02 — Mempool `memory_used` Counter Never Decremented
- **Severity**: Critical
- **File**: `torus-mempool/src/lib.rs:159-161`
- **Description**: `memory_used` uses `fetch_add` on insertion but there is no `fetch_sub` anywhere. `drain_evm()` and eviction paths do not decrement.
- **Proof of Concept**: After enough insert/drain cycles to exceed `max_memory_bytes`, all subsequent `add_evm_tx` calls fail with `PoolFull` permanently, even when the pool is empty.
- **Recommended Fix**: Decrement `memory_used` by `tx_size` on every removal (drain, eviction, replacement).

---

### EVM-FIND-03 — Non-Atomic Block Commit (No WriteBatch)
- **Severity**: High
- **File**: `torus-bridge/src/committer.rs:29-70`
- **Description**: `commit_block` performs 6+ individual `put_cf` calls with no RocksDB `WriteBatch`. A crash mid-commit leaves partially-written state.
- **Proof of Concept**: Crash after writing state but before header. On restart, state reflects new block but header doesn't exist, causing re-execution and double-applying state.
- **Recommended Fix**: Wrap all writes in a single `WriteBatch`.

---

### EVM-FIND-04 — `serde_json` Block Hash Is Not Stable Across Versions
- **Severity**: High
- **File**: `torus-bridge/src/committer.rs:32-34`
- **Description**: Block hash is `keccak256(serde_json::to_vec(&block.header))`. serde_json field ordering follows struct declaration order, not guaranteed stable across serde versions or struct field reordering.
- **Proof of Concept**: Two nodes with different serde_json versions compute different block hashes for the same block, causing chain fork.
- **Recommended Fix**: Use canonical serialization (RLP or manually-defined field order).

---

### EVM-FIND-05 — EIP-712 Nonce/ChainID Not Validated at Mempool Entry
- **Severity**: High
- **File**: `torus-mempool/src/lib.rs:192-196`
- **Description**: `add_native_action()` calls `recover_sender()` but not the full `validate()` method. Nonce freshness (+/-60s) and chain ID checks are skipped.
- **Proof of Concept**: Captured `SignedNativeAction` can be replayed after node restart clears the `seen` dedup set.
- **Recommended Fix**: Call the full `validate()` method.

---

### EVM-FIND-06 — Snapshot Restore Is Non-Atomic (Data Loss on Crash)
- **Severity**: High
- **File**: `torus-state/src/snapshot.rs:177-183`
- **Description**: `remove_dir_all` then `copy_dir_recursive`. Crash between = no data directory.
- **Recommended Fix**: Copy to temp dir first, then atomic rename.

---

### EVM-FIND-07 — FixedPoint i128 Overflow in Lockbox Credit
- **Severity**: High
- **File**: `torus-core/src/lockbox.rs:53` and `torus-types/src/lib.rs:72-75`
- **Description**: `native_bal.available + amount` uses bare `i128 +` with no overflow check. Wraps silently in release mode.
- **Recommended Fix**: Use `checked_add` on inner `i128`.

---

### EVM-FIND-08 — Future Nonce Griefing in EVM Pool
- **Severity**: High
- **File**: `torus-mempool/src/validate.rs:70-77`
- **Description**: Any nonce >= state_nonce accepted with no gap limit. Attacker fills 16-slot queue with far-future nonces, blocking legitimate transactions.
- **Recommended Fix**: Add `max_future_nonce_gap` check (e.g., reject nonces > 64 ahead).

---

### EVM-FIND-09 — `encode_fp_as_u128` Misrepresents Negative FixedPoint
- **Severity**: High
- **File**: `torus-core/src/precompiles.rs:164-166`
- **Description**: `fp.raw() as u128` is bitwise reinterpretation. Negative FixedPoint becomes enormous positive u128 reported to EVM callers.
- **Recommended Fix**: Use `encode_fp_as_i128` for values that can be negative.

---

### EVM-FIND-10 — `eth_estimateGas` Returns Gas Used on Failure Instead of Error
- **Severity**: Medium
- **File**: `torus-rpc/src/eth.rs:785-787`
- **Description**: Reverted probe returns `gas_used` not an error. Callers submit reverting transactions.

---

### EVM-FIND-11 — Native Pool Dedup Uses Debug Format (Unstable Hash)
- **Severity**: Medium
- **File**: `torus-mempool/src/native_pool.rs:164-168`
- **Description**: `format!("{:?}", action.action)` — Debug output not stable across versions.
- **Recommended Fix**: Use EIP-712 struct hash as canonical action hash.

---

### EVM-FIND-12 — CoreWriter Drain Errors Silently Swallowed
- **Severity**: Medium
- **File**: `torus-bridge/src/native_executor.rs:620-621`
- **Description**: Drain failure returns empty vec, indistinguishable from "no queued actions."

---

### EVM-FIND-13 — `action_sort_key` Uses Debug Format (Non-Deterministic)
- **Severity**: Medium
- **File**: `torus-bridge/src/native_executor.rs:870-874`
- **Description**: Sort key is `keccak256(format!("{:?}", action))`. Different binaries may sort differently.
- **Recommended Fix**: Use canonical binary serialization (Borsh).

---

### EVM-FIND-14 — `find_latest_height` Is O(N) Linear Scan
- **Severity**: Medium
- **File**: `torus-rpc/src/lib.rs:147-153`
- **Description**: Sequential scan from block 0. Slow on large chains.

---

### EVM-FIND-15 — `submit_native_action` Is Public and Bypasses Signature Verification
- **Severity**: Medium
- **File**: `torus-mempool/src/lib.rs:200-219`
- **Description**: `pub` method accepts `sender: Address` directly without signature check.

---

### EVM-FIND-16 — `proposer.rs` Uses `.expect()` for Receipts Serialization
- **Severity**: Medium
- **File**: `torus-bridge/src/proposer.rs:244`
- **Description**: Panics instead of returning `BridgeError::Serialization`.

---

### EVM-FIND-17 — RPC Missing `accessList` Field for EIP-2930/1559 Transactions
- **Severity**: Medium
- **File**: `torus-rpc/src/types.rs:138-160`

---

### EVM-FIND-18 — Legacy Transaction `v` Value Uses Raw 27/28 Instead of EIP-155
- **Severity**: Low
- **File**: `torus-rpc/src/eth.rs:225`

---

### EVM-FIND-19 — Rate Limiter Counts Only Committed Transactions
- **Severity**: Medium
- **File**: `torus-mempool/src/rate_limit.rs:110-136`
- **Description**: Pool-level submissions not counted. Attacker can fill pool before rate limit engages.

---

### EVM-FIND-20 — Orphaned Storage on Same-Block Create+Destroy
- **Severity**: Low
- **File**: `torus-bridge/src/committer.rs:91-93`

---

### EVM-FIND-21 — `CF_LOGS` and `CF_LOGS_BLOOM` Never Populated
- **Severity**: Informational
- **File**: `torus-state/src/cf.rs:15-16`

---

### EVM-FIND-22 — No Unit Tests for `executor.rs`
- **Severity**: Informational
- **File**: `torus-evm/src/executor.rs`

---

## 4. State Root Deep Analysis

### Step-by-Step Walk-Through: Block with 3 EVM Txs + Native Actions

**Proposer path** (`proposer.rs:build_block_with_native`):

1. **Sort native actions** into pre_evm (cancellations, non-GTC) and post_evm (GTC, lockbox, oracle, governance, staking).
2. **Execute pre_evm native batch** via `NativeExecutor::execute_batch`. Writes through `ctx.state_db` sharing `Arc<DB>`.
3. **Execute 3 EVM transactions** via `evm_executor.execute_block`. revm uses `WrapDatabaseRef<StateDb>` backed by same `Arc<DB>`. Each tx sees previous tx's results. Returns `BundleState`.
4. **Execute post_evm native batch**.
5. **Compute EVM state root**: `compute_post_bundle_evm_root(state_db, &bundle)` at `state_root.rs:88-145`:
   - Load ALL accounts from `CF_ACCOUNTS` into memory
   - Apply bundle account changes (create, update, delete)
   - Per account: load ALL storage slots, apply bundle storage changes
   - Filter zero-value slots, apply EIP-161 state clearing
   - Build `TrieAccount` entries with `storage_root` per account
   - Call `reth_trie_common::root::state_root_unhashed` for MPT root
6. **Compute native root**: `compute_native_state_root(state_db)` — keccak256 of concatenated KV pairs from 5 CFs
7. **Compute composite root**: `keccak256(evm_root || native_root)`

**Validator path** (`validator.rs:validate_block_with_native`):

Steps 1-4 identical. Then:
5. **drain_core_writer** — processes queued EVM->native actions (writes to native CFs)
6. **process_governance** — applies governance changes
7. **distribute_fees** — distributes gas fees to validators (writes to staking/balance CFs)
8. **process_epoch_boundary** — rotates validators if needed
9. **Compute native root** — now includes effects of steps 5-8
10. **Compute composite root**

### Proposer vs Validator Root Divergence

The proposer computes native root after step 4. The validator computes it after step 8. Steps 5-8 write to native CFs. Therefore:
- `proposer_native_root != validator_native_root`
- `proposer_composite_root != validator_composite_root`
- **Validation always fails with `StateRootMismatch`**

### Native Root Collision Risk

Concrete example (no length framing):
```
State A: CF_NATIVE_BALANCES key=[0xAB, 0xCD], value=[0xEF]
State B: CF_NATIVE_BALANCES key=[0xAB], value=[0xCD, 0xEF]
Both produce: AB CD EF -> same keccak256 hash
```

Cross-CF collision (no CF boundary marker):
```
State A: CF_NATIVE_BALANCES ends with key=K1, value=V1. CF_NATIVE_POSITIONS starts with key=K2
State B: CF_NATIVE_BALANCES ends with key=K1, value=V1||K2. CF_NATIVE_POSITIONS empty.
Same byte stream.
```

### EIP-161 State Clearing Verification

`state_root.rs:111-115` correctly removes accounts with nonce=0, balance=0, and code_hash in {KECCAK_EMPTY, B256::ZERO}. SELFDESTRUCT handled separately at state_root.rs:100-104 (info=None removal). Ordering is correct: SELFDESTRUCT first, then EIP-161 clearing.

---

## 5. Gas Accounting Verification

### Gas Flow: Submission -> Receipt

1. **Submission**: `sendRawTransaction` -> mempool `validate_evm_tx` checks `gas_limit > 0`, balance covers gas (validate.rs:57-68)
2. **Block composition**: `drain_evm()` fills blocks using declared `gas_limit` as budget (evm_pool.rs:256-269)
3. **Execution**: `execute_block` calls `evm.transact_commit(tx)` per tx (executor.rs:159). revm handles gas per Cancun spec.
4. **Post-execution check**: `cumulative_gas += gas_used` with `checked_add` (executor.rs:162-168), then `> gas_limit` check (executor.rs:170-175). **Bug**: check after commit (EVM-PF-13).
5. **Receipt**: `gas_used` from revm (executor.rs:190), `cumulative_gas_used` running total (executor.rs:192), `effective_gas_price` from `calc_effective_gas_price` (executor.rs:195-196).
6. **Header**: `evm_gas_used = cumulative_gas` (proposer.rs:93-94)
7. **Validation**: `exec_result.gas_used == block.header.evm_gas_used` (validator.rs:69-74)

### EIP-1559 Arithmetic Verification

`calc_next_block_base_fee` (eip1559.rs:6-36) matches go-ethereum's `CalcBaseFee`:
- Intermediate u128 promotion prevents overflow
- `max(fee_delta, 1)` minimum increase matches Ethereum
- `saturating_sub` floor at 0 matches Ethereum
- **Only gap**: no guard against `gas_target == 0` (EVM-PF-01)

### Precompile Gas Model

Currently non-existent. `execute_precompile` has no gas parameter. Precompiles not registered with revm. When registered, each call costs only base CALL opcode gas (~100). A 30M gas tx could call `placeOrder` ~300,000 times.

### Gas Griefing Vectors

1. **CoreWriter queue flooding**: Zero precompile gas -> unlimited orders queued per tx
2. **Block gas waste**: Post-commit gas check (EVM-PF-13) wastes full execution before rejection
3. **EIP-1559 manipulation**: CONS-PF-14 allows Byzantine proposer to set arbitrary base fee

---

## 6. Execution Pipeline Determinism

### 10-Step Pipeline: Proposer vs Validator

| Step | Description | Proposer | Validator |
|------|-------------|----------|-----------|
| 1 | Sort native actions | sort_native_actions | sort_native_actions |
| 2 | Pre-EVM native batch | execute_batch(pre_evm) | execute_batch(pre_evm) |
| 3 | EVM execution | execute_block | execute_block |
| 4 | Post-EVM native batch | execute_batch(post_evm) | execute_batch(post_evm) |
| 5 | Drain CoreWriter | **MISSING** | drain_core_writer |
| 6 | Process governance | **MISSING** | process_governance |
| 7 | Oracle aggregation | **MISSING** | **MISSING** |
| 8 | Distribute fees | **MISSING** | distribute_fees |
| 9 | Liquidation checks | **MISSING** | **MISSING** |
| 10 | Epoch boundary | **MISSING** | process_epoch_boundary |

### Sources of Non-Determinism

1. **`action_sort_key` uses `Debug` format** (native_executor.rs:870-874): output depends on compiler/crate version
2. **`serde_json` block hash** (committer.rs:32-34): field ordering depends on struct declaration
3. **Floating-point in RPC** (eth.rs:872-874): display only, not consensus-path — safe
4. **Timestamps**: `header.timestamp` set by proposer, no clock skew validation
5. **Arc<DB> sharing**: deterministic if execution order is identical, but pipeline divergence (EVM-FIND-01) breaks this

---

## 7. RPC Compatibility Assessment

### Deviations from Ethereum JSON-RPC Specification

| Endpoint | Deviation | Breaks |
|----------|-----------|--------|
| `eth_call` | Ignores block tag, always uses latest | hardhat, ethers.js historical calls |
| `eth_estimateGas` | Returns gas number on revert instead of error | MetaMask, hardhat test runner |
| `eth_getLogs` | No block range limit (only log count cap) | DoS vector |
| `eth_getBlockByNumber` | `transactions_root` always 0x0 | block explorers, light clients |
| `eth_getBlockByNumber` | Block hash is keccak256(JSON) not keccak256(RLP) | hash verification tools |
| `eth_getBlockByNumber` | `mix_hash` always 0x0 (should be prevrandao) | prevrandao-dependent contracts |
| `eth_call` | Nonce defaults to 0, chain_id hardcoded 7777 | foundry cast, ethers.js |
| `eth_getTransactionByHash` | Missing `accessList` field | viem, ethers.js v6 |
| `eth_getTransactionByHash` | Legacy `v` uses raw 27/28 not EIP-155 | signature verification |
| `eth_feeHistory` | `reward` always returns 0x0 | MetaMask gas estimation |

### Missing Endpoints

| Endpoint | Impact |
|----------|--------|
| `eth_getProof` (EIP-1186) | Blocks bridges and light clients |
| `eth_createFilter` / `eth_getFilterLogs` | Breaks ethers.js v5 polling |
| `eth_syncing` | Tools can't determine node readiness |

### Priority Ranking

1. **P0**: `eth_call` ignoring block tag, `eth_estimateGas` returning gas on failure, missing `accessList`
2. **P1**: `transactions_root = 0x0`, `feeHistory.reward = 0`, nonce default to 0
3. **P2**: Missing `eth_getProof`, missing filters, `eth_getLogs` DoS, block hash incompatibility
4. **P3**: Legacy `v` value, `total_difficulty = 0`, `mix_hash = 0`, hardcoded epoch length

---

## 8. Severity Classification Summary

### Critical (6)

| ID | File | Description |
|----|------|-------------|
| EVM-FIND-01 | proposer.rs / validator.rs | Proposer/validator pipeline divergence — validate_block_with_native always fails |
| EVM-PF-08 | db.rs:155-162 | BLOCKHASH opcode returns JSON bytes, not block hash |
| EVM-PF-12 | state_root.rs:66-73 | Native root has no length framing — hash collisions possible |
| EVM-PF-10 | precompiles.rs:219-246 | Write precompiles have zero gas + not registered with revm |
| EVM-FIND-02 | lib.rs:159-161 | Mempool memory_used never decremented — permanent pool lockout |
| EVM-FIND-04 | committer.rs:32-34 | serde_json block hash not deterministic across versions |

### High (15)

| ID | File | Description |
|----|------|-------------|
| EVM-PF-13 | executor.rs:159-175 | Gas limit checked after transact_commit |
| EVM-PF-01 | eip1559.rs:16,24-26 | Division by zero when gas_limit < 2 |
| EVM-PF-09 | validator.rs:191-201 | Oracle aggregation and liquidation never called |
| EVM-PF-11 | snapshot.rs:133-136 | Snapshot verification compares wrong root types |
| CONS-PF-13 | validator.rs:43-89 | receipts_root and logs_bloom never verified |
| CONS-PF-14 | validator.rs:52-58 | base_fee trusted without verification |
| ECON-PF-04 | lockbox.rs:49-54 | Non-atomic lockbox deposit/withdraw |
| EVM-PF-06 | precompiles.rs:700-702 | u128->i128 overflow in precompile inputs |
| EVM-FIND-03 | committer.rs:29-70 | Non-atomic block commit (no WriteBatch) |
| EVM-FIND-05 | lib.rs:192-196 | EIP-712 nonce/chainID not validated at mempool |
| EVM-FIND-06 | snapshot.rs:177-183 | Non-atomic snapshot restore — data loss on crash |
| EVM-FIND-07 | lockbox.rs:53 | FixedPoint i128 overflow on credit |
| EVM-FIND-08 | validate.rs:70-77 | Future nonce griefing — no gap limit |
| EVM-FIND-09 | precompiles.rs:164-166 | encode_fp_as_u128 misrepresents negatives |
| EVM-PF-05 | lockbox.rs:113-124 | Direct CF_ACCOUNTS bypass of StateDb |

### Medium (14)

| ID | File | Description |
|----|------|-------------|
| EVM-PF-02 | executor.rs:292,294 | Silent u128->u64 truncation in effective gas price |
| EVM-PF-03 | state_root.rs:93 | all_accounts() O(n) memory — OOM risk |
| EVM-PF-07 | state_root.rs:68 | Iterator errors silently ignored |
| EVM-PF-14 | eth.rs:744-761 | eth_call ignores block tag |
| EVM-PF-15 | eth.rs:800-852 | eth_getLogs no block range limit |
| EVM-PF-16 | precompiles.rs:917-943 | next_sequence O(n) scan per enqueue |
| CONS-PF-04 | proposer.rs:167 | epoch_length=0 and max_validators=0 hardcoded |
| EVM-FIND-10 | eth.rs:785-787 | eth_estimateGas returns gas on failure |
| EVM-FIND-11 | native_pool.rs:164-168 | Dedup uses Debug format — unstable |
| EVM-FIND-12 | native_executor.rs:620-621 | CoreWriter drain errors silently swallowed |
| EVM-FIND-13 | native_executor.rs:870-874 | action_sort_key uses Debug format |
| EVM-FIND-14 | lib.rs:147-153 | find_latest_height is O(N) linear scan |
| EVM-FIND-15 | lib.rs:200-219 | submit_native_action bypasses signature verification |
| EVM-FIND-16 | proposer.rs:244 | .expect() panics on serialization failure |

### Low (4)

| ID | File | Description |
|----|------|-------------|
| EVM-PF-17 | eth.rs:370 | transactions_root always B256::ZERO |
| EVM-PF-18 | eth.rs:196-199 | eth_call nonce defaults to 0 |
| EVM-FIND-18 | eth.rs:225 | Legacy v value uses raw 27/28 |
| EVM-FIND-20 | committer.rs:91-93 | Orphaned storage on same-block create+destroy |

### Informational (4)

| ID | File | Description |
|----|------|-------------|
| EVM-FIND-21 | cf.rs:15-16 | CF_LOGS and CF_LOGS_BLOOM never populated |
| EVM-FIND-22 | executor.rs | No unit tests for critical execution paths |
| — | executor.rs:118,220 | BundleRetention::Reverts memory overhead |
| — | bloom.rs | Custom bloom reimplements alloy_primitives logic |

---

## Appendix: Checklist Cross-Reference

| Check | Status | Finding | Check | Status | Finding |
|-------|--------|---------|-------|--------|---------|
| CHK-01 | PASS | SpecId::CANCUN | CHK-44 | PASS | 72-byte format consistent |
| CHK-02 | FAIL | EVM-PF-13 | CHK-45 | PASS | all_accounts includes contracts |
| CHK-03 | PASS | Sequential state correct | CHK-46 | PASS | account_storage all slots |
| CHK-04 | PASS | checked_add | CHK-47 | FAIL | EVM-PF-08 critical |
| CHK-05 | PASS | Logs gated on success | CHK-48 | PASS | Vestigial |
| CHK-06 | FAIL | EVM-PF-02 | CHK-49 | PASS | Overlay isolation (unused) |
| CHK-07 | INFO | Memory trade-off | CHK-50 | PASS | Rollback via drop |
| CHK-08 | PASS | EIP-1559 formula | CHK-51 | PASS | Storage root correct |
| CHK-10 | FAIL | EVM-PF-01 | CHK-52 | PASS | State root via reth |
| CHK-12 | PASS | fee_delta.max(1) | CHK-53 | PASS | Single definition |
| CHK-13 | PASS | saturating_sub | CHK-54 | PASS | Pruner safe |
| CHK-14 | PASS | Matches go-ethereum | CHK-55 | FAIL | EVM-PF-11 |
| CHK-15 | PASS | Bloom correct | CHK-56 | FAIL | EVM-FIND-06 |
| CHK-16 | PASS | EMPTY_ROOT_HASH | CHK-57 | PASS | 10% bump |
| CHK-17 | FAIL | EVM-PF-12 | CHK-58 | PASS | BTreeMap nonce order |
| CHK-18 | PASS | CF order deterministic | CHK-59 | PASS | Chain ID checked |
| CHK-19 | FAIL | EVM-PF-07 | CHK-60 | FAIL | EVM-FIND-08 |
| CHK-20 | PASS | Composite formula | CHK-61 | PASS | Write lock safe |
| CHK-21 | FAIL | EVM-PF-03 | CHK-62 | FAIL | EVM-FIND-02 |
| CHK-23 | PASS | EIP-161 correct | CHK-64 | FAIL | Wrong eviction order |
| CHK-24 | PASS | Zero-slot defense | CHK-65 | FAIL | EVM-FIND-11 |
| CHK-25 | PASS | State before metadata | CHK-66 | FAIL | EVM-FIND-19 |
| CHK-26 | FAIL | EVM-FIND-04 | CHK-67 | FAIL | EVM-FIND-05 |
| CHK-27 | PASS | EVM-only indexing | CHK-68 | PASS | EIP-155 via alloy |
| CHK-28 | PASS | SELFDESTRUCT guard | CHK-69 | FAIL | EVM-PF-14 |
| CHK-29 | PARTIAL | EVM-FIND-20 | CHK-70 | FAIL | EVM-FIND-10 |
| CHK-30 | PASS | Base fee from parent | CHK-71 | FAIL | EVM-PF-15 |
| CHK-31 | PASS | Default 30M | CHK-72 | FAIL | EVM-PF-17 |
| CHK-32 | PASS | Canonical tx hash | CHK-73 | FAIL | EVM-PF-18 |
| CHK-33 | FAIL | EVM-FIND-01 | CHK-77 | PASS | No precompile collision |
| CHK-34 | PARTIAL | Pipeline divergence | CHK-78 | PASS | Input validated |
| CHK-35 | FAIL | EVM-FIND-16 | CHK-79 | FAIL | EVM-FIND-09 |
| CHK-36 | PASS | EVM-only by design | CHK-80 | FAIL | EVM-PF-06 |
| CHK-37 | PASS | Gas mismatch check | CHK-82 | FAIL | EVM-PF-10 |
| CHK-38 | PASS | State root check | CHK-83 | FAIL | EVM-PF-16 |
| CHK-39 | FAIL | EVM-FIND-01 | CHK-84 | PASS | One-block delay |
| CHK-40 | FAIL | CONS-PF-04 | CHK-90 | PASS | Balance checked |
| CHK-42 | PASS | EIP-2718 types | CHK-92 | FAIL | EVM-FIND-07 |
| CHK-43 | PASS | Canonical hash | CHK-96 | FAIL | ECON-PF-04 |
| — | — | — | CHK-97 | FAIL | EVM-FIND-01 |

---

*End of Audit Report — Phase 3.4.3, Instance B (EVM Correctness)*
