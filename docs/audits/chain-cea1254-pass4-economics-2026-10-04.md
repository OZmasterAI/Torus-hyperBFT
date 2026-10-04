# Torus economics and lifecycle audit, fourth pass

Audited revision `cea1254e34625e6b09c58f794de8793b5c12713c`, branch `merge/item6-sync2`, on 2026-10-04. This companion audit covers staking, economic governance, epoch scheduling, validator eligibility/rewards/slashing/unbonding, and genesis economics. No production code, Git history, deployed chain, or toolchain was changed. Applicable ancestor/repository `AGENTS.md` files were searched; none were found.

**Three additional source-supported P2 candidates:** executed governance settings do not affect their consumer, malformed genesis commission can create nearly `2^256` of claimable rewards, and a quiet chain skips governance deadlines. IDs E01–E03 are local to this report. They are not renumberings of F01–F26 or the known supplemental R2/R3/R5/R7. E02 requires operator-supplied malformed genesis; it is not an arbitrary remote mint on a valid genesis.

Existing audit convention applies: these are candidates for failing production-code regressions. Cargo/rustc were unavailable; no Rust execution, signature-bearing chain fixture, or live reproduction is claimed. The companion [Python model](chain-cea1254-pass4-economics-models.py) passed all three source-guarded arithmetic/control-flow counterexamples. It does not invoke Rust, deserialize a real genesis, execute signatures, or reproduce consensus.

## E01 — Executed governance settings remain disconnected from effective parameters

**Priority:** P2 medium. **Precondition:** a legitimate, sufficiently staked proposer obtains the necessary vote and waits for the existing timelock. No malicious proposer or configuration edit is needed.

**Production reachability and source anchors.**

* Signed `NativeAction::SubmitProposal` maps `ProposalAction::ParameterChange { key, value }` to the economic payload in `crates/torus-bridge/src/native_executor.rs:7932`, and calls `GovernanceManager::submit_proposal` at `:7969`.
* `validate_param_change` accepts bounded governance keys: `voting_period_blocks` (`crates/torus-economics/src/governance.rs:587`), `quorum_bps` (`:601`), permanent-weight numerator/denominator (`:615`, `:629`), `timelock_blocks` (`:657`), and `permanent_unlock_threshold_bps` (`:673`). Parameter bounds are checked again at execution.
* Execution writes only `CF_FEE_CONFIG[param_key] = new_value.as_bytes()` at `governance.rs:1058`. `execute_proposal` then stores `ProposalStatus::Executed` at `:994` and returns `Executed` at `:997`.
* Effective governance is instead read exclusively from the serialized `b"gov_params"` row, declared at `governance.rs:43`, read at `:1212`, and written by `set_governance_params` at `:1223`. No merge of the per-parameter ASCII keys occurs.
* Proposal duration uses the returned struct at `:772`; finalization uses its quorum at `:931`, permanent-unlock threshold at `:941`, and timelock at `:958`; vote-weight computation uses its multiplier at `:1289`–`:1301`. These consumers all retain the original struct values.

Repository-wide `CF_FEE_CONFIG` reader search found no operational reader of the individually changed keys. The existing `execute_parameter_change` test (`crates/torus-economics/tests/governance_tests.rs:347`) checks the ASCII `max_leverage` key and Executed status, which cannot establish that a consumer changed.

**Counterexample.** With default quorum 3300 bps (`governance.rs:33`), execute an accepted proposal changing `quorum_bps` to `"5000"`. The proposal reports Executed and the key contains `"5000"`, but `get_governance_params().quorum_bps` remains 3300. A subsequent proposal with 40% yes weight and no opposition still passes, although the approved 50% threshold would reject it. Fixed, complete voter snapshots and unchanged stake suffice; F09's missing snapshots are not required. Similarly, approving `timelock_blocks="10"` leaves the original 43,200-block default delay in effect.

**Impact.** A successful on-chain vote advertises a governance rule change that never applies. Operators/voters can rely on an approved quorum, voting period, multiplier or delay while future governance continues under the old rule. This finding establishes incorrect execution semantics, not unauthorized governance capture.

**Fix and regression.** Map supported governance settings to typed updates of the canonical `GovernanceParams` record, or make all consumers read a single canonical parameter representation. Preserve input bounds and define which already-open proposals inherit a rule change. Execute a real signed quorum/timelock change through committed-block execution; check the effective RPC configuration, the next proposal's end height, and a 40%-yes subsequent vote under the approved 50% quorum. Assert there is no success status without an effective update.

**Prior provenance.** ECON-PF-12's arbitrary-key overwrite is a different, now allowlisted mechanism. `maintenance_margin_bps` and `max_leverage` being validated but unread are explicitly documented C9 limitations (`docs/plans/liquidation.md:280`); those market keys are not counted as new here. E01 specifically concerns accepted governance settings disconnected from their canonical struct.

## E02 — Genesis commission bypass can create an enormous claimable reward

**Priority:** P2 medium for the operator-controlled bootstrap trigger; the resulting accounting failure is severe. **Precondition:** the chosen genesis contains a validator commission above 10,000 bps and at least one real delegation exists when that validator receives nonzero inflation. Validly registered validators cannot introduce this value through the normal registration/update handlers.

**Production reachability and guards.**

* `GenesisValidator.commission_bps` is an unconstrained deserialized `u16` (`crates/torus-genesis/src/lib.rs:176`); 20,000 is representable. `Genesis::from_file` simply reads/deserializes JSON at `:313`–`:315`.
* Node first-run startup calls `Genesis::from_file`, then `initialize` at `crates/torus-node/src/main.rs:530`–`:537`. No semantic commission validation occurs between these calls.
* Initialization directly copies that commission into an Active staking record (`genesis/src/lib.rs:398`, `:401`) and writes it at `:408`. It bypasses `StakingManager::register_validator`'s `MAX_COMMISSION_BPS` check (`crates/torus-economics/src/staking.rs:51`). Consensus genesis construction validates keys but not commission (`genesis/src/lib.rs:572`–`:583`), so distinct, valid keys are compatible with this counterexample.
* `delegate` accepts any positive affordable amount to an Active validator without checking its commission (`staking.rs:103`–`:136`). A sole delegator therefore supplies the needed real reward row.
* Every height-based epoch boundary runs the native phase (`crates/torus-consensus/src/app.rs:1948`), and `process_epoch_boundary` invokes `distribute_validator_inflation` before rotation (`crates/torus-bridge/src/native_executor.rs:8298`). The historical empty-boundary omission is fixed in this checkout; it is not needed for E02.
* Inflation computes `commission = val_emission * commission_bps / 10000`, then `delegator_pool = val_emission - commission` (`crates/torus-economics/src/rewards.rs:216`–`:217`). The sole last delegation receives the full pool at `:238`–`:245`.
* `credit_rewards` persists that U256 liability (`staking.rs:982`–`:993`). The signed native ClaimRewards handler calls `claim_rewards` (`native_executor.rs:7503`), which credits the EVM account then deletes the liability (`staking.rs:406`–`:420`). ClaimRewards is funding-exempt (`crates/torus-mempool/src/funded.rs:73`–`:79`), so a delegator whose liquid account was drained by delegation can still claim.

**Pinned arithmetic dependency.** `Cargo.lock` pins ruint 1.17.2, checksum `c141e807189ad38a07276942c6623032d3753c8859c146104ac2e4d68865945a`. The downloaded official crate archive matched that checksum. Its `src/add.rs:190`–`:191` maps Add/Sub operators to `wrapping_add`/`wrapping_sub`. Thus this is modular U256 subtraction, not a recoverable subtraction error or debug-only overflow assumption. See the [versioned ruint API/source](https://docs.rs/ruint/1.17.2/ruint/struct.Uint.html); locally inspected source is `/tmp/torus-cea1254-pass4-economics-deps/ruint-1.17.2/src/add.rs`.

**Counterexample.** Use four distinct genesis validators with 10,000 TRS self stake each, a funded delegator D with 1 TRS, and commission 20,000 only on validator V. D delegates its 1 TRS to V. Choose V first in deterministic address order and an epoch length of 100,000. The modeled V emission is `E = 3,171,296,296,296,296,296` wei. Commission is `2E`; subtraction wraps, making D's pool `2^256 - E`. V receives `2E` pending rewards and D receives `2^256 - E`. ClaimRewards turns D's liability into that spendable EVM balance if its pre-claim liquid balance is zero (or a sufficiently small positive balance).

An arbitrary-precision sum of those two liabilities is `2^256 + E`, whereas a U256-only sum wraps back to `E`. A conservation assertion implemented solely with U256 addition can therefore hide this error. This counterexample needs no duplicate keys, synthetic delegation row, custom header epoch, or fee-transition activation. Epoch-zero validator fee share being zero does not disable the independent validator inflation path.

**Fix and regression.** Validate every genesis validator's commission against the protocol maximum before any state writes; share admission invariants with the normal registration path. Reject semantically invalid stored commissions at reward distribution and use checked commission/pool arithmetic as defense in depth. Test genesis commissions 5000, 5001, 10001 and 20000, asserting rejection before partial initialization for invalid inputs. Through an initialized four-validator chain, delegate, cross a boundary, and claim; assert each emitted reward is bounded by the intended emission, using checked or wider aggregate accounting. Existing malformed genesis state requires an explicit repair/relaunch policy.

## E03 — Quiet blocks skip governance finalization and execution deadlines

**Priority:** P2 medium. **Precondition:** an ordinary passed-capable proposal followed by blocks without native actions, EVM fee revenue, due CoreWriter work, oracle submissions or liquidation continuation. A quiet market-free chain or a chain before its price feeders start meets those conditions. Block production itself can continue.

**Production scheduling.** The authoritative native-phase gate (`crates/torus-consensus/src/app.rs:1945`–`:1954`) considers native input, EVM fees, CoreWriter, an epoch boundary, oracle due work and liquidation due work. It contains no governance due check. `NativeExecutor::process_governance` is called only inside that gated phase (`app.rs:2302`); it is the only live caller of `process_pending_proposals` (`crates/torus-bridge/src/native_executor.rs:8341`). The economic method says it is called once per block (`crates/torus-economics/src/governance.rs:1006`), finalizes at `current_block > end_block` (`:1022`), and executes at `current_block >= executable_after` (`:1031`). There is no separate native ExecuteProposal action restoring an independent timer path.

**Counterexample with defaults.** Submit a sufficient-stake validator-registration proposal at height 1, cast sufficient yes votes by height 2, and then continue with empty blocks and no oracle/liquidation/queue rows. Voting lasts 302,400 blocks and timelock lasts 43,200 blocks; use the node's default 100,000-block epoch length. The first eligible finalization is height 302,402. That block does not run the native phase, so the proposal stays Active until height 400,000, when the epoch boundary runs governance. Its delayed timelock then ends at 443,200, another skipped quiet block; execution occurs at height 500,000. Calling the documented block scheduler on every height would instead execute at 345,602. The additional 154,398 blocks are about 3.57 days at the nominal two-second target.

**Impact and limits.** Governance state transitions depend on unrelated transaction/market activity and can be delayed by roughly an epoch per stage. The timelock starts at delayed finalization, compounding the delay. An included native action wakes the scheduler, and epoch boundaries provide eventual progress under a positive epoch length. This is not a permanent consensus wedge or a claim that the existing timelock can be bypassed. Supply is not minted by this delay.

**Fix and regression.** Run governance maintenance outside the optional native-action gate, or track its next pending deadline and include a deterministic governance-due predicate. Keep its writes on the same execution overlay and flush. Submit and vote on a real signed proposal, then feed only otherwise-empty blocks through the live execution context across end/timelock heights; require the correct status and whitelist row at the first eligible heights. Test with and without oracle/fee activity and in serial/pipelined modes.

## Historical issues, corrected claims, and unpromoted leads

The prior chain reports, Astra round 1/2, `research/ECON-AUDIT-3.4.4.md`, and `docs/parity-audit-fixes-s515.md` were checked to avoid counting prior issues as new.

* F09's incomplete voter snapshots/current quorum denominator, F10's duplicate/pending validator keys and silent invalid-key conversion, F11's small-set replacement cap, F07's inherited fee epoch, and F19's restart configuration drift remain prior findings. This pass does not close or relabel them.
* Self-stake reward allocation discontinuity is already Astra round 1 section 8, and current tests intentionally expect commission-only validator rewards when external delegations exist. It needs a policy decision; it is not E02.
* Unbonding principal remains unslashable: slash touches active delegation amounts and ClaimUnbonded releases queued principal unchanged. This is explicitly historical (`docs/parity-audit-fixes-s515.md:206`) and is not a new finding.
* One failing execution can block later passed proposals through `?` (`governance.rs:1032`); that is explicitly historical at `docs/parity-audit-fixes-s515.md:203`. Furthermore, failure examples requiring TreasurySpend/PermanentUnlock payloads are not arbitrary live submissions in this checkout: the signed ProposalAction enum lacks those variants. No remote poison-pill exploit is claimed.
* TreasurySpend and PermanentUnlock economic handlers exist, but the signed proposal mapping cannot create them. PermanentUnlock tests directly invoke the economic API. This is a coverage/integration gap, not an arbitrary unlock vulnerability; the older permanent-staking specification calls the lock irreversible, so no new withdrawal defect was inferred solely from the missing action.
* The historical empty-boundary reward omission (Astra round 2 item 17) is **fixed in current source** by `app.rs:1948`. E03 concerns governance on nonboundary quiet blocks and remains distinct. This is source revalidation, not a passing Rust certification of the fix.
* Jailed/tombstoned records can remain seated for a cap/minimum-floor transition while their reward/jail status is separately retained. That behavior is explicitly documented in epoch-floor code (`crates/torus-economics/src/epoch.rs:358`). No new committee safety theorem or status-only bug is inferred from that intentional split.
* State-hash attestations and JailVote use current Active stake, not historical consensus-power snapshots. Their policy is explicit in the implementation. A richer adversarial schedule is required before promoting a new security finding about those weights; no conflicting commit or remote false-quorum incident was reproduced.

Coverage included registration/delegation/undelegation/ClaimUnbonded, permanent stake and governance unlock internals, reward liability/claim paths, commission updates, slashing and jail votes, state-hash attestations, candidate/jailed/tombstoned status handling, key rotation planning/application, consensus update conversion, genesis economic initialization, governance payload submission/voting/finalization/execution, and block hook scheduling. ClaimUnbonded's atomic batch, positive/zero/no-maturity checks, deterministic set ordering, shared epoch-plan reads, and normal registration/commission bounds were inspected rather than assumed absent. No fresh finding was established for their ordinary guarded paths.

## Validation and remaining work

```sh
python3 docs/audits/chain-cea1254-pass4-economics-models.py
```

Observed output: E01 retained 3300-bps effective quorum after the 5000 write; E02 derived the nearly `2^256` liability and demonstrated modular-sum masking; E03 derived finalization 302402→400000 and execution 345602→500000. All three models passed source guards at the audited SHA. ruint 1.17.2 source/archive checksum was inspected independently. No production-code test passed or failed because Rust was not runnable here.

Required next evidence is the focused signed-action execution/genesis regressions above, with actual RocksDB persistence/restart and serial/pipelined scheduling where relevant. Reports/models are local audit artifacts; no issues were marked fixed, no live transaction was sent, and no production edits or Git mutations were performed.
