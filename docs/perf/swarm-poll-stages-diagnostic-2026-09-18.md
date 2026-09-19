# Actual swarm polls and local receiver stages

Base: body-send diagnostic 14449bc. Exact opt-in `TORUS_SWARM_POLL_TRACE=1`;
all other values and absence are OFF. This is instrumentation, not a fairness
policy, scheduler fix, or throughput improvement. Source only; root schedules
qualification. Follow-up to MAIN findings 2b3fdaef and 927d24a6.

The retained view702 expiry excerpt has early local request initiation but late
remote request admission, and an earlier admitted request at another peer with
later serving. Duplicate hash/view/peer identities do not establish unique
request/response matches. Existing admission is after decode and validation tee.

`swarm_poll_diag` schema1 wraps the original `select_next_some` future. Every
wrapper poll calls that future exactly once with the same Context/waker, records
Pending or Ready, and returns it unchanged. The original biased select order is
unchanged. Tracker state outlives cancelled/recreated select futures. No timer,
wakeup, extra poll, queue, or wire field is added. Command selections distinguish
real commands from channel closure; a ready higher-priority command may prevent
any poll of the swarm branch. Counters are per emission window, not cumulative.

Stage and poll timestamps use one process-local monotonic origin, separate
from the existing body-fetch clock. Never subtract clocks across processes or
between diagnostic families without an anchor. PID scopes the clock. Each poll
aggregate includes `clock=swarm_poll_trace/process_monotonic_us` and a
`clock_anchor`: local-before, one existing BodyFetchTraceStamp (PID/sequence,
body-fetch monotonic and Unix clocks), local-after. Both timestamp reads inside
that existing capture lie within the local bracket. For same-process monotonic
alignment, the offset is bounded by body-fetch mono minus local-after/before
(with microsecond quantization), not a single exact offset. Unix alignment is an
observation bracket only: wall time may jump. Reject missing/mismatched PID or
inconsistent brackets; never extrapolate them into wire latency. Anchoring occurs
only at aggregate emission, with no wall clock or shared stamp atomic per poll.
Receiver records declare the same local clock and can use those anchors. Poll
durations cover the actual future poll call (including any descheduling inside it). Interpoll gaps
are previous poll RETURN to next poll ENTRY, with exact endpoints. The largest
completed gap belongs to its ending window and can start in a previous window.
`open_interpoll_us` is elapsed since last return at emission, not a completed gap.
No prior poll means null endpoints. These measurements cannot distinguish an
unwoken task, scheduler descheduling, command priority, or other handler work by
themselves, and Pending does not establish lack of bytes in the OS.

Emission is checked between select iterations, at most once per elapsed second.
An idle/suspended loop emits nothing until it next progresses: windows can exceed
one second, and the final interval/tail can be censored on exit/crash. Logging and
clock-reading overhead are included in ON execution. Do not infer zero gaps from
missing records or promise zero overhead. No per-poll log is produced.

`swarm_receive_diag` schema1 records direct-request handler entry (after outer
codec/dispatch), inner Borsh decode, validation tee, queue-lock attempt/acquisition, and admission with an explicit
local instance ID (PID+ID); repeated identical messages receive distinct IDs.
Stages absent from an error/filtered path remain null. Outcome distinguishes
admitted, queue_full, decode_error, banned and sender_rejected. Non-body decoded
messages and other valid direct envelopes are filtered out. Body identity is
attached only after successful decode, sender only after existing verification.
When existing body-fetch tracing is ON, `admission_seq` links to its exact local
admission record; it is null otherwise. No unique wire identity is inferred.
Receiver records emit after the direct handler returns, outside queue locks.

ProposalHeader, BlockDataRequest and BlockDataResponse use the existing metadata
classification. Receiver output is capped at 32 records per anchored process-local
one-second window, not every sliding one-second interval.
Suppressed eligible records are counted and reported on the next emitted record;
that count is not a partition by outcome. A suppressed final tail has no receipt.
This is sampled evidence, not a complete-message census. The cap includes errors;
unknown/malformed messages never acquire guessed body identities. There are no
outer-codec stamps: event entry follows libp2p codec/dispatch work. This diagnostic
therefore partitions event-to-admission, not the entire send-to-admission residual,
and measures no transport or wire latency.

Authored tests (not run by agent): exact OFF parsing/clock bypass; Pending, Ready,
repeat polls, cancellations/recreation, command counters and cross-window gap;
receiver IDs/budget and lock-ordered budget clock; bounded existing-clock anchor;
actual biased-command selection leaving the swarm future unpolled; actual inner-decode-to-admission seams preserving queue bytes,
filtered/error/full outcomes and existing admission-sequence linkage. Root should
run focused `cargo test -p torus-network swarm_poll_trace` and
`cargo test -p torus-network swarm_receive_trace`, then full networking coverage
and a separate node build. No performance acceptance follows from unit tests.

Qualification on 2026-09-19: receipt `cb235577-ffad-40f0-8214-caadaec6e882`
passed all 127 networking library tests (including the nine new fixtures), all
20 external parser fixtures with hashes checked before/after, and a separate
release node build. Source fingerprint was unchanged. Live use follows; these
checks establish neither a scheduling fix nor performance acceptance.
