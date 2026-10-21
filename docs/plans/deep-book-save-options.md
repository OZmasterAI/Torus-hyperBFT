# Deep order books with 15 s checkpoints: Classic + L3.1 vs mode 3 + checkpoints

Design note, s131 follow-up to perf doc sections 96, 97 and 99. Doc only, no code. Every claim cites code (path:line,
on origin/main `bcc95fab`) or a results section. Numbers not measured are marked "(estimate)".

**Status (s131): b1 built** on branch `perf/mode3-b1` (on top of `origin/feat/item7-option0` f8a45a2d plus b0; not
merged). With a checkpoint interval, book modes 2 / 3 defer pass 2 again: it rides the block's `Job::Digest`; W applies
it into a sidecar over `window.side`, then the DB (`digest_books_sidecar`, `exec_pipeline.rs`), digests the sidecar with
the block and merges it into `window.side`, which the checkpoint writes with L and the marker in one batch. Tripwire
(`books_tripwire`): a deferred job whose own set holds a book-CF key, or a window mixing deferred blocks with E-side
book writes, fail-stops. `last_job_books_deferred` = "the open window has deferred books" with an interval, cleared only
by a barrier. Byte cap (open question 7, owner s131: option C): a cap (`TORUS_CHECKPOINT_MAX_BYTES` > 0) with modes 2 / 3
refuses to start (`check_checkpoint_byte_cap_book_mode`); the cap's estimate is unchanged. W's write half is in
`torus_flush_worker_save_books_write_seconds` (W only; `exec_save_books_write_seconds` keeps one observation per native
block wherever it ran). Tests: the golden (serial mode 3, 300-block deep stream, hash on, every `h_n` and every CF dump
at every height through the option 0 ledger) equals interval 0 / every 5 / random policies in D and G runs, mode 3 and
mode 2; the three mutations fail the harness (skip side rows: diverges at height 2; planted E-side book write: tripwire
fail-stop; flag cleared at a non-barrier checkpoint with a stale rebuild while it is held: diverges at a + 1). b0's
equivalence and crash-replay tests now run the b1 path.

**Earlier status (s131): b0 built** on branch `perf/mode3-checkpoints-b0` (not merged). With a checkpoint interval, book modes
2 / 3 run the whole `save_order_books` on E (gate at the save in `app.rs`: defer only when `self.checkpoint.is_none()`);
with no interval (per-block flush) the deferred save on W is unchanged. `check_checkpoint_book_mode` is gone: the
interval and the 15 s validator default apply to every book mode. Tests: equivalence on a deep-level P2 stream (serial,
per block, every 5 / every 7 / never / 1 ms interval; every `h_n`, CF dumps incl. `cf_book_order_rows` and native roots
at every durable height), crash replay at non-checkpoint heights with boot verify, stale-holder rebuild mid-window, node
start with `TORUS_BOOK_ROWS=3` + interval and a SIGKILL restart. What moves to E is pass 2 (order rows, level rows,
stop diff, meta), today W's `apply_deferred_book_save`. µbench `crates/torus-bridge/tests/b0_save_books_ubench.rs`
(release, mode 3, 10 markets, 1,000 fills per block, A/B per block, n = 50 per arm, host loaded 16-22 on 18 cores, so
noisy): E save_books p50 shallow (10k resting) 2.57 -> 5.15 ms per block, deep (1M resting) 22.6 -> 24.2 ms; the moved
pass 2 is 2.3-2.6 ms p50 per block on W today. In production E's write half shows in `exec_save_books_write_seconds`
(observed on E under b0, on W for deferred blocks). Bench-scale numbers need a node bench. b1 only if b0 is not enough.

## 1. The problem in one paragraph

At 1M resting orders, Classic `save_books` costs 65.70 ms per native block (0.996 ms per 1k fills; perf doc 99.3). 88% of
it is encoding whole books (borsh 73%, Vec growth 15%; 96.3), and it grows linearly with depth (+0.61 ms per 1k fills
per 1M resting orders; 96.2). Mode 3 removes it (4.93 ms per block, exec-thread on-CPU per 1k fills 3.419 -> 2.551,
matched/s +6.5%, n = 1; 99.3), but modes 2 / 3 force the per-block flush. W then costs 5.3x per fill and the whole
process +18% CPU per fill (99.3). The question is how to get the deep-book gain and keep the 15 s checkpoints.

## 2. Facts both options depend on

| fact | evidence |
|---|---|
| Classic writes the whole blob of every dirty book every block, into E's overlay; the checkpoint keeps only the last value per key | `native_executor.rs:6957-6980`; `checkpoint_layer.rs:1-15`; 99.1 |
| Modes 2 / 3 on the pipelined path: E drains the journals, W writes pass 2 (rows, levels, stop diff, meta) into a sidecar of the block's flush | `native_executor.rs:7225-7292`, `exec_pipeline.rs:960-1012`, `app.rs:4458-4476` |
| W's pass 2 reads state: a stop-row prefix scan and a meta point read | `native_executor.rs:7400-7408`, `7447` |
| Those reads are correct only if "every height < N is durable" (W's view = DB) | `native_executor.rs:3320-3322`, `7218-7224` |
| With an interval, a block's rows are not written: W gets a `Digest`, rows wait in L (E side) or `window.side` (W side) until the checkpoint | `exec_pipeline.rs:317-339`, `859-907`, `709-721` |
| Hence the s109 rule: interval > 0 refuses modes 2 / 3; the role default yields (per-block flush) | `exec_pipeline.rs:266-285`, `149-171`; a deferred save reaching a Digest is a fail-stop, `app.rs:2831-2854` |
| E writes no book-CF key in deferred mode (bbo goes to `CF_NATIVE_BBO`, counter and marker to `CF_NATIVE_MARKETS`) | `native_executor.rs:7259-7290`; the only production book-CF writers are the save paths (grep of `put_cf`/`delete_cf` on both book CFs) |
| A stale resident holder rebuilds books from state; after a deferred parent it barriers first | `native_executor.rs:5729-5810`; `app.rs:4112-4131` |
| A barrier closes the window into a checkpoint, then waits for W; after it the DB is complete | `app.rs:2779-2815` |
| Checkpoint triggers: `h % 100`, epoch boundary (fast path), interval, byte cap, block count | `app.rs:3037-3043` |
| Level rows and node-local order rows must land in one batch; boot verify byte-compares them | `docs/design-level-rows-authority.md` 1.4 (lines 175-178), 5.1 (lines 390-401) |
| Mode 3 is consensus-visible; a default needs the book mode in genesis and the s450 test gaps checked vs mode 3 | `native_executor.rs:2930-2942`; `docs/plans/item6-phase2-impl.md:792-803` |

### 2.1 Who needs per-block book bytes today

* **Running hash h_n.** It chains each block's write set over every consensus CF. `CF_NATIVE_ORDER_BOOKS` is id 3,
  `cf_book_order_rows` is excluded (`running_hash.rs:24-27`, `34-60`; plan `running-state-hash-impl.md:67-86`).
  Validators attest h_n every 100 blocks. Execution only tallies votes, it never reads the local hash (plan :100), and
  fail-stop on mismatch is off by default (plan :118).
* **Is the hash on?** No. The activation is the genesis field `consensus.state_hash_activation_height`, absent = off
  (`torus-genesis/src/lib.rs:126`, test `:1057-1060`). Neither testnet genesis sets it (`testnet/genesis-weighted-base.json`
  and `-setup2b.json`: the `consensus` section has no such key). Every bench cell runs `state_hash_activation = off`
  (`8k-explore-ideas.md` footnote l32; `devnet/wsl/gen-3val-genesis.sh:153-158` sets it only with
  `STATE_HASH_ACTIVATION`). The plan ships it "with the coordinated testnet upgrade" (plan :124-130).
* **Native state root.** Not consensus: the block header's `state_root` is a dead 0x0 field, not hashed
  (`torus-types/src/lib.rs:609-611`). Trie maintenance is off by default (`native_trie.rs:324-336`). When on, it runs
  over the durable batch, so a checkpoint gives the same root at S.
* **EVM reads.** None. `getOrderBook` is removed; the reader precompile only serves `getPosition`
  (`torus-core/src/precompiles.rs:620-627`).
* **RPC.** `torus_getOrderBook` reads the DB (`torus-rpc/src/torus.rs:1084-1090`). On a validator it already lags to the
  last checkpoint. Ingress does not read books.
* **Crash replay.** Replays serially from the checkpoint marker with books loaded from the DB at S. The DB at S must hold
  every book as of S.

So with the hash off, nothing consensus-visible needs per-block book bytes. With the hash on, h_n needs them in every
block's write set.

## 3. Option (a): Classic + full L3.1 (+ L3.3)

**Mechanism.** On pipelined blocks with resident books, a checkpoint interval and the hash inactive, the Classic arm
skips `borsh::to_vec` and adds the market to an "unsaved" set kept with the resident holder. Every window close encodes
each unsaved book once and merges the blobs into the closing layer before `checkpoint_submit`. "Every close" means
every trigger in `app.rs:3037-3043` plus `checkpoint_drain` (barriers, shutdown). A stale-holder rebuild with unsaved
books must fail-stop (crash, replay from S): the holder is the only copy, and DB + L hold older blobs. Serial blocks
run after a barrier, so they keep the per-block encode. L3.3's encoder (`exp/8k-book-fast-encode` `4b916ecf`, merge
candidate, 97.8) makes the close encode ~15% cheaper.

**What changes (estimate).** `native_executor.rs`: Classic arm, unsaved set, an `encode_unsaved` helper (~80 lines).
`app.rs`: gate at the save (`4469`), encode in `checkpoint_upkeep_with` and `checkpoint_drain`, fail-stop on a stale
rebuild (~120 lines). Hash-active gate and flag (~30 lines). Tests ~400 lines. 4-6 days (the 8k critic's estimate,
ideas table row L3.1), plus 0.5 day to merge L3.3.

**Consensus / genesis.** None while the hash is off. The durable DB at every checkpoint height equals the per-block
path's DB at that height. Different nodes encode at different heights, which is fine because nothing observes the
intermediate heights. **With the hash on, (a) cannot run.** h_n would differ per node, since windows close on node-local
wall time. Making it work needs L6.1 (hash books only at `%100` heights). L6.1 changes the hash preimage and touches
`running_hash.rs` and `backend.rs` `block_digest`, so it is genesis-bound and blocked until option 0 lands (ideas row
L6.1).

**What is left of (a) with the hash on, without L6.1.** Only L3.3: `save_order_books` -15% per 1k fills on both shapes
and deep exec on-CPU -3.2%, matched/s not resolved (97.8). A parallel per-book encode could cut E's wall time but not
its CPU: serial save is memory-bound, 8 workers gave 2.8x (ideas footnote l33). The hash also adds work to Classic.
W would SHA-256 ~155 MB of blobs per block at P1M (1.6M x 97 B; footnote l33). At ~1.5-2 GB/s that is ~80-100 ms per
block on W, or ~+1.4 CPU-s per 1M fills (estimate). The s83 cost run (~40 ms per hashed block) used a much smaller
shape (s83-s85 doc section 8).

**Risks.**
* Silent divergence. If one trigger misses its encode, checkpoint S holds an old blob. A crash then replays from wrong
  books. With the hash off, nothing detects it, and Classic has no boot verify.
* A ~66 ms exec spike per window close at P1M (one full save; ~56 ms with L3.3). It is wall time on E's critical path.
* `rank8` paths that drop or advance the holder must all be covered. The fail-stop turns them into a crash plus replay
  (8i CK: 76 blocks in 41.4 s at P1M, 96.4).
* The code is throwaway the day the hash turns on.

**Expected numbers (estimate, from 8m n = 1 and 8i).**
* Deep (P1M). One 65.70 ms encode per 15 s window. 223,857 x 15 = 3.36M fills per window, so 65.70 / 3,358 =
  **0.020 ms per 1k fills** (vs 0.996). E on-CPU 3.419 - 0.976 = **~2.44 per 1k fills** (M3 measured 2.551).
* Deep, settle wait. It is 23.71 ms per block at P1M vs 1.96 at P0 (96.2). CD moved the book CF out of that path and
  gained nothing (99.3), so count on 0 here.
* Deep, CPU. CPU-s per 1M fills 24.85 - 0.98 (E) - ~0.6 (the merge thread: ~14% of a core / 221k fills/s, 96.3) =
  **~23.3** (M3 29.33). W stays at ~0.38 per 1k fills, because the blobs are still written once per checkpoint.
* Deep, throughput. matched/s **+8 to +17%** (critic, footnote l31). The ceiling is P0 / P1M = 263,708 / 221,362 =
  +19% (96.2).
* Shallow (C_P0). save_books 3.63 ms per block. At 14.3 blocks/s the `%100` trigger closes the window every ~7 s, so
  one encode per ~100 blocks: 3.63 / (100 x 17,145) = **~0.002 ms per 1k fills** (vs 0.212). E on-CPU 2.652 ->
  **~2.45 (-8%)**. matched/s +1 to +3% (critic). L3.3's -0.033 ms per 1k fills moved +0.5%, not resolved (97.8).

**Test plan.** A golden in option 0's G0 style. Record main's Classic run, serial and pipelined at interval 0, on a
300-block deep-book stream (places, cancels, cancel-alls, stops, reduce-only, a liquidation), with CF dumps at every
height. With the flag on, under Every5, a random policy and a gated W, the DB dump at every checkpoint height must
equal the golden at that height. Crash at random points: before the close encode, between encode and durable
checkpoint, and with an injected W fault. Restart and replay must reach the golden final dump. Planted faults must
fail the harness: skipping the encode on one trigger kind (each of the 7), and a stale-holder rebuild (must
fail-stop). With the hash active, the flag must be inert: h_n equals the golden.

## 4. Option (b): mode 3 + checkpoints

**What breaks today with interval > 0.** Nothing runs: the node refuses to start (explicit interval) or drops to the
per-block flush (role default) (`exec_pipeline.rs:149-171`). Suppose the rule were simply removed. Block N's deferred
save would go to a Digest job and hit the fail-stop at `app.rs:2847-2853`. Remove that too, and W's pass 2 would read
the stop rows and meta from the DB at the last checkpoint S. Every book whose stops or meta changed in S+1..N-1 would
then produce a wrong diff: a duplicate put, a missed delete, or a meta rewrite. That changes the write set, so h_n and
the checkpoint bytes diverge between nodes and from the serial save.

Two ways to fix it:

**(b0) Pass 2 on E when checkpoints are on.** At `app.rs:4469`, defer only if `self.checkpoint.is_none()`. Otherwise
run the serial `save_order_books`, which reads through E's layers (own -> R -> parent -> L -> C -> DB, `backend.rs:1405-1438`).
Rows go to the frozen set, then L, then the checkpoint, exactly as mode 1 does today (M1 ran 15 s checkpoints in
99.1). It is correct by construction, because it is the serial path. ~20 lines + tests, ~1 day (estimate). The cost
is that E pays the write half again.

**(b1) Pass 2 stays on W and reads through `window.side`.** This reuses option 0's pattern (W reads "sidecar, frozen
set, then DB" per Flush and "L, then DB" at a checkpoint; `item7-option0-impl.md` task 2.4), and its window side rows
(task 2.1).
* `Job::Digest` gains `books`. W applies pass 2 into an overlay whose lower layer is `window.side` (a `CheckpointLayer`,
  already accepted by `with_layers`), then the DB.
* The sidecar is digested as today (`backend.rs:1305-1309` includes it) and merged into `window.side`. The checkpoint
  writes L + side + marker in one batch (`exec_pipeline.rs:932-939`).
* That view is complete because E writes no book-CF key in deferred mode (section 2) and W runs jobs in order. Turn
  that claim into a tripwire: a deferred job whose frozen set holds a book-CF key fail-stops.
* `last_job_books_deferred` becomes "the open window has deferred books". Only a barrier clears it, so a stale-holder
  rebuild barriers (checkpoint + idle) and then reads a complete DB (`app.rs:4112-4131`, `2779-2815`).
* The byte cap must count W's side bytes; today it sees only L (`app.rs:3024-3028`).
* `pipeline_handoff` (`app.rs:2835-2873`) carries the books instead of failing.
* `check_checkpoint_book_mode` allows modes 2 / 3; its tests change (`exec_pipeline.rs:1205-1262`).
* Doc comments at `native_executor.rs:3320-3322` and `7218-7224` change.
* Size: ~200-300 lines + ~500 lines of tests, 3-5 days (estimate). It overlaps option 0's `Job::into_digest` and
  `Window` changes (unmerged branch `origin/feat/item7-option0`), so build it after option 0 lands.

**Consensus / genesis.** Mode 3 already changes the state format, so it needs the book mode in genesis. The
`__book_mode__` marker cannot catch a mixed fleet on a fresh DB (`item6-phase2-impl.md:792-803`). Add the s450
mode-2 test gaps vs mode 3: resident re-verify, match-workers determinism, multi-market chunking. Estimate 1-2 days for
the genesis field and 2-3 days for the gaps. b0 / b1 themselves are node-local: same bytes as the serial mode-3 save,
with the hash on or off. Mode 3's per-block hashed book bytes are small: ~1,387 dirty book-CF entries per block (99.3; levels plus stop / meta writes and deletes), vs
~155 MB of blobs in Classic.

**Crash / replay / boot verify.** Level rows (root CF) and order rows (`cf_book_order_rows`) both sit in L (b0) or
`window.side` (b1). Either way they go into the same checkpoint batch as the marker, so the DB at S is consistent and
boot verify passes. A crash mid-window replays serially from S (the flush worker attaches after replay,
`app.rs:2760-2770`). The serial mode-3 save writes both CFs per block. In b1, a W failure before the checkpoint latches
the fail-stop and nothing of the window lands.

**Risks.**
* b1: an incomplete W view (an E-side book write; guarded by the tripwire).
* b1: the window flag cleared too early.
* b1: the byte cap blind to side bytes.
* b1: merge order against option 0.
* b0: gives back most of the E gain (below).
* Both: mode 3's +18% CPU is not all W. 8m cannot split W, RocksDB background threads and drain workers (99.3), so
  the recovery is uncertain.

**Expected numbers (estimate).**
* Deep, E side. As M3 measured for b1: save_books 0.228, on-CPU 2.551 per 1k fills, matched/s ~238k (+6.5% vs C, n = 1).
  b0 adds the write half to E: 2.551 + 12.51 / 21.635 = **~3.13 per 1k fills**, which keeps only a third of the gain
  (C 3.419).
* Deep, W. Today M3 is 43.14 ms per block, of which 12.51 is the book write half (99.3). With checkpoints, b1's W is the
  apply (~12.5) + a side merge (a few ms) + the amortized checkpoint. Classic's whole W at P1M, blobs included, is 24.91
  ms per block. So **~20-30 ms per block = 0.9-1.4 ms per 1k fills** (vs 1.994).
* Deep, CPU. CPU-s per 1M fills **~26-28** (vs 29.33; C 24.85). Phase 3 measured -7.4% CPU from coalescing on Classic
  (perf doc Open list, section 47).
* Shallow. No current M3 P0 row. Section 36 (old code, both arms per-block flush, standard shape) shows save_books per
  fill flat (0.89-0.98x), W 1.66-1.95x, matched/s 0.96-1.01x (not resolved). b1 at P0: E ~ Classic; W ~0.33 + 0.34-0.6
  (section 36 write half per 1k fills) = **~0.7-0.9 per 1k fills** (C_P0 0.332). CPU-s per 1M ~C_P0 + 0.4-0.6.

**Test plan.** The golden is the serial (non-pipelined) mode-3 run of the same 300-block deep stream, with the hash
active. Record the h_n write set per height and every CF dump at every height, including `cf_book_order_rows`. b0 and
b1 must equal it at interval 0, Every5 and random policies, in drained (D) and W-gated (G) runs: W held so Digests
queue behind E and windows span stop and meta changes. Mutations must fail the harness:
* b1's W reading the DB only (skip the side layer): must fail at the first in-window stop or meta change.
* A planted E-side book write: must trip the tripwire.
* Clearing the window flag at a non-barrier checkpoint, with a forced stale rebuild mid-window: must diverge.

Crash at random points mid-window. Boot verify must pass, and replay must reach the golden dump.

## 5. Side by side (estimates unless measured)

| | Classic today (8m C / C_P0) | (a) L3.1 + L3.3 | M3 today (8m, per-block flush) | (b1) M3 + checkpoints |
|---|---|---|---|---|
| deep save_books ms per 1k fills | 0.996 | ~0.02 | 0.228 | 0.228 |
| deep E on-CPU ms per 1k fills | 3.419 | ~2.44 | 2.551 | ~2.55 |
| deep W ms per 1k fills | 0.378 | ~0.38 | 1.994 | ~0.9-1.4 |
| deep CPU-s per 1M fills | 24.85 | ~23.3 | 29.33 | ~26-28 |
| shallow E on-CPU ms per 1k fills | 2.652 | ~2.45 | not measured (8p) | ~Classic |
| works with the hash on | yes (pays SHA on blobs) | no (needs L6.1) | yes | yes |
| genesis change | no | no | yes | yes |
| size | - | 4-6 d | - | 3-5 d + genesis 1-2 d + s450 gaps 2-3 d |

## 6. What 8p (Classic vs mode 3, deep + shallow) will tell us

* **Deep, n >= 3.** It confirms or rejects M3's E-side gain (on-CPU -25%, matched/s +6.5%, p99, RSS -2.6 GB). b1
  keeps the E side unchanged, so if the gain holds, b1 inherits it. If matched/s does not resolve, the case for a
  genesis change weakens.
* **Shallow (first current-code M3 P0 row).** If M3 loses on E per fill at P0, (b) costs something at normal load that
  checkpoints cannot fix. That favors Classic + (a) while the hash is off. If it loses only on W and CPU, b1 removes
  most of that.
* **The CPU split.** Ask 8p to record per-thread CPU (exec, flush worker, rocksdb, ckpt-merge, drain workers;
  `/proc/<pid>/task/*/stat` or `pidstat -t`). That shows how much of M3's +18% is the per-block flush, which is the
  part b1 can recover.
* 8p measures neither (a) nor (b1); both stay projections. The choice turns mainly on the hash question (section 7),
  not on 8p.

## 7. Recommendation

**(b): mode 3 + checkpoints, built as b1 after option 0 lands. Merge L3.3 now (flag default off).** Reasons:
* The running hash is planned for the testnet genesis (plan :124-130). Once it is on, (a) cannot run, and Classic pays
  a per-block SHA over the whole blobs on top.
* Mode 3's per-block hashed bytes are tiny, and its E gain is measured, not projected.
* The genesis change (the book mode) is already required by the screen's mode-3 pick (99.4).
* b0 is optional, as a 1-day enabler. It allows an early "mode 3 + checkpoints" bench (W and CPU recovery, with E
  paying pass 2), and it is the reference that b1 must match byte for byte.

If the owner keeps the hash off on testnet for a long time, (a) is the cheaper path with no genesis change and better
projected numbers at both shapes. But it is a stopgap with a silent-divergence risk and no detector.

## 8. Open questions for the owner

1. Will the next testnet genesis set `consensus.state_hash_activation_height` (for example 1)? This decides (a) vs (b).
2. Book mode in genesis (s107 rule): approve it with mode 3, plus the s450 test-gap pass?
3. The s109 rule (interval > 0 refuses modes 2 / 3): lift it once b1 (or b0) passes the golden? b0 first as a bench
   enabler, or straight to b1?
4. Order vs option 0: b1 edits the same `pipeline_handoff`, `Job::Digest` and `Window` code. Build after option 0
   merges?
5. 8p: add the per-thread CPU split and keep n >= 3 per shape?
6. Only if (a): accept a ~56-66 ms exec spike per window close at P1M, and a fail-stop on a stale-holder rebuild?
7. b1: should the byte cap count W's side bytes (and E's L), or stay as it is?
