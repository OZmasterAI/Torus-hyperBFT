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
commitments. The deep eight-target dispersed case clearly regresses; other cases
do not establish a convincing benefit against controls. Endpoint cases have
short timings and visible order/control noise. Reject this policy; keep it
isolated for provenance, default OFF, with no chain throughput claim.
Raw logs, strata, source/binary hashes and analysis remain under
`/home/18c/bench-results-matched/s60-campaign-20260918/deep-compaction-micro-r1/`.
