/*
    Copyright © 2023, ParallelChain Lab
    Licensed under the Apache License, Version 2.0: http://www.apache.org/licenses/LICENSE-2.0
*/

//! Event-driven implementation of the HotStuff subprotocol, as specified in
//! [`sequence_flow`](super::sequence_flow).
//!
//! Main type: [`HotStuff`].

use std::{collections::HashSet, sync::mpsc::Sender, time::{Duration, Instant, SystemTime}};
use borsh::BorshSerialize;

use ed25519_dalek::VerifyingKey;

use crate::{
    app::{
        App, ProduceBlockRequest, ProduceBlockResponse, ValidateBlockRequest, ValidateBlockResponse,
    },
    block_tree::{
        accessors::internal::{BlockTreeError, BlockTreeSingleton, UpdateResult},
        invariants::{repropose_block, safe_block, safe_nudge, safe_pc},
        pluggables::KVStore,
    },
    events::{
        CollectPCEvent, Event, InsertBlockEvent, NewViewEvent, NudgeEvent, PhaseVoteEvent,
        ProposeEvent, ReceiveNewViewEvent, ReceiveNudgeEvent, ReceivePhaseVoteEvent,
        ReceiveProposalEvent, StartViewEvent,
    },
    hotstuff::{
        messages::{
            BlockDataRequest, BlockDataResponse, HotStuffMessage, NEMessage, NERequest, NewView,
            Nudge, PhaseVote, Proposal, ProposalHeader, ProposalRequest, ProposalResponse,
            PendingBodies, PendingHeaders,
        },
        roles::{is_phase_voter, is_proposer, new_view_recipients_with_reputation},
        types::{valid_nec, NECollector, Phase, PhaseVoteCollector},
    },
    networking::{
        network::{Network, ValidatorSetUpdateHandle},
        sending::SenderHandle,
    },
    pacemaker::implementation::ViewInfo,
    types::{
        block::Block,
        crypto_primitives::Keypair,
        data_types::{BlockHeight, ChainID, CryptoHash, ViewNumber},
        signed_messages::{ActiveCollectorPair, Certificate, SignedMessage},
        validator_set::ValidatorSetState,
    },
};

use super::roles::{phase_vote_recipient_with_reputation, is_proposer_with_reputation};

/// A single participant in the HotStuff subprotocol.
///
/// # Usage
///
/// The `HotStuff` struct is meant to be used in an "event-oriented" fashion (note that "event" here
/// does not refer to the [`Event`] enum defined in the events module, but to "event" in the abstract
/// sense).
///
/// Reflecting this event-orientation, the two most significant crate-public methods of this struct are
/// "event handlers", which are to be called when specific things happen to the replica. These methods
/// are:
/// 1. [`enter_view`](Self::enter_view): called when
///    [`Pacemaker`](crate::pacemaker::protocol::Pacemaker) causes the replica to enter a new view.
/// 2. [`on_receive_msg`](Self::on_receive_msg): called when a new [`HotStuffMessage`] is received.
///
/// Besides these two, HotStuff has one more crate-public method, namely
/// [`is_view_outdated`](Self::is_view_outdated). This should be called after querying `Pacemaker` for
/// the latest [`ViewInfo`] to decide whether or not the `ViewInfo` is truly "new", that is, whether or
/// not it is more up-to-date than the latest `ViewInfo` the `HotStuff` struct has received through its
/// `enter_view` method.
pub(crate) struct HotStuff<N: Network> {
    config: HotStuffConfiguration,
    view_info: ViewInfo,
    proposal_status: ProposalStatus,
    phase_vote_collectors: ActiveCollectorPair<PhaseVoteCollector>,
    sender_handle: SenderHandle<N>,
    validator_set_update_handle: ValidatorSetUpdateHandle<N>,
    event_publisher: Option<Sender<Event>>,
    /// MonadBFT B2: NEC recovery state for async block retrieval / NEC formation.
    recovery_state: RecoveryState,
    /// MonadBFT B2: views for which this validator has already sent an NE message.
    ne_sent_views: HashSet<ViewNumber>,
    /// MonadBFT B3: Track proposals per (view, leader) for equivocation detection.
    /// Maps (view, leader) → first block hash seen from that leader in that view.
    seen_proposals: std::collections::HashMap<(ViewNumber, VerifyingKey), CryptoHash>,
    sync_needed: bool,
    /// Hybrid pipelining: full blocks stored locally by the proposer after broadcasting header.
    pending_bodies: PendingBodies,
    /// Hybrid pipelining: headers received but body not yet fetched. Maps block_hash → header.
    pending_headers: PendingHeaders,
    /// Hybrid pipelining: tracks body fetch retries. Maps block_hash → (last_request_time, retry_count, origin_peer).
    body_fetch_tracker: std::collections::HashMap<CryptoHash, (Instant, u8, VerifyingKey)>,
    /// Hybrid pipelining: bodies received but parent not yet in tree. Retried after each insertion.
    deferred_bodies: PendingBodies,
    /// True when enter_view succeeded but proposal production failed (parent block not yet in tree).
    /// The algorithm loop should retry the proposal after polling messages.
    proposal_deferred: bool,
}

impl<N: Network> HotStuff<N> {
    /// Create a new HotStuff subprotocol participant.
    pub(crate) fn new(
        config: HotStuffConfiguration,
        view_info: ViewInfo,
        sender_handle: SenderHandle<N>,
        validator_set_update_handle: ValidatorSetUpdateHandle<N>,
        init_validator_set_state: ValidatorSetState,
        event_publisher: Option<Sender<Event>>,
    ) -> Self {
        let phase_vote_collectors = <ActiveCollectorPair<PhaseVoteCollector>>::new(
            config.chain_id,
            view_info.view,
            &init_validator_set_state,
        );
        let proposal_status = ProposalStatus::WaitingForProposal;
        Self {
            config,
            view_info,
            proposal_status,
            phase_vote_collectors,
            sender_handle,
            validator_set_update_handle,
            event_publisher,
            recovery_state: RecoveryState::None,
            ne_sent_views: HashSet::new(),
            seen_proposals: std::collections::HashMap::new(),
            sync_needed: false,
            pending_bodies: PendingBodies::new(),
            pending_headers: PendingHeaders::new(),
            body_fetch_tracker: std::collections::HashMap::new(),
            deferred_bodies: PendingBodies::new(),
            proposal_deferred: false,
        }
    }

    pub(crate) fn take_sync_needed(&mut self) -> bool {
        let needed = self.sync_needed;
        self.sync_needed = false;
        needed
    }

    /// Process an `UpdateResult` from `block_tree.update()`:
    /// 1. Call `app.on_committed_block()` for each newly committed block (oldest first).
    /// 2. Forward validator set updates to the network layer.
    fn process_update_result<K: KVStore>(
        &mut self,
        result: UpdateResult,
        block_tree: &BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) {
        for committed_hash in &result.committed_block_hashes {
            if let Ok(Some(committed_block)) = block_tree.block(committed_hash) {
                app.on_committed_block(&committed_block, *committed_hash);
            }
        }
        if let Some(vs_updates) = result.validator_set_updates {
            self.validator_set_update_handle.update_validator_set(vs_updates);
        }
    }

    /// Checks whether the HotStuff internal view is outdated with respect to the view from [`ViewInfo`] provided
    /// by the [`Pacemaker`](crate::pacemaker::protocol::Pacemaker).
    ///
    /// ## Next step
    ///
    /// If this function returns `true`, [`enter_view`](Self::enter_view) should be called with the latest `ViewInfo`
    /// at the soonest possible opportunity.
    pub(crate) fn is_view_outdated(&self, new_view_info: &ViewInfo) -> bool {
        new_view_info.view != self.view_info.view
    }

    pub(crate) fn has_deferred_proposal(&self) -> bool {
        self.proposal_deferred
    }

    pub(crate) fn pending_body(&self, hash: &CryptoHash) -> Option<&Block> {
        self.pending_bodies.get(hash)
    }

    pub(crate) fn take_pending_body(&mut self, hash: &CryptoHash) -> Option<Block> {
        self.pending_bodies.remove(hash)
    }

    /// On receiving a new [`ViewInfo`] from the [`Pacemaker`](crate::pacemaker::protocol::Pacemaker), send
    /// messages and perform state updates associated with exiting the current view, and update the local
    /// view info.
    ///
    /// # Precondition
    ///
    /// [`is_view_outdated`](Self::is_view_outdated) returns true. This is the case when the Pacemaker has updated
    /// `ViewInfo` but the update has not been made available to the [`HotStuff`] struct yet.
    ///
    /// # Specification
    ///
    /// [Enter View](super::sequence_flow#enter-view).
    pub(crate) fn enter_view<K: KVStore>(
        &mut self,
        new_view_info: ViewInfo,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        // Retry deferred proposal: parent body has now arrived, skip view transition.
        if self.proposal_deferred && new_view_info.view == self.view_info.view {
            self.proposal_deferred = false;
            let highest_pc = block_tree.highest_pc()?;
            if matches!(highest_pc.phase, Phase::Generic | Phase::Decide) {
                let validator_set_state = block_tree.validator_set_state()?;
                let reputation = block_tree.leader_reputation().ok();
                if is_proposer_with_reputation(
                    &self.config.keypair.public(),
                    self.view_info.view,
                    &validator_set_state,
                    reputation.as_ref(),
                ) {
                    let (parent_block, child_height) = if highest_pc.is_genesis_pc() {
                        (None, BlockHeight::new(0))
                    } else {
                        match block_tree.block_height(&highest_pc.block)? {
                            Some(h) => (Some(highest_pc.block), h + 1),
                            None => {
                                self.proposal_deferred = true;
                                return Ok(());
                            }
                        }
                    };

                    let produce_block_request = ProduceBlockRequest::new(
                        self.view_info.view,
                        parent_block,
                        block_tree.app_view(parent_block.as_ref())?,
                    );
                    let ProduceBlockResponse {
                        data,
                        data_hash,
                        app_state_updates,
                        validator_set_updates,
                    } = app.produce_block(produce_block_request);

                    let block = Block::new(child_height, highest_pc, data_hash, data);
                    block_tree.insert(&block, app_state_updates.as_ref(), validator_set_updates.as_ref())?;
                    Event::InsertBlock(InsertBlockEvent {
                        timestamp: SystemTime::now(),
                        block: block.clone(),
                    }).publish(&self.event_publisher);
                    let update_result = block_tree.update(&block.justify, &self.event_publisher).unwrap_or_else(|e| {
                        if let BlockTreeError::BlockExpectedButNotFound { block: missing } = &e {
                            log::warn!("enter_view: missing block {:?} in commit chain — triggering sync", missing);
                            self.sync_needed = true;
                        } else {
                            log::warn!("enter_view: block_tree.update failed after self-insert: {:?}", e);
                        }
                        UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] }
                    });
                    self.process_update_result(update_result, block_tree, app);

                    let proposal = Proposal {
                        chain_id: self.config.chain_id,
                        view: self.view_info.view,
                        block,
                        tc: None,
                        nec: None,
                    };
                    self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
                    self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
                    self.sender_handle.broadcast::<HotStuffMessage>(proposal.clone().into());
                    Event::Propose(ProposeEvent {
                        timestamp: SystemTime::now(),
                        proposal,
                    }).publish(&self.event_publisher);
                }
            }
            return Ok(());
        }

        let validator_set_state = block_tree.validator_set_state()?;

        // 1. Send a NewView message for the current view to the next leader(s).
        let new_view = NewView {
            chain_id: self.config.chain_id,
            view: self.view_info.view,
            highest_pc: block_tree.highest_pc()?,
        };

        // FIX CONS-PF-10: Use reputation-weighted leader for NewView routing.
        let reputation = block_tree.leader_reputation().ok();
        match new_view_recipients_with_reputation(&new_view, &validator_set_state, reputation.as_ref()) {
            (committed_vs_leader, None) => self
                .sender_handle
                .send::<HotStuffMessage>(committed_vs_leader, new_view.clone().into()),
            (committed_vs_leader, Some(prev_vs_leader)) => {
                self.sender_handle
                    .send::<HotStuffMessage>(committed_vs_leader, new_view.clone().into());
                self.sender_handle
                    .send::<HotStuffMessage>(prev_vs_leader, new_view.clone().into());
            }
        }

        Event::NewView(NewViewEvent {
            timestamp: SystemTime::now(),
            new_view,
        })
        .publish(&self.event_publisher);

        // 2. Update the struct's internal view info, proposal status, and vote collectors to collect
        //    votes for the new view.
        self.view_info = new_view_info;
        self.proposal_status = ProposalStatus::WaitingForProposal;
        // MonadBFT B2: Cancel any pending recovery when entering a new view.
        self.recovery_state = RecoveryState::None;
        self.phase_vote_collectors = <ActiveCollectorPair<PhaseVoteCollector>>::new(
            self.config.chain_id,
            self.view_info.view,
            &validator_set_state,
        );

        // FIX CONS-PF-15: Evict stale entries from seen_proposals to bound memory.
        // Equivocation detection only needs current/recent views; 100-view window is generous.
        let cutoff = ViewNumber::new(self.view_info.view.int().saturating_sub(100));
        self.seen_proposals.retain(|&(view, _), _| view >= cutoff);

        // 3. Set `highest_view_entered` in the block tree to the new view, then emit a `StartView` event.
        block_tree.set_highest_view_entered(self.view_info.view)?;

        Event::StartView(StartViewEvent {
            timestamp: SystemTime::now(),
            view: self.view_info.view.clone(),
        })
        .publish(&self.event_publisher);

        // 4. If I am a proposer for the new view, then broadcast a `Proposal` or a `Nudge`.
        //    If the parent block isn't in the tree yet (body fetch in progress),
        //    defer the proposal and retry after message polling.
        self.proposal_deferred = false;
        let reputation = block_tree.leader_reputation().ok();
        let am_proposer = is_proposer_with_reputation(
            &self.config.keypair.public(),
            self.view_info.view,
            &validator_set_state,
            reputation.as_ref(),
        );
        if am_proposer {
            // If a chain of consecutive views of voting for a validator-set-updating block has been interrupted, then
            // re-propose an existing block.
            if let Some(block_hash) = repropose_block(self.view_info.view, block_tree)? {
                let block = block_tree
                    .block(&block_hash)?
                    .ok_or(BlockTreeError::BlockExpectedButNotFound { block: block_hash })?;

                let proposal = Proposal {
                    chain_id: self.config.chain_id,
                    view: self.view_info.view,
                    block,
                    tc: None,
                    nec: None,
                };

                self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
                self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
                self.sender_handle
                    .broadcast::<HotStuffMessage>(proposal.clone().into());

                Event::Propose(ProposeEvent {
                    timestamp: SystemTime::now(),
                    proposal,
                })
                .publish(&self.event_publisher);

                return Ok(());
            }

            // MonadBFT: Check if the previous view timed out and produced a TC.
            if let Some(tc) = block_tree.highest_tc()? {
                if tc.view + 1 == self.view_info.view {
                    if let Some(proposal) = self.create_proposal_based_on_tc(
                        &tc, block_tree, app,
                    )? {
                        self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
                        self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
                        self.sender_handle
                            .broadcast::<HotStuffMessage>(proposal.clone().into());
                        Event::Propose(ProposeEvent {
                            timestamp: SystemTime::now(),
                            proposal,
                        })
                        .publish(&self.event_publisher);
                        return Ok(());
                    }
                }
            }

            // Otherwise, propose a new block or nudge based on highest_pc.
            let highest_pc = block_tree.highest_pc()?;
            match highest_pc.phase {
                // Produce and broadcast a new Proposal.
                Phase::Generic | Phase::Decide => {
                    let (parent_block, child_height) = if highest_pc.is_genesis_pc() {
                        (None, BlockHeight::new(0))
                    } else {
                        match block_tree.block_height(&highest_pc.block)? {
                            Some(parent_height) => (Some(highest_pc.block), parent_height + 1),
                            None => {
                                self.proposal_deferred = true;
                                return Ok(());
                            }
                        }
                    };

                    let produce_block_request = ProduceBlockRequest::new(
                        self.view_info.view,
                        parent_block,
                        block_tree.app_view(parent_block.as_ref())?,
                    );

                    let ProduceBlockResponse {
                        data,
                        data_hash,
                        app_state_updates,
                        validator_set_updates,
                    } = app.produce_block(produce_block_request);

                    let block = Block::new(child_height, highest_pc, data_hash, data);

                    // Proposer self-inserts before broadcasting: ensures the block
                    // is in the tree before QC forms and next view references it.
                    block_tree.insert(
                        &block,
                        app_state_updates.as_ref(),
                        validator_set_updates.as_ref(),
                    )?;
                    Event::InsertBlock(InsertBlockEvent {
                        timestamp: SystemTime::now(),
                        block: block.clone(),
                    })
                    .publish(&self.event_publisher);

                    let update_result =
                        block_tree.update(&block.justify, &self.event_publisher)
                            .unwrap_or(UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] });
                    self.process_update_result(update_result, block_tree, app);

                    let proposal = Proposal {
                        chain_id: self.config.chain_id,
                        view: self.view_info.view,
                        block,
                        tc: None,
                        nec: None,
                    };

                    self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
                    self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
                    self.sender_handle
                        .broadcast::<HotStuffMessage>(proposal.clone().into());

                    Event::Propose(ProposeEvent {
                        timestamp: SystemTime::now(),
                        proposal,
                    })
                    .publish(&self.event_publisher);
                }
                // Produce and broadcast a Nudge.
                Phase::Prepare | Phase::Precommit | Phase::Commit => {
                    let nudge = Nudge {
                        chain_id: self.config.chain_id,
                        view: self.view_info.view,
                        justify: highest_pc,
                    };

                    self.sender_handle
                        .broadcast::<HotStuffMessage>(nudge.clone().into());

                    Event::Nudge(NudgeEvent {
                        timestamp: SystemTime::now(),
                        nudge,
                    })
                    .publish(&self.event_publisher)
                }
            }
        }

        Ok(())
    }

    /// MonadBFT Algorithm 4: CREATEPROPOSALBASEDONTC.
    ///
    /// Case 3 (high_qc wins): fresh proposal extending high_qc — handled by caller.
    /// Case 4 (high_tip wins, block available): repropose the block.
    /// Case 5 (high_tip wins, block NOT available): initiate RECOVER (Algorithm 7).
    ///   RECOVER is async: sends ProposalRequest/NERequest, stores RecoveryState,
    ///   returns None. The event loop handles responses via on_receive_proposal_response
    ///   and on_receive_ne. If the view timer expires, recovery is abandoned.
    fn create_proposal_based_on_tc<K: KVStore>(
        &mut self,
        tc: &crate::pacemaker::types::TimeoutCertificate,
        block_tree: &mut BlockTreeSingleton<K>,
        _app: &mut impl App<K>,
    ) -> Result<Option<Proposal>, HotStuffError> {
        if tc.high_tip_is_winner {
            if let Some(ref tip) = tc.high_tip {
                // Case 4: leader has the block — repropose.
                if let Some(block) = block_tree.block(&tip.block_hash)? {
                    return Ok(Some(Proposal {
                        chain_id: self.config.chain_id,
                        view: self.view_info.view,
                        block,
                        tc: Some(tc.clone()),
                        nec: None,
                    }));
                }

                // Case 5: leader lacks the block — initiate RECOVER (Algorithm 7).
                // This is async: we send requests and return None. The event loop
                // handles responses to complete the proposal.
                let validator_set_state = block_tree.validator_set_state()?;
                let validator_set = validator_set_state.committed_validator_set().clone();
                let total_power = validator_set.total_power().int() as u64;
                let f = if total_power > 0 { (total_power - 1) / 3 } else { 0 };
                let kappa = (f + 1) as usize; // κ = f+1 guarantees at least one honest responder

                // Step 1: Send ProposalRequest to κ validators (prefer those from TC's tips_views).
                let req = ProposalRequest {
                    chain_id: self.config.chain_id,
                    view: self.view_info.view,
                    tc: tc.clone(),
                };
                // FIX CONS-FIND-28: Shuffle recipients to prevent deterministic withholding.
                let mut recipients: Vec<_> = validator_set.validators_and_powers()
                    .into_iter()
                    .map(|(vk, _)| vk)
                    .filter(|vk| *vk != self.config.keypair.public())
                    .collect();

                // Deterministic shuffle seeded by view number.
                let seed = self.view_info.view.int();
                let len = recipients.len();
                if len > 1 {
                    for i in (1..len).rev() {
                        let j = (seed.wrapping_mul(6364136223846793005).wrapping_add(i as u64)
                            % (i as u64 + 1)) as usize;
                        recipients.swap(i, j);
                    }
                }

                for vk in recipients.into_iter().take(kappa) {
                    self.sender_handle.send::<HotStuffMessage>(
                        vk,
                        req.clone().into(),
                    );
                }

                // Step 2: Broadcast NERequest to ALL validators.
                let ne_req = NERequest {
                    chain_id: self.config.chain_id,
                    view: self.view_info.view,
                    tc: tc.clone(),
                };
                self.sender_handle.broadcast::<HotStuffMessage>(ne_req.into());

                // Step 3: Store recovery state for async completion.
                let high_tip_qc_view = tip.block_justify.view;
                self.recovery_state = RecoveryState::Recovering {
                    tc: tc.clone(),
                    ne_collector: NECollector::new(
                        self.view_info.view,
                        high_tip_qc_view,
                        validator_set,
                    ),
                };
            }
        }
        Ok(None)
    }

    /// Process a newly received message for the current view according to the HotStuff subprotocol.
    ///
    /// ## Internal procedure
    ///
    /// This function executes the following steps:
    /// 1. If `msg` is a `Proposal` or a `Nudge`, check if the sender is a proposer for the current view
    ///    and check if the replica is still accepting nudges and proposals. If these checks fail, return
    ///    immediately.
    /// 2. If the checks pass, call one of the following 4 internal event handlers depending on the variant
    ///    of the received message:
    ///     - [`on_receive_proposal`](Self::on_receive_proposal).
    ///     - [`on_receive_nudge`](Self::on_receive_nudge).
    ///     - [`on_receive_phase_vote`](Self::on_receive_phase_vote).
    ///     - [`on_receive_new_view`](Self::on_receive_new_view).
    pub(crate) fn on_receive_msg<K: KVStore>(
        &mut self,
        msg: HotStuffMessage,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        let msg_type = match &msg {
            HotStuffMessage::Proposal(_) => "Proposal",
            HotStuffMessage::Nudge(_) => "Nudge",
            HotStuffMessage::PhaseVote(_) => "PhaseVote",
            HotStuffMessage::NewView(_) => "NewView",
            _ => "Other",
        };
        log::info!("on_receive_msg: type={}, view={}", msg_type, self.view_info.view.int());
        if matches!(msg, HotStuffMessage::Proposal(_) | HotStuffMessage::Nudge(_) | HotStuffMessage::ProposalHeader(_)) {
            let validator_set_state = block_tree.validator_set_state()?;

            // For ProposalHeaders that bypass view filtering, verify the sender
            // was the proposer for the HEADER's view (not the local view).
            let check_view = match &msg {
                HotStuffMessage::ProposalHeader(h) => h.view,
                _ => self.view_info.view,
            };

            // MonadBFT B3: Use reputation-weighted leader selection.
            let reputation = block_tree.leader_reputation().ok();
            if !is_proposer_with_reputation(
                origin,
                check_view,
                &validator_set_state,
                reputation.as_ref(),
            ) {
                return Ok(());
            }

            // Skip duplicate proposals for the current view (not applicable to
            // late-arriving ProposalHeaders from past views).
            if !matches!(msg, HotStuffMessage::ProposalHeader(_)) {
                if self.proposal_status.has_one_leader_proposed(origin)
                    || self.proposal_status.have_all_leaders_proposed()
                {
                    return Ok(());
                }
            }
        }

        // 2. If the check above pass, process the message.
        match msg {
            HotStuffMessage::Proposal(proposal) => {
                self.on_receive_proposal(proposal, origin, block_tree, app)
            }
            HotStuffMessage::Nudge(nudge) => self.on_receive_nudge(nudge, origin, block_tree, app),
            HotStuffMessage::PhaseVote(vote) => {
                self.on_receive_phase_vote(vote, origin, block_tree, app)
            }
            HotStuffMessage::NewView(new_view) => {
                self.on_receive_new_view(new_view, origin, block_tree, app)
            }
            // MonadBFT B2: NEC recovery messages
            HotStuffMessage::ProposalRequest(req) => {
                self.on_receive_proposal_request(req, origin, block_tree)
            }
            HotStuffMessage::ProposalResponse(resp) => {
                self.on_receive_proposal_response(resp, origin, block_tree, app)
            }
            HotStuffMessage::NERequest(req) => {
                self.on_receive_ne_request(req, origin, block_tree)
            }
            HotStuffMessage::NE(ne) => {
                self.on_receive_ne(ne, origin, block_tree, app)
            }
            // Hybrid pipelining
            HotStuffMessage::ProposalHeader(header) => {
                self.on_receive_proposal_header(header, origin, block_tree, app)
            }
            HotStuffMessage::BlockDataRequest(req) => {
                self.on_receive_block_data_request(req, origin, block_tree)
            }
            HotStuffMessage::BlockDataResponse(resp) => {
                self.on_receive_block_data_response(resp, origin, block_tree, app)
            }
        }
    }

    /// Process a newly received `proposal`.
    ///
    /// # Preconditions
    ///
    /// [`is_proposer(origin, self.view_info.view, &block_tree.validator_set_state()?)`](is_proposer).
    ///
    /// # Specification
    ///
    /// [On Receive Proposal](super::sequence_flow#on-receive-proposal).
    fn on_receive_proposal<K: KVStore>(
        &mut self,
        proposal: Proposal,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        Event::ReceiveProposal(ReceiveProposalEvent {
            timestamp: SystemTime::now(),
            origin: *origin,
            proposal: proposal.clone(),
        })
        .publish(&self.event_publisher);

        // MonadBFT B3: Equivocation detection — check if we've seen a different
        // block from this leader in this view. If so, this is leader equivocation.
        let proposal_key = (proposal.view, *origin);
        if let Some(first_block_hash) = self.seen_proposals.get(&proposal_key) {
            if *first_block_hash != proposal.block.hash {
                // EQUIVOCATION DETECTED: same leader, same view, different block.
                let evidence = super::types::EquivocationEvidence {
                    view: proposal.view,
                    leader: *origin,
                    block_a: *first_block_hash,
                    block_b: proposal.block.hash,
                };

                Event::EquivocationDetected(crate::events::EquivocationDetectedEvent {
                    timestamp: SystemTime::now(),
                    equivocator: *origin,
                    view: proposal.view,
                    block_a: *first_block_hash,
                    block_b: proposal.block.hash,
                })
                .publish(&self.event_publisher);

                // Store evidence persistently (survives rollback).
                let _ = block_tree.store_equivocation_evidence(&evidence);

                // Check if the FIRST block was speculatively committed — if so, roll it back.
                if block_tree.is_speculatively_committed(first_block_hash)? {
                    let rolled_back = block_tree.rollback_speculative_block(first_block_hash)?;
                    if rolled_back {
                        Event::RollbackBlock(crate::events::RollbackBlockEvent {
                            timestamp: SystemTime::now(),
                            block: *first_block_hash,
                            view: proposal.view,
                            equivocator: *origin,
                        })
                        .publish(&self.event_publisher);

                        // Notify the application to revert state changes.
                        app.on_speculative_rollback(*first_block_hash, &evidence);
                    }
                }

                // Also check if the SECOND (new) block is somehow speculative.
                if block_tree.is_speculatively_committed(&proposal.block.hash)? {
                    let rolled_back =
                        block_tree.rollback_speculative_block(&proposal.block.hash)?;
                    if rolled_back {
                        Event::RollbackBlock(crate::events::RollbackBlockEvent {
                            timestamp: SystemTime::now(),
                            block: proposal.block.hash,
                            view: proposal.view,
                            equivocator: *origin,
                        })
                        .publish(&self.event_publisher);
                        app.on_speculative_rollback(proposal.block.hash, &evidence);
                    }
                }

                // Don't process the equivocating proposal further.
                return Ok(());
            }
            // Same block hash = duplicate, handled by ProposalStatus below.
        } else {
            self.seen_proposals.insert(proposal_key, proposal.block.hash);
        }

        // MonadBFT B2: validate NEC if present.
        // Check NEC BEFORE reproposal — per paper Alg 5 VALIDPROPOSAL line 15,
        // IsFreshProposal is checked first. A valid NEC makes the proposal fresh
        // regardless of tc.high_tip_is_winner.
        let has_valid_nec = if let Some(ref nec) = proposal.nec {
            if !valid_nec(nec, block_tree)? {
                match self.proposal_status {
                    ProposalStatus::WaitingForProposal => {
                        self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
                    }
                    ProposalStatus::OneLeaderProposed { leader: _ } => {
                        self.proposal_status = ProposalStatus::AllLeadersProposed
                    }
                    _ => {}
                }
                return Ok(());
            }
            true
        } else {
            false
        };

        // MonadBFT: validate reproposal TC if present.
        // Skip this check if the proposal has a valid NEC (it's fresh, not a reproposal).
        if !has_valid_nec && proposal.is_reproposal() {
            if let Some(ref tc) = proposal.tc {
                let tc_valid = tc.is_correct(block_tree)?
                    && self.view_info.view == tc.view + 1
                    && tc.high_tip.as_ref().map_or(false, |tip| tip.block_hash == proposal.block.hash);
                if !tc_valid {
                    match self.proposal_status {
                        ProposalStatus::WaitingForProposal => {
                            self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
                        }
                        ProposalStatus::OneLeaderProposed { leader: _ } => {
                            self.proposal_status = ProposalStatus::AllLeadersProposed
                        }
                        _ => {}
                    }
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }

        // 1. Check if block is correct and safe.
        let is_correct = proposal.block.is_correct(block_tree)?;
        let is_safe = if is_correct { safe_block(&proposal.block, block_tree, self.config.chain_id)? } else { false };
        if !is_correct || !is_safe {
            let justify_block_known = block_tree.contains(&proposal.block.justify.block);
            log::warn!(
                "dropping proposal: view={}, is_correct={}, is_safe={}, justify_block_known={}",
                self.view_info.view.int(),
                is_correct,
                is_safe,
                justify_block_known,
            );
            if !justify_block_known {
                self.sync_needed = true;
            }
            // Ensure that proposals or nudges from this leader should no longer be accepted in this view.
            match self.proposal_status {
                ProposalStatus::WaitingForProposal => {
                    self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
                }
                ProposalStatus::OneLeaderProposed { leader: _ } => {
                    self.proposal_status = ProposalStatus::AllLeadersProposed
                }
                _ => {}
            }
            return Ok(());
        }

        // 2. Validate the block using the app, and insert it into the block tree if it is valid.
        let parent_block = if proposal.block.justify.is_genesis_pc() {
            None
        } else {
            Some(&proposal.block.justify.block)
        };
        let validate_block_request =
            ValidateBlockRequest::new(&proposal.block, block_tree.app_view(parent_block)?);

        if let ValidateBlockResponse::Valid {
            app_state_updates,
            validator_set_updates,
        } = app.validate_block(validate_block_request)
        {
            block_tree.insert(
                &proposal.block,
                app_state_updates.as_ref(),
                validator_set_updates.as_ref(),
            )?;
            Event::InsertBlock(InsertBlockEvent {
                timestamp: SystemTime::now(),
                block: proposal.block.clone(),
            })
            .publish(&self.event_publisher);

            // 3. Trigger block tree updates: update highestPC, lock, commit.
            let update_result =
                block_tree.update(&proposal.block.justify, &self.event_publisher)
                    .unwrap_or(UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] });
            self.process_update_result(update_result, block_tree, app);

            // 4. Access the possibly updated validator set state, and update the vote collectors if needed.
            let validator_set_state = block_tree.validator_set_state()?;

            let _ = self
                .phase_vote_collectors
                .update_validator_sets(&validator_set_state);

            // 5. Vote, if I am allowed to vote and if I haven't voted in this view yet.
            if is_phase_voter(
                &self.config.keypair.public(),
                &validator_set_state,
                &proposal.block.justify,
            ) && (block_tree.highest_view_voted()?.is_none()
                || block_tree.highest_view_voted()?.unwrap() < self.view_info.view)
            {
                let vote_phase = if validator_set_updates.is_some() {
                    Phase::Prepare
                } else {
                    Phase::Generic
                };

                let phase_vote = PhaseVote::new(
                    &self.config.keypair,
                    self.config.chain_id,
                    self.view_info.view,
                    proposal.block.hash,
                    vote_phase,
                );
                // FIX CONS-FIND-13: Use reputation-weighted vote recipient.
                let reputation = block_tree.leader_reputation().ok();
                let vote_recipient = phase_vote_recipient_with_reputation(&phase_vote, &validator_set_state, reputation.as_ref());
                self.sender_handle
                    .send::<HotStuffMessage>(vote_recipient, phase_vote.clone().into());

                // FIX CONS-FIND-16: Atomic write of vote state to prevent crash inconsistency.
                block_tree.set_vote_state_atomic(self.view_info.view, proposal.block.hash)?;

                // MonadBFT: Update local_tip (paper Alg 1, line 13: local_tip ← GetTip(p)).
                // For fresh proposals: tip is the proposal itself.
                // For reproposals: tip is tc.high_tip (paper Alg 6, line 15).
                if !proposal.is_reproposal() && vote_phase == Phase::Generic {
                    use crate::pacemaker::types::TipInfo;
                    let tip = TipInfo {
                        block_hash: proposal.block.hash,
                        block_height: proposal.block.height,
                        block_justify: proposal.block.justify.clone(),
                        block_data_hash: proposal.block.data_hash,
                        view: self.view_info.view,
                    };
                    block_tree.set_local_tip(&tip)?;
                } else if proposal.is_reproposal() {
                    // Reproposal: set local_tip to tc.high_tip per paper GetTip().
                    if let Some(ref tc) = proposal.tc {
                        if let Some(ref tip) = tc.high_tip {
                            block_tree.set_local_tip(tip)?;
                        }
                    }
                }

                Event::PhaseVote(PhaseVoteEvent {
                    timestamp: SystemTime::now(),
                    vote: phase_vote.clone(),
                })
                .publish(&self.event_publisher)
            }
        }

        // 6. Stop accepting proposals or nudges from this leader in this view.
        match self.proposal_status {
            ProposalStatus::WaitingForProposal => {
                self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
            }
            ProposalStatus::OneLeaderProposed { leader: _ } => {
                self.proposal_status = ProposalStatus::AllLeadersProposed
            }
            _ => {}
        }

        Ok(())
    }

    /// Process the received nudge.
    ///
    /// # Preconditions
    ///
    /// [`is_proposer(origin, self.view_info.view, &block_tree.validator_set_state()?)`](is_proposer).
    ///
    /// # Specification
    ///
    /// [On Receive Nudge](super::sequence_flow#on-receive-nudge).
    fn on_receive_nudge<K: KVStore>(
        &mut self,
        nudge: Nudge,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        Event::ReceiveNudge(ReceiveNudgeEvent {
            timestamp: SystemTime::now(),
            origin: *origin,
            nudge: nudge.clone(),
        })
        .publish(&self.event_publisher);

        // 1. Check if the nudge is correct and safe.
        if !nudge.justify.is_correct(block_tree)?
            || !safe_nudge(
                &nudge,
                self.view_info.view,
                block_tree,
                self.config.chain_id,
            )?
        {
            // Take note that proposals or nudges from this leader should no longer be accepted in this view.
            match self.proposal_status {
                ProposalStatus::WaitingForProposal => {
                    self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
                }
                ProposalStatus::OneLeaderProposed { leader: _ } => {
                    self.proposal_status = ProposalStatus::AllLeadersProposed
                }
                _ => {}
            }
            return Ok(());
        }

        // 2. Trigger block tree updates: update highestPC, lock, commit.
        let update_result =
            block_tree.update(&nudge.justify, &self.event_publisher)
                .unwrap_or(UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] });
        self.process_update_result(update_result, block_tree, app);

        // 3. Access the possibly updated validator set state, and update the vote collectors if needed.
        let validator_set_state = block_tree.validator_set_state()?;

        let _ = self
            .phase_vote_collectors
            .update_validator_sets(&validator_set_state);

        // FIX CONS-FIND-27: Update local_tip for nudge-certified blocks.
        {
            use crate::pacemaker::types::TipInfo;
            if let Ok(Some(block)) = block_tree.block(&nudge.justify.block) {
                let tip = TipInfo {
                    block_hash: block.hash,
                    block_height: block.height,
                    block_justify: nudge.justify.clone(),
                    block_data_hash: block.data_hash,
                    view: self.view_info.view,
                };
                block_tree.set_local_tip(&tip)?;
            }
        }

        // 4. Vote, if I am allowed to vote and if I haven't voted in this view yet.
        if is_phase_voter(
            &self.config.keypair.public(),
            &validator_set_state,
            &nudge.justify,
        ) && (block_tree.highest_view_voted()?.is_none()
            || block_tree.highest_view_voted()?.unwrap() < self.view_info.view)
        {
            let vote_phase = match nudge.justify.phase {
                Phase::Prepare => Phase::Precommit,
                Phase::Precommit => Phase::Commit,
                Phase::Commit => Phase::Decide,
                _ => unreachable!("if `safe_nudge` check passed then `vote_phase` should be either `Precommit`, `Commit`, or `Decide`"),
            };

            let vote = PhaseVote::new(
                &self.config.keypair,
                self.config.chain_id,
                self.view_info.view,
                nudge.justify.block,
                vote_phase,
            );
            // FIX CONS-FIND-13: Use reputation-weighted vote recipient.
            let reputation = block_tree.leader_reputation().ok();
            let vote_recipient = phase_vote_recipient_with_reputation(&vote, &validator_set_state, reputation.as_ref());
            self.sender_handle
                .send::<HotStuffMessage>(vote_recipient, vote.clone().into());

            block_tree.set_highest_view_phase_voted(self.view_info.view)?;
            Event::PhaseVote(PhaseVoteEvent {
                timestamp: SystemTime::now(),
                vote: vote.clone(),
            })
            .publish(&self.event_publisher);
        }

        // 5. Stop accepting proposals or nudges from this leader in this view.
        match self.proposal_status {
            ProposalStatus::WaitingForProposal => {
                self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
            }
            ProposalStatus::OneLeaderProposed { leader: _ } => {
                self.proposal_status = ProposalStatus::AllLeadersProposed
            }
            _ => {}
        }

        Ok(())
    }

    /// Process a received `phase_vote`.
    ///
    /// # Specification
    ///
    /// [On Receive Phase Vote](super::sequence_flow#on-receive-phase-vote).
    fn on_receive_phase_vote<K: KVStore>(
        &mut self,
        phase_vote: PhaseVote,
        signer: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        Event::ReceivePhaseVote(ReceivePhaseVoteEvent {
            timestamp: SystemTime::now(),
            origin: *signer,
            phase_vote: phase_vote.clone(),
        })
        .publish(&self.event_publisher);

        // 1. Collect the vote if correct.
        if phase_vote.is_correct(signer) {
            if let Some(new_pc) = self.phase_vote_collectors.collect(signer, phase_vote) {
                Event::CollectPC(CollectPCEvent {
                    timestamp: SystemTime::now(),
                    phase_certificate: new_pc.clone(),
                })
                .publish(&self.event_publisher);

                // If the newly collected PC is not correct or not safe, then ignore it and return.
                // In the header pipeline, the PC's block may not be in the tree
                // yet (body in flight) but is tracked in pending_headers.
                let pc_block_pending = self.pending_headers.contains_key(&new_pc.block)
                    || self.pending_bodies.contains_key(&new_pc.block);
                let pc_safe = if pc_block_pending {
                    new_pc.is_correct(block_tree)?
                } else {
                    new_pc.is_correct(block_tree)?
                        && safe_pc(&new_pc, block_tree, self.config.chain_id)?
                };
                if !pc_safe {
                    return Ok(());
                }

                // 2. Trigger block tree updates: update highestPC, lock, commit (if new PC collected).
                //    In the header pipeline the certified block may not be in
                //    the tree yet (body in flight). Always advance highest_pc
                //    first, then attempt full update for commits.
                let _ = block_tree.advance_highest_pc_from_remote(&new_pc);
                let update_result =
                    block_tree.update(&new_pc, &self.event_publisher)
                        .unwrap_or(UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] });
                self.process_update_result(update_result, block_tree, app);


                // MonadBFT B2: Speculative commit — 1-QC for fresh proposals.
                // A block qualifies for speculative commit if it was a fresh
                // proposal (happy path: justify.view == pc.view - 1).
                // Speculative commit means "probably final, can only revert if
                // leader equivocated." Actual rollback is B3.
                if new_pc.phase.is_generic() && new_pc.view.int() > 0 {
                    if let Ok(block_justify) = block_tree.block_justify(&new_pc.block) {
                        if block_justify.view == new_pc.view - 1 {
                            let _ = block_tree.add_speculative_commit(new_pc.block);
                        }
                    }
                }

                // 3. Access the possibly updated validator set state, and update the vote collectors if needed
                // (if new PC collected).
                let validator_set_state = block_tree.validator_set_state()?;

                let _ = self
                    .phase_vote_collectors
                    .update_validator_sets(&validator_set_state);

                // Broadcast AdvanceView with the new PC so all validators can
                // advance. In the header pipeline, non-proposers don't call
                // block_tree.update() until bodies arrive, so this broadcast is
                // the primary mechanism for view synchronization.
                if new_pc.phase.is_generic() {
                    use crate::pacemaker::messages::ProgressCertificate;
                    let advance_msg = crate::pacemaker::messages::PacemakerMessage::advance_view(
                        ProgressCertificate::PhaseCertificate(new_pc.clone()),
                    );
                    self.sender_handle
                        .broadcast::<crate::networking::messages::Message>(advance_msg.into());
                }
            }
        }

        Ok(())
    }

    /// Process the received NewView.
    ///
    /// # Specification
    ///
    /// [On Receive New View](super::sequence_flow#on-receive-new-view).
    fn on_receive_new_view<K: KVStore>(
        &mut self,
        new_view: NewView,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        Event::ReceiveNewView(ReceiveNewViewEvent {
            timestamp: SystemTime::now(),
            origin: *origin,
            new_view: new_view.clone(),
        })
        .publish(&self.event_publisher);

        // 1. Check if the highest_pc in the NewView message is correct and safe.
        if new_view.highest_pc.is_correct(block_tree)?
            && safe_pc(&new_view.highest_pc, block_tree, self.config.chain_id)?
        {
            // 2. Trigger block tree updates: update highestPC, lock, commit (if new PC collected).
            let update_result =
                block_tree.update(&new_view.highest_pc, &self.event_publisher)
                    .unwrap_or(UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] });
            self.process_update_result(update_result, block_tree, app);

            // 3. Access the possibly updated validator set state, and update the phase vote collectors if needed
            // (if new PC collected).
            let validator_set_state = block_tree.validator_set_state()?;

            let _ = self
                .phase_vote_collectors
                .update_validator_sets(&validator_set_state);
        }

        Ok(())
    }

    // ========================================================================
    // MonadBFT B2: NEC recovery message handlers
    // ========================================================================

    /// PROCESSNEREQUST: On receiving a ProposalRequest, respond with the block if we have it.
    fn on_receive_proposal_request<K: KVStore>(
        &mut self,
        req: ProposalRequest,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), HotStuffError> {
        if req.view != self.view_info.view {
            return Ok(());
        }
        // If we have the requested high_tip block, send it back.
        if let Some(ref tip) = req.tc.high_tip {
            if let Some(block) = block_tree.block(&tip.block_hash)? {
                let response = ProposalResponse {
                    chain_id: req.chain_id,
                    view: req.view,
                    proposal: Proposal {
                        chain_id: req.chain_id,
                        view: req.view,
                        block,
                        tc: Some(req.tc.clone()),
                        nec: None,
                    },
                };
                self.sender_handle.send::<HotStuffMessage>(*origin, response.into());
            }
        }
        Ok(())
    }

    /// On receiving a ProposalResponse during RECOVER: if we're recovering,
    /// use the block to complete the reproposal (Case 4 equivalent).
    fn on_receive_proposal_response<K: KVStore>(
        &mut self,
        resp: ProposalResponse,
        _origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        _app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        if resp.view != self.view_info.view {
            return Ok(());
        }
        // Only process if we are currently recovering.
        let is_recovering = matches!(self.recovery_state, RecoveryState::Recovering { .. });
        if !is_recovering {
            return Ok(());
        }

        // Verify the block matches what the TC expects.
        if let RecoveryState::Recovering { ref tc, .. } = self.recovery_state {
            if let Some(ref tip) = tc.high_tip {
                if resp.proposal.block.hash != tip.block_hash {
                    return Ok(());
                }
            } else {
                return Ok(());
            }
        }

        // Validate the block before reproposing: check hash/justify correctness
        // and safety invariants (safe_pc, block justify type).
        if !resp.proposal.block.is_correct(block_tree)? {
            return Ok(());
        }
        if !safe_block(&resp.proposal.block, block_tree, self.config.chain_id)? {
            return Ok(());
        }

        // Cancel recovery — we got the block. Repropose it (Case 4).
        let tc = if let RecoveryState::Recovering { ref tc, .. } = self.recovery_state {
            tc.clone()
        } else {
            return Ok(());
        };
        self.recovery_state = RecoveryState::None;

        let proposal = Proposal {
            chain_id: self.config.chain_id,
            view: self.view_info.view,
            block: resp.proposal.block,
            tc: Some(tc),
            nec: None,
        };
        self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
        self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
        self.sender_handle.broadcast::<HotStuffMessage>(proposal.clone().into());
        Event::Propose(ProposeEvent {
            timestamp: SystemTime::now(),
            proposal,
        })
        .publish(&self.event_publisher);

        Ok(())
    }

    /// PROCESSNEREQUST: On receiving an NERequest, check if we voted for the
    /// high_tip block. If NOT, sign and send an NE message.
    fn on_receive_ne_request<K: KVStore>(
        &mut self,
        req: NERequest,
        _origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), HotStuffError> {
        if req.view != self.view_info.view {
            return Ok(());
        }

        // Guard: no duplicate NE per view per validator.
        if self.ne_sent_views.contains(&req.view) {
            return Ok(());
        }

        // Check if we voted for the high_tip block in the timed-out view.
        let did_vote_for_high_tip = if let Some(ref tip) = req.tc.high_tip {
            match block_tree.last_voted_proposal()? {
                Some((voted_view, voted_block)) => {
                    voted_view == req.tc.view && voted_block == tip.block_hash
                }
                None => false, // Never voted — safe to send NE.
            }
        } else {
            false
        };

        if did_vote_for_high_tip {
            return Ok(()); // Voted for it — do NOT send NE.
        }

        // Sign (view, high_tip_qc_view) and send NE.
        if let Some(ref tip) = req.tc.high_tip {
            let high_tip_qc_view = tip.block_justify.view;
            let ne_message_bytes = (req.view, high_tip_qc_view).try_to_vec().unwrap();
            let signature = self.config.keypair.sign(&ne_message_bytes);

            let ne = NEMessage {
                chain_id: self.config.chain_id,
                view: req.view,
                high_tip_qc_view,
                signature,
            };

            // FIX CONS-FIND-11: Use reputation-weighted leader for NE routing.
            let validator_set_state = block_tree.validator_set_state()?;
            let reputation = block_tree.leader_reputation().ok();
            let leader = match reputation.as_ref() {
                Some(rep) => crate::pacemaker::implementation::select_leader_with_reputation(
                    req.view,
                    validator_set_state.committed_validator_set(),
                    rep,
                ),
                None => crate::pacemaker::implementation::select_leader(
                    req.view,
                    validator_set_state.committed_validator_set(),
                ),
            };
            self.sender_handle.send::<HotStuffMessage>(leader, ne.into());
            self.ne_sent_views.insert(req.view);
        }

        Ok(())
    }

    /// On receiving an NE message during RECOVER: collect signatures.
    /// If 2f+1 collected, form NEC and issue fresh proposal.
    fn on_receive_ne<K: KVStore>(
        &mut self,
        ne: NEMessage,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        if ne.view != self.view_info.view {
            return Ok(());
        }

        // Only process if we are currently recovering.
        let nec_opt = if let RecoveryState::Recovering { ref mut ne_collector, .. } = self.recovery_state {
            ne_collector.collect(origin, ne.view, ne.high_tip_qc_view, ne.signature)
        } else {
            return Ok(());
        };

        if let Some(nec) = nec_opt {
            // NEC formed! Cancel recovery and issue fresh proposal.
            let tc = if let RecoveryState::Recovering { ref tc, .. } = self.recovery_state {
                tc.clone()
            } else {
                return Ok(());
            };
            self.recovery_state = RecoveryState::None;

            // Fresh proposal with NEC: use the high_tip's QC as the block's justify.
            let high_tip_qc = if let Some(ref tip) = tc.high_tip {
                tip.block_justify.clone()
            } else {
                return Ok(());
            };

            // Produce a new block extending from the high_tip's QC.
            let parent_block = if high_tip_qc.is_genesis_pc() {
                None
            } else {
                Some(high_tip_qc.block)
            };
            let child_height = if let Some(ref pb) = parent_block {
                let h = block_tree.block_height(pb)?.ok_or(
                    BlockTreeError::BlockExpectedButNotFound { block: *pb },
                )?;
                h + 1
            } else {
                BlockHeight::new(0)
            };

            let produce_block_request = ProduceBlockRequest::new(
                self.view_info.view,
                parent_block,
                block_tree.app_view(parent_block.as_ref())?,
            );
            let ProduceBlockResponse {
                data,
                data_hash,
                app_state_updates: _,
                validator_set_updates: _,
            } = app.produce_block(produce_block_request);

            let block = Block::new(child_height, high_tip_qc, data_hash, data);

            let proposal = Proposal {
                chain_id: self.config.chain_id,
                view: self.view_info.view,
                block,
                tc: Some(tc),
                nec: Some(nec),
            };
            self.pending_bodies.insert(proposal.block.hash, proposal.block.clone());
            self.sender_handle.store_block_for_serving(proposal.block.hash, proposal.block.clone());
            self.sender_handle.broadcast::<HotStuffMessage>(proposal.clone().into());
            Event::Propose(ProposeEvent {
                timestamp: SystemTime::now(),
                proposal,
            })
            .publish(&self.event_publisher);
        }

        Ok(())
    }

    // ========================================================================
    // Hybrid pipelining: header-first propagation handlers
    // ========================================================================

    /// Process a ProposalHeader: verify safety, vote, request body.
    /// The block is NOT inserted into the tree yet — that happens when the body arrives.
    fn on_receive_proposal_header<K: KVStore>(
        &mut self,
        header: ProposalHeader,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        // Equivocation detection (same as on_receive_proposal).
        let proposal_key = (header.view, *origin);
        if let Some(first_hash) = self.seen_proposals.get(&proposal_key) {
            if *first_hash != header.block_hash {
                let evidence = super::types::EquivocationEvidence {
                    view: header.view,
                    leader: *origin,
                    block_a: *first_hash,
                    block_b: header.block_hash,
                };
                Event::EquivocationDetected(crate::events::EquivocationDetectedEvent {
                    timestamp: SystemTime::now(),
                    equivocator: *origin,
                    view: header.view,
                    block_a: *first_hash,
                    block_b: header.block_hash,
                })
                .publish(&self.event_publisher);
                let _ = block_tree.store_equivocation_evidence(&evidence);

                if block_tree.is_speculatively_committed(first_hash)? {
                    let rolled_back = block_tree.rollback_speculative_block(first_hash)?;
                    if rolled_back {
                        Event::RollbackBlock(crate::events::RollbackBlockEvent {
                            timestamp: SystemTime::now(),
                            block: *first_hash,
                            view: header.view,
                            equivocator: *origin,
                        })
                        .publish(&self.event_publisher);
                        app.on_speculative_rollback(*first_hash, &evidence);
                    }
                }
                return Ok(());
            }
        } else {
            self.seen_proposals.insert(proposal_key, header.block_hash);
        }

        // Verify block_hash = hash(height, justify, data_hash) — all from the header.
        let expected_hash = Block::hash(header.height, &header.justify, &header.data_hash);
        if header.block_hash != expected_hash {
            log::warn!("dropping proposal header: hash mismatch, view={}", header.view.int());
            return Ok(());
        }

        // If the block is already in the tree (proposer self-inserted), skip body fetch
        // but still vote below.
        let block_already_in_tree = block_tree.block_height(&header.block_hash)?.is_some();

        // Verify justify correctness and safety (same checks as safe_block).
        // In the header pipeline, the justify's block may not yet be in the tree
        // (body still in flight) but we already validated and voted for it. Tracked
        // entries in pending_headers/pending_bodies are proof of prior validation.
        let justify_correct = header.justify.is_correct(block_tree)?;
        let justify_previously_validated =
            self.pending_headers.contains_key(&header.justify.block)
                || self.pending_bodies.contains_key(&header.justify.block);
        let is_safe = if justify_correct {
            if justify_previously_validated {
                header.justify.is_block_justify()
                    && (header.justify.chain_id == self.config.chain_id
                        || header.justify.is_genesis_pc())
            } else {
                safe_pc(&header.justify, block_tree, self.config.chain_id)?
                    && header.justify.is_block_justify()
            }
        } else {
            false
        };

        if !justify_correct || !is_safe {
            let justify_block_known = block_tree.contains(&header.justify.block);
            log::warn!(
                "dropping proposal header: view={}, justify_correct={}, is_safe={}, justify_block_known={}",
                header.view.int(), justify_correct, is_safe, justify_block_known,
            );
            if !justify_block_known {
                self.sync_needed = true;
            }
            match self.proposal_status {
                ProposalStatus::WaitingForProposal => {
                    self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
                }
                ProposalStatus::OneLeaderProposed { leader: _ } => {
                    self.proposal_status = ProposalStatus::AllLeadersProposed
                }
                _ => {}
            }
            return Ok(());
        }

        // Vote on the header (same voting logic as on_receive_proposal).
        let validator_set_state = block_tree.validator_set_state()?;
        if is_phase_voter(
            &self.config.keypair.public(),
            &validator_set_state,
            &header.justify,
        ) && (block_tree.highest_view_voted()?.is_none()
            || block_tree.highest_view_voted()?.unwrap() < self.view_info.view)
        {
            let vote_phase = if header.has_validator_set_updates {
                Phase::Prepare
            } else {
                Phase::Generic
            };

            let phase_vote = PhaseVote::new(
                &self.config.keypair,
                self.config.chain_id,
                self.view_info.view,
                header.block_hash,
                vote_phase,
            );
            let reputation = block_tree.leader_reputation().ok();
            let vote_recipient = phase_vote_recipient_with_reputation(
                &phase_vote,
                &validator_set_state,
                reputation.as_ref(),
            );
            self.sender_handle
                .send::<HotStuffMessage>(vote_recipient, phase_vote.clone().into());

            block_tree.set_vote_state_atomic(self.view_info.view, header.block_hash)?;
            Event::PhaseVote(PhaseVoteEvent {
                timestamp: SystemTime::now(),
                vote: phase_vote,
            })
            .publish(&self.event_publisher);
        }

        // Request the body via the dedicated block-data protocol (skip if already self-inserted).
        if !block_already_in_tree {
            let req = BlockDataRequest {
                chain_id: header.chain_id,
                view: header.view,
                block_hash: header.block_hash,
            };
            self.sender_handle.request_block_data(*origin, req);
            self.body_fetch_tracker.insert(header.block_hash, (Instant::now(), 0, *origin));
            self.pending_headers.insert(header.block_hash, header);
        }

        // Update proposal status.
        match self.proposal_status {
            ProposalStatus::WaitingForProposal => {
                self.proposal_status = ProposalStatus::OneLeaderProposed { leader: *origin }
            }
            ProposalStatus::OneLeaderProposed { leader: _ } => {
                self.proposal_status = ProposalStatus::AllLeadersProposed
            }
            _ => {}
        }

        Ok(())
    }

    /// Serve a BlockDataRequest: look up block in pending_bodies or block_tree, respond.
    fn on_receive_block_data_request<K: KVStore>(
        &mut self,
        req: BlockDataRequest,
        origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
    ) -> Result<(), HotStuffError> {
        let block = if let Some(block) = self.pending_bodies.get(&req.block_hash) {
            Some(block.clone())
        } else {
            block_tree.block(&req.block_hash)?
        };

        if let Some(block) = block {
            let resp = BlockDataResponse { view: req.view, block };
            self.sender_handle
                .send::<HotStuffMessage>(*origin, resp.into());
        }

        Ok(())
    }

    /// Process a BlockDataResponse: validate block, insert into tree, trigger updates.
    /// If parent state isn't available yet, defers the body for later processing.
    fn on_receive_block_data_response<K: KVStore>(
        &mut self,
        resp: BlockDataResponse,
        _origin: &VerifyingKey,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        let block = resp.block;
        let block_hash = block.hash;

        if !self.pending_headers.contains_key(&block_hash) {
            return Ok(());
        }

        if self.try_insert_body(block.clone(), block_tree, app)? {
            self.pending_headers.remove(&block_hash);
            self.body_fetch_tracker.remove(&block_hash);
            self.drain_deferred_bodies(block_tree, app)?;
        } else {
            self.deferred_bodies.insert(block_hash, block);
            self.body_fetch_tracker.remove(&block_hash);
        }

        Ok(())
    }

    /// Attempt to validate and insert a block body. Returns true on success, false if
    /// the parent state isn't available yet.
    fn try_insert_body<K: KVStore>(
        &mut self,
        block: Block,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<bool, HotStuffError> {
        let parent_block = if block.justify.is_genesis_pc() {
            None
        } else {
            Some(&block.justify.block)
        };
        let app_view = match block_tree.app_view(parent_block) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        let validate_block_request = ValidateBlockRequest::new(&block, app_view);

        if let ValidateBlockResponse::Valid {
            app_state_updates,
            validator_set_updates,
        } = app.validate_block(validate_block_request)
        {
            block_tree.insert(
                &block,
                app_state_updates.as_ref(),
                validator_set_updates.as_ref(),
            )?;
            Event::InsertBlock(InsertBlockEvent {
                timestamp: SystemTime::now(),
                block: block.clone(),
            })
            .publish(&self.event_publisher);

            let update_result =
                block_tree.update(&block.justify, &self.event_publisher).unwrap_or_else(|e| {
                    if let BlockTreeError::BlockExpectedButNotFound { block: missing } = &e {
                        log::warn!("body insert: missing block {:?} in commit chain — triggering sync", missing);
                        self.sync_needed = true;
                    } else {
                        log::warn!("block_tree.update after body insert: {:?}", e);
                    }
                    UpdateResult { validator_set_updates: None, committed_block_hashes: vec![] }
                });
            self.process_update_result(update_result, block_tree, app);

            let validator_set_state = block_tree.validator_set_state()?;
            let _ = self
                .phase_vote_collectors
                .update_validator_sets(&validator_set_state);
            Ok(true)
        } else {
            log::warn!("body validation failed for block hash={:?}", block.hash);
            Ok(false)
        }
    }

    /// After a successful body insertion, process any deferred bodies whose
    /// parents are now available.
    fn drain_deferred_bodies<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        let mut progress = true;
        while progress {
            progress = false;
            let hashes: Vec<CryptoHash> = self.deferred_bodies.keys().cloned().collect();
            for hash in hashes {
                let block = match self.deferred_bodies.get(&hash) {
                    Some(b) => b.clone(),
                    None => continue,
                };
                if self.try_insert_body(block, block_tree, app)? {
                    self.deferred_bodies.remove(&hash);
                    self.pending_headers.remove(&hash);
                    self.body_fetch_tracker.remove(&hash);
                    progress = true;
                }
            }
        }
        Ok(())
    }

    /// Poll the dedicated block-data channel for body responses.
    /// Called from the algorithm loop between consensus message processing steps.
    pub(crate) fn poll_block_data_responses<K: KVStore>(
        &mut self,
        block_tree: &mut BlockTreeSingleton<K>,
        app: &mut impl App<K>,
    ) -> Result<(), HotStuffError> {
        while let Some((origin, resp)) = self.sender_handle.recv_block_data() {
            self.on_receive_block_data_response(resp, &origin, block_tree, app)?;
        }
        Ok(())
    }

    const BODY_RETRY_INTERVAL: Duration = Duration::from_millis(300);
    const MAX_BODY_RETRIES: u8 = 3;

    /// Re-request bodies for stale pending headers; trigger block sync after max retries.
    pub(crate) fn tick_pending_body_retries(&mut self) {
        let now = Instant::now();
        let mut expired = Vec::new();
        for (hash, (last_req, count, origin)) in self.body_fetch_tracker.iter_mut() {
            if now.duration_since(*last_req) < Self::BODY_RETRY_INTERVAL {
                continue;
            }
            if *count >= Self::MAX_BODY_RETRIES {
                log::warn!("body fetch exhausted {} retries for {:?} — falling back to sync", Self::MAX_BODY_RETRIES, hash);
                expired.push(*hash);
                continue;
            }
            if let Some(header) = self.pending_headers.get(hash) {
                let req = BlockDataRequest {
                    chain_id: header.chain_id,
                    view: header.view,
                    block_hash: header.block_hash,
                };
                self.sender_handle.request_block_data(*origin, req);
                *last_req = now;
                *count += 1;
                log::debug!("body fetch retry {} for {:?}", count, hash);
            } else {
                expired.push(*hash);
            }
        }
        for hash in &expired {
            self.body_fetch_tracker.remove(hash);
            self.pending_headers.remove(hash);
        }
        if !expired.is_empty() {
            self.sync_needed = true;
        }
    }

    pub(crate) fn has_pending_body_fetches(&self) -> bool {
        !self.body_fetch_tracker.is_empty()
    }
}

/// Configuration parameters for the [`HotStuff`] struct.
#[derive(Clone)]
pub(crate) struct HotStuffConfiguration {
    /// The Chain ID of the blockchain that the current replica is to track.
    pub(crate) chain_id: ChainID,

    /// The keypair with which the HotStuff implementation should sign `PhaseVote`s.
    pub(crate) keypair: Keypair,
}

/// The different ways a call to a method of the `HotStuff` struct can fail.
#[derive(Debug)]
pub enum HotStuffError {
    BlockTreeError(BlockTreeError),
}

impl From<BlockTreeError> for HotStuffError {
    fn from(value: BlockTreeError) -> Self {
        HotStuffError::BlockTreeError(value)
    }
}

/// Keeps track of the set of `Proposal`s that have been received by the `HotStuff` struct in the current
/// view.
///
/// Keeping track of `ProposalStatus` ensures that a leader can only propose once in a view -- any
/// subsequent proposals will be ignored.
///
/// ## Persistence
///
/// Note that a variable of this type is stored in memory allocated to the program at runtime, rather
/// than persistent storage. This is because losing the information stored in `ProposalStatus`, unlike
/// losing the information about the highest view the replica has phase-voted in, does lead to safety
/// violations. In the worst case, it can only enable temporary liveness violations.
pub enum ProposalStatus {
    /// No proposal or nudge was seen in this view so far. Proposals and nudges from a valid leader
    /// (proposer) can be accepted.
    WaitingForProposal,

    /// The leader with a given public key has already proposed or nudged. No more proposals or nudges
    /// from this leader can be accepted.
    OneLeaderProposed { leader: VerifyingKey },

    /// All leaders for the view have proposed or nudged, hence no more proposals or nudges can be accepted
    /// in this view.
    AllLeadersProposed,
}

impl ProposalStatus {
    /// Has this leader already proposed/nudged in the current view?
    fn has_one_leader_proposed(&self, leader: &VerifyingKey) -> bool {
        match self {
            ProposalStatus::OneLeaderProposed { leader: validator } => validator == leader,
            _ => false,
        }
    }

    /// Have all (max. 2) leaders already proposed/nudged in this view?
    ///
    /// Note: this can evaluate to true only during the validator set update period.
    fn have_all_leaders_proposed(&self) -> bool {
        matches!(self, ProposalStatus::AllLeadersProposed)
    }
}

/// MonadBFT B2: State machine for async NEC recovery (Algorithm 7).
///
/// ## Challenge 1 resolution:
/// The RECOVER algorithm is inherently async — the leader sends ProposalRequest
/// and NERequest, then waits for responses while the view timer ticks.
/// We cannot block enter_view. Instead:
/// - enter_view starts recovery (sends requests, transitions to Recovering)
/// - The main event loop continues processing messages
/// - When ProposalResponse or enough NE messages arrive, the leader completes
///   the proposal via on_receive_proposal_response or on_receive_ne
/// - If the view timer expires, entering a new view clears the recovery state
pub(crate) enum RecoveryState {
    /// No recovery in progress.
    None,
    /// Actively recovering: waiting for ProposalResponse or NEC formation.
    Recovering {
        tc: crate::pacemaker::types::TimeoutCertificate,
        ne_collector: NECollector,
    },
}
