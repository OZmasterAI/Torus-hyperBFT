# r2 lens: latency-closed-loop (s80, round 2)

Base: main `eb993e3f` for code anchors (the checkout is now `2d2f3ed9`; `crates/torus-consensus/src/app.rs` shifts
by about +215 lines there, `torus-mempool` and `tools/bench-throughput` are unchanged, `torus-node/src/main.rs` moved
by 3 lines). Nothing was built, run or benched. All new numbers come from re-reading existing 9j2 cell data under
`~/bench-results-matched/ozarchy-9j2-*` (`sampler.csv`, `vmstat-1hz.txt`, `host-1hz.tsv`, `val*.log.gz`,
`metrics-1hz.txt.gz`, `bench.log`). The scripts are in `/home/oz/projects/wt/novel-s80/scratch-r2-closedloop/`
(`storm.py`, `commits.py`, `intervals.py`, `pulls.py`, `resend.py`, `flushage.py`), plus the r2 scratch helpers
`scratch-r2-latency/{tab,pump,tail,ph}.py`. Memory checked: s114 owner decision 2d65fbbe (cancel share oldest-first,
"no per-sender cap now"), d46ec506 (cancel-all expiry root cause).

## 0. What the closed loop actually does (new measurements)

Round 1 found a ~13.5 s burst cycle (phase A: mempool >= 1,500; phase B: exec queue >= 40; phase C: the rest).
This round looked at what happens inside phase A, and why the cycle keeps re-synchronising.

### 0.1 Phase A is a host CPU storm, and it slows the serial chain on the same blocks

`storm.py` joins per-second host CPU (vmstat), per-node CPU (`host-1hz.tsv`) and val0's sampler.

| cell | phase | secs | host busy % | runq | 3-node cores | exec busy | actions/s | exec ms / action |
|---|---|---|---|---|---|---|---|---|
| std r2 CL21 | A | 48 | 81.4 | 58.1 | 22.3 | 0.84 | 1,092 | **0.77** |
| | B | 42 | 57.0 | 26.7 | 17.2 | 1.04 | 2,121 | **0.49** |
| | C | 219 | 63.1 | 39.8 | 17.2 | 0.83 | 1,369 | 0.61 |
| std r1 CL21 | A / B | 55 / 47 | 79.7 / 59.3 | 51 / 27 | 21.7 / 17.7 | 0.85 / 1.00 | 1,142 / 1,990 | 0.74 / 0.50 |
| deep r2 CL21 | A / B | 45 / 37 | 77.2 / 61.0 | 50 / 30 | 21.0 / 17.7 | 0.83 / 1.02 | 1,134 / 1,894 | 0.73 / 0.54 |
| deep r1 R | A / B / C | 107 / 108 / 90 | 79.9 / 59.2 / 63.2 | 55 / 29 / 44 | 21.9 / 17.2 / 16.4 | 0.89 / 1.02 / 0.84 | 1,035 / 1,896 / 1,176 | 0.86 / 0.54 / 0.71 |

- Phase B executes the same 400-action storm blocks that phase A committed. So the +45-60% exec cost per action in
  phase A comes from contention, not from block shape.
- Consensus shows the same effect on identical 400-action blocks. From `commits.py` on the val0 log:
  - heights 602-607 (22:24:12.29-13.84 UTC, host 86-89% busy, runq 70-150): one 400-action commit every **311 ms**;
  - heights 617-632 (22:24:16.15-17.67, host 50-64% busy): one every **102 ms**.
- Every block committed in a storm carries exactly 400 actions, so the action cap binds.
- Phase-A timers vs phase C (val0, `ph.py`): build 66 vs 6 ms, `on_committed_block` 38 vs 3, insert_persist 31 vs 7,
  vote_delay 26 vs 7 ms.
- The extra load is ingest. Phase-A admits run at 1.8-5.4k actions/s, and every action is verified three times (RPC
  ingress plus two gossip receivers, `main.rs:989-1037`, `torus-mempool/src/lib.rs:1347-1390`). That adds about 4-5
  node cores, on top of the bench's own signing burst.

### 0.2 The 25% cancel share defers cancels to the storm's end, and those are the oldest actions

- `pump.py`: 31-80 load-seconds per cell execute exactly 24-26% cancels at >= 1,200 actions/s ("capped"). Once per
  cycle comes a "flush" second that executes 2.8-3.4k cancel-alls, 77-90% of its actions.
- `flushage.py`: per-second exec-age means at val0, grouped by that second's cancel share.

| cell | flush s: secs / actions / exec-age mean | capped s | other s |
|---|---|---|---|
| std r2 CL21 | 12 / 30,185 / **4.18 s** | 31 / 46,980 / 3.16 s | 261 / 364,153 / 1.27 s |
| std r1 CL21 | 15 / 39,080 / **3.82 s** | 42 / 61,184 / 3.08 s | 246 / 341,406 / 1.27 s |
| deep r1 CL21 | 15 / 35,162 / **4.12 s** | 30 / 46,659 / 3.79 s | 259 / 350,382 / 1.30 s |
| deep r1 R | 26 / 57,220 / **5.30 s** | 80 / 110,633 / 3.68 s | 193 / 252,326 / 2.83 s |

- The bench cannot place again until its own cancel-all commits. `in_flight.rs:256` `Tracker::ready` blocks a sender
  whose cancel-all is in flight while it is over budget.
- So the flush releases most of the fleet at once, and that is the trigger of the next wave.
- Bench mix: 38.5% cancels. That is 1.25 cancel-alls per two places, against 1.0 for a clean P,P,C cycle plus about
  5% random cancel-alls. So a small extra share of cycles needs a second cancel-all. Ordering is one possible cause:
  today a sender's cancel-all can land before, or in the same block as, its own older places, and a block runs its
  cancel-alls first (`in_flight.rs:315`; the bench comment at `main.rs:884-887`).

### 0.3 Other regime facts (evidence for existing ledger items, not proposed here)

**Empty blocks and the 250 ms header re-send.**
- 55-67% of committed blocks are empty (`intervals.py`, all four cells).
- Commit intervals (val0 std r2): p50 8.4 / p95 **257.5** / p99 339 ms.
- The 250-275 ms bin holds 312 intervals, against 38 in the bin before it and 57 in the bin after. That is 4.3% of all
  intervals, and 85% of them are empty-to-empty.
- The rate limiter dropped 668-706 consensus messages per node (`torus_consensus_rate_limited_dropped_total`,
  `metrics-after-val*.txt`).
- So block p95 is the header re-send timer. This supports the existing `consensus-ratelimit-per-class-buckets`
  (proposed-not-done).

**The bench tail is gated by body size.**
- val2 reads 3-4 bodies/s while 400-action storm bodies (~160k orders of JSON) are at its cursor, and 30-65/s
  afterwards (`tail.py`).
- Releases come in quanta, so phase-C blocks are bimodal: either empty or 100-400 actions.
- 9-14% of phase-B/C block hashes are pulled from the proposer because they are fresh at proposal time; in phase A
  only 0.8-1.5% are (`pulls.py`).
- Existing items cover this: r1 `commit-inclusion-stream` and `rpc-block-body-raw-json`.

**Deep R vs CL21 blk/s (s124.5, "unexplained").**
- Deep R spends 35% of load-seconds in phase A and 35% in phase B. Deep CL21 spends 14% in A.
- Storm blocks are 400-cap blocks committed at 2-10/s, so blk/s here measures the closed-loop regime, not exec speed.
  This plausibly explains equal matched/s at 15.95 vs 21.5 blk/s.

**The order-age histograms cannot resolve p99.** The edges are 5.12 / 10.24 s, so exec p99 is interpolated inside a
2x bucket. Any latency A/B should use per-second sum/count by phase, or finer edges.

---

## Idea 1: causal cancel lane (cancels outside the action cap, never ahead of their own sender's places)

**Mechanism.**
- Today `NativePool::select_entries_before` (`torus-mempool/src/native_pool.rs:564-610`) puts cancels inside the same
  400-action limit:
  - phase 1: oldest-first cancels up to `ceil(400 × 25%)` (`:575-576`);
  - phase 2: non-cancels in (sender, nonce) order (`:594`);
  - phase 3: leftover slots to cancels (`:603`).
- Proposed, behind an env knob `TORUS_CANCEL_LANE_CAP` (0 = today):
  - walk the non-cancels first, up to the unchanged action cap;
  - then walk cancels oldest-first (`CancelMerge`) up to a separate lane cap (e.g. 400), counting their tiny bytes
    against the byte cap;
  - skip a sender's cancel while that sender still has an older non-cancel that is pooled and not in flight, or is
    selected in this same block (a block runs cancel-alls first). Check it with one `entries.range` over
    `(PRIO_NORMAL, sender, ..nonce)` plus a per-sender "max selected place nonce" in `SelectionWalk`.
- This is proposer-local: `validate_block` does not reject on count (`rate_limit.rs:88-91`).

**Why it helps.**
- Storm blocks would stop deferring cancels behind every other sender's places.
- Each sender's cancel-all would commit about one view after its own places. Releases would then spread across the
  storm in address order, instead of arriving in one flush.
- It also stops cancel-alls overtaking their own sender's places.

**Expected gain.**
- Flush seconds hold 6.9-9.4% of all executed actions (30-57k per cell, about 75-90% of them cancel-alls) at an
  exec-age mean of 3.8-5.3 s, against 1.3 s for normal seconds.
- If those cancels commit 1-2 storm views (0.3-0.6 s) after their own places instead of at the flush, their exec age
  drops by about 1.5-2.5 s.
  - Mean exec age: about −0.1..−0.2 s on ~1.6 s.
  - Commit p99: 5.0 → ~4.0-4.3 s.
  - Exec p99: −10..−20% (it needs finer buckets to see).
- Second-order effect: the flush-triggered fleet wave shrinks. Its sign and size are unknown; matched/s ±3%.
- Byte cost: +≤400 × ~0.15 KB per block, about +1.4% of a 4.3 MB storm block.

**Nearest prior (grepped: cancel, share, lane, cap, exempt, overtake, causal, round-robin, oldest-first).**
- `antispam-cancel-block-share` (merged): 25% of the same 400 slots.
- `cancel-share-oldest-first` (merged; s114 decision 2d65fbbe: "no per-sender cap now"): the order inside the share.
- `cancel-share-higher-pct-or-round-robin` (rejected / not chosen): a bigger share or round-robin inside the share.
- `antispam-pool-cancel-quota` (rejected): a pool quota.
- `consensus-work-ceilings-r16` (proposed): metered work ceilings.
- `in-block-seeded-priority-r31` (proposed): nonce order inside a block.
- Difference: this is an additive per-block cancel budget plus a per-sender causal gate across blocks. It is not a
  share of, or a reordering within, the same 400 slots.

**Risks.**
- Cancel spam is bounded by the lane cap, the per-address limits and the funded check. The anti-spam rounds should be
  re-run.
- Storm blocks get +100 places of bytes if the non-cancel cap stays at 400. Also run an arm at 300.
- The bench mix may move: fewer double cancel-alls.
- Same-block P+C semantics stay as they are (cancel first). A consensus-level "nonce-scoped cancel-all" would fix
  that, but it is product work.

**Prototype (≤ 3 h).**
1. Patch `select_entries_before` behind the knob, plus two unit tests: the lane never displaces places, and a cancel
   never selected with or ahead of its own older place.
2. Run ABBA std 120 s cells, 2 per arm, on the h2h base.
3. Read `pump.py` (flush seconds should drop to about 0), `flushage.py`, `intervals.py` and the bench econ mix line,
   plus matched/s and blk/s.
4. Go if flush seconds ≤ 2 per cell, capped-second exec-age mean −20%, matched/s within ±2% and the cancel share within
   ±3 pts.

---

## Idea 2: storm-deferred ("just-in-time") gossip verification

**Mechanism.**
- Today every gossip-received action is verified on arrival (`main.rs:989-1037` → `torus-gossip-verify` pool, 16
  threads per node; `lib.rs:1347-1390`: DA mirror first, then trust-cache lookup, then `recover_sender`).
- Proposed: when the node's verified pool already holds at least D non-cancel actions (e.g. 2 × 400), the
  gossip inbound task does only the DA mirror and the action hash, then parks the action in a bounded deferred queue.
- Deferred actions are verified when either:
  - (a) their hash appears in a received proposal: `check_proposal_data` (`app.rs:10103`) sends the compact hash
    list to the gossip task, which verifies those first and seeds the exec trust cache before exec reaches the block
    (exec verify `app.rs:5381`); or
  - (b) the verified pool drops below D; or
  - (c) the host calms down.
- Below D, behaviour is unchanged.

**Why it helps.**
- During a storm the pool already holds 3-8k verified actions: several seconds of consensus work. Verifying the rest
  on arrival only competes for CPU with the serial chain.
- The verification work stays the same. It just moves out of the storm peak (host 77-89% busy, runq 50-150) into
  phases B and C (57-63% busy). No trust change: every node still verifies every action before it proposes or
  executes it.

**Expected gain.**
- Gossip verify is about two thirds of verify work. Removing ~2.5-4 cores from storm peaks puts the host at ~68-70%
  busy.
- Storm views on identical 400-action blocks: 311 ms contended vs 102 ms uncontended today. Estimated −25..−45%.
- Exec cost per action in A: 0.74-0.86 → ~0.55-0.6 ms.
- Phase A drains 1.3-1.6x faster:
  - commit p99 5.0 → ~3.3-4.0 s;
  - block p99 −10..−20% (35 of the top-1% intervals in std r2 val0 are phase A);
  - exec p99 −10..−15%.
- Exec time saved: ~0.2 ms × ~1,100 actions/s × 48-55 storm-seconds ≈ 10 s per 300 s cell (~3.5-4% of exec busy).
  Through the closed-loop elasticity that is matched/s 0..+2%; deep R (35% storm) up to +3%.

**Nearest prior (grepped: verify, gossip, lazy, defer, on-demand, jit, nice, budget, sojourn, attest).**
- `origin-attested-gossip` (proposed): trusts the origin and skips re-verify. Here the trust model is unchanged.
- `l6-3-preexec-lookahead-verify` (proposed): an exec-side lookahead recover. Here the gossip-side arrival work is
  deferred.
- `bulk-thread-nice` / `renice-sched-batch` (rejected, BORE): priority. This removes runnable work instead, and does
  not depend on the scheduler.
- `core-budget-umbrella` (rejected, −67.7%) and `verify-pool-sizing-by-budget` (proposed): static caps. This one
  acts only in storms and stays work-conserving.
- `gossip-prescreen-sojourn-gate` (inconclusive): an admission shed.
- r1 #9 `serial-chain-sched-priority`: raises serial-thread priority, which needs privileges and leaves the 32-worker
  exec-pool fork-join exposed.

**Risks.**
- Exec trust-cache misses if (a) lags. Exec would then recover on the exec pool, which is a cost on the ceiling.
  Watch exec verify ms/blk.
- Leaders mostly propose their own-ingress actions during storms. That changes the inclusion mix, and an imbalanced
  ingress needs the (b) refill.
- The deferred queue needs a hard bound and a drop rule (unverified bodies are already DA-mirrored first; see
  `da-mirror-after-signature`).
- A follower that lacks verified copies still votes, since voting needs DA presence only.

**Prototype (≤ 3 h).**
1. Probe: a `TORUS_GOSSIP_VERIFY_DEFER_DEPTH` knob in the gossip task. Above D, push to a `VecDeque` drained by 2
   threads; no proposal-priority path.
2. Run ABBA std 120 s cells, 2 per arm.
3. Measure with `storm.py` (phase-A host busy, exec ms/action in A), `commits.py` (400-block commit spacing in A) and
   `pulls.py`, plus exec verify ms/blk, per-phase age means, matched/s and blk/s.
4. Go if phase-A 400-block spacing is −25% and exec verify ≤ +0.5 ms/blk.

---

## Considered and dropped

| idea | why dropped |
|---|---|
| Work-weighted exec channel | Phase B holds 58% empty blocks (564 of 964 at val0), so a block-count bound stalls consensus on cheap slots. But exec age is unchanged (same work ahead), and only 8 of 75 top-1% intervals are in B. Near `deepen-exec-lead-past-64` (rejected) and `exec-queue-depth-16` (parked). |
| Suppress idle empty views / adaptive re-send | Same family as `hold-light-proposals-under-rate-limit`, `consensus-ratelimit-per-class-buckets`, `header-resend-250ms`; also timeout tuning (excluded). The evidence is in 0.3. |
| Maturity-aware selection (skip fresh actions to avoid pulls) | Pulls are rare in phase A. In phase C the long intervals are re-send stalls, so the gain is about 0. |
| Bigger blocks when exec is idle and the mempool is deep | A re-parameterisation of the block cap (`block-cap-1000` rejected, `cap-sweep-uncapped`). In storms the cost per action dominates, so only about +5%. |
| Nonce-scoped cancel-all (cancel only orders signed before the cancel's nonce) | Consensus-visible semantic change. It counts as product work (P3 surface), and it could lower bench matched/s. Mentioned as the clean fix for same-block P+C. |
| Fair or arrival-order place selection | `rotate-selection-cursor` (proposed), `arrival-order-pool-selection` (rejected), `in-block-seeded-priority-r31`. |
