# Build-list fixes (ozarchy s17, 2026-10-06) — client & deploy notes

Five branches off main `59fa407`, one fix each, pushed, not merged; 18c
integrates them on `integrate/s94-batch`. Items 1-4 change
executor output (block results), so every validator must run the same build:
fresh genesis or an activation height. A mixed validator set forks on the
first block with a tipped EVM transaction (1), a reader precompile call (2), a
stop-type `placeOrder` (3) or an epoch boundary (4). Item 5 is node-local. After the merge: fresh devnet genesis, all nodes on the
same binary; wallet / feeder / bench default chain id stays 7778.

Every fix has a test that failed before it. A merge of all five: workspace
nextest 2839 passed, 0 failed, 0 flaky; doc tests 1/1.

| # | Branch | Bug | Change | Decision (18c, s94) |
|---|---|---|---|---|
| 1 | `fix/evm-gas-double-credit` | revm pays the priority tip to the beneficiary (the proposer), and `compute_fee_revenue` (`torus-bridge` `proposer.rs`) also returned `gas_used × effective_gas_price` into `distribute_fees`, which credits 90% of it again: tips were paid twice and blocks with tipped EVM transactions minted supply. | The distributor gets only the base-fee part: `Σ gas_used × min(effective_gas_price, base_fee)`. Proposer header, the three validator checks and the consensus exec path all pass the header's base fee. revm's tip payment and the fee split are unchanged. Header field `evm_fee_revenue` changes for blocks with tipped transactions. | Keep as fixed: tip to the proposer via revm, the distributor gets the base-fee part only. No burn for now (separate economics decision). |
| 2 | `fix/read-precompile-gas` | Readers 0x0800-0x0803 charged a flat 2,600 gas before the call, whatever they read or returned: a contract looping cheap `staticcall`s made every node read and encode whole books, market lists or validator sets (~11k full scans per 30M-gas block; `eth_call` is free). | 2,600 + 50 gas per unit (one row read, 32 bytes of a classic book blob, or one 32-byte word returned), on success and on revert. The reader gets a budget `(gas_limit − 2,600) / 50` and stops with out-of-gas once over: at most `remaining + 1` rows per scan (scans are prefix-bounded), and the classic blob is sized (`get_cf_len`) and charged before it is fetched. The book layout is decided per market from consensus (hashed) rows only; scanning a column family outside the state hash is an error. Uncharged: one node-local marker row in `cf_native_markets`, and RocksDB deletion markers inside a scanned prefix (until compaction; CPU only, gas stays deterministic). Three review rounds, the last with no blocking findings. | 50 gas per unit is a placeholder. Before testnet: microbench ns per unit (row read / 32 B blob / 32 B returned) and size it so a 30M-gas block of reads stays within the block exec budget. |
| 3 | `fix/corewriter-oid-stops` | `placeOrder` returned `((block + 1) << 64) \| seq`, which never matched the executor's real order id; stop types 2/3 were accepted without a trigger price and ran as Limit. | `placeOrder` returns `bytes32(0)` (fire-and-forget like HL's CoreWriter; read orders back with `getOpenOrders`). Stop types 2/3 revert in the precompile; `core_writer_to_native` errors on any type other than Limit / Market, so queued stop rows drain as errors. A contract passing the returned 0 to `cancelOrder` queues a cancel that fails. | Keep `bytes32(0)` (HL's `sendRawAction` returns nothing and HL delays CoreWriter orders); stop types reverting is fine. |
| 4 | `fix/inflation-self-stake` | Inflation, the fee validator share and `FeeSplitter` (`torus-economics` `rewards.rs`) weighted emission by self + delegated stake but gave everything after commission to the delegations: the validator's own stake earned nothing. | One helper, `split_validator_reward`: self-stake gets its pro-rata share, commission applies only to the delegators' share, the rest is split pro rata over delegations, the last taking the rounding remainder (value conserved exactly; U256 integer math; sorted delegation order). 8 existing tests that pinned the old split were updated. Pending rewards change at every epoch boundary (fee rewards too once the validator fee share is above 0 bps). Pre-existing: `delegations_for_validator` scans the whole delegation table per call (perf backlog). | OK. |
| 5 | `fix/liq-oracle-metrics` | No metric for a liquidator vault in deficit (negative cash, no positions, never acted on) or for oracle submissions evicted inside the mempool. | Gauge `torus_liquidator_vault_deficit` (set after each liquidation pass, one point read per block) and counter `torus_mempool_oracle_evicted_total` (a newer submission evicting the validator's oldest pooled one at the cap of 4). Listed in `docs/monitoring-setup.md`. | Added: RPC `torus_getLiquidatorVault` (`address`, signed `availableBalance`, `deficit`, `openPositions`; `docs/api/liquidator-vault.md`), `86c42f4`; noted in `docs/plans/liquidation.md`. |

## Pending: liquidation-stress cell (plan row 76), design summary sent to 18c

No full-node cell has fired a liquidation, and the standard bench shape cannot:
each sender holds 150 longs and 150 shorts across 300 markets on 100M TRS, and
the oracle walk is bounded and mean-reverting (`PriceWalk`, `BOUND_STEPS = 8`,
±8 steps), so a sender's loss stays near 1% of its notional against ~37% needed.
Proposal (standard shape, walk 10 + `ORACLE_FEED_DRAIN=1`):

- `LIQ_THIN=200` in `run-cell.sh`: bulk senders 60-259 get 1M TRS (edit of the
  generated genesis; includes the 50 digest accounts).
- `oracle-feed --shock-bp S --shock-round R`: from round R, odd markets +S bp,
  even markets −S bp, so thin senders on the losing parity lose on every
  position. S = 400 reaches stage 1; S = 700-800 reaches backstop, ADL and the
  vault.
- Add the liquidator vault to the digest accounts, so its balance (a deficit)
  is checked for AGREE.

Pass: `torus_liquidations_triggered` > 0 and identical on every node; AGREE
including thin accounts and the vault; no halt; the feed-live drain completes;
vault ≥ 0 at S = 400, any deficit cleared by ADL by the end of the drain at
S ≥ 700.
