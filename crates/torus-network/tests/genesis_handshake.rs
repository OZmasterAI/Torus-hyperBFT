//! Owner s138 (K question, option B): nodes compare their genesis digest when
//! peering and refuse a peer whose digest differs (see
//! `torus_network::genesis_id`). Two real QUIC nodes in-process:
//!
//! - same digest: they connect and exchange consensus messages (direct send
//!   and gossip), no refusal counted;
//! - different digest (the K-only difference of dead-leader phase 3 is one):
//!   nothing crosses in either direction and both sides count the refusal;
//! - a node without a digest (no `--genesis`) and one with: same refusal.

use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{SigningKey, VerifyingKey};
use hotstuff_rs::hotstuff::messages::{HotStuffMessage, PhaseVote};
use hotstuff_rs::hotstuff::types::Phase;
use hotstuff_rs::networking::messages::Message;
use hotstuff_rs::networking::network::Network;
use hotstuff_rs::types::data_types::{ChainID, CryptoHash, Power, SignatureBytes, ViewNumber};
use hotstuff_rs::types::validator_set::ValidatorSet;
use libp2p::{Multiaddr, PeerId};
use torus_network::bridge::peer_id_from_verifying_key;
use torus_network::{LibP2PNetwork, NativeGossipHandle, NetworkConfig, TxGossipHandle};
use torus_telemetry::Metrics;

const DIGEST_A: [u8; 32] = [0xa1; 32];
const DIGEST_B: [u8; 32] = [0xb2; 32];

fn seed_key(seed: u8) -> SigningKey {
    let mut b = [0u8; 32];
    b[0] = seed;
    b[31] = 0x6c;
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

struct Node {
    net: LibP2PNetwork,
    vk: VerifyingKey,
    metrics: Arc<Metrics>,
    port: u16,
    // Dropping a gossip handle closes its channel and the swarm loop's biased
    // select would spin on it: keep them alive.
    _handles: (TxGossipHandle, NativeGossipHandle),
}

async fn start(
    seed: u8,
    port: u16,
    peers: Vec<(PeerId, Multiaddr)>,
    digest: Option<[u8; 32]>,
) -> Node {
    let sk = seed_key(seed);
    let cfg = NetworkConfig {
        listen_addr: quic_addr(port),
        bootstrap_peers: peers,
        allow_private_addrs: true,
        genesis_digest: digest,
        ..Default::default()
    };
    let metrics = Arc::new(Metrics::new());
    let (net, tx, na) = LibP2PNetwork::with_metrics(cfg, sk.clone(), Some(metrics.clone()))
        .await
        .unwrap();
    Node {
        net,
        vk: sk.verifying_key(),
        metrics,
        port,
        _handles: (tx, na),
    }
}

/// Two nodes, B dialing A, both with both in the validator set.
async fn pair(digest_a: Option<[u8; 32]>, digest_b: Option<[u8; 32]>) -> (Node, Node) {
    let (pa, pb) = (free_udp_port(), free_udp_port());
    let a = start(41, pa, vec![], digest_a).await;
    let b = start(
        42,
        pb,
        vec![(peer_id_from_verifying_key(&a.vk), quic_addr(pa))],
        digest_b,
    )
    .await;
    let mut vs = ValidatorSet::new();
    vs.put(&a.vk, Power::new(100));
    vs.put(&b.vk, Power::new(100));
    let (mut a, mut b) = (a, b);
    a.net.init_validator_set(vs.clone());
    b.net.init_validator_set(vs);
    (a, b)
}

/// For `budget`: each side sends a direct message and a gossip broadcast to
/// the other every 200 ms. Returns (a heard b, b heard a); stops early once
/// both heard and `stop_when_both` is set.
async fn exchange(
    a: &mut Node,
    b: &mut Node,
    budget: Duration,
    stop_when_both: bool,
) -> (bool, bool) {
    let start = Instant::now();
    let (mut a_heard, mut b_heard) = (false, false);
    let mut n = 0u64;
    while start.elapsed() < budget {
        n += 1;
        a.net.send(b.vk, vote(n));
        b.net.send(a.vk, vote(n));
        a.net.broadcast(vote(1_000_000 + n));
        b.net.broadcast(vote(1_000_000 + n));
        tokio::time::sleep(Duration::from_millis(200)).await;
        while let Some((from, _)) = a.net.recv() {
            a_heard |= from == b.vk;
        }
        while let Some((from, _)) = b.net.recv() {
            b_heard |= from == a.vk;
        }
        if stop_when_both && a_heard && b_heard {
            break;
        }
    }
    (a_heard, b_heard)
}

async fn wait_refusals(a: &Node, b: &Node, budget: Duration) {
    let start = Instant::now();
    while start.elapsed() < budget {
        if a.metrics.p2p_genesis_mismatch_refusals.get() > 0
            && b.metrics.p2p_genesis_mismatch_refusals.get() > 0
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_genesis_peers_connect_and_exchange_messages() {
    let _ = tracing_subscriber::fmt::try_init();
    let (mut a, mut b) = pair(Some(DIGEST_A), Some(DIGEST_A)).await;
    let (a_heard, b_heard) = exchange(&mut a, &mut b, Duration::from_secs(30), true).await;
    assert!(
        a_heard && b_heard,
        "same genesis must peer: a_heard={a_heard} b_heard={b_heard}"
    );
    assert_eq!(a.metrics.p2p_genesis_mismatch_refusals.get(), 0);
    assert_eq!(b.metrics.p2p_genesis_mismatch_refusals.get(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_genesis_peer_is_refused() {
    let _ = tracing_subscriber::fmt::try_init();
    let (mut a, mut b) = pair(Some(DIGEST_A), Some(DIGEST_B)).await;
    wait_refusals(&a, &b, Duration::from_secs(20)).await;
    let (a_heard, b_heard) = exchange(&mut a, &mut b, Duration::from_secs(6), false).await;
    assert!(
        a.metrics.p2p_genesis_mismatch_refusals.get() > 0
            && b.metrics.p2p_genesis_mismatch_refusals.get() > 0,
        "both sides refuse: a={} b={}",
        a.metrics.p2p_genesis_mismatch_refusals.get(),
        b.metrics.p2p_genesis_mismatch_refusals.get()
    );
    assert!(
        !a_heard && !b_heard,
        "no message crosses a genesis mismatch: a_heard={a_heard} b_heard={b_heard}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_without_genesis_digest_is_refused() {
    let _ = tracing_subscriber::fmt::try_init();
    let (mut a, mut b) = pair(Some(DIGEST_A), None).await;
    wait_refusals(&a, &b, Duration::from_secs(20)).await;
    let (a_heard, b_heard) = exchange(&mut a, &mut b, Duration::from_secs(6), false).await;
    assert!(
        a.metrics.p2p_genesis_mismatch_refusals.get() > 0
            && b.metrics.p2p_genesis_mismatch_refusals.get() > 0,
        "both sides refuse: a={} b={}",
        a.metrics.p2p_genesis_mismatch_refusals.get(),
        b.metrics.p2p_genesis_mismatch_refusals.get()
    );
    assert!(
        !a_heard && !b_heard,
        "no message crosses a genesis mismatch: a_heard={a_heard} b_heard={b_heard}"
    );
}

fn refusals(a: &Node, b: &Node) -> (u64, u64) {
    (
        a.metrics.p2p_genesis_mismatch_refusals.get(),
        b.metrics.p2p_genesis_mismatch_refusals.get(),
    )
}

/// Codex P2 on the handshake branch: a refused validator stays in the peer
/// map, so every consensus send (here: a direct send and a broadcast every
/// 200 ms, as hotstuff does) redialed it. Each new QUIC connection ran
/// identify again and was refused again: ~130 connections and refusal lines
/// per side in 6 s. A refused peer must be held off for a cooldown across
/// every dial path, so the 6 s of sends add at most a couple of connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_peer_is_not_redialed_by_ordinary_sends() {
    let _ = tracing_subscriber::fmt::try_init();
    let (mut a, mut b) = pair(Some(DIGEST_A), Some(DIGEST_B)).await;
    wait_refusals(&a, &b, Duration::from_secs(20)).await;
    let before = refusals(&a, &b);
    assert!(
        before.0 > 0 && before.1 > 0,
        "both sides refused: {before:?}"
    );
    let (a_heard, b_heard) = exchange(&mut a, &mut b, Duration::from_secs(6), false).await;
    let after = refusals(&a, &b);
    assert!(!a_heard && !b_heard, "nothing crosses");
    assert!(
        after.0 - before.0 <= 3 && after.1 - before.1 <= 3,
        "a refused peer is not redialed in a loop: refusals before={before:?} after 6 s of sends={after:?}"
    );
}

/// The cooldown is not a permanent ban: a validator refused for its genesis
/// and restarted (same key) WITH the right genesis is accepted once the
/// cooldown has passed, and messages, including ones buffered meanwhile,
/// flow both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_restarted_with_the_right_genesis_is_accepted_after_cooldown() {
    let _ = tracing_subscriber::fmt::try_init();
    let (mut a, b) = pair(Some(DIGEST_A), Some(DIGEST_B)).await;
    wait_refusals(&a, &b, Duration::from_secs(20)).await;
    assert!(
        a.metrics.p2p_genesis_mismatch_refusals.get() > 0,
        "a refused b"
    );
    // Restart b with the same key, A's genesis, on a fresh port.
    drop(b);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let a_peer = (peer_id_from_verifying_key(&a.vk), quic_addr(a.port));
    let mut b = start(42, free_udp_port(), vec![a_peer], Some(DIGEST_A)).await;
    let mut vs = ValidatorSet::new();
    vs.put(&a.vk, Power::new(100));
    vs.put(&b.vk, Power::new(100));
    b.net.init_validator_set(vs);
    let (a_heard, b_heard) = exchange(&mut a, &mut b, Duration::from_secs(60), true).await;
    assert!(
        a_heard && b_heard,
        "a peer restarted with the right genesis is accepted after the cooldown: a_heard={a_heard} b_heard={b_heard}"
    );
}
