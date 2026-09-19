# Cancel-all action attribution

Candidate based on `0571fba`, motivated by accepted deeper-book cell
`s60-deeper-fixed-off-10m-r1`: phase1 increased from 122.5 to 805.4 ms/native
block between early and late load windows. This is instrumentation, not a new
cancellation algorithm or a performance improvement.

Exact `TORUS_CANCEL_ALL_DIAG=1` enables one log per completed cancel-all action.
The default performs no added clock reads or logging. Existing book iteration,
order removal, margin arithmetic, clamp and ignored balance errors are preserved.
The diagnostic separates book removal, per-order margin calculation and balance
read/update/write. It records visited existing markets, removed orders, balance
read/write errors, total elapsed time, residual time and a partition-valid flag.
Market collection, dirty-book bookkeeping, vector destruction and instrumentation
overhead lie outside the three named spans. Logging follows the elapsed stamp
and remains inside phase1 when this action is called by batch execution.

These are local elapsed durations, including descheduling. They do not identify
individual order-book subroutines, pure CPU time, or causal throughput gains.
Records contain block height and market scope, not trader identities or order
payloads. Panic paths do not emit a completed-action record.

Authored tests compare ON/OFF results, all persisted column-family rows, book
rows, level commitments and journals for targeted/all/missing markets, repeated
empty cancellation, clamped refunds and malformed balance reads. The existing
integer-margin tests additionally compare production behavior with the prior
arithmetic implementation. Tests and a separate node build must pass before use.

Qualification receipt `bc2febfa-1412-4c6c-a941-8383d523b494` passed on
2026-09-19: two new fixtures, ten margin tests OFF and ten ON, 27 native
integration tests ON (49 executions, 39 unique), and a separate node build.
Source fingerprint was unchanged. Live attribution follows.
