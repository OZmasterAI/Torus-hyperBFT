# Implementation Plan: Body-durability invariant (S459 wedge fix)

## Design Decision
Option A from `docs/plans/wedge-durability-fix.md`: harden the *existing* implicit
"a vote implies the body is durable locally" gate — fail-closed — rather than add
a quorum barrier (B, throughput cost) or fsync (C, proven unnecessary: `da_store`
writes go through RocksDB WAL, so a SIGKILL/`docker restart -t 0` cannot lose a
*successfully written* body — only a body that was never durably written).

## Invariant
`validate_block` returning `Valid` (→ the node casts a PhaseVote) MUST imply every
native-action body referenced by the block is durably in THIS node's `da_store`.
`produce_block` MUST NOT propose a block referencing a body it did not durably
store. Then a QC (2f+1 votes) ⇒ ≥ f+1 honest nodes hold every body durably ⇒ no
permanent commit-hole.

## Root-cause facts (verified, file:line)
- Compact voter path is ALREADY safe: `reconstruct_native_actions_hot`
  (`app.rs:2120`) returns `Ok` only when every body is read from the durable
  `da_store` via `get_native_da` (`mempool/lib.rs:638`), else `Err` →
  `MissingData` (`app.rs:2604`). Bodies come from the durable store, not RAM.
- **Hole 1 (primary): legacy inline path.** `validate_block` TorusBlock branch
  (`app.rs:2557-2564`) uses `block.native_actions` inline and returns without
  ever calling `mirror_native_to_da` — votes without persisting to `da_store`.
- **Hole 2: leader proposer-guarantee is silent.** `mirror_native_to_da`
  (`mempool/lib.rs:654-658`) swallows a `put_batch` failure — no re-queue, no
  propagation (unlike `mirror_to_da`/`flush_da_mirrors`, which re-queue). Leader
  proposes even if the durable write failed.
- `put_batch` → `db.write(batch)` default WAL opts (`native_da.rs:85-97`) → SIGKILL-durable.

## Success Criteria
1. New unit test: a legacy full-`TorusBlock` proposal, whose bodies are NOT
   pre-loaded, after `validate_block` → bodies are present in `da_store`
   (`get_native_da` returns them). FAILS on current code, passes after Task 2.
2. `mirror_native_to_da` returns `Result` and re-queues on failure (parity with
   the other two mirror paths); all callers handle it.
3. `produce_block` proposes without native actions (not a wedge-able block) if it
   cannot durably store them; a metric counts it.
4. Deterministic end-to-end regression via `torus-wedge-inject` still recovers,
   and the ORGANIC gossip-off recipe (`wedge-repro-s459.sh`) goes 0/N wedges with
   the fix pulled (was ~1/3).
5. No change to blockspeed / orders-s / native-actions-per-block on the happy path
   (all changes are error-path or the off-default legacy branch).
6. Liveness: a transient `put_batch` error must NOT permanently stall a node — the
   re-queue retries; a voter returns `MissingData` (re-proposed next view), a
   leader proposes empty-native that view. No node drops out on a blip.

## Tasks

### Task 1: `mirror_native_to_da` → fallible + re-queue (foundation)
- **Test first** (`crates/torus-mempool/src/lib.rs` tests): assert the signature
  is `Result<(), StateError>` and returns `Ok(())` on a healthy temp-db mirror;
  assert `da_flush_failures` is unchanged on success. (Failure-path is covered
  end-to-end by Task 4; forcing a RocksDB write error in a unit test is not worth
  the fault-injection scaffolding.)
- **Implementation** (`mempool/lib.rs:654-658`):
  ```rust
  pub fn mirror_native_to_da(&self, actions: &[SignedNativeAction]) -> Result<(), StateError> {
      if let Err(e) = self.da_store.put_batch(actions) {
          tracing::error!("native DA store batch write failed: {e}");
          // S459: parity with flush_da_mirrors/mirror_to_da — never silently drop a
          // proposer/validator durability write. Re-queue for retry AND surface the
          // error so callers fail-closed instead of voting/proposing an unbacked body.
          self.requeue_failed_da_batch(actions.to_vec());
          return Err(e);
      }
      Ok(())
  }
  ```
  Adjust the two current callers to the new signature:
  - `app.rs:2222` (`absorb_fetched_bodies`): `let _ = mempool.mirror_native_to_da(&actions);`
    (the subsequent `get_native_da` recheck already fails-closed on a miss).
  - `app.rs:2431` (`produce_block`): handled in Task 3.
- **Verify**: `cargo build -p torus-mempool && cargo test -p torus-mempool`
- **Depends on**: none

### Task 2: Legacy `TorusBlock` voter path mirrors bodies before `Valid` (PRIMARY FIX)
- **Test first** (`crates/torus-consensus/src/app.rs` tests, or an integration
  test): build a full-inline `TorusBlock` proposal carrying one native action
  whose body is NOT pre-put into `da_store`; call `validate_block`; assert
  (a) response is `Valid`, and (b) `mempool.get_native_da(hash)` now returns the
  body. On current code (b) FAILS (legacy path never mirrors).
- **Implementation** (`app.rs:2557-2564`, inside the `TorusBlock` branch, before
  `block` is yielded):
  ```rust
  let torus_block = if let Ok(block) = bincode::deserialize::<TorusBlock>(datum_bytes) {
      tracing::info!(/* unchanged */);
      // S459 wedge fix: the compact path guarantees every referenced body is
      // durable locally (reconstruct_native_actions_hot). A full-inline TorusBlock
      // must meet the SAME bar before we vote, or a later restart/backfill can't
      // reconstruct it. Fail-closed to MissingData (re-proposed next view) if the
      // durable mirror fails.
      if !block.native_actions.is_empty() {
          if let Some(ref mempool) = self.mempool {
              if let Err(e) = mempool.mirror_native_to_da(&block.native_actions) {
                  tracing::warn!(%e, height = block.header.height,
                      "validate_block: TorusBlock body mirror failed — MissingData");
                  return ValidateBlockResponse::MissingData;
              }
          }
      }
      block
  } else if let Ok(compact) = bincode::deserialize::<CompactBlock>(datum_bytes) {
      /* unchanged */
  ```
- **Verify**: `cargo test -p torus-consensus validate_block_legacy_durable`
- **Depends on**: [1]

### Task 3: `produce_block` fail-closed — never propose an unbacked body
- **Test first**: assert the happy-path invariant — after `produce_block`, every
  native action in the produced (compact) proposal is present in `da_store`.
  (Holds today; guards against regressing the mirror out of the hot path.)
- **Implementation** (`app.rs:2427-2432`): thread the new `Result` and, on
  failure, exclude native actions from THIS proposal:
  ```rust
  let native_actions = if let Some(ref mempool) = self.mempool {
      match mempool.mirror_native_to_da(&native_actions) {
          Ok(()) => native_actions,
          Err(e) => {
              // S459: proposer guarantee enforced. Can't durably store ⇒ don't
              // reference them (a validator could never reconstruct → wedge).
              // Propose empty-native this view; the re-queued batch retries.
              tracing::error!(%e, count = native_actions.len(),
                  "produce_block: durable body mirror FAILED — proposing without native actions");
              if let Some(ref m) = self.metrics { m.proposer_body_mirror_failures.inc(); }
              Vec::new()
          }
      }
  } else { native_actions };
  ```
  VERIFY the downstream encode path (sig_attestation `app.rs:2435`, compact-block
  build) consumes this (possibly reduced) `native_actions` and the paired
  `native_with_senders` set — drop the corresponding senders so the two stay
  consistent. Add the `proposer_body_mirror_failures` counter next to
  `missing_action_rejections` in the metrics struct.
- **Verify**: `cargo test -p torus-consensus produce_block_bodies_durable`
- **Depends on**: [1]

### Task 4: End-to-end regression — deterministic + organic
- **Test first**: `PROVE_UNWEDGE=1 SKIP_BUILD=1 bash devnet/wedge-repro-s459.sh`
  (injection mode) still proves inject→wedge→unwedge (fix must not break recovery).
- **Then**: with the fix built, run the ORGANIC gossip-off recipe (friend2's loop,
  `TORUS_NATIVE_GOSSIP=false` + single-node DA-holder restart) N=30 and assert
  0 permanent wedges (baseline was ~1/3). This is the real proof the formation
  path is closed.
- **Verify**: both harness runs; capture counts in `devnet/`.
- **Depends on**: [2, 3]

## Verification (end-to-end)
- `cargo build --release -p torus-node -p torus-mempool -p torus-consensus`
- `cargo test -p torus-mempool -p torus-consensus`
- Injection harness recovers; organic gossip-off harness 0/30.
- Reconcile with friend2's captured timeline: confirm the observed hole is Hole 1
  (legacy path) and/or Hole 2 (leader swallow). If the capture shows a DISTINCT
  path (e.g. a reconstruct race that returns without persisting), add a targeted
  Task 2b before shipping. **Do not ship until the capture is reconciled.**
- Bench parity (no regression): a short `bench-throughput consensus` A/B (fix vs
  `1e2fe8c5`) on devnet — blockspeed / orders-s / native-actions-per-block within noise.

## Rollback
Single-branch change; `git revert` the commit. No genesis/state/schema change, no
wire-format change (durability is local-store only), so a rolled-back node is
compatible with fixed peers. Backups `data.wedged-761993-s459` remain as fixtures.
