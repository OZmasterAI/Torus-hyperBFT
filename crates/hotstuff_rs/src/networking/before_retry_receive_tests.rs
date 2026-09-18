use super::*;
use crate::hotstuff::messages::{BlockDataRequest, HotStuffMessage, PhaseVote};
use crate::hotstuff::types::Phase;
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::CryptoHash;

fn sender() -> VerifyingKey {
    SigningKey::from_bytes(&[1; 32]).verifying_key()
}

fn request(chain: u64, view: u64, id: u8) -> ProgressMessage {
    ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(BlockDataRequest {
        chain_id: ChainID::new(chain),
        view: ViewNumber::new(view),
        block_hash: CryptoHash::new([id; 32]),
    }))
}

fn vote(view: u64) -> ProgressMessage {
    ProgressMessage::HotStuffMessage(
        PhaseVote::new(
            &Keypair::new(SigningKey::from_bytes(&[1; 32])),
            ChainID::new(0),
            ViewNumber::new(view),
            CryptoHash::new([7; 32]),
            Phase::Generic,
        )
        .into(),
    )
}

fn request_id(message: (VerifyingKey, ProgressMessage)) -> CryptoHash {
    match message.1 {
        ProgressMessage::HotStuffMessage(HotStuffMessage::BlockDataRequest(r)) => r.block_hash,
        _ => panic!("expected a body request"),
    }
}

#[test]
fn expired_deadline_legacy_leaves_ready_channel_but_bounded_receives_only_one() {
    let (tx, rx) = mpsc::channel();
    let mut stub = ProgressMessageStub::new(rx, BufferSize::new(1_000_000));
    tx.send((sender(), request(0, 1, 1))).unwrap();
    tx.send((sender(), request(0, 1, 2))).unwrap();
    let deadline = Instant::now();
    assert!(matches!(
        stub.recv(ChainID::new(0), ViewNumber::new(2), deadline),
        Err(ProgressMessageReceiveError::Timeout)
    ));
    assert_eq!(
        request_id(
            stub.recv_before_body_retry(ChainID::new(0), ViewNumber::new(2), deadline)
                .unwrap()
        ),
        CryptoHash::new([1; 32])
    );
    assert_eq!(
        request_id(stub.receiver.try_recv().unwrap()),
        CryptoHash::new([2; 32])
    );
}

#[test]
fn raw_budget_counts_wrong_chain_stale_and_future_only_envelopes() {
    let (tx, rx) = mpsc::channel();
    let mut stub = ProgressMessageStub::new(rx, BufferSize::new(1_000_000));
    for i in 0..ProgressMessageStub::MAX_BEFORE_RETRY_ENVELOPES {
        let message = match i % 3 {
            0 => request(99, 2, i as u8),
            1 => vote(1),
            _ => vote(3),
        };
        tx.send((sender(), message)).unwrap();
    }
    tx.send((sender(), request(0, 2, 99))).unwrap();
    // A future deadline cannot turn the bounded policy into a filter-until-match loop.
    assert!(matches!(
        stub.recv_before_body_retry(
            ChainID::new(0),
            ViewNumber::new(2),
            Instant::now() + std::time::Duration::from_secs(60)
        ),
        Err(ProgressMessageReceiveError::Timeout)
    ));
    assert_eq!(
        request_id(stub.receiver.try_recv().unwrap()),
        CryptoHash::new([99; 32])
    );
    assert_eq!(
        stub.msg_buffer
            .buffer
            .get(&ViewNumber::new(3))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn future_recovery_redelivery_and_buffer_priority_are_preserved() {
    let (tx, rx) = mpsc::channel();
    let mut stub = ProgressMessageStub::new(rx, BufferSize::new(1_000_000));
    tx.send((sender(), request(0, 3, 1))).unwrap();
    assert_eq!(
        request_id(
            stub.recv_before_body_retry(ChainID::new(0), ViewNumber::new(2), Instant::now())
                .unwrap()
        ),
        CryptoHash::new([1; 32])
    );
    tx.send((sender(), request(0, 3, 2))).unwrap();
    assert_eq!(
        request_id(
            stub.recv_before_body_retry(ChainID::new(0), ViewNumber::new(3), Instant::now())
                .unwrap()
        ),
        CryptoHash::new([1; 32]),
        "cached recovery is delivered again before new channel traffic"
    );
    assert_eq!(
        request_id(stub.receiver.try_recv().unwrap()),
        CryptoHash::new([2; 32])
    );
    assert_eq!(stub.msg_buffer.buffer_size.int(), 0);
}

#[test]
fn empty_expired_deadline_and_disconnected_channel_remain_distinct() {
    let (tx, rx) = mpsc::channel();
    let mut stub = ProgressMessageStub::new(rx, BufferSize::new(1_000_000));
    assert!(matches!(
        stub.recv_before_body_retry(ChainID::new(0), ViewNumber::new(1), Instant::now()),
        Err(ProgressMessageReceiveError::Timeout)
    ));
    tx.send((sender(), request(0, 1, 1))).unwrap();
    drop(tx);
    assert!(stub
        .recv_before_body_retry(ChainID::new(0), ViewNumber::new(1), Instant::now())
        .is_ok());
    assert!(matches!(
        stub.recv_before_body_retry(ChainID::new(0), ViewNumber::new(1), Instant::now()),
        Err(ProgressMessageReceiveError::Disconnected)
    ));
    assert!(matches!(
        stub.recv(
            ChainID::new(0),
            ViewNumber::new(1),
            Instant::now() + std::time::Duration::from_secs(1)
        ),
        Err(ProgressMessageReceiveError::Disconnected)
    ));
}

type ReceiveOutcome = Result<(VerifyingKey, ProgressMessage), ProgressMessageReceiveError>;

// The rendezvous is inside the initially-empty branch, after its future
// deadline check. Sending/dropping after this helper returns must therefore
// exercise recv_timeout rather than accidentally satisfying try_recv. Whether
// the OS has parked the receiver thread at that instant is intentionally not
// asserted. Every wait has a generous bound; no sleeps or elapsed-time limits.
fn begin_empty_wait(
    receive_timeout: std::time::Duration,
) -> (
    mpsc::Sender<(VerifyingKey, ProgressMessage)>,
    mpsc::Receiver<ReceiveOutcome>,
    std::thread::JoinHandle<()>,
) {
    let (sender, receiver) = mpsc::channel();
    let (entered, entered_receiver) = mpsc::sync_channel(0);
    let (finished, outcome) = mpsc::channel();
    let mut stub = ProgressMessageStub::new(receiver, BufferSize::new(1_000_000));
    stub.before_retry_wait = Some(Box::new(move || {
        entered
            .send(())
            .expect("parent must observe the empty wait branch");
    }));
    let worker = std::thread::spawn(move || {
        let result = stub.recv_before_body_retry(
            ChainID::new(0),
            ViewNumber::new(1),
            Instant::now() + receive_timeout,
        );
        finished
            .send(result)
            .expect("parent must observe the receive result");
    });
    entered_receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("receiver must reach the initially-empty future-deadline branch");
    (sender, outcome, worker)
}

#[test]
fn initially_empty_future_deadline_receives_an_arrival_after_empty_observation() {
    let (message_sender, outcome, worker) = begin_empty_wait(std::time::Duration::from_secs(5));
    message_sender.send((sender(), request(0, 1, 42))).unwrap();
    let received = outcome
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert_eq!(request_id(received), CryptoHash::new([42; 32]));
    worker.join().unwrap();
}

#[test]
fn initially_empty_future_deadline_times_out_with_sender_still_connected() {
    let (message_sender, outcome, worker) = begin_empty_wait(std::time::Duration::from_secs(1));
    assert!(matches!(
        outcome
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        Err(ProgressMessageReceiveError::Timeout)
    ));
    worker.join().unwrap();
    drop(message_sender);
}

#[test]
fn initially_empty_future_deadline_observes_disconnection_after_empty_observation() {
    let (message_sender, outcome, worker) = begin_empty_wait(std::time::Duration::from_secs(5));
    drop(message_sender);
    assert!(matches!(
        outcome
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap(),
        Err(ProgressMessageReceiveError::Disconnected)
    ));
    worker.join().unwrap();
}
