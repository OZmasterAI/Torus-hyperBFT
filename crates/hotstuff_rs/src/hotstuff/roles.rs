/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Functions that determine what roles a replica should play at any given View and Validator Set State.

use ed25519_dalek::VerifyingKey;

use crate::{
    pacemaker::implementation::select_leader,
    types::{data_types::ViewNumber, validator_set::ValidatorSetState},
};

use super::{
    messages::{NewView, PhaseVote},
    types::{Phase, PhaseCertificate},
};

/// Determine whether the `replica` is an "active" validator, given the current `validator_set_state`.
///
/// An active validator can:
/// - Propose/Nudge and phase vote in the HotStuff protocol,
/// - Contribute [timeout votes](crate::pacemaker::messages::TimeoutVote) and
///   [advance view messages](crate::pacemaker::messages::AdvanceView).
///
/// ## `is_validator` logic
///
/// Whether or not `replica` is an active validator given the current `validator_set_state` depends on
/// two factors:
/// 1. Whether `replica` is part of the Committed Validator Set (CVS), the Previous Validator Set (PVS),
///    both validator sets, or neither.
/// 2. Whether `validator_set_state.update_decided()` or not.
///
/// The below table specifies exactly the return value of `is_validator` in every possible combination
/// of the two factors:
///
/// ||Validator Set Update Decided|Validator Set Update Not Decided|
/// |---|---|---|
/// |Part of CVS (and maybe also PVS)|`true`|`true`|
/// |Part of PVS only|`false`|`true`|
/// |Part of neither neither CVS or PVS|`false`|`false`|
pub(crate) fn is_validator(
    replica: &VerifyingKey,
    validator_set_state: &ValidatorSetState,
) -> bool {
    validator_set_state
        .committed_validator_set()
        .contains(replica)
        || (!validator_set_state.update_decided()
            && validator_set_state
                .previous_validator_set()
                .contains(replica))
}

/// Determine whether `validator` should act as a proposer in the given `view`, given the current
/// `validator_set_state`.
///
/// ## `is_proposer` Logic
///
/// Whether or not `validator` is a proposer in `view` depends on two factors:
/// 1. Whether or not  `validator` is the [leader](select_leader) in either the CVS or the PVS in
///    `view` (it could possibly be in both), and
/// 2. Whether or not `validator_set_update.update_decided`.
///
/// The below table specifies exactly the return value of `is_proposer` in every possible combination
/// of the two factors:
///
/// ||Validator Set Update Decided|Validator Set Update Not Decided|
/// |---|---|---|
/// |Leader in CVS (and maybe also PVS)|`true`|`true`|
/// |Leader in PVS only|`false`|`true`|
/// |Leader in neither CVS or PVS|`false`|`false`|
pub(crate) fn is_proposer(
    validator: &VerifyingKey,
    view: ViewNumber,
    validator_set_state: &ValidatorSetState,
) -> bool {
    validator == &select_leader(view, validator_set_state.committed_validator_set())
        || (!validator_set_state.update_decided()
            && validator == &select_leader(view, validator_set_state.previous_validator_set()))
}

/// MonadBFT B3: Determine whether `validator` should act as proposer, using
/// reputation-weighted leader selection.
///
/// Same logic as `is_proposer` but uses `select_leader_with_reputation`
/// for the leader check. If reputation is None, falls back to standard selection.
pub(crate) fn is_proposer_with_reputation(
    validator: &VerifyingKey,
    view: ViewNumber,
    validator_set_state: &ValidatorSetState,
    reputation: Option<&crate::hotstuff::types::LeaderReputation>,
) -> bool {
    use crate::pacemaker::implementation::select_leader_with_reputation;
    match reputation {
        Some(rep) => {
            validator
                == &select_leader_with_reputation(
                    view,
                    validator_set_state.committed_validator_set(),
                    rep,
                )
                || (!validator_set_state.update_decided()
                    && validator
                        == &select_leader_with_reputation(
                            view,
                            validator_set_state.previous_validator_set(),
                            rep,
                        ))
        }
        None => is_proposer(validator, view, validator_set_state),
    }
}

/// The leader `phase_vote` is sent to: the leader of `phase_vote.view + 1` (see [`phase_vote_leader`]
/// for the validator set). MonadBFT B3: reputation-aware.
pub(crate) fn phase_vote_recipient_with_reputation(
    phase_vote: &PhaseVote,
    validator_set_state: &ValidatorSetState,
    reputation: Option<&crate::hotstuff::types::LeaderReputation>,
) -> VerifyingKey {
    phase_vote_leader(
        phase_vote,
        phase_vote.view + 1,
        validator_set_state,
        reputation,
    )
}

/// s84 liveness: the BACKUP recipient of `phase_vote`, i.e. the leader of `phase_vote.view + 2` in
/// the same validator set [`phase_vote_recipient_with_reputation`] picks from; `None` when that is
/// the primary recipient itself.
///
/// Votes for view `w` are aggregated by `leader(w + 1)`. If that leader is down, the block of view
/// `w` is never certified; with three round-robin validators and one down that is every third
/// block, so two consecutive-view QCs (the 2-chain commit rule) never form and nothing commits even
/// though the live validators hold a quorum. Also sending the vote to `leader(w + 2)` lets the next
/// live leader certify the block (it is still in view `w` when the votes arrive: view `w` cannot
/// end without that QC or a timeout), so the chain commits with one leader down. A QC is valid
/// whoever assembles it, so no safety rule changes; with every leader up the backup assembles the
/// same QC as the primary and the later copy is ignored.
///
/// Rolling upgrade: no activation gate is needed. Only the SEND side changed; the receive side
/// already collects a correctly signed vote for the current view from any validator, whoever the
/// intended collector was (`on_receive_phase_vote` and `PhaseVoteCollector::collect` check chain,
/// view, signature and validator-set membership only), and the network layer does not penalise an
/// extra well-formed direct message. Old nodes therefore act as backups too; they just never send
/// backup votes, so the backup forms a QC only once a quorum of the voting power has upgraded.
pub(crate) fn phase_vote_backup_recipient_with_reputation(
    phase_vote: &PhaseVote,
    validator_set_state: &ValidatorSetState,
    reputation: Option<&crate::hotstuff::types::LeaderReputation>,
) -> Option<VerifyingKey> {
    let primary = phase_vote_recipient_with_reputation(phase_vote, validator_set_state, reputation);
    let backup = phase_vote_leader(
        phase_vote,
        phase_vote.view + 2,
        validator_set_state,
        reputation,
    );
    (backup != primary).then_some(backup)
}

/// Leader of `leader_view` in the validator set whose quorum `phase_vote` counts towards, which is
/// either the committed validator set (CVS) or the previous validator set (PVS) depending on whether
/// `validator_set_state.update_decided()` and on `phase_vote.phase`:
///
/// ||Validator Set Update Decided|Validator Set Update Not Decided|
/// |---|---|---|
/// |Phase == `Generic`, `Prepare`, `Precommit`, or `Commit`|CVS|PVS|
/// |Phase == `Decide`|CVS|CVS|
///
/// Reputation-weighted when `reputation` is `Some` (which respects the reputation kill switch), plain
/// [`select_leader`] otherwise.
fn phase_vote_leader(
    phase_vote: &PhaseVote,
    leader_view: ViewNumber,
    validator_set_state: &ValidatorSetState,
    reputation: Option<&crate::hotstuff::types::LeaderReputation>,
) -> VerifyingKey {
    use crate::pacemaker::implementation::select_leader_with_reputation;
    let validator_set = if validator_set_state.update_decided() || phase_vote.phase == Phase::Decide
    {
        validator_set_state.committed_validator_set()
    } else {
        validator_set_state.previous_validator_set()
    };
    match reputation {
        Some(rep) => select_leader_with_reputation(leader_view, validator_set, rep),
        None => select_leader(leader_view, validator_set),
    }
}

/// Determine whether or not `replica` should phase-vote for the `Proposal` or `Nudge` that `justify`
/// was taken from. This depends on whether `replica`'s `PhaseVote`s can become part of quorum
/// certificates that directly extend `justify`.
///
/// If this predicate evaluates to `false`, then it is fruitless to vote for the `Proposal` or `Nudge`
/// that `justify` was taken from, since according to the protocol, the next leader will ignore the
/// replica's phase votes anyway.
///
/// ## `is_phase_voter` Logic
///
/// `replica`'s phase vote can become part of PCs that directly extend `justify` if-and-only-if
/// `replica` is part of the appropriate validator set in the `validator_set_state`, which is either the
/// Committed Validator Set (CVS), or the Previous Validator Set (PVS). In turn, which of CVS and PVS is
/// the appropriate validator set depends on two factors:
/// 1. Whether or not `validator_set_update.update_decided()`, and
/// 2. What `justify.phase` is:
///
/// The below table specifies which validator set `replica` must be in order to be a voter in every
/// possible combination of the two factors:
///
/// ||Validator Set Update Decided|Validator Set Update Not Decided|
/// |---|---|---|
/// |Phase == `Generic`, `Prepare`, `Precommit`, or `Decide`|CVS|PVS|
/// |Phase == `Commit`|CVS|CVS|
///
/// ## Preconditions
///
/// `justify` satisfies [`safe_pc`](crate::block_tree::invariants::safe_pc) and
/// [`is_correct`](crate::types::signed_messages::Certificate::is_correct), and the block tree updates
/// associated with this `justify` have already been applied.
pub(crate) fn is_phase_voter(
    replica: &VerifyingKey,
    validator_set_state: &ValidatorSetState,
    justify: &PhaseCertificate,
) -> bool {
    if validator_set_state.update_decided() {
        validator_set_state
            .committed_validator_set()
            .contains(replica)
    } else {
        match justify.phase {
            Phase::Generic | Phase::Prepare | Phase::Precommit | Phase::Decide => {
                validator_set_state
                    .previous_validator_set()
                    .contains(replica)
            }
            Phase::Commit => validator_set_state
                .committed_validator_set()
                .contains(replica),
        }
    }
}

/// Identify the leader(s) that `new_view` should be sent to, given the current `validator_set_state`.
///
/// ## `new_view_recipients` Logic
///
/// Upon exiting a view, a replica should send a `new_view` message to the leader of `new_view.view + 1`
/// in the committed validator set (CVS), *and*, if `!validator_set_state.update_decided` (not decided),
/// *also* to the leader of the same view in the previous validator set (PVS).
///
/// ## Return value
///
/// Returns a pair containing the following items:
/// 1. `VerifyingKey`: the leader in the committed validator set in `new_view.view + 1`.
/// 2. `Option<VerifyingKey>`: the leader in the resigning validator set in `new_view.view + 1` (`None`
///    if the most recently initiated validator set update has been decided).
#[allow(dead_code)]
pub(crate) fn new_view_recipients(
    new_view: &NewView,
    validator_set_state: &ValidatorSetState,
) -> (VerifyingKey, Option<VerifyingKey>) {
    (
        select_leader(
            new_view.view + 1,
            validator_set_state.committed_validator_set(),
        ),
        if validator_set_state.update_decided() {
            None
        } else {
            Some(select_leader(
                new_view.view + 1,
                validator_set_state.previous_validator_set(),
            ))
        },
    )
}

/// Leader of `view` in a single validator set, using the same reputation-aware
/// selection the rest of the protocol runs: `select_leader_with_reputation`
/// when reputation state exists (which internally respects the
/// [`set_reputation_leader_selection`](crate::pacemaker::implementation::set_reputation_leader_selection)
/// kill switch), plain [`select_leader`] when it does not.
///
/// Surfaced on [`StartViewEvent`](crate::events::StartViewEvent) so external
/// observers (the RPC leader hint) see the pacemaker's actual choice.
pub(crate) fn view_leader_with_reputation(
    view: ViewNumber,
    validator_set: &crate::types::validator_set::ValidatorSet,
    reputation: Option<&crate::hotstuff::types::LeaderReputation>,
) -> VerifyingKey {
    use crate::pacemaker::implementation::select_leader_with_reputation;
    match reputation {
        Some(rep) => select_leader_with_reputation(view, validator_set, rep),
        None => select_leader(view, validator_set),
    }
}

/// FIX CONS-PF-10: Reputation-weighted new_view recipients.
pub(crate) fn new_view_recipients_with_reputation(
    new_view: &NewView,
    validator_set_state: &ValidatorSetState,
    reputation: Option<&crate::hotstuff::types::LeaderReputation>,
) -> (VerifyingKey, Option<VerifyingKey>) {
    use crate::pacemaker::implementation::select_leader_with_reputation;
    let select = |view, vs: &crate::types::validator_set::ValidatorSet| match reputation {
        Some(rep) => select_leader_with_reputation(view, vs, rep),
        None => select_leader(view, vs),
    };
    (
        select(
            new_view.view + 1,
            validator_set_state.committed_validator_set(),
        ),
        if validator_set_state.update_decided() {
            None
        } else {
            Some(select(
                new_view.view + 1,
                validator_set_state.previous_validator_set(),
            ))
        },
    )
}

#[cfg(test)]
mod view_leader_tests {
    use super::*;
    use crate::hotstuff::types::LeaderReputation;
    use crate::pacemaker::implementation::select_leader_with_reputation;
    use crate::types::{
        data_types::Power, update_sets::ValidatorSetUpdates, validator_set::ValidatorSet,
    };
    use ed25519_dalek::SigningKey;

    fn make_validator_set(n: u8) -> (ValidatorSet, Vec<VerifyingKey>) {
        let vks: Vec<VerifyingKey> = (0..n)
            .map(|i| SigningKey::from_bytes(&[i + 1; 32]).verifying_key())
            .collect();
        let mut vs = ValidatorSet::new();
        let mut updates = ValidatorSetUpdates::new();
        for vk in &vks {
            updates.insert(*vk, Power::new(1));
        }
        vs.apply_updates(&updates);
        (vs, vks)
    }

    /// The single-VK view-leader helper (surfaced on `StartViewEvent` for the
    /// RPC leader hint) must be byte-for-byte the protocol's own selection:
    /// `select_leader_with_reputation` when reputation state exists, plain
    /// `select_leader` when it does not. Covers views on both sides of the
    /// reputation warm-up boundary (view 20).
    #[test]
    fn view_leader_with_reputation_matches_protocol_selection() {
        let (vs, vks) = make_validator_set(4);
        // Skewed reputation: vks[0] times out constantly.
        let mut rep = LeaderReputation::new(100);
        for _ in 0..10 {
            rep.record_timeout(&vks[0]);
            rep.record_success(&vks[1]);
            rep.record_success(&vks[2]);
            rep.record_success(&vks[3]);
        }
        for v in 0..40u64 {
            let view = ViewNumber::new(v);
            assert_eq!(
                view_leader_with_reputation(view, &vs, Some(&rep)),
                select_leader_with_reputation(view, &vs, &rep),
                "view {v}: Some(rep) must delegate to select_leader_with_reputation"
            );
            assert_eq!(
                view_leader_with_reputation(view, &vs, None),
                select_leader(view, &vs),
                "view {v}: None must delegate to plain select_leader"
            );
        }
    }

    /// s84: a phase vote for view `w` goes to `leader(w + 1)` and, as backup, to
    /// `leader(w + 2)`; no backup when both views have the same leader.
    #[test]
    fn backup_vote_recipient_is_the_leader_two_views_ahead() {
        use crate::hotstuff::messages::PhaseVote;
        use crate::types::{
            crypto_primitives::Keypair,
            data_types::{ChainID, CryptoHash},
        };
        let (vs, _) = make_validator_set(3);
        let vss = ValidatorSetState::new(vs.clone(), vs.clone(), None, true);
        let kp = Keypair::new(SigningKey::from_bytes(&[1; 32]));
        let vote_at = |v: u64| {
            PhaseVote::new(
                &kp,
                ChainID::new(0),
                ViewNumber::new(v),
                CryptoHash::new([0; 32]),
                Phase::Generic,
            )
        };
        for v in 0..12u64 {
            let vote = vote_at(v);
            assert_eq!(
                phase_vote_recipient_with_reputation(&vote, &vss, None),
                select_leader(ViewNumber::new(v + 1), &vs)
            );
            assert_eq!(
                phase_vote_backup_recipient_with_reputation(&vote, &vss, None),
                Some(select_leader(ViewNumber::new(v + 2), &vs)),
                "view {v}: three round-robin leaders, backup is leader(v + 2)"
            );
        }

        // A single validator leads every view: the backup would be the primary.
        let (solo, _) = make_validator_set(1);
        let solo_vss = ValidatorSetState::new(solo.clone(), solo, None, true);
        assert_eq!(
            phase_vote_backup_recipient_with_reputation(&vote_at(5), &solo_vss, None),
            None
        );
    }
}
