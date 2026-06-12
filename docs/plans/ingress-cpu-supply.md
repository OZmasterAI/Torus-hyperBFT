# Design: Ingress Throughput — CPU Supply, Not CPU Cost (Option B reframed)

## Problem
Post-s353, ingress verify was assumed to cost ~70ms CPU per bs500 action
(the wall ceiling). Re-profiling DISPROVES this: the per-action crypto
cost is ~6ms; the 10-70x inflation in every live measurement is CPU
starvation on this 8-core box (load ~11 at idle from non-chain
processes). The chain's ingress code is not the bottleneck — core supply
is.

## Evidence
- `verify_breakdown_by_batch_size` (release, this box, 2026-06-11):
  bs500 phases hex 1.15 + parse 0.71 + sig 2.43 + ser 1.14 + keccak 0.40
  = **5.8ms**; the same function end-to-end read 71.3ms — but the test
  ran nice-19 on a load-11 box, so the total counted PREEMPTION.
  Matches sprint5-A (mem 6b852ecc): "~97% of production verify_seconds
  is queue + core contention, NOT crypto."
- `validate_batch_size` is O(1) (empty/cap check) — not a hidden cost.
- Session DB reads don't fire for plain EIP-712 actions.
- Capacity math: 4 free cores ÷ 5.8ms ≈ 690 actions/s ≈ 345k orders/s
  ingress headroom. Measured: 14/s. Gap = scheduling, 100%.
- s353 5.4s in-closure spans ≈ 600ms of real work stretched ~9x by
  oversubscription — consistent.
- All probes to date used `--format json`; the cheaper bincode endpoint
  (sprint5-B, `torus_submitNativeActionsBin`) was never benched.

## Options

### Option B1: CPU isolation for the node (cpuset/taskset)
Pin the node process (or just consensus + exec + ingress pools) to
dedicated cores (e.g., 0-5), confine the framework/memory-server/etc. to
the rest (systemd AllowedCPUs / cgroup cpuset). Deterministic CPU for
the chain without touching chain code.
- Files: none (ops: systemd user units / launch wrapper).
- Trade-offs: + immediate, reversible, fixes ALL thread starvation
  (consensus too); − box-specific ops work, needs an audit of what's
  burning load-11, other services get less CPU.
- Effort: S (ops). Risk: Low.

### Option B2: Off-box / decluttered probe (measure truth first)
Run the identical bs500 probe with the box decluttered (stop
non-essential services) or drive the node from val1 while this box runs
ONLY the node. Establishes the chain's true ingress ceiling before any
further code work.
- Trade-offs: + answers "what can the chain actually do" definitively;
  − requires coordinating box usage, no permanent fix by itself.
- Effort: S. Risk: Low.

### Option B3: Switch probes/benches to bincode ingress (free win)
`--format bin` exercises the sprint5-B endpoint: drops hex+JSON-parse
(~1.9ms of 5.8ms ≈ 30% of real CPU). Already implemented and
hash-identity-gated; just unused by the bench invocations.
- Files: probe procedure only.
- Trade-offs: + free, also derisks the bin path before clients adopt it;
  − meaningless until contention is fixed (saves 2ms under a 65ms tax).
- Effort: XS. Risk: Low.

### Option B4: Per-action crypto optimization — REJECTED
EIP-712 struct-hash tuning would save ~1-2ms/action against a ~65ms
contention tax. Chasing a ghost; revisit only if a clean-CPU probe
shows verify dominating again.

## Recommendation
B1 + B2 together (audit what's eating the box, pin the node, rerun the
probe), with B3 folded into the rerun procedure. No chain-code changes
until a contention-free probe says otherwise. The s352/s353 pool
isolation stays — it's correct architecture regardless.

## Open Questions
- ~~What exactly produces idle load ~11?~~ ANSWERED (s356 audit below).
- Do the OTHER validators (val1/val2) have the same contention profile?
  Their inclusion latency affects end-to-end numbers too.
- bench-throughput counts duplicate block-body inclusions — fix its
  accounting (exec-side dedup is truth) before trusting its included/s.

## s356 audit (2026-06-12, devnet down, 10s /proc delta sample)
Box steady-state CPU attribution (8 cores):
- toolshed.py (MCP gateway): **100% — one full core, sustained 6 days**.
  Single biggest standing tax; likely a poll loop missing a sleep.
- active claude session: ~37% while working (×3 sessions resident, bursty)
- everything else ≤2% each: surrealdb, mongod, TORUSd, vscode-server,
  ngrok, tmux, memory_server
Per-tool-call hook spikes (statusline.py etc.) hit 100% momentarily but
are short-lived. The historical "load ~11" = this baseline + devnet
containers + bench + hook storms stacking.

Cross-confirmation: the s356 devnet shake-out saturated the box (load 40)
with 5 node containers; averaged CPU was identical for pre-Sprint5 and
HEAD binaries (~270-310% each) — starvation is environmental, not a code
regression. Reconfirms the 5.8ms-real / starvation-tax thesis.

## B1 pinning runbook (prepared s356 — NOT yet applied, needs user go)
Goal: node on cores 2-7 (6 dedicated), framework noise confined to 0-1.
No root needed:
1. `systemctl --user set-property --runtime <toolshed unit> AllowedCPUs=0-1`
   (same for surrealdb.service, memory server unit if systemd-managed;
   ad-hoc processes instead get `taskset -cp 0-1 <pid>`)
2. `taskset -cp 0-1 <tmux-server-pid>` — claude sessions + shells inherit.
3. Node start command gains `taskset -c 2-7` prefix:
   `taskset -c 2-7 nohup ./target/release/torus-node --genesis ... &`
4. Drop `--runtime`/persist only after a validated probe.
Rollback: `taskset -cp 0-7 <pid>` / drop the property.

## B2+B3 re-probe procedure (post-relaunch, quiet + pinned box)
bs500, 10+ senders, `--format bin` (sprint5-B endpoint: drops ~30% of the
5.8ms real cost), exec-side dedup as the truth metric. Expected if thesis
holds: per-action verify CPU ≈ 4-6ms, ingress ceiling ≈ 600+ actions/s
on 6 dedicated cores.
