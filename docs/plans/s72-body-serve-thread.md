# Design: serve block-data requests off the consensus thread (s72 option 3)

## Problem

A `BlockDataRequest` (a peer fetching a block body by hash) is served by
`on_receive_block_data_request` on the hotstuff algorithm thread. That thread
handles one progress message per loop pass, and it can be busy for 100 ms to
1+ s (validate_block, the app commit feed, a blocking exec dispatch). Requests
wait behind it:

- s72-dpf, flag off: leader serve queue (request admitted -> served) p50 1-2,
  mean 12-14, p90 20-32 ms.
- With fix D the feed runs after the proposal, exactly when followers request
  the new body: serve queue p50 21-39, mean 90-114 ms, and 1.35 s stalls that
  exhausted justify-fetch retries (dissemination gate REJECT in 4/4 cells).

## Context

- The block-sync server already serves from its own thread: a
  `BlockTreeCamera` snapshot per request, replies through its own
  `SenderHandle` (`block_sync/server.rs:86-150`). Production KV is RocksDB
  (`RocksKVStore`, real snapshots), safe to read concurrently.
- `Network` is `Clone + Send`; `LibP2PNetwork::send` only enqueues a command
  for the swarm task, so any thread can reply.
- The poller (`networking/receiving.rs:32`) splits traffic into progress /
  sync-request / sync-response channels; block-data requests ride the progress
  channel with votes and headers.
- The leader inserts its block into the tree before the header broadcast
  (`implementation.rs:464`, `:679` -> broadcast at `:764`), so a snapshot has
  it. Only two rare paths leave a body solely in the in-memory
  `pending_bodies`: the recovery repropose (`:1751`) and the NEC fresh
  proposal (`:1884-1910`).
- A dedicated network-thread protocol (`/torus/block-data/1.0`, May 2026) was
  abandoned for the ordinary HotStuffMessage path (S223); its swarm-side
  `block_store` is still filled and never pruned. Not touched here.

## Options

### Option A: block-data server thread fed by the poller (recommended)

Behind `TORUS_BODY_SERVE_THREAD=1` (default OFF). The poller routes
`HotStuffMessage::BlockDataRequest` to a new channel instead of the progress
channel. A new `BlockDataServer` thread blocks on that channel; per request it
checks the chain ID, reads the block from a fresh `BlockTreeCamera` snapshot
and sends `BlockDataResponse { view: req.view, block }` to the requester. On a
miss it forwards the request unchanged into the progress channel, so the algo
thread serves it exactly as today (covers `pending_bodies` and keeps today's
`found=false` handling). It logs the same `body_fetch_diag serve:` trace line
(with `local_view=-` and `via=server`) so the s72 joins keep working. If the
server thread is gone, the poller falls back to the progress channel instead
of dropping the request (review M1).

- Files: `networking/receiving.rs` (route + extra receiver/sender),
  new `hotstuff/block_data_server.rs`, `replica.rs` (spawn, shutdown order),
  `hotstuff/mod.rs`.
- Trade-offs: mirrors a proven production pattern; the algo path is unchanged
  for misses and when the flag is off. One RocksDB snapshot + multi-get per
  request (the sync server does the same).
- Effort: Medium. Risk: Low-Medium (new thread; serving is read-only).

### Option B: serve on the swarm task from a shared in-memory store

Revive the swarm-side `block_store` path: the leader stores its body for
serving and the swarm answers requests itself.

- Trade-offs: no extra thread hop, but it reopens the abandoned protocol, the
  store is unbounded today, and bodies are copied into a second store.
- Effort: Medium-Large. Risk: Medium.

### Option C: answer block-data requests first in the algo loop

Drain all queued block-data requests at the top of each loop pass.

- Trade-offs: trivial, but a busy algo thread (the actual problem) still
  blocks serving.
- Effort: Small. Risk: Low. Does not fix the stall.

## Recommendation

Option A. A/B on one binary (branch includes fix D, default off), ABBA-style
rotation, s70 settings:

| arm | env |
|---|---|
| off | `TORUS_BODY_SERVE_THREAD=0 TORUS_DEFER_PARENT_FEED=0` |
| serve | `TORUS_BODY_SERVE_THREAD=1 TORUS_DEFER_PARENT_FEED=0` |
| serve+D | `TORUS_BODY_SERVE_THREAD=1 TORUS_DEFER_PARENT_FEED=1` |

Gate: every cell ACCEPT/AGREE/PASS. Measure the leader serve queue, the
next leader's StartView -> produce_block, matched/s, timeouts.

## Not Building (YAGNI)

- No pruning of the legacy swarm `block_store` / `pending_bodies` growth
  (pre-existing, separate issue).
- No serving of `pending_bodies` from the new thread: misses fall back to the
  algo path.
- No new metric: the trace line and the join measure it.

## Open Questions

- Snapshot cost per request under load (bodies are ~4 MB); the sync server
  pays the same. The A/B will show whether serve latency drops.
