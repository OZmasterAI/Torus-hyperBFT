# Design — asynchronous / off-critical-path native trie + mirror maintenance

**Branch:** `perf/book-digest-3c` @ `31a5739` · **Status:** DESIGN (read-only investigation; no production code written)
**Verdict up front:** the async worker is the *wrong first move*. See §3 and §5.

---

## 0. TL;DR for the impatient

Two corrections to the mission premise, both verified below:

1. **The 100 ms number is real but belongs to a different operating point than the 199 ms
   chain it is being divided into.** `root_seconds = 100–113 ms` is measured in the
   **uncapped** reproof5 cells (`R190/R750/R1000`, ~27k orders/block, **1.3–1.6 s block wall**,
   0.62–0.76 blk/s). The `~199 ms` loaded-block exec chain is the **cap-400** l3scrape cell,
   where `root_seconds = 9.1 ms`. At the gate-relevant operating point, root is **~7 % of the
   residual chain (~9 ms of ~125 ms)** — roughly 3× `body_persist`, which measured NEUTRAL.
2. **Nothing in production reads the thing this 100 ms maintains.** `flagged_native_root` —
   the sole production accessor of `persisted_native_root` — has **zero production callers**.
   Both of its callsites live in functions (`build_block_with_native`,
   `validate_block_with_native_inner`) that are only reached from `native_bridge_tests.rs`.
   `CF_NATIVE_TRIE` / `CF_NATIVE_HASHED` are, today, **write-only column families**.

Consequence: the cheapest and largest win here is not "move the work off-thread" but
**"stop doing the work"** behind a flag, with the already-existing `build_native_trie_to_cf`
rebuild as the recovery path. That recovers **100 %** of `root_seconds` for ~1 % of the
engineering surface of the async design, with **no** crash-atomicity change, **no** queue,
**no** ordering discipline, and **no** cache-ownership migration.

The async design is still specified below (§4 Option A) because it is the correct shape
*if and when* a live root consumer is reintroduced — but it should not be built now.

---

## 1. Fact verification (each fact independently confirmed this session)

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | `flush_with_native_trie` at `backend.rs:558`, body `flush_with_native_trie_stats` at `:601`; one atomic batch; atomicity intent documented `:549-557` | **CONFIRMED** | `backend.rs:558-560`, `:601-607`; single `WriteBatch` built at `:612`, state appended `:613`, trie ops appended `:653-661`, marker appended `:686-692`, one `target.write(batch)` at `:695` |
| 2 | `apply_native_dirty` is O(dirty keys), not O(total state) | **CONFIRMED** | `native_trie.rs:1037-1057` groups the dirty map by bucket; `run_buckets` (`:914`, called `:1108`) iterates only dirty buckets; path propagation (`:1158-1178`) walks only the `changed` set × `TREE_DEPTH`. Empirically: reproof5 reports 1,654–1,988 dirty buckets/blk vs a 65,536-bucket space |
| 3 | `CF_NATIVE_TRIE` / `CF_NATIVE_HASHED` are read only by `native_trie.rs` | **CONFIRMED, and stronger than stated** | Repo-wide grep outside `native_trie.rs`: `cf.rs:152,158,203-204` (constants), `db.rs:100` (prefix-extractor config), `lib.rs:215,907` (telemetry help strings), `app.rs:482` (comment), `app.rs:7522-7523` (inside `#[test] async_post_flush_state_identical_over_sequence`), plus `torus-bridge/tests/*` and `torus-integration-tests/tests/chaos.rs` — **all tests**. **No production reader.** |
| 4 | `state_root` excluded from the header preimage; live proposer copies parent's value; catch-up passes `skip_state_root_check = true` | **CONFIRMED** | `torus-types/src/lib.rs:388-401` (field doc), `:437-443` (`canonical_header_bytes` NOTE — field not hashed); `app.rs:3350` `state_root: parent_header.state_root`; `validator.rs:98-105` → `validate_block_inner(..., true)`, called from `app.rs:1140` |

### 1a. CORRECTION — the root has no live consumer (new fact, not in the brief)

`persisted_native_root` is called from exactly one production site outside `native_trie.rs`:
`torus-bridge/src/state_root.rs:80`, inside `native_root_routed`, which is reached only via
`flagged_native_root` (`state_root.rs:96`). `flagged_native_root` has exactly two callsites:

- `proposer.rs:263` — inside `build_block_with_native` (`proposer.rs:181`)
- `validator.rs:454` — inside `validate_block_with_native_inner` (`validator.rs:356`),
  reachable via `validate_block_with_native` (`:336`) / `validate_block_with_native_for_catchup` (`:347`)

A repo-wide grep for callers of those four public entry points returns **only**
`torus-bridge/tests/native_bridge_tests.rs:412,414,665,678`. The live proposer is
`TorusApp::produce_block` (`app.rs:3352`), which copies the parent root; the live validator
path is `validate_block_for_catchup` → `validate_block_inner` (`validator.rs:107`), whose
root call at `:192` is `compute_post_bundle_state_root_with_updates` — the **EVM** trie
(`CF_TRIE_*` / `CF_HASHED_*`), never `flagged_native_root`. That path is additionally gated
on `if has_evm` (`app.rs:1139`), which is false for every block in the native-only bench.

Note also that `incremental_state_root_enabled()` (`state_root.rs:24-29`) **defaults to
`true`** (`Err(_) => true`). The comment at `app.rs:2364-2371` ("…while the full-scan native
root stays primary until `TORUS_INCREMENTAL_STATE_ROOT` is enabled") is **stale**. This does
not change the conclusion — the routed function is unreachable either way — but it is a
latent trap for anyone reasoning about defaults, and should be fixed as a drive-by.

**Therefore:** the ~100 ms (uncapped) / ~9 ms (cap-400) of `root_seconds` currently buys
*optionality only* — it keeps the incremental base warm for a consumer that does not exist.
That is a legitimate thing to want, but it is a much weaker justification for a
crash-safety-breaking async subsystem than "45 % of flush."

### 1b. CORRECTION — the measurement arithmetic mixes two operating points

| source | cell | orders/blk | blk/s | root | state_write | flush | loaded chain |
|---|---|--:|--:|--:|--:|--:|--:|
| `reproof5-18c-4f832e5.md:33-40` | R190/R750/R1000 (**uncapped**) | ~27k | 0.62–0.76 | **100/113/104 ms** | 113/99/102 | 231/229/220 | ~1,300–1,600 ms |
| `devnet/wsl/results/l3scrape-18c-b1aba10.md:16-33` | S400_OFF2 (**cap-400**) | ≤400 | 8.43 | **9.1 ms** | 23.8 | 35.8 | **~199 ms** |
| `cacheab-18c-13bd833.md:18-34` (latest/best) | cap-400 + level-hash cache | ≤400 | — | ~9–10 ms | dominant | 38.3 | **~125 ms** |

(The brief cites the l3scrape doc as `docs/l3scrape-18c-b1aba10.md`; its actual path is
`devnet/wsl/results/l3scrape-18c-b1aba10.md`.)

- "~45 % of flush" is true **only** in the uncapped cells.
- "~8 % of the ~199 ms loaded-block exec chain" takes the numerator from the uncapped cells
  and the denominator from cap-400. The correct cap-400 figure is **9.1 / 199 = 4.6 %**, and
  against the current best config **~9 / 125 = 7 %**.
- In the uncapped cells, removing 100 % of root moves 0.72 blk/s → ~0.77 blk/s against a
  21.0 blk/s gate. Irrelevant.
- `reproof5-18c-4f832e5.md:63-72` (Verdict item 4) explicitly designates **capped cells as
  "the rational gate play"**, and the entire cacheab campaign runs cap-400. The cap-400
  number is the one that matters.

**`root_seconds` is therefore not "the largest node-local performance slice available."** At
the gate operating point the ranked residual is engine ~42 ms > state_write ~24 ms >
save_books ~37 ms ≫ root ~9 ms (`re-proof5-context.md:114-115`, `cacheab:32-34`).

---

## 2. Why the two historical rejections are (and are not) stale

### 2a. `docs/plans/incremental-state-root.md:37-41` — Option C, "still O(state), a band-aid"

**Genuinely stale.** That objection was written when the native root was a **flat keccak over
all entries of 6 CFs** — see the same doc `:9` and `:19` ("Native root has **NO
merkleization** — a flat keccak over all entries"). Relocating an O(total-state) scan to a
background thread just moves an unbounded cost, correctly rejected.

Since then Option A **shipped**: `native_trie.rs` implements the bucketed Merkle trie
(A2.1/A2.2, memory `07794c3b80cc6a32`), and `apply_native_dirty` (`native_trie.rs:1037`) is
O(dirty buckets × TREE_DEPTH), proven in vivo at 1,654–1,988 dirty buckets/blk against a
65,536-bucket space (`reproof5:33-40`). The premise of the rejection — "still O(state)/block"
— **no longer describes the code**. Deferring a bounded O(dirty) unit is a different
proposition from deferring an unbounded O(state) scan.

The same doc's `:47` already anticipated this: "Option C is worth folding in
*opportunistically* … but C alone is not a cure." Now that A is the cure and has landed, C's
opportunistic half is the only part left. So: rejection stale, **but** the thing it now
guards has also shrunk by ~7× (root fell 750 ms → 100 ms uncapped per `reproof5:33-40`), which
is precisely why the remaining prize is small.

### 2b. `docs/design-flush-pipeline.md:88-99` — `state_write` stays sync on read-your-writes

**Not stale, but it does not apply here.** That doc's `:88-98` bullet lists the read-your-writes
consumers of the *state* half: nonces (`get_cf_raw(CF_NATIVE_NONCES)`, `app.rs:1260`),
balances/accounts via a fresh `NativeStateOverlay` for N+1, session/member fills. Those are
genuine, and none of them are proposed for deferral here.

Fact 3 establishes that the **trie half** has no such consumer: `CF_NATIVE_TRIE` and
`CF_NATIVE_HASHED` have no production reader at all, and §1a shows even the derived
`persisted_native_root` is unreachable in production. The read-your-writes hazard that forced
`state_write` synchronous is **structurally absent** for the trie/mirror CFs. That is the real
unlock, and it is correctly identified in the brief.

**However, the doc's *other* lesson is the one that binds.** `design-flush-pipeline.md`
deferred `body_persist` on exactly the same "no reader depends on this" reasoning (`:104-106`)
— and it measured **NEUTRAL** in vivo (`l3scrape:40-47`: "the deferral verifiably works
(body_persist 3.3 → 0.8 ms) but the stage is too small to matter and **the win is absorbed
per the work-conservation law**"). The brief argues this is a footnote because "this is 100 ms,
30× larger." Per §1b that scaling is wrong: at the same operating point where body_persist was
3.3 ms, root is **9.1 ms — 2.8×, not 30×**. The precedent applies almost directly.

**A second, sharper falsifier from the same doc.** `design-flush-pipeline.md:50-53` records
that `TORUS_PARALLEL_BUCKET_HASH` — thread-parallelism applied to *this exact work* — **loses**
at cap-400 dirty-set sizes: "p1 4.22 ms vs p4 6.47 ms; the scoped-thread spawn/join cost
DOMINATES." The bench standard consequently runs `TORUS_PARALLEL_BUCKET_HASH=1` (serial)
(`re-proof5-context.md:87`). If spawning threads to *do* this work costs more than the work,
adding a channel hop plus a ~250 KB payload handoff to *move* it is unlikely to net out
better at the same shape. This is direct, in-tree, measured evidence against the thesis.

---

## 3. The noise floor kills the cap-400 case regardless of design

From `l3scrape:5-8`, the OFF and OFF2 arms of the *same configuration* on the *same binary*
measured **8.43 vs 7.46 blk/s — a 12 % spread at n=1 cell**, plus a first-cell cold-cache
anomaly severe enough to invalidate a whole cell (`l3scrape:52-58`).

12 % of a ~125 ms chain ≈ **15 ms**. The entire cap-400 prize is **~9 ms**. A win of 9 ms is
**below the resolution of the rig at n=1**, and would require ≥3 repeats per arm (or an
effect >12 %) to distinguish from cell noise. This is not a reason to disbelieve the physics;
it is a reason to disbelieve any single-cell A/B that claims the win — and it means the
async design cannot be *validated* even if it is built.

---

## 4. Design options

### Option 0 — Do not maintain what nothing reads (`TORUS_NATIVE_TRIE_MAINTENANCE`)

**Moves off-thread:** nothing. **Removes from the critical path:** 100 % of `root_seconds`.

`flush_with_native_trie_stats` skips the `apply_native_dirty` call entirely when the flag is
off, and instead sets a **staleness sentinel in the same atomic batch**. Native state + nonces
+ applied-height marker commit exactly as today.

- **Atomicity:** unchanged. One batch, same fence. `backend.rs:549-557` stays literally true.
- **Restart:** the sentinel makes the trie self-describing as invalid. `ensure_native_trie_built`
  (`native_trie.rs:296`) is widened from "is it non-empty?" to "is it non-empty **and not
  stale**?" and rebuilds via the existing `build_native_trie_to_cf` (`:245`) at boot
  (`app.rs:2371`). No new rebuild machinery.
- **Fail-safe by construction:** while stale, `native_root_routed` (`state_root.rs:77-79`)
  takes its **already-coded** full-scan fallback (`native_root_full`) — the determinism
  oracle. The system can be slow, never wrong. Contrast with async, where a lagging trie
  returns a *plausible but wrong* root.
- **Fencing (constraint d):** trivially satisfied. Both caches (`ensure_cache_usable:446`,
  `ensure_usable:725`) are only reached from inside `apply_native_dirty`, which is not called.
  The A2 oracle takes the full-scan branch.
- **Cost when a consumer appears:** one O(state) `build_native_trie_to_cf` at boot. Measured
  proxy: uncapped `native_root_full`-class work was ~750 ms/block pre-3c (`reproof5:33`), so a
  single full rebuild is sub-second at devnet scale — a one-time boot cost, not per-block.
- **Recovered share of root_seconds:** **100 %**, subject to §3's work-conservation caveat
  (the freed CPU still has to translate into throughput; see §6).

**Honest downside:** this is a *capability rollback*, not a *performance technique*. It trades
"incremental root always ready" for "root ready after a rebuild." That is exactly the right
trade **while nothing reads it**, and exactly the wrong trade the day something does.

### Option A — Fully async worker thread with a bounded queue

**Moves off-thread:** all of `apply_native_dirty` — bucket grouping, mirror prefix-scans, leaf
hashing, path propagation — plus a **separate** trie/mirror `WriteBatch`.
**Stays inline:** `state.append_to_batch` (native CFs + nonces) and the applied-height marker,
in today's atomic batch.

- **Capture (constraint b):** `state.native_dirty()` is already materialized under the pending
  read-lock at `backend.rs:616`. It is an owned `BTreeMap<(u8, Vec<u8>), Option<Vec<u8>>>`;
  today it is consumed inline, so it can simply be **moved** into the message rather than
  cloned — the capture is free. Send `(height, dirty)` on a bounded `sync_channel`.
- **Ordering (constraint b):** single producer (the one exec worker) + one FIFO channel ⇒
  strict height order by construction. This is the `BackgroundCfWriter` idiom already in-tree
  at `crates/torus-state/src/bg_writer.rs:30-83` (`sync_channel`, `send`, `queued_batches`,
  drain-and-join on `Drop`) — **reuse it structurally; do not invent a new pattern.**
- **Queue-full policy:** **BLOCK** (`SyncSender::send` backpressure), matching
  `design-flush-pipeline.md:110-115`. Backpressure is downstream of consensus, so it surfaces
  as a slower exec drain → qc−committed grows → the S470 `commit_lag_backoff_cap` engages.
  The signal stays visible, which `re-proof5-context.md:33-34` requires. **Do not "drop to
  sync"** — a sync apply would jump ahead of queued heights and silently break strict order.
  **Do not fail-stop** — this is an off-by-default perf flag and must never be able to halt a
  node.
- **Crash / restart (constraint c):** the documented atomicity **is broken by design**. Need a
  second marker `META_NATIVE_TRIE_APPLIED_HEIGHT` in `CF_CONSENSUS_META`, written by the
  worker **in its own batch** so trie ops and trie-height commit together. At boot, if
  `trie_height != native_applied_height`, the intervening dirty sets are **gone** (they lived
  only in RAM). There is no incremental repair — the only recovery is a full
  `build_native_trie_to_cf`. So: **every unclean shutdown pays an O(state) rebuild**, and the
  code path that detects and triggers it is new.
- **Fencing (constraint d):** three consumers compare against `persisted_native_root`, which
  under async reflects height M ≤ N:
  1. **`NativeTrieCache` / `NativeMemberCache`** — ownership **must move to the worker
     thread**. Today they are `Mutex<…>` fields on `ExecutionContext` locked by the exec
     thread (`app.rs:1433-1447`). Move them into the worker's owned state and delete the
     mutexes; then the worker is the sole writer of both the CFs and the caches, and
     `ensure_cache_usable` / `ensure_usable` are consistent by construction.
  2. **A2 determinism oracle** (`state_root.rs:80-88`) — must observe a quiesced trie.
  3. **Snapshot verify** (`snapshot.rs:139`) — uses `native_root_full`, so unaffected.

  Minimum honest answer for (2): add a `quiesce()` (drain + join outstanding work) on the
  worker handle and require it before any authoritative `persisted_native_root` read; and,
  until that handle is plumbed to `torus-bridge`, **fail-stop at boot if
  `TORUS_ASYNC_NATIVE_TRIE` and `TORUS_INCREMENTAL_STATE_ROOT` are both enabled**. Mutual
  exclusion is the small, correct answer; a plumbed quiesce handle is the large one.
- **Recovered share:** nominally 100 % of `root_seconds` off the exec chain; realistically
  less — see §6. Note the worker also acquires a *new* cost the sync path does not pay: a
  second `WriteBatch` + `db.write()` per block (WAL append, memtable insert), which is
  additional total system work, not just relocated work.
- **Surface:** new worker module, new marker + its boot check, rebuild-on-mismatch path,
  cache-ownership migration across two crates, quiesce/mutual-exclusion, plus tests.
  Realistically ~400–600 LoC touching `backend.rs`, `native_trie.rs`, `app.rs`, `cf.rs`,
  `state_root.rs` — the two hottest of which two other agents are editing concurrently.

### Option B — Pipelined one-block lag (block N's trie applied during N+1)

This is Option A with queue depth 1. It pays **every** cost of A — marker, rebuild path,
cache migration, fencing, broken atomicity — while capping the achievable overlap at one
block. The lag is bounded and known (exactly 1), but that does **not** buy an incremental
repair, because the dirty set still lives only in RAM; restart still full-rebuilds. **Strictly
dominated by A.** Not recommended.

> **Sub-variant evaluated and rejected:** make the dirty set durable (write it to a new CF
> inside the *atomic* state batch) so the trie can be repaired by replaying dirty sets from
> `trie_height+1`. This fixes the restart story — at the cost of adding ~250 KB/block
> (~1,680 entries × ~150 B) of writes to `state_write`, which is **23.8 ms at cap-400, the
> flush's dominant term** (`l3scrape:16-33`). You would be growing the larger stage to shrink
> the smaller one. Rejected.

### Option C — Narrow split: mirror/leaf-hash off-thread, tree fold inline

**Not viable as described.** The decomposition is real — `run_buckets` (`native_trie.rs:914`)
produces per-bucket `BucketOutcome { leaf, mirror_ops, member_final, changed }` (`:805-809`),
and steps 6–8 (`:1155-1200`) are the serial fold — but the fold **consumes the leaves**. An
inline fold must therefore *wait* for the off-thread bucket work, yielding zero pipelining
unless you defer by a block, at which point this is Option A/B. Speculative early start is
impossible: the dirty set does not exist until the engine finishes.

Worse, the bucket half is exactly what `TORUS_PARALLEL_BUCKET_HASH` already parallelizes, and
`design-flush-pipeline.md:50-53` measured it **losing** at cap-400 shape (p1 4.22 ms vs p4
6.47 ms). Option C proposes to add a thread hop to the sub-stage that is already known to be
too small to survive thread coordination overhead.

---

## 5. Recommendation

**Build Option 0. Do not build Option A now.**

| | Option 0 | Option A |
|---|---|---|
| root_seconds removed from exec chain | 100 % | 100 % nominal, less realized |
| Atomicity (`backend.rs:549-557`) | unchanged | **broken by design** |
| New durable markers | 1 sentinel (fail-safe) | 1 height marker (fail-*wrong* if mishandled) |
| Restart cost | O(state) rebuild, only when a consumer is enabled | O(state) rebuild after **every** unclean shutdown |
| Failure mode when stale | falls back to full-scan oracle → **slow, never wrong** | returns a lagged root → **plausible and wrong** |
| Cache ownership migration | none | required, cross-crate |
| Oracle fencing | trivial | quiesce handle or flag mutual-exclusion |
| Concurrent-edit collision risk | small | large (`backend.rs` + `native_trie.rs`) |
| Est. surface | ~40–60 LoC + tests | ~400–600 LoC + tests |

Justification: given §1a (no live consumer) the two options deliver the *same* headline win,
and Option 0 delivers it with a fail-safe failure mode and without touching the crash-safety
invariant that `design-flush-pipeline.md` and `T156-F1` were both written to protect. Given
§1b and §3, that headline win is ~9 ms at the gate operating point and is **below the rig's
n=1 resolution** — which is a strong argument for spending 50 lines on it, and a decisive
argument against spending 500.

**Sequencing.** Option 0 is not a dead end for Option A; it is the correct precursor. It
introduces the staleness sentinel, the "trie may be invalid → rebuild at boot" path, and the
`native_root_routed` fallback exercise — three of the four hard pieces Option A also needs.
If a live root consumer is ever reintroduced (see §7 / constraint e), Option A becomes the
right build, on top of Option 0's plumbing.

**Also recommended, independently and cheaply:** fix the stale comment at `app.rs:2364-2371`,
which claims `TORUS_INCREMENTAL_STATE_ROOT` is off by default when
`state_root.rs:24-29` defaults it **on**.

---

## 6. Work conservation — when is the win real, and what would falsify it?

The win is **real** only if all three hold:

1. The exec worker thread is the binding constraint (it is — `l3scrape:35-37`: exec_queue
   pegged 49–66 during bursts).
2. The removed work does not reappear as contention elsewhere. **This is where it fails.**
   `l3scrape:16-24` measures engine at 43.0 ms in vivo vs ~16 ms µbench — **~2.7× deschedule
   inflation** — i.e. the 18-core box is already CPU-contended with 3 nodes + load-gen. Under
   Option 0 the freed cycles go back to a contended pool and partially reappear as faster
   engine/save_books, which *is* a real win; under Option A the work is merely relocated onto
   the same contended pool and mostly **does not disappear at all**. This asymmetry is a
   further argument for 0 over A.
3. The effect exceeds the measurement noise floor. **It does not** at cap-400 (§3: ~9 ms prize
   vs ~15 ms n=1 spread).

**What would falsify the win:** `exec_root_seconds` drops to ~0 (proving the change works)
while `worst-60s blk/s` and `matched/s` do not move outside the OFF-vs-OFF2 control spread.
That is exactly the shape of the async-`body_persist` result (`l3scrape:40-47`) and is the
single most likely outcome.

**The measurement that would settle it** (rig unavailable this session — defined, not run):

- **Rig:** 18c, one devnet, single binary, env-gated A/B (no rebuild between arms) per
  `re-proof5-context.md:161`. Port guard per `:157`.
- **Config:** cap-400 (`TORUS_NATIVE_ORDERS_PER_BLOCK_CAP=400`) with the bench-standard block
  (`re-proof5-context.md:86-88`) + level-hash cache default-on. Cap-400 because §1b shows it
  is the gate-relevant point; **additionally run one uncapped arm** to size the ceiling,
  labelled as a ceiling probe, not a gate result.
- **Arms and order:** `WARM` (discard — l3scrape's first-cell cold-cache anomaly, `:52-58`),
  then `OFF, ON, OFF, ON, OFF, ON` interleaved, **≥3 repeats per arm**, ~10 min/cell.
  Interleaving, not blocking, so drift is not confounded with treatment.
- **Primary endpoints:** `worst-60s blk/s` and window-avg `matched/s` (the gate metrics).
  **Not** `exec_root_seconds`.
- **Secondary / mechanism:** `exec_root_seconds` → ~0 (proves the deferral engaged),
  `exec_block` mean, `exec_state_write_seconds`, `exec_queue` depth distribution, and per-core
  utilisation (`mpstat -P ALL`, which the rig does **not** currently capture — add it; it is
  the only direct test of hypothesis 2 above).
- **Pre-registered decision rule:** accept only if the mean `worst-60s blk/s` improvement
  exceeds the pooled within-OFF standard deviation by ≥2σ. Given the observed 12 % n=1 spread,
  declare the experiment **underpowered and inconclusive** — not "negative" — if the point
  estimate is under ~10 %.
- **Counters, not the load-gen headline** (`re-proof5-context.md:159`).

---

## 7. Constraint e — header semantics (FLAGGED, scoped OUT)

If `state_root` is ever re-added to the header preimage (`torus-types/src/lib.rs:396-401`
field doc; `docs/plans/s442-consensus-findings-and-limitations.md:102-105`), an asynchronously
maintained root has a **defined lag `N−k` that becomes consensus-visible**: every validator
must agree on `k`, and `k` must be stable across restarts, catch-up, and backpressure — but
under Option A `k` is *precisely the queue depth*, which varies with load. That is a
fleet-uniform protocol parameter, not a node-local tuning knob, and it interacts with
`validate_block_for_sync` / catch-up semantics.

**Scoped out. Do not design blind.** The design-level consequence for this document: **Option
A's variable lag is incompatible with a consensus-visible state root as currently framed.**
Option 0 is not — it makes the root *unavailable* (fail-safe fallback) rather than *lagged*.
This is a further, structural reason to prefer 0.

---

## 8. Implementation steps (Option 0)

Naming, gating, and test strategy. **No code is written by this document.**

**Env var:** `TORUS_NATIVE_TRIE_MAINTENANCE` — unset or `1` = maintain (**exact-today**);
`0` = skip. Follows the `OnceLock` reader idiom of `native_root_cache_enabled()` /
`parallel_bucket_hash_threads()` / `bucket_hash_min_buckets()` in `native_trie.rs`. Default-off
in the sense required by constraint (a): **default = today's behaviour, byte-identical roots.**

**New constant:** `META_NATIVE_TRIE_STALE` in `crates/torus-state/src/cf.rs` (alongside
`META_NATIVE_APPLIED_HEIGHT`), a `CF_CONSENSUS_META` key. Value `[1u8]` = stale. **No new CF.**

1. **`native_trie.rs`** — add `native_trie_maintenance_enabled() -> bool` (OnceLock reader);
   add `mark_trie_stale(batch, db)` / `is_native_trie_stale(db) -> Result<bool>`; widen
   `ensure_native_trie_built` (`:296`) to rebuild when `is_native_trie_built() == false
   || is_native_trie_stale() == true`, and to clear the sentinel **inside**
   `build_native_trie_to_cf`'s batch (`:245-282`) so rebuild-and-unstale is atomic.
2. **`backend.rs` `flush_with_native_trie_stats` (`:601`)** — thread the flag through:
   when disabled, skip the `apply_native_dirty` block (`:650-680`) and instead call
   `mark_trie_stale(&mut batch, raw)`. `dirty_entries_by_cf` attribution (`:645-649`) is
   cheap and should be **kept** so telemetry still shows the dirty shape. `root_seconds`
   naturally reports ~0. Everything else — `append_to_batch`, the marker, the single
   `target.write` — is untouched. Read the flag **once at the `app.rs` production callsite**
   and pass it as an explicit parameter, mirroring how `bucket_hash_min` is threaded
   (`backend.rs:629`), so the differential-test entry stays env-free.
3. **`state_root.rs`** — extend the `native_root_routed` guard (`:77-79`) from
   `!is_native_trie_built(...)` to also cover `is_native_trie_stale(...)`, so a stale trie
   routes to `native_root_full`. Fix the stale default comment at `app.rs:2364-2371`.
4. **`app.rs`** — read the flag at the `flush_with_native_trie_stats` callsite (`:1453`);
   when maintenance is off, skip acquiring the two cache mutexes (`:1433-1447`) and pass
   `None`. `ensure_native_trie_built` at `:2371` already runs at boot and now also repairs
   staleness — **no new rebuild machinery**.
5. **Telemetry** — add a `native_trie_stale` gauge (0/1) next to the existing native-trie
   metrics in `torus-telemetry/src/lib.rs` so an operator can see the trie is not being
   maintained. Non-negotiable: a silently-stale trie must be visible.

### Test strategy

Precedent to follow: the mode-combo byte-identity tests in
`crates/torus-bridge/tests/level_rows_tests.rs:660-690, 800-830` and
`root_cache_tests.rs:239-260`, which run an identical block sequence across flag combos and
`assert_eq!` on `dump(&db, CF_NATIVE_TRIE)` / `dump(&db, CF_NATIVE_HASHED)` /
`persisted_native_root`.

1. **`native_trie_maintenance_default_is_exact_today`** (`native_trie.rs` test mod) — the
   parsed default is `true`; the flag-on path is bit-for-bit the current call.
2. **`maintenance_off_then_rebuild_matches_maintained_root`** (`torus-bridge/tests/`, new or
   appended to `root_cache_tests.rs`) — **this is the byte-identity proof.** Two DBs, identical
   block sequence: DB-A with maintenance ON; DB-B with maintenance OFF followed by
   `build_native_trie_to_cf`. Assert `persisted_native_root(A) == persisted_native_root(B)`
   **and** `dump(A, CF_NATIVE_TRIE) == dump(B, CF_NATIVE_TRIE)` **and**
   `dump(A, CF_NATIVE_HASHED) == dump(B, CF_NATIVE_HASHED)`. This works because
   `build_native_trie_to_cf` is documented idempotent and root-reproducing (`native_trie.rs:243-244`).
3. **`maintenance_off_leaves_native_state_byte_identical`** — the 6 native root CFs +
   `CF_NATIVE_NONCES` + `META_NATIVE_APPLIED_HEIGHT` are byte-identical between the two arms
   *before* any rebuild. Proves only the trie/mirror differ.
4. **`stale_trie_routes_to_full_scan`** (`torus-bridge/tests/`) — with the sentinel set,
   `flagged_native_root` returns `native_root_full`, and it equals the maintained root. Proves
   the fail-safe: **slow, never wrong**.
5. **`boot_rebuilds_stale_trie`** — set the sentinel, call `ensure_native_trie_built`, assert it
   returns `true` (a build happened), the sentinel is cleared, and the root matches the
   maintained arm.
6. **`crash_with_stale_trie_replays_clean`** (extend the `chaos.rs:321-372` pattern) — kill
   between flush and rebuild; assert `replay_committed` completes, applied-height is correct,
   native state is identical, no fail-stop. The atomic batch is untouched, so this should be
   a *confirmation*, not a new guarantee.
7. **Determinism sweep** — reuse the multi-arm harness at `native_trie.rs:2015-2055`
   (reference DB vs N flag-combo DBs) and add the maintenance flag as a new axis, asserting
   post-rebuild equality across every combination with `TORUS_NATIVE_ROOT_CACHE`,
   `TORUS_BUCKET_MEMBER_CACHE_MB`, and `TORUS_PARALLEL_BUCKET_HASH`.

**Ordering:** tests 2 and 4 are the RED-first pair — write them before any production change.

---

## 9. What I could NOT establish

1. **Current-HEAD `root_seconds` at cap-400.** The 9.1 ms figure is from `b1aba10`. HEAD is
   `31a5739`, which since then landed the asm keccak backend workspace-wide (`82523da`), the
   level-hash sponge cache default-on (`3a3a96d`), and preimage scratch-buffer reuse
   (`31a5739`). The asm keccak change plausibly reduced `root_seconds` further, making the
   prize **smaller** than 9 ms. **The single most important thing to re-measure before
   building anything.**
2. **Actual queue payload size.** The brief's ~1,680 entries × ~150 B = ~250 KB/block: the
   entry count checks out (`reproof5:44-50`: 1,318 positions + 238 balances + 124 order_books
   = 1,680), but I did not verify the ~150 B mean key+value size. Unverified.
3. **Whether the 18c box has spare cores at the bench shape.** The 2.7× engine deschedule
   inflation is suggestive but indirect. The rig does not capture per-core utilisation; §6
   proposes adding it.
4. **Whether any RPC / state-sync path exposes the native root to peers.** I confirmed
   snapshot verify uses `native_root_full` (`snapshot.rs:139`), not the persisted trie, and
   found no other production reader — but I did not exhaustively audit the RPC surface for a
   root-serving endpoint.
5. **Why `flagged_native_root`'s callers went dead.** `build_block_with_native` /
   `validate_block_with_native*` are fully implemented and test-covered but production-orphaned.
   Whether that is intentional (superseded by CTE) or an accident of the CTE migration is not
   established by the code alone, and **materially affects whether Option 0 is a cleanup or a
   regression.** Worth one question to the maintainer before implementing.
6. **Nothing was executed.** No build, no test run, no bench — read-only session, rig
   unavailable, and two agents are concurrently editing `native_trie.rs` and `backend.rs`, so
   all line numbers in this document should be re-confirmed before use.
