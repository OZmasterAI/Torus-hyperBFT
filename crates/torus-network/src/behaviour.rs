use std::time::Duration;

use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    allow_block_list, connection_limits, gossipsub, identify, kad, request_response,
    StreamProtocol,
};

use crate::codec::{BlockDataCodec, BorshCodec};
use crate::sync::{SyncRequest, SyncResponse};

pub const CONSENSUS_TOPIC: &str = "/torus/consensus/1.0";
pub const TX_TOPIC: &str = "/torus/transactions/1.0";

#[derive(NetworkBehaviour)]
pub struct TorusBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub direct: request_response::Behaviour<BorshCodec>,
    pub sync_proto: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    /// Connection limits enforcement (Phase 3: 3.1.7).
    pub connection_limits: connection_limits::Behaviour,
    /// Dedicated block-data fetch protocol (hybrid pipelining Task 2).
    pub block_data: request_response::Behaviour<BlockDataCodec>,
    /// Peer block list for banning (Phase 3: 3.1.7).
    pub block_list: allow_block_list::Behaviour<allow_block_list::BlockedPeers>,
}

impl TorusBehaviour {
    pub fn new(key: &libp2p::identity::Keypair) -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_limits(key, 100)
    }

    pub fn with_limits(
        key: &libp2p::identity::Keypair,
        max_peers: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let peer_id = key.public().to_peer_id();

        // GossipSub
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_millis(500))
            .max_transmit_size(256 * 1024)
            .validation_mode(gossipsub::ValidationMode::Strict)
            .build()
            .map_err(|e| format!("gossipsub config: {e}"))?;
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(key.clone()),
            gossipsub_config,
        )
        .map_err(|e| format!("gossipsub: {e}"))?;

        // Direct message request-response (borsh codec)
        let direct = request_response::Behaviour::<BorshCodec>::new(
            [(
                StreamProtocol::new("/torus/direct/1.0"),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Block data fetch request-response (borsh codec, hybrid pipelining)
        let block_data = request_response::Behaviour::<BlockDataCodec>::new(
            [(
                StreamProtocol::new("/torus/block-data/1.0"),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Block sync request-response (cbor codec)
        let sync_proto = request_response::cbor::Behaviour::<SyncRequest, SyncResponse>::new(
            [(
                StreamProtocol::new("/torus/sync/1.0"),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default().with_request_timeout(Duration::from_secs(10)),
        );

        // Kademlia DHT — server mode so nodes respond to bootstrap queries
        // and share peer addresses with each other.
        let mut kademlia = kad::Behaviour::new(peer_id, kad::store::MemoryStore::new(peer_id));
        kademlia.set_mode(Some(kad::Mode::Server));

        // Identify protocol
        let identify =
            identify::Behaviour::new(identify::Config::new("/torus/1.0".into(), key.public()));

        // Connection limits (Phase 3: 3.1.7)
        // 80% inbound, 20% reserved for outbound
        let max_inbound = (max_peers * 80) / 100;
        let max_outbound = max_peers - max_inbound;
        let limits = connection_limits::ConnectionLimits::default()
            .with_max_established_incoming(Some(max_inbound as u32))
            .with_max_established_outgoing(Some(max_outbound as u32))
            .with_max_established(Some(max_peers as u32))
            .with_max_established_per_peer(Some(2));
        let connection_limits = connection_limits::Behaviour::new(limits);

        // Block list (Phase 3: 3.1.7)
        let block_list = allow_block_list::Behaviour::default();

        Ok(Self {
            gossipsub,
            direct,
            block_data,
            sync_proto,
            kademlia,
            identify,
            connection_limits,
            block_list,
        })
    }
}
