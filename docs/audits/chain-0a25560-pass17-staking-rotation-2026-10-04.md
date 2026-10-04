# Pass 17 — staking, rewards and validator rotation

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. This is the first of five
additional focused passes requested after pass 16. It traces delegation and
undelegation, unbonding and reward claims, slashing, epoch reward allocation,
and planned validator-set rotation. The audit branch is separate from the
source branch.

**Result: no new finding is promoted in this pass.** The inspected reward and
rotation paths retain earlier candidate findings and qualifications; this
pass did not reproduce a new production failure. It is source inspection, not
a complete review of economics or a runtime test.

## Review notes

- [`claim_unbonded`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L240)
  calculates all matured delegation-row changes and the account credit before
  issuing one atomic write. This retains the pass-4 all-or-nothing conclusion.
  Ordinary `claim_rewards` and governance unlock use multiple manager writes;
  the production executor stages those operations on its per-block overlay,
  which is flushed with the block state. This static path review did not
  inject backend failures between manager calls, so it makes no independent
  fault-injection guarantee for direct manager use.
- The commission split and residual allocation in
  [`distribute_validator_inflation`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L165)
  were checked against active-set filtering, address ordering, delegated
  totals and the final-recipient remainder. The known malformed-genesis
  commission candidate E02 and F45's zero-active unbonding-row dust case are
  already documented; they are not counted again here.
- [`plan_rotation`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/epoch_plan.rs#L166)
  computes the next boundary from the post-boundary state, applies the
  deterministic rotation cap and minimum floor, and carries only a set that
  passes the configured minimum-set check. At the boundary,
  [`execute_planned_rotation`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/epoch_plan.rs#L255)
  applies the stored plan and records the installed set before planning the
  next boundary. This review found no new mismatch between those stored plan
  steps and their call sites.
- The current source still has unchecked U256 additions and height additions
  in parts of manager/reward code. This pass did not establish a reachable
  input that crosses those bounds under protocol admission, so they remain
  leads rather than numbered findings.

The earlier [economics audit](chain-cea1254-pass4-economics-2026-10-04.md),
[pass-12 staking report](chain-d52a33f-pass12-sol-staking-2026-10-04.md), and
[pass-13 execution report](chain-d9ef4f7-pass13-sol-execution-2026-10-04.md)
retain their finding numbers, source revisions and limits. Their issue count
is unchanged by this pass.

## Limits

No Rust tests, boundary execution, malformed-genesis initialization, storage
fault injection, chain replay or economic simulation was run. Test assertions
and source references were inspected only. No source changes or Torus issue
writes were made; no new finding was supported strongly enough to save.
