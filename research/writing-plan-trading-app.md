# Writing Plan: Torus Trading App (Phase 1 MVP)

**Spec:** [PRD-trading-app.md](./PRD-trading-app.md)
**Date:** 2026-04-16
**Estimated scope:** ~4,500 lines TypeScript/TSX + ~800 lines CSS/config
**Repo:** New standalone repo `torus-trading-app` (separate from torus-web-design)
**RPC dependencies:** all required endpoints (`torus_getOpenOrders`, `torus_getOrderBook`,
`torus_getPosition`, `torus_getBalances`, `torus_getMarkets`, `torus_getTradeHistory`,
`torus_submitNativeAction`) are implemented in torus-node. See
`crates/torus-rpc/src/torus.rs` for the server trait and
`crates/torus-rpc/src/lib.rs` tests for wire formats.

---

## Phase 1 MVP Scope (Steps 1-14)

The MVP delivers: wallet connect, market selector, price chart, order book,
recent trades, order entry (limit + market), position management, open orders
with cancel, and a basic portfolio/balances page. No staking, governance,
vaults, or referrals — those are Phase 2.

---

## Step 1: Project scaffold

Create the Next.js 15 project with the exact same stack as torus-web-design.

```bash
npx create-next-app@latest torus-trading-app \
  --typescript --tailwind --eslint --app --src-dir=false \
  --import-alias "@/*" --turbopack
```

**Install core dependencies:**

```bash
npm install @reown/appkit @reown/appkit-adapter-wagmi wagmi viem @wagmi/core
npm install @tanstack/react-query zustand
npm install lightweight-charts                # TradingView charts (MIT)
npm install @radix-ui/react-dialog @radix-ui/react-dropdown-menu \
  @radix-ui/react-tabs @radix-ui/react-select @radix-ui/react-slider \
  @radix-ui/react-toggle-group @radix-ui/react-tooltip @radix-ui/react-popover
npm install class-variance-authority clsx tailwind-merge
npm install lucide-react sonner               # icons + toasts
npm install framer-motion
npm install -D vitest @testing-library/react @testing-library/jest-dom
```

**Project structure:**

```
torus-trading-app/
├── app/
│   ├── layout.tsx              # Root layout (fonts, metadata, providers)
│   ├── client-layout.tsx       # Client shell (WalletProvider, QueryClient, Navbar)
│   ├── page.tsx                # Redirect to /trade/BTC-USD
│   ├── trade/
│   │   └── [market]/
│   │       └── page.tsx        # Main trading page
│   ├── portfolio/
│   │   └── page.tsx            # Portfolio page (Phase 1)
│   ├── markets/
│   │   └── page.tsx            # Markets overview
│   ├── staking/
│   │   └── page.tsx            # Phase 2 placeholder
│   └── governance/
│       └── page.tsx            # Phase 2 placeholder
├── components/
│   ├── ui/                     # shadcn primitives (button, dialog, tabs, etc.)
│   ├── navbar.tsx
│   ├── wallet-provider.tsx     # Adapted from torus-web-design
│   ├── connect-wallet-prompt.tsx
│   ├── trading/
│   │   ├── order-book.tsx
│   │   ├── price-chart.tsx
│   │   ├── order-entry.tsx
│   │   ├── recent-trades.tsx
│   │   ├── market-selector.tsx
│   │   ├── positions-table.tsx
│   │   ├── open-orders-table.tsx
│   │   └── trade-layout.tsx    # Grid layout orchestrator
│   └── portfolio/
│       ├── balance-cards.tsx
│       ├── positions-summary.tsx
│       └── transfer-modal.tsx
├── lib/
│   ├── rpc.ts                  # Torus JSON-RPC client
│   ├── ws.ts                   # WebSocket subscription manager
│   ├── fixed-point.ts          # FixedPoint <-> decimal conversion
│   ├── sign.ts                 # EIP-712 NativeAction signing
│   ├── appkit.ts               # Reown AppKit config (from torus-web-design)
│   ├── utils.ts                # cn() helper (shadcn)
│   ├── format.ts               # Price/qty/address formatting
│   └── constants.ts            # Chain ID, RPC URL, market names
├── hooks/
│   ├── use-order-book.ts       # React Query + polling for order book
│   ├── use-positions.ts        # React Query for positions
│   ├── use-balances.ts         # React Query for balances
│   ├── use-markets.ts          # React Query for market list
│   ├── use-open-orders.ts      # React Query for open orders
│   ├── use-trades-stream.ts    # WebSocket subscription hook
│   └── use-submit-action.ts    # Mutation hook for NativeAction submission
├── stores/
│   └── trading-store.ts        # Zustand: selected market, order form state, layout prefs
├── types/
│   └── index.ts                # TypeScript types for all RPC responses
├── styles/
│   └── globals.css             # Theme tokens (from torus-web-design) + trading additions
└── next.config.mjs
```

**Verify:** `npm run dev` starts without errors, blank page loads.

---

## Step 2: Design system + theme

**Files:**
- `app/globals.css`
- `lib/utils.ts`
- `components.json`
- Initialize shadcn components

**2a. globals.css** — Copy the `:root` / `.dark` CSS variable block from
torus-web-design's `globals.css`. Keep all theme tokens identical:

```css
@import "tailwindcss";

@theme inline {
  --color-background: hsl(0 0% 0%);
  --color-foreground: hsl(0 0% 95%);
  --color-primary: hsl(18 96% 49%);
  --color-primary-foreground: hsl(0 0% 100%);
  --color-card: hsl(0 0% 3%);
  --color-border: hsl(0 0% 12%);
  --color-muted: hsl(0 0% 8%);
  --color-muted-foreground: hsl(0 0% 60%);
  --color-destructive: hsl(0 85% 60%);
  --radius: 1rem;
  --font-sans: 'Orbitron', sans-serif;
}
```

Add trading-specific tokens:

```css
  --color-long: #22c55e;        /* green for buy/long/positive */
  --color-short: #ef4444;       /* red for sell/short/negative */
  --color-long-muted: #22c55e33;
  --color-short-muted: #ef444433;
```

Add trading-specific utilities:

```css
.font-mono { font-family: 'JetBrains Mono', 'Fira Code', monospace; }
.text-long { color: var(--color-long); }
.text-short { color: var(--color-short); }
.bg-long-muted { background-color: var(--color-long-muted); }
.bg-short-muted { background-color: var(--color-short-muted); }
```

Copy the `torus-glow`, `energy-button`, glassmorphism card classes from
torus-web-design for brand consistency.

**2b. lib/utils.ts** — Standard shadcn helper:

```typescript
import { type ClassValue, clsx } from "clsx"
import { twMerge } from "tailwind-merge"
export function cn(...inputs: ClassValue[]) { return twMerge(clsx(inputs)) }
```

**2c. Initialize shadcn components:**

```bash
npx shadcn@latest init
npx shadcn@latest add button dialog tabs select slider toggle-group \
  tooltip popover dropdown-menu input label separator scroll-area
```

**2d. Add Orbitron + JetBrains Mono fonts** in `app/layout.tsx` via
`next/font/google`:

```typescript
import { Orbitron, JetBrains_Mono } from 'next/font/google'
const orbitron = Orbitron({ subsets: ['latin'], variable: '--font-sans' })
const jetbrains = JetBrains_Mono({ subsets: ['latin'], variable: '--font-mono' })
```

**Verify:** `npm run dev` — dark page with Orbitron font, orange accent color visible.

---

## Step 3: Wallet connection

**Files:**
- `lib/appkit.ts` — copy from torus-web-design, update metadata
- `components/wallet-provider.tsx` — adapt from torus-web-design
- `components/connect-wallet-prompt.tsx` — copy from torus-web-design
- `app/client-layout.tsx` — wrap app in providers

**3a. lib/appkit.ts:**

Copy from torus-web-design verbatim. Changes:
- Update `metadata.name` to `"Torus Trading"`
- Update `metadata.description` to `"Torus Perpetual Futures Exchange"`
- Add Torus chain to the `networks` array (define custom chain with
  Torus chain ID, RPC URL, block explorer URL)

```typescript
import { defineChain } from 'viem'
export const torusDevnet = defineChain({
  id: 7777,  // Torus chain ID — verify from genesis
  name: 'Torus Devnet',
  nativeCurrency: { name: 'TRS', symbol: 'TRS', decimals: 18 },
  rpcUrls: { default: { http: ['http://localhost:8545'] } },
})
```

**3b. components/wallet-provider.tsx:**

Adapt from torus-web-design. Strip out:
- `IUser` type and all user/auth state (`signIn`, `refetchUser`, `updateUsername`)
- MongoDB-backed JWT authentication (trading app authenticates via wallet signature only)
- CSRF token logic

Keep:
- `WagmiProvider` + `QueryClientProvider` wrapping
- `isConnected`, `address`, `connect`, `disconnect`
- `useAccount`, `useDisconnect` from wagmi

The trading app doesn't need a backend auth flow — the wallet address IS
the identity. EIP-712 signatures authenticate each action.

~80 lines (down from 337 in torus-web-design).

**3c. app/client-layout.tsx:**

```tsx
"use client"
import { WalletProvider } from "@/components/wallet-provider"
import { Navbar } from "@/components/navbar"
import { Toaster } from "sonner"

export function ClientLayout({ children }: { children: React.ReactNode }) {
  return (
    <WalletProvider>
      <Navbar />
      <main className="min-h-screen">{children}</main>
      <Toaster theme="dark" />
    </WalletProvider>
  )
}
```

**3d. app/layout.tsx:**

```tsx
import { ClientLayout } from "./client-layout"
// fonts, metadata...
export default function RootLayout({ children }) {
  return (
    <html lang="en" className="dark">
      <body className={`${orbitron.variable} ${jetbrains.variable} font-sans`}>
        <ClientLayout>{children}</ClientLayout>
      </body>
    </html>
  )
}
```

**Verify:** Wallet connect button works, shows address after connecting.

---

## Step 4: Torus RPC client + types

**Files:**
- `types/index.ts` — all TypeScript interfaces for RPC responses
- `lib/rpc.ts` — JSON-RPC client
- `lib/fixed-point.ts` — FixedPoint utility class
- `lib/constants.ts` — RPC URLs, chain config

**4a. types/index.ts:**

Define interfaces matching every RPC response from the PRD Section 5.1:

```typescript
// Wire convention: numeric fields from the node are hex strings.
//   - FixedPoint values (price, quantity, pnl, margin): hex-encoded i128
//     (two's-complement; parse via `fromHex` → divide by 10^8 for decimal).
//   - MarketId: hex-encoded u64 (e.g. "0x1"). The on-wire u64 is what gets
//     signed inside EIP-712 PlaceOrder structs (convert `BigInt("0x1")` → u64).
//   - Address: 0x-prefixed 20-byte hex.
//   - block/timestamp: hex-encoded u64.

export interface OrderBookLevel { price: string; quantity: string; orderCount: number }
export interface OrderBook { marketId: string; bids: OrderBookLevel[]; asks: OrderBookLevel[] }

export interface Position {
  marketId: string;
  side: 'long' | 'short';            // RpcPosition uses "long"/"short", NOT "buy"/"sell"
  size: string; entryPrice: string;
  unrealizedPnl: string; realizedPnl: string; margin: string;
  marginMode: 'cross' | 'isolated';
  liquidationPrice: string;
}

export interface Balances {
  nativeBalance: string; evmBalance: string;
  totalMarginUsed: string; availableBalance: string; permanentStake: string;
}

export interface Market {
  marketId: string; baseAsset: string; quoteAsset: string;
  lotSize: string; tickSize: string; status: string;
}

export interface Trade {
  tradeId: string; marketId: string; price: string; quantity: string;
  side: string; blockNumber: number; timestamp: number;
}

export interface OpenOrder {
  orderId: string; marketId: string; side: string; price: string;
  remainingQty: string; originalQty?: string; orderType?: string;
  timeInForce?: string; reduceOnly?: boolean; clientOrderId?: string;
  timestamp?: number;
}

export interface Validator {
  address: string; pubkey: string; power: number;
  commissionBps: number; status: string;
}

export interface EpochInfo {
  currentEpoch: number; epochStartBlock: number; epochEndBlock: number;
  blocksRemaining: number; epochLength: number;
}

export interface StakingInfo { /* ... */ }
export interface Proposal { /* ... */ }
export interface GovernanceParams { /* ... */ }
export interface TreasuryInfo { /* ... */ }

// NativeAction: matches serde's default EXTERNALLY-TAGGED enum encoding
// of torus_types::NativeAction (crates/torus-types/src/lib.rs:290-376).
// Verified by running `serde_json::to_string` against the real type:
//   Unit variant     → bare string:   "ClaimRewards"
//   Struct variant   → { VariantName: { snake_case_fields... } }
//   Tuple variant    → { VariantName: { ...inner_struct_fields } }
// Field names are snake_case (Rust default), NOT camelCase.
// Numeric fields (FixedPoint, U256, u64, u128) serialize as JSON numbers,
// NOT hex strings — see "bigint safety" note below for the JSON.stringify caveat.

export type NativeAction =
  // --- Order book ---
  | { PlaceOrder: SerdePlaceOrderParams }
  | { CancelOrder: { order_id: number | bigint } }                           // u128
  | { CancelAllOrders: { market_id: number | null } }                        // u64 | null
  | { ModifyOrder: { order_id: number | bigint;
                     new_price: bigint | null; new_qty: bigint | null } }
  // --- Transfers ---
  | { TransferToPerp: { amount: bigint } }                                   // U256
  | { TransferToSpot: { amount: bigint } }
  | { Withdraw:       { amount: bigint; to: `0x${string}` } }
  // --- Staking ---
  | { Delegate:        { validator: `0x${string}`; amount: bigint } }
  | { Undelegate:      { validator: `0x${string}`; amount: bigint } }
  | { PermanentStake:  { amount: bigint } }
  | 'ClaimRewards'
  | { TopUpSelfStake:  { amount: bigint } }
  // --- Governance ---
  | { SubmitProposal: SerdeProposal }
  | { Vote:           { proposal_id: number; option: 'Yes' | 'No' | 'Abstain' } }
  // --- Oracle ---
  | { SubmitOraclePrices: SerdeOracleSubmission }
  // --- Validator ---
  | { RegisterValidator: { pubkey: number[]; commission: number } }          // [u8;32], u16
  | { UpdateCommission:  { new_rate: number } }
  | { JailVote:          { target: `0x${string}` } }
  | 'UnjailSelf'
  | { RotateValidatorKey: { new_pubkey: number[] } }
  // --- Admin ---
  | { UpdateMarketParams: { market_id: number; params: SerdeMarketParams } }
  | { ListMarket:         SerdeMarketListing }
  | { DelistMarket:       { market_id: number } }

// Matches torus_types::PlaceOrderParams (crates/torus-types/src/lib.rs:600-610).
// FixedPoint serializes as raw i128 (JSON number). Use `bigint` to preserve
// precision through JSON.stringify with a bigint-aware replacer (see Step 6).
export interface SerdePlaceOrderParams {
  market_id: number;
  is_buy: boolean;
  price: bigint;              // FixedPoint raw i128 (value * 10^8)
  quantity: bigint;           // FixedPoint raw i128
  order_type: SerdeOrderType;
  time_in_force: 'GTC' | 'IOC' | 'FOK' | 'PostOnly';
  reduce_only: boolean;
  client_order_id: number | null;
}

// torus_types::OrderType is externally tagged too:
//   "Limit" | "Market"
//   { StopMarket: { trigger: i128 } }
//   { StopLimit:  { trigger: i128, limit: i128 } }
export type SerdeOrderType =
  | 'Limit'
  | 'Market'
  | { StopMarket: { trigger: bigint } }
  | { StopLimit:  { trigger: bigint; limit: bigint } }

// UI-layer form state (human-readable decimal strings, camelCase). This is
// the input to `signAndSubmit()`, which converts to SerdePlaceOrderParams.
export interface PlaceOrderParams {
  marketId: string;                                                      // "0x1"
  isBuy: boolean;
  price: string;                                                         // "65000.5"
  quantity: string;                                                      // "0.1"
  orderType: 'Limit' | 'Market' | 'StopMarket' | 'StopLimit';
  trigger?: string;                                                      // decimal, required for Stop*
  limit?: string;                                                        // decimal, required for StopLimit
  timeInForce: 'GTC' | 'IOC' | 'FOK' | 'PostOnly';
  reduceOnly: boolean;
  clientOrderId?: number;
}
```

**Bigint safety for JSON.** `JSON.stringify` throws on raw `bigint` values.
Use a bigint-aware replacer when serializing a `SignedNativeAction`:

```typescript
export function stringifyWithBigint(v: unknown): string {
  return JSON.stringify(v, (_k, x) => typeof x === 'bigint' ? Number(x) : x)
}
```

This is safe as long as every bigint value fits in `Number.MAX_SAFE_INTEGER`
(2^53 - 1). FixedPoint prices up to ~$90M, U256 wei amounts up to ~0.009 ETH
wei-equivalents, and u64 nonces (ms-since-epoch) all fit. **For U256 `amount`
fields that could exceed 2^53** (e.g. ≥ 0.01 TRS at 18 decimals = 10^16 wei),
the replacer must emit a JSON number via a string-to-number-or-decimal library,
or the Rust side must accept a string — verify by roundtripping one large
amount through the probe crate before shipping Withdraw / Delegate flows.

// Placeholder types referenced above — fill in as needed:
// SerdeProposal, SerdeOracleSubmission, SerdeMarketParams, SerdeMarketListing.
// See crates/torus-types/src/lib.rs for the Rust source of truth.

**4b. lib/fixed-point.ts:**

```typescript
const SCALE = 100_000_000n; // 10^8

export function fromHex(hex: string): bigint { return BigInt(hex) }
export function toDecimal(raw: bigint, dp: number = 4): string {
  const sign = raw < 0n ? '-' : '';
  const abs = raw < 0n ? -raw : raw;
  const whole = abs / SCALE;
  const frac = abs % SCALE;
  const fracStr = frac.toString().padStart(8, '0').slice(0, dp);
  return `${sign}${whole}.${fracStr}`;
}
export function fromDecimal(s: string): bigint {
  const [whole, frac = ''] = s.split('.');
  const fracPadded = (frac + '00000000').slice(0, 8);
  const sign = s.startsWith('-') ? -1n : 1n;
  const absWhole = whole.replace('-', '');
  return sign * (BigInt(absWhole) * SCALE + BigInt(fracPadded));
}
export function toHex(raw: bigint): string {
  return raw < 0n ? `-0x${(-raw).toString(16)}` : `0x${raw.toString(16)}`;
}
```

**4c. lib/rpc.ts:**

```typescript
export class TorusRpc {
  private url: string;
  private id = 0;

  constructor(url: string) { this.url = url; }

  private async call<T>(method: string, params: unknown[] = []): Promise<T> {
    const res = await fetch(this.url, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: ++this.id, method, params }),
    });
    const json = await res.json();
    if (json.error) throw new Error(json.error.message);
    return json.result as T;
  }

  getOrderBook(marketId: string) { return this.call<OrderBook>('torus_getOrderBook', [marketId]); }
  getPosition(trader: string, marketId: string) { return this.call<Position | null>('torus_getPosition', [trader, marketId]); }
  getBalances(trader: string) { return this.call<Balances>('torus_getBalances', [trader]); }
  getMarkets(offset?: number, limit?: number) { return this.call<Market[]>('torus_getMarkets', [offset, limit]); }
  getTradeHistory(marketId: string, limit?: number) { return this.call<Trade[]>('torus_getTradeHistory', [marketId, limit]); }
  getOpenOrders(trader: string, marketId?: string) { return this.call<OpenOrder[]>('torus_getOpenOrders', [trader, marketId]); }
  getTradeHistoryRange(marketId: string, from: string, to: string, limit?: number) {
    return this.call<Trade[]>('torus_getTradeHistoryRange', [marketId, from, to, limit]);
  }
  getValidators() { return this.call<Validator[]>('torus_getValidators', []); }
  getEpoch() { return this.call<EpochInfo>('torus_getEpoch', []); }
  getStakingInfo(addr: string) { return this.call<StakingInfo>('torus_getStakingInfo', [addr]); }
  getDelegations(addr: string) { return this.call<Delegation[]>('torus_getDelegations', [addr]); }
  getProposals(status?: string) { return this.call<Proposal[]>('torus_getProposals', [status]); }
  getGovernanceParams() { return this.call<GovernanceParams>('torus_getGovernanceParams', []); }
  getTreasuryInfo() { return this.call<TreasuryInfo>('torus_getTreasuryInfo', []); }
  submitNativeAction(signed: unknown) { return this.call<string>('torus_submitNativeAction', [signed]); }
  getBlockNumber() { return this.call<string>('eth_blockNumber', []); }
  getBalance(addr: string) { return this.call<string>('eth_getBalance', [addr, 'latest']); }
  getChainId() { return this.call<string>('eth_chainId', []); }
}
```

~60 lines. One class, one method per RPC call.

**4d. lib/constants.ts:**

```typescript
export const RPC_URL = process.env.NEXT_PUBLIC_RPC_URL || 'http://localhost:8545';
export const WS_URL = process.env.NEXT_PUBLIC_WS_URL || 'ws://localhost:8545';
export const CHAIN_ID = Number(process.env.NEXT_PUBLIC_CHAIN_ID || '7777');

export const MARKET_NAMES: Record<string, string> = {
  '0x1': 'BTC-USD',
  '0x2': 'ETH-USD',
  '0x3': 'SOL-USD',
};
export const MARKET_IDS: Record<string, string> = Object.fromEntries(
  Object.entries(MARKET_NAMES).map(([k, v]) => [v, k])
);
```

**Verify:** Import `TorusRpc` in a test page, call `getMarkets()`, log result.

---

## Step 5: WebSocket subscription manager

**File:** `lib/ws.ts`

Manages a single WebSocket connection with auto-reconnect. Supports
multiple subscriptions multiplexed over one connection.

```typescript
export class TorusWs {
  private ws: WebSocket | null = null;
  private subs = new Map<string, (data: unknown) => void>();
  private reconnectTimer: NodeJS.Timeout | null = null;
  private url: string;

  constructor(url: string) { this.url = url; this.connect(); }

  private connect() {
    this.ws = new WebSocket(this.url);
    this.ws.onmessage = (e) => {
      const msg = JSON.parse(e.data);
      if (msg.method === 'torus_subscription' || msg.method === 'eth_subscription') {
        const cb = this.subs.get(msg.params.subscription);
        if (cb) cb(msg.params.result);
      }
    };
    this.ws.onclose = () => {
      this.reconnectTimer = setTimeout(() => this.connect(), 2000);
    };
  }

  async subscribe(method: string, params: unknown[], cb: (data: unknown) => void): Promise<string> {
    // Send subscribe RPC, get subscription ID, register callback
  }

  unsubscribe(subId: string) {
    this.subs.delete(subId);
    // Send unsubscribe RPC
  }

  destroy() { this.ws?.close(); }
}
```

~80 lines. Used by `use-trades-stream.ts` hook.

**Verify:** Connect to node WS, subscribe to `newHeads`, see block numbers logged.

---

## Step 6: EIP-712 signing

**File:** `lib/sign.ts`

Builds and signs NativeAction payloads. **The node verifies signatures using
a distinct EIP-712 type per NativeAction variant** — not a generic wrapper.
See `crates/torus-types/src/eip712.rs:216-740` for the authoritative set
of type strings and field layouts. A mismatch in type string, field order,
or scalar width (`uint64` vs `uint128` vs `int128`) causes signature
recovery to fail.

**6a. Domain separator.**

```typescript
import { type WalletClient } from 'viem'
import { CHAIN_ID } from './constants'

// Matches eip712.rs: { name: "Torus", version: "1", chainId: 7777,
// verifyingContract: 0x0000...0000 } (zero address, NOT 0x...0001)
const DOMAIN = {
  name: 'Torus',
  version: '1',
  chainId: CHAIN_ID,                                           // 7777
  verifyingContract: '0x0000000000000000000000000000000000000000' as `0x${string}`,
} as const
```

**6b. Nonce rule.** The node enforces `|now_ms - nonce| ≤ 60_000`
(`eip712.rs:26` `NONCE_WINDOW_MS = 60_000`). Use **milliseconds since
epoch directly** — do NOT multiply.

```typescript
const nonce = BigInt(Date.now())  // u64 milliseconds, within 60s window
```

**6c. EIP-712 type table — all 22 variants.** One entry per variant.
Each type string and field list below is taken directly from its `hash_*`
function in `eip712.rs`. **Do not edit without updating the Rust side in
lockstep — Rust is the source of truth.**

```typescript
// File locations are crates/torus-types/src/eip712.rs:line

const TYPES = {
  PlaceOrder: [                                                     // :217
    { name: 'marketId',         type: 'uint64' },
    { name: 'isBuy',            type: 'bool'   },
    { name: 'price',            type: 'int128' },
    { name: 'quantity',         type: 'int128' },
    { name: 'orderType',        type: 'uint8'  },   // Limit=0 Market=1 StopMarket=2 StopLimit=3
    { name: 'timeInForce',      type: 'uint8'  },   // GTC=0 IOC=1 FOK=2 PostOnly=3
    { name: 'reduceOnly',       type: 'bool'   },
    { name: 'clientOrderId',    type: 'uint64' },   // 0 if absent
    { name: 'hasClientOrderId', type: 'bool'   },
    { name: 'nonce',            type: 'uint64' },
  ],
  CancelOrder: [                                                    // :244
    { name: 'orderId', type: 'uint128' },
    { name: 'nonce',   type: 'uint64'  },
  ],
  CancelAllOrders: [                                                // :253
    { name: 'marketId',    type: 'uint64' },
    { name: 'hasMarketId', type: 'bool'   },
    { name: 'nonce',       type: 'uint64' },
  ],
  ModifyOrder: [                                                    // inspect eip712.rs ~:269
    { name: 'orderId',     type: 'uint128' },
    { name: 'newPrice',    type: 'int128'  },
    { name: 'hasNewPrice', type: 'bool'    },
    { name: 'newQty',      type: 'int128'  },
    { name: 'hasNewQty',   type: 'bool'    },
    { name: 'nonce',       type: 'uint64'  },
  ],
  TransferToPerp:  [{ name: 'amount', type: 'uint256' }, { name: 'nonce', type: 'uint64' }],
  TransferToSpot:  [{ name: 'amount', type: 'uint256' }, { name: 'nonce', type: 'uint64' }],
  Withdraw:        [{ name: 'amount', type: 'uint256' },
                    { name: 'to',     type: 'address' },
                    { name: 'nonce',  type: 'uint64'  }],
  Delegate:        [{ name: 'validator', type: 'address' },
                    { name: 'amount',    type: 'uint256' },
                    { name: 'nonce',     type: 'uint64'  }],
  Undelegate:      [{ name: 'validator', type: 'address' },
                    { name: 'amount',    type: 'uint256' },
                    { name: 'nonce',     type: 'uint64'  }],
  PermanentStake:  [{ name: 'amount', type: 'uint256' }, { name: 'nonce', type: 'uint64' }],
  ClaimRewards:    [{ name: 'nonce',  type: 'uint64'  }],
  TopUpSelfStake:  [{ name: 'amount', type: 'uint256' }, { name: 'nonce', type: 'uint64' }],
  Vote:            [{ name: 'proposalId', type: 'uint64' },
                    { name: 'option',     type: 'uint8'  },   // Yes=0 No=1 Abstain=2
                    { name: 'nonce',      type: 'uint64' }],
  RegisterValidator: [{ name: 'pubkey',     type: 'bytes32' },
                      { name: 'commission', type: 'uint16'  },
                      { name: 'nonce',      type: 'uint64'  }],
  UpdateCommission:   [{ name: 'newRate', type: 'uint16' }, { name: 'nonce', type: 'uint64' }],
  JailVote:           [{ name: 'target',  type: 'address' }, { name: 'nonce', type: 'uint64' }],
  UnjailSelf:         [{ name: 'nonce',   type: 'uint64'  }],
  RotateValidatorKey: [{ name: 'newPubkey', type: 'bytes32' }, { name: 'nonce', type: 'uint64' }],
  DelistMarket:       [{ name: 'marketId',  type: 'uint64' },  { name: 'nonce', type: 'uint64' }],
  // UpdateMarketParams, ListMarket, SubmitProposal, SubmitOraclePrices have
  // nested struct types. Mirror them from eip712.rs one-for-one — add the
  // nested type entries to TYPES and reference via `type: 'StructName'` in
  // the parent.
} as const satisfies Record<string, readonly { name: string; type: string }[]>
```

**6d. Sign + submit.** The wire format for `submitNativeAction` is
`hex(utf8(json(SignedNativeAction)))`. Wire shape was verified with a
probe run (`serde_json::to_string` on real Rust types, 2026-04-16):

```json
{"action":"ClaimRewards","nonce":1713312000123,
 "signature":{"v":27,"r":[17,17,…],"s":[34,34,…]}}
```

So `r`/`s` are arrays of 32 byte-numbers (not hex strings), `v` is a
number, and `nonce` is a number.

```typescript
import { type WalletClient } from 'viem'
import { fromDecimal, stringifyWithBigint } from './fixed-point'

// Matches torus_types::Signature (crates/torus-types/src/lib.rs:183).
interface WireSignature { v: number; r: number[]; s: number[] }

function splitSig(hex: `0x${string}`): WireSignature {
  const stripped = hex.slice(2)
  const r = Array.from(Buffer.from(stripped.slice(0, 64),   'hex'))
  const s = Array.from(Buffer.from(stripped.slice(64, 128), 'hex'))
  const v = parseInt(stripped.slice(128, 130), 16)      // viem returns 27|28
  return { v, r, s }
}

export async function signAndSubmit(
  rpc: TorusRpc,
  walletClient: WalletClient,
  account: `0x${string}`,
  action: NativeAction,                                 // serde-tagged shape from Step 4
): Promise<string> {
  const nonce = BigInt(Date.now())
  const { primaryType, message } = buildTypedMessage(action, nonce)

  const sigHex = await walletClient.signTypedData({
    account, domain: DOMAIN, types: TYPES, primaryType, message,
  })

  const envelope = {
    action,                                             // already serde shape (Step 4)
    nonce: Number(nonce),
    signature: splitSig(sigHex),
  }
  const jsonBytes = Buffer.from(stringifyWithBigint(envelope), 'utf8')
  const hexBody = '0x' + jsonBytes.toString('hex')
  return rpc.submitNativeAction(hexBody)
}
```

**v-byte normalization.** viem returns `v ∈ {27, 28}` (Ethereum style).
The probe output shows the Rust side serializes `v: 27` directly, and
`eip712.rs` uses `k256::ecdsa::RecoveryId::from_byte(v - 27)`-style
recovery (inspect the `recover_signer` helper before shipping — if it
expects 0/1, subtract 27 in `splitSig`). Add a `v-byte` test vector
to the fixtures in 6h to lock this down.

**6e. `buildTypedMessage` — full dispatcher.**

```typescript
function buildTypedMessage(a: NativeAction, nonce: bigint): {
  primaryType: keyof typeof TYPES; message: Record<string, unknown>;
} {
  if (a === 'ClaimRewards') return { primaryType: 'ClaimRewards', message: { nonce } }
  if (a === 'UnjailSelf')   return { primaryType: 'UnjailSelf',   message: { nonce } }

  if ('PlaceOrder' in a) {
    const p = a.PlaceOrder
    return {
      primaryType: 'PlaceOrder',
      message: {
        marketId: BigInt(p.market_id),
        isBuy: p.is_buy,
        price: p.price,
        quantity: p.quantity,
        orderType: orderTypeCode(p.order_type),
        timeInForce: TIF_CODE[p.time_in_force],
        reduceOnly: p.reduce_only,
        clientOrderId: BigInt(p.client_order_id ?? 0),
        hasClientOrderId: p.client_order_id != null,
        nonce,
      },
    }
  }
  if ('CancelOrder' in a)
    return { primaryType: 'CancelOrder',
             message: { orderId: BigInt(a.CancelOrder.order_id), nonce } }
  if ('CancelAllOrders' in a) {
    const mid = a.CancelAllOrders.market_id
    return { primaryType: 'CancelAllOrders',
             message: { marketId: BigInt(mid ?? 0), hasMarketId: mid != null, nonce } }
  }
  if ('ModifyOrder' in a) {
    const m = a.ModifyOrder
    return { primaryType: 'ModifyOrder',
             message: {
               orderId: BigInt(m.order_id),
               newPrice: m.new_price ?? 0n, hasNewPrice: m.new_price != null,
               newQty:   m.new_qty   ?? 0n, hasNewQty:   m.new_qty   != null,
               nonce } }
  }
  if ('TransferToPerp' in a) return { primaryType: 'TransferToPerp',
    message: { amount: a.TransferToPerp.amount, nonce } }
  if ('TransferToSpot' in a) return { primaryType: 'TransferToSpot',
    message: { amount: a.TransferToSpot.amount, nonce } }
  if ('Withdraw' in a) return { primaryType: 'Withdraw',
    message: { amount: a.Withdraw.amount, to: a.Withdraw.to, nonce } }
  if ('Delegate' in a) return { primaryType: 'Delegate',
    message: { validator: a.Delegate.validator, amount: a.Delegate.amount, nonce } }
  if ('Undelegate' in a) return { primaryType: 'Undelegate',
    message: { validator: a.Undelegate.validator, amount: a.Undelegate.amount, nonce } }
  if ('PermanentStake' in a) return { primaryType: 'PermanentStake',
    message: { amount: a.PermanentStake.amount, nonce } }
  if ('TopUpSelfStake' in a) return { primaryType: 'TopUpSelfStake',
    message: { amount: a.TopUpSelfStake.amount, nonce } }
  if ('Vote' in a) return { primaryType: 'Vote',
    message: { proposalId: BigInt(a.Vote.proposal_id),
               option: VOTE_CODE[a.Vote.option], nonce } }
  if ('RegisterValidator' in a) return { primaryType: 'RegisterValidator',
    message: { pubkey: bytes32Hex(a.RegisterValidator.pubkey),
               commission: a.RegisterValidator.commission, nonce } }
  if ('UpdateCommission' in a) return { primaryType: 'UpdateCommission',
    message: { newRate: a.UpdateCommission.new_rate, nonce } }
  if ('JailVote' in a) return { primaryType: 'JailVote',
    message: { target: a.JailVote.target, nonce } }
  if ('RotateValidatorKey' in a) return { primaryType: 'RotateValidatorKey',
    message: { newPubkey: bytes32Hex(a.RotateValidatorKey.new_pubkey), nonce } }
  if ('DelistMarket' in a) return { primaryType: 'DelistMarket',
    message: { marketId: BigInt(a.DelistMarket.market_id), nonce } }

  // SubmitProposal, SubmitOraclePrices, UpdateMarketParams, ListMarket carry
  // nested structs. Once TYPES has the nested entries, mirror
  // hash_submit_proposal / hash_submit_oracle_prices / hash_update_market_params
  // / hash_list_market from eip712.rs field-for-field here.
  throw new Error(`buildTypedMessage: unsupported variant ${JSON.stringify(a).slice(0, 64)}`)
}

function orderTypeCode(t: SerdeOrderType): 0 | 1 | 2 | 3 {
  if (t === 'Limit')     return 0
  if (t === 'Market')    return 1
  if ('StopMarket' in t) return 2     // trigger NOT in EIP-712 struct (eip712.rs:230-234 matches `{..}`)
  return 3                            // StopLimit; trigger/limit also NOT in struct
}
const TIF_CODE  = { GTC: 0, IOC: 1, FOK: 2, PostOnly: 3 } as const
const VOTE_CODE = { Yes: 0, No: 1, Abstain: 2 } as const
function bytes32Hex(b: number[]): `0x${string}` { return `0x${Buffer.from(b).toString('hex')}` }
```

**Stop-order caveat.** `hash_place_order` at `eip712.rs:230-234` matches
`StopMarket { .. }` and `StopLimit { .. }` via `{ .. }` — **the trigger
and limit fields are NOT committed to in the EIP-712 struct hash.** The
dispatcher above preserves that behavior. If Rust ever starts including
them in the hash (closing a potential malleability window), `TYPES` and
the dispatcher must both be updated in the same PR.

**6f. UI → serde conversion helper.** UI components construct
`PlaceOrderParams` (camelCase decimals, Step 4); `signAndSubmit` takes
the serde shape. Provide one converter so components never hand-build the
wire shape:

```typescript
export function toSerdeAction(ui: UiAction): NativeAction {
  switch (ui.kind) {
    case 'place': {
      const p = ui.params
      const order_type: SerdeOrderType =
        p.orderType === 'Limit'      ? 'Limit' :
        p.orderType === 'Market'     ? 'Market' :
        p.orderType === 'StopMarket' ? { StopMarket: { trigger: fromDecimal(p.trigger!) } } :
                                       { StopLimit:  { trigger: fromDecimal(p.trigger!),
                                                       limit:   fromDecimal(p.limit!)   } }
      return { PlaceOrder: {
        market_id: Number(BigInt(p.marketId)),
        is_buy: p.isBuy,
        price: fromDecimal(p.price),
        quantity: fromDecimal(p.quantity),
        order_type,
        time_in_force: p.timeInForce,
        reduce_only: p.reduceOnly,
        client_order_id: p.clientOrderId ?? null,
      } }
    }
    case 'cancel':    return { CancelOrder: { order_id: BigInt(ui.orderId) } }
    case 'cancelAll': return { CancelAllOrders: {
      market_id: ui.marketId ? Number(BigInt(ui.marketId)) : null } }
    // …remaining UI-shape → serde-shape mappings
  }
}
```

**6g. Test-vector CI gate (mandatory).** The only way to catch type-string,
field-order, or scalar-width drift before a user clicks Place Order is to
share a fixture file across Rust and TS.

- **Producer** (Rust): add a `#[test]` in `torus-types` that, for a pinned
  list of `(NativeAction, nonce)` tuples covering every variant, writes
  `crates/torus-types/tests/fixtures/eip712_vectors.json` with records of
  shape `{ label, action: <serde>, nonce: <u64>, struct_hash: "0x…",
  signing_hash: "0x…" }`. `struct_hash` comes from `eip712_struct_hash`
  and `signing_hash` from `eip712_signing_hash(domain_separator, struct_hash)`.
- **Consumer** (TS): a vitest fixture loader reads the same JSON, reconstructs
  the typed message via `buildTypedMessage`, and asserts:
  1. `hashStruct({ types: TYPES, primaryType, message })` (viem) equals
     `struct_hash` byte-for-byte.
  2. `hashTypedData({ domain: DOMAIN, types: TYPES, primaryType, message })`
     equals `signing_hash`.
  22 variants × 3 cases (Limit / Market / Stop\*) for PlaceOrder = ~25 tests.
- **CI**: Rust regenerates fixtures on every `eip712.rs` change (git diff is
  the review surface); TS CI fails on any divergence.
- **v-byte probe**: one fixture case signs with a pinned k256 secret key,
  stores the resulting signature, and asserts the TS dispatcher produces
  an identical wire envelope (confirms r/s endianness and v-byte convention).

This catches every realistic drift between the two sides — type string edit,
field reorder, width change, new variant, signature shape — without needing
a running node. Reference implementation in sibling project:
`torus-HBFT-explorer/lib/eip712.ts` already demonstrates this pattern for
an earlier subset of variants (memory id `2415a29f4b9c8a7b`).

**6h. Implementation + rollout order.** Build and test one variant at a
time against the fixtures first, then against a local node:

1. **Fixture infrastructure** (Rust test producer + TS vitest consumer).
2. `ClaimRewards` — empty action, proves domain + nonce + submission wire.
3. `CancelAllOrders` — single optional field + marker bool, proves the
   `hasMarketId` idiom and the externally-tagged `{ CancelAllOrders: {...} }`
   envelope.
4. `PlaceOrder` (Limit → Market → StopMarket → StopLimit) — full happy path.
5. Remaining 18 variants, each gated behind a passing test vector.

**Node-side debug aids if a submission fails despite vectors passing:**
`RUST_LOG=torus_rpc=debug,torus_types=debug` prints the recovered signer
and the struct hash the node computed. Compare byte-for-byte against the
TS-side hash.

---

## Step 7: React Query data hooks

**Files:** `hooks/use-order-book.ts`, `use-positions.ts`, `use-balances.ts`,
`use-markets.ts`, `use-open-orders.ts`, `use-trades-stream.ts`,
`use-submit-action.ts`

Each hook wraps a `TorusRpc` call with React Query for caching, polling,
and loading/error states.

**7a. hooks/use-order-book.ts:**

```typescript
import { useQuery } from '@tanstack/react-query'
import { rpc } from '@/lib/rpc'

export function useOrderBook(marketId: string) {
  return useQuery({
    queryKey: ['orderBook', marketId],
    queryFn: () => rpc.getOrderBook(marketId),
    refetchInterval: 500,   // 500ms polling
    enabled: !!marketId,
  })
}
```

**7b. hooks/use-positions.ts:**

```typescript
export function usePositions(trader: string | undefined) {
  const { data: markets } = useMarkets();
  return useQuery({
    queryKey: ['positions', trader],
    queryFn: async () => {
      if (!trader || !markets) return [];
      const positions = await Promise.all(
        markets.map(m => rpc.getPosition(trader, m.marketId))
      );
      return positions.filter(Boolean) as Position[];
    },
    refetchInterval: 2000,
    enabled: !!trader && !!markets,
  })
}
```

**7c-7e:** Same pattern for `useBalances` (5s poll), `useMarkets` (10s poll),
`useOpenOrders` (2s poll).

**7f. hooks/use-trades-stream.ts:**

```typescript
export function useTradesStream(marketId: string, onTrade: (t: Trade) => void) {
  useEffect(() => {
    const unsub = ws.subscribe('torus_subscribe', ['newTrades', { marketId }], onTrade);
    return () => { unsub.then(id => ws.unsubscribe(id)); };
  }, [marketId]);
}
```

**7g. hooks/use-submit-action.ts:**

```typescript
export function useSubmitAction() {
  const { data: walletClient } = useWalletClient();
  return useMutation({
    mutationFn: (action: NativeAction) => signAndSubmit(rpc, walletClient!, action),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['positions'] });
      queryClient.invalidateQueries({ queryKey: ['openOrders'] });
      queryClient.invalidateQueries({ queryKey: ['balances'] });
      toast.success('Action submitted');
    },
    onError: (e) => toast.error(e.message),
  })
}
```

~200 lines total across all hooks.

**Verify:** Render `useOrderBook('0x1')` data in a test component, see bid/ask levels.

---

## Step 8: Zustand trading store

**File:** `stores/trading-store.ts`

Client-side state that doesn't come from the server.

```typescript
import { create } from 'zustand'
import { persist } from 'zustand/middleware'

interface TradingStore {
  // Selected market
  selectedMarket: string;       // "BTC-USD"
  selectedMarketId: string;     // "0x1"
  setMarket: (name: string, id: string) => void;

  // Order form
  orderSide: 'buy' | 'sell';
  orderType: 'limit' | 'market' | 'stop_market' | 'stop_limit';
  orderPrice: string;
  orderQuantity: string;
  orderLeverage: number;
  orderTif: 'gtc' | 'ioc' | 'fok' | 'post_only';
  orderReduceOnly: boolean;
  setOrderSide: (s: 'buy' | 'sell') => void;
  setOrderType: (t: string) => void;
  setOrderPrice: (p: string) => void;
  setOrderQuantity: (q: string) => void;
  setOrderLeverage: (l: number) => void;
  setOrderTif: (tif: string) => void;
  setOrderReduceOnly: (r: boolean) => void;
  resetOrderForm: () => void;

  // Favorites
  favoriteMarkets: string[];
  toggleFavorite: (market: string) => void;

  // Layout prefs
  bottomTab: 'positions' | 'orders' | 'orderHistory' | 'tradeHistory';
  setBottomTab: (t: string) => void;
}

export const useTradingStore = create<TradingStore>()(
  persist(
    (set) => ({
      selectedMarket: 'BTC-USD',
      selectedMarketId: '0x1',
      setMarket: (name, id) => set({ selectedMarket: name, selectedMarketId: id }),
      orderSide: 'buy',
      orderType: 'limit',
      orderPrice: '',
      orderQuantity: '',
      orderLeverage: 10,
      orderTif: 'gtc',
      orderReduceOnly: false,
      // ... setters and resetOrderForm
      favoriteMarkets: [],
      toggleFavorite: (m) => set((s) => ({
        favoriteMarkets: s.favoriteMarkets.includes(m)
          ? s.favoriteMarkets.filter(x => x !== m)
          : [...s.favoriteMarkets, m],
      })),
      bottomTab: 'positions',
      setBottomTab: (t) => set({ bottomTab: t as any }),
    }),
    { name: 'torus-trading-store' } // localStorage key
  )
)
```

~80 lines.

**Verify:** Toggle order side, refresh page, state persists.

---

## Step 9: Navbar

**File:** `components/navbar.tsx`

```
┌───────────────────────────────────────────────────────────────────┐
│ [Logo] │ Trade │ Portfolio │ Markets │ Staking │ Governance │ 🔗  │
└───────────────────────────────────────────────────────────────────┘
```

- Logo: Torus icon + "TORUS" text, links to `/`
- Nav links: `Trade`, `Portfolio`, `Markets`, `Staking` (disabled), `Governance` (disabled)
- Active link highlighted with orange underline
- Right side: wallet connect button (address display when connected, truncated `0xabc...def`)
- Responsive: hamburger menu on mobile

Uses `next/link`, `usePathname()` for active state, `useWallet()` for
connect/disconnect. ~100 lines.

**Verify:** Navigate between pages, active state updates, wallet button works.

---

## Step 10: Market selector sidebar

**File:** `components/trading/market-selector.tsx`

Left sidebar on the trade page.

- List of markets from `useMarkets()` hook
- Each row: market name, last price (from recent trades), star button (favorite)
- Search input at top (client-side filter)
- Favorites section pinned at top
- Click → `router.push(/trade/${marketName})` + update Zustand store
- Compact: ~200px wide, scrollable

~100 lines.

**Verify:** Markets load, search filters, click navigates, favorites persist.

---

## Step 11: Order book + recent trades

**Files:**
- `components/trading/order-book.tsx`
- `components/trading/recent-trades.tsx`

**11a. order-book.tsx:**

Two-column display (bids green below, asks red above, spread in middle).

```
        Price      Size      Total
  Ask  65,150    0.12     ████░░░░
  Ask  65,140    0.35     ██████░░
  ─── Spread: $10 (0.015%) ───────
  Bid  65,130    0.28     █████░░░
  Bid  65,120    0.45     ████████
```

- Data from `useOrderBook(marketId)`
- Depth bars: width = `cumulative_size / max_cumulative * 100%`
- Click price → fills order entry price (`setOrderPrice`)
- Click quantity → fills order entry quantity
- Grouping selector (dropdown): tick size multiples
- ~150 lines

**11b. recent-trades.tsx:**

- Live stream from `useTradesStream(marketId)` + initial data from `useTradeHistory`
- Columns: Price, Size, Time (HH:MM:SS)
- Green/red based on trade side
- Rolling buffer of last 50 trades
- ~60 lines

**Verify:** Order book shows bids/asks with depth bars, trades stream in real-time.

---

## Step 12: Price chart

**File:** `components/trading/price-chart.tsx`

TradingView Lightweight Charts integration.

```typescript
import { createChart, CandlestickSeries } from 'lightweight-charts'

export function PriceChart({ marketId }: { marketId: string }) {
  const chartRef = useRef<HTMLDivElement>(null);
  const chartInstance = useRef<IChartApi | null>(null);

  useEffect(() => {
    if (!chartRef.current) return;
    const chart = createChart(chartRef.current, {
      width: chartRef.current.clientWidth,
      height: 400,
      layout: { background: { color: '#000' }, textColor: '#999' },
      grid: { vertLines: { color: '#1a1a1a' }, horzLines: { color: '#1a1a1a' } },
      crosshair: { mode: 0 },
    });
    const series = chart.addSeries(CandlestickSeries, {
      upColor: '#22c55e', downColor: '#ef4444',
      wickUpColor: '#22c55e', wickDownColor: '#ef4444',
    });
    chartInstance.current = chart;
    return () => chart.remove();
  }, []);

  // Load historical candles from torus_getTradeHistoryRange
  // Build 1m candles client-side from raw trades
  // Subscribe to newTrades WS to update latest candle in real-time
  // Timeframe selector: 1m, 5m, 15m, 1h, 4h, 1D
}
```

**Candle building:** Raw trades → group by time bucket → OHLCV per bucket.
Utility function `buildCandles(trades: Trade[], interval: number): Candle[]`.

**Position overlay:** Dashed line at entry price (green/red), dotted line at
liquidation price (red). Uses `chart.addLineSeries()`.

~200 lines (chart + candle builder + timeframe logic).

**Verify:** Chart renders with candles, updates on new trades, timeframe switch works.

---

## Step 13: Order entry panel + submission

**File:** `components/trading/order-entry.tsx`

Right side panel. This is the most complex component.

**Layout:**
```
┌─────────────────────────┐
│  [Limit] [Market] [Stop]│  ← order type tabs
├─────────────────────────┤
│  [Buy Long] [Sell Short]│  ← side toggle (green/red)
├─────────────────────────┤
│  Price:    [_________]  │  ← hidden for Market orders
│  Quantity: [_________]  │
│  Leverage: ──●──────── 10x│ ← slider
│  TIF:      [GTC ▼]     │  ← dropdown
│  □ Reduce Only          │  ← checkbox
│  □ Post Only            │  ← shortcut for TIF
├─────────────────────────┤
│  Est. Margin:  $6,500   │  ← computed
│  Est. Fee:     $1.30    │  ← computed
│  Est. Liq:     $62,100  │  ← computed
├─────────────────────────┤
│  [ Buy / Long BTC-USD ] │  ← submit button (green or red)
└─────────────────────────┘
```

- All form state from Zustand `useTradingStore()`
- Decimal input handling: prevent non-numeric, respect tick/lot size
- Max button on quantity: `availableBalance / price / leverage`
- Computed fields update live as user types:
  - `margin = price * quantity / leverage`
  - `fee = margin * feeBps / 10000` (get fee BPS from treasury info)
  - `liquidationPrice` = approximate based on margin mode
- Submit: builds `NativeAction::PlaceOrder`, calls `useSubmitAction().mutate()`
- Loading state during signing + submission
- Toast on success/error

~250 lines (form + computation + submission).

**Verify:** Fill in order form, click Buy, wallet signature popup, order submitted,
toast confirms, order appears in Open Orders tab.

---

## Step 14: Bottom panel — positions + open orders

**Files:**
- `components/trading/positions-table.tsx`
- `components/trading/open-orders-table.tsx`
- `components/trading/trade-layout.tsx`

**14a. positions-table.tsx:**

Table showing all open positions.

| Column | Source |
|---|---|
| Market | position.marketId → MARKET_NAMES lookup |
| Side | Long (green) / Short (red) |
| Size | position.size (formatted) |
| Entry | position.entryPrice |
| Mark | latest trade price from WS |
| Unrealized PnL | computed or from position data |
| Margin | position.margin |
| Liq. Price | position.liquidationPrice |
| Actions | [Close Market] button → PlaceOrder reduce-only market order |

~100 lines.

**14b. open-orders-table.tsx:**

Table showing user's open orders from `useOpenOrders(address)`.

| Column | Source |
|---|---|
| Market | from order marketId |
| Side | buy/sell |
| Type | Limit/Stop (from V2 StoredOrder, or "—" for V1) |
| Price | order.price |
| Quantity | order.remainingQty |
| Actions | [Cancel] → CancelOrder, [Cancel All] → CancelAllOrders |

~80 lines.

**14c. trade-layout.tsx:**

The grid orchestrator that assembles all trading components into the
3-column layout from the PRD:

```tsx
export function TradeLayout() {
  return (
    <div className="grid grid-cols-[200px_1fr_320px] h-[calc(100vh-56px)]">
      {/* Left: Market Selector */}
      <MarketSelector />

      {/* Center: Chart + OrderBook/Trades + Bottom Panel */}
      <div className="flex flex-col border-x border-border">
        <PriceChart marketId={marketId} />
        <div className="grid grid-cols-2 h-[300px] border-t border-border">
          <OrderBook marketId={marketId} />
          <RecentTrades marketId={marketId} />
        </div>
        <BottomPanel />
      </div>

      {/* Right: Order Entry */}
      <OrderEntry />
    </div>
  )
}
```

Responsive: On mobile (<768px), switch to tab-based layout (single column).

~60 lines.

**Verify:** Full trading page renders with all panels, data flows, orders can be
placed and cancelled.

---

## Step 15: Portfolio page

**File:** `app/portfolio/page.tsx`, `components/portfolio/balance-cards.tsx`,
`components/portfolio/positions-summary.tsx`, `components/portfolio/transfer-modal.tsx`

**15a. balance-cards.tsx:**

Three stat cards: Available Balance, Margin Used, Total Account Value.
Data from `useBalances(address)`.

**15b. positions-summary.tsx:**

Reuse `positions-table.tsx` with larger layout and PnL totals row.

**15c. transfer-modal.tsx:**

Dialog with tabs: Deposit to Perp | Withdraw to Spot | Withdraw.
Amount input + submit → `TransferToPerp` / `TransferToSpot` / `Withdraw` action.

~150 lines across all 3 components.

**Verify:** Portfolio page shows balances, positions, transfer works.

---

## Step 16: Markets overview page

**File:** `app/markets/page.tsx`

Table of all markets from `useMarkets()`.

| Column | Data |
|---|---|
| Market | pair name |
| Last Price | from latest trade (trade history limit=1 per market) |
| 24h Change | computed client-side from trade history |
| 24h Volume | computed client-side |
| Status | from market data |
| Action | [Trade] button → navigate to /trade/[market] |

~80 lines.

**Verify:** Markets page loads, clicking Trade navigates to trade page.

---

## Step 17: Format utilities + polish

**File:** `lib/format.ts`

```typescript
export function formatPrice(hex: string, dp: number = 2): string { /* FixedPoint → display */ }
export function formatQty(hex: string, dp: number = 4): string { /* FixedPoint → display */ }
export function formatUsd(hex: string): string { /* U256 wei → "$1,234.56" */ }
export function formatAddress(addr: string): string { /* "0xabc...def" */ }
export function formatPnl(hex: string): string { /* "+$123.45" green or "-$45.67" red */ }
export function formatTime(ts: number): string { /* "14:32:05" */ }
export function formatPercent(value: number): string { /* "+4.5%" or "-2.1%" */ }
```

Apply formatting across all components. Also:
- Add loading skeletons to all data-dependent components
- Add error boundaries with retry buttons
- Add `<title>` tags per page (`BTC-USD | Torus`)

~100 lines.

**Verify:** All numbers display correctly, loading states work, page titles update.

---

## Step 18: Environment config + deployment prep

**Files:**
- `.env.example`
- `.env.local`
- `next.config.mjs`
- `Dockerfile` (optional)

**.env.example:**
```
NEXT_PUBLIC_RPC_URL=http://localhost:8545
NEXT_PUBLIC_WS_URL=ws://localhost:8545
NEXT_PUBLIC_CHAIN_ID=7777
NEXT_PUBLIC_WALLETCONNECT_PROJECT_ID=your_project_id
```

**next.config.mjs:**
```javascript
export default {
  reactStrictMode: true,
  images: { remotePatterns: [] },
  // Proxy /api/rpc to torus-node to avoid CORS in production
  async rewrites() {
    return [
      { source: '/api/rpc', destination: process.env.NEXT_PUBLIC_RPC_URL || 'http://localhost:8545' },
    ];
  },
}
```

**Verify:** `npm run build` succeeds without errors. `npm start` serves production build.

---

## Dependency Chain

```
Step 1 (scaffold) ─── Step 2 (theme) ─── Step 3 (wallet) ──┐
                                                             │
Step 4 (RPC client + types) ─────────────────────────────────┤
Step 5 (WebSocket) ──────────────────────────────────────────┤
Step 6 (EIP-712 signing) ───────────────────────────────────┤
                                                             │
                                              Step 7 (hooks) ┤
                                              Step 8 (store) ┤
                                                             │
Step 9 (navbar) ─────────────────────────────────────────────┤
Step 10 (market selector) ───────────────────────────────────┤
Step 11 (order book + trades) ───────────────────────────────┤
Step 12 (price chart) ──────────────────────────────────────┤
Step 13 (order entry) ──────────────────────────────────────┤
Step 14 (positions + orders + layout) ──────────────────────┤
                                                             │
                                              Step 15 (portfolio)
                                              Step 16 (markets page)
                                              Step 17 (formatting + polish)
                                              Step 18 (env + deploy)
```

**Parallel tracks:**
- Steps 1-3 are sequential (scaffold → theme → wallet)
- Steps 4, 5, 6 can start after Step 1 (independent of theme/wallet)
- Steps 7-8 need Step 4
- Steps 9-14 need Steps 3 + 7 + 8 (the full foundation)
- Steps 10-14 can be built in any order (independent components)
- Steps 15-18 are independent polish after the core trade page works

**Critical path:** 1 → 2 → 3 → 4 → 7 → 13 → 14 = core trading flow.

**No external blockers.** All required RPC endpoints and the EIP-712
verification path are implemented and tested in torus-node
(`crates/torus-rpc/src/lib.rs`, `crates/torus-types/src/eip712.rs`).
Step 6 is the highest-risk client-side work: signature recovery is
verified on the node for every submission, so a type-string or field-order
mismatch will block *every* write action regardless of UI completeness.
Budget a signing-variant integration pass before declaring Step 13 done.
