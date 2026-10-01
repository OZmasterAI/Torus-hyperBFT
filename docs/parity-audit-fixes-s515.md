# Hyperliquid-parity audit fixes (s515) — client & deploy notes

Branch `fix/parity-audit-bugs`. Consensus-visible: every change below alters
executor output, so all validators must run the same build. The oracle, epoch
and block-timestamp rows are on `feat/oracle-aggregation` (stacked on it, s517).

## Client-visible behaviour / ABI

| Bug | Change |
|-----|--------|
| 1 — market orders | `Market` / `StopMarket` `price` is a **required** worst-acceptable-price cap (`<= 0` rejected); the book never matches past it and the unfilled remainder is cancelled. **Margin (Hyperliquid-style, review 4):** a market order (buy or sell; a triggered stop-market is placed as one at trigger time) reserves `qty × mark price / tiered max leverage` — the limit-order formula at the **mark** (the oracle's aggregated stake-weighted price for the market). With no usable oracle price (none, stale, `<= 0`) it reserves at its **cap** — the case until the validators run the price feeder (`tools/price-feeder`, s517 item B; see *Oracle signer*). **Review 5:** only the quantity beyond what closes the sender's opposite-side position is reserved (see *closing needs no margin*). Nothing else in the block or batch moves that reservation. Its whole reservation is released after matching. A pending stop-market still holds `reserve(cap, qty)` until it fires. A price × qty that overflows is rejected (no panic). |
| match-time margin (review 4, F1 s517) | Margin is checked **again as the order matches** (Hyperliquid: "when orders are placed and again when they match"), **account-level** (F1). **Takers** whose fills can cost more than they reserved — market buys / sells, **limit sells**, IOC / FOK limit buys — are checked before each fill: the **increase of the position's initial margin at the position-size tier** (the position valued at the mark, entry price without one; the closing part releases its IM; plus, for a GTC limit, the part of its unfilled rest that would OPEN, at its limit) must fit the order's reservation + the sender's **running free margin** in that book. Batch: a sender's free margin after all its Phase-2 reservations (minus what its unchecked orders committed beyond their reservations, see *Placement gate*) is an **exclusive** pool of the market of its first checked taker (flat order) — its other markets start at 0 — so no two market workers spend the same free margin; in that book its takers share the pool as a running budget, credited by their closing fills. A fill that does not fit is cut to the largest lot multiple that does, then filling stops and the rest is cancelled (never rests); a FOK order whose complete fill does not fit is rejected whole. **Makers** are checked too (HL `marginCanceled`, see *Makers*). Reduce-only orders are exempt; GTC / PostOnly limit **buys** are not re-checked at match (they fill at or below their limit; their account cost is enforced at placement). |
| closing needs no margin (review 5, F2; F1 s517) | Hyperliquid never charges margin to reduce a position. **Match time:** the part of a checked taker's fills that reduces the sender's opposite-side position is free and **releases that position's initial margin** (F1), so a flip is charged only the net IM change; a purely closing fill always fits, even for an under-margined account. The **resting** part of a GTC order that would only close is not charged at match either. The position is the one reduce-only policing uses — read at placement (single path) or when the batch's matching starts, then advanced through every fill of the block in that market — so all three paths free the same quantity (e.g. long 20, two market sells of 20 in one batch: the first closes free, the second opens 1 unit on the IM its predecessor released). **Placement:** an order that cannot rest (market, IOC / FOK limit — reduce-only or not) reserves only for its quantity beyond the closing allowance of the current position (single path) / the pre-batch position (batch), running per sender / market / side across the batch's orders so one allowance never frees two orders. Its whole reservation is released after matching. A GTC / PostOnly order still **reserves** its full quantity at placement (a resting row's reservation is `price × remaining` for every later release), but its account check (see *Placement gate*) charges only what it would open, and under strict HL that reservation no longer needs free cash. Example: long 20 @100, 95 of 100 locked in resting orders, plain market sell 20 → all 20 fill. |
| leverage tiers at match time (review 5, F4) | Fills and the GTC hold are one notional charged at that notional's tier (they were charged apart, each at its own lower tier, which undercharged across a tier boundary). The need is monotone in the fill size, so the "largest lot multiple that fits" search is exact. |
| Placement gate (F1 s517, D1 strict HL) | Account-level, Hyperliquid cross margin, computed (nothing new is stored): `equity = available + order_margin + Σ UPnL`, `free = available + Σ UPnL − Σ position IM` (positions valued at the mark, at their **entry price** without one; each position's IM at its **position-size tier**). An order is accepted iff the increase of its market's IM (a complete fill; for an order that can rest, also resting its opening part) is `<= 0` (it only reduces) or `<= free`. This is the **only** placement gate on every path (single, batch serial, batch sharded) and for `ModifyOrder`: the old `available >= reservation` check is gone, so **unrealized profit funds reservations** and `available` may go **negative** (RPC `torus_getBalances` `available_balance` can then be a signed `-0x…`; the 0x0801 `getBalances` reader reports a negative balance as 0 — ABI unchanged). Error text keeps the `insufficient margin: need X, have Y` prefix (plus ` (account)`). Pending stops are checked as resting orders at their reservation price and re-checked when they trigger. Batch (Phase 2, identical serial / sharded): each sender's earlier accepted orders are projected as if filled, so a later order is charged at the projected position's tier; the part of an order's need beyond its order-tier reservation stays **committed** and comes off `free` for the sender's later orders (review fix: two GTC buys crossing a tier in one batch could otherwise exceed max leverage) and, for unchecked orders, off the match-time pool. **T7 decision:** the IM the sender's earlier orders are projected to RELEASE (their closing parts) is credited to `free` **only** when checking a match-checked order (market, IOC / FOK limit, limit sell — its fills are re-checked against the real position); GTC / PostOnly buys and stops get no such credit (the closing order may rest unfilled), and the credit never enters the match-time pool. |
| Makers (F1 s517, HL `marginCanceled`) | Every maker fill is checked: the fill's IM increase (position tier, closing part free) minus the fill's share of the maker's reservation must fit the maker's free margin — a snapshot of its account taken when the book first sees it in the placement / batch (Phase 3 reads the frozen pre-batch state), advanced by its own fills. A maker that cannot afford the fill is **cancelled whole** (its reservation released, like a reduce-only cut) and the taker continues with the next maker; the FOK pre-check skips such makers the same way. A purely closing maker fill always fits. Review fixes: in the book holding the sender's batch pool (and on the single path) its makers share that running pool with its takers; in its other markets they use the snapshot (they were checked against the 0 taker budget there); and a +1 raw-unit cost produced only by floor rounding of two IM differences is not charged. A maker filling in several markets of one batch can overshoot by at most its snapshot. |
| Withdrawals (F1 s517, D3 SAFE) | `TransferToSpot`, `Withdraw{to}` and CoreWriter `LockboxWithdraw` (drained as TransferToSpot) are allowed iff `amount <= available` **and** `available + Σ UPnL − amount >= max(Σ position IM, 10% × Σ position notional)` (HL `transfer_margin_required`; SAFE variant: resting orders' reservations are not collateral for positions). New error: `withdrawal of X would leave the account under-margined: …`; `amount > available` (any amount while `available < 0`) fails with the lockbox's existing error. Flat accounts withdraw everything, as before. |
| Liquidation (item 3, s517, `feat/liquidation`) | **Hyperliquid-style, wired** at the end of every native block (after CoreWriter, before governance), on the block-start mark. **Trigger:** account value (available + order margin + UPnL at the mark) < maintenance margin; MM = **half the initial margin at max leverage**, position-size tier, each market's own tiers (`maintenance_margin`). Margin configs now come from the market listing: one flat tier at the listing's max leverage (`100 / initial_margin %`); a 20x market is unchanged. **An account with a stale / absent mark in ANY of its markets is skipped.** Classes: `AV >= MM` healthy; `AV < 0` **ADL**; `AV < 2/3 MM` **backstop**; else **stage 1**. Every acted account first loses all resting orders **and pending stops** (reservations released). **Stage 1:** reduce-only IOC market orders into the book (ordinary fills / trades, stops they fire run too), price cap `mark ∓ mark / (2 × max leverage)` (the MM rate, 2.5% at 20x), largest MM first, until `AV >= MM` — the rest of the collateral and positions stay with the trader. Positions above **100,000** notional at the mark go in **20%** chunks with a **30 s** block-time cooldown per account (only backstop / ADL act meanwhile). **Backstop:** positions and the remaining collateral move to the **liquidator vault** `0x746f7275732d6c697175696461746f722d766c74` (`b"torus-liquidator-vlt"`, no key) at the mark. **ADL:** opposite-side positions ranked by HL's (mark/entry) × (notional/account value), closed at the **previous** liquidation step's mark; the vault is ADL'd when its value goes negative; a flat account's remaining deficit moves to the vault. **No liquidation penalty, no insurance fund, no socialized loss**; size only moves through fills between two accounts (Σ long == Σ short). Budgets: 2,048 accounts valued / 64 acted on per block, round-robin over position holders. Defaults D1-D11: `docs/plans/liquidation.md`. Liquidation fills look like ordinary trades (no flag); the vault is an ordinary visible account. |
| 2 — `reduce_only` | Enforced. Placement from a flat position or on the increasing side is rejected (stops included, on every path); an oversize order is clamped to the position size and reserves margin only for the clamped size; resting reduce-only orders are shrunk / cancelled when the position shrinks, closes or flips (margin released). A reduce-only maker never fills past its owner's position. |
| stops | Triggered stops are now placed through the normal path after the block's matching settles: they reserve margin, move positions, respect the StopMarket cap and re-check `reduce_only` at trigger time. |
| CancelAll (s517 C4) | `CancelAllOrders` now also cancels the sender's **pending stop orders** (TP/SL, stop-market / stop-limit) in the targeted market(s) and releases their margin reservation. It used to leave the stops pending when the sender had no resting order, and to drop them without releasing their reservation otherwise. |
| ModifyOrder | Only the order's **owner** may modify it (was: any account, margin charged to the owner). Validated like placement before anything changes: price `> 0`, a *new* price on the tick (a quantity-only modify of an order resting off the current tick is accepted), quantity `> 0` and `>=` lot, at least one field set; a new price at / through the opposite best is **rejected** (a modify never matches — cancel and place instead). A reduce-only order is clamped to the position (rejected when there is nothing to reduce); as in placement the lot applies to the requested quantity, so the clamp may rest below the lot. Margin (F1 s517, review fix): the modify is gated like cancel + place — the new order's position-tier need (closing free) minus what the old order gives back (the larger of its need and its reservation) must fit the account's free margin (UPnL counts; `insufficient margin for modify: need X, have Y (account)`, order and balances unchanged); there is no `available >= extra` check any more. The reservation difference (placement formula, tiered leverage) is then reserved (it may take `available` negative) or released exactly; overflowing price × qty is rejected. A quantity-only decrease keeps time priority. |
| 3 — unbonding | New `ClaimUnbonded` native action (canonical action tag **26**; EIP-712 `ClaimUnbonded(uint64 nonce)`, fund-moving, not session-signable) and CoreWriterStaking `claimUnbonded()` (queued kind tag **7**). Releases every matured unbonding entry of the sender, all-or-nothing. |
| 4 — listing | `ListMarket` / `DelistMarket` / `UpdateMarketParams` native actions now error ("governance-only"). Listings go through governance; the market id is `max(existing) + 1`, assigned at proposal **execution**. |
| 5/6 — lockbox 0x0820 | Native amounts are 8-dec, EVM is 18-dec wei: native→EVM ×10^10; EVM→native floors ÷10^10 and burns the dust. `depositToNative(uint128)` is **payable** and requires `msg.value == arg`; the value is burned in-frame and the native credit is queued for the **next block**. `withdrawFromNative(uint128)` is non-payable, takes a multiple of 10^10 wei, and is also applied next block. Queue rows now commit atomically with the block's EVM bundle (F1). This branch first added a node-local EVM-applied marker (`cf_consensus_meta` / `evm_applied_block`) so crash replay could skip a committed block's EVM txs; that marker was superseded by main's one-flush-batch EVM commit (93d4fff): the bundle, its queue rows, the native phase and the applied-height marker land in ONE write, so after a crash the whole block replays from its parent state and a tx skipped the first time (e.g. nonce too high) can never execute on replay. |
| writer precompiles | DELEGATECALL / CALLCODE / STATICCALL to any writer precompile (CoreWriter 0x0810, CoreWriterStaking 0x0811, Lockbox 0x0820) reverts — they act for `msg.sender`, so only a plain CALL is accepted. Readers 0x0800–0x0803 stay callable any way. |
| Oracle submissions (s517 T3/T4) | `SubmitOraclePrices` is validated as a whole before anything is written (all-or-nothing): sender an Active validator (as before) or its registered hot oracle signer (see *Oracle signer*), 1..=256 entries (`MAX_ORACLE_PRICES_PER_SUBMISSION`), no repeated market, market listed (governance), `0 < price <= 10^12` units. One row per (market, validator): a new submission overwrites the validator's previous one. **Review M1(b) (s517 feeder):** the submission's signed `timestamp` is its sample time (unix ms) and must be within **5 s** of the block's header time (`MAX_ORACLE_SAMPLE_SKEW_MS`), else the whole action is rejected; the row stores it and a write whose sample is not newer than the stored row's is skipped, so the newest sample per (market, validator) wins in any order. A **session key** with scope `Full` of the validator OR of its signer can also submit `SubmitOraclePrices` (`Full` excludes only CreateSession, RevokeSession, Withdraw, Delegate, Undelegate, PermanentStake, ClaimRewards, ClaimUnbonded and SetOracleSigner); it reports exactly as its owner would. Treat a Full-scope session of either like the signer key. |
| Oracle aggregation / mark (s517 T2, T5–T7) | Runs at the **start of every block**, before any action (`begin_block_oracle`): prune, then aggregate every listed market (ascending); the whole block reads one mark, and a submission of block h counts from block h+1. Time-based on the block header timestamp: a validator's latest submission counts while `<= 10 s` old; 3×MAD outlier cut (exact integers), **stake-weighted median** (Active validators, whole-token stake weight). **Fresh-price rule (review M1):** after the outlier cut, **at least 3 reporters** whose stake is **more than 2/3 of the total Active stake** (checked in exact integers as `3 × reporting > 2 × total`; exactly 2/3 is not enough). Otherwise the last aggregate is kept and ages, so 3 colluding validators cannot keep a price fresh while the honest feeders are down. The quorum counts the reporters left after the cut (the same set as the median). The cut uses the unweighted median, so counting before it would let many low-stake validators cut the high-stake honest reports and set the price. Cost: an honest report that gets cut does not count toward the quorum. At exactly half the stake, the weighted median picks the **lower** price (cumulative stake `>= total / 2`). **Usable** (one rule for every reader) iff it exists, `price > 0` and it is `<= 60 s` older than the block's timestamp. Readers: `AccountReader::mark` / market-order reservation, precompiles 0x0802 (stale flag) and 0x0800 (position UPnL), RPC `torus_getMarkPrice` (stale ⇒ `markPrice = indexPrice = 0`, `timestamp 0`) and `torus_getPosition` (stale ⇒ UPnL at entry price) — "now" is the latest committed header's timestamp. ABIs unchanged. **EVM vs native (review M3):** the EVM section of block h runs before `begin_block_oracle`, so EVM transactions (precompiles 0x0802 / 0x0800) in block h read the aggregate written by block h−1, while native actions in block h read block h's. Both are deterministic. `torus_getMarkPrice`'s `timestamp` is the aggregate's **block number**, not a time (pre-existing). Per-market errors never abort the block; a storage fault fail-stops. The native phase also runs while submission rows exist, so an idle chain still aggregates. |
| Oracle signer (s517 feeder S1–S4, R1–R2) | New native action `SetOracleSigner { signer }` (canonical tag 27, EIP-712 `SetOracleSigner(address signer,uint64 nonce)`, validator's EVM key only — never a session key). A validator in any status but Tombstoned sets, rotates or (with `0x0`) clears its **hot oracle signer**: a separate address that may submit `SubmitOraclePrices` for that validator. Its rows, Active check and stake weight are the **validator's**. The signer gets no other authority (its own account is ordinary). One validator per signer; the signer cannot be a validator or the sender. Rotation takes effect from the **next block**: a submission from the old signer in the rotation block still counts (oracle actions run before `Other`). `torus_getValidators` adds `oracleSigner` (omitted when unset). RPC ingress now rejects an invalid `SubmitOraclePrices` (1..=256 entries, duplicate market, unlisted market, invalid price) with an error instead of a silent exec failure. Operator tool: `tools/price-feeder` (README there) plus `torus-wallet set-oracle-signer`. **Review fixes:** setting a signer needs a `proof` — the signer key's EIP-712 signature over `OracleSignerProof(address validator,uint64 chainId,uint64 nonce)` — so nobody can squat an address whose key they do not hold or replay another validator's proof (M3); clearing needs none, and a **tombstoned** validator may still clear (L2); an address serving as a signer cannot `RegisterValidator` (L1). |
| Oracle mempool priority (s517 feeder M1–M4) | Native pool order is **cancels, then oracle submissions, then the rest**. Oracle submissions from an Active validator or its registered signer get the cancel treatment: they evict a normal entry from a full pool and pass the RPC busy / pool-full pre-verify screens, and the deepest exec-backlog pacing tier proposes cancels + oracle only. Any other sender's oracle submission is rejected at admission (`not an active validator or its signer`). At most **4 pending per validator** (own address + signer). Node-local (proposer-side admission and selection), not consensus-visible. **Review M1(a):** at the cap a NEWER submission evicts the validator's oldest pooled one (a duplicate evicts nothing; one older than all pooled is rejected). |
| Epoch processing (s517 T0) | Runs on **every** epoch-boundary block. It lived in the native phase, which an empty block skipped, so an empty boundary block ran no epoch (no rewards / inflation, no validator status changes). Empty non-boundary blocks are unchanged. |
| Block timestamps (s517 T0b/T1) | Body validation (`validate_block` → `finish_validate`) rejects a proposal whose timestamp is below its parent's, more than **5 s** ahead of the local clock (`MAX_BLOCK_TIMESTAMP_DRIFT_SECS`). The proposer uses `max(now, parent.ts)`. **No lower bound (review M2, open):** a byzantine leader can propose `ts = parent.ts` after a stall, so an old oracle aggregate has age 0 in that leader's blocks (the next honest proposer moves time to `now`). A "not more than 30 s behind" rule cannot be enforced yet: fresh and late-recovered bodies share one validation path, so it would also reject honest late bodies (network-wide pause → wedge), and a hotstuff-level flag is bypassed by withholding the body. It becomes enforceable with the "validate before voting" consensus item. Not applied on block sync, to blocks at or below the committed height, in execution or in replay. Every execution path reads the committed header timestamp. **Validators need NTP-synced clocks** (a node more than 5 s behind the proposer rejects valid proposals). Replicas vote on the header **before** body validation (pre-existing), so a bad timestamp is never executed but can stall, and committed history can still hold one; oracle ages clamp at 0. |

## Deployment requirements

* **Fresh genesis.** The lockbox unit change (8 ↔ 18 decimals) reinterprets
  every existing EVM↔native balance; there is no migration.
* **Simultaneous upgrade of every validator** (lockstep): a mixed set forks.
* **Liquidation (s517, `feat/liquidation`): lockstep + fresh genesis.** A new
  native-root column family (`cf_native_liquidation`, root tag 6: cooldown /
  previous-mark / cursor rows) changes the native root once the step writes
  a row; margin configs now come from market rows; the step moves positions,
  balances and books at the end of native blocks, and the native phase also
  runs while cooldown / cursor / pending rows exist. The vault can optionally be seeded
  through genesis `native_balances` (no code needed).
  **No activation height:** the step (and the CancelAll / margin-config
  changes) apply from block 1, so this binary must start from a fresh genesis —
  it cannot replay or sync a chain produced by an older build (the replay
  would liquidate / cancel where the original did not and diverge from the
  recorded roots).
* F3 — legacy stop rows: `StopLimit` / `StopMarket` rows written before this
  branch carry no StopMarket cap and were reserved under the old formula; on
  trigger they would release the wrong amount of margin. Fresh genesis means
  none exist.
* F7 — old nodes cannot decode `ClaimUnbonded` (tag 27 / queued tag 7), and
  the CoreWriter drain deletes queue rows it cannot decode (new lockbox kinds
  0x20 / 0x21), silently dropping them on an old node. Another reason the
  upgrade must be lockstep.

* F1 — (superseded) this branch wrote a node-local EVM-applied marker and
  fail-stopped on a marker AHEAD of the block being executed. Main's
  one-flush-batch EVM commit (93d4fff) replaced it: an EVM block is durable
  only together with its native phase and applied-height marker, so there is
  no marker and no deployment step for it.
* F1 (review 3) — (superseded) the marker-staging fail-stop, the marker-ahead
  check for every block and the fallback-commit trailer (flags byte + bundle
  account addresses) went with the marker. Main's one-flush-batch EVM commit
  (93d4fff) latches the fail-stop instead when an EVM block's batch cannot be
  built or its flush fails.
* Review 4 — more fail-stops (the node halts; operator restores / resyncs):
  * B1: once the fail-stop is latched, no further block is executed or
    flushed. The boot replay stops at the failed height (it used to run the
    later heights on top of the missing state and move the native marker
    past it);
  * B2-B4 (superseded with the marker): B3 (malformed marker) and B4 (failed
    `commit_pending_bundle` fallback) concerned the marker's separate EVM
    commit. On main (93d4fff) the EVM batch, incremental or the plain
    `pending_bundle_batch` fallback, is the prefix of the block's one flush
    batch, and a batch that cannot be built latches the fail-stop. B2 (fail-stop
    when a fallback bundle writes contract storage while the incremental root
    is active) is not on this branch: the plain fallback still writes no
    `CF_HASHED_*` / `CF_TRIE_*` rows, so after a fallback the incremental trie
    can lag the full scan (open item).
* Oracle (s517, `feat/oracle-aggregation`) — **lockstep**: aggregation at every
  block start writes the native root, and the epoch, timestamp and submission
  rules change which blocks and actions are valid; a mixed set forks.
  **Fresh genesis semantics** for the changed `CF_NATIVE_ORACLE` row layouts:
  submissions are keyed `"sub"‖market‖validator` (31 bytes, was 39 with the
  block number) and the aggregate row is 36 bytes (a timestamp appended to the
  28-byte row). Old rows are not migrated, and a 28-byte aggregate does not
  decode as usable. **Upgrade (review L2):** an old 39-byte submission key
  starts with the new `"sub"‖market` prefix and its value layout is
  unchanged, so it decodes: it counts next to the validator's new row while
  it is `<= 10 s` old, and the first block-start prune deletes it, which is a
  native-root write. The epoch fix (T0) runs epoch processing on EMPTY
  boundary blocks, so replaying old history with the new binary changes
  state at those blocks. Deploy with a **fresh genesis**: never replay old
  history with the new binary.
* Oracle signer (s517, `feat/oracle-feeder`, commit `6b33a7d`) — **lockstep**,
  **fresh genesis**: a new action variant (`SetOracleSigner`, tag 27), the
  `ValidatorState` borsh layout gains a trailing `oracle_signer` (old validator
  rows do not decode), and `"sgn"‖signer` rows in `CF_NATIVE_ORACLE` are part
  of the native root. The oracle mempool priority (`cbab523`) is node-local but
  ships with it. Then each validator runs `tools/price-feeder` against its own
  node. The review fixes are lockstep too: `bcc73a7` (sample-time bound,
  submission row gains `sample_ms` — fresh genesis), `035e5ed` (tombstoned may
  clear), `a4b7545` (a serving signer cannot register), `f209ebc`
  (`SetOracleSigner.proof`, new action layout — fresh genesis).

## Known deferred items (not fixed on this branch)

* **F1 — margin is account-level now (fixed on this branch, s517).** Formerly
  per order (100 USDC at 20x, two market sells each sized to the full 100 →
  ~40x). See *Placement gate*, *match-time margin*, *Makers*, *Withdrawals*.
  Known deviations from Hyperliquid, kept deliberately (details:
  `docs/plans/account-level-margin-f1-impl.md`, *Risks / design corrections*):
  * D1 — orders that only reduce (need `<= 0`) pass with no gate but still
    debit their full reservation, so `available` can go negative without UPnL
    behind it (bounded; nothing is withdrawable while it is).
  * D2 — batch pools: a sender's free margin is exclusive to the market of its
    first checked taker; its checked takers in other markets of the same batch
    fill only within their own reservation (conservative).
  * D3 — withdrawals do not count resting orders' reservations as collateral
    (stricter than HL).
  * D4 — a resting closing quantity is free at match but still reserved at
    placement.
  * D5 — opening fills are valued at the fill price, positions at mark / entry;
    UPnL of fills in the same batch is seen only from the next batch.
  * D6 — Phase 2 projects a sender's earlier orders as if filled (resting ones
    too); the single path does not, so outcomes can differ between paths when
    earlier orders rest.
  * D7 — GTC / PostOnly limit buys are not re-checked at match; their residual
    cross-market leak within one batch is the gap between order-tier and
    position-tier IM.
  * D8 — maker snapshots are pre-batch; a maker filling in several markets of
    one batch can overshoot by at most its snapshot.
* **Liquidation (item 3, `feat/liquidation`)** is wired (see *Liquidation*
  above) — but it only acts on markets with a usable mark, and no price
  feeder exists yet (below): until one does, no account is liquidated.
  Known limitations: a listing's `maintenance_margin_bps` is ignored (MM =
  ½ IM, HL); the vault cannot unwind its positions (no orders / deposits
  until the HLP branch) — it only shrinks through ADL, and its balance can
  go negative (absorbed deficits).
* **Mark price in production — aggregation fixed on `feat/oracle-aggregation`
  (s517).** Time-based (a submission counts 10 s, the aggregate is stale 60 s
  after the last fresh one), stake-weighted median, min 3 reporters holding
  more than 2/3 of the Active stake,
  aggregation at every block start, submission hardening (see *Oracle
  aggregation / mark*, *Oracle submissions*). **Still deferred:**
  * ~~no price feeder exists~~ **done on `feat/oracle-feeder` (s517,
    item B):** `tools/price-feeder`, one per validator, signs with the
    validator's hot oracle signer (`SetOracleSigner`). Until the validators
    run it, no market has a mark: market orders reserve at their cap and F1
    values positions at their **entry price**. On a 3-validator net all
    three must run it for the price to stay fresh;
  * mark = oracle aggregate, not the Hyperliquid mark formula (item C).
* **Validate before voting (CometBFT-style), consensus item.** Replicas vote
  on a proposal's header before its body is validated (incl. the timestamp
  rule, see *Block timestamps*). A bad block is never executed, but it can
  gather votes and stall the round, and committed history can hold an
  out-of-range timestamp. Move validity checks before the vote, and add the
  timestamp lower bound there (reject a proposal more than 30 s behind the
  local clock, CometBFT PBTS-style; review M2, which cannot be enforced
  before this).
* **F3 — B2 fail-stop scope.** B2 (the fallback commit would write contract
  storage while the incremental root is active — it is ON by default) is
  node-local in practice: a deterministic trie error fails block validation
  (`validator.rs` ~198, root computation) before anything commits, on every
  node alike. Residual risk: the multi-block catch-up path, which commits
  with `evm_root_updates: None` (`validator.rs` ~322) and so always takes the
  fallback; there, a transient I/O error on a storage-writing block now
  halts that node (operator restores / resyncs). Better long-term: a
  fallback resync that covers storage too.
* GTC / PostOnly orders that close a position still reserve their full
  notional at placement (see *closing needs no margin*). Under F1 strict HL
  this no longer needs free cash (the account check charges nothing to
  close; `available` may go negative), but the reservation is still held.
  Needs a per-order stored reservation to fix.

* A pending stop-market holds margin at its cap, not the mark. Its book row
  stores no other price, and the release at trigger time must be exact.
* After an incremental-batch failure the plain fallback leaves the
  incremental trie stale (accounts and storage). With the incremental root on
  (the default) later EVM roots are computed on that stale trie; the
  validator `StateRootMismatch` check is the safety net (see B2 above).

* A `ModifyOrder` whose new price would cross the book is rejected; Hyperliquid
  (cancel + new order) would match it.

* The executor accepts orders for unknown market ids (a book is created on demand).
* A governance proposal whose execution fails blocks later proposals.
* `cancel_all` does not release the margin of the trader's pending stops.
* Stops cannot be cancelled by order id.
* Unbonding stake is not slashable.
* `eth_call` / `eth_estimateGas` reject calls reaching 0x0820 (writer
  precompiles are denied in simulation), so wallets cannot estimate deposits.
* Wallet staking commands send amounts in wei.
* Governance `Delist` / `UpdateParams` proposals are text-only (no executor effect).
