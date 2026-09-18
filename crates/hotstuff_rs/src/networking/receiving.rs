//! Functions and types for receiving messages from the P2P network.

use std::{
    collections::{BTreeMap, VecDeque},
    mem,
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    thread::{self, JoinHandle},
    time::Instant,
};

use ed25519_dalek::VerifyingKey;

use crate::{
    block_sync::messages::{BlockSyncMessage, BlockSyncRequest, BlockSyncResponse},
    types::data_types::{BufferSize, ChainID, ViewNumber},
};

use super::{
    messages::{Message, ProgressMessage},
    network::Network,
};

/// Spawn the poller thread, which polls the [`Network`] for messages and distributes them into receiver
/// handles.
///
/// The kinds of messages that the pollers poll are:
/// 1. Progress messages (processed by the [`Algorithm`][crate::algorithm::Algorithm]'s execute loop), and
/// 2. Block sync requests (processed by [`BlockSyncServer`][crate::block_sync::server::BlockSyncServer]),
///    and
/// 3. Block sync responses (processed by [`BlockSyncClient`][crate::block_sync::client::BlockSyncClient]).
#[allow(clippy::type_complexity)]
pub(crate) fn start_polling<N: Network + 'static>(
    mut network: N,
    shutdown_signal: Receiver<()>,
) -> (
    JoinHandle<()>,
    Receiver<(VerifyingKey, ProgressMessage)>,
    Receiver<(VerifyingKey, BlockSyncRequest)>,
    Receiver<(VerifyingKey, BlockSyncResponse)>,
) {
    let (to_progress_msg_receiver, progress_msg_receiver) = mpsc::channel();
    let (to_sync_request_receiver, sync_request_receiver) = mpsc::channel();
    let (to_sync_response_receiver, sync_response_receiver) = mpsc::channel();

    let poller_thread = thread::spawn(move || loop {
        match shutdown_signal.try_recv() {
            Ok(()) => return,
            Err(TryRecvError::Empty) => (),
            Err(TryRecvError::Disconnected) => {
                panic!("Poller thread disconnected from main thread")
            }
        }

        if let Some((origin, msg)) = network.recv() {
            match msg {
                Message::ProgressMessage(p_msg) => {
                    let _ = to_progress_msg_receiver.send((origin, p_msg));
                }
                Message::BlockSyncMessage(s_msg) => match s_msg {
                    BlockSyncMessage::BlockSyncRequest(s_req) => {
                        let _ = to_sync_request_receiver.send((origin, s_req));
                    }
                    BlockSyncMessage::BlockSyncResponse(s_res) => {
                        let _ = to_sync_response_receiver.send((origin, s_res));
                    }
                },
            }
        } else {
            // S370: yield_now() does NOT sleep, so an idle node spins this core (the
            // box self-saturates and stretches block time). Park briefly instead —
            // when messages are queued recv() returns Some and we never sleep.
            thread::sleep(std::time::Duration::from_micros(250))
        }
    });
    (
        poller_thread,
        progress_msg_receiver,
        sync_request_receiver,
        sync_response_receiver,
    )
}

fn is_chain_neutral_genesis_body(message: &ProgressMessage) -> bool {
    use crate::hotstuff::messages::HotStuffMessage;
    use crate::types::block::Block;
    // Legacy body responses have no outer chain ID; they derive it from the
    // justify. The universal genesis PC deliberately carries chain 0, even on
    // nonzero chains. Admit only structurally bound height-zero bodies here.
    // HotStuff still requires a tracked hash before validation/insertion; an
    // unsolicited push is only parked until a local-chain header claims it.
    matches!(message,
        ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataResponse(response))
        if response.block.height.int() == 0
            && response.block.justify.is_genesis_pc()
            && response.block.hash == Block::hash(response.block.height,
                &response.block.justify, &response.block.data_hash))
}

/// A receiving end for [`ProgressMessage`](ProgressMessage)s.
///
/// ## View-aware buffering
///
/// `ProgressMessageStub` performs "view-aware buffering". This means that it inspects incoming
/// messages' view numbers to decide whether to:
/// 1. Return it from `recv` for immediate processing.
/// 2. Place it in its buffer for future processing.
/// 3. Discard it.
///
/// `ProgressMessageStub` applies different view-aware policies depending on whether the incoming
/// message is a HotStuff message, a Pacemaker message, or a BlockSyncTrigger message. These policies
/// are detailed below:
///
/// ### HotStuff messages
///
/// `recv` returns HotStuff messages for **only** the current view, and caches messages from future
/// views for future processing. This helps prevent interruptions to progress when replicas' views are
/// mostly synchronized but they enter views at slightly different times.
///
/// ### Pacemaker messages
///
/// `recv` returns Pacemaker messages for any view **greater than or equal to** the current view. It
/// **also** caches all messages for views greater than the current view, for processing in the intended
/// view in case immediate processing is not possible.
///
/// ### BlockSyncTrigger messages
///
/// `recv` returns block sync trigger messages immediately without buffering.
///
/// ## Buffer management
///
/// If `ProgressMessageStub`'s message buffer grows beyond the maximum capacity specified in
/// [`new`](Self::new), some future-viewed messages might be removed from the buffer to make space for
/// the new message. The logic for removing future-viewed messages removes highest-viewed messages first.
pub(crate) struct ProgressMessageStub {
    receiver: Receiver<(VerifyingKey, ProgressMessage)>,
    msg_buffer: ProgressMessageBuffer,
}

impl ProgressMessageStub {
    /// Create a fresh [ProgressMessageStub] with a given receiver end and buffer capacity.
    pub(crate) fn new(
        receiver: Receiver<(VerifyingKey, ProgressMessage)>,
        msg_buffer_capacity: BufferSize,
    ) -> ProgressMessageStub {
        let msg_buffer: ProgressMessageBuffer = ProgressMessageBuffer::new(msg_buffer_capacity);
        Self {
            receiver,
            msg_buffer,
        }
    }

    /// Receive a message matching the specified `chain_id`, and view >= current view (if any). Cache and/or
    /// return immediately, depending on the message type. Messages older than current view are dropped
    /// immediately. [`BlockSyncAdvertiseMessage`][crate::block_sync::messages::BlockSyncAdvertiseMessage]
    /// messages are not associated with a view, and so they are returned immediately.
    pub(crate) fn recv(
        &mut self,
        chain_id: ChainID,
        cur_view: ViewNumber,
        deadline: Instant,
    ) -> Result<(VerifyingKey, ProgressMessage), ProgressMessageReceiveError> {
        // Clear buffer of messages with views lower than the current one.
        self.msg_buffer.remove_expired_msgs(cur_view);

        // Try to get buffered messages for the current view.
        if let Some((sender, msg)) = self.msg_buffer.get_msg(&cur_view) {
            return Ok((sender, msg));
        }

        // Try to get messages from the poller.
        while Instant::now() < deadline {
            match self.receiver.recv_timeout(deadline - Instant::now()) {
                Ok((sender, msg)) => {
                    if msg.chain_id() != chain_id && !is_chain_neutral_genesis_body(&msg) {
                        continue;
                    }

                    // If the message is for a future view then cache it.
                    //
                    // Note:
                    // If `msg` is a Pacemaker Message and `msg.view > cur_view`, then we will cache the message *and*
                    // return it. This is to give the message two opportunities to be processed: 1. When the message is
                    // first received, and 2. When the replica is in `msg.view` and therefore caught up with the validator
                    // set.
                    //
                    // This behavior is not absolutely necessary, but helps with liveness.
                    if msg.view().is_some_and(|view| view > cur_view) {
                        match msg.clone() {
                            ProgressMessage::HotStuffMessage(msg) => {
                                self.msg_buffer.insert(msg, sender);
                            }
                            ProgressMessage::PacemakerMessage(msg) => {
                                self.msg_buffer.insert(msg, sender);
                            }
                            ProgressMessage::BlockSyncAdvertiseMessage(_) => (),
                        }
                    }

                    // Return the message if either:
                    // 1. It is a HotStuff message for the current view, or
                    // 1b. It is a block data fetch message (viewless — body may arrive after view advances), or
                    // 2. If it is a Pacemaker message for the current view or a future view, or
                    // 3. If it is a BlockSyncAdvertise message.
                    let return_msg = match &msg {
                        ProgressMessage::HotStuffMessage(hotstuff_msg) => {
                            hotstuff_msg.view() == cur_view || hotstuff_msg.is_block_data_msg()
                        }
                        ProgressMessage::PacemakerMessage(pacemaker_msg) => {
                            pacemaker_msg.view() >= cur_view
                        }
                        ProgressMessage::BlockSyncAdvertiseMessage(_) => true,
                    };

                    if return_msg {
                        return Ok((sender, msg));
                    }
                }
                Err(RecvTimeoutError::Timeout) => thread::yield_now(),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ProgressMessageReceiveError::Disconnected)
                }
            }
        }

        Err(ProgressMessageReceiveError::Timeout)
    }
}

#[derive(Debug)]
pub(crate) enum ProgressMessageReceiveError {
    Timeout,
    Disconnected,
}

/// Message buffer intended for storing received [`ProgressMessage`]s for future views.
///
/// Its size is bounded by its capacity, and when the capacity is reached messages for highest views may
/// be removed.
struct ProgressMessageBuffer {
    buffer_capacity: BufferSize,
    buffer: BTreeMap<ViewNumber, VecDeque<(VerifyingKey, ProgressMessage)>>,
    buffer_size: BufferSize,
}

impl ProgressMessageBuffer {
    /// Create an empty message buffer.
    fn new(buffer_capacity: BufferSize) -> Self {
        Self {
            buffer_capacity,
            buffer: BTreeMap::new(),
            buffer_size: BufferSize::new(0),
        }
    }

    /// Try inserting the message into the buffer.
    /// In case caching the message makes the buffer grow beyond its capacity, this function either:
    /// 1. If the message has the highest view among the views of messages currently in the buffer,
    ///    then the message is dropped, or
    /// 2. Otherwise, just enough highest-viewed messages are removed from the buffer to make space
    ///    for the new message.
    ///
    /// Returns whether the message was successfully inserted into the buffer.
    fn insert<M: Into<ProgressMessage> + Cacheable>(
        &mut self,
        msg: M,
        sender: VerifyingKey,
    ) -> bool {
        let Some(bytes_requested) = (mem::size_of::<VerifyingKey>() as u64).checked_add(msg.size()) else {
            return false;
        };
        // Reject an individually oversized message before evicting useful work.
        if bytes_requested > self.buffer_capacity.int() {
            return false;
        }
        let free_bytes = self.buffer_capacity.int() - self.buffer_size.int();
        if bytes_requested > free_bytes {
            if !self.buffer.last_key_value().is_some_and(|(view, _)| msg.view() < *view) {
                return false;
            }
            // Reclaim only the deficit, retaining highest-view-first eviction.
            self.remove_highest_viewed_msgs(bytes_requested - free_bytes);
        }

        self.buffer_size += bytes_requested;
        self.buffer.entry(msg.view()).or_default().push_back((sender, msg.into()));
        true
    }

    /// If there are messages for this view in the buffer, remove and return the message at the front
    /// of the queue.
    fn get_msg(&mut self, view: &ViewNumber) -> Option<(VerifyingKey, ProgressMessage)> {
        let queue = self.buffer.get_mut(view)?;
        let message = queue.pop_front()?;
        self.buffer_size -= mem::size_of::<VerifyingKey>() as u64 + message.1.size();
        if queue.is_empty() {
            self.buffer.remove(view);
        }
        Some(message)
    }

    /// Given the number of bytes that need to be removed, removes just enough highest-viewed messages
    /// to free up (at least) the required number of bytes in the buffer.
    fn remove_highest_viewed_msgs(&mut self, bytes_to_remove: u64) {
        let verifying_key_size = mem::size_of::<VerifyingKey>() as u64;

        let mut bytes_removed = 0;
        let mut views_removed = Vec::new();
        let mut msg_queues_iter = self.buffer.iter_mut().rev();

        // Removes messages from the message buffer until the required number of bytes is freed.
        while bytes_removed < bytes_to_remove {
            // Take the message queue for the next highest view, and remove its messages until the required number of bytes is freed.
            if let Some((view, msg_queue)) = msg_queues_iter.next() {
                let removals = msg_queue
                    .iter()
                    .rev()
                    .take_while(|(_, msg)| {
                        if bytes_removed < bytes_to_remove {
                            bytes_removed += msg.size() + verifying_key_size;
                            true
                        } else {
                            false
                        }
                    })
                    .count() as u64;
                (0..removals).for_each(|_| {
                    let _ = msg_queue.pop_back();
                });
                if msg_queue.is_empty() {
                    views_removed.push(*view)
                }
            } else {
                break;
            }
        }

        self.buffer_size -= bytes_removed;

        // If some views in the message buffer have lost all messages as a result of the removal,
        // then also remove their corresponding keys from the buffer.
        views_removed.iter().for_each(|view| {
            let _ = self.buffer.remove(view);
        });
    }

    /// Remove all messages for views less than the current view.
    fn remove_expired_msgs(&mut self, cur_view: ViewNumber) {
        let retained = self.buffer.split_off(&cur_view);
        let expired = mem::replace(&mut self.buffer, retained);
        for queue in expired.into_values() {
            for (_, msg) in queue {
                self.buffer_size -= mem::size_of::<VerifyingKey>() as u64 + msg.size();
            }
        }
    }
}

/// A cacheable message can be inserted into the
/// [progress message buffer](crate::networking::receiving::ProgressMessageStub).
///
/// For this, we require that:
/// 1. The message is associated with a view,
/// 2. The message size is statically known and depends on a particular enum variant.
pub(crate) trait Cacheable {
    fn view(&self) -> ViewNumber;
    fn size(&self) -> u64;
}

/// A receiving end for sync responses. The [`BlockSyncClientStub::recv_response`] method returns
/// the received response.
pub(crate) struct BlockSyncClientStub {
    responses: Receiver<(VerifyingKey, BlockSyncResponse)>,
}

impl BlockSyncClientStub {
    pub(crate) fn new(
        responses: Receiver<(VerifyingKey, BlockSyncResponse)>,
    ) -> BlockSyncClientStub {
        BlockSyncClientStub { responses }
    }

    /// Receive a [BlockSyncResponse] from a given peer. Waits for the response until the deadline is
    /// reached, and if no response is received it returns [BlockSyncResponseReceiveError::Timeout].
    pub(crate) fn recv_response(
        &self,
        peer: VerifyingKey,
        deadline: Instant,
    ) -> Result<BlockSyncResponse, BlockSyncResponseReceiveError> {
        while Instant::now() < deadline {
            match self.responses.recv_timeout(deadline - Instant::now()) {
                Ok((sender, sync_response)) => {
                    if sender == peer {
                        return Ok(sync_response);
                    }
                }
                Err(RecvTimeoutError::Timeout) => thread::yield_now(),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(BlockSyncResponseReceiveError::Disconnected)
                }
            }
        }

        Err(BlockSyncResponseReceiveError::Timeout)
    }
}

#[derive(Debug)]
pub enum BlockSyncResponseReceiveError {
    Disconnected,
    Timeout,
}

/// A receiving end for sync requests. The [`BlockSyncServerStub::recv_request`] method returns the
/// received request.
pub(crate) struct BlockSyncServerStub {
    requests: Receiver<(VerifyingKey, BlockSyncRequest)>,
}

impl BlockSyncServerStub {
    pub(crate) fn new(requests: Receiver<(VerifyingKey, BlockSyncRequest)>) -> BlockSyncServerStub {
        BlockSyncServerStub { requests }
    }

    /// Receive a [BlockSyncRequest] if available, else return [BlockSyncRequestReceiveError::NotAvailable].
    pub(crate) fn recv_request(
        &self,
    ) -> Result<(VerifyingKey, BlockSyncRequest), BlockSyncRequestReceiveError> {
        match self.requests.try_recv() {
            Ok((origin, request)) => Ok((origin, request)),
            // Safety: the sync server thread (the only caller of this function) shuts down before the poller thread
            // (the sender side of this channel), so we will never be disconnected at this point.
            Err(TryRecvError::Disconnected) => Err(BlockSyncRequestReceiveError::Disconnected),
            Err(TryRecvError::Empty) => Err(BlockSyncRequestReceiveError::NotAvailable),
        }
    }
}

#[derive(Debug)]
pub enum BlockSyncRequestReceiveError {
    Disconnected,
    NotAvailable,
}

#[cfg(test)]
mod progress_message_buffer_tests {
    use super::*;
    use crate::hotstuff::messages::{BlockDataRequest, HotStuffMessage, ProposalHeader};
    use crate::hotstuff::types::PhaseCertificate;
    use crate::types::data_types::{BlockHeight, CryptoHash};

    fn sender() -> VerifyingKey {
        ed25519_dalek::SigningKey::from_bytes(&[1; 32]).verifying_key()
    }

    fn request(view: u64, id: u8) -> HotStuffMessage {
        BlockDataRequest {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(view),
            block_hash: CryptoHash::new([id; 32]),
        }.into()
    }

    fn header(view: u64) -> HotStuffMessage {
        ProposalHeader {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(view),
            block_hash: CryptoHash::new([0; 32]),
            height: BlockHeight::new(0),
            data_hash: CryptoHash::new([0; 32]),
            justify: PhaseCertificate::genesis_pc(),
            tc: None,
            nec: None,
            has_validator_set_updates: false,
        }.into()
    }

    fn entry_size(msg: &HotStuffMessage) -> u64 {
        mem::size_of::<VerifyingKey>() as u64 + msg.size()
    }

    fn assert_accounted(buffer: &ProgressMessageBuffer) {
        let queued_bytes: u64 = buffer.buffer.values().map(|queue| {
            assert!(!queue.is_empty(), "empty views must not affect eviction priority");
            queue.iter().map(|(_, msg)| mem::size_of::<VerifyingKey>() as u64 + msg.size()).sum::<u64>()
        }).sum();
        assert_eq!(buffer.buffer_size.int(), queued_bytes);
        assert!(queued_bytes <= buffer.buffer_capacity.int());
    }

    #[test]
    fn delivery_releases_capacity_preserves_fifo_and_removes_empty_views() {
        let size = entry_size(&request(1, 1));
        let mut buffer = ProgressMessageBuffer::new(BufferSize::new(2 * size));
        // Repeated drain/refill catches occupancy accumulating across views.
        for view in 1..=4 {
            assert!(buffer.insert(request(view, 1), sender()));
            assert!(buffer.insert(request(view, 2), sender()));
            assert_accounted(&buffer);
            for id in [1, 2] {
                let (_, msg) = buffer.get_msg(&ViewNumber::new(view)).unwrap();
                assert!(matches!(msg, ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(req))
                    if req.block_hash == CryptoHash::new([id; 32])));
                assert_accounted(&buffer);
            }
            assert!(buffer.buffer.is_empty());
            assert!(buffer.get_msg(&ViewNumber::new(view)).is_none());
            assert_eq!(buffer.buffer_size.int(), 0);
        }
    }

    #[test]
    fn expiration_releases_only_older_views_and_allows_refill() {
        let size = entry_size(&request(1, 1));
        let mut buffer = ProgressMessageBuffer::new(BufferSize::new(4 * size));
        for view in [1, 1, 2, 3] {
            assert!(buffer.insert(request(view, 1), sender()));
        }
        buffer.remove_expired_msgs(ViewNumber::new(2));
        assert_accounted(&buffer);
        assert_eq!(buffer.buffer_size.int(), 2 * size);
        assert!(buffer.buffer.contains_key(&ViewNumber::new(2)));
        assert!(buffer.buffer.contains_key(&ViewNumber::new(3)));
        buffer.remove_expired_msgs(ViewNumber::new(2));
        assert_accounted(&buffer);
        assert!(buffer.insert(request(4, 1), sender()));
        assert!(buffer.insert(request(5, 1), sender()));
        assert_accounted(&buffer);
        buffer.remove_expired_msgs(ViewNumber::new(6));
        assert_accounted(&buffer);
        assert_eq!(buffer.buffer_size.int(), 0);
        assert!(buffer.insert(request(7, 1), sender()));
        assert_accounted(&buffer);
    }

    #[test]
    fn eviction_reclaims_only_deficit_from_highest_view() {
        let small = entry_size(&request(4, 1));
        let large = entry_size(&header(3));
        assert!(large > small);
        // There is already room for large-small bytes. Only one request needs eviction.
        let mut buffer = ProgressMessageBuffer::new(BufferSize::new(2 * small + large));
        for view in [4, 5, 6] {
            assert!(buffer.insert(request(view, 1), sender()));
        }
        assert!(buffer.insert(header(3), sender()));
        assert_accounted(&buffer);
        assert_eq!(buffer.buffer_size.int(), buffer.buffer_capacity.int());
        assert!(buffer.buffer.contains_key(&ViewNumber::new(3)));
        assert!(buffer.buffer.contains_key(&ViewNumber::new(4)));
        assert!(buffer.buffer.contains_key(&ViewNumber::new(5)));
        assert!(!buffer.buffer.contains_key(&ViewNumber::new(6)));
        // Equal/higher views do not displace existing messages at capacity.
        assert!(!buffer.insert(request(5, 2), sender()));
        assert!(!buffer.insert(request(9, 2), sender()));
        assert_accounted(&buffer);
    }

    #[test]
    fn oversized_messages_are_rejected_without_eviction_even_when_empty() {
        let small = entry_size(&request(5, 1));
        let large = entry_size(&header(1));
        assert!(large > small);
        let mut buffer = ProgressMessageBuffer::new(BufferSize::new(small));
        assert!(!buffer.insert(header(1), sender()));
        assert_accounted(&buffer);
        assert!(buffer.insert(request(5, 1), sender()));
        assert!(!buffer.insert(header(1), sender()));
        assert_accounted(&buffer);
        assert!(buffer.get_msg(&ViewNumber::new(5)).is_some());
        assert_accounted(&buffer);
        let mut zero = ProgressMessageBuffer::new(BufferSize::new(0));
        assert!(!zero.insert(request(1, 1), sender()));
        assert_accounted(&zero);
    }
}

#[cfg(test)]
mod genesis_body_filter_tests {
    use super::*;
    use std::time::Duration;
    use crate::hotstuff::messages::{BlockDataRequest, BlockDataResponse, HotStuffMessage, ProposalHeader};
    use crate::hotstuff::types::PhaseCertificate;
    use crate::types::block::Block;
    use crate::types::crypto_primitives::SigningKey;
    use crate::types::data_types::{BlockHeight, CryptoHash, Data};

    fn response(block: Block) -> ProgressMessage {
        ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataResponse(BlockDataResponse {
            view: ViewNumber::new(1), block,
        }))
    }

    #[test]
    fn nonzero_chain_receives_only_structurally_valid_genesis_body_exception() {
        let chain = ChainID::new(7778);
        let parent = Block::new(BlockHeight::new(0), PhaseCertificate::genesis_pc(),
            CryptoHash::new([9; 32]), Data::new(vec![]));
        let sender = SigningKey::from_bytes(&[1; 32]).verifying_key();
        let (tx, rx) = mpsc::channel();
        let mut receiver = ProgressMessageStub::new(rx, BufferSize::new(1024 * 1024));
        let mut forged_hash = parent.clone();
        forged_hash.hash = CryptoHash::new([99; 32]);
        let wrong_height = Block::new(BlockHeight::new(1), PhaseCertificate::genesis_pc(),
            parent.data_hash, Data::new(vec![]));
        let mut non_genesis_pc = PhaseCertificate::genesis_pc();
        non_genesis_pc.view = ViewNumber::new(2);
        let non_genesis = Block::new(BlockHeight::new(0), non_genesis_pc,
            parent.data_hash, Data::new(vec![]));
        let wrong_request = ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(BlockDataRequest {
            chain_id: ChainID::new(0), view: ViewNumber::new(1), block_hash: parent.hash,
        }));
        let wrong_header = ProgressMessage::HotStuffMessage(HotStuffMessage::ProposalHeader(ProposalHeader {
            chain_id: ChainID::new(0), view: ViewNumber::new(1), block_hash: parent.hash,
            height: parent.height, data_hash: parent.data_hash, justify: parent.justify.clone(),
            tc: None, nec: None, has_validator_set_updates: false,
        }));
        for msg in [response(forged_hash), response(wrong_height), response(non_genesis), wrong_request, wrong_header] {
            assert!(!is_chain_neutral_genesis_body(&msg));
            tx.send((sender, msg)).unwrap();
        }
        tx.send((sender, response(parent.clone()))).unwrap();
        let (_, received) = receiver.recv(chain, ViewNumber::new(5),
            Instant::now() + Duration::from_secs(1)).expect("genesis body must cross nonzero-chain filter even late");
        assert!(matches!(received, ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataResponse(r))
            if r.block.hash == parent.hash && r.block.justify.is_genesis_pc()));
        // A same-chain request must retain ordinary delivery semantics.
        tx.send((sender, ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(BlockDataRequest {
            chain_id: chain, view: ViewNumber::new(1), block_hash: parent.hash,
        })))).unwrap();
        let (_, received) = receiver.recv(chain, ViewNumber::new(5), Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(matches!(received, ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(r)) if r.chain_id == chain));
    }
}
