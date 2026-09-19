# Rejected deep cancellation compaction experiment

Based on diagnostic `cad99c7`, exact default-off flag `TORUS_CANCEL_COMPACT_DEEP=1`
lowers eligibility to eight targets and the movement threshold to one queue
length only at depth >=32768. The existing stable compaction/removal paths
preserve cancellation output and bookkeeping. The legacy above-limit path keeps
its existing policy.

Receipt `2997f617-0c54-4bdf-ba7c-c34ed90cd99c`: 127 core tests passed (8 ignored),
including explicit current/deep/naive comparisons of orders, journals,
commitments and following append; 10 margin integration tests passed with the
flag enabled. No release node was built for this rejected experiment.

Isolated release microbenchmark `deep-compaction-micro-r1` finished in 267.74s.
Frozen test binary SHA256:
`23204e953081c32478fe407f6ad27b031400d9c0339c2e69245e04e46e49fc83`.
Two independent fixture hash seeds and all eight construction/preparation/
execution orders produce 16 pairs per comparison, with AA/BB controls. Setup,
persistence preparation, result checks and destruction are outside timing.

| Depth | Targets/level | Layout | Current/deep paired ratio | AA | BB |
| --- | --- | --- | --- | --- | --- |
| 8192 | 16 | spread | 1.007 | 1.027 | 0.963 |
| 32768 | 8 | spread | 0.728 | 0.984 | 0.989 |
| 32768 | 16 | spread | 0.908 | 1.047 | 1.035 |
| 32768 | 32 | spread | 0.948 | 0.962 | 1.012 |
| 32768 | 8 | front | 2.130 | 1.025 | 1.010 |
| 32768 | 8 | back | 2.111 | 1.140 | 0.947 |

Ratio >1 favors the candidate. All shapes use five price levels and chunked
commitments. The deep eight-target dispersed case clearly regresses; the dispersed
16/32-target cases do not establish a benefit. Endpoint eight-target cases
improve about 2.1x, although short timings show visible order/control noise.
Reject this combined policy; keep it
isolated for provenance, default OFF, with no chain throughput claim.
Raw logs, strata, source/binary hashes and analysis remain under
`/home/18c/bench-results-matched/s60-campaign-20260918/deep-compaction-micro-r1/`.


A second experiment separates eligibility from movement policy: keep
budget four for every depth and only lower eligibility to eight targets at
32768 depth. This keeps the endpoint grouping opportunity while testing whether
the dispersed regression came from aggressive compaction. Qualification passed 127 core tests under `a960e68c`, then 10 enabled margin
and 27 native integration tests plus a separate release node build under
`187c5329-0541-4cbd-bbaf-253c2aee6927`. Fresh balanced micro-r2 ratios for
dispersed 8/16/32 targets are 0.999/1.000/0.973; endpoints 1.798/1.597 with
noisy controls (back AA 0.782). This supports experimental live screening only,
not a throughput claim. Default remains OFF.
