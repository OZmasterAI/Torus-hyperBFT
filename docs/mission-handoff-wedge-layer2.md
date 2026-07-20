# Mission Handoff — Wedge CLOSED, Layer 2 next (updated 2026-07-20)

Continuation doc for a clean session. Supersedes the *phase state* in `mission-s470-execution-plan.md` / `mission-s470-status.md` (keep those for history/context; the frontier-findings list in the status doc is still live reference). Memory manifest `mission-recovery-manifest` mirrors this.

---

## Where we are, in one paragraph

The s470 throughput mission (250k–400k matched/s target) plateaued at 2.4–3.2k matched/s after Packages A–D + rank8 + root-cache + 3b were all built and verified — because the real gate-killer was a **HotStuff liveness wedge** (commit frontier freezes while the QC frontier crawls; views race; locks fork). That wedge is now **diagnosed (W0), fixed (W1), and proven eliminated on a live devnet (W2)**: matched hit a mission-record **3,789/s** (2.6× best-wedged), zero freezes ≥30s at any cap, worst-60s finally >0. The remaining wall is the **exec envelope (~2.2–2.7s/block, flush-dominated)** — that is Layer 2, planned for the 18-core box. The 23.8 blk/s health gate remains failing until Layer 2 lands.

## The wedge fix (Layer 1 — CLOSED)

- **Mechanism** (W0 report, agent transcript; key refs in memory manifest): header-first voting advances `highest_qc_view` at sub-ms speed; commit waits on ~2.7s body exec; 500ms views tick ~5×/block; leaders fork off un-executed tips; monotonic locks diverge >3 generations; `safe_pc` lock clause rejects cross-branch proposals (`is_safe=false, justify_block_known=true`); S459 backoff watches the wrong signal (`view−qc`, which stays small); no recovery on the known-justify drop path; block_sync can't heal lock forks.
- **Fix** (`perf/wedge-fix` @ 172b09a, merged into `perf/re-proof4`): commit-lag view backoff — deadline multiplier gains a `min((qc_view − committed_view) − 4, commit_lag_cap)` exponent term, combined with the S459 term by max. Consensus-visible inputs only (lockstep-safe, stateright-proven). Genesis `commit_lag_backoff_cap` (+ env `TORUS_COMMIT_LAG_BACKOFF_CAP`), **default 0 = exact-today**; FLEET-UNIFORM required. Companion `TORUS_PROPOSER_EXEC_WATERMARK` (default off, yield-not-starve). Diag logging `TORUS_WEDGE_DIAG=1`. 298 tests green incl. wedge repro RED→GREEN and both stateright models.
- **W2 proof** (`95cd399` on `perf/re-proof4`; doc `devnet/wsl/results/w2-wedge-proof-re-proof4.md`): V0 control reproduced (298s freeze, 534/s); cap4 → 3,195/s, gap ≤25s; **cap8 → 3,789/s, gap ≤12s, qc−commit ≤8**; cap8+watermark8 → 3,447/s, most blocks (123), fewest drops. Variance 2.7× → 1.19×.
- **RECOMMENDED SETTING: genesis `commit_lag_backoff_cap = 8`, fleet-uniform; watermark=8 optional.** Residual 10–18 benign lock-drops/run (resolve <30s; Rank-3 recovery net is the deferred deeper fix).

## Layer 2 — exec envelope (NEXT; implementation, not just testing)

Healthy-window budget at the wedge-free state: flush ~1.4s (root 0.75 + state_write 0.48) > engine 0.45 > verify 0.29 > body_persist 0.27 > evm 0.15 (per block). Work items, in order:

1. **Move to the 18-core box** (`18c`/`newserver`, 13.140.140.138 — user-authorized for Layer 2). Prereqs: (a) **coordinate with the user's parallel Fable session** that builds there — agree build-lock file, reserved ports, one-devnet rule BEFORE first use (ask the user to broker); (b) stand up clones + flock + sccache(IDLE_TIMEOUT=0, started OUTSIDE any lock); (c) **fresh baseline trio on 18c** (idle blk/s; flags-off cell; win-combo+cap8 cell) — never compare cross-machine numbers.
2. **Contention re-tune** (bench-only): `TORUS_PARALLEL_BUCKET_HASH` sweep (2/4/8/12) + `TORUS_PARALLEL_SETTLE` on 18 cores — the 6× in-vivo-vs-microbench per-bucket gap (107µs vs 17µs) is suspected CPU oversubscription on 8c.
3. **3c — level-rows-as-authority (Fable, consensus-visible preimage change):** per-price-level aggregate rows become the root authority; order identity lives in resident books + compact per-market commitment. Mine `origin/feat/precompile-0800-topn-gas` (39c74a1) for the built machinery (`price_enc` sign-flip/NOT encoding, `level_journal`, `iterate_cf_bounded`) — see BRANCH-REVIEW verdict in memory. Standardize the save path on the journal-in-book idiom (AB-DIFFER verdict) — delete the executor-side shadow differ.
4. **Hash-only native mirror (Fable, same preimage round):** design doc at `origin/perf/deep-book-storage` 42bca69, written against our exact `native_trie.rs`; ~50–100 LOC; cuts per-bucket bytes ~4×. Batch with (3) as ONE preimage change.
5. **Re-proof on 18c**: gate target ≥23.8 worst-60s at stepped-down offered rates first, then push rate up.

## Layer 3 — after the exec envelope (outlook, not yet scheduled)

- **Matching-engine scaling** (engine share grows as storage shrinks), **RPC/ingress ceiling** (frontier findings #1–6 — 250–400k offered must physically enter the node), **real-WAN dissemination** (Package B has only ever run on loopback).
- **Production-hardening debt (tracked, must-fix before prod):** margin_configs empty ⇒ positions reserve ZERO margin, liquidation never fires (found in A4); Rank-3 lock-fork recovery net; RPC/precompile book readers not row-aware (empty books under `TORUS_BOOK_ROWS=1`); flaky `justify_block_livelock_test` poll bound; consensus-visible flag composition needs ONE fresh genesis at final integration (A5 + C4 + commit_lag_cap + rank15/16 if built).

## Branch map (all pushed to VPS bare repo `/home/crab/torus-b.git`, remote `vpsbuild`; NOTHING on GitHub)

| Branch | HEAD | Contents |
|---|---|---|
| `perf/funnel-truth` | a29a08e | Reference: A-series + Phase0 harness + re-baseline + mission docs |
| `perf/exec-scaleup` | 81ea957 | C1–C4 + rank8 + root-cache (all verified) |
| `perf/package-d` | 650f68b | rank1/2/3 (verified) |
| `perf/package-d-wave2` | ef4bab6 | rank13/10 (verify never ran) |
| `perf/dissemination` | 5915317 | B1–B3 + leader-hint (verify WIP; loopback-benched only via re-proof3+) |
| `perf/root-cost` | 5a443ab | 3b: parallel bucket hash + member cache (verified) |
| `perf/wedge-fix` | 172b09a | W1 commit-lag fix (verified, 298 green) |
| `perf/ab-differ` | (scratch) | 3a microbench + findings doc |
| `perf/profiling` | 6817dda | Phase timers + attribution doc |
| `perf/re-proof4` | 95cd399 | EVERYTHING merged (C+D+rank8+rootcache+3b+B+wedge) + all proof results. **The integration tip.** |
| `perf/early-proof`/`re-proof`/`re-proof2`/`re-proof3` | — | Historical proof branches (results docs on each) |

Their side (user's parallel session, GitHub `origin`): `perf/p3-throughput`, `perf/deep-book-storage`, `feat/precompile-0800-topn-gas` — **never merge wholesale** (pre-A5 fork; Phase-4 conflict would silently revert A5; `0x02` tag collision). Mine for design only.

## Key results docs (in-repo, on the branches noted)

`devnet/wsl/results/`: `rebaseline-f4a12c9-report.md` (funnel-truth) · `early-proof-b8d6780.md` (early-proof) · `profiling-attribution.md` (profiling) · `re-proof-cc8e39e.md` (re-proof) · `r3-proof-re-proof2.md` (re-proof2) · `sys-proof-re-proof3.md` (re-proof3) · `w2-wedge-proof-re-proof4.md` + `w2-re-proof4/` (re-proof4) · `docs/ab-differ-findings.md` (ab-differ).

## Operational rules (hard-won; every agent brief restates these)

1. VPS (`ssh vps`, 95.111.231.121 crab) is the only build/test/bench machine until 18c is stood up; local box is scratch. Every cargo behind `flock ~/.torus-build.lock`; devnet-guard (`ss -ltn | grep -qE "864[567]" && exit 1`) inside the same shell; one devnet at a time on 8645-7/30401-3/9161-3; node-Prometheus counters only, never load-gen ×batch.
2. **Detached + marker + background watcher** for all long remote work: nohup, `set -o pipefail`, `MARK=$?` (or `${PIPESTATUS[0]}`) with NO pipeline between command and marker; one run_in_background watcher polling the marker; never foreground-babysit; never relaunch a step whose marker =0. Watchers must not self-match; test-fire them against a file that already has the marker.
3. Flock fd is inherited by children (devnet nodes = feature, keeps builds out; sccache = trap, run it outside the lock with IDLE_TIMEOUT=0).
4. Agent hygiene: fresh agent + distilled brief for new scope; don't resume heavy-context agents (judgment, not a fixed number); Fable for consensus-critical/preimage work, Opus for mechanical/bench; every brief restates rule 1 verbatim; benches verify node env via /proc + startup lines per cell.
5. Boundaries: merges/pushes to GitHub/deploys and devnet launches need explicit user go (VPS-internal pushes of perf/* are pre-authorized). Exact-today defaults for every change; consensus-visible flags fleet-uniform + fresh genesis.
