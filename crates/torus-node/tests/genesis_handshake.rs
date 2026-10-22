//! Owner s138 (K question, option B): the real `torus-node` binary refuses a
//! peer whose genesis digest differs from its own, at connect time.
//!
//! A 1-validator chain V runs from a genesis carrying
//! `consensus.leader_miss_threshold = 3` (K; dead-leader phase 3 found that a
//! K differing across nodes diverges state from height 1). Two `--rpc-only`
//! nodes dial it:
//!
//! - R1's genesis is the same JSON, compact and with every object's keys in
//!   reverse order: the same canonical digest, so it peers and syncs blocks;
//! - R2's genesis differs only in K (5): both V and R2 log
//!   `genesis digest <x> != ours <y>: refusing`, and R2 never executes a block.
#![cfg(unix)]

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use torus_genesis::GenesisDigest;

const BOUND: Duration = Duration::from_secs(120);
const VALIDATOR_KEY: &str = "0100000000000000000000000000000000000000000000000000000000000000";

/// 1-validator genesis value from `devnet/genesis.json` with K = `k`.
fn genesis_value(k: u64) -> serde_json::Value {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let first = g["validators"][0].clone();
    g["validators"] = serde_json::Value::Array(vec![first]);
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
            items.iter().map(reversed_keys_json).collect::<Vec<_>>().join(",")
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
                .map(|n| format!("--- {}:\n{}", n.log.display(), n.tail()))
                .collect();
            panic!("{what}: not within {BOUND:?}\n{}", tails.join("\n"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn genesis_digest_is_canonical_and_a_k_difference_is_refused_at_connect() {
    let work = tempfile::tempdir().unwrap();
    let g3 = genesis_value(3);
    let g5 = genesis_value(5);
    let v_text = serde_json::to_string_pretty(&g3).unwrap();
    let r1_text = reversed_keys_json(&g3);
    let r2_text = serde_json::to_string_pretty(&g5).unwrap();
    assert_ne!(v_text, r1_text);

    // Unit level: the canonical digest P0.0 binds data dirs to.
    let d3 = GenesisDigest::of_json(&v_text).unwrap();
    assert_eq!(
        GenesisDigest::of_json(&r1_text).unwrap(),
        d3,
        "whitespace / key order must not change the digest"
    );
    let d5 = GenesisDigest::of_json(&r2_text).unwrap();
    assert_ne!(d5.digest, d3.digest, "K must change the digest");
    assert_eq!(d3.changed_fields(&d5), vec!["consensus.leader_miss_threshold"]);

    let v_genesis = write(work.path(), "genesis-v.json", &v_text);
    let r1_genesis = write(work.path(), "genesis-r1.json", &r1_text);
    let r2_genesis = write(work.path(), "genesis-r2.json", &r2_text);

    let mut key = [0u8; 32];
    key[0] = 1;
    let v_peer = torus_network::bridge::peer_id_from_verifying_key(
        &SigningKey::from_bytes(&key).verifying_key(),
    );
    let v_port = free_udp_port();
    let mut v = Node::start(
        work.path(),
        "v",
        &v_genesis,
        v_port,
        &[
            "--validator-key".into(),
            VALIDATOR_KEY.into(),
            // No bootstrap peers (not the compiled testnet seeds).
            "--p2p-peers=".into(),
        ],
    );
    let v_addr = format!("/ip4/127.0.0.1/udp/{v_port}/quic-v1/p2p/{v_peer}");
    let rpc = |peers: &str| vec!["--rpc-only".to_string(), format!("--p2p-peers={peers}")];
    let mut r1 = Node::start(work.path(), "r1", &r1_genesis, free_udp_port(), &rpc(&v_addr));
    let mut r2 = Node::start(work.path(), "r2", &r2_genesis, free_udp_port(), &rpc(&v_addr));

    // The same genesis, reformatted: peers and syncs.
    wait_until(&mut [&mut v, &mut r1, &mut r2], "r1 syncs blocks from v", |n| {
        n[1].executed_height().is_some_and(|h| h >= 2)
    });

    // K differs: both ends refuse, naming both digests.
    let x3 = format!("{:#x}", d3.digest);
    let x5 = format!("{:#x}", d5.digest);
    let v_line = format!("genesis digest {x5} != ours {x3}: refusing");
    let r2_line = format!("genesis digest {x3} != ours {x5}: refusing");
    wait_until(&mut [&mut v, &mut r1, &mut r2], "both ends refuse r2", |n| {
        n[0].log_text().contains(&v_line) && n[2].log_text().contains(&r2_line)
    });
    assert!(
        !v.log_text().contains(&format!("genesis digest {x3} != ours {x3}")),
        "r1 must not be refused:\n{}",
        v.tail()
    );
    assert!(
        r1.log_text().lines().all(|l| !l.contains("genesis digest") || !l.contains("refusing")),
        "r1 refused nothing:\n{}",
        r1.tail()
    );

    // R2 keeps getting nothing while R1 keeps syncing.
    let h1 = r1.executed_height().unwrap();
    wait_until(&mut [&mut v, &mut r1, &mut r2], "r1 keeps syncing", |n| {
        n[1].executed_height().is_some_and(|h| h > h1 + 2)
    });
    assert_eq!(
        r2.executed_height(),
        None,
        "r2 (K differs) must not sync a block:\n{}",
        r2.tail()
    );
}
