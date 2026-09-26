# Design: vote state out of the shared RocksDB

## Problem
Since f05b20e the vote state (`HIGHEST_VIEW_PHASE_VOTED` [16], `LAST_VOTED_PROPOSAL` [18])
is written before the vote is sent. Campaign s66-abc3 measured that write
(`torus_vote_state_write_seconds`): p50 40-80 us, p99 41-164 ms, p999 > 164 ms,
mean 1-6 ms. With n=3 every vote is needed, so one slow save delays the QC.

## Context (from memory + exploration)
- `RocksKVStore::new(state_db.db_arc())` (torus-node main.rs:498, 569): the consensus
  CF lives in the same RocksDB as exec state. Same WAL, same write group, same write
  controller and the DB-wide 1 GiB memtable budget (`db_write_buffer_size`).
- So a vote save can wait behind (a) a 4 MB exec batch in the write group and
  (b) any write stall / memtable-budget stall. (a) alone should cost ~ms; the
  41-164 ms tail suggests (b) matters too. Unverified which one dominates.
- A plain column family does not help: WAL and write group are per DB.
- Vote keys are only written by three single-purpose batches
  (`set_highest_view_phase_voted`, `set_vote_state_atomic`, `set_last_voted_proposal`,
  hotstuff_rs block_tree/accessors/internal.rs) and never mixed with other keys.
  torus-unwedge / torus-wedge-inject do not touch them.
- Writes are `sync=false` today (process-crash safety only, not power loss).
- `StateDb::open(&cli.data_dir)`: data_dir is itself the RocksDB directory.
- s46: `TORUS_ROCKSDB_PIPELINED_WRITE=1` gave -10% view_ms at n=1, never confirmed.
- Expected gain is modest (mean save 1-6 ms per vote) unless the tail lines up with
  the 125-160 view timeouts per node per 300 s. The A/B answers that.

## Options
### Option A: small separate RocksDB for the two vote keys (recommended)
`RocksKVStore` gets an optional second `Arc<DB>` (`vote_db`) at `<data_dir>/vote_state`.
`write()` sends a batch whose keys are all vote keys to `vote_db`; any other batch
goes to the main DB (a mixed batch panics in debug, since the split would break
atomicity). `get()` / snapshot reads of the vote keys read `vote_db` first, falling
back to the main DB (migration for existing nodes: views only grow, so once
`vote_db` has the key it is the newer value). `clear()` clears both.
- Files: torus-consensus/src/kv_store.rs (+ tests), torus-node/src/main.rs (open + wire),
  kv_store tests.
- Pros: own WAL and write group, escapes exec write stalls; checksummed WAL, no
  hand-rolled format; hotstuff_rs untouched.
- Cons: second DB open at start (tiny); one more directory to know about.
- Effort: Medium. Risk: Low-Medium (safety-relevant keys; covered by tests).

### Option B: fixed-size file, pwrite + CRC
Same routing, but the two values live in a 64-byte record written with `pwrite`
(no fsync), with a CRC and two alternating slots for torn-write safety.
- Pros: fastest possible save (~us), no RocksDB machinery.
- Cons: hand-rolled on-disk format and recovery for safety-critical state; no
  real gain over A (A's tiny DB write is already ~10s of us).
- Effort: Medium. Risk: Medium.

### Option C: no code, `TORUS_ROCKSDB_PIPELINED_WRITE=1`
Already a flag. WAL writes and memtable inserts pipeline, shortening the queue
behind big batches.
- Pros: free to test.
- Cons: does nothing for write stalls; affects every writer.
- Effort: none. Risk: Low. Useful as a diagnostic arm.

## Recommendation
Option A, tests first, then an A/B: ctl (main) vs votedb, plus a pipelined arm
(option C) as a free diagnostic, n >= 4 with a warm-up cell. Success: vote-save
p99 < 1 ms and fewer view timeouts; throughput is the secondary readout.

## Not Building (YAGNI)
- fsync on the vote DB (power-loss safety is a separate decision; keep sync=false).
- A generic "route keys to stores" abstraction: two fixed keys only.
- Changes to hotstuff_rs.

## Open Questions
- Does RocksDB tolerate a subdirectory inside its own DB dir? (Test it; else use a
  sibling path, but a sibling survives a data_dir wipe and would keep a stale, higher
  highest_view_voted on a fresh chain: the node would refuse to vote.)
- Which part of the tail is write-group queueing vs stalls? The pipelined arm tells us.
