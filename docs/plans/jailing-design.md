# Jailing: design

Status: design only, s136, base main af53295c. Derived from code and docs; nothing built or measured.
Jailing = removing a validator's voting power (and its leader slots) after it is dead or misbehaves. Not b1:
dead-leader skip (b1, docs/plans/dead-leader-skip.md) only removes leader slots and keeps power and quorums.

Paths: **ST** crates/torus-economics/src/staking.rs, **TY** crates/torus-economics/src/types.rs, **EPO**
crates/torus-economics/src/epoch.rs, **EP** crates/torus-economics/src/epoch_plan.rs, **RW**
crates/torus-economics/src/rewards.rs, **NE** crates/torus-bridge/src/native_executor.rs, **APP**
crates/torus-consensus/src/app.rs, **SL** crates/torus-consensus/src/slashing.rs, **HT**
crates/hotstuff_rs/src/hotstuff/types.rs, **HM** crates/hotstuff_rs/src/hotstuff/messages.rs, **VS**
crates/hotstuff_rs/src/types/validator_set.rs. On `origin/feat/dead-leader-p2` (0f3f6524, not on main): **LS**
crates/torus-economics/src/leader_stats.rs, **LK** crates/torus-consensus/src/leader_skip.rs.

## 1. Today

More exists than the roadmap suggests. Most of it is reachable, but at testnet size it does not remove anyone.

**Statuses and rows.**
- `ValidatorStatus` = Candidate / Active / Jailed / Tombstoned (TY:41-70); `jailed_until` is a block height (TY:87).
- `slash` (ST:492-573): cuts self stake and every delegation by `fraction_bps`, burns the total, writes a
  `SlashRecord`, auto-jails if self stake falls below `MIN_SELF_DELEGATION`. Unbonding entries are not slashed.
- `jail_validator` (ST:580-600), `tombstone_validator` (ST:603-614), `unjail` (ST:842-880: not tombstoned, cooldown
  over, self stake >= minimum; returns as Candidate), `top_up_self_stake` (ST:888, allowed while jailed).
- Jailed or tombstoned validators refuse new delegations (ST:108-118), oracle submissions (NE:13770-13777) and state
  hash attestations (ST:745). Inflation goes to Active only (RW:132-139). Block fee share goes to the proposer with no
  status check (RW:24-31, :371-391).

**Peer jail votes (HyperBFT style) are live on main.**
- `NativeAction::JailVote { target }` and `UnjailSelf` are dispatched (NE:8110, :8114; EIP-712 eip712.rs:308, :312).
- `record_jail_vote` (ST:618-671): voter must be Active; votes expire after `JAIL_VOTE_EXPIRY_BLOCKS`; at > 2/3 of
  the Active stake in staking rows (ST:658, not hotstuff power) the target is slashed `DOWNTIME_SLASH_BPS` = 10 bps
  and jailed for `JAIL_DURATION_BLOCKS` (TY:381-399).
- Nothing submits jail votes automatically. An operator would have to send them.

**Effect on the consensus set.**
- `compute_new_validator_set` keeps only Active and Candidate rows (EPO:35-70). So a jailed validator drops out of the
  plan the next boundary computes (EP:249-326) and leaves hotstuff one to two epochs later (plan(H+L) is written at H).
- `enforce_minimum_floor` (EPO:318-361, called at EP:284) re-seats departed validators whenever the new set would
  fall below `MIN_ACTIVE_VALIDATORS` = 4 (TY:620) and the old set met it (FIX 4, the t15 "wrongly auto-jailed"
  chain). `update_validator_statuses` leaves Jailed rows alone (EPO:365-385).
- Net effect at 4 validators: a jailed validator is re-seated with its old power. Jailing is status-only (no
  rewards, no oracle, no attestations) and never touches voting power. At 5 or more it would remove power.
- Rotation cap `n / 3` changes per epoch (EPO:140-142, :149-215).

**Double-sign / equivocation.**
- `DoubleSignDetector` and `DoubleSignEvidence` (SL:51-216) are not wired. `verify` builds
  `chain_id || view || hash || phase` big-endian with phase 0 = Prepare (SL:107-114). Real votes sign borsh
  `(chain_id, view, block, phase)` (HM:273, :287-291), little-endian, with `Phase::Generic` = 0 (HT:235-250). So it
  cannot verify a real vote (also noted in docs/plans/consensus-bug-a-equivocation.md option 2). TC phase C adds
  domain tags to every consensus signature (tc-required doc §3 "As built"), so the encoding moves again.
- `DowntimeTracker` (SL:235-300) is in-memory, local and has no caller. It must not feed consensus state.
- Local-observation slashing was removed (consensus bug (a)): `on_speculative_rollback` logs and does not slash
  (APP:10779-10813). `PendingSlash` (APP:388-393) is still drained and flushed per block (APP:10770, :4751-4800), but
  no production code pushes one. The roadmap T2.1 line "slashes+tombstones the proposer on any invalid sig" is stale.
- Owner s84: on-chain double-vote slashing with conflicting signed PhaseVotes (option B), 5% slash, 25% of it to the
  reporter; not decided where the other 75% goes or whether self-reports pay. Not built.

**Tests named "slashing_jailing".** crates/torus-integration-tests/tests/slashing_jailing.rs (5 tests) drives
`StakingManager` and the detector directly. The detector test signs with the detector's own encoding, so it passes. No
multi-node test removes a validator for misbehaviour.

**Durations are block counts against a 2 s block.** `TARGET_BLOCK_TIME_SECS` = 2 (TY:335). `JAIL_DURATION_BLOCKS` =
86,400 ("2 days"), `UNBONDING_PERIOD` ("7 days", TY:338), `BLOCKS_PER_YEAR` (TY:347). At the bench's ~16 blocks/s
(dead-leader doc §1: 61 ms views, one block per view) that is ~1.5 h of jail, ~5 h of unbonding, and validator
inflation (RW:153-158, per block of epoch) running ~30x its 5% APY. Whether an idle testnet runs that fast is not
measured here. This is a P5 problem, but jailing must not inherit it.

**b1 (dead-leader-p2, not on main).** Execution writes hashed `leader_stats:` rows from each committed block's
justify QC: `miss_run`, `j`, `excluded_at`, `readmit_after`, `seen_at` (LS:64-80). The QC evidence (view, phase class,
signer bitmap, LK:47-70) rides the commit manifest and the header batch (0f3f6524), after a review found that a
separate unsynced put could be lost on power loss and fail-stop execution on every restart. Exclusion keeps eligible
power >= quorum (LS:371-392). Parameters K and the j cap are genesis fields (`DeadLeaderParams`, torus-types lib.rs
on the branch :1755-1800). `ChainConfig::dead_leader_skip` is the P0.0 activation point.

**P0.0 is on main** (40705baf): header `features` bitmap, `Feature` registry (crates/torus-types/src/features.rs:19),
`ChainConfig.activations` (torus-types lib.rs:1834-1839). Execution-only rules can be gated. hotstuff_rs formats cannot
(dead-leader doc §3 "P0.0").

## 2. Quorum arithmetic: when jailing helps

Quorum = floor(2P/3) + 1 (VS:153-165). Jailing dead power d lowers the quorum; the live power is unchanged.

| Set (equal stakes unless noted) | Down tolerated | One dead, not jailed | One dead, jailed | Gain |
|---|---|---|---|---|
| 3 | 0 | halted, nothing commits, so no jail | — | none |
| 4 | 1 | 3 live, q = 3: tolerates 0 | 3 members, q = 3: tolerates 0 | **none** |
| 5 | 1 | 4 live, q = 4: tolerates 0 | 4 members, q = 3: tolerates 1 | yes |
| 7 | 2 | 6 live, q = 5: tolerates 1 | 6 members, q = 5: tolerates 1 | after the 2nd death (5 left: 1 vs 0) |
| 2/2/1 (bench), stake-1 dead | — | 4 of 5 live, q = 4: tolerates 0 | P = 4, q = 3: a stake-2 death leaves 2 | none |
| 3/2/2/2, a stake-2 dead | — | 7 of 9 live, q = 7: tolerates 0 | P = 7, q = 5: tolerates a stake-2 death | yes |

- **Correction to dead-leader doc §7a Q6.** "With 4 validators two dead halt the chain; jailing the first restores the
  margin" does not hold for 4 equal stakes: 3 remain, their quorum is all 3, so a second death still halts. Equal
  stakes need >= 4 members left (>= 5 before) to regain a fault.
- At 4 the floor (§1) re-seats the jailed validator anyway, so jailing cannot remove power on a 4-validator testnet.
- Jailing needs a live chain. It can only remove what is dead while a quorum still commits.

## 3. Triggers

### 3a. Downtime

Evidence must come from committed data only and be durable with the block (the b1 power-loss lesson).

- **QC signatures are not a liveness signal by themselves.** The collector builds the QC from the first quorum and
  drops later votes (HT:371-386). With 4 equal stakes each QC holds 3 of 4 signatures; the slowest honest validator
  (WAN, far away) is systematically the one left out. A Cosmos-style "signed X% of the last N blocks" rule over our
  QCs would jail it. CometBFT's last commit carries every precommit that arrives within `timeout_commit` (from
  memory, unverified); ours does not.
- **Leader slots are.** A missed slot is attributed only when the view ends with no committed certificate (b1 A1). A
  slow but live validator still leads its own views.
- **D-rule (proposed): escalate b1.** At boundary H, jail X if X has been leader-excluded by b1 since boundary E and no
  committed QC since E carries X's signature (`seen_at` empty), and header time(H) - time(E) >= `jail_after_s`.
  Inputs: `leader_stats:` rows plus the boundary header timestamps, all hashed state written by execution. No new
  node-local input, so nothing new to make durable. One new field per row: the exclusion start time.
- **False positive left:** a recovered validator whose votes never land in a QC because quorum forms without it (b1
  doc §3 caveat: 2/2/1 stake-1). Excluded, it never leads, so it can never prove itself through QCs. It gets jailed
  and must unjail (§5). Optional extra evidence: a committed `AttestStateHash` or oracle submission by X counts as
  "seen" (account-key actions; they show the operator is up, not the consensus process). Open question 3.

### 3b. Double-sign

- **E1, conflicting signed PhaseVotes** (owner s84). Two votes by X with the same (chain_id, view, phase) and different
  blocks. Producer: a node-local vote observer hook in hotstuff_rs (observe only, no format change) feeding a rebuilt
  detector. Consumer: `NativeAction::SubmitDoubleSignEvidence`, verified in execution against X's consensus key at
  that view, deduplicated by (X, view, phase).
- **E2, conflicting signed proposals.** TC phase C (F1) signs every header and proposal under the
  `proposal-header` domain tag. Two headers by the leader of view v with different hashes are transferable evidence.
  This was blocked in consensus bug (a) for lack of a signature; phase C removes the blocker. Same action, second
  evidence kind.
- Both must be proven honest-safe by test. An honest replica must never sign two PhaseVotes with the same (view, phase)
  and different blocks: the guard is `highest_view_voted` / `last_voted_proposal`
  (block_tree/accessors/internal.rs:635-660, :1681-1710). Check it across the validator-set update window (two leaders
  may nudge and re-propose, tc doc §4), TC re-proposals and crash restarts. An honest leader must never sign two
  headers for one view (re-proposals re-sign the same block).
- Evidence needs the key X had at that view (key rotation, EP:262-273) and a maximum age. That needs a key-history row
  or an age shorter than one rotation.

### 3c. Other misbehaviour

- Peer jail votes (`JailVote`, §1): subjective, but the outcome is deterministic. HyperBFT's model; see §8.
- Invalid action signatures in a proposal: today the actions are skipped, not punished. No proposer penalty proposed.
- Divergent state-hash attestation (a minority hash at a checkpoint): a sign of a broken node, not provably
  malicious. Not a trigger; maybe a metric.

## 4. Effect

- **Mechanism.** Jail = status Jailed at boundary H (execution), which removes X from plan(H+L) through the existing
  members path (EPO:43-46). Consensus at H+L sends a normal validator-set update with X deleted. No new hotstuff_rs
  format, so jailing is execution-only and P0.0-gateable.
- **Latency.** Death -> b1 exclusion (3 slots + L to 2L heights) -> `jail_after_s` -> next boundary -> + L. With L =
  100 heights and full speed after b1: about `jail_after_s` + 10-20 s.
- **Safety cap.** At most total - quorum power jailed for downtime per boundary (same bound as b1 A2). The rotation
  cap (n / 3) and the floor stay. Removing dead power keeps both update quorums live: the update block needs the old
  set's quorum and its Decide the new set's. Live power >= old quorum > new quorum.
- **The floor.** Keep `MIN_ACTIVE_VALIDATORS` = 4 as it is. It guards against a wrong jail. Consequence: at 4,
  jailing only changes status. Removal is exercised only with >= 5 validators (open question 1).
- **b1 vs jailing.** b1 excludes a leader and keeps its vote; jailing removes both. A jailed member leaves the hotstuff
  set and with it `leader_excluded` (ValidatorSet `remove()` drops it, b1 phase 1). `leader_stats:` rows of a jailed
  validator are kept for escalation; on re-entry it starts unexcluded with `miss_run` = 0 and `j` kept.
- **TC-required window.** The jail update is an ordinary epoch-boundary update. "One fresh proposer per view" (tc doc
  §4) covers it: PVS leader up to `update_view`, CVS leader after. A dead PVS leader was already skipped by b1. No new
  rule.
- **3 vs 4 vs 5.** §2. At 3, nothing; at 4, status only (floor) and no liveness gain anyway; from 5 up, real gain.

## 5. Unjail

- **Manual tx, as today (`UnjailSelf`).** Automatic unjail from QC evidence is impossible: with no power X signs no
  QC. Rate limits like HyperBFT's: one unjail per X per `unjail_cooldown_s`.
- **Minimum jail time** in header seconds, not blocks: `jail_min_s` (testnet default 600 s; Cosmos Hub launch: 10
  min, verified). Header time is bounded (not below the parent, at most 5 s ahead, roadmap P0 row; the 30 s lower
  bound is not in the testnet genesis, so a byzantine proposer can freeze time for its own blocks, which only delays
  unjails).
- **Escalation.** jail time = `jail_min_s` * 2^c, c = jails in the last `jail_decay_s`, capped (c <= 6, like b1's j).
- **Re-entry.** Unjail returns Candidate (ST:871); the next boundary plans it in and it is installed one epoch later.
  It re-enters unexcluded; if still dead, b1 excludes it again after K misses and the escalation doubles its jail.
- **Tombstone (double-sign).** Permanent; `unjail` refuses (ST:847). Also refuse its consensus pubkey at
  `register_validator` and `RotateValidatorKey` (today only the address is checked, ST:62-64).

## 6. Economics

- **Testnet:** downtime slash 0 (jail only, as HyperBFT; Cosmos Hub launch 0.01%, ours 0.1% TY:384). Double-sign 5% +
  tombstone (owner s84), 25% to the reporter, the rest burned (today's `execute_burn`) unless the owner decides
  otherwise.
- **Rewards:** jailed validators get no inflation (RW:139, already true). Add: no proposer fee share for a non-Active
  proposer (RW:371-391). Both moot for a 4-validator testnet with no external delegators.
- **Slash scope:** slash unbonding entries created after the offence (Cosmos does; ST:492-573 does not).
- **What P5 needs that does not exist:** time-based durations (§1), a reward rate that does not scale with block rate,
  permissionless entry (registration is governance-whitelisted, NE:13549), delegator UX for jailed
  validators, a decision on where the other 75% of a slash goes. Staking, delegation, unbonding, inflation and fee
  sharing exist; their parameters are not testnet-ready.

## 7. Rollout and build

**Before genesis or after.** Jailing touches execution only (statuses, rows, plan) plus one new action; it rides the
existing validator-set update. So it can ship after genesis as a P0.0 feature (`Feature::Jailing`): below H, today's
rules; from H, the D-rule, the new unjail rules and the evidence action. Old binaries stop at H (P0.0 decision s131).
It depends on b1 (rows) and TC phase C (signed bytes for E1/E2), neither on main yet.

**Parameters.** Genesis fields would have to be reserved before genesis (the genesis file cannot change later).
Alternative: governance keys with built-in defaults and ranges, read at boundaries like the EVM values
(EP:398-420, `gov_u64_in`). Proposed: `jail_after_s` 600, `jail_min_s` 600, escalation cap 6, `jail_decay_s` 1 day,
`unjail_cooldown_s` 600, downtime slash 0, double-sign 500 bps, reporter share 2500 bps, evidence max age.

**Before genesis, cheap (genesis-bound because these rules are live today):**
- `JailVote`: today 3 of 4 validators can slash and jail the 4th. Proposed: reject it unless a (never scheduled)
  `Feature::PeerJailVote` is active, or drop its slash. ~0.5 d.
- Nothing else is required pre-genesis if parameters go through governance.

**Phases (after b1 and TC phase C merge).**

| Phase | Work | Estimate |
|---|---|---|
| 0 (pre-genesis, optional) | Gate `JailVote`; reserve genesis params if wanted | 0.5 d |
| 1 | D-rule at boundaries (row field, time window, cap), status + plan removal, `Feature::Jailing`, params, unit tests | 2-3 d |
| 2 | Unjail rules: time-based minimum, escalation, cooldown, pubkey refusal for tombstoned; RPC status fields | 1-1.5 d |
| 3 | Double-sign: vote/header observer hook, detector on phase C signed bytes, `SubmitDoubleSignEvidence` (action, EIP-712, fixtures), key-at-view check, dedup, 5% + tombstone + reporter share | 3-4 d |
| 4 | Multi-node, crash replay and mutation tests | 2-2.5 d |
| 5 | Activation runbook, testnet activation and live drill | 1 d |

Codex torus review after each phase. Total 9.5-12.5 days; without phase 3 (downtime only) 6.5-8.5 days.

**Tests.**
- **Determinism:** two executors fed the same random committed chains write identical rows, statuses and plans;
  running hash covers every new row.
- **Crash replay:** crash between the boundary that jails and the one that installs; between unjail and re-entry;
  power loss after the manifest but before the flush. Same plan after replay, no fail-stop. Assert no new node-local
  input (no new `leader_qc:`-style row).
- **Multi-node:** 5 equal: kill one -> excluded -> jailed -> removed -> a second kill does not halt; restart + unjail
  -> back in the set; dead node never re-admitted. 4 equal: floor keeps it seated, status Jailed, no reward. 2/2/1:
  recovered stake-1 jailed, unjails. Byzantine double-voter (and double-proposer) tombstoned and removed at 5. Jail
  update through timeout views (tc doc window). P0.0 crossing: old rules below H, new from H.
- **Mutations** (each must fail a test): jail from QC signature counts alone; drop the power cap; drop the floor; unjail
  without the time check; count block heights instead of header time; accept evidence signed with the wrong encoding or
  a rotated key; let a tombstoned key re-register.

## 8. Options

Other chains:
- **HyperBFT** (docs, verified s136): validators vote to jail peers that respond too slowly or rarely to the voter's
  consensus messages; a quorum of jail votes jails; jailed validators stop participating and earn nothing; unjail is a
  self action after the jailed-until time, under on-chain rate limits. "Jailing is not the same as slashing", reserved
  for provable faults such as double-signing; "currently no automatic slashing". Epochs of 100k rounds (~90 min).
- **Cosmos x/slashing** (Cosmos Hub launch values, verified): `SignedBlocksWindow` 10,000 blocks,
  `MinSignedPerWindow` 5%, `DowntimeJailDuration` 10 min, slash 0.01% downtime, 5% double-sign + tombstone (permanent,
  slashed once), `MsgUnjail` after the jail time.

| Option | Trigger | Deterministic | Frames honest? | Works at 4 | Effort | Verdict |
|---|---|---|---|---|---|---|
| J0 Nothing new (b1 only; JailVote as is) | manual votes | yes | 3 of 4 can jail the 4th | status only | 0 | keeps a live griefing rule |
| J1 Cosmos window over QC signatures | signed < x% of N QCs | yes | **yes: the slowest honest validator** | status only | 2-3 d | rejected as is |
| J1b J1 + leader carries late votes in the next block | full signer set | yes | no | status only | +3-5 d, hotstuff_rs format (genesis-bound), bandwidth | heavy |
| **J2 Escalate b1 exclusion (D-rule)** | excluded + never signed for `jail_after_s` | yes | only an unneeded recovered voter (unjails) | status only | 3-4.5 d | **recommended** |
| J3 Automated peer jail votes (HyperBFT) | each node's local latency monitor sends `JailVote` | outcome yes, inputs local | a 2/3 coalition can jail anyone (it can stall anyway) | needs all 3 live | 2-3 d + monitor | later, for large sets |
| J4 J2 + J3 | both | — | — | — | 5-7 d | if J2 alone proves slow |
| E1 / E2 double-vote / double-proposal evidence | on-chain evidence tx | yes | no (honest-safety tested) | removes at >= 5 | 3-4 d | recommended, with phase C |

| Timing | Pros | Cons |
|---|---|---|
| T1 before testnet genesis | no activation needed | b1 and TC phase C not merged; nothing to test live at 4 validators; delays genesis |
| **T2 testnet upgrade via P0.0** | tested live, as owner s134 wanted; b1 runs first | needs >= 5 validators to remove power live; params via governance or reserved fields |

**Recommendation.** J2 + E1/E2, shipped as T2 (`Feature::Jailing`) after b1 and TC phase C merge. Testnet economics: no
downtime slash, 5% + tombstone for double-sign, 25% to the reporter. Manual unjail with a time-based minimum and
doubling. Keep the floor. Before genesis, only gate `JailVote` (0.5 d). Decide the validator count first: at 4 the
whole feature is status-only.

## 9. Open questions for the owner

1. Testnet validator count: jailing removes power only at >= 5 (equal stakes) and gains margin only from 5 up. Go to 5,
   stay at 4 (jailing = status only), or lower the floor to 3 (no margin gain, re-opens the t15 risk)?
2. `JailVote` before genesis: gate it behind an unscheduled feature (recommended), drop its slash, or keep it?
3. D-rule evidence: QC signatures only, or also committed `AttestStateHash` / oracle submissions as "seen"?
4. Double-sign evidence: votes (s84), proposals (possible after phase C), or both (recommended)?
5. Penalties: downtime slash 0 on testnet? Where does the other 75% of a double-sign slash go (burn, treasury)? Pay
   self-reports?
6. Parameters: governance keys with built-in defaults (recommended), or reserve genesis fields now?
7. Durations: move every staking duration to header time now (P5; affects unbonding and inflation), or only jailing?
8. HyperBFT-style automated peer votes (J3): later for large sets, or never?
9. Slash unbonding entries created after the offence (Cosmos), before mainnet?

Sources: Hyperliquid docs, HyperCore "Staking" (hyperliquid.gitbook.io, fetched s136); Cosmos Hub parameter wiki,
x/slashing launch values (github.com/gavinly/CosmosParametersWiki, fetched s136); CometBFT last-commit behaviour from
memory (unverified).
