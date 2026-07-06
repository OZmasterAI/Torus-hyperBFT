# BS-4a — DA reconstruct off-thread (coordinator prompt, Fable 5)

Copy-paste everything below the `---` into a fresh session. Written for Fable 5
acting as COORDINATOR with opus/fable subagents. Plan:
`docs/plans/bs4a-da-reconstruct-offthread-impl.md` · PRP:
`PRPs/bs4a-da-reconstruct-offthread.tasks.json` · Design:
`docs/plans/bs4a-da-reconstruct-offthread.md`. Authored S421 (2026-07-06).

---

You are the COORDINATOR for implementing BS-4a (+BS-4b piggyback). You do not
write the production code yourself — you brief builder subagents, independently
verify everything they claim, run adversarial reviews, and own every commit.
Optimize for implementation quality over speed. There is no time pressure;
never skip a verification step to save wall-clock.

## MISSION

Stop `reconstruct_native_actions_hot` from blocking the single hotstuff
consensus thread for up to ~260ms on a native-DA body miss (100ms local retry +
160ms in-line hot pull vs the 500ms view timeout). Approved design (Option B,
mem 7efe7062188cf8d0): fail the view fast (MissingData after ONE ≤20ms
wake-on-arrival slice) and hand missing hashes to a dedicated `DaRecoveryWorker`
thread running the pull loop off-thread with a ~1s event-driven budget, so the
re-proposed view finds the bodies durably local. Consensus-thread worst case
~260ms → ~20ms. Targets the mechanism implicated in the S419 O2 death spiral
(mem 7d140b09d2f0ad37, c8308bdbdcac27e4).

SCOPE GUARANTEE: per-node liveness policy ONLY — no consensus-validity, wire,
or persistence change. NO lockstep. NOT part of the pinned relaunch binary
(fleet pin = 6e03294).

## SETUP (coordinator, before any delegation)

1. `git switch -c perf/bs4a-da-recovery 7c122f7` — local sprint head; code
   IDENTICAL to fleet-pin 6e03294 (the two commits above are doc-only
   auto-commits that contain the plan files). Verify `git log --oneline -3`
   shows 7c122f7 / 5d8b1a8 / 6e03294. Never touch or push
   `sprint/blockspeed-orders-s395`.
2. Read IN FULL: the plan (-impl.md), the PRP json, the design doc.
3. Memory: run_tool("memory","search_knowledge",{"query":"BS-4a DA recovery
   worker reconstruct off-thread MissingData hot path"}), then get_memory each
   (FULL ids): 7efe7062188cf8d0 (design verdict + key insight: fetch() is
   already non-blocking — only the wait loops block), f09d43b16a3adef2 (plan),
   7d140b09d2f0ad37 (S419 root cause), df0a895bff62b4e0 (auto-commit hijack),
   843092a5764de1fb (S420 suite baseline).
4. Read crates/torus-consensus/src/app.rs landmarks YOURSELF (lines may have
   drifted — verify before briefing anyone): reconstruct_native_actions_hot
   :1124 (local retry :1148-1188, in-line pull :1190-1205),
   pull_missing_bodies_bounded :1041, NativeDaFetcher trait :633, da_fetcher
   field :614, set_native_da_fetcher :852, consts :699-714, tests :2883/:2926/
   :2973/:3006. Also skim torus_state::BackgroundCfWriter (O3 precedent,
   commit 1a51d1e) and crates/torus-integration-tests/tests/native_da_pull_fallback.rs.
5. Confirm a green baseline: `cargo test -p torus-consensus` (hot_path*, pull_*,
   prewarm* modules). Paste the output. If baseline is red, STOP and diagnose
   before any delegation.

## ORCHESTRATION PROTOCOL

**Serialize implementation. Parallelize verification.** Tasks 1-7 nearly all
edit the same file (app.rs); concurrent builders would collide — run ONE
builder at a time, in dependency order (1 and 2 first — order between them is
yours; 3 after 2; 4 after 1+3; 5,6 after 4; 7 after 2, any time; 8 last).
Reviews are read-only and MUST fan out in parallel.

**Model policy (hard rule):** subagents are opus (4.8) or fable ONLY — Sonnet
is FORBIDDEN (user preference). Assignments:
- fable builders — Task 3 (worker lifecycle/concurrency) and Task 4 (the hot-path
  behavior change). These are the two places a subtle bug ships a consensus
  stall or a shutdown hang.
- opus builders — Tasks 1, 2, 5, 6, 7 (well-specified test writing, mechanical
  refactor, telemetry).
- opus reviewers — all adversarial review panels.
- fable reviewer — the final whole-branch review (step FINAL below).

**Per-task loop (repeat for each task):**
1. BRIEF: spawn the builder with a self-contained prompt: the task's section
   from the plan verbatim (including exact test code where given), the current
   (re-verified) line landmarks, the contract it must not break (below), and
   the instruction that its final message must contain: the full diff, the
   validate command output, and any deviation from the plan with justification.
   Builders NEVER commit, push, run git state-changing commands, or touch
   anything outside crates/torus-consensus + crates/torus-telemetry.
2. VERIFY (trust nothing): re-run the task's PRP validate command YOURSELF and
   read the diff YOURSELF. For Task 1 the test MUST fail red first — paste the
   red evidence; a test that passes immediately means it tests nothing: reject
   and re-brief. For Task 2 run the sync pull tests before AND after and diff
   the behavior surface (pure refactor = zero test churn).
3. REVIEW (behavioral tasks 2, 3, 4, 7): spawn TWO opus reviewers IN PARALLEL,
   read-only, each with a distinct lens, briefed to REFUTE the change:
   - Lens A concurrency/lifecycle: deadlocks, drop ordering, notifier races,
     panic paths, thread leaks across tests.
   - Lens B contract regression: the NON-NEGOTIABLES below, test-weakening,
     accidental sync-path behavior change, missed callers.
   Triage their findings yourself (they will overcall — verify each claimed
   defect in the code before acting). Apply real fixes via the same builder
   (SendMessage to continue it) or a small edit yourself if trivial; re-run
   validate after any fix.
4. COMMIT (coordinator only): ONE Bash call, single `git add <files> && git
   commit -m "..."` — never chain commits across calls (auto-commit hook
   hijacks multi-commit chains into "auto:" commits, mem df0a895b — bit S421
   twice). Message ends with the Co-Authored-By footer. If the hook still
   produces an "auto:" commit, recover per the gotcha (git reset --mixed to
   base, ONE add+commit) before proceeding.
5. Record progress: update the task's status in
   PRPs/bs4a-da-reconstruct-offthread.tasks.json and remember_this() any
   surprise/deviation immediately (don't batch to session end).

**Concurrency checklist — inject into Task 3 + 4 briefs AND both reviewer
lenses (this is where this feature can rot):**
- Drop must terminate: Drop takes tx (drop it) THEN joins; worker recv() must
  error out promptly; an in-flight batch is deadline-bounded ≤1s so join is
  bounded. No join while tx still alive anywhere (deadlock).
- submit() after worker death must not panic — send Err is logged, not
  unwrapped.
- Worker spawn ONLY when both mempool and fetcher exist; set_native_da_fetcher
  called twice must not leak the first worker (drop replaces it).
- NativeDaStore::arrival_generation/wait_for_arrival are process-global — the
  worker's waits must not starve or self-wake the hot path's single slice
  (re-snapshot generation after own absorbs, as the existing loops do).
- Worker panic must not take the node down or wedge Drop (join handles the
  Err; log it).
- Tests: each test's TorusApp spawns a worker thread — assert no cross-test
  interference (named thread helps debugging; drop app before asserting
  store state where relevant).

## TASKS (full detail + exact test code lives in the plan — the plan is the
spec; this list is only the map)

1. RED: hot_path_hands_off_and_recovers_in_background — Err within 80ms + body
   recovered in background within 2s. MUST fail before implementation exists.
2. Extract free fn recover_bodies_bounded(mempool, fetcher, missing, retries,
   delay, metrics) from pull_missing_bodies_bounded; TorusApp delegates. Pure
   refactor.
3. DaRecoveryWorker: named thread "torus-da-recovery", mpsc channel,
   store-check dedup per batch, Drop = drop tx + join. Unit test
   recovery_worker_recovers_late_body_off_thread.
4. Wire worker (spawn in set_native_da_fetcher), shrink local retry
   RECONSTRUCT_RETRIES 5→1, DELETE HOT_PULL_RETRIES/HOT_PULL_DELAY, ADD
   WORKER_PULL_RETRIES=50 / WORKER_PULL_DELAY=20ms, replace in-line pull with
   worker.submit + return Err. Task 1 goes GREEN; prewarm test stays green.
5. Rewrite superseded tests: DELETE hot_path_pulls_missing_body_within_budget
   (:2883, superseded by Task 1's), tighten
   hot_path_fails_view_fast_when_body_never_arrives (:2973) to <80ms,
   recompute hot_pull_budget_under_view_timeout (:3006) for the new consts
   (<50ms consensus-thread; worker budget <2s).
6. Telemetry: native_da_recovery_handoffs + native_da_recovery_timeouts
   counters (RED in the torus-telemetry registry test first; wire per plan).
7. BS-4b: ONE event-driven mid-budget re-fetch inside recover_bodies_bounded
   (still-missing at half-budget, exactly once; fetcher-stub test counts
   fetch() calls).
8. Full sweep (coordinator runs it): `cargo fmt --all -- --check` &&
   `cargo clippy --workspace --all-targets -- -D warnings` (warm ~5min) &&
   `cargo nextest run --workspace` via nohup + done-marker (Bash bg ~600s cap).
   Baseline 1066/1069; ONLY known non-passes allowed: pacemaker
   cumulative-deadline LOAD-FLAKE (retries=2 scoped) + 3 unbounded-loop hangers
   killed at the 10min cap. Any NEW failure = stop, root-cause, fix — never
   rationalize a new failure as flake without proving it (run it solo 3×).

## FINAL (after Task 8 green)

1. Spawn a fable code-reviewer subagent over the WHOLE branch diff
   (`git diff 7c122f7..HEAD`), briefed with the design doc + the
   NON-NEGOTIABLES, asked for correctness findings only. Triage, fix, re-sweep
   anything behavioral.
2. remember_this(): outcome + evidence pointer (commits, test counts) +
   anything future sessions must know. Update
   .claude-state.json whats_next: BS-4a status → implemented-awaiting-devnet-A/B
   (the devnet O2-bs400 A/B protocol is in the plan's end-to-end section — it
   gates fleet rollout, not this branch's completion).
3. Report: commits list, before/after consensus-thread budget, suite result vs
   baseline, open questions (retry-slice tuning, worker deadline) carried to
   the devnet A/B. Do NOT push.

## NON-NEGOTIABLE CONTRACTS (give to every builder and reviewer)

- Sync path (pull_missing_bodies, ~1-8s budgets) behavior-identical — it is
  explicitly allowed to block (off the voting path).
- Prewarm contract: a body already in the fetcher inbound is absorbed by the
  LOCAL slice with zero fetch() calls
  (prewarmed_bodies_absorbed_without_redundant_hot_fetch stays green,
  unmodified).
- MissingData semantics unchanged (NOT Invalid, no peer blacklisting — mem
  28e1a821 lineage).
- No changes outside crates/torus-consensus/src/app.rs + torus-telemetry.
- No test may be weakened to pass; superseded tests are REPLACED per Task 5,
  never silently deleted elsewhere.

## GOTCHAS (this box)

- Commit hook AUTO-STAGES crates/ and hijacks multi-commit chains into "auto:"
  commits — ONE add+commit per Bash call (mem df0a895b).
- Gate 2 blocks heredocs — Write commit-message files + `git commit -F`, or
  single-line -m.
- Gate 4 wants a memory query within ~15min before Edit/Write — re-query when
  blocked.
- Graph STOP hints fire on greps — advisory, ignore.
- cargo clippy --fix no-ops on cached crates (touch lib.rs first) and rolls
  back ALL fixes if one breaks compile.
- Bash bg ~600s cap — nohup + done-marker + poll for nextest/clippy-workspace.
- remember_this has an 800-char cap.

## GUARDRAILS (absolute)

- Do NOT start/stop/restart any testnet node; do NOT touch testnet/data*,
  seed.keystore, genesis — the fleet is prepped for a coordinated relaunch and
  MUST stay down (mem 6d9ef802e7e33f2f).
- Do NOT push ANY branch (unpushed doc commits above 6e03294 are a
  val3-instructions footgun — same memory).
- Do NOT commit: .claude-state.json, .claude/GRAPH_REPORT.md, prompts/, build
  logs, devnet artifacts.
- Subagents: opus/fable only. NEVER Sonnet.

Start with SETUP step 1 now.
