# Bench queue (archy)

Benches run on archy only. 18c keeps this list current; archy reads it on pull. IDs (Q1, Q2, ...) stay fixed when an
item moves between sections.
Last archy benches: `ozarchy-p5s2` (step 8d, section 91), 2026-10-18; `ozarchy-campA` (campaign A + Q10 arm C, section 82), 2026-10-18; `ozarchy-q7m3s` + `ozarchy-q7cls` (Q7 run 2; run 1 `ozarchy-q7m3` + `ozarchy-q7cl`;
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

**Exec-path A/B merge rule (owner s128, after section 91):** any A/B that decides a merge of an exec-path change
judges, besides matched/s, exec ms per 1k fills and CPU-s per 1M fills: **exec-thread on-CPU ms per 1k fills**, native
blocks/s, block p50 / p99 and timeouts per 1,000 blocks. A change that adds exec-thread CPU per fill (CI above 1) or
worsens blocks/s or p50 / p99 (CI on the wrong side of 1) does not merge, even if the other rows pass: the exec thread
runs ~95% busy, so a few % more work there doubles block size and halves the block rate (section 91: +5% exec-thread
CPU per fill -> blocks/s 10.8 -> 5.2, p50 18 -> 92 ms).

**Main as benched (s126):** main carries the three merged s123 experiments (KP, DB, PC; flags off) and archy's s58
bundle (merge 4156a83b: D `TORUS_SETTLE_DEFER_RESULTS`, M `TORUS_PARALLEL_MATCH_SETUP`, T `TORUS_FAST_SELL_TOP_UPS`,
all ON by default; 18c Codex torus review no findings, torus-adversarial approve; main gate 3923/3923). Build M from
main at or after 50d2316c.
- D, M and T ship on and are already measured off vs on (sections 78, 79): no re-bench, no DMT/DMTE arms, no =0 arm.
  Every arm runs main as it ships: D, M, T unset (= on).
- `TORUS_HEADER_RESEND_MS=${...:-250}` in `devnet/wsl/env.sh` and `devnet/docker-compose.yml` gives every cell 250 ms
  (code default 3 s; revisited pre-testnet, roadmap). Comparisons inside a campaign stay valid; comparisons with
  results from before the resend change do not (fewer lost-proposal timeouts move block rate and matched/s).
- **Defaults change (on main since `3429855d`, s128; binaries built from it or later):** an arm that leaves PC and
  F unset gets PC (`TORUS_PARALLEL_CANCEL_ALL`, 8 workers) and F (`TORUS_EPOCH_FAST_PATH`) ON. A baseline arm
  without them must set `TORUS_PARALLEL_CANCEL_ALL=0 TORUS_EPOCH_FAST_PATH=0` (an `absent_env` entry for either name
  now means ON, not off). Header re-send is 250 ms both from the code default and from `env.sh` /
  `docker-compose.yml`, so `HEADER_RESEND_UNSET=1` no longer gives 3 s: an arm that wants 3 s sets
  `TORUS_HEADER_RESEND_MS=3000` (`=0` = off). Compare against results from before the merge only with the
  pre-merge arms' env spelled out. Binaries built before it (M `e3d11cb9`, Q9's `e1be6c15` / `b2dd21a4`, step Z's
  `9d770519`) keep the old defaults: their arms set PC + F by env, as in sections 83-87.
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
   **Done s68 (ozarchy-z2, binary Z `9d770519` node md5 `0d234898`, both arms 15 s + PC + F + push threshold 524288,
   n = 2 ABBA, 5 / 5 valid, section 89): no GO / NO-GO; the "else" branch, decide with 18c. W_db fell 72.7% (455 ->
   124 ms per checkpoint; batch 115 -> 45 MB mean, 207 -> 68 MB max) and X (manifest in - out) fell 47.6% (12.07 ->
   6.32 ms per call), but G did not move (637 -> 649 ms, rounds 1.225 / 0.830; in-window p99 ~1.1 s on both arms).
   ZC is confounded: the cap (counted from above) fires every ~3.7 blocks (26.8 vs 1.04 checkpoints per 100 blocks,
   96% cap-triggered), upkeep on E 0.8 -> 14 ms per block, state-DB writes 2.46x, and the node drops to native
   blocks/s 0.364 [0.278, 0.477] (15.91 -> 5.79) with block p50 13 -> 64 ms, p99 630 -> 1,152 ms; matched/s 0.955
   [0.911, 1.001], CPU-s per 1M +3.7%. Not SHIP-ZC. Z15 reproduces step Z's throughput (250,402 vs 250,599) but G
   637 vs 497 ms: use a same-campaign control. Options for 18c (89.7): drop A1 on G, a cadence-preserving cap
   (~80-100 MB), or build A1 on the manifest evidence.**
9. **Q9. Data-dir growth: main vs main + RH4** (60 min soak per arm plus the idle phase, ~2.5-3 h; Items, Q9).
   **READY (s128): arm (b) binary = `perf/rh4-da-body-gc` `b2dd21a4`** (main e1be6c15 merged into 4514573f, no
   conflicts; workspace 4069 passed / 0 failed, clippy no new warnings). Owner s128: custody OFF by default until T3.1,
   so arm (b) runs custody off (Items, Q9, "Arm b depends on Q10"). `run-cell.sh` clears every `TORUS_*` variable
   before starting the nodes, so `TORUS_RETENTION_BLOCKS=1000` must go in the arm's `EXTRA_ENV` (`--extra-env`); a
   plain environment variable does nothing. Arm (a) = main `e1be6c15` (the merged main, so the arms differ only by
   RH4); set PC + F by env on both arms (the coming defaults). Moved here in
   s127: nothing in steps 2-6 needs RH4 (its GC only runs with `--retention-blocks`); only pre-testnet calls (prune by
   default, validator retention N) and the RH4 merge wait on it.
   **Done s68 (ozarchy-q9, A = main `e1be6c15` node md5 `5ab8040e`, B = `b2dd21a4` node md5 `d44f8b3b` +
   `TORUS_RETENTION_BLOCKS=1000`, both PC + F + push threshold 524288 + custody 0, n = 1 per arm, 2 / 2 valid, section
   88): pass. Load = the standard shape paced to 250 actions/s (49.7k matched/s, 0 margin rejects over the hour; the
   unpaced load is in-flight-cap bound at ~251k matched/s and runs out of margin at ~minute 21). Over load minutes
   5-60, SST growth per node: A consensus-db 0.0222 + state 0.0203 GB/min (14 MB per 1M fills, the s58safe rate);
   B 0.0001 + 0.0007 GB/min. At 60 min: A SST 1.30 + 1.33 GB, B 0.03 + 0.25 GB; after shutdown A 3.7-4.1 GB per data
   dir, B 0.30-0.36 GB. GC deleted all 915,892 bodies per node, 0.18 ms per block (p99 1.35 ms) on the consensus
   thread. Idle (21-24 min, load stopped): SST flat in both arms; du grows ~0.004 GB/min per DB from WAL alone (the
   s58safe "idle" growth came from a reject-only load). matched/s 49,751 vs 49,733 (paced, equal by construction),
   native blk/s 26.50 vs 26.55, block p50 / p99 8.4 / 479 vs 8.6 / 479 ms, order-to-commit p50 / p99 159 / 582 vs
   157 / 580 ms, CPU-s per 1M 44.1 vs 43.9, 0 timeouts. fds max 234-240 vs 183-189, 0 deleted-open. One cell per
   arm, at a paced load below capacity: no CI, and it cannot show a GC throughput cost at saturation.**
8c. **Q8 short re-check: checkpoint and save-books rows with F on** (owner s128; after 8b; ~45 min plus setup;
   profile only, no A/B). Triggered by the rule below the queue ("If Q1 ... becomes a default"): F
   (`TORUS_EPOCH_FAST_PATH`) is on by default since main `3429855d`. Binary built from main `b4cdbfc1` or later (the
   new defaults: PC 8, F on, re-send 250 ms, all unset; 512 KB push threshold from `env.sh`; custody 0). Standard
   load, 300 markets, Classic, 15 s checkpoints, oracle on; the Q8 recipe (`perf_event_paranoid=1` for this run
   only), 2-3 cells of 600 s. Report against section 81 only the rows that F and the new defaults can move:
   checkpoint upkeep (`exec_checkpoint_upkeep_seconds`, `flush_worker_checkpoint_seconds`, the `pipeline_barrier`
   wait per 15 s window, which section 81 put at ~1 s in 3 of 4 cells), `save_order_books`, `flush_all`, exec
   off-CPU share and hotstuff-algo blocked on the exec channel; plus exec ms per native block and per 1k fills. Say
   whether Phase 5's remaining scope (steps 3 and 7) still targets the biggest rows.
   **Done s68 (ozarchy-q8re, main `fbe36391` node `cb8d8ad8`, nothing set, ONP, 600 s, n = 3, 4 / 4 cells valid,
   section 90).** `pipeline_barrier` 0.00 s in 9 / 9 off-CPU windows (81: 0.09-1.15 s per 15 s), 0 blocks
   `pipelined=false`; residual untimed 12.9-15.8 -> 6.0-7.7 ms per block. Upkeep 0.60-0.73 -> 0.86-1.02 ms per
   native block; flush-worker checkpoint 670-788 -> 804-935 ms each (on W). Exec on-CPU 131-133 -> 75-83 ms per
   native block, mostly smaller blocks (native blk/s 5.6-5.9 -> 7.7-9.9, fills/blk 36-41k -> 25-32k); per 1k fills
   2.99 -> 2.46-2.67 ms. `save_order_books` 7.7-8.0 -> 5.6-5.9 ms per native block, 0.24-0.27 -> 0.22-0.23 ms per 1k
   fills; `flush_all` 14.1-14.6 -> 10.4-10.9 ms (13-14% of exec, still the largest). Exec off-CPU 20-22% -> 25-29%
   (pool waits); hotstuff blocked on the exec channel 30-33% -> 0-26% per window (6 of 9 windows 19-26%). matched/s
   259.2k / 265.3k / 259.6k, block p50 / p99 21-39 / 892-938 ms, CPU-s per 1M 23.0-23.5. Phase 5: step 3 targets
   a mid-size row (save_books 7%); the larger ones are `flush_all` (step 2, 8d), Keccak 12% and the pool waits.
8d. **Phase 5 step 2 A/B: main vs step 2** (owner s128; right after 8c; ~1 h 10). Does step 2 (`Users` mutated in
   place, write-through, dirty-set drain) speed up execution in a real run? The builder's release microbench showed
   the exec thread only level with Phase 4 (~160 ms per write set) and ~75 ms saved on the flush worker. Two binaries:
   A = main `d3cab057` (step 2's base), B = `feat/item6-phase5-step2` `1e51c8b9` (Codex torus-adversarial round 2
   approve; 10k differential 12 configurations no divergence). Both predate the new code defaults, so both arms set
   `TORUS_PARALLEL_CANCEL_ALL=8 TORUS_EPOCH_FAST_PATH=1` and `TORUS_HASH_ONLY_PUSH_THRESHOLD=524288` by env; custody 0.
   Standard load, 300 markets, Classic, 15 s checkpoints, oracle on, 600 s cells, warm-up then A B B A A B (n = 3).
   Report paired ratios (as in 83.3) on matched/s, exec block_ms, exec ms per 1k fills, CPU-s per 1M, native
   blocks/s, block p50 / p99, the flush-worker time per block, `flush_all` / `merge_disjoint` / `save_order_books`
   ms per block if the timers exist, node RSS mean / max, and `torus_exec_evm_scope_typed_writes_total` (expect 0).
   Rule: exec ms per 1k fills and CPU per fill must not regress (CI not above 1) for the merge; a gain decides
   whether the drain encoding moves to the flush worker next.
   **Done s68 (ozarchy-p5s2, A `d3cab057` node `0bc9880e`, B `1e51c8b9` node `83052e22`, PC + F + 524288 by env,
   600 s, n = 3, 7 / 7 cells valid, section 91).** Rule, read literally: met. Exec ms per 1k fills 1.0064 [0.9959,
   1.0171], CPU-s per 1M 0.9950 [0.9800, 1.0102]. No speed-up: exec thread on-CPU per 1k fills +5.0% [1.0327,
   1.0681]; cache-flush (`flush_all` / in-place apply) per 1k fills +44% [1.41, 1.47]; B's drain 6.4 ms per native
   block (0.130 ms per 1k fills); flush worker per 1k fills only -9% [0.86, 0.96]. B flips to big blocks from minute 1
   (3 / 3): native blk/s 10.80 -> 5.17 (0.48 [0.32, 0.70]), fills / blk x2.1, block p50 18 -> 92 ms, p99 872 -> 1,192
   ms (+37% [1.25, 1.49]), timeouts 0.36 -> 1.39 per 1,000 blocks, RSS +19% mean / +23% max (CI spans 1). matched/s
   258.8k -> 256.8k (-0.7%, CI spans 1). `evm_scope_typed_writes` 0. `merge_disjoint` has no timer. No gain, so the
   drain trigger is not met; moving the drain to the worker would take back most of the exec-thread +5%. Owner / 18c:
   merge call (latency regression).
8e. **Keccak and exec pool-wait attribution from 8c's profiles** (owner s128; after 8d; analysis only, no new cells;
   ~30-60 min). Section 90.5: Keccak hashing is ~12% of exec (outside the Phase 5 plan, callers unknown) and the exec
   thread waits on its own worker pool 16-18% of wall (`run_jobs` + match setup 2.4-2.8 s per 15 s, up from ~1.5 s).
   From the existing `ozarchy-q8re-300m-r{1,2,3}-ONP` `perf.data` (call graphs; `perf script --no-inline` as in 90):
   (a) Keccak by caller, as % of exec and ms per native block: state / trie roots, the running hash, action and order
   hashing, signature recovery, CF row hashing, EVM; name the top 3 call paths with file:function; (b) the pool waits:
   which job kinds the exec thread waits on (match setup, settle passes, cancel-all, book save, flush), per 15 s and
   per native block, and the slowest job's share of each wait (straggler vs too little parallel work). Report whether
   each top item has an obvious fix (cache, batch, skip, parallelize) and its upper-bound gain. Owner / 18c decide
   what enters a plan.
   **Done s68 (ozarchy-q8re analysis, section 92, no new cells; W1 Keccak by neighbour sample + code order, since
   99-99.9% of Keccak samples have no caller in the fp stacks; W3-W5 switch records for the pool waits).** Exec
   Keccak 8.75 / 9.53 / 10.39 ms per native block (11.6-12.5%). It is two hashes over the same ~10.8 KB per action:
   the trust-cache key pre-pass (`verified_cache_key_with_scratch`) 4.25 / 4.55 / 4.63 ms and the anti-MEV sort key
   (`sort_native_actions_indexed` -> `action_sort_key`) 3.98 / 4.18 / 5.11 ms; 0.5-0.8 ms unsplit. Roots, running
   hash, row / level hashing, EVM and ecrecover: 0 exec samples. Pool waits 16.9-18.9 ms per native block (18-19% of
   wall): match setup 7.3-8.1, settle plan 4.1-4.6, cancel-all 3.6-4.4 (~5 jobs, too little work), and `par_recover`
   1.9-2.2 (all trust-cache hits, 0 ecrecover samples: pure overhead). No book-save or flush jobs. Not stragglers:
   the slowest job is on CPU for 45-58% of the wait, the last finisher is preempted 15-38% (shared host), and the
   median worker starts 13-19% in. Upper bounds, ms per native block: skip the key pre-pass ~5.3, move the sort key
   upstream ~5.1-6.3, one absorb for both ~4.1-4.9, skip an empty `par_recover` 1.9-2.2, match / settle / cancel-all
   waits down to the slowest job 4.0-4.3 / 2.1-2.3 / 1.5-2.1. Owner / 18c: plan entry.
8f. **1M-account RAM check** (owner s128; next while the queue is otherwise empty; item 6 Phase 5's revisit trigger 3
   and a pre-testnet check; the genesis cell of Phase 5 step 7, run on main without Phase 5 steps 2-3). Binary built
   from main `477ba3e5` or later (code defaults: PC 8, F on, re-send 250 ms; 512 KB from `env.sh`; custody off).
   Genesis: 1M accounts with ~1M resting orders spread over the 300 markets (Phase 5 plan section 1 "genesis cell";
   section 4 estimates ~8 GB of state for 1M mixed users; no number yet for 1M orders). If the genesis tooling cannot
   produce this yet, report what is missing (generator, `FORCE=1` path, account keys, margin) before building
   anything; 18c decides. Measure per node: RSS and jemalloc allocated at ready (after boot / load of genesis), during
   10 min of the standard load, and after it; data dir size; boot -> ready time; restart -> ready after SIGTERM and
   after SIGKILL (gate: <= 60 s) and replay correctness (AGREE, one digest); block p50 / p99 and matched/s during the
   load window vs a 300-market standard cell on the same binary. One cell per kind is enough (n = 1): this is a
   capacity check, not an A/B.
   **Update s128:** tooling (BULK_COUNT pass-through + FUNDED_BULK_END, a bench-throughput pre-load command, a
   PRELOAD= phase) is built by archy on a branch for 18c's review. Node fix first: the boot-time `all_accounts()`
   emptiness check becomes a single-key probe (`fix/boot-empty-db-probe`, 18c gives the SHA); run 8f on a binary
   with it (optional: one restart on the current main binary for the before number). Staged: (1) boot ONE node on
   the 1M genesis, no load: RSS and jemalloc allocated at ready, boot time; confirm the host's RAM. (2) Only if one
   node stays under ~15 GB, run the 3-node cell; watch free memory and swap, and stop if it swaps (swapped restart
   times are meaningless). (3) If it does not fit: stop and report; 18c and the owner pick 500k on archy's host or a
   capacity-only 3-node cell (RSS, restart, replay; no throughput) on 18c's 94 GB host.
   **Done s68 (ozarchy-8f, main `76204a69` node `7f91438d`, tools `c2336e70`; perf doc section 93).** Stage a, one
   node, 1M accounts: ready 34.2 s (before fix 34.3 s), RSS 893 MB, jemalloc 350 MB. 1M orders pre-loaded in 25 s:
   peak RSS 2.6 GB; restart with the books 2.0 s (SIGTERM) / 11.4 s (SIGKILL, a ~10 s view timeout): gate passed.
   Stage b, 3 nodes, 1M accounts + 1M orders, standard load 600 s: RSS 6.6-7.8 GB mean, 8.1-9.2 GB max per node
   (standard cell 4.8-5.9 GB), host >= 26.6 GB free, no swap. After the load: restart 3.0 s (SIGTERM) / 3.5 s
   (SIGKILL); AGREE; digest 6.6 s per node. FINDING: matched/s -14.3% (256k -> 219k), blocks/s 13.3 -> 3.0, p50 15 ->
   240 ms, p99 774 -> 1,416 ms; `save_books` x19 per block at 1M resting orders. Stage c not needed for capacity;
   owner / 18c: the deep-book throughput cost (section 93.7).
8g. **Exec hashing quick wins A/B** (owner s128; right after 8f; ~1 h 10). Section 92's fix on `perf/exec-hash-once`
   `70993fa9` (from main `25c5939f`; Codex torus-adversarial approve: 16,160 differential hash cases, keys byte-identical):
   the sort key and the trust-cache key come from one encoding of each action, signature recovery is skipped when
   every action hits the cache, the EIP-712 domain separator is cached. Release microbench, verify + sort per block:
   13.1 -> ~5.2 ms (100 actions x 239 orders); 61.6 -> 50-54 ms (24k one-order actions). Two binaries, both with the
   new code defaults (nothing set; 512 KB from `env.sh`; custody 0): A = main `25c5939f`, B = `70993fa9`. Standard load,
   300 markets, 600 s cells, warm-up then A B B A A B (n = 3). Judge by the exec-path merge rule above: exec-thread
   on-CPU ms per 1k fills must fall (CI below 1), and native blocks/s, block p50 / p99 and timeouts per 1,000 blocks
   must not get worse; also report matched/s, CPU-s per 1M, fills per native block and the verify / sort / recovery
   phase timers if present.
   **Done s68 (ozarchy-p8g, A `25c5939f` node `d9c5d63e`, B `70993fa9` node `af40fa06`, both arms on A's bench
   `24939967`; perf doc section 94).** Merge rule met on every row. Exec-thread on-CPU ms per 1k fills 2.823 -> 2.695
   (0.9546 [0.9358, 0.9738]); native blocks/s 11.03 -> 15.34 (1.3931 [1.1215, 1.7305]); block p50 17 -> 14 ms
   (0.8292 [0.5409, 1.2712]), p99 861 -> 653 ms (0.7514 [0.5043, 1.1196]); timeouts per 1,000 native blocks 0.351 ->
   0.072. matched/s +1.5% and CPU-s per 1M +0.4% (both CIs span 1); exec ms per 1k fills -2.7% [0.9621, 0.9850];
   fills per native block 22,759 -> 16,799. Timers: verify (`torus_exec_verify_seconds`, includes recovery) 5.95 ->
   3.68 ms per native block; the sort has no timer and sits in the untimed residual, 5.40 -> 0.42 ms. Verify + sort per
   1k orders placed -51% (0.4864 [0.4816, 0.4913]); ~9.1 -> ~4.4 ms at 100 x 239 orders vs the microbench's 13.1 ->
   ~5.2. Owner / 18c: merge.
8h. **RH4 GC cost at full load** (owner s131; right after 8g; ~1 h 15). Q9 (section 88) ran paced below capacity, so
   it could not show a GC cost at saturation. RH4 is merged: main `687eac9e` (retention off by default; the GC runs
   only with `--retention-blocks`). One binary built from `687eac9e` (code defaults, nothing set; 512 KB from `env.sh`;
   custody 0). Arms: A = no retention; B = `TORUS_RETENTION_BLOCKS=1000` in the arm's `EXTRA_ENV` (`--extra-env`;
   run-cell clears plain `TORUS_*` env). Standard unpaced load, 300 markets, 15 min cells (margin lasts ~21 min), warm-up
   then A B B A (n = 2). The GC starts after `max(N, 1000) + 128` blocks (~1.5 min at ~13 blocks/s), so report the
   window after the first GC pass. Judge by the exec-path merge rule above: native blocks/s, block p50 / p99 and
   timeouts per 1,000 blocks must not get worse in B; also matched/s, CPU-s per 1M, `torus_da_gc_seconds` (mean, p99,
   per block), `torus_da_gc_held_passes` by reason (expect ~0 outside start-up), bodies deleted, SST / data-dir
   growth per arm. Not covered: the GC's tail decode with EVM txs (each pass decodes the 2-3 uncommitted tail blocks;
   the standard load is native only).
   **Done s68 (ozarchy-p8h, main `687eac9e` node `f38d132e`, bench `c1210126`, A no retention vs B
   `TORUS_RETENTION_BLOCKS=1000`, w1 then A B B A, 900 s; perf doc section 95).** All 5 cells AGREE, 111 CHECK OK, 0
   FAIL; retention on in every B node only. Window after the first deleting pass (+82 / +90 s). No cost per fill:
   exec-thread on-CPU ms per 1k fills 2.829 -> 2.837 (1.0026 [0.9283, 1.0828]), CPU-s per 1M 23.83 -> 23.81, matched/s
   257,139 -> 259,772 (+1.0%, CI spans 1). GC 1.51 / 2.09 ms per native block (~1 pass per block, 107 / 147 bodies),
   ~19.5 ms per s on the consensus thread, p99 ~11 ms, a few passes 51-205 ms; held passes 3 (`dispatch_behind`, r2-B)
   else 0; SST growth A 0.200 -> B ~0 GB/min per node (du 0.229 -> 0.058, WAL). Block rows split by pair: r1 same
   regime, B not worse (blk/s 12.61 -> 13.12, p50 15.0 -> 15.3, p99 830 -> 793 ms, timeouts per 1k 0.325 -> 0.219); r2-B
   ran the big-block regime from minute 1, before its first deleting pass (9.32 vs 16.42 blk/s). Arm means blk/s 14.52
   -> 11.22, p50 14.1 -> 20.4, p99 715 -> 853 ms, timeouts per 1k 0.175 -> 0.176; every CI spans 1 (formally met, but
   n = 2 cannot separate GC from the regime spread). Owner / 18c: one more A B B A to settle the blocks/s and latency rows.
8i. **Deep-book follow-up to 8f** (owner s131; after 8h; ~1 h 30-2 h, estimate; a pre-testnet check). Section 93's
   M cell changed two things at once (1M accounts and 1M resting orders), and restarts ran only after the load. Same
   node binary as 8f (main `76204a69`, md5 `7f91438d`) so the cells line up with section 93; tools `c2336e70` or
   main if it has the pre-load by then; section 93's hand-written driver (`ozarchy-8f-b/campaign.sh`, per-cell genesis,
   swap stop rule). Standard load, 300 markets, 600 s, n = 1 per cell. (1) **Depth sweep:** the 1M-account genesis
   with `PRELOAD=` 0 / 250k / 500k / 1M: matched/s, native blocks/s, block p50 / p99, timeouts, exec ms per block,
   `save_books` ms per block and per 1k fills, fills per native block, resting depth during the load, RSS. Question:
   does the 8f slowdown follow the resting depth (orders) or is it there at 0 pre-load (accounts)? (2) **Profile:**
   during the 1M cell, a `perf` profile of the exec thread as section 92 did, split `save_books` into its parts (book
   row encode, level hashing, DB writes, other). (3) **Crash-kill at 1M:** one more 1M cell with `CRASH_KILL_AT_S`
   (SIGKILL val1 mid-load, restart): kill -> first commit, replayed blocks, catch-up time, AGREE and one digest, and
   matched/s around the kill. If the depth curve is unclear at n = 1 (e.g. 0 and 1M within the S-cell spread), report
   it and 18c decides on replicates.
   **Done s68 (ozarchy-8i, main `76204a69` node `7f91438d`, bench `0a030a0c`, tools `c2336e70`, 1M-account genesis
   reused (`a644d0a7`), code defaults, 600 s, n = 1; perf doc section 96).** Depth, not accounts. P0 (1M accounts,
   no pre-load) = the standard cell: 263,708 matched/s (S 256,224), save_books 0.217 ms per 1k fills (S 0.218),
   CPU-s per 1M 23.16 (S 23.56), 10.5 blocks/s (standard spread 9.3-16.4). PRELOAD 250k / 500k / 1M: matched/s
   251,920 / 244,017 / 221,362 (0.955 / 0.925 / 0.839 of P0), blocks/s 3.83 / 3.42 / 2.91 (big-block regime from
   250k), p50 177 / 223 / 243 ms, save_books 37.1 / 49.9 / 78.9 ms per block = 0.576 / 0.719 / 1.070 ms per 1k fills,
   linear in mean resting depth (0.21 / 0.75 / 1.04 / 1.61M; +0.61 ms per 1k fills per 1M). P1M repeats 8f's M within
   0.9%. 0 vs 1M is clear at n = 1; the middle points are one cell each (18c: replicates only if needed). Profile
   (user space, paranoid 2 kept; no frame pointers, so leaf + neighbour rule): save_books 83 ms per block at 1M =
   borsh encode of the whole book 73% + output-Vec memmove 15% + jemalloc 3% + journal drop 2% + overlay put 0.1-0.2%
   + hashing 0 (Classic) + not sampled 6-7%. Crash-kill at bench+300 s: chain halts (3 validators), val1 replays 76
   blocks (applied marker 2,144 vs committed 2,220) in 41.4 s, first commit 48.4 s after the kill, at par at ~50 s;
   ~28 s of zero fills, matched/s 226k before, 193k in the first minute after. AGREE, one digest `a99ad5c1`; REJECT
   on liveness (30 s stall) and the crash gate (marker 12 blocks beyond the exec queue, gate allows 2; plus a
   false-positive panic count on an INFO line in `crash-kill.sh`). No swap growth, host >= 22.1 GB free, highest
   node RSS 9.9 GB (P1M) / 10.3 GB (CK).
8j. **[DROPPED s131, owner] Item 7 option 0 G1b flag-off cost.** Option 0 is fully built (G0-G4), so the early
   warning is moot; its question (flag-off cost vs main under EVM load) moved into 8l as arm M.
8k. **Explore workflow: new high-gain ideas, build and bench the best** (owner s131; starts right after 8i if 8j's
   SHAs are not posted by then, and once started it runs to the end without pausing; 8j then runs after 8k; ~a day, bench time <= ~4 h). Run the prompt in `docs/perf/8k-explore-workflow.md` as one
   multi-agent workflow (owner opt-in given): survey what is already explored, fan-out ideation (consensus, execution,
   storage, network / DA, crypto, wildcard incl. theoretical), adversarial review and ranking, build the top 1-2 on
   `exp/8k-<name>` behind a flag (tests first, microbench gate), A/B n = 3 under the exec-path merge rule, write-up.
   Never merges to main; keeps off item 7 option 0's and RH4's code.
   **Done s68 (8k, phases 1-5 run, verdict pending; perf doc section 97). 21 ideas (4 dead, 15 backlog, 2 picked;
   top 5: L3.3 1.60, L2.2 0.84, L3.1 0.54, L5.1 0.47, L2.1 0.37; `docs/perf/8k-explore-ideas.md`). L3.3 exact-size
   Classic book encoder built on `exp/8k-book-fast-encode` `4b916ecf` (`TORUS_BOOK_FAST_ENCODE=1`, default off;
   nextest 4161 / 0 failed; flag on == off on a 130-block app stream; microbench P1M encode -30 to -36% with
   spread book sizes, -5.5 to -18.7% with equal sizes; the ranker's size-pass design -9 to -16%, dead). A/B, A = main
   `0f5bef38` node `0d24371e`, B = `4b916ecf` node `b0a4c2f1`, 600 s, A B B A A B: ozarchy-8k standard 7 / 7 cells
   valid by driver (raw matched/s A 259,917, B 261,123); ozarchy-8kd deep book (PRELOAD=1000000) 7 / 7 valid by
   driver (raw A 224,195, B 227,344, +1.4%). Analysis not run, so no verdict yet: run
   `ozarchy-8k-run/analyst/8k-analyse.py` on both run dirs, then judge by the exec-path merge rule. L2.2 no-merge position flush written but
   not compiled (worktree `wt/8k-settle-no-merge`, uncommitted). Next best idea: L3.1.**
   **Final (s131, section 97.8, archy's 8k report; the Done line above was written mid-run):** L3.3 A/B, both shapes
   n = 3: save_books per 1k fills 0.845 [0.762, 0.938] standard, 0.853 [0.836, 0.872] deep; exec-thread on-CPU per 1k
   fills 0.990 (spans 1) standard, 0.968 [0.962, 0.973] deep; matched/s, blocks/s, p99 span 1. Codex torus review of
   `4b916ecf`: no findings. L2.2 committed and pushed (`exp/8k-settle-no-merge` `dbe14b21`, workspace 4163 / 4163;
   microbench -5.3 / -7.0 ms per block at 16.8k / 22.8k fills); Codex torus review P1: aliased opaque position rows
   make the market caches overlap, so flag on != off and the release state is nondeterministic (unstable sort); fix
   before any merge (prove disjointness incl. clean entries, else fall back to merge_disjoint). The 8n screen stays
   valid (benches create no aliased rows).
   **Pacing rule (owner s131, every step from here on):** run all cells of a step from ONE detached driver, back to
   back, with at most ~60 s idle between the end of one cell and the start of the next (teardown, the quiet check and
   the next genesis included where possible). Don't wait for an agent wake-up between cells; the driver chains them.
   quiet() now gates on a 5 s CPU busy sample (`max_busy_pct`, default 10) instead of the 1-min load average (commit
   "matched-bench: quiet() gates on a 5 s CPU busy sample", the commit that adds this line).
   Generated drivers also run the report-only post-cell scans (ingest evictions, log excerpts, hardscan, soakchk) once after the last cell (`end_pass`, before `campaign.done`; validity scans stay in-line), and reuse an unchanged genesis per cell (`GENESIS_CACHE_DIR=$RUN/genesis-cache`, key = every genesis input, md5 checked; tools/matched-bench/README.md "Post-cell work", "Genesis reuse").
8n. **L2.2 screen (8k pick 2)** (owner s131; done, section 98; ~40 min). `exp/8k-settle-no-merge` `dbe14b21`
   (`TORUS_SETTLE_NO_MERGE=1`, byte-identical; microbench -5.3 to -7.0 ms per block) vs main `0f5bef38` (its base).
   One binary per arm, md5 checked. Shape: the 8i P1M deep book (1M-account genesis, `PRELOAD=1000000`), standard
   load, 600 s. Warm-up (300 s, either arm), then A and B one cell each (n = 1, a screen). Judge by the per-fill and
   per-block timers, not matched/s (one pair can't resolve ~2%): exec-thread on-CPU ms per 1k fills, settle phase ms
   per block (`torus_exec_phase_settle_seconds`, pass A / pass B, `torus_exec_cache_flush_seconds`), fills per block;
   report matched/s, blocks/s, p50 / p99 too. If the settle timer drops by roughly the microbench's 5-7 ms per block
   and nothing else gets worse, the full A B B A A B comes later, before any merge.
   **Done s68 (ozarchy-8n, A main `0f5bef38` node `0d24371e` (8k stage reused), B `dbe14b21` node `4cecf409` +
   `TORUS_SETTLE_NO_MERGE=1`, both A's bench `708eb332`, P1M deep book, smoke B then w1 A, r1-A, r1-B, 600 s; perf
   doc section 98).** All 4 cells ACCEPT / AGREE / PASS, md5 12 / 12, flag on=true once per B node and absent in A,
   0 CHECK FAIL, no swap growth. Settle phase 72.20 -> 62.41 ms per native block (-9.79, 0.864; pass B 34.40 ->
   18.94, cache flush 25.69 -> 31.38, pass A flat), exec-thread on-CPU 3.362 -> 3.228 ms per 1k fills (0.960), fills
   per block 71,388 -> 71,152, save_books 73.20 -> 73.77 ms per block. matched/s 226,021 -> 235,116, blocks/s 3.076 ->
   3.206, p50 / p99 228 / 1,485 -> 225 / 1,434 ms, timeouts 12 -> 6, RSS mean 6,792 -> 7,048 MB (in 8kd's spread);
   n = 1, not resolved. Verdict: full A B B A A B later. Inter-cell gaps 62 / 62 / 62 s (`max_load` 10).
8l. **Q11: item 7 option 0 (EVM fast path) A/B** (owner s128 decisions 7a / 8a; UNPAUSED s131; briefly paused again
   for the 8r attribution (8r-attr below), then restarts from scratch;
   replaces 8j; ~4-5 h incl. the extra cells, estimate; tools from main `3aaaa046` or later). Full spec: `docs/plans/item7-option0-impl.md` section 6 on the branch. **Binary:** one build of
   `origin/feat/item7-option0` at `f8a45a2d` (G0-G4 done; G3 and the whole branch Codex torus-adversarial reviewed;
   not merged: the owner decides the merge), code defaults, 512 KB from `env.sh`, custody 0. The off and on arms run
   the SAME binary. **Arms (2 x 2 + M):** N-off, E-off, N-on, E-on; every arm sets `TORUS_EVM_FAST_PATH=0` or `=1`
   explicitly via `EXTRA_ENV` (never absent). **Arm M (owner s131, from dropped 8j):** main `fd94818d` (the branch's
   base; the second binary) under the same EVM load, flag unset (it doesn't exist there): E-off / M answers the
   branch's flag-off cost, judged by the exec-path merge rule (must not be worse). E arms: `EVM_LOAD_RATE=10 EVM_LOAD_SENDERS=40`. Standard load
   (hsdb-300m), N = 11 bench genesis, 300 markets, 600 s, 3 validators. **Order:** smoke per arm (60 s), warm-up E-on
   300 s, r1 N-off E-off M E-on N-on, r2 N-on E-on M E-off N-off, r3 as r1 (n = 3; n = 4 if a merge-rule CI straddles 1
   by under 1 point). **Judge** E-on / E-off by the exec-path merge rule above; N-on / N-off within noise (the flag
   is inert without EVM); plus section 6's six option 0 passes (E-on / N-on >= 0.97 matched/s; E-off / N-off near
   section 85's 0.913; pipelined share at EVM heights >= 99% from `torus_exec_evm_fast_path_blocks` /
   `torus_exec_evm_blocks`; exec idle before an EVM block from `torus_exec_evm_idle_before_seconds` (the 4 / 8 ms
   bucket edges can't resolve 5 ms: use the logs); 0 deferred-read waits, 0 resident-row rebuilds, 0 shared layers;
   EVM landed = accepted, eth head within one interval, attestations agree). Also report
   `torus_flush_worker_evm_trie_seconds` and W busy (the EVM trie step moved to W). **Extra cells** (n = 1 each,
   after the matrix): E-heavy `EVM_LOAD_RATE=40` off and on; one SIGKILL crash cell under EVM load with the flag on
   (digests agree, replay length); one mixed-flag cell (val0 on, val1 off, val2 on with
   `TORUS_CHECKPOINT_INTERVAL_MS=0`): attestations agree for the whole cell. The default flip is the owner's call
   after this.
8m. **Book storage mode at depth, + L3.1's zero-build flag** (owner s131; NEXT, 8l is paused; ~1 h 15: 7 cells of
   300 s, ~10 min each incl. genesis / boot / pre-load / post checks, ~1 min between cells). 8i: at 1M resting orders
   Classic's `save_books` is 73% whole-book encode, linear in depth. The other modes save only what changed but were
   only benched at normal load (Classic vs mode 3, Q7: about a tie). Main (current), one binary. One cell each (a
   screen; the expected effect at depth is large), 300 s load (owner s131: enough blocks for the per-block timers).
   **P1M** (8i shape: 1M-account genesis, `PRELOAD=1000000`, standard load): Classic (`TORUS_BOOK_ROWS=0`), Classic +
   `TORUS_CKPT_DEFER_BOOKS=1` (L3.1's zero-build version), mode 1 OrderRows, mode 2 LevelAuthority, mode 3
   LevelAuthorityChunked. **P0** (same genesis, no pre-load): Classic and mode 1 only (does row storage cost anything
   on a shallow book; mode 3 and defer-books were already compared at normal load, mode 2 is superseded by mode 3).
   The mode changes the state-root format: fresh genesis per mode, every node the same mode (check
   `/proc/<pid>/environ` and the boot line). Judge mainly by `save_books` ms per block and per 1k fills and
   exec-thread on-CPU per 1k fills; also report matched/s, native blocks/s, p50 / p99, RSS, data-dir size, boot time
   on the 1M genesis. Question: which mode should the testnet genesis use? A winner gets a longer A/B before any
   default or genesis change.
   **Done s68 (ozarchy-8m, one binary main `898213e5`, node `0d24371e` (= the `0f5bef38` node: no crate change), smoke
   M3 60 s, w1 C 120 s, then C M3 M1 CD M2 C_P0 M1_P0, 300 s, n = 1; perf doc section 99).**
   * Validity: all 9 cells ACCEPT / AGREE / PASS, md5 27 / 27, 0 CHECK FAIL, no swap growth. Book mode took effect
     in every cell (`k8bmchk`: environ, checkpoint vs deferred-save lines, dirty book entries per block, row / level
     counters, level-cache gauges; CD `defer_books=true`). Modes 2 AND 3 both run without checkpoints (per-block
     flush).
   * P1M, C -> M3: save_books 65.70 -> 4.93 ms per block (0.996 -> 0.228 per 1k fills, + a 12.51 ms write half on
     W), exec-thread on-CPU 3.419 -> 2.551 ms per 1k fills, matched/s 223,857 -> 238,445, p50 / p99 202 / 1,563 ->
     34 / 836 ms, RSS mean 6,944 -> 4,360 MB. Costs: W per 1k fills 0.378 -> 1.994, CPU-s per 1M fills 24.85 ->
     29.33.
   * P1M, the other arms: M2 is close to M3, a little worse. M1 0.709 per 1k fills, on-CPU 2.962. CD no gain
     (save_books 66.73, on-CPU 3.727, 218,372 matched/s).
   * P0, C_P0 -> M1_P0: save_books 0.212 -> 0.676 per 1k fills, on-CPU 2.652 -> 2.926, matched/s 265,863 -> 232,960.
   * Screen verdict: mode 3 is the candidate. Mode 1 and defer-books drop out, mode 2 is superseded. Next: Classic vs
     mode 3 A/B, n >= 3, deep and standard shape, judged also on CPU per fill and W busy.
   * Inter-cell gaps 68-70 s.
8o. **8h rerun: does retention cost native blocks/s?** (owner s131; RUNNING, after 8m; ~1 h 30). Section 95 left blocks/s and
   p50 / p99 open (per pair r1 A 12.6 / B 13.1, r2 A 16.4 / B 9.3 blk/s; the owner reads r2-B as a possible
   regression). Since then: RH4 slow-pass logging is merged (main `32648b44`: a WARN line per GC pass over 50 ms with
   phase timings, `torus_da_gc_last_pass_max_seconds`, `torus_da_gc_slow_passes_total`), and
   `docs/plans/rh4-retention-early-cost.md` found ~1 ms per block of retention work before the first deleting pass.
   **Binary:** one build of main `32648b44` (code defaults; 512 KB from `env.sh`; custody 0). A = no retention, B =
   `TORUS_RETENTION_BLOCKS=1000` via `EXTRA_ENV`. Standard unpaced load, 300 markets, **300 s** cells, warm-up, then
   A B B A three times (n = 6 pairs). Report per cell and per pair: native blocks/s (whole cell and after the first
   deleting pass), fills per block, p50 / p99, timeouts, **blocks/s per minute from minute 1** with the first
   deleting pass marked, every slow-pass WARN line, `torus_da_gc_seconds` count / sum, held passes by reason. Judge on
   this campaign's 6 pairs alone (8h ran on an older main without exec-hash; report its 2 pairs next to it, not
   pooled). Verdict in plain words: if B is lower than its paired A in most pairs, say so; prune-by-default waits for
   this.
   **Done s68 (ozarchy-8o, one binary main `32648b44`, node `7585952e`, smoke B 180 s, w1 A 300 s, then A B B A x 3,
   300 s, n = 6 pairs; perf doc section 100).**
   * Validity: all 14 cells ACCEPT / AGREE / PASS, md5 42 / 42, 313 CHECK OK + smoke 25, 0 CHECK FAIL, no swap growth.
     Retention took effect on every B node (cmdline, GC on window 1128, slow_ms=50) and on no A node.
   * Verdict: **B is lower than its paired A in most pairs.** Native blocks/s is lower in 5 / 6: whole 13.481 ->
     12.953 (0.961 [0.914, 1.010]), after the first deleting pass (+80 s) 14.561 -> 13.842 (0.951 [0.890, 1.016]).
     Block p50 is higher in 6 / 6: 14.5 -> 15.7 ms (1.081 [1.060, 1.101]). Before the first deleting pass it is
     already +19% while blocks/s is equal. p99 779 -> 805 ms (CI spans 1). Timeouts 0.6 vs 0.4 per cell. CPU-s per
     1M fills +1.4% (6 / 6). matched/s -0.3%.
   * Per minute: m1 equal (1.007), the gap opens from m2 on (m5 0.890). No drift by run order, same block regime in
     every cell. 8h's r2 0.568 is not reproduced (8o pairs 0.89-1.05).
   * GC: 1.43-1.65 ms per native block on the consensus thread, p99 ~10-11 ms per pass, 0 held passes. 29 slow-pass
     WARN lines (50.9-189.3 ms), 95% of their time `write_ms`. 11 fall in load +120-135 s; one checked lines up
     with the state DB's 2 GiB `max_total_wal_size` flush.
   * Fails the exec-path rule on p50 (probably on blocks/s too): prune-by-default should not ship as it is. Next: move
     the GC write off the consensus commit path, then rerun the same A B B A x 3.
   * Inter-cell gaps 53-54 s (post-cell step 48-49 s, rest 5-6 s).
8q. **L2.2 full A/B (fixed)** (owner s131; after 8o; ~1 h). A = main `0f5bef38` (L2.2's base), B =
   `exp/8k-settle-no-merge-fix` `ddf41758` with `TORUS_SETTLE_NO_MERGE=1` (archy's `dbe14b21` + the fix for Codex's P1
   on aliased position rows: per-cache market-id ranges, exact flag-off fallback on overlap; Codex torus review no
   findings). One binary per arm, md5 checked. Shape: the 8i P1M deep book (1M-account genesis, `PRELOAD=1000000`),
   standard load, 300 s, warm-up then A B B A A B (n = 3). Judge by the exec-path merge rule (exec-thread on-CPU ms per
   1k fills, native blocks/s, block p50 / p99, timeouts per 1,000 blocks); also settle phase / pass B / cache flush ms
   per block (8n: -9.8 ms settle), `torus_exec_settle_no_merge_fallbacks_total` (must stay 0), matched/s, CPU-s per
   1M, RSS (8n: +256 MB, watch). Tools: freeze from a main that has the fix for the deferred-scan placeholder bug
   (after 0f2ca85b); if that fix isn't on main yet, freeze from `32648b44`.
   **Done s68 (ozarchy-8q, A node `0d24371e` / B node `0d6fc0a2`, tools frozen from `32648b44` (18c: the deferred-scan
   change has open review findings), deep P1M, smoke B 60 s, w1 A 300 s, then A B B A A B, 300 s, n = 3; perf doc
   section 101).**
   * Validity: all 8 cells ACCEPT / AGREE / PASS, md5 21 / 21 (+ smoke 3 / 3), 169 CHECK OK + smoke 25, 0 CHECK FAIL.
     Flag on=true once per B node, absent in A. `torus_exec_settle_no_merge_fallbacks_total` 0 on every B node.
     Swap +129 MB, min MemAvailable 22.4 GB.
   * Verdict: **merge candidate** (exec-path rule met). Exec-thread on-CPU per 1k fills 3.401 -> 3.306 (0.972 [0.963,
     0.982], 3 / 3). Native blocks/s 3.326 -> 3.327 (1.000 [0.967, 1.035]), p50 198 -> 201 ms (1.016 [0.929, 1.112]),
     p99 1,440 -> 1,467 ms (1.020 [0.810, 1.284]): all span 1. Timeouts 2.31 -> 4.65 per 1,000 blocks (7 -> 14, not
     resolved, p ~0.19; 8n 12 -> 6): watch.
   * Settle 63.03 -> 56.14 ms per block (-6.89, 0.891 [0.854, 0.929]): pass B 29.79 -> 17.25, cache flush 22.92 ->
     28.22, pass A flat. matched/s 223,639 -> 228,485 (+2.2%, 1.022 [1.010, 1.033]), CPU-s per 1M flat, fills per
     block +1.7%. RSS mean +245 MB (+3.7%, CI spans 1; 8n +256 MB).
   * Inter-cell gaps 66-67 s (post-cell step 61-62 s, rest 5-6 s).
8p. **Classic vs mode 3 A/B, deep and shallow** (owner s131; after 8q; ~2 h). 8m (section 99): at 1M resting orders
   mode 3 beats Classic (save_books 65.7 -> 4.9 ms per block, blocks/s 3.2 -> 10.1, p99 1,563 -> 836 ms, RSS -2.6 GB),
   but modes 2 / 3 can't use checkpoints today (`check_checkpoint_book_mode`, owner s109: W's deferred book save assumes
   every height < N is durable), so they flush every block: flush worker 5.3x per fill, whole-process CPU per fill +18%.
   Mode 3 on a shallow book was not run. One main binary (current, with the deferred-scan fix); A = Classic
   (`TORUS_BOOK_ROWS=0`, 15 s checkpoints), B = mode 3 (`TORUS_BOOK_ROWS=3`, per-block flush as today). Two shapes,
   warm-up then A B B A A B (n = 3) each, 300 s cells: **deep** = the 8i P1M shape (1M-account genesis,
   `PRELOAD=1000000`); **shallow** = the standard shape (100k genesis, no pre-load; comparable with 8g / 8o). The mode
   changes the state-root format: fresh data dirs per cell, every node the same mode (check `/proc/<pid>/environ` and
   the boot line). Judge by: matched/s, native blocks/s, p50 / p99, timeouts, exec-thread on-CPU per 1k fills,
   flush-worker ms per 1k fills and busy share, whole-process CPU-s per 1M fills, save_books, RSS, data-dir growth.
   Question: should the testnet genesis use mode 3, given its shallow-book cost?
   **Done s68 (ozarchy-8p deep / ozarchy-8ps shallow, one binary main `bb2a727f` node `7585952e`, tools frozen from
   `1489255f` (deferred scans + genesis reuse; scans in-line here), both shapes in one detached unit back to back: smoke
   B 60 s, w1 A 300 s, then A B B A A B, 300 s, n = 3 per shape; perf doc section 102).**
   * Validity: all 14 cells ACCEPT / AGREE / PASS, md5 42 / 42, 155 CHECK OK per shape (+ 23 per smoke), 0 CHECK FAIL.
     Mode took effect on every node (environ + boot line: A checkpoints ON, B deferred book save). 1M resting in every
     deep cell. Swap +195 MB, min MemAvailable 22.4 GB. Try 1 stopped at r1-A: a reused genesis has no generator
     `state_hash_activation = off` line; the expectation now also accepts `genesis reused (..., md5 <genesis md5>)`.
   * Answer: **testnet genesis stays Classic.** Shallow: mode 3 matched/s 268,533 -> 251,550 (0.937 [0.923, 0.951]),
     native blocks/s 13.80 -> 8.62 (0.624), p50 14 -> 47 ms, p99 769 -> 878 ms (1.141 [1.039, 1.253]), CPU-s per 1M
     fills 22.71 -> 26.64 (1.173 [1.163, 1.183]), flush worker per 1k fills 5.9x, busy 0.079 -> 0.421.
   * Deep (1M resting): mode 3 wins: matched/s 223,488 -> 242,938 (1.087 [1.058, 1.116]), blocks/s 3.22 -> 6.96, p50
     210 -> 61 ms, p99 1,500 -> 942 ms, timeouts 4.11 -> 0.16 per 1,000 blocks, save_books 65.05 -> 6.97 ms per block,
     exec-thread on-CPU per 1k fills 3.395 -> 2.550 (0.751), RSS -2.2 GB; but CPU-s per 1M +18.5% (1.185 [1.164,
     1.207]), flush worker per 1k fills 4.7x, busy 0.089 -> 0.411. The cost in both shapes is the forced per-block
     flush; re-test mode 3 only once it can use checkpoints.
   * Pacing: gaps deep 68-72 s (in-line post-cell step, 8q 66-67 s), shallow 14-16 s, deep -> shallow 69 s. Genesis
     reuse cut deep non-load time per cell 302 -> 238 s (genesis 55 -> 1 s).
8r. **8o rerun with the RH4 GC worker** (owner s131; after 8p; ~1 h 50). 8o (section 100): retention 1000 cost block
   p50 +8.1% (6/6 pairs), blocks/s ~-4%, CPU per fill +1.4%, from the GC running ~1.5 ms per block on the consensus
   thread. Fix: `perf/rh4-gc-worker` `a0181b8b` (pushed, not merged): consensus only hands the GC a job on a bounded
   queue; a worker thread does record / delete / write; a pin fence protects bodies under check (Codex
   torus-adversarial round 3: approve). **One binary** from `a0181b8b` (code defaults; 512 KB from `env.sh`; custody
   0). Arms: **A** = no retention; **B** = `TORUS_RETENTION_BLOCKS=1000` (worker GC, the default on this branch);
   **C** = `TORUS_RETENTION_BLOCKS=1000` + `TORUS_DA_GC_INLINE=1` (the old inline GC, as 8o's B), all via `EXTRA_ENV`.
   Standard unpaced load, 300 markets, 300 s cells, warm-up then A B C C B A twice (n = 4 per arm). Tools: main
   `3aaaa046` or later. Report as 8o (per cell and per pair, whole / post first deleting pass / pre windows, blocks/s
   per minute from minute 1, slow-pass WARN lines) plus the worker metrics (handoff seconds, queue depth, lag,
   coalesced heights, fence pins / refused / held `fence_full`). Verdicts: B vs A (does the worker remove 8o's
   regression: p50, blocks/s, CPU per fill within noise?) and C vs A (does 8o's regression reproduce on this
   binary?). Prune-by-default waits for this.
   **Done s68 (ozarchy-8r, one binary `perf/rh4-gc-worker` `a0181b8b` node `94053e26`, tools main `d1efbfa6`, smokes
   B and C 180 s, w1 A 300 s, then A B C C B A x 2, 300 s, n = 4 per arm; perf doc section 103).**
   * Validity: all 15 cells ACCEPT / AGREE / PASS, md5 45 / 45, 326 CHECK OK + smokes 26 + 26, 0 CHECK FAIL / WARN, no
     swap growth. Every node ran its arm's GC mode (B worker, C inline, A none). The reused-genesis cells pass 8o's
     plain `state_hash_activation` expectation (3aaaa046). Gaps were 67-70 s.
   * B vs A: **the worker fixes p50 but not blocks/s.** p50 14.9 -> 14.7 ms (0.989 [0.882, 1.110]; C / B 1.084 [1.008,
     1.167]). Native blocks/s 13.420 -> 12.738 whole (0.949 [0.820, 1.099], 3 / 4 lower) and 14.493 -> 13.635 post
     (0.940 [0.854, 1.035]). CPU-s per 1M fills +2.0% (1.020 [1.008, 1.033], 4 / 4). matched/s -1.0% (0.990 [0.979,
     0.9997], 4 / 4; not in C). The consensus-thread GC handoff costs 22 us per height (C: 1.6 ms per native block).
     There was no queue backlog, no coalescing, no refused pin and no `fence_full`, but ~300 `dispatch_behind` held
     passes per node per cell.
   * C vs A: **8o's regression reproduces.** blocks/s 0.947 [0.886, 1.012] whole and 0.935 [0.896, 0.976] post (4 / 4
     lower), p50 +7.3% (4 / 4 higher), CPU per fill +1.5%. Inline passes up to 299 ms ran on the consensus thread.
   * Next: find the ~5% blocks/s cost left in both retention arms (pruners vs GC write, `perf` A vs B), and drop the
     `dispatch_behind` re-passes. Prune-by-default should not go in on this evidence alone.
8r-attr. **8r attribution, analysis only, NOW (8l paused for it)** (owner s131; ~30-60 min, no new cells). 8r (section
   103): worker GC fixed p50 but blocks/s stays ~5% lower in BOTH retention arms (B worker 0.949, C inline 0.947) and
   CPU per fill +1.5-2%, so the cost is not where the GC runs. Question: what does retention slow? Per cell, A vs B
   vs C: RocksDB compaction CPU / bytes, write-stall count and micros, flush count, L0 files, pending compaction bytes,
   delayed write rate, WAL bytes, DB write p99 / max (state DB and consensus DB separately), per-thread CPU
   (exec, hotstuff-algo, flush worker, GC worker, rocksdb bg threads) if sampled, exec / consensus phase timers
   (view durations, commit persist, body persist, handoff waits), blocks/s per minute aligned to the first deleting
   pass, the slow-pass WARN lines, and the worker's ~300 'dispatch_behind' holds per cell. Name the dominant
   difference with numbers. Also push the raw per-cell metric data (text only, no node DBs) to `data/ozarchy-8r`.
   **Done s68 (8r-attr, analysis only, section 103.6): the cost common to B and C is state DB compaction of
   `cf_block_bodies` from the state pruner's `delete_range_cf` tombstones (compaction CPU 0.077 -> 0.151 / 0.154 s/s
   per node, x 2.0, 4 / 4), not the DA GC; with the GC it accounts for CPU per fill; exec us / fill +14-16% in
   state-compaction seconds (23% vs 13% of seconds); blocks/s link medium-low (bigger blocks, saturated exec); no
   RocksDB stalls. Next: retention-on arm with block-body pruning off. Raw 8r data: branch `data/ozarchy-8r` 7c8323ac.**
8s. **b0 screen: Classic vs mode 3 + b0 (mode 3 with checkpoints)** (owner s131; NOW: pause 8l, run 8s, then 8l
   restarts from scratch; ~40 min). 8p
   (section 102): mode 3 wins deep (blocks/s 2.16x) but loses shallow (blocks/s 0.62x, p50 3.3x, CPU per fill +17%)
   because modes 2/3 flushed every block. b0 (`perf/mode3-checkpoints-b0` `c2fbfbc0`, Codex torus-adversarial approve,
   137 targeted tests): with a checkpoint interval set, modes 2/3 use checkpoints; the write half of the book save runs
   on the exec thread instead of the flush worker. **One binary** from `c2fbfbc0`, code defaults (15 s checkpoints now
   apply to mode 3). A = Classic (`TORUS_BOOK_ROWS=0`), B = mode 3 (`TORUS_BOOK_ROWS=3`). Per shape: one 120 s warm-up
   cell, then A and B one 300 s cell each (n = 1, a screen: the expected effect is large). Shapes: **shallow** = the
   standard shape (100k genesis, no pre-load); **deep** = the 8i P1M shape (1M-account genesis, `PRELOAD=1000000`). Fresh
   data dirs per cell; every node the same mode (check environ + boot line); confirm mode 3 cells run checkpoints (the
   checkpoint metrics move, no per-block flush). Report per cell: matched/s, native blocks/s, p50 / p99, exec-thread
   on-CPU per 1k fills, `save_books` ms per block and per 1k fills, flush-worker ms per 1k fills and busy share,
   CPU-s per 1M fills, RSS; and compare with 8p's mode 3 (per-block flush) cells. Question: does b0 close mode 3's
   shallow gap while keeping most of its deep win? It decides whether b1 gets built. Tools: main `3aaaa046` or later.
   **Done s68 (ozarchy-8s = 8ss shallow + 8sd deep, section 104; b0 `c2fbfbc0` node `fde31231`, tools `be87c86e`, 6 / 6
   cells valid, md5 18 / 18, 0 CHECK FAIL; every B node ran 15 s checkpoints with the b0 exec-thread save, no per-block
   flush): no, b0 widens the shallow gap: matched/s B / A 0.911 (8p 0.937), blocks/s 13.65 -> 3.99 (3.2x fills per
   block), p50 14 -> 136 ms, timeouts 1 -> 9. It keeps a smaller deep win: matched/s 1.061 (8p 1.087), blocks/s 1.28x
   (8p 2.16x), p50 189 -> 107 ms. CPU per 1M fills falls to +8.9% / +5.7% over Classic (8p +17.3% / +18.5%) and the
   flush worker is busy 0.11 / 0.12 (8p 0.42 / 0.41). The cost moved to the saturated exec thread: the write half is
   ~20-22 ms per block (save_books 29.8 / 28.3 ms per block). b0 is not enough; if mode 3 is still wanted, build b1
   (the write half off the exec thread, with checkpoints) and re-test both shapes. Gaps 15-17 s shallow, 69 s deep.**
8t. **L2.2 before default-on: standard-shape A/B + longer timeout check** (owner s131; after 8l; ~2 h 20). L2.2
   (`TORUS_SETTLE_NO_MERGE`, merged on main flag off, `be65552e`) was benched on the 1M deep book only (8n, 8q: exec
   on-CPU 0.972, matched/s 1.022; timeouts 7 -> 14 unresolved). One main binary (current), A = flag unset, B =
   `TORUS_SETTLE_NO_MERGE=1`. (1) **Standard** shape, 300 s, warm-up then A B B A A B (n = 3): exec-path merge rule +
   settle / pass B / cache flush ms per block, fallbacks (must be 0). (2) **Timeouts**, deep book (8i P1M), **900 s**
   cells, A B B A (n = 2): timeouts per 1,000 blocks and per pair, view-timeout reasons (`torus_view_timeout_*`),
   p99. Verdict: default-on candidate or not; say plainly whether timeouts rise with B.
8u. **L3.3 engine-timer profile** (owner s131; NOW: right after 8s, before 8l; ~40 min). L3.3 (`TORUS_BOOK_FAST_ENCODE`, merged flag off)
   showed exec engine ms per 1k fills +2.8% [1.020, 1.037] and runqueue wait +5.5% on the deep book (section 97.8),
   unexplained. One main binary, 1M deep book (8i P1M), A = flag unset, B = `TORUS_BOOK_FAST_ENCODE=1`, one 300 s cell
   each with the Q8 perf recipe on the exec thread, **`perf_event_paranoid=1` for this run only** (owner sets it before
   8u and restores 2 after; kernel sampling is needed for page faults, kernel time and off-CPU stacks, which the
   scheduling-artefact question depends on; log the value at every cell start and end). Report the engine phase split A vs B (match, settle, cancel-all, allocation, memmove,
   jemalloc, page faults) and whether the rise is real or a scheduling artefact. Only matters if Classic stays.
   **Done s70 (ozarchy-8u, section 105; main `0e514a5c` node `bbc03997`, 3 / 3 cells valid, 73 CHECK OK, 0 FAIL;
   paranoid 1 throughout, restored to 2 after; no contention overlap): not a scheduling artefact (exec runqueue per 1k
   fills +1.3%, 8kd +5.5%). With one binary, engine per 1k fills is +2.0% node-consistent (+3.1% in 97.8's definition),
   n = 1, not resolved: 44% of it is the post-engine tail (liquidation step on-CPU +20%), margin +2.0%, settle +1.0%;
   the profiled 60 s window is flat (1.002). Allocation / jemalloc / memmove / page faults inside the engine are flat;
   L3.3's change is confined to save_books (memmove 0.124 -> 0.001, page faults +20%). matched/s 1.020, save_books per
   1k fills 0.869. 8kd's uniform rise was two binaries (likely code layout, untested). Next if wanted: 2 more
   one-binary deep pairs without perf, or a liquidation-step timer.**
8v. **T5.3 attribution: block p99 inside checkpoint windows** (owner s131; after 8u; ~1 h incl. analysis; no code).
   Roadmap T5.3: during a checkpoint DB write, block p99 sits ~500-650 ms above the out-of-window p99 (sections 87,
   89); A1 (checkpoint slicing) is parked because its target (manifest put, ~6-12 ms) can't explain it. One main
   binary, code defaults (15 s checkpoints), standard shape, 3 cells of 600 s. Align every view / block with the
   checkpoint windows (start / end of each checkpoint write from the checkpoint metrics / log) and split, inside vs
   outside the windows, the consensus thread's phases (proposal wait, validate, vote, QC collect, commit, manifest /
   body persist) and the exec thread's (checkpoint upkeep, freeze, flush handoff, settle wait), plus RocksDB write /
   stall / flush activity and per-thread CPU if sampled. Name the dominant phase of the in-window gap with numbers.
   This decides whether A1 or something else gets built.
   **Order (owner s131):** 8u and 8v run **before** the RH4 3-arm rerun (8w; filter built and reviewed, `baffa3a5`). Queue after 8s (8s finished, analysis running): 8u now, then 8l, 8t, 8v, then the RH4 rerun (8w).
8w. **RH4 3-arm rerun: state pruner compaction filter** (owner s131; after 8v; ~1 h 50). 8r (section 103.6): the
   cost both retention arms share is state-DB compaction of `cf_block_bodies` from the state pruner's
   `delete_range_cf` range deletes (compaction CPU x2.0, L0 3.3 vs 1.2). Fix: `perf/rh4-gc-worker` `baffa3a5` (on
   `a0181b8b`, pushed, not merged; Codex torus review: no findings): `TORUS_PRUNE_MODE=filter` (default) saves the
   cutoff durably, drops whole SSTs below it (`DeleteFilesInRange`) and lets a compaction filter drop the rest;
   `TORUS_PRUNE_MODE=range` keeps the old path (microbench: compaction CPU 23.2 -> 7.6 s, write 408 -> 124 MiB).
   **One binary** from `baffa3a5` (code defaults; 512 KB from `env.sh`; custody 0). Arms: **A** = no retention;
   **B** = `TORUS_RETENTION_BLOCKS=1000` (filter, the branch default); **C** = `TORUS_RETENTION_BLOCKS=1000` +
   `TORUS_PRUNE_MODE=range` (as 8r's B), all via `EXTRA_ENV`. Standard unpaced load, 300 markets, 300 s cells,
   warm-up then A B C C B A twice (n = 4 per arm). Report as 8r plus state-DB compaction CPU per node,
   `cf_block_bodies` L0 count, SSTs / bytes dropped whole, on-disk size at cell end. Verdicts: B vs A (retention
   within noise: p50, blocks/s, CPU per fill?) and C vs A (does 8r's cost reproduce on this binary?). Prune-by-default
   and the RH4 merge wait on this.
10. **[PARKED s128, owner: A1 not built; sections 87, 89; moved to roadmap T5.3] Q6. 71.8 checkpoint slicing A/B**, only if step Z says GO and 18c then builds A1 (not built; Items, Q6). Run after steps 4-5 have settled the Q1 / Q4 defaults (both touch
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
