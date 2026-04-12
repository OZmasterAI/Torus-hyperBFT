# Torus-hyperBFT: Implementation Plan

**Date:** 2026-04-12
**Status:** Draft v1.0
**Companion Doc:** [technical-requirements.md](./technical-requirements.md)
**Decision Basis:** Custom Rust stack (Path C) — hotstuff_rs + revm + alloy + reth-trie + libp2p + jsonrpsee + RocksDB

---

## Table of Contents

1. [Phase Overview](#1-phase-overview)
2. [Phase 1: EVM Devnet](#2-phase-1-evm-devnet-months-1-5)
3. [Phase 2: Native Order Book](#3-phase-2-native-order-book-months-6-12)
4. [Phase 3: Production Hardening](#4-phase-3-production-hardening-months-12-18)
5. [Critical Path](#5-critical-path)
6. [Risk Register](#6-risk-register)
7. [Testing Strategy](#7-testing-strategy)
8. [Milestone Definitions](#8-milestone-definitions)

---

## 1. Phase Overview

```
Month:  1    2    3    4    5    6    7    8    9   10   11   12   13-18
        ├────┼────┼────┼────┼────┼────┼────┼────┼────┼────┼────┼────┼────►
Phase 1 ███████████████████████████████
        EVM Devnet                     │
Phase 2                                ██████████████████████████████
        Native Order Book              │                             │
Phase 3                                                              ██████
        Production Hardening                                         │
        ─────────────────────────────────────────────────────────────────────
        M1    M2         M3       M4        M5        M6         M7    M8
```

| Phase | Duration | Goal | Team |
|---|---|---|---|
| **Phase 1** | Months 1-5 | 4-validator devnet with EVM + basic staking | 3-4 Rust engineers |
| **Phase 2** | Months 6-12 | Native CLOB + dual-VM + economics modules | 3-4 Rust engineers |
| **Phase 3** | Months 12-18 | Security audit + testnet + mainnet prep | 3-5 engineers + auditors |

---

## 2. Phase 1: EVM Devnet (Months 1-5)

**Goal:** Prove the consensus-execution bridge works. 4 validators producing EVM blocks with MetaMask/Foundry compatibility.

### Task List

| ID | Task | Dependencies | Effort | Week |
|---|---|---|---|---|
| **1.1** | **Workspace scaffold** | None | 1 week | W1 |
| 1.1.1 | Create Cargo workspace with all crate stubs | — | 2 days | W1 |
| 1.1.2 | Define `torus-types`: Block, BlockHeader, StateDiff, ChainConfig | — | 2 days | W1 |
| 1.1.3 | CI setup: cargo check, clippy, test, fmt | 1.1.1 | 1 day | W1 |
| **1.2** | **State layer (torus-state)** | 1.1 | 3 weeks | W2-W4 |
| 1.2.1 | RocksDB wrapper with column family layout | 1.1.2 | 3 days | W2 |
| 1.2.2 | Implement revm `Database` trait over RocksDB | 1.2.1 | 4 days | W2-W3 |
| 1.2.3 | State snapshot and overlay (copy-on-write for validation) | 1.2.2 | 3 days | W3 |
| 1.2.4 | Integrate reth-trie for MPT state root computation | 1.2.2 | 5 days | W3-W4 |
| 1.2.5 | State root tests against Ethereum test vectors | 1.2.4 | 3 days | W4 |
| **1.3** | **EVM execution (torus-evm)** | 1.2 | 3 weeks | W4-W6 |
| 1.3.1 | revm executor wrapper: configure Cancun spec, chain ID | 1.2.2 | 2 days | W4 |
| 1.3.2 | Single-transaction execution with state diff collection | 1.3.1 | 3 days | W5 |
| 1.3.3 | Block-level execution: gas accounting, receipt generation | 1.3.2 | 3 days | W5 |
| 1.3.4 | EIP-1559 base fee calculation (update per block) | 1.3.3 | 2 days | W5 |
| 1.3.5 | Log and bloom filter generation | 1.3.3 | 2 days | W6 |
| 1.3.6 | Ethereum General State Tests (GST) suite pass | 1.3.3 | 4 days | W6 |
| **1.4** | **Consensus integration (torus-consensus)** | 1.1 | 3 weeks | W3-W5 |
| 1.4.1 | Implement hotstuff_rs `KVStore` trait over RocksDB | 1.2.1 | 2 days | W3 |
| 1.4.2 | Implement hotstuff_rs `App` trait (stub: echo blocks) | 1.4.1 | 3 days | W3-W4 |
| 1.4.3 | Implement hotstuff_rs `Network` trait (in-process channels for testing) | 1.4.2 | 2 days | W4 |
| 1.4.4 | 4-node in-process consensus test (blocks producing) | 1.4.3 | 3 days | W4-W5 |
| 1.4.5 | Validator set configuration from genesis | 1.4.2 | 2 days | W5 |
| **1.5** | **Consensus-execution bridge (torus-bridge)** | 1.3, 1.4 | 4 weeks | W6-W9 |
| 1.5.1 | Block proposal construction: pull from mempool, construct TorusBlock | 1.3.3, 1.4.2 | 3 days | W6 |
| 1.5.2 | Block validation pipeline: execute EVM txs, verify state root | 1.5.1, 1.2.4 | 5 days | W7 |
| 1.5.3 | Block commit pipeline: persist to RocksDB, update caches | 1.5.2 | 3 days | W7-W8 |
| 1.5.4 | Wire App trait to bridge: produce_block → build_block, validate_block → validate | 1.5.3 | 3 days | W8 |
| 1.5.5 | State root determinism tests (all validators agree on root) | 1.5.4 | 4 days | W8-W9 |
| 1.5.6 | Block sync: validate_block_for_sync implementation | 1.5.4 | 2 days | W9 |
| **1.6** | **Mempool (torus-mempool)** | 1.1 | 2 weeks | W5-W6 |
| 1.6.1 | EVM transaction pool: nonce tracking, gas price ordering | 1.1.2 | 3 days | W5 |
| 1.6.2 | Transaction validation: signature, nonce, balance check | 1.6.1 | 2 days | W5-W6 |
| 1.6.3 | Mempool eviction: size limits, replacement by gas price | 1.6.2 | 2 days | W6 |
| 1.6.4 | Drain interface for block proposer | 1.6.3 | 1 day | W6 |
| **1.7** | **Network layer (torus-network)** | 1.4 | 3 weeks | W7-W9 |
| 1.7.1 | libp2p swarm setup: QUIC transport, identity management | — | 2 days | W7 |
| 1.7.2 | GossipSub: consensus message topic | 1.7.1 | 3 days | W7-W8 |
| 1.7.3 | GossipSub: transaction gossip topic | 1.7.2 | 2 days | W8 |
| 1.7.4 | Bridge libp2p to hotstuff_rs Network trait | 1.7.2, 1.4.3 | 3 days | W8-W9 |
| 1.7.5 | Request-response: block sync protocol | 1.7.1 | 3 days | W9 |
| 1.7.6 | Kademlia DHT: peer discovery | 1.7.1 | 2 days | W9 |
| **1.8** | **RPC layer (torus-rpc)** | 1.3, 1.6 | 3 weeks | W9-W11 |
| 1.8.1 | jsonrpsee server setup with module registration | — | 2 days | W9 |
| 1.8.2 | Core eth_* methods: chainId, blockNumber, getBalance, getCode, getStorageAt | 1.8.1 | 3 days | W9-W10 |
| 1.8.3 | Transaction methods: sendRawTransaction, getTransactionByHash, getTransactionReceipt | 1.8.2 | 3 days | W10 |
| 1.8.4 | Block methods: getBlockByNumber, getBlockByHash | 1.8.2 | 2 days | W10 |
| 1.8.5 | Execution methods: eth_call, eth_estimateGas | 1.8.2 | 3 days | W10-W11 |
| 1.8.6 | Log methods: eth_getLogs, eth_feeHistory | 1.8.2 | 2 days | W11 |
| 1.8.7 | WebSocket subscriptions: newHeads, logs | 1.8.2 | 3 days | W11 |
| **1.9** | **Genesis and node binary** | 1.5, 1.7, 1.8 | 2 weeks | W10-W11 |
| 1.9.1 | Genesis parser: JSON config → initial state | 1.2.1 | 2 days | W10 |
| 1.9.2 | torus-node binary: wire all crates, CLI args | All crates | 3 days | W10-W11 |
| 1.9.3 | Devnet genesis file: 4 validators, test accounts | 1.9.1 | 1 day | W11 |
| 1.9.4 | Docker compose for 4-node local devnet | 1.9.2 | 2 days | W11 |
| **1.10** | **Integration testing and stabilization** | 1.9 | 4 weeks | W12-W15 |
| 1.10.1 | 4-node devnet: blocks producing with EVM transactions | 1.9.4 | 3 days | W12 |
| 1.10.2 | MetaMask connection test: send TRS, deploy contract | 1.10.1 | 2 days | W12 |
| 1.10.3 | Foundry test suite: forge test against devnet | 1.10.1 | 2 days | W12-W13 |
| 1.10.4 | Block sync: new node catches up to existing chain | 1.7.5 | 3 days | W13 |
| 1.10.5 | Leader rotation: verify blocks produced by different validators | 1.10.1 | 2 days | W13 |
| 1.10.6 | Fault tolerance: 1-of-4 validator goes down, chain continues | 1.10.1 | 2 days | W13-W14 |
| 1.10.7 | Gas accounting: verify EIP-1559 fee behavior | 1.10.1 | 2 days | W14 |
| 1.10.8 | State root consistency: all 4 validators agree on every block | 1.10.1 | 3 days | W14 |
| 1.10.9 | Performance baseline: TPS benchmark, latency measurement | 1.10.1 | 2 days | W14-W15 |
| 1.10.10 | Bug fix buffer | 1.10.1 | 5 days | W15 |
| **1.11** | **Basic staking (Phase 1 subset)** | 1.5 | 2 weeks | W13-W14 |
| 1.11.1 | Delegate/undelegate state management | 1.2.1 | 3 days | W13 |
| 1.11.2 | Epoch-based validator set rotation | 1.11.1, 1.4.5 | 3 days | W13-W14 |
| 1.11.3 | Delegator reward distribution (share of validator fee income, minus commission) | 1.11.2 | 2 days | W14 |
| 1.11.4 | Staking RPC endpoints | 1.11.1, 1.8.1 | 2 days | W14 |

### Phase 1 Parallel Tracks

```
Week: 1  2  3  4  5  6  7  8  9  10  11  12  13  14  15
      ├──┼──┼──┼──┼──┼──┼──┼──┼──┼───┼───┼───┼───┼───┤

Track A (State + EVM):
      ■■ ████████████ ██████████
      1.1  1.2           1.3

Track B (Consensus):
            ██████████████
               1.4

Track C (Bridge): ──────────────────── dependent on A+B
                              ████████████████
                                   1.5

Track D (Mempool):
                     ████████
                      1.6

Track E (Network):
                              ██████████████
                                   1.7

Track F (RPC):
                                    ██████████████
                                        1.8

Track G (Integration):
                                              ████████████████████
                                                1.9   1.10  1.11
```

**Parallelism:** Tracks A+B+D can run concurrently in weeks 1-6. Track C (bridge) is the critical convergence point. Tracks E+F can proceed once bridge works.

---

## 3. Phase 2: Native Order Book (Months 6-12)

**Goal:** Dual-VM architecture with native CLOB, margin engine, cross-VM precompiles, and full economic modules. Feature parity with Torus economics design.

### Task List

| ID | Task | Dependencies | Effort | Month |
|---|---|---|---|---|
| **2.1** | **Order book engine (torus-core)** | Phase 1 | 6 weeks | M6-M7 |
| 2.1.1 | Price-time priority matching engine | — | 5 days | M6 |
| 2.1.2 | Order types: market, limit, stop, post-only | 2.1.1 | 4 days | M6 |
| 2.1.3 | Time-in-force: GTC, IOC, FOK | 2.1.2 | 2 days | M6 |
| 2.1.4 | Order modification and cancel-all | 2.1.2 | 2 days | M6 |
| 2.1.5 | Matching engine unit tests (1000+ test cases) | 2.1.4 | 5 days | M6-M7 |
| 2.1.6 | Matching engine benchmarks (target: 200k orders/sec) | 2.1.4 | 3 days | M7 |
| **2.2** | **Margin engine** | 2.1 | 4 weeks | M7-M8 |
| 2.2.1 | Cross-margin model | 2.1.1 | 4 days | M7 |
| 2.2.2 | Isolated-margin model | 2.2.1 | 3 days | M7 |
| 2.2.3 | Margin tier system (leverage limits by notional) | 2.2.1 | 3 days | M7-M8 |
| 2.2.4 | Double margin check: at submission + at match | 2.2.3 | 3 days | M8 |
| 2.2.5 | Position PnL tracking (unrealized + realized) | 2.2.1 | 3 days | M8 |
| 2.2.6 | Margin edge case tests | 2.2.4 | 4 days | M8 |
| **2.3** | **Liquidation engine** | 2.2 | 3 weeks | M8-M9 |
| 2.3.1 | Continuous liquidation check (per-block scan) | 2.2.1 | 3 days | M8 |
| 2.3.2 | Force close execution at oracle price | 2.3.1 | 3 days | M8-M9 |
| 2.3.3 | Auto-Deleverage (ADL) for failed liquidations | 2.3.2 | 4 days | M9 |
| 2.3.4 | Socialized loss handling (protocol vault backstop) | 2.3.3 | 3 days | M9 |
| 2.3.5 | Liquidation safety tests (no negative equity) | 2.3.4 | 3 days | M9 |
| **2.4** | **Cross-VM precompiles** | Phase 1, 2.1 | 4 weeks | M8-M9 |
| 2.4.1 | Read precompiles: order book, positions, balances | 2.1.1 | 4 days | M8 |
| 2.4.2 | Read precompiles: oracle prices, staking, governance | 2.4.1 | 3 days | M8 |
| 2.4.3 | CoreWriter contract: place/cancel orders from EVM | 2.1.1 | 4 days | M8-M9 |
| 2.4.4 | CoreWriter: staking operations from EVM | 2.4.3 | 2 days | M9 |
| 2.4.5 | Lockbox: bidirectional asset transfer | 2.4.3 | 4 days | M9 |
| 2.4.6 | Delayed execution queue (anti-frontrunning) | 2.4.3 | 3 days | M9 |
| **2.5** | **Native action processing in bridge** | 2.1, 2.4 | 3 weeks | M9-M10 |
| 2.5.1 | Native mempool: action types, priority ordering | — | 3 days | M9 |
| 2.5.2 | Block proposal: include native actions + EVM txs | 2.5.1 | 3 days | M9-M10 |
| 2.5.3 | Block validation: execute native → EVM → lockbox → CoreWriter | 2.5.2 | 5 days | M10 |
| 2.5.4 | Composite state root: build native Merkle tree, combine with EVM root (see tech-req §6.3) | 2.5.3 | 3 weeks | M10 |
| 2.5.5 | Determinism tests: all validators agree with mixed workload | 2.5.4 | 4 days | M10 |
| **2.6** | **Permanent staking** | Phase 1.11 | 3 weeks | M9-M10 |
| 2.6.1 | Permanent stake locking from liquid balance (irreversible, separate from delegation) | 1.2.1 | 3 days | M9 |
| 2.6.2 | 5% annual inflation rewards for permanent stakers | 2.6.1 | 3 days | M9-M10 |
| 2.6.3 | Non-auto-compounding reward distribution | 2.6.2 | 2 days | M10 |
| 2.6.4 | Permanent stake state in genesis | 2.6.1 | 1 day | M10 |
| 2.6.5 | Integration tests: stake, earn, governance weight | 2.6.3 | 3 days | M10 |
| **2.7** | **Fee split module** | Phase 1 | 2 weeks | M10 |
| 2.7.1 | FeeSplitter with integer basis-point interpolation (no f64, 1825-epoch transition) | — | 3 days | M10 |
| 2.7.2 | Burn mechanism (send to zero address) | 2.7.1 | 1 day | M10 |
| 2.7.3 | Validator proposer reward + delegator redistribution (commission model) | 2.7.1 | 3 days | M10 |
| 2.7.4 | Treasury accumulation | 2.7.1 | 1 day | M10 |
| 2.7.5 | Developer pool: gas usage tracking per deployer | 2.7.1 | 3 days | M10 |
| 2.7.6 | Fee split unit tests + epoch transition tests | 2.7.5 | 2 days | M10 |
| **2.8** | **Governance** | 2.6 | 3 weeks | M10-M11 |
| 2.8.1 | Proposal submission and storage | — | 3 days | M10 |
| 2.8.2 | Voting with chain-computed weight (no user-supplied override) | 2.8.1 | 2 days | M10-M11 |
| 2.8.3 | 1.5x auto-computed vote weight for permanent stakers | 2.8.2, 2.6.1 | 2 days | M11 |
| 2.8.4 | Proposal types: param changes, treasury spends, market listing | 2.8.1 | 3 days | M11 |
| 2.8.5 | Proposal execution (apply approved changes) | 2.8.4 | 3 days | M11 |
| 2.8.6 | Governance RPC endpoints | 2.8.5 | 2 days | M11 |
| **2.8b** | **Oracle price feed system** | 2.1 | 3 weeks | M8-M9 |
| 2.8b.1 | Oracle submission: validators submit external exchange prices | 1.4 | 4 days | M8 |
| 2.8b.2 | Oracle aggregation: stake-weighted median across validators | 2.8b.1 | 3 days | M8-M9 |
| 2.8b.3 | Oracle staleness detection and fallback | 2.8b.2 | 2 days | M9 |
| 2.8b.4 | Oracle manipulation resistance (outlier rejection) | 2.8b.2 | 3 days | M9 |
| 2.8b.5 | Oracle integration tests | 2.8b.4 | 3 days | M9 |
| **2.9** | **Torus-specific RPC** | 2.1, 2.6, 2.8 | 2 weeks | M11 |
| 2.9.1 | torus_getOrderBook, torus_getPosition, torus_getBalances | 2.1 | 3 days | M11 |
| 2.9.2 | torus_getMarkets, torus_getTradeHistory | 2.1 | 2 days | M11 |
| 2.9.3 | torus_getStakingInfo, torus_getValidators | 2.6 | 2 days | M11 |
| 2.9.4 | torus_submitNativeAction | 2.5.1 | 2 days | M11 |
| 2.9.5 | torus_getGovernanceProposals | 2.8 | 1 day | M11 |
| **2.10** | **Integration testing and stabilization** | All above | 4 weeks | M11-M12 |
| 2.10.1 | End-to-end: place order via RPC → match → verify position | All | 3 days | M11 |
| 2.10.2 | Cross-VM: EVM contract reads order book via precompile | 2.4 | 2 days | M11 |
| 2.10.3 | Cross-VM: EVM contract places order via CoreWriter | 2.4 | 2 days | M11-M12 |
| 2.10.4 | Lockbox: transfer assets between VMs | 2.4.5 | 2 days | M12 |
| 2.10.5 | Staking lifecycle: delegate → permanent → earn → governance | 2.6, 2.8 | 3 days | M12 |
| 2.10.6 | Fee flow: EVM gas + trading fees → 4-way split | 2.7 | 2 days | M12 |
| 2.10.7 | Stress test: sustained 10k orders/sec with 4 validators | 2.1 | 3 days | M12 |
| 2.10.8 | Chaos testing: validator crashes, network partitions | — | 3 days | M12 |
| 2.10.9 | Bug fix buffer | — | 5 days | M12 |

---

## 4. Phase 3: Production Hardening (Months 12-18)

**Goal:** Security audit, external testnet, mainnet preparation.

### Task List

| ID | Task | Dependencies | Effort | Month |
|---|---|---|---|---|
| **3.1** | **Security hardening** | Phase 2 | 6 weeks | M12-M14 |
| 3.1.1 | Slashing implementation: double-sign detection | 2.6 | 4 days | M12 |
| 3.1.2 | Jailing: validator downtime detection + jail vote | 3.1.1 | 3 days | M12-M13 |
| 3.1.3 | Unjail mechanism with cooldown | 3.1.2 | 2 days | M13 |
| 3.1.4 | Rate limiting: per-address transaction limits | — | 2 days | M13 |
| 3.1.5 | Anti-MEV: CoreWriter delay enforcement | — | 2 days | M13 |
| 3.1.6 | State snapshot verification and recovery | — | 4 days | M13-M14 |
| 3.1.7 | Denial-of-service resilience testing | — | 3 days | M14 |
| 3.1.8 | Key management: validator key rotation | — | 3 days | M14 |
| **3.2** | **Dynamic validator set** | 3.1 | 4 weeks | M13-M14 |
| 3.2.1 | Validator registration via governance | 3.1.1 | 3 days | M13 |
| 3.2.2 | Epoch rotation with 21-validator cap | 3.2.1 | 3 days | M13-M14 |
| 3.2.3 | Commission rate management | 3.2.2 | 2 days | M14 |
| 3.2.4 | Validator set transition: consensus continuity during rotation | 3.2.2 | 4 days | M14 |
| 3.2.5 | Integration tests: validator joins, leaves, gets jailed | 3.2.4 | 3 days | M14 |
| **3.3** | **MonadBFT enhancements** (complete BEFORE audit) | Phase 1 | 8 weeks | M12-M14 |
| 3.3.1 | Fork hotstuff_rs for pacemaker modification | 1.4 | 5 days | M12 |
| 3.3.2 | Tail-fork resistance: reproposal mechanism | 3.3.1 | 5 days | M12-M13 |
| 3.3.3 | No-Endorsement Certificate (NEC) | 3.3.2 | 5 days | M13 |
| 3.3.4 | Speculative finality: execute after 1 QC | 3.3.3 | 5 days | M13 |
| 3.3.5 | Speculative rollback mechanism | 3.3.4 | 5 days | M13-M14 |
| 3.3.6 | Active PaceMaker with leader reputation (requires hotstuff_rs fork) | 3.3.3 | 8 days | M14 |
| 3.3.7 | Consensus safety proofs (TLA+ or similar) | 3.3.6 | 8 days | M14 |
| **3.4** | **External audit** (starts after MonadBFT complete) | 3.1, 3.2, 3.3 | 3-6 months | M15-M18 |
| 3.4.1 | Audit firm selection and scoping | — | 2 weeks | M15 |
| 3.4.2 | Consensus safety audit (includes MonadBFT changes) | 3.3 | 4 weeks | M15-M16 |
| 3.4.3 | EVM correctness audit | Phase 1 | 3 weeks | M15-M16 |
| 3.4.4 | Economic model audit (game theory review) | 2.6, 2.7 | 2 weeks | M16 |
| 3.4.5 | Fix audit findings | 3.4.2-3.4.4 | 4 weeks | M16-M17 |
| 3.4.6 | Re-audit critical findings | 3.4.5 | 2 weeks | M17-M18 |
| **3.5** | **Infrastructure and tooling** | Phase 2 | 4 weeks | M14-M16 |
| 3.5.1 | Block explorer backend (indexer) | 1.8 | 5 days | M14 |
| 3.5.2 | Block explorer frontend (basic) | 3.5.1 | 5 days | M14-M15 |
| 3.5.3 | Faucet for testnet | — | 2 days | M15 |
| 3.5.4 | CLI wallet tool | 1.8 | 3 days | M15 |
| 3.5.5 | Monitoring: Prometheus metrics + Grafana dashboards | — | 3 days | M15-M16 |
| 3.5.6 | Node operator documentation | — | 3 days | M16 |
| **3.6** | **Public testnet** | 3.2, 3.5 | 6 weeks | M15-M17 |
| 3.6.1 | Testnet genesis with 8-12 validators | 3.2 | 2 days | M15 |
| 3.6.2 | External validator onboarding | 3.6.1 | 5 days | M15-M16 |
| 3.6.3 | Public testnet launch | 3.6.2 | 3 days | M16 |
| 3.6.4 | Bug bounty program launch | 3.6.3 | 2 days | M16 |
| 3.6.5 | Testnet stress testing: sustained load from community | 3.6.3 | Ongoing | M16-M17 |
| 3.6.6 | Testnet stability monitoring and fixes | 3.6.3 | Ongoing | M16-M17 |
| **3.7** | **Mainnet preparation** | 3.4, 3.6 | 4 weeks | M17-M18 |
| 3.7.1 | Mainnet genesis creation | 3.6 | 3 days | M17 |
| 3.7.2 | Validator coordination and key ceremonies | 3.7.1 | 5 days | M17 |
| 3.7.3 | Final performance benchmarks | 3.7.1 | 3 days | M17-M18 |
| 3.7.4 | Upgrade mechanism design (binary versioning) | — | 3 days | M18 |
| 3.7.5 | Disaster recovery procedures | 3.1.6 | 3 days | M18 |
| 3.7.6 | Mainnet launch | All | — | M18 |

---

## 5. Critical Path

The critical path determines the minimum timeline. Delays on any critical path task delay the entire project.

### Phase 1 Critical Path

```
1.1 Workspace ──► 1.2 State Layer ──► 1.3 EVM Execution ──► 1.5 Bridge ──► 1.10 Integration
   (W1)              (W2-W4)            (W4-W6)               (W6-W9)       (W12-W15)
                         │
                         └──► 1.4 Consensus ──────────────────────┘
                              (W3-W5)

Critical path: 1.1 → 1.2 → 1.3 → 1.5 → 1.9 → 1.10 = 15 weeks
```

**The bridge (1.5) is the single highest-risk critical path item.** It requires both state+EVM and consensus to converge, and state root determinism bugs here block everything.

### Phase 2 Critical Path

```
2.1 Order Book ──► 2.2 Margin ──► 2.3 Liquidations ──► 2.5 Bridge Integration ──► 2.10 Testing
   (M6-M7)          (M7-M8)        (M8-M9)               (M9-M10)                  (M11-M12)

Critical path: 2.1 → 2.2 → 2.3 → 2.5 → 2.10 = 7 months (zero slack — any delay cascades)
```

**Warning:** Phase 2 critical path exactly fills the 7-month window. Budget 4-6 weeks
slack by starting oracle work (2.8b) and precompiles (2.4) in parallel with order book.

### Phase 3 Critical Path

```
3.3 MonadBFT ──► 3.1 Security ──► 3.4 Audit ──► 3.4.5 Fixes ──► 3.6 Testnet ──► 3.7 Mainnet
   (M12-M14)       (M12-M14)       (M15-M16)     (M16-M17)       (M15-M17)       (M17-M18)

Critical path: MonadBFT must complete before audit starts (M15). Audit findings determine tail.
```

### Blocking Dependencies

| Blocker | What It Blocks | Mitigation |
|---|---|---|
| State root correctness (1.2.4) | All block validation | Test against Ethereum reference tests early |
| Consensus-execution bridge (1.5) | Network, RPC, integration | Start with simple App trait stub, iterate |
| revm API changes | EVM execution | Pin v19.2.0 exactly like Hyperliquid |
| hotstuff_rs stability | Consensus layer | Evaluate code quality W1; prepare fork if needed |
| Matching engine perf (2.1.6) | Native throughput target | Profile early, use SIMD/cache-friendly structures |
| Audit scheduling (3.4.1) | Mainnet timeline | Engage auditors by month 10 |

---

## 6. Risk Register

### Critical Risks

| ID | Risk | Probability | Impact | Mitigation | Owner |
|---|---|---|---|---|---|
| R1 | **State root non-determinism** across validators | High | Critical | Use reth-trie (battle-tested). Run Ethereum GST suite. Fuzz state transitions. | State lead |
| R2 | **hotstuff_rs bugs or abandonment** | Medium | Critical | Evaluate code quality in W1. Prepare to fork and maintain. Contribute fixes upstream. 58 stars = small community. | Consensus lead |
| R3 | **Consensus-execution bridge complexity** exceeds estimate | High | High | Budget 4 weeks (not 2). Start with minimal bridge, add features incrementally. This is the hardest part of the project. | Bridge lead |
| R4 | **revm API breaking changes** when upgrading from v19 | Medium | High | Pin `=19.2.0` initially. Plan upgrade path separately. Hyperliquid has survived on v19 for 18+ months. | EVM lead |
| R5 | **Audit discovers critical consensus flaw** | Medium | Critical | Start TLA+ modeling by month 10. Engage auditors early for pre-audit review. Budget 4 weeks for fixes. | All |

### High Risks

| ID | Risk | Probability | Impact | Mitigation |
|---|---|---|---|---|
| R6 | Matching engine fails to meet 200k/sec target | Medium | High | Profile in isolation before integration. Study Hyperliquid's architecture decisions. Consider SIMD, memory-mapped order book. |
| R7 | libp2p integration with hotstuff_rs is non-trivial | Medium | High | Start with in-process channels for testing. Swap to libp2p gradually. |
| R8 | Cross-VM precompile state consistency | Medium | High | EVM always reads one-block-old native state (like Hyperliquid). Test extensively. |
| R9 | No IBC support limits ecosystem adoption | Low | Medium | Accept for v1. Plan IBC module for v2 (post-mainnet). |
| R10 | Hiring: Rust + blockchain engineers are scarce | High | High | Start with 2 strong hires. Target Reth/Lighthouse/Solana alumni. Budget competitive compensation. |

### Medium Risks

| ID | Risk | Probability | Impact | Mitigation |
|---|---|---|---|---|
| R11 | Permanent staking game theory creates perverse incentives | Medium | Medium | Run economic simulations before launch. Set conservative 5% rate. Monitor and adjust via governance. |
| R12 | Developer pool gas tracking is gameable | Medium | Medium | Per-program cap (10%). Cooldown period (7 epochs). Community governance can adjust. |
| R13 | RocksDB performance under sustained write load | Low | Medium | Benchmark with realistic workload. Tune compaction. Consider MDBX or redb as fallback. |
| R14 | Genesis format changes break node compatibility | Low | Medium | Version genesis format. Validate against schema. |

---

## 7. Testing Strategy

### 7.1 Unit Tests

| Component | Test Focus | Target Coverage | Framework |
|---|---|---|---|
| torus-types | Serialization roundtrips, encoding correctness | 90%+ | cargo test |
| torus-state | RocksDB CRUD, column family isolation, state root computation | 95%+ | cargo test + proptest |
| torus-evm | EVM execution, receipt generation, gas accounting | 90%+ | cargo test + ethereum-tests |
| torus-core | Order matching, margin checks, liquidation logic | 95%+ | cargo test + proptest |
| torus-economics | Fee split ratios, staking rewards, governance weight | 95%+ | cargo test |
| torus-bridge | Block construction, validation, commit pipeline | 90%+ | cargo test |
| torus-mempool | Ordering, eviction, nonce tracking | 85%+ | cargo test |

### 7.2 Integration Tests

| Test | Description | Frequency |
|---|---|---|
| 4-node consensus | Blocks produced, committed, state roots agree | Every PR |
| EVM end-to-end | Deploy contract → interact → verify state | Every PR |
| Block sync | New node catches up from genesis | Weekly |
| Fault tolerance | Kill 1 of 4 validators, verify liveness | Weekly |
| Leader rotation | All validators produce blocks over 100 rounds | Every PR |
| Dual-VM (Phase 2) | Native order → EVM read → CoreWriter → verify | Every PR |
| Fee flow (Phase 2) | Transactions → fee split → verify distributions | Every PR |

### 7.3 Ethereum Test Vectors

| Suite | Source | What It Tests | Phase |
|---|---|---|---|
| General State Tests (GST) | `ethereum/tests` | EVM opcode correctness, state transitions | Phase 1 |
| Blockchain Tests | `ethereum/tests` | Block validation, uncle handling (N/A), difficulty (N/A) | Phase 1 |
| Transaction Tests | `ethereum/tests` | RLP decoding, signature verification, nonce | Phase 1 |
| EIP-1559 Tests | `ethereum/tests` | Base fee calculation, priority fee | Phase 1 |
| Cancun Tests | `ethereum/tests` | EIP-4788, EIP-1153 (transient storage), EIP-5656 (MCOPY) | Phase 1 |

### 7.4 Consensus Safety Tests

| Test | Methodology | Phase |
|---|---|---|
| No forking under honest majority | Run 4 nodes, verify single chain | Phase 1 |
| Byzantine leader | 1 malicious leader proposes conflicting blocks | Phase 1 |
| Network partition (50/50) | Split network, verify no commits during partition, recovery after | Phase 2 |
| State divergence detection | Inject bad state root, verify rejection | Phase 1 |
| View change | Kill leader, verify smooth transition | Phase 1 |
| Tail-fork resistance | Malicious leader drops txs, verify reproposal | Phase 3 |

### 7.5 Performance Benchmarks

| Benchmark | Target | Tool |
|---|---|---|
| EVM TPS (simple transfers) | >5,000 TPS | Custom load generator |
| EVM TPS (contract interactions) | >2,000 TPS | Foundry scripts |
| Native orders/sec | >200,000 | Custom load generator |
| Block finality latency | <500ms (4 validators) | Timestamp measurement |
| State root computation time | <50ms per block | criterion benchmarks |
| RPC response latency (eth_getBalance) | <5ms | wrk/vegeta |
| P2P message propagation | <100ms (4 nodes) | Instrumented logging |
| Block sync speed | >100 blocks/sec | Timing from genesis |

### 7.6 Chaos Testing (Phase 2+)

| Scenario | Expected Behavior |
|---|---|
| Kill random validator every 10 minutes | Chain continues (3/4 honest quorum) |
| Introduce 500ms network latency | Blocks slow but finality maintained |
| Disk full on one validator | Node stops cleanly, recovers on restart |
| Clock skew (±5s) on one validator | Tolerant (timestamp validation) |
| Rapid leader rotation (1s timeouts) | View changes succeed, no stuck views |
| Double-signing attempt | Slashing triggered, evidence recorded |

---

## 8. Milestone Definitions

### M1: Workspace Ready (Week 1)
- [ ] Cargo workspace with all crate stubs compiles (`cargo check`)
- [ ] CI runs: check, clippy, test, fmt
- [ ] `torus-types` defines Block, BlockHeader, StateDiff, ChainConfig
- **"Done" means:** `cargo test` passes on all crates (trivially, with stubs)

### M2: State + EVM Working (Week 6)
- [ ] RocksDB stores and retrieves accounts, storage, code
- [ ] revm executes transactions against RocksDB state
- [ ] State root computed correctly via reth-trie
- [ ] Ethereum General State Tests passing (≥95% of applicable tests)
- [ ] EIP-1559 base fee updates correctly
- **"Done" means:** Can execute a block of EVM transactions and compute a correct state root

### M3: Consensus Producing Blocks (Week 9)
- [ ] 4 hotstuff_rs replicas reach consensus on block ordering
- [ ] App trait wired to bridge: blocks contain EVM transactions
- [ ] All 4 validators compute same state root for each block
- [ ] Leader rotation works (all 4 produce blocks)
- [ ] One validator crash → chain continues (3/4 quorum)
- **"Done" means:** Multi-validator consensus with deterministic EVM execution

### M4: Devnet Live (Week 15)
- [ ] 4-node devnet running in Docker Compose
- [ ] MetaMask connects, sends TRS, shows balance
- [ ] Foundry `forge script` deploys and interacts with contracts
- [ ] eth_getLogs returns correct event logs
- [ ] WebSocket subscriptions deliver newHeads
- [ ] Block sync: new node catches up from genesis
- [ ] Basic staking: delegate, undelegate, earn rewards
- **"Done" means:** A developer can connect standard Ethereum tooling and interact with the chain

### M5: Native Order Book Live (Month 9)
- [ ] Place/cancel orders via RPC
- [ ] Price-time priority matching produces correct trades
- [ ] Cross-margin and isolated-margin both work
- [ ] Liquidation engine closes underwater positions
- [ ] 200k+ orders/sec on a single node
- **"Done" means:** The matching engine is production-quality in isolation

### M6: Dual-VM Integration (Month 12)
- [ ] EVM contracts read native order book via precompiles
- [ ] EVM contracts place orders via CoreWriter (delayed by 1 block)
- [ ] Lockbox transfers assets bidirectionally
- [ ] Permanent staking earns 5% APY
- [ ] Fee split distributes to burn/validator/treasury/dev-pool
- [ ] Governance proposals with 1.5x permanent-staker weight
- [ ] All features work deterministically across 4 validators
- **"Done" means:** Full feature parity with Torus economics design

### M7: Testnet Launch (Month 16)
- [ ] 8-12 validators including external operators
- [ ] Public RPC endpoint
- [ ] Block explorer live
- [ ] Faucet operational
- [ ] Bug bounty program active
- [ ] Audit in progress (≥50% complete)
- **"Done" means:** External users can test the chain

### M8: Mainnet Ready (Month 18)
- [ ] Security audit complete (all critical/high findings resolved)
- [ ] 21 validators with geographic distribution
- [ ] Slashing and jailing implemented and tested
- [ ] Disaster recovery procedures documented and tested
- [ ] Performance benchmarks meet targets under sustained load
- [ ] Node operator documentation complete
- [ ] Upgrade mechanism tested (binary versioning)
- **"Done" means:** Production deployment with acceptable risk

---

## Appendix A: Effort Estimates Summary

| Phase | Tasks | Total Effort | Calendar Time | Team Size |
|---|---|---|---|---|
| Phase 1 | 42 tasks | ~75 engineer-weeks | 15 weeks | 3-4 |
| Phase 2 | 50 tasks | ~115 engineer-weeks | 28 weeks | 3-4 |
| Phase 3 | 37 tasks | ~90 engineer-weeks | 24 weeks | 3-5 + auditors |
| **Total** | **129 tasks** | **~280 engineer-weeks** | **18 months** | **3-5** |

**Note on team sizing:** Phase 1's critical path is 15 weeks, but 75 engineer-weeks
requires 3-4 engineers for full parallelism across tracks A-D. With only 2 engineers,
realistic calendar time extends to ~25 weeks. Budget 3 engineers minimum for Phase 1.

## Appendix B: Technology Comparison — Why These Choices

| Component | Our Choice | Alternative | Why Not Alternative |
|---|---|---|---|
| Consensus | hotstuff_rs | malachite | Malachite acquired by Circle, no longer freely available |
| EVM | revm v19 | evmone (C++) | C FFI overhead, ecosystem mismatch |
| State root | reth-trie | custom MPT | Battle-tested by Reth, avoid reinventing |
| Storage | RocksDB | redb | RocksDB is what Hyperliquid uses; proven at scale |
| Networking | libp2p | custom TCP | libp2p has peer management, DHT, QUIC built in |
| RPC | jsonrpsee | axum + custom | jsonrpsee is what Reth uses; typed RPC, WS built in |
| Serialization (consensus) | Borsh | bincode, protobuf | Fixed-size, deterministic, faster than protobuf |
| Serialization (RPC) | JSON (serde_json) | — | Ethereum standard |

## Appendix C: Team Structure Recommendation

| Role | Count | Focus | Phase |
|---|---|---|---|
| Rust/Blockchain Engineer (Senior) | 2 | Consensus + bridge, state + EVM | Phase 1+ |
| Rust/Distributed Systems Engineer | 1 | Networking, mempool, sync | Phase 1+ |
| Rust/Financial Systems Engineer | 1 | Order book, margin, liquidation | Phase 2+ |
| Security Researcher | 1 | Ongoing review, audit prep, formal verification | Phase 2+ |
| DevOps / Infrastructure | 1 | CI, deployment, monitoring, testnet ops | Phase 2+ |

**Minimum viable team for Phase 1: 2 senior Rust engineers.**
- Engineer A: State layer + EVM execution + bridge
- Engineer B: Consensus integration + networking + RPC
