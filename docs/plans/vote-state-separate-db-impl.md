# Implementation Plan: vote state in its own RocksDB

## Design Decision
Option A of `vote-state-separate-db.md`: a small RocksDB at `<data_dir>/vote_state`
holds `HIGHEST_VIEW_PHASE_VOTED` [16] and `LAST_VOTED_PROPOSAL` [18]. Routing lives in
`RocksKVStore` (torus-consensus/src/kv_store.rs); hotstuff_rs is untouched.

## Success Criteria
1. With a vote DB attached, batches of vote keys land only in the vote DB; every
   other batch lands only in the main DB.
2. Vote-key reads (`get` and snapshot) prefer the vote DB and fall back to the
   main DB (nodes upgraded mid-chain keep their old vote state).
3. A batch mixing vote keys and other keys panics before writing anything
   (splitting it would break atomicity; hotstuff_rs never builds one).
4. The vote DB inside the real StateDb directory survives flush + compaction of
   the main DB and a reopen of both (the open question in the design).
5. `clear()` clears both. Vote-state writes are still timed.
6. Existing torus-consensus, hotstuff_rs and torus-node tests still pass.
7. Bench: vote-save p99 < 1 ms; view_qc_collect and view timeouts down vs main.

## Tasks
### Task 1: tests (all must fail to compile or fail)
New file `crates/torus-consensus/src/kv_store_vote_db_tests.rs`, included from
kv_store.rs like `kv_store_packed_tests.rs`. Tests for criteria 1-5.

### Task 2: routing in RocksKVStore
- `vote_db: Option<Arc<DB>>` field; `with_vote_db(Arc<DB>)`; `open_vote_db(&Path)`.
- `is_vote_key(&[u8])` for the two keys.
- `write`: vote batch -> vote DB (default CF); mixed -> panic; else main DB.
- `get` / `RocksSnapshot::get`: vote key -> vote DB, else main-DB fallback.
- `clear`: both DBs.

### Task 3: wire torus-node
`main.rs`: open the vote DB once after `StateDb::open` at
`cli.data_dir.join("vote_state")`, attach it to `init_kv` and `kv_store`.
torus-unwedge / torus-wedge-inject stay on the main DB: recovery never reads or
writes the vote keys (recovery.rs test at 644-765 asserts they are preserved).

### Task 4: prove
`cargo test -p torus-consensus`, `-p torus-node`, `-p hotstuff_rs --no-fail-fast`
(known flakes: justify_block_livelock_test, progress_and_validator_set_update_test).

### Task 5: bench A/B
Own target dir `~/.cargo-target-s67-vote-db`. Arms: ctl (main 42cde1b), votedb,
pipelined (main + `TORUS_ROCKSDB_PIPELINED_WRITE=1`). n >= 4, warm-up cell first.

## Rollback
Revert the branch. Downgrade caveat: an older binary reads the stale main-DB vote
state; delete nothing, but do not downgrade a node mid-view after a crash.
