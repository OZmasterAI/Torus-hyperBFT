# Product Requirements Document: Torus Trading App

**Date:** 2026-04-16
**Version:** 1.0
**Status:** Draft
**Approach:** Option B — Standalone app (separate from torus-web-design)
**Benchmark:** [app.hyperliquid.xyz/trade](https://app.hyperliquid.xyz/trade)

---

## Table of Contents

1. [Vision & Goals](#1-vision--goals)
2. [Target Users](#2-target-users)
3. [Technical Architecture](#3-technical-architecture)
4. [Feature Specifications](#4-feature-specifications)
   - 4.1 Trading UI (Core)
   - 4.2 Portfolio
   - 4.3 Markets
   - 4.4 Staking
   - 4.5 Governance
   - 4.6 Vaults
   - 4.7 Referrals
   - 4.8 Leaderboards
   - 4.9 Sub-accounts
   - 4.10 TWAP Orders
5. [RPC Integration Map](#5-rpc-integration-map)
6. [Design System](#6-design-system)
7. [Phased Rollout](#7-phased-rollout)
8. [Protocol Dependencies](#8-protocol-dependencies)
9. [Success Metrics](#9-success-metrics)

---

## 1. Vision & Goals

Build a standalone perpetual futures trading application for the Torus chain.
The app is the primary interface for traders interacting with Torus's native
order book. It is separate from the torus-web-design community/social platform
but shares the same wallet connection flow and visual identity.

**Goals:**
- Sub-second order placement via native actions (no EVM gas)
- Full trading experience: order book, charts, positions, margin management
- Staking and governance as first-class tabs (not buried in settings)
- Real-time data via WebSocket subscriptions
- Mobile-responsive (not native app — responsive web)

**Non-goals (v1):**
- Native mobile apps (iOS/Android)
- Spot trading (perps only in v1 — protocol doesn't support spot yet)
- Fiat on-ramp
- Social trading / copy-trading (separate from vaults)

---

## 2. Target Users

| Persona | Description | Primary Features |
|---|---|---|
| **Active Trader** | Executes 10-100+ trades/day, uses limit orders, manages positions | Trading UI, order book, hotkeys |
| **DeFi Yield Seeker** | Delegates stake, earns rewards, participates in governance | Staking, governance, portfolio |
| **API Trader** | Runs bots, needs low-latency programmatic access | Not in scope (direct RPC/WebSocket) |
| **Vault Depositor** | Deposits into strategy vaults, passive returns | Vaults, leaderboards |
| **Referrer** | Invites traders, earns fee rebates | Referrals, dashboard |

---

## 3. Technical Architecture

### 3.1 Stack

```
torus-trading-app/
├── Next.js 15 (App Router, React 19, Turbopack)
├── TypeScript (strict mode)
├── Tailwind v4 (dark theme, Torus orange brand)
├── Radix UI + shadcn/ui (component primitives)
├── Wagmi 2 + Reown AppKit (wallet connection)
├── TradingView Lightweight Charts (order book + price chart)
├── TanStack React Query (server state, polling, WebSocket)
├── Zustand (client state: selected market, order form, layout)
├── Framer Motion (transitions, not heavy animations)
└── Vitest + Playwright (testing)
```

**Why separate from torus-web-design:**
- Different user mental model (trading app vs social/gaming platform)
- Different performance requirements (real-time data, low latency)
- Independent deployment cadence
- Shared: design tokens, wallet connection config, Torus chain definition

### 3.2 Data Flow

```
┌─────────────────────────────────────────────────────────┐
│                   Browser (Client)                       │
│                                                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │
│  │ Order Book   │  │ Chart        │  │ Positions    │  │
│  │ Component    │  │ (TradingView)│  │ Table        │  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  │
│         │                 │                  │           │
│  ┌──────▼─────────────────▼──────────────────▼───────┐  │
│  │              Torus RPC Client (lib/rpc.ts)         │  │
│  │                                                    │  │
│  │  HTTP:  torus_getOrderBook, torus_getPosition, ... │  │
│  │  WS:    torus_subscribe("newTrades")               │  │
│  │         eth_subscribe("newHeads")                   │  │
│  │  Write: torus_submitNativeAction (EIP-712 signed)  │  │
│  └──────────────────────┬────────────────────────────┘  │
│                         │                                │
│  ┌──────────────────────▼────────────────────────────┐  │
│  │           Wagmi / viem (wallet + signing)          │  │
│  └──────────────────────┬────────────────────────────┘  │
└─────────────────────────┼────────────────────────────────┘
                          │ JSON-RPC over HTTP + WebSocket
┌─────────────────────────▼────────────────────────────────┐
│                    torus-node                              │
│  :8545 (HTTP + WS)                                        │
│  eth_* (EVM) + torus_* (native) endpoints                │
└──────────────────────────────────────────────────────────┘
```

### 3.3 RPC Client Design

A single `lib/rpc.ts` module wraps all Torus-specific RPC calls:

```typescript
// lib/rpc.ts
export class TorusRpcClient {
  constructor(httpUrl: string, wsUrl: string);

  // Queries (HTTP, polled via React Query)
  getOrderBook(marketId: bigint): Promise<OrderBook>;
  getPosition(trader: Address, marketId: bigint): Promise<Position | null>;
  getBalances(trader: Address): Promise<Balances>;
  getMarkets(offset?: number, limit?: number): Promise<Market[]>;
  getTradeHistory(marketId: bigint, limit?: number): Promise<Trade[]>;
  getStakingInfo(address: Address): Promise<StakingInfo>;
  getValidators(): Promise<Validator[]>;
  getEpoch(): Promise<EpochInfo>;
  getDelegations(delegator: Address): Promise<Delegation[]>;
  getProposals(status?: ProposalStatus): Promise<Proposal[]>;
  getGovernanceParams(): Promise<GovernanceParams>;
  getTreasuryInfo(): Promise<TreasuryInfo>;

  // Subscriptions (WebSocket)
  subscribeNewTrades(marketId: bigint, cb: (trade: Trade) => void): Unsubscribe;
  subscribeNewHeads(cb: (header: BlockHeader) => void): Unsubscribe;

  // Write (EIP-712 sign + submit)
  submitAction(action: NativeAction, signer: WalletClient): Promise<string>;
}
```

**FixedPoint handling:** All prices/quantities from the node are `i128` with
8 implicit decimals. The RPC client converts to/from `string` for display
and `bigint` for computation. A `FixedPoint` utility class handles this:

```typescript
// lib/fixed-point.ts
export class FixedPoint {
  static SCALE = 100_000_000n; // 10^8
  static fromRaw(hex: string): FixedPoint;
  static fromDecimal(s: string): FixedPoint;
  toDecimal(dp?: number): string;
  toRaw(): bigint;
}
```

### 3.4 EIP-712 Native Action Signing

All write operations (place order, delegate, vote, etc.) are signed as
EIP-712 typed data and submitted via `torus_submitNativeAction`. The signing
uses the connected wallet (MetaMask `signTypedData_v4`).

```typescript
async function signAndSubmitAction(
  action: NativeAction,
  walletClient: WalletClient,
  chainId: number,
): Promise<string> {
  const nonce = BigInt(Date.now()) * 1_000_000n; // nanosecond timestamp
  const domain = { name: "Torus", version: "1", chainId, verifyingContract: "0x..." };
  const signature = await walletClient.signTypedData({ domain, types, primaryType, message: { action, nonce } });
  return rpc.call("torus_submitNativeAction", [{ action, nonce, signature }]);
}
```

No gas fees. No EVM transaction. The wallet just signs a message.

---

## 4. Feature Specifications

### 4.1 Trading UI (Core)

**Route:** `/trade/[market]` (e.g., `/trade/BTC-USD`)

**Layout** (modeled after Hyperliquid):

```
┌──────────────────────────────────────────────────────────────────────┐
│ Navbar: Logo │ Trade │ Portfolio │ Staking │ Governance │ ··· │ 🔗  │
├──────────┬───────────────────────────────────────┬───────────────────┤
│          │                                       │                   │
│  Market  │         Price Chart                   │   Order Entry     │
│  Selector│         (TradingView Lightweight)     │   Panel           │
│          │                                       │                   │
│  BTC-USD │                                       │  ┌─────────────┐ │
│  ETH-USD │                                       │  │ Limit│Market│ │
│  SOL-USD │                                       │  ├─────────────┤ │
│  ...     │                                       │  │ Price       │ │
│          │                                       │  │ Quantity    │ │
│          │                                       │  │ Leverage    │ │
│          │                                       │  │ [Buy] [Sell]│ │
│          │                                       │  └─────────────┘ │
│          ├───────────────────┬───────────────────┤                   │
│          │                   │                   │  Order summary:   │
│          │   Order Book      │  Recent Trades    │  margin, fees,    │
│          │   (bid/ask depth) │  (live stream)    │  liq price est    │
│          │                   │                   │                   │
├──────────┴───────────────────┴───────────────────┴───────────────────┤
│ Positions │ Open Orders │ Order History │ Trade History │ Fills      │
│                                                                      │
│ ┌────────┬────────┬───────┬───────┬────────┬──────┬──────┬────────┐ │
│ │ Market │ Side   │ Size  │ Entry │ Mark   │ PnL  │ Liq  │ Close  │ │
│ ├────────┼────────┼───────┼───────┼────────┼──────┼──────┼────────┤ │
│ │BTC-USD │ Long   │ 0.5   │65,000 │65,120  │+$60  │62,100│  [X]   │ │
│ └────────┴────────┴───────┴───────┴────────┴──────┴──────┴────────┘ │
└──────────────────────────────────────────────────────────────────────┘
```

#### 4.1.1 Market Selector (Left Sidebar)

- List of all active markets from `torus_getMarkets`
- Each row: pair name, last price, 24h change %, 24h volume
- Search/filter input at top
- Starred/favorites (persisted in localStorage)
- Click navigates to `/trade/[market]`
- **Poll:** `torus_getMarkets` every 10s + price tickers from `newTrades` WS

#### 4.1.2 Price Chart (Center Top)

- **Library:** TradingView Lightweight Charts v4 (MIT license, 40KB gzipped)
- Candlestick chart (1m, 5m, 15m, 1h, 4h, 1D timeframes)
- **Data source:** Build candles client-side from `torus_getTradeHistoryRange`
  (historical) + `torus_subscribe("newTrades")` (live updates)
- Overlay: position entry price line (dashed), liquidation price line (red)
- Volume bars below candles
- Drawing tools: horizontal line, trend line (stored in localStorage)
- **No server-side candle aggregation needed** — client builds from raw trades

#### 4.1.3 Order Book (Center Left)

- Two-column display: bids (green) on left/bottom, asks (red) on right/top
- Depth visualization: horizontal bars showing cumulative size
- Data from `torus_getOrderBook(marketId)` polled every 500ms
- Click on price level → fills price in order entry
- Click on quantity → fills quantity in order entry
- Spread indicator between best bid and best ask
- Grouping selector: tick size, 5x, 10x, 50x, 100x

#### 4.1.4 Recent Trades (Center Right)

- Live stream from `torus_subscribe("newTrades", { marketId })`
- Columns: Price, Size, Time
- Color: green if buy-side aggressor, red if sell-side
- Shows last ~50 trades, auto-scrolling

#### 4.1.5 Order Entry Panel (Right)

**Order Types:**

| Type | Fields | Notes |
|---|---|---|
| **Limit** | Price, Quantity | Default. Time-in-force selector: GTC, PostOnly, IOC, FOK |
| **Market** | Quantity only | Executes at best available. Warning if slippage >1% |
| **Stop Market** | Trigger Price, Quantity | Triggers when mark price crosses trigger |
| **Stop Limit** | Trigger Price, Limit Price, Quantity | Triggers → places limit order |

**Common fields:**
- Side toggle: Buy Long (green) / Sell Short (red)
- Quantity input with max button (uses available balance / margin)
- Leverage slider: 1x - 50x (respects margin tier limits)
- Margin mode toggle: Cross / Isolated
- Reduce-only checkbox
- Client Order ID (optional, collapsed by default)

**Order summary (computed live as user types):**
- Estimated margin required
- Estimated fee (fee split basis points)
- Estimated liquidation price
- Cost basis

**Submit flow:**
1. User clicks Buy/Sell
2. App builds `NativeAction::PlaceOrder` with all params
3. Wallet popup for EIP-712 signature (no gas, just sign)
4. Submit via `torus_submitNativeAction`
5. Show success toast with order details, or error toast

#### 4.1.6 Bottom Panel — Positions

**Tabs:** Positions | Open Orders | Order History | Trade History | Fills

**Positions tab:**

| Column | Source |
|---|---|
| Market | from position data |
| Side | Long/Short |
| Size | position quantity |
| Entry Price | weighted average entry |
| Mark Price | latest trade price (from WS) |
| Unrealized PnL | computed: `(mark - entry) * size * direction` |
| Realized PnL | from position data |
| Margin | allocated margin |
| Liquidation Price | from position data |
| TP/SL | take-profit / stop-loss (if set) |
| Actions | [Close] [Modify TP/SL] |

Data from `torus_getPosition(trader, marketId)` for each market where user
has a position. Poll every 2s, update mark price from WS in real-time.

**Open Orders tab:**
- All open orders across all markets
- Columns: Market, Side, Type, Price, Quantity, Filled, Status, Time, [Cancel]
- Cancel button submits `NativeAction::CancelOrder`
- "Cancel All" button submits `NativeAction::CancelAllOrders`

**Order History / Trade History / Fills:**
- Historical data from `torus_getTradeHistory` / explorer API
- Paginated, filterable by market and date range

---

### 4.2 Portfolio

**Route:** `/portfolio`

**Layout:**

```
┌─────────────────────────────────────────────────────────────────┐
│ Portfolio Overview                                               │
│                                                                  │
│  Account Value: $125,430.00    24h PnL: +$1,230 (+0.99%)       │
│                                                                  │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────┐  │
│  │ Available Balance │  │ Margin Used      │  │ Margin Ratio │  │
│  │ $82,100          │  │ $43,330          │  │ 34.6%        │  │
│  └──────────────────┘  └──────────────────┘  └──────────────┘  │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ Equity Curve (line chart — 1D, 1W, 1M, 3M, All)         │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│  Positions │ Balances │ PnL Breakdown │ Transfer History        │
│  ┌────────┬──────┬───────┬──────┬───────────────────────────┐   │
│  │BTC-USD │ Long │ 0.5BTC│+$60  │ ████████████████ 78%      │   │
│  │ETH-USD │ Short│ 2 ETH │-$15  │ ████ 22%                  │   │
│  └────────┴──────┴───────┴──────┴───────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Data sources:**
- `torus_getBalances(address)` → nativeBalance, evmBalance, totalMarginUsed, availableBalance
- `torus_getPosition(address, marketId)` for each active market
- Equity curve: computed from historical balance snapshots (requires indexer — v2)

**Transfer actions:**
- Deposit to Perp: `TransferToPerp { amount }` — move from spot to perp margin
- Withdraw to Spot: `TransferToSpot { amount }` — move from perp to spot
- Withdraw: `Withdraw { amount, to }` — withdraw to external address

---

### 4.3 Markets

**Route:** `/markets`

Overview page showing all available perpetual markets.

| Column | Description |
|---|---|
| Market | Pair name (BTC-USD, ETH-USD, ...) |
| Last Price | Latest trade price |
| 24h Change | % change |
| 24h Volume | Total traded volume |
| Open Interest | Sum of all open positions |
| Funding Rate | Current funding rate (if implemented) |
| Status | Active / Halted |

Click on any row → navigate to `/trade/[market]`.

**Data:** `torus_getMarkets()` + aggregated trade data.

**Note:** Funding rate is not currently in the RPC responses. This is an
RPC endpoint gap — see Section 8.

---

### 4.4 Staking

**Route:** `/staking`

**Layout:**

```
┌─────────────────────────────────────────────────────────────────┐
│ Staking Dashboard                                                │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────────┐ │
│  │ Total Staked  │  │ Rewards      │  │ Permanent Stake       │ │
│  │ 50,000 TRS   │  │ 1,234 TRS    │  │ 10,000 TRS (locked)  │ │
│  │              │  │ [Claim]      │  │ APY: 5.00%            │ │
│  └──────────────┘  └──────────────┘  └───────────────────────┘ │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ Validator Table                                           │   │
│  │ ┌──────────┬───────┬───────────┬─────────┬─────────────┐ │   │
│  │ │Validator │ Power │ Commission│ Status  │ Action      │ │   │
│  │ ├──────────┼───────┼───────────┼─────────┼─────────────┤ │   │
│  │ │ 0xabc... │ 25%   │ 5%        │ Active  │ [Delegate]  │ │   │
│  │ │ 0xdef... │ 18%   │ 10%       │ Active  │ [Delegate]  │ │   │
│  │ │ 0x123... │ 12%   │ 3%        │ Jailed  │ —           │ │   │
│  │ └──────────┴───────┴───────────┴─────────┴─────────────┘ │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│  My Delegations │ Unbonding │ Reward History                    │
│  ┌──────────┬────────┬──────────┬──────────────────────────┐    │
│  │Validator │ Amount │ Rewards  │ [Undelegate] [Claim]     │    │
│  └──────────┴────────┴──────────┴──────────────────────────┘    │
│                                                                  │
│  Permanent Staking                                               │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ Lock TRS permanently for 5% APY + 1.5x governance weight │   │
│  │ Amount: [________] TRS    [Stake Permanently]             │   │
│  │ ⚠️  This action is IRREVERSIBLE. Permanently staked       │   │
│  │    tokens cannot be withdrawn.                             │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Data sources:**
- `torus_getValidators()` — validator list
- `torus_getStakingInfo(address)` — delegations, permanent stake, pending rewards
- `torus_getDelegations(address)` — delegation breakdown
- `torus_getEpoch()` — epoch info (for estimated reward timing)

**Actions:**
- Delegate: `NativeAction::Delegate { validator, amount }` — modal with validator picker + amount
- Undelegate: `NativeAction::Undelegate { validator, amount }` — confirm modal with unbonding period warning
- Claim Rewards: `NativeAction::ClaimRewards` — one-click
- Permanent Stake: `NativeAction::PermanentStake { amount }` — double-confirm modal (irreversible)

**Computed displays:**
- Estimated APY per validator: `validator_inflation_APY = 200 / sqrt(total_staked_TRS)` + fee share
- Time until next epoch reward distribution
- Validator uptime / participation (future — not in current RPC)

---

### 4.5 Governance

**Route:** `/governance`

**Layout:**

```
┌─────────────────────────────────────────────────────────────────┐
│ Governance                                       [New Proposal] │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────────────┐ │
│  │ Active        │  │ Your Weight  │  │ Treasury              │ │
│  │ 3 proposals   │  │ 75,000 TRS   │  │ 1.2M TRS             │ │
│  │              │  │ (1.5x perm)  │  │                       │ │
│  └──────────────┘  └──────────────┘  └───────────────────────┘ │
│                                                                  │
│  Filter: [All] [Active] [Passed] [Rejected] [Executed]          │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ #12: Increase max validators to 25                       │   │
│  │ Status: Active │ Ends: Block 450,000 (est. 2 days)      │   │
│  │ For: 68% ████████████████████░░░░░░░░ Against: 32%      │   │
│  │ Quorum: 45% / 51% required                              │   │
│  │ [Vote Yes] [Vote No] [Vote Abstain]                     │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ #11: Treasury spend — 50,000 TRS for market maker grant │   │
│  │ Status: Passed │ Executed at block 440,120               │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Data sources:**
- `torus_getProposals(status?)` — proposal list
- `torus_getProposal(id)` — single proposal detail
- `torus_getGovernanceParams()` — voting period, quorum, min stake
- `torus_getTreasuryInfo()` — treasury balance, fee split ratios

**Actions:**
- Vote: `NativeAction::Vote { proposal_id, option: Yes|No|Abstain }`
- New Proposal: `NativeAction::SubmitProposal { type, title, description, params }`
  - Types: Parameter Change, Treasury Spend, Market Listing
  - Requires min stake (from governance params)

---

### 4.6 Vaults

**Route:** `/vaults`

**Status: Requires protocol work** — no vault mechanism exists on-chain.

**Concept:** Strategy vaults where users deposit TRS (or margin), a vault
manager (human or bot) trades on their behalf, profits/losses are shared.

**Design:**

```
┌─────────────────────────────────────────────────────────────────┐
│ Vaults                                                           │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ HyperScalper Vault          Managed by: 0xabc...        │   │
│  │ Strategy: BTC-USD momentum │ TVL: $2.1M │ APY: +34%    │   │
│  │ 30d PnL: +$180k │ Max DD: -8.2% │ Depositors: 142      │   │
│  │ [Deposit]  [Withdraw]                                    │   │
│  └──────────────────────────────────────────────────────────┘   │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │ DeltaNeutral Vault          Managed by: 0xdef...        │   │
│  │ Strategy: ETH/BTC spread │ TVL: $500k │ APY: +12%      │   │
│  │ [Deposit]  [Withdraw]                                    │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

**Protocol requirements (new NativeAction variants needed):**
- `CreateVault { name, manager, performance_fee_bps, lock_period }`
- `DepositToVault { vault_id, amount }`
- `WithdrawFromVault { vault_id, amount }`
- `VaultPlaceOrder { vault_id, ... }` (manager-only)
- Vault accounting in a new CF (`CF_VAULTS`)
- Vault PnL tracking

**RPC endpoints needed:**
- `torus_getVaults()` — list all vaults
- `torus_getVault(vault_id)` — vault details + performance
- `torus_getVaultPositions(vault_id)` — current positions held by vault

**Implementation phase:** Phase 3 (after core trading + staking ship)

---

### 4.7 Referrals

**Route:** `/referrals` (tab within profile or standalone page)

**Concept:** Users share referral links. When referred traders pay fees,
the referrer earns a rebate (e.g., 10% of referred user's fees).

**Frontend (can build now):**
- Referral code generation (derived from wallet address)
- Referral link: `https://app.torus.xyz/trade/BTC-USD?ref=CODE`
- Dashboard: referred users count, total fees generated, rebates earned

**Protocol requirements:**
- `NativeAction::SetReferralCode { code: String }` or auto-derive from address
- On-chain referral tracking in fee distribution path (`FeeSplitter`)
- Referral rebate percentage (governable parameter)
- New CF: `CF_REFERRALS` storing referrer→referee mappings + cumulative fees

**RPC endpoints needed:**
- `torus_getReferralInfo(address)` — code, referred users, rebates
- `torus_getReferralStats(address)` — volume, fees generated

**Note:** torus-web-design already has a referral system (MongoDB-based,
`?ref=CODE` query param). The on-chain version is separate but the UX
pattern can be borrowed.

**Implementation phase:** Phase 2 (protocol work needed, but smaller scope than vaults)

---

### 4.8 Leaderboards

**Route:** `/leaderboard`

**Concept:** Rank traders by PnL, volume, or other metrics. Like Hyperliquid's
leaderboard page.

**Layout:**

```
┌─────────────────────────────────────────────────────────────────┐
│ Leaderboard          [Daily] [Weekly] [Monthly] [All Time]      │
│                                                                  │
│  ┌────┬───────────┬──────────┬──────────┬──────────┬─────────┐ │
│  │Rank│ Trader    │ PnL      │ ROI %    │ Volume   │ Trades  │ │
│  ├────┼───────────┼──────────┼──────────┼──────────┼─────────┤ │
│  │ 1  │ 0xabc...  │ +$89,230 │ +42.1%   │ $12.5M   │ 1,847  │ │
│  │ 2  │ 0xdef...  │ +$71,100 │ +38.7%   │ $8.2M    │ 923    │ │
│  │ 3  │ 0x789...  │ +$55,600 │ +31.2%   │ $15.1M   │ 3,102  │ │
│  └────┴───────────┴──────────┴──────────┴──────────┴─────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

**Backend requirement:** This needs an **indexer** that aggregates per-trader
PnL over time windows. Two options:
1. Extend `torus-explorer` to track per-address PnL snapshots
2. New lightweight indexer service that reads from torus-node WS

**No protocol changes needed** — all data derivable from trade history + positions.

**RPC endpoints needed (on explorer/indexer):**
- `GET /api/leaderboard?period=daily&limit=100`
- `GET /api/trader/:address/stats`

**Implementation phase:** Phase 2 (needs indexer work, no protocol changes)

---

### 4.9 Sub-accounts

**Status: Requires protocol work**

**Concept:** A single wallet address can create multiple isolated trading accounts.
Each sub-account has its own margin, positions, and orders — but shares the
same wallet for signing.

**Protocol requirements:**
- `NativeAction::CreateSubAccount { label }` → returns sub_account_id
- All order/position actions gain optional `sub_account_id` field
- Separate balance/margin tracking per sub-account
- New CF or key prefix scheme in existing CFs

**Frontend:**
- Sub-account selector dropdown in navbar
- Per-sub-account portfolio view
- Transfer between sub-accounts

**Implementation phase:** Phase 3 (significant protocol change)

---

### 4.10 TWAP Orders

**Status: Requires protocol work**

**Concept:** Time-Weighted Average Price — split a large order into smaller
chunks executed over a time period to minimize market impact.

**Protocol requirements:**
- New order type: `OrderType::TWAP { total_qty, duration_blocks, num_slices }`
- TWAP execution engine in `native_executor.rs` that places child orders
  at each block interval
- Child order tracking (link to parent TWAP)

**Frontend:**
- New order type tab in order entry
- Fields: Total Quantity, Duration (e.g., "30 minutes"), Number of slices
- Progress bar showing execution status
- Cancel TWAP (cancels remaining slices)

**Implementation phase:** Phase 3 (moderate protocol work)

---

## 5. RPC Integration Map

### 5.1 Existing Endpoints (ready to use)

| Feature | RPC Method | Poll Interval |
|---|---|---|
| Order Book | `torus_getOrderBook(market_id)` | 500ms |
| Positions | `torus_getPosition(trader, market_id)` | 2s |
| Balances | `torus_getBalances(trader)` | 5s |
| Markets | `torus_getMarkets()` | 10s |
| Trade History | `torus_getTradeHistory(market_id, limit)` | on-demand |
| Historical Range | `torus_getTradeHistoryRange(market, from, to, limit)` | on-demand |
| Staking | `torus_getStakingInfo(address)` | 10s |
| Validators | `torus_getValidators()` | 30s |
| Epoch | `torus_getEpoch()` | 10s |
| Delegations | `torus_getDelegations(address)` | 10s |
| Proposals | `torus_getProposals(status?)` | 30s |
| Governance Params | `torus_getGovernanceParams()` | 60s |
| Treasury | `torus_getTreasuryInfo()` | 30s |
| Submit Action | `torus_submitNativeAction(signed)` | on-demand |
| Live Trades | `torus_subscribe("newTrades")` | WebSocket stream |
| New Blocks | `eth_subscribe("newHeads")` | WebSocket stream |
| EVM Balance | `eth_getBalance(address)` | 10s |

### 5.2 Missing Endpoints (need protocol/RPC work)

| Feature | Needed Endpoint | Complexity |
|---|---|---|
| Open orders per user | `torus_getOpenOrders(trader, market_id?)` | Medium — needs new RPC method reading from order book state |
| Funding rate | `torus_getFundingRate(market_id)` | Medium — funding not implemented in protocol |
| Open interest | `torus_getOpenInterest(market_id)` | Small — aggregate from positions CF |
| Mark price | `torus_getMarkPrice(market_id)` | Small — oracle price or last trade |
| User trade history | `torus_getUserTrades(trader, market_id?, limit?)` | Medium — needs index by trader address |
| Order status/history | `torus_getOrderStatus(order_id)` | Medium — orders are removed after fill/cancel |
| 24h price change | Computed client-side from trade history | None |
| Vault endpoints | `torus_getVaults`, `torus_getVault`, etc. | Large — protocol feature |
| Referral endpoints | `torus_getReferralInfo`, etc. | Medium — protocol feature |
| Leaderboard | Explorer API, not torus-node RPC | Medium — indexer work |

### 5.3 Critical Gap: Open Orders Per User

The most important missing endpoint is **open orders for a specific user**.
Currently the order book can be read (`torus_getOrderBook`) but there's no
way to query "show me all of 0xabc's open orders." This is essential for the
trading UI's bottom panel.

**Options:**
1. Add `torus_getOpenOrders(trader, market_id?)` to torus-rpc — scans order
   books in memory, filters by owner address
2. Track user orders in a separate CF index (`CF_USER_ORDERS`: `address → [order_id]`)

Option 1 is simpler for now (order books are in memory). Option 2 scales better.

---

## 6. Design System

### 6.1 Visual Identity (inherit from torus-web-design)

| Token | Value | Usage |
|---|---|---|
| **Background** | `hsl(0 0% 0%)` — pure black | Page background |
| **Card surface** | `hsl(0 0% 3%)` — near-black | Panels, cards |
| **Primary** | `hsl(18 96% 49%)` — Torus orange `#ed5209` | CTAs, brand accents, active states |
| **Border** | `hsl(0 0% 12%)` — dark grey | Panel borders, dividers |
| **Text primary** | `hsl(0 0% 95%)` — near-white | Main text |
| **Text muted** | `hsl(0 0% 60%)` — grey | Secondary text, labels |
| **Long/Buy** | `#22c55e` (green-500) | Buy buttons, positive PnL, bid side |
| **Short/Sell** | `#ef4444` (red-500) | Sell buttons, negative PnL, ask side |
| **Font** | Orbitron (headings), Inter or JetBrains Mono (data) | Orbitron for brand, monospace for prices/numbers |

### 6.2 Trading-Specific Design Notes

- **Number formatting:** All prices/quantities in monospace font for alignment
- **Decimal precision:** Prices to tick size (varies per market), quantities to lot size
- **Color intensity for depth:** Order book bars use opacity (0.1-0.5) of green/red
- **Glassmorphism cards:** Inherit `backdrop-filter: blur(40px)` from torus-web-design
- **No heavy 3D:** The torus visualization (Three.js) stays on the landing page
  of torus-web-design, NOT on the trading app — performance matters here

### 6.3 Responsive Breakpoints

| Breakpoint | Layout |
|---|---|
| Desktop (>1280px) | Full 3-column layout as shown |
| Tablet (768-1280px) | Chart full-width, order book + entry below, stacked |
| Mobile (<768px) | Single column, tab navigation between chart/book/entry/positions |

---

## 7. Phased Rollout

### Phase 1: Core Trading (MVP) — 6-8 weeks

**Goal:** A trader can connect wallet, see order book, place/cancel orders,
manage positions. Minimum viable trading app.

| Feature | Included | Notes |
|---|---|---|
| Wallet connection (Wagmi + Reown) | Yes | |
| Market selector | Yes | |
| Price chart (TradingView Lightweight) | Yes | Candles from trade history |
| Order book display | Yes | |
| Recent trades (WebSocket) | Yes | |
| Order entry (Limit, Market) | Yes | Stop orders in Phase 1b |
| Position management | Yes | |
| Basic portfolio (balances) | Yes | |
| EIP-712 native action signing | Yes | |
| `lib/rpc.ts` client | Yes | |
| Mobile responsive | Basic | Functional, not polished |

**Blocked by:** `torus_getOpenOrders` endpoint (Section 5.3)

### Phase 1b: Trading Polish — 2-3 weeks

| Feature | Notes |
|---|---|
| Stop Market + Stop Limit orders | |
| TP/SL on positions | |
| Order modification | |
| Leverage slider with margin tier display | |
| Keyboard shortcuts (B=buy, S=sell, Esc=cancel) | |
| Sound effects (optional, togglable) | |
| Chart drawing tools | |
| Market stats (24h volume, computed client-side) | |

### Phase 2: Social + Staking — 4-6 weeks

| Feature | Protocol Work? |
|---|---|
| Staking UI (delegate, undelegate, claim) | No |
| Permanent staking UI | No |
| Governance UI (proposals, voting) | No |
| Leaderboard (basic — needs indexer) | Indexer extension |
| Referral system | Protocol: on-chain referral tracking |
| Portfolio equity curve | Indexer: historical balance snapshots |
| Open interest display | Small RPC addition |

### Phase 3: Advanced — 6-8 weeks

| Feature | Protocol Work? |
|---|---|
| Vaults | Yes — significant protocol addition |
| Sub-accounts | Yes — protocol NativeAction additions |
| TWAP orders | Yes — new order type in matching engine |
| Funding rate display | Yes — if funding implemented |
| Advanced chart indicators | No |
| API documentation page | No |
| Mobile-optimized UI polish | No |

### Timeline Estimate

```
Week:  1    2    3    4    5    6    7    8    9   10   11   12   13-18
       ├────┼────┼────┼────┼────┼────┼────┼────┼────┼────┼────┼────┼────►
Phase 1 █████████████████████████████████████
        Core Trading MVP          │ Polish
Phase 2                           ████████████████████████
                                  Staking + Social + Leaderboard
Phase 3                                                    ██████████████
                                                           Vaults, Subs
```

---

## 8. Protocol Dependencies Summary

Features that **cannot** be built frontend-only and need protocol/RPC changes:

| Feature | Protocol Change | RPC Change | Effort | Phase |
|---|---|---|---|---|
| Open orders per user | None (in-memory scan) | `torus_getOpenOrders` | Small | **Blocker for Phase 1** |
| Open interest | None | `torus_getOpenInterest` | Small | Phase 1b |
| Mark/index price | None | `torus_getMarkPrice` | Small | Phase 1b |
| User trade history | CF index by address | `torus_getUserTrades` | Medium | Phase 1b |
| On-chain referrals | Fee split modification + CF | `torus_getReferralInfo` | Medium | Phase 2 |
| Leaderboard data | Explorer/indexer extension | REST API | Medium | Phase 2 |
| Vaults | New NativeAction variants + CF | Multiple new endpoints | Large | Phase 3 |
| Sub-accounts | NativeAction + balance refactor | Modify existing endpoints | Large | Phase 3 |
| TWAP | New order type + execution engine | `torus_getTwapStatus` | Medium | Phase 3 |
| Funding rate | Funding mechanism in protocol | `torus_getFundingRate` | Large | Phase 3 |

---

## 9. Success Metrics

| Metric | Target (3 months post-launch) |
|---|---|
| Daily Active Traders | 500+ |
| Daily Trading Volume | $10M+ |
| Order Placement Latency (UI→confirmed) | <1s |
| Page Load Time | <2s (desktop), <3s (mobile) |
| Order Book Refresh Rate | 500ms |
| Uptime | 99.5% |
| Wallet Connection Success Rate | >95% |
| Orders per Active Trader per Day | 15+ |
