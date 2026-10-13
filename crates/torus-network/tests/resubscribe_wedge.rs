//! Gossipsub resubscribe wedge (in-place fast restart), end-to-end over QUIC.
//!
//! libp2p-gossipsub sends our subscriptions to a peer only on that peer's FIRST
//! connection (`on_connection_established` returns early when
//! `other_established > 0`). A validator killed with SIGKILL and restarted on
//! the same key/port within a few seconds reconnects while its peers still
//! hold the dead QUIC connection (idle timeout 10 s), so they never resend
//! their subscriptions: the restarted node sees its peers connected but
//! unsubscribed, its consensus publishes fail with `NoPeersSubscribedToTopic`,
//! and with 3 validators (quorum = all) the chain halts until the mesh
//! watchdog forces a reconnect.
//!
//! Node B runs in a CHILD PROCESS (this test binary re-executed into the
//! `#[ignore]`d `wedge_child_node` entry point) so the kill is a real SIGKILL:
//! no QUIC CONNECTION_CLOSE ever reaches A or C, exactly like `kill -9` /
//! `docker restart -t0` on a live validator. A and C run in-process.

use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
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
use torus_network::{LibP2PNetwork, NetworkConfig};

const CHILD_ENV: &str = "TORUS_WEDGE_CHILD";
/// Restarted-node publishes must reach both peers within this budget. Before
/// the fix recovery waits for the 60 s watchdog grace + a 10 s tick.
const RECOVERY_BUDGET: Duration = Duration::from_secs(15);
/// Views carry `generation * GEN_STRIDE + counter`, so the parent can tell
/// post-restart publishes apart from anything the first incarnation sent.
const GEN_STRIDE: u64 = 1_000_000;

fn seed_key(seed: u8) -> SigningKey {
    let mut b = [0u8; 32];
    b[0] = seed;
    b[31] = 0xA5;
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

fn node_config(port: u16, peers: Vec<(PeerId, Multiaddr)>) -> NetworkConfig {
    NetworkConfig {
        listen_addr: quic_addr(port),
        bootstrap_peers: peers,
        // Loopback mesh: the kademlia/identify address book must accept
        // 127.0.0.1 or the mesh-maintenance redial has nothing to dial.
        allow_private_addrs: true,
        ..Default::default()
    }
}

/// A unique consensus message per publish (the view carries the payload).
fn vote(view: u64) -> Message {
    HotStuffMessage::PhaseVote(PhaseVote {
        chain_id: ChainID::new(7),
        view: ViewNumber::new(view),
        block: CryptoHash::new([9; 32]),
        phase: Phase::Generic,
        signature: SignatureBytes::new([0; 64]),
    })
    .into()
}

fn view_of(msg: &Message) -> Option<u64> {
    use hotstuff_rs::networking::messages::ProgressMessage;
    match msg {
        Message::ProgressMessage(ProgressMessage::HotStuffMessage(HotStuffMessage::PhaseVote(
            v,
        ))) => Some(v.view.int()),
        _ => None,
    }
}

/// Kills node B's process on drop, so a failing assertion never leaks it.
struct ChildGuard(Child);

impl ChildGuard {
    fn kill(&mut self) {
        let _ = self.0.kill(); // SIGKILL on unix: no graceful QUIC close
        let _ = self.0.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

struct ChildSpec {
    seed: u8,
    port: u16,
    generation: u64,
    peers: Vec<(PeerId, Multiaddr)>,
    validators: Vec<VerifyingKey>,
}

fn spawn_child(spec: &ChildSpec) -> ChildGuard {
    let peers = spec
        .peers
        .iter()
        .map(|(p, a)| format!("{a}/p2p/{p}"))
        .collect::<Vec<_>>()
        .join(",");
    let vals = spec
        .validators
        .iter()
        .map(|vk| hex::encode(vk.to_bytes()))
        .collect::<Vec<_>>()
        .join(",");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["wedge_child_node", "--exact", "--ignored", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("WEDGE_SEED", spec.seed.to_string())
        .env("WEDGE_PORT", spec.port.to_string())
        .env("WEDGE_GEN", spec.generation.to_string())
        .env("WEDGE_PEERS", peers)
        .env("WEDGE_VALIDATORS", vals)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn node B child");
    ChildGuard(child)
}

/// Node B (child process): join the 3-validator mesh and publish one unique
/// consensus message every 200 ms until killed (hard cap 180 s as a safety
/// net should the parent die without reaping it).
#[tokio::test]
#[ignore = "child-process entry point for restarted_validator_can_publish_within_budget"]
async fn wedge_child_node() {
    if std::env::var(CHILD_ENV).is_err() {
        return;
    }
    let seed: u8 = std::env::var("WEDGE_SEED").unwrap().parse().unwrap();
    let port: u16 = std::env::var("WEDGE_PORT").unwrap().parse().unwrap();
    let generation: u64 = std::env::var("WEDGE_GEN").unwrap().parse().unwrap();
    let peers = NetworkConfig::parse_bootstrap_peers(&std::env::var("WEDGE_PEERS").unwrap());
    let validators: Vec<VerifyingKey> = std::env::var("WEDGE_VALIDATORS")
        .unwrap()
        .split(',')
        .map(|h| {
            let b: [u8; 32] = hex::decode(h).unwrap().try_into().unwrap();
            VerifyingKey::from_bytes(&b).unwrap()
        })
        .collect();

    let (mut net, _tx, _na) = LibP2PNetwork::new(node_config(port, peers), seed_key(seed))
        .await
        .unwrap();
    net.init_validator_set(validator_set(&validators));

    let start = Instant::now();
    let mut counter = 0u64;
    while start.elapsed() < Duration::from_secs(180) {
        counter += 1;
        net.broadcast(vote(generation * GEN_STRIDE + counter));
        while net.recv().is_some() {}
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait until every net in `nets` has received a message from `from` whose
/// view is in generation `generation`. Returns the elapsed time, or `None`
/// on timeout.
async fn wait_for_generation(
    nets: &mut [&mut LibP2PNetwork],
    from: VerifyingKey,
    generation: u64,
    budget: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    let mut got = vec![false; nets.len()];
    while start.elapsed() < budget {
        for (i, net) in nets.iter_mut().enumerate() {
            while let Some((vk, msg)) = net.recv() {
                if vk == from && view_of(&msg).is_some_and(|v| v / GEN_STRIDE == generation) {
                    got[i] = true;
                }
            }
        }
        if got.iter().all(|g| *g) {
            return Some(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

/// RED (s114): kill -9 a validator and restart it on the same key/port within
/// ~1.5 s; its consensus publishes must reach both peers within
/// [`RECOVERY_BUDGET`]. Before the fix the restarted node stays
/// connected-but-unsubscribed until the 60 s watchdog grace expires.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restarted_validator_can_publish_within_budget() {
    let _ = tracing_subscriber::fmt::try_init();
    let budget = std::env::var("TORUS_WEDGE_TEST_BUDGET_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(RECOVERY_BUDGET);

    let (sk_a, sk_c) = (seed_key(1), seed_key(3));
    let (vk_a, vk_b, vk_c) = (
        sk_a.verifying_key(),
        seed_key(2).verifying_key(),
        sk_c.verifying_key(),
    );
    let (pid_a, pid_b, pid_c) = (
        peer_id_from_verifying_key(&vk_a),
        peer_id_from_verifying_key(&vk_b),
        peer_id_from_verifying_key(&vk_c),
    );
    let (port_a, port_b, port_c) = (free_udp_port(), free_udp_port(), free_udp_port());
    let validators = vec![vk_a, vk_b, vk_c];

    let spec = |generation| ChildSpec {
        seed: 2,
        port: port_b,
        generation,
        peers: vec![(pid_a, quic_addr(port_a)), (pid_c, quic_addr(port_c))],
        validators: validators.clone(),
    };

    let mut child = spawn_child(&spec(1));
    let (mut net_a, _ta, _na) = LibP2PNetwork::new(
        node_config(
            port_a,
            vec![(pid_b, quic_addr(port_b)), (pid_c, quic_addr(port_c))],
        ),
        sk_a,
    )
    .await
    .unwrap();
    let (mut net_c, _tc, _nc) = LibP2PNetwork::new(
        node_config(
            port_c,
            vec![(pid_a, quic_addr(port_a)), (pid_b, quic_addr(port_b))],
        ),
        sk_c,
    )
    .await
    .unwrap();
    net_a.init_validator_set(validator_set(&validators));
    net_c.init_validator_set(validator_set(&validators));

    // Baseline: the healthy mesh carries B's publishes to A and C.
    let baseline = wait_for_generation(
        &mut [&mut net_a, &mut net_c],
        vk_b,
        1,
        Duration::from_secs(30),
    )
    .await
    .expect("baseline: B's publishes never reached A and C on a fresh mesh");
    eprintln!("baseline mesh delivery after {baseline:?}");
    // Let the mesh settle (heartbeats, explicit peering) like a live chain.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // kill -9 B, restart it on the same key and port within ~1.5 s, while A
    // and C still hold the dead QUIC connection.
    child.kill();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let _child2 = spawn_child(&spec(2));
    let restarted = Instant::now();

    let recovered = wait_for_generation(&mut [&mut net_a, &mut net_c], vk_b, 2, budget).await;
    let line = format!(
        "baseline {baseline:?}; restart -> B's publishes at A and C: {recovered:?} \
         (budget {budget:?}, wall {:?})\n",
        restarted.elapsed()
    );
    eprint!("{line}");
    // Optional timing report for A/B runs (TESTING.md: never grep test logs).
    if let Ok(path) = std::env::var("TORUS_WEDGE_TEST_REPORT") {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }
    assert!(
        recovered.is_some(),
        "restarted validator could not publish to its peers within {budget:?} \
         (gossipsub resubscribe wedge)"
    );
}
