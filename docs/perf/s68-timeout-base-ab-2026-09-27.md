# Timeout base 500 vs 1200 with view-timeout telemetry (s68, 2026-09-27)

Binary `90fcbf2` (main `6087761` + view-timeout telemetry, sha `eadad6c7`).
Campaign `~/bench-results-matched/s68-tbase-20260927`: one warm-up cell, then
ABBA with n=3 per arm. Settings: cap 200, 300 s, rate 76000,
`TORUS_BODY_FETCH_TRACE=1`, and `RUST_LOG=info,libp2p_gossipsub::behaviour=error`
in every arm. `t1200` sets `TIMEOUT_BASE_MS=1200` in genesis; run.log confirms
1200 there and 500 in `base`. All seven cells were accepted, AGREE, PASS.

## Results (LOAD window, per node, averaged over the three nodes)

| cell | matched/s | view ms | arrival | qc_collect | views/committed | committed blocks | timed-out views | entered past deadline | entry slack ms | timeout after ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| base r1 | 62.4k | 569.5 | 288.4 | 303.9 | 1.5 | 410.7 | 135.7 | 3.7 | 1925 | 840 |
| base r2 | 68.0k | 596.2 | 321.8 | 306.3 | 1.6 | 369.3 | 148.7 | 5.3 | 1968 | 781 |
| base r3 | 66.1k | 583.3 | 310.0 | 319.0 | 1.6 | 369.7 | 142.3 | 2.3 | 1756 | 812 |
| t1200 r1 | 67.6k | 736.4 | 393.4 | 333.9 | 1.2 | 348.0 | 19.3 | 0 | 1753 | 1490 |
| t1200 r2 | 73.4k | 687.5 | 351.2 | 328.1 | 1.2 | 377.0 | 20.0 | 0 | 1896 | 1600 |
| t1200 r3 | 68.3k | 675.8 | 342.7 | 320.7 | 1.2 | 378.3 | 21.7 | 0 | 1705 | 1372 |

Timeout classes per node-run (base → t1200):
- after_vote (voted, no QC in time): 71–79 → 10–13
- leader (own proposal not certified): 43–47 → 6–8
- no_proposal: 19–24 → 1–2
- no_vote: 1–2 → 0

Uncertified own proposals: 34–38 → 3–5.

## Reading
- **Dead views fell 85%** (about 140 → 20 per node-run) and views per
  committed block fell from 1.5–1.6 to 1.2. matched/s rose from 65.5k to
  69.8k (+6.5%). t1200 won all three ABBA pairs. That is suggestive, not
  conclusive: a one-sided sign test gives p=0.125, and single cells vary by
  ±10k.
- **Committed blocks did not rise** (base 370–411, t1200 348–378). Surviving
  views get longer instead (view 570–596 → 676–736 ms), as s55 saw. The gain
  is in fuller blocks and less redone work, not in block rate.
- **The explorers' "entered past deadline → instant burn" is refuted.** Only
  2–5 of about 590 views were entered past their deadline. Entry slack is
  already ~1.8–2.0 s because of backoff, and base timeouts fire after
  ~0.8 s, not instantly.
- **At base 500 most timeouts are premature.** The node had already voted,
  or had proposed, and the certificate simply took longer than the deadline.
  With 1200 those views complete.
- **What sets the ~1.2–1.3 blk/s commit rate is per-block latency on the
  follower and dissemination path.** Proposal arrival is ~290–390 ms and
  qc_collect ~300–330 ms, in both arms. Neither leader build (s68 DA-skip
  null) nor timeouts (this A/B) set it.

## Before adopting 1200 as the genesis default
A real crashed leader would cost ~1.2 s+ per view instead of ~0.5 s. A crash
cell (`--crash-at 60`) should confirm that rejoin and liveness stay
acceptable. This is a genesis (consensus) parameter.

## s69 follow-up: crash cells, confirmation pairs, adopted (2026-09-27)

Same binary (`90fcbf2`, sha `eadad6c7`) and settings throughout.

**Crash cells** (`~/bench-results-matched/s69-tcrash-20260927`, `--crash-at 60`,
val1 killed and restarted). Both passed the crash gate: AGREE, replay found,
no fail-stop or holes. Survivor freeze, from the 1 s sampler:

| arm | freeze | val1 ready | survivor views in freeze | matched/s (whole / outside freeze) |
| --- | --- | --- | --- | --- |
| 1200 | 37 s | +28.8 s | 12 | 53.5k / 60.4k |
| 500 | 62 s | +27.6 s | 20 | 54.0k / 67.2k |

The freeze ends at the first survivor view boundary after the restarted node
can vote. The doubling backoff puts those boundaries at 15.5 / 31.5 / 63.5 s
(500) and 18 / 37.2 / 75.6 s (1200). At 500, val1 was up at +28 s but missed
the 31.5 s view, as in s64. So 37 vs 62 s reflects where the ~28 s restart
lands, not a structural gain. If a restart took more than ~33 s, 1200 would
wait until ~76 s. The fix for rejoin is view sync plus a faster RocksDB open,
not the timeout base.

**Confirmation** (`~/bench-results-matched/s69-tbase2-20260927`: warm-up, then
ABBA, run after host services were stopped). All cells ACCEPT/AGREE/PASS.
- base: 66.1k, 62.9k. t1200: 68.0k, 68.2k. Warm-up (500): 64.8k.
- One same-day normal cell before that (`s69-tcrash-ctl-r1`, 500) scored 61.4k.

Pooled with the s68 cells: 500 = 62.4 / 68.0 / 66.1 / 61.4 / 66.1 / 62.9
(mean 64.5k); 1200 = 67.6 / 73.4 / 68.3 / 68.0 / 68.2 (mean 69.1k, +7%).
1200 won all 5 ABBA pairs (one-sided sign test p ≈ 0.03).

**Adopted:** `timeout_base_ms` 1200 in `devnet/genesis.json`,
`devnet/ab-harness/devnet-genesis.json`, `testnet/genesis-weighted-base.json`,
the `ChainConfig` serde default and the node's no-genesis fallback. The
matched bench caches `testnet/genesis-weighted-full.json`; regenerate it
(`FORCE=1`) or delete it, or cells keep running 500. A 500 control arm now
needs `TIMEOUT_BASE_MS=500`.
