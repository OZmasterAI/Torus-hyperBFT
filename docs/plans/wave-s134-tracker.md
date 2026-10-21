# Wave s134 tracker

Owner go s134 (2026-10-21). Base: main `9f8fb795` (nextest 4324/4324).
Gate hosts (owner s134): order 1. 18c, 2. cachyoz, 3. ubuntoz. Heavy runs: 18c (one build lock, flock -w 60 per
command), else cachyoz, never ubuntoz. Small tests: first free of 18c, cachyoz, ubuntoz. Archy only between benches.
Owner decisions s134: trial-net `--archive` validator decided at trial net; no Codex second opinions on the
Claude-reviewed items; archy go for 9g. Open: running state hash on or off (9g gives the data).

## In progress

| item | branch | agent | state |
|---|---|---|---|
| 3b. L3.1 off-log line | fix/l31-off-log 05cbd500 (on 9f8fb795) | builder, opus medium | built: one ON / OFF: <reason> startup line (book_coalesce_off_reason); ubuntoz targeted 8/8, clippy 0 errors; Codex quick review running |
| 3c. Book-mode genesis fixes | feat/book-mode-genesis b17f9281 (main 9f8fb795 merged in) | builder, opus medium | built: harness P1 (summarize.py --genesis-book-mode), explicit classic omitted, bench-throughput set_process_book_mode, upgrade note; cachyoz full PASS (nextest 4343/4343); Codex torus review running. Merge to main = book mode field lands (default Classic) |
| 4. P0.0 activation gating, phase 1 | feat/p00-activation-gating 75cab960 (RED 7f2b4be2) | builder, opus high | built: feature registry + schedule (genesis activations, legacy alias, built-in table), header features (hashed only when non-zero), proposer stamps, check_parent_link before vote + at insert, UPGRADE REQUIRED on unknown bits. cachyoz nextest 4395/4395, doc, clippy 0 errors, release. Codex torus review: 1 P2 (unknown bits -> Invalid -> sync blacklists honest upgraded peers; confirmed by probe) -> fix added to the phase 2 builder's branch. Phases: 1 ~3.5 d (done), 2 executor/EVM read bits ~1 d, 3 running-hash migration ~0.5-1 d, 4 genesis digest ~0.5 d, 5 integration tests ~2-3 d, 6 ops ~0.5-1 d, 7 governance ScheduleActivation 3-4 d (pre-mainnet) |
| 4b. P0.0 phase 2 + phase-1 P2 fix | feat/p00-phase2 8a5632a6 (P2: RED 61787e99, fix 7742979a; phase 2: RED bdc199d8) | builder, opus high | built: UnsupportedUpgrade (no peer blacklist); EVM spec / execution / replay from header bits, mempool tip+1, RPC head+1. 18c nextest 4403/4403, doc, clippy 0 errors, release. Codex torus review: 1 P2 (UnsupportedUpgrade returned before the datum hash check: tampered data dodges blacklisting and inflates the global sync retry delay) -> fix sent to the phase 3 builder |
| 4c. P0.0 phase 3 + phase-2 P2 fix | feat/p00-phase3 8e269d77 (P2 edd7e77e/54d97aaf) | builder, opus high | built: running hash activates at the first block with the bit, live + replay, independent of local schedule; P2: unknown bit only an upgrade if data matches data_hash, genuine replies cool the peer (no global backoff). cachyoz nextest 4408/4408, doc, clippy 0 errors, release. o0 golden regenerated (BLOCKHASH-derived only; owner OK). Codex torus review: no findings. cachyoz final PASS (cargo test 4409/0). Merges to main with phase 4 as one P0.0 set |
| 4d. P0.0 phase 4 + hash-unverified option b | feat/p00-phase4 a7803f82 (RED 7577b7c0, d47749be) | builder, opus high | built: genesis digest keccak('torus-genesis-file-v1\0' + sorted compact JSON), changed genesis refused naming the fields, legacy dirs adopt with a warning; lost hash rows -> ERROR + gauge + no attestations, keeps voting. cachyoz nextest 4424/4424, doc, clippy 0 errors, release. Codex torus review: 3 P2 (retention marker written before the genesis refusal -> archive protection bypass; crash between genesis apply and digest store -> legacy rebinding; serde_json::Value rounds big ints in the digest) -> sent to the phase 5 builder to fix first |
| 4e. P0.0 phase 5 (integration tests) + phase-4 P2 fixes | feat/p00-phase5 342aa460 | builder, opus high | built: phase-4 P2s fixed (genesis checked before any boot write; digest stored before genesis apply; exact numbers); 4-validator cluster tests: crossing, mixed binaries (3/4 continues, 2/4 stalls without fork), crash across H, schedule change; 2 mutations caught; 5x ubuntoz 45/45; nextest 4437/4437 (18c + cachyoz). No P0.0 bugs found. Codex review: 2 P2 (test bit 127 leaks into --all-features production KNOWN_FEATURES; mixed-binaries test assumes consecutive commits -> flaky) -> sent to the phase 6 builder |
| 2a. TC validation gap | fix/tc-validation-gap 46f58794 | builder, opus high | built + gated (cachyoz nextest 4339/4339, doc, clippy 0 errors, 18c release); Codex review blocked by OpenAI cyber filter; [claude-review]: mergeable, no liveness regression, paths share the check (8/9 mutations caught); final cargo test on 18c running |
| 2d. TC review gaps + NEC set mismatch | fix/tc-require-tc 62c1964a (tests 8ae8358f; on 46f58794) | builder, opus high | built: winner-rule tie test, header-view test, valid_nec accepts committed set (+ previous before update decided). cachyoz nextest 4342/4342, doc, clippy 0 errors, release; ubuntoz hotstuff+consensus 858/858. Merges after the TC gap branch; needs a quick review |
| 2e. TC required (P1, VALIDPROPOSAL) | exp/tc-require-tc-probe | — | BLOCKED on owner design decision: strict rule stalls the chain (one_down_liveness_test: no commit in 120 s) because the pacemaker advances on timeout without a TC and drops older-view messages. Options: (A) advance views only on QC/TC (pacemaker rewrite), (B) leader waits for / collects a TC for view-1 (leader-skip + RECOVER changes), (C) keep TC optional, document no tail-fork resistance |
| 3a. Aliased rows reject-on-load (pre-genesis) | fix/aliased-rows-reject efb01bad (on 9f8fb795) | builder, opus high | built: cannot exist on a fresh genesis (only put_position writes rows, keyed from the row); decode_position_row rejects mismatched key/row on every load path, fail-stop like an undecodable row. RED: serial vs parallel settle disagreed (size 4 vs 1). cachyoz nextest 4327/4327, doc, clippy 0 errors, release; ubuntoz 9/9. Codex torus review: 1 P2 (RPC scans trader_positions / open_interest skip the check) -> fixed d46234af (RPC returns an error on a bad row; RED 834d0055; ubuntoz 695/695 + 418/418, clippy 0 errors), head 17035b17. Final cargo test goes with batch 2. Note: item6 phase 5 step 2 (parked) would need a rebase in position.rs / native_executor.rs if revived |
| 2b+2c. Delist option C (grace N blocks reduce-only, then force-close at oracle mark) + P0/P1 test gaps | fix/delist-option-c 23b0190e (RED 46792ffc) | builder, opus high | built: governance key delist_grace_blocks (default 43,200 = 24 h), bounded close step (2,048 traders / 256 positions per block) moving positions to the liquidator vault at one fixed price (fresh mark, else last aggregate, else wait); Part B found no bugs. 18c nextest 4342/4342; cachyoz full PASS. Codex torus review: 1 P2 (stale-price settlement leaves ADL escrow/vault exposure stranded; drain needs a fresh mark) -> fixed ad3f967d (adl_drain ranks a delisted market at the obligation's stored price when no usable mark; RED 6189ddd6/eeff055a; ubuntoz 91/91, 18c nextest 4344/4344, doc, clippy 0 errors, release). Codex quick review of the fix + cachyoz final running |
| 5. b1 onto main | perf/mode3-b1-main f4faa419 (main 9f8fb795 merged into ab4ded4e) | builder, opus high | built: 18c targeted 130/130, nextest 4339/4339, doc, clippy 0 errors, release (md5 95932e0e on 18c). Codex torus review of the merge: no findings. Head-to-head needs TORUS_BOOK_ROWS=3, no TORUS_CHECKPOINT_MAX_BYTES; Classic arm has L3.1 on by default. Not for main until the head-to-head |
| 6b. Head-to-head combined binary | perf/h2h-combined 29bfc705 (main + L6.1 + b1 + L2.1) | builder, opus high | built: mode 3 + b1 SAFE with hash on (b1_mode3_hash_on_same_h_n_at_every_height_on_every_node: identical h_n heights 1-300 across checkpoint timings + crash/replay; mutation B1_MUT_SKIP_SIDE diverges by h 2-4); L6.1 + non-classic book_mode refused; L2.1 tests adapted to main (listed markets; aliased-row tests now stop the block like flag-off -> owner/reviewer glance). cachyoz nextest 4498/4498, doc, clippy 0 errors, release md5 8e188408 (cachyoz). Codex torus review: only a P2 in the b0 microbench fixture (markets never listed -> timing empty books) -> fixed: h2h e13831cd (bench/test files only; node binary = 29bfc705, staged on archy) + fix/b0-ubench-markets 956d4969 for main (engine_parallel_bench, l3_savebooks_ubench, load_books_ubench, exec_* benches also listed markets; l3 ubench also had an open-order-limit and wrong-owner ModifyOrder bug). Merge resolutions and the mode-3 hash test: no findings |
| 3d. L6.1 review nits + main merge | perf/l61-window-books 87cbeee5 (nits e044e53c) | builder, opus medium | done: forced-transition test asserts L3.1 actually on; L3.1 hash dependency in one helper l31_hash_blocker keyed on ChainConfig::book_window_activation() (P0.0 switches that one method); main b452b4af merged, no conflicts; 18c targeted 26/26, nextest 4397/4397, doc, clippy 0 errors, release. Merge waits on the h_n check + running-hash decision |
| 6. 9g L6.1 3-arm (53a6d83c) | docs/perf-section-120-9g e3d610ae | archy | DONE, section 120: passes the queue rule. Deep matched/s B/A 0.997 [0.867, 1.147], B/C 1.167, blk/s 4.82x vs C; std exec on-CPU/1k fills B/A +0.9% (cost of hash on, not of L6.1). Caveat: h_n not compared value for value (no attest key) -> follow-up asked of archy (attest key or h_n dump + short B check) |
| Harness attest key (h_n value-for-value check) | main 54ffec29 (archy 6a85d512) | archy | merged. Arm-B smoke (53a6d83c std 90 s): h_n equal on 63 checkpoints x 3 nodes, quorum 63/63, 0 mismatch; but 2 harness bugs: drain never quiets (attest native actions every checkpoint), min-compared 64 too strict (pruned mid-walk -> 63). archy fixed on fix/harness-attest-drain b4856e8d; Codex review: P1 (attest drain allowance also accepts stranded load actions -> false ACCEPT) + P2 (cancels hidden from the digest-window quiescence check) -> archy fixed b278ab9e; re-review: earlier P1/P2 resolved, 2 new P2 (first responding node's cancel status only; oracle feed + attest drain restarts the quiet window) -> sent to archy; patched smoke passes (drained after 12 s; 28 checkpoints x 3 nodes h_n equal, quorum 28/28); h_n check PASS (4/4 cells ACCEPT AGREE; h_n equal on all compared checkpoints x 3 nodes, quorum full, fail-stop live; deep 261k / std 269k matched/s); section 122 pending. Don't use STATE_HASH_ATTEST=1 in the head-to-head until b4856e8d is merged |
| 6c. Head-to-head (owner go s134) | perf/h2h-combined node 29bfc705 | archy | arm list posted (C15, C5, CL21, M15, M5 stage 1; best vs best + hash-off ref stage 2; ~6-6.5 h); drain fix merged main 0f8eedb3 (Codex clean after 3 rounds). 9j STAGE 1 LAUNCHED ~17:45 (node f4f2679f, tools 0f8eedb3; smokes M15 + C15 ACCEPT, attest ok); deep then std, w1 + r1 C15 C5 CL21 M15 M5 + r2 reversed, 300 s; done ~20:45, then winner analysis and stage 2. Archy writes the no-restart kill knob meanwhile (code only). 17:46: stage 1 stopped on a false driver check (L6.1 line only printed on a genesis-cache miss; node logs fine) -> expects fixed (always-printed genesis audit lines), stage 1 RELAUNCHED 17:57, FINISHED 21:06 (22/22 clean). Headline matched/s (r1/r2): deep C15 255.2k/249.3k,
C5 256.1k/254.7k, CL21 256.3k/250.5k, M15 243.7k/231.9k, M5 244.2k/237.0k; std C15 264.3k/263.0k, C5 263.0k/263.1k,
CL21 266.1k/265.1k, M15 251.4k/250.1k, M5 251.6k/253.5k. Section 123 (merged locally b78ca907): rule winners C15,
M15; OWNER EXCEPTION: stage 2 Classic arm = CL21 (L2.1 on; +0.75% exec CPU/fill accepted for +8.6% deep blk/s, lower
latency). Stage 2: CL21 vs M15 + Classic hash-off reference, ~3 h. Stage-2 smoke hit an attestation nonce
collision (oracle feed shares the validator key, ms nonces; node re-signs after 65 s) -> archy fix/attest-hole-wait
1aa2fd0a (heal holes up to 100 s); Codex: P1 false ACCEPT (retry overwrites an initial mismatch) + P2 deadline overrun
-> archy fixed 3ea3ddfd; re-check: one remaining P1 (a peer RPC error discards another node's observed mismatch) ->
archy; stage 2 LAUNCHED 21:49 (CL21 re-smoke clean, no holes), done
~01:00; attest verdicts with healed holes re-checked offline in the analysis.
- Delist keep close price (feat/delist-keep-close-price bf532e56): Codex no findings; cachyoz nextest 4469/4469.
  Batch 6 candidate merge/s134-batch6 52a95d86 (cachyoz final running).
- Batch 5 pushed to main **b78ca907**: delist node-local follow-ups ed1d6426 (cachyoz final 4467/0), kill knob
  bc408dab, section 123 b78ca907. torus-clean-merged freed 61 GB (delist-node-local worktree, p00-phase2 / tc-a /
  tc-b-dbg target dirs). Kill knob (tools/harness-kill-no-restart 87e91dfd; Codex 2 P2 fixed, re-check clean; merged into local main bc408dab with batch 5): KILL_NO_RESTART, VALIDATOR_STAKES; decided: one-down baseline feed-off (same for TC phase D), 2/2/1 stakes. Then: no-restart kill knob + one-validator-down baseline on main |
| 6. 100k retention disk soak on b452b4af | archy | archy | 9i finished 14:19: A and B rc 0, nodes agree, md5 ok, 0 bad rows; analysis -> section 121 (docs/perf-section-121-9i). Next on archy: arm-B h_n check on 53a6d83c (deep + std), then the head-to-head (L2.1 rerun folded in) |

## Started s134 evening (owner go)

- Docs refresh of stale status lines (docs/status-refresh-s134, opus medium).
- Delist node-local follow-ups: RPC pre-reject, getMarkets delist/settlement state, feeder skips settled markets
  (feat/delist-node-local, opus medium). BUILT f6d8a24c (RED 26a0b658 6/7; ubuntoz 355/355; cachyoz nextest
  4466/4466, release; clippy fixed). Limitation: after settlement closePrice/closeAt are null (not kept in state).
  Codex review running.
- P0.0 phase 6 ops: schedule gauge/metrics, RPC schedule listing, operator runbook (feat/p00-phase6, opus high).
  BUILT bc01a285 (on phase 5 342aa460; + phase-5 P2 fixes: test bit 127 out of production, 2-of-4 test prefix wait,
  10x 9/9); cachyoz nextest 4446/4446, doc, clippy, release. Codex review: 1 P2 (feature gauges initialised from genesis
  on clean restart) -> small fix builder; then P0.0 phases 1-6 merge as one set (final gate on the main merge).
- Design doc: dead-leader skip (explore, opus xhigh) -> docs/plans/dead-leader-skip.md. DONE (main d8156eca,
  decisions 7a 6c16e5c0 / 287701a5: b1 chosen, W=L, genesis parameters, a2 possible upgrade, jailing separate).
- Dead-leader b1 phase 1 (feat/dead-leader-p1, stacked on TC phase B; must land before TC phase C), opus high.
- Investigation: fail-stop restart bug -> CONFIRMED reachable in production (feed marks blocks delivered after the
  fail-stop latch during the ~200-450 ms before exit(70); after restart execution freezes silently). Fix builder:
  fix/failstop-delivered-height (app halt signal stops the feed advancing the delivered height + boot safety net),
  opus high. BUILT 69206dbe: feed stops at a halted app; boot lowers the delivered height and re-delivers the gap from
  the block tree when complete; drain logs 'execution FROZEN'. RED: delivered 8 vs app 3; cluster node stuck at 3 vs
  432. Mutations 2/2 caught; 5x 526/526 ubuntoz; cachyoz nextest 4469/4469. Codex review: no findings -> batch 7 after batch 6
  lands. Note: P0.0 merge must update chain_config() in failstop_feed_tests.rs (activations).
- Jailing design doc: DEFERRED; revisit before testnet; plan = testnet upgrade or before mainnet.
- Delist: keep settled close price in state (feat/delist-keep-close-price), opus medium.

## Queued (order)

1. 2b. Delist option C (grace period of N blocks reduce-only, then force-close at the oracle mark).
2. 2c. P0/P1 test gaps (resident-mode governance cancel, reduce-only).
3. 3. Small builds: aliased-rows reject-on-load cleanup; L3.1 off-log line; book-mode fixes (summarize.py env read P1,
   upgrade note, bench-throughput set_process_book_mode, explicit classic omit). L6.1 nits wait for 9g.
4. 5. Rebase b1 onto main.
5. 4. P0.0 activation-height gating (~8-9 days; docs/plans/p00-activation-height-gating.md).
6. Benches (archy, one at a time): L2.1 deep rerun on new main (after 9g); 100k retention disk soak (after RH4 merges;
   archive arms TORUS_RETENTION_BLOCKS=0); final head-to-head (after b1 rebase + running-hash decision).

## Done

- Batch 1 pushed to main **54ffec29**: L3.1 off-log 8a804dbd, TC gap + TC review gaps 1b751f6c, section 120 docs
  9e2cf2ec (cachyoz final on 9e2cf2ec PASS: nextest, doc, clippy 0 errors, release, cargo test 4403/0); harness attest
  key 95cf0d40 and DetachTest fix 54ffec29 (python only, tests OK on the merge).
- Batch 2 pushed to main **b57b3fd4**: delist C 21f385c6, aliased rows b57b3fd4 (cachyoz final PASS, cargo test
  4424/0).
- Batch 3 pushed to main **28ba931e**: book-mode genesis 1646d99e, watchdog flake fix 28ba931e (cachyoz final PASS,
  cargo test 4443/0).
- Batch 4 pushed to main **cecc90ec**: delist decisions + EIP-712 fix 9f02839a, microbench fixtures 7ec29084
  (cachyoz final PASS, cargo test 4462/0), disk-hygiene tools + builder rule 12. First torus-clean-merged --apply:
  3 merged worktrees + .cargo-target-delist-dec (24 GB) removed; 18c 133 GB free.
- Owner s134: TC-required -> option B; design docs/plans/tc-required-and-tip-signing.md committed to main e54a5f43
  (tip signing option 3, domain tags, drop nudge tips; phases A-D ~6-8 d) -> phase A builder started
  (feat/tc-required-phase-a). Phase A DONE 3e5317ff (RED 221e139e: strict one-down clusters stuck at
  committed=None; GREEN: both commit, 0 proposals missing a TC; cachyoz nextest 4465/4465, doc, release; clippy 0
  errors). Codex review of A: P1 (VSU winning-tip reproposals hardcode has_validator_set_updates=false -> Generic votes, update can stall) + P2 (genesis exemption ignores an available TC winner) -> both sent to the phase B builder to fix first; phase B builder started (feat/tc-required-phase-b: V1 + V3 on, V3 amended for
  VSU reproposals, timeout-vote high QC verified, byzantine/liveness tests, mutations). Phase B DONE 40cea7fc
  (A's P1/P2 fixed; V1 always on; V3 amended; votes with unverifiable high QC dropped; epoch-change wedge from phase A
  fixed; 7/7 mutations caught; multi-node 5x 15/15 on ubuntoz; cachyoz nextest 4486/4486, doc, clippy, release).
  Codex review of B: 2 P2 in case-5 recovery (justify commits not processed -> stale validator set; MissingData
  response dropped without retry) -> sent to the phase C builder to fix first; phase C (tip signing, domain tags, drop nudge tips) builder started. Open: one V3 edge
  case can cost a view (VSU reproposal with leader highest PC above the TC high QC).
  Phase C DONE f1c8269b (B's 2 P2 fixed f8e782e1; domain-tagged signing for all consensus messages, leader-signed
  headers/tips, signed tip views, nudge tips dropped; tail-fork test now asserts B re-proposed; 6/6 mutations; 5x
  15/15 ubuntoz; 18c nextest 4513/4513; header sign 36 us, verify 54 us). Devnet/testnet DBs must be wiped (local_tip
  format). Codex review of C running. Then A+B+C merge as one set; dead-leader p1 (on B) needs rebasing onto C.
- TC phase C [claude-review] (Codex blocked by cyber filter): merge after P1 (tail-fork window around validator-set
  updates: F2 accepts the previous-set leader's tip even after the update is decided, and both leaders can propose
  during the window -> TC can name the other block) + P2 (reputation-on guard) + nit (slashing build_vote_message).
  Owner s134: option a (fix the window) + P2 guard + delete/align the slashing encoder, as ONE small builder;
  LAUNCHED (owner go): builder on feat/tc-required-phase-c, opus high.
- Oracle vs attestation keys: production does NOT share them (feeder uses a separate SetOracleSigner key; nonce rule
  is a per-sender set, not monotonic). Bench-only collision. Owner s134: do both (document --state-hash-attest-key
  in the validator/operator guides; bench registers separate oracle signers) — LAUNCHED (owner go):
  tools/bench-oracle-signers, opus medium; archy informed.
- NEW (from TC phase C): slashing evidence format in torus-consensus doesn't match real phase-vote bytes (pre-existing). Head-to-head: best per family then best vs best, hash on, checkpoints 15 s default
  (5 s screen arm), L2.1 included -> combined-binary builder started (perf/h2h-combined: main + L6.1 + b1 + L2.1,
  plus the mode 3 + b1 hash-on determinism test). Book-mode genesis left out (harness/roadmap conflicts with main) -> merged main into it: c644d48f (both
  sides kept; python suite 630/0, genesis/types/book_mode_genesis 174/174, clippy 0 errors). Batch 3 = book-mode
  c644d48f + watchdog flake 867de0f3, after batch 2 lands.
- RH4 merged to main **b452b4af** (from 3b31e8ab; cachyoz cargo test 4384/0, nextest 4383/4383; Codex: approve).

## Unlocked by finished work

- RH4 on main -> 100k retention disk soak (archy, queued after 9g; archive arms TORUS_RETENTION_BLOCKS=0).
- Branches based on 9f8fb795 (TC gap, delist C, aliased rows, small builds, b1-main) now merge onto b452b4af: each
  merge needs a post-merge check (RH4 touched app.rs, node_dbs, pruner).

## New items found

- Docs refresh (small): stale status lines in item6-phase1/3/4 impl docs, item6-phases-2-5-plans, item7-step2/step3
  design, roadmap.md:34-36 (P0/P1 "in progress"), roadmap.md:271 (T5.3 "OPEN"), roadmap.md:4 ("not yet accepted"),
  roadmap.md:42 (book-mode decision vs the existing feat/book-mode-genesis branch), roadmap.md:365 (delist: owner chose
  option C). Source: s134 status audit.
- From the TC gap fix (46f58794), consensus, not built:
  - OWNER DECISION: `TipInfo.view` is unsigned, so a TC's tip can't be proven highest; any signer can sign any tip
    hash. Needs a message-format change: sign the tip view in timeout votes, or leaders sign proposal headers and that
    signature is the tip (MonadBFT), or both. Pre-genesis is the cheap time to change the format.
  - `cast_header_vote` never sets `local_tip`.
  - Nudge tips store the nudge's QC in `block_justify` (not the block's), so a tip hash-consistency check would reject
    honest TCs.
  - Nothing requires a proposal after a timeout to carry a TC.
  - Note: `tip_vouched` (signer-set power fix) is only on wip/m2-timestamp-lower-bound, not on main.
- From RH4: with TORUS_DA_GC_INLINE=1 the GC pass now also waits for the body write on the consensus thread, so
  inline-vs-worker bisects include the write wait (bench note, not a bug).
- Port genesis_cache.py's BOOK_WINDOW_COMMIT keying (from perf/l61-window-books) to main; without it B/C-style arms
  would share a cached genesis (archy, 9g). Small build.
- Harness DetachTest has no wait timeout on the shared build lock, so it hangs and leaves lock waiters when a builder
  holds the lock (3 harness test failures, same on main). Small fix.
- In tests the L3.1 blocker reads the process-wide book mode, not the context's (same in production). Nit.
- From delist C, DECIDED s134: (1) close price a+c (no age cap + governance can set the close price; not chosen: cap +
  wait); (2) no fee on the forced close (a); (3) stop oracle submissions once settlement completes (b; not chosen: keep
  forever, stop at delist); (4) no relist, new id (a; not chosen: relist same id). -> builder feat/delist-decisions
  (roadmap doc with chosen + not-chosen options, governance close-price action, oracle stop). BUILT f8f1216f (docs
  5ef80c82; SetDelistClosePrice; oracle stop after settlement; RED fe891553 10/10; 18c nextest 4454/4454, doc,
  clippy 0 errors, release). Codex torus review running. Open: governance can't set a price after close_at if none
  exists (looser rule: allow while unset); feeder keeps sending ignored prices; RPC shows no delist/settlement state;
  CONFIRMED P1 (pre-existing, Codex probe): EIP-712 hash of ValidatorRegistration(0x..01) == DelistMarket(1), so a
  signature for one is valid for the other -> builder fixing on feat/delist-decisions (per-variant type hashes +
  cross-variant tests). Codex found nothing else in the delist decisions. FIXED 9f53045c (per-variant Proposal<Variant>
  typed structs, alloy reference test, 3/42 fixture vectors changed; RED cdf7a917 5/6) + looser close-price rule
  0b14b95d (RED 5f54e8b6); head 117ea0cd; 18c nextest 4461/4461, doc, clippy 0 errors, release. Codex re-review
  re-review: no findings; cachyoz final running. EXTERNAL: torus-trading-app TS signer (eip712.spec.ts) is NOT updated
  (owner s134: don't touch the app now, it may be rebuilt); any client must match tests/fixtures/eip712_vectors.json.
- RUNNING STATE HASH (owner s134): LEANING ON (with L6.1). Final decision after the head-to-head (it has a hash-off
  Classic reference arm), before genesis.
- Infra: the shared ~/.cargo-target gives stale-artifact build failures across worktrees (L6.1 nits: 33 errors; delist
  clippy: torus-core failed to compile) -> builders fell back to per-branch target dirs. Worth a rule or a fix.
- Flake: torus-network resubscribe_wedge restarted_validator_recovers_via_watchdog_alone (20 s budget fails under load)
  -> fixed fix/watchdog-test-flake 867de0f3: budget derived from the watchdog schedule (3rd retry ~23 s + 4 s) x 1.5 =
  40.5 s; mutation (watchdog off) still fails; 20/20 + 10/10 under load ~25; nextest 4383/4383. Codex quick review
  running; goes in batch 3.
- P0.0 phase 1 open questions: features appended to canonical_header_bytes only when non-zero (vs always); item7
  golden fixture regenerated (only digests changed); an old node rejects unknown bits at insertion too, so it stops
  syncing at H and may blacklist honest upgraded peers; legacy state_hash_activation_height = 0 now refused; built-in
  table empty until the testnet chain id exists.
- b0_save_books_ubench fixture (crates/torus-bridge/tests/b0_save_books_ubench.rs:99) omits market listings, so since
  UnknownMarket it may time rejected orders (Codex stop-review WARN). Small fix.
- PRE-EXISTING BUG (found by P0.0 phase 5): after an execution fail-stop the app ignores new commits but hotstuff
  marks them delivered, so an in-process restarted node is stuck (real process restarts avoid it). Codex asked
  whether it is reachable in production; fix if so.
- Head-to-head design (owner): Classic arm on b1-main's binary (same code, L3.1 on) or on main's? And with the running
  hash on (Classic needs L6.1) or off? Decide after the h_n check on 53a6d83c.
- Build/disk plan (owner s134): (1) sccache cap 15 GB on 18c when builders idle; (2)+(3) torus-clean-merged +
  torus-target-dir on tools/disk-hygiene (building; then builder.md rule via the framework instance); (4) opt-level 1
  dependency A/B on 18c and (5) merge test binaries -> cargo-hakari: PARKED for later. Remote runners
  CARGO_INCREMENTAL=0 done (cachyoz, ubuntoz); 18c keeps incremental; ozarchy jobs = owner's other machine, don't touch.
- Roadmap Tier 5 still open: T5.1 (cadence), T5.2 (p99 floor). Not in this wave.
