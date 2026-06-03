<!--
Step 3 (Option D — Native-Action Dissemination Hardening) implementation prompt.
Created session 297 (2026-06-03), branch cap100-3val-perf.
Purpose: paste the block below into a FRESH Claude Code session on this repo to
implement the plan with high fidelity. It loads the decision trail from memory +
docs instead of re-deriving, hard-codes the regression guardrails, makes Task 1 a
triage gate, and demands proof over claims.
Plan: docs/plans/step3-dissemination-hardening-impl.md + PRPs/step3-dissemination-hardening.tasks.json
Design: docs/plans/step3-dissemination-hardening.md
-->

# Step 3 — Dissemination Hardening (Option D): Implementation Prompt

Paste everything inside the fence into a fresh session on `cap100-3val-perf`.

```
Implement Step 3 — Native-Action Dissemination Hardening (Option D) — on branch
cap100-3val-perf of the Torus-hyperBFT repo. The brainstorm + TDD writing-plan are
DONE; your job is to implement them. Do NOT re-derive the design — load it first.

═══ STEP 0: LOAD CONTEXT (before any code) ═══
1. Read the plan + rationale:
   - docs/plans/step3-dissemination-hardening-impl.md   (the 5-task TDD plan)
   - docs/plans/step3-dissemination-hardening.md         (design + why D over A/B/C/E)
   - PRPs/step3-dissemination-hardening.tasks.json       (orchestrator tasks)
2. Pull the decision trail from memory (read these IDs — don't rediscover):
   97dcf42b (plan) · c19a35aa (decision: D locked) · 75ef1599 (why gossip is OFF) ·
   a6cf33a9 (delivery history) · 56c65c25 (root-cause brainstorm) · 07d92e4f (live
   degraded-state finding). Then: search_knowledge "step3 dissemination Option D
   unicast delivery cap100-3val-perf".
3. Confirm `git branch` = cap100-3val-perf. Do NOT work on main. Do NOT rebuild or
   deploy the live testnet — it runs the OLD 59e595e (cap-1000) binary, not this branch.

═══ HARD GUARDRAILS (each encodes a hard-won regression — violating them breaks the chain) ═══
✗ DO NOT re-enable native-action gossip (native_gossip_enabled, mempool/lib.rs:116).
  It was disabled on purpose in 66ddef4 — it floods GossipSub and drowns consensus.
✗ DO NOT remove the 100ms retry in validate_block (app.rs:941-959). On 3-validator
  all-3 quorum we can't tolerate a rejection; the "remove retry" note (mem 8ee99db3)
  assumed a 4-validator set we DON'T have.
✗ DO NOT revert to full-block proposals. The missing-action stall is a DELIVERY bug,
  not a CompactBlock flaw; CompactBlock is deliberate prep for the deferred cap raise.
✗ DO NOT make a per-block pull-fetch the PRIMARY path — that round-trip caused the
  46→211ms regression (06620a1). Harden the push so any fetch stays a rare exception.

═══ TASK 1 IS A TRIAGE GATE — do it first, then STOP and report ═══
Task 1 is READ-ONLY (no code). Triage both the delivery seams AND the block-sync
failures seen live in testnet/node.log:
  - "block_sync: worker fetch error (timeout/disconnect)"
  - "dropping proposal header ... justify_block_known=false" (missing parent block)
  - views turning over ~500-600ms WITHOUT committing.
Determine: (a) reconnect trigger — ConnectionEstablished vs Identify (swarm.rs:538);
(b) how request_response OutboundFailure is handled; (c) congestion hypothesis vs
current gossip-off code; (d) why block-sync fetch fails / parents go missing (cf. mem
a06daf38 body_fetch_tracker, hotstuff_rs/src/block_sync/client.rs).
DELIVERABLE: write findings into the "## Task 1 findings" section of the impl doc, and
give an explicit verdict — "Is Option D's delivery hardening SUFFICIENT, or is a
companion block-sync fix needed?" — then PAUSE and report before building Tasks 2-5.
If block-sync is the dominant cause, propose a scope change; do not blindly proceed.

═══ TASKS 2-5: TDD-STRICT (failing test first, show it fail, then pass) ═══
T2  PendingSendQueue<T> — pure, bounded, generic struct + full unit tests
    (new crates/torus-network/src/pending_send.rs). Fully unit-testable (u32 stand-in).
T3  Enqueue-on-miss / flush-on-register — fixes the consensus unicast drop at
    swarm.rs:749. Add pending_sends to SharedState (swarm.rs:69) + test_shared();
    extract a send_direct() helper reused by Send and RegisterPeer.
    NOTE: handle_command has no Swarm test seam → wiring is integration-verified.
T4  Re-push recent native-action bundle ring on (re)registration — fixes the
    fire-and-forget missing-action push at swarm.rs:794-810.
T5  Metrics + QUIET-HOST devnet flood verification. MEASURE whether event-loop
    congestion (mem 8ee99db3) still dominates; if it does, Step 3 is NOT done — say so.

═══ ENTRY-POINT MAP (already mapped — don't re-explore) ═══
crates/torus-network/src/swarm.rs : SharedState@69 · NetworkCommand@24-62 ·
  Send(drop@749)@731-751 · RegisterPeer@752-755 · BroadcastNativeActions@794-810 ·
  consensus proposal publish@718 · enqueue_inbound@814 (bounded-drop pattern;
  MAX_INBOUND_QUEUE@815) · Identify handler@538 · BlockDataRequest@768 ·
  tests + test_shared()/test_vk()@913-998
crates/torus-network/src/peer.rs  : PeerMap@8 (insert@14, get_peer_id@41, peer_ids@53)
crates/torus-node/src/main.rs     : native-inbound task@403-413 · pre-proposal bridge@417-427
crates/torus-consensus/src/app.rs : produce_block push@851-859 · CompactBlock
  serialize@861-862 · pending_proposals@863 · validate_block@877 (TorusBlock-first
  deser@901, CompactBlock fallback@909, retry@941-959, reject@962)
crates/torus-mempool/src/lib.rs   : native_gossip_enabled@116 (KEEP false) · dead
  setter@121 · gate@296 · add_native_action_from_gossip_trusted (dedups DuplicateNativeAction)
crates/hotstuff_rs/src/block_sync/client.rs : block-sync fetch (Task 1d triage)
testnet/genesis.json : timeout_base_ms=500@8 (view timeout) · 3 validators@47-65
crates/hotstuff_rs/src/types/validator_set.rs : quorum floor(n*2/3)+1 @157-170 (3-val = all-3)

═══ VERIFICATION — prove it, never claim it ═══
- cargo test -p torus-network -p torus-consensus  → must be green (paste output).
- cargo build --release -p torus-node BEFORE any devnet run (fast Dockerfile copies host binary).
- T5 bench on a QUIET host (pause torus-web; do NOT co-run the live testnet node + 5
  devnet nodes — prior benches were confounded by load-34-on-8-cores).
  Run: bench-throughput consensus --senders 100 --duration 30  (cap-100).
  Capture: missing-action rejection rate, included/sec, block-time p50/p99,
  reconnect-recovery time.
  PASS BAR (proposed, adjust if Task 1 says otherwise): ≥120 native orders/sec
  sustained, <1% missing-action rejections, block-time p99 < 450ms, no view stalls.

═══ WORKING RULES ═══
Follow the CLAUDE.md LOOP (memory → tests → implement → prove → review → commit).
Save every decision/fix/correction to memory (≤800 chars). Don't go down rabbit holes:
if you hit an unexpected root cause, report it and ask before changing scope. Verify
before asserting. Commit per-task on cap100-3val-perf only; run the wrap-up skill at the end.
```

## Optional: orchestrated execution after the gate
After the Task 1 gate verdict is approved, add to the prompt:
`After the Task 1 gate is approved, use torus-loop.sh step3-dissemination-hardening for orchestrated per-task execution.`
