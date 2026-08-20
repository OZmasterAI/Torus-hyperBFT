# Block-latency campaign — 2026-08-20 (branch `perf/matched-200k`)

Goal, honestly stated: take the **full-block exec chain** at 10 markets down toward the
**empty-block cadence** (~65 ms idle), i.e. close the gap between what a *full* native block costs
on the serial exec thread and what an *empty* one costs. Two tracks were run:

- **A** — get fixed per-block costs off the serial exec thread (pipeline track).
- **B.1** — find the block cap / commit-lag operating point that gives the best *cadence* at
  flat matched/s (harness track), and answer whether **sub-100 ms full-block cadence** is reachable.

Everything below is node-side only. Nothing consensus-visible changed. Nothing was pushed.
Every number names the cell it came from; raw data is
`/home/18c/bench-results-matched/<label>/summary.json`.

---

## 1. Baseline on `1dcf339`

Bench worktree `/home/18c/projects/wt/matched-bench` detached at `1dcf339`, `dirty_files=0`.
**No build was needed**: `git diff --stat d7a073f 1dcf339` is docs-only (+211 lines, 1 file) and
`/home/18c/.cargo-target-matched/release/torus-node` md5 `33c423fc6d56bd3335767e4ca8c88443` already
equalled the `torus_node_md5` recorded in the carried `r9-merged-confirm-10m-r2` summary — the
on-disk binary *is* `1dcf339`'s code. Genesis md5 `478d698b…` and per-node `TORUS_*` env digest
`4f79dee6` identical across all three cells, so they are apples-to-apples.

| cell | dur | block ms (val0) | blk/s | idle blk/s | matched/s avg | best-60 | agree |
|---|---|---|---|---|---|---|---|
| `bl-base-10m-r1` | 120 s | **789.79** | 0.9 | 12.3 | 21,361.6 | 56,619 | AGREE |
| `bl-base-10m-r2` | 120 s | **969.24** | 1.4 | 28.7 | 40,497.9 | 51,773.4 | AGREE |
| `r9-merged-confirm-10m-r2` (carried, `d7a073f`, same binary) | 300 s | 1168.02 | 1.0 | 14.8 | 42,283.5 | 56,707.3 | AGREE |

**Primary metric baseline (120 s, n=2): mean 879.5 ms, spread 179.5 ms = ±10.2 %** (sample sd
≈127 ms, ≈14 %). This is the campaign's answer to the previously-unknown block-ms sd: *at n=2 on
120 s cells a single-cell block-ms delta under roughly 15 % is unreadable.*

Phase split, `bl-base-10m-r2` (the exec-saturated rep, busy 0.828, therefore the representative one):

```
block 969.24 = engine 441.35 (phase1_actions 90.27 + settle 191.64 [A 83.25 / B 100.56 / cf 4.10]
                              + match 120.59 + margin 34.21 + untimed 4.53)
             + flush 258.48 (root 99.30 + state_write 145.61 [build 14.83 + db 130.78])
             + save_books 138.55 + load_books 59.40 + verify 29.93 + replay_guard 4.63
             + body_persist 0.31 + residual_untimed 36.59;   dirty buckets/flush 2644
```

Two facts from the baseline that shaped every later comparison:

1. **Within-cell drift.** Block ms roughly doubles inside a single 120 s cell as the book deepens:
   `bl-base-10m-r1` early_60s 419.9 → late_60s 774.4 (resting 169k → 1.10M);
   `bl-base-10m-r2` 563.1 → 1262.8 (615k → 1.27M). Cell-mean block ms is therefore a function of
   cell *duration*. That is exactly why the 300 s carried cell reads 1168 ms against a 120 s mean of
   879.5. **120 s cells were only ever compared against 120 s cells.**
2. `matched/s` on `bl-base-10m-r1` (21.4k) is an **ingest artifact**, not exec: r1 submitted 79,751
   actions vs r2's 42,490 and paid 58.9k/node nonce-expiry evictions (vs 19.9k), leaving the exec
   thread 52.8 % busy vs 82.8 %. r1's *best-60* (56,619) is in fact the highest of the three cells,
   and matched-per-exec-block is within 3.5 % (43,552 vs 45,097). The >5 %-regression gate was
   carried against **40.5k (r2)**, not against 21.4k.

---

## 2. The design

`docs/perf/design-exec-pipeline-2026-08-20.md` (revision 2, 590 lines).

**One paragraph.** The serial exec thread runs, per committed block,
`verify → replay_guard → load_books → engine → save_books → flush → resync → body_persist`.
The **engine cannot be pipelined** — it *is* the state transition. What can come off the thread is
the fixed post-engine tail. The design moves the *whole* `flush_with_native_trie_stats(N)`
(build + native-trie root + applied-height marker, one atomic `WriteBatch` — the crash-safety
fence stays intact) onto a dedicated flush worker **W**, handed off over a **`sync_channel(0)`
rendezvous** so that at most one non-durable pending set exists at a time; `engine(N+1)` then reads
through a `NativeStateOverlay` whose parent is *always* the previous job's frozen pending set
(a `Flush` job, or a 1-key `Marker` job for empty blocks), which makes the layered read return
byte-identical bytes to the serial read while every height `< N` is already durable. Around that
sit the supporting items: an E-owned `exec_applied` watermark for the skip-check, serial boot
replay with W constructed afterwards, epoch-boundary blocks forced serial with `resync_evm_accounts`
run on W after its own write, and a resident-book holder that advances on untouched blocks so the
staleness guard stops forcing a rebuild after every empty block.

**Review verdicts.** Two adversarial reviews — *crash-consistency & ordering* and
*determinism & throughput* — both returned **needs-revision**, 9 findings each. Revision 2 resolves
all of them in a new §8 table. The load-bearing ones: **F1/F6** (a `sync_channel(1)` buffer would
allow two non-durable pending sets against a single parent layer → stale reads → per-node
divergence) forced the rendezvous `sync_channel(0)`; **F2** (`resync_evm_accounts` is *not* a
no-op at epoch boundaries) forced boundary blocks serial; **F3** (skip-check reading the durable
marker would re-execute a re-delivered height inside the depth window) forced the E-owned
watermark; **F4** (boot replay through the same function) forced `pipeline_allowed=false` on
replay; **F5/F7** (Marker jobs with no pending layer) forced the 1-key marker layer, without which
*every* native-after-empty block rebuilds and barriers; **F9** (`fold_header` blocks silently
disabling the parent-link check) forced `durable.header && durable.body` on the fast path.
**F8** documented the consensus-thread `epoch_validator_set_updates` CF_STAKING writer as a
*pre-existing* consensus-visible hazard, in scope only via a static staking set + a differential
test. Arithmetic was also corrected: the r9-shape E-bound chain predicts **906 ms** (residual stays
on E), acceptance `chain_ms ≤ 940` at equal fills/native block, with
`fills_per_native_block` and `engine_ms_per_1k_fills` mandatory columns so a thinner-block artifact
cannot read as a chain win.

---

## 3. Per-round results

Δ columns are **vs the carried best at the time** unless the reason column gives a paired
same-binary ON/OFF number, which is always the honest one. Track/model follow the design's §6
assignment (pipeline → fable, instrumentation/harness → opus).

| rd | candidate | track | model | Δ blockMs | Δ matched | verdict | reason |
|---|---|---|---|---|---|---|---|
| 1 | `exec-chain-sub-100-attribution` | instrumentation | opus | −8.5 % | +43.2 % | **MERGED** (`6eab88d`) | `chain_ms + empty_block_ms == block_ms` to the printed digit on all 9 node-cells (e.g. 893.31+0.20=893.51); `native_blocks_counter == executed_native_blocks` 128/178/189; diff is 4 `Instant::now()` + histograms + one counter, no state/ordering change. It is the ruler the campaign needed: it showed that moving **100 %** of flush + save_books pass 2 off the chain still leaves ~490–530 ms, so engine work is mandatory for 100 ms. |
| 1 | `resident-books-untouched-advance-restack` | pipeline | fable | −1.5 % | +44.1 % | not merged | `load_books` 80.5/59.4 → **0.08** ms/blk met acceptance, but the same-binary OFF control did not reproduce the baseline's 59–80 ms (`off-r2` load_books 0.60, 24 stale WARNs all inside the first 1.7 s of ramp), so the mechanism was not yet demonstrated on a paired arm. |
| 2 | `flush-1deep-worker-applied-height-fence` | pipeline | fable | **−31.8 %** | +24.9 % | **MERGED** (`fe4b40a`) | Same binary `2723f158…`: ON 618.14 / 581.42 (mean **599.78**) vs same-day same-binary OFF 853.88 / 799.08 (mean **826.48**) = **−27.4 %** paired. `pipelined_ms` 252.5/237.0 equals each cell's flush ms to within 2 ms, `handoff_wait` 3.7/3.2 ms, `flush_worker_depth_max` exactly 1.0, exec busy 0.81→0.61, peak exec queue 48→7. matched/s 50,672 vs OFF 49,350 (+2.7 %). 11/11 pipeline tests, 8/8 resident-books, 4/4 layered-overlay re-run by the judge, incl. crash-before-write replay == serial dump and ON-vs-OFF 13-block state/root identity. |
| 2 | `resident-books-…-restack-restack` | pipeline | fable | −7.4 % | +13.3 % | not merged | OFF control *did* prove the mechanism (2 mid-run stale rebuilds/node, `resident_height=238 block_height=240 applied_marker=239`), but the cost of those rebuilds was only 0.29 ms/blk (`load_books` 0.40 vs 0.11); the 174 ms ON/OFF gap was spread across engine/flush/save_books = box variance, not the candidate. |
| 3 | `resident-books-…-restack-restack-restack` | pipeline | fable | −23.4 % headline / **−8.4 % paired** | +14.1 % | **MERGED** (`ac031c7`) | Paired same-binary same-session ON `632.60` vs kill-switch OFF `690.83` = −8.4 %, and the delta *is* the targeted phase leaving the chain: `load_books` 0.06 vs **65.74** ms/blk while engine (418.82 vs 418.39), save_books and flush are flat. `torus_exec_resident_rebuilds_total` 1/1/1 ON (startup only) vs 5/5/5 OFF; "resident books stale" 0 vs 4 per node. The −23.4 % headline is mostly cross-session drift and was **not** credited. |
| 3 | `exec-pipeline-crash-gate-and-summarize-fix` | harness | opus | +6.6 % | +5.7 % | **REJECTED** | The headline deliverable was unpassable *by construction*: `crash_gate()` deferred to `agreement_verdict` → `counters_equal` over **process-lifetime** Prometheus counters, which a SIGKILLed-and-restarted node resets. `crash-on-r1` reported FAIL on a node that behaved perfectly (replayed gap=1 from marker 134, `rewind_beyond_exec_queue` 0/2, `state_digest_equal=TRUE` and quiescent on all 3). Merging it would have installed a gate that blocks the `TORUS_EXEC_PIPELINE` default-ON flip forever. Its worker-aware `summarize.py` residual fix (−212 → +42 with block/chain/matched/agreement byte-identical) was good and was re-submitted. |
| 4 | `crash-gate-survivor-agreement-fix` | harness | opus | +6.4 % | −4.75 % | **MERGED** (`db02d93`) | Binary byte-identical to the best (`git diff ac031c7..fc23d8a` on `*.rs`/`Cargo.*` empty, md5 `bcd18bd` unchanged), so the +6.4 %/−4.75 % on two valid 300 s cells (946.33, 889.52; idle 22.7/16.2, AGREE, drained) is box noise. 52 pytest pass. Judge re-ran a fabricated-fork battery: baseline PASS; killed-node digest forged → FAIL; survivor digest forged → FAIL; killed-node block-hash forged → FAIL; survivor counter off → FAIL; killed-node panic line → FAIL. **First live `kill -9` PASS**: val1 down 1.09 s, rewind 0, worker reattached, all three on digest `7ff770ae416d`. |
| 4 | `phase1-actions-drift-attribution` | instrumentation | opus | +10.1 % | −4.65 % | not merged | Instrumentation-only, phase set identical to best. Delivered the verdict: `phase1_actions` on val0 goes **first 58.4 → last 741.6 ms (13.1×)** while resting orders go 579k → 1.62M (2.8×); unit cost 31.3 → 88.7 µs/cancelled-order. Verdicts `grows_with_run_length` + `grows_with_depth`, agreed by all 3 validators. Confirms `phase1_actions` is *not* a 10-market constant and must not be treated as a fixed lever. |

Cells discarded for rig reasons, not counted in any mean, listed for honesty:
`bl2-…-on-r2` (idle 8.8), `bl2-flush-1deep-…-on-10m-r1` (idle 0.4),
`bl3-…-10m-r2` (idle 1.5), `bl4-…-10m-r2` (idle 2.5),
`bl1-exec-chain-sub-100-attribution-10m-r1` (idle 2.5, kept only for the identity checks).

---

## 4. What merged

| round | merge head | what | kill switch | default |
|---|---|---|---|---|
| 1 | `6eab88d` | exec-chain attribution: `exec_chain_seconds`, `exec_handoff_wait_seconds`, `flush_worker_seconds`, `flush_worker_depth`, save_books drain/write split, native-vs-empty block counters; `summarize.py` gains `chain_ms`, `pipelined_ms`, `handoff_wait_ms`, `fills_per_native_block`, `engine_ms_per_1k_fills`, `gap_to_100ms` | — (telemetry) | always on |
| 2 | `fe4b40a` | flush worker W + `FrozenPending` layered overlay + rendezvous hand-off + applied-height fence on W (`crates/torus-consensus/src/exec_pipeline.rs`) | `TORUS_EXEC_PIPELINE` (`=1` enables; anything else disables — `exec_pipeline.rs:45-52`) | **OFF** — kept OFF because at merge time the live `kill -9` gate had not passed |
| 3 | `ac031c7` | `ResidentBooks::advance_untouched` on the non-native path (`native_executor.rs:1137-1150`) | `TORUS_RESIDENT_ADVANCE_UNTOUCHED` (`=0` disables) | **ON** — justified by tests + 2 agreeing ON cells + an agreeing OFF control |
| 4 | `db02d93` | crash gate judges the killed node on digest/hash/root and compares counters among survivors only; height-skew ⇒ `DIGEST_UNVERIFIED` not `DISAGREE`; worker-aware residual accounting | — (harness) | always on |

`TORUS_EXEC_PIPELINE` remaining **OFF by default is the honest state**: the flag now has a passing
crash gate (round 4), but only the *zero-rewind arm* was exercised live
(`replay_line_found=false`), so the flip is a backlog item, not a shipped one.

---

## 5. Block ms and phase breakdown, before → after

Like-for-like **300 s, 10 markets** — carried baseline `r9-merged-confirm-10m-r2` (`d7a073f`, serial)
vs `bl4-merged-confirm-10m-pipe` (`db02d93`, `TORUS_EXEC_PIPELINE=1`):

| phase (val0, ms/native blk) | before (serial, 1168.02) | after (pipelined, 868.09) | where it runs now |
|---|---|---|---|
| engine | 708.43 | 651.34 | E (unchanged by design) |
| ├ phase1_actions | 391.47 | 342.30 | E |
| ├ settle | 191.33 (A 63.24 / B 120.32 / cf 4.30) | 173.11 (A 65.83 / B 99.55 / cf 4.35) | E |
| ├ match | 88.61 | 99.63 | E |
| └ margin | 30.41 | 32.94 | E |
| save_books | 130.68 | 133.06 (drain 79.82 + write 53.23) | E |
| verify | 27.19 | 32.20 | E |
| replay_guard | 4.88 | 4.95 | E |
| load_books | 0.23 | 0.05 | E |
| residual_untimed | 34.75 | 45.96 | E |
| **flush** | **261.84** (root 87.95 + sw 162.18 [build 14.83 + db 147.35]) | **271.05 — OFF-CHAIN** (`pipelined_ms` 268.80) | **W** |
| handoff_wait | n/a | 1.20 | E waits on W |
| **exec chain** | **1168.02** | **867.51** (+ empty 0.58) | **−25.7 %** |

Exec busy 0.913 → 0.804; `flush_worker_depth_max` 1.0; `rocksdb stall_ms_per_native_block` 0.0;
AGREE, digest quiescent, drained, `rebuilds [1,1,1]`, 0 panics.

Two controls that keep this honest:

- The **serial path on the merged head** (`bl4-merged-confirm-10m`, same 300 s, flag not set) reads
  **1116.09 ms** vs the carried serial baseline 1168 = −4.4 %. So ~4 pp of the 25.7 % is
  resident-books + drift and **~21 pp is the flush moving to W**, which matches
  `bl2-merged-confirm-10m` exactly: 921.93 ms with engine unchanged (696.76 vs 708.43) and
  305 ms of flush on W.
- At **120 s** the same shape reads far lower on both arms (`bl-sweep-200-8` control mean
  **644.6 ms** vs the 120 s baseline mean 879.5 = −26.7 %), because `phase1_actions` is 106–119 ms
  at 120 s against 342–394 ms at 300 s. Never mix the two durations.

---

## 6. Cadence sweep (B.1) — `db02d93`, 7 cells, no builds

All cells: 10 markets, 120 s, RATE=76000, `EXTRA_ENV='TORUS_EXEC_PIPELINE=1
TORUS_COMMIT_LAG_BACKOFF_CAP=<b>'`, binary md5 `bcd18bdcec885ae8e6b6b86922e2c0ca` (byte-identical
to the round-4 confirm cell — nothing was built). Cadence is reported as
`phase_by_node.val0.native_blk_s`, not the headline `blk_s_avg`: the two identical control reps gave
headline 1.4 and 0.9 while `native_blk_s` reproduced to 4 % and shares its span with `block_ms`.

| cap | backoff | blockMs (val0 exec chain) | full-block cadence | matched/s avg | vs control | agree |
|---|---|---|---|---|---|---|
| 200 | 8 | 644.0 / 645.2 (mean **644.6**) | 0.919 / 0.879 blk/s (1112 ms) | 50,086 / 48,006 (mean 49,046) | — (control) | AGREE |
| **100** | **8** | **319.3 / 331.0 (mean 325.1)** | **1.949 / 1.931 blk/s (515 ms)** | **45,806 / 46,958 (mean 46,382)** | **−5.4 %** | **AGREE** |
| 50 | 8 | 167.9 | 3.309 blk/s (302 ms) | 36,669 | −25.2 % | AGREE |
| 50 | 2 | 166.3 | 3.214 blk/s (311 ms) | 35,499 | −27.6 % | AGREE |
| 25 | 2 | 114.9 | 4.611 blk/s (217 ms) | 22,974 | −53.2 % | AGREE |

Idle blk/s before each run: 27.0, 27.2, 18.7, 12.8, 23.8, 11.4, 22.9 — all healthy except
cap50/backoff2 at 11.4, whose cap50/backoff8 twin (idle 23.8) agrees with it to 3 %, so the cell
stands. Every cell AGREE on block hash + RPC state digest + counters, 0 panics, `drained=true`,
`rebuilds [1,1,1]`. No cadence candidate was rejected on correctness.

**Best operating point within −10 % of control: cap 100, backoff 8.** It doubles the full-block
cadence (0.90 → 1.94 blk/s, 1112 → 515 ms per full block) and halves the exec chain
(644.6 → 325.1 ms) for −5.4 % matched/s — inside the band and inside the campaign's ~7 % matched/s
noise. It is also the **fills/s peak of the whole sweep** (52.9k/s vs 50.1k at cap 200 and 43.7k at
cap 50), so the matched/s dip is scatter around a flat-to-better matching rate.

### The plain answer on sub-100 ms

**No. Sub-100 ms full-block cadence was not reached, and it is not close.** The best cadence
measured anywhere is **217 ms** (`bl-sweep-25-2`), 2.2× short — and it fails on two independent
counts:

1. **The exec chain alone still exceeds 100 ms at cap 25** (114.9 ms). Cutting the cap 8× from 200
   cut the chain only 5.6×, because the per-block *fixed* cost stops shrinking: at cap 25,
   `save_books` 32.9 + `verify` 7.3 + `residual_untimed` 9.4 + `replay_guard` 0.9 = **50.5 of the
   114.9 ms is fixed overhead**. Thinner blocks buy almost nothing more; the fixed costs have to
   come off the chain, and `save_books` is the obvious next one — flush already left.
2. **Cadence is not exec-bound at any cap tested.** `exec_thread_busy_fraction` is **0.53–0.64 in
   every cell** and `peak_exec_queue_depth` is **3–12 of 64 slots**. The exec worker is *starved*:
   consensus cannot hand it blocks fast enough. Cadence per full block stays ~1.7–1.9× the exec
   chain and that ratio does **not** improve as the chain shrinks.

**What bounds it: the commit path.** Not the commit-lag backoff — the cap50 backoff-2 vs backoff-8
pair is the direct test and it is a null result (166.3 vs 167.9 ms, 311 vs 302 ms cadence, 35.5k vs
36.7k matched, all inside noise), exactly as the code says: `commit_lag_exponent`
(`crates/hotstuff_rs/src/pacemaker/implementation.rs:1229`) is
`min((highest_qc_view − committed_view) − COMMIT_LAG_GRACE(4), cap)`, constantly 0 while the commit
frontier is within 4 views of the QC frontier; its only visible effect is the timeout count (23 at
cap 2 vs 41 at cap 8). And not the cap either, past 100: the cap *is* the cadence lever
(`actions_per_exec_block` tracks it exactly — 191/96/49/24.5, so blocks are genuinely full and the
mempool never limits) but below cap 100 it trades matched/s roughly proportionally. It is the
**committed-block interval**: empty blocks stay pinned at ~2.0–2.4/s regardless of cap, and total
committed-block rate tops out at 3.2 → 4.1 → 5.4 → **6.6/s** (cap 25), i.e. a per-committed-block
floor of 295 → 258 → 186 → **151 ms under load against 37–83 ms idle**, with commit-interval p95
fat throughout (2260 ms at cap 200, 508 ms at cap 25) and 18–76 view timeouts per cell. 100 ms
full-block cadence needs ~10 native + ~2 empty = **12 committed blocks/s**; the chain currently
tops out at 6.6.

---

## 7. Remaining backlog, ranked

Ranked by expected effect on the *now-binding* constraint (the commit path), then by exec-side
fixed cost, then by hygiene.

| # | item | why it is here |
|---|---|---|
| 1 | `consensus-timeout-attribution-under-pipeline` — attribute the 18–76 view timeouts and the 500–2300 ms commit p95 now that exec queue depth is 3–12/64 | The sweep says exec has ~40 % headroom at every cap; cadence is producer-side. This is the only item that can move cadence further, and it also explains why −27 % `chain_ms` converted to only +2.7 % matched/s. |
| 2 | Adopt **cap 100** as the default operating point and confirm it at 300 s | Free 2.16× cadence / 2× chain latency at flat fills/s, already measured on n=2 (`bl-sweep-100-8`, `-r2`). Needs one 300 s confirm before it becomes the campaign default. |
| 3 | `save-books-drain-off-chain` / `save-books-pass2-on-worker` (design candidate 4) | `save_books` is the largest remaining fixed cost on E (drain 79.8 + write 53.2 at 300 s; 32.9 ms of a 114.9 ms chain at cap 25). Gated on the pass-1/pass-2 production split the round-1 timers now provide. Must write into a **separate W-owned `books(N)` layer**, not `pending(N)`, to avoid contending with E's parent reads. |
| 4 | `exec-pipeline-default-on` — flip `TORUS_EXEC_PIPELINE` to default ON | The −21 pp win is behind a flag nobody sets by default. Blocked on #5. |
| 5 | `crash-gate-replay-gap-arm` — kill val1 while `exec_queue_depth > 0` and prove `rewind_beyond_exec_queue ≤ 2` with all-3 digest AGREE | The live gate has only ever passed the **zero-rewind arm** (`replay_line_found=false`). Prerequisite for #4. |
| 6 | `idle-out-of-band-fatal` — `run-cell.sh` refuses to run after 3 failed idle probes unless `FORCE_IDLE=1`; `summarize.py` writes `headline.idle_out_of_band` | Five cells this campaign were lost to the settling trap. Cheap, prevents a starved cell from ever passing as a measurement. |
| 7 | `settle-passB-attribution-and-cut` (96–133 ms, the largest single engine chunk) | Engine work is now unavoidable for further latency: the round-1 ruler showed that moving 100 % of flush + save_books pass 2 off the chain still leaves ~490–530 ms. |
| 8 | `phase1-actions-drift-attribution-restack` → `cancel-all-per-level-batch-removal` | Round 4 proved `phase1_actions` grows 13× with run length and 2.95× per unit with depth. `OrderBook::cancel_all` is O(orders × level-queue-length); grouping ids by (side, price) makes it O(levels touched). |
| 9 | `block-cap-resweep-under-pipeline` at RATE=100k | Open question: is ingress already within 15 % of the exec chain at 10 m? Until answered, a chain win is unreadable in matched/s. |
| 10 | `root-maintenance-skip-300m` (async-trie Option 0) | 0 at 10 markets; −415 ms of W at 300 markets where W is the wall. Needs `root_seconds=0` + trie/member cache invalidation + a stale sentinel. |
| 11 | The consensus-thread `epoch_validator_set_updates` CF_STAKING writer (design §3.1 row 11 / F8) | **Pre-existing and consensus-visible**, independent of the flag. In scope here only via a static staking set + differential test; deserves its own design. |

**What the design says cannot be pipelined, and is therefore not on this list:**

- **The engine.** Two engines in flight would need `engine(N+1)` to read `N`'s *in-progress* state.
  That is the market-sharded design, out of scope.
- **`save_books` pass 1.** `take_level_ops_chunked` computes level digests from the *live* book
  levels (`order_book.rs:1617`), which `engine(N+1)` mutates. Only pass 2 can move.
- **`flush-root-overlap-1deep` as a standalone stage** — *retired, not deferred*. Root's trie/mirror
  ops go into the **same** batch as state + marker (`backend.rs:684-727`); overlapping root(N) with
  engine(N+1) while keeping state_write(N) first needs two batches, i.e. the async-trie doc's
  Option A with its trie-height marker and O(state) rebuild after every unclean shutdown — which
  that doc rejected. Moving the whole flush to W overlaps root for free and keeps atomicity.

---

## 8. Rig vs code — an honest statement

- **n is small.** Every candidate arm is n=2 (a few n=3). The measured block-ms noise floor at n=2
  on 120 s cells is **±10.2 % about the mean (sd ≈14 %)** — established from
  `bl-base-10m-r1`/`r2` (789.79 / 969.24). matched/s pool sd is ~7 %, blk/s ~12 %. **A single-cell
  delta under ~15 % on block ms, or ~7 % on matched/s, is not a result.**
- **Consequently, only paired same-binary ON/OFF numbers are credited in this document.** The
  headline "vs carried best" deltas in §3 are printed because the harness computes them, but the
  round-2 (−27.4 %) and round-3 (−8.4 %) verdicts rest on kill-switch controls run on the *same
  binary in the same session*, and the round-3 −23.4 % headline was explicitly **not** credited.
- **Duration is a confound, not a detail.** Block ms roughly doubles inside a single 120 s cell as
  the book deepens (r2: 563 → 1263 ms, resting 615k → 1.27M), and `phase1_actions` reads 106–125 ms
  at 120 s vs 342–394 ms at 300 s **on byte-identical binaries**. Round 4 quantified it directly:
  13.1× growth over a run, 2.95× per cancelled order with depth. Cross-duration comparisons in this
  campaign are invalid and were avoided.
- **Cells lost to the rig: five** (`bl2-…-on-r2` idle 8.8, `bl2-flush-…-on-10m-r1` idle 0.4,
  `bl3-…-10m-r2` idle 1.5, `bl4-…-10m-r2` idle 2.5, `bl1-attribution-10m-r1` idle 2.5), all the same
  signature — a cell started too soon after a cargo link or a previous teardown, idle blk/s far
  below the healthy 12–28 band. Every one of them still AGREEd with 0 panics, so the degradation was
  box-side. `run-cell.sh` exhausts 3 idle probes and then **runs anyway**; backlog item 6 fixes that.
- **Two agreement verdicts were harness artifacts, not forks**, and both were resolved by re-running:
  `bl1-…-off-r1` (`DIGEST_UNVERIFIED`: digest RPC took 3.1–3.4 s while the chain kept committing;
  block hash, header root and all four counters byte-identical) and `bl2-…-on-r2` (height_spread 9
  on a starved box; hash/root/digest/counters all identical). Round 4's merged fix makes exactly
  this distinction structural.
- **`dissemination_clean=false` on every single cell** of the campaign (2–14 `sync_fallback`,
  0–5 `exhausted` per node). It is campaign-wide residue, never candidate-correlated, and no cell
  was ever accepted or rejected on it.
- **The box is shared.** 18 cores carrying 3 devnet validators + the bench + a live testnet
  validator (PID 2442841, untouched throughout, still up); `perf(1)` unavailable, so all attribution
  is in-node timers. At loaded cadence the per-committed-block floor is 151–295 ms against 37–83 ms
  idle — **part of that floor is the rig**. But the exec-starvation signature (busy 0.53–0.64, queue
  depth 3–12 of 64) says the wall is producer-side and real, and it will not move with any further
  exec-chain work.
- **Nothing was pushed.** All work is local on `perf/matched-200k`.
