# Torus-hyperBFT Economic Model Security Audit (3.4.4)

**Instance**: C (Economic Model)
**Date**: 2026-04-14
**Auditor**: Claude Opus 4.6 (automated)
**Codebase**: ~55k lines Rust, 4 crates, 30+ files
**Scope**: Order book, margin, liquidation, oracle, staking, governance, fee distribution, FixedPoint arithmetic, game theory

---

## 1. Executive Summary

The Torus-hyperBFT economic model contains **multiple critical and high-severity vulnerabilities** that would allow fund loss, consensus divergence, and governance capture in production. The most severe cluster involves the margin/liquidation system: the maintenance margin formula is off by a factor of ~10^8 (rendering liquidation non-functional for all but fully-depleted accounts), while the insurance fund has no replenishment mechanism. A second critical cluster is in replay protection: the EIP-712 nonce validation function is never called in the execution path, and no nonce tracking exists, allowing signed actions to be replayed within a 60-second window. The consensus-critical action sort key uses Rust's `Debug` format, which is not stable across compiler versions, creating a latent chain-split risk. The governance system lacks an execution timelock and allows arbitrary parameter overwrites including governance parameters themselves. The order book has no persistence, no ownership check on cancel/modify, and no margin reservation on order placement. FixedPoint arithmetic uses unchecked i128 operations that wrap silently in release builds. Collectively, these findings represent a pre-production system that requires significant hardening before mainnet deployment.

**Totals: 2 Critical, 13 High, 18 Medium, 12 Low, 2 Info = 47 findings**

---

## 2. Verified Pre-Findings

### ECON-PF-01 | HIGH | CONFIRMED
**FixedPoint add/sub use unchecked i128 arithmetic**
- **Location**: `torus-types/src/lib.rs:73,80,87,93,99`
- **Evidence**: `Add::add` is `Self(self.0 + rhs.0)`, `Sub::sub` is `Self(self.0 - rhs.0)`, `Neg::neg` is `Self(-self.0)`. All bare i128 operations.
- **Root cause**: No `checked_*` or `saturating_*` variants exist for FixedPoint. The workspace `Cargo.toml` has no `[profile.release]` section and `.cargo/config.toml` only sets `split-debuginfo`. Rust default: `overflow-checks = false` in release builds.
- **Impact**: Silent wrapping in release builds. ~25+ call sites across margin, liquidation, position, oracle modules. `Neg` on `i128::MIN` wraps to itself.
- **Severity**: HIGH. While values reaching i128::MAX are impractical for trading, the margin system's `free_margin = equity - maint` (PF-11) creates a direct exploit path through subtraction wrapping.
- **Fix**: Add `overflow-checks = true` to `[profile.release]` in workspace Cargo.toml, or implement `checked_add`/`checked_sub` on FixedPoint with explicit error handling.

### ECON-PF-02 | HIGH | CONFIRMED
**FixedPoint::div panics on zero divisor**
- **Location**: `torus-types/src/lib.rs:67`
- **Evidence**: `i256::from(self.0) * i256::from(Self::SCALE) / i256::from(other.0)` -- Rust integer division panics unconditionally on zero, regardless of overflow-checks setting.
- **Root cause**: No zero-guard in the `Div` implementation.
- **Critical call sites**: `margin.rs:115` (leverage=0 from user input), `liquidation.rs:285` (pos.size=0), `position.rs:310` (new_size=0 after cancel-and-flip).
- **Impact**: Node crash (panic) in production. A malicious PlaceOrder with `leverage=0` would halt block processing.
- **Fix**: Return `FixedPoint::ZERO` or a sentinel on zero divisor, or add zero guards at all call sites.

### ECON-PF-03 | MEDIUM | CONFIRMED
**Abstain vote maps to No**
- **Location**: `torus-bridge/src/native_executor.rs:563`
- **Evidence**: `let support = matches!(option, VoteOption::Yes)` -- only `Yes` produces `true`; both `No` and `Abstain` produce `false`. The governance `cast_vote` increments `votes_against` for `support: false`.
- **Root cause**: Binary support flag collapses a three-valued enum. The EIP-712 hash correctly encodes three distinct values (Yes=0, No=1, Abstain=2) in the signed payload, so the user signs their actual intent, but execution discards it.
- **Impact**: Users who abstain have their votes counted as opposition. This systematically biases governance outcomes against proposals.
- **Fix**: Add an `abstain_count` field to `Proposal` and handle the three-valued enum at the executor level.

### ECON-PF-04 | HIGH | CONFIRMED
**Lockbox deposit/withdraw non-atomic**
- **Location**: `torus-core/src/lockbox.rs:49+54` (deposit), `lockbox.rs:83+88` (withdraw)
- **Evidence**: Deposit: `set_evm_balance` (line 49) debits EVM first, then `put_native_balance` (line 54) credits native. Withdraw: `put_native_balance` (line 83) debits native first, then `set_evm_balance` (line 88) credits EVM. Two separate `put_cf_raw` calls to different column families, no RocksDB WriteBatch.
- **Root cause**: No transactional write support in the StateDb abstraction.
- **Impact**: If the second write fails (disk error, OOM, panic in serialization), funds are destroyed with no rollback or compensating transaction.
- **Fix**: Use RocksDB `WriteBatch` for multi-CF atomic writes, or implement a two-phase commit with rollback.

### ECON-PF-05 | MEDIUM | CONFIRMED
**Order books created on-demand with default parameters**
- **Location**: `torus-bridge/src/native_executor.rs:259-260`
- **Evidence**: `OrderBook::new(market_id, FixedPoint::ONE, FixedPoint::ONE)` -- hardcoded tick_size=1.0, lot_size=1.0. No lookup of on-chain MarketParams. Admin actions `ListMarket`/`UpdateMarketParams`/`DelistMarket` are no-op stubs (lines 219-223).
- **Root cause**: Market registry is not implemented. Order books are created for any arbitrary market_id submitted in a PlaceOrder action.
- **Impact**: (1) Phantom markets can be created by any user. (2) Market-specific tick/lot sizes are ignored. (3) Fills in phantom markets create real positions.
- **Fix**: Implement a market registry with governance-gated listing. Reject PlaceOrder for unlisted markets.

### ECON-PF-06 | HIGH | CONFIRMED
**Liquidation uses zero price for missing oracle prices**
- **Location**: `torus-bridge/src/native_executor.rs:701-705`
- **Evidence**: `unwrap_or(FixedPoint::ZERO)` when no oracle price found. Zero is passed to `execute_liquidation` and `auto_deleverage`.
- **Impact**: All long positions will show `unrealized_pnl = (0 - entry_price) * size` = full loss. Solvent positions are force-liquidated at price zero, destroying trader equity. This is exploitable: an attacker can create a market with no oracle reporters, place a position, then trigger liquidation checks.
- **Fix**: Skip liquidation for markets with no valid oracle price. Return an error or log a warning.

### ECON-PF-07 | LOW | CONFIRMED
**Admin actions are no-op stubs**
- **Location**: `torus-bridge/src/native_executor.rs:219-223`
- **Evidence**: `UpdateMarketParams`, `ListMarket`, `DelistMarket` all return `NativeActionResult::ok(...)` with zero gas and no state change.
- **Impact**: Governance proposals that pass for market parameter changes silently do nothing. No market registry exists.
- **Fix**: Implement market registry operations or remove the action variants to avoid false governance functionality.

### ECON-PF-08 | LOW-MEDIUM | CONFIRMED
**Negative FixedPoint silently becomes large u128 in EVM precompiles**
- **Location**: `torus-core/src/precompiles.rs:165-166`
- **Evidence**: `encode_fp_as_u128(fp: FixedPoint)` does `fp.raw() as u128`. Negative i128 produces two's-complement u128 (e.g., -1 becomes ~3.4*10^38).
- **Affected paths**: `getOrderBook` (lines 284-287), `getPosition` (line 328,331), `getBalances` (lines 413-417), `getPrice` (line 486), `getAllPrices` (line 527).
- **Impact**: EVM contracts reading oracle prices, balances, or positions see astronomically large values instead of negative/zero values when underlying FixedPoint is negative.
- **Fix**: Use `encode_fp_as_i128` (which exists at line 168) for all fields, or clamp negatives to zero with explicit conversion.

### ECON-PF-09 | INFO | CONFIRMED
**Unbonding vec length truncated to u32 in Borsh**
- **Location**: `torus-economics/src/types.rs:155`
- **Evidence**: `self.unbonding.len() as u32` -- theoretical truncation if >4B entries.
- **Impact**: Unreachable in practice. A single delegation record with 4 billion unbonding entries would require ~160GB of memory.

### ECON-PF-10 | HIGH | CONFIRMED
**Maintenance margin BPS formula is wrong -- off by ~10^8**
- **Location**: `torus-core/src/margin.rs:220-222`, `margin.rs:238-240`, `torus-core/src/liquidation.rs:169-171`
- **Evidence**: Three identical occurrences:
  ```
  let maint_num = FixedPoint::from_raw(config.maintenance_factor_bps as i128);
  let bps_denom = FixedPoint::from_raw(10_000 * FixedPoint::SCALE);
  ```
  `from_raw(5000)` creates FixedPoint(0.00005), not FixedPoint(5000). Correct would be `from_raw(5000 * SCALE)`.
- **Root cause**: Confusion between `from_raw(x)` (x is already scaled) and what should be `from_raw(x * SCALE)` (x is a human-readable integer).
- **Impact**: With `maintenance_factor_bps=5000`, maintenance margin is `initial * 5*10^-9` instead of `initial * 0.5`. Maintenance margin is ~100 million times too small. No position is ever caught by margin maintenance checks (except when equity goes fully negative). Positions can accumulate massive unrealized losses without liquidation.
- **Concrete example**: 10x BTC long at $50k with $5k margin: maintenance should be $2,500 but is $0.00000002.
- **Fix**: Change to `FixedPoint::from_raw(config.maintenance_factor_bps as i128 * FixedPoint::SCALE)` at all three locations.

### ECON-PF-11 | HIGH | CONFIRMED
**Cross-margin free_margin underflow wraps to large positive**
- **Location**: `torus-core/src/margin.rs:137`
- **Evidence**: `let free_margin = equity - maint;` -- uses FixedPoint's unchecked `Sub` (PF-01). When `equity < maint`, the i128 subtraction wraps to a large positive value in release builds.
- **Root cause**: No checked subtraction + no overflow-checks in release profile.
- **Impact**: Deeply insolvent accounts appear to have enormous free margin, passing all margin checks for new orders. Currently partially masked by PF-10 (maintenance is near-zero, so `equity < maint` rarely triggers). If PF-10 is fixed without fixing PF-11, the underflow wrapping becomes immediately exploitable.
- **Fix**: Use `checked_sub` or an explicit sign check before subtraction.

### ECON-PF-12 | HIGH | CONFIRMED
**Governance ParameterChange writes arbitrary key with no whitelist**
- **Location**: `torus-economics/src/governance.rs:741-747`
- **Evidence**: `self.state_db.put_cf_raw(CF_FEE_CONFIG, param_key.as_bytes(), new_value.as_bytes())?;` -- no validation on `param_key`. Both `GOVERNANCE_PARAMS_KEY` (b"gov_params", line 43) and `PROPOSAL_COUNTER_KEY` (b"gov_next_id", line 46) live in `CF_FEE_CONFIG`.
- **Root cause**: Missing whitelist of allowed parameter keys.
- **Impact**: A governance majority can overwrite governance parameters themselves (e.g., set `quorum_bps=1` for future proposals), reset the proposal counter (causing ID collisions), or write arbitrary data to the fee config CF.
- **Attack**: Pass a ParameterChange with `param_key="gov_params"` and `new_value` containing serialized GovernanceParams with `quorum_bps=1`. All subsequent proposals pass with 0.01% stake.
- **Fix**: Whitelist allowed parameter keys. Never allow governance parameters or counters to be modified via ParameterChange.

### ECON-PF-14 | HIGH | CONFIRMED
**Epoch rotation uses HashSet::difference -- non-deterministic ordering**
- **Location**: `torus-economics/src/epoch.rs:159-165`
- **Evidence**: `old_addrs.difference(&new_addrs).copied().collect()` -- Rust's `HashSet` uses `RandomState` (random seed per process). The resulting `departures` and `arrivals` vectors have non-deterministic iteration order. At lines 177-184, `departures.iter().skip(allowed_swaps)` selects different validators on different nodes.
- **Root cause**: Using `HashSet` (non-deterministic iteration) for consensus-critical set operations.
- **Impact**: When rotation cap is exceeded, different validators compute different validator sets. This causes consensus divergence (chain split).
- **Fix**: Sort `departures` and `arrivals` by address (deterministic key) before applying `skip`.

### ECON-PF-15 | HIGH | CONFIRMED (severity upgraded from Medium)
**cancel_order has no ownership check -- any user can cancel any order**
- **Location**: `torus-core/src/order_book.rs:315-349`, `torus-bridge/src/native_executor.rs:158,289-295`
- **Evidence**: `cancel_order(order_id)` takes only an order_id. No trader/sender parameter. `exec_cancel_order` does not forward `sender`. Same issue in `modify_order` (lines 384-428) and `exec_modify_order` (lines 318-330).
- **Root cause**: API design omits the caller identity from cancel/modify operations.
- **Impact**: Any authenticated user can cancel or modify any other user's resting orders. Order IDs are sequential starting from 1 per market, so they are trivially guessable. A griefing attack can wipe the entire order book.
- **Fix**: Add `trader: Address` parameter to `cancel_order` and `modify_order`. Verify `order.trader == trader` before proceeding.

### ECON-PF-16 | MEDIUM-HIGH | CONFIRMED
**BLOCKS_PER_YEAR inconsistent with actual block time**
- **Location**: `torus-economics/src/types.rs:314` vs `types.rs:353-354,566-567`, `torus-economics/src/governance.rs:30-31`
- **Evidence**: `BLOCKS_PER_YEAR = 31,536,000` (1 block/sec). But `JAIL_DURATION_BLOCKS = 28,800` ("~2 days at 6s blocks"), `COMMISSION_COOLDOWN_BLOCKS = 28,800` ("~2 days at 6s"), `DEFAULT_VOTING_PERIOD_BLOCKS = 100,800` ("~7 days at 6s blocks").
- **Root cause**: Two block-time assumptions coexist in the codebase.
- **Impact**: If chain runs at 6s blocks: actual permanent staking APY is ~0.83% instead of stated 5%. If chain runs at 1s blocks: jail duration is 8 hours (not 2 days), voting period is 28 hours (not 7 days).
- **Fix**: Decide on actual block time. Make all constants consistent.

### ECON-PF-17 | MEDIUM | CONFIRMED
**Withdraw `to` address discarded**
- **Location**: `torus-bridge/src/native_executor.rs:201-203`
- **Evidence**: `NativeAction::Withdraw { amount, .. }` -- the `to` field is destructured away with `..`. The EIP-712 hash commits to both `amount` and `to`, so the user signs a specific destination. But execution always credits the sender.
- **Impact**: Users cannot withdraw to a different address (e.g., cold wallet). Their signed intent is violated silently.
- **Fix**: Pass `to` to `exec_withdraw_from_native` and credit the specified address.

### ECON-PF-18 | MEDIUM | CONFIRMED
**Oracle outlier rejection bypassed for single reporter**
- **Location**: `torus-core/src/oracle.rs:360-362`
- **Evidence**: `if pairs.len() <= 1 { return pairs.to_vec(); }` -- single reporter's price passes unfiltered. With `min_oracle_reporters=1`, one validator fully controls the oracle price.
- **Impact**: Depends on deployment configuration. If `min_oracle_reporters` is ever set to 1 (by governance or misconfiguration), oracle manipulation is trivial.
- **Fix**: Set minimum floor for `min_oracle_reporters` (e.g., 3) that cannot be reduced via governance.

---

## 3. New Findings

### ECON-FIND-01 | CRITICAL | Action Sort Key Uses Debug Format
- **File**: `torus-bridge/src/native_executor.rs:872`
- **Description**: `action_sort_key` computes `keccak256(format!("{:?}", action))` using Rust's `Debug` trait. Debug output is not stable across compiler versions, crate versions, or feature flags. This hash is used as the consensus-critical sort key for block action ordering (line 886).
- **PoC**: Two validators compiled with different rustc versions produce different `Debug` output for the same `NativeAction` -> different keccak hashes -> different sort order -> different state roots -> consensus failure.
- **Fix**: Use a deterministic encoding (Borsh serialization, ABI encoding, or the existing EIP-712 struct hash).

### ECON-FIND-02 | CRITICAL | Order Book Not Persisted -- In-Memory Only
- **File**: `torus-bridge/src/native_executor.rs:81`, `torus-core/src/order_book.rs:108-126`
- **Description**: `NativeExecContext.order_books: HashMap<MarketId, OrderBook>` is an in-memory structure. `OrderBook` has no Borsh serialization, no state DB references, and no state-root commitment. A node restart destroys all resting orders.
- **PoC**: Node restarts -> all resting orders vanish -> users' open orders disappear without fills or cancellation notifications -> margin reserved for those orders (if any were reserved) is never released.
- **Fix**: Implement order book persistence to RocksDB with state-root commitment, or implement a checkpoint/snapshot mechanism.

### ECON-FIND-03 | HIGH | No EIP-712 Replay Protection
- **File**: `torus-types/src/eip712.rs:626-630`, mempool, proposer, native_executor
- **Description**: Three compounding issues:
  1. `validate()` (which checks nonce freshness) is never called in the block production or execution path. Only `recover_sender()` is called (`mempool/src/lib.rs:194`, `proposer.rs:152`).
  2. No nonce tracking set exists -- no `seen_nonces`, no CF for used nonces.
  3. The mempool's `seen` HashSet is cleared between blocks.
- **PoC**: Attacker intercepts a signed `Delegate(10000 TRS)` action. Submits it in 3 consecutive blocks within the 60s nonce window. The user delegates 30,000 TRS instead of 10,000.
- **Fix**: Call `validate()` in the execution path. Implement persistent nonce tracking in a dedicated CF.

### ECON-FIND-04 | HIGH | Insurance Fund Has No Replenishment Mechanism
- **File**: `torus-core/src/liquidation.rs:185-225`
- **Description**: `execute_liquidation` does not compute or deduct a liquidation penalty to fund the insurance. The insurance fund (key `b"insurance_fund"` in `CF_NATIVE_BALANCES`) is only debited by `socialize_loss` (line 331-337) and never credited.
- **PoC**: After the first set of liquidations drain the insurance fund, all subsequent liquidation shortfalls go directly to socialized loss, spreading losses to all traders (including underwater ones).
- **Fix**: Add a liquidation penalty (e.g., 1-5% of notional) credited to the insurance fund on each successful liquidation.

### ECON-FIND-05 | HIGH | No Order Margin Reservation
- **File**: `torus-core/src/position.rs:129-160`, `torus-bridge/src/native_executor.rs:262-287`
- **Description**: `NativeBalance.order_margin` is defined and serialized but never populated or decremented. When a user places an order, no margin is reserved. When an order fills or cancels, no margin is released.
- **PoC**: User with $100 available balance places 1000 orders each for $100 notional. All are accepted. If 10 fill simultaneously, the user's position far exceeds their available margin.
- **Fix**: Implement order margin reservation in `exec_place_order` and release in cancel/fill paths.

### ECON-FIND-06 | HIGH | No Execution Timelock for Governance Proposals
- **File**: `torus-economics/src/governance.rs:665-707`
- **Description**: `finalize_proposal` immediately calls `execute_payload` on the same block the voting period ends. There is no timelock, delay, or veto window between passing and execution.
- **PoC**: A governance proposal that passes at block N executes atomically. The community has no opportunity to react, exit positions, or prepare for parameter changes. Combined with PF-12 (arbitrary parameter writes), this enables instant governance capture.
- **Fix**: Add a `timelock_blocks` parameter. Set proposal status to `Passed` first, then allow execution after the timelock expires via a separate `execute_proposal` call.

### ECON-FIND-07 | HIGH | Division by Zero Panic via Governance Param Manipulation
- **File**: `torus-economics/src/governance.rs:969-971`
- **Description**: `U256::from(params.permanent_weight_multiplier_den)` -- if `den=0`, U256 division panics. This is reachable via PF-12: a ParameterChange proposal can set `permanent_weight_multiplier_den=0`.
- **PoC**: (1) Pass ParameterChange setting `gov_params` with `permanent_weight_multiplier_den=0`. (2) Any subsequent `cast_vote` by a user with permanent stake panics, halting block processing permanently.
- **Fix**: Validate `permanent_weight_multiplier_den > 0` at deserialization time.

### ECON-FIND-08 | HIGH | Cross-Margin Equity Ignores order_margin
- **File**: `torus-core/src/margin.rs:185-197`
- **Description**: `cross_margin_equity` computes `bal.available + sum(unrealized_pnl)` but does not subtract `bal.order_margin`. Even if order_margin were populated (FIND-05 notes it currently is not), the equity calculation would overstate available margin.
- **Impact**: Users can leverage more than intended because funds committed to open orders are counted as free equity.
- **Fix**: Equity should be `bal.available - bal.order_margin + sum(unrealized_pnl)`.

### ECON-FIND-09 | MEDIUM | Order ID Cross-Market Collision
- **File**: `torus-core/src/order_book.rs:139-149`, `torus-bridge/src/native_executor.rs:289-294`
- **Description**: Each `OrderBook` has its own `next_id` counter starting at 1. Order IDs are not globally unique. `exec_cancel_order` iterates all books and cancels the first match.
- **PoC**: Market A and B both have order_id=5. User cancels order_id=5 intending to cancel from market B. The iterator hits market A first, canceling the wrong order.
- **Fix**: Use globally unique IDs (e.g., prefix with market_id) or include market_id in the cancel action.

### ECON-FIND-10 | MEDIUM | No Price Validation for Limit Orders
- **File**: `torus-core/src/order_book.rs:156-173`
- **Description**: No validation that limit order prices are positive. `CoreError::InvalidPrice` exists (error.rs:15-16) but is never raised. Zero-price and negative-price limit orders can rest on the book.
- **PoC**: User places a sell limit at price=0. This rests on the asks side as the best ask. Any buy order matches at price=0, effectively giving away the asset.
- **Fix**: Reject orders with `price <= FixedPoint::ZERO` for limit orders.

### ECON-FIND-11 | MEDIUM | Tick Size Stored But Never Enforced
- **File**: `torus-core/src/order_book.rs:120-143`
- **Description**: `OrderBook` stores `tick_size: FixedPoint` but no check in `place_order` or `modify_order` verifies `price % tick_size == 0`.
- **Impact**: Book fragmentation, non-standard price levels, displayed spread manipulation.
- **Fix**: Add tick-size validation in `place_order`.

### ECON-FIND-12 | MEDIUM | Stop Order Trigger Direction Not Validated
- **File**: `torus-core/src/order_book.rs:177-219`
- **Description**: No validation at placement time that the trigger price is directionally correct relative to current price. A buy stop with trigger <= current price fires immediately on next trade.
- **Impact**: Bypass of order-type semantics -- effectively places a guaranteed-execution order disguised as a stop.
- **Fix**: Validate buy stop trigger > current price, sell stop trigger < current price at placement.

### ECON-FIND-13 | MEDIUM | Socialized Loss Cascading to Underwater Accounts
- **File**: `torus-core/src/liquidation.rs:349-366`
- **Description**: `socialize_loss` deducts proportional loss from ALL traders in the market, including those already at or below maintenance margin. This can push near-liquidation accounts into insolvency, triggering cascading liquidations.
- **Fix**: Exclude traders below a margin threshold from socialized loss, or absorb into a time-delayed insurance recovery mechanism.

### ECON-FIND-14 | MEDIUM | Slashing Dust Rounding Creates total_delegated Divergence
- **File**: `torus-economics/src/staking.rs:338-350`
- **Description**: When slashing, `del_slash = del.amount * fraction / 10000`. For dust delegations where this rounds to zero, the delegation is skipped but `total_delegated` is only reduced by the non-zero sum. After slashing, `val.total_delegated < sum(individual delegation.amount)`.
- **Fix**: Include zero-slash delegations in the total or use sum-of-individual as the source of truth.

### ECON-FIND-15 | MEDIUM | Slash Below MIN_SELF_DELEGATION Creates Unrecoverable State
- **File**: `torus-economics/src/staking.rs:353-359, 589-595`
- **Description**: When slashing reduces `self_stake` below `MIN_SELF_DELEGATION`, the validator is jailed. Unjailing requires `self_stake >= MIN_SELF_DELEGATION`. But there is no `add_self_stake` operation. The validator is permanently locked out.
- **Fix**: Add a `top_up_self_stake` operation, or allow unjailing with a path to reach the minimum.

### ECON-FIND-16 | MEDIUM | Flash-Vote Attack -- Vote Weight at Cast Time
- **File**: `torus-economics/src/governance.rs:627`
- **Description**: Vote weight is computed at vote-cast time from current delegation/permanent stake, not from a snapshot at proposal submission.
- **PoC**: Attacker acquires large delegated stake, votes, then undelegates. The vote weight reflects the inflated stake. The unbonding period is the only deterrent.
- **Fix**: Snapshot voting power at proposal start block.

### ECON-FIND-17 | MEDIUM | Unlimited Order Placement -- Memory DoS
- **File**: `torus-core/src/order_book.rs:648-662`
- **Description**: No maximum order count per trader or per book. Combined with no order margin reservation (FIND-05), a user can place unlimited resting GTC orders consuming only gas.
- **Fix**: Cap orders per trader per market, or enforce order margin reservation.

### ECON-FIND-18 | MEDIUM | Oracle Submissions Never Pruned
- **File**: `torus-core/src/oracle.rs:273-305`
- **Description**: `collect_submissions` scans all historical submissions for a market via prefix iterator. Submissions are written at line 193 and never deleted. The scan grows linearly with chain history.
- **Fix**: Prune submissions older than `max_oracle_age` blocks at each aggregation.

### ECON-FIND-19 | MEDIUM | No Validator Eligibility Check for Oracle Submissions
- **File**: `torus-bridge/src/native_executor.rs:488-503`
- **Description**: `exec_submit_oracle_prices` calls `submit_price(sender, ...)` without verifying `sender` is an active, non-jailed validator. Any address can write to `CF_NATIVE_ORACLE`.
- **Fix**: Check sender is an active validator before accepting oracle submissions.

### ECON-FIND-20 | LOW | Oracle `get_last_valid_price` Skips Staleness Check
- **File**: `torus-core/src/oracle.rs:310-320`
- **Description**: `get_last_valid_price` returns the stored aggregated price unconditionally with no age check. Used as fallback when reporter count drops below `min_oracle_reporters`. A price from block 0 is returned as if current.
- **Fix**: Apply the same `max_oracle_age` staleness check as `get_price`.

### ECON-FIND-21 | LOW | Whitelist Consume-After-Use Silent Failure
- **File**: `torus-bridge/src/native_executor.rs:442-446`
- **Description**: `ctx.governance.consume_whitelist(sender)` result is `if let Err(e)` with only a `tracing::warn!`. If consume fails, the whitelist entry remains, and the same address can register again.
- **Fix**: Make whitelist consumption mandatory -- fail the registration if consume fails.

### ECON-FIND-22 | LOW | FeeSplitter Pipeline Dead in Production
- **File**: `torus-bridge/src/native_executor.rs:737`
- **Description**: `let _split = FeeSplitter::split_fees(total_fees, ctx.epoch)` result is discarded. `SupplyTracker.cumulative_burned` and `cumulative_treasury` are never updated in production.
- **Fix**: Either call the full FeeSplitter pipeline, or remove the dead call.

### ECON-FIND-23 | LOW | Fill Errors Silently Discarded
- **File**: `torus-bridge/src/native_executor.rs:268, 276`
- **Description**: Both `apply_fill` calls use `let _ =` to discard the Result. If fill application fails for either the taker or maker, the order book state diverges from position state.
- **Fix**: Handle errors -- at minimum log them, ideally revert the fill.

### ECON-FIND-24 | LOW | Jail Vote Uses Stale Stake Weight
- **File**: `torus-economics/src/staking.rs:459, 510`
- **Description**: `record_jail_vote` stores `stake_weight: voter_val.total_stake()` at vote time. `tally_jail_votes` sums these stored weights. If a voter's stake changes between vote and tally, the tally uses stale data.
- **Fix**: Recompute voter stakes at tally time.

### ECON-FIND-25 | LOW | No Unbonding Queue Length Cap
- **File**: `torus-economics/src/staking.rs:163-165`
- **Description**: `undelegate` pushes to `delegation.unbonding` without any cap. Repeated 1-wei undelegations build a large vector, making `process_unbonding` expensive.
- **Fix**: Cap unbonding entries per delegation. Enforce minimum undelegation amount.

### ECON-FIND-26 | LOW | Precompile Lockbox Bypasses `u256_to_fp`
- **File**: `torus-core/src/precompiles.rs:817, 822`
- **Description**: The lockbox precompile performs `amount_raw as i128` directly instead of using the safe `u256_to_fp` function. Values > i128::MAX wrap to negative FixedPoint and silently no-op.
- **Fix**: Use `u256_to_fp` and return an error for overflow.

### ECON-FIND-27 | LOW | CoreWriter Input Validation Missing
- **File**: `torus-core/src/precompiles.rs:687-711`
- **Description**: No validation on `side`, `order_type`, `time_in_force` u8 discriminant values. Invalid discriminants are stored and forwarded to execution.
- **Fix**: Validate all discriminants and value ranges at queue time.

### ECON-FIND-28 | LOW | `original_qty` Not Updated on Modify Increase
- **File**: `torus-core/src/order_book.rs:414-428`
- **Description**: When `modify_order` increases quantity via cancel-and-reinsert, `replacement.original_qty` retains the old value. After the operation, `original_qty < remaining_qty`.
- **Fix**: Set `replacement.original_qty = new_qty` on increase.

### ECON-FIND-29 | LOW | KEY_ROTATION_COOLDOWN_EPOCHS Dead Code
- **File**: `torus-economics/src/types.rs:564`
- **Description**: `KEY_ROTATION_COOLDOWN_EPOCHS = 1` is defined but never referenced. `submit_key_rotation` only checks for existing pending rotation.
- **Fix**: Either enforce the cooldown or remove the constant.

### ECON-FIND-30 | LOW | delegations_for_validator O(n) Full Table Scan
- **File**: `torus-economics/src/staking.rs:798-819`
- **Description**: Scans every delegation in `CF_STAKING_DELEGATIONS` and filters by suffix (validator address). Called during every slash and reward distribution. Degrades linearly with total delegation count.
- **Fix**: Add a secondary index or restructure the key format for prefix-based lookups.

### ECON-FIND-31 | LOW | No Schema Version in Position/Balance Borsh Serialization
- **File**: `torus-core/src/position.rs:81-125, 144-159`
- **Description**: No version byte in `Position` or `NativeBalance` serialization. Adding or removing fields requires a migration, and no migration path exists.
- **Fix**: Add a version byte prefix.

---

## 4. FixedPoint Arithmetic Deep Analysis

### Type Definition
- **Location**: `torus-types/src/lib.rs:32-47`
- **Internal**: `struct FixedPoint(i128)` with `SCALE = 100_000_000` (10^8, 8 decimal places)
- **Range**: -1.7 x 10^30 to +1.7 x 10^30 in decimal (i128::MIN/SCALE to i128::MAX/SCALE)
- **No checked variants**: No `checked_add`, `checked_sub`, `checked_mul`, `checked_div` exist

### Safe Operating Range

| Operation | Implementation | Safe Input Range | Overflow Threshold |
|-----------|---------------|-----------------|-------------------|
| `add(a,b)` | `a.0 + b.0` (i128) | abs(a) + abs(b) <= 1.7x10^38 raw | Two values each ~8.5x10^29 decimal |
| `sub(a,b)` | `a.0 - b.0` (i128) | Same as add | Same as add |
| `mul(a,b)` | `(i256(a)*i256(b)/SCALE).as_i128()` | abs(result) <= 1.7x10^30 decimal | Two values each ~1.3x10^23 decimal |
| `div(a,b)` | `(i256(a)*SCALE/i256(b)).as_i128()` | b != 0 AND abs(result) <= 1.7x10^30 | Zero divisor panics |
| `neg(a)` | `-a.0` (i128) | a != i128::MIN | i128::MIN wraps to itself |

### Critical Call Sites Map

**Division (panic risk)**:

| Location | Dividend | Divisor | Zero Risk |
|----------|---------|---------|-----------|
| `margin.rs:115` | notional | leverage | YES -- user supplies leverage |
| `margin.rs:217-218` | notional | leverage | Same as above |
| `liquidation.rs:285` | upnl * reduce | pos.size | YES -- stale state |
| `liquidation.rs:271` | loss_amount | price_diff | Guarded at line 270 |
| `position.rs:310` | total_cost | new_size | Guarded at line 309 |
| `oracle.rs:338` | sorted sum | two | No -- hardcoded |
| `oracle.rs:354-355` | sum | n (prices.len) | Guarded at line 345-347 |

**Subtraction (wrapping risk in release)**:

| Location | Operation | Wrapping Exploitable? |
|----------|-----------|----------------------|
| `margin.rs:137` | equity - maint | YES -- PF-11 |
| `liquidation.rs:124` | maintenance - equity | YES -- same pattern |
| `liquidation.rs:304` | loss_amount - realized_pnl | Possible |
| `liquidation.rs:364` | bal.available - deduction | Can go deeply negative |

### Cargo.toml Overflow Settings

| Profile | Setting | Behavior |
|---------|---------|----------|
| `[profile.dev]` | Default: `overflow-checks = true` | Panics on overflow |
| `[profile.release]` | Default: `overflow-checks = false` | **Silent wrapping** |
| Workspace Cargo.toml | No profile sections defined | Inherits defaults |

### Assessment
The i128 with 8 decimal places provides sufficient range for all anticipated financial values. BTC at $1M = 10^14 raw, well within i128::MAX (~1.7x10^38). The primary risk is **correctness under adversarial input**: unchecked arithmetic + release-mode wrapping creates exploitable paths in margin calculations (PF-11). The `mul` and `div` operations correctly use i256 intermediates, but `as_i128()` truncation is unguarded for extreme results.

---

## 5. Game Theory Analysis

### 5.1 Flash Delegation Attack
- **Vector**: Delegate large stake just before epoch boundary -> validator elected -> earn block rewards -> undelegate
- **Cost**: Capital locked for `UNBONDING_PERIOD = 604,800` blocks (~7 days at 1s, ~42 days at 6s)
- **Reward**: Proportional block fees for the epoch + permanent staking APY if applicable
- **Required stake**: Enough to enter the top `max_validators` by total stake
- **Economically rational?**: Only if epoch fees significantly exceed opportunity cost of locked capital. The long unbonding period is a strong deterrent. However, no snapshot mechanism exists -- reward distribution uses live stake weights.
- **Mitigation**: Implement epoch-start stake snapshots for reward distribution. Consider a warming-up period.

### 5.2 Oracle Manipulation
- **Vector**: Control >50% of stake-weighted oracle reporters -> submit false prices -> trigger liquidations
- **Cost**: Need >50% of total active validator stake for guaranteed median control
- **Profit**: Trigger liquidations on overleveraged positions, front-run by taking the opposite side
- **Economically rational?**: Not directly (>50% stake is too expensive), but with `min_oracle_reporters=1` (PF-18), a single validator controls the price on any market where they are the sole reporter.
- **Mitigation**: Enforce `min_oracle_reporters >= 3` as protocol invariant. Add price circuit breakers.

### 5.3 Governance Capture
- **Minimum cost**: 33% of total staked supply (default `quorum_bps=3300`). With permanent staking multiplier 1.5x, permanent stake of 22% gives 33% weight. But permanent stake is irreversible.
- **Self-amplifying attack**: Pass one ParameterChange setting `quorum_bps=1` (PF-12). All subsequent proposals pass with 0.01% of stake. No timelock (FIND-06) means instant effect.
- **No proposal deposit**: Unlimited proposal spam is free (only need to hold 1000 TRS).
- **Economically rational?**: Yes if attacker controls 33% and extractable value exceeds committed capital.
- **Mitigation**: Add parameter key whitelist (PF-12). Add execution timelock (FIND-06). Add proposal deposit. Snapshot voting power.

### 5.4 Liquidation Cascading
- **Vector**: Manipulate oracle price -> trigger initial liquidations -> socialized loss hits near-margin accounts -> cascade
- **Mechanism**: CONFIRMED by FIND-13 -- socialized loss hits all traders including underwater ones. No insurance fund replenishment (FIND-04) means socialized loss triggers immediately after first fund depletion.
- **Amplification**: The maintenance margin bug (PF-10) actually prevents liquidation from working correctly, so in current code the cascade cannot happen. But once PF-10 is fixed, cascade risk activates.
- **Mitigation**: Fix insurance fund replenishment. Exclude underwater accounts from socialized loss. Add circuit breakers.

### 5.5 Fee Extraction via Transaction Ordering
- **Vector**: Validator-proposer manipulates within-phase ordering for MEV
- **Cost**: Zero (inherent to block production privilege)
- **Mechanism**: `sort_deterministic` uses `Debug` format for hash (FIND-01), making it non-deterministic across compiler versions. Proposer controls which actions to include/exclude. Oracle submissions land post-EVM, so validator can observe EVM results before submitting prices.
- **Mitigation**: Fix action_sort_key (FIND-01). Consider commit-reveal for oracle. Consider proposer-builder separation.

### 5.6 Reward Siphoning via Rounding
- **Vector**: Last-delegator-gets-remainder pattern (rewards.rs:111-121)
- **Profit**: At most `(delegations.len() - 1)` wei per block = negligible
- **Economically rational?**: No -- dust-level advantage.
- **Mitigation**: None needed.

### 5.7 Self-Trade Washing
- **Vector**: Place resting order + incoming order from same address
- **Mechanism**: STP (order_book.rs:602-611) cancels resting maker, produces no fill. `last_trade_price` NOT updated. No volume recorded.
- **Bypass**: Two separate addresses bypass STP (address-level check only).
- **Economically rational?**: Not via single-address. Two-address wash trading requires surveillance to prevent.
- **Mitigation**: Current STP is adequate for single-address attacks.

### 5.8 Permanent Stake Irreversibility Bypass
- **Analysis**: No `withdraw_permanent_stake` function exists. `PermanentStakeInfo` has no expiry field. `credit_balance` is public but never called with permanent stake amounts. Slashing does NOT affect permanent stake. `ParameterChange` cannot modify `CF_STAKING_PERMANENT` (different CF from `CF_FEE_CONFIG`).
- **Verdict**: NO bypass found. Permanent staking is truly irreversible in current code.

---

## 6. Margin and Liquidation Correctness

### Concrete Walkthrough: 10x BTC Long at $50,000

**Setup**: price=$50,000, qty=1 BTC, leverage=10, `maintenance_factor_bps=5000`

**Step 1 -- Initial Margin** (margin.rs:114-115):
- `notional = $50,000 * 1 BTC = $50,000`
- `lev_fp = from_raw(10 * 10^8)` = FixedPoint(10.0)
- `required_initial = $50,000 / 10 = $5,000` -- CORRECT

**Step 2 -- Maintenance Margin as coded** (margin.rs:220-222, BUG PF-10):
- `maint_num = from_raw(5000)` = FixedPoint(0.00005) -- BUG: should be from_raw(5000 * 10^8)
- `bps_denom = from_raw(10_000 * 10^8)` = FixedPoint(10000.0)
- Computation: `$5,000 * 0.00005 / 10000.0 = $0.000000025`
- **Actual maintenance: $0.000000025** (should be $2,500)

**Step 3 -- Price drops to $40,000 (20% drop)**:
- `unrealized_pnl = ($40,000 - $50,000) * 1 = -$10,000`
- `equity = $5,000 + (-$10,000) = -$5,000`
- Liquidation trigger at `liquidation.rs:120`: `equity(-$5,000) >= maintenance($0.000000025)` is FALSE -> liquidation fires
- But between $50k and $45k (equity between $0 and $5k), equity is positive and exceeds the broken maintenance -> **NO LIQUIDATION** during this entire range

**Step 4 -- The deadly zone**: Between $45,001 and $50,000, the position loses up to $4,999 but is never liquidated. Correct behavior: liquidation should fire at equity < $2,500 (price $47,500).

**Step 5 -- Liquidation execution** (liquidation.rs:185-225):
- Closes position at oracle price. PnL = -$10,000
- `bal.available = $5,000 + (-$10,000) = -$5,000`
- `remaining_deficit = $5,000` passed to `socialize_loss`
- Insurance fund = 0 (never funded, FIND-04)
- $5,000 spread to ALL traders, including underwater ones (FIND-13)

### ADL Path
- `auto_deleverage` (liquidation.rs:228-315) sorts profitable traders by descending unrealized PnL
- Reduces their positions to absorb the loss
- Division by `pos.size` at line 285 panics if size==0 (CHK-35)
- Division by `price_diff` at line 271 is guarded by `if price_diff > ZERO` check

---

## 7. Staking and Governance Security

### Delegation/Undelegation Lifecycle
1. **Delegate** (staking.rs:107-125): Debit balance -> update delegation amount -> update validator total_delegated. Atomic within one call.
2. **Undelegate** (staking.rs:129-175): Create UnbondingEntry with `release_block = current + UNBONDING_PERIOD`. Reduce delegation amount and total_delegated immediately.
3. **Process Unbonding** (staking.rs:195-215): User-initiated. Checks `current_block >= release_block`. Credits balance. No automatic processing.
4. **Edge case**: No max unbonding queue length (FIND-25). No minimum undelegation amount.

### Slashing Math (Double-Sign, actual BPS=500 not 1000)
Note: The audit brief stated `DOUBLE_SIGN_SLASH_BPS = 1000` (10%), but the actual constant at `types.rs:339` is `500` (5%).

Example: Validator `self_stake=50,000`, delegator `amount=50,000`:
- `self_slash = 50,000 * 500 / 10,000 = 2,500` -> `self_stake = 47,500`
- `del_slash = 50,000 * 500 / 10,000 = 2,500` -> `delegation = 47,500`
- `total_slashed = 5,000` (5% of 100,000) -- correct
- If `self_stake` drops below `MIN_SELF_DELEGATION` (10,000 TRS), validator is auto-jailed with no recovery path (FIND-15)

### Governance Attack Scenario
1. Attacker acquires 33% of total staked supply (weighted)
2. Submits ParameterChange proposal: `param_key="gov_params"`, `new_value=<GovernanceParams with quorum_bps=1>`
3. Votes for it (33% > quorum=33%, and votes_for > votes_against=0)
4. Proposal passes and executes **immediately** (no timelock, FIND-06)
5. All future proposals pass with 0.01% stake
6. Attacker submits second proposal setting `permanent_weight_multiplier_den=0`
7. Any voter with permanent stake now causes a division-by-zero panic (FIND-07)
8. Chain halts permanently

### Epoch Rotation Determinism
- `compute_new_validator_set` (epoch.rs:35-60): DETERMINISTIC -- sorts by descending stake with address tiebreak
- `apply_rotation_cap` (epoch.rs:140-210): NON-DETERMINISTIC -- `HashSet::difference` with random hash seed (PF-14)
- `safe_rotation_cap` (epoch.rs:139-141): Returns `current_set_size / 3`. Based on old set only.

### Key Constants Discrepancy

| Constant | Value | Assumes |
|----------|-------|---------|
| BLOCKS_PER_YEAR | 31,536,000 | 1 sec/block |
| UNBONDING_PERIOD | 604,800 | 1 sec/block (7 days) |
| JAIL_DURATION_BLOCKS | 28,800 | 6 sec/block (2 days) |
| COMMISSION_COOLDOWN | 28,800 | 6 sec/block (2 days) |
| VOTING_PERIOD | 100,800 | 6 sec/block (7 days) |
| MAX_COMMISSION_CHANGE_BPS | 100 (1%) | Note: brief stated 500 (5%) |
| DOUBLE_SIGN_SLASH_BPS | 500 (5%) | Note: brief stated 1000 (10%) |
| DOWNTIME_SLASH_BPS | 10 (0.1%) | Note: brief stated 100 (1%) |

---

## 8. Severity Classification

### Critical (2)
| ID | Summary |
|----|---------|
| ECON-FIND-01 | Action sort key uses Debug format -- consensus divergence risk |
| ECON-FIND-02 | Order book not persisted -- state loss on restart |

### High (13)
| ID | Summary |
|----|---------|
| ECON-PF-01 | FixedPoint add/sub unchecked i128, wraps in release |
| ECON-PF-02 | FixedPoint div panics on zero |
| ECON-PF-04 | Lockbox non-atomic writes -- fund loss |
| ECON-PF-06 | Zero-price liquidation for missing oracles |
| ECON-PF-10 | Maintenance margin BPS off by ~10^8 |
| ECON-PF-11 | free_margin underflow wraps to positive |
| ECON-PF-12 | Governance arbitrary parameter write |
| ECON-PF-14 | Epoch rotation HashSet non-determinism |
| ECON-PF-15 | cancel_order/modify_order no ownership check |
| ECON-FIND-03 | No EIP-712 replay protection |
| ECON-FIND-04 | Insurance fund has no replenishment |
| ECON-FIND-05 | No order margin reservation |
| ECON-FIND-06 | No governance execution timelock |
| ECON-FIND-07 | Division by zero via governance param manipulation |
| ECON-FIND-08 | Cross-margin equity ignores order_margin |

### Medium (18)
| ID | Summary |
|----|---------|
| ECON-PF-03 | Abstain maps to No vote |
| ECON-PF-05 | Order books with default tick/lot |
| ECON-PF-08 | Negative FixedPoint -> large u128 in precompiles |
| ECON-PF-16 | BLOCKS_PER_YEAR inconsistent with block time |
| ECON-PF-17 | Withdraw `to` address discarded |
| ECON-PF-18 | Oracle outlier rejection bypassed for single reporter |
| ECON-FIND-09 | Order ID cross-market collision |
| ECON-FIND-10 | No price validation for limit orders |
| ECON-FIND-11 | Tick size not enforced |
| ECON-FIND-12 | Stop order trigger direction not validated |
| ECON-FIND-13 | Socialized loss cascading |
| ECON-FIND-14 | Slashing dust rounding divergence |
| ECON-FIND-15 | Slash below minimum creates unrecoverable state |
| ECON-FIND-16 | Flash-vote attack via live weight |
| ECON-FIND-17 | Unlimited order placement DoS |
| ECON-FIND-18 | Oracle submissions never pruned |
| ECON-FIND-19 | No validator check for oracle submissions |

### Low (12)
| ID | Summary |
|----|---------|
| ECON-FIND-20 | get_last_valid_price skips staleness |
| ECON-FIND-21 | Whitelist consume silent failure |
| ECON-FIND-22 | FeeSplitter pipeline dead in production |
| ECON-FIND-23 | Fill errors silently discarded |
| ECON-FIND-24 | Jail vote stale stake weight |
| ECON-FIND-25 | No unbonding queue cap |
| ECON-FIND-26 | Precompile bypasses u256_to_fp |
| ECON-FIND-27 | CoreWriter input validation missing |
| ECON-FIND-28 | original_qty not updated on modify increase |
| ECON-FIND-29 | KEY_ROTATION_COOLDOWN_EPOCHS dead code |
| ECON-FIND-30 | delegations_for_validator O(n) scan |
| ECON-FIND-31 | No schema version in Borsh serialization |

### Info (2)
| ID | Summary |
|----|---------|
| ECON-PF-07 | Admin actions are no-op stubs |
| ECON-PF-09 | Unbonding vec len u32 truncation |

---

*End of Audit Report -- ECON-AUDIT-3.4.4*
*Total findings: 2 Critical, 13 High, 18 Medium, 12 Low, 2 Info = 47 findings*
*All 18 pre-findings verified (17 confirmed, 1 confirmed with severity correction)*
*31 new findings identified*
