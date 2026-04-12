# Hyperliquid Deep Dive: Architecture, Economics, and Feasibility Assessment

**Date:** 2026-04-12
**Purpose:** Research foundation for Torus-hyperBFT — a Hyperliquid-inspired chain with custom economic features (permanent staking, 4-way fee split, custom governance).

> **Methodology:** This report synthesizes findings from Hyperliquid's official documentation, GitHub repositories, third-party security analyses, reverse engineering research, and crypto media coverage. Every claim is source-cited. Items that could not be independently verified are explicitly flagged.

---

## Table of Contents

1. [Architecture](#1-architecture)
2. [Performance](#2-performance)
3. [Validator Model](#3-validator-model)
4. [Fee Model](#4-fee-model)
5. [DEX / Order Book](#5-dex--order-book)
6. [Token Economics](#6-token-economics)
7. [Open Source Status](#7-open-source-status)
8. [Known Limitations and Criticisms](#8-known-limitations-and-criticisms)
9. [Feasibility Assessment: Torus-hyperBFT](#9-feasibility-assessment-torus-hyperbft)

---

## 1. Architecture

### 1.1 High-Level Structure

Hyperliquid is a purpose-built L1 blockchain with three tightly integrated layers:

| Layer | Name | Role |
|---|---|---|
| Consensus | **HyperBFT** | Custom BFT consensus (HotStuff-inspired) |
| Native Execution | **HyperCore** | Rust-based financial state machine (order book, perps, spot, vaults, staking) |
| General Execution | **HyperEVM** | Cancun-spec EVM for arbitrary smart contracts |

All three layers run on the **same L1** — HyperEVM is not a separate chain. They share unified state secured by the same validator set.

Sources: [HyperCore Overview](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview), [HyperEVM Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperevm), [Zealynx Architecture Analysis](https://www.zealynx.io/blogs/Understanding-Hyperliquid-Architecture-HyperBFT-HyperCore-HyperEVM-Part1)

### 1.2 Consensus: HyperBFT

HyperBFT is a custom BFT protocol "heavily inspired by HotStuff and its successors." Key properties:

- **Two-phase pipelined commit** (reduced from HotStuff's original 3-phase, similar to DiemBFT/Fast HotStuff). Once a validator sees two consecutive certified blocks (round N and N+1), it commits block N-1 and all ancestors. Multiple blocks are simultaneously in proposal and attestation phases.
- **Leader-based proposal**: A rotating leader compiles a block; validators route through the leader. This achieves **O(n) communication complexity** vs classical PBFT's O(n^2).
- **Quorum Certificates (QC)**: Leader aggregates signed votes from >2/3 of stake-weighted validators into a single QC. Timeout Certificates (TC) handle view changes.
- **Optimistic responsiveness**: No fixed synchronous timer. Blocks are produced as fast as quorum can communicate — no waiting for timeouts under normal conditions.
- **Deterministic single-block finality**: Once committed, blocks are irreversible. No probabilistic confirmation windows, no reorg risk.
- **Byzantine Fault Tolerance**: Tolerates up to 1/3 malicious validators by stake (standard BFT: requires 2f+1 honest out of 3f+1).

Sources: [HyperBFT Wiki](https://hyperliquid-co.gitbook.io/wiki/architecture/hyperbft), [ASXN X Thread](https://x.com/asxn_r/status/1853871055208640904), [Gate.com L1 Deep Dives](https://www.gate.com/learn/course/l1-deep-dives-hyperliquid-hype/hyper-bft-consensus-mechanism), [Presto Labs Research](https://www.prestolabs.io/research/hyperliquid-the-hype-begins)

### 1.3 Native Execution: HyperCore ("RustVM")

HyperCore is the native financial state machine — a custom Rust execution environment purpose-built for trading. It is **not a general-purpose VM** and is not the EVM. It handles:

- Perpetuals order book and margin engine
- Spot order book
- On-chain CLOB (Central Limit Order Book) matching
- Liquidations and Auto-Deleveraging (ADL)
- Native oracle price feeds (validator-weighted median from external exchanges)
- Protocol vaults (HLP) and user vaults
- Staking and governance
- HIP-1/HIP-2 token mechanics

**All order matching is deterministic and occurs natively within the consensus execution loop.** There is no off-chain matching engine. The matching algorithm uses strict price-time priority.

**Oracle feed weights:** Binance (3), OKX (2), Bybit (2), Kraken/KuCoin/Gate/MEXC/Hyperliquid (1 each). Final oracle price = stake-weighted median across all validators' submissions.

Sources: [HyperCore Overview](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview), [Order Book Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/order-book), [rocknblock.io Deep Dive](https://rocknblock.io/blog/how-does-hyperliquid-work-a-technical-deep-dive)

### 1.4 HyperEVM

HyperEVM launched on mainnet **February 18, 2025**. It implements the **Cancun EVM specification (without EIP-4844 blobs)**. Standard Ethereum tooling works: Foundry, Hardhat, ethers.js, Solidity.

**Dual-block architecture:**

| Block Type | Period | Gas Limit | Use Case |
|---|---|---|---|
| Small (fast) | 1 second | 2M gas | Standard txs, DEX interactions |
| Large (slow) | 1 minute | 30M gas | Contract deployments, heavy computation |

**EIP-1559 is enabled. Both base fees and priority fees are burned** — validators are compensated by staking rewards, not EVM gas. HYPE (18 decimals) is the native gas token.

**L1 <-> EVM Interaction:**

| Direction | Mechanism | Address |
|---|---|---|
| EVM reads HyperCore | Read precompiles | `0x0000...0800` |
| EVM writes to HyperCore | CoreWriter system contract | `0x3333...3333` |
| Asset transfer | Lockbox precompiles | `0x2000...{token_index}` |

- Read precompiles can query: perps positions, spot balances, vault equity, staking delegations, oracle prices, L1 block number
- CoreWriter actions are **delayed on-chain by a few seconds** before execution (anti-MEV)
- Block processing order: L1 HyperCore executes first -> EVM block executes (using latest HyperCore state) -> EVM-to-Core transfers -> CoreWriter actions. EVM contracts always read state one HyperCore block old.
- No wrapped tokens or bridge IOUs between layers — HIP-1 tokens map directly to ERC20s via lockbox precompiles

Sources: [HyperEVM Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperevm), [HyperEVM Wiki](https://hyperliquid-co.gitbook.io/wiki/architecture/hyperevm), [Interacting with HyperCore](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/interacting-with-hypercore), [Dual-block Architecture](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/dual-block-architecture), [LayerZero Concepts](https://docs.layerzero.network/v2/developers/hyperliquid/hyperliquid-concepts)

### 1.5 State Model and Storage

- **Serialization**: MessagePack (`.rmp` via Rust `rmp-serde`). State snapshots every 10,000 blocks to `~/hl/data/periodic_abci_states/{date}/{height}.rmp`
- **Database**: RocksDB for consensus state (confirmed by third-party reverse engineering of `hl-node` binary, not officially documented)
- **Core state object** (`VisorAbciState`): governance state (89 `VoteGlobalAction` variants), user ledger (positions, balances, margin), market data (order books, oracles, liquidation queues), bridge state (bucketed BTreeMaps)
- **Transaction data**: Streams to `~/hl/data/replica_cmds/` as line-delimited JSON. ~100 GB of logs per day at current activity levels.
- **~70+ user-initiated action types** across trading, transfers, vaults, governance, bridge, staking

Sources: [hyperliquid-dex/node README](https://github.com/hyperliquid-dex/node/blob/main/README.md), [L1 Data Schemas](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/nodes/l1-data-schemas), [Reverse Engineering Hyperliquid (can.ac)](https://blog.can.ac/2025/12/20/reverse-engineering-hyperliquid/)

### 1.6 Node Architecture

- **Language**: Rust (x86-64 ELF binary, confirmed by disassembly of `hl-node-v76`, 72.4 MiB)
- **Two-process system**: `hl-visor` (supervisor, GPG verification, upgrade management) spawns `hl-node` (consensus/execution)
- **Distribution**: Closed-source binaries from `binaries.hyperliquid.xyz`, GPG-signed. Visor refuses to launch unsigned binaries.
- **Networking**: Ports 4000-4010 for validator gossip. Up to 2 sentry nodes per validator. Non-validators discover peers via seed list.

Sources: [hyperliquid-dex/node](https://github.com/hyperliquid-dex/node), [Reverse Engineering Hyperliquid](https://blog.can.ac/2025/12/20/reverse-engineering-hyperliquid/)

---

## 2. Performance

### 2.1 Throughput

| Metric | Value | Notes |
|---|---|---|
| Current HyperCore throughput | ~200,000 orders/sec | Execution-bound; consensus can do more |
| Theoretical max (consensus only) | >1,000,000 orders/sec | If execution optimized |
| Team-claimed theoretical max | ~2,000,000 orders/sec | ~100x Tendermint; unverified |

**Important caveat:** The 200k figure is "orders per second" (inclusive of cancels, modifications, liquidations), not Ethereum-style TPS. This number is from Hyperliquid's own documentation and has **not been independently verified** by external benchmarks.

The team acknowledges execution is the current bottleneck, not consensus or networking.

### 2.2 Latency

| Metric | Value | Source |
|---|---|---|
| Median consensus block production | ~0.07s (70ms) | [ASXN](https://x.com/asxn_r/status/1853871055208640904) |
| Median end-to-end order latency | 0.2s | [Official Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview) |
| 99th percentile consensus finality | <0.5s | [HyperBFT Wiki](https://hyperliquid-co.gitbook.io/wiki/architecture/hyperbft) |
| 99th percentile end-to-end latency | 0.9s | [Official Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview) |

The 0.07s is consensus-layer block production under optimistic responsiveness. The 0.2s is the full round trip from external client to confirmation (adds network hop to Tokyo validators).

### 2.3 Block Time

- **HyperCore (L1)**: Variable, driven by optimistic responsiveness. No fixed block time — as fast as quorum responds.
- **HyperEVM small blocks**: 1 second
- **HyperEVM large blocks**: 1 minute
- **Epochs**: 100,000 consensus rounds (~90 minutes). Validator set and stakes are static within an epoch.

### 2.4 Comparison to Other Chains

| Chain | Finality | Throughput | Notes |
|---|---|---|---|
| Hyperliquid | ~0.2s median | ~200k orders/sec | Application-specific L1 |
| Solana | ~0.4s | ~4,000 TPS (theoretical 65k) | General-purpose |
| Sei v2 | ~0.5s | ~12,500 TPS (theoretical) | Built-in order book |
| dYdX v4 | ~1s | ~10,000 orders/sec | Off-chain matching |
| Ethereum | ~12min (finality) | ~15-30 TPS | General-purpose |

Sources: [HyperCore Overview](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview), [HyperBFT Wiki](https://hyperliquid-co.gitbook.io/wiki/architecture/hyperbft), [Presto Labs](https://www.prestolabs.io/research/hyperliquid-the-hype-begins)

---

## 3. Validator Model

### 3.1 Validator Set

- **Active set**: **21 validators**, determined by top 21 accounts by total staked HYPE
- **Permissionless since April 21, 2025** (previously invitation-based, operated by Hyperliquid Labs)
- **History**: 4 validators at launch (Nov 2024) -> expanded to ~20-24 through early 2025 -> fixed at 21 permissionless
- Set updated at **epoch boundaries** (every 100k rounds, ~90 minutes)
- Expected to increase over time (no specific timeline published)

Known initial validators: HypurrCollective x Nansen, Hypurrscanning, Imperator, B-Harvest, ValiDAO, Meria, Enigma, USDT0 x Luganodes.

Sources: [Hyper Foundation Medium](https://hyperfnd.medium.com/hyperliquids-permissionless-validator-network-secured-by-the-community-ad0057cfad71), [FXStreet](https://www.fxstreet.com/cryptocurrencies/news/hyperliquid-updates-validator-to-21-permissionless-nodes-hype-price-breaks-out-202504220700)

### 3.2 Staking (DPoS)

Hyperliquid uses **Delegated Proof-of-Stake**:

| Parameter | Value |
|---|---|
| Minimum self-delegation (validator) | 10,000 HYPE (locked 1 year even if never active) |
| Delegator minimum | None |
| Commission rates | Validator-set, typically 1-5%. Capped at +1% per change. |
| Delegation lockup | 1 day per delegation event |
| Unstaking queue | 7-day wait after undelegating. Max 5 pending withdrawals per address. |
| Reward frequency | Accrues every minute, distributed daily |
| Auto-compounding | Yes (re-delegated to staked validator) |
| Reward formula | Inversely proportional to sqrt(total HYPE staked) |
| Current APY | ~2.1-2.4% (at ~400M HYPE staked) |

The **Hyper Foundation Delegation Program** distributes foundation HYPE to technically strong validators to support decentralization.

Sources: [Staking Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/staking), [StakingRewards](https://www.stakingrewards.com/asset/hyperliquid), [Imperator Guide](https://www.imperator.co/resources/blog/guide-hyperliquid-validators)

### 3.3 Jailing

- Any active validator can cast a **jail vote** against an underperforming peer
- When a **quorum of jail votes** (stake-weighted) is reached, the peer is jailed
- Jailed validators: stop consensus participation, stop earning rewards, can still forward messages
- **Unjailing**: `unjailSelf` action, succeeds after L1 timestamp exceeds "jailed until" time. Rate-limited.
- **Latency target**: 200ms two-way to at least 1/3 of validators by stake

### 3.4 Slashing

**Slashing is NOT currently implemented.** The docs state: "There is currently no automatic slashing implemented."

Defined slashable offense: **double-signing blocks at the same round**. When implemented, penalties would be determined by stake-weighted median of validator votes. No penalty amounts specified. The JELLY governance vote demonstrated validators can coordinate for consequential decisions.

### 3.5 Hardware Requirements

| Node Type | vCPUs | RAM | Storage | OS |
|---|---|---|---|---|
| Validator | 32 | 128 GB | 1 TB SSD | Ubuntu 24.04 |
| Non-validator | 16 | 64 GB | 500 GB SSD | Ubuntu 24.04 |

Ports 4001/4002 must be public. **Tokyo, Japan recommended** for optimal latency. Sentry nodes recommended to shield validators.

Sources: [Running a Validator](https://hyperliquid.gitbook.io/hyperliquid-docs/validators/running-a-validator), [hyperliquid-dex/node](https://github.com/hyperliquid-dex/node)

---

## 4. Fee Model

### 4.1 Gas Fees

**Zero gas fees on all native HyperCore operations.** Order placements, cancellations, settlements cost nothing. Deposits are free. Withdrawals have a flat **1 USDC fee**.

On HyperEVM, EIP-1559 gas applies but **both base and priority fees are burned** (not paid to validators).

### 4.2 Trading Fees (Perpetuals)

Volume tiers use: `14d weighted volume = (14d perps volume) + 2x(14d spot volume)`

| Tier | 14d Volume | Taker | Maker |
|---|---|---|---|
| 0 (Base) | - | 0.045% | 0.015% |
| 1 | >$5M | Lower | Lower |
| ... | ... | ... | ... |
| 6 (Top) | >$7B | 0.024% | 0.000% |

### 4.3 Trading Fees (Spot)

| Tier | Taker | Maker |
|---|---|---|
| 0 (Base) | 0.070% | 0.040% |
| 6 (Top) | 0.025% | 0.000% |

### 4.4 HYPE Staking Fee Discounts

Staking HYPE grants additional fee reductions stacked on volume tiers:

| Tier | HYPE Staked | Discount |
|---|---|---|
| Wood | >10 | 5% |
| Bronze | >100 | 10% |
| Silver | >1,000 | 15% |
| Gold | >10,000 | 20% |
| Platinum | >100,000 | 30% |
| Diamond | >500,000 | 40% |

### 4.5 Additional Fee Modifiers

- **Aligned quote asset pairs**: 20% lower taker, 50% better maker rebates
- **Stable pairs (spot)**: 80% lower taker, maker rebates
- **HIP-3 Growth Mode**: Fees/rebates/volume/rate limits reduced 90% for new markets
- **Maker rebates**: Active makers providing >0.5%/1.5%/3.0% of total maker volume earn -0.001%/-0.002%/-0.003% rebates
- **Referral discounts**: On first $25M volume of referred users

### 4.6 Where Fees Go

**No fees go to validators directly as block rewards.** Fees flow three ways:

1. **HLP (Hyperliquidity Provider vault)**: Protocol's automated market maker. Fees distributed proportional to HYPE deposited.
2. **Assistance Fund**: Auto-converts trading fees into HYPE purchases -> **permanently burned**. ~$1B burned via governance vote Dec 2025.
3. **Deployers**: Creators of spot tokens and HIP-3 perp markets retain **up to 50%** of trading fees generated by their assets.

**Fee revenue scale** (late 2025/early 2026):
- Annualized: ~$1 billion/year
- 30-day: ~$82 million (perps ~$62.6M, spot ~$1.9M, L1 gas ~$549K, HLP returns ~$651K)

Sources: [Fees Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees), [Protocol Vaults](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/vaults/protocol-vaults), [DeFiLlama](https://defillama.com/protocol/hyperliquid-perps), [Tokenomics.com](https://tokenomics.com/articles/hyperliquid-tokenomics-how-hype-captures-65m-monthly-in-holder-revenue)

### 4.7 Fee Comparison

| Exchange | Base Taker (Perps) | Base Maker (Perps) | Gas |
|---|---|---|---|
| **Hyperliquid** | **0.045%** | **0.015%** | **Zero** |
| Binance Futures | 0.040% | 0.020% | Zero |
| OKX Futures | 0.050% | 0.020% | Zero |
| dYdX v4 | 0.050% | 0.020% | Cosmos gas |
| GMX v2 | 0.050-0.070% | N/A (AMM) | EVM gas |

Hyperliquid's maker fee undercuts Binance. Spot fees (0.070%/0.040%) are materially lower than Binance/OKX (0.100%/0.100%).

Sources: [Datawallet](https://www.datawallet.com/crypto/hyperliquid-vs-binance), [21shares](https://www.21shares.com/en-eu/insights/the-perpetual-dex-wars-hyperliquid-aster-and-lighter-in-focus)

---

## 5. DEX / Order Book

### 5.1 On-Chain CLOB Architecture

The order book is a **fully on-chain Central Limit Order Book** embedded directly into HyperBFT consensus — not a smart contract, not off-chain. Every order placement, cancellation, match, trade, and liquidation is recorded in consensus state with single-block finality.

This is the critical architectural distinction from:
- **dYdX v4**: Off-chain order book, on-chain settlement
- **EVM DEXes**: Matching via gas-charged contract calls
- **AMMs (Uniswap, etc.)**: Algorithmic pricing, no order book

On Hyperliquid, matching is a native protocol operation with **no gas overhead per order**.

**Matching rules:**
- Strict price-time priority
- Orders at integer multiples of tick size and lot size
- Double margin check: at order submission AND at matching time
- Mempool priority: (1) non-GTC/IOC actions, (2) cancellations, (3) GTC/IOC orders

### 5.2 Order Types

| Type | Description |
|---|---|
| Market | Immediate execution at current price |
| Limit | At specified price or better |
| Stop Market/Limit | Activates at trigger price |
| Take Market/Limit | Triggered when price hits a level |
| Scale | Multiple limits spread across a price range |
| TWAP | Splits into chunks at 30-second intervals; max 3% slippage per chunk |

Modifiers: Reduce Only, GTC, Post Only (ALO), IOC, Take Profit, Stop Loss.

### 5.3 Perpetual Futures

- **USDC-margined, USDT-denominated linear contracts** (1 contract = 1 unit of underlying)
- **No expiration** — continuous funding mechanism
- **Margin modes**: Cross (shared collateral), Isolated, Strict Isolated
- **Leverage**: User-settable. BTC max 40x, ETH max 25x (reduced after March 2025 incident), others up to 50x. Large positions (>$10M notional) get auto-reduced effective leverage via margin tiers.
- **Funding**: Hourly (1/8th of 8-hour rate). Formula: `F = Average Premium Index + clamp(Interest Rate - P, -0.0005, 0.0005)`. Interest rate fixed at 0.01% per 8h. Cap: 4% per hour.
- **Liquidation**: Continuous monitoring. Cross: account value < maintenance margin x notional. ADL as last resort (force-closes profitable traders on opposite side).
- **Hyperps**: Pre-launch perps on assets with no external oracle — uses 8-hour exponentially weighted moving average. Converts to standard perp when underlying lists on major CEXes.

### 5.4 Spot Trading

- Same CLOB infrastructure, separate spot clearinghouse
- Two account balances: spot (tokens) and perp (USDC collateral)
- Portfolio margin (pre-alpha): Unifies spot and perp into single margin balance
- USDC freely transferable between spot and perp balances

### 5.5 Vaults

**HLP (Protocol Vault):**
- Community-owned vault performing: market making, liquidation backstop, earn supply
- Revenue: taker fees, funding rate collection, spread capture, liquidation proceeds
- 4-day withdrawal lock
- Divided into sub-pools; only one sub-pool per liquidation event

**User (Strategy) Vaults:**
- Custom strategies (directional, MM, arb)
- Leader must maintain minimum 5% ownership (skin-in-the-game)
- Leader earns 10% profit share (nothing on losses)
- 24-hour withdrawal lock

**HyperEVM Vaults (Newer):**
- EIP-4626 compatible, CoreWriter integration
- Supports HIP-3 markets and spot trading

Sources: [Order Book](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/order-book), [Order Types](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/order-types), [Margining](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margining), [Funding](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/funding), [Auto-Deleveraging](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/auto-deleveraging), [Hyperps](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/hyperps), [Protocol Vaults](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/vaults/protocol-vaults)

---

## 6. Token Economics

### 6.1 HYPE Token Distribution

**Fixed supply: 1,000,000,000 HYPE (1 billion). No inflation beyond pre-defined emissions.**

| Category | % | Amount | Notes |
|---|---|---|---|
| Future Emissions & Community Rewards | 38.89% | ~388.9M | Unannounced schedule |
| Genesis Distribution (airdrop) | 31.00% | 310M | Nov 29, 2024. >90k recipients. No lockup. |
| Core Contributors | 23.80% | 238M | 1-year cliff + 24-month linear vesting |
| Hyper Foundation Budget | 6.00% | 60M | Delegation program, ecosystem |
| Community Grants | 0.30% | 3M | |
| HIP-2: Hyperliquidity | 0.01% | 100K | |

**Zero VC allocation** — explicit design decision distinguishing Hyperliquid from most launches.

**Core contributor unlocks:** First: 9.92M HYPE on Nov 29, 2025 (cliff). Then ~1.2M monthly through ~late 2027.

**Circulating supply** (April 2026): ~238M HYPE (23.84% of total). Released (incl. locked): ~425M (42.52%).

**Assistance Fund Burn (Dec 2025):** ~37M HYPE (~$1B) permanently burned via validator governance vote. 85% yes, 7% no, 8% abstain. ~14-16% of circulating supply removed. Ongoing burns continue from fee conversion.

Sources: [Tokenomist.ai](https://tokenomist.ai/hyperliquid), [DropsTab](https://dropstab.com/coins/hyperliquid/vesting), [CoinEdition](https://coinedition.com/hyperliquid-to-distribute-310m-hype-tokens-in-genesis-event-airdrop/), [The Defiant](https://thedefiant.io/news/tokens/hyperliquid-proposes-burning-13-percent-of-circulating-token-supply)

### 6.2 HIP-1: Native Token Standard

HIP-1 is Hyperliquid's **protocol-level fungible token standard** — not a smart contract standard like ERC-20. Tokens are defined at L1 with an **automatically provisioned CLOB** (paired with USDC) created at deployment.

Key parameters: name (<=6 chars), weiDecimals, szDecimals, maxSupply (hard cap, immutable), initialWei (genesis allocations), anchorTokenWei (proportional distribution to existing holders).

**Deployment auction:** 31-hour Dutch auction priced in HYPE. Initial price: 2x last winning price or 500 HYPE. Price decreases linearly to 500 HYPE floor. One buyer per window.

Key difference from ERC-20: no DEX deployment needed (auto-provisioned CLOB), fees redirect to deployer by default, supply only decreases (burns/fees), automatic dust conversion.

### 6.3 HIP-2: Hyperliquidity

Protocol-native automated liquidity mechanism built into HyperCore's block transition logic. Not an AMM, not a smart contract, no operators.

- Price grid: Orders spaced at 0.3% intervals (`px_i = round(px_{i-1} * 1.003)`)
- Refreshes every block (~3s minimum)
- Deployer-funded liquidity is **permanently locked** — cannot be rug-pulled
- As asks fill (tokens -> USDC), mechanism places corresponding bids

### 6.4 HIP-3: Builder-Deployed Perpetual Markets

Activated mainnet **October 13, 2025**. Allows anyone to deploy their own perp DEX on HyperCore inheriting the full matching/margining/liquidation infrastructure.

- **Staking bond**: 500,000 HYPE (~$25M)
- **Fee split**: Deployers earn 50% of trading fees
- **Slash mechanism**: Validators can burn up to 100% of deployer stake
- **Notable deployment**: Trade.xyz listed 24/7 perps for TSLA, AAPL, NVDA, AMZN, synthetic Nasdaq. HIP-3 markets reached $1.43B open interest by early 2026.

### 6.5 HIP-4: Prediction Markets

Announced Feb 2026. Testnet launched Feb 2, 2026. Mainnet expected ~June 2026.

- Fully collateralized binary contracts settling at 0 or 1
- Builder staking: 1,000,000 HYPE per market slot
- Natively on HyperCore alongside perps

Sources: [HIP-1](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-1-native-token-standard), [HIP-2](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-2-hyperliquidity), [CoinGecko HIP-3/4](https://www.coingecko.com/learn/hyperliquid-hip3-hip4-tokenized-stocks-and-prediction-markets), [The Block HIP-3 OI](https://www.theblock.co/post/393810/hyperliquid-hip-3-markets-1-43-billion-open-interest-24-7-trading-tokenized-equities-commodities)

---

## 7. Open Source Status

### 7.1 What Is Closed Source

| Component | Status |
|---|---|
| Node binary (`hl-node`) | **Closed source** — distributed as GPG-signed compiled binary |
| Consensus engine (HyperBFT) | **Closed source** |
| Matching engine (HyperCore) | **Closed source** |
| Internal EVM client | **Undisclosed** (no confirmation of reth or any specific base) |

Team statement: "The node code is currently closed source. Open sourcing is important. Projects open source once development is in a stable state." No timeline given.

**Validators run opaque Docker containers** with no visibility into the code they execute.

Sources: [CoinDesk Jan 2025](https://www.coindesk.com/business/2025/01/08/hyper-liquid-responds-to-scrutiny-over-lack-of-decentralization-hype-slumps-15), [valardragon X post](https://x.com/valardragon/status/1810662137523527969)

### 7.2 What Is Open Source

The `hyperliquid-dex` GitHub org has 11 repositories:

| Repository | License | Description |
|---|---|---|
| `hyperliquid-python-sdk` | MIT | Official Python trading SDK |
| `node` | Apache 2.0 | Dockerfile/node infra (NOT the binary) |
| `hyperliquid-rust-sdk` | MIT | Rust SDK |
| `contracts` | - | Bridge/smart contract code (Arbitrum) |
| `order_book_server` | - | Order book infrastructure |
| `hyper-evm-sync` | - | EVM sync tooling |
| `block-importer` | - | HyperEVM block data importer |
| `hyperliquid-stats-web` | MIT | Statistics explorer |
| `ts-examples` | - | TypeScript examples |
| `hyperliquid-stats` | - | Statistics tools |
| `historical_data` | - | Historical data |

Source: [hyperliquid-dex GitHub](https://github.com/hyperliquid-dex)

### 7.3 Public APIs

- **Info API** (REST): `POST https://api.hyperliquid.xyz/info` — no auth for reads
- **Exchange API** (REST): `POST https://api.hyperliquid.xyz/exchange` — wallet signing required
- **WebSocket**: `wss://api.hyperliquid.xyz/ws` — real-time streaming
- **HyperEVM RPC**: `https://rpc.hyperliquid.xyz/evm` — standard Ethereum JSON-RPC
- **Rate limits**: 1,200 weight/min (REST), 100 req/min (EVM RPC)
- Third-party RPC: QuickNode, Chainstack, Alchemy, Dwellir
- CCXT integration available

### 7.4 Forks and Clones

**No meaningful fork exists.** Core is closed source, tight integration is application-specific, and GitHub forks of the `node` repo only contain Dockerfile/docs. Competitors (Drift, Vertex, AsterDex, Lighter, GRVT) pursue similar goals but with different architectures.

Sources: [Hyperliquid API Docs](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api), [Rate Limits](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/rate-limits-and-user-limits)

---

## 8. Known Limitations and Criticisms

### 8.1 Centralization Concerns

**Bridge:**
- Arbitrum bridge contract does **not use ZK proofs** — not trustless
- **Only 4 team-controlled Ethereum addresses** have approved every withdrawal (~200k invocations each)
- **200-second dispute period** (vs Arbitrum's native 1-week challenge period)
- Team can call `invalidateWithdrawals()` unilaterally
- Admin functions (`changeDisputePeriodSeconds`, `modifyFinalizer`) are fully team-controlled
- `AdminUpgradeabilityProxy` pattern — foundation can swap implementation
- L1 validator set and bridge validator set **diverged in April 2025** without announcement (ChainArgos verified: `RequestedValidatorSetUpdate` / `FinalizedValidatorSetUpdate` events never emitted)
- ChainArgos classification: **"Stage 0" decentralization** — "a custodial multisig with some unnecessary code around the edges"

**Validator set:**
- Only 21 validators (vs Ethereum's 800k+)
- Pre-April 2025: 4 validators, all team-controlled
- Validators run closed-source binaries in Docker containers

**Infrastructure:**
- All infrastructure reportedly runs in **Tokyo, Japan** on AWS/Azure (ChainArgos analysis)

Sources: [ChainArgos/DataFinnovation](https://medium.com/chainargos/centralized-control-in-hyperliquid-3e9f7dd0d706), [AuditOne](https://www.auditone.io/blog-posts/hyperliquid-a-comprehensive-look-at-innovation-growth-and-security-challenges-in-defi), [CoinDesk](https://www.coindesk.com/tech/2025/01/08/the-protocol-hyperliquid-responds-to-decentralization-criticism)

### 8.2 Security Incidents

| Date | Incident | Impact | Type |
|---|---|---|---|
| Dec 2024 | North Korean wallet activity | $256M outflows, 25% HYPE drop | Unconfirmed reconnaissance |
| Mar 12, 2025 | ETH whale suicide liquidation | ~$4M loss to HLP | Market manipulation |
| Mar 26, 2025 | **JELLY manipulation** | $13.5M unrealized HLP loss | Coordinated manipulation |
| Mar 2025 | FARTCOIN manipulation | ~$1.5M loss to HLP | ADL exploitation |
| Sep 2025 | Hyperdrive protocol exploit | $782K | Third-party smart contract bug |
| Oct 2025 | $21M private key compromise | $21M stolen | User key management failure |

**JELLY incident (most significant):** Three coordinated wallets created a $4.1M JELLY short, pumped JELLY 400% externally, forcing HLP to inherit the short. **Validators voted unanimously within 2 minutes to delist JELLY** and settle all positions at $0.0095 (original entry, not $0.50 market price). The attacker had already withdrawn $6.26M of $7.17M deposit. This directly demonstrated centralized intervention capability.

Post-JELLY: Protocol upgraded to include **on-chain validator voting for asset delisting**, minimum collateral enforcement, and open interest limits.

Sources: [Halborn JELLY Analysis](https://www.halborn.com/blog/post/explained-the-hyperliquid-hack-march-2025), [OAK Research](https://oakresearch.io/en/analyses/investigations/hyperliquid-jelly-attack-context-vulnerability-team-solution), [CoinDesk](https://www.coindesk.com/markets/2025/03/26/hyperliquid-delists-jellyjelly-after-vault-squeezed-in-usd13m-tussle)

### 8.3 HLP Structural Risk

HLP acts as the backstop liquidity provider and inherits un-liquidatable positions. This creates structural exposure:
- LPs absorb losses from failed liquidations or manipulation
- ADL can force profitable traders off positions
- Traders could withdraw collateral mid-position for intentional "suicide liquidations" (partially patched)
- HLP outflows surged post-JELLY as confidence dropped

Source: [WisdomTree Prime](https://www.wisdomtreeprime.com/blog/the-great-whale-slap-how-a-whale-offloaded-4m-in-losses-to-hyperliquids-hlp-vault/)

### 8.4 Community Criticisms

- **Bitget CEO (Gracy Chen)**: Called Hyperliquid "an offshore CEX with no KYC/AML" and "on track to become FTX 2.0" (competitive motive noted)
- **Arrington Capital**: Open letter demanding: delist low-liquidity assets, prohibit withdrawal of unrealized profits, implement OI caps (they invest in competing projects)
- **No KYC/AML**: Potential liability under BSA/FinCEN regulations
- **Token unlock pressure**: Only ~17% of monthly core contributor unlocks absorbed by buybacks (~$415M/month potentially sellable)
- **Insider trading**: Former employee dismissed Q1 2024 for insider trading (confirmed by Hyperliquid)

### 8.5 Regulatory

- CFTC engagement (May 2025): Hyperliquid Labs filed comment letters supporting CFTC perpetuals framework
- **Hyperliquid Policy Center** (Feb 2026): 1M HYPE (~$29M) funding a DC nonprofit lobbying group, headed by Jake Chervinsky
- Unresolved: Perps legal gray area in US, no KYC, $10.3B in liquidations in 2025 drew regulatory attention

Sources: [Bitget](https://www.bitget.com/news/detail/12560604666736), [Arrington Capital](https://www.arringtoncapital.com/blog/hyperliquids-tipping-point-three-things-they-must-do-now/), [CoinDesk CFTC](https://www.coindesk.com/markets/2025/05/23/cftcs-plans-for-crypto-perpetual-trading-puts-focus-on-hyperliquids-hype), [CoinDesk Policy Center](https://www.coindesk.com/policy/2026/02/18/hyperliquid-starts-defi-lobbying-group-with-usd29-million-token-backing)

### 8.6 Verified vs. Speculative Claims

| Claim | Status |
|---|---|
| Node/matching engine is closed source | **VERIFIED** (team confirmed) |
| SDKs and bridge contracts are open source | **VERIFIED** (GitHub) |
| Bridge controlled by 4 team wallets | **VERIFIED** (on-chain, ChainArgos) |
| 200-second bridge dispute period | **VERIFIED** (on-chain) |
| JELLY: validators intervened in 2 minutes | **VERIFIED** (multiple sources) |
| DPRK wallets exploited Hyperliquid | **UNCONFIRMED** (team denied; reconnaissance plausible) |
| "FTX 2.0" | **OPINION** (Gracy Chen, contested) |
| Infrastructure on AWS Tokyo | **ASSESSED** (ChainArgos IP analysis, not definitively confirmed) |
| L1 and bridge validator sets diverged | **VERIFIED** (no on-chain events emitted) |
| 200k orders/sec throughput | **SELF-REPORTED** (not independently benchmarked) |
| RocksDB for state storage | **INFERRED** (reverse engineering, not team-confirmed) |

---

## 9. Feasibility Assessment: Torus-hyperBFT

### 9.1 Context from Prior Work

From memory (sessions 98-99 and existing torus-chain project):
- **Hyperliquid is closed source and cannot be forked**
- An existing Torus chain was built on a **Cosmos SDK + CometBFT** base, later recommended to shift to **Sei v2 fork** for built-in order book support
- A working `x/fees` module already implements 4-way fee split
- Economics spec exists with permanent staking (5% APY, 1.5x vote weight)
- Extensive code reviews and devnet testing completed on prior torus-chain

### 9.2 What Hyperliquid Does That We Want

| Feature | Hyperliquid Approach | Torus-hyperBFT Target |
|---|---|---|
| Fast BFT consensus | Custom HyperBFT (HotStuff-inspired) | CometBFT or custom (see below) |
| On-chain order book | Native CLOB in execution layer | Sei v2's built-in order book module |
| EVM compatibility | HyperEVM (Cancun spec) | Ethermint or Sei v2 EVM |
| Sub-second finality | ~0.2s median | Target <1s |
| Zero gas (trading) | Native protocol operations | Possible in custom module |

### 9.3 Custom Economic Features (What Hyperliquid Doesn't Have)

**A. Permanent Staking (1.5x governance vote weight + 5% APY)**

Hyperliquid has NO equivalent. Its staking is standard DPoS with ~2.1-2.4% APY and 7-day unstaking. Torus permanent staking adds:

- **Lock forever** mechanism: A `MsgPermanentStake` that burns the unstaking capability for an address's staked TRS
- **Inflationary 5% yearly rewards**: On permanently staked coins, does NOT auto-compound. E.g., 10M TRS permanently staked -> 500K TRS/year minted to permanent stakers.
- **1.5x governance vote weight**: Permanent stakers get 50% bonus voting power
- **Separate from validators**: Any address can permanently lock coins, not just validator operators
- **Implementation path**: Custom `x/permstake` Cosmos SDK module. Existing torus-chain already has this designed.
- **Key difference from Hyperliquid**: Hyperliquid's fixed 1B supply with no inflation vs. Torus's deliberate inflationary reward for permanent commitment.

**B. 4-Way Fee Split**

Hyperliquid splits fees 3 ways (HLP vault, Assistance Fund/burn, deployers). Torus needs:

| Recipient | Mechanism |
|---|---|
| Burn | Deflationary pressure |
| Validator/Proposer | Direct block reward incentive |
| Treasury | On-chain community pool |
| Developer Pool | Gas-usage-based revenue sharing for contract deployers |

- **Implementation path**: Custom `x/fees` module (already exists in torus-chain). `CalculateSplit` with configurable ratios via governance.
- **Developer pool**: Track gas usage per contract deployer address, distribute proportionally. Similar to Hyperliquid's deployer fee share but generalized to all contracts via gas metering rather than limited to HIP token deployers.

**C. Custom Governance (Perm-Staker Vote Weight Bonus)**

Hyperliquid governance is minimal — validator-only voting on specific actions (delisting, slashing). Torus needs:

- Standard Cosmos SDK governance (`x/gov`) extended with:
  - 1.5x vote weight multiplier for permanently staked TRS
  - Proposal types: parameter changes, treasury spends, dev pool allocation, emergency actions
- **Implementation path**: Override `x/gov` tally logic to query `x/permstake` for permanent stake status and apply multiplier.

### 9.4 Recommended Architecture

Based on Hyperliquid's lessons and prior Torus work:

**Base: Sei v2 fork** (recommended in session 99, still the best fit)

| Layer | Choice | Rationale |
|---|---|---|
| Consensus | CometBFT (Sei v2's default) | Proven, open source, <1s finality with 21 validators. Not as fast as HyperBFT but achievable and auditable. |
| Execution (native) | Sei v2's built-in order book + custom modules | Sei has native CLOB, parallel execution, price oracle module. Closest open-source analog to HyperCore. |
| EVM | Sei v2's EVM module | EVM compatibility built in, Cancun-compatible. Similar to Ethermint but integrated. |
| Custom modules | `x/permstake`, `x/fees`, `x/devpool`, extended `x/gov` | Cosmos SDK module pattern. Already partially built. |
| Bridge | IBC (native) + Gravity Bridge or Axelar for Ethereum | IBC gives free interop with Cosmos chains. Avoid Hyperliquid's bridge centralization problems. |

**Why not build from scratch like Hyperliquid:**
- Hyperliquid's performance comes from a purpose-built closed-source Rust engine. Replicating this would require 2-3 years of engineering.
- Sei v2 gives ~80% of the performance benefit at ~10% of the development cost.
- Open-source base means auditable, forkable, and community-verifiable — directly addressing Hyperliquid's biggest criticism.

### 9.5 Key Engineering Tasks

1. **Fork Sei v2** and strip/customize to Torus requirements
2. **Implement `x/permstake`**: Permanent locking, inflationary reward minting (5%/year), integration with staking and governance weight
3. **Implement/port `x/fees`**: 4-way split with governance-configurable ratios (already exists from prior work)
4. **Implement `x/devpool`**: Gas metering per contract deployer, periodic distribution from dev pool
5. **Extend `x/gov`**: Query permanent stake status, apply 1.5x vote weight multiplier in tally
6. **Configure validator set**: 21 validators, DPoS, jailing (can mirror Hyperliquid's approach but with open-source code)
7. **Bridge**: IBC setup + EVM bridge (consider CCTP for USDC)
8. **Tokenomics**: Define TRS distribution, emission schedule, burn rates, treasury parameters
9. **Testing**: Devnet, stress testing, security audits

### 9.6 Risks and Tradeoffs

| Risk | Mitigation |
|---|---|
| Performance gap vs Hyperliquid | Sei v2 is fast enough for most use cases (~12.5k TPS). Can optimize later. |
| Sei v2 upstream changes | Maintain as a managed fork with clear divergence points. |
| Permanent staking game theory | 5% inflationary reward creates sell pressure. Model carefully — if too many permanently stake, inflation compounds. Cap analysis needed. |
| Developer pool gaming | Gas metering can be gamed (contract calls itself). Need anti-abuse measures. |
| Bridge security | Use battle-tested IBC + established bridge protocols. Don't build custom bridge. |
| Regulatory | Same concerns as Hyperliquid if offering perps. Consider jurisdiction and KYC/AML strategy early. |

### 9.7 What Hyperliquid Got Right (Lessons to Adopt)

1. **Application-specific L1** beats general-purpose for trading — build the order book into consensus, don't run it as a smart contract
2. **Zero gas for native operations** dramatically improves UX — cover validator costs via staking rewards
3. **Deterministic single-block finality** is essential for trading
4. **Native token standard with auto-provisioned order book** (HIP-1/HIP-2 model) is elegant — consider similar for TRS ecosystem
5. **Fee structure competitive with CEXes** — critical for adoption

### 9.8 What Hyperliquid Got Wrong (Lessons to Avoid)

1. **Closed source** — biggest criticism, prevents community verification and forking. Torus should be open source from day one.
2. **Centralized bridge** — 4 team wallets controlling all withdrawals is a single point of failure. Use IBC + established bridges.
3. **Small, centralized validator set** — 21 is fine for performance, but ensure geographic distribution and avoid team-controlled majority.
4. **No slashing** — creates weak security guarantees. Implement slashing from launch.
5. **HLP socialized loss model** — backstop vault inheriting toxic positions is structurally fragile. Design liquidation mechanism carefully.
6. **No KYC/AML** — regulatory risk. Consider compliance strategy based on target jurisdictions.

---

## Sources Index

### Official Hyperliquid Documentation
- [HyperCore Overview](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/overview)
- [HyperEVM](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperevm)
- [HyperBFT Wiki](https://hyperliquid-co.gitbook.io/wiki/architecture/hyperbft)
- [Staking](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/staking)
- [Fees](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees)
- [Order Book](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/order-book)
- [Order Types](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/order-types)
- [Margining](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margining)
- [Funding](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/funding)
- [Auto-Deleveraging](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/auto-deleveraging)
- [Bridge](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/bridge)
- [HIP-1](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-1-native-token-standard)
- [HIP-2](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-2-hyperliquidity)
- [Dual-block Architecture](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/dual-block-architecture)
- [Interacting with HyperCore](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/interacting-with-hypercore)
- [Running a Validator](https://hyperliquid.gitbook.io/hyperliquid-docs/validators/running-a-validator)
- [API Overview](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api)
- [L1 Data Schemas](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/nodes/l1-data-schemas)
- [Protocol Vaults](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/vaults/protocol-vaults)
- [Contract Specifications](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/contract-specifications)

### GitHub Repositories
- [hyperliquid-dex/node](https://github.com/hyperliquid-dex/node)
- [hyperliquid-dex/contracts](https://github.com/hyperliquid-dex/contracts)
- [hyperliquid-dex/hyperliquid-python-sdk](https://github.com/hyperliquid-dex/hyperliquid-python-sdk)
- [hyperliquid-dex/hyperliquid-rust-sdk](https://github.com/hyperliquid-dex/hyperliquid-rust-sdk)

### Third-Party Analysis and Research
- [ChainArgos — Centralized Control in Hyperliquid](https://medium.com/chainargos/centralized-control-in-hyperliquid-3e9f7dd0d706)
- [Reverse Engineering Hyperliquid (can.ac)](https://blog.can.ac/2025/12/20/reverse-engineering-hyperliquid/)
- [Zealynx Architecture Analysis](https://www.zealynx.io/blogs/Understanding-Hyperliquid-Architecture-HyperBFT-HyperCore-HyperEVM-Part1)
- [Presto Labs — The HYPE Begins](https://www.prestolabs.io/research/hyperliquid-the-hype-begins)
- [rocknblock.io Technical Deep Dive](https://rocknblock.io/blog/how-does-hyperliquid-work-a-technical-deep-dive)
- [Halborn JELLY Analysis](https://www.halborn.com/blog/post/explained-the-hyperliquid-hack-march-2025)
- [OAK Research JELLY Analysis](https://oakresearch.io/en/analyses/investigations/hyperliquid-jelly-attack-context-vulnerability-team-solution)
- [AuditOne Security Overview](https://www.auditone.io/blog-posts/hyperliquid-a-comprehensive-look-at-innovation-growth-and-security-challenges-in-defi)

### Token and Fee Data
- [Tokenomist.ai — HYPE](https://tokenomist.ai/hyperliquid)
- [DeFiLlama — Hyperliquid Perps](https://defillama.com/protocol/hyperliquid-perps)
- [StakingRewards.com](https://www.stakingrewards.com/asset/hyperliquid)
- [Tokenomics.com](https://tokenomics.com/articles/hyperliquid-tokenomics-how-hype-captures-65m-monthly-in-holder-revenue)

### News and Community
- [Hyper Foundation — Permissionless Validator Network](https://hyperfnd.medium.com/hyperliquids-permissionless-validator-network-secured-by-the-community-ad0057cfad71)
- [CoinDesk — Decentralization Response](https://www.coindesk.com/tech/2025/01/08/the-protocol-hyperliquid-responds-to-decentralization-criticism)
- [CoinDesk — JELLY Delist](https://www.coindesk.com/markets/2025/03/26/hyperliquid-delists-jellyjelly-after-vault-squeezed-in-usd13m-tussle)
- [The Defiant — $1B Burn](https://thedefiant.io/news/tokens/hyperliquid-proposes-burning-13-percent-of-circulating-token-supply)
- [Bitget CEO FTX 2.0 Warning](https://www.bitget.com/news/detail/12560604666736)
- [Arrington Capital Open Letter](https://www.arringtoncapital.com/blog/hyperliquids-tipping-point-three-things-they-must-do-now/)
- [CoinDesk — CFTC Perpetuals](https://www.coindesk.com/markets/2025/05/23/cftcs-plans-for-crypto-perpetual-trading-puts-focus-on-hyperliquids-hype)
- [CoinDesk — Policy Center](https://www.coindesk.com/policy/2026/02/18/hyperliquid-starts-defi-lobbying-group-with-usd29-million-token-backing)
- [21shares — Perp DEX Wars](https://www.21shares.com/en-eu/insights/the-perpetual-dex-wars-hyperliquid-aster-and-lighter-in-focus)
- [CoinGecko — HIP-3 & HIP-4](https://www.coingecko.com/learn/hyperliquid-hip3-hip4-tokenized-stocks-and-prediction-markets)
- [The Block — HIP-3 OI](https://www.theblock.co/post/393810/hyperliquid-hip-3-markets-1-43-billion-open-interest-24-7-trading-tokenized-equities-commodities)
- [LayerZero Hyperliquid Concepts](https://docs.layerzero.network/v2/developers/hyperliquid/hyperliquid-concepts)
- [ASXN X Thread on HyperBFT](https://x.com/asxn_r/status/1853871055208640904)
