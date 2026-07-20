# Body-push v2 A/B on 18c @ 2a0d207 (2026-07-21)

Verification of the header+pushed-body redesign (v2 of the insert_persist fix;
v1 inline-body post-mortem in docs/l3-persist-attribution.md §6). Same 4-cell
shape as the v1 A/B: idle off/on + cap-400 off/on, TORUS_BODY_PUSH_MAX_BYTES=65536
on all validators in ON cells, bench-standard env (mode2+cap8+bhash1+churny16).
Raw: ~/bench-results-18c/cells-bodypush/ on 18c (EXIT=0, env+sha verified per cell).

## Results

| Cell | blk/s avg | worst-60s | matched/s | views (n) | view ms | insert_persist | arrival |
|---|--:|--:|--:|--:|--:|--:|--:|
| BPI-off (idle) | 27.0 | — | — | 4,740 | 45.8 | 10.2 | 9.0* |
| BPI-on (idle) | **29.0** | — | — | 5,562 | **39.1** | **4.9** | 3.1* |
| BP400-off | 8.85 | 4.30 | 2,718 | 4,374 | 113.6 | 35.2 | 58.7 |
| BP400-on | 8.07 | 4.15 | 2,512 | 4,140 | 115.6 | **16.2** | **21.3** |

\* off/on arrival from the prior campaign's idle cells; consistent here.

## Verdict

1. **v2 is SAFE — v1's collapse signature is gone.** Views are MORE numerous and
   faster with push on at idle; the loaded ON cell is healthy (v1: 0 matched,
   124s blocks). The view-gated-delivery and vote-before-DA hazards are avoided
   by construction (header untouched; body rides the view-exempt BlockData path).
2. **Idle: real win.** +7% idle cadence (27.0→29.0), view 45.8→39.1ms,
   insert_persist halved. Banked.
3. **Loaded: neutral — and the reason is the finding of the campaign.**
   Every pipeline phase the push touches improved dramatically (arrival 58.7→21.3,
   persist 35.2→16.2, build 22.2→7.6) yet total view time was IDENTICAL
   (113.6 vs 115.6ms): qc_collect absorbed the entire saving (72.5→138.0ms).
   **View time under load is WORK-CONSERVED**: replicas are CPU-saturated
   (exec_queue pegged); votes arrive when replicas free up, not when the pipeline
   delivers. Shaving pipeline latency just makes the leader wait earlier.
4. **Consequence for the gate (21.0 worst-60s):** pipeline-latency levers are
   exhausted under load. The remaining levers are (a) cut total per-view WORK
   ~2.4× — engine/verify/flush on the replica critical path — or (b) deepen
   exec/consensus OVERLAP so replica CPU time stops serializing views
   (engine parallelism across markets, parallel signature verify, exec offload
   from the vote path). That is the Layer-3 compute round proper, as scoped in
   the mission handoff — now with measured arithmetic behind it.

## Recommendation

- Adopt TORUS_BODY_PUSH_MAX_BYTES=65536 in the bench-standard env (idle/light-load
  win, zero loaded cost, exact-today default off in-tree).
- Close the consensus-RTT workstream: attribution fully validated, both fix
  vectors measured, work-conservation law established. Open the engine/verify
  parallelism round next — target: replica per-view work 113ms → ≤48ms.

## Provenance

Build 2a0d207 = f9c91ed + 6bee671 (v1 inline branch REVERTED) + 53b233f (v2
body-push: unsolicited BlockDataResponse into pending-header machinery, bounded
16×64KiB parking FIFO, first-copy-wins, hard cap 65536 at parse AND decision)
+ 2a0d207 (dedup scope corrected: BlockData messages exempted — extends the
4f832e5 fix; regression test now 5 cases). Full suite green on 18c incl. both
stateright models, wedge repro 2/2, and both known-flaky tests passing first try.
