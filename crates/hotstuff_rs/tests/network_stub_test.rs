//! Tests of the mock network itself.

use rand_core::OsRng;

use hotstuff_rs::{
    hotstuff::messages::{BlockDataRequest, HotStuffMessage},
    networking::{
        messages::{Message, ProgressMessage},
        network::Network,
    },
    types::{
        crypto_primitives::SigningKey,
        data_types::{ChainID, CryptoHash, ViewNumber},
    },
};

mod common;

use crate::common::network::mock_network;

/// `request_block_data` must reach the peer as a `BlockDataRequest`, as it does on the real network
/// (`torus-network` `LibP2PNetwork`). With the trait's no-op default, a replica that missed a pushed
/// body could never fetch it, and integration tests never exercised the body-request/serve path.
#[test]
fn request_block_data_reaches_peer() {
    let mut csprg = OsRng {};
    let keypairs: Vec<SigningKey> = (0..2).map(|_| SigningKey::generate(&mut csprg)).collect();
    let mut stubs = mock_network(keypairs.iter().map(|kp| kp.verifying_key()));

    let hash = CryptoHash::new([7; 32]);
    stubs[0].request_block_data(
        keypairs[1].verifying_key(),
        BlockDataRequest {
            chain_id: ChainID::new(0),
            view: ViewNumber::new(3),
            block_hash: hash,
        },
    );

    match stubs[1].recv() {
        Some((
            origin,
            Message::ProgressMessage(ProgressMessage::HotStuffMessage(
                HotStuffMessage::BlockDataRequest(req),
            )),
        )) => {
            assert_eq!(origin, keypairs[0].verifying_key());
            assert_eq!(req.view, ViewNumber::new(3));
            assert_eq!(req.block_hash, hash);
        }
        _ => panic!("peer did not receive a BlockDataRequest"),
    }
    assert!(stubs[0].recv().is_none(), "requester must not receive its own request");
}
