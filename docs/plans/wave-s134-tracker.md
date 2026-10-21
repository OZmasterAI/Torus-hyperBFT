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
| 4b. P0.0 phase 2 (executor + EVM SpecId read header bits; mempool/RPC next-height bits) | feat/p00-phase2 (from 75cab960) | builder, opus high | started |
| 2a. TC validation gap | fix/tc-validation-gap 46f58794 | builder, opus high | built + gated (cachyoz nextest 4339/4339, doc, clippy 0 errors, 18c release); Codex review blocked by OpenAI cyber filter; [claude-review]: mergeable, no liveness regression, paths share the check (8/9 mutations caught); final cargo test on 18c running |
| 2d. TC review gaps + NEC set mismatch | fix/tc-require-tc 62c1964a (tests 8ae8358f; on 46f58794) | builder, opus high | built: winner-rule tie test, header-view test, valid_nec accepts committed set (+ previous before update decided). cachyoz nextest 4342/4342, doc, clippy 0 errors, release; ubuntoz hotstuff+consensus 858/858. Merges after the TC gap branch; needs a quick review |
| 2e. TC required (P1, VALIDPROPOSAL) | exp/tc-require-tc-probe | — | BLOCKED on owner design decision: strict rule stalls the chain (one_down_liveness_test: no commit in 120 s) because the pacemaker advances on timeout without a TC and drops older-view messages. Options: (A) advance views only on QC/TC (pacemaker rewrite), (B) leader waits for / collects a TC for view-1 (leader-skip + RECOVER changes), (C) keep TC optional, document no tail-fork resistance |
| 3a. Aliased rows reject-on-load (pre-genesis) | fix/aliased-rows-reject efb01bad (on 9f8fb795) | builder, opus high | built: cannot exist on a fresh genesis (only put_position writes rows, keyed from the row); decode_position_row rejects mismatched key/row on every load path, fail-stop like an undecodable row. RED: serial vs parallel settle disagreed (size 4 vs 1). cachyoz nextest 4327/4327, doc, clippy 0 errors, release; ubuntoz 9/9. Codex torus review: 1 P2 (RPC scans trader_positions / open_interest skip the check) -> fixed d46234af (RPC returns an error on a bad row; RED 834d0055; ubuntoz 695/695 + 418/418, clippy 0 errors), head 17035b17. Final cargo test goes with batch 2. Note: item6 phase 5 step 2 (parked) would need a rebase in position.rs / native_executor.rs if revived |
| 2b+2c. Delist option C (grace N blocks reduce-only, then force-close at oracle mark) + P0/P1 test gaps | fix/delist-option-c 23b0190e (RED 46792ffc) | builder, opus high | built: governance key delist_grace_blocks (default 43,200 = 24 h), bounded close step (2,048 traders / 256 positions per block) moving positions to the liquidator vault at one fixed price (fresh mark, else last aggregate, else wait); Part B found no bugs. 18c nextest 4342/4342; cachyoz full PASS. Codex torus review: 1 P2 (stale-price settlement leaves ADL escrow/vault exposure stranded; drain needs a fresh mark) -> fixed ad3f967d (adl_drain ranks a delisted market at the obligation's stored price when no usable mark; RED 6189ddd6/eeff055a; ubuntoz 91/91, 18c nextest 4344/4344, doc, clippy 0 errors, release). Codex quick review of the fix + cachyoz final running |
| 5. b1 onto main | perf/mode3-b1-main f4faa419 (main 9f8fb795 merged into ab4ded4e) | builder, opus high | built: 18c targeted 130/130, nextest 4339/4339, doc, clippy 0 errors, release (md5 95932e0e on 18c). Codex torus review of the merge: no findings. Head-to-head needs TORUS_BOOK_ROWS=3, no TORUS_CHECKPOINT_MAX_BYTES; Classic arm has L3.1 on by default. Not for main until the head-to-head |
| 3d. L6.1 review nits + main merge | perf/l61-window-books 87cbeee5 (nits e044e53c) | builder, opus medium | done: forced-transition test asserts L3.1 actually on; L3.1 hash dependency in one helper l31_hash_blocker keyed on ChainConfig::book_window_activation() (P0.0 switches that one method); main b452b4af merged, no conflicts; 18c targeted 26/26, nextest 4397/4397, doc, clippy 0 errors, release. Merge waits on the h_n check + running-hash decision |
| 6. 9g L6.1 3-arm (53a6d83c) | docs/perf-section-120-9g e3d610ae | archy | DONE, section 120: passes the queue rule. Deep matched/s B/A 0.997 [0.867, 1.147], B/C 1.167, blk/s 4.82x vs C; std exec on-CPU/1k fills B/A +0.9% (cost of hash on, not of L6.1). Caveat: h_n not compared value for value (no attest key) -> follow-up asked of archy (attest key or h_n dump + short B check) |
| Harness attest key (h_n value-for-value check) | tools/harness-attest-key fe6c8b51 | archy | [claude-review]: no false-PASS path, OK to merge after P2 (resummarize.sh drops .state_attest REJECT) -> archy fixing; then the 53a6d83c arm-B check after 9i |
| 6. 100k retention disk soak on b452b4af | archy | archy | 9i launched 09:37 (node md5 cf0f7a05), A archive then B N=100k, 7200 s load + 660 s quiet each, done ~14:15; smokes OK (B: GC window 100128, 0 deletes as expected before the window) |

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
- Owner s134: TC-required -> option B; design doc (B + tip signing) being written. Head-to-head: best config per
  family (stage 1 screen, stage 2 best vs best), hash on; plan drafted, awaiting owner OK. Book-mode genesis left out (harness/roadmap conflicts with main) -> merged main into it: c644d48f (both
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
- From delist C (owner questions, pre-mainnet): max age for the fallback close price; no fee on the forced close (as
  built); stop oracle submissions after settlement?; relist path; RPC shows no grace/settlement state yet.
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
- Head-to-head design (owner): Classic arm on b1-main's binary (same code, L3.1 on) or on main's? And with the running
  hash on (Classic needs L6.1) or off? Decide after the h_n check on 53a6d83c.
- Roadmap Tier 5 still open: T5.1 (cadence), T5.2 (p99 floor). Not in this wave.
