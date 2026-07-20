# L3 proposal_arrival attribution — delivery path (perf/re-proof5)

Scope: delivery latency BEFORE a proposal reaches consensus. Analysis only.
Sibling doc (Fable): block insert/persist path — NOT covered here.

## What `proposal_arrival` actually measures

`torus_view_proposal_arrival_seconds` = `secs_between(follower.start_view, first ReceiveProposal event)`
(view_metrics.rs:126 `receive_proposal`, armed by `start_view` at view_metrics.rs:69).
Idle RIv value: **16.5 ms/view** (l3verify-18c-06de732.md).

Crucially, the follower's `start_view` clock and the leader's `start_view` clock are
**co-started** (both enter view v from the same QC/TC). The follower cannot receive the
proposal until the leader has *built and dispatched* it. So `proposal_arrival` structurally
**includes the leader's entire propose_delay** — it is NOT a pure network-stack figure.
The mission premise ("16.5 ms is stack overhead, should be sub-ms") is largely incorrect.

## End-to-end path (file:line map)

Leader side (all synchronous, event-driven — no timers):
- `enter_view` builds + broadcasts the proposal inline: `app.produce_block` then
  `broadcast::<HotStuffMessage>` — algorithm.rs:174 (enter_view), :556/:567 (produce), :608-617.
  There is **no wait-for-tx / min-view / propose-delay timer**: the proposal hits the wire at
  `start_view + produce_block + block-tree write`.
- `SenderHandle::broadcast` → `LibP2PNetwork::broadcast` → `command_tx.send(Broadcast)`
  (sending.rs:27; bridge.rs:594) — mpsc **unbounded**, event-driven, ~µs.
- Swarm `tokio::select!` wakes on `command_rx.recv()` (swarm.rs:979) → `handle_command`
  Broadcast (swarm.rs:2151): self-deliver + (fan? `send_direct`) + (mirror? gossip publish).
- Default is **fan OFF / gossip mirror ON** (config.rs:50, :98) → proposal goes via gossipsub
  `publish` (swarm.rs:2184). gossipsub flood-publishes to established mesh peers **immediately**;
  the 100 ms `DEFAULT_GOSSIPSUB_HEARTBEAT_MS` (behaviour.rs:54) governs mesh maintenance /
  IHAVE-IWANT only, NOT direct forward — so no heartbeat delay at idle.
  (With `DIRECT_FAN=1`, `send_direct` → `request_response.send_request`, swarm.rs:2723 — equally
  immediate, no batching. PushScheduler/`NATIVE_BATCH_INTERVAL_MS`/`D2L_BATCH_MS` are the
  native-tx pushers and are NOT on the proposal path — verified.)

Follower side:
- QUIC (quic-v1/quinn) delivers → swarm event → `enqueue_inbound` into `shared.inbound`
  (Mutex<VecDeque>, swarm.rs:2842) — µs.
- Poller thread `network.recv()` = `inbound.pop_front()` (bridge.rs:605); **if empty, parks
  `sleep(250 µs)`** (receiving.rs:72, the S370 anti-core-spin park). **This is the ONLY fixed
  tick on the entire path** → 0–250 µs, avg ~125 µs.
- Poller → `to_progress` mpsc → `ProgressMessageStub::recv` `recv_timeout` (receiving.rs:156):
  **event-driven wake**, no poll interval. The 10 ms fallback in algorithm.rs:242 only *shortens*
  the park during pending sync/body work — it never *adds* latency (recv_timeout returns the
  instant a message lands). View-aware buffering (receiving.rs:171-200) does not inflate the
  metric: a future-view proposal is returned from buffer on the next recv after `start_view`.
- `on_receive_msg` (algorithm.rs:253) → ReceiveProposal event → `receive_proposal` observed.

## Composition estimate of the 16.5 ms

| Term | est. | in delivery scope? |
|---|--:|---|
| Leader propose_delay = propose_build (7.4, start_view→insert) + finalize (~1–3) | **~8–10 ms** | No — exec/insert (Fable) |
| View-entry skew + 3-thread-hop scheduler jitter (swarm→poller→algo) | **~4–6 ms** | Partly (hop count) |
| Genuine delivery stack: cmd chan + QUIC loopback + enqueue + 250 µs park + recv wake | **~0.5–2 ms** | **Yes** |

The delivery stack I own is **sub-2 ms on loopback**. ~60% of the 16.5 ms is leader block-build
(Fable's insert/persist scope); the rest is view-sync skew + thread-hop scheduling.

## Ranked fixes

1. **Instrument the split (do this first, zero risk).** Stamp the leader's wire-dispatch
   `SystemTime` into the proposal (or a paired `propose→send` histogram) so the follower can
   compute *true wire delay* vs. the co-started-clock artifact. Without it every ms below 8–10
   is a guess. File: bridge.rs Broadcast / view_metrics.rs. Expected: attribution, not speedup.
2. **Poller wake: replace the 250 µs park with a condvar** notified by `enqueue_inbound`
   (receiving.rs:72 + swarm.rs:2842/2852). Expected: ~125 µs avg (up to 250 µs tail). Risk: LOW —
   must preserve the S370 guarantee (no `yield_now` spin) and stay bounded; a condvar does both.
   Marginal at idle; irrelevant on WAN.
3. **`TORUS_CONSENSUS_DIRECT_FAN=1` (LOAD only, not idle).** At idle gossip already delivers
   immediately, so this buys ~0 ms in the RIv cell. Under load it stops a proposal queuing FIFO
   behind bulk native batches in gossipsub's per-peer queue (config.rs:19,46). Risk: MEDIUM but
   already mitigated — `dedup_applies` scopes dual-path dedup to one-shot HotStuff messages only
   (swarm.rs:2884, the 4f832e5 fix), so it neither reintroduces the duplicate storm nor suppresses
   pacemaker timer-rebroadcasts. Keep GOSSIP_MIRROR on unless all validators flip together.
4. **Leader finalize / propose-before-full-commit — OUT OF SCOPE (Fable).** The ~8–10 ms floor
   lives in produce_block + block-tree write, not the wire. Hands off the insert/persist path.

## Honest floor & WAN relevance

- **Loopback floor:** `proposal_arrival` cannot fall below the leader's propose_delay (~8–10 ms)
  no matter what the delivery path does, because the clocks are co-started. The *delivery-path*
  floor is sub-millisecond. Delivery tuning cannot move the headline metric meaningfully at idle.
- **WAN (future 3-machine testnet, +10–40 ms physical RTT):** the ~1–2 ms node-local delivery
  stack and the 250 µs park become negligible; physical RTT dominates. Fix #2 stops mattering.
  **Fix #3 (direct fan) is the one that still matters on WAN**: gossip mesh relay can add an
  extra hop (leader→relay→follower = 2×RTT) whereas the direct fan is always 1 hop (1×RTT) — a
  10–40 ms saving per view on a large mesh. Fix #1 (instrumentation) matters everywhere.

**Recommendation:** retarget the 16.5 ms — it is not a delivery-stack defect. Land fix #1 to
prove the split, keep fix #3 for the WAN/load regime, and route the dominant ~8–10 ms to the
insert/persist (Fable) workstream.
