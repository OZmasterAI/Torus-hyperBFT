# Throughput Mission s470 — Status & Findings

**Goal:** sustain **250k–400k orders/s actually matched/placed on-chain** (node counters `torus_orders_placed_accepted` / `torus_orders_matched`) while block production holds under load (idle **~29.8 blk/s**, health gate **≥ 23.8 blk/s**, zero gossip send-queue drops).

**Base:** `a225844` (superset of `think-dev` HEAD; has S459 wedge fix + pacemaker backoff).
**Ground-truth measurement:** node Prometheus counters only — never the load-gen `included_actions × batch` convention (~190× inflation).

> **Honest baseline (VPS, commit `22c3cf0`, A4+A5 applied):** placed **1.7–3.5k/s**, matched **1.4–2.8k/s**, **zero rejections in ~6M orders** (economics fully fixed), but the health gate **FAILS at every offered rate 3.2k–300k/s** — block rate collapses to <1/s because serial execution runs at only ~5–10 actions/s (~2–4k orders/s). The exec queue pegs its 66-entry cap in seconds → commit freeze. **On this box the wall is execution, not dissemination** (zero send-queue errors). This is the ~150× gap the packages must close.

**Measured throughput improvement so far: ~none on the ceiling** (1.3k → ~2k matched/s, within noise of the same exec wall). The banked win is **acceptance: 0.013% → 100%** (economics fixed). The throughput-lifting packages (B/C/D) are committed but **never benchmarked** — their gains below are implementer/analyst *claims*, not measured results.

**Machine:** builds/tests/benches run on the VPS (8-core); the 4-core local box is too small for cargo + devnet. All source-of-truth commits live in local worktrees; the VPS holds only build copies + one results commit (`22c3cf0`, already pulled local). Nothing pushed to GitHub.

---

## Branches created this session

| Branch | Base | Purpose | State |
|---|---|---|---|
| `perf/funnel-truth` | `a225844` | A-series: measurement truth + economics | ✅ complete (A1–A5 + baseline) |
| `perf/dissemination` | `092df8b` (on funnel-truth) | Package B: gossip/consensus dissemination | ✅ B1–B3 + leader-hint done; verify WIP |
| `perf/exec-scaleup` | `98c76c6` (A5) | Package C: execution scale-up | 🟡 C1+C2 done; C3 WIP; C4 not started |
| `perf/package-d` | `98c76c6` (A5) | Package D Wave-1: backpressure + caps | 🟡 rank1+rank2 done; 7 items + verify pending |
| `perf/package-d-wave2` | `98c76c6` (A5) | Package D Wave-2: math/matching/ingest/retention | 🟡 rank13+rank10 done; rank12+rank9 + verify pending |
| `perf/frontier` | `98c76c6` (A5) | Frontier swarm implementation target | ⚪ empty — analysis done, impl not started |

`perf/bs4a-da-recovery` and `think-dev` are **pre-session**, not part of this mission.

### How the branches relate

```
a225844  (approved base)
   │
   └─► perf/funnel-truth ─────────────────────────────────────────► A-SERIES (done)
         │  092df8b  WSL devnet scripts
         │  98c3778  A1 funnel counters
         │  8fa6ccd  A1 TORUS_SHARD_CUSTODY gate
         │  f1758b7  A3 ground-truth doc
         │  37a0958  A4 econ bench mode
         │  944fa78  A4 genesis 100M
         │  98c76c6  A5 maker-margin release fix
         │  eed446b  devnet perf gate
         │  22c3cf0  honest re-baseline
         │
         ├─(from 092df8b)─► perf/dissemination ──► PACKAGE B
         │                    B3 → B2 → B1 → leader-hint → (verify WIP)
         │
         ├─(from 98c76c6)─► perf/exec-scaleup ───► PACKAGE C
         │                    C1 → C2 → C3(WIP) → C4(todo) → verify(todo)
         │
         ├─(from 98c76c6)─► perf/package-d ──────► PACKAGE D WAVE-1
         │                    rank1 → rank2 → (ranks 3,4,5,6,7,11,17 + verify todo)
         │
         ├─(from 98c76c6)─► perf/package-d-wave2 ► PACKAGE D WAVE-2
         │                    rank13 → rank10 → (rank12, rank9 + verify todo)
         │
         └─(from 98c76c6)─► perf/frontier ───────► FRONTIER (empty; 41 findings pending)

  Assembly target (not yet created):
     perf/integration-400k  ⇐  merge funnel-truth + dissemination + exec-scaleup
                                 + package-d + package-d-wave2 + frontier
```

All branches share base lineage through `perf/funnel-truth`; the four package branches fork from `98c76c6` (A5) except dissemination (forked earlier at `092df8b`, before A1–A5, so it must rebase/merge A5 at integration). Nothing pushed to GitHub.

---

## What was done, per branch

### `perf/funnel-truth` — A-series ✅ COMPLETE
- **A1** `98c3778` + `8fa6ccd` — 8 order-funnel counters (`torus_orders_placed_accepted` / `resting` / `matched` / `rejected_margin` / `rejected_book` / `rejected_cancelled` / `cancelled_partial_fill` / `self_trade_cancels` / `rejected_other`); `TORUS_SHARD_CUSTODY` env gate (default on; `0` disables erasure custody for perf runs).
- **A3** `f1758b7` — instrumented funnel run pinned the "400→2 collapse" as **99.987% margin-rejection at Phase-2 pre-reserve** (bench economics, not a chain bug).
- **A4** `37a0958` + `944fa78` — `--econ` bench mode (orders sized to funding via `--target-margin`, guaranteed crossing via `--cross-fraction`, margin recycling via `--cancel-fraction`, one side per sender→no self-trades); genesis bulk-sender balance 1M→100M TRS.
- **A5** `98c76c6` — **node-side maker-fill margin release fix**: resting orders filled as maker (and STP-cancelled makers) now release their reservation via telescoping `reserve()` differences (Σreleased == reserved exactly). Consensus-visible (fresh-genesis). 8 new tests + all 12 torus-bridge suites green.
- **Baseline** `eed446b` + `22c3cf0` — devnet perf gate + the honest re-baseline (see top).

### `perf/dissemination` — Package B ✅ (verify WIP)
- **B3** `ec3200c` + `a2caa6d` — gossipsub `connection_handler_queue_len` made configurable (`TORUS_GOSSIP_QUEUE_LEN`, default 512, `5000`=today); SlowPeer/AllQueuesFull drop counters into telemetry. *(Deviation: libp2p `metrics` feature incompatible — prometheus-client 0.23 vs 0.24; used counter alternative.)*
- **B2** `406953c` + `fcc49b5` — consensus broadcasts move off gossipsub onto the `/torus/direct` unicast fan (`TORUS_CONSENSUS_DIRECT_FAN`, default off; `TORUS_CONSENSUS_GOSSIP_MIRROR`, default on). Sender-side only (receive path already parses hotstuff msgs from direct) → mixed-fleet safe. Dual-path dedup LRU (keccak256, cap 4096).
- **B1** `ccb94ea`…`2c9c0c1` — batched direct-to-leader `0xFD` forward envelope replacing 1-request-per-action; bounded RPC forward channel + drop counter; ForwardBatcher (25ms/512KB flush, flush-time leader re-resolution); retry ≤2 on failure.
- **leader-hint** `9b6d797` + `db1f397` — real pacemaker view + real (reputation-aware) selection into `LeaderState` instead of the `height+1` heuristic.
- **verify** `5915317` — [WIP] verifier fixes (dedup-scope guards + sweep-arming test). **Verify pass did not finish.**

### `perf/exec-scaleup` — Package C 🟡 HALF
- **C1** `1b5494a` + `349a130` — PositionCache (write-back cache for position rows, deterministic sorted flush); per-fill `flush_and_evict` removed. *Claimed* 3–5× on `exec_phase_settle` (unmeasured). Differential byte-equality tests vs classic path.
- **C2** `ba17048` + `d5920bf` + `f3991c1` — overlay CF-name interning + per-CF stack keys (kills `(String, Vec<u8>)` alloc per get/put); removed 4–5 per-order `PlaceOrderParams` deep clones.
- **C3** `fbafbd9` — **[WIP, mid-implementation]** deterministic parallel Phase-4 settlement (`TORUS_PARALLEL_SETTLE`). 875 lines + 494-line test file present; **not finished, not verified.**
- **C4** — per-order-row book persistence (replace whole-book Borsh+keccak). **NOT STARTED.**
- **verify** — **NOT STARTED.**

### `perf/package-d` — Package D Wave-1 🟡 2 of 9
- **rank1** `a992aaa` + `6d3b258` — exec-backlog watermark pacing (`TORUS_EXEC_THROTTLE_WATERMARKS`): scale proposer caps by exec queue depth. Half of the fleet-freeze fix.
- **rank2** `1075f68` + `ee763a2` — non-blocking exec dispatch + bounded `deferred_exec` (try_send instead of blocking send that parks the HotStuff thread). The other half — **together these remove the exact "blk/s collapses under load" mechanism the baseline exhibited** (unmeasured).
- **NOT STARTED:** rank3 (cap 100→400 — *mandatory per baseline arithmetic*), rank4 (FIFO selection), rank5 (bincode-once persistence), rank6 (DA-mirror shave), rank7 (pool-scan indexes), rank11 (ingest QoS lanes), rank17 (hygiene bundle), **+ the Wave-1 verify pass** (never ran).

### `perf/package-d-wave2` — Package D Wave-2 🟡 2 of 4
- **rank13** `8cb5590` + `7a5981f` — FixedPoint i128 fast path (skip ethnum i256 when operands fit); exhaustive-boundary differential oracle.
- **rank10** `3105b56` + `ef4bab6` — matching-core micro-architecture (FxHash indices, single-descent price levels, O(1) trader back-index removal).
- **NOT STARTED:** rank12 (single-pass ingest — hash+bincode once outside the pool lock), rank9 (retention horizon GC for per-order CFs), **+ the Wave-2 verify pass** (never ran).

### `perf/frontier` — ⚪ ANALYSIS ONLY
8 analysts completed and produced **41 findings** (below). The 42 skeptics + synthesizer + all implementers died at the session limit (plus a `plan.implement_now` null-guard bug). **No code written.** Findings are recovered and listed below.

---

## What each wave is still missing (cut off by session limit)

| Wave | Done | Missing | Notes |
|---|---|---|---|
| **A** | A1–A5 + baseline | — | Complete. |
| **B** | B1, B2, B3, leader-hint | Verify pass (WIP `5915317`); design §5 integration tests (see below); A5 rebase at integration | ~95%. |
| **C** | C1, C2 | **C3 finish** (WIP present), **C4** (not started), verify | C is the critical-path package per the baseline; C3+C4 are the exec-ceiling lifters. |
| **D-1** | rank1, rank2 (freeze-fix pair) | **rank3 (cap→400, mandatory)**, rank4, rank5, rank6, rank7, rank11, rank17, **verify pass** | 7 of 9 items + verify. |
| **D-2** | rank13, rank10 | rank12, rank9, **verify pass** | 2 of 4 items + verify. rank9 (retention GC) needed for *multi-hour* runs. |
| **D-3 (Wave-3)** | — | **rank15** (flag-gated O(1) sig-attestation digest), **rank16** (load-scaled view deadline) | **Never scheduled** — both consensus-visible / genesis-gated; land at integration with fresh genesis. |
| **Frontier** | 8-analyst analysis (41 findings) | Skeptic filter + synthesis + all implementation | Resume with null-guard fix; run skeptics→synth→implement. |

**Deferred by design** (collide with C4's book rewrite): Package D **rank8** (resident order books) and **rank14** (parallel trie recompute) — land on top of C after C4.

**Package B deferred tests** (from the B design §5, never written — flagged by the B2/B1 agents): the **3-node in-proc bulk-flood regression** (proposals must beat the view timeout under gossip flood, fan on) and the **2-node direct-to-leader integration test** (bs=1 → all actions in leader pool with bounded request count). These are the "proves it actually fixes the collapse" tests; the unit-level failing-tests-first items are done, these deployment-shaped ones are not.

**Cross-cutting, never done:** the **integration branch** (`perf/integration-400k`), the **integration verify** (cross-package interaction, especially C×D on the exec path, and the A5 semantics surviving C's cache/parallel paths), and the **multi-minute sustained proof run** at target load. No package has been benchmarked — measured throughput gain to date is ~zero (see top).

---

## The 41 frontier findings (8 analysts, NOT yet skeptic-filtered)

Raw candidates with self-claimed gains. `territory: free` = implementable now without touching an in-flight branch's files; `owned-pkgX` = must wait for that package to integrate. Grouped by analyst lens.

### Lens 1 — RPC ingress server
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 1 | Raise jsonrpsee `max_connections` 64→1024 (`TORUS_RPC_MAX_CONNS`) | S | free | Removes hard ingress ceiling (~426k actions/s theoretical binds below 400k) |
| 2 | Raise `SUBMIT_BATCH_MAX` 100→500-1000, scale permits×batch | S | free | Cuts RPC-call rate 5–10× at target |
| 3 | Env-tune RPC runtime `worker_threads` (fixed 4) + verify pool (cores/2) | S | free | Unlocks verify-CPU scaling past cores/2 |
| 4 | Cache known market-ids — drop per-action RocksDB point-get from verify loop | M | free | Removes 100k–400k gets/s from ingress; ~5–15% verify CPU |
| 5 | Kill hex-in-JSON on Bin path (base64 now, raw-bytes endpoint next) | L | free | Halves ingress wire bandwidth |
| 6 | Compact-ack mode for bulk submit (stop serializing 400k hash strings/s) | S | free | −30MB/s + 400k allocs/s off rpc-workers; shrinks ack RTT |

### Lens 2 — RocksDB / storage / build profile
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 7 | **`[profile.release]` thin LTO + codegen-units=1 (workspace has NONE)** | S | free | **5–15% whole-node CPU**, cheapest multiplier |
| 8 | **jemalloc/mimalloc global allocator** | S | free | **5–20%** on alloc-heavy multithreaded Rust; less RSS frag |
| 9 | Shape-differentiated CF options (universal compaction, no upper compression on firehose CFs) | M | free | 3–6× less compaction I/O; removes primary write-stall |
| 10 | DB-wide write headroom: bg jobs 8 + subcompactions + WriteBufferManager + max_total_wal_size | S | free | Prevents flush-queue saturation + WAL blowup; ~2× compaction throughput |
| 11 | `disable_wal(true)` on bg trade-history writer | S | free | Cuts total WAL bytes ~half (~330MB/s) at 400k |
| 12 | **RocksDB write-stall wall: one-size CF opts can't absorb 400k sustained** | M | free | **Removes the "minute-3 dies" mid-run blk/s collapse** |
| 13 | Trade-history amplification: WAL-off bg writer + alert on exec-blocking queue | S | free | −50–150MB/s trade write volume; observable stall |
| 14 | **fd exhaustion: no ulimit + `max_open_files=-1` kills validator mid-run** | S | free | **Eliminates a whole-validator-death mode in multi-minute runs** |

### Lens 3 — hotstuff_rs consensus internals
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 15 | **Arc-ify block `Data` — kill 6MB deep clones on propose/serve/insert** | M | free | −18–30MB memcpy/view; **+5–12% blk/s under 6MB load** |
| 16 | Close the 10ms block-body wakeup gap in the algorithm loop | S | free | Recovers ~4–9ms/view on followers; +10–15% commit cadence |
| 17 | Verified-PC memo cache + batch/hoisted signature verify | M | free | O(V²)→ cached; ~2–3% view @ V=3, ~12ms @ V=10 |
| 18 | In-memory hot-state cache for `Pacemaker::tick` KV churn | S | free | ~0.5–1ms/view off the hottest loop |
| 19 | Per-event-type publish gating + hot-path log demotion | S | free | Removes unbounded event-channel OOM/GC-stall vector |

### Lens 4 — load generator
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 20 | Default econ to `--format bin` + zero-copy JSON-RPC body splice | S | free | ~2.7→~1.1 cores at rate 1000; may avoid a 2nd loadgen host |
| 21 | Cache EIP-712 typehashes + domain separator as statics | S | free | ~0.15–0.3 core back on loadgen + same on each validator verify |

### Lens 5 — proposer egress
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 22 | Compress-once content cache + reusable zstd contexts (/2.0 frames) | M | free | ~0.5–0.75 core off proposer @ 400k; protects consensus-sharing conn |
| 23 | Raise QUIC MTU discovery upper bound for loopback/devnet | S | owned-pkgB | ~45× fewer packets on loopback; 0.3–0.7 core aggregate |
| 24 | Raise per-conn QUIC flow-control windows (conn/stream data) | S | owned-pkgB | Removes latency cliff where consensus queues behind body bytes |
| 25 | Lift `MAX_DA_SERVE_INFLIGHT=8` serve-pool cliff | S | owned-pkgB | Removes empty-response shed / retry storm at manifest scale |
| 26 | Bench A/B raise `TORUS_HASH_ONLY_PUSH_THRESHOLD` to 4MB floor on loopback | S | free | One-way push (no pull RTT) for ≤4MB bodies; ~half proposer serve |

### Lens 6 — execution pipeline shape
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 27 | **Pre-verify N+1's ecrecover while N executes** | M | free | **Hides verify wall — 1.3–2×** |
| 28 | Batch nonce replay guard (multi_get + memtable whole-key filter) | S | free | 5–10× on `exec_replay_guard_seconds` |
| 29 | Write-behind state flush: pipeline N's flush under N+1's execute | L | owned-pkgC | 1.25–1.4× exec (removes save_books+flush from critical path) |
| 30 | Phase-2/Phase-4 overlap verdict: cross-batch unsafe → parallelize Phase-2 by sender | M | owned-pkgC | ~1.1–1.3× engine (Phase-2 reserve / n_cores) |
| 31 | Commit-callback slimming: kill duplicate body persist, move serialize off consensus thread | M | owned-pkgD1 | Removes 1 full-body JSON serialize from consensus callback + 1 from exec |

### Lens 7 — observability
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 32 | Node `/metrics` scraper in bench (measure mission counters directly) | M | free | 400k claim provable from node counters in one run; same-run funnel closure |
| 33 | Machine-readable `--csv` bench emitter with fixed campaign schema | S | free | Sweep cells directly comparable/diffable across branches |
| 34 | Make node-counter deltas the headline; demote block-body re-sweep to opt-in | M | free | Removes GBs of post-run RPC; stops sweep perturbing the drain window |
| 35 | Stop fetching full block bodies in live monitor inside timed window | S | free | Removes biggest bench-induced distortion; protects blk/s gate |
| 36 | If sweep retained: hash raw JSON slices (RawValue) not `Value.to_string()` | S | free | ~half sweep CPU; −1.5GB transient alloc/cell |
| 37 | Network-ingest admission reject counters (gossip/forward/push) | S | owned-pkgD2 | Closes drop accounting across all nodes in one pass |
| 38 | Per-market funnel family + per-block matched histogram | S | owned-pkgC | Shows balanced vs hot-book flow; sustained vs bursty |
| 39 | Action age-at-exec histogram (end-to-end latency + clock alignment) | S | owned-pkgD1 | One-pass backlogged-vs-slow answer |

### Lens 8 — failure modes at 400k
| # | Finding | Size | Terr | Claimed gain |
|---|---|---|---|---|
| 40 | **`getBlockBody` live-poll melts val0 + 10MiB response cap silently drops fullest blocks** | M | free | **Frees ~1 RPC core; guarantees 400k count can't be under-reported** |
| 41 | jsonrpsee `max_connections(64)` undercuts real shed point, starves monitor | S | free | Removes hidden ingest ceiling + measurement blind spot |

*(Findings 41 and 1 overlap RPC-connection tuning; 9/10/12 overlap RocksDB tuning; skeptic/synthesis pass will dedup these — they were not yet filtered when the swarm died.)*

### Orchestrator's read on the frontier findings
Highest-value, lowest-risk, genuinely-new and free-territory: **#7 (missing release profile)** and **#8 (jemalloc)** — pure build config, whole-node multipliers, zero semantic risk; and **#9/#12/#14 (RocksDB CF options + fd limit)** — these target *exactly* the write-stall/swap-pressure mechanism the honest baseline exposed. The RPC-ingress cluster (#1–#6, #41) is real but aims at a ceiling not yet reached (exec is the wall first). The observability cluster (#32–#36, #40) is what makes the eventual 400k proof run trustworthy in one pass.

---

## Resume checklist (next session)
1. **Finish C3** (WIP `fbafbd9`) → C4 → C verify. *(Critical path — this is the exec-ceiling lifter.)*
2. **Package D Wave-1 rank3 (cap→400, mandatory)** + ranks 4,5,6,7,11,17 + Wave-1 verify.
3. **Package D Wave-2** rank12, rank9 + Wave-2 verify.
4. **Package B** verify pass finish + the two design §5 integration tests.
5. **Package D Wave-3** rank15, rank16 (consensus-visible — schedule with fresh genesis at integration).
6. **Frontier**: fix `plan.implement_now` null-guard, resume the frontier workflow → skeptics → synth → implement the free-territory winners (start #7, #8, #9, #12, #14).
7. **Assemble** `perf/integration-400k` (merge all + rebase B onto A5), integration verify (C×D exec-path interaction, A5 semantics survival), rebuild, run the 4-cell ladder with node-counter measurement, then land the deferred rank8/rank14 on top of C4.

*Devnet rule: only one bench agent runs a devnet at a time; implementation workflows never launch devnets.*
