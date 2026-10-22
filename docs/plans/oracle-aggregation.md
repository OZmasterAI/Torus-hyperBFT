# Design: Oracle aggregation into block execution (item 2)

**Status (s517):** option A implemented on `feat/oracle-aggregation`
(4893393..b7a8080 + T9 RPC tests; plan rev. 2 in
`docs/plans/oracle-aggregation-impl.md`): epoch processing on every boundary
block (T0), block-timestamp validation (T0b/T1), time-based aggregation 10 s /
60 s (T2/T8), submission hardening (T3/T4), block-start aggregation (T5–T7).
Review follow-ups (s517): fresh price needs >= 3 reporters holding
> 2/3 of the Active stake (see *Decisions* 4–5, *Notes* below). The
timestamp lower bound (M2) is in the testnet genesis (owner s138, branch
`fix/m2-timestamp-lower-bound`), see Decision 5.
Not merged / pushed. Liquidation (item 3): done on `feat/liquidation`
(`docs/plans/liquidation.md`). Next: B (feeder), then C (HL mark). Client / deploy notes: `docs/parity-audit-fixes-s515.md`.

Branch `fix/parity-audit-bugs` (s517), after F1 (`3e9365e`). Liquidation (item 3)
depends on this.

## Problem

No mark price exists in production. `NativeExecutor::aggregate_oracle_prices`
(NE ~6810) has no caller, and no software produces `SubmitOraclePrices`. So
`AccountReader::mark` is always `None`: account margin values positions at
entry (UPnL 0), market orders reserve at their cap, liquidation cannot work.

## Hyperliquid (docs, s517)

* Oracle: each validator publishes a spot price per perp asset ~every 3 s —
  weighted median of Binance, OKX, Bybit, Kraken, Kucoin, Gate, MEXC, HL spot
  mids (3,2,2,1,1,1,1,1). Final oracle = stake-weighted median of validator
  submissions. Used for funding.
* Mark: median of (a) oracle + 150 s EMA of (HL mid − oracle), (b) median of
  HL best bid / best ask / last trade, (c) weighted median of CEX perp mids
  (Binance 3, OKX 2, Bybit 2, Gate 1, MEXC 1); if exactly two exist, a 30 s
  EMA of (b) is added. Used for margin, liquidation, TP/SL, UPnL. Updated
  when validators publish.

## Context (exploration, s517)

* `oracle.rs`: submissions `CF_NATIVE_ORACLE` `"sub"‖market‖validator‖block`;
  aggregate `"agg"‖market` (price, block, reporters). CF is in the native
  state root. `aggregate_price`: window 10 blocks, latest per validator,
  stake filter, 3×MAD outlier cut, min 3 reporters (else last price keeps
  ageing), stake-weighted median. `get_price`: stale after 100 blocks.
  Pruning only happens inside aggregation (≤100 keys per call).
* Submission handler `exec_submit_oracle_prices` (NE ~6512): only checks the
  sender is an Active validator. No listed-market check, no price > 0, no cap
  on entries, timestamp ignored. Oracle actions run in the post_evm group.
* One native execution path (`app.rs` `execute_committed_block_with`, used by
  live execution, `execute_committed_block` and crash replay). Validator
  never executes native actions.
* Inputs: stakes from `ctx.staking.all_validators()` (deterministic order,
  filter Active, whole-token power — U256 wei overflows FixedPoint);
  markets from `CF_NATIVE_MARKETS` 8-byte keys (governance listing).
* Consumers: `AccountReader::mark` (margin, Phase 2, modify, withdrawal),
  market-order reservation, RPC `getMarkPrice` / positions (no staleness
  check), precompiles 0x0802 OracleReader, 0x0800 position UPnL (ignores
  staleness).
* Tests: `oracle_tests.rs` (10), `set_mark` helpers, `app.rs` whole-block
  helpers (`make_block`, `signed_action`, `register_proposer`).

## Options

### A. On-chain aggregation only (oracle price = mark)  ← recommended first step

* One call at the START of each block (before `execute_batch(pre_evm)`,
  app.rs ~2198): aggregate every listed market from submissions in the
  window; the whole block (native orders, CoreWriter, modify, withdrawals)
  reads one mark. Errors are per-market results, never abort the block.
* Harden submissions: listed market, price > 0, cap on entries per action.
* Pruning of submission rows for every market each aggregation.
* Mark = oracle aggregate (as today's readers expect). Staleness honoured
  by RPC `getMarkPrice` and 0x0800 too.
* Pros: small, deterministic, testable end to end with test submitters;
  unblocks liquidation design. Cons: nothing produces prices in production
  until a feeder exists (option B); mark ≠ HL mark formula.
* Effort: Medium. Risk: Low-Medium (new root writes → lockstep upgrade).

### B. A + validator price feeder (off-chain)

* A task in `torus-node` on each validator: fetch CEX spot mids, HL-style
  weighted median, sign and submit `SubmitOraclePrices` every N blocks via
  the local mempool. Needs a market → exchange-symbol map (governance or
  config), HTTP client, timeouts, backoff; runs only on validators.
* Pros: real prices in production. Cons: network I/O and exchange APIs in
  the node; ops config; min-reporters = 3 means all 3 testnet validators
  must be feeding.
* Effort: Medium-Large. Risk: Medium (ops).

### C. A + B + HL mark formula

* On-chain per-market EMA state (150 s of mid − oracle; 30 s of book
  median), book mid / last trade from the book at block start, CEX perp mids
  from the feeder; mark = median of the three (+ fallback).
* Pros: HL parity for margin / liquidation. Cons: most state and code;
  EMA in blocks (time-based → block timestamps); more consensus surface.
* Effort: Large. Risk: Medium-High.

## Recommendation

A now, on this branch (it is the "item 2" as listed), then B as its own item
(it needs ops decisions), and C after liquidation works, if still wanted.

## Not Building (YAGNI)

* Governance-tunable oracle params (window, max age, min reporters) — constants for now.
* Funding (no funding exists).

## Decisions (user, s517)

1. **Scope:** option A only, on branch `feat/oracle-aggregation` (stacked on
   `fix/parity-audit-bugs` @ 3e9365e). Linear stack, each branched once the
   previous one is satisfactory: A → liquidation → B (feeder) → C (HL mark).
2. **Min reporters:** keep the fixed 3 (as coded). On a 3-validator net all
   three must be submitting for the price to stay fresh.
3. Hook point: start of block, before `execute_batch(pre_evm)` (one mark per block).
4. **Stake quorum (review M1):** a fresh aggregate needs >= 3 reporters AND
   their stake > 2/3 of the total Active stake (`3 × reporting > 2 × total`,
   exact integers). Both counts use the reporters left after the outlier cut:
   the cut uses the unweighted median, so counting before it would let a
   low-stake head-count majority cut the honest high-stake reports and set
   the price. Otherwise the last price is kept and goes stale. HL documents
   only a stake-weighted median; > 2/3 matches the BFT honest-stake bound.
5. **Timestamp lower bound (review M2) — built for the testnet genesis
   (owner s138, `fix/m2-timestamp-lower-bound`); status at the end of this
   item.** A
   byzantine leader can propose `ts = parent.ts` after a stall, so an old
   aggregate has age 0 in that block. A "reject ts < local − 30 s" rule
   cannot be enforced today: every body (fresh or recovered late) reaches
   the app through the same `try_insert_body` → `validate_block` path, so
   the rule would also reject honest bodies validated > 30 s late (a
   network-wide pause could then wedge an already-certified block), and a
   hotstuff "fresh" flag is bypassable because the leader can withhold the
   body (replicas vote on headers first). Exposure: only blocks proposed by
   byzantine leaders — the next honest proposer uses `max(now, parent.ts)`
   and the old price goes stale. Fix with the consensus item "validate
   before voting (CometBFT-style)": once bodies are validated before the
   vote, the 30 s lag check is enforceable like CometBFT PBTS.
   **Status (owner s131): pre-mainnet item, NOT in the testnet genesis.**
   It was built on `fix/pretestnet-p0-p1` as a pre-vote check (`ecba72ca`),
   then given an exemption for authenticated re-proposals (`c779d72f`,
   hotstuff `App::check_reproposal_data`) and TC tip power counted under the
   signing set (`62a6f8f4`). All three are reverted on that branch and kept
   on `wip/m2-timestamp-lower-bound` (`62a6f8f4`). The reviews found two
   wedges:
   - **Round 1 (Codex P1):** after a TC whose winning high tip is an
     uncertified block, the leader must re-propose that exact block and
     cannot refresh its timestamp. After a stall over 30 s, every replica
     refused it forever.
   - **Round 2 ([claude-review]), against the exemption:** one byzantine
     validator signs `local_tip` = B in its timeout vote, where B is a safe
     block with ts = parent.ts. `TipInfo.view` is unsigned, so B wins the
     TC and the honest leader must re-propose B. `tip_vouched` sees power 1
     (not > f), and header-first votes (`cast_header_vote`) never set
     `local_tip`. So every honest replica refuses B, in every view.

   (s131 plan, superseded: land after genesis through P0.0 gating.)

   **As built (s138, on main's TC-required set).** Main now validates bodies
   before the vote and signs tips (F1-F5: a tip is a leader-signed header,
   voters sign the tip view, header-first votes set the tip), so round 2's
   forged view and missing tips are gone. The rule:
   - `check_timestamp_lag` (torus-consensus `app.rs`): no vote when
     `ts + 30 < local clock`, next to the 5 s ahead bound, pre-vote only
     (`check_proposal_data`). Never at insertion, block sync, execution or
     crash replay: a certified old block is always accepted, so catch-up and
     replay never read the local clock.
   - It applies to FRESH blocks only. hotstuff asks
     `App::check_reproposal_data` (every rule but the lower bound) when
     `HotStuff::freshness_exempt` holds: the block is certified (this
     replica's highest or locked PC, or the TC's high QC: the VSU
     re-proposal, including the reflexive-lock re-proposal of the locked
     block), it is the block this replica last voted for, or it is a TC
     re-proposal whose tip is this replica's own tip or is vouched by signed
     tips of more than f power (`TimeoutCertificate::tip_vouched`, counted
     under a set that authenticates the TC, as `62a6f8f4`).
   - Round 2's wedge in its main-era form (a byzantine LEADER proposes B
     with ts = parent.ts, nobody votes, it reports B as its tip, B wins the
     TC): the next honest leader does not re-propose a tip it would refuse
     itself on a freshness rule alone and that is not exempt for it
     (`refused_as_stale`). It runs RECOVER without the block request (NE
     requests only, case 4) or drops a recovered tip (case 5); honest
     replicas never voted B, so they sign No-Endorsements and the NEC block
     replaces B. The hole stays closed and the chain moves on.
   - Residual (liveness, needs > 30 s of asynchrony): an uncertified tip B
     voted by between f + 1 and 2f of the power while it was new, stale
     after a long partition, with at most f signed B-tips in the TC: the
     non-voters refuse it and no NEC forms while B's voters refuse NEs at
     `tc.view`; a later TC that vouches B (or an NEC once
     `tc.view` moves past B's view) resolves it. Skewed clocks: a voter more
     than 30 s ahead refuses every fresh block (it acts as a non-voter;
     more than f such power halts the chain until the clocks are fixed), a
     voter more than 5 s behind refuses by the ahead bound (as before); a
     leader whose clock is behind proposes `max(now, parent.ts)` and passes
     in steady state, after a stall its blocks are refused and its views
     time out.

## Notes (review, s517)

* **M3:** the EVM section of block h runs before `begin_block_oracle`, so
  EVM transactions in block h read block h−1's aggregate while native
  actions read block h's. Deterministic; documented, not changed.
* **L2 (upgrade):** old 39-byte submission keys match the new
  `"sub"‖market` prefix and decode (same value layout); they are pruned by
  the first block-start step. The T0 epoch fix changes the replay of old
  empty boundary blocks. Fresh genesis; old history is not replayed with the
  new binary.
* The weighted median picks the lower price at exactly half the stake.
* `RpcMarkPrice.timestamp` returns the aggregate's block number, not a time
  (pre-existing).

## Open Questions

1. Staleness: 100 blocks / window 10 blocks — confirm in seconds at today's
   block time during planning.
2. **Block timestamp — answered (s517, T0b + T1).** The header timestamp is the
   proposer's wall clock in seconds, hashed into the block identity; every
   execution path (live dispatch, `execute_committed_block`, crash replay) reads
   the COMMITTED header, and oracle submission rows store it
   (`oracle_clock_is_the_committed_header_timestamp_on_every_path`). It was not
   validated; from T0b (placement per rebase s87, owner option A) a block with
   `ts < parent.ts` is refused by `check_parent_link` (before the vote and on
   insertion: a pure header comparison), and a replica refuses to VOTE for a block
   with `ts > local clock + 5 s` (pre-vote check only; never at insertion, block
   sync, execution or replay, so a certified block cannot wedge the chain). The
   proposer uses `max(now, parent.ts)`. A certified block was voted by a quorum
   whose honest members checked the drift bound, so its timestamp is at most
   ~5 s ahead of an honest clock; the oracle's ages still clamp at 0
   (`saturating_sub`). Validators need NTP-synced clocks.
