# S405 Mesh-Watchdog Devnet A/B — Task 5 Summary

**Date:** 2026-07-05 | **Procedure:** 10× `docker restart -t0 validator-1`
(the in-place fast-restart footgun) on a fresh 4-validator devnet, observer =
validator-0. Identical procedure both runs.

## Results

| | fixed (`576d3be`, watchdog + explicit peers) | base (`1e0b916`, pre-watchdog) |
|---|---|---|
| wedges induced | **0 / 10** | **8 / 10** |
| healed | n/a (never wedged) | **0 / 8** |
| post-restart views/block | back to ~1.0–1.2 within 25s | **~2.0, degraded continuously 15:57→16:23 (26 min), never recovered** |
| subscription state | `subscribed_validators=3` after every restart | metric unavailable (pre-watchdog binary) |

Logs: `run-20260705T145908.log` (fixed), `run-20260705T155051.log` (base).

## Reading

- The base build reproduces the S395 failure exactly: one fast in-place
  restart wedges the mesh into a persistent ~2.0 views/block degraded mode
  that never self-heals; subsequent restarts don't clear it.
- The fixed build never entered the state once in 10 attempts — subscription
  exchange completed cleanly within 25s every time.
- Honest attribution: `torus_mesh_watchdog_disconnects` stayed 0 in the fixed
  run — **prevention** came from Task 4 (explicit peering: immediate redial +
  fresh subscription exchange, validators out of mesh entirely). The watchdog
  (Tasks 1–3) is the version-neutral **backstop** for states explicit peering
  can't prevent (and for mixed fleets where explicit peering isn't reciprocal
  yet). Both mechanisms observed working: explicit-peer path carried consensus
  with `consensus_mesh_peers=0` for the whole fixed run.

## Deployment guidance

- Watchdog (Tasks 1–3): version-neutral, safe to deploy to live seed+val1 now.
- Explicit peers (Task 4): reciprocal-only — deploy to live **only after val3
  upgrades** (a GRAFT from an old build is answered with PRUNE, leaving that
  peer fanout-only toward us).
