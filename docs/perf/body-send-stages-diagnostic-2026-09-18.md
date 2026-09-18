# Ordinary body send stages (qualified diagnostic)

This default-off diagnostic measures local outbound stages for ordinary
`BlockDataRequest` and `BlockDataResponse` messages. It does not change message
routes, wire bytes, queues, reconnect retries, validation, timers, or the biased
swarm selection order. It does not establish a throughput improvement or the cause
of a late response. Root verification passed 230 unique network/HotStuff tests
and a separate node build (receipt `249cf648-6a81-48cd-ad04-32fe26849ffb`);
independent review found no blocker. Live measurement remains pending.

## Motivation and scope

Measurement-gap issue `927d24a6-6487-4448-b486-2e5e128662b9` links the original
diagnostic finding `eabb3ae6-f11a-4a47-9f39-a844feb1a574`. In the retained
`s60-body-before-off-10m-r1.expiry-traces.txt`, val1's `oNntict…` view308 expiry
was 13:30:25.114178Z; its first response admission was 25.557292Z, 443.114 ms
later. Admission to handler took 4.453 ms locally. Val2 had completed a successful
same-hash lookup for val1 at 23.899721Z, but repeated requests and responses prevent
identifying which serve produced the first admitted response. Public startup keys
verify val0=`zswVB9…`, val1=`a3nFf…`, val2=`2tvRh…`.

Both ordinary requests and responses use `bridge::Network::send` →
`NetworkCommand::Send` → `send_direct` → `/torus/direct` `DirectRequest`.
`DirectResponse` is the ACK. The dedicated block-data codec is not this path.
The serve trace ends before response construction and the bridge call, so any
unpaired gap before enqueue remains unresolved.

## Enablement and records

Only exact `TORUS_BODY_SEND_TRACE=1` enables the diagnostic (cached per process).
Unset, `0`, and every other value leave it OFF. It is independent of
`TORUS_BODY_FETCH_TRACE`; enable both for outbound and inbound evidence.
No new benchmark harness or default environment setting is included.

OFF retains the original bridge command variant. ON uses a boxed internal command
context only for body requests/responses; no trace fields enter a wire message.
Non-body messages retain the ordinary variant. No diagnostic clock, box, JSON,
payload clone or extra serialization is added to the OFF path; optional trace
branches in shared sending code remain. Instrumentation has overhead when enabled.

The separate `body_fetch_send_diag` prefix is followed by JSON with `schema:1`.
It does not alter the existing `body_fetch_diag` schema. Every stamp contains
`pid`, shared process-local `seq`, `mono_us` and `unix_us`. Every record contains
`pid`, `id` (the pre-enqueue sequence), and `message` with kind, full public target
key, full hash and view. There are no body payloads or private keys.

* `event:enqueue`: `pre_enqueue`, `post_enqueue`, `accepted`. Post is captured
  immediately after the channel send returns and before formatting the record.
  Failed sends retain the existing drop behavior and report `accepted:false`.
* `event:send`: `pre_enqueue`, `dequeue`, `queue_remaining`, `encode_begin/end`,
  `send_begin/end`, `tracking_begin/end`, `finished`, `payload_bytes`,
  `request_id` and `outcome`. Dequeue is stamped inside the selected command arm,
  before dispatch; remaining queue length is a concurrent observation.
  Encoding wraps the existing inner Message Borsh serialization. Send wraps the
  existing libp2p `send_request`. Tracking wraps the existing outbound map insert.
  The record is formatted only after the send path finishes and locks are released.

Outcomes are `self_delivered`, `buffered_unmapped`, `buffered_disconnected`,
`encode_failed`, or `initiated`. Stages that did not run are JSON null, never
zero-duration substitutes. Self-delivery means the existing local enqueue path
was called; that path's capacity drop remains possible and is not overridden.

## Correlation and interpretation limits

Join producer/consumer records by verified process startup scope, PID and ID.
PID alone can be reused across restarts. Duplicate hash/view/target commands have
different IDs. A consumer record may be emitted before its producer record;
file order and adjacent lines do not identify pairs. Sequence numbers are unique
in the process but do not promise cross-thread clock ordering. Use local monotonic
stamps for durations, and reject contradictory/missing evidence. Wall clocks may
jump and do not establish cross-process latency.

`dequeue - pre_enqueue` includes producer preparation/boxing, channel handoff,
possible producer descheduling, and consumer scheduling/arm work after `recv`
has already removed the command but before the dequeue stamp. Actual queue
residence is conservatively bounded by `[0, dequeue-pre_enqueue]`. When all three
stamps are valid, `[max(0,dequeue-post_enqueue), dequeue-pre_enqueue]` bounds
enqueue-to-observed-dequeue-stamp time, which includes that post-removal work;
it is not a lower bound on queue residence. A dequeue stamp before post_enqueue
is legitimate. `queue_remaining` is neither the queue's maximum nor its length
at enqueue time.

`send_end` means request initiation returned, not bytes delivered or an ACK.
Outer Borsh encoding, compression, stream negotiation/writing and later swarm
processing remain downstream. The local `request_id` identifies this initiation;
it is not a wire correlation ID available to the receiver. Missing consumer records
after a successful enqueue are incomplete/censored, not proof of a dropped message.
Missing producer records likewise prevent a complete handoff assessment.

Trace context intentionally ends at buffering or initial initiation. Pending queue
entries and reconnect retries keep their existing message-only storage; they do
not retain these IDs. An initial `buffered_*` record does not prove that message was
never subsequently sent. ACK and failure events are not newly traced. Matching
repeated serves, retries and admissions by hash/view remains ambiguous.

Outbound commands precede swarm events in the existing biased select. Prompt
initiation plus late admission would leave subsequent swarm polling, codec work,
transport and receiver pre-admission scheduling unresolved. This candidate adds
no fairness counters. A later diagnostic must wrap actual future polling to count
Pending and Ready attempts; completed `SwarmEvent`s can only be labeled
`event_ready`, never “every poll.” No fairness or scheduler behavior changes are
authorized by this diagnostic.

## Verification

Seven focused fixtures cover exact gating/non-body parity, OFF/ON request and
response inner and outer byte equality, coordinated queueing with duplicate IDs,
enqueue failure, null absent stages, actual unmapped/disconnected buffering and
self-delivery bytes, and the actual connected encoding/initiation/tracking branch
using an unpolled dummy transport. The latter proves initiation and tracking only,
not live connectivity. Fixtures use explicit enablement rather than global env
mutation; channel coordination uses no sleeps or tight elapsed-time assertions.
The encode-error branch is instrumented but not fault-injected by these fixtures.

Root passed these checks outside any performance cell:

```sh
cargo test --release -p torus-network --lib body_send_trace
cargo test --release -p torus-network --lib
cargo test --release -p hotstuff_rs --lib
cargo build --release -p torus-node
```

The node was built separately after cleaning six alternate-worktree packages.
Its outbound trace markers and expected body-receive-policy base signature were
positively identified; unrelated margin/pass-B candidate signatures were absent.
It is frozen as `artifacts/body-send-stages`. The seven focused cases are included
in the 118 network tests; the full HotStuff library contributed another112 tests.
Qualification does not establish a live delay fix or throughput gain.
