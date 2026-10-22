# s80 prototype: users-position-key-index

Branch `s80/users-position-key-index` (local only, not pushed), worktree
`/home/oz/projects/wt/novel-s80/proto/users-position-key-index`, base 29bfc705, commit 01fce9da.
Target dir `/home/oz/.cargo-target-s80-users-position-key-index` (release/torus-node md5
7c0b4de9a2f19edbd1318de90d7ed490, contains the flag string; bench-throughput fa8fd4bd unchanged; ubench_econ
binary `ubench_econ-1a6f456edd610a23`, built after the last non-test source edit).
Flag `TORUS_S80_POS_KEY_INDEX` (default off = 29bfc705 behaviour; `1` = on).

## 1. Premise check on 29bfc705

Re-located on 29bfc705 (main's line numbers differ):
- `AccountReader::get_position` `crates/torus-bridge/src/native_executor.rs:2850` = `resident_positions`
  (`:2840`: `dirty()` probe, then `Users::get`) then `users::find` (`users.rs:1537`, `binary_search_by_key`
  over `Vec<Position>`, 112-B stride), then a clone.
- `Users::get` `users.rs:705`: on 29bfc705 `opaque_of` already short-circuits when no trader is opaque, so it is ONE
  map probe (the write-up's "3 foldhash passes" is 1 hash + the dirty-set hash).
- `TypedRows::position for Users` `users.rs:1397` (pool maker lookups and the liquidation scan via PositionCache)
  = `get` + `find`.
- Phase 2: `prepare_one` `native_executor.rs:9959`, the position read `:10093` inside
  `st.proj.get_or_try_insert` (already once per (sender, market)); `SenderFold`/`SenderState` `:3108/:3117`.
- The batch reader's dirty set is frozen per `execute_batch` (`BatchSums::new`, `:2525`); Phase 2 writes no position.
- `Users::follow` (end_resident, R side) does the same 112-B-stride `binary_search_by_key` per changed row.

Profile (perf record -F 4999, flat self, ubench_econ UB_MARKS=1 UB_RUNS=2 300 markets, whole process,
29bfc705 binary, ozarchy):

| self % | symbol |
|---|---|
| 3.47 | `AccountReader<NativeStateOverlay>::get_position` (top symbol of the process) |
| 1.38 | `Users as TypedRows>::position` |
| 0.61 | `Users::get` |
| 2.57 / 1.58 | foldhash `hash_bytes_long` / `[u8;20]::hash` (all callers) |

`perf annotate`: 76.4% of `get_position`'s self samples and 76.4% of `Users::position`'s sit on the `cmova`
right after `cmp %rcx,0x50(%rax,%r10,1)` (`imul $0x70`): the binary search's dependent load over 112-B rows.
So ~3.7% of all process cycles are that one load stall. Premise holds (larger than the write-up's 1.43% + 1.28%
node estimate); not killed.

## 2. Design

1. Key index (`keys`): `UserState` gets `keys: Vec<MarketId>` kept in step with `positions` when the record set's
   `key_index` is on (`Users.key_index`, default = the process flag; tests set it per instance with
   `set_key_index`). Every mutation path keeps both: `classify` / `build_maps` (`sync_keys`), `follow` (in-place
   apply at end_resident: `search` / `insert_at` / `remove_at`), `UsersMut::set_position` / `remove_position`
   (Phase 5 write-through), `reload` via `set` -> `classify`. In-place value replacement (`positions[i] = p`,
   `mem::replace`) keeps the market id, so keys need no update there. Lookups go through
   `Rec<'a> { keys, ps }::find`: keys non-empty -> `keys.binary_search` (8-B stride; ~280 keys = 2.2 KB, the last
   3 probes in one cache line), else the old `find`. Used by `AccountReader::get_position`,
   `TypedRows::position` (pool makers, liquidation scan), `Users::row` (ResidentSource positions).
   Flag off: `keys` stays empty (no allocation), every lookup / mutation is the old code.
2. Hoist (`hoist`): `SenderState<'r>` gets `rec: Option<Option<Rec<'r>>>`; Phase 2 reads
   `reader.resident_positions(sender)` (dirty probe + map probe) once per sender per fold and then calls
   `get_position_in(rec, ..)` per market. Valid because the batch reader's dirty set is frozen and Phase 2 writes
   no position (same `Arc<Users>` for the whole fold; the lifetime `'r` ties the fold to the reader). A `None`
   (dirty / opaque / no records) falls back to the overlay read per market, as before. The `#[cfg(test)]`
   shadow check / counters stay per call.
3. Flag `TORUS_S80_POS_KEY_INDEX`: `1`/`true` = both; `keys`, `hoist` = one (attribution); unset/empty/`0`/`false`
   = off; anything else = off + WARN. Read once (`OnceLock`), INFO line
   `TORUS_S80_POS_KEY_INDEX on=<bool> keys=<bool> hoist=<bool>` at the first read (the first record-set build,
   `build_resident` at node start, or the first Phase-2 fold, whichever comes first).

Cost: `UserState` 112 -> 144 B even with the flag off (+24 B empty Vec, padded to 16-B alignment; pinned in
`users_size_of_pins`); +8 B per position with the flag on (~11 MB at 1.4M positions). The off arm's ubench
numbers (2.77-2.92) sit inside the 29bfc705 calibration band (2.79-2.93), so the wider struct costs no visible
time with the flag off.

## 3. Diff summary

`git diff 29bfc705..01fce9da --stat`: native_executor.rs +44/-19 (Rec-returning `resident_positions`,
`get_position_in`, `SenderFold<'r>` / `SenderState<'r>` with `rec`, hoisted read in `prepare_one`), users.rs
+190/-19 (flag parse, `Rec`, `UserState.keys` + `search`/`insert_at`/`remove_at`/`sync_keys`/`keys_in_step`,
`Users.key_index`, `rec`, `set_key_index`, every mutation path, `self_consistent` checks keys), users_tests.rs
+149/-1. 3 files, +387/-34.

## 4. Tests

New (users_tests.rs):
- `pos_key_index_keys_in_step_and_lookups_equal`: 6 seeds x 80 random blocks (positions, balances, cvlm,
  irregular rows: opaque traders come and go) + 8 random typed/raw `UsersMut` ops per block; key index on vs off
  side by side: `same_as`, keys in step (on) / empty (off), and for every trader x market (incl. 0, out of range,
  `MAX`): `rec().find`, `TypedRows::position`, `ResidentSource::get(POSITIONS)` byte-equal; `self_consistent`
  (now also checks the keys).
- `pos_key_index_many_markets_per_trader`: 3 traders x up to 400 markets, 20k inserts/removes, lookups equal.
- `pos_key_index_flag_parse_and_default`: parse table; prints the process flag; a new record set takes it.
- `users_size_of_pins`: UserState 144.
Red: the new tests reference `Users::rec` / `set_key_index` / `keys_in_step`, absent on 29bfc705 (compile-red by
construction; the first dev build caught a borrow error in the test itself, fixed).

Results (dev profile, ozarchy, nextest, `--cargo-quiet --status-level fail --final-status-level fail`):
- perf_equivalence_golden (`-E 'binary(perf_equivalence_golden) | test(perf_equivalence_golden)'`):
  `TORUS_S80_POS_KEY_INDEX=0`: 5 tests run: 5 passed, 1104 skipped (4.8 s);
  `TORUS_S80_POS_KEY_INDEX=1`: 5 tests run: 5 passed, 1104 skipped (4.9 s).
- `cargo nextest run -p torus-bridge` (this session, commit 01fce9da):
  `TORUS_S80_POS_KEY_INDEX=0`: 1092 tests run: 1092 passed, 17 skipped (54.1 s);
  `TORUS_S80_POS_KEY_INDEX=1`: 1092 tests run: 1092 passed, 17 skipped (54.5 s)
  (includes the C7 shadow-check tests and every `self_consistent` caller with keys on). The first session read
  1091 passed / 17 skipped in both arms. No flaky counts.
- parse test: flag 0 prints `flag=PosKeyIndex { keys: false, hoist: false }`, flag 1 `{ keys: true, hoist: true }`.
- `cargo clippy -p torus-bridge --all-targets`: 0 errors; no warning on any line the diff adds (all warnings are in
  untouched files or untouched lines, pre-existing on 29bfc705).
- release torus-node: `cargo build -q --release --locked -p torus-node` at 09:31 is a no-op (rc 0, md5 still
  7c0b4de9a2f19edbd1318de90d7ed490): the binary is fresh for commit 01fce9da and contains the flag string.

## 5. Microbench (ubench_econ via ub.sh, UB_MARKS=1 UB_RUNS=5 300 markets TORUS_SETTLE_STREAMED=1)

ABBA order off,on,on,off,off,on,on,off,off,on,on,off (6 rounds per arm; rounds 1-4 and off-5 in the first
session 06:49-08:04, on-5/on-6/off-6 08:25-08:37, other agents' cells interleaved via the build lock).
Per invocation: MEDIAN engine ms/1k fills, then the per-sub-phase median of the 5 runs
[exec, margin, match, settle, tail(liq)] ms/1k, engine ms/blk, end_resident users ms/blk, then the 5 raw runs.
fills/blk identical in every invocation (median 11,644): same work in both arms.

| label | median | exec | margin | match | settle | tail | eng ms/blk | users ms/blk (R) | raw runs |
|---|---|---|---|---|---|---|---|---|---|
| off-1 | 2.79 | 2.30 | 0.57 | 0.15 | 0.80 | 0.49 | 32.6 | 2.31 | 3.03 2.88 2.79 2.75 2.73 |
| on-1 | 2.71 | 2.27 | 0.51 | 0.16 | 0.79 | 0.44 | 31.5 | 2.06 | 2.74 2.71 2.73 2.70 2.67 |
| on-2 | 2.67 | 2.24 | 0.51 | 0.14 | 0.78 | 0.43 | 31.1 | 2.06 | 2.73 2.70 2.67 2.66 2.63 |
| off-2 | 2.92 | 2.39 | 0.59 | 0.16 | 0.81 | 0.50 | 33.1 | 2.22 | 2.93 2.92 2.92 2.78 2.80 |
| off-3 | 2.77 | 2.28 | 0.55 | 0.15 | 0.78 | 0.47 | 32.3 | 2.22 | 2.82 3.27 2.77 2.69 2.74 |
| on-3 | 2.73 | 2.29 | 0.51 | 0.16 | 0.80 | 0.43 | 31.6 | 2.08 | 2.73 2.71 2.97 2.73 2.67 |
| on-4 | 2.66 | 2.22 | 0.51 | 0.15 | 0.79 | 0.43 | 31.3 | 2.05 | 2.77 2.75 2.59 2.66 2.63 |
| off-4 | 2.85 | 2.36 | 0.57 | 0.17 | 0.80 | 0.48 | 32.8 | 2.24 | 2.87 2.85 2.82 2.79 2.99 |
| off-5 | 2.81 | 2.32 | 0.57 | 0.15 | 0.78 | 0.48 | 32.7 | 2.23 | 2.91 2.82 2.77 2.76 2.81 |
| on-5 | 2.72 | 2.28 | 0.52 | 0.16 | 0.79 | 0.43 | 31.3 | 2.07 | 2.81 2.72 2.67 2.64 3.02 |
| on-6 | 2.68 | 2.25 | 0.51 | 0.15 | 0.79 | 0.43 | 31.2 | 2.08 | 2.73 2.72 2.68 2.64 2.67 |
| off-6 | 2.77 | 2.29 | 0.56 | 0.15 | 0.79 | 0.48 | 32.2 | 2.24 | 2.85 2.85 2.75 2.70 2.77 |

Means of the per-invocation medians, off -> on (n = 6 / 6):

| metric | off | on | delta |
|---|---|---|---|
| engine ms/1k fills | 2.818 | 2.695 | **-4.4%** |
| engine ms/blk | 32.62 | 31.33 | -3.9% (-1.28 ms/blk) |
| exec ms/1k | 2.323 | 2.258 | -2.8% |
| margin ms/1k | 0.568 | 0.512 | -10.0% |
| match ms/1k | 0.155 | 0.153 | -1.1% (noise) |
| settle ms/1k | 0.793 | 0.790 | -0.4% (noise) |
| tail(liq) ms/1k | 0.483 | 0.432 | -10.7% |
| end_resident ms/blk (R side, not in engine) | 2.742 | 2.548 | -7.1% |
| users follow ms/blk (R side) | 2.243 | 2.067 | -7.9% |

No overlap: off medians 2.77-2.92, on medians 2.66-2.73 (every on invocation below every off invocation).
The gain is about 1.5x the spread (~3%) of the off arm's medians. Settle does not move: settle's position reads go
through the overlay write path, not the record lookup; the cut is where the profile put it (Phase 2 margin
prepare and the liquidation tail's per-trader position reads), plus R's follow (keys searched there too, net of
the extra key insert/remove).

Attribution (first session, 3 rounds each, order keys,hoist,hoist,keys,keys,hoist; not interleaved with off):

| label | median | exec | margin | match | settle | tail | raw runs |
|---|---|---|---|---|---|---|---|
| keys-1 | 2.73 | 2.29 | 0.54 | 0.19 | 0.79 | 0.43 | 2.77 3.04 2.68 2.73 2.72 |
| hoist-1 | 2.73 | 2.25 | 0.54 | 0.15 | 0.77 | 0.48 | 2.86 2.80 2.72 2.73 2.71 |
| hoist-2 | 2.77 | 2.29 | 0.55 | 0.14 | 0.80 | 0.48 | 2.84 2.81 2.77 2.75 2.74 |
| keys-2 | 2.75 | 2.30 | 0.53 | 0.17 | 0.79 | 0.44 | 2.79 2.80 2.73 2.67 2.75 |
| keys-3 | 2.69 | 2.25 | 0.52 | 0.14 | 0.78 | 0.44 | 2.76 2.73 2.69 2.68 2.65 |
| hoist-3 | 2.76 | 2.28 | 0.55 | 0.15 | 0.79 | 0.48 | 2.83 2.85 2.76 2.73 2.76 |

keys only 2.723 (-3.4% vs off 2.818), hoist only 2.753 (-2.3%), both 2.695 (-4.4%). The key index carries the
liquidation tail (0.483 -> 0.437) and part of margin (0.568 -> 0.53); the hoist only margin (0.568 -> 0.547).
Together they add up to about the combined margin cut (0.512). Both are worth keeping.

## 6. Verdict

Built. Byte-identical by test (golden flag off and on, the differential record-set test on vs off, the crate
suite in both arms). ubench_econ engine -4.4% ms/1k fills (-1.28 ms/blk, 32.6 -> 31.3), outside the
spread (no overlap over 6 ABBA rounds per arm), above the skeptic-corrected estimate (-0.2..-1.0 ms/blk).
Sub-phases: margin -10.0%, tail(liq) -10.7%, settle -0.4% (unchanged), match unchanged; R-side follow -7.9%.
cell_worthy: yes. Cell arm: `TORUS_S80_POS_KEY_INDEX=1` (off arm: unset). The node log must show
`TORUS_S80_POS_KEY_INDEX on=true keys=true hoist=true`. Expected cell effect: exec thread -1.3 ms/blk of a
~38 ms/blk exec block (map/measured.md: margin 10.3 ms/blk = 27% of the exec block; exec busy 0.88 = the
ceiling), plus R-side follow -0.2 ms/blk (exec waits 1.53 ms/blk on end_resident): about +2..+3.5% matched/s if
the cut passes through 1:1, less if another stage binds.
Risks / follow-ups: +32 B per trader record even with the flag off (size pin updated); a production version
would drop the flag and make `keys` unconditional (then `Rec` needs no empty-keys branch).
