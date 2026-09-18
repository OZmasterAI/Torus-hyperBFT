# Body-fetch admission and handler timing diagnostic

This isolated diagnostic uses `TORUS_BODY_FETCH_TRACE=1` (read once, restart to
change). It does not change message formats, routing, admission rules, retries,
queue capacities, or consensus behavior. Default off adds a cached flag check
at network enqueue; it does not format IDs, sample clocks, allocate diagnostic
records, or log. The existing disabled HotStuff trace path remains disabled.
This is diagnostic instrumentation, not a performance optimization.

## Observation motivating the change

In `s60-sustained-base-10m-r1`, val2 exhausted nine retries for the body whose
base64 hash starts `iwk8LDI` at UTC 11:24:02.414950, while still in the header's
view 1030. Val1 handled that body at 11:24:02.545592 with retry count 9. Val2's
first logged handler invocation was 11:24:04.572053, shortly after synchronous
block construction. Successful serves were suppressed by the previous trace.
The retained 139-line peer window does not establish when the response arrived
at val2's network queue. It establishes late handler processing, not a queue
cause, stale-view cause, remote serving delay, or an appropriate retry budget.

## Three trace points

All new clocked records start `body_fetch_diag` and contain `pid`, `seq`,
`mono_us` and `unix_us`. Full 32-byte hashes and peer public keys use unpadded
base64 (43 characters), with no body bytes. Heights are outer HotStuff heights.

1. `admission`: ordinary network inbound admission of a proposal `header`, body
   `request`, or body `response`. Includes message `view`, `hash`, `peer`,
   `admitted`, `queue_depth_before`, and `queue_depth_after`. The timestamp is
   sampled while holding the queue mutex immediately before push/drop; all
   formatting and logging happen after unlock. This is after decoding and the
   existing speculative-validation tee, not wire arrival. Local loopback uses
   this same queue and records its local sender key.
2. `serve`: every successfully completed block lookup, including hits and
   misses. Main fields stamp handler entry; `lookup_done_seq`,
   `lookup_done_mono_us`, and `lookup_done_unix_us` stamp lookup completion.
   Includes requester key, request/local view, full hash, found flag and optional
   height. A storage error still returns through the existing error path without
   this completed-lookup record. A found record is before enqueueing the reply,
   not proof of its transmission or delivery.
3. `receive`: every response handler invocation, including retry zero and
   unsolicited/duplicate responses. Stamp is sampled at handler entry. Includes
   sender key, response/local view, full hash/parent, height, tracker/deferred
   flags and optional retry count. It precedes validation and is not proof of
   insertion, safety, or acceptance.

## Interpreting the clocks and evidence

`mono_us` uses a single `Instant` origin shared by network and HotStuff within
the node process. Never subtract it across processes or restarts. `seq` is
process-local and unique, but allocation and timestamp sampling are not one
atomic operation: sequence order is not a cross-thread time ordering guarantee.
`unix_us` is signed Unix wall time and can jump; it is only a wall-clock
correlation aid, not evidence of synchronized remote clocks. Retain node/run
identity and restart boundaries alongside PID; PID alone can be reused.

For a uniquely matched same-process response, admission-to-handler monotonic
time measures the combined ordinary network queue, poller handoff/stub queue,
and time waiting for the synchronous algorithm to handle it. It does not split
those components. Request admission-to-serve entry measures the same combined
local delay on the serving node; lookup completion minus entry measures lookup
and pending-body cloning work. Remote intervals still include uninstrumented
send/codec/transport work and cannot be attributed solely to network latency.

There is no wire request identifier. Retries/duplicates with identical
peer/hash/view cannot always be paired uniquely; report ambiguity or bounds
instead of inventing a one-to-one latency. The ordinary receiver also buffers
future-view body messages while returning them immediately, so a single
admission can produce another handler invocation when that view's buffer is
replayed, without any wire duplicate. Do not independently pair each handler
with the earliest matching admission. The dedicated block-data protocol
does not use this ordinary admission queue: a handler without an admission line
is not proof of a lost log. Normal node body requests currently use the ordinary
consensus transport. The dedicated path bypasses the ordinary stub's chain,
genesis-exception and view filtering, so this diagnostic must not be used to
justify rerouting without a separate validation review.

The new records use only fixed-size transient metadata and no retained maps or
extra queues. A successful ordinary fetch produces four records across its two
nodes (request admission, serve, response admission, receive), plus each header
admission. A failed lookup retry produces two. Healthy three-validator proposals
with two follower fetches are roughly eleven fleet-wide records when each node
receives one header; duplicates/retries increase this. Logging can affect timing;
use a declared diagnostic cell, not an unmatched performance comparison.

## Verification status

Independent source review found no blocker. Root ran checks between live cells:
three clock/identity and two real enqueue/drop fixtures passed, followed by all
95 HotStuff and 111 network library tests and a separate node-only release build.
Receipt: `5053b605-8b19-4dff-8e40-8161e6d78ec3`. Affected generated packages were
cleaned first because the shared target has previously retained stale artifacts.
Fixtures cover full-ID collisions/round trips, clock schema and sequence
uniqueness, all three message classifications, unchanged queued bytes, and
admission/drop emission after mutex release without eviction. Commands:

```sh
cargo test --release -p hotstuff_rs --lib body_fetch_trace_tests
cargo test --release -p torus-network --lib body_fetch_trace
cargo test --release -p hotstuff_rs --lib
cargo test --release -p torus-network --lib
cargo build --release -p torus-node
```

The node build must be separate from any generator build and its frozen binary
identity checked before a diagnostic run. Passing unit tests cannot establish
the cause of the retained live failure.
