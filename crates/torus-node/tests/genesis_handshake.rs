//! Owner s138 (K question, option B): the real `torus-node` binary refuses a
//! peer whose genesis digest differs from its own, at connect time.
//!
//! Genesis G: 2 validators (equal stakes, so both must vote for every block)
//! and `consensus.leader_miss_threshold = 3` (K; dead-leader phase 3 found
//! that a K differing across nodes diverges state from height 1).
//!
//! - V0 runs G pretty-printed; V1 runs G compact with every object's keys in
//!   reverse order. Same canonical digest, so they peer and the chain
//!   commits blocks (it cannot without both);
//! - R1 (`--rpc-only`, G) connects to V0 and passes its identify check;
//! - R2 (`--rpc-only`, G with K = 5) is refused by both ends, each logging
//!   `genesis digest <x> != ours <y>: refusing`, and never executes a block.
#![cfg(unix)]

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use torus_genesis::GenesisDigest;

const BOUND: Duration = Duration::from_secs(120);

/// The devnet key of `devnet/genesis.json` validator `i` (0-based).
fn validator_key(i: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0] = i + 1;
    key
}

/// 2-validator genesis value from `devnet/genesis.json` with K = `k`.
fn genesis_value(k: u64) -> serde_json::Value {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let two: Vec<serde_json::Value> = g["validators"].as_array().unwrap()[..2].to_vec();
    g["validators"] = serde_json::Value::Array(two);
    g["consensus"]["leader_miss_threshold"] = k.into();
    g
}

/// Compact JSON with every object's keys in reverse sorted order.
fn reversed_keys_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            keys.reverse();
            let items: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        reversed_keys_json(&map[k])
                    )
                })
                .collect();
            format!("{{{}}}", items.join(","))
        }
        serde_json::Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(reversed_keys_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        scalar => serde_json::to_string(scalar).unwrap(),
    }
}

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

struct Node {
    child: Child,
    log: PathBuf,
}

impl Node {
    fn start(work: &Path, name: &str, genesis: &Path, port: u16, extra: &[String]) -> Node {
        let log = work.join(format!("{name}.log"));
        let out = std::fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_torus-node"))
            .arg("--genesis")
            .arg(genesis)
            .arg("--data-dir")
            .arg(work.join(format!("data-{name}")))
            .args([
                "--p2p-listen",
                &format!("/ip4/127.0.0.1/udp/{port}/quic-v1"),
                "--p2p-private-addrs",
                "--rpc-addr",
                "127.0.0.1:0",
                "--metrics-addr",
                "127.0.0.1:0",
                "--log-level",
                "info",
            ])
            .args(extra)
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn torus-node");
        Node { child, log }
    }

    fn log_text(&self) -> String {
        strip_ansi(&std::fs::read_to_string(&self.log).unwrap_or_default())
    }

    fn tail(&self) -> String {
        let text = self.log_text();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(40)..].join("\n")
    }

    /// Highest `execution pipeline: block done height=N` in the log.
    fn executed_height(&self) -> Option<u64> {
        self.log_text()
            .lines()
            .filter(|l| l.contains("execution pipeline: block done"))
            .filter_map(|l| {
                let rest = &l[l.find("height=")? + "height=".len()..];
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            })
            .max()
    }

    /// Connection / sync / refusal lines (the first 40), for a failure report.
    fn key_lines(&self) -> String {
        const KEYS: [&str; 8] = [
            "peer connected",
            "refusing",
            "block_sync: starting",
            "cannot sync",
            "blacklist",
            "worker returned",
            "ERROR",
            "p2p identity",
        ];
        let text = self.log_text();
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| KEYS.iter().any(|k| l.contains(k)))
            .take(40)
            .collect();
        lines.join("\n")
    }

    fn assert_running(&mut self) {
        if let Ok(Some(st)) = self.child.try_wait() {
            panic!("node exited early ({st}):\n{}", self.tail());
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Poll `cond` over the nodes until it holds (or panic after [`BOUND`]).
fn wait_until(nodes: &mut [&mut Node], what: &str, mut cond: impl FnMut(&[&mut Node]) -> bool) {
    let start = Instant::now();
    loop {
        if cond(nodes) {
            return;
        }
        for n in nodes.iter_mut() {
            n.assert_running();
        }
        if start.elapsed() > BOUND {
            let tails: Vec<String> = nodes
                .iter()
                .map(|n| {
                    format!(
                        "--- {} key lines:\n{}\n--- tail:\n{}",
                        n.log.display(),
                        n.key_lines(),
                        n.tail()
                    )
                })
                .collect();
            panic!("{what}: not within {BOUND:?}\n{}", tails.join("\n"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `local_peer_id=<id>` from the node's `p2p identity` line.
fn local_peer_id(node: &Node) -> Option<String> {
    let text = node.log_text();
    let line = text.lines().find(|l| l.contains("p2p identity"))?;
    let rest = &line[line.find("local_peer_id=")? + "local_peer_id=".len()..];
    Some(rest.split_whitespace().next()?.to_string())
}

#[test]
fn genesis_digest_is_canonical_and_a_k_difference_is_refused_at_connect() {
    let work = tempfile::tempdir().unwrap();
    let g3 = genesis_value(3);
    let g5 = genesis_value(5);
    let v0_text = serde_json::to_string_pretty(&g3).unwrap();
    let v1_text = reversed_keys_json(&g3);
    let r2_text = serde_json::to_string_pretty(&g5).unwrap();
    assert_ne!(v0_text, v1_text);

    // Unit level: the canonical digest P0.0 binds data dirs to.
    let d3 = GenesisDigest::of_json(&v0_text).unwrap();
    assert_eq!(
        GenesisDigest::of_json(&v1_text).unwrap(),
        d3,
        "whitespace / key order must not change the digest"
    );
    let d5 = GenesisDigest::of_json(&r2_text).unwrap();
    assert_ne!(d5.digest, d3.digest, "K must change the digest");
    assert_eq!(
        d3.changed_fields(&d5),
        vec!["consensus.leader_miss_threshold"]
    );

    let v0_genesis = write(work.path(), "genesis-v0.json", &v0_text);
    let v1_genesis = write(work.path(), "genesis-v1.json", &v1_text);
    let r2_genesis = write(work.path(), "genesis-r2.json", &r2_text);

    let v0_peer = torus_network::bridge::peer_id_from_verifying_key(
        &SigningKey::from_bytes(&validator_key(0)).verifying_key(),
    );
    let v0_port = free_udp_port();
    let v0_addr = format!("/ip4/127.0.0.1/udp/{v0_port}/quic-v1/p2p/{v0_peer}");
    let validator = |i: u8, peers: &str| {
        vec![
            "--validator-key".to_string(),
            hex::encode(validator_key(i)),
            // Explicit (possibly empty): never the compiled testnet seeds.
            format!("--p2p-peers={peers}"),
        ]
    };
    let rpc = |peers: &str| vec!["--rpc-only".to_string(), format!("--p2p-peers={peers}")];

    let mut v0 = Node::start(work.path(), "v0", &v0_genesis, v0_port, &validator(0, ""));
    let mut v1 = Node::start(
        work.path(),
        "v1",
        &v1_genesis,
        free_udp_port(),
        &validator(1, &v0_addr),
    );

    // The same genesis, reformatted: the two validators peer, so the chain
    // (which needs both votes) commits and executes blocks on both.
    wait_until(
        &mut [&mut v0, &mut v1],
        "the 2-validator chain executes blocks",
        |n| {
            n.iter()
                .all(|n| n.executed_height().is_some_and(|h| h >= 3))
        },
    );

    // An rpc-only node with the same genesis passes the identify check.
    let mut r1 = Node::start(
        work.path(),
        "r1",
        &v0_genesis,
        free_udp_port(),
        &rpc(&v0_addr),
    );
    wait_until(&mut [&mut v0, &mut v1, &mut r1], "v0 accepts r1", |n| {
        local_peer_id(n[2]).is_some_and(|pid| {
            n[0].log_text()
                .lines()
                .any(|l| l.contains("registered peer from identify exchange") && l.contains(&pid))
        })
    });

    // K differs: both ends refuse, naming both digests.
    let mut r2 = Node::start(
        work.path(),
        "r2",
        &r2_genesis,
        free_udp_port(),
        &rpc(&v0_addr),
    );
    let x3 = format!("{:#x}", d3.digest);
    let x5 = format!("{:#x}", d5.digest);
    let v0_line = format!("genesis digest {x5} != ours {x3}: refusing");
    let r2_line = format!("genesis digest {x3} != ours {x5}: refusing");
    wait_until(
        &mut [&mut v0, &mut v1, &mut r1, &mut r2],
        "both ends refuse r2",
        |n| n[0].log_text().contains(&v0_line) && n[3].log_text().contains(&r2_line),
    );
    // Every refusal involves r2's digest: no same-genesis peer was refused.
    for (name, node) in [("v0", &v0), ("v1", &v1), ("r1", &r1)] {
        let text = node.log_text();
        let bad: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(": refusing") && !l.contains(&x5))
            .collect();
        assert!(
            bad.is_empty(),
            "{name} refused a same-genesis peer: {bad:?}"
        );
    }

    // R2 gets nothing while the chain keeps going.
    let h = v0.executed_height().unwrap();
    wait_until(
        &mut [&mut v0, &mut v1, &mut r1, &mut r2],
        "the chain keeps executing",
        |n| n[0].executed_height().is_some_and(|x| x > h + 3),
    );
    assert_eq!(
        r2.executed_height(),
        None,
        "r2 (K differs) must not sync a block:\n{}",
        r2.tail()
    );
}
