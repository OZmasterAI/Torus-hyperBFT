# Native-Action DA Layer — /implement prompt

Copy-paste this into a fresh (clear-context) session to implement the DA-layer plan.
Plan: `docs/plans/native-action-da-impl.md` · PRP: `PRPs/native-action-da.tasks.json` · Design: `docs/plans/native-action-da.md`
Alternative (orchestrated): replace the first line with `torus-loop.sh native-action-da`.

---

/implement docs/plans/native-action-da-impl.md

GOAL: Implement the Native-Action Data-Availability (DA) layer (Option B) for Torus-hyperBFT — reliable out-of-band native-action body delivery so the chain can reach 400k+ orders/sec without the consensus livelock. This is CONSENSUS-CRITICAL: correctness and determinism over speed.

READ FIRST (in this order), then follow THE LOOP (memory → tests → implement → prove → review → commit):
1. Plan:            docs/plans/native-action-da-impl.md   (9 TDD tasks, success criteria, rollback)
2. PRP / tasks:     PRPs/native-action-da.tasks.json      (per-task files + `validate` commands + deps)
3. Design + why:    docs/plans/native-action-da.md        (Option B chosen; A/C rejected; full history)
4. Memory — run: run_tool("memory","search_knowledge",{"query":"native action data availability dissemination livelock"})
   then get_memory each of:
   - 28e1a821  livelock ROOT CAUSE (the exact bug you're fixing)
   - 531f6061  this implementation plan
   - a6cf33a9  dissemination history — per-block pull regressed 46→211ms (pull is FALLBACK-ONLY)
   - c19a35aa  s297 Option D decision (push-primary / fetch-rare)
   - 0efcef9d  CRITICAL: linear fast-path in validate_block is LOAD-BEARING — do NOT remove

NON-NEGOTIABLE CONSTRAINTS:
- TDD: write the FAILING test first for each task; run that task's `validate` command from the PRP; only proceed when green. Never claim done without pasting test output.
- Determinism: consensus block identity = data_hash over the datum bytes (app.rs:923). Compact vs full = DIFFERENT consensus hash → re-enabling compact (Task 8) is version-gated and requires all validators on the same encoding. Do NOT change the EVM/RPC header hash (keccak256(canonical_header_bytes), app.rs:153).
- Pull is a RARE fallback, NEVER per-block (history proves per-block fetch regresses latency). Push stays primary.
- The DA store must BYPASS the 60s nonce gate (NONCE_WINDOW_MS, eip712.rs:27) for block-referenced bodies, but KEEP that gate on RPC mempool admission (don't open a spam vector).
- Do NOT remove the linear fast-path in validate_block (mem 0efcef9d). Keep `four_node_consensus` + the full `cargo test --workspace` suite green.
- Reuse the existing /torus/block-data/1.0 request_response (codec.rs + swarm.rs ~620-683) as the template for /torus/native-da/1.0 — no hotstuff_rs protocol changes.

SEQUENCING:
- Create branch `phase-c-native-da` off `cap100-3val-perf` FIRST; confirm a green baseline (`cargo build --release -p torus-node`).
- Implement Tasks 1→4 FIRST (durable store + reconstruct-from-store + remove blacklist/freeze footguns) — the "un-wedge" increment. The Task 3 reproduction test must go red→green.
- Then 5→8 (native-da protocol → rare pull-fallback → hardened push → re-enable compact, version-gated). Task 9 is the scale milestone (bench toward 400k).
- Commit per task (or per logical increment) with a clear message; end each commit message with the Co-Authored-By footer. Do NOT push.

QUALITY BAR:
- Real, surgical code matching surrounding style; no placeholders/pseudocode; minimal blast radius in consensus paths.
- Update existing tests for changed behavior (e.g. produce_block_datum_is_full_self_contained_block → compact in Task 8).
- After each task, paste the `validate` command output as proof of green.

GUARDRAILS:
- Do NOT touch the running testnet seed node (PID /tmp/torus-seednode.pid) or testnet/data — the live wedged chain needs a separate coordinated restart, out of scope here.
- Do NOT commit the pre-existing unrelated changes (devnet/docker-compose.yml, devnet/scripts/__pycache__).
- Ask before any push, deploy, or other irreversible action.

Start by reading the three docs + the listed memories, then create the branch and begin Task 1.
