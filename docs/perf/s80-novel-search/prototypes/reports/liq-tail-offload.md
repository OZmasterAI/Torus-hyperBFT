# s80 prototype: liq-tail-offload

Branch `s80/liq-tail-offload` (local only, not pushed), worktree
`/home/oz/projects/wt/novel-s80/proto/liq-tail-offload`, base 29bfc705. Commits: 130036f7 (implementation +
equivalence tests), b40f47f0 (flag parse / pin unit test), 147781cb (fix round: reader-fault fallback, fault
tests, golden path check, startup INFO line, doc fixes; section 8). Target dir
`/home/oz/.cargo-target-s80-liq-tail-offload`. Current binaries (fix round, built 11:01-11:02 in one hold of the
heavy lock from the fix-round source; the commit adds only a test-only type alias on top, see section 8):
release/torus-node md5 070f33779928ac0afa00059ae0e0f510 (11:02:02); ubench_econ `ubench_econ-1a6f456edd610a23`
md5 43b118a1a1eb2124f530fc2e86e90ac9 (11:02:12). bench-throughput fa8fd4bd unchanged. The first round's node was
e1e1273678ba1e1328f2cb79302662c7 (130036f7).
Flag `TORUS_S80_LIQ_PREVALUE` (read once per process; unset / `0` = 29bfc705 behaviour; `1` = on (mask 3);
`pool` = part A only, `2` = part B only). INFO line, logged at node startup (`App::new` reads the flag since the
fix round; before that it was logged on the first liquidation step):
`TORUS_S80_LIQ_PREVALUE on=<bool> (liquidation window valued on the exec pool / dirty traders without key clones)`.
Knobs: `TORUS_S80_LIQ_PREVALUE_WORKERS` (pool jobs per window, default 8), `TORUS_S80_LIQ_PREVALUE_MIN`
(smallest window pre-valued, default 64).

## 1. Premise check on 29bfc705

Re-located on 29bfc705: `run_liquidations` `crates/torus-bridge/src/liquidation_step.rs:150`, pass
`liquidation_pass` `:402`, window `liq_traders_after` `:606` -> `Users::traders_after` `users.rs:748`
(`layer_keys(CF_NATIVE_POSITIONS)`: a clone of every position key the block wrote), valuation `liq_view` `:845`
-> `AccountReader::pos_sums` `native_executor.rs:2918` (dirty trader: `dirty_sums` over the overlay; clean trader:
the slot entry / the block memo over R). `LIQ_SCAN_PER_BLOCK` = 2,048 (`torus-core/src/liquidation.rs:58`).

Node cost (9j2 std CL21 r2 val0, from map/measured.md and the node's metrics): liquidation step 25.4 s over 7,117
blocks = 3.57 ms/blk; `liquidation_scanned` 11,658,240 = 1,638 traders/blk (every holder, under 2,048), 2.18
us/trader. The post-engine tail is 3.65 ms/blk of a ~38 ms block. The cost is there.

Probe (timers inside `liquidation_pass`, ubench_econ, 29bfc705; diff kept at
`proto/results/liq-tail-offload-probe.diff`, logs `ub-liq-tail-offload-probe{,2}-{600,5000}.log`), per block:

| senders | window build | loop | window | dirty (pos rows) | dirty valuation | clean valuation | cooldown/pending reads |
|---|---|---|---|---|---|---|---|
| 600 | 0.78-0.92 ms | 4.24-4.65 ms | 465-474 | 307-317 (66%) | 4.29-4.35 ms | 0.11 ms | 0.064 ms |
| 5000 | 1.38-1.43 ms | 5.40-6.03 ms | 1,114-1,129 | 634-682 (59%) | 5.17-5.29 ms | 0.33-0.34 ms | 0.151 ms |

Finding that changed the design: the step is dominated by the valuation of DIRTY traders (traders the block
wrote: ~14 us each, `dirty_sums` over the overlay), not untouched ones (~0.7 us each from the slot / memo). The
write-up's variant (pre-value the window from the frozen R(N-1) during engine(N), re-value only traders dirtied by
block N) can only take the clean part off the exec thread: ~0.11 of ~4.6 ms at 600 senders, ~0.33 of ~6.0 ms at
5000 (2-7% of the step, ~0.1-0.3 ms/blk); dirty traders cost ~14 us (600) / ~8 us (5000) each.
Not worth a helper thread and an R(N-1) snapshot protocol. The exact node-local variant that does address the cost:
value the whole window (dirty traders included) in parallel on the exec pool right after the engine, from the
block's own overlay, before the serial loop.

## 2. Design (node-local, exact)

Part A (mask bit 0, `liq_prevalue_pass` `liquidation_step.rs:573`, called at `:439`):
- Before the serial loop, the first `scan` non-protocol candidates (exactly the accounts the loop can scan, in loop
  order) are valued with the same `liq_view` on the exec pool (`exec_pool::run_jobs`, `WORKERS` contiguous chunks,
  read-only).
- The loop consumes the pre-values in order. Until the loop's first act it writes only liquidation-CF rows
  (cooldown clear / pending flag), which `liq_view` never reads (it reads positions, balances, the block marks and
  margin configs), so each pre-value equals what the loop's own `liq_view` would return at that point.
- At the first act (a non-healthy account: cancels, stage-1 fills, backstop / ADL transfers write OTHER accounts'
  positions and balances) the remaining pre-values are dropped and every later account is valued live: the exact
  29bfc705 path from there on.
- Errors keep their position: a pre-valued `Err` is returned at the same trader the live loop would return it
  (`v?`), after the same writes for the traders before it. A panicking job -> `None` -> the live path (meets the
  same panic on the exec thread, as today). Windows under `MIN` stay live.
- Shared state touched by a valuation: the block's sums memo (`Mutex<HashMap<_, OnceLock>>`: an entry is the
  trader's sums over R's rows at the memo version, the same value whoever builds it; the carry already handles
  memo entries of traders the block writes later, the engine creates those too), and the reader-fault channel
  (`OnceLock<String>`, only on a storage read fault). No thread-local state is read on the valuation path (all
  thread-local pins in these files are test pins of other stages).

Part B (mask bit 1): `Users::traders_after` takes the block's dirty traders from a new
`StateBackend::layer_traders(cf, key_len)` (`torus-state/src/backend.rs:143` default `None`, overlay impl `:3488`):
the distinct 20-byte prefixes of the own pending keys of one length, merged from writes and tombstones, without
cloning a key. Same sorted list as `layer_keys` + map + dedup; `None` exactly when `layer_keys` is `None`.

Not consensus-visible: block results, state rows, the state hash, metrics counts of the step are those of the
flag-off run.

Reader-fault channel (fix round, Codex P2): the first version had a real difference here, and the earlier claim
("a fail-stop condition on either path") was wrong. The pass values the whole window, also accounts the serial
loop never reaches (act cap `LIQ_ACT_PER_BLOCK` = 64, or an earlier `Err`). Valuing such an account can latch
`ctx.reader_fault` (a `OnceLock`): `AccountReader::mark` -> `usable_mark` for a market outside the block's mark
table, on a local storage read / decode fault. `take_fatal_error` then failed a block that commits with the flag
off (node-local, fail-stop, no wrong state; proven RED by the new test, case a). Fixed in `liquidation_pass`
(`liquidation_step.rs:439-458`): `had_fault` is read before the pass; when the channel holds a fault after the pass
that was not there before, the step takes it out of the channel and drops the pass (`pre = None`), so the loop
values every account live (the 29bfc705 path) and latches the same fault again only if it reaches that account.
This also covers a pass whose other job panicked (`None`). A memo entry valued with a mark from outside the table
is never cached (`cached_sums`: `in_table` false -> `None` -> `direct_sums`), so the live re-run reads the oracle
again. An `Err` from `liq_view` (balance / position read) is not on this channel: it stays in its pre-value and is
returned at its trader, or dropped unconsumed at the first act (cases c, d).

## 3. Diff summary (29bfc705..branch)

- `crates/torus-bridge/src/liquidation_step.rs` (+60/-1): the pre-value pass and its consumption / drop in
  `liquidation_pass`.
- `crates/torus-bridge/src/native_executor.rs` (+188): `LiqPrevalue` config, `parse_liq_prevalue_mask`,
  thread pin `with_liq_prevalue` (tests / differential runs), `liq_prevalue()` (env read once + INFO line), test
  counters; unit test module `liq_prevalue_flag_tests`.
- `crates/torus-bridge/src/users.rs` (+22/-5): part B in `traders_after`.
- `crates/torus-state/src/backend.rs` (+50): `layer_traders` (trait default + overlay impl).
- Tests: `crates/torus-bridge/src/liquidation_l1_tests.rs` (+46), `crates/torus-state/src/resident_rows_tests.rs`
  (+52).

Fix round (147781cb, on top of b40f47f0; 6 files, +317/-6; whole branch vs 29bfc705: 8 files, +729/-6):
- `liquidation_step.rs` (+32/-1): the reader-fault fallback after the pass (`:439-458`); a local `consumed`
  count published once per step; pass doc: the fault discard and the latency note.
- `native_executor.rs` (+27/-3): `fault_fallbacks` test counter; process-wide counters `LIQ_PREVALUE_PASSES` /
  `_CONSUMED` / `_FAULT_FALLBACKS` (`:1252`, relaxed, one add per step each) and `liq_prevalue_counts()`;
  `liq_prevalue()` made `#[doc(hidden)] pub` (read by the golden and by `App::new`).
- `users.rs` (+2/-2): doc nit ("mask bit 1, value 2").
- `torus-consensus/src/app.rs` (+3): `App::new` reads the flag once at startup (the INFO line).
- `tests/perf_equivalence_golden.rs` (+19): `prevalue_path()` (`:840`) at the end of the three digest tests.
- `liquidation_l1_tests.rs` (+234): `FailRead` backend (`:506`), `fault_block` (`:618`), the fault test (`:685`).

## 4. Tests and results

New tests:
- `liquidation_l1_tests::liq_prevalue_masks_equal_flag_off_on_seeded_sequences` (`liquidation_l1_tests.rs:464`):
  3 seeds x {node path (R, L1, shadow), reference walk} x masks {1, 2, 3}, each against the flag-off run of the
  same seed: per block, results, margin configs / listed markets, liquidation / position / balance rows,
  `liquidations_triggered`, ADL work units equal. The seeded sequence moves, refreshes, jumps and drops marks, so
  liquidations fire (stage 1, backstop, ADL). Non-vacuous: pre-values consumed, pre-values DROPPED after an act
  (total `dropped > 0`, so the live path after an act is exercised), prefix-list windows used. Written to fail when
  the pre-values are not dropped at an act.
- `resident_rows_tests::layer_traders_are_layer_keys_prefixes` (`resident_rows_tests.rs:562`): 400 random own
  writes / tombstones (odd key lengths mixed in) over R, `layer_traders` == `layer_keys` prefixes deduped; `None`
  cases (StateDb, no R, non-R CF).
- `native_executor::liq_prevalue_flag_tests::parse_and_pins`: the flag's spellings (unset / 0 / false / off = off;
  1 / true / on / 3 = 3; 2; pool = 1; junk = off) and the pin (wins over the env, nests, restores on unwind,
  per-thread).

Runs (ozarchy, nextest, `--cargo-quiet --status-level fail --final-status-level fail`):

| run | result |
|---|---|
| `-p torus-bridge -p torus-state`, flag off (includes perf_equivalence_golden, liquidation_tests, the new tests) | 1509 passed, 22 skipped, 0 failed, 0 flaky (68.2 s) |
| `-p torus-bridge`, `TORUS_S80_LIQ_PREVALUE=1 TORUS_S80_LIQ_PREVALUE_MIN=1 TORUS_S80_LIQ_PREVALUE_WORKERS=4` (every window pre-valued; includes golden, liquidation_tests) | 1090 passed, 17 skipped, 0 failed (54.3 s) |
| `binary(perf_equivalence_golden) \| binary(liquidation_tests) \| test(liquidation)`, `TORUS_S80_LIQ_PREVALUE=1` (default MIN 64 / WORKERS 8) | 79 passed, 0 failed |
| `test(liq_prevalue_flag_tests) \| test(liq_prevalue_masks)` (after commit 2's test) | 2 passed |
| clippy `-p torus-bridge -p torus-state --tests` (with commit 2) | 0 errors; 143 warnings, none in a changed line (all pre-existing lines of backend.rs, resident_rows_tests.rs, liquidation_step.rs:1175, native_executor.rs:16561) |

(The flag-on suite reads the env in every test process; the pinned equivalence test proves the pre-value path is
taken and drops at acts.)

Fix round, new test (written first; RED on the first-round code):
`liquidation_l1_tests::liq_prevalue_fault_outcomes_equal_flag_off` (`liquidation_l1_tests.rs:685`). One block over
the node's lifecycle (`begin_resident`, R attached for L1, `begin_block_oracle`), run with the flag off and pinned
on (mask 3, WORKERS 4, MIN 1), the block's overlay wrapped in `FailRead` (one key's reads fail with a storage I/O
error, counted). Asserts per case: the step's results and `take_fatal_error()` (fail-stop and its reason, what
the committer checks) equal between flag off and on; the expected fail-stop; one pass ran. Cases:

| case | L1 | act | accounts (trader order) | failing read | outcome (both arms) | non-vacuous checks |
|---|---|---|---|---|---|---|
| a: reader fault past the act cap | on | 1 | backstop, holder of unlisted market 77 | oracle aggregate of 77 (outside the mark table) | commits | off reads the key 0 times, on 2 (the pass); 1 fault fallback, 0 consumed |
| b: reader fault reached | on | 64 | same | same | fail-stop `oracle mark read: ...` | 1 fallback, the live loop latches it again |
| b': reached, no act | on | 64 | healthy, holder of 77 | same | fail-stop `oracle mark read: ...` | 1 fallback |
| c: balance `Err` past the act cap (L1, walk) | on / off | 1 | backstop, healthy | trader 1's balance | commits | off 0 reads, on 1; the `Err` dropped unconsumed at the act (`dropped` 2) |
| d: balance `Err` consumed (L1, walk) | on / off | 64 | healthy, healthy, backstop | trader 1's balance | fail-stop `liquidation step: ...` | `consumed` 2: trader 0's pre-value, then the `Err` from trader 1's pre-value |

RED on the first-round code (fallback absent), case a: flag on returned `fatal: Some("oracle mark read: state
error: io: injected read failure")`, flag off `None`. Green after the fix (run output in
`/tmp/s80-lto-green.txt` at the time; the gate below reruns it).

Golden takes the pre-value path (review issue 6): the three digest tests now end with `prevalue_path()`, which
prints the process-wide counters and asserts `passes > 0 && consumed > 0` when the flag is on with MIN=1 (and
`passes == 0` with the flag off). Measured: with MIN=1 the golden pre-values every window (scenario A: 36 passes,
933 accounts consumed, serial and engine; scenario B: 54 passes, 129 consumed; 0 fault fallbacks). With the
default MIN 64 the golden's windows are all under 64 accounts: 0 passes, so the default-config golden run does
NOT exercise the path; the MIN=1 run is the one that does.

Fix-round gate (ozarchy, one hold of the heavy lock 10:58-11:04, runner
`proto/results/liq-tail-offload-fix-hold.{sh,log}`; nextest `--cargo-quiet --status-level fail
--final-status-level fail`):

| run | result |
|---|---|
| `-p torus-bridge -p torus-state`, flag off (golden, liquidation_tests, the new fault test) | 1511 passed, 22 skipped, 0 failed, 0 flaky (67.8 s) |
| `-p torus-bridge`, `TORUS_S80_LIQ_PREVALUE=1 MIN=1 WORKERS=4` (golden asserts the path ran) | 1092 passed, 17 skipped, 0 failed (54.2 s) |
| golden binary, flag on MIN=1, `--no-capture` | 5 passed; counters above |
| golden + liquidation_tests + `test(liquidation)`, flag on defaults | 80 passed, 0 failed |
| golden binary, flag off | 5 passed; counters 0 |
| release torus-node build (compiles the `App::new` read) | ok |
| clippy `-p torus-bridge --tests` | 0 errors; one new `type_complexity` warning in the new test (fixed with a type alias, hold 2); the others are pre-existing lines |
| hold 2 (after the alias, 11:12, `proto/results/liq-tail-offload-fix-hold2.{sh,log}`): `test(liq_prevalue)` + clippy `-p torus-bridge --tests` | 3 passed; clippy 0 errors, no warning in a changed line (the two left, liquidation_step.rs:1206 and native_executor.rs:16585, are the pre-existing lines :1175 / :16561 shifted) |

## 5. Microbench (ubench_econ, engine proxy)

Method: `UB_MARKETS=300 UB_RUNS=5 UB_MARKS=1 TORUS_SETTLE_STREAMED=1` (ub.sh's defaults), one binary, flag 0 vs 1,
ABBA order (off, on, on, off, off, on) per set, each invocation = 5 runs, the median reported; delta of the means of
the 3 medians per arm. Sets r2 / w10 / s5k ran back to back in one hold of the heavy lock (09:00-09:04, runner
`proto/results/liq-tail-offload-ubbatch.sh`, logs `ub-liq-tail-offload-{r2,w10,s5k}-<i>-<arm>.log`). ab1 ran in the
earlier part of this session on the same binary (07:14-07:26, `ub-liq-tail-offload-ab1-*.log`).

engine ms / 1k fills (medians):

| set | env | off | on | mean off | mean on | delta |
|---|---|---|---|---|---|---|
| ab1 | walk 0 | 2.75, 2.76, 2.80 | 2.43, 2.46, 2.42 | 2.770 | 2.437 | **-12.0%** |
| r2 | `UB_MARK_WALK=0` | 2.77, 2.77, 2.79 | 2.42, 2.47, 2.41 | 2.777 | 2.433 | **-12.4%** |
| w10 | `UB_MARK_WALK=10` (marks move every block) | 3.02, 2.99, 2.99 | 2.43, 2.43, 2.44 | 3.000 | 2.433 | **-18.9%** |
| s5k | `UB_SENDERS=5000` (cell's sender count) | 2.30, 2.30, 2.28 | 1.96, 1.97, 1.97 | 2.293 | 1.967 | **-14.2%** |

tail(liq) ms / 1k fills (per run, all invocations of the arm): r2 off 0.48-0.52 / on 0.15-0.17; w10 off 0.72-0.78 /
on 0.19-0.22; s5k off 0.50-0.51 / on 0.17-0.23. exec/1k (engine minus the tail) is unchanged (r2, 15 runs per arm: off 2.21-2.53 mean
2.305, on 2.19-2.42 mean 2.293). The whole gain is the liquidation step: -0.33 ms/1k (walk 0) to -0.55 ms/1k (walk 10), i.e. 3.7-6.3
ms/blk at ~11.4k fills/blk. Spread: within-arm medians 2.77-2.79 / 2.41-2.47 (~2%); the delta is 6x the spread.

Fix round re-run (binary `ubench_econ-1a6f456edd610a23` md5 43b118a1, rebuilt 11:02:12 from the fix-round source
in the same lock hold; same method and env; ABBA off, on, on, off, off, on; logs
`ub-liq-tail-offload-{fx0,fx10}-<i>-<arm>.log`, 11:02-11:04):

| set | env | off | on | mean off | mean on | delta |
|---|---|---|---|---|---|---|
| fx0 | `UB_MARK_WALK=0` | 2.76, 2.75, 2.77 | 2.43, 2.40, 2.40 | 2.760 | 2.410 | **-12.7%** |
| fx10 | `UB_MARK_WALK=10` | 2.97, 2.98, 3.00 | 2.43, 2.45, 2.45 | 2.983 | 2.443 | **-18.1%** |

Per run, 15 runs per arm: fx0 tail(liq)/1k off 0.47-0.51 (mean 0.482) / on 0.14-0.17 (0.154), exec/1k off mean
2.295 / on 2.272; fx10 tail off 0.72-0.75 (0.741) / on 0.19-0.22 (0.205), exec/1k off 2.271 / on 2.253. The fix
costs nothing measurable (one `OnceLock::get` before and after the pass): the first-round numbers (r2 -12.4%, w10
-18.9%) are reproduced.

Provenance (review issue 5): the first round's ubench binary (built 07:10:29) predates commit 130036f7 (07:55) and
a native_executor.rs edit at 08:48, so the earlier line "built from the same non-test source" was not verified.
The reviewer's rebuild from b40f47f0 gave the same numbers, and the fix-round binary above was built from the
current source in the same hold as its runs, so the claim no longer rests on the old binary.

Attribution (earlier part of the session, same binary, one invocation each, order off, pool, win, win, pool, off):

| arm | engine medians | tail(liq)/1k |
|---|---|---|
| off | 2.78, 2.74 | 0.48-0.51 |
| `pool` (part A only) | 2.48, 2.56 | 0.19-0.20 (two outlier runs 0.29-0.30) |
| `2` (part B only) | 2.80, 2.73 | 0.45-0.49 |

Part A carries the gain (-8.7% engine on 2 rounds with outlier runs; tail 0.49 -> 0.19); part B alone is within
the engine spread (tail -0.04 ms/1k, ~0.45 ms/blk), and adds ~0.03-0.04 ms/1k on top of A (0.19 -> 0.16).

Workers sweep (default env, one invocation each, one lock hold, 09:13-09:15) (order off, 2, 4, 8, 16, 32, off, 16, 8, 4, 2; `proto/results/liq-tail-offload-wsweep.{sh,log}`):

| WORKERS | engine medians | tail(liq)/1k |
|---|---|---|
| off | 2.75, 2.73 | 0.48-0.49 |
| 2 | 2.57, 2.53 | 0.27-0.30 |
| 4 | 2.50, 2.44 | 0.18-0.20 |
| 8 (default) | 2.41, 2.41 | 0.15-0.17 |
| 16 | 2.37, 2.45 | 0.13-0.16 |
| 32 | 2.46 | 0.15-0.16 |

The tail fits ~0.09-0.12 ms/1k serial (window build ~0.07 + the loop's classification / row writes + the pool round
trip) + ~0.40 ms/1k / WORKERS. 8 keeps the default: 16 saves at most 0.02 ms/1k more and would double each node's
burst on a host shared by 3 validators.

## 6. Expected node effect and caveats

- Node: liquidation step 3.57 ms/blk (0.31 ms/1k fills at ~11.5k fills/blk, below ubench's 0.49: the node scans
  1,638 traders/blk at 2.18 us/trader). If the node keeps the ubench cut (65-75%), the exec thread saves ~2.3-2.7
  ms/blk of a ~45-55 ms block: +4-6% blk/s and matched/s if the exec thread stays the bottleneck.
- Risk the microbench cannot show: in a CL21 cell the 3 validators share the host's 32 threads and execute the same
  block at about the same time; each pre-value pass puts 8 jobs on its node's pool for ~1-2 ms. The pool's speedup
  in the node may be lower; only a cell shows it.
- The pass is skipped for windows under 64 accounts and costs one pool round trip otherwise.
- Correction (fix round, review issue 2): the earlier line "pre-values after an early act are wasted work on the
  pool, never on the exec thread" was wrong for latency. `run_jobs` holds the exec thread for the whole pass
  before the serial loop starts. When an account near the start of the window acts (a liquidation storm), the
  pre-values after it are dropped and those accounts are valued again live, so the whole pass is added to that
  block's step: about the pass's wall time, ~1 ms/blk at ubench scale (W8: the pooled part of the tail is ~0.05
  ms/1k fills = ~0.6 ms/blk plus the pool round trip), only in act-heavy blocks. Not mitigated in this
  prototype. A possible mitigation: skip the pass when pending or cooldown liquidation rows exist (the checks
  `liquidation_due` already does). It is not needed for the cell, and it may switch the pass off in steady state
  if the bench keeps pending rows.

## 7. Verdict

Built, exact (fix round: the reader-fault channel now gives the flag-off outcome too, proven by the fault test),
tests green (flag off and on, golden included and shown to take the path with MIN=1). Engine proxy -12.0..-18.9%
(delta of means, 6 ABBA sets, 3 rounds per arm each; fix round fx0 -12.7%, fx10 -18.1%), far beyond the ~3-5%
run-to-run spread. cell_worthy: yes. Cell env: `TORUS_S80_LIQ_PREVALUE=1` (default workers 8, min 64). Known
cost not mitigated: in act-heavy blocks the pass's wall time (~1 ms/blk at ubench scale) is added to the step
(section 6).

What the cell should read besides matched/s and blk/s: the node log line `TORUS_S80_LIQ_PREVALUE on=true` (logged
at startup by `App::new` since the fix round); the step's own histogram `torus_liquidation_step_seconds` (also `torus_exec_liquidation_step_seconds`)
(sum / count, off vs on: the microbench predicts -60..-75%) and the post-engine tail / exec p99; agreement (the
state hash) must stay valid, `torus_liquidation_scanned` / `torus_liquidation_acted` totals per block equal across arms.

## 8. Fix round (review of b40f47f0)

| # | review issue | resolution |
|---|---|---|
| 1 | P2: the pass can latch `ctx.reader_fault` for an account the loop never reaches (act cap / early `Err`), failing a block that commits with the flag off | Confirmed RED (fault test case a). Fixed in `liquidation_pass`: `had_fault` before the pass; a fault latched by the pass is taken out of the channel and the pass dropped (`pre = None`), so the loop runs fully live (29bfc705 path) and latches it again only if it reaches that account. Also covers a pass that returned `None` after another job latched a fault. Section 2 rewritten (the "fail-stop on either path" line was wrong). Test: 7 cases, flag on == off on results, fail-stop and its reason. |
| 2 | Perf claim: an early act makes the whole pass latency overhead on the exec thread | Report section 6 corrected (bounded by the pass's wall time, ~1 ms/blk at ubench scale, act-heavy blocks only). Pass doc comment says so. Not mitigated (skipping the pass when pending / cooldown rows exist could switch it off in steady state; not needed for the cell). |
| 3 | INFO line logged lazily, not at startup | `App::new` (torus-consensus `app.rs`) calls `torus_bridge::native_executor::liq_prevalue()` once next to the other startup flag lines: the `TORUS_S80_LIQ_PREVALUE on=<bool>` line is logged before the first block. |
| 4 | Doc nit users.rs "mask bit 2" | Now "mask bit 1, value 2". |
| 5 | Provenance of the first-round ubench binary unverified | Rebuilt from the fix-round source in the same hold as the new A/B (section 5); numbers reproduced. |
| 6 | Test gaps: reader-fault channel, `Err` from a consumed pre-value, golden taking the path | Fault test cases a/b/b' (reader fault), c/d (an `Err` dropped unconsumed / consumed from a pre-value, L1 and walk). Golden: `prevalue_path()` asserts the path ran with flag on + MIN=1 (36 / 54 passes, 933 / 129 accounts consumed) and shows the default MIN 64 does not take it in the golden. |

Nothing pushed or merged; no other worktree touched.

## 9. Cell A/B (120 s, 300-market CL21, 9j2 harness via TOOLS=best)

Setup. Binary `/home/oz/.cargo-target-s80-liq-tail-offload/release/torus-node` md5 `070f33779928ac0afa00059ae0e0f510`,
checked as built from the final commit 147781cb: worktree clean, `cargo build -q --release --locked -p torus-node`
under the heavy lock at 11:13-11:21 was a no-op (binary mtime 11:02:02 unchanged; the only file in 147781cb touched
after 11:02 is `liquidation_l1_tests.rs`, which is `#[cfg(test)]`). `release/bench-throughput` md5
`fa8fd4bd764b8b5aa6c427a3679c4f8b` (the bench tool for all arms). Driver log:
`/home/oz/projects/wt/novel-s80/proto/results/liq-tail-offload-cells-driver.log`. Plan was ABBA off-1, on-1, on-2, off-2.

Flag active (on-1, val0): `TORUS_S80_LIQ_PREVALUE on=true (...) on=true mask=3 workers=8 min=64`; val1 and val2 log
the same line too. off-1 logs `on=false`.

### Raw rows (cell.sh TSV)

label, validity, agreement, matched_s_avg, matched_s_first120, blk_s_avg, chain_ms, engine_ms/1k, commit p50, commit p99, exec p99, user ms/1k fills

```
s80-liq-tail-offload-off-1	ACCEPT	AGREE	252702.5	257428.0	21.0	34.83	2.64	510.1	7145.9	9666.3	19.807
s80-liq-tail-offload-on-1	ACCEPT	AGREE	251652.7	256429.5	20.8	33.64	2.53	525.6	6912.7	9569.6	19.838
```

### Mechanism counters (after - before /metrics, per node)

| cell node | liq steps | step ms mean | step s total | scanned / step | acted |
|---|---|---|---|---|---|
| off-1 val0 | 2958 | 3.197 | 9.455 | 1601.0 | 0 |
| off-1 val1 | 2958 | 3.162 | 9.352 | 1601.0 | 0 |
| off-1 val2 | 2959 | 3.181 | 9.412 | 1600.5 | 0 |
| on-1 val0 | 2941 | 2.084 | 6.129 | 1598.1 | 0 |
| on-1 val1 | 2940 | 2.102 | 6.180 | 1598.7 | 0 |
| on-1 val2 | 2940 | 2.130 | 6.263 | 1598.7 | 0 |

Summary phases (engine ms per block / margin / match / settle / post_engine_tail / cache_flush, exec_thread_busy_fraction):

| cell node | engine | margin | match | settle | post-engine tail | cache flush | exec busy |
|---|---|---|---|---|---|---|---|
| off-1 val0 | 28.82 | 9.54 | 2.19 | 10.39 | 3.34 | 5.04 | 0.803 |
| off-1 val1 | 28.58 | 9.42 | 2.16 | 10.39 | 3.30 | 5.11 | 0.801 |
| off-1 val2 | 28.44 | 9.31 | 2.16 | 10.38 | 3.32 | 5.14 | 0.797 |
| on-1 val0 | 27.69 | 9.62 | 2.12 | 10.41 | 2.19 | 5.08 | 0.769 |
| on-1 val1 | 27.60 | 9.37 | 2.15 | 10.54 | 2.21 | 5.12 | 0.769 |
| on-1 val2 | 27.43 | 9.47 | 2.19 | 10.26 | 2.24 | 5.05 | 0.764 |

Other headline fields, off-1 / on-1: matched_s_best60 267263.5 / 265107.3; blk_s_worst60 16.917 / 18.083;
fills per native block 10898.5 / 10949.2; wall ms per committed block 47.7 / 48.1; consensus timeouts 2 / 0.
