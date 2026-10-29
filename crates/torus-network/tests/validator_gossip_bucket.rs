//! s140: a validator's consensus gossip at bench-like view rates is not
//! dropped by the receiver's per-author consensus rate limiter.
//!
//! Perf doc section 139: each validator gossips ~2 consensus messages per view
//! and the bench runs up to ~51 views in a second, so the pre-s140 limiter (50
//! per s, burst 50, one bucket for every author) dropped the leader's
//! proposal header and the view stalled until the 250 ms header re-send. The
//! fix charges validator-set authors a separate, larger bucket.
//!
//! Two in-process nodes over QUIC on loopback, both in each other's validator
//! set. A publishes 400 unique consensus messages over 2 s (200 per s, i.e. 2
//! per view at 100 views/s: 4x the legacy rate, half the validator rate); B
//! must receive every one, and its validator-drop counter must stay 0.
//! Before the fix B receives ~150 (50 burst + 2 s x 50/s).

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{SigningKey, VerifyingKey};
use hotstuff_rs::hotstuff::messages::{HotStuffMessage, PhaseVote};
use hotstuff_rs::hotstuff::types::Phase;
use hotstuff_rs::networking::messages::{Message, ProgressMessage};
use hotstuff_rs::networking::network::Network;
use hotstuff_rs::types::data_types::{ChainID, CryptoHash, Power, SignatureBytes, ViewNumber};
use hotstuff_rs::types::validator_set::ValidatorSet;
use libp2p::Multiaddr;
use torus_network::bridge::peer_id_from_verifying_key;
use torus_network::{LibP2PNetwork, NetworkConfig};

/// Views of the probe messages that wait for the mesh (below the burst's).
const PROBE_BASE: u64 = 1_000_000;

fn seed_key(seed: u8) -> SigningKey {
    let mut b = [0u8; 32];
    b[0] = seed;
    b[31] = 0x5C;
    SigningKey::from_bytes(&b)
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn quic_addr(port: u16) -> Multiaddr {
    format!("/ip4/127.0.0.1/udp/{port}/quic-v1")
        .parse()
        .unwrap()
}

fn validator_set(vks: &[VerifyingKey]) -> ValidatorSet {
    let mut vs = ValidatorSet::new();
    for vk in vks {
        vs.put(vk, Power::new(100));
    }
    vs
}

fn vote(view: u64) -> Message {
    HotStuffMessage::PhaseVote(PhaseVote {
        chain_id: ChainID::new(7),
        view: ViewNumber::new(view),
        block: CryptoHash::new([3; 32]),
        phase: Phase::Generic,
        signature: SignatureBytes::new([0; 64]),
    })
    .into()
}

fn view_of(msg: &Message) -> Option<u64> {
    match msg {
        Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::PhaseVote(
            v,
        ))) => Some(v.view.int()),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validator_burst_at_bench_view_rate_is_not_dropped() {
    // 400 messages 5 ms apart: 200 per s for 2 s.
    run_burst(400, Duration::from_micros(5_000)).await;
}

/// s140 review: past the 400 burst, on the refill rate. 1,200 messages
/// 2.5 ms apart (<= 400 per s for >= 3 s; sleeps only stretch the gap) need
/// the validator refill, not just the burst. Before the fix: ~200 received.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validator_stream_beyond_burst_rides_the_refill() {
    run_burst(1_200, Duration::from_micros(2_500)).await;
}

/// Two nodes, A publishes `burst` unique messages `gap` apart, B must get all.
async fn run_burst(burst: u64, gap: Duration) {
    let (sk_a, sk_b) = (seed_key(1), seed_key(2));
    let (vk_a, vk_b) = (sk_a.verifying_key(), sk_b.verifying_key());
    let (pid_a, pid_b) = (
        peer_id_from_verifying_key(&vk_a),
        peer_id_from_verifying_key(&vk_b),
    );
    let (port_a, port_b) = (free_udp_port(), free_udp_port());
    let config = |port, peers| NetworkConfig {
        listen_addr: quic_addr(port),
        bootstrap_peers: peers,
        allow_private_addrs: true,
        ..Default::default()
    };

    let metrics_b = Arc::new(torus_telemetry::Metrics::new());
    let (mut net_a, _ta, _na) =
        LibP2PNetwork::new(config(port_a, vec![(pid_b, quic_addr(port_b))]), sk_a)
            .await
            .unwrap();
    let (mut net_b, _tb, _nb) = LibP2PNetwork::with_metrics(
        config(port_b, vec![(pid_a, quic_addr(port_a))]),
        sk_b,
        Some(metrics_b.clone()),
    )
    .await
    .unwrap();
    let vs = validator_set(&[vk_a, vk_b]);
    net_a.init_validator_set(vs.clone());
    net_b.init_validator_set(vs);

    // Wait for the mesh: A probes every 200 ms until B hears one.
    let start = Instant::now();
    let mut probe = 0u64;
    'mesh: loop {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "mesh never formed: B heard no probe from A"
        );
        probe += 1;
        net_a.broadcast(vote(PROBE_BASE + probe));
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            while net_a.recv().is_some() {}
            while let Some((vk, msg)) = net_b.recv() {
                if vk == vk_a && view_of(&msg).is_some_and(|v| v > PROBE_BASE) {
                    break 'mesh;
                }
            }
        }
    }
    // Let the probe traffic's tokens refill (> 1 s even at the legacy rate),
    // so the burst starts from a full bucket in both arms.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    while net_b.recv().is_some() {}
    let drops_before = metrics_b.consensus_rate_limited_dropped.get();

    let mut got = std::collections::HashSet::new();
    for view in 1..=burst {
        net_a.broadcast(vote(view));
        tokio::time::sleep(gap).await;
        while net_a.recv().is_some() {}
        while let Some((vk, msg)) = net_b.recv() {
            if let Some(v) = view_of(&msg).filter(|v| vk == vk_a && *v <= burst) {
                got.insert(v);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while (got.len() as u64) < burst && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        while let Some((vk, msg)) = net_b.recv() {
            if let Some(v) = view_of(&msg).filter(|v| vk == vk_a && *v <= burst) {
                got.insert(v);
            }
        }
    }

    let dropped = metrics_b.consensus_rate_limited_dropped.get() - drops_before;
    assert_eq!(
        metrics_b.consensus_rate_limited_validator_dropped.get(),
        0,
        "B's limiter dropped validator A's messages (validator bucket)"
    );
    assert_eq!(
        got.len() as u64,
        burst,
        "B received {} of A's {burst} messages; limiter drops during the burst: {dropped}",
        got.len()
    );
    assert_eq!(dropped, 0, "limiter dropped {dropped} of A's messages");
}
