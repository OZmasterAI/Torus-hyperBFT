# Implementation Plan: Native-Action HASH-ONLY Push (Phase 2.3 / #5)

**Design:** `docs/plans/native-da-hash-only-push.md` · **Created:** 2026-06-09
**Branch:** `fix/native-da-push-hardening` (continue)

## Design Decision: Option A — size-gated hash-manifest pre-push + receiver pre-warm pull
Big pre-proposal batches push a tiny `Vec<[u8;32]>` hash manifest (new
`PRE_PROPOSAL_HASHES_MARKER`); the receiver pulls the bodies by-hash from the proposer
to **pre-warm** the DA store before the `CompactBlock` arrives. Small batches keep the
full-body push (no bs100 regression). Reuses PushScheduler/T7/re-push (outbound), T5
serve-by-hash, T6 chunked fetch, T1 hot-pull absorb. **No consensus-format/state-root
change** — transport/timing only, so no coordinated-relaunch fork risk.

## Success Criteria
- A pre-proposal bundle whose `bincode(actions)` exceeds `HASH_ONLY_PUSH_THRESHOLD` is
  pushed as `[PRE_PROPOSAL_HASHES_MARKER] + bincode(Vec<[u8;32]>)`, not full bodies;
  smaller bundles keep the full-body push.
- A validator receiving the manifest issues chunked by-hash `NativeDaNetRequest`s to the
  proposer (`sender_vk`/`peer`), reusing T5 serve; pulled bodies are absorbed by the hot
  **local-retry** (no redundant hot fetch).
- bs-sweep: **bs500 keeps committed height advancing** (un-wedge); **bs100 ≈ 23,430 o/s**
  (no regression); **bs200 recovers** from the degraded 5,783 o/s.
- `cargo test -p torus-network -p torus-consensus` + `four_node_consensus` + bigbody green.

## Constants in play (verified)
- Markers (`swarm.rs:78-80`): `FORWARD_ACTION_MARKER=0xFE`, `PRE_PROPOSAL_BATCH_MARKER=0xFD`
  → new `PRE_PROPOSAL_HASHES_MARKER=0xFC`.
- `NATIVE_DA_FETCH_CHUNK=16` (`bridge.rs:83`, pub); `MAX_DIRECT_MSG_SIZE=4MB`,
  `MAX_NATIVE_DA_MSG_SIZE=8MB` (`codec.rs`).
- Hot budget (`app.rs:559-571`): `RECONSTRUCT_RETRIES=5`×`20ms` + `HOT_PULL_RETRIES=8`×`20ms`
  ≈ 260 ms ≪ 500 ms view timeout.
- `compute_action_hash(&SignedNativeAction)->B256` (`types/lib.rs:811`).

---

## Tasks

### Task 1: Size-gate + hashes marker (pure unit)
- **Test first** (`crates/torus-network/src/bridge.rs` tests):
  ```rust
  #[test]
  fn hash_only_gate_triggers_above_threshold() {
      assert!(!should_push_hashes_only(0));
      assert!(!should_push_hashes_only(HASH_ONLY_PUSH_THRESHOLD));
      assert!(should_push_hashes_only(HASH_ONLY_PUSH_THRESHOLD + 1));
      assert!(HASH_ONLY_PUSH_THRESHOLD < 4 * 1024 * 1024, "stay under the /torus/direct cap");
  }
  ```
  MUST fail today (no `should_push_hashes_only`/`HASH_ONLY_PUSH_THRESHOLD`).
- **Implementation:**
  - `swarm.rs:81`: `const PRE_PROPOSAL_HASHES_MARKER: u8 = 0xFC;`
  - `bridge.rs` (module-level):
    ```rust
    /// Pre-proposal pushes whose encoded body set exceeds this go out as a tiny HASH
    /// MANIFEST (peers pull the bodies) instead of the full multi-MB push that wedges the
    /// view at bs≈500 (Phase 2.3 / #5). Below it, the full-body push is the fast path (no
    /// pull RTT) — protecting the healthy bs100 baseline. Tunable; validated by bs-sweep.
    pub const HASH_ONLY_PUSH_THRESHOLD: usize = 512 * 1024; // 512 KB

    /// Whether a pre-proposal push of `encoded_len` bytes should ship hashes only.
    pub fn should_push_hashes_only(encoded_len: usize) -> bool {
        encoded_len > HASH_ONLY_PUSH_THRESHOLD
    }
    ```
- **Verify:** `cargo test -p torus-network hash_only_gate`
- **Depends on:** none

### Task 2: Outbound hash-manifest push (reuse PushScheduler fan)
- **Test first** (`bridge.rs` tests — pattern of `fetch_native_actions_chunks_per_validator`):
  ```rust
  #[test]
  fn broadcast_hashes_enqueues_hashes_command() {
      let (command_tx, mut rx) = mpsc::unbounded_channel();
      let net = LibP2PNetwork { command_tx, shared: shared_with_validators(&[]),
          native_inbound_rx: None, local_key: test_signing_key(1).verifying_key() };
      net.broadcast_native_action_hashes(vec![[7u8; 32], [9u8; 32]]);
      match rx.try_recv().unwrap() {
          NetworkCommand::BroadcastNativeActionHashes { hashes } =>
              assert_eq!(hashes, vec![[7u8; 32], [9u8; 32]]),
          _ => panic!("expected BroadcastNativeActionHashes"),
      }
  }
  ```
  MUST fail today (no such command/method).
- **Implementation:**
  - `swarm.rs`: add `BroadcastNativeActionHashes { hashes: Vec<[u8; 32]> }` to `NetworkCommand`
    (near `BroadcastNativeActions`, ~L65). Factor the existing push fan
    (`BroadcastNativeActions` body, `swarm.rs:1091-1174`: targets resolve + PushScheduler
    loop + `recent_native_bundles` retain + tracing) into
    `fn fan_native_push(swarm, shared, local_key, envelope: Vec<u8>)`. The
    `BroadcastNativeActions` arm becomes `fan_native_push(.., {let mut e=vec![PRE_PROPOSAL_BATCH_MARKER]; e.extend_from_slice(&payload); e})`;
    the new arm: `fan_native_push(.., {let mut e=vec![PRE_PROPOSAL_HASHES_MARKER]; e.extend_from_slice(&bincode::serialize(&hashes)?); e})`.
  - `bridge.rs`:
    ```rust
    /// Push only the action HASHES (manifest) for an oversized pre-proposal batch; peers
    /// pull the bodies by-hash (Phase 2.3 / #5). The body set is too big to disseminate
    /// within the view, so the full-body push would wedge it.
    pub fn broadcast_native_action_hashes(&self, hashes: Vec<[u8; 32]>) {
        let _ = self.command_tx.send(NetworkCommand::BroadcastNativeActionHashes { hashes });
    }
    ```
- **Verify:** `cargo test -p torus-network broadcast_hashes`
- **Depends on:** 1

### Task 3: Inbound hashes-marker → pre-warm pull from the proposer
- **Test first** (`swarm.rs` tests — pure decode/chunk helper):
  ```rust
  #[test]
  fn prewarm_requests_chunk_the_manifest() {
      let hashes: Vec<[u8; 32]> = (0..40u8).map(|i| [i; 32]).collect();
      let body = bincode::serialize(&hashes).unwrap();
      let chunks = plan_prewarm_requests(&body).unwrap();
      assert_eq!(chunks.len(), 40usize.div_ceil(NATIVE_DA_FETCH_CHUNK)); // 3
      assert!(chunks.iter().all(|c| c.len() <= NATIVE_DA_FETCH_CHUNK));
      assert_eq!(chunks.concat(), hashes);
      assert!(plan_prewarm_requests(b"\xff\x00garbage").is_none());
  }
  ```
  MUST fail today (no `plan_prewarm_requests`; unknown marker is penalized as malformed).
- **Implementation** (`swarm.rs`):
  - Helper:
    ```rust
    /// Decode a PRE_PROPOSAL_HASHES_MARKER manifest body (`bincode(Vec<[u8;32]>)`) into
    /// per-request hash chunks (≤ NATIVE_DA_FETCH_CHUNK) for the pre-warm pull. `None` on a
    /// malformed/empty manifest.
    fn plan_prewarm_requests(body: &[u8]) -> Option<Vec<Vec<[u8; 32]>>> {
        let hashes: Vec<[u8; 32]> = bincode::deserialize(body).ok()?;
        if hashes.is_empty() { return None; }
        Some(hashes.chunks(NATIVE_DA_FETCH_CHUNK).map(|c| c.to_vec()).collect())
    }
    ```
    (`use crate::bridge::NATIVE_DA_FETCH_CHUNK;`)
  - Inbound branch after the `PRE_PROPOSAL_BATCH_MARKER` arm (`swarm.rs:677`):
    ```rust
    } else if request.payload.first() == Some(&PRE_PROPOSAL_HASHES_MARKER) {
        // Phase 2.3 (#5): proposer pushed only the HASHES (body set too big to
        // disseminate in-view). PRE-WARM by pulling the bodies by-hash from the proposer
        // (this `peer` mirrored them) BEFORE its CompactBlock arrives, so the hot validate
        // path finds them present instead of wedging on dissemination.
        match plan_prewarm_requests(&request.payload[1..]) {
            Some(chunks) => {
                let n: usize = chunks.iter().map(|c| c.len()).sum();
                for chunk in chunks {
                    swarm.behaviour_mut().native_da
                        .send_request(&peer, NativeDaNetRequest { hashes: chunk });
                }
                tracing::info!(count = n, %peer, "pre-proposal HASH manifest -> pre-warm pull");
            }
            None => warn!(%peer, "pre-proposal hash manifest decode failed"),
        }
    }
    ```
- **Verify:** `cargo test -p torus-network prewarm_requests`
- **Depends on:** 1

### Task 4: Absorb pre-warmed bodies during the hot local-retry (no redundant fetch)
- **Test first** (`crates/torus-consensus/src/app.rs` tests): a `NativeDaFetcher` double
  whose `drain()` returns pre-queued bodies and whose `fetch()` increments a counter.
  Drive `reconstruct_native_actions_hot(&[h])`; assert `Ok` AND `fetch_count == 0` (the
  pre-warmed body is absorbed by the local retry, no network fetch). MUST fail today (the
  local-retry loop never absorbs; recovery only happens via the hot-pull which calls fetch).
- **Implementation** (`app.rs:954-968`, the local-retry loop in `reconstruct_native_actions_hot`):
  add an absorb-drain each iteration:
  ```rust
  if !missing.is_empty() {
      for _ in 0..RECONSTRUCT_RETRIES {
          std::thread::sleep(RECONSTRUCT_RETRY_DELAY);
          // Phase 2.3 pre-warm (#5): a hash-only push made THIS node pull the bodies
          // out-of-band; they land in the fetcher inbound. Absorb them into the DA store
          // here so the fast local retry picks up a pre-warmed body — no redundant hot
          // network fetch in the common big-batch case.
          if let Some(ref fetcher) = self.da_fetcher {
              Self::absorb_fetched_bodies(mempool, fetcher.as_ref());
          }
          missing.retain(|&i| match mempool.get_native_da(&hashes[i]) {
              Some(action) => { actions[i] = Some(action); false }
              None => true,
          });
          if missing.is_empty() { break; }
      }
  }
  ```
- **Verify:** `cargo test -p torus-consensus reconstruct`
- **Depends on:** none (independent; but its value is realized with 1-3,5)

### Task 5: Wire the size-gate into the pre-proposal glue
- **Test first:** decision logic is already unit-covered (Task 1). The glue is
  integration-level — validate by build + the E2E bs-sweep (Task 6). No new unit.
- **Implementation** (`crates/torus-node/src/main.rs:445-451`):
  ```rust
  std::thread::spawn(move || {
      while let Ok(bundle) = pre_proposal_rx.recv() {
          let Ok(payload) = bincode::serialize(&bundle.actions) else { continue };
          if torus_network::should_push_hashes_only(payload.len()) {
              // Phase 2.3 (#5): body set too big to disseminate in-view — push only the
              // HASHES; validators pull the bodies (pre-warm). Un-wedges bs≈500.
              let hashes: Vec<[u8; 32]> = bundle.actions.iter()
                  .map(|(_, a)| torus_types::compute_action_hash(a).0).collect();
              network_for_pre_proposal.broadcast_native_action_hashes(hashes);
          } else {
              network_for_pre_proposal.broadcast_native_actions(payload);
          }
      }
  });
  ```
  Re-export from `crates/torus-network/src/lib.rs`: `pub use bridge::{should_push_hashes_only, HASH_ONLY_PUSH_THRESHOLD};` (if not already surfaced).
- **Verify:** `cargo build -p torus-node`
- **Depends on:** 1, 2

### Task 6: Regression gate + bs-sweep un-wedge proof
- **Verify:**
  `cargo test -p torus-network -p torus-consensus -p torus-mempool` &&
  `cargo test -p torus-consensus four_node_consensus`; rebuild release; bs-sweep
  (local-devnet or coordinated live):
  - **bs500 keeps committed height advancing** (no permanent wedge) — the acceptance proof.
  - **bs100 ≈ 23,430 o/s @ 289 ms** — no regression (the gate must NOT engage here; if
    bs100's bundle ≥ `HASH_ONLY_PUSH_THRESHOLD`, RAISE the threshold).
  - **bs200 recovers** from 5,783 o/s — if the gate doesn't engage at bs200, LOWER the
    threshold. (Tune the threshold against measured bundle sizes — design Open Q#1/#4.)
- **Depends on:** 1, 2, 3, 4, 5

---

## Verification (end-to-end)
Rebuild release; run the bs-sweep on the live testnet (or a local 3-val devnet on a
THROWAWAY genesis — never wedge the live chain mid-test). Confirm bs500 advances, bs100
holds 23.4k o/s, bs200 recovers. Capture orders/s + block-time per batch_size to a memory.

## Rollback
All changes are additive (a new marker, a new command/method, a size gate, an absorb-drain
in the hot retry). Bodies stay content-addressed by hash — no consensus-format/state-root
change, so a revert needs no coordinated relaunch. To disable without reverting: set
`HASH_ONLY_PUSH_THRESHOLD` above any realistic bundle size (gate never engages → today's
full-body push for all batches).
