# Pass 23 — delegation exit and reward eligibility

Reviewed 2026-10-04 against `perf/item6-phase1` at
`0a255607dbe2227ee47b70638b0198b80eaeb3cd`. Focus: whether ordinary exits
remove a delegation row from validator-inflation recipients before its
unbonded principal is claimed.

**Reconfirmed F45; no new finding.** Full undelegation sets the active amount
to zero but preserves the row while the unbonding queue holds principal. The
reward path fetches the full validator delegation list, uses its last row for
the rounding remainder, and does not filter that row by positive active amount.
If an unbonding-only delegator sorts last and there is a rounding residual, it
can receive the residual as pending rewards. The recorded case is one wei per
allocation under its stated preconditions; this pass does not claim material
loss or extra emission.

The current [`undelegate`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/staking.rs#L143)
continues to retain such rows. [`distribute_validator_inflation`](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-economics/src/rewards.rs#L232)
selects the final row at L235 and assigns it `delegator_pool - del_distributed`
at L238-L245. The eligibility condition is `val.total_delegated > 0`, which
only establishes that someone is delegated; it does not prove every row is an
active delegator. F45 in the pass-12 staking report already models this exact
recipient-order seam, so I do not count it again.

A targeted regression should create two positive delegators and a later-sorting
zero-active row with an unbonding claim outstanding, run validator inflation,
and verify the residual goes only to an eligible active recipient. No test was
run in this pass.

## Limits

Static source recheck at the current pinned SHA. No Rust tests or reward
execution were run. No source changes were made.
