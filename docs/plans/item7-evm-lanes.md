# Item 7: EVM lanes (design, s99)

Status: step 1 built (s123, see below); the rest is design. Owner decision s99: item 7 is its own
item; design now, build after item 6 Phase 2 (before Phase 3 and before testnet; a block-format
change needs a fresh genesis, which is free before testnet). Context: `item6-phase1-impl.md` 9.16.

**Status s121: overdue.** Steps 1-2 were not built after Phase 2 as decided; item 6 Phases 3 and 4 went
ahead without them (no recorded decision moved item 7). No damage yet: no testnet exists (fresh genesis
still free) and the Phase 3/4 benches were native-only. Order from here (owner s121): item 7 step 1, then
step 2, built now while archy runs liq-stress, job 4 and Phase 5 step 0; then item 6 Phase 5 build steps;
then roadmap Tier 5 (block cadence and p99 floor). Step 1's open details (N for "EVM every Nth block",
the per-block EVM gas budget) go to the owner before building.

**Status s123: step 1 built** on `feat/item7-step1-evm-lanes` (commits C1-C7), not merged yet. Design
`item7-step1-design.md` (option A, owner decisions s123), plan `item7-step1-impl.md`. Header field
`evm_block_interval`, genesis 3M / N = 5, governance keys `evm_block_gas_limit` (1M..=10M) and
`evm_block_interval` (2..=100) switching at epoch boundaries, pre-vote cadence / count / 256 KiB caps,
admission caps, lost-proposal reinsert, routing to the next EVM height's leader, RPC gas at the header
limit, `getOrderBook` removed. Next: step 2.

**Owner decisions (s99):** the target is **option C** (HL-style small and big EVM blocks), built in
two steps: step 1 = A + B (consensus EVM budget per block, EVM only every Nth block), then step 2 =
C right after (block types with their own limits and cadence, two EVM pools, big-block opt-in,
RPC / fees); A + B are the base C needs anyway.
Start with HL's EVM throughput: small blocks 3M gas every second, big blocks 30M gas once a
minute (about 3.5M gas/s in total). Step 0 (vote-side EVM header checks) is approved and being
built now on `fix/evm-vote-checks`, ahead of the rest of item 7.

**Owner decisions (s122), step 1 (A + B):**
- Cadence: height based, EVM txs only when `height % N == 0`. **N = 5 for now** (~1.55 s between EVM blocks at
  ~3.2 blocks/s, 300 markets, section 71). Time based like HL needs whole-second header timestamps changed; not now.
- Budget: **3M gas per EVM block for now** (~1.9M gas/s at N = 5, about 0.6x HL's small blocks). The rule is step 0's
  vote check (declared tx gas limits sum <= header `evm_gas_limit`, which equals the parent's, from genesis); step 1
  sets the genesis value to the budget. A tx above 3M (a 24 KB contract deploy, ~5M+) waits for step 2's big blocks.
- N and the budget: genesis values plus a governance key (like `price_band_bps`); step 0's "equals the parent" rule
  gains "unless governance changed it at this height".
- Base fee: fixed (1 gwei, as step 0 enforces) in step 1; EIP-1559 per block type in step 2 (HL parity).
- Per EVM block caps: tx count <= budget / 21,000 (never binds an honest block) and EVM tx bytes <= 256 KB
  (>= the 48 KB initcode limit).
- Read precompiles: `getOrderBook` (0x0800) is removed in step 1 (Classic, the testnet layout, reverts on a real book
  today, so no user loses a working read; `getOpenOrders` was already removed in s99). Step 2 adds an HL-style best
  bid / ask read (HL `bbo`, 0x080e: two numbers, no depth), which needs a small best-price row per market for Classic.
- Values are provisional: the D1 mixed-load bench (section 4, step 2) re-tunes N and the budget on the step 1 binary.
- Step 1 keeps the budget, cadence and cap checks independent of the execution order (EVM first today), so step 3
  only changes execution.

**Step 3 (open, owner s122): isolate trading from EVM, chosen after D1.** Step 1 rations EVM; each EVM block still
runs serial (no pipelining) because revm and the precompile journal read and write RocksDB directly. If D1 shows the
serial EVM block costs real blocks/s at N = 5, pick ONE of (both keep A + B, both need EVM reads and writes through the
overlay / state layers first, both after item 6 Phase 5, which shares the EVM write path via its decision 4):
- **HL style:** trading first, then the EVM part in the same block, then EVM -> native transfers and CoreWriter
  actions (HL's order: L1 block, EVM block, EVM -> Core transfers, CoreWriter actions). Contracts read this block's
  post-trade state. Est. blocks/s ~3.1-3.2 at heavy EVM load (EVM run time on the exec thread, unmeasured), matched/s
  ~-1-2%. Size L + M. HL parity.
- **C, parallel:** EVM on its own thread over a block-start snapshot while trading runs; results merged at freeze
  in a fixed order. Contracts keep today's block-start reads; native -> EVM transfers wait for the next EVM block.
  Est. blocks/s ~0 loss, matched/s ~0, +1 core in EVM blocks. Size XL, highest risk (concurrency inside deterministic
  execution; every EVM <-> native write path audited first).
- Either changes EVM-visible results: free at a fresh genesis before testnet; after testnet it needs activation-height
  gating (roadmap P0.0, not built). So decide before testnet if D1 says it is needed. Not both in sequence (contract
  behaviour would change twice).
- Also for step 2: HL delays CoreWriter order actions a few seconds so contracts cannot bypass the L1 mempool; ours
  wait one block (~0.3 s).

## 1. Problem

EVM transactions and native trading share every block, and EVM work slows the trading in it.

- **One block, both payloads.** `build_proposal` (`app.rs:5324`) builds one `TorusBlock` with
  `native_actions` and `evm_transactions` (`app.rs:5387-5408`); header fields
  `evm_gas_limit` / `evm_gas_used` / `evm_tx_count` / `base_fee_per_gas`
  (`torus-types/src/lib.rs:468-474`).
- **EVM runs first, then trading.** Execution order: EVM section (`validate_block_for_catchup`,
  `app.rs:1854-1921`) -> `seed_from_bundle` (2169) -> native `pre_evm` and `post_evm` batches
  (2319-2320; both run after the EVM despite the names; split at `native_executor.rs:10414-10428`)
  -> `drain_core_writer` (2354) -> liquidations.
- **Any EVM tx turns off pipelining for its block.** `has_evm` sends the block down the serial
  path: the node waits for the flush worker, then executes serially (`app.rs:1755-1763`), because
  revm and the precompile journal read and write RocksDB directly instead of the layered overlay
  (`consensus-bug-c-evm-commit-atomicity.md:38`). Blocks without EVM txs pay nothing.
- **EVM is never paced.** Native selection shrinks with the execution backlog
  (`paced_selection_caps`, `app.rs:5900-5947`); `drain_evm` (`app.rs:5949`,
  `torus-mempool/src/lib.rs:468`) runs in every tier, including "CancelsOnly".
- **The only EVM limit is proposer-local.** Budget = min(header `evm_gas_limit` 30M,
  `TORUS_EVM_BLOCK_GAS_BUDGET`, default 5M, read once per node; `rate_limit.rs:21,34-42`), 25% of
  it per sender (`rate_limit.rs:26`). Validators do not check it when voting.
- **Consensus gaps (security, independent of the lane design).** Votes check neither EVM gas,
  `evm_gas_limit` nor `base_fee_per_gas`. The proposer copies both from the parent, so a dishonest
  proposer could raise them. The EIP-1559 check (`torus-bridge/src/validator.rs:120-131`) is only
  in `validate_block_with_parent`, which consensus never calls, so the base fee is frozen at
  1 gwei. The only hard stop is at execution: cumulative gas over the header limit fails the whole
  EVM section (`torus-evm/src/executor.rs:325-330`).
- **Rates.** Under load main commits about 1.6 blocks/s (`ozarchy-antispam-item6-pf1-2026-10-04.md`
  702-707): the 5M default allows about 8M EVM gas/s; the 30M header limit about 48M gas/s.
- **Cost not measured.** No measurement of an EVM block's serial-path cost under mixed load (D1
  bench open, `evm-blocker-set-decisions.md:107`). Closest proxy: an epoch-boundary serial block,
  h700 1.13 s vs a 674 ms mean (`docs/perf/s65-explorers/killed-runs-finished.json:679`).

Related: read-precompile gas (s99 decisions in 9.16) was sized against a 30M-gas block; with
the 5M proposer budget an honest block is 6x lighter, but nothing stops a 30M block today.

## 2. How Hyperliquid does it

- HyperCore (trading) and HyperEVM have separate block streams on one chain and one consensus.
- Dual EVM blocks: fast blocks 3M gas every 1 s, slow blocks 30M gas every minute; two
  independent EVM mempools; users opt into big blocks with `evmUserModify{usingBigBlocks}`; the
  first L1 block in each time window produces the EVM block
  ([dual-block architecture](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/dual-block-architecture)).
- Read precompiles see "the latest HyperCore state at the time the EVM block is constructed";
  CoreWriter actions are "delayed onchain for a few seconds"; read gas
  2000 + 65 x (input + output bytes)
  ([interacting with HyperCore](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/hyperevm/interacting-with-hypercore)).
- Effect: EVM gets a fixed slice of throughput per second, so a heavy contract load can only
  touch a few L1 blocks, never every block.

Torus today, for comparison: EVM can be in every block; writes (CoreWriter, lockbox
0x0810/0x0811/0x0820) are queued for block N+1 and drained after its native batches, so an order
first sees a block-N deposit in block N+2 (`precompiles.rs:1370,1394`, `app.rs:1991-1994`).

## 3. Options

### A. EVM gas budget per block as a consensus rule
Make the per-block EVM budget a rule every validator checks at vote time (sum of tx gas limits
<= budget, or a new header field), set in genesis and changeable by governance.
- Changes: vote-side check; budget source (genesis / governance param); `add_evm_tx` must keep
  accepting the largest deploy (`lib.rs:400-403` rejects any tx above the budget).
- Gains: closes the "dishonest proposer packs 30M" gap; caps EVM time per block.
- Does not fix: every block with an EVM tx still loses pipelining.

### B. EVM only every Nth block
EVM txs allowed only when `height % N == 0`; the other N-1 blocks always pipeline.
- Changes: proposer gate in `select_block_payload` before the `drain_evm` call (no consensus
  change) plus a vote-side rejection of EVM txs off-cadence (consensus). Header timestamps are
  whole seconds, so a time-based cadence like HL's is a bigger change; height-based is simple.
- Gains: (N-1)/N of blocks keep pipelining no matter the EVM load; combined with A, EVM gas/s is
  fixed (budget x blocks/s / N).
- Costs: EVM latency up to N blocks; CoreWriter / lockbox delay becomes uneven (N+1 rule fires
  only in EVM blocks unless the queue drain stays per block, which it does today).

### C. HL-style small and big EVM blocks
Two EVM block types with their own gas limits and cadence (for example small every block or
every few blocks, big once a minute), two EVM pools, a header type field, user opt-in for big
blocks, fee estimation per type.
- Changes: block format (type field), two pools (the drain is destructive per pool), consensus
  checks per type, a user action like `evmUserModify`, RPC / fee changes.
- Gains: HL parity; big deploys possible without slowing every block.
- Costs: the largest change; needs A and B underneath anyway.

## 4. Recommendation (18c)

Do it in steps; each step stands on its own.
1. **Step 0, consensus gaps (built s99 on `fix/evm-vote-checks` `72ee965a` + review follow-ups
   `4164382d`; also checks the header `evm_tx_count` against the body, which closed a crash-replay
   divergence; merge after ozarchy's suites):** validators check
   `evm_gas_limit` and `base_fee_per_gas` against the rule (genesis value until governance can
   change it) and the EVM gas used against the limit at vote time; wire the EIP-1559 check or
   decide the base fee stays fixed on purpose.
2. **Measure first (ozarchy):** the D1 mixed-load bench: matched/s and block time with EVM load
   at 0 / 5M / 15M gas per block, EVM in every block vs every Nth block. This sizes A and B.
3. **Step 1 = A + B:** consensus budget per block plus EVM every Nth block (N and the budget
   from step 2). Most of HL's benefit at a fraction of C's cost. Also a cap on EVM tx count or
   bytes per block (step 0 review): undecodable or 0-gas txs count 0 toward the gas rule, so
   today only the datum size limits how many a block carries.
4. **Step 2 = C, only if needed:** if contracts need big deploys or step 1 limits real usage.

## 5. Open questions (owner)

1. ~~Step 0 now or with item 7~~: now (s99).
2. ~~Target EVM share~~: HL's numbers to start (s99): 3M gas/s small + 30M per minute big.
3. ~~Cadence~~: height based, N = 5 for now (s122).
4. ~~Base fee~~: fixed in step 1, EIP-1559 per block type in step 2 (s122).
5. ~~Scan reads~~: `getOrderBook` removed in step 1, best bid / ask read added in step 2 (s122;
   `getOpenOrders` already removed s99).
6. Step 3 (trading isolated from EVM): HL style or C, after D1 (s122, open).

## 6. Unknowns

- Serial-path cost of an EVM block under mixed load (no measurement).
- What happens to drained EVM txs when a proposal never commits (`reinsert_evm`,
  `torus-mempool/src/lib.rs:455`; callers not traced).
- Whether the EVM section can move onto the overlay (removing the serial fallback entirely,
  consensus-bug-c follow-up); that would shrink the problem B solves.
