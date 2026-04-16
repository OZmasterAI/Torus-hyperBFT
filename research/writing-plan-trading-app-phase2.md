# Writing Plan: Torus Trading App (Phase 2 — Staking + Governance + Extensions)

**Spec:** [PRD-trading-app.md](./PRD-trading-app.md) §7 (Phase 2) + §8
**Phase 1 plan:** [writing-plan-trading-app.md](./writing-plan-trading-app.md)
**Date:** 2026-04-16
**Estimated scope:** ~1,800 lines TypeScript/TSX + ~200 lines CSS (additive
on top of Phase 1)
**Repo:** `torus-trading-app` (same repo as Phase 1 — Phase 2 extends it)
**Prerequisite:** Phase 1 shipped and green on devnet — wallet connect,
trade page, order placement, cancellations, positions, portfolio all
working. The EIP-712 fixture suite (Phase 1 Step 6g) must be passing so
Phase 2 can extend it variant-by-variant.

---

## Phase 2 scope — what's in, what's blocked

**Unblocked (this plan covers, Steps 1-10):**
- Staking UI (delegate, undelegate, claim rewards, permanent stake, top-up self-stake)
- Validator list
- Governance: proposal list, proposal detail, vote, submit proposal
- Open interest display on trade page
- Mark/index price display (if `torus_getMarkPrice` RPC exists or is trivial)
- Navigation: enable `/staking` and `/governance` routes (Phase 1 placeholders)

**Blocked on protocol or indexer work (out of scope for this plan):**
- **Leaderboard** — needs indexer REST API. Blocked until explorer team
  exposes `/v1/leaderboard/pnl`, `/v1/leaderboard/volume`. Plan file
  `writing-plan-leaderboard-indexer.md` should be authored separately
  before UI work starts.
- **Referral system** — needs on-chain referral tracking (new NativeAction
  variants, new RPC endpoints, fee-split protocol changes). Blocked until
  `torus-economics` adds the referral mechanism. PRD §8 lists this as
  Phase 2 but it's really "Phase 2 UI, after protocol catches up".
- **Portfolio equity curve** — needs indexer historical balance snapshots.
  Blocked on the same indexer work as leaderboard.
- **Open interest per market history** — needs indexer time-series;
  current `torus_getOpenInterest` is point-in-time only.

Step 11 below is a stub for the blocked work so the directory structure
is ready when indexer/protocol dependencies land.

---

## RPC dependencies map (verified against repo state 2026-04-16)

**Already implemented in torus-node (verified against
`crates/torus-rpc/src/torus.rs`):**

| RPC method | Rust impl line | Response type | Phase 2 use |
|---|---|---|---|
| `torus_getValidators` | `torus.rs:112` | `Vec<RpcValidator>` | Validator list |
| `torus_getStakingInfo` | `:488` | `RpcStakingInfo { delegated[], permanentStake, pendingRewards, unbonding[] }` | Staking page |
| `torus_getDelegations` | `:574` | `Vec<RpcDelegation { validator, amount }>` | Staking page |
| `torus_getEpoch` | (existing) | `RpcEpochInfo` | Staking page (countdown) |
| `torus_getProposals` | `:634` | `Vec<RpcProposal>` | Governance list |
| `torus_getGovernanceParams` | `:677` | `RpcGovernanceParams { votingPeriodBlocks, quorumBps, … }` | Governance page |
| `torus_getTreasuryInfo` | `:697` | `RpcTreasuryInfo { treasuryAddress, treasuryBalance, cumulativeBurned, cumulativeTreasury, currentFeeSplit }` | Governance page |
| `torus_getOpenInterest` | `:973` | `RpcOpenInterest { marketId, longOi, shortOi }` | Trade page header |

**Needs verification before use:**

| RPC method | Status check command | Use |
|---|---|---|
| `torus_getMarkPrice` | `grep "get_mark_price" crates/torus-rpc/src/torus.rs` | Mark/index price ticker |
| Individual proposal detail | Check whether `torus_getProposals` supports filtering by ID, or if a dedicated `torus_getProposal(id)` exists | Proposal detail page |
| Vote-history-by-address | Check whether `torus_getVotesByVoter` exists | Governance: "your votes" list |

**Needs adding (separate writing plan required):**

| Feature | New RPC(s) | Protocol change? |
|---|---|---|
| Referrals | `torus_getReferralInfo`, `torus_getReferralTree` | YES — fee-split modification + new NativeAction variants |
| Leaderboard | REST via indexer | NO protocol change, indexer work |
| Portfolio equity curve | `/v1/balances/history` via indexer | NO protocol change, indexer work |

---

## NativeAction dependency map

**Already in torus-types EIP-712 dispatcher (verified against
`crates/torus-types/src/eip712.rs`):**

| Variant | Type string | Line | Phase 2 use |
|---|---|---|---|
| `Delegate` | `Delegate(address validator,uint256 amount,uint64 nonce)` | `:319` | Delegate flow |
| `Undelegate` | `Undelegate(address validator,uint256 amount,uint64 nonce)` | `:329` | Undelegate flow |
| `PermanentStake` | `PermanentStake(uint256 amount,uint64 nonce)` | `:339` | Permanent-stake flow |
| `ClaimRewards` | `ClaimRewards(uint64 nonce)` | `:348` | Claim button |
| `TopUpSelfStake` | `TopUpSelfStake(uint256 amount,uint64 nonce)` | `:482` | Validator self-stake top-up |
| `SubmitProposal` | `SubmitProposal(string title,string description,bytes32 actionHash,uint64 nonce)` | `:359` | Submit proposal flow |
| `Vote` | `Vote(uint64 proposalId,uint8 option,uint64 nonce)` | `:372` | Vote buttons |

**Key simplification for nested types.** `SubmitProposal` wraps
`ProposalAction` into a `bytes32 actionHash` rather than embedding a
nested EIP-712 struct. Same pattern for `SubmitOraclePrices`
(`bytes32 pricesHash`), `ListMarket` (`bytes32 listingHash`), and
`UpdateMarketParams` (`bytes32 paramsHash`). **TS side does NOT need
nested EIP-712 type definitions** — it computes the `bytes32` hash
separately using the canonical byte encoding the Rust side uses.

Concretely, for `SubmitProposal`, TS must:
1. Serialize `ProposalAction` canonically (mirror
   `hash_proposal_action` in `eip712.rs` — find its impl).
2. Compute `keccak256` of that canonical encoding → `actionHash`.
3. Pass `{ title, description, actionHash, nonce }` into `signTypedData`
   with the flat `SubmitProposal` type.

This is materially easier than a true nested EIP-712 type (no subtype
declarations in `TYPES`, no viem quirks with nested types).

---

## Step 1: Extend EIP-712 dispatcher for Phase 2 variants

**Files:**
- `lib/sign.ts` — extend `TYPES` table and `buildTypedMessage` dispatcher
- `lib/proposal-hash.ts` — new file: canonical encoding of `ProposalAction`

**1a. Add to `TYPES` table:**

```typescript
// Additions to the TYPES const from Phase 1 Step 6c:
  Delegate: [
    { name: 'validator', type: 'address' },
    { name: 'amount',    type: 'uint256' },
    { name: 'nonce',     type: 'uint64'  },
  ],
  Undelegate: [
    { name: 'validator', type: 'address' },
    { name: 'amount',    type: 'uint256' },
    { name: 'nonce',     type: 'uint64'  },
  ],
  PermanentStake: [
    { name: 'amount', type: 'uint256' },
    { name: 'nonce',  type: 'uint64'  },
  ],
  ClaimRewards: [
    { name: 'nonce', type: 'uint64' },
  ],
  TopUpSelfStake: [
    { name: 'amount', type: 'uint256' },
    { name: 'nonce',  type: 'uint64'  },
  ],
  SubmitProposal: [
    { name: 'title',       type: 'string'  },
    { name: 'description', type: 'string'  },
    { name: 'actionHash',  type: 'bytes32' },
    { name: 'nonce',       type: 'uint64'  },
  ],
  Vote: [
    { name: 'proposalId', type: 'uint64' },
    { name: 'option',     type: 'uint8'  },  // Yes=0 No=1 Abstain=2
    { name: 'nonce',      type: 'uint64' },
  ],
```

`Delegate`, `Undelegate`, `PermanentStake`, `ClaimRewards`, `Vote`,
`TopUpSelfStake` entries already exist in Phase 1's plan — verify they
made it into the code before duplicating.

**1b. Extend `buildTypedMessage`:** add cases for
`Delegate`/`Undelegate`/`PermanentStake`/`ClaimRewards`/`TopUpSelfStake`/
`Vote`/`SubmitProposal`. Phase 1 already covers all except
`SubmitProposal`.

```typescript
// Inside buildTypedMessage switch:
  if ('SubmitProposal' in a) {
    const p = a.SubmitProposal                                   // { title, description, action }
    const actionHash = hashProposalAction(p.action)              // bytes32 (see 1c)
    return {
      primaryType: 'SubmitProposal',
      message: { title: p.title, description: p.description, actionHash, nonce },
    }
  }
```

**1c. `lib/proposal-hash.ts`:**

Mirror `hash_proposal_action` from `crates/torus-types/src/eip712.rs`
(find it with `grep "fn hash_proposal_action"`; currently near the
`hash_submit_proposal` function at `:357`). Verify field order and
widths by reading the Rust function. Compute `keccak256` via viem's
`keccak256` helper. Add a test vector to the fixture suite (Phase 1
Step 6g) so this encoding is cross-checked automatically.

**1d. Extend fixture suite.** For each new variant above, add a
`(label, action, nonce, struct_hash, signing_hash)` record to
`crates/torus-types/tests/fixtures/eip712_vectors.json`. Minimum
coverage: 1 fixture per variant + one fixture per `ProposalAction`
variant (UpdateMarketParams, ListMarket, DelistMarket, ParameterChange,
ValidatorRegistration — 5 total).

**Verify:** `cargo test -p torus-types eip712_vectors` regenerates
fixtures; `npm test` in `torus-trading-app` all green (12+ new assertions).

---

## Step 2: Extend RPC client + TS types

**Files:**
- `types/index.ts` — add `Validator`, `StakingInfo`, `Delegation`,
  `Unbonding`, `Proposal`, `GovernanceParams`, `TreasuryInfo`,
  `OpenInterest` interfaces
- `lib/rpc.ts` — add methods: already stubbed in Phase 1 Step 4c,
  just verify they're called correctly

**2a. Types (camelCase matches RPC `#[serde(rename_all = "camelCase")]`):**

```typescript
export interface Validator {
  address: string;                  // EVM address
  pubkey: string;                   // hex-encoded 32-byte Ed25519 pubkey
  power: string;                    // hex u256 — voting power
  commissionBps: number;            // basis points, e.g. 500 = 5%
  status: 'active' | 'jailed' | 'inactive';
  selfStake: string;                // hex u256
}

export interface StakingInfo {
  delegated: Delegation[];          // outbound delegations by this address
  permanentStake: string;           // hex u256
  pendingRewards: string;           // hex u256
  unbonding: Unbonding[];
}

export interface Delegation { validator: string; amount: string }

export interface Unbonding {
  amount: string;
  validator: string;
  unlockEpoch: number;              // when tokens become withdrawable
}

export interface Proposal {
  id: number;                       // u64
  proposer: string;                 // EVM address
  title: string;
  description: string;
  proposalType: string;             // discriminant name
  status: 'pending' | 'active' | 'passed' | 'rejected' | 'executed';
  votesFor: string;                 // hex u256
  votesAgainst: string;
  votesAbstain: string;
  submittedBlock: number;
  votingEndsBlock: number;
  action: ProposalAction;           // nested — see torus-types ProposalAction
}

export type ProposalAction =
  | { UpdateMarketParams: { market_id: number; params: MarketParams } }
  | { ListMarket: MarketListing }
  | { DelistMarket: { market_id: number } }
  | { ParameterChange: { key: string; value: string } }
  | { ValidatorRegistration: { candidate: `0x${string}` } }

export interface MarketParams {
  tick_size: bigint; lot_size: bigint;
  max_leverage: number; maintenance_margin_bps: number;
  max_funding_rate_bps: number;
}

export interface MarketListing {
  base_asset: string; quote_asset: string;
  tick_size: bigint; lot_size: bigint;
  max_leverage: number; maintenance_margin_bps: number;
}

export interface GovernanceParams {
  votingPeriodBlocks: string;       // hex u64
  quorumBps: string;                // hex — minimum turnout
  minProposalStake: string;         // hex u256
  permanentWeightMultiplier: string;
  treasuryAddress: string;
}

export interface TreasuryInfo {
  treasuryAddress: string;
  treasuryBalance: string;          // hex u256
  cumulativeBurned: string;
  cumulativeTreasury: string;
  currentFeeSplit: {
    makerRebateBps: number;
    treasuryBps: number;
    burnBps: number;
    devPoolBps: number;
  };
}

export interface OpenInterest {
  marketId: string;
  longOi: string;                   // hex FixedPoint
  shortOi: string;
}
```

**2b. RPC methods already exist in `lib/rpc.ts` (Phase 1 Step 4c).**
Verify they map to the new types. Add `getOpenInterest(marketId)` if
missing. Add `getProposal(id)` once its torus-node counterpart is
confirmed (fall back to `getProposals().find(p => p.id === id)` if not).

**Verify:** Each method returns typed data, no `any` in return paths.
Call each RPC once against a running devnet and log the response shape
— compare against the types above; fix the TS side to match what the
node actually emits.

---

## Step 3: React Query hooks for Phase 2 data

**Files:** `hooks/use-validators.ts`, `use-staking-info.ts`,
`use-delegations.ts`, `use-proposals.ts`, `use-governance-params.ts`,
`use-treasury-info.ts`, `use-open-interest.ts`

**Polling cadences:**

| Hook | Refetch interval | Rationale |
|---|---|---|
| `useValidators` | 30 s | Set changes per epoch only |
| `useStakingInfo(addr)` | 10 s | User cares about pending rewards accrual |
| `useDelegations(addr)` | 10 s | Same |
| `useProposals` | 15 s | Changes on block; proposals submitted infrequently |
| `useGovernanceParams` | 60 s | Rarely changes |
| `useTreasuryInfo` | 30 s | Changes per block (fee accrual) but slowly |
| `useOpenInterest(marketId)` | 5 s | Changes per trade |

Each hook is a ~10-15 line `useQuery` wrapper (Phase 1 pattern).
Invalidate on `useSubmitAction` success where relevant
(`['stakingInfo', …]` after Delegate/Undelegate/Claim; `['proposals']`
after SubmitProposal/Vote).

**Verify:** Render each hook's output in a throwaway test page, observe
cadences and data shapes match the spec. No infinite re-render loops.

---

## Step 4: Staking page

**Files:**
- `app/staking/page.tsx` (replace Phase 1 placeholder)
- `components/staking/stake-dashboard.tsx`
- `components/staking/validator-list.tsx`
- `components/staking/delegate-modal.tsx`
- `components/staking/undelegate-modal.tsx`
- `components/staking/claim-rewards-button.tsx`
- `components/staking/permanent-stake-modal.tsx`
- `components/staking/unbonding-list.tsx`

**Layout:**
```
┌───────────────────────────────────────────────────────────┐
│  Your Staking                                             │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐          │
│  │ Total Staked│ │ Pending Rwd │ │ Perm. Stake │          │
│  │   1,500 TRS │ │   12.4 TRS  │ │    500 TRS  │          │
│  └─────────────┘ └─────────────┘ └─────────────┘          │
│  [ Claim Rewards ]  [ Stake Permanent ]                   │
├───────────────────────────────────────────────────────────┤
│  Your Delegations                                         │
│  Validator              | Amount      | Actions           │
│  0xabc...def (Alice)    | 1,000 TRS   | [Undel.] [Top up] │
│  0x123...456 (Bob)      |   500 TRS   | [Undel.] [Top up] │
├───────────────────────────────────────────────────────────┤
│  Active Validators                                        │
│  # | Name         | Stake   | Comm. | Status | Action     │
│  1 | 0xabc..def   | 100k    | 5%    | Active | [Delegate] │
│  2 | 0x123..456   | 80k     | 3%    | Active | [Delegate] │
├───────────────────────────────────────────────────────────┤
│  Unbonding                                                │
│  Amount | Validator | Unlocks in                          │
│    50 TRS | 0xabc..  | 3 days (epoch 1234)                │
└───────────────────────────────────────────────────────────┘
```

**4a. stake-dashboard.tsx:** Three balance cards + 2 action buttons.
Uses `useStakingInfo(address)`. ~80 lines.

**4b. validator-list.tsx:** Table from `useValidators()`. Sort by stake.
"Delegate" button opens `DelegateModal` prefilled with that validator.
~100 lines.

**4c. delegate-modal.tsx:**
- Dialog with validator pre-selected (or dropdown if opened without one)
- Amount input (available balance → max)
- Submit → `toSerdeAction({ kind: 'delegate', validator, amount })` →
  `useSubmitAction().mutate(action)`
- Toast on success, invalidate `stakingInfo`, `delegations`, `balances`
- ~80 lines

**4d. undelegate-modal.tsx:** Same structure, delegation pre-selected;
action is `Undelegate`. Warn user about unbonding period (pull period
from `GovernanceParams` or a dedicated RPC). ~70 lines.

**4e. claim-rewards-button.tsx:** Single button, disabled if
`pendingRewards === 0`. Action is `ClaimRewards` (no params). ~30 lines.

**4f. permanent-stake-modal.tsx:** **Critical UX warning — permanent
stake is irreversible.** Require explicit checkbox confirmation before
submit. Action is `PermanentStake { amount }`. ~60 lines.

**4g. unbonding-list.tsx:** Table of `StakingInfo.unbonding`. Show
"Unlocks in N days" by computing `(unlockEpoch - currentEpoch) *
epochLengthBlocks * blockTimeSeconds / 86400`. Pulls epoch info from
`useEpoch()`. ~60 lines.

**UiAction extension** (extend `toSerdeAction` from Phase 1 Step 6f):

```typescript
  case 'delegate':
    return { Delegate: { validator: ui.validator, amount: BigInt(ui.amount) } }
  case 'undelegate':
    return { Undelegate: { validator: ui.validator, amount: BigInt(ui.amount) } }
  case 'permanentStake':
    return { PermanentStake: { amount: BigInt(ui.amount) } }
  case 'claimRewards':
    return 'ClaimRewards'
  case 'topUpSelfStake':
    return { TopUpSelfStake: { amount: BigInt(ui.amount) } }
```

**Amount units.** `Delegate` / `Undelegate` / `PermanentStake` /
`TopUpSelfStake` use `U256` (wei-scaled, 18 decimals), NOT FixedPoint
(10^8). UI inputs decimal TRS → multiply by 10^18. Verify the unit by
re-reading `NativeAction::Delegate { amount: U256 }` in
`crates/torus-types/src/lib.rs:321-325` before coding.

**Verify:** Delegate 100 TRS to a validator on devnet, observe
`stakingInfo.delegated` increments by 100 TRS, balance decreases by
100 TRS + gas. Undelegate 50, observe `unbonding` list gains an entry.
Claim rewards when non-zero, observe `pendingRewards → 0` and balance
increases.

---

## Step 5: Validator self-service (optional, validators only)

**Files:**
- `components/staking/validator-admin.tsx` — shown only if connected
  address is a registered validator (check via `useValidators().find(v => v.address === address)`)

Actions: `UpdateCommission`, `RotateValidatorKey`, `UnjailSelf`,
`TopUpSelfStake`, `JailVote` (for voting against another validator).

~150 lines. Can be deferred to Phase 2b if validator set is small and
validators use the CLI (`torus-wallet`) directly.

**Verify:** connect with a validator address, see admin panel;
commission change submits and appears in `useValidators()`.

---

## Step 6: Governance page — proposal list + detail

**Files:**
- `app/governance/page.tsx` (replace Phase 1 placeholder)
- `components/governance/proposal-list.tsx`
- `components/governance/proposal-card.tsx`
- `app/governance/[id]/page.tsx` — detail page
- `components/governance/proposal-detail.tsx`
- `components/governance/vote-buttons.tsx`
- `components/governance/vote-progress-bar.tsx`
- `components/governance/governance-stats-header.tsx`

**6a. proposal-list.tsx:** Filter tabs: All / Active / Passed /
Rejected / Executed. Cards show title, proposer (truncated), status
badge, vote tallies (progress bar), "voting ends in N blocks".
~120 lines.

**6b. proposal-detail.tsx:** Full description, action payload
(formatted per `proposalType`), vote breakdown (Yes / No / Abstain
percentages), quorum indicator, voting deadline.
Users can vote if connected and proposal is active. ~200 lines.

**6c. vote-buttons.tsx:** Three buttons (Yes / No / Abstain) →
`Vote { proposal_id, option }`. Disable all after user has voted
(check history). ~50 lines.

**6d. vote-progress-bar.tsx:** Three-segment horizontal bar.
Quorum line marker. ~40 lines.

**6e. governance-stats-header.tsx:** Total proposals, active count,
total voting power, treasury balance, current quorum. Uses
`useGovernanceParams()` + `useTreasuryInfo()` + `useProposals()`.
~80 lines.

**UiAction extension:**

```typescript
  case 'vote':
    return { Vote: { proposal_id: ui.proposalId, option: ui.option } }
```

**Verify:** list page shows existing proposals from devnet, clicking
one navigates to detail, voting as an active proposal submits and
`votesFor` (or For/Against/Abstain) updates.

---

## Step 7: Submit proposal flow

**Files:**
- `components/governance/submit-proposal-modal.tsx`
- `lib/proposal-hash.ts` — canonical encoding + `keccak256` of `ProposalAction`

**7a. submit-proposal-modal.tsx:** Multi-step wizard:
1. Pick action type: `UpdateMarketParams` / `ListMarket` /
   `DelistMarket` / `ParameterChange` / `ValidatorRegistration`.
2. Fill action-specific fields. Validate on the client (e.g.
   `tick_size` must be a positive FixedPoint).
3. Write title + description (plain text, no markdown yet).
4. Preview: show the exact `ProposalAction` payload + computed
   `actionHash`.
5. Confirm & sign → `SubmitProposal { title, description, action }`.

~250 lines.

**7b. proposal-hash.ts:** Mirror `hash_proposal_action` from Rust.
**Critical:** each `ProposalAction` variant is encoded canonically
(custom byte format per `canonical_bytes()` in
`crates/torus-types/src/lib.rs:477-513`), then `keccak256`'d. Do NOT
use `JSON.stringify` + hash — the node computes on the canonical bytes,
not JSON. Verify by generating a test vector on the Rust side and
asserting the TS helper produces the same bytes and hash.

Add 5 fixtures to the EIP-712 vector suite, one per `ProposalAction`
variant.

**Verify:** Submit an `UpdateMarketParams` proposal on devnet, see it
appear in `useProposals()` with matching `action` field. Vote on it
from another address.

---

## Step 8: Open interest on trade page

**File:** `components/trading/market-header.tsx` (extend Phase 1's)

Add two columns to the trade-page header:
- **Long OI:** from `useOpenInterest(marketId)` — display formatted FixedPoint + "long"
- **Short OI:** same for short

~20 lines of additive JSX. Polling 5s (already in hook).

**Verify:** open trade page, see Long/Short OI values; place a limit
order that fills, see OI tick up.

---

## Step 9: Navigation polish

**File:** `components/navbar.tsx` (extend Phase 1's)

- Enable `/staking` and `/governance` nav links (Phase 1 had them
  disabled as Phase-2 placeholders).
- Active-link highlighting on both new routes.
- Add a "Connected as Validator" badge next to the wallet address if
  the connected address appears in `useValidators()`.

~30 lines changed.

**Verify:** click Staking → routes to `/staking`, active underline
updates. Same for Governance.

---

## Step 10: Format helpers for staking/governance

**File:** `lib/format.ts` (extend Phase 1's)

```typescript
// U256 wei → human-readable TRS with dp decimal places
export function formatTrs(hexU256: string, dp: number = 2): string
// hex u256 → BPS percent (e.g. "5.0%")
export function formatBps(hexBps: string): string
// epoch count + blocks-per-epoch + block-time → "3d 4h 12m"
export function formatUnbondingTime(
  unlockEpoch: number, currentEpoch: number,
  epochLengthBlocks: number, blockTimeMs: number
): string
// proposal status → colored badge string
export function formatProposalStatus(s: Proposal['status']): { label: string; color: string }
```

~80 lines.

**Verify:** correct rounding, negative handling, extreme-value behavior
(quorum 0%, vote tallies in U256 that exceed 2^53 — use bigint
arithmetic, not Number).

---

## Step 11: Blocked-feature stubs (do not implement, just scaffold)

**Files:**
- `app/leaderboard/page.tsx` — "Coming soon — awaiting indexer API"
- `components/portfolio/equity-curve.tsx` — disabled chart with "awaiting indexer"
- `components/referrals/referral-dashboard.tsx` — "Coming soon — awaiting protocol"

One-line placeholders so future PRs have obvious landing sites. Add
entries to the `Navigation` component as disabled links (greyed).

Write separate writing-plan files when the upstream dependencies land:
- `writing-plan-leaderboard-indexer.md`
- `writing-plan-portfolio-history.md`
- `writing-plan-referral-system.md` (+ `tech-req-referral-protocol.md`
  for the Rust side)

---

## Dependency chain

```
Phase 1 shipped + green fixtures
    │
Step 1 (EIP-712 dispatcher extension + fixtures) ─┐
Step 2 (RPC client extension) ────────────────────┤
                                                   │
Step 3 (React Query hooks) ────────────────────────┤
                                                   │
                                          Step 4 (Staking) ──┐
                                          Step 5 (Validator admin — optional)
                                          Step 6 (Gov list + detail) ──┐
                                                                        │
                                          Step 7 (Submit proposal — depends on Step 6) ┤
                                          Step 8 (Open interest — independent)
                                          Step 9 (Navigation)
                                          Step 10 (Format helpers — can be parallel)
                                                                        │
                                                   Step 11 (blocked stubs — last)
```

**Critical path:** Steps 1 + 2 → 3 → 4 (staking) or 6 (governance) in
parallel. Step 7 gates on Step 6.

**Parallelizable:** After Step 3, a frontend dev can work on staking
(Step 4) while another works on governance (Steps 6-7). Steps 8, 9, 10
are independent polish.

---

## Test matrix (per-step)

| Step | Unit tests | Integration (devnet) |
|---|---|---|
| 1 | EIP-712 fixture suite +7 variants | N/A |
| 2 | RPC response type assertions | `npm run probe-rpcs` hits each endpoint |
| 3 | Hook polling cadence + invalidation | N/A |
| 4 | Amount conversion (TRS ↔ U256) | Delegate/undelegate/claim end-to-end |
| 5 | — | Commission change, rotate key |
| 6 | — | Vote on an active proposal |
| 7 | `hashProposalAction` fixtures | Submit one of each proposal type |
| 8 | — | OI ticks up after a fill |
| 9 | — | Click each nav link, verify active state |
| 10 | `formatTrs`, `formatBps`, `formatUnbondingTime` | — |

**CI gate:** all unit tests green + at least one manual browser-test
transcript attached to the PR (record user flow with `playwright codegen`
or similar; actual playwright suite can wait for Phase 2.5).

---

## Quality guardrails (inherit from Phase 1 prompt)

- No `any`. Use `unknown` + narrowing.
- Browser-safe primitives only in component code (no Node `Buffer`).
- No premature abstractions — if two modals share 3 fields, keep them
  separate until the third appears.
- Permanent-stake confirmation MUST require explicit checkbox — this is
  irreversible on-chain; an accidental click is a real user-harm vector.
- Treasury and governance numbers use bigint arithmetic throughout
  (U256 amounts easily exceed 2^53 once multiplied by vote power).
- EIP-712 fixture suite is the gate — no variant ships without a green
  test vector on both Rust and TS sides.

---

## Verification of verified claims

The following were confirmed before authoring this plan by reading the
Rust source (prevents the PRD-vs-reality drift that hurt Phase 1 review):

1. All seven Phase 2 NativeAction variants have EIP-712 hashes in
   `crates/torus-types/src/eip712.rs` (`:319, :329, :339, :348, :371,
   :482` — listed in NativeAction dependency map above).
2. All seven Phase 2 RPC endpoints are implemented in
   `crates/torus-rpc/src/torus.rs` (`:488, :574, :634, :677, :697, :973`
   — listed in RPC dependencies map above).
3. `SubmitProposal` / `SubmitOraclePrices` / `ListMarket` /
   `UpdateMarketParams` use **pre-hashed `bytes32` fields** (not nested
   EIP-712 types). Confirmed at `eip712.rs:357-369, :388-397, :450-458,
   :461-469`.
4. Nonce is milliseconds (not nanoseconds). Confirmed via
   `NONCE_WINDOW_MS = 60_000` at `eip712.rs:26` and
   `current_time_ms` usage at `:622`.
5. `Signature` wire shape is `{v: number, r: number[32], s: number[32]}`.
   Confirmed by serde probe run 2026-04-16 (memory id `a4acab3f91b9178e`).
6. `U256` amounts in `Delegate`/`Undelegate`/`PermanentStake`/
   `TopUpSelfStake` are wei-scaled 18-decimal TRS, NOT FixedPoint 10^8.
   Confirmed at `lib.rs:308-339`.

Implementer: re-verify before coding each step. Rust is always the
source of truth; this plan is secondary.

---

## Out of scope (explicitly)

- Staking delegation to multiple validators atomically (no batch action exists)
- Proposal execution transaction (handled automatically on-chain after passage)
- Historical vote queries by address (requires new RPC or indexer)
- Delegation NFTs or tokenized stake positions (not in protocol)
- Slashing-event display (needs new RPC `torus_getSlashHistory`)
- MEV tips or priority fees for NativeAction submission (not implemented)

Any of the above becoming a requirement bumps that feature into a Phase 2b
or Phase 3 plan with its own tech-req + writing-plan pair.
