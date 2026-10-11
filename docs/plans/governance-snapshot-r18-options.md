# R18: governance snapshot — options (review fix #4)

Status: options for an owner decision. No code. Base: main `d1b0d98d`.

Source findings: astra round 1, section 4 (`docs/audits/astra-round1-2026-09-24.md`):
"Proposal voter snapshots are incomplete" and "Quorum denominator changes after votes are cast".
Line numbers below are main `d1b0d98d`.

## 1. What the code does today

`crates/torus-economics/src/governance.rs`:

| Step | Code | Behaviour |
|---|---|---|
| Submit | `submit_proposal` :734, `snapshot_voter_weights` :1442 | Scans every row of `cf_staking_delegations` and `cf_staking_permanent` and writes one `snap ‖ id ‖ voter` row (32-byte weight) per voter with weight > 0. `snapshot_block` = the submit block. Weight = delegated + permanent × 3 / 2 (params at submit). |
| Yes / No vote | `cast_vote` :837 → `compute_vote_weight_at` :1416 | Snapshot row if present; **no row → live weight** (:1429, "pre-snapshot proposals"). |
| Abstain | `cast_vote_abstain` :900 | **Always live weight** (`compute_vote_weight` :923), never the snapshot. |
| Finalize | `finalize_proposal` :954 | Quorum = `total_staked_supply()` × `quorum_bps` / 10 000, all **live at finalize**; `quorum_bps` and `permanent_unlock_threshold_bps` from the **live** params. |
| Quorum numerator | `total_vote_weight` :1502 | Σ recorded vote weights (yes + no + abstain): **weighted** (permanent × 1.5). |
| Quorum denominator | `total_staked_supply` :1515 | Σ delegations + Σ permanent stake: **unweighted**. |

Consequences:
1. **New voters are admitted.** An address with no stake at submit (no snapshot row) delegates
   during the 7-day vote and votes with its live weight. The snapshot only blocks *more* weight
   for addresses that were already staked.
2. **Abstain bypasses the snapshot** entirely (live weight, any address).
3. **The denominator moves.** `undelegate` (`staking.rs:143`) lowers `delegation.amount` at once
   (the unbonding entry no longer counts), so voters can vote and then undelegate to shrink the
   quorum; new delegation raises it. Params changed by another proposal during the vote
   (`quorum_bps`, the unlock threshold) apply to proposals already running.
4. **Numerator and denominator are on different scales** (not in the audit): votes are weighted,
   supply is not, so permanent stakers reach quorum with up to 1.5× fewer tokens than the
   `quorum_bps` figure suggests. With the snapshot denominator below this has to be decided anyway.
5. Cost and growth (audit MED-NEW-08, noted in the code): each proposal scans all delegations and
   permanent stakes and writes one row per staker; the rows are never deleted.

Governance CFs are not in `NATIVE_ROOT_CFS`, but a different pass / fail changes executed state
(treasury spends, listings, parameters, validator whitelist, permanent unlocks). So **every option
below is a consensus rule change**: all validators must switch at the same height, and replaying
old blocks must keep the old rule. Two ways to get that:
- **Fresh genesis**: no history to replay; simplest.
- **Activation height** in the chain config (pattern: `consensus.state_hash_activation_height`,
  `torus-genesis/src/lib.rs:119`): new rule for proposals submitted at or after `H`, old rule
  before. Adds a genesis field (so a genesis change for the running chain) and two code paths kept
  forever.

## 2. Options

### A. Complete the snapshot (marker + snapshot denominator), keep the scan

At submit, besides today's weight rows, write one per-proposal **snapshot record** under a new key
(e.g. `snapmeta ‖ id` in `cf_governance_votes`): version, Σ snapshot weight (the denominator),
`quorum_bps`, `permanent_unlock_threshold_bps`.
- With the record: no weight row ⇒ weight 0 (`NoVotingWeight`); abstain uses the snapshot weight;
  finalize uses the recorded denominator and thresholds.
- Without it (proposals from before the change): today's behaviour, unchanged.
- `Proposal`'s borsh layout is unchanged (new key only), so stored proposals still decode.

Trade-offs: smallest change (governance.rs only, ~4 functions). Fixes findings 1-3 and, with the
weighted denominator, 4. Keeps the O(stakers) scan and the row growth per proposal (5). The
denominator is fixed at submit, so voters who unstake later still count in it: the right side for
"quorum of the stake that existed", and nobody can move it during the vote.

Genesis: fresh genesis **or** activation height (the record must not be written when old blocks are
replayed). No format change to existing rows.

### B. Same rules, snapshot fields in `Proposal`

As A, but the record's fields (`snapshot_version`, `quorum_supply`, `quorum_bps`,
`unlock_threshold_bps`) become `Proposal` fields.

Trade-offs: one row per proposal instead of two; `torus_getProposal` can return the snapshot with
the proposal (also closes round-2 finding 13, which notes the RPC omits `snapshot_block`). But it
changes the `Proposal` borsh layout: stored rows fail the strict decode unless a tolerant trailing
decode is added (as `GovernanceParams` does for `permanent_unlock_threshold_bps`).

Genesis: **fresh genesis** (or activation height + a tolerant decoder for the old rows).

### C. Stake checkpoints instead of a scan (ERC20Votes-style)

Every delegate / undelegate / permanent-stake change appends a `(block, weight)` checkpoint per
voter and one for the total. A vote reads the voter's checkpoint at `snapshot_block − 1`
(binary search); finalize reads the total at the same block. No per-proposal scan, no snapshot rows.

Trade-offs: fixes 1-5, and the snapshot is the state at the end of the previous block (no same-block
"delegate then propose"). Largest change: every staking write path that changes a voter's
weight (`staking.rs` delegate / undelegate / permanent stake / slashing, and any other writer of
those rows) must append a checkpoint; it adds a CF or key space and needs checkpoint pruning. Weighting is applied at read time, so a multiplier
change during a vote either applies (live) or the multiplier must also be checkpointed.

Genesis: **fresh genesis** (existing stake has no history; seeding it at an activation height is
possible but adds a migration step).

### D. Lock instead of snapshot (not recommended)

Keep live weights but block `undelegate` of stake that voted on a running proposal until it ends.
Fixes the "vote then undelegate" half of finding 3 only; new voters, abstain, rising denominator
and live params stay. Adds a cross-module dependency (staking checks governance) and a user-visible
lock. Genesis: activation height or fresh genesis.

## 3. Sub-decisions (any of A-C)

| # | Question | Options | Recommendation |
|---|---|---|---|
| S1 | Denominator scale | (a) Σ snapshot **weights** (same scale as the votes); (b) raw stake as today | (a): removes the 1.5× skew; quorum then means "33% of voting power". |
| S2 | Which params are frozen at submit | `quorum_bps`, `permanent_unlock_threshold_bps` (multiplier is already inside the weights); `timelock_blocks` | Freeze the two thresholds. Keep `timelock_blocks` live: it applies after the vote, when the result is already fixed. |
| S3 | Snapshot rows after finalize | delete at finalize (bounded by stakers, one extra batch in that block) / keep (audit trail) | Delete in A; nothing to delete in C. |
| S4 | Snapshot point | state when the submit action runs (mid-block, today) / end of previous block | Today's in A/B (simplest); previous block comes free with C. |
| S5 | Legacy proposals | keep old rule (marker / version absent) | Required for the activation-height path; moot with fresh genesis. |

## 4. Effect on existing tests

In `governance_tests.rs`, `quorum_vote_changes_the_outcome_at_the_boundary` and
`unlock_threshold_vote_changes_the_permanent_unlock_outcome` submit their second proposal after the
parameter change, so they still pass with frozen thresholds. A test that stakes after submit and then
votes would start getting `NoVotingWeight` (not searched exhaustively; the implementation's test run
will show them). With S1 (a), quorum tests that mix permanent and delegated stake change their
numbers.

## 5. Recommendation

**A with S1 (a), S2 as above, S3 delete**, behind a **fresh genesis** if one is planned anyway (it
also covers the earlier "needs fresh genesis" items); otherwise behind an activation height. It fixes
all four correctness findings with a governance.rs-only change and no change to stored rows. C is
the better long-term design (no O(stakers) scan per proposal) and is worth doing when the staking
paths are opened for another reason; it should not block R18.

Decisions needed: option (A / B / C), S1, S2, S3, and fresh genesis vs activation height.
