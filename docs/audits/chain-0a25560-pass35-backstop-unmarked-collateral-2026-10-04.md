# Pass 35 — liquidation backstop with mixed marked positions

Reviewed source `0a255607dbe2227ee47b70638b0198b80eaeb3cd` on `perf/item6-phase1`, focusing on the Backstop transition for accounts holding positions across markets with different mark availability.

## Finding — backstop transfers collateral while leaving unmarked positions behind

Backstop iterates the trader's positions, transfers only those whose markets have a mark, skips every unmarked position, then unconditionally moves the trader's full remaining collateral (`available + order_margin`) to the liquidator vault. The block liquidation step invokes this path when the account classifies as `Backstop`.

A concrete mixed-market state is enough: trader T has an under-maintenance marked position A plus a position B in a delisted, stale, or otherwise unmarked market. Backstop transfers A and leaves B in T, but transfers T's collateral to the vault. T retains an open position with zeroed collateral; when B later receives a mark, it may be valued/liquidated without the collateral that previously backed it. Depending on the signs of realized PnL and collateral, the vault can also absorb funds attributable to B while B remains owned by T. The code comment explicitly says an unmarked position stays with the trader, making the unconditional collateral move inconsistent with that preservation rule.

Evidence: [backstop skips unmarked positions then moves all collateral](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/liquidation.rs#L287), [Backstop dispatch](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-bridge/src/liquidation_step.rs#L108), [unmarked positions are documented as retained](https://github.com/OZmasterAI/Torus-hyperBFT/blob/0a255607dbe2227ee47b70638b0198b80eaeb3cd/crates/torus-core/src/liquidation.rs#L287).

Recommended regression: account with two positions, provide a mark for only one, force Backstop, and assert either that both position and corresponding collateral move consistently or that collateral supporting the unmarked position remains with its owner. Include later mark restoration and vault balance checks.

## Verification limits

Source-derived trace; no production test was run. This is separate from pass 3's off-mark-fill/ADL/withdrawal path and pass 16's cached liquidation valuation review.
