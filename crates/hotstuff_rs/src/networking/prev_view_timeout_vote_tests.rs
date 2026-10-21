//! Phase A of docs/plans/tc-required-and-tip-signing.md, C2: TimeoutVotes for
//! `cur - 1` pass the receive filter, and buffered ones survive a skip to
//! `cur`, so the pacemaker can still form TC(cur - 1). Every other message
//! below the current view is dropped as before.
//!
//! RED-first: before phase A both the filter and `remove_expired_msgs` dropped
//! every pacemaker message below the current view.

use super::*;
use crate::hotstuff::messages::PhaseVote;
use crate::hotstuff::types::{Phase, PhaseCertificate};
use crate::pacemaker::messages::{PacemakerMessage, ProgressCertificate};
use crate::types::crypto_primitives::{Keypair, SigningKey};
use crate::types::data_types::{CryptoHash, SignatureSet};

const CHAIN: ChainID = ChainID::new(0);

fn sender() -> VerifyingKey {
    SigningKey::from_bytes(&[1; 32]).verifying_key()
}

fn timeout_vote(view: u64) -> ProgressMessage {
    ProgressMessage::PacemakerMessage(PacemakerMessage::timeout_vote(
        &Keypair::new(SigningKey::from_bytes(&[1; 32])),
        CHAIN,
        ViewNumber::new(view),
        None,
        None,
        None,
    ))
}

fn advance_view(view: u64) -> ProgressMessage {
    let pc = PhaseCertificate {
        chain_id: CHAIN,
        view: ViewNumber::new(view),
        block: CryptoHash::new([3; 32]),
        phase: Phase::Generic,
        signatures: SignatureSet::new(4),
    };
    ProgressMessage::PacemakerMessage(PacemakerMessage::advance_view(
        ProgressCertificate::PhaseCertificate(pc),
    ))
}

fn phase_vote(view: u64) -> ProgressMessage {
    ProgressMessage::HotStuffMessage(
        PhaseVote::new(
            &Keypair::new(SigningKey::from_bytes(&[1; 32])),
            CHAIN,
            ViewNumber::new(view),
            CryptoHash::new([7; 32]),
            Phase::Generic,
        )
        .into(),
    )
}

/// `(kind, view)` of a received message.
fn kind(message: &ProgressMessage) -> (&'static str, u64) {
    match message {
        ProgressMessage::PacemakerMessage(PacemakerMessage::TimeoutVote(tv)) => ("tv", tv.view.int()),
        ProgressMessage::PacemakerMessage(PacemakerMessage::AdvanceView(av)) => {
            ("av", av.progress_certificate.view().int())
        }
        ProgressMessage::HotStuffMessage(m) => ("hs", m.view().int()),
        ProgressMessage::BlockSyncAdvertiseMessage(_) => ("sync", 0),
    }
}

/// Everything `recv` (or the bounded receive) hands out at `cur` right now.
fn drain(stub: &mut ProgressMessageStub, cur: u64, bounded: bool) -> Vec<(&'static str, u64)> {
    let mut out = Vec::new();
    loop {
        let deadline = Instant::now() + std::time::Duration::from_millis(20);
        let got = if bounded {
            stub.recv_before_body_retry(CHAIN, ViewNumber::new(cur), deadline)
        } else {
            stub.recv(CHAIN, ViewNumber::new(cur), deadline)
        };
        match got {
            Ok((_, message)) => out.push(kind(&message)),
            Err(_) => return out,
        }
    }
}

#[test]
fn filter_passes_timeout_votes_for_the_previous_view_only() {
    for bounded in [false, true] {
        let (tx, rx) = mpsc::channel();
        let mut stub = ProgressMessageStub::new(rx, BufferSize::new(1_000_000));
        for message in [timeout_vote(5), timeout_vote(4), advance_view(5), phase_vote(5), timeout_vote(6)] {
            tx.send((sender(), message)).unwrap();
        }
        assert_eq!(
            drain(&mut stub, 6, bounded),
            vec![("tv", 5), ("tv", 6)],
            "bounded={bounded}: TV(cur-1) and TV(cur) pass; TV(cur-2), AV(cur-1), votes(cur-1) do not"
        );
    }
}

#[test]
fn buffered_timeout_votes_for_the_previous_view_survive_a_skip() {
    for bounded in [false, true] {
        let (tx, rx) = mpsc::channel();
        let mut stub = ProgressMessageStub::new(rx, BufferSize::new(1_000_000));
        for message in [timeout_vote(5), advance_view(5), phase_vote(5), timeout_vote(4)] {
            tx.send((sender(), message)).unwrap();
        }
        // At view 3 the future pacemaker messages are delivered and buffered;
        // the future phase vote is only buffered.
        assert_eq!(drain(&mut stub, 3, bounded), vec![("tv", 5), ("av", 5), ("tv", 4)]);
        // Skip to 6: the buffered TV(5) is replayed once; AV(5), the phase
        // vote for 5 and TV(4) are gone.
        assert_eq!(drain(&mut stub, 6, bounded), vec![("tv", 5)], "bounded={bounded}");
        assert_eq!(drain(&mut stub, 6, bounded), vec![], "replayed once");
        assert_eq!(stub.msg_buffer.buffer_size.int(), 0, "bounded={bounded}: buffer accounting");
    }
}
