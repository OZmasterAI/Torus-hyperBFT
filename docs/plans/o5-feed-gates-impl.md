# Implementation Plan: O5 — Feed/Dissemination Gates (cap-ladder reconciliation)

Roadmap: docs/plans/blockspeed-orders-roadmap-s405.md Phase 2, item O5.
Written S415 (2026-07-06). Branch: sprint/blockspeed-orders-s395.

## Design Decision: Accept-first ladder reconciliation + measured threshold raise

The message-size caps form a ladder that today self-limits bs400+ dissemination
and hides two liveness traps:

| Gate | Value | Where | Enforced on |
|------|-------|-------|-------------|
| consensus gossip accept | 256 KB | `torus-network/src/config.rs:118` → `swarm.rs:801` | RECEIVE: drop + penalize author |
| gossipsub transmit | 2 MiB (hardcoded) | `torus-network/src/behaviour.rs:54` | transport, both directions |
| `/torus/direct` codec | 4 MB | `torus-network/src/codec.rs:30` | RECEIVE: read_frame cap |
| hash-only push threshold | 512 KB compiled; **6 MB env on seed+val1 since S388** | `torus-network/src/bridge.rs:96` | SEND: proposer-local policy |
| `/torus/native-da` codec | 8 MB | `codec.rs:33` | RECEIVE (pull chunks of 16 hashes) |
| `/torus/block-data` codec | 16 MB | `codec.rs:35` | RECEIVE (sync fetch) |
| native block bytes budget | 6 MB | `torus-mempool/src/rate_limit.rs:141` | SEND: proposer selection |

Facts established from code + memory (S415):

1. **The 512 KB compiled threshold forces every bs400 block onto the
   manifest+pull path.** bs400 bodies ≈ 2.8 MB > 512 KB → every validator pulls
   ~2.8 MB in 16-hash chunks per block. S387 proved where that path saturates
   (mem f852bcc0): all validators pulling ALL bodies from the proposer wedged
   bs800+ until c408c0e moved pull-serving off the swarm loop. Pull was
   designed as the RARE fallback (mem a6cf33a9); at bs400+ with a 512 KB
   threshold it is the primary path on every block.
2. **The live operating point already violates the ladder.** S388 deployed
   `TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000` on seed+val1 (mem 8201230c, env in
   val1 systemd drop-in + seed nohup line — carried by the relaunch cmdline).
   6 MB > the 4 MB `/torus/direct` codec cap: any body set in (4 MB, 6 MB]
   (bs~570+) is pushed, rejected by every receiver's codec, and survives only
   via pull fallback — wasted MB-sends + added latency, silently. The
   bridge.rs:615 assert pins only the COMPILED const; the env path bypasses it.
3. **The 256 KB accept gate is a livelock trap vs EVM-inline proposals.**
   CompactBlock carries `evm_transactions` INLINE (torus-types/src/lib.rs:432).
   EVM selection is gas-budgeted (5M), not byte-budgeted: 5M gas ÷ 16 gas/byte
   ≈ 312 KB of calldata is selectable — a legal compact proposal > 256 KB.
   There is NO send-side size check before gossip publish; every receiver
   drops it AND penalizes the proposer (swarm.rs:801-815) → view timeout →
   next leader selects the same mempool → repeat. Not yet seen live (testnet
   EVM traffic is small) but reachable by construction.
4. **Compact manifests are small** (32 B/action: cap=100 → ~3.2 KB, cap=1000
   probe → ~32 KB), so 256 KB never binds on native manifests. The bs400+
   binding constraints are (1)/(2) and the 4 MB direct cap (full-body push
   impossible ≥ 4 MB, e.g. bs600 ≈ 4.2 MB).
5. **The 2 MiB gossip cap is hardcoded** — same silent-drift class as the S391
   heartbeat bug documented at behaviour.rs:41-50. Nothing asserts the ladder
   ordering except bridge.rs:615 and a stale comment at hotstuff
   messages.rs:552 ("4MB direct msg" — block-data rides the 16 MB protocol).

**Chosen approach (C): raise the RECEIVE/accept gates now (mixed-fleet safe in
both directions — accepting more than anyone sends is harmless), clamp the env
threshold to what the fleet's oldest codec can actually read, wire the gossip
cap to config, encode the ladder as tests, and change compiled SEND defaults
only on measurement** — devnet proves mechanics this sprint; the WAN decision
rides the testnet-relaunch checklist via the same env knob (proposer-local,
A/B-safe per bridge.rs:99-105, cannot split consensus).

Rejected:
- **(A) Only raise the push threshold default** — leaves the EVM livelock
  trap, the deployed 6 MB-vs-4 MB violation, the hardcode drift, and no path
  past 4 MB; partial fix.
- **(B) Big-bang raise including send-side defaults** — un-upgraded receivers
  (val3 is on an old build) drop + penalize the new sender; threshold raise
  WAN-unproven (the wedge it guards is bandwidth-bound, mem f58957c6);
  violates the explicit-peers-class rollout discipline the roadmap demands.

**Consensus-safety note:** none of these gates are consensus rules —
`validate_block` does not reject on any of them; they are transport/app
receive gates and proposer-local send policy. Mixed values cannot fork. The
risk class is LIVENESS (drop + penalize), which is why rollout ordering still
gets explicit-peers-level care.

## Success Criteria

- New ladder-invariant tests pass, including the EVM worst-case test that is
  RED at 256 KB and GREEN at 1 MB (proves the raise closes a real trap).
- Env threshold requests above the fleet floor are clamped with a WARN log —
  unit-tested; a 6 MB request yields a 4 MB effective threshold.
- Gossip `max_transmit_size` reads from `NetworkConfig` (default behavior
  byte-identical: 2 MiB), with a regression test.
- Devnet bs400 flood at threshold=3 MB: native-DA pull-fallback lines ≈ 0
  (bodies travel as ONE full-body push), block time ≤ the 512 KB control leg
  (regression fit over the series, not endpoints — S405 lesson), no wedge,
  chain advances monotonically in all legs.
- No consensus-rule change: `validate_block` untouched.
- Existing suites pass. Known pre-existing failures excluded:
  `pacemaker::update_view_on_schedule_keeps_cumulative_deadline`,
  `dynamic_validators` ×2 (mem 1b155043c8e36a0f).
- Compiled `HASH_ONLY_PUSH_THRESHOLD` default UNCHANGED this sprint (WAN-gated).

## Tasks

### Task 1: caps.rs — single source of truth + ladder invariants
**Depends on:** —
**Test first** (fails to compile until the module exists):
`crates/torus-network/src/caps.rs`, tests at bottom:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::HASH_ONLY_PUSH_THRESHOLD;
    use crate::config::NetworkConfig;

    #[test]
    fn size_ladder_is_coherent() {
        let cfg = NetworkConfig::default();
        // App-level accept gates fit inside the transport they ride.
        assert!(cfg.max_consensus_message_size <= GOSSIP_MAX_TRANSMIT_SIZE);
        assert!(cfg.max_tx_message_size <= GOSSIP_MAX_TRANSMIT_SIZE);
        // Manifest mode must engage before the direct codec rejects the push,
        // on the OLDEST binary still in the fleet (val3 floor), not just ours.
        assert!(HASH_ONLY_PUSH_THRESHOLD < LEGACY_FLEET_DIRECT_MSG_FLOOR);
        assert!(LEGACY_FLEET_DIRECT_MSG_FLOOR <= MAX_DIRECT_MSG_SIZE);
        // Recovery paths widen monotonically: push < pull < sync.
        assert!(MAX_DIRECT_MSG_SIZE <= MAX_NATIVE_DA_MSG_SIZE);
        assert!(MAX_NATIVE_DA_MSG_SIZE <= MAX_BLOCK_DATA_MSG_SIZE);
    }
}
```
**Implementation:** new `crates/torus-network/src/caps.rs`; MOVE
`MAX_DIRECT_MSG_SIZE`, `MAX_NATIVE_DA_MSG_SIZE`, `MAX_BLOCK_DATA_MSG_SIZE`
(with their doc comments) out of `codec.rs:27-35`; add:
```rust
/// Gossipsub transport frame cap (publish AND inbound decode). Was hardcoded
/// at behaviour.rs:54 — same silent-drift class as the S391 heartbeat bug.
pub const GOSSIP_MAX_TRANSMIT_SIZE: usize = 2 * 1024 * 1024;

/// Direct-msg read cap of the OLDEST binary in the fleet (pre-O5 = 4 MB).
/// Send-side behavior (push threshold) must stay under THIS, not under our
/// own MAX_DIRECT_MSG_SIZE, until the whole fleet carries the raise. Moving
/// this const IS the act of re-declaring the fleet floor.
pub const LEGACY_FLEET_DIRECT_MSG_FLOOR: usize = 4 * 1024 * 1024;
```
`codec.rs` re-imports via `use crate::caps::*;` (call sites unchanged);
`lib.rs` gains `pub mod caps;`. bridge.rs:615's existing assert stays
(now redundant with the ladder test, harmless).
**Verify:** `cargo test -p torus-network caps`

### Task 2: wire gossipsub max_transmit_size to config
**Depends on:** Task 1
**Test first** (fails: signature has no such param):
`crates/torus-network/src/behaviour.rs` tests:
```rust
#[test]
fn gossipsub_transmit_cap_is_configurable_not_hardcoded() {
    let cfg = gossipsub_config(100, 3 * 1024 * 1024).unwrap();
    assert_eq!(cfg.max_transmit_size(), 3 * 1024 * 1024);
    // Default path stays byte-identical to the shipped 2 MiB behavior.
    let default_cfg = gossipsub_config(100, crate::caps::GOSSIP_MAX_TRANSMIT_SIZE).unwrap();
    assert_eq!(default_cfg.max_transmit_size(), 2 * 1024 * 1024);
}
```
**Implementation:** `gossipsub_config(heartbeat_ms: u64, max_transmit: usize)`
(behaviour.rs:51) with `.max_transmit_size(max_transmit)`;
`with_limits_and_heartbeat` gains the param and passes it through; bridge.rs
call site (~bridge.rs:218) passes
`caps::GOSSIP_MAX_TRANSMIT_SIZE.max(config.max_consensus_message_size).max(config.max_tx_message_size)`
so a future config raise can never silently exceed the transport again.
Update the other `with_limits*` callers (behaviour.rs:62-70 defaults, tests).
**Verify:** `cargo test -p torus-network`

### Task 3: EVM worst-case ladder test — make the livelock trap visible (RED)
**Depends on:** Task 1
**Test first** — `crates/torus-integration-tests/tests/o5_feed_gates.rs`:
```rust
//! O5 cross-crate size-ladder invariants (torus-network × torus-mempool).

use torus_mempool::rate_limit::{
    evm_block_gas_budget, native_total_block_cap, NATIVE_ORDERS_PER_BATCH_CAP,
};
use torus_network::config::NetworkConfig;

/// EVM calldata costs >= 16 gas/byte, so the gas budget bounds selectable
/// EVM bytes. A compact proposal carries those bytes INLINE — it must fit
/// the consensus accept gate or the proposer livelocks (drop+penalize loop).
#[test]
fn worst_case_compact_proposal_fits_consensus_accept_gate() {
    let evm_worst = (evm_block_gas_budget() / 16) as usize;      // 5M/16 = 312.5 KB
    let manifest = native_total_block_cap() * 32;                 // action hashes
    let header_slack = 4 * 1024;                                  // header + bincode framing
    let worst = evm_worst + manifest + header_slack;
    let accept = NetworkConfig::default().max_consensus_message_size;
    assert!(
        worst <= accept,
        "worst-case compact proposal {worst}B exceeds consensus accept gate {accept}B \
         — receivers drop+penalize, proposer livelocks (O5)"
    );
}

/// Worst single pull chunk (16 max-size batch bodies) fits the native-DA codec.
#[test]
fn max_pull_chunk_fits_native_da_codec() {
    use torus_types::{
        ActionSignature, FixedPoint, NativeAction, OrderType, PlaceOrderParams, Signature,
        SignedNativeAction, TimeInForce,
    };
    let order = PlaceOrderParams {
        market_id: 1,
        is_buy: true,
        price: FixedPoint::from_raw(6_000_000_000_000),
        quantity: FixedPoint::from_raw(FixedPoint::SCALE),
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::GTC,
        reduce_only: false,
        client_order_id: None,
    };
    let batch = SignedNativeAction {
        action: NativeAction::PlaceOrderBatch(vec![order; NATIVE_ORDERS_PER_BATCH_CAP]),
        nonce: 0,
        signature: ActionSignature::Eip712(Signature { v: 27, r: [0u8; 32], s: [0u8; 32] }),
    };
    let one = bincode::serialize(&batch).unwrap().len();
    assert!(
        torus_network::bridge::NATIVE_DA_FETCH_CHUNK * (one + 64)
            <= torus_network::caps::MAX_NATIVE_DA_MSG_SIZE
    );
}
```
First test MUST FAIL at 256 KB (312.5 KB + 3.2 KB + 4 KB > 256 KB) — run it,
record the failure output; that is the proof the Task 4 raise is load-bearing.
**Verify:** `cargo test -p torus-integration-tests --test o5_feed_gates` → RED

### Task 4: raise the accept gates (receive-side only) → Task 3 goes GREEN
**Depends on:** Task 3 (must be red first)
**Implementation:**
- `config.rs:118`: `max_consensus_message_size: 1024 * 1024` + comment: accept
  gate only (swarm.rs:801 drop+penalize); sized for EVM worst case 312.5 KB +
  manifest + 3× headroom; must stay ≤ gossip transmit (ladder test); senders
  may NOT exploit until fleet-wide (rollout section below).
- `caps.rs`: `MAX_DIRECT_MSG_SIZE: usize = 8 * 1024 * 1024` (read cap raise;
  `LEGACY_FLEET_DIRECT_MSG_FLOOR` stays 4 MB and keeps guarding send policy).
- `app.rs:2140-2166`: update the stale local mirror
  `MAX_CONSENSUS_MESSAGE_SIZE` 256→1024 KB + comment (torus-consensus has no
  torus-network dep, so it stays a documented mirror; the bs=500 full block
  is ~2.5 MB so the test's full>cap assertion still holds).
- `hotstuff_rs/src/hotstuff/messages.rs:552`: keep the 4 MB assert as a
  conservative bound, fix the stale comment to name `/torus/block-data`
  (16 MB) as the actual transport.
**Verify:** `cargo test -p torus-integration-tests --test o5_feed_gates`
(GREEN), then `cargo test -p torus-network -p torus-consensus`

### Task 5: clamp the env threshold to the fleet floor (+WARN)
**Depends on:** Task 1
The S388 deployment (`TORUS_HASH_ONLY_PUSH_THRESHOLD=6000000` > 4 MB codec
cap) is live proof the env knob can silently order pushes every receiver must
reject. Clamp is proposer-local send policy — consensus-safe, and strictly
better than the status quo for bodies in (floor, requested]: manifest+pull
instead of a doomed multi-MB push that ends in pull anyway.
**Test first** (pure seam, no env mutation — same pattern as
`should_push_hashes_only_at`):
```rust
#[test]
fn env_threshold_clamps_to_fleet_floor() {
    use crate::caps::LEGACY_FLEET_DIRECT_MSG_FLOOR;
    // S388 live value: 6 MB requested, 4 MB fleet floor -> clamped.
    assert_eq!(
        effective_push_threshold_at(6_000_000, LEGACY_FLEET_DIRECT_MSG_FLOOR),
        LEGACY_FLEET_DIRECT_MSG_FLOOR
    );
    // At or under the floor: honored verbatim.
    assert_eq!(effective_push_threshold_at(512 * 1024, LEGACY_FLEET_DIRECT_MSG_FLOOR), 512 * 1024);
    assert_eq!(
        effective_push_threshold_at(LEGACY_FLEET_DIRECT_MSG_FLOOR, LEGACY_FLEET_DIRECT_MSG_FLOOR),
        LEGACY_FLEET_DIRECT_MSG_FLOOR
    );
}
```
**Implementation** in bridge.rs, inside `hash_only_push_threshold()`'s
`get_or_init` (so the WARN fires once):
```rust
fn effective_push_threshold_at(requested: usize, floor: usize) -> usize {
    requested.min(floor)
}
```
and in the OnceLock init: if `requested > floor`, `warn!(requested, floor,
"TORUS_HASH_ONLY_PUSH_THRESHOLD above fleet direct-msg floor — clamped");`.
Update the bridge.rs:98-105 doc comment to describe the clamp.
**Verify:** `cargo test -p torus-network bridge`

### Task 6: devnet bs400 threshold sweep (mechanics proof; default unchanged)
**Depends on:** Tasks 1-5 built
**Test first:** the acceptance predicate IS the measurement — write
`devnet/sweep-o5-threshold-s415.sh` emitting `devnet/sweep-o5-s415.csv` with
columns `leg,threshold,orders_s,block_ms_fit,pull_fallback_lines,wedged`.
At fixed bs400 (bodies ≈2.8 MB) the threshold is binary — manifest+pull vs
full push — so two legs decide it (a mid leg like 1.5 MB behaves identically
to 512 KB):
1. control `TORUS_HASH_ONLY_PUSH_THRESHOLD=524288` → manifest+pull every block
2. full-push `=3145728` (2.8 MB < 3 MB → single body push; 3 MB < 4 MB floor,
   so this leg is valid against ANY fleet build)
Same 3-node devnet, native-order-flood bs400, disjoint buyer/seller sender
pools (STP eats fills otherwise — known gotcha), ~90 s each, paced not burst
(s367 lesson). Block-time via regression fit over the height series.
**Verify:** acceptance = leg 2 pull_fallback_lines ≈ 0 AND block_ms_fit(leg2)
≤ block_ms_fit(leg1) AND wedged=0 in both legs. Devnet DOWN after (policy).
**Explicitly out of scope:** flipping the compiled 512 KB default — WAN-gated
(bridge.rs:104, bandwidth-bound wedge f58957c6). At testnet relaunch, run the
seed-only env A/B: deployed-6MB-now-clamped-4MB vs 512 KB control, 150 s
windows; flip the default in its own commit citing the WAN numbers.

### Task 7: rollout doc + roadmap check-off
**Depends on:** Tasks 1-6
**Implementation:** the "Rollout ordering" section below is the deliverable;
update `docs/plans/blockspeed-orders-roadmap-s405.md` O5 entry to [x] with
results + the two follow-ups it spawns (WAN A/B at relaunch; compiled-default
flip); note in the relaunch checklist that seed+val1 env (6 MB) will now
clamp to 4 MB with a WARN — expected, not a regression. remember_this() the
outcome.
**Verify:** `git diff --stat` shows only docs; roadmap renders the check.

## Verification (end-to-end)
1. `cargo test -p torus-network -p torus-consensus -p torus-integration-tests`
   — all green (minus the 3 known pre-existing failures listed above).
2. Task 3 test demonstrably red-then-green across Task 4 (keep both outputs).
3. Devnet sweep CSV meets the Task 6 acceptance predicate.
4. `grep -rn "max_transmit_size" crates/ --include='*.rs'` shows ONLY the
   config-wired path (no literal size at a gossipsub builder call site).

## Rollout ordering (mixed-fleet, explicit-peers care class)
1. Accept-gate raises (1 MB consensus, 8 MB direct read) ship in the next
   fleet build INCLUDING val3 — harmless immediately: nothing sends bigger
   yet, and old senders' messages pass trivially.
2. Send-side exploitation is FORBIDDEN until the fleet floor rises:
   - pushes > 4 MB only after every validator runs the 8 MB read cap — the
     clamp (Task 5) + ladder test enforce this mechanically; raising
     `LEGACY_FLEET_DIRECT_MSG_FLOOR` is the conscious act of re-declaring the
     floor once val3 + seed + val1 all carry the 8 MB build;
   - consensus messages > 256 KB only after every validator runs the 1 MB
     accept gate (else drop + author penalty from stragglers). Today nothing
     legitimately sends > ~350 KB worst-case, and that only under EVM load.
3. Threshold WAN A/B at testnet relaunch: seed-only env override (proposer-
   local, cannot split consensus), 150 s window per leg, then the compiled
   default flip cites those numbers. Current deployed env (6 MB) clamps to
   4 MB effective from this build forward (WARN in log at startup).

## Rollback
- Tasks 1-2: pure refactor + param threading; revert commits independently.
- Task 4: two one-line consts + comments; revert restores 256 KB/4 MB exactly
  (accept gates — no protocol state, no migration).
- Task 5: revert restores verbatim env honoring (and the silent violation).
- Task 6: env-only; nothing to roll back (devnet torn down after).
