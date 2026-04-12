# Torus-hyperBFT: Chain Foundation Research Report

**Date:** 2026-04-12
**Methodology:** 8 parallel research agents covering HotStuff implementations, EVM engines, Sei v2, Monad, Aptos, Sui/Flow/others, Cosmos SDK, and custom stack feasibility.
**Goal:** Identify the most practical foundation for a Hyperliquid-like chain with sub-second finality, on-chain CLOB, EVM, and custom economics.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [HotStuff Consensus Implementations](#2-hotstuff-consensus-implementations)
3. [EVM Execution Engines](#3-evm-execution-engines)
4. [Sei v2](#4-sei-v2)
5. [Monad](#5-monad)
6. [Aptos](#6-aptos)
7. [Sui, Flow, and Other HotStuff-Family Chains](#7-sui-flow-and-other-hotstuff-family-chains)
8. [Cosmos SDK + CometBFT Ecosystem](#8-cosmos-sdk--cometbft-ecosystem)
9. [Custom Stack Feasibility](#9-custom-stack-feasibility)
10. [Comparison Matrix](#10-comparison-matrix)
11. [Technical Risks and Unknowns](#11-technical-risks-and-unknowns)
12. [Recommendation](#12-recommendation)

---

## 1. Executive Summary

After evaluating 19 HotStuff implementations, 11 EVM engines, 8 chain architectures, and the custom-stack feasibility landscape, three viable paths emerge for Torus-hyperBFT:

| Path | Time to Devnet | Performance Ceiling | Risk Level |
|---|---|---|---|
| **A: Cosmos SDK + CometBFT** (existing torus-chain stack) | 2-4 weeks | ~3,000 TPS, 500ms-1s finality | Low |
| **B: Sei v2 Fork** | 4-8 weeks | ~12,000 TPS, 400ms finality | Medium |
| **C: Custom Rust Stack** (hotstuff_rs + revm) | 16-20 weeks | ~200,000 orders/sec, <200ms finality | High |

**Recommendation: Path C (Custom Rust Stack) with a phased approach.** It is the only path that achieves Hyperliquid-parity performance. Paths A and B permanently cap performance below the target. The phased approach mitigates risk: EVM-only devnet in 16-20 weeks, native order book in months 6-12, production hardening in months 12-18.

Key cross-cutting findings:
- **revm is the unambiguous EVM engine choice** (pure Rust, MIT, used by Hyperliquid)
- **MonadBFT has the best publicly available consensus spec** (arxiv paper + Rust reference code)
- **No existing chain is a clean fork target** -- Aptos has a license blocker, Monad is GPL-3.0, Sui has no EVM, Flow is AGPL
- **Sei v2 is the closest open-source analog** but its CLOB module is being orphaned and CometBFT-variant consensus caps latency
- **CometBFT cannot achieve sub-200ms finality** -- this is a protocol-level constraint, not a tuning issue

---

## 2. HotStuff Consensus Implementations

19 implementations surveyed across Rust, Go, and C++.

### Summary Table (Top Candidates)

| Implementation | Language | License | Maturity | Library? | Dynamic Validators | Block Sync |
|---|---|---|---|---|---|---|
| **parallelchain-io/hotstuff_rs** | Rust | Apache-2.0 | Beta/near-production | **Yes (crates.io)** | Yes | Yes |
| **relab/hotstuff** | Go | MIT | Research/active | Partial | No | No |
| **hot-stuff/libhotstuff** | C++ | Apache-2.0 | Research/stale | Partial | No | No |
| **EspressoSystems/HotShot** | Rust | MIT (archived) | Production (moved) | Partial | Yes | Yes |
| **cometbft/cometbft** | Go | Apache-2.0 | Production | **Yes (ABCI)** | Yes | Yes |
| **informalsystems/malachite** | Rust | Apache-2.0 | Alpha | Yes | Yes | WIP |

### Key Findings

**hotstuff_rs (Rust, Apache-2.0)** is the strongest candidate for embedding:
- Only HotStuff library on crates.io (v0.4.0)
- Pluggable `App`, `KVStore`, `Network` traits -- cleanest consensus/execution separation
- Dynamic validator sets and block sync built in
- Used in production by ParallelChain Mainnet
- Risk: small community (58 stars), single maintainer organization

**HotStuff-2 has no production implementation.** The 2-QC variant from Malkhi's 2023 paper would need to be built from scratch or added as a module to relab/hotstuff.

**MonadBFT (arxiv:2502.20692)** is the most advanced publicly specified protocol:
- Tail-forking resistance via reproposal + No-Endorsement Certificates
- Speculative finality in 1 round (~400ms), deterministic in 2 rounds (~800ms)
- Full Rust reference implementation at category-labs/monad-bft (GPL-3.0)
- The paper provides sufficient detail for independent reimplementation

**Production HotStuff implementations (Aptos, Flow, Sui)** are deeply coupled to their execution layers and cannot be extracted as standalone libraries without major effort.

### Protocol Comparison

| Protocol | Commit Rounds | Responsiveness | Tail-Fork Resistant | Communication | Status |
|---|---|---|---|---|---|
| HotStuff (original) | 3 | No | No | O(n) | Reference only |
| Jolteon/DiemBFT v4 | 2 | Yes | No | O(n) | Aptos, Flow production |
| HotStuff-2 | 2 | Yes | No | O(n) | Paper only |
| MonadBFT | 2 (speculative: 1) | Yes | **Yes** | O(n) | Monad production |
| Fast-HotStuff | 2 | Yes | No | O(n) | relab/hotstuff |
| Tendermint/CometBFT | 2 | No | N/A | O(n^2) | 200+ chains |
| Mysticeti (DAG) | 3 messages | Yes | N/A | O(n) per round | Sui production |

---

## 3. EVM Execution Engines

11 engines evaluated. The choice is unambiguous.

### Summary Table

| Engine | Language | License | Integration | Precompile Extensibility | Performance | Verdict |
|---|---|---|---|---|---|---|
| **revm** | Rust | MIT | Native crate | Excellent (stateful) | 100-200 MGas/s | **PRIMARY CHOICE** |
| rust-ethereum/evm (SputnikVM) | Rust | Apache-2.0 | Native crate | Good | Unknown | Fallback |
| go-ethereum/core/vm | Go | LGPL-3.0 | Go module / CGo | Moderate (fork required) | ~161 MGas/s | No (language + LGPL) |
| evmone | C++ | Apache-2.0 | EVMC shared lib | Moderate (host-side) | ~784 MGas/s | No (C FFI complexity) |
| Monad EVM | C++/Rust | GPL-3.0 | Not embeddable | N/A | Production | No (not a library) |
| revmc (JIT) | Rust | MIT | Addon to revm | Same as revm | 2-19x over revm | Future optimization |

### Why revm

1. **Language alignment:** Pure Rust, inline in consensus execution loop (how Hyperliquid does it)
2. **License:** MIT -- no copyleft, no restrictions
3. **Pluggable state:** `Database` trait -- bring your own storage backend
4. **Custom precompiles:** `StatefulPrecompile` trait + `append_handler_register_box()` -- the exact pattern HyperEVM uses for CoreWriter/lockbox precompiles
5. **Ecosystem:** Used by Reth, Foundry, Optimism, Scroll, Hyperliquid. alloy-rs types shared across the stack
6. **Performance path:** revm baseline (100-200 MGas/s) -> pevm parallel (1.7x average) -> revmc JIT (2-19x)

### State Database Options

| Database | Language | Notes | Recommendation |
|---|---|---|---|
| RocksDB | C++ (Rust bindings) | LSM-tree, write-optimized, used by Hyperliquid | **Start here** |
| redb | Pure Rust | ACID, B-tree, fast compilation | Alternative |
| MDBX | C (Rust bindings) | Memory-mapped, used by Erigon | Viable |
| QMDB | Rust | 2.28M state updates/sec, research-grade | Future evaluation |

---

## 4. Sei v2

### Architecture Summary

Sei v2 is a Cosmos SDK fork with aggressive optimizations:
- **Twin-Turbo consensus:** Modified CometBFT with optimistic block processing and intelligent block propagation. ~400ms block time.
- **SeiDB:** Two-tier storage (MemIAVL + PebbleDB) replacing vanilla IAVL. 287x faster block commits.
- **OCC parallel execution:** Optimistic concurrency control across both Cosmos-native and EVM transactions. 3-5x speedup.
- **Dual VM:** Native Cosmos modules + embedded Geth EVM with precompile bridges.
- **x/dex:** Native CLOB module with batch auction semantics (end-of-block matching).

### Fit Assessment

| Requirement | Status | Notes |
|---|---|---|
| Sub-second finality | ~400ms deterministic | Strong fit |
| On-chain CLOB | x/dex exists but orphaned | SIP-3 deprecating CosmWasm breaks it |
| EVM support | Full Geth, Cancun-spec | Strong fit |
| Custom economics | Standard Cosmos module patterns | 12-20 weeks estimated |
| Performance ceiling | ~12,000 TPS theoretical | Below Hyperliquid |
| License | Apache-2.0 | Excellent |
| Upstream trajectory | Moving EVM-only (SIP-3) | Risk: fork diverges from upstream |

### Critical Finding: x/dex Is Being Abandoned

The native order book module is tightly coupled to CosmWasm (requires sudo endpoints on registered contracts). SIP-3 (approved May 2025) deprecates CosmWasm entirely. All major DEX activity on Sei mainnet has migrated to EVM AMMs (DragonSwap). A Torus fork wanting the native CLOB must either keep CosmWasm (maintenance burden) or decouple x/dex from it (non-trivial refactoring).

### Fork Recommendation

Fork at v6.4.1. The monorepo consolidation simplifies dependency management. But understand: the upstream is moving in the opposite direction from what Torus needs (EVM-only, dropping native modules).

---

## 5. Monad

### Architecture Summary

Ground-up C++ (91%) + Rust (3%) chain with:
- **MonadBFT:** HotStuff-lineage with tail-forking resistance (reproposal + NEC). 400ms blocks, 800ms deterministic finality.
- **Parallel EVM:** Optimistic concurrency control, speculative execution, 5,200 TPS mainnet peak.
- **MonadDB:** Custom Patricia trie with io_uring async I/O.
- **RaptorCast:** Erasure-coded block propagation for large validator sets.

### Key Findings

- **Open source (GPL-3.0)** -- both consensus (Rust) and execution (C++) repos available
- **No native order book** -- only EVM Solidity CLOBs (Kuru, Clober)
- **Mainnet live since Nov 2025:** 400ms blocks, up to 200 validators
- **GPL-3.0 copyleft** forces any fork to also be GPL-3.0

### Best Used As: Architectural Inspiration

MonadBFT's formal specification (arxiv:2502.20692) is the most technically rigorous public HotStuff-lineage protocol. Key innovations to study and potentially reimplement:
1. Tail-forking resistance (reproposal + NEC mechanism)
2. Speculative finality (1-round execution safety)
3. RaptorCast (erasure-coded propagation)

The `category-labs/monad-bft` Rust codebase is a readable reference -- study it but reimplement independently to avoid GPL contamination.

---

## 6. Aptos

### Architecture Summary

Most advanced HotStuff-lineage chain in production:
- **AptosBFT v4 (Jolteon):** 2-chain HotStuff with active pacemaker. Sub-50ms block times (Velociraptr).
- **Block-STM:** Optimistic parallel execution, 160k+ TPS in benchmarks.
- **Shoal++/Raptr/Zaptos:** Cutting-edge DAG-BFT research with sub-second finality at 260k TPS.
- **Move VM:** Resource-oriented, formally verifiable, but not EVM.

### License: Hard Blocker

**Innovation-Enabling Source Code License:**
- Non-production, non-commercial use only for 4 years from code publication
- Explicitly prohibits "creating, publishing, deploying, launching or offering any blockchain or protocols"
- "Competing Use" includes any L1 or L2 blockchain development
- Auto-converts to Apache-2.0 after 4 years (earliest: October 2026 for mainnet launch code)
- Recent innovations (Baby Raptr, Velociraptr, Zaptos) won't be free until 2028-2029

### What We Can Use

Published academic papers are freely available: Jolteon (arxiv:2106.10362), Shoal++ (arxiv:2405.20488), Raptr (arxiv:2504.18649), Zaptos (arxiv:2501.10612). Study the algorithms. Don't touch the code.

---

## 7. Sui, Flow, and Other HotStuff-Family Chains

### Sui

- **Consensus:** Mysticeti -- uncertified DAG, 3-message-round commits, 390ms consensus latency
- **License:** Apache-2.0
- **EVM:** **None, and architecturally blocked.** Object-ownership model incompatible with EVM's dynamic state.
- **Consensus separability:** `consensus_core` crate has `TransactionVerifier` seam, but no published stable API
- **Verdict:** Mysticeti paper is academically valuable. The chain itself is a poor fit (no EVM).

### Flow

- **Consensus:** Jolteon (Go), clean multi-module implementation
- **License:** **AGPL-3.0** -- hard commercial blocker
- **EVM:** Full equivalence since Crescendo (September 2024)
- **Performance:** 0.8s block time, 14s finality
- **Verdict:** AGPL kills it for a commercial project.

### Zilliqa 2.0

- **Consensus:** Pipelined Fast-HotStuff (Rust)
- **License:** Apache-2.0 / MIT (ideal)
- **EVM:** Full EVM support
- **Performance:** 2s blocks, **5s finality** (too slow)
- **Verdict:** Perfect license and tech stack, but 10x too slow.

### Others

| Chain | Consensus | License | EVM | Relevance |
|---|---|---|---|---|
| Cypherium | Hybrid PoW + HotStuff | MIT | Yes | Low -- PoW model wrong for trading chain |
| ThunderCore | PaLa (not HotStuff) | Unknown | Yes | Low -- different protocol family |
| Hotstuff Labs L1 | DracoBFT | Unknown | Unknown | Monitor as competitor |

---

## 8. Cosmos SDK + CometBFT Ecosystem

### What It Gets Right

- **Module system is excellent** for custom economics. x/permstake, x/fees, x/gov extensions follow well-established patterns.
- **ABCI 2.0** (PrepareProposal/ExtendVote) enables on-chain oracle injection and block-level batch processing.
- **cosmos/evm v0.6** precompiles bridge EVM calls to Cosmos SDK keepers.
- **IBC** provides native access to Cosmos liquidity ecosystem.
- **Unordered transactions** (v0.53) reduce client-side complexity for HFT.

### What It Gets Wrong

- **CometBFT latency floor (~400ms-1s)** is a protocol-level constraint. Hyperliquid's 70ms is structurally impossible.
- **Sequential EVM execution** in current cosmos/evm -- hundreds of TPS. BlockSTM planned for v0.54 (late 2026).
- **cosmos/evm is pre-v1** (v0.6 as of March 2026). Under audit.
- **CometBFT v1 is terminal** -- development moved to v0.39 line.

### CometBFT Replacement Options

| Engine | Type | License | Status | Notes |
|---|---|---|---|---|
| **Meter Supernova Core** | HotStuff + BLS, ABCI-compatible | Open source | Production (300+ validators, 4 years) | No IBC integration |
| Malachite | Tendermint in Rust | Apache-2.0 | Acquired by Circle (captive) | No longer freely available |
| Rollkit | DA-layer sequencer | Apache-2.0 | Single-sequencer only | Not suitable for sovereign BFT |

### Production Cosmos + EVM Chains

| Chain | Block Time | Observed TPS | Notes |
|---|---|---|---|
| dYdX v4 | ~1s | 500-2,000 fills/sec | Off-chain orderbook, on-chain fills |
| Cronos | 0.49s | 0.33-141 TPS | Light load |
| Evmos | ~2s | Low | Canonical Cosmos EVM |
| Kava | ~6s | Hundreds | Dual co-chain |

No existing Cosmos + EVM chain achieves Hyperliquid-level performance.

---

## 9. Custom Stack Feasibility

### Component Availability

| Component | Available Form | Build Effort |
|---|---|---|
| HotStuff consensus | hotstuff_rs (full library) | Near-zero |
| EVM executor | revm (full) | Near-zero |
| Ethereum types | alloy-primitives, alloy-consensus, alloy-eips | Near-zero |
| JSON-RPC framework | jsonrpsee | Low |
| Transaction pool | reth-transaction-pool (adaptable) | Medium |
| Merkle Patricia Trie | reth-trie | Medium |
| P2P networking | libp2p (QUIC + GossipSub) | Medium |
| State storage | RocksDB or redb | Low |
| **Consensus-execution bridge** | **Nothing reusable** | **High (8-15k LoC)** |
| **Block proposal construction** | **Nothing reusable** | **High** |
| **Native order book engine** | limitbook (basic only) | **High (5-15k LoC)** |

### The hotstuff_rs + revm Integration

The `App` trait from hotstuff_rs provides `produce_block()`, `validate_block()`, `validate_block_for_sync()`. The `Database` trait from revm provides `basic()`, `storage()`, `code_by_hash()`, `block_hash()`. The bridge between these two traits -- transaction ordering, EVM execution, state diff collection, MPT state root computation, receipt/log storage -- is the core engineering challenge (~8,000-15,000 lines of Rust).

### Timeline Estimate

| Milestone | Time | Team |
|---|---|---|
| HotStuff blocks producing (no EVM) | 4 weeks | 2-3 senior Rust engineers |
| Basic EVM transactions working | 10 weeks | State root correctness is hardest |
| JSON-RPC + MetaMask working | 16 weeks | Mempool + gossip complexity |
| Stable 4-validator devnet | 20 weeks | Debugging, sync, edge cases |
| Native order book (HyperCore equivalent) | +3-6 months | Matching engine, margin, liquidations |
| Custom staking/governance | +1-2 months | x/permstake, x/fees equivalents |
| Production hardening + audit | +3-6 months | Security review |

**Total to mainnet-ready: 18-24 months** with a 3-5 person team.

### What Hyperliquid Proves

Hyperliquid built this exact stack with ~5-6 engineers. Their open-source `hyper-evm-sync` tool reveals the architecture:
- revm 19.2.0 with alloy 0.9.2 and reth-primitives v1.1.5
- Custom `State` trait extending revm's `Database` and `DatabaseRef`
- `CfgEnvWithHandlerCfg` (SpecId::CANCUN) + custom precompile handlers
- Block execution order: HyperCore -> EVM block -> EVM-to-Core transfers -> CoreWriter actions
- Chain IDs: mainnet=999, testnet=998

---

## 10. Comparison Matrix

### Foundation Options

| Dimension | Custom Rust Stack | Sei v2 Fork | Cosmos SDK + CometBFT | Monad Fork | Aptos Fork |
|---|---|---|---|---|---|
| **Language** | Rust throughout | Go + C++ (Geth) | Go throughout | C++ + Rust | Rust + Move |
| **License** | Apache-2.0/MIT (all deps) | Apache-2.0 | Apache-2.0 | GPL-3.0 | Restricted (4yr) |
| **Time to devnet** | 16-20 weeks | 4-8 weeks | 2-4 weeks | 4-8 weeks | Blocked |
| **Time to mainnet** | 18-24 months | 8-14 months | 6-12 months | 8-14 months | Blocked |
| **Block finality** | <200ms (HotStuff) | ~400ms (Twin-Turbo) | 500ms-1s (CometBFT) | ~800ms (MonadBFT) | N/A |
| **EVM TPS** | ~10,000+ | ~5,000-12,000 | ~300-3,000 | ~5,000-10,000 | No EVM |
| **Native TPS** | ~200,000 orders/sec | N/A | Via modules (~1,000) | N/A (EVM only) | N/A |
| **On-chain CLOB** | Build native (Rust) | x/dex (orphaned) | Build module (Go) | EVM Solidity only | N/A |
| **Custom economics** | First-class (native Rust) | Cosmos modules (Go) | Cosmos modules (Go) | C++ precompile | Move modules |
| **IBC support** | Must build or skip | Inherited | Native | No | No |
| **Audit complexity** | Very high (novel stack) | Medium | Medium-low | Medium | N/A |
| **Team size** | 3-5 senior Rust | 3-5 Go | 2-3 Go | 3-5 mixed | N/A |
| **Hyperliquid parity** | Achievable | Partial | Not achievable | Partial | N/A |

### Consensus Options

| Consensus | Finality | Throughput | Comm. Complexity | Tail-Fork Safe | Library? | License |
|---|---|---|---|---|---|---|
| CometBFT v1 | 500ms-1s | ~3,000 TPS | O(n^2) | N/A | Yes (ABCI) | Apache-2.0 |
| hotstuff_rs | ~200ms | ~10,000+ TPS | O(n) | No | Yes (crates.io) | Apache-2.0 |
| MonadBFT (reimplement) | ~400ms spec/~200ms opt | ~10,000+ TPS | O(n) | **Yes** | Paper + ref | Paper free |
| Mysticeti (extract) | ~390ms | ~200,000 TPS | O(n)/round | N/A | No stable API | Apache-2.0 |
| Meter Supernova | ~200-300ms | ~9,000 TPS | O(n) | No | ABCI drop-in | Open source |

### EVM Engine Options

| Engine | Language | License | Precompiles | Performance | Verdict |
|---|---|---|---|---|---|
| **revm** | Rust | MIT | Excellent (stateful) | 100-200 MGas/s | **Use this** |
| rust-ethereum/evm | Rust | Apache-2.0 | Good | Unknown | Fallback only |
| go-ethereum | Go | LGPL-3.0 | Moderate | ~161 MGas/s | Wrong language |
| evmone | C++ | Apache-2.0 | Host-side only | ~784 MGas/s | C FFI overhead |

---

## 11. Technical Risks and Unknowns

### Path C (Custom Rust Stack) -- Recommended Path

| Risk | Severity | Mitigation |
|---|---|---|
| State root correctness | Critical | Use reth-trie (battle-tested MPT), test against Ethereum test vectors |
| Consensus-execution bridge bugs | Critical | Extensive simulation testing, formal verification of safety properties |
| hotstuff_rs community size (58 stars) | High | Evaluate code quality directly; contribute upstream; prepare to fork if abandoned |
| No IBC support | Medium | Accept for v1; plan IBC module or bridge for v2 |
| Audit cost for novel stack | High | Budget 3-6 months, engage early with auditors |
| Hiring senior Rust blockchain engineers | High | Small talent pool; competitive compensation required |
| revm API churn (v19 to v37 in 18 months) | Medium | Pin exact version (as Hyperliquid does: `=19.2.0`); upgrade deliberately |
| libp2p-to-hotstuff_rs networking bridge | Medium | Start with hardcoded peers for devnet; GossipSub for transaction gossip |

### Path B (Sei v2 Fork) -- Alternative Path

| Risk | Severity | Mitigation |
|---|---|---|
| x/dex CosmWasm coupling | High | Decouple or replace with EVM precompile-based CLOB |
| Upstream diverging (SIP-3 EVM-only) | High | Freeze fork at v6.4.1; cherry-pick patches selectively |
| CometBFT performance ceiling | High | Accept 400ms for v1; evaluate Meter Supernova swap for v2 |
| Cosmos SDK v0.45 divergence | Medium | Backport critical security patches manually |

### Path A (Cosmos SDK + CometBFT) -- Fastest Path

| Risk | Severity | Mitigation |
|---|---|---|
| CometBFT cannot reach <200ms finality | Critical | Permanent; accept competitive disadvantage vs. Hyperliquid |
| cosmos/evm is pre-v1 | Medium | Track releases closely; budget for API migration |
| Sequential EVM execution | High | BlockSTM in v0.54 (late 2026) may help; not available now |

---

## 12. Recommendation

### Primary Recommendation: Path C -- Custom Rust Stack

**Build a custom chain using hotstuff_rs + revm + alloy + reth-trie + libp2p.**

This is the only path that achieves the stated goal of Hyperliquid-like performance. Paths A and B permanently cap performance below the target -- CometBFT's latency floor and sequential EVM execution are protocol-level constraints that cannot be optimized away.

The existence proof is strong: Hyperliquid built this exact architecture with ~5-6 engineers. Monad built a similar architecture with ~20-30 engineers. The component ecosystem is mature enough that the integration work is tractable.

### Phased Implementation

**Phase 1: EVM-only devnet (months 1-5)**
- hotstuff_rs consensus with 4 validators
- revm execution with RocksDB state + reth-trie MPT
- Basic eth_* JSON-RPC (MetaMask, Foundry compatible)
- No native order book yet
- Goal: Prove the consensus-execution bridge works

**Phase 2: Native execution layer (months 6-12)**
- Build Rust order book matching engine (HyperCore equivalent)
- Implement dual-VM pattern: native execution + EVM sidecar
- Wire precompiles: native state reads from EVM, CoreWriter pattern for EVM-to-native
- Port permanent staking and fee split logic as native Rust modules
- Goal: Feature parity with Torus economics design

**Phase 3: Production hardening (months 12-18)**
- Security audit
- Dynamic validator set management
- Slashing, jailing, epoch rotation
- Block explorer, indexer, wallet integration
- Testnet with external validators
- Goal: Mainnet-ready

### Consensus Design: MonadBFT-Inspired

Start with hotstuff_rs as the base library. Enhance with MonadBFT innovations:
1. **Tail-forking resistance** -- implement the reproposal + NEC mechanism from arxiv:2502.20692
2. **Speculative finality** -- execute blocks after 1 QC, commit after 2 QCs
3. **Active PaceMaker** -- Jolteon-style view synchronization with timeout certificates

### What to Port from torus-chain

The prior torus-chain economic designs are proven and should be reimplemented in Rust:
- **Permanent staking:** 5% inflationary rewards, 1.5x governance weight
- **4-way fee split:** burn / validator / treasury / dev-pool
- **Custom governance:** weighted voting with permanent stake multiplier

### Minimum Viable Team

- 2 senior Rust engineers (blockchain protocol): consensus-execution bridge, state management
- 1 senior Rust engineer (distributed systems): P2P networking, libp2p, transaction gossip
- 1 blockchain security researcher: ongoing review, audit preparation

3 engineers can reach devnet in 5 months. 2 engineers: 8-9 months.

### What NOT to Do

1. **Don't fork Aptos.** The license explicitly prohibits competing L1 development.
2. **Don't fork Monad.** GPL-3.0 forces your entire chain open-source.
3. **Don't build on CometBFT if targeting Hyperliquid-parity.** The 400ms-1s floor is structural.
4. **Don't use Ethermint.** It's dead. cosmos/evm v0.6 is the successor.
5. **Don't use SputnikVM.** revm has larger community, better precompile support, is what Hyperliquid uses.
6. **Don't try to extract Sui's Mysticeti or Aptos's consensus as libraries.** No stable APIs, license/architecture blockers.

---

## Appendix: Key References

### Academic Papers (Freely Available)
- HotStuff: BFT Consensus with Linearity and Responsiveness (PODC 2019)
- Jolteon and Ditto: Network-Adaptive Efficient Consensus (arxiv:2106.10362)
- HotStuff-2: Optimal Two-Phase Responsive BFT (eprint.iacr.org/2023/397)
- MonadBFT: Fast, Responsive, Fork-Resistant Consensus (arxiv:2502.20692)
- Mysticeti: Reaching the Limits of Latency with Uncertified DAGs (arxiv:2310.14821)
- Shoal++: High Throughput DAG BFT Can Be Fast (arxiv:2405.20488)
- Raptr: Prefix Consensus for Robust High-Performance BFT (arxiv:2504.18649)
- Zaptos: Towards Optimal Blockchain Latency (arxiv:2501.10612)
- Block-STM: Scaling Blockchain Execution (PPoPP 2023)

### Key Repositories
- hotstuff_rs: github.com/parallelchain-io/hotstuff_rs (Apache-2.0)
- revm: github.com/bluealloy/revm (MIT)
- alloy: github.com/alloy-rs/alloy (Apache-2.0/MIT)
- reth: github.com/paradigmxyz/reth (Apache-2.0/MIT)
- libp2p: github.com/libp2p/rust-libp2p (MIT)
- jsonrpsee: github.com/paritytech/jsonrpsee (MIT)
- hyper-evm-sync: github.com/hyperliquid-dex/hyper-evm-sync (reference architecture)
- category-labs/monad-bft: MonadBFT Rust reference (GPL-3.0, study only)

### Torus-hyperBFT Prior Research
- research/hyperliquid-deep-dive.md -- Comprehensive Hyperliquid architecture analysis
