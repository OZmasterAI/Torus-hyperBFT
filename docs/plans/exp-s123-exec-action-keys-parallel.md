# exp/s123-exec-action-keys-parallel

Worktree: /home/18c/projects/Torus-hyperBFT/.claude/worktrees/wf_443f7b55-fcb-31
Branch: exp/s123-exec-action-keys-parallel (not committed, not pushed)
Patch: /home/18c/projects/wt/exp-s123/exec-action-keys-parallel.patch (one file, +430 / -18)
Flag: `TORUS_EXEC_PARALLEL_KEYS=1` (default off; only "1" after trim turns it on)

Rebased onto main `2bc88bcf`, built and unit-tested (see "Build and test results" at the end). Not benched.

## What changed (crates/torus-types/src/eip712.rs only)

- New items next to `verify_mode` (:1119-1146):
  - `pub fn parse_exec_parallel_keys(Option<String>) -> bool` (:1125). Pure, so it can be tested.
  - `fn exec_parallel_keys()` (:1132). A OnceLock over the env var.
  - `const PARALLEL_KEYS_MIN_ACTIONS: usize = 16` (:1139).
  - `fn use_parallel_keys(flag, mode, n)` (:1144). It is true only when the flag is on, the mode is not `Serial`, and n >= 16. It is a separate function so the tests can check the gate directly.
- `batch_verify_impl` takes a new last parameter, `parallel_keys: bool` (:1332-1377):
  - Gate off: the old interleaved loop runs exactly as before. Only the indent changed.
  - Gate on: `actions.par_iter().map_init(Vec::new, |buf, a| verified_cache_key_with_scratch(a, buf)).collect()` produces `Vec<Option<B256>>`.
    - It runs on the same pool phase 1 picks (`recover_senders_phase1`, about :1185): `pool.install` for `Pool(_)` with a dedicated pool, otherwise the global pool.
    - The lookups then run on the calling thread, in input order: `keys.into_iter().map(|k| k.and_then(|k| verified_lookup(&k)))`.
- Callers:
  - `batch_verify_native_actions_cached` (:1266, the live path, app.rs:3370) passes `exec_parallel_keys()`.
  - `batch_verify_native_actions_cached_with_mode` (:1295) passes `false`, so the mode-pinned tests stay exactly as they are.
  - The `#[ignore]` µbench calls `batch_verify_impl` directly and now passes `false`, so it also stays as it is.
- Docs: `VerifyMode` and `batch_verify_native_actions_cached` now say the key hashing may run on the verify pool while the lookups stay sequential. So the trust-cache sequence, senders and drops are identical, the change is node-local, and a mixed fleet is safe.

### Determinism argument

- `verified_cache_key_with_scratch` (torus-types lib.rs:1247-1261) is a pure function of the action. It clears the buffer first, so per-split buffer reuse cannot leak bytes from one action into the next.
- An indexed `par_iter` followed by `collect` into a `Vec` keeps input order. So `keys[i]` is exactly what the sequential loop computes for action i.
- `verified_lookup` is called in the same order with the same keys. It makes no call for a session action, whose key is `None`. Only the timing of those calls changes:
  - Today, hashing and lookups are interleaved.
  - With the flag on, all hashing happens first, then all lookups.
- The live lookup is `mempool.verified_sender` (torus-mempool lib.rs:515-529), a RwLock read plus hit/miss counters. A HIT equals a fresh recover by construction (eip712.rs doc at :1245-1258). So when a concurrent mempool insert lands relative to the lookup changes nothing that consensus can see. That timing is already nondeterministic today.
- Phases 1 and 2 are untouched.

## Why (evidence)

- The only production caller of the cached batch verify is app.rs:3370. `exec_verify_seconds` times exactly that call (app.rs:3362 to 3408).
- The trust cache is on by default (torus-node main.rs:193-204, `default_value_t = true`). The bench signs with EIP-712 by default (tools/bench-throughput/src/main.rs:135-141).
  - So phase 1 is mostly cache hits (eip712.rs `recover_one` returns `*hit`).
  - That leaves the sequential key pre-pass as the bulk of the verify phase: per action, canonical bytes || nonce || v || r || s, then keccak.
- Verify is 11.48 ms per native block in the c0 arm (perf doc sec 59.3).
- Sec 60.6 (perf doc line ~7783): the intact Keccak chains on the exec thread name `verified_cache_key_with_scratch`, next to `sort_native_actions_indexed`.
- The global rayon pool has only one live user, this file's phase 1. Checked by grep:
  - par_iter users are eip712.rs and torus-rpc.
  - torus-rpc uses its own `ingress_verify_pool` (torus.rs:565-578, 776).
  - gossip verify has a dedicated pool (torus-node main.rs:825).
  - exec matching uses `exec_pool` (torus-bridge exec_pool.rs:31).
  - So the key pass does not compete with another rayon user for the pool. It does still compete for CPU cores with the rest of the node.

## Expected gain (inferred, not measured)

- exec_verify: about 11.5 ms down to about 1.5-3 ms per native block at 300 m (keccak split across the global pool's cores, plus the rayon dispatch and the sequential lookups). Roughly 8-10 ms gross per native block.
- The pre-`begin_resident` window is at least about 28-30 ms (sec 59.3: end_resident 37.69 minus wait 7.36). end_resident is 15.55 ms after Phase 4 (sec 67.3). So a shorter verify phase can raise `end_resident_wait` by 0-2 ms. Net about 8-10 ms per native block.
- At sec 67.4's ratio (~0.13% matched/s per ms of block_ms) that gives:
  - block_ms -4 to -5%
  - native blocks/s +4-5%
  - matched/s +1-1.5%
  - CPU-s/1M about flat, with maybe a small rise from rayon overhead
- At 10 m: about 0, because exec is not the limit there (busy 0.77-0.80, sec 67.3).

## How to measure (later; nothing was run)

- Cell: 300 markets, ABBA n=4. A = env unset, B = `TORUS_EXEC_PARALLEL_KEYS=1` on all nodes, same binary. Then a 600 s soak pair.
- Do not change `TORUS_PARALLEL_VERIFY`. Default mode means the global pool, which is what the live path uses today.
- Metrics:
  - `exec_verify_seconds` per native block (expect about 11.5 down to about 1.5-3 ms)
  - residual_untimed
  - end_resident_wait (expect +0 to +2 ms)
  - block_ms
  - native blk/s
  - matched/s
  - CPU-s/1M fills
  - block time p99
- Optional 10 m pair to confirm about 0 there.
- Unit tests to run first: `cargo test -p torus-types parallel_keys` and `cargo test -p torus-types parse_exec_parallel_keys`. Also re-run the existing `parallel_verify_*` and `batch_verify_*` tests in the same module.

## Tests added (inline `mod tests`; all pass)

- `parse_exec_parallel_keys_only_one_enables`: the parse table. On: "1", " 1", "1 ", "\t1\n". Off: unset, "", spaces, "0", "2", "01", "10", "1x", "true", "on", "yes".
- `use_parallel_keys_gates_on_flag_mode_and_size`: MIN == 16. Below MIN, flag off and `Serial` all stay serial. `Default`, `Pool(2)` and `Pool(4)` with n >= MIN go parallel.
- `parallel_keys_fixture()`: 74 actions.
  - Five rounds of EIP-712 `PlaceOrderBatch` with 1, 50 and 400 orders, each with and without cloids, plus `ClaimRewards`.
  - A tampered-r signature and a tampered body (signed, then one price changed).
  - Session actions: valid; expired (expiry = timestamp - 1); out of scope (TransfersOnly session on a trading action); on an EIP-712-only action (Withdraw); unregistered key.
  - Four duplicates (a cold batch, a warm 400-order batch, a valid session action, the tampered signature).
  - Every third valid EIP-712 action is pre-filled in the cache map from `verified_cache_key`, each with its own sentinel sender, so a hit shows in the result.
- `verify_recording_keys`: calls `batch_verify_impl` directly. Its `verified_lookup` is backed by the map and records every key it receives in a `RefCell<Vec<B256>>`.
- `parallel_keys_identical_senders_and_lookup_sequence`:
  - The serial ground truth equals the fixture's expected senders, and the recorded keys equal `filter_map(verified_cache_key)` in input order.
  - It then checks `Serial`, `Default`, `Pool(2)` and `Pool(4)` (pools built in the test), plus `Pool(3)` with no pool (the fallback to the global pool).
  - Each runs with parallel_keys false and true, 3 repeats each. Senders and the recorded key sequence must equal the ground truth every time.
- `parallel_keys_small_block_takes_serial_path_with_same_outputs`: n in {0, 1, 15, 16}. It checks the gate decision, that flag-on equals flag-off (senders and key sequence), and that the result equals the expected prefix.

## Risks

- CPU contention: the key hashing moves onto the global pool, which by default has as many threads as there are cores. On a shared bench box (several nodes per host) these threads compete with the market workers, the verify pools and RocksDB.
  - Total keccak work is the same; rayon split and join overhead is added.
  - If exec is not the bottleneck it buys nothing.
  - The B arm's CPU-s/1M shows any rise.
- The rayon dispatch costs some µs per block. The 16-action floor keeps tiny blocks serial.
- Memory: one `Vec<Option<B256>>` of n x 33-40 bytes per block, plus one scratch buffer per split (each grows to the largest canonical preimage, about 30 KB for a 400-order batch). Negligible.
- No consensus risk, as argued above. The tests pin the identical-sequence claim, but they have not been run.
- `TORUS_PARALLEL_VERIFY=0|1` (`Serial`) forces the serial key pass even with the flag on. That is intended, because Serial means no rayon. It is documented.

## Untested / not done

- Nothing compiled or run (the owner's rule for this workflow).
- The sort-key half is left as a follow-up behind the same flag, after the cancel-all worktree merges: `action_sort_key` keccak at native_executor.rs:12725-12728, called from app.rs:3571.
- Also not done:
  - carrying senders from dispatch
  - any change to the identity of `verified_cache_key` or `compute_action_hash`
  - the session / ed25519 path
  - the per-action RwLock lookup in mempool lib.rs:515-529
  - turning it on by default

## Compile confidence: medium-high

- APIs used already appear in this file or in rayon 1.x: `par_iter`, `map_init`, `collect` into `Vec`, and `ThreadPool::install` with a `Send` closure over `&[SignedNativeAction]`. The `&[SignedNativeAction]` capture is already `Sync`-shared by `par_recover`.
- The type inference points I could not prove without a compiler:
  - `map_init(Vec::<u8>::new, |buf, action| ...)`: closure parameter types come from the `map_init` bound (the element type is now written out).
  - The test fixture closure `push_valid` with annotated `&mut Vec<_>` parameters, called several times with fresh `&mut` borrows. This is higher-ranked because the types are annotated.
  - `assert_eq!(Vec<T>, [T])` in the small-block test, through `PartialEq<[U]> for Vec<T>`.
- If anything fails, it should be a one-line inference fix: e.g. turning `push_valid` into a fn.

## Build and test results (s123 builder, on `2bc88bcf`)

- Built as written. Follow-up commit: two clippy `manual_is_multiple_of` warnings in the new tests now use
  `.is_multiple_of()`, and only the experiment's own hunks are in rustfmt layout (main's lines untouched).
- Flag off: `-p torus-types` 103/103; `cargo nextest run --workspace` 3868/3868 (0 flaky), doc tests pass.
- Flag on (`TORUS_EXEC_PARALLEL_KEYS=1`): `-p torus-types -p torus-consensus` 521/521.
- `cargo clippy --workspace --all-targets`: no new warnings vs main.
- Still open (bench, archy): the review asks for the `end_resident_wait` distribution (not just the mean) in
  both the ABBA and the soak, and `verified_sender_cache` hits/misses per arm.
