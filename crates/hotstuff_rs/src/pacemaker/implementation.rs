/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Event-driven implementation of the Pacemaker subprotocol.
//!
//! Main type: [`Pacemaker`].

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::mpsc::Sender,
    time::{Duration, Instant, SystemTime},
};

use ed25519_dalek::VerifyingKey;

use crate::{
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton, BlockTreeWriteBatch},
        pluggables::KVStore,
    },
    events::{
        AdvanceViewEvent, CollectTCEvent, Event, ReceiveAdvanceViewEvent, ReceiveTimeoutVoteEvent,
        TimeoutVoteEvent, UpdateHighestTCEvent, ViewTimeoutEvent,
    },
    hotstuff::roles::is_validator,
    networking::{messages::Message, network::Network, sending::SenderHandle},
    pacemaker::{
        messages::{AdvanceView, PacemakerMessage, ProgressCertificate, TimeoutVote},
        types::TimeoutVoteCollector,
    },
    types::{
        crypto_primitives::Keypair,
        data_types::{ChainID, EpochLength, ViewNumber},
        signed_messages::{ActiveCollectorPair, Certificate, SignedMessage},
        validator_set::{ValidatorSet, ValidatorSetState},
    },
};

/// A single participant in the Pacemaker subprotocol.
///
/// # Usage
///
/// After creating an instance of the `Pacemaker` struct using [`new`](Self::new), the caller interacts
/// with it by calling three methods:
///
/// After creating an instance of `Pacemaker` using [`new`](Self::new), the caller should interact with
/// it by calling three methods:
/// 1. [`on_receive_msg`](Self::on_receive_msg): this method should be called whenever a `PacemakerMessage`
///    is received satisfying the method's [preconditions](Self::on_receive_msg#preconditions).
/// 2. [`tick`](Self::tick): this method should be called *as often as is practical*.
/// 3. [`query`](Self::query): whenever `on_receive_msg` or `tick` is called, the internal view counter of
///    the `Pacemaker` may be updated. The caller should call `query` whenever it needs to see this
///    counter.
pub(crate) struct Pacemaker<N: Network> {
    config: PacemakerConfiguration,
    state: PacemakerState,
    view_info: ViewInfo,
    sender: SenderHandle<N>,
    event_publisher: Option<Sender<Event>>,
}

impl<N: Network> Pacemaker<N> {
    /// Create a new `Pacemaker` instance..
    pub(crate) fn new(
        config: PacemakerConfiguration,
        sender: SenderHandle<N>,
        init_view: ViewNumber,
        init_validator_set_state: &ValidatorSetState,
        event_publisher: Option<Sender<Event>>,
    ) -> Result<Self, PacemakerError> {
        let state = PacemakerState::initialize(&config, init_view, init_validator_set_state);
        let timeout = state
            .timeouts
            .get(&init_view)
            .ok_or(UpdateViewError::GetViewTimeoutError { view: init_view })?;
        let view_info = ViewInfo::new(init_view, *timeout);
        Ok(Self {
            config,
            state,
            view_info,
            sender,
            event_publisher,
        })
    }

    /// Query the Pacemaker for its current `ViewInfo`.
    pub(crate) fn query(&self) -> &ViewInfo {
        &self.view_info
    }

    /// Cause the Pacemaker to check the current time ("clock tick"), possibly updating its internal state
    /// or causing it to send messages to other replicas.
    pub(crate) fn tick<K: KVStore>(
        &mut self,
        block_tree: &BlockTreeSingleton<K>,
    ) -> Result<(), PacemakerError> {
        let cur_view = self.view_info.view;
        let validator_set_state = block_tree.validator_set_state()?;

        // 1. Check if the current view has timed out.
        if Instant::now() > self.view_info.deadline {
            Event::ViewTimeout(ViewTimeoutEvent {
                timestamp: SystemTime::now(),
                view: cur_view,
            })
            .publish(&self.event_publisher);

            // S470 wedge diagnostics: the per-timeout (view − qc) and
            // (qc − committed) series confirm (or rule out) the commit-wedge
            // mechanism live: during the wedge, view − qc stays small while
            // qc − committed races ahead.
            if crate::logging::wedge_diag_enabled() {
                let highest_pc = block_tree.highest_pc()?;
                let locked_pc = block_tree.locked_pc()?;
                let committed_view = block_tree.committed_qc_view()?;
                log::info!(
                    "wedge_diag view_timeout: view={} highest_pc_view={} locked_pc_view={} committed_view={} locked_block_prefix={}",
                    cur_view.int(),
                    highest_pc.view.int(),
                    locked_pc.view.int(),
                    committed_view.int(),
                    crate::logging::block_prefix(&locked_pc.block),
                );
            }

            // 1.1. If the current view is an Epoch-Change view, broadcast a `TimeoutVote`, then extend the view.
            if is_epoch_change_view(&cur_view, self.config.epoch_length) {
                if is_validator(&self.config.keypair.public(), &validator_set_state) {
                    let pacemaker_message = PacemakerMessage::timeout_vote(
                        &self.config.keypair,
                        self.config.chain_id,
                        cur_view,
                        block_tree.highest_tc()?,
                        block_tree.local_tip()?,
                        block_tree.highest_qc_for_timeout()?,
                    );
                    self.sender
                        .broadcast(Message::from(pacemaker_message.clone()));
                    if let PacemakerMessage::TimeoutVote(timeout_vote) = pacemaker_message {
                        Event::TimeoutVote(TimeoutVoteEvent {
                            timestamp: SystemTime::now(),
                            timeout_vote,
                        })
                        .publish(&self.event_publisher)
                    }
                }

                // We extend the view timeout so that we will broadcast a `TimeoutVote` when this view times out again.
                self.extend_view(block_tree.highest_pc()?.view, block_tree.committed_qc_view()?)?

            // 1.2. MonadBFT: broadcast TimeoutVote before advancing on normal view timeout.
            } else {
                if is_validator(&self.config.keypair.public(), &validator_set_state) {
                    let pacemaker_message = PacemakerMessage::timeout_vote(
                        &self.config.keypair,
                        self.config.chain_id,
                        cur_view,
                        block_tree.highest_tc()?,
                        block_tree.local_tip()?,
                        block_tree.highest_qc_for_timeout()?,
                    );
                    self.sender
                        .broadcast(Message::from(pacemaker_message.clone()));
                    if let PacemakerMessage::TimeoutVote(timeout_vote) = pacemaker_message {
                        Event::TimeoutVote(TimeoutVoteEvent {
                            timestamp: SystemTime::now(),
                            timeout_vote,
                        })
                        .publish(&self.event_publisher)
                    }
                }
                self.update_view(
                    cur_view + 1,
                    &validator_set_state,
                    block_tree.highest_pc()?.view,
                    block_tree.committed_qc_view()?,
                )?;
            }

            return Ok(());
        }

        // 2. Update the Pacemaker's timeout vote Collector Pair if the current active validator sets have
        // changed.
        let _ = self
            .state
            .timeout_vote_collectors
            .update_validator_sets(&validator_set_state);

        // 3. If the current Highest PC in the block tree is for the current view or a higher view, and we have
        //    not sent an AdvanceView message in the current view, broadcast a new one with the Highest PC.
        if block_tree.highest_pc()?.view >= cur_view
            && !block_tree.highest_pc()?.is_genesis_pc()
            && is_validator(&self.config.keypair.public(), &validator_set_state)
            && (self.state.last_advance_view.is_none()
                || self.state.last_advance_view.is_some_and(|v| v < cur_view))
        {
            let pacemaker_message = PacemakerMessage::advance_view(
                ProgressCertificate::PhaseCertificate(block_tree.highest_pc()?),
            );
            self.sender
                .broadcast(Message::from(pacemaker_message.clone()));
            if let PacemakerMessage::AdvanceView(advance_view) = pacemaker_message {
                Event::AdvanceView(AdvanceViewEvent {
                    timestamp: SystemTime::now(),
                    advance_view,
                })
                .publish(&self.event_publisher)
            }

            // Record in the internal state that we have broadcasted an `AdvanceView` message in this view.
            self.state.last_advance_view = Some(self.view_info.view);
        }

        Ok(())
    }

    /// Execute the required steps in the Pacemaker subprotocol upon receiving a `PacemakerMessage` from the
    /// replica identified by `origin`.
    ///
    /// # Precondition
    ///
    /// [`msg.view()`](PacemakerMessage::view) must be greater than or equal to the current view.
    pub(crate) fn on_receive_msg<K: KVStore>(
        &mut self,
        msg: PacemakerMessage,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), PacemakerError> {
        match msg {
            PacemakerMessage::TimeoutVote(timeout_vote) => {
                self.on_receive_timeout_vote(timeout_vote, origin, block_tree)?
            }
            PacemakerMessage::AdvanceView(advance_view) => {
                self.on_receive_advance_view(advance_view, origin, block_tree)?
            }
        }
        Ok(())
    }

    /// Execute the required steps in the Pacemaker subprotocol upon receiving a `TimeoutVote` message from
    /// the replica identified by `origin`.
    ///
    /// # Precondition
    ///
    /// `timeout_vote.view >= self.query().view`
    fn on_receive_timeout_vote<K: KVStore>(
        &mut self,
        timeout_vote: TimeoutVote,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), PacemakerError> {
        Event::ReceiveTimeoutVote(ReceiveTimeoutVoteEvent {
            timestamp: SystemTime::now(),
            origin: *origin,
            timeout_vote: timeout_vote.clone(),
        })
        .publish(&self.event_publisher);

        // 1. If the sending replica is not a validator, ignore its TimeoutVote.
        //
        // TODO: even though as a non-validator we do not need to collect `TimeoutVote`s into
        //       `TimeoutCertificate`s, it is still a good idea to at least inspect `timeout_vote.highest_tc`.
        let validator_set_state = block_tree.validator_set_state()?;
        if !is_validator(origin, &validator_set_state) {
            return Ok(());
        };

        // 2. Check whether the `TimeoutVote` is cryptographically correct.
        // MonadBFT: accept timeout votes for ALL views (not just epoch-change views).
        if timeout_vote.is_correct(origin) {
            // MonadBFT Bracha amplification: accumulate voting power per view.
            if timeout_vote.view >= self.view_info.view {
                // Deduplicate: skip if this voter already contributed to this view.
                let voter_key = origin.to_bytes();
                let voters = self
                    .state
                    .bracha_timeout_voters
                    .entry(timeout_vote.view)
                    .or_default();
                if !voters.contains(&voter_key) {
                    voters.insert(voter_key);
                    // Look up voter's power and accumulate.
                    let voter_power = validator_set_state
                        .committed_validator_set()
                        .power(origin)
                        .map(|p| p.int())
                        .unwrap_or(0);
                    let accumulated = self
                        .state
                        .bracha_timeout_power
                        .entry(timeout_vote.view)
                        .or_insert(0);
                    *accumulated += voter_power;
                }
                let accumulated_power = *self
                    .state
                    .bracha_timeout_power
                    .get(&timeout_vote.view)
                    .unwrap_or(&0);
                let total_power = validator_set_state
                    .committed_validator_set()
                    .total_power()
                    .int() as u64;
                let f = if total_power > 0 {
                    (total_power - 1) / 3
                } else {
                    0
                };
                if accumulated_power > f
                    && is_validator(&self.config.keypair.public(), &validator_set_state)
                    && self
                        .state
                        .last_timeout_vote_view
                        .is_none_or(|v| v < timeout_vote.view)
                {
                    let own_timeout = PacemakerMessage::timeout_vote(
                        &self.config.keypair,
                        self.config.chain_id,
                        timeout_vote.view,
                        block_tree.highest_tc()?,
                        block_tree.local_tip()?,
                        block_tree.highest_qc_for_timeout()?,
                    );
                    self.sender.broadcast(Message::from(own_timeout.clone()));
                    if let PacemakerMessage::TimeoutVote(ref tv) = own_timeout {
                        Event::TimeoutVote(TimeoutVoteEvent {
                            timestamp: SystemTime::now(),
                            timeout_vote: tv.clone(),
                        })
                        .publish(&self.event_publisher);
                    }
                    self.state.last_timeout_vote_view = Some(timeout_vote.view);
                }
            }

            let fallback_tc = match &timeout_vote.highest_tc {
                Some(tc) if tc.is_correct(block_tree)? => Some(tc.clone()),
                _ => None,
            };

            // FIX CONS-FIND-26: Validate TipInfo fields before collecting.
            if let Some(ref tip) = timeout_vote.local_tip {
                if tip.view > self.view_info.view {
                    return Ok(());
                }
            }

            // 3. Try to collect the TimeoutVote into a new `TimeoutCertificate`.
            if let Some(new_tc) = self
                .state
                .timeout_vote_collectors
                .collect(origin, timeout_vote)
            {
                Event::CollectTC(CollectTCEvent {
                    timestamp: SystemTime::now(),
                    timeout_certificate: new_tc.clone(),
                })
                .publish(&self.event_publisher);

                // MonadBFT B3: Record leader timeout for reputation tracking.
                // Must use reputation-aware selection to blame the actual leader.
                let timed_out_leader = match block_tree.leader_reputation().ok() {
                    Some(ref rep) => select_leader_with_reputation(
                        new_tc.view,
                        validator_set_state.committed_validator_set(),
                        rep,
                    ),
                    None => {
                        select_leader(new_tc.view, validator_set_state.committed_validator_set())
                    }
                };
                let _ = block_tree.record_leader_timeout(&timed_out_leader);

                // 3.1. If a newly collected Timeout Certificate has a higher view than `highest_tc`, update `highest_tc`.
                //
                // Note: we do not call `update_view` in this conditional block. We will call it when we receive an
                //       AdvanceView message (e.g., the AdvanceView message we may send out in this conditional block).
                if block_tree.highest_tc()?.is_none()
                    || new_tc.view > block_tree.highest_tc()?.unwrap().view
                {
                    let mut wb = BlockTreeWriteBatch::new();
                    wb.set_highest_tc(&new_tc)?;
                    block_tree.write(wb);
                    Event::UpdateHighestTC(UpdateHighestTCEvent {
                        timestamp: SystemTime::now(),
                        highest_tc: new_tc.clone(),
                    })
                    .publish(&self.event_publisher);

                    // 3.2. If we are a validator and we haven't broadcasted an AdvanceView message in the current view,
                    //      broadcast an AdvanceView message containing the newly collected TimeoutCertificate.
                    // FIX CONS-FIND-25: Parenthesize to prevent non-validators broadcasting AdvanceView.
                    if is_validator(&self.config.keypair.public(), &validator_set_state)
                        && (self.state.last_advance_view.is_none()
                            || self
                                .state
                                .last_advance_view
                                .is_some_and(|v| v < self.view_info.view))
                    {
                        let pacemaker_message = PacemakerMessage::advance_view(
                            ProgressCertificate::TimeoutCertificate(new_tc),
                        );
                        self.sender
                            .broadcast(Message::from(pacemaker_message.clone()));
                        if let PacemakerMessage::AdvanceView(advance_view) = pacemaker_message {
                            Event::AdvanceView(AdvanceViewEvent {
                                timestamp: SystemTime::now(),
                                advance_view,
                            })
                            .publish(&self.event_publisher)
                        }
                        self.state.last_advance_view = Some(self.view_info.view);
                    }
                }

            // 4. Else, if we fail to collect a new TimeoutCertificate, process the `fallback_tc` in the vote.
            } else if let Some(tc) = fallback_tc {
                // In case the replica is behind, the "fallback tc" contained in the timeout vote message
                // serves to prove to it that a quorum is ahead and lets the replica catch up.
                if block_tree.highest_tc()?.is_none()
                    || tc.view > block_tree.highest_tc()?.unwrap().view
                {
                    block_tree.set_highest_tc(&tc)?;
                    Event::UpdateHighestTC(UpdateHighestTCEvent {
                        timestamp: SystemTime::now(),
                        highest_tc: tc.clone(),
                    })
                    .publish(&self.event_publisher);

                    let next_view = tc.view + 1;
                    if next_view > self.view_info.view {
                        self.update_view(
                            next_view,
                            &validator_set_state,
                            block_tree.highest_pc()?.view,
                            block_tree.committed_qc_view()?,
                        )?
                    }
                }
            }
        }
        Ok(())
    }

    /// Execute the required steps in the Pacemaker protocol upon receiving an `AdvanceView` message.
    ///
    /// ## Preconditions
    ///
    /// `advance_view.progress_certificate.view() >= self.query().view`
    fn on_receive_advance_view<K: KVStore>(
        &mut self,
        advance_view: AdvanceView,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), PacemakerError> {
        Event::ReceiveAdvanceView(ReceiveAdvanceViewEvent {
            timestamp: SystemTime::now(),
            origin: *origin,
            advance_view: advance_view.clone(),
        })
        .publish(&self.event_publisher);

        // 1. If the `origin` is not a validator, ignore the `AdvanceView` message.
        let validator_set_state = block_tree.validator_set_state()?;
        if !is_validator(origin, &validator_set_state) {
            return Ok(());
        }

        // 2. Check whether the progress certificate contained in the Advance View message is "valid".
        let progress_certificate = advance_view.progress_certificate.clone();
        let is_valid = match &progress_certificate {
            ProgressCertificate::PhaseCertificate(pc) => pc.is_correct(block_tree)?,
            // FIX CONS-FIND-18: Accept TCs in AdvanceView for ANY view, not just
            // epoch-change views. TCs are valid for liveness at any view.
            ProgressCertificate::TimeoutCertificate(tc) => tc.is_correct(block_tree)?,
        };

        if is_valid {
            // 3a. If the certificate is a PC, advance highest_pc and update_decided
            //     without commit processing. In the header pipeline, HotStuff
            //     hasn't called block_tree.update() yet (body in flight), so
            //     the pacemaker keeps highest_pc in sync to prevent proposer
            //     disagreement on validator set state.
            if let ProgressCertificate::PhaseCertificate(pc) = &progress_certificate {
                let _ = block_tree.advance_highest_pc_from_remote(pc, &self.event_publisher);
            }

            // 3b. If the received certificate is a TimeoutCertificate and has a higher view number than `highest_tc`,
            //     update the `highest_tc`.
            if let ProgressCertificate::TimeoutCertificate(tc) = &progress_certificate {
                if block_tree.highest_tc()?.is_none()
                    || tc.view > block_tree.highest_tc()?.unwrap().view
                {
                    block_tree.set_highest_tc(tc)?;
                    Event::UpdateHighestTC(UpdateHighestTCEvent {
                        timestamp: SystemTime::now(),
                        highest_tc: tc.clone(),
                    })
                    .publish(&self.event_publisher);
                }
            };

            // 4. If we are a validator and we haven't broadcasted an AdvanceView message in the current view,
            //    re-broadcast the received AdvanceView message.
            if is_validator(
                &self.config.keypair.public(),
                &block_tree.validator_set_state()?,
            ) && (self.state.last_advance_view.is_none()
                || self
                    .state
                    .last_advance_view
                    .is_some_and(|v| v < self.view_info.view))
            {
                self.sender
                    .broadcast(Message::from(PacemakerMessage::AdvanceView(
                        advance_view.clone(),
                    )));
                Event::AdvanceView(AdvanceViewEvent {
                    timestamp: SystemTime::now(),
                    advance_view,
                })
                .publish(&self.event_publisher);
                self.state.last_advance_view = Some(self.view_info.view);
            }

            let next_view = progress_certificate.view() + 1;
            if next_view > self.view_info.view {
                self.update_view(
                    next_view,
                    &validator_set_state,
                    block_tree.highest_pc()?.view,
                    block_tree.committed_qc_view()?,
                )?
            }
        }

        Ok(())
    }

    /// Update the Pacemaker's state in order to enter a specified `next_view`.
    ///
    /// `highest_qc_view` is the view of the block tree's Highest PC at entry
    /// time, and `committed_view` (S470) is the justify-view of the highest
    /// committed block (see `BlockTreeSingleton::committed_qc_view`). Together they drive the
    /// Task A stall multiplier and the S470 commit-lag multiplier:
    /// consensus-visible state only, so every honest replica derives the
    /// identical (possibly backed-off) deadline for `next_view`.
    ///
    /// # Preconditions
    ///
    /// This function should only be called if `next_view` is greater than the current view. Otherwise, an
    /// [`UpdateViewError`] will be returned.
    fn update_view(
        &mut self,
        next_view: ViewNumber,
        validator_set_state: &ValidatorSetState,
        highest_qc_view: ViewNumber,
        committed_view: ViewNumber,
    ) -> Result<(), PacemakerError> {
        let cur_view = self.view_info.view;

        // 1. Return an error if the precondition that `next_view` must be greater than the current view is
        //    violated.
        if next_view <= cur_view {
            return Err(UpdateViewError::NonIncreasingViewError {
                cur_view,
                next_view,
            }
            .into());
        }

        // 2. If about to enter a new epoch, set timeouts for the new epoch.
        if epoch(cur_view, self.config.epoch_length) != epoch(next_view, self.config.epoch_length) {
            self.state
                .update_timeouts(next_view, &self.config, highest_qc_view, committed_view);
        }

        // 2b. S395 liveness fix: the per-epoch schedule assigns every view an ABSOLUTE
        // deadline (`epoch_start_time + gap * max_view_time`). That is correct for a
        // replica that is BEHIND schedule (missed views' deadlines are in the past, so
        // it burns through them instantly), but a replica that gets AHEAD of schedule
        // would inherit a deadline far in the future and park until it: (a) a restart
        // that jumps forward via a persisted/received TC from a long no-quorum churn
        // (observed live: ~39k-view intra-epoch jump = ~5.5h park at view 642500), and
        // (b) a fast QC run whose accumulated surplus turns one dead leader into a
        // minutes-long stall. When the deadline of the view we are ENTERING is more
        // than 2x max_view_time away, the remaining schedule is stale for this
        // replica: rebase it to start from now (same routine as an epoch entry).
        // Task A: the stall multiplier for the view being entered — 1 while
        // the QC frontier keeps up, factor^min(depth, cap) while views outrun
        // QCs. S470: also grows with the commit lag (QC frontier vs commit
        // frontier) while `commit_lag_cap > 0`. Used by both the clamp
        // threshold (2b) and the deadline stretch (2c) below.
        let multiplier = self
            .config
            .stall_multiplier(next_view, highest_qc_view, committed_view);

        let scheduled = *self
            .state
            .timeouts
            .get(&next_view)
            .ok_or(UpdateViewError::GetViewTimeoutError { view: next_view })?;
        // Task A: the threshold scales with the multiplier so a legitimately
        // backed-off schedule (slots are `max_view_time * multiplier` wide) is
        // not rebased away on every entry, while a genuinely stale jump — whose
        // surplus exceeds even the backed-off allowance — still rebases.
        if scheduled > Instant::now() + self.config.max_view_time * 2 * multiplier {
            self.state
                .update_timeouts(next_view, &self.config, highest_qc_view, committed_view);
        }

        // 2c. Task A (adaptive backoff): if views have outrun the QC frontier —
        // the lockstep-stall signature: views keep being abandoned without a
        // new QC forming — stretch the entered view's deadline to
        // `max_view_time * factor^min(stall_depth, cap)` from now, so honest
        // replicas overlap in a view long enough for a QC to form. The
        // multiplier is a pure function of `(next_view, highest_qc_view,
        // fleet config)`: every honest replica derives the identical deadline.
        // Healthy entries (multiplier 1) leave the schedule untouched.
        if multiplier > 1 {
            let scheduled = *self
                .state
                .timeouts
                .get(&next_view)
                .ok_or(UpdateViewError::GetViewTimeoutError { view: next_view })?;
            let backed_off = Instant::now() + self.config.max_view_time * multiplier;
            if backed_off > scheduled {
                self.state.timeouts.insert(next_view, backed_off);
            }
        }

        // 3. Update the Pacemaker's `view_info` state.
        self.view_info = ViewInfo::new(
            next_view,
            *self
                .state
                .timeouts
                .get(&next_view)
                .ok_or(UpdateViewError::GetViewTimeoutError { view: next_view })?,
        );

        // 4. Replace our current `timeout_vote_collectors` with new ones for the view we just entered.
        self.state.timeout_vote_collectors = <ActiveCollectorPair<TimeoutVoteCollector>>::new(
            self.config.chain_id,
            next_view,
            validator_set_state,
        );

        // FIX CONS-PF-16: Prune bracha state for views we'll never revisit.
        let cutoff_view = ViewNumber::new(next_view.int().saturating_sub(100));
        self.state.bracha_timeout_power = self.state.bracha_timeout_power.split_off(&cutoff_view);
        self.state.bracha_timeout_voters = self.state.bracha_timeout_voters.split_off(&cutoff_view);

        Ok(())
    }

    /// Extend the timeout of the current view, which must be an Epoch-Change View.
    ///
    /// `highest_qc_view` is the view of the block tree's Highest PC and
    /// `committed_view` (S470) the justify-view of the highest committed
    /// block: the Task A stall multiplier and the S470 commit-lag multiplier
    /// scale the extension while views have outrun QCs or QCs have outrun
    /// commits.
    ///
    /// # Errors
    ///
    /// This function should only be called if the current view is an Epoch-Change View. Otherwise, an
    /// [`ExtendViewError`] will be returned.
    fn extend_view(
        &mut self,
        highest_qc_view: ViewNumber,
        committed_view: ViewNumber,
    ) -> Result<(), ExtendViewError> {
        // 1. Confirm that the current view is an Epoch-Change View.
        let cur_view = self.view_info.view;
        if !is_epoch_change_view(&cur_view, self.config.epoch_length) {
            return Err(ExtendViewError::TriedToExtendNonEpochView { view: cur_view });
        };

        // 2. Increase the timeout of the current view inside `PacemakerState`.
        self.state.extend_epoch_change_view_timeout(
            self.view_info.view,
            &self.config,
            highest_qc_view,
            committed_view,
        );

        // 3. Increase the timeout of the current view inside `ViewInfo`.
        let new_timeout = self
            .state
            .timeouts
            .get(&cur_view)
            .ok_or(ExtendViewError::GetViewTimeoutError { view: cur_view })?;
        self.view_info = self.view_info.with_new_timeout(*new_timeout);

        Ok(())
    }
}

/// Configuration variables for the [`Pacemaker`] struct.
#[derive(Clone)]
pub(crate) struct PacemakerConfiguration {
    /// The Chain ID of the blockchain that the current replica is to track.
    pub(crate) chain_id: ChainID,

    /// The keypair with which the Pacemaker implementation should sign `TimeoutVote`s.
    pub(crate) keypair: Keypair,

    /// How many views are in an epoch.
    pub(crate) epoch_length: EpochLength,

    /// How much time can elapse in a view before it times out.
    pub(crate) max_view_time: Duration,

    /// Base of the multiplicative view-timeout backoff: while views outrun the
    /// QC frontier (a stall), each view's timeout is scaled by
    /// `backoff_factor^min(stall_depth, backoff_cap)`. Consensus-liveness
    /// critical — must be identical on every replica of a chain (genesis-sourced).
    pub(crate) backoff_factor: u32,

    /// Exponent cap of the view-timeout backoff. `0` is the runtime kill
    /// switch: the multiplier is then constantly 1 and the schedule is exactly
    /// the pre-backoff one. Fleet-wide (genesis-sourced), like `backoff_factor`.
    pub(crate) backoff_cap: u32,

    /// Exponent cap of the COMMIT-LAG view-timeout backoff (S470 wedge fix).
    ///
    /// The Task A stall term is keyed on `view − highest_qc_view`, which stays
    /// SMALL during the header-pipeline commit wedge (the QC frontier crawls
    /// forward one QC every few views while the commit frontier freezes: body
    /// execution is slower than the view timer, so no proposal ever lands in
    /// `justify.view + 1` and the 2-chain consecutive-views commit rule never
    /// fires). This second term is keyed on the gap that IS large during that
    /// wedge: `highest_qc_view − committed_view` (QC frontier vs commit
    /// frontier). See [`commit_lag_exponent`].
    ///
    /// `0` is the runtime kill switch (the default): the commit-lag term is
    /// then constantly neutral and the deadline schedule is byte-identical to
    /// the pre-S470 one — mirroring the `backoff_cap = 0` semantics.
    ///
    /// LOUD WARNING — fleet-uniform knob: this value must be IDENTICAL on
    /// every replica of a chain (genesis-sourced). Divergent values across a
    /// fleet mean divergent deadline schedules, which degrade liveness (the
    /// earliest replica's timeout triggers Bracha amplification and collapses
    /// lockstep view synchronization). Never a safety violation — but a
    /// misconfigured fleet can stall.
    pub(crate) commit_lag_cap: u32,
}

impl PacemakerConfiguration {
    /// Effective view-timeout multiplier for entering (or rebasing a schedule
    /// at) `view` given the current QC frontier. The QC for view v justifies
    /// *entering* v+1, so a healthy entry (`view == highest_qc_view + 1`) is
    /// stall depth 0 — multiplier 1, schedule byte-identical to pre-backoff.
    /// Each further view entered without the frontier advancing deepens the
    /// stall by one and multiplies the timeout by another `backoff_factor`.
    ///
    /// S470: combined with the commit-lag term (see [`commit_lag_exponent`]),
    /// which grows with `highest_qc_view − committed_view` instead. The two
    /// exponents are combined by MAX (not sum), so each term alone reproduces
    /// its standalone behavior and a wedge that trips both never
    /// double-compounds. `commit_lag_cap = 0` (the default) makes the second
    /// term constantly 0 — schedule byte-identical to the stall-only one.
    ///
    /// Lockstep safety: both inputs are CONSENSUS-VISIBLE quantities —
    /// `highest_qc_view` is the view of the highest PhaseCertificate and
    /// `committed_view` is the justify-view of the highest committed block
    /// (the committed block is determined by the 2-chain rule over the QC
    /// chain, not by local timing). Neither depends on node-local execution
    /// state, so identical inputs yield identical deadlines on every honest
    /// replica. Both frontiers propagate asynchronously, so replicas can
    /// transiently disagree by a view or two — exactly as they already can on
    /// `highest_qc_view` in the pre-S470 stall term — but both are FROZEN and
    /// identical across honest replicas precisely when the backoff is active
    /// (during a stall/wedge), which is when schedule agreement matters.
    fn stall_multiplier(
        &self,
        view: ViewNumber,
        highest_qc_view: ViewNumber,
        committed_view: ViewNumber,
    ) -> u32 {
        let stall_exponent = backoff_exponent(view, highest_qc_view + 1, self.backoff_cap);
        let lag_exponent =
            commit_lag_exponent(highest_qc_view, committed_view, self.commit_lag_cap);
        self.backoff_factor
            .saturating_pow(stall_exponent.max(lag_exponent))
            .max(1)
    }
}

/// In-memory state of a [`Pacemaker`].
struct PacemakerState {
    /// Mapping between current and future view numbers and the timeout assigned to each.
    timeouts: BTreeMap<ViewNumber, Instant>,

    /// `TimeoutVoteCollector`s for the at-most two validator sets that are active in the current view.
    timeout_vote_collectors: ActiveCollectorPair<TimeoutVoteCollector>,

    /// The view in which this replica last broadcasted an [`AdvanceView`] message.
    last_advance_view: Option<ViewNumber>,
    /// Bracha timeout amplification: accumulated voting power per view.
    bracha_timeout_power: BTreeMap<ViewNumber, u64>,
    /// Bracha timeout amplification: dedup set of voters per view (by key bytes).
    bracha_timeout_voters: BTreeMap<ViewNumber, BTreeSet<[u8; 32]>>,
    last_timeout_vote_view: Option<ViewNumber>,
}

impl PacemakerState {
    /// Initializes a `PacemakerState` upon starting the Pacemaker subprotocol.
    fn initialize(
        config: &PacemakerConfiguration,
        init_view: ViewNumber,
        validator_set_state: &ValidatorSetState,
    ) -> Self {
        /// Return initial timeouts on starting the protocol from a given `start_view`.
        fn initial_timeouts(
            start_view: ViewNumber,
            config: &PacemakerConfiguration,
            highest_qc_view: ViewNumber,
            committed_view: ViewNumber,
        ) -> BTreeMap<ViewNumber, Instant> {
            let mut timeouts = BTreeMap::new();

            let epoch = epoch(start_view, config.epoch_length);
            let epoch_view = epoch * config.epoch_length.int() as u64;

            let start_time = Instant::now();

            // Task A: per-view slot scaled by the stall multiplier at the
            // schedule base (1 while the QC frontier keeps up — see
            // `stall_multiplier`).
            let multiplier = config.stall_multiplier(start_view, highest_qc_view, committed_view);

            // Add timeouts for all remaining views in the epoch of start_view.
            for view in start_view.int()..=epoch_view {
                let time_to_view_deadline =
                    config.max_view_time * multiplier * (view - start_view.int() + 1) as u32;
                timeouts.insert(ViewNumber::new(view), start_time + time_to_view_deadline);
            }

            timeouts
        }

        Self {
            // At boot the replica has no evidence of an ongoing stall: treat
            // `init_view` as justified and the commit frontier as caught up
            // (stall depth 0, commit lag 0, multiplier 1). The first
            // block-tree-informed entry (update_view) re-derives both terms
            // from the persisted consensus frontiers.
            timeouts: initial_timeouts(init_view, config, init_view, init_view),
            timeout_vote_collectors: <ActiveCollectorPair<TimeoutVoteCollector>>::new(
                config.chain_id,
                init_view,
                validator_set_state,
            ),
            last_advance_view: None,
            bracha_timeout_power: BTreeMap::new(),
            bracha_timeout_voters: BTreeMap::new(),
            last_timeout_vote_view: None,
        }
    }

    /// Update the `PacemakerState`'s timeouts upon entering the epoch with the given `epoch_start_view`,
    /// or upon rebasing a stale schedule at `epoch_start_view`.
    fn update_timeouts(
        &mut self,
        epoch_start_view: ViewNumber,
        config: &PacemakerConfiguration,
        highest_qc_view: ViewNumber,
        committed_view: ViewNumber,
    ) {
        // Remove timeouts for expired views.
        self.timeouts = self.timeouts.split_off(&epoch_start_view);

        // Compute the `ViewNumber` of the view that ends the epoch that `epoch_start_view` starts.
        let epoch_change_view = {
            let epoch_num = epoch(epoch_start_view, config.epoch_length);
            epoch_num * config.epoch_length.int() as u64
        };

        // Set the current time as the epoch's start time.
        let epoch_start_time = Instant::now();

        // Task A: per-view slot scaled by the stall multiplier at the schedule
        // base — 1 while the QC frontier keeps up (healthy entries/rebases are
        // byte-identical to the pre-backoff schedule), factor^min(depth, cap)
        // while views outrun QCs. S470: also scaled while QCs outrun commits.
        let multiplier = config.stall_multiplier(epoch_start_view, highest_qc_view, committed_view);

        // Populate `self.timeouts` with the timeouts of the views in the newly-entered epoch.
        for view in epoch_start_view.int()..=epoch_change_view {
            let time_to_view_deadline =
                config.max_view_time * multiplier * (view - epoch_start_view.int() + 1) as u32;
            self.timeouts.insert(
                ViewNumber::new(view),
                epoch_start_time + time_to_view_deadline,
            );
        }
    }

    /// Extend the timeout of the `epoch_change_view` by another `config.max_view_time`,
    /// scaled by the Task A stall multiplier (1 while the QC frontier keeps up
    /// — the legacy flat extend).
    ///
    /// # Preconditions
    ///
    /// The caller must ensure that `epoch_change_view` is actually an epoch-change view.
    fn extend_epoch_change_view_timeout(
        &mut self,
        epoch_change_view: ViewNumber,
        config: &PacemakerConfiguration,
        highest_qc_view: ViewNumber,
        committed_view: ViewNumber,
    ) {
        let multiplier =
            config.stall_multiplier(epoch_change_view, highest_qc_view, committed_view);
        self.timeouts.insert(
            epoch_change_view,
            Instant::now() + config.max_view_time * multiplier,
        );
    }
}

/// Enumerates the different ways a call to any of [`Pacemaker`]'s methods can fail.
#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum PacemakerError {
    /// See: [`UpdateViewError`].
    UpdateViewError(#[allow(dead_code)] UpdateViewError),

    /// See: [`ExtendViewError`].
    ExtendViewError(#[allow(dead_code)] ExtendViewError),

    /// See: [`BlockTreeError`]
    BlockTreeError(#[allow(dead_code)] BlockTreeError),
}

impl From<BlockTreeError> for PacemakerError {
    fn from(value: BlockTreeError) -> Self {
        PacemakerError::BlockTreeError(value)
    }
}

impl From<UpdateViewError> for PacemakerError {
    fn from(value: UpdateViewError) -> Self {
        PacemakerError::UpdateViewError(value)
    }
}

impl From<ExtendViewError> for PacemakerError {
    fn from(value: ExtendViewError) -> Self {
        PacemakerError::ExtendViewError(value)
    }
}

/// Enumerates the different ways a [`Pacemaker::update_view`] call can fail.
#[derive(Debug)]
pub enum UpdateViewError {
    /// An attempt was made to update the current view to a lower view. This violates the invariant that views
    /// must be monotonically increasing.
    NonIncreasingViewError {
        /// The current view.
        #[allow(dead_code)]
        cur_view: ViewNumber,

        /// The lower view that the caller tried to change the current view to.
        #[allow(dead_code)]
        next_view: ViewNumber,
    },

    /// The timeout for a requested view cannot be found in the [`PacemakerState`]. This violates the invariant
    /// that the `Pacemaker` should be able to provide the timeout of any view it returns from
    /// [`view_info`](Pacemaker::view_info).
    GetViewTimeoutError {
        #[allow(dead_code)]
        view: ViewNumber,
    },
}

/// Enumerates the different ways a [`Pacemaker::extend_view`] call can fail.
#[derive(Debug)]
pub enum ExtendViewError {
    /// An attempt was made to extend a view that is not an Epoch-Change view.
    TriedToExtendNonEpochView {
        #[allow(dead_code)]
        view: ViewNumber,
    },

    /// Same as [`UpdateViewError::GetViewTimeoutError`].
    GetViewTimeoutError {
        #[allow(dead_code)]
        view: ViewNumber,
    },
}

/// Describes a view (most often the current view), in terms of its view number and its view deadline (the
/// instant in time in which the view should end if no progress was made).
#[derive(PartialEq, Eq, Clone)]
pub(crate) struct ViewInfo {
    pub(crate) view: ViewNumber,
    pub(crate) deadline: Instant,
}

impl ViewInfo {
    /// Create a new `ViewInfo` instance containing the provided parameters.
    pub(crate) fn new(view: ViewNumber, deadline: Instant) -> Self {
        Self { view, deadline }
    }

    /// Return a given [ViewInfo] with updated timeout.
    pub(crate) fn with_new_timeout(&self, new_deadline: Instant) -> Self {
        Self {
            view: self.view,
            deadline: new_deadline,
        }
    }
}

/// Deterministically select a replica in `validator_set` to become the leader of `view` using the
/// [Interleaved WRR](https://en.wikipedia.org/wiki/Weighted_round_robin#Interleaved_WRR) algorithm.
///
/// The abstract IWRR array is: for each threshold `t` in `1..=p_max`, every validator (in
/// `validators()` order) whose power is `>= t`. The leader of `view` is the element at
/// `view % total_power`.
///
/// Computed in closed form, O(n log n) in the number of validators. The previous
/// implementation walked the abstract array entry-by-entry — O(view % total_power) work per
/// call with a HashMap lookup per step. With live powers of stake/wei (= 2,000,000 per
/// validator, total 6,000,000) the walk grew linearly with view number for millions of views:
/// ~1.2M iterations per call at view 1.18M, several calls per view on every replica. That was
/// the chain-wide block-cadence "height drag" (S405: 268ms/blk at view 700k → 475ms at 1.16M —
/// perf showed select_leader + its hashing at >20% of the algorithm thread). The reference
/// walk survives as `select_leader_reference` in tests; `select_leader_closed_form_matches_reference`
/// proves output equivalence exhaustively — the two are consensus-identical.
///
/// [Read more](super#leader-selection).
pub fn select_leader(view: ViewNumber, validator_set: &ValidatorSet) -> VerifyingKey {
    // Length of the abstract array.
    let p_total = validator_set.total_power();
    // Total number of validators.
    let n = validator_set.len();

    assert!(
        n > 0 && p_total.int() > 0,
        "select_leader: validator set must be non-empty with nonzero total power \
         (n={n}, total_power={})",
        p_total.int()
    );

    // Index in the abstract array.
    let index = (view.int() % (p_total.int() as u64)) as u128;

    // Validator powers in `validators()` order (the row order of the abstract array).
    let powers: Vec<u64> = validator_set
        .validators()
        .map(|v| validator_set.power(v).unwrap().int())
        .collect();

    // Distinct positive power levels, ascending. For thresholds `t` in the segment
    // `(prev_level, level]` no validator power lies strictly between the bounds, so the
    // row membership {v : power(v) >= t} = {v : power(v) >= level} is constant across
    // the segment: (level - prev_level) rows of `members` entries each.
    let mut levels = powers
        .iter()
        .copied()
        .filter(|p| *p > 0)
        .collect::<Vec<_>>();
    levels.sort_unstable();
    levels.dedup();

    let mut cum: u128 = 0;
    let mut prev: u64 = 0;
    for level in levels {
        let members = powers.iter().filter(|p| **p >= level).count() as u128;
        let seg = (level - prev) as u128 * members;
        if cum + seg > index {
            // `index` falls inside this segment; identical rows, so only the
            // position within the row matters.
            let pos_in_row = ((index - cum) % members) as usize;
            let validator = validator_set
                .validators()
                .filter(|v| validator_set.power(v).unwrap().int() >= level)
                .nth(pos_in_row)
                .unwrap();
            return *validator;
        }
        cum += seg;
        prev = level;
    }

    // Safety: index = view % total_power < total_power = the abstract array length,
    // so a segment always contains it. This should never happen.
    unreachable!("Cannot select a leader: index not found!")
}

/// MonadBFT B3 kill switch: whether `select_leader_with_reputation` actually
/// weights by reputation (`true`) or delegates to plain IWRR [`select_leader`]
/// (`false`, the default).
///
/// Default-off because reputation is accumulated from locally-observed events
/// (TC formation, QC advancement) that are not totally ordered across replicas:
/// a replica that misses events (offline, or syncing past them) builds a
/// different reputation map, and divergent maps make replicas disagree on the
/// leader of every view — which prevents consecutive-view QCs and therefore
/// halts 2-chain commits. Selection must stay a pure function of consensus
/// state (view, validator set) until reputation is derived from committed
/// chain data.
static REPUTATION_LEADER_SELECTION_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Enable or disable reputation-weighted leader selection (default: disabled).
///
/// Leader selection must agree on every replica of a chain: set this to the
/// same value on ALL replicas, before the replica starts, and never while it
/// is running.
pub fn set_reputation_leader_selection(enabled: bool) {
    REPUTATION_LEADER_SELECTION_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Whether reputation-weighted leader selection is currently enabled.
pub fn reputation_leader_selection_enabled() -> bool {
    REPUTATION_LEADER_SELECTION_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// MonadBFT B3: Deterministically select a leader using reputation-weighted stake.
///
/// When reputation-weighted selection is disabled (the default, see
/// [`set_reputation_leader_selection`]), this ignores `reputation` and behaves
/// exactly like [`select_leader`]. When enabled, it delegates to
/// [`select_leader_reputation_weighted`].
pub fn select_leader_with_reputation(
    view: ViewNumber,
    validator_set: &ValidatorSet,
    reputation: &crate::hotstuff::types::LeaderReputation,
) -> VerifyingKey {
    if !reputation_leader_selection_enabled() {
        return select_leader(view, validator_set);
    }
    select_leader_reputation_weighted(view, validator_set, reputation)
}

/// The reputation-weighted selection algorithm itself (B3), applied
/// unconditionally — callers outside tests should go through
/// [`select_leader_with_reputation`] so the kill switch is respected.
///
/// Adjusts each validator's effective power by their reputation score (in basis
/// points), then delegates to the standard IWRR `select_leader`. Validators with
/// low reputation get proportionally fewer leadership opportunities but are never
/// fully excluded (minimum 1 power unit if they have any stake).
///
/// This function is deterministic: same (view, validator_set, reputation) → same leader.
pub fn select_leader_reputation_weighted(
    view: ViewNumber,
    validator_set: &ValidatorSet,
    reputation: &crate::hotstuff::types::LeaderReputation,
) -> VerifyingKey {
    // Warm-up: use plain round-robin for the first 20 views to let the
    // system bootstrap before reputation data influences leader selection.
    if view.int() < 20 {
        return select_leader(view, validator_set);
    }

    use crate::types::{data_types::Power, update_sets::ValidatorSetUpdates};

    // Build adjusted validator set with reputation-weighted powers.
    let mut adjusted_vs = ValidatorSet::new();
    let mut updates = ValidatorSetUpdates::new();

    for (vk, power) in validator_set.validators_and_powers() {
        let score_bps = reputation.score_bps(&vk) as u64;
        let base_power = power.int();
        // Effective power = base_power * score / 10000, minimum 1.
        let effective = std::cmp::max(1, base_power * score_bps / 10_000);
        updates.insert(vk, Power::new(effective));
    }
    adjusted_vs.apply_updates(&updates);

    select_leader(view, &adjusted_vs)
}

/// Task A: multiplicative view-timeout backoff, derived purely from
/// consensus-visible state — `factor^min(view − highest_qc_view, cap)`,
/// saturating (never below 1, gap saturates at 0 when the QC frontier is
/// ahead). Because `highest_qc_view` is a consensus object (frozen and
/// identical across honest replicas exactly when backoff is active, i.e.
/// during a stall), identical inputs yield an identical schedule on every
/// replica: lockstep by construction. `cap = 0` (or `factor <= 1`) makes the
/// multiplier constantly 1 — the pre-backoff schedule, byte-identical.
///
/// NOTE for callers: the QC for view v justifies *entering* v+1, so a healthy
/// entry has `view == highest_qc_view + 1`. Consult sites that must stay
/// neutral on the happy path evaluate `backoff_multiplier(view,
/// highest_qc_view + 1, ..)` — see [`PacemakerConfiguration::stall_multiplier`].
fn backoff_multiplier(view: ViewNumber, highest_qc_view: ViewNumber, factor: u32, cap: u32) -> u32 {
    factor
        .saturating_pow(backoff_exponent(view, highest_qc_view, cap))
        .max(1)
}

/// The stall exponent of the Task A backoff: `min(view − justified_view, cap)`,
/// saturating at 0 when the frontier is ahead. Pure function of
/// consensus-visible inputs (see [`backoff_multiplier`]).
fn backoff_exponent(view: ViewNumber, justified_view: ViewNumber, cap: u32) -> u32 {
    view.int()
        .saturating_sub(justified_view.int())
        .min(cap as u64) as u32
}

/// S470 wedge fix: healthy pipelined operation keeps the commit frontier a
/// couple of views behind the QC frontier (the 2-chain rule commits the
/// grandparent generation, and `committed_qc_view` reads the committed
/// block's OWN justify — one more generation of skew). Lags at or below this
/// grace never contribute to the deadline, so the happy-path schedule is
/// untouched even with `commit_lag_cap > 0`.
pub(crate) const COMMIT_LAG_GRACE: u64 = 4;

/// The commit-lag exponent of the S470 wedge backoff:
/// `min((highest_qc_view − committed_view).saturating_sub(GRACE), cap)`.
///
/// This is the term that engages during the header-pipeline commit wedge,
/// where the stall exponent stays ~0 (QCs keep crawling forward, resetting
/// `view − highest_qc_view`) while `highest_qc_view − committed_view` races
/// into the tens or hundreds. Pure function of consensus-visible inputs —
/// every honest replica with the same frontiers derives the same exponent.
/// `cap = 0` is the runtime kill switch: exponent constantly 0.
fn commit_lag_exponent(
    highest_qc_view: ViewNumber,
    committed_view: ViewNumber,
    cap: u32,
) -> u32 {
    highest_qc_view
        .int()
        .saturating_sub(committed_view.int())
        .saturating_sub(COMMIT_LAG_GRACE)
        .min(cap as u64) as u32
}

/// Check whether `view` is an epoch-change view given the configured `epoch_length`.
/// FIX CONS-FIND-20: Guard against epoch_length=0 to prevent division by zero.
fn is_epoch_change_view(view: &ViewNumber, epoch_length: EpochLength) -> bool {
    let el = epoch_length.int() as u64;
    if el == 0 {
        return false;
    }
    view.int().is_multiple_of(el)
}

/// Compute the current epoch based on the current `view` and the configured `epoch_length`.
/// FIX CONS-FIND-20: Guard against epoch_length=0 to prevent division by zero.
fn epoch(view: ViewNumber, epoch_length: EpochLength) -> u64 {
    let el = epoch_length.int() as u64;
    if el == 0 {
        return 0;
    }
    view.int().div_ceil(el)
}

/// Reputation-weighted selection must be opt-in: a replica that never calls
/// `set_reputation_leader_selection(true)` selects leaders with plain IWRR.
/// (Nothing else in the lib test binary toggles the switch, so this genuinely
/// observes the default.)
#[test]
fn reputation_leader_selection_disabled_by_default() {
    assert!(!reputation_leader_selection_enabled());
}

/// Tests if the number of times each validator is selected as a leader is proportional to its power.
#[test]
fn select_leader_fairness_test() {
    use crate::types::{data_types::Power, update_sets::ValidatorSetUpdates};
    use ed25519_dalek::{SigningKey, VerifyingKey};
    use rand_core::OsRng;

    let mut csprg = OsRng {};
    let n = 20;
    let keypairs: Vec<SigningKey> = (0..n).map(|_| SigningKey::generate(&mut csprg)).collect();
    let public_keys: Vec<VerifyingKey> = keypairs
        .iter()
        .map(|keypair| keypair.verifying_key())
        .collect();

    let mut validator_set = ValidatorSet::new();
    let mut validator_set_updates = ValidatorSetUpdates::new();
    public_keys
        .iter()
        .zip(0..n)
        .for_each(|(validator, power)| validator_set_updates.insert(*validator, Power::new(power)));
    validator_set.apply_updates(&validator_set_updates);

    let total_power = validator_set.total_power().int() as u64;
    let leader_sequence: Vec<VerifyingKey> = (0..total_power)
        .into_iter()
        .map(|v| select_leader(ViewNumber::new(v), &validator_set))
        .collect();

    validator_set.validators().for_each(|validator| {
        assert_eq!(
            leader_sequence
                .iter()
                .filter(|leader| leader == &validator)
                .count(),
            validator_set.power(validator).unwrap().int() as usize
        )
    })
}

/// The original entry-by-entry IWRR walk, kept as the semantic reference for
/// `select_leader_closed_form_matches_reference`. O(view % total_power) — this
/// was the S405 height-drag root cause and must never be used in production.
#[cfg(test)]
fn select_leader_reference(view: ViewNumber, validator_set: &ValidatorSet) -> VerifyingKey {
    let p_total = validator_set.total_power();
    let n = validator_set.len();
    let index = view.int() % (p_total.int() as u64);
    let p_max = validator_set
        .validators_and_powers()
        .iter()
        .map(|(_, power)| power.int())
        .max()
        .unwrap();

    let mut counter = 0;
    for threshold in 1..=p_max {
        for k in 0..=(n - 1) {
            let validator = validator_set.validators().nth(k).unwrap();
            if validator_set.power(validator).unwrap().int() >= threshold {
                if counter == index {
                    return *validator;
                }
                counter += 1
            }
        }
    }
    unreachable!("Cannot select a leader: index not found!")
}

/// Consensus-safety proof for the closed-form `select_leader`: it must return
/// the IDENTICAL leader to the reference walk for every view — a mixed fleet
/// of old and new binaries must always agree on the leader. Exhaustive over
/// three full array wraps for varied power profiles (duplicates, zeros, ones,
/// gaps, single validator), plus live-testnet-shaped spot checks (3 × 2M power
/// at real view magnitudes).
#[test]
fn select_leader_closed_form_matches_reference() {
    use crate::types::{data_types::Power, update_sets::ValidatorSetUpdates};
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    let mut csprg = OsRng {};
    let mut build = |powers: &[u64]| {
        let mut vs = ValidatorSet::new();
        let mut updates = ValidatorSetUpdates::new();
        for p in powers {
            let vk = SigningKey::generate(&mut csprg).verifying_key();
            updates.insert(vk, Power::new(*p));
        }
        vs.apply_updates(&updates);
        vs
    };

    for powers in [
        vec![2, 2, 2],
        vec![1, 3, 2],
        vec![5, 5, 1],
        vec![7],
        vec![1, 1, 1, 1, 10],
        vec![4, 9, 2, 2, 6, 1, 1, 3],
        vec![0, 3, 5], // power-0 validator never leads but sits in the set
    ] {
        let vs = build(&powers);
        let total: u64 = powers.iter().sum();
        for view in 0..(total * 3) {
            let v = ViewNumber::new(view);
            assert_eq!(
                select_leader(v, &vs),
                select_leader_reference(v, &vs),
                "diverged: powers={powers:?} view={view}"
            );
        }
    }

    // Live-testnet shape: 3 validators, power = stake/wei = 2,000,000 each.
    let vs = build(&[2_000_000, 2_000_000, 2_000_000]);
    for view in [
        0, 1, 19, 20, 1_179_840, 2_500_000, 5_999_999, 6_000_000, 6_000_001,
    ] {
        let v = ViewNumber::new(view);
        assert_eq!(
            select_leader(v, &vs),
            select_leader_reference(v, &vs),
            "diverged at live-shaped view {view}"
        );
    }
}

/// No-op network for pacemaker unit tests: `update_view` never sends, so the
/// sender handle only needs to exist.
#[cfg(test)]
#[derive(Clone)]
struct NullNetwork;

#[cfg(test)]
impl Network for NullNetwork {
    fn init_validator_set(&mut self, _: ValidatorSet) {}
    fn update_validator_set(&mut self, _: crate::types::update_sets::ValidatorSetUpdates) {}
    fn broadcast(&mut self, _: Message) {}
    fn send(&mut self, _: VerifyingKey, _: Message) {}
    fn recv(&mut self) -> Option<(VerifyingKey, Message)> {
        None
    }
}

/// Single-validator pacemaker fixture starting at `init_view`, 500ms view time,
/// 100k-view epochs (the live testnet shape). `commit_lag_cap` defaults to 0
/// (the production default — S470 commit-lag term off), so every pre-S470
/// test in this file also proves the knob-off schedule is byte-identical to
/// the pre-S470 one.
#[cfg(test)]
fn test_pacemaker(init_view: u64) -> (Pacemaker<NullNetwork>, ValidatorSetState) {
    test_pacemaker_with_commit_lag_cap(init_view, 0)
}

/// Like [`test_pacemaker`] but with an explicit S470 `commit_lag_cap`.
#[cfg(test)]
fn test_pacemaker_with_commit_lag_cap(
    init_view: u64,
    commit_lag_cap: u32,
) -> (Pacemaker<NullNetwork>, ValidatorSetState) {
    use crate::types::{data_types::Power, update_sets::ValidatorSetUpdates};
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    let mut csprg = OsRng {};
    let keypair = SigningKey::generate(&mut csprg);
    let config = PacemakerConfiguration {
        chain_id: ChainID::new(0),
        keypair: Keypair::new(keypair.clone()),
        epoch_length: EpochLength::new(100_000),
        max_view_time: Duration::from_millis(500),
        backoff_factor: 2,
        backoff_cap: 8,
        commit_lag_cap,
    };
    let mut vs = ValidatorSet::new();
    let mut updates = ValidatorSetUpdates::new();
    updates.insert(keypair.verifying_key(), Power::new(1));
    vs.apply_updates(&updates);
    let vss = ValidatorSetState::new(vs.clone(), vs, None, true);
    let pacemaker = Pacemaker::new(
        config,
        SenderHandle::new(NullNetwork),
        ViewNumber::new(init_view),
        &vss,
        None,
    )
    .unwrap();
    (pacemaker, vss)
}

/// S395 regression: entering a view whose scheduled deadline is far in the future
/// (forward jump via a persisted/received TC after a long no-quorum churn) must
/// rebase the schedule instead of parking the pacemaker until the stale absolute
/// deadline (observed live: ~39k-view intra-epoch jump = ~5.5h park at view 642500).
#[test]
fn update_view_jump_rebases_stale_schedule() {
    let (mut pacemaker, vss) = test_pacemaker(1);
    let max_view_time = Duration::from_millis(500);

    // Healthy QC frontier (the jump was justified by a fresh certificate):
    // the stall multiplier stays 1 and the legacy rebase semantics apply.
    pacemaker
        .update_view(ViewNumber::new(39_000), &vss, ViewNumber::new(38_999), ViewNumber::new(38_999))
        .unwrap();
    assert!(
        pacemaker.query().deadline <= Instant::now() + max_view_time * 2,
        "jumped-to view must not inherit a stale far-future deadline"
    );

    // Views AFTER the jump target must be rebased too, or every subsequent
    // sequential advance would park again.
    pacemaker
        .update_view(ViewNumber::new(39_001), &vss, ViewNumber::new(39_000), ViewNumber::new(39_000))
        .unwrap();
    assert!(pacemaker.query().deadline <= Instant::now() + max_view_time * 2);
}

/// A fast QC run (advancing far ahead of the wall-clock schedule) must not
/// accumulate unbounded surplus: one dead leader after a fast burst previously
/// stalled until the schedule caught up (minutes), instead of one view time.
#[test]
fn update_view_fast_run_deadline_bounded() {
    let (mut pacemaker, vss) = test_pacemaker(1);
    for v in 2..=50u64 {
        // Fast QC run: every entry is justified by the previous view's QC.
        pacemaker
            .update_view(ViewNumber::new(v), &vss, ViewNumber::new(v - 1), ViewNumber::new(v - 1))
            .unwrap();
    }
    assert!(
        pacemaker.query().deadline <= Instant::now() + Duration::from_millis(500) * 2,
        "fast-run surplus must be bounded to 2x max_view_time"
    );
}

/// Task A (pacemaker backoff): entering a view while views have outrun the QC
/// frontier must yield a deadline stretched to `max_view_time * multiplier`
/// from now — through both the rebase path (scheduled deadline far off) and
/// the in-place path (scheduled deadline near, the common mid-epoch stall).
#[test]
fn update_view_threads_qc_frontier() {
    let mvt = Duration::from_millis(500);

    // Rebase path: fresh schedule, entering view 5 with the QC frontier stuck
    // at view 1 (views 2..=4 died without a QC => stall depth 3 => x8).
    let (mut pacemaker, vss) = test_pacemaker(1);
    let before = Instant::now();
    pacemaker
        .update_view(ViewNumber::new(5), &vss, ViewNumber::new(1), ViewNumber::new(1))
        .unwrap();
    let after = Instant::now();
    let deadline = pacemaker.query().deadline;
    assert!(
        deadline >= before + mvt * 8,
        "stalled entry must back off the deadline by the multiplier"
    );
    assert!(deadline <= after + mvt * 8);

    // In-place path: real time has consumed the schedule (deadline near), so
    // the rebase clamp stays quiet and the entered view is stretched in place.
    let (mut pacemaker, vss) = test_pacemaker(1);
    std::thread::sleep(Duration::from_millis(1200));
    let before = Instant::now();
    pacemaker
        .update_view(ViewNumber::new(3), &vss, ViewNumber::new(1), ViewNumber::new(1))
        .unwrap();
    let after = Instant::now();
    let deadline = pacemaker.query().deadline;
    assert!(
        deadline >= before + mvt * 2,
        "mid-epoch stalled entry must be stretched in place (depth 1 => x2)"
    );
    assert!(deadline <= after + mvt * 2);
}

/// Task A (pacemaker backoff): the epoch-change extend path scales with the
/// stall multiplier too. A healthy frontier keeps the legacy flat
/// `max_view_time` extend; a lagging frontier extends by
/// `max_view_time * factor^min(depth, cap)`.
#[test]
fn extend_scales_with_gap() {
    // The fixture's epoch length is 100_000, so view 100_000 is an
    // epoch-change view (extend_view only accepts those).
    let (mut pacemaker, _vss) = test_pacemaker(100_000);
    let mvt = Duration::from_millis(500);

    // Healthy: the QC for view 99_999 justifies the epoch-change view.
    let before = Instant::now();
    pacemaker.extend_view(ViewNumber::new(99_999), ViewNumber::new(99_999)).unwrap();
    let after = Instant::now();
    let deadline = pacemaker.query().deadline;
    assert!(
        deadline >= before + mvt && deadline <= after + mvt,
        "healthy extend must stay the legacy flat max_view_time"
    );

    // Stalled: frontier stuck 4 views behind the justified entry => x16.
    let before = Instant::now();
    pacemaker.extend_view(ViewNumber::new(99_995), ViewNumber::new(99_995)).unwrap();
    let after = Instant::now();
    let deadline = pacemaker.query().deadline;
    assert!(
        deadline >= before + mvt * 16 && deadline <= after + mvt * 16,
        "stalled extend must scale by the stall multiplier"
    );
}

/// Task A (pacemaker backoff): the S395 rebase clamp must not swallow a
/// legitimate backoff. A schedule legitimately rebuilt with a stall multiplier
/// has deadlines up to `max_view_time * multiplier * k` in the future; the
/// clamp threshold therefore scales with the multiplier, so entering the next
/// view keeps the backed-off cumulative grid intact instead of rebasing it
/// away on every entry. (Genuinely stale jumps still rebase — the jump test
/// pins that — because a healthy frontier keeps the multiplier at 1.)
#[test]
fn clamp_preserves_legitimate_backoff() {
    let (mut pacemaker, vss) = test_pacemaker(1);
    let mvt = Duration::from_millis(500);

    // A legitimately backed-off schedule: rebased at view 10 while the QC
    // frontier is stuck at view 5 (stall depth 4 => x16 slots).
    let before = Instant::now();
    pacemaker
        .state
        .update_timeouts(ViewNumber::new(10), &pacemaker.config, ViewNumber::new(5), ViewNumber::new(5));
    let after = Instant::now();

    // Enter view 11 with the same stale frontier (stall depth 5 => x32).
    pacemaker
        .update_view(ViewNumber::new(11), &vss, ViewNumber::new(5), ViewNumber::new(5))
        .unwrap();
    assert!(
        pacemaker.query().deadline >= before + mvt * 32,
        "entered view must keep a backed-off deadline"
    );

    // The discriminator: view 12's cumulative backed-off slot (3 x16 slots
    // from the build time) must survive the entry un-churned.
    let deadline_12 = *pacemaker
        .state
        .timeouts
        .get(&ViewNumber::new(12))
        .unwrap();
    let cumulative = mvt * 16 * 3;
    assert!(
        deadline_12 >= before + cumulative && deadline_12 <= after + cumulative,
        "clamp must not rebase away a legitimately backed-off grid"
    );
}

/// Task A (pacemaker backoff): a schedule rebuilt while the QC frontier is
/// caught up (healthy entry: the QC for view v justifies entering v+1) must be
/// byte-identical to the legacy formula `epoch_start_time + max_view_time *
/// (view − start + 1)` — backoff is invisible on the happy path.
#[test]
fn update_timeouts_gap0_matches_legacy() {
    let (mut pacemaker, _vss) = test_pacemaker(1);
    let mvt = Duration::from_millis(500);

    // Healthy rebase: QC for view 4 justifies entering view 5 (gap 0).
    let start = ViewNumber::new(5);
    let before = Instant::now();
    pacemaker
        .state
        .update_timeouts(start, &pacemaker.config, ViewNumber::new(4), ViewNumber::new(4));
    let after = Instant::now();

    for k in 0..5u32 {
        let view = ViewNumber::new(5 + k as u64);
        let deadline = *pacemaker.state.timeouts.get(&view).unwrap();
        let legacy_increment = mvt * (k + 1);
        assert!(
            deadline >= before + legacy_increment && deadline <= after + legacy_increment,
            "gap-0 schedule must equal the pre-backoff formula (view offset {k})"
        );
    }
}

/// Task A (pacemaker backoff): a schedule rebuilt while views have outrun the
/// QC frontier scales every per-view increment by `factor^min(stall_depth,
/// cap)`, where stall_depth counts the views entered beyond the one the QC
/// justifies.
#[test]
fn update_timeouts_gap_scales() {
    let (mut pacemaker, _vss) = test_pacemaker(1);
    let mvt = Duration::from_millis(500);

    // Entering view 9 with the QC frontier stuck at view 4: views 5..=8 died
    // without a QC => stall depth 4 => increments scale by 2^4.
    let before = Instant::now();
    pacemaker
        .state
        .update_timeouts(ViewNumber::new(9), &pacemaker.config, ViewNumber::new(4), ViewNumber::new(4));
    let after = Instant::now();
    for k in 0..3u32 {
        let view = ViewNumber::new(9 + k as u64);
        let deadline = *pacemaker.state.timeouts.get(&view).unwrap();
        let scaled_increment = mvt * 16 * (k + 1);
        assert!(
            deadline >= before + scaled_increment && deadline <= after + scaled_increment,
            "stalled schedule must scale increments by factor^stall_depth (view offset {k})"
        );
    }

    // Deep stall: the exponent saturates at backoff_cap (2^8).
    let before = Instant::now();
    pacemaker.state.update_timeouts(
        ViewNumber::new(200),
        &pacemaker.config,
        ViewNumber::new(100),
        ViewNumber::new(100),
    );
    let after = Instant::now();
    let deadline = *pacemaker.state.timeouts.get(&ViewNumber::new(200)).unwrap();
    assert!(
        deadline >= before + mvt * 256 && deadline <= after + mvt * 256,
        "deep-stall multiplier must cap at factor^backoff_cap"
    );
}

/// Task A (pacemaker backoff): the multiplier is a pure function of
/// `(view, highest_qc_view, factor, cap)` — no node-local timing state — so
/// every honest replica derives the identical schedule (lockstep-exact).
#[test]
fn backoff_multiplier_table() {
    let v = ViewNumber::new(1000);

    // Gap 0: QC frontier caught up — neutral.
    assert_eq!(backoff_multiplier(v, v, 2, 8), 1);
    // Gap 3: geometric growth.
    assert_eq!(backoff_multiplier(v + 3, v, 2, 8), 8);
    // Deep stall: exponent capped at `cap`.
    assert_eq!(backoff_multiplier(v + 100, v, 2, 8), 2u32.pow(8));
    // QC frontier ahead of the view: gap saturates to 0 — neutral.
    assert_eq!(backoff_multiplier(v, v + 5, 2, 8), 1);
    // cap = 0 is the runtime kill switch: multiplier constantly 1.
    assert_eq!(backoff_multiplier(v + 100, v, 2, 0), 1);
    // Degenerate factors never zero the schedule: multiplier floors at 1.
    assert_eq!(backoff_multiplier(v + 3, v, 0, 8), 1);
    assert_eq!(backoff_multiplier(v + 3, v, 1, 8), 1);
}

/// Task A (pacemaker backoff): the backoff knobs are fleet-wide configuration
/// threaded from the replica [`Configuration`](crate::replica::Configuration)
/// (genesis-sourced in the node), defaulting to factor=2 / cap=8 — active but
/// neutral while the QC frontier keeps up with the view.
#[test]
fn pacemaker_config_has_backoff_defaults() {
    use crate::{replica::Configuration, types::data_types::BufferSize};
    use ed25519_dalek::SigningKey;
    use rand_core::OsRng;

    let mut csprg = OsRng {};
    let config = Configuration::builder()
        .me(SigningKey::generate(&mut csprg))
        .chain_id(ChainID::new(0))
        .block_sync_request_limit(10)
        .block_sync_server_advertise_time(Duration::from_secs(10))
        .block_sync_response_timeout(Duration::from_secs(3))
        .block_sync_blacklist_expiry_time(Duration::from_secs(10))
        .block_sync_trigger_min_view_difference(2)
        .block_sync_trigger_timeout(Duration::from_secs(60))
        .progress_msg_buffer_capacity(BufferSize::new(1024))
        .epoch_length(EpochLength::new(100))
        .max_view_time(Duration::from_millis(500))
        .log_events(false)
        .build();

    let (_, pacemaker_config, _, _): (_, PacemakerConfiguration, _, _) = config.into();
    assert_eq!(pacemaker_config.backoff_factor, 2);
    assert_eq!(pacemaker_config.backoff_cap, 8);
    // S470: the commit-lag term defaults OFF (cap 0 = kill switch) — a node
    // that never sets the knob runs the exact pre-S470 deadline schedule.
    assert_eq!(pacemaker_config.commit_lag_cap, 0);
}

/// The cumulative schedule is preserved while on/near schedule: a single-step
/// advance right after init keeps its scheduled deadline (~2 view times out)
/// rather than being rebased to one view time.
#[test]
fn update_view_on_schedule_keeps_cumulative_deadline() {
    let (mut pacemaker, vss) = test_pacemaker(1);
    pacemaker
        .update_view(ViewNumber::new(2), &vss, ViewNumber::new(1), ViewNumber::new(1))
        .unwrap();
    let deadline = pacemaker.query().deadline;
    assert!(
        deadline > Instant::now() + Duration::from_millis(600),
        "on-schedule advance must keep the cumulative deadline, not rebase"
    );
    assert!(deadline <= Instant::now() + Duration::from_millis(1100));
}

/// S470 (commit-lag backoff): the commit-lag exponent is a pure function of
/// `(highest_qc_view, committed_view, cap)` — no node-local timing state — so
/// every honest replica derives the identical schedule (lockstep-exact), and
/// `cap = 0` is a hard kill switch.
#[test]
fn commit_lag_exponent_table() {
    let v = |n: u64| ViewNumber::new(n);

    // No lag / lag within the grace window: neutral.
    assert_eq!(commit_lag_exponent(v(100), v(100), 8), 0);
    assert_eq!(commit_lag_exponent(v(100), v(100 - COMMIT_LAG_GRACE), 8), 0);
    // One past the grace window: exponent 1.
    assert_eq!(
        commit_lag_exponent(v(100), v(100 - COMMIT_LAG_GRACE - 1), 8),
        1
    );
    // Deep wedge (the observed shape: qc − committed in the hundreds):
    // exponent saturates at cap.
    assert_eq!(commit_lag_exponent(v(500), v(100), 8), 8);
    assert_eq!(commit_lag_exponent(v(500), v(100), 4), 4);
    // Committed frontier ahead (impossible, but must saturate, not wrap).
    assert_eq!(commit_lag_exponent(v(100), v(200), 8), 0);
    // cap = 0 is the runtime kill switch: exponent constantly 0.
    assert_eq!(commit_lag_exponent(v(500), v(0), 0), 0);
}

/// S470: with `commit_lag_cap = 0` (the shipped default) the combined
/// multiplier equals the pre-S470 stall-only multiplier for EVERY combination
/// of (view, qc frontier, commit frontier) — the schedule is byte-identical
/// to today's, no matter how large the commit lag grows.
#[test]
fn commit_lag_cap_zero_is_exact_today() {
    let (pacemaker, _vss) = test_pacemaker_with_commit_lag_cap(1, 0);
    for view in [1u64, 2, 5, 10, 100, 1_000, 100_000] {
        for qc_gap in [0u64, 1, 2, 3, 10, 300] {
            for lag in [0u64, 1, COMMIT_LAG_GRACE, 10, 160, 100_000] {
                let qc = ViewNumber::new(view.saturating_sub(qc_gap + 1));
                let committed = ViewNumber::new(qc.int().saturating_sub(lag));
                assert_eq!(
                    pacemaker
                        .config
                        .stall_multiplier(ViewNumber::new(view), qc, committed),
                    backoff_multiplier(ViewNumber::new(view), qc + 1, 2, 8),
                    "cap=0 must be byte-identical to the stall-only multiplier \
                     (view={view}, qc={}, committed={})",
                    qc.int(),
                    committed.int(),
                );
            }
        }
    }
}

/// S470: the two exponents combine by MAX — the wedge signature (stall term
/// small, commit lag huge) backs off through the commit-lag term, a plain
/// no-quorum stall (huge stall term, small lag) still backs off through the
/// stall term, and a state tripping both never double-compounds.
#[test]
fn commit_lag_multiplier_combines_by_max() {
    let (pacemaker, _vss) = test_pacemaker_with_commit_lag_cap(1, 8);
    let v = |n: u64| ViewNumber::new(n);

    // Wedge shape: entering view 101 justified by QC(100) — stall depth 0 —
    // while the commit frontier is frozen 10+grace views back: lag exponent
    // wins (2^10 capped to 2^8... lag 14 => exponent min(10, 8) = 8).
    assert_eq!(
        pacemaker
            .config
            .stall_multiplier(v(101), v(100), v(100 - COMMIT_LAG_GRACE - 10)),
        2u32.pow(8)
    );
    // Same shape, shallower: lag = grace + 3 => exponent 3 => x8.
    assert_eq!(
        pacemaker
            .config
            .stall_multiplier(v(101), v(100), v(100 - COMMIT_LAG_GRACE - 3)),
        8
    );
    // No-quorum stall shape: stall depth 4, commit frontier caught up.
    assert_eq!(pacemaker.config.stall_multiplier(v(105), v(100), v(100)), 16);
    // Both tripped: stall depth 4 (x16), lag exponent 3 (x8) => max => x16,
    // NOT x128.
    assert_eq!(
        pacemaker
            .config
            .stall_multiplier(v(105), v(100), v(100 - COMMIT_LAG_GRACE - 3)),
        16
    );
    // Healthy: both neutral.
    assert_eq!(pacemaker.config.stall_multiplier(v(101), v(100), v(98)), 1);
}

/// S470: `update_view` threads the commit frontier — entering a view that is
/// perfectly justified (stall depth 0: the exact wedge signature) with a
/// large commit lag must stretch the deadline by the commit-lag multiplier;
/// with `commit_lag_cap = 0` the same entry must keep the flat schedule.
#[test]
fn update_view_threads_commit_lag() {
    let mvt = Duration::from_millis(500);

    // Knob on: entering view 101 justified by QC(100), commit frontier frozen
    // at view 100 − (grace + 3) => lag exponent 3 => x8.
    let (mut pacemaker, vss) = test_pacemaker_with_commit_lag_cap(1, 8);
    let committed = ViewNumber::new(100 - COMMIT_LAG_GRACE - 3);
    let before = Instant::now();
    pacemaker
        .update_view(ViewNumber::new(101), &vss, ViewNumber::new(100), committed)
        .unwrap();
    let after = Instant::now();
    let deadline = pacemaker.query().deadline;
    assert!(
        deadline >= before + mvt * 8,
        "commit-lagged entry must back off the deadline by the commit-lag multiplier"
    );
    assert!(deadline <= after + mvt * 8);

    // Kill switch: identical entry with cap 0 keeps the flat (rebased)
    // schedule — within 2x max_view_time like any healthy jump entry.
    let (mut pacemaker, vss) = test_pacemaker_with_commit_lag_cap(1, 0);
    pacemaker
        .update_view(ViewNumber::new(101), &vss, ViewNumber::new(100), committed)
        .unwrap();
    assert!(
        pacemaker.query().deadline <= Instant::now() + mvt * 2,
        "cap=0 must keep the exact pre-S470 schedule regardless of commit lag"
    );
}
