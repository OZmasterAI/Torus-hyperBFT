# Implementation Plan: serve block-data requests off the consensus thread (s72 option 3)

## Design Decision

Option A of `docs/plans/s72-body-serve-thread.md`: `TORUS_BODY_SERVE_THREAD=1`
(default OFF) routes `BlockDataRequest`s from the poller to a new
`BlockDataServer` thread that serves from a `BlockTreeCamera` snapshot and
forwards misses to the algo thread.

## Success Criteria

1. Server, block in the tree: sends `BlockDataResponse { view: req.view, block }`
   to the requester; nothing forwarded.
2. Server, block not in the tree: forwards the unchanged request into the
   progress channel; sends nothing.
3. Server, wrong chain ID: drops it (no send, no forward).
4. Server thread: serves a request sent on its channel and exits on shutdown.
5. Poller, flag ON: `BlockDataRequest` goes to the block-data channel, other
   progress messages (header) to the progress channel. Flag OFF: both to the
   progress channel (today).
6. Existing suites green (`hotstuff_rs` sequential, `torus-consensus`).

## Tasks

### Task 1: failing tests
- `hotstuff/block_data_server.rs` test module (criteria 1-4, shared in-memory
  KV so the camera sees tree inserts); `networking/receiving.rs` poller routing
  tests (criterion 5) with a queue-backed test network.
- Verify: `cargo test -p hotstuff_rs --lib block_data_server poller_routes`
  fails to compile (server type, new `start_polling` arity missing).

### Task 2: server + poller routing
- `BlockDataServer { chain_id, camera, requests, fallback, sender, shutdown }`
  with `handle(origin, req) -> Served` and `start()`; `body_serve_thread_from_env`.
- `start_polling(network, shutdown, route_block_data)` returns the block-data
  receiver and a progress-channel sender for the fallback.
- Verify: the Task 1 tests pass.

### Task 3: replica wiring
- `replica.rs`: read the flag, pass it to `start_polling`, spawn the server
  when on; shut it down and join before the poller.
- Verify: `cargo test -p hotstuff_rs -- --test-threads=1`, `cargo test -p torus-consensus`.

## Verification (end-to-end)

3-arm A/B on one binary: off / serve / serve+D (see design). All cells
ACCEPT/AGREE/PASS; leader serve queue and next-leader wait from the s72 join.

## Rollback

Unset `TORUS_BODY_SERVE_THREAD` (default OFF = today's routing).
