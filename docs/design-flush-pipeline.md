# Design — flush pipeline (Layer-3 Option 1): adaptive parallel state-root + async post-flush

Branch `perf/l3-flush-pipe` (from `perf/re-proof5` @ c1f3d1b). Scope = the doc
`l3-work-budget.md` **Option 1** on the ONE exec-worker thread: (a) an adaptive
engagement threshold for the existing parallel bucket-hash state-root, and (b)
an async post-flush stage that moves the deferrable write tail off the per-block
exec critical chain. Target contribution ~15-25 ms/view. **Crash-window analysis
is mandatory and precedes code (below).** Consensus / hotstuff crates are NOT
touched; the 64-slot exec channel, the qc−committed backoff, and the S470 wedge
machinery are untouched.

## 0. The exec critical chain today (native path, `execute_committed_block`, app.rs:988)

Strictly serial on ONE exec worker thread, per committed block:

```
verify ─► engine(match/settle/gov/fees/epoch) ─► save_books(in-RAM→resident)
      ─► flush = { nonces + native state + native-trie root + APPLIED-HEIGHT MARKER }  ONE atomic RocksDB batch
      ─► evm_resync (off-by-default incremental EVM mirror)
      ─► body_persist (JSON block body → CF_BLOCK_BODIES)
```

Measured at cap-400 loaded: flush ~28 ms (root ~7.5 ms + state_write ~rest),
engine ~16 ms, body_persist ~5-10 ms (profiling P1 share 10.1 %).

**The applied-height marker** is folded INTO the flush batch
(`flush_with_native_trie_stats(..., Some(height), ...)`, backend.rs:682-688): native
state, nonces, trie ops, and "height N is applied" all commit in ONE atomic
`WriteBatch`. This is the crash-safety fence: a hard crash can never leave native
state durable with the height un-marked (double-apply) or the height marked with
native state lost. Restart replays from the marker (`read_native_applied_height`,
app.rs:1017; `replay_committed`).

**Decisive pre-existing fact for body_persist.** The block body is ALREADY written
durably to `CF_BLOCK_BODIES` at COMMIT/DISPATCH time — `persist_committed_block_durably`
(FIX 1a, app.rs:587, called at app.rs:4128 in `dispatch_to_exec`) — BEFORE the block
enters the exec channel. The exec-time body write (app.rs:1491-1496) is documented as
an "idempotent repeat" (app.rs:4127) that only additionally covers the boot-replay
execute path. Crash-recovery (`replay_gap`→`load_replay_body`) reads the DISPATCH-time
write, proven by the existing test `crash_after_commit_before_execute_recovers_via_durable_body`
(app.rs:5702). **So the exec-time body_persist is not on any durability-critical path;
deferring it cannot open a new hole.**

## 1. Lever (a) — adaptive parallel state-root engagement

`TORUS_PARALLEL_BUCKET_HASH` (native_trie.rs:589): `>=2` = N worker threads for
per-bucket leaf hashing, else serial. `run_buckets` (native_trie.rs:891) gates on
`if parallel <= 1 || work.len() <= 1` (`work.len()` = dirty-bucket count).

Diagnosis `l3-diagnosis.md §1`: at cap-400's ~226 dirty buckets the scoped-thread
spawn/join cost DOMINATES (p1 4.22 ms vs p4 6.47 ms); parallel only wins on large
dirty sets. Today's bench config runs `TORUS_PARALLEL_BUCKET_HASH=1` (serial) for
exactly this reason — which also loses the win on the *uncapped*/large-dirty shape.

**Change.** Add `TORUS_BUCKET_HASH_MIN_BUCKETS` (native_trie.rs `bucket_hash_min_buckets()`,
OnceLock, same idiom as the sibling readers). The parallel path engages only when
`work.len() > min_buckets`; else serial. Threaded as an explicit parameter into
`apply_native_dirty` → `run_buckets` (env read once at the backend.rs production call
site; the differential-test entry `apply_native_dirty` stays env-free — the value is
explicit). Gate becomes `if parallel <= 1 || work.len() <= min_buckets`.

**Default = exact-today.** Default `min_buckets = 1` reproduces the current
`work.len() <= 1` gate byte-for-byte (parallel engages at `>= 2` buckets exactly as
today) for every existing value of `TORUS_PARALLEL_BUCKET_HASH`. An operator sets e.g.
`TORUS_BUCKET_HASH_MIN_BUCKETS=3000` (the diagnosis-recommended crossover) so parallel
engages only on large dirty sets and cap-400 stays serial.

**Determinism.** Serial and parallel produce byte-identical output for ANY input and
thread count (per-bucket leaf hashes are pure; the tree fold is serial in bucket
order — the existing 20×-run determinism invariant). The threshold only selects WHICH
path runs, never the result. Covered by a boundary determinism test (root identical at
`work.len() == min` vs `== min+1`, serial vs parallel). Value-neutral, node-local, no
format/consensus impact.

## 2. Lever (b) — async post-flush stage (`TORUS_ASYNC_POST_FLUSH`, default OFF)

Reuses the precedent `BackgroundCfWriter` (bg_writer.rs): a single bounded
`sync_channel(cap)`, single-producer (exec thread) / single-consumer (writer thread),
drains-on-`Drop` with `join`. A second instance `post_flush_writer` on
`ExecutionContext`, spawned `Some` iff `TORUS_ASYNC_POST_FLUSH` is on (mirrors
`trade_writer`; `None` = exact-today synchronous write).

**What moves async: `body_persist` (CF_BLOCK_BODIES) ONLY.** The exec-time body write
is serialized on the exec thread (deterministic, matches the bg_writer idiom of queuing
pre-serialized `RawCfKv`) and the RocksDB put is handed to the writer thread. When the
writer is absent/gone the bytes are written synchronously (no row ever lost).

**What STAYS synchronous, and why (constraint #2 — read-your-writes for exec):**

- **state_write (native state + nonces + trie + marker).** engine(N+1) / verify(N+1) /
  the replay-dedup guard do RocksDB POINT-READS on this block's writes: nonces
  (`get_cf_raw(CF_NATIVE_NONCES)`, app.rs:1260), balances/accounts via the fresh
  `NativeStateOverlay::new(state_db)` for N+1, session/member fills. Resident books and
  the native-trie root cache are in-RAM (safe), but nonces/balances are NOT. Deferring
  would require read-through of the async queue for every point-read on the hottest,
  most S470-sensitive path — hairy. **Keep synchronous** (brief's explicit preference).
  It also carries the marker, so it is fenced-before-marker by construction (they are
  the same batch).
- **evm_resync** (app.rs:1451). Maintains the incremental EVM mirror
  (CF_HASHED_*/CF_TRIE_*) that `commit_evm_bundle_incremental` READS for an EVM block
  N+1. Off-by-default and best-effort today, but deferring introduces a read-your-writes
  hazard that would become a latent consensus bug if incremental-root-as-authority is
  ever enabled. **Keep synchronous.**
- **body_persist** is read by NO exec path — only RPC (`torus.rs:1281`, `eth.rs:122`)
  and block-sync serving, and it is already durable from the dispatch-time write.
  **This is the only safe deferral.**

**Ordering / backpressure (constraint #4).** One ordered queue, single producer =
commit order preserved. Bounded `cap` (256, as trade_writer): a full queue blocks the
exec thread's `send` (backpressure) — bounded memory, never unbounded. This backpressure
is entirely DOWNSTREAM of consensus: if the writer stalls, the exec thread drains the
64-slot exec channel slower, which lets qc−committed grow and the existing S470 backoff
engage — the mechanism is preserved, not hidden. **No consensus message, genesis field,
or protocol state changes. The 64-slot channel, qc−committed backoff, and hotstuff
crates are untouched.** Not consensus-visible.

## 3. CRASH-WINDOW TABLE (mandatory — constraint #1)

Executing block N; today's marker fence at flush batch commit. "body dispatch" = the
FIX-1a durable write that already happened before N entered the exec channel.

| # | Crash point | Durable on restart | Replay result | Regression vs serial? |
|---|---|---|---|---|
| W1 | mid engine(N), before flush batch | marker=N-1; state=N-1; body[N] durable (dispatch) | replay re-executes N from durable body → state identical | none (unchanged) |
| W2 | flush batch committed (marker=N), before evm_resync(N) | marker=N; state=N durable; incremental EVM mirror stale for N | replay SKIPS N (marker present); mirror is off-by-default + best-effort, re-synced lazily | none — evm_resync is post-marker TODAY too |
| W3 | marker=N durable; body[N] **queued, not yet drained** (async ON) | marker=N; state=N durable; **body[N] durable from DISPATCH** | replay SKIPS N; body already present → no hole, RPC/sync serve it | **none — body was durable before exec ran** |
| W4 | graceful shutdown, queue non-empty | writer `Drop` drains+joins → every queued body written | — | none |
| W5 | flush batch `db.write` itself fails | marker NOT written; state NOT written (atomic) | replay re-executes N | none (unchanged) |
| W6 | crash before dispatch-time body write (pre-exec) | pre-existing FIX-1a/heal window (commit manifest → peer pull) | unchanged by this work | none — not in our scope |

**Core guarantee.** The marker is durably advanced (W2/W3) only after every write it
COVERS (native state, nonces, trie) is durable — they are the same atomic batch, so the
fence is by construction, unchanged by this work. The only write we defer (body_persist)
is NOT covered by the marker and is ALREADY durable from dispatch before exec begins, so
W3 — the one new window — reconstructs to state byte-identical to the serial path. On
restart, replay from the marker reconstructs exactly today's state (final native root +
all consensus CFs identical); block bodies are equally present because their authoritative
write is the dispatch-time one, not the deferred exec-time one.

## 4. Body readers tolerate the widened window (constraint #3)

- RPC (`torus.rs:1281`, `eth.rs:122`) match on `get_cf_raw → Option` and return
  "not found" gracefully for a briefly-missing body.
- Block-sync serving reads the body written at DISPATCH (synchronous, before exec), so a
  serving node has the body as soon as it has committed the block — the deferred exec-time
  rewrite is irrelevant to serving. The requester side already retries.
- Net: no reader depends on the exec-time write's timing; the dispatch-time write is the
  contract.

## 5. Tests (acceptance c, e, f)

- `async_post_flush_state_identical_over_sequence` (app.rs test mod): execute an identical
  block sequence with the writer OFF and ON; assert final native root + CF_BLOCK_BODIES +
  CF_NATIVE_NONCES + consensus CFs byte-identical after the writer drains.
- `async_post_flush_crash_window_replay` (extends `crash_after_commit_before_execute_...`):
  marker=N durable, body dispatch-durable, exec-time body deferred and DROPPED undrained →
  `replay_committed` completes, applied=N, state identical, no fail-stop.
- `post_flush_writer_backpressure_bounded` (bg_writer.rs test mod): a bounded queue with a
  blocked consumer bounds in-flight batches; `send` blocks rather than growing memory.
- `bucket_hash_threshold_boundary_determinism` (native_trie.rs test mod): root identical at
  `work.len() == min` (serial) vs `== min+1` (parallel), across thread counts.
- `parse_bucket_hash_min_buckets_default_exact_today`: default parses to 1.
- µbench / timed: exec-critical-chain ms per block, serial vs pipelined, at a realistic
  dirty-set shape (cap-400 ~226 buckets).

## 6. Honest scope note

`state_write` (~20 ms, the largest single consumer) is NOT deferred — its read-your-writes
coupling to engine(N+1) makes async-with-read-through the `l3-work-budget.md` **Option 2**
(two-stage MVCC/overlay) design, out of this agent's scope. The safe deferral here is
body_persist (~5-10 ms) plus the adaptive-root threshold (which is exact-today at cap-400
and a win only on large/uncapped dirty sets). Measured contribution is reported honestly.
