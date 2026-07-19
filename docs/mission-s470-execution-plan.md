# Mission s470 — Execution Plan (Opus subagents, no workflows)

Resume plan to finish the throughput mission (250k–400k matched/placed orders/s with block health, proven by node counters). Companion to [`mission-s470-status.md`](mission-s470-status.md), which holds the per-branch state and the 41 frontier findings.

This plan is the product of a multi-pass review; the methodology corrections (early proof, fixed-harness re-baseline, genesis coordination, build serialization, named A5-survival check) are baked into the phase order below — they are not optional add-ons.

---

## Hard constraints (every agent, no exceptions)

- **VPS is the only build/test/bench machine.** All `cargo build`, all `cargo test` / verify passes, and all devnet benches run on the VPS (8-core). The 4-core local Windows/WSL box is scratch only — any number it produces is never a baseline or comparison point.
  - Agents author in the local Windows worktree → sync the branch to that package's VPS clone → build/test/bench there.
  - VPS clones: `~/torus-c` (C), `~/torus-d` (D), `~/torus-b-work` (B), `~/torus-bench` (harness). Bare repo: `/home/crab/torus-b.git`.
- **VPS build-lock.** Every cargo invocation acquires `flock ~/.torus-build.lock` first — exactly one cargo build on the box at a time. (Prevents the concurrent-rocksdb-build corruption already seen this mission.)
- **One devnet at a time.** Only bench/proof agents ever launch a devnet; implementation agents never do. Bench devnet stays on reserved ports `8645-8647 / 30401-3 / 9161-3`, away from live services (SurrealDB :8823, VS Code server, other sessions).
- **Node counters only.** Throughput claims come from node Prometheus counters (`torus_orders_placed_accepted` / `torus_orders_matched`), never the load-gen `included_actions × batch` convention (~190× inflation).
- **Exact-today default behavior.** Any behavior change ships env-gated with the default equal to today's value. Benches turn flags on explicitly.
- **No merge / push / deploy without an explicit ask.** Phase 1's early-proof bench and Phase 3's integration merge + proof run are the boundary actions that require a go-ahead.

---

## Phase 0 — Fix the measurement, then serialize the box *(prerequisite for any bench)*

The current "honest baseline" (1.4–2.8k matched/s) was measured with a broken harness. It cannot be trusted until fixed, and nothing downstream can be measured against it.

**Lands on `perf/funnel-truth`** (the reference branch) — because #40/#41 are node-side (`torus-rpc`) changes to the node binary, not just bench-tool changes. `funnel-truth(+Phase0)` becomes the new measurement base that C, D-rank3, and the re-baseline all build from.

| Agent | Work | Notes |
|---|---|---|
| **M-harness** | #32 node-counter scraper in bench; #34/#35 stop the in-window block-body sweep; #40 kill `getBlockBody` live-poll melt + lift the 10 MiB response cap; #41 raise `max_connections` | Node-side changes (#40 cap, #41 conns) **env-gated, default = today's value**, so default node behavior is untouched and re-baseline + proof run with the identical flag set. |
| **build-lock** | Stand up `flock ~/.torus-build.lock` discipline on the VPS | One cargo at a time. |

**Merge-coordination flag:** #40/#41 touch `torus-rpc/lib.rs` near (not in) pkgB's forward-channel region — expect a neighborhood conflict at B's eventual merge.

---

## Phase 1 — C + mandatory D1, then the EARLY PROOF (the real go/no-go)

Execution is the wall. If C doesn't move the ~2–4k exec ceiling, nothing downstream matters — and we want that answer in one bench, not after six merges. This is the attribution gate.

1. **Re-baseline** `funnel-truth(+Phase0)` on the fixed harness — **no C, no D**. This is the real reference number; it may land **higher** than 1.4–2.8k (the old cap silently dropped the fullest blocks), which recomputes the "~150× gap." Everything downstream is a delta against *this*, not the old number.

| Agent | Branch | Must do | Done = |
|---|---|---|---|
| **C-core** *(critical path)* | `perf/exec-scaleup` | Finish C3 parallel Phase-4 settle (WIP `fbafbd9`) with a **named `a5_maker_release_survives_parallel_settle` test** (A5's maker-margin release logic — the `maker_margin_releases_cfg` helper — is literally being pulled into C3's parallel path; this is C3's core correctness contract, not generic byte-equality) + the 50× determinism harness → then **C4** (per-order-row book persistence; **consensus-visible — changes the state-root format**) → C verify | C3+C4 committed; full torus-bridge suite + named A5-survival + determinism green on VPS |
| **D1-mandatory** | `perf/package-d` | **rank3 (cap 100→400)** — arithmetically required for 400k; freeze-fix pair (rank1/2) already done | rank3 committed + verify green |

2. **EARLY-PROOF** (bench agent, exclusive devnet): measure `funnel-truth(+Phase0) + C`, then `+ D-rank1/2/3`, as deltas against the re-baseline. **Node counters only.**

**→ STOP here for the numbers.** This is the read point. Phase 2 spend is conditional on C actually moving the wall.

---

## Phase 2 — Conditional: author the rest *(only if Phase 1 shows movement)*

| Agent | Branch | Work |
|---|---|---|
| **D1-batch** | `perf/package-d` | rank4 (FIFO select), rank5 (bincode-once), rank6 (DA-mirror shave), rank7 (pool-scan indexes), rank11 (ingest QoS), rank17 (hygiene) + D-1 verify |
| **D2** | `perf/package-d-wave2` | rank12 (single-pass ingest), rank9 (retention GC) + Wave-2 verify |
| **D3** | `perf/package-d-wave2` (or own) | rank15 (O(1) sig-attestation digest), rank16 (load-scaled view deadline) — **consensus-visible / genesis-gated** |
| **B-finish** | `perf/dissemination` | Finish verify (WIP `5915317`) + write the two design §5 integration tests: 3-node in-proc bulk-flood (proposals beat view timeout under gossip flood) and 2-node direct-to-leader (bs=1 → bounded request count) |
| **F-triage** | — | Skeptic-filter + synthesize the 41 frontier findings into a vetted, ranked shortlist |
| **F-impl** | `perf/frontier` | Implement the free-territory winners aimed at the write-stall wall: #7 (`[profile.release]` thin-LTO + codegen-units=1), #8 (jemalloc global allocator), #9/#12 (shape-differentiated RocksDB CF options), #14 (fd-limit + `max_open_files`). Defer `owned-pkgX` findings to post-integration. |

---

## Phase 3 — Integration + final proof *(serial; needs explicit go)*

| Agent | Must do |
|---|---|
| **INT** | Create `perf/integration-400k`; merge `funnel-truth(+Phase0)` + all package branches; **rebase B onto A5** (B forked at `092df8b`, pre-A5); resolve expected **C×D exec-path conflicts** + the #40/#41 vs pkgB neighborhood conflict; land design-deferred **rank8** (resident books) + **rank14** (parallel trie) on top of C4; **compose ONE fresh genesis** covering all consensus-visible changes: **A5 + C4 + rank15 + rank16** (explicit step, not incidental) |
| **PROOF** | Rebuild integration on the VPS; run the full 4-cell ladder on the fixed harness; measure **node counters only**; produce the honest report — is the exec-wall gap closed, and does the health gate hold (≥ 23.8 blk/s) at 250k–400k sustained? |

---

## Consensus-visible / genesis-gated inventory (compose together at INT)
- **A5** — maker-margin release (already on funnel-truth)
- **C4** — per-order-row book persistence (changes state-root format)
- **rank15** — sig-attestation digest
- **rank16** — load-scaled view deadline

## Backup / durability note
The 4 package branches + this session's C3 WIP currently exist **only on the local Windows disk**. The VPS bare repo holds only `funnel-truth` (22c3cf0) + a stale `dissemination` (db1f397). Pushing the branches to `~/torus-b.git` is a real off-machine backup **and** unblocks the VPS rebuilds — but it's a `push`, so it waits for an explicit ask.

---

## The one number that matters
The mission is currently **claim-only: ~zero measured throughput gain**. The banked win so far is *acceptance* (0.013% → 100%, economics fixed), not *ceiling*. EARLY-PROOF (Phase 1) and PROOF (Phase 3) are the only agents that determine whether the mission actually succeeded.
