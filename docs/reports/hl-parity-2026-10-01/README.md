# Hyperliquid product-parity sprint — synthesis (2026-10-01)

**Code under review:** `main` @ `a6917de`
**Method:** four time-boxed, read-only explore rounds, 220 agents total (110 Opus, 110 Fable). Each question was answered by one Opus agent and one Fable agent working independently.
**Companion files:** [findings.md](findings.md) (every topic, both models side by side, evidence, risk and fix design) · [raw.json](raw.json) (structured output)
**Roadmap:** the product-parity track built from this sprint is in [`docs/Torus-hyperBFT-roadmap.md`](../../Torus-hyperBFT-roadmap.md), section "Product-parity track".

---

## 1. Bottom line

The matching engine works: price-time CLOB; GTC/IOC/FOK/ALO; self-trade prevention that expires the maker; one signature per batch; deterministic cancels-first ordering. The exchange around it does not work yet. Most of what makes Hyperliquid a working perps venue is missing, stubbed, or **written but never called**: liquidation, oracle aggregation, margin configs, fees, funding, the market registry and unbonding release. Several gaps are **money-losing bugs**, not missing features, and they must be fixed before any feature work.

The existing roadmap covers performance and liveness only. Before this sprint it had no product track.

## 2. How the sprint ran

| Round | Agents | Box | Scope |
|---|---|---|---|
| 1 | 20 | ~9 min | 10 broad areas (order types, funding/mark, fees, margin/liquidation, vaults/accounts, spot/listing, HyperEVM, API/data, validators/staking, bridge/transfers) |
| 2 | 40 | ~4 min | 20 narrow questions, one gap each |
| 3 | 80 | ~4 min | 18 adversarial re-checks of single-model claims (agents told to *refute* first) + 22 uncovered areas |
| 4 | 80 | ~2.5 min | 20 concrete fix designs with LOC estimates + 20 uncovered areas |

All 220 agents returned and none failed. The evidence comes from what agents report they read. Items marked ✅ were re-checked by hand against `main`.

## 3. Tier A — safety bugs (fix before any parity feature)

All of these were confirmed by both models. In round 3, the agents were told to refute each one first and could not. In the roadmap, A3 and A6 are fixed by P1 (the per-block driver and market registry), because the fix *is* building that driver. All the other items are P0.

| # | Bug | Evidence | Fix size |
|---|---|---|---|
| A1 ✅ | **Anyone can modify anyone's order.** Modify also skips matching, tick/lot and ALO checks (it can leave a crossed or zero-qty book), and it mutates the book *before* the margin check, with no rollback. | `native_executor.rs:3254` dispatch drops `sender`; `exec_modify_order` `:5359`; `order_book.rs:748-772` | M, ~120 LOC + tests |
| A2 ✅ | **Triggered stop fills are dropped.** `trigger_stops` discards `place_order`'s `result.fills`, so book and positions diverge. | `order_book.rs:1316` | M, 150–400 LOC |
| A3 ✅ | **Liquidation and oracle aggregation never run.** `aggregate_oracle_prices` and `run_liquidation_checks` have zero callers. | `native_executor.rs:5904`, `:5923` | L, 350–700 LOC |
| A4 | **Positions hold no collateral**, and withdraw/transfer ignore open positions. Reserved margin returns to `available` on fill. | lockbox `withdraw_from_native` checks only `available` | M, 120–180 LOC |
| A5 | **Market orders reserve zero margin**, and `check_margin_at_match` is unused. The check is missing in all three placement paths (serial, parallel phase-2, single). | `native_executor.rs:3757`, `:4004`, `:4917`; `margin.rs:151` | M–L, 150–450 LOC |
| A6 | **`margin_configs` is always empty**, so every market gets a flat 20x and maintenance margin is never enforced. | `native_executor.rs:1842`; `unwrap_or(20)` at `:4808` and in cancel/modify paths | L |
| A7 | **Lockbox decimal mismatch** (native 8-dec ↔ EVM 18-dec copied raw, a 1e10 factor). Lockbox precompile writes may also be clobbered by the revm account cache. | `lockbox.rs:228-247` | S (decimals) / M (cache) |
| A8 | **Session-key expiry is never enforced in blocks.** Block timestamps are in *seconds*, session expiry in *milliseconds*. Only RPC ingress checks correctly. | `eip712.rs:832`, `:1149`; `app.rs:1680`, `:4635`; `validator.rs:370` | S |
| A9 ✅ | **Undelegated funds are locked forever.** `process_unbonding` is only called from tests. | `staking.rs:189` | M, ~60 LOC + tests |

Other confirmed bugs (both models):

- **EVM gas fees credited twice:** revm pays coinbase, then `distribute_fees` re-mints the revenue.
- **Validator inflation pays the self-stake share to delegators.**
- **Equivocation slashing comes from each node's local view**, not from consensus-ordered evidence, so state can diverge.
- **The epoch boundary has two staking writers**: the consensus thread and the exec thread.
- **CoreWriter `placeOrder` returns a synthetic id** that never matches the real order id. Stop types 2 and 3 run as plain Limit orders.
- **Read precompiles cost a flat 2,600 gas for unbounded output** (DoS risk).
- **The 60 s nonce window is enforced only at the mempool**, and the consumed-nonce CF grows forever.
- **Governance `ParameterChange` is a dead write**, and governance `ListMarket` always writes `market_id 0`.
- **Unknown `market_id`s are rejected only at RPC.** Other paths auto-create a `tick=lot=1` book.
- **Stop orders bypass the 200-orders cap** and cannot be cancelled by id.
- **Stops trigger on last trade price.** With no price bands, a 1-lot print can hunt stops on a thin book.
- **Block timestamps are not validated at all** (no parent-monotonic or drift bound). The proposer's clock is trusted.
- **Full-scope session keys can perform validator and governance actions:** rotate the validator key, vote, list markets, submit oracle prices. Expired sessions are never pruned and still count toward the 5-session cap.
- **The EIP-712 domain is a compile-time constant** (chainId 7778, no network field), so actions replay across testnet, devnet and mainnet within the nonce window.
- **Oracle submissions are free and unbounded** in market ids, and junk rows are never pruned.

**Why these shipped:** no integration test runs modify or a triggered stop through the real executor, and the load benches only send GTC limits (`bench-trading-semantics`, `test-coverage-trading`). The Astra round-2 audit (`docs/audits/astra-round2-2026-09-24.md`) has **no finding fixed on main**. Opus checked 15 of its 26 candidates in code; Fable counted 20 real defects and found all open.

## 4. Tier B — core exchange mechanics (absent, stubbed, or unwired)

| Gap | Status | Fix-design size (round 4) |
|---|---|---|
| Trading fees (maker/taker, `total_native_fees` never incremented); no tiers, rebates, staking discount, referrals, builder codes | absent | 200–400 LOC (flat fees) |
| Funding engine (premium index, hourly settlement, cumulative index; only `max_funding_rate_bps` exists) | absent | 600–850 LOC + tests |
| Mark price (HL median of oracle+basis, book, external); everything uses the raw oracle | absent | 450–900 LOC |
| Validator oracle publisher (nothing fetches CEX prices or submits `SubmitOraclePrices`) | absent | sidecar |
| Price bands, market-order slippage cap, OI caps, max order notional | absent | 250–650 LOC |
| `reduce_only` enforcement (placement, match clamp, auto-cancel, stop re-check) | unwired | 350–650 LOC |
| Market registry: List/Delist/UpdateMarketParams are no-op stubs; governance lists id 0 | stub | 500–1000 LOC |
| Isolated margin, `updateLeverage`, `updateIsolatedMargin` (data model exists in torus-core) | unwired | 650–1000 LOC |
| Liquidation via book + backstop vault; ADL does not cover the deficit and its result is discarded; insurance fund never spent or exposed | partial | XL |
| Cross liquidation applies one market's config to every position | buggy | S |

## 5. Tier C — product surface

- **Orders:** no TP/SL (`tpsl` flag, `normalTpsl`/`positionTpsl` grouping, mark-price trigger), TWAP or scale orders, 128-bit `cloid` with cancel/modify-by-cloid, `batchModify`, `expiresAfter`, or `scheduleCancel` dead-man switch. Design sizes: TP/SL 700–1300 LOC, TWAP 500–650, cloid 600–1300, `scheduleCancel` ~250–350.
- **Accounts:** no subaccounts, vaults (HLP or user), or multisig. Session keys are the agent-wallet analogue but differ: ed25519 only, unnamed, 24 h cap, Full scope too broad.
- **Assets:** no native spot (book, token registry, HIP-1/2/3, `spotSend`/`usdSend`), no stablecoin collateral (margin and PnL are in TRS), and no external-chain bridge (validator-signed withdrawals, dispute period).
- **API:** the WebSocket has only `newTrades` and `userFills`. Missing: `l2Book`/`bbo`/`allMids`/candles/`orderUpdates`/`userEvents`; `orderStatus`/`historicalOrders` (order lifecycle isn't persisted); `clearinghouseState`/`metaAndAssetCtxs`; funding endpoints; an HL-compatible `/info` + `/exchange` shim. Order submission returns only a hash (HL returns resting/filled/error). Rate limits are capacity caps, not volume-earned budgets.
- **HyperEVM:** read precompiles cover a fraction of HL's L1Read; there is no ERC20↔Core token linking, no dual-block architecture, and CoreWriter's action set is narrow. The JSON-RPC handles the basic wallet flows but lacks filters, `eth_subscribe` and historical state.
- **Clients:** there is **no trading UI**. The planned `torus-trading-app` repo was never created. Three of the PRD's order-entry features (leverage slider, cross/isolated toggle, TP/SL) are blocked by protocol gaps, not UI work. The only client is the Rust CLI `tools/wallet`.
- **Infra:** `SnapshotManager` is not wired into commit (no state sync for new nodes). There is **no protocol-upgrade mechanism**: no version or feature gating by height, and nothing like hl-visor. The validator set is governance-whitelisted, not permissionless. Downtime jailing is not automated (`DowntimeTracker`/`DoubleSignDetector` are unused).
- **Ops:** trading telemetry is three unlabeled counters. The matching proptest is one 126-line file; margin and liquidation invariants are untested.

## 6. Healthy areas (both models agree)

- Order-book data structures: `BTreeMap<FixedPoint, VecDeque<Order>>` per side plus an `order_index` HashMap.
- TIF semantics and self-trade prevention match HL.
- `FixedPoint` math: checked i128 add/sub, i256 intermediate for mul/div. Realized PnL and fill transitions are correct.
- The proposer **cannot reorder** native actions, because every node re-sorts deterministically (`app.rs:1812`). It can still *censor*: validators do not check which actions were selected.
- The non-validator RPC node role (`--rpc-only`) follows consensus and serves RPC/WS.
- Execution is largely determinism-hardened (sorted flushes, BTree books, no floats or wall clock in state).

## 7. Where the models disagreed

Most of the 36 label differences are wording only (`absent` vs `design`, `high` vs `critical`). The substantive ones:

- **exec-determinism.** Opus found no hazard. Fable flags `exec_cancel_all`/`exec_cancel_all_run` iterating `ctx.order_books.keys()` unsorted (RandomState). Opus judged the same loop commutative because it only sums margin and applies one clamped release. **Unverified; worth a test that runs cancel-all on two nodes with different hash seeds and compares state roots.**
- **collateral-asset.** Opus rates it a partial gap, Fable a critical bug. Both describe the same 1e10 lockbox mismatch (A7). The disagreement is only over whether "no stablecoin" counts as a bug or a product decision.
- **exec-timestamps.** Fable calls it a confirmed bug because of the seconds/ms session check (A8). Opus files that under A8 and rates unvalidated block timestamps as a partial gap.
- **node-roles.** Opus wants sentry and reserved peers; Fable considers the role complete apart from a doc fix.

## 8. Limitations

- Agents were time-boxed (2.5–9 min) and read-only, so nothing was compiled or tested.
- Hyperliquid behaviour is from model knowledge plus `research/hyperliquid-deep-dive.md`. Exact HL constants (band widths, OI-cap behaviour, scheduleCancel limits, unbonding periods) need checking against current HL docs before they become specs.
- Hand-verified (✅): A1, A2, A3 and A9. Every other row rests on two independent agents agreeing, with file:line evidence in [findings.md](findings.md).
- LOC estimates come from round-4 design agents. Treat them as relative sizing, not commitments.
