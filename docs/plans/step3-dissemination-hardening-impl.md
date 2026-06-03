# Implementation Plan: Step 3 — Native-Action Dissemination Hardening

## Design Decision: Option D (from `step3-dissemination-hardening.md`)
Keep CompactBlock; harden the **unicast delivery** so validators reliably receive
native actions (for reconstruction) and consensus messages. **Gossip stays OFF**
(disabled deliberately in `66ddef4` — it floods GossipSub and drowns consensus).
This supersedes the gossip-era `mempool-gossip-hash-proposals-impl.md`.

## Failure modes being hardened (verified from source)
1. **Consensus unicast dropped** — `NetworkCommand::Send` drops a Vote/NewView when
   the target isn't in `peer_map` (`swarm.rs:749`, `warn!` + return).
2. **Action push is fire-and-forget** — `BroadcastNativeActions` (`swarm.rs:794-810`)
   iterates *currently-mapped* peers only; a validator absent/reconnecting at push
   time receives nothing → CompactBlock reconstruction misses actions → on 3-val
   **all-3 quorum**, one rejection stalls the view.
3. **[OPEN — measure, don't assume]** memory `8ee99db3` attributes push delay to
   swarm **event-loop congestion** (1.5–2s under load), not peer-map misses. Tasks 2–4
   fix the drop/reconnect holes; **Task 5 measures** whether congestion still
   dominates now that gossip is off, and flags a follow-up if so.
4. **[NEW — live evidence, `testnet/node.log`, Jun 3]** Under load the chain also shows
   a **block-sync / connectivity** failure *alongside* dissemination: `block_sync:
   worker fetch error (timeout/disconnect)` + `dropping proposal header ...
   justify_block_known=false` (missing parent block), views turning over ~500–600 ms
   **without committing**. This is **partly outside D's delivery scope** — Task 1(d)
   triages it to decide whether D is sufficient or needs a companion block-sync fix.

**Keep unchanged:** the 100 ms retry in `validate_block` (`app.rs:941-959`) as the
last-resort safety net (correct for all-3 quorum; do NOT remove — that was the
`8ee99db3` mistake, which assumed a 4-validator set).

## Success Criteria
1. New `PendingSendQueue` unit tests pass: `cargo test -p torus-network -- pending_send`.
2. All existing tests pass: `cargo test -p torus-network -p torus-consensus`.
3. Quiet-host devnet flood (`bench-throughput consensus --senders 100 --duration 30`,
   cap-100): CompactBlock missing-action rejections ≈ 0; chain does not stall.
4. A validator reconnecting mid-run catches up within ~1 block (no sustained rejects).

---

## Tasks

### Task 1: Scope the delivery + block-sync seams (read-only, no code)
**Test first**: n/a — investigation. **Verify** = a findings block appended below as
`## Task 1 findings`.
Confirm in `crates/torus-network/src/swarm.rs`:
- (a) What event fires on a validator **reconnect** — `SwarmEvent::ConnectionEstablished`
  vs `identify::Event::Received` (`swarm.rs:538`) — and whether it re-populates
  `peer_map`. **This is the flush trigger for Tasks 3–4.**
- (b) How the `direct` request_response `OutboundFailure` is handled today (find the
  event arm). Decides whether an explicit send-retry is needed.
- (c) Sanity-check the congestion hypothesis (failure mode #3) against current
  (gossip-off) code paths.
- (d) **Block-sync triage (failure mode #4 — live in `node.log`).** In
  `hotstuff_rs::block_sync::client` and the `BlockDataRequest` path (`swarm.rs:768`,
  `request_block_data`): why does `worker fetch error (timeout/disconnect)` fire — is
  the block fetch fire-and-forget / unretried (cf. mem `a06daf38` `body_fetch_tracker`)?
  And why are proposals dropped with `justify_block_known=false` (missing parent) — a
  sync gap, or a peer-map/connectivity gap? **Deliverable: decide whether this is
  resolved by D's delivery hardening, or needs a companion block-sync fix (separate
  task/step) — so the Task 5 bench isn't surprised by a second root cause.**
**Verify**: `grep -n "ConnectionEstablished\|OutboundFailure\|identify::Event\|justify_block_known\|worker fetch error" crates/torus-network/src/swarm.rs crates/hotstuff_rs/src/block_sync/client.rs` returns the arms; findings + the "D sufficient or not" call written.
**Depends on**: none.

### Task 2: Bounded generic `PendingSendQueue` (pure, fully unit-tested)
**Test first** — new file `crates/torus-network/src/pending_send.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{SigningKey, VerifyingKey};
    fn vk(seed: u8) -> VerifyingKey {
        let mut b = [0u8; 32]; b[0] = seed;
        VerifyingKey::from(&SigningKey::from_bytes(&b))
    }
    #[test]
    fn flush_returns_in_order_then_clears() {
        let mut q = PendingSendQueue::<u32>::new(8);
        let k = vk(1);
        q.enqueue(&k, 1); q.enqueue(&k, 2);
        assert_eq!(q.flush(&k), vec![1, 2]);
        assert!(q.flush(&k).is_empty());
    }
    #[test]
    fn per_key_cap_drops_oldest() {
        let mut q = PendingSendQueue::<u32>::new(2);
        let k = vk(1);
        q.enqueue(&k, 1); q.enqueue(&k, 2); q.enqueue(&k, 3);
        assert_eq!(q.flush(&k), vec![2, 3]); // oldest evicted, bounded
    }
    #[test]
    fn keys_are_independent() {
        let mut q = PendingSendQueue::<u32>::new(8);
        q.enqueue(&vk(1), 10); q.enqueue(&vk(2), 20);
        assert_eq!(q.flush(&vk(1)), vec![10]);
        assert_eq!(q.flush(&vk(2)), vec![20]);
    }
}
```
**Implementation** — `crates/torus-network/src/pending_send.rs`:
```rust
use std::collections::{HashMap, VecDeque};
use ed25519_dalek::VerifyingKey;

/// Bounded per-peer buffer of items awaiting peer-map registration.
/// Generic over payload so it is unit-testable without constructing a live
/// consensus `Message`. Mirrors the drop-on-full policy of `enqueue_inbound`
/// (swarm.rs:814), but drops OLDEST (a stale vote/bundle is the least useful).
pub struct PendingSendQueue<T> {
    map: HashMap<[u8; 32], VecDeque<T>>,
    max_per_key: usize,
}
impl<T> PendingSendQueue<T> {
    pub fn new(max_per_key: usize) -> Self {
        Self { map: HashMap::new(), max_per_key }
    }
    pub fn enqueue(&mut self, vk: &VerifyingKey, item: T) {
        let q = self.map.entry(vk.to_bytes()).or_default();
        if q.len() >= self.max_per_key { q.pop_front(); }
        q.push_back(item);
    }
    /// Remove and return all queued items for `vk`, in enqueue order.
    pub fn flush(&mut self, vk: &VerifyingKey) -> Vec<T> {
        self.map.remove(&vk.to_bytes()).map(Vec::from).unwrap_or_default()
    }
}
```
Add `mod pending_send;` + `pub use pending_send::PendingSendQueue;` to
`crates/torus-network/src/lib.rs`.
**Verify**: `cargo test -p torus-network -- pending_send`
**Depends on**: none.

### Task 3: Enqueue-on-miss / flush-on-register for consensus unicast (fixes drop @749)
**Test first**: extend `PendingSendQueue` tests (Task 2) — already cover the
queue semantics. The `handle_command` wiring has **no Swarm seam** (takes
`&mut Swarm`), so it is integration-verified in Task 5, not unit-tested. Document
this in the task.
**Implementation**:
- `swarm.rs:69` — add to `SharedState`:
  `pub pending_sends: Mutex<PendingSendQueue<hotstuff_rs::networking::messages::Message>>,`
  and initialise in `test_shared()` (`swarm.rs:~920`) with `PendingSendQueue::new(256)`.
- Extract a helper from the current `Send` body so it can be reused on flush:
  ```rust
  fn send_direct(swarm: &mut Swarm<TorusBehaviour>, shared: &SharedState,
                 local_key: &VerifyingKey, target: &VerifyingKey,
                 message: hotstuff_rs::networking::messages::Message) {
      let peer_id = shared.peer_map.read().unwrap().get_peer_id(target).copied();
      match peer_id {
          Some(pid) => {
              if let Ok(payload) = message.try_to_vec() {
                  let req = DirectRequest { sender_key: local_key.to_bytes(), payload };
                  swarm.behaviour_mut().direct.send_request(&pid, req);
              }
          }
          None => shared.pending_sends.lock().unwrap().enqueue(target, message), // was: warn-drop @749
      }
  }
  ```
- `NetworkCommand::Send` arm (`swarm.rs:731-751`) → call `send_direct(...)`.
- `NetworkCommand::RegisterPeer` arm (`swarm.rs:752-755`) → after `insert`, drain:
  ```rust
  let pending = shared.pending_sends.lock().unwrap().flush(&vk);
  for message in pending { send_direct(swarm, shared, local_key, &vk, message); }
  ```
- **Also hook the reconnect trigger from Task 1(a)** (likely the same `RegisterPeer`
  path, or the Identify/ConnectionEstablished arm) so a reconnecting validator drains too.
**Verify**: `cargo test -p torus-network` (compiles + existing pass); flush path
exercised in Task 5 devnet run.
**Depends on**: T1, T2.

### Task 4: Re-push recent native-action bundles on (re)registration (fixes missing-action @794)
**Test first**: unit-test the ring bound — reuse a small `VecDeque` cap test (assert
the ring keeps only the last N bundles).
**Implementation**:
- `swarm.rs:69` — add `pub recent_native_bundles: Mutex<VecDeque<Vec<u8>>>,` to
  `SharedState` (+ `test_shared()` init); cap N = 3.
- `BroadcastNativeActions` arm (`swarm.rs:794-810`) — after the broadcast loop, push
  the `envelope` into the ring (bounded: `if len>=3 { pop_front() }`).
- On the flush trigger (RegisterPeer / reconnect, Task 1a) — re-send each recent
  bundle to the newly-(re)registered `vk` via the `direct` protocol (same
  `PRE_PROPOSAL_BATCH_MARKER` envelope), so its mempool catches up before the next
  proposal it must validate. Dedup is free: `add_native_action_from_gossip_trusted`
  ignores `DuplicateNativeAction` (`main.rs:409`).
**Verify**: `cargo test -p torus-network`; devnet reconnect test in Task 5.
**Depends on**: T1, T3.

### Task 5: Metrics + quiet-host flood verification (and congestion measurement)
**Test first**: the bench itself is the test — define the pass bar up front (criteria 3–4).
**Implementation**:
- Add counters (mirror existing `shared.metrics`): `pending_sends_enqueued`,
  `pending_sends_flushed`, `native_bundle_repushed`, and a `missing_action_rejections`
  counter at the `validate_block` reject site (`app.rs:962`).
- Rebuild devnet (`cargo build --release -p torus-node` first — the fast Dockerfile
  copies the host binary), run on a **quiet host** (pause torus-web; do NOT co-run the
  live testnet node).
- Run `bench-throughput consensus --senders 100 --duration 30`; record missing-action
  rejections and included/sec.
- **Measure failure mode #3**: if rejections persist with the queue/reconnect fixes in
  place, the bottleneck is event-loop latency (per `8ee99db3`) → write a follow-up
  note; do NOT silently call Step 3 done.
**Verify**: criteria 3–4 met; metrics logged. `cargo test -p torus-network -p torus-consensus` green.
**Depends on**: T3, T4.

---

## Verification (end-to-end)
```bash
cargo test -p torus-network -p torus-consensus      # units + existing
cargo build --release -p torus-node                 # fast-image binary
cd devnet && ./start.sh --build                      # quiet host only
./target/release/bench-throughput consensus --senders 100 --duration 30
# Expect: missing-action rejections ≈ 0, chain stable, no view stalls.
```

## Rollback
- Each task is additive and independently revertable.
- T2 (`pending_send.rs`) is a standalone module — safe to keep even if T3/T4 revert.
- T3/T4 revert = restore the `warn!`-drop at `swarm.rs:749` and remove the
  `RegisterPeer` flush / bundle re-push. The 100 ms retry is untouched throughout, so
  rollback returns to current behaviour exactly.

## Task 1 findings
_Read-only triage, session (cap100-3val-perf). All line refs verified against current
branch HEAD 5cf4fad. **This gate revises the seams of Tasks 3-4 — read before building.**_

### (a) Reconnect trigger — and a premise correction for Tasks 3-4
- **`peer_map` is populated by the validator set, NOT by `RegisterPeer`.**
  `LibP2PNetwork::init_validator_set` (`bridge.rs:182-198`) writes **all** validators
  into `shared.peer_map` directly at startup, deriving each `peer_id` deterministically
  via `peer_id_from_verifying_key`. `update_validator_set` (`bridge.rs:200-212`) keeps it
  in sync on set changes. The `RegisterPeer` command is documented **"Normally
  unnecessary … kept for ad-hoc registration (observers, tests)"** (`bridge.rs:154-163`).
- **A validator is therefore in `peer_map` from genesis and is never removed on
  disconnect.** `ConnectionClosed` removes **non-validators only** (`swarm.rs:665-673`);
  `Identify::Received` inserts **only when `!contains_vk`** (`swarm.rs:554-560`), i.e. only
  for brand-new (RPC) peers.
- **Consequence — the `Send`-drop @749 premise (failure mode #1) is largely moot for the
  validator↔validator path.** `get_peer_id(target)` returns `Some` for every validator,
  so the `None`/warn-drop branch (`swarm.rs:748-750`) essentially never fires for consensus.
- **The real reconnect signal is `SwarmEvent::ConnectionEstablished` (`swarm.rs:646-658`)**
  — it fires when a disconnected validator's QUIC connection returns, but today it only
  logs + bootstraps Kademlia; it flushes nothing and touches no app state. `Identify` and
  `RegisterPeer` do **not** re-fire on a pure validator reconnect (vk already mapped).
  ⇒ **Flush trigger for Tasks 3-4 must be `ConnectionEstablished`, mapped to a vk via
  `peer_map.get_vk(&peer_id)` — NOT `RegisterPeer`.**

### (b) Direct-protocol `OutboundFailure` is silently dropped
- `handle_event` matches only `Direct(Event::Message{Request})` (`swarm.rs:467-537`).
  There is **no arm** for `Direct` `Response`, `OutboundFailure`, or `InboundFailure`
  — they fall through to `_ => {}` (`swarm.rs:697`). (Contrast: the `BlockData` protocol
  *does* log both failures, `swarm.rs:628-637`.)
- ⇒ When a consensus `Vote`/`NewView` or a pre-proposal native push is `send_request`-ed
  to a **mapped-but-currently-disconnected** validator, libp2p dials; if delivery fails the
  `OutboundFailure` is **lost with no log and no retry**. **This — not the peer-map miss —
  is the actual consensus-unicast loss**, and the queue-on-peer-map-miss design (T3) does
  not catch it.

### (c) Congestion hypothesis (failure mode #3) vs current gossip-off code
- Native-action gossip is confirmed OFF: `native_gossip_enabled=false`
  (`mempool/lib.rs:116`), gated at `lib.rs:296`; the outbound `native_action_rx` flush
  path (`swarm.rs:274-318`) is dormant. **Keep it off** (guardrail).
- But consensus itself still uses **gossipsub** (`Broadcast`→publish, `swarm.rs:709-729`),
  and the per-block pre-proposal push (`BroadcastNativeActions`, `swarm.rs:794-810`)
  serializes the whole action batch and `send_request`s it to each validator on the
  command path (P1). The **receiver deserializes that batch inline on the event loop**
  (`swarm.rs:497-512`). Under cap-100 flood this inline (de)serialization + dial churn is a
  plausible source of the 1.5-2s push latency in mem `8ee99db3`, **independent of peer-map
  state**. ⇒ T3/T4 cannot be assumed to cure it; **T5 must measure** (do not declare done
  on green unit tests alone).

### (d) Block-sync triage (failure mode #4 — live in node.log) — SEPARATE root cause
- **`worker fetch error (timeout/disconnect)` is fatal to the session, no retry**
  (`client.rs:194-199`): `SyncResult::Error` → `check_commitment_and_end_session()` →
  which **blacklists** the peer for `blacklist_expiry=10s` whenever
  `blocks_synced < min_blocks_expected` (`client.rs:503-521`) — true for any 0-block
  timeout. Worker fetch timeout is `block_sync_response_timeout=3s` (`main.rs:442`).
  (NB: distinct from the *body-fetch* path, which already retries via `body_fetch_tracker`
  / `tick_pending_body_retries`, mem `a06daf38` + `algorithm.rs:186` — that one is fine.)
- **On a 3-validator set this is a liveness cliff:** blacklisting the 1-2 reachable sync
  servers empties `available_sync_servers` → `random_sync_server` returns `None` →
  "no available sync servers, cannot sync" (`client.rs:419-422`) until a blacklist expires.
- **`justify_block_known=false` is a *trigger*, not terminal.** A header whose parent is
  unknown is dropped at `implementation.rs:1537-1556`, which sets `sync_needed=true`
  (1543-1544); `algorithm.rs:186-191/215-219` then calls `trigger_sync`
  (`client.rs:99-110`). Recovery hinges on that sync succeeding — which the timeout→
  blacklist→starvation cascade above can defeat.
- **The acute historical cause is already fixed on THIS branch.** Mem `4c5d41161`
  (ProposalHeaders dropped by the view filter) is mitigated: `is_block_data_msg()` includes
  `ProposalHeader` (`messages.rs:106-108`) and the view filter bypasses it
  (`receiving.rs:186`).
- **The live node.log is the OLD `59e595e` cap-1000 binary, not this branch** (mem
  `07d92e4f`) — so its block-sync failures are partly pre-fix. The residual, branch-relevant
  block-sync risk is the **fetch-timeout → blacklist → server-starvation cascade** under load.

### VERDICT — is Option D's delivery hardening SUFFICIENT?
**NECESSARY but NOT SUFFICIENT, and the plan's seams need correction.**
1. **Option D is still the right delivery fix** — gossip stays off, CompactBlock stays,
   retry stays. But two of its target seams are wrong as written:
   - **Flush trigger** = `ConnectionEstablished` (→ vk via `get_vk`), **not** `RegisterPeer`.
   - **Enqueue gate (T3)** = `!swarm.is_connected(&pid)` for a mapped validator, **not**
     "absent from `peer_map`" (which doesn't happen for validators). Plus **handle
     `Direct` `OutboundFailure`** (log + re-enqueue) to catch post-dial loss (finding b).
   - T2 (`PendingSendQueue`) is unaffected — build it as specified.
   - T4 re-push bundles likewise keys off `ConnectionEstablished`.
2. **Block-sync is a co-equal root cause that Option D does not touch.** Recommend a
   **companion task (T6 / separate step):** on transient `worker fetch error`, **retry the
   same fetch ≥1× before ending the session**, and/or **do not blacklist on
   timeout/disconnect** (reserve blacklist for `is_correct`/app-validation failures —
   distinguish "bad data" from "slow/unreachable"), and/or **never blacklist the last
   reachable server** on small (≤4) validator sets.
3. **Do not declare Step 3 done on unit-test green.** Gate the call on the **T5 quiet-host
   bench measuring BOTH** missing-action rejections **and** block-sync errors / view-stall
   rate, on this branch's binary (the live log was the old binary).

**Recommendation:** proceed with Tasks 2-5 using the corrected seams above, and add the
block-sync companion (T6) — but get explicit approval on the scope (D-only vs D+T6) before
building, since T6 widens scope beyond "dissemination hardening".

## Implementation status (branch cap100-3val-perf)
Scope decision: **Option 1** — Option D now (corrected seams); the T5 bench decides T6.

| Task | Status | Commit | Verify |
|------|--------|--------|--------|
| T1 triage gate | done | `50513b3` | findings above; verdict approved |
| T2 PendingSendQueue | done | `f254e66` | `cargo test -p torus-network -- pending_send` (3/3) |
| T3 enqueue/flush + OutboundFailure | done | `ad7c136` | `torus-network` 20 lib + 9 integ green |
| T4 native-bundle re-push ring | done | `7570e7b` | ring unit test + suite green |
| T5 metrics | done | `cc3aec9` | `torus-network`+`torus-consensus` green; `torus-node` compiles |
| T5 quiet-host bench | **DEFERRED** | — | host not quiet (live testnet node 336% CPU + torus-web; load 12/8) — user deferred |
| T6 block-sync companion | gated on T5 bench | — | only if the bench shows block-sync stalls |

**Corrected seams applied (from the T1 triage; differ from the original task text):**
- Flush trigger = `SwarmEvent::ConnectionEstablished` (→ vk via `get_vk`), not `RegisterPeer`
  (validators are pre-mapped by `init_validator_set`, so they never miss registration).
- T3 enqueue gate = `!swarm.is_connected(pid)` for a mapped validator, not "absent from peer_map".
- `Direct` `OutboundFailure` (previously swallowed by `_ => {}`) now logs + re-enqueues via the
  `outbound_direct` request-id map; `Direct` `Response` untracks on ack; `InboundFailure` logs.

**Next session:** run the deferred bench on a quiet host (pause torus-web + the live testnet
node, or use a separate box): `cargo build --release -p torus-node` then
`bench-throughput consensus --senders 100 --duration 30`. Capture `torus_missing_action_rejections`
+ the other 3 counters and the block-sync/view-stall rate against the PASS BAR, then decide T6.
**Do NOT declare Step 3 done until the bench runs.**
