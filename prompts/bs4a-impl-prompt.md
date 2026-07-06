# BS-4a — DA reconstruct off-thread (`/implement`) prompt

Copy-paste everything below the `---` into a fresh session to implement BS-4a
(+BS-4b piggyback). Plan: `docs/plans/bs4a-da-reconstruct-offthread-impl.md` ·
PRP: `PRPs/bs4a-da-reconstruct-offthread.tasks.json` · Design:
`docs/plans/bs4a-da-reconstruct-offthread.md`. Authored S421 (2026-07-06).

---

/implement docs/plans/bs4a-da-reconstruct-offthread-impl.md — BS-4a Tasks 1→8, TDD, one commit per task.

GOAL: Stop `reconstruct_native_actions_hot` from blocking the single hotstuff
consensus thread for up to ~260ms on a native-DA body miss (100ms local retry +
160ms in-line hot pull vs the 500ms view timeout). Design decision (user-approved
Option B, mem 7efe7062188cf8d0): fail the view fast (MissingData after ONE ≤20ms
wake-on-arrival slice) and hand the missing hashes to a dedicated
`DaRecoveryWorker` thread that runs the pull loop off-thread with a ~1s
event-driven budget, so the re-proposed view finds the bodies durably local.
Consensus-thread worst case ~260ms → ~20ms. This targets the mechanism implicated
in the S419 O2 body-miss death spiral (mem 7d140b09d2f0ad37, c8308bdbdcac27e4).

SCOPE GUARANTEE: per-node liveness policy ONLY — no consensus-validity, wire, or
persistence change. NO lockstep deploy. NOT part of the pinned relaunch binary
(fleet pin = 6e03294).

BRANCH (do this FIRST): `git switch -c perf/bs4a-da-recovery 7c122f7` — the local
sprint head; its code is IDENTICAL to fleet-pin 6e03294 (the two commits above are
doc-only auto-commits and contain the plan files you need in-tree). Verify:
`git log --oneline -3` shows 7c122f7 / 5d8b1a8 / 6e03294. Do NOT touch or push
`sprint/blockspeed-orders-s395`.

READ FIRST (in this order):
1. Plan:   docs/plans/bs4a-da-reconstruct-offthread-impl.md  (8 tasks, exact test code, success criteria, rollback)
2. PRP:    PRPs/bs4a-da-reconstruct-offthread.tasks.json      (per-task validate commands + deps)
3. Design: docs/plans/bs4a-da-reconstruct-offthread.md        (Options A/B/C; B chosen; open questions)
4. Memory — run: run_tool("memory","search_knowledge",{"query":"BS-4a DA recovery worker reconstruct off-thread MissingData hot path"})
   then get_memory each (FULL ids — 8-char prefixes fail):
   - 7efe7062188cf8d0  BS-4a design verdict (Option B, key insight: fetch() is already non-blocking — only the wait loops block)
   - f09d43b16a3adef2  BS-4a 8-task plan summary
   - 7d140b09d2f0ad37  S419 collapse root cause (context: why the consensus-thread pull budget matters)
   - df0a895bff62b4e0  auto-commit hook HIJACKS multi-commit chains (see GOTCHAS)
   - 843092a5764de1fb  S420 test-suite baseline (1066/1069 + known non-passes)

KEY LANDMARKS (all crates/torus-consensus/src/app.rs unless noted; verify in-tree, lines may drift):
- reconstruct_native_actions_hot :1124 (the function; phase 1 local retry :1148-1188 KEEP one-slice, phase 2 in-line pull :1190-1205 REPLACE with worker handoff)
- pull_missing_bodies_bounded :1041 (extract body → free fn recover_bodies_bounded; sync path stays behavioral-identical)
- NativeDaFetcher trait :633 · da_fetcher field :614 · set_native_da_fetcher :852 (spawn the worker here when mempool is Some)
- consts :699-714 (RECONSTRUCT_RETRIES 5→1; DELETE HOT_PULL_RETRIES/HOT_PULL_DELAY; ADD WORKER_PULL_RETRIES=50, WORKER_PULL_DELAY=20ms)
- tests to keep green: prewarmed_bodies_absorbed_without_redundant_hot_fetch :2926 (absorb stays in the local slice; fetches==0)
- tests to rewrite (Task 5): hot_path_pulls_missing_body_within_budget :2883 (DELETE, superseded), hot_path_fails_view_fast_when_body_never_arrives :2973 (tighten to <80ms), hot_pull_budget_under_view_timeout :3006 (recompute for new consts)
- worker thread lifecycle precedent: torus_state::BackgroundCfWriter (O3, commit 1a51d1e) — channel-fed, drop-join
- telemetry: add counters next to native_da_pull_requests/native_da_pull_recovered in torus-telemetry
- integration test that must stay green: crates/torus-integration-tests/tests/native_da_pull_fallback.rs

TASKS (TDD — write the FAILING test first, paste the red output, then implement; run the PRP validate per task; ONE `git add <files> && git commit -m "..."` in a SINGLE Bash call per task):
1. RED: hot_path_hands_off_and_recovers_in_background (exact code in the plan) — Err within 80ms + background recovery within 2s. MUST FAIL first.
2. Extract free fn recover_bodies_bounded(mempool, fetcher, missing, retries, delay, metrics) from pull_missing_bodies_bounded; TorusApp delegates. Pure refactor — sync pull tests green before AND after.
3. DaRecoveryWorker: named thread "torus-da-recovery", mpsc channel, store-check dedup per batch, Drop = drop tx + join. Unit test recovery_worker_recovers_late_body_off_thread.
4. Wire worker (spawn in set_native_da_fetcher), shrink local retry to 1 slice, replace in-line pull with worker.submit + return Err. Task 1 goes GREEN; prewarm test stays green.
5. Rewrite the superseded tests + budget const test (see landmarks).
6. Telemetry: native_da_recovery_handoffs (handoff in step 4's path) + native_da_recovery_timeouts (worker budget exhausted). RED in torus-telemetry registry test first.
7. BS-4b: ONE event-driven mid-budget re-fetch inside recover_bodies_bounded (refetch still-missing at half-budget, exactly once; fetcher-stub test counts fetch() calls).
8. Full sweep: cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings (warm ~5min) && cargo nextest run --workspace. Baseline = 1066/1069; ONLY known non-passes allowed: pacemaker cumulative-deadline LOAD-FLAKE (retries=2 scoped) + 3 unbounded-loop hangers killed at the 10min cap. Any NEW failure = stop and fix before proceeding.

NON-NEGOTIABLE CONSTRAINTS:
- Sync path (pull_missing_bodies, ~1-8s budgets) behavior-identical — it is explicitly allowed to block (off the voting path).
- The prewarm contract holds: a body already in the fetcher inbound is absorbed by the LOCAL slice with zero fetch() calls.
- MissingData semantics unchanged (NOT Invalid, no peer blacklisting — mem 28e1a821 lineage).
- No changes outside torus-consensus/app.rs + torus-telemetry.

GOTCHAS (this box):
- The commit hook AUTO-STAGES crates/ and HIJACKS multi-commit chains into "auto:" commits — do ONE add+commit per Bash call, never chain commits (mem df0a895b; bit S421 twice).
- Gate 2 blocks heredocs — Write commit-message files and use `git commit -F`, or single-line -m.
- Gate 4 wants a memory query within ~15min before Edit/Write — re-query when it blocks.
- Graph STOP hints fire on greps — ignore them, they're advisory.
- cargo clippy --fix no-ops on cached crates (touch lib.rs first) and rolls back ALL fixes if one breaks compile.
- Bash bg ~600s cap — nohup + done-marker for anything long; warm workspace clippy ~5min.

GUARDRAILS:
- Do NOT start/stop/restart any testnet node or touch testnet/data*, seed.keystore, genesis — the fleet is prepped for a coordinated relaunch (mem 6d9ef802e7e33f2f) and MUST stay down.
- Do NOT push ANY branch (the unpushed doc commits above 6e03294 are a val3-instructions footgun — see mem 6d9ef802e7e33f2f).
- Do NOT commit: .claude-state.json, .claude/GRAPH_REPORT.md, prompts/, build logs, devnet artifacts.
- Commit messages end with the Co-Authored-By footer.

Start: create the branch, read the three docs + memories, confirm a green baseline (`cargo test -p torus-consensus` for the touched test modules), then Task 1.
