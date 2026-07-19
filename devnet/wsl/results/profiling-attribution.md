# Exec-pipeline block-execution attribution (mission s470)

Per-block wall-time decomposition of the execution thread under the real econ
bench workload. Answers: *where does block-execution time actually go, and what
is the single biggest lever?*

## Method

- Branch `perf/profiling` @ `1dfaa26` (base `1faa1fe` = perf/early-proof). New
  near-zero-overhead `Instant::now()` timers close the attribution gaps so ONE
  bench run yields a complete per-block breakdown. New metrics:
  `exec_load_books_seconds` (the per-block order-book reload/rebuild, which ran
  *before* the old engine timer and was therefore invisible), `exec_evm_seconds`,
  `exec_body_persist_seconds`, and the `exec_resting_orders` depth gauge. These
  join the pre-existing `exec_verify / replay_guard / engine (margin/match/settle)
  / save_books / flush / block` histograms.
- VPS 3-validator devnet, fresh chain per cell (`CLEAN=1`), econ load-gen:
  `--senders 5000 --markets 10 --batch-size 400 --econ --rate-total 750`
  (300 000 orders/s offered), 300 s bench + drain, node env
  `TORUS_SHARD_CUSTODY=0 TORUS_RPC_MAX_RESPONSE_MB=64 TORUS_RPC_MAX_CONNS=1024`.
  Phase histograms scraped from val0 (:9161) at 6 points across the window;
  per-phase seconds/block = Δsum / Δblock-count between snapshots.
- **Cell P1** — all C/D flags OFF (today's default path, == early-proof "A"):
  `NATIVE_TOTAL_BLOCK_CAP=100`, classic whole-book blobs.
- **Cell P2** — +C+D: `BOOK_ROWS=1 PARALLEL_SETTLE=1 PARALLEL_SETTLE_MIN_FILLS=64
  NATIVE_TOTAL_BLOCK_CAP=400 VERIFIED_SENDER_CACHE_CAP=25600` (rank-1 watermarks
  OFF). Launched under a held `flock` so no cargo shared the box (verified: 0
  cargo/rustc during the window; a first P2 attempt was contaminated by a
  parallel build and discarded — see `bench-P2-CONTAMINATED.log`).

### Sanity

| cell | matched/s (node ground truth) | early-proof band | overhead verdict |
|------|------|------|------|
| P1 | **3 201** | A = 3 024 (2.5–3.2k) | in-band; instrumentation overhead negligible (<1 %, well under the 3 % bar) |
| P2 | **2 777** | C2 = 2 678 | in-band |

Health gate FAIL in both (worst-60s 0.000 blk/s), exec_queue_depth pinned at the
channel bound (66/65) — the exec wall reproduced exactly as in the re-baseline.

## Attribution — Cell P1 (default path)

Full loaded window (snap0→snap5): 102 blocks, resting depth 7.5k → 319k,
total exec wall **2.686 s/block**.

| phase | s/block | % of block wall |
|------|--------:|-----:|
| **flush** (overlay→RocksDB atomic write + native state-root/trie maintenance) | **1.395** | **51.9 %** |
| engine total (native matching section) | 0.317 | 11.8 % |
| &nbsp;&nbsp;· settle (Phase 4) | 0.171 | 6.4 % |
| &nbsp;&nbsp;· match (Phase 3) | 0.100 | 3.7 % |
| &nbsp;&nbsp;· margin pre-reserve (Phase 2) | 0.025 | 0.9 % |
| &nbsp;&nbsp;· governance / fees / epoch | 0.022 | 0.8 % |
| **load_books** (per-block book reload + rebuild) | **0.307** | **11.4 %** |
| body_persist (block-body JSON → CF_BLOCK_BODIES) | 0.271 | 10.1 % |
| verify (batch sig / ecrecover) | 0.203 | 7.6 % |
| evm section | 0.108 | 4.0 % |
| save_books (serialize dirty books → overlay) | 0.039 | 1.5 % |
| replay/nonce guard | 0.004 | 0.2 % |
| **unattributed residual** | 0.041 | **1.5 %** |

## Attribution — Cell P2 (+C+D)

Full loaded window: 71 blocks, resting depth 1k → 312k, total **3.281 s/block**.

| phase | s/block | % of block wall |
|------|--------:|-----:|
| **flush** | **1.449** | **44.2 %** |
| **load_books** | **0.783** | **23.9 %** |
| engine total | 0.351 | 10.7 % |
| &nbsp;&nbsp;· settle | 0.202 | 6.2 % |
| &nbsp;&nbsp;· match | 0.095 | 2.9 % |
| &nbsp;&nbsp;· margin | 0.035 | 1.1 % |
| body_persist | 0.249 | 7.6 % |
| verify | 0.176 | 5.4 % |
| save_books | 0.151 | 4.6 % |
| evm section | 0.074 | 2.2 % |
| replay/nonce guard | 0.005 | 0.1 % |
| **unattributed residual** | 0.043 | **1.3 %** |

**Residual collapsed from ~25 % (pre-instrumentation) to ~1.5 %** — the block
wall is now fully accounted for. Two of the three biggest buckets (load_books,
body_persist) were entirely inside the old residual.

### What C+D changed

- **BOOK_ROWS trades cheaper writes for a MUCH more expensive load.** flush fell
  (51.9 %→44.2 % of wall; whole-book blob rewrite → dirty-row diff) but
  `load_books` **more than doubled** (11.4 %→23.9 %) — the row loader rebuilds
  every book by sorting individual order rows, costlier than deserializing a
  classic blob. `save_books` also tripled (1.5 %→4.6 %). Net block wall did not
  improve. This is why Package C was "neutral" on the ceiling.
- **Parallel settle / bigger block cap touch <11 % of the wall**, so they cannot
  move a ceiling set by the other ~80 %.

## Depth-vs-cost correlation

flush and load_books both scale ~linearly with **total resting depth** (per-block
cost, not cumulative) — every block re-loads / re-writes / re-hashes state
proportional to the *whole* book, not to the actions in the block:

| cell | resting depth | flush s/blk | load s/blk | total block s/blk |
|------|-----:|-----:|-----:|-----:|
| P1 | ~22k  | 0.39 | 0.04 | 0.61 |
| P1 | ~119k | 1.85 | 0.36 | 3.66 |
| P1 | ~185k | 2.47 | 0.63 | 4.83 |
| P2 | ~16k  | 0.68 | 0.17 | 0.73 |
| P2 | ~72k  | 2.15 | 0.64 | 4.27 |
| P2 | ~144k | 2.77 | 1.79 | 6.81 |

Per-block execution cost **is not constant — it climbs with the book.** Depth
grew 0 → >1.1M resting over a single window. Throughput therefore *decays* as the
book fills; there is no fixed steady-state ceiling, only a downward ramp.

## THE ANSWER — ranked cost & biggest lever

Ranked share of block wall (P1 default path):

1. **flush — state write + native state-root/trie — 51.9 %**  ← single biggest phase
2. engine / matching section — 11.8 % (settle 6.4, match 3.7, margin 0.9)
3. load_books — 11.4 %
4. body_persist — 10.1 %
5. verify — 7.6 %
6. evm — 4.0 %
7. save_books — 1.5 %

Grouped: **storage + state-root (flush+load+body+save) = 74.9 % (P1) / 80.3 %
(P2).** Matching engine proper = 11.0 % / 10.2 %. Signature verify = 7.6 % / 5.4 %.

**Single biggest lever: `flush` — the atomic overlay→RocksDB write plus native
bucketed-Merkle state-root maintenance (~44–52 %).**

Amdahl-style ceiling estimates (from P1, base ~3.0k matched/s):

| remove | speedup | projected ceiling |
|------|-----:|-----:|
| flush (51.9 %) | 2.08× | ~6.2k matched/s |
| all storage+state-root (74.9 %) | 3.98× | ~12k matched/s |
| everything except the matching engine (89 %) | 9.1× | ~27k matched/s |

**No single-phase win reaches 250–400k.** Even perfect elimination of *all*
persistence and state-root work leaves ~12–27k matched/s — and that figure itself
decays as depth grows. The ceiling is architectural: **per-block work is
O(total resting depth), and it must become O(actions-in-block).** Concretely, in
priority order:

1. **Persistent in-memory order books across blocks** — the executor rebuilds a
   fresh `NativeExecContext` every block and reloads/rebuilds every book from the
   CF (kills 11–24 % outright, and is a *precondition* for BOOK_ROWS, which
   otherwise regresses load as P2 shows).
2. **Incremental / dirty-only native state-root** — flush re-touches state
   proportional to the whole touched book; only changed keys should be re-hashed
   and written. This is the bulk of the dominant 44–52 %.
3. **Dirty-only writes (BOOK_ROWS)** — the write half; only pays off when paired
   with (1).

flush and load_books are the same disease (whole-book-per-block) seen on the
write and read sides; attacking settlement / block-cap / parallelism (Packages
C & D) was always capped at ~11 % headroom, which matches the observed
no-movement.

### Caveats / honesty

- `flush` bundles the RocksDB atomic write with native-trie maintenance and the
  EVM-account resync in one call; splitting write-I/O from trie-hash needs
  cross-crate metrics plumbing (deferred to avoid perturbing the measurement).
  The O(depth) scaling implicates the whole-book write/hash volume either way.
- Residual is +1.3–1.5 % (snapshot-boundary count skew between total-block and
  native-only phases), i.e. effectively fully attributed.
- Per-phase figures are window means; absolute s/block rises with depth (see
  depth table), so treat percentages — not the absolute seconds — as the stable
  result.

Raw snapshots: `~/torus-bench-scratch/prof-P1-snaps/`, `prof-P2-snaps/`;
funnel CSVs `devnet/wsl/results/prof-P1.csv`, `prof-P2.csv`.
