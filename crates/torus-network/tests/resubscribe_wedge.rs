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
/// Restarted-node publishes must reach both peers within this budget via the
/// peers' stale-connection kick (measured 0.4-0.7 s). Below the watchdog
/// backstop's floor (5 s grace + tick), so the kick itself is what passes.
/// Before the fix: 75 s (60 s grace + 10 s tick).
const RECOVERY_BUDGET: Duration = Duration::from_secs(5);
/// Kill -> restart gap in [`restart_scenario`].
const RESTART_GAP: Duration = Duration::from_millis(1500);

/// Watchdog-only recovery budget, derived from the watchdog's own kick
/// schedule (shipped defaults: grace G = 5 s, tick T = 1 s).
///
/// A and C hold B's dead QUIC connection until it idles out (10 s after the
/// kill), and any kick before that redials into a second connection that is
/// again left unsubscribed. Kicks of one peer back off G, G, 2G, ... (each
/// up to a tick late), so the kicks land at about G+T, 2G+2T and 4G+3T
/// after B is up. The first one (~6 s) always precedes the idle-out
/// (8.5 s after the restart); the second normally recovers (measured
/// 11.5-14.3 s under a 24-busy-loop CPU load, 12.4-17.7 s with the whole
/// binary in one process under that load, s135). The budget covers the
/// THIRD kick (4G + 3T = 23 s) plus 4 s for B's process start, redial,
/// subscription exchange and the next 200 ms publish, times 1.5 for host
/// load: 40.5 s. The old fixed 20 s sat below the third kick and flaked at
/// load avg ~24 (s134).
///
/// Still proves the watchdog ALONE recovers: with the watchdog disabled
/// (`TORUS_MESH_WATCHDOG_GRACE_MS=3600000`) the wedge never clears and the
/// test fails at the budget; and the pre-s114 schedule (60 s grace, 10 s
/// tick: 75 s) is outside it, so a regression to it fails too.
fn watchdog_only_budget() -> Duration {
    use torus_network::mesh_watchdog::{DEFAULT_WATCHDOG_GRACE, DEFAULT_WATCHDOG_TICK};
    let third_kick = 4 * DEFAULT_WATCHDOG_GRACE + 3 * DEFAULT_WATCHDOG_TICK;
    (third_kick + Duration::from_secs(4)).mul_f64(1.5)
}
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
    /// Extra env for the child (e.g. `TORUS_MESH_STALE_CONN_KICK=0`).
    env: Vec<(&'static str, &'static str)>,
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
        .envs(spec.env.iter().copied())
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
#[ignore = "child-process entry point for the restart_scenario tests"]
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

/// Kill -9 node B, restart it on the same key/port ~1.5 s later (A and C
/// still hold the dead QUIC connection(s)), and time until B's post-restart
/// publishes reach both A and C. `stale_kick` sets `mesh_stale_conn_kick` on
/// every node. `b_dials_first`: B's first incarnation also dials A and C, so
/// each pair ends up with TWO connections (simultaneous dial) and the dead
/// pair fills the per-peer cap (restarted B is denied); otherwise only A
/// and C dial B, leaving ONE dead connection each (restarted B connects
/// but is never sent their subscriptions). `None` = timed out after `budget`.
async fn restart_scenario(
    stale_kick: bool,
    b_dials_first: bool,
    budget: Duration,
) -> (Duration, Option<Duration>) {
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
        peers: if generation > 1 || b_dials_first {
            vec![(pid_a, quic_addr(port_a)), (pid_c, quic_addr(port_c))]
        } else {
            vec![]
        },
        validators: validators.clone(),
        env: if stale_kick {
            vec![]
        } else {
            vec![("TORUS_MESH_STALE_CONN_KICK", "0")]
        },
    };
    let config = |port, peers| NetworkConfig {
        mesh_stale_conn_kick: stale_kick,
        ..node_config(port, peers)
    };

    let mut child = spawn_child(&spec(1));
    let (mut net_a, _ta, _na) = LibP2PNetwork::new(
        config(
            port_a,
            vec![(pid_b, quic_addr(port_b)), (pid_c, quic_addr(port_c))],
        ),
        sk_a,
    )
    .await
    .unwrap();
    let (mut net_c, _tc, _nc) = LibP2PNetwork::new(
        config(
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
    // Let the mesh settle (heartbeats, explicit peering) like a live chain.
    tokio::time::sleep(Duration::from_secs(3)).await;

    // kill -9 B, restart it on the same key and port within ~1.5 s, while A
    // and C still hold the dead QUIC connection.
    child.kill();
    tokio::time::sleep(RESTART_GAP).await;
    let _child2 = spawn_child(&spec(2));
    let restarted = Instant::now();

    let recovered = wait_for_generation(&mut [&mut net_a, &mut net_c], vk_b, 2, budget).await;
    let line = format!(
        "stale_kick={stale_kick} b_dials_first={b_dials_first}: baseline {baseline:?}; restart -> B's publishes at A and C: \
         {recovered:?} (budget {budget:?}, wall {:?})\n",
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
    (baseline, recovered)
}

fn budget_or(default: Duration) -> Duration {
    std::env::var("TORUS_WEDGE_TEST_BUDGET_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

/// s114: kill -9 a validator and restart it on the same key/port within
/// ~1.5 s; its consensus publishes must reach both peers within
/// [`RECOVERY_BUDGET`]. Before the fix the restarted node stayed
/// connected-but-unsubscribed until the 60 s watchdog grace expired (75 s
/// measured). Two dead connections per pair: the peers' denial-path
/// stale-connection kick.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restarted_validator_can_publish_within_budget() {
    let _ = tracing_subscriber::fmt::try_init();
    let budget = budget_or(RECOVERY_BUDGET);
    let (_, recovered) = restart_scenario(true, true, budget).await;
    assert!(
        recovered.is_some(),
        "restarted validator could not publish to its peers within {budget:?} \
         (gossipsub resubscribe wedge)"
    );
}

/// s114: same restart with ONE dead connection per pair: the restarted
/// node's connection is accepted as a second one (gossipsub skips our
/// subscriptions) and the peers' established-path stale-connection kick
/// forces the clean reconnect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restarted_validator_single_stale_connection_within_budget() {
    let _ = tracing_subscriber::fmt::try_init();
    let budget = budget_or(RECOVERY_BUDGET);
    let (_, recovered) = restart_scenario(true, false, budget).await;
    assert!(
        recovered.is_some(),
        "restarted validator (one stale connection per peer) could not publish within {budget:?}"
    );
}

/// s114: the unsubscribed-watchdog BACKSTOP alone (stale-connection kick off
/// on every node), one dead connection per pair: the restarted node sees its
/// peers connected but unsubscribed and kicks them after the 5 s grace
/// (1 s tick), retrying with backoff while the peers' dead connection has
/// not idled out yet. Budget: [`watchdog_only_budget`] (third kick + slack,
/// 40.5 s). With the old 60 s grace / 10 s tick: 75 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restarted_validator_recovers_via_watchdog_alone() {
    let _ = tracing_subscriber::fmt::try_init();
    let budget = budget_or(watchdog_only_budget());
    let (_, recovered) = restart_scenario(false, false, budget).await;
    assert!(
        recovered.is_some(),
        "watchdog alone did not recover the restarted validator within {budget:?}"
    );
}

/// s114 anti-flap: a healthy 3-validator mesh (bootstrapped at once, so
/// simultaneous dials happen) running consensus-like traffic never triggers
/// the watchdog or the stale-connection kick, even at an aggressive 2 s
/// grace / 200 ms tick, and every node sees both peers subscribed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn healthy_mesh_never_triggers_watchdog() {
    use std::sync::Arc;
    use torus_telemetry::Metrics;

    let _ = tracing_subscriber::fmt::try_init();
    let sks = [seed_key(11), seed_key(12), seed_key(13)];
    let vks: Vec<VerifyingKey> = sks.iter().map(|s| s.verifying_key()).collect();
    let pids: Vec<PeerId> = vks.iter().map(peer_id_from_verifying_key).collect();
    let ports = [free_udp_port(), free_udp_port(), free_udp_port()];

    let mut nets = Vec::new();
    let mut handles = Vec::new();
    let mut metrics = Vec::new();
    for i in 0..3 {
        let peers = (0..3)
            .filter(|j| *j != i)
            .map(|j| (pids[j], quic_addr(ports[j])))
            .collect();
        let cfg = NetworkConfig {
            mesh_watchdog_grace: Duration::from_secs(2),
            mesh_watchdog_tick: Duration::from_millis(200),
            mesh_stale_conn_kick: true,
            ..node_config(ports[i], peers)
        };
        let m = Arc::new(Metrics::new());
        // Keep the gossip handles alive: a dropped handle closes its channel
        // and the swarm loop's biased select would spin on the closed
        // receiver, starving the timer arms (watchdog included).
        let (mut net, tx, na) = LibP2PNetwork::with_metrics(cfg, sks[i].clone(), Some(m.clone()))
            .await
            .unwrap();
        net.init_validator_set(validator_set(&vks));
        handles.push((tx, na));
        nets.push(net);
        metrics.push(m);
    }

    // 20 s of traffic: every node publishes every 200 ms; track who heard
    // whom during the last 5 s.
    let start = Instant::now();
    let mut heard = [[false; 3]; 3];
    let mut counter = 0u64;
    while start.elapsed() < Duration::from_secs(20) {
        counter += 1;
        for net in nets.iter_mut() {
            net.broadcast(vote(counter));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let late = start.elapsed() > Duration::from_secs(15);
        for (i, net) in nets.iter_mut().enumerate() {
            while let Some((vk, _)) = net.recv() {
                if let Some(j) = vks.iter().position(|v| *v == vk) {
                    if late && j != i {
                        heard[i][j] = true;
                    }
                }
            }
        }
    }

    let report: Vec<String> = metrics
        .iter()
        .map(|m| {
            format!(
                "wd={} stale={} subscribed={} mesh={}",
                m.mesh_watchdog_disconnects.get(),
                m.mesh_stale_conn_disconnects.get(),
                m.consensus_subscribed_validators.get(),
                m.consensus_mesh_peers.get()
            )
        })
        .collect();
    let detail = format!("{report:?} heard={heard:?}");
    for (i, m) in metrics.iter().enumerate() {
        assert_eq!(
            m.mesh_watchdog_disconnects.get(),
            0,
            "node {i}: false watchdog kick; {detail}"
        );
        assert_eq!(
            m.mesh_stale_conn_disconnects.get(),
            0,
            "node {i}: false stale-conn kick; {detail}"
        );
        assert_eq!(
            m.consensus_subscribed_validators.get(),
            2,
            "node {i}: both peers subscribed; {detail}"
        );
        for (j, heard_j) in heard[i].iter().enumerate() {
            assert!(
                i == j || *heard_j,
                "node {i} stopped hearing node {j}; {detail}"
            );
        }
    }
}
