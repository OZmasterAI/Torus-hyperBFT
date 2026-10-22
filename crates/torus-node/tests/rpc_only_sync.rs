//! An `--rpc-only` node syncs from a single validator (real `torus-node`
//! binaries, debug build).
//!
//! Testnet wallets read from `--rpc-only` nodes (owner s138, RPC freshness
//! option a), and fresh / archive nodes sync the same way, often from one
//! honest peer. V is a 1-validator chain producing blocks as fast as a debug
//! build can; R (`--rpc-only`) dials only V and must catch up and keep up.
//! This failed on loaded runners (ubuntoz, cachyoz) with `max sub-streams
//! reached` and fetch timeouts on R's side while it passed on 18c.
#![cfg(unix)]

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;

const BOUND: Duration = Duration::from_secs(120);
const VALIDATOR_KEY: [u8; 32] = {
    let mut k = [0u8; 32];
    k[0] = 1;
    k
};

/// 1-validator genesis value from `devnet/genesis.json`.
fn genesis_text() -> String {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../devnet/genesis.json");
    let mut g: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&src).expect("read devnet genesis")).unwrap();
    let first = g["validators"][0].clone();
    g["validators"] = serde_json::Value::Array(vec![first]);
    serde_json::to_string_pretty(&g).unwrap()
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

    /// How often each failure symptom appears in the log.
    fn symptom_counts(&self) -> String {
        const KEYS: [&str; 6] = [
            "max sub-streams",
            "Timeout",
            "timed out",
            "OutboundFailure",
            "at capacity",
            "cannot sync",
        ];
        let text = self.log_text();
        KEYS.iter()
            .map(|k| format!("{k}={}", text.matches(k).count()))
            .collect::<Vec<_>>()
            .join(" ")
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

/// Poll `cond` until it holds (or panic after [`BOUND`], keeping the logs).
fn wait_until(
    work: &Path,
    nodes: &mut [&mut Node],
    what: &str,
    mut cond: impl FnMut(&[&mut Node]) -> bool,
) {
    let start = Instant::now();
    loop {
        if cond(nodes) {
            return;
        }
        for n in nodes.iter_mut() {
            n.assert_running();
        }
        if start.elapsed() > BOUND {
            let keep = std::env::temp_dir().join(format!("rpc-only-sync-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&keep);
            let tails: Vec<String> = nodes
                .iter()
                .map(|n| {
                    let name = n.log.file_name().unwrap();
                    let _ = std::fs::copy(&n.log, keep.join(name));
                    format!(
                        "--- {} height={:?} {}\n--- tail:\n{}",
                        n.log.display(),
                        n.executed_height(),
                        n.symptom_counts(),
                        n.tail()
                    )
                })
                .collect();
            panic!(
                "{what}: not within {BOUND:?} (logs kept in {}; work {})\n{}",
                keep.display(),
                work.display(),
                tails.join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn rpc_only_node_syncs_and_keeps_up_with_a_single_fast_validator() {
    let work = tempfile::tempdir().unwrap();
    let genesis = work.path().join("genesis.json");
    std::fs::write(&genesis, genesis_text()).unwrap();

    let v_peer = torus_network::bridge::peer_id_from_verifying_key(
        &SigningKey::from_bytes(&VALIDATOR_KEY).verifying_key(),
    );
    let v_port = free_udp_port();
    let v_addr = format!("/ip4/127.0.0.1/udp/{v_port}/quic-v1/p2p/{v_peer}");
    let mut v = Node::start(
        work.path(),
        "v",
        &genesis,
        v_port,
        &[
            "--validator-key".into(),
            hex::encode(VALIDATOR_KEY),
            // No bootstrap peers (never the compiled testnet seeds).
            "--p2p-peers=".into(),
        ],
    );

    // Let V get ahead first, so R starts as a node that must catch up.
    wait_until(work.path(), &mut [&mut v], "v executes blocks", |n| {
        n[0].executed_height().is_some_and(|h| h >= 100)
    });

    let mut r = Node::start(
        work.path(),
        "r",
        &genesis,
        free_udp_port(),
        &["--rpc-only".to_string(), format!("--p2p-peers={v_addr}")],
    );

    // Catch up: R reaches the height V had when R started...
    let target = v.executed_height().unwrap();
    wait_until(work.path(), &mut [&mut v, &mut r], "r catches up", |n| {
        n[1].executed_height().is_some_and(|h| h >= target)
    });

    // ...and keeps up: R reaches V's current height again, twice.
    for round in 0..2 {
        let target = v.executed_height().unwrap();
        wait_until(
            work.path(),
            &mut [&mut v, &mut r],
            &format!("r keeps up (round {round}, target {target})"),
            |n| n[1].executed_height().is_some_and(|h| h >= target),
        );
    }
}
