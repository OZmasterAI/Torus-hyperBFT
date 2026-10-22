# s80 keyed-local-trust-key: SHA-256 (SHA-NI) local trust key with keccak(C) memoized in the cache value

Branch `s80/keyed-local-trust-key` (worktree `/home/oz/projects/wt/novel-s80/proto/keyed-local-trust-key`), base
`29bfc705`. Commits:
- `69341e76`: RED, tests only.
- `083baa6c`: library + ubench (round 1, measured-only).
- `f5f55045`: fix round. Node arm behind `TORUS_S80_LOCAL_TRUST_KEY` + the review fixes.

Nothing pushed. Target dir `/home/oz/.cargo-target-s80-keyed-local-trust-key`. Release `torus-node` md5
`ae5afef089b4ca6ea7c267535eddc696`; it contains `TORUS_S80_LOCAL_TRUST_KEY on=` and the mismatch ERROR string.
Host ozarchy (Ryzen 9 5950X, SHA-NI). Status: **built**.

## Verdict (fix round)

**PASS on the ubench, and the node arm is built and tested.** Re-measured E verify+sort per bench-shaped block
(C = 666,415 B), pinned to CPU 6, 2 processes, ABBA, with the release guard on at its default (re-hash every 32nd
hit). A bench cell unit was active on the host during this run (see the caveat in the raw section); keccak medians
match round 1's idle run within +0.6%.

| | tie 0% | tie 20% | tie 50% |
|---|---|---|---|
| keccak (today) | 1.6960-1.6965 ms | 1.6964-1.6965 ms | 1.6967-1.6972 ms |
| SHA-256 memo, guard 32 | 0.6737-0.6744 ms | 0.6739-0.6740 ms | 0.6305-0.6324 ms |
| **saved** | **-1.022 ms (-60.2%)** | **-1.022 ms (-60.3%)** | **-1.065 ms (-62.8%)** |

- The guard costs 0.05-0.09 ms/blk: without it (check 0) the memo is 0.584-0.596 ms, as in round 1.
- Kill bar (< 0.8 ms/blk idle) not met.

**Expected node effect is still small.**
- E on-CPU: about -1.0 ms/blk, about -3.4% of E on-CPU per 1k fills.
- Ingress: with the flag on, each locally verified action pays one more canonical encode and one SHA-256 over
  C||tail. keccak(C) is forked from the existing ingress keccak pass, so there is no extra Keccak. That is about
  +0.55 ms of ingress CPU per block-equivalent per node, off the E thread.
- The round-1 estimate stands: matched/s about +0.2..+0.7%, which is under the 1% bar. Only a cell can tell, and the
  cell needs to read `torus_exec_verify_seconds` as well as matched/s.

**cell_worthy: yes, as one 120 s pair.** The binary is ready. With the flag off it takes the 29bfc705 code paths; the
only difference is one extra `None` field per pool entry. Read in the cell:
- the INFO line `TORUS_S80_LOCAL_TRUST_KEY on=true`;
- no `s80 local trust memo: memoized sort hash differs` ERROR;
- `torus_exec_verify_seconds` sum/count;
- matched_s_first120 and agreement.

Expect the matched/s delta to sit inside cell noise.

## Review issues and what was done

1. **[P1] Correctness test panicked in debug builds. Fixed.**
   - Cause, confirmed: the non-ignored `trust_key_memo_arm_matches_keccak_arm_on_bench_blocks` ran the tie-only arm.
     Its ZERO placeholder sort hashes trip `debug_assert_eq!(hash, keccak256(canonical_bytes))` in
     `sort_native_actions_indexed_with_sort_hashes` (`crates/torus-bridge/src/native_executor.rs:16342`).
   - The tie-only arm is gone from that test. New tests:
     - `tie_only_arm_matches_in_release`, `#[cfg(not(debug_assertions))]`: the old tie-only checks.
     - `tie_only_zero_placeholders_trip_the_sort_debug_assert`, `#[cfg(debug_assertions)]`,
       `#[should_panic(expected = "is not keccak(canonical_bytes)")]`. It reproduces the reviewer's P1 in the dev
       profile and passes there. This is the RED evidence, captured as a test.
   - The ignored ubench drops the tie-only arm in debug builds.
   - The tie-only arm now calls the memo function with the guard off (check 0); the guard would void its placeholders.
   - **Dev profile:** `cargo nextest run -p torus-bridge --test exec_hash_once_ubench --test perf_equivalence_golden`
     gives 7 passed, 4 skipped: the 5 golden tests, the memo correctness test and the should_panic marker. Same
     result with `TORUS_S80_LOCAL_TRUST_KEY=1`, and again after the last edit (`dbg-bridge-2`).
2. **Node arm built.** `TORUS_S80_LOCAL_TRUST_KEY=1` is read once per process and needs SHA-NI. Design below.
   - INFO line at mempool construction (after `init_tracing`, `main.rs:592` < `:790`):
     `TORUS_S80_LOCAL_TRUST_KEY on=<bool>`, with fields requested, sha_ni and check_every.
   - The golden passes with the flag off and on, in both dev and release. Caveat: `perf_equivalence_golden` drives the
     bridge executor directly, with no mempool, so the flag cannot change it. The flag-on equivalence that matters is
     the new app-level test plus the full torus-consensus suite with the flag on (results below).
3. **Design risk: release-mode guard plus seed-path tests.**
   - Guard: `TORUS_S80_LOCAL_TRUST_KEY_CHECK_EVERY`, default 32, `0` = off, `1` = check every hit (shadow mode).
     - The guard re-derives keccak(C) for hit ordinals 0, N, 2N, ... of each block, so the first hit of every block is
       always checked.
     - On any mismatch the memo is void for the whole block: every hit's sort hash is recomputed and its sender
       recovered, which is a cold node's path. An ERROR line is logged with height, mismatches, checked and hits.
   - The guard is a sample, not a proof, and the test says so: an unsampled poisoned entry goes through.
   - It checks the sort hash only. The sender is trusted verbatim, as the keccak cache trusts it today.
   - Seeding is narrowed to two code points, both from one action under the existing provenance gate:
     - `submit_native_action_inner`: the only insert path; it seeds iff `cache_key.is_some()`, i.e. locally verified
       and EIP-712;
     - the commit-time restash.
   - The public paths that reach those points are each tested against ground truth (below), with the flag off and on.
4. **Tautological CPUID test removed.**
   - Replaced by `sha256_known_answers_on_this_cpus_backend`: FIPS 180-2 vectors (empty, "abc", the 448-bit message,
     10^6 x 'a') through `sha2::Sha256::digest`, which is the key's entry point.
   - This checks the correctness of whatever backend runs on this CPU. It does not prove which backend that is. The
     SHA-NI evidence remains throughput: 1,677 MB/s vs keccak 451 MB/s, re-measured this round.
5. **Parallel keys.**
   - The memo pre-pass now honours `TORUS_EXEC_PARALLEL_KEYS` with the same rule as the keccak path
     (`use_parallel_keys`). Local keys are hashed on the verify pool, lookups run on the caller in input order, then
     keccak(C) for misses and sessions runs on the pool.
   - Tested in Serial / Default / Pool(2) x parallel off / on.
   - Measured on 4 pinned CPUs: keccak 0.497-0.499 ms vs memo 0.255-0.306 ms. The memo still saves 0.19-0.24 ms/blk.
6. **Clippy and dev-profile tests run.**
   - `cargo clippy -q --locked -p torus-types -p torus-mempool -p torus-consensus -p torus-bridge --tests`: 0 errors.
     It reports 129 warnings, none on a line this branch changed (checked by mapping each warning location onto
     `git diff -U0 29bfc705`).
   - The one warning on a changed line before this round (`needless_range_loop`, ubench block builder) is fixed. The
     builder still produces a byte-identical block (`take(23)` of 37 keys).
   - Dev-profile results are below.

## Design of the node arm (as built in f5f55045)

- `torus-types` (`lib.rs`):
  - `parse_s80_local_trust_key`, `s80_local_trust_key_requested` (env, read once);
  - `s80_local_trust_key_enabled` = requested AND `sha256_hw_accelerated()`.
- `torus-types` (`eip712.rs`):
  - `batch_verify_native_actions_local_key_memo` now returns `LocalKeyMemoVerify { senders, sort_hashes, hits,
    checked, mismatches }`;
  - `_with_mode(.., mode, parallel_keys, check_every)` for tests;
  - `parse_local_trust_check_every` / `local_trust_check_every` (env, read once, default 32).
- `torus-mempool` (`lib.rs`):
  - New field `local_trust: Option<RwLock<FifoCache<B256, (Address, B256)>>>`, same cap as the keccak cache. `None`
    means off, and every flag-off code path is then exactly 29bfc705.
  - The keccak cache `verified_senders` is untouched and still seeded and read (gossip dedup) with the flag on.
  - `trust_keys(&action)`: the keccak cache key and, when on, keccak(C) from the SAME sponge pass
    (`sort_hash_and_verified_cache_key_with_scratch`, the function E's keccak path uses). It replaces
    `verified_cache_key` at the two ingress sites: gossip recover and `submit_native_action`.
  - `local_trust_seed` computes the local key (one SHA-256) outside the pool lock. The insert stores
    `(local key, keccak(C))` in the pool entry, and the seed happens only after a successful insert, as for the keccak
    key.
  - `remove_committed_native` restashes it next to the keccak restash, under the same read lock.
  - New public API: `with_local_trust_key(bool)`, `local_trust_enabled()`, `local_trusted(&key)`,
    `cache_local_trust(..)`. The last one is public like `cache_verified_sender`, so tests can poison it.
  - Hit and miss metrics use the existing counters.
- `torus-mempool` (`native_pool.rs`): entry field `local_restash: Option<(B256, B256)>`, plus `insert_with_restash_keys`
  and `verified_local_restash`. `insert_with_restash_key` forwards `None`.
- `torus-consensus` (`app.rs`, the verify call, about :5156):
  - If `has_native && exec_trust_cache` and the mempool's local cache is on, take the memo path with
    `|k| m.local_trusted(k)`; otherwise the unchanged keccak call.
  - The sort is unchanged. Its debug_assert stays as the dev-build guard.

Not done:
- The keccak cache is not dropped when the flag is on. Gossip dedup still needs it, and keeping it makes the flag a
  pure addition.
- No new metric for guard checks. The ERROR line is the only signal.

## Tests (fix round)

Tests were written with the code this round, not committed RED first. RED evidence:
- the P1 failure is reproduced by `tie_only_zero_placeholders_trip_the_sort_debug_assert` (should_panic, dev profile);
- the new API tests did not compile before the API existed;
- a mistake in one new test was caught on the first run: the doubled block holds 2 broken signatures, not 1
  (`dbg-types` FAIL, fixed, `dbg-types-2` pass).

New or changed tests:
- `torus-types` (eip712 tests):
  - `local_key_memo_path_matches_keccak_path_senders_and_sort_hashes`: now a doubled 30-action block (over
    `PARALLEL_KEYS_MIN_ACTIONS`), Serial / Default / Pool(2) x parallel keys off / on, check every hit. It asserts:
    - equal to today's `batch_verify_impl_keyed` with the same parallel setting;
    - equal to the ground truth (recover / session owner / None; keccak(canonical_bytes));
    - hits = 24, checked = 24, mismatches = 0;
    - lookups in input order.
  - `local_key_memo_check_voids_a_poisoned_memo_for_the_block` (new):
    - A memo entry with a bogus sender AND a bogus sort hash on the block's first hit is caught at check 32 and at
      check 1. The result equals the cold path; mismatches = 1 / 2.
    - The second hit poisoned is caught at check 1, but NOT at check 32 (checked 1, mismatches 0, the bogus hash goes
      through).
    - check 0 checks nothing.
    - All in Serial, Pool(2)+parallel and Default+parallel.
  - `local_key_memo_hit_is_verbatim_and_keccak_keys_never_hit`: updated to the struct and guard-off API.
  - `parse_local_trust_check_every_default_zero_and_counts` (new).
- `torus-types` (action_hash_scratch_tests):
  - `sha256_known_answers_on_this_cpus_backend` (replaces the CPUID tautology);
  - `parse_s80_local_trust_key_only_one_enables`.
- `torus-mempool`: `local_trust_seeded_on_every_verified_path_with_ground_truth` (new). For the flag off and on (cache
  cap 4), it checks after each path that the local cache holds exactly `(recover_sender, keccak256(C))` under
  `local_trust_key`, and that the keccak cache holds the sender under `verified_cache_key`. Paths:
  - presigned ingress;
  - public `submit_native_action`;
  - gossip recover (CancelAll);
  - gossip trusted, which must NOT seed;
  - the restash after the action was churned out of both caches, which must re-seed;
  - a trusted entry, which is never restashed.

  Flag off: the local cache stays empty and the keccak cache is unchanged.
- `torus-consensus`: `app::crash_recovery_tests::local_trust_memo_execution_state_identical_and_poison_caught` (new).
  A 3-action block with 2 senders and a same-sender pair. All five arms reach identical balances:
  - (a) no mempool;
  - (b) keccak cache warm, local forced off;
  - (c) local memo warm;
  - (d) memo POISONED on the first hit (bogus sender + sort hash): caught, the block runs cold, the real senders are
    debited and the bogus one never is;
  - (e) poisoned memo with `--exec-trust-cache` off.
- `torus-bridge` `exec_hash_once_ubench.rs`: the P1 fix above. `trust_key_memo_arm_matches_keccak_arm_on_bench_blocks`
  keeps the keccak vs memo vs uncached equality at ties 0/20/50% (dev and release).

Results. F = `--cargo-quiet --status-level fail --final-status-level fail --hide-progress-bar`, all under the
cargo-heavy lock. Logs: `/home/oz/projects/wt/novel-s80/proto/results/klk-fix/*.log`.

| run | profile | flag | result |
|---|---|---|---|
| `cargo check -q --locked -p torus-types -p torus-mempool -p torus-consensus -p torus-bridge --tests` | dev | - | rc=0, no output |
| `nextest -p torus-types` (dbg-types) | dev | off | 141 passed, **1 failed** (test bug above), 3 skipped |
| `nextest -p torus-types` (dbg-types-2, after the fix) | dev | off | **142 passed**, 0 failed, 3 skipped |
| `nextest -p torus-types` (rel-types) | release | off | **142 passed**, 3 skipped |
| `nextest -p torus-mempool` | dev | off | **245 passed** |
| `nextest -p torus-mempool` | dev | `TORUS_S80_LOCAL_TRUST_KEY=1` | **245 passed** |
| `nextest -p torus-bridge --test exec_hash_once_ubench --test perf_equivalence_golden` | dev | off | **7 passed**, 4 skipped |
| same | dev | on | **7 passed**, 4 skipped |
| same, after the clippy edit (dbg-bridge-2) | dev | off | **7 passed**, 4 skipped |
| same | release | off | **7 passed** (5 golden + memo correctness + tie-only release), 4 skipped |
| same | release | on | **7 passed**, 4 skipped |
| `nextest -p torus-consensus -E 'test(trust_cache) \| test(local_trust) \| test(native_phase) \| test(verify)'` | dev | off | **7 passed**, 623 skipped |
| same | dev | on | **7 passed**, 623 skipped |
| `cargo clippy -q --locked` (4 crates, `--tests`) | - | - | 0 errors; 129 warnings, 0 on changed lines |
| `cargo build -q --release --locked -p torus-node` | release | - | rc=0, md5 ae5afef0... |
| `nextest -p torus-consensus` (full crate) | dev | off | **623 passed** (37 slow), 7 skipped |
| `nextest -p torus-consensus` (full crate) | dev | `TORUS_S80_LOCAL_TRUST_KEY=1` | **623 passed** (37 slow), 7 skipped |
| `cargo check -q --locked --workspace --tests` | dev | - | rc=0, no output |

The 7 tests in the consensus filter are: `local_trust_memo_execution_state_identical_and_poison_caught`, the three
`trust_cache_*` tests, and three `native_phase` tests. With the flag on, every consensus test that builds a `Mempool`
from the env takes the memo path whenever it executes with `--exec-trust-cache`. All 623 pass.

## Microbench, fix round (raw)

Binary: `exec_hash_once_ubench-6ddd3a9001bbdb0d` (release, f5f55045 sources), test `trust_key_ubench_bench_block`,
`--ignored --nocapture --test-threads=1`. The Sha256 arm calls the node's live entry point
`batch_verify_native_actions_local_key_memo`, so the guard and `TORUS_EXEC_PARALLEL_KEYS` apply. The arm asserts
`mismatches == 0` on every rep.

Per process and per tie share:
- 12 rounds, K S S K K S | K T T K K T; each round is 5 warm-up + 200 timed reps, one median;
- timed region = verify + sort, as in round 1.

Five processes in sequence under the cargo-heavy lock, 11:38:16-11:39:06. **Caveat:** a bench cell unit was active
(CELL_BEFORE=1, CELL_AFTER=1; loadavg 5.22 at start, 3.07 at end). The ubench was pinned to CPU 6 (p5: CPUs 4-7).
Keccak medians match round 1 (idle, 1.685-1.689 ms) within +0.6%. Log:
`/home/oz/projects/wt/novel-s80/proto/results/klk-fix/ubench.log`.

Raw keccak256 speed: 451 MB/s; sha256: 1,677 MB/s; `sha256_hw_accelerated()` = true (every process).

Per-round medians, verify+sort ms. Keccak lists all 6 K rounds: 3 from the memo ABBA, then 3 from the tie-only ABBA.

| process | tie | keccak (6) | sha256 memo (3) | sha256-tieonly (3) |
|---|---|---|---|---|
| p1 guard 32 | 0% | 1.6972 1.6971 1.6962 1.6956 1.6966 1.6961 | 0.6751 0.6748 0.6734 | 0.5960 0.5958 0.5958 |
| p1 guard 32 | 20% | 1.6964 1.6963 1.6965 1.6967 1.6967 1.6964 | 0.6738 0.6740 0.6740 | 0.9547 0.9551 0.9546 |
| p1 guard 32 | 50% | 1.6972 1.6974 1.6973 1.6970 1.6968 1.6974 | 0.6324 0.6322 0.6327 | 1.4095 1.4099 1.4102 |
| p2 guard 32 | 0% | 1.6962 1.6958 1.6961 1.6965 1.6958 1.6958 | 0.6737 0.6737 0.6737 | 0.5962 0.5961 0.5961 |
| p2 guard 32 | 20% | 1.6964 1.6961 1.6967 1.6961 1.6967 1.6962 | 0.6738 0.6742 0.6740 | 0.9551 0.9557 0.9550 |
| p2 guard 32 | 50% | 1.6971 1.6968 1.6968 1.6966 1.6962 1.6969 | 0.6309 0.6303 0.6303 | 1.4059 1.4054 1.4057 |
| p3 guard 0 (off) | 0% | 1.6988 1.6986 1.6986 1.6986 1.6990 1.6982 | 0.5957 0.5957 0.5957 | 0.5986 0.5987 0.5985 |
| p3 guard 0 (off) | 20% | 1.6966 1.6967 1.6964 1.6961 1.6963 1.6965 | 0.5846 0.5843 0.5843 | 0.9559 0.9561 0.9557 |
| p3 guard 0 (off) | 50% | 1.6966 1.6966 1.6956 1.6958 1.6958 1.6963 | 0.5840 0.5839 0.5840 | 1.4057 1.4057 1.4058 |
| p4 guard 1 (shadow) | 0% | 1.6983 1.6990 1.6994 1.6991 1.6988 1.6992 | 2.2495 2.2504 2.2504 | 0.5982 0.5979 0.5985 |
| p4 guard 1 (shadow) | 20% | 1.6971 1.6976 1.6974 1.6976 1.6974 1.6971 | 2.2324 2.2335 2.2331 | 0.9563 0.9575 0.9573 |
| p4 guard 1 (shadow) | 50% | 1.6962 1.6969 1.6968 1.6965 1.6963 1.6964 | 2.2276 2.2275 2.2287 | 1.4057 1.4055 1.4050 |
| p5 parallel keys, 4 CPUs | 0% | 0.4998 0.5023 0.5009 0.4966 0.4984 0.4978 | 0.3057 0.3049 0.3070 | 0.5987 0.5993 0.5988 |
| p5 parallel keys, 4 CPUs | 20% | 0.4983 0.4966 0.4963 0.4977 0.4977 0.4986 | 0.3057 0.3024 0.3014 | 0.9598 0.9600 0.9607 |
| p5 parallel keys, 4 CPUs | 50% | 0.4984 0.4979 0.4974 0.4981 0.4982 0.4983 | 0.2550 0.2552 0.2552 | 1.4132 1.4140 1.4116 |

Delta of means, keccak (all 6 K rounds) -> sha256 memo (the ubench's SUMMARY lines):

| process | tie 0% | tie 20% | tie 50% |
|---|---|---|---|
| p1 guard 32 (node default) | -1.0220 ms (-60.2%) | -1.0226 ms (-60.3%) | -1.0647 ms (-62.7%) |
| p2 guard 32 (node default) | -1.0223 ms (-60.3%) | -1.0224 ms (-60.3%) | -1.0663 ms (-62.8%) |
| p3 guard off | -1.1029 ms (-64.9%) | -1.1120 ms (-65.6%) | -1.1122 ms (-65.6%) |
| p4 guard every hit | +0.5511 ms (+32.4%) | +0.5357 ms (+31.6%) | +0.5314 ms (+31.3%) |
| p5 parallel keys | -0.1935 ms (-38.7%) | -0.1943 ms (-39.1%) | -0.2429 ms (-48.8%) |

Reading:
- **Guard cost at the default 32**, p1/p2 vs p3: +0.079 ms (0% ties), +0.090 ms (20%) and +0.047 ms (50%). That is 2
  sampled hits per 60-hit block (ordinals 0 and 32), each a re-encode + Keccak of one action. Bench blocks are 400-order
  batches, about 18 KB, so the cost depends on which action is sampled. It is 4-8% of the saving.
- **Shadow mode (check every hit) is a debugging mode, not a production setting.** It is 0.53-0.55 ms slower than
  keccak, because it re-encodes C and hashes it again on top of SHA-256.
- **Parallel keys (issue 5).** The keccak path drops to 0.50 ms on 4 CPUs, and the memo still saves 0.19-0.24 ms/blk.
  The tie-only arm has no parallel form and is irrelevant. `TORUS_EXEC_PARALLEL_KEYS` is off in the CL21 cells.
- Round-to-round spread inside an arm is at most 0.3% (p5: 1.2%). The effect is about 100x that.
- The tie-only alternative reproduces round 1: -1.10 / -0.74 / -0.29 ms at 0 / 20 / 50% ties. The memo remains the
  variant.

The ub.sh engine proxy (`ubench_econ`) was not re-run: it does not include the verify pass, so this flag cannot change
it. With the flag off the code is identical to 29bfc705.

## Translation to the node (estimate, not measured in vivo)

- **E thread.** The keccak pass over C (666 KB/blk at 450 MB/s) is replaced by SHA-256 over C||tail (1,677 MB/s) plus
  the guard. The ubench says -1.02..-1.07 ms/blk of on-CPU time. In vivo verify was 2.62 ms/blk (s124 CL21), about
  -3.4% of E on-CPU per 1k fills (30.4 ms on-CPU per block). E wall is -0.3..-0.8 ms/blk after the end_resident shadow
  (round-1 r2 estimate).
- **Ingress (new cost with the flag on).** Per locally verified action: +1 canonical encode of C||tail and +1 SHA-256.
  There is no extra Keccak: keccak(C) is forked from the ingress keccak pass the node already runs. That is about +0.4
  ms of SHA-256 plus about 0.15 ms of encode per block-equivalent per node, on RPC/gossip threads. Each of the 3
  validators pays it, plus the restash bookkeeping (one more FIFO insert per committed action).
- **Memory.** A second FIFO cache with the keccak cache's cap; each entry holds a 32 B key and (20 B sender, 32 B hash).
- **Net.** About 1 ms of CPU leaves the critical E thread per block, and about 0.55 ms is added on ingress threads.
  matched/s is expected at +0.2..+0.7%, under the 1% bar. A CPU-saturated cell box may show less.
- Without SHA-NI, `s80_local_trust_key_enabled()` is false and the node keeps the keccak path even with the env set.

## Risks and open points

- **Fork surface.**
  - What the memo adds: the consensus sort hash comes from a node-local cache instead of from the executed bytes.
  - What bounds it:
    - one seeding function under the existing provenance gate, keyed by SHA-256 of the full (C, nonce, sig) and
      derived from the same action object;
    - the dev-build debug_assert in the sort;
    - the sampled release guard (1 in 32 hits, first hit of every block), which voids the memo for the block on any
      mismatch.
  - What is left: a bug that corrupts a rare, unsampled entry would reorder a block on a warm node only. The hash
    fail-stop (on by default since batch 10) would catch the divergence, but as a stop, not a recovery. Before any
    promotion, run the 1-in-1 shadow mode (`TORUS_S80_LOCAL_TRUST_KEY_CHECK_EVERY=1`) for a soak.
- The guard checks the sort hash, not the sender. Senders are trusted verbatim, exactly as the keccak trust-cache
  already trusts them.
- `perf_equivalence_golden` does not exercise the flag (no mempool). The flag-on proof is the app-level test, the
  mempool path test and the consensus suite with the flag on.
- Clippy reports 129 warnings, all pre-existing in code this branch does not touch.
- The full workspace nextest was not run (s80 rule: the touched crates' suites).
- Composition: this does not stack with K1 or `exec-carried-verified-senders`, which remove the same pre-pass. It
  composes with `rule-versioned-trust-key`.

## Round 1 record (083baa6c)

Round 1 built the library functions and the ubench only, with no node wiring, and measured on an idle core
(CELL_BEFORE=0). Its tests ran in release only, which is why the P1 debug-assert failure was missed. Its premise check
and raw numbers are kept below unchanged. The full round-1 report is at
`/home/oz/projects/wt/novel-s80/proto/results/klk-fix/report-round1.md`.

## Premise check on 29bfc705 (re-located)

- E verify call site: `crates/torus-consensus/src/app.rs:5157`. It calls
  `batch_verify_native_actions_cached_with_sort_hashes` (`crates/torus-types/src/eip712.rs:1382`). That function calls
  `action_keys` -> `sort_hash_and_verified_cache_key_with_scratch` (`crates/torus-types/src/lib.rs:1460`) per action:
  - one canonical encode of C;
  - one Keccak absorb of C, forked into keccak(C) (the sort hash) and keccak(C||nonce||v||r||s) (the trust key);
  - then the trust-cache lookup `Mempool::verified_sender` (`crates/torus-mempool/src/lib.rs:587`, a read lock on
    `RwLock<FifoCache<B256, Address>>`).
- So E pays exactly **one Keccak pass over C per action**, and the sort (`app.rs:5361`,
  `sort_native_actions_indexed_with_sort_hashes`, `native_executor.rs:16327`) reuses the forked hash.
- The cost is real on 29bfc705: s124 9j2 CL21 (this binary) measured verify at 2.62 ms/blk in vivo, 60.5 actions per
  call (map/measured.md section 3.1).
- Raw hash speed on ozarchy, single thread, 1 MiB, best of 5 (printed by the ubench):
  - keccak256 (keccak-asm): **450-451 MB/s**, matching the in vivo ~455 MB/s;
  - sha256: **1,676-1,677 MB/s** (3.7x);
  - `sha256_hw_accelerated()` = true.
- sha2 0.10.9 on x86_64 without the `asm` feature (no `sha2-asm` in Cargo.lock) dispatches to its SHA-NI backend via
  `cpufeatures::new!(shani_cpuid, "sha","sse2","ssse3","sse4.1")` (`sha2-0.10.9/src/sha256/x86.rs:41-51`).
  `sha256_hw_accelerated()` runs the same CPUID test. Round 2 replaced the tautological unit test for it with FIPS
  known answers; which backend runs is evidenced by throughput only.
- Bench-shaped block: 37 PlaceOrderBatch x 400 orders (45 B per order: cloid share 0, as in the 300-market cells)
  + 23 CancelAll. **C = 666,415 B per block.** At 450 vs 1,676 MB/s the hash cost alone is 1.48 vs 0.40 ms, so the
  premise holds.

## Round 1 microbench (083baa6c, raw, unchanged)

Command: the `exec_hash_once_ubench` test binary, `trust_key_ubench_bench_block`, `--ignored --test-threads=1`,
pinned with `taskset -c 6`, run under the cargo-heavy lock. No cell was active before or after (CELL_BEFORE=0,
CELL_AFTER=0). Two separate processes.

Each process does, per tie share:
- 12 rounds in the order K S S K K S | K T T K K T (K = keccak, S = sha256 memo, T = sha256-tieonly);
- each round 5 warm-up reps plus 200 timed reps; one median per round.

The timed region is the E path:
- key pre-pass + lookup + phase-1 skip (`batch_verify_*`);
- `sort_native_actions_indexed_with_sort_hashes`;
- the replay-guard clone sits between the two and is untimed.

Caches: `RwLock<HashMap>` with 65,536 filler entries, seeded outside the timed region. Log:
`/home/oz/projects/wt/novel-s80/proto/results/ub-keyed-local-trust-key-ab.log`.

Per-round medians, verify+sort ms. Each row is process 1 then process 2.

| tie | keccak rounds (6 per process) | sha256-memo rounds (3) | sha256-tieonly rounds (3) |
|---|---|---|---|
| 0% p1 | 1.6856 1.6851 1.6852 1.6857 1.6849 1.6848 | 0.5841 0.5842 0.5843 | 0.5899 0.5898 0.5900 |
| 0% p2 | 1.6866 1.6872 1.6877 1.6873 1.6865 1.6874 | 0.5867 0.5868 0.5869 | 0.5925 0.5925 0.5930 |
| 20% p1 | 1.6867 1.6874 1.6872 1.6865 1.6869 1.6868 | 0.5853 0.5855 0.5856 | 0.9538 0.9539 0.9526 |
| 20% p2 | 1.6856 1.6852 1.6852 1.6855 1.6852 1.6864 | 0.5848 0.5847 0.5845 | 0.9506 0.9507 0.9508 |
| 50% p1 | 1.6885 1.6876 1.6876 1.6873 1.6879 1.6878 | 0.5855 0.5857 0.5857 | 1.4052 1.4062 1.4052 |
| 50% p2 | 1.6867 1.6887 1.6876 1.6875 1.6870 1.6865 | 0.5863 0.5880 0.5858 | 1.4056 1.4061 1.4062 |

Split of the means: verify 1.679-1.681 ms vs 0.578-0.580 ms; sort 0.0065-0.0070 ms in both arms (60 entries).

Delta of means (keccak -> arm), per process:

| tie | sha256 memo | sha256-tieonly |
|---|---|---|
| 0% | -1.1010 / -1.1003 ms (-65.3% / -65.2%) | -1.0953 / -1.0944 ms (-65.0% / -64.9%) |
| 20% | -1.1014 / -1.1008 ms (-65.3% / -65.3%) | -0.7335 / -0.7348 ms (-43.5% / -43.6%) |
| 50% | -1.1021 / -1.1006 ms (-65.3% / -65.2%) | -0.2823 / -0.2814 ms (-16.7% / -16.7%) |

Round-to-round spread inside an arm is at most 0.3%. The effect is about 200x the spread.

### Round 1, under load (one process)

stress-ng is not installed on ozarchy. The load was 24 unpinned `bash -c 'while :; do :; done'` busy loops, with the
ubench pinned to CPU 6. The run held the s80 cell lock (so no cell could start) and the cargo-heavy lock. No cell was
active (CELL_ACTIVE=0). loadavg was 45.06 at start and 41.98 after. Ties 0 and 50% only. Log:
`/home/oz/projects/wt/novel-s80/proto/results/ub-keyed-local-trust-key-load.log`.

Raw hash speed under this load:
- keccak256: **242 MB/s** (vs 450 idle);
- sha256: **1,676 MB/s** (unchanged).

The scalar busy loops share integer ports with keccak-asm on the SMT sibling, while SHA-NI runs on its own unit. So
this load favours the SHA-256 arm more than a mixed load would.

Per-round medians, verify+sort ms:

| tie | keccak (6) | sha256-memo (3) | sha256-tieonly (3) |
|---|---|---|---|
| 0% | 3.0518 3.0516 3.0527 3.0514 3.0516 3.0525 | 0.6488 0.6492 0.6488 | 0.6567 0.6566 0.6567 |
| 50% | 3.0527 3.0537 3.0538 3.0539 3.0533 3.0533 | 0.6496 0.6496 0.6496 | 2.1222 2.1218 2.1196 |

Delta of means:
- **sha256 memo:** -2.4030 ms (-78.7%) at 0% ties, -2.4038 ms (-78.7%) at 50%.
- **tie-only:** -2.3953 ms at 0%, -0.9322 ms (-30.5%) at 50%.

The under-load kill bar (< 0.6 ms) is not met either.
