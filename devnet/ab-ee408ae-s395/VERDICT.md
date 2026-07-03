# Decisive A/B: ee408ae vs sprint binary (3989416) — empty cadence

Ran 2026-07-03 (S395 follow-up). Question: do the code changes after ee408ae
(the s367-era "record" binary) own the 15ms→102ms empty-cadence gap on the
4-val devnet?

## Method

- Same box, same compose (`devnet/docker-compose.yml` with the S395 isolation
  fix), same genesis (byte-identical at both commits), 4 validators only.
- Identical dependency versions: current `Cargo.lock` copied into the ee408ae
  worktree, built `--locked` (resolution succeeded ⇒ dep graphs identical).
- Fresh chain per run (`docker compose -p ab … down -v` between runs).
- ABBA order to cancel box drift: s395, ee408ae, ee408ae, s395.
- Per run: settle to height ≥20, then 3×60s windows of `eth_blockNumber`
  deltas. Live testnet node idling in the background on both legs (boundary
  crawl, constant).

## Results (12 windows, `results.csv`)

| leg | windows (ms/blk) | median | mean |
|-----|------------------|--------|------|
| ee408ae | 98.4, 97.5, 114.1, 159.4, 88.8, 100.5 | ~99.5 | ~109.8 |
| s395    | 103.1, 106.7, 102.9, 77.6, 85.8, 87.0 | ~95   | ~93.9 |

## Verdict

1. **No code regression ee408ae→HEAD for empty cadence.** Both binaries sit
   at the same ~100ms floor on identical topology; the sprint binary produced
   the fastest windows of the experiment (77.6–87.0ms).
2. **The remembered "60–85 blk/s empty" (≈12–17ms) does not reproduce** with
   the actual ee408ae binary on this devnet. That figure was per-block
   processing cost / a local-bench artifact, not wall-clock chain cadence —
   consistent with S383's note that it was "NOT the real cadence" figure.
   The ~100ms devnet floor pre-existed the post-ee408ae changes.
3. **New anomaly (follow-up):** idle CPU is bimodal on BOTH binaries — some
   runs idle at load 1.2–3.7, others at 14.8–26.7 with docker stats showing
   100–230% per container. Uncorrelated with binary version AND with cadence
   (the fastest windows happened during a high-load run). Not the old
   idle-spin (present in both legs, incl. post-fix binary). Cause unknown.

## Artifacts

- `results.csv` — label, window, height, blocks, ms/blk, load1m
- ee408ae worktree kept at `~/projects/torus-ab-ee408ae` (binary built)
- Docker images kept: `torus-devnet-node:ab-ee408ae`, `torus-devnet-node:ab-s395`
- Harness: session scratchpad `ab/` (ab-run.sh, ab-cadence.py, image overrides)
