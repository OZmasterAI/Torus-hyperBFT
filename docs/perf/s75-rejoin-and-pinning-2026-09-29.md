# s75: rejoin freeze fixes A–E and the CPU pinning A/B

## Summary

- **Rejoin freeze.** A crashed validator that restarts now resumes voting
  0.3–0.7 s after it is back on the network (rejoins 2–5; 3.6 s in rejoin 1).
  Main waited 24–36 s in the same place. The freeze that remains is the
  restart itself: DB open plus the replay of committed-but-unexecuted blocks
  before networking starts (11–55 s per rejoin).
- **Five fixes, merged into local main `e12bdac`:** B round-skip, C parked
  header re-check, A leader skip on NewViews + boot `enter_view`, D stuck
  leader re-sends its header directly, E 1 MiB progress-message buffer.
- **CPU pinning (item 5): not adopted.** Pinning each node to its own cores
  did not reduce cell-to-cell spread and cost 5.7% matched/s.
- **Multi-crash harness.** One 660 s cell kills and restarts val1 five times
  (`CRASH_KILL_AT_S=60,180,300,420,540`), with per-kill records and
  `crash-freeze.py` for the freeze of each rejoin.

## Rejoin patterns

n = 3, so every vote is needed: while the restarted node cannot vote, no
QC or TC forms and the survivors keep timing out with backoff (1.2 s
doubling to a 38.4 s cap). Each long freeze is one backed-off view that the
rejoiner cannot end.

| Pattern | What happens | Fix |
|---|---|---|
| A | The rejoiner restarts below the survivors' view v and is leader(v): nobody proposes in v. | NewView-driven leader skip (> f power) + `enter_view` at boot |
| B | A future-view header arrives while the rejoiner is behind: no vote. | Round-skip to the header's view (s74) |
| C | The current header's justify block is unknown: the header is dropped and never re-evaluated. | Park it; re-dispatch when the block is inserted |
| D | The survivors' header was gossiped while the rejoiner was down. Gossip is not queued for a disconnected peer, so it never arrives. | Leader still in its own view 3 s after proposing re-sends the header by direct send (queued per peer, flushed on reconnect), every 3 s |
| E | The survivors' votes for the view the rejoiner collects arrive while it is behind, go into the future-view buffer and are evicted. The node configured that buffer as 1024 **bytes** (since the workspace scaffold): one header or three votes. | `PROGRESS_MSG_BUFFER_BYTES` = 1 MiB (a full 256-message reconnect flush from 3 peers needs 552,960 B) |

Kill switches: `TORUS_ROUND_SKIP=0` (B and A), `TORUS_HEADER_RESEND_MS=0` (D).

## Crash A/B (s75-mc-20260928)

Same schedule in every crash cell: 660 s, val1 SIGKILLed at +60/180/300/420/540 s.
Freeze = longest gap between val0 commits after each kill (`crash-freeze.py`).

| Rejoin | main 097912e | B+C | A+B+C+D | A+B+C+D+E |
|---|---|---|---|---|
| 1 | 18.2 | 18.1 | 22.8 | 24.7 |
| 2 | 74.6 | 74.3 | 74.6 | 63.1 |
| 3 | 36.0 | 74.6 | 58.9 | 53.5 |
| 4 | 76.3 | 73.5 | 55.2 | 46.9 |
| 5 | 75.3 | 74.9 | 38.9 | 60.5 |
| crash-cell matched/s | 52.9k | 51.5k | 56.8k | 60.0k |

A+B+C+D+E, split per rejoin:

| Rejoin | restart → ready | of which replay | ready → chain commit |
|---|---|---|---|
| 1 | 19.9 s | 11.6 s | 3.6 s |
| 2 | 60.3 s | 55.2 s | 0.7 s |
| 3 | 52.0 s | 44.9 s | 0.4 s |
| 4 | 44.1 s | 33.5 s | 0.6 s |
| 5 | 58.0 s | 49.4 s | 0.3 s |

- All cells AGREE; every restart had 0 panics and 0 holes.
- A+B+C+D: rejoin 2 still froze 74.6 s. The rejoiner voted in the survivors'
  view 0.6 s after coming up, but their votes for that view had been evicted
  from the 1024-byte buffer (pattern E). The E run fixed it.
- Why ~74 s on main: the rejoiner's execution backlog (30–44 blocks after the
  first rejoin; it did not drain within 2 minutes under load) is replayed
  before networking, so the survivors climb to the 19.2 s + 38.4 s views
  before it is back. The rejoins in one cell are therefore not independent.
- **Confound:** the fixed-arm binaries were branched before the parallel
  book load (d6528ec) was merged; main 097912e had it. Their restarts were
  about 1.4 s slower to get ready, which works against the fixes on total
  freeze and does not affect the ready → commit column.
- The fixes also fire in healthy cells (warm cells: 8–10 leader skips,
  12–20 round-skips), with no visible cost.

## Verification

- TDD for every fix (tests seen failing first); reviews found no safety
  issue. Review fixes: C keeps the receive wait at 10 ms while a header is
  parked and discards it once voted; A skips to the next view only while no
  proposal was seen and ignores views past the 16-view cap; D does not reset
  a running body fetch on a repeated header.
- Stateright models: round-skip (s74) and leader sync
  (`tests/stateright_rejoin_leader_sync.rs`): one proposal per leader per
  view across crash/restart; the `propose_before_persist` mutation is caught;
  exhaustive run 14.3M states in 127 s.
- Workspace on merged main (own target dir): 1,900 passed, 2 failed,
  30 ignored. Both failures are in torus-core, which the merge did not
  change (see Open items).

## CPU pinning A/B (s75-pin-20260929)

Local main e12bdac, same binary both arms, 300 s cells, no crash, order
pin/unpin/unpin/pin/pin/unpin/unpin/pin. Pinned arm: `NODE_CPUS=0-5/6-10/11-15`
(val0 serves the bench RPC), `BENCH_CPUS=16-17`, from whole-cell averages of
4.2–5.1 cores per node and ~1.3 for the client.

| Arm | matched/s per cell | mean | sd / CV | blk/s | chain_ms | timeouts |
|---|---|---|---|---|---|---|
| unpinned | 70.5k 73.0k 74.0k 78.7k | 74.0k | 3.4k / 4.6% | 1.45 | 722 | 30 |
| pinned | 70.3k 66.8k 69.2k 73.0k | 69.8k | 2.6k / 3.7% | 1.60 | 645 | 32 |

- Spread: not meaningfully lower with 4 cells per arm.
- Throughput: pinned −5.7%, unpinned won all 4 pairs (one by 0.2k), Welch
  t ≈ 1.96.
- Pinned nodes made more blocks with a shorter chain but matched less:
  likely execution/matching capped by the 5–6 cores (an inference, not
  measured).
- The unpinned cells rose through the night (70.5k → 78.7k in run order).
- For throughput A/Bs: cell-to-cell variation is about 4%, so a ~5% effect
  needs 4+ cells per arm in alternating order.
- The harness change (`NODE_CPUS`, `BENCH_CPUS`) stays on branch
  `perf/s75-cpu-pinning`, unmerged.

## Open items

- Startup: start networking before the replay (or replay in the background).
  This is now most of each crash freeze. Parked in favour of throughput.
- Power loss: `highest_view_entered` is not fsync'd before proposing, so after
  a power loss (not a SIGKILL) a leader could re-propose in a view. Same class
  as the vote state; fixing it needs an fsync on the propose path (measure).
- The TC RECOVER path can propose twice in one view (pre-existing, found in
  the D review).
- Flaky `progress_and_validator_set_update_test` (fails on main too: the
  steady-state PC `debug_assert`, or a hang).
- torus-core `cancel_all_many_falls_back_on_stale_or_shared_indexes` and
  `id_lookup_agrees_with_linear_scan_under_mutation` fail deterministically
  on sequence-number `debug_assert`s; the earlier "0 failed" was measured in
  the shared cargo target dir.
- Build hygiene: worktrees share `~/.cargo-target`, which ran a stale binary
  in this session. Use a per-worktree `CARGO_TARGET_DIR`.
