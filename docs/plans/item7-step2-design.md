# Design: item 7 step 2, HL-style small and big EVM blocks

Status: brainstorm (s123), for owner review before writing-plans. Builds on step 1 (`item7-step1-design.md`,
`item7-step1-impl.md`, branch `feat/item7-step1-evm-lanes`). Step 2 scope (s99, s122): block types with their own
limits and cadence, two EVM pools, big-block opt-in, RPC and fees (EIP-1559 per block type), the best bid/ask read
precompile, and the CoreWriter delay. Code facts from a read-only scout on main `2308da3d` and the step 1 branch
`ae437be6`; "inferred" marks what was not read.

## Problem

Step 1 gives one EVM lane: 3M gas every 5th height. A tx above 3M (a 24 KB contract deploy, ~5M; the Uniswap Router
~4.2M) can never land. HL solves this with a second, slow lane (30M gas once a minute) that users opt into, fees per
lane, a cheap best bid/ask read for contracts, and a few seconds of delay on CoreWriter order actions so contracts
cannot jump the L1 mempool.

## Context (memory + code)

- **Every EVM block is a serial barrier until step 3** (`app.rs:3012-3021`: checkpoint drain + wait idle). Per-gas
  cost from ozarchy's read-precompile bench (`docs/perf/read-precompile-gas.md:74-114`): 4.17 ns/gas pricing target
  (125 ms per 30M), cold point reads ~7 ns/gas (~216 ms), tombstone scans 27.3 ns/gas (~820 ms). At 300 markets exec
  lag sits at the 64-block channel (section 73), so serial EVM time reaches consensus about 1:1. Estimate (inferred):
  a 30M block costs a 0.15-2.5 s spike; views time out above ~1.2 s. Nothing is measured (D1 never ran).
- **Lanes cannot be a consensus rule.** A per-account "uses big blocks" flag is executed state, and votes cannot read
  executed state (step 1 finding). Lanes must be proposer-local routing; votes check only per-type caps.
- **One lane per sender, always.** If nonce n waits in the big pool and n + 1 in the small one, the small drain takes
  n + 1, execution skips it as nonce-too-high (`executor.rs:330-334`) and the tx is gone.
- **Pools:** base fee, eviction index and drain all live in one `EvmPool` (`evm_pool.rs:125-169, 334-416`), so two
  pools get per-type fees almost free. `pending_nonce` (`lib.rs:1597`) must span both.
- **No per-account setting action exists** (`NativeAction`, `torus-types/src/lib.rs:709-847`). Model:
  `SetOracleSigner`'s hashed row read by both execution and admission (`cf.rs:144-150`). A new
  `EvmUserModify { using_big_blocks }` would be tag 30, not a trading action (session keys cannot sign it).
- **EIP-1559 has no gas signal at vote time.** `calc_next_block_base_fee` exists (`torus-evm/src/eip1559.rs:15-41`)
  but nothing calls it in consensus, and headers always carry `evm_gas_used: 0` (`app.rs:7165`): execution lags.
  The only pure per-block signal is declared gas (sum of tx gas limits), which a user can inflate for free (refunds).
- **CoreWriter:** actions target `block + 1` (`precompiles.rs:1449-1464`) and drain at that exact height after the
  native batches. A delay of K blocks is a one-line target change, per action kind if wanted. One 30M block can queue
  ~1,500 actions (20,000 gas each) into one native phase, with no cap today.
- **Best bid/ask:** `best_bid`/`best_ask` are in memory per resident book (`order_book.rs:2326-2334`);
  `dirty_books` lists the markets changed per block. A (bid, ask) row per dirty market at block end is at most 300
  small puts per block. It cannot live in `CF_NATIVE_ORDER_BOOKS` (unknown keys are errors, `book_reader.rs:141-160`).
- **Other limits that bind big blocks:** the proposer-local `TORUS_EVM_BLOCK_GAS_BUDGET` (default 5M) caps both drain
  and admission (`lib.rs:605-608, 674`); the consensus message cap is 1 MiB (`torus-network/src/config.rs:276`), and
  30M gas of calldata could be several MB; step 1 forwards each tx to one leader, so for a once-a-minute big height one
  view timeout means a missed big block.

## Options (how big blocks are scheduled and switched on)

### Option A: big heights by height cadence, switched off until measured (recommended)
- The header and the epoch plan gain `evm_big_block_interval` (M) and `evm_big_gas_limit`, governed exactly like
  step 1's N and budget (PlanEvm, epoch-boundary switch). A block with `height % M == 0` is a big EVM block.
- **Genesis M = 0 (off).** Step 2's code lands complete, but big blocks only start when governance sets M after
  D1 (or step 3) shows the serial cost is acceptable. Until then a deploy above 3M can use a temporary budget raise
  inside step 1's range (up to 10M).
- At ~3.2 blocks/s, M = 190 is about once a minute; under load blocks slow down, so big blocks get rarer, which
  protects block time (self-throttling).
- Pros: pure checks, mirrors step 1's code, no p99 risk until measured. Cons: the big-block rate follows the block
  rate, not the wall clock (HL is wall-clock).
- Effort M-L. Risk M (low while M = 0).

### Option B: big blocks by wall-clock window (HL parity)
- A block is big when it is the first in a new 60 s window: `ts / 60 > parent_ts / 60` (header timestamps are whole
  seconds, enough for a minute window). Pure header check (the parent is known in `check_parent_link`).
- Pros: exactly HL's once a minute, at any block rate. Cons: a proposer can shift its timestamp by up to 5 s (drift
  rule) to make or skip a big block; big blocks keep coming at full rate under load, when serial time hurts most.
- Effort M-L. Risk M-H (block time under load).

### Option C: step 2 without big blocks; big blocks with step 3
- Build the opt-in, the two pools, fees per type, bbo and the CoreWriter delay now; build big-block scheduling with
  step 3, once EVM no longer runs serial.
- Pros: no serial 30M block ever hits trading. Cons: the opt-in and the big pool do nothing until then; big deploys
  wait for step 3 (or a temporary budget raise).
- Effort M. Risk L.

### Same in every option
1. **Two pools, one lane per sender.** `EvmUserModify { using_big_blocks }` (native action, tag 30, hashed row) sets
   the lane; admission reads it from the ingress view and refuses a tx while the sender still has pooled txs in the
   other lane. Lanes are proposer-local routing, not a vote rule.
2. **A big height drains both pools:** the small pool up to the small limit first, then the big pool up to the big
   limit. Votes check per-height totals: count, declared gas <= small + big limit, bytes <= a big-block byte cap that
   fits the 1 MiB consensus message (proposed 768 KiB).
3. **Budgets per lane:** the env budget stops capping the big lane (header limits rule).
4. **Routing:** small-lane txs as in step 1, plus decision 7's re-send; big-lane txs go to the leaders of the next two
   big heights, so one view timeout does not cost a whole minute.
5. **bbo precompile at 0x080e:** a (bid, ask) row per dirty market written at block end in both save paths, outside
   the book column family; deleted on delist; read with the existing metering (address checks widened past 0x0803).
6. **CoreWriter:** order and vault-transfer actions wait K blocks (HL-style), others keep N + 1; a per-block drain
   cap moves the excess to the next block, so a big block cannot dump ~1,500 actions into one native phase.
7. **RPC:** `eth_estimateGas` caps at the sender's lane limit; gas price per lane (`eth_gasPrice` for small, a
   `torus_bigBlockGasPrice` like HL's `bigBlockGasPrice`); `eth_feeHistory` per type; an RPC to read a sender's lane.

## Fees per block type (sub-decision)

- **F1, fixed fee per type:** two governance values, no dynamic pricing. Simplest; no HL parity on fees.
- **F2, EIP-1559 per block from declared gas:** the header carries both base fees and the declared gas; each type's
  next fee follows its previous block. Pure, but declared gas can be inflated for free (unused gas is refunded),
  so a griefer can push fees up and fill blocks cheaply.
- **F3, EIP-1559 per epoch from executed gas (recommended):** execution of boundary H sums each type's gas used over
  the epoch and writes the next base fees into plan(H + L), like step 1's PlanEvm. Votes stay pure, the signal is
  real gas used, and it reuses step 1's mechanism. Slower reaction (one change per 100 blocks, ~30 s).

## Recommendation

Option A with genesis M = 0, plus F3. Step 2 lands complete and HL-shaped, but no serial 30M block reaches trading
until D1 (or step 3) says the cost is acceptable; height cadence self-throttles under load, which protects block speed
and matched/s; and both the big-block switch and the fees reuse step 1's epoch-plan path, so votes never read
executed state.

## Not Building (YAGNI)

- A time-based small-block cadence (step 1 decided height).
- Parallel or HL-style EVM isolation (step 3, after D1).
- Lane checks in votes (lanes are executed state; proposer-local routing is enough).
- Gossip of EVM txs to all validators (the in-flight filter it needs lands with decision 7; the re-send makes gossip
  unnecessary).

## Decisions (owner s123)

1. Option A: big heights by height cadence, genesis M = 0 until D1 (or step 3) measures the serial cost.
2. When big blocks are switched on, start at 10-15M gas (not HL's 30M). The governance range of the big-block limit
   tops out at 30M (HL's size), so growing to it takes a vote, not a release; D1 sets the actual values.
3. bbo (and any new read precompile) uses our existing metering: 16,400 base + 500 per row read + 20 per 32-byte word,
   priced to our measured RocksDB read cost (~7 ns/gas cold). **HL alternative kept for the future:**
   `2000 + 65 * (input_len + output_len)` (a bbo call: ~8,240 gas vs 16,440 with ours; owner s125: the row is
   priced like the other point readers, inside the base). Switch only if our read
   cost falls (e.g. resident rows) enough that HL's flat price no longer under-prices the exec thread's time.
4. CoreWriter: **D2**, order actions and vault transfers wait K = 10 blocks (~3 s at 3.2 blocks/s; genesis value plus
   governance key), everything else keeps block + 1; a per-block drain cap of 200 moves any excess to the next block.
   Alternatives kept for a revisit: D1 (keep one block for all; our drain already runs after the block's native
   batches, so no in-block jump) and D3 (wall-clock S seconds by header timestamps; exact at any block rate, but
   whole-second timestamps, up to 5 s proposer skew, and a time-ordered queue with a range drain).
5. Fees: F3 (final), EIP-1559 per epoch from executed gas.
6. Node budget (step 1 review finding gov-2, option a): `TORUS_EVM_BLOCK_GAS_BUDGET` loses its 5M default, so the
   header limits alone size both lanes; the variable stays as an optional per-node cap for A/B tests. The small lane
   keeps step 1's range 1M..=10M (default 3M): a 30M ceiling there would allow ~19M gas/s of serial EVM at N = 5,
   about 5x HL's total (~3.5M gas/s). Until step 2 lands, the gap stays documented in
   `docs/configuration-reference.md`, and no vote takes the small limit above 5M before D1 measures.
7. EVM tx routing fix (step 1 devnet finding: each tx is forwarded once, to one predicted leader, so a wrong guess or a
   late forward left it waiting for the receiving node's own EVM turn: 15-19% of txs took over 2 EVM heights, worst
   24 blocks; one tx sent just before N changed from 5 to 10 waited 661 blocks). Three parts, for both lanes (the big
   lane keeps its two-leader send, item 4 above):
   - **Re-send:** when the target EVM height commits without the tx, the node sends it to the next predicted leader,
     so a miss costs about one EVM height.
   - **Upcoming N:** the prediction uses the N scheduled for the target height (epoch plan) and falls back to the
     current N; the re-send catches a wrong guess.
   - **In-flight filter:** pooled EVM txs are dropped as soon as a committed block carries them, not after execution
     (`prune_committed_txs` uses executed nonces, `torus-mempool/src/lib.rs:1727`; the exec lag is ~14 s at load),
     so a re-sent tx is not proposed twice.
   Test: step 1's 4-validator devnet traffic check, expecting every tx within about one extra EVM height. A validator
   count that shares a factor with N still concentrates EVM leadership (n = 4, N = 10: 2 of 4 validators led the EVM
   heights); with the re-send it no longer strands txs (revisit before mainnet, step 1 decision 3).
8. Big-block byte cap: 768 KiB of raw EVM tx bytes, under the 1 MiB consensus message cap, which stays as it is.

**How HL produces EVM blocks (correction s123):** HL's EVM blocks are a separate EVM chain (own numbers and hashes),
but they are built while executing L1 blocks: EVM txs enter as onchain state (the "onchain mempool") through the same
L1 consensus stream as orders, and the first L1 block in each time window builds the EVM block from that pool. So EVM
tx bytes share the consensus stream with orders on HL too; they arrive spread over time instead of in one proposal.
That is also why HL can run per-block EIP-1559 on real gas used: the EVM block is a product of execution.
- **F4, fees computed at execution (noted, not chosen):** the base fee for each EVM block computed by execution from
  the previous EVM block's gas used, outside the vote rule (HL-like, per block). Problem here: our proposer picks EVM
  txs before execution, with a fee view that lags by the exec lag (~14 s at load), so after a fee jump some included
  txs fail the fee check at execution and are dropped from the pool. F3 avoids that.
- **Onchain mempool (noted for step 3):** EVM txs as L1 actions into a pool in state, EVM blocks built by execution.
  Would give exact per-block fees, no lost txs on failed proposals, no leader routing, and smaller proposals; but it
  redesigns step 1's lanes, writes every EVM tx to state twice, and adds inclusion latency.

HL facts checked s123 (official docs): fast blocks 1 s / 3M gas, slow 1 min / 30M; `evmUserModify{usingBigBlocks}`
at the user level; two independent onchain mempools; next 8 nonces per address; mempool txs pruned after 1 day;
`bigBlockGasPrice` RPC; no byte limit on blocks or txs is documented. CoreWriter: "order actions and vault transfers
sent from CoreWriter are delayed onchain for a few seconds", to remove any latency advantage over the L1 mempool. Read
precompile gas: `2000 + 65 * (input_len + output_len)`.

## Open Questions (owner)

None: fees (decision 5), CoreWriter delay (decision 4), big-block byte cap (decision 8) and bbo gas (decision 3) are
answered.
