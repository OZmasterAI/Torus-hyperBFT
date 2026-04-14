# Torus-hyperBFT Security Audit Scope

**Date**: 2026-04-14
**Version**: 1.0 — Self-audit preparation
**Codebase**: ~55k lines Rust, 16 crates + 2 tools
**Commit**: master branch (pre-audit snapshot)

---

## 1. Methodology

### 1.1 Audit Structure

Three parallel audit instances consume this scope:

| Instance | Domain | Lead Crates | Focus |
|----------|--------|-------------|-------|
| A (3.4.2) | Consensus Safety | hotstuff_rs, torus-consensus, torus-network | BFT correctness, liveness, slashing |
| B (3.4.3) | EVM Correctness | torus-evm, torus-bridge, torus-state, torus-mempool, torus-rpc | State roots, execution, gas, precompiles |
| C (3.4.4) | Economic Model | torus-core, torus-economics, torus-bridge/native_executor | Order book, margin, staking, governance |

### 1.2 Static Analysis Summary

#### Clippy (pedantic)

| Crate | Warnings | Notes |
|-------|----------|-------|
| hotstuff_rs | 740 | Largest crate; mostly style/doc issues |
| torus-core | 212 | must_use, doc, redundant closures |
| torus-economics | 156 | doc, must_use |
| torus-explorer | 117 | doc, style |
| torus-rpc | 86 | doc, style |
| torus-state | 69 | doc, style |
| torus-bridge | 60 | doc, style |
| torus-consensus | 45 | doc, style |
| torus-network | 37 | doc, style |
| torus-mempool | 31 | doc, style |
| torus-types | 30 | must_use, doc |
| torus-evm | 20 | doc |
| Other (tools) | 44 | faucet, wallet, node, genesis, telemetry |

No soundness-critical clippy findings. Mostly `doc_markdown`, `must_use`, `missing_errors_doc`.

#### Cargo Audit

| Advisory | Crate | Severity | Impact |
|----------|-------|----------|--------|
| RUSTSEC-2025-0055 | tracing-subscriber 0.2.25 | Medium | Log injection via ANSI escape sequences |
| RUSTSEC-2024-0388 | derivative 2.2.0 | Warning | Unmaintained (transitive via ark-ff) |
| RUSTSEC-2024-0436 | paste 1.0.15 | Warning | Unmaintained |
| 2 others | — | Warning | Unmaintained transitive deps |

`tracing-subscriber` vulnerability is in a transitive dep (ark-relations → revm-precompile). Not directly exploitable in consensus but could enable log spoofing.

#### Cargo Deny

Not configured (`deny.toml` absent). **Recommendation**: add license and advisory checks.

#### Key Dependency Versions

| Dependency | Version | Notes |
|------------|---------|-------|
| revm | 36.0.0 | EVM executor; Cancun spec |
| alloy-consensus | 1.8.3 | Ethereum primitives |
| libp2p | 0.56.0 | Networking |
| rocksdb | 0.24.0 | State storage |
| jsonrpsee | 0.26.0 | JSON-RPC server |
| reth-trie-common | 2.0.0 (git) | MPT state root computation |
| ethnum | — | i256 for FixedPoint overflow protection |

#### Unsafe Code Inventory

Minimal unsafe usage — only in `hotstuff_rs`:

| File | Line | Usage | Risk |
|------|------|-------|------|
| hotstuff_rs/src/block_tree/accessors/internal.rs | 97 | `pub unsafe fn new_unsafe(kv_store)` | Recovery path for corrupted block tree — intentional |
| hotstuff_rs/src/block_tree/accessors/internal.rs | 808 | `pub fn new_unsafe()` (WriteBatch) | Not actually `unsafe fn` — naming convention only |

**Assessment**: No true unsafe blocks in application crates. The `unsafe` in hotstuff_rs is a named convention for "bypasses invariant checks" rather than Rust's `unsafe` keyword in the function body.

#### Panic Surface (unwrap/expect)

| Crate | Count (non-test) | Risk Level |
|-------|-------------------|------------|
| torus-economics | 119 | **HIGH** — financial logic |
| torus-rpc | 109 | Medium — user-facing |
| torus-explorer | 80 | Low — indexer |
| hotstuff_rs | 76 | **HIGH** — consensus |
| torus-state | 61 | **HIGH** — storage layer |
| torus-mempool | 55 | Medium — tx processing |
| torus-core | 49 | **HIGH** — order book/margin |
| torus-genesis | 31 | Low — one-time init |
| torus-consensus | 20 | **HIGH** — consensus bridge |
| torus-node | 16 | Low — startup |
| torus-network | 13 | Medium — networking |
| torus-types | 9 | Medium — shared types |
| torus-evm | 0 | Clean |
| torus-bridge | 1 | Clean |

**Notable**: `torus-evm` has **zero** unwrap/expect calls. `torus-bridge` has only 1. These are the most critical execution-path crates and are well-written.

#### Truncation Cast Surface

| Crate | Count (non-test) | Risk Level |
|-------|-------------------|------------|
| torus-core | 61 | **HIGH** — FixedPoint ↔ raw conversions |
| hotstuff_rs | 37 | **HIGH** — power/view calculations |
| torus-rpc | 23 | Medium — display formatting |
| torus-explorer | 16 | Low — indexer |
| torus-evm | 11 | **HIGH** — gas/fee calculations |
| torus-economics | 8 | Medium — epoch/staking |
| torus-types | 7 | Medium — EIP-712 encoding |
| torus-network | 6 | Low — codec |
| torus-bridge | 5 | Medium — proposal metadata |

---

## 2. Test Coverage Inventory

### 2.1 Per-Crate Test Counts

| Crate | Lines | #[test] | Test Files | Test Modules | Tests/kLOC | Coverage Assessment |
|-------|-------|---------|------------|--------------|------------|---------------------|
| hotstuff_rs | 15,545 | 38 | 13 | 0 | 2.4 | **Low** — complex consensus logic under-tested |
| torus-core | 7,471 | 143 | 6 | 3 | 19.1 | **Good** — order book and oracle well-tested |
| torus-economics | 6,119 | 88 | 3 | 4 | 14.4 | **Moderate** — staking/governance tested, rewards less so |
| torus-integration-tests | 5,239 | 105 | 14 | 0 | 20.0 | **Good** — broad e2e coverage |
| torus-rpc | 3,629 | 2 | 1 | 1 | 0.6 | **Critical gap** — almost no tests |
| torus-bridge | 2,921 | 31 | 2 | 0 | 10.6 | **Moderate** — proposer/validator tested |
| torus-state | 2,344 | 44 | 1 | 2 | 18.8 | **Good** — state operations covered |
| torus-mempool | 1,985 | 31 | 0 | 3 | 15.6 | **Good** — pool mechanics tested |
| torus-explorer | 1,698 | 15 | 0 | 3 | 8.8 | **Low** — indexer under-tested |
| torus-types | 1,503 | 18 | 0 | 2 | 12.0 | **Moderate** — FixedPoint and EIP-712 |
| torus-network | 1,487 | 10 | 1 | 1 | 6.7 | **Low** — minimal network tests |
| torus-consensus | 1,384 | 14 | 1 | 2 | 10.1 | **Moderate** — slashing tested |
| torus-evm | 877 | 18 | 1 | 2 | 20.5 | **Good** — EIP-1559 and execution |
| torus-node | 678 | 7 | 0 | 2 | 10.3 | **Low** — startup only |
| torus-genesis | 575 | 8 | 0 | 1 | 13.9 | **Moderate** |
| torus-telemetry | 239 | 1 | 0 | 1 | 4.2 | **Low** |

### 2.2 Integration Test Coverage Map

| Test File | Domains | Coverage |
|-----------|---------|----------|
| e2e_trading.rs | ECON, EVM | Full order lifecycle, partial fills, multi-market |
| staking_lifecycle.rs | ECON, CONS | Delegate/undelegate/permanent, epoch rotation, rewards |
| fee_flow.rs | ECON, EVM | 4-way fee split, conservation invariant |
| lockbox_e2e.rs | EVM, ECON | EVM↔native transfers, atomicity |
| dynamic_validators.rs | CONS, ECON | Validator set changes, key rotation, jailing |
| slashing_jailing.rs | CONS | Double-sign lifecycle, downtime, jail votes |
| cross_vm_read.rs | EVM, ECON | Precompile reads (order book, oracle, balances) |
| cross_vm_write.rs | EVM, ECON | CoreWriter precompile actions |
| rate_limit_mev.rs | EVM, CONS | Rate limiting, anti-MEV ordering |
| snapshot_dos_keys.rs | EVM, STATE | Snapshot lifecycle, corrupted metadata |
| chaos.rs | ALL | Mixed load, spammer throttling |
| stress.rs | EVM, ECON | Sustained throughput |

### 2.3 Critical Untested Paths

| Component | Missing Coverage | Risk |
|-----------|-----------------|------|
| torus-rpc | Almost all eth_* endpoints untested (2 tests total) | **Critical** — RPC is the primary user interface |
| hotstuff_rs/implementation.rs | MonadBFT NEC path, speculative finality edge cases | **Critical** — core consensus |
| torus-bridge/validator.rs | `validate_block_with_native` full pipeline not tested in isolation | **High** — validation correctness |
| torus-network | No tests for gossip message authentication | **High** — network attack surface |
| Lockbox crash recovery | No test for crash between debit/credit | **High** — fund safety |
| FixedPoint overflow | No test for i128 overflow in add/sub | **High** — financial correctness |
| Native state root ordering | No test that RocksDB iterator order is deterministic across restarts | **Medium** — consensus divergence risk |

---

## 3. Domain A: Consensus Safety

### 3.1 Threat Model

**Attackers**:
- Byzantine validators (up to f = ⌊(n-1)/3⌋ of total power)
- Network adversary (can delay, reorder, drop messages between honest nodes)
- External DoS attacker (flood messages, exploit amplification)
- Colluding subset of validators attempting equivocation, censorship, or liveness attacks

**Goals**:
- **Safety violation**: cause two honest nodes to commit different blocks at the same height
- **Liveness violation**: halt chain progress (no new blocks committed)
- **Slashing evasion**: double-sign without being detected/penalized
- **Validator set manipulation**: gain disproportionate influence through epoch transition bugs
- **Censorship**: prevent specific transactions from being included

**Assumptions that must hold**:
- At most f = ⌊(n-1)/3⌋ of total voting power is Byzantine
- Network eventually delivers messages between honest nodes (partial synchrony)
- Ed25519 signatures are unforgeable
- SHA-256 is collision-resistant
- System clock skew is bounded (for timeout calculations)

### 3.2 Crates in Scope

| Crate | Files | Lines | Role |
|-------|-------|-------|------|
| hotstuff_rs | 54 | 15,545 | Core BFT consensus engine |
| torus-consensus | 7 | 1,384 | App trait, slashing, validator set |
| torus-network | 11 | 1,487 | libp2p networking, gossip |
| torus-bridge (partial) | 2 | ~600 | proposer.rs, validator.rs |
| torus-types (partial) | 1 | ~200 | ValidatorSet, PublicKey |

### 3.3 Per-File Checklist

#### hotstuff_rs/src/hotstuff/implementation.rs — Core HotStuff Protocol

- **CONS-CHK-01** (line ~350): Quorum calculation `total_power - 1) / 3` — verify BFT threshold is correctly `2f+1` where `f = ⌊(n-1)/3⌋`. Off-by-one here breaks safety.
- **CONS-CHK-02** (line ~352): `kappa = (f + 1) as usize` for NEC (MonadBFT no-equivocation certificate). Verify κ guarantees at least one honest responder.
- **CONS-CHK-03**: Vote deduplication — verify a validator cannot submit multiple votes for different blocks at the same view/phase (the basis of double-sign detection).
- **CONS-CHK-04**: Certificate validation — verify QC/TC aggregate signatures are checked against the current validator set, not a stale one.
- **CONS-CHK-05**: View change — verify honest nodes only enter a new view with a valid timeout certificate or QC, preventing view reversion.

#### hotstuff_rs/src/pacemaker/implementation.rs — Pacemaker & Leader Election

- **CONS-CHK-06** (line 743): Leader election `view.int() % (p_total.int() as u64)` — verify leader is deterministic and not manipulable. Check that `p_total` cannot be 0 (division by zero).
- **CONS-CHK-07** (line 792): `reputation.score_bps(&vk) as u64` — leader reputation scoring. Verify score cannot be manipulated by withholding votes.
- **CONS-CHK-08** (line 805): `is_epoch_change_view` — `view.int() % (epoch_length.int() as u64) == 0`. Verify epoch_length cannot be 0 (division by zero in modulo).
- **CONS-CHK-09** (line 810): `epoch()` — `view.int().div_ceil(epoch_length.int() as u64)`. Same division-by-zero concern.
- **CONS-CHK-10** (line 836): Timeout calculation uses `total_power` — verify timeout escalation cannot be exploited to force premature view changes.
- **CONS-CHK-11** (line 258-259): Pacemaker quorum check — same threshold formula as implementation.rs. Verify consistency.
- **CONS-CHK-12** (line 481): Cross-epoch view change — verify validator set transitions at epoch boundaries don't create a window where neither old nor new set has quorum.
- **CONS-CHK-13** (line 581): `epoch_view = epoch * config.epoch_length.int() as u64` — verify no overflow for large epoch numbers.

#### hotstuff_rs/src/block_tree/invariants.rs — Block Tree Safety Invariants

- **CONS-CHK-14**: Commit rule — verify a block is only committed when it has a chain of 3 certified blocks (prepare, precommit, commit QCs per HotStuff).
- **CONS-CHK-15**: Locked value — verify the locked block cannot regress (monotonicity of the locked QC view).
- **CONS-CHK-16** (line 273): Comment mentions "unsafe to commit" — verify the actual invariant check is correct.

#### hotstuff_rs/src/block_tree/accessors/internal.rs — Block Tree State

- **CONS-CHK-17** (line 97): `new_unsafe` bypasses invariant checks — verify this is only used in recovery paths, never in normal operation.
- **CONS-CHK-18**: Block storage — verify blocks are keyed by hash, not height, to prevent hash collision attacks.

#### hotstuff_rs/src/hotstuff/types.rs — Consensus Types

- **CONS-CHK-19** (line 630): `(self.successes as u64) * 10_000 / (self.total as u64)) as u32` — division by zero if `total == 0`. **(Pre-finding)**
- **CONS-CHK-20**: Reputation struct — verify reputation scores are bounded and cannot overflow u32.

#### hotstuff_rs/src/networking/receiving.rs — Message Reception

- **CONS-CHK-21** (line 248): `mem::size_of::<VerifyingKey>() as u64 + msg.size()` — verify message size calculation doesn't overflow u64 (unlikely but check).
- **CONS-CHK-22** (line 301-322): Rate limiting by message type — verify rate limits are sufficient to prevent memory exhaustion.
- **CONS-CHK-23**: Message authentication — verify all consensus messages are signed and signatures verified before processing.

#### hotstuff_rs/src/block_sync/ — Block Synchronization

- **CONS-CHK-24** (client.rs:243): `view_difference = (advertise_pc.highest_pc.view - highest_view_entered) as u64` — unsigned subtraction could underflow if views are out of order.
- **CONS-CHK-25**: Sync protocol — verify a malicious peer cannot feed invalid blocks during sync that would corrupt local state.

#### hotstuff_rs/src/types/data_types.rs — Numeric Types

- **CONS-CHK-26** (line 298): `self.0.add_assign(rhs.0 as u128)` — Power addition could overflow u128 for extreme values.
- **CONS-CHK-27** (line 428): `(self.0 as i64).sub(rhs.0 as i64)` — ViewNumber subtraction with signed cast, potential truncation/overflow for large view numbers.

#### torus-consensus/src/app.rs — Consensus App Bridge

- **CONS-CHK-28** (line 212-215): `datums.len() != 1` check — verify this correctly rejects multi-datum blocks. What about empty datums?
- **CONS-CHK-29** (line 216): `datums[0].bytes()` — array index access after length check. Safe but fragile.
- **CONS-CHK-30** (line 231-244): Block validation calls `validate_block` (EVM-only) when native actions exist. **(Pre-finding — see CONS-PF-02)**
- **CONS-CHK-31** (line 285): `serde_json::to_vec(&block).expect(...)` — panic in block production if serialization fails.
- **CONS-CHK-32** (line 288): `self.last_header = block.header.clone()` — updates local state before consensus confirmation. If block is rejected, state is stale. **(Pre-finding)**
- **CONS-CHK-33** (line 337-345): Leader address derivation from ed25519 pubkey uses SHA-256 last-20-bytes — verify this matches the actual validator address derivation used elsewhere.
- **CONS-CHK-34** (line 347-352): Slashing in `on_speculative_rollback` uses hardcoded 500 bps (5%) — verify this matches protocol specification.
- **CONS-CHK-35** (line 391): `produce_empty_block` sets `state_root: B256::ZERO` — empty blocks have zero state root. **(Pre-finding — see CONS-PF-01)**

#### torus-consensus/src/slashing.rs — Double-Sign & Downtime Detection

- **CONS-CHK-36** (line 67-95): `DoubleSignEvidence::verify()` — verify evidence verification is complete (checks both signatures, different block hashes, same view/phase).
- **CONS-CHK-37** (line 138-177): `record_vote` — verify sliding window doesn't allow evidence to be lost by pruning.
- **CONS-CHK-38** (line 258): `total_blocks = self.block_signers.len() as u64` — truncation if > 2^64 blocks (theoretical).
- **CONS-CHK-39** (line 263): `threshold = total_blocks * threshold_pct / 100` — integer division truncation could make threshold too lenient.

#### torus-consensus/src/kv_store.rs — Consensus KV Store

- **CONS-CHK-40** (line 40): `DB::open_cf_descriptors(...).expect("open consensus DB")` — panic on DB open failure. Acceptable for startup but should be documented.
- **CONS-CHK-41** (line 104-126): Multiple `.expect()` calls on CF operations — panics if column family is missing.

#### torus-network/src/peer_scoring.rs — Peer Reputation

- **CONS-CHK-42**: Verify peer scoring cannot be exploited to disconnect honest validators.
- **CONS-CHK-43**: Verify ban list threshold prevents an attacker from accumulating enough negative score through legitimate-looking messages.

#### torus-network/src/codec.rs — Network Codec

- **CONS-CHK-44** (line 85): `u32::from_be_bytes(len_buf) as usize` — message length from network. Verify maximum message size is enforced to prevent OOM.
- **CONS-CHK-45** (line 105): `(buf.len() as u32).to_be_bytes()` — truncation if buffer > 4GB.

#### torus-network/src/behaviour.rs — libp2p Behaviour

- **CONS-CHK-46** (line 82-84): Connection limits cast from usize to u32 — verify limits are reasonable and not bypassable.

#### torus-bridge/src/proposer.rs — Block Proposal (Consensus boundary)

- **CONS-CHK-47** (line 99-100): `evm_tx_count: evm_transactions.len() as u32` — truncation if > 4B transactions.
- **CONS-CHK-48** (line 167-168): `epoch_length: 0, max_validators: 0` hardcoded — must be set by production caller. **(Pre-finding)**

#### torus-bridge/src/validator.rs — Block Validation (Consensus boundary)

- **CONS-CHK-49** (line 69-74): Gas used mismatch check — verify this is sufficient to prevent invalid blocks.
- **CONS-CHK-50** (line 76-83): State root mismatch check — the core validity check.
- **CONS-CHK-51** (line 149-150): `epoch_length: 0, max_validators: 0` hardcoded. **(Pre-finding)**

### 3.4 Fuzz Targets

| Function | Invariant to Check |
|----------|--------------------|
| `DoubleSignDetector::record_vote` | Never produces false positives; always detects conflicting votes in window |
| `DowntimeTracker::detect_downtime` | Threshold calculation matches spec; no off-by-one |
| `leader_for_view` (pacemaker) | Deterministic for same inputs; within validator set bounds |
| `epoch()` / `is_epoch_change_view()` | Consistent epoch numbering; no gaps or overlaps |
| `do_validate` (app.rs) | Rejects all invalid blocks; accepts all valid blocks |
| `sort_native_actions` | Deterministic ordering; pre/post-EVM classification correct |
| Message deserialization (networking) | No panic on malformed input |

### 3.5 Property Test Invariants

1. **Safety**: If two honest nodes commit blocks at height h, the blocks are identical
2. **Liveness**: If ≤f validators are Byzantine and network is synchronous, new blocks are eventually committed
3. **Quorum overlap**: Any two quorums (2f+1 each out of 3f+1) share at least one honest validator
4. **Monotonic locking**: The locked QC view number never decreases
5. **Epoch continuity**: Validator set changes at epoch boundaries maintain quorum overlap between consecutive epochs
6. **Deterministic leader**: Same (view, validator_set) → same leader
7. **Evidence validity**: `DoubleSignEvidence::verify()` returns true iff both signatures are valid for different blocks at same (view, phase)
8. **Slashing finality**: A slashed validator cannot re-enter the active set (tombstone is permanent)

### 3.6 Cross-Domain Boundaries

| File | Consensus Checks | Delegates to |
|------|-----------------|--------------|
| torus-bridge/src/proposer.rs | Block construction, tx ordering | EVM (execution), Economics (native action sorting) |
| torus-bridge/src/validator.rs | State root verification, gas check | EVM (re-execution), Economics (native pipeline) |
| torus-consensus/src/app.rs | Datum hash, block validity, epoch transitions | EVM (validate_block), Economics (staking/epoch) |
| torus-bridge/src/native_executor.rs | Action ordering (anti-MEV) | Economics (all native actions) |

---

## 4. Domain B: EVM Correctness

### 4.1 Threat Model

**Attackers**:
- Malicious contract deployer (reentrancy, gas griefing, precompile abuse)
- Transaction spammer (mempool flooding, tx ordering manipulation)
- MEV extractor (frontrunning, sandwich attacks via precompile interaction)
- Validator-proposer (selectively include/exclude transactions, manipulate base fee)

**Goals**:
- **State divergence**: cause honest nodes to compute different state roots
- **Fund theft**: exploit precompile or lockbox to create/destroy tokens
- **Gas manipulation**: bypass gas limits, cause block gas overflow
- **State corruption**: break overlay isolation, corrupt trie
- **RPC inconsistency**: make eth_call return results different from actual execution

**Assumptions that must hold**:
- revm 36 (Cancun spec) is correct
- reth-trie MPT implementation matches go-ethereum
- RocksDB provides atomic batch writes within a single WriteBatch
- EIP-1559 base fee algorithm follows the Ethereum specification
- Block execution order (native → EVM → lockbox → fees) is deterministic

### 4.2 Crates in Scope

| Crate | Files | Lines | Role |
|-------|-------|-------|------|
| torus-evm | 6 | 877 | revm wrapper, EIP-1559, bloom |
| torus-bridge | 10 | 2,921 | Proposal, validation, commitment, native execution |
| torus-state | 9 | 2,344 | RocksDB, trie, overlay, pruner, snapshots |
| torus-mempool | 6 | 1,985 | EVM tx pool, native action pool, rate limiting |
| torus-rpc | 8 | 3,629 | eth_*, torus_* JSON-RPC |
| torus-core/src/precompiles.rs | 1 | ~1,200 | Native CLOB precompiles |
| torus-core/src/lockbox.rs | 1 | ~174 | EVM↔Native balance bridge |
| torus-types (partial) | 2 | ~560 | TorusBlock, Receipt, ChainConfig |

### 4.3 Per-File Checklist

#### torus-evm/src/executor.rs — EVM Execution Engine

- **EVM-CHK-01** (line 104): `SpecId::CANCUN` — verify Cancun is the intended hard fork. No Shanghai, no Prague features.
- **EVM-CHK-02** (line 155-175): Block gas limit enforcement — cumulative gas checked after each tx. Verify a single tx exceeding gas limit is handled.
- **EVM-CHK-03** (line 159): `evm.transact_commit(tx)` — commits each tx immediately. Verify this provides proper transaction isolation (each tx sees results of previous ones).
- **EVM-CHK-04** (line 162-168): `checked_add(gas_used)` — correct overflow protection. Good.
- **EVM-CHK-05** (line 180-187): Logs only from successful txs — matches Ethereum behavior. Good.
- **EVM-CHK-06** (line 199-200): `calc_effective_gas_price` — see EVM-PF-02 for truncation risk.
- **EVM-CHK-07** (line 206): `tx_index: idx as u32` — truncation if > 4B txs per block (practically impossible).
- **EVM-CHK-08** (line 217-222): Bundle retention uses `BundleRetention::Reverts` — verify revert data is actually needed (memory impact).
- **EVM-CHK-09** (line 288-295): `calc_effective_gas_price` — `(base_fee as u128 + max_priority) as u64`. **(Pre-finding)**

#### torus-evm/src/eip1559.rs — Base Fee Calculation

- **EVM-CHK-10** (line 16): `gas_target = gas_limit / ELASTICITY_MULTIPLIER` — if gas_limit < 2, gas_target = 0. **(Pre-finding)**
- **EVM-CHK-11** (line 24-26): Division by `gas_target as u128` — panics if gas_target = 0. **(Pre-finding)**
- **EVM-CHK-12** (line 28): `fee_delta.max(1)` — correct minimum increase. Matches Ethereum.
- **EVM-CHK-13** (line 34): `base_fee.saturating_sub(fee_delta)` — correct floor at 0. Matches Ethereum.
- **EVM-CHK-14**: Verify the calculation matches EIP-1559 spec exactly (same integer arithmetic as go-ethereum).

#### torus-evm/src/bloom.rs — Bloom Filter

- **EVM-CHK-15** (line 33): `(hash[2 * i] as usize) << 8 | hash[2 * i + 1] as usize) & 0x7FF` — verify bloom bit calculation matches Ethereum's 2048-bit Bloom filter spec.

#### torus-bridge/src/state_root.rs — State Root Computation

- **EVM-CHK-16** (line 22-28): `compute_post_bundle_state_root` — uses `EMPTY_ROOT_HASH` for native root in Phase 1. Verify Phase 2 transition doesn't break existing blocks.
- **EVM-CHK-17** (line 48-81): `compute_native_state_root` — keccak256 of concatenated KV pairs. **(Pre-finding — see EVM-PF-04)**
- **EVM-CHK-18** (line 58-64): CF iteration order — relies on RocksDB iterator ordering being consistent. Verify `IteratorMode::Start` is deterministic across compactions.
- **EVM-CHK-19** (line 68): `if let Ok((key, value)) = item` — silently skips iterator errors. Should propagate or at least log.
- **EVM-CHK-20** (line 79-80): `compute_composite_root` — `keccak256(evm_root || native_root)`. Simple but correct composition.
- **EVM-CHK-21** (line 88-158): `compute_post_bundle_evm_root` — O(n) full scan via `all_accounts()`. **(Pre-finding)**
- **EVM-CHK-22** (line 93): `state_db.all_accounts()` — loads entire state into memory. DoS vector as state grows.
- **EVM-CHK-23** (line 111-115): EIP-161 state clearing — verify the clearing conditions match exactly (zero nonce + zero balance + empty code hash).
- **EVM-CHK-24** (line 126-137): Bundle storage merges — verify zero-value storage slots are correctly removed (not leaving ghost entries in trie).

#### torus-bridge/src/committer.rs — Block Commitment

- **EVM-CHK-25** (line 28-29): `apply_bundle_to_db` then store block data — verify the write order is correct (state before metadata).
- **EVM-CHK-26** (line 32-35): Block hash is `keccak256(serde_json(header))` — not standard Ethereum RLP encoding. Verify this is intentional and consistent between proposer and validator.
- **EVM-CHK-27** (line 60-71): Tx hash→location index only maps EVM transactions, not native actions. Verify native actions are retrievable by other means.
- **EVM-CHK-28** (line 64): `i as u32` cast — same truncation concern.
- **EVM-CHK-29** (line 78-101): `apply_bundle_to_db` — account deletion when `info` is None but `original_info` is Some. Verify this matches revm's SELFDESTRUCT handling.

#### torus-bridge/src/proposer.rs — Block Proposal

- **EVM-CHK-30** (line 50-54): `calc_next_block_base_fee` uses parent's gas stats — correct.
- **EVM-CHK-31** (line 57-61): Gas limit defaults to `DEFAULT_BLOCK_GAS_LIMIT` if parent is 0 — verify this only happens at genesis.
- **EVM-CHK-32** (line 77): `execute_block` with tx_envs — verify decoded TxEnvs match the raw RLP bytes 1:1.
- **EVM-CHK-33** (line 83): State root computed from bundle — verify this matches what validator will compute.
- **EVM-CHK-34** (line 196-197): `compute_native_state_root` reads from `state_db` BEFORE native execution writes are committed — potential stale read. **(Pre-finding)**
- **EVM-CHK-35** (line 244): `serde_json::to_vec(receipts).expect(...)` — panic if receipts can't serialize.

#### torus-bridge/src/validator.rs — Block Validation

- **EVM-CHK-36** (line 43-90): `validate_block` — only validates EVM, not native. **(Pre-finding — see CONS-PF-02)**
- **EVM-CHK-37** (line 69-74): Gas used mismatch detection — good check.
- **EVM-CHK-38** (line 76-83): State root mismatch detection — the core safety check.
- **EVM-CHK-39** (line 118-221): `validate_block_with_native` — full pipeline. Verify execution order matches proposer exactly (determinism requirement).
- **EVM-CHK-40** (line 149): `epoch_length: 0` hardcoded — epoch boundary won't trigger during validation. **(Pre-finding)**
- **EVM-CHK-41** (line 205-207): `compute_native_state_root` read timing — must match proposer's read timing or state roots diverge.

#### torus-bridge/src/decode.rs — RLP Transaction Decoding

- **EVM-CHK-42**: Verify RLP decoding handles all EIP-2718 transaction types (Legacy, EIP-2930, EIP-1559, EIP-4844).
- **EVM-CHK-43**: Verify tx_hash computation matches Ethereum (keccak256 of signed RLP).

#### torus-state/src/db.rs — State Database

- **EVM-CHK-44**: `put_account` / `get_account` — verify serialization format (72 bytes: balance[32] + nonce[8] + code_hash[32]) is consistent with lockbox raw access.
- **EVM-CHK-45**: `all_accounts()` — verify this returns ALL accounts including contracts, not just EOAs.
- **EVM-CHK-46**: `account_storage()` — verify this returns ALL storage slots for an account.

#### torus-state/src/overlay.rs — State Overlay

- **EVM-CHK-47**: Overlay isolation — verify overlays don't leak state between transactions.
- **EVM-CHK-48**: Overlay rollback — verify discarding an overlay restores exact previous state.

#### torus-state/src/trie.rs — MPT Computation

- **EVM-CHK-49** (line 19-30): `compute_storage_root` — filters zero values, hashes slot keys with keccak256. Matches Ethereum.
- **EVM-CHK-50** (line 37-43): `compute_state_root` — delegates to reth-trie `state_root_unhashed`. Verify reth implementation.
- **EVM-CHK-51** (line 79-84): `compute_composite_root` — `keccak256(evm_root || native_root)`. Verify this is the only place composite roots are computed (consistency).

#### torus-state/src/pruner.rs — State Pruning

- **EVM-CHK-52**: Verify pruning doesn't delete state needed by ongoing validation or sync.

#### torus-state/src/snapshot.rs — State Snapshots

- **EVM-CHK-53**: Verify snapshot integrity checks prevent corrupted snapshots from being restored.

#### torus-mempool/src/evm_pool.rs — EVM Transaction Pool

- **EVM-CHK-54** (line 138): `saturating_mul(100 + replacement_bump_pct as u128)` — replacement bump prevents spam. Verify minimum bump percentage matches Ethereum (10%).
- **EVM-CHK-55**: Nonce ordering — verify transactions from same sender are ordered by nonce.
- **EVM-CHK-56**: Replay protection — verify chain_id is checked during pool insertion.

#### torus-mempool/src/validate.rs — Transaction Validation

- **EVM-CHK-57**: Verify signature recovery matches EIP-155 (chain_id in v value).
- **EVM-CHK-58**: Verify gas price validation checks against current base fee.

#### torus-mempool/src/native_pool.rs — Native Action Pool

- **EVM-CHK-59**: EIP-712 signature verification for native actions — verify typed data hash matches spec.
- **EVM-CHK-60**: Deduplication — verify identical native actions are rejected.

#### torus-rpc/src/eth.rs — eth_* JSON-RPC

- **EVM-CHK-61** (line 292): `tx_index as u64` cast — verify consistency with block storage.
- **EVM-CHK-62** (line 592): `body.evm_transactions.get(tx_index as usize)` — verify bounds checking.
- **EVM-CHK-63** (line 662): `log_index_offset += r.logs.len() as u32` — log index could overflow with many logs.
- **EVM-CHK-64** (line 873): `evm_gas_used as f64 / evm_gas_limit as f64` — floating-point division for gas ratio. Verify this is display-only, not consensus-critical.

#### torus-rpc/src/torus.rs — torus_* JSON-RPC

- **EVM-CHK-65** (line 326): `limit.unwrap_or(100).min(1000) as usize` — pagination limit capped at 1000. Good.
- **EVM-CHK-66** (line 460): `let epoch_length = 100u64` — hardcoded epoch length in RPC. **(Pre-finding)**

#### torus-core/src/precompiles.rs — CLOB Precompiles

- **EVM-CHK-67** (line 41): Precompile address derivation `Address::new([0,..,b[0],b[1]])` — verify address space doesn't collide with user contracts.
- **EVM-CHK-68** (line 69): `u32::from_be_bytes([input[0..3]])` — function selector parsing. Verify input length validation before array access.
- **EVM-CHK-69** (line 166): `encode_u128(fp.raw() as u128)` — negative FixedPoint values silently truncated to 0 or wrap. **(Pre-finding)**
- **EVM-CHK-70** (line 191,194): Array length encoding `as u32` — verify consistency with decoder.
- **EVM-CHK-71** (line 650): `u16::from_le_bytes([value[52], value[53]])` — verify byte offset matches ABI layout.
- **EVM-CHK-72** (line 701-702): `FixedPoint::from_raw(price as i128)` / `quantity as i128` — u128→i128 cast. Values > i128::MAX become negative. **(Pre-finding)**
- **EVM-CHK-73** (line 710): `order_id = ((current_block + 1) as u128) << 64 | seq as u128` — order ID generation. Verify uniqueness across blocks and no collision with native order IDs.
- **EVM-CHK-74** (line 1067): `match disc[0]` — discriminant byte for action deserialization. Verify all cases are handled and unknown discriminants error cleanly.
- **EVM-CHK-75**: Gas metering — verify precompile gas costs are reasonable (too low = DoS, too high = unusable).

#### torus-core/src/lockbox.rs — EVM↔Native Bridge

- **EVM-CHK-76** (line 33-34): Zero/negative amount check `amount <= FixedPoint::ZERO` — returns Ok(()) silently. Verify this is the intended behavior (not error).
- **EVM-CHK-77** (line 41-45): Insufficient balance check — good.
- **EVM-CHK-78** (line 49): `evm_balance - evm_amount` — unchecked U256 subtraction. Safe because of the check above, but fragile if check is ever removed.
- **EVM-CHK-79** (line 53): `native_bal.available + amount` — FixedPoint addition. Could overflow i128. **(Pre-finding)**
- **EVM-CHK-80** (line 82): `updated_bal.available - amount` — FixedPoint subtraction. Same overflow concern.
- **EVM-CHK-81** (line 99-104): Raw CF access for EVM balance — `data[..32]` extraction. Verify 72-byte format is stable.
- **EVM-CHK-82** (line 113-125): `set_evm_balance` creates new account with KECCAK_EMPTY code hash — verify this doesn't conflict with revm's account creation.
- **EVM-CHK-83** (line 155-161): `fp_to_u256` — negative FixedPoint returns U256::ZERO. Verify callers handle this correctly.
- **EVM-CHK-84** (line 164-173): `u256_to_fp` — returns None for values > i128::MAX. Good bounds checking.
- **EVM-CHK-85**: Non-atomicity — debit then credit in separate DB operations. **(Pre-finding — see ECON-PF-04)**

### 4.4 Fuzz Targets

| Function | Invariant to Check |
|----------|--------------------|
| `calc_next_block_base_fee(gas_used, gas_limit, base_fee)` | No panic; matches Ethereum EIP-1559 |
| `execute_block(state_db, block_cfg, txs)` | Deterministic receipts and bundle for same inputs |
| `compute_post_bundle_state_root(state_db, bundle)` | Same state → same root; different state → different root |
| `compute_native_state_root(state_db)` | Deterministic across calls; consistent with iterator order |
| Precompile dispatch (all function selectors) | No panic on any input; gas charged correctly |
| `fp_to_u256` / `u256_to_fp` | Roundtrip: `u256_to_fp(fp_to_u256(x)) == x` for valid x |
| `Lockbox::deposit_to_native` / `withdraw_from_native` | Conservation: EVM + native total unchanged |
| RLP tx decoding | No panic on malformed input; tx_hash correct |
| `decode_all_txs(raw_bytes)` | Consistent with revm interpretation |

### 4.5 Property Test Invariants

1. **State determinism**: Same (state, block) → same (state_root, receipts, bundle) on every execution
2. **Gas conservation**: `sum(receipt.gas_used) == block.gas_used` for all transactions
3. **Base fee monotonicity**: Base fee increases if gas_used > target, decreases if < target
4. **Lockbox conservation**: For any sequence of deposits/withdrawals, `Σ(EVM balance) + Σ(native balance) = constant`
5. **State root uniqueness**: Two different post-states produce different state roots (collision resistance)
6. **Transaction isolation**: Reverting tx N does not affect the state changes of tx N+1
7. **Precompile safety**: No precompile call can create or destroy tokens (only transfer between EVM and native)
8. **Nonce monotonicity**: Each successful tx increments sender's nonce by exactly 1

### 4.6 Cross-Domain Boundaries

| File | EVM Checks | Delegates to |
|------|-----------|--------------|
| torus-bridge/src/proposer.rs | EVM execution, base fee, gas limit | Consensus (block hash), Economics (native actions) |
| torus-bridge/src/validator.rs | State root, gas verification | Consensus (block validity), Economics (native pipeline) |
| torus-bridge/src/state_root.rs | EVM MPT root computation | Economics (native state root) |
| torus-core/src/lockbox.rs | EVM balance read/write | Economics (native balance) |
| torus-core/src/precompiles.rs | Gas metering, ABI encoding | Economics (order book, staking, oracle) |
| torus-bridge/src/committer.rs | State persistence | Consensus (block finalization) |

---

## 5. Domain C: Economic Model

### 5.1 Threat Model

**Attackers**:
- Malicious trader (self-trade, wash trading, order manipulation, margin gaming)
- Oracle manipulator (submit false prices, delay submissions, front-run aggregation)
- Staking attacker (flash delegation, governance capture, reward siphoning)
- Governance attacker (proposal spam, vote buying, parameter manipulation)
- Liquidation exploiter (front-run liquidations, manipulate trigger conditions)

**Goals**:
- **Token creation/destruction**: exploit arithmetic to create tokens from nothing
- **Unfair trading**: manipulate order matching, bypass time priority
- **Oracle manipulation**: control oracle price to trigger liquidations or avoid them
- **Governance capture**: gain majority voting power cheaply
- **Reward theft**: claim rewards not earned, exploit distribution rounding
- **Margin abuse**: avoid liquidation when undercollateralized, or liquidate solvent positions

**Assumptions that must hold**:
- FixedPoint arithmetic (i128, 8 decimals) is sufficient for all financial values
- Total token supply fits in i128 (max ~1.7×10^29 raw units = ~1.7×10^21 tokens)
- Oracle reports are eventually honest (majority of stake-weighted reporters are honest)
- Epoch rotation is infrequent enough for governance processes to complete
- Staking unbonding period is long enough to slash misbehaving validators

### 5.2 Crates in Scope

| Crate | Files | Lines | Role |
|-------|-------|-------|------|
| torus-core | 16 | 7,471 | Order book, margin, liquidation, oracle, lockbox, precompiles |
| torus-economics | 12 | 6,119 | Staking, governance, fee splits, epoch rotation, rewards |
| torus-bridge/native_executor.rs | 1 | ~950 | Native action dispatch and execution |
| torus-bridge/committer.rs | 1 | ~100 | Fee distribution |
| torus-types (partial) | 2 | ~560 | FixedPoint, NativeAction, order types |

### 5.3 Per-File Checklist

#### torus-types/src/lib.rs — FixedPoint Arithmetic

- **ECON-CHK-01** (line 54-57): `FixedPoint::mul` — uses i256 for intermediate product. Correct overflow protection. But `as_i128()` at the end could still truncate if result > i128::MAX. **(Pre-finding)**
- **ECON-CHK-02** (line 63-68): `FixedPoint::div` — uses i256. Same truncation risk on result. Division by zero panics via i256 division. **(Pre-finding)**
- **ECON-CHK-03** (line 72-74): `FixedPoint::add` — `self.0 + rhs.0` is unchecked i128 addition. Silently wraps on overflow. **(Pre-finding)**
- **ECON-CHK-04** (line 78-80): `FixedPoint::sub` — same unchecked subtraction. **(Pre-finding)**
- **ECON-CHK-05** (line 36): `SCALE: i128 = 100_000_000` — verify 8 decimal places is sufficient for all markets (e.g., BTC at $100k = 10^13 raw, well within i128).
- **ECON-CHK-06** (line 106-107): `Display` — `self.0 / Self::SCALE` and `self.0 % Self::SCALE` — verify negative number display is correct.
- **ECON-CHK-07**: No `checked_add`/`checked_sub`/`checked_mul` equivalents provided — all financial arithmetic uses wrapping operations.

#### torus-core/src/order_book.rs — Order Book Engine

- **ECON-CHK-08**: Price-time priority — verify bids sorted descending by price, then ascending by time; asks sorted ascending by price, then ascending by time.
- **ECON-CHK-09**: Self-trade prevention — verify a trader cannot match against their own orders.
- **ECON-CHK-10**: Partial fill accounting — verify `remaining_quantity` is correctly updated after each fill.
- **ECON-CHK-11**: Post-only rejection — verify PostOnly orders that would immediately match are rejected (not executed).
- **ECON-CHK-12**: FOK semantics — verify Fill-or-Kill orders that cannot be completely filled are fully rejected (no partial execution).
- **ECON-CHK-13**: IOC semantics — verify Immediate-or-Cancel orders cancel unfilled remainder immediately.
- **ECON-CHK-14**: Order ID uniqueness — verify `place_order` assigns unique IDs that don't collide.
- **ECON-CHK-15**: Quantity validation — verify zero or negative quantities are rejected.
- **ECON-CHK-16**: Price validation — verify zero or negative prices are rejected.
- **ECON-CHK-17**: Tick size enforcement — verify prices are rounded to tick size.
- **ECON-CHK-18**: Lot size enforcement — verify quantities are rounded to lot size.

#### torus-core/src/margin.rs — Margin System

- **ECON-CHK-19** (line 114): `FixedPoint::from_raw(leverage as i128 * FixedPoint::SCALE)` — u32→i128 cast is safe.
- **ECON-CHK-20** (line 217): `FixedPoint::from_raw(max_lev as i128 * FixedPoint::SCALE)` — same.
- **ECON-CHK-21** (line 220): `FixedPoint::from_raw(config.maintenance_factor_bps as i128)` — BPS to FixedPoint conversion. Verify this is scaled correctly (BPS is 1/10000, FixedPoint SCALE is 10^8).
- **ECON-CHK-22**: Initial margin calculation — verify `initial_margin = notional_value / leverage`.
- **ECON-CHK-23**: Maintenance margin — verify `maintenance_margin = notional_value * maintenance_margin_bps / 10000`.
- **ECON-CHK-24**: Margin requirement check — verify positions cannot be opened with insufficient margin.
- **ECON-CHK-25** (line 272): `FixedPoint::from_raw(v as i128 * FixedPoint::SCALE)` — test helper, but verify production code doesn't use this pattern with potential overflow.

#### torus-core/src/liquidation.rs — Liquidation Engine

- **ECON-CHK-26**: Liquidation trigger — verify the condition is `margin_ratio <= maintenance_margin_ratio`, not `<` (boundary correctness).
- **ECON-CHK-27**: Liquidation penalty — verify penalty is applied correctly and goes to insurance fund.
- **ECON-CHK-28** (line 167-169): Same FixedPoint casts as margin.rs — verify consistency.
- **ECON-CHK-29**: Auto-deleverage — verify ADL only triggers when insurance fund is insufficient, and respects trader position priority.
- **ECON-CHK-30**: Bankruptcy handling — verify positions with negative equity are handled (socialized loss or insurance fund).

#### torus-core/src/oracle.rs — Oracle Price Aggregation

- **ECON-CHK-31** (line 239): `num_reporters: filtered.len() as u32` — truncation if > 4B reporters.
- **ECON-CHK-32** (line 265): `num_reporters: stored.num_reporters as usize` — u32→usize cast, safe on 64-bit.
- **ECON-CHK-33** (line 348): `FixedPoint::from_raw(prices.len() as i128 * FixedPoint::SCALE)` — safe for reasonable market counts.
- **ECON-CHK-34**: Median calculation — verify the aggregation correctly computes stake-weighted median (not mean).
- **ECON-CHK-35**: Staleness check — verify stale prices are rejected (timestamp-based).
- **ECON-CHK-36**: Outlier filtering — verify extreme price submissions are filtered to prevent manipulation.
- **ECON-CHK-37** (line 394): `return pairs[0].0` — direct index access. Verify `pairs` is never empty when this line is reached.
- **ECON-CHK-38**: Oracle reporter eligibility — verify only active validators can submit prices.

#### torus-core/src/position.rs — Position Management

- **ECON-CHK-39** (line 89): `self.margin_type as u8` — verify enum discriminant is stable across versions.
- **ECON-CHK-40** (line 102, 109): `lb[0]` and `mt[0]` — single-byte discriminant deserialization. Verify no index panic.
- **ECON-CHK-41**: Position accumulation — verify averaging entry price correctly on position increase.
- **ECON-CHK-42**: PnL calculation — verify `pnl = (exit_price - entry_price) * quantity * direction_sign`.
- **ECON-CHK-43**: Position close — verify closing a position releases all margin.

#### torus-core/src/lockbox.rs — Balance Bridge

- **ECON-CHK-44** (line 28-57): `deposit_to_native` — non-atomic debit-then-credit. **(Pre-finding)**
- **ECON-CHK-45** (line 62-91): `withdraw_from_native` — non-atomic credit-then-debit. Same risk.
- **ECON-CHK-46** (line 155-161): `fp_to_u256` — negative FixedPoint silently maps to U256::ZERO. Verify no value is lost.
- **ECON-CHK-47** (line 164-173): `u256_to_fp` — returns None for large values. Verify callers handle None.

#### torus-economics/src/staking.rs — Staking System

- **ECON-CHK-48**: Delegation — verify delegated funds are deducted from sender's balance and credited to validator's total stake.
- **ECON-CHK-49**: Undelegation — verify unbonding period is enforced (not immediate).
- **ECON-CHK-50**: Permanent staking — verify permanent locks are truly irreversible.
- **ECON-CHK-51**: Self-delegation minimum — verify validators must maintain minimum self-stake.
- **ECON-CHK-52**: Slashing — verify slashed amounts are correctly deducted from all delegators proportionally.
- **ECON-CHK-53**: Reward distribution — verify rewards are distributed proportionally to stake weight, after commission.
- **ECON-CHK-54**: Key rotation — verify pending rotations are applied at epoch boundaries, not immediately.

#### torus-economics/src/governance.rs — Governance

- **ECON-CHK-55**: Voting power — verify voting power = permanent stake weight (per spec).
- **ECON-CHK-56**: Proposal threshold — verify minimum stake required to submit proposals.
- **ECON-CHK-57**: Quorum check — verify proposals need sufficient total votes to pass.
- **ECON-CHK-58**: Execution delay — verify passed proposals have a time lock before execution.
- **ECON-CHK-59** (line 96, 104): Borsh discriminant serialization — verify round-trip correctness.
- **ECON-CHK-60** (line 246): `match disc[0]` — discriminant deserialization. Verify all variants handled.
- **ECON-CHK-61** (line 298): `proposal_id as u64 BE` — key format. Verify ordering is correct for iteration.
- **ECON-CHK-62** (line 351): `opt_flag[0] == 0` — optional field deserialization. Verify correctness.

#### torus-economics/src/rewards.rs — Reward Distribution

- **ECON-CHK-63** (line 168-171): `delta = (end - start) as u64` / `(start - end) as u64` — i128→u64 cast. Could truncate for large values.
- **ECON-CHK-64**: APY calculation — verify 5% annual rate is correctly converted to per-block rate.
- **ECON-CHK-65**: Reward minting — verify rewards don't exceed the designed inflation schedule.
- **ECON-CHK-66**: Commission deduction — verify `validator_reward = total * commission_bps / 10000`, `delegator_share = total - validator_reward`.

#### torus-economics/src/epoch.rs — Epoch Management

- **ECON-CHK-67** (line 18-22): `is_epoch_boundary` — returns false for epoch_length=0 and block_height=0. Correct guard.
- **ECON-CHK-68** (line 26-30): `epoch_for_block` — returns 0 for epoch_length=0. Correct guard.
- **ECON-CHK-69** (line 55): `eligible.truncate(max_validators as usize)` — drops validators beyond max. Verify ordering ensures highest-stake validators are kept.

#### torus-economics/src/dev_pool.rs — Developer Pool

- **ECON-CHK-70**: Pro-rata distribution — verify `developer_share = total * developer_weight / total_weight`.
- **ECON-CHK-71**: Division by zero — verify total_weight=0 is handled.

#### torus-economics/src/types.rs — Economic Types

- **ECON-CHK-72** (line 155): `self.unbonding.len() as u32` — truncation in Borsh serialization.
- **ECON-CHK-73** (line 168): `u32::deserialize_reader(reader)? as usize` — u32→usize cast. Safe on 64-bit.
- **ECON-CHK-74** (line 389, 401): Same pattern for contract list serialization.

#### torus-bridge/src/native_executor.rs — Native Execution Pipeline

- **ECON-CHK-75** (line 260-261): `OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE)` — default tick/lot size = 1.0. **(Pre-finding)**
- **ECON-CHK-76** (line 267-268): `taker_is_buy = fill.maker_side != Side::Buy` — verify taker side derivation is correct.
- **ECON-CHK-77** (line 268-283): Fill application — both taker and maker fills applied. Verify double-counting doesn't occur.
- **ECON-CHK-78** (line 472): `ctx.block_height / ctx.epoch_length.max(1)` — `.max(1)` prevents division by zero but gives wrong epoch for epoch_length=0. **(Pre-finding)**
- **ECON-CHK-79** (line 532): `maintenance_margin_bps as i128 * FixedPoint::SCALE / 10000` — verify BPS→FixedPoint conversion is correct (should be `bps * SCALE / 10000`).
- **ECON-CHK-80** (line 563): `let support = matches!(option, VoteOption::Yes)` — Abstain maps to false (same as No). **(Pre-finding)**
- **ECON-CHK-81** (line 617-648): CoreWriter delay guard — `qa.block_queued >= ctx.block_height` check. Correct.
- **ECON-CHK-82** (line 700-705): `oracle_price = ...unwrap_or(FixedPoint::ZERO)` — using zero oracle price for liquidation if price not found. **(Pre-finding)**
- **ECON-CHK-83** (line 731-732): `total_fees = U256::from(ctx.total_native_fees + total_evm_fees)` — u64 addition could overflow.
- **ECON-CHK-84** (line 872): `format!("{:?}", action)` for hash — Debug format instability risk. **(Pre-finding)**
- **ECON-CHK-85** (line 906): `is_buy: *side == 0` — CoreWriter side encoding. Verify 0=buy convention is consistent with NativeAction.
- **ECON-CHK-86** (line 936-950): `decode_order_type` / `decode_time_in_force` — unknown codes default to Limit/GTC. Verify this is safe (shouldn't silently change order semantics).

#### torus-economics/src/queries.rs — Query Helpers

- **ECON-CHK-87** (line 33-36): Epoch info calculation — verify epoch boundaries are correctly computed.

### 5.4 Fuzz Targets

| Function | Invariant to Check |
|----------|--------------------|
| `FixedPoint::mul(a, b)` | No panic; result fits i128; `a * b / SCALE` is correct |
| `FixedPoint::div(a, b)` | Panic only if b=0; result fits i128; `a * SCALE / b` is correct |
| `FixedPoint::add(a, b)` / `sub(a, b)` | No silent overflow for realistic values |
| `OrderBook::place_order(params, sender, ts)` | Price-time priority preserved; conservation of quantity |
| `OrderBook::cancel_order(id)` | Successfully cancels existing; errors on non-existent |
| `OracleManager::aggregate_price(market, height, stakes)` | Deterministic; stake-weighted median is correct |
| `LiquidationEngine::check_liquidations(...)` | Only triggers for undercollateralized positions |
| `StakingManager::delegate/undelegate(...)` | Balance conservation; minimum stake enforced |
| `FeeSplitter::split_fees(total, epoch)` | Sum of parts = total (no rounding loss) |
| `RewardDistributor::distribute_block_fees(...)` | Total distributed = total input (conservation) |
| `GovernanceManager::cast_vote(...)` | Vote recorded correctly; cannot double-vote |
| `sort_native_actions(actions)` | Deterministic; correct pre/post-EVM classification |

### 5.5 Property Test Invariants

1. **Token conservation**: For any sequence of operations, `Σ(all balances) + Σ(locked) + Σ(staked) + Σ(fees_burned) = total_supply + Σ(rewards_minted)`
2. **Order book conservation**: For any trade, `taker_fill_qty == maker_fill_qty` and `taker_debit == maker_credit`
3. **Price-time priority**: No fill occurs at a price worse than the best available price at that time
4. **FixedPoint commutativity**: `a + b == b + a`, `a * b == b * a`
5. **FixedPoint identity**: `a * ONE == a`, `a / ONE == a`, `a + ZERO == a`
6. **Staking conservation**: `Σ(delegator_stakes) == Σ(validator_total_stakes)` at all times
7. **Fee split conservation**: `burn + validator + treasury + dev_pool == total_fees`
8. **Margin safety**: No position can have `margin_ratio < maintenance_margin_ratio` after a non-liquidation operation
9. **Governance fairness**: A proposal passes iff `yes_votes > no_votes` and `total_votes >= quorum`
10. **Epoch monotonicity**: Epoch number is non-decreasing with block height

### 5.6 Cross-Domain Boundaries

| File | Economics Checks | Delegates to |
|------|-----------------|--------------|
| torus-core/src/lockbox.rs | Native balance conservation | EVM (balance read/write via raw CF) |
| torus-core/src/precompiles.rs | Order/staking parameter validation | EVM (gas metering, ABI encoding) |
| torus-bridge/src/native_executor.rs | All native action execution | Consensus (action ordering), EVM (CoreWriter from previous block) |
| torus-bridge/src/state_root.rs | Native state root (CF iteration) | EVM (composite root composition) |
| torus-economics/src/epoch.rs | Validator set computation | Consensus (epoch transition in HotStuff) |
| torus-economics/src/staking.rs | Stake management | Consensus (validator power) |

---

## 6. Cross-Domain Overlap Map

### 6.1 Shared File Responsibilities

| File | Domain A (Consensus) | Domain B (EVM) | Domain C (Economics) | Blind Spot Risk |
|------|---------------------|----------------|---------------------|-----------------|
| **torus-bridge/src/proposer.rs** | Block construction, datum serialization | EVM execution, base fee, state root | Native action sorting, pre/post-EVM classification | **HIGH**: Execution ordering mismatch between proposer and validator could cause consensus split |
| **torus-bridge/src/validator.rs** | State root verification trigger | EVM re-execution, gas match | Native pipeline execution, epoch check | **CRITICAL**: `validate_block` vs `validate_block_with_native` — which is called in production? |
| **torus-bridge/src/state_root.rs** | Composite root in block header | EVM MPT root computation | Native keccak root computation | **HIGH**: Native root computation is non-standard; any ordering change = fork |
| **torus-bridge/src/native_executor.rs** | Action ordering (anti-MEV) | CoreWriter drain (from EVM) | All native action dispatch | **HIGH**: `Debug` format hash for ordering could break across Rust versions |
| **torus-bridge/src/committer.rs** | Block finalization trigger | EVM state persistence | Fee distribution | **MEDIUM**: Write ordering could affect crash recovery |
| **torus-core/src/lockbox.rs** | — | EVM balance raw CF access | Native balance management | **HIGH**: Lockbox bypasses StateDb API; inconsistent with revm bundle |
| **torus-core/src/precompiles.rs** | — | Gas metering, ABI encoding | Order/staking logic | **HIGH**: Precompile ↔ native action semantic equivalence |
| **torus-types/src/lib.rs** | ValidatorSet, PublicKey | TorusBlock, Receipt | FixedPoint, NativeAction | **HIGH**: FixedPoint arithmetic bugs affect both EVM (via precompiles) and Economics |
| **torus-economics/src/epoch.rs** | Validator set transitions | — | Stake ranking, rotation cap | **MEDIUM**: Epoch length mismatch between consensus and economics layers |
| **torus-economics/src/staking.rs** | Validator power | — | Stake management, slashing | **MEDIUM**: Staking state consistency between epochs |
| **torus-consensus/src/app.rs** | HotStuff App trait impl | Block validation call | Epoch boundary processing | **CRITICAL**: Bridge between all three domains |

### 6.2 Critical Blind Spots

| # | Description | Domains | Risk |
|---|-------------|---------|------|
| 1 | **Validation path mismatch**: `app.rs` calls `validate_block` (EVM-only), not `validate_block_with_native`. Native actions in blocks are never validated during consensus. | A, B, C | **CRITICAL** |
| 2 | **State root timing**: Proposer computes native state root BEFORE native execution writes for the current block are committed. Validator must compute at the same point or roots diverge. | A, B | **HIGH** |
| 3 | **Lockbox bypasses revm**: Lockbox reads/writes EVM balances via raw RocksDB CF access, while the EVM executor uses revm's bundled state. If both modify the same account in one block, changes may conflict. | B, C | **HIGH** |
| 4 | **epoch_length = 0 in bridge**: Both proposer and validator hardcode `epoch_length: 0`. Epoch boundaries never trigger in the bridge path, but DO trigger in `app.rs` (which uses `config.epoch_length`). | A, C | **HIGH** |
| 5 | **Debug format determinism**: Native action sorting uses `format!("{:?}", action)` to produce a hash for ordering. If Debug output changes (Rust version, struct field reorder), nodes on different compiler versions would disagree on ordering → consensus divergence. | A, C | **MEDIUM** |
| 6 | **FixedPoint overflow in lockbox**: Lockbox uses FixedPoint add/sub without overflow protection. An attacker depositing amounts that cause i128 overflow could corrupt native balances. | B, C | **HIGH** |
| 7 | **Oracle price zero fallback**: Liquidation checks use `unwrap_or(FixedPoint::ZERO)` for missing oracle prices. Liquidation at price=0 would close positions at worst possible price. | C (but could cause chain halt via panics) | **MEDIUM** |

---

## 7. Pre-Findings Summary

| ID | Severity | Domain | File:Line | Summary |
|----|----------|--------|-----------|---------|
| EVM-PF-08 | **Critical** | B | torus-state/src/db.rs:153-162 | `get_block_hash` reads first 32 bytes of JSON-serialized header as block hash. EVM `BLOCKHASH` opcode returns garbage — breaks all contracts using `blockhash()`. |
| CONS-PF-07 | **Critical** | A | hotstuff_rs/src/hotstuff/types.rs:422-441 | NEC validation does NOT verify that NEC signers didn't vote for high_tip. Byzantine validators who DID vote can also sign the NEC, breaking the NEC safety property and enabling abandonment of a validly-QC'd block. |
| CONS-PF-01 | **Critical** | A | torus-consensus/src/app.rs:391 | `produce_empty_block` sets `state_root: B256::ZERO` — empty blocks have zero state root instead of actual state root. |
| CONS-PF-02 | **Critical** | A, B, C | torus-consensus/src/app.rs:231-234 | `do_validate` calls `validate_block` (EVM-only) even when blocks contain native actions. Native actions never validated during consensus. |
| CONS-PF-08 | **Critical** | A | torus-consensus/src/app.rs:319-373 | `on_speculative_rollback` applies slashing (5% + tombstone) to RocksDB immediately. If equivocation evidence is based on a speculative block that gets rolled back, validator is irreversibly slashed incorrectly. |
| EVM-PF-09 | **Critical** | B, C | torus-bridge/src/validator.rs:191-201 | Oracle aggregation and liquidation checks never called during `validate_block_with_native`. Oracle prices cannot be resolved and positions cannot be liquidated. |
| ECON-PF-10 | **High** | C | torus-core/src/margin.rs:220-224 | Maintenance margin BPS formula computes `initial * (bps_raw / 10^8) / (10000 * 10^8)` ≈ 5e-9 instead of 50%. Every position either immediately liquidatable or maintenance margin effectively zero. Same bug in 3 locations. |
| CONS-PF-09 | **High** | A | hotstuff_rs/src/pacemaker/implementation.rs:253-281 | Bracha timeout amplification uses message COUNT not VOTING POWER. Low-power validators have equal influence on timeout amplification as high-power ones. |
| CONS-PF-10 | **High** | A | hotstuff_rs/src/hotstuff/roles.rs:256-274 | `new_view_recipients` uses non-reputation leader, but proposal uses reputation-weighted leader. NewView messages go to wrong leader when reputation is active → view-change liveness failure. |
| CONS-PF-11 | **High** | A | hotstuff_rs/src/hotstuff/implementation.rs:1004-1055 | Block from `ProposalResponse` not validated through `safe_block` before reproposal. Byzantine responder can send structurally invalid block that passes hash check. |
| CONS-PF-12 | **High** | A | torus-network/src/swarm.rs:320-351 | Gossip sender identity from payload bytes, not authenticated libp2p `message.source`. Any node can spoof origin VerifyingKey of consensus messages. |
| CONS-PF-13 | **High** | A, B | torus-bridge/src/validator.rs:43-89 | `receipts_root` and `logs_bloom` in block header never verified. Byzantine proposer can falsify receipt data. |
| CONS-PF-14 | **High** | A, B | torus-bridge/src/validator.rs:52-58 | Validator uses `base_fee_per_gas` from proposed header without EIP-1559 recalculation. Byzantine proposer can set arbitrary base fee. |
| ECON-PF-11 | **High** | C | torus-core/src/margin.rs:130-143 | Cross-margin `free_margin = equity - maint` underflows when `equity < maint`. In release builds wraps to large positive → deeply insolvent accounts pass margin checks. |
| EVM-PF-10 | **High** | B | torus-core/src/precompiles.rs:220-247 | Write precompiles (CoreWriter, Lockbox) have no gas cost. Single EVM tx can call `placeOrder` thousands of times for CALL opcode base cost only — DoS via queue flooding. |
| EVM-PF-11 | **High** | B | torus-state/src/snapshot.rs:133-136 | Snapshot verification computes EVM-only root but compares against composite root from header. Verification always fails. |
| ECON-PF-01 | **High** | C | torus-types/src/lib.rs:73-80 | `FixedPoint::add`/`sub` unchecked i128 arithmetic. Silent overflow wrapping corrupts financial balances. |
| ECON-PF-02 | **High** | C | torus-types/src/lib.rs:67 | `FixedPoint::div` panics on division by zero. Code paths dividing by user-controlled FixedPoint halt node. |
| EVM-PF-01 | **High** | B | torus-evm/src/eip1559.rs:16,24-26 | `calc_next_block_base_fee` divides by `gas_target = gas_limit / 2`. If gas_limit < 2, `gas_target = 0` → division by zero panic. |
| ECON-PF-04 | **High** | B, C | torus-core/src/lockbox.rs:28-91 | Lockbox deposit/withdraw non-atomic: debit succeeds then credit fails → funds destroyed. |
| ECON-PF-12 | **High** | C | torus-economics/src/governance.rs:741-747 | `ParameterChange` writes arbitrary `param_key` to DB — no whitelist. Governance majority can overwrite governance params (set quorum to 0.01%). |
| EVM-PF-13 | **High** | B | torus-evm/src/executor.rs:162-175 | Block gas limit checked AFTER `transact_commit`. Over-limit tx already merged into bundle state. |
| EVM-PF-12 | **High** | B | torus-bridge/src/state_root.rs:48-81 | Native state root concatenates KV pairs without length framing — collision potential. |
| CONS-PF-03 | **Medium** | A, C | torus-bridge/src/native_executor.rs:872 | `action_sort_key` uses `format!("{:?}", action)` — Debug format not stable across Rust versions → consensus divergence. |
| ECON-PF-14 | **Medium** | C | torus-economics/src/epoch.rs:164-198 | `apply_rotation_cap` uses `HashSet::difference` with non-deterministic iteration → different nodes may select different validators at epoch boundaries. |
| ECON-PF-03 | **Medium** | C | torus-bridge/src/native_executor.rs:563 | `VoteOption::Abstain` maps to `support: false` (same as No). |
| ECON-PF-15 | **Medium** | C | torus-core/src/order_book.rs:314-348 | `cancel_order` has no ownership check. Any user can cancel any other user's resting orders. |
| ECON-PF-16 | **Medium** | C | torus-economics/src/rewards.rs:145-149 | `BLOCKS_PER_YEAR = 31_536_000` assumes 1 block/sec but chain targets 6s blocks. Actual permanent staking APY would be ~30% instead of 5%. |
| ECON-PF-06 | **Medium** | C | torus-bridge/src/native_executor.rs:700-705 | Liquidation uses `unwrap_or(FixedPoint::ZERO)` for missing oracle prices → forces full loss on longs. |
| EVM-PF-14 | **Medium** | B | torus-rpc/src/eth.rs:744-761 | `eth_call` ignores block tag; always executes against latest state. |
| EVM-PF-02 | **Medium** | B | torus-evm/src/executor.rs:292 | `calc_effective_gas_price` truncates u128 → u64 silently. |
| EVM-PF-03 | **Medium** | B | torus-bridge/src/state_root.rs:93 | State root loads ALL accounts via `all_accounts()` — OOM risk. |
| CONS-PF-04 | **Medium** | A, C | torus-bridge/src/proposer.rs:167 & validator.rs:149 | `epoch_length: 0` hardcoded in NativeExecContext. |
| CONS-PF-05 | **Medium** | A | torus-consensus/src/app.rs:288 | `last_header` updated before consensus confirmation. |
| ECON-PF-05 | **Medium** | C | torus-bridge/src/native_executor.rs:260-261 | Markets created with default tick/lot instead of loading params. |
| EVM-PF-05 | **Medium** | B, C | torus-core/src/lockbox.rs:108-125 | Lockbox bypasses StateDb API for EVM balance writes. |
| ECON-PF-17 | **Medium** | C | torus-bridge/src/native_executor.rs:200-204 | `Withdraw { to }` discards `to` address — withdrawals always go to sender. |
| ECON-PF-18 | **Medium** | C | torus-core/src/oracle.rs:359-386 | Outlier rejection bypassed for single-validator oracle. |
| CONS-PF-15 | **Medium** | A | hotstuff_rs/src/hotstuff/implementation.rs:86-88 | `seen_proposals` HashMap grows unbounded — memory DoS. |
| CONS-PF-16 | **Medium** | A | hotstuff_rs/src/pacemaker/implementation.rs:562-564 | `bracha_timeout_counts` BTreeMap grows unbounded — memory DoS. |
| EVM-PF-15 | **Medium** | B | torus-rpc/src/eth.rs:800-853 | `eth_getLogs` no block range limit — DoS. |
| EVM-PF-16 | **Medium** | B | torus-core/src/precompiles.rs:916-943 | CoreWriter `next_sequence` O(n) scan → O(n²) total enqueue — DoS. |
| ECON-PF-07 | **Low** | C | torus-bridge/src/native_executor.rs:219-223 | Admin actions are no-op stubs. |
| CONS-PF-06 | **Low** | A | torus-rpc/src/torus.rs:460 | `torus_getEpoch` hardcodes epoch_length=100. |
| EVM-PF-06 | **Low** | B | torus-core/src/precompiles.rs:701-702 | u128→i128 cast makes large prices negative. |
| ECON-PF-08 | **Low** | C | torus-core/src/precompiles.rs:166 | Negative FixedPoint silently truncated for EVM. |
| EVM-PF-17 | **Low** | B | torus-rpc/src/eth.rs:370-371 | `transactions_root` always B256::ZERO. |
| EVM-PF-18 | **Low** | B | torus-rpc/src/eth.rs:196-199 | `eth_call` nonce defaults to 0. |
| EVM-PF-07 | **Info** | B | torus-bridge/src/state_root.rs:68 | RocksDB iterator errors silently ignored. |
| ECON-PF-09 | **Info** | C | torus-economics/src/types.rs:155 | Borsh u32 truncation for unbonding list. |

### 7.1 Severity Distribution

| Severity | Count |
|----------|-------|
| Critical | 6 |
| High | 18 |
| Medium | 18 |
| Low | 6 |
| Informational | 2 |
| **Total** | **50** |

### 7.2 Severity Classification Key

- **Critical**: Lose funds, permanent chain halt, consensus fork (different state roots on honest nodes), bypass slashing
- **High**: Temporary halt (node panic), state divergence after commit, drain economic modules, bypass validation
- **Medium**: Performance degradation, MEV extraction, economic miscalibration, non-standard behavior
- **Low**: Code quality, fragile assumptions, maintenance risk, stub implementations
- **Informational**: Style, dead code, documentation, theoretical edge cases

---

## Appendix A: Crate Dependency Graph (Critical Path)

```
torus-node
  ├── torus-consensus (App trait) ──► hotstuff_rs (BFT engine)
  │     ├── torus-bridge (proposer, validator, committer)
  │     │     ├── torus-evm (revm executor)
  │     │     ├── torus-core (order book, margin, lockbox, precompiles)
  │     │     └── torus-economics (staking, governance, rewards, epoch)
  │     └── torus-state (RocksDB, trie)
  ├── torus-rpc (JSON-RPC server)
  ├── torus-mempool (tx/action pools)
  ├── torus-network (libp2p)
  └── torus-genesis (chain init)

torus-types (leaf crate — no internal deps, used by all)
```

## Appendix B: File→Domain Assignment

Every `.rs` file in scope assigned to exactly one primary domain, with secondary domains noted.

### Consensus (A) Primary
- `crates/hotstuff_rs/src/**/*.rs` (54 files)
- `crates/torus-consensus/src/**/*.rs` (7 files)
- `crates/torus-network/src/**/*.rs` (11 files)

### EVM (B) Primary
- `crates/torus-evm/src/**/*.rs` (6 files)
- `crates/torus-state/src/**/*.rs` (9 files)
- `crates/torus-mempool/src/**/*.rs` (6 files)
- `crates/torus-rpc/src/**/*.rs` (8 files)

### Economics (C) Primary
- `crates/torus-core/src/**/*.rs` (16 files)
- `crates/torus-economics/src/**/*.rs` (12 files)

### Cross-Domain (assigned to multiple)
- `crates/torus-bridge/src/**/*.rs` → A + B + C
- `crates/torus-types/src/**/*.rs` → A + B + C
