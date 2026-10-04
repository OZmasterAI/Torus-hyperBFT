# Pass 30 — EVM fee revenue and beneficiary accounting

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: EIP-1559 effective gas
price, EVM beneficiary credits, fee revenue computation and the later fee split.

**Reconfirmed issue b108ffe0; no duplicate issue opened.** The EVM block
configuration sets the beneficiary to the proposer, and the normal revm
transaction transition credits that beneficiary the priority-fee component.
Torus then computes `gas_used * effective_gas_price` for receipts. That gross
amount includes both the base fee and priority fee. The committed EVM bundle
retains the EVM transition, while the native execution path separately passes
the gross receipt amount to fee distribution, which credits treasury,
development pool and, after transition, validator rewards. The scheduled burn
is only the excluded share. This can credit the proposer tip twice and
redistribute part of the base fee that the EVM transition burned; depending on
the tip/base-fee ratio it can produce a net supply increase. The issue was
previously saved as b108ffe0 at earlier source; this round confirms the same
flow is still present at `0a25560`.

The current path sets [`BlockEnvCfg.beneficiary`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-evm/src/executor.rs#L544)
to the block proposer, executes transactions through revm, and records the
computed effective price in receipts. [`compute_fee_revenue`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/proposer.rs#L395)
multiplies gas by that effective price. [`execute_committed_block_with`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L1818)
keeps the validated EVM bundle and computes the same gross fee revenue; later
in the block it calls [`distribute_fees`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-consensus/src/app.rs#L2305).
The split credits from that second path at
[`rewards.rs`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L57).

A regression should account for sender debit, base-fee burn, beneficiary tip,
fee-split credits and net supply for transactions with zero, low and high
priority fees. Settlement should split only proceeds actually retained by the
protocol, or replace the default beneficiary credit with the chosen protocol
fee routing. This pass did not execute that regression; the detailed historical
revm handler trace is retained with issue b108ffe0.

## Limits

Source call-path recheck only, using the prior dependency-level analysis for
revm fee settlement. No EVM transaction was executed and no balances were
reconciled on a running node. This is not a runtime reproduction or fix.
