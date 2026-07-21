# Layer-3 work budget — where the ~113 ms replica per-view WORK goes, and whether it closes to ≤48 ms

Analysis only; no code changed. Branch `perf/re-proof5` @ d80b422. This doc gates
whether a *consensus-visible* (Fable + stateright) exec/consensus-overlap round is
needed, or whether the Layer-3 target is a node-local exec-throughput round.

Evidence base: `bodypush-ab-18c-2a0d207.md`, `l3verify-18c-06de732.md`,
`reproof5-18c-4f832e5.md`, `profiling-attribution.md`, `docs/l3-diagnosis.md`,
`docs/l3-persist-attribution.md`, `docs/l3-arrival-attribution.md`, and code:
`crates/torus-telemetry/src/view_metrics.rs`, `crates/torus-consensus/src/app.rs`,
`crates/hotstuff_rs/src/hotstuff/implementation.rs`,
`crates/hotstuff_rs/src/pacemaker/implementation.rs`.

Cap-400 loaded reference cell = `BP400-on` (bodypush-ab): view 115.6 ms, blk/s 8.07
(≈124 ms/blk wall), matched/s 2,512, **exec_queue pegged 64–66 in every loaded cell**.
The headline "~113 ms" is `torus_view_duration_seconds` at cap-400 (113.6 BP400-off /
115.6 BP400-on); it is the authoritative per-view wall.

---

## 0. The one architectural fact everything below turns on

There are **two CPU threads per node in the hot path**, plus bursty helper pools:

- **Consensus/algorithm thread** (HotStuff): receives proposals/headers, runs
  safety checks, inserts bodies, votes, collects QCs, commits (block-tree 3-chain),
  and at commit does a **blocking `send` into a 64-slot exec channel**
  (`app.rs:2311` `sync_channel(64)`; `dispatch_to_exec` `app.rs:4166-4220`, blocking
  branch `tx.send(msg)` at `:4220`).
- **Execution worker thread** (`torus-execution`, ONE thread, `app.rs:2312-2315`):
  drains the channel and runs `execute_committed_block` (`app.rs:988`) = the real
  state CPU work (verify, engine match/settle, save_books, flush, evm-resync,
  body_persist).
- Helper pools that steal cores from both: `TORUS_PARALLEL_SETTLE` settle workers,
  `TORUS_PARALLEL_BUCKET_HASH` (now =1, off), RocksDB compaction (`max_background_jobs`),
  the `torus-trade-writer` background CF writer.

**Consequence:** the `exec_*` histograms and the `torus_view_*` histograms live on
*different threads that run concurrently but contend for the same cores*. Under
CPU saturation the exec channel fills (pegged 64), so the consensus thread blocks on
`send` and **consensus advance is throttled to the single exec worker's drain rate**.
That is the mechanism behind the work-conservation law: shaving consensus-thread
latency (arrival, insert_persist, build) frees nothing, because the binding
constraint is exec-worker throughput, and `qc_collect` (the leader idling for a
quorum of replicas to free a core and vote) simply expands to absorb the slack
(bodypush-ab: arrival 58.7→21.3, persist 35.2→16.2, build 22.2→7.6, yet
qc_collect 72.5→**138.0** and view stayed 113.6→115.6).

---

## 1. Per-view WORK decomposition at cap-400 loaded

### 1a. What each histogram measures, and what may be summed

**`torus_view_*` (consensus thread; `view_metrics.rs`).** These are **nested,
overlapping spans of the same wall — they MUST NOT be summed:**

| histogram | span (view_metrics.rs) | overlaps / contains |
|---|---|---|
| `view_duration` | StartView→StartView (:69-82) | the whole view wall — **the total** |
| `proposal_arrival` | follower StartView→first ReceiveProposal(Header) (:126-135) | co-started clocks ⇒ **contains the leader's entire build+dispatch** (arrival-attribution §"co-started"); not pure wire time |
| `insert_persist` | ReceiveProposal→InsertBlock (:146-161) | header-first = fetch-then-persist window, mostly RTT (persist-attribution §1); a *sub-span* after arrival |
| `vote_delay` | ReceiveProposal→PhaseVote (:166-177) | 1.6 ms; overlaps insert_persist (vote precedes body insert on the header path) |
| `qc_collect` | leader Propose→UpdateHighestPC (:111-121) | **spans the successor view under rotation** — contains the *next* block's arrival+insert+vote; the dominant term and the "sink" |
| `propose_build`/`finalize` | StartView→own-insert→Propose (:89-105) | leader-only; disjoint pair that sums to `propose_delay` |

Summing these double-counts massively (e.g. qc_collect 138 ms already contains the
next view's arrival 21 + insert 16). **Only `view_duration` (113 ms) is the WORK
figure.** The others are diagnostic decompositions of *where inside the wall* the
consensus thread is blocked — and every one of them turned out to be blocked
*waiting on replica CPU*, not on its named subsystem.

**`exec_*` (exec worker thread; `app.rs`).** These are **sequential, disjoint spans
of one `execute_committed_block` call — they ARE summable** into the exec-worker
per-block CPU wall (each is `Instant::elapsed` around its stage; the stages run
strictly in series on the one exec thread):

| exec_* histogram | stage (app.rs) | cap-400 measured / est. |
|---|---|---|
| `exec_evm_seconds` | EVM validate+commit bundle (:1106-1162) | ~2–4 ms (est; few EVM txs) |
| `exec_verify_seconds` | `batch_verify_native_actions_cached` (:1169-1194) | ~2–6 ms (est; ~400 orders × ~µs–15µs, trust-cache dependent) |
| `exec_engine_seconds` | `NativeExecutor::execute_batch` ×2 = margin+match+settle+gov+fees+epoch (:1334-1358) | **~16 ms** (measured, l3verify) |
| `exec_save_books_seconds` | serialize dirty books (:1360-1365) | ~1–3 ms (est; resident books) |
| `exec_flush_seconds` | nonces + `flush_with_native_trie_stats` (root+state_write) + evm_resync (:1373-1463) | **~28 ms** (measured; of which `exec_root` ~7.5 ms, `exec_state_write` the rest) |
| `exec_body_persist_seconds` | block-body JSON → CF_BLOCK_BODIES (:1488+) | ~5–10 ms (est; profiling P1 share 10.1%) |

Summable exec-worker CPU wall ≈ **16 + 28 + verify ~5 + evm ~3 + body_persist ~8 +
save ~2 ≈ ~62 ms measured/estimated**, plus untimed loop overhead
(`persist_committed_block_durably`, dispatch prep, replay/nonce guard, resident-book
stash, deschedule between timed spans). With `exec_queue` pegged 64, the exec thread
is never starved, so its true per-block wall ≈ the view cadence ≈ **~110–124 ms/blk**
— i.e. the ~62 ms of *timed* exec work plus ~50 ms of untimed overhead **and
deschedule inflation** (the exec thread is repeatedly preempted by the consensus
thread, settle workers, and compaction on an oversubscribed 18 cores).

### 1b. Is `flush` on the view critical path?

**Yes — indirectly, and bindingly, via channel backpressure.** `flush` runs on the
exec worker *after* consensus commit, so no *single* vote or commit waits on any
*specific* block's flush. But `exec_queue` is pegged at 64: the channel is full, so
the consensus thread blocks on `dispatch_to_exec`'s `send` until the exec worker
drains a slot, and the exec worker's drain rate is set by its per-block wall, of
which flush (28 ms) is the largest single component. So flush caps the sustained
commit rate 1:1. **Flush is off the *latency* path of any one view but squarely on
the *throughput* critical path of every view** — and at cap-400 the system is
throughput-bound, not latency-bound (that is the whole work-conservation finding).

### 1c. What is NOT double-counted / what genuinely fills the 113 ms

The honest reconstruction: the 113 ms view wall is **one CPU's worth of the
exec-worker per-block chain**, because consensus is pinned to exec drain rate. The
real CPU consumers, ranked, are:

1. **flush (state-root + state-write) ~28 ms** — single-threaded, O(dirty state).
2. **engine match/settle ~16 ms** — serial match loop (settle already partly parallel).
3. **body_persist ~5–10 ms** (est), **verify ~2–6 ms** (est), **evm ~2–4 ms** (est).
4. **untimed exec-loop overhead + cross-thread deschedule ~40–50 ms** (residual;
   this is the CPU-contention tax, the thing engine/verify parallelism most relieves).
5. Consensus-thread work (validate sig at `validate_block`, insert, block-tree ops,
   message processing) — overlaps exec in wall-clock but competes for the same cores;
   work-conserved, so it shows up as qc_collect, not as a separable addend.

**Measurement gap (recommend before any overlap build):** the `exec_*` histograms
were only partially scraped at the cap-400 *pegged* cell (l3verify gives root 7.5,
flush 27.6, engine 16; verify/evm/body_persist/save not tabulated there). A single
pegged-cell scrape of all `exec_*` + `exec_queue_depth` closes the ~40 ms untimed
residual and confirms the deschedule-tax hypothesis. This is the highest-value cheap
next measurement.

---

## 2. Vote-path inventory — what a replica MUST do before voting

Production path is **header-first** (`broadcast_proposal_as_header`,
implementation.rs:698; all propose sites use it). `on_receive_proposal_header`
(implementation.rs:1803) does, in order, **before the vote leaves**:

1. Equivocation check against `seen_proposals` (:1810-1847).
2. Header integrity: `block_hash == Block::hash(height, justify, data_hash)` (:1849-1857).
3. Justify correctness + safety: `justify.is_correct` + `safe_pc` (or the
   pending-bypass lock clause `safe_pc_lock_clause`) — a pure predicate over the
   block tree, **no execution** (:1873-1908).
4. **Lock advance before vote** (P0 safety): `update_locks_only(&justify)` — a
   non-sync RocksDB batch (:1982).
5. **VOTE** + `set_vote_state_atomic` (non-sync batch) + send (:1984-2028).

**Only after the vote is sent** does it obtain/insert the body (:2040-2066):
`try_insert_body` → `app.validate_block` (sig-verify + durability + ancestry;
`app.rs:3343`) → `block_tree.insert` → `block_tree.update` (commit walk). The actual
state execution (engine/settle/EVM/flush) happens **later still**, on the exec worker,
after consensus commit dispatches it.

### The precise answer

> **The vote waits on data availability of the HEADER + safety checks + a lock
> advance — NOT on execution, NOT even on body availability, NOT on `validate_block`.**
> On the header-first path the replica votes at ~1.6 ms (`vote_delay`), before the
> body is fetched, before sig-verification of the body's actions, and long before
> the block is executed (exec is async, post-commit, on another thread). Data
> availability that the vote *does* require is only the header's own integrity and a
> cryptographically-verified justify QC. `validate_block` (which runs post-vote in
> `try_insert_body`) does signature verification and durability/ancestry checks but
> **still not** engine/settle/EVM — those run only in `execute_committed_block` after
> commit.

**Where exec of N serializes against consensus of N+1.** Not on the vote, and not on
a per-view latency edge. It serializes at exactly two places, both throughput edges:
(a) the **blocking `send` into the 64-slot exec channel** at commit — consensus may
lead exec by at most 64 blocks, then stalls (`dispatch_to_exec`, app.rs:4220); and
(b) **shared cores** — the single exec worker (engine+flush CPU) competes with the
consensus thread, so votes for N+1 land "when a replica CPU frees up," i.e. between
exec bursts. The header-first pipeline has *already* decoupled the vote from exec;
what remains coupled is the sustained commit rate, bounded by exec-worker throughput.

**What the proposer waits on before proposing.**
- Parent body in-tree: `enter_view`→`produce_block` needs `highest_pc.block` present;
  if `block_height(&highest_pc.block)` is `None` the proposal is **deferred**
  (`proposal_deferred`, implementation.rs:122/537-553), retried on the ≤10 ms
  algorithm-loop cadence. This keys on *insert* (data availability), not exec.
- **`TORUS_PROPOSER_EXEC_WATERMARK`** (S470 Piece 2, implementation.rs:232-259):
  the leader **yields** (defers) its slot when `exec_backlog = pending_headers.len()
  + deferred_bodies.len()` exceeds the watermark. **Default OFF (0/unset = never
  yields, byte-identical to today).** Node-local, always safe (declining to propose
  == a slow leader the pacemaker already handles). Keys on the *local* exec backlog.
- **`commit_lag_backoff_cap` = 8** (S470 wedge fix, fleet-uniform genesis field,
  `torus-types/src/lib.rs:1180`; pacemaker `implementation.rs:110-151`): lengthens
  view deadlines as a function of **`highest_qc_view − committed_view`** (GRACE=4,
  exponent = `max(stall_exp, min((qc − committed) − 4, lag_cap))`; stateright
  `stateright_lockstep_backoff.rs:184-204`). This is the signal that **must stay
  visible**: during the commit wedge `view − qc` stays small while `qc − committed`
  races ahead; the backoff reads `qc − committed` to give exec time to drain without
  the pacemaker timing views out into a cascade. **Any overlap change that hides or
  compresses the qc−committed lag re-arms the S470 wedge and is consensus-visible.**

---

## 3. Closure arithmetic — does ≤48 ms fall out of items 1+2?

Assume the two in-flight parallelism streams land:
- **engine 16 → ~3 ms** (parallel match across independent markets).
- **verify parallelized**: cap-400 serial verify est. ~2–6 ms → ~1 ms.

Direct exec-worker CPU removed: **~13 ms (engine) + ~4 ms (verify) ≈ 17 ms.** Plus a
second-order gain: those two were the most parallelizable consumers, so freeing
~15 idle cores to them also relieves the ~40–50 ms deschedule tax on the *remaining*
serial spans (flush, body_persist). Optimistic model:

```
per-view wall today                              ~113 ms
 − engine parallelism            −13 ms
 − verify parallelism            − 4 ms
 − deschedule relief (est)       −10 to −20 ms      (freed cores, less preemption)
 ------------------------------------------------
 projected per-view wall         ~76 to ~86 ms      (ESTIMATE)
```

**≤48 ms does NOT close on items 1+2.** The residual is dominated by:

1. **flush (~28 ms, single-threaded state-root + state-write)** — untouched by
   engine/verify work, now the single largest consumer and still 1:1 on the
   throughput critical path.
2. **exec-loop untimed overhead + body_persist (~15–20 ms est)** — persist,
   dispatch prep, JSON body write.
3. **the single-exec-thread serialization itself** — even with cheaper stages, one
   thread runs verify→engine→save→flush→resync→persist in series per block.

Ranked residual consumers after items 1+2: **flush ≫ body_persist ≈ exec-loop
overhead > evm > (engine, verify now small)**.

---

## 4. Overlap options, ranked

The decisive finding from §2: **the vote already does not wait on exec, and
consensus already overlaps exec by up to 64 blocks.** So the classic
"vote-before-exec" consensus relaxation is a no-op here — it is already the case.
The binding constraint is **exec-worker throughput on one thread + core contention**,
which is **node-local**. This reframes the ranking: the top levers are NOT
consensus-visible.

| # | Option | est. ms saved (per view) | consensus-visible? | safety sketch | stateright needed |
|---|---|---:|---|---|---|
| **1** | **Parallelize / pipeline `flush`** — split state-root hashing across dirty buckets ONLY when the set is large (adaptive; today bhash=1 because at ~226 buckets spawn cost dominates, l3-diagnosis §1), and/or move `state_write` + `body_persist` + evm_resync to an async post-flush stage so the exec critical chain is engine+root only | ~15–25 | **No** (exec is already async post-commit; per-block state is deterministic and commit order preserved) | flush(N) must complete before N+1's engine reads N's committed state; async-write must fence before the applied-height marker so crash-replay stays correct (the marker fold at app.rs:1383-1420 already gates this) | No — node-local determinism; existing exec crash-replay tests cover it |
| **2** | **Two-stage exec pipeline**: overlap `flush(N)` with `engine(N+1)` on a second exec worker, with snapshot-isolated reads for N+1 | ~15–28 (hides the smaller of engine/flush) | **No** | N+1's engine must read N's *post-commit* state; requires either flush(N) fenced before engine(N+1) reads (serializes, no win) OR an MVCC/overlay snapshot so N+1 reads N-1 base + N overlay. Non-trivial; the win is real but needs a state-snapshot design | No consensus stateright; needs a determinism/replay test for the snapshot path |
| **3** | **Engine parallelism across markets** (mission item 1) + **parallel verify** (item 2) | ~17 (engine −13, verify −4) | **No** | markets independent ⇒ event order preserved per market; determinism proof required (settle already parallel under PARALLEL_SETTLE) | No — node-local determinism proof only |
| **4** | **`TORUS_EXEC_NONBLOCKING_DISPATCH` + deeper channel** (already gated, app.rs:4177) | ~0 mean (variance only) | **No** | strict-order park preserves commit order; bounded by `DEFERRED_EXEC_READY_BYTES`. Smooths transient exec spikes; does NOT raise mean throughput (still one exec thread) | No |
| **5** | Vote earlier relative to exec | **0 — already done** | n/a | header-first already votes at +1.6 ms, pre-exec, pre-DA-of-body | n/a |
| — | *Deepen consensus lead over exec past 64 (raise the channel bound / weaken backpressure)* — **do NOT** | negative | **Yes** | would let qc−committed grow unbounded, re-arming the S470 wedge and OOM; the 64-cap + watermark + commit-lag-backoff exist precisely to bound this safely. Explicitly out of scope (would touch commit-waits-on-exec) | — |

---

## Verdict (the gate decision)

- **Items 1+2 do NOT close the ≤48 ms budget.** Best-case with engine→3 ms and
  parallel verify lands per-view work at **~76–86 ms (estimate)**, because **flush
  (~28 ms single-threaded) plus exec-loop overhead survive untouched**, and one exec
  worker still serializes the per-block chain.
- **The vote already waits only on header integrity + justify safety + a lock
  advance — never on execution, body availability, or `validate_block`.** Exec is
  async and post-commit; consensus already overlaps it by up to 64 blocks. So the
  *consensus-visible* exec/consensus-overlap round (Fable + stateright) is **NOT the
  lever** — voting cannot move earlier than it already is.
- **A further round IS needed, but it is node-local, not consensus-visible.**
  Top-ranked lever: **flush parallelism / pipelining (Option 1)** — adaptive
  large-dirty-set state-root parallelism plus moving state-write/body_persist/resync
  to an async post-flush stage — combined with engine+verify parallelism (Option 3).
  These together plausibly reach the ~48 ms neighbourhood without touching the
  commit-waits-on-exec contract or the qc−committed backoff signal.
- **The only consensus-visible constraint to respect is a negative one:** keep the
  qc−committed lag visible and the 64-deep exec backpressure intact (S470 wedge).
  Do not raise the channel bound to "overlap" more.
- **Cheap prerequisite:** scrape all `exec_*` + `exec_queue_depth` on one cap-400
  pegged cell to pin the ~40 ms untimed exec-loop residual before building.
