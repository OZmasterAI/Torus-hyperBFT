# Mission s470 — New-Session Kickoff Prompt

Copy-paste the block below into a fresh session to resume the mission as orchestrator.

---

```
Resume the s470 throughput mission. Read these first, in order:
- docs/mission-s470-execution-plan.md   (the phased plan — follow it exactly)
- docs/mission-s470-status.md           (per-branch state + 41 frontier findings)
Also load your memory: throughput-mission-s470, autonomous-execution-throughput-mission,
vps-build-bench-escalation, mission-recovery-manifest.

YOUR ROLE: orchestrator. You plan, sequence, review, and integrate. You do NOT write the
implementation yourself — subagents do the actual coding, building, testing, and benching.
Your job is to dispatch them, review their output, and stitch the results together.

SUBAGENT RULES (hard):
- Opus subagents ONLY. NOT Fable. NOT Sonnet. NO workflows of any kind.
- The agents do the work — but keep the count LOW. One agent per workstream, running
  mostly sequentially, not a swarm. A phase should be 1–3 agents at a time, never 6+.
  Give each agent a substantial, well-scoped chunk rather than splitting work across many
  small agents.
- Never launch a new agent for work an existing/recent agent can continue (use SendMessage
  to continue an agent with its context intact).
- Every agent brief you write MUST restate the VPS build rule verbatim — the agent does NOT
  see this prompt and will otherwise default to building locally:
    "Build, test, and bench ONLY on the VPS (ssh vps), never on the local box. Use this
     workstream's VPS clone: ~/torus-c (C), ~/torus-d (D), ~/torus-b-work (B), ~/torus-bench
     (harness). Before any cargo build/test/bench, acquire flock ~/.torus-build.lock so only
     one cargo runs on the box at a time. Only bench agents launch a devnet, one at a time,
     on ports 8645-8647 / 30401-3 / 9161-3. Report throughput from node Prometheus counters
     only, never load-gen ×batch."

EXECUTION RULES (from the plan — do not deviate):
- VPS is the ONLY place anything is built, tested, or benched. Local 4-core box is scratch;
  its numbers are never baselines. Every cargo build/test/bench acquires flock ~/.torus-build.lock
  (one cargo at a time). One devnet at a time; only bench agents launch one.
- Node Prometheus counters only for any throughput claim — never load-gen ×batch.
- Follow the phase order strictly: Phase 0 (fix harness on perf/funnel-truth + build-lock)
  → re-baseline on the FIXED harness → Phase 1 (C-core + D1-rank3) → EARLY PROOF.
  STOP at the early-proof numbers and report to me before any Phase 2 spend.
- Exact-today default behavior: all changes env-gated, default = today.

HARD BOUNDARIES — stop and ask me first:
- Any merge, push, or deploy (incl. pushing branches to the VPS bare repo).
- Launching the early-proof or final-proof devnet bench.
Everything up to those points is pre-authorized — proceed without asking.

Start by reading the docs, then give me a short confirmation of Phase 0's concrete
first steps and which single agent you'll dispatch first. Do not start until I confirm.
```
