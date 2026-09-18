# Bounded ordinary receive before body expiry

Source candidate on `perf/body-before-expiry`, based on diagnostic commit
`bb3409b`. Default **OFF**. Root qualification passed; live effect remains
unmeasured and the experiment is not promoted.

The retained 10-market diagnostic contains one clear local ordering witness:
val0 admitted the first retained `GEu17YN` response at 12:10:06.685881,
expired its header fetch at 06.694304, and entered the response handler at
06.694424 with `tracked_header=false`. Its local monotonic admission/handler
stamps differ by 8,544 microseconds. Later duplicate admissions make this a
specific retained event sequence, not an automatically unique latency cohort.
Val1's `CaEbsdC` response was first admitted about 80.7 ms **after** its expiry;
this change does not establish or fix that earlier delay.

Separate scoped issue `09c15cac-07f4-4667-95ed-4b7708ba5fdd` records the receiver
gap: legacy `ProgressMessageStub::recv` checks its current-view buffer but never
polls the ordinary channel after the view deadline. It links to the original
expiry observations in `eabb3ae6-f11a-4a47-9f39-a844feb1a574` without reusing that
cross-worktree issue for this attempt.

## Treatment

Set `TORUS_BODY_BEFORE_EXPIRY=1` on each validator for a declared treatment.
Only that exact value enables it; unset, `0`, and other values preserve the
legacy receive and maintenance order. The flag is read once at algorithm
startup and the enabled policy emits one startup log line.

At the ordinary receive/retry step, capture whether the flag is enabled **and**
a body or justify fetch is already pending. Only then:

1. Use the existing current-view buffer first.
2. Inspect at most **eight raw ordinary-channel envelopes total**, counting
   wrong-chain, stale, and future-only messages against the same budget.
   Try the ready channel even if the original view deadline has elapsed. If
   the channel is empty, wait only for the original remaining deadline
   (still capped at 10 ms while a fetch is pending). There is no unbounded
   filtering fallback and no extra wait after the deadline.
3. Dispatch at most one accepted ordinary message through the existing
   HotStuff, pacemaker, or sync-advertisement handler.
4. Run the existing body retries, justify retries, missing-PC check, and
   retry-triggered sync handling once, in their existing relative order.

With no pending fetch at entry, retain the legacy order even when the flag is
enabled. In particular, missing-PC recovery still arms before an idle receive
wait; a freshly armed fetch cannot expire in that same maintenance call.
Pacemaker ticking, view entry/proposal work, sync processing, the existing
dedicated body channel, and app reconciliation retain their positions.

Both receiver policies call the same chain/genesis/view filter. Future-view
recovery messages remain cached **and** returned immediately, with later
buffered redelivery. Old ordinary consensus messages remain filtered, while
recovery messages retain their existing viewless eligibility. This changes no
wire messages, queue routing/capacity, validation, vote gates, retry counts,
retry intervals, or durable state rules. Handler-generated and retry-generated
sync requests are still consumed through the existing sync trigger.

The bound is eight inspected channel envelopes and one dispatch, **not** a
wall-time bound on application validation, sync work, buffer cleanup, or a
handler. A ready body behind more than eight rejected messages, a current-view
buffer entry, or another accepted message can still lose its fetch to expiry.
The experiment deliberately does not drain indefinitely or prioritize bodies
over all other ordinary consensus traffic. An admitted ordinary network message
may also still be waiting upstream of the progress-message channel.

## Qualification and interpretation

Authored coverage exercises the production algorithm receive/retry method with
a real authenticated pending header, expired retry budget, queued body, normal
application validation, and a signed sync-server advertisement. It compares
OFF/ON and expired/future view deadlines, preserves application rejection,
checks malformed/empty fallback, filtered-flood bounds, one ordinary dispatch,
handler plus retry sync consumption, and idle missing-PC arming. Receiver
fixtures cover genesis negatives, future buffering and redelivery, expired
deadlines, raw-envelope accounting, and disconnection. Three initially-empty
receiver fixtures use a test-only rendezvous immediately before `recv_timeout`
to cover later arrival, connected timeout, and later disconnect without sleeps
or tight elapsed-time assertions; production code and layout omit that seam. The existing future
header voting regression runs through both receive policies.

Independent review found no blocker in runtime or the added waiting fixtures.
Root ran the focused 10 algorithm and seven receiver tests, then all 112
HotStuff and 111 network library tests, and a separate node-only release build.
All passed receipt `3bcb0276-2f16-4971-96ce-a4e78ea157b2`. Six affected or
alternate-worktree packages were cleaned first to prevent stale shared-target
artifacts. The node contains the new policy startup signature and existing
queue-timing signature, and excludes the isolated cancellation helper.

Executed HotStuff commands:

```sh
cargo test -p hotstuff_rs --release --lib body_before_expiry_tests
cargo test -p hotstuff_rs --release --lib before_retry_receive_tests
cargo test -p hotstuff_rs --release --lib genesis_body_filter_tests
cargo test -p hotstuff_rs --release --lib sync_recovery_tests
cargo test -p hotstuff_rs --release --lib
```

The separately built node is frozen before any other build changes the target.
For an eventual matched diagnostic pair, record the effective flag in the
runner's `EXTRA_ENV` provenance, hold other flags/workload/artifacts fixed, and
compare expiry/fallback counts, pre-expiry admissions, tracking at handler
entry, drain/agreement, and throughput. Existing strict acceptance remains in
force. Reduced local expiries would not prove a transport fix, a throughput
gain, or achievement of the 200k target.
