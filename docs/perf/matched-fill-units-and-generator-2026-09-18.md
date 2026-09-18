# Matched-fill units and load-generator diagnostics

Read-only source/log review at `f4c2d12`; Torus discovery
`1588ac36-b279-4e6b-8852-4a1bfcfa348d`. No new benchmark, build, or test was run
for this analysis. Rates below describe the existing counter, not a new metric.

## What the headline counts

`torus_orders_matched_total` counts **fill events per validator**. Its registered
help text is “Total number of order fills” (`torus-telemetry/src/lib.rs:754`).
The executor increments it by `result.fills.len()` in all three placement paths
(`torus-bridge/src/native_executor.rs:3752`, `:4076`, `:4578`).
Each `Fill` contains one maker order and one taker order, quantity and price
(`torus-core/src/order_book.rs:47`, `:1090`). Applying it to both counterparties
does not increment the metric twice. An order can participate in multiple fills.
Consequently this is neither unique matched orders nor maker-plus-taker order
legs. Do not sum validators' counters: each executes the same committed work.

Existing approximately 45–55k campaign results remain comparable as **matched
fills/s**, since their counter definition is unchanged. Comparing this with any
external “orders/s” target requires that target's measured unit, workload, and
measurement boundary to be established separately. No cross-chain equivalence
is established here.

## Requested, attempted, acknowledged, and executed load

`--rate-total 76000` is a requested pacing budget in **native actions/s**.
A placement action contains 400 orders in the current workload; a cancellation
action contains no placement orders. Cancellation probability is 5% per action.

The generator's `submitted` counter increments only for RPC result entries
carrying an accepted hash. It does not count attempted requests, attempted
placement orders, or an action admitted when its response cannot be decoded.
Only the first five errors are printed (`bench-throughput/src/main.rs:1467`).
Partial batch successes also need separate rejected-item accounting. Thus an ACK
rate alone cannot establish actual offered load or an admission ceiling.

Small retained-log samples:

| Cell | Final acknowledged actions | Generator-reported ACK rate |
| --- | ---: | ---: |
| `s60-recovery-cap200-r1` | 55,592 | 443 actions/s |
| `s60-hashes-cap200-r1` | 57,821 | 460 actions/s |

The generator reporting interval includes its final wait and differs from the
runner's load interval. The recovery log prints five response-body parse errors;
their cause and total count are not established. The recovery node reports the
same 55,592 submit-pipeline calls, but this alone does not identify where failed
HTTP requests stopped.

The arithmetic `460 * 0.95 * 400 = 174,800` estimates placement orders/s only if
that ACK population actually has the configured action mix. It is **not** a
measurement of attempted or admitted placement orders. If a sustained fresh-book
workload truly admitted fewer than 200k placement orders/s, it could not sustain
200k fill events/s indefinitely: every fill exhausts at least one participating
order. Existing resting inventory and short measurement windows complicate that
bound. The observed ACK rate does not prove this condition or a fixed ceiling.

Current results also demonstrate substantial processing backlog: recovery's
summary records 32,974 executed actions, 22,817 nonce-expired evictions on val0,
peak pending actions 17,157, and peak execution queue 56. Its headline is
49,242.8 fills/s over the runner's 127s load window. This is evidence against
explaining the current result solely as lack of queued input. Drain can finish
after expiry; it does not mean every acknowledged action executed.

## Candidate bottlenecks and controlled knobs

The economic generator creates actions inline and signs/encodes each submit
batch through `spawn_blocking`, then takes its HTTP semaphore. Default Tokio
uses available parallelism for runtime workers and permits up to 512 blocking
threads; those are separate pools. Signing still hashes all order fields before
one ECDSA signature per action. `--pre-sign` is ignored in economic mode.
The client timeout is 10s and its idle connection pool limit is 256.

Current `CONC=256` exceeds the node's default RPC connection limit of 64.
`TORUS_RPC_MAX_CONNS` is absent from the recorded node environment. This is a
configuration mismatch worth testing, not a proven explanation of parse errors.
The node separately permits 64 concurrent submit pipelines, with a 250ms permit
timeout and an ingress verification pool sized to half available CPUs.
Recovery val0's post-run sums over 55,592 submit calls give approximately 4us
permit wait, 23.9ms verification wall time, and 2.49ms admission wall time per
call. These aggregate observations do not establish client-side waiting, and
the metric named verify CPU also uses `Instant` wall time in source.

Useful separate experiments, holding node code, economic shape, senders and
nominal window fixed:

1. **RPC connection headroom:** existing runner `EXTRA_ENV` can set
   `TORUS_RPC_MAX_CONNS=256` or `512`; keep `CONC=256`, `SUBMIT=1`, `BATCH=400`.
   Compare parse/admission failures, acknowledged and executed actions, fill
   rate, queues, expiry and all acceptance gates. This needs no code change.
2. **Client concurrency:** compare `CONC=32/64/128/256` under a declared fixed
   server connection limit. More concurrency can increase contention; no gain
   is assumed.
3. **RPC batching:** compare `SUBMIT=1/4/8`, keeping `BATCH=400`. This preserves
   action shape and per-sender RNG action order while changing grouping and
   timing. It amortizes HTTP and permit overhead; the generator clamps submit
   batches to 1..100. Watch same-sender queueing and request bytes.
4. **Generator worker budget:** scope `TOKIO_WORKER_THREADS` to the generator
   process only, for example 2/4/8/default. Setting it globally through the runner
   can also change node runtimes and confound the experiment. There is currently
   no generator CLI for the blocking-pool limit; runtime worker count does not
   bound signing threads.

Increasing `BATCH` changes cancellation frequency per placement order, margin
bursts, action sizes and block-cap packing. The runner also derives node order
and byte-cap settings from `BATCH`; this is a distinct workload/configuration,
not a transparent generator-only acceleration. Session signing changes ingress
authorization work and requires registrations, so it is another declared shape.
Multiple RPC endpoints or generator processes change ingress distribution and
possibly ownership/sender populations. Keep these separate from default-shape
comparisons.

Before attributing any ceiling to the generator, instrument attempted actions
and placement orders, acknowledged action types, per-item and transport failures,
and generation/signing/semaphore/HTTP durations. This diagnostic instrumentation
has not been implemented here. New scheduled-load pacing also acquires the
semaphore before signing; compare scheduled cells with scheduled controls.
