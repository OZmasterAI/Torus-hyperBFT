use super::*;
use borsh::BorshSerialize;
use ed25519_dalek::SigningKey;
use hotstuff_rs::hotstuff::messages::{BlockDataRequest, BlockDataResponse};
use hotstuff_rs::hotstuff::types::PhaseCertificate;
use hotstuff_rs::types::block::Block;
use hotstuff_rs::types::data_types::{BlockHeight, ChainID, CryptoHash, Data, Datum, ViewNumber};

fn target() -> VerifyingKey {
    SigningKey::from_bytes(&[71; 32]).verifying_key()
}

pub(crate) fn messages() -> [Message; 2] {
    let block = Block::new(
        BlockHeight::new(3),
        PhaseCertificate::genesis_pc(),
        CryptoHash::new([2; 32]),
        Data::new(vec![Datum::new(vec![9, 8, 7])]),
    );
    [
        HotStuffMessage::BlockDataRequest(BlockDataRequest {
            chain_id: ChainID::new(7),
            view: ViewNumber::new(42),
            block_hash: block.hash,
        })
        .into(),
        HotStuffMessage::BlockDataResponse(BlockDataResponse {
            view: ViewNumber::new(42),
            block,
        })
        .into(),
    ]
}

#[test]
fn body_send_trace_exact_flag_and_non_body_parity() {
    assert!(flag(Some("1")));
    for value in [
        None,
        Some("0"),
        Some("true"),
        Some("01"),
        Some(" 1"),
        Some("1 "),
    ] {
        assert!(!flag(value));
    }
    let message: Message = hotstuff_rs::pacemaker::messages::PacemakerMessage::advance_view(
        hotstuff_rs::pacemaker::messages::ProgressCertificate::PhaseCertificate(
            PhaseCertificate::genesis_pc(),
        ),
    )
    .into();
    let expected = message.try_to_vec().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(enqueue(&tx, target(), message, true).is_none());
    let NetworkCommand::Send { message, .. } = rx.try_recv().unwrap() else {
        panic!("non-body messages must retain ordinary commands");
    };
    assert_eq!(message.try_to_vec().unwrap(), expected);
}

#[test]
fn body_send_trace_request_response_bytes_and_disabled_command_parity() {
    for message in messages() {
        let expected = message.try_to_vec().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(enqueue(&tx, target(), message.clone(), false).is_none());
        let NetworkCommand::Send { message: plain, .. } = rx.try_recv().unwrap() else {
            panic!("OFF must retain ordinary command");
        };
        let record = enqueue(&tx, target(), message, true).unwrap();
        assert!(record.accepted);
        let NetworkCommand::SendTraced {
            message: traced,
            trace,
            ..
        } = rx.try_recv().unwrap()
        else {
            panic!("ON body must carry local trace");
        };
        assert_eq!(plain.try_to_vec().unwrap(), expected);
        assert_eq!(traced.try_to_vec().unwrap(), expected);
        assert_eq!(trace.pre_enqueue.seq, record.pre.seq);
        assert_eq!(trace.meta.view, 42);
        let wire_plain = crate::codec::DirectRequest {
            sender_key: target().to_bytes(),
            payload: plain.try_to_vec().unwrap(),
        };
        let wire_traced = crate::codec::DirectRequest {
            sender_key: target().to_bytes(),
            payload: traced.try_to_vec().unwrap(),
        };
        assert_eq!(
            wire_plain.try_to_vec().unwrap(),
            wire_traced.try_to_vec().unwrap()
        );
    }
}

#[test]
fn body_send_trace_coordinated_queue_retains_distinct_duplicate_ids() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
    let producer = std::thread::spawn(move || {
        let request = messages().into_iter().next().unwrap();
        let a = enqueue(&tx, target(), request.clone(), true).unwrap();
        let b = enqueue(&tx, target(), request, true).unwrap();
        ready_tx.send((a, b)).unwrap();
    });
    // The rendezvous proves both producer completions precede either dequeue.
    // No elapsed-time threshold or sleep is needed to manufacture queue delay.
    let (a, b) = ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert_ne!(a.pre.seq, b.pre.seq);
    assert_eq!(a.meta.hash, b.meta.hash);
    assert_eq!(a.meta.view, b.meta.view);
    assert_eq!(a.meta.target, b.meta.target);
    for (index, record) in [a, b].into_iter().enumerate() {
        let NetworkCommand::SendTraced { mut trace, .. } = rx.try_recv().unwrap() else {
            panic!()
        };
        trace.dequeued(rx.len());
        let dequeue = trace.dequeue.unwrap();
        assert_eq!(trace.pre_enqueue.seq, record.pre.seq);
        assert!(dequeue.seq > record.post.seq);
        assert!(dequeue.mono_us >= record.post.mono_us);
        assert_eq!(trace.queue_remaining, Some(1 - index));
        // Consumer and producer records share the ID, independent of emission order.
        assert_eq!(trace.json()["id"], record.pre.seq);
    }
    producer.join().unwrap();
}

#[test]
fn body_send_trace_enqueue_failure_is_explicit_without_send_record() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    drop(rx);
    let record = enqueue(&tx, target(), messages().into_iter().next().unwrap(), true).unwrap();
    assert!(!record.accepted);
    assert!(record.post.seq > record.pre.seq);
    assert!(record.post.mono_us >= record.pre.mono_us);
}

#[test]
fn body_send_trace_missing_stages_are_null_not_zero_durations() {
    let mut trace = BodySendTrace::new(target(), &messages()[0]).unwrap();
    trace.finish("buffered_unmapped");
    let value = trace.json();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["outcome"], "buffered_unmapped");
    assert!(value["encode_begin"].is_null());
    assert!(value["send_end"].is_null());
    assert!(value["request_id"].is_null());
    assert!(value["queue_remaining"].is_null());
    assert_eq!(value["message"]["hash"].as_str().unwrap().len(), 43);
    assert!(value.get("payload").is_none());
}
