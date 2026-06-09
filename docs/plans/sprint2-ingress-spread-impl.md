# Implementation Plan: Sprint 2 — Ingress Spread

**Design:** docs/plans/sprint2-ingress-spread.md (Option A)

## Success Criteria
- `torus_submitNativeActions` accepts up to 100 hex payloads in one call, returns
  per-item `{hash}`/`{error}` in order; one permit per call; old endpoint unchanged.
- Bench can drive it (`--submit-batch N`) with split key ranges (`--sender-offset K`).
- Dual-box live proof: bench on our box → localhost + bench on smallserver →
  friend1 localhost, simultaneously; submitted actions/s ≥ 3× the single-tunnel
  baseline; chain healthy; orders/s from ONE instance's sweep.

## Tasks

### T1: Batch endpoint (torus-rpc)
- **Test first** (`lib.rs` tests, `start_server` harness): POST
  `torus_submitNativeActions` with [valid signed action (HARDHAT key 0, ms nonce),
  valid (key 1), garbage hex]; assert 3 results in order: two `hash` entries, one
  `error`; assert old single endpoint still works.
- **Implementation** (`torus.rs`):
  - trait: `#[method(name = "submitNativeActions")] async fn
    submit_native_actions(&self, signed_actions: Vec<String>) ->
    RpcResult<Vec<RpcSubmitResult>>;` (`types.rs`: `RpcSubmitResult { hash:
    Option<String>, error: Option<String> }`).
  - const `SUBMIT_BATCH_MAX: usize = 100` (lib.rs, near SUBMIT_PERMITS); reject
    larger batches with InvalidParams.
  - impl: one `acquire_submit_permit`; ONE `spawn_blocking` that loops items:
    parse → `validate_batch_size` → `validate_with_sessions` → per-item
    Ok((sender, action, bytes, hash)) | Err(msg); then per-item
    `add_native_action_presigned` + leader-forward (reuse the single-path tail,
    factored into `fn admit_and_forward(...)`).
- **Verify:** `cargo test -p torus-rpc --lib`

### T2: Bench support (tools/bench-throughput)
- `--submit-batch N` (default 1 = old endpoint): sender task builds N signed
  actions, POSTs one `torus_submitNativeActions`, counts per-item oks into
  `submitted`.
- `--sender-offset K` (default 0): use `HARDHAT_KEYS[K..K+senders]`; assert
  K+senders ≤ 20 (funded range).
- **Verify:** `cargo test -p bench-throughput`; manual `--help`.

### T3: Prove on live testnet (dual-box)
- Deploy new binary (ours + friend1, systemctl/scp as before).
- Simultaneous runs (30s, bs500): our box `--rpc-urls http://localhost:8545
  --senders 10 --sender-offset 0 --submit-batch 10`; smallserver same against its
  localhost with `--sender-offset 10`.
- Pass bar: combined submit rate ≥ 3× single-tunnel baseline (~150+ act/s);
  included orders/s (one instance's sweep) > 30k; no stall (height advances
  within 60s of load end without intervention).

## Rollback
Endpoint is additive; revert commit + redeploy. Bench flags default to old behavior.
