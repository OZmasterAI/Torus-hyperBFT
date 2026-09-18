# Strict live replay qualification

Harness based on f4c2d12, committed as 441e967 with genesis fix f99e15c.
Offline harness/health/summarizer tests passed, as did three genesis fixtures.
This changes only the benchmark harness; the measured node remains frozen 9af0eea.

`CRASH_REQUIRE_REPLAY=1` requires a positive restart replay log gap with
consistent committed/applied heights. With `TORUS_EXEC_PIPELINE=1`, the worker
must attach after replay at or above that committed tip. Existing all-three
block hash, header root, quiescent state digest, panic/hole, survivor counter,
and bounded rewind checks still apply. Default `0` preserves zero-rewind
legacy scoring when the pre-kill metrics are valid.

A failed HTTP request, five-second timeout, missing, duplicate, non-finite,
negative or fractional pre-kill metric is recorded as null and UNKNOWN, never
zero. Keep `crash-pre-kill-metrics.txt`, its `.stderr`, and
`crash-pre-kill.json` alongside the existing crash and replay artifacts.
An independent known failure takes precedence over UNKNOWN. Crash UNKNOWN
keeps overall acceptance UNVERIFIED. A crash PASS cannot waive the killed
node's counter-reset UNKNOWN in throughput liveness.

After the root coordinator schedules exclusive use of the devnet ports and
frozen binary, a short qualification attempt is:

```bash
CRASH_KILL_AT_S=25 CRASH_REQUIRE_REPLAY=1 KILL_NODE=val1 \
TARGET_DIR=/home/18c/bench-results-matched/s60-campaign-20260918/artifacts/recovery \
DATA_ROOT=/home/18c/torus-pipeline-qualification \
RESULTS_ROOT=/home/18c/bench-results-matched/s60-campaign-20260918 \
BLOCK_CAP=200 SENDERS=5000 CONC=256 BATCH=400 SUBMIT=1 \
tools/matched-bench/run-cell.sh \
  /home/18c/projects/wt/matched-crash-replay \
  pipeline-replay-60s-r1 10 60 76000 'TORUS_EXEC_PIPELINE=1'
```

The runner may return nonzero because a restart makes throughput liveness
UNKNOWN. Inspect the distinct `summary.json.crash` verdict and reasons;
never count this as an accepted throughput cell. Only strict crash PASS
qualifies observed replay. Record binary hashes, source revision, environment,
pre-crash worker activity, positive replay log and all-three agreement.
Zero/no replay fails strict qualification; retain that unsuccessful attempt.

This is timing-dependent qualification, not deterministic C1 coverage. The
scrape and SIGKILL are not atomic. No pre-kill committed-versus-applied gap wait
is implemented: `torus_block_height` advances after the execution handoff,
before worker durability; `torus_blocks_committed_total` is a process-lifetime
counter, not a durable height. Subtracting them cannot establish the marker
fence. A future bounded wait requires explicit durable applied and persisted
committed height observations. Even those observations would not prove E ran
N+1 while W held N without additional targeted evidence.

Parent-scheduled offline verification (no Cargo or live node required):

```bash
python3 tools/matched-bench/test_harness.py
python3 tools/matched-bench/test_health.py
python3 tools/matched-bench/test_summarize.py
bash -n tools/matched-bench/crash-kill.sh tools/matched-bench/run-cell.sh
```

Coverage includes strict zero/missing/positive/inconsistent replay evidence,
worker attachment below the replay tip, missing and failed metric evidence,
stubbed curl timeout after partial output, failure precedence, and throughput
liveness remaining UNKNOWN across a restart despite a crash PASS. No runtime
fault hook, storage mutation, or node behavior change is included.

## Observed short qualification

`s60-pipeline-replay45-r3` (nominal45s, actual58s, killval1+15s, pipeline1)
replayed from applied783 to committed784, gap1, and attached its worker at784.
The separate strict crash gate passed and all validators agreed at height790.
The full run was REJECT: commit progress stopped with pending actions and
empty execution/flush queues; drain timed out after208s. No panic/fail-stop
was observed. Thus this is positive replay evidence, not healthy post-restart
liveness. Preserve the failed cell; pipeline remains defaultOFF.

Artifacts: `/home/18c/bench-results-matched/s60-pipeline-replay45-r3/`; frozen
binary/config provenance and rawlogs under the campaign root. The two earlier
45s labels never launched validators (invalidkilloffset, then inheritedgenesis
OUT). Neither is a replay attempt. See [deterministic C1 design](c1-pending-parent-qualification-design-2026-09-18.md)
for the stronger pending-parent test still needed.

The follow-up `s60-viewbound-replay45-r1` used frozen runtime `b60097a`
(header-view voting and future-buffer accounting fixes), with extra wedge
diagnostics. It completed drain in57s and final AGREE with clean dissemination,
but had no replay gap: worker attachment586, pre-kill execution/flush queues0.
Strict crash verdict FAIL, overall REJECT, liveness UNKNOWN after counter reset.
This is healthy restart/drain evidence for one cell, not positive replay or a
throughput result. It does not prove what caused the earlier stalled cell.
