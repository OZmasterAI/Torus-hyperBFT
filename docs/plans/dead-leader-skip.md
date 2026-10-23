# Dead-leader skip: design

Status: design s134 (base main 42c408e4); phases 1-2 built on the TC set (feat/dead-leader-p2-on-tc); phase 3
multi-node and mutation tests s138 (§8: two bugs fixed, the 2:2:1 bench shape halts at its first exclusion, open).
Nothing benched yet (the one-validator-down baseline on archy will be the "before").
Paths under `crates/hotstuff_rs/src/` unless stated: **PM** pacemaker/implementation.rs, **HS**
hotstuff/implementation.rs, **HT** hotstuff/types.rs, **RO** hotstuff/roles.rs, **VS** types/validator_set.rs;
**TT** crates/torus-types/src/lib.rs, **APP** crates/torus-consensus/src/app.rs, **EP**
crates/torus-economics/src/epoch_plan.rs.

## 1. Today

**Leader selection.**
- leader(v) is IWRR over (v, committed validator set) (PM:1066-1123).
- Reputation weighting exists but is off: the kill switch defaults to false (PM:1125-1147) and genesis
  `reputation_leader_selection` is false (TT:1780-1785).
- Even switched on, it would not skip a dead leader: its 2,500 bps floor keeps a dead node at 25% of its weight
  (HT:694-704), and its inputs are local events (PM:363-375, block_tree/accessors/internal.rs:365-382), which caused
  the B3 stall.

**What a dead leader(v) costs.**
- Votes for v-1 go to leader(v) and to the backup leader(v+1) (HS:1197-1220, RO:131-163). QC(v-1) still forms and
  replicas enter v on AdvanceView (PM:192-215, :529-537). No block is lost.
- Nobody proposes in v. Each replica waits for its deadline, broadcasts a TimeoutVote and enters v+1 (PM:153-180).
- Deadline schedule: absolute per epoch, one slot = `timeout_base_ms` = 1200 ms (TT:1754-1756, :1808-1810;
  torus-node main.rs:1141; PM:894-928), rebased when more than 2 slots ahead (PM:599-615; test PM:1658-1674). At
  61 ms views the time left on entry alternates between ~1.2 s and ~2.34 s, so the dead-view time T averages
  ~1.8 s. The stall multiplier is 1 on entering v because QC(v-1) exists (PM:805-817); view v+1 gets x2
  (PM:617-635), which costs nothing while leader(v+1) is live.
- The s75 leader-skip (HS:1809-1858) only pulls a leader that is behind forward, on NewViews from more than f power.
  It never skips a dead leader.
- Nothing removes a dead validator: `DowntimeTracker` (torus-consensus/src/slashing.rs:231-295) has no caller.

**Arithmetic.** t = 61 ms live view time (1/16.4 s, one block per view), s = the dead leader's share of views:
blocks/s = (1-s) / ((1-s)·t + s·T).
- 4 equal stakes, s = 1/4: 1.51 blk/s (2.17 at T = 1.2 s, 1.19 at 2.34 s), i.e. -87% to -93%.
- Bench 2/2/1: stakes are weight x 1M TRS (`VALIDATOR_STAKES`, tools/harness-kill-no-restart), power is whole TRS
  (crates/torus-genesis/src/lib.rs:376-382), so powers 2M/2M/1M. IWRR runs [A,B,C] while view mod 5M < 3M and
  [A,B] after (PM:1088-1118): the stake-1 node leads 1 view in 3 for the first ~50 h. Predicted 1.04 blk/s
  (0.81-1.51), -91% to -95%.
- TC-required adds 0.5-1 RTT per dead view (tc-required doc §5).
- matched/s: orders wait out the dead view and ride the next block, so matched/s falls less than blocks/s unless
  blocks hit their caps; p99 order latency grows by about T. Only the bench will tell.

## 2. Options

Blocks/s derived for LAN, 4 equal stakes.

| Option | One-down | All-up | Safety / agreement on leader(v) | Complexity |
|---|---|---|---|---|
| (a1) base 1200 -> 500 ms | ~3.2 | s68 A/B at 500 ms: -6.5% matched/s, 7x timeouts; WAN premature | unchanged / unchanged | trivial |
| (a2) ~250 ms timeout only for a leader whose last slot missed | ~6.9 | ~unchanged | timers only / unchanged | low |
| (b0) switch B3 reputation on | ~4.7 (floor) | stall risk | **diverges** (local inputs) | rejected |
| (b1) exclusion set in the validator set, computed by execution in the epoch plan | ~all-up after 1-2 epochs | unchanged | voting power kept / via validator-set update (CVS/PVS) | medium |
| (b2) same set, derived per block in hotstuff_rs from the justify chain | ~all-up after ~3 slots | unchanged | same / mid-epoch updates in the consensus core | medium-high |
| (c1) leader per k views | same as today unless the term ends on a TC (needs agreement on TCs) | — | — | medium |
| (c2) skip on >f "no proposal" NewViews | ~(a2) plus one message round | — | unchanged | medium |
| (d) b1 + a2 | ~all-up; the transient runs at ~6.9 | unchanged | as b1 | medium+ |

Other chains (from memory and public docs; unsure where noted):
- **HotStuff-2:** round-robin; a crashed leader costs one timeout per rotation; no reputation in the paper as far as
  known.
- **HyperBFT** (closed source): docs describe peer jailing — validators vote to jail a slow or unresponsive peer; a
  quorum of jail votes stops it voting and proposing; the validator unjails itself under on-chain rate limits.
  Unknown whether the leader schedule itself skips.
- **CometBFT:** weighted round-robin by proposer priority; an offline proposer costs `timeout_propose` (~3 s default,
  as recalled) plus a new round. Cosmos `x/slashing` jails low signers (removed from the set), `MsgUnjail` brings them
  back. Closest to b1, but also removes voting power.
- **Aptos (LeaderReputation V2):** seeded weighted-random leader from committed history (weights: active 1000,
  inactive 10, failed 1 when the failure rate > 10%), history read at round - `exclude_round` (40). Governance raised
  `exclude_round` 20 -> 40 because a ~30-deep pipeline under load made validators disagree on the leader. Lesson:
  "committed state at v-D" diverges whenever commits lag more than D views (our S470 wedge lagged tens to hundreds of
  views), so leader(v) should change only through validator-set updates.
- **MonadBFT:** stake-weighted pseudorandom leader schedule per epoch, fixed one epoch ahead; no reputation mechanism
  found; a failed leader costs one timeout and the next leader carries the TC.

## 3. Recommendation: (b1) leader exclusion through the epoch plan

### Rules

Execution keeps per-validator state in hashed META rows (new prefix `leader_stats:`): `miss_run`, exclusion count
`j`, `excluded_at`.

- **A1 Attribution.** When executing committed block B with parent P (APP:9993 receives the full Block): every view
  strictly between P.justify.view and B.justify.view is a miss for leader(m); B.justify.view is a success for its
  leader; leader(m) = `select_leader` (PM:1066) over the installed set, exclusions included (EP `epoch_vset:current`);
  gaps touching a validator-set-update block (Prepare to Decide) are not attributed; a success sets `miss_run` = 0 and
  `j` = 0.
- **A2 Exclude.** Executing boundary H writes the plan for H+L (EP:1-16): exclude every X with `miss_run` >= K = 3,
  most misses first then by key, while eligible power stays >= quorum (VS:153-165).
- **A3 Re-admit.** X returns in the plan when a committed QC from the last L heights carries X's signature
  (`SignatureSet`, types/data_types.rs:328) AND X has been excluded for >= L·2^j heights (j capped at 6).
- **A4 Apply.** Consensus at H+L reads the plan (APP:8938-8967, :8993-9029) and emits the exclusion delta with the
  validator-set update. hotstuff_rs `ValidatorSet` gains a sorted `leader_excluded` set; `select_leader` runs IWRR
  over eligible members at their own powers and falls back to all members if eligible power < quorum. Unchanged:
  voting power, quorums, `is_phase_voter`, and the schedule when the set is empty. Leader hints (APP:1373, :1496) use
  the same set.

### Determinism

Inputs are committed blocks only (justify views and signatures) and the app state their execution writes. Each
replica derives the boundary update from its own plan row, exactly as epoch rotation does today (EP:1-16), resting on
deterministic execution. The running hash covers the new rows when on. leader(v) stays a pure function of
(v, validator set state).

### Fit with TC-required

leader(v) changes only through a validator-set update. During the transition `is_proposer` accepts the CVS or PVS
leader (RO:74-82) and tips verify under leader(view) in the committed or previous set (TC doc F2). V1-V4 and L1-L4
are unchanged: exclusion changes who leads, not which certificate a leader needs. Once the dead node is excluded
there are no dead-leader views, so the TC wait disappears from the one-down case. Backup aggregator, NewView routing
and leader-skip all call `select_leader`, so they stay consistent.

### Recovered validator

On restart it votes again; the first committed QC carrying its signature satisfies A3, and it is re-admitted at the
next applied plan once the 2^j floor has passed. Caveat: the collector stops at quorum (HT:374-388); with 2/2/1 the
stake-1 vote is never needed, so it lands in a QC only when it beats A or B (question 3).

### Abuse resistance

No replica takes exclusions from a proposer; each uses the plan its own execution wrote. Framing honest X requires
its view to end with no committed certificate: the QC is aggregated by leader(m+1) and backup leader(m+2), so both
would have to be byzantine; orphaning by a later leader is closed by V1/V3 plus tip signing (TC doc §4); and it must
happen for K slots in a row. With fixed IWRR neighbours a byzantine pair right after X could repeat this (needs
n >= 7); bounded because excluded power <= total - quorum, X keeps its voting power and returns via A3, and liveness
is no worse than today. A byzantine X can still waste its own slots, as today; A3's 2^j floor makes repeats cheaper.

### Transient

After 3 misses (~6 s), the rest of the epoch plus one more: L to 2L heights, ~100-200 s at the bench's degraded
~1 blk/s and ~6-12 s at full speed. Levers: (a2), a shorter window W, or b2.

### P0.0

The hotstuff_rs `ValidatorSet` and update encodings are genesis-bound; P0.0 bits live on the app header and cannot
gate hotstuff_rs formats (same as TC doc §3). A1-A3 are execution logic and can be gated by a P0.0 feature if they
land after genesis.

**Consensus DB format 2 (s136, on the TC set).** `CONSENSUS_DB_FORMAT_VERSION` goes from 1 to 2: a format-1 build
reads a stored validator set with borsh `deserialize`, which ignores the trailing leader-excluded set (it would name
other leaders than its peers without an error), and cannot decode the `PendingWithLeaderExclusions` status tag; a
format-2 build on a format-1 store would execute committed heights without leader evidence (fail-stop) and without
`leader_stats:` history (another state than nodes that ran from genesis). Pre-genesis, so no migration: fresh data
dirs. With K = 0 the stored bytes are the format-1 bytes except the format key.

**Manifest height guard (Codex P3 on 0f3f6524).** A plain manifest starts with its height (u64 LE); heights whose
bytes spell the envelope tag (`0xffff01564543514c` is `MANIFEST_EVIDENCE_V1`) would read back as an envelope.
`manifest_value` refuses a plain manifest from height 2^48 on (and any datum shaped like an envelope); the commit
hook then writes no manifest, as on a failed write.

## 4. Test plan

- **Unit (hotstuff_rs):** `select_leader` never returns an excluded validator; empty set = identical schedule to
  today; closed-form vs reference test extended to exclusions; quorum fallback; round-trip encode/decode of the set and
  its update; `is_proposer` while an exclusion update is pending or decided.
- **Unit (execution):** A1 gaps, successes and the skip rule around validator-set updates; property test: two
  executors fed the same random chains write identical rows; A2 cap and ordering; A3 evidence, the 2^j floor and its
  reset; crash replay gives the same plan; framing cases (one withholding aggregator: no exclusion; two adjacent
  byzantine aggregators: within the cap).
- **Multi-node:** `one_down_liveness_test` with 2/2/1 (exclusion within 2L, no timeouts on the dead node's slots
  afterwards); 4 equal stakes, one down at each rotation position; restarted node re-admitted and proposing; dead node
  never re-admitted; `epoch_boundary_rejoin_test` and `multiple_validator_set_updates_test` with exclusions; TC tip
  checks across an exclusion update.
- **Mutations** (each must fail a test): attribute from local state; drop the cap; re-admit without evidence;
  `select_leader` ignores exclusions; skip the gap rule around validator-set updates.

## 5. Bench plan

- Before: the one-down baseline on main (archy, after 9j): `VALIDATOR_STAKES=2,2,1`, `KILL_NO_RESTART=val2`,
  `KILL_NO_RESTART_AT_S=120`, `BENCH_RPCS=0,1`, oracle feed off, 600 s, plus the all-up arm.
- After: the same cells on the branch, ABBA, 3 runs each.
- Report for three windows (pre-kill, kill to exclusion update, post-exclusion): blocks/s, matched/s, timeout views
  per minute, p50/p99 order latency, kill-to-exclusion time (s and heights).
- Pass: post-exclusion blocks/s >= 90% of all-up; all-up within noise; no new stalls.
- Optional: a re-admission cell (needs a delayed-restart knob) and a WAN netem cell.

## 6. Phases

| Phase | Work | Estimate |
|---|---|---|
| 1 | hotstuff_rs set and update encoding, `select_leader` filter, leader hints, unit tests | 1.5-2 d |
| 2 | Execution A1-A3, stats rows, plan and boundary update, determinism test | 2.5-3 d |
| 3 | Multi-node and mutation tests | 1.5-2 d |
| 4 | Bench before/after and report | 1 d |
| 5 (optional) | (a2) if the transient matters | 1-1.5 d |

Codex torus review after each phase. Total 6.5-8 days. Proposed order: start after TC phase B merges; land phase 1
before TC phase C, since F2 checks leader(view).

## 7a. Decisions (owner s134)

- **Approach: (b1) leader exclusion through the epoch plan — CHOSEN.** Not chosen: (a1) shorter base timeout (all-up
  regression, WAN risk), (b0) B3 reputation (diverges on local inputs), (b2) per-block exclusion in hotstuff_rs (more
  complex; revisit if the transient matters), (c1)/(c2) rotation changes (little gain or needs TC agreement).
- **Transient (Q1): accept W = L** (exclusion lands 1-2 epochs after the 3rd miss). (a2) short timeout for a leader
  that missed its last slot is the follow-up if the one-down bench shows the transient matters; (b2) stays the
  heavier alternative.
- **Possible later upgrade: (a2) short timeout for a leader that missed its last slot.** Every node gives such a
  leader ~250 ms instead of the normal 1.2-2.4 s the next time it leads; it resets after one on-time proposal. Local
  timers only (no message or format change), ~1-1.5 d. Derived effect in the transient before exclusion: ~1.5 ->
  ~6.9 blk/s (4 equal stakes, LAN). Risk: a recovered but slow (high-RTT) leader can be cut off at 250 ms (tunable,
  resets after one success). Owner s134: b1 first; a2 kept as a possible upgrade, decided after the one-down bench.
- **Parameters (Q2): genesis parameters** with defaults K = 3 and j cap = 6, range-checked at genesis validation
  (later governance-adjustable). Not chosen: hard-coded constants (each change needs a release + P0.0 activation).
- **Default (owner s138): off.** A genesis without `consensus.leader_miss_threshold` gets K = 0: no exclusion row,
  plan or validator-set update is ever written (`DEFAULT_LEADER_MISS_THRESHOLD = 0`; the j cap default stays 6,
  inert with K = 0). Supersedes the s134 default K = 3. Reason: §8 finding 4 (at 2:2:1 with the stake-1 validator
  down no validator-set update commits, so b1 with K = 3 halts that chain at its first exclusion; bench 9o runs
  exactly 2/2/1 with val2 killed). The testnet genesis sets K = 3 explicitly, and only with >= 4 validators;
  bench 9p sets `DL_K=3`. The 2:2:1 limitation stays open (finding 4 options).
- **Jailing (Q6): not part of b1.** b1 only removes leader slots; voting power and quorum math are unchanged.
  Jailing (removing voting power, HyperBFT / Cosmos style) is a separate design, to land **before mainnet or as a
  testnet upgrade so it can be tested live** (via P0.0 activation if after genesis), together with the staking /
  rewards / unjail rules. Reason to want it: with 4 validators two dead halt the chain; jailing the first restores
  the margin.

## 7. Open questions for the owner

1. Transient: keep W = L (recommended start), a shorter W, or b2? A shorter W needs execution to have applied h-W at
   every W boundary, so execution lag can stall proposals (`TORUS_PROPOSER_EXEC_WATERMARK` is off by default,
   HS:464-489).
2. K = 3 and the j cap of 6: constants or genesis parameters?
3. Re-admission evidence: QC signatures only, also committed oracle submissions, or an explicit rejoin action (unjail
   style) for low-power or distant validators?
4. IWRR coarseness: normalize powers before genesis (gcd, or smooth WRR)? Consensus-visible; changes the bench's dead
   share from 1/3 to 1/5.
5. Delete B3 reputation once this lands?
6. Excluded validators lose only proposer rewards. Acceptable? Wire `DowntimeTracker` jailing separately?
7. A per-epoch seeded rotation order, to stop fixed-neighbour framing at n >= 7?
8. Add (a2) now, or only if the bench shows the transient matters?

## 8. Phase 3 results (s138, feat/dead-leader-p3)

### Harness

- **torus-consensus `dead_leader_cluster_tests.rs`:** hotstuff_rs replicas over the in-process `ChannelNetwork`, each
  with a real `TorusApp` (state DB, consensus DB, execution pipeline, running hash), stopped, restarted and crashed
  like processes; every state DB seeded with the genesis validators (staking rows and the installed epoch set, as
  `torus-genesis` does). K = 3, L = 10, max view time 1 s. Expected heights come from the committed QC chain in a
  live replica's block tree: the update is the plan of the first boundary at or above the height whose execution
  brings the dead validator's run to K, plus L. Every test compares the running-hash write sets of the nodes that
  ran, height by height. (Torus height = hotstuff height + 1.)
- **hotstuff_rs `leader_exclusion_multi_node_test.rs`:** NumberApp `ExcludeLeader` / `ReadmitLeader` transactions,
  so the update's proposer (and its view) is chosen.

### Tests

| Test | What it proves | Status |
|---|---|---|
| `dl_cluster_one_down_at_rotation_position_{0..3}` | 4 equal stakes, one never runs: the update lands exactly at the predicted height (within 2L of the K-th miss); its old slots stop costing timeouts; its stats are the exclusion's (no miss counted after the plan); never re-admitted while down (3 more boundaries); the update's Decide and the block after it attribute nothing (gap rule); write sets agree | runs (batch C) |
| `dl_cluster_restarted_validator_is_readmitted_and_proposes` | re-admitted by the plan of the first boundary whose window holds a QC with its signature; proposes again; its success resets run and j; not excluded again | runs (batch C) |
| `dl_cluster_readmission_floor_doubles_up_to_the_cap` | cap 1, three exclusions without a success in between: (j, floor) = (1, L), (1, 2L), (1, 2L) | runs (batch C) |
| `dl_cluster_crash_with_execution_behind_replays_the_same_rows` | execution held at 6, commits past boundary L, crash: the boot replay executes from the durable headers and their evidence rows; plan rows agree | runs |
| `dl_cluster_power_loss_keeps_the_leader_evidence_in_the_manifests` | power loss drops every unexecuted height's header, body and evidence rows; the heights park on their manifests and execute with the envelopes' evidence; write sets agree | runs |
| `dl_cluster_power_loss_parked_plan_source_drains_for_the_boundary` | the same with the parked heights holding the plan source of the boundary that needs this node's vote (finding 2) | runs (K = 0) |
| `dl_cluster_mixed_k_config_diverges_in_state_and_halts_at_the_exclusion` | one node K = 0: same headers, running hashes differ from height 1; with a validator down the chain halts below the planned update, no fork | runs |
| `dl_cluster_crash_between_the_exclusion_plan_and_its_update` | a node crashes between the plan and the update | runs (batch C) |
| `dl_cluster_2_2_1_stake_one_down_is_excluded` | the bench shape | ignored, RED (finding 4) |
| hotstuff `exclusion_of_a_down_validator_commits_and_its_slots_stop_timing_out` | an exclusion update of a down validator commits under the TC rules (no TC-less proposal), power kept, its old slots stop timing out across pacemaker epoch changes | runs |
| hotstuff `readmitted_validator_proposes_again` | the excluded validator syncs back, is re-admitted and leads again | runs |
| hotstuff `{exclusion,power}_update_commits_at_2_2_1_with_the_stake_one_validator_down` | any update at 2:2:1 with the stake-1 validator down | ignored, RED (finding 4) |

Repeats: the six running multi-node tests (four cluster, two hotstuff) 5 of 5 on ubuntoz at 2d24a458; the
ignored-flaky cluster tests livelocked at the exclusion update in 3 of 24 runs on 18c; with a throwaway prototype of
the f407d49c vote rule all ten cluster tests passed 3 of 3 (18c). torus-consensus + torus-economics + hotstuff_rs at
2d24a458: 1283 of 1283 on cachyoz.

Batch C (`merge/s138-batch-c`, s138): with the reflexive lock (batch 12) and the sibling unlock rule merged, the
seven window-livelock cluster tests (the four one-down positions, re-admission, the j cap, the crash between plan
and update) passed 5 of 5 stress runs on ubuntoz and are no longer ignored. The 2:2:1 tests stay ignored (finding 4;
owner: 9p runs 4 validators).

### Findings

1. **Fixed (RED af6827cc, fix 2530c354): stale misses re-excluded a re-admitted validator.** A planned validator
   still leads until the install boundary and, dead, misses those slots; they were counted, A3 keeps the run, so it
   came back with miss_run >= K and the next plan excluded it again (j + 1) before it led once. A validator in the
   exclusion lifecycle now collects no misses.
2. **Fixed (RED be86c1f9): a power-loss-parked node never drains when its vote is needed for the boundary its
   parked heights plan.** Not dead-leader specific. After a power loss the boot parks committed heights on their
   manifests (FIX 1b / FIX 6); they drain only with the next commit (or the rank-2 tick, off by default). If that
   commit needs this node's vote on boundary block B and the parked heights hold B - L, the node answers
   `EpochPlanNotReady` forever: the chain halts (one of four already down). Now the boundary paths (validate,
   propose, vote check) dispatch what is parked; a no-op when nothing is parked.
3. **Open (owner), = fix/vsu-window-flake f407d49c: the update window livelocks when a silent leader follows the
   update block's Precommit PC.** The backup aggregator locks on the Precommit PC and refuses the block's
   re-proposal (its justify is below the lock); with one of four down every vote is needed. b1 emits its exclusion
   update exactly while a validator is down, so it hits this whenever the update is proposed two views before the
   dead slot (3 of 24 cluster runs). The vote rule f407d49c names (a proposal of the locked block satisfies the lock)
   makes every affected test pass.
4. **Open (owner), critical for phase 4: at 2:2:1 with the stake-1 validator down no validator-set update commits,
   so b1 halts the bench shape at its first exclusion.** IWRR [A, B, C] gives the dead C every third view; an update
   needs Prepare, Precommit and Commit QCs in consecutive views (`safe_nudge` 4, `repropose_block`), and three
   consecutive live views never come (for the first 3M of every 5M views at powers 2M/2M/1M). Without the vote rule
   it wedges at the first Precommit lock; with the prototype it livelocks (Prepare, Precommit, timeout,
   re-proposal: views 10 to 105 in 150 s). Pre-existing for any update (a power update wedges the same way); today
   the bench shape just never has one. Options: (a) bench a shape where the dead validator leads at most one view in
   four (4 equal stakes, or 2/2/1/1), after the vote rule; (b) let a view certified only by a TC (nothing proposed)
   not break the consecutive-views chain of an update (protocol change, needs a safety argument); (c) apply an
   exclusion-only delta without the phased protocol (it changes no voting power; agreement on leader(v) then needs
   another mechanism); (d) b2 or a2 (§2).
5. **Mixed K is neither refused nor consistent.** Nothing compares K across nodes. A K = 0 node commits the same
   headers until an exclusion is due, but its state lacks the `leader_stats:` rows from height 1, so only the
   state-hash attestation catches it. Once an exclusion is planned, the K = 3 nodes' update for that block differs
   from the K = 0 node's (Prepare vs Generic votes): with a validator down the chain halts below the update, without
   a fork. A refusal at startup would need K and the cap in something every node checks (a chain-config fingerprint
   in the genesis-bound state or the handshake): owner decision.
6. **The A2 quorum cap cannot bind on a live chain of honest-but-dead validators** (dead power <= total - quorum,
   else no QCs): no multi-node test can kill its removal; the unit tests do.

### Mutations

Each applied alone, the tests listed run (the ignored cluster tests included, on the f407d49c vote-rule
prototype so they run deterministically), restored after.

| Mutation | Killed by (multi-node) | Killed by (unit) |
|---|---|---|
| M1 attribution ignores the installed exclusions | one_down_0 | `installed_exclusions_change_the_attributed_leaders` |
| M2 A2 drops the quorum cap | none (finding 6) | `exclusion_never_takes_eligible_power_below_quorum`, `exclusion_keeps_eligible_power_at_quorum_most_misses_first` |
| M3 A3 re-admits without signature evidence | one_down_0 | `readmission_needs_signature_evidence_and_the_floor`, `third_consecutive_miss_excludes_at_the_next_plan` |
| M4 `select_leader` ignores the exclusions | one_down_0, hotstuff `exclusion_of_a_down_validator_...` | 5 `pacemaker::leader_exclusion_test` |
| M5 no gap rule (phased QCs attributed as generic) | one_down_0 (the post-update write-set check) | `phased_and_genesis_qcs_skip_attribution` |
| M6 K off by one (miss_run > K) | one_down_1, one_down_3 | 9 `leader_stats` |
| M7 the floor ignores j | readmission_floor_doubles_up_to_the_cap | `exclusion_count_saturates_at_the_cap`, `readmission_floor_doubles_until_a_success` |
| M8 j not capped | readmission_floor_doubles_up_to_the_cap | `exclusion_count_saturates_at_the_cap` |
| M9 a missing evidence row guessed (no fail-stop) | none (never missing on the cluster) | `dead_leader_missing_evidence_fail_stops` |
| M10 the manifest carries no evidence | power_loss_keeps_the_leader_evidence | `dead_leader_evidence_survives_a_power_loss_that_keeps_the_manifest`, `dead_leader_kill_after_the_manifest_heals_in_either_order`, `dead_leader_commit_writes_the_evidence_row` |
| M11 the header batch carries no evidence row | crash_with_execution_behind, power_loss_keeps_the_leader_evidence | `dead_leader_evidence_survives_with_the_header_after_the_manifest_prune` and 2 more |
| M12 dispatch ignores the manifest's evidence | power_loss_keeps_the_leader_evidence | `dead_leader_evidence_survives_a_power_loss_that_keeps_the_manifest`, `dead_leader_kill_after_the_manifest_heals_in_either_order` |
| M13 consensus ignores the exclusion plan | one_down_0 | `boundary_updates_carry_the_leader_exclusion_delta` |
| M14 execution never installs `lx_current` | one_down_0 | 8 (`dead_leader_rows_identical_*`, `leader_stats`) |
| M15 a success does not reset j | restarted_validator_is_readmitted_and_proposes | `readmission_floor_doubles_until_a_success` |
| M16 lifecycle misses counted (finding 1 reverted) | one_down_0, restarted_validator_is_readmitted_and_proposes | `misses_between_plan_and_install_do_not_re_exclude_after_readmission` |
| M17 the installed-set cache key ignores the exclusions row | one_down_0 | none |

### Phase 4 (bench) prerequisites

- The f407d49c vote rule merged (else roughly one exclusion in three livelocks the window at 4 equal stakes).
- Finding 4 decided (owner s138: b1 off by default; on only by an explicit genesis K, with >= 4 validators): the
  planned 2:2:1 `KILL_NO_RESTART=val2` cell (9o) runs with b1 off; with K set it halts at the first exclusion update. A 4-equal
  (or 2/2/1/1) one-down cell works with the vote rule.
- Then: kill-to-exclusion time (expected 1-2 epochs after the 3rd miss), blocks/s before and after.

Sources: Aptos consensus_config.rs (LeaderReputation defaults); Aptos governance proposal 12 (exclude_round 20->40);
Hyperliquid staking docs (jailing); Monad docs (MonadBFT); Chorus One, Monad architecture.
