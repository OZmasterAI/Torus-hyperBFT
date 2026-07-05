# Implementation Plan: Mesh Watchdog + Gossipsub Explicit Peers

## Design Decision

Fix the fast-restart gossipsub wedge (S395/S400/S402: validator peer connected but
absent from consensus-topic subscriptions → silent degraded mode 2.1 blk/s @ 1.4
views/block, or full wedge) with two complementary mechanisms plus visibility:

1. **Watchdog** (the cure for the subscription wedge): a validator peer that stays
   *connected but not subscribed* to `/torus/consensus/1.0` for >60s is
   force-disconnected via `disconnect_peer_id`. Reconnection (mesh tick redials
   within ≤10s) re-runs the subscription exchange, which is the state that got
   lost in the race. Memory: 23c1b10e89525a41 item (a).
2. **Explicit peers** (hardening, NOT the cure): `gossipsub.add_explicit_peer`
   for all validator peer-ids. Per gossipsub v1.1 spec, explicit peers are
   redialed on heartbeat and receive forwarded messages regardless of mesh
   membership — but **only for topics they are subscribed to**, so this does not
   fix a lost subscription on its own. It removes the mesh-prune failure mode
   and adds automatic redial.
3. **Mesh metrics**: gauges for consensus-topic mesh size and subscribed
   validator count, counter for watchdog kicks — so degraded mode is visible
   instead of inferred from views/block.

Rationale for watchdog-over-rewrite: subscription exchange happens once per
connection in libp2p gossipsub; when the initial subscription message races a
dying connection (the in-place-restart footgun), no protocol mechanism ever
retransmits it. Forcing a fresh connection is the only recovery that doesn't
patch libp2p itself.

Safety argument: a false-positive kick costs one reconnect (≤10s to next mesh
tick redial, subscription exchange <1s) vs. an indefinite wedge. The 60s grace
comfortably covers normal post-connect subscription latency (<1s).

## Verified code anchors (read S405)

- `crates/torus-network/src/swarm.rs:544-545` — existing 10s `mesh_interval`
- `crates/torus-network/src/swarm.rs:587-605` — mesh tick body (dials unconnected validators from `peer_map`)
- `crates/torus-network/src/swarm.rs:513` — `consensus_topic: IdentTopic` built before the loop
- `crates/torus-network/src/swarm.rs:238-240` — `SharedState { peer_map, validators, metrics: Option<Arc<Metrics>> }`
- `crates/torus-network/src/swarm.rs:371-377` — `is_validator_peer` (peer_map vk → validators set)
- `crates/torus-network/src/swarm.rs:1239` — precedent for `swarm.disconnect_peer_id(peer_id)`
- `crates/torus-network/src/behaviour.rs:12` — `CONSENSUS_TOPIC`
- `crates/torus-telemetry/src/lib.rs:176+` — `Metrics::new()` registration idiom (prometheus-client `Registry::register`, `Counter::default()`/`Gauge::default()`)
- libp2p pinned `=0.56.0` (workspace Cargo.toml:72); gossipsub exposes
  `add_explicit_peer(&PeerId)`, `all_peers() -> (&PeerId, Vec<&TopicHash>)`,
  `mesh_peers(&TopicHash)`.

## Success Criteria

- Unit: watchdog state machine — grace respected, no kick-spam, cleanup on
  disconnect/resubscribe — all green.
- All existing torus-network + torus-telemetry tests still pass
  (`cargo test -p torus-network -p torus-telemetry`).
- End-to-end (devnet, 3 validators): induced fast-restart wedge heals within
  ≤70s with the fix (watchdog counter increments, views/block returns to ~1.0);
  identical scenario on the base build stays degraded.
- New metrics visible on `/metrics`: `torus_consensus_mesh_peers`,
  `torus_consensus_subscribed_validators`, `torus_mesh_watchdog_disconnects`.

## Tasks (one normal commit each, in order)

### Task 1: Telemetry — mesh gauges + watchdog counter

**Test first** (`crates/torus-telemetry/src/lib.rs` tests module):
```rust
#[test]
fn mesh_watchdog_metrics_registered() {
    let m = Metrics::new();
    m.consensus_mesh_peers.set(2);
    m.consensus_subscribed_validators.set(2);
    m.mesh_watchdog_disconnects.inc();
    let text = m.render(); // existing encode path used by /metrics endpoint
    assert!(text.contains("torus_consensus_mesh_peers"));
    assert!(text.contains("torus_consensus_subscribed_validators"));
    assert!(text.contains("torus_mesh_watchdog_disconnects"));
}
```
(Adjust `render()` to whatever the existing registry-encode fn is named.)

**Implementation**: add three fields to `Metrics` (lib.rs:20+) and register in
`Metrics::new()` following the lib.rs:186-191 idiom:
```rust
pub consensus_mesh_peers: Gauge,            // gossipsub mesh size, consensus topic
pub consensus_subscribed_validators: Gauge, // connected validator peers subscribed to consensus topic
pub mesh_watchdog_disconnects: Counter,     // forced disconnects of zero-subscription validators
```

**Verify**: `cargo test -p torus-telemetry`
**Depends on**: —

### Task 2: `mesh_watchdog` module — pure state machine + unit tests

**Test first** (`crates/torus-network/src/mesh_watchdog.rs` tests):
- `subscribed_peer_never_kicked`
- `unsubscribed_peer_kicked_only_after_grace` (t0 tracked, t0+59s no kick, t0+60s kick)
- `kick_resets_timer_no_spam` (after a kick, next tick does not kick again immediately)
- `disconnect_clears_tracking` (peer absent from `connected` → forgotten)
- `resubscribe_clears_tracking`

**Implementation** (new file, `pub mod mesh_watchdog;` in lib.rs):
```rust
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use libp2p::PeerId;

/// How long a validator peer may stay connected-but-unsubscribed to the
/// consensus topic before we force a reconnect. Subscription exchange normally
/// completes <1s after ConnectionEstablished; 60s = wedged, not slow.
pub const WATCHDOG_GRACE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct MeshWatchdog {
    unsubscribed_since: HashMap<PeerId, Instant>,
}

impl MeshWatchdog {
    /// One mesh-tick evaluation. `connected` = currently connected validator
    /// peers; `subscribed` = subset gossipsub reports as subscribed to the
    /// consensus topic. Returns peers to force-disconnect (grace expired).
    pub fn tick(
        &mut self,
        now: Instant,
        connected: &[PeerId],
        subscribed: &HashSet<PeerId>,
    ) -> Vec<PeerId> {
        self.unsubscribed_since
            .retain(|p, _| connected.contains(p) && !subscribed.contains(p));
        let mut kick = Vec::new();
        for p in connected {
            if subscribed.contains(p) {
                continue;
            }
            let since = *self.unsubscribed_since.entry(*p).or_insert(now);
            if now.duration_since(since) >= WATCHDOG_GRACE {
                kick.push(*p);
                self.unsubscribed_since.remove(p); // grace restarts post-kick
            }
        }
        kick
    }
}
```

**Verify**: `cargo test -p torus-network mesh_watchdog`
**Depends on**: —

### Task 3: Wire watchdog into the mesh tick

**Test first**: devnet-level (Task 5 script); at unit level the seam is the pure
module (Task 2). Add a compile-level assertion test only if a cheap one exists.

**Implementation** (`swarm.rs`, extend the `mesh_interval.tick()` arm at 587-605):
```rust
_ = mesh_interval.tick() => {
    let local_pid = *swarm.local_peer_id();
    let peer_map = shared.peer_map.read().unwrap();
    let validator_pids: Vec<PeerId> =
        peer_map.peer_ids().filter(|pid| **pid != local_pid).copied().collect();
    drop(peer_map);

    // (existing) dial unconnected validators
    let to_dial: Vec<PeerId> =
        validator_pids.iter().filter(|pid| !swarm.is_connected(pid)).copied().collect();
    ...existing dial block...

    // watchdog: connected-but-unsubscribed validators (S395 wedge)
    let connected: Vec<PeerId> =
        validator_pids.iter().filter(|pid| swarm.is_connected(pid)).copied().collect();
    let consensus_hash = consensus_topic.hash();
    let subscribed: HashSet<PeerId> = swarm.behaviour().gossipsub.all_peers()
        .filter(|(_, topics)| topics.contains(&&consensus_hash))
        .map(|(p, _)| *p)
        .collect();
    for pid in watchdog.tick(std::time::Instant::now(), &connected, &subscribed) {
        warn!(%pid, "mesh watchdog: validator connected but unsubscribed >60s — forcing reconnect");
        let _ = swarm.disconnect_peer_id(pid);
        if let Some(ref m) = shared.metrics { m.mesh_watchdog_disconnects.inc(); }
    }
    if let Some(ref m) = shared.metrics {
        m.consensus_mesh_peers
            .set(swarm.behaviour_mut().gossipsub.mesh_peers(&consensus_hash).count() as i64);
        m.consensus_subscribed_validators
            .set(connected.iter().filter(|p| subscribed.contains(*p)).count() as i64);
    }
    continue;
}
```
`let mut watchdog = MeshWatchdog::default();` next to `mesh_interval` (swarm.rs:544).

**Verify**: `cargo test -p torus-network` + `cargo build --release` + Task 5 devnet run
**Depends on**: Tasks 1, 2

### Task 4: Gossipsub explicit peers for validators

**Pre-step (verify semantics, no code)**: read vendored
`libp2p-gossipsub-0.49*/src/behaviour.rs` in `~/.cargo/registry` to confirm
0.56.0 behavior of `add_explicit_peer`: heartbeat redial + forward-regardless-
of-mesh (subscription still required). Record findings in the commit message.

**Test first**: behavior is not observable via public gossipsub API; verified
at devnet level (Task 5). Unit test only that the wiring is idempotent (calling
twice doesn't panic).

**Implementation** (same mesh-tick arm, before the dial block):
```rust
// explicit peering: keep validators gossip-connected regardless of mesh
// churn; gossipsub redials explicit peers on heartbeat. (Does NOT replace
// the watchdog — explicit forwarding still requires their subscription.)
for pid in &validator_pids {
    swarm.behaviour_mut().gossipsub.add_explicit_peer(pid);
}
```
(Idempotent per tick; peer_map can gain validator pids after startup, so the
tick is the right place, not one-shot init.)

**Verify**: `cargo test -p torus-network` + Task 5 devnet A/B
**Depends on**: Task 3

### Task 5: Devnet wedge repro + A/B verification

**Test first**: this IS the end-to-end test.

**Implementation**: `devnet/scripts/wedge-repro-s405.sh`:
1. Launch 3-validator local devnet (existing devnet scripts as base).
2. Induce the footgun: in-place fast restart (kill + immediate relaunch, no
   20s wait) of val B, repeat up to 10× or until seed reports val B connected
   with `torus_consensus_subscribed_validators` < expected for >30s.
3. Record: views/block, `torus_mesh_watchdog_disconnects`,
   `torus_consensus_mesh_peers`, block cadence, before/after heal.
4. Run once on base build (expect: degraded state persists ≥5min or wedge)
   and once on fixed build (expect: watchdog kick within ≤70s, views/block
   back to ~1.0, cadence recovers).
5. While a node is wedged (base run): send SIGTERM, record whether it hangs
   (SIGTERM-HANG bug repro data — observation only, out of scope to fix).

**Verify**: script output committed under `devnet/wedge-repro-s405/` summary
(not raw data dirs — respect DO-NOT-COMMIT list).
**Depends on**: Tasks 3, 4

## Verification (end-to-end)

- `cargo test -p torus-network -p torus-telemetry` green.
- Devnet A/B per Task 5: fixed build heals induced wedge ≤70s, base does not.
- `curl -s 127.0.0.1:<metrics>/metrics | grep -E 'mesh_peers|subscribed_validators|watchdog'` shows sane values on devnet.
- NOT deployed to live testnet in this plan — live rollout is a separate
  decision after devnet proof (stop→wait-20s→start protocol, and ideally after
  val3's upgrade so we don't confound the cadence re-measurement).

## Rollback

Each task is one normal commit; revert in reverse order (5→1) with plain
`git revert` — no wire-format or consensus-rule changes anywhere in this plan,
so reverts are deployment-safe per node.
