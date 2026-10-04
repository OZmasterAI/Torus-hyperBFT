# Forty focused review runs — source `14236fa`

Read-only follow-up review against `origin/perf/item6-phase1` at
`14236fa50d685649bc9bdfb6440eede7fe57d576`. “Run” here means a separately
scoped static review question, not a test execution. The source was not
modified and no runtime/test claim is made. Each item records the improvement
or evidence needed; existing issue numbers are rechecks and are not counted as
new findings. See the [deduplicated synthesis](chain-findings-synthesis-2026-10-05.md)
for canonical issue groups.

| Run | Review scope | Result / documented improvement |
| ---: | --- | --- |
| 1 | Proposer execution gas bounds (F01) | Recheck the pre-execution bound at this revision; add a regression proving the proposer and executor apply the same cap to every accepted transaction class. |
| 2 | EVM priority fee accounting (F02; pass 30) | One accounting claim, not repeated findings. Assert effective gas price, base-fee burn, priority-fee recipient, and protocol share in one receipt fixture. |
| 3 | Session-key expiry (F03) | Keep admission-time and execution-time expiry semantics aligned; test a key expiring between pool admission and committed execution. |
| 4 | Storage fault during committed execution (F04) | Recheck write ordering and commit behavior; fault-inject each persistence boundary and inspect restart state before assigning impact. |
| 5 | Vote persistence before send (F05) | Verify durable vote state precedes outbound publication and that restart cannot emit conflicting votes; record persistence and network ordering explicitly. |
| 6 | ADL scan progress (F06; pass 46) | Treat scan-window progress as one invariant. Exercise more candidates than one scan window and prove the cursor advances or wraps without starvation. |
| 7 | Header epoch and fee transition (F07) | Trace epoch authority end to end; test transition blocks where header epoch and stored fee schedule differ. |
| 8 | Consensus-height overflow (F08) | Prove checked height arithmetic at the execution-feed boundary; exercise maximum representable height and verify failure is controlled. |
| 9 | Governance snapshot omission (F09) | Keep snapshot membership and abstain semantics distinct; test absent, zero, and positive voting-power rows across snapshot creation. |
| 10 | Duplicate validator keys (F10) | Validate uniqueness before constructing voting power; test duplicate consensus and account keys in genesis and rotation inputs. |
| 11 | Validator rotation capacity (F11) | Specify the small-set replacement rule and test a full set at each supported size, including the minimum size. |
| 12 | Off-mark fill / ADL / withdrawal solvency (F12) | Keep as one cross-feature solvency scenario; assert vault equity and withdrawable balance after adverse marks, ADL, and withdrawal sequencing. |
| 13 | Wrong-phase certificate handling (F13) | Specify certificate phase validation and recovery; test malformed phase input followed by valid progress without restart. |
| 14 | Sync certification and fork choice (F14) | Keep certified and merely received sync content distinct; test that uncertified data cannot become the finalized fork after restart. |
| 15 | Pre-validation memory exposure (F15) | Bound allocations before expensive validation; test oversized and deeply nested inputs at the transport boundary. |
| 16 | Pre-validation disk exposure (F16) | Bound durable staging before validation; test rejected bodies leave no unbounded or orphaned persistent data. |
| 17 | Typed EVM transaction environment (F17) | Add type-specific fixtures proving typed transactions preserve all envelope fields through environment construction. |
| 18 | Persisted bytecode analysis padding (F18) | Separate executable bytecode from analyzer-only padding; compare stored bytes, code hash, and execution behavior for boundary bytecode lengths. |
| 19 | Restart configuration identity (F19) | Persist or validate chain-critical configuration identity; test normal restart with a changed chain ID and changed fee parameters. |
| 20 | Wallet dry-run side effects (F20) | Define dry-run as side-effect free and assert balances, nonce, pool, and network remain unchanged after invocation. |
| 21 | Restored snapshot security state (F21) | Verify restored state is committed and checked before service; test tampered security rows and clean restart. |
| 22 | CoreWriter and market initialization gaps (F22–F25) | Keep each externally visible operation separate; exercise create/list/update paths and require returned identifiers to resolve to persisted entities. |
| 23 | Explorer partial-ingestion repair (F26) | Add resumable ingestion from a durable cursor and test recovery after a crash midway through a block. |
| 24 | Governance update application (F27) | Distinguish accepted proposal from applied state; assert the effective configuration changes at its specified activation height. |
| 25 | Genesis commission bounds (F28; pass 25) | Recheck genesis validation against reward arithmetic assumptions; test minimum, maximum, and just-outside values. |
| 26 | Governance processing triggers (F29) | Ensure due proposals progress without unrelated user activity; test empty blocks across an activation height. |
| 27 | EVM account mirror repair (F30) | Keep applied markers ordered after repair completion; fault-inject between marker and mirror updates and verify restart convergence. |
| 28 | Pruning versus replay safety (F31) | Derive retention from replay dependencies rather than committed height alone; test recovery from the oldest supported checkpoint. |
| 29 | RPC market identifier parsing (F32) | Preserve canonical market selection; test numeric-looking identifiers, radix prefixes, and unknown-market rejection. |
| 30 | EVM `getOpenOrders` production data source (F37) | Recheck reader/writer pairing; either wire a production writer or return an explicit unsupported response rather than an empty success. |
| 31 | EVM pool byte-cap admission (F39) | Recheck occupancy accounting under concurrent admission/removal; compare gross, reserved, and committed bytes at the cap boundary. |
| 32 | Trade-history backfill (F40) | Make pagination continue beyond the newest page; test stable cursors while new fills arrive. |
| 33 | Ethereum quantity/access-list RPC behavior (F41–F42) | Keep serialization and simulation input as separate contracts; test leading-zero quantities and access-list propagation independently. |
| 34 | Wallet nonce reuse (F44) | Reserve nonce across queued sends; test sequential submissions before pool inclusion and after rejection/replacement. |
| 35 | Zero-active delegation reward residue (F45) | Preserve eligibility semantics for unbonding-only rows; use a rounding fixture that demonstrates whether residue can reach an inactive recipient. |
| 36 | EVM pool backlog telemetry (F46) | Recheck the gauge writer and alert query as one instrumentation path; add a scrape-level assertion that a growing pool changes the exported series. |
| 37 | Faucet nonce reservation (F47) | Recheck failure ordering around gas-price lookup; test transient RPC failure and ensure the next request uses the first unsent nonce. |
| 38 | C3 sums cache and PF1 ask-depth accumulator | No new defect promoted by the earlier integration rechecks. Preserve reference-equivalence tests for cache invalidation and prefix sums across insert, remove, and level changes. |
| 39 | Receipt fee bounds and aggregation (passes 77–78) | Consolidate multiplication and sum into one boundedness investigation with two arithmetic sites; document the maximum valid receipt and block totals before promotion. |
| 40 | Epoch reward and multi-row staking/governance writes (passes 83–96) | Consolidate partial-write candidates as one atomicity risk class while retaining per-operation fault scenarios. Specify commission timing and prove overlay rollback/persistence before counting any as confirmed. |

## Run summary

- 40 scoped static review runs recorded.
- 0 source changes and 0 runtime tests claimed.
- Existing findings were rechecked or translated into concrete evidence and
  regression targets; no duplicate issue numbers were added.
- The runs reuse pinned evidence from the detailed reports where the review
  question is a direct recheck. They do not claim that all 40 areas were
  independently reimplemented or dynamically reproduced.
