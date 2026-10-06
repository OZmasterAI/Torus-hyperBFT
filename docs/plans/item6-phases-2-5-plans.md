# Plans: item 6 Phases 2-5 (short form)

Status: PLAN, nothing built. Written s89 (2026-10-04). Each phase gets a full plan (in the
form of `item6-phase1-impl.md`: budget, step 0 guardrails, steps with cost gates, stop
rule) when the phase before it has passed its gate, because the profile after each phase
decides the details. Design: `market-scaling-in-memory-design.md` section 3.

Common to every phase:
- Base: main after the crab + Phase 1 merge (Gate 2); each phase is a branch on the
  previous one (D16), benched against main and against the previous phase (same campaign,
  interleaved, >= 4 cells per arm, crab cells with the oracle on).
- Step 0 of each phase: re-profile the previous phase's binary (300 markets uniform,
  oracle on), confirm the phase's target items are still the top costs; if they are not,
  the plan changes before any code.
- Correctness: differential vs the previous phase (CF dumps + `h_n` per block, all four
  BookModes, serial vs pipelined), goldens A/B unchanged, P1/P3 rerun.

## Phase 2: per-block work in proportion to fills (size M)

| item | today (main, per native block, 300 mkts) | gate |
|---|---|---|
| cancel/modify scan of all books -> OrderId -> MarketId index | part of phase 1 105-140 ms | phase-1 actions ms down, error strings and result order unchanged |
| persistent worker pool (match, settle, Phase-2 shards, book drain) | ~1,300 thread ids / 60 s | thread ids per minute ~constant; fail-stop on worker panic kept |
| cache flush: serialise once, same bytes to the overlay and to R | 65-81 ms | flush ms down |
| stops dirty flag in `diff_stop_rows` | part of save books 80-115 ms | save-books ms down |

Phase gate: +7-15% matched/s vs Phase 1 (lower bound), no 10-market regression.
Depends on Phase 1 measurement: if the crab paths still dominate after Phase 1 (gate 3/4
optional steps), those come first.

## Phase 3: coalesced state checkpoints + replay (size L)

| item | gate |
|---|---|
| since-checkpoint layer; checkpoint every ~30 s of execution, forced at EVM, epoch-boundary and slash blocks (D6) | crash at random points (incl. mid-checkpoint) -> identical state and every `h_n` |
| running hash digest at freeze, `h(n-1)` in memory, `h_S` persisted | `h_n` identical to Phase 2 on the same blocks |
| readers on a block-consistent view (RPC, `eth_call`, ingress, consensus-thread lookups) | RPC / consensus suites identical with interval 1 vs "never" |
| pruner clamped to the oldest snapshot and to S; snapshot rotation | pruner never deletes a body above S |
| crab: R and the sums cache rebuilt cold after replay | warm == cold after checkpoint replay (P5/P6 with interval > 1) |

Phase gate: about -10% node CPU-s/1M, disk write bytes down by the coalescing factor,
restart -> ready <= 60 s (s75 multi-crash cells), no throughput loss.

## Phase 4: one record per trader in memory (size M)

| item | gate |
|---|---|
| typed `UserState { balance, cum_volume, open_order_count, positions: sorted Vec }`, replaces R's two raw maps; rows produced at freeze (D4 a) | identical rows and `h_n` vs Phase 3 |
| crab: the sums cache moves into `UserState`; the separate map is deleted | P1 and P3 green |
| `UserState` map ordered by address serves `traders_after` / `adl_candidates` | P3 green |

Phase gate: +0-10% matched/s; RSS per user within section 4's estimate.

## Phase 5: full in-memory execution (size L)

| item | gate |
|---|---|
| all native state typed in memory; overlay off the hot path; change set from typed dirty sets | full differential vs Phase 4 on long randomized sequences |
| crab: sums updated inline per fill | crab overhead <= +15% per fill vs the same phase without crab |
| EVM precompiles and RPC through a `StateBackend` adapter | EVM / RPC suites green |

Phase gate: Gate 3, >= 1.4x today's main matched/s at 300 markets; 1M-account /
1M-resting-order genesis cell for RSS and restart.

## After Phase 5 (separate plans)

- D: sequential second pass for margin-cut takers (design section 3.6), golden A re-pin.
- L3 + D12 at one fresh genesis (owner decision).
