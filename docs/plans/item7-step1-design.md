# Design: item 7 step 1, EVM lanes (A + B)

Status: brainstorm (s123); owner chose option A and the proposed answers (s123, "Decisions" below). Next: writing-plans. Decisions it builds on: `item7-evm-lanes.md`
lines 22-38 (s122). Code read on main `46428bfa`; "inferred" marks what was not read directly.

## Problem

Step 1 adds a consensus EVM budget (3M gas per EVM block) and a height cadence (EVM txs only when
`height % N == 0`, N = 5). It also adds per-block caps (tx count <= budget / 21,000, EVM tx bytes <= 256 KB), keeps the base
fee fixed at 1 gwei, removes `getOrderBook` (0x0800), and makes N and the budget genesis values plus a governance key.
The open design point is how a governance change of N or the budget reaches the vote-time checks.

## Context (memory + code)

- **Step 0 is on main.** `check_parent_link` (`app.rs:2065`) requires `evm_gas_limit` and `base_fee_per_gas` to equal the
  parent's (pure header check, runs pre-vote and at insertion). `evm_body_fits_header` (`app.rs:2134`) checks the tx
  count against the header and declared gas <= `evm_gas_limit` (pre-vote only, from `check_proposal_data`).
- **Vote-time code cannot read a governance value as of the parent height.** It sees only the durable DB, which trails
  consensus: the exec queue holds 64 blocks (`app.rs:5757`), and with nonblocking dispatch the lag in heights is unbounded
  (inferred). No governance param is checked by consensus today. Governance changes land in the native tail of a block
  whose height is not fixed (`governance.rs:1154, 1180`; timelock min 10, default 43,200 blocks).
- **The one existing consensus read of executed state is the epoch plan.** Execution of boundary H writes the plan for
  H + L (`epoch_plan.rs`, L = `epoch_length` = 100 in every genesis file). Consensus at a boundary needs durable applied
  height >= H - L, otherwise validate returns MissingData and propose is refused (`app.rs:6514-6570, 7098-7101, 7209`).
- **Genesis ignores the JSON EVM values.** `genesis_parent_header` hardcodes 30M / 1 gwei (`torus-bridge/src/proposer.rs:189-211`;
  callers `app.rs:1851, 1947, 2002, 2071, 6144`). JSON `evm.gas_limit` only feeds mempool admission (`main.rs:658, 682`).
- **Proposer:** `build_proposal` (`app.rs:7094`) knows the next height and passes the parent's limit to
  `select_block_payload`. `drain_evm` runs in every pacing tier and is destructive. `reinsert_evm` (`torus-mempool/src/lib.rs:656`)
  has no production caller, so a drained proposal that never commits loses its txs at that leader.
- **Per-sender share is safe at 3M.** A sender's first tx is always eligible (`evm_pool.rs:362-373`), so a single tx above
  the 25 % share still lands.
- **EVM tx routing assumes EVM in every block.** RPC unicasts each EVM tx to the current-view leader only
  (`torus-rpc/src/torus.rs:945`; EVM has no gossip). Leader = `view % n` with equal power (`hotstuff_rs/src/pacemaker/implementation.rs:1066`).
  Under the cadence a tx waits until its holder leads an EVM height: up to 5n blocks when gcd(n, 5) = 1 (n = 3: 15 blocks,
  ~4.7 s), and never (until a timeout shifts views) when n is a multiple of 5.
- **Mempool prune uses executed nonces** (`prune_committed_txs`, `torus-mempool/src/lib.rs:1638`), so a tx held by several
  validators can be re-proposed for the whole exec lag. Spreading txs to all validators needs an in-flight filter first.
- **RPC breaks at a 3M header.** `eth_call` without gas and `eth_estimateGas` use 30M (`eth.rs:339, 1123`), and revm rejects
  tx gas above the block limit, so both fail once the header carries 3M.
- **Sizes.** A 3M-gas tx can carry ~745 KB of zero calldata, so admission needs the byte cap too, or such a tx is never
  selectable. The consensus message cap is 1 MiB (`torus-network/src/config.rs:276`); today a 5M EVM block can exceed it
  (inferred).
- **getOrderBook:** 0x0800 serves `getOrderBook` and `getPosition` (`precompiles.rs:620-638`), so only the selector goes.
  Model: `fc1fb25a` (getOpenOrders removed, reverts `UnknownSelector`) and `1ac33b91` (tests: base gas only).

## Options (how N and the budget change by governance)

### Option A: both values in the header, changes switch at epoch boundaries (recommended)
- The header already carries `evm_gas_limit`. Add `evm_block_interval` (N). Off a boundary, both must equal the parent's
  (`check_parent_link`, pure). At a boundary they must equal the values in that boundary's epoch plan.
- Execution of boundary H copies the governance values into the plan for H + L, next to the validator set. Consensus
  already waits for that plan at every boundary, so this adds no new wait.
- Cadence and caps are pure functions of header + body: pre-vote, beside `evm_body_fits_header`.
- A governance change takes effect 100-200 blocks after it executes (30-60 s at ~3.2 blocks/s).
- Files: `torus-types` header + canonical bytes (29 literal constructions in 12 files), `epoch_plan.rs` + epoch step,
  `app.rs` (boundary rule, cadence + caps, proposer gate), governance allowlist (2 keys with ranges), genesis loader +
  JSON files, mempool admission, `eth.rs`, precompiles, tests.
- Pros: consensus reads executed state only where it already does; sync and insertion stay independent of execution;
  step 2's per-type limits can use the same header + plan pattern. Cons: header format change (fresh genesis, free
  before testnet); changes wait for an epoch boundary.
- Effort M. Risk M.

### Option B: gas limit in the header, N read from the current epoch's plan at vote time
- Same boundary switch, but no header field. The cadence check reads N from state whenever a block carries EVM txs.
- Pros: no header format change. Cons: a new executed-state read on every EVM vote. A validator whose execution is more
  than L blocks behind cannot vote on EVM blocks (MissingData); the lag is unbounded under nonblocking dispatch.
- Effort S-M. Risk M-H (liveness).

### Option C: genesis values only in step 1, governance keys later
- N from chain config, budget in the header (equals parent's, as step 0). No plan or header change.
- Pros: smallest. Cons: goes against the s122 decision; after testnet, a change needs a coordinated upgrade until
  governance lands. The D1 re-tune before testnet is free in every option (fresh genesis).
- Effort S. Risk L.

### Same in every option
1. Genesis: `evm.gas_limit` = 3M and a new `evm.block_interval` = 5 feed `genesis_parent_header`. Every repo genesis
   file gets them.
2. Proposer: skip `drain_evm` when (parent height + 1) % N != 0. Txs stay pooled.
3. Vote, pre-vote only (the step 0 pattern): reject EVM txs off-cadence, more than limit / 21,000 txs, or more than
   256 KB of EVM tx bytes.
4. Mempool admission: tx gas <= the current header limit (not ChainConfig's 30M), tx bytes <= 256 KB.
5. RPC: `eth_call` / `eth_estimateGas` default and cap gas at the header limit.
6. Remove the `getOrderBook` selector (keep `getPosition`), following `fc1fb25a`.
7. Routing: forward each EVM tx to the leader of the next EVM height (predicted from the current view and height),
   not the current-view leader. Keeps one holder per tx, so no new duplicates.
8. Tests: 2 existing tests change (`app.rs:13849` EVM at height 2; `app.rs:20874` 10M-gas txs at height 1);
   `make_block`'s 30M follows the genesis value. The 17 execution-only tests with off-cadence EVM stay (execution
   does not enforce cadence).

## Recommendation

Option A. Consensus reads executed state only at epoch boundaries, where it already waits for the plan, and every
other check stays a pure header + body function, so step 3 can change execution order without touching them. Its
costs are a header format change (free before testnet) and governance changes landing at the next boundary after the
plan, which is negligible next to the 43,200-block default timelock.

## Not Building (YAGNI)

- Time-based cadence (owner s122: not now). EIP-1559 (step 2). Big blocks / block types (step 2).
- Cadence or caps at insertion, sync or execution: pre-vote only like step 0 (a certified block had >= f + 1 honest
  votes); execution-side checks would also break 17 tests and change EVM results only beyond the fault bound.
- A new per-sender rule: the 25 % share already lets a sender's first tx use the remaining budget.
- Gossip of EVM txs to all validators: needs an in-flight filter, because prune uses executed nonces.

## Decisions (owner s123)

Option A, with these answers to the open questions:
1. Governance changes take effect at an epoch boundary, 100-200 blocks after the proposal executes (not "at this
   height").
2. Governance ranges, sized for matched/s and block speed until D1 measures an EVM block's serial cost: budget
   1M..=10M (every EVM block runs serial), N 2..=100 (N = 1 puts EVM back in every block). Widen at step 3 if D1 says so.
3. A validator count that is a multiple of N (one subset leads every EVM height) is accepted for now (testnet n = 3 / 4);
   revisit before mainnet.
4. Lost txs when an EVM-height proposal fails (`reinsert_evm` unused, pre-existing): fixed in step 1.
5. CI / devnet genesis keeps a higher budget until step 2 if the Uniswap e2e deploy needs more than 3M (verify in
   writing-plans).
6. The per-market tombstone compaction (`cf.rs:94-106`, `db.rs:940`) is deleted with `getOrderBook`, once
   writing-plans confirms nothing else range-scans that column family.
