# Validator-set membership sync: what the D2 fix leaves open (s138)

**Status: design, s138. No code.** Follows `docs/plans/disjoint-vset-livelock.md` (D2, §3.3). Code read on
`fix/vset-membership-sync` `9942394b` (D2 + D3 fix, Codex adversarial: approve). Line numbers are that commit's.

Notation as in the parent doc: an update U at height h(U) changes `S_old` to `S_new`. `P_old`, `P_new` are total powers,
`q = floor(2P/3) + 1` the quorums, `f_old = P_old - q_old` the old fault bound (`types.rs:289-294`). A **laggard** is any
replica that holds `S_old` as its committed set while U is decided: an old member that missed the window, a joiner that
was offline in the window, or a node syncing from genesis.

## 1. What the D2 fix does and what it cannot do

A laggard checks U's Decide PC (carried as the justify of U's child) in `PhaseCertificate::is_correct`
(`crates/hotstuff_rs/src/hotstuff/types.rs:50-125`). With U Pending it now goes to
`is_correct_above_latest_update` (`:194-212`) and `check_decide_on_pending_update` (`:269-300`):

1. U extends the committed block through blocks that update nothing (`extends_committed_block_without_updates`,
   `:312-340`);
2. the PC is correctly signed for `S_old ⊕ U.updates`;
3. the signers' power **in `S_old`** is above `f_old` (`:283-299`), so at least one honest old member voted Decide, which
   it does only after a correct `S_old` Commit PC on U.

Rule 3 is what makes the check safe (without it one Byzantine sync server forks a laggard, test T7b,
`types.rs` `decide_pc_on_pending_update_needs_an_old_set_signer`). It is also what leaves these cases open:

| Case | Example | Why it fails | Today on the fix branch |
|---|---|---|---|
| **A. Disjoint replacement** | step 4 of `multiple_validator_set_updates_test`: {n0..n3} -> {n4, n5} | no `S_old` member signs Decide (`C = ∅`), old power 0 ≤ f_old | `is_unprovable_decide_on_pending_update` (`types.rs:247-267`) is true; block sync cools the server down instead of blacklisting it (`block_sync/client.rs:436-463`). The laggard never commits U. |
| **B. Large power shift** | T6b 1,1,1,1 -> 1,1,1,10 (`tests/validator_set_membership_sync_test.rs:463-464`, `#[ignore]`) | q_new = 9: n3 alone (new power 10) is a Decide quorum; its old power 1 = f_old | same as A |
| **B'. Unlucky quorum** | any update where *some* Decide quorum holds ≤ f_old of old power, even if most do not | the collector forms the PC from the first quorum to arrive (`hotstuff/types.rs:491-525`), so whether the laggard can cross depends on vote arrival order | same as A, nondeterministic per update |
| **C. No admissible sync server** | the whole genesis set has been replaced and every genesis node is stopped; a fresh node starts from genesis | the client registers a sync server only if it is in the client's committed set, or is inserted by a Pending update in its tree (`is_sync_server_address`, `client.rs:1748-1781`, called from `on_receive_advertise_block`, `:653`). `S_new` members are admitted only once U is in the tree, and U can come only from an admitted server | sync never starts. Independent of A/B: it also hits gradual rotations once every member of the client's set is gone |
| **D. Sync from genesis must cross every update** | a node joining after k updates | no state sync (`docs/pruning-guide.md:45, 69-74`): it crosses U1..Uk one by one, each by rule 3 or not at all; past the retention window (100,000 blocks) it also needs an archive validator, which must be admissible (C) | each A/B/B' update in the history is a permanent wall for every later node |

The missing evidence in A/B/B' is U's **Commit PC**, signed by a quorum of `S_old`. Every replica that commits U live
holds it for a moment: it commits on that PC (`block_tree/accessors/internal.rs` `update` `:336-372`, commit at `:359`;
`commit` `:716-860`). Nothing stores it: the DB keeps `HIGHEST_PC` / `LOCKED_PC` (`block_tree/variables.rs:145-147`),
which move on, and blocks carry only their parent's justify (U's child carries the Decide PC, not the Commit PC). Block
sync sends blocks plus the server's current `highest_pc` (`block_sync/server.rs:121-147`, `messages.rs` `BlockSyncResponse`).
Once the window nudges stop, U's Commit PC exists nowhere.

Other carriers do not help a laggard either: `AdvertisePC` from an `S_new` server fails `is_correct` at the laggard
(`client.rs:690`, PC on a block it does not have, checked against `S_old`), so only the 60 s timeout trigger starts sync.

## 2. Options

| # | Option | Genesis-bound? | DB format | Message / block format | P0.0-gateable | Covers | Size |
|---|---|---|---|---|---|---|---|
| 1 | **Store U's Commit PC; sync server ends a batch at U and sends it as the terminal PC** | not a format break, but **history-bound**: only updates committed after it lands get a record | additive key, no `CONSENSUS_DB_FORMAT` bump needed | none (reuses `BlockSyncResponse.highest_pc`) | n/a (no consensus rule changes) | A, B, B', D (with an admissible server) | S-M: ~200 lines + tests |
| 2a | Carry U's Commit PC in U's child (new `Block` field or header field) | yes | block encoding | `Block` / header / hash preimage | no (hotstuff_rs formats, `tc-required-and-tip-signing.md:221-223`) | A, B, B', D, and the proof survives in every block server | M-L |
| 2b | Dedicated sync message `UpdateCertificateRequest(height) -> Commit PC` | wire: yes (borsh enum variant; old nodes drop it) | same as 1 | new `BlockSyncMessage` variants | no | same as 1 | M |
| 2c | Joint Decide: the Decide PC needs a quorum of both sets (old members keep voting in the window) | yes | PC shape | new PC with two signature sets, new collectors | no | A, B, B' | L; window liveness then also depends on `S_old` |
| 3a | **Power-overlap rule in the torus epoch plan**: every update keeps `G < q_new - f_old` (below) | consensus-visible (changes which updates the plan emits) | none | none | **yes** (execution logic, height-gated) | makes A, B, B' impossible for torus | S-M: `torus-economics/src/epoch.rs` cap by power + tests |
| 3b | Same rule as a hotstuff_rs validity / vote rule on updating blocks | yes (vote rule) | none | none | no | same, also for non-torus apps | M |
| 4 | Checkpoint / state sync with a signed validator-set history (+ weak-subjectivity checkpoint) | protocol can come later; needs the set-change records from genesis (option 1's key) | snapshot format | new sync protocol | partly | A-D, and the pruning window | L (mainnet) |
| 5 | **Sync-server admission fallback**: if no admissible server is known, accept `AdvertiseBlock` from any peer (or from configured bootstrap peers) | no | none | none | n/a (local) | C | S |

### Option 1 in detail (recommended)

- **Store.** In `update` (`internal.rs:336-372`), when `justify.phase` is Commit and `commit()` committed an updating
  block equal to `justify.block`, write `justify` under a new never-pruned variable, e.g.
  `VALIDATOR_SET_UPDATE_RECORDS [26] ++ height(U)` -> `(U hash, U.updates, Commit PC)`. One record per update, a few
  KB each. The block-tree pruner must not delete it. It applies to every path that commits U on a Commit PC: nudge,
  NewView, a locally collected PC, and the sync terminal PC below, so a node that crossed U by option 1 can serve U too.
  A node that crossed U by the D2 Decide rule has no record; it does not need one, because the Decide check is
  deterministic (it depends only on `S_old` and the Decide PC in U's child), so every client crosses U the same way
  from it.
- **Serve.** In `server.rs:121-147`, if `blocks_from_height_to_newest(start, limit)` contains an updating block U with
  a record, cut the batch after U and send U's stored Commit PC as `highest_pc`. Without a record, serve as today.
  Cost: one extra round trip per update in the range.
- **Client: no change.** The batch ends at U. `apply_response_highest_pc` (`client.rs:617-640`) checks the terminal
  PC with `is_correct` (U is Pending, height above `update_height`, a Commit PC: checked against the committed set
  `S_old`, `types.rs:76-84`) and `safe_pc` (`block_tree/invariants.rs:382-398`), then `update` commits U. The next
  batch starts at U's child; its Decide PC now hits the `Committed` arm (`types.rs:93-96`) and checks against `S_new`.
  A wrong terminal PC is ignored there without a blacklist, so pre-option-1 clients also accept these responses.
  Truncated batches are not treated as a shortfall: `check_commitment_and_end_session` (`client.rs:1011-1035`) runs
  only on an empty response or an error.
- **Safety sketch.** The client trusts nothing new. A Commit PC on U signed by a quorum of the client's committed set
  is the same evidence on which every live replica commits U; the client's `S_old` is correct by induction from
  genesis. A server can withhold the record (liveness: another server or the Decide rule), but cannot forge one without
  a quorum of `S_old` keys. A Commit PC on a conflicting U* at the same height is already a safety violation of `S_old`.
  The one exposure is shared with the D2 rule and with genesis sync in general: **long-range attacks**. Once `S_old`
  keys are retired (unbonded, then sold or leaked), a quorum of them can sign a fake history for a fresh node. That is
  answered by a weak-subjectivity checkpoint (a trusted recent block hash in the node config, option 4), not here.
- **Tests.** Un-ignore T6b (`laggard_crosses_a_large_power_change`); add T11 a disjoint replacement with an old-set
  laggard, T12 a fresh node started after a disjoint replacement (old nodes still running), a server unit test (the
  batch ends at U and the terminal PC is U's Commit PC; no record means an unchanged response), and a pruner test (the
  record survives pruning of U).

### Option 3a: the overlap rule

Gain `g(v) = max(0, P_new(v) - P_old(v))` (a joiner gains all its power), `G = Σ g(v)` over `S_new`. Any Decide quorum
X has `P_old(X) ≥ P_new(X) - G ≥ q_new - G`. So **`G < q_new - f_old`** guarantees that every Decide quorum holds more
than `f_old` of old power, and the D2 rule always passes (A, B and B' cannot be decided). The rule is sufficient, not
necessary. Checks:

- 4 equal, add 1 or 2 equal: G = 1 or 2 < 4 - 1 or 5 - 1. Passes.
- `3k` equal, swap k (the member rotation cap `n/3`, `epoch.rs:140-147`): G = k < (2k+1) - (k-1) = k + 2. Passes. Every
  equal-power update the current cap allows passes.
- T6b 1,1,1,1 -> 1,1,1,10: G = 9, q_new - f_old = 9 - 1 = 8. Refused.
- Disjoint: G = P_new ≥ q_new. Refused.
- Removals and jailing (G = 0): need q_new > f_old, which holds while the removed power is at most f_old.

The torus cap counts members, not power (`apply_rotation_cap`, `crates/torus-economics/src/epoch.rs:149-215`), and
`epoch_validator_set_updates` (`crates/torus-consensus/src/app.rs:9744-9792`) emits plan powers unchanged. 3a would
defer arrivals and clamp power increases (`power_e = min(stake, power_{e-1} + allowance)`) until G fits, so a large
stake becomes voting power over several epochs. That is an economics choice (voting power temporarily below stake) for
the owner.

## 3. What matters when

| Event | Case | Testnet | Mainnet |
|---|---|---|---|
| Owner adds or removes a validator with equal power | none of A/B/B' (the cap allows at most n/3 changes; the checks above pass) | D2 fix is enough | same |
| Owner adds a validator with large stake, or a stake top-up / delegation raises one validator's power a lot in one epoch | B, B' | **possible**: stakes are open (`testnet-genesis-checklist.md:32`), power = whole TRS, the cap ignores power | likely (open delegation) |
| Disjoint replacement in one update | A | not reachable through torus epochs (member cap n/3); reachable in hotstuff_rs tests / other apps | same |
| Dead-leader exclusions (b1) | none (same members and powers; the D2 rule passes with all signers) | fine | fine |
| Jailing (pre-mainnet, `testnet-genesis-checklist.md:24`) | none (G = 0, jailed power ≤ f_old) | n/a | fine |
| A node joins after the genesis set has rotated out and the genesis nodes are stopped | C | possible late in the testnet (rotation over many epochs) | certain |
| A node joins after the retention window | D + C | needs an archive validator (`pruning-guide.md:67-74`) that is admissible | needs option 4 |

## 4. Recommendation

**Before genesis (formats are free now; option 1 is history-bound):**

1. **Option 1** (store and serve U's Commit PC). It changes no message, block or vote rule, so it is low-risk. It is
   the only option whose value is lost if it lands late: updates committed before it have no record anywhere, so any
   A/B/B' update before it lands blocks every later laggard and genesis-syncing node for good. Optionally bump
   `CONSENSUS_DB_FORMAT_VERSION` to 2 to mark stores that keep records. Not required, because the key is additive.
2. **Option 3a** (power-overlap cap in the epoch plan): recommended for the genesis, decided with the stakes row of
   the checklist. It is cheap, it makes the D2 Decide rule always sufficient for torus updates (no dependence on which
   servers hold records), and it can still be added later through P0.0. If equal stakes are chosen and the owner
   avoids big top-ups, it can wait.

**After genesis (local, not genesis-bound):**

3. **Option 5** (sync-server admission fallback), before the genesis set has fully rotated out. Until then, testnet
   ops rule: keep at least one genesis validator node (archive) running. To check: whether the torus network layer
   delivers hotstuff messages from non-validator peers (`crates/torus-network`).

**Mainnet:**

4. **Option 4** (checkpoint / state sync with a signed validator-set history and a weak-subjectivity checkpoint),
   seeded by option 1's records. Note: hotstuff_rs blocks do not bind `updates` into the block hash (torus derives them
   from the epoch plan in state), so a light verifier of the set history also needs the state root of the plan, or a
   committed next-set hash.

**Not recommended:** 2a, 2b, 2c, 3b. Each is genesis-bound, and option 1 gives the same coverage without a format change.
