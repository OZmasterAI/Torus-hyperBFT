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

### Task 1: Scope the delivery seams (read-only, no code)
**Test first**: n/a — investigation. **Verify** = a findings block appended below as
`## Task 1 findings`.
Confirm in `crates/torus-network/src/swarm.rs`:
- (a) What event fires on a validator **reconnect** — `SwarmEvent::ConnectionEstablished`
  vs `identify::Event::Received` (`swarm.rs:538`) — and whether it re-populates
  `peer_map`. **This is the flush trigger for Tasks 3–4.**
- (b) How the `direct` request_response `OutboundFailure` is handled today (find the
  event arm). Decides whether an explicit send-retry is needed.
- (c) Sanity-check failure mode #3 against current (gossip-off) code paths.
**Verify**: `grep -n "ConnectionEstablished\|OutboundFailure\|identify::Event" crates/torus-network/src/swarm.rs` returns the arms; findings written.
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
_(to be filled during implementation)_
