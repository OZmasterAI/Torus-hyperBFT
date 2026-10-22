# s80 prototype: oracle-step-memo

Branch `s80/oracle-step-memo` (local only), worktree `/home/oz/projects/wt/novel-s80/proto/oracle-step-memo`,
base 29bfc705, commits beba69e0 + b314c9c5 (head b314c9c5). Target dir `/home/oz/.cargo-target-s80-oracle-step-memo` (release/torus-node
md5 e70f8a024b59a85877b267d726cc497d, built 09:32 from b314c9c5 source, bench-throughput fa8fd4bd unchanged). Flag `TORUS_S80_ORACLE_MEMO=1` (default off = 29bfc705 behaviour;
read once, INFO `TORUS_S80_ORACLE_MEMO on=<bool>` at the first context; INFO `TORUS_S80_ORACLE_MEMO steps hits=..
misses=..` every 2048 steps = the cell's proof of the hit rate).

## 1. Premise check on 29bfc705

Re-located on 29bfc705 (main's line numbers differ):
- `NativeExecutor::begin_block_oracle` `crates/torus-bridge/src/native_executor.rs:15813` = `oracle_inputs` (`:15831`:
  `prune_submissions`, `listed_market_ids`, `all_validators` -> Active whole-token stakes), `aggregate_oracle_prices`
  (`:15899`, one `aggregate_price` per listed market), `fill_block_marks` (`:15857`: `aggregated_market_ids`, then
  `BlockMarks::read` = one `get_price_opt` per market of listed ∪ margin configs ∪ aggregated ∪ books, version rule
  vs the slot's `BlockMarksState` `:2693`).
- `OracleManager` `crates/torus-core/src/oracle.rs`: `aggregate_price` `:286`, `prune_submissions` `:408`,
  `aggregated_market_ids` `:425`, `collect_submissions` `:444` (window rule `now - ts <= window`, saturating, so a
  future-dated row counts at once), `is_stale` `:471` (`now - ts > max_age`, monotone in `now`). Window 10 s, max age
  60 s.
- Node call site: `crates/torus-consensus/src/app.rs:5555` (once per executed native block, right after
  `attach_resident_block`).
- µbench: `crates/torus-bridge/tests/ubench_econ.rs:170` (inside the timed `exec` span).

Step 1 (ubench_econ, UB_MARKS=1, 300 markets, 600 senders, UB_RUNS=5, default feed = all reporters before every step;
`oracle_ms/blk` = `begin_block_oracle` alone, already inside `engine_ms`). Stub arm `UB_ORACLE_STUB=1` = the hit
floor: re-put the 300 × 36 B aggregate rows with block / timestamp patched, reuse the previous table and version.

| arm | round | MEDIAN oracle_ms/blk | runs | MEDIAN engine_ms/1k |
|---|---|---|---|---|
| real | 1 | 0.682 | 0.678 0.682 0.729 0.683 0.682 | 2.80 |
| stub | 1 | 0.088 | 0.087 0.088 0.089 0.091 0.083 | 2.77 |
| stub | 2 | 0.089 | 0.087 0.089 0.087 0.090 0.092 | 2.80 |
| real | 2 | 0.700 | 0.692 0.700 0.702 0.698 0.751 | 2.92 |
| real | 3 | 0.695 | 0.685 0.695 0.712 0.699 0.692 | 2.74 |
| stub | 3 | 0.087 | 0.087 0.088 0.087 0.085 0.083 | 2.71 |

Real − stub = 0.692 − 0.088 = **0.60 ms/blk** (kill line 0.3): premise holds, not killed. (Engine medians 2.82 vs
2.76 mean, inside the spread; the step is ~2% of a 32 ms engine block.)

## 2. Design

`OracleMemo` (native_executor.rs, next to `BlockMarksState`) lives in the resident rows slot (`RowsSlot.oracle_memo`),
moves with the mark state through `begin_resident` -> `ResidentBlock` -> `attach_resident_block` -> ctx ->
`detach_resident_block` -> `end_resident` (inline or worker) -> slot. Built empty with R, dropped with it (rebuild,
restart, a block that never reaches `end_resident`, a non-reusable slot).

Recorded after a full step (memo on, R attached, no fault, the block's own layer did not touch an `agg` row before
the step): `t0 = now`, `expiry = OracleManager::memo_expiry(now)` (new, oracle.rs: the first second a submission row
leaves the window `ts + window + 1` or a fresh aggregate turns stale `ts + max_age + 1`; `None` on an undecodable
row), listed markets, Active stakes, the step's results, the mark table, the fresh `agg` rows (36 B, each must carry
this block's height and timestamp, else no memo), the table's "extra" market set (margin-config ∪ book markets
outside listed ∪ aggregated), and `expect` = the block's own `agg` + `sub` changes after the step
(`resident_changes`).

Hit (next block): `t0 <= now < expiry`, listed markets equal, stakes equal, own layer touches neither `sub` nor
`agg` before the step (`layer_touches`), extra market set equal. Then: re-put the fresh rows with bytes 16..24
(block) and 28..36 (timestamp) patched, reuse the table (version by the same rule as `fill_block_marks`: unchanged
iff previous marks and configs equal), return the memo's results. No prune, no collect, no median, no mark reads.

Carry (`end_resident`): the memo survives the block iff the block's `ResidentDelta` rows of the oracle CF under
`agg` / `sub` are exactly `expect` (after a full step: its rows; after a hit: the fresh rows stamped with this
block). Any feed action, prune, delete or rewrite after the step therefore drops it; the next block runs the full
step and records a new memo.

Why exact: with the `sub` / `agg` rows, listed set and stakes unchanged and `now < expiry`, the full step at `now`
prunes nothing, collects the same submissions per market, takes the same fresh / fallback branch per market (the
single-reporter 10% cap compares against the row the memo's own step wrote: diff 0, passes), writes the same fresh
rows with only block and timestamp changed, and every mark read sees the same usable price (fresh rows stay fresh;
the staleness of every other row cannot flip before `expiry`). The table's market set is unchanged (listed equal,
aggregated set unchanged since only the step writes `agg`, extra equal).

## 3. Diff summary

`git diff --stat 29bfc705..b314c9c5`: native_executor.rs +~376 (OracleMemo / OracleExpect, flag + counters + 2048-step
INFO log, RowsSlot / ResidentBlock / EndResidentJob / ctx plumbing, `begin_block_oracle_memo`, `record_oracle_memo`,
`oracle_listed_stakes` split out of `oracle_inputs`, test accessors `oracle_memo_hit` / `block_mark_table`, bench-only
`oracle_step_stub_for_bench`); oracle.rs +26 (`memo_expiry`); ubench_econ.rs +73 (`oracle_ms/blk`, `UB_ORACLE_STUB`,
`UB_FEED_EVERY`, `UB_SECOND_EVERY`, hit/miss split); oracle_tests.rs +53; new tests/oracle_memo_equivalence_tests.rs
(344 lines). Flag off: `begin_block_oracle` takes the 29bfc705 branch (one bool test); the slot carries `None`.
b314c9c5: no memo record when the block's own layer wrote an `agg` OR `sub` row before the step (the ubench default
feed does that every block; the node never writes oracle rows before the step).

## 4. Tests

All in release (`--release`, target dir above; no debug tree in it).
- New `crates/torus-bridge/tests/oracle_memo_equivalence_tests.rs`:
  `oracle_memo_on_equals_the_full_step` (4 seeds x 400 blocks, memo off vs on with end_resident inline AND on its
  worker; per block results, every oracle row, get_price per market, mark table, version moved; final DB oracle CF;
  warm R == cold build every 50 blocks; scenarios: second advancing / jumping past window and max age, feeds before and
  after the step, pauses, bad prices, undecodable sub row, agg deleted / rewritten before and after the step, signer
  rows, validator jailed / re-activated, market listed, book for an unlisted market, margin-config market) and
  `oracle_memo_hit_rate_on_a_steady_feed` (feed every 15th block after the step, second every 22 blocks: misses ==
  1 + feed blocks exactly, outputs equal). 2/2 pass.
- RED / power check: temporary env-switched mutations (removed afterwards, binary re-checked for no `OSM_MUT`
  string) disabling each validity check: expiry, stakes, extra market set, own-layer touch, end_resident delta check.
  Every one fails `oracle_memo_on_equals_the_full_step` (block-equality assert, line 263); the delta mutation also
  fails the steady-feed test. No check is dead.
- torus-core `oracle_tests` +3 `memo_expiry` tests: 29/29. torus-core full: 420/420 (14 skipped).
- perf_equivalence_golden: flag off 9 run / 9 pass (with memo + ubench targets), flag on (TORUS_S80_ORACLE_MEMO=1)
  7/7.
- torus-bridge full suite (nextest --release, 1091 tests) at beba69e0: off 1091/1091, on 1091/1091; at b314c9c5:
  off 1091/1091, on 1091/1091 (17 skipped each, 0 flaky). Clippy not run.

## 5. Microbench A/B

ubench_econ via the same env as proto/ub.sh (UB_MARKETS=300 UB_RUNS=5 UB_MARKS=1 TORUS_SETTLE_STREAMED=1), all
rounds of a set inside ONE heavy-lock hold (/tmp/osm-abba.sh; per-run flock queueing took >20 min per run under
contention). Logs: proto/results/ub-oracle-step-memo-<label>.log. Binary beba69e0 (f15 / every sets).

Feed cadence like the cell (UB_FEED_EVERY=15 = feed as actions after the step every 15th block, UB_SECOND_EVERY=22,
UB_MEASURE=15 -> 14 hits / 1 miss per run = 93%), order off,on,on,off,off,on:

| label | MEDIAN engine_ms/1k | MEDIAN oracle_ms/blk | engine_ms/blk (median run) |
|---|---|---|---|
| f15-off-0 (lone run before the set) | 2.89 | 0.596 | 33.0 |
| f15-off-1 | 2.88 | 0.597 | 32.7 |
| f15-on-1 | 2.82 | 0.175 | 32.2 |
| f15-on-2 | 2.86 | 0.178 | 32.5 |
| f15-off-2 | 2.89 | 0.593 | 32.8 |
| f15-off-3 | 2.88 | 0.594 | 32.8 |
| f15-on-3 | 2.87 | 0.177 | 32.3 |

Delta of means (rounds 1-3): engine_ms/1k 2.883 -> 2.850 = **-1.16%**; engine_ms/blk 32.77 -> 32.33 = -1.3%;
oracle step 0.595 -> 0.177 ms/blk = **-0.418 ms/blk** (the direct timer, spread < 3%). Per step with the memo on:
hit 0.128-0.135 ms, miss 0.80-0.84 ms (a miss = full step + record, +0.23 ms over the plain step).

Worst case, default feed (all reporters BEFORE every step -> never a hit), beba69e0:

| label | MEDIAN engine_ms/1k | MEDIAN oracle_ms/blk |
|---|---|---|
| every-off-1 | 2.83 | 0.681 |
| every-on-1 | 2.94 | 1.259 |
| every-on-2 | 2.88 | 1.250 |
| every-off-2 | 4.30 (runs 3-5 at 4.3-4.5: host interference, discard) | 0.885 |

The memo cost +0.57 ms/blk there (record every block, never used). b314c9c5 skips the record in that pattern; its
re-measure (labels f15b-* / everyb-*, ABBA x3 each) was queued on the lock (/tmp/osm-ub3.log) and had not run when
this report was written. Expected: everyb on == off (only two layer_touches calls added), f15b as f15.

## 6. Verdict

Built, exact, small. Premise holds (0.60 ms/blk real minus hit floor). Memo hit rate 93% at a cell-like cadence; the
oracle step drops 0.595 -> 0.177 ms/blk, engine proxy -1.16% (delta of means, inside the 3-5% run spread but matching
the direct step timer, -1.3% of engine_ms/blk). That meets the candidate's own bar (cell-worthy if engine gain >= 1%)
but not the generic >= 2% bar. A cell's matched/s gain (+0.4..+1.0% expected) is below cell noise; a cell is mainly
worth it to read the real hit rate (INFO `TORUS_S80_ORACLE_MEMO steps hits= misses=` every 2048 steps) and confirm
agreement with the flag on. Risks: a feed every block (memo recorded, dropped at end_resident) costs ~+0.23 ms/blk;
an invalidation source missed by the five checks would diverge (the randomized test + mutation check cover the known
inputs: sub/agg rows, time, listed set, Active stakes, table market set). Cell env: TORUS_S80_ORACLE_MEMO=1.
