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
