# Fresh-database WAL-budget experiment

Status: verified isolated candidate based on `b60097a`; no live performance
result. The setting defaults off. Retained controls reportedly have about
6 GB WAL per node versus 0.4 GB SST; cold column families retaining the oldest
WAL are a hypothesis, not an established cause or a throughput limitation.
Compression, overwrites, and preallocation also affect those sizes.

## Change and boundaries

`TORUS_ROCKSDB_MAX_TOTAL_WAL_MB` accepts whole MiB using checked u64 conversion.
Unset or zero leaves RocksDB's automatic option unchanged. A positive value
sets `Options::set_max_total_wal_size` through `DbTuning` at StateDb open. Invalid
node values warn and leave the option unchanged; the benchmark rejects invalid
values before launch. Fixtures supply explicit tuning without mutating the
test runner's environment.

Bundled RocksDB 10.4.2 `include/rocksdb/options.h:778` defines this as a trigger
to flush CFs retaining the oldest live WAL. Zero automatically derives a limit
of four times the sum of per-CF buffer capacity; the 1 GiB aggregate memtable
budget in `torus-state/src/db.rs` does not replace that calculation.
`db/db_impl/db_impl_write.cc:1505,1998,2192` implements ordinary write-triggered
flush scheduling. This is a **soft threshold**, not a hard disk bound: an
in-flight batch and flush backlog can overshoot it. Idle shutdown does not
guarantee the threshold is reached again or all obsolete WALs are deleted.

WAL stays enabled; write batches, commit/replay ordering, atomic-flush setting,
and fsync policy stay unchanged. `backend.rs` still writes native state, trie
updates, and applied marker in one batch. RocksDB's `options.h:1397` explicitly
states that WAL-enabled recovery does not require atomic CF flushing. Current
commit fsync remains separately gated, off by default. This experiment does
not establish power-loss durability or qualify the execution pipeline's C1
pending-parent scenario.

## New databases only

Every control and treatment must use a new label and a new database path.
For a nonzero treatment, `run-cell.sh` rejects any existing `DATA_ROOT/data`
path (including a dangling symlink) during preflight. At launch, `FRESH_ONLY=1`
atomically claims the new data directory with `mkdir` and refuses if the path
has appeared meanwhile. This mode never enters the launcher's cleanup branch,
even if CLEAN=1 is also supplied. It never opens, flushes, compacts, deletes, or
otherwise maintains retained DBs. Fixture tests cover the preflight/launch race.
The generic StateDb API still supports reopening: a same-cell crash restart
uses the experiment's newly created DB with the same flag; this is not
permission to apply the setting to older campaign databases.

Pass the setting through the existing EXTRA_ENV argument, for example
`TORUS_ROCKSDB_MAX_TOTAL_WAL_MB=1024`. The runner records explicit zero for the
default and canonicalizes the treatment value. `cell.node_env` and per-node
environment digests retain the flag; `cell.wal_budget` records requested MiB,
effective option bytes, and automatic/explicit mode. Zero means RocksDB's
automatic policy, not a zero-byte limit. Historical summaries without the
flag are marked `recorded=false`. This records configuration; the binary hash
must still identify a build containing the option. No existing run was launched
or rescored by preparing this candidate.

## Qualification before any performance claim

Commands for the root agent to schedule when no performance cell is active:

```sh
CARGO_TARGET_DIR=/home/18c/.cargo-target-matched cargo test --release -p torus-state --test wal_budget
python3 tools/matched-bench/test_wal_budget.py
```

The Rust fixture uses temporary fresh databases and explicit options. Parsing
checks cover unset/zero, ASCII integer syntax, and overflow; persisted OPTIONS
checks distinguish unset/zero from a 1 MiB trigger. A child writes one cold
sentinel and 4 MiB of hot data in cross-CF state/marker batches, below normal
memtable thresholds. Treatment readiness requires RocksDB's oldest-WAL flush
log plus cold-CF SST bytes and flush statistics; the control requires no flush.
No manual CF flush, WAL flush, or fsync manufactures this result. After an
acknowledged tail batch the parent verifies the child's identity and SIGKILLs
it, then reopens and checks the cold sentinel, every hot row, and matching
state/marker tail. The child/controller have finite deadlines and cleanup of
their own temporary fixture only. This is a process-crash test, not a claim
that killing during every possible partial flush has been exhaustively tested.

First performance comparison: automatic versus 1024 MiB per node, same frozen
binary and otherwise identical settings, fresh paths, interleaved repeats.
Capture peak/final WAL logical and allocated bytes, SST and total disk bytes,
flush/compaction bytes, write stalls, latency, matched-fill throughput, restart
time if tested, and strict drain/agreement. More small flushes can increase L0
files, write/read amplification, CPU/I/O contention, and tail latency. Any disk
or page-cache benefit is unmeasured; a RocksDB block-cache benefit is not
implied. Preserve failed runs and existing retention decisions. A smaller
threshold is a follow-up only if evidence warrants it.

## Verification

Receipt `8d837342-6089-427c-9d9f-96895e08ed9a`: 76 Python tests, summarizer
checks, four WAL integration tests, 131 state tests (two ignored), separate
node-only release build, shell syntax and diff checks passed. The fixture
observed an automatic cold-CF flush and preserved all acknowledged data after
SIGKILL. The first verification exposed an old explicit DbTuning test initializer
missing the new optional field; adding `None` fixed it before this full rerun.
No retained campaign database was operated. Live A/B remains pending capacity.
