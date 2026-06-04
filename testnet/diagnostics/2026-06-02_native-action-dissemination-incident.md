# Native-Action Dissemination Incident — testnet run 2026-06-02 → 2026-06-04

> Diagnostic evidence extracted from `testnet/node.log` (18 GB) before that log was
> deleted to reclaim disk. The raw 18 GB trace is gone; this file + the companion
> `2026-06-02_missing-action-hash-errors.txt` (1,656 raw ERROR lines) are the preserved signal.

## Node under test
- `torus-node version=0.1.0`, started **2026-06-02 11:19:00 UTC**, PID 2557265.
- `data_dir=testnet/data`, `chain_id=7778`, `epoch_length=100` (cap100), 3 validators, archive mode (pruning disabled).
- Peer set: bootstrap `/ip4/84.32.108.220/udp/30333/quic-v1`, `available_servers=3`.
- Ran ~1.75 days; stopped via SIGTERM on 2026-06-04 ~14:49 UTC (manual, this session).
- Original log size: 18,933,201,681 bytes (~18 GB).
- ⚠️ **Binary identity caveat:** version string is `0.1.0` and the process started 2026-06-02 11:19.
  It is **not confirmed** whether the `cap100-3val-perf` T2–T5 dissemination fixes were compiled into
  this binary. This run may *predate* those fixes (i.e. it is the "before" picture) or show them
  insufficient. Confirm against the binary's build before drawing conclusions.

## Headline signals (full-log counts + time bounds)

| Signal (log substring) | Count | First | Last | Shape |
|---|---:|---|---|---|
| `gossip native action rejected: ... nonce too old (>60s)` | **85,753,969** | 2026-06-02T21:11:30Z | 2026-06-04T14:49:23Z | **entire run** |
| `validate_block: REJECTED -- missing native actions after retry` | **5,453,676** | 2026-06-02T21:11:32Z | 2026-06-03T05:35:47Z | first ~8 h, then **stopped** |
| `body validation failed for block` (same events as above) | 5,453,668 | — | — | tracks the REJECTED count |
| `on_committed_block: missing action hash — block will not execute` (ERROR) | **1,656** | 2026-06-02T21:56:30.466Z | 2026-06-02T21:56:32.770Z | single ~2 s sync-replay burst |

Zero panics / FATALs across the whole run.

The 18 GB is dominated by the **85.7 M `nonce too old`** rejections (~360/s sustained for ~42 h).

## Interpretation

This run exhibits a severe **native-action dissemination failure**, which is precisely the
problem domain of the `cap100-3val-perf` branch (T2–T5: bounded `PendingSendQueue`,
enqueue-on-disconnect / flush-on-reconnect, re-push recent native-action bundles, decision-gate telemetry).

1. **`nonce too old (>60s)` — 85.7 M, whole run.** Native actions are gossiped but rejected at
   validation because their nonce has already aged past the 60 s window by the time they are seen.
   Consistent with dissemination latency / backlog or a re-gossip storm: blocks carry
   `native_actions=1000` each, and actions cannot propagate+execute fast enough to stay inside the window.

2. **`missing native actions after retry` — 5.45 M, first ~8 h only.** Blocks failed body validation
   because a large fraction of their native actions were absent even after retry
   (observed `missing_count=832` on `height=153681`, i.e. 832/1000 actions missing). This ran from
   2026-06-02 21:11 until **2026-06-03 05:35**, then stopped — something changed around then
   (network stabilized, peer caught up, or production characteristics shifted). Worth root-causing.

3. **The 1,656-error burst (acute episode within the above).** A block-sync catch-up replay that
   committed headers/QCs faster than action bundles were available — see next section.

## The 1,656 `missing action hash` ERROR burst (2026-06-02 21:56:30–32)

A single block-sync catch-up replay episode:

- **Trigger** — `block_sync` timeout at 21:54:32, `committed_height=152012`, `available_servers=3`:
  ```
  21:54:32.953  INFO block_sync: timeout trigger fired, available_servers=3, committed_height=Some(152012)
  21:54:32.954  INFO block_sync: starting sync with peer, our committed_height=Some(152012)
  ```
  1,171 × `block_sync: worker returned 128 blocks`; 3 sync sessions (21:54:32, 21:57:32, 21:58:32).
- **Burst** — heights **152014 → 153670** (1,656 contiguous blocks) committed in ~2 s without their
  action bundles, each logging:
  ```
  ERROR torus_consensus::app: on_committed_block: missing action hash — block will not execute height=<H>
  ```
- **Recovery** — immediately after height 153670 (21:56:32.770), execution resumed normally:
  ```
  21:56:32.770  INFO on_committed_block: sending to execution pipeline height=153671 evm_txs=0 native=1000
  ...
  21:56:32.995  INFO produce_block called (CTE) parent_height=153679 local_height=153678
  ```
  By 153679 the node was producing its own blocks again. **Open question:** whether the synced blocks
  **152014–153670 ever executed** (they were committed but "will not execute") or remain a state gap
  back-filled later. The companion `.txt` lists every affected height.

## Verbatim samples
```
WARN torus_node: gossip native action rejected: native action validation failed: nonce too old (>60s)
WARN torus_consensus::app: validate_block: REJECTED -- missing native actions after retry missing_count=832 height=153681
WARN hotstuff_rs::hotstuff::implementation: body validation failed for block hash=[68, 80, 60, 147, ...]
ERROR torus_consensus::app: on_committed_block: missing action hash — block will not execute height=152014
INFO  hotstuff_rs::block_sync::client: block_sync: timeout trigger fired, available_servers=3, committed_height=Some(BlockHeight(152012))
```

## Follow-ups worth opening
- [ ] Confirm whether this binary included T2–T5 (before/after the fixes).
- [ ] Root-cause the 60 s nonce-staleness: dissemination latency vs. re-gossip storm vs. nonce tracking.
- [ ] Why did `missing native actions after retry` stop at 2026-06-03 05:35? (recovery signal)
- [ ] Did committed-but-unexecuted heights 152014–153670 get back-filled, or is there a state gap?
- [ ] Re-run with the same load and compare these counts as a regression/perf metric.
