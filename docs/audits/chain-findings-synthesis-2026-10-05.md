# Chain audit findings synthesis — 2026-10-05

This document consolidates the October 4–5 audit reports in this directory. It
counts a behavior once even when later reports recheck it or find another
entry-point into the same underlying risk. The detailed reports remain as the
historical evidence trail; repeated report sections are not separate defects.

## Scope and confidence

The report set spans several source revisions, from `cea1254` through
`14236fa50d685649bc9bdfb6440eede7fe57d576`. Findings must be checked against
the source revision named in each report before being treated as current. These
are static-review results. Unless a report explicitly cites a reproducing
production-code test, runtime reachability and consequences remain unconfirmed.
The 40 follow-up review runs are in
[`chain-14236fa-40-review-runs-2026-10-05.md`](chain-14236fa-40-review-runs-2026-10-05.md).

## Canonical finding groups

| Canonical issue | Consolidated reports | Current disposition |
| --- | --- | --- |
| EVM fee charging/revenue arithmetic and beneficiary accounting | F02; passes 30, 77–80 | Keep the priority-fee double-credit report as the accounting issue. Passes 77 and 78 are two unchecked-arithmetic sites within one boundedness investigation, not two confirmed defects. Pass 80 is a recheck. Fee limits and receipt bounds still need proof. |
| Fee burn and supply reporting | passes 29 and 81 | One reporting/tracker question. Pass 81 is a recheck; distinguish actual token supply from cumulative counters before claiming a supply invariant violation. |
| Reward/commission boundary semantics | F28; passes 25, 64, 83 | One effective-height and arithmetic contract. Pass 83 rechecks whether a boundary-block commission update affects the epoch being closed. Genesis bounds and rotation snapshots are related guards, not duplicate independent failures. |
| Reward allocation and reward accounting | F45; passes 62–69, 84–88 | Keep only separately demonstrated behaviors: eligibility/proration, rounding residue, arithmetic bounds, and failure atomicity. Passes 84–85 are conditional overflow hypotheses; 86–88 are separate payout paths sharing the same partial-write concern. Do not count every recipient loop as a distinct root cause. |
| Multi-row staking/governance state atomicity | passes 24, 89–95 | One systemic risk class with distinct operations (delegate, undelegate, permanent stake, slash, jail vote, unlock, treasury spend). Preserve operation-specific scenarios as test cases, but report a single systemic issue until fault injection proves separate externally observable failures. |
| Treasury and developer/community fee-pool routing/reporting | passes 71–76, 95 | Keep routing, epoch reset, and balance-versus-cumulative reporting as separate questions only where their state and observable consequences differ. Pass 95 is a treasury transfer atomicity case within the multi-row group above. |
| C3/PF1 integration and performance behavior | passes 13–16 | No new correctness defect established for the C3 cache or PF1 prefix accumulator in the cited rechecks. F46 backlog telemetry is a distinct observability candidate; F47 faucet nonce gaps is a separate availability candidate. |
| Liquidation/ADL ordering and accounting | F06, F12; passes 16, 35, 37–49 | Keep independent invariants (ADL scan progress, solvency, backstop position cleanup, cooldown timing, rounding, deterministic ranking) separate. Repeated boundary probes of the same invariant are qualifications, not additional findings. |
| Persistence, replay, and execution durability | F04, F13–F16, F18–F21, F26, F30–F31; passes 18–21 | Group around distinct contracts: committed execution/replay, sync certification, restore verification, config identity, and history repair. Do not merge merely because each involves storage. Passes 18–21 are focused follow-ups, not automatically new findings. |
| Client/RPC compatibility and operational signals | F32, F37, F39–F44, F46–F47; passes 7–14 | Deduplicate repeated wallet nonce, serializer/query, telemetry, and RPC input claims by their externally visible contract. F46 and F47 remain separate from client compatibility. |

## Highest-priority follow-up

1. Reproduce committed-state behavior under injected storage failures for the
   reward, staking, slash, unlock, and treasury paths. Source ordering alone
   does not establish that earlier overlay writes survive a failed action or
   committed block.
2. Prove the protocol bounds on EVM gas, effective gas price, receipt count,
   cumulative fees, reward accumulation, and account balance credits. Promote
   passes 77, 78, 84, and 85 only if a valid state can cross the respective
   arithmetic boundary.
3. Specify commission effective height and reward eligibility at epoch
   boundaries, then compare implementation and genesis validation against that
   rule.
4. Revalidate high-impact historical findings against one current source
   revision and attach targeted production-code regressions before describing
   them as confirmed.

## Duplicate handling

No historical report was deleted: reports pin distinct revisions and contain
useful falsification evidence. Duplicate *finding counts* are removed by this
canonical grouping. The index continues to list each review pass, while this
file is the entry point for deduplicated conclusions. No source code was
changed.
