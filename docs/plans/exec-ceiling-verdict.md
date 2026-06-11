# Exec-Ceiling Verdict — s351 bs500 Solo Probe (2026-06-11)

Instrumented seed (commits e85ff75 + dfc8399), exact-PID bounce, 30s idle
baseline, bs500 solo (10 senders, sb10, 30s). Snapshots in
`testnet/sweep-s351/`, table via `devnet/scripts/exec_phase_table.py`.

## Phase decomposition (per native block; 6 native blocks, ~75 actions each)

| phase        | ms/native-blk | share of exec_block |
|--------------|--------------:|--------------------:|
| engine       |        529.5  |               38.7% |
| verify       |        429.1  |               31.3% |
| flush        |         16.7  |                1.2% |
| replay_guard |          6.6  |                0.5% |
| save_books   |          0.07 |                0.0% |
| residual     | (372 empty blocks ≈ 6ms ea) | 28.3% |

`exec_queue_depth`: **0 at every 3s sample during load.** 448 actions
processed; `block_build_seconds` now observing (188 obs, 221ms avg,
leader-only). Chain cadence healthy after probe (95486→95545 in 12s).

## Verdict

**Backpressure theory REFUTED.** The execution pipeline never queued a
single block — it is idle ~90% of wall time at current ingress. A loaded
native block costs ~1s (engine + verify ≈ 70%), but loaded blocks arrive
~10s apart because the mempool is starved upstream.

**The real ceiling is RPC ingress verify** (matches the sprint-5 A/B
trigger): per 10-action batch, verify wall 1.47s (0.70s CPU +
0.77s blocking-pool queue) + admit 0.42s ⇒ ~1.9s ack round-trip ⇒
10 serial senders cap at ~52/s theoretical, 39/s observed; bench drop
65.5%. Zero labeled admit-rejects ⇒ the node never sheds — clients stall.

## Decision

- Option B (book persistence): **dead** — save_books is 0.0%.
- Option C (3-stage exec pipeline): **dead** — queue depth never left 0.
- Engine optimization: not the limiter at current ingress; revisit only
  after ingress is fixed and the exec queue actually builds.
- **Next move: ingress verify path.** Candidates: parallelize within-batch
  verify inside the spawn_blocking closure (rayon), widen/dedicate the
  verify pool (0.77s/batch lost to pool queueing), and eliminate the exec
  thread's double-verification via an ingress-verified hash trust-cache
  (would also cut the 429ms exec verify phase).
