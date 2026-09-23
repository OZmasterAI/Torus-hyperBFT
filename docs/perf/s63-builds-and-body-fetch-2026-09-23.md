# s63: four-build comparison, body-fetch exhaustion and fix

Date: 2026-09-23. All cells: 3-validator local devnet, cap 200, 300 s load,
rate 76,000, 10 markets, `TORUS_BODY_FETCH_TRACE=1`, generator
`bench-throughput` sha256 `ce06befb…` (frozen s58 generator, source `4076355`),
harness `tools/matched-bench` from `wt/matched-200k-next` (identical to
`62ba68f`). Frozen node binaries and manifests:
`~/bench-results-matched/s63-4build-20260923/artifacts/<build>/`.

## Builds

| Label | Commit | Node sha256 | Notes |
| --- | --- | --- | --- |
| `ctl9af` | `9af0eea` | `49282cf7…` | s60 accepted control |
| `hash388` | `388f7cd` | `d6dfe515…` | s60 hash-cache record (51.6k at 120 s, n=1) |
| `base080` | `080c4fa` | `4c325452…` | s61 `base_runtime` |
| `s61d733` | `d73320e` | `fdc2fb56…` | s61 load improvements (31 commits) |
| `fix63` | `ac8782c` | `eea6a306…` | s61 + this document's consensus fix |

`9af0eea` and `388f7cd` lack `09f3449` (bind header votes to their proposal
view) and `b60097a` (future-message buffer release); `080c4fa`, `d73320e` and
`ac8782c` have both.

## Four-build campaign (03:53–11:56, interleaved, every cell kept)

| Build | Accepted | Live-cell matched/s | Engine ms / 1k fills |
| --- | --- | --- | ---: |
| `d73320e` | 0/3 | 42,986 · 39,393 · 41,182 | 10.6–13.4 |
| `080c4fa` | 1/3 | 37,084 · 38,452, plus a 4-minute stall (3,800) | 16.0–18.7 |
| `388f7cd` | 1/2 | 38,715 · 36,205 | 16.5–17.8 |
| `9af0eea` | 0/2 | 27,433, plus a safety fail-stop (478) | 15.0 |

The last two cells (`9af0eea` r3, `388f7cd` r3) were skipped once the safety
finding below ruled both builds out as a working line. Rejections were almost
all "dissemination failures": a follower's body fetch exhausted its retries
and fell back to block sync. RSS showed no regression (steady 3.9–4.4 GB).

**Safety finding.** In `s63-ctl9af-r1` (`9af0eea`), val2 fail-stopped with
`ConflictingCommittedChain` at height 482: block sync returned a committed
block that conflicted with val2's own committed block. val0/val1 agreed at
height 488. Evidence: `s63-ctl9af-r1/val2.log.gz` near line 205288. The
missing `09f3449` is the likely, unproven, link. Neither `9af0eea` nor
`388f7cd` can become the working line.

**Decision.** `d73320e` became the working line: its live cells beat
`080c4fa` with non-overlapping ranges and it is the only build with lower
engine cost per fill. There was no reason left to port `388f7cd`.

## Why body fetches exhausted

Log analysis of all 10 untraced exhaustions and 3 traced cells:

- The retry budget is 9 attempts 100 ms apart (~1 s). Normal header-to-body
  latency has p99 ≈ 0.9–1.3 s, so the budget sits at the tail.
- In every case the proposer had the body. It reached the requester between
  0.8 s before and 3.45 s after the give-up.
- Lateness came from two sources. The serving node's consensus thread (which
  serves requests) blocked off-CPU for 1–4 s: 0 % CPU and 0 context switches
  in `pidstat`. What it blocks on is unknown. Direct messages were also
  delivered 0.5–3.4 s late, in bursts.
- Retries after the first two go to the other follower, which usually lacks
  the body. The give-up check runs before queued messages are read.
- UDP receive-buffer drops (~70/s, `rmem` 212992) happen in clean and failing
  cells alike and do not coincide with slow legs.
- Stalls escalate through 10 s direct-send timeouts, "Send Queue full" and
  pacemaker backoff (16/32/64 s views).

**Separate bug (MissingData wedge).** `try_insert_body` mapped
`ValidateBlockResponse::MissingData` to the same `false` as `Invalid`. The body
was parked and its trackers were dropped. Its parent was present, so nothing
re-requested it. Its child parked too, and the node waited for the 60 s
block-sync timeout (val2 stalled 54 s in `s63-trace-s61-r1`).

## Fix (`ac8782c`, branch `perf/s63-body-fetch`)

1. **Wall-clock body-fetch budget.** The fast phase is unchanged (9 attempts,
   100 ms apart, 2 at the proposer). A slow phase then retries every 500 ms,
   alternating proposer and other validators. The fetch gives up only after
   9 attempts and 4 s since the first request.
2. **MissingData retry.** Validation is retried every 500 ms. After 3 s the
   node sets `sync_needed` and logs "falling back to sync", which the harness
   still counts as a sync fallback. The retry map is bounded to 64 entries.

Tests: `hotstuff_rs` lib 121/121 (9 new; 6 failed before the fix);
`torus-consensus` lib 159 passed, 1 ignored. All `hotstuff_rs` integration
binaries pass except `progress_and_validator_set_update_test`, which fails
identically on unfixed `62ba68f` (pre-existing). Logs:
`~/bench-results-matched/s63-fix-build/`.

## Fix A/B (13:48–14:50, fix/s61/s61/fix/fix/s61)

| Build | Accepted | Matched/s | Steady RSS |
| --- | --- | --- | --- |
| `ac8782c` | 3/3 | 47,343 · 50,401 · 44,864 | 3.9–4.4 GB |
| `d73320e` | 3/3 | 46,295 · 48,658 · 47,150 | 3.9–4.0 GB |

The fix is cost-neutral. Its reliability benefit is **not proven live**,
because the control stopped failing.

## Host-state regime shift

Nine cells from 03:53 to 11:37 had exhaustions in eight. The ten cells from
11:47 to 14:42 had none: `080c4fa`, three traced `d73320e`, and all six A/B
cells. The same binary also ran about 15 % faster in the afternoon.
`load1`, node CPU and non-node load are indistinguishable between the two
periods, so the trigger is host state the harness does not record.

## Current profile of the working line (A/B cells)

Execution is the bottleneck. The exec thread is 97 % busy and there is no
pipelining. Per native block, chain_ms is 1,070–1,180, broken down as:

| Phase | ms | Share |
| --- | ---: | ---: |
| Engine | ~585 | 54 % |
| Flush | ~256 | 24 % |
| save_books | ~151 | 14 % |
| Verify, replay guard, untimed | ~80 | 8 % |

## Open items

- Record host state (memory, page cache, disk I/O, other processes) in every
  cell so the next bad-regime period can be explained.
- Prove the fix's benefit live, in a failing regime or under controlled
  fault injection.
- Longer cells are disk-limited. `run_cell.py` requires 6 + 0.2 × duration
  GiB free, and RocksDB grows about 0.1 GB/s, so cells over ~500 s cannot run
  with ~113 GB free.
- `progress_and_validator_set_update_test` fails on the working line (it
  stalls after the validator set grows).
