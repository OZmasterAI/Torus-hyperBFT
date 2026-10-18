# Bench queue (archy)

Benches run on archy only. 18c keeps this list current; archy reads it on pull. IDs (Q1, Q2, ...) stay fixed when an
item moves between sections.
Last archy benches: `ozarchy-campA` (campaign A + Q10 arm C, section 82), 2026-10-18; `ozarchy-q7m3s` + `ozarchy-q7cls` (Q7 run 2; run 1 `ozarchy-q7m3` + `ozarchy-q7cl`;
`item7-step2-native-ab.md`), `ozarchy-q8p2` (Q8, section 81)
and `ozarchy-q5c` (Q5, section 80) of `ozarchy-antispam-item6-pf1-2026-10-04.md`, 2026-10-17; before them `ozarchy-s58b600`, `ozarchy-s58dmt`, `ozarchy-s58safe` (sections 78-79), 2026-10-16.

## Order (owner s125, updated s127)

Every campaign uses the N = 11 bench genesis (rebuilt once with `FORCE=1` in setup). Arms that compare against main
share one binary and one all-off baseline inside a campaign.

**Standard load (s127, every cell unless its item says otherwise): the `hsdb-300m` shape.** 300 markets, rate 76,000,
SENDERS 5,000, BAND 5, CROSS_FRACTION 0.5, CANCEL_FRACTION 0.05, and the harness env of
`tools/matched-bench/campaign/driver-spec.example.json`: `BLOCK_CAP=400 RETRY_BUSY=1 MAX_IN_FLIGHT=4
OPEN_ORDER_BUDGET=900 ORACLE_FEED=1 IN_FLIGHT_TAIL=commit IN_FLIGHT_TAIL_DEPTH=1 CLOID_SHARE=0`; Classic book rows,
15 s checkpoints, oracle on. "Harness defaults" in any item means this load, NEVER `run-cell.sh`'s bare defaults (its
load settings are unset there). An item that changes one setting (e.g. `TORUS_BOOK_ROWS=3`, oracle off, a cell
length) keeps every other setting of this load. Check each cell's `cell=` log line against it before analysis.
Why: Q7 run 1 (s66) ran the bare defaults (MAX_IN_FLIGHT, IN_FLIGHT_TAIL, OPEN_ORDER_BUDGET, RETRY_BUSY, CLOID_SHARE
unset): ~140-156k matched/s instead of ~240-250k, deeper books, ~76k fills per block vs ~50k, ~4.9 vs ~2.9 ms exec per
1k fills. The A/B stayed fair but not at the load every other bench uses.

**Main as benched (s126):** main carries the three merged s123 experiments (KP, DB, PC; flags off) and archy's s58
bundle (merge 4156a83b: D `TORUS_SETTLE_DEFER_RESULTS`, M `TORUS_PARALLEL_MATCH_SETUP`, T `TORUS_FAST_SELL_TOP_UPS`,
all ON by default; 18c Codex torus review no findings, torus-adversarial approve; main gate 3923/3923). Build M from
main at or after 50d2316c.
- D, M and T ship on and are already measured off vs on (sections 78, 79): no re-bench, no DMT/DMTE arms, no =0 arm.
  Every arm runs main as it ships: D, M, T unset (= on).
- `TORUS_HEADER_RESEND_MS=${...:-250}` in `devnet/wsl/env.sh` and `devnet/docker-compose.yml` gives every cell 250 ms
  (code default 3 s; revisited pre-testnet, roadmap). Comparisons inside a campaign stay valid; comparisons with
  results from before the resend change do not (fewer lost-proposal timeouts move block rate and matched/s).
- Block rate from cells shorter than about 5 min is indicative only: under sustained load it falls to exec pace
  (5.9 -> 3.0 blocks/s by minute 5) while matched/s stays flat (section 79). matched/s stays the primary metric.
- Watch `no_proposal` in every replicate (1 in the s58dmt check cell, 1 in the s58safe soak).
- Soaks longer than about 20 min need account-margin top-ups or a lower load (the s58safe load ran out of margin).
- Not on main: B (`TORUS_PARALLEL_BOOK_SAVE`) and G (`TORUS_EXEC_POOL_GRAIN`) are parked (78.6); archy's E duplicate
  (bb62bab1) is not merged (E is main's `TORUS_EPOCH_FAST_PATH`).

**Driver generator (main d280a8fd):** a campaign check sees only its own checks file's variables and the driver's
globals, never a driver function's local (`$dur`, `$rc`, `$label`, `$t`, ...). Before the next campaign with a checks
file (e.g. `s58dmt-checks.sh`): freeze the new tools, regenerate the driver, run `DRIVER dry` and `DRIVER run smoke`; a
check that read a driver local now fails (use `$1` label, `$2` arm, or globals `R`, `AENV`, `AXE`, ...). `_DRV_` is
reserved. Warm-up gate on by default (`"gate": false` restores the old behaviour); 1 Hz soak sampler on every cell.

**Now (owner s127 order):**

1. **Setup:** build binary M from main, rebuild the bench genesis with `FORCE=1` (N = 11), run the Q5 smoke cell and
   its checks (Q5 below). Gates every later campaign. **Done s64 (setup PASS, M = e3d11cb9).**
2. **Q8. Item 6 Phase 5 step 0 profile** (campaign P, profile only, no A/B, ~1.5 h, `perf_event_paranoid=1` for this
   run only). Sent to archy directly (s125); it decides whether and how much of Phase 5 gets built.
   **Done s66 (ozarchy-q8p2, 9 / 9 valid; section 81): exec ~131 / ~149 ms per native block ON / OFF; flush_all 11%,
   Keccak 10-11%, save_order_books 6-7%, put_cf_raw 5%, borsh 5%, merge_disjoint 4%; exec off-CPU 20-26% (pool waits,
   one ~1 s pipeline_barrier per 15 s); hotstuff-algo blocked on the full exec channel 9-35%.**
3. **Q7. Item 7 step 2: READY (s126), ~2.5 h.** Arm A = main at the merge base **db84a1ad**; arm B =
   `feat/item7-step2` head **c567b1b3** (origin). C1-C10 built and Codex-clean, 4-validator devnet check done, main
   with the s58 bundle merged in, EVM double-inclusion fix and its follow-ups Codex-clean; gates nextest --workspace
   4021/4021, full cargo test 4022. Spec under Items, Q7. (Main after db84a1ad adds only docs, harness fixes and the
   behaviour-neutral exec-failed latch order fix, so arm A stays db84a1ad.) Both arms are fixed SHAs: campaign A
   does not move it. Moved ahead of campaign A (s127): Q7 gates the step 2 merge, the last item before the item 6
   Phase 5 build (roadmap order s121; item 7 step 3 comes after Phase 5, `item7-evm-lanes.md`).
   Run 1 (s66, `item7-step2-native-ab.md`): matched/s B/A mode 3 1.0082 [0.9646, 1.0538], Classic 0.9963 [0.9896,
   1.0031]; save_books Classic -0.22 ms, mode 3 +0.52 ms [-0.92, +1.96] (point estimate over the 0.3 ms bound, CI
   unresolved). It ran `run-cell.sh`'s bare load defaults, not the standard load (see above), so it does not decide the
   merge. **RERUN (owner s127): both shapes at the standard load, same binaries (A db84a1ad, B c567b1b3), same
   rounds, before campaign A (~2 h). The rerun decides the merge.**
   **Run 2 done s66 (ozarchy-q7m3s + ozarchy-q7cls, standard load, load check passed on every cell;
   `item7-step2-native-ab.md` Run 2): matched/s B/A mode 3 1.0264 [0.9497, 1.1093], Classic 1.0115 [0.9942, 1.0290],
   both pass; save_books per native block mode 3 +0.54 ms [-0.60, +1.69], Classic +0.62 ms [-0.34, +1.58], over the
   0.3 ms bound by point estimate (CI unresolved); B's blocks are ~7.7% bigger and per 1k fills B - A is ~0 (+0.0033 /
   +0.0003 ms). Owner / 18c decide the merge.**
4. **Campaign A** (~4 h 40): 7 arms on M, 300 markets, 600 s, 3 replicates, rotating order, native load only.
   **Shape: the `hsdb-300m` shape** (Classic, 15 s checkpoints, oracle on), NOT the harness default
   `TORUS_BOOK_ROWS=3`: mode 3 turns checkpoints off, and then Q1 and DB do nothing.

   | arm | env |
   |---|---|
   | base | all flags absent (list them in `absent_env`); D, M, T unset = on, as shipped (no =0) |
   | F (Q1) | `TORUS_EPOCH_FAST_PATH=1` |
   | PE (Q2) | `TORUS_PARALLEL_ENGINE=4` (N >= 2 workers; 0, 1 or anything else = off) |
   | PC (Q4) | `TORUS_PARALLEL_CANCEL_ALL=8` (unset, 0, 1 or garbage = the serial loop) |
   | KP (Q4) | `TORUS_EXEC_PARALLEL_KEYS=1` (only "1" turns it on) |
   | DB (Q4) | `TORUS_CKPT_DEFER_BOOKS=1`; check its `defer_books=true` info line in each node's log (it logs `false` if gated off) |
   | C (Q10) | `TORUS_SHARD_CUSTODY=1` (overrides `env.sh`'s pinned 0); fresh data dirs per cell and a free-disk check first (Items, Q10) |

   **Done s66 (ozarchy-campA, 22 / 22 valid, section 82): PC +4.51% [1.0392, 1.0511] clear win; F +1.50% [1.0019,
   1.0283] win; PE +1.03% [1.0025, 1.0182] marginal (+2% CPU per fill); KP +1.03% [0.9970, 1.0238] and DB +0.95%
   within noise; C (custody) -21.61% [0.7794, 0.7884] loss (140 ms per validate, shards ~1.7 GB per node per cell, not
   20 GB). Step 5 candidates PC + F (PE owner's call); step 6 not triggered.**

5. **Q4 winners together: does PE stack on PC+F?** (~1 h 20; owner s128): 2 arms, 300 markets, 600 s, 3
   replicates, alternating order (AB BA AB), native load only, the same shape and checks as campaign A. Binary: campaign
   A's M (`e3d11cb9`, node md5 `bf720f9f`, already staged); custody pinned 0 on both arms.

   | arm | env |
   |---|---|
   | PC+F (reference) | `TORUS_PARALLEL_CANCEL_ALL=8 TORUS_EPOCH_FAST_PATH=1` |
   | PC+F+PE | PC+F + `TORUS_PARALLEL_ENGINE=4` |

   PC and F both won in section 82 and go on by default; no all-off base and no PC-only arm (owner s128). Question:
   does PE still pay for its +2% CPU per fill with PC+F on? In section 82, PE alone gave native blocks/s +9.2%
   [1.05, 1.13] and exec block_ms -8.1% [0.88, 0.96], but PC already cuts exec block_ms by 45%, so PE's gain may not
   stack. Report paired ratios (as in 82.3) on matched/s, native blocks/s, exec block_ms, block p50 / p99 and CPU-s
   per 1M. matched/s keeps the win rule; PE's default is the owner's call on block speed vs CPU, so its secondary
   metrics matter as much as matched/s. KP and DB do not run (section 82: no gain on any metric; DB is parked for the
   book-depth scaling check).
   **Done s68 (ozarchy-q4w, 7 / 7 valid, section 83): PE does not stack on matched/s: PCFPE / PCF 0.9862 [0.9768,
   0.9957], a loss. Its block-speed gain stacks and grows: native blocks/s +15.7% [1.08, 1.24] (alone +9.2%), exec
   block_ms -16.8% [0.79, 0.88] (alone -8.1%), block p99 -7.0% [0.91, 0.95]; p50 -6.7% within noise; CPU per fill
   +2.4% [1.020, 1.029]. Blocks are 14% smaller (exec per 1k fills only -3.1%). no_proposal 0 in every cell. PE default:
   owner's call (off by the matched/s rule). Caveat: PC+F's 248k matched/s is below 82's PC alone (254k, cross-campaign)
   at 11.8 vs 6.5 native blocks/s; a paired PC vs PC+F pair would show whether F costs matched/s on top of PC.**
   **Owner s128: PE off by default.** It stays a node-local opt-in (`TORUS_PARALLEL_ENGINE=N`, read at startup,
   byte-identical outputs) and is a candidate for activation on the live testnet, after a WAN A/B (one host has no
   round trips to cap the block rate) and a mixed-flag agree check (2 nodes on, 1 off). Roadmap, "Pre-testnet decisions".
5b. **PC vs PC+F, paired** (~1 h 10; owner s128; done s68, before step 7): does F cost matched/s on top of PC (section 83
   caveat: PC+F 248k at 11.8 native blocks/s vs 82's PC alone 254k at 6.5, cross-campaign)? 2 arms on binary M
   (`e3d11cb9`, md5 `bf720f9f`), standard load, 300 markets, **600 s cells** (shorter cells overstate blocks/s, section
   79), warm-up w1 then 3 pairs in A B B A A B order; custody 0; PE, KP, DB off.

   | arm | env |
   |---|---|
   | PC (reference) | `TORUS_PARALLEL_CANCEL_ALL=8` |
   | PC+F | PC + `TORUS_EPOCH_FAST_PATH=1` |

   Report paired ratios (as in 83.3) on matched/s, native blocks/s, fills per native block, exec block_ms, block p50 /
   p99, CPU-s per 1M, timeouts and `no_proposal` (per 1,000 blocks too). F ships on regardless of a small matched/s
   cost if its latency gain holds (owner's call on the numbers); this run sizes that trade.
   **Done s68 (ozarchy-q5b, 7 / 7 valid, section 84): F on top of PC: matched/s PCF / PC 0.9872 [0.9702, 1.0046],
   -1.3% within noise (all 3 rounds below 1, so ~1% is likely). The latency gain holds and is large: native blocks/s
   +88.6% [1.71, 2.08] (6.38 -> 12.04), block p50 -73.6% [0.25, 0.28] (57 -> 15 ms), p99 -14.7% [0.78, 0.93]
   (1,025 -> 874 ms), exec block_ms -46.6% [0.46, 0.61] (147 -> 79 ms); blocks half the size (fills per block -46.5%),
   exec per 1k fills unchanged (0.998). CPU per fill +2.2% [1.020, 1.025]. Timeouts 8 vs 3 (0.69 vs 0.14 per 1,000
   native blocks), no_proposal 2 vs 0 (0.17 vs 0). Both arms reproduce 82 (PC 254k at 6.5) and 83 (PC+F 248k at 11.8)
   within 1%. F's ship-on rule (latency gain holds) is met; owner's call.**
6. **Q10 10-market cell** (conditional): only if campaign A's arm C shows no cost at 300 markets (Items, Q10). **Not triggered (section 82: C -21.6%); skip.**
7. **[SUPERSEDED s128 by Q11, owner: no r2/r3, N stays 11] Q12. Option S: larger EVM cadence N** (~1 h 45, n = 2; no node code, binary M; harness knob `EVM_BLOCK_INTERVAL`
   on main since 64c41f13; Items, Q12). Item 7 step 3: decides whether raising N is a usable stopgap before option 0.
   **Owner s128: every arm sets PC + F by env** (`TORUS_PARALLEL_CANCEL_ALL=8 TORUS_EPOCH_FAST_PATH=1`, the coming
   defaults; Q12's own N11 arm is the reference, so section 80's baseline is not needed) **and the EVM arms are
   E11 / E33 / E66** instead of E11 / E22 / E33: at PC+F's ~11.8 native blocks/s an EVM block every 11 heights comes
   ~3x as often in wall time as in Q5, so the barrier costs more and the useful N is higher. At 10 tx/s an N = 66
   block carries ~56 txs (66 / 11.8 s x 10), under the 142-tx cap; if the block rate is lower, check the cap rule.
   **Stopped s68 after r1 (ozarchy-q12, binary M `e3d11cb9`, section 85; n = 1 screen, no verdict): E66 could not
   take the load: the EVM mempool rejected 41% (`mempool full`), landed 3,115 < accepted 3,547 (check failed, campaign
   stopped), one block at the 142-tx cap; E33 lost 13% of its load the same way. Under EVM load native blocks/s fall
   12.99 (N11) -> 3.33 / 4.37 / 5.68 (E11 / E33 / E66), so N = 66 blocks average ~117 txs nominal. r1 matched/s
   N11 246,458, E11 225,060 (0.913), E33 242,841 (0.985, at 87% load), E66 247,708 (1.005, at 52% landed, invalid).
   Point estimate reads S-INTERIM (L(33) 1.5%), unconfirmed. Next: owner / 18c choose how E33 / E66 get the full
   load (more senders, lower rate, or fix the pool fill), then r2 (+ r3).**
7b. **Push-manifest threshold: the bench pin vs the code default** (~1 h 20; archy's s124 candidate, owner s128 agreed
   to the baseline shift). The node's default is 512 KB (`HASH_ONLY_PUSH_THRESHOLD`, `torus-network/src/bridge.rs`);
   `devnet/wsl/env.sh` pins 8 MB, so every bench so far pushed full bodies before the proposal and never used the
   hash-manifest + pull path that production defaults to. 2 arms on binary M, setting only: A =
   `TORUS_HASH_ONLY_PUSH_THRESHOLD=8000000` (env.sh as today) vs B = `524288`; both arms with the flags step 5 picks
   set by env (PC + F, plus PE if the owner turns it on). Standard load, 300 markets, 600 s, n = 3, alternating order.
   Report CPU-s per 1M (expected -3.8 to -4.9%), matched/s, native blocks/s, block p50 / p99, timeouts and
   `no_proposal`, and the push / pull counts (manifest vs body pushes, bodies pulled, pull fallbacks). If B holds
   matched/s and timeouts within noise, `env.sh` moves to the code default and later benches run it. One host has no
   WAN latency or bandwidth, so this says nothing about the WAN threshold (pre-testnet, measured on WAN).
   **Done s68 (ozarchy-q7b, binary M `e3d11cb9`, both arms PC + F, 7 / 7 valid, section 86): B (P512K) / A (P8M)
   CPU-s per 1M fills 0.9412 [0.9341, 0.9484] (-5.9%, 24.92 -> 23.46) and matched/s 1.0434 [1.0400, 1.0467] (+4.3%,
   246,971 -> 257,680; all 3 rounds 1.042-1.045); native blocks/s, block p50 / p99, exec block_ms within noise (12.50
   -> 12.19, 15 -> 15 ms, 856 -> 830 ms); node RSS -10% (CI spans 1). Timeouts 2 vs 5 (0.09 vs 0.23 per 1,000 native
   blocks), no_proposal 0 vs 0: within noise. B pushes 69% of pre-proposal sets as manifests (full-body volume 10.1 GB
   -> 0.12 GB per cell), receivers pre-warm ~83k bodies per cell, 0 pull failures / fallback pulls / recovery
   timeouts. Rule met: `env.sh` moves to the code default (18c); one host, says nothing about the WAN threshold.**
8. **Step Z (Q6 step 0 + zero-code screen): checkpoint interval 15000 vs 5000 ms** (n = 2 per arm ABBA, ~45 min plus
   setup; step 0 binary from `perf/q6-ckpt-slicing` **`9d770519`** (s128: Codex torus re-review of step 0 had one P2,
   a failed scrape corrupting `commit_db_split`, fixed in 84ea54cf + 9d770519, Codex quick review clean; Items, Q6).
   Decides GO / NO-GO / SHIP-Z for building checkpoint slicing (A1).
   **Done s68 (ozarchy-stepz, binary Z `9d770519` node md5 `0d234898`, both arms PC + F + push threshold 524288,
   n = 2 ABBA, 5 / 5 valid, section 87): no GO / NO-GO; the rule's "anything else" branch. W_db fell only 26.5% (407
   -> 299 ms per checkpoint, rounds 0.685 / 0.789; batch 108 -> 85 MB), short of the 40% bar, because attest heights
   (h % 100) already checkpoint Z15 every ~100 blocks (~5.8 s at ~17 blocks/s; the 15 s interval never fires). Within
   that step, G fell in proportion (497 -> 375 ms, -25%) and the in-window manifest wall fell 22% (7.56 -> 5.88 ms per
   call); X at 1 Hz (in - out) fell 11%. The step 0 split: the manifest put waits in the write queue behind the batch
   (pre/post + residual; mutex and cond-wait 0), i.e. "A1 aimed right". Not SHIP-Z: G ~375 ms, and Z5 costs native
   blocks/s 0.777 [0.555, 1.089] (16.97 -> 13.18), block p50 +15% [1.080, 1.236], p99 556 -> 780 ms, state-DB writes
   +10%; matched/s 0.991 [0.905, 1.085], CPU per fill flat. Not extended to n = 4 (more rounds of the same arms cannot
   reach -40%). Next, decided with 18c: a stronger zero-code arm (`TORUS_CHECKPOINT_MAX_BYTES` ~40-50 MB or 2,000 ms),
   or build A1 on this evidence.**
   **Q12 r1 (section 85) ran binary M, before item 7 step 2's EVM routing fixes (16f201c4 committed txs leave every
   pool, 0edfb154 routing per lane with re-send, 2cdc3ac7 drains skip parent-block txs): at N = 33 / 66 one validator
   proposed every EVM block (multiples of 3 with 3 validators), txs reached it by a single forward, and gapped nonces
   were drained and dropped, so the pool filled. Q11 (option 0's A/B on current main) measures today's EVM cost.
   Item 7 to check before genesis: with N = 11 and 11 validators, one validator may propose every EVM block.**
8b. **Step Z2: checkpoint size cap** (owner s128; ~45 min plus setup; zero code). Same binary as step Z (`9d770519`,
   md5 `0d234898`), same shape, flags and push threshold (PC + F by env, 512 KB), ABBA n = 2: Z15 (as step Z) vs ZC =
   Z15 + `TORUS_CHECKPOINT_MAX_BYTES=50000000` (step Z batches: mean 108 MB, max 208 MB, so ZC more than halves
   them). Same metrics and the same GO / NO-GO / SHIP-ZC rule as step Z (W_db must fall >= 40%; then GO needs X and G
   to fall >= half as much in relative terms, i.e. G 497 -> <= ~400 ms). Report also checkpoints per 100 blocks, the
   share triggered by the cap, native blocks/s, block p50 / p99, state-DB writes and Dirty / Writeback. A GO means 18c
   builds A1; a NO-GO drops A1 (the p99 windows are then not from the batch size).
9. **Q9. Data-dir growth: main vs main + RH4** (60 min soak per arm plus the idle phase, ~2.5-3 h; Items, Q9).
   **READY (s128): arm (b) binary = `perf/rh4-da-body-gc` `b2dd21a4`** (main e1be6c15 merged into 4514573f, no
   conflicts; workspace 4069 passed / 0 failed, clippy no new warnings). Owner s128: custody OFF by default until T3.1,
   so arm (b) runs custody off (Items, Q9, "Arm b depends on Q10"). `run-cell.sh` clears every `TORUS_*` variable
   before starting the nodes, so `TORUS_RETENTION_BLOCKS=1000` must go in the arm's `EXTRA_ENV` (`--extra-env`); a
   plain environment variable does nothing. Arm (a) = main `e1be6c15` (the merged main, so the arms differ only by
   RH4); set PC + F by env on both arms (the coming defaults). Moved here in
   s127: nothing in steps 2-6 needs RH4 (its GC only runs with `--retention-blocks`); only pre-testnet calls (prune by
   default, validator retention N) and the RH4 merge wait on it.
10. **Q6. 71.8 checkpoint slicing A/B**, only if step Z says GO and 18c then builds A1 (not built; Items, Q6). Run after steps 4-5 have settled the Q1 / Q4 defaults (both touch
   checkpoint code, so they move slicing's baseline).

Done / dropped: **Q5** native vs native + EVM (campaign C, done s64: E/N 0.876, -12.4% matched/s; EVM heights never
pipelined, ~0.5 s exec idle before each, section 80). **Campaign B** (propose-defer) dropped (owner s126; branch
parked); revisit rule and arms: `docs/plans/item6-phase5-impl.md`, "Parked for a decision at Phase 5", item 3.

If Q1 or `TORUS_CKPT_DEFER_BOOKS` becomes a default, re-check the checkpoint and save-books rows of the Q8 profile
(a short re-run, not a full profile).

## Items

- **Q1. Epoch-boundary fast path** (`TORUS_EPOCH_FAST_PATH`, in main since `efe9aa47`, default off). Runs in campaign A.
  - Arms: J (flag absent, list it in `absent_env`) vs F (`"env": {"TORUS_EPOCH_FAST_PATH": "1"}`). Native load only
    (a boundary block with an EVM tx stays serial).
  - Arm F took effect: `node-env-all.txt` shows the flag on all 3 nodes; `zgrep -c 'epoch fast path ON'
    val*.log.gz` gives 1 per node; `executing finalized block height=..00 ... pipelined=true` about 18-19 times per
    node per 600 s (J: 0).
  - Metrics, per node on heights `h % 100 == 0`: the gap from 'block done' at h-1 to 'executing finalized block' at
    h (J ~500-530 ms, F ~1 ms); the boundary block's run time (J 364-479 ms); matched/s (expected +1.2-2%). Also
    `exec_checkpoint_upkeep_seconds`, `flush_worker_checkpoint_seconds`, commit-interval p99.
- **Q2. `TORUS_PARALLEL_ENGINE`** A/B (setting only, no code). Runs in campaign A.
- **Q3.** Archy's own s124 candidates (for example the push-manifest threshold): tracked by archy, slotted by archy.
- **Q4. The s123 experiments on main** (flags off): `TORUS_PARALLEL_CANCEL_ALL` (merge 7f9bb3a9; flag-off allocations
  = main), `TORUS_EXEC_PARALLEL_KEYS`, `TORUS_CKPT_DEFER_BOOKS`. They run in campaign A, the winners together in step
  5. Branch notes: `/home/18c/projects/wt/exp-s123/<name>.md` (18c). The fourth, propose-defer, is parked (step 6).
- **Q5. Native vs native + EVM transfers** at N = 11 (`EVM_LOAD_RATE` and `EVM_LOAD_SENDERS`, off by default). Shows
  what EVM blocks cost native matched/s.
  - Uses the setup genesis (an old `testnet/genesis-weighted-full.json` without the `FORCE=1` rebuild keeps N = 5).
  - Build `torus-node`, `bench-throughput` and `torus-tx-flood` (release) into `$T`, then a smoke cell:
    `FORCE=1 EVM_LOAD_RATE=10 TARGET_DIR=$T tools/matched-bench/campaign/detach.sh evm-smoke
    ~/bench-results-matched/evm-smoke.log tools/matched-bench/run-cell.sh "$PWD" evm-smoke-10m 10 120 76000`.
  - Checks: `summary.json` `.evm_load` has `ok` true, `interval` 11, `off_cadence_blocks` 0, `landed` about equal to
    `accepted`, `rc` 0; `jq '[.evm_block_txs[][0] % 11] | unique' evm-load-scan.json` gives `[0]`; the cell still
    passes `health.py accept`. A cell that asked for EVM load and did not get it is REJECT in `summary.json`.
  - Then the A/B (campaign C): arms without and with `EVM_LOAD_RATE`, 300 markets, 600 s; compare matched/s, block
    time and the blocks at EVM heights. At 10 tx/s from 40 senders an EVM block holds ~34 transfers (cap 142 at 3M
    gas).
- **Q6. 71.8 checkpoint slicing** vs option A (= main), after the slicing build (section 74.6 next cells): 300
  markets, 600 s, n = 3 ABBA (4-5 per arm resolves a ~200 ms step). Success: in-window block p99 within ~100 ms of the
  out-of-window p99, and `on_committed_block` in-window = out. 18c adds a timer around `persist_commit_manifest` first.
  - s127 plan (design `docs/plans/q6-checkpoint-slicing.md` on `perf/q6-ckpt-slicing`, revision 2 after two adversarial
    reviews; owner accepted all picks): measure first. Step 0 (built, under review): per-checkpoint DB-write stage
    histograms `torus_exec_checkpoint_db_{write,wal,memtable,delay,pre_post,residual}_seconds` (+ `mutex`,
    `cond_wait`), start / end gauges `torus_exec_checkpoint_db_write_{start,end}_ms` and a `checkpoint db write` log
    line, the 1 Hz / 10 Hz Dirty-Writeback sampler (`SOAK_DIRTY_HZ=10`), and `torus_commit_manifest_seconds`.
  - Step Z (queue step 8): one step 0 binary, arms `"env": {"TORUS_CHECKPOINT_INTERVAL_MS": "15000"}` vs `"5000"`, ABBA,
    n = 2 (extend to 4 if ambiguous), hsdb-300m shape. Both arms set the new defaults by env (s128: PC
    `TORUS_PARALLEL_CANCEL_ALL=8` + F `TORUS_EPOCH_FAST_PATH=1`, plus PE if step 5 adopts it; custody 0) and the push
    threshold `env.sh` has after step 7b. Cut windows from the
    start / end gauges (DB-write span), not the scrape-widened windows. Report per arm: checkpoint batch bytes and
    DB-write s (W_db), manifest put and persist-wait p99 / max and > 250 ms counts in vs out (X), in-vs-out block p99
    gap (G), overall block p99, matched/s, state-DB bytes/s, Dirty / Writeback max in windows, the stage split.
    Rule (design section 5): GO if W_db falls >= 40% and X and G fall by at least half as much in relative terms;
    NO-GO if W_db falls >= 40% but X and G move < 10% or stay in the noise; SHIP-Z if Z alone meets the Q6 criterion
    with matched/s in the noise; otherwise extend or decide with 18c.
  - The A/B (queue step 10) runs only after GO and the A1 build: flag A/B on one binary (`TORUS_CKPT_SLICE_MB` 0 vs 4),
    n = 4 ABBA.
- **Q7. Item 7 step 2**, native load only, main vs step 2 (after the step 2 build and the 18c devnet check): the best
  bid/ask rows add up to one write per dirty market per block. Full spec: `docs/plans/item7-step2-impl.md`,
  "Archy native-only A/B". Two binaries (commit vs commit, no flag): arm A = main at the step 2 merge base, arm B =
  the step 2 head (18c gives both SHAs).
  - Genesis: the 300-market weighted genesis; arm B with the new step 2 keys absent (M = 0, big blocks off). EVM
    load off.
  - Build each binary once and use one genesis for both shapes.
  - Load: the standard load (Order, "Standard load"), in both shapes; only the book-rows mode differs.
  - Shape 1, mode 3 (the standard load with `TORUS_BOOK_ROWS=3`): warm-up cell, n = 4 ABBA rounds of
    120 s cells at 300 markets. No 10-market secondary (owner s126: step 2 adds one bbo write per dirty market per
    block, so 300 markets is the worst case).
  - Shape 2, Classic (the shipped shape): both arms `EXTRA_ENV='TORUS_BOOK_ROWS='`, 15 s checkpoints
    (`KNOB=TORUS_CHECKPOINT_INTERVAL_MS=15000`, as `tools/matched-bench/campaign-template.sh`; its `envok` Classic
    check), 300 markets, n = 4 ABBA. Mode 3 turns checkpoints off, so it alone would miss the bbo writes in
    `save_order_books`.
  - Report per shape: matched/s (primary), native blocks/s (indicative only, see above), `torus_exec_save_books_seconds`
    per native block, exec `block_ms`, block time p50/p99, CPU-s/1M.
  - Pass, both shapes: paired B/A matched/s 95% CI includes 1.0 or lies above it, and save_books rises by at most
    0.3 ms per native block. A fail blocks the step 2 merge (follow-up: B16's `saved_bbo` hit rate).
  - Doc target: `docs/perf/item7-step2-native-ab.md`.
- **Q8. Item 6 Phase 5 step 0 profile:** see the message sent to archy (s125) and `docs/plans/item6-phase5-impl.md`
  "Step 0" (on main).
- **Q9. Data-dir growth: main vs main + RH4** (s126, from s58safe, section 79): in archy's 60 min soak each node's data
  dir grew about 0.19-0.2 GB/min; open files 351 -> 667. 18c's code read (not measured): committed action bodies are
  kept twice and never deleted: DA bodies in `<data_dir>/consensus-db` (`cf_native_pending`, no production delete) and
  full block bodies in the state DB (pruned only with `--retention-blocks`, which the harness doesn't pass). The
  "no-load" phase still committed blocks full of margin-rejected orders (rejects are counted inside execution), so it
  was not idle. Open files follow the SST count (`max_open_files` unset). 18c ports the RH4 DA body GC to
  `perf/rh4-da-body-gc` (GC only on pruning nodes; window `max(N, 1000) + 128` blocks) with the harness env var
  `TORUS_RETENTION_BLOCKS`; 18c says when the branch is ready (SHA then).
  - Arm b depends on Q10 (s127). The branch is pushed as `origin/perf/rh4-da-body-gc` 4514573f, a backup, NOT the
    Q9 SHA. Custody OFF by default after Q10: arm (b) = that head with `main` merged in, custody off as specified.
    Custody stays ON: 18c first builds the RH4 shard-count fix (option d; today a validator-set shrink leaves
    n_old - n_new shards per body of the ~3 boundary blocks), and arm (b) also runs `TORUS_SHARD_CUSTODY=1` so the
    shard GC is measured too. Either way 18c merges `main` into the branch once, runs the gates and gives the SHA.
  - Arms (owner s126, two binaries): (a) main; (b) main + RH4 with `TORUS_RETENTION_BLOCKS=1000` (adds
    `--retention-blocks=1000`). RH4's GC is OFF on archive nodes (a short window would strand a node that was down,
    without state sync), so (b) needs the retention to do anything; 1000 is a test value (the GC starts within
    minutes; the devnet's 100,000 is about 5-9 h and would delete nothing in 60 min). FRESH data dirs for every cell
    (bodies written before the RH4 binary are never GC'd). 60 min each, 300 markets, hsdb-300m shape (Classic, 15 s
    checkpoints), margin top-ups or a lower load; then a TRULY idle phase of at least 10 min (load generator stopped).
  - Sample per node every 60 s: `du -sb` of `<data_dir>/consensus-db` and of the state DB separately, SST count per DB,
    per-CF sizes from each RocksDB `LOG`'s Compaction Stats (`cf_native_pending`, block bodies),
    `torus_rocksdb_pending_compaction_bytes`, L0 file counts, open fds (`ls /proc/PID/fd | wc -l`) and deleted-but-open
    fds (`ls -l /proc/PID/fd | grep -c deleted`, expect 0), `torus_da_bodies_gc_deleted` and `torus_da_gc_seconds`
    (b). The harness summary needs the 40 consensus-DB columns (archy's `fix/harness-upstream-s64`, merged before Q9).
  - Also report per arm: matched/s, block rate, commit latency p50/p99 (the GC runs on the commit path).
  - Pass: (b) holds both `consensus-db` and the state DB flat once past the window (about 1,128 blocks), with
    matched/s and block rate within noise of (a). Then validators prune by default (owner s126: only if Q9 proves it);
    production retention N is decided after Q9.

- **Q10. Shard custody on vs off** (s127, setting only, no code). `TORUS_SHARD_CUSTODY=0` is pinned in
  `devnet/wsl/env.sh` since perf A1 (8fa6ccdb, Jul 16), so every bench so far ran custody off; the node binary
  ships it ON (only "0" disables) and custody-on was never measured. Custody runs synchronously on hotstuff-algo
  twice per view (leader `block_build_mirror`, every follower's `validate_block` decode, before the vote). s124's
  model (n = 3, not measured): ~95-145 ms per block per node, 0 to -25% matched/s at 300 markets, worse at 10. At
  N = 11 (k = 4) the per-shard proof work grows about n^2/k, so expect more.
  - 300 markets: arm C of campaign A (same binary, genesis and base arm). 10 markets: one ABBA pair base vs C
    (n = 3, 600 s) after the Q4 winners round (step 6), only if arm C is within noise at 300.
  - DISK: main never deletes shards (`cf_native_shards` in `<data_dir>/consensus-db`). Each body writes n shards of
    ~body/k plus proofs, about 2.75x the body bytes at N = 11: rough estimate 20+ GB per node per 600 s cell (x 11 if
    all nodes share a host). Fresh data dirs per cell, delete them after, check free disk before each C cell.
  - Report per arm: matched/s (primary), native blocks/s, block time p50/p99, `torus_validate_block_custody_seconds`
    (expect ~0 on base), `torus_block_build_mirror_seconds` (leader), hotstuff-algo CPU, CPU-s/1M, consensus-DB write
    bytes and `cf_native_shards` size, `torus_native_da_shard_recovered`.
  - Decision it feeds (owner): a real cost -> ship custody OFF by default until the validator set is large (roadmap
    T3.1), and the RH4 shard-count fix waits for T3.1 (likely lazy serve-time shards: nothing stored, nothing to GC).
    Within noise at 300 and 10 -> keep it on, 18c builds the RH4 shard-count fix (option d: max of the old and new
    set's n for 64 blocks after a set change, kept in the GC's own batch), benches switch to custody on, and Q9's
    arm (b) runs custody on (Items, Q9).
  - **Decided (owner s128, on section 82: C -21.6% matched/s, 140 ms per validate):** custody OFF by default until
    T3.1; the RH4 shard-count fix waits for T3.1. Code default flipped on main `d6c4d0d2` (s128): only `TORUS_SHARD_CUSTODY=1` enables.

- **Q12. Option S: does a larger EVM cadence N recover Q5's loss?** (item 7 step 3, `docs/plans/item7-step3-design.md`
  sections 3 S and 6.2; no node code). Q5 (section 80): -12.4% matched/s at N = 11, from the barrier before every EVM
  block. Raising N (genesis `evm.block_interval`, governance 2..=100) is the zero-code alternative to option 0.
  - Binary: M (the Q5 binary `e3d11cb9`, or the campaign's M), unchanged.
  - Genesis change: the per-cell `devnet/wsl/genesis-3val.json` with `.evm.block_interval = N` (`EvmConfig.block_interval`,
    `crates/torus-genesis/src/lib.rs:141-156`; absent = `torus_types::DEFAULT_EVM_BLOCK_INTERVAL` = 11; 0 refused,
    lib.rs:382).
    - Prerequisite (tooling, ~15 lines + a test, 18c): `devnet/wsl/gen-3val-genesis.sh` applies `EVM_BLOCK_INTERVAL`
      with a jq patch like its `TIMEOUT_BASE_MS` step. `tools/matched-bench/run-cell.sh` lifts it from `EXTRA_ENV` like
      `TIMEOUT_BASE_MS` (run-cell.sh:906-908) and logs it. `evm_load.py` already takes N from the genesis
      (run-cell.sh:934).
    - Without the knob: patch `testnet/genesis-weighted-full.json` per cell (`jq '.evm.block_interval = N'`; the
      staleness check ignores `evm`, run-cell.sh:748-760) and restore 11 after.
    - Either way, check `.evm_load.interval` in every `summary.json`.
  - Arms (one binary, one genesis base):
    - N11: native only at N = 11 (no EVM load).
    - E11 / E22 / E33: native + `EVM_LOAD_RATE=10 EVM_LOAD_SENDERS=40` at N = 11 / 22 / 33.
    - No native arm per N: a height without EVM txs never barriers (`has_evm = !evm_transactions.is_empty()`,
      `app.rs:3110`; the predicate needs `!has_evm`, 3166), so native-only is the same at every N.
    - 33 is the top: at 10 tx/s an N = 44 block would carry ~130 txs (nominal), close to the 142-tx cap of a 3M block
      (`app.rs:2261`).
  - Shape: hsdb-300m as Q5 (Classic, 15 s checkpoints, oracle on, 300 markets, 600 s, BLOCK_CAP 400, MAX_IN_FLIGHT 4,
    commit tail, rate 76,000); D, M, T unset; header re-send 250 ms.
  - Cell order: warm-up w1 (N11, 300 s), then r1 N11, E11, E22, E33 and r2 E33, E22, E11, N11 (n = 2, rotating):
    8 cells of ~10 min, ~1 h 45 in all. If the queue is tight, n = 1 is a ~55 min screen. Extend to n = 3 if a rule
    below sits within 1.5 points of its threshold.
  - Checks per E cell (Q5's): `.evm_load.ok` true, `interval` = the cell's N, `off_cadence_blocks` 0, landed =
    accepted, `[.evm_block_txs[][0] % N] | unique` = `[0]`, `health.py accept`.
    - New: max txs per EVM block < 142; a cell at the cap is flagged (the cap, not the load, then sets the EVM rate).
    - Q5 reported 58.8 txs per EVM block, but 6,000 txs / 161 blocks = 37. Check which count the gaps script uses
      before reading the cap margin.
  - Report per arm:
    - matched/s (primary), and E/N = matched/s divided by N11's, paired by replicate;
    - blocks/s and native blocks/s (indicative);
    - EVM blocks per minute, txs per EVM block (mean, max);
    - EVM latency: the EVM height interval in s (p50, p99) and commit -> exec lag p50, giving the inclusion latency
      (interval / 2 + lag; a per-tx latency needs tx-flood send timestamps, not built);
    - exec idle before EVM blocks (mean, p50) and exec ms at EVM heights vs others (q5-gaps view);
    - `torus_exec_checkpoints` per minute and mean `torus_exec_checkpoint_layer_bytes` (does the forced checkpoint
      grow with N?);
    - `torus_exec_evm_seconds` per EVM block, CPU-s / 1M, timeouts, `no_proposal`.
  - Decision. L(N) = 1 - E/N(N); L11 comes from this campaign (Q5: 0.124).
    - **S-INTERIM** if L(22) <= 0.03 or L(33) <= 0.03: raising N recovers the loss at 2-3x the EVM latency. The owner
      may set that N for devnet / testnet until P2 (no code), and P2's slot may wait (question 2).
    - **STOPGAP** if L(33) <= L11 / 2 but above 0.03: record N = 33 (or 22) as the no-code fallback in case EVM traffic
      arrives before P2. Plan unchanged.
    - **REJECT** if L(33) > L11 / 2: S does not halve the loss even at 3x the latency. Dropped with data.
    - Either way, read the mechanism: the per-EVM-block cost (idle + extra exec ms) against the bytes per forced
      checkpoint. A cost flat in N means the barrier is not bound by the window (this doc's inference is wrong), and
      S then scales ~1/N.
  - Doc target: the next results-doc section; 18c copies the outcome into `item7-step3-design.md` section 3 S.

## Later

- HL-style storage for write-once, delete-by-age data (s127 design item, not built): HL keeps one file per height
  and prunes by deleting files older than 48 h (cron, `hyperliquid-dex/node` `pruner/`), so nothing is compacted and
  nothing counts what a file holds. Ours: full block bodies (`cf_block_bodies`, per height already: segment files,
  prune by unlink; first stop storing bodies twice, the s124 compact body record) and DA bodies (`cf_native_pending`,
  keyed by hash before a height exists: an append-only log plus a hash index, or RocksDB blob files as a
  config-only try). Gain is CPU and disk (s124: bodies ~15 s and DA ~113 s compaction CPU per 600 s cell), not
  matched/s.
- Big-block sizing with mixed load (native + swaps), only when big blocks are switched on (item 7 step 2 decision 1).
- Q5 rerun on the item 7 step 2 build (native vs native + EVM load): step 2 adds a lane-row read per EVM tx and the
  per-epoch fee rows. Only if Q7 shows a cost.
