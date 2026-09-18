# Economic locality: balanced owners and workload preparation

Candidate `perf/bench-locality-balance`, based on `9af0eea`. Torus issue
`9e090077-ed05-4b02-a618-f934a503cc4f`; attempt
`ca0c40ea-f3b0-48dc-b665-77e755b8a4be`. Verification passed 46 release Rust
tests and 76 Python harness/workload/health tests. Same-scope integration issue
`da5a85ee-9466-4a4c-80c4-65f1ae1c4cf8` has receipt
`15f18e8b-139c-4830-b7c7-e4acac9409a3`. No live cells have run with this
generator; test results are not throughput measurements.

## Defect and correction

Locality assigns sender `i` the markets `(i*K+j)%N+1`, for `j=0..K-1`.
The old economic side rule `(i+market)%2` balances all senders collectively,
but can give every actual owner of a market the same side. For `N=10,K=1`,
successive owners differ by ten; for `N=300,K=3`, they differ by one hundred.
Both preserve parity, so those locality workloads have no opposing traders
within each market. Existing tests only checked balance over all senders.

For genuine locality (`0<K<N`), this change alternates the actual owners of
each market. Flatten assignments into slots `i*K+j`. Market `m` occurs in
slots `(m-1)+q*N`, where `q=0,1,...` is its owner rank. Side is the parity of
`q+m`. Thus every market has equal buy/sell owner counts when its owner count
is even, and a difference of exactly one when odd. A sender's side in a market
stays fixed, preserving the generator's self-trade avoidance.

This is an owner-count guarantee, not equal executed volume: submission,
selection, margin, fills and cancellations can still differ. Zero-owner markets
remain unused; one-owner markets cannot match. Use enough senders for at least
two owners per market. The rule uses the generator's local sender indices;
multiple generator processes have separate ownership plans unless coordinated.

The default `K=0` and explicit full-market plans keep the legacy side rule.
The default random draw schedule and action bytes are unchanged; a differential
test preserves the earlier uniform generator as an oracle. Locality startup
logs identify `per-market-owner-rank-v1`, making the changed workload explicit.
Assignment arithmetic is widened to avoid wraparound at large sender/market
indices; ordinary existing market assignments and RNG draws are unchanged.

New tests enumerate the assigned owners, including both failing configurations,
non-divisor `K`, odd counts and incomplete coverage; check actual generated
orders against their fixed sides; and compare serialized uniform action streams
against the baseline. These tests passed in the verification above.

## Existing stage-5 controls

`bench-throughput consensus` supports:

- `--duration`, `--senders`, `--sender-offset`, `--markets`, and
  `--markets-per-sender` for duration and ownership.
- `--econ --target-margin --econ-mid --band --cross-fraction --cancel-fraction`
  for fixed-side economic flow. Cancellation probability is per action;
  `--batch-size` is orders per placement action.
- `--rate-total` in **actions/s**, or `--rate` in actions/s per sender.
  Zero for both means unpaced submission; offered rate is not matched orders/s.
- `--submit-batch`, `--concurrency`, `--format`, `--sign-mode`, `--rpc-urls`,
  and `--metrics-urls` for submission and observation.

`--pre-sign` is explicitly ignored in economic mode, which always signs a fresh
stream. `--warmup` belongs to the standalone `matching-engine` subcommand, not
live consensus. There is no consensus seed-to-depth phase.
Economic RNG is fixed per local sender/ordinal;
there is no consensus RNG-seed CLI option.

`tools/matched-bench/run-cell.sh` accepts market count, duration and aggregate
rate as arguments, and `SENDERS`, `BATCH`, `SUBMIT`, `CONC`, `MPS` as environment
settings. This candidate adds `BAND=5`, `CROSS_FRACTION=0.5`, and
`CANCEL_FRACTION=0.05` overrides, with the same defaults and benchmark command
when unset. Runner preflight rejects nonfinite/out-of-range fractions and bands
outside `1..29999` (the fixed derived mid is 30000). It still fixes
`target-margin=1500` and starts with `CLEAN=1`; reusing a seeded live chain needs
a separate preparation design. `workload.json` and `cell.workload` in the summary
record effective parameters. Historical cells without this manifest report
unknown workload metadata rather than invented defaults.

## Optional rate schedule, implemented but not benchmarked

`--rate-schedule '0:76000,30:120000,60:0,90:76000'` declares absolute integer
second offsets and aggregate **actions/s** for economic mode only. The runner
passes the flag only when `RATE_SCHEDULE` is set. The schedule overrides scalar
rate flags, starts at zero, ends at `--duration`, permits at most 64 phases and
requires strictly increasing offsets before the duration and finite nonnegative
rates. Runner schedule text must not contain whitespace, preserving unambiguous
command provenance. Scheduled zero pauses new dispatch; unscheduled zero retains its legacy
unbounded meaning. Non-economic schedule use is rejected.

Each sender retains its nonce/RNG stream through phases. The scheduled generator
does not consume the legacy random initial-jitter draw, so scheduled versus
scalar runs do not have byte-identical action streams. Scheduled A/B comparisons
must use the same declared schedule; the unscheduled default remains unchanged.
Dispatch pacing uses a
shared monotonic start, spreads the first round by sender index, skips elapsed
phases and advances from actual dispatch rather than accumulating catch-up debt.
Signing and HTTP dispatch share the existing concurrency semaphore. A delayed
permit or signature cannot dispatch under an expired phase: an unsigned batch
is retained and re-signed with fresh nonces at its next eligible slot. A second
check at the first poll of the HTTP task prevents a task queued before the
boundary from beginning an expired request. Such late queued actions are skipped
and counted in `RATE_SCHEDULE_LATE_ACTIONS_SKIPPED`; nonce gaps are allowed. Work
already submitted before a pause may still arrive or execute during it. Timers
are not hard real-time, and actual offered rate can be below the requested rate
when signing, scheduling or the chain is saturated.

`RATE_SCHEDULE_PHASE` JSON log records contain nominal start/end, requested rate,
planned Unix time and observed elapsed time. The summary copies observed records
to `cell.rate_schedule_observed` and reports malformed/truncated records in
`cell.rate_schedule_parse_errors`. `cell.rate_schedule_provenance` checks manifest,
command and observed phase count/order/values, finite clock values and observation
within the intended interval. Missing or mismatched scheduled evidence prevents
benchmark acceptance; unscheduled acceptance remains unchanged.
These records establish the declared timeline;
they do not prove that every requested phase rate was achieved. The whole-cell
node-counter scorer and strict drain/agreement gates remain in place. Automated
phase-specific throughput/recovery scoring is still needed.

New pure planner tests cover invalid offsets/rates, duration zero, the phase cap,
slow-rate boundary crossing, pauses, skipping elapsed phases, no catch-up debt,
batch units, sender spacing and extreme finite rates. A bounded async test checks
that a delayed HTTP task skips its request and releases its semaphore permit.
Python fixtures cover
runner validation and summary provenance including re-summarization. These tests passed; live schedule validation remains pending.

## Parameterization plan and useful cells

The economic runner controls above are implemented in this candidate. Next add a
separate, declared warmup/seed phase before measurement:
submit passive flow (`cross-fraction=0`, initially no cancels), require observed
book depth and completed execution, and record the preparation counters/window.
Do not infer depth from submitted orders; rejected orders and fills matter.
Use equivalent fresh seeds for control and candidate, or an explicitly validated
snapshot/replay process. Keep preparation outside the measured interval.

For cancellation compaction, start with the unchanged 10-market cap200 control,
then a declared `band=1` deeper-book workload with lower crossing probability
(e.g. 0.2) and the same cancellation probability (0.05). Uniform ownership avoids
confounding the first A/B with the locality fix. Measure the actual number of
cancellations satisfying the candidate's gate (at least 32 targets, all on one
level, depth at least 1024), queue position/depth, phase1 time and whole-chain
matched rate. Aggressive remainders can rest at another price even with band1,
so eligibility must be observed. A passive-seed/cancel-only cell can isolate
the mechanism, but cannot establish matched-throughput improvement. Follow any
win with unchanged band5 controls and 300s/900s duration comparisons.

For locality after this fix, run uniform and `MPS=3` at 300 markets as separate
declared shapes. Inspect actual opposite-side participation and matches in all
active markets, rather than just aggregate matched rate. Do not compare new
balanced locality results with old one-sided locality as a chain-code speedup.

For bursts, validate the new in-process schedule with sustainable load, higher
offered load, then sustainable load/recovery. Launching independent generators
with disjoint sender offsets alters sender population and locality plans; the
new in-process schedule keeps the same senders. Preserve node-counter throughput, pending-work
and commit/execution progress, rejection counts, drain, dissemination and final
agreement for the whole cell; report phase-specific rates and recovery time too.
